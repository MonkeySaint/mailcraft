//! Tasks module (classic Tasks / To-Do List): My Tasks (folder pane), the task list of the folder's current
//! view (Detailed, Simple List, To-Do List, Prioritized, Active, Completed, Today, Next 7 Days, Overdue,
//! Assigned, Server Tasks) with complete checkboxes, due dates and flags, grouped by the view's arrangement
//! (collapsible), a quick "new task" box, the task notes as message preview and a reading pane (right,
//! bottom or off). The rows, their filter, order and groups come from the engine
//! (`mailcraft_engine::cmd::tasks::view_rows`); View commands change them.

use egui::{
    Align, Align2, CornerRadius, FontId, Id, Key, Modifiers, Rect, RichText, ScrollArea, Sense, Stroke,
    TextEdit, Ui, Vec2, WidgetType, pos2, vec2,
};
use serde_json::{Value, json};

use mailcraft_engine::cmd::calendar::pim::Kind;
use mailcraft_engine::cmd::view::modules::{ModuleKind, ModuleView, RowKind, ViewRow, arrange_label};
use mailcraft_model::task::TaskRequestState;
use mailcraft_model::{FlagState, FolderId, FolderKind, ItemId, TaskPriority, TaskStatus};

use super::common::{
    DAY, ModState, a11y, change_view, child, collapsed_labels, current_folder, day_string, draw_check,
    empty_message, folder_rows, group_header, group_row, load, local_secs, long_date, module_folder_pane,
    module_view, paint_text_clipped, pane_bg, pane_separator, reading_split, row_height, select_folder,
    select_item, short_date, store, sync_selection, today, todo_bar_strip, toggle_group, tz, weekday_short,
};
use super::editors;
use crate::App;

const ROW_H: f32 = 30.0;

/// One row of the task list (a task, a flagged message or a flagged contact).
#[derive(Clone, Debug)]
pub(super) struct Row {
    pub id: ItemId,
    pub kind: RowKind,
    pub subject: String,
    pub due: Option<i64>,
    pub done: bool,
    pub priority: TaskPriority,
    pub group: Option<String>,
}

impl Row {
    /// A flagged message or contact (not a task).
    pub fn flagged_item(&self) -> bool {
        self.kind != RowKind::Task
    }
}

fn row_of(app: &App, r: &ViewRow) -> Option<Row> {
    let tz = tz(app);
    let day = |d: Option<mailcraft_model::DateTime>| d.map(|d| local_secs(d, tz).div_euclid(DAY));
    let (subject, due, done, priority) = match r.kind {
        RowKind::Task => {
            let t = app.s.mailbox.tasks.get(&r.id)?;
            (t.subject.clone(), day(t.due), t.status == TaskStatus::Completed, t.priority)
        }
        RowKind::Mail => {
            let m = app.s.mailbox.mail.get(&r.id)?;
            let due = match &m.flag {
                FlagState::Flagged { due, .. } => day(*due),
                _ => None,
            };
            let prio = match m.importance {
                mailcraft_model::Importance::High => TaskPriority::High,
                mailcraft_model::Importance::Low => TaskPriority::Low,
                mailcraft_model::Importance::Normal => TaskPriority::Normal,
            };
            (m.subject.clone(), due, false, prio)
        }
        RowKind::Contact => {
            let c = app.s.mailbox.contacts.get(&r.id)?;
            let due = match &c.flag {
                FlagState::Flagged { due, .. } => day(*due),
                _ => None,
            };
            (mailcraft_engine::cmd::people::display_name(c), due, false, TaskPriority::Normal)
        }
        RowKind::Note => return None,
    };
    Some(Row { id: r.id, kind: r.kind, subject, due, done, priority, group: r.group.clone() })
}

/// Rows of the current tasks folder's view.
pub(super) fn rows(app: &App) -> Vec<Row> {
    let Some(folder) = current_folder(app, Kind::Task) else { return Vec::new() };
    mailcraft_engine::cmd::tasks::view_rows(&app.s, folder).iter().filter_map(|r| row_of(app, r)).collect()
}

/// Applies the legacy To-Do List / folder switch (`modules::set_tasks_todo`) once.
fn legacy_request(app: &mut App, st: &mut ModState) {
    if st.tasks_folder_view != st.tasks_folder_seen {
        st.tasks_folder_seen = st.tasks_folder_view;
        change_view(app, if st.tasks_folder_view { "Simple List" } else { "To-Do List" });
    }
}

