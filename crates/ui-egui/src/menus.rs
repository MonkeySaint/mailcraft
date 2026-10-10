//! Context menus (keys/menus area): message row, group header, folder, account root, status bar, sort menu.
//!
//! Contents follow `plan/outlook/context-menus.md` and `message-list.md`; every entry runs a command through
//! [`App::run`]. Entries for services MailCraft does not have (retention policies, OneNote) are left out.
//! Callers open these from `Response::context_menu(|ui| menus::…(ui, app, …))` (or a `menu_button` for the
//! arrange menu); the functions draw the menu body.

use std::collections::BTreeSet;

use egui::{Atom, Button, Color32, Id, Rect, Ui, vec2};
use serde_json::{Value, json};

use mailcraft_engine::queries;
use mailcraft_model::{AccountId, ArrangeBy, FlagState, FolderId, FolderRole, ItemId, ListFilter, SortOrder};

use crate::App;

const ICON: f32 = 16.0;
/// Most folders kept in the Move ▸ recent list.
const MAX_RECENT: usize = 10;

/// What the icon column of a row shows.
enum Glyph<'a> {
    None,
    Icon(&'a str),
    Check(bool),
    Swatch(Color32),
}

/// One menu row with an icon column; returns true when clicked (the menu then closes).
fn row(ui: &mut Ui, app: &mut App, glyph: Glyph<'_>, label: &str, enabled: bool) -> bool {
    let id = ui.id().with(("menu-row", label));
    let btn = Button::new((Atom::custom(id, vec2(ICON, ICON)), label));
    let resp = ui.add_enabled_ui(enabled, |ui| btn.atom_ui(ui)).inner;
    if let Some(rect) = resp.rect(id) {
        paint_glyph(ui, app, rect, &glyph, enabled);
    }
    let clicked = resp.response.clicked();
    if clicked {
        ui.close();
    }
    clicked
}

fn paint_glyph(ui: &Ui, app: &mut App, rect: Rect, glyph: &Glyph<'_>, enabled: bool) {
    let tint = if enabled { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
    match glyph {
        Glyph::None | Glyph::Check(false) => {}
        Glyph::Icon(name) => app.icons.paint(ui, rect, name, tint),
        Glyph::Check(true) => app.icons.paint(ui, rect, "check", tint),
        Glyph::Swatch(c) => {
            let r = Rect::from_center_size(rect.center(), vec2(12.0, 12.0));
            ui.painter().rect_filled(r, 2.0, *c);
        }
    }
}

/// A row that runs `id` with `params` when clicked; enabled from the command's own check.
fn cmd(ui: &mut Ui, app: &mut App, icon: &str, label: &str, id: &str, params: Value) -> Option<Value> {
    let enabled = app.s.enabled(id).is_none();
    let glyph = if icon.is_empty() { Glyph::None } else { Glyph::Icon(icon) };
    if row(ui, app, glyph, label, enabled) { app.run(id, params) } else { None }
}

/// Like [`cmd`] but with an explicit enabled state.
fn cmd_if(
    ui: &mut Ui,
    app: &mut App,
    icon: &str,
    label: &str,
    enabled: bool,
    id: &str,
    params: Value,
) -> Option<Value> {
    let glyph = if icon.is_empty() { Glyph::None } else { Glyph::Icon(icon) };
    if row(ui, app, glyph, label, enabled) { app.run(id, params) } else { None }
}

/// A checkable row running `id`.
fn check_cmd(ui: &mut Ui, app: &mut App, on: bool, label: &str, id: &str, params: Value) -> Option<Value> {
    let enabled = app.s.enabled(id).is_none();
    if row(ui, app, Glyph::Check(on), label, enabled) { app.run(id, params) } else { None }
}

/// A submenu row with an icon.
fn sub(
    ui: &mut Ui,
    app: &mut App,
    icon: &str,
    label: &str,
    enabled: bool,
    add: impl FnOnce(&mut Ui, &mut App),
) {
    let id = ui.id().with(("menu-sub", label));
    let resp = ui
        .add_enabled_ui(enabled, |ui| {
            ui.menu_button((Atom::custom(id, vec2(ICON, ICON)), label), |ui| add(ui, app)).response
        })
        .inner;
    if !icon.is_empty() {
        let pad = ui.spacing().button_padding.x;
        let rect = Rect::from_min_size(
            egui::pos2(resp.rect.left() + pad, resp.rect.center().y - ICON / 2.0),
            vec2(ICON, ICON),
        );
        paint_glyph(ui, app, rect, &Glyph::Icon(icon), enabled);
    }
}

/// Small grey section heading inside a menu ("Filter", "Arrange by", "Sort").
fn heading(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).small().weak()).selectable(false));
}

