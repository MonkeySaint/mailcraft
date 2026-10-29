//! JSCalendar events (RFC 8984, as JMAP for Calendars serves them) to and from [`Appointment`]s: one event
//! with its recurrence overrides is a series and its exceptions here (sharing the UID), as one iCalendar
//! resource is in CalDAV.
//!
//! Mapping: `title`, `description`, the first `locations` entry's name, `start` + `timeZone` (an IANA zone
//! MailCraft knows; `null` is floating time, read in the user's zone) + `duration`, `showWithoutTime` (all
//! day), `freeBusyStatus` (free / busy), `privacy` (private, secret → private), `priority` (1–4 high, 6–9
//! low), `keywords` → categories, the first `alerts` entry with an offset before the start → reminder,
//! `participants` (owner → organizer, attendee → attendees, with their answers), `recurrenceRule`
//! (JSCalendar 2.0, as JMAP for Calendars has it; the first entry of RFC 8984's `recurrenceRules` is read
//! too) ↔ RRULE, and `recurrenceOverrides` (`excluded` → an exception date, a patch → an
//! exception: the series with the patch applied).
//!
//! An appointment without a time zone of its own is written in UTC. Server data is untrusted: lists are
//! capped, times bounded, and nothing panics on odd input.

use mailcraft_formats::ics::tz::{TimeZone, builtin, builtin_name};
use mailcraft_formats::ics::value::{civil, format_duration, local_secs, parse_duration};
use mailcraft_mime::Mailbox;
use mailcraft_model::calendar::{AttendeeRole, ResponseStatus};
use mailcraft_model::{Appointment, Attendee, BusyStatus, DateTime, Importance, Recurrence};
use serde_json::{Map, Value, json};

/// The event properties MailCraft writes (an update sets those the edit changed, `null` when the series has
/// none any more; see `jmap_pim.rs`).
pub(super) const MAPPED: [&str; 15] = [
    "title",
    "description",
    "start",
    "duration",
    "timeZone",
    "showWithoutTime",
    "locations",
    "freeBusyStatus",
    "privacy",
    "priority",
    "keywords",
    "alerts",
    "participants",
    "recurrenceRule",
    "recurrenceOverrides",
];

/// Sets `key` of the JSON object `v` (nothing when `v` is not an object).
fn set(v: &mut Value, key: &str, x: Value) {
    if let Some(m) = v.as_object_mut() {
        m.insert(key.to_owned(), x);
    }
}

/// Most overrides (exceptions and excluded dates) read from one event.
const MAX_OVERRIDES: usize = 500;
/// Most participants read from one event.
const MAX_PARTICIPANTS: usize = 200;
/// Longest text kept from one value.
const MAX_TEXT: usize = 256 * 1024;
/// Longest duration read (ten years).
const MAX_DURATION: i64 = 10 * 366 * 86_400;
const DAY: i64 = 86_400;

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).map(|s| s.chars().take(MAX_TEXT).collect::<String>()).unwrap_or_default()
}

/// Where an event's local times are.
enum Zone {
    /// A zone MailCraft knows (its IANA name, as MailCraft spells it).
    Named(TimeZone, String),
    /// UTC, a fixed offset, or a zone MailCraft does not know (read as UTC).
    Fixed(i32),
    /// Floating time: the user's zone.
    Floating,
}

fn zone_of(tz: Option<&str>) -> Zone {
    let Some(name) = tz.map(str::trim).filter(|t| !t.is_empty()) else { return Zone::Floating };
    match (builtin_name(name), builtin(name)) {
        (Some(canonical), Some(z)) => Zone::Named(z, canonical.to_owned()),
        (None, Some(z)) => Zone::Fixed(z.offset_at_utc(0)),
        _ => Zone::Fixed(0),
    }
}

/// `YYYY-MM-DDTHH:MM:SS` (a LocalDateTime; a trailing `Z` or fraction tolerated) → local seconds.
fn parse_local(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 || b.get(4) != Some(&b'-') || b.get(10) != Some(&b'T') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let small = |r: std::ops::Range<usize>| n(r).and_then(|v| u8::try_from(v).ok());
    local_secs(n(0..4)?, small(5..7)?, small(8..10)?, small(11..13)?, small(14..16)?, small(17..19)?)
}

