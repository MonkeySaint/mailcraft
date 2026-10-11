//! Reminders (View › Window › Reminders Window).
//!
//! [`due`] scans the mailbox for reminders that are due now: appointment occurrences (calendar folders,
//! `reminder_min` before the start, until the occurrence ends; a recurring series shows its first due
//! occurrence), open tasks with a reminder time, and flagged mail and contacts with a reminder time
//! (contacts through [`crate::cmd::people::due_reminders`]). The UI polls it once a minute and opens the
//! Reminders window when a reminder it has not shown yet comes due.
//!
//! **State.** The session has no reminder fields (its interface is frozen), so — like the calendar view
//! state — what can't be kept on the items lives in a [`ReminderState`] that the caller holds and passes to
//! the `calendar.*Reminder*` commands as `{"state": {...}}` (they return the new state). Dismissing keeps what
//! it can on the item itself, so it survives a restart: a one-off appointment loses its reminder
//! (`reminder_min = None`), a task, flagged message or flagged contact loses its reminder time. Dismissing
//! one occurrence of a recurring series is recorded in the state. Snoozing a task, message or contact moves
//! its reminder time; snoozing an appointment is recorded in the state.
//!
//! Keys identify a reminder: `appointment:<id>:<occurrence start, unix>`, `task:<id>`, `mail:<id>`,
//! `contact:<id>`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cmd::calendar::{occurrences, pim};
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::{DateTime, FlagState, FolderKind, FolderRole, ItemId, Mailbox, TaskStatus};

/// Snooze choices offered by the Reminders window (minutes).
pub const SNOOZE_MINUTES: [u32; 13] = [5, 10, 15, 30, 60, 120, 240, 480, 1440, 2880, 4320, 10_080, 20_160];
/// Default snooze (minutes).
pub const DEFAULT_SNOOZE: u32 = 5;
/// Longest snooze accepted (minutes; four weeks).
pub const MAX_SNOOZE: u32 = 4 * 7 * 24 * 60;
/// Most reminders returned by one scan.
pub const MAX_REMINDERS: usize = 500;
/// Most dismissed/snoozed keys kept in a state (oldest snoozes drop first).
pub const MAX_STATE_KEYS: usize = 5_000;
/// Most occurrences of one series examined per scan.
const MAX_OCCURRENCES: usize = 64;

/// What a reminder is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReminderKind {
    Appointment,
    Task,
    /// A follow-up flag's reminder on a message (key `mail:<id>`).
    Mail,
    /// A follow-up flag's reminder on a contact (key `contact:<id>`).
    Contact,
}

/// One due reminder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    pub key: String,
    pub kind: ReminderKind,
    pub item: u64,
    pub subject: String,
    pub location: String,
    /// When the reminder came due (unix seconds).
    pub remind_at: i64,
    /// Appointment occurrence start, task due date or flag due date (unix seconds).
    pub due: Option<i64>,
    /// Appointment occurrence end (unix seconds).
    pub end: Option<i64>,
    pub all_day: bool,
    pub recurring: bool,
}

/// Reminder state the caller keeps between scans.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReminderState {
    /// Dismissed occurrences of recurring appointments.
    pub dismissed: BTreeSet<String>,
    /// Snoozed reminders: key → unix time they come back.
    pub snoozed: BTreeMap<String, i64>,
}

impl ReminderState {
    /// Drops snoozes that have run out and caps the sizes.
    fn tidy(&mut self, now: i64) {
        self.snoozed.retain(|_, until| *until > now);
        while self.snoozed.len() > MAX_STATE_KEYS {
            let Some(oldest) = self.snoozed.iter().min_by_key(|(_, u)| **u).map(|(k, _)| k.clone()) else {
                break;
            };
            self.snoozed.remove(&oldest);
        }
        while self.dismissed.len() > MAX_STATE_KEYS {
            let Some(first) = self.dismissed.iter().next().cloned() else { break };
            self.dismissed.remove(&first);
        }
    }

    fn hidden(&self, key: &str, now: i64) -> bool {
        self.dismissed.contains(key) || self.snoozed.get(key).is_some_and(|u| *u > now)
    }
}

/// A parsed reminder key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Appointment(ItemId, i64),
    Task(ItemId),
    Mail(ItemId),
    Contact(ItemId),
}

