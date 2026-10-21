//! VEVENT/VTODO ↔ model mapping.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use mailcraft_mime::Mailbox;
use mailcraft_model::calendar::{
    AttendeeRole, ItemAttachment, MAX_ITEM_ATTACHMENT_BYTES, MAX_ITEM_ATTACHMENTS, ResponseStatus,
};
use mailcraft_model::{
    Appointment, Attendee, BusyStatus, DateTime, Importance, Recurrence, Task, TaskPriority, TaskStatus,
};

use super::parse::Component;
use super::rrule::Rrule;
use super::tz::{TimeZone, ZoneLookup, Zones, builtin, builtin_name, equivalent_builtin, fixed_tzid};
use super::value::{self, DAY, TimeValue};
use super::{Event, Method, PRODID};
use crate::text::{CategoryList, ContentLine, MAX_CATEGORIES, escape, split_unescaped};

/// Most attendees / EXDATEs / RDATEs kept per event.
const MAX_LIST: usize = 2_000;
/// Longest reminder lead time kept (minutes): one year.
const MAX_REMINDER_MIN: i64 = 525_600;
/// Most zone lookups (with their listed onsets) kept while reading one file.
const MAX_LOOKUPS: usize = 16;
/// Most TZID → IANA zone answers kept while reading one file.
const MAX_ZONE_NAMES: usize = 1_000;

pub(crate) struct Ctx<'a> {
    pub zones: &'a Zones,
    pub local_offset: i32,
    /// The file's zone definitions by lower-cased TZID (first definition wins), so a TZID lookup does not
    /// scan them all.
    defined: HashMap<String, usize>,
    /// Zone lookups by lower-cased TZID (`None`: the TZID names no known zone).
    lookups: RefCell<HashMap<String, Option<ZoneLookup<'a>>>>,
    /// The built-in (IANA) zone each TZID stands for, by lower-cased TZID (`None`: none, or a fixed offset).
    named: RefCell<HashMap<String, Option<&'static str>>>,
}

fn zone_key(tzid: &str) -> String {
    tzid.trim().trim_matches('"').to_ascii_lowercase()
}

impl<'a> Ctx<'a> {
    pub fn new(zones: &'a Zones, local_offset: i32) -> Self {
        let mut defined = HashMap::new();
        for (i, z) in zones.defined.iter().enumerate() {
            defined.entry(z.tzid.to_ascii_lowercase()).or_insert(i);
        }
        Ctx {
            zones,
            local_offset,
            defined,
            lookups: RefCell::new(HashMap::new()),
            named: RefCell::new(HashMap::new()),
        }
    }

