//! Window chrome: title bar (QAT, search box, account), tab row, module rail, status bar (shell area), and the
//! main-window composition: bands from plan/reference/layout.md, rounded panes on a backdrop, splitters and the
//! narrow-window rules from plan/reference/responsive.md.
//!
//! Options that act here (engine settings, read every frame): General › Theme ("Use system setting" follows
//! the system), Advanced › animations, Quick Access Toolbar (show, position, labels), the pane sizes the
//! splitters leave (saved with the profile), the F6 pane focus ring. The shell also persists the dialogs'
//! own settings ([`sync_ui_prefs`]).

use egui::{Align, Align2, Color32, CornerRadius, Layout, Rect, Sense, Stroke, Ui, UiBuilder, pos2, vec2};
use serde_json::{Map, Value, json};

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

// ---------------------------------------------------------------------------------------------------------
// Settings bridge

/// Settings kept by dialogs for themselves (columns, folder descriptions, quick actions…) in egui memory
/// (`dialogs::common::pref`); `rev` moves with every write so [`sync_ui_prefs`] can save them.
#[derive(Clone, Debug, Default)]
pub(crate) struct UiPrefs {
    pub values: Map<String, Value>,
    pub rev: u64,
}

/// egui memory id of [`UiPrefs`].
pub(crate) const UI_PREFS_ID: &str = "mc-ui-prefs";

/// Older dialog-pref names that code outside the settings area still reads: (setting, pref).
const LEGACY_PREFS: [(&str, &str); 7] = [
    ("mail.arrival.playSound", "options.mail.arrivalSound"),
    ("mail.readingPane.markReadWhenViewed", "readingPane.markWhenViewed"),
    ("mail.readingPane.markReadAfterSeconds", "readingPane.waitSeconds"),
    ("mail.readingPane.markReadOnSelectionChange", "readingPane.markOnSelectionChange"),
    ("mail.readingPane.spaceBarReading", "readingPane.singleKey"),
    ("mail.readingPane.portraitFullScreen", "readingPane.portraitFullScreen"),
    ("mail.readingPane.alwaysPreview", "readingPane.alwaysPreview"),
];

fn is_mirror(key: &str) -> bool {
    key.starts_with("options.") || LEGACY_PREFS.iter().any(|(_, p)| *p == key)
}

/// Keys of the saved dialog settings that the engine writes itself (saved views, Quick Actions, status bar
/// items: `cmd::view::mail`). The shell copies them in but never writes them back.
fn is_engine(key: &str) -> bool {
    key.starts_with(mailcraft_engine::cmd::view::mail::PREF_PREFIX)
}

/// The engine-written part of the saved dialog settings.
fn engine_prefs(app: &App) -> Map<String, Value> {
    match app.s.settings.get("state.uiPrefs") {
        Some(Value::Object(saved)) => saved.into_iter().filter(|(k, _)| is_engine(k)).collect(),
        _ => Map::new(),
    }
}

/// What [`sync_ui_prefs`] last saw.
#[derive(Clone, Copy, Debug, Default)]
struct PrefSync {
    loaded: bool,
    settings_rev: Option<u64>,
    prefs_rev: u64,
}

