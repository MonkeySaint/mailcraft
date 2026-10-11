//! Named time zones (IANA ids) with daylight-saving rules, for appointments (`Appointment::tz`), the
//! calendar's own zone and the second zone shown in day and week views (Change Time Zone).
//!
//! Zone rules come from `mailcraft-formats` (its built-in table of common IANA zones with their current DST
//! rules, plus fixed `UTC±hh:mm` names). Every lookup in MailCraft goes through [`zone`], [`offset_at`] and
//! [`to_utc`], so a full tzdb crate (chrono-tz or jiff) can replace the table in one place.

use std::borrow::Cow;

use mailcraft_formats::ics::ZoneLookup;
use mailcraft_formats::ics::rrule::{self, Expand, Rrule};
use mailcraft_formats::ics::tz::{TimeZone, builtin};
use mailcraft_model::{Appointment, DateTime};

/// Zones offered by Change Time Zone (IANA ids, west to east).
pub const ZONES: &[&str] = &[
    "Pacific/Honolulu",
    "America/Anchorage",
    "America/Los_Angeles",
    "America/Vancouver",
    "America/Tijuana",
    "America/Phoenix",
    "America/Denver",
    "America/Edmonton",
    "America/Chicago",
    "America/Winnipeg",
    "America/Regina",
    "America/Mexico_City",
    "America/New_York",
    "America/Toronto",
    "America/Detroit",
    "America/Bogota",
    "America/Lima",
    "America/Halifax",
    "America/Caracas",
    "America/Puerto_Rico",
    "America/St_Johns",
    "America/Sao_Paulo",
    "America/Argentina/Buenos_Aires",
    "Atlantic/Reykjavik",
    "UTC",
    "Europe/London",
    "Europe/Dublin",
    "Europe/Lisbon",
    "Africa/Lagos",
    "Europe/Paris",
    "Europe/Berlin",
    "Europe/Madrid",
    "Europe/Rome",
    "Europe/Amsterdam",
    "Europe/Brussels",
    "Europe/Zurich",
    "Europe/Stockholm",
    "Europe/Warsaw",
    "Africa/Johannesburg",
    "Africa/Cairo",
    "Europe/Athens",
    "Europe/Helsinki",
    "Europe/Kyiv",
    "Europe/Istanbul",
    "Europe/Moscow",
    "Asia/Riyadh",
    "Africa/Nairobi",
    "Asia/Dubai",
    "Asia/Karachi",
    "Asia/Kolkata",
    "Asia/Kathmandu",
    "Asia/Dhaka",
    "Asia/Bangkok",
    "Asia/Shanghai",
    "Asia/Singapore",
    "Asia/Tokyo",
    "Asia/Seoul",
    "Australia/Adelaide",
    "Australia/Brisbane",
    "Australia/Sydney",
    "Australia/Perth",
    "Pacific/Auckland",
];

/// Longest zone id accepted.
const MAX_ID: usize = 64;

/// The zone named `id` (an IANA id or a fixed `UTC±hh:mm` name), when known.
pub fn zone(id: &str) -> Option<TimeZone> {
    let id = id.trim();
    if id.is_empty() || id.len() > MAX_ID || id.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    builtin(id)
}

/// True when [`zone`] knows `id`.
pub fn known(id: &str) -> bool {
    zone(id).is_some()
}

/// The canonical spelling of a known zone id (the [`ZONES`] entry when it is one, else the trimmed input).
pub fn canonical(id: &str) -> Option<String> {
    zone(id)?;
    let t = id.trim();
    Some(ZONES.iter().find(|z| z.eq_ignore_ascii_case(t)).map_or_else(|| t.to_owned(), |z| (*z).to_owned()))
}

/// UTC offset (minutes east) of zone `id` at instant `unix`.
pub fn offset_at(id: &str, unix: i64) -> Option<i32> {
    zone(id).map(|z| z.offset_at_utc(unix))
}

/// Wall-clock time `local` (seconds since 1970-01-01 00:00 local) in zone `id` → (unix seconds, offset). A time
/// skipped by a DST change moves forward past it; a repeated time is the first of the two (RFC 5545 §3.3.5).
pub fn to_utc(id: &str, local: i64) -> Option<(i64, i32)> {
    let z = zone(id)?;
    let l = ZoneLookup::new(Cow::Owned(z));
    Some(l.resolve_local(local))
}

