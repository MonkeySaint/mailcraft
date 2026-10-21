//! Compose windows (viewports) and inline responses in the reading pane, with the body editor (compose area).
//!
//! Layout follows plan/reference/compose.md, reading-pane.md (inline reply) and responsive.md (compose window):
//!
//! - Each non-inline [`ComposeState`] is an immediate viewport (default 1440×780, title from
//!   [`ComposeState::title`], OS title bar). Under it: a Quick Access row (Save, Undo, Redo, Previous Item, Next
//!   Item), the tab row (File + the compose tabs) and the ribbon from [`crate::ribbon::ribbon`].
//! - Header: a big Send button (60×76) spanning the first two address rows; To / Cc buttons (80×32) that open the
//!   address book; Subject label; optional From and Bcc rows; the attachments well; a 1-px separator; the body.
//! - At 300×200 (anything below ~360×260, `responsive::inspector_tiny`, shared with read windows) the Quick
//!   Access row, tab row and ribbon disappear; the header stays.
//! - While any compose has unsaved changes, `compose.autoSaveTick` runs every [`AUTO_SAVE_CHECK_SECS`]
//!   seconds ([`auto_save_timer`]); the engine saves the drafts whose changes are older than the Options ›
//!   Mail auto-save minutes (nothing when auto-save is off).
//! - Closing a window with unsaved changes asks "Do you want to save changes?" (Yes / No / Cancel) →
//!   `compose.close {"save": true|false}`.
//! - Inline responses: the same header plus Discard and Pop Out at the top right, the body editor below.
//!
//! Every change goes through `App::run` (`compose.*`, `format.*`, `nav.*`); header fields run their `compose.set*`
//! command once per frame when the text changed.
//!
//! Submodules: `editor` (the body editor), `recipients` (To / Cc / Bcc: Auto-Complete, resolved names, Ctrl+K
//! Check Names), `attach` (attachments well, Browse This PC…, recent files, drag and drop, file hooks),
//! `spelling` (the F7 Spelling and Grammar dialog), `clipboard` (the system clipboard's HTML flavour for rich
//! paste and copy, and a picture reader for picture paste), `voting` (reading-pane voting, read-receipt and
//! tracking bars), `panes` (the Clipboard, Styles and Selection panes beside the body), `minibar` (the Mini
//! Toolbar over a mouse selection), `mention` (the @mention list) and `a11y` (the accessibility warning bar
//! under the header).

mod a11y;
mod attach;
pub(crate) mod capture;
mod clipboard;
mod drawing;
#[cfg(test)]
mod drawing_tests;
mod editor;
mod fonts;
#[cfg(test)]
mod insert_tests;
mod mention;
mod minibar;
mod panes;
mod recipients;
mod spelling;

/// Hands half-typed drawing text of compose `cid` to the engine before another command runs (`App::run`).
pub(crate) fn flush_drawing_text(ctx: &egui::Context, app: &mut App, cid: u64) {
    drawing::flush(ctx, app, cid);
}

/// Whether the keyboard focus is in the text of a text box, shape or WordArt in a compose body.
pub(crate) fn drawing_text_focused(ctx: &egui::Context) -> bool {
    drawing::text_focused(ctx)
}
#[cfg(test)]
mod tests;
mod voting;

use egui::{
    Align, Align2, CornerRadius, Id, Layout, Rect, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2,
    ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, WidgetInfo, WidgetType, pos2, vec2,
};
use serde_json::json;

use mailcraft_engine::compose::{ComposeKind, ComposeState};

use crate::responsive::Frame;
use crate::{App, Surface};

pub use a11y::BAR_LABEL as ACCESSIBILITY_BAR_LABEL;
pub use attach::{
    DROP_HINT, FileHooks, MAX_RECENT, SAVE_TITLE, file_hooks, file_name as recent_label, recent_files,
    remember_file, set_file_hooks, size_text,
};
pub(crate) use capture::own_window_title;
pub use capture::{
    CaptureWindow, Captured, MAIN_WINDOW_TITLE, NO_BACKEND as NO_CAPTURE_BACKEND, ScreenCapture,
    capture_backend, set_capture, window_items as screenshot_windows,
};
pub use clipboard::{
    ClipboardHooks, WATCH_SECS as CLIPBOARD_WATCH_SECS, clipboard_hooks, clipboard_html, clipboard_image,
    clipboard_text, clipboard_watch, copy_html, html_for_paste, set_clipboard_hooks,
    set_clipboard_image_reader,
};
pub use editor::{
    BODY_LABEL, NO_SUGGESTIONS, PASTE_MODES, body_focused, editor_id, refocus_body, request_live_preview,
    request_paste,
};
pub(crate) use editor::{EditorHost, body_editor_in};
pub use fonts::{
    InstalledFonts, MAX_FONT_NAMES, font_names, installed_fonts, merge_font_names, set_installed_fonts_reader,
};
pub use mention::{LIST_LABEL as MENTION_LIST_LABEL, is_open as mention_list_open};
pub use minibar::{LABEL as MINI_TOOLBAR_LABEL, is_showing as mini_toolbar_showing};
pub use panes::{
    CLIPBOARD_EMPTY, CLIPBOARD_TITLE, NO_OBJECTS, PANE_W, SELECTION_TITLE, STYLES_TITLE, clip_label,
    hidden_objects,
};
pub use recipients::{
    Entry as RecipientEntry, entries as recipient_entries, split_entries as split_recipients,
};
pub use spelling::{COMPLETE as SPELLING_COMPLETE, TITLE as SPELLING_TITLE, is_open as spelling_open};
pub use voting::{READ_RECEIPT_REQUEST, ReceiptPolicy, tracking_bars};

