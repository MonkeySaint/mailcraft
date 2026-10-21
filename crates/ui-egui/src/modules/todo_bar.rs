//! The To-Do Bar (View › Layout › To-Do Bar › Calendar / People / Tasks / Off): a pane on the right of the
//! Mail, People, Tasks and Notes modules with a month calendar and the selected day's (or the next days') appointments, a people
//! search, and the To-Do List with "Type a new task".
//!
//! The parts shown are session state (`ViewState::todo_bar`, changed by `view.todoBar` from the ribbon, the
//! control channel, MCP or the CLI alike) and are saved with the profile's view state, so the To-Do Bar comes
//! back as it was left. The shell reserves [`TODO_BAR_WIDTH`] on the right for [`todo_bar`] while
//! [`todo_bar_shown`]. Older profiles kept the parts in the dialog settings (`todoBar.parts`); the shell takes
//! them over once ([`saved_todo_bar`]).

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

/// UI state of the To-Do Bar's parts (egui memory); which parts show is session state.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TodoBarState {
    /// Day picked in the month calendar (`None` = upcoming days from today).
    pub day: Option<i64>,
    /// Month shown when paged away from today's.
    pub month: Option<(i64, u8)>,
    pub search: String,
    pub new_task: String,
}

/// Dialog-settings key where older profiles kept the parts shown (`["calendar", "people", "tasks"]`).
pub(crate) const PREF_KEY: &str = "todoBar.parts";

/// The parts an older profile kept in the dialog settings, once those are loaded (`None` before, or when
/// there are none).
pub fn saved_todo_bar(ctx: &egui::Context) -> Option<mailcraft_engine::TodoBar> {
    let Some(Value::Array(saved)) = crate::dialogs::ui_pref(ctx, PREF_KEY) else { return None };
    let on = |k: &str| saved.iter().any(|v| v.as_str() == Some(k));
    Some(mailcraft_engine::TodoBar { calendar: on("calendar"), people: on("people"), tasks: on("tasks") })
}

