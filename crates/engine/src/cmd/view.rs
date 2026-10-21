//! View commands (`view.*`): per-folder view settings (`Mailbox::views`: layout, arrangement, sort,
//! groups, conversations, preview lines, filter, Focused Inbox, density, reading pane) and window-level
//! [`crate::ViewState`] (panes, Favorites, reading view, zoom, group collapse state).
//!
//! Filter Email kinds that the per-folder [`ListFilter`] cannot express (Has Attachments, Important, Sent
//! To: Me or CC: Me, Expiring Soon, categories) are applied as search tokens in `ViewState::search`, the
//! same syntax `search.refine` builds.
//!
//! In the People, Tasks and Notes modules (a contacts, tasks or notes folder selected) the current view,
//! arrangement, message preview, reading pane, saved views, View Settings and Apply View act on the
//! folder's module view ([`modules`]). In the Calendar the calendar's own view handling
//! (`calendar::module_view`) runs first.
//!
//! Mail and search folders add the per-folder columns, Format Columns, Conditional Formatting, View Settings
//! filter and saved views of [`mail`], which also keeps the window-wide Quick Actions and status bar items.

pub mod mail;
pub mod modules;
pub mod table;

use serde_json::{Value, json};

use super::folder::{current_folder, dialog, resolve_folder};
use super::search::toggle_token;
use super::util::{opt_bool, opt_str, req_str};
use crate::{CmdError, CmdResult, CommandSpec, Module, Session, always};
use mailcraft_model::dates::DateGroup;
use mailcraft_model::thread::ConversationSettings;
use mailcraft_model::{
    ArrangeBy, FlagState, FolderId, FolderKind, FolderPaneState, FolderRole, Importance, ItemId, ListFilter,
    MailItem, ReadingPanePos, SortOrder, ViewLayout, ViewSettings,
};
use modules::ModuleKind;
use std::collections::BTreeSet;

/// Zoom limits, percent.
pub const ZOOM_MIN: u16 = 10;
pub const ZOOM_MAX: u16 = 500;

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
        spec("view.addColumns", "Add Columns", "View › Arrangement", "", "", false, list_view, add_columns),
        spec(
            "view.applyViewTo",
            "Apply View to Folders",
            "View › Current View › Change View",
            "",
            r#"{"folders"?:[string],"subfolders"?:bool}"#,
            true,
            has_folder,
            apply_view_to,
        ),
        spec(
            "view.arrangeBy",
            "Arrange By",
            "View › Arrangement",
            "",
            r#"{"key":"date"|"from"|"to"|"categories"|"flagStatus"|"flagStart"|"flagDue"|"size"|"subject"|"type"|"attachments"|"account"|"importance"}"#,
            true,
            arrange_view,
            arrange_by,
        ),
        spec(
            "view.changeView",
            "Change View",
            "View › Current View",
            "",
            r#"{"name":string}"#,
            true,
            has_folder,
            change_view,
        ),
        spec(
            "view.collapseAll",
            "Collapse All Groups",
            "View › Arrangement › Expand/Collapse",
            "",
            "",
            false,
            groups_shown,
            collapse_all,
        ),
        spec(
            "view.collapseGroup",
            "Collapse This Group",
            "View › Arrangement › Expand/Collapse",
            "",
            r#"{"conversation"?:bool,"id"?:number,"label"?:string}"#,
            false,
            group_or_conversation_selected,
            collapse_group,
        ),
        spec(
            "view.conversationSettings",
            "Conversation Settings",
            "View › Messages",
            "",
            r#"{"setting":"fromOtherFolders|sendersAboveSubject|alwaysExpand|classicIndent","on"?:bool}"#,
            true,
            conversations_on,
            conversation_settings,
        ),
        spec(
            "view.conversations",
            "Show as Conversations",
            "View › Messages",
            "",
            r#"{"on"?:bool,"scope"?:"folder"|"all"}"#,
            true,
            has_folder,
            conversations,
        ),
        spec(
            "view.expandAll",
            "Expand All Groups",
            "View › Arrangement › Expand/Collapse",
            "",
            "",
            false,
            groups_shown,
            expand_all,
        ),
        spec(
            "view.expandGroup",
            "Expand This Group",
            "View › Arrangement › Expand/Collapse",
            "",
            r#"{"conversation"?:bool,"id"?:number,"label"?:string}"#,
            false,
            group_or_conversation_selected,
            expand_group,
        ),
        spec("view.favorites", "Favorites", "View › Layout › Folder Pane", "", "", false, always, favorites),
        spec(
            "view.filter",
            "Filter Email",
            "Home › Find › Filter Email",
            "",
            r#"{"kind":"all"|"unread"|"hasAttachments"|"flagged"|"mentioned"|"important"|"toOrCcMe"|"expiringSoon"|"category"|"anyCategory"|"noCategory","name"?:string}"#,
            true,
            has_folder,
            filter,
        ),
        spec(
            "view.focusedInbox",
            "Show Focused Inbox",
            "View › Focused Inbox",
            "",
            "",
            true,
            has_inbox,
            focused_inbox,
        ),
        spec(
            "view.focusedTab",
            "Focused / Other",
            "Message list header tabs",
            "",
            r#"{"tab":"focused|other"}"#,
            false,
            focused_tabs_shown,
            focused_tab,
        ),
        spec(
            "view.folderPane",
            "Folder Pane",
            "View › Layout",
            "",
            r#"{"mode":"normal"|"minimized"|"off"}"#,
            false,
            always,
            folder_pane,
        ),
        spec(
            "view.layout",
            "Change View",
            "View › Current View › Change View",
            "",
            r#"{"layout":"compact"|"single"|"preview"}"#,
            true,
            has_folder,
            layout,
        ),
        spec(
            "view.manageViews",
            "Manage Views",
            "View › Current View › Change View",
            "",
            r#"{"op"?:"new"|"copy"|"rename"|"delete"|"reset"|"apply"|"update","name"?:string,"newName"?:string,"base"?:string (a built-in view),"scope"?:"all"|"folder"}"#,
            true,
            always,
            manage_views,
        ),
        spec(
            "view.messagePreview",
            "Message Preview",
            "View › Arrangement",
            "",
            r#"{"lines":0|1|2|3}"#,
            true,
            preview_view,
            message_preview,
        ),
        spec("view.normalView", "Normal View", "Status bar", "", "", false, always, normal_view),
        spec(
            "view.quickActions",
            "Set Quick Actions",
            "Home › Tags › Follow Up",
            "",
            r#"{"actions"?:[string]}"#,
            false,
            always,
            mail::quick_actions_cmd,
        ),
        spec(
            "view.readingPane",
            "Reading Pane",
            "View › Layout",
            "",
            r#"{"position":"right"|"bottom"|"off"}"#,
            true,
            always,
            reading_pane,
        ),
        spec(
            "view.readingPaneOptions",
            "Reading Pane Options",
            "View › Layout › Reading Pane",
            "",
            "",
            false,
            always,
            reading_pane_options,
        ),
        spec("view.readingView", "Reading View", "Status bar", "", "", false, always, reading_view),
        spec(
            "view.reminderWindow",
            "Reminders Window",
            "View › Window",
            "",
            "",
            false,
            always,
            reminder_window,
        ),
        spec(
            "view.resetView",
            "Reset View",
            "View › Current View",
            "",
            "",
            true,
            view_customized,
            reset_view,
        ),
        spec(
            "view.reverseSort",
            "Reverse Sort",
            "View › Arrangement",
            "",
            "",
            true,
            has_folder,
            reverse_sort,
        ),
        spec(
            "view.saveViewAs",
            "Save View As",
            "View › Current View › Change View",
            "",
            r#"{"name"?:string,"scope"?:"all"|"folder","confirm"?:bool}"#,
            true,
            save_view_enabled,
            save_view_as,
        ),
        spec(
            "view.showInGroups",
            "Show in Groups",
            "View › Arrangement › Arrange By",
            "",
            "",
            true,
            has_folder,
            show_in_groups,
        ),
        spec(
            "view.sortOrder",
            "Newest/Oldest on Top",
            "Message list sort menu",
            "",
            r#"{"dir":"desc|asc"}"#,
            true,
            has_folder,
            sort_order,
        ),
        spec(
            "view.sortBy",
            "Sort",
            "View › Current View › View Settings › Sort; a click on a column header",
            "",
            r#"{"field":string|null,"dir"?:"asc"|"desc"}"#,
            true,
            has_folder,
            sort_by,
        ),
        spec(
            "view.statusBar",
            "Customize Status Bar",
            "Status bar context menu",
            "",
            r#"{"item":"quota|filter|itemsInView|headerItems|unreadItems|unseenItems|reminders|groupNotifications|viewShortcuts|zoomSlider|zoom","show":bool}"#,
            false,
            always,
            mail::status_bar,
        ),
        spec(
            "view.tighterSpacing",
            "Use Tighter Spacing",
            "View › Layout",
            "",
            "",
            true,
            always,
            tighter_spacing,
        ),
        spec(
            "view.todoBar",
            "To-Do Bar",
            "View › Layout",
            "",
            r#"{"part"?:"calendar"|"people"|"tasks"|"off"} (no part = report the parts shown)"#,
            false,
            always,
            todo_bar,
        ),
        spec(
            "view.viewSettings",
            "View Settings",
            "View › Current View",
            "",
            r#"{} (opens the dialog) | module folders: {"sort"?:string,"columns"?:[string],"filter"?:{"words"?:string,"inBody"?:bool,"category"?:string,"days"?:number}|null,"rules"?:[{"name":string,"on":bool,"filter":object,"bold"?:bool,"italic"?:bool,"strike"?:bool,"color"?:string}],"autoPreview"?:bool,"gridLines"?:bool} | calendar: {"view"?:object,"sort"?,"ascending"?,"timeScale"?,"readingPane"?,"tighterSpacing"?,"dailyTaskList"?,"columns"?,"groupBy"?,"filter"?,"rules"?,"autoPreview"?,"gridLines"?}"#,
            false,
            has_folder,
            view_settings,
        ),
        spec("view.zoom", "Zoom", "Message › Zoom", "", r#"{"pct"?:number}"#, false, always, zoom),
    ]
    .into_iter()
    .chain(mail::specs())
    .collect()
}

