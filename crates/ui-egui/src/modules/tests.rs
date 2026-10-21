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

// ---------------------------------------------------------------------------------------------------------
// Module views (view.changeView and friends), module actions and dialogs
// ---------------------------------------------------------------------------------------------------------

/// The shell forwards `Notice::OpenDialog` into `ui.dialogs`; the tests do the same.
fn forward(app: &mut App) {
    for n in std::mem::take(&mut app.s.notices) {
        if let mailcraft_engine::Notice::OpenDialog(name, p) = n {
            app.ui.dialogs.push((name, p));
        }
    }
}

/// Lets the sample account send (it is a local data file in the sample).
fn can_send(a: &mut App) {
    for acct in &mut a.s.mailbox.accounts {
        acct.kind = mailcraft_model::AccountKind::Jmap;
    }
}

fn select_module_folder(a: &mut App, kind: mailcraft_model::FolderKind) -> mailcraft_model::FolderId {
    let f = a.s.mailbox.folders.values().find(|f| f.kind == kind).map(|f| f.id).unwrap();
    a.run("nav.selectFolder", json!({ "folder": f.0 })).unwrap();
    f
}

/// Runs a command in a harness and forwards its dialogs.
fn run_cmd(h: &mut Harness<'_, App>, id: &str, p: serde_json::Value) {
    let app = h.state_mut();
    let r = app.run(id, p);
    assert!(r.is_some(), "{id} failed: {:?}", app.ui.toast);
    forward(app);
    h.run_ok();
}

#[test]
fn people_views_render() {
    let mut a = app();
    select_module_folder(&mut a, mailcraft_model::FolderKind::Contacts);
    let mut h = harness(super::people, a, (1500.0, 900.0));
    assert!(has(&h, "Rivera, Alice"));
    run_cmd(&mut h, "view.changeView", json!({"name": "Business Card"}));
    assert!(has(&h, "Business card Rivera, Alice"));
    run_cmd(&mut h, "view.changeView", json!({"name": "Phone"}));
    assert!(has_part(&h, "Rivera, Alice"));
    assert!(!has_part(&h, "Group "), "the Phone view is not grouped");
    run_cmd(&mut h, "view.changeView", json!({"name": "List"}));
    // The List view groups by company; a group header collapses its contacts.
    assert!(has(&h, "Group Example Co.: 2 items, expanded"));
    click(&h, "Group Example Co.: 2 items, expanded");
    h.run_ok();
    assert!(has(&h, "Group Example Co.: 2 items, collapsed"));
    assert!(!has(&h, "Rivera, Alice"));
    run_cmd(&mut h, "view.expandAll", json!({}));
    assert!(has(&h, "Rivera, Alice"));
    run_cmd(&mut h, "view.arrangeBy", json!({"key": "categories"}));
    assert!(has(&h, "Group (none): 6 items, expanded"));
    // The toolbar switches views too.
    click(&h, "People");
    h.run_ok();
    assert!(has(&h, "Meeting"), "the contact card shows in the People view");
    run_cmd(&mut h, "view.readingPane", json!({"position": "off"}));
    assert!(!has(&h, "Meeting"), "Reading Pane › Off hides the contact card");
}

#[test]
fn people_ribbon_actions_reach_contacts() {
    let mut a = app();
    select_module_folder(&mut a, mailcraft_model::FolderKind::Contacts);
    let mut h = harness(super::people, a, (1500.0, 900.0));
    click(&h, "Rivera, Alice");
    h.run_ok();
    let id = ItemId(h.state().ui.module_selection.unwrap());
    // The engine selection follows the view, so ribbon commands without ids act on the contact.
    assert_eq!(h.state().s.selection.focus, Some(id));
    run_cmd(&mut h, "people.flag", json!({"kind": "tomorrow"}));
    assert!(matches!(h.state().s.mailbox.contacts[&id].flag, mailcraft_model::FlagState::Flagged { .. }));
    run_cmd(&mut h, "people.private", json!({}));
    assert!(h.state().s.mailbox.contacts[&id].private);
    // Move › Other Folder opens the module's folder picker.
    let root = h.state().s.mailbox.contacts[&id].folder;
    let sub = h.state_mut().s.mailbox.create_folder(root, "Clients").unwrap();
    run_cmd(&mut h, "people.move", json!({}));
    assert!(has(&h, "Move Items"));
    click(&h, &h.state().s.mailbox.folder_path(sub));
    h.run_ok();
    click(&h, "OK");
    h.run_ok();
    assert_eq!(h.state().s.mailbox.contacts[&id].folder, sub);
    assert!(!has(&h, "Move Items"));
}

