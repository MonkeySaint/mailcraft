//! Side panes of the compose window: Clipboard on the left (Message › Clipboard / Format Text › Clipboard
//! launcher, `format.clipboardPane`), Editor (`review.editor`), Styles (`format.stylesPane`) and Selection
//! (`format.selectionPane`) on the right, stacked when more than one is open. Each pane is a column beside the body editor with a title and a
//! close button; on a window too narrow to keep a usable body the panes are left out.
//!
//! - Clipboard: the last 24 items copied or cut in MailCraft, copied from the reading pane and (while
//!   MailCraft runs) copied in other programs, newest first. Click an item to paste it; its menu pastes or
//!   deletes it; Paste All / Clear All; Clipboard Options (collect from other programs, collect while the
//!   pane is closed, open the pane by itself).
//! - Editor: the Editor score and the issue counts by group (spelling and grammar corrections; clarity,
//!   conciseness, clichés, formality and inclusiveness refinements). Pick a group to step through its
//!   issues: click one to select it in the message, apply a suggestion, or ignore it.
//! - Styles: the built-in styles and the ones created in this message, the caret's style marked. Click to
//!   apply; a style's menu updates it to match the selection, renames or deletes a created style (or reverts
//!   a modified built-in one); New Style… creates one from the selection; Clear All clears the formatting.
//! - Selection: the message's pictures, shapes, charts, SmartArt, WordArt and icons (all held as inline
//!   pictures). Click to select one; Hide / Show toggles it in the editor view (the message still contains
//!   it); Show All / Hide All.
//!
//! Every change runs a command (`format.clipboardPane`, `format.style`, `format.selectionPane`…).

use egui::{Align, Id, Layout, Rect, RichText, Stroke, Ui, UiBuilder, pos2, vec2};
use serde_json::json;

use mailcraft_engine::cmd::format::styles_list;
use mailcraft_engine::compose::ComposeState;

use crate::App;

/// Width of a pane column.
pub const PANE_W: f32 = 260.0;
/// Narrowest body kept beside the panes.
const MIN_BODY_W: f32 = 280.0;
/// Pane titles (also their accessible names).
pub const CLIPBOARD_TITLE: &str = "Clipboard";
pub const STYLES_TITLE: &str = "Styles";
pub const SELECTION_TITLE: &str = "Selection";
pub const EDITOR_TITLE: &str = "Editor";
/// Shown when the clipboard history is empty.
pub const CLIPBOARD_EMPTY: &str =
    "Nothing copied yet. What you copy or cut in MailCraft, or copy in other programs, collects here.";
/// The Clipboard pane's Options menu.
pub const CLIPBOARD_OPTIONS: &str = "Clipboard Options";
/// Options menu entries: (`format.clipboardOptions` parameter, setting, label).
pub const CLIPBOARD_OPTION_ITEMS: [(&str, &str, &str); 3] = [
    ("collectOther", "state.clipboard.collectOther", "Collect text copied in other programs"),
    ("collectHidden", "state.clipboard.collectHidden", "Collect while the pane is closed"),
    ("showAuto", "state.clipboard.showAuto", "Open the pane by itself on the second item"),
];
/// Name field of New Style… in the Styles pane.
pub const NEW_STYLE_NAME: &str = "Style name";
/// Shown when the message has no objects.
pub const NO_OBJECTS: &str = "There are no pictures or shapes in this message.";

/// Widths of the left (Clipboard) and right (Styles / Selection) columns in a body area `width` wide;
/// 0 for a column that is closed or has no room.
pub fn widths(c: &ComposeState, width: f32) -> (f32, f32) {
    let left_on = c.editor.clipboard_pane;
    let right_on = c.editor.styles_pane || c.editor.selection_pane || c.editor.editor_pane;
    let width = if width.is_finite() { width } else { 0.0 };
    let mut lw = if left_on { PANE_W } else { 0.0 };
    let mut rw = if right_on { PANE_W } else { 0.0 };
    if width - lw - rw < MIN_BODY_W {
        // Keep the right column (where the caret's formatting is) when only one fits.
        lw = 0.0;
        if width - rw < MIN_BODY_W {
            rw = 0.0;
        }
    }
    (lw, rw)
}

// -----------------------------------------------------------------------------------------------------------
// Hidden objects (view only)

fn hidden_id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-hidden-objects", cid))
}

/// Picture sources hidden from the editor view by the Selection pane.
pub fn hidden_objects(ctx: &egui::Context, cid: u64) -> Vec<String> {
    ctx.data(|d| d.get_temp::<Vec<String>>(hidden_id(cid))).unwrap_or_default()
}

