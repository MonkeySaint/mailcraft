//! People module: My Contacts (folder pane) and the folder's current view: People (letter index, contact
//! list with search and the contact card), Business Card (business cards), Card (address cards), Phone and
//! List (tables grouped by the arrangement, collapsible). The reading pane (contact card) follows View ›
//! Reading Pane in the People, Phone and List views. The rows, their order and groups come from the engine
//! (`mailcraft_engine::cmd::people::view_rows`).

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Id, Key, Layout, Modifiers, Rect, RichText, ScrollArea,
    Sense, Stroke, TextEdit, Ui, Vec2, WidgetType, pos2, vec2,
};
use serde_json::json;

use mailcraft_engine::cmd::calendar::pim::Kind;
use mailcraft_engine::cmd::people::{business_card_lines, display_name, is_group};
use mailcraft_engine::cmd::view::modules::{ModuleKind, ModuleView, ViewRow};
use mailcraft_engine::queries::{initials, persona_color};
use mailcraft_model::{Contact, FlagState, FolderId, FolderKind, ItemId};

use super::common::{
    ModState, a11y, change_view, child, collapsed_labels, current_folder, empty_message, folder_rows,
    group_header, group_row, load, module_folder_pane, module_view, paint_text_clipped, pane_bg,
    pane_separator, persona_fill, reading_split, row_height, select_folder, select_item, store,
    sync_selection, todo_bar_strip, toggle_group, view_switch,
};
use super::editors;
use crate::App;

const INDEX_W: f32 = 30.0;
const ROW_H: f32 = 56.0;
const INDEX: [char; 27] = [
    '#', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't',
    'u', 'v', 'w', 'x', 'y', 'z',
];

/// Index bucket of a contact ('#' for anything that does not start with a letter a–z).
fn bucket(name: &str) -> char {
    match name.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some(c) if c.is_ascii_lowercase() => c,
        _ => '#',
    }
}

/// The People views with their toolbar icons.
const VIEWS: [(&str, &str); 5] = [
    ("People", "people"),
    ("Business Card", "contact-group"),
    ("Card", "person"),
    ("Phone", "phone"),
    ("List", "bullets"),
];

/// Rows of the current contacts folder's view matching the search.
fn view_rows(app: &App, st: &ModState) -> Vec<ViewRow> {
    let Some(folder) = current_folder(app, Kind::Contact) else { return Vec::new() };
    mailcraft_engine::cmd::people::view_rows(&app.s, folder, &st.people_search)
}

/// Contacts of the current folder matching the search, in list order.
#[cfg(test)]
fn contact_ids(app: &App, st: &ModState) -> Vec<ItemId> {
    view_rows(app, st).into_iter().map(|r| r.id).collect()
}

/// Applies the legacy People/Card switch (`modules::set_people_view`) once.
fn legacy_request(app: &mut App, st: &mut ModState) {
    if st.people_cards != st.people_cards_seen {
        st.people_cards_seen = st.people_cards;
        change_view(app, if st.people_cards { "Business Card" } else { "People" });
    }
}

fn first_address(c: &Contact) -> String {
    c.emails.iter().find(|e| !e.trim().is_empty()).cloned().unwrap_or_default()
}

/// The contact's picture, else a persona circle with initials.
fn avatar(ui: &Ui, center: egui::Pos2, radius: f32, c: &Contact) {
    let rect = egui::Rect::from_center_size(center, egui::Vec2::splat(radius * 2.0));
    if super::common::paint_contact_picture(ui, rect, c) {
        return;
    }
    let name = display_name(c);
    let addr = first_address(c);
    let key = if addr.is_empty() { name.clone() } else { addr.clone() };
    let fill = persona_fill(persona_color(&key));
    ui.painter().circle_filled(center, radius, fill);
    let full = c.full_name();
    let text = initials(Some(if full.is_empty() { &name } else { &full }), &addr);
    ui.painter().text(
        center,
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(radius * 0.8),
        Color32::WHITE,
    );
}

