//! Item windows of the modules: appointment, contact, contact group and task editors, and Go to Date.
//!
//! Each is a local egui window holding plain text fields; Save runs the matching `calendar.*`,
//! `people.*` or `tasks.*` command (new or update). A failed save keeps the window open and shows the
//! command's error.

use egui::{Align, ComboBox, Id, Layout, RichText, ScrollArea, TextEdit, Ui, vec2};
use serde_json::{Value, json};

use mailcraft_engine::cmd::calendar::CalendarMode;
use mailcraft_engine::cmd::calendar::pim::Kind;
use mailcraft_engine::cmd::people::{CONTACT_GROUP_MARKER, is_group};
use mailcraft_model::{BusyStatus, FolderId, ItemId, TaskPriority, TaskStatus};

use super::common::{
    DAY, ModState, clock, current_folder, day_string, day_time_string, load, local_secs, parse_day,
    parse_day_time, run_checked, store, today, tz, weekday,
};
use super::{HANDLED_DIALOGS, calendar_view};
use crate::App;

/// Open item windows.
#[derive(Clone, Default)]
pub(crate) struct Editors {
    pub appt: Option<ApptEditor>,
    pub contact: Option<ContactEditor>,
    pub task: Option<TaskEditor>,
    pub go_to: Option<GoToEditor>,
    /// Makes every newly opened window a fresh egui window (position/size not inherited).
    pub nonce: u64,
}

impl Editors {
    fn next(&mut self) -> u64 {
        self.nonce = self.nonce.wrapping_add(1);
        self.nonce
    }
}

#[derive(Clone, Default)]
pub(crate) struct ApptEditor {
    pub id: Option<u64>,
    pub folder: Option<u64>,
    pub subject: String,
    pub location: String,
    pub body: String,
    pub start_date: String,
    pub start_min: i64,
    pub end_date: String,
    pub end_min: i64,
    pub all_day: bool,
    pub show_as: String,
    pub reminder: Option<u32>,
    pub private: bool,
    pub categories: Vec<String>,
    pub recurrence: String,
    pub attendees: String,
    pub meeting: bool,
    pub error: Option<String>,
    pub nonce: u64,
    pub focus: bool,
}

#[derive(Clone, Default)]
pub(crate) struct ContactEditor {
    pub id: Option<u64>,
    pub folder: Option<u64>,
    pub group: bool,
    pub full_name: String,
    pub orig_full_name: String,
    pub file_as: String,
    pub orig_file_as: String,
    pub company: String,
    pub job_title: String,
    pub department: String,
    pub email: [String; 3],
    pub phone_business: String,
    pub phone_mobile: String,
    pub phone_home: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal: String,
    pub country: String,
    pub had_business_address: bool,
    pub other_addresses: Vec<Value>,
    pub web: String,
    pub im: String,
    pub birthday: String,
    pub notes: String,
    /// Contact group members ("Name <a@b>; …").
    pub members: String,
    pub categories: Vec<String>,
    pub error: Option<String>,
    pub nonce: u64,
    pub focus: bool,
}

#[derive(Clone, Default)]
pub(crate) struct TaskEditor {
    pub id: Option<u64>,
    pub folder: Option<u64>,
    pub subject: String,
    pub start: String,
    pub due: String,
    pub status: String,
    pub priority: String,
    pub percent: u8,
    pub reminder_on: bool,
    pub reminder: String,
    pub body: String,
    pub categories: Vec<String>,
    pub linked_mail: bool,
    pub error: Option<String>,
    pub nonce: u64,
    pub focus: bool,
}

#[derive(Clone, Default)]
pub(crate) struct GoToEditor {
    pub date: String,
    pub mode: String,
    pub error: Option<String>,
    pub nonce: u64,
}

// ---------------------------------------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------------------------------------

/// Opens an existing appointment.
pub(crate) fn open_appointment(app: &App, st: &mut ModState, id: ItemId) {
    let Some(a) = app.s.mailbox.appointments.get(&id) else { return };
    let tz = tz(app);
    let s = local_secs(a.start, tz);
    let mut e = local_secs(a.end, tz).max(s);
    if a.all_day {
        // Shown with the inclusive last day.
        e = (e - DAY).max(s);
    }
    let nonce = st.editors.next();
    st.editors.appt = Some(ApptEditor {
        id: Some(id.0),
        folder: Some(a.folder.0),
        subject: a.subject.clone(),
        location: a.location.clone(),
        body: a.body.clone(),
        start_date: day_string(s.div_euclid(DAY)),
        start_min: s.rem_euclid(DAY) / 60,
        end_date: day_string(e.div_euclid(DAY)),
        end_min: e.rem_euclid(DAY) / 60,
        all_day: a.all_day,
        show_as: show_as_name(a.show_as).to_owned(),
        reminder: a.reminder_min,
        private: a.private,
        categories: a.categories.clone(),
        recurrence: a.recurrence.as_ref().map(|r| r.rrule.clone()).unwrap_or_default(),
        attendees: a
            .attendees
            .iter()
            .filter(|x| x.role == mailcraft_model::calendar::AttendeeRole::Required)
            .map(|x| x.mailbox.to_display_string())
            .collect::<Vec<_>>()
            .join("; "),
        meeting: a.is_meeting,
        error: None,
        nonce,
        focus: false,
    });
}

/// Opens a new appointment at `day` `minute` lasting `dur` minutes (or all day).
pub(crate) fn new_appointment(
    app: &App,
    st: &mut ModState,
    folder: Option<FolderId>,
    day: i64,
    minute: i64,
    dur: i64,
    all_day: bool,
) {
    let folder = folder.or_else(|| current_folder(app, Kind::Appointment));
    let start = day * 24 * 60 + minute;
    let end = start + dur.max(0);
    let nonce = st.editors.next();
    st.editors.appt = Some(ApptEditor {
        folder: folder.map(|f| f.0),
        start_date: day_string(day),
        start_min: minute,
        end_date: day_string(if all_day { day } else { end.div_euclid(24 * 60) }),
        end_min: end.rem_euclid(24 * 60),
        all_day,
        show_as: if all_day { "free" } else { "busy" }.to_owned(),
        reminder: Some(if all_day { 18 * 60 } else { 15 }),
        nonce,
        focus: true,
        ..Default::default()
    });
}

