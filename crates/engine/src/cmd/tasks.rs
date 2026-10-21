//! Tasks commands (`tasks.*`) and the To-Do list.
//!
//! Task items live in tasks folders (`Mailbox.tasks`). The To-Do list is the open tasks plus flagged mail
//! and flagged contacts.
//! **Flag-to-task linkage:** `tasks.fromMail` creates a task linked to a flagged message (`Task.from_mail`);
//! completing either side completes the other, and `tasks.removeFromList` clears the message's flag and
//! drops the linked task. Status, percent complete and the completed date are kept consistent: 100 % ⇔
//! Completed ⇔ a completion date.
//!
//! **Recurrence.** A recurring task (`Task.recurrence`) follows an RFC 5545 RRULE anchored at its first due
//! date, expanded with `mailcraft-formats` (`ics::expand_appointment`), or regenerates N days after each
//! completion. Completing it keeps a completed copy (without recurrence) and moves the task to its next
//! occurrence; when the pattern has ended the task simply completes.
//!
//! **Respond.** Reply / Reply All answer a task request (to the person who assigned the task, and the other
//! assignees); Forward sends the task details with the task as an iCalendar VTODO attachment; Meeting
//! starts a meeting about the task. **Task requests** (`tasks.assign`, People › Assign Task) send a
//! message with a VTODO (`METHOD:REQUEST`, written by `mailcraft-formats`) to the assignees and keep an
//! assigned copy (the Assigned view) whose UID ties later answers to it.
//!
//! **Received task requests** (RFC 5546 iTIP with a VTODO: our own `tasks.assign` messages and other
//! clients'): [`task_request_info`] is what the reading pane shows for a message carrying a task
//! (`tasks.requestInfo`). `tasks.acceptRequest` adds the task to my list with `requested_by` (the
//! organizer) and answers with a VTODO `REPLY` (PARTSTAT=ACCEPTED); `tasks.declineRequest` answers
//! DECLINED. `tasks.sendStatusReport` sends the requester the task's progress (REPLY, PARTSTAT IN-PROCESS
//! or COMPLETED), and completing a requested task sends a completion report by itself while Options ›
//! Tasks › "Send status report when I complete an assigned task" is on. On the requester's side
//! [`process_report`] (`tasks.processReport`) applies an answer or report to the assigned copy (accepted /
//! declined, status, % complete, completion date); it only takes answers from the people the task was
//! assigned to.
//!
//! **Views.** [`view_rows`] is the list of the Tasks views (`cmd::view::modules`).

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{Value, json};

use crate::cmd::calendar::{pim, pim::Kind};
use crate::cmd::util::{opt_bool, opt_str};
use crate::cmd::view::modules::{self, ModuleKind, ViewRow};
use crate::cmd::view::table;
use crate::compose::{ComposeAttachment, ComposeState};
use crate::{CmdError, CmdResult, CommandSpec, Session, always};
use mailcraft_model::task::{TaskRecurrence, TaskRequestState};
use mailcraft_model::{
    Appointment, DateTime, Doc, FlagState, FolderId, FolderRole, ItemId, Recurrence, Task, TaskPriority,
    TaskStatus,
};

const TASK_FIELDS: [&str; 12] = [
    "subject",
    "body",
    "start",
    "due",
    "status",
    "priority",
    "percent",
    "reminder",
    "categories",
    "complete",
    "recurrence",
    "private",
];

/// Longest RRULE accepted (bytes).
const MAX_RRULE: usize = 1_024;
/// Most days "Regenerate new task" accepts.
pub const MAX_REGENERATE_DAYS: u32 = 999;
/// Furthest ahead (days) the next occurrence of a recurring task is looked for.
const NEXT_OCCURRENCE_HORIZON_DAYS: i64 = 400 * 366;

fn parse_status(s: &str) -> Option<TaskStatus> {
    match s {
        "notStarted" => Some(TaskStatus::NotStarted),
        "inProgress" => Some(TaskStatus::InProgress),
        "completed" => Some(TaskStatus::Completed),
        "waitingOnSomeoneElse" => Some(TaskStatus::WaitingOnSomeoneElse),
        "deferred" => Some(TaskStatus::Deferred),
        _ => None,
    }
}

pub fn status_name(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::NotStarted => "notStarted",
        TaskStatus::InProgress => "inProgress",
        TaskStatus::Completed => "completed",
        TaskStatus::WaitingOnSomeoneElse => "waitingOnSomeoneElse",
        TaskStatus::Deferred => "deferred",
    }
}

fn priority_name(p: TaskPriority) -> &'static str {
    match p {
        TaskPriority::Low => "low",
        TaskPriority::Normal => "normal",
        TaskPriority::High => "high",
    }
}

/// Marks a task complete (or not) keeping status / percent / completed date consistent.
pub fn set_complete(t: &mut Task, done: bool, now: DateTime) {
    t.modified = Some(now);
    if done {
        t.status = TaskStatus::Completed;
        t.percent_complete = 100;
        if t.completed.is_none() {
            t.completed = Some(now);
        }
    } else {
        if t.status == TaskStatus::Completed {
            t.status = TaskStatus::NotStarted;
        }
        if t.percent_complete >= 100 {
            t.percent_complete = 0;
        }
        t.completed = None;
    }
}

/// Applies task field params (shared by new and update).
pub fn apply_task_fields(s: &Session, t: &mut Task, p: &Value) -> Result<(), CmdError> {
    let tz = s.tz_offset_min;
    let now = s.now();
    if let Some(v) = pim::text(p, "subject")? {
        t.subject = v;
    }
    if let Some(v) = pim::text(p, "body")? {
        t.body = v;
    }
    // Start and due are dates: stored at local midnight.
    if let Some(v) = pim::date(p, "start", tz)? {
        t.start = v.map(|d| pim::local_midnight(d, tz));
    }
    if let Some(v) = pim::date(p, "due", tz)? {
        t.due = v.map(|d| pim::local_midnight(d, tz));
    }
    if let (Some(st), Some(du)) = (t.start, t.due)
        && du.unix < st.unix
    {
        return Err(CmdError::Params("the due date is before the start date".into()));
    }
    if let Some(v) = pim::date(p, "reminder", tz)? {
        t.reminder = v;
    }
    if let Some(v) = opt_str(p, "priority") {
        t.priority = match v {
            "low" => TaskPriority::Low,
            "normal" => TaskPriority::Normal,
            "high" => TaskPriority::High,
            _ => return Err(CmdError::Params("`priority` must be low|normal|high".into())),
        };
    }
    if let Some(v) = pim::list(p, "categories")? {
        t.categories = v;
    }
    if let Some(v) = p.get("status").filter(|v| !v.is_null()) {
        let st = v.as_str().and_then(parse_status).ok_or_else(|| {
            CmdError::Params(
                "`status` must be notStarted|inProgress|completed|waitingOnSomeoneElse|deferred".into(),
            )
        })?;
        if st == TaskStatus::Completed {
            set_complete(t, true, now);
        } else {
            set_complete(t, false, now);
            t.status = st;
        }
    }
    if let Some(v) = p.get("percent").filter(|v| !v.is_null()) {
        let pct = v
            .as_u64()
            .filter(|x| *x <= 100)
            .and_then(|x| u8::try_from(x).ok())
            .ok_or_else(|| CmdError::Params("`percent` must be 0..100".into()))?;
        if pct == 100 {
            set_complete(t, true, now);
        } else {
            if t.status == TaskStatus::Completed {
                set_complete(t, false, now);
                t.status = if pct > 0 { TaskStatus::InProgress } else { TaskStatus::NotStarted };
            } else if pct > 0 && t.status == TaskStatus::NotStarted {
                t.status = TaskStatus::InProgress;
            }
            t.percent_complete = pct;
        }
    }
    if let Some(done) = pim::flag(p, "complete")? {
        set_complete(t, done, now);
    }
    if let Some(v) = pim::flag(p, "private")? {
        t.private = v;
    }
    if let Some(r) = p.get("recurrence") {
        t.recurrence = parse_recurrence(r, t, p)?;
    }
    t.modified = Some(s.now());
    Ok(())
}

/// `recurrence` param: `null` / `""` (none), an RRULE string, or `{"rrule"?, "regenerateDays"?}`. The
/// first occurrence is the task's due date (else its start date, else today).
fn parse_recurrence(v: &Value, t: &Task, p: &Value) -> Result<Option<TaskRecurrence>, CmdError> {
    let (rrule, regen) = match v {
        Value::Null => return Ok(None),
        Value::String(r) => (r.trim().to_owned(), 0u32),
        Value::Object(o) => {
            let rrule = match o.get("rrule") {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(r)) => r.trim().to_owned(),
                Some(_) => return Err(CmdError::Params("`recurrence.rrule` must be a string".into())),
            };
            let regen = match o.get("regenerateDays") {
                None | Some(Value::Null) => 0,
                Some(n) => n
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| (1..=MAX_REGENERATE_DAYS).contains(n))
                    .ok_or_else(|| {
                        CmdError::Params(format!("`regenerateDays` must be 1..{MAX_REGENERATE_DAYS}"))
                    })?,
            };
            (rrule, regen)
        }
        _ => return Err(CmdError::Params("`recurrence` must be an RRULE string, an object or null".into())),
    };
    let rrule = rrule.strip_prefix("RRULE:").map(str::to_owned).unwrap_or(rrule);
    if rrule.is_empty() && regen == 0 {
        return Ok(None);
    }
    if regen == 0 {
        if rrule.len() > MAX_RRULE {
            return Err(CmdError::Params("the recurrence rule is too long".into()));
        }
        mailcraft_formats::ics::Rrule::parse(&rrule)
            .map_err(|e| CmdError::Params(format!("invalid recurrence rule: {e}")))?;
    }
    // Keep the anchor and the count of completed occurrences while the rule stays the same.
    let same = t.recurrence.as_ref().filter(|r| r.rrule == rrule && r.regenerate_days == regen);
    let anchor = match same {
        Some(r) if p.get("due").is_none() => r.start,
        _ => t.due.or(t.start),
    };
    Ok(Some(TaskRecurrence {
        rrule: if regen > 0 { String::new() } else { rrule },
        start: anchor,
        regenerate_days: regen,
        completed_count: same.map_or(0, |r| r.completed_count),
    }))
}

/// Due date of the occurrence after `t`'s current one (`None` when the pattern has ended or the task does
/// not recur). `done_at` is the completion time (Regenerate counts from it).
pub fn next_occurrence(t: &Task, done_at: DateTime, tz: i32) -> Option<DateTime> {
    let r = t.recurrence.as_ref()?;
    if r.regenerate_days > 0 {
        let day = done_at.day_number(tz).saturating_add(i64::from(r.regenerate_days));
        return Some(day_start(day, tz));
    }
    let current = t.due.or(t.start)?;
    let anchor = r.start.unwrap_or(current);
    let a = Appointment {
        start: anchor,
        end: anchor,
        all_day: true,
        recurrence: Some(Recurrence { rrule: r.rrule.clone(), exdates: Vec::new() }),
        ..Default::default()
    };
    // The next occurrence strictly after the current due day.
    let from =
        DateTime::from_unix(day_start(current.day_number(tz), tz).unix.saturating_add(pim::DAY_SECS), tz);
    let to = DateTime::from_unix(
        from.unix.saturating_add(NEXT_OCCURRENCE_HORIZON_DAYS.saturating_mul(pim::DAY_SECS)),
        tz,
    );
    mailcraft_formats::ics::expand_appointment(&a, from, to, 1).first().map(|(s, _)| *s)
}

/// Local midnight of day number `day`.
fn day_start(day: i64, tz: i32) -> DateTime {
    let unix = day.saturating_mul(pim::DAY_SECS).saturating_sub(i64::from(tz).saturating_mul(60));
    DateTime::from_unix(unix, tz)
}

/// After task `id` was completed: a recurring task keeps a completed copy and moves on to its next
/// occurrence (start, due and reminder shift by the same amount). Returns the copy's id.
fn advance_recurring(s: &mut Session, id: ItemId) -> Option<ItemId> {
    let t = s.mailbox.tasks.get(&id)?.clone();
    if !t.is_complete() || t.recurrence.is_none() {
        return None;
    }
    let tz = s.tz_offset_min;
    let done_at = t.completed.unwrap_or_else(|| s.now());
    let next = next_occurrence(&t, done_at, tz)?;
    let copy_id = ItemId(s.mailbox.alloc_id());
    let mut copy = t.clone();
    copy.id = copy_id;
    copy.recurrence = None;
    copy.from_mail = None;
    copy.reminder = None;
    // The completed occurrence is an item of its own: the series keeps the UID and the server copy.
    copy.remote = None;
    if !copy.uid.trim().is_empty() {
        copy.uid = crate::cmd::calendar::new_uid(s, copy_id, s.now().unix);
    }
    s.mailbox.tasks.insert(copy_id, copy);
    let shift = next.unix.saturating_sub(t.due.or(t.start).map_or(next.unix, |d| d.unix));
    let mut t = t;
    t.start = t.start.map(|d| pim::add_secs(d, shift));
    t.due = Some(next);
    t.reminder = t.reminder.map(|d| pim::add_secs(d, shift));
    t.status = TaskStatus::NotStarted;
    t.percent_complete = 0;
    t.completed = None;
    if let Some(r) = t.recurrence.as_mut() {
        r.completed_count = r.completed_count.saturating_add(1);
        if r.start.is_none() {
            r.start = Some(next);
        }
    }
    s.mailbox.tasks.insert(id, t);
    Some(copy_id)
}

/// "none", "waiting", "accepted" or "declined".
pub fn request_state_name(r: TaskRequestState) -> &'static str {
    match r {
        TaskRequestState::None => "none",
        TaskRequestState::Waiting => "waiting",
        TaskRequestState::Accepted => "accepted",
        TaskRequestState::Declined => "declined",
    }
}

/// Summary of a task for results.
pub fn task_json(t: &Task, tz: i32) -> Value {
    json!({
        "id": t.id.0,
        "folder": t.folder.0,
        "subject": t.subject,
        "start": t.start.map(|d| pim::date_string(d, tz)),
        "due": t.due.map(|d| pim::date_string(d, tz)),
        "status": status_name(t.status),
        "priority": priority_name(t.priority),
        "percent": t.percent_complete,
        "complete": t.status == TaskStatus::Completed,
        "categories": t.categories,
        "fromMail": t.from_mail.map(|m| m.0),
        "recurrence": t.recurrence.as_ref().map(|r| json!({
            "rrule": r.rrule,
            "regenerateDays": r.regenerate_days,
            "start": r.start.map(|d| pim::date_string(d, tz)),
        })),
        "private": t.private,
        "assignedTo": t.assigned_to,
        "requestedBy": t.requested_by,
        "requestState": request_state_name(t.request_state),
    })
}

/// Creates a task in `folder` from field params; returns its id. With Options › Tasks › "Set reminders on
/// tasks with due dates" on, a task with a due date and no `reminder` param gets a reminder on the due date
/// at the default reminder time (`"reminder": null` asks for none).
pub fn create_task(s: &mut Session, folder: FolderId, p: &Value) -> Result<ItemId, CmdError> {
    let mut t = Task { folder, ..Default::default() };
    apply_task_fields(s, &mut t, p)?;
    if p.get("reminder").is_none()
        && t.reminder.is_none()
        && !t.is_complete()
        && s.settings.task_reminders_on_due()
        && let Some(due) = t.due
    {
        t.reminder = Some(pim::add_secs(due, i64::from(s.settings.task_reminder_minutes()) * 60));
    }
    let id = ItemId(s.mailbox.alloc_id());
    t.id = id;
    s.mailbox.tasks.insert(id, t);
    Ok(id)
}

fn run_new(s: &mut Session, p: &Value) -> CmdResult {
    let folder = pim::new_item_folder(s, p, Kind::Task)?;
    if !pim::has_any(p, &TASK_FIELDS) {
        return pim::dialog(s, "task", json!({ "folder": folder.0, "focus": "subject" }));
    }
    let id = create_task(s, folder, p)?;
    let tz = s.tz_offset_min;
    Ok(s.mailbox.tasks.get(&id).map_or_else(|| json!({ "id": id.0 }), |t| task_json(t, tz)))
}

/// Mirrors a task's completion onto its linked message flag.
fn sync_mail_flag(s: &mut Session, task: ItemId) {
    let Some(t) = s.mailbox.tasks.get(&task) else { return };
    let (Some(mail), done, completed) = (t.from_mail, t.status == TaskStatus::Completed, t.completed) else {
        return;
    };
    if let Some(m) = s.mailbox.mail.get_mut(&mail) {
        if done {
            m.flag = FlagState::Complete { completed };
        } else if matches!(m.flag, FlagState::Complete { .. }) {
            m.flag = FlagState::Flagged {
                label: "Follow up".into(),
                start: t.start,
                due: t.due,
                reminder: t.reminder,
            };
        }
    }
}

fn run_update(s: &mut Session, p: &Value) -> CmdResult {
    with_reports(s, p, run_update_now)
}

fn run_update_now(s: &mut Session, p: &Value) -> CmdResult {
    let ids = pim::require_targets(s, p, Kind::Task)?;
    let mut updated = Vec::new();
    for id in &ids {
        let Some(mut t) = s.mailbox.tasks.get(id).cloned() else { continue };
        let was_done = t.is_complete();
        apply_task_fields(s, &mut t, p)?;
        updated.push((t, was_done));
    }
    let tz = s.tz_offset_min;
    let mut out: Vec<Value> = Vec::new();
    let mut copies = Vec::new();
    for (t, was_done) in updated {
        let id = t.id;
        s.mailbox.tasks.insert(id, t);
        sync_mail_flag(s, id);
        if !was_done && let Some(c) = advance_recurring(s, id) {
            copies.push(c);
        }
        if let Some(t) = s.mailbox.tasks.get(&id) {
            out.push(task_json(t, tz));
        }
    }
    Ok(json!({ "updated": out, "completedCopies": pim::ids_json(&copies) }))
}