fn set_hidden(ctx: &egui::Context, cid: u64, v: Vec<String>) {
    ctx.data_mut(|d| d.insert_temp(hidden_id(cid), v));
}

// -----------------------------------------------------------------------------------------------------------
// Layout

/// The body editor of compose `cid` with its open panes beside it. In a compose window (`scroll`) the body
/// scrolls inside the remaining height; inline (no scroll) it is at least `min_h` tall.
pub fn body_with_panes(ui: &mut Ui, app: &mut App, cid: u64, min_h: f32, scroll: bool) {
    let Some(c) = app.s.composes.get(&cid) else {
        return;
    };
    if c.editor.apply_styles {
        let near = ui.available_rect_before_wrap().right_top();
        apply_styles_window(ui.ctx(), app, cid, near);
    }
    let Some(c) = app.s.composes.get(&cid) else {
        return;
    };
    let avail = ui.available_rect_before_wrap();
    let (lw, rw) = widths(c, avail.width());
    let h = if scroll { ui.available_height().max(40.0) } else { min_h.max(40.0) };
    if lw <= 0.0 && rw <= 0.0 {
        body(ui, app, cid, h, scroll);
        return;
    }
    let full = Rect::from_min_size(avail.min, vec2(avail.width(), h));
    let left = Rect::from_min_size(full.min, vec2(lw, h));
    let right = Rect::from_min_max(pos2(full.right() - rw, full.top()), full.max);
    let mid = Rect::from_min_max(pos2(left.right(), full.top()), pos2(right.left(), full.bottom()));
    let mut m = ui.new_child(
        UiBuilder::new().max_rect(mid).layout(Layout::top_down(Align::Min)).id_salt(("mc-compose-mid", cid)),
    );
    body(&mut m, app, cid, h, scroll);
    let used = full.union(m.min_rect());
    if lw > 0.0 {
        let r = Rect::from_min_size(left.min, vec2(lw, used.height()));
        pane_frame(ui, app, r, ("mc-pane-clip", cid), |ui, app| clipboard_pane(ui, app, cid));
    }
    if rw > 0.0 {
        // The open right-hand panes share the column, top to bottom: Editor, Styles, Selection.
        let open: Vec<RightPane> = app.s.composes.get(&cid).map(right_panes).unwrap_or_default();
        let r = Rect::from_min_size(right.min, vec2(rw, used.height()));
        let n = open.len().max(1) as f32;
        let h = r.height() / n;
        for (i, pane) in open.into_iter().enumerate() {
            let top = r.top() + h * i as f32;
            let rect = Rect::from_min_max(pos2(r.left(), top), pos2(r.right(), top + h));
            match pane {
                RightPane::Editor => {
                    pane_frame(ui, app, rect, ("mc-pane-editor", cid), |ui, app| editor_pane(ui, app, cid));
                }
                RightPane::Styles => {
                    pane_frame(ui, app, rect, ("mc-pane-styles", cid), |ui, app| styles_pane(ui, app, cid));
                }
                RightPane::Selection => {
                    pane_frame(ui, app, rect, ("mc-pane-selection", cid), |ui, app| {
                        selection_pane(ui, app, cid)
                    });
                }
            }
        }
    }
    ui.advance_cursor_after_rect(used);
}

/// A pane of the right-hand column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightPane {
    Editor,
    Styles,
    Selection,
}

/// The open right-hand panes of `c`, top to bottom.
pub fn right_panes(c: &ComposeState) -> Vec<RightPane> {
    let mut out = Vec::new();
    if c.editor.editor_pane {
        out.push(RightPane::Editor);
    }
    if c.editor.styles_pane {
        out.push(RightPane::Styles);
    }
    if c.editor.selection_pane {
        out.push(RightPane::Selection);
    }
    out
}

fn body(ui: &mut Ui, app: &mut App, cid: u64, h: f32, scroll: bool) {
    if scroll {
        egui::ScrollArea::vertical()
            .id_salt(("mailcraft-compose-scroll", cid))
            .auto_shrink([false, false])
            .max_height(h)
            .show(ui, |ui| {
                super::editor::body_editor(ui, app, cid, h);
            });
    } else {
        super::editor::body_editor(ui, app, cid, h);
    }
}

/// A pane column: background, border and contents clipped to `rect`.
fn pane_frame(
    ui: &mut Ui,
    app: &mut App,
    rect: Rect,
    salt: (&'static str, u64),
    f: impl FnOnce(&mut Ui, &mut App),
) {
    if rect.width() < 40.0 || rect.height() < 40.0 {
        return;
    }
    ui.painter().rect_filled(rect, 0.0, app.theme.pane);
    ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, app.theme.separator), egui::StrokeKind::Inside);
    let inner = rect.shrink(8.0);
    let mut child =
        ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)).id_salt(salt));
    child.set_clip_rect(inner.intersect(ui.clip_rect()));
    f(&mut child, app);
}