fn format_local(secs: i64) -> String {
    let (y, m, d, h, mi, s) = civil(secs);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}

/// Local seconds in `zone` → (Unix time, offset in force).
fn to_unix(local: i64, zone: &Zone, user_offset: i32) -> (i64, i32) {
    match zone {
        Zone::Named(z, _) => {
            let unix = z.to_utc(local);
            (unix, z.offset_at_utc(unix))
        }
        Zone::Fixed(off) => (local.saturating_sub(i64::from(*off) * 60), *off),
        Zone::Floating => (local.saturating_sub(i64::from(user_offset) * 60), user_offset),
    }
}

/// Unix time → local seconds in `zone`.
fn to_local(unix: i64, zone: &Zone, user_offset: i32) -> i64 {
    let off = match zone {
        Zone::Named(z, _) => z.offset_at_utc(unix),
        Zone::Fixed(off) => *off,
        Zone::Floating => user_offset,
    };
    unix.saturating_add(i64::from(off) * 60)
}

// ---------------------------------------------------------------------------------------------------------
// RRULE ↔ RecurrenceRule
// ---------------------------------------------------------------------------------------------------------

const NUMBER_PARTS: [(&str, &str); 7] = [
    ("BYMONTHDAY", "byMonthDay"),
    ("BYYEARDAY", "byYearDay"),
    ("BYWEEKNO", "byWeekNo"),
    ("BYHOUR", "byHour"),
    ("BYMINUTE", "byMinute"),
    ("BYSECOND", "bySecond"),
    ("BYSETPOS", "bySetPosition"),
];

/// An RFC 5545 RRULE value as a RecurrenceRule (`UNTIL` as local time in `zone`).
fn rule_object(rrule: &str, zone: &Zone, user_offset: i32, all_day: bool) -> Option<Value> {
    let mut o = Map::new();
    o.insert("@type".into(), json!("RecurrenceRule"));
    for part in rrule.split(';').take(32) {
        let Some((k, v)) = part.split_once('=') else { continue };
        let k = k.trim().to_ascii_uppercase();
        let v = v.trim();
        let list = || v.split(',').map(str::trim).filter(|x| !x.is_empty()).take(400);
        match k.as_str() {
            "FREQ" => {
                o.insert("frequency".into(), json!(v.to_ascii_lowercase()));
            }
            "INTERVAL" => {
                if let Ok(n) = v.parse::<u32>() {
                    o.insert("interval".into(), json!(n));
                }
            }
            "COUNT" => {
                if let Ok(n) = v.parse::<u32>() {
                    o.insert("count".into(), json!(n));
                }
            }
            "UNTIL" => {
                let digits: String = v.chars().filter(char::is_ascii_alphanumeric).collect();
                let date = |d: &str| -> Option<i64> {
                    let n = |r: std::ops::Range<usize>| d.get(r)?.parse::<i64>().ok();
                    let small = |r: std::ops::Range<usize>| n(r).and_then(|x| u8::try_from(x).ok());
                    let (y, m, dd) = (n(0..4)?, small(4..6)?, small(6..8)?);
                    let (h, mi, s) = if d.len() >= 15 {
                        (small(9..11)?, small(11..13)?, small(13..15)?)
                    } else {
                        (0, 0, 0)
                    };
                    local_secs(y, m, dd, h, mi, s)
                };
                if let Some(secs) = date(&digits) {
                    let local = if digits.ends_with('Z') && !all_day {
                        to_local(secs, zone, user_offset)
                    } else {
                        secs
                    };
                    o.insert("until".into(), json!(format_local(local)));
                }
            }
            "WKST" => {
                o.insert("firstDayOfWeek".into(), json!(v.to_ascii_lowercase()));
            }
            "BYDAY" => {
                let days: Vec<Value> = list()
                    .filter_map(|d| {
                        let split = d.len().checked_sub(2)?;
                        let (nth, day) = (d.get(..split)?, d.get(split..)?);
                        let mut nd = json!({"@type": "NDay", "day": day.to_ascii_lowercase()});
                        if !nth.is_empty() {
                            set(
                                &mut nd,
                                "nthOfPeriod",
                                json!(nth.trim_start_matches('+').parse::<i32>().ok()?),
                            );
                        }
                        Some(nd)
                    })
                    .collect();
                o.insert("byDay".into(), json!(days));
            }
            "BYMONTH" => {
                let months: Vec<Value> = list().map(|m| json!(m)).collect();
                o.insert("byMonth".into(), json!(months));
            }
            _ => {
                if let Some((_, name)) = NUMBER_PARTS.iter().find(|(p, _)| *p == k) {
                    let nums: Vec<Value> =
                        list().filter_map(|x| x.parse::<i32>().ok()).map(|x| json!(x)).collect();
                    o.insert((*name).into(), json!(nums));
                }
            }
        }
    }
    o.contains_key("frequency").then_some(Value::Object(o))
}