fn ids_json(ids: &[ItemId]) -> Value {
    Value::Array(ids.iter().map(|i| json!(i.0)).collect())
}

// ---------------------------------------------------------------------------------------------------------
// Recent folders (Move ▸)

#[derive(Clone, Debug, Default)]
struct RecentFolders(Vec<FolderId>);

fn recent_id() -> Id {
    Id::new("mailcraft.menus.recentFolders")
}

/// Records `folder` as the most recent Move target (the Move dialog should call this too).
pub fn note_recent_folder(ctx: &egui::Context, folder: FolderId) {
    ctx.data_mut(|d| {
        let r = d.get_temp_mut_or_default::<RecentFolders>(recent_id());
        r.0.retain(|f| *f != folder);
        r.0.insert(0, folder);
        r.0.truncate(MAX_RECENT);
    });
}

/// Most recent Move targets, newest first.
pub fn recent_folders(ctx: &egui::Context) -> Vec<FolderId> {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<RecentFolders>(recent_id()).0.clone())
}

// ---------------------------------------------------------------------------------------------------------
// Shared submenus

/// Facts about the targeted messages used for check marks and enabled states.
struct Targets {
    ids: Vec<ItemId>,
    any_category: bool,
    /// Categories every target carries (lower-case).
    common: BTreeSet<String>,
    any_flag: bool,
    all_complete: bool,
}

impl Targets {
    fn of(app: &App, ids: Vec<ItemId>) -> Targets {
        let items: Vec<_> = ids.iter().filter_map(|i| app.s.mailbox.mail.get(i)).collect();
        let mut common: Option<BTreeSet<String>> = None;
        for m in &items {
            let set: BTreeSet<String> = m.categories.iter().map(|c| c.to_lowercase()).collect();
            common = Some(match common {
                None => set,
                Some(c) => c.intersection(&set).cloned().collect(),
            });
        }
        Targets {
            any_category: items.iter().any(|m| !m.categories.is_empty()),
            common: common.unwrap_or_default(),
            any_flag: items.iter().any(|m| !matches!(m.flag, FlagState::None)),
            all_complete: !items.is_empty()
                && items.iter().all(|m| matches!(m.flag, FlagState::Complete { .. })),
            ids,
        }
    }
    fn params(&self) -> Value {
        json!({"ids": ids_json(&self.ids)})
    }
    fn with(&self, extra: Value) -> Value {
        let mut v = self.params();
        if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
            o.extend(e);
        }
        v
    }
}

fn categorize_menu(ui: &mut Ui, app: &mut App, t: &Targets) {
    cmd_if(ui, app, "", "Clear All Categories", t.any_category, "mail.clearCategories", t.params());
    let cats: Vec<(String, Color32)> = app
        .s
        .mailbox
        .categories
        .iter()
        .map(|c| {
            let rgb = c.color.rgb();
            (c.name.clone(), Color32::from_rgba_unmultiplied(rgb.r, rgb.g, rgb.b, rgb.a))
        })
        .collect();
    for (name, color) in cats {
        let on = t.common.contains(&name.to_lowercase());
        let glyph = if on { Glyph::Check(true) } else { Glyph::Swatch(color) };
        if row(ui, app, glyph, &name, true) {
            app.run("mail.categorize", t.with(json!({"name": name})));
        }
    }
    ui.separator();
    cmd(ui, app, "categorize", "All Categories...", "mail.allCategories", t.params());
    cmd(ui, app, "", "Set Quick Click...", "categories.setQuickClick", json!({}));
}

fn follow_up_menu(ui: &mut Ui, app: &mut App, t: &Targets) {
    for (label, kind) in [
        ("Today", "today"),
        ("Tomorrow", "tomorrow"),
        ("This Week", "thisWeek"),
        ("Next Week", "nextWeek"),
        ("No Date", "noDate"),
    ] {
        cmd(ui, app, "flag", label, "mail.flag", t.with(json!({"kind": kind})));
    }
    cmd(ui, app, "flag", "Custom...", "mail.flagCustom", t.params());
    ui.separator();
    cmd(ui, app, "bell", "Add Reminder...", "mail.addReminder", t.params());
    let complete = t.all_complete;
    let enabled = app.s.enabled("mail.markComplete").is_none();
    if row(
        ui,
        app,
        if complete { Glyph::Check(true) } else { Glyph::Icon("flag-complete") },
        "Mark Complete",
        enabled,
    ) {
        app.run("mail.markComplete", t.params());
    }
    cmd_if(ui, app, "", "Clear Flag", t.any_flag, "mail.clearFlag", t.params());
    ui.separator();
    cmd(ui, app, "", "Set Quick Click...", "mail.setQuickClickFlag", json!({}));
    cmd(ui, app, "", "Set Quick Actions...", "view.quickActions", json!({}));
}