// ---------------------------------------------------------------------------------------------------------
// Helpers

/// View settings of `folder` (defaults when none are stored).
pub fn settings(s: &Session, folder: FolderId) -> ViewSettings {
    s.mailbox.views.get(&folder).cloned().unwrap_or_default()
}

/// Runs `f` on the selected folder's stored view settings (created from the defaults on first change).
fn update(s: &mut Session, f: impl FnOnce(&mut ViewSettings) -> Value) -> CmdResult {
    let folder = current_folder(s, &Value::Null)?;
    let out = f(s.mailbox.views.entry(folder).or_default());
    // Keep only customised views stored, so "default" stays observable (Reset View's enabled state).
    if s.mailbox.views.get(&folder).is_some_and(|v| *v == ViewSettings::default()) {
        s.mailbox.views.remove(&folder);
    }
    // In a module folder showing a saved view, the change is kept in that view too.
    modules::sync_saved(s, folder)?;
    Ok(out)
}

/// `view.arrangeBy` key of an arrangement.
pub fn arrange_name(a: ArrangeBy) -> &'static str {
    match a {
        ArrangeBy::Date => "date",
        ArrangeBy::From => "from",
        ArrangeBy::To => "to",
        ArrangeBy::Categories => "categories",
        ArrangeBy::FlagStatus => "flagStatus",
        ArrangeBy::FlagStartDate => "flagStart",
        ArrangeBy::FlagDueDate => "flagDue",
        ArrangeBy::Size => "size",
        ArrangeBy::Subject => "subject",
        ArrangeBy::Type => "type",
        ArrangeBy::Attachments => "attachments",
        ArrangeBy::Account => "account",
        ArrangeBy::Importance => "importance",
    }
}

fn order_name(o: SortOrder) -> &'static str {
    match o {
        SortOrder::Descending => "desc",
        SortOrder::Ascending => "asc",
    }
}

fn layout_name(l: ViewLayout) -> &'static str {
    match l {
        ViewLayout::Compact => "Compact",
        ViewLayout::Single => "Single",
        ViewLayout::Preview => "Preview",
    }
}

fn parse_layout(name: &str) -> Option<ViewLayout> {
    match name.trim().to_ascii_lowercase().as_str() {
        "compact" => Some(ViewLayout::Compact),
        "single" => Some(ViewLayout::Single),
        "preview" => Some(ViewLayout::Preview),
        _ => None,
    }
}

/// Date-group label of an optional date ("(none)" when absent).
fn date_label(s: &Session, t: Option<mailcraft_model::DateTime>) -> String {
    t.map_or_else(|| "(none)".to_owned(), |t| DateGroup::of(t, s.now(), s.tz_offset_min).label())
}

fn person(m: Option<&mailcraft_model::Address>) -> String {
    match m {
        Some(a) => a.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&a.addr).to_owned(),
        None => "(none)".to_owned(),
    }
}

/// Group header label of `m` when the list is arranged by `by` (the key collapse state is stored under).
pub fn group_label(s: &Session, m: &MailItem, by: ArrangeBy) -> String {
    match by {
        ArrangeBy::Date => date_label(s, Some(m.received)),
        ArrangeBy::From => person(m.from.as_ref()),
        ArrangeBy::To => person(m.to.first()),
        ArrangeBy::Categories => m.categories.first().cloned().unwrap_or_else(|| "(none)".to_owned()),
        ArrangeBy::FlagStatus => match m.flag {
            FlagState::None => "Unflagged",
            FlagState::Flagged { .. } => "Flagged",
            FlagState::Complete { .. } => "Completed",
        }
        .to_owned(),
        ArrangeBy::FlagStartDate => match &m.flag {
            FlagState::Flagged { start, .. } => date_label(s, *start),
            _ => "(none)".to_owned(),
        },
        ArrangeBy::FlagDueDate => match &m.flag {
            FlagState::Flagged { due, .. } => date_label(s, *due),
            _ => "(none)".to_owned(),
        },
        ArrangeBy::Size => {
            let kb = m.size_kb();
            match kb {
                0..=9 => "Under 10 KB",
                10..=24 => "10 to 25 KB",
                25..=99 => "25 to 100 KB",
                100..=499 => "100 to 500 KB",
                500..=1023 => "500 KB to 1 MB",
                1024..=5119 => "1 to 5 MB",
                _ => "Over 5 MB",
            }
            .to_owned()
        }
        ArrangeBy::Subject => {
            let t = mailcraft_model::subject::strip_prefixes(&m.subject).trim();
            if t.is_empty() { "(no subject)".to_owned() } else { t.to_owned() }
        }
        ArrangeBy::Type => if m.is_draft { "Draft" } else { "Message" }.to_owned(),
        ArrangeBy::Attachments => {
            if m.has_attachments() { "With attachments" } else { "No attachments" }.to_owned()
        }
        ArrangeBy::Account => s
            .mailbox
            .folder_account(m.folder)
            .and_then(|a| s.mailbox.account(a))
            .map(|a| if a.display_name.is_empty() { a.address.clone() } else { a.display_name.clone() })
            .unwrap_or_else(|| "(none)".to_owned()),
        ArrangeBy::Importance => match m.importance {
            Importance::High => "High",
            Importance::Normal => "Normal",
            Importance::Low => "Low",
        }
        .to_owned(),
    }
}

/// Group label of the focused (else first selected) item in the selected folder. In conversation view the
/// item's conversation decides (it sits in the group of its sort item).
fn selected_group(s: &Session) -> Option<String> {
    let id = s.selection.focus.or_else(|| s.selection.items.first().copied())?;
    if modules::module_kind(s, s.selection.folder).is_some() {
        return modules::group_of(s, s.selection.folder, id);
    }
    let m = s.mailbox.mail.get(&id)?;
    // The list's own header label (pinned messages, conversations grouped by their sort item).
    Some(crate::queries::group_label(s, m))
}

