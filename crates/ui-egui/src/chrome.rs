//! Window chrome: title bar (QAT, search box, account), tab row, module rail, status bar (shell area), and the
//! main-window composition: bands from plan/outlook/layout.md, rounded panes on a backdrop, splitters and the
//! narrow-window rules from plan/outlook/responsive.md.

use egui::{Align, Align2, Color32, CornerRadius, Layout, Rect, Sense, Stroke, Ui, UiBuilder, pos2, vec2};
use serde_json::json;

use crate::responsive::Frame;
use crate::theme::{Theme, size};
use crate::{App, Surface, widgets, with_shell};
use mailcraft_engine::{Module, SearchScope};
use mailcraft_model::{FolderPaneState, ReadingPanePos};

/// Fallback tab list for the main window while the ribbon reports none.
const DEFAULT_TABS: [&str; 4] = ["Home", "Send / Receive", "View", "Help"];

/// Commands offered in the Quick Access Toolbar customise menu (labels come from the registry).
const QAT_CHOICES: [&str; 11] = [
    "mail.newEmail",
    "mail.reply",
    "mail.replyAll",
    "mail.forward",
    "mail.delete",
    "mail.markAllRead",
    "mail.print",
    "sendReceive.all",
    "app.undo",
    "app.redo",
    "folder.emptyDeleted",
];

/// Module rail entries: module, command, icon, name.
const RAIL: [(Module, &str, &str, &str); 4] = [
    (Module::Mail, "nav.mail", "mail", "Mail"),
    (Module::Calendar, "nav.calendar", "calendar", "Calendar"),
    (Module::People, "nav.people", "people", "People"),
    (Module::Tasks, "nav.tasks", "tasks", "To Do"),
];

/// More Apps flyout entries.
const MORE_APPS: [(Module, &str, &str, &str); 3] = [
    (Module::Notes, "nav.notes", "notes", "Notes"),
    (Module::Folders, "nav.folders", "folders", "Folders"),
    (Module::Shortcuts, "nav.shortcuts", "shortcuts", "Shortcuts"),
];

/// Icon for a command shown as a toolbar button.
pub fn command_icon(id: &str) -> &'static str {
    match id {
        "sendReceive.all" | "sendReceive.folder" => "sync",
        "sendReceive.sendAll" => "send",
        "app.undo" => "undo",
        "app.redo" => "redo",
        "mail.newEmail" => "new-mail",
        "mail.reply" => "reply",
        "mail.replyAll" => "reply-all",
        "mail.forward" => "forward",
        "mail.delete" => "delete",
        "mail.markAllRead" | "mail.markRead" => "read",
        "mail.markUnread" => "unread",
        "mail.print" | "file.print" => "print",
        "folder.emptyDeleted" => "deleted",
        "app.copy" => "copy",
        "app.paste" => "paste",
        "file.options" => "settings",
        _ => "ellipsis",
    }
}

/// Registry label of a command (the id itself when unknown).
fn command_label(app: &App, id: &str) -> String {
    app.s.registry.get(id).map_or_else(|| id.to_owned(), |s| s.label.to_owned())
}

/// Narrows `ui` to `rect` as a fresh child (clipped to it).
fn child(ui: &mut Ui, rect: Rect, salt: &str, layout: Layout) -> Ui {
    let mut c = ui.new_child(UiBuilder::new().id_salt(salt).max_rect(rect).layout(layout));
    c.set_clip_rect(rect.intersect(ui.clip_rect()));
    c
}

/// Text and hover colours for widgets drawn on the dark chrome bands.
fn chrome_visuals(ui: &mut Ui, theme: &Theme) {
    let v = ui.visuals_mut();
    v.override_text_color = Some(theme.chrome_text);
    let hover = theme.chrome_text.gamma_multiply(0.14);
    let press = theme.chrome_text.gamma_multiply(0.24);
    v.widgets.hovered.weak_bg_fill = hover;
    v.widgets.hovered.bg_fill = hover;
    v.widgets.active.weak_bg_fill = press;
    v.widgets.active.bg_fill = press;
    v.selection.bg_fill = press;
    v.selection.stroke = Stroke::new(1.0, theme.chrome_text);
}

