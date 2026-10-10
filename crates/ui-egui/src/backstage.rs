//! File backstage (main and compose) — backstage/dialogs area.
//!
//! A full-window page below the title bar (plan/outlook/backstage.md): a left navigation column (Back, Info,
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
    let bottom_h = 56.0 + 40.0 * (bottom.len().saturating_sub(1)) as f32;
    let gap = (rect.height() - used - bottom_h - 8.0).max(8.0);
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
    ids.iter().filter_map(|i| app.s.mailbox.mail.get(i)).collect()
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
    print_page: usize,
    table: bool,
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
    let mut clicked = false;
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
        clicked = enabled && resp.clicked();
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
    clicked
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
                kind_label(a.kind),
                if a.can_send() { "can send mail" } else { "stores mail on this computer" }
            ),
        );
    }
    ui.add_space(6.0);
    if ui.add(egui::Button::new("+ Add Account").min_size(Vec2::new(109.0, 28.0))).clicked() {
        crate::dialogs::open(app, "addAccount", json!({}));
    }
    rule(ui, app);
    if tile(
        ui,
        app,
        "account",
        "Account Settings...",
        "Change the settings for this account or add more connections.",
    ) {
        app.run("file.accountSettings", json!({"account": current}));
    }
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
            size_bar(ui, app, used, deleted);
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
            app.run("folder.autoArchiveSettings", json!({}));
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

fn kind_label(k: mailcraft_model::AccountKind) -> &'static str {
    match k {
        mailcraft_model::AccountKind::Local => "Local data file",
        mailcraft_model::AccountKind::Imap => "IMAP/SMTP",
        mailcraft_model::AccountKind::Jmap => "JMAP",
        mailcraft_model::AccountKind::Exchange => "Exchange",
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
    for m in mb.mail.values() {
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

fn size_bar(ui: &mut Ui, app: &App, used: u64, deleted: u64) {
    let w = 300.0;
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, 8.0), Sense::hover());
    ui.painter().rect_filled(r, 3.0, app.theme.hover);
    if used > 0 {
        let frac = (deleted as f32 / used as f32).clamp(0.0, 1.0);
        let keep = Rect::from_min_size(r.min, Vec2::new(w * (1.0 - frac), 8.0));
        ui.painter().rect_filled(keep, 3.0, app.theme.accent);
    }
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
    let draft = c.draft.and_then(|d| app.s.mailbox.mail.get(&d)).map(|m| (m.id.0, m.folder, m.size));
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

const ACTIONS: [(&str, &str); 4] = [
    ("importEml", "Import an .eml message"),
    ("importMbox", "Import an MBOX file"),
    ("importMaildir", "Import a Maildir folder"),
    ("exportMbox", "Export this folder to MBOX"),
];

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
        "sync",
        "Import/Export",
        "Step through importing EML, MBOX or Maildir mail, or exporting a folder to MBOX.",
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
    ui.horizontal(|ui| {
        let names: Vec<&str> = ACTIONS.iter().map(|a| a.1).collect();
        egui::ComboBox::from_id_salt("mc-bs-ie")
            .selected_text(names.get(st.action).copied().unwrap_or_default())
            .width(240.0)
            .show_ui(ui, |ui| {
                for (i, n) in names.iter().enumerate() {
                    if ui.selectable_label(st.action == i, *n).clicked() {
                        st.action = i;
                    }
                }
            });
        ui.add(egui::TextEdit::singleline(&mut st.path).hint_text("/path/to/file.mbox").desired_width(320.0));
    });
    let folder = app.s.selection.folder;
    muted(ui, app, &format!("Folder: {}", app.s.mailbox.folder_path(folder)));
    if ui.add_enabled(!st.path.trim().is_empty(), egui::Button::new("Run")).clicked() {
        let action = ACTIONS.get(st.action).map_or("importEml", |a| a.0);
        st.message = match app
            .run("file.openExport", json!({"action": action, "path": st.path.trim(), "folder": folder.0}))
        {
            Some(v) => match (v.get("imported"), v.get("exported")) {
                (Some(n), _) => format!("Imported {n} message(s)."),
                (_, Some(n)) => format!("Exported {n} message(s)."),
                _ => "Done.".into(),
            },
            None => app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default(),
        };
    }
    if !st.message.is_empty() {
        muted(ui, app, &st.message);
    }
}

fn home() -> String {
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
    muted(ui, app, "Saves the message as an Internet message file (.eml) other mail programs can open.");
    if st.path.is_empty() {
        st.path = home();
    }
    ui.horizontal(|ui| {
        ui.label("Save to (file or folder):");
        ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(420.0));
    });
    if ui.add(egui::Button::new("Save").min_size(Vec2::new(90.0, 28.0))).clicked() {
        let mut ids = ids;
        if let Some(c) = compose {
            app.run("compose.saveDraft", json!({"compose": c}));
            ids = app.s.composes.get(&c).and_then(|c| c.draft).map(|d| vec![d.0]).unwrap_or_default();
        }
        st.message = match app.run("file.saveAs", json!({"path": st.path.trim(), "ids": ids})) {
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

fn print(ui: &mut Ui, app: &mut App, surface: Surface, st: &mut PageState) {
    title(ui, app, "Print");
    let compose = match surface {
        Surface::Compose(c) => Some(c),
        _ => None,
    };
    let ids = match compose {
        Some(c) => app.s.composes.get(&c).and_then(|c| c.draft).map(|d| vec![d.0]).unwrap_or_default(),
        None => selected_ids(app, surface),
    };
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
                if let Some(c) = compose {
                    app.run("compose.saveDraft", json!({"compose": c}));
                    let ids: Vec<u64> =
                        app.s.composes.get(&c).and_then(|c| c.draft).map(|d| vec![d.0]).unwrap_or_default();
                    app.run("file.print", json!({"ids": ids}));
                } else {
                    app.run("file.print", json!({"ids": ids}));
                }
            }
            ui.add_space(10.0);
            ui.label(RichText::new("Printer").strong());
            muted(ui, app, "Save as HTML file");
            if ui.button("Print Options...").clicked() {
                app.run("file.print", json!({"ids": ids}));
            }
            ui.add_space(10.0);
            ui.label(RichText::new("Settings").strong());
            for (i, s) in files::STYLES.iter().enumerate() {
                if ui.selectable_label(st.table == (i == 1), *s).clicked() {
                    st.table = i == 1;
                }
            }
        });
        ui.vertical(|ui| {
            ui.set_width((avail - 290.0).max(240.0));
            match compose.and_then(|c| app.s.composes.get(&c)).cloned() {
                Some(c) if c.draft.is_none() => {
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
                    files::print_preview(ui, app, &ids, st.table, &mut st.print_page, "mc-bs-preview", 520.0)
                }
            }
        });
    });
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
    muted(ui, app, &format!("Version {}", mailcraft_engine::cmd::help::VERSION));
    muted(ui, app, "Open source: MIT or Apache-2.0.");
    ui.add_space(6.0);
    if tile(ui, app, "info", "About MailCraft", "Version, license and system details.") {
        app.run("help.about", json!({}));
    }
    if tile(ui, app, "help", "What's New", "See what changed in this version.") {
        app.run("help.whatsNew", json!({}));
    }
}