pub(super) fn show(ui: &mut Ui, app: &mut App) {
    let ctx = ui.ctx().clone();
    let mut st = load(&ctx);
    super::common::ensure_folder(app, Kind::Contact);
    legacy_request(app, &mut st);
    let folder = current_folder(app, Kind::Contact);
    let view = module_view(app, folder, ModuleKind::People);
    let rows = view_rows(app, &st);
    sync_selection(app, |_, id| rows.iter().any(|r| r.id == id));
    let full = ui.max_rect();
    let (pane, main) = module_folder_pane(ui, app);
    if let Some(p) = pane {
        child(ui, p.shrink2(egui::vec2(6.0, 0.0)), "mc-people-pane", |ui| {
            group_header(ui, app, "My Contacts", false, "mc-people-group");
            let current = current_folder(app, Kind::Contact);
            if let (Some(f), _) = folder_rows(ui, app, FolderKind::Contacts, current, None) {
                select_folder(app, f);
                select_item(app, None);
            }
        });
    }
    let main = todo_bar_strip(ui, app, main);
    pane_bg(ui, main, app.theme.reading);
    child(ui, main, "mc-people-main", |ui| content(ui, app, &mut st, folder, &view, &rows));
    keyboard(&ctx, app, &mut st, &rows);
    ui.expand_to_include_rect(full);
    store(&ctx, st);
}

fn selected(app: &App) -> Option<ItemId> {
    app.ui.module_selection.map(ItemId).filter(|i| app.s.mailbox.contacts.contains_key(i))
}

fn keyboard(ctx: &egui::Context, app: &mut App, st: &mut ModState, rows: &[ViewRow]) {
    if ctx.memory(|m| m.focused().is_some()) {
        return;
    }
    let Some(sel) = selected(app) else { return };
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete)) {
        // The engine selects the neighbour; the view follows it.
        app.run("people.delete", json!({ "ids": [sel.0] }));
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        editors::open_contact(app, st, sel);
    } else if let Some(delta) = ctx.input_mut(|i| {
        if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
            Some(1i64)
        } else if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
            Some(-1)
        } else {
            None
        }
    }) && let Some(pos) = rows.iter().position(|r| r.id == sel)
    {
        let next = i64::try_from(pos).unwrap_or(0).saturating_add(delta);
        if let Some(r) = usize::try_from(next).ok().and_then(|i| rows.get(i)) {
            select_item(app, Some(r.id.0));
        }
    }
}

fn content(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    folder: Option<FolderId>,
    view: &ModuleView,
    rows: &[ViewRow],
) {
    let rect = ui.max_rect();
    let ids: Vec<ItemId> = rows.iter().map(|r| r.id).collect();
    // Toolbar: search, new, view switch.
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 44.0));
    child(ui, bar.shrink2(vec2(10.0, 7.0)), "mc-people-bar", |ui| {
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add(
                TextEdit::singleline(&mut st.people_search)
                    .hint_text("Search contacts")
                    .desired_width(220.0_f32.min(ui.available_width() * 0.4)),
            );
            if crate::widgets::icon_button(ui, app, "person", "New Contact", true).clicked() {
                let f = current_folder(app, Kind::Contact);
                editors::new_contact(app, st, f, false);
            }
            let group = crate::widgets::icon_only(ui, app, "contact-group", "New Contact Group", true);
            a11y(&group, WidgetType::Button, "New Contact Group");
            if group.clicked() {
                let f = current_folder(app, Kind::Contact);
                editors::new_contact(app, st, f, true);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(v) = view_switch(ui, app, &VIEWS, &view.view) {
                    change_view(app, &v);
                }
            });
        });
    });
    ui.painter().hline(rect.x_range(), bar.max.y, Stroke::new(1.0, app.theme.separator));
    let body = Rect::from_min_max(pos2(rect.min.x, bar.max.y + 1.0), rect.max);
    if body.height() < 30.0 {
        return;
    }
    // Like the observed People view, a contact is always selected while the list has any (the first one on
    // entry, after a delete or when a search hides the selected one).
    if st.people_jump.is_none()
        && selected(app).filter(|s| ids.contains(s)).is_none()
        && let Some(first) = ids.first()
    {
        select_item(app, Some(first.0));
    }
    let index = Rect::from_min_size(body.min, vec2(INDEX_W, body.height()));
    letter_index(ui, app, st, index, &ids);
    let rest = Rect::from_min_max(pos2(index.max.x, body.min.y), body.max);
    match view.view.as_str() {
        "Business Card" => return business_cards(ui, app, st, rest, &ids),
        "Card" => return cards(ui, app, st, rest, &ids),
        _ => {}
    }
    let table = matches!(view.view.as_str(), "Phone" | "List");
    let list_w = if table { (rest.width() * 0.62).max(420.0) } else { 300.0 };
    let (list, pane) = reading_split(rest, view.reading_pane, list_w, 0.55);
    if table {
        child(ui, list, "mc-people-table", |ui| table_view(ui, app, st, folder, &view.view, rows));
    } else {
        child(ui, list, "mc-people-list", |ui| contact_list(ui, app, st, &ids));
    }
    if let Some(details) = pane {
        pane_separator(ui, app, list, details);
        match selected(app).filter(|s| ids.contains(s)) {
            Some(id) => child(ui, details.shrink2(vec2(24.0, 16.0)), "mc-people-card", |ui| {
                contact_card(ui, app, st, id)
            }),
            None => empty_message(
                ui,
                details,
                app,
                "No contact selected",
                "Pick someone from the list to see their details.",
            ),
        }
    }
}

