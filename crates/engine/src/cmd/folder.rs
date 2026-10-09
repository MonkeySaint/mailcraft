//! Folder commands (`folder.*`): create, rename, copy, move and delete folders; favourites and manual order;
//! clean-up (Mark All as Read, Delete All, empty Deleted Items, Recover Deleted Items); search folders; and
//! Run Rules Now, whose executor [`apply_rules`] is shared with the rest of the engine.
//!
//! Folder parameters (`folder`, `parent`) accept a folder id (number or numeric string), a folder path as
//! shown in the Move dialog (`Inbox\Projects`, optionally prefixed by the account name) or a folder name.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::util::{opt_bool, opt_str};
use crate::{CmdError, CmdResult, CommandSpec, Notice, Selection, Session, always};
use mailcraft_model::rule::RuleContext;
use mailcraft_model::{
    DateTime, FlagState, Folder, FolderId, FolderKind, FolderRole, ItemId, MailItem, Mailbox, Rule,
    RuleAction,
};

const DAY_SECS: i64 = 86_400;

#[allow(clippy::too_many_arguments)]
fn spec(
    id: &'static str,
    label: &'static str,
    location: &'static str,
    shortcut: &'static str,
    params: &'static str,
    mutates: bool,
    enabled: fn(&Session) -> Option<&'static str>,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec { id, label, location, shortcut, params, mutates, enabled, run }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "folder.addFavorite",
            "Add to Favorites",
            "Folder › Favorites",
            "",
            r#"{"folder"?:string}"#,
            true,
            has_folder,
            add_favorite,
        ),
        spec(
            "folder.autoArchiveSettings",
            "AutoArchive Settings",
            "Folder › Properties",
            "",
            "",
            false,
            has_folder,
            auto_archive_settings,
        ),
        spec(
            "folder.collapse",
            "Collapse",
            "Folder context menu",
            "",
            r#"{"folder"?:string}"#,
            true,
            always,
            collapse,
        ),
        spec(
            "folder.copy",
            "Copy Folder",
            "Folder › Actions",
            "",
            r#"{"parent"?:string}"#,
            true,
            copyable,
            copy,
        ),
        spec("folder.delete", "Delete Folder", "Folder › Actions", "", "", true, user_folder, delete),
        spec(
            "folder.deleteAll",
            "Delete All",
            "Folder › Clean Up",
            "",
            "",
            true,
            folder_has_items,
            delete_all,
        ),
        spec(
            "folder.emptyDeleted",
            "Empty Folder",
            "Folder context menu (Deleted Items)",
            "",
            "",
            true,
            deleted_has_content,
            empty_deleted,
        ),
        spec(
            "folder.expand",
            "Expand",
            "Folder context menu",
            "",
            r#"{"folder"?:string}"#,
            true,
            always,
            expand,
        ),
        spec(
            "folder.markAllRead",
            "Mark All as Read",
            "Folder › Clean Up",
            "",
            "",
            true,
            has_folder,
            mark_all_read,
        ),
        spec(
            "folder.move",
            "Move Folder",
            "Folder › Actions",
            "",
            r#"{"parent"?:string}"#,
            true,
            user_folder,
            move_,
        ),
        spec(
            "folder.moveDown",
            "Move Down",
            "Folder context menu (Favorites)",
            "",
            r#"{"folder":string,"in"?:"favorites|tree"}"#,
            true,
            always,
            move_down,
        ),
        spec(
            "folder.moveUp",
            "Move Up",
            "Folder context menu (Favorites)",
            "",
            r#"{"folder":string,"in"?:"favorites|tree"}"#,
            true,
            always,
            move_up,
        ),
        spec(
            "folder.new",
            "New Folder",
            "Folder › New",
            "Mod+Shift+E",
            r#"{"name"?:string,"parent"?:string}"#,
            true,
            always,
            new_folder,
        ),
        spec(
            "folder.newSearchFolder",
            "New Search Folder",
            "Folder › New",
            "Mod+Shift+P",
            r#"{"name"?:string,"query"?:string}"#,
            true,
            always,
            new_search_folder,
        ),
        spec(
            "folder.openInNewWindow",
            "Open in New Window",
            "View › Window",
            "",
            "",
            false,
            has_folder,
            open_in_new_window,
        ),
        spec(
            "folder.properties",
            "Folder Properties",
            "Folder › Properties",
            "",
            "",
            false,
            has_folder,
            properties,
        ),
        spec(
            "folder.recoverDeleted",
            "Recover Deleted Items",
            "Folder › Clean Up",
            "",
            r#"{"ids"?:[number]}"#,
            true,
            always,
            recover_deleted,
        ),
        spec(
            "folder.removeFavorite",
            "Remove from Favorites",
            "Folder context menu (Favorites)",
            "",
            r#"{"folder":string}"#,
            true,
            always,
            remove_favorite,
        ),
        spec(
            "folder.rename",
            "Rename Folder",
            "Folder › Actions",
            "",
            r#"{"name"?:string}"#,
            true,
            user_folder,
            rename,
        ),
        spec(
            "folder.runRulesNow",
            "Run Rules Now",
            "Folder › Clean Up",
            "",
            r#"{"rules"?:[number|string],"all"?:bool,"folder"?:string,"subfolders"?:bool,"which"?:"all|read|unread"}"#,
            true,
            always,
            run_rules_now,
        ),
        spec(
            "folder.showAllAZ",
            "Show All Folders A to Z",
            "Folder › Clean Up",
            "",
            r#"{"on"?:bool}"#,
            false,
            always,
            show_all_az,
        ),
        spec(
            "folder.sortAZ",
            "Sort Subfolders A to Z",
            "Folder/account context menu",
            "",
            r#"{"folder":string}"#,
            true,
            always,
            sort_az,
        ),
    ]
}

// ---------------------------------------------------------------------------------------------------------
// Shared helpers (also used by view.rs and search.rs)

/// Queues `Notice::OpenDialog(name, params)` and returns `{"dialog": name}`.
pub fn dialog(s: &mut Session, name: &str, params: Value) -> Value {
    s.notify(Notice::OpenDialog(name.to_owned(), params));
    json!({ "dialog": name })
}

