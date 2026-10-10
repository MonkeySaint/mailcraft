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
