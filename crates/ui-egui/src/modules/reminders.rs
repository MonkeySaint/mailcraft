//! The Reminders window (View › Window › Reminders Window): modeless, centred (`plan/reference/layout.md`).
//!
//! The engine's reminder scanner (`calendar::reminders::due`) runs once a minute (and whenever this window
//! acts); when a reminder comes due that has not been shown yet the window opens by itself. Dismiss,
//! Dismiss All, Snooze and Open Item run the `calendar.dismissReminder`, `calendar.snoozeReminder` and
//! `calendar.openReminderItem` commands with the reminder state this module keeps in egui memory.

use std::collections::BTreeSet;
use std::time::Duration;

use egui::{Align, ComboBox, Id, Layout, RichText, ScrollArea, Sense, vec2};
use serde_json::{Value, json};

use mailcraft_engine::cmd::calendar::reminders::{
    self, DEFAULT_SNOOZE, Reminder, ReminderKind, ReminderState, SNOOZE_MINUTES,
};
use mailcraft_model::DateTime;

use super::common::{clock, local_secs, long_date};
use crate::App;

/// Seconds between scans.
pub(crate) const POLL_SECS: f64 = 60.0;

/// Reminders window state (egui memory).
#[derive(Clone, Default)]
pub(crate) struct RemindersUi {
    /// Dismissed occurrences and snoozes the engine cannot keep on the items.
    pub state: ReminderState,
    pub open: bool,
    /// Reminders already shown (the window pops up only for new ones).
    pub shown: BTreeSet<String>,
    /// Due reminders from the last scan.
    pub list: Vec<Reminder>,
    pub selected: Option<String>,
    /// Snooze choice (minutes).
    pub snooze: u32,
    /// egui time of the last scan.
    pub last_poll: Option<f64>,
    /// egui time of the next scan (a minute later, or when a reminder comes due sooner).
    pub wake_at: f64,
    /// Opens the window on new reminders (off in tests that do not want pop-ups).
    pub no_popup: bool,
}

fn state_id() -> Id {
    Id::new("mailcraft.modules.reminders")
}

pub(crate) fn load(ctx: &egui::Context) -> RemindersUi {
    ctx.data_mut(|d| d.get_temp::<RemindersUi>(state_id())).unwrap_or_default()
}

pub(crate) fn store(ctx: &egui::Context, st: RemindersUi) {
    ctx.data_mut(|d| d.insert_temp(state_id(), st));
}

/// Scans now; opens the window when something new came due.
fn scan(app: &App, st: &mut RemindersUi, now: f64) {
    st.last_poll = Some(now);
    st.list = reminders::due(&app.s, &st.state);
    let wait = reminders::next_due_after(&app.s, &st.state, POLL_SECS as i64)
        .map_or(POLL_SECS, |t| (t.saturating_sub(app.s.now().unix).max(1) as f64).min(POLL_SECS));
    st.wake_at = now + wait;
    let fresh = st.list.iter().any(|r| !st.shown.contains(&r.key));
    if fresh && !st.no_popup {
        st.open = true;
    }
    st.shown = st.list.iter().map(|r| r.key.clone()).collect();
    if st.selected.as_ref().is_none_or(|k| !st.list.iter().any(|r| &r.key == k)) {
        st.selected = st.list.first().map(|r| r.key.clone());
    }
    if st.snooze == 0 {
        st.snooze = DEFAULT_SNOOZE;
    }
}

/// Opens the Reminders window (scanning first).
pub fn open(ctx: &egui::Context, app: &App) {
    let mut st = load(ctx);
    let now = ctx.input(|i| i.time);
    scan(app, &mut st, now);
    st.open = true;
    store(ctx, st);
}

/// Number of reminders due (from the last scan).
pub fn due_count(ctx: &egui::Context) -> usize {
    load(ctx).list.len()
}

/// Scans once a minute (or when the next reminder comes due) and draws the window when open. Called every
/// frame; the scans themselves are the only mailbox walks.
pub(crate) fn show(ctx: &egui::Context, app: &mut App) {
    let mut st = load(ctx);
    let now = ctx.input(|i| i.time);
    if st.last_poll.is_none_or(|t| now >= st.wake_at || now < t) {
        scan(app, &mut st, now);
    }
    // Wake up for the next scan even when nothing else repaints.
    ctx.request_repaint_after(Duration::from_secs_f64((st.wake_at - now).clamp(0.5, POLL_SECS)));
    if st.open {
        window(ctx, app, &mut st);
    }
    store(ctx, st);
}