/// Keeps the dialogs' settings and the engine settings in step (once per frame): restores the saved dialog
/// settings (`state.uiPrefs`) at start-up, mirrors every engine option in as `options.<key>` (read-only copies
/// for code that still reads dialog prefs) and saves dialog settings that changed.
pub(crate) fn sync_ui_prefs(ctx: &egui::Context, app: &mut App) {
    let sync_id = egui::Id::new("mc-ui-prefs-sync");
    let prefs_id = egui::Id::new(UI_PREFS_ID);
    let mut sync = ctx.data_mut(|d| d.get_temp::<PrefSync>(sync_id)).unwrap_or_default();
    let mut p = ctx.data_mut(|d| d.get_temp::<UiPrefs>(prefs_id)).unwrap_or_default();
    let mut touched = false;
    if !sync.loaded {
        if let Some(Value::Object(saved)) = app.s.settings.get("state.uiPrefs") {
            for (k, v) in saved {
                if !is_mirror(&k) {
                    p.values.entry(k).or_insert(v);
                }
            }
        }
        sync.loaded = true;
        touched = true;
    }
    if touched || p.rev != sync.prefs_rev {
        // A dialog changed its settings: save them with the profile (the engine's own keys as it has them).
        let mut own: Map<String, Value> = p
            .values
            .iter()
            .filter(|(k, _)| !is_mirror(k) && !is_engine(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        own.extend(engine_prefs(app));
        let own = Value::Object(own);
        if app.s.settings.get("state.uiPrefs").as_ref() != Some(&own)
            && let Err(e) = app.s.set_option("state.uiPrefs", &own)
        {
            log::warn!("dialog settings not saved: {e}");
        }
        sync.prefs_rev = p.rev;
    }
    if sync.settings_rev != Some(app.s.settings.rev()) {
        p.values.retain(|k, _| !is_engine(k));
        p.values.extend(engine_prefs(app));
        for d in mailcraft_engine::settings::DEFS {
            if d.page == "state" {
                continue;
            }
            if let Some(v) = app.s.option(d.key) {
                if let Some((_, legacy)) = LEGACY_PREFS.iter().find(|(k, _)| *k == d.key) {
                    p.values.insert((*legacy).to_owned(), v.clone());
                }
                p.values.insert(format!("options.{}", d.key), v);
            }
        }
        sync.settings_rev = Some(app.s.settings.rev());
        touched = true;
    }
    if touched {
        p.rev = p.rev.wrapping_add(1);
        sync.prefs_rev = p.rev;
        ctx.data_mut(|d| d.insert_temp(prefs_id, p));
    }
    ctx.data_mut(|d| d.insert_temp(sync_id, sync));
}

/// Display settings applied by the shell: what was last installed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct DisplayApplied {
    animations: Option<(bool, bool)>,
    /// A splitter moved; the sizes are saved once the pointer is released.
    panes_dirty: bool,
}

fn display_id() -> egui::Id {
    egui::Id::new("mc-display-settings")
}

/// General › Theme "Use system setting" (follows the system's light/dark preference) and Advanced › "Use
/// animations when expanding conversations and groups".
fn apply_display_settings(ctx: &egui::Context, app: &mut App) {
    if app.s.settings.text("general.theme") == "system"
        && let Some(sys) = ctx.system_theme()
    {
        let dark = sys == egui::Theme::Dark;
        if app.s.view.dark != dark {
            app.s.view.dark = dark;
            ctx.request_repaint();
        }
    }
    let anim = app.s.settings.bool("advanced.animations");
    let mut st = ctx.data_mut(|d| d.get_temp::<DisplayApplied>(display_id())).unwrap_or_default();
    // Installing a theme replaces the style, so animations are applied again after a theme change.
    if st.animations != Some((anim, app.theme.dark)) {
        let t = if anim { egui::Style::default().animation_time } else { 0.0 };
        ctx.all_styles_mut(|s| s.animation_time = t);
        st.animations = Some((anim, app.theme.dark));
        ctx.data_mut(|d| d.insert_temp(display_id(), st));
    }
}

/// Draws the F6 focus ring around `rect` when `name` is the focused pane.
fn focus_ring(ui: &Ui, app: &App, name: &str, rect: Rect) {
    if crate::keys::focused_pane(ui.ctx()).as_deref() == Some(name) {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(size::PANE_RADIUS),
            Stroke::new(2.0, app.theme.accent),
            egui::StrokeKind::Inside,
        );
    }
}

/// Keyboard focus (Tab, F6) on a chrome control (tab row, status bar): the accent outline the ribbon's
/// controls draw too.
fn paint_focus(ui: &Ui, app: &App, rect: Rect) {
    ui.painter().rect_stroke(
        rect.shrink(0.5),
        CornerRadius::same(4),
        Stroke::new(1.5, app.theme.accent),
        egui::StrokeKind::Inside,
    );
}

/// Where the main window drew its status bar and ribbon (tab row and ribbon body) in the last frame; `None`
/// when hidden. The keyboard handler takes the F6 stops from here.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bands {
    pub status_bar: Option<Rect>,
    pub ribbon: Option<Rect>,
}

fn bands_id() -> egui::Id {
    egui::Id::new("mailcraft.chrome.bands")
}

/// The status bar and ribbon rectangles of the main window's last frame.
pub fn bands(ctx: &egui::Context) -> Bands {
    ctx.data(|d| d.get_temp::<Bands>(bands_id())).unwrap_or_default()
}

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
    sync_ui_prefs(&ctx, app);
    apply_display_settings(&ctx, app);
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
    let mut bands = Bands::default();
    if show_tabs {
        let r = Rect::from_min_max(pos2(full.left(), top), pos2(full.right(), top + size::TAB_ROW));
        top = r.bottom();
        keep = keep.union(r);
        bands.ribbon = Some(r);
        let mut c = child(ui, r, "mc_tab_row", Layout::left_to_right(Align::Center));
        tab_row(&mut c, app, frame);
    }
    if show_ribbon {
        let h = if app.s.view.simplified_ribbon { size::RIBBON_SIMPLIFIED } else { size::RIBBON_CLASSIC };
        let r = Rect::from_min_max(pos2(full.left(), top), pos2(full.right(), (top + h).min(full.bottom())));
        top = r.bottom();
        keep = keep.union(r);
        bands.ribbon = Some(bands.ribbon.map_or(r, |b| b.union(r)));
        ui.painter().rect_filled(r, CornerRadius::ZERO, app.theme.ribbon);
        ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, app.theme.separator));
        let mut c = child(ui, r, "mc_ribbon", Layout::left_to_right(Align::Center));
        crate::ribbon::ribbon(&mut c, app, Surface::Main, frame);
        focus_ring(ui, app, "ribbon", r);
    }
    // Quick Access Toolbar › Toolbar Position: Below Ribbon.
    if qat_below(app) {
        let r = Rect::from_min_max(
            pos2(full.left(), top),
            pos2(full.right(), (top + size::TAB_ROW).min(full.bottom())),
        );
        top = r.bottom();
        keep = keep.union(r);
        ui.painter().rect_filled(r, CornerRadius::ZERO, app.theme.ribbon);
        ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, app.theme.separator));
        let mut c =
            child(ui, r.shrink2(vec2(8.0, 0.0)), "mc_qat_below", Layout::left_to_right(Align::Center));
        let theme = app.theme.clone();
        qat_buttons(&mut c, app, &theme);
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
        bands.status_bar = Some(r);
        let mut c = child(ui, r, "mc_status_bar", Layout::left_to_right(Align::Center));
        status_bar(&mut c, app, frame);
        focus_ring(ui, app, "statusBar", r);
    }
    // A ribbon shown only on demand (tabs only) is not an F6 stop while its tab row is hidden.
    if !show_tabs {
        bands.ribbon = None;
    }
    ctx.data_mut(|d| d.insert_temp(bands_id(), bands));

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
    // General › Personalize › Background: a faint pattern across the title bar.
    let ink = theme.chrome_text.gamma_multiply(0.12);
    ui.painter().extend(title_background(&app.s.settings.text("general.background"), bar, ink));

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

    // Quick Access Toolbar (Options › Quick Access Toolbar: shown, above the ribbon).
    let qat_rect = Rect::from_min_max(pos2(bar.left() + 42.0, bar.top()), pos2(bar.right(), bar.bottom()));
    let mut qat = child(ui, qat_rect, "mc_qat", Layout::left_to_right(Align::Center));
    let qat_end = if app.s.settings.bool("qat.show") && !qat_below(app) {
        qat_buttons(&mut qat, app, &theme);
        qat.min_rect().right()
    } else {
        qat_rect.left()
    };

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

