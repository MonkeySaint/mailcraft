//! Alternate calendars for Options › Calendar › "Enable an alternate calendar": pure arithmetic from a day
//! number (days since 1970-01-01, the calendar views' day index) to a date in another calendar.
//!
//! - **Hijri (tabular)**: the arithmetic Islamic calendar (30-year cycle with leap years 2, 5, 7, 10, 13, 16,
//!   18, 21, 24, 26 and 29; epoch 16 July 622 Julian). Observed-moon calendars can differ by a day or two.
//! - **Hebrew**: the fixed arithmetic Hebrew calendar (Metonic 19-year cycle, molad and postponement rules).
//!
//! Both follow the published calendrical algorithms on "fixed" day numbers (day 1 = 1 January of year 1,
//! proleptic Gregorian). Inputs far outside ±1 million years are clamped, so no arithmetic overflows.

/// Which alternate calendar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AltCalendar {
    Hijri,
    Hebrew,
}

/// A date in an alternate calendar: year, month (1-based in that calendar's own numbering), day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AltDate {
    pub calendar: AltCalendar,
    pub year: i64,
    pub month: u8,
    pub day: u8,
}

/// Fixed day number of 1970-01-01.
const UNIX_FIXED: i64 = 719_163;
/// Largest |day| accepted (about ±2.7 million years).
const MAX_DAYS: i64 = 1_000_000_000;

fn fixed(day: i64) -> i64 {
    day.clamp(-MAX_DAYS, MAX_DAYS).saturating_add(UNIX_FIXED)
}

// ---------------------------------------------------------------------------------------------------------
// Hijri (arithmetic Islamic)

const ISLAMIC_EPOCH: i64 = 227_015;

fn fixed_from_islamic(y: i64, m: i64, d: i64) -> i64 {
    d + 29 * (m - 1)
        + (6 * m - 1).div_euclid(11)
        + (y - 1) * 354
        + (3 + 11 * y).div_euclid(30)
        + ISLAMIC_EPOCH
        - 1
}

fn islamic_from_fixed(date: i64) -> (i64, i64, i64) {
    let year = (30 * (date - ISLAMIC_EPOCH) + 10_646).div_euclid(10_631);
    let prior = date - fixed_from_islamic(year, 1, 1);
    let month = ((11 * prior + 330).div_euclid(325)).clamp(1, 12);
    let day = date - fixed_from_islamic(year, month, 1) + 1;
    (year, month, day)
}

const HIJRI_MONTHS: [&str; 12] = [
    "Muharram",
    "Safar",
    "Rabi\u{2bf} I",
    "Rabi\u{2bf} II",
    "Jumada I",
    "Jumada II",
    "Rajab",
    "Sha\u{2bf}ban",
    "Ramadan",
    "Shawwal",
    "Dhu\u{2bf}l-Qa\u{2bf}dah",
    "Dhu\u{2bf}l-Hijjah",
];

// ---------------------------------------------------------------------------------------------------------
// Hebrew

const HEBREW_EPOCH: i64 = -1_373_427;
const NISAN: i64 = 1;
const TISHRI: i64 = 7;

fn hebrew_leap(y: i64) -> bool {
    (7 * y + 1).rem_euclid(19) < 7
}

fn last_month(y: i64) -> i64 {
    if hebrew_leap(y) { 13 } else { 12 }
}

/// Days from the epoch to the (unpostponed) new year of `y`, with the molad-of-Tishri postponements.
fn elapsed_days(y: i64) -> i64 {
    let months = (235 * y - 234).div_euclid(19);
    let parts = 12_084 + 13_753 * months;
    let days = 29 * months + parts.div_euclid(25_920);
    if (3 * (days + 1)).rem_euclid(7) < 3 { days + 1 } else { days }
}

fn year_length_correction(y: i64) -> i64 {
    let (ny0, ny1, ny2) = (elapsed_days(y - 1), elapsed_days(y), elapsed_days(y + 1));
    if ny2 - ny1 == 356 {
        2
    } else if ny1 - ny0 == 382 {
        1
    } else {
        0
    }
}

fn new_year(y: i64) -> i64 {
    HEBREW_EPOCH + elapsed_days(y) + year_length_correction(y)
}

fn days_in_year(y: i64) -> i64 {
    new_year(y + 1) - new_year(y)
}

fn last_day(m: i64, y: i64) -> i64 {
    let short = matches!(m, 2 | 4 | 6 | 10 | 13)
        || (m == 12 && !hebrew_leap(y))
        || (m == 8 && !matches!(days_in_year(y), 355 | 385))
        || (m == 9 && matches!(days_in_year(y), 353 | 383));
    if short { 29 } else { 30 }
}

fn fixed_from_hebrew(y: i64, m: i64, d: i64) -> i64 {
    let mut t = new_year(y) + d - 1;
    if m < TISHRI {
        t += (TISHRI..=last_month(y)).map(|mm| last_day(mm, y)).sum::<i64>();
        t += (NISAN..m).map(|mm| last_day(mm, y)).sum::<i64>();
    } else {
        t += (TISHRI..m).map(|mm| last_day(mm, y)).sum::<i64>();
    }
    t
}