/// Opens an existing contact or contact group.
pub(crate) fn open_contact(app: &App, st: &mut ModState, id: ItemId) {
    let Some(c) = app.s.mailbox.contacts.get(&id) else { return };
    let group = is_group(c);
    let phone = |kind: &str| {
        c.phones
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(kind))
            .map(|(_, n)| n.clone())
            .unwrap_or_default()
    };
    let business = c.addresses.iter().find(|a| a.kind.eq_ignore_ascii_case("Business"));
    let other_addresses = c
        .addresses
        .iter()
        .filter(|a| !a.kind.eq_ignore_ascii_case("Business"))
        .map(|a| {
            json!({ "kind": a.kind, "street": a.street, "city": a.city, "region": a.region,
                    "postalCode": a.postal_code, "country": a.country })
        })
        .collect();
    let notes = if group {
        c.notes.split_once('\n').map(|(_, rest)| rest.to_owned()).unwrap_or_default()
    } else {
        c.notes.clone()
    };
    let full = c.full_name();
    let nonce = st.editors.next();
    st.editors.contact = Some(ContactEditor {
        id: Some(id.0),
        folder: Some(c.folder.0),
        group,
        full_name: if group { c.file_as.clone() } else { full.clone() },
        orig_full_name: full,
        file_as: c.file_as.clone(),
        orig_file_as: c.file_as.clone(),
        company: c.company.clone(),
        job_title: c.job_title.clone(),
        department: c.department.clone(),
        email: [0, 1, 2]
            .map(|i| if group { String::new() } else { c.emails.get(i).cloned().unwrap_or_default() }),
        phone_business: phone("Business"),
        phone_mobile: phone("Mobile"),
        phone_home: phone("Home"),
        street: business.map(|a| a.street.clone()).unwrap_or_default(),
        city: business.map(|a| a.city.clone()).unwrap_or_default(),
        region: business.map(|a| a.region.clone()).unwrap_or_default(),
        postal: business.map(|a| a.postal_code.clone()).unwrap_or_default(),
        country: business.map(|a| a.country.clone()).unwrap_or_default(),
        had_business_address: business.is_some(),
        other_addresses,
        web: c.web_page.clone(),
        im: c.im_address.clone(),
        birthday: c.birthday.clone().unwrap_or_default(),
        notes,
        members: if group { c.emails.join("; ") } else { String::new() },
        categories: c.categories.clone(),
        error: None,
        nonce,
        focus: false,
    });
}

/// Opens a blank contact (or contact group) window.
pub(crate) fn new_contact(app: &App, st: &mut ModState, folder: Option<FolderId>, group: bool) {
    let folder = folder.or_else(|| current_folder(app, Kind::Contact));
    let nonce = st.editors.next();
    st.editors.contact =
        Some(ContactEditor { folder: folder.map(|f| f.0), group, nonce, focus: true, ..Default::default() });
}

/// Opens an existing task.
pub(crate) fn open_task(app: &App, st: &mut ModState, id: ItemId) {
    let Some(t) = app.s.mailbox.tasks.get(&id) else { return };
    let tz = tz(app);
    let date = |d: Option<mailcraft_model::DateTime>| {
        d.map(|d| day_string(local_secs(d, tz).div_euclid(DAY))).unwrap_or_default()
    };
    let nonce = st.editors.next();
    st.editors.task = Some(TaskEditor {
        id: Some(id.0),
        folder: Some(t.folder.0),
        subject: t.subject.clone(),
        start: date(t.start),
        due: date(t.due),
        status: mailcraft_engine::cmd::tasks::status_name(t.status).to_owned(),
        priority: priority_name(t.priority).to_owned(),
        percent: t.percent_complete,
        reminder_on: t.reminder.is_some(),
        reminder: t
            .reminder
            .map(|r| {
                let s = local_secs(r, tz);
                day_time_string(s.div_euclid(DAY), s.rem_euclid(DAY) / 60)
            })
            .unwrap_or_default(),
        body: t.body.clone(),
        categories: t.categories.clone(),
        linked_mail: t.from_mail.is_some(),
        error: None,
        nonce,
        focus: false,
    });
}

/// Opens a blank task window.
pub(crate) fn new_task(app: &App, st: &mut ModState, folder: Option<FolderId>) {
    let folder = folder.or_else(|| current_folder(app, Kind::Task));
    let nonce = st.editors.next();
    st.editors.task = Some(TaskEditor {
        folder: folder.map(|f| f.0),
        status: "notStarted".into(),
        priority: "normal".into(),
        reminder: day_time_string(today(app) + 1, 8 * 60),
        nonce,
        focus: true,
        ..Default::default()
    });
}

/// Opens Go to Date.
pub(crate) fn open_go_to(st: &mut ModState, date: &str, mode: CalendarMode) {
    let nonce = st.editors.next();
    let mode = if mode == CalendarMode::Schedule { CalendarMode::Day } else { mode };
    st.editors.go_to =
        Some(GoToEditor { date: date.to_owned(), mode: mode.name().to_owned(), error: None, nonce });
}