/// General › Personalize › "Background": the title bar pattern (our own designs: "circles" = rings and
/// diagonal stripes, "geometry" = a band of triangles), drawn from the bar's right third leftwards in `ink`.
/// Nothing for "none".
pub(crate) fn title_background(kind: &str, bar: Rect, ink: Color32) -> Vec<egui::Shape> {
    let mut out = Vec::new();
    let h = bar.height();
    if h <= 1.0 || bar.width() <= 1.0 {
        return out;
    }
    let stroke = Stroke::new(1.2, ink);
    match kind {
        "circles" => {
            let center = pos2(bar.right() - h * 2.2, bar.center().y);
            for k in 1..=5 {
                out.push(egui::Shape::circle_stroke(center, h * 0.32 * k as f32, stroke));
            }
            let mut x = bar.right() - h * 6.0;
            while x < bar.right() {
                out.push(egui::Shape::line_segment([pos2(x, bar.bottom()), pos2(x + h, bar.top())], stroke));
                x += h * 0.45;
            }
        }
        "geometry" => {
            let side = h * 0.9;
            let mut x = bar.right() - side * 10.0;
            let mut up = true;
            while x + side < bar.right() + side {
                let (a, b, c) = if up {
                    (pos2(x, bar.bottom()), pos2(x + side, bar.bottom()), pos2(x + side / 2.0, bar.top()))
                } else {
                    (pos2(x, bar.top()), pos2(x + side, bar.top()), pos2(x + side / 2.0, bar.bottom()))
                };
                out.push(egui::Shape::closed_line(vec![a, b, c], stroke));
                x += side / 2.0;
                up = !up;
            }
        }
        _ => {}
    }
    out
}

/// Options › Quick Access Toolbar › Toolbar Position: Below Ribbon (and the toolbar shown).
fn qat_below(app: &App) -> bool {
    app.s.settings.bool("qat.show") && app.s.settings.text("qat.position") == "below"
}

/// The Quick Access Toolbar's buttons and its customise menu (labels shown when the option says so).
fn qat_buttons(qat: &mut Ui, app: &mut App, theme: &Theme) {
    qat.spacing_mut().item_spacing.x = 1.0;
    let keytips = app.ui.keytips.as_deref() == Some("");
    let labels = app.s.settings.bool("qat.showLabels");
    for (i, id) in app.s.view.quick_access.clone().into_iter().enumerate() {
        let label = command_label(app, &id);
        let enabled = app.s.enabled(&id).is_none();
        let r = if labels {
            widgets::icon_button(qat, app, command_icon(&id), &label, enabled)
        } else {
            widgets::icon_only(qat, app, command_icon(&id), &label, enabled)
        };
        if keytips && i < 9 {
            keytip_badge(qat, r.rect, &(i + 1).to_string(), theme);
        }
        if r.clicked() {
            app.run(&id, json!({}));
        }
    }
    let custom = widgets::icon_only(qat, app, "chevron-down", "Customize Quick Access Toolbar", true);
    egui::Popup::menu(&custom).show(|ui| qat_menu(ui, app));
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
        SearchScope::AllItems => "All Items",
    }
}

const SCOPES: [(SearchScope, &str); 5] = [
    (SearchScope::AllMailboxes, "allMailboxes"),
    (SearchScope::CurrentMailbox, "currentMailbox"),
    (SearchScope::CurrentFolder, "currentFolder"),
    (SearchScope::Subfolders, "subfolders"),
    (SearchScope::AllItems, "allItems"),
];

/// Keyboard shortcut shown next to a scope in the dropdown (`plan/reference/keyboard.md`).
fn scope_shortcut(scope: SearchScope) -> &'static str {
    match scope {
        SearchScope::AllMailboxes => "Ctrl+Alt+A",
        SearchScope::CurrentFolder => "Ctrl+Alt+K",
        SearchScope::Subfolders => "Ctrl+Alt+Z",
        SearchScope::CurrentMailbox | SearchScope::AllItems => "",
    }
}

