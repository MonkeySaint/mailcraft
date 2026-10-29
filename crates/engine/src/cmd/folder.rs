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
use mailcraft_model::{AccountId, Folder, FolderId, FolderKind, FolderRole, ItemId, MailItem, Mailbox, Rule};

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
            r#"{"folder"?:string} | {"account":number} (account root: its Inbox)"#,
            true,
            has_folder,
            add_favorite,
        ),
        spec(
            "folder.autoArchiveSettings",
            "AutoArchive Settings",
            "Folder › Properties",
            "",
            r#"{"folder"?:string} (dialog) | {"folder"?:string,"mode"?:"default"|"never"|"custom","age"?:number,"unit"?:"days"|"weeks"|"months","action"?:"move"|"delete","target"?:string|null}"#,
            true,
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
        spec(
            "folder.delete",
            "Delete Folder",
            "Folder › Actions",
            "",
            r#"{"folder"?: number|string, "confirm"?: bool} (asks first unless confirm: true)"#,
            true,
            user_folder,
            delete,
        ),
        spec(
            "folder.deleteAll",
            "Delete All",
            "Folder › Clean Up",
            "",
            r#"{"folder"?: number|string, "confirm"?: bool} (asks first unless confirm: true)"#,
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
            r#"{"name"?:string,"parent"?:string|number,"account"?:number (with no parent: at the top of this account's folders),"contains"?:"mail"|"calendar"|"contacts"|"journal"|"notes"|"tasks" (also "kind"; what the folder holds; default: the module's items in Calendar, People, Tasks and Notes, else what the parent holds)}"#,
            true,
            always,
            new_folder,
        ),
        spec(
            "folder.newSearchFolder",
            "New Search Folder",
            "Folder › New",
            "Mod+Shift+P",
            r#"{"name"?:string,"query"?:string,"template"?:"unread|flagged|unreadOrFlagged|important|fromAndTo|from|toMe|toGroup|categorized|large|old|attachments|words|custom","value"?:string,"account"?:number} (account: the mailbox searched, default the current one)"#,
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
            "folder.setProperties",
            "Folder Properties: General",
            "Folder › Properties › General",
            "",
            r#"{"folder"?: number, "description"?: string, "showTotal"?: bool|null} (showTotal: the folder pane counts every item, false = unread items, null = the folder's usual count)"#,
            true,
            has_folder,
            set_properties,
        ),
        spec(
            "folder.recoverDeleted",
            "Recover Deleted Items",
            "Folder › Clean Up",
            "",
            r#"{"ids"?:[number] (recover these), "purge"?:[number] (remove these from the list for good)}"#,
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
    let keep = |id: &ItemId| mb.mail.get(*id).is_some_and(|m| m.folder == folder);
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
        || mb.mail.contains(ItemId(id))
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

/// "Folder contains" choice of New Folder: a key, a few other spellings, or a label of [`FOLDER_CONTENTS`].
fn folder_kind(v: &str) -> Option<FolderKind> {
    let v = v.trim();
    Some(match v.to_ascii_lowercase().as_str() {
        "mail" | "post" => FolderKind::Mail,
        "calendar" => FolderKind::Calendar,
        "contacts" | "contact" => FolderKind::Contacts,
        "tasks" | "task" => FolderKind::Tasks,
        "notes" | "note" => FolderKind::Notes,
        "journal" => FolderKind::Journal,
        _ => {
            return FOLDER_CONTENTS
                .iter()
                .find(|(_, label, _)| label.eq_ignore_ascii_case(v))
                .map(|(_, _, k)| *k);
        }
    })
}

/// Create New Folder › Folder contains: (key, label, kind), in the dialog's order.
pub const FOLDER_CONTENTS: [(&str, &str, FolderKind); 6] = [
    ("mail", "Mail and Post Items", FolderKind::Mail),
    ("calendar", "Calendar Items", FolderKind::Calendar),
    ("contacts", "Contact Items", FolderKind::Contacts),
    ("journal", "Journal Items", FolderKind::Journal),
    ("notes", "Note Items", FolderKind::Notes),
    ("tasks", "Task Items", FolderKind::Tasks),
];

/// The Folder contains key of a folder kind ("mail" for mail and search folders).
pub fn contents_key(kind: FolderKind) -> &'static str {
    FOLDER_CONTENTS.iter().find(|(_, _, k)| *k == kind).map_or("mail", |(key, _, _)| *key)
}

/// What a new folder contains by default: in the Calendar, People, Tasks and Notes modules their items,
/// else what the parent holds (mail at the top of an account and under Search Folders).
fn default_contents(s: &Session, parent: Option<FolderId>) -> FolderKind {
    match s.view.module {
        crate::Module::Calendar => FolderKind::Calendar,
        crate::Module::People => FolderKind::Contacts,
        crate::Module::Tasks => FolderKind::Tasks,
        crate::Module::Notes => FolderKind::Notes,
        _ => match parent.and_then(|f| s.mailbox.folder(f)).map(|f| f.kind) {
            Some(FolderKind::Search | FolderKind::SearchRoot) | None => FolderKind::Mail,
            Some(k) => k,
        },
    }
}

fn new_folder(s: &mut Session, p: &Value) -> CmdResult {
    let account = match p.get("account").filter(|v| !v.is_null()) {
        Some(v) => {
            let id = v
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .map(AccountId)
                .filter(|a| s.mailbox.account(*a).is_some())
                .ok_or_else(|| CmdError::Params(format!("no such account: {v}")))?;
            Some(id)
        }
        None => None,
    };
    let parent = match param_folder(s, p, "parent")? {
        Some(f) => Some(f),
        // The account root: a top-level folder of that account.
        None if account.is_some() => None,
        None => Some(current_folder(s, &Value::Null)?),
    };
    let contains = match opt_str(p, "contains").or_else(|| opt_str(p, "kind")) {
        Some(v) => {
            Some(folder_kind(v).ok_or_else(|| CmdError::Params(format!("unknown folder contents `{v}`")))?)
        }
        None => None,
    };
    let parent_kind = parent.and_then(|f| s.mailbox.folder(f)).map(|f| f.kind).unwrap_or_default();
    // Search folders take saved searches only: what they contain is not a choice.
    let fixed = matches!(parent_kind, FolderKind::Search | FolderKind::SearchRoot);
    let Some(name) = opt_str(p, "name") else {
        let path = parent.map(|f| s.mailbox.folder_path(f)).unwrap_or_default();
        let default = contents_key(default_contents(s, parent));
        let kinds: Vec<Value> =
            FOLDER_CONTENTS.iter().map(|(k, l, _)| json!({ "key": k, "label": l })).collect();
        let mut params = match parent {
            Some(f) => json!({ "parent": f.0, "path": path }),
            None => json!({ "account": account.map(|a| a.0), "path": path }),
        };
        if let Some(o) = params.as_object_mut() {
            o.insert("contains".into(), json!(default));
            o.insert("kind".into(), json!(default));
            o.insert("kinds".into(), json!(kinds));
            o.insert("fixed".into(), json!(fixed));
        }
        return Ok(dialog(s, "createFolder", params));
    };
    if fixed && contains.is_some() {
        return Err(CmdError::Params("folders under Search Folders are search folders".into()));
    }
    // No choice given: the module's items (Calendar, People, Tasks, Notes), else what the parent holds.
    let contains = if fixed { None } else { Some(contains.unwrap_or_else(|| default_contents(s, parent))) };
    let id = match (parent, contains) {
        (Some(parent), kind) => s.mailbox.create_folder_of_kind(parent, name, kind)?,
        (None, kind) => {
            let acct = account.ok_or_else(|| CmdError::Params("no account for the new folder".into()))?;
            s.mailbox.create_folder_in(acct, None, name, kind.unwrap_or(FolderKind::Mail))?
        }
    };
    if let Some(parent) = parent
        && let Some(f) = s.mailbox.folders.get_mut(&parent)
    {
        f.expanded = true;
    }
    let kind = s.mailbox.folder(id).map(|f| f.kind).unwrap_or_default();
    Ok(json!({ "folder": id.0, "path": s.mailbox.folder_path(id), "contains": contents_key(kind) }))
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
                // A calendar keeps its properties there (colour, saved views); other kinds keep the
                // server's name, which the copy does not have.
                remote_name: if kind == FolderKind::Calendar { of.remote_name.clone() } else { None },
                auto_archive: of.auto_archive,
                description: of.description.clone(),
                show_total: of.show_total,
            },
        );
        if kind == FolderKind::Calendar {
            // The copy is an ordinary calendar: not a subscription (its items are copies), not the default
            // calendar's time zones.
            super::calendar::props::update(mb, id, |x| {
                if x.subscription.take().is_some()
                    || x.source == mailcraft_model::calendar::CalendarSource::Internet
                {
                    x.source = mailcraft_model::calendar::CalendarSource::Local;
                    x.read_only = false;
                }
                x.time_zones = None;
                // Nor the original's publication: publishing it would overwrite the original's published file,
                // and the original's pending removals are not the copy's to carry out.
                x.publication = None;
                x.withdrawals.clear();
            });
        }
        if let Some(v) = mb.views.get(old).cloned() {
            mb.views.insert(id, v);
        }
        map.insert(*old, id);
    }
    let mut items = 0usize;
    let mail: Vec<MailItem> = mb.mail.items().filter(|m| map.contains_key(&m.folder)).cloned().collect();
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
    // The copies are new items: none of them is the server copy of its original.
    let copied: BTreeSet<FolderId> = map.values().copied().collect();
    for a in mb.appointments.values_mut().filter(|a| copied.contains(&a.folder)) {
        a.remote = None;
    }
    for c in mb.contacts.values_mut().filter(|c| copied.contains(&c.folder)) {
        c.remote = None;
    }
    for t in mb.tasks.values_mut().filter(|t| copied.contains(&t.folder)) {
        t.remote = None;
    }
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