/// Default compose window size (compose.md).
pub const DEFAULT_SIZE: [f32; 2] = [1440.0, 780.0];
/// Smallest size accepted (responsive.md: 300×200 accepted).
pub const MIN_SIZE: [f32; 2] = [300.0, 200.0];
/// Seconds between auto-save checks while a compose has unsaved changes (`compose.autoSaveTick`; the engine
/// saves only once File › Options › Mail › "Automatically save items…" minutes have passed).
pub const AUTO_SAVE_CHECK_SECS: f64 = 15.0;

const SEND_SIZE: Vec2 = Vec2::new(60.0, 76.0);
const NAME_BTN: Vec2 = Vec2::new(80.0, 32.0);
const ROW_H: f32 = 38.0;
const MARGIN: f32 = 16.0;
const GAP: f32 = 12.0;
const QAT_H: f32 = 30.0;
const TAB_H: f32 = 30.0;
/// Left inset of the attachments well (lines the chips up with the address buttons).
const WELL_INDENT: f32 = MARGIN + SEND_SIZE.x + MARGIN;

/// Compose tabs used when the ribbon area does not list any (compose.md).
const FALLBACK_TABS: [&str; 6] = ["Message", "Insert", "Options", "Format Text", "Review", "Help"];

/// Save-changes prompt text (our wording).
pub const SAVE_PROMPT: &str = "Do you want to save changes?";

/// Most symbols kept in the Symbol menu's recently used list.
pub const MAX_RECENT_SYMBOLS: usize = 20;

static RECENT_SYMBOLS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Symbols inserted recently, newest first (Insert › Symbol ▾).
pub fn recent_symbols() -> Vec<String> {
    RECENT_SYMBOLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// Puts `symbol` (the text `insert.symbol` inserted) at the top of the recently used symbols.
pub fn remember_symbol(symbol: &str) {
    if symbol.is_empty() || symbol.chars().count() > 4 {
        return;
    }
    let mut list = RECENT_SYMBOLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    list.retain(|s| s != symbol);
    list.insert(0, symbol.to_owned());
    list.truncate(MAX_RECENT_SYMBOLS);
}

/// Runs `insert.symbol` for compose `cid` and remembers the symbol it inserted.
pub(crate) fn insert_symbol(app: &mut App, cid: u64, ch: &str) -> bool {
    let r = app.run("insert.symbol", json!({"compose": cid, "char": ch}));
    if let Some(text) = r.as_ref().and_then(|v| v.get("inserted")).and_then(serde_json::Value::as_str) {
        remember_symbol(text);
    }
    r.is_some()
}

/// The viewport of compose window `cid`.
pub fn viewport_id(cid: u64) -> ViewportId {
    ViewportId::from_hash_of(("mailcraft-compose", cid))
}

/// Header fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Field {
    From,
    To,
    Cc,
    Bcc,
    Subject,
}

impl Field {
    fn command(self) -> &'static str {
        match self {
            Field::From => "compose.setFrom",
            Field::To => "compose.setTo",
            Field::Cc => "compose.setCc",
            Field::Bcc => "compose.setBcc",
            Field::Subject => "compose.setSubject",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Field::From => "From",
            Field::To => "To",
            Field::Cc => "Cc",
            Field::Bcc => "Bcc",
            Field::Subject => "Subject",
        }
    }
    fn book_field(self) -> &'static str {
        match self {
            Field::Cc => "cc",
            Field::Bcc => "bcc",
            _ => "to",
        }
    }
    fn value(self, c: &ComposeState) -> &str {
        match self {
            Field::From => &c.from,
            Field::To => &c.to,
            Field::Cc => &c.cc,
            Field::Bcc => &c.bcc,
            Field::Subject => &c.subject,
        }
    }
}

/// egui id of a header field of compose `cid`.
fn field_id(cid: u64, f: Field) -> Id {
    Id::new(("mailcraft-compose-field", cid, f))
}

/// The compose whose header field or body has keyboard focus, if any (for the keyboard handler: compose
/// shortcuts such as Ctrl+Enter apply to it, and list shortcuts must not fire while typing).
pub fn focused_compose(ctx: &egui::Context, app: &App) -> Option<u64> {
    if let Some(cid) = drawing::text_focus_compose(ctx).filter(|cid| app.s.composes.contains_key(cid)) {
        return Some(cid);
    }
    let focus = ctx.memory(|m| m.focused())?;
    app.s.composes.keys().copied().find(|&cid| {
        focus == editor_id(cid)
            || [Field::From, Field::To, Field::Cc, Field::Bcc, Field::Subject]
                .iter()
                .any(|f| focus == field_id(cid, *f))
    })
}