#[test]
fn mail_merge_window_previews_and_sends() {
    let mut a = app();
    can_send(&mut a);
    select_module_folder(&mut a, mailcraft_model::FolderKind::Contacts);
    let mut h = harness(super::people, a, (1500.0, 1000.0));
    run_cmd(&mut h, "people.mailMerge", json!({"contacts": "all", "output": "letters"}));
    assert!(has(&h, "Mail Merge Contacts"));
    h.get_by_label("Message subject line").focus();
    h.run_ok();
    h.get_by_label("Message subject line").type_text("Hello");
    h.run_ok();
    assert!(has_part(&h, "Dear Bob,"), "the preview merges the first contact");
    click(&h, "▶");
    h.run_ok();
    assert!(has_part(&h, "Dear Grace,"));
    click(&h, "Send");
    h.run_ok();
    // The sample account sends: one message per contact in the Outbox.
    let outbox: Vec<String> = h
        .state()
        .s
        .mailbox
        .mail
        .values()
        .filter(|m| {
            h.state()
                .s
                .mailbox
                .folder(m.folder)
                .is_some_and(|f| f.role == Some(mailcraft_model::FolderRole::Outbox))
        })
        .map(|m| m.subject.clone())
        .collect();
    assert_eq!(outbox.iter().filter(|s| *s == "Hello").count(), 6, "{outbox:?}");
    assert!(!has(&h, "Mail Merge Contacts"));
}

#[test]
fn assign_task_window_sends_a_request() {
    let mut a = app();
    can_send(&mut a);
    select_module_folder(&mut a, mailcraft_model::FolderKind::Contacts);
    let mut h = harness(super::people, a, (1500.0, 1000.0));
    click(&h, "Rivera, Alice");
    h.run_ok();
    run_cmd(&mut h, "people.assignTask", json!({}));
    assert!(has(&h, "Untitled - Task Request"));
    h.get_by_label("Subject").focus();
    h.run_ok();
    h.get_by_label("Subject").type_text("Order supplies");
    h.run_ok();
    click(&h, "Send");
    h.run_ok();
    let t =
        h.state().s.mailbox.tasks.values().find(|t| t.subject == "Order supplies").expect("assigned copy");
    assert_eq!(t.assigned_to.len(), 1);
    assert!(t.assigned_to[0].contains("alice@example.com"));
    let sent = h.state().s.mailbox.mail.values().any(|m| m.subject == "Task Request: Order supplies");
    assert!(sent, "the request went to the Outbox");
}