/// Delete. Deleting a task I accepted from someone else's task request first asks what to tell them
/// (`request`): `decline` answers DECLINED, `complete` marks it complete and sends the completion report,
/// `delete` just deletes. The answers are sent at once; one that cannot be sent stays open in a message
/// window. Flagged messages and flagged contacts of the To-Do List are deleted as Mail and People delete them.
fn run_delete(s: &mut Session, p: &Value) -> CmdResult {
    let r = require_rows(s, p)?;
    let ids = r.tasks.clone();
    let requested: Vec<ItemId> = ids
        .iter()
        .filter(|i| {
            s.mailbox.tasks.get(i).is_some_and(|t| {
                t.is_requested() && !t.is_complete() && t.request_state == TaskRequestState::Accepted
            })
        })
        .copied()
        .collect();
    let mut notified = 0usize;
    match opt_str(p, "request") {
        None if !requested.is_empty() => {
            let subject = requested
                .first()
                .and_then(|i| s.mailbox.tasks.get(i))
                .map(|t| t.subject.clone())
                .unwrap_or_default();
            return pim::dialog(
                s,
                "deleteTaskRequest",
                json!({ "ids": pim::ids_json(&r.all()), "subject": subject, "requested": requested.len() }),
            );
        }
        None | Some("delete") => {}
        Some("decline") => {
            for id in &requested {
                let Some(t) = s.mailbox.tasks.get(id).cloned() else { continue };
                let to = reply_targets(s, &t, false);
                let subject = format!("Task Declined: {}", t.subject.trim());
                let intro = "I have declined this task.";
                if send_task_reply(s, &t, &to, subject, intro, "DECLINED", SendMode::Now).is_ok_and(|r| r.1) {
                    notified = notified.saturating_add(1);
                }
            }
        }
        Some("complete") => {
            let now = s.now();
            for id in &requested {
                if let Some(t) = s.mailbox.tasks.get_mut(id) {
                    set_complete(t, true, now);
                }
                if send_status_report(s, *id, SendMode::Now).is_ok_and(|r| r.1) {
                    notified = notified.saturating_add(1);
                }
            }
        }
        Some(_) => return Err(CmdError::Params("`request` must be decline, complete or delete".into())),
    }
    let mut deleted = pim::delete(s, &ids);
    if !r.mail.is_empty() {
        route(s, "mail.delete", &r.mail, json!({}))?;
        // Options › Mail › "Mark messages as read when deleted", as Mail's Delete does.
        if s.settings.bool("mail.other.markReadOnDelete") {
            s.mailbox.set_read(&r.mail, true);
        }
        deleted.extend(r.mail.iter().copied());
    }
    if !r.contacts.is_empty() {
        route(s, "people.delete", &r.contacts, json!({}))?;
        deleted.extend(r.contacts.iter().copied());
    }
    Ok(json!({
        "deleted": pim::ids_json(&deleted),
        "mail": pim::ids_json(&r.mail),
        "contacts": pim::ids_json(&r.contacts),
        "notified": notified,
    }))
}
fn run_copy(s: &mut Session, p: &Value) -> CmdResult {
    move_rows(s, p, true)
}
fn run_move(s: &mut Session, p: &Value) -> CmdResult {
    move_rows(s, p, false)
}

/// Move / Copy to Folder on To-Do List rows. Without a folder: tasks (or a mix of rows) get the tasks
/// folder picker; flagged messages alone get Mail's folder picker, flagged contacts alone People's. With a
/// folder, the rows that folder can hold go there (tasks to a tasks folder, messages to a mail folder,
/// contacts to a contacts folder); the others stay where they are and are reported as `skipped`.
fn move_rows(s: &mut Session, p: &Value, copy: bool) -> CmdResult {
    let r = require_rows(s, p)?;
    let Some(folder) = crate::cmd::util::opt_folder(p) else {
        return match (r.tasks.is_empty(), r.mail.is_empty(), r.contacts.is_empty()) {
            (true, false, true) => route(s, if copy { "mail.copy" } else { "mail.move" }, &r.mail, json!({})),
            (true, true, false) => {
                route(s, if copy { "people.copy" } else { "people.move" }, &r.contacts, json!({}))
            }
            _ => {
                let current = r.tasks.first().and_then(|i| pim::folder_of(&s.mailbox, *i)).map(|f| f.0);
                pim::dialog(
                    s,
                    "pimMove",
                    json!({ "ids": pim::ids_json(&r.all()), "area": "tasks", "copy": copy, "current": current }),
                )
            }
        };
    };
    let kind = s
        .mailbox
        .folder(folder)
        .map(|f| f.kind)
        .ok_or_else(|| CmdError::Params(format!("no such folder {}", folder.0)))?;
    let (fits, cmd, skipped): (&[ItemId], &str, Vec<ItemId>) = match kind {
        mailcraft_model::FolderKind::Tasks => {
            (&r.tasks, "", r.mail.iter().chain(&r.contacts).copied().collect())
        }
        mailcraft_model::FolderKind::Mail => (
            &r.mail,
            if copy { "mail.copy" } else { "mail.move" },
            r.tasks.iter().chain(&r.contacts).copied().collect(),
        ),
        mailcraft_model::FolderKind::Contacts => (
            &r.contacts,
            if copy { "people.copy" } else { "people.move" },
            r.tasks.iter().chain(&r.mail).copied().collect(),
        ),
        _ => (&[], "", r.all()),
    };
    if fits.is_empty() {
        return Err(CmdError::Params(format!("folder {} cannot hold the selected items", folder.0)));
    }
    let fits = fits.to_vec();
    let done = if cmd.is_empty() {
        if copy {
            pim::copy_to(s, &fits, folder, Kind::Task)?
        } else {
            pim::move_to(s, &fits, folder, Kind::Task)?
        }
    } else {
        let out = route(s, cmd, &fits, json!({ "folder": folder.0 }))?;
        ["ids", "copies", "moved"]
            .iter()
            .find_map(|k| out.get(*k).and_then(Value::as_array))
            .map(|a| a.iter().filter_map(Value::as_u64).map(ItemId).collect())
            .unwrap_or(fits)
    };
    let key = if copy { "copies" } else { "moved" };
    Ok(json!({ key: pim::ids_json(&done), "skipped": pim::ids_json(&skipped), "folder": folder.0 }))
}

/// Move › Other Folder / Copy to Folder without a folder: the folder picker of the module (`pimMove`), which
/// runs `<area>.move` / `<area>.copy` with the chosen folder.
pub(crate) fn move_dialog(s: &mut Session, p: &Value, kind: Kind, copy: bool) -> CmdResult {
    let ids = pim::require_targets(s, p, kind)?;
    let area = area_of(kind);
    let current = ids.first().and_then(|i| pim::folder_of(&s.mailbox, *i)).map(|f| f.0);
    pim::dialog(
        s,
        "pimMove",
        json!({ "ids": pim::ids_json(&ids), "area": area, "copy": copy, "current": current }),
    )
}

/// Categorize › All Categories for module items: the Color Categories dialog (the one Mail uses: New, Rename,
/// Delete, Color, Shortcut Key), which assigns with `<area>.categorize`.
pub(crate) fn categories_dialog(s: &mut Session, p: &Value, kind: Kind) -> CmdResult {
    let ids = pim::require_targets(s, p, kind)?;
    let mut on: Vec<String> = Vec::new();
    for id in &ids {
        for c in pim::categories(&s.mailbox, *id).into_iter().flatten() {
            if !on.iter().any(|x| x.eq_ignore_ascii_case(c)) {
                on.push(c.clone());
            }
        }
    }
    pim::dialog(s, "colorCategories", json!({ "ids": pim::ids_json(&ids), "area": area_of(kind), "on": on }))
}

/// Command area of a PIM kind ("people", "tasks", "notes", "calendar").
pub(crate) fn area_of(kind: Kind) -> &'static str {
    match kind {
        Kind::Appointment => "calendar",
        Kind::Contact => "people",
        Kind::Task => "tasks",
        Kind::Note => "notes",
    }
}

/// All Categories on To-Do List rows (tasks, flagged messages and contacts): the Color Categories dialog.
fn run_all_categories(s: &mut Session, p: &Value) -> CmdResult {
    let ids = require_rows(s, p)?.all();
    let mut on: Vec<String> = Vec::new();
    for id in &ids {
        for c in pim::categories(&s.mailbox, *id).into_iter().flatten() {
            if !on.iter().any(|x| x.eq_ignore_ascii_case(c)) {
                on.push(c.clone());
            }
        }
    }
    pim::dialog(s, "colorCategories", json!({ "ids": pim::ids_json(&ids), "area": "tasks", "on": on }))
}

/// Categorize on To-Do List rows: tasks, flagged messages and flagged contacts alike.
fn run_categorize(s: &mut Session, p: &Value) -> CmdResult {
    let ids = require_rows(s, p)?.all();
    let out = pim::categorize_ids(s, p, &ids)?;
    let now = s.now();
    for id in out.get("ids").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_u64) {
        if let Some(t) = s.mailbox.tasks.get_mut(&ItemId(id)) {
            t.modified = Some(now);
        }
    }
    Ok(out)
}

/// To-do targets: (tasks, flagged messages) among the ids a command acts on.
fn todo_targets(s: &Session, p: &Value) -> (Vec<ItemId>, Vec<ItemId>) {
    let ids = match crate::cmd::util::opt_u64(p, "id") {
        Some(id) => vec![ItemId(id)],
        None => s.target_ids(p),
    };
    let tasks = ids.iter().copied().filter(|i| s.mailbox.tasks.contains_key(i)).collect();
    let mail = ids
        .iter()
        .copied()
        .filter(|i| {
            s.mailbox.mail.get(i).is_some_and(|m| !matches!(m.flag, FlagState::None))
                || s.mailbox.contacts.get(i).is_some_and(|c| !matches!(c.flag, FlagState::None))
        })
        .collect();
    (tasks, mail)
}

fn todo_selected(s: &Session) -> Option<&'static str> {
    let (t, m) = todo_targets(s, &Value::Null);
    if t.is_empty() && m.is_empty() { Some("nothing selected") } else { None }
}

// ---------------------------------------------------------------------------------------------------------
// To-Do List rows: the Tasks Home ribbon on flagged messages and flagged contacts

/// Kind of a To-Do List row a Tasks Home command acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowOf {
    Task,
    Mail,
    Contact,
}

/// What a Tasks Home command acts on, split by row kind: tasks, and the flagged messages and flagged contacts
/// the To-Do List shows beside them (a message or contact counts while it carries a flag, complete or not).
/// `order` keeps the targets in the order given.
#[derive(Clone, Debug, Default)]
struct Rows {
    tasks: Vec<ItemId>,
    mail: Vec<ItemId>,
    contacts: Vec<ItemId>,
    order: Vec<(ItemId, RowOf)>,
}

impl Rows {
    fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
    fn first(&self) -> Option<(ItemId, RowOf)> {
        self.order.first().copied()
    }
    fn all(&self) -> Vec<ItemId> {
        self.order.iter().map(|(i, _)| *i).collect()
    }
}

/// Kind of row `id` is in the To-Do List, if any.
fn row_of(s: &Session, id: ItemId) -> Option<RowOf> {
    if s.mailbox.tasks.contains_key(&id) {
        Some(RowOf::Task)
    } else if s.mailbox.mail.get(&id).is_some_and(|m| !matches!(m.flag, FlagState::None)) {
        Some(RowOf::Mail)
    } else if s.mailbox.contacts.get(&id).is_some_and(|c| !matches!(c.flag, FlagState::None)) {
        Some(RowOf::Contact)
    } else {
        None
    }
}

/// The rows a Tasks Home command acts on: `params.id`, else `params.ids`, else the selection (else focus).
fn rows(s: &Session, p: &Value) -> Rows {
    let ids = match crate::cmd::util::opt_u64(p, "id") {
        Some(id) => vec![ItemId(id)],
        None => s.target_ids(p),
    };
    let mut out = Rows::default();
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            continue;
        }
        let Some(kind) = row_of(s, id) else { continue };
        match kind {
            RowOf::Task => out.tasks.push(id),
            RowOf::Mail => out.mail.push(id),
            RowOf::Contact => out.contacts.push(id),
        }
        out.order.push((id, kind));
    }
    out
}

const NO_ROW: &str = "no task or flagged item selected (pass `id` or `ids`)";

fn require_rows(s: &Session, p: &Value) -> Result<Rows, CmdError> {
    let r = rows(s, p);
    if r.is_empty() { Err(CmdError::Params(NO_ROW.into())) } else { Ok(r) }
}

/// Runs the Mail or People command that does for flagged messages / contacts what a Tasks Home command does
/// for tasks, on `ids` with `extra` params (as part of the Tasks command: one undo step).
fn route(s: &mut Session, cmd: &str, ids: &[ItemId], extra: Value) -> CmdResult {
    let mut params = match extra {
        Value::Object(o) => o,
        _ => serde_json::Map::new(),
    };
    params.remove("id");
    params.insert("ids".into(), pim::ids_json(ids));
    crate::rules_exec::invoke(s, cmd, &Value::Object(params))
}

/// Enabled: a task, flagged message or flagged contact is selected.
fn row_selected(s: &Session) -> Option<&'static str> {
    if rows(s, &Value::Null).is_empty() { Some("select a task or a flagged item first") } else { None }
}

fn linked_tasks(s: &Session, mail: ItemId) -> Vec<ItemId> {
    s.mailbox.tasks.values().filter(|t| t.from_mail == Some(mail)).map(|t| t.id).collect()
}

fn run_mark_complete(s: &mut Session, p: &Value) -> CmdResult {
    with_reports(s, p, run_mark_complete_now)
}

fn run_mark_complete_now(s: &mut Session, p: &Value) -> CmdResult {
    let (tasks, mail) = todo_targets(s, p);
    if tasks.is_empty() && mail.is_empty() {
        return Err(CmdError::Params("no task or flagged item selected".into()));
    }
    let now = s.now();
    let mut all_tasks = tasks.clone();
    for m in &mail {
        if let Some(item) = s.mailbox.mail.get_mut(m) {
            item.flag = FlagState::Complete { completed: Some(now) };
        }
        all_tasks.extend(linked_tasks(s, *m));
    }
    let mut copies = Vec::new();
    for id in &all_tasks {
        let mut was_done = true;
        if let Some(t) = s.mailbox.tasks.get_mut(id) {
            was_done = t.is_complete();
            set_complete(t, true, now);
        }
        sync_mail_flag(s, *id);
        if !was_done && let Some(c) = advance_recurring(s, *id) {
            copies.push(c);
        }
    }
    // Flagged contacts in the To-Do List complete like messages.
    let (contacts, mail): (Vec<ItemId>, Vec<ItemId>) =
        mail.into_iter().partition(|i| s.mailbox.contacts.contains_key(i));
    for c in &contacts {
        if let Some(contact) = s.mailbox.contacts.get_mut(c) {
            contact.flag = FlagState::Complete { completed: Some(now) };
        }
    }
    Ok(json!({
        "tasks": pim::ids_json(&all_tasks),
        "mail": pim::ids_json(&mail),
        "contacts": pim::ids_json(&contacts),
        "completedCopies": pim::ids_json(&copies),
    }))
}

fn run_remove_from_list(s: &mut Session, p: &Value) -> CmdResult {
    let (tasks, mail) = todo_targets(s, p);
    if tasks.is_empty() && mail.is_empty() {
        return Err(CmdError::Params("no task or flagged item selected".into()));
    }
    let mut cleared = Vec::new();
    let mut drop_links = Vec::new();
    let mut plain = Vec::new();
    for id in &tasks {
        match s.mailbox.tasks.get(id).and_then(|t| t.from_mail) {
            Some(m) => {
                cleared.push(m);
                drop_links.push(*id);
            }
            None => plain.push(*id),
        }
    }
    for m in &mail {
        cleared.push(*m);
        drop_links.extend(linked_tasks(s, *m));
    }
    cleared.sort();
    cleared.dedup();
    drop_links.sort();
    drop_links.dedup();
    for m in &cleared {
        if let Some(item) = s.mailbox.mail.get_mut(m) {
            item.flag = FlagState::None;
        }
        if let Some(c) = s.mailbox.contacts.get_mut(m) {
            c.flag = FlagState::None;
        }
    }
    for t in &drop_links {
        s.mailbox.tasks.remove(t);
    }
    let deleted = pim::delete(s, &plain);
    for id in drop_links.iter().chain(deleted.iter()).chain(cleared.iter()) {
        s.selection.items.retain(|i| i != id);
        if s.selection.focus == Some(*id) {
            s.selection.focus = None;
        }
    }
    Ok(json!({
        "unflagged": pim::ids_json(&cleared),
        "deleted": pim::ids_json(&deleted),
        "unlinked": pim::ids_json(&drop_links),
    }))
}

/// Creates (or refreshes) tasks linked to flagged messages; unflagged messages get a "Follow up" flag.
fn run_from_mail(s: &mut Session, p: &Value) -> CmdResult {
    let ids: Vec<ItemId> = s.target_ids(p).into_iter().filter(|i| s.mailbox.mail.contains_key(i)).collect();
    if ids.is_empty() {
        return Err(CmdError::Params("no message selected".into()));
    }
    let mut out = Vec::new();
    for m in ids {
        let Some(item) = s.mailbox.mail.get(&m).cloned() else { continue };
        let (start, due, reminder, done) = match &item.flag {
            FlagState::Flagged { start, due, reminder, .. } => (*start, *due, *reminder, false),
            FlagState::Complete { .. } => (None, None, None, true),
            FlagState::None => {
                if let Some(mi) = s.mailbox.mail.get_mut(&m) {
                    mi.flag = FlagState::Flagged {
                        label: "Follow up".into(),
                        start: None,
                        due: None,
                        reminder: None,
                    };
                }
                (None, None, None, false)
            }
        };
        let existing = linked_tasks(s, m).first().copied();
        let folder = s
            .mailbox
            .folder_account(item.folder)
            .and_then(|a| s.mailbox.role_folder(a, FolderRole::Tasks))
            .or_else(|| pim::default_folder(s, Kind::Task))
            .ok_or_else(|| CmdError::Failed("there is no tasks folder".into()))?;
        let now = s.now();
        let id = existing.unwrap_or_else(|| ItemId(s.mailbox.alloc_id()));
        let mut t = s.mailbox.tasks.get(&id).cloned().unwrap_or(Task { id, folder, ..Default::default() });
        t.subject = item.subject.clone();
        t.start = start;
        t.due = due;
        t.reminder = reminder;
        t.from_mail = Some(m);
        set_complete(&mut t, done, now);
        s.mailbox.tasks.insert(id, t);
        out.push(id);
    }
    Ok(json!({ "tasks": pim::ids_json(&out) }))
}

/// One entry of the To-Do List or a Tasks view before grouping.
#[derive(Clone, Debug)]
struct Entry {
    id: ItemId,
    kind: modules::RowKind,
    folder: FolderId,
    subject: String,
    start: Option<DateTime>,
    due: Option<DateTime>,
    priority: TaskPriority,
    categories: Vec<String>,
}

fn task_entry(t: &Task) -> Entry {
    Entry {
        id: t.id,
        kind: modules::RowKind::Task,
        folder: t.folder,
        subject: t.subject.clone(),
        start: t.start,
        due: t.due,
        priority: t.priority,
        categories: t.categories.clone(),
    }
}

/// The To-Do List entries: open tasks of every tasks folder (outside Deleted Items), flagged messages
/// without a linked task and flagged contacts.
fn todo_entries(s: &Session) -> Vec<Entry> {
    let in_deleted = |f: FolderId| {
        s.mailbox
            .folder_account(f)
            .and_then(|a| s.mailbox.role_folder(a, FolderRole::DeletedItems))
            .is_some_and(|d| s.mailbox.is_in_subtree(f, d))
    };
    let mut out: Vec<Entry> = Vec::new();
    for t in s.mailbox.tasks.values() {
        if t.is_complete() || in_deleted(t.folder) {
            continue;
        }
        out.push(task_entry(t));
    }
    let linked: std::collections::BTreeSet<ItemId> =
        s.mailbox.tasks.values().filter_map(|t| t.from_mail).collect();
    for m in s.mailbox.mail.values() {
        let FlagState::Flagged { start, due, .. } = &m.flag else { continue };
        if in_deleted(m.folder) || linked.contains(&m.id) {
            continue;
        }
        let priority = match m.importance {
            mailcraft_model::Importance::High => TaskPriority::High,
            mailcraft_model::Importance::Low => TaskPriority::Low,
            mailcraft_model::Importance::Normal => TaskPriority::Normal,
        };
        out.push(Entry {
            id: m.id,
            kind: modules::RowKind::Mail,
            folder: m.folder,
            subject: m.subject.clone(),
            start: *start,
            due: *due,
            priority,
            categories: m.categories.clone(),
        });
    }
    for c in s.mailbox.contacts.values() {
        let FlagState::Flagged { start, due, .. } = &c.flag else { continue };
        if in_deleted(c.folder) {
            continue;
        }
        out.push(Entry {
            id: c.id,
            kind: modules::RowKind::Contact,
            folder: c.folder,
            subject: crate::cmd::people::display_name(c),
            start: *start,
            due: *due,
            priority: TaskPriority::Normal,
            categories: c.categories.clone(),
        });
    }
    out
}

