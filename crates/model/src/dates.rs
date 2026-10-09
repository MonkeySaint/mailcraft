//! Date groups and list time labels (`plan/outlook/reading-pane.md` › List rows).
//!
//! Observed (compact view, arranged by Date, newest first): groups "Today", "Yesterday", weekday names for
//! earlier days of this week ("Tuesday", "Monday"), "Last Week", "Two Weeks Ago", "Three Weeks Ago",
//! "Last Month", "Older". Time column: today "9:10 AM"; this week "Thu 7:30 AM"; last week "Wed 9/30"; older
//! "9/19/2026". Weeks start on Sunday (en-US default).

use serde::{Deserialize, Serialize};

use crate::DateTime;
use mailcraft_mime::Weekday;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DateGroup {
    /// Items dated after now (clock skew / scheduled).
    Future,
    Today,
    Yesterday,
    /// Earlier this week (not today/yesterday).
    ThisWeek(WeekdayName),
    LastWeek,
    TwoWeeksAgo,
    ThreeWeeksAgo,
    LastMonth,
    Older,
}

/// Serializable weekday wrapper with its English name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WeekdayName(pub u8);

impl WeekdayName {
    pub fn from_weekday(w: Weekday) -> Self {
        WeekdayName(match w {
            Weekday::Sun => 0,
            Weekday::Mon => 1,
            Weekday::Tue => 2,
            Weekday::Wed => 3,
            Weekday::Thu => 4,
            Weekday::Fri => 5,
            Weekday::Sat => 6,
        })
    }
    pub fn name(self) -> &'static str {
        ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]
            .get(self.0 as usize)
            .copied()
            .unwrap_or("Sunday")
    }
    pub fn short(self) -> &'static str {
        ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"].get(self.0 as usize).copied().unwrap_or("Sun")
    }
}

/// Seconds per civil day.
const DAY_SECS: i64 = 86_400;

const MONTH_NAMES: [&str; 12] = [
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

/// A broken-down local date and time computed without overflow for any `i64` instant.
///
/// `mailcraft_mime::Civil` uses an `i32` year; this keeps an `i64` year so hostile timestamps
/// (far past/future) never overflow while formatting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalTime {
    pub year: i64,
    /// 1..=12
    pub month: u8,
    /// 1..=31
    pub day: u8,
    /// 0..=23
    pub hour: u8,
    /// 0..=59
    pub minute: u8,
    /// 0..=59
    pub second: u8,
    /// Day of the week (Sunday = 0).
    pub weekday: WeekdayName,
    /// Days since 1970-01-01 of this local date.
    pub day_number: i64,
}

impl LocalTime {
    /// Breaks `t` down in the local offset `offset_min` (minutes east of UTC).
    pub fn of(t: DateTime, offset_min: i32) -> LocalTime {
        let local = local_seconds(t, offset_min);
        let days = local.div_euclid(DAY_SECS);
        let secs = local.rem_euclid(DAY_SECS);
        let (year, month, day) = civil_from_days(days);
        LocalTime {
            year,
            month,
            day,
            hour: u8::try_from(secs / 3600).unwrap_or(0),
            minute: u8::try_from((secs % 3600) / 60).unwrap_or(0),
            second: u8::try_from(secs % 60).unwrap_or(0),
            weekday: weekday_of_day(days),
            day_number: days,
        }
    }

    /// English month name ("October").
    pub fn month_name(&self) -> &'static str {
        MONTH_NAMES.get(usize::from(self.month).saturating_sub(1)).copied().unwrap_or("January")
    }

    /// Month index counted from year 0 (for "same / previous calendar month" comparisons).
    fn month_index(&self) -> i64 {
        self.year.saturating_mul(12).saturating_add(i64::from(self.month))
    }
}

/// Local seconds since the epoch (saturating; never overflows).
fn local_seconds(t: DateTime, offset_min: i32) -> i64 {
    t.unix.saturating_add(i64::from(offset_min).saturating_mul(60))
}

/// Local civil day number (days since 1970-01-01) of `t` in `offset_min`.
pub fn local_day(t: DateTime, offset_min: i32) -> i64 {
    local_seconds(t, offset_min).div_euclid(DAY_SECS)
}