/// Resolves a folder reference (see the module docs). Prefers folders of the current account.
pub fn resolve_folder(s: &Session, v: &Value) -> Option<FolderId> {
    if let Some(n) = v.as_u64() {
        let id = FolderId(n);
        return s.mailbox.folders.contains_key(&id).then_some(id);
    }
    let text = v.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(n) = text.parse::<u64>()
        && s.mailbox.folders.contains_key(&FolderId(n))
    {
        return Some(FolderId(n));
    }
    let want = text.trim_matches('\\').to_lowercase();
    let account = s.current_account();
    let pick = |cands: Vec<&Folder>| -> Option<FolderId> {
        cands.iter().find(|f| Some(f.account) == account).or_else(|| cands.first()).map(|f| f.id)
    };
    let mb = &s.mailbox;
    let by_path: Vec<&Folder> =
        mb.folders.values().filter(|f| mb.folder_path(f.id).to_lowercase() == want).collect();
    if let Some(id) = pick(by_path) {
        return Some(id);
    }
    let by_account_path: Vec<&Folder> = mb
        .folders
        .values()
        .filter(|f| {
            mb.account(f.account).is_some_and(|a| {
                [&a.display_name, &a.address].iter().any(|root| {
                    !root.is_empty() && format!("{}\\{}", root, mb.folder_path(f.id)).to_lowercase() == want
                })
            })
        })
        .collect();
    if let Some(id) = pick(by_account_path) {
        return Some(id);
    }
    pick(mb.folders.values().filter(|f| f.name.to_lowercase() == want).collect())
}

/// Optional folder parameter `key`: `Ok(None)` when absent or null, an error when it names no folder.
pub fn param_folder(s: &Session, p: &Value, key: &str) -> Result<Option<FolderId>, CmdError> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            resolve_folder(s, v).map(Some).ok_or_else(|| CmdError::Params(format!("no such folder: {v}")))
        }
    }
}

/// `params.folder` when given, else the selected folder; it must exist.
pub fn current_folder(s: &Session, p: &Value) -> Result<FolderId, CmdError> {
    let id = param_folder(s, p, "folder")?.unwrap_or(s.selection.folder);
    if s.mailbox.folders.contains_key(&id) {
        Ok(id)
    } else {
        Err(CmdError::Disabled("no folder is selected".into()))
    }
}

/// Drops selected items that no longer exist or have left the selected folder.
pub fn prune_selection(s: &mut Session) {
    let folder = s.selection.folder;
    let mb = &s.mailbox;
    let keep = |id: &ItemId| mb.mail.get(id).is_some_and(|m| m.folder == folder);
    s.selection.items.retain(keep);
    if s.selection.focus.is_some_and(|f| !keep(&f)) {
        s.selection.focus = s.selection.items.first().copied();
    }
}

/// Selects `folder` (clearing the item selection), or the default folder when it does not exist.
fn select_folder_or_default(s: &mut Session, folder: Option<FolderId>) {
    match folder.filter(|f| s.mailbox.folders.contains_key(f)) {
        Some(f) => s.selection = Selection { folder: f, items: Vec::new(), focus: None },
        None => s.select_default_folder(),
    }
}

/// True when any folder or item uses `id`.
fn id_taken(mb: &Mailbox, id: u64) -> bool {
    mb.folders.contains_key(&FolderId(id))
        || mb.mail.contains_key(&ItemId(id))
        || mb.appointments.contains_key(&ItemId(id))
        || mb.contacts.contains_key(&ItemId(id))
        || mb.tasks.contains_key(&ItemId(id))
        || mb.notes.contains_key(&ItemId(id))
}

/// Appointments, contacts, tasks and notes stored directly in `folder`.
fn pim_items_in(mb: &Mailbox, folder: FolderId) -> Vec<ItemId> {
    let in_folder = |f: FolderId| f == folder;
    mb.appointments
        .values()
        .filter(|a| in_folder(a.folder))
        .map(|a| a.id)
        .chain(mb.contacts.values().filter(|c| in_folder(c.folder)).map(|c| c.id))
        .chain(mb.tasks.values().filter(|t| in_folder(t.folder)).map(|t| t.id))
        .chain(mb.notes.values().filter(|n| in_folder(n.folder)).map(|n| n.id))
        .collect()
}

/// Items of every kind (mail, appointments, contacts, tasks, notes) stored directly in `folder`.
fn item_count(mb: &Mailbox, folder: FolderId) -> usize {
    mb.total_count(folder).saturating_add(pim_items_in(mb, folder).len())
}

/// `base`, or `base (2)`, `base (3)`… so no other child of `parent` has it (case-insensitive).
fn unique_child_name(mb: &Mailbox, parent: FolderId, base: &str) -> String {
    let Some(account) = mb.folder_account(parent) else {
        return base.to_owned();
    };
    let taken = |name: &str| {
        let lower = name.to_lowercase();
        mb.folders
            .values()
            .any(|f| f.account == account && f.parent == Some(parent) && f.name.to_lowercase() == lower)
    };
    if !taken(base) {
        return base.to_owned();
    }
    let max = mailcraft_model::mailbox::MAX_FOLDER_NAME_CHARS;
    let mut n: u32 = 2;
    loop {
        let suffix = format!(" ({n})");
        let keep = max.saturating_sub(suffix.chars().count());
        let stem: String = base.chars().take(keep).collect();
        let candidate = format!("{}{suffix}", stem.trim_end());
        if !taken(&candidate) || n == u32::MAX {
            return candidate;
        }
        n = n.saturating_add(1);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Enabled predicates

fn selected(s: &Session) -> Option<&Folder> {
    s.mailbox.folders.get(&s.selection.folder)
}

fn has_folder(s: &Session) -> Option<&'static str> {
    if selected(s).is_some() { None } else { Some("no folder is selected") }
}

fn user_folder(s: &Session) -> Option<&'static str> {
    match selected(s) {
        None => Some("no folder is selected"),
        Some(f) if f.is_default() => Some("default folders cannot be renamed, moved or deleted"),
        Some(_) => None,
    }
}

fn copyable(s: &Session) -> Option<&'static str> {
    match selected(s) {
        None => Some("no folder is selected"),
        Some(f) if f.kind == FolderKind::SearchRoot => Some("this folder cannot be copied"),
        Some(_) => None,
    }
}

fn folder_has_items(s: &Session) -> Option<&'static str> {
    if let Some(why) = has_folder(s) {
        return Some(why);
    }
    if item_count(&s.mailbox, s.selection.folder) == 0 { Some("the folder is empty") } else { None }
}

fn deleted_items_folder(s: &Session) -> Option<FolderId> {
    s.current_account().and_then(|a| s.mailbox.role_folder(a, FolderRole::DeletedItems))
}

fn deleted_has_content(s: &Session) -> Option<&'static str> {
    let Some(d) = deleted_items_folder(s) else {
        return Some("there is no Deleted Items folder");
    };
    let has_sub = s.mailbox.folders.values().any(|f| f.parent == Some(d));
    if has_sub || item_count(&s.mailbox, d) > 0 { None } else { Some("Deleted Items is empty") }
}

// ---------------------------------------------------------------------------------------------------------
// Create / rename / copy / move / delete