/// Commands that destroy many items at once ask first: without `confirm: true` they put a `confirm` dialog
/// up (its Yes runs `command` again with the folder fixed and `confirm: true`) and return
/// `{"asked": true}` (`Some`); with it they go ahead (`None`).
pub(crate) fn ask_first(
    s: &mut Session,
    p: &Value,
    command: &str,
    folder: FolderId,
    title: &str,
    text: String,
    yes: &str,
) -> Option<CmdResult> {
    if opt_bool(p, "confirm") == Some(true) {
        return None;
    }
    let mut params = p.as_object().cloned().unwrap_or_default();
    params.insert("folder".into(), json!(folder.0));
    params.insert("confirm".into(), json!(true));
    s.notify(Notice::OpenDialog(
        "confirm".into(),
        json!({"title": title, "text": text, "command": command, "params": params, "yes": yes, "no": "Cancel"}),
    ));
    Some(Ok(json!({"asked": true, "folder": folder.0, "dialog": "confirm"})))
}

/// True when `id` is (inside) its account's Deleted Items, where deleting is for good.
fn in_deleted_items(s: &Session, id: FolderId) -> bool {
    s.mailbox
        .folder_account(id)
        .and_then(|a| s.mailbox.role_folder(a, FolderRole::DeletedItems))
        .is_some_and(|d| s.mailbox.is_in_subtree(id, d))
}

