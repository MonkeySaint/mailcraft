//! Tasks module (classic Tasks / To-Do List): My Tasks (folder pane), task list with complete checkboxes,
//! due dates and flags (grouped by due date in the To-Do List), quick "new task" box and a details pane.

use egui::{
    Align, Align2, CornerRadius, FontId, Id, Key, Modifiers, Rect, RichText, ScrollArea, Sense, Stroke,
    TextEdit, Ui, Vec2, WidgetType, pos2, vec2,
};
use serde_json::{Value, json};

use mailcraft_engine::cmd::calendar::pim::Kind;
use mailcraft_model::{FolderKind, ItemId, TaskPriority, TaskStatus};

use super::common::{
    DAY, ModState, a11y, child, current_folder, day_string, draw_check, empty_message, folder_rows,
    group_header, load, local_secs, long_date, paint_text_clipped, pane_bg, parse_day, select_folder,
    short_date, split_folder_pane, store, today, tz, week_start, weekday_short,
};
use super::editors;
use crate::App;

const ROW_H: f32 = 30.0;

/// One row of the task list (a task or a flagged message).
#[derive(Clone, Debug)]
pub(super) struct Row {
    pub id: ItemId,
    pub mail: bool,
    pub subject: String,
    pub due: Option<i64>,
    pub done: bool,
    pub priority: TaskPriority,
}

fn task_row(app: &App, id: ItemId) -> Option<Row> {
    let t = app.s.mailbox.tasks.get(&id)?;
    let tz = tz(app);
    Some(Row {
        id,
        mail: false,
        subject: t.subject.clone(),
        due: t.due.map(|d| local_secs(d, tz).div_euclid(DAY)),
        done: t.status == TaskStatus::Completed,
        priority: t.priority,
    })
}

/// Rows shown: the To-Do List (open tasks + flagged mail, by due date) or the current tasks folder.
pub(super) fn rows(app: &App, st: &ModState) -> Vec<Row> {
    if !st.tasks_folder_view {
        return mailcraft_engine::cmd::tasks::todo_list(&app.s)
            .iter()
            .filter_map(|v| {
                let id = ItemId(v.get("id").and_then(Value::as_u64)?);
                if v.get("kind").and_then(Value::as_str) == Some("mail") {
                    Some(Row {
                        id,
                        mail: true,
                        subject: v.get("subject").and_then(Value::as_str).unwrap_or("").to_owned(),
                        due: v.get("due").and_then(Value::as_str).and_then(parse_day),
                        done: false,
                        priority: TaskPriority::Normal,
                    })
                } else {
                    task_row(app, id)
                }
            })
            .collect();
    }
    let Some(folder) = current_folder(app, Kind::Task) else { return Vec::new() };
    let mut v: Vec<Row> = app
        .s
        .mailbox
        .tasks
        .values()
        .filter(|t| t.folder == folder)
        .filter_map(|t| task_row(app, t.id))
        .collect();
    v.sort_by_key(|r| (r.done, r.due.unwrap_or(i64::MAX), r.subject.to_lowercase(), r.id));
    v
}

/// Due-date group of the To-Do List.
pub(super) fn group_of(due: Option<i64>, now: i64) -> &'static str {
    let ws = week_start(now);
    match due {
        None => "No Date",
        Some(d) if d < now => "Overdue",
        Some(d) if d == now => "Today",
        Some(d) if d == now + 1 => "Tomorrow",
        Some(d) if d <= ws + 6 => "This Week",
        Some(d) if d <= ws + 13 => "Next Week",
        Some(_) => "Later",
    }
}

pub(super) fn show(ui: &mut Ui, app: &mut App) {
    let ctx = ui.ctx().clone();
    let mut st = load(&ctx);
    let full = ui.max_rect();
    let (pane, main) = split_folder_pane(ui, app);
    if let Some(p) = pane {
        pane_bg(ui, p, app.theme.pane);
        child(ui, p.shrink(6.0), "mc-tasks-pane", |ui| folder_pane(ui, app, &mut st));
    }
    pane_bg(ui, main, app.theme.reading);
    let list = rows(app, &st);
    child(ui, main, "mc-tasks-main", |ui| content(ui, app, &mut st, &list));
    keyboard(&ctx, app, &mut st, &list);
    ui.expand_to_include_rect(full);
    store(&ctx, st);
}