/// Opens the window a command asked for (`Notice::OpenDialog` forwarded into `app.ui.dialogs`).
fn open_from_dialog(app: &mut App, st: &mut ModState, name: &str, p: &Value) {
    let folder = p.get("folder").and_then(Value::as_u64).map(FolderId);
    let id = p.get("id").and_then(Value::as_u64).map(ItemId);
    match name {
        "appointment" => {
            if let Some(id) = id {
                open_appointment(app, st, id);
                return;
            }
            let now = today(app);
            let start =
                p.get("start").and_then(Value::as_str).and_then(parse_day_time).unwrap_or((now, 9 * 60));
            let end = p
                .get("end")
                .and_then(Value::as_str)
                .and_then(parse_day_time)
                .unwrap_or((start.0, start.1 + 30));
            let dur = (end.0 - start.0) * 24 * 60 + end.1 - start.1;
            new_appointment(app, st, folder, start.0, start.1, dur.max(0), false);
            if let Some(ed) = st.editors.appt.as_mut()
                && let Some(r) = p.get("reminderMin")
            {
                ed.reminder = r.as_u64().and_then(|m| u32::try_from(m).ok());
            }
        }
        "contact" | "contactGroup" => {
            if let Some(id) = id {
                open_contact(app, st, id);
                return;
            }
            new_contact(app, st, folder, name == "contactGroup");
            if let (Some(ed), Some(pre)) = (st.editors.contact.as_mut(), p.get("prefill")) {
                ed.company = pre.get("company").and_then(Value::as_str).unwrap_or("").to_owned();
                if let Some(a) = pre.get("businessAddress").filter(|a| a.is_object()) {
                    let f = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
                    ed.street = f("street");
                    ed.city = f("city");
                    ed.region = f("region");
                    ed.postal = f("postalCode");
                    ed.country = f("country");
                }
                if let Some(ph) = pre.get("phones").and_then(Value::as_object) {
                    for (k, v) in ph {
                        if k.to_ascii_lowercase().starts_with("business") {
                            ed.phone_business = v.as_str().unwrap_or("").to_owned();
                        }
                    }
                }
            }
        }
        "task" => match id {
            Some(id) => open_task(app, st, id),
            None => new_task(app, st, folder),
        },
        "note" => {
            if let Some(id) = id {
                let text = app.s.mailbox.notes.get(&id).map(|n| n.text.clone()).unwrap_or_default();
                st.note_edit = Some((id.0, text));
                st.note_focus = true;
                app.ui.module_selection = Some(id.0);
            }
        }
        "goToDate" => {
            let date = p
                .get("date")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| st.cal.date.clone());
            let mode =
                p.get("mode").and_then(Value::as_str).and_then(CalendarMode::parse).unwrap_or(st.cal.mode);
            open_go_to(st, &date, mode);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------------------------

/// Draws every open item window (once per pass).
pub(crate) fn show(ctx: &egui::Context, app: &mut App) {
    let mut st = load(ctx);
    let pass = ctx.cumulative_pass_nr();
    if st.windows_pass == Some(pass) {
        return;
    }
    st.windows_pass = Some(pass);
    let mut i = 0;
    while let Some((name, params)) = app.ui.dialogs.get(i).cloned() {
        if HANDLED_DIALOGS.contains(&name.as_str()) {
            app.ui.dialogs.remove(i);
            open_from_dialog(app, &mut st, &name, &params);
        } else {
            i += 1;
        }
    }
    appointment_window(ctx, app, &mut st);
    contact_window(ctx, app, &mut st);
    task_window(ctx, app, &mut st);
    go_to_window(ctx, app, &mut st);
    store(ctx, st);
}

fn window<'a>(title: String, id: (&'static str, u64)) -> egui::Window<'a> {
    egui::Window::new(title).id(Id::new(id)).collapsible(false).resizable(true).default_width(560.0)
}

fn field(ui: &mut Ui, label: &str, text: &mut String, hint: &str) -> egui::Response {
    ui.label(label);
    let r = ui.add(TextEdit::singleline(text).hint_text(hint).desired_width(f32::INFINITY));
    ui.end_row();
    r
}

fn date_field(ui: &mut Ui, text: &mut String, id: &str) -> bool {
    let ok = text.trim().is_empty() || parse_day(text).is_some();
    let mut edit = TextEdit::singleline(text).hint_text("YYYY-MM-DD").desired_width(110.0).id_salt(id);
    if !ok {
        edit = edit.text_color(egui::Color32::from_rgb(0xc4, 0x31, 0x2f));
    }
    ui.add(edit).on_hover_text("Date as YYYY-MM-DD");
    ok
}

fn time_combo(ui: &mut Ui, id: &str, minute: &mut i64) {
    ComboBox::from_id_salt(id).selected_text(clock(*minute)).width(100.0).show_ui(ui, |ui| {
        for k in 0..48 {
            let m = k * 30;
            ui.selectable_value(minute, m, clock(m));
        }
    });
}

fn categories_menu(ui: &mut Ui, app: &App, cats: &mut Vec<String>) {
    let text = if cats.is_empty() { "None".to_owned() } else { cats.join(", ") };
    ui.menu_button(text, |ui| {
        for c in &app.s.mailbox.categories {
            let mut on = cats.iter().any(|x| x.eq_ignore_ascii_case(&c.name));
            if ui.checkbox(&mut on, &c.name).changed() {
                if on {
                    cats.push(c.name.clone());
                } else {
                    cats.retain(|x| !x.eq_ignore_ascii_case(&c.name));
                }
            }
        }
    });
}

fn error_line(ui: &mut Ui, app: &App, error: &Option<String>) {
    if let Some(e) = error {
        ui.colored_label(app.theme.danger, e);
    }
}

/// Buttons row: returns (save, delete, cancel).
fn buttons(ui: &mut Ui, can_delete: bool) -> (bool, bool, bool) {
    let mut out = (false, false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        out.2 = ui.button("Cancel").clicked();
        if can_delete {
            out.1 = ui.button("Delete").clicked();
        }
        out.0 = ui.button(RichText::new("Save & Close").strong()).clicked();
    });
    out
}

const SHOW_AS: [(&str, &str); 5] = [
    ("free", "Free"),
    ("workingElsewhere", "Working Elsewhere"),
    ("tentative", "Tentative"),
    ("busy", "Busy"),
    ("outOfOffice", "Out of Office"),
];