fn inbox_of_current_account(s: &Session) -> Option<FolderId> {
    s.current_account().and_then(|a| s.mailbox.role_folder(a, FolderRole::Inbox))
}

/// The selected folder and its module when it is a contacts, tasks or notes folder.
fn module_here(s: &Session) -> Option<(FolderId, ModuleKind)> {
    let folder = s.selection.folder;
    modules::module_kind(s, folder).map(|k| (folder, k))
}

/// In the Calendar, the calendar's own handling of a view command (`None` = not a calendar view command).
fn calendar_first(s: &mut Session, id: &str, p: &Value) -> Option<CmdResult> {
    if s.view.module == Module::Calendar { crate::cmd::calendar::module_view(s, id, p) } else { None }
}

// ---------------------------------------------------------------------------------------------------------
// Enabled predicates

fn has_folder(s: &Session) -> Option<&'static str> {
    if s.mailbox.folders.contains_key(&s.selection.folder) { None } else { Some("no folder is selected") }
}

/// Save Current View As a New View: in a tasks folder only once the view differs from the folder's defaults
/// (observed greyed out on an unchanged Tasks view); elsewhere whenever a folder is selected.
fn save_view_enabled(s: &Session) -> Option<&'static str> {
    if let Some(why) = has_folder(s) {
        return Some(why);
    }
    let f = s.selection.folder;
    match modules::module_kind(s, f) {
        Some(ModuleKind::Tasks)
            if !modules::customized(s, f)
                && s.mailbox.views.get(&f).is_none_or(|v| *v == ViewSettings::default()) =>
        {
            Some("the view has no changes to save yet")
        }
        _ => None,
    }
}

/// Arrange By: any folder, except the module views without arrangements (People, Business Card and Card;
/// Notes), which grey it out as observed.
fn arrange_view(s: &Session) -> Option<&'static str> {
    if let Some(why) = has_folder(s) {
        return Some(why);
    }
    if s.view.module == Module::Calendar {
        return None;
    }
    match modules::current(s, s.selection.folder) {
        Some((kind, st)) if !modules::arrange_allowed(kind, &st.view) => Some("not available in this view"),
        _ => None,
    }
}

/// Message Preview, Add Columns and Expand/Collapse belong to list views (mail, tasks); the People
/// (business card) view of a contacts folder and the calendar / notes views do not offer them.
fn list_view(s: &Session) -> Option<&'static str> {
    match s.mailbox.folder(s.selection.folder).map(|f| f.kind) {
        None => Some("no folder is selected"),
        Some(FolderKind::Contacts) => {
            // The Phone and List table views of a contacts folder have groups and columns.
            match modules::current(s, s.selection.folder) {
                Some((kind, st)) if modules::grouped_view(kind, &st.view) => None,
                _ => Some("not available in this view"),
            }
        }
        Some(FolderKind::Calendar | FolderKind::Notes) => Some("not available in this view"),
        Some(_) => None,
    }
}

/// Message Preview: mail and tasks folders (not contacts, calendar or notes).
fn preview_view(s: &Session) -> Option<&'static str> {
    match s.mailbox.folder(s.selection.folder).map(|f| f.kind) {
        None => Some("no folder is selected"),
        Some(FolderKind::Contacts | FolderKind::Calendar | FolderKind::Notes) => {
            Some("not available in this view")
        }
        Some(_) => None,
    }
}

fn groups_shown(s: &Session) -> Option<&'static str> {
    if let Some(why) = list_view(s) {
        return Some(why);
    }
    if settings(s, s.selection.folder).show_in_groups {
        None
    } else {
        Some("the list is not shown in groups")
    }
}

fn group_selected(s: &Session) -> Option<&'static str> {
    if let Some(why) = groups_shown(s) {
        return Some(why);
    }
    if selected_group(s).is_some() { None } else { Some("no item is selected") }
}

/// Expand/Collapse This Group, or (in conversation view, `{"conversation": true}`) the selected
/// conversation.
fn group_or_conversation_selected(s: &Session) -> Option<&'static str> {
    let why = group_selected(s)?;
    // The conversation form names its message (`id`) or falls back to the selection when run.
    if conversations_on(s).is_none() { None } else { Some(why) }
}

fn conversations_on(s: &Session) -> Option<&'static str> {
    if let Some(why) = has_folder(s) {
        return Some(why);
    }
    if settings(s, s.selection.folder).conversations { None } else { Some("Show as Conversations is off") }
}

fn has_inbox(s: &Session) -> Option<&'static str> {
    if inbox_of_current_account(s).is_some() { None } else { Some("this account has no Inbox") }
}

fn focused_tabs_shown(s: &Session) -> Option<&'static str> {
    let is_inbox = s.mailbox.folder(s.selection.folder).is_some_and(|f| f.role == Some(FolderRole::Inbox));
    if !is_inbox {
        return Some("Focused Inbox applies to the Inbox");
    }
    if settings(s, s.selection.folder).focused_inbox { None } else { Some("Focused Inbox is off") }
}

fn view_customized(s: &Session) -> Option<&'static str> {
    if let Some(why) = has_folder(s) {
        return Some(why);
    }
    let f = s.selection.folder;
    // Tasks folders offer Reset View on their views as they come (as observed); it then just puts the
    // view's own settings back.
    if modules::module_kind(s, f) == Some(modules::ModuleKind::Tasks) {
        return None;
    }
    // Conversation expansion is not a view customisation; collapsed groups and Conversation Settings are.
    let custom = s.mailbox.views.get(&f).is_some_and(|v| *v != ViewSettings::default())
        || modules::customized(s, f)
        || mail::customized(s, f)
        || !crate::cmd::notes::positions(s, f).is_empty()
        || (s.view.module == Module::Calendar && crate::cmd::calendar::views::view_customized(s))
        || s.view
            .collapsed_groups
            .get(&f)
            .is_some_and(|g| g.iter().any(|e| is_group_label(e) || is_setting(e)));
    if custom { None } else { Some("the view already has its default settings") }
}

// ---------------------------------------------------------------------------------------------------------
// Current view

fn change_view(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.changeView", p) {
        return r;
    }
    let name = req_str(p, "name")?;
    if let Some((folder, kind)) = module_here(s) {
        return modules::change_view(s, folder, kind, name);
    }
    change_view_by_name(s, name)
}

/// Change View in a mail folder: a built-in view (the layout) or a saved view usable here.
fn change_view_by_name(s: &mut Session, name: &str) -> CmdResult {
    let Some(l) = parse_layout(name) else {
        let clipped: String = name.trim().chars().take(mail::MAX_NAME).collect();
        return mail::change_to_saved(s, name)
            .unwrap_or_else(|| Err(CmdError::Params(format!("no view named \"{clipped}\""))));
    };
    let r = update(s, |v| {
        v.layout = l;
        json!({ "view": layout_name(l) })
    })?;
    let folder = s.selection.folder;
    mail::clear_view_name(s, folder);
    Ok(r)
}

fn layout(s: &mut Session, p: &Value) -> CmdResult {
    let name = req_str(p, "layout")?;
    let l = parse_layout(name)
        .ok_or_else(|| CmdError::Params("`layout` must be compact, single or preview".into()))?;
    let r = update(s, |v| {
        v.layout = l;
        json!({ "view": layout_name(l) })
    })?;
    let folder = s.selection.folder;
    mail::clear_view_name(s, folder);
    Ok(r)
}

fn reset_view(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.resetView", p) {
        return r;
    }
    let folder = current_folder(s, &Value::Null)?;
    s.mailbox.views.remove(&folder);
    // Columns, rules, filter, collapsed groups and dragged note icons go; pinned messages (marked on the
    // messages) stay pinned.
    crate::cmd::notes::clear_positions(s, folder);
    modules::reset_keeping_saved(s, folder);
    Ok(json!({ "folder": folder.0, "reset": true }))
}

fn save_view_as(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.saveViewAs", p) {
        return r;
    }
    if let Some((folder, kind)) = module_here(s) {
        return modules::save_view_as(s, folder, kind, p);
    }
    mail::save_view_as(s, p)
}

