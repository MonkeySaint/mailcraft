//! Dates for IMAP INTERNALDATE, SEARCH and RFC 5322 `Date:` fields, without a date library.

use std::time::{SystemTime, UNIX_EPOCH};

/// A point in time with the UTC offset it was expressed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    /// Seconds since the Unix epoch (UTC).
    pub unix: i64,
    /// Offset from UTC in minutes (east positive).
    pub offset_minutes: i32,
}

const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

impl DateTime {
    /// Now, in UTC.
    pub fn now() -> DateTime {
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
            .unwrap_or(0);
        DateTime { unix, offset_minutes: 0 }
    }

    /// From a Unix timestamp, UTC.
    pub fn from_unix(unix: i64) -> DateTime {
        DateTime { unix, offset_minutes: 0 }
    }

    /// From civil fields in the given offset. `None` if out of range.
    pub fn from_civil(
        y: i64,
        mo: u32,
        d: u32,
        h: u32,
        mi: u32,
        s: u32,
        offset_minutes: i32,
    ) -> Option<DateTime> {
        if !(1..=12).contains(&mo) || d == 0 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 60 {
            return None;
        }
        if !(1..=9999).contains(&y) || offset_minutes.abs() > 24 * 60 {
            return None;
        }
        let days = days_from_civil(y, mo, d);
        let local = days
            .checked_mul(86_400)?
            .checked_add(i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s.min(59)))?;
        Some(DateTime { unix: local.checked_sub(i64::from(offset_minutes) * 60)?, offset_minutes })
    }

    /// Day number (days since 1970-01-01) of the date in its own offset.
    pub fn local_day(&self) -> i64 {
        self.unix.saturating_add(i64::from(self.offset_minutes) * 60).div_euclid(86_400)
    }

    /// IMAP `date-time`: `dd-Mon-yyyy hh:mm:ss +zzzz` (without quotes).
    pub fn to_imap(&self) -> String {
        let local = self.unix.saturating_add(i64::from(self.offset_minutes) * 60);
        let (y, m, d) = civil_from_days(local.div_euclid(86_400));
        let secs = local.rem_euclid(86_400);
        let mon = MONTHS.get((m as usize).saturating_sub(1)).copied().unwrap_or("Jan");
        let sign = if self.offset_minutes < 0 { '-' } else { '+' };
        let off = self.offset_minutes.unsigned_abs();
        format!(
            "{d:02}-{mon}-{y:04} {:02}:{:02}:{:02} {sign}{:02}{:02}",
            secs / 3600,
            (secs / 60) % 60,
            secs % 60,
            off / 60,
            off % 60
        )
    }

    /// RFC 5322 date, e.g. `Sat, 10 Oct 2026 09:30:00 +0000`.
    pub fn to_rfc5322(&self) -> String {
        let local = self.unix.saturating_add(i64::from(self.offset_minutes) * 60);
        let days = local.div_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        let secs = local.rem_euclid(86_400);
        let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]
            .get(days.rem_euclid(7) as usize)
            .copied()
            .unwrap_or("Mon");
        let mon = MONTHS.get((m as usize).saturating_sub(1)).copied().unwrap_or("Jan");
        let sign = if self.offset_minutes < 0 { '-' } else { '+' };
        let off = self.offset_minutes.unsigned_abs();
        format!(
            "{wd}, {d} {mon} {y:04} {:02}:{:02}:{:02} {sign}{:02}{:02}",
            secs / 3600,
            (secs / 60) % 60,
            secs % 60,
            off / 60,
            off % 60
        )
    }

    /// Parses an IMAP `date-time` (`dd-Mon-yyyy hh:mm:ss +zzzz`, day may be space-padded).
    pub fn parse_imap(s: &str) -> Option<DateTime> {
        let s = s.trim();
        let (date, rest) = s.split_once(' ')?;
        let (day, y, m) = parse_date_parts(date)?;
        let (time, zone) = rest.trim_start().split_once(' ')?;
        let (h, mi, sec) = parse_time(time)?;
        let off = parse_numeric_zone(zone.trim())?;
        DateTime::from_civil(y, m, day, h, mi, sec, off)
    }

    /// Parses an RFC 5322 `Date:` value (lenient: optional weekday, optional seconds, named zones).
    pub fn parse_rfc5322(s: &str) -> Option<DateTime> {
        let mut s = strip_comments(s);
        if let Some((_, rest)) = s.split_once(',') {
            s = rest.to_owned();
        }
        let mut it = s.split_whitespace();
        let day: u32 = it.next()?.parse().ok()?;
        let m = month_from_name(it.next()?)?;
        let mut y: i64 = it.next()?.parse().ok()?;
        if y < 50 {
            y += 2000;
        } else if y < 1000 {
            y += 1900;
        }
        let (h, mi, sec) = parse_time(it.next().unwrap_or("00:00:00")).unwrap_or((0, 0, 0));
        let off = match it.next() {
            Some(z) => parse_numeric_zone(z).or_else(|| named_zone(z)).unwrap_or(0),
            None => 0,
        };
        DateTime::from_civil(y, m, day, h, mi, sec, off)
    }
}