fn new_folder(s: &mut Session, p: &Value) -> CmdResult {
    let parent = match param_folder(s, p, "parent")? {
        Some(f) => f,
        None => current_folder(s, &Value::Null)?,
    };
    let Some(name) = opt_str(p, "name") else {
        let path = s.mailbox.folder_path(parent);
        return Ok(dialog(s, "createFolder", json!({ "parent": parent.0, "path": path })));
    };
    let id = s.mailbox.create_folder(parent, name)?;
    if let Some(f) = s.mailbox.folders.get_mut(&parent) {
        f.expanded = true;
    }
    Ok(json!({ "folder": id.0, "path": s.mailbox.folder_path(id) }))
}

fn rename(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let Some(name) = opt_str(p, "name") else {
        let current = s.mailbox.folder(id).map(|f| f.name.clone()).unwrap_or_default();
        return Ok(dialog(s, "renameFolder", json!({ "folder": id.0, "name": current })));
    };
    s.mailbox.rename_folder(id, name)?;
    let name = s.mailbox.folder(id).map(|f| f.name.clone()).unwrap_or_default();
    Ok(json!({ "folder": id.0, "name": name }))
}

/// Deep-copies `src` and everything below it (folders, items, view settings) under `parent`, with new ids.
/// Returns (new root, folders copied, items copied).
pub fn copy_folder_tree(
    mb: &mut Mailbox,
    src: FolderId,
    parent: FolderId,
) -> Result<(FolderId, usize, usize), CmdError> {
    let f = mb.folder(src).ok_or_else(|| CmdError::Failed(format!("no such folder {}", src.0)))?;
    let p = mb.folder(parent).ok_or_else(|| CmdError::Failed(format!("no such folder {}", parent.0)))?;
    if f.kind == FolderKind::SearchRoot {
        return Err(CmdError::Failed("this folder cannot be copied".into()));
    }
    if p.kind == FolderKind::Search {
        return Err(CmdError::Failed("search folders cannot contain folders".into()));
    }
    if (f.kind == FolderKind::Search) != (p.kind == FolderKind::SearchRoot) {
        return Err(CmdError::Failed("search folders stay under Search Folders".into()));
    }
    if mb.is_in_subtree(parent, src) {
        return Err(CmdError::Failed("a folder cannot be copied into itself or its subfolders".into()));
    }
    let account = p.account;
    let root_name = unique_child_name(mb, parent, &f.name);
    let tree = mb.subtree(src);
    let mut map: BTreeMap<FolderId, FolderId> = BTreeMap::new();
    for old in &tree {
        let Some(of) = mb.folder(*old).cloned() else { continue };
        let new_parent =
            if *old == src { Some(parent) } else { of.parent.and_then(|op| map.get(&op).copied()) };
        let Some(new_parent) = new_parent else { continue };
        let id = FolderId(mb.alloc_id());
        let is_root = *old == src;
        let kind = if of.kind == FolderKind::SearchRoot { FolderKind::Mail } else { of.kind };
        mb.folders.insert(
            id,
            Folder {
                id,
                account,
                parent: Some(new_parent),
                name: if is_root { root_name.clone() } else { of.name.clone() },
                role: None,
                kind,
                expanded: of.expanded,
                favorite_pos: None,
                manual_pos: if is_root { None } else { of.manual_pos },
                search_query: of.search_query.clone(),
                remote_name: None,
            },
        );
        if let Some(v) = mb.views.get(old).cloned() {
            mb.views.insert(id, v);
        }
        map.insert(*old, id);
    }
    let mut items = 0usize;
    let mail: Vec<MailItem> = mb.mail.values().filter(|m| map.contains_key(&m.folder)).cloned().collect();
    for mut m in mail {
        let Some(to) = map.get(&m.folder).copied() else { continue };
        m.id = ItemId(mb.alloc_id());
        m.folder = to;
        m.remote_id = None;
        mb.mail.insert(m.id, m);
        items = items.saturating_add(1);
    }
    macro_rules! copy_items {
        ($field:ident) => {{
            let list: Vec<_> = mb.$field.values().filter(|x| map.contains_key(&x.folder)).cloned().collect();
            for mut x in list {
                let Some(to) = map.get(&x.folder).copied() else { continue };
                x.id = ItemId(mb.alloc_id());
                x.folder = to;
                mb.$field.insert(x.id, x);
                items = items.saturating_add(1);
            }
        }};
    }
    copy_items!(appointments);
    copy_items!(contacts);
    copy_items!(tasks);
    copy_items!(notes);
    let root = map.get(&src).copied().ok_or_else(|| CmdError::Failed("copy failed".into()))?;
    Ok((root, map.len(), items))
}

fn copy(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let Some(parent) = param_folder(s, p, "parent")? else {
        return Ok(dialog(s, "copyFolder", json!({ "folder": id.0, "path": s.mailbox.folder_path(id) })));
    };
    let (root, folders, items) = copy_folder_tree(&mut s.mailbox, id, parent)?;
    if let Some(f) = s.mailbox.folders.get_mut(&parent) {
        f.expanded = true;
    }
    Ok(json!({ "folder": root.0, "path": s.mailbox.folder_path(root), "folders": folders, "items": items }))
}

fn move_(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let Some(parent) = param_folder(s, p, "parent")? else {
        return Ok(dialog(s, "moveFolder", json!({ "folder": id.0, "path": s.mailbox.folder_path(id) })));
    };
    s.mailbox.move_folder(id, parent)?;
    if let Some(f) = s.mailbox.folders.get_mut(&parent) {
        f.expanded = true;
    }
    Ok(json!({ "folder": id.0, "path": s.mailbox.folder_path(id) }))
}

fn delete(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let parent = s.mailbox.folder(id).and_then(|f| f.parent);
    s.mailbox.delete_folder(id)?;
    let permanent = !s.mailbox.folders.contains_key(&id);
    if !s.mailbox.folders.contains_key(&s.selection.folder) {
        select_folder_or_default(s, parent);
    }
    prune_selection(s);
    Ok(json!({ "folder": id.0, "permanent": permanent }))
}

// ---------------------------------------------------------------------------------------------------------
// Clean up

fn delete_all(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let ids: Vec<ItemId> = s.mailbox.items_in(id).iter().map(|m| m.id).collect();
    let removed = s.mailbox.delete_items(&ids)?;
    // Calendar, contacts, tasks and notes folders hold other kinds of items; Delete All covers them too.
    let pim = pim_items_in(&s.mailbox, id);
    let removed_pim = super::calendar::pim::delete(s, &pim);
    prune_selection(s);
    Ok(json!({ "deleted": removed.len().saturating_add(removed_pim.len()) }))
}

fn mark_all_read(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let ids: Vec<ItemId> = s.mailbox.items_in(id).iter().map(|m| m.id).collect();
    let changed = s.mailbox.set_read(&ids, true);
    Ok(json!({ "changed": changed.len() }))
}

