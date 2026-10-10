//! `mail.*` message actions (Home tab › Delete / Move / Tags / Find, message context menu): delete, archive,
//! move/copy, read state, follow-up flags, categories, importance, sensitivity, ignore, conversation clean-up,
//! junk, Focused/Other, new items and the dialog openers (print, open, message options, view source).
//!
//! Replies and forwards live in `respond.rs`; the helpers marked `pub(crate)` here are shared with it.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::cmd::util::{ok, opt_bool, opt_str, req_str};
use crate::compose::{ComposeKind, ComposeState};
use crate::{CmdError, CmdResult, CommandSpec, Notice, SearchScope, Session, always};
use mailcraft_model::dates::{local_day, weekday_of_day};
use mailcraft_model::subject::strip_prefixes;
use mailcraft_model::{
    AccountId, AccountKind, BodyFormat, Category, CategoryColor, Conversation, DateTime, FlagState,
    FocusClass, FolderId, FolderKind, FolderRole, Importance, ItemId, MailItem, Sensitivity, thread,
};

// ---------------------------------------------------------------------------------------------------------
// Shared helpers (also used by respond.rs)
// ---------------------------------------------------------------------------------------------------------

/// Item ids the command acts on (`params.ids` or the selection), restricted to existing mail items.
pub(crate) fn mail_targets(s: &Session, p: &Value) -> Vec<ItemId> {
    let mut seen = BTreeSet::new();
    s.target_ids(p).into_iter().filter(|id| s.mailbox.mail.contains_key(id) && seen.insert(*id)).collect()
}

/// The single item a command acts on: first of `params.ids`, else the focused item, else the first selected.
pub(crate) fn primary_item(s: &Session, p: &Value) -> Option<ItemId> {
    if let Some(ids) = crate::cmd::util::opt_ids(p) {
        return ids.into_iter().find(|id| s.mailbox.mail.contains_key(id));
    }
    s.selection
        .focus
        .into_iter()
        .chain(s.selection.items.iter().copied())
        .find(|id| s.mailbox.mail.contains_key(id))
}

/// Enabled when at least one mail item is selected.
pub(crate) fn need_items(s: &Session) -> Option<&'static str> {
    if mail_targets(s, &Value::Null).is_empty() { Some("No message is selected.") } else { None }
}

/// Enabled when a message is focused/selected.
pub(crate) fn need_one(s: &Session) -> Option<&'static str> {
    if primary_item(s, &Value::Null).is_some() { None } else { Some("No message is selected.") }
}

/// Journals the current state of `ids` (upsert, or removal when the item no longer exists).
pub(crate) fn journal(s: &mut Session, ids: &[ItemId]) {
    for id in ids {
        let op = match s.mailbox.mail.get(id) {
            Some(m) => mailcraft_store::Op::PutMail(Box::new(m.clone())),
            None => mailcraft_store::Op::RemoveMail(*id),
        };
        s.pending_ops.push(op);
    }
}

/// Journals a whole-mailbox change (master category list, rules).
fn journal_mailbox(s: &mut Session) {
    s.pending_ops.push(mailcraft_store::Op::Replace(Box::new(s.mailbox.clone())));
}

/// Queues a dialog request and returns `{"dialog": name}`.
pub(crate) fn open_dialog(s: &mut Session, name: &str, params: Value) -> CmdResult {
    s.notify(Notice::OpenDialog(name.to_owned(), params));
    Ok(json!({"dialog": name}))
}

/// Account of the folder holding `id`.
pub(crate) fn item_account(s: &Session, id: ItemId) -> Option<AccountId> {
    s.mailbox.mail.get(&id).and_then(|m| s.mailbox.folder_account(m.folder))
}

/// Folder with `role` in the account holding item `id`.
fn role_folder_of(s: &Session, id: ItemId, role: FolderRole) -> Option<FolderId> {
    item_account(s, id).and_then(|a| s.mailbox.role_folder(a, role))
}

/// Role of the folder holding item `id`.
fn item_role(s: &Session, id: ItemId) -> Option<FolderRole> {
    s.mailbox.mail.get(&id).and_then(|m| s.mailbox.folder(m.folder)).and_then(|f| f.role)
}

/// The user's own addresses (lower-case): `Session::me` plus every account address.
pub(crate) fn my_addresses(s: &Session) -> Vec<String> {
    let mut out: Vec<String> = s.me.iter().map(|a| a.trim().to_lowercase()).collect();
    for a in &s.mailbox.accounts {
        let addr = a.address.trim().to_lowercase();
        if !addr.is_empty() && !out.contains(&addr) {
            out.push(addr);
        }
    }
    out
}

/// Resolves `params.folder`: a folder id (number or numeric string), a folder path ("Inbox\\Projects") or a
/// folder name. Paths and names prefer the current account. `Ok(None)` when the param is absent.
pub(crate) fn param_folder(s: &Session, p: &Value) -> Result<Option<FolderId>, CmdError> {
    let v = match p.get("folder") {
        None | Some(Value::Null) => return Ok(None),
        Some(v) => v,
    };
    let bad = || CmdError::Params(format!("no such folder {v}"));
    if let Some(n) = v.as_u64() {
        let id = FolderId(n);
        return if s.mailbox.folders.contains_key(&id) { Ok(Some(id)) } else { Err(bad()) };
    }
    let Some(text) = v.as_str() else {
        return Err(CmdError::Params("`folder` must be a folder id or name".into()));
    };
    let text = text.trim().trim_start_matches('\\').trim();
    if let Ok(n) = text.parse::<u64>()
        && s.mailbox.folders.contains_key(&FolderId(n))
    {
        return Ok(Some(FolderId(n)));
    }
    let current = s.current_account();
    let mut candidates: Vec<&mailcraft_model::Folder> = s.mailbox.folders.values().collect();
    // Current account first, then the rest in id order.
    candidates.sort_by_key(|f| (Some(f.account) != current, f.id));
    let by_path = candidates.iter().find(|f| s.mailbox.folder_path(f.id).eq_ignore_ascii_case(text));
    let by_name = || candidates.iter().find(|f| f.name.eq_ignore_ascii_case(text));
    by_path.or_else(by_name).map(|f| Some(f.id)).ok_or_else(bad)
}

/// List order of the selected folder (newest first), used to pick the neighbour after items leave it.
fn list_order(s: &Session) -> Vec<ItemId> {
    let mut items = s.mailbox.items_in(s.selection.folder);
    items.sort_by(|a, b| b.received.unix.cmp(&a.received.unix).then(b.id.cmp(&a.id)));
    items.into_iter().map(|m| m.id).collect()
}

/// After items left the selected folder: drops them from the selection and, when nothing stays selected,
/// selects the neighbouring item (the next one below, else the one above), as Delete does.
fn reselect(s: &mut Session, order_before: &[ItemId]) {
    let folder = s.selection.folder;
    let present = |s: &Session, id: &ItemId| s.mailbox.mail.get(id).is_some_and(|m| m.folder == folder);
    let selected: BTreeSet<ItemId> = s.selection.items.iter().copied().chain(s.selection.focus).collect();
    let gone: Vec<usize> = order_before
        .iter()
        .enumerate()
        .filter(|(_, id)| selected.contains(id) && !present(s, id))
        .map(|(i, _)| i)
        .collect();
    let kept: Vec<ItemId> = s.selection.items.iter().copied().filter(|id| present(s, id)).collect();
    s.selection.items = kept;
    if s.selection.focus.is_some_and(|f| !present(s, &f)) {
        s.selection.focus = None;
    }
    if s.selection.items.is_empty()
        && let (Some(&lo), Some(&hi)) = (gone.iter().min(), gone.iter().max())
    {
        let after = order_before.iter().skip(hi.saturating_add(1)).find(|id| present(s, id));
        let before = order_before.iter().take(lo).rev().find(|id| present(s, id));
        if let Some(&n) = after.or(before) {
            s.selection.items = vec![n];
            s.selection.focus = Some(n);
        }
    }
    if s.selection.focus.is_none() {
        s.selection.focus = s.selection.items.first().copied();
    }
}

/// Moves `ids` to `to`, journals, reselects; returns the moved ids.
fn move_and_reselect(s: &mut Session, ids: &[ItemId], to: FolderId) -> Result<Vec<ItemId>, CmdError> {
    let order = list_order(s);
    let moved = s.mailbox.move_items(ids, to)?;
    journal(s, &moved);
    reselect(s, &order);
    Ok(moved)
}

/// Moves each item to the `role` folder of its own account; items without such a folder are skipped.
/// Returns the moved ids.
fn move_to_role(s: &mut Session, ids: &[ItemId], role: FolderRole) -> Result<Vec<ItemId>, CmdError> {
    let order = list_order(s);
    let mut moved = Vec::new();
    for id in ids {
        if let Some(dest) = role_folder_of(s, *id, role) {
            moved.extend(s.mailbox.move_items(&[*id], dest)?);
        }
    }
    journal(s, &moved);
    reselect(s, &order);
    Ok(moved)
}

fn ids_json(ids: &[ItemId]) -> Value {
    Value::Array(ids.iter().map(|i| json!(i.0)).collect())
}

