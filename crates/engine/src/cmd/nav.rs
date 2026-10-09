//! Module switching (module rail, More Apps) and selection in the folder pane and message list.
//!
//! Selection follows the observed read marking: a selected unread message stays unread while selected and
//! is marked read when the selection moves to another message (`plan/outlook/reading-pane.md`).

use serde_json::{Value, json};

use crate::cmd::util::{dialog, opt_bool, opt_ids};
use crate::queries;
use crate::{CmdError, CmdResult, CommandSpec, Module, Selection, Session, always};
use mailcraft_model::{FolderId, FolderKind, FolderRole, ItemId, TaskStatus};

pub fn specs() -> Vec<CommandSpec> {
    let module = |id, label, location, shortcut, run| CommandSpec {
        id,
        label,
        location,
        shortcut,
        params: "",
        mutates: true,
        enabled: always,
        run,
    };
    vec![
        module("nav.mail", "Mail", "Module rail", "Mod+1", nav_mail as fn(&mut Session, &Value) -> CmdResult),
        module("nav.calendar", "Calendar", "Module rail", "Mod+2", nav_calendar),
        module("nav.people", "People", "Module rail", "Mod+3", nav_people),
        module("nav.tasks", "To Do", "Module rail", "Mod+4", nav_tasks),
        module("nav.notes", "Notes", "Module rail › More Apps", "", nav_notes),
        module("nav.folders", "More Apps / Folders", "Module rail › More Apps", "", nav_folders),
        module("nav.shortcuts", "Shortcuts", "Module rail › More Apps", "", nav_shortcuts),
        CommandSpec {
            id: "nav.selectFolder",
            label: "Select Folder",
            location: "Folder pane",
            shortcut: "",
            params: r#"{"folder": number}"#,
            mutates: true,
            enabled: always,
            run: select_folder,
        },
        CommandSpec {
            id: "nav.goToFolder",
            label: "Go to Folder",
            location: "Folder pane",
            shortcut: "Mod+Y",
            params: r#"{"folder"?: number}  (absent: opens the Go to Folder dialog)"#,
            mutates: true,
            enabled: always,
            run: go_to_folder,
        },
        CommandSpec {
            id: "nav.inbox",
            label: "Go to Inbox",
            location: "Keyboard",
            shortcut: "Mod+Shift+I",
            params: "",
            mutates: true,
            enabled: always,
            run: |s, _| go_to_role(s, FolderRole::Inbox),
        },
        CommandSpec {
            id: "nav.outbox",
            label: "Go to Outbox",
            location: "Keyboard",
            shortcut: "Mod+Shift+O",
            params: "",
            mutates: true,
            enabled: always,
            run: |s, _| go_to_role(s, FolderRole::Outbox),
        },
        CommandSpec {
            id: "nav.selectItem",
            label: "Select Item",
            location: "Message list",
            shortcut: "",
            params: r#"{"id": number} | {"ids": [number]}, "toggle"?: bool (Ctrl+click), "extend"?: bool (Shift+click)"#,
            mutates: true,
            enabled: always,
            run: select_item,
        },
        CommandSpec {
            id: "nav.selectAll",
            label: "Select All",
            location: "Message list",
            shortcut: "Mod+A",
            params: "",
            mutates: false,
            enabled: always,
            run: select_all,
        },
        CommandSpec {
            id: "nav.nextItem",
            label: "Next Item",
            location: "Message list",
            shortcut: "Down",
            params: r#"{"extend"?: bool}"#,
            mutates: true,
            enabled: always,
            run: |s, p| step(s, p, Step::Next),
        },
        CommandSpec {
            id: "nav.prevItem",
            label: "Previous Item",
            location: "Message list",
            shortcut: "Up",
            params: r#"{"extend"?: bool}"#,
            mutates: true,
            enabled: always,
            run: |s, p| step(s, p, Step::Prev),
        },
        CommandSpec {
            id: "nav.firstItem",
            label: "First Item",
            location: "Message list",
            shortcut: "Home",
            params: "",
            mutates: true,
            enabled: always,
            run: |s, p| step(s, p, Step::First),
        },
        CommandSpec {
            id: "nav.lastItem",
            label: "Last Item",
            location: "Message list",
            shortcut: "End",
            params: "",
            mutates: true,
            enabled: always,
            run: |s, p| step(s, p, Step::Last),
        },
    ]
}