/// Whole main window: title bar, tab row, ribbon, module rail, content, status bar.
pub fn main_window(ui: &mut Ui, app: &mut App, frame: &Frame) {
    let full = ui.max_rect();
    let ctx = ui.ctx().clone();
    with_shell(&ctx, |st| {
        let prev = std::mem::take(&mut st.layout);
        st.prev_pane_rects = prev.rects;
    });
    let mut top = full.top();

    let title = Rect::from_min_max(full.min, pos2(full.right(), top + size::TITLE_BAR));
    top = title.bottom();
    let mut c = child(ui, title, "mc_title_bar", Layout::left_to_right(Align::Center));
    title_bar(&mut c, app, frame);

    let reveal = with_shell(&ctx, |st| st.ribbon_reveal);
    let auto_hide = app.s.view.ribbon_auto_hide;
    let show_tabs = !auto_hide || reveal;
    let show_ribbon = (!auto_hide && !app.s.view.ribbon_collapsed) || reveal;
    with_shell(&ctx, |st| {
        st.layout.tabs_shown = show_tabs;
        st.layout.ribbon_shown = show_ribbon;
    });
    let mut keep = title;
    if show_tabs {
        let r = Rect::from_min_max(pos2(full.left(), top), pos2(full.right(), top + size::TAB_ROW));
        top = r.bottom();
        keep = keep.union(r);
        let mut c = child(ui, r, "mc_tab_row", Layout::left_to_right(Align::Center));
        tab_row(&mut c, app, frame);
    }
    if show_ribbon {
        let h = if app.s.view.simplified_ribbon { size::RIBBON_SIMPLIFIED } else { size::RIBBON_CLASSIC };
        let r = Rect::from_min_max(pos2(full.left(), top), pos2(full.right(), (top + h).min(full.bottom())));
        top = r.bottom();
        keep = keep.union(r);
        ui.painter().rect_filled(r, CornerRadius::ZERO, app.theme.ribbon);
        ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, app.theme.separator));
        let mut c = child(ui, r, "mc_ribbon", Layout::left_to_right(Align::Center));
        crate::ribbon::ribbon(&mut c, app, Surface::Main, frame);
    }
    // A ribbon shown on demand hides again on the next click outside it.
    if reveal
        && ctx.input(|i| i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !keep.contains(p)))
    {
        with_shell(&ctx, |st| st.ribbon_reveal = false);
    }

    let mut bottom = full.bottom();
    if app.s.view.status_bar {
        let r = Rect::from_min_max(
            pos2(full.left(), (bottom - size::STATUS_BAR).max(top)),
            pos2(full.right(), bottom),
        );
        bottom = r.top();
        let mut c = child(ui, r, "mc_status_bar", Layout::left_to_right(Align::Center));
        status_bar(&mut c, app, frame);
    }

    let rail = Rect::from_min_max(
        pos2(full.left(), top),
        pos2((full.left() + size::RAIL).min(full.right()), bottom),
    );
    let mut c = child(ui, rail, "mc_module_rail", Layout::top_down(Align::Center));
    module_rail(&mut c, app);

    let content = Rect::from_min_max(pos2(rail.right(), top), pos2(full.right(), bottom));
    if content.width() > 0.0 && content.height() > 0.0 {
        let mut c = child(ui, content, "mc_content", Layout::top_down(Align::Min));
        content_area(&mut c, app, frame);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Title bar

pub fn title_bar(ui: &mut Ui, app: &mut App, frame: &Frame) {
    let bar = ui.max_rect();
    ui.painter().rect_filled(bar, CornerRadius::ZERO, app.theme.chrome);
    let theme = app.theme.clone();
    chrome_visuals(ui, &theme);

    // App icon.
    let icon_rect = Rect::from_center_size(pos2(bar.left() + 22.0, bar.center().y), vec2(20.0, 20.0));
    if let Some(tex) = app_icon_texture(ui.ctx()) {
        ui.painter().image(
            tex.id(),
            icon_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    // Quick Access Toolbar.
    let qat_rect = Rect::from_min_max(pos2(bar.left() + 42.0, bar.top()), pos2(bar.right(), bar.bottom()));
    let mut qat = child(ui, qat_rect, "mc_qat", Layout::left_to_right(Align::Center));
    qat.spacing_mut().item_spacing.x = 1.0;
    let keytips = app.ui.keytips.as_deref() == Some("");
    for (i, id) in app.s.view.quick_access.clone().into_iter().enumerate() {
        let label = command_label(app, &id);
        let enabled = app.s.enabled(&id).is_none();
        let r = widgets::icon_only(&mut qat, app, command_icon(&id), &label, enabled);
        if keytips && i < 9 {
            keytip_badge(&qat, r.rect, &(i + 1).to_string(), &theme);
        }
        if r.clicked() {
            app.run(&id, json!({}));
        }
    }
    let custom = widgets::icon_only(&mut qat, app, "chevron-down", "Customize Quick Access Toolbar", true);
    egui::Popup::menu(&custom).show(|ui| qat_menu(ui, app));
    let qat_end = qat.min_rect().right();

    // Right side: account button, then (full-screen ribbon mode) the show-ribbon button.
    let account = Rect::from_min_max(pos2(bar.right() - size::ACCOUNT_BUTTON_W, bar.top()), bar.max);
    account_button(ui, app, account);
    let mut right_edge = account.left();
    if app.s.view.ribbon_auto_hide {
        let r = Rect::from_min_max(pos2(right_edge - 36.0, bar.top()), pos2(right_edge, bar.bottom()));
        right_edge = r.left();
        let mut c = child(ui, r, "mc_reveal", Layout::left_to_right(Align::Center));
        if widgets::icon_only(&mut c, app, "ellipsis", "Show Ribbon", true).clicked() {
            with_shell(ui.ctx(), |st| st.ribbon_reveal = !st.ribbon_reveal);
        }
    }

    search_box(ui, app, frame, bar, qat_end, right_edge);
}

fn app_icon_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    if let Some(t) = with_shell(ctx, |st| st.app_icon.clone()) {
        return Some(t);
    }
    let image = crate::app_icon_image(40)?;
    let tex = ctx.load_texture("mailcraft-app-icon", image, egui::TextureOptions::LINEAR);
    with_shell(ctx, |st| st.app_icon = Some(tex.clone()));
    Some(tex)
}

fn qat_menu(ui: &mut Ui, app: &mut App) {
    ui.label(egui::RichText::new("Customize Quick Access Toolbar").strong());
    ui.separator();
    for id in QAT_CHOICES {
        if app.s.registry.get(id).is_none() {
            continue;
        }
        let on = app.s.view.quick_access.iter().any(|q| q == id);
        let label = command_label(app, id);
        if ui.add(egui::Button::new(label).selected(on)).clicked() {
            let key = if on { "remove" } else { "add" };
            app.run("app.quickAccess", json!({ key: id }));
            ui.close();
        }
    }
    ui.separator();
    if ui.button("More Commands...").clicked() {
        app.run("app.quickAccess", json!({}));
        ui.close();
    }
}

fn account_button(ui: &mut Ui, app: &mut App, rect: Rect) {
    let (name, addr) = app
        .s
        .mailbox
        .default_account()
        .map(|a| (a.display_name.clone(), a.address.clone()))
        .unwrap_or_default();
    let initials = mailcraft_engine::queries::initials(Some(name.as_str()), &addr);
    let label = if addr.is_empty() { "Account".to_owned() } else { format!("Account: {addr}") };
    let resp = ui.interact(rect, ui.id().with("mc_account"), Sense::click());
    let info = label.clone();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, info.clone()));
    let theme = &app.theme;
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::ZERO, theme.chrome_text.gamma_multiply(0.14));
    }
    let c = rect.center();
    ui.painter().circle_filled(c, 14.0, theme.accent);
    ui.painter().circle_stroke(c, 14.0, Stroke::new(1.0, theme.chrome_text.gamma_multiply(0.6)));
    let text = if initials.is_empty() { "?".to_owned() } else { initials };
    ui.painter().text(c, Align2::CENTER_CENTER, text, egui::FontId::proportional(12.0), Color32::WHITE);
    let resp = resp.on_hover_text(label);
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(220.0);
        if name.is_empty() && addr.is_empty() {
            ui.label("No account");
        } else {
            ui.label(egui::RichText::new(&name).strong());
            ui.label(egui::RichText::new(&addr).color(ui.visuals().weak_text_color()));
        }
        ui.separator();
        if ui
            .add_enabled(
                app.s.enabled("file.accountSettings").is_none(),
                egui::Button::new("Account Settings..."),
            )
            .clicked()
        {
            app.run("file.accountSettings", json!({}));
            ui.close();
        }
        if ui.button("Options").clicked() {
            app.run("file.options", json!({}));
            ui.close();
        }
    });
}

