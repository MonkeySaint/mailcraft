//! Meeting messages in the reading pane: the info area of a message that carries a `text/calendar` part.
//!
//! Requests show Accept / Tentative / Decline (each: edit the response first, send it now, or send none)
//! and Propose New Time; responses update the organizer's tracking (recorded automatically the first time
//! the response is shown); cancellations offer Remove from Calendar; new-time proposals offer Accept
//! Proposal. Everything runs through the `calendar.*` meeting commands.

use egui::{Align, ComboBox, Id, Layout, RichText, Sense, TextEdit, Ui, vec2};
use serde_json::json;

use mailcraft_engine::cmd::calendar::{self, ItipMessage, MeetingMessage};
use mailcraft_engine::cmd::tasks::{self, TaskMessage, TaskRequestInfo};
use mailcraft_model::ItemId;

use super::common::{DAY, clock, day_string, day_time_string, local_secs, parse_day};
use crate::App;

/// The parsed calendar part of the message shown last (parsing the source every frame would be wasteful).
#[derive(Clone, Default)]
struct Cache {
    mail: u64,
    /// Size + received time of the item when parsed (a re-synced item re-parses).
    stamp: (u64, i64),
    msg: Option<ItipMessage>,
    /// A response whose tracking was already recorded.
    processed: bool,
}

/// The Propose New Time window.
#[derive(Clone, Default)]
struct Propose {
    mail: u64,
    response: String,
    date: String,
    start_min: i64,
    end_date: String,
    end_min: i64,
    error: Option<String>,
}

fn cache_id() -> Id {
    Id::new("mailcraft.modules.meeting-cache")
}

fn task_cache_id() -> Id {
    Id::new("mailcraft.modules.task-message-cache")
}

/// The parsed task part of the message shown last.
#[derive(Clone, Default)]
struct TaskCache {
    mail: u64,
    stamp: (u64, i64),
    msg: Option<TaskMessage>,
}

fn propose_id() -> Id {
    Id::new("mailcraft.modules.meeting-propose")
}

/// The meeting summary for `mail`, or `None` when it carries no calendar part.
pub(crate) fn summary(ctx: &egui::Context, app: &App, mail: u64) -> Option<MeetingMessage> {
    let m = app.s.mailbox.mail.get(ItemId(mail))?;
    let stamp = (m.size, m.received.unix);
    let mut cache: Cache = ctx.data_mut(|d| d.get_temp(cache_id())).unwrap_or_default();
    if cache.mail != mail || cache.stamp != stamp {
        cache = Cache { mail, stamp, msg: calendar::read_itip(&app.s, ItemId(mail)), processed: false };
        ctx.data_mut(|d| d.insert_temp(cache_id(), cache.clone()));
    }
    let msg = cache.msg.as_ref()?;
    Some(calendar::meeting_message_from(&app.s, ItemId(mail), msg))
}

/// The task summary for `mail` (its info bar against my task list), or `None` when it carries no task.
pub(crate) fn task_summary(ctx: &egui::Context, app: &App, mail: u64) -> Option<TaskRequestInfo> {
    let m = app.s.mailbox.mail.get(ItemId(mail))?;
    let stamp = (m.size, m.received.unix);
    let mut cache: TaskCache = ctx.data_mut(|d| d.get_temp(task_cache_id())).unwrap_or_default();
    if cache.mail != mail || cache.stamp != stamp {
        cache = TaskCache { mail, stamp, msg: tasks::read_task_message(&app.s, ItemId(mail)) };
        ctx.data_mut(|d| d.insert_temp(task_cache_id(), cache.clone()));
    }
    let msg = cache.msg.as_ref()?;
    Some(tasks::task_request_info_from(&app.s, ItemId(mail), msg))
}

/// Draws the meeting area for `mail`, or the task bar when the message carries a task (nothing for other
/// messages). Returns true when it drew something.
pub fn meeting_bar(ui: &mut Ui, app: &mut App, mail: u64) -> bool {
    let ctx = ui.ctx().clone();
    let Some(m) = summary(&ctx, app, mail) else { return task_bar(ui, app, mail) };
    if m.method == "REPLY" {
        record_reply(&ctx, app, &m);
    }
    let theme = app.theme.clone();
    egui::Frame::new()
        .fill(theme.info_bar)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                app.icons.paint(ui, rect, "meeting", theme.accent);
                ui.add(egui::Label::new(RichText::new(&m.info).color(theme.text)).wrap());
            });
            ui.add_space(4.0);
            egui::Grid::new(("mc-meeting-grid", mail)).num_columns(2).spacing(vec2(8.0, 2.0)).show(
                ui,
                |ui| {
                    ui.label(RichText::new("When").color(theme.text_muted));
                    ui.label(m.recurrence.clone().unwrap_or_else(|| m.when.clone()));
                    ui.end_row();
                    if !m.location.trim().is_empty() {
                        ui.label(RichText::new("Where").color(theme.text_muted));
                        ui.label(&m.location);
                        ui.end_row();
                    }
                    if !m.organizer.is_empty() {
                        ui.label(RichText::new("Organizer").color(theme.text_muted));
                        ui.label(&m.organizer);
                        ui.end_row();
                    }
                },
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| buttons(ui, app, &m));
        });
    propose_window(&ctx, app);
    true
}