/// The To-Do list: open tasks (outside Deleted Items), flagged messages without a linked task and flagged
/// contacts, ordered by due date (no date last), then subject.
pub fn todo_list(s: &Session) -> Vec<Value> {
    let tz = s.tz_offset_min;
    let mut rows: Vec<(i64, String, Value)> = Vec::new();
    for e in todo_entries(s) {
        let v = match e.kind {
            modules::RowKind::Task => {
                let Some(t) = s.mailbox.tasks.get(&e.id) else { continue };
                let mut v = task_json(t, tz);
                if let Some(o) = v.as_object_mut() {
                    o.insert("kind".into(), json!("task"));
                }
                v
            }
            kind => json!({
                "kind": if kind == modules::RowKind::Contact { "contact" } else { "mail" },
                "id": e.id.0,
                "folder": e.folder.0,
                "subject": e.subject,
                "due": e.due.map(|d| pim::date_string(d, tz)),
            }),
        };
        rows.push((e.due.map_or(i64::MAX, |d| d.unix), e.subject.to_lowercase(), v));
    }
    rows.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    rows.into_iter().map(|(_, _, v)| v).collect()
}

/// Rows of the selected Tasks view of `folder` (`cmd::view::modules`): the view's filter, its arrangement
/// as groups when Show in Groups is on, ordered by due date (no date last) then subject, all reversed by
/// Reverse Sort.
pub fn view_rows(s: &Session, folder: FolderId) -> Vec<ViewRow> {
    let kind = ModuleKind::Tasks;
    let st = modules::state(s, folder, kind);
    let tz = s.tz_offset_min;
    let today = s.now().day_number(tz);
    let day = |t: Option<DateTime>| t.map(|d| d.day_number(tz));
    let entries: Vec<Entry> = if st.view == "To-Do List" {
        todo_entries(s)
    } else {
        s.mailbox
            .tasks
            .values()
            .filter(|t| t.folder == folder)
            .filter(|t| {
                let due = day(t.due);
                let open = !t.is_complete();
                match st.view.as_str() {
                    "Active" => open,
                    "Completed" => !open,
                    "Today" => open && due.is_some_and(|d| d <= today),
                    "Next 7 Days" => open && due.is_some_and(|d| d >= today && d < today.saturating_add(7)),
                    "Overdue" => open && due.is_some_and(|d| d < today),
                    "Assigned" => t.is_assigned_by_me(),
                    // Tasks that live on a task server (CalDAV), kept in step by send/receive.
                    "Server Tasks" => t.on_server(),
                    _ => true,
                }
            })
            .map(task_entry)
            .collect()
    };
    let custom = table::custom(&st);
    let entries: Vec<Entry> = match &custom.filter {
        Some(f) => entries
            .into_iter()
            .filter(|e| table::item(s, e.kind, e.id).is_some_and(|it| table::matches(s, it, f)))
            .collect(),
        None => entries,
    };
    let arrange =
        if modules::show_groups(s, folder) { st.arrangement(kind).map(str::to_owned) } else { None };
    // Arranged by Categories, an item with several categories is listed under each of them.
    let entries: Vec<(Entry, Option<String>)> = entries
        .into_iter()
        .flat_map(|e| {
            let mut cats: Vec<String> = Vec::new();
            if arrange.as_deref() == Some("categories") {
                for c in e.categories.iter().map(|c| c.trim()).filter(|c| !c.is_empty()) {
                    if !cats.iter().any(|x| x.eq_ignore_ascii_case(c)) && cats.len() < pim::MAX_LIST {
                        cats.push(c.to_owned());
                    }
                }
            }
            if cats.len() > 1 {
                cats.into_iter().map(|c| (e.clone(), Some(c))).collect::<Vec<_>>()
            } else {
                vec![(e, cats.into_iter().next())]
            }
        })
        .collect();
    let mut keyed: Vec<((u8, String), (i64, String, ItemId), ViewRow)> = entries
        .into_iter()
        .map(|(e, cat)| {
            let group = arrange.as_deref().map(|a| match a {
                "categories" => match cat {
                    Some(c) => (0, c),
                    None => (1, "(none)".to_owned()),
                },
                "startDate" => {
                    let (r, l) = modules::date_group(day(e.start), today, "Earlier");
                    (r, l.to_owned())
                }
                "folder" => (0, s.mailbox.folder(e.folder).map(|f| f.name.clone()).unwrap_or_default()),
                "type" => match e.kind {
                    modules::RowKind::Mail => (1, "Flagged Message".to_owned()),
                    modules::RowKind::Contact => (2, "Contact".to_owned()),
                    _ => (0, "Task".to_owned()),
                },
                "importance" => match e.priority {
                    TaskPriority::High => (0, "High".to_owned()),
                    TaskPriority::Normal => (1, "Normal".to_owned()),
                    TaskPriority::Low => (2, "Low".to_owned()),
                },
                _ => {
                    let (r, l) = modules::date_group(day(e.due), today, "Overdue");
                    (r, l.to_owned())
                }
            });
            let order = (e.due.map_or(i64::MAX, |d| d.unix), e.subject.to_lowercase(), e.id);
            let row = ViewRow { id: e.id, kind: e.kind, group: group.as_ref().map(|g| g.1.clone()) };
            (group.unwrap_or((0, String::new())), order, row)
        })
        .collect();
    let by = |a: &ViewRow, b: &ViewRow| match (
        &custom.sort,
        table::item(s, a.kind, a.id),
        table::item(s, b.kind, b.id),
    ) {
        (Some(f), Some(x), Some(y)) => table::compare(s, kind, f, x, y),
        _ => std::cmp::Ordering::Equal,
    };
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| by(&a.2, &b.2)).then_with(|| a.1.cmp(&b.1)));
    let mut out: Vec<ViewRow> = keyed.into_iter().map(|(_, _, r)| r).collect();
    if modules::reversed(s, folder) {
        out.reverse();
    }
    out
}

fn run_todo_list(s: &mut Session, _p: &Value) -> CmdResult {
    Ok(json!({ "items": todo_list(s) }))
}

// ---------------------------------------------------------------------------------------------------------
// Tags: Follow Up, Private, Importance

/// Follow Up kinds of the Tasks ribbon (`today`, `tomorrow`, `thisWeek`, `nextWeek`, `noDate`).
pub(crate) fn parse_flag_kind(k: &str) -> Option<crate::cmd::mail::FlagKind> {
    use crate::cmd::mail::FlagKind;
    match k.to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
        "today" => Some(FlagKind::Today),
        "tomorrow" => Some(FlagKind::Tomorrow),
        "thisweek" => Some(FlagKind::ThisWeek),
        "nextweek" => Some(FlagKind::NextWeek),
        "nodate" => Some(FlagKind::NoDate),
        _ => None,
    }
}

/// Follow Up on tasks sets their start and due dates (Today, Tomorrow, This Week, Next Week, No Date);
/// `custom` sets `start` / `due` / `reminder` from params, or opens the Custom dialog without them.
fn run_flag(s: &mut Session, p: &Value) -> CmdResult {
    with_reports(s, p, run_flag_now)
}

/// The flag of a flagged message or contact row.
fn row_flag(s: &Session, id: ItemId) -> Option<&FlagState> {
    s.mailbox.mail.get(&id).map(|m| &m.flag).or_else(|| s.mailbox.contacts.get(&id).map(|c| &c.flag))
}

fn set_row_flag(s: &mut Session, id: ItemId, flag: FlagState) {
    if let Some(m) = s.mailbox.mail.get_mut(&id) {
        m.flag = flag;
    } else if let Some(c) = s.mailbox.contacts.get_mut(&id) {
        c.flag = flag;
    }
}

/// (start, due, reminder) of a row: a task's dates, or a flagged message's / contact's flag dates.
fn row_dates(s: &Session, id: ItemId) -> (Option<DateTime>, Option<DateTime>, Option<DateTime>) {
    if let Some(t) = s.mailbox.tasks.get(&id) {
        return (t.start, t.due, t.reminder);
    }
    match row_flag(s, id) {
        Some(FlagState::Flagged { start, due, reminder, .. }) => (*start, *due, *reminder),
        _ => (None, None, None),
    }
}

/// The Follow Up entry (`today` … `noDate`) the dates of To-Do List row `id` came from, for the checked
/// state of the Tasks Home Follow Up buttons; `custom` for dates none of those entries gives; `None` for no
/// row or a completed one.
pub fn follow_up_state(s: &Session, id: ItemId) -> Option<&'static str> {
    let done = match row_of(s, id)? {
        RowOf::Task => s.mailbox.tasks.get(&id).is_some_and(Task::is_complete),
        _ => !matches!(row_flag(s, id), Some(FlagState::Flagged { .. })),
    };
    if done {
        return None;
    }
    let (start, due, _) = row_dates(s, id);
    let flag = FlagState::Flagged { label: String::new(), start, due, reminder: None };
    crate::cmd::mail::follow_up_kind(s, &flag).or(Some("custom"))
}

fn run_flag_now(s: &mut Session, p: &Value) -> CmdResult {
    let r = require_rows(s, p)?;
    let ids = r.tasks.clone();
    let flagged: Vec<ItemId> = r.mail.iter().chain(&r.contacts).copied().collect();
    // No kind: the Quick Click flag (Options › Tasks › Quick Click).
    let quick = crate::cmd::mail::quick_click_kind(s);
    let kind = opt_str(p, "kind").unwrap_or(&quick);
    let tz = s.tz_offset_min;
    let now = s.now();
    if kind == "complete" {
        for id in &ids {
            if let Some(t) = s.mailbox.tasks.get_mut(id) {
                set_complete(t, true, now);
            }
        }
        for id in &flagged {
            set_row_flag(s, *id, FlagState::Complete { completed: Some(now) });
        }
        return Ok(json!({ "flagged": pim::ids_json(&r.all()), "kind": "complete" }));
    }
    if kind == "custom" {
        if !pim::has_any(p, &["start", "due", "reminder"]) {
            // The Custom dialog starts from the dates of a single row; with several rows it opens blank
            // (the dialog then sends only the fields changed, so every row keeps its other dates).
            let all = r.all();
            let (start, due, reminder) = match all.as_slice() {
                [one] => row_dates(s, *one),
                _ => (None, None, None),
            };
            return pim::dialog(
                s,
                "pimFlag",
                json!({
                    "ids": pim::ids_json(&all),
                    "area": "tasks",
                    "start": start.map(|d| pim::date_string(d, tz)),
                    "due": due.map(|d| pim::date_string(d, tz)),
                    "reminderAt": reminder.map(|d| pim::datetime_string(d, tz)),
                }),
            );
        }
        // A field given as null clears it (the dialog's None); a field left out stays as it is.
        let fields: serde_json::Map<String, Value> = ["start", "due", "reminder"]
            .iter()
            .filter_map(|k| p.get(*k).map(|v| ((*k).to_owned(), v.clone())))
            .collect();
        let fields = Value::Object(fields);
        let mut out = Vec::new();
        for id in &ids {
            let Some(mut t) = s.mailbox.tasks.get(id).cloned() else { continue };
            apply_task_fields(s, &mut t, &fields)?;
            out.push(*id);
            s.mailbox.tasks.insert(*id, t);
        }
        let day = |key: &str| -> Result<Option<Option<DateTime>>, CmdError> {
            Ok(pim::date(&fields, key, tz)?.map(|d| d.map(|d| pim::local_midnight(d, tz))))
        };
        let (start, due, reminder) = (day("start")?, day("due")?, pim::date(&fields, "reminder", tz)?);
        for id in &flagged {
            let (label, old_start, old_due, old_reminder) = match row_flag(s, *id) {
                Some(FlagState::Flagged { label, start, due, reminder }) => {
                    (label.clone(), *start, *due, *reminder)
                }
                _ => ("Follow up".to_owned(), None, None, None),
            };
            let (start, due) = (start.unwrap_or(old_start), due.unwrap_or(old_due));
            if let (Some(a), Some(b)) = (start, due)
                && b.unix < a.unix
            {
                return Err(CmdError::Params("the due date is before the start date".into()));
            }
            let reminder = reminder.unwrap_or(old_reminder);
            set_row_flag(s, *id, FlagState::Flagged { label, start, due, reminder });
            out.push(*id);
        }
        return Ok(json!({ "flagged": pim::ids_json(&out), "kind": "custom" }));
    }
    let fk = parse_flag_kind(kind).ok_or_else(|| {
        CmdError::Params("`kind` must be today|tomorrow|thisWeek|nextWeek|noDate|custom|complete".into())
    })?;
    let (start, due) = crate::cmd::mail::flag_dates(s, fk);
    for id in &ids {
        if let Some(t) = s.mailbox.tasks.get_mut(id) {
            t.start = start;
            t.due = due;
            t.modified = Some(now);
        }
    }
    // Flagged messages and contacts keep their flag text and reminder.
    for id in &flagged {
        let (label, reminder) = match row_flag(s, *id) {
            Some(FlagState::Flagged { label, reminder, .. }) => (label.clone(), *reminder),
            _ => ("Follow up".to_owned(), None),
        };
        set_row_flag(s, *id, FlagState::Flagged { label, start, due, reminder });
    }
    Ok(json!({
        "flagged": pim::ids_json(&r.all()),
        "kind": kind,
        "start": start.map(|d| pim::date_string(d, tz)),
        "due": due.map(|d| pim::date_string(d, tz)),
    }))
}

/// Private toggle (`on` sets it; without it, on unless every target is private already).
/// Whether To-Do List row `id` is private (a task's or contact's Private, a message's Private sensitivity).
fn row_private(s: &Session, id: ItemId) -> bool {
    s.mailbox.tasks.get(&id).map(|t| t.private).unwrap_or_else(|| {
        s.mailbox
            .mail
            .get(&id)
            .map(|m| m.sensitivity == mailcraft_model::Sensitivity::Private)
            .or_else(|| s.mailbox.contacts.get(&id).map(|c| c.private))
            .unwrap_or(false)
    })
}

fn run_private(s: &mut Session, p: &Value) -> CmdResult {
    let ids = require_rows(s, p)?.all();
    let all = ids.iter().all(|i| row_private(s, *i));
    let on = opt_bool(p, "on").unwrap_or(!all);
    let now = s.now();
    for id in &ids {
        if let Some(t) = s.mailbox.tasks.get_mut(id) {
            t.private = on;
            t.modified = Some(now);
        } else if let Some(m) = s.mailbox.mail.get_mut(id) {
            m.sensitivity =
                if on { mailcraft_model::Sensitivity::Private } else { mailcraft_model::Sensitivity::Normal };
        } else if let Some(c) = s.mailbox.contacts.get_mut(id) {
            c.private = on;
        }
    }
    Ok(json!({ "ids": pim::ids_json(&ids), "private": on }))
}

/// Priority of a task row, or the importance of a flagged message (contacts have none: Normal).
fn row_priority(s: &Session, id: ItemId) -> TaskPriority {
    if let Some(t) = s.mailbox.tasks.get(&id) {
        return t.priority;
    }
    match s.mailbox.mail.get(&id).map(|m| m.importance) {
        Some(mailcraft_model::Importance::High) => TaskPriority::High,
        Some(mailcraft_model::Importance::Low) => TaskPriority::Low,
        _ => TaskPriority::Normal,
    }
}

/// High / Low Importance (a task's priority, a flagged message's importance): toggles back to Normal when
/// every target already has it. Flagged contacts have no importance and are left alone.
fn run_importance(s: &mut Session, p: &Value) -> CmdResult {
    let r = require_rows(s, p)?;
    let ids: Vec<ItemId> = r.tasks.iter().chain(&r.mail).copied().collect();
    if ids.is_empty() {
        return Err(CmdError::Params("contacts have no importance".into()));
    }
    let level = match opt_str(p, "level").unwrap_or("high") {
        "high" => TaskPriority::High,
        "low" => TaskPriority::Low,
        "normal" => TaskPriority::Normal,
        other => return Err(CmdError::Params(format!("`level` must be high|normal|low, not {other}"))),
    };
    let all = ids.iter().all(|i| row_priority(s, *i) == level);
    let set = if all && p.get("on").is_none() { TaskPriority::Normal } else { level };
    let now = s.now();
    for id in &ids {
        if let Some(t) = s.mailbox.tasks.get_mut(id) {
            t.priority = set;
            t.modified = Some(now);
        } else if let Some(m) = s.mailbox.mail.get_mut(id) {
            m.importance = match set {
                TaskPriority::High => mailcraft_model::Importance::High,
                TaskPriority::Low => mailcraft_model::Importance::Low,
                TaskPriority::Normal => mailcraft_model::Importance::Normal,
            };
        }
    }
    Ok(json!({ "ids": pim::ids_json(&ids), "priority": priority_name(set) }))
}

fn task_selected(s: &Session) -> Option<&'static str> {
    if pim::selected(s, Kind::Task).is_empty() { Some("select a task first") } else { None }
}

/// Reply / Reply All: a flagged message row, or an assigned task or task request.
fn task_request_selected(s: &Session) -> Option<&'static str> {
    match rows(s, &Value::Null).first() {
        Some((_, RowOf::Mail)) => return None,
        Some((_, RowOf::Contact)) => return Some("a contact cannot be answered"),
        _ => {}
    }
    let any = pim::selected(s, Kind::Task).iter().any(|i| {
        s.mailbox.tasks.get(i).is_some_and(|t| t.requested_by.is_some() || !t.assigned_to.is_empty())
    });
    if any { None } else { Some("only assigned tasks and task requests can be answered") }
}

// ---------------------------------------------------------------------------------------------------------
// Respond: Reply, Reply All, Forward, Meeting

/// Task details as message text ("Subject:", "Due date:", "Status:", …, then the notes).
pub fn task_text(t: &Task, tz: i32) -> String {
    let date = |d: Option<DateTime>| {
        d.map_or_else(|| "None".to_owned(), |d| mailcraft_model::ListTime::long_date(d, tz))
    };
    let status = match t.status {
        TaskStatus::NotStarted => "Not Started",
        TaskStatus::InProgress => "In Progress",
        TaskStatus::Completed => "Completed",
        TaskStatus::WaitingOnSomeoneElse => "Waiting on someone else",
        TaskStatus::Deferred => "Deferred",
    };
    let priority = match t.priority {
        TaskPriority::High => "High",
        TaskPriority::Normal => "Normal",
        TaskPriority::Low => "Low",
    };
    let mut text = format!(
        "Subject: {}\nStart date: {}\nDue date: {}\nStatus: {status}\nPriority: {priority}\n% Complete: {}%\n",
        t.subject,
        date(t.start),
        date(t.due),
        t.percent_complete
    );
    if let Some(r) = t.recurrence.as_ref() {
        let pattern = if r.regenerate_days > 0 {
            Some(format!("{} day(s) after each task is completed", r.regenerate_days))
        } else {
            crate::cmd::calendar::recurrence::pattern_text(&r.rrule)
        };
        if let Some(p) = pattern {
            text.push_str(&format!("Recurrence: {p}\n"));
        }
    }
    if !t.assigned_to.is_empty() {
        text.push_str(&format!("Owner: {}\n", t.assigned_to.join("; ")));
    }
    if !t.body.trim().is_empty() {
        text.push('\n');
        text.push_str(&t.body);
        text.push('\n');
    }
    text
}