impl Key {
    pub fn parse(key: &str) -> Option<Key> {
        let mut parts = key.split(':');
        let kind = parts.next()?;
        let id = ItemId(parts.next()?.trim().parse().ok()?);
        let key = match kind {
            "appointment" => Key::Appointment(id, parts.next()?.trim().parse().ok()?),
            "task" => Key::Task(id),
            "mail" => Key::Mail(id),
            "contact" => Key::Contact(id),
            _ => return None,
        };
        if parts.next().is_some() { None } else { Some(key) }
    }

    pub fn text(self) -> String {
        match self {
            Key::Appointment(id, at) => format!("appointment:{}:{at}", id.0),
            Key::Task(id) => format!("task:{}", id.0),
            Key::Mail(id) => format!("mail:{}", id.0),
            Key::Contact(id) => format!("contact:{}", id.0),
        }
    }

    pub fn item(self) -> ItemId {
        match self {
            Key::Appointment(id, _) | Key::Task(id) | Key::Mail(id) | Key::Contact(id) => id,
        }
    }
}

fn in_kind(mb: &Mailbox, folder: mailcraft_model::FolderId, kind: FolderKind) -> bool {
    mb.folder(folder).is_some_and(|f| f.kind == kind)
}

fn in_deleted(mb: &Mailbox, folder: mailcraft_model::FolderId) -> bool {
    mb.folder_account(folder)
        .and_then(|a| mb.role_folder(a, FolderRole::DeletedItems))
        .is_some_and(|d| mb.is_in_subtree(folder, d))
}

/// Reminders due at `s.now()` that `st` does not hide, soonest first (appointments by start, then tasks, mail
/// and contacts by due date and reminder time).
pub fn due(s: &Session, st: &ReminderState) -> Vec<Reminder> {
    let now = s.now();
    let mb = &s.mailbox;
    let mut out = Vec::new();
    for a in mb.appointments.values() {
        let Some(m) = a.reminder_min else { continue };
        if !in_kind(mb, a.folder, FolderKind::Calendar) {
            continue;
        }
        let lead = i64::from(m).saturating_mul(60);
        let to = pim::add_secs(now, lead.saturating_add(1));
        for (start, end) in occurrences(a, now, to, MAX_OCCURRENCES) {
            let remind_at = start.unix.saturating_sub(lead);
            // Ended occurrences drop out; a zero-length one stays until its start passes.
            let running = end.unix > now.unix || (end.unix == start.unix && start.unix >= now.unix);
            if remind_at > now.unix || !running {
                continue;
            }
            let key = Key::Appointment(a.id, start.unix).text();
            if st.hidden(&key, now.unix) {
                continue;
            }
            out.push(Reminder {
                key,
                kind: ReminderKind::Appointment,
                item: a.id.0,
                subject: a.subject.clone(),
                location: a.location.clone(),
                remind_at,
                due: Some(start.unix),
                end: Some(end.unix),
                all_day: a.all_day,
                recurring: a.recurrence.is_some(),
            });
            // A series reminds of one occurrence at a time.
            break;
        }
    }
    for t in mb.tasks.values() {
        let Some(r) = t.reminder else { continue };
        if r.unix > now.unix || t.status == TaskStatus::Completed || !in_kind(mb, t.folder, FolderKind::Tasks)
        {
            continue;
        }
        let key = Key::Task(t.id).text();
        if st.hidden(&key, now.unix) {
            continue;
        }
        out.push(Reminder {
            key,
            kind: ReminderKind::Task,
            item: t.id.0,
            subject: t.subject.clone(),
            location: String::new(),
            remind_at: r.unix,
            due: t.due.map(|d| d.unix),
            end: None,
            all_day: false,
            recurring: false,
        });
    }
    for m in mb.mail.values() {
        let FlagState::Flagged { reminder: Some(r), due: flag_due, .. } = &m.flag else { continue };
        if r.unix > now.unix || in_deleted(mb, m.folder) {
            continue;
        }
        let key = Key::Mail(m.id).text();
        if st.hidden(&key, now.unix) {
            continue;
        }
        out.push(Reminder {
            key,
            kind: ReminderKind::Mail,
            item: m.id.0,
            subject: m.subject.clone(),
            location: String::new(),
            remind_at: r.unix,
            due: flag_due.map(|d| d.unix),
            end: None,
            all_day: false,
            recurring: false,
        });
    }
    for c in crate::cmd::people::due_reminders(s, now.unix) {
        let key = Key::Contact(ItemId(c.contact)).text();
        if st.hidden(&key, now.unix) {
            continue;
        }
        out.push(Reminder {
            key,
            kind: ReminderKind::Contact,
            item: c.contact,
            subject: c.subject,
            location: String::new(),
            remind_at: c.remind_at,
            due: c.due,
            end: None,
            all_day: false,
            recurring: false,
        });
    }
    out.sort_by_key(|r| {
        (r.kind != ReminderKind::Appointment, r.due.unwrap_or(r.remind_at), r.remind_at, r.item)
    });
    out.truncate(MAX_REMINDERS);
    out
}

