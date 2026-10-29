//! Folder pane: Favorites + mailbox trees, badges, minimised strip, drag targets (folders area).
//!
//! Behaviour from `plan/reference/folders.md` and `plan/reference/layout.md`: a collapsible Favorites section,
//! then one tree per account (root row 34 px, folder rows 24 px), expand chevrons, unread badges (total in
//! brackets for Drafts / Junk Email / Outbox), a « button that minimises the pane to a 26 px strip with the
//! Favorites written vertically, and folders that accept messages dragged from the list. Rows come from
//! `queries::folder_rows`; every change goes through [`App::run`].

use std::collections::BTreeSet;

use egui::epaint::TextShape;
use egui::{Id, Rect, Sense, Stroke, Ui, UiBuilder, WidgetInfo, WidgetType, pos2, vec2};
use mailcraft_engine::Module;
use mailcraft_engine::queries::{self, FolderRow, FolderRowKind};
use mailcraft_model::{FolderId, FolderKind, FolderPaneState, FolderRole};
use serde_json::{Value, json};

use crate::App;
use crate::list::{DraggedItems, Text, icon_rect};
use crate::responsive::Frame;
use crate::theme::size;

/// Folder-pane UI state kept in egui memory.
#[derive(Clone, Debug, Default)]
struct PaneState {
    /// Collapsed section headers ("fav", "acct:<name>"); the engine always reports them expanded.
    collapsed: BTreeSet<String>,
    /// Auto-minimised pane temporarily shown as a flyout.
    flyout: bool,
    /// Row last clicked: (is a Favorites row, folder id). A folder listed both under Favorites and in its
    /// mailbox is highlighted only where it was picked.
    picked: Option<(bool, u64)>,
}

fn state_id() -> Id {
    Id::new("mailcraft.folders.state")
}

fn run(ui: &Ui, app: &mut App, id: &str, params: Value) {
    app.run(id, params);
    ui.ctx().request_repaint();
}

/// Icon for a folder by role / kind.
fn folder_icon(role: Option<FolderRole>, kind: FolderKind, open: bool) -> &'static str {
    match role {
        Some(FolderRole::Inbox) => "inbox",
        Some(FolderRole::Drafts) => "drafts",
        Some(FolderRole::SentItems) => "sent",
        Some(FolderRole::DeletedItems) => "deleted",
        Some(FolderRole::Archive) => "archive-folder",
        Some(FolderRole::ConversationHistory) => "conversation",
        Some(FolderRole::JunkEmail) => "junk-folder",
        Some(FolderRole::Outbox) => "outbox",
        Some(FolderRole::RssFeeds) => "rss",
        Some(FolderRole::SearchFolders) => "search-folder",
        Some(FolderRole::SyncIssues) => "warning",
        Some(FolderRole::Calendar) => "calendar",
        Some(FolderRole::Contacts) => "people",
        Some(FolderRole::Journal) => "note",
        Some(FolderRole::Notes) => "notes",
        Some(FolderRole::Tasks) => "tasks",
        None => match kind {
            FolderKind::Calendar => "calendar",
            FolderKind::Contacts => "people",
            FolderKind::Tasks => "tasks",
            FolderKind::Notes => "notes",
            FolderKind::Journal => "note",
            FolderKind::SearchRoot | FolderKind::Search => "search-folder",
            FolderKind::Mail if open => "folder-open",
            FolderKind::Mail => "folder",
        },
    }
}

/// Section key for a header row.
fn section_key(row: &FolderRow) -> String {
    match row.kind {
        FolderRowKind::FavoritesHeader => "fav".to_owned(),
        _ => format!("acct:{}", row.name),
    }
}

/// Account id (u32) of an account root row (matched by address, then display name).
fn account_of(app: &App, name: &str) -> Option<u32> {
    let accounts = &app.s.mailbox.accounts;
    accounts
        .iter()
        .find(|a| a.address == name)
        .or_else(|| accounts.iter().find(|a| a.display_name == name))
        .map(|a| a.id.0)
}