/// A RecurrenceRule as an RRULE value (`until` read as local time in `zone`, written in UTC).
fn rule_text(r: &Value, zone: &Zone, user_offset: i32, all_day: bool) -> Option<String> {
    let freq = r.get("frequency")?.as_str()?.to_ascii_uppercase();
    if !matches!(
        freq.as_str(),
        "YEARLY" | "MONTHLY" | "WEEKLY" | "DAILY" | "HOURLY" | "MINUTELY" | "SECONDLY"
    ) {
        return None;
    }
    let mut parts = vec![format!("FREQ={freq}")];
    if let Some(n) = r.get("interval").and_then(Value::as_u64).filter(|n| *n > 1 && *n < 100_000) {
        parts.push(format!("INTERVAL={n}"));
    }
    if let Some(n) = r.get("count").and_then(Value::as_u64).filter(|n| *n < 1_000_000) {
        parts.push(format!("COUNT={n}"));
    } else if let Some(local) = r.get("until").and_then(Value::as_str).and_then(parse_local) {
        if all_day {
            let (y, m, d, ..) = civil(local);
            parts.push(format!("UNTIL={y:04}{m:02}{d:02}"));
        } else {
            let (unix, _) = to_unix(local, zone, user_offset);
            let (y, m, d, h, mi, s) = civil(unix);
            parts.push(format!("UNTIL={y:04}{m:02}{d:02}T{h:02}{mi:02}{s:02}Z"));
        }
    }
    let day_ok = |d: &str| matches!(d, "MO" | "TU" | "WE" | "TH" | "FR" | "SA" | "SU");
    if let Some(days) = r.get("byDay").and_then(Value::as_array) {
        let list: Vec<String> = days
            .iter()
            .take(400)
            .filter_map(|nd| {
                let day = nd.get("day")?.as_str()?.to_ascii_uppercase();
                if !day_ok(&day) {
                    return None;
                }
                Some(match nd.get("nthOfPeriod").and_then(Value::as_i64).filter(|n| n.unsigned_abs() <= 53) {
                    Some(n) => format!("{n}{day}"),
                    None => day,
                })
            })
            .collect();
        if !list.is_empty() {
            parts.push(format!("BYDAY={}", list.join(",")));
        }
    }
    if let Some(months) = r.get("byMonth").and_then(Value::as_array) {
        let list: Vec<String> = months
            .iter()
            .take(12)
            .filter_map(|m| m.as_str().map(str::to_owned).or_else(|| m.as_u64().map(|n| n.to_string())))
            .filter(|m| m.trim_end_matches('L').parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)))
            .collect();
        if !list.is_empty() {
            parts.push(format!("BYMONTH={}", list.join(",")));
        }
    }
    for (part, name) in NUMBER_PARTS {
        if let Some(nums) = r.get(name).and_then(Value::as_array) {
            let list: Vec<String> = nums
                .iter()
                .take(400)
                .filter_map(Value::as_i64)
                .filter(|n| n.unsigned_abs() <= 400)
                .map(|n| n.to_string())
                .collect();
            if !list.is_empty() {
                parts.push(format!("{part}={}", list.join(",")));
            }
        }
    }
    if let Some(w) = r.get("firstDayOfWeek").and_then(Value::as_str).map(str::to_ascii_uppercase)
        && day_ok(&w)
    {
        parts.push(format!("WKST={w}"));
    }
    Some(parts.join(";"))
}

// ---------------------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------------------

