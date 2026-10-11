//! Notes module: My Notes (folder pane) and the folder's current view: Icon (large icons, small icons or a
//! list; icons can be dragged anywhere — where they are dropped is saved with `notes.setPosition` and
//! kept with the profile — and View › Line Up Icons snaps them back to the grid), Notes List and
//! Last 7 Days (lists, newest first). Notes are edited in place; the text is saved with `notes.update` when
//! the edit box loses focus. The reading pane (off by default) shows the selected note. The order and the
//! filter come from the engine (`mailcraft_engine::cmd::notes::view_rows`), the view and the icon
//! arrangement from the folder's module view.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Id, Key, Layout, Modifiers, Rect, ScrollArea, Sense,
    Stroke, TextEdit, Ui, Vec2, WidgetType, pos2, vec2,
};
use serde_json::{Value, json};

use mailcraft_engine::cmd::calendar::pim::Kind;
use mailcraft_engine::cmd::notes::{IconMode, NotesViewState, title};
use mailcraft_engine::cmd::view::modules::{ModuleKind, ModuleView};
use mailcraft_model::{FolderKind, ItemId, ListTime, Note, NoteColor};

use super::common::{
    ModState, a11y, blend, change_view, child, current_folder, empty_message, folder_rows, group_header,
    load, module_folder_pane, module_view, note_fill, paint_text_clipped, pane_bg, pane_separator,
    reading_split, row_height, select_folder, select_item, store, sync_selection, todo_bar_strip,
    view_switch,
};
use crate::App;

pub(super) const COLORS: [(NoteColor, &str, &str); 5] = [
    (NoteColor::Yellow, "yellow", "Yellow"),
    (NoteColor::Blue, "blue", "Blue"),
    (NoteColor::Green, "green", "Green"),
    (NoteColor::Pink, "pink", "Pink"),
    (NoteColor::White, "white", "White"),
];

/// Applies a view from a command result (`{"view": {...}}`), a view object, or a mode name.
pub(super) fn apply_view(st: &mut ModState, v: &Value) {
    if let Some(s) = v.as_str() {
        st.notes_view.icon_mode = match s.to_ascii_lowercase().as_str() {
            "small" | "small icons" => IconMode::Small,
            "list" | "notes list" => IconMode::List,
            _ => IconMode::Large,
        };
        return;
    }
    let inner = v.get("view").unwrap_or(v);
    if let Ok(view) = serde_json::from_value::<NotesViewState>(inner.clone()) {
        st.notes_view = view;
    }
}

/// The Notes views with their toolbar icons.
const VIEWS: [(&str, &str); 3] = [("Icon", "note"), ("Notes List", "bullets"), ("Last 7 Days", "today")];

/// Applies an icon arrangement requested with `modules::set_notes_view` once.
fn legacy_request(app: &mut App, st: &mut ModState) {
    if st.notes_view != st.notes_view_seen {
        st.notes_view_seen = st.notes_view.clone();
        let mode = match st.notes_view.icon_mode {
            IconMode::Large => "large",
            IconMode::Small => "small",
            IconMode::List => "list",
        };
        app.run("notes.iconView", json!({ "mode": mode, "view": st.notes_view }));
    }
}

/// Notes of the current folder's view, in order.
fn order(app: &App) -> Vec<ItemId> {
    current_folder(app, Kind::Note)
        .map(|f| mailcraft_engine::cmd::notes::view_rows(&app.s, f).into_iter().map(|r| r.id).collect())
        .unwrap_or_default()
}

