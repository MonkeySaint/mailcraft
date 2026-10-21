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
    AccountId, BodyFormat, Category, CategoryColor, Conversation, DateTime, FlagState, FocusClass, FolderId,
    FolderKind, FolderRole, Importance, ItemId, JunkLevel, JunkList, MailItem, Sensitivity, thread,
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
            "mail.newFromMailto",
            "New Email from a mailto: Link",
            "Reading pane › links; command line",
            "",
            r#"{"url":string}  (RFC 6068 mailto: URL; to, cc, bcc, subject and body are used)"#,
            false,
            always,
            new_from_mailto,
        ),
        spec(
            "mail.newItem",
            "New Items",
            "Home › New › New Items",
            "",
            r#"{"kind":"appointment"|"meeting"|"allDayEvent"|"recurringAppointment"|"recurringMeeting"|"recurringEvent"|"meetingWithAll"|"contact"|"task"|"post"|"contactGroup"|"taskRequest"|"fromForm", "subject"?:string, "text"?:string, "html"?:string, "folder"?:number|string (post: subject/text save the post in the mail folder)}"#,
            true,
            always,
            new_item,
        ),
        spec(
            "calendar.meetingWithAll",
            "Meeting with All",
            "Calendar › Home › New Items",
            "",
            r#"{"ids"?:[number]}"#,
            false,
            need_meeting_or_message,
            meeting_with_all,
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
            r#"{"folder"?:number|string,"ids"?:[number],"remember"?:bool (make the folder the account's Archive folder when it has none)}"#,
            true,
            need_items,
            archive,
        ),
        spec(
            "mail.ignore",
            "Ignore",
            "Home › Delete",
            "Mod+Delete",
            r#"{"ids"?:[number],"confirm"?:bool} (turning Ignore on asks first unless confirm is true or the prompt was switched off)"#,
            true,
            need_one,
            ignore,
        ),
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
            r#"{"folder"?:number|string,"confirm"?:bool (asks first without it, unless the prompt was switched off)}"#,
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
            r#"{"action":"options"|"get"|"set"|"add"|"remove"|"import"|"export"|"moveToJunk"|"notJunk"|"run"|"classify", "list"?:"safeSenders"|"safeRecipients"|"blockedSenders"|"blockedTlds"|"blockedEncodings", "entry"?:string, "entries"?:[string], "path"?:string, "settings"?:{"level"?:"noFiltering"|"low"|"high"|"safeListsOnly", "deleteSuspected"?:bool, "disableLinks"?:bool, "warnSuspiciousDomains"?:bool, "trustContacts"?:bool, "addRecipientsToSafe"?:bool}, "addToSafe"?:bool, "folder"?:number|string, "ids"?:[number]}"#,
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
            r#"{"kind"?:"junk"|"phishing","ids"?:[number],"confirm"?:bool (phishing asks first without it)}"#,
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
        spec(
            "mail.moveToFocused",
            "Move to Focused",
            "Message context menu (Focused Inbox, Other tab)",
            "",
            "",
            true,
            need_items,
            move_to_focused,
        ),
        spec(
            "mail.alwaysMoveToFocused",
            "Always Move to Focused",
            "Message context menu (Focused Inbox, Other tab)",
            "",
            "",
            true,
            need_one,
            always_move_to_focused,
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
            "mail.pin",
            "Pin / Unpin",
            "Message list › Quick Actions",
            "",
            r#"{"ids"?:[number],"on"?:bool}"#,
            true,
            always,
            pin,
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
        from: account_from(s, new_mail_account(s)),
        format,
        inline: false,
        ..Default::default()
    };
    let c = crate::compose::with_default_signature(&s.mailbox, c);
    let id = s.open_compose(c);
    Ok(json!({"compose": id}))
}

/// A new message from a `mailto:` link (RFC 6068): its recipients, subject and body. Options › Advanced ›
/// "Allow UTF-8 support for the mailto: protocol" says how its percent-encoded text decodes.
fn new_from_mailto(s: &mut Session, p: &Value) -> CmdResult {
    let url = req_str(p, "url")?;
    let m =
        crate::intl::parse_mailto(url, s.settings.bool("advanced.mailtoUtf8")).map_err(CmdError::Params)?;
    let mut c = ComposeState {
        kind: ComposeKind::New,
        from: account_from(s, new_mail_account(s)),
        format: s.settings.compose_format(),
        to: m.to.join("; "),
        cc: m.cc.join("; "),
        bcc: m.bcc.join("; "),
        subject: m.subject,
        inline: false,
        ..Default::default()
    };
    if !m.body.is_empty() {
        c.body = mailcraft_model::Doc::from_plain_text(&m.body);
    }
    let c = crate::compose::with_default_signature(&s.mailbox, c);
    let id = s.open_compose(c);
    Ok(json!({"compose": id}))
}

/// The account a new message is sent from: the current folder's account, unless Options › Mail › "Always use
/// the default account when composing new messages" is on or that account cannot send (a data file without
/// an address); then the default account.
pub(crate) fn new_mail_account(s: &Session) -> Option<AccountId> {
    let default = s.mailbox.default_account().map(|a| a.id);
    if s.settings.bool("mail.send.alwaysDefaultAccount") {
        return default;
    }
    s.mailbox
        .folder_account(s.selection.folder)
        .filter(|a| s.mailbox.account(*a).is_some_and(|acc| !acc.address.trim().is_empty()))
        .or(default)
}

fn new_item(s: &mut Session, p: &Value) -> CmdResult {
    let kind = req_str(p, "kind")?;
    // New Items presets: (dialog, all day, recurrence pattern the form starts with).
    let (dialog, all_day, recurrence) = match kind {
        "appointment" => ("newAppointment", false, None),
        "meeting" => ("newMeeting", false, None),
        "allDayEvent" => ("newAppointment", true, None),
        "recurringAppointment" => ("newAppointment", false, Some("weekly")),
        "recurringMeeting" => ("newMeeting", false, Some("weekly")),
        "recurringEvent" => ("newAppointment", true, Some("weekly")),
        "meetingWithAll" => return meeting_with_all(s, p),
        "post" => return new_post(s, p),
        "contact" => ("newContact", false, None),
        "task" => ("newTask", false, None),
        "contactGroup" => ("newContactGroup", false, None),
        "taskRequest" => ("newTaskRequest", false, None),
        "fromForm" => ("chooseForm", false, None),
        "email" | "message" => return new_email(s, p),
        other => return Err(CmdError::Params(format!("unknown item kind `{other}`"))),
    };
    // Appointments, contacts and tasks go to their own module's current or default folder, so no mail folder
    // is passed for them.
    let mut params = json!({"kind": kind});
    if let Some(o) = params.as_object_mut() {
        if all_day {
            o.insert("allDay".into(), json!(true));
        }
        if let Some(r) = recurrence {
            o.insert("recurrence".into(), json!(r));
        }
        // A start / end / subject chosen by the caller (the calendar's selected slot) carries over.
        for key in ["start", "end", "subject"] {
            if let Some(v) = opt_str(p, key) {
                o.insert(key.into(), json!(v));
            }
        }
    }
    open_dialog(s, dialog, params)
}

/// Lower-case address of `m` is one of the user's own.
fn is_me(me: &[String], addr: &str) -> bool {
    let a = addr.trim().to_lowercase();
    !a.is_empty() && me.contains(&a)
}

/// Meeting with All is offered for a selected meeting or appointment that has other people on it (organizer
/// or attendees besides the user), or a selected message (observed disabled with nothing selected).
fn need_meeting_or_message(s: &Session) -> Option<&'static str> {
    let me = my_addresses(s);
    let others = |a: &mailcraft_model::Appointment| {
        a.organizer
            .iter()
            .chain(
                a.attendees
                    .iter()
                    .filter(|x| x.role != mailcraft_model::calendar::AttendeeRole::Resource)
                    .map(|x| &x.mailbox),
            )
            .any(|m| !is_me(&me, &m.addr))
    };
    let meeting = s.target_ids(&Value::Null).iter().filter_map(|i| s.mailbox.appointments.get(i)).any(others);
    if meeting || primary_item(s, &Value::Null).is_some() {
        None
    } else {
        Some("Select a meeting or a message first.")
    }
}

/// New Items › Meeting with All (`calendar.meetingWithAll`, also `mail.newItem {"kind": "meetingWithAll"}`):
/// a meeting request to everybody on the selected meeting (organizer and attendees, without the user), or
/// to the people on the selected message.
fn meeting_with_all(s: &mut Session, p: &Value) -> CmdResult {
    let appointment = s.target_ids(p).into_iter().find_map(|id| s.mailbox.appointments.get(&id).cloned());
    let Some(a) = appointment else {
        if primary_item(s, p).is_some() {
            return s.run("mail.replyWithMeeting", p);
        }
        return Err(CmdError::Disabled("Select a meeting or a message first.".into()));
    };
    let me = my_addresses(s);
    let mut required: Vec<String> = Vec::new();
    let mut optional: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut add = |list: &mut Vec<String>, m: &mailcraft_model::Address| {
        if !is_me(&me, &m.addr) && seen.insert(m.addr.trim().to_lowercase()) {
            list.push(crate::cmd::compose::field_mailbox(m));
        }
    };
    if let Some(o) = &a.organizer {
        add(&mut required, o);
    }
    for at in &a.attendees {
        match at.role {
            mailcraft_model::calendar::AttendeeRole::Optional => add(&mut optional, &at.mailbox),
            mailcraft_model::calendar::AttendeeRole::Required => add(&mut required, &at.mailbox),
            // Rooms and equipment are booked by the new meeting's own Scheduling, not invited.
            mailcraft_model::calendar::AttendeeRole::Resource => {}
        }
    }
    if required.is_empty() && optional.is_empty() {
        return Err(CmdError::Disabled("The selected item has no other attendees.".into()));
    }
    let params = json!({
        "kind": "meetingWithAll",
        "source": a.id.0,
        "subject": a.subject,
        "attendees": required,
        "optional": optional,
        "location": a.location,
    });
    open_dialog(s, "newMeeting", params)
}

/// The mail folder a post goes to: `params.folder`, else the selected folder; it must hold mail.
fn post_folder(s: &Session, p: &Value) -> Result<FolderId, CmdError> {
    let folder = param_folder(s, p)?.unwrap_or(s.selection.folder);
    match s.mailbox.folder(folder) {
        Some(f) if f.kind == FolderKind::Mail => Ok(folder),
        Some(_) => Err(CmdError::Disabled("Posts can be added to mail folders only.".into())),
        None => Err(CmdError::Disabled("No mail folder is selected.".into())),
    }
}

/// Post in This Folder (Ctrl+Shift+S): without `subject`/`text`/`html` opens the post form for the selected
/// mail folder; with them saves a post there: a message from the user with no recipients, stored (not
/// sent) in the folder like any message (a synchronised folder uploads it).
fn new_post(s: &mut Session, p: &Value) -> CmdResult {
    let folder = post_folder(s, p)?;
    let (subject, text, html) = (opt_str(p, "subject"), opt_str(p, "text"), opt_str(p, "html"));
    if subject.is_none() && text.is_none() && html.is_none() {
        let path = s.mailbox.folder_path(folder);
        return open_dialog(s, "newPost", json!({"kind": "post", "folder": folder.0, "path": path}));
    }
    let subject = subject.unwrap_or("").trim();
    if subject.chars().count() > MAX_EDIT_SUBJECT_CHARS {
        return Err(CmdError::Params("the subject is too long".into()));
    }
    if text.map_or(0, str::len).saturating_add(html.map_or(0, str::len)) > MAX_EDIT_BODY_BYTES {
        return Err(CmdError::Params("the post is too large".into()));
    }
    let account = s.mailbox.folder_account(folder);
    let from = account.and_then(|a| s.mailbox.account(a)).filter(|a| !a.address.trim().is_empty()).map(|a| {
        let name = if a.sender_name.trim().is_empty() { a.display_name.trim() } else { a.sender_name.trim() };
        mailcraft_model::Address::new(Some(name).filter(|n| !n.is_empty()), a.address.trim())
    });
    let now = s.now();
    let body = match html {
        Some(h) => mailcraft_model::Body {
            format: BodyFormat::Html,
            text: Some(text.map_or_else(|| mailcraft_html::html_to_text(h), str::to_owned)),
            html: Some(h.to_owned()),
        },
        None => mailcraft_model::Body {
            format: BodyFormat::PlainText,
            text: Some(text.unwrap_or("").to_owned()),
            html: None,
        },
    };
    let builder = mailcraft_mime::MessageBuilder {
        from: from.clone(),
        subject: subject.to_owned(),
        date: Some(now),
        ..Default::default()
    };
    let message_id = builder.generate_message_id(now, s.mailbox.next_id);
    let size =
        body.text.as_ref().map_or(0, String::len).saturating_add(body.html.as_ref().map_or(0, String::len));
    let mut item = MailItem {
        folder,
        message_id: Some(message_id),
        subject: subject.to_owned(),
        from,
        sent: Some(now),
        received: now,
        size: u64::try_from(size).unwrap_or(u64::MAX),
        read: true,
        body,
        ..Default::default()
    };
    item.refresh_preview();
    let id = s.mailbox.add_mail(item);
    journal(s, &[id]);
    Ok(json!({"id": id.0, "folder": folder.0, "post": true}))
}

