//! iCalendar value types (RFC 5545 §3.3): DATE, DATE-TIME, DURATION, UTC-OFFSET.
//!
//! Local ("wall clock") times are carried as seconds since 1970-01-01T00:00:00 of the local calendar
//! (`local secs`); a time zone maps them to UTC instants.

use mailcraft_model::dates::{civil_from_days, days_from_civil};

pub const DAY: i64 = 86_400;

/// Supported years (keeps every computation far from overflow).
pub const MIN_YEAR: i64 = 1;
pub const MAX_YEAR: i64 = 9999;

/// A DATE or DATE-TIME value as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimeValue {
    /// `VALUE=DATE`: a calendar day (local secs at midnight).
    Date(i64),
    /// `…Z`: a UTC instant (unix seconds).
    Utc(i64),
    /// Local time, in the named zone (`TZID`) or floating when `None`.
    Local { secs: i64, tzid: Option<String> },
}

/// Local secs of a civil date-time; `None` when invalid or out of range.
pub fn local_secs(year: i64, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Option<i64> {
    if !(MIN_YEAR..=MAX_YEAR).contains(&year) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let s = i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second.min(59));
    Some(days * DAY + s)
}

/// Civil fields of local secs: (year, month, day, hour, minute, second).
pub fn civil(secs: i64) -> (i64, u8, u8, u8, u8, u8) {
    let days = secs.div_euclid(DAY);
    let tod = secs.rem_euclid(DAY);
    let (y, m, d) = civil_from_days(days);
    let h = u8::try_from(tod / 3600).unwrap_or(0);
    let mi = u8::try_from(tod / 60 % 60).unwrap_or(0);
    let s = u8::try_from(tod % 60).unwrap_or(0);
    (y, m, d, h, mi, s)
}

fn digits(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `YYYYMMDD` (also tolerates `YYYY-MM-DD`) → local secs at midnight.
pub fn parse_date(s: &str) -> Option<i64> {
    let s: String = s.trim().chars().filter(|&c| c != '-').collect();
    if s.len() != 8 {
        return None;
    }
    let y = digits(s.get(..4)?)?;
    let m = u8::try_from(digits(s.get(4..6)?)?).ok()?;
    let d = u8::try_from(digits(s.get(6..8)?)?).ok()?;
    local_secs(y, m, d, 0, 0, 0)
}

/// `YYYYMMDDTHHMMSS[Z]` (tolerates `-`/`:` separators and a missing seconds field) → (local secs, is UTC).
pub fn parse_date_time(s: &str) -> Option<(i64, bool)> {
    let s = s.trim();
    let (body, utc) = match s.strip_suffix(['Z', 'z']) {
        Some(b) => (b, true),
        None => (s, false),
    };
    let (date, time) = body.split_once(['T', 't'])?;
    let day = parse_date(date)?;
    let time: String = time.chars().filter(|&c| c != ':').collect();
    let time = time.split(['.', ',']).next().unwrap_or("");
    let (h, mi, sec) = match time.len() {
        6 => (digits(time.get(..2)?)?, digits(time.get(2..4)?)?, digits(time.get(4..6)?)?),
        4 => (digits(time.get(..2)?)?, digits(time.get(2..4)?)?, 0),
        _ => return None,
    };
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some((day + h * 3600 + mi * 60 + sec.min(59), utc))
}

/// A DATE or DATE-TIME property value, honouring `VALUE=DATE` and `TZID`.
pub fn parse_time_value(value: &str, value_param: Option<&str>, tzid: Option<&str>) -> Option<TimeValue> {
    let value = value.trim();
    let is_date = value_param.is_some_and(|v| v.eq_ignore_ascii_case("DATE")) || !value.contains(['T', 't']);
    if is_date {
        return parse_date(value).map(TimeValue::Date);
    }
    let (secs, utc) = parse_date_time(value)?;
    if utc {
        return Some(TimeValue::Utc(secs));
    }
    let tzid = tzid.map(|t| t.trim().trim_matches('"').to_owned()).filter(|t| !t.is_empty());
    Some(TimeValue::Local { secs, tzid })
}

/// `20261012`.
pub fn format_date(secs: i64) -> String {
    let (y, m, d, ..) = civil(secs);
    format!("{y:04}{m:02}{d:02}")
}

/// `20261012T143000` (append `Z` for UTC).
pub fn format_date_time(secs: i64) -> String {
    let (y, m, d, h, mi, s) = civil(secs);
    format!("{y:04}{m:02}{d:02}T{h:02}{mi:02}{s:02}")
}

/// DURATION (`[+-]P[nW][nD][T[nH][nM][nS]]`) → seconds.
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let rest = rest.strip_prefix(['P', 'p'])?;
    let mut total: i64 = 0;
    let mut num = String::new();
    let mut in_time = false;
    let mut any = false;
    for c in rest.chars() {
        match c.to_ascii_uppercase() {
            '0'..='9' => {
                if num.len() > 12 {
                    return None;
                }
                num.push(c);
            }
            'T' => in_time = true,
            unit @ ('W' | 'D' | 'H' | 'M' | 'S') => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                let mult = match (unit, in_time) {
                    ('W', _) => 7 * DAY,
                    ('D', _) => DAY,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
                total = total.checked_add(n.checked_mul(mult)?)?;
                any = true;
            }
            _ => return None,
        }
    }
    if !any || !num.is_empty() {
        return None;
    }
    Some(if neg { -total } else { total })
}

/// Seconds → DURATION (`-PT15M`, `P1D`, `PT1H30M`).
pub fn format_duration(secs: i64) -> String {
    let neg = secs < 0;
    let mut s = secs.unsigned_abs();
    let mut out = String::from(if neg { "-P" } else { "P" });
    let days = s / 86_400;
    s %= 86_400;
    if days > 0 {
        if s == 0 && days.is_multiple_of(7) {
            out.push_str(&format!("{}W", days / 7));
            return out;
        }
        out.push_str(&format!("{days}D"));
    }
    if s > 0 || days == 0 {
        out.push('T');
        let (h, m, sec) = (s / 3600, s / 60 % 60, s % 60);
        if h > 0 {
            out.push_str(&format!("{h}H"));
        }
        if m > 0 {
            out.push_str(&format!("{m}M"));
        }
        if sec > 0 || (h == 0 && m == 0) {
            out.push_str(&format!("{sec}S"));
        }
    }
    out
}

/// UTC-OFFSET (`-0500`, `+0530`, `+053000`) → minutes.
pub fn parse_utc_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let (sign, rest) = match s.chars().next()? {
        '+' => (1, s.get(1..)?),
        '-' => (-1, s.get(1..)?),
        _ => (1, s),
    };
    let rest: String = rest.chars().filter(|&c| c != ':').collect();
    if rest.len() != 4 && rest.len() != 6 && rest.len() != 2 {
        return None;
    }
    let h = digits(rest.get(..2)?)?;
    let m = if rest.len() >= 4 { digits(rest.get(2..4)?)? } else { 0 };
    if h > 23 || m > 59 {
        return None;
    }
    i32::try_from(sign * (h * 60 + m)).ok()
}

/// Minutes → `+0530` / `-0400`.
pub fn format_utc_offset(min: i32) -> String {
    let a = min.unsigned_abs();
    format!("{}{:02}{:02}", if min < 0 { '-' } else { '+' }, a / 60 % 100, a % 60)
}