pub(super) fn show(ui: &mut Ui, app: &mut App) {
    let ctx = ui.ctx().clone();
    let mut st = load(&ctx);
    super::common::ensure_folder(app, Kind::Note);
    legacy_request(app, &mut st);
    let view = module_view(app, current_folder(app, Kind::Note), ModuleKind::Notes);
    if view.lined_up && !st.note_pos.is_empty() {
        // Line Up Icons.
        st.note_pos.clear();
    }
    let ids = order(app);
    sync_selection(app, |_, id| ids.contains(&id));
    let full = ui.max_rect();
    let (pane, main) = module_folder_pane(ui, app);
    if let Some(p) = pane {
        child(ui, p.shrink2(egui::vec2(6.0, 0.0)), "mc-notes-pane", |ui| {
            group_header(ui, app, "My Notes", false, "mc-notes-group");
            let current = current_folder(app, Kind::Note);
            if let (Some(f), _) = folder_rows(ui, app, FolderKind::Notes, current, None) {
                commit_edit(app, &mut st);
                select_folder(app, f);
                select_item(app, None);
            }
        });
    }
    let main = todo_bar_strip(ui, app, main);
    pane_bg(ui, main, app.theme.reading);
    child(ui, main, "mc-notes-main", |ui| content(ui, app, &mut st, &view, &ids));
    keyboard(&ctx, app, &mut st);
    ui.expand_to_include_rect(full);
    store(&ctx, st);
}

/// Saves the note being edited (when its text changed) and leaves edit mode.
fn commit_edit(app: &mut App, st: &mut ModState) {
    let Some((id, text)) = st.note_edit.take() else { return };
    let changed = app.s.mailbox.notes.get(&ItemId(id)).is_some_and(|n| n.text != text);
    if changed {
        app.run("notes.update", json!({ "id": id, "text": text }));
    }
}

fn start_edit(app: &App, st: &mut ModState, id: ItemId) {
    let text = app.s.mailbox.notes.get(&id).map(|n| n.text.clone()).unwrap_or_default();
    st.note_edit = Some((id.0, text));
    st.note_focus = true;
}

fn keyboard(ctx: &egui::Context, app: &mut App, st: &mut ModState) {
    if ctx.memory(|m| m.focused().is_some()) || st.note_edit.is_some() {
        return;
    }
    let Some(sel) = app.ui.module_selection.map(ItemId).filter(|i| app.s.mailbox.notes.contains_key(i))
    else {
        return;
    };
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete)) {
        app.run("notes.delete", json!({ "id": sel.0 }));
        select_item(app, None);
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        start_edit(app, st, sel);
    }
}

fn new_note(app: &mut App, st: &mut ModState) {
    commit_edit(app, st);
    let mut p = json!({});
    if let (Some(f), Some(o)) = (current_folder(app, Kind::Note), p.as_object_mut()) {
        o.insert("folder".into(), json!(f.0));
    }
    if let Some(out) = app.run("notes.new", p)
        && let Some(id) = out.get("id").and_then(Value::as_u64)
    {
        select_item(app, Some(id));
        st.note_edit = Some((id, String::new()));
        st.note_focus = true;
    }
}

fn icon_mode_name(m: IconMode) -> &'static str {
    match m {
        IconMode::Large => "large",
        IconMode::Small => "small",
        IconMode::List => "list",
    }
}

