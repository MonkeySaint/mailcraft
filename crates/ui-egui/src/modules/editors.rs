//! Item windows of the modules: appointment / meeting, contact, contact group, task and note editors, the
//! recurring-item prompt, and Go to Date.
//!
//! Each is a local egui window holding plain fields; Save runs the matching `calendar.*`, `people.*`,
//! `tasks.*` or `notes.*` command (new or update). A failed save keeps the window open and shows the
//! command's error.
//!
//! The appointment window has three pages: Appointment (subject, attendees, location, start/end with time
//! pickers, all day, recurrence, show as, reminder, categories, private, notes), Scheduling Assistant (see
//! `scheduling`) and Tracking (attendee responses of a meeting). Opening one occurrence of a recurring series
//! first asks whether to open the occurrence or the series; saving an occurrence makes it an exception
//! (`calendar.editOccurrence`).

use std::collections::BTreeSet;

use egui::{
    Align, Color32, ComboBox, CornerRadius, Id, Layout, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
    vec2,
};
use serde_json::{Value, json};

use mailcraft_engine::cmd::calendar::pim::Kind;
use mailcraft_engine::cmd::calendar::{CalendarMode, recurrence::pattern_text};
use mailcraft_engine::cmd::people::{
    CONTACT_GROUP_MARKER, business_card_lines, contact_addresses, display_name, is_group,
};
use mailcraft_model::calendar::{AttendeeRole, ResponseStatus};
use mailcraft_model::contact::PostalAddress;
use mailcraft_model::{BusyStatus, Contact, FolderId, ItemId, NoteColor, TaskPriority, TaskStatus};

use super::common::{
    DAY, ModState, clock, current_folder, day_string, day_time_string, load, local_secs, note_fill,
    parse_day, parse_day_time, persona_fill, run_checked, store, today, tz, weekday,
};
use super::recurrence::{Outcome, RecurrenceDialog};
use super::{HANDLED_DIALOGS, calendar_view};
use crate::App;

/// Open item windows.
#[derive(Clone, Default)]
pub(crate) struct Editors {
    pub appt: Option<ApptEditor>,
    pub contact: Option<ContactEditor>,
    pub task: Option<TaskEditor>,
    pub note: Option<NoteEditor>,
    pub go_to: Option<GoToEditor>,
    /// "Open this occurrence or the series?" for a recurring appointment.
    pub occ_prompt: Option<OccPrompt>,
    /// Makes every newly opened window a fresh egui window (position/size not inherited).
    pub nonce: u64,
}

impl Editors {
    fn next(&mut self) -> u64 {
        self.nonce = self.nonce.wrapping_add(1);
        self.nonce
    }
}

/// Pages of the appointment window.
pub(crate) const PAGE_APPOINTMENT: u8 = 0;
pub(crate) const PAGE_SCHEDULING: u8 = 1;
pub(crate) const PAGE_TRACKING: u8 = 2;

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
    /// Required attendees ("Name <address>; …").
    pub attendees: String,
    /// Optional attendees.
    pub optional: String,
    pub meeting: bool,
    /// The user organizes this meeting (Send Update / Cancel Meeting apply).
    pub organizer_me: bool,
    /// Editing one occurrence of a series: its start (unix seconds).
    pub occurrence: Option<i64>,
    /// Page shown (Appointment, Scheduling Assistant, Tracking).
    pub page: u8,
    /// Day shown by the Scheduling Assistant (`None` = the meeting's day).
    pub scheduling_day: Option<i64>,
    pub recurrence_dlg: Option<RecurrenceDialog>,
    pub error: Option<String>,
    pub nonce: u64,
    pub focus: bool,
}

impl ApptEditor {
    fn is_meeting(&self) -> bool {
        self.meeting || !self.attendees.trim().is_empty() || !self.optional.trim().is_empty()
    }
}

/// Address fields of one postal address (street, city, state/province, postal code, country/region).
pub(crate) type AddressFields = [String; 5];

#[derive(Clone, Default)]
pub(crate) struct ContactEditor {
    pub id: Option<u64>,
    pub folder: Option<u64>,
    pub group: bool,
    pub full_name: String,
    pub orig_full_name: String,
    /// Title, first, middle, last, suffix (the name details).
    pub name_parts: [String; 5],
    pub parts_edited: bool,
    pub parts_open: bool,
    pub file_as: String,
    pub orig_file_as: String,
    pub company: String,
    pub job_title: String,
    pub department: String,
    pub email: [String; 3],
    pub phone_business: String,
    pub phone_mobile: String,
    pub phone_home: String,
    pub phone_fax: String,
    pub phone_other: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal: String,
    pub country: String,
    pub home: AddressFields,
    /// Addresses other than Business and Home, kept as they are.
    pub other_addresses: Vec<Value>,
    pub web: String,
    pub im: String,
    pub birthday: String,
    pub notes: String,
    /// Contact group members ("Name <a@b>; …").
    pub members: String,
    /// New E-mail Contact form (group): open, name, address.
    pub new_member: Option<(String, String)>,
    /// Add Members picker (group).
    pub picker: Option<MemberPicker>,
    pub categories: Vec<String>,
    pub error: Option<String>,
    pub nonce: u64,
    pub focus: bool,
}

/// Add Members › From Contacts / From Address Book.
#[derive(Clone, Default)]
pub(crate) struct MemberPicker {
    pub search: String,
    /// Picked display strings ("Name <a@b>").
    pub chosen: BTreeSet<String>,
    /// Include recently used addresses (Address Book) besides contacts.
    pub address_book: bool,
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
    pub reminder_date: String,
    pub reminder_min: i64,
    pub body: String,
    pub categories: Vec<String>,
    pub linked_mail: bool,
    pub error: Option<String>,
    pub nonce: u64,
    pub focus: bool,
}

#[derive(Clone, Default)]
pub(crate) struct NoteEditor {
    pub id: u64,
    pub text: String,
    pub color: NoteColor,
    pub categories: Vec<String>,
    pub modified: String,
    pub error: Option<String>,
    pub nonce: u64,
}

#[derive(Clone, Default)]
pub(crate) struct GoToEditor {
    pub date: String,
    pub mode: String,
    pub error: Option<String>,
    pub nonce: u64,
}

#[derive(Clone, Default)]
pub(crate) struct OccPrompt {
    pub id: u64,
    pub occurrence: i64,
    /// Open the whole series (else just this occurrence).
    pub series: bool,
    pub nonce: u64,
}

// ---------------------------------------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------------------------------------

fn attendee_list(a: &mailcraft_model::Appointment, role: AttendeeRole) -> String {
    a.attendees
        .iter()
        .filter(|x| x.role == role || (role == AttendeeRole::Required && x.role == AttendeeRole::Resource))
        .map(|x| x.mailbox.to_display_string())
        .collect::<Vec<_>>()
        .join("; ")
}