/// Delete Folder (asks first, see [`ask_first`]).
fn delete(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let name = s.mailbox.folder(id).map(|f| f.name.clone()).unwrap_or_default();
    let text = if in_deleted_items(s, id) {
        format!("Permanently delete the folder \"{name}\" and everything in it? This can't be undone.")
    } else {
        format!("Move the folder \"{name}\" and everything in it to Deleted Items?")
    };
    if let Some(asked) = ask_first(s, p, "folder.delete", id, "Delete Folder", text, "Delete") {
        return asked;
    }
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

/// Delete All: every item of the folder goes to Deleted Items (asks first, see [`ask_first`]).
fn delete_all(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let name = s.mailbox.folder(id).map(|f| f.name.clone()).unwrap_or_default();
    let text = if in_deleted_items(s, id) {
        format!("Permanently delete everything in \"{name}\"? This can't be undone.")
    } else {
        format!("Move everything in \"{name}\" to Deleted Items?")
    };
    if let Some(asked) = ask_first(s, p, "folder.deleteAll", id, "Delete All", text, "Delete All") {
        return asked;
    }
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

/// Recover Deleted Items: no `ids` lists what can be recovered from the recoverable-items bin (dialog,
/// newest first, see `crate::recover`); `ids` puts those items back into the Deleted Items folder of their
/// account; `purge` removes those entries from the bin for good.
fn recover_deleted(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(purge) = p.get("purge") {
        let ids: Vec<ItemId> = purge
            .as_array()
            .ok_or_else(|| CmdError::Params("`purge` takes a list of item ids".into()))?
            .iter()
            .filter_map(Value::as_u64)
            .map(ItemId)
            .collect();
        let purged = crate::recover::purge(s, &ids);
        return Ok(json!({ "purged": purged }));
    }
    let Some(ids) = super::util::opt_ids(p) else {
        let list: Vec<Value> = crate::recover::listed(s)
            .into_iter()
            .map(|e| {
                let (subject, from, when) = e.item.summary();
                json!({
                    "id": e.item.id().0,
                    "kind": e.item.kind(),
                    "subject": subject,
                    "from": from,
                    "received": when,
                    "deleted": e.deleted,
                    "folder": e.folder_path,
                })
            })
            .collect();
        let count = list.len();
        let days = crate::recover::keep_days(s);
        let mut v = dialog(s, "recoverDeletedItems", json!({ "items": list, "days": days }));
        if let Some(o) = v.as_object_mut() {
            o.insert("count".into(), json!(count));
        }
        return Ok(v);
    };
    let fallback = deleted_items_folder(s);
    let wanted: BTreeSet<ItemId> = ids.into_iter().collect();
    let items: Vec<mailcraft_model::DeletedItem> = crate::recover::listed(s)
        .into_iter()
        .filter(|e| wanted.contains(&e.item.id()))
        .map(|e| e.item.clone())
        .collect();
    let mut restored = Vec::new();
    let mut recovered = Vec::new();
    let mut pairs = Vec::new();
    for item in items {
        let old_id = item.id();
        let account = s.mailbox.folder_account(item.folder());
        let target = account
            .and_then(|a| s.mailbox.role_folder(a, FolderRole::DeletedItems))
            .or_else(|| s.mailbox.folders.contains_key(&item.folder()).then_some(item.folder()))
            .or(fallback);
        let Some(target) = target else { continue };
        let id = if id_taken(&s.mailbox, old_id.0) { ItemId(s.mailbox.alloc_id()) } else { old_id };
        s.mailbox.next_id = s.mailbox.next_id.max(id.0);
        item.restore(&mut s.mailbox, id, target);
        restored.push(id.0);
        recovered.push(old_id.0);
        pairs.push((old_id, id));
    }
    crate::recover::mark_recovered(s, &pairs);
    Ok(json!({ "restored": restored.len(), "ids": restored, "recovered": recovered }))
}

// ---------------------------------------------------------------------------------------------------------
// Favourites, order, expansion

/// Add to Favorites (a toggle on the Folder tab). On an account root (`{"account": n}`, no folder) it adds
/// the account's Inbox.
fn add_favorite(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("folder").is_none()
        && let Some(account) = super::util::opt_u64(p, "account")
    {
        let account = u32::try_from(account)
            .ok()
            .map(mailcraft_model::AccountId)
            .filter(|a| s.mailbox.account(*a).is_some())
            .ok_or_else(|| CmdError::Params(format!("no such account {account}")))?;
        let inbox = s
            .mailbox
            .role_folder(account, FolderRole::Inbox)
            .ok_or_else(|| CmdError::Failed("this account has no Inbox".into()))?;
        let was = s.mailbox.folder(inbox).is_some_and(|f| f.favorite_pos.is_some());
        s.mailbox.add_favorite(inbox)?;
        return Ok(json!({ "folder": inbox.0, "favorite": true, "changed": !was }));
    }
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

/// Setting that sorts every folder of the folder pane alphabetically, default folders included (Folder ›
/// Show All Folders A to Z), saved with the profile when the settings store defines it.
pub const SHOW_ALL_AZ_SETTING: &str = "state.showAllFoldersAZ";

/// True when Show All Folders A to Z is on.
pub fn show_all_az_on(s: &Session) -> bool {
    s.settings.bool(SHOW_ALL_AZ_SETTING)
}

/// `account`'s folders under `parent` in folder-pane order: [`Mailbox::children`] order, or by name only
/// (default folders mixed in) when Show All Folders A to Z is on.
pub fn pane_children(
    s: &Session,
    account: mailcraft_model::AccountId,
    parent: Option<FolderId>,
) -> Vec<FolderId> {
    let mut kids = s.mailbox.children(account, parent);
    if show_all_az_on(s) {
        kids.sort_by_cached_key(|id| {
            (s.mailbox.folder(*id).map(|f| f.name.to_lowercase()).unwrap_or_default(), *id)
        });
    }
    kids
}

/// Show All Folders A to Z (toggle): `on` sets it, no `on` flips it. Returns the new state and whether it
/// was saved with the profile.
fn show_all_az(s: &mut Session, p: &Value) -> CmdResult {
    let on = opt_bool(p, "on").unwrap_or(!show_all_az_on(s));
    let saved = crate::settings::def(SHOW_ALL_AZ_SETTING).is_some()
        && s.settings.set(SHOW_ALL_AZ_SETTING, &json!(on)).is_ok();
    Ok(json!({ "showAllAZ": on, "saved": saved }))
}

// ---------------------------------------------------------------------------------------------------------
// Search folders

/// New Search Folder: `{"name", "query"}` saves a query; `{"template", "value"?, "name"?}` builds one from
/// a New Search Folder template (`search_index::SEARCH_FOLDER_TEMPLATES`); no params opens the dialog.
/// The folder's items are evaluated live (`search_index::search_folder_items` / `search_folder_counts`).
fn new_search_folder(s: &mut Session, p: &Value) -> CmdResult {
    let from_template = match opt_str(p, "template") {
        Some(t) => Some(
            crate::search_index::search_folder_template(t, opt_str(p, "value").unwrap_or(""))
                .map_err(CmdError::Params)?,
        ),
        None => None,
    };
    let name = opt_str(p, "name").map(str::trim).filter(|n| !n.is_empty());
    let query = opt_str(p, "query").map(str::trim).filter(|q| !q.is_empty());
    let (name, query) = match (&from_template, name, query) {
        (Some((tn, tq)), n, _) => (n.unwrap_or(tn.as_str()), tq.as_str()),
        (None, Some(n), Some(q)) => (n, q),
        _ => {
            let templates: Vec<Value> = crate::search_index::SEARCH_FOLDER_TEMPLATES
                .iter()
                .map(|(id, group, label, value)| json!({"id": id, "group": group, "label": label, "value": value}))
                .collect();
            return Ok(dialog(s, "newSearchFolder", json!({ "templates": templates })));
        }
    };
    // "Search mail in": the mailbox (account or data file) whose mail the folder searches.
    let account = match p.get("account").filter(|v| !v.is_null()) {
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(AccountId)
            .filter(|a| s.mailbox.accounts.iter().any(|x| x.id == *a))
            .ok_or_else(|| CmdError::Params(format!("no such account {v}")))?,
        None => s.current_account().ok_or_else(|| CmdError::Disabled("no account".into()))?,
    };
    let root = s
        .mailbox
        .role_folder(account, FolderRole::SearchFolders)
        .ok_or_else(|| CmdError::Failed("this mailbox has no Search Folders folder".into()))?;
    let id = s.mailbox.create_folder(root, name)?;
    if let Some(f) = s.mailbox.folders.get_mut(&id) {
        f.kind = FolderKind::Search;
        f.search_query = Some(query.to_owned());
    }
    Ok(json!({ "folder": id.0, "path": s.mailbox.folder_path(id) }))
}

// ---------------------------------------------------------------------------------------------------------
// Dialog-only commands

/// What a folder holds, as the Properties dialog names it.
pub fn folder_type_label(kind: FolderKind) -> &'static str {
    match kind {
        FolderKind::Mail => "Mail and post items",
        FolderKind::Calendar => "Calendar items",
        FolderKind::Contacts => "Contacts",
        FolderKind::Tasks => "Tasks",
        FolderKind::Notes => "Notes",
        FolderKind::Journal => "Journal entries",
        FolderKind::SearchRoot => "Search folders",
        FolderKind::Search => "Saved search (search folder)",
    }
}

fn kind_id(kind: FolderKind) -> &'static str {
    match kind {
        FolderKind::Mail => "mail",
        FolderKind::Calendar => "calendar",
        FolderKind::Contacts => "contacts",
        FolderKind::Tasks => "tasks",
        FolderKind::Notes => "notes",
        FolderKind::Journal => "journal",
        FolderKind::SearchRoot => "searchRoot",
        FolderKind::Search => "search",
    }
}

/// Folder Properties (Calendar Properties for a calendar): name, type, location, item counts and size; a
/// calendar adds its colour, source and Internet subscription (changed with `calendar.setProperties`), a
/// contacts folder its Address Book settings (`people.addressBook`), a search folder its query.
fn properties(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let mb = &s.mailbox;
    let Some(folder) = mb.folder(id) else {
        return Err(CmdError::Params("no such folder".into()));
    };
    let kind = folder.kind;
    let size: u64 = mb.items_in(id).iter().fold(0u64, |acc, m| acc.saturating_add(m.size));
    let pim = pim_items_in(mb, id).len();
    let mut params = json!({
        "folder": id.0,
        "name": folder.name,
        "path": mb.folder_path(id),
        "kind": kind_id(kind),
        "type": folder_type_label(kind),
        "default": folder.is_default(),
        "unread": mb.unread_count(id),
        "total": mb.total_count(id).saturating_add(pim),
        "sizeKb": size.div_ceil(1024),
        "sizes": folder_sizes(mb, id),
        "description": folder.description,
        "showTotal": show_total_of(s, id),
    });
    if let Some(o) = params.as_object_mut() {
        match kind {
            FolderKind::Calendar => {
                o.insert("calendar".into(), super::calendar::calendar_properties_json(mb, id));
            }
            FolderKind::Contacts => {
                let b = super::people::address_book(s, id);
                let name = if b.name.is_empty() { folder.name.clone() } else { b.name };
                o.insert("addressBook".into(), json!({ "show": b.show, "name": name }));
            }
            FolderKind::Search => {
                o.insert("query".into(), json!(folder.search_query.clone().unwrap_or_default()));
            }
            _ => {}
        }
    }
    Ok(dialog(s, "folderProperties", params))
}

/// Whether the folder pane shows folder `id`'s total number of items (else its unread count): the
/// folder's Properties choice, else (profiles from before it was kept with the folder) the window's saved
/// dialog setting, else the folder's usual count.
pub fn show_total_of(s: &Session, id: FolderId) -> bool {
    let Some(f) = s.mailbox.folder(id) else { return false };
    match f.show_total {
        Some(v) => v,
        None => crate::queries::show_total_folders(s).contains(&id.0) || f.counts_total(),
    }
}

/// Properties › General: the folder's Description and "Show number of unread items" / "Show total number
/// of items" (`showTotal`; null = the folder's usual count).
fn set_properties(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    let description = match p.get("description") {
        None | Some(Value::Null) => None,
        Some(Value::String(d)) => {
            if d.chars().count() > mailcraft_model::folder::MAX_FOLDER_DESCRIPTION {
                return Err(CmdError::Params(format!(
                    "the description is longer than {} characters",
                    mailcraft_model::folder::MAX_FOLDER_DESCRIPTION
                )));
            }
            Some(d.trim_end().to_owned())
        }
        Some(_) => return Err(CmdError::Params("`description` must be text".into())),
    };
    let show_total = match p.get("showTotal") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::Bool(b)) => Some(Some(*b)),
        Some(_) => return Err(CmdError::Params("`showTotal` must be true, false or null".into())),
    };
    let Some(f) = s.mailbox.folders.get_mut(&id) else {
        return Err(CmdError::Params("no such folder".into()));
    };
    if let Some(d) = description {
        f.description = d;
    }
    if let Some(t) = show_total {
        f.show_total = t;
    }
    Ok(json!({"folder": id.0, "description": f.description, "showTotal": f.counts_total()}))
}