/// Conversations (threaded over the whole account) of the given items.
pub(crate) fn conversations_of(s: &Session, ids: &[ItemId]) -> Vec<Conversation> {
    let accounts: BTreeSet<AccountId> = ids.iter().filter_map(|id| item_account(s, *id)).collect();
    let mut out = Vec::new();
    for account in accounts {
        let items: Vec<&MailItem> =
            s.mailbox.mail.values().filter(|m| s.mailbox.folder_account(m.folder) == Some(account)).collect();
        out.extend(thread(&items).into_iter().filter(|c| c.items.iter().any(|i| ids.contains(i))));
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------------------------------------

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
        // New
        spec(
            "mail.newEmail",
            "New Email",
            "Home › New",
            "Mod+Shift+M",
            r#"{"format"?:"html"|"rich"|"plain","pickStationery"?:bool}"#,
            false,
            always,
            new_email,
        ),
        spec(
            "mail.newItem",
            "New Items",
            "Home › New › New Items",
            "",
            r#"{"kind":"appointment"|"meeting"|"contact"|"task"|"post"|"contactGroup"|"taskRequest"|"fromForm"}"#,
            false,
            always,
            new_item,
        ),
        // Delete group
        spec(
            "mail.delete",
            "Delete",
            "Home › Delete",
            "Delete / Mod+D",
            r#"{"ids"?:[number]}"#,
            true,
            need_items,
            delete,
        ),
        spec(
            "mail.permanentDelete",
            "Permanently Delete",
            "Home › Delete",
            "Shift+Delete",
            r#"{"ids"?:[number]}"#,
            true,
            need_items,
            permanent_delete,
        ),
        spec(
            "mail.archive",
            "Archive",
            "Home › Delete",
            "",
            r#"{"folder"?:number|string}"#,
            true,
            need_items,
            archive,
        ),
        spec("mail.ignore", "Ignore", "Home › Delete", "Mod+Delete", "", true, need_one, ignore),
        spec(
            "mail.cleanUpConversation",
            "Clean Up Conversation",
            "Home › Delete › Clean Up",
            "",
            "",
            true,
            need_items,
            clean_up_conversation,
        ),
        spec(
            "mail.cleanUpFolder",
            "Clean Up Folder",
            "Home › Delete › Clean Up; Folder › Clean Up › Clean Up Folder",
            "",
            r#"{"folder"?:number|string}"#,
            true,
            need_mail_folder,
            clean_up_folder,
        ),
        spec(
            "mail.cleanUpSubfolders",
            "Clean Up Folder & Subfolders",
            "Home › Delete › Clean Up; Folder › Clean Up › Clean Up Folder",
            "",
            r#"{"folder"?:number|string}"#,
            true,
            need_mail_folder,
            clean_up_subfolders,
        ),
        spec(
            "mail.junk",
            "Junk E-mail Options",
            "Home › Delete › Block",
            "",
            r#"{"action":"options"|"moveToJunk"|"notJunk"}"#,
            true,
            always,
            junk,
        ),
        spec(
            "mail.block",
            "Block",
            "Home › Delete › Block",
            "",
            r#"{"kind":"sender"|"neverSender"|"neverDomain"|"neverList"}"#,
            true,
            need_one,
            block,
        ),
        spec(
            "mail.report",
            "Report",
            "Home › Report",
            "",
            r#"{"kind"?:"junk"|"phishing"}"#,
            true,
            need_items,
            report,
        ),
        // Move group
        spec(
            "mail.move",
            "Move to Folder",
            "Home › Move › Move",
            "Mod+Shift+V",
            r#"{"folder"?:number|string}"#,
            true,
            need_items,
            move_to_folder,
        ),
        spec(
            "mail.copy",
            "Copy to Folder",
            "Home › Move › Move",
            "",
            r#"{"folder"?:number|string}"#,
            true,
            need_items,
            copy_to_folder,
        ),
        spec(
            "mail.alwaysMoveConversation",
            "Always Move Conversation",
            "Home › Move › Move",
            "",
            r#"{"folder"?:number|string}"#,
            true,
            need_one,
            always_move_conversation,
        ),
        spec(
            "mail.moveToOther",
            "Move to Other",
            "Message context menu (Focused Inbox)",
            "",
            "",
            true,
            need_items,
            move_to_other,
        ),
        spec(
            "mail.alwaysMoveToOther",
            "Always Move to Other",
            "Message context menu (Focused Inbox)",
            "",
            "",
            true,
            need_one,
            always_move_to_other,
        ),
        // Tags group
        spec(
            "mail.markRead",
            "Mark as Read",
            "Group header context menu; Home › Tags",
            "Mod+Q",
            r#"{"ids"?:[number]}"#,
            true,
            need_items,
            mark_read,
        ),
        spec(
            "mail.markUnread",
            "Mark as Unread",
            "Message/group context menu; Home › Tags",
            "Mod+U",
            r#"{"ids"?:[number]}"#,
            true,
            need_items,
            mark_unread,
        ),
        spec(
            "mail.toggleRead",
            "Unread/Read",
            "Home › Tags",
            "",
            r#"{"ids"?:[number]}"#,
            true,
            need_items,
            toggle_read,
        ),
        spec(
            "mail.markAllRead",
            "Mark All as Read",
            "Folder context menu; Folder › Clean Up",
            "",
            r#"{"folder"?:number|string}"#,
            true,
            need_mail_folder,
            mark_all_read,
        ),
        spec(
            "mail.flag",
            "Follow Up",
            "Home › Tags › Follow Up",
            "Insert",
            r#"{"kind"?:"today"|"tomorrow"|"thisWeek"|"nextWeek"|"noDate"|"complete"|"clear"}"#,
            true,
            need_items,
            flag,
        ),
        spec(
            "mail.flagCustom",
            "Custom Flag",
            "Home › Tags › Follow Up",
            "Mod+Shift+G",
            r#"{"reminder"?:bool|number,"label"?:string,"start"?:number,"due"?:number}"#,
            true,
            need_items,
            flag_custom,
        ),
        spec(
            "mail.addReminder",
            "Add Reminder",
            "Home › Tags › Follow Up",
            "",
            r#"{"at"?:number}"#,
            true,
            need_items,
            add_reminder,
        ),
        spec(
            "mail.markComplete",
            "Mark Complete",
            "Home › Tags › Follow Up",
            "",
            "",
            true,
            need_items,
            mark_complete,
        ),
        spec("mail.clearFlag", "Clear Flag", "Home › Tags › Follow Up", "", "", true, need_items, clear_flag),
        spec(
            "mail.flagQuickClick",
            "Set Quick Click (flag)",
            "Home › Tags › Follow Up",
            "",
            r#"{"kind":string}"#,
            false,
            always,
            set_quick_click_flag,
        ),
        spec(
            "mail.setQuickClickFlag",
            "Set Quick Click Flag",
            "Home › Tags › Follow Up",
            "",
            r#"{"kind"?:string}"#,
            false,
            always,
            set_quick_click_flag,
        ),
        spec(
            "mail.categorize",
            "Categorize",
            "Home › Tags › Categorize",
            "",
            r#"{"name"?:string}"#,
            true,
            need_items,
            categorize,
        ),
        spec(
            "mail.clearCategories",
            "Clear All Categories",
            "Home › Tags › Categorize",
            "",
            "",
            true,
            need_items,
            clear_categories,
        ),
        spec(
            "mail.allCategories",
            "All Categories",
            "Home › Tags › Categorize",
            "",
            "",
            false,
            always,
            all_categories,
        ),
        spec(
            "mail.importance",
            "Importance",
            "Home › Tags",
            "",
            r#"{"level":"high"|"normal"|"low"}"#,
            true,
            need_items,
            importance,
        ),
        spec(
            "mail.private",
            "Private",
            "Home › Tags",
            "",
            r#"{"ids"?:[number]}"#,
            true,
            need_private_targets,
            private,
        ),
        spec(
            "mail.togglePrivate",
            "Private",
            "People › Home › Tags",
            "",
            r#"{"ids"?:[number]}"#,
            true,
            need_private_targets,
            private,
        ),
        // Find
        spec(
            "mail.findRelated",
            "Find Related",
            "Message context › Find Related",
            "",
            r#"{"kind":"conversation"|"sender"}"#,
            false,
            need_one,
            find_related,
        ),
        // Open / print / actions
        spec(
            "mail.open",
            "Open",
            "Message context menu",
            "Mod+O",
            r#"{"ids"?:[number]}"#,
            false,
            need_one,
            open,
        ),
        spec(
            "mail.print",
            "Print",
            "Message context menu; File › Print",
            "",
            r#"{"ids"?:[number], (with "style", "printer" or "path": as print.print)}"#,
            false,
            need_items,
            print,
        ),
        spec(
            "mail.quickPrint",
            "Quick Print",
            "Message context menu",
            "",
            r#"{"ids"?:[number], "printer"?: string, "path"?: string (PDF)}"#,
            false,
            need_items,
            quick_print,
        ),
        spec(
            "mail.editMessage",
            "Edit Message",
            "Message › More Commands › Actions",
            "",
            r#"{"subject"?:string,"text"?:string,"html"?:string}"#,
            true,
            need_one,
            edit_message,
        ),
        spec(
            "mail.messageOptions",
            "Message Options",
            "Message › Tags (dialog launcher)",
            "",
            r#"{"importance"?:"high"|"normal"|"low","sensitivity"?:"normal"|"personal"|"private"|"confidential"}"#,
            true,
            need_one,
            message_options,
        ),
        spec(
            "mail.viewSource",
            "View Source",
            "Message › More Commands › Actions › Other Actions",
            "",
            "",
            false,
            need_one,
            view_source,
        ),
        spec(
            "mail.recall",
            "Recall This Message",
            "Message › More Commands › Actions",
            "",
            "",
            false,
            need_recallable,
            recall,
        ),
        spec(
            "mail.resend",
            "Resend This Message",
            "Message › More Commands › Actions",
            "",
            "",
            false,
            need_sent_item,
            resend,
        ),
    ]
}

// ---------------------------------------------------------------------------------------------------------
// Enabled predicates
// ---------------------------------------------------------------------------------------------------------

fn need_mail_folder(s: &Session) -> Option<&'static str> {
    match s.mailbox.folder(s.selection.folder) {
        Some(f) if f.kind == FolderKind::Mail => None,
        _ => Some("No mail folder is selected."),
    }
}

fn need_private_targets(s: &Session) -> Option<&'static str> {
    let any = s
        .target_ids(&Value::Null)
        .iter()
        .any(|id| s.mailbox.mail.contains_key(id) || s.mailbox.appointments.contains_key(id));
    if any { None } else { Some("No item is selected.") }
}

fn need_sent_item(s: &Session) -> Option<&'static str> {
    match primary_item(s, &Value::Null) {
        Some(id) if item_role(s, id) == Some(FolderRole::SentItems) => None,
        _ => Some("Only sent messages can be resent."),
    }
}

fn need_recallable(s: &Session) -> Option<&'static str> {
    if need_sent_item(s).is_some() {
        return Some("Only sent messages can be recalled.");
    }
    let exchange = primary_item(s, &Value::Null)
        .and_then(|id| item_account(s, id))
        .and_then(|a| s.mailbox.account(a))
        .is_some_and(|a| a.kind == AccountKind::Exchange);
    if exchange { None } else { Some("Recall needs a server that supports it.") }
}

// ---------------------------------------------------------------------------------------------------------
// New
// ---------------------------------------------------------------------------------------------------------