fn is_my_address(app: &App, addr: &str) -> bool {
    let a = addr.trim().to_ascii_lowercase();
    app.s.me.contains(&a) || app.s.mailbox.accounts.iter().any(|x| x.address.eq_ignore_ascii_case(&a))
}

/// Opens an existing appointment (the whole series when it recurs).
pub(crate) fn open_appointment(app: &App, st: &mut ModState, id: ItemId) {
    open_appointment_at(app, st, id, None);
}

/// Opens an appointment; with `occurrence` (unix start) only that occurrence of its series.
pub(crate) fn open_appointment_at(app: &App, st: &mut ModState, id: ItemId, occurrence: Option<(i64, i64)>) {
    let Some(a) = app.s.mailbox.appointments.get(&id) else { return };
    let tz = tz(app);
    let (start, end) = match occurrence {
        Some((s, e)) => {
            (mailcraft_model::DateTime::from_unix(s, tz), mailcraft_model::DateTime::from_unix(e, tz))
        }
        None => (a.start, a.end),
    };
    let s = local_secs(start, tz);
    let mut e = local_secs(end, tz).max(s);
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
        attendees: attendee_list(a, AttendeeRole::Required),
        optional: attendee_list(a, AttendeeRole::Optional),
        meeting: a.is_meeting,
        organizer_me: a.organizer.as_ref().is_none_or(|o| is_my_address(app, &o.addr)),
        occurrence: occurrence.map(|(s, _)| s),
        nonce,
        ..Default::default()
    });
}

/// Double-click on an occurrence: recurring appointments first ask "this occurrence or the series?".
pub(crate) fn open_occurrence(app: &App, st: &mut ModState, id: ItemId, occurrence: i64) {
    let recurring = app.s.mailbox.appointments.get(&id).is_some_and(|a| a.recurrence.is_some());
    if !recurring {
        open_appointment(app, st, id);
        return;
    }
    let nonce = st.editors.next();
    st.editors.occ_prompt = Some(OccPrompt { id: id.0, occurrence, series: false, nonce });
}

/// Opens one occurrence (by its start) for editing.
pub(crate) fn open_one_occurrence(app: &App, st: &mut ModState, id: ItemId, occurrence: i64) {
    let Some(a) = app.s.mailbox.appointments.get(&id) else { return };
    let tz = tz(app);
    let at = mailcraft_model::DateTime::from_unix(occurrence, tz);
    let found = mailcraft_engine::cmd::calendar::occurrences(
        a,
        at,
        mailcraft_model::DateTime::from_unix(occurrence + 1, tz),
        // Long events of a frequent series overlap: every occurrence running then is listed.
        usize::MAX,
    )
    .into_iter()
    .find(|(s, _)| s.unix == occurrence);
    match found {
        Some((s, e)) => open_appointment_at(app, st, id, Some((s.unix, e.unix))),
        None => open_appointment(app, st, id),
    }
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
        organizer_me: true,
        nonce,
        focus: true,
        ..Default::default()
    });
}

fn address_fields(a: Option<&PostalAddress>) -> AddressFields {
    match a {
        Some(a) => {
            [a.street.clone(), a.city.clone(), a.region.clone(), a.postal_code.clone(), a.country.clone()]
        }
        None => Default::default(),
    }
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
    let home = c.addresses.iter().find(|a| a.kind.eq_ignore_ascii_case("Home"));
    let other_addresses = c
        .addresses
        .iter()
        .filter(|a| !a.kind.eq_ignore_ascii_case("Business") && !a.kind.eq_ignore_ascii_case("Home"))
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
    let [street, city, region, postal, country] = address_fields(business);
    let nonce = st.editors.next();
    st.editors.contact = Some(ContactEditor {
        id: Some(id.0),
        folder: Some(c.folder.0),
        group,
        full_name: if group { c.file_as.clone() } else { full.clone() },
        orig_full_name: full,
        name_parts: [
            c.title.clone(),
            c.first_name.clone(),
            c.middle_name.clone(),
            c.last_name.clone(),
            c.suffix.clone(),
        ],
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
        phone_fax: phone("Business Fax"),
        phone_other: phone("Other"),
        street,
        city,
        region,
        postal,
        country,
        home: address_fields(home),
        other_addresses,
        web: c.web_page.clone(),
        im: c.im_address.clone(),
        birthday: c.birthday.clone().unwrap_or_default(),
        notes,
        members: if group { c.emails.join("; ") } else { String::new() },
        categories: c.categories.clone(),
        nonce,
        ..Default::default()
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
    let (reminder_date, reminder_min) = match t.reminder {
        Some(r) => {
            let s = local_secs(r, tz);
            (day_string(s.div_euclid(DAY)), s.rem_euclid(DAY) / 60)
        }
        None => (day_string(today(app) + 1), 8 * 60),
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
        reminder_date,
        reminder_min,
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
        reminder_date: day_string(today(app) + 1),
        reminder_min: 8 * 60,
        nonce,
        focus: true,
        ..Default::default()
    });
}

/// Opens a note in its own window.
pub(crate) fn open_note(app: &App, st: &mut ModState, id: ItemId) {
    let Some(n) = app.s.mailbox.notes.get(&id) else { return };
    let nonce = st.editors.next();
    st.editors.note = Some(NoteEditor {
        id: id.0,
        text: n.text.clone(),
        color: n.color,
        categories: n.categories.clone(),
        modified: mailcraft_model::ListTime::short_date_time(n.modified, tz(app)),
        error: None,
        nonce,
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
    occurrence_prompt(ctx, app, &mut st);
    appointment_window(ctx, app, &mut st);
    contact_window(ctx, app, &mut st);
    task_window(ctx, app, &mut st);
    note_window(ctx, app, &mut st);
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

/// Time picker in 15-minute steps (an off-grid time stays selectable).
fn time_combo(ui: &mut Ui, id: &str, minute: &mut i64) {
    ComboBox::from_id_salt(id).selected_text(clock(*minute)).width(100.0).show_ui(ui, |ui| {
        if minute.rem_euclid(15) != 0 {
            let m = *minute;
            ui.selectable_value(minute, m, clock(m));
        }
        for k in 0..96 {
            let m = k * 15;
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

/// What a window's button row asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Act {
    #[default]
    None,
    Save,
    /// Save and send (meeting invitation or update).
    SaveSend,
    Delete,
    Close,
    CancelMeeting,
    MarkComplete,
    ForwardCard,
    ForwardContact,
}

/// Standard buttons row (right-aligned: primary, Delete, Cancel). `primary` is the main button's label.
fn buttons(ui: &mut Ui, primary: &str, can_delete: bool, act: &mut Act) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui.button("Cancel").clicked() {
            *act = Act::Close;
        }
        if can_delete && ui.button("Delete").clicked() {
            *act = Act::Delete;
        }
        if ui.button(RichText::new(primary).strong()).clicked() {
            *act = if primary == "Save & Close" { Act::Save } else { Act::SaveSend };
        }
    });
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

/// (label, RRULE) quick choices; weekly repeats on the start's weekday.
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

pub(crate) fn split_list(s: &str) -> Vec<String> {
    s.split(';').map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned).collect()
}

fn occurrence_prompt(ctx: &egui::Context, app: &mut App, st: &mut ModState) {
    let Some(mut p) = st.editors.occ_prompt.take() else { return };
    let subject = app
        .s
        .mailbox
        .appointments
        .get(&ItemId(p.id))
        .map(|a| if a.subject.trim().is_empty() { "(No subject)".to_owned() } else { a.subject.clone() })
        .unwrap_or_default();
    let mut open = true;
    let mut choice: Option<bool> = None;
    let mut cancel = false;
    window("Open Recurring Item".into(), ("mc-occ-prompt", p.nonce))
        .default_width(340.0)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            crate::dialogs::form_visuals(ui, &app.theme);
            ui.label(format!("\"{subject}\" is one of a series. Open:"));
            ui.radio_value(&mut p.series, false, "Just this one");
            ui.radio_value(&mut p.series, true, "The entire series");
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                cancel = ui.button("Cancel").clicked();
                if ui.button(RichText::new("OK").strong()).clicked() {
                    choice = Some(p.series);
                }
            });
        });
    match choice {
        Some(true) => open_appointment(app, st, ItemId(p.id)),
        Some(false) => open_one_occurrence(app, st, ItemId(p.id), p.occurrence),
        None if open && !cancel => st.editors.occ_prompt = Some(p),
        None => {}
    }
}