fn scope_label(scope: SearchScope) -> &'static str {
    match scope {
        SearchScope::CurrentFolder => "Current Folder",
        SearchScope::Subfolders => "Subfolders",
        SearchScope::CurrentMailbox => "Current Mailbox",
        SearchScope::AllMailboxes => "All Mailboxes",
        SearchScope::AllOutlookItems => "All Items",
    }
}

const SCOPES: [(SearchScope, &str); 5] = [
    (SearchScope::AllMailboxes, "allMailboxes"),
    (SearchScope::CurrentMailbox, "currentMailbox"),
    (SearchScope::CurrentFolder, "currentFolder"),
    (SearchScope::Subfolders, "subfolders"),
    (SearchScope::AllOutlookItems, "allItems"),
];

/// Leaves search: clears the query, drops the Search tab.
fn close_search(app: &mut App) {
    app.run("search.close", json!({}));
    app.ui.search_text.clear();
    app.ui.search_active = false;
    if app.ui.tab(Surface::Main) == Some("Search") {
        app.ui.set_tab(Surface::Main, "Home");
    }
}

fn search_box(ui: &mut Ui, app: &mut App, _frame: &Frame, bar: Rect, qat_end: f32, right_edge: f32) {
    let ctx = ui.ctx().clone();
    let list_left = with_shell(&ctx, |st| st.list_left).unwrap_or(bar.left() + 294.0);
    let x = list_left.max(qat_end + 8.0);
    let avail = right_edge - 8.0 - x;
    let w = if avail * 0.6 >= size::SEARCH_W { size::SEARCH_W } else { (avail * 0.6).max(20.0).min(avail) };
    if w < 20.0 {
        return;
    }
    let rect = Rect::from_center_size(pos2(x + w / 2.0, bar.center().y), vec2(w, size::SEARCH_H));
    let edit_id = egui::Id::new("mailcraft_search_box");
    let focused = ctx.memory(|m| m.has_focus(edit_id));

    // Pick up queries set by commands (refine, recent searches) while the box is not being edited.
    if !focused && app.ui.search_text != app.s.view.search {
        app.ui.search_text = app.s.view.search.clone();
        if !app.ui.search_text.is_empty() {
            app.ui.search_active = true;
        }
    }
    let active = app.ui.search_active || focused;
    let theme = app.theme.clone();
    let (fill, fg) = if active {
        (theme.pane, theme.text)
    } else {
        (theme.chrome_text.gamma_multiply(0.16), theme.chrome_text)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(6),
        fill,
        Stroke::new(1.0, fg.gamma_multiply(0.25)),
        egui::StrokeKind::Inside,
    );

    let mut c = child(ui, rect.shrink2(vec2(8.0, 2.0)), "mc_search", Layout::left_to_right(Align::Center));
    {
        let v = c.visuals_mut();
        v.override_text_color = Some(fg);
        v.widgets.hovered.weak_bg_fill = fg.gamma_multiply(0.12);
        v.widgets.active.weak_bg_fill = fg.gamma_multiply(0.2);
    }
    c.spacing_mut().item_spacing.x = 4.0;
    let icon = c.allocate_exact_size(vec2(18.0, 18.0), Sense::hover()).0;
    app.icons.paint(&c, icon, "search", fg);

    let show_scope = active && w >= 220.0;
    let show_clear = active || !app.ui.search_text.is_empty();
    let tail = if show_scope { 128.0 } else { 0.0 } + if show_clear { 30.0 } else { 0.0 };
    let edit_w = (c.available_width() - tail).max(10.0);
    let hint = egui::RichText::new("Search").color(fg.gamma_multiply(0.75));
    let edit = c.add(
        egui::TextEdit::singleline(&mut app.ui.search_text)
            .id(edit_id)
            .hint_text(hint)
            .frame(egui::Frame::NONE)
            .text_color(fg)
            .desired_width(edit_w)
            .vertical_align(Align::Center),
    );
    let focus_requested = with_shell(&ctx, |st| std::mem::take(&mut st.focus_search));
    if focus_requested {
        edit.request_focus();
    }
    if edit.gained_focus() {
        app.ui.search_active = true;
        app.ui.set_tab(Surface::Main, "Search");
    }
    if edit.changed() {
        let text = app.ui.search_text.clone();
        app.run("search.query", json!({ "text": text }));
    }
    if edit.has_focus() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close_search(app);
        edit.surrender_focus();
    }

    let mut scope_open = false;
    if show_scope {
        let label = scope_label(app.s.view.search_scope);
        let r = widgets::dropdown_button(&mut c, app, "", label, true).on_hover_text("Search scope");
        let popup = egui::Popup::menu(&r).show(|ui| {
            for (scope, key) in SCOPES {
                let on = app.s.view.search_scope == scope;
                if ui.add(egui::Button::new(scope_label(scope)).selected(on)).clicked() {
                    app.run("search.scope", json!({ "scope": key }));
                    ui.close();
                }
            }
        });
        scope_open = popup.is_some();
    }
    if show_clear && widgets::icon_only(&mut c, app, "close", "Close Search", true).clicked() {
        close_search(app);
    }
    // Leaving an empty box (focus moves away, or a click lands elsewhere) ends search mode and drops the
    // Search tab. A box that is still waiting for focus (dialog on top) stays active.
    // "Elsewhere" = in a content pane, so clicks on the Search tab and its ribbon keep search mode.
    let panes = with_shell(&ctx, |st| st.prev_pane_rects.clone());
    let clicked_elsewhere = ctx.input(|i| {
        i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| panes.iter().any(|r| r.contains(p)))
    });
    if app.ui.search_active
        && !edit.has_focus()
        && !focus_requested
        && app.ui.search_text.is_empty()
        && app.s.view.search.is_empty()
        && !scope_open
        && (edit.lost_focus() || clicked_elsewhere)
    {
        app.ui.search_active = false;
        if app.ui.tab(Surface::Main) == Some("Search") {
            app.ui.set_tab(Surface::Main, "Home");
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Tab row

/// Main-window tab names (File excluded).
fn main_tabs(app: &App) -> Vec<&'static str> {
    let tabs = crate::ribbon::tabs(app, Surface::Main);
    if tabs.is_empty() {
        let mut t = DEFAULT_TABS.to_vec();
        if app.ui.search_active {
            t.push("Search");
        }
        t
    } else {
        tabs
    }
}

pub fn tab_row(ui: &mut Ui, app: &mut App, frame: &Frame) {
    let _ = frame;
    let row = ui.max_rect();
    ui.painter().rect_filled(row, CornerRadius::ZERO, app.theme.chrome);
    let theme = app.theme.clone();
    chrome_visuals(ui, &theme);
    ui.spacing_mut().item_spacing.x = 2.0;
    ui.add_space(4.0);

    // File → backstage.
    let font = egui::FontId::proportional(13.0);
    let file = tab_button(ui, "File", &font, false, &theme, size::FILE_BUTTON_W);
    if file.clicked() {
        app.ui.backstage = Some((Surface::Main, "info".to_owned()));
    }

    let tabs = main_tabs(app);
    let selected = app
        .ui
        .tab(Surface::Main)
        .and_then(|t| tabs.iter().copied().find(|x| *x == t))
        .or_else(|| tabs.first().copied())
        .unwrap_or("Home");
    let ribbon_hidden = app.s.view.ribbon_collapsed || app.s.view.ribbon_auto_hide;
    let keytips = app.ui.keytips.as_deref() == Some("");
    if keytips {
        keytip_badge(ui, file.rect, "F", &theme);
    }
    for tab in &tabs {
        let on = *tab == selected;
        let r = tab_button(ui, tab, &font, on, &theme, 0.0);
        if keytips {
            keytip_badge(ui, r.rect, crate::ribbon::tab_keytip(app, Surface::Main, tab), &theme);
        }
        if r.clicked() {
            app.ui.set_tab(Surface::Main, tab);
            if ribbon_hidden {
                with_shell(ui.ctx(), |st| st.ribbon_reveal = !(on && st.ribbon_reveal));
            }
        }
        if r.double_clicked() && !app.s.view.ribbon_auto_hide {
            let mode = if app.s.view.ribbon_collapsed { "always" } else { "tabsOnly" };
            app.run("app.toggleRibbonMode", json!({ "mode": mode }));
            with_shell(ui.ctx(), |st| st.ribbon_reveal = false);
        }
    }

    // Right: Ribbon Display Options, only while the ribbon body (which carries its own ⌄ at the bottom-right,
    // layout.md) is hidden, so there is one such control on screen at a time.
    let revealed = with_shell(ui.ctx(), |st| st.ribbon_reveal);
    if !ribbon_hidden || revealed {
        return;
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add_space(6.0);
        let r = widgets::dropdown_button(ui, app, "ribbon-display", "", true)
            .on_hover_text("Ribbon Display Options");
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Ribbon Display Options"));
        egui::Popup::menu(&r).show(|ui| ribbon_display_menu(ui, app));
    });
}