/// Recent Searches under the focused, empty search box: shown on focus, closed by a pick, typing, Escape or
/// a click elsewhere. A pick runs `search.recent` (which also moves it to the top of the list).
fn recent_searches_popup(ctx: &egui::Context, app: &mut App, edit: &egui::Response, rect: Rect) {
    let id = egui::Id::new("mailcraft_search_recent");
    let mut open = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    if edit.gained_focus() && app.ui.search_text.is_empty() && !app.s.search.recent.is_empty() {
        open = true;
    }
    if !app.ui.search_text.is_empty() || app.s.search.recent.is_empty() {
        open = false;
    }
    // Up / Down move a highlight through the list while the box has the keyboard; Enter runs the highlighted
    // search.
    let hi_id = id.with("highlight");
    let mut hi = ctx.data(|d| d.get_temp::<Option<usize>>(hi_id)).flatten();
    if open {
        let n = app.s.search.recent.len();
        if edit.has_focus() || edit.lost_focus() {
            let (down, up) =
                ctx.input(|i| (i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::ArrowUp)));
            if down {
                hi = Some(hi.map_or(0, |h| h.saturating_add(1).min(n.saturating_sub(1))));
            }
            if up {
                hi = hi.and_then(|h| h.checked_sub(1));
            }
        }
        hi = hi.filter(|h| *h < n);
        let enter = hi.filter(|_| ctx.input(|i| i.key_pressed(egui::Key::Enter)));
        let shown = egui::Area::new(id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(rect.left(), rect.bottom() + 2.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .show(ui, |ui| {
                        ui.set_min_width(rect.width());
                        ui.label(egui::RichText::new("Recent Searches").weak());
                        let mut picked = None;
                        for (i, q) in app.s.search.recent.iter().enumerate() {
                            let label: String = q.chars().take(80).collect();
                            let button = egui::Button::new(label).frame(false).selected(hi == Some(i));
                            if ui.add(button).clicked() {
                                picked = Some(i);
                            }
                        }
                        picked
                    })
                    .inner
            });
        if let Some(i) = shown.inner.or(enter) {
            app.run("search.recent", json!({ "index": i }));
            app.ui.search_text = app.s.view.search.clone();
            app.ui.search_active = true;
            open = false;
        } else {
            let clicked_out = ctx.input(|i| i.pointer.any_click())
                && !shown.response.contains_pointer()
                && !edit.contains_pointer();
            if clicked_out || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                open = false;
            }
        }
    }
    if !open {
        hi = None;
    }
    ctx.data_mut(|d| {
        d.insert_temp(id, open);
        d.insert_temp(hi_id, hi);
    });
}

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
        // A refiner waiting for a value: the caret (or selection) goes inside its quotes.
        if let Some((a, b)) = with_shell(&ctx, |st| st.search_select.take()) {
            let mut state = egui::TextEdit::load_state(&ctx, edit_id).unwrap_or_default();
            state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(a),
                egui::text::CCursor::new(b),
            )));
            state.store(&ctx, edit_id);
            ctx.request_repaint();
        }
    }
    if edit.gained_focus() {
        app.ui.search_active = true;
        app.ui.set_tab(Surface::Main, "Search");
    }
    if edit.changed() {
        let text = app.ui.search_text.clone();
        app.run("search.query", json!({ "text": text }));
    }
    // Enter runs the search for good: it goes into Recent Searches.
    if edit.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
        let text = app.ui.search_text.clone();
        app.run("search.query", json!({ "text": text, "commit": true }));
    }
    recent_searches_popup(&ctx, app, &edit, rect);
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
                let button =
                    egui::Button::new(scope_label(scope)).selected(on).shortcut_text(scope_shortcut(scope));
                if ui.add(button).clicked() {
                    app.run("search.scope", json!({ "scope": key }));
                    ui.close();
                }
            }
            ui.separator();
            let deleted = app.s.search.include_deleted;
            if ui.add(egui::Button::new("Include Deleted Items").selected(deleted)).clicked() {
                app.run("search.scope", json!({ "includeDeleted": !deleted }));
                ui.close();
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
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink2(vec2(0.5, 2.5)),
            CornerRadius::same(4),
            Stroke::new(1.5, theme.accent),
            egui::StrokeKind::Inside,
        );
    }
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
        // The People peek: hovering People shows the people mailed most.
        let r = if module == Module::People { r.on_hover_ui(|ui| people_peek(ui, app)) } else { r };
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

/// Title of the People peek.
pub const PEOPLE_PEEK: &str = "People you mail most";

/// The People peek's contents ([`mailcraft_engine::cmd::people::peek_people`]): a name per person, plus
/// their job title and company or address unless People › "Show only names in the People peek" is on.
pub(crate) fn people_peek(ui: &mut Ui, app: &mut App) {
    let people = mailcraft_engine::cmd::people::peek_people(&app.s);
    ui.set_min_width(200.0);
    ui.label(egui::RichText::new(PEOPLE_PEEK).strong());
    if people.is_empty() {
        ui.label(egui::RichText::new("People you send mail to show up here.").color(app.theme.text_muted));
        return;
    }
    for p in people {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(&p.name).color(app.theme.text));
        if let Some(d) = &p.detail {
            ui.label(egui::RichText::new(d).size(11.5).color(app.theme.text_muted));
        }
    }
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