/// Records a response on the organizer's meeting once (when it changes anything).
fn record_reply(ctx: &egui::Context, app: &mut App, m: &MeetingMessage) {
    let mut cache: Cache = ctx.data_mut(|d| d.get_temp(cache_id())).unwrap_or_default();
    if cache.processed || cache.mail != m.mail {
        return;
    }
    cache.processed = true;
    ctx.data_mut(|d| d.insert_temp(cache_id(), cache));
    let Some(appt) = m.appointment.and_then(|id| app.s.mailbox.appointments.get(&ItemId(id))) else { return };
    let stale = m.replies.iter().any(|(who, resp)| {
        !appt.attendees.iter().any(|a| {
            (a.mailbox.display() == who || a.mailbox.addr.eq_ignore_ascii_case(who))
                && calendar_response(a.response) == resp
        })
    });
    if stale {
        app.run("calendar.processReply", json!({ "id": m.mail }));
    }
}

fn calendar_response(r: mailcraft_model::calendar::ResponseStatus) -> &'static str {
    use mailcraft_model::calendar::ResponseStatus;
    match r {
        ResponseStatus::None => "none",
        ResponseStatus::Accepted => "accepted",
        ResponseStatus::Tentative => "tentative",
        ResponseStatus::Declined => "declined",
    }
}

fn respond_menu(ui: &mut Ui, app: &mut App, m: &MeetingMessage, label: &str, response: &str) {
    ui.menu_button(label, |ui| {
        for (text, send) in [
            ("Edit response before sending", "edit"),
            ("Send response now", "now"),
            ("Don't send a response", "none"),
        ] {
            if ui.button(text).clicked() {
                app.run("calendar.respond", json!({ "id": m.mail, "response": response, "send": send }));
                ui.close();
            }
        }
    });
}

fn buttons(ui: &mut Ui, app: &mut App, m: &MeetingMessage) {
    match m.method.as_str() {
        "REQUEST" | "ADD" if m.can_respond => {
            respond_menu(ui, app, m, "Accept", "accept");
            respond_menu(ui, app, m, "Tentative", "tentative");
            respond_menu(ui, app, m, "Decline", "decline");
            ui.menu_button("Propose New Time", |ui| {
                for (text, response) in [
                    ("Tentative and propose a new time", "tentative"),
                    ("Decline and propose a new time", "decline"),
                ] {
                    if ui.button(text).clicked() {
                        open_propose(ui.ctx(), app, m, response);
                        ui.close();
                    }
                }
            });
        }
        "CANCEL" if m.appointment.is_some() => {
            if ui.button("Remove from Calendar").clicked() {
                app.run("calendar.removeFromCalendar", json!({ "id": m.mail }));
            }
        }
        "COUNTER" if m.appointment.is_some() => {
            if ui.button("Accept Proposal").clicked() {
                app.run("calendar.acceptProposal", json!({ "id": m.mail }));
            }
        }
        "REPLY" => {
            if let Some(a) = m.appointment.and_then(|id| app.s.mailbox.appointments.get(&ItemId(id))) {
                use mailcraft_model::calendar::ResponseStatus;
                let n = |r: ResponseStatus| a.attendees.iter().filter(|x| x.response == r).count();
                let text = format!(
                    "{} accepted, {} tentatively accepted, {} declined.",
                    n(ResponseStatus::Accepted),
                    n(ResponseStatus::Tentative),
                    n(ResponseStatus::Declined)
                );
                ui.label(RichText::new(text).color(app.theme.text_muted));
            }
        }
        _ => {}
    }
    if let Some(id) = m.appointment
        && m.method != "REPLY"
        && ui.button("Open in Calendar").clicked()
    {
        app.run("nav.calendar", json!({}));
        app.ui.module_selection = Some(id);
    }
}

/// The task area of a message carrying a task (a request, a response or a status report). Returns true
/// when it drew something.
pub(crate) fn task_bar(ui: &mut Ui, app: &mut App, mail: u64) -> bool {
    let ctx = ui.ctx().clone();
    let Some(t) = task_summary(&ctx, app, mail) else { return false };
    let theme = app.theme.clone();
    egui::Frame::new()
        .fill(theme.info_bar)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                app.icons.paint(ui, rect, "task", theme.accent);
                ui.add(egui::Label::new(RichText::new(&t.info).color(theme.text)).wrap());
            });
            ui.add_space(4.0);
            egui::Grid::new(("mc-task-grid", mail)).num_columns(2).spacing(vec2(8.0, 2.0)).show(ui, |ui| {
                let row = |ui: &mut Ui, label: &str, value: &str| {
                    ui.label(RichText::new(label).color(theme.text_muted));
                    ui.label(value);
                    ui.end_row();
                };
                row(ui, "Subject", &t.subject);
                if let Some(d) = &t.start {
                    row(ui, "Start date", d);
                }
                row(ui, "Due date", t.due.as_deref().unwrap_or("None"));
                row(ui, "Status", task_status_label(&t.status));
                row(ui, "% Complete", &format!("{}%", t.percent));
                if !t.requested_by.trim().is_empty() {
                    row(ui, "Assigned by", &t.requested_by);
                }
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| task_buttons(ui, app, &t));
        });
    true
}

