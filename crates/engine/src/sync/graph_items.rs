//! Microsoft Graph contacts, To Do tasks and events (REST v1.0 resources `contact`, `todoTask`, `event`) to
//! and from MailCraft's [`Contact`], [`Task`] and [`Appointment`].
//!
//! **Contacts.** Name parts (`givenName`, `middleName`, `surname`, `title`, `generation`), `displayName`,
//! `fileAs`, `companyName`, `department`, `jobTitle`, `emailAddresses` (three), `businessPhones` /
//! `homePhones` / `mobilePhone` (the Business, Home and Mobile numbers; Graph v1.0 keeps no fax, pager or
//! other numbers, which stay here), the business, home and other addresses, `businessHomePage`,
//! `imAddresses`, `birthday`, `personalNotes`, `categories`, `nickName`, `spouseName`, `officeLocation`,
//! `profession`, `manager`, `assistantName`.
//!
//! **Tasks.** `title`, `body` (text), `importance`, `status`, `startDateTime` / `dueDateTime` (days),
//! `completedDateTime`, the reminder (`isReminderOn`, `reminderDateTime`), `categories` and `recurrence`.
//!
//! **Events.** `subject`, `body` (text; HTML is read as its text), `location`, `start` / `end` (with their
//! zones: a Windows zone name or an IANA one, read through MailCraft's zone table; times are asked for in
//! UTC and the zone the event was made in is `originalStartTimeZone`), `isAllDay`, `showAs`, the reminder,
//! `attendees` (with their answers), `organizer` (read only), `categories`, `sensitivity`, `importance`, and
//! a series' `recurrence` (pattern and range) ↔ RRULE. A series' exceptions are events of type `exception`
//! with their `originalStart`; cancelled occurrences become exception dates.
//!
//! Writing produces every mapped property (`null` or empty where the item has none), so an update can send
//! only those whose value differs from the server's copy read the same way (see `graph_pim.rs`). Server
//! data is untrusted: lists and texts are capped and nothing panics on odd input.

use mailcraft_formats::ics::tz::{TimeZone, builtin, builtin_name, iana_of_windows, windows_of_iana};
use mailcraft_formats::ics::value::{civil, local_secs};
use mailcraft_mime::Mailbox;
use mailcraft_model::calendar::{AttendeeRole, ResponseStatus};
use mailcraft_model::contact::PostalAddress;
use mailcraft_model::task::{TaskPriority, TaskRecurrence, TaskStatus};
use mailcraft_model::{Appointment, Attendee, BusyStatus, Contact, DateTime, Importance, Recurrence, Task};
use serde_json::{Map, Value, json};

const DAY: i64 = 86_400;
/// Longest text kept from one value.
const MAX_TEXT: usize = 256 * 1024;
/// Most entries read from one list.
const MAX_LIST: usize = 200;
/// Longest event read (ten years).
const MAX_DURATION: i64 = 10 * 366 * DAY;

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).map(|s| s.chars().take(MAX_TEXT).collect::<String>()).unwrap_or_default()
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.chars().take(1000).collect::<String>())
                .filter(|s| !s.trim().is_empty())
                .take(MAX_LIST)
                .collect()
        })
        .unwrap_or_default()
}

fn opt_text(s: &str) -> Value {
    if s.is_empty() { Value::Null } else { json!(s) }
}

/// A body's text: plain text as it is, HTML as its text.
fn body_text(v: Option<&Value>) -> String {
    let Some(b) = v else { return String::new() };
    let content = text(b.get("content"));
    if b.get("contentType").and_then(Value::as_str).is_some_and(|t| t.eq_ignore_ascii_case("html")) {
        mailcraft_html::html_to_text(&content).trim_end().to_owned()
    } else {
        content.trim_end_matches(['\r', '\n']).to_owned()
    }
}

fn body_of(s: &str) -> Value {
    json!({"contentType": "text", "content": s})
}

// ---------------------------------------------------------------------------------------------------------
// Contacts
// ---------------------------------------------------------------------------------------------------------

/// The contact properties MailCraft writes.
pub(super) const CONTACT_MAPPED: [&str; 28] = [
    "givenName",
    "middleName",
    "surname",
    "title",
    "generation",
    "displayName",
    "fileAs",
    "companyName",
    "department",
    "jobTitle",
    "emailAddresses",
    "businessPhones",
    "homePhones",
    "mobilePhone",
    "businessAddress",
    "homeAddress",
    "otherAddress",
    "businessHomePage",
    "imAddresses",
    "birthday",
    "personalNotes",
    "categories",
    "nickName",
    "spouseName",
    "officeLocation",
    "profession",
    "manager",
    "assistantName",
];

fn address_of(v: Option<&Value>, kind: &str) -> Option<PostalAddress> {
    let a = v?.as_object()?;
    let field = |k: &str| text(a.get(k)).chars().take(1000).collect::<String>();
    let out = PostalAddress {
        kind: kind.to_owned(),
        street: field("street"),
        city: field("city"),
        region: field("state"),
        postal_code: field("postalCode"),
        country: field("countryOrRegion"),
    };
    let empty = out.street.is_empty()
        && out.city.is_empty()
        && out.region.is_empty()
        && out.postal_code.is_empty()
        && out.country.is_empty();
    (!empty).then_some(out)
}

fn address_json(a: Option<&PostalAddress>) -> Value {
    match a {
        Some(a) => json!({"street": a.street, "city": a.city, "state": a.region, "postalCode": a.postal_code,
                          "countryOrRegion": a.country}),
        None => Value::Null,
    }
}

/// `YYYY-MM-DD` of a Graph date or date-time.
fn day_of(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?.trim();
    let d = s.get(..10)?;
    let b = d.as_bytes();
    let ok = b.len() == 10
        && b.get(4) == Some(&b'-')
        && b.get(7) == Some(&b'-')
        && d.chars().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    (ok && !d.starts_with("0001")).then(|| d.to_owned())
}

/// The contact a Graph contact describes.
pub(super) fn to_contact(v: &Value) -> Contact {
    let mut c = Contact {
        first_name: text(v.get("givenName")),
        middle_name: text(v.get("middleName")),
        last_name: text(v.get("surname")),
        title: text(v.get("title")),
        suffix: text(v.get("generation")),
        file_as: text(v.get("fileAs")),
        company: text(v.get("companyName")),
        department: text(v.get("department")),
        job_title: text(v.get("jobTitle")),
        web_page: text(v.get("businessHomePage")),
        notes: text(v.get("personalNotes")),
        nickname: text(v.get("nickName")),
        spouse: text(v.get("spouseName")),
        office: text(v.get("officeLocation")),
        profession: text(v.get("profession")),
        manager: text(v.get("manager")),
        assistant: text(v.get("assistantName")),
        birthday: day_of(v.get("birthday")),
        categories: strings(v.get("categories")),
        ..Default::default()
    };
    if c.full_name().is_empty() {
        c.first_name = text(v.get("displayName"));
    }
    c.emails = v
        .get("emailAddresses")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| e.get("address").and_then(Value::as_str))
                .map(|s| s.trim().chars().take(320).collect::<String>())
                .filter(|s| !s.is_empty())
                .take(3)
                .collect()
        })
        .unwrap_or_default();
    for n in strings(v.get("businessPhones")) {
        c.phones.push(("Business".into(), n));
    }
    for n in strings(v.get("homePhones")) {
        c.phones.push(("Home".into(), n));
    }
    let mobile = text(v.get("mobilePhone"));
    if !mobile.trim().is_empty() {
        c.phones.push(("Mobile".into(), mobile));
    }
    for (k, kind) in [("businessAddress", "Business"), ("homeAddress", "Home"), ("otherAddress", "Other")] {
        if let Some(a) = address_of(v.get(k), kind) {
            c.addresses.push(a);
        }
    }
    c.im_address = strings(v.get("imAddresses")).into_iter().next().unwrap_or_default();
    c
}