    /// The IANA zone an event in `tzid` keeps: the built-in zone of that name, or for a zone the file defines
    /// under another name ("Eastern Standard Time"), the built-in zone with the same offsets around `year`.
    /// `None` for fixed offsets, zones without daylight saving time and unknown zones.
    pub fn named_zone(&self, tzid: &str, year: i64) -> Option<&'static str> {
        let key = zone_key(tzid);
        if let Ok(map) = self.named.try_borrow()
            && let Some(known) = map.get(&key)
        {
            return *known;
        }
        let defined = self.defined.get(&key).and_then(|&i| self.zones.defined.get(i));
        let found = match defined {
            // The file's own rules decide; its name wins when it is the matching built-in zone.
            Some(z) => equivalent_builtin(z, year).map(|eq| match builtin_name(tzid) {
                Some(own) if builtin(own).is_some_and(|b| equivalent_builtin(&b, year) == Some(eq)) => own,
                _ => eq,
            }),
            None => builtin_name(tzid),
        };
        if let Ok(mut map) = self.named.try_borrow_mut()
            && map.len() < MAX_ZONE_NAMES
        {
            map.insert(key, found);
        }
        found
    }

    /// [`Zones::get`] through the index.
    fn zone(&self, tzid: &str) -> Option<Cow<'a, TimeZone>> {
        let zones: &'a Zones = self.zones;
        match self.defined.get(&zone_key(tzid)).and_then(|&i| zones.defined.get(i)) {
            Some(z) => Some(Cow::Borrowed(z)),
            None => builtin(tzid.trim().trim_matches('"')).map(Cow::Owned),
        }
    }

    /// True when `tzid` names a zone the file defines or a built-in / fixed-offset one.
    pub fn knows_zone(&self, tzid: &str) -> bool {
        if let Ok(map) = self.lookups.try_borrow()
            && let Some(known) = map.get(&zone_key(tzid))
        {
            return known.is_some();
        }
        self.zone(tzid).is_some()
    }

    /// Local time `secs` in zone `tzid` → (unix seconds, offset in force); `None` for an unknown zone.
    fn zone_resolve(&self, tzid: &str, secs: i64) -> Option<(i64, i32)> {
        let key = zone_key(tzid);
        let Ok(mut map) = self.lookups.try_borrow_mut() else {
            return self.zone(tzid).map(|z| ZoneLookup::new(z).resolve_local(secs));
        };
        if !map.contains_key(&key) && map.len() >= MAX_LOOKUPS {
            map.clear();
        }
        map.entry(key)
            .or_insert_with(|| self.zone(tzid).map(ZoneLookup::new))
            .as_ref()
            .map(|z| z.resolve_local(secs))
    }

    pub fn resolve(&self, tv: &TimeValue) -> DateTime {
        let lo = self.local_offset;
        let local = |secs: i64, off: i32| DateTime::from_unix(secs.saturating_sub(i64::from(off) * 60), off);
        match tv {
            TimeValue::Date(secs) => local(*secs, lo),
            TimeValue::Utc(u) => DateTime::from_unix(*u, 0),
            TimeValue::Local { secs, tzid } => {
                match tzid.as_deref().and_then(|t| self.zone_resolve(t, *secs)) {
                    Some((unix, off)) => DateTime::from_unix(unix, off),
                    None => local(*secs, lo),
                }
            }
        }
    }

    fn time_value(p: &ContentLine) -> Option<TimeValue> {
        value::parse_time_value(&p.value, p.param("VALUE"), p.param("TZID"))
    }

    fn time(&self, c: &Component, name: &str) -> Option<(TimeValue, DateTime)> {
        let tv = Self::time_value(c.prop(name)?)?;
        let dt = self.resolve(&tv);
        Some((tv, dt))
    }

    /// EXDATE/RDATE style comma lists over every `name` property (PERIOD values use their start).
    fn time_list(&self, c: &Component, name: &str) -> Vec<DateTime> {
        let mut out = Vec::new();
        for p in c.props_named(name) {
            for v in p.value.split(',') {
                let v = v.split('/').next().unwrap_or(v);
                if out.len() >= MAX_LIST {
                    return out;
                }
                if let Some(tv) = value::parse_time_value(v, p.param("VALUE"), p.param("TZID")) {
                    out.push(self.resolve(&tv));
                }
            }
        }
        out
    }
}

/// `mailto:` value + CN parameter → mailbox.
pub(crate) fn mailbox_of(p: &ContentLine) -> Option<Mailbox> {
    let v = p.value.trim();
    let addr = match v.get(..7) {
        Some(s) if s.eq_ignore_ascii_case("mailto:") => v.get(7..).unwrap_or(""),
        _ => v,
    };
    let addr = addr.trim().replace("%40", "@");
    if addr.is_empty() {
        return None;
    }
    let name = p.param("CN").map(str::trim).filter(|n| !n.is_empty());
    Some(Mailbox::new(name, &addr))
}

pub(crate) fn attendee_of(p: &ContentLine) -> Option<Attendee> {
    let mailbox = mailbox_of(p)?;
    let up = |name: &str| p.param(name).map(|v| v.trim().to_ascii_uppercase()).unwrap_or_default();
    let cutype = up("CUTYPE");
    let role = if cutype == "RESOURCE" || cutype == "ROOM" {
        AttendeeRole::Resource
    } else {
        match up("ROLE").as_str() {
            "OPT-PARTICIPANT" | "NON-PARTICIPANT" => AttendeeRole::Optional,
            _ => AttendeeRole::Required,
        }
    };
    let response = match up("PARTSTAT").as_str() {
        "ACCEPTED" => ResponseStatus::Accepted,
        "TENTATIVE" => ResponseStatus::Tentative,
        "DECLINED" => ResponseStatus::Declined,
        _ => ResponseStatus::None,
    };
    Some(Attendee { mailbox, role, response })
}

fn categories_of(c: &Component) -> Vec<String> {
    let mut out = CategoryList::default();
    for p in c.props_named("CATEGORIES") {
        if out.is_full() {
            break;
        }
        for cat in split_unescaped(&p.value, ',', MAX_CATEGORIES) {
            out.add(&cat);
        }
    }
    out.into_vec()
}