fn tab_button(
    ui: &mut Ui,
    label: &str,
    font: &egui::FontId,
    on: bool,
    theme: &Theme,
    min_w: f32,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), theme.chrome_text);
    let w = (galley.size().x + 20.0).max(min_w);
    let h = ui.available_height();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let info = label.to_owned();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, info.clone()));
    if resp.hovered() {
        ui.painter().rect_filled(
            rect.shrink2(vec2(0.0, 2.0)),
            CornerRadius::same(4),
            theme.chrome_text.gamma_multiply(0.12),
        );
    }
    let pos = pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0);
    let text_w = galley.size().x;
    ui.painter().galley(pos, galley, theme.chrome_text);
    if on {
        let underline = if theme.dark { theme.accent } else { theme.chrome_text };
        let y = rect.bottom() - 3.0;
        let half = (text_w / 2.0).min(rect.width() / 2.0 - 2.0).max(4.0);
        ui.painter().line_segment(
            [pos2(rect.center().x - half, y), pos2(rect.center().x + half, y)],
            Stroke::new(2.5, underline),
        );
    }
    resp
}

/// Alt keytip badge centred under `rect` (drawn above everything else on the window).
fn keytip_badge(ui: &Ui, rect: Rect, text: &str, theme: &Theme) {
    if text.is_empty() {
        return;
    }
    let painter =
        ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("mc_keytips")));
    let galley = painter.layout_no_wrap(text.to_owned(), egui::FontId::proportional(11.0), theme.text);
    let size = vec2(galley.size().x + 8.0, galley.size().y + 2.0);
    let badge = Rect::from_center_size(pos2(rect.center().x, rect.bottom()), size);
    painter.rect(
        badge,
        CornerRadius::same(2),
        theme.pane,
        Stroke::new(1.0, theme.text_muted),
        egui::StrokeKind::Inside,
    );
    painter.galley(badge.center() - galley.size() / 2.0, galley, theme.text);
}

