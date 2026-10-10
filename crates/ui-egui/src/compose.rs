//! Compose windows (viewports) and inline responses in the reading pane, with the body editor (compose area).
//!
//! Layout follows plan/outlook/compose.md, reading-pane.md (inline reply) and responsive.md (compose window):
//!
//! - Each non-inline [`ComposeState`] is an immediate viewport (default 1440×780, title from
//!   [`ComposeState::title`], OS title bar). Under it: a Quick Access row (Save, Undo, Redo, Previous Item, Next
//!   Item), the tab row (File + the compose tabs) and the ribbon from [`crate::ribbon::ribbon`].
//! - Header: a big Send button (60×76) spanning the first two address rows; To / Cc buttons (80×32) that open the
//!   address book; Subject label; optional From and Bcc rows; the attachments well; a 1-px separator; the body.
//! - At 300×200 (anything below ~360×260) the Quick Access row, tab row and ribbon disappear; the header stays.
//! - Closing a window with unsaved changes asks "Do you want to save changes?" (Yes / No / Cancel) →
//!   `compose.close {"save": true|false}`.
//! - Inline responses: the same header plus Discard and Pop Out at the top right, the body editor below.
//!
//! Every change goes through `App::run` (`compose.*`, `format.*`, `nav.*`); header fields run their `compose.set*`
//! command once per frame when the text changed.

mod editor;
#[cfg(test)]
mod tests;

use egui::{
    Align, Align2, CornerRadius, Id, Layout, Rect, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2,
    ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, WidgetInfo, WidgetType, pos2, vec2,
};
use serde_json::json;

use mailcraft_engine::compose::{ComposeKind, ComposeState};

use crate::responsive::Frame;
use crate::{App, Surface};

pub use editor::{BODY_LABEL, body_focused, editor_id};

/// Default compose window size (compose.md).
pub const DEFAULT_SIZE: [f32; 2] = [1440.0, 780.0];
/// Smallest size accepted (responsive.md: 300×200 accepted).
pub const MIN_SIZE: [f32; 2] = [300.0, 200.0];
/// Below this width or height the Quick Access row, tab row and ribbon are hidden (300×200 observation).
pub const TINY_W: f32 = 360.0;
pub const TINY_H: f32 = 260.0;

const SEND_SIZE: Vec2 = Vec2::new(60.0, 76.0);
const NAME_BTN: Vec2 = Vec2::new(80.0, 32.0);
const ROW_H: f32 = 38.0;
const MARGIN: f32 = 16.0;
const GAP: f32 = 12.0;
const QAT_H: f32 = 30.0;
const TAB_H: f32 = 30.0;

/// Compose tabs used when the ribbon area does not list any (compose.md).
const FALLBACK_TABS: [&str; 6] = ["Message", "Insert", "Options", "Format Text", "Review", "Help"];

/// Save-changes prompt text (our wording).
pub const SAVE_PROMPT: &str = "Do you want to save changes?";

/// Whether a compose window of this size hides its Quick Access row, tabs and ribbon.
pub fn is_tiny(width: f32, height: f32) -> bool {
    width < TINY_W || height < TINY_H
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

/// Inline response inside the reading pane.
pub fn inline_compose(ui: &mut Ui, app: &mut App, id: u64) {
    if !app.s.composes.contains_key(&id) {
        return;
    }
    let ctx = ui.ctx().clone();
    initial_focus(&ctx, app, id);
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
        attachments(ui, app, id);
        separator(ui, app);
        let h = ui.available_height();
        let min_h = if h.is_finite() && h < 4000.0 { h.max(160.0) } else { 240.0 };
        editor::body_editor(ui, app, id, min_h);
    });
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
    if !embedded {
        attach_dropped_files(&ctx, app, cid);
    }
    initial_focus(&ctx, app, cid);

    let size = if embedded { ui.available_size() } else { ctx.content_rect().size() };
    let tiny = is_tiny(size.x, size.y);
    let frame = Frame::compute(size.x, size.y);

    let backstage_here = matches!(&app.ui.backstage, Some((Surface::Compose(s), _)) if *s == cid);
    egui::CentralPanel::default().frame(egui::Frame::new().fill(app.theme.pane)).show(ui, |ui| {
        if backstage_here {
            crate::backstage::compose_backstage(ui, app, cid);
            return;
        }
        if !tiny || embedded {
            quick_access(ui, app, cid, embedded, tiny);
        }
        if !tiny {
            tab_row(ui, app, cid);
            crate::ribbon::ribbon(ui, app, Surface::Compose(cid), &frame);
        }
        if !app.s.composes.contains_key(&cid) {
            return;
        }
        ui.add_space(8.0);
        header(ui, app, cid);
        attachments(ui, app, cid);
        separator(ui, app);
        let h = ui.available_height().max(40.0);
        egui::ScrollArea::vertical()
            .id_salt(("mailcraft-compose-scroll", cid))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                editor::body_editor(ui, app, cid, h);
            });
    });

    save_prompt(&ctx, app, cid);

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

/// Files dropped onto the window become attachments.
fn attach_dropped_files(ctx: &egui::Context, app: &mut App, cid: u64) {
    let paths: Vec<String> = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_string_lossy().into_owned())
            .filter(|p| !p.is_empty())
            .collect()
    });
    for p in paths {
        app.run("compose.attachFile", json!({"compose": cid, "path": p}));
    }
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
        field_edit(ui, app, cid, f, field, label_id);
    }
}

/// Attachment chips (paperclip, name, size, remove).
fn attachments(ui: &mut Ui, app: &mut App, cid: u64) {
    let Some(c) = app.s.composes.get(&cid) else {
        return;
    };
    if c.attachments.is_empty() {
        return;
    }
    let items: Vec<(String, usize)> =
        c.attachments.iter().map(|a| (a.filename.clone(), a.data.len())).collect();
    let mut remove = None;
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(MARGIN + SEND_SIZE.x + MARGIN);
        for (i, (name, len)) in items.iter().enumerate() {
            egui::Frame::new()
                .stroke(Stroke::new(1.0, app.theme.separator))
                .corner_radius(CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(6, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (ir, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                        app.icons.paint(ui, ir, "paperclip", app.theme.text_muted);
                        ui.label(egui::RichText::new(name).color(app.theme.text));
                        ui.label(egui::RichText::new(size_text(*len)).color(app.theme.text_muted).small());
                        if ui
                            .add(egui::Button::new("×").frame(false).small())
                            .on_hover_text(format!("Remove {name}"))
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                });
        }
    });
    ui.add_space(4.0);
    if let Some(i) = remove {
        app.run("compose.removeAttachment", json!({"compose": cid, "index": i}));
    }
}

/// "12 KB" / "1.2 MB".
pub fn size_text(bytes: usize) -> String {
    let b = bytes as f64;
    if b < 1024.0 * 1024.0 {
        format!("{} KB", ((b / 1024.0).ceil() as u64).max(1))
    } else {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    }
}