pub(super) fn show(ui: &mut Ui, app: &mut App) {
    let ctx = ui.ctx().clone();
    let mut st = load(&ctx);
    super::common::ensure_folder(app, Kind::Task);
    legacy_request(app, &mut st);
    let folder = current_folder(app, Kind::Task);
    let view = module_view(app, folder, ModuleKind::Tasks);
    let list = rows(app);
    sync_selection(app, |_, id| list.iter().any(|r| r.id == id));
    let full = ui.max_rect();
    let (pane, main) = module_folder_pane(ui, app);
    if let Some(p) = pane {
        child(ui, p.shrink2(egui::vec2(6.0, 0.0)), "mc-tasks-pane", |ui| folder_pane(ui, app, &view));
    }
    let main = todo_bar_strip(ui, app, main);
    pane_bg(ui, main, app.theme.reading);
    child(ui, main, "mc-tasks-main", |ui| content(ui, app, &mut st, folder, &view, &list));
    keyboard(&ctx, app, &mut st, &list);
    ui.expand_to_include_rect(full);
    store(&ctx, st);
}

fn folder_pane(ui: &mut Ui, app: &mut App, view: &ModuleView) {
    group_header(ui, app, "My Tasks", false, "mc-tasks-group");
    let todo = view.view == "To-Do List";
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), crate::theme::size::FOLDER_ROW), Sense::click());
    a11y(&resp, WidgetType::Button, "To-Do List");
    if todo {
        ui.painter().rect_filled(rect, CornerRadius::same(4), app.theme.selected_folder);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(4), app.theme.hover);
    }
    ui.painter().text(
        pos2(rect.min.x + 16.0, rect.center().y),
        Align2::LEFT_CENTER,
        "To-Do List",
        FontId::proportional(13.0),
        app.theme.text,
    );
    if resp.clicked() && !todo {
        change_view(app, "To-Do List");
        select_item(app, None);
    }
    let current = if todo { None } else { current_folder(app, Kind::Task) };
    if let (Some(f), _) = folder_rows(ui, app, FolderKind::Tasks, current, None) {
        select_folder(app, f);
        // A folder shows its own tasks: the To-Do List view spans every folder.
        let v = mailcraft_engine::cmd::view::modules::state(&app.s, f, ModuleKind::Tasks);
        if v.view == "To-Do List" {
            change_view(app, "Simple List");
        }
        select_item(app, None);
    }
}

fn selected<'a>(app: &App, list: &'a [Row]) -> Option<&'a Row> {
    let sel = app.ui.module_selection?;
    list.iter().find(|r| r.id.0 == sel)
}

fn keyboard(ctx: &egui::Context, app: &mut App, st: &mut ModState, list: &[Row]) {
    if ctx.memory(|m| m.focused().is_some()) {
        return;
    }
    let Some(row) = selected(app, list).cloned() else { return };
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete)) {
        if row.flagged_item() {
            app.run("tasks.removeFromList", json!({ "ids": [row.id.0] }));
        } else {
            app.run("tasks.delete", json!({ "ids": [row.id.0] }));
        }
        select_item(app, None);
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        open_row(app, st, &row);
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Insert)) {
        toggle_complete(app, &row);
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
        step(app, list, &row, 1);
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
        step(app, list, &row, -1);
    }
}

fn step(app: &mut App, list: &[Row], row: &Row, delta: i64) {
    let Some(pos) = list.iter().position(|r| r.id == row.id) else { return };
    let next = i64::try_from(pos).unwrap_or(0).saturating_add(delta);
    if let Some(r) = usize::try_from(next).ok().and_then(|i| list.get(i)) {
        select_item(app, Some(r.id.0));
    }
}

fn open_row(app: &mut App, st: &mut ModState, row: &Row) {
    match row.kind {
        RowKind::Mail => {
            app.run("mail.open", json!({ "ids": [row.id.0] }));
        }
        RowKind::Contact => editors::open_contact(app, st, row.id),
        _ => editors::open_task(app, st, row.id),
    }
}