/// Folders that accept dropped messages.
fn accepts_mail(app: &App, id: FolderId) -> bool {
    app.s.mailbox.folder(id).is_some_and(|f| f.kind == FolderKind::Mail)
}

/// Moves (or with Ctrl, copies) dropped messages into `folder`; items already there are left alone.
fn drop_items(ui: &Ui, app: &mut App, ids: &[u64], folder: FolderId) {
    let copy = ui.input(|i| i.modifiers.command);
    let ids: Vec<u64> = ids
        .iter()
        .copied()
        .filter(|id| {
            copy || app.s.mailbox.mail.get(mailcraft_model::ItemId(*id)).is_some_and(|m| m.folder != folder)
        })
        .collect();
    if ids.is_empty() {
        return;
    }
    let cmd = if copy { "mail.copy" } else { "mail.move" };
    crate::list::run_on_items(ui, app, cmd, &ids, json!({ "folder": folder.0 }));
}

pub fn folder_pane(ui: &mut Ui, app: &mut App, frame: &Frame) {
    let mode = app.s.view.folder_pane;
    if mode == FolderPaneState::Off {
        return;
    }
    let ctx = ui.ctx().clone();
    let mut st: PaneState = ctx.data_mut(|d| d.get_temp(state_id())).unwrap_or_default();
    let rect = ui.available_rect_before_wrap();
    if rect.width() >= 1.0 && rect.height() >= 1.0 {
        // Narrow windows (≤ 800 wide) and Reading view minimise a Normal pane without changing the setting;
        // its › then opens the tree as a flyout instead of restoring the pane.
        let auto =
            (frame.folder_auto_minimized || app.s.view.reading_view) && mode == FolderPaneState::Normal;
        if mode == FolderPaneState::Minimized || auto {
            minimized_strip(ui, app, rect, auto, &mut st);
        } else {
            st.flyout = false;
            full_pane(ui, app, rect, &mut st);
        }
        ui.advance_cursor_after_rect(rect);
    }
    ctx.data_mut(|d| d.insert_temp(state_id(), st));
}

fn full_pane(ui: &mut Ui, app: &mut App, rect: Rect, st: &mut PaneState) {
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    egui::ScrollArea::vertical().id_salt("mailcraft.folders.scroll").auto_shrink([false, false]).show(
        &mut child,
        |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            tree(ui, app, st);
        },
    );
    // « collapse button, top-right (on top of the first row).
    let th = app.theme.clone();
    let b = Rect::from_min_size(pos2(rect.right() - 19.0 - 6.0, rect.top() + 7.5), vec2(19.0, 19.0));
    let resp = ui.interact(b, Id::new("mailcraft.folders.minimize"), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(b, 3.0, th.hover);
    }
    app.icons.paint(ui, icon_rect(b.center(), 14.0), "chevron-left", th.text_muted);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Minimize the Folder Pane"));
    if resp.on_hover_text("Minimize the Folder Pane").clicked() {
        run(ui, app, "view.folderPane", json!({ "mode": "minimized" }));
    }
}

/// Draws the Favorites section and account trees; returns true when a folder was selected.
fn tree(ui: &mut Ui, app: &mut App, st: &mut PaneState) -> bool {
    let rows = queries::folder_rows(&app.s);
    let highlight = highlighted_row(&rows, st);
    let mut skipping = false;
    let mut navigated = false;
    for (i, row) in rows.iter().enumerate() {
        match row.kind {
            FolderRowKind::FavoritesHeader | FolderRowKind::AccountRoot => {
                if i > 0 {
                    section_separator(ui, &app.theme);
                }
                let key = section_key(row);
                let collapsed = st.collapsed.contains(&key);
                skipping = collapsed;
                if section_row(ui, app, row, collapsed) {
                    if collapsed {
                        st.collapsed.remove(&key);
                    } else {
                        st.collapsed.insert(key);
                    }
                }
            }
            FolderRowKind::Favorite | FolderRowKind::Folder => {
                if !skipping {
                    let fav = row.kind == FolderRowKind::Favorite;
                    if folder_row(ui, app, row, highlight == Some(i)) {
                        navigated = true;
                        st.picked = row.id.map(|f| (fav, f.0));
                    }
                }
            }
        }
    }
    // Leave a little room at the bottom so the last row is not flush with the pane edge.
    ui.add_space(8.0);
    navigated
}