fn quick_steps_menu(ui: &mut Ui, app: &mut App, t: &Targets) {
    let steps: Vec<(u64, String)> =
        app.s.mailbox.quick_steps.iter().map(|q| (q.id, q.name.clone())).collect();
    for (id, name) in steps {
        cmd(ui, app, "quick-steps", &name, "quicksteps.run", t.with(json!({"id": id})));
    }
    if !app.s.mailbox.quick_steps.is_empty() {
        ui.separator();
    }
    sub(ui, app, "plus", "New Quick Step", true, |ui, app| {
        for (label, template) in [
            ("Move to Folder...", "moveToFolder"),
            ("Categorize & Move...", "categorizeMove"),
            ("Flag & Move...", "flagMove"),
            ("New E-mail To...", "newEmailTo"),
            ("Forward To...", "forwardTo"),
            ("New Meeting...", "newMeeting"),
            ("Custom...", "custom"),
        ] {
            cmd(ui, app, "", label, "quicksteps.new", json!({"template": template}));
        }
    });
    cmd(ui, app, "settings", "Manage Quick Steps...", "quicksteps.manage", json!({}));
}

fn rules_menu(ui: &mut Ui, app: &mut App, t: &Targets, sender: Option<&str>) {
    if let Some(sender) = sender {
        let label = format!("Always Move Messages From: {sender}");
        cmd(ui, app, "move", &label, "rules.alwaysMoveFrom", t.params());
        ui.separator();
    }
    let one = t.ids.len() == 1;
    cmd_if(ui, app, "rules", "Create Rule...", one, "rules.createFromMessage", t.params());
    cmd(ui, app, "", "Manage Rules & Alerts...", "rules.manage", json!({}));
}

fn move_menu(ui: &mut Ui, app: &mut App, t: &Targets, conversation: bool) {
    let current = app.s.selection.folder;
    let recent: Vec<(FolderId, String)> = recent_folders(ui.ctx())
        .into_iter()
        .filter(|f| *f != current)
        .filter_map(|f| app.s.mailbox.folder(f).map(|x| (f, x.name.clone())))
        .collect();
    for (f, name) in &recent {
        if row(ui, app, Glyph::Icon("folder"), name, !t.ids.is_empty())
            && app.run("mail.move", t.with(json!({"folder": f.0}))).is_some()
        {
            note_recent_folder(ui.ctx(), *f);
        }
    }
    if !recent.is_empty() {
        ui.separator();
    }
    cmd(ui, app, "move", "Other Folder...", "mail.move", t.params());
    cmd(ui, app, "copy", "Copy to Folder...", "mail.copy", t.params());
    cmd_if(
        ui,
        app,
        "",
        "Always Move Messages in This Conversation...",
        conversation,
        "mail.alwaysMoveConversation",
        t.params(),
    );
}

fn report_menu(ui: &mut Ui, app: &mut App, t: &Targets) {
    cmd(ui, app, "warning", "Report Phishing", "mail.report", t.with(json!({"kind": "phishing"})));
    cmd(ui, app, "junk", "Report Junk", "mail.report", t.with(json!({"kind": "junk"})));
}

fn block_menu(ui: &mut Ui, app: &mut App, t: &Targets, domain: Option<&str>) {
    cmd(ui, app, "block", "Block Sender", "mail.block", t.with(json!({"kind": "sender"})));
    cmd(ui, app, "", "Never Block Sender", "mail.block", t.with(json!({"kind": "neverSender"})));
    let label = match domain {
        Some(d) => format!("Never Block Sender's Domain (@{d})"),
        None => "Never Block Sender's Domain".to_owned(),
    };
    cmd(ui, app, "", &label, "mail.block", t.with(json!({"kind": "neverDomain"})));
    cmd(
        ui,
        app,
        "",
        "Never Block this Group or Mailing List",
        "mail.block",
        t.with(json!({"kind": "neverList"})),
    );
    ui.separator();
    cmd(ui, app, "junk-folder", "Junk E-mail Options...", "mail.junk", json!({"action": "options"}));
}

// ---------------------------------------------------------------------------------------------------------
// Message row