/// The next time (after now) a reminder comes due or a snooze runs out, within `horizon_secs`; lets the UI
/// wake up exactly then instead of polling.
pub fn next_due_after(s: &Session, st: &ReminderState, horizon_secs: i64) -> Option<i64> {
    let now = s.now().unix;
    let limit = now.saturating_add(horizon_secs.max(0));
    let mut best: Option<i64> = None;
    let mut consider = |t: i64| {
        if t > now && t <= limit && best.is_none_or(|b| t < b) {
            best = Some(t);
        }
    };
    for until in st.snoozed.values() {
        consider(*until);
    }
    let mb = &s.mailbox;
    for t in mb.tasks.values() {
        if t.status != TaskStatus::Completed
            && let Some(r) = t.reminder
        {
            consider(r.unix);
        }
    }
    for m in mb.mail.values() {
        if let FlagState::Flagged { reminder: Some(r), .. } = &m.flag {
            consider(r.unix);
        }
    }
    if let Some(t) = crate::cmd::people::next_reminder_after(s, now, limit) {
        consider(t);
    }
    let from = s.now();
    for a in mb.appointments.values() {
        let Some(m) = a.reminder_min else { continue };
        if !in_kind(mb, a.folder, FolderKind::Calendar) {
            continue;
        }
        let lead = i64::from(m).saturating_mul(60);
        let to = pim::add_secs(from, lead.saturating_add(horizon_secs.max(0)));
        if let Some((start, _)) = occurrences(a, from, to, MAX_OCCURRENCES)
            .into_iter()
            .find(|(st, _)| st.unix.saturating_sub(lead) > now)
        {
            consider(start.unix.saturating_sub(lead));
        }
    }
    best
}

/// "5 minutes overdue", "Now", "In 10 minutes", "2 days overdue" — the Reminders window's Due In column.
pub fn due_in_text(r: &Reminder, now: i64) -> String {
    let target = r.due.unwrap_or(r.remind_at);
    let diff = target.saturating_sub(now);
    let span = |secs: i64| -> String {
        let mins = secs.unsigned_abs() / 60;
        if mins < 60 {
            format!("{mins} minute{}", if mins == 1 { "" } else { "s" })
        } else if mins < 24 * 60 {
            let h = mins / 60;
            format!("{h} hour{}", if h == 1 { "" } else { "s" })
        } else if mins < 7 * 24 * 60 {
            let d = mins / (24 * 60);
            format!("{d} day{}", if d == 1 { "" } else { "s" })
        } else {
            let w = mins / (7 * 24 * 60);
            format!("{w} week{}", if w == 1 { "" } else { "s" })
        }
    };
    if diff.abs() < 60 {
        "Now".to_owned()
    } else if diff > 0 {
        format!("In {}", span(diff))
    } else {
        format!("{} overdue", span(diff))
    }
}

/// Dismisses reminders `keys`. Returns the number dismissed.
pub fn dismiss(s: &mut Session, st: &mut ReminderState, keys: &[String]) -> usize {
    let mut n = 0usize;
    for k in keys {
        let Some(key) = Key::parse(k) else { continue };
        let done = match key {
            Key::Appointment(id, _) => match s.mailbox.appointments.get_mut(&id) {
                Some(a) if a.recurrence.is_some() => {
                    st.dismissed.insert(key.text());
                    true
                }
                Some(a) => {
                    a.reminder_min = None;
                    true
                }
                None => false,
            },
            Key::Task(id) => match s.mailbox.tasks.get_mut(&id) {
                Some(t) => {
                    t.reminder = None;
                    true
                }
                None => false,
            },
            Key::Mail(id) => match s.mailbox.mail.get_mut(&id) {
                Some(m) => match &mut m.flag {
                    FlagState::Flagged { reminder, .. } => {
                        *reminder = None;
                        true
                    }
                    _ => false,
                },
                None => false,
            },
            Key::Contact(id) => crate::cmd::people::dismiss_reminder(s, id),
        };
        if done {
            st.snoozed.remove(&key.text());
            n = n.saturating_add(1);
        }
    }
    st.tidy(s.now().unix);
    n
}