fn module_name(m: Module) -> &'static str {
    match m {
        Module::Mail => "Mail",
        Module::Calendar => "Calendar",
        Module::People => "People",
        Module::Tasks => "Tasks",
        Module::Notes => "Notes",
        Module::Folders => "Folders",
        Module::Shortcuts => "Shortcuts",
    }
}

/// Folder kind a module lists (`None` = any).
fn module_kind(m: Module) -> Option<FolderKind> {
    match m {
        Module::Mail => Some(FolderKind::Mail),
        Module::Calendar => Some(FolderKind::Calendar),
        Module::People => Some(FolderKind::Contacts),
        Module::Tasks => Some(FolderKind::Tasks),
        Module::Notes => Some(FolderKind::Notes),
        Module::Folders | Module::Shortcuts => None,
    }
}

fn module_role(m: Module) -> Option<FolderRole> {
    match m {
        Module::Mail => Some(FolderRole::Inbox),
        Module::Calendar => Some(FolderRole::Calendar),
        Module::People => Some(FolderRole::Contacts),
        Module::Tasks => Some(FolderRole::Tasks),
        Module::Notes => Some(FolderRole::Notes),
        Module::Folders | Module::Shortcuts => None,
    }
}

/// Module that shows a folder of `kind`.
fn module_for_kind(kind: FolderKind) -> Module {
    match kind {
        FolderKind::Calendar => Module::Calendar,
        FolderKind::Contacts => Module::People,
        FolderKind::Tasks => Module::Tasks,
        FolderKind::Notes => Module::Notes,
        FolderKind::Mail | FolderKind::SearchRoot | FolderKind::Search | FolderKind::Journal => Module::Mail,
    }
}

/// Marks the focused message read when the selection leaves it for `next`.
fn leave_focus(s: &mut Session, next: Option<ItemId>) {
    if let Some(cur) = s.selection.focus
        && Some(cur) != next
    {
        s.mailbox.set_read(&[cur], true);
    }
}

fn remember_folder(s: &mut Session) {
    let (module, folder) = (s.view.module, s.selection.folder);
    s.module_folders.retain(|(m, _)| *m != module);
    s.module_folders.push((module, folder));
}

/// Shows `folder` in the list (clears the selection and any search).
fn show_folder(s: &mut Session, folder: FolderId) {
    leave_focus(s, None);
    if s.selection.folder != folder {
        s.view.search.clear();
    }
    s.selection = Selection { folder, items: Vec::new(), focus: None };
}

fn switch_module(s: &mut Session, module: Module) -> FolderId {
    remember_folder(s);
    s.view.module = module;
    let kind = module_kind(module);
    let fits =
        |s: &Session, f: FolderId| s.mailbox.folder(f).is_some_and(|f| kind.is_none_or(|k| f.kind == k));
    let remembered =
        s.module_folders.iter().find(|(m, _)| *m == module).map(|(_, f)| *f).filter(|f| fits(s, *f));
    let default = || {
        let role = module_role(module)?;
        let account = s.current_account();
        account
            .and_then(|a| s.mailbox.role_folder(a, role))
            .or_else(|| s.mailbox.folders.values().find(|f| f.role == Some(role)).map(|f| f.id))
    };
    let target = if fits(s, s.selection.folder) && kind.is_some() {
        // Already showing a folder of this module's kind (e.g. Mail after a mail folder was selected).
        remembered.or(Some(s.selection.folder))
    } else {
        remembered.or_else(default)
    };
    if let Some(f) = target
        && f != s.selection.folder
    {
        show_folder(s, f);
    }
    s.selection.folder
}

