//! The To-Do Bar (View › Layout › To-Do Bar › Calendar / People / Tasks / Off): a pane on the right of the
//! Mail module with a month calendar and the selected day's (or the next days') appointments, a people
//! search, and the To-Do List with "Type a new task".
//!
//! The session keeps no To-Do Bar state (`view.todoBar` only validates its part), so the parts shown live in
//! egui memory: the shell calls [`set_todo_bar`] with the part `view.todoBar` returned and reserves
//! [`TODO_BAR_WIDTH`] on the right for [`todo_bar`] while [`todo_bar_shown`].

use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Rect, RichText, ScrollArea, Sense, TextEdit, Ui, pos2, vec2,
};
use serde_json::{Value, json};

use mailcraft_engine::cmd::people::{display_name, find_contacts, is_group};
use mailcraft_model::{FolderKind, ItemId};

use super::common::{
    DAY, a11y, add_months, day_of, day_string, load as load_mod, month_name, store as store_mod, today, tz,
    week_start, ymd,
};
use crate::App;

/// Width the shell reserves for the To-Do Bar.
pub const TODO_BAR_WIDTH: f32 = 280.0;
/// Days of upcoming appointments listed when no day is picked.
const UPCOMING_DAYS: i64 = 7;
/// Most rows per section.
const MAX_ROWS: usize = 40;

/// Parts shown and their UI state (egui memory).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TodoBarState {
    pub calendar: bool,
    pub people: bool,
    pub tasks: bool,
    /// Day picked in the month calendar (`None` = upcoming days from today).
    pub day: Option<i64>,
    /// Month shown when paged away from today's.
    pub month: Option<(i64, u8)>,
    pub search: String,
    pub new_task: String,
}

fn state_id() -> Id {
    Id::new("mailcraft.modules.todo-bar")
}

pub(crate) fn load(ctx: &egui::Context) -> TodoBarState {
    ctx.data_mut(|d| d.get_temp::<TodoBarState>(state_id())).unwrap_or_default()
}

pub(crate) fn store(ctx: &egui::Context, st: TodoBarState) {
    ctx.data_mut(|d| d.insert_temp(state_id(), st));
}

/// Applies View › To-Do Bar › `part` ("calendar", "people", "tasks" toggle that part; "off" hides the bar).
pub fn set_todo_bar(ctx: &egui::Context, part: &str) {
    let mut st = load(ctx);
    match part {
        "calendar" => st.calendar = !st.calendar,
        "people" => st.people = !st.people,
        "tasks" => st.tasks = !st.tasks,
        "off" => {
            st.calendar = false;
            st.people = false;
            st.tasks = false;
        }
        _ => {}
    }
    store(ctx, st);
}

/// Parts shown: (calendar, people, tasks).
pub fn todo_bar_parts(ctx: &egui::Context) -> (bool, bool, bool) {
    let st = load(ctx);
    (st.calendar, st.people, st.tasks)
}

/// True when any part is on.
pub fn todo_bar_shown(ctx: &egui::Context) -> bool {
    let st = load(ctx);
    st.calendar || st.people || st.tasks
}

/// Draws the To-Do Bar in `ui` (its whole rect).
pub fn todo_bar(ui: &mut Ui, app: &mut App) {
    let ctx = ui.ctx().clone();
    let mut st = load(&ctx);
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, CornerRadius::same(6), app.theme.pane);
    let inner = rect.shrink(8.0);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).id_salt("mc-todo-bar"));
    ScrollArea::vertical().id_salt("mc-todo-bar-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        if st.calendar {
            calendar_part(ui, app, &mut st);
            ui.separator();
        }
        if st.people {
            people_part(ui, app, &mut st);
            ui.separator();
        }
        if st.tasks {
            tasks_part(ui, app, &mut st);
        }
    });
    store(&ctx, st);
}

fn heading(ui: &mut Ui, app: &App, text: &str) {
    ui.label(RichText::new(text).strong().size(14.0).color(app.theme.text));
}