#[test]
fn task_views_render_and_group() {
    let mut a = app();
    let folder = select_module_folder(&mut a, mailcraft_model::FolderKind::Tasks);
    let mut h = harness(super::tasks, a, (1500.0, 900.0));
    // To-Do List: grouped by due date.
    assert!(has_part(&h, "Group Next Week") || has_part(&h, "Group This Week"));
    run_cmd(&mut h, "view.changeView", json!({"name": "Detailed"}));
    assert!(has(&h, "% Complete") && has(&h, "In Folder"));
    assert!(has(&h, "Book meeting room for kickoff"));
    run_cmd(&mut h, "view.changeView", json!({"name": "Active"}));
    assert!(!has(&h, "Book meeting room for kickoff"));
    assert!(has(&h, "Submit benefits forms"));
    run_cmd(&mut h, "view.changeView", json!({"name": "Completed"}));
    assert!(has(&h, "Book meeting room for kickoff"));
    assert!(!has(&h, "Submit benefits forms"));
    run_cmd(&mut h, "view.changeView", json!({"name": "Prioritized"}));
    assert!(has(&h, "Group High: 1 items, expanded"));
    click(&h, "Group High: 1 items, expanded");
    h.run_ok();
    assert!(!has(&h, "Complete Review quarterly report"), "collapsed");
    assert!(
        h.state().s.view.collapsed_groups.get(&folder).is_some_and(|g| g.iter().any(|x| x == "High")),
        "the engine keeps the collapsed group"
    );
    run_cmd(&mut h, "view.expandAll", json!({}));
    // Message preview shows the task notes.
    let id = h.state().s.mailbox.tasks.values().find(|t| t.subject == "Submit benefits forms").unwrap().id;
    h.state_mut().run("tasks.update", json!({"id": id.0, "body": "Forms are in the HR portal"}));
    run_cmd(&mut h, "view.messagePreview", json!({"lines": 1}));
    assert!(has_part(&h, "Forms are in the HR portal"));
    // Reading pane at the bottom still shows the details.
    click(&h, "Submit benefits forms");
    h.run_ok();
    run_cmd(&mut h, "view.readingPane", json!({"position": "bottom"}));
    assert!(has(&h, "Mark Complete"));
    run_cmd(&mut h, "view.readingPane", json!({"position": "off"}));
    assert!(!has(&h, "Remove from List"));
}

#[test]
fn task_ribbon_actions_and_recurrence() {
    let mut a = app();
    select_module_folder(&mut a, mailcraft_model::FolderKind::Tasks);
    let mut h = harness(super::tasks, a, (1500.0, 900.0));
    click(&h, "Submit benefits forms");
    h.run_ok();
    let id = ItemId(h.state().ui.module_selection.unwrap());
    run_cmd(&mut h, "tasks.importance", json!({"level": "high"}));
    assert_eq!(h.state().s.mailbox.tasks[&id].priority, mailcraft_model::TaskPriority::High);
    run_cmd(&mut h, "tasks.flag", json!({"kind": "tomorrow"}));
    let tz = h.state().s.tz_offset_min;
    let due =
        h.state().s.mailbox.tasks[&id].due.map(|d| mailcraft_engine::cmd::calendar::pim::date_string(d, tz));
    assert_eq!(due.as_deref(), Some("2026-10-10"));
    run_cmd(&mut h, "tasks.forward", json!({}));
    assert!(h.state().s.composes.values().any(|c| c.subject == "FW: Submit benefits forms"));
    // The task window sets a weekly recurrence; completing moves the task on a week.
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert!(has(&h, "Submit benefits forms - Task"));
    // The recurrence combo's "Every week" (the due date is a Saturday).
    let mut st = super::common::load(&h.ctx);
    if let Some(ed) = st.editors.task.as_mut() {
        ed.recurrence = "FREQ=WEEKLY;BYDAY=SA".into();
    }
    super::common::store(&h.ctx, st);
    h.run_ok();
    assert!(has_part(&h, "Occurs every Saturday"));
    click(&h, "Save & Close");
    h.run_ok();
    assert!(h.state().s.mailbox.tasks[&id].recurrence.is_some());
    click(&h, "Complete Submit benefits forms");
    h.run_ok();
    let t = &h.state().s.mailbox.tasks[&id];
    assert_eq!(t.status, TaskStatus::NotStarted, "the series moved to its next occurrence");
    assert_eq!(
        t.due.map(|d| mailcraft_engine::cmd::calendar::pim::date_string(d, tz)).as_deref(),
        Some("2026-10-17")
    );
    assert!(
        h.state()
            .s
            .mailbox
            .tasks
            .values()
            .any(|x| x.subject == "Submit benefits forms" && x.status == TaskStatus::Completed)
    );
}

