//! File backstage (main and compose) — backstage/dialogs area.
//!
//! A full-window page below the title bar (plan/reference/backstage.md): a left navigation column (Back, Info,
//! Open & Export, Save As, Save Attachments, Print, then Account, Options, Exit at the bottom; the compose
//! variant adds Save and Close) and the selected page on the right. Pages act only through `app.run`.
//!
//! Drawn by [`backstage`] when `app.ui.backstage` is set: the main window's in the root viewport, a compose
//! window's in that compose's viewport (or in the root viewport for an inline response). Page ids: `info`,
//! `openExport`, `save`, `saveAs`, `saveAttachments`, `print`, `close`, `account`, `options`, `exit`.

use egui::{
    Align, Color32, Context, Id, Layout, Order, Rect, RichText, Sense, Ui, Vec2, WidgetInfo, WidgetType,
};
use serde_json::{Value, json};

use crate::dialogs::files;
use crate::theme::size;
use crate::{App, Surface};
use mailcraft_model::{FolderRole, ItemId, MailItem};

pub fn backstage(ctx: &egui::Context, app: &mut App) {
    let Some((surface, page)) = app.ui.backstage.clone() else { return };
    let root = ctx.viewport_id() == egui::ViewportId::ROOT;
    let here = match surface {
        Surface::Main => root,
        Surface::Compose(cid) => match app.s.composes.get(&cid) {
            Some(c) if c.inline => root,
            // A compose window draws its own backstage ([`compose_backstage`]), as an OS window or as an
            // embedded one.
            Some(_) => return,
            None => {
                app.ui.backstage = None;
                return;
            }
        },
        // A read window that hosts its own backstage ([`window_backstage`]) drew it already this frame;
        // otherwise the main window shows it (read windows have no other place for File).
        Surface::Read(_) => root && !take_window_drawn(ctx),
    };
    if !here {
        return;
    }
    if app.ui.dialogs.is_empty() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        app.ui.backstage = None;
        return;
    }
    let mut rect = ctx.content_rect();
    // Drawn in the main window: its title bar stays visible above the page.
    if root {
        rect.min.y = (rect.min.y + size::TITLE_BAR).min(rect.max.y);
    }
    egui::Area::new(Id::new(("mc-backstage", viewport_salt(ctx))))
        .order(Order::Middle)
        .fixed_pos(rect.min)
        .interactable(true)
        .show(ctx, |ui| {
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            // Swallow clicks so nothing below the page reacts.
            let _ = ui.interact(rect, Id::new("mc-backstage-blocker"), Sense::click());
            ui.painter().rect_filled(rect, 0.0, app.theme.pane);
            show_in(ui, app, surface, &page, rect);
        });
}

/// The backstage of compose window `cid`, drawn over the whole of `ui` (the compose window's content).
/// Escape (with no dialog open) goes back to the message.
pub fn compose_backstage(ui: &mut Ui, app: &mut App, cid: u64) {
    window_backstage(ui, app, Surface::Compose(cid));
}

/// The backstage of a compose or read window, drawn over the whole of `ui` (the window's content) when
/// `app.ui.backstage` belongs to `surface`. Read windows that call this keep their backstage in their own
/// window; [`backstage`] then skips it.
pub fn window_backstage(ui: &mut Ui, app: &mut App, surface: Surface) {
    let Some((s, page)) = app.ui.backstage.clone() else { return };
    if s != surface || surface == Surface::Main {
        return;
    }
    if matches!(surface, Surface::Read(_)) {
        ui.ctx().data_mut(|d| d.insert_temp(window_drawn_id(), true));
    }
    if app.ui.dialogs.is_empty()
        && ui.ctx().input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        app.ui.backstage = None;
        return;
    }
    let rect = ui.max_rect();
    let _ = ui.interact(rect, Id::new(("mc-backstage-blocker", surface.key())), Sense::click());
    ui.painter().rect_filled(rect, 0.0, app.theme.pane);
    let mut child =
        ui.new_child(egui::UiBuilder::new().id_salt(("mc-window-backstage", surface.key())).max_rect(rect));
    show_in(&mut child, app, surface, &page, rect);
}

fn window_drawn_id() -> Id {
    Id::new("mc-backstage-window-drawn")
}

/// Whether a read window drew the backstage since the last call (clears the mark).
fn take_window_drawn(ctx: &Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(window_drawn_id())).unwrap_or(false)
}

fn viewport_salt(ctx: &Context) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ctx.viewport_id().hash(&mut h);
    h.finish()
}

/// Nav entries: (page id, label, icon, keytip letter).
fn nav_items(surface: Surface) -> (Vec<(&'static str, &'static str)>, Vec<(&'static str, &'static str)>) {
    match surface {
        Surface::Compose(_) => (
            vec![
                ("info", "Info"),
                ("openExport", "Open & Export"),
                ("save", "Save"),
                ("saveAs", "Save As"),
                ("saveAttachments", "Save Attachments"),
                ("print", "Print"),
                ("close", "Close"),
            ],
            vec![("account", "Account"), ("options", "Options")],
        ),
        _ => (
            vec![
                ("info", "Info"),
                ("openExport", "Open & Export"),
                ("saveAs", "Save As"),
                ("saveAttachments", "Save Attachments"),
                ("print", "Print"),
            ],
            vec![("account", "Account"), ("options", "Options"), ("exit", "Exit")],
        ),
    }
}

/// The backstage drawn into `rect` of `ui` (for hosts that place it themselves).
pub fn show_in(ui: &mut Ui, app: &mut App, surface: Surface, page: &str, rect: Rect) {
    let nav_w = 158.0;
    let nav = Rect::from_min_size(rect.min, Vec2::new(nav_w, rect.height()));
    let body = Rect::from_min_max(egui::pos2(rect.min.x + nav_w, rect.min.y), rect.max);
    let nav_fill = app.theme.chrome;
    ui.painter().rect_filled(nav, 0.0, nav_fill);
    let mut nav_ui = ui.new_child(egui::UiBuilder::new().max_rect(nav).layout(Layout::top_down(Align::Min)));
    let clicked = nav_column(&mut nav_ui, app, surface, page, nav);
    if let Some(p) = clicked {
        nav_action(ui.ctx(), app, surface, p);
    }
    let Some((_, page)) = app.ui.backstage.clone() else { return };
    let mut body_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(body.shrink2(Vec2::new(40.0, 20.0)))
            .layout(Layout::top_down(Align::Min)),
    );
    crate::dialogs::form_visuals(&mut body_ui, &app.theme);
    egui::ScrollArea::vertical().id_salt("mc-backstage-page").auto_shrink([false, false]).show(
        &mut body_ui,
        |ui| {
            page_ui(ui, app, surface, &page);
        },
    );
}

/// Room the backstage nav keeps below its last entry (Exit).
const NAV_BOTTOM_PAD: f32 = 20.0;