/// Selects `ids` in the list unless they already make up the selection (right-click acts on the selection).
fn ensure_selected(app: &mut App, ids: &[ItemId]) {
    if ids.is_empty() {
        return;
    }
    let sel = &app.s.selection.items;
    let all_in = ids.iter().all(|i| sel.contains(i));
    let single_in = ids.len() == 1 && all_in;
    let same = sel.len() == ids.len() && all_in;
    if single_in || same {
        return;
    }
    let params =
        if ids.len() == 1 { json!({"id": ids.first().map(|i| i.0)}) } else { json!({"ids": ids_json(ids)}) };
    app.run("nav.selectItem", params);
}

/// Context menu of a message row (right-click). Acts on the selection when the row is part of it, else on
/// the row (which becomes selected).
pub fn message_row_menu(ui: &mut Ui, app: &mut App, item: u64) {
    let id = ItemId(item);
    let Some(m) = app.s.mailbox.mail.get(&id) else {
        ui.label("This item no longer exists.");
        return;
    };
    let read = m.read;
    let ignored = m.ignored;
    let conversation = m.conversation.is_some();
    let folder = m.folder;
    let sender = m.from.as_ref().map(|a| a.addr.clone()).filter(|a| !a.is_empty());
    ensure_selected(app, &[id]);
    let ids = if app.s.selection.items.contains(&id) { app.s.selection.items.clone() } else { vec![id] };
    let t = Targets::of(app, ids);
    let domain = sender.as_deref().and_then(|a| a.rsplit_once('@')).map(|(_, d)| d.to_owned());

    cmd(ui, app, "copy", "Copy", "app.copy", t.params());
    cmd(ui, app, "print", "Quick Print", "mail.quickPrint", t.params());
    ui.separator();
    cmd(ui, app, "reply", "Reply", "mail.reply", t.params());
    cmd(ui, app, "reply-all", "Reply All", "mail.replyAll", t.params());
    cmd(ui, app, "forward", "Forward", "mail.forward", t.params());
    ui.separator();
    if read {
        cmd(ui, app, "unread", "Mark as Unread", "mail.markUnread", t.params());
    } else {
        cmd(ui, app, "read", "Mark as Read", "mail.markRead", t.params());
    }
    sub(ui, app, "categorize", "Categorize", true, |ui, app| categorize_menu(ui, app, &t));
    sub(ui, app, "flag", "Follow Up", true, |ui, app| follow_up_menu(ui, app, &t));
    ui.separator();
    sub(ui, app, "search", "Find Related", t.ids.len() == 1, |ui, app| {
        cmd(
            ui,
            app,
            "conversation",
            "Messages in this Conversation",
            "mail.findRelated",
            t.with(json!({"kind": "conversation"})),
        );
        cmd_if(
            ui,
            app,
            "person",
            "Messages from Sender",
            sender.is_some(),
            "mail.findRelated",
            t.with(json!({"kind": "sender"})),
        );
    });
    ui.separator();
    sub(ui, app, "quick-steps", "Quick Steps", true, |ui, app| quick_steps_menu(ui, app, &t));
    cmd(ui, app, "", "Set Quick Actions...", "view.quickActions", json!({}));
    sub(ui, app, "rules", "Rules", true, |ui, app| rules_menu(ui, app, &t, sender.as_deref()));
    sub(ui, app, "move", "Move", true, |ui, app| move_menu(ui, app, &t, conversation));
    if focused_inbox_active(app, folder) {
        ui.separator();
        cmd(ui, app, "", "Move to Other", "mail.moveToOther", t.params());
        cmd(ui, app, "", "Always Move to Other", "mail.alwaysMoveToOther", t.params());
    }
    ui.separator();
    check_cmd(ui, app, ignored, "Ignore", "mail.ignore", t.params());
    sub(ui, app, "junk", "Report", true, |ui, app| report_menu(ui, app, &t));
    sub(ui, app, "block", "Block", sender.is_some(), |ui, app| block_menu(ui, app, &t, domain.as_deref()));
    cmd(ui, app, "delete", "Delete", "mail.delete", t.params());
    cmd(ui, app, "archive", "Archive...", "mail.archive", t.params());
}

// ---------------------------------------------------------------------------------------------------------
// Conversations