/// Columns of the Phone and List views: (header, value of a contact).
type Column = (&'static str, fn(&Contact) -> String);

fn phone(c: &Contact, kind: &str) -> String {
    c.phones.iter().find(|(k, _)| k.eq_ignore_ascii_case(kind)).map(|(_, n)| n.clone()).unwrap_or_default()
}

const PHONE_COLUMNS: [Column; 7] = [
    ("Full Name", |c| if is_group(c) { display_name(c) } else { c.full_name() }),
    ("Company", |c| c.company.clone()),
    ("File As", display_name),
    ("Business Phone", |c| phone(c, "Business")),
    ("Business Fax", |c| phone(c, "Business Fax")),
    ("Home Phone", |c| phone(c, "Home")),
    ("Mobile Phone", |c| phone(c, "Mobile")),
];

const LIST_COLUMNS: [Column; 7] = [
    ("Full Name", |c| if is_group(c) { display_name(c) } else { c.full_name() }),
    ("Job Title", |c| c.job_title.clone()),
    ("Company", |c| c.company.clone()),
    ("File As", display_name),
    ("Department", |c| c.department.clone()),
    ("Business Phone", |c| phone(c, "Business")),
    ("Email", |c| c.emails.first().cloned().unwrap_or_default()),
];

/// The Phone and List views: a table of the contacts, grouped by the arrangement (collapsible).
fn table_view(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    folder: Option<FolderId>,
    view: &str,
    rows: &[ViewRow],
) {
    let columns: &[Column] = if view == "Phone" { &PHONE_COLUMNS } else { &LIST_COLUMNS };
    let width = ui.available_width();
    // The first column gets a double share.
    let unit = (width - 32.0).max(80.0) / (columns.len() as f32 + 1.0);
    let col_x = |i: usize| 28.0 + unit * (i as f32 + if i > 0 { 1.0 } else { 0.0 });
    let col_w = |i: usize| if i == 0 { unit * 2.0 } else { unit } - 6.0;
    let (head, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    for (i, (name, _)) in columns.iter().enumerate() {
        let cell =
            Rect::from_min_size(pos2(head.min.x + col_x(i), head.min.y), vec2(col_w(i).max(1.0), 22.0));
        let resp = ui.interact(cell, Id::new(("mc-people-col", i)), Sense::hover());
        a11y(&resp, WidgetType::Label, name);
        paint_text_clipped(
            ui,
            pos2(head.min.x + col_x(i), head.min.y + 5.0),
            name,
            FontId::proportional(11.5),
            app.theme.text_muted,
            col_w(i),
        );
    }
    ui.painter().hline(head.x_range(), head.max.y - 1.0, Stroke::new(1.0, app.theme.separator));
    if rows.is_empty() {
        let hint = if st.people_search.trim().is_empty() {
            "Add someone with New Contact."
        } else {
            "Nothing matches the search."
        };
        empty_message(ui, ui.available_rect_before_wrap(), app, "No contacts", hint);
        return;
    }
    let collapsed = folder.map(|f| collapsed_labels(app, f)).unwrap_or_default();
    let row_h = row_height(app, 26.0);
    ScrollArea::vertical().id_salt("mc-people-table-scroll").auto_shrink([false, false]).show(ui, |ui| {
        let mut last: Option<&str> = None;
        for (k, r) in rows.iter().enumerate() {
            if let Some(g) = r.group.as_deref()
                && last != Some(g)
            {
                last = Some(g);
                let count = rows.iter().skip(k).take_while(|x| x.group.as_deref() == Some(g)).count();
                let shut = collapsed.contains(g);
                if group_row(ui, app, g, count, shut) {
                    toggle_group(app, r.id, shut);
                }
            }
            if r.group.as_ref().is_some_and(|g| collapsed.contains(g)) {
                continue;
            }
            let Some(c) = app.s.mailbox.contacts.get(&r.id).cloned() else { continue };
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
            a11y(&resp, WidgetType::Button, &display_name(&c));
            if app.ui.module_selection == Some(r.id.0) {
                ui.painter().rect_filled(rect, CornerRadius::same(3), app.theme.selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, CornerRadius::same(3), app.theme.hover);
            }
            let icon = if is_group(&c) { "contact-group" } else { "person" };
            app.icons.paint(
                ui,
                Rect::from_center_size(pos2(rect.min.x + 12.0, rect.center().y), Vec2::splat(14.0)),
                icon,
                app.theme.text_muted,
            );
            if matches!(c.flag, FlagState::Flagged { .. }) {
                app.icons.paint(
                    ui,
                    Rect::from_center_size(pos2(rect.max.x - 10.0, rect.center().y), Vec2::splat(14.0)),
                    "flag",
                    app.theme.flag,
                );
            }
            for (i, (_, value)) in columns.iter().enumerate() {
                paint_text_clipped(
                    ui,
                    pos2(rect.min.x + col_x(i), rect.center().y - 8.0),
                    &value(&c),
                    FontId::proportional(12.5),
                    app.theme.text,
                    col_w(i),
                );
            }
            ui.painter().hline(rect.x_range(), rect.max.y, Stroke::new(0.5, app.theme.separator));
            if resp.clicked() {
                select_item(app, Some(r.id.0));
            }
            if resp.double_clicked() {
                editors::open_contact(app, st, r.id);
            }
            resp.context_menu(|ui| row_menu(ui, app, st, r.id));
        }
    });
}

fn letter_index(ui: &mut Ui, app: &mut App, st: &mut ModState, rect: Rect, ids: &[ItemId]) {
    let present: std::collections::BTreeSet<char> = ids
        .iter()
        .filter_map(|id| app.s.mailbox.contacts.get(id))
        .map(|c| bucket(&display_name(c)))
        .collect();
    let h = ((rect.height() - 8.0) / INDEX.len() as f32).clamp(10.0, 23.0);
    for (k, &c) in INDEX.iter().enumerate() {
        let r = Rect::from_min_size(pos2(rect.min.x, rect.min.y + 4.0 + h * k as f32), vec2(INDEX_W, h));
        let label = if c == '#' { "123".to_owned() } else { c.to_string() };
        let resp = ui.interact(r, Id::new(("mc-people-index", c)), Sense::click());
        a11y(&resp, WidgetType::Button, &format!("Jump to {label}"));
        if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(3), app.theme.hover);
        }
        let color =
            if present.contains(&c) { app.theme.text } else { app.theme.text_muted.gamma_multiply(0.6) };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.5), color);
        if resp.clicked() {
            st.people_jump = Some(c);
        }
    }
}