/// Snoozes reminders `keys` for `minutes`. Returns the number snoozed.
pub fn snooze(s: &mut Session, st: &mut ReminderState, keys: &[String], minutes: u32) -> usize {
    let now = s.now();
    let until = pim::add_secs(now, i64::from(minutes.min(MAX_SNOOZE)).saturating_mul(60));
    let mut n = 0usize;
    for k in keys {
        let Some(key) = Key::parse(k) else { continue };
        let done = match key {
            Key::Appointment(id, _) => {
                if s.mailbox.appointments.contains_key(&id) {
                    st.snoozed.insert(key.text(), until.unix);
                    true
                } else {
                    false
                }
            }
            Key::Task(id) => match s.mailbox.tasks.get_mut(&id) {
                Some(t) => {
                    t.reminder = Some(until);
                    true
                }
                None => false,
            },
            Key::Mail(id) => match s.mailbox.mail.get_mut(&id) {
                Some(m) => match &mut m.flag {
                    FlagState::Flagged { reminder, .. } => {
                        *reminder = Some(until);
                        true
                    }
                    _ => false,
                },
                None => false,
            },
            Key::Contact(id) => crate::cmd::people::snooze_reminder(s, id, until),
        };
        if done {
            n = n.saturating_add(1);
        }
    }
    st.tidy(now.unix);
    n
}

// ---------------------------------------------------------------------------------------------------------
// Command bodies (registered by `cmd::calendar`)
// ---------------------------------------------------------------------------------------------------------

fn state_param(p: &Value) -> Result<ReminderState, CmdError> {
    match p.get("state") {
        None | Some(Value::Null) => Ok(ReminderState::default()),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| CmdError::Params(format!("`state`: {e}"))),
    }
}

fn keys_param(s: &Session, st: &ReminderState, p: &Value) -> Result<Vec<String>, CmdError> {
    if p.get("all").and_then(Value::as_bool) == Some(true) {
        return Ok(due(s, st).into_iter().map(|r| r.key).collect());
    }
    let mut keys = Vec::new();
    match p.get("keys").or_else(|| p.get("key")) {
        Some(Value::String(k)) => keys.push(k.clone()),
        Some(Value::Array(a)) if a.len() <= pim::MAX_LIST => {
            for k in a {
                match k.as_str() {
                    Some(k) => keys.push(k.to_owned()),
                    None => return Err(CmdError::Params("`keys` must hold strings".into())),
                }
            }
        }
        _ => return Err(CmdError::Params("pass `keys` (reminder keys) or `all: true`".into())),
    }
    if let Some(bad) = keys.iter().find(|k| Key::parse(k).is_none()) {
        return Err(CmdError::Params(format!("`{bad}` is not a reminder key")));
    }
    Ok(keys)
}

fn reminders_json(s: &Session, list: &[Reminder]) -> Value {
    let now = s.now().unix;
    let tz = s.tz_offset_min;
    Value::Array(
        list.iter()
            .map(|r| {
                let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
                if let Some(o) = v.as_object_mut() {
                    o.insert("dueIn".into(), json!(due_in_text(r, now)));
                    if let Some(d) = r.due {
                        o.insert(
                            "dueText".into(),
                            json!(pim::datetime_string(DateTime::from_unix(d, tz), tz)),
                        );
                    }
                }
                v
            })
            .collect(),
    )
}

/// `calendar.reminders {state?}` → `{"reminders": [...], "state": {...}, "next": unix|null}`.
pub fn run_list(s: &mut Session, p: &Value) -> CmdResult {
    let mut st = state_param(p)?;
    st.tidy(s.now().unix);
    let list = due(s, &st);
    let next = next_due_after(s, &st, 7 * 24 * 3600);
    Ok(json!({ "reminders": reminders_json(s, &list), "state": st, "next": next }))
}