/// "Name <address>" of the default (or given) account, empty when it has no address.
pub(crate) fn account_from(s: &Session, account: Option<AccountId>) -> String {
    let acc = account.and_then(|a| s.mailbox.account(a)).or_else(|| s.mailbox.default_account());
    match acc {
        Some(a) if !a.address.trim().is_empty() => {
            let name =
                if a.sender_name.trim().is_empty() { a.display_name.trim() } else { a.sender_name.trim() };
            crate::cmd::compose::field_mailbox(&mailcraft_model::Address::new(Some(name), a.address.trim()))
        }
        _ => String::new(),
    }
}

fn parse_format(v: Option<&str>) -> Result<BodyFormat, CmdError> {
    match v.map(str::to_ascii_lowercase).as_deref() {
        None | Some("html") => Ok(BodyFormat::Html),
        Some("rich") | Some("richtext") => Ok(BodyFormat::RichText),
        Some("plain") | Some("text") | Some("plaintext") => Ok(BodyFormat::PlainText),
        Some(other) => Err(CmdError::Params(format!("unknown format `{other}`"))),
    }
}

fn new_email(s: &mut Session, p: &Value) -> CmdResult {
    let format = parse_format(opt_str(p, "format"))?;
    if opt_bool(p, "pickStationery") == Some(true) {
        return open_dialog(s, "stationery", json!({}));
    }
    let c = ComposeState {
        kind: ComposeKind::New,
        from: account_from(s, s.mailbox.default_account().map(|a| a.id)),
        format,
        inline: false,
        ..Default::default()
    };
    let c = crate::compose::with_default_signature(&s.mailbox, c);
    let id = s.open_compose(c);
    Ok(json!({"compose": id}))
}

fn new_item(s: &mut Session, p: &Value) -> CmdResult {
    let kind = req_str(p, "kind")?;
    let dialog = match kind {
        "appointment" => "newAppointment",
        "meeting" => "newMeeting",
        "contact" => "newContact",
        "task" => "newTask",
        "post" => "newPost",
        "contactGroup" => "newContactGroup",
        "taskRequest" => "newTaskRequest",
        "fromForm" => "chooseForm",
        "email" | "message" => return new_email(s, p),
        other => return Err(CmdError::Params(format!("unknown item kind `{other}`"))),
    };
    let folder = s.selection.folder.0;
    open_dialog(s, dialog, json!({"kind": kind, "folder": folder}))
}

// ---------------------------------------------------------------------------------------------------------
// Delete / archive / move / copy
// ---------------------------------------------------------------------------------------------------------

fn delete(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    if ids.is_empty() {
        return Err(CmdError::Disabled("No message is selected.".into()));
    }
    let order = list_order(s);
    let removed = s.mailbox.delete_items(&ids)?;
    let touched: Vec<ItemId> = removed.iter().map(|m| m.id).collect();
    journal(s, &touched);
    reselect(s, &order);
    Ok(json!({"deleted": touched.len(), "ids": ids_json(&touched)}))
}

fn permanent_delete(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let order = list_order(s);
    let mut removed = Vec::new();
    for id in &ids {
        if s.mailbox.mail.remove(id).is_some() {
            removed.push(*id);
        }
    }
    journal(s, &removed);
    reselect(s, &order);
    Ok(json!({"deleted": removed.len(), "ids": ids_json(&removed)}))
}

fn archive(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    if let Some(folder) = param_folder(s, p)? {
        let moved = move_and_reselect(s, &ids, folder)?;
        return Ok(json!({"moved": moved.len(), "folder": folder.0}));
    }
    let missing = ids.iter().any(|id| role_folder_of(s, *id, FolderRole::Archive).is_none());
    if missing {
        return open_dialog(s, "archiveFolder", json!({"ids": ids_json(&ids)}));
    }
    let moved = move_to_role(s, &ids, FolderRole::Archive)?;
    Ok(json!({"moved": moved.len(), "ids": ids_json(&moved)}))
}

fn move_to_folder(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let Some(folder) = param_folder(s, p)? else {
        return open_dialog(s, "moveToFolder", json!({"ids": ids_json(&ids)}));
    };
    let moved = move_and_reselect(s, &ids, folder)?;
    Ok(json!({"moved": moved.len(), "folder": folder.0, "ids": ids_json(&moved)}))
}

fn copy_to_folder(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let Some(folder) = param_folder(s, p)? else {
        return open_dialog(s, "copyToFolder", json!({"ids": ids_json(&ids)}));
    };
    match s.mailbox.folder(folder) {
        Some(f) if f.kind == FolderKind::Mail => {}
        Some(f) => return Err(CmdError::Failed(format!("mail items cannot be copied into \"{}\"", f.name))),
        None => return Err(CmdError::Params(format!("no such folder {}", folder.0))),
    }
    let mut copies = Vec::new();
    for id in &ids {
        if let Some(item) = s.mailbox.mail.get(id).cloned() {
            let mut copy = item;
            copy.folder = folder;
            copy.remote_id = None;
            copies.push(s.mailbox.add_mail(copy));
        }
    }
    journal(s, &copies);
    Ok(json!({"copied": copies.len(), "folder": folder.0, "ids": ids_json(&copies)}))
}

fn always_move_conversation(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let Some(folder) = param_folder(s, p)? else {
        return open_dialog(s, "alwaysMoveConversation", json!({"ids": [focus.0]}));
    };
    let convs = conversations_of(s, &[focus]);
    let members: Vec<ItemId> = convs.iter().flat_map(|c| c.items.iter().copied()).collect();
    let topic = convs.first().map(|c| c.topic.clone()).unwrap_or_default();
    // Drafts and outgoing mail stay where they are.
    let movable: Vec<ItemId> = members
        .into_iter()
        .filter(|id| {
            !matches!(
                item_role(s, *id),
                Some(FolderRole::Drafts | FolderRole::Outbox | FolderRole::SentItems)
            )
        })
        .collect();
    let moved = move_and_reselect(s, &movable, folder)?;
    // Future messages: a rule on the conversation topic keeps moving them.
    if !topic.trim().is_empty() {
        let name = format!("Always move conversation: {topic}");
        let rule_id = s.mailbox.rules.iter().map(|r| r.id).max().unwrap_or(0).saturating_add(1);
        s.mailbox.rules.retain(|r| r.name != name);
        s.mailbox.rules.push(mailcraft_model::Rule {
            id: rule_id,
            name,
            enabled: true,
            conditions: vec![mailcraft_model::RuleCondition::SubjectContains(vec![topic.clone()])],
            actions: vec![
                mailcraft_model::RuleAction::MoveTo(folder),
                mailcraft_model::RuleAction::StopProcessingMoreRules,
            ],
            exceptions: Vec::new(),
        });
        journal_mailbox(s);
    }
    Ok(json!({"moved": moved.len(), "folder": folder.0, "topic": topic}))
}

// ---------------------------------------------------------------------------------------------------------
// Focused / Other
// ---------------------------------------------------------------------------------------------------------

fn set_focus_class(s: &mut Session, ids: &[ItemId], class: FocusClass) -> Vec<ItemId> {
    let mut changed = Vec::new();
    for id in ids {
        if let Some(m) = s.mailbox.mail.get_mut(id)
            && m.focus != class
        {
            m.focus = class;
            changed.push(*id);
        }
    }
    journal(s, &changed);
    changed
}

fn move_to_other(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let changed = set_focus_class(s, &ids, FocusClass::Other);
    Ok(json!({"changed": changed.len(), "ids": ids_json(&changed)}))
}

fn always_move_to_other(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let Some((sender, folder)) = s
        .mailbox
        .mail
        .get(&focus)
        .map(|m| (m.from.as_ref().map(|a| a.addr.clone()).unwrap_or_default(), m.folder))
    else {
        return Err(CmdError::Failed("message not found".into()));
    };
    // Every message from the sender in this folder goes to Other.
    let mut ids: Vec<ItemId> = vec![focus];
    if !sender.is_empty() {
        ids.extend(
            s.mailbox
                .mail
                .values()
                .filter(|m| {
                    m.folder == folder
                        && m.from.as_ref().is_some_and(|a| a.addr.eq_ignore_ascii_case(&sender))
                })
                .map(|m| m.id),
        );
    }
    let changed = set_focus_class(s, &ids, FocusClass::Other);
    Ok(json!({"changed": changed.len(), "sender": sender}))
}

// ---------------------------------------------------------------------------------------------------------
// Read state
// ---------------------------------------------------------------------------------------------------------

fn set_read(s: &mut Session, ids: &[ItemId], read: bool) -> CmdResult {
    let changed = s.mailbox.set_read(ids, read);
    journal(s, &changed);
    Ok(json!({"changed": changed.len(), "read": read}))
}

fn mark_read(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    set_read(s, &ids, true)
}

fn mark_unread(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    set_read(s, &ids, false)
}

/// Unread/Read: when every selected item is read, marks them unread; otherwise marks them read.
fn toggle_read(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let all_read = ids.iter().all(|id| s.mailbox.mail.get(id).is_some_and(|m| m.read));
    set_read(s, &ids, !all_read)
}

fn mark_all_read(s: &mut Session, p: &Value) -> CmdResult {
    let folder = param_folder(s, p)?.unwrap_or(s.selection.folder);
    let ids: Vec<ItemId> = s.mailbox.items_in(folder).iter().map(|m| m.id).collect();
    set_read(s, &ids, true)
}

// ---------------------------------------------------------------------------------------------------------
// Follow-up flags
// ---------------------------------------------------------------------------------------------------------

/// Seconds per day.
const DAY: i64 = 86_400;

/// Local midnight of day number `day` as an instant in the session's offset.
fn day_start(s: &Session, day: i64) -> Option<DateTime> {
    let local = day.checked_mul(DAY)?;
    let unix = local.checked_sub(i64::from(s.tz_offset_min).checked_mul(60)?)?;
    Some(DateTime { unix, offset_min: s.tz_offset_min })
}

/// Flag kinds of the Follow Up menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlagKind {
    Today,
    Tomorrow,
    ThisWeek,
    NextWeek,
    NoDate,
}

impl FlagKind {
    fn parse(s: &str) -> Option<FlagKind> {
        match s.to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
            "today" => Some(FlagKind::Today),
            "tomorrow" => Some(FlagKind::Tomorrow),
            "thisweek" => Some(FlagKind::ThisWeek),
            "nextweek" => Some(FlagKind::NextWeek),
            "nodate" => Some(FlagKind::NoDate),
            _ => None,
        }
    }
}