fn show_as_name(b: BusyStatus) -> &'static str {
    match b {
        BusyStatus::Free => "free",
        BusyStatus::WorkingElsewhere => "workingElsewhere",
        BusyStatus::Tentative => "tentative",
        BusyStatus::Busy => "busy",
        BusyStatus::OutOfOffice => "outOfOffice",
    }
}

fn reminder_label(r: Option<u32>) -> String {
    match r {
        None => "None".into(),
        Some(0) => "At start".into(),
        Some(m) if m % (7 * 24 * 60) == 0 => format!("{} week(s)", m / (7 * 24 * 60)),
        Some(m) if m % (24 * 60) == 0 => format!("{} day(s)", m / (24 * 60)),
        Some(m) if m % 60 == 0 => format!("{} hour(s)", m / 60),
        Some(m) => format!("{m} minutes"),
    }
}

/// (label, RRULE) choices; weekly repeats on the start's weekday.
fn recurrence_choices(start_day: Option<i64>) -> Vec<(String, String)> {
    let wd = start_day.map(weekday).unwrap_or(1);
    let code = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"].get(usize::from(wd)).copied().unwrap_or("MO");
    vec![
        ("Does not repeat".into(), String::new()),
        ("Every day".into(), "FREQ=DAILY".into()),
        ("Every weekday".into(), "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into()),
        ("Every week".into(), format!("FREQ=WEEKLY;BYDAY={code}")),
        ("Every month".into(), "FREQ=MONTHLY".into()),
        ("Every year".into(), "FREQ=YEARLY".into()),
    ]
}

fn split_list(s: &str) -> Vec<String> {
    s.split(';').map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned).collect()
}

fn appointment_window(ctx: &egui::Context, app: &mut App, st: &mut ModState) {
    let Some(mut ed) = st.editors.appt.take() else { return };
    let mut open = true;
    let kind = if ed.meeting || !ed.attendees.trim().is_empty() { "Meeting" } else { "Appointment" };
    let title =
        format!("{} - {kind}", if ed.subject.trim().is_empty() { "Untitled" } else { ed.subject.trim() });
    let mut action = (false, false, false);
    window(title, ("mc-appt-editor", ed.nonce)).open(&mut open).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &app.theme);
        let before =
            parse_day(&ed.start_date).map(|d| d * 24 * 60 + if ed.all_day { 0 } else { ed.start_min });
        egui::Grid::new("mc-appt-grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            let r = field(ui, "Title", &mut ed.subject, "Add a title");
            if ed.focus {
                r.request_focus();
                ed.focus = false;
            }
            if ed.meeting || !ed.attendees.trim().is_empty() || ed.id.is_none() {
                field(
                    ui,
                    "Attendees",
                    &mut ed.attendees,
                    "Name <address>; …  (invites are sent as a meeting)",
                );
            }
            field(ui, "Location", &mut ed.location, "Add a location");
            ui.label("Start");
            ui.horizontal(|ui| {
                date_field(ui, &mut ed.start_date, "mc-appt-sd");
                if !ed.all_day {
                    time_combo(ui, "mc-appt-st", &mut ed.start_min);
                }
                if ui.checkbox(&mut ed.all_day, "All day").changed() {
                    ed.show_as = if ed.all_day { "free" } else { "busy" }.into();
                }
            });
            ui.end_row();
            ui.label("End");
            ui.horizontal(|ui| {
                date_field(ui, &mut ed.end_date, "mc-appt-ed");
                if !ed.all_day {
                    time_combo(ui, "mc-appt-et", &mut ed.end_min);
                }
            });
            ui.end_row();
            ui.label("Repeat");
            let choices = recurrence_choices(parse_day(&ed.start_date));
            let current = choices
                .iter()
                .find(|(_, r)| r.eq_ignore_ascii_case(&ed.recurrence))
                .map(|(l, _)| l.clone())
                .unwrap_or_else(|| format!("Custom ({})", ed.recurrence));
            ComboBox::from_id_salt("mc-appt-rec").selected_text(current).width(220.0).show_ui(ui, |ui| {
                for (label, rule) in &choices {
                    ui.selectable_value(&mut ed.recurrence, rule.clone(), label);
                }
            });
            ui.end_row();
            ui.label("Show as");
            let cur = SHOW_AS.iter().find(|(k, _)| *k == ed.show_as).map(|(_, l)| *l).unwrap_or("Busy");
            ComboBox::from_id_salt("mc-appt-showas").selected_text(cur).width(160.0).show_ui(ui, |ui| {
                for (k, l) in SHOW_AS {
                    ui.selectable_value(&mut ed.show_as, k.to_owned(), l);
                }
            });
            ui.end_row();
            ui.label("Reminder");
            ComboBox::from_id_salt("mc-appt-rem")
                .selected_text(reminder_label(ed.reminder))
                .width(160.0)
                .show_ui(ui, |ui| {
                    for r in [
                        None,
                        Some(0),
                        Some(5),
                        Some(10),
                        Some(15),
                        Some(30),
                        Some(60),
                        Some(120),
                        Some(24 * 60),
                        Some(18 * 60),
                        Some(7 * 24 * 60),
                    ] {
                        ui.selectable_value(&mut ed.reminder, r, reminder_label(r));
                    }
                });
            ui.end_row();
            ui.label("Categories");
            categories_menu(ui, app, &mut ed.categories);
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut ed.private, "Private");
            ui.end_row();
        });
        ui.add_space(4.0);
        ui.add(
            TextEdit::multiline(&mut ed.body).hint_text("Notes").desired_rows(5).desired_width(f32::INFINITY),
        );
        // Moving the start keeps the duration.
        let after =
            parse_day(&ed.start_date).map(|d| d * 24 * 60 + if ed.all_day { 0 } else { ed.start_min });
        if let (Some(b), Some(a), Some(end_day)) = (before, after, parse_day(&ed.end_date))
            && a != b
        {
            let end = end_day * 24 * 60 + if ed.all_day { 0 } else { ed.end_min } + (a - b);
            ed.end_date = day_string(end.div_euclid(24 * 60));
            if !ed.all_day {
                ed.end_min = end.rem_euclid(24 * 60);
            }
        }
        error_line(ui, app, &ed.error);
        ui.separator();
        action = buttons(ui, ed.id.is_some());
    });
    let (save, delete, cancel) = action;
    if save {
        match save_appointment(app, &ed) {
            Ok(id) => {
                if let Some(id) = id {
                    app.ui.module_selection = Some(id);
                }
                return;
            }
            Err(e) => ed.error = Some(e),
        }
    }
    if delete && let Some(id) = ed.id {
        app.run("calendar.delete", json!({ "id": id }));
        if app.ui.module_selection == Some(id) {
            app.ui.module_selection = None;
        }
        return;
    }
    if open && !cancel {
        st.editors.appt = Some(ed);
    }
}