/// Per-compose window state in egui temp memory.
#[derive(Clone, Copy, Debug, Default)]
struct WinState {
    /// Initial focus placed.
    started: bool,
    /// Widget to focus once it is drawn for real.
    pending_focus: Option<Id>,
    /// The save-changes prompt is open.
    ask_save: bool,
}

fn win_state_id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-win", cid))
}

fn win_state(ctx: &egui::Context, cid: u64) -> WinState {
    ctx.data(|d| d.get_temp::<WinState>(win_state_id(cid))).unwrap_or_default()
}

fn set_win_state(ctx: &egui::Context, cid: u64, st: WinState) {
    ctx.data_mut(|d| d.insert_temp(win_state_id(cid), st));
}

/// Opens the "Do you want to save changes?" prompt in compose window `cid` (e.g. for File › Close or a
/// `saveChanges` dialog notice from `compose.close`).
pub fn ask_save_changes(ctx: &egui::Context, cid: u64) {
    let mut st = win_state(ctx, cid);
    st.ask_save = true;
    set_win_state(ctx, cid, st);
}

/// Close request for a window: prompt when there are unsaved changes, else close right away.
fn request_close(ctx: &egui::Context, app: &mut App, cid: u64) {
    let dirty = app.s.composes.get(&cid).is_some_and(|c| c.dirty);
    if dirty {
        ask_save_changes(ctx, cid);
    } else {
        app.run("compose.close", json!({"compose": cid, "save": false}));
    }
}

// =======================================================================================================
// Entry points
// =======================================================================================================

/// All open compose windows (non-inline composes in `app.s.composes`).
pub fn compose_windows(ctx: &egui::Context, app: &mut App) {
    app.ui.egui_ctx = Some(ctx.clone());
    capture::poll(ctx, app);
    let now = ctx.input(|i| i.time);
    auto_save_timer(ctx, app, now);
    let ids: Vec<u64> = app.s.composes.values().filter(|c| !c.inline).map(|c| c.id).collect();
    for cid in ids {
        let Some(c) = app.s.composes.get(&cid) else {
            continue;
        };
        let builder = ViewportBuilder::default()
            .with_title(c.title())
            .with_inner_size(DEFAULT_SIZE)
            .with_min_inner_size(MIN_SIZE);
        ctx.show_viewport_immediate(viewport_id(cid), builder, |ui, class| {
            compose_viewport(ui, app, cid, class)
        });
    }
}

fn auto_save_id() -> Id {
    Id::new("mailcraft-compose-autosave")
}

/// The auto-save timer: while a compose (window or inline) has unsaved changes, runs `compose.autoSaveTick`
/// every [`AUTO_SAVE_CHECK_SECS`] seconds of `now` (egui time) and asks for a repaint then, so a message
/// nobody is typing in is still saved. The first check comes one period after the changes are first seen.
/// Returns true when it ran the tick.
pub fn auto_save_timer(ctx: &egui::Context, app: &mut App, now: f64) -> bool {
    let dirty = app.s.composes.values().any(|c| c.dirty);
    let last: Option<f64> = ctx.data(|d| d.get_temp(auto_save_id()));
    if !dirty {
        if last.is_some() {
            ctx.data_mut(|d| d.remove::<f64>(auto_save_id()));
        }
        return false;
    }
    // A clock that went backwards (a new context) restarts the period.
    let since = match last {
        Some(t) if t <= now => t,
        _ => {
            ctx.data_mut(|d| d.insert_temp(auto_save_id(), now));
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(AUTO_SAVE_CHECK_SECS));
            return false;
        }
    };
    let waited = now - since;
    if waited < AUTO_SAVE_CHECK_SECS {
        let left = (AUTO_SAVE_CHECK_SECS - waited).clamp(0.1, AUTO_SAVE_CHECK_SECS);
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
        return false;
    }
    ctx.data_mut(|d| d.insert_temp(auto_save_id(), now));
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(AUTO_SAVE_CHECK_SECS));
    app.run("compose.autoSaveTick", json!({}));
    true
}

/// Inline response inside the reading pane.
pub fn inline_compose(ui: &mut Ui, app: &mut App, id: u64) {
    if !app.s.composes.contains_key(&id) {
        return;
    }
    let ctx = ui.ctx().clone();
    initial_focus(&ctx, app, id);
    attach::take_dropped_files(&ctx, app, id, Some(ui.max_rect()));
    ui.push_id(("mailcraft-inline-compose", id), |ui| {
        // Discard / Pop Out at the top right.
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 28.0),
            Layout::right_to_left(Align::Center),
            |ui| {
                ui.add_space(MARGIN);
                if crate::widgets::icon_button(ui, app, "close", "Discard", true).clicked() {
                    app.run("compose.discard", json!({"compose": id}));
                }
                if crate::widgets::icon_button(ui, app, "restore", "Pop Out", true).clicked() {
                    app.run("compose.popOut", json!({"compose": id}));
                }
            },
        );
        if !app.s.composes.contains_key(&id) {
            return;
        }
        header(ui, app, id);
        attach::well(ui, app, id, WELL_INDENT);
        a11y::bar(ui, app, id);
        separator(ui, app);
        let h = ui.available_height();
        let min_h = if h.is_finite() && h < 4000.0 { h.max(160.0) } else { 240.0 };
        panes::body_with_panes(ui, app, id, min_h, false);
    });
    spelling::dialog(&ctx, app, id);
    attach::save_as_dialog(&ctx, app, id);
}

