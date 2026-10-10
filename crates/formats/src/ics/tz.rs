//! Time zones: VTIMEZONE components (RFC 5545 §3.6.5), fixed-offset TZIDs and a small built-in table of
//! common zones (used when a file names a zone without defining it).
//!
//! An observance (STANDARD/DAYLIGHT) starts at its DTSTART and recurs by its RRULE/RDATEs; onsets are local
//! times in the offset in force before the change (TZOFFSETFROM). A local time takes the offset of the
//! observance with the latest onset at or before it.
//!
//! Onsets of an observance rule are listed per four-year chunk (or once, for a COUNT-limited rule) and kept in
//! an [`OnsetMemo`]; [`ZoneLookup`] carries one across the many lookups of an expansion so a hostile zone
//! (dozens of observances with daily or COUNT rules) costs a bounded amount of work per chunk, not per lookup.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

use super::parse::Component;
use super::rrule::{self, Expand, Rrule};
use super::value::{self, DAY, TimeValue};

/// Most observances kept per zone.
const MAX_OBSERVANCES: usize = 64;
/// Most RDATEs kept per observance.
const MAX_RDATES: usize = 512;
/// Length of one memoised onset chunk (local seconds): four years.
const CHUNK: i64 = 4 * 366 * DAY;
/// Most onsets listed for one chunk or one COUNT-limited rule.
const MAX_LISTED_ONSETS: usize = 4096;
/// Most recurrence periods examined when listing onsets.
const MAX_ONSET_PERIODS: usize = 2_000;
/// Most onsets a memo keeps before it is cleared.
const MAX_MEMO_ONSETS: usize = 262_144;

/// Listed onsets of observance rules, keyed by observance index.
#[derive(Debug, Default)]
pub struct OnsetMemo {
    /// Every onset of a COUNT-limited rule (capped).
    counted: HashMap<usize, Vec<i64>>,
    /// Onsets of other rules per (observance, chunk).
    chunks: HashMap<(usize, i64), Vec<i64>>,
    stored: usize,
}

impl OnsetMemo {
    fn make_room(&mut self) {
        if self.stored >= MAX_MEMO_ONSETS {
            *self = OnsetMemo::default();
        }
    }
}

/// Onsets (local secs, sorted) of `rule` from `start` within `[from, to)`.
fn rule_onsets(rule: &Rrule, start: i64, from: i64, to: i64) -> Vec<i64> {
    let ident = |l: i64| l;
    let never = |_: i64, _: i64| false;
    let x = Expand { rule, dtstart: start, duration: 0, to_utc: &ident, excluded: &never };
    let mut v: Vec<i64> = rrule::expand_periods(&x, from, to, MAX_LISTED_ONSETS, MAX_ONSET_PERIODS)
        .into_iter()
        .map(|o| o.local)
        .collect();
    v.sort_unstable();
    v
}

