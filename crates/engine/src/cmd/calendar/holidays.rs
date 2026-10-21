//! File › Options › Calendar › Add Holidays: public holidays of a country added to the calendar as all-day
//! events (shown as Free, category "Holiday", the country as their location).
//!
//! The dates are computed from each holiday's rule (a fixed date, the n-th or last weekday of a month, or a
//! number of days from Easter Sunday, Gregorian computus), so no holiday tables are bundled. Each country
//! lists its national public holidays: for Canada the federal statutory holidays (some provinces keep others),
//! for the United Kingdom the bank holidays of England and Wales (Scotland and Northern Ireland differ in a
//! few), for the United States the federal holidays, for Germany the holidays kept in every state. Regional
//! holidays and substitute days off are left out. Adding the same country again adds only the holidays that
//! are not there yet (each holiday has a stable UID).

use serde_json::{Value, json};

use super::{pim, tz};
use crate::{CmdError, CmdResult, Session};
use mailcraft_formats::ics::value::local_secs;
use mailcraft_model::{Appointment, BusyStatus, ItemId};

/// Most years added in one go.
pub const MAX_YEARS: i64 = 30;
/// Category the holidays carry.
pub const HOLIDAY_CATEGORY: &str = "Holiday";

/// When a holiday falls.
#[derive(Clone, Copy, Debug)]
enum Rule {
    /// Month, day.
    Fixed(u8, u8),
    /// The `n`-th weekday (0 = Monday) of a month (`n` from 1).
    Nth(u8, u8, u8),
    /// The last weekday (0 = Monday) of a month.
    Last(u8, u8),
    /// The last weekday on or before a date (month, day, weekday).
    OnOrBefore(u8, u8, u8),
    /// Days from Easter Sunday.
    Easter(i64),
    /// From `since` on: the first Monday of a month, or its 1st when that is a Friday (month, since).
    FirstMondayOrFriday(u8, i64),
}

use Rule::*;

const MON: u8 = 0;
const THU: u8 = 3;
const FRI: u8 = 4;

/// Countries offered (code, name, holidays).
pub const COUNTRIES: &[(&str, &str)] = &[
    ("AU", "Australia"),
    ("CA", "Canada"),
    ("FR", "France"),
    ("DE", "Germany"),
    ("IE", "Ireland"),
    ("NZ", "New Zealand"),
    ("GB", "United Kingdom"),
    ("US", "United States"),
];