fn nav_column(ui: &mut Ui, app: &mut App, surface: Surface, page: &str, rect: Rect) -> Option<&'static str> {
    let text = app.theme.chrome_text;
    let w = rect.width();
    let mut clicked = None;
    // Back
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 45.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Back"));
    if resp.hovered() {
        ui.painter().rect_filled(r, 0.0, Color32::from_white_alpha(24));
    }
    let icon = Rect::from_center_size(egui::pos2(r.left() + 26.0, r.center().y), Vec2::splat(22.0));
    app.icons.paint(ui, icon, "back", text);
    if resp.on_hover_text("Back").clicked() {
        clicked = Some("back");
    }
    let (top, bottom) = nav_items(surface);
    let has_att = has_attachments(app, surface);
    for (id, label) in top {
        let enabled = id != "saveAttachments" || has_att;
        if nav_item(ui, app, label, page == id, enabled, 40.0, w) {
            clicked = Some(id);
        }
    }
    let used = ui.min_rect().height();
    // The entries plus the spacing before each (left out, it pushed Exit onto the window's edge).
    let bottom_h = 56.0
        + 40.0 * (bottom.len().saturating_sub(1)) as f32
        + ui.spacing().item_spacing.y * bottom.len() as f32;
    let gap = (rect.height() - used - bottom_h - NAV_BOTTOM_PAD).max(8.0);
    ui.add_space(gap);
    for (i, (id, label)) in bottom.into_iter().enumerate() {
        let h = if i == 0 { 56.0 } else { 40.0 };
        if nav_item(ui, app, label, page == id, true, h, w) {
            clicked = Some(id);
        }
    }
    clicked
}

fn nav_item(ui: &mut Ui, app: &App, label: &str, selected: bool, enabled: bool, h: f32, w: f32) -> bool {
    let (r, resp) =
        ui.allocate_exact_size(Vec2::new(w, h), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, enabled, selected, label));
    if selected {
        ui.painter().rect_filled(r, 0.0, Color32::from_white_alpha(40));
    } else if resp.hovered() && enabled {
        ui.painter().rect_filled(r, 0.0, Color32::from_white_alpha(20));
    }
    let color = if enabled { app.theme.chrome_text } else { app.theme.chrome_text.gamma_multiply(0.45) };
    ui.painter().text(
        egui::pos2(r.left() + 20.0, r.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(14.0),
        color,
    );
    enabled && resp.clicked()
}

fn nav_action(ctx: &Context, app: &mut App, surface: Surface, id: &str) {
    match id {
        "back" => app.ui.backstage = None,
        "options" => {
            app.run("file.options", json!({}));
            crate::dialogs::take_notices(app);
        }
        "exit" => {
            app.ui.backstage = None;
            app.run("file.exit", json!({}));
        }
        "close" => {
            if let Surface::Compose(c) = surface {
                app.ui.backstage = None;
                // Unsaved changes: the compose window's own save prompt (Yes / No / Cancel).
                if app.s.composes.get(&c).is_some_and(|x| x.dirty) {
                    crate::compose::ask_save_changes(ctx, c);
                } else {
                    app.run("compose.close", json!({"compose": c, "save": false}));
                }
            }
        }
        "save" => {
            if let Surface::Compose(c) = surface {
                app.run("compose.saveDraft", json!({"compose": c}));
                app.ui.backstage = None;
            }
        }
        other => app.ui.backstage = Some((surface, other.to_owned())),
    }
}

fn has_attachments(app: &App, surface: Surface) -> bool {
    match surface {
        Surface::Compose(c) => app.s.composes.get(&c).is_some_and(|c| !c.attachments.is_empty()),
        _ => selected_items(app, surface).iter().any(|m| m.has_attachments()),
    }
}

/// The messages a page acts on: a read window's own message, else the main window's selection (or focus).
fn selected_items(app: &App, surface: Surface) -> Vec<&MailItem> {
    let mut ids: Vec<ItemId> = match surface {
        Surface::Read(id) => vec![ItemId(id)],
        _ => app.s.selection.items.clone(),
    };
    if ids.is_empty()
        && let Some(f) = app.s.selection.focus
    {
        ids.push(f);
    }
    ids.iter().filter_map(|i| app.s.mailbox.mail.get(*i)).collect()
}

fn selected_ids(app: &App, surface: Surface) -> Vec<u64> {
    selected_items(app, surface).iter().map(|m| m.id.0).collect()
}

// ---------------------------------------------------------------------------------------------------------
// Pages

/// Per-page UI state kept across frames.
#[derive(Clone, Debug, Default)]
struct PageState {
    account: Option<u32>,
    path: String,
    action: usize,
    message: String,
    /// Save As type (index into `files::SAVE_TYPES`).
    kind: usize,
}

fn state(ctx: &Context) -> PageState {
    ctx.data_mut(|d| d.get_temp::<PageState>(Id::new("mc-backstage-state"))).unwrap_or_default()
}

fn set_state(ctx: &Context, st: PageState) {
    ctx.data_mut(|d| d.insert_temp(Id::new("mc-backstage-state"), st));
}

fn page_ui(ui: &mut Ui, app: &mut App, surface: Surface, page: &str) {
    let ctx = ui.ctx().clone();
    let mut st = state(&ctx);
    match page {
        "openExport" => open_export(ui, app, &mut st),
        "saveAs" => save_as(ui, app, surface, &mut st),
        "saveAttachments" => save_attachments(ui, app, surface, &mut st),
        "print" => print(ui, app, surface, &mut st),
        "account" => account(ui, app),
        "options" => {
            title(ui, app, "Options");
            muted(ui, app, "Settings for mail, calendar, people, search and more.");
            if tile(ui, app, "settings", "Options", "Open the Options window.") {
                app.run("file.options", json!({}));
            }
        }
        _ => match surface {
            Surface::Compose(c) => compose_info(ui, app, c),
            _ => info(ui, app, &mut st),
        },
    }
    set_state(&ctx, st);
}

fn title(ui: &mut Ui, app: &App, text: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(text).size(28.0).color(app.theme.text));
    ui.add_space(10.0);
}

fn muted(ui: &mut Ui, app: &App, text: &str) {
    ui.label(RichText::new(text).color(app.theme.text_muted));
}

fn rule(ui: &mut Ui, app: &App) {
    ui.add_space(6.0);
    let w = ui.available_width().min(760.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, app.theme.separator);
    ui.add_space(6.0);
}

/// A large square button (icon over label) with a heading and description beside it. Returns a click.
fn tile(ui: &mut Ui, app: &mut App, icon: &str, label: &str, desc: &str) -> bool {
    tile_with(ui, app, icon, label, desc, true, |_, _| {})
}

fn tile_with(
    ui: &mut Ui,
    app: &mut App,
    icon: &str,
    label: &str,
    desc: &str,
    enabled: bool,
    extra: impl FnOnce(&mut Ui, &mut App),
) -> bool {
    let r = tile_full(ui, app, icon, label, desc, enabled, extra);
    enabled && r.clicked()
}

/// A tile that opens a menu (Account Settings ▾): the tile's response, for [`egui::Popup::menu`].
fn tile_full(
    ui: &mut Ui,
    app: &mut App,
    icon: &str,
    label: &str,
    desc: &str,
    enabled: bool,
    extra: impl FnOnce(&mut Ui, &mut App),
) -> egui::Response {
    let mut response: Option<egui::Response> = None;
    ui.horizontal_top(|ui| {
        let (r, resp) = ui.allocate_exact_size(
            Vec2::new(97.0, 86.0),
            if enabled { Sense::click() } else { Sense::hover() },
        );
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
        let stroke = if resp.hovered() && enabled { app.theme.accent } else { app.theme.separator };
        ui.painter().rect(
            r,
            4.0,
            if resp.hovered() && enabled { app.theme.hover } else { app.theme.pane },
            egui::Stroke::new(1.0, stroke),
            egui::StrokeKind::Inside,
        );
        let tint = if enabled { app.theme.accent } else { app.theme.text_muted };
        app.icons.paint(
            ui,
            Rect::from_center_size(egui::pos2(r.center().x, r.top() + 30.0), Vec2::splat(30.0)),
            icon,
            tint,
        );
        // The caption wraps inside the square (two lines at most fit under the icon).
        let color = if enabled { app.theme.text } else { app.theme.text_muted };
        let mut job = egui::text::LayoutJob::simple(
            label.trim_end_matches("...").to_owned(),
            egui::FontId::proportional(12.0),
            color,
            r.width() - 10.0,
        );
        job.halign = Align::Center;
        job.wrap.max_rows = 2;
        let galley = ui.painter().layout_job(job);
        ui.painter().with_clip_rect(r).galley(egui::pos2(r.center().x, r.top() + 50.0), galley, color);
        response = Some(resp);
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.set_max_width(460.0);
            ui.add_space(8.0);
            ui.label(RichText::new(label.trim_end_matches("...")).strong().size(15.0));
            ui.label(RichText::new(desc).color(app.theme.text_muted));
            extra(ui, app);
        });
    });
    rule(ui, app);
    response.unwrap_or_else(|| ui.label(""))
}

