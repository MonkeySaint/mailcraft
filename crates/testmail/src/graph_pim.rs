//! The fake Microsoft Graph service's contacts, calendars and Microsoft To Do tasks (REST v1.0 shapes from the
//! public API reference), next to its mail (`graph.rs`), for every user of the store:
//!
//! - contacts: `GET /me/contactFolders` (the folders below the default one, each with its `parentFolderId`),
//!   `POST`; `…/contacts` and `/me/contacts` (the default folder) `GET` / `POST`; `GET …/contacts/delta`;
//!   `GET` / `PATCH` / `DELETE` of one contact;
//! - calendars: `GET /me/calendars` (the user's own and the ones shared with them, with `owner` and
//!   `canEdit`), `GET /me/calendar`, `POST /me/calendars`; events of a calendar (`…/events`, also with
//!   `$filter=iCalUId eq '…'`) `GET` / `POST`; `GET …/calendarView/delta?startDateTime=…&endDateTime=…`
//!   (the instances in the range: single events, occurrences and exceptions of series, as Graph lists them);
//!   one event `GET` / `PATCH` / `DELETE` (an occurrence id makes an exception, or cancels the occurrence);
//!   `GET …/instances?startDateTime=…&endDateTime=…`; `POST …/accept` / `tentativelyAccept` / `decline`;
//!   other users' calendars under `/users/{address}/calendar…` when the owner shared theirs
//!   ([`crate::TestServers::graph_share_calendar`]), else 403;
//! - scheduling done by the service: an event with attendees made or changed by its organizer puts a copy on
//!   each local attendee's default calendar and an invitation (iTIP REQUEST) in their Inbox; an answer
//!   updates the organizer's copy; every invitation and answer is recorded for tests;
//! - To Do: `GET` / `POST /me/todo/lists`, `…/tasks` `GET` / `POST`, `GET …/tasks/delta`, one task `GET` /
//!   `PATCH` / `DELETE`;
//! - `GET /me/mailboxSettings/timeZone`.
//!
//! Delta tokens are the service's change counter: a delta link lists what changed after it (objects whole,
//! deletions as `@removed`), paged through `$skiptoken`; a token from before
//! [`crate::TestServers::graph_expire_deltas`] answers 410 `SyncStateNotFound`. Event times are kept as
//! written (`dateTime` and `timeZone`) and listed in UTC; the zones the fake knows are UTC, the North
//! American and European ones with their daylight saving rules, Tokyo and India (by Windows or IANA name).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::date::{DateTime, civil_from_days, days_from_civil};
use crate::server::Ctx;
use crate::store::append_msg;

const DAY: i64 = 86_400;
/// Most occurrences one series expands to.
const MAX_EXPAND: usize = 3_000;
/// Most days one expansion walks.
const MAX_DAYS: i64 = 366 * 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Contacts,
    Calendar,
    Todo,
}

#[derive(Clone, Debug)]
struct Coll {
    owner: String,
    kind: Kind,
    name: String,
    default: bool,
    /// A contact folder below another one (`None`: below the default folder).
    parent: Option<String>,
    /// A To Do list's well-known name (`flaggedEmails`: the read-only list of flagged messages).
    wellknown: Option<&'static str>,
}

#[derive(Clone, Debug, Default)]
struct Obj {
    coll: String,
    data: Value,
    seq: u64,
    change: u64,
    deleted: bool,
    /// An exception: its series and the original start (UTC) of the occurrence it replaces.
    master: Option<String>,
    original: Option<i64>,
    /// A series: original starts of its cancelled occurrences.
    cancelled: BTreeSet<i64>,
    /// An event's iCalendar UID.
    uid: String,
}

/// The PIM part of the fake service (inside the Graph state's lock).
#[derive(Debug, Default)]
pub struct PimState {
    seq: u64,
    next: u64,
    colls: BTreeMap<String, Coll>,
    objs: BTreeMap<String, Obj>,
    users: BTreeSet<String>,
    /// (owner, grantee): the owner shared their calendar with the grantee (`true`: may edit).
    grants: BTreeMap<(String, String), bool>,
    min_token: u64,
    /// Mailbox time zone per user (unset: "Pacific Standard Time").
    pub zones: BTreeMap<String, String>,
    /// Invitations, updates and cancellations the service sent: `{"organizer", "attendee", "subject", "kind"}`.
    pub invitations: Vec<Value>,
    /// Answers recorded: `{"user", "uid", "action", "comment", "sendResponse"}`.
    pub responses: Vec<Value>,
    /// Every contacts, calendar and To Do request answers 403 (a token signed in for mail only).
    pub deny: bool,
    /// The next this many event actions (`accept`, `tentativelyAccept`, `decline`, `cancel`) answer 503.
    pub fail_actions: u32,
}

fn key(u: &str) -> String {
    u.trim().to_ascii_lowercase()
}

fn err(status: u16, code: &str, message: &str) -> (u16, Value) {
    (status, json!({"error": {"code": code, "message": message}}))
}

fn not_found() -> (u16, Value) {
    err(404, "ErrorItemNotFound", "The specified object was not found in the store.")
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Times and zones
// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dst {
    None,
    NorthAmerica,
    Europe,
}

fn zone_rule(name: &str) -> (i64, Dst) {
    match name.trim().to_ascii_lowercase().as_str() {
        "eastern standard time" | "america/new_york" | "america/toronto" | "us/eastern" => {
            (-300, Dst::NorthAmerica)
        }
        "central standard time" | "america/chicago" => (-360, Dst::NorthAmerica),
        "mountain standard time" | "america/denver" => (-420, Dst::NorthAmerica),
        "pacific standard time" | "america/los_angeles" | "america/vancouver" => (-480, Dst::NorthAmerica),
        "gmt standard time" | "europe/london" => (0, Dst::Europe),
        "w. europe standard time" | "romance standard time" | "europe/berlin" | "europe/paris" => {
            (60, Dst::Europe)
        }
        "tokyo standard time" | "asia/tokyo" => (540, Dst::None),
        "india standard time" | "asia/kolkata" | "asia/calcutta" => (330, Dst::None),
        _ => (0, Dst::None),
    }
}

fn weekday(day: i64) -> i64 {
    // 1970-01-01 was a Thursday; 0 = Sunday.
    (day + 4).rem_euclid(7)
}

/// The day number of the `n`th (1-based; -1 = last) Sunday of month `m` of year `y`.
fn nth_sunday(y: i64, m: u32, n: i32) -> i64 {
    if n > 0 {
        let first = days_from_civil(y, m, 1);
        let to_sun = (7 - weekday(first)).rem_euclid(7);
        first + to_sun + 7 * i64::from(n - 1)
    } else {
        let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        let last = days_from_civil(ny, nm, 1) - 1;
        last - weekday(last)
    }
}

fn in_dst(utc: i64, std: i64, rule: Dst) -> bool {
    let (y, _, _) = civil_from_days(utc.div_euclid(DAY));
    match rule {
        Dst::None => false,
        Dst::NorthAmerica => {
            let start = nth_sunday(y, 3, 2) * DAY + 2 * 3600 - std * 60;
            let end = nth_sunday(y, 11, 1) * DAY + 2 * 3600 - (std + 60) * 60;
            utc >= start && utc < end
        }
        Dst::Europe => {
            let start = nth_sunday(y, 3, -1) * DAY + 3600;
            let end = nth_sunday(y, 10, -1) * DAY + 3600;
            utc >= start && utc < end
        }
    }
}