#[test]
fn notes_views_icons_and_line_up() {
    let mut a = app();
    let folder = select_module_folder(&mut a, mailcraft_model::FolderKind::Notes);
    let mut h = harness(super::notes, a, (1500.0, 900.0));
    assert!(has(&h, "Wi-Fi for guests"));
    run_cmd(&mut h, "view.changeView", json!({"name": "Last 7 Days"}));
    assert!(has(&h, "Retro ideas"));
    assert!(!has(&h, "Wi-Fi for guests"), "changed 20 days ago");
    run_cmd(&mut h, "view.changeView", json!({"name": "Notes List"}));
    assert!(has(&h, "Wi-Fi for guests"));
    // The ribbon's Small Icons switches back to the Icon view.
    run_cmd(&mut h, "notes.iconView", json!({"mode": "small"}));
    let st = mailcraft_engine::cmd::view::modules::state(
        &h.state().s,
        folder,
        mailcraft_engine::cmd::view::modules::ModuleKind::Notes,
    );
    assert_eq!(st.view, "Icon");
    // Dragging an icon takes it off the grid; Line Up Icons puts it back.
    let node = h.query_all_by_label("Retro ideas").next().unwrap();
    let r = node.rect();
    h.drag_at(r.center());
    h.run_ok();
    h.hover_at(r.center() + egui::vec2(260.0, 160.0));
    h.run_ok();
    h.drop_at(r.center() + egui::vec2(260.0, 160.0));
    h.run_ok();
    assert!(!super::common::load(&h.ctx).note_pos.is_empty(), "dragged off the grid");
    let st = mailcraft_engine::cmd::view::modules::state(
        &h.state().s,
        folder,
        mailcraft_engine::cmd::view::modules::ModuleKind::Notes,
    );
    assert!(!st.lined_up);
    run_cmd(&mut h, "notes.lineUpIcons", json!({}));
    assert!(super::common::load(&h.ctx).note_pos.is_empty(), "lined up again");
    // The reading pane shows the selected note.
    click(&h, "Retro ideas");
    h.run_ok();
    run_cmd(&mut h, "view.readingPane", json!({"position": "right"}));
    assert!(has_part(&h, "shared glossary"));
}

#[test]
fn todo_bar_shows_in_every_module() {
    for v in [super::people as View, super::tasks, super::notes] {
        let a = app();
        let mut h = harness(v, a, (1600.0, 900.0));
        assert!(!has(&h, "Search people"));
        super::set_todo_bar(&h.ctx, "people");
        h.run_ok();
        assert!(has(&h, "Search people"), "the To-Do Bar shows next to the module");
        super::set_todo_bar(&h.ctx, "off");
        h.run_ok();
    }
}

#[test]
fn module_view_settings_dialog_applies() {
    let mut a = app();
    let folder = select_module_folder(&mut a, mailcraft_model::FolderKind::Tasks);
    let mut h = harness(super::tasks, a, (1500.0, 1000.0));
    run_cmd(&mut h, "view.viewSettings", json!({}));
    assert!(has(&h, "Advanced View Settings: To-Do List"));
    click(&h, "Reverse sort");
    h.run_ok();
    click(&h, "OK");
    h.run_ok();
    assert!(mailcraft_engine::cmd::view::modules::reversed(&h.state().s, folder));
    run_cmd(&mut h, "view.saveViewAs", json!({}));
    assert!(has(&h, "Save Current View As a New View"));
    h.get_by_label("Name of new view:").focus();
    h.run_ok();
    h.get_by_label("Name of new view:").type_text("Upside down");
    h.run_ok();
    click(&h, "OK");
    h.run_ok();
    let saved = mailcraft_engine::cmd::view::modules::saved_views(&h.state().s, folder);
    assert_eq!(saved.first().map(|v| v.name.as_str()), Some("Upside down"));
}

#[test]
fn task_colour_options_override_the_theme() {
    let mut app = app();
    let danger = app.theme.danger;
    assert_eq!(super::tasks_view::option_color(&app, "tasks.overdueColor", danger), danger);
    app.s.run("options.set", &json!({"key": "tasks.overdueColor", "value": "#0000ff"})).unwrap();
    assert_eq!(super::tasks_view::option_color(&app, "tasks.overdueColor", danger), egui::Color32::BLUE);
    app.s.run("options.set", &json!({"key": "tasks.completedColor", "value": "nonsense"})).unwrap();
    let muted = app.theme.text_muted;
    assert_eq!(super::tasks_view::option_color(&app, "tasks.completedColor", muted), muted);
}