/// Latest entry of a sorted list at or before `local`.
fn latest_at_or_before(list: &[i64], local: i64) -> Option<i64> {
    let n = list.partition_point(|&o| o <= local);
    n.checked_sub(1).and_then(|i| list.get(i)).copied()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observance {
    pub daylight: bool,
    /// Local secs of the first onset.
    pub start: i64,
    pub offset_from: i32,
    pub offset_to: i32,
    pub rrule: Option<Rrule>,
    pub rdates: Vec<i64>,
}

impl Observance {
    /// Latest onset (local secs) at or before `local`; `idx` keys this observance's entries in `memo`.
    fn last_onset(&self, idx: usize, local: i64, memo: &mut OnsetMemo) -> Option<i64> {
        if local < self.start {
            return None;
        }
        let mut best = self.start;
        for &r in &self.rdates {
            if r <= local && r > best {
                best = r;
            }
        }
        let Some(rule) = &self.rrule else { return Some(best) };
        // A rule that ended (UNTIL) before `local`: its last onset is near the UNTIL bound, so look there.
        let probe = match rule.until {
            Some(rrule::Until::Date(d) | rrule::Until::Local(d)) => local.min(d.saturating_add(DAY)),
            // UTC bound as local time: allow for any offset.
            Some(rrule::Until::Utc(u)) => local.min(u.saturating_add(DAY)),
            None => local,
        };
        let found = if rule.count.is_some() {
            // COUNT is counted from DTSTART, so the whole (capped) list is built once.
            if !memo.counted.contains_key(&idx) {
                memo.make_room();
                let list = rule_onsets(rule, self.start, i64::MIN / 4, i64::MAX / 4);
                memo.stored = memo.stored.saturating_add(list.len().max(1));
                memo.counted.insert(idx, list);
            }
            memo.counted.get(&idx).and_then(|l| latest_at_or_before(l, local))
        } else {
            // The chunk holding `probe`, else the one before it (at least four years of look-back).
            let chunk = probe.div_euclid(CHUNK);
            let mut found = None;
            for c in [chunk, chunk.saturating_sub(1)] {
                if !memo.chunks.contains_key(&(idx, c)) {
                    memo.make_room();
                    let from = c.saturating_mul(CHUNK);
                    let list = rule_onsets(rule, self.start, from, from.saturating_add(CHUNK));
                    memo.stored = memo.stored.saturating_add(list.len().max(1));
                    memo.chunks.insert((idx, c), list);
                }
                found = memo.chunks.get(&(idx, c)).and_then(|l| latest_at_or_before(l, local));
                if found.is_some() {
                    break;
                }
            }
            found
        };
        Some(found.map_or(best, |f| f.max(best)))
    }
}

/// A time zone: a TZID and its observances (one observance = fixed offset).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeZone {
    pub tzid: String,
    pub observances: Vec<Observance>,
}

impl TimeZone {
    /// A zone with one fixed offset (minutes east of UTC).
    pub fn fixed(tzid: &str, offset: i32) -> TimeZone {
        TimeZone {
            tzid: tzid.to_owned(),
            observances: vec![Observance {
                daylight: false,
                start: i64::MIN / 4,
                offset_from: offset,
                offset_to: offset,
                rrule: None,
                rdates: Vec::new(),
            }],
        }
    }

    /// Offset (minutes) in force at local time `local`.
    pub fn offset_at_local(&self, local: i64) -> i32 {
        self.offset_at_local_memo(local, &mut OnsetMemo::default())
    }

    /// [`TimeZone::offset_at_local`] reusing listed onsets.
    pub fn offset_at_local_memo(&self, local: i64, memo: &mut OnsetMemo) -> i32 {
        let mut best: Option<(i64, i32)> = None;
        for (i, o) in self.observances.iter().enumerate() {
            if let Some(t) = o.last_onset(i, local, memo)
                && best.is_none_or(|(b, _)| t > b)
            {
                best = Some((t, o.offset_to));
            }
        }
        match best {
            Some((_, off)) => off,
            // Before every onset: the earliest observance's "from" offset.
            None => self.observances.iter().min_by_key(|o| o.start).map(|o| o.offset_from).unwrap_or(0),
        }
    }

    /// Local secs → unix seconds.
    pub fn to_utc(&self, local: i64) -> i64 {
        self.resolve_local_memo(local, &mut OnsetMemo::default()).0
    }

    /// Local secs → (unix seconds, offset in force then). RFC 5545 §3.3.5: a local time skipped by a forward
    /// shift (a gap) is interpreted with the offset before the transition, which lands it after the change; a
    /// repeated local time (a fold) is the first of the two instants.
    pub fn resolve_local_memo(&self, local: i64, memo: &mut OnsetMemo) -> (i64, i32) {
        let off = self.offset_at_local_memo(local, memo);
        let unix = local.saturating_sub(i64::from(off) * 60);
        // Inside a gap `unix` falls before the transition, where the earlier (smaller) offset is in force.
        let before = self.offset_at_utc_memo(unix, memo);
        if before < off { (local.saturating_sub(i64::from(before) * 60), off) } else { (unix, off) }
    }

    /// Unix seconds → offset in force (minutes).
    pub fn offset_at_utc(&self, unix: i64) -> i32 {
        self.offset_at_utc_memo(unix, &mut OnsetMemo::default())
    }