/// Resolves a pending letter jump to the first contact at or after that letter.
fn jump_target(app: &App, st: &ModState, ids: &[ItemId]) -> Option<ItemId> {
    let want = st.people_jump?;
    ids.iter()
        .copied()
        .find(|id| app.s.mailbox.contacts.get(id).is_some_and(|c| bucket(&display_name(c)) >= want))
        .or_else(|| ids.last().copied())
}

fn row_menu(ui: &mut Ui, app: &mut App, st: &mut ModState, id: ItemId) {
    select_item(app, Some(id.0));
    if ui.button("Open").clicked() {
        editors::open_contact(app, st, id);
        ui.close();
    }
    if ui.button("New Meeting with Contact").clicked() {
        app.run("people.meeting", json!({ "ids": [id.0] }));
        ui.close();
    }
    if ui.button("Assign Task").clicked() {
        app.run("people.assignTask", json!({ "ids": [id.0] }));
        ui.close();
    }
    ui.menu_button("Follow Up", |ui| {
        for (kind, label) in [
            ("today", "Today"),
            ("tomorrow", "Tomorrow"),
            ("thisWeek", "This Week"),
            ("nextWeek", "Next Week"),
            ("noDate", "No Date"),
            ("custom", "Custom..."),
            ("complete", "Mark Complete"),
            ("clear", "Clear Flag"),
        ] {
            if ui.button(label).clicked() {
                app.run("people.flag", json!({ "ids": [id.0], "kind": kind }));
                ui.close();
            }
        }
    });
    ui.menu_button("Categorize", |ui| {
        let cats: Vec<String> = app.s.mailbox.categories.iter().map(|c| c.name.clone()).collect();
        for c in cats {
            if ui.button(&c).clicked() {
                app.run("people.categorize", json!({ "id": id.0, "name": c }));
                ui.close();
            }
        }
    });
    if ui.button("Move to Folder...").clicked() {
        app.run("people.move", json!({ "ids": [id.0] }));
        ui.close();
    }
    if ui.button("Delete").clicked() {
        app.run("people.delete", json!({ "ids": [id.0] }));
        ui.close();
    }
}