/// My address (the current account's) as "Name <address>", if any.
fn my_address(s: &Session) -> Option<String> {
    let a = s.current_account().and_then(|a| s.mailbox.account(a))?;
    if a.address.trim().is_empty() {
        return None;
    }
    let name = if a.sender_name.trim().is_empty() { a.display_name.trim() } else { a.sender_name.trim() };
    Some(if name.is_empty() { a.address.clone() } else { format!("{name} <{}>", a.address.trim()) })
}

fn is_me(s: &Session, addr: &str) -> bool {
    pim::parse_addresses(&[addr.to_owned()]).iter().all(|a| s.me.contains(&a.addr.to_ascii_lowercase()))
}

/// The task as an iCalendar object (VTODO, `mailcraft-formats`) with `method`, the organizer and the
/// attendees, and the recurrence rule.
pub fn task_ics(
    t: &Task,
    method: Option<&str>,
    organizer: Option<&str>,
    attendees: &[String],
    now: DateTime,
) -> String {
    use mailcraft_formats::ics::{Method, parse::parse};
    let m = method.map(Method::parse);
    let text = mailcraft_formats::ics::write(&[], &[t], m.as_ref(), now);
    let mut comps = parse(&text);
    let Some(cal) = comps.first_mut() else { return text };
    if let Some(todo) = cal.children.iter_mut().find(|c| c.name == "VTODO") {
        let mailto = |a: &mailcraft_model::Address| format!("mailto:{}", a.addr);
        let cn = |a: &mailcraft_model::Address| {
            a.name.as_deref().map(|n| {
                n.chars()
                    .filter(|c| !matches!(c, '"' | ';' | ':' | ',') && !c.is_control())
                    .collect::<String>()
            })
        };
        if let Some(org) =
            organizer.map(|o| pim::parse_addresses(&[o.to_owned()])).and_then(|v| v.into_iter().next())
        {
            let params = cn(&org).filter(|n| !n.is_empty()).map(|n| vec![("CN", n)]).unwrap_or_default();
            todo.push("ORGANIZER", params, mailto(&org));
        }
        for a in pim::parse_addresses(attendees) {
            let mut params = vec![
                ("ROLE", "REQ-PARTICIPANT".to_owned()),
                ("PARTSTAT", "NEEDS-ACTION".to_owned()),
                ("RSVP", "TRUE".to_owned()),
            ];
            if let Some(n) = cn(&a).filter(|n| !n.is_empty()) {
                params.push(("CN", n));
            }
            todo.push("ATTENDEE", params, mailto(&a));
        }
        if let Some(r) = t.recurrence.as_ref().filter(|r| r.regenerate_days == 0 && !r.rrule.is_empty()) {
            todo.push("RRULE", vec![], r.rrule.clone());
        }
        // A task request's UID stays the same in every message about it.
        let uid = clean_uid(&t.uid);
        if !uid.is_empty() {
            todo.props.retain(|p| !p.name.eq_ignore_ascii_case("UID"));
            todo.push("UID", vec![], uid);
        }
    }
    let mut out = String::new();
    cal.write(&mut out);
    out
}

fn ics_name(t: &Task) -> String {
    let base = if t.subject.trim().is_empty() { "Task" } else { t.subject.trim() };
    crate::compose::clean_attachment_name(&format!("{base}.ics"), "task.ics")
}

/// Opens a compose about task `t` (`to`, subject, task text body, optional VTODO attachment).
fn task_compose(
    s: &mut Session,
    t: &Task,
    to: String,
    subject: String,
    method: Option<&str>,
    attach: bool,
    text: bool,
) -> u64 {
    let tz = s.tz_offset_min;
    let now = s.now();
    let body = if text { task_text(t, tz) } else { String::new() };
    let attachments = if attach {
        let mime = match method {
            Some(m) => format!("text/calendar; method={m}"),
            None => "text/calendar".to_owned(),
        };
        vec![ComposeAttachment {
            filename: ics_name(t),
            mime_type: mime,
            data: task_ics(t, method, None, &[], now).into_bytes(),
        }]
    } else {
        Vec::new()
    };
    let mut c = ComposeState {
        to,
        subject,
        body: Doc::from_plain_text(&body),
        attachments,
        dirty: true,
        ..Default::default()
    };
    crate::compose::insert_default_signature(&s.mailbox, &mut c);
    s.open_compose(c)
}

fn first_task(s: &Session, p: &Value) -> Result<Task, CmdError> {
    pim::require_targets(s, p, Kind::Task)?
        .first()
        .and_then(|i| s.mailbox.tasks.get(i))
        .cloned()
        .ok_or_else(|| CmdError::Params("no task selected".into()))
}

/// The people a reply goes to: the task's requester (Reply) plus the other assignees (Reply All), without me.
fn reply_targets(s: &Session, t: &Task, all: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |a: &String| {
        if !is_me(s, a) && !out.iter().any(|x| x.eq_ignore_ascii_case(a)) {
            out.push(a.clone());
        }
    };
    if let Some(r) = &t.requested_by {
        add(r);
    }
    if all || t.requested_by.is_none() {
        for a in &t.assigned_to {
            add(a);
        }
    }
    out
}

fn run_reply(s: &mut Session, p: &Value) -> CmdResult {
    respond(s, p, false)
}

fn run_reply_all(s: &mut Session, p: &Value) -> CmdResult {
    respond(s, p, true)
}

/// The first row of a Respond command when it is a flagged message or contact (they answer as Mail and
/// People do); `None` for a task (or nothing).
fn first_flagged(s: &Session, p: &Value) -> Option<(ItemId, RowOf)> {
    rows(s, p).first().filter(|(_, k)| *k != RowOf::Task)
}

fn respond(s: &mut Session, p: &Value, all: bool) -> CmdResult {
    match first_flagged(s, p) {
        Some((id, RowOf::Mail)) => {
            return route(s, if all { "mail.replyAll" } else { "mail.reply" }, &[id], json!({}));
        }
        Some(_) => return Err(CmdError::Disabled("a contact cannot be answered".into())),
        None => {}
    }
    let t = first_task(s, p)?;
    let to = reply_targets(s, &t, all);
    if to.is_empty() {
        return Err(CmdError::Disabled("only assigned tasks and task requests can be answered".into()));
    }
    let subject = mailcraft_model::subject::reply_subject(&t.subject);
    let cid = task_compose(s, &t, to.join("; "), subject, None, false, true);
    Ok(json!({ "compose": cid, "task": t.id.0, "to": to }))
}

/// Forward: the task text and the task as a VTODO attachment; `{"as": "attachment"}` sends the attachment
/// alone (Respond › More › Forward as Attachment).
fn run_forward(s: &mut Session, p: &Value) -> CmdResult {
    let text = opt_str(p, "as") != Some("attachment");
    match first_flagged(s, p) {
        Some((id, RowOf::Mail)) => {
            return route(
                s,
                if text { "mail.forward" } else { "mail.forwardAsAttachment" },
                &[id],
                json!({}),
            );
        }
        // A contact goes as its vCard, the way People's Forward Contact sends it.
        Some((id, _)) => return route(s, "people.forward", &[id], json!({ "as": "contact" })),
        None => {}
    }
    let t = first_task(s, p)?;
    let subject = mailcraft_model::subject::forward_subject(&t.subject);
    let cid = task_compose(s, &t, String::new(), subject, Some("PUBLISH"), true, text);
    Ok(json!({ "compose": cid, "task": t.id.0 }))
}

/// Meeting: a new meeting about the task (its subject and notes) with the requester and assignees.
fn run_meeting(s: &mut Session, p: &Value) -> CmdResult {
    match first_flagged(s, p) {
        Some((id, RowOf::Mail)) => return route(s, "mail.replyWithMeeting", &[id], json!({})),
        Some((id, _)) => return route(s, "people.meeting", &[id], json!({})),
        None => {}
    }
    let t = first_task(s, p)?;
    let attendees = reply_targets(s, &t, true);
    let mut params =
        json!({ "subject": t.subject, "body": task_text(&t, s.tz_offset_min), "attendees": attendees });
    if let (Some(o), Some(extra)) = (params.as_object_mut(), p.as_object()) {
        for (k, v) in extra {
            if !matches!(k.as_str(), "ids" | "id" | "folder") {
                o.insert(k.clone(), v.clone());
            }
        }
    }
    crate::cmd::calendar::create_meeting(s, &params)
}

// ---------------------------------------------------------------------------------------------------------
// Task requests (Assign Task)

/// Assigns a task: `to` (addresses or contact ids, required) and task fields. The assigned copy is kept on
/// my task list (`keepCopy`, default Options › Tasks › keep copies) and a task request (subject "Task Request: …", the task text and
/// a VTODO `METHOD:REQUEST` attachment) goes to the assignees: sent to the Outbox (`send`, default true) or
/// left open in a compose window. Without `to` the Task Request window opens.
pub fn assign(s: &mut Session, p: &Value) -> CmdResult {
    let to = recipients(s, p.get("to"))?;
    if to.is_empty() {
        let folder = pim::default_folder(s, Kind::Task).map(|f| f.0);
        return pim::dialog(s, "taskRequest", json!({ "to": "", "folder": folder, "focus": "to" }));
    }
    if pim::text(p, "subject")?.is_none_or(|t| t.trim().is_empty()) {
        return Err(CmdError::Params("the task request needs a subject".into()));
    }
    let folder = pim::new_item_folder(s, p, Kind::Task)?;
    let mut t = Task { folder, ..Default::default() };
    apply_task_fields(s, &mut t, p)?;
    t.assigned_to = to.clone();
    // Defaults: Options › Tasks › "Keep my task list updated with copies of tasks I assign to other people".
    let keep = opt_bool(p, "keepCopy").unwrap_or_else(|| s.settings.bool("tasks.keepCopies"));
    let id = ItemId(s.mailbox.alloc_id());
    t.id = id;
    // The UID ties the assignee's answers and status reports to this copy.
    t.uid = clean_uid(&crate::cmd::calendar::new_uid(s, id, s.now().unix));
    t.request_state = TaskRequestState::Waiting;
    if keep {
        s.mailbox.tasks.insert(id, t.clone());
    }
    let now = s.now();
    let tz = s.tz_offset_min;
    let me = my_address(s);
    let ics = task_ics(&t, Some("REQUEST"), me.as_deref(), &to, now);
    let mut text = task_text(&t, tz);
    if opt_bool(p, "statusReport").unwrap_or_else(|| s.settings.bool("tasks.statusReport")) {
        text.push_str("\nPlease send me a status report when this task is complete.\n");
    }
    let mut c = ComposeState {
        to: to.join("; "),
        subject: format!("Task Request: {}", t.subject.trim()),
        body: Doc::from_plain_text(&text),
        attachments: vec![ComposeAttachment {
            filename: ics_name(&t),
            mime_type: "text/calendar; method=REQUEST".into(),
            data: ics.into_bytes(),
        }],
        dirty: true,
        ..Default::default()
    };
    crate::compose::insert_default_signature(&s.mailbox, &mut c);
    let cid = s.open_compose(c);
    let mut sent = false;
    let mut error = Value::Null;
    if opt_bool(p, "send").unwrap_or(true) {
        let r = s.run("compose.send", &json!({ "compose": cid }))?;
        sent = r.get("sent").and_then(Value::as_bool).unwrap_or(false);
        error = r.get("error").cloned().unwrap_or(Value::Null);
    }
    Ok(json!({
        "task": if keep { json!(id.0) } else { Value::Null },
        "compose": cid,
        "sent": sent,
        "error": error,
        "to": to,
    }))
}

/// Recipients from a `to` param: "a; b" text, or a list of address strings and contact ids (groups expand).
pub(crate) fn recipients(s: &Session, v: Option<&Value>) -> Result<Vec<String>, CmdError> {
    let mut raw: Vec<String> = Vec::new();
    match v {
        None | Some(Value::Null) => {}
        Some(Value::String(t)) if t.len() <= pim::MAX_TEXT => {
            raw.extend(t.split([';', ',']).map(str::to_owned))
        }
        Some(Value::Array(a)) if a.len() <= pim::MAX_LIST => {
            for x in a {
                match x {
                    Value::String(t) if t.len() <= pim::MAX_TEXT => raw.push(t.clone()),
                    Value::Number(n) => {
                        let c = n
                            .as_u64()
                            .and_then(|i| s.mailbox.contacts.get(&ItemId(i)))
                            .ok_or_else(|| CmdError::Params(format!("no contact {n}")))?;
                        raw.extend(crate::cmd::people::contact_addresses(c));
                    }
                    _ => return Err(CmdError::Params("`to` must list addresses or contact ids".into())),
                }
            }
        }
        Some(_) => return Err(CmdError::Params("`to` must be text or a list of addresses".into())),
    }
    let raw: Vec<String> = raw.into_iter().map(|x| x.trim().to_owned()).filter(|x| !x.is_empty()).collect();
    let parsed = pim::parse_addresses(&raw);
    if parsed.len() < raw.len() {
        return Err(CmdError::Params("some recipients are not valid email addresses".into()));
    }
    let mut out: Vec<String> = Vec::new();
    for a in parsed {
        let d = a.to_display_string();
        if !out.iter().any(|x| x.eq_ignore_ascii_case(&d)) {
            out.push(d);
        }
    }
    Ok(out)
}

fn run_assign(s: &mut Session, p: &Value) -> CmdResult {
    assign(s, p)
}

// ---------------------------------------------------------------------------------------------------------
// Received task requests, task responses and status reports (iTIP VTODO, RFC 5546 §3.4)

/// Most attendees read from one task message.
const MAX_TASK_ATTENDEES: usize = 500;
/// Longest task subject kept from a received message (characters).
const MAX_SUBJECT_CHARS: usize = 4_096;
/// Longest UID kept from a received message (characters).
const MAX_UID_CHARS: usize = 255;

/// A task carried by a message: a VTODO in its `text/calendar` part (a task request, a task response or
/// status report, a cancellation, or a published task).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskMessage {
    /// Upper-case iTIP METHOD ("REQUEST", "REPLY", "CANCEL", "PUBLISH", …).
    pub method: String,
    pub uid: String,
    /// The VTODO's fields (subject, dates, status, % complete, priority, notes, categories, recurrence).
    pub task: Task,
    /// The person who assigned the task ("Name <address>").
    pub organizer: Option<String>,
    /// (attendee "Name <address>", upper-case PARTSTAT).
    pub attendees: Vec<(String, String)>,
}

fn clip_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// A UID safe to write back as a property value (no control characters, no TEXT separators).
fn clean_uid(s: &str) -> String {
    s.trim()
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\\' | ';' | ','))
        .take(MAX_UID_CHARS)
        .collect()
}

/// Parses a task message from iCalendar data (`tz` places floating and all-day times). `None` when the
/// data has no VCALENDAR with a VTODO.
pub fn parse_task_message(data: &[u8], tz: i32) -> Option<TaskMessage> {
    use mailcraft_formats::ics::{ImportOptions, parse::parse};
    let text = String::from_utf8_lossy(data);
    let comps = parse(&text);
    let cal = comps.iter().find(|c| c.name == "VCALENDAR")?;
    let todo = cal.children_named("VTODO").next()?;
    let method = cal
        .text("METHOD")
        .map(|m| clip_chars(m.trim(), 64).to_ascii_uppercase())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "PUBLISH".to_owned());
    let uid = todo.text("UID").map(|u| clean_uid(&u)).unwrap_or_default();
    let read = mailcraft_formats::ics::read_str(&text, ImportOptions { local_offset_min: tz }).ok()?;
    let mut task = read.tasks.into_iter().next()?;
    task.subject = clip_chars(&task.subject, MAX_SUBJECT_CHARS);
    if task.body.len() > pim::MAX_TEXT {
        task.body = clip_chars(&task.body, pim::MAX_TEXT / 4);
    }
    task.categories.truncate(64);
    if let Some(rule) = todo.text("RRULE").map(|r| clip_chars(r.trim(), MAX_RRULE)).filter(|r| !r.is_empty())
    {
        task.recurrence = Some(TaskRecurrence { rrule: rule, start: task.due, ..Default::default() });
    }
    let address = |value: &str, cn: Option<&str>| -> Option<String> {
        let v = value.trim();
        let addr = match v.get(..7) {
            Some(p) if p.eq_ignore_ascii_case("mailto:") => v.get(7..).unwrap_or(""),
            _ => v,
        };
        let addr = addr.trim().replace("%40", "@");
        let found = pim::parse_addresses(&[addr]).into_iter().next()?;
        let name = cn
            .map(|n| n.trim().chars().filter(|c| !c.is_control() && *c != '"').take(256).collect::<String>());
        Some(match name.filter(|n| !n.is_empty()) {
            Some(n) => mailcraft_model::Address::new(Some(&n), &found.addr).to_display_string(),
            None => found.addr,
        })
    };
    let organizer = todo.prop("ORGANIZER").and_then(|p| address(&p.value, p.param("CN")));
    let attendees: Vec<(String, String)> = todo
        .props_named("ATTENDEE")
        .take(MAX_TASK_ATTENDEES)
        .filter_map(|p| {
            let who = address(&p.value, p.param("CN"))?;
            let stat = p.param("PARTSTAT").map(|v| clip_chars(v.trim(), 32).to_ascii_uppercase());
            Some((who, stat.unwrap_or_else(|| "NEEDS-ACTION".to_owned())))
        })
        .collect();
    Some(TaskMessage { method, uid, task, organizer, attendees })
}

/// The task message carried by mail item `mail` (its `text/calendar` part, else an attached `.ics`).
pub fn read_task_message(s: &Session, mail: ItemId) -> Option<TaskMessage> {
    let m = s.mailbox.mail.get(&mail)?;
    let data = crate::cmd::calendar::calendar_data(s, m)?;
    parse_task_message(&data, s.tz_offset_min)
}

/// True when `addr` ("Name <a@b>" or "a@b") is one of the user's addresses.
fn is_my_address(s: &Session, addr: &str) -> bool {
    let parsed = pim::parse_addresses(&[addr.to_owned()]);
    !parsed.is_empty()
        && parsed.iter().all(|a| {
            let a = a.addr.trim().to_ascii_lowercase();
            s.me.contains(&a)
                || s.mailbox.accounts.iter().any(|acc| acc.address.trim().eq_ignore_ascii_case(&a))
        })
}

fn same_address(a: &str, b: &str) -> bool {
    let a = pim::parse_addresses(&[a.to_owned()]);
    let b = pim::parse_addresses(&[b.to_owned()]);
    a.first().zip(b.first()).is_some_and(|(x, y)| x.same_address(y))
}

/// The task I accepted from the task request with `uid`.
pub fn find_requested(s: &Session, uid: &str) -> Option<ItemId> {
    let uid = uid.trim();
    if uid.is_empty() {
        return None;
    }
    s.mailbox.tasks.values().filter(|t| t.is_requested() && t.uid.trim() == uid).map(|t| t.id).min()
}

/// My copy of the task I assigned with `uid`.
pub fn find_assigned(s: &Session, uid: &str) -> Option<ItemId> {
    let uid = uid.trim();
    if uid.is_empty() {
        return None;
    }
    s.mailbox
        .tasks
        .values()
        .filter(|t| t.is_assigned_by_me() && t.uid.trim() == uid && !t.is_complete())
        .chain(s.mailbox.tasks.values().filter(|t| t.is_assigned_by_me() && t.uid.trim() == uid))
        .map(|t| t.id)
        .next()
}