fn folder_pane(ui: &mut Ui, app: &mut App, st: &mut ModState) {
    group_header(ui, app, "My Tasks", false, "mc-tasks-group");
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), crate::theme::size::FOLDER_ROW), Sense::click());
    a11y(&resp, WidgetType::Button, "To-Do List");
    if !st.tasks_folder_view {
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
    if resp.clicked() {
        st.tasks_folder_view = false;
        app.ui.module_selection = None;
    }
    let current = if st.tasks_folder_view { current_folder(app, Kind::Task) } else { None };
    if let (Some(f), _) = folder_rows(ui, app, FolderKind::Tasks, current, None) {
        select_folder(app, f);
        st.tasks_folder_view = true;
        app.ui.module_selection = None;
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
        if row.mail {
            app.run("tasks.removeFromList", json!({ "id": row.id.0 }));
        } else {
            app.run("tasks.delete", json!({ "id": row.id.0 }));
        }
        app.ui.module_selection = None;
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        open_row(app, st, &row);
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Insert)) {
        toggle_complete(app, &row);
    }
}

fn open_row(app: &mut App, st: &mut ModState, row: &Row) {
    if row.mail {
        app.run("mail.open", json!({ "ids": [row.id.0] }));
    } else {
        editors::open_task(app, st, row.id);
    }
}

fn toggle_complete(app: &mut App, row: &Row) {
    if row.mail {
        app.run("tasks.markComplete", json!({ "id": row.id.0 }));
    } else {
        app.run("tasks.update", json!({ "id": row.id.0, "complete": !row.done }));
    }
}

fn content(ui: &mut Ui, app: &mut App, st: &mut ModState, list: &[Row]) {
    let rect = ui.max_rect();
    let list_w =
        if rect.width() > 700.0 { 380.0_f32.max(rect.width() * 0.38).min(520.0) } else { rect.width() };
    let left = Rect::from_min_size(rect.min, vec2(list_w, rect.height()));
    child(ui, left.shrink2(vec2(8.0, 8.0)), "mc-tasks-list", |ui| task_list(ui, app, st, list));
    if rect.width() > 700.0 {
        let right = Rect::from_min_max(pos2(left.max.x + 1.0, rect.min.y), rect.max);
        ui.painter().vline(left.max.x, rect.y_range(), Stroke::new(1.0, app.theme.separator));
        match selected(app, list).cloned() {
            Some(row) => child(ui, right.shrink2(vec2(24.0, 16.0)), "mc-tasks-details", |ui| {
                details(ui, app, st, &row)
            }),
            None => empty_message(ui, right, app, "No task selected", "Pick a task to see its details."),
        }
    }
}

fn task_list(ui: &mut Ui, app: &mut App, st: &mut ModState, list: &[Row]) {
    let arrange =
        if st.tasks_folder_view { "Arrange by: Due Date" } else { "To-Do List · Arrange by: Due Date" };
    ui.horizontal(|ui| {
        ui.label(RichText::new(arrange).size(12.0).color(app.theme.text_muted));
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
            if st.tasks_folder_view {
                if let Some(f) = current_folder(app, Kind::Task) {
                    o.insert("folder".into(), json!(f.0));
                }
            } else {
                o.insert("due".into(), json!(day_string(today(app))));
            }
        }
        if let Some(out) = app.run("tasks.new", p) {
            app.ui.module_selection = out.get("id").and_then(Value::as_u64);
            st.new_task.clear();
        }
        resp.request_focus();
    }
    ui.add_space(4.0);
    if list.is_empty() {
        let r = ui.available_rect_before_wrap();
        let r = Rect::from_min_size(r.min, vec2(r.width(), 80.0_f32.min(r.height())));
        empty_message(ui, r, app, "No tasks here", "Type one in the box above.");
        return;
    }
    let now = today(app);
    ScrollArea::vertical().id_salt("mc-tasks-scroll").auto_shrink([false, false]).show(ui, |ui| {
        let mut last_group = "";
        for row in list {
            if !st.tasks_folder_view {
                let g = group_of(row.due, now);
                if g != last_group {
                    last_group = g;
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
                    ui.painter().text(
                        pos2(r.min.x + 4.0, r.center().y),
                        Align2::LEFT_CENTER,
                        g,
                        FontId::proportional(12.5),
                        if g == "Overdue" { app.theme.danger } else { app.theme.accent },
                    );
                    ui.painter().hline(r.x_range(), r.max.y - 1.0, Stroke::new(1.0, app.theme.separator));
                }
            }
            row_ui(ui, app, st, row, now);
        }
    });
}