/// Title row with a close button; returns whether close was clicked.
fn title_row(ui: &mut Ui, app: &mut App, title: &str) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong().size(14.0).color(app.theme.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = format!("Close {title} pane");
            let r = ui.add(egui::Button::new("\u{2715}").frame(false)).on_hover_text(label.clone());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));
            close = r.clicked();
        });
    });
    ui.add_space(4.0);
    close
}

fn muted(ui: &mut Ui, app: &App, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(app.theme.text_muted)).wrap());
}

// -----------------------------------------------------------------------------------------------------------
// Clipboard

/// One line of a clipboard item for the list (pictures counted).
pub fn clip_label(text: &str, pictures: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s: String = flat.chars().take(120).collect();
    if flat.chars().count() > 120 {
        s.push('\u{2026}');
    }
    match (s.is_empty(), pictures) {
        (true, 0) => "(empty)".to_owned(),
        (true, n) => format!("[{n} picture{}]", if n == 1 { "" } else { "s" }),
        (false, 0) => s,
        (false, n) => format!("{s}  [{n} picture{}]", if n == 1 { "" } else { "s" }),
    }
}

fn clipboard_pane(ui: &mut Ui, app: &mut App, cid: u64) {
    if title_row(ui, app, CLIPBOARD_TITLE) {
        app.run("format.clipboardPane", json!({"compose": cid, "show": false}));
        return;
    }
    // The history is shared by the open messages: a message opened after the copies picks it up.
    let own_empty = app.s.composes.get(&cid).is_some_and(|c| c.clipboard.is_empty());
    let others = app.s.composes.iter().any(|(k, c)| *k != cid && !c.clipboard.is_empty());
    if own_empty && others {
        app.run("format.clipboardPane", json!({"compose": cid, "list": true}));
    }
    let items: Vec<String> = app
        .s
        .composes
        .get(&cid)
        .map(|c| {
            c.clipboard
                .iter()
                .rev()
                .map(|e| {
                    let pictures = mailcraft_html::doc_to_html(&e.doc).matches("<img").count();
                    clip_label(&e.text, pictures)
                })
                .collect()
        })
        .unwrap_or_default();
    let mut action: Option<serde_json::Value> = None;
    let mut options: Option<serde_json::Value> = None;
    ui.horizontal(|ui| {
        if ui.add_enabled(!items.is_empty(), egui::Button::new("Paste All")).clicked() {
            action = Some(json!({"pasteAll": true}));
        }
        if ui.add_enabled(!items.is_empty(), egui::Button::new("Clear All")).clicked() {
            action = Some(json!({"clear": true}));
        }
        ui.menu_button(CLIPBOARD_OPTIONS, |ui| {
            for (param, key, label) in CLIPBOARD_OPTION_ITEMS {
                let mut on = app.s.settings.bool(key);
                if ui.checkbox(&mut on, label).clicked() {
                    let mut m = serde_json::Map::new();
                    m.insert(param.to_owned(), json!(on));
                    options = Some(serde_json::Value::Object(m));
                    ui.close();
                }
            }
        });
    });
    if let Some(o) = options {
        app.run("format.clipboardOptions", o);
    }
    ui.add_space(4.0);
    if items.is_empty() {
        muted(ui, app, CLIPBOARD_EMPTY);
    } else {
        muted(ui, app, "Click an item to paste it:");
    }
    egui::ScrollArea::vertical().id_salt(("mc-clip-list", cid)).auto_shrink([false, false]).show(ui, |ui| {
        for (i, label) in items.iter().enumerate() {
            let r = ui.add(egui::Button::new(label).wrap().min_size(vec2(ui.available_width(), 24.0)));
            if r.clicked() {
                action = Some(json!({"paste": i}));
            }
            r.context_menu(|ui| {
                if ui.button("Paste").clicked() {
                    action = Some(json!({"paste": i}));
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    action = Some(json!({"delete": i}));
                    ui.close();
                }
            });
        }
    });
    if let Some(mut p) = action {
        if let Some(o) = p.as_object_mut() {
            o.insert("compose".into(), json!(cid));
        }
        let pasted = p.get("paste").is_some() || p.get("pasteAll").is_some();
        app.run("format.clipboardPane", p);
        if pasted {
            ui.ctx().memory_mut(|m| m.request_focus(super::editor_id(cid)));
        }
    }
}

// -----------------------------------------------------------------------------------------------------------
// Styles