fn offset_at(utc: i64, zone: &str) -> i64 {
    let (std, rule) = zone_rule(zone);
    if in_dst(utc, std, rule) { std + 60 } else { std }
}

/// Wall-clock seconds in `zone` → UTC.
fn utc_of_local(local: i64, zone: &str) -> i64 {
    let (std, rule) = zone_rule(zone);
    let dst = local - (std + 60) * 60;
    if rule != Dst::None && in_dst(dst, std, rule) { dst } else { local - std * 60 }
}

/// `YYYY-MM-DDTHH:MM:SS[.fff][Z]` as civil seconds.
fn parse_civil(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T').unwrap_or((s, "00:00:00"));
    let mut d = date.split('-');
    let (y, mo, da) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let time = time.split(['.', '+']).next().unwrap_or("00:00:00");
    let mut t = time.split(':');
    let (h, mi, se) = (
        t.next()?.parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
    );
    DateTime::from_civil(y, mo, da, h, mi, se, 0).map(|d| d.unix)
}

fn civil_text(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(DAY));
    let t = secs.rem_euclid(DAY);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}

/// A `dateTimeTimeZone` (`{"dateTime", "timeZone"}`) as UTC seconds.
fn dtz_utc(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    let local = parse_civil(v.get("dateTime")?.as_str()?)?;
    let zone = v.get("timeZone").and_then(Value::as_str).unwrap_or("UTC");
    Some(utc_of_local(local, zone))
}

fn zone_of(v: Option<&Value>) -> String {
    v.and_then(|v| v.get("timeZone")).and_then(Value::as_str).unwrap_or("UTC").to_owned()
}

// ---------------------------------------------------------------------------------------------------------
// Recurrence expansion
// ---------------------------------------------------------------------------------------------------------

fn day_index(name: &str) -> Option<i64> {
    ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"]
        .iter()
        .position(|d| d.eq_ignore_ascii_case(name))
        .and_then(|i| i64::try_from(i).ok())
}