// =======================================================================================================
// Window
// =======================================================================================================

fn compose_viewport(ui: &mut Ui, app: &mut App, cid: u64, class: ViewportClass) {
    let ctx = ui.ctx().clone();
    if !app.s.composes.contains_key(&cid) {
        return;
    }
    let embedded = matches!(class, ViewportClass::EmbeddedWindow);
    if !embedded && ctx.input(|i| i.viewport().close_requested()) {
        let dirty = app.s.composes.get(&cid).is_some_and(|c| c.dirty);
        if dirty {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        }
        request_close(&ctx, app, cid);
        if !app.s.composes.contains_key(&cid) {
            return;
        }
    }
    // Files dropped from the OS become attachments (an embedded window shares the main window's input: only
    // drops over the window count).
    attach::take_dropped_files(&ctx, app, cid, embedded.then(|| ui.max_rect()));
    initial_focus(&ctx, app, cid);

    let size = if embedded { ui.available_size() } else { ctx.content_rect().size() };
    let frame = Frame::compute(size.x, size.y);
    let tiny = frame.inspector_tiny;

    let backstage_here = matches!(&app.ui.backstage, Some((Surface::Compose(s), _)) if *s == cid);
    egui::CentralPanel::default().frame(egui::Frame::new().fill(app.theme.pane)).show(ui, |ui| {
        if backstage_here {
            crate::backstage::compose_backstage(ui, app, cid);
            return;
        }
        if !tiny || embedded {
            quick_access(ui, app, cid, embedded, tiny);
        }
        // Where the F6 stops are drawn (header fields, body, ribbon).
        let band = |ui: &Ui, top: f32| {
            Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top().max(top))
        };
        let ribbon_top = ui.cursor().top();
        if !tiny {
            tab_row(ui, app, cid);
            crate::ribbon::ribbon(ui, app, Surface::Compose(cid), &frame);
        }
        let ribbon = band(ui, ribbon_top);
        if !app.s.composes.contains_key(&cid) {
            return;
        }
        ui.add_space(8.0);
        let header_top = ui.cursor().top();
        header(ui, app, cid);
        attach::well(ui, app, cid, WELL_INDENT);
        let head = band(ui, header_top);
        a11y::bar(ui, app, cid);
        separator(ui, app);
        let body_top = ui.cursor().top();
        panes::body_with_panes(ui, app, cid, 0.0, true);
        let body =
            Rect::from_x_y_ranges(ui.max_rect().x_range(), body_top..=ui.max_rect().bottom().max(body_top));
        // F6 (the main window's rule): header fields (To first), body, ribbon (the selected tab).
        let first_field =
            if app.s.composes.get(&cid).is_some_and(|c| c.show_from) { Field::From } else { Field::To };
        crate::keys::window_stops(
            ui,
            app,
            Surface::Compose(cid),
            vec![
                crate::keys::WindowStop {
                    name: "header",
                    rect: head,
                    target: Some(field_id(cid, first_field)),
                },
                crate::keys::WindowStop { name: "body", rect: body, target: Some(editor_id(cid)) },
                crate::keys::WindowStop { name: "ribbon", rect: ribbon, target: None },
            ],
        );
    });

    save_prompt(&ctx, app, cid);
    spelling::dialog(&ctx, app, cid);
    attach::save_as_dialog(&ctx, app, cid);

    // F7 anywhere in the window (the body editor already took it when it has focus).
    let f7 = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F7));
    if f7 && app.s.composes.contains_key(&cid) {
        spelling::open(&ctx, app, cid);
    }

    // Compose-surface shortcuts (Ctrl+S, Ctrl+Enter, Alt keytips…) on this window's own input. The body editor
    // already took the keys it handles out of it. An embedded window shares the main viewport's input, which
    // the main handler routes to the focused compose itself (`focused_compose`).
    if !embedded {
        crate::keys::handle(&ctx, app, Surface::Compose(cid));
    }
}

/// Focus starts in To for new messages and forwards, in the body for replies and drafts (compose.md,
/// reply-forward.md).
fn initial_focus(ctx: &egui::Context, app: &App, cid: u64) {
    let mut st = win_state(ctx, cid);
    if st.started {
        return;
    }
    st.started = true;
    let to_first =
        app.s.composes.get(&cid).is_some_and(|c| matches!(c.kind, ComposeKind::New | ComposeKind::Forward));
    st.pending_focus = Some(if to_first { field_id(cid, Field::To) } else { editor_id(cid) });
    set_win_state(ctx, cid, st);
}