/// Inline edits of the Styles pane (new style name, rename).
#[derive(Clone, Debug, Default)]
struct StylesState {
    new_name: Option<String>,
    rename: Option<(String, String)>,
}

fn styles_state_id(cid: u64) -> Id {
    Id::new(("mc-styles-pane", cid))
}

fn styles_pane(ui: &mut Ui, app: &mut App, cid: u64) {
    if title_row(ui, app, STYLES_TITLE) {
        app.run("format.stylesPane", json!({"compose": cid, "show": false}));
        return;
    }
    let Some(c) = app.s.composes.get(&cid) else {
        return;
    };
    let list = styles_list(c);
    let ctx = ui.ctx().clone();
    let mut st: StylesState = ctx.data(|d| d.get_temp(styles_state_id(cid))).unwrap_or_default();
    let mut run: Option<(&'static str, serde_json::Value)> = None;
    if ui.button("Clear All").on_hover_text("Clear the formatting of the selection").clicked() {
        run = Some(("format.clearFormatting", json!({})));
    }
    ui.add_space(4.0);
    let list_h = (ui.available_height() - 64.0).max(40.0);
    egui::ScrollArea::vertical()
        .id_salt(("mc-styles-list", cid))
        .max_height(list_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for v in &list {
                let name = v.get("name").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
                let builtin = v.get("builtin").and_then(serde_json::Value::as_bool).unwrap_or(true);
                let modified = v.get("modified").and_then(serde_json::Value::as_bool).unwrap_or(false);
                let current = v.get("current").and_then(serde_json::Value::as_bool).unwrap_or(false);
                if let Some((old, new)) = st.rename.as_mut()
                    && *old == name
                {
                    let mut done = false;
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(new).desired_width(140.0));
                        if ui.button("Rename").clicked()
                            || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        {
                            run = Some((
                                "format.style",
                                json!({"name": name, "newName": new.trim(), "action": "rename"}),
                            ));
                            done = true;
                        }
                        if ui.button("Cancel").clicked() {
                            done = true;
                        }
                    });
                    if done {
                        st.rename = None;
                    }
                    continue;
                }
                let shown = if modified { format!("{name} *") } else { name.clone() };
                let r = ui
                    .add(egui::Button::selectable(current, shown).min_size(vec2(ui.available_width(), 22.0)));
                let r = if modified { r.on_hover_text("Changed in this message") } else { r };
                if r.clicked() {
                    run = Some(("format.style", json!({"name": name})));
                }
                r.context_menu(|ui| {
                    if ui.button("Modify...").clicked() {
                        run = Some(("format.style", json!({"name": name, "action": "define"})));
                        ui.close();
                    }
                    if ui.button(format!("Update {name} to Match Selection")).clicked() {
                        run = Some(("format.style", json!({"name": name, "action": "modify"})));
                        ui.close();
                    }
                    if !builtin && ui.button("Rename...").clicked() {
                        st.rename = Some((name.clone(), name.clone()));
                        ui.close();
                    }
                    if (!builtin || modified)
                        && ui.button(if builtin { "Revert to Built-in" } else { "Delete" }).clicked()
                    {
                        run = Some(("format.style", json!({"name": name, "action": "delete"})));
                        ui.close();
                    }
                });
            }
        });
    ui.add_space(4.0);
    match st.new_name.as_mut() {
        Some(name) => {
            let mut done = false;
            ui.horizontal(|ui| {
                let r =
                    ui.add(egui::TextEdit::singleline(name).hint_text(NEW_STYLE_NAME).desired_width(130.0));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, NEW_STYLE_NAME));
                let ok = !name.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new("Create")).clicked()
                    || (ok && r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    run = Some(("format.style", json!({"name": name.trim(), "action": "create"})));
                    done = true;
                }
                if ui.button("Cancel").clicked() {
                    done = true;
                }
            });
            if done {
                st.new_name = None;
            }
        }
        None => {
            if ui
                .button("New Style...")
                .on_hover_text("Create a style from the formatting at the cursor")
                .clicked()
            {
                st.new_name = Some(String::new());
            }
        }
    }
    ctx.data_mut(|d| d.insert_temp(styles_state_id(cid), st));
    if let Some((id, mut p)) = run {
        if let Some(o) = p.as_object_mut() {
            o.insert("compose".into(), json!(cid));
        }
        app.run(id, p);
    }
}

// -----------------------------------------------------------------------------------------------------------
// Apply Styles

/// Title of the Apply Styles pane.
pub const APPLY_STYLES_TITLE: &str = "Apply Styles";
/// Label of its style name box.
pub const STYLE_NAME_LABEL: &str = "Style Name:";
/// Most style names suggested under the box while typing.
const MAX_STYLE_SUGGESTIONS: usize = 8;