fn calendar_part(ui: &mut Ui, app: &mut App, st: &mut TodoBarState) {
    let now = today(app);
    let (y, m) = st.month.unwrap_or_else(|| {
        let (y, m, _) = ymd(st.day.unwrap_or(now));
        (y, m)
    });
    ui.horizontal(|ui| {
        if ui.small_button("◀").on_hover_text("Previous month").clicked() {
            st.month = Some(add_months(y, m, -1));
        }
        ui.label(RichText::new(format!("{} {y}", month_name(m))).strong().color(app.theme.text));
        if ui.small_button("▶").on_hover_text("Next month").clicked() {
            st.month = Some(add_months(y, m, 1));
        }
    });
    let w = ui.available_width().min(260.0);
    let cell = (w / 7.0).floor();
    let first = day_of(y, m, 1).unwrap_or(now);
    let start = week_start(first);
    let (grid, _) = ui.allocate_exact_size(vec2(w, 20.0 * 7.0), Sense::hover());
    for (c, name) in ["S", "M", "T", "W", "T", "F", "S"].iter().enumerate() {
        ui.painter().text(
            pos2(grid.min.x + cell * c as f32 + cell / 2.0, grid.min.y + 10.0),
            Align2::CENTER_CENTER,
            *name,
            FontId::proportional(11.0),
            app.theme.text_muted,
        );
    }
    for i in 0..42 {
        let d = start + i;
        let (r, c) = (i / 7, i % 7);
        let cr = Rect::from_min_size(
            pos2(grid.min.x + cell * c as f32, grid.min.y + 20.0 * (r as f32 + 1.0)),
            vec2(cell, 20.0),
        );
        let resp = ui.interact(cr, Id::new(("mc-todo-day", d)), Sense::click());
        a11y(&resp, egui::WidgetType::Button, &super::common::long_date(d));
        let in_month = ymd(d).1 == m;
        let mut color = if in_month { app.theme.text } else { app.theme.text_muted };
        if d == now {
            ui.painter().rect_filled(cr.shrink(1.0), CornerRadius::same(3), app.theme.accent);
            color = Color32::WHITE;
        } else if st.day == Some(d) {
            ui.painter().rect_filled(cr.shrink(1.0), CornerRadius::same(3), app.theme.selected);
        } else if resp.hovered() {
            ui.painter().rect_filled(cr.shrink(1.0), CornerRadius::same(3), app.theme.hover);
        }
        ui.painter().text(
            cr.center(),
            Align2::CENTER_CENTER,
            ymd(d).2.to_string(),
            FontId::proportional(11.0),
            color,
        );
        if resp.clicked() {
            st.day = if st.day == Some(d) { None } else { Some(d) };
        }
    }
    // Appointments: the picked day, else the next days.
    let (from, to) = match st.day {
        Some(d) => (d, d + 1),
        None => (now, now + UPCOMING_DAYS),
    };
    let calendars =
        app.s.mailbox.folders.values().filter(|f| f.kind == FolderKind::Calendar).map(|f| f.id).collect();
    let occs = super::calendar_view::gather(app, &calendars, from, to);
    let now_local = super::common::local_secs(app.s.now(), tz(app));
    let list: Vec<_> =
        occs.into_iter().filter(|o| st.day.is_some() || o.end > now_local).take(MAX_ROWS).collect();
    if list.is_empty() {
        ui.label(
            RichText::new(if st.day.is_some() { "No appointments." } else { "No upcoming appointments." })
                .color(app.theme.text_muted),
        );
    }
    let mut last_day = None;
    for o in list {
        let day = o.start.div_euclid(DAY);
        if last_day != Some(day) {
            last_day = Some(day);
            let label = if day == now {
                "Today".to_owned()
            } else if day == now + 1 {
                "Tomorrow".to_owned()
            } else {
                super::common::long_date(day)
            };
            ui.label(RichText::new(label).size(11.5).color(app.theme.text_muted));
        }
        let when =
            if o.all_day { "All day".to_owned() } else { super::common::clock(o.start.rem_euclid(DAY) / 60) };
        let subject = if o.subject.trim().is_empty() { "(No subject)".to_owned() } else { o.subject.clone() };
        let text = if o.location.trim().is_empty() {
            format!("{when}  {subject}")
        } else {
            format!("{when}  {subject} ({})", o.location.trim())
        };
        let resp = ui.add(egui::Label::new(text).truncate().sense(Sense::click()));
        if resp.double_clicked() || resp.clicked() {
            let mut ms = load_mod(ui.ctx());
            let unix = o.start.saturating_sub(i64::from(tz(app)).saturating_mul(60));
            super::editors::open_occurrence(app, &mut ms, o.id, unix);
            store_mod(ui.ctx(), ms);
        }
    }
}