/// `t` re-expressed with zone `id`'s offset at that instant (the same instant).
pub fn in_zone(id: &str, t: DateTime) -> Option<DateTime> {
    offset_at(id, t.unix).map(|off| DateTime::from_unix(t.unix, off))
}

/// "(UTC-04:00) America/Toronto": the offset at `unix` and the id with spaces for underscores.
pub fn label(id: &str, unix: i64) -> String {
    let off = offset_at(id, unix).unwrap_or(0);
    let a = off.unsigned_abs();
    format!(
        "(UTC{}{:02}:{:02}) {}",
        if off < 0 { '-' } else { '+' },
        a / 60,
        a % 60,
        id.trim().replace('_', " ")
    )
}

/// Seconds in a day.
const DAY: i64 = 86_400;

/// The calendar's wall clock: its named time zone (Change Time Zone, daylight-saving aware) when it has one,
/// else the session's fixed display offset. Every place that turns appointment times into wall-clock text or
/// grid positions, or wall-clock input back into instants, goes through one clock, so a time shown after a
/// daylight-saving change reads back as the same instant. "Local seconds" count from 1970-01-01 00:00 on
/// this clock.
#[derive(Debug)]
pub struct Clock {
    fixed: i32,
    zone: Option<(String, ZoneLookup<'static>)>,
}

impl Clock {
    /// A clock at a fixed offset (minutes east of UTC).
    pub fn fixed(offset_min: i32) -> Clock {
        Clock { fixed: offset_min, zone: None }
    }

    /// A clock in zone `id` (an IANA id); a fixed clock at `fallback` when `id` names no known zone.
    pub fn named(id: &str, fallback: i32) -> Clock {
        let zone = canonical(id).and_then(|c| zone(&c).map(|z| (c, ZoneLookup::new(Cow::Owned(z)))));
        Clock { fixed: fallback, zone }
    }

    /// The clock of the session's calendar.
    pub fn of(s: &crate::Session) -> Clock {
        Clock::named(&super::views::time_zones(s).zone, s.tz_offset_min)
    }

    /// The IANA zone, when the clock has one.
    pub fn zone_id(&self) -> Option<&str> {
        self.zone.as_ref().map(|(id, _)| id.as_str())
    }

    /// UTC offset (minutes) at instant `unix`.
    pub fn offset_at(&self, unix: i64) -> i32 {
        self.zone.as_ref().map_or(self.fixed, |(_, z)| z.offset_at_utc(unix))
    }

    /// Local seconds of instant `unix`.
    pub fn local(&self, unix: i64) -> i64 {
        unix.saturating_add(i64::from(self.offset_at(unix)).saturating_mul(60))
    }

    /// `unix` as a date-time carrying this clock's offset then.
    pub fn at(&self, unix: i64) -> DateTime {
        DateTime::from_unix(unix, self.offset_at(unix))
    }

    /// The instant of wall-clock time `local` (a time skipped by a daylight-saving change moves past it; a
    /// repeated one is the first of the two).
    pub fn from_local(&self, local: i64) -> DateTime {
        match &self.zone {
            Some((_, z)) => {
                let (unix, off) = z.resolve_local(local);
                DateTime::from_unix(unix, off)
            }
            None => DateTime::from_unix(
                local.saturating_sub(i64::from(self.fixed).saturating_mul(60)),
                self.fixed,
            ),
        }
    }

    /// Local day number (days since 1970-01-01) of `t`.
    pub fn day(&self, t: DateTime) -> i64 {
        self.local(t.unix).div_euclid(DAY)
    }

    /// Midnight starting the local day of `t`.
    pub fn midnight(&self, t: DateTime) -> DateTime {
        self.from_local(self.day(t).saturating_mul(DAY))
    }

    /// Wall-clock text "YYYY-MM-DD", "YYYY-MM-DDTHH:MM[:SS]" → instant.
    pub fn parse(&self, text: &str) -> Option<DateTime> {
        super::pim::parse_local(text, 0).map(|wall| self.from_local(wall.unix))
    }

    /// "YYYY-MM-DDTHH:MM" of `t` on this clock.
    pub fn datetime_string(&self, t: DateTime) -> String {
        super::pim::datetime_string(t, self.offset_at(t.unix))
    }

