//! Tasks commands (`tasks.*`) and the To-Do list.
//!
//! Task items live in tasks folders (`Mailbox.tasks`). The To-Do list is the open tasks plus flagged mail.
//! **Flag-to-task linkage:** `tasks.fromMail` creates a task linked to a flagged message (`Task.from_mail`);
//! completing either side completes the other, and `tasks.removeFromList` clears the message's flag and
//! drops the linked task. Status, percent complete and the completed date are kept consistent: 100 % ⇔
//! Completed ⇔ a completion date.

use serde_json::{Value, json};

use crate::cmd::calendar::{pim, pim::Kind};
use crate::cmd::util::opt_str;
use crate::{CmdError, CmdResult, CommandSpec, Session, always};
use mailcraft_model::{DateTime, FlagState, FolderId, FolderRole, ItemId, Task, TaskPriority, TaskStatus};

const TASK_FIELDS: [&str; 10] = [
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
];

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
    Ok(())
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
    })
}

/// Creates a task in `folder` from field params; returns its id.
pub fn create_task(s: &mut Session, folder: FolderId, p: &Value) -> Result<ItemId, CmdError> {
    let mut t = Task { folder, ..Default::default() };
    apply_task_fields(s, &mut t, p)?;
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
        apply_task_fields(s, &mut t, p)?;
        updated.push(t);
    }
    let tz = s.tz_offset_min;
    let out: Vec<Value> = updated.iter().map(|t| task_json(t, tz)).collect();
    for t in updated {
        let id = t.id;
        s.mailbox.tasks.insert(id, t);
        sync_mail_flag(s, id);
    }
    Ok(json!({ "updated": out }))
}

fn run_delete(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_delete(s, p, Kind::Task)
}
fn run_copy(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_copy(s, p, Kind::Task)
}
fn run_move(s: &mut Session, p: &Value) -> CmdResult {
    pim::run_move(s, p, Kind::Task)
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
        .filter(|i| s.mailbox.mail.get(i).is_some_and(|m| !matches!(m.flag, FlagState::None)))
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
    for id in &all_tasks {
        if let Some(t) = s.mailbox.tasks.get_mut(id) {
            set_complete(t, true, now);
        }
        sync_mail_flag(s, *id);
    }
    Ok(json!({ "tasks": pim::ids_json(&all_tasks), "mail": pim::ids_json(&mail) }))
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

/// The To-Do list: open tasks (outside Deleted Items) and flagged messages without a linked task, ordered
/// by due date (no date last), then subject.
pub fn todo_list(s: &Session) -> Vec<Value> {
    let tz = s.tz_offset_min;
    let in_deleted = |f: FolderId| {
        s.mailbox
            .folder_account(f)
            .and_then(|a| s.mailbox.role_folder(a, FolderRole::DeletedItems))
            .is_some_and(|d| s.mailbox.is_in_subtree(f, d))
    };
    let mut rows: Vec<(i64, String, Value)> = Vec::new();
    for t in s.mailbox.tasks.values() {
        if t.status == TaskStatus::Completed || in_deleted(t.folder) {
            continue;
        }
        let mut v = task_json(t, tz);
        if let Some(o) = v.as_object_mut() {
            o.insert("kind".into(), json!("task"));
        }
        rows.push((t.due.map_or(i64::MAX, |d| d.unix), t.subject.to_lowercase(), v));
    }
    for m in s.mailbox.mail.values() {
        let FlagState::Flagged { due, .. } = &m.flag else { continue };
        if in_deleted(m.folder) || s.mailbox.tasks.values().any(|t| t.from_mail == Some(m.id)) {
            continue;
        }
        let v = json!({
            "kind": "mail",
            "id": m.id.0,
            "folder": m.folder.0,
            "subject": m.subject,
            "due": due.map(|d| pim::date_string(d, tz)),
        });
        rows.push((due.map_or(i64::MAX, |d| d.unix), m.subject.to_lowercase(), v));
    }
    rows.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    rows.into_iter().map(|(_, _, v)| v).collect()
}

fn run_todo_list(s: &mut Session, _p: &Value) -> CmdResult {
    Ok(json!({ "items": todo_list(s) }))
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
}
