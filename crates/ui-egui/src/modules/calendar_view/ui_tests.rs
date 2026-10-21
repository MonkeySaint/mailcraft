//! Calendar view tests (kittest) over the sample mailbox (now = Fri 9 Oct 2026 2:30 PM UTC): List/Active
//! tables and sorting, saved views, reading pane, several calendars, drag to move, the Daily Task List and
//! the calendar's own windows.

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mailcraft_engine::Module;
use mailcraft_engine::cmd::calendar::{self as engine_cal, CalendarColumn, pim::Kind};
use mailcraft_engine::sample::sample_session;
use mailcraft_model::FolderId;
use serde_json::{Value, json};

use super::super::common::{load, store};
use crate::App;

fn harness(app: App) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(vec2(1400.0, 900.0))
        .build_ui_state(|ui: &mut egui::Ui, app: &mut App| super::super::calendar(ui, app), app);
    h.run_ok();
    h
}

fn app() -> App {
    let mut a = App::new(sample_session());
    a.s.view.module = Module::Calendar;
    a
}

fn has(h: &Harness<'_, App>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

fn has_part(h: &Harness<'_, App>, label: &str) -> bool {
    h.query_all_by_label_contains(label).next().is_some()
}

fn module_view(a: &mut App, id: &str, p: Value) {
    engine_cal::module_view(&mut a.s, id, &p).unwrap().unwrap();
}

fn main_calendar(a: &App) -> FolderId {
    engine_cal::pim::default_folder(&a.s, Kind::Appointment).unwrap()
}

#[test]
fn saved_list_view_is_adopted_and_sorts_by_header() {
    let mut a = app();
    module_view(&mut a, "view.changeView", json!({"name": "List"}));
    let mut h = harness(a);
    assert!(has(&h, "Sort by Subject"));
    assert!(has_part(&h, "Lunch with Frank, Fri 10/9/2026"));
    assert!(has_part(&h, "Company holiday"));
    h.query_all_by_label("Sort by Subject").next().unwrap().click();
    h.run_ok();
    let st = load(&h.ctx);
    assert_eq!(st.cal.sort, CalendarColumn::Subject);
    assert!(st.cal.ascending);
    // Active: items that have not ended, and series with occurrences to come.
    module_view(h.state_mut(), "view.changeView", json!({"name": "Active", "view": st.cal}));
    h.run_ok();
    assert_eq!(load(&h.ctx).cal.current, engine_cal::CalendarCurrentView::Active);
    assert!(has_part(&h, "Team stand-up"), "a series with occurrences to come stays");
}

#[test]
fn reading_pane_shows_the_selected_item() {
    let mut a = app();
    module_view(&mut a, "view.readingPane", json!({"position": "right"}));
    a.ui.calendar_view = "day".into();
    let mut h = harness(a);
    assert!(!has(&h, "Location: Noodle place"));
    h.query_all_by_label("Lunch with Frank, Friday, October 9, 2026 12:00 PM").next().unwrap().click();
    h.run_ok();
    assert!(has(&h, "Location: Noodle place"));
}

#[test]
fn several_calendars_side_by_side_and_overlaid() {
    let mut a = app();
    let main = main_calendar(&a);
    let r = a.run("calendar.addCalendar", json!({"source": "blank", "name": "Family"})).unwrap();
    let family = r["folder"].as_u64().unwrap();
    a.run(
        "calendar.newAppointment",
        json!({"subject": "Piano lesson", "start": "2026-10-09T16:00", "folder": family}),
    )
    .unwrap();
    a.ui.calendar_view = "week".into();
    let mut h = harness(a);
    let mut st = load(&h.ctx);
    st.cal_shown = [main.0, family].into_iter().collect();
    store(&h.ctx, st);
    h.run_ok();
    // Side by side: a tab per calendar, each item in its own pane.
    assert!(has(&h, "Family"));
    assert!(has(&h, "Calendar"));
    assert!(has_part(&h, "Piano lesson"));
    assert!(has_part(&h, "Lunch with Frank"));
    // Overlay keeps the tabs over one grid.
    let view = load(&h.ctx).cal;
    let r = h.state_mut().run("calendar.overlay", json!({"on": true, "view": view})).unwrap();
    let mut st = load(&h.ctx);
    st.cal = serde_json::from_value(r["view"].clone()).unwrap();
    store(&h.ctx, st);
    h.run_ok();
    assert!(has(&h, "Family"));
    assert!(has_part(&h, "Piano lesson"));
}

#[test]
fn dragging_a_block_moves_the_appointment() {
    let mut a = app();
    a.ui.calendar_view = "day".into();
    let mut h = harness(a);
    let rect =
        h.query_all_by_label("Lunch with Frank, Friday, October 9, 2026 12:00 PM").next().unwrap().rect();
    // 30-minute slots of 20 px: 80 px down = two hours later.
    let from = rect.center();
    let to = from + vec2(0.0, 80.0);
    h.hover_at(from);
    h.run_ok();
    h.drag_at(from);
    h.run_ok();
    h.hover_at(from + vec2(0.0, 40.0));
    h.run_ok();
    h.hover_at(to);
    h.run_ok();
    assert_eq!(super::load_ui(&h.ctx).drag.map(|d| d.kind), Some(super::DragKind::Move));
    h.drop_at(to);
    h.run_ok();
    let lunch =
        h.state().s.mailbox.appointments.values().find(|x| x.subject == "Lunch with Frank").unwrap().clone();
    let local = lunch.start.unix.rem_euclid(86_400);
    assert_eq!(local, 14 * 3600, "moved from 12:00 to 14:00");
    assert_eq!(lunch.end.unix - lunch.start.unix, 3600, "length kept");
    assert!(super::load_ui(&h.ctx).drag.is_none());
    // Undo puts it back.
    h.state_mut().s.undo_last().unwrap();
    let lunch =
        h.state().s.mailbox.appointments.values().find(|x| x.subject == "Lunch with Frank").unwrap().clone();
    assert_eq!(lunch.start.unix.rem_euclid(86_400), 12 * 3600);
}

#[test]
fn daily_task_list_under_the_week() {
    let mut a = app();
    a.ui.calendar_view = "week".into();
    let mut h = harness(a);
    // "Review quarterly report" is due Monday 12 October: next week.
    h.query_all_by_label("Next").next().unwrap().click();
    h.run_ok();
    assert!(has(&h, "Task Review quarterly report"));
    h.query_all_by_label("Mark Review quarterly report complete").next().unwrap().click();
    h.run_ok();
    assert!(!has(&h, "Task Review quarterly report"));
}

#[test]
fn calendar_windows_open_from_requests() {
    let mut a = app();
    crate::dialogs::open(&mut a, super::windows::TZ_DIALOG, json!({}));
    let mut h = harness(a);
    assert!(has(&h, "Show a second time zone"));
    assert!(h.state().ui.dialogs.is_empty(), "the calendar took the request");
    // The ribbon's Add Calendar › From Internet without a URL opens the calendar's window.
    let mut st = load(&h.ctx);
    let out =
        super::run_view_cmd(h.state_mut(), &mut st, "calendar.addCalendar", json!({"source": "internet"}));
    store(&h.ctx, st);
    assert_eq!(out.unwrap()["dialog"], super::windows::INTERNET_DIALOG);
    h.run_ok();
    assert!(has(&h, "New Internet Calendar Subscription"));
    // View Settings (the engine's notice) opens the calendar's own window.
    let mut st = load(&h.ctx);
    let out = super::run_view_cmd(h.state_mut(), &mut st, "view.viewSettings", json!({}));
    store(&h.ctx, st);
    assert!(out.is_some());
    h.run_ok();
    assert!(has(&h, "View Settings: Calendar"));
}

#[test]
fn internet_calendar_strip_and_read_only_items() {
    let mut a = app();
    let data = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Holidays\r\nBEGIN:VEVENT\r\nUID:h1@example.org\r\nDTSTART;VALUE=DATE:20261012\r\nSUMMARY:Thanksgiving\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let r = a
        .run(
            "calendar.addCalendar",
            json!({"source": "internet", "url": "https://cal.example.org/h.ics", "data": data}),
        )
        .unwrap();
    let f = r["folder"].as_u64().unwrap();
    a.run("nav.selectFolder", json!({"folder": f})).unwrap();
    a.ui.calendar_view = "month".into();
    let h = harness(a);
    assert!(has_part(&h, "Internet calendar · updated"));
    assert!(has(&h, "Refresh"));
    assert!(has_part(&h, "Thanksgiving"));
}

#[test]
fn dragging_the_bottom_edge_changes_the_length() {
    let mut a = app();
    a.ui.calendar_view = "day".into();
    let mut h = harness(a);
    let rect =
        h.query_all_by_label("Lunch with Frank, Friday, October 9, 2026 12:00 PM").next().unwrap().rect();
    let from = egui::pos2(rect.center().x, rect.max.y - 2.0);
    let to = from + vec2(0.0, 40.0);
    h.hover_at(from);
    h.run_ok();
    h.drag_at(from);
    h.run_ok();
    h.hover_at(from + vec2(0.0, 20.0));
    h.run_ok();
    h.hover_at(to);
    h.run_ok();
    h.drop_at(to);
    h.run_ok();
    let lunch =
        h.state().s.mailbox.appointments.values().find(|x| x.subject == "Lunch with Frank").unwrap().clone();
    assert_eq!(lunch.start.unix.rem_euclid(86_400), 12 * 3600, "the start stays");
    assert_eq!(lunch.end.unix.rem_euclid(86_400), 14 * 3600, "an hour longer");
}

#[test]
fn a_named_calendar_zone_shows_and_drags_times_across_dst() {
    // Now is 9 Oct 2026 (EDT, UTC-4); 16 Nov is after the change to EST (UTC-5).
    let mut a = app();
    a.run("calendar.changeTimeZone", json!({"zone": "America/Toronto"})).unwrap();
    let r = a
        .run(
            "calendar.newAppointment",
            json!({"subject": "Eye exam", "start": "2026-11-16T12:00", "durationMin": 60}),
        )
        .unwrap();
    let id = mailcraft_model::ItemId(r["id"].as_u64().unwrap());
    assert_eq!(a.s.mailbox.appointments[&id].start.unix.rem_euclid(86_400), 17 * 3600, "12:00 EST");
    a.ui.calendar_view = "day".into();
    let mut h = harness(a);
    let mut st = load(&h.ctx);
    st.cal.date = "2026-11-16".into();
    store(&h.ctx, st);
    h.run_ok();
    // The grid scrolls to the working day for the new date; let the block settle there.
    h.step();
    h.step();
    let label = "Eye exam, Monday, November 16, 2026 12:00 PM";
    assert!(has(&h, label), "shown at the time it was entered, not at EDT's 1:00 PM");
    // Dragged two hours down: 2:00 PM EST.
    let from = h.query_all_by_label(label).next().unwrap().rect().center();
    let to = from + vec2(0.0, 80.0);
    h.hover_at(from);
    h.run_ok();
    h.drag_at(from);
    h.run_ok();
    h.hover_at(from + vec2(0.0, 40.0));
    h.run_ok();
    h.hover_at(to);
    h.run_ok();
    assert_eq!(super::load_ui(&h.ctx).drag.map(|d| d.kind), Some(super::DragKind::Move));
    h.drop_at(to);
    h.run_ok();
    let start = h.state().s.mailbox.appointments[&id].start.unix;
    assert_eq!(start.rem_euclid(86_400), 19 * 3600, "14:00 EST = 19:00 UTC");
    // The window shows the same wall-clock time.
    let mut st = load(&h.ctx);
    super::super::editors::open_appointment(h.state(), &mut st, id);
    let ed = st.editors.appt.as_ref().unwrap();
    assert_eq!((ed.start_date.as_str(), ed.start_min), ("2026-11-16", 14 * 60));
}

#[test]
fn new_meeting_opens_a_prefilled_window_and_saves_nothing() {
    let a = app();
    let before = a.s.mailbox.appointments.len();
    let mut h = harness(a);
    // Home › New Meeting (the ribbon runs it through the calendar).
    let mut st = load(&h.ctx);
    let out = super::run_view_cmd(
        h.state_mut(),
        &mut st,
        "calendar.newMeeting",
        json!({"subject": "Kickoff", "attendees": ["Ann Lee <ann@example.com>"], "body": "Agenda"}),
    );
    store(&h.ctx, st);
    assert_eq!(out.unwrap()["dialog"], "newMeeting");
    let ed = load(&h.ctx).editors.appt.unwrap();
    assert!(ed.meeting && ed.id.is_none());
    assert_eq!(
        (ed.subject.as_str(), ed.attendees.as_str(), ed.body.as_str()),
        ("Kickoff", "Ann Lee <ann@example.com>", "Agenda")
    );
    assert!(h.state().s.notices.iter().all(|n| !matches!(n, mailcraft_engine::Notice::OpenDialog(..))));
    h.run_ok();
    assert!(has_part(&h, "Kickoff - Meeting"));
    assert_eq!(h.state().s.mailbox.appointments.len(), before);
    assert!(h.state().s.composes.is_empty());
    // A request made elsewhere (People › Meeting, a quick step) opens the same window here.
    h.state_mut()
        .run("calendar.newMeeting", json!({"subject": "Elsewhere", "optional": ["bob@example.org"]}));
    h.run_ok();
    let ed = load(&h.ctx).editors.appt.unwrap();
    assert_eq!((ed.subject.as_str(), ed.optional.as_str()), ("Elsewhere", "bob@example.org"));
    assert!(h.state().ui.dialogs.is_empty(), "the calendar took the request");
    assert_eq!(h.state().s.mailbox.appointments.len(), before);
}

#[test]
fn new_items_presets_and_option_defaults() {
    let mut a = app();
    a.s.settings.set("calendar.defaultDuration", &json!(45)).unwrap();
    a.s.settings.set("calendar.reminderMinutes", &json!(10)).unwrap();
    let before = a.s.mailbox.appointments.len();
    let mut h = harness(a);
    let mut st = load(&h.ctx);
    // New Items › More Items › Recurring Appointment: the window with the Recurrence dialog over it.
    super::run_view_cmd(
        h.state_mut(),
        &mut st,
        "calendar.newAppointment",
        json!({"form": true, "recurring": true}),
    );
    let ed = st.editors.appt.clone().unwrap();
    assert!(ed.recurrence_dlg.is_some() && !ed.meeting && !ed.all_day);
    assert_eq!(ed.reminder, Some(10));
    assert_eq!(ed.end_min - ed.start_min, 45);
    // New Items › All Day Event.
    super::run_view_cmd(
        h.state_mut(),
        &mut st,
        "calendar.newAppointment",
        json!({"form": true, "allDay": true}),
    );
    let ed = st.editors.appt.clone().unwrap();
    assert!(ed.all_day && ed.recurrence_dlg.is_none());
    assert_eq!(ed.start_date, ed.end_date);
    assert_eq!(ed.reminder, Some(engine_cal::ALL_DAY_REMINDER_MIN));
    // Double-clicking a slot takes the defaults too.
    super::forms::new_at(h.state(), &mut st, None, 20_000, 9 * 60, false);
    let ed = st.editors.appt.clone().unwrap();
    assert_eq!((ed.start_min, ed.end_min, ed.reminder), (540, 585, Some(10)));
    h.state_mut().s.settings.set("calendar.defaultReminders", &json!(false)).unwrap();
    super::forms::new_at(h.state(), &mut st, None, 20_000, 0, true);
    assert_eq!(st.editors.appt.clone().unwrap().reminder, None);
    store(&h.ctx, st);
    h.run_ok();
    assert_eq!(h.state().s.mailbox.appointments.len(), before, "nothing saved");
}

#[test]
fn share_and_view_windows_are_the_calendars_own() {
    let mut h = harness(app());
    for (id, title) in [
        ("calendar.share", "Share Calendar"),
        ("view.manageViews", "Manage All Views"),
        ("view.saveViewAs", "Save View As"),
        ("view.applyViewTo", "Apply View"),
    ] {
        let mut st = load(&h.ctx);
        let out = super::run_view_cmd(h.state_mut(), &mut st, id, json!({}));
        store(&h.ctx, st);
        assert!(out.is_some(), "{id}");
        h.run_ok();
        assert!(has(&h, title), "{id} opens {title}");
        assert!(h.state().ui.dialogs.is_empty(), "{id}: no generic window");
    }
    assert!(has_part(&h, "Free and busy times only"), "the share window's detail note");
    assert!(has_part(&h, "All Calendar folders"), "where each view can be used");
    assert!(has(&h, "New..."), "Manage Views can make a view");
}

#[test]
fn saved_view_from_the_save_window_is_listed() {
    let mut h = harness(app());
    let mut st = load(&h.ctx);
    super::run_view_cmd(h.state_mut(), &mut st, "view.saveViewAs", json!({"name": "Planning"}));
    store(&h.ctx, st);
    let names = engine_cal::views::view_names(&h.state().s);
    assert_eq!(names.last().map(String::as_str), Some("Planning"));
    let mut st = load(&h.ctx);
    super::run_view_cmd(h.state_mut(), &mut st, "view.manageViews", json!({}));
    store(&h.ctx, st);
    h.run_ok();
    assert!(has(&h, "Planning"));
}