fn contact_list(ui: &mut Ui, app: &mut App, st: &mut ModState, ids: &[ItemId]) {
    if ids.is_empty() {
        let hint = if st.people_search.trim().is_empty() {
            "Add someone with New Contact."
        } else {
            "Nothing matches the search."
        };
        empty_message(ui, ui.max_rect(), app, "No contacts", hint);
        return;
    }
    let target = jump_target(app, st, ids);
    ScrollArea::vertical().id_salt("mc-people-scroll").auto_shrink([false, false]).show(ui, |ui| {
        for &id in ids {
            let Some(c) = app.s.mailbox.contacts.get(&id).cloned() else { continue };
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_height(app, ROW_H)), Sense::click());
            let name = display_name(&c);
            a11y(&resp, WidgetType::Button, &name);
            let sel = app.ui.module_selection == Some(id.0);
            if sel {
                ui.painter().rect_filled(
                    rect.shrink2(vec2(4.0, 1.0)),
                    CornerRadius::same(4),
                    app.theme.selected,
                );
            } else if resp.hovered() {
                ui.painter().rect_filled(
                    rect.shrink2(vec2(4.0, 1.0)),
                    CornerRadius::same(4),
                    app.theme.hover,
                );
            }
            avatar(ui, pos2(rect.min.x + 30.0, rect.center().y), 17.0, &c);
            let x = rect.min.x + 56.0;
            let w = rect.max.x - x - 8.0;
            if is_group(&c) {
                let r =
                    Rect::from_min_size(pos2(rect.max.x - 26.0, rect.center().y - 8.0), Vec2::splat(16.0));
                app.icons.paint(ui, r, "contact-group", app.theme.text_muted);
            } else if matches!(c.flag, FlagState::Flagged { .. }) {
                let r =
                    Rect::from_min_size(pos2(rect.max.x - 26.0, rect.center().y - 8.0), Vec2::splat(16.0));
                app.icons.paint(ui, r, "flag", app.theme.flag);
            }
            paint_text_clipped(
                ui,
                pos2(x, rect.min.y + 10.0),
                &name,
                FontId::proportional(15.0),
                app.theme.text,
                w - 20.0,
            );
            let sub = if is_group(&c) {
                format!("{} members", c.emails.len())
            } else if !c.company.trim().is_empty() {
                c.company.clone()
            } else {
                first_address(&c)
            };
            paint_text_clipped(
                ui,
                pos2(x, rect.min.y + 31.0),
                &sub,
                FontId::proportional(12.0),
                app.theme.text_muted,
                w,
            );
            ui.painter().hline(x..=rect.max.x - 8.0, rect.max.y, Stroke::new(1.0, app.theme.separator));
            if target == Some(id) {
                ui.scroll_to_rect(rect, Some(Align::TOP));
                select_item(app, Some(id.0));
                st.people_jump = None;
            }
            if resp.clicked() {
                select_item(app, Some(id.0));
            }
            if resp.double_clicked() {
                editors::open_contact(app, st, id);
            }
            resp.context_menu(|ui| row_menu(ui, app, st, id));
        }
    });
}

/// One "label: value" line of the contact card with an icon.
fn info_row(ui: &mut Ui, app: &mut App, icon: &str, label: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    let lines = value.lines().count().max(1) as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0 + 17.0 * lines), Sense::hover());
    app.icons.paint(
        ui,
        Rect::from_min_size(pos2(rect.min.x, rect.min.y + 6.0), Vec2::splat(16.0)),
        icon,
        app.theme.text_muted,
    );
    ui.painter().text(
        pos2(rect.min.x + 28.0, rect.min.y + 2.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(12.0),
        app.theme.text_muted,
    );
    let galley = ui.painter().layout(
        value.to_owned(),
        FontId::proportional(13.5),
        app.theme.text,
        rect.width() - 30.0,
    );
    ui.painter().galley(pos2(rect.min.x + 28.0, rect.min.y + 18.0), galley, app.theme.text);
}

