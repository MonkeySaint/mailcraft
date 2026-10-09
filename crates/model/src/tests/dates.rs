use proptest::prelude::*;

use super::{EDT, edt};
use crate::dates::{
    LocalTime, WeekdayName, civil_from_days, days_from_civil, days_in_month, is_leap_year, local_datetime,
    weekday_of_day,
};
use crate::{DateGroup, DateTime, ListTime};

/// "Now" from the observation session: Friday 2026-10-09, 10:00 AM local.
fn now() -> DateTime {
    edt(2026, 10, 9, 10, 0)
}

fn group(y: i64, m: u8, d: u8) -> DateGroup {
    DateGroup::of(edt(y, m, d, 12, 0), now(), EDT)
}

#[test]
fn epoch_and_known_days() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(days_from_civil(1970, 1, 1), Some(0));
    assert_eq!(days_from_civil(2000, 3, 1), Some(11_017));
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    assert_eq!(days_from_civil(1600, 2, 29).map(civil_from_days), Some((1600, 2, 29)));
    assert_eq!(weekday_of_day(0).name(), "Thursday");
    let fri = days_from_civil(2026, 10, 9).unwrap();
    assert_eq!(weekday_of_day(fri).name(), "Friday");
    assert_eq!(weekday_of_day(fri - 5).name(), "Sunday");
}

#[test]
fn invalid_civil_dates_rejected() {
    assert_eq!(days_from_civil(2026, 2, 29), None);
    assert_eq!(days_from_civil(2026, 13, 1), None);
    assert_eq!(days_from_civil(2026, 0, 1), None);
    assert_eq!(days_from_civil(2026, 4, 31), None);
    assert_eq!(days_from_civil(2026, 4, 0), None);
    assert!(days_from_civil(2024, 2, 29).is_some());
    assert_eq!(local_datetime(2026, 1, 1, 24, 0, 0, 0), None);
    assert_eq!(local_datetime(2026, 1, 1, 0, 60, 0, 0), None);
    assert_eq!(local_datetime(2026, 1, 1, 0, 0, 60, 0), None);
}

#[test]
fn leap_years() {
    assert!(is_leap_year(2000));
    assert!(is_leap_year(2024));
    assert!(!is_leap_year(1900));
    assert!(!is_leap_year(2026));
    assert_eq!(days_in_month(2024, 2), 29);
    assert_eq!(days_in_month(2026, 2), 28);
    assert_eq!(days_in_month(2026, 9), 30);
    assert_eq!(days_in_month(2026, 13), 0);
}

#[test]
fn local_time_breakdown() {
    let t = edt(2026, 10, 8, 7, 30);
    let lt = LocalTime::of(t, EDT);
    assert_eq!((lt.year, lt.month, lt.day, lt.hour, lt.minute), (2026, 10, 8, 7, 30));
    assert_eq!(lt.weekday.short(), "Thu");
    assert_eq!(lt.month_name(), "October");
    // Same instant in UTC is 11:30.
    let utc = LocalTime::of(t, 0);
    assert_eq!(utc.hour, 11);
}

#[test]
fn groups_observed_session() {
    assert_eq!(group(2026, 10, 9), DateGroup::Today);
    assert_eq!(group(2026, 10, 8), DateGroup::Yesterday);
    assert_eq!(group(2026, 10, 7), DateGroup::ThisWeek(WeekdayName(3)));
    assert_eq!(group(2026, 10, 6).label(), "Tuesday");
    assert_eq!(group(2026, 10, 5).label(), "Monday");
    assert_eq!(group(2026, 10, 4).label(), "Sunday");
    assert_eq!(group(2026, 10, 3), DateGroup::LastWeek);
    assert_eq!(group(2026, 9, 30), DateGroup::LastWeek);
    assert_eq!(group(2026, 9, 27), DateGroup::LastWeek);
    assert_eq!(group(2026, 9, 26), DateGroup::TwoWeeksAgo);
    assert_eq!(group(2026, 9, 20), DateGroup::TwoWeeksAgo);
    assert_eq!(group(2026, 9, 19), DateGroup::ThreeWeeksAgo);
    assert_eq!(group(2026, 9, 13), DateGroup::ThreeWeeksAgo);
    assert_eq!(group(2026, 9, 12), DateGroup::LastMonth);
    assert_eq!(group(2026, 9, 1), DateGroup::LastMonth);
    assert_eq!(group(2026, 8, 31), DateGroup::Older);
    assert_eq!(group(2025, 10, 9), DateGroup::Older);
    assert_eq!(group(2026, 10, 10), DateGroup::Future);
}