/// The Graph properties for `c` (every [`CONTACT_MAPPED`] one).
pub(super) fn from_contact(c: &Contact) -> Map<String, Value> {
    let mut m = Map::new();
    let mut put = |k: &str, v: Value| {
        m.insert(k.to_owned(), v);
    };
    put("givenName", opt_text(&c.first_name));
    put("middleName", opt_text(&c.middle_name));
    put("surname", opt_text(&c.last_name));
    put("title", opt_text(&c.title));
    put("generation", opt_text(&c.suffix));
    let display = [c.full_name(), c.file_as.clone(), c.company.clone()]
        .into_iter()
        .find(|s| !s.trim().is_empty())
        .or_else(|| c.emails.first().cloned())
        .unwrap_or_default();
    put("displayName", opt_text(&display));
    put("fileAs", opt_text(&c.file_as));
    put("companyName", opt_text(&c.company));
    put("department", opt_text(&c.department));
    put("jobTitle", opt_text(&c.job_title));
    let name = c.full_name();
    put(
        "emailAddresses",
        Value::Array(
            c.emails
                .iter()
                .filter(|e| !e.trim().is_empty())
                .take(3)
                .map(
                    |e| json!({"address": e, "name": if name.is_empty() { e.clone() } else { name.clone() }}),
                )
                .collect(),
        ),
    );
    let numbers = |kind: &str| -> Vec<Value> {
        c.phones.iter().filter(|(k, n)| k == kind && !n.trim().is_empty()).map(|(_, n)| json!(n)).collect()
    };
    put("businessPhones", Value::Array(numbers("Business")));
    put("homePhones", Value::Array(numbers("Home")));
    put("mobilePhone", numbers("Mobile").into_iter().next().unwrap_or(Value::Null));
    let addr = |kind: &str| c.addresses.iter().find(|a| a.kind == kind);
    put("businessAddress", address_json(addr("Business")));
    put("homeAddress", address_json(addr("Home")));
    put("otherAddress", address_json(addr("Other")));
    put("businessHomePage", opt_text(&c.web_page));
    put("imAddresses", if c.im_address.trim().is_empty() { json!([]) } else { json!([c.im_address]) });
    put(
        "birthday",
        c.birthday
            .as_deref()
            .and_then(|b| day_of(Some(&json!(b))))
            .map_or(Value::Null, |d| json!(format!("{d}T00:00:00Z"))),
    );
    put("personalNotes", opt_text(&c.notes));
    put("categories", json!(c.categories));
    put("nickName", opt_text(&c.nickname));
    put("spouseName", opt_text(&c.spouse));
    put("officeLocation", opt_text(&c.office));
    put("profession", opt_text(&c.profession));
    put("manager", opt_text(&c.manager));
    put("assistantName", opt_text(&c.assistant));
    m
}

// ---------------------------------------------------------------------------------------------------------
// Zones and times
// ---------------------------------------------------------------------------------------------------------

/// Where a Graph time is.
pub(super) enum Zone {
    /// A zone MailCraft knows (its IANA name, as MailCraft spells it).
    Named(TimeZone, &'static str),
    /// UTC, or a zone MailCraft does not know (read as UTC).
    Utc,
}

/// The zone a Graph time zone name (Windows or IANA) stands for.
pub(super) fn zone_named(name: &str) -> Zone {
    let n = name.trim();
    if n.is_empty() || n.eq_ignore_ascii_case("utc") || n.to_ascii_lowercase().contains("microsoft/utc") {
        return Zone::Utc;
    }
    match iana_of_windows(n).or_else(|| builtin_name(n)) {
        Some(iana) => match builtin(iana) {
            Some(z) => Zone::Named(z, iana),
            None => Zone::Utc,
        },
        None => Zone::Utc,
    }
}

/// True when MailCraft knows the zone a Graph time zone name stands for, or the name says UTC: times in it
/// can be compared with the service's to the second (an unknown zone is read as UTC, which drifts by the
/// zone's offset and its daylight saving time).
pub(super) fn zone_known(name: &str) -> bool {
    let n = name.trim();
    if n.is_empty() {
        return false;
    }
    if n.eq_ignore_ascii_case("utc") || n.to_ascii_lowercase().contains("microsoft/utc") {
        return true;
    }
    matches!(zone_named(n), Zone::Named(..))
}

/// The local midnight (civil seconds) of the day an all-day time falls on. A time that is a midnight is that
/// day as written; another one (the service gives all-day times in the zone the request asked for, so an
/// event made in Berlin comes as 22:00 UTC the evening before) is the instant `unix` seen in `zone`, the
/// zone the event was made in (the user's offset when MailCraft does not know that zone), to the nearest
/// midnight.
pub(super) fn all_day_local(unix: i64, zone: &Zone, user_offset: i32) -> i64 {
    if unix.rem_euclid(DAY) == 0 {
        return unix;
    }
    let seen = match zone {
        Zone::Named(..) => zone.to_local(unix),
        Zone::Utc => unix.saturating_add(i64::from(user_offset) * 60),
    };
    seen.saturating_add(DAY / 2).div_euclid(DAY) * DAY
}

/// The local midnight of an all-day `dateTimeTimeZone` (see [`all_day_local`]).
fn all_day_of(v: Option<&Value>, zone: &Zone, user_offset: i32) -> Option<i64> {
    let v = v?;
    let local = parse_local(v.get("dateTime")?.as_str()?)?;
    if local.rem_euclid(DAY) == 0 {
        return Some(local);
    }
    let (unix, _) = zone_named(v.get("timeZone").and_then(Value::as_str).unwrap_or("UTC")).to_unix(local);
    Some(all_day_local(unix, zone, user_offset))
}

impl Zone {
    fn to_unix(&self, local: i64) -> (i64, i32) {
        match self {
            Zone::Named(z, _) => {
                let unix = z.to_utc(local);
                (unix, z.offset_at_utc(unix))
            }
            Zone::Utc => (local, 0),
        }
    }

    fn to_local(&self, unix: i64) -> i64 {
        match self {
            Zone::Named(z, _) => unix.saturating_add(i64::from(z.offset_at_utc(unix)) * 60),
            Zone::Utc => unix,
        }
    }