/// The status bar (`plan/reference/layout.md`, `message-list.md` › Customize Status Bar): items in view, unread,
/// header items, filter and reminders at the left, then send/receive progress; view shortcuts and the zoom
/// slider and percentage at the right. Each element can be hidden from the right-click menu (`view.statusBar`,
/// saved with the profile). Every part stays down to the 400-px minimum window (`responsive.md`); only the
/// slider gets shorter.
pub fn status_bar(ui: &mut Ui, app: &mut App, frame: &Frame) {
    let bar = ui.max_rect();
    let theme = app.theme.clone();
    ui.painter().rect_filled(bar, CornerRadius::ZERO, theme.ribbon);
    ui.painter().hline(bar.x_range(), bar.top() + 0.5, Stroke::new(1.0, theme.separator));
    let bg = ui.interact(bar, ui.id().with("mc_status_bg"), Sense::click());
    bg.context_menu(|ui| crate::menus::status_bar_menu(ui, app));
    let items = crate::menus::status_bar_items_of(ui.ctx(), app);

    ui.visuals_mut().override_text_color = Some(theme.text_muted);
    ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));
    ui.spacing_mut().interact_size.y = 20.0;
    ui.spacing_mut().item_spacing.x = if frame.width < 600.0 { 8.0 } else { 14.0 };
    ui.add_space(10.0);

    if app.mail_like_module() {
        let s = &app.s;
        let (count, unread) = mailcraft_engine::queries::status_counts(s);
        if items.shown("itemsInView") {
            ui.label(format!("Items: {}", crate::menus::thousands(count)));
        }
        if items.shown("unreadItems") && unread > 0 {
            ui.label(format!("Unread: {}", crate::menus::thousands(unread)));
        }
        if items.shown("headerItems") {
            let headers = mailcraft_engine::queries::header_items_in_view(s);
            if headers > 0 {
                ui.label(format!("Header Items: {}", crate::menus::thousands(headers)));
            }
        }
        if items.shown("filter") && crate::menus::filter_applied(app) {
            ui.label("Filter applied");
        }
        if items.shown("unseenItems") {
            let unseen = mailcraft_engine::queries::unseen_in_view(&app.s);
            if unseen > 0 {
                ui.label(format!("Unseen: {}", crate::menus::thousands(unseen)))
                    .on_hover_text("Items that arrived since you last opened this folder");
            }
        }
    }
    if items.shown("quota")
        && let Some((name, q)) = mailcraft_engine::sync::status_quota(&app.s)
    {
        quota_meter(ui, app, &name, &q);
    }
    // Mail › Message arrival › "Show an envelope icon in the taskbar": the in-app envelope (the window icon
    // carries it too, `App::sync_envelope`); clicking it opens the folder of the newest new message.
    let new = mailcraft_engine::new_mail::envelope(&app.s);
    if new > 0 {
        let label = if new == 1 { "1 new message".to_owned() } else { format!("{new} new messages") };
        if widgets::icon_button(ui, app, "new-mail", &label, true).clicked()
            && let Some(f) = mailcraft_engine::new_mail::envelope_folder(&app.s)
        {
            app.run("nav.selectFolder", json!({ "folder": f.0 }));
        }
    }
    // The Reminders window's last scan (also counts flagged mail); before its first scan, a direct count.
    let due = reminders_due(app).max(crate::modules::reminders_due(ui.ctx()));
    if due > 0 && items.shown("reminders") {
        let r = widgets::icon_button(ui, app, "bell", &format!("Reminders: {due}"), true);
        if r.clicked() {
            app.run("view.reminderWindow", json!({}));
        }
    }
    sync_status(ui, app, frame);

    // Right side: zoom percentage, zoom slider, view shortcuts, then the connection state.
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.add_space(8.0);
        let zoom = app.s.view.zoom;
        if items.shown("zoom")
            && ui.add(egui::Button::new(format!("{zoom}%")).frame(false)).on_hover_text("Zoom").clicked()
        {
            app.run("view.zoom", json!({}));
        }
        if items.shown("zoomSlider") {
            if widgets::icon_only(ui, app, "zoom-in", "Zoom In", zoom < 500).clicked() {
                let next = (zoom / 10 + 1).saturating_mul(10).min(500);
                app.run("view.zoom", json!({ "pct": next }));
            }
            let mut t = zoom_to_t(zoom);
            ui.spacing_mut().slider_width = if frame.width < 600.0 { 56.0 } else { 96.0 };
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
        }
        if items.shown("zoom") || items.shown("zoomSlider") {
            ui.add_space(if frame.width < 600.0 { 4.0 } else { 10.0 });
        }
        if items.shown("viewShortcuts") {
            let reading = app.s.view.reading_view;
            if widgets::icon_toggle(ui, app, "reading-pane", "Reading", reading, 20.0).clicked() && !reading {
                app.run("view.readingView", json!({}));
            }
            if widgets::icon_toggle(ui, app, "folder-pane", "Normal", !reading, 20.0).clicked() && reading {
                app.run("view.normalView", json!({}));
            }
        }
        connection_state(ui, app, frame);
    });
    // The control with keyboard focus (Tab, F6 lands on the first one) gets the accent outline.
    let ctx = ui.ctx().clone();
    if let Some(id) = ctx.memory(|m| m.focused()).filter(|id| *id != bg.id)
        && let Some(r) = ctx.read_response(id)
        && bar.contains(r.rect.center())
    {
        paint_focus(ui, app, r.rect.intersect(bar));
    }
}