fn ribbon_display_menu(ui: &mut Ui, app: &mut App) {
    ui.set_min_width(200.0);
    let v = app.s.view.clone();
    let pick = |ui: &mut Ui, app: &mut App, label: &str, on: bool, mode: &str| {
        if ui.add(egui::Button::new(label).selected(on)).clicked() {
            app.run("app.toggleRibbonMode", json!({ "mode": mode }));
            with_shell(ui.ctx(), |st| st.ribbon_reveal = false);
            ui.close();
        }
    };
    ui.label(egui::RichText::new("Ribbon layout").color(ui.visuals().weak_text_color()));
    pick(ui, app, "Classic ribbon", !v.simplified_ribbon, "classic");
    pick(ui, app, "Simplified ribbon", v.simplified_ribbon, "simplified");
    ui.separator();
    ui.label(egui::RichText::new("Show ribbon").color(ui.visuals().weak_text_color()));
    pick(ui, app, "Full-screen mode", v.ribbon_auto_hide, "autoHide");
    pick(ui, app, "Show tabs only", v.ribbon_collapsed && !v.ribbon_auto_hide, "tabsOnly");
    pick(ui, app, "Always show Ribbon", !v.ribbon_collapsed && !v.ribbon_auto_hide, "always");
}

// ---------------------------------------------------------------------------------------------------------
// Module rail

pub fn module_rail(ui: &mut Ui, app: &mut App) {
    let rail = ui.max_rect();
    ui.painter().rect_filled(rail, CornerRadius::ZERO, app.theme.chrome);
    let theme = app.theme.clone();
    chrome_visuals(ui, &theme);
    ui.spacing_mut().item_spacing.y = 0.0;
    let current = app.s.view.module;

    let mut entries: Vec<(Module, &str, &str, &str)> = RAIL.to_vec();
    if let Some(p) = app.s.view.pinned_app
        && let Some(e) = MORE_APPS.iter().find(|e| e.0 == p)
    {
        entries.push(*e);
    }
    for (module, cmd, icon, name) in entries {
        let on = current == module;
        let r = widgets::icon_toggle(ui, app, icon, name, on, size::RAIL_ITEM);
        if on {
            let bar = Rect::from_min_size(
                pos2(r.rect.left(), r.rect.top() + 12.0),
                vec2(3.0, r.rect.height() - 24.0),
            );
            ui.painter().rect_filled(bar, CornerRadius::same(1), theme.chrome_text);
        }
        if r.clicked() && !on {
            app.run(cmd, json!({}));
        }
    }
    let more_on = MORE_APPS.iter().any(|e| e.0 == current && app.s.view.pinned_app != Some(e.0));
    let r = widgets::icon_toggle(ui, app, "more-apps", "More Apps", more_on, size::RAIL_ITEM);
    egui::Popup::menu(&r).align(egui::RectAlign::RIGHT_START).show(|ui| {
        ui.set_min_width(180.0);
        for (module, cmd, icon, name) in MORE_APPS {
            if widgets::toggle_button(ui, app, icon, name, current == module).clicked() {
                app.run(cmd, json!({}));
                ui.close();
            }
        }
        ui.separator();
        if widgets::icon_button(ui, app, "settings", "Options", true).clicked() {
            app.run("file.options", json!({}));
            ui.close();
        }
    });
}

// ---------------------------------------------------------------------------------------------------------
// Status bar

/// Reminders due now: open tasks whose reminder time has passed, appointments inside their reminder window.
pub fn reminders_due(app: &App) -> usize {
    let now = app.s.now().unix;
    let mb = &app.s.mailbox;
    let tasks = mb
        .tasks
        .values()
        .filter(|t| t.completed.is_none() && t.reminder.is_some_and(|r| r.unix <= now))
        .count();
    let appts = mb
        .appointments
        .values()
        .filter(|a| {
            a.reminder_min.is_some_and(|m| {
                let due = a.start.unix.saturating_sub(i64::from(m).saturating_mul(60));
                due <= now && now < a.end.unix
            })
        })
        .count();
    tasks + appts
}