    /// The name written for this zone: its Windows name when it has one, else its IANA name.
    fn graph_name(&self) -> String {
        match self {
            Zone::Named(_, iana) => windows_of_iana(iana).unwrap_or(iana).to_owned(),
            Zone::Utc => "UTC".to_owned(),
        }
    }

    fn iana(&self) -> Option<String> {
        match self {
            Zone::Named(_, iana) => Some((*iana).to_owned()),
            Zone::Utc => None,
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.fffffff][Z]` → civil seconds.
pub(super) fn parse_local(s: &str) -> Option<i64> {
    let s = s.trim();
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let small = |r: std::ops::Range<usize>| n(r).and_then(|v| u8::try_from(v).ok());
    if s.as_bytes().get(4) != Some(&b'-') {
        return None;
    }
    let (h, mi, se) = if s.as_bytes().get(10) == Some(&b'T') {
        (small(11..13)?, small(14..16).unwrap_or(0), small(17..19).unwrap_or(0))
    } else {
        (0, 0, 0)
    };
    local_secs(n(0..4)?, small(5..7)?, small(8..10)?, h, mi, se)
}

pub(super) fn format_local(secs: i64) -> String {
    let (y, m, d, h, mi, s) = civil(secs);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}

fn format_day(secs: i64) -> String {
    let (y, m, d, _, _, _) = civil(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A `dateTimeTimeZone` as Unix time (with the offset in force).
fn dtz(v: Option<&Value>) -> Option<(i64, i32)> {
    let v = v?;
    let local = parse_local(v.get("dateTime")?.as_str()?)?;
    Some(zone_named(v.get("timeZone").and_then(Value::as_str).unwrap_or("UTC")).to_unix(local))
}

/// An ISO 8601 date-time with an offset or `Z` (`originalStart`) as Unix time.
pub(super) fn parse_instant(s: &str) -> Option<i64> {
    let s = s.trim();
    let local = parse_local(s)?;
    let tail = s.get(19..).unwrap_or("");
    let tail = tail.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if tail.is_empty() || tail.eq_ignore_ascii_case("z") {
        return Some(local);
    }
    let sign = match tail.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return Some(local),
    };
    let h: i64 = tail.get(1..3)?.parse().ok()?;
    let m: i64 = tail.get(4..6).and_then(|x| x.parse().ok()).unwrap_or(0);
    Some(local.saturating_sub(sign * (h * 3600 + m * 60)))
}

fn utc_text(unix: i64) -> String {
    format!("{}Z", format_local(unix))
}

// ---------------------------------------------------------------------------------------------------------
// Recurrence pattern ↔ RRULE
// ---------------------------------------------------------------------------------------------------------

const DAYS: [(&str, &str); 7] = [
    ("sunday", "SU"),
    ("monday", "MO"),
    ("tuesday", "TU"),
    ("wednesday", "WE"),
    ("thursday", "TH"),
    ("friday", "FR"),
    ("saturday", "SA"),
];

fn ical_day(graph: &str) -> Option<&'static str> {
    DAYS.iter().find(|(g, _)| g.eq_ignore_ascii_case(graph)).map(|(_, i)| *i)
}

fn graph_day(ical: &str) -> Option<&'static str> {
    DAYS.iter().find(|(_, i)| i.eq_ignore_ascii_case(ical)).map(|(g, _)| *g)
}

const INDEX: [(&str, i32); 5] = [("first", 1), ("second", 2), ("third", 3), ("fourth", 4), ("last", -1)];

/// A Graph `patternedRecurrence` as an RRULE value; `until` turns the range's end date (a local day) into
/// the RRULE's UNTIL.
fn rrule_of(rec: &Value, until: &dyn Fn(i64) -> String) -> Option<String> {
    let p = rec.get("pattern")?;
    let r = rec.get("range");
    let interval = p.get("interval").and_then(Value::as_u64).unwrap_or(1).clamp(1, 1000);
    let days: Vec<&str> = p
        .get("daysOfWeek")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).filter_map(ical_day).take(7).collect())
        .unwrap_or_default();
    let month = p.get("month").and_then(Value::as_u64).filter(|m| (1..=12).contains(m));
    let dom = p.get("dayOfMonth").and_then(Value::as_u64).filter(|d| (1..=31).contains(d));
    let index = p
        .get("index")
        .and_then(Value::as_str)
        .and_then(|i| INDEX.iter().find(|(n, _)| n.eq_ignore_ascii_case(i)))
        .map_or(1, |(_, v)| *v);
    let mut parts: Vec<String> = Vec::new();
    let nth_days = |parts: &mut Vec<String>| {
        if days.len() == 1 {
            parts.push(format!("BYDAY={index}{}", days.first().copied().unwrap_or("MO")));
        } else if !days.is_empty() {
            parts.push(format!("BYDAY={}", days.join(",")));
            parts.push(format!("BYSETPOS={index}"));
        }
    };
    match p.get("type").and_then(Value::as_str)? {
        "daily" => parts.push("FREQ=DAILY".into()),
        "weekly" => {
            parts.push("FREQ=WEEKLY".into());
            if !days.is_empty() {
                parts.push(format!("BYDAY={}", days.join(",")));
            }
            if interval > 1 {
                let wkst = p.get("firstDayOfWeek").and_then(Value::as_str).and_then(ical_day).unwrap_or("SU");
                parts.push(format!("WKST={wkst}"));
            }
        }
        "absoluteMonthly" => {
            parts.push("FREQ=MONTHLY".into());
            if let Some(d) = dom {
                parts.push(format!("BYMONTHDAY={d}"));
            }
        }
        "relativeMonthly" => {
            parts.push("FREQ=MONTHLY".into());
            nth_days(&mut parts);
        }
        "absoluteYearly" => {
            parts.push("FREQ=YEARLY".into());
            if let Some(m) = month {
                parts.push(format!("BYMONTH={m}"));
            }
            if let Some(d) = dom {
                parts.push(format!("BYMONTHDAY={d}"));
            }
        }
        "relativeYearly" => {
            parts.push("FREQ=YEARLY".into());
            if let Some(m) = month {
                parts.push(format!("BYMONTH={m}"));
            }
            nth_days(&mut parts);
        }
        _ => return None,
    }
    if interval > 1 {
        parts.insert(1, format!("INTERVAL={interval}"));
    }
    match r.and_then(|r| r.get("type")).and_then(Value::as_str) {
        Some("numbered") => {
            let n = r.and_then(|r| r.get("numberOfOccurrences")).and_then(Value::as_u64).unwrap_or(1);
            parts.push(format!("COUNT={}", n.clamp(1, 10_000)));
        }
        Some("endDate") => {
            if let Some(day) = r.and_then(|r| r.get("endDate")).and_then(Value::as_str).and_then(parse_local)
            {
                parts.push(format!("UNTIL={}", until(day.div_euclid(DAY) * DAY)));
            }
        }
        _ => {}
    }
    Some(parts.join(";"))
}

/// An RRULE value as a Graph `patternedRecurrence` starting on local day `start_local` (seconds); `until_day`
/// turns an UNTIL value into the local day it ends on. `None` for a rule Graph cannot hold.
fn pattern_of(
    rrule: &str,
    start_local: i64,
    zone_name: Option<&str>,
    until_day: &dyn Fn(&str) -> Option<i64>,
) -> Option<Value> {
    let mut freq = "";
    let mut interval = 1u64;
    let mut byday: Vec<String> = Vec::new();
    let mut bymonthday: Option<u64> = None;
    let mut bymonth: Option<u64> = None;
    let mut setpos: Option<i32> = None;
    let mut count: Option<u64> = None;
    let mut until: Option<i64> = None;
    let mut wkst = "sunday";
    for part in rrule.split(';').take(32) {
        let Some((k, v)) = part.split_once('=') else { continue };
        let v = v.trim();
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = if v.eq_ignore_ascii_case("daily") {
                    "daily"
                } else if v.eq_ignore_ascii_case("weekly") {
                    "weekly"
                } else if v.eq_ignore_ascii_case("monthly") {
                    "monthly"
                } else if v.eq_ignore_ascii_case("yearly") {
                    "yearly"
                } else {
                    ""
                }
            }
            "INTERVAL" => interval = v.parse().unwrap_or(1),
            "BYDAY" => {
                byday = v
                    .split(',')
                    .map(|d| d.trim().to_ascii_uppercase())
                    .filter(|d| !d.is_empty())
                    .take(7)
                    .collect()
            }
            "BYMONTHDAY" => bymonthday = v.split(',').next().and_then(|d| d.trim().parse().ok()),
            "BYMONTH" => bymonth = v.split(',').next().and_then(|d| d.trim().parse().ok()),
            "BYSETPOS" => setpos = v.split(',').next().and_then(|d| d.trim().parse().ok()),
            "COUNT" => count = v.parse().ok(),
            "UNTIL" => until = until_day(v),
            "WKST" => wkst = graph_day(v).unwrap_or("sunday"),
            _ => {}
        }
    }
    let (_, sm, sd, _, _, _) = civil(start_local);
    let start_day = start_local.div_euclid(DAY);
    let weekday = (start_day + 4).rem_euclid(7);
    let start_weekday = DAYS.get(usize::try_from(weekday).unwrap_or(0)).map_or("monday", |(g, _)| *g);
    // `2TU` / `-1FR` → (index, day).
    let split = |d: &str| -> Option<(i32, &'static str)> {
        let cut = d.len().checked_sub(2)?;
        let n = d.get(..cut)?.trim_start_matches('+');
        let day = graph_day(d.get(cut..)?)?;
        Some((if n.is_empty() { 0 } else { n.parse().ok()? }, day))
    };
    let parsed: Vec<(i32, &str)> = byday.iter().filter_map(|d| split(d)).collect();
    let index_name = |n: i32| INDEX.iter().find(|(_, v)| *v == n).map(|(k, _)| *k);
    let mut p = Map::new();
    p.insert("interval".into(), json!(interval.clamp(1, 1000)));
    match freq {
        "daily" => {
            p.insert("type".into(), json!("daily"));
        }
        "weekly" => {
            let days: Vec<&str> = if parsed.is_empty() {
                vec![start_weekday]
            } else {
                parsed.iter().map(|(_, d)| *d).collect()
            };
            p.insert("type".into(), json!("weekly"));
            p.insert("daysOfWeek".into(), json!(days));
            p.insert("firstDayOfWeek".into(), json!(wkst));
        }
        "monthly" | "yearly" => {
            let nth = parsed
                .iter()
                .find(|(n, _)| *n != 0)
                .map(|(n, _)| *n)
                .or(setpos.filter(|_| !parsed.is_empty()));
            match nth.and_then(index_name) {
                Some(index) => {
                    let days: Vec<&str> = parsed.iter().map(|(_, d)| *d).collect();
                    p.insert(
                        "type".into(),
                        json!(if freq == "monthly" { "relativeMonthly" } else { "relativeYearly" }),
                    );
                    p.insert("daysOfWeek".into(), json!(days));
                    p.insert("index".into(), json!(index));
                }
                None if !parsed.is_empty() => return None,
                None => {
                    p.insert(
                        "type".into(),
                        json!(if freq == "monthly" { "absoluteMonthly" } else { "absoluteYearly" }),
                    );
                    p.insert("dayOfMonth".into(), json!(bymonthday.unwrap_or(u64::from(sd)).clamp(1, 31)));
                }
            }
            if freq == "yearly" {
                p.insert("month".into(), json!(bymonth.unwrap_or(u64::from(sm)).clamp(1, 12)));
            }
        }
        _ => return None,
    }
    let mut range = Map::new();
    range.insert("startDate".into(), json!(format_day(start_local)));
    match (count, until) {
        (Some(n), _) => {
            range.insert("type".into(), json!("numbered"));
            range.insert("numberOfOccurrences".into(), json!(n.clamp(1, 10_000)));
        }
        (None, Some(day)) => {
            range.insert("type".into(), json!("endDate"));
            range.insert("endDate".into(), json!(format_day(day)));
        }
        (None, None) => {
            range.insert("type".into(), json!("noEnd"));
        }
    }
    if let Some(z) = zone_name {
        range.insert("recurrenceTimeZone".into(), json!(z));
    }
    Some(json!({"pattern": Value::Object(p), "range": Value::Object(range)}))
}

/// The RRULE of a Graph recurrence for an event starting in `zone` (an all-day one in the user's offset).
fn event_rrule(rec: &Value, zone: &Zone, all_day: bool) -> Option<String> {
    let until = |day: i64| -> String {
        if all_day {
            format_day(day).replace('-', "")
        } else {
            // The end of that day in the event's zone, in UTC.
            let (unix, _) = zone.to_unix(day.saturating_add(DAY - 1));
            utc_text(unix).replace(['-', ':'], "")
        }
    };
    rrule_of(rec, &until)
}

/// The local day (seconds at midnight) an RRULE UNTIL value ends on, in `zone`.
fn until_day(v: &str, zone: &Zone) -> Option<i64> {
    let d: String = v.chars().filter(char::is_ascii_alphanumeric).collect();
    let n = |r: std::ops::Range<usize>| d.get(r)?.parse::<i64>().ok();
    let small = |r: std::ops::Range<usize>| n(r).and_then(|x| u8::try_from(x).ok());
    let (h, mi, s) = if d.len() >= 15 { (small(9..11)?, small(11..13)?, small(13..15)?) } else { (0, 0, 0) };
    let secs = local_secs(n(0..4)?, small(4..6)?, small(6..8)?, h, mi, s)?;
    let local = if d.ends_with('Z') { zone.to_local(secs) } else { secs };
    Some(local.div_euclid(DAY) * DAY)
}

// ---------------------------------------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------------------------------------

/// The task properties MailCraft writes.
pub(super) const TASK_MAPPED: [&str; 11] = [
    "title",
    "body",
    "importance",
    "status",
    "startDateTime",
    "dueDateTime",
    "completedDateTime",
    "isReminderOn",
    "reminderDateTime",
    "categories",
    "recurrence",
];

/// A task day (`dateTimeTimeZone`) as local midnight in the user's offset: a midnight as written, another
/// time (a day the service gives in the zone the request asked for) the day it is in the user's offset.
fn task_day(v: Option<&Value>, user_offset: i32) -> Option<DateTime> {
    let day = all_day_of(v, &Zone::Utc, user_offset)?;
    Some(DateTime::from_unix(day.saturating_sub(i64::from(user_offset) * 60), user_offset))
}

fn day_json(t: Option<DateTime>) -> Value {
    match t {
        Some(t) => {
            let local = t.unix.saturating_add(i64::from(t.offset_min) * 60);
            json!({"dateTime": format!("{}T00:00:00", format_day(local)), "timeZone": "UTC"})
        }
        None => Value::Null,
    }
}

fn instant_json(t: Option<DateTime>) -> Value {
    match t {
        Some(t) => json!({"dateTime": format_local(t.unix), "timeZone": "UTC"}),
        None => Value::Null,
    }
}

/// The task a To Do task describes.
pub(super) fn to_task(v: &Value, user_offset: i32) -> Task {
    let status = match v.get("status").and_then(Value::as_str) {
        Some("inProgress") => TaskStatus::InProgress,
        Some("completed") => TaskStatus::Completed,
        Some("waitingOnOthers") => TaskStatus::WaitingOnSomeoneElse,
        Some("deferred") => TaskStatus::Deferred,
        _ => TaskStatus::NotStarted,
    };
    let priority = match v.get("importance").and_then(Value::as_str) {
        Some("high") => TaskPriority::High,
        Some("low") => TaskPriority::Low,
        _ => TaskPriority::Normal,
    };
    let due = task_day(v.get("dueDateTime"), user_offset);
    let reminder = if v.get("isReminderOn").and_then(Value::as_bool) == Some(true) {
        dtz(v.get("reminderDateTime")).map(|(u, o)| DateTime::from_unix(u, o))
    } else {
        None
    };
    let completed = if status == TaskStatus::Completed {
        dtz(v.get("completedDateTime")).map(|(u, _)| DateTime::from_unix(u, user_offset))
    } else {
        None
    };
    let recurrence = v.get("recurrence").filter(|r| r.is_object()).and_then(|r| {
        let until = |day: i64| format_day(day).replace('-', "");
        rrule_of(r, &until)
    });
    Task {
        subject: text(v.get("title")),
        body: body_text(v.get("body")),
        status,
        priority,
        percent_complete: if status == TaskStatus::Completed { 100 } else { 0 },
        start: task_day(v.get("startDateTime"), user_offset),
        due,
        completed,
        reminder,
        categories: strings(v.get("categories")),
        recurrence: recurrence.map(|rrule| TaskRecurrence { rrule, start: due, ..Default::default() }),
        ..Default::default()
    }
}

/// The To Do properties for `t` (every [`TASK_MAPPED`] one).
pub(super) fn from_task(t: &Task) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("title".into(), json!(t.subject));
    m.insert("body".into(), body_of(&t.body));
    m.insert(
        "importance".into(),
        json!(match t.priority {
            TaskPriority::High => "high",
            TaskPriority::Low => "low",
            TaskPriority::Normal => "normal",
        }),
    );
    m.insert(
        "status".into(),
        json!(match t.status {
            TaskStatus::NotStarted => "notStarted",
            TaskStatus::InProgress => "inProgress",
            TaskStatus::Completed => "completed",
            TaskStatus::WaitingOnSomeoneElse => "waitingOnOthers",
            TaskStatus::Deferred => "deferred",
        }),
    );
    m.insert("startDateTime".into(), day_json(t.start));
    m.insert("dueDateTime".into(), day_json(t.due));
    m.insert(
        "completedDateTime".into(),
        if t.status == TaskStatus::Completed { instant_json(t.completed) } else { Value::Null },
    );
    m.insert("isReminderOn".into(), json!(t.reminder.is_some()));
    m.insert("reminderDateTime".into(), instant_json(t.reminder));
    m.insert("categories".into(), json!(t.categories));
    let rec =
        t.recurrence.as_ref().filter(|r| !r.rrule.trim().is_empty() && r.regenerate_days == 0).and_then(
            |r| {
                let anchor = r.start.or(t.due)?;
                let local = anchor.unix.saturating_add(i64::from(anchor.offset_min) * 60);
                pattern_of(&r.rrule, local, None, &|v| until_day(v, &Zone::Utc))
            },
        );
    m.insert("recurrence".into(), rec.unwrap_or(Value::Null));
    m
}

// ---------------------------------------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------------------------------------

/// The event properties MailCraft writes.
pub(super) const EVENT_MAPPED: [&str; 14] = [
    "subject",
    "body",
    "location",
    "start",
    "end",
    "isAllDay",
    "showAs",
    "isReminderOn",
    "reminderMinutesBeforeStart",
    "attendees",
    "categories",
    "sensitivity",
    "importance",
    "recurrence",
];

fn mailbox_of(v: Option<&Value>) -> Option<Mailbox> {
    let e = v?.get("emailAddress")?;
    let addr: String = text(e.get("address")).trim().chars().take(320).collect();
    if addr.is_empty() {
        return None;
    }
    let name = text(e.get("name"));
    Some(Mailbox { name: (!name.trim().is_empty() && name != addr).then_some(name), addr })
}

/// The zone an event was made in (`originalStartTimeZone`, else its start's zone).
fn event_zone(ev: &Value, key: &str, fallback: &str) -> Zone {
    let own = ev.get(key).and_then(Value::as_str).filter(|z| !z.trim().is_empty());
    let given = ev.get(fallback).and_then(|s| s.get("timeZone")).and_then(Value::as_str);
    zone_named(own.or(given).unwrap_or("UTC"))
}

/// One appointment from one Graph event (a series, a single event or an exception), without recurrence.
pub(super) fn appointment_of(ev: &Value, user_offset: i32) -> Option<Appointment> {
    let all_day = ev.get("isAllDay").and_then(Value::as_bool) == Some(true);
    let (start, end, tz, end_tz) = if all_day {
        // An all-day event is a run of days, in the user's zone: the days it has in the zone it was made in.
        let zone = event_zone(ev, "originalStartTimeZone", "start");
        let end_zone = event_zone(ev, "originalEndTimeZone", "end");
        let s = all_day_of(ev.get("start"), &zone, user_offset)?;
        let e = all_day_of(ev.get("end"), &end_zone, user_offset)
            .unwrap_or(s + DAY)
            .clamp(s + DAY, s.saturating_add(MAX_DURATION));
        let off = i64::from(user_offset) * 60;
        (
            DateTime::from_unix(s.saturating_sub(off), user_offset),
            DateTime::from_unix(e.saturating_sub(off), user_offset),
            None,
            None,
        )
    } else {
        let (s, so) = dtz(ev.get("start"))?;
        let (e, eo) = dtz(ev.get("end")).unwrap_or((s, so));
        let e = e.clamp(s, s.saturating_add(MAX_DURATION));
        let zone = event_zone(ev, "originalStartTimeZone", "start");
        let end_zone = event_zone(ev, "originalEndTimeZone", "end");
        let off = |z: &Zone, t: i64, fallback: i32| match z {
            Zone::Named(tz, _) => tz.offset_at_utc(t),
            Zone::Utc => fallback,
        };
        let tz = zone.iana();
        let end_tz = end_zone.iana().filter(|e| Some(e) != tz.as_ref());
        (DateTime::from_unix(s, off(&zone, s, so)), DateTime::from_unix(e, off(&end_zone, e, eo)), tz, end_tz)
    };
    let show_as = match ev.get("showAs").and_then(Value::as_str) {
        Some("free") => BusyStatus::Free,
        Some("tentative") => BusyStatus::Tentative,
        Some("oof") => BusyStatus::OutOfOffice,
        Some("workingElsewhere") => BusyStatus::WorkingElsewhere,
        _ => BusyStatus::Busy,
    };
    let reminder_min = (ev.get("isReminderOn").and_then(Value::as_bool) == Some(true))
        .then(|| ev.get("reminderMinutesBeforeStart").and_then(Value::as_u64).unwrap_or(15))
        .and_then(|m| u32::try_from(m.min(60 * 24 * 7 * 52)).ok());
    let organizer = mailbox_of(ev.get("organizer"));
    let mut attendees = Vec::new();
    for a in ev.get("attendees").and_then(Value::as_array).into_iter().flatten().take(MAX_LIST) {
        let Some(mailbox) = mailbox_of(Some(a)) else { continue };
        if organizer.as_ref().is_some_and(|o| o.addr.eq_ignore_ascii_case(&mailbox.addr)) {
            continue;
        }
        let role = match a.get("type").and_then(Value::as_str) {
            Some("optional") => AttendeeRole::Optional,
            Some("resource") => AttendeeRole::Resource,
            _ => AttendeeRole::Required,
        };
        let response = match a.get("status").and_then(|s| s.get("response")).and_then(Value::as_str) {
            Some("accepted") => ResponseStatus::Accepted,
            Some("tentativelyAccepted") => ResponseStatus::Tentative,
            Some("declined") => ResponseStatus::Declined,
            _ => ResponseStatus::None,
        };
        attendees.push(Attendee { mailbox, role, response });
    }
    let importance = match ev.get("importance").and_then(Value::as_str) {
        Some("high") => Importance::High,
        Some("low") => Importance::Low,
        _ => Importance::Normal,
    };
    let is_meeting = !attendees.is_empty();
    Some(Appointment {
        uid: text(ev.get("iCalUId")).chars().take(500).collect(),
        subject: text(ev.get("subject")),
        body: body_text(ev.get("body")),
        location: ev.get("location").map(|l| text(l.get("displayName"))).unwrap_or_default(),
        start,
        end,
        all_day,
        show_as,
        reminder_min,
        organizer: if is_meeting { organizer } else { None },
        attendees,
        categories: strings(ev.get("categories")),
        private: matches!(ev.get("sensitivity").and_then(Value::as_str), Some("private" | "confidential")),
        is_meeting,
        importance,
        tz,
        end_tz,
        ..Default::default()
    })
}

/// A Graph series (or single event) with its exceptions and cancelled occurrences (original starts, Unix)
/// as a series and its exceptions here, sharing the event's iCalendar UID; and that UID.
pub(super) fn to_appointments(
    master: &Value,
    exceptions: &[Value],
    cancelled: &[i64],
    user_offset: i32,
) -> Option<(Vec<Appointment>, String)> {
    let mut series = appointment_of(master, user_offset)?;
    if series.uid.trim().is_empty() {
        series.uid = format!("graph-{}", text(master.get("id")).chars().take(300).collect::<String>());
    }
    let uid = series.uid.clone();
    let zone = event_zone(master, "originalStartTimeZone", "start");
    let rid_of = |t: i64| -> DateTime {
        if series.all_day {
            let day = all_day_local(t, &zone, user_offset);
            DateTime::from_unix(day.saturating_sub(i64::from(user_offset) * 60), user_offset)
        } else {
            let off = match &zone {
                Zone::Named(z, _) => z.offset_at_utc(t),
                Zone::Utc => 0,
            };
            DateTime::from_unix(t, off)
        }
    };
    let mut out = Vec::new();
    if let Some(rule) =
        master.get("recurrence").filter(|r| r.is_object()).and_then(|r| event_rrule(r, &zone, series.all_day))
    {
        let mut exdates: Vec<DateTime> = cancelled.iter().map(|t| rid_of(*t)).collect();
        exdates.sort_by_key(|d| d.unix);
        exdates.dedup_by_key(|d| d.unix);
        series.recurrence = Some(Recurrence { rrule: rule, exdates });
        for x in exceptions.iter().take(super::pim::MAX_GROUP) {
            let Some(orig) = x.get("originalStart").and_then(Value::as_str).and_then(parse_instant) else {
                continue;
            };
            let Some(mut a) = appointment_of(x, user_offset) else { continue };
            a.uid.clone_from(&uid);
            a.recurrence_id = Some(rid_of(orig));
            out.push(a);
        }
    }
    out.insert(0, series);
    Some((out, uid))
}

/// The Graph zone of an appointment: its own zone when MailCraft knows it, else UTC.
fn zone_for(zone: Option<&str>) -> Zone {
    zone.map_or(Zone::Utc, zone_named)
}

/// The scheduling properties of one appointment (every [`EVENT_MAPPED`] one; `recurrence` only for a series).
pub(super) fn event_props(a: &Appointment) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("subject".into(), json!(a.subject));
    m.insert("body".into(), body_of(&a.body));
    m.insert("location".into(), json!({"displayName": a.location}));
    let end = a.end.unix.max(a.start.unix);
    let zone = zone_for(a.zone());
    if a.all_day {
        // Midnights in the appointment's zone (the mailbox's, see `graph_pim.rs`), so other clients show the
        // same days wherever that zone is.
        let local = a.start.unix.saturating_add(i64::from(a.start.offset_min) * 60).div_euclid(DAY) * DAY;
        let days = ((end - a.start.unix + DAY - 1) / DAY).max(1);
        let name = zone.graph_name();
        m.insert("start".into(), json!({"dateTime": format_local(local), "timeZone": name}));
        m.insert("end".into(), json!({"dateTime": format_local(local + days * DAY), "timeZone": name}));
        m.insert("isAllDay".into(), json!(true));
    } else {
        let end_zone = zone_for(a.end_zone());
        m.insert(
            "start".into(),
            json!({"dateTime": format_local(zone.to_local(a.start.unix)), "timeZone": zone.graph_name()}),
        );
        m.insert(
            "end".into(),
            json!({"dateTime": format_local(end_zone.to_local(end)), "timeZone": end_zone.graph_name()}),
        );
        m.insert("isAllDay".into(), json!(false));
    }
    m.insert(
        "showAs".into(),
        json!(match a.show_as {
            BusyStatus::Free => "free",
            BusyStatus::WorkingElsewhere => "workingElsewhere",
            BusyStatus::Tentative => "tentative",
            BusyStatus::Busy => "busy",
            BusyStatus::OutOfOffice => "oof",
        }),
    );
    m.insert("isReminderOn".into(), json!(a.reminder_min.is_some()));
    m.insert("reminderMinutesBeforeStart".into(), json!(a.reminder_min.unwrap_or(0)));
    let attendees: Vec<Value> = a
        .attendees
        .iter()
        .take(MAX_LIST)
        .map(|x| {
            json!({"emailAddress": {"address": x.mailbox.addr, "name": x.mailbox.name.clone().unwrap_or_else(|| x.mailbox.addr.clone())},
                   "type": match x.role {
                       AttendeeRole::Required => "required",
                       AttendeeRole::Optional => "optional",
                       AttendeeRole::Resource => "resource",
                   }})
        })
        .collect();
    m.insert("attendees".into(), Value::Array(attendees));
    m.insert("categories".into(), json!(a.categories));
    m.insert("sensitivity".into(), json!(if a.private { "private" } else { "normal" }));
    m.insert(
        "importance".into(),
        json!(match a.importance {
            Importance::High => "high",
            Importance::Low => "low",
            Importance::Normal => "normal",
        }),
    );
    let rec =
        a.recurrence.as_ref().filter(|r| !r.rrule.trim().is_empty() && a.recurrence_id.is_none()).and_then(
            |r| {
                let local = if a.all_day {
                    a.start.unix.saturating_add(i64::from(a.start.offset_min) * 60)
                } else {
                    zone.to_local(a.start.unix)
                };
                let name = if a.all_day { None } else { Some(zone.graph_name()) };
                let rule_zone = if a.all_day { Zone::Utc } else { zone_for(a.zone()) };
                pattern_of(&r.rrule, local, name.as_deref(), &|v| until_day(v, &rule_zone))
            },
        );
    m.insert("recurrence".into(), rec.unwrap_or(Value::Null));
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contacts_round_trip_through_graph_properties() {
        let c = Contact {
            first_name: "Pat".into(),
            last_name: "Lee".into(),
            company: "Example Corp".into(),
            job_title: "Engineer".into(),
            emails: vec!["pat@example.com".into(), "pat.lee@example.org".into()],
            phones: vec![("Business".into(), "+1 555 0100".into()), ("Mobile".into(), "+1 555 0101".into())],
            addresses: vec![PostalAddress {
                kind: "Home".into(),
                street: "1 Main St".into(),
                city: "Springfield".into(),
                region: "ON".into(),
                postal_code: "K1A 0B1".into(),
                country: "Canada".into(),
            }],
            birthday: Some("1990-05-01".into()),
            notes: "Met at the conference".into(),
            categories: vec!["Work".into()],
            nickname: "P".into(),
            ..Default::default()
        };
        let m = from_contact(&c);
        assert_eq!(m.len(), CONTACT_MAPPED.len());
        let back = to_contact(&Value::Object(m.clone()));
        assert_eq!(back.first_name, "Pat");
        assert_eq!(back.emails, c.emails);
        assert_eq!(back.phones, c.phones);
        assert_eq!(back.addresses, c.addresses);
        assert_eq!(back.birthday.as_deref(), Some("1990-05-01"));
        assert_eq!(back.notes, c.notes);
        assert_eq!(from_contact(&back), m, "read back the same way, nothing differs");
        // Hostile input: wrong types, a broken birthday, huge lists.
        let junk = json!({"givenName": 5, "birthday": "x", "emailAddresses": [{"address": 1}, {"address": "a@example.com"}],
                          "businessPhones": (0..500).map(|i| i.to_string()).collect::<Vec<_>>(), "displayName": "Only Name"});
        let j = to_contact(&junk);
        assert_eq!(j.first_name, "Only Name");
        assert_eq!(j.birthday, None);
        assert_eq!(j.emails, vec!["a@example.com".to_owned()]);
        assert!(j.phones.len() <= MAX_LIST);
    }

    #[test]
    fn tasks_round_trip_through_todo_properties() {
        let t = Task {
            subject: "File the report".into(),
            body: "Before Friday".into(),
            priority: TaskPriority::High,
            status: TaskStatus::InProgress,
            due: Some(DateTime::from_unix(1_791_331_200 + 240 * 60, -240)),
            reminder: Some(DateTime::from_unix(1_791_300_000, 0)),
            categories: vec!["Work".into()],
            recurrence: Some(TaskRecurrence {
                rrule: "FREQ=WEEKLY;BYDAY=FR".into(),
                start: Some(DateTime::from_unix(1_791_331_200 + 240 * 60, -240)),
                ..Default::default()
            }),
            ..Default::default()
        };
        let m = from_task(&t);
        let back = to_task(&Value::Object(m.clone()), -240);
        assert_eq!(back.subject, t.subject);
        assert_eq!(back.priority, TaskPriority::High);
        assert_eq!(back.status, TaskStatus::InProgress);
        assert_eq!(back.due.map(|d| d.unix), t.due.map(|d| d.unix));
        assert_eq!(back.reminder.map(|d| d.unix), t.reminder.map(|d| d.unix));
        assert_eq!(back.recurrence.as_ref().map(|r| r.rrule.as_str()), Some("FREQ=WEEKLY;BYDAY=FR"));
        assert_eq!(from_task(&back), m);
        let done = to_task(
            &json!({"title": "x", "status": "completed",
                                   "completedDateTime": {"dateTime": "2026-10-01T10:00:00.0000000", "timeZone": "UTC"}}),
            0,
        );
        assert_eq!(
            (done.percent_complete, done.completed.map(|c| c.unix)),
            (100, parse_local("2026-10-01T10:00:00"))
        );
    }

    #[test]
    fn all_day_days_are_read_in_the_zone_they_were_made_in() {
        // Made in Berlin, given in UTC: 22:00 the evening before is 10 May there, for a user anywhere.
        let berlin = json!({"isAllDay": true, "originalStartTimeZone": "W. Europe Standard Time",
                            "originalEndTimeZone": "W. Europe Standard Time",
                            "start": {"dateTime": "2027-05-09T22:00:00.0000000", "timeZone": "UTC"},
                            "end": {"dateTime": "2027-05-10T22:00:00.0000000", "timeZone": "UTC"}});
        for offset in [-480, 0, 120, 540] {
            let a = appointment_of(&berlin, offset).unwrap();
            let local = a.start.unix + i64::from(a.start.offset_min) * 60;
            assert_eq!(local, parse_local("2027-05-10T00:00:00").unwrap(), "{offset}");
            assert_eq!(a.end.unix - a.start.unix, DAY);
        }
        // Made in Los Angeles: 07:00 UTC is the same day there.
        let la = json!({"isAllDay": true, "originalStartTimeZone": "Pacific Standard Time",
                        "start": {"dateTime": "2027-05-12T07:00:00", "timeZone": "UTC"},
                        "end": {"dateTime": "2027-05-13T07:00:00", "timeZone": "UTC"}});
        let a = appointment_of(&la, 120).unwrap();
        assert_eq!(a.start.unix + 120 * 60, parse_local("2027-05-12T00:00:00").unwrap());
        // A zone MailCraft does not know: to the nearest midnight in the user's offset.
        let custom = json!({"isAllDay": true, "originalStartTimeZone": "tzone://Microsoft/Custom",
                            "start": {"dateTime": "2027-05-09T22:00:00", "timeZone": "UTC"}});
        let a = appointment_of(&custom, 60).unwrap();
        assert_eq!(a.start.unix + 60 * 60, parse_local("2027-05-10T00:00:00").unwrap());
        // Midnights are the day as written.
        let plain =
            json!({"isAllDay": true, "start": {"dateTime": "2027-05-10T00:00:00", "timeZone": "UTC"}});
        let a = appointment_of(&plain, -300).unwrap();
        assert_eq!(a.start.unix - 300 * 60, parse_local("2027-05-10T00:00:00").unwrap());
        // Written in the appointment's zone (the mailbox's), at its midnights.
        let mut here = a.clone();
        here.tz = Some("Europe/Berlin".into());
        let p = event_props(&here);
        assert_eq!(
            p["start"],
            json!({"dateTime": "2027-05-10T00:00:00", "timeZone": "W. Europe Standard Time"})
        );
        assert_eq!(p["end"]["dateTime"], "2027-05-11T00:00:00");
        // To Do days given in UTC: the day in the user's offset.
        let due = json!({"title": "x", "dueDateTime": {"dateTime": "2027-05-09T22:00:00.0000000", "timeZone": "UTC"}});
        let t = to_task(&due, 120);
        assert_eq!(t.due.map(|d| d.unix + 120 * 60), parse_local("2027-05-10T00:00:00"));
        let midnight =
            json!({"title": "x", "dueDateTime": {"dateTime": "2027-05-10T00:00:00", "timeZone": "UTC"}});
        assert_eq!(
            to_task(&midnight, -300).due.map(|d| d.unix - 300 * 60),
            parse_local("2027-05-10T00:00:00")
        );
    }

    #[test]
    fn events_keep_zones_series_and_exceptions() {
        let master = json!({
            "id": "AAev1", "iCalUId": "uid-1", "subject": "Standup", "type": "seriesMaster",
            "start": {"dateTime": "2026-10-05T13:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-10-05T13:30:00.0000000", "timeZone": "UTC"},
            "originalStartTimeZone": "Eastern Standard Time", "originalEndTimeZone": "Eastern Standard Time",
            "isReminderOn": true, "reminderMinutesBeforeStart": 10, "showAs": "busy",
            "organizer": {"emailAddress": {"name": "Alice", "address": "alice@example.com"}},
            "attendees": [{"type": "required", "status": {"response": "accepted"},
                           "emailAddress": {"name": "Bob", "address": "bob@example.org"}}],
            "recurrence": {"pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["monday"], "firstDayOfWeek": "sunday"},
                           "range": {"type": "endDate", "startDate": "2026-10-05", "endDate": "2026-12-28"}}
        });
        let exception = json!({
            "id": "AAev2", "type": "exception", "seriesMasterId": "AAev1", "subject": "Standup (moved)",
            "originalStart": "2026-10-12T13:00:00Z",
            "start": {"dateTime": "2026-10-12T15:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-10-12T15:30:00.0000000", "timeZone": "UTC"},
            "originalStartTimeZone": "Eastern Standard Time"
        });
        let cancelled = [parse_instant("2026-10-19T13:00:00Z").unwrap()];
        let (items, uid) = to_appointments(&master, &[exception], &cancelled, -240).unwrap();
        assert_eq!(uid, "uid-1");
        let s = &items[0];
        assert_eq!(s.tz.as_deref(), Some("America/New_York"));
        assert_eq!(s.start.offset_min, -240);
        assert_eq!(s.reminder_min, Some(10));
        assert_eq!(s.attendees[0].response, ResponseStatus::Accepted);
        let rec = s.recurrence.as_ref().unwrap();
        assert!(rec.rrule.starts_with("FREQ=WEEKLY;BYDAY=MO;UNTIL=20261229T045959Z"), "{}", rec.rrule);
        assert_eq!(rec.exdates.len(), 1);
        assert_eq!(items[1].recurrence_id.map(|r| r.unix), parse_instant("2026-10-12T13:00:00Z"));
        assert_eq!(items[1].uid, "uid-1");
        // Written in the event's zone, by its Windows name, and read back the same.
        let p = event_props(s);
        assert_eq!(p["start"]["dateTime"], "2026-10-05T09:00:00");
        assert_eq!(p["start"]["timeZone"], "Eastern Standard Time");
        assert_eq!(p["recurrence"]["range"]["endDate"], "2026-12-28");
        assert_eq!(p["recurrence"]["pattern"]["daysOfWeek"], json!(["monday"]));
        let mut again = Value::Object(p.clone());
        again["originalStartTimeZone"] = json!("Eastern Standard Time");
        let back = appointment_of(&again, -240).unwrap();
        assert_eq!((back.start.unix, back.end.unix), (s.start.unix, s.end.unix));
        // All-day events are days, wherever the user is.
        let all = appointment_of(
            &json!({"isAllDay": true, "start": {"dateTime": "2026-10-09T00:00:00.0000000", "timeZone": "UTC"},
                    "end": {"dateTime": "2026-10-10T00:00:00.0000000", "timeZone": "UTC"}}),
            -240,
        )
        .unwrap();
        assert!(all.all_day);
        assert_eq!(all.end.unix - all.start.unix, DAY);
        assert_eq!(event_props(&all)["start"]["dateTime"], "2026-10-09T00:00:00");
        // Patterns Graph has, and one it has not.
        let monthly = pattern_of("FREQ=MONTHLY;BYDAY=-1FR;COUNT=3", 0, None, &|_| None).unwrap();
        assert_eq!(monthly["pattern"]["index"], "last");
        assert_eq!(monthly["range"]["numberOfOccurrences"], 3);
        assert_eq!(
            rrule_of(&monthly, &|_| String::new()).as_deref(),
            Some("FREQ=MONTHLY;BYDAY=-1FR;COUNT=3")
        );
        assert!(pattern_of("FREQ=MONTHLY;BYDAY=FR", 0, None, &|_| None).is_none());
        assert!(pattern_of("FREQ=HOURLY", 0, None, &|_| None).is_none());
        // Hostile input never panics.
        assert!(appointment_of(&json!({"start": {"dateTime": "garbage"}}), 0).is_none());
        assert!(zone_known("UTC") && zone_known("Eastern Standard Time") && zone_known("Europe/Berlin"));
        assert!(
            !zone_known("tzone://Microsoft/Custom") && !zone_known("Customized Time Zone") && !zone_known("")
        );
        assert!(to_appointments(&json!([]), &[], &[], 0).is_none());
        assert_eq!(parse_instant("2026-10-12T09:00:00-04:00"), parse_instant("2026-10-12T13:00:00Z"));
    }
}