fn module_result(s: &Session) -> Value {
    json!({"module": module_name(s.view.module), "folder": s.selection.folder.0})
}

fn nav_mail(s: &mut Session, _: &Value) -> CmdResult {
    switch_module(s, Module::Mail);
    Ok(module_result(s))
}

fn nav_calendar(s: &mut Session, _: &Value) -> CmdResult {
    switch_module(s, Module::Calendar);
    Ok(module_result(s))
}

fn nav_people(s: &mut Session, _: &Value) -> CmdResult {
    switch_module(s, Module::People);
    Ok(module_result(s))
}

/// To Do: flagged mail and open tasks.
fn nav_tasks(s: &mut Session, _: &Value) -> CmdResult {
    switch_module(s, Module::Tasks);
    let flagged: Vec<u64> = s.mailbox.mail.values().filter(|m| m.is_flagged()).map(|m| m.id.0).collect();
    let tasks: Vec<u64> =
        s.mailbox.tasks.values().filter(|t| t.status != TaskStatus::Completed).map(|t| t.id.0).collect();
    Ok(json!({
        "module": module_name(s.view.module),
        "folder": s.selection.folder.0,
        "flagged": flagged,
        "tasks": tasks,
    }))
}

fn nav_notes(s: &mut Session, _: &Value) -> CmdResult {
    switch_module(s, Module::Notes);
    s.view.pinned_app = Some(Module::Notes);
    Ok(module_result(s))
}

fn nav_shortcuts(s: &mut Session, _: &Value) -> CmdResult {
    switch_module(s, Module::Shortcuts);
    s.view.pinned_app = Some(Module::Shortcuts);
    Ok(module_result(s))
}

/// Every folder of every type, in tree order: `{id, name, path, kind, depth}`.
fn all_folders(s: &Session) -> Vec<Value> {
    let mb = &s.mailbox;
    let mut out = Vec::new();
    for account in &mb.accounts {
        for root in mb.children(account.id, None) {
            for id in mb.subtree(root) {
                let Some(f) = mb.folder(id) else { continue };
                let path = mb.folder_path(id);
                let depth = path.matches('\\').count();
                out.push(json!({
                    "id": id.0,
                    "name": f.name,
                    "path": path,
                    "kind": format!("{:?}", f.kind),
                    "depth": depth,
                }));
            }
        }
    }
    out
}

fn nav_folders(s: &mut Session, _: &Value) -> CmdResult {
    switch_module(s, Module::Folders);
    s.view.pinned_app = Some(Module::Folders);
    Ok(json!({
        "module": module_name(s.view.module),
        "folder": s.selection.folder.0,
        "folders": all_folders(s),
    }))
}

fn select_folder_id(s: &mut Session, folder: FolderId) -> CmdResult {
    let kind = s
        .mailbox
        .folder(folder)
        .map(|f| f.kind)
        .ok_or_else(|| CmdError::Params(format!("no folder with id {}", folder.0)))?;
    let module = module_for_kind(kind);
    if s.view.module != module && s.view.module != Module::Folders {
        remember_folder(s);
        s.view.module = module;
    }
    show_folder(s, folder);
    let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
    Ok(json!({"folder": folder.0, "name": name, "module": module_name(s.view.module)}))
}

fn folder_param(p: &Value) -> Result<Option<FolderId>, CmdError> {
    match p.get("folder").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .map(|n| Some(FolderId(n)))
            .ok_or_else(|| CmdError::Params("`folder` must be a folder id (non-negative integer)".into())),
    }
}

fn select_folder(s: &mut Session, p: &Value) -> CmdResult {
    let folder = folder_param(p)?.ok_or_else(|| CmdError::Params("`folder` (folder id) required".into()))?;
    select_folder_id(s, folder)
}