#[derive(Clone, Debug, Default)]
struct ApplyStylesState {
    name: String,
    /// The box is being edited (it stops following the caret's style).
    editing: bool,
    autocomplete: bool,
    /// Set once `autocomplete` was initialised.
    ready: bool,
}

/// Style names starting with (then containing) `typed`, without regard to case.
pub fn style_suggestions(names: &[String], typed: &str) -> Vec<String> {
    let t = typed.trim().to_lowercase();
    if t.is_empty() {
        return Vec::new();
    }
    let mut starts: Vec<String> =
        names.iter().filter(|n| n.to_lowercase().starts_with(&t)).cloned().collect();
    let more =
        names.iter().filter(|n| !n.to_lowercase().starts_with(&t) && n.to_lowercase().contains(&t)).cloned();
    starts.extend(more);
    starts.retain(|n| !n.eq_ignore_ascii_case(typed.trim()));
    starts.truncate(MAX_STYLE_SUGGESTIONS);
    starts
}

/// Apply Styles: a small window that stays open while you write, with the style at the cursor, a style
/// name box (with suggestions), Reapply, Modify and the Styles pane button.
fn apply_styles_window(ctx: &egui::Context, app: &mut App, cid: u64, near: egui::Pos2) {
    let Some(c) = app.s.composes.get(&cid) else { return };
    let names: Vec<String> = styles_list(c)
        .iter()
        .filter_map(|v| v.get("name").and_then(serde_json::Value::as_str).map(str::to_owned))
        .collect();
    let current = mailcraft_engine::cmd::format::current_style(c);
    let styles_open = c.editor.styles_pane;
    let state_id = Id::new(("mc-apply-styles", cid));
    let mut st: ApplyStylesState = ctx.data(|d| d.get_temp(state_id)).unwrap_or_default();
    if !st.ready {
        st.autocomplete = true;
        st.ready = true;
    }
    if !st.editing {
        st.name = current.clone();
    }
    let mut open = true;
    let mut run: Option<(&'static str, serde_json::Value)> = None;
    egui::Window::new(APPLY_STYLES_TITLE)
        .id(state_id.with("window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_pos(near + vec2(-PANE_W - 20.0, 8.0))
        .default_width(240.0)
        .show(ctx, |ui| {
            ui.label(STYLE_NAME_LABEL);
            let r = ui
                .add(egui::TextEdit::singleline(&mut st.name).desired_width(220.0).id(state_id.with("name")));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, STYLE_NAME_LABEL));
            if r.changed() || r.has_focus() {
                st.editing = true;
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if r.lost_focus() && !enter && !ui.input(|i| i.pointer.any_down()) {
                st.editing = false;
            }
            if st.autocomplete && st.editing {
                for n in style_suggestions(&names, &st.name) {
                    if ui
                        .add(egui::Button::selectable(false, n.as_str()).min_size(vec2(220.0, 20.0)))
                        .clicked()
                    {
                        st.name = n;
                        run = Some(("format.style", json!({"name": st.name.trim()})));
                        st.editing = false;
                    }
                }
            }
            let known = names.iter().any(|n| n.eq_ignore_ascii_case(st.name.trim()));
            if enter && !st.name.trim().is_empty() {
                // A name that is not a style yet makes one from the selection, as the original does.
                let action = if known { "apply" } else { "create" };
                run = Some(("format.style", json!({"name": st.name.trim(), "action": action})));
                st.editing = false;
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let label =
                    if known && st.name.trim().eq_ignore_ascii_case(&current) { "Reapply" } else { "Apply" };
                if ui
                    .add_enabled(known, egui::Button::new(label))
                    .on_hover_text("Apply this style to the selection again")
                    .clicked()
                {
                    run = Some(("format.style", json!({"name": st.name.trim()})));
                    st.editing = false;
                }
                if ui.add_enabled(known, egui::Button::new("Modify...")).clicked() {
                    run = Some(("format.style", json!({"name": st.name.trim(), "action": "define"})));
                }
                let styles = ui.add(egui::Button::selectable(styles_open, "Styles"));
                styles.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::Button, true, styles_open, "Styles pane")
                });
                if styles.on_hover_text("Show the Styles pane").clicked() {
                    run = Some(("format.stylesPane", json!({"show": !styles_open})));
                }
            });
            ui.checkbox(&mut st.autocomplete, "AutoComplete style names");
        });
    ctx.data_mut(|d| d.insert_temp(state_id, st));
    if !open {
        run = Some(("format.stylesPane", json!({"apply": true, "show": false})));
    }
    if let Some((id, mut p)) = run {
        if let Some(o) = p.as_object_mut() {
            o.insert("compose".into(), json!(cid));
        }
        app.run(id, p);
    }
}