fn section(ui: &mut Ui, app: &App, title: &str) {
    ui.add_space(14.0);
    ui.label(RichText::new(title).size(15.0).strong().color(app.theme.text));
    ui.add_space(6.0);
}

fn contact_card(ui: &mut Ui, app: &mut App, st: &mut ModState, id: ItemId) {
    let Some(c) = app.s.mailbox.contacts.get(&id).cloned() else { return };
    ScrollArea::vertical().id_salt("mc-people-card-scroll").auto_shrink([false, false]).show(ui, |ui| {
        let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), 88.0), Sense::hover());
        avatar(ui, pos2(head.min.x + 40.0, head.center().y), 38.0, &c);
        let name = display_name(&c);
        let shown = if c.full_name().is_empty() || is_group(&c) { name.clone() } else { c.full_name() };
        paint_text_clipped(
            ui,
            pos2(head.min.x + 96.0, head.min.y + 16.0),
            &shown,
            FontId::proportional(24.0),
            app.theme.text,
            head.width() - 100.0,
        );
        let role = [c.job_title.trim(), c.department.trim(), c.company.trim()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" · ");
        let role = if is_group(&c) { "Contact group".to_owned() } else { role };
        paint_text_clipped(
            ui,
            pos2(head.min.x + 96.0, head.min.y + 52.0),
            &role,
            FontId::proportional(13.0),
            app.theme.text_muted,
            head.width() - 100.0,
        );
        ui.horizontal(|ui| {
            if crate::widgets::icon_button(ui, app, "settings", "Edit", true).clicked() {
                editors::open_contact(app, st, id);
            }
            if crate::widgets::icon_button(ui, app, "meeting", "Meeting", true).clicked() {
                app.run("people.meeting", json!({ "ids": [id.0] }));
            }
            if crate::widgets::icon_button(ui, app, "delete", "Delete", true).clicked() {
                app.run("people.delete", json!({ "ids": [id.0] }));
            }
        });
        ui.separator();
        if is_group(&c) {
            section(ui, app, "Members");
            if c.emails.is_empty() {
                ui.label(RichText::new("No members yet.").color(app.theme.text_muted));
            }
            for m in &c.emails {
                info_row(ui, app, "email", "Member", m);
            }
        } else {
            section(ui, app, "Contact information");
            for (i, e) in c.emails.iter().enumerate() {
                let label = if i == 0 { "Email".to_owned() } else { format!("Email {}", i + 1) };
                info_row(ui, app, "email", &label, e);
            }
            for (kind, number) in &c.phones {
                info_row(ui, app, "phone", &format!("{kind} phone"), number);
            }
            for a in &c.addresses {
                let line2 = [a.city.trim(), a.region.trim(), a.postal_code.trim()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" ");
                let text = [a.street.trim(), line2.as_str(), a.country.trim()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n");
                info_row(ui, app, "location", &format!("{} address", a.kind), &text);
            }
            info_row(ui, app, "link", "Web page", &c.web_page);
            info_row(ui, app, "person", "IM address", &c.im_address);
            if let Some(b) = &c.birthday {
                info_row(ui, app, "calendar", "Birthday", b);
            }
            if !c.categories.is_empty() {
                info_row(ui, app, "category-tag", "Categories", &c.categories.join(", "));
            }
            // Details page.
            let details = [
                ("Office", &c.office),
                ("Profession", &c.profession),
                ("Manager's name", &c.manager),
                ("Assistant's name", &c.assistant),
                ("Nickname", &c.nickname),
                ("Spouse/Partner", &c.spouse),
            ];
            if details.iter().any(|(_, v)| !v.trim().is_empty()) || c.anniversary.is_some() {
                section(ui, app, "Details");
                for (label, value) in details {
                    info_row(ui, app, "person", label, value);
                }
                if let Some(a) = &c.anniversary {
                    info_row(ui, app, "calendar", "Anniversary", a);
                }
            }
            if let mailcraft_model::FlagState::Flagged { label, reminder: Some(r), .. } = &c.flag {
                let when = mailcraft_model::ListTime::short_date_time(*r, app.s.tz_offset_min);
                info_row(ui, app, "flag", "Follow up", &format!("{label}. Reminder: {when}"));
            }
        }
        let notes = if is_group(&c) {
            c.notes.split_once('\n').map(|(_, r)| r.to_owned()).unwrap_or_default()
        } else {
            c.notes.clone()
        };
        section(ui, app, "Notes");
        if notes.trim().is_empty() {
            if ui.link("Add notes").clicked() {
                editors::open_contact(app, st, id);
            }
        } else {
            ui.label(RichText::new(notes).color(app.theme.text));
        }
    });
}

