//! iCalendar (RFC 5545) and iTIP (RFC 5546) basics.
//!
//! Reading: VCALENDAR › VEVENT / VTODO / VALARM / VTIMEZONE into [`mailcraft_model::Appointment`] and
//! [`mailcraft_model::Task`]. DATE values become local midnight in [`ImportOptions::local_offset_min`]; floating
//! times are taken in that offset too; `TZID` times resolve through the file's VTIMEZONEs, then a built-in table
//! of common zones and fixed-offset names, else they are treated as floating. Recurrence keeps the RRULE text
//! and EXDATEs on the appointment; [`Event::occurrences`] expands with full time-zone rules and
//! [`expand_appointment`] with the appointment's own (fixed) offset. Overridden instances (RECURRENCE-ID)
//! become separate appointments and an EXDATE on the series.
//!
//! Writing: VERSION 2.0 with CRLF and 75-octet folding; times are UTC (`Z`) when the item's offset is 0, else
//! local time with a fixed-offset VTIMEZONE (`TZID=UTC-04:00`) so the written offset round-trips.

pub mod itip;
mod map;
pub mod parse;
pub mod rrule;
pub mod tz;
pub mod value;

use mailcraft_model::{Appointment, DateTime, Task};

pub use itip::Method;
pub use parse::Component;
pub use rrule::{Rrule, Until};
pub use tz::{TimeZone, ZoneLookup, Zones};

use crate::{Result, check_size};

/// Largest iCalendar input accepted.
pub const MAX_ICS_BYTES: usize = 32 * 1024 * 1024;
/// Most events/tasks read from one input.
pub const MAX_ITEMS: usize = 100_000;
/// Most VTIMEZONE definitions kept from one input (real files define a handful).
pub const MAX_ZONES: usize = 1_000;
/// Product identifier written to every calendar.
pub const PRODID: &str = "-//MailCraft//MailCraft Calendar//EN";

/// Reader settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// The user's UTC offset (minutes east): all-day dates and floating times are placed in it.
    pub local_offset_min: i32,
}

/// One VEVENT with the details the model does not keep.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub appointment: Appointment,
    /// TZID of DTSTART, when zoned.
    pub tzid: Option<String>,
    /// DTSTART as written (local secs in its zone; for UTC, the unix time).
    pub start_local: i64,
    /// SEQUENCE (iTIP revision).
    pub sequence: u32,
    /// STATUS (TENTATIVE / CONFIRMED / CANCELLED).
    pub status: Option<String>,
    /// RECURRENCE-ID: this VEVENT overrides one instance of a series.
    pub recurrence_id: Option<DateTime>,
    /// RDATE instants (not kept on the appointment).
    pub rdates: Vec<DateTime>,
}

/// A parsed calendar.
#[derive(Clone, Debug, Default)]
pub struct Calendar {
    pub method: Option<Method>,
    pub prodid: Option<String>,
    pub events: Vec<Event>,
    pub tasks: Vec<Task>,
    pub zones: Zones,
}

impl Calendar {
    /// The events' appointments.
    pub fn appointments(&self) -> Vec<Appointment> {
        self.events.iter().map(|e| e.appointment.clone()).collect()
    }
}

/// Parses iCalendar bytes.
pub fn read(data: &[u8], opts: ImportOptions) -> Result<Calendar> {
    check_size("ics", data.len(), MAX_ICS_BYTES)?;
    let text = crate::text::decode_bytes(data);
    read_str(&text, opts)
}

/// Parses iCalendar text. Errors only when no VCALENDAR (or bare VEVENT/VTODO) is present.
pub fn read_str(text: &str, opts: ImportOptions) -> Result<Calendar> {
    let top = parse::parse(text);
    let mut cals: Vec<&Component> = top.iter().filter(|c| c.name == "VCALENDAR").collect();
    // Tolerate bare components without the VCALENDAR wrapper.
    let wrapper;
    if cals.is_empty() {
        let bare: Vec<Component> = top
            .iter()
            .filter(|c| matches!(c.name.as_str(), "VEVENT" | "VTODO" | "VTIMEZONE"))
            .cloned()
            .collect();
        if bare.is_empty() {
            return Err(crate::FormatError::malformed("ics", "no VCALENDAR object"));
        }
        wrapper = Component { name: "VCALENDAR".into(), props: Vec::new(), children: bare };
        cals.push(&wrapper);
    }
    let mut cal = Calendar::default();
    // Every TZID lookup scans the definitions: keep the first definition of each TZID, up to a cap.
    'zones: for c in &cals {
        for z in c.children_named("VTIMEZONE") {
            if cal.zones.defined.len() >= MAX_ZONES {
                break 'zones;
            }
            if let Some(tz) = TimeZone::from_component(z)
                && !cal.zones.defined.iter().any(|d| d.tzid.eq_ignore_ascii_case(&tz.tzid))
            {
                cal.zones.defined.push(tz);
            }
        }
    }
    let mut events = Vec::new();
    let mut tasks = Vec::new();
    let ctx = map::Ctx::new(&cal.zones, opts.local_offset_min);
    for c in cals {
        if cal.method.is_none() {
            cal.method = c.prop("METHOD").map(|p| Method::parse(&p.value));
        }
        if cal.prodid.is_none() {
            cal.prodid = c.text("PRODID");
        }
        for child in &c.children {
            if events.len() + tasks.len() >= MAX_ITEMS {
                break;
            }
            match child.name.as_str() {
                "VEVENT" => {
                    if let Some(e) = map::event_from(child, &ctx) {
                        events.push(e);
                    }
                }
                "VTODO" => tasks.push(map::task_from(child, &ctx)),
                _ => {}
            }
        }
    }
    drop(ctx);
    map::link_overrides(&mut events);
    cal.events = events;
    cal.tasks = tasks;
    Ok(cal)
}