/// Longest subject a post or an edited message takes.
const MAX_EDIT_SUBJECT_CHARS: usize = 1000;
/// Largest text (+ HTML) a post or an edited message takes.
const MAX_EDIT_BODY_BYTES: usize = 8 * 1024 * 1024;

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

/// Archive: the messages go to their account's Archive folder. An account without one asks for a folder
/// (`archiveFolder`); the folder picked there (`remember`) becomes the account's Archive folder, so later
/// archives need no question.
fn archive(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    if let Some(folder) = param_folder(s, p)? {
        let mut remembered = false;
        if opt_bool(p, "remember") == Some(true)
            && let Some(account) = s.mailbox.folder_account(folder)
            && s.mailbox.role_folder(account, FolderRole::Archive).is_none()
            && let Some(f) = s.mailbox.folders.get_mut(&folder)
            && f.role.is_none()
            && f.kind == FolderKind::Mail
        {
            f.role = Some(FolderRole::Archive);
            remembered = true;
            journal_mailbox(s);
        }
        let moved = move_and_reselect(s, &ids, folder)?;
        return Ok(json!({"moved": moved.len(), "folder": folder.0, "remembered": remembered}));
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

/// Most Message-IDs an Always Move Conversation rule lists.
const MAX_CONVERSATION_IDS: usize = 50;

/// Message-IDs (without brackets) of the conversations' messages, newest first, without duplicates.
fn members_ids(s: &Session, convs: &[Conversation]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in convs.iter().flat_map(|c| c.items.iter()) {
        if let Some(mid) = s.mailbox.mail.get(id).and_then(|m| m.message_id.as_deref()) {
            let mid = mid.trim().trim_start_matches('<').trim_end_matches('>').trim();
            if !mid.is_empty() && !mid.contains(['<', '>']) && !out.iter().any(|x| x == mid) {
                out.push(mid.to_owned());
            }
        }
    }
    out
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
    // Future messages: a rule keeps moving them. It recognises the conversation by its Message-IDs, which
    // replies carry in In-Reply-To / References, so unrelated mail with the same subject stays put; only a
    // conversation whose messages carry no Message-ID falls back to the subject.
    let mut ids: Vec<String> = members_ids(s, &convs);
    ids.truncate(MAX_CONVERSATION_IDS);
    let moved = move_and_reselect(s, &movable, folder)?;
    if !topic.trim().is_empty() {
        let name = format!("Always move conversation: {topic}");
        let rule_id = s.mailbox.rules.iter().map(|r| r.id).max().unwrap_or(0).saturating_add(1);
        s.mailbox.rules.retain(|r| r.name != name);
        let condition = if ids.is_empty() {
            mailcraft_model::RuleCondition::SubjectContains(vec![topic.clone()])
        } else {
            mailcraft_model::RuleCondition::HeaderContains(ids.iter().map(|i| format!("<{i}>")).collect())
        };
        s.mailbox.rules.push(mailcraft_model::Rule {
            id: rule_id,
            name,
            enabled: true,
            conditions: vec![condition],
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

/// Setting holding the senders whose mail always goes to Other (lower-case addresses), saved with the
/// profile when the settings store defines it.
pub const OTHER_SENDERS_SETTING: &str = "mail.focused.otherSenders";

/// Most sender overrides kept (the oldest are dropped first).
const MAX_OTHER_SENDERS: usize = 1000;

/// Senders whose mail always goes to Other (lower-case addresses).
pub fn other_senders(s: &Session) -> Vec<String> {
    s.settings.list(OTHER_SENDERS_SETTING)
}

/// Setting holding the senders whose mail always goes to Focused (Always Move to Focused).
pub const FOCUSED_SENDERS_SETTING: &str = "mail.focused.focusedSenders";

/// Senders whose mail always goes to Focused (lower-case addresses).
pub fn focused_senders(s: &Session) -> Vec<String> {
    s.settings.list(FOCUSED_SENDERS_SETTING)
}

/// The Focused Inbox class a sender override gives `m` (`None` = no override: the default classification
/// applies): `other` holds the Always Move to Other senders. New mail goes through [`apply_focus_overrides`],
/// which also applies the Always Move to Focused senders.
pub fn focus_override(other: &[String], m: &MailItem) -> Option<FocusClass> {
    let from = m.from.as_ref()?.addr.trim().to_lowercase();
    (!from.is_empty() && other.contains(&from)).then_some(FocusClass::Other)
}

/// Applies the sender overrides (Always Move to Other, Always Move to Focused) to `ids`, e.g. newly arrived
/// mail; returns the items whose class changed (journaled).
pub fn apply_focus_overrides(s: &mut Session, ids: &[ItemId]) -> Vec<ItemId> {
    let other_list = other_senders(s);
    let focused_list = focused_senders(s);
    if other_list.is_empty() && focused_list.is_empty() {
        return Vec::new();
    }
    let sender = |m: &MailItem| m.from.as_ref().map(|a| a.addr.trim().to_lowercase()).unwrap_or_default();
    let mut other = Vec::new();
    let mut focused = Vec::new();
    for id in ids {
        let Some(m) = s.mailbox.mail.get(id) else { continue };
        let from = sender(m);
        if from.is_empty() {
            continue;
        }
        if other_list.contains(&from) {
            other.push(*id);
        } else if focused_list.contains(&from) {
            focused.push(*id);
        }
    }
    let mut changed = set_focus_class(s, &other, FocusClass::Other);
    changed.extend(set_focus_class(s, &focused, FocusClass::Focused));
    changed
}

fn move_to_focused(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let changed = set_focus_class(s, &ids, FocusClass::Focused);
    Ok(json!({"changed": changed.len(), "ids": ids_json(&changed)}))
}

fn always_move_to_other(s: &mut Session, p: &Value) -> CmdResult {
    always_move_sender(s, p, FocusClass::Other)
}

fn always_move_to_focused(s: &mut Session, p: &Value) -> CmdResult {
    always_move_sender(s, p, FocusClass::Focused)
}

/// Always Move to Other / Focused: every message from the sender in this folder goes to that tab now, and
/// the sender is remembered (and dropped from the other tab's list) so later mail from them is classified
/// the same way ([`apply_focus_overrides`]).
fn always_move_sender(s: &mut Session, p: &Value, class: FocusClass) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let Some((sender, folder)) = s
        .mailbox
        .mail
        .get(&focus)
        .map(|m| (m.from.as_ref().map(|a| a.addr.trim().to_lowercase()).unwrap_or_default(), m.folder))
    else {
        return Err(CmdError::Failed("message not found".into()));
    };
    // Every message from the sender in this folder (the Focused Inbox) goes to that tab.
    let mut ids: Vec<ItemId> = vec![focus];
    if !sender.is_empty() {
        ids.extend(
            s.mailbox
                .mail
                .values()
                .filter(|m| {
                    m.folder == folder
                        && m.from.as_ref().is_some_and(|a| a.addr.trim().eq_ignore_ascii_case(&sender))
                })
                .map(|m| m.id),
        );
    }
    let changed = set_focus_class(s, &ids, class);
    let (keep, drop) = match class {
        FocusClass::Other => (OTHER_SENDERS_SETTING, FOCUSED_SENDERS_SETTING),
        _ => (FOCUSED_SENDERS_SETTING, OTHER_SENDERS_SETTING),
    };
    let mut persisted = false;
    if !sender.is_empty() && crate::settings::def(keep).is_some() {
        let mut list = s.settings.list(keep);
        list.retain(|x| *x != sender);
        list.push(sender.clone());
        let excess = list.len().saturating_sub(MAX_OTHER_SENDERS);
        list.drain(..excess);
        persisted = s.settings.set(keep, &json!(list)).is_ok();
        let mut other = s.settings.list(drop);
        if other.contains(&sender) {
            other.retain(|x| *x != sender);
            persisted &= s.settings.set(drop, &json!(other)).is_ok();
        }
    }
    Ok(json!({"changed": changed.len(), "sender": sender, "persisted": persisted}))
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
/// The Follow Up menu entry ("today", "tomorrow", "thisWeek", "nextWeek", "noDate") whose dates `flag`
/// carries as of today, for the menu's check marks; `None` for no flag, a completed one or custom dates.
pub fn follow_up_kind(s: &Session, flag: &FlagState) -> Option<&'static str> {
    let FlagState::Flagged { start, due, .. } = flag else { return None };
    let day = |d: Option<DateTime>| d.map(|t| local_day(t, s.tz_offset_min));
    let kinds = [
        ("today", FlagKind::Today),
        ("tomorrow", FlagKind::Tomorrow),
        ("thisWeek", FlagKind::ThisWeek),
        ("nextWeek", FlagKind::NextWeek),
        ("noDate", FlagKind::NoDate),
    ];
    kinds.into_iter().find_map(|(key, kind)| {
        let (ks, kd) = flag_dates(s, kind);
        (day(*start) == day(ks) && day(*due) == day(kd)).then_some(key)
    })
}

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

/// The Quick Click flag kind (`today` … `noDate`, or `complete`): Options › Tasks › Quick Click, set with
/// Follow Up › Set Quick Click (`tasks.quickClickFlag`).
pub(crate) fn quick_click_kind(s: &Session) -> String {
    let k = s.settings.text("tasks.quickClickFlag");
    if k == "complete" || FlagKind::parse(&k).is_some() { k } else { "today".to_owned() }
}

/// The flag a plain click / Insert applies (the Quick Click flag).
fn quick_click_flag(s: &Session) -> FlagState {
    let kind = quick_click_kind(s);
    if kind == "complete" {
        return FlagState::Complete { completed: Some(s.now()) };
    }
    let (start, due) = flag_dates(s, FlagKind::parse(&kind).unwrap_or(FlagKind::Today));
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
/// Set Quick Click (flags): without `kind` opens the dialog; with it stores the Quick Click flag in the
/// profile settings (`tasks.quickClickFlag`), used by a click in the flag column, Insert and Follow Up.
fn set_quick_click_flag(s: &mut Session, p: &Value) -> CmdResult {
    match opt_str(p, "kind") {
        None => open_dialog(s, "setQuickClick", json!({"for": "flag", "kind": quick_click_kind(s)})),
        Some(k) => {
            let id = if k.eq_ignore_ascii_case("complete") {
                "complete"
            } else {
                match FlagKind::parse(k) {
                    Some(FlagKind::Today) => "today",
                    Some(FlagKind::Tomorrow) => "tomorrow",
                    Some(FlagKind::ThisWeek) => "thisWeek",
                    Some(FlagKind::NextWeek) => "nextWeek",
                    Some(FlagKind::NoDate) => "noDate",
                    None => {
                        let shown: String = k.chars().take(40).collect();
                        return Err(CmdError::Params(format!("unknown flag kind `{shown}`")));
                    }
                }
            };
            crate::cmd::options::set_option(s, "tasks.quickClickFlag", &json!(id))?;
            Ok(json!({"quickClick": id}))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Categories, importance, sensitivity
// ---------------------------------------------------------------------------------------------------------

/// Toggles a category on the selection: removed from all when every item has it, else added where missing.
/// Unknown names are added to the master list. Without a name the Quick Click category
/// ([`crate::cmd::categories::quick_click_category`]).
fn categorize(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    if ids.is_empty() {
        return Err(CmdError::Disabled("No message is selected.".into()));
    }
    let name = match opt_str(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.to_owned(),
        None => match crate::cmd::categories::quick_click_category(s) {
            Some(c) => c,
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
    if !currently && opt_bool(p, "confirm") != Some(true) && !s.settings.bool(IGNORE_DONT_ASK) {
        return open_dialog(
            s,
            "confirm",
            json!({
                "title": "Ignore Conversation",
                "text": "The selected conversation and any messages that arrive in it later go to Deleted Items.",
                "command": "mail.ignore",
                "params": {"ids": [focus.0], "confirm": true},
                "dontAsk": IGNORE_DONT_ASK,
            }),
        );
    }
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

/// Setting that switches the Ignore Conversation prompt off ("Don't show this message again").
pub const IGNORE_DONT_ASK: &str = "state.confirm.ignoreDontAsk";

/// New mail that joins an ignored conversation: marked ignored and moved to its account's Deleted Items, as
/// the earlier members were (`mail.ignore`). Runs on arrival ([`crate::sync::arrival::on_new_mail`]); returns
/// the items moved. Costs nothing while no item is ignored.
///
/// A new message joins an ignored conversation only through Message-IDs: its In-Reply-To or References name
/// an ignored message of its account (or an ignored message names it). The subject never counts, so after
/// ignoring "Invoice" or "Weekly report", someone else's message with the same subject still arrives.
pub(crate) fn ignore_arrivals(s: &mut Session, ids: &[ItemId]) -> Vec<ItemId> {
    if ids.is_empty() || !s.mailbox.mail.values().any(|m| m.ignored) {
        return Vec::new();
    }
    fn norm(id: &str) -> Option<String> {
        let t = id.trim().trim_start_matches('<').trim_end_matches('>').trim();
        (!t.is_empty()).then(|| t.to_owned())
    }
    let new: BTreeSet<ItemId> = ids.iter().copied().collect();
    // Per account: Message-IDs of ignored messages, and the ids ignored messages refer to.
    let mut ignored_ids: BTreeSet<(AccountId, String)> = BTreeSet::new();
    let mut referred: BTreeSet<(AccountId, String)> = BTreeSet::new();
    for m in s.mailbox.mail.values().filter(|m| m.ignored && !new.contains(&m.id)) {
        let Some(a) = s.mailbox.folder_account(m.folder) else { continue };
        if let Some(mid) = m.message_id.as_deref().and_then(norm) {
            ignored_ids.insert((a, mid));
        }
        for r in m.in_reply_to.iter().chain(&m.references).take(MAX_LINKS_PER_MESSAGE) {
            if let Some(r) = norm(r) {
                referred.insert((a, r));
            }
        }
    }
    // New messages can link to each other (a reply arriving with its parent): repeat until nothing joins.
    let mut out = Vec::new();
    let mut pending: Vec<ItemId> = ids.to_vec();
    loop {
        let mut joined = Vec::new();
        for id in &pending {
            let Some(m) = s.mailbox.mail.get(id) else { continue };
            let Some(a) = s.mailbox.folder_account(m.folder) else { continue };
            let own = m.message_id.as_deref().and_then(norm);
            let links = m
                .in_reply_to
                .iter()
                .chain(&m.references)
                .take(MAX_LINKS_PER_MESSAGE)
                .filter_map(|r| norm(r))
                .any(|r| ignored_ids.contains(&(a, r)));
            let named = own.as_ref().is_some_and(|o| referred.contains(&(a, o.clone())));
            if links || named {
                joined.push((*id, a, own));
            }
        }
        if joined.is_empty() {
            break;
        }
        for (id, a, own) in joined {
            pending.retain(|p| *p != id);
            if let Some(own) = own {
                ignored_ids.insert((a, own));
            }
            let Some(deleted) = role_folder_of(s, id, FolderRole::DeletedItems) else { continue };
            if let Some(m) = s.mailbox.mail.get_mut(&id) {
                m.ignored = true;
            }
            if s.mailbox.move_items(&[id], deleted).is_ok() {
                out.push(id);
            }
        }
    }
    out
}

/// Most In-Reply-To / References entries looked at per message when matching ignored conversations.
const MAX_LINKS_PER_MESSAGE: usize = 200;

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

/// Body lines normalised like [`normalised_text`] (quote markers removed, whitespace collapsed), empty lines
/// left out.
fn normalised_lines(m: &MailItem) -> Vec<String> {
    m.body_text()
        .lines()
        .map(|l| {
            l.trim_start_matches(|c: char| c == '>' || c.is_whitespace())
                .split_whitespace()
                .collect::<Vec<_>>()
        })
        .filter(|w| !w.is_empty())
        .map(|w| w.join(" "))
        .collect()
}

/// True when every line of `original` appears in `later`, in order, with other lines between them: a reply
/// that answered inside the quoted original (inline comments) and so modified it.
fn quoted_with_changes(original: &[String], later: &[String]) -> bool {
    if original.is_empty() || later.len() <= original.len() {
        return false;
    }
    let mut want = original.iter().peekable();
    for line in later {
        if want.peek().is_some_and(|w| *w == line) {
            want.next();
        }
    }
    want.peek().is_none()
}

/// Members of `conv` passing `candidate` whose text is fully contained in a later member. Options › Mail ›
/// Conversation Clean Up says which are kept whatever their text (unread, categorized, flagged, digitally
/// signed); drafts are never moved. "When a reply modifies a message, don't move the original": with it off,
/// a message a later reply quotes with comments added inside the quote (every line still there, in order)
/// counts as redundant too; with it on (the default) only an unchanged quote does.
fn redundant_members(
    s: &Session,
    conv: &Conversation,
    candidate: &dyn Fn(&MailItem) -> bool,
    opts: &crate::settings::CleanUpOptions,
) -> Vec<ItemId> {
    let members: Vec<(&MailItem, String, Vec<String>)> = conv
        .items
        .iter()
        .filter_map(|id| s.mailbox.mail.get(id))
        .map(|m| (m, normalised_text(m), if opts.keep_original { Vec::new() } else { normalised_lines(m) }))
        .collect();
    let mut out = Vec::new();
    for (m, text, lines) in &members {
        if text.is_empty() || m.is_draft || !candidate(m) || opts.keeps(m, is_signed(m)) {
            continue;
        }
        let quoted_later = members.iter().any(|(n, ntext, nlines)| {
            n.id != m.id
                && (n.received.unix > m.received.unix || (n.received.unix == m.received.unix && n.id > m.id))
                && ntext.len() > text.len()
                && (ntext.contains(text.as_str())
                    || (!opts.keep_original && quoted_with_changes(lines, nlines)))
        });
        if quoted_later {
            out.push(m.id);
        }
    }
    out
}

/// The Conversation Clean Up folder named in the options (a folder path, preferring `account`'s folders);
/// `None` when the option is empty (each item's account's Deleted Items).
fn clean_up_target(
    s: &Session,
    folder: &str,
    account: Option<AccountId>,
) -> Result<Option<FolderId>, CmdError> {
    let want = folder.trim().trim_matches('\\').trim();
    if want.is_empty() {
        return Ok(None);
    }
    let lower = want.to_lowercase();
    let mail_folder = |f: FolderId| s.mailbox.folder(f).is_some_and(|f| f.kind == FolderKind::Mail);
    account
        .and_then(|a| {
            s.mailbox
                .folders
                .values()
                .find(|f| f.account == a && s.mailbox.folder_path(f.id).to_lowercase() == lower)
                .map(|f| f.id)
        })
        .or_else(|| crate::cmd::folder::resolve_folder(s, &json!(want)))
        .filter(|f| mail_folder(*f))
        .map(Some)
        .ok_or_else(|| {
            CmdError::Failed(format!(
                "The Conversation Clean Up folder \"{want}\" was not found. Choose another one in File › Options \
                 › Mail."
            ))
        })
}

/// `dest`'s subfolder at `path` (folder names), created where missing.
fn ensure_subfolder(s: &mut Session, dest: FolderId, path: &[String]) -> Result<FolderId, CmdError> {
    let mut cur = dest;
    for name in path {
        let existing = s
            .mailbox
            .folders
            .values()
            .find(|f| f.parent == Some(cur) && f.name.eq_ignore_ascii_case(name))
            .map(|f| f.id);
        cur = match existing {
            Some(f) => f,
            None => s.mailbox.create_folder(cur, name)?,
        };
    }
    Ok(cur)
}

/// Folder names from just below `root` down to `folder` (empty for `root` itself or a folder outside it).
fn path_below(s: &Session, root: FolderId, folder: FolderId) -> Vec<String> {
    let mut names = Vec::new();
    let mut cur = Some(folder);
    for _ in 0..=s.mailbox.folders.len() {
        match cur {
            Some(c) if c == root => {
                names.reverse();
                return names;
            }
            Some(c) => {
                let Some(f) = s.mailbox.folder(c) else { break };
                names.push(f.name.clone());
                cur = f.parent;
            }
            None => break,
        }
    }
    Vec::new()
}

/// Moves redundant items to the Conversation Clean Up folder of the options (else their account's Deleted
/// Items). Cleaning a folder and its subfolders (`root`) with "recreate the folder hierarchy" on files items
/// of each subfolder in a subfolder of the same name below the destination (created where missing).
fn clean_up(
    s: &mut Session,
    convs: &[Conversation],
    candidate: &dyn Fn(&MailItem) -> bool,
    root: Option<FolderId>,
) -> CmdResult {
    let opts = s.settings.clean_up();
    let mut redundant = Vec::new();
    for c in convs {
        redundant.extend(redundant_members(s, c, candidate, &opts));
    }
    let order = list_order(s);
    let mut moved = Vec::new();
    for id in redundant {
        let Some((folder, account)) =
            s.mailbox.mail.get(&id).map(|m| (m.folder, s.mailbox.folder_account(m.folder)))
        else {
            continue;
        };
        let base = match clean_up_target(s, &opts.folder, account)? {
            Some(f) => f,
            None => match account.and_then(|a| s.mailbox.role_folder(a, FolderRole::DeletedItems)) {
                Some(f) => f,
                None => continue,
            },
        };
        if s.mailbox.is_in_subtree(folder, base) {
            // Already cleaned up into the destination (or below it).
            continue;
        }
        let dest = match root.filter(|_| opts.recreate_hierarchy) {
            Some(r) => {
                let path = path_below(s, r, folder);
                ensure_subfolder(s, base, &path)?
            }
            None => base,
        };
        moved.extend(s.mailbox.move_items(&[id], dest)?);
    }
    journal(s, &moved);
    reselect(s, &order);
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
    clean_up(s, &convs, &skip, None)
}

fn clean_up_folders(s: &mut Session, folders: Vec<FolderId>, root: Option<FolderId>) -> CmdResult {
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
    clean_up(s, &convs, &|m: &MailItem| folders.contains(&m.folder), root)
}

/// Setting that switches the Clean Up Folder prompt off.
pub const CLEAN_UP_DONT_ASK: &str = "state.confirm.cleanUpDontAsk";

fn clean_up_folder(s: &mut Session, p: &Value) -> CmdResult {
    let folder = param_folder(s, p)?.unwrap_or(s.selection.folder);
    if opt_bool(p, "confirm") != Some(true) && !s.settings.bool(CLEAN_UP_DONT_ASK) {
        let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
        return open_dialog(
            s,
            "confirm",
            json!({
                "title": "Clean Up Folder",
                "text": format!("Clean up \"{name}\"? Messages whose whole text is quoted in a later reply in the same conversation go to Deleted Items."),
                "command": "mail.cleanUpFolder",
                "params": {"folder": folder.0, "confirm": true},
                "dontAsk": CLEAN_UP_DONT_ASK,
            }),
        );
    }
    clean_up_folders(s, vec![folder], None)
}

fn clean_up_subfolders(s: &mut Session, p: &Value) -> CmdResult {
    let folder = param_folder(s, p)?.unwrap_or(s.selection.folder);
    let tree = s.mailbox.subtree(folder);
    clean_up_folders(s, tree, Some(folder))
}

// ---------------------------------------------------------------------------------------------------------
// Junk
// ---------------------------------------------------------------------------------------------------------

/// Upper bound for an imported list file.
const MAX_JUNK_LIST_FILE: u64 = 4 * 1024 * 1024;

fn not_junk(s: &mut Session, ids: &[ItemId]) -> Result<Vec<ItemId>, CmdError> {
    let in_junk: Vec<ItemId> =
        ids.iter().copied().filter(|id| item_role(s, *id) == Some(FolderRole::JunkEmail)).collect();
    move_to_role(s, &in_junk, FolderRole::Inbox)
}

/// `params.list` as a junk list.
fn junk_list(p: &Value) -> Result<JunkList, CmdError> {
    let key = req_str(p, "list")?;
    JunkList::parse(key).ok_or_else(|| {
        CmdError::Params(format!(
            "unknown list `{key}` (safeSenders, safeRecipients, blockedSenders, blockedTlds, blockedEncodings)"
        ))
    })
}

/// `params.path` as a non-empty file path.
fn junk_path(p: &Value) -> Result<std::path::PathBuf, CmdError> {
    let path = req_str(p, "path")?.trim();
    if path.is_empty() || path.contains('\0') {
        return Err(CmdError::Params("`path` must name a file".into()));
    }
    Ok(std::path::PathBuf::from(path))
}

/// Junk Email Options as JSON (the settings plus the choices offered on the International tab).
fn junk_settings_json(s: &Session) -> Value {
    let mut v = serde_json::to_value(&s.mailbox.junk).unwrap_or_else(|_| json!({}));
    if let Some(o) = v.as_object_mut() {
        o.insert("levelIndex".into(), json!(s.mailbox.junk.level.index()));
    }
    v
}

/// Applies `mail.junk {"action":"set"}`: any of `level`, the option flags and whole lists, from
/// `params.settings` or the params themselves. Invalid list entries are skipped and reported.
fn junk_set(s: &mut Session, p: &Value) -> CmdResult {
    let src = p.get("settings").filter(|v| v.is_object()).unwrap_or(p);
    let mut next = s.mailbox.junk.clone();
    if let Some(v) = src.get("level").filter(|v| !v.is_null()) {
        let parsed = match v {
            Value::String(t) => JunkLevel::parse(t),
            Value::Number(n) => {
                n.as_u64().and_then(|i| usize::try_from(i).ok()).and_then(|i| JunkLevel::ALL.get(i).copied())
            }
            _ => None,
        };
        next.level = parsed.ok_or_else(|| {
            CmdError::Params(format!("unknown junk level {v} (noFiltering, low, high, safeListsOnly)"))
        })?;
    }
    let flags: [(&str, &mut bool); 5] = [
        ("deleteSuspected", &mut next.delete_suspected),
        ("disableLinks", &mut next.disable_links),
        ("warnSuspiciousDomains", &mut next.warn_suspicious_domains),
        ("trustContacts", &mut next.trust_contacts),
        ("addRecipientsToSafe", &mut next.add_recipients_to_safe),
    ];
    for (key, slot) in flags {
        if let Some(v) = src.get(key).filter(|v| !v.is_null()) {
            *slot = v.as_bool().ok_or_else(|| CmdError::Params(format!("`{key}` must be true or false")))?;
        }
    }
    let mut rejected: Vec<String> = Vec::new();
    for l in JunkList::ALL {
        let Some(v) = src.get(l.key()).filter(|v| !v.is_null()) else { continue };
        let Some(arr) = v.as_array() else {
            return Err(CmdError::Params(format!("`{}` must be a list of entries", l.key())));
        };
        next.list_mut(l).clear();
        for e in arr {
            let text = e.as_str().unwrap_or_default();
            match next.add(l, text) {
                Ok(_) => {}
                Err(_) if rejected.len() < 100 => rejected.push(text.chars().take(80).collect()),
                Err(_) => {}
            }
        }
    }
    s.mailbox.junk = next;
    let mut out = junk_settings_json(s);
    if let Some(o) = out.as_object_mut() {
        o.insert("rejected".into(), json!(rejected));
    }
    Ok(out)
}

/// `params.entry` / `params.entries` as strings.
fn junk_entries(p: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(e) = p.get("entry").and_then(Value::as_str) {
        out.push(e.to_owned());
    }
    if let Some(a) = p.get("entries").and_then(Value::as_array) {
        out.extend(a.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    out
}

/// Junk E-mail Options and the junk actions: `options` (dialog), `get`, `set`, `add`/`remove` list entries,
/// `import`/`export` a list from/to a `.txt` file, `moveToJunk`, `notJunk` (back to the Inbox; the senders
/// join the Safe Senders List unless `addToSafe` is false), `run` (Run Junk Filter on a folder) and
/// `classify` (verdicts with their scoring signals).
fn junk(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    match opt_str(p, "action").unwrap_or("options") {
        "options" => open_dialog(s, "junkOptions", json!({})),
        "get" => Ok(junk_settings_json(s)),
        "set" => junk_set(s, p),
        "add" | "remove" => {
            let list = junk_list(p)?;
            let entries = junk_entries(p);
            if entries.is_empty() {
                return Err(CmdError::Params("`entry` or `entries` is required".into()));
            }
            let adding = opt_str(p, "action") == Some("add");
            let mut changed = 0usize;
            for e in &entries {
                let did = if adding { s.mailbox.junk.add(list, e)? } else { s.mailbox.junk.remove(list, e) };
                changed = changed.saturating_add(usize::from(did));
            }
            Ok(json!({"list": list.key(), "changed": changed, "entries": s.mailbox.junk.list(list)}))
        }
        "import" => {
            let list = junk_list(p)?;
            let path = junk_path(p)?;
            let bytes = mailcraft_store::journal::read_capped(&path, MAX_JUNK_LIST_FILE)
                .map_err(|e| CmdError::Failed(format!("could not read {}: {e}", path.display())))?
                .ok_or_else(|| CmdError::Failed(format!("{} does not exist", path.display())))?;
            let text = String::from_utf8_lossy(&bytes);
            let added = s.mailbox.junk.import_text(list, &text);
            Ok(json!({"list": list.key(), "added": added, "total": s.mailbox.junk.list(list).len()}))
        }
        "export" => {
            let list = junk_list(p)?;
            let path = junk_path(p)?;
            let text = s.mailbox.junk.export_text(list);
            mailcraft_store::journal::write_atomic(&path, |w| std::io::Write::write_all(w, text.as_bytes()))
                .map_err(|e| CmdError::Failed(format!("could not write {}: {e}", path.display())))?;
            Ok(
                json!({"list": list.key(), "exported": s.mailbox.junk.list(list).len(), "path": path.display().to_string()}),
            )
        }
        "moveToJunk" | "junk" => {
            let moved = move_to_role(s, &ids, FolderRole::JunkEmail)?;
            Ok(json!({"moved": moved.len()}))
        }
        "notJunk" => {
            let add = opt_bool(p, "addToSafe").unwrap_or(true);
            let mut trusted = Vec::new();
            if add {
                let senders: Vec<String> = ids
                    .iter()
                    .filter(|id| item_role(s, **id) == Some(FolderRole::JunkEmail))
                    .filter_map(|id| {
                        s.mailbox.mail.get(id).and_then(|m| m.from.as_ref()).map(|a| a.addr.clone())
                    })
                    .collect();
                for a in senders {
                    s.mailbox.junk.remove(JunkList::BlockedSenders, &a);
                    if matches!(s.mailbox.junk.add(JunkList::SafeSenders, &a), Ok(true)) {
                        trusted.push(a.to_lowercase());
                    }
                }
            }
            let moved = not_junk(s, &ids)?;
            Ok(json!({"moved": moved.len(), "trusted": trusted}))
        }
        "run" => {
            let folder = param_folder(s, p)?.unwrap_or(s.selection.folder);
            if !s.mailbox.folder(folder).is_some_and(|f| f.kind == FolderKind::Mail) {
                return Err(CmdError::Params("Run Junk Filter needs a mail folder".into()));
            }
            Ok(crate::junk::run_filter(s, folder).to_json())
        }
        "classify" => {
            let verdicts: Vec<Value> = ids
                .iter()
                .filter_map(|id| crate::junk::classify(s, *id).map(|v| crate::junk::verdict_json(*id, &v)))
                .collect();
            Ok(json!({"verdicts": verdicts}))
        }
        other => Err(CmdError::Params(format!("unknown junk action `{other}`"))),
    }
}

/// The group or mailing list a message came through: its List-Post address, else the first To/Cc address
/// that is not one of the user's own.
fn list_entry(s: &Session, item: &MailItem) -> Option<String> {
    if let Some(a) = crate::junk::list_address(item) {
        return Some(a);
    }
    let mine = my_addresses(s);
    item.to
        .iter()
        .chain(item.cc.iter())
        .map(|a| a.addr.trim().to_lowercase())
        .find(|a| !a.is_empty() && !mine.contains(a))
}

/// Block Sender adds the sender to the Blocked Senders List and moves the selection to Junk Email. Never
/// Block Sender / Sender's Domain add the address / `@domain` to the Safe Senders List, Never Block this
/// Group or Mailing List adds the list address to the Safe Recipients List; those also move the messages
/// back to the Inbox when they are in Junk Email.
fn block(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let kind = req_str(p, "kind")?;
    let Some(item) = s.mailbox.mail.get(&focus) else {
        return Err(CmdError::Failed("message not found".into()));
    };
    let sender = item.from.as_ref().map(|a| a.addr.trim().to_lowercase()).unwrap_or_default();
    let group = list_entry(s, item);
    let ids = mail_targets(s, p);
    let no_sender = || CmdError::Failed("The message has no sender address.".into());
    match kind {
        "sender" => {
            if sender.is_empty() {
                return Err(no_sender());
            }
            s.mailbox.junk.remove(JunkList::SafeSenders, &sender);
            s.mailbox.junk.add(JunkList::BlockedSenders, &sender)?;
            let moved = move_to_role(s, &ids, FolderRole::JunkEmail)?;
            Ok(json!({"blocked": sender, "moved": moved.len()}))
        }
        "neverSender" | "neverDomain" | "neverList" => {
            let (list, entry) = match kind {
                "neverSender" => (JunkList::SafeSenders, sender),
                "neverDomain" => (
                    JunkList::SafeSenders,
                    mailcraft_model::junk::domain_of(&sender).map(|d| format!("@{d}")).unwrap_or_default(),
                ),
                _ => (JunkList::SafeRecipients, group.unwrap_or_default()),
            };
            if entry.is_empty() {
                return Err(if kind == "neverList" {
                    CmdError::Failed("The message names no group or mailing list.".into())
                } else {
                    no_sender()
                });
            }
            if list == JunkList::SafeSenders {
                s.mailbox.junk.remove(JunkList::BlockedSenders, &entry);
            }
            s.mailbox.junk.add(list, &entry)?;
            let moved = not_junk(s, &ids)?;
            Ok(json!({"safe": entry, "kind": kind, "list": list.key(), "moved": moved.len()}))
        }
        other => Err(CmdError::Params(format!("unknown block kind `{other}`"))),
    }
}

/// Report Junk / Report Phishing: marks the messages read and moves them to Junk Email.
/// Setting: where reported messages are forwarded (as attachments) for the mail provider's abuse desk.
pub const REPORT_ADDRESS: &str = "mail.report.address";

/// Report Junk: the messages are marked read and go to Junk Email. Report Phishing (after a confirmation):
/// they are marked read and go to Deleted Items, and their senders are blocked so later mail from them is
/// treated as junk. With an abuse address set (Options › Mail › Report), a new message to that address
/// opens with the reported messages attached, ready to send.
fn report(s: &mut Session, p: &Value) -> CmdResult {
    let kind = opt_str(p, "kind").unwrap_or("junk");
    if !matches!(kind, "junk" | "phishing") {
        return Err(CmdError::Params(format!("unknown report kind `{kind}`")));
    }
    let ids = mail_targets(s, p);
    if ids.is_empty() {
        return Err(CmdError::Disabled("No message is selected.".into()));
    }
    if kind == "phishing" && opt_bool(p, "confirm") != Some(true) {
        let n = ids.len();
        let what = if n == 1 { "this message".to_owned() } else { format!("these {n} messages") };
        return open_dialog(
            s,
            "confirm",
            json!({
                "title": "Report Phishing",
                "text": format!("Report {what} as phishing? It goes to Deleted Items and its sender is blocked."),
                "command": "mail.report",
                "params": {"kind": "phishing", "ids": ids_json(&ids), "confirm": true},
            }),
        );
    }
    // The report copy is made before the messages move (the forward reads them where they are).
    let address = s.settings.text(REPORT_ADDRESS).trim().to_owned();
    let mut compose = None;
    let mut report_error = None;
    if !address.is_empty()
        && let Some(run) = s.registry.get("mail.forwardAsAttachment").map(|c| c.run)
    {
        // The messages are reported even when the copy for the abuse desk cannot be made (too many or too
        // large to attach): the user hears why.
        match run(s, &json!({"ids": ids_json(&ids)})) {
            Ok(r) => {
                if let Some(cid) = r.get("compose").and_then(Value::as_u64)
                    && let Some(c) = s.composes.get_mut(&cid)
                {
                    c.to = address.clone();
                    let label = if kind == "phishing" { "Phishing report" } else { "Junk report" };
                    c.subject = format!("{label}: {}", c.subject.trim_start_matches("FW: "));
                    compose = Some(cid);
                }
            }
            Err(e) => {
                let why = format!("The report to {address} was not prepared: {e}");
                s.notify(Notice::Info(why.clone()));
                report_error = Some(why);
            }
        }
    }
    let read = s.mailbox.set_read(&ids, true);
    journal(s, &read);
    let mut blocked = Vec::new();
    let mut not_blocked = Vec::new();
    let role = if kind == "phishing" {
        // Phishing often forges the user's own address as the sender: blocking it would send the user's
        // own mail (notes to self, copies to self) to Junk from then on, so own addresses are left alone.
        let mine = my_addresses(s);
        for id in &ids {
            let sender = s
                .mailbox
                .mail
                .get(id)
                .and_then(|m| m.from.as_ref())
                .map(|a| a.addr.trim().to_lowercase())
                .unwrap_or_default();
            if sender.is_empty() || blocked.contains(&sender) || not_blocked.contains(&sender) {
                continue;
            }
            if mine.contains(&sender) {
                not_blocked.push(sender);
                continue;
            }
            s.mailbox.junk.remove(JunkList::SafeSenders, &sender);
            if s.mailbox.junk.add(JunkList::BlockedSenders, &sender).is_ok() {
                blocked.push(sender);
            }
        }
        FolderRole::DeletedItems
    } else {
        FolderRole::JunkEmail
    };
    let moved = move_to_role(s, &ids, role)?;
    Ok(json!({
        "reported": kind,
        "moved": moved.len(),
        "blocked": blocked,
        "notBlocked": not_blocked,
        "compose": compose,
        "reportError": report_error,
    }))
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
/// Quick Print: the messages in Memo style straight to the system's default printer (where the session
/// allows direct printing and a default printer exists), else the Print page with Memo chosen. A printer or
/// PDF path in the params prints there.
fn quick_print(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    let raw: Vec<u64> = ids.iter().map(|i| i.0).collect();
    let explicit = ["printer", "path", "pdf_path"].iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null()));
    if !explicit && !raw.is_empty() && s.direct_print && crate::cmd::print::has_default_printer() {
        let params = json!({"ids": raw, "style": "memo", "printer": crate::cmd::print::DEFAULT_PRINTER});
        if let Some(done) = crate::cmd::print::forward(s, &params, &raw) {
            return done;
        }
    }
    if explicit {
        let mut params = p.as_object().cloned().unwrap_or_default();
        params.entry("style").or_insert_with(|| json!("memo"));
        let params = Value::Object(params);
        if let Some(done) = crate::cmd::print::forward(s, &params, &raw) {
            return done;
        }
    }
    open_dialog(s, "print", json!({"ids": ids_json(&ids), "quick": true, "style": "memo"}))
}

/// Edit Message: with `subject`/`text`/`html` saves the edits to the stored item; otherwise opens the Edit
/// Message window (`editMessage`: subject and text of the message, to edit and save).
fn edit_message(s: &mut Session, p: &Value) -> CmdResult {
    let Some(focus) = primary_item(s, p) else {
        return Err(CmdError::Disabled("No message is selected.".into()));
    };
    let (subject, text, html) = (opt_str(p, "subject"), opt_str(p, "text"), opt_str(p, "html"));
    if subject.is_none() && text.is_none() && html.is_none() {
        let Some(m) = s.mailbox.mail.get(&focus) else {
            return Err(CmdError::Failed("message not found".into()));
        };
        let params = json!({
            "id": focus.0,
            "subject": m.subject,
            "text": m.body_text(),
            "html": m.body.html.is_some(),
        });
        return open_dialog(s, "editMessage", params);
    }
    if subject.is_some_and(|x| x.chars().count() > MAX_EDIT_SUBJECT_CHARS)
        || text.map_or(0, str::len).saturating_add(html.map_or(0, str::len)) > MAX_EDIT_BODY_BYTES
    {
        return Err(CmdError::Params("the edited message is too large".into()));
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
        let headers = s.mailbox.mail.get(&focus).map(|m| internet_headers(s, m)).unwrap_or_default();
        return open_dialog(s, "messageOptions", json!({"id": focus.0, "headers": headers}));
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

/// Longest header block shown by Message Options (bytes).
const MAX_HEADER_BYTES: usize = 256 * 1024;

/// The Internet headers of a message, as received: the header block of its source (the stored original or
/// its blob in the store; a message without either gets the headers rebuilt from its fields). Unfolded
/// lines are kept as they are; the text is capped at [`MAX_HEADER_BYTES`].
pub(crate) fn internet_headers(s: &Session, m: &MailItem) -> String {
    let stored = match &s.store {
        Some(st) if m.raw.is_none() => st.head_of(m, MAX_HEADER_BYTES).ok().flatten(),
        _ => m.raw.as_ref().map(|r| r.get(..r.len().min(MAX_HEADER_BYTES)).unwrap_or_default().to_vec()),
    };
    let raw = match stored {
        Some(r) if !r.is_empty() => r,
        _ => item_source(m),
    };
    header_block(&raw)
}

/// The header block of an RFC 5322 message (everything before the first empty line), as text.
fn header_block(raw: &[u8]) -> String {
    let mut end = raw.len().min(MAX_HEADER_BYTES);
    let head = raw.get(..end).unwrap_or_default();
    for (i, w) in head.windows(2).enumerate() {
        if w == b"\n\n" {
            end = i.saturating_add(1);
            break;
        }
        if w == b"\n\r" && head.get(i.saturating_add(2)) == Some(&b'\n') {
            end = i.saturating_add(1);
            break;
        }
    }
    let text = String::from_utf8_lossy(raw.get(..end).unwrap_or_default()).replace("\r\n", "\n");
    text.trim_end_matches('\n').to_owned()
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

/// `mail.pin`: pins the target messages to the top of their folder's list, or unpins them when all are
/// pinned (`on` sets the state). The pin is kept on the message (it follows it to another folder) and is
/// local: servers have no pin flag.
fn pin(s: &mut Session, p: &Value) -> CmdResult {
    let ids = mail_targets(s, p);
    if ids.is_empty() {
        return Err(CmdError::Disabled("No message is selected.".into()));
    }
    let on = match p.get("on") {
        None | Some(Value::Null) => !ids.iter().all(|i| s.mailbox.mail.get(i).is_some_and(|m| m.pinned)),
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(CmdError::Params("`on` must be true or false".into())),
    };
    for id in &ids {
        if let Some(m) = s.mailbox.mail.get_mut(id) {
            m.pinned = on;
        }
    }
    let max = crate::cmd::view::mail::MAX_PINNED;
    let folders: BTreeSet<FolderId> =
        ids.iter().filter_map(|i| s.mailbox.mail.get(i).map(|m| m.folder)).collect();
    if on && folders.iter().any(|f| crate::cmd::view::mail::pinned_in(s, *f) > max) {
        // The session rolls the change back.
        return Err(CmdError::Params(format!("a folder keeps at most {max} pinned messages")));
    }
    Ok(json!({ "on": on, "ids": ids.iter().map(|i| i.0).collect::<Vec<_>>() }))
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
            "mail.pin",
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
    fn always_move_conversation_rule_follows_message_ids_not_the_subject() {
        let mut f = fx();
        f.s.mailbox.mail.get_mut(&f.c).unwrap().message_id = Some("lunch-1@example.com".into());
        let proj = f.s.mailbox.create_folder(f.inbox, "Lunch stuff").unwrap();
        f.s.selection.items = vec![f.c];
        f.s.selection.focus = Some(f.c);
        always_move_conversation(&mut f.s, &json!({"folder": proj.0})).unwrap();
        let rule = f.s.mailbox.rules.last().unwrap().clone();
        assert_eq!(
            rule.conditions,
            vec![mailcraft_model::RuleCondition::HeaderContains(vec!["<lunch-1@example.com>".into()])]
        );
        let ctx = mailcraft_model::rule::RuleContext::default();
        let mut reply = msg(f.inbox, "Re: Lunch", "dan@example.com", "Count me in", at(20736, 9));
        reply.references = vec!["lunch-1@example.com".into()];
        assert!(rule.conditions[0].matches(&reply, &ctx));
        let unrelated = msg(f.inbox, "Lunch", "erin@example.com", "Different lunch", at(20736, 10));
        assert!(!rule.conditions[0].matches(&unrelated, &ctx), "same subject, other conversation");
    }

    /// Params of the last `name` dialog the session asked for.
    fn last_dialog(s: &Session, name: &str) -> Value {
        s.notices
            .iter()
            .rev()
            .find_map(|n| match n {
                Notice::OpenDialog(d, p) if d == name => Some(p.clone()),
                _ => None,
            })
            .unwrap_or(Value::Null)
    }

    #[test]
    fn new_items_presets() {
        let mut f = fx();
        let r = f.s.run("mail.newItem", &json!({"kind": "allDayEvent"})).unwrap();
        assert_eq!(r["dialog"], "newAppointment");
        let p = last_dialog(&f.s, "newAppointment");
        assert_eq!(p["allDay"], true);
        assert!(p.get("recurrence").is_none() && p.get("folder").is_none());
        f.s.run("mail.newItem", &json!({"kind": "recurringMeeting"})).unwrap();
        let p = last_dialog(&f.s, "newMeeting");
        assert_eq!(p["recurrence"], "weekly");
        assert!(p.get("allDay").is_none());
        f.s.run("mail.newItem", &json!({"kind": "recurringEvent", "start": "2026-10-12T00:00"})).unwrap();
        let p = last_dialog(&f.s, "newAppointment");
        assert_eq!((p["allDay"].clone(), p["recurrence"].clone()), (json!(true), json!("weekly")));
        assert_eq!(p["start"], "2026-10-12T00:00");
        f.s.run("mail.newItem", &json!({"kind": "recurringAppointment"})).unwrap();
        assert_eq!(last_dialog(&f.s, "newAppointment")["recurrence"], "weekly");
        assert!(f.s.run("mail.newItem", &json!({"kind": "fax"})).is_err());
        // Opening a form changes nothing, so it leaves no undo step.
        assert!(f.s.undo.undo.is_empty());
    }

    #[test]
    fn meeting_with_all_invites_everyone_but_me() {
        let mut f = fx();
        let cal = role(&f.s, FolderRole::Calendar);
        let att = |name: &str, addr: &str, role| mailcraft_model::Attendee {
            mailbox: Address::new(Some(name), addr),
            role,
            ..Default::default()
        };
        use mailcraft_model::calendar::AttendeeRole;
        let id = ItemId(f.s.mailbox.alloc_id());
        f.s.mailbox.appointments.insert(
            id,
            mailcraft_model::Appointment {
                id,
                folder: cal,
                subject: "Budget review".into(),
                location: "Room 4".into(),
                organizer: Some(Address::new(Some("Dana"), "dana@example.org")),
                attendees: vec![
                    att("Test User", "Test.User@example.net", AttendeeRole::Required),
                    att("Eve", "eve@example.com", AttendeeRole::Required),
                    att("Dana", "DANA@example.org", AttendeeRole::Optional),
                    att("Frank", "frank@example.com", AttendeeRole::Optional),
                    att("Room 4", "room4@example.com", AttendeeRole::Resource),
                ],
                is_meeting: true,
                ..Default::default()
            },
        );
        f.s.selection.items = vec![id];
        f.s.selection.focus = Some(id);
        // The Calendar's New Items › Meeting with All is the same, offered while such an item is selected.
        assert!(f.s.enabled("calendar.meetingWithAll").is_none());
        assert_eq!(f.s.run("calendar.meetingWithAll", &json!({})).unwrap()["dialog"], "newMeeting");
        assert_eq!(last_dialog(&f.s, "newMeeting")["optional"], json!(["Frank <frank@example.com>"]));
        let r = f.s.run("mail.newItem", &json!({"kind": "meetingWithAll"})).unwrap();
        assert_eq!(r["dialog"], "newMeeting");
        let p = last_dialog(&f.s, "newMeeting");
        assert_eq!(p["subject"], "Budget review");
        assert_eq!(p["attendees"], json!(["Dana <dana@example.org>", "Eve <eve@example.com>"]));
        assert_eq!(p["optional"], json!(["Frank <frank@example.com>"]));
        // A selected message: the people on it (Reply with Meeting).
        f.s.selection.items = vec![f.c];
        f.s.selection.focus = Some(f.c);
        f.s.notices.clear();
        f.s.run("mail.newItem", &json!({"kind": "meetingWithAll"})).unwrap();
        let p = last_dialog(&f.s, "newMeeting");
        assert_eq!(p["source"], f.c.0);
        assert!(p["attendees"].to_string().contains("alice@example.com"));
        // Nothing selected: disabled (as observed).
        f.s.selection.items.clear();
        f.s.selection.focus = None;
        assert!(f.s.enabled("calendar.meetingWithAll").is_some());
        // An appointment with nobody else on it.
        let alone = ItemId(f.s.mailbox.alloc_id());
        let cal = f.s.mailbox.appointments[&id].folder;
        f.s.mailbox.appointments.insert(
            alone,
            mailcraft_model::Appointment {
                id: alone,
                folder: cal,
                subject: "Dentist".into(),
                ..Default::default()
            },
        );
        f.s.selection.items = vec![alone];
        f.s.selection.focus = Some(alone);
        assert!(f.s.enabled("calendar.meetingWithAll").is_some());
        f.s.selection.items.clear();
        f.s.selection.focus = None;
        assert!(matches!(
            f.s.run("mail.newItem", &json!({"kind": "meetingWithAll"})),
            Err(CmdError::Disabled(_))
        ));
    }

    #[test]
    fn post_in_this_folder_saves_a_post_in_the_mail_folder() {
        let mut f = fx();
        let r = f.s.run("mail.newItem", &json!({"kind": "post"})).unwrap();
        assert_eq!(r["dialog"], "newPost");
        assert_eq!(last_dialog(&f.s, "newPost")["folder"], f.inbox.0);
        assert!(f.s.mailbox.notes.is_empty());
        let before = f.s.mailbox.mail.len();
        let r =
            f.s.run(
                "mail.newItem",
                &json!({"kind": "post", "subject": " Team lunch ", "text": "Friday at noon."}),
            )
            .unwrap();
        let id = ItemId(r["id"].as_u64().unwrap());
        let m = &f.s.mailbox.mail[&id];
        assert_eq!(m.folder, f.inbox);
        assert_eq!(m.subject, "Team lunch");
        assert!(m.to.is_empty() && m.cc.is_empty() && m.bcc.is_empty());
        assert_eq!(m.from.as_ref().map(|a| a.addr.as_str()), Some("test.user@example.net"));
        assert!(m.read && !m.is_draft && m.message_id.is_some());
        assert_eq!(m.preview, "Friday at noon.");
        assert_eq!(f.s.mailbox.mail.len(), before + 1);
        assert!(f.s.mailbox.notes.is_empty(), "a post is not a note");
        // One undo step takes it back.
        f.s.run("app.undo", &json!({})).unwrap();
        assert!(!f.s.mailbox.mail.contains_key(&id));
        // HTML posts keep their text too.
        let r = f.s.run("mail.newItem", &json!({"kind": "post", "html": "<p>Hi <b>all</b></p>"})).unwrap();
        let m = &f.s.mailbox.mail[&ItemId(r["id"].as_u64().unwrap())];
        assert_eq!(m.body.format, BodyFormat::Html);
        assert!(m.body.text.as_deref().unwrap_or("").contains("Hi all"));
        // Only mail folders take posts.
        let cal = role(&f.s, FolderRole::Calendar);
        assert!(f.s.run("mail.newItem", &json!({"kind": "post", "folder": cal.0})).is_err());
        f.s.selection.folder = cal;
        assert!(f.s.run("mail.newItem", &json!({"kind": "post", "text": "x"})).is_err());
        assert!(
            f.s.run(
                "mail.newItem",
                &json!({"kind": "post", "folder": f.inbox.0, "text": "x".repeat(MAX_EDIT_BODY_BYTES + 1)})
            )
            .is_err()
        );
    }

    #[test]
    fn other_sender_override() {
        let mut f = fx();
        let senders = vec!["alice@example.com".to_owned()];
        assert_eq!(focus_override(&senders, &f.s.mailbox.mail[&f.c]), Some(FocusClass::Other));
        assert_eq!(focus_override(&senders, &f.s.mailbox.mail[&f.a]), None);
        assert_eq!(focus_override(&[], &f.s.mailbox.mail[&f.c]), None);
        f.s.selection.items = vec![f.c];
        f.s.selection.focus = Some(f.c);
        let r = f.s.run("mail.alwaysMoveToOther", &json!({})).unwrap();
        let defined = crate::settings::def(OTHER_SENDERS_SETTING).is_some();
        assert_eq!(r["persisted"], defined);
        // Later mail from the sender: classified Other when the override is kept.
        let later =
            f.s.mailbox.add_mail(msg(f.inbox, "Lunch again", "Alice@Example.com", "Tuesday?", at(20736, 9)));
        let changed = apply_focus_overrides(&mut f.s, &[later, f.a]);
        if defined {
            assert_eq!(other_senders(&f.s), senders);
            assert_eq!(changed, vec![later]);
            assert_eq!(f.s.mailbox.mail[&later].focus, FocusClass::Other);
        } else {
            assert!(changed.is_empty());
        }
        assert_eq!(f.s.mailbox.mail[&f.a].focus, FocusClass::Focused);
    }

    #[test]
    fn focused_sender_override_wins_back_a_sender() {
        let mut f = fx();
        f.s.selection.items = vec![f.c];
        f.s.selection.focus = Some(f.c);
        f.s.run("mail.alwaysMoveToOther", &json!({})).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.c].focus, FocusClass::Other);
        // From the Other tab: Move to Focused moves one message, Always Move to Focused the sender.
        f.s.run("mail.moveToFocused", &json!({"ids": [f.c.0]})).unwrap();
        assert_eq!(f.s.mailbox.mail[&f.c].focus, FocusClass::Focused);
        f.s.run("mail.moveToOther", &json!({"ids": [f.c.0]})).unwrap();
        let r = f.s.run("mail.alwaysMoveToFocused", &json!({"ids": [f.c.0]})).unwrap();
        assert_eq!(r["persisted"], true);
        assert_eq!(f.s.mailbox.mail[&f.c].focus, FocusClass::Focused);
        assert!(other_senders(&f.s).is_empty(), "the sender leaves the Other list");
        assert_eq!(focused_senders(&f.s), vec!["alice@example.com".to_owned()]);
        let later =
            f.s.mailbox.add_mail(msg(f.inbox, "Lunch again", "Alice@Example.com", "Tuesday?", at(20736, 9)));
        f.s.mailbox.mail.get_mut(&later).unwrap().focus = FocusClass::Other;
        assert_eq!(apply_focus_overrides(&mut f.s, &[later]), vec![later]);
        assert_eq!(f.s.mailbox.mail[&later].focus, FocusClass::Focused);
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
        // Turning Ignore on asks first; nothing changes until the answer comes back.
        let r = ignore(&mut f.s, &Value::Null).unwrap();
        assert_eq!(r["dialog"], "confirm");
        assert!(!f.s.mailbox.mail[&f.b].ignored);
        let r = ignore(&mut f.s, &json!({"confirm": true})).unwrap();
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
    fn message_options_shows_the_received_headers() {
        let mut f = fx();
        let raw = b"Received: from mx.example.net by mail.example.org\r\n\tid 42; Thu, 8 Oct 2026 07:30:00 +0000\r\nFrom: Ann <ann@example.com>\r\nSubject: Hi\r\nX-Spam-Score: 0.1\r\n\r\nBody text\r\nFrom: not a header\r\n";
        f.s.mailbox.mail.get_mut(&f.a).unwrap().raw = Some(raw.to_vec());
        message_options(&mut f.s, &json!({"ids": [f.a.0]})).unwrap();
        let Some(Notice::OpenDialog(name, p)) = f.s.notices.last() else { panic!("no dialog") };
        assert_eq!(name, "messageOptions");
        let h = p["headers"].as_str().unwrap();
        assert!(h.starts_with("Received: from mx.example.net"), "{h}");
        assert!(h.contains("\tid 42;"), "folded lines stay");
        assert!(h.contains("X-Spam-Score: 0.1"));
        assert!(!h.contains("Body text") && !h.contains("not a header"), "{h}");
        // Without a stored source the headers are rebuilt from the item.
        message_options(&mut f.s, &json!({"ids": [f.b.0]})).unwrap();
        let Some(Notice::OpenDialog(_, p)) = f.s.notices.last() else { panic!("no dialog") };
        assert!(p["headers"].as_str().unwrap().contains("Subject: "), "{p}");
        assert_eq!(header_block(b"A: 1\nB: 2\n\nbody"), "A: 1\nB: 2");
        assert_eq!(header_block(b""), "");
    }

    #[test]
    fn archive_remembers_the_folder_picked_for_an_account_without_one() {
        let mut f = fx();
        let archive_role = role(&f.s, FolderRole::Archive);
        f.s.mailbox.folders.get_mut(&archive_role).unwrap().role = None;
        let r = archive(&mut f.s, &json!({"ids": [f.a.0]})).unwrap();
        assert_eq!(r["dialog"], "archiveFolder");
        let keep = f.s.mailbox.create_folder(f.inbox, "Kept").unwrap();
        let r = archive(&mut f.s, &json!({"ids": [f.a.0], "folder": keep.0, "remember": true})).unwrap();
        assert_eq!(r["remembered"], true);
        assert_eq!(folder_of(&f.s, f.a), keep);
        // The next archive goes straight there.
        let r = archive(&mut f.s, &json!({"ids": [f.b.0]})).unwrap();
        assert_eq!(r["moved"], 1);
        assert_eq!(folder_of(&f.s, f.b), keep);
        // An account that has an Archive folder keeps it.
        let other = f.s.mailbox.create_folder(f.inbox, "Other").unwrap();
        let r = archive(&mut f.s, &json!({"ids": [f.c.0], "folder": other.0, "remember": true})).unwrap();
        assert_eq!(r["remembered"], false);
        assert!(f.s.mailbox.folder(other).unwrap().role.is_none());
    }

    #[test]
    fn follow_up_kind_names_the_menu_entry_a_flag_came_from() {
        let mut f = fx();
        assert_eq!(follow_up_kind(&f.s, &FlagState::None), None);
        for key in ["today", "nextWeek", "noDate"] {
            f.s.run("mail.flag", &json!({"ids": [f.a.0], "kind": key})).unwrap();
            assert_eq!(follow_up_kind(&f.s, &f.s.mailbox.mail[&f.a].flag), Some(key), "{key}");
        }
        assert_eq!(follow_up_kind(&f.s, &FlagState::Complete { completed: None }), None);
    }

    #[test]
    fn ignore_prompt_can_be_switched_off() {
        let mut f = fx();
        let r = ignore(&mut f.s, &json!({"ids": [f.c.0]})).unwrap();
        assert_eq!(r["dialog"], "confirm");
        let Some(Notice::OpenDialog(_, p)) = f.s.notices.last() else { panic!("no dialog") };
        assert_eq!(p["command"], "mail.ignore");
        assert_eq!(p["params"]["confirm"], true);
        assert_eq!(p["dontAsk"], IGNORE_DONT_ASK);
        f.s.settings.set(IGNORE_DONT_ASK, &json!(true)).unwrap();
        assert_eq!(ignore(&mut f.s, &json!({"ids": [f.c.0]})).unwrap()["ignored"], true);
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
        assert_eq!(clean_up_folder(&mut f.s, &Value::Null).unwrap()["dialog"], "confirm", "asks first");
        let r = clean_up_folder(&mut f.s, &json!({"confirm": true})).unwrap();
        assert_eq!(r["moved"], 0);
        f.s.mailbox.mail.get_mut(&f.c).unwrap().categories.clear();
        f.s.settings.set(CLEAN_UP_DONT_ASK, &json!(true)).unwrap();
        let sub = f.s.mailbox.create_folder(f.inbox, "Sub").unwrap();
        f.s.mailbox.move_items(&[f.c], sub).unwrap();
        // Folder-only clean up of Inbox no longer sees c; subfolders variant does.
        assert_eq!(clean_up_folder(&mut f.s, &Value::Null).unwrap()["moved"], 0);
        assert_eq!(clean_up_subfolders(&mut f.s, &Value::Null).unwrap()["moved"], 1);
    }

    #[test]
    fn clean_up_follows_the_options() {
        let mut f = fx();
        f.s.selection.items = vec![f.b];
        f.s.selection.focus = Some(f.b);
        // "Don't move unread messages": c is unread.
        f.s.settings.set("mail.cleanup.keepUnread", &json!(true)).unwrap();
        assert_eq!(f.s.run("mail.cleanUpConversation", &json!({})).unwrap()["moved"], 0);
        // Flagged messages stay unless that option is off.
        f.s.settings.set("mail.cleanup.keepUnread", &json!(false)).unwrap();
        f.s.mailbox.mail.get_mut(&f.c).unwrap().flag =
            FlagState::Flagged { label: "Follow up".into(), start: None, due: None, reminder: None };
        assert_eq!(f.s.run("mail.cleanUpConversation", &json!({})).unwrap()["moved"], 0);
        f.s.settings.set("mail.cleanup.keepFlagged", &json!(false)).unwrap();
        // A folder that does not exist: refused, nothing moves.
        f.s.settings.set("mail.cleanup.folder", &json!("Nowhere")).unwrap();
        assert!(matches!(f.s.run("mail.cleanUpConversation", &json!({})), Err(CmdError::Failed(_))));
        assert_eq!(folder_of(&f.s, f.c), f.inbox);
        // The chosen folder.
        let cleaned = f.s.mailbox.create_folder(f.inbox, "Cleaned").unwrap();
        f.s.settings.set("mail.cleanup.folder", &json!("Inbox\\Cleaned")).unwrap();
        let r = f.s.run("mail.cleanUpConversation", &json!({})).unwrap();
        assert_eq!(r["moved"], 1);
        assert_eq!(folder_of(&f.s, f.c), cleaned);
        // Undo puts it back (one step).
        f.s.run("app.undo", &json!({})).unwrap();
        assert_eq!(folder_of(&f.s, f.c), f.inbox);
    }

    #[test]
    fn clean_up_moves_an_original_a_reply_modified_only_when_that_option_is_off() {
        let mut f = fx();
        // b answers inside the quote: every line of c is still there, with a comment between them.
        let c = f.s.mailbox.mail.get_mut(&f.c).unwrap();
        c.body.text = Some("Shall we meet at noon?\nThe usual place.".into());
        let b = f.s.mailbox.mail.get_mut(&f.b).unwrap();
        b.body.text =
            Some("See below.\n\n> Shall we meet at noon?\nYes, noon.\n> The usual place.\nFine.".into());
        f.s.selection.items = vec![f.b];
        f.s.selection.focus = Some(f.b);
        // On (default): the modified original stays.
        assert_eq!(f.s.run("mail.cleanUpConversation", &json!({})).unwrap()["moved"], 0);
        assert_eq!(folder_of(&f.s, f.c), f.inbox);
        // Off: it goes to Deleted Items.
        f.s.settings.set("mail.cleanup.keepOriginal", &json!(false)).unwrap();
        assert_eq!(f.s.run("mail.cleanUpConversation", &json!({})).unwrap()["moved"], 1);
        assert_eq!(folder_of(&f.s, f.c), role(&f.s, FolderRole::DeletedItems));
        // A reply that dropped part of the original does not count.
        let lines = |t: &[&str]| t.iter().map(|l| (*l).to_owned()).collect::<Vec<_>>();
        assert!(!quoted_with_changes(&lines(&["a", "b"]), &lines(&["a", "x", "c"])));
        assert!(quoted_with_changes(&lines(&["a", "b"]), &lines(&["a", "x", "b"])));
        assert!(!quoted_with_changes(&[], &lines(&["a"])));
    }

    #[test]
    fn clean_up_recreates_the_folder_hierarchy() {
        let mut f = fx();
        let sub = f.s.mailbox.create_folder(f.inbox, "Projects").unwrap();
        let deeper = f.s.mailbox.create_folder(sub, "Lunch").unwrap();
        f.s.mailbox.move_items(&[f.c], deeper).unwrap();
        let archive = role(&f.s, FolderRole::Archive);
        f.s.settings.set("mail.cleanup.folder", &json!("Archive")).unwrap();
        f.s.settings.set("mail.cleanup.recreateHierarchy", &json!(true)).unwrap();
        let r = f.s.run("mail.cleanUpSubfolders", &json!({"folder": f.inbox.0})).unwrap();
        assert_eq!(r["moved"], 1);
        let dest = folder_of(&f.s, f.c);
        assert_eq!(f.s.mailbox.folder_path(dest), "Archive\\Projects\\Lunch");
        assert!(f.s.mailbox.is_in_subtree(dest, archive));
        // Off: straight into the destination.
        f.s.mailbox.move_items(&[f.c], deeper).unwrap();
        f.s.settings.set("mail.cleanup.recreateHierarchy", &json!(false)).unwrap();
        f.s.run("mail.cleanUpSubfolders", &json!({"folder": f.inbox.0})).unwrap();
        assert_eq!(folder_of(&f.s, f.c), archive);
        // Already in the destination: left alone.
        assert_eq!(f.s.run("mail.cleanUpSubfolders", &json!({"folder": archive.0})).unwrap()["moved"], 0);
    }

    #[test]
    fn new_email_uses_the_current_folders_account() {
        let mut f = fx();
        let work = f.s.mailbox.add_account(mailcraft_model::Account {
            display_name: "Work".into(),
            address: "kim@work.example.org".into(),
            sender_name: "Kim".into(),
            ..Default::default()
        });
        f.s.mailbox.create_default_folders(work);
        let data = f.s.mailbox.add_account(mailcraft_model::Account {
            display_name: "Archive file".into(),
            ..Default::default()
        });
        f.s.mailbox.create_default_folders(data);
        let from = |s: &mut Session| {
            let id = s.run("mail.newEmail", &json!({})).unwrap()["compose"].as_u64().unwrap();
            s.composes[&id].from.clone()
        };
        f.s.selection.folder = f.s.mailbox.role_folder(work, FolderRole::Inbox).unwrap();
        assert_eq!(from(&mut f.s), "Kim <kim@work.example.org>");
        // A data file cannot send: the default account.
        f.s.selection.folder = f.s.mailbox.role_folder(data, FolderRole::Inbox).unwrap();
        assert_eq!(from(&mut f.s), "Test User <test.user@example.net>");
        // "Always use the default account".
        f.s.selection.folder = f.s.mailbox.role_folder(work, FolderRole::Inbox).unwrap();
        f.s.settings.set("mail.send.alwaysDefaultAccount", &json!(true)).unwrap();
        assert_eq!(from(&mut f.s), "Test User <test.user@example.net>");
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
        report(&mut f.s, &json!({"ids": [f.c.0], "kind": "junk"})).unwrap();
        assert_eq!(folder_of(&f.s, f.c), junkf);
        assert!(f.s.mailbox.mail[&f.c].read);
        assert!(block(&mut f.s, &json!({"kind": "everyone"})).is_err());
    }

    #[test]
    fn report_phishing_asks_then_deletes_and_blocks_and_can_forward_a_copy() {
        let mut f = fx();
        let sender = f.s.mailbox.mail[&f.c].from.as_ref().unwrap().addr.to_lowercase();
        let r = report(&mut f.s, &json!({"ids": [f.c.0], "kind": "phishing"})).unwrap();
        assert_eq!(r["dialog"], "confirm");
        assert_eq!(folder_of(&f.s, f.c), f.inbox, "nothing happens before the answer");
        f.s.settings.set(REPORT_ADDRESS, &json!("abuse@example.net")).unwrap();
        let r = report(&mut f.s, &json!({"ids": [f.c.0], "kind": "phishing", "confirm": true})).unwrap();
        assert_eq!(folder_of(&f.s, f.c), role(&f.s, FolderRole::DeletedItems));
        assert!(f.s.mailbox.mail[&f.c].read);
        assert_eq!(r["blocked"], json!([sender]));
        assert!(f.s.mailbox.junk.list(JunkList::BlockedSenders).contains(&sender));
        let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.to, "abuse@example.net");
        assert!(c.subject.starts_with("Phishing report: "), "{}", c.subject);
        assert_eq!(c.attachments.len(), 1);
        assert_eq!(c.attachments[0].mime_type, "message/rfc822");
    }

    /// Regression: reporting phishing that forged the user's own address blocked that address, sending the
    /// user's own mail to Junk from then on.
    #[test]
    fn report_phishing_never_blocks_my_own_address() {
        let mut f = fx();
        if let Some(m) = f.s.mailbox.mail.get_mut(&f.c) {
            m.from = Some(mailcraft_model::Address::new(Some("Me"), "Test.User@example.net"));
        }
        f.s.run(
            "mail.junk",
            &json!({"action": "add", "list": "safeSenders", "entry": "test.user@example.net"}),
        )
        .unwrap();
        let r =
            report(&mut f.s, &json!({"ids": [f.c.0, f.a.0], "kind": "phishing", "confirm": true})).unwrap();
        assert_eq!(r["blocked"], json!(["carol@example.com"]));
        assert_eq!(r["notBlocked"], json!(["test.user@example.net"]));
        assert!(
            !f.s.mailbox.junk.list(JunkList::BlockedSenders).contains(&"test.user@example.net".to_owned())
        );
        assert!(f.s.mailbox.junk.list(JunkList::SafeSenders).contains(&"test.user@example.net".to_owned()));
        assert_eq!(folder_of(&f.s, f.c), role(&f.s, FolderRole::DeletedItems), "it is still reported");
    }

    /// Regression: the abuse-desk copy of a message whose source had moved to the store's blobs was rebuilt
    /// from the stored fields, without the Received / Authentication-Results headers the desk needs.
    #[test]
    fn report_forwards_the_original_source_from_the_store() {
        const RAW: &[u8] = b"Received: from mx.example.org by mail.example.net; Thu, 08 Oct 2026 07:30:00 +0000\r\n\
Authentication-Results: mail.example.net; spf=fail\r\nFrom: Bank <bank@example.org>\r\nTo: me@example.net\r\n\
Subject: Verify now\r\nDate: Thu, 08 Oct 2026 07:30:00 +0000\r\nMessage-ID: <p1@example.org>\r\n\r\nClick.\r\n";
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path()).unwrap();
        let acct = s.mailbox.default_account().map(|a| a.id).unwrap();
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        let msg = mailcraft_mime::parse(RAW).unwrap();
        let mut item = MailItem::from_message(&msg, RAW, s.now());
        item.folder = inbox;
        item.raw = Some(RAW.to_vec());
        let id = s.mailbox.add_mail(item);
        s.store.as_mut().unwrap().checkpoint_mut(&mut s.mailbox, true).unwrap();
        assert!(s.mailbox.mail[&id].raw.is_none(), "the source lives in the store now");
        s.settings.set(REPORT_ADDRESS, &json!("abuse@example.net")).unwrap();
        s.selection.folder = inbox;
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        let r = s.run("mail.report", &json!({"ids": [id.0], "kind": "phishing", "confirm": true})).unwrap();
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.attachments[0].data, RAW, "the original source, every header included");
    }

    /// Too many messages for the abuse-desk copy: the messages are still reported, and the user is told.
    #[test]
    fn report_still_reports_when_the_copy_cannot_be_attached() {
        let mut f = fx();
        f.s.settings.set(REPORT_ADDRESS, &json!("abuse@example.net")).unwrap();
        let mut ids = vec![f.a.0];
        for i in 0..=crate::cmd::respond::MAX_FORWARD_ITEMS {
            ids.push(
                f.s.mailbox
                    .add_mail(msg(f.inbox, &format!("Spam {i}"), "spam@example.org", "x", at(20735, 9)))
                    .0,
            );
        }
        let r = report(&mut f.s, &json!({"ids": ids, "kind": "junk"})).unwrap();
        assert!(r["compose"].is_null());
        assert!(r["reportError"].as_str().unwrap().contains("at most"));
        assert_eq!(r["moved"], ids.len());
    }

    #[test]
    fn block_and_never_block_fill_the_lists() {
        let mut f = fx();
        let r = f.s.run("mail.block", &json!({"kind": "sender"})).unwrap();
        assert_eq!(r["blocked"], "carol@example.com");
        assert_eq!(f.s.mailbox.junk.blocked_senders, vec!["carol@example.com"]);
        // Never Block Sender takes it off the Blocked Senders List and moves it back.
        let r = f.s.run("mail.block", &json!({"ids": [f.a.0], "kind": "neverSender"})).unwrap();
        assert_eq!(r["list"], "safeSenders");
        assert!(f.s.mailbox.junk.blocked_senders.is_empty());
        assert_eq!(f.s.mailbox.junk.safe_senders, vec!["carol@example.com"]);
        assert_eq!(folder_of(&f.s, f.a), f.inbox);
        f.s.run("mail.block", &json!({"ids": [f.b.0], "kind": "neverDomain"})).unwrap();
        assert!(f.s.mailbox.junk.safe_senders.contains(&"@example.org".to_owned()));
        // The group: the first To address that is not mine, or the List-Post address.
        let r = f.s.run("mail.block", &json!({"ids": [f.c.0], "kind": "neverList"}));
        assert!(r.is_err(), "c is addressed only to me: {r:?}");
        if let Some(m) = f.s.mailbox.mail.get_mut(&f.c) {
            m.raw = Some(
                b"From: alice@example.com\r\nList-Post: <mailto:team@lists.example.org>\r\n\r\nx".to_vec(),
            );
        }
        let r = f.s.run("mail.block", &json!({"ids": [f.c.0], "kind": "neverList"})).unwrap();
        assert_eq!(r["safe"], "team@lists.example.org");
        assert_eq!(f.s.mailbox.junk.safe_recipients, vec!["team@lists.example.org"]);
        // Undo restores the previous lists.
        f.s.run("app.undo", &json!({})).unwrap();
        assert!(f.s.mailbox.junk.safe_recipients.is_empty());
    }

    #[test]
    fn not_junk_trusts_the_sender_unless_asked_not_to() {
        let mut f = fx();
        f.s.run("mail.junk", &json!({"ids": [f.a.0, f.b.0], "action": "moveToJunk"})).unwrap();
        let r = f.s.run("mail.junk", &json!({"ids": [f.a.0], "action": "notJunk"})).unwrap();
        assert_eq!(r["trusted"], json!(["carol@example.com"]));
        assert_eq!(folder_of(&f.s, f.a), f.inbox);
        let r =
            f.s.run("mail.junk", &json!({"ids": [f.b.0], "action": "notJunk", "addToSafe": false})).unwrap();
        assert_eq!(r["moved"], 1);
        assert_eq!(f.s.mailbox.junk.safe_senders, vec!["carol@example.com"]);
    }

    #[test]
    fn junk_settings_get_set_add_remove() {
        let mut f = fx();
        let g = f.s.run("mail.junk", &json!({"action": "get"})).unwrap();
        assert_eq!(g["level"], "low");
        assert_eq!(g["disableLinks"], true);
        let r =
            f.s.run(
                "mail.junk",
                &json!({"action": "set", "settings": {
                    "level": "safeListsOnly", "deleteSuspected": true, "warnSuspiciousDomains": false,
                    "blockedSenders": ["Spam@Example.net", "not valid", "@example.org"],
                    "blockedTlds": [".RU"], "blockedEncodings": ["KOI8-R"]
                }}),
            )
            .unwrap();
        assert_eq!(r["rejected"], json!(["not valid"]));
        let j = &f.s.mailbox.junk;
        assert_eq!(j.level, JunkLevel::SafeListsOnly);
        assert!(j.delete_suspected && !j.warn_suspicious_domains && j.disable_links);
        assert_eq!(j.blocked_senders, vec!["@example.org", "spam@example.net"]);
        assert_eq!(j.blocked_tlds, vec!["ru"]);
        assert_eq!(j.blocked_encodings, vec!["koi8-r"]);
        f.s.run("mail.junk", &json!({"action": "set", "level": 2})).unwrap();
        assert_eq!(f.s.mailbox.junk.level, JunkLevel::High);
        assert!(f.s.run("mail.junk", &json!({"action": "set", "level": "extreme"})).is_err());
        assert!(f.s.run("mail.junk", &json!({"action": "set", "disableLinks": "yes"})).is_err());
        assert!(f.s.run("mail.junk", &json!({"action": "set", "safeSenders": "x@example.com"})).is_err());
        assert_eq!(f.s.mailbox.junk.level, JunkLevel::High);
        let r = f
            .s
            .run(
                "mail.junk",
                &json!({"action": "add", "list": "safeRecipients", "entries": ["a@example.com", "@example.net"]}),
            )
            .unwrap();
        assert_eq!(r["changed"], 2);
        assert!(
            f.s.run("mail.junk", &json!({"action": "add", "list": "safeRecipients", "entry": "x y"}))
                .is_err()
        );
        assert!(f.s.run("mail.junk", &json!({"action": "add", "list": "nope", "entry": "a@b.c"})).is_err());
        assert!(f.s.run("mail.junk", &json!({"action": "add", "list": "safeRecipients"})).is_err());
        let r =
            f.s.run(
                "mail.junk",
                &json!({"action": "remove", "list": "safeRecipients", "entry": "A@example.com"}),
            )
            .unwrap();
        assert_eq!(r["entries"], json!(["@example.net"]));
        // Settings survive a store round trip (serde default on old data).
        let v = serde_json::to_value(&f.s.mailbox).unwrap();
        let back: Mailbox = serde_json::from_value(v).unwrap();
        assert_eq!(back.junk, f.s.mailbox.junk);
        let mut old = serde_json::to_value(&f.s.mailbox).unwrap();
        old.as_object_mut().unwrap().remove("junk");
        let back: Mailbox = serde_json::from_value(old).unwrap();
        assert_eq!(back.junk, mailcraft_model::JunkSettings::default());
    }

    #[test]
    fn junk_lists_import_and_export_txt() {
        let mut f = fx();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("blocked.txt");
        std::fs::write(&src, "# my list\r\nspam@example.net\r\n@example.org\r\n\r\nbad entry\r\n").unwrap();
        let r =
            f.s.run(
                "mail.junk",
                &json!({"action": "import", "list": "blockedSenders", "path": src.to_str().unwrap()}),
            )
            .unwrap();
        assert_eq!(r["added"], 2);
        let out = dir.path().join("out.txt");
        let r =
            f.s.run(
                "mail.junk",
                &json!({"action": "export", "list": "blockedSenders", "path": out.to_str().unwrap()}),
            )
            .unwrap();
        assert_eq!(r["exported"], 2);
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "@example.org\r\nspam@example.net\r\n");
        let missing = dir.path().join("missing.txt");
        assert!(
            f.s.run(
                "mail.junk",
                &json!({"action": "import", "list": "safeSenders", "path": missing.to_str().unwrap()})
            )
            .is_err()
        );
        assert!(
            f.s.run("mail.junk", &json!({"action": "import", "list": "safeSenders", "path": ""})).is_err()
        );
        let bad_dir = dir.path().join("no/such/dir/x.txt");
        assert!(
            f.s.run(
                "mail.junk",
                &json!({"action": "export", "list": "safeSenders", "path": bad_dir.to_str().unwrap()})
            )
            .is_err()
        );
    }

    #[test]
    fn run_junk_filter_and_classify() {
        let mut f = fx();
        let junkf = role(&f.s, FolderRole::JunkEmail);
        f.s.run("mail.junk", &json!({"action": "add", "list": "blockedSenders", "entry": "@example.org"}))
            .unwrap();
        let r = f.s.run("mail.junk", &json!({"action": "classify", "ids": [f.a.0, f.b.0]})).unwrap();
        assert_eq!(r["verdicts"][0]["junk"], false);
        assert_eq!(r["verdicts"][1]["reason"], "blockedSender");
        assert_eq!(r["verdicts"][1]["entry"], "@example.org");
        let r = f.s.run("mail.junk", &json!({"action": "run", "folder": f.inbox.0})).unwrap();
        assert_eq!(r["checked"], 3);
        assert_eq!(r["moved"], 1);
        assert_eq!(folder_of(&f.s, f.b), junkf);
        assert_eq!(folder_of(&f.s, f.a), f.inbox);
        // Running it on Junk Email itself does nothing.
        let r = f.s.run("mail.junk", &json!({"action": "run", "folder": junkf.0})).unwrap();
        assert_eq!(r["checked"], 0);
        assert!(f.s.run("mail.junk", &json!({"action": "nonsense"})).is_err());
        f.s.run("app.undo", &json!({})).unwrap();
        f.s.run("app.undo", &json!({})).unwrap();
        assert_eq!(folder_of(&f.s, f.b), f.inbox);
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
        assert_eq!(edit_message(&mut f.s, &Value::Null).unwrap()["dialog"], "editMessage");
        let p = last_dialog(&f.s, "editMessage");
        assert_eq!(p["id"], f.a.0);
        assert_eq!(p["subject"], "Invoice");
        assert_eq!(p["text"], "Invoice attached.");
        assert_eq!(p["html"], false);
        assert!(edit_message(&mut f.s, &json!({"subject": "x".repeat(MAX_EDIT_SUBJECT_CHARS + 1)})).is_err());
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
    fn resend_needs_sent_items_and_recall_is_gone() {
        let mut f = fx();
        assert!(need_sent_item(&f.s).is_some());
        let sent = role(&f.s, FolderRole::SentItems);
        f.s.mailbox.move_items(&[f.a], sent).unwrap();
        f.s.selection.folder = sent;
        assert!(need_sent_item(&f.s).is_none());
        let r = resend(&mut f.s, &Value::Null).unwrap();
        let c = &f.s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.subject, "Invoice");
        assert_eq!(c.to, "Test User <test.user@example.net>");
        // Message recall needs an Exchange server (its catalog row is not applicable): there is no command.
        f.s.mailbox.accounts[0].kind = mailcraft_model::AccountKind::Exchange;
        assert!(matches!(f.s.run("mail.recall", &Value::Null), Err(CmdError::Unknown(_))));
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