/// Context menu of a conversation header row (conversation view): acts on the conversation's messages in
/// this folder; Reply / Forward answer its newest message. The header becomes the selection.
pub fn conversation_menu(ui: &mut Ui, app: &mut App, header: u64) {
    let id = ItemId(header);
    let Some(conv) = queries::conversation_of(&app.s, id).filter(|c| c.is_thread()) else {
        message_row_menu(ui, app, header);
        return;
    };
    let ignored = app.s.mailbox.mail.get(&id).is_some_and(|m| m.ignored);
    ensure_selected(app, &[id]);
    let t = Targets::of(app, conv.view_members());
    let one = json!({"ids": [header]});
    let any_unread = t.ids.iter().any(|i| app.s.mailbox.mail.get(i).is_some_and(|m| !m.read));
    let any_read = t.ids.iter().any(|i| app.s.mailbox.mail.get(i).is_some_and(|m| m.read));

    let toggle = json!({"conversation": true, "id": header});
    // The engine state after selecting (the selected conversation shows expanded).
    let expanded = queries::conversation_of(&app.s, id).is_some_and(|c| c.expanded);
    if expanded {
        cmd(ui, app, "chevron-up", "Collapse Conversation", "view.collapseGroup", toggle);
    } else {
        cmd(ui, app, "chevron-down", "Expand Conversation", "view.expandGroup", toggle);
    }
    ui.separator();
    cmd(ui, app, "reply", "Reply", "mail.reply", one.clone());
    cmd(ui, app, "reply-all", "Reply All", "mail.replyAll", one.clone());
    cmd(ui, app, "forward", "Forward", "mail.forward", one.clone());
    ui.separator();
    cmd_if(ui, app, "read", "Mark as Read", any_unread, "mail.markRead", t.params());
    cmd_if(ui, app, "unread", "Mark as Unread", any_read, "mail.markUnread", t.params());
    sub(ui, app, "categorize", "Categorize", true, |ui, app| categorize_menu(ui, app, &t));
    sub(ui, app, "flag", "Follow Up", true, |ui, app| follow_up_menu(ui, app, &t));
    ui.separator();
    cmd(ui, app, "clean-up", "Clean Up Conversation", "mail.cleanUpConversation", one.clone());
    check_cmd(ui, app, ignored, "Ignore", "mail.ignore", one);
    sub(ui, app, "move", "Move", true, |ui, app| move_menu(ui, app, &t, true));
    ui.separator();
    cmd(ui, app, "delete", "Delete", "mail.delete", t.params());
}

/// Conversation Settings ▸ items with the selected folder's state (View › Messages › Conversation
/// Settings).
pub fn conversation_settings_menu(ui: &mut Ui, app: &mut App) {
    let cs = queries::conversation_settings(&app.s, app.s.selection.folder);
    for (name, label) in [
        ("fromOtherFolders", "Show Messages from Other Folders"),
        ("sendersAboveSubject", "Show Senders Above the Subject"),
        ("alwaysExpand", "Always Expand Selected Conversation"),
        ("classicIndent", "Use Classic Indented View"),
    ] {
        let on = cs.get(name).unwrap_or(false);
        check_cmd(ui, app, on, label, "view.conversationSettings", json!({"setting": name, "on": !on}));
    }
}

/// Show as Conversations (asks This folder / All mailboxes first) and Conversation Settings ▸, for menus
/// that offer the conversation view.
pub fn conversation_view_menu(ui: &mut Ui, app: &mut App) {
    let on = queries::view_settings(&app.s, app.s.selection.folder).conversations;
    let enabled = app.s.enabled("view.conversations").is_none();
    if row(ui, app, Glyph::Check(on), "Show as Conversations", enabled) {
        crate::list::ask_conversation_scope(ui.ctx(), app);
    }
    sub(ui, app, "settings", "Conversation Settings", on, conversation_settings_menu);
}

/// The folder is an Inbox showing Focused / Other.
fn focused_inbox_active(app: &App, folder: FolderId) -> bool {
    let is_inbox = app.s.mailbox.folder(folder).is_some_and(|f| f.role == Some(FolderRole::Inbox));
    is_inbox && queries::view_settings(&app.s, folder).focused_inbox
}

// ---------------------------------------------------------------------------------------------------------
// Group header

#[derive(Clone, Debug, Default)]
struct GroupCache {
    label: String,
    folder: FolderId,
    ids: Vec<ItemId>,
}