fn content(ui: &mut Ui, app: &mut App, st: &mut ModState, view: &ModuleView, order: &[ItemId]) {
    let rect = ui.max_rect();
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 44.0));
    child(ui, bar.shrink2(vec2(10.0, 7.0)), "mc-notes-bar", |ui| {
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            if crate::widgets::icon_button(ui, app, "note", "New Note", true).clicked() {
                new_note(app, st);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(v) = view_switch(ui, app, &VIEWS, &view.view) {
                    commit_edit(app, st);
                    change_view(app, &v);
                }
                if view.view == "Icon" {
                    ui.separator();
                    for (mode, icon, label) in [
                        (IconMode::List, "bullets", "List"),
                        (IconMode::Small, "view", "Small Icons"),
                        (IconMode::Large, "note", "Large Icons"),
                    ] {
                        if crate::widgets::toggle_button(ui, app, icon, label, view.icon_mode == mode).clicked()
                            && view.icon_mode != mode
                        {
                            app.run(
                                "notes.iconView",
                                json!({ "mode": icon_mode_name(mode), "view": { "iconMode": icon_mode_name(mode), "linedUp": view.lined_up } }),
                            );
                        }
                    }
                }
            });
        });
    });
    ui.painter().hline(rect.x_range(), bar.max.y, Stroke::new(1.0, app.theme.separator));
    let body = Rect::from_min_max(pos2(rect.min.x, bar.max.y + 1.0), rect.max);
    if order.is_empty() {
        let hint = if view.view == "Last 7 Days" {
            "No note changed in the last seven days."
        } else {
            "Jot something down with New Note."
        };
        empty_message(ui, body, app, "No notes", hint);
        return;
    }
    let (list_rect, pane) = reading_split(body, view.reading_pane, (body.width() * 0.6).max(360.0), 0.6);
    // Clicking the background ends editing.
    let bg = ui.interact(list_rect, Id::new("mc-notes-bg"), Sense::click());
    if bg.clicked() {
        commit_edit(app, st);
        select_item(app, None);
    }
    let icons = view.view == "Icon" && view.icon_mode != IconMode::List;
    child(ui, list_rect.shrink(10.0), "mc-notes-body", |ui| {
        ScrollArea::vertical().id_salt("mc-notes-scroll").auto_shrink([false, false]).show(ui, |ui| {
            if icons {
                grid(ui, app, st, order, view);
            } else {
                list(ui, app, st, order);
            }
        });
    });
    if let Some(p) = pane {
        pane_separator(ui, app, list_rect, p);
        reading(ui, app, st, p);
    }
}