/// Takes over the parts an older profile kept in the dialog settings, once: the old key is removed (and the
/// dialog settings saved without it), so a bar turned off later stays off after a restart.
pub fn take_saved_todo_bar(ctx: &egui::Context) -> Option<mailcraft_engine::TodoBar> {
    let parts = saved_todo_bar(ctx);
    crate::dialogs::take_ui_pref(ctx, PREF_KEY);
    parts
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
pub fn set_todo_bar(app: &mut App, part: &str) {
    app.run("view.todoBar", json!({ "part": part }));
}

/// Parts shown: (calendar, people, tasks).
pub fn todo_bar_parts(app: &App) -> (bool, bool, bool) {
    let t = app.s.view.todo_bar;
    (t.calendar, t.people, t.tasks)
}

/// True when any part is on.
pub fn todo_bar_shown(app: &App) -> bool {
    app.s.view.todo_bar.shown()
}

/// Draws the To-Do Bar in `ui` (its whole rect).
pub fn todo_bar(ui: &mut Ui, app: &mut App) {
    let ctx = ui.ctx().clone();
    let mut st = load(&ctx);
    let parts = app.s.view.todo_bar;
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, CornerRadius::same(6), app.theme.pane);
    let inner = rect.shrink(8.0);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).id_salt("mc-todo-bar"));
    ScrollArea::vertical().id_salt("mc-todo-bar-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        if parts.calendar {
            calendar_part(ui, app, &mut st);
            ui.separator();
        }
        if parts.people {
            people_part(ui, app, &mut st);
            ui.separator();
        }
        if parts.tasks {
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
                    // Flagged messages and contacts complete through the To-Do List command, which takes the
                    // item ids whatever is selected.
                    app.run("tasks.markComplete", json!({ "ids": [id] }));
                }
            }
            let label = if due.is_empty() { subject.clone() } else { format!("{subject}  ({due})") };
            let resp = ui.add(egui::Label::new(label).truncate().sense(Sense::click()));
            if resp.double_clicked() {
                if kind == "task" {
                    let mut ms = load_mod(ui.ctx());
                    super::editors::open_task(app, &mut ms, ItemId(id));
                    store_mod(ui.ctx(), ms);
                } else if kind == "contact" {
                    let mut ms = load_mod(ui.ctx());
                    super::editors::open_contact(app, &mut ms, ItemId(id));
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
        let mut app = crate::App::new(mailcraft_engine::sample::sample_session());
        assert!(!todo_bar_shown(&app));
        set_todo_bar(&mut app, "calendar");
        set_todo_bar(&mut app, "tasks");
        assert_eq!(todo_bar_parts(&app), (true, false, true));
        set_todo_bar(&mut app, "calendar");
        assert_eq!(todo_bar_parts(&app), (false, false, true));
        // A part that does not exist is refused and changes nothing.
        set_todo_bar(&mut app, "junk");
        assert!(todo_bar_shown(&app));
        set_todo_bar(&mut app, "off");
        assert!(!todo_bar_shown(&app));
    }

    #[test]
    fn parts_of_an_older_profile_are_taken_over() {
        let ctx = egui::Context::default();
        assert_eq!(saved_todo_bar(&ctx), None);
        ctx.data_mut(|d| {
            let p = d.get_temp_mut_or_default::<crate::chrome::UiPrefs>(Id::new(crate::chrome::UI_PREFS_ID));
            p.values.insert(PREF_KEY.to_owned(), json!(["people", "tasks"]));
        });
        let t = saved_todo_bar(&ctx).unwrap();
        assert_eq!((t.calendar, t.people, t.tasks), (false, true, true));
        // Junk in the saved value shows nothing rather than failing.
        let junk = egui::Context::default();
        junk.data_mut(|d| {
            let p = d.get_temp_mut_or_default::<crate::chrome::UiPrefs>(Id::new(crate::chrome::UI_PREFS_ID));
            p.values.insert(PREF_KEY.to_owned(), json!([1, null, "mail"]));
        });
        assert!(!saved_todo_bar(&junk).unwrap().shown());
    }

    #[test]
    fn older_profile_parts_are_taken_over_once_and_off_stays_off() {
        // Regression: the old key stayed in the saved dialog settings, so a bar turned off came back on at
        // every start.
        let mut s = mailcraft_engine::sample::sample_session();
        s.set_option("state.uiPrefs", &json!({ "todoBar.parts": ["calendar"], "columns.7": ["from"] }))
            .unwrap();
        let mut app = crate::App::new(s);
        let ctx = egui::Context::default();
        crate::chrome::sync_ui_prefs(&ctx, &mut app);
        app.apply_todo_bar(&ctx);
        assert_eq!(todo_bar_parts(&app), (true, false, false));
        crate::chrome::sync_ui_prefs(&ctx, &mut app);
        let saved = app.s.settings.get("state.uiPrefs").unwrap();
        assert!(saved.get(PREF_KEY).is_none(), "{saved}");
        assert_eq!(saved["columns.7"], json!(["from"]), "other dialog settings stay");
        set_todo_bar(&mut app, "off");
        // A restart: a new window (fresh egui memory and UI state) on the same profile.
        let mut app = crate::App::new(app.s);
        let ctx = egui::Context::default();
        for _ in 0..3 {
            crate::chrome::sync_ui_prefs(&ctx, &mut app);
            app.apply_todo_bar(&ctx);
        }
        assert!(!todo_bar_shown(&app));
    }

    #[test]
    fn scripts_change_the_bar_the_window_shows() {
        // The control channel, MCP and the CLI run the same command: the window follows the session.
        let mut app = crate::App::new(mailcraft_engine::sample::sample_session());
        app.s.run("view.todoBar", &json!({"part": "people"})).unwrap();
        assert_eq!(todo_bar_parts(&app), (false, true, false));
        assert_eq!(app.s.run("view.todoBar", &json!({})).unwrap()["parts"]["people"], true);
    }
}