fn empty_deleted(s: &mut Session, _p: &Value) -> CmdResult {
    let d = deleted_items_folder(s)
        .ok_or_else(|| CmdError::Disabled("there is no Deleted Items folder".into()))?;
    let below: BTreeSet<FolderId> = s.mailbox.subtree(d).into_iter().collect();
    let items = below.iter().fold(0usize, |n, f| n.saturating_add(item_count(&s.mailbox, *f)));
    let direct: Vec<ItemId> = s.mailbox.items_in(d).iter().map(|m| m.id).collect();
    s.mailbox.delete_items(&direct)?;
    // Deleted contacts, tasks, notes and appointments land in Deleted Items too.
    let direct_pim = pim_items_in(&s.mailbox, d);
    super::calendar::pim::delete(s, &direct_pim);
    let subs: Vec<FolderId> =
        s.mailbox.folders.values().filter(|f| f.parent == Some(d)).map(|f| f.id).collect();
    for f in &subs {
        s.mailbox.delete_folder(*f)?;
    }
    if !s.mailbox.folders.contains_key(&s.selection.folder) {
        select_folder_or_default(s, Some(d));
    }
    prune_selection(s);
    Ok(json!({ "items": items, "folders": subs.len() }))
}

/// Items that are gone from the mailbox but still held by an undo snapshot (newest snapshot first).
fn recoverable(s: &Session) -> Vec<MailItem> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for entry in s.undo.undo.iter().rev() {
        for m in entry.before.mail.values() {
            if !s.mailbox.mail.contains_key(&m.id) && seen.insert(m.id) {
                out.push(m.clone());
            }
        }
    }
    out
}

fn recover_deleted(s: &mut Session, p: &Value) -> CmdResult {
    let items = recoverable(s);
    let Some(ids) = super::util::opt_ids(p) else {
        let list: Vec<Value> = items
            .iter()
            .map(|m| {
                json!({
                    "id": m.id.0,
                    "subject": m.subject,
                    "from": m.from.as_ref().map(|f| f.addr.clone()).unwrap_or_default(),
                    "received": m.received.unix,
                    "folder": s.mailbox.folder_path(m.folder),
                })
            })
            .collect();
        let count = list.len();
        let mut v = dialog(s, "recoverDeletedItems", json!({ "items": list }));
        if let Some(o) = v.as_object_mut() {
            o.insert("count".into(), json!(count));
        }
        return Ok(v);
    };
    let fallback = deleted_items_folder(s);
    let mut restored = Vec::new();
    for id in ids {
        let Some(mut m) = items.iter().find(|m| m.id == id).cloned() else { continue };
        let account = s.mailbox.folder_account(m.folder);
        let target = account
            .and_then(|a| s.mailbox.role_folder(a, FolderRole::DeletedItems))
            .or_else(|| s.mailbox.folders.contains_key(&m.folder).then_some(m.folder))
            .or(fallback);
        let Some(target) = target else { continue };
        if id_taken(&s.mailbox, m.id.0) {
            m.id = ItemId(s.mailbox.alloc_id());
        }
        s.mailbox.next_id = s.mailbox.next_id.max(m.id.0);
        m.folder = target;
        restored.push(m.id.0);
        s.mailbox.mail.insert(m.id, m);
    }
    Ok(json!({ "restored": restored.len(), "ids": restored }))
}

// ---------------------------------------------------------------------------------------------------------
// Favourites, order, expansion

fn add_favorite(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let is_fav = s.mailbox.folder(id).is_some_and(|f| f.favorite_pos.is_some());
    if is_fav {
        s.mailbox.remove_favorite(id)?;
    } else {
        s.mailbox.add_favorite(id)?;
    }
    Ok(json!({ "folder": id.0, "favorite": !is_fav }))
}

fn remove_favorite(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let was = s.mailbox.folder(id).is_some_and(|f| f.favorite_pos.is_some());
    s.mailbox.remove_favorite(id)?;
    Ok(json!({ "folder": id.0, "favorite": false, "changed": was }))
}

/// Moves `id` one place up (`up`) or down within Favorites or among its user-folder siblings.
fn shift(s: &mut Session, p: &Value, up: bool) -> CmdResult {
    let id = current_folder(s, p)?;
    let Some(f) = s.mailbox.folder(id) else {
        return Err(CmdError::Params("no such folder".into()));
    };
    let in_favorites = match opt_str(p, "in") {
        Some("favorites") => true,
        Some("tree") => false,
        Some(other) => return Err(CmdError::Params(format!("`in` must be favorites or tree, not {other}"))),
        None => f.favorite_pos.is_some(),
    };
    if in_favorites {
        let favs = s.mailbox.favorites();
        let Some(pos) = favs.iter().position(|x| *x == id) else {
            return Err(CmdError::Failed("the folder is not in Favorites".into()));
        };
        let at_edge = if up { pos == 0 } else { pos.saturating_add(1) >= favs.len() };
        if !at_edge {
            s.mailbox.move_favorite(id, if up { -1 } else { 1 })?;
        }
        return Ok(json!({ "folder": id.0, "moved": !at_edge, "in": "favorites" }));
    }
    if f.is_default() {
        return Err(CmdError::Failed("default folders keep their place".into()));
    }
    let (account, parent) = (f.account, f.parent);
    let mut order: Vec<FolderId> = s
        .mailbox
        .children(account, parent)
        .into_iter()
        .filter(|c| s.mailbox.folder(*c).is_some_and(|cf| cf.role.is_none()))
        .collect();
    let Some(pos) = order.iter().position(|x| *x == id) else {
        return Err(CmdError::Failed("folder not found among its siblings".into()));
    };
    let target = if up { pos.checked_sub(1) } else { pos.checked_add(1).filter(|t| *t < order.len()) };
    let Some(target) = target else {
        return Ok(json!({ "folder": id.0, "moved": false, "in": "tree" }));
    };
    order.swap(pos, target);
    for (i, fid) in order.iter().enumerate() {
        if let Some(sf) = s.mailbox.folders.get_mut(fid) {
            sf.manual_pos = u32::try_from(i).ok();
        }
    }
    Ok(json!({ "folder": id.0, "moved": true, "in": "tree" }))
}

fn move_up(s: &mut Session, p: &Value) -> CmdResult {
    shift(s, p, true)
}

fn move_down(s: &mut Session, p: &Value) -> CmdResult {
    shift(s, p, false)
}

fn sort_az(s: &mut Session, p: &Value) -> CmdResult {
    // `{"account": n}` sorts the account's top-level folders (account root context menu).
    let (account, parent) = match super::util::opt_u64(p, "account").and_then(|a| u32::try_from(a).ok()) {
        Some(a) if p.get("folder").is_none() => (mailcraft_model::AccountId(a), None),
        _ => {
            let id = current_folder(s, p)?;
            let account =
                s.mailbox.folder_account(id).ok_or_else(|| CmdError::Params("no such folder".into()))?;
            (account, Some(id))
        }
    };
    let mut cleared = 0usize;
    for f in s.mailbox.folders.values_mut() {
        if f.account == account && f.parent == parent && f.manual_pos.take().is_some() {
            cleared = cleared.saturating_add(1);
        }
    }
    Ok(json!({ "cleared": cleared }))
}