fn appointment_window(ctx: &egui::Context, app: &mut App, st: &mut ModState) {
    let Some(mut ed) = st.editors.appt.take() else { return };
    let mut open = true;
    let meeting = ed.is_meeting();
    let kind = match (meeting, ed.occurrence.is_some()) {
        (true, true) => "Meeting Occurrence",
        (true, false) => "Meeting",
        (false, true) => "Appointment Occurrence",
        (false, false) => "Appointment",
    };
    let title =
        format!("{} - {kind}", if ed.subject.trim().is_empty() { "Untitled" } else { ed.subject.trim() });
    let mut act = Act::None;
    window(title, ("mc-appt-editor", ed.nonce)).open(&mut open).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &app.theme);
        // Pages.
        ui.horizontal(|ui| {
            ui.selectable_value(&mut ed.page, PAGE_APPOINTMENT, "Appointment");
            if meeting || ed.id.is_none() {
                ui.selectable_value(&mut ed.page, PAGE_SCHEDULING, "Scheduling Assistant");
            }
            if meeting && ed.id.is_some() && ed.organizer_me {
                ui.selectable_value(&mut ed.page, PAGE_TRACKING, "Tracking");
            }
        });
        ui.separator();
        match ed.page {
            PAGE_SCHEDULING => super::scheduling::show(ui, app, &mut ed),
            PAGE_TRACKING => tracking_page(ui, app, &ed),
            _ => appointment_page(ui, app, &mut ed),
        }
        error_line(ui, app, &ed.error);
        ui.separator();
        ui.horizontal(|ui| {
            if meeting
                && ed.id.is_some()
                && ed.organizer_me
                && ed.occurrence.is_none()
                && ui.button("Cancel Meeting").clicked()
            {
                act = Act::CancelMeeting;
            }
            let primary = match (meeting && ed.organizer_me, ed.id.is_some()) {
                (true, false) => "Send Invitation",
                (true, true) => "Send Update",
                _ => "Save & Close",
            };
            buttons(ui, primary, ed.id.is_some(), &mut act);
        });
    });
    // The recurrence dialog (over the editor).
    if let Some(mut d) = ed.recurrence_dlg.take() {
        match super::recurrence::show(ctx, app, &mut d, !ed.recurrence.trim().is_empty()) {
            Outcome::Keep => ed.recurrence_dlg = Some(d),
            Outcome::Cancel => {}
            Outcome::Remove => ed.recurrence.clear(),
            Outcome::Set(rule, date, start_min, end_min) => {
                let len_days =
                    parse_day(&ed.end_date).zip(parse_day(&ed.start_date)).map_or(0, |(e, s)| (e - s).max(0));
                ed.recurrence = rule;
                ed.all_day = d.all_day;
                ed.start_min = start_min;
                ed.end_min = end_min;
                if let Some(day) = parse_day(&date) {
                    // All-day events keep their length in days; timed ones end the same day (or the next,
                    // when the end time is before the start time).
                    let end_day = if d.all_day {
                        day + len_days
                    } else if end_min < start_min {
                        day + 1
                    } else {
                        day
                    };
                    ed.start_date = date;
                    ed.end_date = day_string(end_day);
                }
            }
        }
    }
    match act {
        Act::Save | Act::SaveSend => match save_appointment(app, &ed) {
            Ok(id) => {
                if let Some(id) = id {
                    app.ui.module_selection = Some(id);
                    // An occurrence's update goes out for its exception (RECURRENCE-ID), the rest for the series.
                    if act == Act::SaveSend && ed.id.is_some() && ed.is_meeting() {
                        app.run("calendar.sendUpdate", json!({ "id": id }));
                    }
                }
                return;
            }
            Err(e) => ed.error = Some(e),
        },
        Act::Delete => {
            if let Some(id) = ed.id {
                match ed.occurrence {
                    Some(occ) => {
                        app.run("calendar.deleteOccurrence", json!({ "id": id, "occurrence": occ }));
                    }
                    None => {
                        app.run("calendar.delete", json!({ "id": id }));
                        if app.ui.module_selection == Some(id) {
                            app.ui.module_selection = None;
                        }
                    }
                }
                return;
            }
        }
        Act::CancelMeeting => {
            if let Some(id) = ed.id {
                match run_checked(app, "calendar.cancelMeeting", json!({ "id": id, "send": "edit" })) {
                    Ok(_) => {
                        if app.ui.module_selection == Some(id) {
                            app.ui.module_selection = None;
                        }
                        return;
                    }
                    Err(e) => ed.error = Some(e),
                }
            }
        }
        Act::Close => return,
        _ => {}
    }
    if open {
        st.editors.appt = Some(ed);
    }
}