/// Trigger time of the first VALARM, given the owner's start and end.
fn alarm_time(
    c: &Component,
    ctx: &Ctx<'_>,
    start: Option<DateTime>,
    end: Option<DateTime>,
) -> Option<DateTime> {
    let alarm = c.children_named("VALARM").next()?;
    let p = alarm.prop("TRIGGER")?;
    let absolute = p.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME"))
        || (!p.value.contains(['P', 'p']) && p.value.contains(['T', 't']));
    if absolute {
        let tv = value::parse_time_value(&p.value, Some("DATE-TIME"), p.param("TZID"))?;
        return Some(ctx.resolve(&tv));
    }
    let dur = value::parse_duration(&p.value)?;
    let related_end = p.param("RELATED").is_some_and(|r| r.eq_ignore_ascii_case("END"));
    let base = if related_end { end.or(start) } else { start.or(end) }?;
    Some(DateTime::from_unix(base.unix.saturating_add(dur), base.offset_min))
}

fn text_or_empty(c: &Component, name: &str) -> String {
    c.text(name).map(|s| s.replace("\r\n", "\n")).unwrap_or_default()
}

/// FNV-1a over a component's property lines (stable fallback UID).
fn content_hash(c: &Component) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in &c.props {
        for b in p.name.bytes().chain(p.value.bytes()) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

pub(crate) fn event_from(c: &Component, ctx: &Ctx<'_>) -> Option<Event> {
    let (start_tv, start) = ctx.time(c, "DTSTART")?;
    let all_day = matches!(start_tv, TimeValue::Date(_));
    let (start_local, tzid) = match &start_tv {
        TimeValue::Date(s) => (*s, None),
        TimeValue::Utc(u) => (*u, None),
        TimeValue::Local { secs, tzid } => (*secs, tzid.clone().filter(|t| ctx.knows_zone(t))),
    };
    let dtend = ctx.time(c, "DTEND");
    let end_tzid = match dtend.as_ref().map(|(tv, _)| tv) {
        Some(TimeValue::Local { tzid: Some(t), .. }) if ctx.knows_zone(t) => Some(t.clone()),
        _ => None,
    };
    let end = if let Some((_, e)) = dtend {
        e
    } else if let Some(d) = c.prop("DURATION").and_then(|p| value::parse_duration(&p.value)) {
        DateTime::from_unix(start.unix.saturating_add(d), start.offset_min)
    } else if all_day {
        DateTime::from_unix(start.unix.saturating_add(DAY), start.offset_min)
    } else {
        start
    };
    let end = if end.unix < start.unix { start } else { end };
    let upper = |name: &str| c.text(name).map(|v| v.trim().to_ascii_uppercase()).unwrap_or_default();
    let status = upper("STATUS");
    let show_as = match upper("X-MICROSOFT-CDO-BUSYSTATUS").as_str() {
        "FREE" => BusyStatus::Free,
        "TENTATIVE" => BusyStatus::Tentative,
        "OOF" => BusyStatus::OutOfOffice,
        "WORKINGELSEWHERE" => BusyStatus::WorkingElsewhere,
        "BUSY" => BusyStatus::Busy,
        _ if upper("TRANSP") == "TRANSPARENT" => BusyStatus::Free,
        _ if status == "TENTATIVE" => BusyStatus::Tentative,
        _ => BusyStatus::Busy,
    };
    let reminder_min = alarm_time(c, ctx, Some(start), Some(end)).map(|t| {
        let mins = start.unix.saturating_sub(t.unix).div_euclid(60).clamp(0, MAX_REMINDER_MIN);
        u32::try_from(mins).unwrap_or(0)
    });
    let organizer = c.prop("ORGANIZER").and_then(mailbox_of);
    let attendees: Vec<Attendee> = c.props_named("ATTENDEE").take(MAX_LIST).filter_map(attendee_of).collect();
    let recurrence = c.prop("RRULE").and_then(|p| {
        let v = p.value.trim();
        Rrule::parse(v).ok().map(|_| Recurrence { rrule: v.to_owned(), exdates: ctx.time_list(c, "EXDATE") })
    });
    let class = upper("CLASS");
    let uid = c
        .text("UID")
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| format!("{:016x}@mailcraft.import", content_hash(c)));
    let recurrence_id = ctx.time(c, "RECURRENCE-ID").map(|(_, t)| t);
    // The IANA zone the times were entered in (recurrences then follow its daylight-saving rules).
    let year = value::civil(start_local).0;
    let tz =
        if all_day { None } else { tzid.as_deref().and_then(|t| ctx.named_zone(t, year)).map(str::to_owned) };
    // An end written in another zone than the start (Time Zones in the item window).
    let end_tz = if all_day {
        None
    } else {
        end_tzid
            .as_deref()
            .and_then(|t| ctx.named_zone(t, year))
            .map(str::to_owned)
            .filter(|e| Some(e) != tz.as_ref())
    };
    let importance = match c.text("PRIORITY").and_then(|p| p.trim().parse::<u8>().ok()) {
        Some(1..=4) => Importance::High,
        Some(6..=9) => Importance::Low,
        _ => Importance::Normal,
    };
    let appointment = Appointment {
        uid,
        // An override replaces one instance; it never carries its own rule.
        recurrence: if recurrence_id.is_some() { None } else { recurrence },
        recurrence_id,
        subject: text_or_empty(c, "SUMMARY"),
        location: text_or_empty(c, "LOCATION"),
        start,
        end,
        all_day,
        show_as,
        reminder_min,
        is_meeting: !attendees.is_empty(),
        organizer,
        attendees,
        categories: categories_of(c),
        private: class == "PRIVATE" || class == "CONFIDENTIAL",
        body: text_or_empty(c, "DESCRIPTION"),
        body_html: html_description(c),
        attachments: attachments_of(c),
        importance,
        tz,
        end_tz,
        ..Default::default()
    };
    Some(Event {
        appointment,
        tzid,
        start_local,
        sequence: c.text("SEQUENCE").and_then(|s| s.trim().parse().ok()).unwrap_or(0),
        status: (!status.is_empty()).then_some(status),
        recurrence_id,
        rdates: ctx.time_list(c, "RDATE"),
    })
}