fn set_expanded(s: &mut Session, p: &Value, on: bool) -> CmdResult {
    let id = current_folder(s, p)?;
    if let Some(f) = s.mailbox.folders.get_mut(&id) {
        f.expanded = on;
    }
    Ok(json!({ "folder": id.0, "expanded": on }))
}

fn expand(s: &mut Session, p: &Value) -> CmdResult {
    set_expanded(s, p, true)
}

fn collapse(s: &mut Session, p: &Value) -> CmdResult {
    set_expanded(s, p, false)
}

/// Show All Folders A to Z is a folder-pane display mode; the session keeps no flag for it yet, so the
/// command reports the requested state (`on`, default true) for the UI to apply.
fn show_all_az(s: &mut Session, p: &Value) -> CmdResult {
    let on = opt_bool(p, "on").unwrap_or(true);
    let _ = s;
    Ok(json!({ "showAllAZ": on }))
}

// ---------------------------------------------------------------------------------------------------------
// Search folders

fn new_search_folder(s: &mut Session, p: &Value) -> CmdResult {
    let name = opt_str(p, "name").map(str::trim).filter(|n| !n.is_empty());
    let query = opt_str(p, "query").map(str::trim).filter(|q| !q.is_empty());
    let (Some(name), Some(query)) = (name, query) else {
        return Ok(dialog(s, "newSearchFolder", json!({})));
    };
    let account = s.current_account().ok_or_else(|| CmdError::Disabled("no account".into()))?;
    let root = s
        .mailbox
        .role_folder(account, FolderRole::SearchFolders)
        .ok_or_else(|| CmdError::Failed("this account has no Search Folders folder".into()))?;
    let id = s.mailbox.create_folder(root, name)?;
    if let Some(f) = s.mailbox.folders.get_mut(&id) {
        f.kind = FolderKind::Search;
        f.search_query = Some(query.to_owned());
    }
    Ok(json!({ "folder": id.0, "path": s.mailbox.folder_path(id) }))
}

// ---------------------------------------------------------------------------------------------------------
// Dialog-only commands

fn properties(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let mb = &s.mailbox;
    let size: u64 = mb.items_in(id).iter().fold(0u64, |acc, m| acc.saturating_add(m.size));
    let params = json!({
        "folder": id.0,
        "name": mb.folder(id).map(|f| f.name.clone()).unwrap_or_default(),
        "path": mb.folder_path(id),
        "unread": mb.unread_count(id),
        "total": mb.total_count(id),
        "sizeKb": size.div_ceil(1024),
    });
    Ok(dialog(s, "folderProperties", params))
}

fn auto_archive_settings(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    Ok(dialog(s, "autoArchiveSettings", json!({ "folder": id.0 })))
}

fn open_in_new_window(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    Ok(dialog(s, "mainWindow", json!({ "folder": id.0 })))
}

// ---------------------------------------------------------------------------------------------------------
// Rules

/// Rule context for an item: the user's addresses and the address of the account it lives in.
fn rule_context(s: &Session, item: &MailItem) -> RuleContext {
    let account_address = s
        .mailbox
        .folder_account(item.folder)
        .and_then(|a| s.mailbox.account(a))
        .map(|a| a.address.to_lowercase())
        .unwrap_or_default();
    RuleContext { my_addresses: s.me.clone(), account_address }
}

/// Runs `actions` on item `id`; returns true when no further rules should run for it (Stop Processing More
/// Rules, or the item was deleted). Actions that need the network or the UI (forward, redirect, reply,
/// print, sound, desktop alert) are skipped here.
fn execute_actions(s: &mut Session, id: ItemId, actions: &[RuleAction]) -> bool {
    let mut stop = false;
    for action in actions {
        if !s.mailbox.mail.contains_key(&id) {
            return true;
        }
        match action {
            RuleAction::MoveTo(f) => {
                if s.mailbox.folder(*f).is_some_and(|t| t.kind == FolderKind::Mail) {
                    let _ = s.mailbox.move_items(&[id], *f);
                }
            }
            RuleAction::CopyTo(f) => {
                if s.mailbox.folder(*f).is_some_and(|t| t.kind == FolderKind::Mail)
                    && let Some(mut copy) = s.mailbox.mail.get(&id).cloned()
                {
                    copy.folder = *f;
                    copy.remote_id = None;
                    s.mailbox.add_mail(copy);
                }
            }
            RuleAction::AssignCategory(c) => {
                let c = c.trim();
                if let Some(m) = s.mailbox.mail.get_mut(&id)
                    && !c.is_empty()
                    && !m.categories.iter().any(|x| x.eq_ignore_ascii_case(c))
                {
                    m.categories.push(c.to_owned());
                }
            }
            RuleAction::Delete => {
                let _ = s.mailbox.delete_items(&[id]);
                stop = true;
            }
            RuleAction::PermanentlyDelete => {
                s.mailbox.mail.remove(&id);
                return true;
            }
            RuleAction::MarkAsRead => {
                s.mailbox.set_read(&[id], true);
            }
            RuleAction::FlagForFollowUp(label, days) => {
                let now = s.now();
                let due = i64::from(*days)
                    .checked_mul(DAY_SECS)
                    .and_then(|d| now.unix.checked_add(d))
                    .map(|unix| DateTime { unix, offset_min: now.offset_min });
                let label =
                    if label.trim().is_empty() { "Follow up".to_owned() } else { label.trim().to_owned() };
                if let Some(m) = s.mailbox.mail.get_mut(&id) {
                    m.flag = FlagState::Flagged { label, start: Some(now), due, reminder: None };
                }
            }
            RuleAction::ClearFlag => {
                if let Some(m) = s.mailbox.mail.get_mut(&id) {
                    m.flag = FlagState::None;
                }
            }
            RuleAction::ClearCategories => {
                if let Some(m) = s.mailbox.mail.get_mut(&id) {
                    m.categories.clear();
                }
            }
            RuleAction::MarkImportance(i) => {
                if let Some(m) = s.mailbox.mail.get_mut(&id) {
                    m.importance = *i;
                }
            }
            RuleAction::StopProcessingMoreRules => stop = true,
            RuleAction::NewItemAlert(msg) => s.notify(Notice::NewItemAlert(msg.clone(), id)),
            RuleAction::ForwardTo(_)
            | RuleAction::ForwardAsAttachment(_)
            | RuleAction::RedirectTo(_)
            | RuleAction::ReplyWithTemplate(_)
            | RuleAction::Print
            | RuleAction::PlaySound(_)
            | RuleAction::DesktopAlert => {}
        }
    }
    stop
}