/// "accepted", "declined", "inProgress", "completed" or "none" for an attendee PARTSTAT.
fn partstat_name(p: &str) -> &'static str {
    match p {
        "ACCEPTED" => "accepted",
        "DECLINED" => "declined",
        "IN-PROCESS" => "inProgress",
        "COMPLETED" => "completed",
        "TENTATIVE" => "tentative",
        "DELEGATED" => "delegated",
        _ => "none",
    }
}

/// The PARTSTAT that reports `t`'s progress.
fn progress_partstat(t: &Task) -> &'static str {
    match t.status {
        TaskStatus::Completed => "COMPLETED",
        TaskStatus::NotStarted => "ACCEPTED",
        _ => "IN-PROCESS",
    }
}

/// What the reading pane shows for a message carrying a task (the info bar and its buttons).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRequestInfo {
    pub mail: u64,
    /// "REQUEST", "REPLY", "CANCEL", "PUBLISH", …
    pub method: String,
    pub uid: String,
    pub subject: String,
    pub start: Option<String>,
    pub due: Option<String>,
    pub status: String,
    pub percent: u8,
    pub priority: String,
    /// The person who assigned the task.
    pub requested_by: String,
    /// REPLY: the assignee who answered, and how ("accepted", "declined", "inProgress", "completed").
    pub assignee: Option<String>,
    pub response: Option<String>,
    /// The task on my task list: the task I accepted (REQUEST) or my copy of the task I assigned (REPLY).
    pub task: Option<u64>,
    pub i_am_requester: bool,
    /// Accept / Decline apply (`tasks.acceptRequest` / `tasks.declineRequest`).
    pub can_respond: bool,
    /// The response can update my copy of the task (`tasks.processReport`).
    pub can_update: bool,
    /// The info-bar line.
    pub info: String,
}

/// Summary of the task message carried by `mail` against my task list; `None` when it carries no task.
pub fn task_request_info(s: &Session, mail: ItemId) -> Option<TaskRequestInfo> {
    let msg = read_task_message(s, mail)?;
    Some(task_request_info_from(s, mail, &msg))
}

/// [`task_request_info`] for an already parsed message.
pub fn task_request_info_from(s: &Session, mail: ItemId, msg: &TaskMessage) -> TaskRequestInfo {
    let tz = s.tz_offset_min;
    let t = &msg.task;
    let requested_by = msg.organizer.clone().unwrap_or_default();
    let i_am_requester = msg.organizer.as_deref().is_some_and(|o| is_my_address(s, o));
    let reply = msg.method == "REPLY";
    let answer = if reply {
        msg.attendees.iter().find(|(a, _)| !is_my_address(s, a)).or(msg.attendees.first())
    } else {
        None
    };
    let task = if reply { find_assigned(s, &msg.uid) } else { find_requested(s, &msg.uid) };
    let name = |who: &str| {
        pim::parse_addresses(&[who.to_owned()])
            .first()
            .map(|a| a.display().to_owned())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "The assignee".to_owned())
    };
    let subject = if t.subject.trim().is_empty() { "(No subject)".to_owned() } else { t.subject.clone() };
    let (info, can_respond, can_update) = match msg.method.as_str() {
        "REQUEST" if i_am_requester => ("You sent this task request.".to_owned(), false, false),
        "REQUEST" => match task.and_then(|i| s.mailbox.tasks.get(&i)).map(|t| t.request_state) {
            Some(TaskRequestState::Declined) => ("You declined this task.".to_owned(), true, false),
            Some(_) => ("You accepted this task. It is on your task list.".to_owned(), true, false),
            None => ("Please respond. Accept the task to add it to your task list.".to_owned(), true, false),
        },
        "REPLY" => {
            let (who, stat) =
                answer.map(|(w, p)| (name(w), p.as_str())).unwrap_or_else(|| ("The assignee".to_owned(), ""));
            let mut line = match stat {
                "ACCEPTED" => format!("{who} accepted the task."),
                "DECLINED" => format!("{who} declined the task."),
                "COMPLETED" => format!("{who} completed the task."),
                "IN-PROCESS" => format!(
                    "{who} reports: {}, {}% complete.",
                    task_status_text(t.status),
                    t.percent_complete.min(100)
                ),
                _ => format!("{who} has not responded yet."),
            };
            if task.is_none() {
                line.push_str(" The task is not on your task list.");
            }
            (line, false, task.is_some())
        }
        "CANCEL" => ("This task request was canceled.".to_owned(), false, false),
        _ => ("This message carries a task.".to_owned(), false, false),
    };
    TaskRequestInfo {
        mail: mail.0,
        method: msg.method.clone(),
        uid: msg.uid.clone(),
        subject,
        start: t.start.map(|d| pim::date_string(d, tz)),
        due: t.due.map(|d| pim::date_string(d, tz)),
        status: status_name(t.status).to_owned(),
        percent: t.percent_complete.min(100),
        priority: priority_name(t.priority).to_owned(),
        requested_by,
        assignee: answer.map(|(w, _)| w.clone()),
        response: answer.map(|(_, p)| partstat_name(p).to_owned()),
        task: task.map(|i| i.0),
        i_am_requester,
        can_respond,
        can_update,
        info,
    }
}

fn task_status_text(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::NotStarted => "Not Started",
        TaskStatus::InProgress => "In Progress",
        TaskStatus::Completed => "Completed",
        TaskStatus::WaitingOnSomeoneElse => "Waiting on someone else",
        TaskStatus::Deferred => "Deferred",
    }
}

/// The task message a command acts on: `id` (a message), else the focused message.
fn req_task_mail(s: &Session, p: &Value) -> Result<(ItemId, TaskMessage), CmdError> {
    let id = crate::cmd::util::opt_u64(p, "id")
        .map(ItemId)
        .or(s.selection.focus)
        .ok_or_else(|| CmdError::Params("`id` (the task request message) is required".into()))?;
    if !s.mailbox.mail.contains_key(&id) {
        return Err(CmdError::Params(format!("no message {}", id.0)));
    }
    let msg = read_task_message(s, id)
        .ok_or_else(|| CmdError::Params("the message has no task information".into()))?;
    Ok((id, msg))
}

/// How a response goes out: sent now (default for answers), opened for editing, or not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendMode {
    Now,
    Edit,
    None,
}

fn send_mode(p: &Value, default: SendMode) -> Result<SendMode, CmdError> {
    match opt_str(p, "send") {
        None => Ok(default),
        Some("now") => Ok(SendMode::Now),
        Some("edit") => Ok(SendMode::Edit),
        Some("none") => Ok(SendMode::None),
        Some(_) => Err(CmdError::Params("`send` must be now|edit|none".into())),
    }
}

/// The VTODO reply (METHOD:REPLY) about `t`: its UID, the requester as ORGANIZER and me as the ATTENDEE
/// with `partstat`, with the task's current status, % complete and completion date.
pub fn reply_ics(
    t: &Task,
    organizer: Option<&str>,
    me: Option<&str>,
    partstat: &str,
    now: DateTime,
) -> String {
    use mailcraft_formats::ics::parse::parse;
    let base = task_ics(t, Some("REPLY"), organizer, &[], now);
    let mut comps = parse(&base);
    let Some(cal) = comps.first_mut() else { return base };
    if let Some(todo) = cal.children.iter_mut().find(|c| c.name == "VTODO")
        && let Some(a) = me.map(|m| pim::parse_addresses(&[m.to_owned()])).and_then(|v| v.into_iter().next())
    {
        let mut params = vec![("PARTSTAT", partstat.to_owned())];
        if let Some(n) = a.name.as_deref() {
            let n: String =
                n.chars().filter(|c| !matches!(c, '"' | ';' | ':' | ',') && !c.is_control()).collect();
            if !n.is_empty() {
                params.push(("CN", n));
            }
        }
        todo.push("ATTENDEE", params, format!("mailto:{}", a.addr));
    }
    let mut out = String::new();
    cal.write(&mut out);
    out
}

/// Opens (and for [`SendMode::Now`] sends) a message about task `t` to `to` with a VTODO reply carrying
/// `partstat`. Returns (compose id, sent).
fn send_task_reply(
    s: &mut Session,
    t: &Task,
    to: &[String],
    subject: String,
    intro: &str,
    partstat: &str,
    mode: SendMode,
) -> Result<(Option<u64>, bool), CmdError> {
    if mode == SendMode::None {
        return Ok((None, false));
    }
    let now = s.now();
    let tz = s.tz_offset_min;
    let me = my_address(s);
    let mut text = String::new();
    if !intro.trim().is_empty() {
        text.push_str(intro.trim());
        text.push_str("\n\n");
    }
    text.push_str(&task_text(t, tz));
    let ics = reply_ics(t, t.requested_by.as_deref(), me.as_deref(), partstat, now);
    let mut c = ComposeState {
        to: to.join("; "),
        subject,
        body: Doc::from_plain_text(&text),
        attachments: vec![ComposeAttachment {
            filename: ics_name(t),
            mime_type: "text/calendar; method=REPLY".into(),
            data: ics.into_bytes(),
        }],
        dirty: true,
        ..Default::default()
    };
    crate::compose::insert_default_signature(&s.mailbox, &mut c);
    let cid = s.open_compose(c);
    if mode == SendMode::Now {
        if to.is_empty() {
            return Ok((Some(cid), false));
        }
        let r = s.run("compose.send", &json!({ "compose": cid }))?;
        return Ok((Some(cid), r.get("sent").and_then(Value::as_bool).unwrap_or(false)));
    }
    Ok((Some(cid), false))
}

/// The tasks folder a task accepted from `mail` goes to: `folder` param, else the receiving account's Tasks
/// folder, else the default tasks folder.
fn accept_folder(s: &Session, p: &Value, mail: ItemId) -> Result<FolderId, CmdError> {
    if p.get("folder").is_some_and(|v| !v.is_null()) {
        return pim::new_item_folder(s, p, Kind::Task);
    }
    s.mailbox
        .mail
        .get(&mail)
        .and_then(|m| s.mailbox.folder_account(m.folder))
        .and_then(|a| s.mailbox.role_folder(a, FolderRole::Tasks))
        .or_else(|| pim::default_folder(s, Kind::Task))
        .ok_or_else(|| CmdError::Failed("there is no tasks folder".into()))
}

/// Accept / Decline a task request (`id` = the request message; `send` now|edit|none, default now;
/// `comment` goes above the task details). Accept adds the task to my task list (requested by the
/// organizer) or updates the one accepted before; Decline moves an accepted copy to Deleted Items. Either
/// way a task response (VTODO REPLY, PARTSTAT ACCEPTED / DECLINED) goes to the requester.
fn respond_request(s: &mut Session, p: &Value, accept: bool) -> CmdResult {
    let (mail, msg) = req_task_mail(s, p)?;
    if msg.method != "REQUEST" {
        return Err(CmdError::Params("the message is not a task request".into()));
    }
    let requester = msg
        .organizer
        .clone()
        .or_else(|| s.mailbox.mail.get(&mail).and_then(|m| m.from.as_ref()).map(|a| a.to_display_string()))
        .ok_or_else(|| CmdError::Params("the task request does not say who sent it".into()))?;
    if is_my_address(s, &requester) {
        return Err(CmdError::Disabled("you sent this task request".into()));
    }
    let mode = send_mode(p, SendMode::Now)?;
    let comment = pim::text(p, "comment")?.unwrap_or_default();
    let now = s.now();
    let tz = s.tz_offset_min;
    let existing = find_requested(s, &msg.uid);
    let assignees: Vec<String> = msg.attendees.iter().map(|(a, _)| a.clone()).collect();
    let mut answered = existing
        .and_then(|id| s.mailbox.tasks.get(&id))
        .cloned()
        .unwrap_or_else(|| Task { priority: msg.task.priority, ..Default::default() });
    {
        let src = &msg.task;
        answered.subject = src.subject.clone();
        answered.body = src.body.clone();
        answered.start = src.start.map(|d| pim::local_midnight(d, tz));
        answered.due = src.due.map(|d| pim::local_midnight(d, tz));
        answered.priority = src.priority;
        if existing.is_none() {
            answered.status = src.status;
            answered.percent_complete = src.percent_complete.min(100);
            answered.completed = src.completed;
            if answered.categories.is_empty() {
                answered.categories = src.categories.clone();
            }
        }
        answered.requested_by = Some(requester.clone());
        answered.assigned_to = assignees;
        answered.uid = if msg.uid.is_empty() { answered.uid.clone() } else { msg.uid.clone() };
        answered.request_state = if accept { TaskRequestState::Accepted } else { TaskRequestState::Declined };
        answered.last_report = Some(now);
        answered.recurrence = None;
    }
    // The pattern is checked like a typed one; a pattern MailCraft cannot follow is dropped.
    if let Some(rule) = msg.task.recurrence.as_ref().map(|r| r.rrule.clone()) {
        let mut tmp = answered.clone();
        if apply_task_fields(s, &mut tmp, &json!({ "recurrence": rule })).is_ok() {
            answered = tmp;
        }
    }
    let task_id = if accept {
        let id = match existing {
            // Accepted again after declining: back from Deleted Items to the task list.
            Some(id) if !pim::is_kind_folder(s, answered.folder, Kind::Task) => {
                answered.folder = accept_folder(s, p, mail)?;
                id
            }
            Some(id) => id,
            None => {
                let folder = accept_folder(s, p, mail)?;
                answered.folder = folder;
                if answered.reminder.is_none()
                    && !answered.is_complete()
                    && s.settings.task_reminders_on_due()
                    && let Some(due) = answered.due
                {
                    answered.reminder =
                        Some(pim::add_secs(due, i64::from(s.settings.task_reminder_minutes()) * 60));
                }
                ItemId(s.mailbox.alloc_id())
            }
        };
        answered.id = id;
        s.mailbox.tasks.insert(id, answered.clone());
        Some(id)
    } else {
        if let Some(id) = existing {
            if let Some(t) = s.mailbox.tasks.get_mut(&id) {
                t.request_state = TaskRequestState::Declined;
                t.last_report = Some(now);
            }
            pim::delete(s, &[id]);
        }
        None
    };
    let (prefix, partstat) =
        if accept { ("Task Accepted", "ACCEPTED") } else { ("Task Declined", "DECLINED") };
    let subject = format!("{prefix}: {}", answered.subject.trim());
    let (compose, sent) =
        send_task_reply(s, &answered, std::slice::from_ref(&requester), subject, &comment, partstat, mode)?;
    Ok(json!({
        "task": task_id.map(|i| i.0),
        "response": if accept { "accepted" } else { "declined" },
        "requestedBy": requester,
        "compose": compose,
        "sent": sent,
    }))
}

fn run_accept_request(s: &mut Session, p: &Value) -> CmdResult {
    respond_request(s, p, true)
}

fn run_decline_request(s: &mut Session, p: &Value) -> CmdResult {
    respond_request(s, p, false)
}

fn run_request_info(s: &mut Session, p: &Value) -> CmdResult {
    let (mail, msg) = req_task_mail(s, p)?;
    serde_json::to_value(task_request_info_from(s, mail, &msg)).map_err(|e| CmdError::Failed(e.to_string()))
}

/// Applies a task response or status report (`mail`, a VTODO REPLY from one of the assignees) to my copy
/// of the task I assigned: accepted / declined, status, % complete and completion date. The receiving
/// side can call this when a REPLY arrives; `tasks.processReport` runs it for one message.
pub fn process_report(s: &mut Session, mail: ItemId) -> CmdResult {
    let msg = read_task_message(s, mail)
        .ok_or_else(|| CmdError::Params("the message has no task information".into()))?;
    if msg.method != "REPLY" {
        return Err(CmdError::Params("the message is not a task response".into()));
    }
    let id = find_assigned(s, &msg.uid)
        .ok_or_else(|| CmdError::Params("the task is not on your task list".into()))?;
    let Some(mut t) = s.mailbox.tasks.get(&id).cloned() else {
        return Err(CmdError::Params("the task is not on your task list".into()));
    };
    // Only the people the task was assigned to can answer for it.
    let Some((who, stat)) =
        msg.attendees.iter().find(|(a, _)| t.assigned_to.iter().any(|x| same_address(x, a))).cloned()
    else {
        return Err(CmdError::Params("the response is not from someone the task was assigned to".into()));
    };
    let now = s.now();
    let src = &msg.task;
    match stat.as_str() {
        "DECLINED" => t.request_state = TaskRequestState::Declined,
        "ACCEPTED" | "IN-PROCESS" | "COMPLETED" => {
            t.request_state = TaskRequestState::Accepted;
            let done =
                stat == "COMPLETED" || src.status == TaskStatus::Completed || src.percent_complete >= 100;
            if done {
                if !t.is_complete() {
                    t.completed = None;
                }
                set_complete(&mut t, true, src.completed.unwrap_or(now));
            } else {
                t.status = src.status;
                t.percent_complete = src.percent_complete;
                t.completed = None;
            }
        }
        _ => t.request_state = TaskRequestState::Waiting,
    }
    t.last_report = Some(now);
    let tz = s.tz_offset_min;
    let out = task_json(&t, tz);
    s.mailbox.tasks.insert(id, t);
    sync_mail_flag(s, id);
    Ok(json!({ "task": id.0, "from": who, "response": partstat_name(&stat), "updated": out }))
}

fn run_process_report(s: &mut Session, p: &Value) -> CmdResult {
    let (mail, _) = req_task_mail(s, p)?;
    process_report(s, mail)
}

/// Sends a status report about `t` to the person who assigned it (for my own tasks: the people I assigned it
/// to). Completed tasks report as "Task Completed". Returns (compose id, sent).
fn send_status_report(s: &mut Session, id: ItemId, mode: SendMode) -> Result<(Option<u64>, bool), CmdError> {
    let Some(t) = s.mailbox.tasks.get(&id).cloned() else {
        return Err(CmdError::Params(format!("no task {}", id.0)));
    };
    let to = reply_targets(s, &t, false);
    let prefix = if t.is_complete() { "Task Completed" } else { "Task Status Report" };
    let subject = format!("{prefix}: {}", t.subject.trim());
    let now = s.now();
    let r = send_task_reply(s, &t, &to, subject, "", progress_partstat(&t), mode)?;
    if let Some(task) = s.mailbox.tasks.get_mut(&id) {
        task.last_report = Some(now);
    }
    Ok(r)
}

/// Send Status Report (`send` now|edit|none, default edit: the report opens in a message window).
fn run_status_report(s: &mut Session, p: &Value) -> CmdResult {
    let t = first_task(s, p)?;
    let mode = send_mode(p, SendMode::Edit)?;
    let (compose, sent) = send_status_report(s, t.id, mode)?;
    Ok(json!({ "task": t.id.0, "compose": compose, "sent": sent, "to": reply_targets(s, &t, false) }))
}

/// Tasks accepted from task requests that are complete now.
fn requested_complete(s: &Session) -> BTreeSet<ItemId> {
    s.mailbox.tasks.values().filter(|t| t.is_requested() && t.is_complete()).map(|t| t.id).collect()
}

