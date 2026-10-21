//! Context menus (keys/menus area): message row, group header, folder, account root, status bar, sort menu.
//!
//! Contents follow `plan/reference/context-menus.md` and `message-list.md`; every entry runs a command through
//! [`App::run`]. Entries for services MailCraft does not have (retention policies, OneNote) are left out.
//! Callers open these from `Response::context_menu(|ui| menus::…(ui, app, …))` (or a `menu_button` for the
//! arrange menu); the functions draw the menu body.

use std::collections::BTreeSet;

use egui::{Atom, Button, Color32, Id, Rect, Ui, vec2};
use serde_json::{Value, json};

use mailcraft_engine::cmd::quicksteps;
use mailcraft_engine::queries;
use mailcraft_model::{
    AccountId, ArrangeBy, FlagState, FolderId, FolderKind, FolderRole, ItemId, ListFilter, SortOrder,
};

use crate::App;
use crate::menu_keys::{self, Access, Hit, literal};

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

/// One menu row with an icon column; returns true when clicked or its access key is typed (the menu then
/// closes). `label` marks the access key with `&` ([`menu_keys`]).
fn row(ui: &mut Ui, app: &mut App, glyph: Glyph<'_>, label: &str, enabled: bool) -> bool {
    let (text, at) = menu_keys::parse(label);
    let key = menu_keys::key_at(&text, at, false);
    row_keyed(ui, app, glyph, &text, at, key, enabled)
}