/// Applies `rules` in order to the mail items `ids`; returns how many items matched at least one rule.
pub fn apply_rule_list(s: &mut Session, ids: &[ItemId], rules: &[Rule]) -> usize {
    let mut acted = 0usize;
    for &id in ids {
        let mut hit = false;
        for rule in rules {
            let Some(item) = s.mailbox.mail.get(&id) else { break };
            let ctx = rule_context(s, item);
            if !rule.matches(item, &ctx) {
                continue;
            }
            hit = true;
            if execute_actions(s, id, &rule.actions) {
                break;
            }
        }
        if hit {
            acted = acted.saturating_add(1);
        }
    }
    prune_selection(s);
    acted
}

/// Applies every enabled rule (in list order) to the mail items `ids`, executing their actions; returns how
/// many items matched at least one rule.
pub fn apply_rules(s: &mut Session, ids: &[ItemId]) -> usize {
    let rules: Vec<Rule> = s.mailbox.rules.iter().filter(|r| r.enabled).cloned().collect();
    apply_rule_list(s, ids, &rules)
}

fn run_rules_now(s: &mut Session, p: &Value) -> CmdResult {
    let folder = current_folder(s, p)?;
    let all = opt_bool(p, "all").unwrap_or(false);
    let chosen = p.get("rules").and_then(Value::as_array);
    if !all && chosen.is_none() {
        let rules: Vec<Value> = s
            .mailbox
            .rules
            .iter()
            .map(|r| json!({ "id": r.id, "name": r.name, "enabled": r.enabled }))
            .collect();
        let path = s.mailbox.folder_path(folder);
        return Ok(dialog(s, "runRulesNow", json!({ "folder": folder.0, "path": path, "rules": rules })));
    }
    let rules: Vec<Rule> = if let Some(list) = chosen {
        let mut out = Vec::new();
        for v in list {
            let found = s.mailbox.rules.iter().find(|r| match v {
                Value::Number(n) => n.as_u64() == Some(r.id),
                Value::String(name) => r.name.eq_ignore_ascii_case(name.trim()),
                _ => false,
            });
            match found {
                Some(r) => out.push(r.clone()),
                None => return Err(CmdError::Params(format!("no such rule: {v}"))),
            }
        }
        out
    } else {
        s.mailbox.rules.iter().filter(|r| r.enabled).cloned().collect()
    };
    let folders: BTreeSet<FolderId> = if opt_bool(p, "subfolders").unwrap_or(false) {
        s.mailbox.subtree(folder).into_iter().collect()
    } else {
        [folder].into_iter().collect()
    };
    let which = opt_str(p, "which").unwrap_or("all");
    let want: fn(&MailItem) -> bool = match which {
        "all" => |_| true,
        "read" => |m| m.read,
        "unread" => |m| !m.read,
        other => return Err(CmdError::Params(format!("`which` must be all, read or unread, not {other}"))),
    };
    let ids: Vec<ItemId> =
        s.mailbox.mail.values().filter(|m| folders.contains(&m.folder) && want(m)).map(|m| m.id).collect();
    let matched = apply_rule_list(s, &ids, &rules);
    Ok(json!({ "rules": rules.len(), "items": ids.len(), "matched": matched }))
}