#[test]
fn later_today_is_still_today() {
    assert_eq!(DateGroup::of(edt(2026, 10, 9, 23, 0), now(), EDT), DateGroup::Today);
}

#[test]
fn last_month_includes_earlier_this_month() {
    // Today Wednesday 2026-10-28: Oct 1 is more than three weeks back but in the current month.
    let n = edt(2026, 10, 28, 9, 0);
    assert_eq!(DateGroup::of(edt(2026, 10, 1, 9, 0), n, EDT), DateGroup::LastMonth);
    assert_eq!(DateGroup::of(edt(2026, 9, 1, 9, 0), n, EDT), DateGroup::LastMonth);
    assert_eq!(DateGroup::of(edt(2026, 8, 31, 9, 0), n, EDT), DateGroup::Older);
}

#[test]
fn last_month_across_new_year() {
    let n = edt(2027, 1, 30, 9, 0);
    assert_eq!(DateGroup::of(edt(2026, 12, 2, 9, 0), n, EDT), DateGroup::LastMonth);
    assert_eq!(DateGroup::of(edt(2026, 11, 30, 9, 0), n, EDT), DateGroup::Older);
}

#[test]
fn yesterday_wins_across_week_boundary() {
    // Sunday 2026-10-11: Saturday is yesterday even though it belongs to last week.
    let sunday = edt(2026, 10, 11, 8, 0);
    assert_eq!(DateGroup::of(edt(2026, 10, 10, 8, 0), sunday, EDT), DateGroup::Yesterday);
    assert_eq!(DateGroup::of(edt(2026, 10, 9, 8, 0), sunday, EDT), DateGroup::LastWeek);
    assert_eq!(ListTime::format(edt(2026, 10, 10, 8, 0), sunday, EDT), "Sat 8:00 AM");
}

#[test]
fn offset_changes_the_day() {
    // 02:00 UTC on the 9th is 22:00 on the 8th in EDT.
    let t = edt(2026, 10, 8, 22, 0);
    assert_eq!(LocalTime::of(t, 0).day, 9);
    assert_eq!(LocalTime::of(t, EDT).day, 8);
    assert_eq!(DateGroup::of(t, now(), EDT), DateGroup::Yesterday);
    assert_eq!(DateGroup::of(t, DateTime { unix: now().unix, offset_min: 0 }, 0), DateGroup::Today);
}

#[test]
fn labels() {
    assert_eq!(DateGroup::Today.label(), "Today");
    assert_eq!(DateGroup::Yesterday.label(), "Yesterday");
    assert_eq!(DateGroup::ThisWeek(WeekdayName(2)).label(), "Tuesday");
    assert_eq!(DateGroup::LastWeek.label(), "Last Week");
    assert_eq!(DateGroup::TwoWeeksAgo.label(), "Two Weeks Ago");
    assert_eq!(DateGroup::ThreeWeeksAgo.label(), "Three Weeks Ago");
    assert_eq!(DateGroup::LastMonth.label(), "Last Month");
    assert_eq!(DateGroup::Older.label(), "Older");
    assert_eq!(DateGroup::Future.label(), "Future");
}

#[test]
fn group_rank_orders_newest_first() {
    let groups = [
        group(2026, 10, 10),
        group(2026, 10, 9),
        group(2026, 10, 8),
        group(2026, 10, 7),
        group(2026, 10, 5),
        group(2026, 9, 30),
        group(2026, 9, 22),
        group(2026, 9, 15),
        group(2026, 9, 2),
        group(2020, 1, 1),
    ];
    for w in groups.windows(2) {
        assert!(w[0].rank() < w[1].rank(), "{:?} before {:?}", w[0], w[1]);
    }
}

#[test]
fn list_time_observed_formats() {
    let n = now();
    assert_eq!(ListTime::format(edt(2026, 10, 9, 9, 10), n, EDT), "9:10 AM");
    assert_eq!(ListTime::format(edt(2026, 10, 8, 7, 30), n, EDT), "Thu 7:30 AM");
    assert_eq!(ListTime::format(edt(2026, 10, 6, 15, 5), n, EDT), "Tue 3:05 PM");
    assert_eq!(ListTime::format(edt(2026, 9, 30, 9, 0), n, EDT), "Wed 9/30");
    assert_eq!(ListTime::format(edt(2026, 9, 19, 9, 0), n, EDT), "9/19/2026");
    assert_eq!(ListTime::format(edt(2025, 12, 25, 9, 0), n, EDT), "12/25/2025");
    assert_eq!(ListTime::format(edt(2026, 10, 12, 9, 0), n, EDT), "10/12/2026");
}