fn info(ui: &mut Ui, app: &mut App, st: &mut PageState) {
    title(ui, app, "Account Information");
    let accounts = app.s.mailbox.accounts.clone();
    let current = st
        .account
        .filter(|a| accounts.iter().any(|x| x.id.0 == *a))
        .or_else(|| app.s.current_account().map(|a| a.0))
        .or_else(|| accounts.first().map(|a| a.id.0));
    st.account = current;
    let acct = accounts.iter().find(|a| Some(a.id.0) == current).cloned();
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
        app.icons.paint(ui, r, "account", app.theme.accent);
        let label = acct
            .as_ref()
            .map(|a| format!("{}  ({})", crate::dialogs::account_label(a), a.display_name))
            .unwrap_or_default();
        egui::ComboBox::from_id_salt("mc-bs-account").selected_text(label).width(420.0).show_ui(ui, |ui| {
            for a in &accounts {
                if ui.selectable_label(Some(a.id.0) == current, crate::dialogs::account_label(a)).clicked() {
                    st.account = Some(a.id.0);
                }
            }
        });
    });
    if let Some(a) = &acct {
        muted(
            ui,
            app,
            &format!(
                "{} · {}",
                kind_label(&app.s, a),
                if a.can_send() { "can send mail" } else { "stores mail on this computer" }
            ),
        );
    }
    ui.add_space(6.0);
    if ui.add(egui::Button::new("+ Add Account").min_size(Vec2::new(109.0, 28.0))).clicked() {
        crate::dialogs::open(app, "addAccount", json!({}));
    }
    rule(ui, app);
    account_settings_menu(ui, app, acct.as_ref());
    tile_with(
        ui,
        app,
        "calendar-reminder",
        "Automatic Replies...",
        "Let people know you're away. Needs a mail server that sends replies for you.",
        false,
        |_, _| {},
    );
    let (used, items, deleted) = mailbox_size(app, current);
    let quota = acct.as_ref().and_then(|a| mailcraft_engine::sync::account_quota(&app.s, a.id));
    let mut tools: Option<&str> = None;
    if tile_with(
        ui,
        app,
        "clean-up",
        "Tools",
        "Keep the mailbox small by emptying Deleted Items and archiving.",
        true,
        |ui, app| {
            ui.label(format!("{} in {} items", files_size(used), items));
            size_bar(ui, app, quota);
            muted(ui, app, &format!("Deleted Items: {}", files_size(deleted)));
            ui.horizontal(|ui| {
                if ui.link("Mailbox Cleanup...").clicked() {
                    tools = Some("cleanup");
                }
                if ui.link("Empty Deleted Items Folder").clicked() {
                    tools = Some("empty");
                }
                if ui.link("Clean Up Old Items...").clicked() {
                    tools = Some("archive");
                }
            });
        },
    ) {
        tools = Some("cleanup");
    }
    match tools {
        Some("cleanup") => {
            app.run("file.cleanupTools", json!({}));
        }
        Some("empty") => crate::dialogs::confirm(
            app,
            "Empty Deleted Items",
            "Permanently delete everything in Deleted Items?",
            "folder.emptyDeleted",
            json!({}),
        ),
        Some(_) => {
            app.run("file.archive", json!({}));
        }
        None => {}
    }
    let rules = app.s.mailbox.rules.len();
    if tile(
        ui,
        app,
        "rules",
        "Manage Rules & Alerts...",
        &format!("Organize incoming mail with rules. {rules} rule(s) set up."),
    ) {
        app.run("file.manageRules", json!({}));
    }
}

/// Account Settings ▾: Account Settings..., Account Name and Sync Settings, Server Settings, Test Account
/// Settings... (email accounts), and the profile entries MailCraft does not have (shown disabled).
fn account_settings_menu(ui: &mut Ui, app: &mut App, acct: Option<&mailcraft_model::Account>) {
    let r = tile_full(
        ui,
        app,
        "account",
        "Account Settings",
        "Change the settings for this account or set up more connections.",
        true,
        |_, _| {},
    );
    let current = acct.map(|a| a.id.0);
    let email = acct.filter(|a| a.kind != mailcraft_model::AccountKind::Local).map(|a| a.id.0);
    let mut pick: Option<&str> = None;
    egui::Popup::menu(&r).show(|ui| {
        ui.set_min_width(240.0);
        if ui.button("Account Settings...").clicked() {
            pick = Some("settings");
        }
        if ui.add_enabled(email.is_some(), egui::Button::new("Account Name and Sync Settings")).clicked() {
            pick = Some("change");
        }
        if ui.add_enabled(email.is_some(), egui::Button::new("Server Settings")).clicked() {
            pick = Some("change");
        }
        if ui.add_enabled(email.is_some(), egui::Button::new("Test Account Settings...")).clicked() {
            pick = Some("test");
        }
        ui.separator();
        ui.add_enabled(false, egui::Button::new("Change Profile"))
            .on_disabled_hover_text("MailCraft keeps one profile per data folder (--profile).");
        ui.add_enabled(false, egui::Button::new("Manage Profiles"))
            .on_disabled_hover_text("MailCraft keeps one profile per data folder (--profile).");
    });
    match pick {
        Some("settings") => {
            app.run("file.accountSettings", json!({"account": current}));
        }
        Some("change") => {
            if let Some(id) = email {
                crate::dialogs::open(app, "addAccount", json!({"change": id}));
            }
        }
        Some(_) => {
            if let Some(id) = email {
                crate::dialogs::open(app, "testAccount", json!({"account": id}));
            }
        }
        None => {}
    }
}

/// Account type shown under the account picker (POP3 accounts are stored as IMAP plus a POP3 option).
fn kind_label(s: &mailcraft_engine::Session, a: &mailcraft_model::Account) -> &'static str {
    if mailcraft_engine::accounts::kind_label(s, a) == "pop3" {
        return "POP/SMTP";
    }
    match a.kind {
        mailcraft_model::AccountKind::Local => "Local data file",
        mailcraft_model::AccountKind::Imap => "IMAP/SMTP",
        mailcraft_model::AccountKind::Jmap => "JMAP",
        mailcraft_model::AccountKind::Exchange => "Exchange",
        mailcraft_model::AccountKind::Graph => "Microsoft 365 (Graph)",
    }
}

fn files_size(b: u64) -> String {
    mailcraft_engine::queries::size_text(b)
}