/// Parses an IMAP `date` (`d-Mon-yyyy`) into a day number.
pub fn parse_imap_date(s: &str) -> Option<i64> {
    let (d, y, m) = parse_date_parts(s.trim())?;
    if !(1..=9999).contains(&y) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

fn parse_date_parts(s: &str) -> Option<(u32, i64, u32)> {
    let mut it = s.split('-');
    let d: u32 = it.next()?.trim().parse().ok()?;
    let m = month_from_name(it.next()?)?;
    let y: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((d, y, m))
}

fn parse_time(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.split(':');
    let h = it.next()?.parse().ok()?;
    let m = it.next()?.parse().ok()?;
    let sec = match it.next() {
        Some(x) => x.parse().ok()?,
        None => 0,
    };
    Some((h, m, sec))
}

fn parse_numeric_zone(z: &str) -> Option<i32> {
    let (sign, digits) = match z.as_bytes().first()? {
        b'+' => (1, z.get(1..)?),
        b'-' => (-1, z.get(1..)?),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i32 = digits.parse().ok()?;
    let mins = (n / 100) * 60 + n % 100;
    Some(sign * mins)
}

fn named_zone(z: &str) -> Option<i32> {
    Some(match z.to_ascii_uppercase().as_str() {
        "UT" | "GMT" | "Z" => 0,
        "EST" => -300,
        "EDT" => -240,
        "CST" => -360,
        "CDT" => -300,
        "MST" => -420,
        "MDT" => -360,
        "PST" => -480,
        "PDT" => -420,
        _ => return None,
    })
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0u32;
    for c in s.chars() {
        match c {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn month_from_name(s: &str) -> Option<u32> {
    let s = s.get(..3)?;
    MONTHS.iter().position(|m| m.eq_ignore_ascii_case(s)).and_then(|i| u32::try_from(i + 1).ok())
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for z in [-800_000i64, -1, 0, 1, 19_000, 20_736, 2_000_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(20_736), (2026, 10, 10));
    }

    #[test]
    fn imap_date_time() {
        let dt = DateTime::parse_imap("10-Oct-2026 09:30:05 -0400").unwrap();
        assert_eq!(dt.to_imap(), "10-Oct-2026 09:30:05 -0400");
        assert_eq!(dt.local_day(), days_from_civil(2026, 10, 10));
        let dt = DateTime::parse_imap(" 7-Jul-2026 23:59:59 +0000").unwrap();
        assert_eq!(dt.to_imap(), "07-Jul-2026 23:59:59 +0000");
        assert!(DateTime::parse_imap("32-Jan-2026 00:00:00 +0000").is_none());
        assert!(DateTime::parse_imap("garbage").is_none());
        assert_eq!(parse_imap_date("1-Feb-2026"), Some(days_from_civil(2026, 2, 1)));
        assert_eq!(parse_imap_date("29-Feb-2026"), None);
    }

    #[test]
    fn rfc5322_dates() {
        let dt = DateTime::parse_rfc5322("Sat, 10 Oct 2026 09:30:00 +0200 (CEST)").unwrap();
        assert_eq!(dt.offset_minutes, 120);
        assert_eq!(dt.to_rfc5322(), "Sat, 10 Oct 2026 09:30:00 +0200");
        let dt = DateTime::parse_rfc5322("1 Jan 26 00:00 GMT").unwrap();
        assert_eq!(dt.local_day(), days_from_civil(2026, 1, 1));
        assert!(DateTime::parse_rfc5322("").is_none());
        assert!(DateTime::parse_rfc5322("Sat, 99 Foo 2026").is_none());
    }
}