/// Index of the one visible row drawn as selected: the row the user picked when it shows the selected
/// folder, else the first visible row of the selected folder.
fn highlighted_row(rows: &[FolderRow], st: &PaneState) -> Option<usize> {
    let mut skipping = false;
    let mut first = None;
    for (i, row) in rows.iter().enumerate() {
        match row.kind {
            FolderRowKind::FavoritesHeader | FolderRowKind::AccountRoot => {
                skipping = st.collapsed.contains(&section_key(row));
            }
            FolderRowKind::Favorite | FolderRowKind::Folder if !skipping && row.selected => {
                let fav = row.kind == FolderRowKind::Favorite;
                if st.picked.is_some() && st.picked == row.id.map(|f| (fav, f.0)) {
                    return Some(i);
                }
                first = first.or(Some(i));
            }
            _ => {}
        }
    }
    first
}

/// Hairline between the Favorites section and each mailbox tree.
fn section_separator(ui: &mut Ui, th: &crate::theme::Theme) {
    let w = ui.available_width();
    let (_, rect) = ui.allocate_space(vec2(w, 9.0));
    ui.painter().hline(
        (rect.left() + 6.0)..=(rect.right() - 6.0),
        rect.center().y,
        Stroke::new(1.0, th.separator),
    );
}

/// Section header (Favorites / account root). Returns true when it was toggled.
fn section_row(ui: &mut Ui, app: &mut App, row: &FolderRow, collapsed: bool) -> bool {
    let th = app.theme.clone();
    let w = ui.available_width();
    let (_, rect) = ui.allocate_space(vec2(w, size::ACCOUNT_ROW));
    let key = section_key(row);
    let resp = ui.interact(rect, Id::new(("mailcraft.folders.section", &key)), Sense::click());
    let painter = ui.painter().clone();
    if resp.hovered() {
        painter.rect_filled(rect, 3.0, th.hover);
    }
    let chevron = if collapsed { "chevron-right" } else { "chevron-down" };
    app.icons.paint(ui, icon_rect(pos2(rect.left() + 12.0, rect.center().y), 12.0), chevron, th.text_muted);
    let (font, bold) = if row.kind == FolderRowKind::FavoritesHeader { (13.5, true) } else { (14.0, true) };
    // Leave room for the « button on the first row.
    let t = Text::new(ui, &row.name, font, bold, th.text, (rect.width() - 56.0).max(1.0), 1);
    t.paint_left(&painter, rect, rect.left() + 24.0);
    let state = if collapsed { "collapsed" } else { "expanded" };
    resp.widget_info(|| {
        WidgetInfo::labeled(WidgetType::CollapsingHeader, true, format!("{}, {state}", row.accessible))
    });
    let toggled = resp.clicked();
    if row.kind == FolderRowKind::AccountRoot
        && let Some(account) = account_of(app, &row.name)
    {
        resp.context_menu(|ui| crate::menus::account_menu(ui, app, account));
    }
    toggled
}

/// Widget id (keyboard focus target) of a folder's row in the folder pane: its Favorites row when `favorite`,
/// else its row in the account tree. A folder listed in both has one row of each.
pub fn row_focus_id(folder: FolderId, favorite: bool) -> Id {
    Id::new(("mailcraft.folders.row", if favorite { "fav" } else { "tree" }, folder.0))
}