/// Zoom percent ↔ slider position (100 % sits in the middle; 10..=500).
fn zoom_to_t(pct: u16) -> f32 {
    let p = f32::from(pct);
    if p <= 100.0 {
        ((p - 10.0) / 90.0 * 0.5).clamp(0.0, 0.5)
    } else {
        (0.5 + (p - 100.0) / 400.0 * 0.5).clamp(0.5, 1.0)
    }
}
fn t_to_zoom(t: f32) -> u16 {
    let p = if t <= 0.5 { 10.0 + t / 0.5 * 90.0 } else { 100.0 + (t - 0.5) / 0.5 * 400.0 };
    // Rounded and clamped to 10..=500, so the cast cannot truncate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let p = p.round().clamp(10.0, 500.0) as u16;
    p
}

pub fn status_bar(ui: &mut Ui, app: &mut App, frame: &Frame) {
    let bar = ui.max_rect();
    let theme = app.theme.clone();
    ui.painter().rect_filled(bar, CornerRadius::ZERO, theme.ribbon);
    ui.painter().hline(bar.x_range(), bar.top() + 0.5, Stroke::new(1.0, theme.separator));
    let bg = ui.interact(bar, ui.id().with("mc_status_bg"), Sense::click());
    bg.context_menu(|ui| crate::menus::status_bar_menu(ui, app));

    ui.visuals_mut().override_text_color = Some(theme.text_muted);
    ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));
    ui.spacing_mut().interact_size.y = 20.0;
    ui.spacing_mut().item_spacing.x = 14.0;
    ui.add_space(10.0);

    if app.mail_like_module() {
        let (items, unread) = mailcraft_engine::queries::status_counts(&app.s);
        ui.label(format!("Items: {items}"));
        if unread > 0 {
            ui.label(format!("Unread: {unread}"));
        }
        if !app.s.view.search.is_empty() {
            ui.label("Filter applied");
        }
    }
    let due = reminders_due(app);
    if due > 0 {
        let r = widgets::icon_button(ui, app, "bell", &format!("Reminders: {due}"), true);
        if r.clicked() {
            app.run("view.reminderWindow", json!({}));
        }
    }
    sync_status(ui, app, frame);

    // Right side: view buttons and zoom (the zoom group is dropped first on very narrow windows), then the
    // connection state.
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.add_space(8.0);
        let zoom = app.s.view.zoom;
        if frame.width > 520.0 {
            if ui.add(egui::Button::new(format!("{zoom}%")).frame(false)).on_hover_text("Zoom").clicked() {
                app.run("view.zoom", json!({}));
            }
            if widgets::icon_only(ui, app, "zoom-in", "Zoom In", zoom < 500).clicked() {
                let next = (zoom / 10 + 1).saturating_mul(10).min(500);
                app.run("view.zoom", json!({ "pct": next }));
            }
            let mut t = zoom_to_t(zoom);
            ui.spacing_mut().slider_width = 96.0;
            let slider = ui
                .scope(|ui| {
                    ui.spacing_mut().interact_size.y = 12.0;
                    // The track uses the inactive fill, which equals the status bar background.
                    ui.visuals_mut().widgets.inactive.bg_fill = theme.separator;
                    ui.visuals_mut().selection.bg_fill = theme.text_muted;
                    let handle = egui::style::HandleShape::Circle;
                    ui.add(egui::Slider::new(&mut t, 0.0..=1.0).show_value(false).handle_shape(handle))
                })
                .inner
                .on_hover_text("Zoom");
            if slider.changed() {
                app.run("view.zoom", json!({ "pct": t_to_zoom(t) }));
            }
            if widgets::icon_only(ui, app, "zoom-out", "Zoom Out", zoom > 10).clicked() {
                let next = zoom.saturating_sub(1) / 10 * 10;
                app.run("view.zoom", json!({ "pct": next.max(10) }));
            }
            ui.add_space(10.0);
        }
        let reading = app.s.view.reading_view;
        if widgets::icon_toggle(ui, app, "reading-pane", "Reading", reading, 20.0).clicked() && !reading {
            app.run("view.readingView", json!({}));
        }
        if widgets::icon_toggle(ui, app, "folder-pane", "Normal", !reading, 20.0).clicked() && reading {
            app.run("view.normalView", json!({}));
        }
        connection_state(ui, app, frame);
    });
}

/// Total messages waiting in the Outboxes of server accounts, and the first account with any.
fn outbox_waiting(app: &App) -> (usize, Option<u32>) {
    let st = app.s.sync_status();
    let total = st.accounts.iter().fold(0usize, |n, a| n.saturating_add(a.outbox));
    (total, st.accounts.iter().find(|a| a.outbox > 0).map(|a| a.account))
}