// ---------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::{Address, Importance, RuleCondition};

    fn session() -> Session {
        let mb = Mailbox::new_local("Test");
        let mut s = Session::new(mb);
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        s.selection.folder = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s
    }

    fn role(s: &Session, r: FolderRole) -> FolderId {
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        s.mailbox.role_folder(acct, r).unwrap()
    }

    fn mail(s: &mut Session, folder: FolderId, subject: &str, from: &str) -> ItemId {
        s.mailbox.add_mail(MailItem {
            folder,
            subject: subject.into(),
            from: Some(Address { name: None, addr: from.into() }),
            ..Default::default()
        })
    }

    fn run(s: &mut Session, id: &str, p: Value) -> Value {
        s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    fn dialogs(s: &Session) -> Vec<String> {
        s.notices
            .iter()
            .filter_map(|n| if let Notice::OpenDialog(name, _) = n { Some(name.clone()) } else { None })
            .collect()
    }

    #[test]
    fn all_ids_registered() {
        let ids: Vec<&str> = specs().iter().map(|s| s.id).collect();
        for id in [
            "folder.addFavorite",
            "folder.autoArchiveSettings",
            "folder.copy",
            "folder.delete",
            "folder.deleteAll",
            "folder.markAllRead",
            "folder.move",
            "folder.moveDown",
            "folder.moveUp",
            "folder.new",
            "folder.newSearchFolder",
            "folder.openInNewWindow",
            "folder.properties",
            "folder.recoverDeleted",
            "folder.removeFavorite",
            "folder.rename",
            "folder.runRulesNow",
            "folder.showAllAZ",
            "folder.sortAZ",
            "folder.expand",
            "folder.collapse",
            "folder.emptyDeleted",
        ] {
            assert!(ids.contains(&id), "{id} missing");
        }
    }

    #[test]
    fn new_folder_under_selection_and_dialog() {
        let mut s = session();
        let inbox = s.selection.folder;
        let r = run(&mut s, "folder.new", json!({"name": "Projects"}));
        let id = FolderId(r["folder"].as_u64().unwrap());
        assert_eq!(s.mailbox.folder(id).unwrap().parent, Some(inbox));
        assert_eq!(r["path"], "Inbox\\Projects");
        let r = run(&mut s, "folder.new", json!({"name": "Sub", "parent": "Inbox\\Projects"}));
        let sub = FolderId(r["folder"].as_u64().unwrap());
        assert_eq!(s.mailbox.folder(sub).unwrap().parent, Some(id));
        assert!(s.run("folder.new", &json!({"name": "projects"})).is_err());
        let r = run(&mut s, "folder.new", json!({}));
        assert_eq!(r["dialog"], "createFolder");
        assert_eq!(dialogs(&s), vec!["createFolder"]);
        assert!(s.run("folder.new", &json!({"name": "x", "parent": "Nope"})).is_err());
    }

    #[test]
    fn rename_only_user_folders() {
        let mut s = session();
        assert!(s.run("folder.rename", &json!({"name": "X"})).is_err());
        let r = run(&mut s, "folder.new", json!({"name": "A"}));
        s.selection.folder = FolderId(r["folder"].as_u64().unwrap());
        let r = run(&mut s, "folder.rename", json!({"name": "  B "}));
        assert_eq!(r["name"], "B");
        assert_eq!(run(&mut s, "folder.rename", json!({}))["dialog"], "renameFolder");
    }

    #[test]
    fn copy_is_deep_with_new_ids() {
        let mut s = session();
        let inbox = s.selection.folder;
        let a = FolderId(run(&mut s, "folder.new", json!({"name": "A"}))["folder"].as_u64().unwrap());
        let b = FolderId(
            run(&mut s, "folder.new", json!({"name": "B", "parent": a.0}))["folder"].as_u64().unwrap(),
        );
        let m1 = mail(&mut s, a, "one", "x@example.com");
        mail(&mut s, b, "two", "y@example.com");
        let archive = role(&s, FolderRole::Archive);
        s.selection.folder = a;
        let r = run(&mut s, "folder.copy", json!({"parent": archive.0}));
        assert_eq!(r["folders"], 2);
        assert_eq!(r["items"], 2);
        let root = FolderId(r["folder"].as_u64().unwrap());
        assert_ne!(root, a);
        assert_eq!(s.mailbox.folder(root).unwrap().parent, Some(archive));
        assert_eq!(s.mailbox.subtree(root).len(), 2);
        assert_eq!(s.mailbox.total_count(root), 1);
        assert!(s.mailbox.items_in(root).iter().all(|m| m.id != m1));
        assert_eq!(s.mailbox.total_count(a), 1);
        // Copy again under the same parent gets a numbered name; copying into itself fails.
        let r = run(&mut s, "folder.copy", json!({"parent": archive.0}));
        assert_eq!(s.mailbox.folder(FolderId(r["folder"].as_u64().unwrap())).unwrap().name, "A (2)");
        assert!(s.run("folder.copy", &json!({"parent": b.0})).is_err());
        assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
        assert_eq!(run(&mut s, "folder.copy", json!({}))["dialog"], "copyFolder");
        let _ = inbox;
    }

    #[test]
    fn move_and_delete_folder() {
        let mut s = session();
        let a = FolderId(run(&mut s, "folder.new", json!({"name": "A"}))["folder"].as_u64().unwrap());
        s.selection.folder = a;
        let archive = role(&s, FolderRole::Archive);
        run(&mut s, "folder.move", json!({"parent": "Archive"}));
        assert_eq!(s.mailbox.folder(a).unwrap().parent, Some(archive));
        assert_eq!(run(&mut s, "folder.move", json!({}))["dialog"], "moveFolder");
        let r = run(&mut s, "folder.delete", json!({}));
        assert_eq!(r["permanent"], false);
        let deleted = role(&s, FolderRole::DeletedItems);
        assert_eq!(s.mailbox.folder(a).unwrap().parent, Some(deleted));
        let r = run(&mut s, "folder.delete", json!({}));
        assert_eq!(r["permanent"], true);
        assert!(s.mailbox.folder(a).is_none());
        assert_ne!(s.selection.folder, a);
        // Default folders cannot be deleted.
        s.selection.folder = role(&s, FolderRole::Inbox);
        assert!(matches!(s.run("folder.delete", &json!({})), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn delete_all_and_mark_all_read() {
        let mut s = session();
        let inbox = s.selection.folder;
        let a = mail(&mut s, inbox, "a", "a@example.com");
        mail(&mut s, inbox, "b", "b@example.com");
        s.selection.items = vec![a];
        assert_eq!(run(&mut s, "folder.markAllRead", json!({}))["changed"], 2);
        assert_eq!(s.mailbox.unread_count(inbox), 0);
        assert_eq!(run(&mut s, "folder.deleteAll", json!({}))["deleted"], 2);
        assert_eq!(s.mailbox.total_count(inbox), 0);
        assert!(s.selection.items.is_empty());
        assert_eq!(s.mailbox.total_count(role(&s, FolderRole::DeletedItems)), 2);
        assert!(matches!(s.run("folder.deleteAll", &json!({})), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn empty_deleted_and_recover() {
        let mut s = session();
        let deleted = role(&s, FolderRole::DeletedItems);
        assert!(s.run("folder.emptyDeleted", &json!({})).is_err());
        let id = mail(&mut s, deleted, "gone", "a@example.com");
        s.mailbox.create_folder(deleted, "Old").unwrap();
        // Simulate the undo snapshot the session records before a mutating command.
        s.undo.undo.push(crate::undo::UndoEntry { label: "x".into(), before: Box::new(s.mailbox.clone()) });
        let r = run(&mut s, "folder.emptyDeleted", json!({}));
        assert_eq!(r["items"], 1);
        assert_eq!(r["folders"], 1);
        assert!(!s.mailbox.mail.contains_key(&id));
        let r = run(&mut s, "folder.recoverDeleted", json!({}));
        assert_eq!(r["count"], 1);
        let r = run(&mut s, "folder.recoverDeleted", json!({"ids": [id.0]}));
        assert_eq!(r["restored"], 1);
        assert_eq!(s.mailbox.mail.get(&id).unwrap().folder, deleted);
        assert!(s.mailbox.validate().is_empty());
    }

    #[test]
    fn empty_deleted_removes_deleted_contacts_tasks_notes_and_appointments() {
        let mut s = session();
        let deleted = role(&s, FolderRole::DeletedItems);
        let task = s.run("tasks.new", &json!({"subject": "old task"})).unwrap()["id"].as_u64().unwrap();
        s.run("tasks.delete", &json!({"id": task})).unwrap();
        assert_eq!(s.mailbox.tasks[&ItemId(task)].folder, deleted);
        // Only a deleted task is in Deleted Items: Empty Folder is available and removes it.
        assert_eq!(s.enabled("folder.emptyDeleted"), None);
        let r = run(&mut s, "folder.emptyDeleted", json!({}));
        assert_eq!(r["items"], 1);
        assert!(!s.mailbox.tasks.contains_key(&ItemId(task)));
        assert!(s.enabled("folder.emptyDeleted").is_some());
    }

    #[test]
    fn delete_all_covers_non_mail_items() {
        let mut s = session();
        let notes = role(&s, FolderRole::Notes);
        let deleted = role(&s, FolderRole::DeletedItems);
        let note = s.run("notes.new", &json!({"text": "x"})).unwrap()["id"].as_u64().unwrap();
        s.selection.folder = notes;
        assert_eq!(s.enabled("folder.deleteAll"), None);
        assert_eq!(run(&mut s, "folder.deleteAll", json!({}))["deleted"], 1);
        assert_eq!(s.mailbox.notes[&ItemId(note)].folder, deleted);
        // Delete All in Deleted Items removes permanently.
        s.selection.folder = deleted;
        assert_eq!(run(&mut s, "folder.deleteAll", json!({}))["deleted"], 1);
        assert!(s.mailbox.notes.is_empty());
    }

    #[test]
    fn favorites_toggle_and_order() {
        let mut s = session();
        let inbox = s.selection.folder;
        let archive = role(&s, FolderRole::Archive);
        s.selection.folder = archive;
        assert_eq!(run(&mut s, "folder.addFavorite", json!({}))["favorite"], true);
        assert_eq!(s.mailbox.favorites().last(), Some(&archive));
        run(&mut s, "folder.moveUp", json!({"folder": archive.0}));
        let favs = s.mailbox.favorites();
        assert_eq!(favs.get(favs.len() - 2), Some(&archive));
        run(&mut s, "folder.moveDown", json!({"folder": "Archive"}));
        assert_eq!(s.mailbox.favorites().last(), Some(&archive));
        let r = run(&mut s, "folder.moveDown", json!({"folder": "Archive"}));
        assert_eq!(r["moved"], false);
        assert_eq!(run(&mut s, "folder.moveUp", json!({"folder": inbox.0}))["moved"], false);
        run(&mut s, "folder.removeFavorite", json!({"folder": "Archive"}));
        assert!(!s.mailbox.favorites().contains(&archive));
        assert_eq!(run(&mut s, "folder.addFavorite", json!({}))["favorite"], true);
        assert_eq!(run(&mut s, "folder.addFavorite", json!({}))["favorite"], false);
    }

    #[test]
    fn tree_move_and_sort_az() {
        let mut s = session();
        let inbox = s.selection.folder;
        for n in ["A", "B", "C"] {
            run(&mut s, "folder.new", json!({"name": n}));
        }
        let acct = s.mailbox.folder_account(inbox).unwrap();
        let names = |s: &Session| -> Vec<String> {
            s.mailbox
                .children(acct, Some(inbox))
                .iter()
                .map(|f| s.mailbox.folder(*f).unwrap().name.clone())
                .collect()
        };
        run(&mut s, "folder.moveUp", json!({"folder": "Inbox\\C"}));
        assert_eq!(names(&s), ["A", "C", "B"]);
        run(&mut s, "folder.moveDown", json!({"folder": "Inbox\\A", "in": "tree"}));
        assert_eq!(names(&s), ["C", "A", "B"]);
        assert!(s.run("folder.moveUp", &json!({"folder": inbox.0, "in": "tree"})).is_err());
        assert_eq!(run(&mut s, "folder.sortAZ", json!({"folder": inbox.0}))["cleared"], 3);
        assert_eq!(names(&s), ["A", "B", "C"]);
    }

    #[test]
    fn expand_collapse_and_show_all() {
        let mut s = session();
        let inbox = s.selection.folder;
        run(&mut s, "folder.collapse", json!({}));
        assert!(!s.mailbox.folder(inbox).unwrap().expanded);
        run(&mut s, "folder.expand", json!({"folder": "Inbox"}));
        assert!(s.mailbox.folder(inbox).unwrap().expanded);
        assert_eq!(run(&mut s, "folder.showAllAZ", json!({"on": false}))["showAllAZ"], false);
    }

    #[test]
    fn new_search_folder_under_search_folders() {
        let mut s = session();
        assert_eq!(run(&mut s, "folder.newSearchFolder", json!({}))["dialog"], "newSearchFolder");
        let r = run(&mut s, "folder.newSearchFolder", json!({"name": "Unread Mail", "query": "isread:no"}));
        let f = s.mailbox.folder(FolderId(r["folder"].as_u64().unwrap())).unwrap();
        assert_eq!(f.kind, FolderKind::Search);
        assert_eq!(f.parent, Some(role(&s, FolderRole::SearchFolders)));
        assert_eq!(f.search_query.as_deref(), Some("isread:no"));
    }

    #[test]
    fn dialog_commands() {
        let mut s = session();
        assert_eq!(run(&mut s, "folder.properties", json!({}))["dialog"], "folderProperties");
        assert_eq!(run(&mut s, "folder.autoArchiveSettings", json!({}))["dialog"], "autoArchiveSettings");
        assert_eq!(run(&mut s, "folder.openInNewWindow", json!({}))["dialog"], "mainWindow");
        assert_eq!(run(&mut s, "folder.runRulesNow", json!({}))["dialog"], "runRulesNow");
        assert_eq!(dialogs(&s).len(), 4);
    }

    #[test]
    fn run_rules_now_executes_actions() {
        let mut s = session();
        let inbox = s.selection.folder;
        let archive = role(&s, FolderRole::Archive);
        let a = mail(&mut s, inbox, "Budget review", "boss@example.com");
        let b = mail(&mut s, inbox, "Lunch", "pal@example.com");
        s.mailbox.rules = vec![
            Rule {
                id: 1,
                name: "Budget".into(),
                enabled: true,
                conditions: vec![RuleCondition::SubjectContains(vec!["budget".into()])],
                actions: vec![
                    RuleAction::AssignCategory("Red category".into()),
                    RuleAction::MarkImportance(Importance::High),
                    RuleAction::FlagForFollowUp("Follow up".into(), 2),
                    RuleAction::MarkAsRead,
                    RuleAction::CopyTo(archive),
                    RuleAction::MoveTo(archive),
                    RuleAction::StopProcessingMoreRules,
                ],
                exceptions: vec![],
            },
            Rule {
                id: 2,
                name: "Everything".into(),
                enabled: true,
                conditions: vec![],
                actions: vec![RuleAction::ClearCategories, RuleAction::Delete],
                exceptions: vec![],
            },
            Rule {
                id: 3,
                name: "Off".into(),
                enabled: false,
                conditions: vec![],
                actions: vec![RuleAction::ClearFlag],
                exceptions: vec![],
            },
        ];
        let r = run(&mut s, "folder.runRulesNow", json!({"all": true}));
        assert_eq!(r["matched"], 2);
        let ma = s.mailbox.mail.get(&a).unwrap();
        assert_eq!(ma.folder, archive);
        assert!(ma.read && ma.is_flagged() && ma.importance == Importance::High);
        assert_eq!(ma.categories, vec!["Red category".to_string()]);
        assert_eq!(s.mailbox.total_count(archive), 2);
        assert_eq!(s.mailbox.mail.get(&b).unwrap().folder, role(&s, FolderRole::DeletedItems));
        // Explicitly chosen rules run even when disabled.
        s.selection.folder = archive;
        let r = run(&mut s, "folder.runRulesNow", json!({"rules": ["Off"]}));
        assert_eq!(r["matched"], 2);
        assert!(!s.mailbox.mail.get(&a).unwrap().is_flagged());
        assert!(s.run("folder.runRulesNow", &json!({"rules": [99]})).is_err());
        // The shared executor counts matching items.
        let c = mail(&mut s, inbox, "budget again", "x@example.com");
        assert_eq!(apply_rules(&mut s, &[c, ItemId(999_999)]), 1);
    }

    #[test]
    fn resolve_folder_forms() {
        let s = session();
        let inbox = s.selection.folder;
        assert_eq!(resolve_folder(&s, &json!(inbox.0)), Some(inbox));
        assert_eq!(resolve_folder(&s, &json!(inbox.0.to_string())), Some(inbox));
        assert_eq!(resolve_folder(&s, &json!("inbox")), Some(inbox));
        assert_eq!(resolve_folder(&s, &json!("Test\\Inbox")), Some(inbox));
        assert_eq!(resolve_folder(&s, &json!("nope")), None);
        assert_eq!(resolve_folder(&s, &json!(123_456)), None);
    }
}