/// After a command completed tasks: a completion report goes to the requester of every task accepted
/// from a task request that was not complete before (`before`), when Options › Tasks › "Send status report
/// when I complete an assigned task" is on. Returns the report compose ids.
fn completion_reports(s: &mut Session, before: &BTreeSet<ItemId>) -> Vec<u64> {
    if !s.settings.bool("tasks.statusReport") {
        return Vec::new();
    }
    let done: Vec<ItemId> = requested_complete(s).into_iter().filter(|i| !before.contains(i)).collect();
    let mut out = Vec::new();
    for id in done {
        // The task is complete either way: a report that cannot go out stays open in its window.
        match send_status_report(s, id, SendMode::Now) {
            Ok((Some(cid), _)) => out.push(cid),
            Ok((None, _)) => {}
            Err(e) => log::warn!("completion report for task {} not sent: {e}", id.0),
        }
    }
    out
}

/// Runs `f`, then sends the completion reports it calls for (added to its result as `reports`).
fn with_reports(s: &mut Session, p: &Value, f: fn(&mut Session, &Value) -> CmdResult) -> CmdResult {
    let before = requested_complete(s);
    let mut out = f(s, p)?;
    let reports = completion_reports(s, &before);
    if !reports.is_empty()
        && let Some(o) = out.as_object_mut()
    {
        o.insert("reports".into(), json!(reports));
    }
    Ok(out)
}