/// (bytes used, items, bytes in Deleted Items) for an account.
fn mailbox_size(app: &App, account: Option<u32>) -> (u64, usize, u64) {
    let mb = &app.s.mailbox;
    let Some(acct) = account.and_then(|a| mb.accounts.iter().find(|x| x.id.0 == a)).map(|a| a.id) else {
        return (0, 0, 0);
    };
    let deleted: Vec<mailcraft_model::FolderId> =
        mb.role_folder(acct, FolderRole::DeletedItems).map(|d| mb.subtree(d)).unwrap_or_default();
    let mut used = 0u64;
    let mut items = 0usize;
    let mut del = 0u64;
    for m in mb.mail.items() {
        if mb.folder(m.folder).is_some_and(|f| f.account == acct) {
            used = used.saturating_add(m.size);
            items = items.saturating_add(1);
            if deleted.contains(&m.folder) {
                del = del.saturating_add(m.size);
            }
        }
    }
    (used, items, del)
}

/// Width of the Tools tile's storage bar.
const SIZE_BAR_W: f32 = 300.0;

/// Share of the storage limit in use (0 to 1), or `None` when the server reported no size limit: the bar is then
/// left out rather than drawn full (a full bar read as "mailbox full" for a mailbox of a few hundred KB).
pub(crate) fn storage_fill(quota: Option<&mailcraft_engine::MailboxQuota>) -> Option<f32> {
    let q = quota?;
    if q.limit == 0 {
        return None;
    }
    Some((q.used as f64 / q.limit as f64).clamp(0.0, 1.0) as f32)
}

/// The storage bar: a visible track, filled in proportion to the server's limit, and the free space; nothing
/// when no limit is known.
fn size_bar(ui: &mut Ui, app: &App, quota: Option<mailcraft_engine::MailboxQuota>) {
    let Some(frac) = storage_fill(quota.as_ref()) else { return };
    let (r, resp) = ui.allocate_exact_size(Vec2::new(SIZE_BAR_W, 8.0), Sense::hover());
    let limit = quota.map_or(0, |q| q.limit);
    let used = quota.map_or(0, |q| q.used);
    resp.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::ProgressIndicator,
            true,
            format!("{:.0}% of the mailbox limit used", frac * 100.0),
        )
    });
    ui.painter().rect_filled(r, 3.0, app.theme.hover);
    ui.painter().rect_stroke(r, 3.0, egui::Stroke::new(1.0, app.theme.separator), egui::StrokeKind::Inside);
    if frac > 0.0 {
        let fill = Rect::from_min_size(r.min, Vec2::new((SIZE_BAR_W * frac).max(3.0), 8.0));
        ui.painter().rect_filled(fill, 3.0, app.theme.accent);
    }
    muted(ui, app, &format!("{} free of {}", files_size(limit.saturating_sub(used)), files_size(limit)));
}

fn compose_info(ui: &mut Ui, app: &mut App, cid: u64) {
    let Some(c) = app.s.composes.get(&cid).cloned() else { return };
    // The page title is the message subject (backstage.md, compose window: "Untitled" until one is typed).
    title(ui, app, if c.subject.trim().is_empty() { "Untitled" } else { c.subject.trim() });
    let encrypt_desc = if c.encrypt {
        "This message will be sent encrypted. Click to send it without encryption."
    } else {
        "Restrict who can read this message by encrypting it for its recipients."
    };
    if tile(ui, app, "private", "Encrypt", encrypt_desc) {
        app.run("compose.encrypt", json!({"compose": cid}));
    }
    let draft = c.draft.and_then(|d| app.s.mailbox.mail.get(d)).map(|m| (m.id.0, m.folder, m.size));
    let folder = draft.map_or_else(|| "Not yet saved".to_owned(), |(_, f, _)| app.s.mailbox.folder_path(f));
    let moved = tile_with(
        ui,
        app,
        "move",
        "Move to Folder",
        "Move or copy the saved message to a different folder.",
        draft.is_some(),
        |ui, app| muted(ui, app, &format!("Current folder:  {folder}")),
    );
    if moved && let Some((id, _, _)) = draft {
        crate::dialogs::open(app, "moveToFolder", json!({"ids": [id]}));
    }
    let size = match draft {
        Some((_, _, bytes)) if !c.dirty => files_size(bytes),
        Some(_) => "Changed since it was last saved".to_owned(),
        None => "Not yet saved".to_owned(),
    };
    if tile_with(
        ui,
        app,
        "settings",
        "Properties",
        "Importance, sensitivity, voting, receipts and delivery options for this message.",
        true,
        |ui, app| muted(ui, app, &format!("Size:  {size}")),
    ) {
        app.run("compose.messageOptions", json!({"compose": cid}));
    }
}

fn open_export(ui: &mut Ui, app: &mut App, st: &mut PageState) {
    title(ui, app, "Open & Export");
    if tile(
        ui,
        app,
        "calendar",
        "Open Calendar",
        "Add a calendar from the internet, a room list or another person.",
    ) {
        app.run("calendar.addCalendar", json!({}));
    }
    if tile(
        ui,
        app,
        "folder-open",
        "New Data File...",
        "Add another data file to keep mail separately, for example an archive.",
    ) {
        app.run("file.newDataFile", json!({}));
    }
    if tile(
        ui,
        app,
        "folder-open",
        "Open Data File...",
        "Open a data file you removed earlier, or a MailCraft data file from another profile.",
    ) {
        app.run("file.openDataFile", json!({}));
    }
    if tile(
        ui,
        app,
        "sync",
        "Import/Export",
        "Import mail (EML, MSG, MBOX, Maildir), calendars (iCalendar) or contacts (vCard, CSV), or export them.",
    ) {
        app.run("file.openExport", json!({}));
    }
    tile_with(
        ui,
        app,
        "people",
        "Other User's Folder...",
        "Open a folder another person shared with you. Needs a mail server.",
        false,
        |_, _| {},
    );
    ui.add_space(4.0);
    ui.label(RichText::new("Quick import and export").strong().size(15.0));
    let action = files::IE_ACTIONS.get(st.action).copied();
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("mc-bs-ie")
            .selected_text(action.map(|a| a.label).unwrap_or_default())
            .width(300.0)
            .show_ui(ui, |ui| {
                for (i, a) in files::IE_ACTIONS.iter().enumerate() {
                    if ui.selectable_label(st.action == i, a.label).clicked() {
                        st.action = i;
                    }
                }
            });
        let hint = match action {
            Some(a) if a.export() => format!("/path/to/{}", a.suggested),
            Some(a) if a.folder_input() => "/path/to/Maildir".to_owned(),
            Some(a) => format!("/path/to/file.{}", a.filter.1.first().copied().unwrap_or("eml")),
            None => String::new(),
        };
        ui.add(egui::TextEdit::singleline(&mut st.path).hint_text(hint).desired_width(320.0));
        if let Some(a) = action
            && app.files.is_some()
            && ui.button("Browse...").clicked()
        {
            let picked = if a.export() {
                crate::dialogs::browse_save(app, a.label, &a.filters(), &st.path)
            } else if a.folder_input() {
                crate::dialogs::browse_folder(app, a.label, &st.path)
            } else {
                crate::dialogs::browse_open(app, a.label, &a.filters(), &st.path)
            };
            if let Some(p) = picked {
                st.path = p;
            }
        }
    });
    let folder = action.and_then(|a| files::ie_folder(app, &a, Some(app.s.selection.folder.0)));
    muted(ui, app, &format!("Folder: {}", files::folder_label(app, folder)));
    let ready = !st.path.trim().is_empty() && folder.is_some();
    if ui.add_enabled(ready, egui::Button::new("Run")).clicked()
        && let Some(a) = action
    {
        st.message = match app
            .run("file.openExport", json!({"action": a.key, "path": st.path.trim(), "folder": folder}))
        {
            Some(v) => files::ie_result_text(app, &a, &v, st.path.trim(), folder),
            None => app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default(),
        };
    }
    if !st.message.is_empty() {
        muted(ui, app, &st.message);
    }
}