/// Left side of the status bar, after the item counts: what send/receive is doing ("Updating Inbox…", "All
/// folders are up to date.") or its error with a Retry button, and the Outbox badge. Clicking the text opens
/// Send/Receive Progress; the badge opens the Outbox.
fn sync_status(ui: &mut Ui, app: &mut App, frame: &Frame) {
    let st = app.s.sync_status().clone();
    if st.accounts.is_empty() {
        return;
    }
    let theme = app.theme.clone();
    let (outbox, outbox_account) = outbox_waiting(app);
    let error = st.accounts.iter().find_map(|a| a.error.clone());
    if st.busy {
        ui.add(egui::Spinner::new().size(12.0).color(theme.text_muted));
    }
    if !st.text.is_empty() && !st.busy && error.is_some() && !app.s.view.work_offline {
        let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
        app.icons.paint(ui, r, "error", theme.danger);
        // A failed send leaves the message in the Outbox: Retry sends again, otherwise it runs a full
        // send/receive.
        let (label, retry) = if outbox > 0 {
            (format!("{outbox} message(s) in the Outbox could not be sent"), "sendReceive.sendAll")
        } else {
            (st.text.clone(), "sendReceive.all")
        };
        let text = egui::RichText::new(truncate_status(&label, frame.width)).color(theme.danger);
        let r = ui.add(egui::Label::new(text).sense(Sense::click())).on_hover_text(st.text.as_str());
        if r.clicked() {
            app.run("sendReceive.showProgress", json!({}));
        }
        if ui.add(egui::Button::new("Retry").small()).on_hover_text("Try send/receive again").clicked() {
            app.run(retry, json!({}));
        }
    } else if !st.text.is_empty() {
        let r = ui
            .add(egui::Label::new(truncate_status(&st.text, frame.width)).sense(Sense::click()))
            .on_hover_text("Send/Receive Progress");
        if r.clicked() {
            app.run("sendReceive.showProgress", json!({}));
        }
    }
    if outbox > 0 && error.is_none() {
        let r = widgets::icon_button(ui, app, "outbox", &format!("Outbox: {outbox}"), true)
            .on_hover_text("Messages waiting to be sent");
        if r.clicked()
            && let Some(folder) = outbox_account.and_then(|a| {
                app.s.mailbox.role_folder(mailcraft_model::AccountId(a), mailcraft_model::FolderRole::Outbox)
            })
        {
            app.run("nav.selectFolder", json!({"folder": folder.0}));
        }
    }
}

/// Status text cut to fit narrow windows (char-safe).
fn truncate_status(text: &str, width: f32) -> String {
    let max = if width < 700.0 {
        28
    } else if width < 1000.0 {
        60
    } else {
        120
    };
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Right side of the status bar: "Connected", "Disconnected" or "Working offline". Clicking it toggles Work
/// Offline (the same command as Send / Receive › Work Offline).
fn connection_state(ui: &mut Ui, app: &mut App, frame: &Frame) {
    let st = app.s.sync_status();
    if st.accounts.is_empty() || frame.width < 600.0 {
        return;
    }
    let offline = app.s.view.work_offline;
    let text = if offline {
        "Working offline".to_owned()
    } else if st.connection_text.is_empty() {
        "Online".to_owned()
    } else {
        st.connection_text.clone()
    };
    ui.add_space(12.0);
    let tip = if offline { "Work Offline is on. Click to go online." } else { "Click to work offline." };
    let r = ui.add(egui::Button::new(text).frame(false).selected(offline)).on_hover_text(tip);
    if r.clicked() {
        app.run("sendReceive.workOffline", json!({"offline": offline}));
    }
}

// ---------------------------------------------------------------------------------------------------------
// Content: rounded panes on the backdrop

/// Paints a rounded pane at `rect` and runs `add` in a clipped child ui covering it.
fn pane(
    ui: &mut Ui,
    app: &mut App,
    rect: Rect,
    name: &'static str,
    fill: Color32,
    add: impl FnOnce(&mut Ui, &mut App),
) {
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    with_shell(ui.ctx(), |st| {
        st.layout.panes.push(name);
        st.layout.rects.push(rect);
    });
    let salt = format!("mc_pane_{name}");
    let stroke = Stroke::new(if app.theme.dark { 0.0 } else { 1.0 }, app.theme.separator);
    ui.painter().rect(rect, CornerRadius::same(size::PANE_RADIUS), fill, stroke, egui::StrokeKind::Inside);
    let mut c = child(ui, rect, &salt, Layout::top_down(Align::Min));
    add(&mut c, app);
}

/// A drag handle over the gap `rect`; returns the drag delta along its axis.
fn splitter(ui: &mut Ui, salt: &str, rect: Rect, horizontal_drag: bool, theme: &Theme) -> f32 {
    let resp = ui.interact(rect, ui.id().with(salt), Sense::drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(if horizontal_drag {
            egui::CursorIcon::ResizeHorizontal
        } else {
            egui::CursorIcon::ResizeVertical
        });
        let c = rect.center();
        let line = if horizontal_drag {
            [pos2(c.x, rect.top() + 8.0), pos2(c.x, rect.bottom() - 8.0)]
        } else {
            [pos2(rect.left() + 8.0, c.y), pos2(rect.right() - 8.0, c.y)]
        };
        ui.painter().line_segment(line, Stroke::new(2.0, theme.accent));
    }
    let d = resp.drag_delta();
    if horizontal_drag { d.x } else { d.y }
}

/// `v` clamped to `lo..=hi`, preferring `lo` when the range is empty (never panics).
fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    v.min(hi).max(lo)
}

fn content_area(ui: &mut Ui, app: &mut App, frame: &Frame) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, CornerRadius::ZERO, app.theme.backdrop);
    let gap = size::PANE_GAP;
    let inner = Rect::from_min_max(
        pos2(rect.left() + gap, rect.top() + 8.0),
        pos2(rect.right() - gap, rect.bottom() - 8.0),
    );
    if inner.width() < 1.0 || inner.height() < 1.0 {
        return;
    }
    let fill = app.theme.pane;
    match app.s.view.module {
        Module::Mail | Module::Folders | Module::Shortcuts => mail_panes(ui, app, frame, inner),
        Module::Calendar => pane(ui, app, inner, "calendar", fill, crate::modules::calendar),
        Module::People => pane(ui, app, inner, "people", fill, crate::modules::people),
        Module::Tasks => pane(ui, app, inner, "tasks", fill, crate::modules::tasks),
        Module::Notes => pane(ui, app, inner, "notes", fill, crate::modules::notes),
    }
}