/// `calendar.dismissReminder {keys | all, state?}`.
pub fn run_dismiss(s: &mut Session, p: &Value) -> CmdResult {
    let mut st = state_param(p)?;
    let keys = keys_param(s, &st, p)?;
    let n = dismiss(s, &mut st, &keys);
    let left = due(s, &st);
    Ok(json!({ "dismissed": n, "state": st, "reminders": reminders_json(s, &left) }))
}

/// `calendar.snoozeReminder {keys | all, minutes?, state?}`.
pub fn run_snooze(s: &mut Session, p: &Value) -> CmdResult {
    let mut st = state_param(p)?;
    let keys = keys_param(s, &st, p)?;
    let minutes = match p.get("minutes") {
        None | Some(Value::Null) => DEFAULT_SNOOZE,
        Some(v) => v
            .as_u64()
            .filter(|m| (1..=u64::from(MAX_SNOOZE)).contains(m))
            .and_then(|m| u32::try_from(m).ok())
            .ok_or_else(|| CmdError::Params(format!("`minutes` must be 1..{MAX_SNOOZE}")))?,
    };
    let n = snooze(s, &mut st, &keys, minutes);
    let left = due(s, &st);
    Ok(json!({ "snoozed": n, "minutes": minutes, "state": st, "reminders": reminders_json(s, &left) }))
}