pub(crate) fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".into())
}

fn save_as(ui: &mut Ui, app: &mut App, surface: Surface, st: &mut PageState) {
    title(ui, app, "Save As");
    let ids = match surface {
        Surface::Compose(c) => {
            app.s.composes.get(&c).and_then(|c| c.draft).map(|d| vec![d.0]).unwrap_or_default()
        }
        _ => selected_ids(app, surface),
    };
    let compose = match surface {
        Surface::Compose(c) => Some(c),
        _ => None,
    };
    if ids.is_empty() && compose.is_none() {
        muted(ui, app, "Select a message to save it as a file.");
        return;
    }
    muted(
        ui,
        app,
        "Saves the message as a file: an Internet message (.eml) other mail programs can open, a .msg file, text or HTML.",
    );
    if st.path.is_empty() {
        st.path = home();
    }
    ui.horizontal(|ui| {
        ui.label("Save to (file or folder):");
        ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(420.0));
        if app.files.is_some() && ui.button("Browse...").clicked() {
            let subject = ids
                .first()
                .and_then(|id| app.s.mailbox.mail.get(ItemId(*id)))
                .map(|m| m.subject.clone())
                .unwrap_or_default();
            let ext = files::SAVE_TYPES.get(st.kind).and_then(|t| t.2.first()).copied().unwrap_or("eml");
            let stem: String = subject
                .chars()
                .map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') { c } else { '_' })
                .take(80)
                .collect();
            let stem = if stem.trim().is_empty() { "Message".to_owned() } else { stem.trim().to_owned() };
            let base = std::path::Path::new(st.path.trim());
            let dir = if base.is_dir() {
                base.to_path_buf()
            } else {
                base.parent().map(std::path::Path::to_path_buf).unwrap_or_default()
            };
            let start = dir.join(format!("{stem}.{ext}")).display().to_string();
            if let Some(p) =
                crate::dialogs::browse_save(app, "Save As", &files::save_filters(st.kind), &start)
            {
                st.kind = files::save_type_of(&p);
                st.path = p;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Save as type:");
        let labels: Vec<&str> = files::SAVE_TYPES.iter().map(|t| t.1).collect();
        egui::ComboBox::from_id_salt("mc-bs-saveas-type")
            .selected_text(labels.get(st.kind).copied().unwrap_or_default())
            .width(240.0)
            .show_ui(ui, |ui| {
                for (i, l) in labels.iter().enumerate() {
                    if ui.selectable_label(st.kind == i, *l).clicked() && st.kind != i {
                        st.kind = i;
                        st.path = files::with_save_ext(&st.path, i);
                    }
                }
            });
    });
    if ui.add(egui::Button::new("Save").min_size(Vec2::new(90.0, 28.0))).clicked() {
        let mut ids = ids;
        if let Some(c) = compose {
            app.run("compose.saveDraft", json!({"compose": c}));
            ids = app.s.composes.get(&c).and_then(|c| c.draft).map(|d| vec![d.0]).unwrap_or_default();
        }
        let kind = files::SAVE_TYPES.get(st.kind).map_or("eml", |t| t.0);
        st.message = match app.run("file.saveAs", json!({"path": st.path.trim(), "ids": ids, "type": kind})) {
            Some(v) => format!("Saved to {}.", v.get("path").and_then(Value::as_str).unwrap_or_default()),
            None => app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default(),
        };
    }
    if !st.message.is_empty() {
        muted(ui, app, &st.message);
    }
}

fn save_attachments(ui: &mut Ui, app: &mut App, surface: Surface, st: &mut PageState) {
    title(ui, app, "Save Attachments");
    if matches!(surface, Surface::Compose(_)) {
        muted(ui, app, "Attachments of a message you are writing are already files on this computer.");
        return;
    }
    let items = selected_items(app, surface);
    let files: Vec<String> =
        items.iter().flat_map(|m| m.attachments.iter().map(|a| a.filename.clone())).collect();
    if files.is_empty() {
        muted(ui, app, "The selected message has no attachments.");
        return;
    }
    for f in &files {
        ui.label(format!("•  {f}"));
    }
    if st.path.is_empty() {
        st.path = home();
    }
    ui.horizontal(|ui| {
        ui.label("Save in folder:");
        ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(420.0));
        if app.files.is_some()
            && ui.button("Browse...").clicked()
            && let Some(p) = crate::dialogs::browse_folder(app, "Save Attachments", &st.path)
        {
            st.path = p;
        }
    });
    if ui.add(egui::Button::new("Save All").min_size(Vec2::new(90.0, 28.0))).clicked() {
        let ids: Vec<u64> = items.iter().map(|m| m.id.0).collect();
        st.message = match app.run("file.saveAttachments", json!({"dir": st.path.trim(), "ids": ids})) {
            Some(v) => {
                format!("Saved {} file(s).", v.get("saved").and_then(Value::as_array).map_or(0, Vec::len))
            }
            None => app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default(),
        };
    }
    if !st.message.is_empty() {
        muted(ui, app, &st.message);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Print (plan/reference/backstage.md "PAGE Print"): Print, Which Printer, Print Options..., Settings gallery,
// and the preview with Previous Page / Current Page / Next Page / Actual Size / One Page / Multiple Pages.
// Everything goes through the engine's `print.*` commands; the preview pages are rendered by the engine
// (`print.preview`) and shown as textures.

/// Paper names offered by Page Setup (engine `setup.paper`).
pub const PAPERS: [&str; 4] = ["Letter", "Legal", "A4", "A5"];
/// The built-in printer that writes a PDF file (engine `print::SAVE_AS_PDF`).
pub const SAVE_AS_PDF: &str = "Save as PDF";
/// Header / footer fields (engine `print::FIELDS`).
pub const PRINT_FIELDS: [&str; 5] =
    ["[Page #]", "[Total Pages]", "[Date Printed]", "[Time Printed]", "[User Name]"];

/// How the preview fits pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewZoom {
    #[default]
    OnePage,
    ActualSize,
    MultiplePages,
}

/// Print choices shared by the backstage Print page and the Print / Page Setup dialogs (kept for the session).
#[derive(Clone, Debug)]
pub struct PrintSettings {
    /// Style id ("" = the default for what is printed).
    pub style: String,
    pub printer: String,
    /// Printers offered (fetched once).
    pub printers: Vec<String>,
    /// PDF file for Save as PDF.
    pub path: String,
    pub copies: u32,
    /// Page range ("" = all pages), e.g. "1-3,5".
    pub pages: String,
    /// Index into [`PAPERS`]; `None` = the style default.
    pub paper: Option<usize>,
    /// `None` = the style default (Monthly prints landscape).
    pub landscape: Option<bool>,
    /// Inches: top, bottom, left, right.
    pub margins: [f32; 4],
    pub header: [String; 3],
    pub footer: [String; 3],
    pub new_page_per_item: bool,
    pub zoom: PreviewZoom,
    /// Preview page (0-based).
    pub page: usize,
}

impl Default for PrintSettings {
    fn default() -> Self {
        PrintSettings {
            style: String::new(),
            printer: SAVE_AS_PDF.to_owned(),
            printers: Vec::new(),
            path: String::new(),
            copies: 1,
            pages: String::new(),
            paper: None,
            landscape: None,
            margins: [0.5; 4],
            header: Default::default(),
            footer: [String::new(), "[Page #]".to_owned(), String::new()],
            new_page_per_item: true,
            zoom: PreviewZoom::OnePage,
            page: 0,
        }
    }
}

impl PrintSettings {
    /// The `setup` object of the `print.*` commands.
    pub fn setup_json(&self) -> Value {
        let mut o = json!({
            "margins": {
                "top": self.margins[0],
                "bottom": self.margins[1],
                "left": self.margins[2],
                "right": self.margins[3],
            },
            "header": self.header,
            "footer": self.footer,
            "newPagePerItem": self.new_page_per_item,
        });
        if let Some(obj) = o.as_object_mut() {
            if let Some(p) = self.paper.and_then(|i| PAPERS.get(i)) {
                obj.insert("paper".into(), json!(p));
            }
            if let Some(l) = self.landscape {
                obj.insert("orientation".into(), json!(if l { "landscape" } else { "portrait" }));
            }
        }
        o
    }

    /// Parameters describing the job (what `print.preview` and `print.print` share).
    pub fn job(&self, ids: &[u64]) -> Value {
        let mut p = json!({"ids": ids, "setup": self.setup_json()});
        if !self.style.is_empty()
            && let Some(obj) = p.as_object_mut()
        {
            obj.insert("style".into(), json!(self.style));
        }
        p
    }

    /// Takes a Page Setup result (`setup` JSON as the engine writes it).
    pub fn apply_setup(&mut self, v: &Value) {
        if let Some(p) = v.get("paper").and_then(Value::as_str) {
            self.paper = PAPERS.iter().position(|x| x.eq_ignore_ascii_case(p));
        }
        if let Some(o) = v.get("orientation").and_then(Value::as_str) {
            self.landscape = Some(o == "landscape");
        }
        if let Some(m) = v.get("margins") {
            for (i, k) in ["top", "bottom", "left", "right"].iter().enumerate() {
                if let (Some(x), Some(slot)) = (m.get(*k).and_then(Value::as_f64), self.margins.get_mut(i))
                    && x.is_finite()
                {
                    *slot = (x as f32).clamp(0.0, 4.0);
                }
            }
        }
        let slots = |v: Option<&Value>, d: &mut [String; 3]| {
            if let Some(a) = v.and_then(Value::as_array) {
                for (i, slot) in d.iter_mut().enumerate() {
                    *slot = a.get(i).and_then(Value::as_str).unwrap_or_default().to_owned();
                }
            }
        };
        slots(v.get("header"), &mut self.header);
        slots(v.get("footer"), &mut self.footer);
        if let Some(b) = v.get("newPagePerItem").and_then(Value::as_bool) {
            self.new_page_per_item = b;
        }
    }
}

fn print_settings_id() -> Id {
    Id::new("mc-print-settings")
}

/// The session's print settings (printers fetched on first use).
pub fn print_settings(ctx: &Context, app: &mut App) -> PrintSettings {
    let mut st = ctx.data_mut(|d| d.get_temp::<PrintSettings>(print_settings_id())).unwrap_or_default();
    if st.printers.is_empty() {
        st.printers = app
            .s
            .run("print.printers", &json!({}))
            .ok()
            .and_then(|v| v.get("printers").and_then(Value::as_array).cloned())
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| vec![SAVE_AS_PDF.to_owned()]);
    }
    st
}