pub fn specs() -> Vec<CommandSpec> {
    let spec = |id, label, location, shortcut, params, mutates, enabled, run| CommandSpec {
        id,
        label,
        location,
        shortcut,
        params,
        mutates,
        enabled,
        run,
    };
    vec![
        spec(
            "tasks.new",
            "New Task",
            "Tasks Home › New; Home › New Items › Task",
            "Mod+Shift+K",
            r#"{"subject"?, "body"?, "start"?: "YYYY-MM-DD", "due"?: "YYYY-MM-DD", "status"?: "notStarted|inProgress|completed|waitingOnSomeoneElse|deferred", "priority"?: "low|normal|high", "percent"?: 0..100, "reminder"?: "YYYY-MM-DDTHH:MM", "categories"?: [string], "folder"?: number} (no fields = open a blank task window)"#,
            true,
            always,
            run_new,
        ),
        spec(
            "tasks.removeFromList",
            "Remove from List",
            "Tasks Home › Manage Task",
            "",
            r#"{"ids"?: [number]}"#,
            true,
            todo_selected,
            run_remove_from_list,
        ),
        spec(
            "tasks.markComplete",
            "Mark Complete",
            "Tasks Home › Manage Task",
            "",
            r#"{"ids"?: [number]}"#,
            true,
            always,
            run_mark_complete,
        ),
        spec(
            "tasks.update",
            "Edit Task",
            "Tasks › Item",
            "",
            r#"{"id": number | "ids": [number], "complete"?: bool, ...task fields}"#,
            true,
            always,
            run_update,
        ),
        spec(
            "tasks.delete",
            "Delete",
            "Tasks Home › Delete",
            "",
            r#"{"id"?: number, "ids"?: [number], "request"?: "decline"|"complete"|"delete"} (a task accepted from a task request: what to tell the person who assigned it; without it the Delete dialog asks)"#,
            true,
            always,
            run_delete,
        ),
        spec(
            "tasks.copy",
            "Copy to Folder",
            "Tasks › Item",
            "",
            r#"{"folder": number, "id"?: number, "ids"?: [number]}"#,
            true,
            always,
            run_copy,
        ),
        spec(
            "tasks.move",
            "Move to Folder",
            "Tasks Home › Actions › Move",
            "",
            r#"{"folder": number, "id"?: number, "ids"?: [number]}"#,
            true,
            always,
            run_move,
        ),
        spec(
            "tasks.categorize",
            "Categorize",
            "Tasks Home › Tags",
            "",
            r#"{"name": string, "on"?: bool, "clear"?: bool, "id"?: number, "ids"?: [number]}"#,
            true,
            always,
            run_categorize,
        ),
        spec(
            "tasks.fromMail",
            "Add Flagged Message to Tasks",
            "Tasks › To-Do List",
            "",
            r#"{"ids"?: [number]} (message ids)"#,
            true,
            always,
            run_from_mail,
        ),
        spec("tasks.todoList", "To-Do List", "Tasks › To-Do List", "", "", false, always, run_todo_list),
        spec(
            "tasks.flag",
            "Follow Up",
            "Tasks Home › Follow Up",
            "",
            r#"{"kind"?: "today|tomorrow|thisWeek|nextWeek|noDate|custom|complete" (none = the Quick Click flag), "start"?, "due"?, "reminder"?, "ids"?: [number]} (custom without dates = open the Custom dialog)"#,
            true,
            row_selected,
            run_flag,
        ),
        spec(
            "tasks.private",
            "Private",
            "Tasks Home › Tags",
            "",
            r#"{"on"?: bool, "ids"?: [number]}"#,
            true,
            row_selected,
            run_private,
        ),
        spec(
            "tasks.importance",
            "Importance",
            "Tasks Home › Tags",
            "",
            r#"{"level": "high|low|normal", "on"?: bool, "ids"?: [number]} (toggles back to normal)"#,
            true,
            row_selected,
            run_importance,
        ),
        spec(
            "tasks.allCategories",
            "All Categories",
            "Tasks Home › Tags › Categorize",
            "",
            r#"{"ids"?: [number]}"#,
            false,
            row_selected,
            run_all_categories,
        ),
        spec(
            "tasks.reply",
            "Reply",
            "Tasks Home › Respond",
            "",
            r#"{"ids"?: [number]}"#,
            false,
            task_request_selected,
            run_reply,
        ),
        spec(
            "tasks.replyAll",
            "Reply All",
            "Tasks Home › Respond",
            "",
            r#"{"ids"?: [number]}"#,
            false,
            task_request_selected,
            run_reply_all,
        ),
        spec(
            "tasks.forward",
            "Forward",
            "Tasks Home › Respond",
            "",
            r#"{"as"?: "text|attachment", "ids"?: [number]}"#,
            false,
            row_selected,
            run_forward,
        ),
        spec(
            "tasks.meeting",
            "Meeting",
            "Tasks Home › Respond",
            "",
            r#"{"ids"?: [number], ...appointment fields}"#,
            true,
            row_selected,
            run_meeting,
        ),
        spec(
            "tasks.assign",
            "Assign Task",
            "Home › New Items › Task Request",
            "",
            r#"{"to": string | [string | contact id], "subject": string, ...task fields, "keepCopy"?: bool, "statusReport"?: bool, "send"?: bool} (no `to` = open the Task Request window)"#,
            true,
            always,
            run_assign,
        ),
        spec(
            "tasks.requestInfo",
            "Task Request Information",
            "Reading Pane › Task request",
            "",
            r#"{"id"?: number} (a message carrying a task; default the focused message)"#,
            false,
            always,
            run_request_info,
        ),
        spec(
            "tasks.acceptRequest",
            "Accept",
            "Reading Pane › Task request",
            "",
            r#"{"id"?: number, "send"?: "now|edit|none", "comment"?: string, "folder"?: number}"#,
            true,
            always,
            run_accept_request,
        ),
        spec(
            "tasks.declineRequest",
            "Decline",
            "Reading Pane › Task request",
            "",
            r#"{"id"?: number, "send"?: "now|edit|none", "comment"?: string}"#,
            true,
            always,
            run_decline_request,
        ),
        spec(
            "tasks.processReport",
            "Update Task from Response",
            "Reading Pane › Task response",
            "",
            r#"{"id"?: number} (a task response or status report about a task I assigned)"#,
            true,
            always,
            run_process_report,
        ),
        spec(
            "tasks.sendStatusReport",
            "Send Status Report",
            "Task › Manage Task",
            "",
            r#"{"ids"?: [number], "send"?: "now|edit|none"} (default edit: the report opens in a message window)"#,
            true,
            task_selected,
            run_status_report,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Selection;
    use crate::cmd::calendar::tests::{role, session};
    use mailcraft_model::MailItem;

    fn add_mail(s: &mut Session, subject: &str, flag: FlagState) -> ItemId {
        let folder = role(s, FolderRole::Inbox);
        s.mailbox.add_mail(MailItem { folder, subject: subject.into(), flag, ..Default::default() })
    }

    fn flagged() -> FlagState {
        FlagState::Flagged { label: "Follow up".into(), start: None, due: None, reminder: None }
    }

    #[test]
    fn new_task_dialog_or_create() {
        let mut s = session();
        let r = s.run("tasks.new", &json!({})).unwrap();
        assert_eq!(r["dialog"], "task");
        let r = s
            .run("tasks.new", &json!({"subject": "Report", "due": "2026-10-15", "priority": "high"}))
            .unwrap();
        let t = &s.mailbox.tasks[&ItemId(r["id"].as_u64().unwrap())];
        assert_eq!(t.folder, role(&s, FolderRole::Tasks));
        assert_eq!(r["due"], "2026-10-15");
        assert_eq!(t.priority, TaskPriority::High);
        assert!(matches!(
            s.run("tasks.new", &json!({"subject": "x", "percent": 101})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("tasks.new", &json!({"subject": "x", "start": "2026-10-20", "due": "2026-10-10"})),
            Err(CmdError::Params(_))
        ));
        assert!(s.mailbox.validate().is_empty());
    }

    #[test]
    fn percent_status_and_complete_stay_consistent() {
        let mut s = session();
        let id = s.run("tasks.new", &json!({"subject": "T"})).unwrap()["id"].as_u64().unwrap();
        let r = s.run("tasks.update", &json!({"id": id, "percent": 40})).unwrap();
        assert_eq!(r["updated"][0]["status"], "inProgress");
        let r = s.run("tasks.update", &json!({"id": id, "percent": 100})).unwrap();
        assert_eq!(r["updated"][0]["status"], "completed");
        assert!(s.mailbox.tasks[&ItemId(id)].completed.is_some());
        let r = s.run("tasks.update", &json!({"id": id, "status": "deferred"})).unwrap();
        assert_eq!(r["updated"][0]["percent"], 0);
        assert!(s.mailbox.tasks[&ItemId(id)].completed.is_none());
        let r = s.run("tasks.update", &json!({"id": id, "status": "completed"})).unwrap();
        assert_eq!(r["updated"][0]["percent"], 100);
        assert!(matches!(
            s.run("tasks.update", &json!({"id": id, "status": "done"})),
            Err(CmdError::Params(_))
        ));
    }

    #[test]
    fn mark_complete_links_flag_and_task() {
        let mut s = session();
        let m = add_mail(&mut s, "Contract", flagged());
        let r = s.run("tasks.fromMail", &json!({"ids": [m.0]})).unwrap();
        let t = ItemId(r["tasks"][0].as_u64().unwrap());
        assert_eq!(s.mailbox.tasks[&t].from_mail, Some(m));
        assert_eq!(s.mailbox.tasks[&t].subject, "Contract");
        // Running again refreshes the same task.
        let r = s.run("tasks.fromMail", &json!({"ids": [m.0]})).unwrap();
        assert_eq!(r["tasks"][0], t.0);
        assert_eq!(s.mailbox.tasks.len(), 1);
        s.run("tasks.markComplete", &json!({"ids": [t.0]})).unwrap();
        assert!(matches!(s.mailbox.mail[&m].flag, FlagState::Complete { .. }));
        assert_eq!(s.mailbox.tasks[&t].status, TaskStatus::Completed);
        // Reopening the task re-flags the message.
        s.run("tasks.update", &json!({"id": t.0, "complete": false})).unwrap();
        assert!(matches!(s.mailbox.mail[&m].flag, FlagState::Flagged { .. }));
    }

    #[test]
    fn remove_from_list_clears_flags_and_deletes_tasks() {
        let mut s = session();
        let m = add_mail(&mut s, "Flagged", flagged());
        let plain = s.run("tasks.new", &json!({"subject": "Plain"})).unwrap()["id"].as_u64().unwrap();
        s.selection = Selection { folder: role(&s, FolderRole::Tasks), items: vec![], focus: None };
        assert!(matches!(s.run("tasks.removeFromList", &json!({})), Err(CmdError::Disabled(_))));
        s.selection.items = vec![m, ItemId(plain)];
        let r = s.run("tasks.removeFromList", &json!({})).unwrap();
        assert_eq!(r["unflagged"], json!([m.0]));
        assert_eq!(r["deleted"], json!([plain]));
        assert_eq!(s.mailbox.mail[&m].flag, FlagState::None);
        assert_eq!(s.mailbox.tasks[&ItemId(plain)].folder, role(&s, FolderRole::DeletedItems));
        assert!(s.selection.items.is_empty());
    }

    #[test]
    fn remove_linked_task_unflags_message() {
        let mut s = session();
        let m = add_mail(&mut s, "Linked", flagged());
        let t = s.run("tasks.fromMail", &json!({"ids": [m.0]})).unwrap()["tasks"][0].as_u64().unwrap();
        s.selection =
            Selection { folder: role(&s, FolderRole::Tasks), items: vec![ItemId(t)], focus: Some(ItemId(t)) };
        let r = s.run("tasks.removeFromList", &json!({})).unwrap();
        assert_eq!(r["unlinked"], json!([t]));
        assert!(!s.mailbox.tasks.contains_key(&ItemId(t)));
        assert_eq!(s.mailbox.mail[&m].flag, FlagState::None);
    }

    #[test]
    fn todo_list_merges_tasks_and_flagged_mail() {
        let mut s = session();
        add_mail(&mut s, "Mail item", flagged());
        add_mail(&mut s, "Not flagged", FlagState::None);
        s.run("tasks.new", &json!({"subject": "Soon", "due": "2026-10-10"})).unwrap();
        s.run("tasks.new", &json!({"subject": "Done", "complete": true})).unwrap();
        let r = s.run("tasks.todoList", &json!({})).unwrap();
        let items = r["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["subject"], "Soon");
        assert_eq!(items[1]["kind"], "mail");
    }

    #[test]
    fn copy_move_categorize_delete_tasks() {
        let mut s = session();
        let id = s.run("tasks.new", &json!({"subject": "T"})).unwrap()["id"].as_u64().unwrap();
        let root = role(&s, FolderRole::Tasks);
        let sub = s.mailbox.create_folder(root, "Home").unwrap();
        s.run("tasks.copy", &json!({"id": id, "folder": sub.0})).unwrap();
        s.run("tasks.move", &json!({"id": id, "folder": sub.0})).unwrap();
        assert_eq!(s.mailbox.tasks[&ItemId(id)].folder, sub);
        s.run("tasks.categorize", &json!({"id": id, "name": "Green category"})).unwrap();
        assert_eq!(s.mailbox.tasks[&ItemId(id)].categories.len(), 1);
        s.run("tasks.delete", &json!({"id": id})).unwrap();
        assert_eq!(s.mailbox.tasks[&ItemId(id)].folder, role(&s, FolderRole::DeletedItems));
        assert_eq!(s.mailbox.tasks.len(), 2);
    }

    fn rule_task(s: &mut Session, rec: Value) -> ItemId {
        let r = s.run("tasks.new", &json!({"subject": "Weekly report", "due": "2026-10-09", "start": "2026-10-08", "recurrence": rec})).unwrap();
        ItemId(r["id"].as_u64().unwrap())
    }

    #[test]
    fn completing_a_recurring_task_moves_it_to_the_next_occurrence() {
        let mut s = session(); // Friday 2026-10-09
        let id = rule_task(&mut s, json!("FREQ=WEEKLY;BYDAY=FR;COUNT=3"));
        let tz = s.tz_offset_min;
        assert!(s.mailbox.tasks[&id].recurrence.is_some());
        let r = s.run("tasks.markComplete", &json!({"ids": [id.0]})).unwrap();
        let copy = ItemId(r["completedCopies"][0].as_u64().unwrap());
        let t = &s.mailbox.tasks[&id];
        assert_eq!(t.due.map(|d| pim::date_string(d, tz)).as_deref(), Some("2026-10-16"));
        assert_eq!(
            t.start.map(|d| pim::date_string(d, tz)).as_deref(),
            Some("2026-10-15"),
            "start moves too"
        );
        assert_eq!(t.status, TaskStatus::NotStarted);
        assert_eq!(t.recurrence.as_ref().unwrap().completed_count, 1);
        let c = &s.mailbox.tasks[&copy];
        assert!(c.is_complete() && c.recurrence.is_none());
        assert_eq!(c.due.map(|d| pim::date_string(d, tz)).as_deref(), Some("2026-10-09"));
        // Second completion through tasks.update, third ends the series (COUNT=3).
        s.run("tasks.update", &json!({"id": id.0, "complete": true})).unwrap();
        assert_eq!(s.mailbox.tasks[&id].due.map(|d| pim::date_string(d, tz)).as_deref(), Some("2026-10-23"));
        let r = s.run("tasks.update", &json!({"id": id.0, "percent": 100})).unwrap();
        assert_eq!(r["completedCopies"], json!([]));
        assert!(s.mailbox.tasks[&id].is_complete(), "the last occurrence just completes");
        assert_eq!(s.mailbox.tasks.len(), 3);
        assert!(s.mailbox.validate().is_empty());
    }

    #[test]
    fn regenerating_task_counts_from_completion() {
        let mut s = session();
        let id = rule_task(&mut s, json!({"regenerateDays": 3}));
        s.fixed_now = s.fixed_now.map(|t| pim::add_secs(t, 2 * pim::DAY_SECS)); // Sunday 10-11
        s.run("tasks.markComplete", &json!({"ids": [id.0]})).unwrap();
        let tz = s.tz_offset_min;
        assert_eq!(s.mailbox.tasks[&id].due.map(|d| pim::date_string(d, tz)).as_deref(), Some("2026-10-14"));
        assert!(matches!(
            s.run("tasks.update", &json!({"id": id.0, "recurrence": {"regenerateDays": 0}})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("tasks.update", &json!({"id": id.0, "recurrence": "FREQ=SOMETIMES"})),
            Err(CmdError::Params(_))
        ));
        assert!(matches!(
            s.run("tasks.update", &json!({"id": id.0, "recurrence": 7})),
            Err(CmdError::Params(_))
        ));
        s.run("tasks.update", &json!({"id": id.0, "recurrence": null})).unwrap();
        assert!(s.mailbox.tasks[&id].recurrence.is_none());
        // Without a due date the next occurrence cannot be computed: the task just completes.
        let t = task_with(&mut s, json!({"subject": "x", "recurrence": "FREQ=DAILY"}));
        let r = s.run("tasks.markComplete", &json!({"ids": [t]})).unwrap();
        assert_eq!(r["completedCopies"], json!([]));
    }

    fn task_with(s: &mut Session, p: Value) -> u64 {
        s.run("tasks.new", &p).unwrap()["id"].as_u64().unwrap()
    }

    #[test]
    fn follow_up_private_importance_on_tasks() {
        let mut s = session(); // Friday 2026-10-09
        let id = task_with(&mut s, json!({"subject": "T"}));
        s.selection = Selection {
            folder: role(&s, FolderRole::Tasks),
            items: vec![ItemId(id)],
            focus: Some(ItemId(id)),
        };
        let r = s.run("tasks.flag", &json!({"kind": "tomorrow"})).unwrap();
        assert_eq!((r["start"].as_str(), r["due"].as_str()), (Some("2026-10-10"), Some("2026-10-10")));
        let r = s.run("tasks.flag", &json!({"kind": "nextWeek"})).unwrap();
        assert_eq!((r["start"].as_str(), r["due"].as_str()), (Some("2026-10-12"), Some("2026-10-16")));
        s.run("tasks.flag", &json!({"kind": "noDate"})).unwrap();
        assert!(s.mailbox.tasks[&ItemId(id)].due.is_none());
        assert_eq!(s.run("tasks.flag", &json!({"kind": "custom"})).unwrap()["dialog"], "pimFlag");
        s.run("tasks.flag", &json!({"kind": "custom", "due": "2026-11-02"})).unwrap();
        assert!(s.mailbox.tasks[&ItemId(id)].due.is_some());
        assert!(s.run("tasks.flag", &json!({"kind": "someday"})).is_err());
        assert_eq!(s.run("tasks.private", &json!({})).unwrap()["private"], true);
        assert_eq!(s.run("tasks.private", &json!({})).unwrap()["private"], false);
        assert_eq!(s.run("tasks.importance", &json!({"level": "high"})).unwrap()["priority"], "high");
        assert_eq!(s.run("tasks.importance", &json!({"level": "high"})).unwrap()["priority"], "normal");
        assert_eq!(s.run("tasks.importance", &json!({"level": "low"})).unwrap()["priority"], "low");
        assert!(s.run("tasks.importance", &json!({"level": "urgent"})).is_err());
        assert_eq!(s.run("tasks.allCategories", &json!({})).unwrap()["dialog"], "colorCategories");
        assert_eq!(s.run("tasks.move", &json!({})).unwrap()["dialog"], "pimMove");
        s.selection.items.clear();
        s.selection.focus = None;
        assert!(matches!(s.run("tasks.flag", &json!({"kind": "today"})), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn respond_to_tasks() {
        let mut s = session();
        let plain = task_with(&mut s, json!({"subject": "Plain", "body": "Some notes"}));
        assert!(matches!(s.run("tasks.reply", &json!({"ids": [plain]})), Err(CmdError::Disabled(_))));
        s.selection = Selection {
            folder: role(&s, FolderRole::Tasks),
            items: vec![ItemId(plain)],
            focus: Some(ItemId(plain)),
        };
        assert!(s.enabled("tasks.reply").is_some());
        // Forward: the text and the VTODO.
        let r = s.run("tasks.forward", &json!({})).unwrap();
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.subject, "FW: Plain");
        assert!(c.body.to_plain_text().contains("Some notes"));
        let ics = String::from_utf8(c.attachments[0].data.clone()).unwrap();
        assert!(
            ics.contains("BEGIN:VTODO") && ics.contains("SUMMARY:Plain") && ics.contains("METHOD:PUBLISH"),
            "{ics}"
        );
        let back = mailcraft_formats::ics::read_str(&ics, Default::default()).unwrap();
        assert_eq!(back.tasks[0].subject, "Plain");
        let r = s.run("tasks.forward", &json!({"as": "attachment"})).unwrap();
        assert!(s.composes[&r["compose"].as_u64().unwrap()].body.to_plain_text().trim().is_empty());
        // A task request I received: Reply goes to the requester, Reply All adds the other assignees.
        let got = task_with(&mut s, json!({"subject": "Assigned to me"}));
        if let Some(t) = s.mailbox.tasks.get_mut(&ItemId(got)) {
            t.requested_by = Some("Boss <boss@example.com>".into());
            t.assigned_to = vec!["Me Myself <me@example.com>".into(), "Pat <pat@example.org>".into()];
        }
        s.selection.items = vec![ItemId(got)];
        s.selection.focus = Some(ItemId(got));
        let r = s.run("tasks.reply", &json!({"ids": [got]})).unwrap();
        assert_eq!(r["to"], json!(["Boss <boss@example.com>"]));
        assert_eq!(s.composes[&r["compose"].as_u64().unwrap()].subject, "RE: Assigned to me");
        let r = s.run("tasks.replyAll", &json!({"ids": [got]})).unwrap();
        assert_eq!(r["to"], json!(["Boss <boss@example.com>", "Pat <pat@example.org>"]));
        // Meeting about the task (`form: false` saves it without opening the meeting form first).
        let r = s.run("tasks.meeting", &json!({"ids": [got], "form": false})).unwrap();
        let a = &s.mailbox.appointments[&ItemId(r["id"].as_u64().unwrap())];
        assert_eq!(a.subject, "Assigned to me");
        assert_eq!(a.attendees.len(), 2);
    }

    #[test]
    fn assign_task_sends_a_vtodo_request() {
        let mut s = session();
        assert_eq!(s.run("tasks.assign", &json!({})).unwrap()["dialog"], "taskRequest");
        assert!(s.run("tasks.assign", &json!({"to": "pat@example.org"})).is_err(), "needs a subject");
        assert!(s.run("tasks.assign", &json!({"to": "not an address", "subject": "x"})).is_err());
        // A local-only profile cannot send: the request stays open in its window.
        let r = s
            .run("tasks.assign", &json!({"to": "Pat <pat@example.org>", "subject": "Budget", "due": "2026-10-20", "recurrence": "FREQ=MONTHLY"}))
            .unwrap();
        assert_eq!(r["sent"], false);
        let cid = r["compose"].as_u64().unwrap();
        let c = &s.composes[&cid];
        assert_eq!(c.subject, "Task Request: Budget");
        assert_eq!(c.to, "Pat <pat@example.org>");
        assert_eq!(c.attachments[0].mime_type, "text/calendar; method=REQUEST");
        let ics = String::from_utf8(c.attachments[0].data.clone()).unwrap().replace("\r\n ", "");
        for needle in [
            "METHOD:REQUEST",
            "BEGIN:VTODO",
            "SUMMARY:Budget",
            "ATTENDEE",
            "mailto:pat@example.org",
            "ORGANIZER",
            "mailto:me@example.com",
            "RRULE:FREQ=MONTHLY",
        ] {
            assert!(ics.contains(needle), "{needle} missing in {ics}");
        }
        let t = &s.mailbox.tasks[&ItemId(r["task"].as_u64().unwrap())];
        assert_eq!(t.assigned_to, vec!["Pat <pat@example.org>".to_string()]);
        // Assigned view.
        let folder = role(&s, FolderRole::Tasks);
        s.selection = Selection { folder, items: vec![], focus: None };
        s.run("view.changeView", &json!({"name": "Assigned"})).unwrap();
        assert_eq!(view_rows(&s, folder).len(), 1);
        // With a sending account the request goes to the Outbox.
        if let Some(a) = s.mailbox.accounts.first_mut() {
            a.kind = mailcraft_model::AccountKind::Jmap;
        }
        let outbox = role(&s, FolderRole::Outbox);
        let r = s
            .run("tasks.assign", &json!({"to": ["pat@example.org"], "subject": "Send it", "keepCopy": false}))
            .unwrap();
        assert_eq!(r["sent"], true, "{r}");
        assert_eq!(r["task"], Value::Null);
        assert!(s.mailbox.mail.values().any(|m| m.folder == outbox && m.subject == "Task Request: Send it"));
    }

    #[test]
    fn task_options_reminders_quick_click_and_assign_defaults() {
        let mut s = session();
        // Off (default): no reminder.
        let r = s.run("tasks.new", &json!({"subject": "A", "due": "2026-10-15"})).unwrap();
        assert_eq!(s.mailbox.tasks[&ItemId(r["id"].as_u64().unwrap())].reminder, None);
        s.run("options.set", &json!({"key": "tasks.remindersOnDueDates", "value": true})).unwrap();
        s.run("options.set", &json!({"key": "tasks.reminderTime", "value": "9:30 AM"})).unwrap();
        let r = s.run("tasks.new", &json!({"subject": "B", "due": "2026-10-15"})).unwrap();
        let t = &s.mailbox.tasks[&ItemId(r["id"].as_u64().unwrap())];
        let due = t.due.unwrap();
        assert_eq!(t.reminder.unwrap().unix - due.unix, (9 * 60 + 30) * 60);
        // An explicit reminder (or none) wins; no due date, no reminder.
        let r = s.run("tasks.new", &json!({"subject": "C", "due": "2026-10-15", "reminder": null})).unwrap();
        assert_eq!(s.mailbox.tasks[&ItemId(r["id"].as_u64().unwrap())].reminder, None);
        let r = s.run("tasks.new", &json!({"subject": "D"})).unwrap();
        assert_eq!(s.mailbox.tasks[&ItemId(r["id"].as_u64().unwrap())].reminder, None);

        // Quick Click flag: Follow Up without a kind.
        let id = r["id"].as_u64().unwrap();
        let folder = role(&s, FolderRole::Tasks);
        s.selection = Selection { folder, items: vec![ItemId(id)], focus: Some(ItemId(id)) };
        s.run("mail.setQuickClickFlag", &json!({"kind": "Tomorrow"})).unwrap();
        assert_eq!(s.option("tasks.quickClickFlag"), Some(json!("tomorrow")));
        let r = s.run("tasks.flag", &json!({"ids": [id]})).unwrap();
        assert_eq!(r["kind"], "tomorrow");
        assert_eq!(r["due"], "2026-10-10");
        s.run("mail.setQuickClickFlag", &json!({"kind": "complete"})).unwrap();
        s.run("tasks.flag", &json!({"ids": [id]})).unwrap();
        assert!(s.mailbox.tasks[&ItemId(id)].is_complete());
        assert!(matches!(
            s.run("mail.setQuickClickFlag", &json!({"kind": "never"})),
            Err(CmdError::Params(_))
        ));
        assert_eq!(s.option("tasks.quickClickFlag"), Some(json!("complete")));

        // Assign Task keeps a copy unless Options › Tasks says not to.
        s.run("options.set", &json!({"key": "tasks.keepCopies", "value": false})).unwrap();
        let r = s.run("tasks.assign", &json!({"to": ["pat@example.org"], "subject": "No copy"})).unwrap();
        assert_eq!(r["task"], Value::Null, "{r}");
        assert!(!s.mailbox.tasks.values().any(|t| t.subject == "No copy"));
    }

    #[test]
    fn todo_list_includes_flagged_contacts() {
        let mut s = session();
        let c = s.run("people.new", &json!({"fullName": "Ann Lee"})).unwrap()["id"].as_u64().unwrap();
        s.selection = Selection {
            folder: role(&s, FolderRole::Contacts),
            items: vec![ItemId(c)],
            focus: Some(ItemId(c)),
        };
        s.run("people.flag", &json!({"ids": [c], "kind": "today"})).unwrap();
        let items = todo_list(&s);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["kind"], "contact");
        let r = s.run("tasks.markComplete", &json!({"ids": [c]})).unwrap();
        assert_eq!(r["contacts"], json!([c]));
        assert!(todo_list(&s).is_empty());
    }

    #[test]
    fn next_occurrence_is_bounded() {
        let t = Task {
            due: Some(DateTime::from_unix(0, 0)),
            recurrence: Some(TaskRecurrence {
                rrule: "FREQ=YEARLY;INTERVAL=99".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(next_occurrence(&t, DateTime::from_unix(0, 0), 0).is_some());
        let far = Task { due: Some(DateTime::from_unix(253_402_000_000, 0)), ..t.clone() };
        assert!(next_occurrence(&far, DateTime::from_unix(0, 0), 0).is_none());
    }

    // -----------------------------------------------------------------------------------------------------
    // Task requests between two people

    fn person(addr: &str, name: &str) -> Session {
        let mut s = session();
        if let Some(a) = s.mailbox.accounts.first_mut() {
            a.address = addr.into();
            a.sender_name = name.into();
        }
        s.refresh_me();
        s
    }

    /// Delivers compose `cid` of `from` into `to`'s Inbox, built like a sent message.
    fn deliver(from: &Session, cid: u64, to: &mut Session) -> ItemId {
        let c = &from.composes[&cid];
        let acc = from.mailbox.default_account().unwrap();
        let attachments = c
            .attachments
            .iter()
            .map(|a| (a.filename.clone(), a.mime_type.clone(), a.data.clone()))
            .collect::<Vec<_>>();
        deliver_raw(
            to,
            (&acc.sender_name, &acc.address),
            &c.to,
            &c.subject,
            &c.body.to_plain_text(),
            attachments,
        )
    }

    fn deliver_raw(
        to: &mut Session,
        from: (&str, &str),
        rcpt: &str,
        subject: &str,
        text: &str,
        attachments: Vec<(String, String, Vec<u8>)>,
    ) -> ItemId {
        let mut b = mailcraft_mime::MessageBuilder::new();
        b.from = Some(mailcraft_model::Address::new(Some(from.0), from.1));
        b.to = mailcraft_mime::parse_address_list(rcpt);
        b.subject = subject.into();
        b.text = Some(text.into());
        b.attachments = attachments
            .into_iter()
            .map(|(filename, mime_type, data)| mailcraft_mime::AttachmentSpec {
                filename,
                mime_type,
                data,
                content_id: None,
            })
            .collect();
        let raw = b.build();
        let msg = mailcraft_mime::parse(&raw).unwrap();
        let mut item = MailItem::from_message(&msg, &raw, to.now());
        item.folder = role(to, FolderRole::Inbox);
        to.mailbox.add_mail(item)
    }

    fn assign_to_ann(org: &mut Session, subject: &str) -> (ItemId, u64) {
        let r = org
            .run(
                "tasks.assign",
                &json!({"to": "Ann Lee <ann@example.com>", "subject": subject, "due": "2026-10-20",
                        "body": "Numbers for Q4", "priority": "high", "send": false}),
            )
            .unwrap();
        (ItemId(r["task"].as_u64().unwrap()), r["compose"].as_u64().unwrap())
    }

    #[test]
    fn deleting_an_accepted_task_request_asks_and_answers_the_assigner() {
        let mut org = person("me@example.com", "Me Myself");
        let (_, cid) = assign_to_ann(&mut org, "Budget");
        let mut ann = person("ann@example.com", "Ann Lee");
        let req = deliver(&org, cid, &mut ann);
        let accept = |ann: &mut Session| {
            let r = ann.run("tasks.acceptRequest", &json!({"id": req.0, "send": "none"})).unwrap();
            ItemId(r["task"].as_u64().unwrap())
        };
        let t = accept(&mut ann);
        // Delete asks first.
        assert_eq!(ann.run("tasks.delete", &json!({"ids": [t.0]})).unwrap()["dialog"], "deleteTaskRequest");
        assert_eq!(ann.mailbox.tasks[&t].folder, role(&ann, FolderRole::Tasks), "nothing deleted yet");
        assert!(ann.run("tasks.delete", &json!({"ids": [t.0], "request": "maybe"})).is_err());
        // The assigner was told: the answer went out (Sent or Outbox, counted in `notified`) or waits in an
        // open message window.
        let told = |ann: &Session, r: &Value, subject: &str| {
            let mailed = ann.mailbox.mail.values().any(|m| m.subject == subject);
            let open = ann.composes.values().any(|c| c.subject == subject);
            if r["notified"] == 1 {
                assert!(mailed, "{subject}: counted as sent but no message");
            } else {
                assert_eq!(r["notified"], 0);
                assert!(open, "{subject}: not sent and no message window");
            }
        };
        // Decline: the DECLINED answer goes to the assigner, then the task is deleted.
        let r = ann.run("tasks.delete", &json!({"ids": [t.0], "request": "decline"})).unwrap();
        assert_eq!(ann.mailbox.tasks[&t].folder, role(&ann, FolderRole::DeletedItems));
        told(&ann, &r, "Task Declined: Budget");
        if let Some(c) = ann.composes.values().find(|c| c.subject == "Task Declined: Budget") {
            let ics = String::from_utf8(c.attachments[0].data.clone()).unwrap().replace("\r\n ", "");
            assert!(ics.contains("PARTSTAT=DECLINED"), "{ics}");
        }
        // Mark complete and notify: the task completes before it goes, and the completion report is sent.
        let t = accept(&mut ann);
        let r = ann.run("tasks.delete", &json!({"ids": [t.0], "request": "complete"})).unwrap();
        let done = ann.mailbox.tasks.get(&t).is_none_or(|x| x.is_complete());
        assert!(done);
        told(&ann, &r, "Task Completed: Budget");
        // Delete without telling anyone: no answer at all.
        let t = accept(&mut ann);
        let (mails, windows) = (ann.mailbox.mail.len(), ann.composes.len());
        let r = ann.run("tasks.delete", &json!({"ids": [t.0], "request": "delete"})).unwrap();
        assert_eq!(r["notified"], 0);
        assert_eq!((ann.mailbox.mail.len(), ann.composes.len()), (mails, windows));
        // Plain tasks delete straight away.
        let tasks_folder = role(&ann, FolderRole::Tasks);
        let plain = create_task(&mut ann, tasks_folder, &json!({"subject": "Mine"})).unwrap();
        assert!(ann.run("tasks.delete", &json!({"ids": [plain.0]})).unwrap().get("dialog").is_none());
    }

    #[test]
    fn task_request_round_trip() {
        let mut org = session();
        let (copy, cid) = assign_to_ann(&mut org, "Budget");
        assert_eq!(org.mailbox.tasks[&copy].request_state, TaskRequestState::Waiting);
        assert!(!org.mailbox.tasks[&copy].uid.is_empty());
        let mut ann = person("ann@example.com", "Ann Lee");
        let req = deliver(&org, cid, &mut ann);

        // The reading pane's query.
        let info = task_request_info(&ann, req).unwrap();
        assert_eq!((info.method.as_str(), info.can_respond, info.task), ("REQUEST", true, None));
        assert_eq!(info.subject, "Budget");
        assert_eq!(info.due.as_deref(), Some("2026-10-20"));
        assert!(info.requested_by.contains("me@example.com"), "{info:?}");
        assert_eq!(info.uid, org.mailbox.tasks[&copy].uid);
        let r = ann.run("tasks.requestInfo", &json!({"id": req.0})).unwrap();
        assert_eq!(r["canRespond"], true);
        // My own request is not mine to answer.
        let atts: Vec<(String, String, Vec<u8>)> = org.composes[&cid]
            .attachments
            .iter()
            .map(|a| (a.filename.clone(), a.mime_type.clone(), a.data.clone()))
            .collect();
        let mine =
            deliver_raw(&mut org, ("Me Myself", "me@example.com"), "ann@example.com", "Copy", "", atts);
        let info = task_request_info(&org, mine).unwrap();
        assert!(!info.can_respond && info.i_am_requester);
        assert_eq!(info.info, "You sent this task request.");
        assert!(matches!(org.run("tasks.acceptRequest", &json!({"id": mine.0})), Err(CmdError::Disabled(_))));

        // Accept: the task joins Ann's list, requested by the organizer; the response goes back.
        let r = ann
            .run("tasks.acceptRequest", &json!({"id": req.0, "send": "edit", "comment": "On it"}))
            .unwrap();
        let t = ItemId(r["task"].as_u64().unwrap());
        let task = &ann.mailbox.tasks[&t];
        assert_eq!(task.folder, role(&ann, FolderRole::Tasks));
        assert_eq!(task.subject, "Budget");
        assert_eq!(task.priority, TaskPriority::High);
        assert_eq!(task.body, "Numbers for Q4");
        assert!(task.requested_by.as_deref().unwrap().contains("me@example.com"));
        assert_eq!(task.request_state, TaskRequestState::Accepted);
        assert_eq!(task.uid, org.mailbox.tasks[&copy].uid);
        assert!(!task.is_assigned_by_me());
        let reply = r["compose"].as_u64().unwrap();
        assert_eq!(ann.composes[&reply].subject, "Task Accepted: Budget");
        assert!(ann.composes[&reply].to.contains("me@example.com"));
        assert!(ann.composes[&reply].body.to_plain_text().starts_with("On it"));
        let ics =
            String::from_utf8(ann.composes[&reply].attachments[0].data.clone()).unwrap().replace("\r\n ", "");
        assert!(ics.contains("METHOD:REPLY") && ics.contains("PARTSTAT=ACCEPTED"), "{ics}");
        assert!(task_request_info(&ann, req).unwrap().task == Some(t.0));
        // Accepting again updates the same task.
        ann.run("tasks.acceptRequest", &json!({"id": req.0, "send": "none"})).unwrap();
        assert_eq!(ann.mailbox.tasks.values().filter(|x| x.is_requested()).count(), 1);
        // Declining after accepting moves it to Deleted Items; accepting once more brings it back.
        ann.run("tasks.declineRequest", &json!({"id": req.0, "send": "none"})).unwrap();
        assert_eq!(ann.mailbox.tasks[&t].folder, role(&ann, FolderRole::DeletedItems));
        assert_eq!(task_request_info(&ann, req).unwrap().info, "You declined this task.");
        let r = ann.run("tasks.acceptRequest", &json!({"id": req.0, "send": "none"})).unwrap();
        assert_eq!(r["task"], t.0);
        assert_eq!(ann.mailbox.tasks[&t].folder, role(&ann, FolderRole::Tasks));
        assert_eq!(ann.mailbox.tasks[&t].request_state, TaskRequestState::Accepted);
        // Ann's task is not in her Assigned view but Reply reaches the requester.
        ann.selection = Selection { folder: role(&ann, FolderRole::Tasks), items: vec![t], focus: Some(t) };
        ann.run("view.changeView", &json!({"name": "Assigned"})).unwrap();
        assert!(view_rows(&ann, role(&ann, FolderRole::Tasks)).is_empty());
        let r = ann.run("tasks.reply", &json!({"ids": [t.0]})).unwrap();
        assert!(r["to"][0].as_str().unwrap().contains("me@example.com"));

        // The organizer applies the answer to the assigned copy.
        let answer = deliver(&ann, reply, &mut org);
        let info = task_request_info(&org, answer).unwrap();
        assert_eq!(info.method, "REPLY");
        assert_eq!(info.response.as_deref(), Some("accepted"));
        assert_eq!((info.task, info.can_update), (Some(copy.0), true));
        assert_eq!(info.info, "Ann Lee accepted the task.");
        org.run("tasks.processReport", &json!({"id": answer.0})).unwrap();
        assert_eq!(org.mailbox.tasks[&copy].request_state, TaskRequestState::Accepted);

        // A status report carries the progress.
        ann.run("tasks.update", &json!({"id": t.0, "percent": 50})).unwrap();
        let r = ann.run("tasks.sendStatusReport", &json!({"ids": [t.0]})).unwrap();
        let rep = r["compose"].as_u64().unwrap();
        assert_eq!(ann.composes[&rep].subject, "Task Status Report: Budget");
        assert!(ann.composes[&rep].body.to_plain_text().contains("% Complete: 50%"));
        let report = deliver(&ann, rep, &mut org);
        assert!(task_request_info(&org, report).unwrap().info.contains("50% complete"));
        org.run("tasks.processReport", &json!({"id": report.0})).unwrap();
        assert_eq!(org.mailbox.tasks[&copy].percent_complete, 50);
        assert_eq!(org.mailbox.tasks[&copy].status, TaskStatus::InProgress);

        // Completing the task sends the completion report by itself (Options › Tasks, on by default).
        let r = ann.run("tasks.markComplete", &json!({"ids": [t.0]})).unwrap();
        let done = r["reports"][0].as_u64().unwrap();
        assert_eq!(ann.composes[&done].subject, "Task Completed: Budget");
        let done_mail = deliver(&ann, done, &mut org);
        org.run("tasks.processReport", &json!({"id": done_mail.0})).unwrap();
        assert!(org.mailbox.tasks[&copy].is_complete());
        assert!(org.mailbox.tasks[&copy].completed.is_some());
        // Reopening and completing again with the option off sends nothing.
        ann.run("options.set", &json!({"key": "tasks.statusReport", "value": false})).unwrap();
        ann.run("tasks.update", &json!({"id": t.0, "complete": false})).unwrap();
        let r = ann.run("tasks.update", &json!({"id": t.0, "complete": true})).unwrap();
        assert!(r.get("reports").is_none(), "{r}");
        assert!(ann.mailbox.validate().is_empty() && org.mailbox.validate().is_empty());
    }

    #[test]
    fn declining_and_hostile_answers() {
        let mut org = session();
        let (copy, cid) = assign_to_ann(&mut org, "Audit");
        let mut ann = person("ann@example.com", "Ann Lee");
        let req = deliver(&org, cid, &mut ann);
        let r = ann.run("tasks.declineRequest", &json!({"id": req.0, "send": "edit"})).unwrap();
        assert_eq!(r["task"], Value::Null);
        assert!(!ann.mailbox.tasks.values().any(|t| t.subject == "Audit"));
        let reply = r["compose"].as_u64().unwrap();
        assert_eq!(ann.composes[&reply].subject, "Task Declined: Audit");
        let answer = deliver(&ann, reply, &mut org);
        assert_eq!(task_request_info(&org, answer).unwrap().info, "Ann Lee declined the task.");
        // A REPLY is not a request, and a request is not a report.
        assert!(matches!(org.run("tasks.acceptRequest", &json!({"id": answer.0})), Err(CmdError::Params(_))));
        assert!(matches!(ann.run("tasks.processReport", &json!({"id": req.0})), Err(CmdError::Params(_))));
        // Someone the task was not assigned to cannot answer for it.
        let mut eve = person("eve@example.net", "Eve");
        let eve_req = deliver(&org, cid, &mut eve);
        let r = eve.run("tasks.acceptRequest", &json!({"id": eve_req.0, "send": "edit"})).unwrap();
        let forged = deliver(&eve, r["compose"].as_u64().unwrap(), &mut org);
        assert!(matches!(org.run("tasks.processReport", &json!({"id": forged.0})), Err(CmdError::Params(_))));
        assert_eq!(org.mailbox.tasks[&copy].request_state, TaskRequestState::Waiting);
        org.run("tasks.processReport", &json!({"id": answer.0})).unwrap();
        assert_eq!(org.mailbox.tasks[&copy].request_state, TaskRequestState::Declined);
        // Messages without a task.
        let plain =
            deliver_raw(&mut org, ("Pat", "pat@example.org"), "me@example.com", "Hi", "Hello", vec![]);
        assert!(task_request_info(&org, plain).is_none());
        for cmd in ["tasks.requestInfo", "tasks.acceptRequest", "tasks.declineRequest", "tasks.processReport"]
        {
            assert!(matches!(org.run(cmd, &json!({"id": plain.0})), Err(CmdError::Params(_))), "{cmd}");
            assert!(org.run(cmd, &json!({"id": u64::MAX})).is_err());
        }
    }

    #[test]
    fn itip_vtodo_from_another_client() {
        let mut ann = person("ann@example.com", "Ann Lee");
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Other client//EN\r\nMETHOD:REQUEST\r\n\
BEGIN:VTODO\r\nUID:todo-42@example.com\r\nDTSTAMP:20261009T080000Z\r\nSUMMARY:Water the plants\r\n\
DUE;VALUE=DATE:20261016\r\nPRIORITY:9\r\nSTATUS:NEEDS-ACTION\r\nRRULE:FREQ=WEEKLY;BYDAY=FR\r\n\
ORGANIZER;CN=\"Boss\":mailto:boss@example.com\r\n\
ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ann@example.com\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let req = deliver_raw(
            &mut ann,
            ("Boss", "boss@example.com"),
            "ann@example.com",
            "Plants",
            "Please",
            vec![("task.ics".into(), "text/calendar; method=REQUEST".into(), ics.as_bytes().to_vec())],
        );
        let info = task_request_info(&ann, req).unwrap();
        assert_eq!(info.requested_by, "Boss <boss@example.com>");
        assert_eq!(info.priority, "low");
        let r = ann.run("tasks.acceptRequest", &json!({"id": req.0, "send": "edit"})).unwrap();
        let t = &ann.mailbox.tasks[&ItemId(r["task"].as_u64().unwrap())];
        assert_eq!(t.uid, "todo-42@example.com");
        assert_eq!(t.recurrence.as_ref().map(|r| r.rrule.as_str()), Some("FREQ=WEEKLY;BYDAY=FR"));
        assert_eq!(t.due.map(|d| pim::date_string(d, ann.tz_offset_min)).as_deref(), Some("2026-10-16"));
        let reply = &ann.composes[&r["compose"].as_u64().unwrap()];
        let text = String::from_utf8(reply.attachments[0].data.clone()).unwrap().replace("\r\n ", "");
        assert!(text.contains("UID:todo-42@example.com") && text.contains("ORGANIZER"), "{text}");
        // Junk calendar data is not a task.
        assert!(parse_task_message(b"BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:\x00\r\n", 0).is_some());
        assert!(parse_task_message(b"\xff\xfe garbage", 0).is_none());
        assert!(
            parse_task_message(b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n", 0)
                .is_none()
        );
    }

    /// Selects `id` the way the Tasks view does (the engine selection follows the To-Do List row).
    fn select(s: &mut Session, id: ItemId) {
        s.selection = Selection { folder: role(s, FolderRole::Tasks), items: vec![id], focus: Some(id) };
    }

    fn flag_of(s: &Session, id: ItemId) -> FlagState {
        s.mailbox.mail.get(&id).map(|m| m.flag.clone()).unwrap_or(FlagState::None)
    }

    #[test]
    fn tasks_home_follow_up_tags_and_categories_act_on_flagged_messages_and_contacts() {
        let mut s = session();
        let m = add_mail(&mut s, "Quarterly report", flagged());
        let c = ItemId(s.run("people.new", &json!({"fullName": "Ann Lee"})).unwrap()["id"].as_u64().unwrap());
        s.run("people.flag", &json!({"ids": [c.0], "kind": "today"})).unwrap();
        select(&mut s, m);
        // The ribbon's checks follow the To-Do List row, not only tasks.
        for cmd in ["tasks.flag", "tasks.private", "tasks.importance", "tasks.allCategories", "tasks.forward"]
        {
            assert_eq!(s.enabled(cmd), None, "{cmd}");
        }
        // Follow Up › Tomorrow on a flagged message keeps its flag text and moves its dates.
        s.run("tasks.flag", &json!({"kind": "tomorrow"})).unwrap();
        let (start, due) = crate::cmd::mail::flag_dates(&s, crate::cmd::mail::FlagKind::Tomorrow);
        assert_eq!(
            flag_of(&s, m),
            FlagState::Flagged { label: "Follow up".into(), start, due, reminder: None }
        );
        assert_eq!(follow_up_state(&s, m), Some("tomorrow"));
        // Custom: a null date clears it, a date left out stays.
        s.run("tasks.flag", &json!({"kind": "custom", "start": null, "due": "2026-10-20"})).unwrap();
        let FlagState::Flagged { start, due, .. } = flag_of(&s, m) else { panic!("still flagged") };
        assert_eq!(start, None);
        assert_eq!(due.map(|d| pim::date_string(d, s.tz_offset_min)).as_deref(), Some("2026-10-20"));
        assert_eq!(follow_up_state(&s, m), Some("custom"), "custom dates match no named Follow Up entry");
        // The flagged contact too.
        s.run("tasks.flag", &json!({"ids": [c.0], "kind": "nextWeek"})).unwrap();
        assert_eq!(follow_up_state(&s, c), Some("nextWeek"));
        // Categorize (and All Categories) on a message and a contact together.
        let cat = s.mailbox.categories.first().map(|x| x.name.clone()).unwrap();
        s.run("tasks.categorize", &json!({"ids": [m.0, c.0], "name": cat})).unwrap();
        assert!(s.mailbox.mail[&m].categories.contains(&cat));
        assert!(s.mailbox.contacts[&c].categories.contains(&cat));
        assert_eq!(
            s.run("tasks.allCategories", &json!({"ids": [m.0]})).unwrap()["dialog"],
            "colorCategories"
        );
        s.run("tasks.categorize", &json!({"ids": [m.0], "name": "", "clear": true})).unwrap();
        assert!(s.mailbox.mail[&m].categories.is_empty());
        // Private and High Importance.
        s.run("tasks.private", &json!({"ids": [m.0]})).unwrap();
        assert_eq!(s.mailbox.mail[&m].sensitivity, mailcraft_model::Sensitivity::Private);
        s.run("tasks.importance", &json!({"ids": [m.0], "level": "high"})).unwrap();
        assert_eq!(s.mailbox.mail[&m].importance, mailcraft_model::Importance::High);
        // Follow Up › Complete (the Quick Click when set to it) completes the message's flag.
        s.run("tasks.flag", &json!({"ids": [m.0], "kind": "complete"})).unwrap();
        assert!(matches!(flag_of(&s, m), FlagState::Complete { .. }));
        // A message that is not flagged is not a To-Do List row.
        let plain = add_mail(&mut s, "Plain", FlagState::None);
        assert!(s.run("tasks.flag", &json!({"ids": [plain.0], "kind": "today"})).is_err());
        assert_eq!(flag_of(&s, plain), FlagState::None);
    }

    #[test]
    fn tasks_home_respond_move_and_delete_act_on_flagged_messages() {
        let mut s = session();
        let m = add_mail(&mut s, "Budget", flagged());
        if let Some(x) = s.mailbox.mail.get_mut(&m) {
            x.from = Some(mailcraft_model::Address::new(Some("Ann Lee"), "ann@example.com"));
        }
        select(&mut s, m);
        assert_eq!(s.enabled("tasks.reply"), None);
        // Reply / Reply All / Forward answer the message as Mail does.
        let r = s.run("tasks.reply", &json!({})).unwrap();
        let reply = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(reply.source, Some(m));
        assert!(reply.to.contains("ann@example.com"), "{}", reply.to);
        assert!(reply.subject.contains("Budget"));
        let r = s.run("tasks.replyAll", &json!({"ids": [m.0]})).unwrap();
        assert!(r["compose"].as_u64().is_some());
        let r = s.run("tasks.forward", &json!({"ids": [m.0]})).unwrap();
        let fwd = &s.composes[&r["compose"].as_u64().unwrap()];
        assert!(fwd.to.is_empty() && fwd.subject.contains("Budget"));
        // Move without a folder: Mail's folder picker; with a mail folder: the message moves.
        assert_eq!(s.run("tasks.move", &json!({"ids": [m.0]})).unwrap()["dialog"], "moveToFolder");
        let inbox = role(&s, FolderRole::Inbox);
        let archive = s.mailbox.create_folder(inbox, "Projects").unwrap();
        let r = s.run("tasks.move", &json!({"ids": [m.0], "folder": archive.0})).unwrap();
        assert_eq!(r["moved"], json!([m.0]));
        assert_eq!(s.mailbox.mail[&m].folder, archive);
        // A tasks folder cannot hold the message.
        let tasks = role(&s, FolderRole::Tasks);
        assert!(s.run("tasks.move", &json!({"ids": [m.0], "folder": tasks.0})).is_err());
        // Delete: the message goes to Deleted Items (and leaves the To-Do List), in one undo step.
        let r = s.run("tasks.delete", &json!({"ids": [m.0]})).unwrap();
        assert_eq!(r["mail"], json!([m.0]));
        assert_eq!(s.mailbox.mail[&m].folder, role(&s, FolderRole::DeletedItems));
        assert!(todo_list(&s).is_empty());
        s.run("app.undo", &json!({})).unwrap();
        assert_eq!(s.mailbox.mail[&m].folder, archive);
        // Nothing that is a To-Do List row: refused, nothing changes.
        let plain = add_mail(&mut s, "Plain", FlagState::None);
        assert!(s.run("tasks.delete", &json!({"ids": [plain.0]})).is_err());
        assert_eq!(s.mailbox.mail[&plain].folder, inbox);
    }

    #[test]
    fn tasks_home_on_a_flagged_contact_and_a_mixed_selection() {
        let mut s = session();
        let c = ItemId(
            s.run("people.new", &json!({"fullName": "Ann Lee", "email": "ann@example.com"})).unwrap()["id"]
                .as_u64()
                .unwrap(),
        );
        s.run("people.flag", &json!({"ids": [c.0], "kind": "today"})).unwrap();
        select(&mut s, c);
        // A contact cannot be answered; it is forwarded as a contact card.
        assert!(s.enabled("tasks.reply").is_some());
        assert!(s.run("tasks.reply", &json!({"ids": [c.0]})).is_err());
        let r = s.run("tasks.forward", &json!({"ids": [c.0]})).unwrap();
        let fwd = &s.composes[&r["compose"].as_u64().unwrap()];
        assert!(fwd.attachments.iter().any(|a| a.filename.ends_with(".vcf")), "{:?}", fwd.attachments);
        // A task and a flagged message moved to a tasks folder: the task moves, the message is skipped.
        let t = ItemId(s.run("tasks.new", &json!({"subject": "Call"})).unwrap()["id"].as_u64().unwrap());
        let m = add_mail(&mut s, "Notes", flagged());
        let tasks = role(&s, FolderRole::Tasks);
        let other = s.mailbox.create_folder(tasks, "Later").unwrap();
        if let Some(f) = s.mailbox.folders.get_mut(&other) {
            f.kind = mailcraft_model::FolderKind::Tasks;
        }
        let r = s.run("tasks.move", &json!({"ids": [t.0, m.0], "folder": other.0})).unwrap();
        assert_eq!(r["moved"], json!([t.0]));
        assert_eq!(r["skipped"], json!([m.0]));
        // Without a folder a mixed selection gets the tasks folder picker with every row.
        assert_eq!(s.run("tasks.move", &json!({"ids": [t.0, m.0]})).unwrap()["dialog"], "pimMove");
        // Delete on a task, a message and a contact together.
        let r = s.run("tasks.delete", &json!({"ids": [t.0, m.0, c.0]})).unwrap();
        assert_eq!(r["deleted"].as_array().map(Vec::len), Some(3));
        assert_eq!(s.mailbox.mail[&m].folder, role(&s, FolderRole::DeletedItems));
        assert_eq!(s.mailbox.contacts.get(&c).map(|x| x.folder), Some(role(&s, FolderRole::DeletedItems)));
    }

    #[test]
    fn custom_follow_up_dialog_starts_from_the_dates_and_can_clear_them() {
        let mut s = session();
        let t = ItemId(
            s.run("tasks.new", &json!({"subject": "Plan", "start": "2026-10-09", "due": "2026-10-12", "reminder": "2026-10-12T09:00"}))
                .unwrap()["id"]
                .as_u64()
                .unwrap(),
        );
        select(&mut s, t);
        s.run("tasks.flag", &json!({"kind": "custom"})).unwrap();
        let Some(crate::Notice::OpenDialog(name, p)) = s.notices.last().cloned() else { panic!("no dialog") };
        assert_eq!(name, "pimFlag");
        assert_eq!((p["start"].as_str(), p["due"].as_str()), (Some("2026-10-09"), Some("2026-10-12")));
        assert_eq!(p["reminderAt"], "2026-10-12T09:00");
        // The dialog's None (null) clears a date; a reminder unchecked (null) clears the reminder.
        s.run("tasks.flag", &json!({"kind": "custom", "start": null, "due": "2026-10-12", "reminder": null}))
            .unwrap();
        let task = &s.mailbox.tasks[&t];
        assert_eq!(task.start, None);
        assert!(task.due.is_some());
        assert_eq!(task.reminder, None);
        // Dates no named entry gives check Custom.
        assert_eq!(follow_up_state(&s, t), Some("custom"));
        s.run("tasks.flag", &json!({"kind": "today"})).unwrap();
        assert_eq!(follow_up_state(&s, t), Some("today"));
    }
}
