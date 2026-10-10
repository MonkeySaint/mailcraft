//! Calendar, People, Tasks (To Do), Notes module views (modules area).
//!
//! Layout and behaviour follow `plan/outlook/modules.md` and its screenshots (geometry only): each module
//! draws its own folder pane (calendar: date navigator + My Calendars; people: My Contacts; tasks: My Tasks
//! with the To-Do List; notes: My Notes) next to the module's content. Item changes go through
//! [`App::run`] with the `calendar.*`, `people.*`, `tasks.*` and `notes.*` commands; the views only read the
//! mailbox.
//!
//! UI-only state (the calendar view state the engine's view commands take and return, the selected day,
//! search text, open editors…) lives in egui memory under one id (see `common::ModState`).
//!
//! Item windows (appointment, contact, contact group, task, Go to Date) are local egui windows drawn by
//! [`pim_windows`]. Commands that ask the UI to open one of them (`Notice::OpenDialog` with an id in
//! [`HANDLED_DIALOGS`], forwarded by the shell into `app.ui.dialogs`) are picked up from there.

mod calendar_view;
mod common;
mod editors;
mod notes_view;
mod people_view;
mod tasks_view;

use serde_json::Value;

use crate::App;

/// Dialog ids (from `Notice::OpenDialog`) that this area shows as its own item windows. The dialogs area
/// should leave these in `app.ui.dialogs`; [`pim_windows`] takes them out.
pub const HANDLED_DIALOGS: [&str; 6] = ["appointment", "contact", "contactGroup", "task", "note", "goToDate"];

pub fn calendar(ui: &mut egui::Ui, app: &mut App) {
    calendar_view::show(ui, app);
    pim_windows(ui.ctx(), app);
}
pub fn people(ui: &mut egui::Ui, app: &mut App) {
    people_view::show(ui, app);
    pim_windows(ui.ctx(), app);
}
pub fn tasks(ui: &mut egui::Ui, app: &mut App) {
    tasks_view::show(ui, app);
    pim_windows(ui.ctx(), app);
}
pub fn notes(ui: &mut egui::Ui, app: &mut App) {
    notes_view::show(ui, app);
    pim_windows(ui.ctx(), app);
}

/// Item windows (appointment / contact / contact group / task editors, Go to Date). Safe to call more than
/// once per frame (only the first call in a pass draws). The shell should call it every frame so windows
/// requested from other modules (New Items › Appointment in Mail, Ctrl+G) appear too.
pub fn pim_windows(ctx: &egui::Context, app: &mut App) {
    editors::show(ctx, app);
}

/// Runs a calendar view command (`calendar.today`, `calendar.next7Days`, `calendar.goToDate`,
/// `calendar.arrange`, `calendar.timeScale`, `calendar.workingHours`, `calendar.overlay`,
/// `calendar.dailyTaskList`) with the calendar's current view state and keeps the returned state, so the
/// ribbon and keyboard can drive the calendar. Other ids run unchanged. Returns the command result.
pub fn calendar_command(ctx: &egui::Context, app: &mut App, id: &str, params: Value) -> Option<Value> {
    let mut st = common::load(ctx);
    calendar_view::sync_mode(app, &mut st);
    let out = calendar_view::run_view_cmd(app, &mut st, id, params);
    common::store(ctx, st);
    out
}

/// Sets the People view ("people" = list with contact card, "card" = business-card grid).
pub fn set_people_view(ctx: &egui::Context, view: &str) {
    let mut st = common::load(ctx);
    st.people_cards = view.eq_ignore_ascii_case("card") || view.eq_ignore_ascii_case("business card");
    common::store(ctx, st);
}

/// Sets the Notes view from a `notes.iconView` / `notes.lineUpIcons` result (`{"view": {...}}`) or a mode
/// name ("large", "small", "list").
pub fn set_notes_view(ctx: &egui::Context, view: &Value) {
    let mut st = common::load(ctx);
    notes_view::apply_view(&mut st, view);
    common::store(ctx, st);
}

/// Shows the To-Do List (true) or the selected tasks folder (false) in the Tasks module.
pub fn set_tasks_todo(ctx: &egui::Context, todo: bool) {
    let mut st = common::load(ctx);
    st.tasks_folder_view = !todo;
    common::store(ctx, st);
}

#[cfg(test)]
mod tests;