fn people_part(ui: &mut Ui, app: &mut App, st: &mut TodoBarState) {
    heading(ui, app, "People");
    let r =
        ui.add(TextEdit::singleline(&mut st.search).hint_text("Search people").desired_width(f32::INFINITY));
    a11y(&r, egui::WidgetType::TextEdit, "Search people");
    let ids: Vec<ItemId> = if st.search.trim().is_empty() {
        let mut all: Vec<_> = app.s.mailbox.contacts.values().filter(|c| !is_group(c)).collect();
        all.sort_by_key(|c| display_name(c).to_lowercase());
        all.into_iter().map(|c| c.id).collect()
    } else {
        find_contacts(&app.s, st.search.trim())
    };
    for id in ids.into_iter().take(MAX_ROWS) {
        let Some(c) = app.s.mailbox.contacts.get(&id) else { continue };
        let name = display_name(c);
        let detail = c.emails.first().cloned().unwrap_or_default();
        let resp = ui.add(egui::Label::new(format!("{name}  {detail}")).truncate().sense(Sense::click()));
        if resp.clicked() {
            let mut ms = load_mod(ui.ctx());
            super::editors::open_contact(app, &mut ms, id);
            store_mod(ui.ctx(), ms);
        }
    }
}

fn tasks_part(ui: &mut Ui, app: &mut App, st: &mut TodoBarState) {
    heading(ui, app, "Tasks");
    let resp = ui.add(
        TextEdit::singleline(&mut st.new_task).hint_text("Type a new task").desired_width(f32::INFINITY),
    );
    a11y(&resp, egui::WidgetType::TextEdit, "Type a new task");
    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !st.new_task.trim().is_empty() {
        let subject = std::mem::take(&mut st.new_task);
        let mut p = json!({ "subject": subject.trim(), "due": day_string(today(app)) });
        if let (Some(o), Some(f)) = (
            p.as_object_mut(),
            mailcraft_engine::cmd::calendar::pim::default_folder(
                &app.s,
                mailcraft_engine::cmd::calendar::pim::Kind::Task,
            ),
        ) {
            o.insert("folder".into(), json!(f.0));
        }
        app.run("tasks.new", p);
    }
    let items = mailcraft_engine::cmd::tasks::todo_list(&app.s);
    if items.is_empty() {
        ui.label(RichText::new("Nothing to do.").color(app.theme.text_muted));
    }
    for v in items.into_iter().take(MAX_ROWS) {
        let id = v.get("id").and_then(Value::as_u64).unwrap_or(0);
        let kind = v.get("kind").and_then(Value::as_str).unwrap_or("task").to_owned();
        let subject = v.get("subject").and_then(Value::as_str).unwrap_or("").to_owned();
        let due = v.get("due").and_then(Value::as_str).unwrap_or("").to_owned();
        ui.horizontal(|ui| {
            let mut done = false;
            if ui.checkbox(&mut done, "").on_hover_text("Mark Complete").clicked() && done {
                if kind == "task" {
                    app.run("tasks.update", json!({ "id": id, "complete": true }));
                } else {
                    app.run("mail.markComplete", json!({ "ids": [id] }));
                }
            }
            let label = if due.is_empty() { subject.clone() } else { format!("{subject}  ({due})") };
            let resp = ui.add(egui::Label::new(label).truncate().sense(Sense::click()));
            if resp.double_clicked() {
                if kind == "task" {
                    let mut ms = load_mod(ui.ctx());
                    super::editors::open_task(app, &mut ms, ItemId(id));
                    store_mod(ui.ctx(), ms);
                } else {
                    app.run("mail.open", json!({ "id": id }));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_toggle_and_off() {
        let ctx = egui::Context::default();
        assert!(!todo_bar_shown(&ctx));
        set_todo_bar(&ctx, "calendar");
        set_todo_bar(&ctx, "tasks");
        assert_eq!(todo_bar_parts(&ctx), (true, false, true));
        set_todo_bar(&ctx, "calendar");
        assert_eq!(todo_bar_parts(&ctx), (false, false, true));
        set_todo_bar(&ctx, "junk");
        assert!(todo_bar_shown(&ctx));
        set_todo_bar(&ctx, "off");
        assert!(!todo_bar_shown(&ctx));
    }
}