    /// Optional date param read on this clock (as `pim::date`): absent → `None`; `null` → `Some(None)`; wall
    /// clock text or a unix time → `Some(Some(t))`.
    pub fn date_param(
        &self,
        p: &serde_json::Value,
        key: &str,
    ) -> Result<Option<Option<DateTime>>, crate::CmdError> {
        match p.get(key) {
            Some(serde_json::Value::String(t)) => self.parse(t).map(|d| Some(Some(d))).ok_or_else(|| {
                crate::CmdError::Params(format!(
                    "`{key}`: expected \"YYYY-MM-DD\", \"YYYY-MM-DDTHH:MM\" or a unix time"
                ))
            }),
            _ => Ok(super::pim::date(p, key, self.fixed)?.map(|d| d.map(|t| self.at(t.unix)))),
        }
    }
}

/// Occurrences of recurring `a` in zone `z` overlapping `[from, to)`: the rule repeats the start's wall-clock
/// time in that zone, so a 09:00 weekly meeting stays at 09:00 across daylight-saving changes.
pub fn expand_zoned(
    a: &Appointment,
    z: TimeZone,
    from: DateTime,
    to: DateTime,
    max: usize,
) -> Vec<(DateTime, DateTime)> {
    let single = || {
        if a.start.unix < to.unix && (a.end.unix > from.unix || a.start.unix >= from.unix) {
            vec![(a.start, a.end)]
        } else {
            Vec::new()
        }
    };
    let Some(rec) = &a.recurrence else { return single() };
    let Ok(rule) = Rrule::parse(&rec.rrule) else { return single() };
    let lookup = ZoneLookup::new(Cow::Owned(z));
    let duration = a.end.unix.saturating_sub(a.start.unix).max(0);
    let start_off = lookup.offset_at_utc(a.start.unix);
    let dtstart = a.start.unix.saturating_add(i64::from(start_off).saturating_mul(60));
    let to_utc = |local: i64| lookup.to_utc(local);
    let exdates = &rec.exdates;
    let excluded = |_local: i64, unix: i64| exdates.iter().any(|x| x.unix == unix);
    let x = Expand { rule: &rule, dtstart, duration, to_utc: &to_utc, excluded: &excluded };
    rrule::expand(&x, from.unix, to.unix, max.min(rrule::MAX_OCCURRENCES))
        .into_iter()
        .map(|o| {
            let off = lookup.offset_at_utc(o.unix);
            let end = o.unix.saturating_add(duration);
            (DateTime::from_unix(o.unix, off), DateTime::from_unix(end, lookup.offset_at_utc(end)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::Recurrence;

    fn local(y: i64, m: u8, d: u8, h: u8, mi: u8) -> i64 {
        mailcraft_model::dates::days_from_civil(y, m, d).unwrap() * 86_400
            + i64::from(h) * 3600
            + i64::from(mi) * 60
    }

    #[test]
    fn every_offered_zone_is_known() {
        for z in ZONES {
            assert!(known(z), "{z}");
            assert_eq!(canonical(&z.to_ascii_lowercase()).as_deref(), Some(*z));
        }
        assert!(!known(""));
        assert!(!known("Mars/Olympus_Mons"));
        assert!(!known("Europe/ Paris"));
        assert!(!known(&"A".repeat(100)));
    }

    #[test]
    fn clocks_read_and_write_wall_clock_time_across_dst() {
        let c = Clock::named("america/toronto", -240);
        assert_eq!(c.zone_id(), Some("America/Toronto"));
        // 09:00 on a November date reads back as 09:00 although the offset differs from October's.
        let nov = c.parse("2026-11-02T09:00").unwrap();
        assert_eq!(nov.unix, local(2026, 11, 2, 14, 0));
        assert_eq!(nov.offset_min, -300);
        assert_eq!(c.datetime_string(nov), "2026-11-02T09:00");
        let oct = c.parse("2026-10-26T09:00").unwrap();
        assert_eq!(oct.unix, local(2026, 10, 26, 13, 0));
        assert_eq!(c.datetime_string(oct), "2026-10-26T09:00");
        // Midnight and day numbers follow the date's own offset.
        assert_eq!(c.midnight(nov).unix, local(2026, 11, 2, 5, 0));
        assert_eq!(c.day(nov), local(2026, 11, 2, 0, 0) / 86_400);
        // The fall-back day is 25 hours long.
        let sun = c.parse("2026-11-01").unwrap();
        let mon = c.parse("2026-11-02").unwrap();
        assert_eq!(mon.unix - sun.unix, 25 * 3600);
        // A fixed clock (no or an unknown zone) is the plain offset.
        for f in [Clock::fixed(-240), Clock::named("", -240), Clock::named("Mars/Base", -240)] {
            assert_eq!(f.zone_id(), None);
            assert_eq!(f.parse("2026-11-02T09:00").unwrap().unix, local(2026, 11, 2, 13, 0));
            assert_eq!(f.datetime_string(nov), "2026-11-02T10:00");
        }
        assert!(c.date_param(&serde_json::json!({"x": "nope"}), "x").is_err());
        assert_eq!(c.date_param(&serde_json::json!({}), "x").unwrap(), None);
        let n = c.date_param(&serde_json::json!({"x": nov.unix}), "x").unwrap().flatten().unwrap();
        assert_eq!((n.unix, n.offset_min), (nov.unix, -300));
    }

    #[test]
    fn daylight_saving_offsets() {
        // Toronto: EDT (-4) in July, EST (-5) in January.
        let (july, _) = to_utc("America/Toronto", local(2026, 7, 1, 12, 0)).unwrap();
        assert_eq!(offset_at("America/Toronto", july), Some(-240));
        let (jan, off) = to_utc("America/Toronto", local(2026, 1, 15, 9, 0)).unwrap();
        assert_eq!(off, -300);
        assert_eq!(jan, local(2026, 1, 15, 14, 0));
        // Paris and Sydney (southern hemisphere).
        assert_eq!(offset_at("Europe/Paris", local(2026, 7, 1, 0, 0)), Some(120));
        assert_eq!(offset_at("Australia/Sydney", local(2026, 7, 1, 0, 0)), Some(600));
        assert_eq!(offset_at("Australia/Sydney", local(2026, 1, 1, 0, 0)), Some(660));
        // A skipped local time (02:30 on the spring-forward day) lands after the change.
        let (gap, off) = to_utc("America/New_York", local(2026, 3, 8, 2, 30)).unwrap();
        assert_eq!(off, -240);
        assert_eq!(gap, local(2026, 3, 8, 7, 30));
        assert_eq!(label("America/New_York", jan), "(UTC-05:00) America/New York");
        assert_eq!(label("Asia/Kolkata", jan), "(UTC+05:30) Asia/Kolkata");
    }

    #[test]
    fn weekly_meeting_keeps_its_wall_clock_time_across_dst() {
        // 09:00 Toronto every Monday from October into November (DST ends 2026-11-01).
        let (start, off) = to_utc("America/Toronto", local(2026, 10, 19, 9, 0)).unwrap();
        let a = Appointment {
            start: DateTime::from_unix(start, off),
            end: DateTime::from_unix(start + 3600, off),
            recurrence: Some(Recurrence { rrule: "FREQ=WEEKLY;COUNT=4".into(), exdates: Vec::new() }),
            tz: Some("America/Toronto".into()),
            ..Default::default()
        };
        let z = zone("America/Toronto").unwrap();
        let occ = expand_zoned(
            &a,
            z,
            DateTime::from_unix(0, 0),
            DateTime::from_unix(local(2027, 1, 1, 0, 0), 0),
            10,
        );
        assert_eq!(occ.len(), 4);
        for (s, e) in &occ {
            let wall = s.unix + i64::from(s.offset_min) * 60;
            assert_eq!(wall.rem_euclid(86_400), 9 * 3600, "{s:?}");
            assert_eq!(e.unix - s.unix, 3600);
        }
        // Before the change UTC 13:00, after it 14:00.
        assert_eq!(occ[1].0.unix.rem_euclid(86_400), 13 * 3600);
        assert_eq!(occ[2].0.unix.rem_euclid(86_400), 14 * 3600);
        // EXDATE removes one occurrence.
        let mut b = a.clone();
        b.recurrence.as_mut().unwrap().exdates.push(occ[2].0);
        let z = zone("America/Toronto").unwrap();
        assert_eq!(
            expand_zoned(
                &b,
                z,
                DateTime::from_unix(0, 0),
                DateTime::from_unix(local(2027, 1, 1, 0, 0), 0),
                10
            )
            .len(),
            3
        );
    }
}