fn appointment_page(ui: &mut Ui, app: &mut App, ed: &mut ApptEditor) {
    let before = parse_day(&ed.start_date).map(|d| d * 24 * 60 + if ed.all_day { 0 } else { ed.start_min });
    egui::Grid::new("mc-appt-grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        let r = field(ui, "Title", &mut ed.subject, "Add a title");
        if ed.focus {
            r.request_focus();
            ed.focus = false;
        }
        if ed.is_meeting() || ed.id.is_none() {
            field(ui, "Required", &mut ed.attendees, "Name <address>; …  (invites are sent as a meeting)");
            field(ui, "Optional", &mut ed.optional, "Name <address>; …");
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
        if ed.occurrence.is_none() {
            ui.label("Repeat");
            ui.horizontal(|ui| {
                let choices = recurrence_choices(parse_day(&ed.start_date));
                let current = choices
                    .iter()
                    .find(|(_, r)| r.eq_ignore_ascii_case(&ed.recurrence))
                    .map(|(l, _)| l.clone())
                    .unwrap_or_else(|| "Custom".to_owned());
                ComboBox::from_id_salt("mc-appt-rec").selected_text(current).width(160.0).show_ui(ui, |ui| {
                    for (label, rule) in &choices {
                        ui.selectable_value(&mut ed.recurrence, rule.clone(), label);
                    }
                });
                if ui.button("Recurrence…").clicked() {
                    ed.recurrence_dlg = Some(RecurrenceDialog::new(
                        app,
                        &ed.start_date,
                        ed.start_min,
                        ed.end_min,
                        ed.all_day,
                        &ed.recurrence,
                        ed.nonce,
                    ));
                }
            });
            ui.end_row();
            if let Some(text) = pattern_text(&ed.recurrence) {
                ui.label("");
                ui.label(RichText::new(format!("Occurs {text}")).color(app.theme.text_muted));
                ui.end_row();
            }
        }
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
    ui.add(TextEdit::multiline(&mut ed.body).hint_text("Notes").desired_rows(5).desired_width(f32::INFINITY));
    // Moving the start keeps the duration.
    let after = parse_day(&ed.start_date).map(|d| d * 24 * 60 + if ed.all_day { 0 } else { ed.start_min });
    if let (Some(b), Some(a), Some(end_day)) = (before, after, parse_day(&ed.end_date))
        && a != b
    {
        let end = end_day * 24 * 60 + if ed.all_day { 0 } else { ed.end_min } + (a - b);
        ed.end_date = day_string(end.div_euclid(24 * 60));
        if !ed.all_day {
            ed.end_min = end.rem_euclid(24 * 60);
        }
    }
}

fn response_label(r: ResponseStatus) -> &'static str {
    match r {
        ResponseStatus::None => "None",
        ResponseStatus::Accepted => "Accepted",
        ResponseStatus::Tentative => "Tentative",
        ResponseStatus::Declined => "Declined",
    }
}

fn tracking_page(ui: &mut Ui, app: &App, ed: &ApptEditor) {
    let Some(a) = ed.id.and_then(|id| app.s.mailbox.appointments.get(&ItemId(id))) else { return };
    egui::Grid::new("mc-appt-tracking").num_columns(3).striped(true).spacing(vec2(16.0, 4.0)).show(
        ui,
        |ui| {
            ui.label(RichText::new("Name").strong());
            ui.label(RichText::new("Attendance").strong());
            ui.label(RichText::new("Response").strong());
            ui.end_row();
            if let Some(o) = &a.organizer {
                ui.label(o.to_display_string());
                ui.label("Meeting Organizer");
                ui.label("");
                ui.end_row();
            }
            for x in &a.attendees {
                ui.label(x.mailbox.to_display_string());
                ui.label(match x.role {
                    AttendeeRole::Required => "Required Attendee",
                    AttendeeRole::Optional => "Optional Attendee",
                    AttendeeRole::Resource => "Resource",
                });
                ui.label(response_label(x.response));
                ui.end_row();
            }
        },
    );
    let n = |r: ResponseStatus| a.attendees.iter().filter(|x| x.response == r).count();
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "{} accepted, {} tentatively accepted, {} declined, {} not responded.",
            n(ResponseStatus::Accepted),
            n(ResponseStatus::Tentative),
            n(ResponseStatus::Declined),
            n(ResponseStatus::None)
        ))
        .color(app.theme.text_muted),
    );
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
    let optional = split_list(&ed.optional);
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
    });
    let Some(o) = p.as_object_mut() else { return Err("internal error".into()) };
    let meeting = ed.meeting || !attendees.is_empty() || !optional.is_empty();
    if meeting {
        o.insert("attendees".into(), json!(attendees));
        o.insert("optional".into(), json!(optional));
    }
    if let (Some(id), Some(occ)) = (ed.id, ed.occurrence) {
        o.insert("id".into(), json!(id));
        o.insert("occurrence".into(), json!(occ));
        let out = run_checked(app, "calendar.editOccurrence", p)?;
        return Ok(out.get("id").and_then(Value::as_u64));
    }
    let rule = if ed.recurrence.trim().is_empty() { Value::Null } else { json!(ed.recurrence) };
    o.insert("recurrence".into(), rule);
    if let Some(id) = ed.id {
        o.insert("id".into(), json!(id));
        run_checked(app, "calendar.update", p)?;
        Ok(Some(id))
    } else {
        if let Some(f) = ed.folder {
            o.insert("folder".into(), json!(f));
        }
        let cmd = if meeting { "calendar.newMeeting" } else { "calendar.newAppointment" };
        let out = run_checked(app, cmd, p)?;
        Ok(out.get("id").and_then(Value::as_u64))
    }
}

// ---------------------------------------------------------------------------------------------------------
// Contacts and contact groups
// ---------------------------------------------------------------------------------------------------------

/// The contact the editor would save (for the business card preview).
fn preview_contact(ed: &ContactEditor) -> Contact {
    let mut c = Contact {
        first_name: ed.full_name.trim().to_owned(),
        file_as: ed.file_as.clone(),
        company: ed.company.clone(),
        job_title: ed.job_title.clone(),
        department: ed.department.clone(),
        emails: ed.email.iter().filter(|e| !e.trim().is_empty()).cloned().collect(),
        web_page: ed.web.clone(),
        ..Default::default()
    };
    for (kind, n) in [
        ("Business", &ed.phone_business),
        ("Mobile", &ed.phone_mobile),
        ("Home", &ed.phone_home),
        ("Business Fax", &ed.phone_fax),
        ("Other", &ed.phone_other),
    ] {
        if !n.trim().is_empty() {
            c.phones.push((kind.into(), n.trim().into()));
        }
    }
    let business = [&ed.street, &ed.city, &ed.region, &ed.postal, &ed.country];
    if business.iter().any(|x| !x.trim().is_empty()) {
        c.addresses.push(PostalAddress {
            kind: "Business".into(),
            street: ed.street.clone(),
            city: ed.city.clone(),
            region: ed.region.clone(),
            postal_code: ed.postal.clone(),
            country: ed.country.clone(),
        });
    }
    c
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

/// Paints a business card (photo placeholder with initials, then the card lines).
pub(crate) fn business_card(ui: &mut Ui, app: &App, c: &Contact, width: f32) {
    let lines = business_card_lines(c);
    egui::Frame::new()
        .fill(app.theme.reading)
        .stroke(Stroke::new(1.0, app.theme.separator))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_width(width);
            ui.horizontal_top(|ui| {
                let (r, resp) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::hover());
                super::common::a11y(&resp, egui::WidgetType::Image, "Contact picture");
                let name = lines.first().cloned().unwrap_or_default();
                let idx = name.bytes().fold(0u8, |a, b| a.wrapping_add(b));
                ui.painter().circle_filled(r.center(), 24.0, persona_fill(idx));
                ui.painter().text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    initials(&name),
                    egui::FontId::proportional(17.0),
                    Color32::WHITE,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for (i, l) in lines.iter().enumerate() {
                        if i == 0 {
                            ui.label(RichText::new(l).strong().size(14.0).color(app.theme.text));
                        } else {
                            ui.label(RichText::new(l).size(12.0).color(app.theme.text));
                        }
                    }
                });
            });
        });
}

