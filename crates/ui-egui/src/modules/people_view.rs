//! People module: My Contacts (folder pane) and the folder's current view: People (letter index, contact
//! list with search and the contact card), Business Card (business cards), Card (address cards), Phone and
//! List (tables with the view's columns, grouped by the arrangement, collapsible; a header click sorts, the
//! Flag Status and Categories cells take the Quick Click flag and category). The reading pane (contact card)
//! follows View › Reading Pane in the People, Phone and List views. The rows, their order and groups come
//! from the engine (`mailcraft_engine::cmd::people::view_rows`); the columns from
//! `mailcraft_engine::cmd::view::table`.

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
    group_header, load, module_folder_pane, module_view, nested_headers, paint_text_clipped, pane_bg,
    pane_separator, persona_fill, reading_split, row_font, row_height, select_folder, select_item, store,
    sync_selection, todo_bar_strip, view_switch, view_switch_mode, view_switch_width,
};
use super::editors;
use super::table as mt;
use crate::App;
use mailcraft_engine::cmd::view::table;

const INDEX_W: f32 = 30.0;
/// The narrowest search box the People bar keeps before the view switch gives up its labels.
const SEARCH_MIN_W: f32 = 140.0;
/// The narrowest search box beside a labelled New Contact button.
const SEARCH_NARROW_W: f32 = 90.0;
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