/// Gives `resp` the initial focus of compose `cid` when it is the pending target and really on screen (a new
/// window's first frame is a sizing pass, where a focus request would be dropped).
pub(crate) fn claim_initial_focus(ui: &Ui, cid: u64, resp: &egui::Response) {
    let ctx = ui.ctx();
    let mut st = win_state(ctx, cid);
    if st.pending_focus != Some(resp.id) || ui.is_sizing_pass() || !ui.is_visible() {
        return;
    }
    resp.request_focus();
    st.pending_focus = None;
    set_win_state(ctx, cid, st);
}

/// Quick Access row: Save, Undo, Redo, Previous Item, Next Item (plus Close when embedded).
fn quick_access(ui: &mut Ui, app: &mut App, cid: u64, embedded: bool, tiny: bool) {
    let Some(c) = app.s.composes.get(&cid) else {
        return;
    };
    let can_undo = !c.edit_undo.is_empty();
    let can_redo = !c.edit_redo.is_empty();
    let has_source = c.source.is_some();
    let ctx = ui.ctx().clone();
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), QAT_H),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_space(8.0);
            if !tiny {
                if crate::widgets::icon_only(ui, app, "save", "Save", true).clicked() {
                    app.run("compose.saveDraft", json!({"compose": cid}));
                }
                if crate::widgets::icon_only(ui, app, "undo", "Undo", can_undo).clicked() {
                    app.run("compose.editUndo", json!({"compose": cid}));
                }
                if crate::widgets::icon_only(ui, app, "redo", "Redo", can_redo).clicked() {
                    app.run("compose.editRedo", json!({"compose": cid}));
                }
                if crate::widgets::icon_only(ui, app, "chevron-up", "Previous Item", has_source).clicked() {
                    app.run("nav.prevItem", json!({}));
                }
                if crate::widgets::icon_only(ui, app, "chevron-down", "Next Item", has_source).clicked() {
                    app.run("nav.nextItem", json!({}));
                }
            }
            if embedded {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(8.0);
                    // The embedding egui window shows the title; it has no close button of its own.
                    if crate::widgets::icon_only(ui, app, "close", "Close", true).clicked() {
                        request_close(&ctx, app, cid);
                    }
                });
            }
        },
    );
}

/// File + the compose tabs; the selected tab is `app.ui.tab(Surface::Compose(cid))`.
fn tab_row(ui: &mut Ui, app: &mut App, cid: u64) {
    let surface = Surface::Compose(cid);
    let mut tabs: Vec<&str> = crate::ribbon::tabs(app, surface);
    if tabs.is_empty() {
        tabs = FALLBACK_TABS.to_vec();
    }
    let selected = app
        .ui
        .tab(surface)
        .filter(|t| tabs.contains(t))
        .map(str::to_owned)
        .unwrap_or_else(|| tabs.first().map(|t| (*t).to_owned()).unwrap_or_default());
    let accent = app.theme.accent;
    let text = app.theme.text;
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), TAB_H),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_space(8.0);
            // Tabs sit apart like the main window's tab row, not packed together.
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.spacing_mut().button_padding.x = 6.0;
            if ui.add(egui::Button::new(egui::RichText::new("File").color(text)).frame(false)).clicked() {
                app.ui.backstage = Some((surface, "info".to_owned()));
            }
            for t in tabs {
                let on = t == selected;
                let r = ui
                    .add(egui::Button::new(egui::RichText::new(t).color(text)).frame(false).selected(false));
                if on {
                    let y = r.rect.bottom() + 1.0;
                    ui.painter().line_segment(
                        [pos2(r.rect.left() + 4.0, y), pos2(r.rect.right() - 4.0, y)],
                        Stroke::new(3.0, accent),
                    );
                }
                if r.clicked() {
                    app.ui.set_tab(surface, t);
                }
            }
        },
    );
}

/// The save-changes prompt (Yes saves to Drafts and closes, No closes, Cancel keeps editing).
fn save_prompt(ctx: &egui::Context, app: &mut App, cid: u64) {
    let mut st = win_state(ctx, cid);
    if !st.ask_save {
        return;
    }
    let mut choice: Option<Option<bool>> = None;
    let theme = app.theme.clone();
    let modal = egui::Modal::new(Id::new(("mailcraft-compose-save", cid))).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &theme);
        ui.set_min_width(300.0);
        ui.label(egui::RichText::new(SAVE_PROMPT).size(15.0));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let size = vec2(80.0, 26.0);
            if ui.add(egui::Button::new("Yes").min_size(size)).clicked() {
                choice = Some(Some(true));
            }
            if ui.add(egui::Button::new("No").min_size(size)).clicked() {
                choice = Some(Some(false));
            }
            if ui.add(egui::Button::new("Cancel").min_size(size)).clicked() {
                choice = Some(None);
            }
        });
    });
    if choice.is_none() && modal.should_close() {
        choice = Some(None);
    }
    if let Some(c) = choice {
        st.ask_save = false;
        set_win_state(ctx, cid, st);
        if let Some(save) = c {
            app.run("compose.close", json!({"compose": cid, "save": save}));
        }
    }
}