/// Most bytes of formatted notes kept from one event.
const MAX_HTML_NOTES: usize = 1024 * 1024;

/// The formatted notes of an event: `X-ALT-DESC;FMTTYPE=text/html` ([MS-OXCICAL] section 2.1.3.1.1.20.3),
/// or an RFC 9073 `STYLED-DESCRIPTION` with `FMTTYPE=text/html`.
fn html_description(c: &Component) -> Option<String> {
    let html = |name: &'static str| {
        c.props_named(name)
            .find(|p| p.param("FMTTYPE").is_some_and(|f| f.trim().eq_ignore_ascii_case("text/html")))
    };
    let p = html("X-ALT-DESC").or_else(|| html("STYLED-DESCRIPTION"))?;
    let text = crate::text::unescape(&p.value);
    (!text.trim().is_empty() && text.len() <= MAX_HTML_NOTES).then_some(text)
}

/// Files carried inline by an event (`ATTACH;VALUE=BINARY;ENCODING=BASE64`, RFC 5545 section 3.8.1.1), up to
/// the per-item limits. Attachments by URI are not fetched.
fn attachments_of(c: &Component) -> Vec<ItemAttachment> {
    let mut out: Vec<ItemAttachment> = Vec::new();
    let mut total = 0usize;
    for p in c.props_named("ATTACH") {
        if out.len() >= MAX_ITEM_ATTACHMENTS {
            break;
        }
        let base64 = p.param("ENCODING").is_some_and(|e| e.trim().eq_ignore_ascii_case("BASE64"));
        if !base64 {
            continue;
        }
        let data = mailcraft_mime::transfer::decode_base64(p.value.as_bytes());
        if data.is_empty() || total.saturating_add(data.len()) > MAX_ITEM_ATTACHMENT_BYTES {
            continue;
        }
        total = total.saturating_add(data.len());
        let name = ["X-FILENAME", "FILENAME", "X-APPLE-FILENAME"]
            .iter()
            .find_map(|n| p.param(n))
            .map(|n| n.trim().chars().filter(|c| !c.is_control()).take(200).collect::<String>())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("attachment{}", out.len().saturating_add(1)));
        let mime_type = p.param("FMTTYPE").map(|m| m.trim().to_ascii_lowercase()).unwrap_or_default();
        out.push(ItemAttachment { filename: name, mime_type, data: data.into() });
    }
    out
}