fn contact_window(ctx: &egui::Context, app: &mut App, st: &mut ModState) {
    let Some(mut ed) = st.editors.contact.take() else { return };
    let mut open = true;
    let kind = if ed.group { "Contact Group" } else { "Contact" };
    let title =
        format!("{} - {kind}", if ed.full_name.trim().is_empty() { "Untitled" } else { ed.full_name.trim() });
    let mut act = Act::None;
    window(title, ("mc-contact-editor", ed.nonce))
        .default_width(if ed.group { 560.0 } else { 760.0 })
        .open(&mut open)
        .show(ctx, |ui| {
            crate::dialogs::form_visuals(ui, &app.theme);
            if ed.group {
                group_page(ui, app, &mut ed);
            } else {
                contact_page(ui, app, &mut ed);
            }
            error_line(ui, app, &ed.error);
            ui.separator();
            ui.horizontal(|ui| {
                if !ed.group && ed.id.is_some() {
                    ui.menu_button("Forward", |ui| {
                        if ui.button("As a Business Card").clicked() {
                            act = Act::ForwardCard;
                            ui.close();
                        }
                        if ui.button("As an Outlook Contact").clicked() {
                            act = Act::ForwardContact;
                            ui.close();
                        }
                    });
                }
                buttons(ui, "Save & Close", ed.id.is_some(), &mut act);
            });
        });
    if ed.group {
        member_picker(ctx, app, &mut ed);
    }
    match act {
        Act::Save | Act::SaveSend => match save_contact(app, &ed) {
            Ok(id) => {
                if let Some(id) = id {
                    app.ui.module_selection = Some(id);
                }
                return;
            }
            Err(e) => ed.error = Some(e),
        },
        Act::Delete => {
            if let Some(id) = ed.id {
                app.run("people.delete", json!({ "ids": [id] }));
                if app.ui.module_selection == Some(id) {
                    app.ui.module_selection = None;
                }
                return;
            }
        }
        Act::ForwardCard | Act::ForwardContact => {
            if let Some(id) = ed.id {
                let as_kind = if act == Act::ForwardCard { "businessCard" } else { "contact" };
                if let Err(e) = run_checked(app, "people.forward", json!({ "ids": [id], "as": as_kind })) {
                    ed.error = Some(e);
                }
            }
        }
        Act::Close => return,
        _ => {}
    }
    if open {
        st.editors.contact = Some(ed);
    }
}

fn address_grid(ui: &mut Ui, id: &str, [street, city, region, postal, country]: [&mut String; 5]) {
    egui::Grid::new(id).num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
        field(ui, "Street", street, "");
        field(ui, "City", city, "");
        field(ui, "State/Province", region, "");
        field(ui, "Postal code", postal, "");
        field(ui, "Country/Region", country, "");
    });
}

fn contact_page(ui: &mut Ui, app: &App, ed: &mut ContactEditor) {
    let preview = preview_contact(ed);
    let wide = ui.available_width() > 640.0;
    ui.horizontal_top(|ui| {
        let left_w = if wide { ui.available_width() - 280.0 } else { ui.available_width() };
        ui.vertical(|ui| {
            ui.set_width(left_w);
            ScrollArea::vertical().id_salt("mc-contact-scroll").max_height(520.0).show(ui, |ui| {
                egui::Grid::new("mc-contact-grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                    ui.label("Full name");
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            TextEdit::singleline(&mut ed.full_name)
                                .desired_width((left_w - 220.0).max(120.0)),
                        );
                        if r.changed() {
                            ed.parts_edited = false;
                        }
                        if ed.focus {
                            r.request_focus();
                            ed.focus = false;
                        }
                        if ui.button("Name details…").clicked() {
                            ed.parts_open = !ed.parts_open;
                        }
                    });
                    ui.end_row();
                    if ed.parts_open {
                        let [t, f, m, l, s] = &mut ed.name_parts;
                        let mut changed = false;
                        for (label, v) in
                            [("Title", t), ("First", f), ("Middle", m), ("Last", l), ("Suffix", s)]
                        {
                            ui.label(label);
                            changed |= ui.add(TextEdit::singleline(v).desired_width(f32::INFINITY)).changed();
                            ui.end_row();
                        }
                        if changed {
                            ed.parts_edited = true;
                            ed.full_name = ed
                                .name_parts
                                .iter()
                                .map(|x| x.trim())
                                .filter(|x| !x.is_empty())
                                .collect::<Vec<_>>()
                                .join(" ");
                        }
                    }
                    field(ui, "Company", &mut ed.company, "");
                    field(ui, "Job title", &mut ed.job_title, "");
                    field(ui, "Department", &mut ed.department, "");
                    field(ui, "File as", &mut ed.file_as, "Last, First (automatic when empty)");
                    let [e1, e2, e3] = &mut ed.email;
                    field(ui, "Email", e1, "name@example.com");
                    field(ui, "Email 2", e2, "");
                    field(ui, "Email 3", e3, "");
                    field(ui, "Web page", &mut ed.web, "");
                    field(ui, "IM address", &mut ed.im, "");
                    field(ui, "Business phone", &mut ed.phone_business, "");
                    field(ui, "Home phone", &mut ed.phone_home, "");
                    field(ui, "Business fax", &mut ed.phone_fax, "");
                    field(ui, "Mobile phone", &mut ed.phone_mobile, "");
                    field(ui, "Other phone", &mut ed.phone_other, "");
                    ui.label("Birthday");
                    date_field(ui, &mut ed.birthday, "mc-contact-bday");
                    ui.end_row();
                    ui.label("Categories");
                    categories_menu(ui, app, &mut ed.categories);
                    ui.end_row();
                });
                egui::CollapsingHeader::new("Business address")
                    .id_salt("mc-contact-baddr")
                    .default_open(true)
                    .show(ui, |ui| {
                        address_grid(
                            ui,
                            "mc-contact-business",
                            [&mut ed.street, &mut ed.city, &mut ed.region, &mut ed.postal, &mut ed.country],
                        );
                    });
                egui::CollapsingHeader::new("Home address").id_salt("mc-contact-haddr").show(ui, |ui| {
                    let [a, b, c, d, e] = &mut ed.home;
                    address_grid(ui, "mc-contact-home", [a, b, c, d, e]);
                });
                ui.add_space(4.0);
                ui.add(
                    TextEdit::multiline(&mut ed.notes)
                        .hint_text("Notes")
                        .desired_rows(4)
                        .desired_width(f32::INFINITY),
                );
            });
        });
        if wide {
            ui.vertical(|ui| {
                ui.label(RichText::new("Business card").color(app.theme.text_muted));
                business_card(ui, app, &preview, 250.0);
            });
        }
    });
}

