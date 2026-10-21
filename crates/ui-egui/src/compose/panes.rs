//! Side panes of the compose window: Clipboard on the left (Message › Clipboard / Format Text › Clipboard
//! launcher, `format.clipboardPane`), Styles (`format.stylesPane`) and Selection (`format.selectionPane`) on
//! the right, stacked when both are open. Each pane is a column beside the body editor with a title and a
//! close button; on a window too narrow to keep a usable body the panes are left out.
//!
//! - Clipboard: the last 24 items copied or cut in MailCraft, newest first. Click an item to paste it; its
//!   menu pastes or deletes it; Paste All / Clear All.
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
/// Shown when the clipboard history is empty.
pub const CLIPBOARD_EMPTY: &str = "Nothing copied yet. Items you copy or cut in a message collect here.";
/// Name field of New Style… in the Styles pane.
pub const NEW_STYLE_NAME: &str = "Style name";
/// Shown when the message has no objects.
pub const NO_OBJECTS: &str = "There are no pictures or shapes in this message.";

/// Widths of the left (Clipboard) and right (Styles / Selection) columns in a body area `width` wide;
/// 0 for a column that is closed or has no room.
pub fn widths(c: &ComposeState, width: f32) -> (f32, f32) {
    let left_on = c.editor.clipboard_pane;
    let right_on = c.editor.styles_pane || c.editor.selection_pane;
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
        let both = app.s.composes.get(&cid).is_some_and(|c| c.editor.styles_pane && c.editor.selection_pane);
        let r = Rect::from_min_size(right.min, vec2(rw, used.height()));
        let (top, bottom) = if both {
            let mid_y = r.center().y;
            (
                Rect::from_min_max(r.min, pos2(r.right(), mid_y)),
                Some(Rect::from_min_max(pos2(r.left(), mid_y), r.max)),
            )
        } else {
            (r, None)
        };
        let styles = app.s.composes.get(&cid).is_some_and(|c| c.editor.styles_pane);
        if styles {
            pane_frame(ui, app, top, ("mc-pane-styles", cid), |ui, app| styles_pane(ui, app, cid));
        }
        let selection_rect = if styles { bottom } else { Some(top) };
        if let Some(sr) = selection_rect
            && app.s.composes.get(&cid).is_some_and(|c| c.editor.selection_pane)
        {
            pane_frame(ui, app, sr, ("mc-pane-selection", cid), |ui, app| selection_pane(ui, app, cid));
        }
    }
    ui.advance_cursor_after_rect(used);
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
    ui.horizontal(|ui| {
        if ui.add_enabled(!items.is_empty(), egui::Button::new("Paste All")).clicked() {
            action = Some(json!({"pasteAll": true}));
        }
        if ui.add_enabled(!items.is_empty(), egui::Button::new("Clear All")).clicked() {
            action = Some(json!({"clear": true}));
        }
    });
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
    let sel_drawing = c.selected_drawing;
    let ctx = ui.ctx().clone();
    let mut hidden = hidden_objects(&ctx, cid);
    let before = hidden.clone();
    let mut select: Option<usize> = None;
    ui.horizontal(|ui| {
        if ui.add_enabled(!objects.is_empty(), egui::Button::new("Show All")).clicked() {
            hidden.clear();
        }
        if ui.add_enabled(!objects.is_empty(), egui::Button::new("Hide All")).clicked() {
            hidden = objects.iter().map(|o| o.src.clone()).collect();
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
                    Some(id) => sel_drawing == Some(id),
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
                    let text =
                        if is_hidden { RichText::new(&o.name).italics() } else { RichText::new(&o.name) };
                    if ui.add(egui::Button::selectable(selected, text)).clicked() {
                        select = Some(i);
                    }
                });
            }
        },
    );
    if hidden != before {
        set_hidden(&ctx, cid, hidden);
        ctx.request_repaint();
    }
    if let Some(i) = select {
        app.run("format.selectionPane", json!({"compose": cid, "select": i}));
        ctx.memory_mut(|m| m.request_focus(super::editor_id(cid)));
    }
}