/// Overridden instances (RECURRENCE-ID) become EXDATEs on their series.
pub(crate) fn link_overrides(events: &mut [Event]) {
    let overrides: Vec<(String, DateTime)> = events
        .iter()
        .filter_map(|e| e.recurrence_id.map(|r| (e.appointment.uid.clone(), r)))
        .take(MAX_LIST * 10)
        .collect();
    if overrides.is_empty() {
        return;
    }
    // The series (first event without RECURRENCE-ID) of each UID.
    let mut series: HashMap<String, usize> = HashMap::new();
    for (i, e) in events.iter().enumerate() {
        if e.recurrence_id.is_none() {
            series.entry(e.appointment.uid.clone()).or_insert(i);
        }
    }
    for (uid, rid) in overrides {
        let master = series.get(&uid).and_then(|&i| events.get_mut(i));
        if let Some(rec) = master.and_then(|m| m.appointment.recurrence.as_mut())
            && !rec.exdates.iter().any(|x| x.unix == rid.unix)
            && rec.exdates.len() < MAX_LIST
        {
            rec.exdates.push(rid);
        }
    }
}

pub(crate) fn task_from(c: &Component, ctx: &Ctx<'_>) -> Task {
    let start = ctx.time(c, "DTSTART").map(|(_, t)| t);
    let due = ctx.time(c, "DUE").map(|(_, t)| t).or_else(|| {
        let d = c.prop("DURATION").and_then(|p| value::parse_duration(&p.value))?;
        start.map(|s| DateTime::from_unix(s.unix.saturating_add(d), s.offset_min))
    });
    let upper = |name: &str| c.text(name).map(|v| v.trim().to_ascii_uppercase()).unwrap_or_default();
    let status = match (upper("X-MAILCRAFT-STATUS").as_str(), upper("STATUS").as_str()) {
        ("WAITING", _) => TaskStatus::WaitingOnSomeoneElse,
        ("DEFERRED", _) | (_, "CANCELLED") => TaskStatus::Deferred,
        (_, "COMPLETED") => TaskStatus::Completed,
        (_, "IN-PROCESS") => TaskStatus::InProgress,
        _ => TaskStatus::NotStarted,
    };
    let priority = match c.text("PRIORITY").and_then(|p| p.trim().parse::<u8>().ok()) {
        Some(1..=4) => TaskPriority::High,
        Some(6..=9) => TaskPriority::Low,
        _ => TaskPriority::Normal,
    };
    let completed = ctx.time(c, "COMPLETED").map(|(_, t)| t);
    let mut percent =
        c.text("PERCENT-COMPLETE").and_then(|p| p.trim().parse::<u8>().ok()).unwrap_or(0).min(100);
    if status == TaskStatus::Completed {
        percent = 100;
    }
    Task {
        subject: text_or_empty(c, "SUMMARY"),
        start,
        due,
        status,
        priority,
        percent_complete: percent,
        reminder: alarm_time(c, ctx, start, due),
        body: text_or_empty(c, "DESCRIPTION"),
        categories: categories_of(c),
        completed,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------------------------------------
// Writing

fn line(name: &str, params: Vec<(&str, String)>, value: impl Into<String>) -> ContentLine {
    ContentLine {
        group: None,
        name: name.to_owned(),
        params: params.into_iter().map(|(n, v)| (n.to_owned(), vec![v])).collect(),
        value: value.into(),
    }
}

fn no_breaks(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

pub(crate) fn calendar_shell(method: Option<&Method>) -> Component {
    let mut cal = Component::new("VCALENDAR");
    cal.push("PRODID", vec![], PRODID);
    cal.push("VERSION", vec![], "2.0");
    cal.push("CALSCALE", vec![], "GREGORIAN");
    if let Some(m) = method {
        cal.push("METHOD", vec![], m.as_str());
    }
    cal
}

/// UTC DATE-TIME text (`20261012T143000Z`).
pub(crate) fn utc_text(t: DateTime) -> String {
    format!("{}Z", value::format_date_time(t.unix))
}

/// The time zones a written calendar refers to, each written once as a VTIMEZONE: fixed offsets
/// (`UTC-04:00`) and named built-in zones (`America/Toronto`).
#[derive(Debug, Default)]
pub(crate) struct UsedZones {
    pub fixed: BTreeSet<i32>,
    pub named: BTreeMap<String, TimeZone>,
}

impl UsedZones {
    /// The VTIMEZONE components, fixed offsets first.
    pub fn components(self) -> Vec<Component> {
        let mut out: Vec<Component> = self.fixed.into_iter().map(TimeZone::fixed_component).collect();
        out.extend(self.named.into_values().map(|z| z.to_component()));
        out
    }
}

/// DTSTART-style line for `t` written in `offset` (UTC when 0, else a fixed-offset TZID).
fn time_line(name: &str, t: DateTime, all_day: bool, offset: i32, zones: &mut UsedZones) -> ContentLine {
    let local = t.unix.saturating_add(i64::from(offset) * 60);
    if all_day {
        return line(name, vec![("VALUE", "DATE".into())], value::format_date(local));
    }
    if offset == 0 {
        return line(name, vec![], utc_text(t));
    }
    zones.fixed.insert(offset);
    line(name, vec![("TZID", fixed_tzid(offset))], value::format_date_time(local))
}

/// DTSTART-style line for `t` as wall-clock time in named zone `z` (TZID = its name).
fn zoned_line(name: &str, t: DateTime, z: &ZoneLookup<'_>, zones: &mut UsedZones) -> ContentLine {
    let tzid = z.zone.tzid.clone();
    let local = t.unix.saturating_add(i64::from(z.offset_at_utc(t.unix)) * 60);
    if !zones.named.contains_key(&tzid) {
        zones.named.insert(tzid.clone(), z.zone.clone().into_owned());
    }
    line(name, vec![("TZID", tzid)], value::format_date_time(local))
}

fn mailto(m: &Mailbox) -> String {
    format!("mailto:{}", no_breaks(m.addr.trim()))
}

fn cn(m: &Mailbox) -> Vec<(&'static str, String)> {
    match m.name.as_deref().map(no_breaks).filter(|n| !n.trim().is_empty()) {
        Some(n) => vec![("CN", n)],
        None => vec![],
    }
}

pub(crate) fn partstat(r: ResponseStatus) -> &'static str {
    match r {
        ResponseStatus::None => "NEEDS-ACTION",
        ResponseStatus::Accepted => "ACCEPTED",
        ResponseStatus::Tentative => "TENTATIVE",
        ResponseStatus::Declined => "DECLINED",
    }
}

pub(crate) fn attendee_line(a: &Attendee, rsvp: bool) -> ContentLine {
    let mut params = cn(&a.mailbox);
    match a.role {
        AttendeeRole::Required => params.push(("ROLE", "REQ-PARTICIPANT".into())),
        AttendeeRole::Optional => params.push(("ROLE", "OPT-PARTICIPANT".into())),
        AttendeeRole::Resource => {
            params.push(("CUTYPE", "RESOURCE".into()));
            params.push(("ROLE", "NON-PARTICIPANT".into()));
        }
    }
    params.push(("PARTSTAT", partstat(a.response).into()));
    if rsvp && a.response == ResponseStatus::None {
        params.push(("RSVP", "TRUE".into()));
    }
    line("ATTENDEE", params, mailto(&a.mailbox))
}

/// Per-message choices for a written VEVENT.
#[derive(Clone, Debug, Default)]
pub(crate) struct EventOut<'a> {
    pub sequence: u32,
    pub status: Option<&'a str>,
    /// iTIP REPLY: only this attendee, with this response.
    pub only_attendee: Option<(&'a Mailbox, ResponseStatus)>,
    pub no_alarm: bool,
    pub rsvp: bool,
    /// Leave the attached files out (an invitation carries them as message attachments instead).
    pub no_attachments: bool,
    /// Stored on a CalDAV server, not sent: ORGANIZER and ATTENDEE carry SCHEDULE-AGENT=CLIENT (RFC 6638
    /// section 7.1), so a server that schedules does not mail the attendees itself. MailCraft sends the
    /// invitations, updates and replies.
    pub schedule_client: bool,
}

pub(crate) fn event_component(
    a: &Appointment,
    o: &EventOut<'_>,
    now: DateTime,
    offsets: &mut UsedZones,
) -> Component {
    let mut c = Component::new("VEVENT");
    let off = a.start.offset_min.clamp(-23 * 60 - 59, 23 * 60 + 59);
    // A timed appointment entered in a named zone is written in it (TZID + VTIMEZONE), so other calendars
    // repeat it at the same wall-clock time across daylight-saving changes.
    let named = if a.all_day { None } else { a.zone().and_then(builtin_name).and_then(builtin) };
    let named = named.map(|z| ZoneLookup::new(Cow::Owned(z)));
    let time_line = |name: &str, t: DateTime, all_day: bool, off: i32, zones: &mut UsedZones| match &named {
        Some(z) => zoned_line(name, t, z, zones),
        None => time_line(name, t, all_day, off, zones),
    };
    // An end in another zone (Time Zones in the item window) is written in that zone.
    let end_named = match (a.all_day, a.end_tz.as_deref().map(str::trim).filter(|z| !z.is_empty())) {
        (false, Some(z)) if Some(z) != a.zone() => {
            builtin_name(z).and_then(builtin).map(|z| ZoneLookup::new(Cow::Owned(z)))
        }
        _ => None,
    };
    let uid = if a.uid.trim().is_empty() {
        format!("{:016x}@mailcraft.local", uid_hash(a))
    } else {
        no_breaks(&a.uid)
    };
    c.props.push(line("UID", vec![], escape(&uid)));
    c.props.push(line("DTSTAMP", vec![], utc_text(now)));
    if o.sequence > 0 {
        c.props.push(line("SEQUENCE", vec![], o.sequence.to_string()));
    }
    c.props.push(time_line("DTSTART", a.start, a.all_day, off, offsets));
    let end = if a.end.unix < a.start.unix { a.start } else { a.end };
    match &end_named {
        Some(z) => c.props.push(zoned_line("DTEND", end, z, offsets)),
        None => c.props.push(time_line("DTEND", end, a.all_day, off, offsets)),
    }
    c.props.push(line("SUMMARY", vec![], escape(&a.subject)));
    if !a.location.is_empty() {
        c.props.push(line("LOCATION", vec![], escape(&a.location)));
    }
    if !a.body.is_empty() {
        c.props.push(line("DESCRIPTION", vec![], escape(&a.body)));
    }
    if let Some(html) = a.body_html.as_deref().filter(|h| !h.trim().is_empty() && h.len() <= MAX_HTML_NOTES) {
        c.props.push(line("X-ALT-DESC", vec![("FMTTYPE", "text/html".into())], escape(html)));
    }
    match a.importance {
        Importance::High => c.props.push(line("PRIORITY", vec![], "1")),
        Importance::Low => c.props.push(line("PRIORITY", vec![], "9")),
        Importance::Normal => {}
    }
    if !o.no_attachments {
        for f in a.attachments.iter().take(MAX_ITEM_ATTACHMENTS) {
            let mut params = vec![
                ("ENCODING", "BASE64".to_owned()),
                ("VALUE", "BINARY".to_owned()),
                ("X-FILENAME", no_breaks(&f.filename)),
            ];
            if !f.mime_type.trim().is_empty() {
                params.insert(0, ("FMTTYPE", no_breaks(f.mime_type.trim())));
            }
            c.props.push(line("ATTACH", params, mailcraft_mime::transfer::encode_base64_flat(&f.data)));
        }
    }
    if let Some(s) = o.status {
        c.props.push(line("STATUS", vec![], s));
    }
    if a.private {
        c.props.push(line("CLASS", vec![], "PRIVATE"));
    }
    let transp = if a.show_as == BusyStatus::Free { "TRANSPARENT" } else { "OPAQUE" };
    c.props.push(line("TRANSP", vec![], transp));
    let busy = match a.show_as {
        BusyStatus::Free => "FREE",
        BusyStatus::WorkingElsewhere => "WORKINGELSEWHERE",
        BusyStatus::Tentative => "TENTATIVE",
        BusyStatus::Busy => "BUSY",
        BusyStatus::OutOfOffice => "OOF",
    };
    c.props.push(line("X-MICROSOFT-CDO-BUSYSTATUS", vec![], busy));
    let client = |mut l: ContentLine| {
        if o.schedule_client {
            l.params.push(("SCHEDULE-AGENT".to_owned(), vec!["CLIENT".to_owned()]));
        }
        l
    };
    if let Some(org) = &a.organizer {
        c.props.push(client(line("ORGANIZER", cn(org), mailto(org))));
    }
    match o.only_attendee {
        Some((who, response)) => {
            let found = a.attendees.iter().find(|x| x.mailbox.same_address(who));
            let att = Attendee {
                mailbox: found.map(|f| f.mailbox.clone()).unwrap_or_else(|| who.clone()),
                role: found.map(|f| f.role).unwrap_or_default(),
                response,
            };
            c.props.push(client(attendee_line(&att, false)));
        }
        None => {
            for att in &a.attendees {
                c.props.push(client(attendee_line(att, o.rsvp)));
            }
        }
    }
    if let Some(rid) = a.recurrence_id.filter(|_| a.recurrence.is_none()) {
        c.props.push(time_line("RECURRENCE-ID", rid, a.all_day, off, offsets));
    }
    if let Some(rec) = &a.recurrence
        && Rrule::parse(&rec.rrule).is_ok()
    {
        c.props.push(line("RRULE", vec![], no_breaks(rec.rrule.trim())));
        for x in rec.exdates.iter().take(MAX_LIST) {
            c.props.push(time_line("EXDATE", *x, a.all_day, off, offsets));
        }
    }
    if !a.categories.is_empty() {
        let cats: Vec<String> = a.categories.iter().map(|s| escape(s)).collect();
        c.props.push(line("CATEGORIES", vec![], cats.join(",")));
    }
    if let Some(m) = a.reminder_min
        && !o.no_alarm
    {
        let mut alarm = Component::new("VALARM");
        alarm.push("ACTION", vec![], "DISPLAY");
        alarm.push("DESCRIPTION", vec![], "Reminder");
        alarm.push("TRIGGER", vec![], value::format_duration(-(i64::from(m) * 60)));
        c.children.push(alarm);
    }
    c
}

fn uid_hash(a: &Appointment) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in a.subject.bytes().chain(a.start.unix.to_be_bytes()).chain(a.id.0.to_be_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

pub(crate) fn task_component(t: &Task, now: DateTime, offsets: &mut UsedZones) -> Component {
    let mut c = Component::new("VTODO");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in t.subject.bytes().chain(t.id.0.to_be_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    c.props.push(line("UID", vec![], format!("task-{}-{h:016x}@mailcraft.local", t.id.0)));
    c.props.push(line("DTSTAMP", vec![], utc_text(now)));
    c.props.push(line("SUMMARY", vec![], escape(&t.subject)));
    if let Some(s) = t.start {
        c.props.push(time_line("DTSTART", s, false, s.offset_min, offsets));
    }
    if let Some(d) = t.due {
        c.props.push(time_line("DUE", d, false, d.offset_min, offsets));
    }
    let (status, x) = match t.status {
        TaskStatus::NotStarted => ("NEEDS-ACTION", None),
        TaskStatus::InProgress => ("IN-PROCESS", None),
        TaskStatus::Completed => ("COMPLETED", None),
        TaskStatus::WaitingOnSomeoneElse => ("IN-PROCESS", Some("WAITING")),
        TaskStatus::Deferred => ("NEEDS-ACTION", Some("DEFERRED")),
    };
    c.props.push(line("STATUS", vec![], status));
    if let Some(x) = x {
        c.props.push(line("X-MAILCRAFT-STATUS", vec![], x));
    }
    let prio = match t.priority {
        TaskPriority::High => "1",
        TaskPriority::Normal => "5",
        TaskPriority::Low => "9",
    };
    c.props.push(line("PRIORITY", vec![], prio));
    c.props.push(line("PERCENT-COMPLETE", vec![], t.percent_complete.min(100).to_string()));
    if let Some(done) = t.completed {
        c.props.push(line("COMPLETED", vec![], utc_text(done)));
    }
    if !t.body.is_empty() {
        c.props.push(line("DESCRIPTION", vec![], escape(&t.body)));
    }
    if !t.categories.is_empty() {
        let cats: Vec<String> = t.categories.iter().map(|s| escape(s)).collect();
        c.props.push(line("CATEGORIES", vec![], cats.join(",")));
    }
    if let Some(r) = t.reminder {
        let mut alarm = Component::new("VALARM");
        alarm.push("ACTION", vec![], "DISPLAY");
        alarm.push("DESCRIPTION", vec![], "Reminder");
        alarm.push("TRIGGER", vec![("VALUE", "DATE-TIME".into())], utc_text(r));
        c.children.push(alarm);
    }
    c
}