/// A colour option of Options › Tasks (`#rrggbb`): the theme's own colour while the option keeps its default
/// (so light and dark themes stay legible), else the chosen colour.
pub(crate) fn option_color(app: &App, key: &str, theme: egui::Color32) -> egui::Color32 {
    let v = app.s.settings.text(key);
    let default = mailcraft_engine::settings::def(key).map(|d| d.default_value());
    if default.as_ref().and_then(Value::as_str) == Some(v.as_str()) {
        return theme;
    }
    mailcraft_geom::Color::parse_hex(&v).map_or(theme, |c| egui::Color32::from_rgb(c.r, c.g, c.b))
}

fn toggle_complete(app: &mut App, row: &Row) {
    if row.flagged_item() {
        app.run("tasks.markComplete", json!({ "ids": [row.id.0] }));
    } else {
        app.run("tasks.update", json!({ "id": row.id.0, "complete": !row.done }));
    }
}

fn content(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    folder: Option<FolderId>,
    view: &ModuleView,
    list: &[Row],
) {
    let rect = ui.max_rect();
    let list_w = if view.view == "Detailed" {
        (rect.width() * 0.65).max(420.0)
    } else {
        380.0_f32.max(rect.width() * 0.4).min(560.0)
    };
    let (left, pane) = reading_split(rect, view.reading_pane, list_w, 0.5);
    child(ui, left.shrink2(vec2(8.0, 8.0)), "mc-tasks-list", |ui| task_list(ui, app, st, folder, view, list));
    if let Some(right) = pane {
        pane_separator(ui, app, left, right);
        match selected(app, list).cloned() {
            Some(row) => child(ui, right.shrink2(vec2(24.0, 16.0)), "mc-tasks-details", |ui| {
                details(ui, app, st, &row)
            }),
            None => empty_message(ui, right, app, "No task selected", "Pick a task to see its details."),
        }
    }
}

/// Columns of the Detailed view (besides the subject).
const DETAILED: [&str; 4] = ["Status", "% Complete", "In Folder", "Categories"];
const DETAILED_W: f32 = 92.0;

fn task_list(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    folder: Option<FolderId>,
    view: &ModuleView,
    list: &[Row],
) {
    let arrange = view.arrangement(ModuleKind::Tasks).map(|a| arrange_label(ModuleKind::Tasks, a));
    let head = match arrange {
        Some(a) => format!("{} · Arrange by: {a}", view.view),
        None => view.view.clone(),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(head).size(12.0).color(app.theme.text_muted));
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if crate::widgets::icon_button(ui, app, "task", "New Task", true).clicked() {
                let f = current_folder(app, Kind::Task);
                editors::new_task(app, st, f);
            }
        });
    });
    let resp = ui.add(
        TextEdit::singleline(&mut st.new_task).hint_text("Type a new task").desired_width(f32::INFINITY),
    );
    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !st.new_task.trim().is_empty() {
        let mut p = json!({ "subject": st.new_task.trim() });
        if let Some(o) = p.as_object_mut() {
            if let Some(f) = current_folder(app, Kind::Task) {
                o.insert("folder".into(), json!(f.0));
            }
            if matches!(view.view.as_str(), "To-Do List" | "Today") {
                o.insert("due".into(), json!(day_string(today(app))));
            }
        }
        if let Some(out) = app.run("tasks.new", p) {
            select_item(app, out.get("id").and_then(Value::as_u64));
            st.new_task.clear();
        }
        resp.request_focus();
    }
    ui.add_space(4.0);
    let detailed = view.view == "Detailed" && ui.available_width() > 600.0;
    if detailed {
        column_header(ui, app);
    }
    if list.is_empty() {
        let r = ui.available_rect_before_wrap();
        let r = Rect::from_min_size(r.min, vec2(r.width(), 80.0_f32.min(r.height())));
        empty_message(ui, r, app, "No tasks here", "Type one in the box above.");
        return;
    }
    let now = today(app);
    let collapsed = folder.map(|f| collapsed_labels(app, f)).unwrap_or_default();
    ScrollArea::vertical().id_salt("mc-tasks-scroll").auto_shrink([false, false]).show(ui, |ui| {
        let mut last_group: Option<&str> = None;
        for (i, row) in list.iter().enumerate() {
            if let Some(g) = row.group.as_deref()
                && last_group != Some(g)
            {
                last_group = Some(g);
                let count = list.iter().skip(i).take_while(|r| r.group.as_deref() == Some(g)).count();
                let shut = collapsed.contains(g);
                if group_row(ui, app, g, count, shut) {
                    toggle_group(app, row.id, shut);
                }
            }
            if row.group.as_ref().is_some_and(|g| collapsed.contains(g)) {
                continue;
            }
            row_ui(ui, app, st, row, now, view.preview_lines, detailed);
        }
    });
}