fn rules(code: &str) -> &'static [(&'static str, Rule)] {
    match code {
        "US" => &[
            ("New Year's Day", Fixed(1, 1)),
            ("Martin Luther King Jr. Day", Nth(1, MON, 3)),
            ("Washington's Birthday", Nth(2, MON, 3)),
            ("Memorial Day", Last(5, MON)),
            ("Juneteenth", Fixed(6, 19)),
            ("Independence Day", Fixed(7, 4)),
            ("Labor Day", Nth(9, MON, 1)),
            ("Columbus Day", Nth(10, MON, 2)),
            ("Veterans Day", Fixed(11, 11)),
            ("Thanksgiving Day", Nth(11, THU, 4)),
            ("Christmas Day", Fixed(12, 25)),
        ],
        "CA" => &[
            ("New Year's Day", Fixed(1, 1)),
            ("Good Friday", Easter(-2)),
            ("Easter Monday", Easter(1)),
            ("Victoria Day", OnOrBefore(5, 24, MON)),
            ("Canada Day", Fixed(7, 1)),
            ("Civic Holiday", Nth(8, MON, 1)),
            ("Labour Day", Nth(9, MON, 1)),
            ("National Day for Truth and Reconciliation", Fixed(9, 30)),
            ("Thanksgiving", Nth(10, MON, 2)),
            ("Remembrance Day", Fixed(11, 11)),
            ("Christmas Day", Fixed(12, 25)),
            ("Boxing Day", Fixed(12, 26)),
        ],
        "GB" => &[
            ("New Year's Day", Fixed(1, 1)),
            ("Good Friday", Easter(-2)),
            ("Easter Monday", Easter(1)),
            ("Early May Bank Holiday", Nth(5, MON, 1)),
            ("Spring Bank Holiday", Last(5, MON)),
            ("Summer Bank Holiday", Last(8, MON)),
            ("Christmas Day", Fixed(12, 25)),
            ("Boxing Day", Fixed(12, 26)),
        ],
        "IE" => &[
            ("New Year's Day", Fixed(1, 1)),
            // A public holiday since 2023.
            ("St. Brigid's Day", FirstMondayOrFriday(2, 2023)),
            ("St. Patrick's Day", Fixed(3, 17)),
            ("Easter Monday", Easter(1)),
            ("May Bank Holiday", Nth(5, MON, 1)),
            ("June Bank Holiday", Nth(6, MON, 1)),
            ("August Bank Holiday", Nth(8, MON, 1)),
            ("October Bank Holiday", Last(10, MON)),
            ("Christmas Day", Fixed(12, 25)),
            ("St. Stephen's Day", Fixed(12, 26)),
        ],
        "AU" => &[
            ("New Year's Day", Fixed(1, 1)),
            ("Australia Day", Fixed(1, 26)),
            ("Good Friday", Easter(-2)),
            ("Easter Monday", Easter(1)),
            ("Anzac Day", Fixed(4, 25)),
            ("Christmas Day", Fixed(12, 25)),
            ("Boxing Day", Fixed(12, 26)),
        ],
        "NZ" => &[
            ("New Year's Day", Fixed(1, 1)),
            ("Day after New Year's Day", Fixed(1, 2)),
            ("Waitangi Day", Fixed(2, 6)),
            ("Good Friday", Easter(-2)),
            ("Easter Monday", Easter(1)),
            ("Anzac Day", Fixed(4, 25)),
            ("King's Birthday", Nth(6, MON, 1)),
            ("Labour Day", Nth(10, MON, 4)),
            ("Christmas Day", Fixed(12, 25)),
            ("Boxing Day", Fixed(12, 26)),
        ],
        "DE" => &[
            ("New Year's Day", Fixed(1, 1)),
            ("Good Friday", Easter(-2)),
            ("Easter Monday", Easter(1)),
            ("Labour Day", Fixed(5, 1)),
            ("Ascension Day", Easter(39)),
            ("Whit Monday", Easter(50)),
            ("German Unity Day", Fixed(10, 3)),
            ("Christmas Day", Fixed(12, 25)),
            ("Second Day of Christmas", Fixed(12, 26)),
        ],
        "FR" => &[
            ("New Year's Day", Fixed(1, 1)),
            ("Easter Monday", Easter(1)),
            ("Labour Day", Fixed(5, 1)),
            ("Victory in Europe Day", Fixed(5, 8)),
            ("Ascension Day", Easter(39)),
            ("Whit Monday", Easter(50)),
            ("Bastille Day", Fixed(7, 14)),
            ("Assumption of Mary", Fixed(8, 15)),
            ("All Saints' Day", Fixed(11, 1)),
            ("Armistice Day", Fixed(11, 11)),
            ("Christmas Day", Fixed(12, 25)),
        ],
        _ => &[],
    }
}

/// Day number (days since 1970-01-01) of a date.
fn day_of(year: i64, month: u8, day: u8) -> Option<i64> {
    local_secs(year, month, day, 0, 0, 0).map(|s| s.div_euclid(pim::DAY_SECS))
}

/// Weekday of a day number, 0 = Monday.
fn weekday(day: i64) -> u8 {
    u8::try_from((day + 3).rem_euclid(7)).unwrap_or(0)
}

/// Easter Sunday of `year` (Gregorian; the anonymous computus).
fn easter(year: i64) -> Option<i64> {
    let a = year.rem_euclid(19);
    let b = year.div_euclid(100);
    let c = year.rem_euclid(100);
    let d = b.div_euclid(4);
    let e = b.rem_euclid(4);
    let f = (b + 8).div_euclid(25);
    let g = (b - f + 1).div_euclid(3);
    let h = (19 * a + b - d - g + 15).rem_euclid(30);
    let i = c.div_euclid(4);
    let k = c.rem_euclid(4);
    let l = (32 + 2 * e + 2 * i - h - k).rem_euclid(7);
    let m = (a + 11 * h + 22 * l).div_euclid(451);
    let month = (h + l - 7 * m + 114).div_euclid(31);
    let day = (h + l - 7 * m + 114).rem_euclid(31) + 1;
    day_of(year, u8::try_from(month).ok()?, u8::try_from(day).ok()?)
}

fn date(rule: Rule, year: i64) -> Option<i64> {
    match rule {
        Fixed(m, d) => day_of(year, m, d),
        Nth(m, wd, n) => {
            let first = day_of(year, m, 1)?;
            let shift = i64::from((wd + 7 - weekday(first)) % 7);
            Some(first + shift + 7 * i64::from(n.saturating_sub(1)))
        }
        Last(m, wd) => {
            let next = if m == 12 { day_of(year + 1, 1, 1)? } else { day_of(year, m + 1, 1)? };
            let last = next - 1;
            Some(last - i64::from((weekday(last) + 7 - wd) % 7))
        }
        OnOrBefore(m, d, wd) => {
            let at = day_of(year, m, d)?;
            Some(at - i64::from((weekday(at) + 7 - wd) % 7))
        }
        Easter(n) => easter(year).map(|e| e + n),
        FirstMondayOrFriday(m, since) => {
            if year < since {
                return None;
            }
            let first = day_of(year, m, 1)?;
            if weekday(first) == FRI { Some(first) } else { date(Nth(m, MON, 1), year) }
        }
    }
}