// -----------------------------------------------------------------------------------------------------------
// Selection

fn selection_pane(ui: &mut Ui, app: &mut App, cid: u64) {
    if title_row(ui, app, SELECTION_TITLE) {
        app.run("format.selectionPane", json!({"compose": cid, "show": false}));
        return;
    }
    let Some(c) = app.s.composes.get(&cid) else {
        return;
    };
    let objects = mailcraft_engine::cmd::format::pane_objects(c);
    let sel = c.selection.clone();
    let selected_drawings = c.selected_drawings();
    let sel_drawing = c.selected_drawing;
    let ctx = ui.ctx().clone();
    let mut hidden = hidden_objects(&ctx, cid);
    let before = hidden.clone();
    // (index, Ctrl held)
    let mut select: Option<(usize, bool)> = None;
    let mut reorder: Option<(usize, i64)> = None;
    let rename_id = Id::new(("mc-selection-rename", cid));
    let mut renaming: Option<(usize, String)> = ctx.data(|d| d.get_temp(rename_id));
    let mut run: Option<serde_json::Value> = None;
    let current = objects.iter().position(|o| o.drawing.is_some() && o.drawing == sel_drawing);
    ui.horizontal(|ui| {
        if ui.add_enabled(!objects.is_empty(), egui::Button::new("Show All")).clicked() {
            hidden.clear();
        }
        if ui.add_enabled(!objects.is_empty(), egui::Button::new("Hide All")).clicked() {
            hidden = objects.iter().map(|o| o.src.clone()).collect();
        }
        // Reorder: moves the selected drawing one step up (in front) or down (behind) the list.
        let up =
            ui.add_enabled(current.is_some(), egui::Button::new("▲").small()).on_hover_text("Bring Forward");
        up.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Bring Forward"));
        if up.clicked()
            && let Some(i) = current
        {
            reorder = Some((i, 1));
        }
        let down =
            ui.add_enabled(current.is_some(), egui::Button::new("▼").small()).on_hover_text("Send Backward");
        down.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Send Backward"));
        if down.clicked()
            && let Some(i) = current
        {
            reorder = Some((i, -1));
        }
    });
    ui.add_space(4.0);
    if objects.is_empty() {
        muted(ui, app, NO_OBJECTS);
    }
    egui::ScrollArea::vertical().id_salt(("mc-selection-list", cid)).auto_shrink([false, false]).show(
        ui,
        |ui| {
            for (i, o) in objects.iter().enumerate() {
                let selected = match o.drawing {
                    Some(id) => selected_drawings.contains(&id),
                    None => {
                        sel_drawing.is_none()
                            && sel.start() == &o.at
                            && sel.end().path == o.at.path
                            && sel.end().offset == o.at.offset.saturating_add(1)
                    }
                };
                let is_hidden = hidden.contains(&o.src);
                ui.horizontal(|ui| {
                    let toggle = if is_hidden { "Show" } else { "Hide" };
                    let label = format!("{toggle} {}", o.name);
                    let t = ui.add(egui::Button::new(toggle).small()).on_hover_text(label.clone());
                    t.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone())
                    });
                    if t.clicked() {
                        if is_hidden {
                            hidden.retain(|h| *h != o.src);
                        } else if hidden.len() < 2_000 {
                            hidden.push(o.src.clone());
                        }
                    }
                    // Double-click (or Rename on the menu) to rename; Enter keeps it, Escape cancels, and an
                    // empty name gives the generated one back.
                    if let Some((at, name)) = renaming.as_mut()
                        && *at == i
                    {
                        let r = ui.add(egui::TextEdit::singleline(name).desired_width(140.0));
                        r.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Object name")
                        });
                        if r.lost_focus() {
                            if !ui.input(|inp| inp.key_pressed(egui::Key::Escape)) {
                                run = Some(json!({"compose": cid, "rename": i, "name": name.trim()}));
                            }
                            renaming = None;
                        } else {
                            r.request_focus();
                        }
                        return;
                    }
                    let text =
                        if is_hidden { RichText::new(&o.name).italics() } else { RichText::new(&o.name) };
                    let r = ui.add(egui::Button::selectable(selected, text));
                    if r.double_clicked() {
                        renaming = Some((i, o.name.clone()));
                    } else if r.clicked() {
                        let add = ui.input(|inp| inp.modifiers.command || inp.modifiers.shift);
                        select = Some((i, add && o.drawing.is_some()));
                    }
                    r.context_menu(|ui| {
                        if ui.button("Rename").clicked() {
                            renaming = Some((i, o.name.clone()));
                            ui.close();
                        }
                    });
                });
            }
        },
    );
    match renaming {
        Some(r) => ctx.data_mut(|d| {
            d.insert_temp(rename_id, r);
        }),
        None => ctx.data_mut(|d| d.remove::<(usize, String)>(rename_id)),
    }
    if let Some(p) = run {
        app.run("format.selectionPane", p);
    }
    if hidden != before {
        set_hidden(&ctx, cid, hidden);
        ctx.request_repaint();
    }
    if let Some((i, by)) = reorder {
        app.run("format.selectionPane", json!({"compose": cid, "reorder": i, "by": by}));
    }
    if let Some((i, add)) = select {
        let p = if add {
            json!({"compose": cid, "select": i, "add": true})
        } else {
            json!({"compose": cid, "select": i})
        };
        app.run("format.selectionPane", p);
        ctx.memory_mut(|m| m.request_focus(super::editor_id(cid)));
    }
}