/// Original starts (UTC) of a series' occurrences overlapping `[from, to)`, cancelled ones included.
fn expand(ev: &Value, from: i64, to: i64) -> Vec<i64> {
    let Some(rec) = ev.get("recurrence").filter(|r| r.is_object()) else { return Vec::new() };
    let zone = zone_of(ev.get("start"));
    let Some(start_local) =
        ev.get("start").and_then(|s| s.get("dateTime")).and_then(Value::as_str).and_then(parse_civil)
    else {
        return Vec::new();
    };
    let end_local = ev
        .get("end")
        .and_then(|s| s.get("dateTime"))
        .and_then(Value::as_str)
        .and_then(parse_civil)
        .unwrap_or(start_local);
    let duration = (end_local - start_local).max(0);
    let p = rec.get("pattern").cloned().unwrap_or(Value::Null);
    let r = rec.get("range").cloned().unwrap_or(Value::Null);
    let kind = p.get("type").and_then(Value::as_str).unwrap_or("daily").to_owned();
    let interval = p.get("interval").and_then(Value::as_i64).unwrap_or(1).max(1);
    let days: Vec<i64> = p
        .get("daysOfWeek")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).filter_map(day_index).collect())
        .unwrap_or_default();
    let dom = p.get("dayOfMonth").and_then(Value::as_i64).unwrap_or(0);
    let month = p.get("month").and_then(Value::as_i64).unwrap_or(0);
    let index = p.get("index").and_then(Value::as_str).unwrap_or("first").to_owned();
    let wkst = p.get("firstDayOfWeek").and_then(Value::as_str).and_then(day_index).unwrap_or(0);
    let first_day = r
        .get("startDate")
        .and_then(Value::as_str)
        .and_then(parse_civil)
        .map_or(start_local.div_euclid(DAY), |s| s.div_euclid(DAY))
        .max(start_local.div_euclid(DAY));
    let tod = start_local.rem_euclid(DAY);
    let end_day = match r.get("type").and_then(Value::as_str) {
        Some("endDate") => {
            r.get("endDate").and_then(Value::as_str).and_then(parse_civil).map(|e| e.div_euclid(DAY))
        }
        _ => None,
    };
    let count = match r.get("type").and_then(Value::as_str) {
        Some("numbered") => r.get("numberOfOccurrences").and_then(Value::as_i64).map(|n| n.max(0)),
        _ => None,
    };
    let base_day = start_local.div_euclid(DAY);
    let (by, bm, _) = civil_from_days(base_day);
    let months_of = |d: i64| {
        let (y, m, _) = civil_from_days(d);
        (y - by) * 12 + i64::from(m) - i64::from(bm)
    };
    let mut out = Vec::new();
    let mut n = 0i64;
    for d in first_day..first_day.saturating_add(MAX_DAYS) {
        if end_day.is_some_and(|e| d > e) || count.is_some_and(|c| n >= c) || out.len() >= MAX_EXPAND {
            break;
        }
        let (y, m, dd) = civil_from_days(d);
        let wd = weekday(d);
        let nth_ok = |d: i64| {
            let (_, _, dd) = civil_from_days(d);
            let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            let last = days_from_civil(ny, nm, 1) - 1;
            match index.as_str() {
                "second" => (8..=14).contains(&dd),
                "third" => (15..=21).contains(&dd),
                "fourth" => (22..=28).contains(&dd),
                "last" => d + 7 > last,
                _ => dd <= 7,
            }
        };
        let hit = match kind.as_str() {
            "daily" => (d - base_day).rem_euclid(interval) == 0,
            "weekly" => {
                let week = |x: i64| (x - (wkst - 4).rem_euclid(7)).div_euclid(7);
                let day_ok = if days.is_empty() { wd == weekday(base_day) } else { days.contains(&wd) };
                day_ok && (week(d) - week(base_day)).rem_euclid(interval) == 0
            }
            "absoluteMonthly" => i64::from(dd) == dom && months_of(d).rem_euclid(interval) == 0,
            "relativeMonthly" => days.contains(&wd) && nth_ok(d) && months_of(d).rem_euclid(interval) == 0,
            "absoluteYearly" => {
                i64::from(m) == month && i64::from(dd) == dom && (y - by).rem_euclid(interval) == 0
            }
            "relativeYearly" => {
                i64::from(m) == month && days.contains(&wd) && nth_ok(d) && (y - by).rem_euclid(interval) == 0
            }
            _ => false,
        };
        if !hit {
            continue;
        }
        n += 1;
        let local = d * DAY + tod;
        let utc = utc_of_local(local, &zone);
        if utc >= to {
            break;
        }
        if utc.saturating_add(duration) > from || (duration == 0 && utc >= from) {
            out.push(utc);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------------------

impl PimState {
    fn id(&mut self, prefix: &str) -> String {
        self.next = self.next.saturating_add(1);
        format!("{prefix}{:06}", self.next)
    }

    fn bump(&mut self) -> u64 {
        self.seq = self.seq.saturating_add(1);
        self.seq
    }

    /// Makes `user`'s default contacts folder, calendar and task list.
    fn ensure(&mut self, user: &str) {
        let u = key(user);
        if !self.users.insert(u.clone()) {
            return;
        }
        for (prefix, kind, name) in [
            ("AAcf", Kind::Contacts, "Contacts"),
            ("AAcal", Kind::Calendar, "Calendar"),
            ("AAtl", Kind::Todo, "Tasks"),
        ] {
            let id = self.id(prefix);
            self.colls.insert(
                id,
                Coll {
                    owner: u.clone(),
                    kind,
                    name: name.into(),
                    default: true,
                    parent: None,
                    wellknown: None,
                },
            );
        }
        // The service's own list of flagged messages: listed with the To Do lists, read-only.
        let id = self.id("AAtl");
        self.colls.insert(
            id,
            Coll {
                owner: u,
                kind: Kind::Todo,
                name: "Flagged email".into(),
                default: false,
                parent: None,
                wellknown: Some("flaggedEmails"),
            },
        );
    }

    fn default_coll(&self, user: &str, kind: Kind) -> Option<String> {
        let u = key(user);
        self.colls.iter().find(|(_, c)| c.owner == u && c.kind == kind && c.default).map(|(id, _)| id.clone())
    }

    /// True when `who` may read (and with `write`, change) collection `coll`.
    fn may(&self, who: &str, coll: &str, write: bool) -> bool {
        let Some(c) = self.colls.get(coll) else { return false };
        let who = key(who);
        if c.owner == who {
            return true;
        }
        c.kind == Kind::Calendar
            && self.grants.get(&(c.owner.clone(), who)).is_some_and(|edit| *edit || !write)
    }

    fn live(&self, coll: &str) -> impl Iterator<Item = (&String, &Obj)> {
        let coll = coll.to_owned();
        self.objs.iter().filter(move |(_, o)| o.coll == coll && !o.deleted)
    }

    fn json_of(&self, id: &str, o: &Obj) -> Value {
        let kind = self.colls.get(&o.coll).map_or(Kind::Contacts, |c| c.kind);
        let mut v = o.data.clone();
        if !v.is_object() {
            v = json!({});
        }
        let tag = format!("W/\"ck{}\"", o.change);
        put(&mut v, "id", json!(id));
        put(&mut v, "@odata.etag", json!(tag));
        match kind {
            Kind::Contacts => {
                put(&mut v, "changeKey", json!(format!("ck{}", o.change)));
                put(&mut v, "parentFolderId", json!(o.coll));
            }
            Kind::Todo => {
                if v.get("status").is_none() {
                    put(&mut v, "status", json!("notStarted"));
                }
                if v.get("importance").is_none() {
                    put(&mut v, "importance", json!("normal"));
                }
            }
            Kind::Calendar => {
                put(&mut v, "changeKey", json!(format!("ck{}", o.change)));
                self.event_shape(&mut v, o);
            }
        }
        v
    }

    /// Event times in UTC, the zones written, type, UID and organizer.
    fn event_shape(&self, v: &mut Value, o: &Obj) {
        for k in ["start", "end"] {
            let zone = zone_of(v.get(k));
            // All-day times too: their midnights in the zone they were written in, given in UTC (as the
            // service gives every time in the zone the request asks for).
            let text = dtz_utc(v.get(k)).map(civil_text);
            if let Some(t) = text {
                put(v, k, json!({"dateTime": format!("{t}.0000000"), "timeZone": "UTC"}));
            }
            let name = if k == "start" { "originalStartTimeZone" } else { "originalEndTimeZone" };
            put(v, name, json!(zone));
        }
        put(v, "iCalUId", json!(o.uid));
        let owner = self.colls.get(&o.coll).map(|c| c.owner.clone()).unwrap_or_default();
        if v.get("organizer").is_none() {
            put(v, "organizer", json!({"emailAddress": {"name": owner, "address": owner}}));
        }
        let organizer = v
            .get("organizer")
            .and_then(|x| x.get("emailAddress"))
            .and_then(|x| x.get("address"))
            .and_then(Value::as_str)
            .map(key)
            .unwrap_or_default();
        put(v, "isOrganizer", json!(organizer == owner));
        if v.get("responseStatus").is_none() {
            let r = if organizer == owner { "organizer" } else { "none" };
            put(v, "responseStatus", json!({"response": r, "time": "0001-01-01T00:00:00Z"}));
        }
        let kind = match (&o.master, v.get("recurrence").is_some_and(Value::is_object)) {
            (Some(m), _) => {
                put(v, "seriesMasterId", json!(m));
                if let Some(orig) = o.original {
                    put(v, "originalStart", json!(format!("{}Z", civil_text(orig))));
                }
                "exception"
            }
            (None, true) => "seriesMaster",
            (None, false) => "singleInstance",
        };
        put(v, "type", json!(kind));
    }

    /// The occurrence of series `master` that started at `orig` (UTC), as the service lists it.
    fn occurrence_json(&self, master: &str, m: &Obj, orig: i64) -> Value {
        let mut base = m.data.clone();
        let start = dtz_utc(base.get("start")).unwrap_or(orig);
        let end = dtz_utc(base.get("end")).unwrap_or(start);
        let shift = |v: &mut Value, k: &str, t: i64| {
            let zone = zone_of(v.get(k));
            let local = t + offset_at(t, &zone) * 60;
            put(v, k, json!({"dateTime": civil_text(local), "timeZone": zone}));
        };
        shift(&mut base, "start", orig);
        shift(&mut base, "end", orig + (end - start).max(0));
        if let Some(o) = base.as_object_mut() {
            o.remove("recurrence");
        }
        let occ = Obj { data: base, master: Some(master.to_owned()), original: Some(orig), ..m.clone() };
        let mut v = self.json_of(&occurrence_id(master, orig), &occ);
        put(&mut v, "type", json!("occurrence"));
        put(&mut v, "recurrence", Value::Null);
        v
    }

    /// Instances of event `id` (a single event, or a series' occurrences and exceptions) overlapping
    /// `[from, to)`.
    fn instances(&self, id: &str, from: i64, to: i64) -> Vec<Value> {
        let Some(o) = self.objs.get(id).filter(|o| !o.deleted) else { return Vec::new() };
        if o.data.get("recurrence").is_none_or(|r| !r.is_object()) {
            let s = dtz_utc(o.data.get("start")).unwrap_or(0);
            let e = dtz_utc(o.data.get("end")).unwrap_or(s);
            return if s < to && e.max(s + 1) > from { vec![self.json_of(id, o)] } else { Vec::new() };
        }
        let exceptions: BTreeMap<i64, (&String, &Obj)> = self
            .objs
            .iter()
            .filter(|(_, x)| !x.deleted && x.master.as_deref() == Some(id))
            .filter_map(|(xid, x)| Some((x.original?, (xid, x))))
            .collect();
        let mut out = Vec::new();
        for orig in expand(&o.data, from, to) {
            if o.cancelled.contains(&orig) {
                continue;
            }
            match exceptions.get(&orig) {
                Some((xid, x)) => out.push(self.json_of(xid, x)),
                None => out.push(self.occurrence_json(id, o, orig)),
            }
        }
        out
    }

    fn event_path_coll(&self, id: &str) -> Option<String> {
        self.objs.get(id).map(|o| o.coll.clone())
    }
}

fn put(v: &mut Value, key: &str, x: Value) {
    if let Some(m) = v.as_object_mut() {
        m.insert(key.to_owned(), x);
    }
}

/// Merges `patch` into `data` (top-level keys; `null` removes).
fn merge(data: &mut Value, patch: &Value) {
    if !data.is_object() {
        *data = json!({});
    }
    let (Some(d), Some(p)) = (data.as_object_mut(), patch.as_object()) else { return };
    for (k, v) in p {
        if matches!(k.as_str(), "id" | "@odata.etag" | "changeKey" | "iCalUId" | "type" | "seriesMasterId") {
            continue;
        }
        if v.is_null() {
            d.remove(k);
        } else {
            d.insert(k.clone(), v.clone());
        }
    }
}

fn occurrence_id(master: &str, orig: i64) -> String {
    format!("{master}_occ_{orig}")
}

fn split_occurrence(id: &str) -> Option<(&str, i64)> {
    let (m, t) = id.rsplit_once("_occ_")?;
    Some((m, t.parse().ok()?))
}

// ---------------------------------------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------------------------------------

/// Answers a PIM request of `user` (`None`: not a PIM path).
pub(crate) fn route(
    ctx: &Ctx,
    user: &str,
    method: &str,
    segs: &[&str],
    q: &BTreeMap<String, String>,
    body: &Value,
    page: usize,
    base: &str,
) -> Option<(u16, Value)> {
    let (target, rest): (String, &[&str]) = match segs {
        ["me", rest @ ..] => (key(user), rest),
        ["users", u, rest @ ..] => (key(u), rest),
        _ => return None,
    };
    let first = rest.first().copied().unwrap_or_default();
    if !matches!(
        first,
        "contactFolders"
            | "contacts"
            | "calendars"
            | "calendar"
            | "calendarView"
            | "events"
            | "todo"
            | "mailboxSettings"
    ) {
        return None;
    }
    let mut mails: Vec<(String, Vec<u8>)> = Vec::new();
    let answer = {
        let mut g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
        let st = &mut g.pim;
        if st.deny {
            return Some(denied());
        }
        st.ensure(user);
        st.ensure(&target);
        let me = key(user);
        if target != me && !matches!(first, "calendar" | "calendars" | "calendarView" | "events") {
            return Some(err(403, "ErrorAccessDenied", "Access is denied. Check credentials and try again."));
        }
        let path = segs.iter().map(|s| enc(s)).collect::<Vec<_>>().join("/");
        let r = Req { me: &me, target: &target, method, q, body, page, base, path: &path };
        handle(st, &r, rest, &mut mails)
    };
    if !mails.is_empty() {
        {
            let mut store = ctx.shared.lock();
            for (to, raw) in mails {
                if let Some(mb) = store.user_mut(&to).and_then(|u| u.mailbox_mut("INBOX")) {
                    append_msg(mb, Arc::from(raw.as_slice()), Vec::new(), DateTime::now());
                }
            }
        }
        ctx.shared.notify();
    }
    Some(answer)
}

struct Req<'a> {
    me: &'a str,
    target: &'a str,
    method: &'a str,
    q: &'a BTreeMap<String, String>,
    body: &'a Value,
    page: usize,
    base: &'a str,
    path: &'a str,
}

fn handle(st: &mut PimState, r: &Req<'_>, rest: &[&str], mails: &mut Vec<(String, Vec<u8>)>) -> (u16, Value) {
    let m = r.method;
    match (m, rest) {
        ("GET", ["mailboxSettings", "timeZone"]) => {
            let z = st.zones.get(r.me).cloned().unwrap_or_else(|| "Pacific Standard Time".into());
            (200, json!({"value": z}))
        }
        ("GET", ["mailboxSettings"]) => {
            let z = st.zones.get(r.me).cloned().unwrap_or_else(|| "Pacific Standard Time".into());
            (200, json!({"timeZone": z}))
        }
        // Contacts
        ("GET", ["contactFolders"]) => {
            let Some(def) = st.default_coll(r.me, Kind::Contacts) else { return not_found() };
            let list: Vec<Value> = st
                .colls
                .iter()
                .filter(|(_, c)| {
                    c.owner == r.me && c.kind == Kind::Contacts && !c.default && c.parent.is_none()
                })
                .map(|(id, c)| json!({"id": id, "displayName": c.name, "parentFolderId": def}))
                .collect();
            page_of(list, r)
        }
        ("POST", ["contactFolders"]) => create_coll(st, r, Kind::Contacts, "AAcf", None),
        ("GET", ["contactFolders", f, "childFolders"]) => {
            if !st.may(r.me, f, false) || st.colls.get(*f).is_none_or(|c| c.kind != Kind::Contacts) {
                return not_found();
            }
            let list: Vec<Value> = st
                .colls
                .iter()
                .filter(|(_, c)| c.owner == r.me && c.parent.as_deref() == Some(*f))
                .map(|(id, c)| json!({"id": id, "displayName": c.name, "parentFolderId": f}))
                .collect();
            page_of(list, r)
        }
        ("POST", ["contactFolders", f, "childFolders"]) => {
            if !st.may(r.me, f, true) || st.colls.get(*f).is_none_or(|c| c.kind != Kind::Contacts) {
                return not_found();
            }
            let parent = (*f).to_owned();
            create_coll(st, r, Kind::Contacts, "AAcf", Some(parent))
        }
        ("GET", ["contacts"]) | ("POST", ["contacts"]) => {
            let Some(def) = st.default_coll(r.me, Kind::Contacts) else { return not_found() };
            objects(st, r, &def, mails)
        }
        ("GET", ["contactFolders", f, "contacts"]) | ("POST", ["contactFolders", f, "contacts"]) => {
            if !st.may(r.me, f, false) || st.colls.get(*f).is_none_or(|c| c.kind != Kind::Contacts) {
                return not_found();
            }
            objects(st, r, f, mails)
        }
        ("GET", ["contactFolders", f, "contacts", "delta"]) => {
            if !st.may(r.me, f, false) {
                return not_found();
            }
            delta(st, r, f, None)
        }
        (_, ["contacts", id]) | (_, ["contactFolders", _, "contacts", id]) => {
            one(st, r, id, Kind::Contacts, mails)
        }
        // To Do
        ("GET", ["todo", "lists"]) => {
            let list: Vec<Value> = st
                .colls
                .iter()
                .filter(|(_, c)| c.owner == r.me && c.kind == Kind::Todo)
                .map(|(id, c)| {
                    let known = c.wellknown.unwrap_or(if c.default { "defaultList" } else { "none" });
                    json!({"id": id, "displayName": c.name, "isOwner": true, "wellknownListName": known})
                })
                .collect();
            page_of(list, r)
        }
        ("POST", ["todo", "lists"]) => create_coll(st, r, Kind::Todo, "AAtl", None),
        ("GET", ["todo", "lists", l, "tasks"]) | ("POST", ["todo", "lists", l, "tasks"]) => {
            if !st.may(r.me, l, false) || st.colls.get(*l).is_none_or(|c| c.kind != Kind::Todo) {
                return not_found();
            }
            if m == "POST" && st.colls.get(*l).is_some_and(|c| c.wellknown == Some("flaggedEmails")) {
                return err(400, "invalidRequest", "Tasks cannot be created in the flagged email list.");
            }
            objects(st, r, l, mails)
        }
        ("GET", ["todo", "lists", l, "tasks", "delta"]) => {
            if !st.may(r.me, l, false) {
                return not_found();
            }
            delta(st, r, l, None)
        }
        (_, ["todo", "lists", _, "tasks", id]) => one(st, r, id, Kind::Todo, mails),
        // Calendars
        ("GET", ["calendars"]) if r.target == r.me => {
            let me = r.me.to_owned();
            let list: Vec<Value> = st
                .colls
                .iter()
                .filter(|(_, c)| c.kind == Kind::Calendar)
                .filter(|(id, c)| c.owner == me || (c.default && st.may(&me, id, false)))
                .map(|(id, c)| {
                    let edit = c.owner == me || st.may(&me, id, true);
                    json!({"id": id, "name": c.name, "isDefaultCalendar": c.default && c.owner == me,
                           "canEdit": edit, "canShare": c.owner == me,
                           "owner": {"name": c.owner, "address": c.owner}})
                })
                .collect();
            page_of(list, r)
        }
        ("POST", ["calendars"]) if r.target == r.me => create_coll(st, r, Kind::Calendar, "AAcal", None),
        ("GET", ["calendarView", "delta"]) => {
            let Some(c) = st.default_coll(r.target, Kind::Calendar) else { return not_found() };
            if !st.may(r.me, &c, false) {
                return denied();
            }
            delta(st, r, &c, window(r.q))
        }
        _ => calendar_routes(st, r, rest, mails),
    }
}

fn denied() -> (u16, Value) {
    err(403, "ErrorAccessDenied", "Access is denied. Check credentials and try again.")
}

fn window(q: &BTreeMap<String, String>) -> Option<(i64, i64)> {
    let from = q.get("startDateTime").and_then(|s| parse_civil(s))?;
    let to = q.get("endDateTime").and_then(|s| parse_civil(s))?;
    Some((from, to))
}

fn calendar_routes(
    st: &mut PimState,
    r: &Req<'_>,
    rest: &[&str],
    mails: &mut Vec<(String, Vec<u8>)>,
) -> (u16, Value) {
    // The calendar addressed (`calendar` = the target's default) and what follows it.
    let (cal, tail): (Option<String>, &[&str]) = match rest {
        ["calendar", tail @ ..] => (st.default_coll(r.target, Kind::Calendar), tail),
        ["calendars", c, tail @ ..] => (Some((*c).to_owned()), tail),
        ["events", ..] => (None, rest),
        _ => return err(400, "BadRequest", "unsupported request"),
    };
    if let Some(c) = &cal {
        if st.colls.get(c).is_none_or(|x| x.kind != Kind::Calendar) {
            return not_found();
        }
        if !st.may(r.me, c, false) {
            return denied();
        }
        if r.target != r.me && st.colls.get(c).is_some_and(|x| key(&x.owner) != r.target) {
            return not_found();
        }
    }
    match (r.method, cal.as_deref(), tail) {
        ("GET", Some(c), []) => {
            let Some(x) = st.colls.get(c) else { return not_found() };
            let edit = st.may(r.me, c, true);
            (
                200,
                json!({"id": c, "name": x.name, "isDefaultCalendar": x.default, "canEdit": edit,
                         "owner": {"name": x.owner, "address": x.owner}}),
            )
        }
        ("GET", Some(c), ["calendarView", "delta"]) => {
            let c = c.to_owned();
            delta(st, r, &c, window(r.q))
        }
        ("GET", Some(c), ["events"]) => {
            let c = c.to_owned();
            objects(st, r, &c, mails)
        }
        ("POST", Some(c), ["events"]) => {
            if !st.may(r.me, c, true) {
                return denied();
            }
            let c = c.to_owned();
            objects(st, r, &c, mails)
        }
        ("GET", None, ["events"]) => {
            // Every calendar of the target the user may read.
            let me = r.me.to_owned();
            let target = r.target.to_owned();
            let colls: Vec<String> = st
                .colls
                .iter()
                .filter(|(id, c)| c.kind == Kind::Calendar && c.owner == target && st.may(&me, id, false))
                .map(|(id, _)| id.clone())
                .collect();
            let mut list = Vec::new();
            for c in colls {
                list.extend(filtered(st, r, &c));
            }
            page_of(list, r)
        }
        (_, _, ["events", id, rest @ ..]) => event_routes(st, r, id, rest, mails),
        _ => err(400, "BadRequest", "unsupported request"),
    }
}

/// The objects of `coll` (masters and single events for a calendar), filtered by `$filter=iCalUId eq '…'`.
fn filtered(st: &PimState, r: &Req<'_>, coll: &str) -> Vec<Value> {
    let uid = r.q.get("$filter").and_then(|f| {
        let rest = f.trim().strip_prefix("iCalUId eq '")?;
        Some(rest.strip_suffix('\'')?.replace("''", "'"))
    });
    st.live(coll)
        .filter(|(_, o)| o.master.is_none())
        .filter(|(_, o)| uid.as_ref().is_none_or(|u| o.uid == *u))
        .map(|(id, o)| st.json_of(id, o))
        .collect()
}

fn page_of(list: Vec<Value>, r: &Req<'_>) -> (u16, Value) {
    let skip: usize = r.q.get("$skip").and_then(|s| s.parse().ok()).unwrap_or(0);
    let top: usize = r.q.get("$top").and_then(|s| s.parse().ok()).unwrap_or(r.page).min(r.page).max(1);
    let chunk: Vec<Value> = list.iter().skip(skip).take(top).cloned().collect();
    let mut v = json!({"value": chunk});
    if skip.saturating_add(top) < list.len() {
        let mut params: Vec<String> =
            r.q.iter()
                .filter(|(k, _)| *k != "$skip" && *k != "$top")
                .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
                .collect();
        params.push(format!("$top={top}"));
        params.push(format!("$skip={}", skip + top));
        put(&mut v, "@odata.nextLink", json!(format!("{}/{}?{}", r.base, r.path, params.join("&"))));
    }
    (200, v)
}

fn create_coll(
    st: &mut PimState,
    r: &Req<'_>,
    kind: Kind,
    prefix: &str,
    parent: Option<String>,
) -> (u16, Value) {
    let name = r
        .body
        .get("displayName")
        .or_else(|| r.body.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if name.is_empty() {
        return err(400, "ErrorInvalidRequest", "a name is required");
    }
    let id = st.id(prefix);
    st.colls.insert(
        id.clone(),
        Coll { owner: r.me.to_owned(), kind, name: name.clone(), default: false, parent, wellknown: None },
    );
    (201, json!({"id": id, "displayName": name, "name": name}))
}

/// `GET` (list) or `POST` (create) the objects of `coll`.
fn objects(st: &mut PimState, r: &Req<'_>, coll: &str, mails: &mut Vec<(String, Vec<u8>)>) -> (u16, Value) {
    let kind = st.colls.get(coll).map_or(Kind::Contacts, |c| c.kind);
    if r.method == "GET" {
        let list = filtered(st, r, coll);
        return page_of(list, r);
    }
    if !r.body.is_object() {
        return err(400, "ErrorInvalidRequest", "a JSON object is required");
    }
    if kind == Kind::Todo && r.body.get("title").and_then(Value::as_str).is_none_or(|t| t.trim().is_empty()) {
        return err(400, "invalidRequest", "title is required");
    }
    if kind == Kind::Calendar
        && (dtz_utc(r.body.get("start")).is_none() || dtz_utc(r.body.get("end")).is_none())
    {
        return err(400, "ErrorInvalidRequest", "start and end are required");
    }
    let prefix = match kind {
        Kind::Contacts => "AAct",
        Kind::Calendar => "AAev",
        Kind::Todo => "AAtk",
    };
    let id = st.id(prefix);
    let seq = st.bump();
    let mut data = json!({});
    merge(&mut data, r.body);
    let uid = format!("040000008200E00074C5B7101A82E008{:016X}", st.next);
    st.objs
        .insert(id.clone(), Obj { coll: coll.to_owned(), data, seq, change: seq, uid, ..Default::default() });
    if kind == Kind::Calendar {
        schedule(st, &id, "request", mails);
    }
    let v = st.objs.get(&id).map(|o| st.json_of(&id, o)).unwrap_or(Value::Null);
    (201, v)
}

/// `GET` / `PATCH` / `DELETE` of contact or task `id`.
fn one(
    st: &mut PimState,
    r: &Req<'_>,
    id: &str,
    kind: Kind,
    _mails: &mut Vec<(String, Vec<u8>)>,
) -> (u16, Value) {
    let Some(o) = st.objs.get(id).filter(|o| !o.deleted) else { return not_found() };
    if st.colls.get(&o.coll).is_none_or(|c| c.kind != kind) || !st.may(r.me, &o.coll, r.method != "GET") {
        return not_found();
    }
    match r.method {
        "GET" => (200, st.json_of(id, o)),
        "PATCH" => {
            let seq = st.bump();
            if let Some(o) = st.objs.get_mut(id) {
                merge(&mut o.data, r.body);
                o.seq = seq;
                o.change = seq;
            }
            let v = st.objs.get(id).map(|o| st.json_of(id, o)).unwrap_or(Value::Null);
            (200, v)
        }
        "DELETE" => {
            let seq = st.bump();
            if let Some(o) = st.objs.get_mut(id) {
                o.deleted = true;
                o.seq = seq;
            }
            (204, Value::Null)
        }
        _ => err(405, "ErrorInvalidRequest", "method not allowed"),
    }
}

/// Requests on event `id` (or an occurrence of a series).
fn event_routes(
    st: &mut PimState,
    r: &Req<'_>,
    id: &str,
    rest: &[&str],
    mails: &mut Vec<(String, Vec<u8>)>,
) -> (u16, Value) {
    let occurrence = split_occurrence(id).map(|(m, t)| (m.to_owned(), t));
    let coll = match &occurrence {
        Some((m, _)) => st.event_path_coll(m),
        None => st.event_path_coll(id),
    };
    let Some(coll) = coll.filter(|c| st.colls.get(c).is_some_and(|x| x.kind == Kind::Calendar)) else {
        return not_found();
    };
    if !st.may(r.me, &coll, false) {
        return denied();
    }
    if r.target != r.me && st.colls.get(&coll).is_some_and(|x| x.owner != r.target) {
        return not_found();
    }
    let write = r.method != "GET";
    if write && rest.is_empty() && !st.may(r.me, &coll, true) {
        return denied();
    }
    // An occurrence that is an exception already is that exception.
    let id: String = match &occurrence {
        Some((m, t)) => st
            .objs
            .iter()
            .find(|(_, o)| !o.deleted && o.master.as_deref() == Some(m.as_str()) && o.original == Some(*t))
            .map(|(x, _)| x.clone())
            .unwrap_or_else(|| id.to_owned()),
        None => id.to_owned(),
    };
    let id = id.as_str();
    match (r.method, rest) {
        ("GET", []) => match (st.objs.get(id).filter(|o| !o.deleted), &occurrence) {
            (Some(o), _) => (200, st.json_of(id, o)),
            (None, Some((m, t))) => match st.objs.get(m).filter(|o| !o.deleted && !o.cancelled.contains(t)) {
                Some(mo) if expand(&mo.data, *t, t + 1).contains(t) => (200, st.occurrence_json(m, mo, *t)),
                _ => not_found(),
            },
            (None, None) => not_found(),
        },
        ("GET", ["instances"]) => {
            let Some((from, to)) = window(r.q) else {
                return err(400, "ErrorInvalidParameter", "startDateTime and endDateTime are required");
            };
            if st.objs.get(id).is_none_or(|o| o.deleted) {
                return not_found();
            }
            let list = st.instances(id, from, to);
            page_of(list, r)
        }
        ("PATCH", []) => {
            if st.objs.get(id).is_none_or(|o| o.deleted) {
                // A first change to an occurrence makes it an exception.
                let Some((m, t)) = &occurrence else { return not_found() };
                let Some(mo) = st.objs.get(m).filter(|o| !o.deleted && !o.cancelled.contains(t)).cloned()
                else {
                    return not_found();
                };
                if !expand(&mo.data, *t, t + 1).contains(t) {
                    return not_found();
                }
                let occ = st.occurrence_json(m, &mo, *t);
                let mut data = json!({});
                merge(&mut data, &occ);
                for k in [
                    "recurrence",
                    "originalStart",
                    "originalStartTimeZone",
                    "originalEndTimeZone",
                    "isOrganizer",
                    "responseStatus",
                ] {
                    if let Some(o) = data.as_object_mut() {
                        o.remove(k);
                    }
                }
                // Keep the occurrence's times in the zone they were written in.
                if let (Some(s), Some(e)) = (mo.data.get("start"), mo.data.get("end")) {
                    let dur = dtz_utc(Some(e)).unwrap_or(0) - dtz_utc(Some(s)).unwrap_or(0);
                    let zone = zone_of(Some(s));
                    let local = t + offset_at(*t, &zone) * 60;
                    let end = t + dur.max(0);
                    let elocal = end + offset_at(end, &zone) * 60;
                    put(&mut data, "start", json!({"dateTime": civil_text(local), "timeZone": zone}));
                    put(&mut data, "end", json!({"dateTime": civil_text(elocal), "timeZone": zone}));
                }
                merge(&mut data, r.body);
                let seq = st.bump();
                st.objs.insert(
                    id.to_owned(),
                    Obj {
                        coll: coll.clone(),
                        data,
                        seq,
                        change: seq,
                        master: Some(m.clone()),
                        original: Some(*t),
                        uid: mo.uid.clone(),
                        ..Default::default()
                    },
                );
                if let Some(mm) = st.objs.get_mut(m) {
                    mm.seq = seq;
                    mm.change = seq;
                }
                let v = st.objs.get(id).map(|o| st.json_of(id, o)).unwrap_or(Value::Null);
                return (200, v);
            }
            let seq = st.bump();
            let mut master = None;
            if let Some(o) = st.objs.get_mut(id) {
                merge(&mut o.data, r.body);
                o.seq = seq;
                o.change = seq;
                master = o.master.clone();
            }
            if let Some(m) = master.and_then(|m| st.objs.get_mut(&m)) {
                m.seq = seq;
            }
            schedule(st, id, "update", mails);
            let v = st.objs.get(id).map(|o| st.json_of(id, o)).unwrap_or(Value::Null);
            (200, v)
        }
        ("DELETE", []) => {
            let seq = st.bump();
            match st.objs.get(id).filter(|o| !o.deleted).cloned() {
                Some(o) => {
                    if o.master.is_none() {
                        schedule(st, id, "cancel", mails);
                    }
                    if let Some(x) = st.objs.get_mut(id) {
                        x.deleted = true;
                        x.seq = seq;
                    }
                    if let (Some(m), Some(t)) = (&o.master, o.original)
                        && let Some(mm) = st.objs.get_mut(m)
                    {
                        mm.cancelled.insert(t);
                        mm.seq = seq;
                        mm.change = seq;
                    }
                    if o.master.is_none() {
                        // A series takes its exceptions with it.
                        for x in st.objs.values_mut().filter(|x| x.master.as_deref() == Some(id)) {
                            x.deleted = true;
                            x.seq = seq;
                        }
                    }
                    (204, Value::Null)
                }
                None => match &occurrence {
                    Some((m, t)) => match st.objs.get_mut(m).filter(|o| !o.deleted) {
                        Some(mm) => {
                            mm.cancelled.insert(*t);
                            mm.seq = seq;
                            mm.change = seq;
                            (204, Value::Null)
                        }
                        None => not_found(),
                    },
                    None => not_found(),
                },
            }
        }
        ("POST", [action]) if matches!(*action, "accept" | "tentativelyAccept" | "decline" | "cancel") => {
            if st.fail_actions > 0 {
                st.fail_actions -= 1;
                return err(503, "ServiceUnavailable", "The service is temporarily unavailable.");
            }
            if *action == "cancel" {
                return cancel(st, id, r, mails);
            }
            answer(st, r, id, action, mails)
        }
        _ => err(400, "BadRequest", "unsupported request"),
    }
}

fn attendee_addresses(v: &Value) -> Vec<String> {
    v.get("attendees")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("emailAddress")?.get("address")?.as_str().map(key))
                .take(200)
                .collect()
        })
        .unwrap_or_default()
}