/// The holidays of country `code` in `year`: (name, day number), in date order.
pub fn holidays(code: &str, year: i64) -> Vec<(&'static str, i64)> {
    let mut out: Vec<(&'static str, i64)> =
        rules(code).iter().filter_map(|(name, r)| date(*r, year).map(|d| (*name, d))).collect();
    out.sort_by_key(|(_, d)| *d);
    out
}

fn country_name(code: &str) -> Option<&'static str> {
    COUNTRIES.iter().find(|(c, _)| c.eq_ignore_ascii_case(code)).map(|(_, n)| *n)
}

/// `calendar.addHolidays {countries, from?, to?, folder?}`: adds the holidays of the countries (codes) for
/// the years `from` to `to` (default: this year and the next two) to the calendar. Without `countries` the
/// Add Holidays window opens.
pub(super) fn run_add_holidays(s: &mut Session, p: &Value) -> CmdResult {
    let list = match p.get("countries") {
        None | Some(Value::Null) => {
            let countries: Vec<Value> =
                COUNTRIES.iter().map(|(c, n)| json!({"code": c, "name": n})).collect();
            return pim::dialog(s, "addHolidays", json!({"countries": countries}));
        }
        Some(Value::Array(a)) => a,
        Some(_) => return Err(CmdError::Params("`countries` must be a list of country codes".into())),
    };
    let mut codes: Vec<&'static str> = Vec::new();
    for v in list.iter().take(COUNTRIES.len().saturating_add(10)) {
        let c = v.as_str().map(str::trim).unwrap_or_default();
        let found = COUNTRIES
            .iter()
            .find(|(code, name)| code.eq_ignore_ascii_case(c) || name.eq_ignore_ascii_case(c));
        match found {
            Some((code, _)) if !codes.contains(code) => codes.push(code),
            Some(_) => {}
            None => return Err(CmdError::Params(format!("no holidays for `{c}`"))),
        }
    }
    if codes.is_empty() {
        return Err(CmdError::Params("choose at least one country".into()));
    }
    let clock = tz::Clock::of(s);
    let this_year = mailcraft_formats::ics::value::civil(clock.local(s.now().unix)).0;
    let year = |key: &str, default: i64| -> Result<i64, CmdError> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(v) => v
                .as_i64()
                .filter(|y| (1900..=2200).contains(y))
                .ok_or_else(|| CmdError::Params(format!("`{key}` must be a year (1900-2200)"))),
        }
    };
    let from = year("from", this_year)?;
    let to = year("to", this_year.saturating_add(2))?;
    if to < from || to - from >= MAX_YEARS {
        return Err(CmdError::Params(format!("choose up to {MAX_YEARS} years, the first before the last")));
    }
    // A calendar folder that exists and can be written to (never a mail folder or an unknown id).
    let folder = super::new_appointment_folder(s, p)?;
    let mut added = 0usize;
    let mut skipped = 0usize;
    for code in &codes {
        let name = country_name(code).unwrap_or(code);
        for y in from..=to {
            for (holiday, day) in holidays(code, y) {
                let uid =
                    format!("holiday-{}-{day}-{}@mailcraft.local", code.to_ascii_lowercase(), slug(holiday));
                let exists = s
                    .mailbox
                    .appointments
                    .values()
                    .any(|a| a.folder == folder && a.uid.eq_ignore_ascii_case(&uid));
                if exists {
                    skipped = skipped.saturating_add(1);
                    continue;
                }
                let start = clock.from_local(day.saturating_mul(pim::DAY_SECS));
                let end = clock.from_local(day.saturating_add(1).saturating_mul(pim::DAY_SECS));
                let id = ItemId(s.mailbox.alloc_id());
                s.mailbox.appointments.insert(
                    id,
                    Appointment {
                        id,
                        folder,
                        uid,
                        subject: holiday.to_owned(),
                        location: name.to_owned(),
                        start,
                        end,
                        all_day: true,
                        show_as: BusyStatus::Free,
                        categories: vec![HOLIDAY_CATEGORY.to_owned()],
                        ..Default::default()
                    },
                );
                added = added.saturating_add(1);
            }
        }
    }
    Ok(
        json!({"added": added, "alreadyThere": skipped, "folder": folder.0, "from": from, "to": to, "countries": codes}),
    )
}

