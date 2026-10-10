//! iTIP (RFC 5546) basics: meeting requests, replies and cancellations carried as `text/calendar`.

use std::collections::BTreeSet;

use mailcraft_mime::Mailbox;
use mailcraft_model::calendar::ResponseStatus;
use mailcraft_model::{Appointment, DateTime};

use super::map::{self, EventOut};
use super::{Event, ImportOptions, TimeZone};
use crate::Result;

/// iTIP METHOD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    Publish,
    Request,
    Reply,
    Add,
    Cancel,
    Refresh,
    Counter,
    DeclineCounter,
    Other(String),
}

impl Method {
    pub fn parse(s: &str) -> Method {
        match s.trim().to_ascii_uppercase().as_str() {
            "PUBLISH" => Method::Publish,
            "REQUEST" => Method::Request,
            "REPLY" => Method::Reply,
            "ADD" => Method::Add,
            "CANCEL" => Method::Cancel,
            "REFRESH" => Method::Refresh,
            "COUNTER" => Method::Counter,
            "DECLINECOUNTER" => Method::DeclineCounter,
            other => Method::Other(crate::text::clip(other, 64)),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Method::Publish => "PUBLISH",
            Method::Request => "REQUEST",
            Method::Reply => "REPLY",
            Method::Add => "ADD",
            Method::Cancel => "CANCEL",
            Method::Refresh => "REFRESH",
            Method::Counter => "COUNTER",
            Method::DeclineCounter => "DECLINECOUNTER",
            Method::Other(s) => s,
        }
    }
}

/// Most overridden instances kept from one scheduling message.
pub const MAX_OVERRIDES: usize = 2_000;

/// A scheduling message: its method, its main event and the overridden instances sent with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItipMessage {
    pub method: Method,
    /// The series (the first VEVENT without RECURRENCE-ID), else the first instance when the message is
    /// about single instances only (`event.recurrence_id` is then set).
    pub event: Event,
    /// Other VEVENTs with the same UID that override one instance each (RFC 5546 §3.2.2, RECURRENCE-ID).
    pub overrides: Vec<Event>,
}

/// Parses a scheduling message (requires METHOD and a VEVENT).
pub fn read(data: &[u8], opts: ImportOptions) -> Result<ItipMessage> {
    let cal = super::read(data, opts)?;
    let method = cal
        .method
        .ok_or_else(|| crate::FormatError::malformed("ics", "no METHOD (not a scheduling message)"))?;
    let mut events = cal.events;
    let main = events.iter().position(|e| e.recurrence_id.is_none()).unwrap_or(0);
    if main >= events.len() {
        return Err(crate::FormatError::malformed("ics", "scheduling message without a VEVENT"));
    }
    let event = events.remove(main);
    let overrides = events
        .into_iter()
        .filter(|e| {
            e.recurrence_id.is_some() && e.appointment.uid.eq_ignore_ascii_case(&event.appointment.uid)
        })
        .take(MAX_OVERRIDES)
        .collect();
    Ok(ItipMessage { method, event, overrides })
}

fn one_event(a: &Appointment, method: Method, o: &EventOut<'_>, now: DateTime) -> String {
    let mut cal = map::calendar_shell(Some(&method));
    let mut offsets = BTreeSet::new();
    let ev = map::event_component(a, o, now, &mut offsets);
    for off in offsets {
        cal.children.push(TimeZone::fixed_component(off));
    }
    cal.children.push(ev);
    let mut out = String::new();
    cal.write(&mut out);
    out
}

/// METHOD:REQUEST invitation (attendees asked to RSVP; no alarm).
pub fn request(a: &Appointment, sequence: u32, now: DateTime) -> String {
    let o =
        EventOut { sequence, status: Some("CONFIRMED"), rsvp: true, no_alarm: true, ..Default::default() };
    one_event(a, Method::Request, &o, now)
}

/// METHOD:REPLY from `attendee` with `response`.
pub fn reply(
    a: &Appointment,
    attendee: &Mailbox,
    response: ResponseStatus,
    sequence: u32,
    now: DateTime,
) -> String {
    let o = EventOut {
        sequence,
        only_attendee: Some((attendee, response)),
        no_alarm: true,
        ..Default::default()
    };
    one_event(a, Method::Reply, &o, now)
}

/// METHOD:CANCEL for the whole meeting.
pub fn cancel(a: &Appointment, sequence: u32, now: DateTime) -> String {
    let o = EventOut { sequence, status: Some("CANCELLED"), no_alarm: true, ..Default::default() };
    one_event(a, Method::Cancel, &o, now)
}

/// Applies a REPLY's attendee responses to the organiser's copy (matched by UID and address).
/// Returns the number of attendees updated.
pub fn apply_reply(a: &mut Appointment, reply: &ItipMessage) -> usize {
    if reply.method != Method::Reply || !reply.event.appointment.uid.eq_ignore_ascii_case(&a.uid) {
        return 0;
    }
    let mut n = 0;
    for r in &reply.event.appointment.attendees {
        if let Some(mine) = a.attendees.iter_mut().find(|x| x.mailbox.same_address(&r.mailbox)) {
            mine.response = r.response;
            n += 1;
        }
    }
    n
}

/// True when a CANCEL message cancels `a` (same UID).
pub fn cancels(a: &Appointment, msg: &ItipMessage) -> bool {
    msg.method == Method::Cancel && msg.event.appointment.uid.eq_ignore_ascii_case(&a.uid)
}