fn cards(ui: &mut Ui, app: &mut App, st: &mut ModState, rect: Rect, ids: &[ItemId]) {
    if ids.is_empty() {
        empty_message(ui, rect, app, "No contacts", "Add someone with New Contact.");
        return;
    }
    let target = jump_target(app, st, ids);
    child(ui, rect.shrink(8.0), "mc-people-cards", |ui| {
        ScrollArea::vertical().id_salt("mc-people-cards-scroll").auto_shrink([false, false]).show(ui, |ui| {
            let card_w = 236.0;
            let card_h = 150.0;
            let cols = ((ui.available_width() + 10.0) / (card_w + 10.0)).floor().max(1.0) as usize;
            let rows = ids.len().div_ceil(cols);
            let (area, _) = ui.allocate_exact_size(
                vec2(ui.available_width(), rows as f32 * (card_h + 10.0)),
                Sense::hover(),
            );
            for (k, &id) in ids.iter().enumerate() {
                let Some(c) = app.s.mailbox.contacts.get(&id).cloned() else { continue };
                let (r, col) = (k / cols, k % cols);
                let card = Rect::from_min_size(
                    pos2(area.min.x + col as f32 * (card_w + 10.0), area.min.y + r as f32 * (card_h + 10.0)),
                    vec2(card_w, card_h),
                );
                let resp = ui.interact(card, Id::new(("mc-people-cardbox", id.0)), Sense::click());
                let name = display_name(&c);
                a11y(&resp, WidgetType::Button, &name);
                let sel = app.ui.module_selection == Some(id.0);
                let p = ui.painter();
                p.rect_filled(card, CornerRadius::same(4), app.theme.pane);
                p.rect_stroke(
                    card,
                    CornerRadius::same(4),
                    Stroke::new(
                        if sel { 2.0 } else { 1.0 },
                        if sel { app.theme.accent } else { app.theme.separator },
                    ),
                    egui::StrokeKind::Inside,
                );
                let head = Rect::from_min_size(card.min, vec2(card_w, 28.0));
                p.rect_filled(
                    head.shrink(1.0),
                    CornerRadius::same(3),
                    if sel { app.theme.selected } else { app.theme.group_header },
                );
                paint_text_clipped(
                    ui,
                    pos2(head.min.x + 8.0, head.min.y + 6.0),
                    &name,
                    FontId::proportional(13.5),
                    app.theme.text,
                    card_w - 16.0,
                );
                let mut y = head.max.y + 6.0;
                let mut line = |label: &str, value: &str| {
                    if value.trim().is_empty() || y > card.max.y - 16.0 {
                        return;
                    }
                    paint_text_clipped(
                        ui,
                        pos2(card.min.x + 8.0, y),
                        label,
                        FontId::proportional(11.5),
                        app.theme.text_muted,
                        70.0,
                    );
                    paint_text_clipped(
                        ui,
                        pos2(card.min.x + 80.0, y),
                        value,
                        FontId::proportional(12.0),
                        app.theme.text,
                        card_w - 88.0,
                    );
                    y += 18.0;
                };
                line("Full name", &c.full_name());
                line("Company", &c.company);
                line("Job title", &c.job_title);
                for (i, e) in c.emails.iter().enumerate().take(2) {
                    line(if i == 0 { "Email" } else { "Email 2" }, e);
                }
                for (kind, n) in c.phones.iter().take(2) {
                    line(kind, n);
                }
                if target == Some(id) {
                    ui.scroll_to_rect(card, Some(Align::TOP));
                    select_item(app, Some(id.0));
                    st.people_jump = None;
                }
                if resp.clicked() {
                    select_item(app, Some(id.0));
                }
                if resp.double_clicked() {
                    editors::open_contact(app, st, id);
                }
                resp.context_menu(|ui| row_menu(ui, app, st, id));
            }
        });
    });
}