/// Quota Information: "Mailbox 42%" and a small meter (red from 90 %); the hover tells the sizes.
fn quota_meter(ui: &mut Ui, app: &App, account: &str, q: &mailcraft_engine::MailboxQuota) {
    let Some(pct) = q.percent() else { return };
    let theme = &app.theme;
    let full = pct >= 90;
    let color = if full { theme.danger } else { theme.text_muted };
    let hover = format!("{account}: {}", q.text());
    ui.label(egui::RichText::new(format!("Mailbox {pct}% full")).color(color)).on_hover_text(hover.as_str());
    let (r, resp) = ui.allocate_exact_size(vec2(48.0, 6.0), Sense::hover());
    ui.painter().rect_filled(r, CornerRadius::same(3), theme.separator);
    let mut fill = r;
    fill.set_width(r.width() * f32::from(pct) / 100.0);
    ui.painter().rect_filled(fill, CornerRadius::same(3), if full { theme.danger } else { theme.accent });
    resp.on_hover_text(hover);
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
        let text = match st.detail.as_ref().filter(|_| st.busy) {
            Some(d) => progress_status(d, &st.text, frame.width),
            None => truncate_status(&st.text, frame.width),
        };
        let tip = if text == st.text { "Send/Receive Progress" } else { st.text.as_str() };
        let r = ui.add(egui::Label::new(text).sense(Sense::click())).on_hover_text(tip);
        if r.clicked() {
            app.run("sendReceive.showProgress", json!({}));
        }
        if let Some(f) =
            st.detail.as_ref().filter(|_| st.busy && frame.width >= 1000.0).and_then(|d| d.fraction())
        {
            let bar = egui::ProgressBar::new(f).desired_width(90.0).desired_height(8.0).fill(theme.accent);
            ui.add(bar).on_hover_text(st.text.as_str());
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

/// Download progress for the status bar: the whole text ("Downloading Inbox: 1,240 of 5,000 messages (folder
/// 3 of 12)") when it fits, else the counts alone ("Inbox: 1,240 of 5,000"), so a narrow window still shows
/// how far the download has got.
fn progress_status(d: &mailcraft_engine::sync::SyncProgress, full: &str, width: f32) -> String {
    let fits = truncate_status(full, width);
    if fits == full || d.total == 0 {
        return fits;
    }
    let counts = format!(
        "{} of {}",
        mailcraft_engine::sync::grouped(d.done.min(d.total)),
        mailcraft_engine::sync::grouped(d.total)
    );
    if d.folder.is_empty() {
        return truncate_status(&counts, width);
    }
    // A long folder name gives way to the counts.
    let room = status_max(width).saturating_sub(counts.chars().count().saturating_add(2)).max(4);
    format!("{}: {counts}", truncate_status_to(&d.folder, room))
}

/// Status text cut to fit narrow windows (char-safe).
fn truncate_status(text: &str, width: f32) -> String {
    truncate_status_to(text, status_max(width))
}

/// Characters of status text that fit a window `width` px wide.
fn status_max(width: f32) -> usize {
    if width < 700.0 {
        28
    } else if width < 1000.0 {
        60
    } else {
        120
    }
}

/// `text` cut to `max` characters, ending in "…" when cut (char-safe).
fn truncate_status_to(text: &str, max: usize) -> String {
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
    focus_ring(ui, app, name, rect);
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
        Module::Mail | Module::Folders | Module::Shortcuts => {
            // To-Do Bar (View › Layout › To-Do Bar) on the right of the mail panes, when there is room.
            let bar_w = crate::modules::TODO_BAR_WIDTH;
            if crate::modules::todo_bar_shown(app) && inner.width() >= bar_w + size::TODO_BAR_MIN_MAIL {
                let split_x = inner.right() - bar_w;
                let mail = Rect::from_min_max(inner.min, pos2(split_x - gap, inner.bottom()));
                let bar = Rect::from_min_max(pos2(split_x, inner.top()), inner.max);
                mail_panes(ui, app, frame, mail);
                pane(ui, app, bar, "todoBar", fill, crate::modules::todo_bar);
            } else {
                mail_panes(ui, app, frame, inner);
            }
        }
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
    // Pane sizes the splitters left last time (saved with the profile; 0 = default).
    let saved = |key: &str| Some(app.s.settings.int(key)).filter(|v| *v > 0).map(|v| v as f32);
    let saved_folder = saved("state.folderPaneWidth");
    let saved_list = saved("state.listWidth");
    let saved_frac = saved("state.listFractionPermille").map(|p| p / 1000.0);
    let (mut folder_w, mut list_w, mut list_frac) = with_shell(&ctx, |st| {
        (
            st.folder_w.or(saved_folder).unwrap_or(size::FOLDER_PANE),
            st.list_w.or(saved_list).unwrap_or(size::LIST),
            st.list_frac.or(saved_frac).unwrap_or(size::BOTTOM_LIST_FRACTION),
        )
    });
    let before = (folder_w, list_w, list_frac);

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
    save_pane_sizes(&ctx, app, before != (folder_w, list_w, list_frac), (folder_w, list_w, list_frac));
}

/// Saves the splitter positions once a drag ends.
fn save_pane_sizes(
    ctx: &egui::Context,
    app: &mut App,
    moved: bool,
    (folder_w, list_w, list_frac): (f32, f32, f32),
) {
    let mut st = ctx.data_mut(|d| d.get_temp::<DisplayApplied>(display_id())).unwrap_or_default();
    if moved {
        st.panes_dirty = true;
    }
    if st.panes_dirty && !ctx.input(|i| i.pointer.any_down()) {
        st.panes_dirty = false;
        let values = [
            ("state.folderPaneWidth", json!(folder_w.round().clamp(0.0, 4000.0) as i64)),
            ("state.listWidth", json!(list_w.round().clamp(0.0, 8000.0) as i64)),
            ("state.listFractionPermille", json!((list_frac * 1000.0).round().clamp(1.0, 1000.0) as i64)),
        ];
        for (k, v) in values {
            if app.s.option(k).as_ref() != Some(&v)
                && let Err(e) = app.s.set_option(k, &v)
            {
                log::warn!("pane size not saved: {e}");
            }
        }
    }
    ctx.data_mut(|d| d.insert_temp(display_id(), st));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rectangles outlined with the accent keyboard-focus stroke in the last frame.
    fn focus_outlines(h: &egui_kittest::Harness<'_, App>) -> Vec<Rect> {
        fn walk(shape: &egui::Shape, accent: egui::Color32, out: &mut Vec<Rect>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, accent, out)),
                egui::Shape::Rect(r) if r.stroke.color == accent && (r.stroke.width - 1.5).abs() < 0.01 => {
                    out.push(r.rect);
                }
                _ => {}
            }
        }
        let accent = h.state().theme.accent;
        let mut out = Vec::new();
        for c in &h.output().shapes {
            walk(&c.shape, accent, &mut out);
        }
        out
    }

    #[test]
    fn bands_and_keyboard_focus_on_tabs_and_status_bar() {
        use egui_kittest::kittest::Queryable;
        let app = App::new(mailcraft_engine::sample::sample_session());
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .build_ui_state(|ui, app: &mut App| app.ui_root(ui), app);
        h.run();
        h.run();
        // The shell records where it drew the ribbon (tab row + body) and the status bar.
        let b = bands(&h.ctx);
        let ribbon = b.ribbon.unwrap();
        assert_eq!(ribbon.top(), size::TITLE_BAR);
        let body =
            if h.state().s.view.simplified_ribbon { size::RIBBON_SIMPLIFIED } else { size::RIBBON_CLASSIC };
        assert!((ribbon.height() - (size::TAB_ROW + body)).abs() < 0.5, "{ribbon:?}");
        let status = b.status_bar.unwrap();
        assert!((status.bottom() - 900.0).abs() < 0.5 && (status.height() - size::STATUS_BAR).abs() < 0.5);
        assert!(focus_outlines(&h).is_empty(), "nothing focused, no outline");
        // A focused tab (F6 lands on the selected one) shows the outline in the tab row.
        let tab_band = Rect::from_min_size(ribbon.min, egui::vec2(ribbon.width(), size::TAB_ROW));
        h.get_by_label("View").focus();
        h.run();
        h.run();
        let outlines = focus_outlines(&h);
        assert!(outlines.iter().any(|r| tab_band.contains_rect(*r)), "{outlines:?}");
        // So does a focused status-bar control.
        h.get_by_label("Zoom In").focus();
        h.run();
        h.run();
        let outlines = focus_outlines(&h);
        assert!(outlines.iter().any(|r| status.contains_rect(*r)), "{outlines:?}");
        assert!(!outlines.iter().any(|r| tab_band.contains_rect(*r)));
        // Hiding the status bar and the ribbon removes their bands.
        h.state_mut().s.view.status_bar = false;
        h.state_mut().run("app.toggleRibbonMode", json!({"mode": "autoHide"}));
        h.run();
        h.run();
        let b = bands(&h.ctx);
        assert_eq!(b.status_bar, None, "{b:?}");
        assert_eq!(b.ribbon, None, "{b:?}");
    }

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
    fn dialog_settings_are_saved_and_options_mirrored() {
        let ctx = egui::Context::default();
        let mut app = App::new(mailcraft_engine::sample::sample_session());
        sync_ui_prefs(&ctx, &mut app);
        // Engine options appear as read-only copies for older readers.
        assert_eq!(crate::dialogs::ui_pref(&ctx, "options.search.highlight"), Some(json!(true)));
        app.run("options.set", json!({"key": "search.highlight", "value": false}));
        sync_ui_prefs(&ctx, &mut app);
        assert_eq!(crate::dialogs::ui_pref(&ctx, "options.search.highlight"), Some(json!(false)));
        assert!(!crate::dialogs::search_highlight_on(&app.s), "the list reads the setting itself");
        assert_eq!(crate::dialogs::ui_pref(&ctx, "readingPane.singleKey"), Some(json!(true)));
        // A dialog's own setting is saved in the profile settings (not the mirrors).
        let id = egui::Id::new(UI_PREFS_ID);
        let mut p = ctx.data_mut(|d| d.get_temp::<UiPrefs>(id)).unwrap_or_default();
        p.values.insert("columns.7".into(), json!(["from", "subject"]));
        p.rev += 1;
        ctx.data_mut(|d| d.insert_temp(id, p));
        sync_ui_prefs(&ctx, &mut app);
        let saved = app.s.settings.get("state.uiPrefs").unwrap();
        assert_eq!(saved["columns.7"], json!(["from", "subject"]));
        assert!(saved.get("options.search.highlight").is_none());
        // A new window (fresh egui memory) gets it back.
        let ctx2 = egui::Context::default();
        sync_ui_prefs(&ctx2, &mut app);
        assert_eq!(crate::dialogs::ui_pref(&ctx2, "columns.7"), Some(json!(["from", "subject"])));
    }

    #[test]
    fn status_bar_items_follow_the_engine_and_keep_zoom_when_narrow() {
        use egui_kittest::kittest::Queryable;
        let app = App::new(mailcraft_engine::sample::sample_session());
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(400.0, 30.0)).build_ui_state(
            |ui, app: &mut App| {
                let frame = Frame::compute(400.0, 400.0);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| status_bar(ui, app, &frame));
            },
            app,
        );
        h.run();
        assert!(h.query_by_label("Zoom In").is_some(), "the zoom slider stays at 400 px (responsive.md)");
        assert!(h.query_by_label("Normal").is_some(), "view shortcuts");
        assert!(h.query_all_by_label_contains("Items: ").next().is_some());
        h.state_mut().run("view.statusBar", json!({"item": "zoomSlider", "show": false}));
        h.state_mut().run("view.statusBar", json!({"item": "itemsInView", "show": false}));
        h.state_mut().run("view.statusBar", json!({"item": "viewShortcuts", "show": false}));
        h.run();
        assert!(h.query_by_label("Zoom In").is_none());
        assert!(h.query_by_label("Normal").is_none());
        assert!(h.query_all_by_label_contains("Items: ").next().is_none());
        assert!(!crate::menus::status_bar_item_shown(&h.ctx, "zoomSlider"), "the menu reads the same state");
        // Saved with the profile settings; the shell's own dialog settings leave it alone.
        let ctx = h.ctx.clone();
        sync_ui_prefs(&ctx, h.state_mut());
        let id = egui::Id::new(UI_PREFS_ID);
        let mut p = ctx.data_mut(|d| d.get_temp::<UiPrefs>(id)).unwrap_or_default();
        p.values.insert("columns.7".into(), json!(["a"]));
        // A stale copy of the engine's key in the shell's memory must not overwrite the engine's value.
        p.values.insert("view.statusBar.hidden".into(), json!([]));
        p.rev += 1;
        ctx.data_mut(|d| d.insert_temp(id, p));
        sync_ui_prefs(&ctx, h.state_mut());
        let saved = h.state().s.settings.get("state.uiPrefs").unwrap();
        assert!(saved.get("view.statusBar.hidden").is_some());
        assert_eq!(saved["columns.7"], json!(["a"]));
        assert!(!mailcraft_engine::cmd::view::mail::status_item_shown(&h.state().s, "zoomSlider"));
    }

    #[test]
    fn reading_view_hides_the_pinned_todo_bar_and_normal_view_brings_it_back() {
        use egui_kittest::kittest::Queryable;
        let mut app = App::new(mailcraft_engine::sample::sample_session());
        app.run("view.todoBar", json!({"part": "tasks"}));
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1600.0, 900.0)).build_ui_state(
            |ui, app: &mut App| {
                let frame = Frame::compute(1600.0, 900.0);
                main_window(ui, app, &frame);
            },
            app,
        );
        h.run();
        assert!(h.query_by_label("Type a new task").is_some(), "the pinned To-Do Bar shows");
        h.state_mut().run("view.readingView", json!({}));
        h.run();
        assert!(h.query_by_label("Type a new task").is_none(), "Reading View hides it");
        h.state_mut().run("view.normalView", json!({}));
        h.run();
        assert!(h.query_by_label("Type a new task").is_some(), "Normal View shows it again");
    }

    #[test]
    fn quick_access_toolbar_options() {
        let mut app = App::new(mailcraft_engine::sample::sample_session());
        assert!(!qat_below(&app));
        app.run("options.set", json!({"key": "qat.position", "value": "below"}));
        assert!(qat_below(&app));
        app.run("options.set", json!({"key": "qat.show", "value": false}));
        assert!(!qat_below(&app));
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_ui_state(
            |ui, app: &mut App| {
                let frame = Frame::compute(1200.0, 800.0);
                main_window(ui, app, &frame);
            },
            app,
        );
        h.run();
        use egui_kittest::kittest::Queryable;
        assert!(h.query_by_label("Customize Quick Access Toolbar").is_none(), "hidden");
        h.state_mut().run("options.set", json!({"key": "qat.show", "value": true}));
        h.state_mut().run("options.set", json!({"key": "qat.showLabels", "value": true}));
        h.run();
        assert!(h.query_by_label("Customize Quick Access Toolbar").is_some());
    }

    #[test]
    fn qat_icons() {
        assert_eq!(command_icon("sendReceive.all"), "sync");
        assert_eq!(command_icon("app.undo"), "undo");
        assert_eq!(command_icon("nope"), "ellipsis");
    }
}