fn mailbox_of(p: &Value) -> Option<Mailbox> {
    let addr = p
        .get("sendTo")
        .and_then(Value::as_object)
        .and_then(|m| {
            m.values().filter_map(Value::as_str).find(|u| u.to_ascii_lowercase().starts_with("mailto:"))
        })
        .and_then(|u| u.get(7..))
        .map(str::to_owned)
        .or_else(|| p.get("email").and_then(Value::as_str).map(str::to_owned))?;
    let addr: String = addr.trim().chars().take(320).collect();
    if addr.is_empty() {
        return None;
    }
    let name = text(p.get("name"));
    Some(Mailbox { name: (!name.trim().is_empty()).then_some(name), addr })
}

/// One appointment from an event (or an override already merged into the series), without recurrence.
fn appointment_of(ev: &Value, user_offset: i32) -> Option<Appointment> {
    let zone = zone_of(ev.get("timeZone").and_then(Value::as_str));
    let all_day = ev.get("showWithoutTime").and_then(Value::as_bool) == Some(true);
    let local = parse_local(ev.get("start")?.as_str()?)?;
    let (start, end, tz) = if all_day {
        let day = local.div_euclid(DAY) * DAY;
        let unix = day.saturating_sub(i64::from(user_offset) * 60);
        let dur = ev.get("duration").and_then(Value::as_str).and_then(parse_duration).unwrap_or(DAY);
        let days = (dur.clamp(DAY, MAX_DURATION) + DAY - 1) / DAY;
        (
            DateTime::from_unix(unix, user_offset),
            DateTime::from_unix(unix.saturating_add(days * DAY), user_offset),
            None,
        )
    } else {
        let (unix, off) = to_unix(local, &zone, user_offset);
        let dur = ev.get("duration").and_then(Value::as_str).and_then(parse_duration).unwrap_or(0);
        let dur = dur.clamp(0, MAX_DURATION);
        let tz = match &zone {
            Zone::Named(_, name) => Some(name.clone()),
            _ => None,
        };
        (DateTime::from_unix(unix, off), DateTime::from_unix(unix.saturating_add(dur), off), tz)
    };
    let location = ev
        .get("locations")
        .and_then(Value::as_object)
        .and_then(|m| {
            let mut l: Vec<(&String, &Value)> = m.iter().collect();
            l.sort_by(|a, b| a.0.cmp(b.0));
            l.into_iter().map(|(_, v)| text(v.get("name"))).find(|n| !n.is_empty())
        })
        .unwrap_or_default();
    let show_as = match ev.get("freeBusyStatus").and_then(Value::as_str) {
        Some("free") => BusyStatus::Free,
        _ => BusyStatus::Busy,
    };
    let importance = match ev.get("priority").and_then(Value::as_u64) {
        Some(1..=4) => Importance::High,
        Some(6..=9) => Importance::Low,
        _ => Importance::Normal,
    };
    let reminder_min = ev.get("alerts").and_then(Value::as_object).and_then(|m| {
        m.values().take(50).find_map(|a| {
            let t = a.get("trigger")?;
            if t.get("relativeTo").and_then(Value::as_str).is_some_and(|r| r != "start") {
                return None;
            }
            let secs = parse_duration(t.get("offset")?.as_str()?)?;
            u32::try_from((-secs).div_euclid(60)).ok().filter(|m| *m <= 60 * 24 * 7 * 52)
        })
    });
    let mut organizer = None;
    let mut attendees = Vec::new();
    if let Some(ps) = ev.get("participants").and_then(Value::as_object) {
        let mut list: Vec<(&String, &Value)> = ps.iter().take(MAX_PARTICIPANTS).collect();
        list.sort_by(|a, b| a.0.cmp(b.0));
        for (_, p) in list {
            let Some(mb) = mailbox_of(p) else { continue };
            let role = |r: &str| p.get("roles").and_then(|x| x.get(r)).and_then(Value::as_bool) == Some(true);
            if role("owner") && organizer.is_none() {
                organizer = Some(mb.clone());
                if !role("attendee") {
                    continue;
                }
            }
            let response = match p.get("participationStatus").and_then(Value::as_str) {
                Some("accepted") => ResponseStatus::Accepted,
                Some("tentative") => ResponseStatus::Tentative,
                Some("declined") => ResponseStatus::Declined,
                _ => ResponseStatus::None,
            };
            let role = if p.get("kind").and_then(Value::as_str) == Some("resource") {
                AttendeeRole::Resource
            } else if role("optional") {
                AttendeeRole::Optional
            } else {
                AttendeeRole::Required
            };
            if organizer.as_ref().is_some_and(|o| o.addr.eq_ignore_ascii_case(&mb.addr)) {
                continue;
            }
            attendees.push(Attendee { mailbox: mb, role, response });
        }
    }
    Some(Appointment {
        uid: text(ev.get("uid")),
        subject: text(ev.get("title")),
        body: text(ev.get("description")),
        location,
        start,
        end,
        all_day,
        show_as,
        reminder_min,
        is_meeting: !attendees.is_empty(),
        organizer: if attendees.is_empty() { None } else { organizer },
        attendees,
        categories: ev
            .get("keywords")
            .and_then(Value::as_object)
            .map(|k| {
                k.iter()
                    .filter(|(_, on)| on.as_bool() == Some(true))
                    .map(|(k, _)| k.clone())
                    .take(50)
                    .collect()
            })
            .unwrap_or_default(),
        private: matches!(ev.get("privacy").and_then(Value::as_str), Some("private" | "secret")),
        importance,
        tz,
        ..Default::default()
    })
}