fn column_header(ui: &mut Ui, app: &App) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    // Column headers are announced (and found by tests) like labels.
    for (i, name) in ["Subject", "Due Date"].iter().chain(DETAILED.iter()).enumerate() {
        let r = Rect::from_min_size(pos2(rect.min.x + i as f32, rect.min.y), vec2(1.0, 1.0));
        let resp = ui.interact(r, Id::new(("mc-task-col", i)), Sense::hover());
        a11y(&resp, WidgetType::Label, name);
    }
    let font = FontId::proportional(11.5);
    let p = ui.painter();
    p.text(
        pos2(rect.min.x + 52.0, rect.center().y),
        Align2::LEFT_CENTER,
        "Subject",
        font.clone(),
        app.theme.text_muted,
    );
    let mut x = rect.max.x - 120.0;
    p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, "Due Date", font.clone(), app.theme.text_muted);
    for c in DETAILED.iter().rev() {
        x -= DETAILED_W;
        p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, *c, font.clone(), app.theme.text_muted);
    }
    p.hline(rect.x_range(), rect.max.y - 1.0, Stroke::new(1.0, app.theme.separator));
}

/// Text of the Detailed view's extra columns for a row.
fn detailed_cells(app: &App, row: &Row) -> [String; 4] {
    let Some(t) = app.s.mailbox.tasks.get(&row.id).filter(|_| row.kind == RowKind::Task) else {
        let kind = if row.kind == RowKind::Mail { "Flagged message" } else { "Flagged contact" };
        return [kind.to_owned(), String::new(), String::new(), String::new()];
    };
    let folder = app.s.mailbox.folder(t.folder).map(|f| f.name.clone()).unwrap_or_default();
    [
        editors::status_label(editors::status_key(t.status)).to_owned(),
        format!("{}%", t.percent_complete),
        folder,
        t.categories.join(", "),
    ]
}

fn preview_text(app: &App, row: &Row, lines: u8) -> String {
    if lines == 0 || row.kind != RowKind::Task {
        return String::new();
    }
    let body = app.s.mailbox.tasks.get(&row.id).map(|t| t.body.clone()).unwrap_or_default();
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(usize::from(lines))
        .collect::<Vec<_>>()
        .join("\n")
}