fn slug(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd(day: i64) -> (i64, u8, u8) {
        let (y, m, d, ..) = mailcraft_formats::ics::value::civil(day * pim::DAY_SECS);
        (y, m, d)
    }

    fn find(code: &str, year: i64, name: &str) -> (i64, u8, u8) {
        let d = holidays(code, year).into_iter().find(|(n, _)| *n == name).unwrap().1;
        ymd(d)
    }

    #[test]
    fn holiday_rules_give_the_known_dates() {
        assert_eq!(ymd(easter(2026).unwrap()), (2026, 4, 5));
        assert_eq!(ymd(easter(2027).unwrap()), (2027, 3, 28));
        assert_eq!(ymd(easter(2000).unwrap()), (2000, 4, 23));
        assert_eq!(find("US", 2026, "Thanksgiving Day"), (2026, 11, 26));
        assert_eq!(find("US", 2026, "Memorial Day"), (2026, 5, 25));
        assert_eq!(find("US", 2026, "Martin Luther King Jr. Day"), (2026, 1, 19));
        assert_eq!(find("CA", 2026, "Victoria Day"), (2026, 5, 18));
        assert_eq!(find("CA", 2027, "Victoria Day"), (2027, 5, 24));
        assert_eq!(find("CA", 2026, "Thanksgiving"), (2026, 10, 12));
        assert_eq!(find("GB", 2026, "Good Friday"), (2026, 4, 3));
        assert_eq!(find("GB", 2026, "Summer Bank Holiday"), (2026, 8, 31));
        assert_eq!(find("DE", 2026, "Ascension Day"), (2026, 5, 14));
        assert_eq!(find("DE", 2026, "Whit Monday"), (2026, 5, 25));
        assert_eq!(find("NZ", 2026, "Labour Day"), (2026, 10, 26));
        // St. Brigid's Day: the first Monday of February, or 1 February when that is a Friday; from 2023.
        assert_eq!(find("IE", 2026, "St. Brigid's Day"), (2026, 2, 2));
        assert_eq!(find("IE", 2030, "St. Brigid's Day"), (2030, 2, 1));
        assert_eq!(find("IE", 2023, "St. Brigid's Day"), (2023, 2, 6));
        assert!(holidays("IE", 2022).iter().all(|(n, _)| *n != "St. Brigid's Day"));
        assert!(holidays("XX", 2026).is_empty());
        for (code, _) in COUNTRIES {
            let h = holidays(code, 2026);
            assert!(h.len() >= 7, "{code}");
            assert!(h.windows(2).all(|w| w[0].1 <= w[1].1), "{code} in date order");
        }
    }

    #[test]
    fn adding_holidays_twice_adds_them_once() {
        let mut s = super::super::tests::session();
        let r = s.run("calendar.addHolidays", &json!({})).unwrap();
        assert_eq!(r["dialog"], "addHolidays");
        let r = s
            .run(
                "calendar.addHolidays",
                &json!({"countries": ["CA", "United States"], "from": 2026, "to": 2026}),
            )
            .unwrap();
        let n = r["added"].as_u64().unwrap();
        assert_eq!(n, 12 + 11);
        let canada = s.mailbox.appointments.values().find(|a| a.subject == "Canada Day").unwrap();
        assert!(canada.all_day && canada.show_as == BusyStatus::Free);
        assert_eq!(canada.location, "Canada");
        assert_eq!(canada.categories, [HOLIDAY_CATEGORY]);
        assert_eq!(canada.end.unix - canada.start.unix, pim::DAY_SECS);
        let inbox = super::super::tests::role(&s, mailcraft_model::FolderRole::Inbox);
        let again =
            s.run("calendar.addHolidays", &json!({"countries": ["ca"], "from": 2026, "to": 2027})).unwrap();
        assert_eq!(again["alreadyThere"], 12);
        assert_eq!(again["added"], 12, "only the new year's");
        for bad in [
            json!({"countries": ["Atlantis"]}),
            json!({"countries": []}),
            json!({"countries": "CA"}),
            json!({"countries": ["CA"], "from": 2030, "to": 2026}),
            json!({"countries": ["CA"], "from": 1000}),
            json!({"countries": ["CA"], "from": 2000, "to": 2100}),
            // Regression: a mail folder or an unknown folder took the holidays.
            json!({"countries": ["CA"], "folder": inbox.0}),
            json!({"countries": ["CA"], "folder": 987_654_321}),
        ] {
            let before = s.mailbox.appointments.len();
            assert!(s.run("calendar.addHolidays", &bad).is_err(), "{bad}");
            assert_eq!(s.mailbox.appointments.len(), before, "{bad}");
        }
    }
}