/// The reading pane: the selected note's text.
fn reading(ui: &mut Ui, app: &mut App, st: &mut ModState, rect: Rect) {
    let Some(id) = app.ui.module_selection.map(ItemId).filter(|i| app.s.mailbox.notes.contains_key(i)) else {
        empty_message(ui, rect, app, "No note selected", "Pick a note to read it here.");
        return;
    };
    let Some(n) = app.s.mailbox.notes.get(&id).cloned() else { return };
    child(ui, rect.shrink2(vec2(20.0, 14.0)), "mc-notes-reading", |ui| {
        ui.horizontal(|ui| {
            let (sw, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
            ui.painter().rect_filled(sw, CornerRadius::same(3), note_fill(n.color, app.theme.dark));
            let t = title(&n);
            ui.label(
                egui::RichText::new(if t.is_empty() { "(Empty note)" } else { &t })
                    .size(18.0)
                    .color(app.theme.text),
            );
            if crate::widgets::icon_button(ui, app, "settings", "Edit", true).clicked() {
                start_edit(app, st, id);
            }
        });
        ui.label(
            egui::RichText::new(format!(
                "Modified {}",
                ListTime::short_date_time(n.modified, app.s.tz_offset_min)
            ))
            .size(11.5)
            .color(app.theme.text_muted),
        );
        ui.separator();
        ScrollArea::vertical().id_salt("mc-notes-reading-scroll").auto_shrink([false, false]).show(
            ui,
            |ui| {
                ui.label(egui::RichText::new(&n.text).color(app.theme.text));
            },
        );
    });
}

fn note_menu(ui: &mut Ui, app: &mut App, st: &mut ModState, id: ItemId) {
    if ui.button("Open").clicked() {
        commit_edit(app, st);
        super::editors::open_note(app, st, id);
        ui.close();
    }
    if ui.button("Edit").clicked() {
        start_edit(app, st, id);
        ui.close();
    }
    ui.menu_button("Color", |ui| {
        for (_, key, label) in COLORS {
            if ui.button(label).clicked() {
                app.run("notes.update", json!({ "id": id.0, "color": key }));
                ui.close();
            }
        }
    });
    ui.menu_button("Categorize", |ui| {
        let cats: Vec<String> = app.s.mailbox.categories.iter().map(|c| c.name.clone()).collect();
        for c in cats {
            if ui.button(&c).clicked() {
                app.run("notes.categorize", json!({ "id": id.0, "name": c }));
                ui.close();
            }
        }
    });
    if ui.button("Forward").clicked() {
        app.run("notes.forward", json!({ "ids": [id.0] }));
        ui.close();
    }
    if ui.button("Move to Folder...").clicked() {
        app.run("notes.move", json!({ "ids": [id.0] }));
        ui.close();
    }
    if ui.button("Delete").clicked() {
        if st.note_edit.as_ref().is_some_and(|(e, _)| *e == id.0) {
            st.note_edit = None;
        }
        app.run("notes.delete", json!({ "id": id.0 }));
        select_item(app, None);
        ui.close();
    }
}

/// Click handling shared by the grid and the list.
fn note_click(app: &mut App, st: &mut ModState, resp: &egui::Response, id: ItemId) {
    let editing = st.note_edit.as_ref().is_some_and(|(e, _)| *e == id.0);
    if resp.clicked() && !editing {
        let was_selected = app.ui.module_selection == Some(id.0);
        commit_edit(app, st);
        select_item(app, Some(id.0));
        if was_selected {
            start_edit(app, st, id);
        }
    }
    if resp.double_clicked() && !editing {
        commit_edit(app, st);
        select_item(app, Some(id.0));
        start_edit(app, st, id);
    }
}

/// In-place editor inside `rect`; commits when it loses focus.
fn edit_box(ui: &mut Ui, app: &mut App, st: &mut ModState, rect: Rect, id: ItemId) {
    let Some((_, text)) = st.note_edit.as_mut() else { return };
    let resp = ui.put(
        rect,
        TextEdit::multiline(text)
            .id(Id::new(("mc-note-edit", id.0)))
            .frame(egui::Frame::NONE)
            .text_color(app.theme.text)
            .desired_width(rect.width())
            .hint_text("Type your note"),
    );
    a11y(&resp, WidgetType::TextEdit, "Note text");
    if st.note_focus {
        resp.request_focus();
        st.note_focus = false;
    } else if resp.lost_focus() {
        commit_edit(app, st);
    }
}

fn paint_note(ui: &Ui, app: &App, card: Rect, n: &Note, selected: bool, small: bool, editing: bool) {
    let fill = note_fill(n.color, app.theme.dark);
    let p = ui.painter();
    p.rect_filled(
        card.translate(vec2(2.0, 3.0)),
        CornerRadius::same(4),
        app.theme.separator.gamma_multiply(0.7),
    );
    p.rect_filled(card, CornerRadius::same(4), fill);
    let band = Rect::from_min_size(card.min, vec2(card.width(), if small { 6.0 } else { 10.0 }));
    p.rect_filled(band, CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, blend(fill, Color32::BLACK, 0.08));
    if selected {
        p.rect_stroke(
            card,
            CornerRadius::same(4),
            Stroke::new(2.0, app.theme.accent),
            egui::StrokeKind::Outside,
        );
    }
    if editing {
        return;
    }
    let inner = card.shrink2(vec2(10.0, 8.0));
    let t = title(n);
    let head = paint_text_clipped(
        ui,
        pos2(inner.min.x, band.max.y + 4.0),
        if t.is_empty() { "(Empty note)" } else { &t },
        FontId::proportional(if small { 12.5 } else { 14.0 }),
        app.theme.text,
        inner.width(),
    );
    if small {
        return;
    }
    let rest: String = n
        .text
        .lines()
        .map(str::trim_end)
        .skip_while(|l| l.trim().is_empty())
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n");
    let body = Rect::from_min_max(pos2(inner.min.x, head.max.y + 4.0), pos2(inner.max.x, inner.max.y - 16.0));
    if body.height() > 8.0 && !rest.trim().is_empty() {
        let galley = ui.painter().layout(rest, FontId::proportional(12.5), app.theme.text, body.width());
        ui.painter().with_clip_rect(body).galley(body.min, galley, app.theme.text);
    }
    let when = ListTime::short_date_time(n.modified, app.s.tz_offset_min);
    p.text(
        pos2(inner.max.x, inner.max.y),
        Align2::RIGHT_BOTTOM,
        when,
        FontId::proportional(10.5),
        app.theme.text_muted,
    );
}

fn grid(ui: &mut Ui, app: &mut App, st: &mut ModState, order: &[ItemId], view: &ModuleView) {
    let small = view.icon_mode == IconMode::Small;
    let (w, h) = if small { (160.0, 64.0) } else { (200.0, 180.0) };
    let gap = 16.0;
    let cols = ((ui.available_width() + gap) / (w + gap)).floor().max(1.0) as usize;
    let rows = order.len().div_ceil(cols);
    // Room for icons dragged below or right of the grid.
    // Saved positions, with the icon being dragged (saved when it is dropped) on top.
    let mut placed = current_folder(app, Kind::Note)
        .map(|f| mailcraft_engine::cmd::notes::positions(&app.s, f))
        .unwrap_or_default();
    if let Some((id, pos)) = st.note_drag {
        placed.insert(id, pos);
    }
    st.note_pos = placed.clone();
    let (mut need_w, mut need_h) = (ui.available_width(), rows as f32 * (h + gap) + 4.0);
    for (x, y) in placed.values() {
        need_w = need_w.max(x + w);
        need_h = need_h.max(y + h + gap);
    }
    let (area, _) = ui.allocate_exact_size(vec2(need_w, need_h), Sense::hover());
    for (k, &id) in order.iter().enumerate() {
        let Some(n) = app.s.mailbox.notes.get(&id).cloned() else { continue };
        let slot = vec2((k % cols) as f32 * (w + gap), (k / cols) as f32 * (h + gap));
        let off = placed.get(&id.0).map_or(slot, |(x, y)| vec2(*x, *y));
        let card = Rect::from_min_size(area.min + off, vec2(w, h));
        let editing = st.note_edit.as_ref().is_some_and(|(e, _)| *e == id.0);
        let sense = if editing { Sense::click() } else { Sense::click_and_drag() };
        let resp = ui.interact(card, Id::new(("mc-note", id.0)), sense);
        let t = title(&n);
        a11y(&resp, WidgetType::Button, if t.is_empty() { "(Empty note)" } else { &t });
        paint_note(ui, app, card, &n, app.ui.module_selection == Some(id.0), small, editing);
        if editing {
            edit_box(ui, app, st, card.shrink2(vec2(8.0, 12.0)), id);
        }
        if resp.dragged() {
            let d = resp.drag_delta();
            let (x, y) = placed.get(&id.0).copied().unwrap_or((slot.x, slot.y));
            let max = mailcraft_engine::cmd::notes::MAX_OFFSET;
            let pos = ((x + d.x).clamp(0.0, max), (y + d.y).clamp(0.0, max));
            st.note_drag = Some((id.0, pos));
            st.note_pos.insert(id.0, pos);
        }
        if resp.drag_stopped()
            && let Some((_, (x, y))) = st.note_drag.take().filter(|(d, _)| *d == id.0)
        {
            app.run("notes.setPosition", json!({ "id": id.0, "x": x, "y": y }));
        }
        if resp.drag_started() && view.lined_up {
            // Icons are no longer lined up (Line Up Icons becomes useful again).
            let mode = icon_mode_name(view.icon_mode);
            app.run(
                "notes.iconView",
                json!({ "mode": mode, "view": { "iconMode": mode, "linedUp": false } }),
            );
        }
        note_click(app, st, &resp, id);
        resp.context_menu(|ui| note_menu(ui, app, st, id));
    }
}

fn list(ui: &mut Ui, app: &mut App, st: &mut ModState, order: &[ItemId]) {
    let tz = app.s.tz_offset_min;
    for &id in order {
        let Some(n) = app.s.mailbox.notes.get(&id).cloned() else { continue };
        let editing = st.note_edit.as_ref().is_some_and(|(e, _)| *e == id.0);
        let h = if editing { 120.0 } else { row_height(app, 46.0) };
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
        let t = title(&n);
        a11y(&resp, WidgetType::Button, if t.is_empty() { "(Empty note)" } else { &t });
        let sel = app.ui.module_selection == Some(id.0);
        if sel {
            ui.painter().rect_filled(rect, CornerRadius::same(3), app.theme.selected);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(3), app.theme.hover);
        }
        let sw = Rect::from_min_size(pos2(rect.min.x + 8.0, rect.min.y + 8.0), Vec2::splat(18.0));
        ui.painter().rect_filled(sw, CornerRadius::same(3), note_fill(n.color, app.theme.dark));
        ui.painter().rect_stroke(
            sw,
            CornerRadius::same(3),
            Stroke::new(1.0, app.theme.separator),
            egui::StrokeKind::Inside,
        );
        let x = rect.min.x + 36.0;
        if editing {
            edit_box(
                ui,
                app,
                st,
                Rect::from_min_max(pos2(x, rect.min.y + 4.0), pos2(rect.max.x - 8.0, rect.max.y - 4.0)),
                id,
            );
        } else {
            let when = ListTime::short_date_time(n.modified, tz);
            let wr = ui.painter().text(
                pos2(rect.max.x - 8.0, rect.min.y + 8.0),
                Align2::RIGHT_TOP,
                when,
                FontId::proportional(11.5),
                app.theme.text_muted,
            );
            paint_text_clipped(
                ui,
                pos2(x, rect.min.y + 6.0),
                if t.is_empty() { "(Empty note)" } else { &t },
                FontId::proportional(13.5),
                app.theme.text,
                wr.min.x - x - 8.0,
            );
            let preview: String = n
                .text
                .lines()
                .skip(1)
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join("  ");
            paint_text_clipped(
                ui,
                pos2(x, rect.min.y + 25.0),
                &preview,
                FontId::proportional(12.0),
                app.theme.text_muted,
                rect.max.x - x - 8.0,
            );
        }
        ui.painter().hline(rect.x_range(), rect.max.y, Stroke::new(1.0, app.theme.separator));
        note_click(app, st, &resp, id);
        resp.context_menu(|ui| note_menu(ui, app, st, id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_view_request_reaches_the_engine() {
        let mut app = App::new(mailcraft_engine::sample::sample_session());
        let notes =
            app.s.mailbox.folders.values().find(|f| f.kind == FolderKind::Notes).map(|f| f.id).unwrap();
        app.run("nav.selectFolder", json!({"folder": notes.0}));
        let mut st = ModState::default();
        apply_view(&mut st, &json!("small"));
        legacy_request(&mut app, &mut st);
        let v = mailcraft_engine::cmd::view::modules::state(&app.s, notes, ModuleKind::Notes);
        assert_eq!(v.icon_mode, IconMode::Small);
        // Applied once: changing the engine view later is not undone.
        app.run("notes.iconView", json!({"mode": "large"}));
        legacy_request(&mut app, &mut st);
        let v = mailcraft_engine::cmd::view::modules::state(&app.s, notes, ModuleKind::Notes);
        assert_eq!(v.icon_mode, IconMode::Large);
        assert_eq!(order(&app).len(), 2);
    }

    #[test]
    fn view_from_results() {
        let mut st = ModState::default();
        apply_view(&mut st, &json!("list"));
        assert_eq!(st.notes_view.icon_mode, IconMode::List);
        apply_view(&mut st, &json!({ "view": { "iconMode": "small", "linedUp": true } }));
        assert_eq!(st.notes_view.icon_mode, IconMode::Small);
        assert!(st.notes_view.lined_up);
        apply_view(&mut st, &json!(42));
        assert_eq!(st.notes_view.icon_mode, IconMode::Small);
    }

    #[test]
    fn commit_saves_changed_text_only() {
        let mut app = App::new(mailcraft_engine::sample::sample_session());
        let mut st = ModState::default();
        let id = *app.s.mailbox.notes.keys().next().unwrap();
        let before = app.s.mailbox.notes[&id].modified;
        start_edit(&app, &mut st, id);
        commit_edit(&mut app, &mut st);
        assert_eq!(app.s.mailbox.notes[&id].modified, before);
        start_edit(&app, &mut st, id);
        if let Some((_, t)) = st.note_edit.as_mut() {
            *t = "Changed\nbody".into();
        }
        commit_edit(&mut app, &mut st);
        assert_eq!(app.s.mailbox.notes[&id].text, "Changed\nbody");
        new_note(&mut app, &mut st);
        assert!(st.note_edit.is_some());
    }
}