    /// [`TimeZone::offset_at_utc`] reusing listed onsets.
    pub fn offset_at_utc_memo(&self, unix: i64, memo: &mut OnsetMemo) -> i32 {
        // Try the local time under each candidate offset; keep the one that maps back to `unix`.
        let mut offsets: Vec<i32> =
            self.observances.iter().flat_map(|o| [o.offset_to, o.offset_from]).collect();
        offsets.sort_unstable();
        offsets.dedup();
        for off in &offsets {
            let local = unix.saturating_add(i64::from(*off) * 60);
            if self.offset_at_local_memo(local, memo) == *off {
                return *off;
            }
        }
        offsets.first().copied().unwrap_or(0)
    }

    /// Builds a zone from a VTIMEZONE component.
    pub fn from_component(c: &Component) -> Option<TimeZone> {
        let tzid = c.prop("TZID")?.value.trim().to_owned();
        if tzid.is_empty() {
            return None;
        }
        let mut observances = Vec::new();
        for sub in c.children.iter().take(MAX_OBSERVANCES) {
            let daylight = match sub.name.as_str() {
                "STANDARD" => false,
                "DAYLIGHT" => true,
                _ => continue,
            };
            let Some(offset_to) = sub.prop("TZOFFSETTO").and_then(|p| value::parse_utc_offset(&p.value))
            else {
                continue;
            };
            let offset_from =
                sub.prop("TZOFFSETFROM").and_then(|p| value::parse_utc_offset(&p.value)).unwrap_or(offset_to);
            let start = sub
                .prop("DTSTART")
                .and_then(|p| value::parse_time_value(&p.value, p.param("VALUE"), None))
                .map(|t| match t {
                    TimeValue::Date(s) | TimeValue::Utc(s) | TimeValue::Local { secs: s, .. } => s,
                })
                .unwrap_or(0);
            let rrule = sub.prop("RRULE").and_then(|p| Rrule::parse(&p.value).ok());
            let mut rdates = Vec::new();
            for p in sub.props_named("RDATE") {
                for v in p.value.split(',').take(MAX_RDATES) {
                    if let Some(t) = value::parse_time_value(v, p.param("VALUE"), None) {
                        let s = match t {
                            TimeValue::Date(s) | TimeValue::Utc(s) | TimeValue::Local { secs: s, .. } => s,
                        };
                        if rdates.len() < MAX_RDATES {
                            rdates.push(s);
                        }
                    }
                }
            }
            observances.push(Observance { daylight, start, offset_from, offset_to, rrule, rdates });
        }
        if observances.is_empty() {
            return fixed_from_name(&tzid).map(|off| TimeZone::fixed(&tzid, off));
        }
        Some(TimeZone { tzid, observances })
    }

    /// VTIMEZONE component for a fixed-offset zone named by [`fixed_tzid`].
    pub fn fixed_component(offset: i32) -> Component {
        let mut tz = Component::new("VTIMEZONE");
        tz.push("TZID", vec![], fixed_tzid(offset));
        let mut std = Component::new("STANDARD");
        std.push("DTSTART", vec![], "19700101T000000");
        std.push("TZOFFSETFROM", vec![], value::format_utc_offset(offset));
        std.push("TZOFFSETTO", vec![], value::format_utc_offset(offset));
        std.push("TZNAME", vec![], fixed_tzid(offset));
        tz.children.push(std);
        tz
    }
}

/// TZID used for a fixed offset: `UTC+05:30`, `UTC-04:00`.
pub fn fixed_tzid(offset: i32) -> String {
    let a = offset.unsigned_abs();
    format!("UTC{}{:02}:{:02}", if offset < 0 { '-' } else { '+' }, a / 60, a % 60)
}