/// Context menu of a date-group header (acts on every item of the group, which becomes the selection).
pub fn group_header_menu(ui: &mut Ui, app: &mut App, label: &str) {
    let cache_id = Id::new("mailcraft.menus.group");
    let folder = app.s.selection.folder;
    let cached = ui.ctx().data_mut(|d| d.get_temp::<GroupCache>(cache_id));
    let ids = match cached {
        Some(c) if c.label == label && c.folder == folder => c.ids,
        _ => {
            let ids = queries::group_items(&app.s, label);
            let c = GroupCache { label: label.to_owned(), folder, ids: ids.clone() };
            ui.ctx().data_mut(|d| d.insert_temp(cache_id, c));
            ids
        }
    };
    ensure_selected(app, &ids);
    let t = Targets::of(app, ids);
    let any_unread = t.ids.iter().any(|i| app.s.mailbox.mail.get(i).is_some_and(|m| !m.read));
    let any_read = t.ids.iter().any(|i| app.s.mailbox.mail.get(i).is_some_and(|m| m.read));

    cmd(ui, app, "collapse-ribbon", "Collapse All Groups", "view.collapseAll", json!({}));
    cmd(ui, app, "chevron-down", "Expand All Groups", "view.expandAll", json!({}));
    ui.separator();
    cmd(ui, app, "envelope-open", "Open", "mail.open", t.params());
    cmd(ui, app, "copy", "Copy", "app.copy", json!({"group": label}));
    ui.separator();
    cmd_if(ui, app, "read", "Mark as Read", any_unread, "mail.markRead", t.params());
    cmd_if(ui, app, "unread", "Mark as Unread", any_read, "mail.markUnread", t.params());
    sub(ui, app, "categorize", "Categorize", !t.ids.is_empty(), |ui, app| categorize_menu(ui, app, &t));
    sub(ui, app, "flag", "Follow Up", !t.ids.is_empty(), |ui, app| follow_up_menu(ui, app, &t));
    ui.separator();
    sub(ui, app, "quick-steps", "Quick Steps", !t.ids.is_empty(), |ui, app| quick_steps_menu(ui, app, &t));
    cmd(ui, app, "", "Set Quick Actions...", "view.quickActions", json!({}));
    sub(ui, app, "rules", "Rules", true, |ui, app| rules_menu(ui, app, &t, None));
    sub(ui, app, "move", "Move", !t.ids.is_empty(), |ui, app| move_menu(ui, app, &t, false));
    ui.separator();
    cmd(ui, app, "delete", "Delete", "mail.delete", t.params());
}

// ---------------------------------------------------------------------------------------------------------
// Folders and accounts

/// Runs a folder command for `folder`; when the command is disabled only because another folder is
/// selected, selects `folder` first.
fn run_folder(app: &mut App, folder: FolderId, id: &str, params: Value) -> Option<Value> {
    if app.s.selection.folder != folder && app.s.enabled(id).is_some() {
        app.run("nav.selectFolder", json!({"folder": folder.0}));
    }
    app.run(id, params)
}

fn folder_row(
    ui: &mut Ui,
    app: &mut App,
    f: FolderId,
    icon: &str,
    label: &str,
    enabled: bool,
    id: &str,
    p: Value,
) {
    let glyph = if icon.is_empty() { Glyph::None } else { Glyph::Icon(icon) };
    if row(ui, app, glyph, label, enabled) {
        run_folder(app, f, id, p);
    }
}

/// Context menu of a folder in the account tree.
pub fn folder_menu(ui: &mut Ui, app: &mut App, folder: FolderId) {
    folder_menu_in(ui, app, folder, false);
}

/// Context menu of a folder shown under Favorites (Move Up / Down reorder Favorites).
pub fn favorite_folder_menu(ui: &mut Ui, app: &mut App, folder: FolderId) {
    folder_menu_in(ui, app, folder, true);
}