fn manage_views(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.manageViews", p) {
        return r;
    }
    if let Some((folder, kind)) = module_here(s) {
        return modules::manage_views(s, folder, kind, p);
    }
    mail::manage_views(s, p)
}

fn view_settings(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.viewSettings", p) {
        return r;
    }
    if let Some((folder, kind)) = module_here(s) {
        return modules::view_settings(s, folder, kind, p);
    }
    let folder = current_folder(s, &Value::Null)?;
    let current = serde_json::to_value(settings(s, folder)).unwrap_or(Value::Null);
    let mut params = json!({ "folder": folder.0, "settings": current });
    if mail::mail_list(s).is_none()
        && let Some(o) = params.as_object_mut()
    {
        o.insert("summary".into(), mail::summary(s, folder));
        if let Some(name) = opt_str(p, "savedView") {
            o.insert("savedView".into(), json!(name));
        }
    }
    Ok(dialog(s, "advancedViewSettings", params))
}

fn add_columns(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.addColumns", p) {
        return r;
    }
    let folder = current_folder(s, &Value::Null)?;
    if mail::mail_list(s).is_some() {
        // Module tables (Tasks, People › Phone and List) have their own columns.
        let Some((folder, kind, _)) = table::table_here(s) else {
            return Err(CmdError::Disabled("not available in this view".into()));
        };
        let mut params = table::columns_json(s, folder, kind);
        if let Some(o) = params.as_object_mut() {
            o.insert("mail".into(), json!(false));
        }
        return Ok(dialog(s, "showColumns", params));
    }
    let shown: Vec<&str> = mail::columns(s, folder).iter().map(|c| c.id).collect();
    let available: Vec<Value> = mail::COLUMNS.iter().map(|c| json!({"id": c.id, "label": c.label})).collect();
    Ok(dialog(
        s,
        "showColumns",
        json!({ "folder": folder.0, "mail": true, "columns": shown, "available": available }),
    ))
}

fn apply_view_to(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.applyViewTo", p) {
        return r;
    }
    let src = current_folder(s, &Value::Null)?;
    let module = modules::module_kind(s, src);
    let Some(list) = p.get("folders").and_then(Value::as_array) else {
        if let Some(kind) = module {
            return modules::apply_view_to(s, src, kind, None);
        }
        return Ok(dialog(s, "applyView", json!({ "folder": src.0 })));
    };
    let subfolders = opt_bool(p, "subfolders").unwrap_or(false);
    let mut targets = std::collections::BTreeSet::new();
    for v in list {
        let id = resolve_folder(s, v).ok_or_else(|| CmdError::Params(format!("no such folder: {v}")))?;
        if subfolders {
            targets.extend(s.mailbox.subtree(id));
        } else {
            targets.insert(id);
        }
    }
    if let Some(kind) = module {
        return modules::apply_view_to(s, src, kind, Some(targets));
    }
    let current = settings(s, src);
    let extra = mail::mail_view(s, src);
    let mut applied = 0usize;
    for id in targets {
        if id != src
            && s.mailbox.folder(id).is_some_and(|f| matches!(f.kind, FolderKind::Mail | FolderKind::Search))
        {
            if current == ViewSettings::default() {
                s.mailbox.views.remove(&id);
            } else {
                s.mailbox.views.insert(id, current.clone());
            }
            mail::store_view(s, id, &extra);
            applied = applied.saturating_add(1);
        }
    }
    Ok(json!({ "applied": applied }))
}

// ---------------------------------------------------------------------------------------------------------
// Arrangement

fn arrange_by(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.arrangeBy", p) {
        return r;
    }
    let key = req_str(p, "key")?;
    if let Some((folder, kind)) = module_here(s) {
        return modules::arrange_by(s, folder, kind, key);
    }
    let a = ArrangeBy::ALL
        .into_iter()
        .find(|a| {
            arrange_name(*a).eq_ignore_ascii_case(key.trim()) || a.label().eq_ignore_ascii_case(key.trim())
        })
        .ok_or_else(|| CmdError::Params(format!("unknown arrangement `{key}`")))?;
    let folder = s.selection.folder;
    let r = update(s, |v| {
        v.arrange_by = a;
        json!({ "arrangeBy": arrange_name(a), "order": order_name(v.order) })
    })?;
    // Group labels change meaning with the arrangement (reserved state entries stay).
    modules::clear_groups(s, folder);
    Ok(r)
}

fn reverse_sort(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.reverseSort", p) {
        return r;
    }
    update(s, |v| {
        v.order = match v.order {
            SortOrder::Descending => SortOrder::Ascending,
            SortOrder::Ascending => SortOrder::Descending,
        };
        json!({ "order": order_name(v.order) })
    })
}

fn sort_order(s: &mut Session, p: &Value) -> CmdResult {
    let order = match req_str(p, "dir")? {
        "desc" => SortOrder::Descending,
        "asc" => SortOrder::Ascending,
        other => return Err(CmdError::Params(format!("`dir` must be desc or asc, not {other}"))),
    };
    update(s, |v| {
        v.order = order;
        json!({ "order": order_name(order) })
    })
}

/// `view.sortBy`: sorts the list by a column. In a module table the column (`null`: the view's own order)
/// orders the rows inside their groups; in a mail folder the column's arrangement is used.
fn sort_by(s: &mut Session, p: &Value) -> CmdResult {
    if let Some((folder, kind, _)) = table::table_here(s) {
        return table::sort_by(s, folder, kind, p);
    }
    if module_here(s).is_some() || s.view.module == Module::Calendar {
        return Err(CmdError::Disabled("not available in this view".into()));
    }
    let field = req_str(p, "field")?;
    let clipped: String = field.chars().take(40).collect();
    let arrange = mail::column(field)
        .and_then(|c| c.arrange)
        .ok_or_else(|| CmdError::Params(format!("the list cannot be sorted by `{clipped}`")))?;
    let order = match p.get("dir").and_then(Value::as_str) {
        None => None,
        Some("desc") => Some(SortOrder::Descending),
        Some("asc") => Some(SortOrder::Ascending),
        Some(_) => return Err(CmdError::Params("`dir` must be asc or desc".into())),
    };
    let folder = s.selection.folder;
    let r = update(s, |v| {
        v.arrange_by = arrange;
        if let Some(o) = order {
            v.order = o;
        }
        json!({ "arrangeBy": arrange_name(arrange), "order": order_name(v.order) })
    })?;
    modules::clear_groups(s, folder);
    Ok(r)
}

fn show_in_groups(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.showInGroups", p) {
        return r;
    }
    update(s, |v| {
        v.show_in_groups = !v.show_in_groups;
        json!({ "on": v.show_in_groups })
    })
}

fn set_group(s: &mut Session, p: &Value, collapsed: bool) -> CmdResult {
    let id = if collapsed { "view.collapseGroup" } else { "view.expandGroup" };
    if let Some(r) = calendar_first(s, id, p) {
        return r;
    }
    if opt_bool(p, "conversation").unwrap_or(false) {
        return set_conversation(s, p, collapsed);
    }
    let label = match opt_str(p, "label") {
        // A header names its own group: an item listed under several groups (Arrange by Categories) would
        // otherwise resolve to its first one.
        Some(name) => {
            if let Some(why) = groups_shown(s) {
                return Err(CmdError::Disabled(why.into()));
            }
            let folder = s.selection.folder;
            let labels = if modules::module_kind(s, folder).is_some() {
                modules::groups(s, folder)
            } else {
                crate::queries::group_labels(s)
            };
            labels
                .into_iter()
                .find(|l| l == name)
                .ok_or_else(|| CmdError::Params(format!("no group \"{name}\" in this view")))?
        }
        None => {
            if let Some(why) = group_selected(s) {
                return Err(CmdError::Disabled(why.into()));
            }
            selected_group(s).ok_or_else(|| CmdError::Disabled("no item is selected".into()))?
        }
    };
    let list = s.view.collapsed_groups.entry(s.selection.folder).or_default();
    list.retain(|g| *g != label);
    if collapsed {
        list.push(label.clone());
    }
    Ok(json!({ "group": label, "collapsed": collapsed }))
}