/// The contact's picture (while People › "Show user photographs when available" is on, `photos`), else a
/// persona circle with initials.
fn avatar(ui: &Ui, photos: bool, center: egui::Pos2, radius: f32, c: &Contact) {
    let rect = egui::Rect::from_center_size(center, egui::Vec2::splat(radius * 2.0));
    if photos && super::common::paint_contact_picture(ui, rect, c) {
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
            // Fit the row before drawing it: the view switch takes the room the left-hand controls leave
            // (labels, else icons, else a menu); then New Contact drops its label and the search box narrows.
            let bar_w = ui.available_width();
            let gap = ui.spacing().item_spacing.x;
            let side = crate::widgets::icon_side(ui);
            let new_w = crate::widgets::button_width(ui, "person", "New Contact", false);
            let group_w = side + 4.0;
            let min_left = SEARCH_MIN_W + new_w + group_w + 3.0 * gap;
            let mode = view_switch_mode(ui, &VIEWS, bar_w - min_left);
            let rest = bar_w - view_switch_width(ui, &VIEWS, mode) - 3.0 * gap - group_w;
            let new_label = rest >= SEARCH_NARROW_W + new_w;
            let new_space = if new_label { new_w } else { side + 4.0 };
            let search_w = (rest - new_space - ui.spacing().button_padding.x * 2.0).clamp(40.0, 220.0);
            ui.add(
                TextEdit::singleline(&mut st.people_search)
                    .hint_text("Search contacts")
                    .desired_width(search_w),
            );
            let new = if new_label {
                crate::widgets::icon_button(ui, app, "person", "New Contact", true)
            } else {
                crate::widgets::icon_only(ui, app, "person", "New Contact", true)
            };
            if new.clicked() {
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
                if let Some(v) = view_switch(ui, app, &VIEWS, &view.view, mode) {
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
    let table = matches!(view.view.as_str(), "Phone" | "List");
    let cards_view = matches!(view.view.as_str(), "Business Card" | "Card");
    let list_w = if table || cards_view { (rest.width() * 0.62).max(420.0) } else { 300.0 };
    // Every view takes the reading pane where the folder put it (the card views too).
    let (list, pane) = reading_split(rest, view.reading_pane, list_w, 0.55);
    match view.view.as_str() {
        "Business Card" => business_cards(ui, app, st, list, &ids, folder),
        "Card" => cards(ui, app, st, list, &ids, folder),
        _ if table => child(ui, list, "mc-people-table", |ui| table_view(ui, app, st, folder, rows)),
        _ => child(ui, list, "mc-people-list", |ui| contact_list(ui, app, st, &ids)),
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

/// The Phone and List views: a table of the contacts with the view's columns (Show Columns), grouped by the
/// arrangement (collapsible). A header click sorts by the column; the Flag Status and Categories cells take
/// a click (the Quick Click flag and category).
fn table_view(ui: &mut Ui, app: &mut App, st: &mut ModState, folder: Option<FolderId>, rows: &[ViewRow]) {
    let cols = folder.map(|f| table::shown(&app.s, f)).unwrap_or_default();
    let sort = folder.and_then(|f| table::sort(&app.s, f));
    let rules = folder.map(|f| table::rules(&app.s, f)).unwrap_or_default();
    let other = folder.map(|f| table::other(&app.s, f)).unwrap_or_default();
    let width = ui.available_width();
    let laid = mt::layout(width, &cols);
    let (head, _) = ui.allocate_exact_size(vec2(width, mt::head_height(&other)), Sense::hover());
    if let Some(c) = mt::header(ui, app, head, &laid, sort.as_ref(), "mc-people", &other) {
        mt::sort_by_column(app, sort.as_ref(), c);
    }
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
    let name_size = row_font(&other, 12.5);
    let cell_size = row_font(&other, 12.0);
    let row_h = row_height(app, 26.0).max((name_size * 2.0).round());
    ScrollArea::vertical().id_salt("mc-people-table-scroll").auto_shrink([false, false]).show(ui, |ui| {
        for (k, r) in rows.iter().enumerate() {
            let hidden = nested_headers(
                ui,
                app,
                rows,
                k,
                |x: &ViewRow| x.levels.as_slice(),
                |x: &ViewRow| x.id,
                &collapsed,
            );
            if hidden {
                continue;
            }
            let Some(c) = app.s.mailbox.contacts.get(&r.id).cloned() else { continue };
            // Other Settings › AutoPreview: the first lines of the contact's notes under the row.
            let notes: String = if other.auto_preview {
                c.notes
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .take(3)
                    .map(|l| l.chars().take(300).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                String::new()
            };
            let extra = if notes.is_empty() { 0.0 } else { 15.0 * notes.lines().count() as f32 + 2.0 };
            let (full, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_h + extra), Sense::click());
            let rect = Rect::from_min_size(full.min, vec2(full.width(), row_h));
            let name = display_name(&c);
            a11y(&resp, WidgetType::Button, &name);
            if app.ui.module_selection == Some(r.id.0) {
                ui.painter().rect_filled(full, CornerRadius::same(3), app.theme.selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(full, CornerRadius::same(3), app.theme.hover);
            }
            let style = table::row_style(&app.s, &rules, table::Item::Contact(&c));
            let (color, look) = mt::row_look(&style, app.theme.text, app.theme.danger, app.theme.text_muted);
            let hovered = resp.hovered();
            let mut flag_click = false;
            let mut cat_click = false;
            for l in &laid {
                let cell = Rect::from_min_size(pos2(rect.min.x + l.x, rect.min.y), vec2(l.w.max(1.0), row_h));
                match l.col.id {
                    "icon" => {
                        let icon = if is_group(&c) {
                            "contact-group"
                        } else if c.private {
                            "private"
                        } else {
                            "person"
                        };
                        mt::paint_icon(ui, app, cell, icon);
                    }
                    "flagStatus" => {
                        let look = match c.flag {
                            FlagState::Flagged { .. } => mt::FlagLook::Flagged,
                            FlagState::Complete { .. } => mt::FlagLook::Complete,
                            FlagState::None => mt::FlagLook::None,
                        };
                        let label = format!("Flag {name}");
                        flag_click |= mt::flag_cell(ui, app, cell, look, hovered, ("people", r.id.0), &label);
                    }
                    "categories" => {
                        cat_click |=
                            mt::categories_cell(ui, app, cell, &c.categories, hovered, ("people", r.id.0));
                    }
                    id => {
                        let text = table::contact_cell(&c, id);
                        let size = if id == "fullName" { name_size } else { cell_size };
                        let tint =
                            if id == "fullName" || color != app.theme.text { color } else { app.theme.text };
                        mt::styled_cell(ui, cell, &text, size, look, tint, l.align);
                    }
                }
            }
            if !notes.is_empty() {
                let x = laid
                    .iter()
                    .find(|l| l.col.id == "fullName")
                    .map_or(rect.min.x + 30.0, |l| rect.min.x + l.x + 4.0);
                let area = Rect::from_min_max(pos2(x, rect.max.y - 2.0), full.max);
                let galley = ui.painter().layout(
                    notes,
                    FontId::proportional(11.5),
                    app.theme.text_muted,
                    (full.max.x - x - 8.0).max(10.0),
                );
                ui.painter().with_clip_rect(area).galley(
                    pos2(x, rect.max.y - 2.0),
                    galley,
                    app.theme.text_muted,
                );
            }
            if !other.no_grid {
                ui.painter().hline(full.x_range(), full.max.y, Stroke::new(0.5, app.theme.separator));
            }
            if flag_click {
                // Unflagged: the Quick Click flag; flagged: complete; complete: clear (like the mail list).
                let kind = match c.flag {
                    FlagState::None => None,
                    FlagState::Flagged { .. } => Some("complete"),
                    FlagState::Complete { .. } => Some("clear"),
                };
                select_item(app, Some(r.id.0));
                let p = match kind {
                    Some(k) => json!({ "ids": [r.id.0], "kind": k }),
                    None => json!({ "ids": [r.id.0] }),
                };
                app.run("people.flag", p);
                continue;
            }
            if cat_click {
                select_item(app, Some(r.id.0));
                app.run("people.categorize", json!({ "ids": [r.id.0] }));
                continue;
            }
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

/// egui memory: the additional index is the one shown (People › "Show an additional index").
fn alt_index_id() -> Id {
    Id::new("mc-people-index-alt")
}

/// Accessible names of the index switch.
pub const SHOW_ADDITIONAL_INDEX: &str = "Show the additional index";
pub const SHOW_LATIN_INDEX: &str = "Show the a to z index";

fn letter_index(ui: &mut Ui, app: &mut App, st: &mut ModState, rect: Rect, ids: &[ItemId]) {
    // People › Contacts index › "Show an additional index": a switch at the bottom flips the bar between a–z
    // and the chosen alphabet (Arabic, Cyrillic, Greek, Hebrew, Thai or Vietnamese).
    let extra = mailcraft_engine::cmd::people::additional_index(&app.s);
    let showing_alt =
        extra.is_some() && ui.ctx().data(|d| d.get_temp::<bool>(alt_index_id())).unwrap_or(false);
    if let Some(letters) = extra.filter(|_| showing_alt) {
        alt_letter_index(ui, app, st, rect, ids, letters);
        return;
    }
    let slots = INDEX.len() + usize::from(extra.is_some());
    let present: std::collections::BTreeSet<char> = ids
        .iter()
        .filter_map(|id| app.s.mailbox.contacts.get(id))
        .map(|c| bucket(&display_name(c)))
        .collect();
    let h = ((rect.height() - 8.0) / slots as f32).clamp(10.0, 23.0);
    if let Some(letters) = extra {
        let r = Rect::from_min_size(
            pos2(rect.min.x, rect.min.y + 4.0 + h * INDEX.len() as f32),
            vec2(INDEX_W, h),
        );
        let label: String = letters.iter().take(2).collect();
        index_switch(ui, app, r, &label, SHOW_ADDITIONAL_INDEX, true);
    }
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

/// The switch button below the index letters.
fn index_switch(ui: &mut Ui, app: &App, r: Rect, label: &str, name: &str, to_alt: bool) {
    let resp = ui.interact(r, Id::new(("mc-people-index-switch", to_alt)), Sense::click());
    a11y(&resp, WidgetType::Button, name);
    let fill = if resp.hovered() { app.theme.hover } else { app.theme.group_header };
    ui.painter().rect_filled(r.shrink(2.0), CornerRadius::same(3), fill);
    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(10.5), app.theme.accent);
    if resp.clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(alt_index_id(), to_alt));
    }
}

/// The additional index: its letters (dimmed where no contact starts with one), then the a–z switch. A
/// letter jumps to the first contact (list order) whose name starts with it.
fn alt_letter_index(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    rect: Rect,
    ids: &[ItemId],
    letters: &[char],
) {
    use mailcraft_engine::cmd::people::index_bucket;
    let names: Vec<(ItemId, Option<usize>)> = ids
        .iter()
        .filter_map(|id| {
            app.s.mailbox.contacts.get(id).map(|c| (*id, index_bucket(&display_name(c), letters)))
        })
        .collect();
    let h = ((rect.height() - 8.0) / (letters.len() + 1) as f32).clamp(8.0, 23.0);
    for (k, &c) in letters.iter().enumerate() {
        let r = Rect::from_min_size(pos2(rect.min.x, rect.min.y + 4.0 + h * k as f32), vec2(INDEX_W, h));
        let resp = ui.interact(r, Id::new(("mc-people-index-alt", k)), Sense::click());
        a11y(&resp, WidgetType::Button, &format!("Jump to {c}"));
        if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(3), app.theme.hover);
        }
        let first = names.iter().find(|(_, b)| *b == Some(k)).map(|(id, _)| *id);
        let color = if first.is_some() { app.theme.text } else { app.theme.text_muted.gamma_multiply(0.6) };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, c, FontId::proportional(11.5), color);
        if resp.clicked() {
            st.people_jump = Some(c);
        }
    }
    let r =
        Rect::from_min_size(pos2(rect.min.x, rect.min.y + 4.0 + h * letters.len() as f32), vec2(INDEX_W, h));
    index_switch(ui, app, r, "az", SHOW_LATIN_INDEX, false);
}

/// Resolves a pending letter jump to the first contact at or after that letter (of the a–z index, or of the
/// additional index for its letters).
fn jump_target(app: &App, st: &ModState, ids: &[ItemId]) -> Option<ItemId> {
    let want = st.people_jump?;
    if !INDEX.contains(&want)
        && let Some(letters) = mailcraft_engine::cmd::people::additional_index(&app.s)
        && let Some(k) = letters.iter().position(|l| *l == want)
    {
        let at_or_after = |id: &ItemId| {
            app.s.mailbox.contacts.get(id).is_some_and(|c| {
                mailcraft_engine::cmd::people::index_bucket(&display_name(c), letters).is_some_and(|b| b >= k)
            })
        };
        return ids.iter().copied().find(at_or_after).or_else(|| ids.last().copied());
    }
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
            avatar(
                ui,
                app.s.settings.bool("people.photos"),
                pos2(rect.min.x + 30.0, rect.center().y),
                17.0,
                &c,
            );
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
            if c.private {
                let r =
                    Rect::from_min_size(pos2(rect.max.x - 46.0, rect.center().y - 7.0), Vec2::splat(14.0));
                let lock = ui.interact(r, Id::new(("mc-people-private", id.0)), Sense::hover());
                a11y(&lock, WidgetType::Label, &format!("Private {name}"));
                app.icons.paint(ui, r, "private", app.theme.text_muted);
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
        avatar(ui, app.s.settings.bool("people.photos"), pos2(head.min.x + 40.0, head.center().y), 38.0, &c);
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
            if c.private {
                info_row(ui, app, "private", "Sensitivity", "Private");
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

/// Width of a card in the Card view.
const CARD_W: f32 = 250.0;
/// Text size of the field labels on a card.
const CARD_LABEL_SIZE: f32 = 11.5;

/// Width of a card's label column: the widest field label (so "Business Phone" shows whole), at least 60
/// points and at most 45 % of the card (a very long custom label is cut, not the values).
fn card_label_width(ui: &Ui, labels: &[&str]) -> f32 {
    let font = FontId::proportional(CARD_LABEL_SIZE);
    let widest = labels
        .iter()
        .map(|l| ui.painter().layout_no_wrap((*l).to_owned(), font.clone(), Color32::WHITE).size().x)
        .fold(0.0_f32, f32::max);
    (widest.ceil() + 2.0).clamp(60.0, CARD_W * 0.45)
}

fn cards(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    rect: Rect,
    ids: &[ItemId],
    folder: Option<FolderId>,
) {
    if ids.is_empty() {
        empty_message(ui, rect, app, "No contacts", "Add someone with New Contact.");
        return;
    }
    let target = jump_target(app, st, ids);
    // The card's fields are the view's columns (View Settings › Fields); Conditional Formatting colours the
    // name.
    let fields = folder.map(|f| table::shown(&app.s, f)).unwrap_or_default();
    let rules = folder.map(|f| table::rules(&app.s, f)).unwrap_or_default();
    child(ui, rect.shrink(8.0), "mc-people-cards", |ui| {
        ScrollArea::vertical().id_salt("mc-people-cards-scroll").auto_shrink([false, false]).show(ui, |ui| {
            let card_w = CARD_W;
            let card_h = 150.0;
            let labels: Vec<&str> =
                fields.iter().filter(|f| f.col.id != "icon").map(|f| f.label.as_str()).collect();
            let label_w = card_label_width(ui, &labels);
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
                let style = table::row_style(&app.s, &rules, table::Item::Contact(&c));
                let (color, look) =
                    mt::row_look(&style, app.theme.text, app.theme.danger, app.theme.text_muted);
                mt::styled_cell(
                    ui,
                    Rect::from_min_size(pos2(head.min.x + 4.0, head.min.y), vec2(card_w - 8.0, 28.0)),
                    &name,
                    13.5,
                    look,
                    color,
                    "left",
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
                        FontId::proportional(CARD_LABEL_SIZE),
                        app.theme.text_muted,
                        label_w,
                    );
                    let value_x = 8.0 + label_w + 8.0;
                    paint_text_clipped(
                        ui,
                        pos2(card.min.x + value_x, y),
                        value,
                        FontId::proportional(12.0),
                        app.theme.text,
                        card_w - value_x - 8.0,
                    );
                    y += 18.0;
                };
                for f in fields.iter().filter(|f| f.col.id != "icon") {
                    line(&f.label, &table::contact_cell(&c, f.col.id));
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
fn business_cards(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    rect: Rect,
    ids: &[ItemId],
    folder: Option<FolderId>,
) {
    if ids.is_empty() {
        empty_message(ui, rect, app, "No contacts", "Add someone with New Contact.");
        return;
    }
    let target = jump_target(app, st, ids);
    let rules = folder.map(|f| table::rules(&app.s, f)).unwrap_or_default();
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
                    avatar(
                        ui,
                        app.s.settings.bool("people.photos"),
                        pos2(card.min.x + 46.0, card.min.y + 46.0),
                        28.0,
                        &c,
                    );
                    let lines = if is_group(&c) {
                        vec![name.clone(), format!("Contact group · {} members", c.emails.len())]
                    } else {
                        business_card_lines(&c)
                    };
                    let x = card.min.x + 86.0;
                    let mut y = card.min.y + 12.0;
                    // Conditional Formatting styles the name line.
                    let style = table::row_style(&app.s, &rules, table::Item::Contact(&c));
                    let (name_color, look) =
                        mt::row_look(&style, app.theme.text, app.theme.danger, app.theme.text_muted);
                    for (i, l) in lines.iter().enumerate() {
                        if y > card.max.y - 18.0 {
                            break;
                        }
                        if i == 0 {
                            let band =
                                Rect::from_min_size(pos2(x - 4.0, y - 2.0), vec2(card.max.x - x - 6.0, 22.0));
                            mt::styled_cell(ui, band, l, 15.0, look, name_color, "left");
                        } else {
                            paint_text_clipped(
                                ui,
                                pos2(x, y),
                                l,
                                FontId::proportional(12.0),
                                app.theme.text_muted,
                                card.max.x - x - 10.0,
                            );
                        }
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

    /// Release check: "Business Phone" was cut to "Business P..." on every card (fixed 70-point labels).
    #[test]
    fn card_labels_fit_the_longest_field_name() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let labels = ["Full Name", "Company", "Business Phone", "E-mail"];
            let w = card_label_width(ui, &labels);
            let font = FontId::proportional(CARD_LABEL_SIZE);
            let need = ui.painter().layout_no_wrap("Business Phone".into(), font, Color32::WHITE).size().x;
            assert!(w >= need, "label column {w} narrower than \"Business Phone\" ({need})");
            assert!(w <= CARD_W * 0.45);
            // A very long label is cut, the value column keeps room.
            let long = card_label_width(ui, &["A field label far too long to show whole on a card"]);
            assert!((long - CARD_W * 0.45).abs() < 0.01);
            assert!(card_label_width(ui, &[]) >= 60.0);
        });
        out.textures_delta.clear();
    }
}