/// The Business Card view: each contact as a business card (picture, name, title, company, phones,
/// addresses, web page).
fn business_cards(ui: &mut Ui, app: &mut App, st: &mut ModState, rect: Rect, ids: &[ItemId]) {
    if ids.is_empty() {
        empty_message(ui, rect, app, "No contacts", "Add someone with New Contact.");
        return;
    }
    let target = jump_target(app, st, ids);
    child(ui, rect.shrink(8.0), "mc-people-bcards", |ui| {
        ScrollArea::vertical().id_salt("mc-people-bcards-scroll").auto_shrink([false, false]).show(
            ui,
            |ui| {
                let (card_w, card_h, gap) = (300.0, 170.0, 12.0);
                let cols = ((ui.available_width() + gap) / (card_w + gap)).floor().max(1.0) as usize;
                let rows = ids.len().div_ceil(cols);
                let (area, _) = ui.allocate_exact_size(
                    vec2(ui.available_width(), rows as f32 * (card_h + gap)),
                    Sense::hover(),
                );
                for (k, &id) in ids.iter().enumerate() {
                    let Some(c) = app.s.mailbox.contacts.get(&id).cloned() else { continue };
                    let card = Rect::from_min_size(
                        pos2(
                            area.min.x + (k % cols) as f32 * (card_w + gap),
                            area.min.y + (k / cols) as f32 * (card_h + gap),
                        ),
                        vec2(card_w, card_h),
                    );
                    let resp = ui.interact(card, Id::new(("mc-people-bcard", id.0)), Sense::click());
                    let name = display_name(&c);
                    a11y(&resp, WidgetType::Button, &format!("Business card {name}"));
                    let sel = app.ui.module_selection == Some(id.0);
                    let p = ui.painter();
                    p.rect_filled(card.translate(vec2(2.0, 2.0)), CornerRadius::same(6), app.theme.separator);
                    p.rect_filled(card, CornerRadius::same(6), app.theme.pane);
                    p.rect_stroke(
                        card,
                        CornerRadius::same(6),
                        Stroke::new(
                            if sel { 2.0 } else { 1.0 },
                            if sel { app.theme.accent } else { app.theme.separator },
                        ),
                        egui::StrokeKind::Inside,
                    );
                    // Accent band on the left, picture, then the card text.
                    p.rect_filled(
                        Rect::from_min_size(card.min, vec2(6.0, card_h)),
                        CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 },
                        app.theme.accent,
                    );
                    avatar(ui, pos2(card.min.x + 46.0, card.min.y + 46.0), 28.0, &c);
                    let lines = if is_group(&c) {
                        vec![name.clone(), format!("Contact group · {} members", c.emails.len())]
                    } else {
                        business_card_lines(&c)
                    };
                    let x = card.min.x + 86.0;
                    let mut y = card.min.y + 12.0;
                    for (i, l) in lines.iter().enumerate() {
                        if y > card.max.y - 18.0 {
                            break;
                        }
                        let (size, color) =
                            if i == 0 { (15.0, app.theme.text) } else { (12.0, app.theme.text_muted) };
                        paint_text_clipped(
                            ui,
                            pos2(x, y),
                            l,
                            FontId::proportional(size),
                            color,
                            card.max.x - x - 10.0,
                        );
                        y += if i == 0 { 22.0 } else { 17.0 };
                    }
                    if target == Some(id) {
                        ui.scroll_to_rect(card, Some(Align::TOP));
                        select_item(app, Some(id.0));
                        st.people_jump = None;
                    }
                    if resp.clicked() {
                        select_item(app, Some(id.0));
                    }
                    if resp.double_clicked() {
                        editors::open_contact(app, st, id);
                    }
                    resp.context_menu(|ui| row_menu(ui, app, st, id));
                }
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets() {
        assert_eq!(bucket("Rivera, Alice"), 'r');
        assert_eq!(bucket("6474479602"), '#');
        assert_eq!(bucket(""), '#');
        assert_eq!(bucket("Ünal"), '#');
    }

    #[test]
    fn search_filters() {
        let app = App::new(mailcraft_engine::sample::sample_session());
        let mut st = ModState::default();
        assert_eq!(contact_ids(&app, &st).len(), 6);
        st.people_search = "example org".into();
        assert_eq!(contact_ids(&app, &st).len(), 3);
        st.people_search = "0104".into();
        assert_eq!(contact_ids(&app, &st).len(), 1);
    }
}