fn icon_for(kind: ReminderKind) -> &'static str {
    match kind {
        ReminderKind::Appointment => "calendar-reminder",
        ReminderKind::Task => "task",
        ReminderKind::Mail => "flag",
        ReminderKind::Contact => "person",
    }
}

/// "Tuesday, October 13, 2026 3:00 PM" for the selected reminder's start / due line.
fn start_text(app: &App, r: &Reminder) -> String {
    let tz = app.s.tz_offset_min;
    let Some(due) = r.due else { return String::new() };
    let local = local_secs(DateTime::from_unix(due, tz), tz);
    let day = local.div_euclid(86_400);
    match r.kind {
        ReminderKind::Appointment if r.all_day => format!("Start time: {}", long_date(day)),
        ReminderKind::Appointment => {
            format!("Start time: {} {}", long_date(day), clock(local.rem_euclid(86_400) / 60))
        }
        _ => format!("Due: {}", long_date(day)),
    }
}

fn snooze_label(m: u32) -> String {
    match m {
        m if m % (7 * 24 * 60) == 0 => {
            let w = m / (7 * 24 * 60);
            format!("{w} week{}", if w == 1 { "" } else { "s" })
        }
        m if m % (24 * 60) == 0 => {
            let d = m / (24 * 60);
            format!("{d} day{}", if d == 1 { "" } else { "s" })
        }
        m if m % 60 == 0 => {
            let h = m / 60;
            format!("{h} hour{}", if h == 1 { "" } else { "s" })
        }
        m => format!("{m} minute{}", if m == 1 { "" } else { "s" }),
    }
}

/// Runs a reminder command with the current state and keeps the returned state.
fn act(app: &mut App, st: &mut RemindersUi, id: &str, mut params: Value) {
    if let Some(o) = params.as_object_mut() {
        o.insert("state".into(), serde_json::to_value(&st.state).unwrap_or(Value::Null));
    }
    if let Some(out) = app.run(id, params)
        && let Some(state) = out.get("state")
        && let Ok(new) = serde_json::from_value::<ReminderState>(state.clone())
    {
        st.state = new;
    }
}