/// Fixed offsets spelled in a TZID: `UTC+05:30`, `GMT-5`, `UTC+0530`, `Etc/GMT+5` (POSIX sign: UTC−5),
/// `(UTC-05:00) Some City`.
pub fn fixed_from_name(tzid: &str) -> Option<i32> {
    let t = tzid.trim().trim_matches('"');
    let t = t.strip_prefix('(').and_then(|r| r.split_once(')')).map(|(inner, _)| inner).unwrap_or(t);
    let upper = t.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "UTC" | "GMT" | "Z" | "UT" | "ETC/UTC" | "ETC/GMT" | "ETC/UCT" | "UCT" | "ZULU"
    ) {
        return Some(0);
    }
    let (posix, rest) = if let Some(r) = upper.strip_prefix("ETC/GMT") {
        (true, r)
    } else {
        let r = upper.strip_prefix("UTC").or_else(|| upper.strip_prefix("GMT"))?;
        (false, r)
    };
    let rest = rest.trim();
    let sign = match rest.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let num = rest.get(1..)?.trim();
    let (h, m) = match num.split_once(':') {
        Some((h, m)) => (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?),
        None if num.len() == 4 => (num.get(..2)?.parse().ok()?, num.get(2..)?.parse().ok()?),
        None => (num.parse::<i32>().ok()?, 0),
    };
    if !(0..=14).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    let off = sign * (h * 60 + m);
    Some(if posix { -off } else { off })
}

/// Daylight-saving rule of a built-in zone.
#[derive(Clone, Copy)]
enum Dst {
    None,
    /// US/Canada: 2nd Sunday March 02:00 → 1st Sunday November 02:00 (local).
    NorthAmerica,
    /// EU: last Sunday March 01:00 UTC → last Sunday October 01:00 UTC.
    Europe,
    /// South-east Australia: 1st Sunday October 02:00 → 1st Sunday April 03:00.
    Australia,
    /// New Zealand: last Sunday September 02:00 → 1st Sunday April 03:00.
    NewZealand,
}