fn go_to_folder(s: &mut Session, p: &Value) -> CmdResult {
    match folder_param(p)? {
        Some(folder) => select_folder_id(s, folder),
        None => {
            let folders = all_folders(s);
            dialog(s, "goToFolder", json!({"folders": folders, "current": s.selection.folder.0}))
        }
    }
}

fn go_to_role(s: &mut Session, role: FolderRole) -> CmdResult {
    let account = s.current_account();
    let folder = account
        .and_then(|a| s.mailbox.role_folder(a, role))
        .or_else(|| s.mailbox.folders.values().find(|f| f.role == Some(role)).map(|f| f.id))
        .ok_or_else(|| CmdError::Failed(format!("there is no {} folder", role.default_name())))?;
    select_folder_id(s, folder)
}

fn selection_result(s: &Session) -> Value {
    let items: Vec<u64> = s.selection.items.iter().map(|i| i.0).collect();
    json!({"folder": s.selection.folder.0, "items": items, "focus": s.selection.focus.map(|f| f.0)})
}

fn select_item(s: &mut Session, p: &Value) -> CmdResult {
    let mut ids: Vec<ItemId> =
        match p.get("id").filter(|v| !v.is_null()) {
            Some(v) => vec![ItemId(v.as_u64().ok_or_else(|| {
                CmdError::Params("`id` must be a message id (non-negative integer)".into())
            })?)],
            None => opt_ids(p).ok_or_else(|| CmdError::Params("`id` or `ids` required".into()))?,
        };
    let mut seen = std::collections::BTreeSet::new();
    ids.retain(|id| seen.insert(*id));
    if ids.is_empty() {
        return Err(CmdError::Params("no message ids given".into()));
    }
    if let Some(missing) = ids.iter().find(|id| !s.mailbox.mail.contains_key(id)) {
        return Err(CmdError::Params(format!("no message with id {}", missing.0)));
    }
    let toggle = opt_bool(p, "toggle").unwrap_or(false);
    let extend = opt_bool(p, "extend").unwrap_or(false);
    let focus = ids.last().copied();
    // Selecting a message from another folder (outside a search) shows that folder.
    if s.view.search.trim().is_empty()
        && let Some(folder) = focus.and_then(|f| s.mailbox.mail.get(&f)).map(|m| m.folder)
        && folder != s.selection.folder
    {
        select_folder_id(s, folder)?;
    }
    leave_focus(s, focus);
    if toggle {
        for id in &ids {
            if let Some(pos) = s.selection.items.iter().position(|x| x == id) {
                s.selection.items.remove(pos);
            } else {
                s.selection.items.push(*id);
            }
        }
    } else if extend && let (Some(anchor), Some(target)) = (s.selection.focus, focus) {
        let order = queries::visible_item_ids(s);
        let a = order.iter().position(|x| *x == anchor);
        let b = order.iter().position(|x| *x == target);
        s.selection.items = match (a, b) {
            (Some(a), Some(b)) => order.get(a.min(b)..=a.max(b)).map(<[ItemId]>::to_vec).unwrap_or_default(),
            _ => ids.clone(),
        };
    } else {
        s.selection.items = ids;
    }
    s.selection.focus = focus;
    Ok(selection_result(s))
}

fn select_all(s: &mut Session, _: &Value) -> CmdResult {
    let ids = queries::view_item_ids(s);
    s.selection.focus = s.selection.focus.filter(|f| ids.contains(f)).or_else(|| ids.first().copied());
    s.selection.items = ids;
    Ok(selection_result(s))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Next,
    Prev,
    First,
    Last,
}