// =======================================================================================================
// Header
// =======================================================================================================

fn separator(ui: &mut Ui, app: &App) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, CornerRadius::ZERO, app.theme.separator);
}

/// The big Send button (icon over label).
fn send_button(ui: &mut Ui, app: &mut App, rect: Rect) -> egui::Response {
    let r = ui.allocate_rect(rect, Sense::click());
    r.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Send"));
    let fill = if r.is_pointer_button_down_on() {
        app.theme.selected
    } else if r.hovered() {
        app.theme.hover
    } else {
        app.theme.pane
    };
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(4), fill);
    p.rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0, app.theme.separator), StrokeKind::Inside);
    let icon = Rect::from_center_size(pos2(rect.center().x, rect.top() + 26.0), vec2(24.0, 24.0));
    app.icons.paint(ui, icon, "send", app.theme.accent);
    ui.painter().text(
        pos2(rect.center().x, rect.bottom() - 18.0),
        Align2::CENTER_CENTER,
        "Send",
        egui::FontId::proportional(13.0),
        app.theme.text,
    );
    r
}

/// To / Cc / Bcc / From buttons (80×32, outlined like the Send button: they are buttons, not labels).
fn name_button(ui: &mut Ui, app: &App, rect: Rect, label: &str) -> egui::Response {
    ui.put(
        rect,
        egui::Button::new(egui::RichText::new(label).color(app.theme.text))
            .fill(app.theme.pane)
            .stroke(Stroke::new(1.0, app.theme.separator))
            .min_size(NAME_BTN),
    )
}

/// An address / subject field filling `rect`; runs the field's command when the text changes.
fn field_edit(ui: &mut Ui, app: &mut App, cid: u64, f: Field, rect: Rect, label_id: Option<Id>) {
    let Some(c) = app.s.composes.get(&cid) else {
        return;
    };
    let mut text = f.value(c).to_owned();
    let before = text.clone();
    let edit_rect =
        Rect::from_min_max(pos2(rect.left(), rect.top() + 3.0), pos2(rect.right(), rect.bottom() - 3.0));
    let te = TextEdit::singleline(&mut text)
        .id(field_id(cid, f))
        .frame(egui::Frame::NONE)
        .text_color(app.theme.text)
        .vertical_align(Align::Center)
        .desired_width(edit_rect.width());
    let mut r = ui.put(edit_rect, te);
    if let Some(l) = label_id {
        r = r.labelled_by(l);
    }
    claim_initial_focus(ui, cid, &r);
    let line_col = if r.has_focus() { app.theme.accent } else { app.theme.separator };
    ui.painter().line_segment(
        [pos2(rect.left(), rect.bottom() - 1.0), pos2(rect.right(), rect.bottom() - 1.0)],
        Stroke::new(1.0, line_col),
    );
    if text != before {
        app.run(f.command(), json!({"compose": cid, "value": text}));
    }
}

/// F7 on a compose surface: opens the Spelling and Grammar dialog (for the keyboard handler; compose windows
/// and the body editor catch F7 themselves).
pub fn open_spelling_dialog(ctx: &egui::Context, app: &mut App, cid: u64) {
    spelling::open(ctx, app, cid);
}

/// The compose whose To, Cc or Bcc field has keyboard focus (Ctrl+K there is Check Names, not Insert Link).
pub fn focused_recipient_field(ctx: &egui::Context, app: &App) -> Option<u64> {
    let focus = ctx.memory(|m| m.focused())?;
    app.s
        .composes
        .keys()
        .copied()
        .find(|&cid| [Field::To, Field::Cc, Field::Bcc].iter().any(|f| focus == field_id(cid, *f)))
}