fn folder_menu_in(ui: &mut Ui, app: &mut App, folder: FolderId, in_favorites: bool) {
    let mb = &app.s.mailbox;
    let Some(f) = mb.folder(folder) else {
        ui.label("This folder no longer exists.");
        return;
    };
    let user = !f.is_default();
    let is_fav = f.favorite_pos.is_some();
    let deleted_items = f.role == Some(FolderRole::DeletedItems);
    let has_children = mb.folders.values().any(|c| c.parent == Some(folder));
    let has_items = !mb.items_in(folder).is_empty();
    let unread = mb.unread_count(folder) > 0;
    let (can_up, can_down) = if in_favorites {
        let favs = mb.favorites();
        let pos = favs.iter().position(|x| *x == folder);
        (pos.is_some_and(|p| p > 0), pos.is_some_and(|p| p.saturating_add(1) < favs.len()))
    } else {
        (user, user)
    };
    let p = json!({"folder": folder.0});
    let where_ = if in_favorites { "favorites" } else { "tree" };

    folder_row(
        ui,
        app,
        folder,
        "folder-open",
        "Open in New Window",
        true,
        "folder.openInNewWindow",
        p.clone(),
    );
    ui.separator();
    folder_row(ui, app, folder, "folder", "New Folder...", true, "folder.new", json!({"parent": folder.0}));
    folder_row(ui, app, folder, "", "Rename Folder", user, "folder.rename", p.clone());
    folder_row(ui, app, folder, "copy", "Copy Folder", true, "folder.copy", p.clone());
    folder_row(ui, app, folder, "move", "Move Folder", user, "folder.move", p.clone());
    folder_row(ui, app, folder, "delete", "Delete Folder", user, "folder.delete", p.clone());
    ui.separator();
    folder_row(ui, app, folder, "read", "Mark All as Read", unread, "folder.markAllRead", p.clone());
    folder_row(ui, app, folder, "clean-up", "Clean Up Folder", true, "mail.cleanUpFolder", p.clone());
    folder_row(ui, app, folder, "", "Delete All", has_items, "folder.deleteAll", p.clone());
    if deleted_items {
        folder_row(
            ui,
            app,
            folder,
            "deleted",
            "Empty Folder",
            has_items || has_children,
            "folder.emptyDeleted",
            p.clone(),
        );
    }
    ui.separator();
    if is_fav {
        folder_row(
            ui,
            app,
            folder,
            "favorites",
            "Remove from Favorites",
            true,
            "folder.removeFavorite",
            p.clone(),
        );
    } else {
        folder_row(ui, app, folder, "favorites", "Add to Favorites", true, "folder.addFavorite", p.clone());
    }
    ui.separator();
    folder_row(ui, app, folder, "sort", "Sort Subfolders A to Z", has_children, "folder.sortAZ", p.clone());
    folder_row(
        ui,
        app,
        folder,
        "chevron-up",
        "Move Up",
        can_up,
        "folder.moveUp",
        json!({"folder": folder.0, "in": where_}),
    );
    folder_row(
        ui,
        app,
        folder,
        "chevron-down",
        "Move Down",
        can_down,
        "folder.moveDown",
        json!({"folder": folder.0, "in": where_}),
    );
    ui.separator();
    folder_row(ui, app, folder, "info", "Properties...", true, "folder.properties", p);
}

/// Context menu of an account (mailbox / data file) root in the folder pane.
pub fn account_menu(ui: &mut Ui, app: &mut App, account: u32) {
    let acc = AccountId(account);
    let mb = &app.s.mailbox;
    let Some(a) = mb.accounts.iter().find(|a| a.id == acc) else {
        ui.label("This account no longer exists.");
        return;
    };
    let is_default = a.is_default;
    let inbox = mb.role_folder(acc, FolderRole::Inbox).or_else(|| mb.children(acc, None).first().copied());
    let store = json!({"store": account});

    if let Some(inbox) = inbox {
        folder_row(
            ui,
            app,
            inbox,
            "folder-open",
            "Open in New Window",
            true,
            "folder.openInNewWindow",
            json!({"folder": inbox.0}),
        );
    }
    cmd(ui, app, "folder-open", "Open File Location", "file.openFileLocation", store.clone());
    cmd_if(ui, app, "close", "Remove", !is_default, "file.closeDataFile", store);
    ui.separator();
    cmd(ui, app, "folder", "New Folder...", "folder.new", json!({}));
    cmd_if(ui, app, "favorites", "Add to Favorites", false, "folder.addFavorite", json!({}));
    cmd(ui, app, "sort", "Sort Subfolders A to Z", "folder.sortAZ", json!({"account": account}));
    ui.separator();
    cmd(ui, app, "account", "Account Properties...", "file.accountSettings", json!({"account": account}));
    if let Some(inbox) = inbox {
        folder_row(
            ui,
            app,
            inbox,
            "info",
            "Data File Properties...",
            true,
            "folder.properties",
            json!({"folder": inbox.0}),
        );
    }
}

// ---------------------------------------------------------------------------------------------------------
// Status bar

/// Status bar elements (Customize Status Bar): (key used by `view.statusBar`, label).
pub const STATUS_BAR_ITEMS: [(&str, &str); 11] = [
    ("quota", "Quota Information"),
    ("filter", "Filter"),
    ("itemsInView", "Items in View"),
    ("headerItems", "Header Items in View"),
    ("unreadItems", "Unread Items in View"),
    ("unseenItems", "Unseen Items in View"),
    ("reminders", "Reminders"),
    ("groupNotifications", "Group Notifications"),
    ("viewShortcuts", "View Shortcuts"),
    ("zoomSlider", "Zoom Slider"),
    ("zoom", "Zoom"),
];

/// Which status bar elements are hidden (the session keeps no field for this; the UI does).
#[derive(Clone, Debug)]
pub struct StatusBarItems {
    pub hidden: BTreeSet<String>,
}

impl Default for StatusBarItems {
    fn default() -> Self {
        // Quota Information is off by default; everything else shows.
        StatusBarItems { hidden: ["quota".to_owned()].into_iter().collect() }
    }
}

impl StatusBarItems {
    pub fn shown(&self, key: &str) -> bool {
        !self.hidden.contains(key)
    }
}