fn row_ui(ui: &mut Ui, app: &mut App, st: &mut ModState, row: &Row, now: i64, preview: u8, detailed: bool) {
    let body = preview_text(app, row, preview);
    let extra = if body.is_empty() { 0.0 } else { 15.0 * body.lines().count() as f32 + 2.0 };
    let base = row_height(app, ROW_H);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), base + extra), Sense::click());
    a11y(&resp, WidgetType::Button, &row.subject);
    let line = Rect::from_min_size(rect.min, vec2(rect.width(), base));
    let sel = app.ui.module_selection == Some(row.id.0);
    if sel {
        ui.painter().rect_filled(rect, CornerRadius::same(3), app.theme.selected);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(3), app.theme.hover);
    }
    // Complete checkbox.
    let cb = Rect::from_center_size(pos2(line.min.x + 14.0, line.center().y), Vec2::splat(14.0));
    let cb_resp = ui.interact(cb.expand(4.0), Id::new(("mc-task-check", row.id.0)), Sense::click());
    a11y(&cb_resp, WidgetType::Checkbox, &format!("Complete {}", row.subject));
    draw_check(ui, cb, row.done, app.theme.complete, app.theme.text_muted);
    // Kind icon.
    let icon = match row.kind {
        RowKind::Mail => "mail",
        RowKind::Contact => "person",
        _ => "task",
    };
    app.icons.paint(
        ui,
        Rect::from_min_size(pos2(line.min.x + 30.0, line.center().y - 8.0), Vec2::splat(16.0)),
        icon,
        app.theme.text_muted,
    );
    let mut x = line.min.x + 52.0;
    let recurring = app.s.mailbox.tasks.get(&row.id).is_some_and(|t| t.recurrence.is_some());
    if row.priority == TaskPriority::High {
        app.icons.paint(
            ui,
            Rect::from_min_size(pos2(x, line.center().y - 7.0), Vec2::splat(14.0)),
            "importance-high",
            app.theme.importance_high,
        );
        x += 16.0;
    }
    if recurring {
        app.icons.paint(
            ui,
            Rect::from_min_size(pos2(x, line.center().y - 7.0), Vec2::splat(14.0)),
            "recurrence",
            app.theme.text_muted,
        );
        x += 16.0;
    }
    // Flag at the right, due date left of it.
    let flag = Rect::from_center_size(pos2(line.max.x - 14.0, line.center().y), Vec2::splat(16.0));
    let flag_resp = ui.interact(flag.expand(3.0), Id::new(("mc-task-flag", row.id.0)), Sense::click());
    a11y(&flag_resp, WidgetType::Button, &format!("Mark complete {}", row.subject));
    if row.done {
        app.icons.paint(ui, flag, "flag-complete", app.theme.complete);
    } else {
        app.icons.paint(ui, flag, "flag", app.theme.flag);
    }
    let due_text = row.due.map(|d| {
        format!(
            "{} {}",
            weekday_short(d),
            short_date(d).rsplit_once('/').map(|(md, _)| md.to_owned()).unwrap_or_default()
        )
    });
    let mut right = flag.min.x - 6.0;
    if detailed {
        right = line.max.x - 120.0 - 6.0;
    }
    let overdue = row.due.is_some_and(|d| d < now) && !row.done;
    let overdue_color = option_color(app, "tasks.overdueColor", app.theme.danger);
    let done_color = option_color(app, "tasks.completedColor", app.theme.text_muted);
    if let Some(t) = &due_text {
        let pos =
            if detailed { pos2(line.max.x - 120.0, line.center().y) } else { pos2(right, line.center().y) };
        let align = if detailed { Align2::LEFT_CENTER } else { Align2::RIGHT_CENTER };
        let r = ui.painter().text(
            pos,
            align,
            t,
            FontId::proportional(12.0),
            if overdue { overdue_color } else { app.theme.text_muted },
        );
        if !detailed {
            right = r.min.x - 8.0;
        }
    }
    if detailed {
        let cells = detailed_cells(app, row);
        let mut cx = line.max.x - 120.0;
        for c in cells.iter().rev() {
            cx -= DETAILED_W;
            paint_text_clipped(
                ui,
                pos2(cx, line.center().y - 7.0),
                c,
                FontId::proportional(11.5),
                app.theme.text_muted,
                DETAILED_W - 6.0,
            );
        }
        right = cx - 6.0;
    }
    // Options › Tasks › Overdue / Completed task color.
    let color = if row.done {
        done_color
    } else if overdue {
        overdue_color
    } else {
        app.theme.text
    };
    let text_rect = paint_text_clipped(
        ui,
        pos2(x, line.center().y - 8.0),
        if row.subject.trim().is_empty() { "(No subject)" } else { &row.subject },
        FontId::proportional(13.5),
        color,
        right - x,
    );
    if row.done {
        ui.painter().hline(text_rect.x_range(), text_rect.center().y, Stroke::new(1.0, done_color));
    }
    if !body.is_empty() {
        let area = Rect::from_min_max(pos2(x, line.max.y - 2.0), rect.max);
        let pr = ui.interact(area, Id::new(("mc-task-preview", row.id.0)), Sense::hover());
        a11y(&pr, WidgetType::Label, &body);
        let galley = ui.painter().layout(
            body,
            FontId::proportional(11.5),
            app.theme.text_muted,
            (rect.max.x - x - 8.0).max(10.0),
        );
        let clip = Rect::from_min_max(pos2(x, line.max.y - 2.0), rect.max);
        ui.painter().with_clip_rect(clip).galley(pos2(x, line.max.y - 2.0), galley, app.theme.text_muted);
    }
    if cb_resp.clicked() || flag_resp.clicked() {
        toggle_complete(app, row);
        return;
    }
    if resp.clicked() {
        select_item(app, Some(row.id.0));
    }
    if resp.double_clicked() {
        open_row(app, st, row);
    }
    let r = row.clone();
    resp.context_menu(|ui| row_menu(ui, app, st, &r));
}