/// Ribbon commands the compose window handles itself (called by the ribbon before running a command on a
/// compose surface). Returns true when handled:
/// - `compose.attachFile` without a file (Browse This PC…) runs the platform's file picker when installed;
///   with a `path` it attaches and remembers the file for the recent list;
/// - `options.open {"page": "signatures"}` (Signature ▾ › Signatures…) opens Signatures and Stationery itself
///   rather than the Options dialog;
/// - `review.spelling` opens the Spelling and Grammar dialog (spelling only or with grammar, as last picked in
///   the button's menu);
/// - `format.paste` with a `mode` (Paste ▾ menu) pastes the system clipboard with that mode;
/// - the Insert and Options commands in [`insert_command`] (screen capture, native file pickers, recent
///   symbols).
pub(crate) fn ribbon_command(
    ctx: &egui::Context,
    app: &mut App,
    cid: u64,
    cmd: &str,
    params: &serde_json::Value,
) -> bool {
    match cmd {
        "compose.attachFile" if attach::names_no_file(params) => attach::browse(app, cid),
        "compose.attachFile" => match params.get("path").and_then(serde_json::Value::as_str) {
            Some(p) => {
                attach::attach_path(app, cid, p);
                true
            }
            None => false,
        },
        "options.open" if params.get("page").and_then(serde_json::Value::as_str) == Some("signatures") => {
            crate::dialogs::open(app, "signatures", json!({"compose": cid}));
            true
        }
        "review.spelling" => {
            // Without a choice (the button, F7) the mode last picked in the button's menu.
            let grammar = params
                .get("grammar")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or_else(|| mailcraft_engine::cmd::review::grammar_mode(&app.s));
            spelling::open_with(ctx, app, cid, grammar);
            true
        }
        // Cut / Copy: like Ctrl+X / Ctrl+C, the selection goes on the system clipboard too (HTML and text).
        "format.copy" | "format.cut" => {
            if let Some(r) = app.run(cmd, params.clone()) {
                editor::put_copy_on_clipboard(ctx, cid, &r);
            }
            editor::refocus_body(ctx, cid);
            true
        }
        // Paste (and Paste ▾ › Keep Source Formatting / Merge Formatting / Keep Text Only): paste the system
        // clipboard (HTML when it has some) like Ctrl+V, with the chosen mode or the Options default.
        "format.paste"
            if params.get("text").is_none()
                && params.get("html").is_none()
                && params.get("image").is_none() =>
        {
            let wanted = params.get("mode").and_then(serde_json::Value::as_str);
            let mode = PASTE_MODES.iter().map(|(m, _)| *m).find(|m| Some(*m) == wanted);
            if wanted.is_some() && mode.is_none() {
                // An unknown mode: the engine reports it.
                return false;
            }
            let system = clipboard_hooks().read_text.map(|_| {
                let text = clipboard_text().unwrap_or_default();
                let image = if text.is_empty() { clipboard_image() } else { None };
                (text, image)
            });
            let handled = match (system, mode) {
                (Some((text, image)), _) if !text.is_empty() || image.is_some() => {
                    let html = if text.is_empty() { None } else { html_for_paste(&text) };
                    paste_system_clipboard(ctx, app, cid, &text, html, image, mode);
                    true
                }
                // The system clipboard is empty or could not be read (a Wayland session without
                // data-control, a clipboard whose owner has exited): the engine pastes what Copy / Cut
                // put on the in-app clipboard, or says there is nothing to paste.
                (Some(_), _) => false,
                // No clipboard reader: ask the platform for a paste event, which the body editor takes
                // with the mode.
                (None, Some(m)) => {
                    request_paste(ctx, cid, m);
                    true
                }
                // Without a reader and a mode the in-app clipboard is all there is: the engine pastes it.
                (None, None) => false,
            };
            if handled {
                editor::refocus_body(ctx, cid);
            }
            handled
        }
        _ => insert_command(ctx, app, cid, cmd, params),
    }
}

/// Picks one file with the platform's open dialog (`None`: no dialog installed or cancelled).
fn pick_one(app: &App, title: &str, filters: &[crate::FileFilter]) -> Option<String> {
    let files = app.files.clone()?;
    files.open_files(title, filters, false, None)?.into_iter().next().map(|p| p.display().to_string())
}