fn window(ctx: &egui::Context, app: &mut App, st: &mut RemindersUi) {
    let now = app.s.now().unix;
    let title = format!("{} Reminder{}", st.list.len(), if st.list.len() == 1 { "" } else { "s" });
    let mut open = true;
    let mut action: Option<(&'static str, Value)> = None;
    egui::Window::new(title)
        .id(Id::new("mc-reminders-window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(472.0)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            crate::dialogs::form_visuals(ui, &app.theme);
            let selected = st.selected.clone().and_then(|k| st.list.iter().find(|r| r.key == k).cloned());
            // Heading: the selected reminder.
            match &selected {
                Some(r) => {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                        app.icons.paint(ui, rect, icon_for(r.kind), app.theme.accent);
                        let subject =
                            if r.subject.trim().is_empty() { "(No subject)" } else { r.subject.as_str() };
                        ui.label(RichText::new(subject).strong().size(15.0));
                    });
                    let start = start_text(app, r);
                    if !start.is_empty() {
                        ui.label(RichText::new(start).color(app.theme.text_muted));
                    }
                    if !r.location.trim().is_empty() {
                        ui.label(
                            RichText::new(format!("Location: {}", r.location.trim()))
                                .color(app.theme.text_muted),
                        );
                    }
                }
                None => {
                    ui.label(RichText::new("No reminders are due.").color(app.theme.text_muted));
                }
            }
            ui.add_space(6.0);
            egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(
                ui,
                |ui| {
                    ScrollArea::vertical()
                        .id_salt("mc-reminders-list")
                        .max_height(170.0)
                        .min_scrolled_height(170.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            egui::Grid::new("mc-reminders-grid").num_columns(2).striped(true).show(
                                ui,
                                |ui| {
                                    ui.label(RichText::new("Subject").strong());
                                    ui.label(RichText::new("Due in").strong());
                                    ui.end_row();
                                    for r in &st.list {
                                        let subject = if r.subject.trim().is_empty() {
                                            "(No subject)".to_owned()
                                        } else {
                                            r.subject.clone()
                                        };
                                        let is_sel = st.selected.as_deref() == Some(r.key.as_str());
                                        let resp = ui.selectable_label(is_sel, subject);
                                        if resp.clicked() {
                                            st.selected = Some(r.key.clone());
                                        }
                                        if resp.double_clicked() {
                                            action =
                                                Some(("calendar.openReminderItem", json!({ "key": r.key })));
                                        }
                                        let due = reminders::due_in_text(r, now);
                                        let color = if due.ends_with("overdue") {
                                            app.theme.danger
                                        } else {
                                            app.theme.text
                                        };
                                        ui.label(RichText::new(due).color(color));
                                        ui.end_row();
                                    }
                                },
                            );
                        });
                },
            );
            ui.add_space(6.0);
            let has_sel = selected.is_some();
            ui.horizontal(|ui| {
                if ui.add_enabled(!st.list.is_empty(), egui::Button::new("Dismiss All")).clicked() {
                    action = Some(("calendar.dismissReminder", json!({ "all": true })));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(has_sel, egui::Button::new("Dismiss")).clicked()
                        && let Some(r) = &selected
                    {
                        action = Some(("calendar.dismissReminder", json!({ "keys": [r.key] })));
                    }
                    if ui.add_enabled(has_sel, egui::Button::new("Open Item")).clicked()
                        && let Some(r) = &selected
                    {
                        action = Some(("calendar.openReminderItem", json!({ "key": r.key })));
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Remind me again in:");
                ComboBox::from_id_salt("mc-reminders-snooze")
                    .selected_text(snooze_label(st.snooze))
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for m in SNOOZE_MINUTES {
                            ui.selectable_value(&mut st.snooze, m, snooze_label(m));
                        }
                    });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(has_sel, egui::Button::new("Snooze")).clicked()
                        && let Some(r) = &selected
                    {
                        action = Some((
                            "calendar.snoozeReminder",
                            json!({ "keys": [r.key], "minutes": st.snooze }),
                        ));
                    }
                });
            });
        });
    if let Some((id, params)) = action {
        act(app, st, id, params);
        let t = ctx.input(|i| i.time);
        scan(app, st, t);
        if st.list.is_empty() {
            st.open = false;
        }
        ctx.request_repaint();
    }
    if !open {
        st.open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_engine::sample::sample_session;

    #[test]
    fn labels() {
        assert_eq!(snooze_label(5), "5 minutes");
        assert_eq!(snooze_label(60), "1 hour");
        assert_eq!(snooze_label(1440), "1 day");
        assert_eq!(snooze_label(20_160), "2 weeks");
        assert_eq!(icon_for(ReminderKind::Mail), "flag");
        assert_eq!(icon_for(ReminderKind::Contact), "person");
    }

    #[test]
    fn new_reminders_open_the_window_once() {
        let mut app = App::new(sample_session());
        let ctx = egui::Context::default();
        // Nothing due in the sample at 2:30 PM.
        ctx.run_ui(egui::RawInput::default(), |ui| show(ui.ctx(), &mut app)).textures_delta.clear();
        assert!(!load(&ctx).open);
        // A task reminder an hour ago.
        let at = mailcraft_engine::cmd::calendar::pim::datetime_string(
            mailcraft_engine::cmd::calendar::pim::add_secs(app.s.now(), -3600),
            0,
        );
        app.run("tasks.new", json!({ "subject": "Pay rent", "reminder": at })).unwrap();
        let mut st = load(&ctx);
        st.last_poll = None;
        store(&ctx, st);
        ctx.run_ui(egui::RawInput::default(), |ui| show(ui.ctx(), &mut app)).textures_delta.clear();
        let st = load(&ctx);
        assert!(st.open);
        assert_eq!(st.list.len(), 1);
        assert_eq!(due_count(&ctx), 1);
        // Closing it and scanning again does not pop it up again.
        let mut st = load(&ctx);
        st.open = false;
        st.last_poll = None;
        store(&ctx, st);
        ctx.run_ui(egui::RawInput::default(), |ui| show(ui.ctx(), &mut app)).textures_delta.clear();
        assert!(!load(&ctx).open);
    }
}