/// Most rows the Folder Size list shows.
const MAX_SIZE_ROWS: usize = 500;

/// Folder Size: the folder and each subfolder with its own size and its size with its subfolders (KB), the
/// folder itself first, then the subfolders by path.
fn folder_sizes(mb: &Mailbox, id: FolderId) -> Vec<Value> {
    let tree: BTreeSet<FolderId> = mb.subtree(id).into_iter().collect();
    let mut own: BTreeMap<FolderId, u64> = BTreeMap::new();
    for m in mb.mail.items().filter(|m| tree.contains(&m.folder)) {
        let e = own.entry(m.folder).or_default();
        *e = e.saturating_add(m.size);
    }
    let base = mb.folder_path(id);
    let mut rows: Vec<(String, u64, u64)> = tree
        .iter()
        .take(MAX_SIZE_ROWS)
        .map(|f| {
            let total = mb
                .subtree(*f)
                .iter()
                .fold(0u64, |acc, x| acc.saturating_add(own.get(x).copied().unwrap_or(0)));
            let path = mb.folder_path(*f);
            let rel =
                path.strip_prefix(&base).map(|r| r.trim_start_matches('\\')).unwrap_or(&path).to_owned();
            (rel, own.get(f).copied().unwrap_or(0), total)
        })
        .collect();
    rows.sort_by(|a, b| (!a.0.is_empty(), &a.0).cmp(&(!b.0.is_empty(), &b.0)));
    rows.into_iter()
        .map(|(path, size, total)| {
            json!({"path": path, "sizeKb": size.div_ceil(1024), "totalKb": total.div_ceil(1024)})
        })
        .collect()
}