fn group_page(ui: &mut Ui, app: &App, ed: &mut ContactEditor) {
    egui::Grid::new("mc-group-grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        let r = field(ui, "Name", &mut ed.full_name, "");
        if ed.focus {
            r.request_focus();
            ed.focus = false;
        }
        ui.label("Categories");
        categories_menu(ui, app, &mut ed.categories);
        ui.end_row();
    });
    ui.horizontal(|ui| {
        ui.menu_button("Add Members", |ui| {
            if ui.button("From Contacts…").clicked() {
                ed.picker = Some(MemberPicker::default());
                ui.close();
            }
            if ui.button("From Address Book…").clicked() {
                ed.picker = Some(MemberPicker { address_book: true, ..Default::default() });
                ui.close();
            }
            if ui.button("New Email Contact…").clicked() {
                ed.new_member = Some((String::new(), String::new()));
                ui.close();
            }
        });
        let n = split_list(&ed.members).len();
        ui.label(
            RichText::new(format!("{n} member{}", if n == 1 { "" } else { "s" })).color(app.theme.text_muted),
        );
    });
    if let Some((name, addr)) = ed.new_member.as_mut() {
        let mut add = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(name).hint_text("Display name").desired_width(160.0));
            ui.add(TextEdit::singleline(addr).hint_text("name@example.com").desired_width(200.0));
            add = ui.button("Add").clicked();
            cancel = ui.button("Cancel").clicked();
        });
        if add {
            let a = addr.trim();
            if a.contains('@') && !a.contains([';', '<', '>']) {
                let n = name.trim().replace(['<', '>', ';', '"'], "");
                let entry = if n.is_empty() { a.to_owned() } else { format!("{n} <{a}>") };
                add_member(ed, &entry);
                ed.new_member = None;
            } else {
                ed.error = Some("Enter an email address such as name@example.com.".into());
            }
        } else if cancel {
            ed.new_member = None;
        }
    }
    // Members table.
    let members = split_list(&ed.members);
    let mut remove: Option<usize> = None;
    egui::Frame::new().stroke(Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(ui, |ui| {
        ScrollArea::vertical().id_salt("mc-group-members").max_height(220.0).min_scrolled_height(120.0).show(
            ui,
            |ui| {
                egui::Grid::new("mc-group-table").num_columns(3).striped(true).spacing(vec2(16.0, 4.0)).show(
                    ui,
                    |ui| {
                        ui.label(RichText::new("Name").strong());
                        ui.label(RichText::new("Email").strong());
                        ui.label("");
                        ui.end_row();
                        for (i, m) in members.iter().enumerate() {
                            let parsed = mailcraft_mime::parse_address_list(m)
                                .first()
                                .and_then(|a| a.mailboxes().into_iter().next());
                            let (name, addr) = match &parsed {
                                Some(mb) => (mb.display().to_owned(), mb.addr.clone()),
                                None => (m.clone(), String::new()),
                            };
                            ui.label(name);
                            ui.label(addr);
                            if ui.small_button("Remove").on_hover_text("Remove Member").clicked() {
                                remove = Some(i);
                            }
                            ui.end_row();
                        }
                    },
                );
                if members.is_empty() {
                    ui.label(RichText::new("No members yet. Use Add Members.").color(app.theme.text_muted));
                }
            },
        );
    });
    if let Some(i) = remove {
        let mut list = members;
        if i < list.len() {
            list.remove(i);
        }
        ed.members = list.join("; ");
    }
    ui.add_space(4.0);
    ui.add(
        TextEdit::multiline(&mut ed.notes).hint_text("Notes").desired_rows(3).desired_width(f32::INFINITY),
    );
}

/// Adds a member string unless its address is already in the group.
fn add_member(ed: &mut ContactEditor, entry: &str) {
    let addr_of = |s: &str| {
        mailcraft_mime::parse_address_list(s)
            .first()
            .and_then(|a| a.mailboxes().into_iter().next())
            .map(|m| m.addr.to_ascii_lowercase())
    };
    let mut list = split_list(&ed.members);
    let new = addr_of(entry);
    if new.is_some() && list.iter().any(|m| addr_of(m) == new) {
        return;
    }
    list.push(entry.to_owned());
    ed.members = list.join("; ");
}

/// Candidates for Add Members: contacts (and, for the address book, recent recipients) as "Name <a@b>".
fn member_candidates(app: &App, address_book: bool, search: &str) -> Vec<String> {
    let needle = search.trim().to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut contacts: Vec<&Contact> = app.s.mailbox.contacts.values().filter(|c| !is_group(c)).collect();
    contacts.sort_by_key(|c| display_name(c).to_lowercase());
    for c in contacts {
        for a in contact_addresses(c) {
            out.push(a);
        }
    }
    if address_book {
        for r in &app.s.mailbox.recent_recipients {
            let entry = if r.name.trim().is_empty() {
                r.address.clone()
            } else {
                format!("{} <{}>", r.name.trim(), r.address)
            };
            if !out.iter().any(|o| o.to_lowercase().contains(&r.address.to_lowercase())) {
                out.push(entry);
            }
        }
    }
    out.retain(|x| needle.is_empty() || x.to_lowercase().contains(&needle));
    out.truncate(500);
    out
}

fn member_picker(ctx: &egui::Context, app: &App, ed: &mut ContactEditor) {
    let Some(mut p) = ed.picker.take() else { return };
    let mut open = true;
    let mut done = false;
    let title = if p.address_book { "Select Members: Address Book" } else { "Select Members: Contacts" };
    egui::Window::new(title)
        .id(Id::new(("mc-member-picker", ed.nonce)))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .default_width(420.0)
        .open(&mut open)
        .show(ctx, |ui| {
            crate::dialogs::form_visuals(ui, &app.theme);
            ui.add(TextEdit::singleline(&mut p.search).hint_text("Search").desired_width(f32::INFINITY));
            let list = member_candidates(app, p.address_book, &p.search);
            ScrollArea::vertical()
                .id_salt("mc-member-list")
                .max_height(260.0)
                .min_scrolled_height(200.0)
                .show(ui, |ui| {
                    for entry in &list {
                        let mut on = p.chosen.contains(entry);
                        if ui.checkbox(&mut on, entry.as_str()).changed() {
                            if on {
                                p.chosen.insert(entry.clone());
                            } else {
                                p.chosen.remove(entry);
                            }
                        }
                    }
                    if list.is_empty() {
                        ui.label(RichText::new("Nothing matches.").color(app.theme.text_muted));
                    }
                });
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Cancel").clicked() {
                    p.chosen.clear();
                    done = true;
                }
                if ui
                    .add_enabled(!p.chosen.is_empty(), egui::Button::new(RichText::new("OK").strong()))
                    .clicked()
                {
                    done = true;
                }
            });
        });
    if done {
        for entry in std::mem::take(&mut p.chosen) {
            add_member(ed, &entry);
        }
        return;
    }
    if open {
        ed.picker = Some(p);
    }
}