/// Saves the appointment editor; returns the item id.
pub(crate) fn save_appointment(app: &mut App, ed: &ApptEditor) -> Result<Option<u64>, String> {
    let sd = parse_day(&ed.start_date).ok_or("Enter the start date as YYYY-MM-DD.")?;
    let end_day = parse_day(&ed.end_date).ok_or("Enter the end date as YYYY-MM-DD.")?;
    let (start, end) = if ed.all_day {
        if end_day < sd {
            return Err("The end date is before the start date.".into());
        }
        (day_string(sd), day_string(end_day + 1))
    } else {
        if end_day * 1440 + ed.end_min < sd * 1440 + ed.start_min {
            return Err("The end time is before the start time.".into());
        }
        (day_time_string(sd, ed.start_min), day_time_string(end_day, ed.end_min))
    };
    let attendees = split_list(&ed.attendees);
    let mut p = json!({
        "subject": ed.subject,
        "location": ed.location,
        "body": ed.body,
        "start": start,
        "end": end,
        "allDay": ed.all_day,
        "showAs": ed.show_as,
        "reminderMin": ed.reminder,
        "private": ed.private,
        "categories": ed.categories,
        "recurrence": if ed.recurrence.trim().is_empty() { Value::Null } else { json!(ed.recurrence) },
    });
    let Some(o) = p.as_object_mut() else { return Err("internal error".into()) };
    let (id, out) = if let Some(id) = ed.id {
        o.insert("id".into(), json!(id));
        if ed.meeting || !attendees.is_empty() {
            o.insert("attendees".into(), json!(attendees));
        }
        (Some(id), run_checked(app, "calendar.update", p)?)
    } else {
        if let Some(f) = ed.folder {
            o.insert("folder".into(), json!(f));
        }
        let cmd = if attendees.is_empty() {
            "calendar.newAppointment"
        } else {
            o.insert("attendees".into(), json!(attendees));
            "calendar.newMeeting"
        };
        let out = run_checked(app, cmd, p)?;
        (out.get("id").and_then(Value::as_u64), out)
    };
    let _ = out;
    Ok(id)
}

fn contact_window(ctx: &egui::Context, app: &mut App, st: &mut ModState) {
    let Some(mut ed) = st.editors.contact.take() else { return };
    let mut open = true;
    let kind = if ed.group { "Contact Group" } else { "Contact" };
    let title =
        format!("{} - {kind}", if ed.full_name.trim().is_empty() { "Untitled" } else { ed.full_name.trim() });
    let mut action = (false, false, false);
    window(title, ("mc-contact-editor", ed.nonce)).open(&mut open).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &app.theme);
        ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
            egui::Grid::new("mc-contact-grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                let r = field(ui, if ed.group { "Name" } else { "Full name" }, &mut ed.full_name, "");
                if ed.focus {
                    r.request_focus();
                    ed.focus = false;
                }
                if ed.group {
                    ui.label("Members");
                    ui.add(
                        TextEdit::multiline(&mut ed.members)
                            .hint_text("Name <address>; …")
                            .desired_rows(4)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                } else {
                    field(ui, "Company", &mut ed.company, "");
                    field(ui, "Job title", &mut ed.job_title, "");
                    field(ui, "Department", &mut ed.department, "");
                    field(ui, "File as", &mut ed.file_as, "Last, First (automatic when empty)");
                    let [e1, e2, e3] = &mut ed.email;
                    field(ui, "Email", e1, "name@example.com");
                    field(ui, "Email 2", e2, "");
                    field(ui, "Email 3", e3, "");
                    field(ui, "Business phone", &mut ed.phone_business, "");
                    field(ui, "Mobile phone", &mut ed.phone_mobile, "");
                    field(ui, "Home phone", &mut ed.phone_home, "");
                    field(ui, "Street", &mut ed.street, "Business address");
                    field(ui, "City", &mut ed.city, "");
                    field(ui, "State/Province", &mut ed.region, "");
                    field(ui, "Postal code", &mut ed.postal, "");
                    field(ui, "Country/Region", &mut ed.country, "");
                    field(ui, "Web page", &mut ed.web, "");
                    field(ui, "IM address", &mut ed.im, "");
                    ui.label("Birthday");
                    date_field(ui, &mut ed.birthday, "mc-contact-bday");
                    ui.end_row();
                }
                ui.label("Categories");
                categories_menu(ui, app, &mut ed.categories);
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.add(
                TextEdit::multiline(&mut ed.notes)
                    .hint_text("Notes")
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            );
        });
        error_line(ui, app, &ed.error);
        ui.separator();
        action = buttons(ui, ed.id.is_some());
    });
    let (save, delete, cancel) = action;
    if save {
        match save_contact(app, &ed) {
            Ok(id) => {
                if let Some(id) = id {
                    app.ui.module_selection = Some(id);
                }
                return;
            }
            Err(e) => ed.error = Some(e),
        }
    }
    if delete && let Some(id) = ed.id {
        app.run("people.delete", json!({ "ids": [id] }));
        if app.ui.module_selection == Some(id) {
            app.ui.module_selection = None;
        }
        return;
    }
    if open && !cancel {
        st.editors.contact = Some(ed);
    }
}

