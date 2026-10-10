//! Rendering tests for the module views (kittest) over the sample mailbox (now = Fri 9 Oct 2026 2:30 PM UTC).

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mailcraft_engine::sample::sample_session;
use mailcraft_model::{ItemId, TaskStatus};
use serde_json::json;

use crate::App;

type View = fn(&mut egui::Ui, &mut App);

fn harness(view: View, app: App, size: (f32, f32)) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(vec2(size.0, size.1))
        .build_ui_state(move |ui: &mut egui::Ui, app: &mut App| view(ui, app), app);
    h.run_ok();
    h
}

fn app() -> App {
    App::new(sample_session())
}

fn has(h: &Harness<'_, App>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

fn click(h: &Harness<'_, App>, label: &str) {
    match h.query_all_by_label(label).next() {
        Some(node) => node.click(),
        None => panic!("no node labelled {label:?}"),
    }
}

fn has_part(h: &Harness<'_, App>, label: &str) -> bool {
    h.query_all_by_label_contains(label).next().is_some()
}

#[test]
fn calendar_month_shows_appointments() {
    let h = harness(super::calendar, app(), (1400.0, 900.0));
    assert!(has(&h, "October 2026"));
    assert!(has(&h, "My Calendars"));
    assert!(has_part(&h, "Team stand-up"));
    assert!(has_part(&h, "Lunch with Frank"));
    assert!(has_part(&h, "Company holiday"));
    assert_eq!(h.state().ui.calendar_view, "month");
}

#[test]
fn calendar_views_and_navigation() {
    let mut a = app();
    a.ui.calendar_view = "week".into();
    let mut h = harness(super::calendar, a, (1400.0, 900.0));
    assert!(has(&h, "October 4 – 10, 2026"));
    assert!(has(&h, "Team stand-up, Friday, October 9, 2026 9:00 AM"));
    h.state_mut().ui.calendar_view = "workWeek".into();
    h.run_ok();
    assert!(has(&h, "October 5 – 9, 2026"));
    h.state_mut().ui.calendar_view = "day".into();
    h.run_ok();
    assert!(has(&h, "Friday, October 9, 2026"));
    assert!(has(&h, "Lunch with Frank, Friday, October 9, 2026 12:00 PM"));
    click(&h, "Next");
    h.run_ok();
    assert!(has(&h, "Saturday, October 10, 2026"));
    click(&h, "Today");
    h.run_ok();
    assert!(has(&h, "Friday, October 9, 2026"));
    h.state_mut().ui.calendar_view = "schedule".into();
    h.run_ok();
    assert!(has_part(&h, "Team stand-up"));
    h.state_mut().ui.calendar_view = "month".into();
    h.run_ok();
    click(&h, "Next");
    h.run_ok();
    assert!(has(&h, "November 2026"));
    // Date navigator: clicking a day selects it.
    click(&h, "Thursday, November 12, 2026");
    h.run_ok();
    let st = super::common::load(&h.ctx);
    assert_eq!(st.cal.date, "2026-11-12");
}

#[test]
fn calendar_open_selected_appointment_and_save() {
    let mut a = app();
    a.ui.calendar_view = "day".into();
    let mut h = harness(super::calendar, a, (1400.0, 900.0));
    let label = "Lunch with Frank, Friday, October 9, 2026 12:00 PM";
    click(&h, label);
    h.run_ok();
    let id = h.state().ui.module_selection.unwrap();
    assert!(h.state().s.mailbox.appointments.contains_key(&ItemId(id)));
    // Enter opens the selected appointment (double-click does the same in the app).
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert!(has(&h, "Save & Close"));
    click(&h, "Save & Close");
    h.run_ok();
    assert!(!has(&h, "Save & Close"));
    assert!(h.state().s.mailbox.appointments.contains_key(&ItemId(id)));
}

#[test]
fn calendar_handles_command_dialogs() {
    let mut a = app();
    a.run("calendar.newAppointment", json!({}));
    // The shell forwards notices into `ui.dialogs`; do the same here.
    for n in std::mem::take(&mut a.s.notices) {
        if let mailcraft_engine::Notice::OpenDialog(name, p) = n {
            a.ui.dialogs.push((name, p));
        }
    }
    let h = harness(super::calendar, a, (1400.0, 900.0));
    assert!(has(&h, "Save & Close"));
    assert!(h.state().ui.dialogs.is_empty());
}

#[test]
fn people_list_card_and_details() {
    let mut h = harness(super::people, app(), (1400.0, 900.0));
    assert!(has(&h, "My Contacts"));
    assert!(has(&h, "Rivera, Alice"));
    assert!(has(&h, "Patel, Dev"));
    // The first contact is selected on entry, so its card shows.
    let first = h.state().ui.module_selection.expect("first contact selected");
    assert_eq!(h.state().s.mailbox.contacts[&ItemId(first)].last_name, "Chen");
    assert!(has(&h, "Meeting"));
    click(&h, "Rivera, Alice");
    h.run_ok();
    assert!(has(&h, "Meeting"));
    click(&h, "Jump to p");
    h.run_ok();
    let sel = h.state().ui.module_selection.unwrap();
    assert_eq!(h.state().s.mailbox.contacts[&ItemId(sel)].last_name, "Patel");
    click(&h, "Card");
    h.run_ok();
    assert!(has(&h, "Rivera, Alice"));
    click(&h, "New Contact");
    h.run_ok();
    assert!(has(&h, "Save & Close"));
}

#[test]
fn tasks_todo_and_folder() {
    let mut h = harness(super::tasks, app(), (1400.0, 900.0));
    assert!(has(&h, "To-Do List"));
    assert!(has(&h, "Review quarterly report"));
    assert!(!has(&h, "Book meeting room for kickoff"));
    click(&h, "Complete Review quarterly report");
    h.run_ok();
    let done =
        h.state().s.mailbox.tasks.values().find(|t| t.subject == "Review quarterly report").map(|t| t.status);
    assert_eq!(done, Some(TaskStatus::Completed));
    assert!(!has(&h, "Review quarterly report"));
    super::set_tasks_todo(&h.ctx, false);
    h.run_ok();
    assert!(has(&h, "Book meeting room for kickoff"));
    assert!(has(&h, "Review quarterly report"));
    click(&h, "Submit benefits forms");
    h.run_ok();
    assert!(has(&h, "Mark Complete"));
}

#[test]
fn notes_grid_list_and_new_note() {
    let mut h = harness(super::notes, app(), (1400.0, 900.0));
    assert!(has(&h, "My Notes"));
    assert!(has(&h, "Wi-Fi for guests"));
    assert!(has(&h, "Retro ideas"));
    click(&h, "New Note");
    h.run_ok();
    assert_eq!(h.state().s.mailbox.notes.len(), 3);
    assert!(has(&h, "Note text"));
    super::set_notes_view(&h.ctx, &json!("list"));
    h.run_ok();
    assert!(has(&h, "Wi-Fi for guests"));
}

#[test]
fn every_view_survives_dark_narrow_and_empty() {
    for (dark, size) in [(false, (1400.0, 900.0)), (true, (640.0, 420.0)), (false, (200.0, 120.0))] {
        for mode in ["day", "workWeek", "week", "month", "schedule", "junk"] {
            let mut a = app();
            if dark {
                a.theme = crate::theme::Theme::dark();
            }
            a.ui.calendar_view = mode.into();
            let _ = harness(super::calendar, a, size);
        }
        let views: [View; 4] = [super::calendar, super::people, super::tasks, super::notes];
        for v in views {
            let mut a =
                App::new(mailcraft_engine::Session::new(mailcraft_model::Mailbox::new_local("Empty")));
            if dark {
                a.theme = crate::theme::Theme::dark();
            }
            let _ = harness(v, a, size);
            let _ = harness(v, app(), size);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Reminders, meetings, recurrence, scheduling, To-Do Bar, contact groups, notes
// ---------------------------------------------------------------------------------------------------------

fn an_hour_ago(a: &App) -> String {
    let p = mailcraft_engine::cmd::calendar::pim::add_secs(a.s.now(), -3600);
    mailcraft_engine::cmd::calendar::pim::datetime_string(p, 0)
}

#[test]
fn reminders_window_pops_up_and_dismisses() {
    let mut a = app();
    let at = an_hour_ago(&a);
    a.run("tasks.new", json!({ "subject": "Pay rent", "reminder": at })).unwrap();
    let mut h = harness(super::tasks, a, (1400.0, 900.0));
    h.run_ok();
    assert!(has(&h, "1 Reminder"), "the window opens for a new reminder");
    assert!(has(&h, "1 hour overdue"));
    assert_eq!(super::reminders_due(&h.ctx), 1);
    click(&h, "Dismiss");
    h.run_ok();
    let t = h.state().s.mailbox.tasks.values().find(|t| t.subject == "Pay rent").unwrap();
    assert!(t.reminder.is_none());
    assert!(!has(&h, "1 Reminder"), "nothing left: the window closes");
}

#[test]
fn reminders_window_snoozes_and_opens_items() {
    let mut a = app();
    let at = an_hour_ago(&a);
    a.run("tasks.new", json!({ "subject": "Call Ann", "reminder": at })).unwrap();
    let mut h = harness(super::tasks, a, (1400.0, 900.0));
    h.run_ok();
    click(&h, "Open Item");
    h.run_ok();
    // The engine asked for the task window (the shell forwards notices; do it here).
    let notices = std::mem::take(&mut h.state_mut().s.notices);
    for n in notices {
        if let mailcraft_engine::Notice::OpenDialog(name, p) = n {
            h.state_mut().ui.dialogs.push((name, p));
        }
    }
    h.run_ok();
    assert!(has(&h, "Call Ann - Task"));
    click(&h, "Snooze");
    h.run_ok();
    let t = h.state().s.mailbox.tasks.values().find(|t| t.subject == "Call Ann").unwrap();
    assert!(t.reminder.unwrap().unix > h.state().s.now().unix, "snoozed for five minutes");
    super::open_reminders(&h.ctx, h.state());
    h.run_ok();
    assert!(has(&h, "0 Reminders"));
}

/// A meeting request from Alice in the sample Inbox (built like a received message).
fn invitation(a: &mut App) -> u64 {
    let mut org = mailcraft_engine::Session::new(mailcraft_model::Mailbox::new_local("Alice"));
    org.fixed_now = Some(a.s.now());
    if let Some(acc) = org.mailbox.accounts.first_mut() {
        acc.address = "alice@example.com".into();
        acc.sender_name = "Alice Rivera".into();
    }
    let me = a.s.mailbox.default_account().map(|x| x.address.clone()).unwrap();
    let r = org
        .run(
            "calendar.newMeeting",
            &json!({"subject": "Budget sync", "start": "2026-10-13T15:00", "durationMin": 30, "attendees": [me]}),
        )
        .unwrap();
    let c = &org.composes[&r["compose"].as_u64().unwrap()];
    let mut b = mailcraft_mime::MessageBuilder::new();
    b.from = Some(mailcraft_mime::Mailbox::new(Some("Alice Rivera"), "alice@example.com"));
    b.to = mailcraft_mime::parse_address_list(&c.to);
    b.subject = c.subject.clone();
    b.text = Some(c.body.to_plain_text());
    b.attachments = c
        .attachments
        .iter()
        .map(|x| mailcraft_mime::AttachmentSpec {
            filename: x.filename.clone(),
            mime_type: x.mime_type.clone(),
            data: x.data.clone(),
            content_id: None,
        })
        .collect();
    let raw = b.build();
    let msg = mailcraft_mime::parse(&raw).unwrap();
    let mut item = mailcraft_model::MailItem::from_message(&msg, &raw, a.s.now());
    let acc = a.s.mailbox.default_account().unwrap().id;
    item.folder = a.s.mailbox.role_folder(acc, mailcraft_model::FolderRole::Inbox).unwrap();
    a.s.mailbox.add_mail(item).0
}

#[test]
fn meeting_bar_accepts_a_request() {
    let mut a = app();
    let mail = invitation(&mut a);
    let mut h = Harness::builder().with_size(vec2(900.0, 500.0)).build_ui_state(
        move |ui: &mut egui::Ui, app: &mut App| {
            super::meeting_bar(ui, app, mail);
        },
        a,
    );
    h.run_ok();
    assert!(has(&h, "Please respond."));
    assert!(has(&h, "Tuesday, October 13, 2026 3:00 PM-3:30 PM"));
    assert!(has(&h, "Alice Rivera <alice@example.com>"));
    click(&h, "Accept");
    h.run_ok();
    click(&h, "Don't send a response");
    h.run_ok();
    let added =
        h.state().s.mailbox.appointments.values().find(|x| x.subject == "Budget sync").map(|x| x.is_meeting);
    assert_eq!(added, Some(true));
    assert!(has(&h, "You accepted this meeting."));
    assert!(has(&h, "Open in Calendar"));
    // Propose New Time opens its window.
    click(&h, "Propose New Time");
    h.run_ok();
    click(&h, "Tentative and propose a new time");
    h.run_ok();
    assert!(has(&h, "Propose Time"));
    click(&h, "Propose Time");
    h.run_ok();
    assert!(!has(&h, "Propose Time"));
    let c = h.state().s.composes.values().last().unwrap();
    assert!(c.subject.starts_with("New Time Proposed:"), "{}", c.subject);
}

#[test]
fn meeting_bar_ignores_plain_mail() {
    let a = app();
    let mail = *a.s.mailbox.mail.keys().next().unwrap();
    let mut drew = true;
    let ctx = egui::Context::default();
    let mut a = a;
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| drew = super::meeting_bar(ui, &mut a, mail.0));
    out.textures_delta.clear();
    assert!(!drew);
}

#[test]
fn appointment_recurrence_dialog_and_scheduling() {
    let mut a = app();
    a.ui.dialogs
        .push(("appointment".into(), json!({ "start": "2026-10-12T09:00", "end": "2026-10-12T09:30" })));
    let mut h = harness(super::calendar, a, (1400.0, 1000.0));
    h.run_ok();
    click(&h, "Recurrence…");
    h.run_ok();
    assert!(has(&h, "Appointment Recurrence"));
    assert!(has_part(&h, "Occurs every Monday effective 10/12/2026"));
    click(&h, "Daily");
    h.run_ok();
    click(&h, "Every weekday");
    h.run_ok();
    click(&h, "OK");
    h.run_ok();
    assert!(!has(&h, "Appointment Recurrence"));
    assert!(has(&h, "Occurs every weekday"));
    // Scheduling Assistant: the user's own busy time.
    click(&h, "Scheduling Assistant");
    h.run_ok();
    assert!(has(&h, "Scheduling grid"));
    assert!(has(&h, "Next free time"));
    click(&h, "Appointment");
    h.run_ok();
    click(&h, "Save & Close");
    h.run_ok();
    let a = h
        .state()
        .s
        .mailbox
        .appointments
        .values()
        .find(|x| x.start.unix % 86_400 == 9 * 3600 && x.recurrence.is_some() && x.subject.is_empty());
    assert_eq!(
        a.map(|x| x.recurrence.clone().unwrap().rrule),
        Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".to_string())
    );
}

#[test]
fn recurring_occurrence_prompt() {
    let mut a = app();
    a.ui.calendar_view = "day".into();
    let mut h = harness(super::calendar, a, (1400.0, 900.0));
    let label = "Team stand-up, Friday, October 9, 2026 9:00 AM";
    let node = h.query_all_by_label(label).next().expect("stand-up block");
    node.click();
    h.run_ok();
    let id = ItemId(h.state().ui.module_selection.unwrap());
    let occ = mailcraft_engine::cmd::calendar::pim::parse_local("2026-10-09T09:00", 0).unwrap().unix;
    let mut st = super::common::load(&h.ctx);
    super::editors::open_occurrence(h.state(), &mut st, id, occ);
    super::common::store(&h.ctx, st);
    h.run_ok();
    assert!(has(&h, "Open Recurring Item"));
    click(&h, "OK");
    h.run_ok();
    assert!(has(&h, "Team stand-up - Meeting Occurrence"));
}

#[test]
fn todo_bar_parts_render_and_add_tasks() {
    let a = app();
    let mut h = Harness::builder()
        .with_size(vec2(300.0, 900.0))
        .build_ui_state(|ui: &mut egui::Ui, app: &mut App| super::todo_bar(ui, app), a);
    super::set_todo_bar(&h.ctx, "calendar");
    super::set_todo_bar(&h.ctx, "people");
    super::set_todo_bar(&h.ctx, "tasks");
    h.run_ok();
    assert!(super::todo_bar_shown(&h.ctx));
    assert!(has(&h, "October 2026"));
    assert!(has_part(&h, "Website refresh kickoff"));
    assert!(has(&h, "Search people"));
    assert!(has_part(&h, "Rivera, Alice"));
    assert!(has_part(&h, "Review quarterly report"));
    h.get_by_label("Type a new task").focus();
    h.run_ok();
    h.get_by_label("Type a new task").type_text("Water plants");
    h.run_ok();
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert!(h.state().s.mailbox.tasks.values().any(|t| t.subject == "Water plants"));
}

#[test]
fn contact_group_add_members() {
    let mut a = app();
    a.ui.dialogs.push(("contactGroup".into(), json!({})));
    let mut h = harness(super::people, a, (1400.0, 900.0));
    h.run_ok();
    assert!(has(&h, "No members yet. Use Add Members."));
    click(&h, "Add Members");
    h.run_ok();
    click(&h, "From Contacts…");
    h.run_ok();
    assert!(has(&h, "Select Members: Contacts"));
    click(&h, "Alice Rivera <alice@example.com>");
    h.run_ok();
    click(&h, "OK");
    h.run_ok();
    assert!(has(&h, "1 member"));
    assert!(has(&h, "Remove"));
    click(&h, "Remove");
    h.run_ok();
    assert!(has(&h, "0 members"));
}

#[test]
fn contact_editor_shows_business_card_and_forwards() {
    let mut a = app();
    let id = *a.s.mailbox.contacts.values().find(|c| c.last_name == "Rivera").map(|c| &c.id).unwrap();
    a.ui.dialogs.push(("contact".into(), json!({ "id": id.0 })));
    let mut h = harness(super::people, a, (1600.0, 1000.0));
    h.run_ok();
    assert!(has(&h, "Business card"));
    assert!(has(&h, "Contact picture"));
    click(&h, "Forward");
    h.run_ok();
    click(&h, "As a Business Card");
    h.run_ok();
    let c = h.state().s.composes.values().last().expect("a compose opened");
    assert!(c.attachments[0].filename.ends_with(".vcf"));
}

#[test]
fn note_window_colours_and_saves() {
    let a = app();
    let id = *a.s.mailbox.notes.keys().next().unwrap();
    let mut h = harness(super::notes, a, (1400.0, 900.0));
    let mut st = super::common::load(&h.ctx);
    super::editors::open_note(h.state(), &mut st, id);
    super::common::store(&h.ctx, st);
    h.run_ok();
    assert!(has_part(&h, "Modified"));
    click(&h, "Pink note");
    h.run_ok();
    click(&h, "Save & Close");
    h.run_ok();
    assert_eq!(h.state().s.mailbox.notes[&id].color, mailcraft_model::NoteColor::Pink);
}