/// Applies a PatchObject (RFC 8620 section 5.3: keys are JSON pointers, `null` removes) to `base`.
fn apply_patch(base: &mut Value, patch: &Map<String, Value>) {
    fn set_path(v: &mut Value, segs: &[String], val: &Value) {
        let Some((first, rest)) = segs.split_first() else { return };
        let Some(m) = v.as_object_mut() else { return };
        if rest.is_empty() {
            if val.is_null() {
                m.remove(first);
            } else {
                m.insert(first.clone(), val.clone());
            }
        } else if let Some(next) = m.get_mut(first) {
            set_path(next, rest, val);
        }
    }
    for (path, v) in patch.iter().take(200) {
        let segs: Vec<String> = path
            .trim_start_matches('/')
            .split('/')
            .take(32)
            .map(|s| s.replace("~1", "/").replace("~0", "~"))
            .collect();
        set_path(base, &segs, v);
    }
}

/// The appointments an event describes (the series first, then its exceptions) and its UID. `None` for
/// something that is not an event with a start.
pub(super) fn to_appointments(ev: &Value, user_offset: i32) -> Option<(Vec<Appointment>, String)> {
    let mut series = appointment_of(ev, user_offset)?;
    let uid = series.uid.clone();
    let zone = zone_of(ev.get("timeZone").and_then(Value::as_str));
    let rule = ev
        .get("recurrenceRule")
        .filter(|r| r.is_object())
        .or_else(|| ev.get("recurrenceRules").and_then(Value::as_array).and_then(|r| r.first()))
        .and_then(|r| rule_text(r, &zone, user_offset, series.all_day));
    let mut out = Vec::new();
    let mut exdates = Vec::new();
    let mut base = ev.clone();
    if let Some(m) = base.as_object_mut() {
        for k in [
            "recurrenceRule",
            "recurrenceRules",
            "recurrenceOverrides",
            "excludedRecurrenceRules",
            "recurrenceId",
        ] {
            m.remove(k);
        }
    }
    if let Some(overrides) = ev.get("recurrenceOverrides").and_then(Value::as_object) {
        for (key, patch) in overrides.iter().take(MAX_OVERRIDES) {
            let Some(local) = parse_local(key) else { continue };
            let rid = if series.all_day {
                let unix = (local.div_euclid(DAY) * DAY).saturating_sub(i64::from(user_offset) * 60);
                DateTime::from_unix(unix, user_offset)
            } else {
                let (unix, off) = to_unix(local, &zone, user_offset);
                DateTime::from_unix(unix, off)
            };
            let Some(p) = patch.as_object() else { continue };
            if p.get("excluded").and_then(Value::as_bool) == Some(true) {
                exdates.push(rid);
                continue;
            }
            if p.is_empty() {
                continue;
            }
            let mut inst = base.clone();
            // An override without its own start keeps the occurrence's time.
            if let Some(m) = inst.as_object_mut() {
                m.insert("start".into(), json!(key));
            }
            apply_patch(&mut inst, p);
            if let Some(mut a) = appointment_of(&inst, user_offset) {
                a.uid.clone_from(&uid);
                a.recurrence_id = Some(rid);
                out.push(a);
            }
        }
    }
    if let Some(rrule) = rule {
        exdates.sort_by_key(|d| d.unix);
        series.recurrence = Some(Recurrence { rrule, exdates });
    }
    out.insert(0, series);
    Some((out, uid))
}