/// (start, due) for a flag kind relative to `Session::now()`. Work week Monday–Friday; weeks start Sunday.
///
/// Today/Tomorrow: start = due = that day. This Week: due = this week's Friday (today when already past
/// it), start = two days from today but not after the due date. Next Week: Monday to Friday of next week.
pub(crate) fn flag_dates(s: &Session, kind: FlagKind) -> (Option<DateTime>, Option<DateTime>) {
    let today = local_day(s.now(), s.tz_offset_min);
    let weekday = i64::from(weekday_of_day(today).0); // Sunday = 0
    let week_start = today.saturating_sub(weekday);
    let (start, due) = match kind {
        FlagKind::NoDate => return (None, None),
        FlagKind::Today => (today, today),
        FlagKind::Tomorrow => (today.saturating_add(1), today.saturating_add(1)),
        FlagKind::ThisWeek => {
            let friday = week_start.saturating_add(5).max(today);
            (today.saturating_add(2).min(friday), friday)
        }
        FlagKind::NextWeek => (week_start.saturating_add(8), week_start.saturating_add(12)),
    };
    (day_start(s, start), day_start(s, due))
}

/// The flag a plain click / Insert applies (the Quick Click default: Today).
fn quick_click_flag(s: &Session) -> FlagState {
    let (start, due) = flag_dates(s, FlagKind::Today);
    FlagState::Flagged { label: "Follow up".into(), start, due, reminder: None }
}

fn set_flags(s: &mut Session, ids: &[ItemId], f: impl Fn(&FlagState) -> FlagState) -> Vec<ItemId> {
    let mut changed = Vec::new();
    for id in ids {
        if let Some(m) = s.mailbox.mail.get_mut(id) {
            let next = f(&m.flag);
            if next != m.flag {
                m.flag = next;
                changed.push(*id);
            }
        }
    }
    journal(s, &changed);
    changed
}

fn flag(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let now = s.now();
    let kind = opt_str(p, "kind");
    let changed = match kind {
        None => {
            // Insert: unflagged → flagged → complete (complete stays complete).
            let quick = quick_click_flag(s);
            set_flags(s, &ids, |f| match f {
                FlagState::None => quick.clone(),
                FlagState::Flagged { .. } => FlagState::Complete { completed: Some(now) },
                done @ FlagState::Complete { .. } => done.clone(),
            })
        }
        Some("complete") => set_flags(s, &ids, |_| FlagState::Complete { completed: Some(now) }),
        Some("clear") => set_flags(s, &ids, |_| FlagState::None),
        Some(k) => {
            let kind =
                FlagKind::parse(k).ok_or_else(|| CmdError::Params(format!("unknown flag kind `{k}`")))?;
            let (start, due) = flag_dates(s, kind);
            set_flags(s, &ids, |old| {
                let (label, reminder) = match old {
                    FlagState::Flagged { label, reminder, .. } => (label.clone(), *reminder),
                    _ => ("Follow up".to_string(), None),
                };
                FlagState::Flagged { label, start, due, reminder }
            })
        }
    };
    Ok(json!({"changed": changed.len(), "ids": ids_json(&changed)}))
}

/// Reads an instant param given as unix seconds.
fn opt_time(p: &Value, key: &str) -> Option<DateTime> {
    p.get(key).and_then(Value::as_i64).map(|unix| DateTime { unix, offset_min: 0 })
}

/// Custom flag: with `label`/`start`/`due`/`reminder` (unix seconds) applies them directly; otherwise opens
/// the Custom dialog.
fn flag_custom(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let has_values = ["label", "start", "due"].iter().any(|k| p.get(*k).is_some())
        || p.get("reminder").is_some_and(Value::is_i64);
    if !has_values {
        let reminder = opt_bool(p, "reminder").unwrap_or(false);
        return open_dialog(s, "customFlag", json!({"ids": ids_json(&ids), "reminder": reminder}));
    }
    let label = opt_str(p, "label").map(str::to_owned);
    let (start, due, reminder) = (opt_time(p, "start"), opt_time(p, "due"), opt_time(p, "reminder"));
    if let (Some(a), Some(b)) = (start, due)
        && a.unix > b.unix
    {
        return Err(CmdError::Params("start date is after the due date".into()));
    }
    let changed = set_flags(s, &ids, |old| {
        let old_label = match old {
            FlagState::Flagged { label, .. } => label.clone(),
            _ => "Follow up".to_string(),
        };
        FlagState::Flagged { label: label.clone().unwrap_or(old_label), start, due, reminder }
    });
    Ok(json!({"changed": changed.len()}))
}

/// Add Reminder: with `at` (unix seconds) sets the reminder (flagging unflagged items); otherwise opens the
/// Custom dialog with the reminder enabled.
fn add_reminder(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let Some(at) = opt_time(p, "at") else {
        return open_dialog(s, "customFlag", json!({"ids": ids_json(&ids), "reminder": true}));
    };
    let quick = quick_click_flag(s);
    let changed = set_flags(s, &ids, |old| match old {
        FlagState::Flagged { label, start, due, .. } => {
            FlagState::Flagged { label: label.clone(), start: *start, due: *due, reminder: Some(at) }
        }
        _ => match &quick {
            FlagState::Flagged { label, start, due, .. } => {
                FlagState::Flagged { label: label.clone(), start: *start, due: *due, reminder: Some(at) }
            }
            other => other.clone(),
        },
    });
    Ok(json!({"changed": changed.len()}))
}

fn mark_complete(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let now = s.now();
    let changed = set_flags(s, &ids, |old| match old {
        done @ FlagState::Complete { .. } => done.clone(),
        _ => FlagState::Complete { completed: Some(now) },
    });
    Ok(json!({"changed": changed.len()}))
}

fn clear_flag(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let changed = set_flags(s, &ids, |_| FlagState::None);
    Ok(json!({"changed": changed.len()}))
}

