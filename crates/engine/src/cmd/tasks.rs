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
//! assigned copy (the Assigned view).
//!
//! **Views.** [`view_rows`] is the list of the Tasks views (`cmd::view::modules`).

use serde_json::{Value, json};

use crate::cmd::calendar::{pim, pim::Kind};
use crate::cmd::util::{opt_bool, opt_str};
use crate::cmd::view::modules::{self, ModuleKind, ViewRow};
use crate::compose::{ComposeAttachment, ComposeState};
use crate::{CmdError, CmdResult, CommandSpec, Session, always};
use mailcraft_model::task::TaskRecurrence;
use mailcraft_model::{
    AccountKind, Appointment, DateTime, Doc, FlagState, FolderId, FolderRole, ItemId, Recurrence, Task,
    TaskPriority, TaskStatus,
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

fn run_delete(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_delete(s, p, Kind::Task)
}
fn run_copy(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("folder").is_none_or(Value::is_null) {
        return move_dialog(s, p, Kind::Task, true);
    }
    pim::run_copy(s, p, Kind::Task)
}
fn run_move(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("folder").is_none_or(Value::is_null) {
        return move_dialog(s, p, Kind::Task, false);
    }
    pim::run_move(s, p, Kind::Task)
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

/// Categorize › All Categories for module items: the module's categories dialog (`pimCategories`), which
/// runs `<area>.categorize`.
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
    pim::dialog(s, "pimCategories", json!({ "ids": pim::ids_json(&ids), "area": area_of(kind), "on": on }))
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

fn run_all_categories(s: &mut Session, p: &Value) -> CmdResult {
    categories_dialog(s, p, Kind::Task)
}
fn run_categorize(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_categorize(s, p, Kind::Task)
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

fn linked_tasks(s: &Session, mail: ItemId) -> Vec<ItemId> {
    s.mailbox.tasks.values().filter(|t| t.from_mail == Some(mail)).map(|t| t.id).collect()
}

fn run_mark_complete(s: &mut Session, p: &Value) -> CmdResult {
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
    let server = s
        .mailbox
        .folder_account(folder)
        .and_then(|a| s.mailbox.account(a))
        .is_some_and(|a| a.kind != AccountKind::Local);
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
                    "Assigned" => !t.assigned_to.is_empty(),
                    "Server Tasks" => server,
                    _ => true,
                }
            })
            .map(task_entry)
            .collect()
    };
    let arrange =
        if modules::show_groups(s, folder) { st.arrangement(kind).map(str::to_owned) } else { None };
    let mut keyed: Vec<((u8, String), (i64, String, ItemId), ViewRow)> = entries
        .into_iter()
        .map(|e| {
            let group = arrange.as_deref().map(|a| match a {
                "categories" => match e.categories.first() {
                    Some(c) => (0, c.clone()),
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
    keyed.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
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
    let ids = pim::require_targets(s, p, Kind::Task)?;
    // No kind: the Quick Click flag (Options › Tasks › Quick Click).
    let quick = crate::cmd::mail::quick_click_kind(s);
    let kind = opt_str(p, "kind").unwrap_or(&quick);
    if kind == "complete" {
        let now = s.now();
        for id in &ids {
            if let Some(t) = s.mailbox.tasks.get_mut(id) {
                set_complete(t, true, now);
            }
        }
        return Ok(json!({ "flagged": pim::ids_json(&ids), "kind": "complete" }));
    }
    if kind == "custom" {
        if !pim::has_any(p, &["start", "due", "reminder"]) {
            return pim::dialog(s, "pimFlag", json!({ "ids": pim::ids_json(&ids), "area": "tasks" }));
        }
        let mut out = Vec::new();
        for id in &ids {
            let Some(mut t) = s.mailbox.tasks.get(id).cloned() else { continue };
            let fields =
                json!({ "start": p.get("start"), "due": p.get("due"), "reminder": p.get("reminder") });
            let fields: serde_json::Map<String, Value> = fields
                .as_object()
                .map(|o| {
                    o.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), v.clone())).collect()
                })
                .unwrap_or_default();
            apply_task_fields(s, &mut t, &Value::Object(fields))?;
            out.push(*id);
            s.mailbox.tasks.insert(*id, t);
        }
        return Ok(json!({ "flagged": pim::ids_json(&out), "kind": "custom" }));
    }
    let fk = parse_flag_kind(kind).ok_or_else(|| {
        CmdError::Params("`kind` must be today|tomorrow|thisWeek|nextWeek|noDate|custom".into())
    })?;
    let (start, due) = crate::cmd::mail::flag_dates(s, fk);
    for id in &ids {
        if let Some(t) = s.mailbox.tasks.get_mut(id) {
            t.start = start;
            t.due = due;
        }
    }
    let tz = s.tz_offset_min;
    Ok(json!({
        "flagged": pim::ids_json(&ids),
        "kind": kind,
        "start": start.map(|d| pim::date_string(d, tz)),
        "due": due.map(|d| pim::date_string(d, tz)),
    }))
}

/// Private toggle (`on` sets it; without it, on unless every target is private already).
fn run_private(s: &mut Session, p: &Value) -> CmdResult {
    let ids = pim::require_targets(s, p, Kind::Task)?;
    let all = ids.iter().all(|i| s.mailbox.tasks.get(i).is_some_and(|t| t.private));
    let on = opt_bool(p, "on").unwrap_or(!all);
    for id in &ids {
        if let Some(t) = s.mailbox.tasks.get_mut(id) {
            t.private = on;
        }
    }
    Ok(json!({ "ids": pim::ids_json(&ids), "private": on }))
}

/// High / Low Importance (a task's priority): toggles back to Normal when every target already has it.
fn run_importance(s: &mut Session, p: &Value) -> CmdResult {
    let ids = pim::require_targets(s, p, Kind::Task)?;
    let level = match opt_str(p, "level").unwrap_or("high") {
        "high" => TaskPriority::High,
        "low" => TaskPriority::Low,
        "normal" => TaskPriority::Normal,
        other => return Err(CmdError::Params(format!("`level` must be high|normal|low, not {other}"))),
    };
    let all = ids.iter().all(|i| s.mailbox.tasks.get(i).is_some_and(|t| t.priority == level));
    let set = if all && p.get("on").is_none() { TaskPriority::Normal } else { level };
    for id in &ids {
        if let Some(t) = s.mailbox.tasks.get_mut(id) {
            t.priority = set;
        }
    }
    Ok(json!({ "ids": pim::ids_json(&ids), "priority": priority_name(set) }))
}

fn task_selected(s: &Session) -> Option<&'static str> {
    if pim::selected(s, Kind::Task).is_empty() { Some("select a task first") } else { None }
}

