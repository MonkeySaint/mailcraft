//! RFC 5322 §3.3 date-time, plus civil-calendar helpers used for list grouping and display.
//!
//! Civil conversions use the proleptic Gregorian calendar via the classic days-from-civil / civil-from-days
//! algorithms (era-based, valid for the whole `i32` year range).

use serde::{Deserialize, Serialize};

/// An instant (seconds since the Unix epoch, UTC) plus the UTC offset it was written with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DateTime {
    pub unix: i64,
    /// Offset from UTC in minutes as written in the source (e.g. -240 for -0400).
    pub offset_min: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    /// All days, Monday first.
    pub const ALL: [Weekday; 7] =
        [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri, Weekday::Sat, Weekday::Sun];

    /// 0 = Monday … 6 = Sunday.
    pub fn index_from_monday(self) -> u8 {
        match self {
            Weekday::Mon => 0,
            Weekday::Tue => 1,
            Weekday::Wed => 2,
            Weekday::Thu => 3,
            Weekday::Fri => 4,
            Weekday::Sat => 5,
            Weekday::Sun => 6,
        }
    }
    /// 0 = Sunday … 6 = Saturday.
    pub fn index_from_sunday(self) -> u8 {
        (self.index_from_monday() + 1) % 7
    }
    /// Day from a Monday-based index (taken modulo 7).
    pub fn from_monday_index(i: i64) -> Weekday {
        match i.rem_euclid(7) {
            0 => Weekday::Mon,
            1 => Weekday::Tue,
            2 => Weekday::Wed,
            3 => Weekday::Thu,
            4 => Weekday::Fri,
            5 => Weekday::Sat,
            _ => Weekday::Sun,
        }
    }
    /// Three-letter English abbreviation as used in RFC 5322 (`Mon`).
    pub fn short_name(self) -> &'static str {
        match self {
            Weekday::Mon => "Mon",
            Weekday::Tue => "Tue",
            Weekday::Wed => "Wed",
            Weekday::Thu => "Thu",
            Weekday::Fri => "Fri",
            Weekday::Sat => "Sat",
            Weekday::Sun => "Sun",
        }
    }
    /// Full English name (`Monday`).
    pub fn long_name(self) -> &'static str {
        match self {
            Weekday::Mon => "Monday",
            Weekday::Tue => "Tuesday",
            Weekday::Wed => "Wednesday",
            Weekday::Thu => "Thursday",
            Weekday::Fri => "Friday",
            Weekday::Sat => "Saturday",
            Weekday::Sun => "Sunday",
        }
    }
}

/// Broken-down civil time in some offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Civil {
    pub year: i32,
    /// 1..=12
    pub month: u8,
    /// 1..=31
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub weekday: Weekday,
}

/// Three-letter English month abbreviations, January first.
pub const MONTH_SHORT: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
/// Full English month names, January first.
pub const MONTH_LONG: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Largest absolute UTC offset accepted (RFC 5322 allows ±9959; we accept up to ±99:59).
pub const MAX_OFFSET_MIN: i32 = 99 * 60 + 59;

/// Largest absolute year [`days_from_civil`] computes exactly; larger magnitudes are clamped to it.
pub const MAX_CIVIL_YEAR: i64 = 1_000_000_000_000_000;