// -----------------------------------------------------------------------------------------------------------
// Editor

/// Most issues listed for one group.
const MAX_LISTED: usize = 60;

/// The Editor pane's view state: the group being stepped through, and the last `review.editor` result with
/// what it was computed for (body edit counter, check counter, group).
#[derive(Clone, Debug, Default)]
struct EditorPaneState {
    category: Option<String>,
    cache: Option<(u64, Option<u64>, Option<String>, serde_json::Value)>,
    /// The body revision last seen and when (seconds): the pane re-asks once typing pauses.
    seen: Option<(u64, f64)>,
}

/// How long typing must pause before the Editor pane re-checks the body itself (seconds), when the editor's
/// own as-you-type check has not caught up.
pub const EDITOR_PANE_PAUSE: f64 = 0.8;

fn editor_state_id(cid: u64) -> Id {
    Id::new(("mc-editor-pane", cid))
}

/// The score line: "Editor score 92%".
pub fn score_label(score: u64) -> String {
    format!("Editor score {score}%")
}

/// A group row: "Grammar  3" or "Clarity  no suggestions".
pub fn group_label(title: &str, n: u64) -> String {
    match n {
        0 => format!("{title}: nothing to fix"),
        1 => format!("{title}: 1 suggestion"),
        n => format!("{title}: {n} suggestions"),
    }
}

