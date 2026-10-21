//! New appointment and meeting windows opened from a request: `calendar.newAppointment` /
//! `calendar.newMeeting` (and People › Meeting, Tasks › Meeting, Quick Steps, the keyboard) ask for the
//! appointment window with `Notice::OpenDialog("appointment" | "newMeeting", fields)` instead of saving
//! anything first; this fills the appointment editor from those fields (subject, attendees, notes, times,
//! all day, recurrence…), so cancelling the window leaves nothing behind. Opening Recurring Appointment /
//! Recurring Meeting / Recurring Event also opens the Recurrence dialog over it.
//!
//! New items started in the views (double-click on a slot, a day or the all-day strip) take the default
//! duration and reminder from File › Options › Calendar ([`new_at`]).

use serde_json::Value;

use mailcraft_engine::cmd::calendar::{
    self as engine_cal, ALL_DAY_REMINDER_MIN, APPOINTMENT_FORM, MEETING_FORM,
};
use mailcraft_model::FolderId;

use super::super::common::{ModState, day_string, parse_day_time, today};
use super::super::editors;
use super::super::recurrence::RecurrenceDialog;
use crate::App;

/// Most attendees taken from a request (the editor's text field holds them all).
const MAX_ATTENDEES: usize = 500;

/// True for a request this module answers with a prefilled new-item window: a new meeting, or the
/// appointment window without an item id.
pub(crate) fn is_form(name: &str, p: &Value) -> bool {
    name == MEETING_FORM || (name == APPOINTMENT_FORM && p.get("id").and_then(Value::as_u64).is_none())
}

/// Takes the new-item requests a command just made out of the session's notices (before the dialogs area
/// sees them) and opens the last one. True when one opened.
pub(super) fn take_notices(app: &mut App, st: &mut ModState) -> bool {
    let mut found: Option<(String, Value)> = None;
    app.s.notices.retain(|n| match n {
        mailcraft_engine::Notice::OpenDialog(name, p) if is_form(name, p) => {
            found = Some((name.clone(), p.clone()));
            false
        }
        _ => true,
    });
    match found {
        Some((name, p)) => {
            open(app, st, &name, &p);
            true
        }
        None => false,
    }
}

fn text(p: &Value, key: &str) -> String {
    p.get(key).and_then(Value::as_str).unwrap_or("").to_owned()
}

fn flag(p: &Value, key: &str) -> bool {
    p.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// "Name <address>; …" from a list param (or a string).
fn people(p: &Value, key: &str) -> String {
    match p.get(key) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .take(MAX_ATTENDEES)
            .collect::<Vec<_>>()
            .join("; "),
        Some(Value::String(t)) => t.trim().to_owned(),
        _ => String::new(),
    }
}

/// The reminder a request asks for: its `reminderMin` (`null` = none), else the default for the kind.
fn reminder(app: &App, p: &Value, all_day: bool) -> Option<u32> {
    match p.get("reminderMin") {
        Some(Value::Null) => None,
        Some(v) => v.as_u64().and_then(|m| u32::try_from(m).ok()),
        None => default_reminder(app, all_day),
    }
}

/// File › Options › Calendar › Default reminders (all-day events: the evening before).
fn default_reminder(app: &App, all_day: bool) -> Option<u32> {
    engine_cal::default_reminder_min(&app.s).map(|m| if all_day { ALL_DAY_REMINDER_MIN } else { m })
}

/// Opens the appointment editor for request `name` with fields `p`.
pub(crate) fn open(app: &App, st: &mut ModState, name: &str, p: &Value) {
    let folder = p.get("folder").and_then(Value::as_u64).map(FolderId);
    let all_day = flag(p, "allDay");
    let start =
        p.get("start").and_then(Value::as_str).and_then(parse_day_time).unwrap_or((today(app), 9 * 60));
    let len = engine_cal::default_duration_min(&app.s);
    let end = p
        .get("end")
        .and_then(Value::as_str)
        .and_then(parse_day_time)
        .unwrap_or((start.0, start.1.saturating_add(len)));
    let dur = (end.0 - start.0).saturating_mul(24 * 60).saturating_add(end.1 - start.1);
    editors::new_appointment(app, st, folder, start.0, start.1, dur.max(0), all_day);
    let Some(ed) = st.editors.appt.as_mut() else { return };
    if all_day {
        // The request names the inclusive last day.
        ed.end_date = day_string(end.0.max(start.0));
    }
    ed.reminder = reminder(app, p, all_day);
    ed.subject = text(p, "subject");
    ed.location = text(p, "location");
    ed.body = text(p, "body");
    let show_as = text(p, "showAs");
    if !show_as.is_empty() {
        ed.show_as = show_as;
    }
    ed.private = flag(p, "private");
    if let Some(cats) = p.get("categories").and_then(Value::as_array) {
        ed.categories = cats.iter().filter_map(Value::as_str).map(str::to_owned).take(100).collect();
    }
    ed.recurrence = text(p, "recurrence");
    let mut required = people(p, "attendees");
    if required.is_empty() {
        required = people(p, "to");
    }
    ed.attendees = required;
    ed.optional = people(p, "optional");
    ed.resources = people(p, "resources");
    let importance = text(p, "importance");
    if !importance.is_empty() {
        ed.importance = importance;
    }
    ed.body_html =
        p.get("bodyHtml").and_then(Value::as_str).filter(|h| !h.trim().is_empty()).map(str::to_owned);
    // A request in other time zones: its times are in them (the engine sends them so).
    if !all_day && flag(p, "zonesShown") {
        let start_zone = text(p, "timeZone");
        let end_zone = text(p, "endTimeZone");
        ed.zones_shown = true;
        ed.tz_end = if end_zone.is_empty() { start_zone.clone() } else { end_zone };
        ed.tz_start = start_zone;
    }
    ed.meeting = name == MEETING_FORM || flag(p, "meeting");
    if flag(p, "recurring") {
        ed.recurrence_dlg = Some(RecurrenceDialog::new(
            app,
            &ed.start_date,
            ed.start_min,
            ed.end_min,
            ed.all_day,
            &ed.recurrence,
            ed.nonce,
        ));
    }
}

/// A new appointment started in a view at `day` `minute` (or all day): the default duration and reminder
/// from File › Options › Calendar.
pub(super) fn new_at(
    app: &App,
    st: &mut ModState,
    folder: Option<FolderId>,
    day: i64,
    minute: i64,
    all_day: bool,
) {
    let len = if all_day { 0 } else { engine_cal::default_duration_min(&app.s) };
    editors::new_appointment(app, st, folder, day, minute, len, all_day);
    if let Some(ed) = st.editors.appt.as_mut() {
        ed.reminder = default_reminder(app, all_day);
    }
}
