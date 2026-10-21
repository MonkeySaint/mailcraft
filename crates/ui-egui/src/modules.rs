//! Calendar, People, Tasks (To Do), Notes module views (modules area).
//!
//! Layout and behaviour follow `plan/reference/modules.md` and its screenshots (geometry only): each module
//! draws its own folder pane (calendar: date navigator + My Calendars; people: My Contacts; tasks: My Tasks
//! with the To-Do List; notes: My Notes) next to the module's content. Item changes go through
//! [`App::run`] with the `calendar.*`, `people.*`, `tasks.*` and `notes.*` commands; the views only read the
//! mailbox.
//!
//! UI-only state (the calendar view state the engine's view commands take and return, the selected day,
//! search text, open editors…) lives in egui memory under one id (see `common::ModState`); the calendar's
//! view state is kept per window, so an explorer window (Open in New Window) shows its own calendar view.
//!
//! Item windows (appointment, contact, contact group, task, note, Go to Date) and the Reminders window are
//! local egui windows drawn by [`pim_windows`]. [`meeting_bar`] is the reading pane's meeting area and
//! [`todo_bar`] the To-Do Bar. Commands that ask the UI to open one of them (`Notice::OpenDialog` with an id in
//! [`HANDLED_DIALOGS`], forwarded by the shell into `app.ui.dialogs`) are picked up from there.

mod calendar_view;
mod common;
pub(crate) use common::paint_contact_picture;
mod editors;
mod meeting_bar;
mod notes_view;
mod people_view;
mod recurrence;
mod reminders;
mod scheduling;
mod table;
mod tasks_view;
mod todo_bar;

pub use meeting_bar::meeting_bar;
pub use todo_bar::{
    TODO_BAR_WIDTH, saved_todo_bar, set_todo_bar, take_saved_todo_bar, todo_bar, todo_bar_parts,
    todo_bar_shown,
};

use serde_json::Value;

use crate::App;

/// Dialog ids (from `Notice::OpenDialog`) that this area shows as its own item windows. The dialogs area
/// should leave these in `app.ui.dialogs`; [`pim_windows`] takes them out.
pub const HANDLED_DIALOGS: [&str; 6] = ["appointment", "contact", "contactGroup", "task", "note", "goToDate"];

/// True for a dialog request this area draws itself (item windows, module dialogs, and the calendar's own
/// windows while the Calendar shows). The dialogs area leaves these in `app.ui.dialogs` for [`pim_windows`]
/// (or the calendar view) to take on the next pass, instead of flashing a generic renderer for one frame.
pub fn takes_dialog(app: &App, name: &str, params: &Value) -> bool {
    HANDLED_DIALOGS.contains(&name)
        || editors::dialogs::HANDLED.contains(&name)
        // New meetings and appointments asked for from any module (New Items, Reply with Meeting).
        || calendar_view::forms::is_form(name, params)
        || (app.s.view.module == mailcraft_engine::Module::Calendar
            && calendar_view::windows::is_request(name, params))
}

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

/// Item windows (appointment / contact / contact group / task / note editors, Go to Date) and the Reminders
/// window (which also polls for due reminders once a minute). Safe to call more than once per frame (only
/// the first call in a pass draws). The shell should call it every frame so windows requested from other
/// modules (New Items › Appointment in Mail, Ctrl+G) appear and reminders pop up anywhere.
pub fn pim_windows(ctx: &egui::Context, app: &mut App) {
    let pass = ctx.cumulative_pass_nr();
    let first = common::load(ctx).windows_pass != Some(pass);
    editors::show(ctx, app);
    if first {
        reminders::show(ctx, app);
    }
}

/// Tests: the folder of the open contact window (`None` when no contact window is open).
#[cfg(test)]
pub(crate) fn contact_window_folder(ctx: &egui::Context) -> Option<Option<u64>> {
    common::load(ctx).editors.contact.as_ref().map(|e| e.folder)
}

/// Forgets what the module views kept for a closed explorer window (its calendar view and windows).
pub fn forget_window(ctx: &egui::Context, viewport: egui::ViewportId) {
    common::forget_window(ctx, viewport);
    calendar_view::forget_window(ctx, viewport);
}

/// Opens the Reminders window (View › Window › Reminders Window, the status bar's reminder button).
pub fn open_reminders(ctx: &egui::Context, app: &App) {
    reminders::open(ctx, app);
}

/// Reminders due at the last scan (for the status bar).
pub fn reminders_due(ctx: &egui::Context) -> usize {
    reminders::due_count(ctx)
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

/// Internet calendar refreshes queued by Send/Receive or Update Folder, in any module (see
/// `calendar_view::background`).
pub fn calendar_background(ctx: &egui::Context, app: &mut App) {
    calendar_view::background(ctx, app);
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