/// Set Quick Click: without `kind` opens the dialog; with a kind validates and reports it. There is no
/// session or mailbox field for the setting yet (same as `categories.quickClick`), so the caller (UI
/// profile) keeps the returned value; the engine's Insert/click default is Today.
fn set_quick_click_flag(s: &mut Session, p: &Value) -> CmdResult {
    match opt_str(p, "kind") {
        None => open_dialog(s, "setQuickClick", json!({"for": "flag"})),
        Some(k) => {
            if FlagKind::parse(k).is_none() && !k.eq_ignore_ascii_case("complete") {
                return Err(CmdError::Params(format!("unknown flag kind `{k}`")));
            }
            Ok(json!({"quickClick": k}))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Categories, importance, sensitivity
// ---------------------------------------------------------------------------------------------------------

/// Toggles a category on the selection: removed from all when every item has it, else added where missing.
/// Unknown names are added to the master list. Without a name the Quick Click category (the first one).
fn categorize(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    if ids.is_empty() {
        return Err(CmdError::Disabled("No message is selected.".into()));
    }
    let name = match opt_str(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.to_owned(),
        None => match s.mailbox.categories.first() {
            Some(c) => c.name.clone(),
            None => return open_dialog(s, "colorCategories", json!({"ids": ids_json(&ids)})),
        },
    };
    let name = match Category::find(&s.mailbox.categories, &name) {
        Some(c) => c.name.clone(),
        None => {
            s.mailbox.categories.push(Category {
                name: name.clone(),
                color: CategoryColor::None,
                shortcut: String::new(),
            });
            journal_mailbox(s);
            name
        }
    };
    let has = |m: &MailItem| m.categories.iter().any(|c| c.eq_ignore_ascii_case(&name));
    let all_have = ids.iter().all(|id| s.mailbox.mail.get(id).is_some_and(has));
    let mut changed = Vec::new();
    for id in &ids {
        if let Some(m) = s.mailbox.mail.get_mut(id) {
            if all_have {
                m.categories.retain(|c| !c.eq_ignore_ascii_case(&name));
                changed.push(*id);
            } else if !has(m) {
                m.categories.push(name.clone());
                changed.push(*id);
            }
        }
    }
    journal(s, &changed);
    Ok(json!({"category": name, "applied": !all_have, "changed": changed.len()}))
}

fn clear_categories(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let mut changed = Vec::new();
    for id in &ids {
        if let Some(m) = s.mailbox.mail.get_mut(id)
            && !m.categories.is_empty()
        {
            m.categories.clear();
            changed.push(*id);
        }
    }
    journal(s, &changed);
    Ok(json!({"changed": changed.len()}))
}

fn all_categories(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    open_dialog(s, "colorCategories", json!({"ids": ids_json(&ids)}))
}

fn parse_importance(v: &str) -> Result<Importance, CmdError> {
    match v.to_ascii_lowercase().as_str() {
        "high" => Ok(Importance::High),
        "normal" => Ok(Importance::Normal),
        "low" => Ok(Importance::Low),
        other => Err(CmdError::Params(format!("unknown importance `{other}`"))),
    }
}

/// High/Low Importance are toggles: when every selected item already has the level it returns to Normal.
fn importance(s: &mut Session, p: &Value) -> CmdResult {
    let level = parse_importance(req_str(p, "level")?)?;
    let ids = mail_targets(s, p);
    let all_have = ids.iter().all(|id| s.mailbox.mail.get(id).is_some_and(|m| m.importance == level));
    let target = if all_have { Importance::Normal } else { level };
    let mut changed = Vec::new();
    for id in &ids {
        if let Some(m) = s.mailbox.mail.get_mut(id)
            && m.importance != target
        {
            m.importance = target;
            changed.push(*id);
        }
    }
    journal(s, &changed);
    let label = match target {
        Importance::High => "high",
        Importance::Normal => "normal",
        Importance::Low => "low",
    };
    Ok(json!({"importance": label, "changed": changed.len()}))
}

/// Private toggle on mail items and appointments: on when any selected item is not private, else off.
fn private(s: &mut Session, p: &Value) -> CmdResult {
    let ids = s.target_ids(p);
    let is_private = |s: &Session, id: &ItemId| {
        s.mailbox
            .mail
            .get(id)
            .map(|m| m.sensitivity == Sensitivity::Private)
            .or_else(|| s.mailbox.appointments.get(id).map(|a| a.private))
    };
    let states: Vec<bool> = ids.iter().filter_map(|id| is_private(s, id)).collect();
    if states.is_empty() {
        return Err(CmdError::Disabled("No item is selected.".into()));
    }
    let on = !states.iter().all(|p| *p);
    let mut mail_changed = Vec::new();
    let mut other_changed = false;
    for id in &ids {
        if let Some(m) = s.mailbox.mail.get_mut(id) {
            m.sensitivity = if on { Sensitivity::Private } else { Sensitivity::Normal };
            mail_changed.push(*id);
        } else if let Some(a) = s.mailbox.appointments.get_mut(id) {
            a.private = on;
            other_changed = true;
        }
    }
    journal(s, &mail_changed);
    if other_changed {
        journal_mailbox(s);
    }
    Ok(json!({"private": on}))
}

// ---------------------------------------------------------------------------------------------------------
// Ignore and clean-up
// ---------------------------------------------------------------------------------------------------------

/// Ignore toggle on the focused message's conversation. On: every member is marked ignored and moved to
/// its account's Deleted Items (drafts and outgoing items excepted). Off: the marks are cleared and ignored
/// members in Deleted Items go back to the Inbox.
fn ignore(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let members: Vec<ItemId> = conversations_of(s, &[focus]).iter().flat_map(|c| c.items.clone()).collect();
    let currently = members.iter().any(|id| s.mailbox.mail.get(id).is_some_and(|m| m.ignored));
    let order = list_order(s);
    let mut touched = Vec::new();
    if currently {
        for id in &members {
            let back = item_role(s, *id) == Some(FolderRole::DeletedItems)
                && s.mailbox.mail.get(id).is_some_and(|m| m.ignored);
            if let Some(m) = s.mailbox.mail.get_mut(id) {
                m.ignored = false;
                touched.push(*id);
            }
            if back && let Some(inbox) = role_folder_of(s, *id, FolderRole::Inbox) {
                s.mailbox.move_items(&[*id], inbox)?;
            }
        }
    } else {
        for id in &members {
            let role = item_role(s, *id);
            if matches!(role, Some(FolderRole::Drafts | FolderRole::Outbox)) {
                continue;
            }
            if let Some(m) = s.mailbox.mail.get_mut(id) {
                m.ignored = true;
                touched.push(*id);
            }
            if let Some(deleted) = role_folder_of(s, *id, FolderRole::DeletedItems) {
                s.mailbox.move_items(&[*id], deleted)?;
            }
        }
    }
    journal(s, &touched);
    reselect(s, &order);
    Ok(json!({"ignored": !currently, "changed": touched.len()}))
}

/// Body text normalised for "fully quoted" comparison: quote markers removed, whitespace collapsed.
fn normalised_text(m: &MailItem) -> String {
    let text = m.body_text();
    let mut out = String::new();
    for line in text.lines() {
        let line = line.trim_start_matches(|c: char| c == '>' || c.is_whitespace());
        for word in line.split_whitespace() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(word);
        }
    }
    out
}

fn is_signed(m: &MailItem) -> bool {
    m.attachments.iter().any(|a| {
        let t = a.mime_type.to_ascii_lowercase();
        t.contains("pkcs7-signature") || t.contains("pgp-signature")
    })
}

/// Members of `conv` passing `candidate` whose text is fully contained in a later member (Conversation
/// Clean Up defaults: categorized, flagged and signed messages are kept; drafts are never moved).
fn redundant_members(s: &Session, conv: &Conversation, candidate: &dyn Fn(&MailItem) -> bool) -> Vec<ItemId> {
    let members: Vec<(&MailItem, String)> =
        conv.items.iter().filter_map(|id| s.mailbox.mail.get(id)).map(|m| (m, normalised_text(m))).collect();
    let mut out = Vec::new();
    for (m, text) in &members {
        if text.is_empty()
            || m.is_draft
            || !candidate(m)
            || !m.categories.is_empty()
            || m.flag != FlagState::None
            || is_signed(m)
        {
            continue;
        }
        let quoted_later = members.iter().any(|(n, ntext)| {
            n.id != m.id
                && (n.received.unix > m.received.unix || (n.received.unix == m.received.unix && n.id > m.id))
                && ntext.len() > text.len()
                && ntext.contains(text.as_str())
        });
        if quoted_later {
            out.push(m.id);
        }
    }
    out
}

/// Moves redundant items to their account's Deleted Items.
fn clean_up(s: &mut Session, convs: &[Conversation], candidate: &dyn Fn(&MailItem) -> bool) -> CmdResult {
    let mut redundant = Vec::new();
    for c in convs {
        redundant.extend(redundant_members(s, c, candidate));
    }
    let moved = move_to_role(s, &redundant, FolderRole::DeletedItems)?;
    Ok(json!({"moved": moved.len(), "ids": ids_json(&moved)}))
}

fn clean_up_conversation(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let convs = conversations_of(s, &ids);
    let roles: Vec<(FolderId, Option<FolderRole>)> =
        s.mailbox.folders.values().map(|f| (f.id, f.role)).collect();
    let skip = move |m: &MailItem| {
        let role = roles.iter().find(|(id, _)| *id == m.folder).and_then(|(_, r)| *r);
        !matches!(
            role,
            Some(
                FolderRole::Drafts
                    | FolderRole::Outbox
                    | FolderRole::SentItems
                    | FolderRole::DeletedItems
                    | FolderRole::JunkEmail
            )
        )
    };
    clean_up(s, &convs, &skip)
}

fn clean_up_folders(s: &mut Session, folders: Vec<FolderId>) -> CmdResult {
    let folders: BTreeSet<FolderId> = folders
        .into_iter()
        .filter(|f| {
            s.mailbox.folder(*f).is_some_and(|f| {
                f.kind == FolderKind::Mail
                    && !matches!(
                        f.role,
                        Some(FolderRole::Drafts | FolderRole::Outbox | FolderRole::DeletedItems)
                    )
            })
        })
        .collect();
    let ids: Vec<ItemId> =
        s.mailbox.mail.values().filter(|m| folders.contains(&m.folder)).map(|m| m.id).collect();
    let convs = conversations_of(s, &ids);
    clean_up(s, &convs, &|m: &MailItem| folders.contains(&m.folder))
}

fn clean_up_folder(s: &mut Session, p: &Value) -> CmdResult {
    let folder = param_folder(s, p)?.unwrap_or(s.selection.folder);
    clean_up_folders(s, vec![folder])
}

fn clean_up_subfolders(s: &mut Session, p: &Value) -> CmdResult {
    let folder = param_folder(s, p)?.unwrap_or(s.selection.folder);
    let tree = s.mailbox.subtree(folder);
    clean_up_folders(s, tree)
}

// ---------------------------------------------------------------------------------------------------------
// Junk
// ---------------------------------------------------------------------------------------------------------

fn not_junk(s: &mut Session, ids: &[ItemId]) -> Result<Vec<ItemId>, CmdError> {
    let in_junk: Vec<ItemId> =
        ids.iter().copied().filter(|id| item_role(s, *id) == Some(FolderRole::JunkEmail)).collect();
    move_to_role(s, &in_junk, FolderRole::Inbox)
}

fn junk(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    match opt_str(p, "action").unwrap_or("options") {
        "options" => open_dialog(s, "junkOptions", json!({})),
        "moveToJunk" | "junk" => {
            let moved = move_to_role(s, &ids, FolderRole::JunkEmail)?;
            Ok(json!({"moved": moved.len()}))
        }
        "notJunk" => {
            let moved = not_junk(s, &ids)?;
            Ok(json!({"moved": moved.len()}))
        }
        other => Err(CmdError::Params(format!("unknown junk action `{other}`"))),
    }
}

/// Block Sender moves the message to Junk Email; the "never block" kinds treat it as not junk (back to the
/// Inbox when it is in Junk Email). The safe/blocked lists themselves live in the Junk E-mail Options
/// dialog; the result names the address or domain for it.
fn block(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let kind = req_str(p, "kind")?;
    let Some(item) = s.mailbox.mail.get(&focus) else {
        return Err(CmdError::Failed("message not found".into()));
    };
    let sender = item.from.as_ref().map(|a| a.addr.clone()).unwrap_or_default();
    let ids = mail_targets(s, p);
    match kind {
        "sender" => {
            let moved = move_to_role(s, &ids, FolderRole::JunkEmail)?;
            Ok(json!({"blocked": sender, "moved": moved.len()}))
        }
        "neverSender" | "neverDomain" | "neverList" => {
            let entry = match kind {
                "neverSender" => sender,
                "neverDomain" => sender.rsplit_once('@').map(|(_, d)| format!("@{d}")).unwrap_or_default(),
                _ => item.to.first().map(|a| a.addr.clone()).unwrap_or_default(),
            };
            let moved = not_junk(s, &ids)?;
            Ok(json!({"safe": entry, "kind": kind, "moved": moved.len()}))
        }
        other => Err(CmdError::Params(format!("unknown block kind `{other}`"))),
    }
}

/// Report Junk / Report Phishing: marks the messages read and moves them to Junk Email.
fn report(s: &mut Session, p: &Value) -> CmdResult {
    let kind = opt_str(p, "kind").unwrap_or("junk");
    if !matches!(kind, "junk" | "phishing") {
        return Err(CmdError::Params(format!("unknown report kind `{kind}`")));
    }
    let ids = mail_targets(s, p);
    let read = s.mailbox.set_read(&ids, true);
    journal(s, &read);
    let moved = move_to_role(s, &ids, FolderRole::JunkEmail)?;
    Ok(json!({"reported": kind, "moved": moved.len()}))
}

// ---------------------------------------------------------------------------------------------------------
// Find related, open, print, actions
// ---------------------------------------------------------------------------------------------------------

fn quote_term(s: &str) -> String {
    s.replace('"', "")
}

fn find_related(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let Some(item) = s.mailbox.mail.get(&focus) else {
        return Err(CmdError::Failed("message not found".into()));
    };
    let query = match opt_str(p, "kind").unwrap_or("conversation") {
        "conversation" => format!("subject:\"{}\"", quote_term(strip_prefixes(&item.subject))),
        "sender" => match &item.from {
            Some(a) => format!("from:\"{}\"", quote_term(&a.addr)),
            None => return Err(CmdError::Failed("the message has no sender".into())),
        },
        other => return Err(CmdError::Params(format!("unknown kind `{other}`"))),
    };
    s.view.search = query.clone();
    s.view.search_scope = SearchScope::CurrentMailbox;
    Ok(json!({"search": query}))
}

fn open(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    // Opening a message reads it.
    let changed = s.mailbox.set_read(&[focus], true);
    journal(s, &changed);
    open_dialog(s, "readWindow", json!({"id": focus.0}))
}

fn print(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let raw: Vec<u64> = ids.iter().map(|i| i.0).collect();
    if let Some(done) = crate::cmd::print::forward(s, p, &raw) {
        return done;
    }
    open_dialog(s, "print", json!({"ids": ids_json(&ids), "quick": false}))
}

/// Quick Print: Memo Style straight to `printer` (or a PDF `path`) when given; otherwise the Print page with
/// Memo Style picked (there is no silent default printer: Save as PDF needs a file name).
fn quick_print(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let raw: Vec<u64> = ids.iter().map(|i| i.0).collect();
    if ["printer", "path", "pdf_path"].iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null())) {
        let mut params = p.as_object().cloned().unwrap_or_default();
        params.entry("style").or_insert_with(|| json!("memo"));
        let params = Value::Object(params);
        if let Some(done) = crate::cmd::print::forward(s, &params, &raw) {
            return done;
        }
    }
    open_dialog(s, "print", json!({"ids": ids_json(&ids), "quick": true, "style": "memo"}))
}