fn collapse_group(s: &mut Session, p: &Value) -> CmdResult {
    set_group(s, p, true)
}

fn expand_group(s: &mut Session, p: &Value) -> CmdResult {
    set_group(s, p, false)
}

fn collapse_all(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.collapseAll", p) {
        return r;
    }
    let folder = current_folder(s, &Value::Null)?;
    let mut labels: Vec<String> = if modules::module_kind(s, folder).is_some() {
        modules::groups(s, folder)
    } else {
        // The list's own headers (conversations group by their sort item, pinned messages on top).
        crate::queries::group_labels(s)
    };
    labels.sort();
    labels.dedup();
    let n = labels.len();
    let kept: Vec<String> = s
        .view
        .collapsed_groups
        .get(&folder)
        .map(|v| v.iter().filter(|e| !is_group_label(e)).cloned().collect())
        .unwrap_or_default();
    labels.extend(kept);
    s.view.collapsed_groups.insert(folder, labels);
    Ok(json!({ "collapsed": n }))
}

fn expand_all(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.expandAll", p) {
        return r;
    }
    let folder = current_folder(s, &Value::Null)?;
    if let Some(list) = s.view.collapsed_groups.get_mut(&folder) {
        list.retain(|e| !is_group_label(e));
        if list.is_empty() {
            s.view.collapsed_groups.remove(&folder);
        }
    }
    Ok(json!({ "collapsed": 0 }))
}

/// Show as Conversations. `on` sets the state (default: toggles the selected folder's); `scope` is the
/// answer to the "This folder / All mailboxes" question: `folder` (default) or `all` (every mail folder of
/// every account, plus the selected folder).
fn conversations(s: &mut Session, p: &Value) -> CmdResult {
    let folder = current_folder(s, &Value::Null)?;
    let on = match p.get("on") {
        None | Some(Value::Null) => !settings(s, folder).conversations,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(CmdError::Params("`on` must be true or false".into())),
    };
    let scope = opt_str(p, "scope").map(|v| v.trim().to_ascii_lowercase()).unwrap_or_default();
    match scope.as_str() {
        "" | "folder" | "thisfolder" | "this folder" => update(s, |v| {
            v.conversations = on;
            json!({ "on": on, "scope": "folder", "folders": 1 })
        }),
        "all" | "allmailboxes" | "all mailboxes" => {
            let mut ids: Vec<FolderId> =
                s.mailbox.folders.values().filter(|f| f.kind == FolderKind::Mail).map(|f| f.id).collect();
            if !ids.contains(&folder) {
                ids.push(folder);
            }
            for id in &ids {
                s.mailbox.views.entry(*id).or_default().conversations = on;
            }
            s.mailbox.views.retain(|_, v| *v != ViewSettings::default());
            Ok(json!({ "on": on, "scope": "all", "folders": ids.len() }))
        }
        other => Err(CmdError::Params(format!("`scope` must be folder or all, not `{other}`"))),
    }
}

/// Conversation Settings of the selected folder: sets `setting` to `on` (default: toggles it).
fn conversation_settings(s: &mut Session, p: &Value) -> CmdResult {
    let setting = req_str(p, "setting")?;
    let Some(name) = ConversationSettings::canonical(setting) else {
        return Err(CmdError::Params(format!("unknown conversation setting `{setting}`")));
    };
    let folder = current_folder(s, &Value::Null)?;
    let mut st = conversation_state(s, folder);
    let on = match p.get("on") {
        None | Some(Value::Null) => !st.settings.get(name).unwrap_or(false),
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(CmdError::Params("`on` must be true or false".into())),
    };
    st.settings.set(name, on);
    store_conversation_state(s, folder, &st);
    Ok(json!({ "setting": name, "on": on, "settings": st.settings }))
}

/// Expands or collapses the conversation of message `id` (default: the focused, else first selected
/// message). Collapsing the selected conversation moves the focus to its header message and keeps it
/// collapsed while that message keeps the focus; expanding a conversation that is not selected keeps it
/// open after the selection leaves.
fn set_conversation(s: &mut Session, p: &Value, collapse: bool) -> CmdResult {
    let folder = current_folder(s, &Value::Null)?;
    if !settings(s, folder).conversations {
        return Err(CmdError::Disabled("Show as Conversations is off".into()));
    }
    let target = match p.get("id").filter(|v| !v.is_null()) {
        Some(v) => ItemId(
            v.as_u64()
                .ok_or_else(|| CmdError::Params("`id` must be a message id (non-negative integer)".into()))?,
        ),
        None => s
            .selection
            .focus
            .or_else(|| s.selection.items.first().copied())
            .ok_or_else(|| CmdError::Disabled("no conversation is selected".into()))?,
    };
    let conv = crate::queries::conversation_of(s, target)
        .ok_or_else(|| CmdError::Params(format!("message {} is not in this view", target.0)))?;
    let key = format!("{:016x}", conv.key);
    if !conv.is_thread() {
        return Ok(json!({ "conversation": key, "expanded": false, "messages": 1 }));
    }
    let mut st = conversation_state(s, folder);
    let focus_in = s.selection.focus.is_some_and(|f| conv.contains(f));
    if collapse {
        st.expanded.remove(&conv.key);
        if focus_in {
            if s.selection.focus != Some(conv.header) {
                s.selection.items = vec![conv.header];
                s.selection.focus = Some(conv.header);
            }
            st.collapsed = Some((conv.key, conv.header));
        }
    } else {
        if st.collapsed.is_some_and(|(k, _)| k == conv.key) {
            st.collapsed = None;
        }
        if !focus_in {
            st.expanded.insert(conv.key);
        }
    }
    store_conversation_state(s, folder, &st);
    let expanded = crate::queries::conversation_of(s, conv.header).is_some_and(|c| c.expanded);
    Ok(json!({
        "conversation": key,
        "expanded": expanded,
        "messages": conv.items.len(),
        "header": conv.header.0,
    }))
}

// ---------------------------------------------------------------------------------------------------------
// Conversation view state
//
// Interim storage until `ViewSettings` carries the Conversation Settings and `ViewState` the per-folder
// expansion state: both live in `ViewState::collapsed_groups` as reserved entries starting with
// `CONV_MARK` (a control character no group label produced by the list starts with). Group code skips
// them through [`is_group_label`].

/// First character of every reserved (non group label) entry.
const RESERVED: char = '\u{1}';
const CONV_MARK: &str = "\u{1}conv.";
/// Most conversations remembered as expanded by hand per folder.
const MAX_EXPANDED: usize = 1_000;

/// Conversation view state of one folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationState {
    pub settings: ConversationSettings,
    /// Conversations expanded by hand (they stay expanded when the selection leaves them).
    pub expanded: BTreeSet<u64>,
    /// The selected conversation collapsed by hand: (conversation key, message focused then). It stays
    /// collapsed while that message keeps the focus.
    pub collapsed: Option<(u64, ItemId)>,
}

/// True for real group labels in `ViewState::collapsed_groups` (false for the reserved conversation and
/// module view state entries, which start with a control character).
pub fn is_group_label(entry: &str) -> bool {
    !entry.starts_with(RESERVED)
}

fn is_setting(entry: &str) -> bool {
    entry.strip_prefix(CONV_MARK).is_some_and(|r| r.starts_with("set:"))
}

/// Conversation Settings and expansion state of `folder`.
pub fn conversation_state(s: &Session, folder: FolderId) -> ConversationState {
    let mut st = ConversationState::default();
    for e in s.view.collapsed_groups.get(&folder).into_iter().flatten() {
        let Some(rest) = e.strip_prefix(CONV_MARK) else { continue };
        if let Some((name, value)) = rest.strip_prefix("set:").and_then(|kv| kv.split_once('=')) {
            st.settings.set(name, value == "1");
        } else if let Some(k) = rest.strip_prefix("open:").and_then(|k| u64::from_str_radix(k, 16).ok()) {
            st.expanded.insert(k);
        } else if let Some((k, i)) = rest.strip_prefix("shut:").and_then(|kv| kv.split_once(':'))
            && let (Ok(k), Ok(i)) = (u64::from_str_radix(k, 16), i.parse::<u64>())
        {
            st.collapsed = Some((k, ItemId(i)));
        }
    }
    st
}