/// Display name of a task status key (`tasks::status_name`).
fn task_status_label(key: &str) -> &'static str {
    match key {
        "inProgress" => "In Progress",
        "completed" => "Completed",
        "waiting" | "waitingOnSomeoneElse" => "Waiting on someone else",
        "deferred" => "Deferred",
        _ => "Not Started",
    }
}

fn task_respond_menu(ui: &mut Ui, app: &mut App, t: &TaskRequestInfo, label: &str, cmd: &str) {
    ui.menu_button(label, |ui| {
        for (text, send) in [("Edit the response before sending", "edit"), ("Send the response now", "now")] {
            if ui.button(text).clicked() {
                app.run(cmd, json!({ "id": t.mail, "send": send }));
                ui.close();
            }
        }
    });
}

fn task_buttons(ui: &mut Ui, app: &mut App, t: &TaskRequestInfo) {
    if t.method == "REQUEST" && t.can_respond {
        task_respond_menu(ui, app, t, "Accept", "tasks.acceptRequest");
        task_respond_menu(ui, app, t, "Decline", "tasks.declineRequest");
    }
    if t.method == "REPLY"
        && t.can_update
        && ui.button("Update").on_hover_text("Update my copy of the task").clicked()
    {
        app.run("tasks.processReport", json!({ "id": t.mail }));
    }
    if let Some(id) = t.task.filter(|id| app.s.mailbox.tasks.contains_key(&ItemId(*id)))
        && ui.button("Open Task").clicked()
    {
        app.ui.dialogs.push(("task".to_owned(), json!({ "id": id })));
    }
}

fn open_propose(ctx: &egui::Context, app: &App, m: &MeetingMessage, response: &str) {
    let tz = app.s.tz_offset_min;
    let s = local_secs(mailcraft_model::DateTime::from_unix(m.start, tz), tz);
    let e = local_secs(mailcraft_model::DateTime::from_unix(m.end, tz), tz).max(s);
    let p = Propose {
        mail: m.mail,
        response: response.to_owned(),
        date: day_string(s.div_euclid(DAY)),
        start_min: s.rem_euclid(DAY) / 60,
        end_date: day_string(e.div_euclid(DAY)),
        end_min: e.rem_euclid(DAY) / 60,
        error: None,
    };
    ctx.data_mut(|d| d.insert_temp(propose_id(), Some(p)));
}

fn time_combo(ui: &mut Ui, id: &str, minute: &mut i64) {
    ComboBox::from_id_salt(id).selected_text(clock(*minute)).width(100.0).show_ui(ui, |ui| {
        for k in 0..96 {
            let m = k * 15;
            ui.selectable_value(minute, m, clock(m));
        }
    });
}

fn propose_window(ctx: &egui::Context, app: &mut App) {
    let Some(mut p) = ctx.data_mut(|d| d.get_temp::<Option<Propose>>(propose_id())).flatten() else { return };
    let mut open = true;
    let mut done = false;
    egui::Window::new("Propose New Time")
        .id(Id::new(("mc-propose", p.mail)))
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            crate::dialogs::form_visuals(ui, &app.theme);
            egui::Grid::new("mc-propose-grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                ui.label("Start");
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut p.date).desired_width(110.0).hint_text("YYYY-MM-DD"));
                    time_combo(ui, "mc-propose-st", &mut p.start_min);
                });
                ui.end_row();
                ui.label("End");
                ui.horizontal(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut p.end_date).desired_width(110.0).hint_text("YYYY-MM-DD"),
                    );
                    time_combo(ui, "mc-propose-et", &mut p.end_min);
                });
                ui.end_row();
            });
            if let Some(e) = &p.error {
                ui.colored_label(app.theme.danger, e);
            }
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Cancel").clicked() {
                    done = true;
                }
                if ui.button(RichText::new("Propose Time").strong()).clicked() {
                    match (parse_day(&p.date), parse_day(&p.end_date)) {
                        (Some(sd), Some(ed)) => {
                            let params = json!({
                                "id": p.mail,
                                "response": p.response,
                                "start": day_time_string(sd, p.start_min),
                                "end": day_time_string(ed, p.end_min),
                                "send": "edit",
                            });
                            match super::common::run_checked(app, "calendar.proposeNewTime", params) {
                                Ok(_) => done = true,
                                Err(e) => p.error = Some(e),
                            }
                        }
                        _ => p.error = Some("Enter the dates as YYYY-MM-DD.".into()),
                    }
                }
            });
        });
    let keep = open && !done;
    ctx.data_mut(|d| d.insert_temp(propose_id(), keep.then_some(p)));
}