/// One folder (or favourite) row; `highlight` = drawn as the selected row. Returns true when it was clicked
/// to select its folder.
fn folder_row(ui: &mut Ui, app: &mut App, row: &FolderRow, highlight: bool) -> bool {
    let Some(fid) = row.id else { return false };
    let th = app.theme.clone();
    let w = ui.available_width();
    let (_, rect) = ui.allocate_space(vec2(w, size::FOLDER_ROW));
    let resp = ui.interact(rect, row_focus_id(fid, row.kind == FolderRowKind::Favorite), Sense::click());
    let tag = if row.kind == FolderRowKind::Favorite { "fav" } else { "tree" };
    let painter = ui.painter().clone();

    // Drop target for messages dragged from the list.
    let droppable = accepts_mail(app, fid);
    let drop_hover = droppable && resp.dnd_hover_payload::<DraggedItems>().is_some();
    let fill = if drop_hover {
        Some(th.selected)
    } else if highlight {
        Some(th.selected_folder)
    } else if resp.hovered() {
        Some(th.hover)
    } else {
        None
    };
    if let Some(fill) = fill {
        painter.rect_filled(rect.shrink2(vec2(4.0, 0.0)), 3.0, fill);
    }
    if drop_hover {
        painter.rect_stroke(
            rect.shrink2(vec2(4.0, 0.0)),
            3.0,
            Stroke::new(1.0, th.accent),
            egui::StrokeKind::Inside,
        );
    }

    let depth = row.depth.min(12) as f32;
    let indent = rect.left() + 6.0 + 14.0 * (depth - 1.0).max(0.0);
    let chevron_rect = Rect::from_min_size(pos2(indent, rect.top()), vec2(16.0, rect.height()));
    let folder = app.s.mailbox.folder(fid);
    let (role, kind) = folder.map(|f| (f.role, f.kind)).unwrap_or((None, FolderKind::Mail));
    let icon = folder_icon(role, kind, row.has_children && row.expanded);
    let icon_c = pos2(indent + 26.0, rect.center().y);
    app.icons.paint(ui, icon_rect(icon_c, 16.0), icon, th.text_muted);

    // Badge (right).
    let mut name_right = rect.right() - 10.0;
    if !row.badge.is_empty() {
        let t = Text::new(ui, &row.badge, 12.5, !row.badge_total, th.unread, 80.0, 1);
        name_right = t.paint_right(&painter, rect, rect.right() - 10.0) - 6.0;
    }
    let name_x = icon_c.x + 14.0;
    let renaming = row.kind != FolderRowKind::Favorite
        && app.ui.inline_rename.as_ref().is_some_and(|r| r.folder == fid.0);
    if renaming {
        let edit =
            Rect::from_min_max(pos2(name_x - 2.0, rect.top() + 2.0), pos2(name_right, rect.bottom() - 2.0));
        inline_rename(ui, app, fid, edit);
    } else {
        let t = Text::new(ui, &row.name, 13.5, false, th.text, (name_right - name_x).max(1.0), 1);
        t.paint_left(&painter, rect, name_x);
    }

    let mut navigated = false;
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, highlight, &row.accessible));
    if resp.clicked() {
        if !row.selected {
            run(ui, app, "nav.selectFolder", json!({ "folder": fid.0 }));
        }
        navigated = true;
    }
    if droppable && let Some(payload) = resp.dnd_release_payload::<DraggedItems>() {
        drop_items(ui, app, &payload.0, fid);
    }
    if row.kind == FolderRowKind::Favorite {
        resp.context_menu(|ui| crate::menus::favorite_folder_menu(ui, app, fid));
    } else {
        resp.context_menu(|ui| crate::menus::folder_menu(ui, app, fid));
    }

    // Expand / collapse chevron (on top of the row).
    if row.has_children {
        let chevron = if row.expanded { "chevron-down" } else { "chevron-right" };
        app.icons.paint(ui, icon_rect(chevron_rect.center(), 12.0), chevron, th.text_muted);
        let cresp =
            ui.interact(chevron_rect, Id::new(("mailcraft.folders.chevron", tag, fid.0)), Sense::click());
        let label = if row.expanded { "Collapse" } else { "Expand" };
        cresp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{label} {}", row.name)));
        if cresp.clicked() {
            let cmd = if row.expanded { "folder.collapse" } else { "folder.expand" };
            run(ui, app, cmd, json!({ "folder": fid.0 }));
        }
    }
    if resp.double_clicked() && row.has_children {
        let cmd = if row.expanded { "folder.collapse" } else { "folder.expand" };
        run(ui, app, cmd, json!({ "folder": fid.0 }));
    }
    navigated
}