// ---------------------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------------------

/// The zone an appointment is written in.
fn zone_for(a: &Appointment) -> Zone {
    match a.zone().and_then(builtin_name) {
        Some(name) => match builtin(name) {
            Some(z) => Zone::Named(z, name.to_owned()),
            None => Zone::Fixed(0),
        },
        None => Zone::Fixed(0),
    }
}

/// The scheduling properties of one appointment (no uid, recurrence or overrides).
fn props_of(a: &Appointment, zone: &Zone) -> Map<String, Value> {
    let mut m = Map::new();
    let null = Value::Null;
    m.insert("title".into(), json!(a.subject));
    m.insert("description".into(), if a.body.is_empty() { null.clone() } else { json!(a.body) });
    let end = a.end.unix.max(a.start.unix);
    if a.all_day {
        let local = a.start.unix.saturating_add(i64::from(a.start.offset_min) * 60);
        let days = ((end - a.start.unix + DAY - 1) / DAY).max(1);
        m.insert("start".into(), json!(format_local(local.div_euclid(DAY) * DAY)));
        m.insert("duration".into(), json!(format!("P{days}D")));
        m.insert("timeZone".into(), null.clone());
        m.insert("showWithoutTime".into(), json!(true));
    } else {
        m.insert("start".into(), json!(format_local(to_local(a.start.unix, zone, 0))));
        m.insert("duration".into(), json!(format_duration(end - a.start.unix)));
        let tz = match zone {
            Zone::Named(_, name) => name.clone(),
            _ => "Etc/UTC".to_owned(),
        };
        m.insert("timeZone".into(), json!(tz));
        m.insert("showWithoutTime".into(), json!(false));
    }
    m.insert(
        "locations".into(),
        if a.location.is_empty() {
            null.clone()
        } else {
            json!({"l1": {"@type": "Location", "name": a.location}})
        },
    );
    m.insert("freeBusyStatus".into(), json!(if a.show_as == BusyStatus::Free { "free" } else { "busy" }));
    m.insert("privacy".into(), json!(if a.private { "private" } else { "public" }));
    m.insert(
        "priority".into(),
        json!(match a.importance {
            Importance::High => 1,
            Importance::Low => 9,
            Importance::Normal => 0,
        }),
    );
    m.insert(
        "keywords".into(),
        if a.categories.is_empty() {
            null.clone()
        } else {
            Value::Object(a.categories.iter().map(|c| (c.clone(), json!(true))).collect())
        },
    );
    m.insert(
        "alerts".into(),
        match a.reminder_min {
            Some(min) => json!({"a1": {"@type": "Alert", "action": "display",
                "trigger": {"@type": "OffsetTrigger", "relativeTo": "start",
                    "offset": format_duration(-i64::from(min) * 60)}}}),
            None => null.clone(),
        },
    );
    let mut parts = Map::new();
    if a.is_meeting || !a.attendees.is_empty() {
        if let Some(o) = &a.organizer {
            parts.insert(
                "p0".into(),
                json!({"@type": "Participant", "name": o.name.clone().unwrap_or_default(),
                    "sendTo": {"imip": format!("mailto:{}", o.addr)}, "roles": {"owner": true}}),
            );
        }
        for (i, at) in a.attendees.iter().enumerate() {
            let mut roles = json!({"attendee": true});
            if at.role == AttendeeRole::Optional {
                set(&mut roles, "optional", json!(true));
            }
            let status = match at.response {
                ResponseStatus::Accepted => "accepted",
                ResponseStatus::Tentative => "tentative",
                ResponseStatus::Declined => "declined",
                ResponseStatus::None => "needs-action",
            };
            let mut p = json!({"@type": "Participant", "name": at.mailbox.name.clone().unwrap_or_default(),
                "sendTo": {"imip": format!("mailto:{}", at.mailbox.addr)}, "roles": roles,
                "participationStatus": status});
            if at.role == AttendeeRole::Resource {
                set(&mut p, "kind", json!("resource"));
            }
            parts.insert(format!("p{}", i + 1), p);
        }
    }
    m.insert("participants".into(), if parts.is_empty() { null } else { Value::Object(parts) });
    m
}