/// [`row`] with the text and access key given apart (Alt keys of the arrange menu).
fn row_keyed(
    ui: &mut Ui,
    app: &mut App,
    glyph: Glyph<'_>,
    text: &str,
    at: Option<usize>,
    key: Option<Access>,
    enabled: bool,
) -> bool {
    let id = ui.id().with(("menu-row", text));
    let hit = menu_keys::access(ui, id, key, enabled);
    let btn = Button::new((Atom::custom(id, vec2(ICON, ICON)), menu_keys::label(ui, text, at)));
    let resp = ui.add_enabled_ui(enabled, |ui| btn.atom_ui(ui)).inner;
    if let Some(rect) = resp.rect(id) {
        paint_glyph(ui, app, rect, &glyph, enabled);
    }
    menu_keys::drawn(ui, id, resp.response.id);
    if hit == Hit::Focus {
        resp.response.request_focus();
    }
    let clicked = resp.response.clicked() || (enabled && hit == Hit::Activate);
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
    let (text, at) = menu_keys::parse(label);
    let id = ui.id().with(("menu-sub", text.as_str()));
    let hit = menu_keys::access(ui, id, menu_keys::key_at(&text, at, false), enabled);
    let shown = menu_keys::label(ui, &text, at);
    let resp = ui
        .add_enabled_ui(enabled, |ui| {
            // The submenu's button takes the next id; its access key opens the submenu.
            let button = ui.next_auto_id();
            if hit == Hit::Activate {
                menu_keys::open_submenu(ui, button);
            }
            let r = ui.menu_button((Atom::custom(id, vec2(ICON, ICON)), shown), |ui| add(ui, app)).response;
            menu_keys::drawn(ui, id, r.id);
            if hit == Hit::Focus {
                r.request_focus();
            }
            r
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
    /// The Follow Up entry every target's flag came from (checked in the menu).
    follow_up: Option<&'static str>,
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
        let kinds: Vec<Option<&'static str>> =
            items.iter().map(|m| mailcraft_engine::cmd::mail::follow_up_kind(&app.s, &m.flag)).collect();
        let follow_up = match kinds.first() {
            Some(Some(k)) if kinds.iter().all(|x| *x == Some(*k)) => Some(*k),
            _ => None,
        };
        Targets {
            follow_up,
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
    cmd_if(ui, app, "", "&Clear All Categories", t.any_category, "mail.clearCategories", t.params());
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
    // Alphabetical, as the original lists them.
    let mut cats = cats;
    cats.sort_by_key(|(name, _)| name.to_lowercase());
    for (name, color) in cats {
        let on = t.common.contains(&name.to_lowercase());
        let glyph = if on { Glyph::Check(true) } else { Glyph::Swatch(color) };
        if row(ui, app, glyph, &literal(&name), true) {
            app.run("mail.categorize", t.with(json!({"name": name})));
        }
    }
    ui.separator();
    cmd(ui, app, "categorize", "&All Categories...", "mail.allCategories", t.params());
    cmd(ui, app, "", "Set &Quick Click...", "categories.setQuickClick", json!({}));
}

/// The Follow Up submenu. On a group header (`group`) Custom... and Add Reminder... are greyed out: they set
/// one message's dates.
fn follow_up_menu(ui: &mut Ui, app: &mut App, t: &Targets, group: bool) {
    for (label, kind) in [
        ("&Today", "today"),
        ("T&omorrow", "tomorrow"),
        ("This &Week", "thisWeek"),
        ("&Next Week", "nextWeek"),
        ("No D&ate", "noDate"),
    ] {
        let glyph = if t.follow_up == Some(kind) { Glyph::Check(true) } else { Glyph::Icon("flag") };
        if row(ui, app, glyph, label, true) {
            app.run("mail.flag", t.with(json!({"kind": kind})));
        }
    }
    let custom_on = !group && app.s.enabled("mail.flagCustom").is_none();
    cmd_if(ui, app, "flag", "&Custom...", custom_on, "mail.flagCustom", t.params());
    ui.separator();
    let reminder_on = !group && app.s.enabled("mail.addReminder").is_none();
    cmd_if(ui, app, "bell", "Add &Reminder...", reminder_on, "mail.addReminder", t.params());
    let complete = t.all_complete;
    let enabled = app.s.enabled("mail.markComplete").is_none();
    if row(
        ui,
        app,
        if complete { Glyph::Check(true) } else { Glyph::Icon("flag-complete") },
        "&Mark Complete",
        enabled,
    ) {
        app.run("mail.markComplete", t.params());
    }
    cmd_if(ui, app, "", "Cl&ear Flag", t.any_flag, "mail.clearFlag", t.params());
    ui.separator();
    cmd(ui, app, "", "Set &Quick Click...", "mail.setQuickClickFlag", json!({}));
    cmd(ui, app, "", "&Set Quick Actions...", "view.quickActions", json!({}));
}

/// The Quick Steps submenu of the message and group-header menus. A step that replies is greyed out for
/// several messages or a group, one that forwards for a group (`quicksteps::blocked_for`).
fn quick_steps_menu(ui: &mut Ui, app: &mut App, t: &Targets, group: bool) {
    let count = t.ids.len();
    let steps: Vec<(u64, String, bool)> = app
        .s
        .mailbox
        .quick_steps
        .iter()
        .map(|q| (q.id, q.name.clone(), quicksteps::blocked_for(q, count, group).is_none()))
        .collect();
    for (id, name, ok) in steps {
        let params = if group { t.with(json!({"id": id, "group": true})) } else { t.with(json!({"id": id})) };
        cmd_if(ui, app, "quick-steps", &literal(&name), ok, "quicksteps.run", params);
    }
    // The fixed Create New entry (as the ribbon gallery has it): the Edit Quick Step editor for a new step.
    cmd(ui, app, "plus", "Create New", "quicksteps.new", json!({"template": "custom"}));
    ui.separator();
    sub(ui, app, "plus", "&New Quick Step", true, |ui, app| {
        for (label, template) in [
            ("&Move to Folder...", "moveToFolder"),
            ("Ca&tegorize && Move...", "categorizeMove"),
            ("Fla&g && Move...", "flagMove"),
            ("&New E-mail To...", "newEmailTo"),
            ("&Forward To...", "forwardTo"),
            ("Ne&w Meeting...", "newMeeting"),
            ("&Custom...", "custom"),
        ] {
            cmd(ui, app, "", label, "quicksteps.new", json!({"template": template}));
        }
    });
    cmd(ui, app, "settings", "&Manage Quick Steps...", "quicksteps.manage", json!({}));
}

fn rules_menu(ui: &mut Ui, app: &mut App, t: &Targets, sender: Option<&str>) {
    if let Some(sender) = sender {
        let label = format!("Always Move Messages From: {}", literal(sender));
        cmd(ui, app, "move", &label, "rules.alwaysMoveFrom", t.params());
        ui.separator();
    }
    let one = t.ids.len() == 1;
    cmd_if(ui, app, "rules", "Create R&ule...", one, "rules.createFromMessage", t.params());
    cmd(ui, app, "", "Manage Ru&les && Alerts...", "rules.manage", json!({}));
}

fn move_menu(ui: &mut Ui, app: &mut App, t: &Targets, conversation: bool) {
    let current = app.s.selection.folder;
    let recent: Vec<(FolderId, String)> = recent_folders(ui.ctx())
        .into_iter()
        .filter(|f| *f != current)
        .filter_map(|f| app.s.mailbox.folder(f).map(|x| (f, x.name.clone())))
        .collect();
    for (f, name) in &recent {
        if row(ui, app, Glyph::Icon("folder"), &literal(name), !t.ids.is_empty())
            && app.run("mail.move", t.with(json!({"folder": f.0}))).is_some()
        {
            note_recent_folder(ui.ctx(), *f);
        }
    }
    if !recent.is_empty() {
        ui.separator();
    }
    cmd(ui, app, "move", "&Other Folder...", "mail.move", t.params());
    cmd(ui, app, "copy", "&Copy to Folder...", "mail.copy", t.params());
    cmd_if(
        ui,
        app,
        "",
        "&Always Move Messages in This Conversation...",
        conversation,
        "mail.alwaysMoveConversation",
        t.params(),
    );
}

fn report_menu(ui: &mut Ui, app: &mut App, t: &Targets) {
    cmd(ui, app, "warning", "Report &Phishing", "mail.report", t.with(json!({"kind": "phishing"})));
    cmd(ui, app, "junk", "Report &Junk", "mail.report", t.with(json!({"kind": "junk"})));
}

fn block_menu(ui: &mut Ui, app: &mut App, t: &Targets, domain: Option<&str>) {
    cmd(ui, app, "block", "&Block Sender", "mail.block", t.with(json!({"kind": "sender"})));
    cmd(ui, app, "", "Never Block &Sender", "mail.block", t.with(json!({"kind": "neverSender"})));
    let label = match domain {
        Some(d) => format!("Never Block Sender's &Domain (@{})", literal(d)),
        None => "Never Block Sender's &Domain".to_owned(),
    };
    cmd(ui, app, "", &label, "mail.block", t.with(json!({"kind": "neverDomain"})));
    // Only for a message that came through a group or mailing list.
    let focus = app.s.selection.focus.filter(|f| t.ids.contains(f)).or_else(|| t.ids.first().copied());
    let list = focus.and_then(|id| mailcraft_engine::cmd::mail::list_entry_of(&app.s, id)).is_some();
    cmd_if(
        ui,
        app,
        "",
        "Never Block this Group or &Mailing List",
        list,
        "mail.block",
        t.with(json!({"kind": "neverList"})),
    );
    ui.separator();
    cmd(ui, app, "junk-folder", "Junk E-mail &Options...", "mail.junk", json!({"action": "options"}));
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

    cmd(ui, app, "copy", "&Copy", "app.copy", t.params());
    cmd(ui, app, "print", "&Quick Print", "mail.quickPrint", t.params());
    ui.separator();
    // A reply answers one message: with several selected, Reply and Reply All are greyed out (Forward sends
    // them all as attachments of one message).
    let one = t.ids.len() == 1;
    let reply_on = one && app.s.enabled("mail.reply").is_none();
    let reply_all_on = one && app.s.enabled("mail.replyAll").is_none();
    cmd_if(ui, app, "reply", "&Reply", reply_on, "mail.reply", t.params());
    cmd_if(ui, app, "reply-all", "Reply &All", reply_all_on, "mail.replyAll", t.params());
    cmd(ui, app, "forward", "For&ward", "mail.forward", t.params());
    ui.separator();
    if read {
        cmd(ui, app, "unread", "Mark as U&nread", "mail.markUnread", t.params());
    } else {
        cmd(ui, app, "read", "Mar&k as Read", "mail.markRead", t.params());
    }
    sub(ui, app, "categorize", "Ca&tegorize", true, |ui, app| categorize_menu(ui, app, &t));
    sub(ui, app, "flag", "Follow &Up", true, |ui, app| follow_up_menu(ui, app, &t, false));
    ui.separator();
    sub(ui, app, "search", "&Find Related", t.ids.len() == 1, |ui, app| {
        cmd(
            ui,
            app,
            "conversation",
            "Messages in this &Conversation",
            "mail.findRelated",
            t.with(json!({"kind": "conversation"})),
        );
        cmd_if(
            ui,
            app,
            "person",
            "Messages from &Sender",
            sender.is_some(),
            "mail.findRelated",
            t.with(json!({"kind": "sender"})),
        );
    });
    ui.separator();
    sub(ui, app, "quick-steps", "&Quick Steps", true, |ui, app| quick_steps_menu(ui, app, &t, false));
    cmd(ui, app, "", "&Set Quick Actions...", "view.quickActions", json!({}));
    sub(ui, app, "rules", "Rule&s", true, |ui, app| rules_menu(ui, app, &t, sender.as_deref()));
    sub(ui, app, "move", "&Move", true, |ui, app| move_menu(ui, app, &t, conversation));
    if focused_inbox_active(app, folder) {
        ui.separator();
        // The tab showing decides the way: Focused offers Other, Other offers Focused.
        if app.s.view.focused_tab {
            cmd(ui, app, "", "Mo&ve to Other", "mail.moveToOther", t.params());
            cmd(ui, app, "", "A&lways Move to Other", "mail.alwaysMoveToOther", t.params());
        } else {
            cmd(ui, app, "", "Mo&ve to Focused", "mail.moveToFocused", t.params());
            cmd(ui, app, "", "A&lways Move to Focused", "mail.alwaysMoveToFocused", t.params());
        }
    }
    ui.separator();
    check_cmd(ui, app, ignored, "&Ignore", "mail.ignore", t.params());
    sub(ui, app, "junk", "&Report", true, |ui, app| report_menu(ui, app, &t));
    // The reference notes record Block with no access key on this menu.
    sub(ui, app, "block", "Block", sender.is_some(), |ui, app| block_menu(ui, app, &t, domain.as_deref()));
    cmd(ui, app, "delete", "&Delete", "mail.delete", t.params());
    cmd(ui, app, "archive", "&Archive...", "mail.archive", t.params());
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
        cmd(ui, app, "chevron-up", "Co&llapse Conversation", "view.collapseGroup", toggle);
    } else {
        cmd(ui, app, "chevron-down", "E&xpand Conversation", "view.expandGroup", toggle);
    }
    ui.separator();
    cmd(ui, app, "reply", "&Reply", "mail.reply", one.clone());
    cmd(ui, app, "reply-all", "Reply &All", "mail.replyAll", one.clone());
    cmd(ui, app, "forward", "For&ward", "mail.forward", one.clone());
    ui.separator();
    cmd_if(ui, app, "read", "Mar&k as Read", any_unread, "mail.markRead", t.params());
    cmd_if(ui, app, "unread", "Mark as U&nread", any_read, "mail.markUnread", t.params());
    sub(ui, app, "categorize", "Ca&tegorize", true, |ui, app| categorize_menu(ui, app, &t));
    sub(ui, app, "flag", "Follow &Up", true, |ui, app| follow_up_menu(ui, app, &t, false));
    ui.separator();
    cmd(ui, app, "clean-up", "&Clean Up Conversation", "mail.cleanUpConversation", one.clone());
    check_cmd(ui, app, ignored, "&Ignore", "mail.ignore", one);
    sub(ui, app, "move", "&Move", true, |ui, app| move_menu(ui, app, &t, true));
    ui.separator();
    cmd(ui, app, "delete", "&Delete", "mail.delete", t.params());
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

    cmd(ui, app, "collapse-ribbon", "Collapse All &Groups", "view.collapseAll", json!({}));
    cmd(ui, app, "chevron-down", "E&xpand All Groups", "view.expandAll", json!({}));
    ui.separator();
    cmd(ui, app, "envelope-open", "&Open", "mail.open", t.params());
    cmd(ui, app, "copy", "&Copy", "app.copy", json!({"group": label}));
    ui.separator();
    cmd_if(ui, app, "read", "Mar&k as Read", any_unread, "mail.markRead", t.params());
    cmd_if(ui, app, "unread", "Mark as U&nread", any_read, "mail.markUnread", t.params());
    sub(ui, app, "categorize", "Ca&tegorize", !t.ids.is_empty(), |ui, app| categorize_menu(ui, app, &t));
    sub(ui, app, "flag", "Follow &Up", !t.ids.is_empty(), |ui, app| follow_up_menu(ui, app, &t, true));
    ui.separator();
    sub(ui, app, "quick-steps", "&Quick Steps", !t.ids.is_empty(), |ui, app| {
        quick_steps_menu(ui, app, &t, true)
    });
    cmd(ui, app, "", "&Set Quick Actions...", "view.quickActions", json!({}));
    sub(ui, app, "rules", "Rule&s", true, |ui, app| rules_menu(ui, app, &t, None));
    sub(ui, app, "move", "&Move", !t.ids.is_empty(), |ui, app| move_menu(ui, app, &t, false));
    ui.separator();
    cmd(ui, app, "delete", "&Delete", "mail.delete", t.params());
}

// ---------------------------------------------------------------------------------------------------------
// Folders and accounts

/// Runs a folder command for `folder`; when the command is disabled only because another folder is
/// selected, selects `folder` first.
pub(crate) fn run_folder(app: &mut App, folder: FolderId, id: &str, params: Value) -> Option<Value> {
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
        "Open in New &Window",
        true,
        "folder.openInNewWindow",
        p.clone(),
    );
    ui.separator();
    folder_row(ui, app, folder, "folder", "&New Folder...", true, "folder.new", json!({"parent": folder.0}));
    folder_row(ui, app, folder, "", "&Rename Folder", user, "folder.rename", p.clone());
    folder_row(ui, app, folder, "copy", "&Copy Folder", true, "folder.copy", p.clone());
    folder_row(ui, app, folder, "move", "&Move Folder", user, "folder.move", p.clone());
    folder_row(ui, app, folder, "delete", "&Delete Folder", user, "folder.delete", p.clone());
    // Paste the messages copied with Copy (Ctrl+C) into this folder.
    let can_paste = !app.s.clipboard.is_empty()
        && app.s.mailbox.folder(folder).is_some_and(|f| f.kind == FolderKind::Mail);
    folder_row(ui, app, folder, "paste", "Pa&ste", can_paste, "app.paste", p.clone());
    ui.separator();
    folder_row(ui, app, folder, "read", "Mark All as R&ead", unread, "folder.markAllRead", p.clone());
    folder_row(ui, app, folder, "clean-up", "C&lean Up Folder", true, "mail.cleanUpFolder", p.clone());
    folder_row(ui, app, folder, "", "Delete &All", has_items, "folder.deleteAll", p.clone());
    if deleted_items {
        folder_row(
            ui,
            app,
            folder,
            "deleted",
            "Empt&y Folder",
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
            "Remove from &Favorites",
            true,
            "folder.removeFavorite",
            p.clone(),
        );
    } else {
        folder_row(ui, app, folder, "favorites", "Add to &Favorites", true, "folder.addFavorite", p.clone());
    }
    ui.separator();
    folder_row(ui, app, folder, "sort", "Sort Subfolders A to &Z", has_children, "folder.sortAZ", p.clone());
    folder_row(
        ui,
        app,
        folder,
        "chevron-up",
        "Move &Up",
        can_up,
        "folder.moveUp",
        json!({"folder": folder.0, "in": where_}),
    );
    folder_row(
        ui,
        app,
        folder,
        "chevron-down",
        "Move D&own",
        can_down,
        "folder.moveDown",
        json!({"folder": folder.0, "in": where_}),
    );
    ui.separator();
    folder_row(ui, app, folder, "info", "&Properties...", true, "folder.properties", p);
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
            "Open in New &Window",
            true,
            "folder.openInNewWindow",
            json!({"folder": inbox.0}),
        );
    }
    cmd(ui, app, "folder-open", "&Open File Location", "file.openFileLocation", store.clone());
    cmd_if(ui, app, "close", "&Remove", !is_default, "file.closeDataFile", store);
    ui.separator();
    cmd(ui, app, "folder", "&New Folder...", "folder.new", json!({"account": account}));
    cmd(ui, app, "favorites", "Add to &Favorites", "folder.addFavorite", json!({"account": account}));
    cmd(ui, app, "sort", "Sort Subfolders A to &Z", "folder.sortAZ", json!({"account": account}));
    ui.separator();
    cmd(ui, app, "account", "&Account Properties...", "file.accountSettings", json!({"account": account}));
    cmd(ui, app, "info", "Data F&ile Properties...", "file.dataFileProperties", json!({"store": account}));
}