/// Mail › "Don't automatically expand conversations when using the keyboard to change messages": the
/// keyboard moved the focus onto the header message `item` of the collapsed conversation `key`, which stays
/// collapsed while that message keeps the focus (as a collapse by hand would).
pub(crate) fn keep_collapsed(s: &mut Session, folder: FolderId, key: u64, item: ItemId) {
    let mut st = conversation_state(s, folder);
    if st.expanded.contains(&key) {
        return;
    }
    st.collapsed = Some((key, item));
    store_conversation_state(s, folder, &st);
}

fn store_conversation_state(s: &mut Session, folder: FolderId, st: &ConversationState) {
    let defaults = ConversationSettings::default();
    let list = s.view.collapsed_groups.entry(folder).or_default();
    list.retain(|e| !e.starts_with(CONV_MARK));
    for name in ConversationSettings::NAMES {
        if let Some(on) = st.settings.get(name).filter(|on| Some(*on) != defaults.get(name)) {
            list.push(format!("{CONV_MARK}set:{name}={}", u8::from(on)));
        }
    }
    // Newest keys are not known; keep the largest set allowed (BTreeSet order) when over the cap.
    for k in st.expanded.iter().take(MAX_EXPANDED) {
        list.push(format!("{CONV_MARK}open:{k:016x}"));
    }
    if let Some((k, i)) = st.collapsed {
        list.push(format!("{CONV_MARK}shut:{k:016x}:{}", i.0));
    }
    if list.is_empty() {
        s.view.collapsed_groups.remove(&folder);
    }
}

fn message_preview(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.messagePreview", p) {
        return r;
    }
    let lines = p
        .get("lines")
        .and_then(Value::as_u64)
        .and_then(|n| u8::try_from(n).ok())
        .filter(|n| *n <= 3)
        .ok_or_else(|| CmdError::Params("`lines` must be 0, 1, 2 or 3".into()))?;
    if let Some((folder, kind)) = module_here(s) {
        return modules::message_preview(s, folder, kind, lines);
    }
    update(s, |v| {
        v.preview_lines = lines;
        json!({ "lines": lines })
    })
}

fn tighter_spacing(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.tighterSpacing", p) {
        return r;
    }
    let on = !settings(s, s.selection.folder).tighter_spacing;
    // Density applies across the window: every folder's view gets the same value.
    let ids: Vec<FolderId> = s.mailbox.folders.keys().copied().collect();
    for id in ids {
        s.mailbox.views.entry(id).or_default().tighter_spacing = on;
    }
    s.mailbox.views.retain(|_, v| *v != ViewSettings::default());
    Ok(json!({ "on": on }))
}

// ---------------------------------------------------------------------------------------------------------
// Filter / Focused Inbox

fn filter(s: &mut Session, p: &Value) -> CmdResult {
    let kind = req_str(p, "kind")?;
    let list_filter = match kind {
        "all" => Some(ListFilter::All),
        "unread" => Some(ListFilter::Unread),
        "flagged" => Some(ListFilter::Flagged),
        "mentioned" => Some(ListFilter::Mentioned),
        _ => None,
    };
    if list_filter == Some(ListFilter::All) {
        // "All" also drops the filter terms the other kinds put in the query.
        let kept: Vec<String> = super::search::terms(&s.view.search)
            .into_iter()
            .filter(|t| {
                let t = t.to_ascii_lowercase();
                !(t.starts_with("hasattachments:")
                    || t.starts_with("importance:")
                    || t.starts_with("tocc:")
                    || t.starts_with("expires:")
                    || t.starts_with("category:"))
            })
            .collect();
        s.view.search = kept.join(" ");
    }
    if let Some(f) = list_filter {
        return update(s, |v| {
            v.filter = if v.filter == f { ListFilter::All } else { f };
            json!({ "filter": kind, "on": v.filter != ListFilter::All })
        });
    }
    current_folder(s, &Value::Null)?;
    let token = match kind {
        "hasAttachments" => "hasattachments:yes".to_owned(),
        "important" => "importance:high".to_owned(),
        "toOrCcMe" => "tocc:me".to_owned(),
        "expiringSoon" => "expires:soon".to_owned(),
        "anyCategory" => "category:any".to_owned(),
        "noCategory" => "category:none".to_owned(),
        "category" => {
            let name = req_str(p, "name")?.trim();
            if name.is_empty() {
                return Err(CmdError::Params("`name` (category) required".into()));
            }
            super::search::token("category", name)
        }
        other => return Err(CmdError::Params(format!("unknown filter `{other}`"))),
    };
    let (query, on) = toggle_token(&s.view.search, &token);
    s.view.search = query;
    Ok(json!({ "filter": kind, "on": on, "query": s.view.search }))
}

fn focused_inbox(s: &mut Session, _p: &Value) -> CmdResult {
    let inbox =
        inbox_of_current_account(s).ok_or_else(|| CmdError::Disabled("this account has no Inbox".into()))?;
    let v = s.mailbox.views.entry(inbox).or_default();
    v.focused_inbox = !v.focused_inbox;
    let on = v.focused_inbox;
    if on {
        s.view.focused_tab = true;
    }
    Ok(json!({ "on": on }))
}

fn focused_tab(s: &mut Session, p: &Value) -> CmdResult {
    s.view.focused_tab = match req_str(p, "tab")? {
        "focused" => true,
        "other" => false,
        other => return Err(CmdError::Params(format!("`tab` must be focused or other, not {other}"))),
    };
    Ok(json!({ "tab": if s.view.focused_tab { "focused" } else { "other" } }))
}

// ---------------------------------------------------------------------------------------------------------
// Layout

fn reading_pane(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(r) = calendar_first(s, "view.readingPane", p) {
        return r;
    }
    let pos = match req_str(p, "position")? {
        "right" => ReadingPanePos::Right,
        "bottom" => ReadingPanePos::Bottom,
        "off" => ReadingPanePos::Off,
        other => {
            return Err(CmdError::Params(format!("`position` must be right, bottom or off, not {other}")));
        }
    };
    if let Some((folder, kind)) = module_here(s) {
        // A module folder keeps its own position; the window default (Mail) does not change.
        return modules::reading_pane(s, folder, kind, pos);
    }
    s.view.reading_pane = pos;
    let folder = s.selection.folder;
    if s.mailbox.folders.contains_key(&folder) {
        s.mailbox.views.entry(folder).or_default().reading_pane = pos;
    }
    Ok(json!({ "position": req_str(p, "position")? }))
}

fn folder_pane(s: &mut Session, p: &Value) -> CmdResult {
    s.view.folder_pane = match req_str(p, "mode")? {
        "normal" => FolderPaneState::Normal,
        "minimized" => FolderPaneState::Minimized,
        "off" => FolderPaneState::Off,
        other => {
            return Err(CmdError::Params(format!("`mode` must be normal, minimized or off, not {other}")));
        }
    };
    Ok(json!({ "mode": req_str(p, "mode")? }))
}

fn favorites(s: &mut Session, _p: &Value) -> CmdResult {
    s.view.favorites_visible = !s.view.favorites_visible;
    Ok(json!({ "on": s.view.favorites_visible }))
}

/// The To-Do Bar's parts as JSON.
pub fn todo_bar_json(t: crate::TodoBar) -> Value {
    json!({ "calendar": t.calendar, "people": t.people, "tasks": t.tasks })
}

/// View › To-Do Bar: `part` (calendar, people, tasks) shows or hides that part, `off` hides the bar; the
/// session keeps the parts ([`crate::ViewState::todo_bar`], saved with the profile), so every front end and
/// script sees the same bar. Without `part` it reports the parts.
fn todo_bar(s: &mut Session, p: &Value) -> CmdResult {
    let Some(part) = opt_str(p, "part") else {
        return Ok(json!({ "parts": todo_bar_json(s.view.todo_bar) }));
    };
    if !["calendar", "people", "tasks", "off"].contains(&part) {
        let shown: String = part.chars().take(40).collect();
        return Err(CmdError::Params(format!("`part` must be calendar, people, tasks or off, not {shown}")));
    }
    // In the Calendar the calendar's view command applies it (with the calendar's view state).
    if let Some(r) = calendar_first(s, "view.todoBar", p) {
        return r;
    }
    toggle_todo_bar(s, part);
    Ok(json!({ "todoBar": part, "parts": todo_bar_json(s.view.todo_bar) }))
}