const BUILTIN: &[(&[&str], i32, Dst)] = &[
    (
        &["America/New_York", "US/Eastern", "America/Toronto", "America/Detroit", "America/Montreal"],
        -300,
        Dst::NorthAmerica,
    ),
    (&["America/Chicago", "US/Central", "America/Winnipeg"], -360, Dst::NorthAmerica),
    (&["America/Denver", "US/Mountain", "America/Edmonton", "America/Boise"], -420, Dst::NorthAmerica),
    (&["America/Phoenix", "US/Arizona"], -420, Dst::None),
    (&["America/Los_Angeles", "US/Pacific", "America/Vancouver", "America/Tijuana"], -480, Dst::NorthAmerica),
    (&["America/Anchorage", "US/Alaska"], -540, Dst::NorthAmerica),
    (&["Pacific/Honolulu", "US/Hawaii"], -600, Dst::None),
    (&["America/Halifax", "Atlantic/Bermuda"], -240, Dst::NorthAmerica),
    (&["America/St_Johns"], -210, Dst::NorthAmerica),
    (&["America/Regina"], -360, Dst::None),
    (&["America/Mexico_City", "America/Costa_Rica", "America/Guatemala"], -360, Dst::None),
    (&["America/Bogota", "America/Lima", "America/Panama"], -300, Dst::None),
    (&["America/Caracas", "America/La_Paz", "America/Puerto_Rico"], -240, Dst::None),
    (
        &[
            "America/Sao_Paulo",
            "America/Argentina/Buenos_Aires",
            "America/Buenos_Aires",
            "America/Montevideo",
        ],
        -180,
        Dst::None,
    ),
    (&["Atlantic/Reykjavik", "Africa/Abidjan", "Africa/Accra"], 0, Dst::None),
    (
        &["Europe/London", "Europe/Dublin", "Europe/Lisbon", "Atlantic/Canary", "Atlantic/Faroe"],
        0,
        Dst::Europe,
    ),
    (
        &[
            "Europe/Paris",
            "Europe/Berlin",
            "Europe/Madrid",
            "Europe/Rome",
            "Europe/Amsterdam",
            "Europe/Brussels",
            "Europe/Vienna",
            "Europe/Zurich",
            "Europe/Stockholm",
            "Europe/Oslo",
            "Europe/Copenhagen",
            "Europe/Warsaw",
            "Europe/Prague",
            "Europe/Budapest",
            "Europe/Belgrade",
            "Europe/Zagreb",
            "Europe/Ljubljana",
            "Europe/Bratislava",
            "Europe/Luxembourg",
            "Europe/Monaco",
            "Europe/Malta",
            "CET",
        ],
        60,
        Dst::Europe,
    ),
    (
        &[
            "Europe/Athens",
            "Europe/Helsinki",
            "Europe/Kyiv",
            "Europe/Kiev",
            "Europe/Bucharest",
            "Europe/Sofia",
            "Europe/Riga",
            "Europe/Tallinn",
            "Europe/Vilnius",
            "EET",
        ],
        120,
        Dst::Europe,
    ),
    (&["Africa/Lagos", "Africa/Algiers", "Africa/Tunis"], 60, Dst::None),
    (&["Africa/Johannesburg", "Africa/Cairo", "Africa/Harare", "Africa/Maputo"], 120, Dst::None),
    (
        &["Europe/Moscow", "Europe/Istanbul", "Europe/Minsk", "Asia/Riyadh", "Asia/Qatar", "Africa/Nairobi"],
        180,
        Dst::None,
    ),
    (&["Asia/Dubai", "Asia/Muscat", "Asia/Baku", "Asia/Tbilisi"], 240, Dst::None),
    (&["Asia/Karachi", "Asia/Tashkent"], 300, Dst::None),
    (&["Asia/Kolkata", "Asia/Calcutta", "Asia/Colombo"], 330, Dst::None),
    (&["Asia/Kathmandu"], 345, Dst::None),
    (&["Asia/Dhaka", "Asia/Almaty"], 360, Dst::None),
    (&["Asia/Bangkok", "Asia/Jakarta", "Asia/Ho_Chi_Minh"], 420, Dst::None),
    (
        &[
            "Asia/Shanghai",
            "Asia/Hong_Kong",
            "Asia/Singapore",
            "Asia/Taipei",
            "Asia/Manila",
            "Asia/Kuala_Lumpur",
            "Australia/Perth",
            "PRC",
        ],
        480,
        Dst::None,
    ),
    (&["Asia/Tokyo", "Asia/Seoul", "Japan"], 540, Dst::None),
    (&["Australia/Darwin"], 570, Dst::None),
    (&["Australia/Adelaide"], 570, Dst::Australia),
    (&["Australia/Brisbane"], 600, Dst::None),
    (
        &["Australia/Sydney", "Australia/Melbourne", "Australia/Canberra", "Australia/Hobart"],
        600,
        Dst::Australia,
    ),
    (&["Pacific/Auckland", "NZ"], 720, Dst::NewZealand),
];

fn observance(daylight: bool, start: i64, from: i32, to: i32, rule: &str) -> Observance {
    Observance {
        daylight,
        start,
        offset_from: from,
        offset_to: to,
        rrule: Rrule::parse(rule).ok(),
        rdates: Vec::new(),
    }
}

