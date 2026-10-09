//! Small, self-contained civil-calendar helpers for the formats this crate writes and reads itself:
//! the asctime date on MBOX `From ` lines and a minimal RFC 5322 date for rebuilt messages.
//!
//! Proleptic Gregorian calendar, day-count algorithm after the well-known "days from civil" derivation.
//! Every function clamps its input to years 0001..=9999 so no arithmetic can overflow.

use std::time::{SystemTime, UNIX_EPOCH};

use mailcraft_model::DateTime;

/// 0001-01-01T00:00:00Z.
pub(crate) const MIN_UNIX: i64 = -62_135_596_800;
/// 9999-12-31T23:59:59Z.
pub(crate) const MAX_UNIX: i64 = 253_402_300_799;

const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Current wall-clock time as a UTC [`DateTime`].
pub(crate) fn now() -> DateTime {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    DateTime { unix: i64::try_from(secs).unwrap_or(0).clamp(MIN_UNIX, MAX_UNIX), offset_min: 0 }
}

/// Seconds since the epoch, for lock-file timestamps.
pub(crate) fn now_secs() -> i64 {
    now().unix
}

/// Days since 1970-01-01 for a civil date (month 1..=12, day 1..=31; not range-checked beyond that).
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date (year, month, day) for a day count since 1970-01-01.
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// Builds a UTC instant from civil fields; `None` when any field is out of range.
pub(crate) fn unix_from_civil(year: i64, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> Option<i64> {
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    if day == 0 || day > days_in_month(year, month) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + i64::from(hour) * 3600 + i64::from(min) * 60 + i64::from(sec.min(59));
    Some(secs.clamp(MIN_UNIX, MAX_UNIX))
}

struct Broken {
    year: i64,
    month: u32,
    day: u32,
    hour: i64,
    min: i64,
    sec: i64,
    weekday: usize,
}

fn broken(unix: i64) -> Broken {
    let unix = unix.clamp(MIN_UNIX, MAX_UNIX);
    let days = unix.div_euclid(86_400);
    let rem = unix.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let weekday = usize::try_from((days + 4).rem_euclid(7)).unwrap_or(0);
    Broken { year, month, day, hour: rem / 3600, min: (rem % 3600) / 60, sec: rem % 60, weekday }
}

fn month_name(m: u32) -> &'static str {
    usize::try_from(m)
        .ok()
        .and_then(|m| m.checked_sub(1))
        .and_then(|i| MONTHS.get(i))
        .copied()
        .unwrap_or("Jan")
}

/// `Thu Oct  8 04:30:00 2026` (UTC), the classic asctime layout used on MBOX `From ` lines.
pub(crate) fn format_asctime(t: DateTime) -> String {
    let b = broken(t.unix);
    format!(
        "{} {} {:>2} {:02}:{:02}:{:02} {}",
        WEEKDAYS.get(b.weekday).copied().unwrap_or("Sun"),
        month_name(b.month),
        b.day,
        b.hour,
        b.min,
        b.sec,
        b.year
    )
}

/// `Thu, 08 Oct 2026 04:30:00 +0000`, used only when rebuilding a message without a builder result.
pub(crate) fn format_rfc5322_utc(t: DateTime) -> String {
    let b = broken(t.unix);
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} +0000",
        WEEKDAYS.get(b.weekday).copied().unwrap_or("Sun"),
        b.day,
        month_name(b.month),
        b.year,
        b.hour,
        b.min,
        b.sec
    )
}

fn parse_month(tok: &str) -> Option<u32> {
    let lower = tok.get(..3)?.to_ascii_lowercase();
    MONTHS.iter().position(|m| m.eq_ignore_ascii_case(&lower)).and_then(|i| u32::try_from(i + 1).ok())
}