fn step(s: &mut Session, p: &Value, dir: Step) -> CmdResult {
    let order = queries::visible_item_ids(s);
    if order.is_empty() {
        return Ok(selection_result(s));
    }
    let cur = s.selection.focus.and_then(|f| order.iter().position(|x| *x == f));
    let last = order.len().saturating_sub(1);
    let idx = match (dir, cur) {
        (Step::First, _) | (Step::Next | Step::Prev, None) => 0,
        (Step::Last, _) => last,
        (Step::Next, Some(i)) => i.saturating_add(1).min(last),
        (Step::Prev, Some(i)) => i.saturating_sub(1),
    };
    let Some(target) = order.get(idx).copied() else {
        return Ok(selection_result(s));
    };
    let extend = opt_bool(p, "extend").unwrap_or(false);
    leave_focus(s, Some(target));
    if extend {
        if !s.selection.items.contains(&target) {
            s.selection.items.push(target);
        }
    } else {
        s.selection.items = vec![target];
    }
    s.selection.focus = Some(target);
    Ok(selection_result(s))
}

#[cfg(test)]
mod tests {
    use crate::Module;
    use crate::sample::sample_session;
    use mailcraft_model::FolderRole;
    use serde_json::json;

    #[test]
    fn module_switching_remembers_folders() {
        let mut s = sample_session();
        let inbox = s.selection.folder;
        let r = s.run("nav.calendar", &json!({})).unwrap();
        assert_eq!(r["module"], "Calendar");
        assert_eq!(s.view.module, Module::Calendar);
        assert_eq!(s.mailbox.folder(s.selection.folder).map(|f| f.role), Some(Some(FolderRole::Calendar)));
        s.run("nav.people", &json!({})).unwrap();
        assert_eq!(s.mailbox.folder(s.selection.folder).map(|f| f.role), Some(Some(FolderRole::Contacts)));
        s.run("nav.mail", &json!({})).unwrap();
        assert_eq!(s.selection.folder, inbox);
        let r = s.run("nav.tasks", &json!({})).unwrap();
        assert!(!r["flagged"].as_array().unwrap().is_empty());
        assert!(!r["tasks"].as_array().unwrap().is_empty());
        let r = s.run("nav.folders", &json!({})).unwrap();
        assert_eq!(s.view.pinned_app, Some(Module::Folders));
        assert!(r["folders"].as_array().unwrap().len() >= 15);
        // The Folders view lists every kind of folder.
        s.view.module = Module::Folders;
        let rows = crate::queries::folder_rows(&s);
        assert!(rows.iter().any(|r| r.name == "Calendar"));
    }

    #[test]
    fn selection_marks_previous_read() {
        let mut s = sample_session();
        let order = crate::queries::visible_item_ids(&s);
        let (a, b) = (order[0], order[1]);
        s.mailbox.set_read(&[a, b], false);
        s.run("nav.selectItem", &json!({"id": a.0})).unwrap();
        assert!(!s.mailbox.mail[&a].read, "still unread while selected");
        s.run("nav.nextItem", &json!({})).unwrap();
        assert_eq!(s.selection.focus, Some(b));
        assert!(s.mailbox.mail[&a].read, "marked read when the selection moved");
        s.run("nav.selectItem", &json!({"id": a.0, "extend": true})).unwrap();
        assert_eq!(s.selection.items.len(), 2);
        s.run("nav.selectAll", &json!({})).unwrap();
        assert_eq!(s.selection.items.len(), crate::queries::view_item_ids(&s).len());
        assert!(s.run("nav.selectItem", &json!({"id": 999_999})).is_err());
        assert!(s.run("nav.selectItem", &json!({})).is_err());
    }

    #[test]
    fn go_to_folder_and_roles() {
        let mut s = sample_session();
        let r = s.run("nav.goToFolder", &json!({})).unwrap();
        assert_eq!(r["dialog"], "goToFolder");
        s.run("nav.outbox", &json!({})).unwrap();
        assert_eq!(s.mailbox.folder(s.selection.folder).and_then(|f| f.role), Some(FolderRole::Outbox));
        s.run("nav.inbox", &json!({})).unwrap();
        assert_eq!(s.mailbox.folder(s.selection.folder).and_then(|f| f.role), Some(FolderRole::Inbox));
        assert!(s.run("nav.selectFolder", &json!({"folder": -1})).is_err());
    }
}