/// A built-in zone by IANA name (case-insensitive), or a fixed-offset name.
pub fn builtin(tzid: &str) -> Option<TimeZone> {
    let name = tzid.trim().trim_matches('"').trim_start_matches('/');
    if let Some(off) = fixed_from_name(name) {
        return Some(TimeZone::fixed(tzid, off));
    }
    let (_, std, dst) =
        BUILTIN.iter().find(|(names, _, _)| names.iter().any(|n| n.eq_ignore_ascii_case(name)))?;
    let std = *std;
    let day = std + 60;
    // Onsets from 2007 (the current North American rules); earlier dates use the first observance's offsets.
    let at = |y: i64, m: u8, d: u8, h: u8| value::local_secs(y, m, d, h, 0, 0).unwrap_or(0);
    let observances = match dst {
        Dst::None => return Some(TimeZone::fixed(tzid, std)),
        Dst::NorthAmerica => vec![
            observance(true, at(2007, 3, 11, 2), std, day, "FREQ=YEARLY;BYMONTH=3;BYDAY=2SU"),
            observance(false, at(2007, 11, 4, 2), day, std, "FREQ=YEARLY;BYMONTH=11;BYDAY=1SU"),
        ],
        Dst::Europe => {
            // 01:00 UTC expressed in local time before the change.
            let start_local = 60 + std;
            let end_local = 60 + day;
            let hm = |mins: i32| (u8::try_from(mins.div_euclid(60)).unwrap_or(1), mins.rem_euclid(60));
            let (sh, _) = hm(start_local);
            let (eh, _) = hm(end_local);
            vec![
                observance(true, at(1996, 3, 31, sh), std, day, "FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU"),
                observance(false, at(1996, 10, 27, eh), day, std, "FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU"),
            ]
        }
        Dst::Australia => vec![
            observance(false, at(2008, 4, 6, 3), day, std, "FREQ=YEARLY;BYMONTH=4;BYDAY=1SU"),
            observance(true, at(2008, 10, 5, 2), std, day, "FREQ=YEARLY;BYMONTH=10;BYDAY=1SU"),
        ],
        Dst::NewZealand => vec![
            observance(false, at(2008, 4, 6, 3), day, std, "FREQ=YEARLY;BYMONTH=4;BYDAY=1SU"),
            observance(true, at(2007, 9, 30, 2), std, day, "FREQ=YEARLY;BYMONTH=9;BYDAY=-1SU"),
        ],
    };
    Some(TimeZone { tzid: tzid.to_owned(), observances })
}

/// The zones a calendar defines plus the built-in fallback.
#[derive(Clone, Debug, Default)]
pub struct Zones {
    pub defined: Vec<TimeZone>,
}

impl Zones {
    /// Zone for `tzid`: a VTIMEZONE from the file, else built-in / fixed-offset, else `None` (floating).
    pub fn resolve(&self, tzid: &str) -> Option<TimeZone> {
        self.get(tzid).map(Cow::into_owned)
    }

    /// [`Zones::resolve`] without copying a zone the file defines.
    pub fn get(&self, tzid: &str) -> Option<Cow<'_, TimeZone>> {
        let t = tzid.trim().trim_matches('"');
        match self.defined.iter().find(|z| z.tzid.eq_ignore_ascii_case(t)) {
            Some(z) => Some(Cow::Borrowed(z)),
            None => builtin(t).map(Cow::Owned),
        }
    }
}

/// A zone plus the onsets listed so far, for many lookups in one zone (an expansion, a file's import).
#[derive(Debug)]
pub struct ZoneLookup<'a> {
    pub zone: Cow<'a, TimeZone>,
    memo: RefCell<OnsetMemo>,
}

impl<'a> ZoneLookup<'a> {
    pub fn new(zone: Cow<'a, TimeZone>) -> Self {
        ZoneLookup { zone, memo: RefCell::new(OnsetMemo::default()) }
    }

    /// [`TimeZone::offset_at_local`].
    pub fn offset_at_local(&self, local: i64) -> i32 {
        match self.memo.try_borrow_mut() {
            Ok(mut m) => self.zone.offset_at_local_memo(local, &mut m),
            Err(_) => self.zone.offset_at_local(local),
        }
    }

    /// [`TimeZone::offset_at_utc`].
    pub fn offset_at_utc(&self, unix: i64) -> i32 {
        match self.memo.try_borrow_mut() {
            Ok(mut m) => self.zone.offset_at_utc_memo(unix, &mut m),
            Err(_) => self.zone.offset_at_utc(unix),
        }
    }

    /// [`TimeZone::to_utc`].
    pub fn to_utc(&self, local: i64) -> i64 {
        self.resolve_local(local).0
    }

    /// [`TimeZone::resolve_local_memo`].
    pub fn resolve_local(&self, local: i64) -> (i64, i32) {
        match self.memo.try_borrow_mut() {
            Ok(mut m) => self.zone.resolve_local_memo(local, &mut m),
            Err(_) => self.zone.resolve_local_memo(local, &mut OnsetMemo::default()),
        }
    }
}