fn parse_clock(tok: &str) -> Option<(u32, u32, u32)> {
    let mut parts = tok.split(':');
    let h = parts.next()?.parse::<u32>().ok()?;
    let m = parts.next()?.parse::<u32>().ok()?;
    let s = match parts.next() {
        Some(s) => s.parse::<u32>().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((h, m, s))
}

fn parse_offset(tok: &str) -> Option<i64> {
    let (sign, digits) = match tok.as_bytes().first()? {
        b'+' => (1, tok.get(1..)?),
        b'-' => (-1, tok.get(1..)?),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i64 = digits.parse().ok()?;
    if v % 100 > 59 {
        return None;
    }
    let mins = (v / 100) * 60 + v % 100;
    if mins > 18 * 60 {
        return None;
    }
    Some(sign * mins)
}

/// Lenient asctime parser for `From ` lines: `Thu Oct  8 04:30:00 2026`, optionally with a numeric zone
/// (`... 2026 -0400` or `... 04:30:00 -0400 2026`), a missing weekday, or the day before the month.
pub(crate) fn parse_asctime(s: &str) -> Option<DateTime> {
    let toks: Vec<&str> = s.split_whitespace().take(12).collect();
    let mut month = None;
    let mut day = None;
    let mut clock = None;
    let mut year = None;
    let mut offset = 0i64;
    for tok in &toks {
        let tok = tok.trim_end_matches(',');
        if month.is_none()
            && tok.len() >= 3
            && tok.bytes().all(|b| b.is_ascii_alphabetic())
            && let Some(m) = parse_month(tok)
        {
            month = Some(m);
            continue;
        }
        if clock.is_none() && tok.contains(':') {
            clock = Some(parse_clock(tok)?);
            continue;
        }
        if let Some(o) = parse_offset(tok) {
            offset = o;
            continue;
        }
        if tok.bytes().all(|b| b.is_ascii_digit()) && !tok.is_empty() {
            if tok.len() == 4 && year.is_none() && (day.is_some() || clock.is_some()) {
                year = tok.parse::<i64>().ok();
            } else if tok.len() <= 2 && day.is_none() {
                day = tok.parse::<u32>().ok();
            } else if tok.len() == 4 && year.is_none() {
                year = tok.parse::<i64>().ok();
            }
        }
    }
    let (h, m, sec) = clock?;
    let unix = unix_from_civil(year?, month?, day?, h, m, sec)?;
    let unix = unix.checked_sub(offset * 60)?.clamp(MIN_UNIX, MAX_UNIX);
    Some(DateTime { unix, offset_min: i32::try_from(offset).unwrap_or(0) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(days_from_civil(2000, 2, 29)), (2000, 2, 29));
        assert_eq!(unix_from_civil(2026, 10, 8, 4, 30, 0), Some(1_791_433_800));
        assert_eq!(unix_from_civil(2026, 2, 29, 0, 0, 0), None);
        assert_eq!(unix_from_civil(2024, 2, 29, 0, 0, 0).map(|u| u > 0), Some(true));
    }

    #[test]
    fn asctime_round_trip() {
        let t = DateTime { unix: 1_791_433_800, offset_min: 0 };
        let s = format_asctime(t);
        assert_eq!(s, "Thu Oct  8 04:30:00 2026");
        assert_eq!(parse_asctime(&s), Some(t));
        assert_eq!(format_rfc5322_utc(t), "Thu, 08 Oct 2026 04:30:00 +0000");
    }

    #[test]
    fn asctime_variants() {
        let base = parse_asctime("Thu Oct  8 04:30:00 2026").map(|d| d.unix);
        assert_eq!(parse_asctime("Oct 8 04:30:00 2026").map(|d| d.unix), base);
        assert_eq!(parse_asctime("Thu, 8 Oct 2026 04:30:00").map(|d| d.unix), base);
        assert_eq!(parse_asctime("Thu Oct  8 00:30:00 2026 -0400").map(|d| d.unix), base);
        assert_eq!(parse_asctime("Thu Oct  8 04:30 2026").map(|d| d.unix), base);
        assert_eq!(parse_asctime("garbage"), None);
        assert_eq!(parse_asctime("Thu Foo 8 04:30:00 2026"), None);
        assert_eq!(parse_asctime("Thu Oct 32 04:30:00 2026"), None);
    }

    #[test]
    fn extremes_do_not_overflow() {
        for u in [i64::MIN, i64::MAX, MIN_UNIX, MAX_UNIX, 0, -1] {
            let t = DateTime { unix: u, offset_min: 0 };
            let s = format_asctime(t);
            assert!(!s.is_empty());
            let _ = parse_asctime(&s);
            let _ = format_rfc5322_utc(t);
        }
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    #[test]
    fn offset_minutes_must_be_valid() {
        assert_eq!(
            parse_asctime("Thu Oct  8 04:30:00 2026 +0099"),
            parse_asctime("Thu Oct  8 04:30:00 2026")
        );
        assert_eq!(parse_offset("+0099"), None);
        assert_eq!(parse_offset("-0530"), Some(-330));
    }
}