/// Insert and Options commands that need the window: Screenshot captures through the platform backend;
/// This Device… (Pictures, 3D Models, Object), the saved screenshot, Browse for Themes and Save Current Theme
/// use the platform's file dialogs when the app installed them (else the engine's typed-path forms open);
/// symbols inserted from the menu join the recently used list. Returns true when handled.
fn insert_command(
    ctx: &egui::Context,
    app: &mut App,
    cid: u64,
    cmd: &str,
    params: &serde_json::Value,
) -> bool {
    use crate::FileFilter;
    let no_file = params.get("path").is_none() && params.get("data_base64").is_none();
    let picture_types =
        || vec![FileFilter::new("Pictures", &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg"])];
    let picked = |app: &App, title: &str, filters: &[FileFilter]| -> Option<Option<String>> {
        app.files.as_ref()?;
        Some(pick_one(app, title, filters))
    };
    let run_with_path = |app: &mut App, cmd: &str, path: Option<String>| {
        if let Some(path) = path {
            let mut p = params.clone();
            if let Some(o) = p.as_object_mut() {
                o.insert("path".into(), json!(path));
                o.insert("compose".into(), json!(cid));
            }
            app.run(cmd, p);
        }
        true
    };
    match cmd {
        "insert.screenshot" if capture::ribbon_screenshot(ctx, app, cid, params) => true,
        "insert.screenshot" if no_file && params.get("unavailable").is_none() => {
            match picked(app, "Insert a Saved Screenshot", &picture_types()) {
                Some(path) => run_with_path(app, cmd, path),
                None => false,
            }
        }
        "insert.picture" if no_file => match picked(app, "Insert Picture", &picture_types()) {
            Some(path) => run_with_path(app, cmd, path),
            None => false,
        },
        "insert.model3d" if no_file && params.get("edit").is_none() => {
            match picked(
                app,
                "Insert 3D Model",
                &[FileFilter::new("3D models", &["obj", "stl", "ply", "gltf", "glb"])],
            ) {
                Some(path) => run_with_path(app, cmd, path),
                None => false,
            }
        }
        // The picked file then goes to the Object dialog, which asks how to put it in (icon, link or text).
        "insert.object" if no_file && params.get("ask").is_none() => {
            match picked(app, "Insert Object", &[]) {
                Some(Some(path)) => {
                    app.run(cmd, json!({"compose": cid, "path": path, "ask": true}));
                    true
                }
                Some(None) => true,
                None => false,
            }
        }
        "compose.theme"
            if params.get("action").and_then(serde_json::Value::as_str) == Some("browse")
                && params.get("path").is_none() =>
        {
            match picked(app, "Browse for Themes", &[FileFilter::new("MailCraft themes", &["json"])]) {
                Some(path) => run_with_path(app, cmd, path),
                None => false,
            }
        }
        "insert.symbol" => match params.get("char").and_then(serde_json::Value::as_str) {
            Some(ch) => {
                insert_symbol(app, cid, ch);
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// Pastes what the system clipboard holds into compose `cid`: its text `text` (with the matching HTML
/// flavour `html`), or, when it holds no text, its picture `image`. Returns whether something was pasted.
pub(crate) fn paste_system_clipboard(
    ctx: &egui::Context,
    app: &mut App,
    cid: u64,
    text: &str,
    html: Option<String>,
    image: Option<Vec<u8>>,
    mode: Option<&str>,
) -> bool {
    if text.len() > MAX_PASTE_TEXT {
        app.s.notify(mailcraft_engine::Notice::Error("The clipboard holds too much text to paste.".into()));
        return false;
    }
    if !text.is_empty() {
        return editor::paste_clipboard(ctx, app, cid, text, html, mode);
    }
    match image {
        Some(data) => {
            let b64 = mailcraft_mime::transfer::encode_base64_flat(&data);
            app.run("format.paste", json!({"compose": cid, "image": b64})).is_some()
        }
        None => false,
    }
}

/// Most clipboard text the ribbon's Paste takes (bigger content is refused with a message).
pub const MAX_PASTE_TEXT: usize = 8 * 1024 * 1024;

/// Send + address rows + Subject (shared by windows and inline responses).
fn header(ui: &mut Ui, app: &mut App, cid: u64) {
    let Some(c) = app.s.composes.get(&cid) else {
        return;
    };
    let mut rows: Vec<Field> = Vec::new();
    if c.show_from {
        rows.push(Field::From);
    }
    rows.extend([Field::To, Field::Cc]);
    if c.show_bcc {
        rows.push(Field::Bcc);
    }
    rows.push(Field::Subject);
    let accounts: Vec<(u32, String)> = app
        .s
        .mailbox
        .accounts
        .iter()
        .filter(|a| !a.address.trim().is_empty())
        .map(|a| (a.id.0, a.address.clone()))
        .collect();

    let width = ui.available_width();
    let rows_h = ROW_H * rows.len() as f32;
    let height = rows_h.max(SEND_SIZE.y) + 4.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let left = rect.left() + MARGIN;
    let send_rect = Rect::from_min_size(pos2(left, rect.top()), SEND_SIZE);
    if send_button(ui, app, send_rect).clicked() {
        app.run("compose.send", json!({"compose": cid}));
        if !app.s.composes.contains_key(&cid) {
            return;
        }
    }
    let btn_x = send_rect.right() + MARGIN;
    let field_x = btn_x + NAME_BTN.x + GAP;
    let field_r = (rect.right() - MARGIN).max(field_x + 20.0);
    for (i, f) in rows.into_iter().enumerate() {
        let top = rect.top() + ROW_H * i as f32;
        let btn = Rect::from_min_size(pos2(btn_x, top + (ROW_H - NAME_BTN.y) / 2.0), NAME_BTN);
        let field = Rect::from_min_max(pos2(field_x, top), pos2(field_r, top + ROW_H));
        let label_id = match f {
            Field::Subject => {
                let r = ui.put(btn, egui::Label::new(egui::RichText::new("Subject").color(app.theme.text)));
                Some(r.id)
            }
            Field::From => {
                let r = name_button(ui, app, btn, "From");
                egui::Popup::menu(&r).show(|ui| {
                    for (id, addr) in &accounts {
                        if ui.button(addr).clicked() {
                            app.run("compose.setFrom", json!({"compose": cid, "value": id.to_string()}));
                        }
                    }
                    if accounts.is_empty() {
                        ui.label("No accounts");
                    }
                });
                Some(r.id)
            }
            _ => {
                let r = name_button(ui, app, btn, f.label());
                if r.clicked() {
                    app.run("compose.addressBook", json!({"compose": cid, "field": f.book_field()}));
                }
                Some(r.id)
            }
        };
        match f {
            Field::To | Field::Cc | Field::Bcc => recipients::address_field(ui, app, cid, f, field, label_id),
            Field::From | Field::Subject => field_edit(ui, app, cid, f, field, label_id),
        }
    }
}