pub fn set_print_settings(ctx: &Context, st: PrintSettings) {
    ctx.data_mut(|d| d.insert_temp(print_settings_id(), st));
}

/// Items a print job acts on: a read window's message, a compose window's saved draft, else the main
/// window's selection (any item kind) or its focused item.
fn print_ids(app: &App, surface: Surface) -> Vec<u64> {
    match surface {
        Surface::Read(id) => vec![id],
        Surface::Compose(c) => {
            app.s.composes.get(&c).and_then(|c| c.draft).map(|d| vec![d.0]).unwrap_or_default()
        }
        Surface::Main => {
            let mut ids: Vec<u64> = app.s.selection.items.iter().map(|i| i.0).collect();
            if ids.is_empty()
                && let Some(f) = app.s.selection.focus
            {
                ids.push(f.0);
            }
            ids
        }
    }
}

/// Styles offered for `ids`: (id, label), and the default style id.
pub fn print_styles(app: &mut App, ids: &[u64]) -> (Vec<(String, String)>, String) {
    let Ok(v) = app.s.run("print.styles", &json!({"ids": ids})) else { return (Vec::new(), String::new()) };
    let styles = v
        .get("styles")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    Some((s.get("id")?.as_str()?.to_owned(), s.get("label")?.as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    let default = v.get("default").and_then(Value::as_str).unwrap_or_default().to_owned();
    (styles, default)
}

/// A file name for the PDF from the first item's subject (or "Print").
fn pdf_name(app: &App, ids: &[u64]) -> String {
    let subject = ids
        .first()
        .and_then(|i| app.s.mailbox.mail.get(ItemId(*i)).map(|m| m.subject.clone()))
        .unwrap_or_default();
    let clean: String = subject
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        .take(60)
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_owned();
    format!("{}.pdf", if clean.is_empty() { "Print" } else { &clean })
}

/// Runs the job on the chosen printer. A message for the page, or the error.
pub fn run_print(app: &mut App, st: &mut PrintSettings, ids: &[u64]) -> Result<String, String> {
    let mut p = st.job(ids);
    let pdf = st.printer == SAVE_AS_PDF;
    if pdf && st.path.trim().is_empty() {
        let start = format!("{}/{}", home(), pdf_name(app, ids));
        match crate::dialogs::browse_save(
            app,
            "Save as PDF",
            &[crate::FileFilter::new("PDF", &["pdf"])],
            &start,
        ) {
            Some(path) => st.path = path,
            None if app.files.is_some() => return Err(String::new()),
            None => st.path = start,
        }
    }
    if let Some(obj) = p.as_object_mut() {
        obj.insert("printer".into(), json!(st.printer));
        obj.insert("copies".into(), json!(st.copies.clamp(1, 99)));
        obj.insert("pages".into(), json!(st.pages.trim()));
        if pdf {
            obj.insert("path".into(), json!(st.path.trim()));
        }
    }
    match app.s.run("print.print", &p) {
        Ok(v) => {
            let n = v.get("pages").and_then(Value::as_u64).unwrap_or(0);
            Ok(match v.get("path").and_then(Value::as_str) {
                Some(path) => format!("Saved {n} page(s) to {path}."),
                None => format!("Sent {n} page(s) to {}.", st.printer),
            })
        }
        Err(e) => Err(e.to_string()),
    }
}

fn print(ui: &mut Ui, app: &mut App, surface: Surface, st: &mut PageState) {
    title(ui, app, "Print");
    let ctx = ui.ctx().clone();
    let mut ps = print_settings(&ctx, app);
    let compose = match surface {
        Surface::Compose(c) => Some(c),
        _ => None,
    };
    let ids = print_ids(app, surface);
    let (styles, default_style) = print_styles(app, &ids);
    if !ps.style.is_empty() && !styles.iter().any(|(i, _)| *i == ps.style) {
        ps.style.clear();
    }
    let avail = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(260.0);
            let (r, resp) = ui.allocate_exact_size(Vec2::new(90.0, 90.0), Sense::click());
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Print"));
            ui.painter().rect(
                r,
                4.0,
                if resp.hovered() { app.theme.hover } else { app.theme.pane },
                egui::Stroke::new(1.0, app.theme.separator),
                egui::StrokeKind::Inside,
            );
            app.icons.paint(
                ui,
                Rect::from_center_size(egui::pos2(r.center().x, r.top() + 34.0), Vec2::splat(34.0)),
                "print",
                app.theme.accent,
            );
            ui.painter().text(
                egui::pos2(r.center().x, r.bottom() - 16.0),
                egui::Align2::CENTER_CENTER,
                "Print",
                egui::FontId::proportional(13.0),
                app.theme.text,
            );
            if resp.clicked() {
                let ids = match compose {
                    Some(c) => {
                        app.run("compose.saveDraft", json!({"compose": c}));
                        print_ids(app, surface)
                    }
                    None => ids.clone(),
                };
                st.message = match run_print(app, &mut ps, &ids) {
                    Ok(m) | Err(m) => m,
                };
            }
            ui.add_space(10.0);
            ui.label(RichText::new("Printer").strong());
            printer_combo(ui, "mc-bs-printer", &mut ps, 240.0);
            if ps.printer == SAVE_AS_PDF {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut ps.path).hint_text("File name").desired_width(170.0),
                    );
                    if app.files.is_some() && ui.button("Browse...").clicked() {
                        let start = if ps.path.trim().is_empty() {
                            format!("{}/{}", home(), pdf_name(app, &ids))
                        } else {
                            ps.path.clone()
                        };
                        let filters = [crate::FileFilter::new("PDF", &["pdf"])];
                        if let Some(p) = crate::dialogs::browse_save(app, "Save as PDF", &filters, &start) {
                            ps.path = p;
                        }
                    }
                });
            }
            if ui.button("Print Options...").clicked() {
                let mut p = ps.job(&ids);
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("style".into(), json!(ps.style));
                }
                crate::dialogs::open(app, "print", p);
            }
            ui.add_space(10.0);
            ui.label(RichText::new("Settings").strong());
            for (id, label) in &styles {
                let on = if ps.style.is_empty() { *id == default_style } else { *id == ps.style };
                if ui.selectable_label(on, label.as_str()).clicked() {
                    ps.style = id.clone();
                    ps.page = 0;
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let mut paper = ps.paper.unwrap_or(0);
                if egui::ComboBox::from_id_salt("mc-bs-paper")
                    .width(90.0)
                    .selected_text(PAPERS.get(paper).copied().unwrap_or("Letter"))
                    .show_index(ui, &mut paper, PAPERS.len(), |i| PAPERS.get(i).copied().unwrap_or(""))
                    .changed()
                {
                    ps.paper = Some(paper);
                }
                let mut land = ps
                    .landscape
                    .unwrap_or(ps.style == "monthly" || (ps.style.is_empty() && default_style == "monthly"));
                let labels = ["Portrait", "Landscape"];
                let mut idx = usize::from(land);
                if egui::ComboBox::from_id_salt("mc-bs-orient")
                    .width(100.0)
                    .selected_text(labels.get(idx).copied().unwrap_or("Portrait"))
                    .show_index(ui, &mut idx, 2, |i| labels.get(i).copied().unwrap_or(""))
                    .changed()
                {
                    land = idx == 1;
                    ps.landscape = Some(land);
                }
            });
            if ui.button("Page Setup...").clicked() {
                let mut p = ps.job(&ids);
                if let Some(obj) = p.as_object_mut() {
                    obj.insert(
                        "style".into(),
                        json!(if ps.style.is_empty() { &default_style } else { &ps.style }),
                    );
                }
                app.run("print.pageSetup", p);
            }
            if !st.message.is_empty() {
                ui.add_space(8.0);
                muted(ui, app, &st.message);
            }
        });
        ui.vertical(|ui| {
            ui.set_width((avail - 290.0).max(240.0));
            match compose.and_then(|c| app.s.composes.get(&c)).cloned() {
                Some(c) if c.draft.is_none() => {
                    muted(ui, app, "The preview appears once the message is saved as a draft.");
                    egui::Frame::new().fill(Color32::WHITE).inner_margin(18.0).show(ui, |ui| {
                        ui.visuals_mut().override_text_color = Some(Color32::from_rgb(0x20, 0x20, 0x20));
                        ui.label(RichText::new(&c.subject).strong());
                        let id = Id::new(("mc-bs-compose-preview", c.id));
                        let mut view = ui
                            .ctx()
                            .data_mut(|d| d.get_temp::<crate::body::BodyView>(id))
                            .unwrap_or_default();
                        crate::body::show_doc(ui, &mut view, &c.body, false);
                        ui.ctx().data_mut(|d| d.insert_temp(id, view));
                    });
                }
                _ => {
                    let job = ps.job(&ids);
                    let h = (ui.available_height() - 8.0).max(240.0);
                    print_preview(ui, app, &job, &mut ps, "mc-bs-preview", h);
                }
            }
        });
    });
    set_print_settings(&ctx, ps);
}