/// Applies To-Do Bar `part` (a validated part name) to the session's view state.
pub(crate) fn toggle_todo_bar(s: &mut Session, part: &str) {
    let t = &mut s.view.todo_bar;
    match part {
        "calendar" => t.calendar = !t.calendar,
        "people" => t.people = !t.people,
        "tasks" => t.tasks = !t.tasks,
        _ => *t = crate::TodoBar::default(),
    }
}

fn reading_view(s: &mut Session, _p: &Value) -> CmdResult {
    s.view.reading_view = true;
    Ok(json!({ "readingView": true }))
}

fn normal_view(s: &mut Session, _p: &Value) -> CmdResult {
    s.view.reading_view = false;
    Ok(json!({ "readingView": false }))
}

fn zoom(s: &mut Session, p: &Value) -> CmdResult {
    let Some(pct) = p.get("pct").filter(|v| !v.is_null()) else {
        let current = s.view.zoom;
        return Ok(dialog(s, "zoom", json!({ "pct": current })));
    };
    let pct = pct
        .as_f64()
        .filter(|f| f.is_finite())
        .map(f64::round)
        .filter(|f| (f64::from(ZOOM_MIN)..=f64::from(ZOOM_MAX)).contains(f))
        .ok_or_else(|| CmdError::Params(format!("`pct` must be between {ZOOM_MIN} and {ZOOM_MAX}")))?;
    // In range 10..=500 after the check above, so the conversion cannot truncate.
    let pct = pct as u16;
    s.view.zoom = pct.clamp(ZOOM_MIN, ZOOM_MAX);
    Ok(json!({ "pct": s.view.zoom }))
}

// ---------------------------------------------------------------------------------------------------------
// Dialog-only

fn reading_pane_options(s: &mut Session, _p: &Value) -> CmdResult {
    Ok(dialog(s, "readingPaneOptions", json!({})))
}

fn reminder_window(s: &mut Session, _p: &Value) -> CmdResult {
    Ok(dialog(s, "reminders", json!({})))
}