#[test]
fn header_and_long_formats() {
    assert_eq!(ListTime::header(edt(2026, 10, 8, 7, 30), EDT), "Thu 10/8/2026 7:30 AM");
    assert_eq!(ListTime::long(edt(2026, 10, 8, 4, 30), EDT), "Thursday, October 8, 2026 4:30 AM");
    assert_eq!(ListTime::long_date(edt(2026, 10, 9, 4, 30), EDT), "Friday, October 9, 2026");
    assert_eq!(ListTime::short_date_time(edt(2026, 10, 9, 3, 0), EDT), "10/9/2026 3:00 AM");
    assert_eq!(ListTime::date(edt(2026, 1, 2, 3, 0), EDT), "1/2/2026");
}

#[test]
fn twelve_hour_clock_edges() {
    assert_eq!(ListTime::clock(edt(2026, 10, 9, 0, 0), EDT), "12:00 AM");
    assert_eq!(ListTime::clock(edt(2026, 10, 9, 0, 59), EDT), "12:59 AM");
    assert_eq!(ListTime::clock(edt(2026, 10, 9, 1, 0), EDT), "1:00 AM");
    assert_eq!(ListTime::clock(edt(2026, 10, 9, 11, 59), EDT), "11:59 AM");
    assert_eq!(ListTime::clock(edt(2026, 10, 9, 12, 5), EDT), "12:05 PM");
    assert_eq!(ListTime::clock(edt(2026, 10, 9, 13, 0), EDT), "1:00 PM");
    assert_eq!(ListTime::clock(edt(2026, 10, 9, 23, 59), EDT), "11:59 PM");
}

#[test]
fn pre_epoch_dates_format() {
    let t = local_datetime(1969, 7, 20, 20, 17, 0, 0).unwrap();
    assert_eq!(ListTime::header(t, 0), "Sun 7/20/1969 8:17 PM");
}

#[test]
fn extreme_values_do_not_panic() {
    for unix in [i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX] {
        for off in [i32::MIN, -1440, 0, 1440, i32::MAX] {
            let t = DateTime { unix, offset_min: off };
            let _ = DateGroup::of(t, now(), off);
            let _ = DateGroup::of(now(), t, off);
            let _ = ListTime::format(t, now(), off);
            let _ = ListTime::header(t, off);
            let _ = ListTime::long(t, off);
        }
    }
}

proptest! {
    #[test]
    fn civil_round_trip(days in -(1_i64 << 40)..(1_i64 << 40)) {
        let (y, m, d) = civil_from_days(days);
        prop_assert!((1..=12).contains(&m));
        prop_assert!(d >= 1 && d <= days_in_month(y, m));
        prop_assert_eq!(days_from_civil(y, m, d), Some(days));
    }

    #[test]
    fn consecutive_days_advance_weekday(days in -1_000_000_i64..1_000_000) {
        prop_assert_eq!((weekday_of_day(days).0 + 1) % 7, weekday_of_day(days + 1).0);
    }

    #[test]
    fn hostile_times_never_panic(t in any::<i64>(), n in any::<i64>(), off in any::<i32>()) {
        let a = DateTime { unix: t, offset_min: 0 };
        let b = DateTime { unix: n, offset_min: 0 };
        let g = DateGroup::of(a, b, off);
        prop_assert!(!g.label().is_empty());
        let _ = ListTime::format(a, b, off);
        let _ = ListTime::long(a, off);
    }

    #[test]
    fn groups_are_monotonic_in_age(age_days in 0_i64..400, n_days in 0_i64..30_000) {
        // An older message never lands in a "newer" group than a younger one.
        let now = DateTime { unix: n_days * 86_400 + 43_200, offset_min: 0 };
        let a = DateTime { unix: now.unix - age_days * 86_400, offset_min: 0 };
        let b = DateTime { unix: now.unix - (age_days + 1) * 86_400, offset_min: 0 };
        prop_assert!(DateGroup::of(a, now, 0).rank() <= DateGroup::of(b, now, 0).rank());
    }
}