/// The service's scheduling for event `id` changed by its organizer (`kind`: request, update, cancel): each
/// attendee gets the invitation, and a local attendee a copy on their default calendar.
fn schedule(st: &mut PimState, id: &str, kind: &str, mails: &mut Vec<(String, Vec<u8>)>) {
    let Some(o) = st.objs.get(id).cloned() else { return };
    if o.master.is_some() {
        return;
    }
    let Some(owner) = st.colls.get(&o.coll).map(|c| c.owner.clone()) else { return };
    let shaped = st.json_of(id, &o);
    if shaped.get("isOrganizer").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let subject = o.data.get("subject").and_then(Value::as_str).unwrap_or_default().to_owned();
    for a in attendee_addresses(&o.data) {
        if a == owner {
            continue;
        }
        st.invitations.push(json!({"organizer": owner, "attendee": a, "subject": subject, "kind": kind}));
        st.ensure(&a);
        let Some(cal) = st.default_coll(&a, Kind::Calendar) else { continue };
        let copy = st
            .objs
            .iter()
            .find(|(_, x)| !x.deleted && x.coll == cal && x.uid == o.uid)
            .map(|(x, _)| x.clone());
        let seq = st.bump();
        if kind == "cancel" {
            if let Some(c) = copy.and_then(|c| st.objs.get_mut(&c)) {
                c.deleted = true;
                c.seq = seq;
            }
            continue;
        }
        let mut data = o.data.clone();
        put(&mut data, "organizer", json!({"emailAddress": {"name": owner, "address": owner}}));
        match copy {
            Some(c) => {
                if let Some(x) = st.objs.get_mut(&c) {
                    let keep = x.data.get("responseStatus").cloned();
                    x.data = data;
                    if let Some(k) = keep {
                        put(&mut x.data, "responseStatus", k);
                    }
                    x.seq = seq;
                    x.change = seq;
                }
            }
            None => {
                let nid = st.id("AAev");
                put(
                    &mut data,
                    "responseStatus",
                    json!({"response": "notResponded", "time": "0001-01-01T00:00:00Z"}),
                );
                st.objs.insert(
                    nid,
                    Obj { coll: cal, data, seq, change: seq, uid: o.uid.clone(), ..Default::default() },
                );
            }
        }
        let start = dtz_utc(o.data.get("start")).unwrap_or(0);
        let end = dtz_utc(o.data.get("end")).unwrap_or(start);
        let stamp = |t: i64| civil_text(t).replace(['-', ':'], "") + "Z";
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//testmail//fake graph//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
             UID:{uid}\r\nSEQUENCE:{seq}\r\nDTSTAMP:{now}\r\nDTSTART:{s}\r\nDTEND:{e}\r\nSUMMARY:{subject}\r\n\
             ORGANIZER:mailto:{owner}\r\nATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{a}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            uid = o.uid,
            now = stamp(DateTime::now().unix),
            s = stamp(start),
            e = stamp(end),
        );
        let raw = format!(
            "From: {owner}\r\nTo: {a}\r\nSubject: {subject}\r\nDate: {date}\r\nMessage-ID: <inv{seq}.{id}@graph.invalid>\r\n\
             MIME-Version: 1.0\r\nContent-Type: text/calendar; method=REQUEST; charset=utf-8\r\n\r\n{ics}",
            date = DateTime::now().to_rfc5322(),
        );
        mails.push((a.clone(), raw.into_bytes()));
    }
}