/// A message from `from` carrying the task part `ics` (as a received message in the sample Inbox).
fn task_mail(a: &mut App, from: (&str, &str), ics: &str) -> u64 {
    let me = a.s.mailbox.default_account().map(|x| x.address.clone()).unwrap();
    let mut b = mailcraft_mime::MessageBuilder::new();
    b.from = Some(mailcraft_mime::Mailbox::new(Some(from.0), from.1));
    b.to = mailcraft_mime::parse_address_list(&me);
    b.subject = "Task".into();
    b.text = Some("See the task.".into());
    b.attachments = vec![mailcraft_mime::AttachmentSpec {
        filename: "task.ics".into(),
        mime_type: "text/calendar; method=REQUEST".into(),
        data: ics.as_bytes().to_vec(),
        content_id: None,
    }];
    let raw = b.build();
    let msg = mailcraft_mime::parse(&raw).unwrap();
    let mut item = mailcraft_model::MailItem::from_message(&msg, &raw, a.s.now());
    let acc = a.s.mailbox.default_account().unwrap().id;
    item.folder = a.s.mailbox.role_folder(acc, mailcraft_model::FolderRole::Inbox).unwrap();
    a.s.mailbox.add_mail(item).0
}

fn bar_harness(a: App, mail: u64) -> Harness<'static, App> {
    let mut h = Harness::builder().with_size(vec2(900.0, 500.0)).build_ui_state(
        move |ui: &mut egui::Ui, app: &mut App| {
            super::meeting_bar(ui, app, mail);
        },
        a,
    );
    h.run_ok();
    h
}

#[test]
fn task_bar_accepts_and_declines_a_task_request() {
    let mut a = app();
    let me = a.s.mailbox.default_account().map(|x| x.address.clone()).unwrap();
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Other client//EN\r\nMETHOD:REQUEST\r\n\
BEGIN:VTODO\r\nUID:todo-7@example.com\r\nDTSTAMP:20261009T080000Z\r\nSUMMARY:Water the plants\r\n\
DUE;VALUE=DATE:20261016\r\nSTATUS:NEEDS-ACTION\r\nORGANIZER;CN=\"Boss\":mailto:boss@example.com\r\n\
ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{me}\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
    );
    let mail = task_mail(&mut a, ("Boss", "boss@example.com"), &ics);
    let mut h = bar_harness(a, mail);
    assert!(has(&h, "Please respond. Accept the task to add it to your task list."));
    assert!(has(&h, "Water the plants"));
    assert!(has(&h, "2026-10-16"));
    assert!(has(&h, "Boss <boss@example.com>"));
    assert!(!has(&h, "Update"), "requests have no Update");
    click(&h, "Accept");
    h.run_ok();
    click(&h, "Edit the response before sending");
    h.run_ok();
    let task = h.state().s.mailbox.tasks.values().find(|t| t.subject == "Water the plants").map(|t| t.id);
    assert!(task.is_some(), "accepting adds the task");
    assert!(
        h.state().s.composes.values().any(|c| c.subject.starts_with("Task Accepted:")),
        "the response opens for editing"
    );
    assert!(has(&h, "You accepted this task. It is on your task list."));
    click(&h, "Open Task");
    h.run_ok();
    assert!(h.state().ui.dialogs.iter().any(|(n, p)| n == "task" && p["id"] == task.unwrap().0));
    // Declining afterwards still works from the bar.
    click(&h, "Decline");
    h.run_ok();
    click(&h, "Edit the response before sending");
    h.run_ok();
    assert!(has(&h, "You declined this task."));
    // Plain mail gets no task bar.
    let plain = *h.state().s.mailbox.mail.keys().next().unwrap();
    let ctx = egui::Context::default();
    let mut drew = true;
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        drew = super::meeting_bar::task_bar(ui, h.state_mut(), plain.0);
    });
    out.textures_delta.clear();
    assert!(!drew);
}