fn address_json(kind: &str, f: [&String; 5]) -> Option<Value> {
    if f.iter().all(|x| x.trim().is_empty()) {
        return None;
    }
    let [street, city, region, postal, country] = f;
    Some(json!({ "kind": kind, "street": street.trim(), "city": city.trim(), "region": region.trim(),
                 "postalCode": postal.trim(), "country": country.trim() }))
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
        if ed.parts_edited {
            let [t, f, m, l, s] = &ed.name_parts;
            for (k, v) in [("title", t), ("firstName", f), ("middleName", m), ("lastName", l), ("suffix", s)]
            {
                p.insert(k.into(), json!(v.trim()));
            }
        } else if ed.id.is_none() || ed.full_name.trim() != ed.orig_full_name.trim() {
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
            json!({ "Business": ed.phone_business.trim(), "Mobile": ed.phone_mobile.trim(), "Home": ed.phone_home.trim(),
                    "Business Fax": ed.phone_fax.trim(), "Other": ed.phone_other.trim() }),
        );
        let mut addresses: Vec<Value> = Vec::new();
        addresses
            .extend(address_json("Business", [&ed.street, &ed.city, &ed.region, &ed.postal, &ed.country]));
        let [a, b, c, d, e] = &ed.home;
        addresses.extend(address_json("Home", [a, b, c, d, e]));
        addresses.extend(ed.other_addresses.iter().cloned());
        p.insert("addresses".into(), Value::Array(addresses));
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

// ---------------------------------------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------------------------------------

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
    let mut act = Act::None;
    let now = today(app);
    window(title, ("mc-task-editor", ed.nonce)).open(&mut open).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &app.theme);
        if let Some(due) = parse_day(&ed.due)
            && ed.status != "completed"
        {
            let text = match due - now {
                d if d < 0 => format!("Overdue by {} day{}.", -d, if d == -1 { "" } else { "s" }),
                0 => "Due today.".to_owned(),
                d => format!("Due in {d} day{}.", if d == 1 { "" } else { "s" }),
            };
            ui.label(RichText::new(text).color(if due < now {
                app.theme.danger
            } else {
                app.theme.text_muted
            }));
        }
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
                ui.add_enabled_ui(ed.reminder_on, |ui| {
                    date_field(ui, &mut ed.reminder_date, "mc-task-rdate");
                    time_combo(ui, "mc-task-rtime", &mut ed.reminder_min);
                });
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
        ui.horizontal(|ui| {
            if ed.status != "completed" && ui.button("Mark Complete").clicked() {
                act = Act::MarkComplete;
            }
            buttons(ui, "Save & Close", ed.id.is_some(), &mut act);
        });
    });
    if act == Act::MarkComplete {
        ed.status = "completed".into();
        ed.percent = 100;
        act = Act::Save;
    }
    match act {
        Act::Save | Act::SaveSend => match save_task(app, &ed) {
            Ok(id) => {
                if let Some(id) = id {
                    app.ui.module_selection = Some(id);
                }
                return;
            }
            Err(e) => ed.error = Some(e),
        },
        Act::Delete => {
            if let Some(id) = ed.id {
                app.run("tasks.delete", json!({ "id": id }));
                if app.ui.module_selection == Some(id) {
                    app.ui.module_selection = None;
                }
                return;
            }
        }
        Act::Close => return,
        _ => {}
    }
    if open {
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
        let d = parse_day(&ed.reminder_date).ok_or("Enter the reminder date as YYYY-MM-DD.")?;
        json!(day_time_string(d, ed.reminder_min))
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

// ---------------------------------------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------------------------------------

fn note_window(ctx: &egui::Context, app: &mut App, st: &mut ModState) {
    let Some(mut ed) = st.editors.note.take() else { return };
    if !app.s.mailbox.notes.contains_key(&ItemId(ed.id)) {
        return;
    }
    let mut open = true;
    let mut act = Act::None;
    let title = ed.text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Note").to_owned();
    window(title, ("mc-note-editor", ed.nonce)).default_width(320.0).open(&mut open).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &app.theme);
        ui.horizontal(|ui| {
            for (color, _, label) in super::notes_view::COLORS {
                let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                super::common::a11y(&resp, egui::WidgetType::RadioButton, &format!("{label} note"));
                ui.painter().rect_filled(r, CornerRadius::same(3), note_fill(color, app.theme.dark));
                let stroke = if ed.color == color {
                    Stroke::new(2.0, app.theme.accent)
                } else {
                    Stroke::new(1.0, app.theme.separator)
                };
                ui.painter().rect_stroke(r, CornerRadius::same(3), stroke, egui::StrokeKind::Inside);
                if resp.on_hover_text(label).clicked() {
                    ed.color = color;
                }
            }
            ui.separator();
            ui.label("Categories");
            categories_menu(ui, app, &mut ed.categories);
        });
        egui::Frame::new().fill(note_fill(ed.color, app.theme.dark)).inner_margin(8.0).show(ui, |ui| {
            ui.add(
                TextEdit::multiline(&mut ed.text)
                    .frame(egui::Frame::NONE)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .hint_text("Type your note"),
            );
        });
        ui.label(RichText::new(format!("Modified {}", ed.modified)).size(11.0).color(app.theme.text_muted));
        error_line(ui, app, &ed.error);
        ui.separator();
        buttons(ui, "Save & Close", true, &mut act);
    });
    let color_key = super::notes_view::COLORS
        .iter()
        .find(|(c, _, _)| *c == ed.color)
        .map(|(_, k, _)| *k)
        .unwrap_or("yellow");
    match act {
        Act::Save | Act::SaveSend => {
            let p = json!({ "id": ed.id, "text": ed.text, "color": color_key, "categories": ed.categories });
            match run_checked(app, "notes.update", p) {
                Ok(_) => return,
                Err(e) => ed.error = Some(e),
            }
        }
        Act::Delete => {
            app.run("notes.delete", json!({ "id": ed.id }));
            if app.ui.module_selection == Some(ed.id) {
                app.ui.module_selection = None;
            }
            return;
        }
        Act::Close => return,
        _ => {}
    }
    if open {
        st.editors.note = Some(ed);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Go to Date
// ---------------------------------------------------------------------------------------------------------

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
    fn meeting_with_optional_attendees_and_tracking() {
        let mut app = app();
        let mut st = ModState::default();
        new_appointment(&app, &mut st, None, parse_day("2026-10-13").unwrap(), 15 * 60, 60, false);
        let mut ed = st.editors.appt.clone().unwrap();
        ed.subject = "Review".into();
        ed.attendees = "Ann Lee <ann@example.com>".into();
        ed.optional = "bob@example.org".into();
        let id = save_appointment(&mut app, &ed).unwrap().unwrap();
        let a = &app.s.mailbox.appointments[&ItemId(id)];
        assert!(a.is_meeting);
        assert_eq!(a.attendees.iter().filter(|x| x.role == AttendeeRole::Optional).count(), 1);
        assert!(!app.s.composes.is_empty(), "the invitation opens");
        open_appointment(&app, &mut st, ItemId(id));
        let ed = st.editors.appt.clone().unwrap();
        assert_eq!(ed.optional, "bob@example.org");
        assert!(ed.organizer_me);
    }

    #[test]
    fn occurrence_editing_makes_an_exception() {
        let mut app = app();
        let mut st = ModState::default();
        // The sample stand-up repeats every weekday at 9:00.
        let standup = app.s.mailbox.appointments.values().find(|a| a.subject == "Team stand-up").unwrap().id;
        let monday = mailcraft_engine::cmd::calendar::pim::parse_local("2026-10-12T09:00", 0).unwrap().unix;
        open_occurrence(&app, &mut st, standup, monday);
        assert!(st.editors.occ_prompt.is_some(), "a recurring item asks first");
        open_one_occurrence(&app, &mut st, standup, monday);
        let mut ed = st.editors.appt.clone().unwrap();
        assert_eq!(ed.occurrence, Some(monday));
        assert_eq!(ed.start_date, "2026-10-12");
        ed.start_min = 10 * 60;
        ed.end_min = 10 * 60 + 15;
        let ex = save_appointment(&mut app, &ed).unwrap().unwrap();
        assert_ne!(ex, standup.0);
        assert!(
            app.s.mailbox.appointments[&standup]
                .recurrence
                .as_ref()
                .unwrap()
                .exdates
                .iter()
                .any(|x| x.unix == monday)
        );
        // Not an occurrence: falls back to the series.
        open_one_occurrence(&app, &mut st, standup, monday + 60);
        assert_eq!(st.editors.appt.as_ref().unwrap().occurrence, None);
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
        assert_eq!(ed.name_parts[1], "Hana");
        ed.city.clear();
        ed.job_title = "Writer".into();
        ed.home = ["1 Elm St".into(), "Shelbyville".into(), String::new(), String::new(), String::new()];
        ed.phone_fax = "+1 555 0198".into();
        save_contact(&mut app, &ed).unwrap();
        let c = &app.s.mailbox.contacts[&ItemId(id)];
        assert_eq!(c.addresses.len(), 1);
        assert_eq!(c.addresses[0].kind, "Home");
        assert_eq!(c.job_title, "Writer");
        assert!(c.phones.contains(&("Mobile".to_string(), "+1 555 0199".to_string())));
        assert!(c.phones.contains(&("Business Fax".to_string(), "+1 555 0198".to_string())));
        // Name details.
        open_contact(&app, &mut st, ItemId(id));
        let mut ed = st.editors.contact.clone().unwrap();
        ed.name_parts = ["Dr.".into(), "Hana".into(), "M".into(), "Ito".into(), "PhD".into()];
        ed.parts_edited = true;
        save_contact(&mut app, &ed).unwrap();
        let c = &app.s.mailbox.contacts[&ItemId(id)];
        assert_eq!(c.full_name(), "Dr. Hana M Ito PhD");
        // Groups.
        new_contact(&app, &mut st, None, true);
        let mut g = st.editors.contact.clone().unwrap();
        g.full_name = "Team".into();
        g.members = "a@example.com; Bob <b@example.com>".into();
        add_member(&mut g, "B <B@example.com>");
        assert_eq!(split_list(&g.members).len(), 2, "same address not added twice");
        add_member(&mut g, "Hana Ito <hana@example.com>");
        let gid = save_contact(&mut app, &g).unwrap().unwrap();
        let group = &app.s.mailbox.contacts[&ItemId(gid)];
        assert!(is_group(group));
        assert_eq!(group.emails.len(), 3);
    }

    #[test]
    fn member_candidates_search_contacts_and_recent() {
        let mut app = app();
        app.s.mailbox.recent_recipients.push(mailcraft_model::RecentRecipient {
            name: "Zed".into(),
            address: "zed@example.net".into(),
            ..Default::default()
        });
        let all = member_candidates(&app, false, "");
        assert!(all.iter().any(|x| x.contains("Alice Rivera")));
        assert!(!all.iter().any(|x| x.contains("zed@")));
        assert!(member_candidates(&app, true, "zed").iter().any(|x| x == "Zed <zed@example.net>"));
        assert_eq!(member_candidates(&app, false, "rivera").len(), 1);
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
        ed.reminder_on = true;
        ed.reminder_min = 9 * 60 + 45;
        let id = save_task(&mut app, &ed).unwrap().unwrap();
        let t = &app.s.mailbox.tasks[&ItemId(id)];
        assert_eq!(t.priority, TaskPriority::High);
        assert!(t.due.is_some());
        assert_eq!(t.reminder.unwrap().unix % 86_400, (9 * 60 + 45) * 60);
        open_task(&app, &mut st, ItemId(id));
        let mut ed = st.editors.task.clone().unwrap();
        assert_eq!(ed.reminder_min, 9 * 60 + 45);
        ed.status = "completed".into();
        save_task(&mut app, &ed).unwrap();
        assert_eq!(app.s.mailbox.tasks[&ItemId(id)].status, TaskStatus::Completed);
        ed.due = "soon".into();
        assert!(save_task(&mut app, &ed).is_err());
    }

    #[test]
    fn preview_card_has_the_fields() {
        let ed = ContactEditor {
            full_name: "Ann Lee".into(),
            company: "Example Co.".into(),
            phone_mobile: "+1 555 0100".into(),
            email: ["ann@example.com".into(), String::new(), String::new()],
            city: "Springfield".into(),
            ..Default::default()
        };
        let lines = business_card_lines(&preview_contact(&ed));
        assert_eq!(lines[0], "Ann Lee");
        assert!(lines.contains(&"+1 555 0100 Mobile".to_string()));
        assert!(lines.contains(&"Springfield".to_string()));
        assert_eq!(initials("Ann Lee"), "AL");
        assert_eq!(initials(""), "");
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