/// Edit Message: with `subject`/`text`/`html` saves the edits to the stored item; otherwise opens the read
/// window in edit mode.
fn edit_message(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let (subject, text, html) = (opt_str(p, "subject"), opt_str(p, "text"), opt_str(p, "html"));
    if subject.is_none() && text.is_none() && html.is_none() {
        return open_dialog(s, "readWindow", json!({"id": focus.0, "edit": true}));
    }
    let Some(m) = s.mailbox.mail.get_mut(&focus) else {
        return Err(CmdError::Failed("message not found".into()));
    };
    if let Some(subj) = subject {
        m.subject = subj.to_owned();
    }
    match (text, html) {
        (_, Some(h)) => {
            m.body.html = Some(h.to_owned());
            m.body.text = Some(text.map_or_else(|| mailcraft_html::html_to_text(h), str::to_owned));
        }
        (Some(t), None) => {
            m.body.text = Some(t.to_owned());
            if m.body.html.is_some() {
                m.body.html = Some(mailcraft_html::text_to_html(t));
            }
        }
        (None, None) => {}
    }
    m.refresh_preview();
    // The stored source no longer matches the edited item.
    m.raw = None;
    journal(s, &[focus]);
    Ok(ok())
}

fn parse_sensitivity(v: &str) -> Result<Sensitivity, CmdError> {
    match v.to_ascii_lowercase().as_str() {
        "normal" => Ok(Sensitivity::Normal),
        "personal" => Ok(Sensitivity::Personal),
        "private" => Ok(Sensitivity::Private),
        "confidential" => Ok(Sensitivity::Confidential),
        other => Err(CmdError::Params(format!("unknown sensitivity `{other}`"))),
    }
}

/// Message Options: applies `importance`/`sensitivity` when given; otherwise opens the properties dialog.
fn message_options(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let imp = opt_str(p, "importance").map(parse_importance).transpose()?;
    let sens = opt_str(p, "sensitivity").map(parse_sensitivity).transpose()?;
    if imp.is_none() && sens.is_none() {
        return open_dialog(s, "messageOptions", json!({"id": focus.0}));
    }
    if let Some(m) = s.mailbox.mail.get_mut(&focus) {
        if let Some(i) = imp {
            m.importance = i;
        }
        if let Some(x) = sens {
            m.sensitivity = x;
        }
    }
    journal(s, &[focus]);
    Ok(ok())
}

/// RFC 5322 source of an item: the stored original, else one rebuilt from its fields.
pub(crate) fn item_source(m: &MailItem) -> Vec<u8> {
    if let Some(raw) = &m.raw
        && !raw.is_empty()
    {
        return raw.clone();
    }
    let mb = |list: &[mailcraft_model::Address]| {
        list.iter().cloned().map(mailcraft_mime::Address::Mailbox).collect::<Vec<_>>()
    };
    let b = mailcraft_mime::MessageBuilder {
        from: m.from.clone(),
        sender: m.sender.clone(),
        to: mb(&m.to),
        cc: mb(&m.cc),
        reply_to: mb(&m.reply_to),
        subject: m.subject.clone(),
        date: Some(m.sent.unwrap_or(m.received)),
        message_id: m.message_id.clone(),
        in_reply_to: m.in_reply_to.clone(),
        references: m.references.clone(),
        importance: match m.importance {
            Importance::High => Some("high".into()),
            Importance::Low => Some("low".into()),
            Importance::Normal => None,
        },
        text: m.body.text.clone().or_else(|| m.body.html.is_none().then(String::new)),
        html: m.body.html.clone(),
        ..Default::default()
    };
    b.build()
}

fn view_source(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let Some(m) = s.mailbox.mail.get(&focus) else {
        return Err(CmdError::Failed("message not found".into()));
    };
    let source = String::from_utf8_lossy(&item_source(m)).into_owned();
    s.notify(Notice::OpenDialog("viewSource".into(), json!({"id": focus.0, "source": source})));
    Ok(json!({"dialog": "viewSource", "source": source}))
}

fn recall(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    open_dialog(s, "recallMessage", json!({"id": focus.0}))
}

/// Attachments with their data, from the item's stored source (none when no source is kept).
pub(crate) fn source_attachments(m: &MailItem) -> Vec<crate::compose::ComposeAttachment> {
    let Some(raw) = m.raw.as_deref() else {
        return Vec::new();
    };
    let Ok(msg) = mailcraft_mime::parse(raw) else {
        return Vec::new();
    };
    msg.attachments()
        .into_iter()
        .filter(|a| !a.inline)
        .map(|a| crate::compose::ComposeAttachment {
            filename: a.filename,
            mime_type: a.mime_type,
            data: a.data,
        })
        .collect()
}

/// The item's body as a compose document.
pub(crate) fn body_doc(m: &MailItem) -> mailcraft_model::Doc {
    match (&m.body.format, &m.body.html) {
        (BodyFormat::PlainText, _) | (_, None) => {
            mailcraft_model::Doc::from_plain_text(m.body.text.as_deref().unwrap_or_default())
        }
        (_, Some(h)) => mailcraft_html::html_to_doc(h),
    }
}

/// Resend: a new message prefilled with the sent message's recipients, subject, body and attachments.
fn resend(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let Some(m) = s.mailbox.mail.get(&focus) else {
        return Err(CmdError::Failed("message not found".into()));
    };
    let join = |l: &[mailcraft_model::Address]| {
        l.iter().map(crate::cmd::compose::field_mailbox).collect::<Vec<_>>().join("; ")
    };
    let c = ComposeState {
        kind: ComposeKind::New,
        inline: false,
        to: join(&m.to),
        cc: join(&m.cc),
        bcc: join(&m.bcc),
        show_bcc: !m.bcc.is_empty(),
        from: m.from.as_ref().map(crate::cmd::compose::field_mailbox).unwrap_or_default(),
        subject: m.subject.clone(),
        body: body_doc(m),
        format: m.body.format,
        importance: m.importance,
        attachments: source_attachments(m),
        ..Default::default()
    };
    let id = s.open_compose(c);
    Ok(json!({"compose": id}))
}