fn row_menu(ui: &mut Ui, app: &mut App, st: &mut ModState, r: &Row) {
    select_item(app, Some(r.id.0));
    if ui.button("Open").clicked() {
        open_row(app, st, r);
        ui.close();
    }
    if ui.button(if r.done { "Mark Not Complete" } else { "Mark Complete" }).clicked() {
        toggle_complete(app, r);
        ui.close();
    }
    if !r.flagged_item() {
        ui.menu_button("Follow Up", |ui| {
            for (kind, label) in [
                ("today", "Today"),
                ("tomorrow", "Tomorrow"),
                ("thisWeek", "This Week"),
                ("nextWeek", "Next Week"),
                ("noDate", "No Date"),
                ("custom", "Custom..."),
            ] {
                if ui.button(label).clicked() {
                    app.run("tasks.flag", json!({ "ids": [r.id.0], "kind": kind }));
                    ui.close();
                }
            }
        });
        ui.menu_button("Categorize", |ui| {
            let cats: Vec<String> = app.s.mailbox.categories.iter().map(|c| c.name.clone()).collect();
            for c in cats {
                if ui.button(&c).clicked() {
                    app.run("tasks.categorize", json!({ "ids": [r.id.0], "name": c }));
                    ui.close();
                }
            }
        });
        if ui.button("Forward").clicked() {
            app.run("tasks.forward", json!({ "ids": [r.id.0] }));
            ui.close();
        }
        if ui.button("Move to Folder...").clicked() {
            app.run("tasks.move", json!({ "ids": [r.id.0] }));
            ui.close();
        }
    }
    if ui.button("Remove from List").clicked() {
        app.run("tasks.removeFromList", json!({ "ids": [r.id.0] }));
        ui.close();
    }
    if !r.flagged_item() && ui.button("Delete").clicked() {
        app.run("tasks.delete", json!({ "ids": [r.id.0] }));
        select_item(app, None);
        ui.close();
    }
}

fn info(ui: &mut Ui, app: &App, label: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        ui.add_sized(vec2(110.0, 18.0), egui::Label::new(RichText::new(label).color(app.theme.text_muted)));
        ui.label(RichText::new(value).color(app.theme.text));
    });
}