pub fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days in `month` (1..=12) of `year`; 0 for an invalid month.
pub fn days_in_month(year: i64, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 of the proleptic Gregorian date (month 1..=12, day 1..=31; not validated).
pub fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    // Clamp so the era arithmetic below cannot overflow; years this far out have no meaningful calendar anyway.
    let year = year.clamp(-MAX_CIVIL_YEAR, MAX_CIVIL_YEAR);
    let m = i64::from(month);
    let d = i64::from(day);
    let y = if m <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // March = 0
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`]: (year, month, day).
pub fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, u8::try_from(m).unwrap_or(1), u8::try_from(d).unwrap_or(1))
}

/// Weekday of a day number (days since 1970-01-01, which was a Thursday).
pub fn weekday_from_days(days: i64) -> Weekday {
    Weekday::from_monday_index(days.rem_euclid(7) + 3)
}

impl DateTime {
    pub fn from_unix(unix: i64, offset_min: i32) -> Self {
        Self { unix, offset_min }
    }
    /// Builds from a civil time in the given offset; `None` for invalid fields.
    pub fn from_civil(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
        offset_min: i32,
    ) -> Option<Self> {
        if !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(i64::from(year), month)
            || hour > 23
            || minute > 59
            || second > 60
            || offset_min.unsigned_abs() > MAX_OFFSET_MIN.unsigned_abs()
        {
            return None;
        }
        let days = days_from_civil(i64::from(year), month, day);
        let secs = i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
        let unix = days.checked_mul(86_400)?.checked_add(secs)?.checked_sub(i64::from(offset_min) * 60)?;
        Some(Self { unix, offset_min })
    }
    /// Civil time in `offset_min` (use the local offset for display).
    pub fn civil(&self, offset_min: i32) -> Civil {
        let local = self.unix.saturating_add(i64::from(offset_min) * 60);
        let days = local.div_euclid(86_400);
        let secs = local.rem_euclid(86_400);
        let (y, month, day) = civil_from_days(days);
        let year = i32::try_from(y).unwrap_or(if y < 0 { i32::MIN } else { i32::MAX });
        Civil {
            year,
            month,
            day,
            hour: u8::try_from(secs / 3600).unwrap_or(0),
            minute: u8::try_from(secs / 60 % 60).unwrap_or(0),
            second: u8::try_from(secs % 60).unwrap_or(0),
            weekday: weekday_from_days(days),
        }
    }
    /// Civil time in the offset the date was written with.
    pub fn civil_original(&self) -> Civil {
        self.civil(self.offset_min)
    }
    /// Days since 1970-01-01 of the civil date in `offset_min` (for "same day" / "yesterday" comparisons).
    pub fn day_number(&self, offset_min: i32) -> i64 {
        self.unix.saturating_add(i64::from(offset_min) * 60).div_euclid(86_400)
    }
    /// The current time (UTC offset 0). Returns the epoch if the system clock is before 1970.
    pub fn now_utc() -> Self {
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        Self { unix, offset_min: 0 }
    }
    /// Parses an RFC 5322 date-time (lenient: obsolete zones like EST/GMT/UT, missing weekday or seconds,
    /// two-digit years, extra whitespace and comments).
    pub fn parse_rfc5322(s: &str) -> crate::Result<Self> {
        parse_lenient(s).ok_or_else(|| crate::MimeError::InvalidDate(s.chars().take(64).collect()))
    }
    /// `Thu, 08 Oct 2026 04:30:00 -0400`.
    pub fn to_rfc5322(&self) -> String {
        let off = self.offset_min.clamp(-MAX_OFFSET_MIN, MAX_OFFSET_MIN);
        let c = self.civil(off);
        let sign = if off < 0 { '-' } else { '+' };
        let a = off.unsigned_abs();
        let month = MONTH_SHORT.get(usize::from(c.month.saturating_sub(1))).copied().unwrap_or("Jan");
        format!(
            "{}, {:02} {} {:04} {:02}:{:02}:{:02} {}{:02}{:02}",
            c.weekday.short_name(),
            c.day,
            month,
            c.year,
            c.hour,
            c.minute,
            c.second,
            sign,
            a / 60,
            a % 60
        )
    }
    /// ISO 8601 / RFC 3339 form in the original offset: `2026-10-08T04:30:00-04:00`.
    pub fn to_rfc3339(&self) -> String {
        let off = self.offset_min.clamp(-MAX_OFFSET_MIN, MAX_OFFSET_MIN);
        let c = self.civil(off);
        let a = off.unsigned_abs();
        let zone = if off == 0 {
            "Z".to_owned()
        } else {
            format!("{}{:02}:{:02}", if off < 0 { '-' } else { '+' }, a / 60, a % 60)
        };
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{zone}",
            c.year, c.month, c.day, c.hour, c.minute, c.second
        )
    }
}

/// Removes (possibly nested) comments and replaces them by a space.
fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    let mut escaped = false;
    for c in s.chars() {
        if depth > 0 {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
                if depth == 0 {
                    out.push(' ');
                }
            }
            continue;
        }
        if c == '(' {
            depth = 1;
        } else {
            out.push(c);
        }
    }
    out
}

fn month_from_name(t: &str) -> Option<u8> {
    let lower = t.to_ascii_lowercase();
    let p = lower.get(..3)?;
    let idx = MONTH_SHORT.iter().position(|m| m.eq_ignore_ascii_case(p))?;
    // Accept "Oct", "October", "Sept"; reject other words that merely share a prefix.
    let full = MONTH_LONG.get(idx)?.to_ascii_lowercase();
    if lower.len() == 3 || full.starts_with(&lower) || lower == "sept" {
        u8::try_from(idx + 1).ok()
    } else {
        None
    }
}

fn is_weekday_name(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    Weekday::ALL.iter().any(|d| {
        let l = d.long_name().to_ascii_lowercase();
        (lower.len() >= 2 && l.starts_with(&lower)) || lower == d.short_name().to_ascii_lowercase()
    })
}

/// Obsolete and common alphabetic zones (RFC 5322 §4.3), in minutes. Military single letters other than `Z`
/// are treated as `-0000` (unknown) as the RFC recommends.
fn zone_from_name(t: &str) -> Option<i32> {
    let u = t.to_ascii_uppercase();
    Some(match u.as_str() {
        "UT" | "UTC" | "GMT" | "Z" | "WET" | "UCT" => 0,
        "EST" => -5 * 60,
        "EDT" => -4 * 60,
        "CST" => -6 * 60,
        "CDT" => -5 * 60,
        "MST" => -7 * 60,
        "MDT" => -6 * 60,
        "PST" => -8 * 60,
        "PDT" => -7 * 60,
        "AKST" => -9 * 60,
        "AKDT" => -8 * 60,
        "HST" => -10 * 60,
        "AST" => -4 * 60,
        "ADT" => -3 * 60,
        "NST" => -(3 * 60 + 30),
        "NDT" => -(2 * 60 + 30),
        "BST" | "CET" | "WEST" => 60,
        "CEST" | "EET" => 2 * 60,
        "EEST" | "MSK" => 3 * 60,
        "IST" => 5 * 60 + 30,
        "JST" | "KST" => 9 * 60,
        "AEST" => 10 * 60,
        "AEDT" => 11 * 60,
        "NZST" => 12 * 60,
        "NZDT" => 13 * 60,
        _ if u.len() == 1 && u.chars().all(|c| c.is_ascii_alphabetic() && c != 'J') => 0,
        _ => return None,
    })
}

/// `+0200`, `-0430`, `+02:00`, `+2`, `GMT+0200`, `UTC-5`.
fn numeric_zone(t: &str) -> Option<i32> {
    let t =
        t.strip_prefix("GMT").or_else(|| t.strip_prefix("UTC")).or_else(|| t.strip_prefix("UT")).unwrap_or(t);
    let (sign, rest) = match t.as_bytes().first()? {
        b'+' => (1, t.get(1..)?),
        b'-' => (-1, t.get(1..)?),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i32 = digits.parse().ok()?;
    let (h, m) = if digits.len() <= 2 { (n, 0) } else { (n / 100, n % 100) };
    if m > 59 {
        return None;
    }
    Some(sign * (h * 60 + m))
}

/// `hh:mm[:ss[.fff]]`, optionally with a trailing zone glued on (`10:00:00+0200`, `10:00Z`).
fn parse_time(t: &str) -> Option<((u8, u8, u8), Option<i32>)> {
    let (time_part, zone) = match t.find(['+', '-', 'Z', 'z']) {
        Some(i) => {
            let z = t.get(i..)?;
            let zone = if z.eq_ignore_ascii_case("z") { Some(0) } else { Some(numeric_zone(z)?) };
            (t.get(..i)?, zone)
        }
        None => (t, None),
    };
    let mut it = time_part.split(':');
    let h: u8 = it.next()?.trim().parse().ok()?;
    let m: u8 = it.next()?.trim().parse().ok()?;
    let s: u8 = match it.next() {
        Some(sec) => {
            let whole = sec.split(['.', ',']).next()?;
            if whole.is_empty() { 0 } else { whole.parse().ok()? }
        }
        None => 0,
    };
    if it.next().is_some() || h > 23 || m > 59 || s > 60 {
        return None;
    }
    Some(((h, m, s), zone))
}

/// RFC 5322 §4.3: two-digit years 00–49 are 2000–2049, 50–99 are 1950–1999; three-digit years add 1900.
fn expand_year(digits: &str, value: i32) -> i32 {
    match digits.len() {
        1 | 2 if value < 50 => value + 2000,
        1 | 2 => value + 1900,
        3 => value + 1900,
        _ => value,
    }
}

fn parse_iso(s: &str) -> Option<DateTime> {
    // 2026-10-08T04:30:00-04:00 / 2026-10-08 04:30:00Z / 2026-10-08
    let s = s.trim();
    let (date, time) = match s.find(['T', 't', ' ']) {
        Some(i) => (s.get(..i)?, Some(s.get(i + 1..)?.trim())),
        None => (s, None),
    };
    let mut it = date.split('-');
    let y_str = it.next()?;
    if y_str.len() != 4 {
        return None;
    }
    let y: i32 = y_str.parse().ok()?;
    let mo: u8 = it.next()?.parse().ok()?;
    let d: u8 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    let ((h, mi, se), zone) = match time {
        Some(t) => {
            let t = t.replace(' ', "");
            parse_time(&t)?
        }
        None => ((0, 0, 0), None),
    };
    DateTime::from_civil(y, mo, d, h, mi, se, zone.unwrap_or(0))
}

fn parse_lenient(s: &str) -> Option<DateTime> {
    if s.len() > 512 {
        return None;
    }
    let cleaned = strip_comments(s);
    if let Some(dt) = parse_iso(&cleaned) {
        return Some(dt);
    }
    let normalized: String = cleaned.chars().map(|c| if c == ',' { ' ' } else { c }).collect();
    let mut tokens: Vec<String> = Vec::new();
    for raw in normalized.split_whitespace() {
        // "08-Oct-2026" and "8/Oct/2026" style dates.
        if raw.contains(['-', '/'])
            && !raw.starts_with(['+', '-'])
            && !raw.contains(':')
            && raw.chars().any(|c| c.is_ascii_alphabetic())
        {
            tokens.extend(raw.split(['-', '/']).filter(|p| !p.is_empty()).map(str::to_owned));
        } else {
            tokens.push(raw.to_owned());
        }
    }
    let mut month: Option<u8> = None;
    let mut numbers: Vec<(String, i32)> = Vec::new();
    let mut time: Option<(u8, u8, u8)> = None;
    let mut zone: Option<i32> = None;
    let mut numeric_zone_seen = false;
    for t in &tokens {
        let t = t.trim_end_matches('.');
        if t.is_empty() {
            continue;
        }
        if time.is_none()
            && t.contains(':')
            && !t.starts_with(['+', '-'])
            && t.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            let (hms, z) = parse_time(t)?;
            time = Some(hms);
            if z.is_some() {
                zone = z;
                numeric_zone_seen = true;
            }
            continue;
        }
        if t.bytes().all(|b| b.is_ascii_digit()) {
            if t.len() > 9 {
                return None;
            }
            numbers.push((t.to_owned(), t.parse().ok()?));
            continue;
        }
        if let Some(z) = numeric_zone(t)
            && (time.is_some() || zone.is_none())
        {
            zone = Some(z);
            numeric_zone_seen = true;
            continue;
        }
        if month.is_none()
            && let Some(m) = month_from_name(t)
        {
            month = Some(m);
            continue;
        }
        if is_weekday_name(t) {
            continue;
        }
        if let Some(z) = zone_from_name(t) {
            // A numeric offset is authoritative; an alphabetic zone only fills in when none was given.
            if !numeric_zone_seen {
                zone = Some(z);
            }
            continue;
        }
        // Unknown words ("at", trailing junk) are ignored.
    }
    let month = month?;
    let (day, year) = match numbers.as_slice() {
        [(ds, d), (ys, y), ..] if ds.len() <= 2 => (*d, expand_year(ys, *y)),
        [(ys, y), (ds, d), ..] if ys.len() == 4 && ds.len() <= 2 => (*d, *y),
        _ => return None,
    };
    let (h, m, sec) = time.unwrap_or((0, 0, 0));
    DateTime::from_civil(year, month, u8::try_from(day).ok()?, h, m, sec, zone.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn p(s: &str) -> DateTime {
        match DateTime::parse_rfc5322(s) {
            Ok(d) => d,
            Err(e) => panic!("{s}: {e}"),
        }
    }

    #[test]
    fn civil_algorithms() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(weekday_from_days(0), Weekday::Thu);
        assert_eq!(weekday_from_days(-1), Weekday::Wed);
        assert_eq!(weekday_from_days(days_from_civil(2026, 10, 8)), Weekday::Thu);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2023, 13), 0);
    }

    #[test]
    fn from_civil_validation() {
        assert!(DateTime::from_civil(2026, 2, 29, 0, 0, 0, 0).is_none());
        assert!(DateTime::from_civil(2024, 2, 29, 0, 0, 0, 0).is_some());
        assert!(DateTime::from_civil(2026, 0, 1, 0, 0, 0, 0).is_none());
        assert!(DateTime::from_civil(2026, 1, 1, 24, 0, 0, 0).is_none());
        assert!(DateTime::from_civil(2026, 1, 1, 0, 60, 0, 0).is_none());
        assert!(DateTime::from_civil(2026, 1, 1, 0, 0, 61, 0).is_none());
        assert!(DateTime::from_civil(2026, 1, 1, 0, 0, 0, 6000).is_none());
        assert_eq!(DateTime::from_civil(1970, 1, 1, 0, 0, 0, 0), Some(DateTime::from_unix(0, 0)));
        assert_eq!(DateTime::from_civil(1970, 1, 1, 0, 0, 0, 60), Some(DateTime::from_unix(-3600, 60)));
    }

    #[test]
    fn civil_round_trip_and_day_number() {
        let d = DateTime::from_civil(2026, 10, 8, 4, 30, 0, -240).unwrap();
        let c = d.civil(-240);
        assert_eq!((c.year, c.month, c.day, c.hour, c.minute, c.second), (2026, 10, 8, 4, 30, 0));
        assert_eq!(c.weekday, Weekday::Thu);
        let u = d.civil(0);
        assert_eq!((u.day, u.hour), (8, 8));
        assert_eq!(d.day_number(0), days_from_civil(2026, 10, 8));
        assert_eq!(d.civil_original(), c);
    }

    #[test]
    fn formats() {
        let d = DateTime::from_civil(2026, 10, 8, 4, 30, 0, -240).unwrap();
        assert_eq!(d.to_rfc5322(), "Thu, 08 Oct 2026 04:30:00 -0400");
        assert_eq!(d.to_rfc3339(), "2026-10-08T04:30:00-04:00");
        assert_eq!(DateTime::from_unix(0, 0).to_rfc5322(), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(DateTime::from_unix(0, 330).to_rfc5322(), "Thu, 01 Jan 1970 05:30:00 +0530");
        assert_eq!(DateTime::from_unix(0, 0).to_rfc3339(), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn parses_standard_forms() {
        let expect = DateTime::from_civil(2026, 10, 8, 4, 30, 0, -240).unwrap();
        assert_eq!(p("Thu, 08 Oct 2026 04:30:00 -0400"), expect);
        assert_eq!(p("Thu, 8 Oct 2026 04:30:00 -0400"), expect);
        assert_eq!(p("8 Oct 2026 04:30:00 -0400"), expect);
        assert_eq!(p("  Thu,  08   Oct 2026   04:30:00   -0400  "), expect);
        assert_eq!(p("Thu, 08 Oct 2026 04:30 -0400"), DateTime { unix: expect.unix, offset_min: -240 });
        assert_eq!(p("Thu, 08 Oct 2026 04:30:00 -0400 (EDT)"), expect);
        assert_eq!(p("Thu, 08 Oct 2026 04:30:00 EDT"), expect);
        assert_eq!(p("Thu (comment (nested)), 08 Oct 2026 04:30:00 -0400"), expect);
        assert_eq!(p("Thursday, 08 October 2026 04:30:00 -0400"), expect);
        assert_eq!(p("Thu, 08 Oct 2026 04:30:00.123 -0400"), expect);
        assert_eq!(p("Thu, 08-Oct-2026 04:30:00 -0400"), expect);
    }

    #[test]
    fn obsolete_zones() {
        let utc = DateTime::from_civil(2026, 1, 2, 3, 4, 5, 0).unwrap();
        for z in ["UT", "GMT", "Z", "UTC", "+0000", "-0000", "A", "m", "(no zone)"] {
            assert_eq!(p(&format!("2 Jan 2026 03:04:05 {z}")).unix, utc.unix, "{z}");
        }
        for (z, off) in [
            ("EST", -300),
            ("EDT", -240),
            ("CST", -360),
            ("CDT", -300),
            ("MST", -420),
            ("MDT", -360),
            ("PST", -480),
            ("PDT", -420),
        ] {
            let d = p(&format!("2 Jan 2026 03:04:05 {z}"));
            assert_eq!(d.offset_min, off, "{z}");
            assert_eq!(d.unix, utc.unix - i64::from(off) * 60, "{z}");
        }
        assert_eq!(p("2 Jan 2026 03:04:05 +02:00").offset_min, 120);
        assert_eq!(p("2 Jan 2026 03:04:05 GMT+0200").offset_min, 120);
        assert_eq!(p("2 Jan 2026 03:04:05 +0530").offset_min, 330);
    }

    #[test]
    fn two_and_three_digit_years() {
        assert_eq!(p("1 Jan 26 00:00:00 +0000").civil(0).year, 2026);
        assert_eq!(p("1 Jan 49 00:00:00 +0000").civil(0).year, 2049);
        assert_eq!(p("1 Jan 50 00:00:00 +0000").civil(0).year, 1950);
        assert_eq!(p("1 Jan 99 00:00:00 +0000").civil(0).year, 1999);
        assert_eq!(p("1 Jan 103 00:00:00 +0000").civil(0).year, 2003);
    }

    #[test]
    fn asctime_and_iso() {
        let d = p("Thu Oct  8 04:30:00 2026");
        assert_eq!(d, DateTime::from_civil(2026, 10, 8, 4, 30, 0, 0).unwrap());
        assert_eq!(
            p("2026-10-08T04:30:00-04:00"),
            DateTime::from_civil(2026, 10, 8, 4, 30, 0, -240).unwrap()
        );
        assert_eq!(p("2026-10-08T04:30:00Z").offset_min, 0);
        assert_eq!(p("2026-10-08").unix, days_from_civil(2026, 10, 8) * 86_400);
        assert_eq!(p("8 Oct 2026").unix, days_from_civil(2026, 10, 8) * 86_400);
    }

    #[test]
    fn rejects_garbage() {
        for s in
            ["", "garbage", "32 Oct 2026 00:00:00", "8 Foo 2026", "8 Oct 2026 25:00:00", "Feb 30 2026", "Oct"]
        {
            assert!(DateTime::parse_rfc5322(s).is_err(), "{s}");
        }
        let long = "1 ".repeat(1000);
        assert!(DateTime::parse_rfc5322(&long).is_err());
    }

    proptest! {
        #[test]
        fn format_parse_round_trip(unix in -62_135_596_800i64..253_402_300_799i64, off in -1439i32..1440) {
            let d = DateTime::from_unix(unix, off);
            let s = d.to_rfc5322();
            prop_assert_eq!(DateTime::parse_rfc5322(&s).ok(), Some(d));
        }

        #[test]
        fn days_round_trip(days in -10_000_000i64..10_000_000) {
            let (y, m, d) = civil_from_days(days);
            prop_assert_eq!(days_from_civil(y, m, d), days);
            prop_assert!(d >= 1 && d <= days_in_month(y, m));
        }

        #[test]
        fn civil_never_panics(unix in any::<i64>(), off in any::<i32>()) {
            let _ = DateTime::from_unix(unix, off).civil(off);
            let _ = DateTime::from_unix(unix, off).to_rfc5322();
            let _ = DateTime::from_unix(unix, off).day_number(0);
        }

        #[test]
        fn parse_never_panics(s in "\\PC{0,80}") {
            let _ = DateTime::parse_rfc5322(&s);
        }

        #[test]
        fn parse_never_panics_datelike(s in "[A-Za-z]{0,4},? [0-9]{0,3} [A-Za-z]{0,9} [0-9]{0,5} [0-9:]{0,9} [+-]?[0-9A-Z]{0,5}") {
            let _ = DateTime::parse_rfc5322(&s);
        }
    }
}