/// The folder's AutoArchive properties: opens the dialog, or saves them (see `crate::archive`).
fn auto_archive_settings(s: &mut Session, p: &Value) -> CmdResult {
    crate::archive::folder_settings(s, p)
}

fn open_in_new_window(s: &mut Session, p: &Value) -> CmdResult {
    let id = current_folder(s, p)?;
    Ok(dialog(s, "mainWindow", json!({ "folder": id.0 })))
}

// ---------------------------------------------------------------------------------------------------------
// Rules

/// The rule executor lives in `rules_exec` (shared with send/receive and the `rules.*` commands).
pub use crate::cmd::rules::exec::{apply_rule_list, apply_rules};

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
    let which = opt_str(p, "which").unwrap_or("all");
    let Some(which) = crate::cmd::rules::exec::Which::parse(which) else {
        return Err(CmdError::Params(format!("`which` must be all, read or unread, not {which}")));
    };
    let subfolders = opt_bool(p, "subfolders").unwrap_or(false);
    let (items, report) = crate::cmd::rules::exec::run_now(s, &rules, folder, subfolders, which);
    Ok(json!({ "rules": rules.len(), "items": items, "matched": report.matched, "report": report.to_json() }))
}

// ---------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::{Address, Importance, RuleAction, RuleCondition};

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

    #[test]
    fn properties_count_choice_and_description_live_in_the_folder() {
        let mut s = session();
        let inbox = s.selection.folder;
        mail(&mut s, inbox, "One", "a@example.com");
        let read = mail(&mut s, inbox, "Two", "b@example.com");
        if let Some(m) = s.mailbox.mail.get_mut(read) {
            m.read = true;
        }
        assert_eq!(s.mailbox.badge(inbox).as_deref(), Some("1"));
        run(
            &mut s,
            "folder.setProperties",
            json!({"folder": inbox.0, "showTotal": true, "description": "Work"}),
        );
        assert_eq!(s.mailbox.badge(inbox).as_deref(), Some("[2]"), "every item counts");
        assert!(s.mailbox.folder_accessible_name(inbox).ends_with("2 items"));
        let r = run(&mut s, "folder.properties", json!({"folder": inbox.0}));
        assert_eq!(r["dialog"], "folderProperties");
        let Some(Notice::OpenDialog(_, p)) = s.notices.last() else { panic!() };
        assert_eq!((p["description"].as_str(), p["showTotal"].as_bool()), (Some("Work"), Some(true)));
        // Drafts counts every item by default; unread can be asked for, and null goes back.
        let drafts = role(&s, FolderRole::Drafts);
        run(&mut s, "folder.setProperties", json!({"folder": drafts.0, "showTotal": false}));
        assert!(!s.mailbox.folder(drafts).unwrap().counts_total());
        run(&mut s, "folder.setProperties", json!({"folder": drafts.0, "showTotal": null}));
        assert!(s.mailbox.folder(drafts).unwrap().counts_total());
        // Undo puts the old choice back; junk is refused.
        run(&mut s, "app.undo", json!({}));
        assert!(!s.mailbox.folder(drafts).unwrap().counts_total());
        assert!(s.run("folder.setProperties", &json!({"folder": inbox.0, "showTotal": "yes"})).is_err());
        let long = "x".repeat(mailcraft_model::folder::MAX_FOLDER_DESCRIPTION + 1);
        assert!(s.run("folder.setProperties", &json!({"folder": inbox.0, "description": long})).is_err());
        // The folder pane row follows the folder's choice.
        let rows = crate::queries::folder_rows(&s);
        let row = rows.iter().find(|r| r.id == Some(inbox)).unwrap();
        assert!(row.badge_total);
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
    fn new_folder_at_the_account_root_and_of_another_kind() {
        let mut s = session();
        let inbox = s.selection.folder;
        let acct = s.mailbox.folder_account(inbox).unwrap();
        let r = run(&mut s, "folder.new", json!({"account": acct.0}));
        assert_eq!(r["dialog"], "createFolder");
        let r = run(&mut s, "folder.new", json!({"name": "Projects", "account": acct.0}));
        let top = FolderId(r["folder"].as_u64().unwrap());
        assert_eq!(s.mailbox.folder(top).unwrap().parent, None);
        assert_eq!(s.mailbox.folder(top).unwrap().kind, FolderKind::Mail);
        assert!(s.run("folder.new", &json!({"name": "projects", "account": acct.0})).is_err());
        let r = run(&mut s, "folder.new", json!({"name": "Team", "account": acct.0, "contains": "calendar"}));
        let cal = FolderId(r["folder"].as_u64().unwrap());
        assert_eq!(s.mailbox.folder(cal).unwrap().kind, FolderKind::Calendar);
        let r =
            run(&mut s, "folder.new", json!({"name": "People", "parent": inbox.0, "contains": "contacts"}));
        let people = FolderId(r["folder"].as_u64().unwrap());
        assert_eq!(s.mailbox.folder(people).unwrap().parent, Some(inbox));
        assert_eq!(s.mailbox.folder(people).unwrap().kind, FolderKind::Contacts);
        assert!(s.run("folder.new", &json!({"name": "X", "account": 999})).is_err());
        assert!(s.run("folder.new", &json!({"name": "X", "account": acct.0, "contains": "search"})).is_err());
        assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
    }

    #[test]
    fn new_folder_contains_a_chosen_kind() {
        let mut s = session();
        let inbox = s.selection.folder;
        let r = run(&mut s, "folder.new", json!({"name": "Clients", "contains": "contacts"}));
        let id = FolderId(r["folder"].as_u64().unwrap());
        assert_eq!(s.mailbox.folder(id).unwrap().kind, FolderKind::Contacts);
        assert_eq!(r["contains"], "contacts");
        assert_eq!(s.mailbox.folder(id).unwrap().parent, Some(inbox));
        // Without `contains` a folder takes its parent's kind; the dialog offers it as the default.
        let r = run(&mut s, "folder.new", json!({"name": "Family", "parent": id.0}));
        assert_eq!(r["contains"], "contacts");
        s.selection.folder = id;
        run(&mut s, "folder.new", json!({}));
        let Some(crate::Notice::OpenDialog(_, p)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(p["contains"], "contacts");
        assert_eq!(p["fixed"], false);
        assert!(s.run("folder.new", &json!({"name": "Odd", "contains": "spreadsheets"})).is_err());
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
    fn copy_calendar_keeps_its_colour_and_views_but_not_the_subscription() {
        let mut s = session();
        let cal = role(&s, FolderRole::Calendar);
        let data = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//T//EN\r\nBEGIN:VEVENT\r\nUID:a@example.org\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261020T080000Z\r\nDURATION:PT1H\r\nSUMMARY:Feed item\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let r = run(
            &mut s,
            "calendar.addCalendar",
            json!({"source": "internet", "url": "https://cal.example.org/f.ics", "data": data, "name": "Feed"}),
        );
        let feed = FolderId(r["folder"].as_u64().unwrap());
        run(&mut s, "calendar.color", json!({"folder": feed.0, "color": "Teal"}));
        let r = run(&mut s, "folder.copy", json!({"folder": feed.0, "parent": cal.0}));
        let copy = FolderId(r["folder"].as_u64().unwrap());
        let p = crate::cmd::calendar::props::read(&s.mailbox, copy);
        assert_eq!(p.color.as_deref(), Some("Teal"), "the colour comes along");
        assert!(p.subscription.is_none() && !p.read_only, "the copy is an ordinary, editable calendar");
        assert_eq!(p.source, mailcraft_model::calendar::CalendarSource::Local);
        assert_eq!(s.mailbox.appointments.values().filter(|a| a.folder == copy).count(), 1);
        // The original is still the subscription.
        assert!(crate::cmd::calendar::props::read(&s.mailbox, feed).subscription.is_some());
        assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
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
        // Asked first; the answer names the folder.
        let r = run(&mut s, "folder.delete", json!({}));
        assert_eq!(r["asked"], true);
        assert!(s.mailbox.folder(a).is_some());
        let question = s
            .notices
            .iter()
            .rev()
            .find_map(|n| match n {
                Notice::OpenDialog(d, p) if d == "confirm" => Some(p.clone()),
                _ => None,
            })
            .unwrap();
        assert!(question["text"].as_str().unwrap().contains("\"A\""), "{question}");
        assert_eq!(question["params"]["folder"], a.0);
        let r = run(&mut s, "folder.delete", question["params"].clone());
        assert_eq!(r["permanent"], false);
        s.selection.folder = a;
        let deleted = role(&s, FolderRole::DeletedItems);
        assert_eq!(s.mailbox.folder(a).unwrap().parent, Some(deleted));
        let r = run(&mut s, "folder.delete", json!({}));
        assert!(r["asked"] == true && s.mailbox.folder(a).is_some());
        let r = run(&mut s, "folder.delete", json!({"confirm": true}));
        assert_eq!(r["permanent"], true);
        assert!(s.mailbox.folder(a).is_none());
        assert_ne!(s.selection.folder, a);
        // Default folders cannot be deleted.
        s.selection.folder = role(&s, FolderRole::Inbox);
        assert!(matches!(s.run("folder.delete", &json!({"confirm": true})), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn deleting_a_deleted_folder_is_for_good() {
        let mut s = session();
        let a = FolderId(run(&mut s, "folder.new", json!({"name": "A"}))["folder"].as_u64().unwrap());
        s.selection.folder = a;
        let r = run(&mut s, "folder.delete", json!({"confirm": true}));
        assert_eq!(r["permanent"], false);
        let deleted = role(&s, FolderRole::DeletedItems);
        assert_eq!(s.mailbox.folder(a).unwrap().parent, Some(deleted));
        let r = run(&mut s, "folder.delete", json!({"confirm": true}));
        assert_eq!(r["permanent"], true);
        assert!(s.mailbox.folder(a).is_none());
        assert_ne!(s.selection.folder, a);
        // Default folders cannot be deleted.
        s.selection.folder = role(&s, FolderRole::Inbox);
        assert!(matches!(s.run("folder.delete", &json!({"confirm": true})), Err(CmdError::Disabled(_))));
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
        assert_eq!(run(&mut s, "folder.deleteAll", json!({}))["dialog"], "confirm");
        assert_eq!(s.mailbox.total_count(inbox), 2, "Delete All asks first");
        assert_eq!(run(&mut s, "folder.deleteAll", json!({"confirm": true}))["deleted"], 2);
        assert_eq!(s.mailbox.total_count(inbox), 0);
        assert!(s.selection.items.is_empty());
        assert_eq!(s.mailbox.total_count(role(&s, FolderRole::DeletedItems)), 2);
        assert!(matches!(s.run("folder.deleteAll", &json!({"confirm": true})), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn empty_deleted_and_recover() {
        let mut s = session();
        let deleted = role(&s, FolderRole::DeletedItems);
        assert!(s.run("folder.emptyDeleted", &json!({})).is_err());
        let id = mail(&mut s, deleted, "gone", "a@example.com");
        s.mailbox.create_folder(deleted, "Old").unwrap();
        // Simulate the undo snapshot the session records before a mutating command.
        s.undo.undo.push(crate::undo::UndoEntry::new("x", Box::new(s.mailbox.clone())));
        let r = run(&mut s, "folder.emptyDeleted", json!({}));
        assert_eq!(r["items"], 1);
        assert_eq!(r["folders"], 1);
        assert!(!s.mailbox.mail.contains(id));
        let r = run(&mut s, "folder.recoverDeleted", json!({}));
        assert_eq!(r["count"], 1);
        let r = run(&mut s, "folder.recoverDeleted", json!({"ids": [id.0]}));
        assert_eq!(r["restored"], 1);
        assert_eq!(s.mailbox.mail.get(id).unwrap().folder, deleted);
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
        assert_eq!(run(&mut s, "folder.deleteAll", json!({"confirm": true}))["deleted"], 1);
        assert_eq!(s.mailbox.notes[&ItemId(note)].folder, deleted);
        // Delete All in Deleted Items removes permanently.
        s.selection.folder = deleted;
        assert_eq!(run(&mut s, "folder.deleteAll", json!({"confirm": true}))["deleted"], 1);
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
    fn show_all_folders_a_to_z_orders_default_folders_by_name_too() {
        let mut s = session();
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        let names = |s: &Session| -> Vec<String> {
            pane_children(s, acct, None).iter().map(|f| s.mailbox.folder(*f).unwrap().name.clone()).collect()
        };
        // Off: default folders first, in their fixed order.
        assert_eq!(
            names(&s),
            s.mailbox
                .children(acct, None)
                .iter()
                .map(|f| s.mailbox.folder(*f).unwrap().name.clone())
                .collect::<Vec<_>>()
        );
        let r = run(&mut s, "folder.showAllAZ", json!({"on": true}));
        assert_eq!(r["showAllAZ"], true);
        let saved = crate::settings::def(SHOW_ALL_AZ_SETTING).is_some();
        assert_eq!(r["saved"], saved);
        if saved {
            assert!(show_all_az_on(&s));
            let got = names(&s);
            let mut sorted = got.clone();
            sorted.sort_by_key(|n| n.to_lowercase());
            assert_eq!(got, sorted);
            // No `on`: flips.
            assert_eq!(run(&mut s, "folder.showAllAZ", json!({}))["showAllAZ"], false);
            assert!(!show_all_az_on(&s));
        }
    }

    #[test]
    fn add_to_favorites_on_an_account_root_adds_its_inbox() {
        let mut s = session();
        let inbox = role(&s, FolderRole::Inbox);
        let acct = s.mailbox.accounts.first().map(|a| a.id.0).unwrap();
        s.mailbox.remove_favorite(inbox).unwrap();
        let r = run(&mut s, "folder.addFavorite", json!({"account": acct}));
        assert_eq!(
            (r["folder"].clone(), r["favorite"].clone(), r["changed"].clone()),
            (json!(inbox.0), json!(true), json!(true))
        );
        assert!(s.mailbox.favorites().contains(&inbox));
        // Already a favourite: stays one (not a toggle on the account root).
        let r = run(&mut s, "folder.addFavorite", json!({"account": acct}));
        assert_eq!(r["changed"], false);
        assert!(s.mailbox.favorites().contains(&inbox));
        assert!(s.run("folder.addFavorite", &json!({"account": 999})).is_err());
    }

    #[test]
    fn properties_name_the_folder_type_and_calendar_details() {
        let mut s = session();
        let p = |s: &Session| -> Value {
            s.notices
                .iter()
                .rev()
                .find_map(|n| match n {
                    Notice::OpenDialog(d, p) if d == "folderProperties" => Some(p.clone()),
                    _ => None,
                })
                .unwrap()
        };
        run(&mut s, "folder.properties", json!({}));
        assert_eq!(p(&s)["kind"], "mail");
        assert_eq!(p(&s)["type"], folder_type_label(FolderKind::Mail));
        assert!(p(&s).get("calendar").is_none());
        // Folder Size: the folder, then each subfolder with and without its own subfolders.
        let inbox = s.selection.folder;
        let sub = s.mailbox.create_folder(inbox, "Big").unwrap();
        let deeper = s.mailbox.create_folder(sub, "Bigger").unwrap();
        s.mailbox.add_mail(MailItem { folder: sub, size: 4096, ..Default::default() });
        s.mailbox.add_mail(MailItem { folder: deeper, size: 2048, ..Default::default() });
        run(&mut s, "folder.properties", json!({"folder": inbox.0}));
        let sizes = p(&s)["sizes"].as_array().unwrap().clone();
        assert_eq!(sizes[0]["path"], "");
        assert_eq!(sizes[0]["totalKb"].as_u64().unwrap(), p(&s)["sizeKb"].as_u64().unwrap() + 6);
        let big = sizes.iter().find(|r| r["path"] == "Big").unwrap();
        assert_eq!((big["sizeKb"].as_u64(), big["totalKb"].as_u64()), (Some(4), Some(6)));
        assert!(sizes.iter().any(|r| r["path"] == "Big\\Bigger" && r["sizeKb"] == 2));
        let cal = role(&s, FolderRole::Calendar);
        run(&mut s, "calendar.newAppointment", json!({"subject": "Dentist", "start": "2026-10-12T09:00"}));
        let mut props = crate::cmd::calendar::props::read(&s.mailbox, cal);
        props.color = Some("Teal".into());
        crate::cmd::calendar::props::write(&mut s.mailbox, cal, &props);
        run(&mut s, "folder.properties", json!({"folder": cal.0}));
        let v = p(&s);
        assert_eq!(v["kind"], "calendar");
        assert_eq!(v["type"], folder_type_label(FolderKind::Calendar));
        assert_eq!(v["total"], 1, "appointments count as the calendar's items");
        assert_eq!(v["calendar"]["color"], "Teal");
        assert_eq!(v["calendar"]["source"], "local");
        assert_eq!(v["calendar"]["readOnly"], false);
        let contacts = role(&s, FolderRole::Contacts);
        run(&mut s, "folder.properties", json!({"folder": contacts.0}));
        assert_eq!(p(&s)["type"], folder_type_label(FolderKind::Contacts));
        let sf = run(&mut s, "folder.newSearchFolder", json!({"name": "Unread", "query": "isread:no"}));
        run(&mut s, "folder.properties", json!({"folder": sf["folder"]}));
        assert_eq!(p(&s)["query"], "isread:no");
    }

    #[test]
    fn recover_deleted_brings_back_contacts_tasks_notes_and_appointments() {
        let mut s = session();
        let deleted = role(&s, FolderRole::DeletedItems);
        let task = run(&mut s, "tasks.new", json!({"subject": "old task"}))["id"].as_u64().unwrap();
        let appt = run(
            &mut s,
            "calendar.newAppointment",
            json!({"subject": "Dentist", "start": "2026-10-12T09:00"}),
        )["id"]
            .as_u64()
            .unwrap();
        run(&mut s, "tasks.delete", json!({"id": task}));
        run(&mut s, "calendar.delete", json!({"ids": [appt]}));
        // The appointment may go to Deleted Items or straight away; empty Deleted Items removes what is left.
        if s.enabled("folder.emptyDeleted").is_none() {
            run(&mut s, "folder.emptyDeleted", json!({}));
        }
        assert!(!s.mailbox.tasks.contains_key(&ItemId(task)));
        assert!(!s.mailbox.appointments.contains_key(&ItemId(appt)));
        let r = run(&mut s, "folder.recoverDeleted", json!({}));
        assert_eq!(r["dialog"], "recoverDeletedItems");
        assert!(r["count"].as_u64().unwrap() >= 2);
        let listed = s
            .notices
            .iter()
            .rev()
            .find_map(|n| match n {
                Notice::OpenDialog(d, p) if d == "recoverDeletedItems" => Some(p["items"].clone()),
                _ => None,
            })
            .unwrap();
        let kinds: Vec<&str> = listed.as_array().unwrap().iter().filter_map(|i| i["kind"].as_str()).collect();
        assert!(kinds.contains(&"task") && kinds.contains(&"appointment"), "{kinds:?}");
        let r = run(&mut s, "folder.recoverDeleted", json!({"ids": [task, appt]}));
        assert_eq!(r["restored"], 2);
        let mut recovered: Vec<u64> =
            r["recovered"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
        recovered.sort_unstable();
        let mut want = vec![task, appt];
        want.sort_unstable();
        assert_eq!(recovered, want);
        assert_eq!(s.mailbox.tasks[&ItemId(task)].folder, deleted);
        assert_eq!(s.mailbox.appointments[&ItemId(appt)].folder, deleted);
        assert!(s.mailbox.validate().is_empty());
        // Recovering is one undo step.
        run(&mut s, "app.undo", json!({}));
        assert!(!s.mailbox.tasks.contains_key(&ItemId(task)));
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
        // Search mail in another mailbox: the folder goes under that mailbox's Search Folders.
        let other = s.mailbox.add_account(mailcraft_model::Account {
            display_name: "Archive file".into(),
            address: "archive@example.org".into(),
            ..Default::default()
        });
        s.mailbox.create_default_folders(other);
        let r = run(&mut s, "folder.newSearchFolder", json!({"template": "unread", "account": other.0}));
        let f = s.mailbox.folder(FolderId(r["folder"].as_u64().unwrap())).unwrap();
        assert_eq!(f.account, other);
        assert!(s.run("folder.newSearchFolder", &json!({"template": "unread", "account": 99_999})).is_err());
        assert!(s.run("folder.newSearchFolder", &json!({"template": "unread", "account": "x"})).is_err());
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
        let ma = s.mailbox.mail.get(a).unwrap();
        assert_eq!(ma.folder, archive);
        assert!(ma.read && ma.is_flagged() && ma.importance == Importance::High);
        assert_eq!(ma.categories, vec!["Red category".to_string()]);
        assert_eq!(s.mailbox.total_count(archive), 2);
        assert_eq!(s.mailbox.mail.get(b).unwrap().folder, role(&s, FolderRole::DeletedItems));
        // Explicitly chosen rules run even when disabled.
        s.selection.folder = archive;
        let r = run(&mut s, "folder.runRulesNow", json!({"rules": ["Off"]}));
        assert_eq!(r["matched"], 2);
        assert!(!s.mailbox.mail.get(a).unwrap().is_flagged());
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