/// Which Printer: Save as PDF and the system printers.
pub fn printer_combo(
    ui: &mut Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    ps: &mut PrintSettings,
    width: f32,
) {
    let printers = ps.printers.clone();
    egui::ComboBox::from_id_salt(salt).width(width).selected_text(ps.printer.as_str()).show_ui(ui, |ui| {
        for p in &printers {
            if ui.selectable_label(ps.printer == *p, p.as_str()).clicked() {
                ps.printer = p.clone();
            }
        }
    });
}

/// One cached preview page texture.
#[derive(Clone, Default)]
struct PreviewTex {
    key: u64,
    tex: Option<egui::TextureHandle>,
    pages: usize,
    /// Logical page size (px), for fitting.
    page_size: (f32, f32),
    error: String,
}

/// Something that changes when the mailbox does (cheap; cache key for preview textures).
fn mailbox_stamp(app: &App) -> u64 {
    let mb = &app.s.mailbox;
    mb.next_id
        ^ ((mb.mail.len() as u64) << 20)
        ^ ((mb.appointments.len() as u64) << 32)
        ^ ((mb.contacts.len() as u64) << 44)
        ^ app.s.composes.len() as u64
}

/// Decodes standard base64 (padding optional); `None` on bad input.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let bytes = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 3);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for &c in chunk {
            acc = (acc << 6) | val(c)?;
        }
        match chunk.len() {
            4 => out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]),
            3 => out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]),
            2 => out.push((acc >> 4) as u8),
            _ => return None,
        }
    }
    Some(out)
}