// ---------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::{Address, Mailbox};

    pub(crate) struct Fx {
        pub s: Session,
        pub inbox: FolderId,
        pub a: ItemId,
        pub b: ItemId,
        pub c: ItemId,
    }

    fn at(day: i64, hour: i64) -> DateTime {
        DateTime { unix: day * DAY + hour * 3600, offset_min: 0 }
    }

    fn msg(folder: FolderId, subject: &str, from: &str, text: &str, t: DateTime) -> MailItem {
        MailItem {
            folder,
            subject: subject.into(),
            from: Some(Address::new(Some("Alice Rivera"), from)),
            to: vec![Address::new(Some("Test User"), "test.user@example.net")],
            received: t,
            sent: Some(t),
            body: mailcraft_model::Body {
                format: BodyFormat::PlainText,
                text: Some(text.into()),
                html: None,
            },
            ..Default::default()
        }
    }

    /// Local mailbox: a, b, c in the Inbox (a newest). 2026-10-09 is a Friday (day 20735).
    pub(crate) fn fx() -> Fx {
        let mut mb = Mailbox::new_local("Test");
        let account = mb.default_account().map(|a| a.id).unwrap_or_default();
        if let Some(acc) = mb.accounts.first_mut() {
            acc.address = "test.user@example.net".into();
            acc.sender_name = "Test User".into();
        }
        let inbox = mb.role_folder(account, FolderRole::Inbox).unwrap();
        let c = mb.add_mail(msg(inbox, "Lunch", "alice@example.com", "Shall we meet at noon?", at(20733, 9)));
        let b = mb.add_mail(msg(
            inbox,
            "RE: Lunch",
            "bob@example.org",
            "Noon works.\n\n> Shall we meet at noon?",
            at(20734, 9),
        ));
        let a = mb.add_mail(msg(inbox, "Invoice", "carol@example.com", "Invoice attached.", at(20735, 8)));
        let mut s = Session::new(mb);
        s.fixed_now = Some(at(20735, 12));
        s.selection.folder = inbox;
        s.selection.items = vec![a];
        s.selection.focus = Some(a);
        Fx { s, inbox, a, b, c }
    }

    fn role(s: &Session, r: FolderRole) -> FolderId {
        let acc = s.mailbox.default_account().map(|a| a.id).unwrap();
        s.mailbox.role_folder(acc, r).unwrap()
    }

    fn folder_of(s: &Session, id: ItemId) -> FolderId {
        s.mailbox.mail[&id].folder
    }

    #[test]
    fn every_id_registered_once() {
        let ids: Vec<&str> = specs().iter().map(|s| s.id).collect();
        let set: BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(ids.len(), set.len());
        for id in [
            "mail.addReminder",
            "mail.allCategories",
            "mail.alwaysMoveConversation",
            "mail.alwaysMoveToOther",
            "mail.archive",
            "mail.block",
            "mail.categorize",
            "mail.cleanUpConversation",
            "mail.cleanUpFolder",
            "mail.cleanUpSubfolders",
            "mail.clearCategories",
            "mail.clearFlag",
            "mail.copy",
            "mail.delete",
            "mail.editMessage",
            "mail.findRelated",
            "mail.flag",
            "mail.flagCustom",
            "mail.flagQuickClick",
            "mail.ignore",
            "mail.importance",
            "mail.junk",
            "mail.markComplete",
            "mail.markRead",
            "mail.markUnread",
            "mail.messageOptions",
            "mail.move",
            "mail.moveToOther",
            "mail.newEmail",
            "mail.newItem",
            "mail.private",
            "mail.quickPrint",
            "mail.recall",
            "mail.report",
            "mail.resend",
            "mail.setQuickClickFlag",
            "mail.togglePrivate",
            "mail.toggleRead",
            "mail.viewSource",
        ] {
            assert!(set.contains(id), "{id} missing");
        }
    }

    #[test]
    fn disabled_without_selection() {
        let mut f = fx();
        f.s.selection.items.clear();
        f.s.selection.focus = None;
        assert!(need_items(&f.s).is_some());
        assert!(need_one(&f.s).is_some());
        assert!(matches!(delete(&mut f.s, &Value::Null), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn delete_moves_to_deleted_and_selects_neighbour() {
        let mut f = fx();
        let r = delete(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["deleted"], 1);
        assert_eq!(folder_of(&f.s, f.a), role(&f.s, FolderRole::DeletedItems));
        assert_eq!(f.s.selection.focus, Some(f.b));
        assert_eq!(f.s.selection.items, vec![f.b]);
        assert!(!f.s.pending_ops.is_empty());
        // Deleting from Deleted Items removes permanently.
        delete(&mut f.s, &json!({"ids": [f.a.0]})).unwrap();
        assert!(!f.s.mailbox.mail.contains_key(&f.a));
    }

    #[test]
    fn permanent_delete_removes() {
        let mut f = fx();
        permanent_delete(&mut f.s, &json!({"ids": [f.c.0]})).unwrap();
        assert!(!f.s.mailbox.mail.contains_key(&f.c));
    }

    #[test]
    fn archive_moves_or_asks() {
        let mut f = fx();
        archive(&mut f.s, &Value::Null).unwrap();
        assert_eq!(folder_of(&f.s, f.a), role(&f.s, FolderRole::Archive));
        // Without an Archive folder: asks for one.
        let arch = role(&f.s, FolderRole::Archive);
        f.s.mailbox.folders.get_mut(&arch).unwrap().role = None;
        let r = archive(&mut f.s, &json!({"ids": [f.b.0]})).unwrap();
        assert_eq!(r["dialog"], "archiveFolder");
        assert!(matches!(f.s.notices.last(), Some(Notice::OpenDialog(n, _)) if n == "archiveFolder"));
    }

    #[test]
    fn move_and_copy() {
        let mut f = fx();
        let proj = f.s.mailbox.create_folder(f.inbox, "Projects").unwrap();
        let r = move_to_folder(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["dialog"], "moveToFolder");
        move_to_folder(&mut f.s, &json!({"folder": "Inbox\\Projects"})).unwrap();
        assert_eq!(folder_of(&f.s, f.a), proj);
        let r = copy_to_folder(&mut f.s, &json!({"ids": [f.b.0], "folder": proj.0})).unwrap();
        assert_eq!(r["copied"], 1);
        assert_eq!(folder_of(&f.s, f.b), f.inbox);
        assert_eq!(f.s.mailbox.items_in(proj).len(), 2);
        assert!(move_to_folder(&mut f.s, &json!({"ids": [f.b.0], "folder": "Nope"})).is_err());
        let r = copy_to_folder(&mut f.s, &json!({"ids": [f.b.0]})).unwrap();
        assert_eq!(r["dialog"], "copyToFolder");
        let cal = role(&f.s, FolderRole::Calendar);
        assert!(copy_to_folder(&mut f.s, &json!({"ids": [f.b.0], "folder": cal.0})).is_err());
    }

    #[test]
    fn always_move_conversation_moves_and_adds_rule() {
        let mut f = fx();
        let proj = f.s.mailbox.create_folder(f.inbox, "Lunch stuff").unwrap();
        f.s.selection.items = vec![f.c];
        f.s.selection.focus = Some(f.c);
        let r = always_move_conversation(&mut f.s, &json!({"folder": proj.0})).unwrap();
        assert_eq!(r["moved"], 2);
        assert_eq!(folder_of(&f.s, f.b), proj);
        assert_eq!(folder_of(&f.s, f.a), f.inbox);
        assert!(
            f.s.mailbox.rules.iter().any(|r| r.actions.contains(&mailcraft_model::RuleAction::MoveTo(proj)))
        );
        let r = always_move_conversation(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["dialog"], "alwaysMoveConversation");
    }

    #[test]
    fn other_tab() {
        let mut f = fx();
        move_to_other(&mut f.s, &Value::Null).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.a].focus, FocusClass::Other);
        f.s.selection.focus = Some(f.c);
        f.s.selection.items = vec![f.c];
        let r = always_move_to_other(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["sender"], "alice@example.com");
        assert_eq!(f.s.mailbox.mail[&f.c].focus, FocusClass::Other);
        assert_eq!(f.s.mailbox.mail[&f.b].focus, FocusClass::Focused);
    }

    #[test]
    fn read_state() {
        let mut f = fx();
        f.s.selection.items = vec![f.a, f.b];
        mark_read(&mut f.s, &Value::Null).unwrap();
        assert!(f.s.mailbox.mail[&f.a].read && f.s.mailbox.mail[&f.b].read);
        toggle_read(&mut f.s, &Value::Null).unwrap();
        assert!(!f.s.mailbox.mail[&f.a].read && !f.s.mailbox.mail[&f.b].read);
        toggle_read(&mut f.s, &Value::Null).unwrap();
        assert!(f.s.mailbox.mail[&f.a].read);
        mark_unread(&mut f.s, &json!({"ids": [f.b.0]})).unwrap();
        assert!(!f.s.mailbox.mail[&f.b].read);
        mark_all_read(&mut f.s, &Value::Null).unwrap();
        assert_eq!(f.s.mailbox.unread_count(f.inbox), 0);
    }

    #[test]
    fn flag_dates_follow_the_week() {
        let f = fx(); // Friday 2026-10-09
        let day = |t: Option<DateTime>| t.map(|t| t.unix.div_euclid(DAY));
        assert_eq!(flag_dates(&f.s, FlagKind::Today), (Some(at(20735, 0)), Some(at(20735, 0))));
        let (s, d) = flag_dates(&f.s, FlagKind::Tomorrow);
        assert_eq!((day(s), day(d)), (Some(20736), Some(20736)));
        let (s, d) = flag_dates(&f.s, FlagKind::ThisWeek);
        assert_eq!((day(s), day(d)), (Some(20735), Some(20735)));
        let (s, d) = flag_dates(&f.s, FlagKind::NextWeek);
        assert_eq!((day(s), day(d)), (Some(20738), Some(20742))); // Mon 10/12 .. Fri 10/16
        assert_eq!(flag_dates(&f.s, FlagKind::NoDate), (None, None));
        // On a Monday, This Week starts on Wednesday and is due Friday.
        let mut g = fx();
        g.s.fixed_now = Some(at(20731, 10));
        let (s, d) = flag_dates(&g.s, FlagKind::ThisWeek);
        assert_eq!((day(s), day(d)), (Some(20733), Some(20735)));
    }

    #[test]
    fn flag_commands() {
        let mut f = fx();
        flag(&mut f.s, &json!({"kind": "tomorrow"})).unwrap();
        assert!(f.s.mailbox.mail[&f.a].is_flagged());
        // Insert cycle: flagged → complete → stays complete.
        flag(&mut f.s, &Value::Null).unwrap();
        assert!(matches!(f.s.mailbox.mail[&f.a].flag, FlagState::Complete { .. }));
        flag(&mut f.s, &Value::Null).unwrap();
        assert!(matches!(f.s.mailbox.mail[&f.a].flag, FlagState::Complete { .. }));
        clear_flag(&mut f.s, &Value::Null).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.a].flag, FlagState::None);
        flag(&mut f.s, &Value::Null).unwrap();
        assert!(f.s.mailbox.mail[&f.a].is_flagged());
        mark_complete(&mut f.s, &Value::Null).unwrap();
        assert!(matches!(f.s.mailbox.mail[&f.a].flag, FlagState::Complete { .. }));
        assert!(flag(&mut f.s, &json!({"kind": "someday"})).is_err());
    }

    #[test]
    fn custom_flag_and_reminder() {
        let mut f = fx();
        let r = flag_custom(&mut f.s, &json!({"reminder": true})).unwrap();
        assert_eq!(r["dialog"], "customFlag");
        flag_custom(&mut f.s, &json!({"label": "Reply", "due": 20740 * DAY})).unwrap();
        match &f.s.mailbox.mail[&f.a].flag {
            FlagState::Flagged { label, due, .. } => {
                assert_eq!(label, "Reply");
                assert_eq!(due.map(|d| d.unix), Some(20740 * DAY));
            }
            other => panic!("{other:?}"),
        }
        assert!(flag_custom(&mut f.s, &json!({"start": 10, "due": 5})).is_err());
        let r = add_reminder(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["dialog"], "customFlag");
        add_reminder(&mut f.s, &json!({"ids": [f.b.0], "at": 20736 * DAY})).unwrap();
        assert!(
            matches!(&f.s.mailbox.mail[&f.b].flag, FlagState::Flagged { reminder: Some(r), .. } if r.unix == 20736 * DAY)
        );
    }

    #[test]
    fn quick_click_setting() {
        let mut f = fx();
        assert_eq!(set_quick_click_flag(&mut f.s, &Value::Null).unwrap()["dialog"], "setQuickClick");
        assert_eq!(
            set_quick_click_flag(&mut f.s, &json!({"kind": "tomorrow"})).unwrap()["quickClick"],
            "tomorrow"
        );
        assert!(set_quick_click_flag(&mut f.s, &json!({"kind": "bogus"})).is_err());
    }

    #[test]
    fn categories_toggle_and_clear() {
        let mut f = fx();
        f.s.selection.items = vec![f.a, f.b];
        categorize(&mut f.s, &json!({"name": "blue CATEGORY"})).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.a].categories, vec!["Blue category".to_string()]);
        categorize(&mut f.s, &json!({"name": "Blue category"})).unwrap();
        assert!(f.s.mailbox.mail[&f.a].categories.is_empty());
        // Quick Click = first category; unknown names join the master list.
        categorize(&mut f.s, &Value::Null).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.b].categories, vec!["Red category".to_string()]);
        categorize(&mut f.s, &json!({"name": "Travel"})).unwrap();
        assert!(Category::find(&f.s.mailbox.categories, "travel").is_some());
        clear_categories(&mut f.s, &Value::Null).unwrap();
        assert!(f.s.mailbox.mail[&f.b].categories.is_empty());
        assert_eq!(all_categories(&mut f.s, &Value::Null).unwrap()["dialog"], "colorCategories");
        // No existing target: nothing is categorized and the master list is left alone.
        let before = f.s.mailbox.categories.len();
        let r = categorize(&mut f.s, &json!({"ids": [999_999], "name": "Ghost"}));
        assert!(matches!(r, Err(CmdError::Disabled(_))), "{r:?}");
        assert_eq!(f.s.mailbox.categories.len(), before);
    }

    #[test]
    fn importance_toggles() {
        let mut f = fx();
        importance(&mut f.s, &json!({"level": "high"})).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.a].importance, Importance::High);
        importance(&mut f.s, &json!({"level": "high"})).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.a].importance, Importance::Normal);
        importance(&mut f.s, &json!({"level": "low"})).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.a].importance, Importance::Low);
        assert!(importance(&mut f.s, &json!({"level": "urgent"})).is_err());
    }

    #[test]
    fn private_toggles_mail_and_appointments() {
        let mut f = fx();
        let appt = ItemId(f.s.mailbox.alloc_id());
        f.s.mailbox
            .appointments
            .insert(appt, mailcraft_model::Appointment { id: appt, ..Default::default() });
        private(&mut f.s, &Value::Null).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.a].sensitivity, Sensitivity::Private);
        private(&mut f.s, &Value::Null).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.a].sensitivity, Sensitivity::Normal);
        f.s.selection.items = vec![appt];
        f.s.selection.focus = Some(appt);
        assert!(need_private_targets(&f.s).is_none());
        private(&mut f.s, &Value::Null).unwrap();
        assert!(f.s.mailbox.appointments[&appt].private);
    }

    #[test]
    fn ignore_toggles_conversation() {
        let mut f = fx();
        f.s.selection.items = vec![f.c];
        f.s.selection.focus = Some(f.c);
        let r = ignore(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["ignored"], true);
        let deleted = role(&f.s, FolderRole::DeletedItems);
        assert_eq!(folder_of(&f.s, f.b), deleted);
        assert_eq!(folder_of(&f.s, f.c), deleted);
        assert!(f.s.mailbox.mail[&f.b].ignored);
        assert_eq!(folder_of(&f.s, f.a), f.inbox);
        // Stop ignoring from Deleted Items.
        let r = ignore(&mut f.s, &json!({"ids": [f.b.0]})).unwrap();
        assert_eq!(r["ignored"], false);
        assert_eq!(folder_of(&f.s, f.b), f.inbox);
        assert!(!f.s.mailbox.mail[&f.c].ignored);
    }

    #[test]
    fn clean_up_removes_fully_quoted_messages() {
        let mut f = fx();
        f.s.selection.items = vec![f.b];
        f.s.selection.focus = Some(f.b);
        let r = clean_up_conversation(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["moved"], 1);
        assert_eq!(folder_of(&f.s, f.c), role(&f.s, FolderRole::DeletedItems));
        assert_eq!(folder_of(&f.s, f.b), f.inbox);
    }

    #[test]
    fn clean_up_keeps_flagged_and_categorized() {
        let mut f = fx();
        f.s.mailbox.mail.get_mut(&f.c).unwrap().categories.push("Red category".into());
        let r = clean_up_folder(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["moved"], 0);
        f.s.mailbox.mail.get_mut(&f.c).unwrap().categories.clear();
        let sub = f.s.mailbox.create_folder(f.inbox, "Sub").unwrap();
        f.s.mailbox.move_items(&[f.c], sub).unwrap();
        // Folder-only clean up of Inbox no longer sees c; subfolders variant does.
        assert_eq!(clean_up_folder(&mut f.s, &Value::Null).unwrap()["moved"], 0);
        assert_eq!(clean_up_subfolders(&mut f.s, &Value::Null).unwrap()["moved"], 1);
    }

    #[test]
    fn junk_block_report() {
        let mut f = fx();
        let junkf = role(&f.s, FolderRole::JunkEmail);
        assert_eq!(junk(&mut f.s, &json!({"action": "options"})).unwrap()["dialog"], "junkOptions");
        let r = block(&mut f.s, &json!({"kind": "sender"})).unwrap();
        assert_eq!(r["blocked"], "carol@example.com");
        assert_eq!(folder_of(&f.s, f.a), junkf);
        let r = block(&mut f.s, &json!({"ids": [f.a.0], "kind": "neverDomain"})).unwrap();
        assert_eq!(r["safe"], "@example.com");
        assert_eq!(folder_of(&f.s, f.a), f.inbox);
        junk(&mut f.s, &json!({"ids": [f.b.0], "action": "moveToJunk"})).unwrap();
        assert_eq!(folder_of(&f.s, f.b), junkf);
        junk(&mut f.s, &json!({"ids": [f.b.0], "action": "notJunk"})).unwrap();
        assert_eq!(folder_of(&f.s, f.b), f.inbox);
        report(&mut f.s, &json!({"ids": [f.c.0], "kind": "phishing"})).unwrap();
        assert_eq!(folder_of(&f.s, f.c), junkf);
        assert!(f.s.mailbox.mail[&f.c].read);
        assert!(block(&mut f.s, &json!({"kind": "everyone"})).is_err());
    }

    #[test]
    fn find_related_sets_search() {
        let mut f = fx();
        f.s.selection.focus = Some(f.b);
        find_related(&mut f.s, &json!({"kind": "conversation"})).unwrap();
        assert_eq!(f.s.view.search, "subject:\"Lunch\"");
        find_related(&mut f.s, &json!({"kind": "sender"})).unwrap();
        assert_eq!(f.s.view.search, "from:\"bob@example.org\"");
        assert_eq!(f.s.view.search_scope, SearchScope::CurrentMailbox);
    }

    #[test]
    fn new_email_and_items() {
        let mut f = fx();
        let r = new_email(&mut f.s, &json!({"format": "plain"})).unwrap();
        let id = r["compose"].as_u64().unwrap();
        let c = &f.s.composes[&id];
        assert_eq!(c.kind, ComposeKind::New);
        assert_eq!(c.format, BodyFormat::PlainText);
        assert_eq!(c.from, "Test User <test.user@example.net>");
        assert!(!c.inline);
        assert_eq!(new_email(&mut f.s, &json!({"pickStationery": true})).unwrap()["dialog"], "stationery");
        assert!(new_email(&mut f.s, &json!({"format": "pdf"})).is_err());
        assert_eq!(new_item(&mut f.s, &json!({"kind": "meeting"})).unwrap()["dialog"], "newMeeting");
        assert!(new_item(&mut f.s, &json!({"kind": "spaceship"})).is_err());
        assert!(new_item(&mut f.s, &Value::Null).is_err());
    }

    #[test]
    fn open_print_and_dialogs() {
        let mut f = fx();
        assert_eq!(open(&mut f.s, &Value::Null).unwrap()["dialog"], "readWindow");
        assert!(f.s.mailbox.mail[&f.a].read);
        assert_eq!(print(&mut f.s, &Value::Null).unwrap()["dialog"], "print");
        assert_eq!(quick_print(&mut f.s, &Value::Null).unwrap()["dialog"], "print");
        assert_eq!(message_options(&mut f.s, &Value::Null).unwrap()["dialog"], "messageOptions");
        message_options(&mut f.s, &json!({"sensitivity": "confidential", "importance": "low"})).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.a].sensitivity, Sensitivity::Confidential);
        assert_eq!(f.s.mailbox.mail[&f.a].importance, Importance::Low);
    }

    #[test]
    fn edit_message_saves() {
        let mut f = fx();
        assert_eq!(edit_message(&mut f.s, &Value::Null).unwrap()["dialog"], "readWindow");
        edit_message(&mut f.s, &json!({"subject": "Invoice 7", "text": "Paid."})).unwrap();
        let m = &f.s.mailbox.mail[&f.a];
        assert_eq!(m.subject, "Invoice 7");
        assert_eq!(m.preview, "Paid.");
    }

    #[test]
    fn view_source_rebuilds_when_missing() {
        let mut f = fx();
        let r = view_source(&mut f.s, &Value::Null).unwrap();
        let src = r["source"].as_str().unwrap();
        assert!(src.contains("Subject: Invoice"));
        assert!(src.contains("carol@example.com"));
    }

    #[test]
    fn recall_and_resend_need_sent_items() {
        let mut f = fx();
        assert!(need_recallable(&f.s).is_some());
        assert!(need_sent_item(&f.s).is_some());
        let sent = role(&f.s, FolderRole::SentItems);
        f.s.mailbox.move_items(&[f.a], sent).unwrap();
        f.s.selection.folder = sent;
        assert!(need_sent_item(&f.s).is_none());
        assert!(need_recallable(&f.s).is_some()); // local account
        let r = resend(&mut f.s, &Value::Null).unwrap();
        let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.subject, "Invoice");
        assert_eq!(c.to, "Test User <test.user@example.net>");
        f.s.mailbox.accounts[0].kind = AccountKind::Exchange;
        assert!(need_recallable(&f.s).is_none());
        assert_eq!(recall(&mut f.s, &Value::Null).unwrap()["dialog"], "recallMessage");
    }

    #[test]
    fn session_run_dispatches() {
        let mut f = fx();
        f.s.run("mail.markRead", &Value::Null).unwrap();
        assert!(f.s.mailbox.mail[&f.a].read);
        f.s.run("mail.delete", &Value::Null).unwrap();
        assert_eq!(folder_of(&f.s, f.a), role(&f.s, FolderRole::DeletedItems));
    }

    #[test]
    fn sample_mailbox_commands_never_panic() {
        let now = at(20735, 12);
        let mut s = Session::new(crate::sample::sample_mailbox(now));
        s.fixed_now = Some(now);
        let ids: Vec<u64> = s.mailbox.mail.keys().take(3).map(|i| i.0).collect();
        if let Some(first) = s.mailbox.mail.values().next() {
            s.selection.folder = first.folder;
        }
        s.selection.items = ids.iter().map(|i| ItemId(*i)).collect();
        s.selection.focus = s.selection.items.first().copied();
        for spec in specs() {
            let _ = (spec.run)(
                &mut s,
                &json!({"ids": ids, "kind": "today", "level": "high", "name": "Red category"}),
            );
        }
    }
}