/// `calendar.openReminderItem {key}`: opens the item's window.
pub fn run_open(s: &mut Session, p: &Value) -> CmdResult {
    let key = p
        .get("key")
        .and_then(Value::as_str)
        .and_then(Key::parse)
        .ok_or_else(|| CmdError::Params("`key` must be a reminder key".into()))?;
    match key {
        Key::Appointment(id, _) if s.mailbox.appointments.contains_key(&id) => {
            pim::dialog(s, "appointment", json!({ "id": id.0 }))
        }
        Key::Task(id) if s.mailbox.tasks.contains_key(&id) => pim::dialog(s, "task", json!({ "id": id.0 })),
        Key::Mail(id) if s.mailbox.mail.contains_key(&id) => {
            pim::dialog(s, "readWindow", json!({ "id": id.0 }))
        }
        Key::Contact(id) if s.mailbox.contacts.contains_key(&id) => {
            pim::dialog(s, "contact", json!({ "id": id.0 }))
        }
        _ => Err(CmdError::Params("the item no longer exists".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::calendar::tests::{role, session};
    use mailcraft_model::{FolderRole, MailItem};

    fn at(s: &Session, minutes: i64) -> DateTime {
        pim::add_secs(s.now(), minutes * 60)
    }

    fn appt(
        s: &mut Session,
        start_min: i64,
        len_min: i64,
        reminder: Option<u32>,
        rrule: Option<&str>,
    ) -> u64 {
        let start = pim::datetime_string(at(s, start_min), 0);
        let mut p =
            json!({ "subject": "Sync", "start": start, "durationMin": len_min, "reminderMin": reminder });
        if let Some(r) = rrule {
            p["recurrence"] = json!(r);
        }
        s.run("calendar.newAppointment", &p).unwrap()["id"].as_u64().unwrap()
    }

    #[test]
    fn keys_round_trip_and_reject_junk() {
        for k in ["appointment:7:1760000000", "task:3", "mail:12", "contact:4"] {
            assert_eq!(Key::parse(k).unwrap().text(), k);
        }
        for k in [
            "",
            "task",
            "task:x",
            "appointment:1",
            "mail:1:2",
            "note:1",
            "task:1:",
            "contact:",
            "contact:1:2",
        ] {
            assert!(Key::parse(k).is_none(), "{k}");
        }
    }

    #[test]
    fn appointment_reminder_comes_due_and_goes_after_it_ends() {
        let mut s = session(); // now = 10:07
        let id = appt(&mut s, 20, 30, Some(15), None); // 10:27 → reminds at 10:12
        let st = ReminderState::default();
        assert!(due(&s, &st).is_empty());
        assert_eq!(next_due_after(&s, &st, 3600), Some(at(&s, 5).unix - at(&s, 5).unix % 60));
        s.fixed_now = Some(at(&s, 6));
        let d = due(&s, &st);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].item, id);
        assert_eq!(d[0].kind, ReminderKind::Appointment);
        assert_eq!(due_in_text(&d[0], s.now().unix), "In 14 minutes");
        s.fixed_now = Some(at(&s, 30)); // during the meeting: overdue
        assert_eq!(due_in_text(&due(&s, &st)[0], s.now().unix), "16 minutes overdue");
        s.fixed_now = Some(at(&s, 40)); // ended
        assert!(due(&s, &st).is_empty());
    }

    #[test]
    fn dismissing_a_one_off_clears_its_reminder() {
        let mut s = session();
        let id = appt(&mut s, 10, 30, Some(15), None);
        let r = s.run("calendar.reminders", &json!({})).unwrap();
        let key = r["reminders"][0]["key"].as_str().unwrap().to_owned();
        assert_eq!(r["reminders"][0]["dueIn"], "In 10 minutes");
        let r = s.run("calendar.dismissReminder", &json!({ "keys": [key] })).unwrap();
        assert_eq!(r["dismissed"], 1);
        assert_eq!(s.mailbox.appointments[&ItemId(id)].reminder_min, None);
        assert!(r["reminders"].as_array().unwrap().is_empty());
        // Undoable like any change.
        s.run("app.undo", &json!({})).unwrap();
        assert_eq!(s.mailbox.appointments[&ItemId(id)].reminder_min, Some(15));
    }

    #[test]
    fn recurring_series_dismisses_one_occurrence_and_shows_the_next() {
        let mut s = session();
        // Every 30 minutes isn't expressible; a daily series with a two-day reminder has two due at once.
        let id = appt(&mut s, 60, 30, Some(2 * 24 * 60), Some("FREQ=DAILY;COUNT=5"));
        let mut st = ReminderState::default();
        let d = due(&s, &st);
        assert_eq!(d.len(), 1, "one reminder per series");
        assert!(d[0].recurring);
        let first = d[0].key.clone();
        assert_eq!(dismiss(&mut s, &mut st, std::slice::from_ref(&first)), 1);
        assert!(st.dismissed.contains(&first));
        assert!(s.mailbox.appointments[&ItemId(id)].reminder_min.is_some(), "series keeps its reminder");
        let d = due(&s, &st);
        assert_eq!(d.len(), 1);
        assert_ne!(d[0].key, first);
        assert_eq!(d[0].due, Some(at(&s, 60 + 24 * 60).unix));
    }

    #[test]
    fn snooze_hides_until_time_passes() {
        let mut s = session();
        appt(&mut s, 10, 30, Some(15), None);
        let r = s.run("calendar.snoozeReminder", &json!({ "all": true, "minutes": 5 })).unwrap();
        assert_eq!(r["snoozed"], 1);
        let st = r["state"].clone();
        assert!(
            s.run("calendar.reminders", &json!({ "state": st })).unwrap()["reminders"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        s.fixed_now = Some(at(&s, 6));
        let r = s.run("calendar.reminders", &json!({ "state": st })).unwrap();
        assert_eq!(r["reminders"].as_array().unwrap().len(), 1);
        assert!(r["state"]["snoozed"].as_object().unwrap().is_empty(), "spent snoozes are dropped");
    }

    #[test]
    fn tasks_and_flagged_mail_remind_until_dismissed() {
        let mut s = session();
        let reminder = pim::datetime_string(at(&s, -60), 0);
        let t = s.run("tasks.new", &json!({ "subject": "Call Ann", "reminder": reminder })).unwrap()["id"]
            .as_u64()
            .unwrap();
        let inbox = role(&s, FolderRole::Inbox);
        let mail = s.mailbox.add_mail(MailItem {
            folder: inbox,
            subject: "Invoice".into(),
            flag: FlagState::Flagged {
                label: "Follow up".into(),
                start: None,
                due: None,
                reminder: Some(at(&s, -5)),
            },
            ..Default::default()
        });
        let st = ReminderState::default();
        let d = due(&s, &st);
        assert_eq!(d.len(), 2);
        assert_eq!(
            due_in_text(&d.iter().find(|r| r.kind == ReminderKind::Task).unwrap().clone(), s.now().unix),
            "1 hour overdue"
        );
        // Snoozing a task moves its reminder time (kept with the task).
        let r = s
            .run("calendar.snoozeReminder", &json!({ "keys": [format!("task:{t}")], "minutes": 10 }))
            .unwrap();
        assert_eq!(r["snoozed"], 1);
        assert_eq!(s.mailbox.tasks[&ItemId(t)].reminder.unwrap().unix, at(&s, 10).unix);
        // Dismiss the message's reminder; the flag stays.
        s.run("calendar.dismissReminder", &json!({ "keys": [format!("mail:{}", mail.0)] })).unwrap();
        assert!(matches!(s.mailbox.mail[&mail].flag, FlagState::Flagged { reminder: None, .. }));
        assert!(due(&s, &st).is_empty());
        // Completed tasks never remind.
        s.run("tasks.update", &json!({ "id": t, "reminder": reminder, "complete": true })).unwrap();
        assert!(due(&s, &st).is_empty());
    }

    #[test]
    fn contact_reminders_come_due_snooze_and_dismiss() {
        let mut s = session(); // 10:07
        let c = s.run("people.new", &json!({"fullName": "Ann Lee"})).unwrap()["id"].as_u64().unwrap();
        s.selection = crate::Selection {
            folder: role(&s, FolderRole::Contacts),
            items: vec![ItemId(c)],
            focus: Some(ItemId(c)),
        };
        // Comes due at 10:30: the scan wakes up then.
        s.run("people.flag", &json!({"kind": "reminder", "reminder": "2026-10-09T10:30", "label": "Call"}))
            .unwrap();
        let st = ReminderState::default();
        assert!(due(&s, &st).is_empty());
        assert_eq!(next_due_after(&s, &st, 3600), Some(at(&s, 23).unix - at(&s, 23).unix % 60));
        s.fixed_now = Some(at(&s, 25));
        let d = due(&s, &st);
        assert_eq!(d.len(), 1);
        let key = format!("contact:{c}");
        assert_eq!(
            (d[0].key.as_str(), d[0].kind, d[0].subject.as_str()),
            (key.as_str(), ReminderKind::Contact, "Ann Lee")
        );
        // Snooze moves the contact's reminder; dismiss keeps the flag without it.
        let r = s.run("calendar.snoozeReminder", &json!({"keys": [key], "minutes": 10})).unwrap();
        assert_eq!(r["snoozed"], 1);
        assert!(due(&s, &st).is_empty());
        s.fixed_now = Some(at(&s, 10));
        assert_eq!(due(&s, &st).len(), 1);
        let r = s.run("calendar.dismissReminder", &json!({"keys": [key]})).unwrap();
        assert_eq!(r["dismissed"], 1);
        assert!(matches!(s.mailbox.contacts[&ItemId(c)].flag, FlagState::Flagged { reminder: None, .. }));
        assert!(due(&s, &st).is_empty());
        // Opening it opens the contact; a contact that is gone is an error.
        assert_eq!(s.run("calendar.openReminderItem", &json!({"key": key})).unwrap()["dialog"], "contact");
        assert!(s.run("calendar.openReminderItem", &json!({"key": "contact:999999"})).is_err());
        assert_eq!(
            s.run("calendar.dismissReminder", &json!({"keys": ["contact:999999"]})).unwrap()["dismissed"],
            0
        );
    }

    #[test]
    fn deleted_items_do_not_remind() {
        let mut s = session();
        let id = appt(&mut s, 10, 30, Some(15), None);
        s.run("calendar.delete", &json!({ "id": id })).unwrap();
        assert!(due(&s, &ReminderState::default()).is_empty());
    }

    #[test]
    fn open_item_and_bad_params() {
        let mut s = session();
        let id = appt(&mut s, 10, 30, Some(15), None);
        let key = Key::Appointment(ItemId(id), 0).text();
        assert_eq!(
            s.run("calendar.openReminderItem", &json!({ "key": key })).unwrap()["dialog"],
            "appointment"
        );
        assert!(matches!(
            s.run("calendar.openReminderItem", &json!({ "key": "task:999" })),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(s.run("calendar.dismissReminder", &json!({})), Err(CmdError::Params(_))));
        assert!(matches!(
            s.run("calendar.dismissReminder", &json!({ "keys": ["x"] })),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("calendar.snoozeReminder", &json!({ "all": true, "minutes": 0 })),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(s.run("calendar.reminders", &json!({ "state": 5 })), Err(CmdError::Params(_))));
    }
}