// ---------------------------------------------------------------------------------------------------------
// Status bar

/// Status bar elements (Customize Status Bar): (key used by `view.statusBar`, label).
pub const STATUS_BAR_ITEMS: [(&str, &str); 11] = mailcraft_engine::cmd::view::mail::STATUS_ITEMS;

/// Which status bar elements are hidden: a copy of the engine's setting (`view.statusBar`, saved with the
/// profile) refreshed when the settings change.
#[derive(Clone, Debug)]
pub struct StatusBarItems {
    pub hidden: BTreeSet<String>,
    /// Settings revision the copy was taken at (`None`: never taken; the defaults apply).
    rev: Option<u64>,
}

impl Default for StatusBarItems {
    fn default() -> Self {
        // Quota Information is off by default; everything else shows.
        StatusBarItems { hidden: ["quota".to_owned()].into_iter().collect(), rev: None }
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

/// Status bar element visibility as last read from the engine (the defaults before the first read).
pub fn status_bar_items(ctx: &egui::Context) -> StatusBarItems {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<StatusBarItems>(status_id()).clone())
}

/// Status bar element visibility from the engine's settings (read again only when they changed).
pub fn status_bar_items_of(ctx: &egui::Context, app: &App) -> StatusBarItems {
    let rev = app.s.settings.rev();
    let cur = status_bar_items(ctx);
    if cur.rev == Some(rev) {
        return cur;
    }
    let fresh =
        StatusBarItems { hidden: mailcraft_engine::cmd::view::mail::status_hidden(&app.s), rev: Some(rev) };
    ctx.data_mut(|d| d.insert_temp(status_id(), fresh.clone()));
    fresh
}

/// Whether one status bar element (`STATUS_BAR_ITEMS` key) is shown.
pub fn status_bar_item_shown(ctx: &egui::Context, key: &str) -> bool {
    status_bar_items(ctx).shown(key)
}

/// The list is filtered: Filter Email, a View Settings filter or a search.
pub fn filter_applied(app: &App) -> bool {
    let folder = app.s.selection.folder;
    queries::view_settings(&app.s, folder).filter != ListFilter::All
        || !app.s.view.search.trim().is_empty()
        || mailcraft_engine::cmd::view::mail::custom_filter(&app.s, folder).is_some()
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
    let items = status_bar_items_of(ui.ctx(), app);
    let (in_view, unread) = queries::status_counts(&app.s);
    let filtered = filter_applied(app);
    for (key, label) in STATUS_BAR_ITEMS {
        let value = match key {
            "quota" => match mailcraft_engine::sync::status_quota(&app.s).and_then(|(_, q)| q.percent()) {
                Some(p) => format!("{p}% full"),
                None => "Off".to_owned(),
            },
            "unseenItems" => {
                let n = queries::unseen_in_view(&app.s);
                if n > 0 { thousands(n) } else { String::new() }
            }
            "headerItems" => {
                let n = queries::header_items_in_view(&app.s);
                if n > 0 { thousands(n) } else { String::new() }
            }
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
            "reminders" => {
                let n = crate::chrome::reminders_due(app);
                if n > 0 { thousands(n) } else { String::new() }
            }
            _ => String::new(),
        };
        let on = items.shown(key);
        let id = ui.id().with(("status-item", key));
        // View Shortcuts (V), Zoom Slider and Zoom (Z) have access keys.
        let letter = match key {
            "viewShortcuts" => Some('V'),
            "zoomSlider" | "zoom" => Some('Z'),
            _ => None,
        };
        let at = letter.and_then(|c| menu_keys::find_key(label, c));
        let hit = menu_keys::access(ui, id, letter.and_then(Access::plain), true);
        let btn = Button::new((Atom::custom(id, vec2(ICON, ICON)), menu_keys::label(ui, label, at)))
            .right_text(value);
        let resp = btn.atom_ui(ui);
        if let Some(rect) = resp.rect(id) {
            paint_glyph(ui, app, rect, &Glyph::Check(on), true);
        }
        menu_keys::drawn(ui, id, resp.response.id);
        if hit == Hit::Focus {
            resp.response.request_focus();
        }
        if resp.response.clicked() || hit == Hit::Activate {
            // The engine keeps the state; the next read of the settings picks it up.
            app.run("view.statusBar", json!({"item": key, "show": !on}));
            status_bar_items_of(ui.ctx(), app);
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

/// The Alt access key of an arrangement in the By Date menu.
pub fn arrange_key(a: ArrangeBy) -> char {
    match a {
        ArrangeBy::Date => 'D',
        ArrangeBy::From => 'F',
        ArrangeBy::To => 'T',
        ArrangeBy::Categories => 'e',
        ArrangeBy::FlagStatus => 'l',
        ArrangeBy::FlagStartDate => 'g',
        ArrangeBy::FlagDueDate => 'u',
        ArrangeBy::Size => 'S',
        ArrangeBy::Subject => 'j',
        ArrangeBy::Type => 'y',
        ArrangeBy::Attachments => 'c',
        ArrangeBy::Account => 'o',
        ArrangeBy::Importance => 'I',
    }
}

/// A sort direction's label with its access key.
fn sort_marked(label: &str) -> String {
    let marked = match label {
        "Newest on Top" => "&Newest on Top",
        "Oldest on Top" => "O&ldest on Top",
        "Largest on Top" => "&Largest on Top",
        "Smallest on Top" => "&Smallest on Top",
        "High on Top" => "&High on Top",
        "Low on Top" => "L&ow on Top",
        "A on Top" => "A o&n Top",
        "Z on Top" => "&Z on Top",
        other => return literal(other),
    };
    marked.to_owned()
}

/// The message list's "By Date ▾" menu: Filter, Arrange by (13 keys), Sort, Show Focused Inbox.
pub fn arrange_menu(ui: &mut Ui, app: &mut App) {
    let folder = app.s.selection.folder;
    let vs = queries::view_settings(&app.s, folder);
    heading(ui, "Filter");
    for (label, kind, f) in [
        ("All &Mail", "all", ListFilter::All),
        ("Un&read Mail", "unread", ListFilter::Unread),
        ("Fl&agged Mail", "flagged", ListFilter::Flagged),
        ("M&entioned Mail", "mentioned", ListFilter::Mentioned),
    ] {
        check_cmd(ui, app, vs.filter == f, label, "view.filter", json!({"kind": kind}));
    }
    ui.separator();
    heading(ui, "Arrange by");
    let enabled = app.s.enabled("view.arrangeBy").is_none();
    for a in ArrangeBy::ALL {
        // The arrangements take their letter with Alt (Alt+D Date, Alt+F From, ...).
        let text = a.label();
        let ch = arrange_key(a);
        let at = menu_keys::find_key(text, ch);
        let glyph = Glyph::Check(vs.arrange_by == a);
        if row_keyed(ui, app, glyph, text, at, Access::alt(ch), enabled) {
            app.run("view.arrangeBy", json!({"key": text}));
        }
    }
    ui.separator();
    heading(ui, "Sort");
    for (label, dir) in sort_labels(vs.arrange_by) {
        let on = match vs.order {
            SortOrder::Descending => dir == "desc",
            SortOrder::Ascending => dir == "asc",
        };
        check_cmd(ui, app, on, &sort_marked(label), "view.sortOrder", json!({"dir": dir}));
    }
    ui.separator();
    let inbox = app.s.current_account().and_then(|a| app.s.mailbox.role_folder(a, FolderRole::Inbox));
    let focused = inbox.is_some_and(|i| queries::view_settings(&app.s, i).focused_inbox);
    check_cmd(ui, app, focused, "Sho&w Focused Inbox", "view.focusedInbox", json!({}));
}

/// Context menu of a column header (column layout): the "By Date ▾" menu, then the column's own commands.
pub fn column_header_menu(ui: &mut Ui, app: &mut App, column: &str) {
    arrange_menu(ui, app);
    ui.separator();
    cmd(ui, app, "table", "A&dd Columns...", "view.addColumns", json!({}));
    cmd(ui, app, "settings", "&Format Columns...", "view.formatColumns", json!({}));
    let shown = mailcraft_engine::cmd::view::mail::columns(&app.s, app.s.selection.folder);
    let removable = shown.len() > 1 && shown.iter().any(|c| c.id == column);
    cmd_if(ui, app, "minus", "Remo&ve This Column", removable, "view.columns", json!({"remove": column}));
    let sized = mailcraft_engine::cmd::view::mail::mail_view(&app.s, app.s.selection.folder)
        .widths
        .contains_key(column);
    cmd_if(ui, app, "", "Automatic W&idth", sized, "view.columns", json!({"widths": {column: null}}));
    ui.separator();
    cmd(ui, app, "settings", "View &Settings...", "view.viewSettings", json!({}));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use mailcraft_engine::sample::sample_session;

    #[test]
    fn account_root_add_to_favorites_adds_its_inbox() {
        let mut app = App::new(sample_session());
        let account = app.s.mailbox.accounts.first().map(|a| a.id).unwrap();
        let inbox = app.s.mailbox.role_folder(account, FolderRole::Inbox).unwrap();
        app.run("folder.removeFavorite", json!({"folder": inbox.0})).unwrap();
        assert!(app.s.mailbox.folder(inbox).unwrap().favorite_pos.is_none());
        let mut h = Harness::builder()
            .with_size(vec2(300.0, 400.0))
            .build_ui_state(move |ui, app: &mut App| account_menu(ui, app, account.0), app);
        h.run();
        h.get_by_label("Add to Favorites").click();
        h.run();
        assert!(h.state().s.mailbox.folder(inbox).unwrap().favorite_pos.is_some());
    }

    /// A window with one row whose context menu is `menu`, right-clicked open.
    fn menu_harness(app: App, menu: impl Fn(&mut Ui, &mut App) + 'static) -> Harness<'static, App> {
        let mut h = Harness::builder().with_size(vec2(900.0, 1000.0)).build_ui_state(
            move |ui, app: &mut App| {
                ui.add_space(10.0);
                let r = ui.add(egui::Label::new("Menu target").sense(egui::Sense::click()));
                r.context_menu(|ui| menu(ui, app));
            },
            app,
        );
        h.run();
        h.get_by_label("Menu target").click_secondary();
        h.run();
        h
    }

    fn press(h: &mut Harness<'_, App>, key: egui::Key) {
        h.key_press(key);
        h.run();
    }

    fn shown(h: &Harness<'_, App>, label: &str) -> bool {
        h.query_by_label(label).is_some()
    }

    /// The first Inbox message of the sample, selected, marked read and without a flag.
    fn one_message(app: &mut App) -> ItemId {
        let inbox = app.s.selection.folder;
        let id = app.s.mailbox.items_in(inbox).first().map(|m| m.id).unwrap();
        app.run("nav.selectItem", json!({"id": id.0})).unwrap();
        if let Some(m) = app.s.mailbox.mail.get_mut(&id) {
            m.read = true;
            m.flag = FlagState::None;
        }
        id
    }

    #[test]
    fn message_menu_access_keys_open_submenus_and_run_entries() {
        let mut app = App::new(sample_session());
        let id = one_message(&mut app);
        let mut h = menu_harness(app, move |ui, app| message_row_menu(ui, app, id.0));
        assert!(shown(&h, "Quick Print"));
        assert!(!shown(&h, "Mark Complete"));
        // U opens Follow Up; T there flags the message for today and closes the menus.
        press(&mut h, egui::Key::U);
        assert!(shown(&h, "Mark Complete"), "Follow Up opened by its key");
        press(&mut h, egui::Key::T);
        assert!(matches!(h.state().s.mailbox.mail[&id].flag, FlagState::Flagged { .. }));
        assert!(!shown(&h, "Quick Print"), "the menu closed");

        // N: Mark as Unread (the message is read).
        h.get_by_label("Menu target").click_secondary();
        h.run();
        press(&mut h, egui::Key::N);
        assert!(!h.state().s.mailbox.mail[&id].read);
    }

    #[test]
    fn block_has_no_access_key_on_the_message_menu() {
        // Regression: Block was given B, which the reference notes do not show for this menu.
        let mut app = App::new(sample_session());
        let id = one_message(&mut app);
        let mut h = menu_harness(app, move |ui, app| message_row_menu(ui, app, id.0));
        assert!(h.query_by_label_contains("Block").is_some());
        press(&mut h, egui::Key::B);
        assert!(!shown(&h, "Block Sender"), "B opens nothing");
        assert!(shown(&h, "Quick Print"), "the menu stays open");
    }

    #[test]
    fn a_shared_key_moves_the_focus_and_enter_runs_the_entry() {
        let mut app = App::new(sample_session());
        let id = one_message(&mut app);
        let mut h = menu_harness(app, move |ui, app| message_row_menu(ui, app, id.0));
        // R is Reply and Report: the first press focuses Reply, the second Report; Enter opens Report.
        press(&mut h, egui::Key::R);
        assert!(h.get_by_label("Reply").is_focused());
        assert_eq!(h.state().s.composes.len(), 0, "a shared key runs nothing by itself");
        press(&mut h, egui::Key::R);
        assert!(h.get_by_label_contains("Report").is_focused());
        press(&mut h, egui::Key::Enter);
        assert!(shown(&h, "Report Junk"), "Enter opened the Report submenu");
    }

    #[test]
    fn reply_is_greyed_out_for_several_messages() {
        let mut app = App::new(sample_session());
        let inbox = app.s.selection.folder;
        let ids: Vec<u64> = app.s.mailbox.items_in(inbox).iter().take(2).map(|m| m.id.0).collect();
        app.run("nav.selectItem", json!({"ids": ids})).unwrap();
        let first = ids[0];
        let h = menu_harness(app, move |ui, app| message_row_menu(ui, app, first));
        assert!(h.get_by_label("Reply").accesskit_node().is_disabled());
        assert!(h.get_by_label("Reply All").accesskit_node().is_disabled());
        assert!(!h.get_by_label("Forward").accesskit_node().is_disabled());
    }

    #[test]
    fn group_header_follow_up_greys_out_custom_and_reminder() {
        let app = App::new(sample_session());
        let label = queries::group_labels(&app.s).into_iter().next().unwrap();
        let mut h = menu_harness(app, move |ui, app| group_header_menu(ui, app, &label));
        press(&mut h, egui::Key::U);
        assert!(h.get_by_label("Custom...").accesskit_node().is_disabled());
        assert!(h.get_by_label("Add Reminder...").accesskit_node().is_disabled());
        assert!(!h.get_by_label("Today").accesskit_node().is_disabled());
        // C is Custom...'s key; greyed out, it does nothing.
        press(&mut h, egui::Key::C);
        assert!(h.state().ui.dialogs.is_empty(), "{:?}", h.state().ui.dialogs);
    }

    #[test]
    fn group_header_keys_collapse_the_groups() {
        let app = App::new(sample_session());
        let label = queries::group_labels(&app.s).into_iter().next().unwrap();
        let mut h = menu_harness(app, move |ui, app| group_header_menu(ui, app, &label));
        press(&mut h, egui::Key::G);
        let folder = h.state().s.selection.folder;
        assert!(!h.state().s.view.collapsed_groups.get(&folder).is_none_or(Vec::is_empty));
    }

    #[test]
    fn by_date_menu_takes_alt_keys_for_arrangements_and_letters_for_the_rest() {
        let app = App::new(sample_session());
        let mut h = menu_harness(app, arrange_menu);
        h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::F);
        h.run();
        let folder = h.state().s.selection.folder;
        assert_eq!(queries::view_settings(&h.state().s, folder).arrange_by, ArrangeBy::From);
        // R alone: Unread Mail (not an arrangement).
        h.get_by_label("Menu target").click_secondary();
        h.run();
        press(&mut h, egui::Key::R);
        assert_eq!(queries::view_settings(&h.state().s, folder).filter, ListFilter::Unread);
        // Every arrangement's Alt key is in its label.
        for a in ArrangeBy::ALL {
            assert!(menu_keys::find_key(a.label(), arrange_key(a)).is_some(), "{a:?}");
        }
    }

    #[test]
    fn folder_and_status_bar_menus_have_access_keys() {
        let app = App::new(sample_session());
        let inbox = app.s.selection.folder;
        let was_favorite = app.s.mailbox.folder(inbox).unwrap().favorite_pos.is_some();
        let mut h = menu_harness(app, move |ui, app| folder_menu(ui, app, inbox));
        press(&mut h, egui::Key::F);
        assert_ne!(h.state().s.mailbox.folder(inbox).unwrap().favorite_pos.is_some(), was_favorite);

        let app = App::new(sample_session());
        let mut h = menu_harness(app, status_bar_menu);
        assert!(mailcraft_engine::cmd::view::mail::status_item_shown(&h.state().s, "viewShortcuts"));
        press(&mut h, egui::Key::V);
        assert!(!mailcraft_engine::cmd::view::mail::status_item_shown(&h.state().s, "viewShortcuts"));
    }

    #[test]
    fn delete_in_deleted_items_asks_before_removing_for_good() {
        let mut app = App::new(sample_session());
        let id = one_message(&mut app);
        app.run("mail.delete", json!({"ids": [id.0]})).unwrap();
        assert!(app.ui.dialogs.is_empty(), "moving to Deleted Items asks nothing");
        let deleted = app.s.mailbox.mail[&id].folder;
        app.run("nav.selectFolder", json!({"folder": deleted.0})).unwrap();
        app.run("nav.selectItem", json!({"id": id.0})).unwrap();
        let mut h = menu_harness(app, move |ui, app| message_row_menu(ui, app, id.0));
        press(&mut h, egui::Key::D);
        assert!(h.state().s.mailbox.mail.contains_key(&id), "not removed yet");
        let (name, p) = h.state().ui.dialogs.last().cloned().unwrap();
        assert_eq!(name, "confirm");
        assert!(p["text"].as_str().unwrap().contains("permanently deleted"), "{p}");
        // Yes runs Delete with the answer.
        let (cmd, params) = (p["command"].as_str().unwrap().to_owned(), p["params"].clone());
        h.state_mut().run(&cmd, params);
        assert!(!h.state().s.mailbox.mail.contains_key(&id));
    }

    #[test]
    fn names_with_ampersands_show_as_typed() {
        let mut app = App::new(sample_session());
        let id = one_message(&mut app);
        app.run("quicksteps.new", json!({"name": "R&D Review", "actions": ["MarkRead"]})).unwrap();
        let mut h = menu_harness(app, move |ui, app| message_row_menu(ui, app, id.0));
        press(&mut h, egui::Key::Q);
        // Q is Quick Print and Quick Steps: focus Quick Print, then Quick Steps; Enter opens it.
        press(&mut h, egui::Key::Q);
        press(&mut h, egui::Key::Enter);
        assert!(shown(&h, "R&D Review"), "the step's name keeps its &");
    }
}