fn mail_panes(ui: &mut Ui, app: &mut App, frame: &Frame, area: Rect) {
    let ctx = ui.ctx().clone();
    let theme = app.theme.clone();
    let split = size::PANE_SPLIT;
    let (mut folder_w, mut list_w, mut list_frac) = with_shell(&ctx, |st| {
        (
            st.folder_w.unwrap_or(size::FOLDER_PANE),
            st.list_w.unwrap_or(size::LIST),
            st.list_frac.unwrap_or(size::BOTTOM_LIST_FRACTION),
        )
    });

    // Folder pane: Normal (draggable), minimised strip (user setting, ≤ 800 wide, Reading view) or off.
    let view = &app.s.view;
    let minimized =
        view.folder_pane == FolderPaneState::Minimized || frame.folder_auto_minimized || view.reading_view;
    let folder_shown = match view.folder_pane {
        FolderPaneState::Off => 0.0,
        _ if minimized => size::FOLDER_PANE_MIN,
        _ => {
            clamp(folder_w, size::FOLDER_PANE_DRAG_MIN, (area.width() * 0.45).max(size::FOLDER_PANE_DRAG_MIN))
        }
    };
    let mut rest = area;
    if folder_shown > 0.0 {
        let fr = Rect::from_min_size(area.min, vec2(folder_shown.min(area.width()), area.height()));
        pane(ui, app, fr, "folders", theme.pane, |ui, app| crate::folders::folder_pane(ui, app, frame));
        let gap = Rect::from_min_max(pos2(fr.right(), area.top()), pos2(fr.right() + split, area.bottom()));
        if !minimized {
            let d = splitter(ui, "mc_split_folder", gap, true, &theme);
            if d != 0.0 {
                folder_w = clamp(folder_shown + d, size::FOLDER_PANE_DRAG_MIN, area.width() * 0.45);
            }
        }
        rest.min.x = gap.right().min(rest.max.x);
    }
    with_shell(&ctx, |st| {
        st.list_left = Some(rest.left());
        st.layout.folder_pane_width = folder_shown;
    });

    let list = |ui: &mut Ui, app: &mut App, r: Rect| {
        pane(ui, app, r, "list", theme.pane, |ui, app| crate::list::message_list(ui, app, frame));
    };
    let reading = |ui: &mut Ui, app: &mut App, r: Rect| {
        pane(ui, app, r, "reading", theme.reading, |ui, app| crate::reading::reading_pane(ui, app, frame));
    };

    if frame.single_pane {
        if app.ui.single_pane_reading && app.s.selection.focus.is_some() {
            reading(ui, app, rest);
        } else {
            list(ui, app, rest);
        }
    } else {
        match app.reading_pane_pos() {
            ReadingPanePos::Right => {
                let max_list = rest.width() - split - size::READING_MIN;
                let lw = clamp(
                    list_w,
                    size::LIST_MIN.min(rest.width()),
                    max_list.max(size::LIST_MIN.min(rest.width())),
                );
                let lr = Rect::from_min_size(rest.min, vec2(lw, rest.height()));
                list(ui, app, lr);
                let gap =
                    Rect::from_min_max(pos2(lr.right(), rest.top()), pos2(lr.right() + split, rest.bottom()));
                let d = splitter(ui, "mc_split_list", gap, true, &theme);
                if d != 0.0 {
                    list_w = clamp(lw + d, size::LIST_MIN, max_list.max(size::LIST_MIN));
                }
                let rr = Rect::from_min_max(pos2(gap.right(), rest.top()), rest.max);
                reading(ui, app, rr);
            }
            ReadingPanePos::Bottom => {
                let min_h = 120.0_f32.min(rest.height() / 2.0);
                let lh = clamp(rest.height() * list_frac, min_h, rest.height() - split - min_h);
                let lr = Rect::from_min_size(rest.min, vec2(rest.width(), lh));
                list(ui, app, lr);
                let gap = Rect::from_min_max(
                    pos2(rest.left(), lr.bottom()),
                    pos2(rest.right(), lr.bottom() + split),
                );
                let d = splitter(ui, "mc_split_bottom", gap, false, &theme);
                if d != 0.0 && rest.height() > 1.0 {
                    list_frac = ((lh + d) / rest.height()).clamp(0.1, 0.9);
                }
                let rr = Rect::from_min_max(pos2(rest.left(), gap.bottom()), rest.max);
                reading(ui, app, rr);
            }
            ReadingPanePos::Off => list(ui, app, rest),
        }
    }
    with_shell(&ctx, |st| {
        st.folder_w = Some(folder_w);
        st.list_w = Some(list_w);
        st.list_frac = Some(list_frac);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_mapping_round_trips() {
        for pct in [10_u16, 50, 100, 150, 300, 500] {
            assert_eq!(t_to_zoom(zoom_to_t(pct)), pct);
        }
        assert_eq!(t_to_zoom(0.5), 100);
        assert_eq!(t_to_zoom(-3.0), 10);
        assert_eq!(t_to_zoom(9.0), 500);
    }

    #[test]
    fn clamp_never_panics_on_empty_range() {
        assert_eq!(clamp(5.0, 10.0, 2.0), 10.0);
        assert!((1.0..=2.0).contains(&clamp(f32::NAN, 1.0, 2.0)));
    }

    #[test]
    fn qat_icons() {
        assert_eq!(command_icon("sendReceive.all"), "sync");
        assert_eq!(command_icon("app.undo"), "undo");
        assert_eq!(command_icon("nope"), "ellipsis");
    }
}