/// The organizer cancels meeting `id` with a note: the attendees get the cancellation (recorded with the
/// note) and the meeting goes.
fn cancel(st: &mut PimState, id: &str, r: &Req<'_>, mails: &mut Vec<(String, Vec<u8>)>) -> (u16, Value) {
    let Some(o) = st.objs.get(id).filter(|o| !o.deleted && o.master.is_none()).cloned() else {
        return not_found();
    };
    if st.json_of(id, &o).get("isOrganizer").and_then(Value::as_bool) != Some(true) {
        return err(400, "ErrorInvalidRequest", "Only the meeting organizer can cancel the meeting.");
    }
    let before = st.invitations.len();
    schedule(st, id, "cancel", mails);
    let note = r.body.get("Comment").or_else(|| r.body.get("comment")).cloned().unwrap_or(Value::Null);
    for i in st.invitations.iter_mut().skip(before) {
        put(i, "comment", note.clone());
    }
    let seq = st.bump();
    for x in st.objs.iter_mut().filter(|(k, x)| k.as_str() == id || x.master.as_deref() == Some(id)) {
        x.1.deleted = true;
        x.1.seq = seq;
    }
    (202, Value::Null)
}

/// An attendee answers the invitation on their copy `id`.
fn answer(
    st: &mut PimState,
    r: &Req<'_>,
    id: &str,
    action: &str,
    _mails: &mut Vec<(String, Vec<u8>)>,
) -> (u16, Value) {
    let Some(o) = st.objs.get(id).filter(|o| !o.deleted).cloned() else { return not_found() };
    let shaped = st.json_of(id, &o);
    if shaped.get("isOrganizer").and_then(Value::as_bool) == Some(true) {
        return err(
            400,
            "ErrorInvalidRequest",
            "Your request can't be completed. You can't respond to a meeting you organized.",
        );
    }
    let response = match action {
        "accept" => "accepted",
        "tentativelyAccept" => "tentativelyAccepted",
        _ => "declined",
    };
    let seq = st.bump();
    if let Some(x) = st.objs.get_mut(id) {
        put(
            &mut x.data,
            "responseStatus",
            json!({"response": response, "time": format!("{}Z", civil_text(DateTime::now().unix))}),
        );
        if action == "decline" {
            x.deleted = true;
        }
        x.seq = seq;
        x.change = seq;
    }
    st.responses.push(json!({
        "user": r.me, "uid": o.uid, "action": action,
        "comment": r.body.get("comment").cloned().unwrap_or(Value::Null),
        "sendResponse": r.body.get("sendResponse").cloned().unwrap_or(json!(true)),
    }));
    // The organizer's copy records the answer.
    let organizer = shaped
        .get("organizer")
        .and_then(|x| x.get("emailAddress"))
        .and_then(|x| x.get("address"))
        .and_then(Value::as_str)
        .map(key)
        .unwrap_or_default();
    let theirs: Vec<String> = st
        .objs
        .iter()
        .filter(|(_, x)| {
            !x.deleted && x.uid == o.uid && st.colls.get(&x.coll).is_some_and(|c| c.owner == organizer)
        })
        .map(|(x, _)| x.clone())
        .collect();
    let seq = st.bump();
    for t in theirs {
        if let Some(x) = st.objs.get_mut(&t)
            && let Some(list) = x.data.get_mut("attendees").and_then(Value::as_array_mut)
        {
            for a in list.iter_mut() {
                let mine =
                    a.get("emailAddress").and_then(|e| e.get("address")).and_then(Value::as_str).map(key);
                if mine.as_deref() == Some(r.me) {
                    put(
                        a,
                        "status",
                        json!({"response": response, "time": format!("{}Z", civil_text(DateTime::now().unix))}),
                    );
                }
            }
            x.seq = seq;
            x.change = seq;
        }
    }
    (202, Value::Null)
}