/// The texture of page `page` rendered at `scale` device px per logical px (cached per slot).
fn page_texture(
    ui: &Ui,
    app: &mut App,
    job: &Value,
    page: usize,
    scale: f32,
    salt: &str,
    slot: usize,
) -> PreviewTex {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    job.to_string().hash(&mut h);
    page.hash(&mut h);
    ((scale * 100.0).round() as i64).hash(&mut h);
    mailbox_stamp(app).hash(&mut h);
    let key = h.finish();
    let id = Id::new((salt, "tex", slot));
    if let Some(t) = ui.ctx().data_mut(|d| d.get_temp::<PreviewTex>(id))
        && t.key == key
    {
        return t;
    }
    let mut p = job.clone();
    if let Some(obj) = p.as_object_mut() {
        obj.insert("page".into(), json!(page));
        obj.insert("scale".into(), json!(scale));
        obj.insert("rgba".into(), json!(true));
    }
    let mut t = PreviewTex { key, ..PreviewTex::default() };
    match app.s.run("print.preview", &p) {
        Ok(v) => {
            let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
            let f = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
            t.pages = usize::try_from(n("pages")).unwrap_or(0);
            t.page_size = (f("pageWidth"), f("pageHeight"));
            let (w, hgt) =
                (usize::try_from(n("width")).unwrap_or(0), usize::try_from(n("height")).unwrap_or(0));
            let rgba = v.get("rgba").and_then(Value::as_str).and_then(decode_base64).unwrap_or_default();
            if w > 0 && hgt > 0 && w.checked_mul(hgt).and_then(|p| p.checked_mul(4)) == Some(rgba.len()) {
                let img = egui::ColorImage::from_rgba_unmultiplied([w, hgt], &rgba);
                t.tex =
                    Some(ui.ctx().load_texture(format!("{salt}-{slot}"), img, egui::TextureOptions::LINEAR));
            }
        }
        Err(e) => t.error = e.to_string(),
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

/// Print preview of `job` with the page controls (Previous Page, Current Page, Next Page, Actual Size,
/// One Page, Multiple Pages). Pages are rendered by the engine at the size they are shown.
pub fn print_preview(
    ui: &mut Ui,
    app: &mut App,
    job: &Value,
    ps: &mut PrintSettings,
    salt: &str,
    height: f32,
) {
    // Page count and size from the cached first slot (or Letter before the first render).
    let first =
        ui.ctx().data_mut(|d| d.get_temp::<PreviewTex>(Id::new((salt, "tex", 0usize)))).unwrap_or_default();
    let pages = first.pages.max(1);
    ps.page = ps.page.min(pages - 1);
    ui.horizontal(|ui| {
        let prev = ui.add_enabled(ps.page > 0, egui::Button::new("◀")).on_hover_text("Previous Page");
        if prev.clicked() {
            ps.page = ps.page.saturating_sub(1);
        }
        let mut cur = ps.page + 1;
        let r = ui.add(egui::DragValue::new(&mut cur).range(1..=pages).speed(0.1));
        r.widget_info(|| WidgetInfo::labeled(WidgetType::DragValue, true, "Current Page"));
        ps.page = cur.clamp(1, pages) - 1;
        ui.label(format!("of {pages}"));
        let next = ui.add_enabled(ps.page + 1 < pages, egui::Button::new("▶")).on_hover_text("Next Page");
        if next.clicked() {
            ps.page += 1;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            for (z, label) in [
                (PreviewZoom::MultiplePages, "Multiple Pages"),
                (PreviewZoom::OnePage, "One Page"),
                (PreviewZoom::ActualSize, "Actual Size"),
            ] {
                if ui.selectable_label(ps.zoom == z, label).clicked() {
                    ps.zoom = z;
                }
            }
        });
    });
    if !first.error.is_empty() {
        muted(ui, app, &first.error);
    }
    let (pw, ph) =
        if first.page_size.0 > 0.0 && first.page_size.1 > 0.0 { first.page_size } else { (816.0, 1056.0) };
    let w = ui.available_width().max(100.0);
    let h = (height - 36.0).min(ui.available_height() - 4.0).max(100.0);
    let gap = 12.0;
    let count = match ps.zoom {
        PreviewZoom::MultiplePages => (pages - ps.page).min(4),
        _ => 1,
    };
    let fit = match ps.zoom {
        PreviewZoom::ActualSize => 1.0,
        _ => ((w - gap * (count as f32 - 1.0)) / (pw * count as f32)).min(h / ph).clamp(0.05, 4.0),
    };
    let ppp = ui.ctx().pixels_per_point();
    let scale = ((fit * ppp * 20.0).round() / 20.0).clamp(0.05, 4.0);
    let show = |ui: &mut Ui, app: &mut App, ps: &PrintSettings| {
        ui.horizontal_top(|ui| {
            for k in 0..count {
                let t = page_texture(ui, app, job, ps.page + k, scale, salt, k);
                let size = Vec2::new(pw * fit, ph * fit);
                match &t.tex {
                    Some(tex) => {
                        let img = egui::Image::new(egui::load::SizedTexture::new(tex.id(), size));
                        let r = ui.add(img);
                        ui.painter().rect_stroke(
                            r.rect,
                            0.0,
                            egui::Stroke::new(1.0, app.theme.separator),
                            egui::StrokeKind::Outside,
                        );
                    }
                    None => {
                        let (r, _) = ui.allocate_exact_size(size, Sense::hover());
                        ui.painter().rect_filled(r, 0.0, Color32::WHITE);
                    }
                }
                ui.add_space(gap - ui.spacing().item_spacing.x);
            }
        });
    };
    if ps.zoom == PreviewZoom::ActualSize {
        egui::ScrollArea::both()
            .id_salt((salt, "scroll"))
            .max_height(h)
            .auto_shrink([false, false])
            .show(ui, |ui| show(ui, app, ps));
    } else {
        show(ui, app, ps);
    }
}

fn account(ui: &mut Ui, app: &mut App) {
    title(ui, app, "Account");
    ui.label(RichText::new("User Information").strong().size(15.0));
    for a in app.s.mailbox.accounts.clone() {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
            app.icons.paint(ui, r, "person", app.theme.accent);
            ui.label(RichText::new(&a.display_name).strong());
            muted(ui, app, &crate::dialogs::account_label(&a));
        });
    }
    ui.add_space(10.0);
    ui.label(RichText::new("Theme").strong().size(15.0));
    let mut dark = app.s.view.dark;
    ui.horizontal(|ui| {
        let a = ui.radio_value(&mut dark, false, "Light").changed();
        let b = ui.radio_value(&mut dark, true, "Dark").changed();
        if a || b {
            app.run("options.set", json!({"key": "view.darkMode", "value": dark}));
            app.theme = crate::theme::Theme::for_dark(dark);
        }
    });
    rule(ui, app);
    ui.label(RichText::new("Product Information").strong().size(15.0));
    ui.label(RichText::new("MailCraft").size(20.0));
    muted(ui, app, &version_line());
    muted(ui, app, "Open source: MIT or Apache-2.0.");
    ui.add_space(6.0);
    if tile(ui, app, "info", "About MailCraft", "Version, license and system details.") {
        app.run("help.about", json!({}));
    }
    if tile(ui, app, "help", "What's New", "See what changed in this version.") {
        app.run("help.whatsNew", json!({}));
    }
}

static BUILD_INFO: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets the build details the Account page shows after the version (`git <hash>, <date>`). The desktop app
/// calls this at start-up with the values from its build script, so the page matches `mailcraft --version`.
pub fn set_build_info(info: &str) {
    let _ = BUILD_INFO.set(info.to_owned());
}

/// `Version X.Y.Z (git <hash>, <date>)`; without build details (web, tests) just `Version X.Y.Z`, unless
/// `MAILCRAFT_GIT_HASH` / `MAILCRAFT_BUILD_DATE` were in this crate's build environment.
pub fn version_line() -> String {
    let version = mailcraft_engine::cmd::help::VERSION;
    let info = BUILD_INFO.get().cloned().or_else(|| {
        let hash = option_env!("MAILCRAFT_GIT_HASH").filter(|h| !h.is_empty())?;
        Some(match option_env!("MAILCRAFT_BUILD_DATE") {
            Some(date) => format!("git {hash}, {date}"),
            None => format!("git {hash}"),
        })
    });
    match info {
        Some(info) => format!("Version {version} ({info})"),
        None => format!("Version {version}"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn account_kinds_are_labelled_with_pop_accounts_as_pop() {
        let mut s = mailcraft_engine::sample::sample_session();
        s.run(
            "accounts.add",
            &json!({"kind": "pop3", "address": "pop@example.org", "incoming": {"host": "pop.example.org"}, "password": "x"}),
        )
        .unwrap();
        let pop = s.mailbox.accounts.iter().find(|a| a.address == "pop@example.org").unwrap();
        assert_eq!(super::kind_label(&s, pop), "POP/SMTP");
        let local =
            s.mailbox.accounts.iter().find(|a| a.kind == mailcraft_model::AccountKind::Local).unwrap();
        assert_eq!(super::kind_label(&s, local), "Local data file");
    }
}