fn editor_pane(ui: &mut Ui, app: &mut App, cid: u64) {
    use mailcraft_engine::cmd::review::EDITOR_CATEGORIES;
    if title_row(ui, app, EDITOR_TITLE) {
        app.run("review.editor", json!({"compose": cid, "show": false}));
        return;
    }
    let ctx = ui.ctx().clone();
    let mut st: EditorPaneState = ctx.data(|d| d.get_temp(editor_state_id(cid))).unwrap_or_default();
    let Some((edit_rev, spell_rev)) = app.s.composes.get(&cid).map(|c| (c.edit_rev, c.spell_rev)) else {
        return;
    };
    // Re-ask the engine only when the body or the check changed, or another group is shown. While typing,
    // the last result stays up: the pane re-asks once the editor's as-you-type check caught up with the body,
    // or typing paused for EDITOR_PANE_PAUSE (proofing as you type off), never on every keystroke.
    let now = ctx.input(|i| i.time);
    if st.seen.is_none_or(|(rev, _)| rev != edit_rev) {
        st.seen = Some((edit_rev, now));
    }
    let fresh = st.cache.as_ref().is_some_and(|(e, r, cat, _)| {
        *e == edit_rev && (r.is_none() || *r == spell_rev) && *cat == st.category
    });
    let same_group = st.cache.as_ref().is_some_and(|(_, _, cat, _)| *cat == st.category);
    let checked_now = spell_rev == Some(edit_rev);
    let paused = st.seen.is_some_and(|(_, at)| now - at >= EDITOR_PANE_PAUSE);
    let typing = same_group && !checked_now && !paused;
    if !fresh && typing {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(EDITOR_PANE_PAUSE));
    }
    if !fresh && !typing {
        let mut p = json!({"compose": cid, "list": true});
        if let (Some(cat), Some(o)) = (&st.category, p.as_object_mut()) {
            o.insert("category".into(), json!(cat));
        }
        let r = app.run("review.editor", p).unwrap_or(serde_json::Value::Null);
        let spell_rev = app.s.composes.get(&cid).and_then(|c| c.spell_rev);
        st.cache = Some((edit_rev, spell_rev, st.category.clone(), r));
    }
    let r = st.cache.as_ref().map(|(_, _, _, v)| v.clone()).unwrap_or(serde_json::Value::Null);
    let mut run: Option<(&'static str, serde_json::Value)> = None;
    let mut pick: Option<Option<String>> = None;
    if let Some(why) = r.get("notChecked").and_then(serde_json::Value::as_str) {
        muted(ui, app, &format!("This message is not checked: {why}"));
    } else {
        let score = r.get("score").and_then(serde_json::Value::as_u64).unwrap_or(100);
        ui.label(RichText::new(score_label(score)).strong().size(18.0).color(app.theme.text));
        let corrections = r.get("corrections").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let refinements = r.get("refinements").and_then(serde_json::Value::as_u64).unwrap_or(0);
        muted(ui, app, &format!("{corrections} corrections \u{b7} {refinements} refinements"));
        ui.add_space(6.0);
        match st.category.clone() {
            None => {
                for (heading, refine) in [("Corrections", false), ("Refinements", true)] {
                    ui.label(RichText::new(heading).strong().color(app.theme.text));
                    for (id, title, is_refine) in EDITOR_CATEGORIES {
                        if is_refine != refine {
                            continue;
                        }
                        let n = r
                            .get("counts")
                            .and_then(|c| c.get(id))
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        let b = egui::Button::new(group_label(title, n))
                            .min_size(vec2(ui.available_width(), 24.0));
                        if ui.add_enabled(n > 0, b).clicked() {
                            pick = Some(Some(id.to_owned()));
                        }
                    }
                    ui.add_space(4.0);
                }
            }
            Some(cat) => {
                let title = EDITOR_CATEGORIES.iter().find(|(id, ..)| *id == cat).map_or("", |(_, t, _)| *t);
                ui.horizontal(|ui| {
                    if ui.button("\u{2190} All groups").clicked() {
                        pick = Some(None);
                    }
                    ui.label(RichText::new(title).strong().color(app.theme.text));
                });
                let issues: Vec<serde_json::Value> = r
                    .get("issues")
                    .and_then(serde_json::Value::as_array)
                    .map(|a| a.iter().take(MAX_LISTED).cloned().collect())
                    .unwrap_or_default();
                if issues.is_empty() {
                    muted(ui, app, "Nothing left to look at here.");
                }
                egui::ScrollArea::vertical()
                    .id_salt(("mc-editor-list", cid))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, issue) in issues.iter().enumerate() {
                            editor_issue(ui, app, cid, i, issue, &mut run);
                        }
                    });
            }
        }
    }
    if let Some(p) = pick {
        st.category = p;
        st.cache = None;
    }
    if let Some((cmd, p)) = run {
        app.run(cmd, p);
        st.cache = None;
        ctx.memory_mut(|m| m.request_focus(super::editor_id(cid)));
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(editor_state_id(cid), st));
}

/// One issue of the Editor pane's group list: the flagged text and what is wrong (click: select it in the
/// message), its suggestions and Ignore.
fn editor_issue(
    ui: &mut Ui,
    app: &App,
    cid: u64,
    i: usize,
    issue: &serde_json::Value,
    run: &mut Option<(&'static str, serde_json::Value)>,
) {
    let word = issue.get("word").and_then(serde_json::Value::as_str).unwrap_or("");
    let message = issue.get("message").and_then(serde_json::Value::as_str).unwrap_or("");
    let at = issue.get("start").cloned().unwrap_or(serde_json::Value::Null);
    ui.push_id(("mc-editor-issue", i), |ui| {
        egui::Frame::new().stroke(Stroke::new(1.0, app.theme.separator)).inner_margin(6.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let shown: String = word.chars().take(80).collect();
            let r = ui
                .add(egui::Button::new(RichText::new(shown).strong()).frame(false))
                .on_hover_text("Show in the message");
            if r.clicked() {
                *run = Some(("review.editor", json!({"compose": cid, "select": at})));
            }
            let note = if message.is_empty() { "Not in the dictionary." } else { message };
            muted(ui, app, note);
            ui.horizontal_wrapped(|ui| {
                let suggestions: Vec<&str> = issue
                    .get("suggestions")
                    .and_then(serde_json::Value::as_array)
                    .map(|a| a.iter().filter_map(serde_json::Value::as_str).take(4).collect())
                    .unwrap_or_default();
                for sugg in suggestions {
                    let label = if sugg.is_empty() { "(remove)" } else { sugg };
                    if ui.button(label).clicked() {
                        *run =
                            Some(("review.correct", json!({"compose": cid, "at": at, "replacement": sugg})));
                    }
                }
                if ui.button("Ignore").clicked() {
                    *run = Some(("review.ignore", json!({"compose": cid, "at": at})));
                }
            });
        });
    });
    ui.add_space(4.0);
}