// ---------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Notice;
    use mailcraft_model::{DateTime, ItemId, Mailbox};

    fn session() -> Session {
        let mut s = Session::new(Mailbox::new_local("Test"));
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        s.selection.folder = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s.fixed_now = Some(DateTime { unix: 1_791_000_000, offset_min: 0 });
        s
    }

    fn run(s: &mut Session, id: &str, p: Value) -> Value {
        s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    fn cur(s: &Session) -> ViewSettings {
        settings(s, s.selection.folder)
    }

    fn add(s: &mut Session, subject: &str, days_ago: i64) -> ItemId {
        let now = s.now();
        let folder = s.selection.folder;
        s.mailbox.add_mail(MailItem {
            folder,
            subject: subject.into(),
            received: DateTime { unix: now.unix - days_ago * 86_400, offset_min: 0 },
            ..Default::default()
        })
    }

    #[test]
    fn all_ids_registered() {
        let ids: Vec<&str> = specs().iter().map(|s| s.id).collect();
        for id in [
            "view.addColumns",
            "view.applyViewTo",
            "view.arrangeBy",
            "view.changeView",
            "view.collapseAll",
            "view.collapseGroup",
            "view.conversationSettings",
            "view.conversations",
            "view.expandAll",
            "view.expandGroup",
            "view.favorites",
            "view.filter",
            "view.focusedInbox",
            "view.focusedTab",
            "view.folderPane",
            "view.manageViews",
            "view.messagePreview",
            "view.normalView",
            "view.quickActions",
            "view.readingPane",
            "view.readingPaneOptions",
            "view.readingView",
            "view.reminderWindow",
            "view.resetView",
            "view.reverseSort",
            "view.saveViewAs",
            "view.showInGroups",
            "view.sortOrder",
            "view.statusBar",
            "view.tighterSpacing",
            "view.todoBar",
            "view.viewSettings",
            "view.zoom",
            "view.layout",
        ] {
            assert!(ids.contains(&id), "{id} missing");
        }
    }

    #[test]
    fn change_view_and_layout() {
        let mut s = session();
        run(&mut s, "view.changeView", json!({"name": "Single"}));
        assert_eq!(cur(&s).layout, ViewLayout::Single);
        run(&mut s, "view.layout", json!({"layout": "preview"}));
        assert_eq!(cur(&s).layout, ViewLayout::Preview);
        assert!(s.run("view.changeView", &json!({"name": "Nope"})).is_err());
        assert!(s.run("view.changeView", &json!({})).is_err());
    }

    #[test]
    fn arrange_sort_groups() {
        let mut s = session();
        assert_eq!(run(&mut s, "view.arrangeBy", json!({"key": "from"}))["arrangeBy"], "from");
        assert_eq!(cur(&s).arrange_by, ArrangeBy::From);
        run(&mut s, "view.arrangeBy", json!({"key": "Flag: Due Date"}));
        assert_eq!(cur(&s).arrange_by, ArrangeBy::FlagDueDate);
        assert!(s.run("view.arrangeBy", &json!({"key": "colour"})).is_err());
        assert_eq!(run(&mut s, "view.reverseSort", json!({}))["order"], "asc");
        assert_eq!(run(&mut s, "view.reverseSort", json!({}))["order"], "desc");
        run(&mut s, "view.sortOrder", json!({"dir": "asc"}));
        assert_eq!(cur(&s).order, SortOrder::Ascending);
        assert!(s.run("view.sortOrder", &json!({"dir": "up"})).is_err());
        assert_eq!(run(&mut s, "view.showInGroups", json!({}))["on"], false);
        assert!(matches!(s.run("view.collapseAll", &json!({})), Err(CmdError::Disabled(_))));
        assert_eq!(run(&mut s, "view.showInGroups", json!({}))["on"], true);
    }

    #[test]
    fn expand_collapse_groups() {
        let mut s = session();
        let today = add(&mut s, "a", 0);
        add(&mut s, "b", 1);
        assert!(matches!(s.run("view.collapseGroup", &json!({})), Err(CmdError::Disabled(_))));
        s.selection.items = vec![today];
        s.selection.focus = Some(today);
        let r = run(&mut s, "view.collapseGroup", json!({}));
        assert_eq!(r["group"], "Today");
        let folder = s.selection.folder;
        assert_eq!(s.view.collapsed_groups.get(&folder).unwrap(), &vec!["Today".to_string()]);
        run(&mut s, "view.expandGroup", json!({}));
        assert!(s.view.collapsed_groups.get(&folder).unwrap().is_empty());
        assert_eq!(run(&mut s, "view.collapseAll", json!({}))["collapsed"], 2);
        run(&mut s, "view.expandAll", json!({}));
        assert!(!s.view.collapsed_groups.contains_key(&folder));
    }

    #[test]
    fn conversations_and_settings() {
        let mut s = session();
        assert!(matches!(
            s.run("view.conversationSettings", &json!({"setting": "alwaysExpand", "on": true})),
            Err(CmdError::Disabled(_))
        ));
        assert_eq!(run(&mut s, "view.conversations", json!({}))["on"], true);
        let r = run(&mut s, "view.conversationSettings", json!({"setting": "alwaysExpand", "on": true}));
        assert_eq!(r["on"], true);
        assert!(s.run("view.conversationSettings", &json!({"setting": "x", "on": true})).is_err());
    }

    #[test]
    fn preview_reading_pane_folder_pane() {
        let mut s = session();
        run(&mut s, "view.messagePreview", json!({"lines": 3}));
        assert_eq!(cur(&s).preview_lines, 3);
        assert!(s.run("view.messagePreview", &json!({"lines": 4})).is_err());
        run(&mut s, "view.readingPane", json!({"position": "bottom"}));
        assert_eq!(s.view.reading_pane, ReadingPanePos::Bottom);
        assert_eq!(cur(&s).reading_pane, ReadingPanePos::Bottom);
        assert!(s.run("view.readingPane", &json!({"position": "left"})).is_err());
        run(&mut s, "view.folderPane", json!({"mode": "minimized"}));
        assert_eq!(s.view.folder_pane, FolderPaneState::Minimized);
        assert_eq!(run(&mut s, "view.favorites", json!({}))["on"], false);
        assert!(!s.view.favorites_visible);
    }

    #[test]
    fn reading_view_zoom_spacing() {
        let mut s = session();
        run(&mut s, "view.readingView", json!({}));
        assert!(s.view.reading_view);
        run(&mut s, "view.normalView", json!({}));
        assert!(!s.view.reading_view);
        run(&mut s, "view.zoom", json!({"pct": 150}));
        assert_eq!(s.view.zoom, 150);
        assert!(s.run("view.zoom", &json!({"pct": 5})).is_err());
        assert!(s.run("view.zoom", &json!({"pct": 501})).is_err());
        assert_eq!(run(&mut s, "view.zoom", json!({}))["dialog"], "zoom");
        assert_eq!(run(&mut s, "view.tighterSpacing", json!({}))["on"], true);
        let other = s.mailbox.folders.keys().copied().find(|f| *f != s.selection.folder).unwrap();
        assert!(settings(&s, other).tighter_spacing);
        assert_eq!(run(&mut s, "view.tighterSpacing", json!({}))["on"], false);
    }

    #[test]
    fn filters() {
        let mut s = session();
        assert_eq!(run(&mut s, "view.filter", json!({"kind": "unread"}))["on"], true);
        assert_eq!(cur(&s).filter, ListFilter::Unread);
        assert_eq!(run(&mut s, "view.filter", json!({"kind": "unread"}))["on"], false);
        assert_eq!(cur(&s).filter, ListFilter::All);
        run(&mut s, "view.filter", json!({"kind": "hasAttachments"}));
        assert_eq!(s.view.search, "hasattachments:yes");
        run(&mut s, "view.filter", json!({"kind": "category", "name": "Red category"}));
        assert_eq!(s.view.search, "hasattachments:yes category:\"Red category\"");
        run(&mut s, "view.filter", json!({"kind": "hasAttachments"}));
        assert_eq!(s.view.search, "category:\"Red category\"");
        s.view.search.push_str(" budget");
        run(&mut s, "view.filter", json!({"kind": "all"}));
        assert_eq!(s.view.search, "budget");
        assert!(s.run("view.filter", &json!({"kind": "category"})).is_err());
        assert!(s.run("view.filter", &json!({"kind": "bogus"})).is_err());
    }

    #[test]
    fn focused_inbox_and_tabs() {
        let mut s = session();
        run(&mut s, "view.focusedTab", json!({"tab": "other"}));
        assert!(!s.view.focused_tab);
        assert_eq!(run(&mut s, "view.focusedInbox", json!({}))["on"], false);
        assert!(matches!(s.run("view.focusedTab", &json!({"tab": "focused"})), Err(CmdError::Disabled(_))));
        assert_eq!(run(&mut s, "view.focusedInbox", json!({}))["on"], true);
        assert!(s.view.focused_tab);
    }

    #[test]
    fn reset_and_apply_view() {
        let mut s = session();
        let dbg = s.run("view.resetView", &json!({}));
        assert!(matches!(dbg, Err(CmdError::Disabled(_))));
        run(&mut s, "view.changeView", json!({"name": "Single"}));
        let r = run(&mut s, "view.applyViewTo", json!({"folders": ["Archive", "Sent Items"]}));
        assert_eq!(r["applied"], 2);
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        let archive = s.mailbox.role_folder(acct, FolderRole::Archive).unwrap();
        assert_eq!(settings(&s, archive).layout, ViewLayout::Single);
        assert_eq!(run(&mut s, "view.applyViewTo", json!({}))["dialog"], "applyView");
        run(&mut s, "view.resetView", json!({}));
        assert_eq!(cur(&s), ViewSettings::default());
        assert!(s.run("view.applyViewTo", &json!({"folders": ["Nope"]})).is_err());
    }

    #[test]
    fn todo_and_status_bar() {
        let mut s = session();
        assert_eq!(run(&mut s, "view.todoBar", json!({"part": "tasks"}))["todoBar"], "tasks");
        assert!(s.run("view.todoBar", &json!({"part": "mail"})).is_err());
        // The session keeps the parts: scripts can read and change the bar.
        run(&mut s, "view.todoBar", json!({"part": "calendar"}));
        assert_eq!(
            run(&mut s, "view.todoBar", json!({}))["parts"],
            json!({"calendar": true, "people": false, "tasks": true})
        );
        run(&mut s, "view.todoBar", json!({"part": "tasks"}));
        assert_eq!(s.view.todo_bar, crate::TodoBar { calendar: true, people: false, tasks: false });
        run(&mut s, "view.todoBar", json!({"part": "off"}));
        assert!(!s.view.todo_bar.shown());
        // It is part of the saved view state.
        s.view.todo_bar.people = true;
        let v: crate::ViewState = serde_json::from_value(serde_json::to_value(&s.view).unwrap()).unwrap();
        assert!(v.todo_bar.people);
        assert_eq!(
            run(&mut s, "view.statusBar", json!({"item": "zoomSlider", "show": false}))["show"],
            false
        );
        assert!(s.run("view.statusBar", &json!({"item": "zoomSlider"})).is_err());
        assert!(s.run("view.statusBar", &json!({"item": "clock", "show": true})).is_err());
    }

    #[test]
    fn dialogs() {
        let mut s = session();
        for (id, name) in [
            ("view.addColumns", "showColumns"),
            ("view.manageViews", "manageViews"),
            ("view.quickActions", "quickActions"),
            ("view.readingPaneOptions", "readingPaneOptions"),
            ("view.reminderWindow", "reminders"),
            ("view.viewSettings", "advancedViewSettings"),
            ("view.saveViewAs", "saveViewAs"),
        ] {
            assert_eq!(run(&mut s, id, json!({"name": "Mine"}))["dialog"], name);
            assert!(matches!(s.notices.last(), Some(Notice::OpenDialog(n, _)) if n == name));
        }
        assert!(s.run("view.saveViewAs", &json!({"name": "Compact"})).is_err());
    }

    #[test]
    fn list_arrangement_commands_disabled_in_contacts_folders() {
        // modules.md, People › View: Message Preview, Add Columns and Expand/Collapse are disabled; in
        // Tasks › View they are available.
        let mut s = session();
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        let contacts = s.mailbox.role_folder(acct, FolderRole::Contacts).unwrap();
        let tasks = s.mailbox.role_folder(acct, FolderRole::Tasks).unwrap();
        let ids = ["view.messagePreview", "view.addColumns", "view.expandAll", "view.collapseAll"];
        s.selection.folder = contacts;
        for id in ids {
            assert!(s.enabled(id).is_some(), "{id} should be disabled in Contacts");
        }
        assert!(matches!(s.run("view.messagePreview", &json!({"lines": 2})), Err(CmdError::Disabled(_))));
        assert!(s.enabled("view.reverseSort").is_none());
        s.selection.folder = tasks;
        for id in ids {
            assert!(s.enabled(id).is_none(), "{id} should be enabled in Tasks");
        }
    }

    #[test]
    fn group_labels_by_key() {
        let s = session();
        let m = MailItem {
            subject: "RE: Plans".into(),
            size: 30 * 1024,
            importance: Importance::High,
            ..Default::default()
        };
        assert_eq!(group_label(&s, &m, ArrangeBy::Subject), "Plans");
        assert_eq!(group_label(&s, &m, ArrangeBy::Size), "25 to 100 KB");
        assert_eq!(group_label(&s, &m, ArrangeBy::Importance), "High");
        assert_eq!(group_label(&s, &m, ArrangeBy::FlagStatus), "Unflagged");
        assert_eq!(group_label(&s, &m, ArrangeBy::From), "(none)");
    }
}