/// The name box of a folder renamed in place: Enter (or leaving the box) renames, Escape keeps the name.
fn inline_rename(ui: &mut Ui, app: &mut App, fid: FolderId, rect: Rect) {
    let Some(mut r) = app.ui.inline_rename.take() else { return };
    let id = Id::new(("mailcraft.folders.rename", fid.0));
    let resp = ui.put(rect, egui::TextEdit::singleline(&mut r.text).id(id).desired_width(rect.width()));
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, "Folder name"));
    if !r.focused {
        resp.request_focus();
        r.focused = true;
    }
    r.drawn = true;
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        return;
    }
    if resp.lost_focus() {
        let name = r.text.trim().to_owned();
        let current = app.s.mailbox.folder(fid).map(|f| f.name.clone()).unwrap_or_default();
        if name.is_empty() || name == current {
            return;
        }
        if crate::menus::run_folder(app, fid, "folder.rename", json!({"folder": fid.0, "name": name}))
            .is_none()
        {
            // Refused (a duplicate name…): keep editing; the toast says why.
            r.focused = false;
            app.ui.inline_rename = Some(r);
        }
        return;
    }
    app.ui.inline_rename = Some(r);
}

/// Minimised pane: › button and the Favorites (or the current folder) written top to bottom.
fn minimized_strip(ui: &mut Ui, app: &mut App, rect: Rect, auto: bool, st: &mut PaneState) {
    let th = app.theme.clone();
    let strip = rect;
    let painter = ui.painter_at(strip);
    let b = Rect::from_min_size(pos2(strip.center().x - 9.5, strip.top() + 7.5), vec2(19.0, 19.0));
    let resp = ui.interact(b, Id::new("mailcraft.folders.expand"), Sense::click());
    if resp.hovered() || st.flyout {
        painter.rect_filled(b, 3.0, th.hover);
    }
    app.icons.paint(ui, icon_rect(b.center(), 14.0), "chevron-right", th.text_muted);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Expand the Folder Pane"));
    let expand_clicked = resp.on_hover_text("Expand the Folder Pane").clicked();
    if expand_clicked {
        if auto {
            st.flyout = !st.flyout;
        } else {
            run(ui, app, "view.folderPane", json!({ "mode": "normal" }));
        }
    }

    // Vertical entries: Favorites in the Mail module, else the selected folder.
    let rows = queries::folder_rows(&app.s);
    let mut entries: Vec<(FolderId, String, String, bool, String)> = rows
        .iter()
        .filter(|r| r.kind == FolderRowKind::Favorite)
        .filter_map(|r| Some((r.id?, r.name.clone(), r.badge.clone(), r.selected, r.accessible.clone())))
        .collect();
    if entries.is_empty() || app.s.view.module != Module::Mail {
        let sel = app.s.selection.folder;
        if let Some(f) = app.s.mailbox.folder(sel) {
            let counts = app.s.folder_counts(sel);
            let badge = app.s.mailbox.badge_with(sel, counts).unwrap_or_default();
            let name = app.s.mailbox.folder_accessible_name_with(sel, counts);
            entries = vec![(sel, f.name.clone(), badge, true, name)];
        }
    }
    let mut y = b.bottom() + 12.0;
    for (fid, name, badge, selected, accessible) in entries {
        if y >= strip.bottom() - 20.0 {
            break;
        }
        let label = if badge.is_empty() { name.clone() } else { format!("{name}  {badge}") };
        let max_len = (strip.bottom() - y - 12.0).max(1.0);
        let t = Text::new(ui, &label, 13.0, false, th.text, max_len, 1);
        let len = t.width();
        let item = Rect::from_min_size(pos2(strip.left(), y - 6.0), vec2(strip.width(), len + 12.0));
        let iresp = ui.interact(item, Id::new(("mailcraft.folders.vertical", fid.0)), Sense::click());
        let drop_hover = accepts_mail(app, fid) && iresp.dnd_hover_payload::<DraggedItems>().is_some();
        if drop_hover {
            painter.rect_filled(item.shrink2(vec2(2.0, 0.0)), 3.0, th.selected);
        } else if selected {
            painter.rect_filled(item.shrink2(vec2(2.0, 0.0)), 3.0, th.selected_folder);
        } else if iresp.hovered() {
            painter.rect_filled(item.shrink2(vec2(2.0, 0.0)), 3.0, th.hover);
        }
        // Rotated a quarter turn clockwise about its top-left corner: the text runs downwards and its
        // height extends to the left, so the pivot sits half a line right of the strip's centre.
        let pos = pos2(strip.center().x + t.height() / 2.0, y);
        painter.add(TextShape::new(pos, t.galley.clone(), th.text).with_angle(std::f32::consts::FRAC_PI_2));
        iresp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, &accessible));
        if iresp.clicked() && !selected {
            run(ui, app, "nav.selectFolder", json!({ "folder": fid.0 }));
        }
        if accepts_mail(app, fid)
            && let Some(payload) = iresp.dnd_release_payload::<DraggedItems>()
        {
            drop_items(ui, app, &payload.0, fid);
        }
        iresp.context_menu(|ui| crate::menus::folder_menu(ui, app, fid));
        y = item.bottom() + 10.0;
    }

    // Flyout: the full tree over the content while the window is too narrow for the pane.
    if auto && st.flyout {
        let pos = pos2(strip.right() + 2.0, strip.top());
        let area = egui::Area::new(Id::new("mailcraft.folders.flyout"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).fill(th.pane).show(ui, |ui| {
                    ui.set_width(size::FOLDER_PANE);
                    ui.set_max_height((strip.height() - 16.0).max(100.0));
                    egui::ScrollArea::vertical()
                        .id_salt("mailcraft.folders.flyout.scroll")
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            tree(ui, app, st)
                        })
                        .inner
                })
            });
        let navigated = area.inner.inner;
        let clicked_outside = ui.input(|i| i.pointer.any_click())
            && !area.response.contains_pointer()
            && !expand_clicked
            && !egui::Popup::is_any_open(ui.ctx());
        if navigated || clicked_outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            st.flyout = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use mailcraft_engine::sample::{SAMPLE_USER_ADDR, sample_session};

    fn harness(app: App, size: egui::Vec2) -> Harness<'static, App> {
        Harness::builder().with_size(size).build_ui_state(
            |ui, app: &mut App| {
                let frame = Frame::compute(1600.0, 900.0);
                folder_pane(ui, app, &frame);
            },
            app,
        )
    }

    #[test]
    fn renders_favorites_and_tree_with_badges() {
        let app = App::new(sample_session());
        let rows = queries::folder_rows(&app.s);
        let mut h = harness(app, vec2(230.0, 900.0));
        h.run();
        assert!(h.query_all_by_label_contains("Favorites").next().is_some());
        assert!(h.query_all_by_label_contains(SAMPLE_USER_ADDR).next().is_some());
        assert!(h.query_all_by_label("Drafts: 2 items").next().is_some());
        let inbox = rows.iter().find(|r| r.kind == FolderRowKind::Folder && r.name == "Inbox").unwrap();
        assert!(inbox.accessible.contains("unread"));
        assert_eq!(h.query_all_by_label(&inbox.accessible).count(), 2, "favourite + tree row");
        assert!(h.query_by_label("Minimize the Folder Pane").is_some());
    }

    #[test]
    fn rename_folder_in_place() {
        let mut app = App::new(sample_session());
        let projects = app.s.mailbox.folders.values().find(|f| f.name == "Projects").map(|f| f.id.0).unwrap();
        crate::dialogs::open(&mut app, "renameFolder", json!({"folder": projects, "name": ""}));
        assert!(app.ui.dialogs.is_empty(), "renamed in the folder pane, not a dialog");
        let mut h = harness(app, vec2(230.0, 900.0));
        h.run();
        h.get_by_label("Folder name").type_text("Ventures");
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        h.run();
        let name = h.state().s.mailbox.folder(FolderId(projects)).unwrap().name.clone();
        assert_eq!(name, "Ventures");
        assert!(h.state().ui.inline_rename.is_none());
    }

    #[test]
    fn select_expand_and_collapse() {
        let app = App::new(sample_session());
        let mut h = harness(app, vec2(230.0, 900.0));
        h.run();
        h.query_all_by_label("Drafts: 2 items").last().unwrap().click();
        h.run();
        let s = &h.state().s;
        assert_eq!(s.mailbox.folder(s.selection.folder).unwrap().role, Some(FolderRole::Drafts));
        // Inbox has a subfolder: collapse it with its chevron.
        h.get_by_label("Collapse Inbox").click();
        h.run();
        assert!(h.query_all_by_label_contains("Projects").next().is_none());
        h.get_by_label("Expand Inbox").click();
        h.run();
        assert!(h.query_all_by_label_contains("Projects").next().is_some());
        // Favorites section collapses locally.
        h.query_all_by_label_contains("Favorites").next().unwrap().click();
        h.run();
        assert_eq!(h.query_all_by_label("Drafts: 2 items").count(), 1);
    }

    #[test]
    fn folder_listed_twice_is_highlighted_where_picked() {
        use egui_kittest::kittest::NodeT;
        let app = App::new(sample_session());
        let inbox = queries::folder_rows(&app.s)
            .into_iter()
            .find(|r| r.kind == FolderRowKind::Folder && r.name == "Inbox")
            .unwrap()
            .accessible;
        let mut h = harness(app, vec2(230.0, 900.0));
        h.run();
        let selected = |h: &Harness<'static, App>| -> Vec<bool> {
            h.query_all_by_label(&inbox)
                .map(|n| n.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True))
                .collect()
        };
        assert_eq!(selected(&h), [true, false], "Favorites row first by default");
        h.query_all_by_label(&inbox).last().unwrap().click();
        h.run();
        assert_eq!(selected(&h), [false, true], "the mailbox row once picked there");
        h.query_all_by_label(&inbox).next().unwrap().click();
        h.run();
        assert_eq!(selected(&h), [true, false]);
    }

    #[test]
    fn row_focus_ids_name_the_drawn_rows() {
        let app = App::new(sample_session());
        let rows = queries::folder_rows(&app.s);
        let inbox = rows.iter().find(|r| r.kind == FolderRowKind::Folder && r.name == "Inbox").unwrap();
        let fid = inbox.id.unwrap();
        let mut h = harness(app, vec2(230.0, 900.0));
        h.run();
        let tree = h.ctx.read_response(row_focus_id(fid, false)).unwrap();
        let fav = h.ctx.read_response(row_focus_id(fid, true)).unwrap();
        assert!(fav.rect.top() < tree.rect.top(), "the Favorites row is drawn above the tree row");
        assert_ne!(row_focus_id(fid, false), row_focus_id(fid, true));
        // Focusing the id focuses that row (what F6 does).
        h.ctx.memory_mut(|m| m.request_focus(row_focus_id(fid, false)));
        h.run();
        assert!(h.ctx.memory(|m| m.has_focus(row_focus_id(fid, false))));
    }

    #[test]
    fn minimize_and_restore() {
        let app = App::new(sample_session());
        let mut h = harness(app, vec2(230.0, 900.0));
        h.run();
        h.get_by_label("Minimize the Folder Pane").click();
        h.run();
        assert_eq!(h.state().s.view.folder_pane, FolderPaneState::Minimized);
        assert!(h.query_by_label("Expand the Folder Pane").is_some());
        // Vertical favourites are still there and clickable.
        let drafts = h.query_all_by_label("Drafts: 2 items").next();
        assert!(drafts.is_some());
        h.get_by_label("Expand the Folder Pane").click();
        h.run();
        assert_eq!(h.state().s.view.folder_pane, FolderPaneState::Normal);
    }

    #[test]
    fn auto_minimised_flyout() {
        let mut h = Harness::builder().with_size(vec2(600.0, 900.0)).build_ui_state(
            |ui, app: &mut App| {
                let frame = Frame::compute(700.0, 900.0);
                ui.allocate_ui(vec2(size::FOLDER_PANE_MIN, 880.0), |ui| folder_pane(ui, app, &frame));
            },
            App::new(sample_session()),
        );
        h.run();
        assert!(h.query_by_label("Collapse Inbox").is_none(), "tree hidden in the strip");
        h.get_by_label("Expand the Folder Pane").click();
        h.run();
        assert_eq!(
            h.state().s.view.folder_pane,
            FolderPaneState::Normal,
            "auto-minimised: setting unchanged"
        );
        assert!(h.query_by_label("Collapse Inbox").is_some(), "flyout shows the tree");
        h.query_all_by_label("Drafts: 2 items").last().unwrap().click();
        h.run();
        assert!(h.query_by_label("Collapse Inbox").is_none(), "flyout closes after navigating");
    }

    #[test]
    fn reading_view_shows_the_strip_and_keeps_the_setting() {
        let mut s = sample_session();
        s.run("view.readingView", &json!({})).unwrap();
        let mut h = Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            |ui, app: &mut App| {
                let frame = Frame::compute(1600.0, 900.0);
                ui.allocate_ui(vec2(size::FOLDER_PANE_MIN, 880.0), |ui| folder_pane(ui, app, &frame));
            },
            App::new(s),
        );
        h.run();
        assert!(h.query_by_label("Collapse Inbox").is_none(), "Reading view: no full tree in the strip");
        assert!(h.query_by_label("Minimize the Folder Pane").is_none());
        h.get_by_label("Expand the Folder Pane").click();
        h.run();
        assert_eq!(h.state().s.view.folder_pane, FolderPaneState::Normal);
        assert!(h.query_by_label("Collapse Inbox").is_some(), "flyout shows the tree");
    }

    #[test]
    fn off_draws_nothing() {
        let mut s = sample_session();
        s.run("view.folderPane", &json!({"mode": "off"})).unwrap();
        let mut h = harness(App::new(s), vec2(230.0, 600.0));
        h.run();
        assert!(h.query_all_by_label_contains("Favorites").next().is_none());
    }

    #[test]
    fn drop_moves_items_but_skips_their_own_folder() {
        let s = sample_session();
        let inbox = s.selection.folder;
        let deleted = s.mailbox.role_folder(s.current_account().unwrap(), FolderRole::DeletedItems).unwrap();
        let id = s.mailbox.items_in(inbox).first().unwrap().id.0;
        let mut h = Harness::new_ui_state(
            |ui, app: &mut App| {
                drop_items(ui, app, &[id], deleted);
                drop_items(ui, app, &[id], deleted);
            },
            App::new(s),
        );
        h.step();
        let app = h.state();
        assert_eq!(app.s.mailbox.mail.get(mailcraft_model::ItemId(id)).unwrap().folder, deleted);
        assert!(app.ui.toast.is_none(), "{:?}", app.ui.toast);
    }

    #[test]
    fn icons_cover_roles() {
        assert_eq!(folder_icon(Some(FolderRole::Inbox), FolderKind::Mail, false), "inbox");
        assert_eq!(folder_icon(None, FolderKind::Mail, true), "folder-open");
        assert_eq!(folder_icon(None, FolderKind::Search, false), "search-folder");
    }
}