/// Weekday of a day number (1970-01-01 was a Thursday); Sunday = 0.
pub fn weekday_of_day(days: i64) -> WeekdayName {
    WeekdayName(u8::try_from(days.saturating_add(4).rem_euclid(7)).unwrap_or(0))
}

/// Proleptic Gregorian date of a day number (days since 1970-01-01).
///
/// The classic era-based conversion: shift the epoch to 0000-03-01 so leap days fall at the end of the
/// computational year, then split into 400-year eras.
pub fn civil_from_days(days: i64) -> (i64, u8, u8) {
    // Clamp to a range where every intermediate value fits comfortably in i64.
    let z = days.clamp(-(1_i64 << 50), 1_i64 << 50).saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, u8::try_from(m).unwrap_or(1), u8::try_from(d).unwrap_or(1))
}

/// Day number (days since 1970-01-01) of a proleptic Gregorian date; `None` for invalid dates.
pub fn days_from_civil(year: i64, month: u8, day: u8) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    if !(-(1_i64 << 40)..=(1_i64 << 40)).contains(&year) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400); // [0, 399]
    let m = i64::from(month);
    let mp = if m > 2 { m - 3 } else { m + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    Some(era * 146_097 + doe - 719_468)
}

/// True for Gregorian leap years.
pub fn is_leap_year(year: i64) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

/// Number of days in `month` (1..=12) of `year`; 0 for an invalid month.
pub fn days_in_month(year: i64, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Builds an instant from a local civil date/time in `offset_min`; `None` for invalid fields.
/// (Independent of `mailcraft_mime::DateTime::from_civil`; handy for tests and the date pickers.)
pub fn local_datetime(
    year: i64,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    offset_min: i32,
) -> Option<DateTime> {
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let local = days
        .checked_mul(DAY_SECS)?
        .checked_add(i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))?;
    let unix = local.checked_sub(i64::from(offset_min).checked_mul(60)?)?;
    Some(DateTime { unix, offset_min })
}

/// First day (Sunday) of the week containing day number `day`.
fn week_start(day: i64) -> i64 {
    day.saturating_sub(i64::from(weekday_of_day(day).0))
}

impl DateGroup {
    /// Group for `t` relative to `now`, both shown in local offset `offset_min`.
    ///
    /// Weeks start on Sunday. Precedence: a later calendar day is `Future`; the same day `Today`; the
    /// previous day `Yesterday` (even across a week boundary); other days of the current week
    /// `ThisWeek(weekday)`; then the three previous whole weeks; then `LastMonth` for anything still in the
    /// current or the previous calendar month; everything else is `Older`.
    pub fn of(t: DateTime, now: DateTime, offset_min: i32) -> DateGroup {
        let lt = LocalTime::of(t, offset_min);
        let ln = LocalTime::of(now, offset_min);
        let td = lt.day_number;
        let nd = ln.day_number;
        if td > nd {
            return DateGroup::Future;
        }
        if td == nd {
            return DateGroup::Today;
        }
        if td == nd.saturating_sub(1) {
            return DateGroup::Yesterday;
        }
        let ws = week_start(nd);
        if td >= ws {
            return DateGroup::ThisWeek(lt.weekday);
        }
        if td >= ws.saturating_sub(7) {
            return DateGroup::LastWeek;
        }
        if td >= ws.saturating_sub(14) {
            return DateGroup::TwoWeeksAgo;
        }
        if td >= ws.saturating_sub(21) {
            return DateGroup::ThreeWeeksAgo;
        }
        let months_back = ln.month_index().saturating_sub(lt.month_index());
        if (0..=1).contains(&months_back) {
            return DateGroup::LastMonth;
        }
        DateGroup::Older
    }

    /// Header label ("Today", "Yesterday", "Tuesday", "Last Week", …).
    pub fn label(&self) -> String {
        match self {
            DateGroup::Future => "Future".to_string(),
            DateGroup::Today => "Today".to_string(),
            DateGroup::Yesterday => "Yesterday".to_string(),
            DateGroup::ThisWeek(w) => w.name().to_string(),
            DateGroup::LastWeek => "Last Week".to_string(),
            DateGroup::TwoWeeksAgo => "Two Weeks Ago".to_string(),
            DateGroup::ThreeWeeksAgo => "Three Weeks Ago".to_string(),
            DateGroup::LastMonth => "Last Month".to_string(),
            DateGroup::Older => "Older".to_string(),
        }
    }