impl Event {
    /// Occurrences (start, end) overlapping `[from, to)`, using the event's zone rules (DST-aware).
    pub fn occurrences(
        &self,
        zones: &Zones,
        from: DateTime,
        to: DateTime,
        max: usize,
    ) -> Vec<(DateTime, DateTime)> {
        let a = &self.appointment;
        let duration = a.end.unix.saturating_sub(a.start.unix).max(0);
        let zone = self.tzid.as_deref().and_then(|t| zones.get(t)).map(ZoneLookup::new);
        let (start_local, to_utc): (i64, Box<dyn Fn(i64) -> i64>) = match &zone {
            Some(z) => (self.start_local, Box::new(move |l| z.to_utc(l))),
            None => {
                let off = i64::from(a.start.offset_min) * 60;
                (a.start.unix.saturating_add(off), Box::new(move |l: i64| l.saturating_sub(off)))
            }
        };
        let offset_of = |unix: i64| match &zone {
            Some(z) => z.offset_at_utc(unix),
            None => a.start.offset_min,
        };
        let mut out = occurrences_with(a, start_local, duration, &*to_utc, &offset_of, from, to, max);
        // RFC 5545 §3.8.5: the recurrence set is DTSTART + RRULE + RDATE, minus EXDATE, each instance once.
        let exdates = a.recurrence.as_ref().map(|r| r.exdates.as_slice()).unwrap_or_default();
        for r in &self.rdates {
            let wanted = r.unix < to.unix && r.unix.saturating_add(duration) > from.unix;
            if wanted
                && out.len() < max
                && !exdates.iter().any(|x| x.unix == r.unix)
                && !out.iter().any(|(s, _)| s.unix == r.unix)
            {
                out.push((*r, DateTime::from_unix(r.unix.saturating_add(duration), r.offset_min)));
            }
        }
        out.sort_by_key(|(s, _)| s.unix);
        out
    }
}

fn occurrences_with(
    a: &Appointment,
    start_local: i64,
    duration: i64,
    to_utc: &dyn Fn(i64) -> i64,
    offset_of: &dyn Fn(i64) -> i32,
    from: DateTime,
    to: DateTime,
    max: usize,
) -> Vec<(DateTime, DateTime)> {
    let single = || {
        if a.start.unix < to.unix && (a.end.unix > from.unix || a.start.unix >= from.unix) {
            vec![(a.start, a.end)]
        } else {
            Vec::new()
        }
    };
    let Some(rec) = &a.recurrence else { return single() };
    let Ok(rule) = Rrule::parse(&rec.rrule) else { return single() };
    let exdates: Vec<DateTime> = rec.exdates.clone();
    let all_day = a.all_day;
    let excluded = move |local: i64, unix: i64| {
        exdates.iter().any(|x| {
            x.unix == unix
                || (all_day
                    && x.unix.saturating_add(i64::from(x.offset_min) * 60).div_euclid(value::DAY)
                        == local.div_euclid(value::DAY))
        })
    };
    let x = rrule::Expand { rule: &rule, dtstart: start_local, duration, to_utc, excluded: &excluded };
    rrule::expand(&x, from.unix, to.unix, max)
        .into_iter()
        .map(|o| {
            let off = offset_of(o.unix);
            (DateTime::from_unix(o.unix, off), DateTime::from_unix(o.unix.saturating_add(duration), off))
        })
        .collect()
}

/// Occurrences (start, end) of an appointment overlapping `[from, to)`, using its start's fixed UTC offset.
pub fn expand_appointment(
    a: &Appointment,
    from: DateTime,
    to: DateTime,
    max: usize,
) -> Vec<(DateTime, DateTime)> {
    let off = i64::from(a.start.offset_min) * 60;
    let duration = a.end.unix.saturating_sub(a.start.unix).max(0);
    let to_utc = move |l: i64| l.saturating_sub(off);
    let offset_of = |_: i64| a.start.offset_min;
    occurrences_with(a, a.start.unix.saturating_add(off), duration, &to_utc, &offset_of, from, to, max)
}

/// Writes appointments and tasks as one VCALENDAR (`method` adds a METHOD line).
pub fn write(
    appointments: &[&Appointment],
    tasks: &[&Task],
    method: Option<&Method>,
    now: DateTime,
) -> String {
    let mut cal = map::calendar_shell(method);
    let mut offsets = std::collections::BTreeSet::new();
    let mut children = Vec::new();
    for a in appointments {
        children.push(map::event_component(a, &map::EventOut::default(), now, &mut offsets));
    }
    for t in tasks {
        children.push(map::task_component(t, now, &mut offsets));
    }
    for off in offsets {
        cal.children.push(TimeZone::fixed_component(off));
    }
    cal.children.extend(children);
    let mut out = String::new();
    cal.write(&mut out);
    out
}