fn row_ui(ui: &mut Ui, app: &mut App, st: &mut ModState, row: &Row, now: i64) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    a11y(&resp, WidgetType::Button, &row.subject);
    let sel = app.ui.module_selection == Some(row.id.0);
    if sel {
        ui.painter().rect_filled(rect, CornerRadius::same(3), app.theme.selected);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(3), app.theme.hover);
    }
    // Complete checkbox.
    let cb = Rect::from_center_size(pos2(rect.min.x + 14.0, rect.center().y), Vec2::splat(14.0));
    let cb_resp = ui.interact(cb.expand(4.0), Id::new(("mc-task-check", row.id.0)), Sense::click());
    a11y(&cb_resp, WidgetType::Checkbox, &format!("Complete {}", row.subject));
    draw_check(ui, cb, row.done, app.theme.complete, app.theme.text_muted);
    // Kind icon.
    let icon = if row.mail { "mail" } else { "task" };
    app.icons.paint(
        ui,
        Rect::from_min_size(pos2(rect.min.x + 30.0, rect.center().y - 8.0), Vec2::splat(16.0)),
        icon,
        app.theme.text_muted,
    );
    let mut x = rect.min.x + 52.0;
    if row.priority == TaskPriority::High {
        app.icons.paint(
            ui,
            Rect::from_min_size(pos2(x, rect.center().y - 7.0), Vec2::splat(14.0)),
            "importance-high",
            app.theme.importance_high,
        );
        x += 16.0;
    }
    // Flag at the right, due date left of it.
    let flag = Rect::from_center_size(pos2(rect.max.x - 14.0, rect.center().y), Vec2::splat(16.0));
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
    if let Some(t) = &due_text {
        let overdue = row.due.is_some_and(|d| d < now) && !row.done;
        let r = ui.painter().text(
            pos2(right, rect.center().y),
            Align2::RIGHT_CENTER,
            t,
            FontId::proportional(12.0),
            if overdue { app.theme.danger } else { app.theme.text_muted },
        );
        right = r.min.x - 8.0;
    }
    let color = if row.done { app.theme.text_muted } else { app.theme.text };
    let text_rect = paint_text_clipped(
        ui,
        pos2(x, rect.center().y - 8.0),
        if row.subject.trim().is_empty() { "(No subject)" } else { &row.subject },
        FontId::proportional(13.5),
        color,
        right - x,
    );
    if row.done {
        ui.painter().hline(text_rect.x_range(), text_rect.center().y, Stroke::new(1.0, app.theme.text_muted));
    }
    if cb_resp.clicked() || flag_resp.clicked() {
        toggle_complete(app, row);
        return;
    }
    if resp.clicked() {
        app.ui.module_selection = Some(row.id.0);
    }
    if resp.double_clicked() {
        open_row(app, st, row);
    }
    let r = row.clone();
    resp.context_menu(|ui| {
        if ui.button("Open").clicked() {
            open_row(app, st, &r);
            ui.close();
        }
        if ui.button(if r.done { "Mark Not Complete" } else { "Mark Complete" }).clicked() {
            toggle_complete(app, &r);
            ui.close();
        }
        if ui.button("Remove from List").clicked() {
            app.run("tasks.removeFromList", json!({ "id": r.id.0 }));
            ui.close();
        }
        if !r.mail && ui.button("Delete").clicked() {
            app.run("tasks.delete", json!({ "id": r.id.0 }));
            ui.close();
        }
    });
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
            if crate::widgets::icon_button(ui, app, "settings", "Edit", !row.mail).clicked() {
                editors::open_task(app, st, row.id);
            }
            if row.mail
                && crate::widgets::icon_button(ui, app, "envelope-open", "Open Message", true).clicked()
            {
                app.run("mail.open", json!({ "ids": [row.id.0] }));
            }
            let label = if row.done { "Mark Not Complete" } else { "Mark Complete" };
            if crate::widgets::icon_button(ui, app, "check", label, !(row.mail && row.done)).clicked() {
                toggle_complete(app, row);
            }
            if crate::widgets::icon_button(ui, app, "flag-complete", "Remove from List", true).clicked() {
                app.run("tasks.removeFromList", json!({ "id": row.id.0 }));
                app.ui.module_selection = None;
            }
            if !row.mail && crate::widgets::icon_button(ui, app, "delete", "Delete", true).clicked() {
                app.run("tasks.delete", json!({ "id": row.id.0 }));
                app.ui.module_selection = None;
            }
        });
        ui.separator();
        if row.mail {
            info(ui, app, "Type", "Flagged message");
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
        if let Some(c) = t.completed {
            info(ui, app, "Completed", &date(Some(c)));
        }
        if !t.categories.is_empty() {
            info(ui, app, "Categories", &t.categories.join(", "));
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

#[cfg(test)]
mod tests {
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
        let app = App::new(mailcraft_engine::sample::sample_session());
        let mut st = ModState::default();
        let todo = rows(&app, &st);
        // Two open tasks plus any flagged mail; the completed one is not on the To-Do List.
        assert!(todo.iter().filter(|r| !r.mail).count() == 2);
        assert!(todo.iter().all(|r| !r.done));
        st.tasks_folder_view = true;
        let all = rows(&app, &st);
        assert_eq!(all.len(), 3);
        assert!(all.last().is_some_and(|r| r.done));
    }
}