    /// Display order when the list is sorted newest first (smaller = higher in the list).
    pub fn rank(&self) -> u8 {
        match self {
            DateGroup::Future => 0,
            DateGroup::Today => 1,
            DateGroup::Yesterday => 2,
            // Later weekdays come first when newest is on top.
            DateGroup::ThisWeek(w) => 3 + (6_u8.saturating_sub(w.0.min(6))),
            DateGroup::LastWeek => 10,
            DateGroup::TwoWeeksAgo => 11,
            DateGroup::ThreeWeeksAgo => 12,
            DateGroup::LastMonth => 13,
            DateGroup::Older => 14,
        }
    }
}

/// List time column formatter.
pub struct ListTime;

impl ListTime {
    /// "9:10 AM" (today), "Thu 7:30 AM" (this week), "Wed 9/30" (last week … this year), "9/19/2026" (older).
    ///
    /// Concretely: same local day → clock; yesterday or an earlier day of the current week → short weekday
    /// and clock; the previous week → short weekday and month/day; anything else (including later days)
    /// → full numeric date.
    pub fn format(t: DateTime, now: DateTime, offset_min: i32) -> String {
        let lt = LocalTime::of(t, offset_min);
        let nd = local_day(now, offset_min);
        let td = lt.day_number;
        let ws = week_start(nd);
        if td == nd {
            Self::clock(t, offset_min)
        } else if td < nd && (td >= ws || td == nd.saturating_sub(1)) {
            format!("{} {}", lt.weekday.short(), clock_of(&lt))
        } else if td < ws && td >= ws.saturating_sub(7) {
            format!("{} {}/{}", lt.weekday.short(), lt.month, lt.day)
        } else {
            numeric_date(&lt)
        }
    }

    /// Reading-pane / read-window header date: "Thu 10/8/2026 7:30 AM".
    pub fn header(t: DateTime, offset_min: i32) -> String {
        let lt = LocalTime::of(t, offset_min);
        format!("{} {} {}", lt.weekday.short(), numeric_date(&lt), clock_of(&lt))
    }

    /// Quoted-header "Sent:" line: "Thursday, October 8, 2026 4:30 AM".
    pub fn long(t: DateTime, offset_min: i32) -> String {
        let lt = LocalTime::of(t, offset_min);
        format!("{}, {} {}, {} {}", lt.weekday.name(), lt.month_name(), lt.day, lt.year, clock_of(&lt))
    }

    /// "h:mm AM/PM" 12-hour clock.
    pub fn clock(t: DateTime, offset_min: i32) -> String {
        clock_of(&LocalTime::of(t, offset_min))
    }

    /// Numeric date "10/8/2026" (month/day/year, no padding).
    pub fn date(t: DateTime, offset_min: i32) -> String {
        numeric_date(&LocalTime::of(t, offset_min))
    }

    /// Long date without time: "Friday, October 9, 2026" (flag info bar, appointment headers).
    pub fn long_date(t: DateTime, offset_min: i32) -> String {
        let lt = LocalTime::of(t, offset_min);
        format!("{}, {} {}, {}", lt.weekday.name(), lt.month_name(), lt.day, lt.year)
    }

    /// Short numeric date and clock: "10/9/2026 3:00 AM" (info bars such as "You forwarded this message on …").
    pub fn short_date_time(t: DateTime, offset_min: i32) -> String {
        let lt = LocalTime::of(t, offset_min);
        format!("{} {}", numeric_date(&lt), clock_of(&lt))
    }
}

fn numeric_date(lt: &LocalTime) -> String {
    format!("{}/{}/{}", lt.month, lt.day, lt.year)
}

fn clock_of(lt: &LocalTime) -> String {
    let (h12, suffix) = match lt.hour {
        0 => (12, "AM"),
        h @ 1..=11 => (h, "AM"),
        12 => (12, "PM"),
        h => (h.saturating_sub(12), "PM"),
    };
    format!("{h12}:{:02} {suffix}", lt.minute)
}