#[test]
fn task_bar_updates_my_copy_from_a_response() {
    let mut a = app();
    let r = a
        .s
        .run("tasks.assign", &json!({"to": "Ann Lee <ann@example.com>", "subject": "Budget", "send": false}))
        .unwrap();
    let copy = ItemId(r["task"].as_u64().unwrap());
    let uid = a.s.mailbox.tasks[&copy].uid.clone();
    let me = a.s.mailbox.default_account().map(|x| x.address.clone()).unwrap();
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Other client//EN\r\nMETHOD:REPLY\r\n\
BEGIN:VTODO\r\nUID:{uid}\r\nDTSTAMP:20261009T080000Z\r\nSUMMARY:Budget\r\nSTATUS:IN-PROCESS\r\n\
PERCENT-COMPLETE:40\r\nORGANIZER:mailto:{me}\r\n\
ATTENDEE;CN=\"Ann Lee\";PARTSTAT=IN-PROCESS:mailto:ann@example.com\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
    );
    let mail = task_mail(&mut a, ("Ann Lee", "ann@example.com"), &ics);
    let mut h = bar_harness(a, mail);
    assert!(has(&h, "Ann Lee reports: In Progress, 40% complete."));
    assert!(!has(&h, "Accept"), "responses cannot be accepted");
    assert_eq!(h.state().s.mailbox.tasks[&copy].percent_complete, 0);
    click(&h, "Update");
    h.run_ok();
    let t = &h.state().s.mailbox.tasks[&copy];
    assert_eq!(t.percent_complete, 40);
    assert_eq!(t.request_state, mailcraft_model::task::TaskRequestState::Accepted);
}

/// The shell's order in the Mail module: the modules' item windows, then the dialogs area.
fn mail_window(ui: &mut egui::Ui, app: &mut App) {
    let ctx = ui.ctx().clone();
    super::pim_windows(&ctx, app);
    crate::dialogs::dialogs(&ctx, app);
}

#[test]
fn mail_new_items_open_the_full_module_windows() {
    let mut a = app();
    assert_eq!(a.s.view.module, mailcraft_engine::Module::Mail);
    // Appointment: the calendar's own window (pages, recurrence, Scheduling Assistant).
    a.run("mail.newItem", json!({"kind": "appointment"})).unwrap();
    let mut h = harness(mail_window, a, (1400.0, 1000.0));
    h.run_ok();
    assert!(has(&h, "Untitled - Appointment"));
    assert!(has(&h, "Scheduling Assistant"));
    assert!(h.state().ui.dialogs.is_empty(), "{:?}", h.state().ui.dialogs);
    click(&h, "Save & Close");
    h.run_ok();
    // Meeting (also Reply with Meeting): the meeting window.
    h.state_mut().run("mail.newItem", json!({"kind": "recurringMeeting"}));
    h.run_ok();
    h.run_ok();
    assert!(has(&h, "Untitled - Meeting"));
    assert!(has(&h, "Appointment Recurrence"), "recurring kinds open the Recurrence dialog");
    // Contact, contact group and task: the People and Tasks windows.
    for (kind, title) in [
        ("contact", "Untitled - Contact"),
        ("contactGroup", "Untitled - Contact Group"),
        ("task", "Untitled - Task"),
    ] {
        h.state_mut().run("mail.newItem", json!({"kind": kind}));
        h.run_ok();
        h.run_ok();
        assert!(has(&h, title), "{kind}");
    }
    assert!(h.state().ui.dialogs.is_empty(), "{:?}", h.state().ui.dialogs);
}

#[test]
fn reply_with_meeting_opens_the_meeting_window_prefilled() {
    let mut a = app();
    let folder = a.s.selection.folder;
    let id = a.s.mailbox.items_in(folder).iter().find(|m| m.from.is_some()).map(|m| m.id).unwrap();
    let subject = a.s.mailbox.mail[&id].subject.clone();
    a.run("nav.selectItem", json!({"id": id.0})).unwrap();
    a.run("mail.replyWithMeeting", json!({})).unwrap();
    let mut h = harness(mail_window, a, (1400.0, 1000.0));
    h.run_ok();
    assert!(has(&h, &format!("{} - Meeting", subject.trim())), "{subject}");
    assert!(has(&h, "Scheduling Assistant"));
    let st = super::common::load(&h.ctx);
    let ed = st.editors.appt.as_ref().expect("meeting window");
    assert!(ed.meeting && !ed.attendees.trim().is_empty(), "{}", ed.attendees);
}