/// Saves the contact editor; returns the item id.
pub(crate) fn save_contact(app: &mut App, ed: &ContactEditor) -> Result<Option<u64>, String> {
    let mut p = serde_json::Map::new();
    if ed.group {
        if ed.full_name.trim().is_empty() {
            return Err("Give the contact group a name.".into());
        }
        p.insert("name".into(), json!(ed.full_name.trim()));
        p.insert("members".into(), json!(split_list(&ed.members)));
        p.insert("notes".into(), json!(ed.notes));
        p.insert("categories".into(), json!(ed.categories));
    } else {
        if ed.full_name.trim().is_empty()
            && ed.company.trim().is_empty()
            && ed.email.iter().all(|e| e.trim().is_empty())
        {
            return Err("Enter a name, a company or an email address.".into());
        }
        if !ed.birthday.trim().is_empty() && parse_day(&ed.birthday).is_none() {
            return Err("Enter the birthday as YYYY-MM-DD.".into());
        }
        if ed.notes.lines().next().is_some_and(|l| l.trim() == CONTACT_GROUP_MARKER) {
            return Err(format!("Notes cannot start with \"{CONTACT_GROUP_MARKER}\"."));
        }
        if ed.id.is_none() || ed.full_name.trim() != ed.orig_full_name.trim() {
            p.insert("fullName".into(), json!(ed.full_name.trim()));
        }
        if ed.file_as.trim() != ed.orig_file_as.trim() {
            p.insert("fileAs".into(), json!(ed.file_as.trim()));
        }
        let [email1, email2, email3] = &ed.email;
        for (k, v) in [
            ("company", &ed.company),
            ("jobTitle", &ed.job_title),
            ("department", &ed.department),
            ("webPage", &ed.web),
            ("im", &ed.im),
            ("notes", &ed.notes),
            ("email", email1),
            ("email2", email2),
            ("email3", email3),
        ] {
            p.insert(k.into(), json!(v.trim()));
        }
        p.insert(
            "phones".into(),
            json!({ "Business": ed.phone_business.trim(), "Mobile": ed.phone_mobile.trim(), "Home": ed.phone_home.trim() }),
        );
        let addr = [&ed.street, &ed.city, &ed.region, &ed.postal, &ed.country];
        if addr.iter().any(|x| !x.trim().is_empty()) {
            p.insert(
                "businessAddress".into(),
                json!({ "street": ed.street.trim(), "city": ed.city.trim(), "region": ed.region.trim(),
                        "postalCode": ed.postal.trim(), "country": ed.country.trim() }),
            );
        } else if ed.had_business_address {
            p.insert("addresses".into(), Value::Array(ed.other_addresses.clone()));
        }
        p.insert("birthday".into(), json!(ed.birthday.trim()));
        p.insert("categories".into(), json!(ed.categories));
    }
    if let Some(id) = ed.id {
        p.insert("id".into(), json!(id));
        run_checked(app, "people.update", Value::Object(p))?;
        Ok(Some(id))
    } else {
        if let Some(f) = ed.folder {
            p.insert("folder".into(), json!(f));
        }
        let cmd = if ed.group { "people.newGroup" } else { "people.new" };
        let out = run_checked(app, cmd, Value::Object(p))?;
        Ok(out.get("id").and_then(Value::as_u64))
    }
}

const STATUSES: [(&str, &str); 5] = [
    ("notStarted", "Not Started"),
    ("inProgress", "In Progress"),
    ("completed", "Completed"),
    ("waitingOnSomeoneElse", "Waiting on someone else"),
    ("deferred", "Deferred"),
];

const PRIORITIES: [(&str, &str); 3] = [("low", "Low"), ("normal", "Normal"), ("high", "High")];

pub(crate) fn status_label(key: &str) -> &'static str {
    STATUSES.iter().find(|(k, _)| *k == key).map(|(_, l)| *l).unwrap_or("Not Started")
}

pub(crate) fn priority_name(p: TaskPriority) -> &'static str {
    match p {
        TaskPriority::Low => "low",
        TaskPriority::Normal => "normal",
        TaskPriority::High => "high",
    }
}

pub(crate) fn status_key(s: TaskStatus) -> &'static str {
    mailcraft_engine::cmd::tasks::status_name(s)
}