fn hebrew_from_fixed(date: i64) -> (i64, i64, i64) {
    // Mean year length 35975351 / 98496 days.
    let approx = ((date - HEBREW_EPOCH) * 98_496).div_euclid(35_975_351) + 1;
    let year = (approx - 1..=approx + 1).rev().find(|y| new_year(*y) <= date).unwrap_or(approx - 1);
    let start = if date < fixed_from_hebrew(year, NISAN, 1) { TISHRI } else { NISAN };
    let mut month = start;
    while month < 13 && date > fixed_from_hebrew(year, month, last_day(month, year)) {
        month += 1;
    }
    let day = date - fixed_from_hebrew(year, month, 1) + 1;
    (year, month, day)
}

const HEBREW_MONTHS: [&str; 13] = [
    "Nisan", "Iyyar", "Sivan", "Tammuz", "Av", "Elul", "Tishri", "Heshvan", "Kislev", "Tevet", "Shevat",
    "Adar", "Adar II",
];

// ---------------------------------------------------------------------------------------------------------

/// The date `day` (days since 1970-01-01) falls on in `calendar`.
pub fn to_alt(calendar: AltCalendar, day: i64) -> AltDate {
    // Arithmetic below stays well inside i64 for the clamped range (|fixed| < 1.1e9, products < 3e13).
    let f = fixed(day);
    let (year, month, d) = match calendar {
        AltCalendar::Hijri => islamic_from_fixed(f),
        AltCalendar::Hebrew => hebrew_from_fixed(f),
    };
    AltDate {
        calendar,
        year,
        month: u8::try_from(month.clamp(1, 13)).unwrap_or(1),
        day: u8::try_from(d.clamp(1, 30)).unwrap_or(1),
    }
}

impl AltDate {
    /// The month's name ("Rabiʿ II", "Tishri"; Adar is "Adar I" in a Hebrew leap year).
    pub fn month_name(&self) -> &'static str {
        let i = usize::from(self.month.saturating_sub(1));
        match self.calendar {
            AltCalendar::Hijri => HIJRI_MONTHS.get(i).copied().unwrap_or(""),
            AltCalendar::Hebrew if self.month == 12 && hebrew_leap(self.year) => "Adar I",
            AltCalendar::Hebrew => HEBREW_MONTHS.get(i).copied().unwrap_or(""),
        }
    }

    /// "25 Rabiʿ II 1448" / "27 Tishri 5787".
    pub fn long(&self) -> String {
        format!("{} {} {}", self.day, self.month_name(), self.year)
    }

    /// "25 Rabiʿ II" / "27 Tishri" (day headers and month cells).
    pub fn short(&self) -> String {
        format!("{} {}", self.day, self.month_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Days since 1970-01-01 of a Gregorian date.
    fn day(y: i64, m: u8, d: u8) -> i64 {
        mailcraft_mime::date::days_from_civil(y, m, d)
    }

    #[test]
    fn hijri_dates() {
        let d = |y, m, dd| to_alt(AltCalendar::Hijri, day(y, m, dd));
        assert_eq!(d(2000, 1, 1).long(), "24 Ramadan 1420");
        assert_eq!(d(2024, 3, 11).long(), "1 Ramadan 1445");
        assert_eq!(d(2026, 10, 8).long(), "25 Rabi\u{2bf} II 1448");
        assert_eq!(d(1970, 1, 1).short(), "22 Shawwal");
    }

    #[test]
    fn hebrew_dates() {
        let d = |y, m, dd| to_alt(AltCalendar::Hebrew, day(y, m, dd));
        assert_eq!(d(2000, 1, 1).long(), "23 Tevet 5760");
        // Rosh Hashanah 5787, Passover 5784, Purim 5787 (a leap year: Adar II), 1 Adar II 5784.
        assert_eq!(d(2026, 9, 12).long(), "1 Tishri 5787");
        assert_eq!(d(2024, 4, 23).long(), "15 Nisan 5784");
        assert_eq!(d(2027, 3, 23).long(), "14 Adar II 5787");
        assert_eq!(d(2024, 3, 11).long(), "1 Adar II 5784");
        assert_eq!(d(2024, 2, 10).month_name(), "Adar I");
        assert_eq!(d(2026, 10, 8).short(), "27 Tishri");
    }

    #[test]
    fn every_day_maps_forward_without_gaps() {
        for cal in [AltCalendar::Hijri, AltCalendar::Hebrew] {
            let mut prev = to_alt(cal, day(2019, 1, 1));
            for n in 1..3000 {
                let cur = to_alt(cal, day(2019, 1, 1) + n);
                let next_day = cur.day == prev.day + 1 && cur.month == prev.month && cur.year == prev.year;
                let new_month = cur.day == 1 && (cur.month != prev.month || cur.year != prev.year);
                assert!(next_day || new_month, "{cal:?}: {prev:?} -> {cur:?}");
                assert!(prev.day >= 29 || next_day, "{cal:?}: month ended on {prev:?}");
                prev = cur;
            }
        }
        // Hostile day numbers never panic.
        for n in [i64::MIN, i64::MAX, -MAX_DAYS, MAX_DAYS] {
            let _ = to_alt(AltCalendar::Hebrew, n).long();
            let _ = to_alt(AltCalendar::Hijri, n).long();
        }
    }
}