fn status_id() -> Id {
    Id::new("mailcraft.menus.statusBar")
}

/// Current status bar element visibility (the status bar reads this when drawing).
pub fn status_bar_items(ctx: &egui::Context) -> StatusBarItems {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<StatusBarItems>(status_id()).clone())
}

/// Whether one status bar element (`STATUS_BAR_ITEMS` key) is shown.
pub fn status_bar_item_shown(ctx: &egui::Context, key: &str) -> bool {
    status_bar_items(ctx).shown(key)
}

/// `7219` → `"7,219"`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Customize Status Bar menu (right-click on the status bar).
pub fn status_bar_menu(ui: &mut Ui, app: &mut App) {
    let items = status_bar_items(ui.ctx());
    let (in_view, unread) = queries::status_counts(&app.s);
    let filtered = queries::view_settings(&app.s, app.s.selection.folder).filter != ListFilter::All
        || !app.s.view.search.trim().is_empty();
    for (key, label) in STATUS_BAR_ITEMS {
        let value = match key {
            "quota" => "Off".to_owned(),
            "filter" => {
                if filtered {
                    "Filter Applied".to_owned()
                } else {
                    String::new()
                }
            }
            "itemsInView" => thousands(in_view),
            "unreadItems" => thousands(unread),
            "zoom" => format!("{}%", app.s.view.zoom),
            _ => String::new(),
        };
        let on = items.shown(key);
        let id = ui.id().with(("status-item", key));
        let btn = Button::new((Atom::custom(id, vec2(ICON, ICON)), label)).right_text(value);
        let resp = btn.atom_ui(ui);
        if let Some(rect) = resp.rect(id) {
            paint_glyph(ui, app, rect, &Glyph::Check(on), true);
        }
        if resp.response.clicked() {
            let show = !on;
            if app.run("view.statusBar", json!({"item": key, "show": show})).is_some() {
                ui.ctx().data_mut(|d| {
                    let s = d.get_temp_mut_or_default::<StatusBarItems>(status_id());
                    if show {
                        s.hidden.remove(key);
                    } else {
                        s.hidden.insert(key.to_owned());
                    }
                });
            }
            ui.close();
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Arrange / filter / sort ("By Date ▾")

/// Labels of the two sort directions for an arrangement: [(label, dir)] in menu order.
pub fn sort_labels(a: ArrangeBy) -> [(&'static str, &'static str); 2] {
    match a {
        ArrangeBy::Date | ArrangeBy::FlagStartDate | ArrangeBy::FlagDueDate => {
            [("Newest on Top", "desc"), ("Oldest on Top", "asc")]
        }
        ArrangeBy::Size => [("Largest on Top", "desc"), ("Smallest on Top", "asc")],
        ArrangeBy::Importance => [("High on Top", "desc"), ("Low on Top", "asc")],
        _ => [("A on Top", "asc"), ("Z on Top", "desc")],
    }
}

/// The message list's "By Date ▾" menu: Filter, Arrange by (13 keys), Sort, Show Focused Inbox.
pub fn arrange_menu(ui: &mut Ui, app: &mut App) {
    let folder = app.s.selection.folder;
    let vs = queries::view_settings(&app.s, folder);
    heading(ui, "Filter");
    for (label, kind, f) in [
        ("All Mail", "all", ListFilter::All),
        ("Unread Mail", "unread", ListFilter::Unread),
        ("Flagged Mail", "flagged", ListFilter::Flagged),
        ("Mentioned Mail", "mentioned", ListFilter::Mentioned),
    ] {
        check_cmd(ui, app, vs.filter == f, label, "view.filter", json!({"kind": kind}));
    }
    ui.separator();
    heading(ui, "Arrange by");
    for a in ArrangeBy::ALL {
        check_cmd(ui, app, vs.arrange_by == a, a.label(), "view.arrangeBy", json!({"key": a.label()}));
    }
    ui.separator();
    heading(ui, "Sort");
    for (label, dir) in sort_labels(vs.arrange_by) {
        let on = match vs.order {
            SortOrder::Descending => dir == "desc",
            SortOrder::Ascending => dir == "asc",
        };
        check_cmd(ui, app, on, label, "view.sortOrder", json!({"dir": dir}));
    }
    ui.separator();
    let inbox = app.s.current_account().and_then(|a| app.s.mailbox.role_folder(a, FolderRole::Inbox));
    let focused = inbox.is_some_and(|i| queries::view_settings(&app.s, i).focused_inbox);
    check_cmd(ui, app, focused, "Show Focused Inbox", "view.focusedInbox", json!({}));
}