fn task_window(ctx: &egui::Context, app: &mut App, st: &mut ModState) {
    let Some(mut ed) = st.editors.task.take() else { return };
    let mut open = true;
    let title =
        format!("{} - Task", if ed.subject.trim().is_empty() { "Untitled" } else { ed.subject.trim() });
    let mut action = (false, false, false);
    let now = today(app);
    window(title, ("mc-task-editor", ed.nonce)).open(&mut open).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &app.theme);
        egui::Grid::new("mc-task-grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            let r = field(ui, "Subject", &mut ed.subject, "Add a task");
            if ed.focus {
                r.request_focus();
                ed.focus = false;
            }
            for (label, value, id) in
                [("Start date", &mut ed.start, "mc-task-start"), ("Due date", &mut ed.due, "mc-task-due")]
            {
                ui.label(label);
                ui.horizontal(|ui| {
                    date_field(ui, value, id);
                    if ui.small_button("Today").clicked() {
                        *value = day_string(now);
                    }
                    if ui.small_button("Tomorrow").clicked() {
                        *value = day_string(now + 1);
                    }
                    if ui.small_button("None").clicked() {
                        value.clear();
                    }
                });
                ui.end_row();
            }
            ui.label("Status");
            ComboBox::from_id_salt("mc-task-status")
                .selected_text(status_label(&ed.status))
                .width(200.0)
                .show_ui(ui, |ui| {
                    for (k, l) in STATUSES {
                        ui.selectable_value(&mut ed.status, k.to_owned(), l);
                    }
                });
            ui.end_row();
            ui.label("Priority");
            let cur = PRIORITIES.iter().find(|(k, _)| *k == ed.priority).map(|(_, l)| *l).unwrap_or("Normal");
            ComboBox::from_id_salt("mc-task-prio").selected_text(cur).width(200.0).show_ui(ui, |ui| {
                for (k, l) in PRIORITIES {
                    ui.selectable_value(&mut ed.priority, k.to_owned(), l);
                }
            });
            ui.end_row();
            ui.label("% Complete");
            let mut pct = i64::from(ed.percent);
            if ui.add(egui::Slider::new(&mut pct, 0..=100).step_by(25.0).suffix("%")).changed() {
                ed.percent = u8::try_from(pct.clamp(0, 100)).unwrap_or(0);
                if ed.percent == 100 {
                    ed.status = "completed".into();
                } else if ed.status == "completed" {
                    ed.status = if ed.percent > 0 { "inProgress" } else { "notStarted" }.into();
                }
            }
            ui.end_row();
            ui.label("Reminder");
            ui.horizontal(|ui| {
                ui.checkbox(&mut ed.reminder_on, "");
                ui.add_enabled(
                    ed.reminder_on,
                    TextEdit::singleline(&mut ed.reminder).hint_text("YYYY-MM-DDTHH:MM").desired_width(150.0),
                );
            });
            ui.end_row();
            ui.label("Categories");
            categories_menu(ui, app, &mut ed.categories);
            ui.end_row();
        });
        if ed.linked_mail {
            ui.label(RichText::new("Linked to a flagged message.").color(app.theme.text_muted));
        }
        ui.add_space(4.0);
        ui.add(
            TextEdit::multiline(&mut ed.body).hint_text("Notes").desired_rows(5).desired_width(f32::INFINITY),
        );
        error_line(ui, app, &ed.error);
        ui.separator();
        action = buttons(ui, ed.id.is_some());
    });
    let (save, delete, cancel) = action;
    if save {
        match save_task(app, &ed) {
            Ok(id) => {
                if let Some(id) = id {
                    app.ui.module_selection = Some(id);
                }
                return;
            }
            Err(e) => ed.error = Some(e),
        }
    }
    if delete && let Some(id) = ed.id {
        app.run("tasks.delete", json!({ "id": id }));
        if app.ui.module_selection == Some(id) {
            app.ui.module_selection = None;
        }
        return;
    }
    if open && !cancel {
        st.editors.task = Some(ed);
    }
}

/// Saves the task editor; returns the item id.
pub(crate) fn save_task(app: &mut App, ed: &TaskEditor) -> Result<Option<u64>, String> {
    if ed.subject.trim().is_empty() {
        return Err("Give the task a subject.".into());
    }
    let date = |s: &str, what: &str| -> Result<Value, String> {
        if s.trim().is_empty() {
            Ok(Value::Null)
        } else {
            parse_day(s)
                .map(|d| json!(day_string(d)))
                .ok_or_else(|| format!("Enter the {what} as YYYY-MM-DD."))
        }
    };
    let reminder = if ed.reminder_on {
        let (d, m) = parse_day_time(&ed.reminder).ok_or("Enter the reminder as YYYY-MM-DDTHH:MM.")?;
        json!(day_time_string(d, m))
    } else {
        Value::Null
    };
    let mut p = serde_json::Map::new();
    p.insert("subject".into(), json!(ed.subject.trim()));
    p.insert("body".into(), json!(ed.body));
    p.insert("start".into(), date(&ed.start, "start date")?);
    p.insert("due".into(), date(&ed.due, "due date")?);
    p.insert("priority".into(), json!(ed.priority));
    p.insert("status".into(), json!(ed.status));
    if ed.status != "completed" {
        p.insert("percent".into(), json!(ed.percent.min(99)));
    }
    p.insert("reminder".into(), reminder);
    p.insert("categories".into(), json!(ed.categories));
    if let Some(id) = ed.id {
        p.insert("id".into(), json!(id));
        run_checked(app, "tasks.update", Value::Object(p))?;
        Ok(Some(id))
    } else {
        if let Some(f) = ed.folder {
            p.insert("folder".into(), json!(f));
        }
        let out = run_checked(app, "tasks.new", Value::Object(p))?;
        Ok(out.get("id").and_then(Value::as_u64))
    }
}