fn task_request_selected(s: &Session) -> Option<&'static str> {
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

fn respond(s: &mut Session, p: &Value, all: bool) -> CmdResult {
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
    let t = first_task(s, p)?;
    let text = opt_str(p, "as") != Some("attachment");
    let subject = mailcraft_model::subject::forward_subject(&t.subject);
    let cid = task_compose(s, &t, String::new(), subject, Some("PUBLISH"), true, text);
    Ok(json!({ "compose": cid, "task": t.id.0 }))
}

/// Meeting: a new meeting about the task (its subject and notes) with the requester and assignees.
fn run_meeting(s: &mut Session, p: &Value) -> CmdResult {
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
            r#"{"id"?: number, "ids"?: [number]}"#,
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
            task_selected,
            run_flag,
        ),
        spec(
            "tasks.private",
            "Private",
            "Tasks Home › Tags",
            "",
            r#"{"on"?: bool, "ids"?: [number]}"#,
            true,
            task_selected,
            run_private,
        ),
        spec(
            "tasks.importance",
            "Importance",
            "Tasks Home › Tags",
            "",
            r#"{"level": "high|low|normal", "on"?: bool, "ids"?: [number]} (toggles back to normal)"#,
            true,
            task_selected,
            run_importance,
        ),
        spec(
            "tasks.allCategories",
            "All Categories",
            "Tasks Home › Tags › Categorize",
            "",
            r#"{"ids"?: [number]}"#,
            false,
            task_selected,
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
            task_selected,
            run_forward,
        ),
        spec(
            "tasks.meeting",
            "Meeting",
            "Tasks Home › Respond",
            "",
            r#"{"ids"?: [number], ...appointment fields}"#,
            true,
            task_selected,
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
        assert_eq!(s.run("tasks.allCategories", &json!({})).unwrap()["dialog"], "pimCategories");
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
        // Meeting about the task.
        let r = s.run("tasks.meeting", &json!({"ids": [got]})).unwrap();
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
}