fn details(ui: &mut Ui, app: &mut App, st: &mut ModState, row: &Row) {
    ScrollArea::vertical().id_salt("mc-tasks-details-scroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.label(
            RichText::new(if row.subject.trim().is_empty() { "(No subject)" } else { &row.subject })
                .size(22.0)
                .color(app.theme.text),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if crate::widgets::icon_button(ui, app, "settings", "Edit", row.kind != RowKind::Mail).clicked() {
                open_row(app, st, row);
            }
            if row.kind == RowKind::Mail
                && crate::widgets::icon_button(ui, app, "envelope-open", "Open Message", true).clicked()
            {
                app.run("mail.open", json!({ "ids": [row.id.0] }));
            }
            let label = if row.done { "Mark Not Complete" } else { "Mark Complete" };
            if crate::widgets::icon_button(ui, app, "check", label, !(row.flagged_item() && row.done))
                .clicked()
            {
                toggle_complete(app, row);
            }
            if crate::widgets::icon_button(ui, app, "flag-complete", "Remove from List", true).clicked() {
                app.run("tasks.removeFromList", json!({ "ids": [row.id.0] }));
                select_item(app, None);
            }
            if !row.flagged_item() && crate::widgets::icon_button(ui, app, "delete", "Delete", true).clicked()
            {
                app.run("tasks.delete", json!({ "ids": [row.id.0] }));
                select_item(app, None);
            }
        });
        ui.separator();
        if row.flagged_item() {
            info(
                ui,
                app,
                "Type",
                if row.kind == RowKind::Mail { "Flagged message" } else { "Flagged contact" },
            );
            info(ui, app, "Due", &row.due.map(long_date).unwrap_or_else(|| "None".into()));
            return;
        }
        let Some(t) = app.s.mailbox.tasks.get(&row.id).cloned() else { return };
        let tz = tz(app);
        let date = |d: Option<mailcraft_model::DateTime>| {
            d.map(|d| long_date(local_secs(d, tz).div_euclid(DAY))).unwrap_or_else(|| "None".into())
        };
        info(ui, app, "Due", &date(t.due));
        info(ui, app, "Start", &date(t.start));
        info(ui, app, "Status", editors::status_label(editors::status_key(t.status)));
        let prio = match t.priority {
            TaskPriority::Low => "Low",
            TaskPriority::Normal => "Normal",
            TaskPriority::High => "High",
        };
        info(ui, app, "Priority", prio);
        ui.horizontal(|ui| {
            ui.add_sized(
                vec2(110.0, 18.0),
                egui::Label::new(RichText::new("% Complete").color(app.theme.text_muted)),
            );
            ui.add(
                egui::ProgressBar::new(f32::from(t.percent_complete) / 100.0)
                    .desired_width(200.0)
                    .text(format!("{}%", t.percent_complete)),
            );
        });
        if let Some(r) = t.reminder {
            let s = local_secs(r, tz);
            info(
                ui,
                app,
                "Reminder",
                &format!("{} {}", long_date(s.div_euclid(DAY)), super::common::clock(s.rem_euclid(DAY) / 60)),
            );
        }
        if let Some(r) = &t.recurrence {
            let text = if r.regenerate_days > 0 {
                format!("{} day(s) after each task is completed", r.regenerate_days)
            } else {
                mailcraft_engine::cmd::calendar::recurrence::pattern_text(&r.rrule)
                    .unwrap_or_else(|| r.rrule.clone())
            };
            info(ui, app, "Recurrence", &text);
        }
        if let Some(c) = t.completed {
            info(ui, app, "Completed", &date(Some(c)));
        }
        if !t.categories.is_empty() {
            info(ui, app, "Categories", &t.categories.join(", "));
        }
        if t.private {
            info(ui, app, "Sensitivity", "Private");
        }
        if !t.assigned_to.is_empty() {
            info(ui, app, "Assigned to", &t.assigned_to.join("; "));
        }
        if let Some(r) = &t.requested_by {
            info(ui, app, "Assigned by", r);
        }
        let request = match t.request_state {
            TaskRequestState::Waiting => "Waiting for a response",
            TaskRequestState::Accepted if t.requested_by.is_some() => "Accepted by you",
            TaskRequestState::Accepted => "Accepted",
            TaskRequestState::Declined => "Declined",
            TaskRequestState::None => "",
        };
        info(ui, app, "Task request", request);
        if let Some(when) = t.last_report {
            let s = local_secs(when, tz);
            let text =
                format!("{} {}", long_date(s.div_euclid(DAY)), super::common::clock(s.rem_euclid(DAY) / 60));
            info(ui, app, "Last update", &text);
        }
        if t.from_mail.is_some() {
            info(ui, app, "Linked", "Created from a flagged message");
        }
        ui.add_space(12.0);
        if t.body.trim().is_empty() {
            ui.label(RichText::new("No notes.").color(app.theme.text_muted));
        } else {
            ui.label(RichText::new(&t.body).color(app.theme.text));
        }
    });
}

/// Due-date group of the To-Do List (kept for the To-Do Bar and tests; the engine groups the views).
#[cfg(test)]
pub(super) fn group_of(due: Option<i64>, now: i64) -> &'static str {
    mailcraft_engine::cmd::view::modules::date_group(due, now, "Overdue").1
}

#[cfg(test)]
mod tests {
    use super::super::common::parse_day;
    use super::*;

    #[test]
    fn groups() {
        let now = parse_day("2026-10-09").unwrap(); // Friday
        assert_eq!(group_of(None, now), "No Date");
        assert_eq!(group_of(Some(now - 1), now), "Overdue");
        assert_eq!(group_of(Some(now), now), "Today");
        assert_eq!(group_of(Some(now + 1), now), "Tomorrow");
        assert_eq!(group_of(Some(now + 3), now), "Next Week");
        assert_eq!(group_of(Some(now + 30), now), "Later");
    }

    #[test]
    fn todo_and_folder_rows() {
        let mut app = App::new(mailcraft_engine::sample::sample_session());
        let tasks =
            app.s.mailbox.folders.values().find(|f| f.kind == FolderKind::Tasks).map(|f| f.id).unwrap();
        app.run("nav.selectFolder", json!({"folder": tasks.0}));
        let todo = rows(&app);
        // Two open tasks plus any flagged mail; the completed one is not on the To-Do List.
        assert!(todo.iter().filter(|r| !r.flagged_item()).count() == 2);
        assert!(todo.iter().all(|r| !r.done));
        assert!(todo.iter().all(|r| r.group.is_some()), "the To-Do List is grouped by due date");
        change_view(&mut app, "Simple List");
        let all = rows(&app);
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(|r| r.done));
        change_view(&mut app, "Completed");
        assert_eq!(rows(&app).len(), 1);
    }
}