/// The event for a series and its exceptions (or one appointment): every [`MAPPED`] property (`null` where
/// the series has none), plus `@type` and `uid`.
pub(super) fn from_appointments(items: &[Appointment], uid: &str) -> Map<String, Value> {
    let series = items.iter().find(|a| a.recurrence_id.is_none()).or_else(|| items.first());
    let mut m = Map::new();
    m.insert("@type".into(), json!("Event"));
    m.insert("uid".into(), json!(uid));
    let Some(series) = series else { return m };
    let zone = zone_for(series);
    m.extend(props_of(series, &zone));
    let rule = series
        .recurrence
        .as_ref()
        .and_then(|r| rule_object(&r.rrule, &zone, series.start.offset_min, series.all_day));
    let key = |t: DateTime| -> String {
        if series.all_day {
            let local = t.unix.saturating_add(i64::from(series.start.offset_min) * 60);
            format_local(local.div_euclid(DAY) * DAY)
        } else {
            format_local(to_local(t.unix, &zone, 0))
        }
    };
    let mut overrides = Map::new();
    if let Some(r) = &series.recurrence {
        for x in r.exdates.iter().take(MAX_OVERRIDES) {
            overrides.insert(key(*x), json!({"excluded": true}));
        }
    }
    for ex in items.iter().filter(|a| a.recurrence_id.is_some()).take(MAX_OVERRIDES) {
        let Some(rid) = ex.recurrence_id else { continue };
        let props = props_of(ex, &zone_for(ex));
        // Only what differs from the series.
        let patch: Map<String, Value> =
            props.into_iter().filter(|(k, v)| m.get(k).is_none_or(|s| s != v)).collect();
        overrides.insert(key(rid), Value::Object(patch));
    }
    m.insert("recurrenceRule".into(), rule.unwrap_or(Value::Null));
    m.insert(
        "recurrenceOverrides".into(),
        if overrides.is_empty() || series.recurrence.is_none() {
            Value::Null
        } else {
            Value::Object(overrides)
        },
    );
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u8, d: u8, h: u8) -> i64 {
        DateTime::from_civil(y, mo, d, h, 0, 0, 0).unwrap().unix
    }

    #[test]
    fn a_series_with_an_exception_and_an_excluded_date() {
        let ev = json!({
            "@type": "Event", "uid": "s-1", "title": "Weekly sync", "start": "2026-11-12T14:00:00",
            "timeZone": "Etc/UTC", "duration": "PT1H", "showWithoutTime": false,
            "locations": {"l1": {"@type": "Location", "name": "Room 1"}},
            "recurrenceRules": [{"@type": "RecurrenceRule", "frequency": "weekly", "count": 6,
                "byDay": [{"@type": "NDay", "day": "th"}]}],
            "recurrenceOverrides": {
                "2026-11-19T14:00:00": {"title": "Weekly sync (moved)", "start": "2026-11-19T16:00:00"},
                "2026-11-26T14:00:00": {"excluded": true},
                "2026-12-03T14:00:00": {"locations/l1/name": "Room 2"},
            },
            "alerts": {"a": {"trigger": {"@type": "OffsetTrigger", "offset": "-PT15M"}}},
            "priority": 1, "keywords": {"Work": true}, "privacy": "private",
        });
        let (items, uid) = to_appointments(&ev, 120).unwrap();
        assert_eq!(uid, "s-1");
        assert_eq!(items.len(), 3, "{items:?}");
        let s = &items[0];
        assert_eq!(s.start.unix, at(2026, 11, 12, 14));
        assert_eq!(s.end.unix - s.start.unix, 3600);
        assert_eq!(s.location, "Room 1");
        let r = s.recurrence.as_ref().unwrap();
        assert_eq!(r.rrule, "FREQ=WEEKLY;COUNT=6;BYDAY=TH");
        assert_eq!(r.exdates.iter().map(|d| d.unix).collect::<Vec<_>>(), [at(2026, 11, 26, 14)]);
        assert_eq!(s.reminder_min, Some(15));
        assert_eq!((s.importance, s.private), (Importance::High, true));
        assert_eq!(s.categories, ["Work"]);
        let moved = items.iter().find(|a| a.subject == "Weekly sync (moved)").unwrap();
        assert_eq!(moved.recurrence_id.unwrap().unix, at(2026, 11, 19, 14));
        assert_eq!(moved.start.unix, at(2026, 11, 19, 16));
        let room2 = items.iter().find(|a| a.location == "Room 2").unwrap();
        assert_eq!(room2.start.unix, at(2026, 12, 3, 14), "keeps the occurrence's time");
        // Written and read again.
        let back = Value::Object(from_appointments(&items, &uid));
        let (again, _) = to_appointments(&back, 120).unwrap();
        assert_eq!(again.len(), 3);
        let strip = |v: &[Appointment]| {
            let mut v: Vec<(i64, i64, String, String, Option<i64>)> = v
                .iter()
                .map(|a| {
                    (
                        a.start.unix,
                        a.end.unix,
                        a.subject.clone(),
                        a.location.clone(),
                        a.recurrence_id.map(|r| r.unix),
                    )
                })
                .collect();
            v.sort();
            v
        };
        assert_eq!(strip(&again), strip(&items));
        assert_eq!(again[0].recurrence, items[0].recurrence);
    }

    #[test]
    fn zones_all_day_and_until() {
        let ev = json!({"uid": "z", "title": "Standup", "start": "2026-07-01T09:00:00",
            "timeZone": "America/New_York", "duration": "PT15M",
            "recurrenceRules": [{"frequency": "daily", "until": "2026-07-10T09:00:00"}]});
        let (items, _) = to_appointments(&ev, 0).unwrap();
        let a = &items[0];
        assert_eq!(a.start.unix, at(2026, 7, 1, 13), "09:00 EDT is 13:00 UTC");
        assert_eq!(a.tz.as_deref(), Some("America/New_York"));
        assert_eq!(a.recurrence.as_ref().unwrap().rrule, "FREQ=DAILY;UNTIL=20260710T130000Z");
        let back = from_appointments(&items, "z");
        assert_eq!(back["start"], "2026-07-01T09:00:00");
        assert_eq!(back["timeZone"], "America/New_York");
        assert_eq!(back["recurrenceRule"]["until"], "2026-07-10T09:00:00");

        let ev = json!({"uid": "d", "title": "Holiday", "start": "2026-10-12T00:00:00", "showWithoutTime": true,
            "duration": "P2D"});
        let (items, _) = to_appointments(&ev, -300).unwrap();
        let a = &items[0];
        assert!(a.all_day);
        assert_eq!(a.end.unix - a.start.unix, 2 * DAY);
        let back = from_appointments(&items, "d");
        assert_eq!(
            (back["start"].as_str(), back["duration"].as_str()),
            (Some("2026-10-12T00:00:00"), Some("P2D"))
        );
    }

    #[test]
    fn odd_events_never_panic() {
        for v in [
            json!(null),
            json!({"start": "nonsense"}),
            json!({"start": "2026-13-45T99:99:99"}),
            json!({"start": "2026-01-01T00:00:00", "duration": "P99999999999D", "recurrenceRules": [{"frequency": 5}],
                "recurrenceOverrides": {"x": {"excluded": true}, "2026-01-02T00:00:00": "y"},
                "participants": {"a": {"sendTo": {"imip": "mailto:"}}, "b": 3}, "alerts": {"a": {"trigger": {"offset": "zz"}}}}),
            json!({"start": "2026-01-01T00:00:00", "recurrenceOverrides": {"2026-01-02T00:00:00": {"a/b/c": 1, "": null}}}),
        ] {
            if let Some((items, uid)) = to_appointments(&v, 0) {
                let _ = from_appointments(&items, &uid);
            }
        }
        assert!(rule_object("FREQ=WEEKLY;BYDAY=X,1;UNTIL=zz", &Zone::Fixed(0), 0, false).is_some());
        assert!(rule_text(&json!({"frequency": "fortnightly"}), &Zone::Fixed(0), 0, false).is_none());
    }
}