/// A delta query over `coll` (`window`: a calendar view's range, listing instances).
fn delta(st: &PimState, r: &Req<'_>, coll: &str, window: Option<(i64, i64)>) -> (u16, Value) {
    let kind = st.colls.get(coll).map_or(Kind::Contacts, |c| c.kind);
    if kind == Kind::Calendar && window.is_none() {
        return err(400, "ErrorInvalidParameter", "startDateTime and endDateTime are required");
    }
    // `$skiptoken=from.upto.offset` continues a round; `$deltatoken=n` starts one after change n.
    let (from, upto, offset) = match (r.q.get("$skiptoken"), r.q.get("$deltatoken")) {
        (Some(s), _) => {
            let p: Vec<u64> = s.split('.').filter_map(|x| x.parse().ok()).collect();
            match p.as_slice() {
                [a, b, c] => (*a, *b, usize::try_from(*c).unwrap_or(0)),
                _ => return err(400, "ErrorInvalidParameter", "bad skip token"),
            }
        }
        (None, Some(t)) => match t.parse::<u64>() {
            Ok(t) if t >= st.min_token && t <= st.seq => (t, st.seq, 0),
            _ => return err(410, "SyncStateNotFound", "The sync state generation is not found."),
        },
        (None, None) => (0, st.seq, 0),
    };
    let mut entries: Vec<Value> = Vec::new();
    for (id, o) in st.objs.iter().filter(|(_, o)| o.coll == coll && o.seq > from && o.seq <= upto) {
        if o.deleted {
            if from > 0 {
                entries.push(json!({"id": id, "@removed": {"reason": "deleted"}}));
            }
            continue;
        }
        match window {
            None => entries.push(st.json_of(id, o)),
            Some((a, b)) => {
                if o.master.is_some() {
                    // Listed with its series.
                    continue;
                }
                entries.extend(st.instances(id, a, b));
            }
        }
    }
    let total = entries.len();
    let page: Vec<Value> = entries.into_iter().skip(offset).take(r.page).collect();
    let mut keep: Vec<String> =
        r.q.iter()
            .filter(|(k, _)| !matches!(k.as_str(), "$skiptoken" | "$deltatoken"))
            .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
            .collect();
    let next = offset.saturating_add(r.page);
    let mut v = json!({"value": page});
    if next < total {
        keep.push(format!("$skiptoken={from}.{upto}.{next}"));
        put(&mut v, "@odata.nextLink", json!(format!("{}/{}?{}", r.base, r.path, keep.join("&"))));
    } else {
        keep.push(format!("$deltatoken={upto}"));
        put(&mut v, "@odata.deltaLink", json!(format!("{}/{}?{}", r.base, r.path, keep.join("&"))));
    }
    (200, v)
}