fn go_to_window(ctx: &egui::Context, app: &mut App, st: &mut ModState) {
    let Some(mut ed) = st.editors.go_to.take() else { return };
    let mut open = true;
    let mut ok = false;
    let mut cancel = false;
    window("Go To Date".into(), ("mc-goto", ed.nonce))
        .default_width(320.0)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            crate::dialogs::form_visuals(ui, &app.theme);
            egui::Grid::new("mc-goto-grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                ui.label("Date");
                ui.horizontal(|ui| {
                    let r = ui
                        .add(TextEdit::singleline(&mut ed.date).hint_text("YYYY-MM-DD").desired_width(120.0));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        ok = true;
                    }
                    if ui.small_button("Today").clicked() {
                        ed.date = day_string(today(app));
                    }
                });
                ui.end_row();
                ui.label("Show in");
                let label = |m: &str| match m {
                    "day" => "Day Calendar",
                    "workWeek" => "Work Week Calendar",
                    "week" => "Week Calendar",
                    _ => "Month Calendar",
                };
                ComboBox::from_id_salt("mc-goto-mode").selected_text(label(&ed.mode)).width(180.0).show_ui(
                    ui,
                    |ui| {
                        for m in ["day", "workWeek", "week", "month"] {
                            ui.selectable_value(&mut ed.mode, m.to_owned(), label(m));
                        }
                    },
                );
                ui.end_row();
            });
            error_line(ui, app, &ed.error);
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                cancel = ui.button("Cancel").clicked();
                if ui.button(RichText::new("OK").strong()).clicked() {
                    ok = true;
                }
            });
        });
    if ok {
        match parse_day(&ed.date) {
            Some(d) => {
                let params = json!({ "date": day_string(d), "mode": ed.mode });
                if calendar_view::run_view_cmd(app, st, "calendar.goToDate", params).is_some() {
                    st.nav_month = None;
                    if app.s.view.module != mailcraft_engine::Module::Calendar {
                        app.run("nav.calendar", json!({}));
                    }
                    return;
                }
                ed.error = app.ui.toast.as_ref().map(|t| t.0.clone());
            }
            None => ed.error = Some("Enter the date as YYYY-MM-DD.".into()),
        }
    }
    if open && !cancel {
        st.editors.go_to = Some(ed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_engine::sample::sample_session;

    fn app() -> App {
        App::new(sample_session())
    }

    #[test]
    fn new_appointment_saves_through_command() {
        let mut app = app();
        let mut st = ModState::default();
        let d = parse_day("2026-10-12").unwrap();
        new_appointment(&app, &mut st, None, d, 14 * 60, 60, false);
        let mut ed = st.editors.appt.clone().unwrap();
        ed.subject = "Planning".into();
        ed.categories = vec!["Red category".into()];
        let id = save_appointment(&mut app, &ed).unwrap().unwrap();
        let a = &app.s.mailbox.appointments[&ItemId(id)];
        assert_eq!(a.subject, "Planning");
        assert_eq!(a.end.unix - a.start.unix, 3600);
        assert_eq!(a.categories, vec!["Red category".to_string()]);
        // Edit it back through the editor.
        open_appointment(&app, &mut st, ItemId(id));
        let mut ed = st.editors.appt.clone().unwrap();
        assert_eq!(ed.start_min, 14 * 60);
        ed.all_day = true;
        ed.recurrence = "FREQ=WEEKLY;BYDAY=MO".into();
        save_appointment(&mut app, &ed).unwrap();
        let a = &app.s.mailbox.appointments[&ItemId(id)];
        assert!(a.all_day);
        assert_eq!(a.end.unix - a.start.unix, 86_400);
        assert!(a.recurrence.is_some());
        // Bad dates are reported, nothing changes.
        ed.start_date = "2026-13-01".into();
        assert!(save_appointment(&mut app, &ed).is_err());
    }

    #[test]
    fn contact_round_trip() {
        let mut app = app();
        let mut st = ModState::default();
        new_contact(&app, &mut st, None, false);
        let mut ed = st.editors.contact.clone().unwrap();
        assert!(save_contact(&mut app, &ed).is_err());
        ed.full_name = "Hana Ito".into();
        ed.email[0] = "hana@example.com".into();
        ed.phone_mobile = "+1 555 0199".into();
        ed.city = "Springfield".into();
        let id = save_contact(&mut app, &ed).unwrap().unwrap();
        let c = &app.s.mailbox.contacts[&ItemId(id)];
        assert_eq!(c.file_as, "Ito, Hana");
        assert_eq!(c.addresses.len(), 1);
        open_contact(&app, &mut st, ItemId(id));
        let mut ed = st.editors.contact.clone().unwrap();
        ed.city.clear();
        ed.job_title = "Writer".into();
        save_contact(&mut app, &ed).unwrap();
        let c = &app.s.mailbox.contacts[&ItemId(id)];
        assert!(c.addresses.is_empty());
        assert_eq!(c.job_title, "Writer");
        assert_eq!(c.phones, vec![("Mobile".to_string(), "+1 555 0199".to_string())]);
        // Groups.
        new_contact(&app, &mut st, None, true);
        let mut g = st.editors.contact.clone().unwrap();
        g.full_name = "Team".into();
        g.members = "a@example.com; Bob <b@example.com>".into();
        let gid = save_contact(&mut app, &g).unwrap().unwrap();
        assert!(is_group(&app.s.mailbox.contacts[&ItemId(gid)]));
    }

    #[test]
    fn task_round_trip() {
        let mut app = app();
        let mut st = ModState::default();
        new_task(&app, &mut st, None);
        let mut ed = st.editors.task.clone().unwrap();
        assert!(save_task(&mut app, &ed).is_err());
        ed.subject = "File expenses".into();
        ed.due = "2026-10-20".into();
        ed.priority = "high".into();
        let id = save_task(&mut app, &ed).unwrap().unwrap();
        let t = &app.s.mailbox.tasks[&ItemId(id)];
        assert_eq!(t.priority, TaskPriority::High);
        assert!(t.due.is_some());
        open_task(&app, &mut st, ItemId(id));
        let mut ed = st.editors.task.clone().unwrap();
        ed.status = "completed".into();
        save_task(&mut app, &ed).unwrap();
        assert_eq!(app.s.mailbox.tasks[&ItemId(id)].status, TaskStatus::Completed);
        ed.due = "soon".into();
        assert!(save_task(&mut app, &ed).is_err());
    }

    #[test]
    fn dialogs_are_taken_over() {
        let mut app = app();
        let ctx = egui::Context::default();
        app.ui.dialogs.push(("task".into(), json!({})));
        app.ui.dialogs.push(("other".into(), json!({})));
        app.ui.dialogs.push(("goToDate".into(), json!({ "date": "2026-11-02", "mode": "week" })));
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| show(ui.ctx(), &mut app));
        out.textures_delta.clear();
        assert_eq!(app.ui.dialogs.len(), 1);
        let st = load(&ctx);
        assert!(st.editors.task.is_some());
        assert_eq!(st.editors.go_to.as_ref().unwrap().mode, "week");
    }
}