// ---------------------------------------------------------------------------------------------------------
// Test controls
// ---------------------------------------------------------------------------------------------------------

impl PimState {
    pub(crate) fn share_calendar(&mut self, owner: &str, with: &str, edit: bool) {
        self.ensure(owner);
        self.ensure(with);
        self.grants.insert((key(owner), key(with)), edit);
    }

    pub(crate) fn expire_deltas(&mut self) {
        self.min_token = self.seq.saturating_add(1);
        self.seq = self.seq.saturating_add(1);
    }

    /// Every live object of `user` of `kind` ("contacts", "events", "tasks") as the service lists it, with
    /// the name of its collection as `_collection`.
    pub(crate) fn objects_json(&mut self, user: &str, kind: &str) -> Vec<Value> {
        self.ensure(user);
        let k = match kind {
            "contacts" => Kind::Contacts,
            "events" => Kind::Calendar,
            _ => Kind::Todo,
        };
        let u = key(user);
        self.objs
            .iter()
            .filter(|(_, o)| !o.deleted)
            .filter(|(_, o)| self.colls.get(&o.coll).is_some_and(|c| c.owner == u && c.kind == k))
            .map(|(id, o)| {
                let mut v = self.json_of(id, o);
                let name = self.colls.get(&o.coll).map(|c| c.name.clone()).unwrap_or_default();
                put(&mut v, "_collection", json!(name));
                put(
                    &mut v,
                    "_cancelled",
                    json!(o.cancelled.iter().map(|t| format!("{}Z", civil_text(*t))).collect::<Vec<_>>()),
                );
                v
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_and_expansion() {
        // 2026-07-06 09:00 in New York (EDT) is 13:00 UTC; in January (EST) 14:00 UTC.
        let summer = parse_civil("2026-07-06T09:00:00").unwrap();
        assert_eq!(utc_of_local(summer, "Eastern Standard Time") - summer, 4 * 3600);
        let winter = parse_civil("2026-01-05T09:00:00").unwrap();
        assert_eq!(utc_of_local(winter, "America/New_York") - winter, 5 * 3600);
        assert_eq!(utc_of_local(summer, "Europe/Berlin") - summer, -2 * 3600);
        let ev = json!({
            "start": {"dateTime": "2026-10-05T09:00:00", "timeZone": "Eastern Standard Time"},
            "end": {"dateTime": "2026-10-05T10:00:00", "timeZone": "Eastern Standard Time"},
            "recurrence": {"pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["monday"]},
                           "range": {"type": "numbered", "startDate": "2026-10-05", "numberOfOccurrences": 6}}
        });
        let all = expand(&ev, 0, i64::MAX / 2);
        assert_eq!(all.len(), 6);
        // Across the end of daylight saving time the wall-clock time stays 09:00.
        assert_eq!(all[0], parse_civil("2026-10-05T13:00:00").unwrap());
        assert_eq!(all[5], parse_civil("2026-11-09T14:00:00").unwrap());
        let monthly = json!({
            "start": {"dateTime": "2026-01-30T12:00:00", "timeZone": "UTC"},
            "end": {"dateTime": "2026-01-30T13:00:00", "timeZone": "UTC"},
            "recurrence": {"pattern": {"type": "relativeMonthly", "interval": 1, "daysOfWeek": ["friday"], "index": "last"},
                           "range": {"type": "endDate", "startDate": "2026-01-01", "endDate": "2026-03-31"}}
        });
        assert_eq!(expand(&monthly, 0, i64::MAX / 2).len(), 3);
        assert_eq!(split_occurrence(&occurrence_id("AAev1", 42)), Some(("AAev1", 42)));
    }
}
