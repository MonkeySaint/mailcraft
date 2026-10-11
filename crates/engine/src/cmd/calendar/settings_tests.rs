//! Calendar: new appointment and meeting windows (nothing saved before Save / Send), File › Options ›
//! Calendar settings that change behaviour, Share Calendar ranges and privacy, automatic accept / decline,
//! and calendar views saved by name.

use serde_json::{Value, json};

use super::tests::{deliver, invite, person, role, session};
use super::*;
use crate::{Module, Notice};

fn last_dialog(s: &Session) -> (String, Value) {
    s.notices
        .iter()
        .rev()
        .find_map(|n| match n {
            Notice::OpenDialog(name, p) => Some((name.clone(), p.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no dialog in {:?}", s.notices))
}

fn set(s: &mut Session, key: &str, v: Value) {
    s.settings.set(key, &v).unwrap_or_else(|e| panic!("{key}: {e}"));
}

fn ics_of(s: &Session, compose: &Value) -> String {
    let c = &s.composes[&compose.as_u64().unwrap()];
    String::from_utf8(c.attachments[0].data.clone()).unwrap()
}

// ---------------------------------------------------------------------------------------------------------
// New appointment / meeting windows

#[test]
fn new_meeting_opens_a_prefilled_form_and_saves_nothing() {
    let mut s = session();
    let undo = s.undo.undo.len();
    let r = s.run("calendar.newMeeting", &json!({})).unwrap();
    assert_eq!(r["dialog"], MEETING_FORM);
    let (name, p) = last_dialog(&s);
    assert_eq!(name, MEETING_FORM);
    assert_eq!(p["meeting"], true);
    assert_eq!(p["start"], "2026-10-09T10:30");
    assert_eq!(p["end"], "2026-10-09T11:00");
    assert_eq!(p["reminderMin"], 15);
    assert!(s.mailbox.appointments.is_empty());
    assert!(s.composes.is_empty());
    s.run(
        "calendar.newMeeting",
        &json!({"subject": "Plan", "attendees": ["Ann Lee <ann@example.com>"], "optional": ["bob@example.org"],
                "body": "Agenda", "location": "Room 4"}),
    )
    .unwrap();
    let (_, p) = last_dialog(&s);
    assert_eq!(p["subject"], "Plan");
    assert_eq!(p["attendees"], json!(["Ann Lee <ann@example.com>"]));
    assert_eq!(p["to"], "Ann Lee <ann@example.com>");
    assert_eq!(p["optional"], json!(["bob@example.org"]));
    assert_eq!(p["body"], "Agenda");
    assert_eq!(p["location"], "Room 4");
    // Cancelling the window leaves nothing behind: nothing was saved, nothing to undo.
    assert!(s.mailbox.appointments.is_empty() && s.composes.is_empty());
    assert_eq!(s.undo.undo.len(), undo);
    // Bad fields are refused before any window opens.
    assert!(matches!(s.run("calendar.newMeeting", &json!({"attendees": 5})), Err(CmdError::Params(_))));
    assert!(matches!(s.run("calendar.newMeeting", &json!({"form": "yes"})), Err(CmdError::Params(_))));
    // The window's Send (a start) or `form: false` creates the meeting and its invitation.
    let r = s
        .run(
            "calendar.newMeeting",
            &json!({"subject": "Plan", "attendees": ["ann@example.com"], "form": false}),
        )
        .unwrap();
    assert!(r["id"].as_u64().is_some() && r["compose"].as_u64().is_some(), "{r}");
    let r = s
        .run(
            "calendar.newMeeting",
            &json!({"subject": "Sync", "start": "2026-10-12T09:00", "attendees": "ann@example.com"}),
        )
        .unwrap();
    assert!(r["compose"].as_u64().is_some(), "{r}");
    assert_eq!(s.mailbox.appointments.len(), 2);
    // `form: true` asks for the window even with a start.
    let n = s.mailbox.appointments.len();
    assert_eq!(
        s.run("calendar.newMeeting", &json!({"start": "2026-10-12T09:00", "form": true})).unwrap()["dialog"],
        MEETING_FORM
    );
    assert_eq!(last_dialog(&s).1["start"], "2026-10-12T09:00");
    assert_eq!(s.mailbox.appointments.len(), n);
}

#[test]
fn people_and_task_meetings_open_the_form() {
    let mut s = session();
    let c = s.run("people.new", &json!({"fullName": "Ann Lee", "email": "ann@example.com"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    let r = s.run("people.meeting", &json!({"ids": [c]})).unwrap();
    assert_eq!(r["dialog"], MEETING_FORM, "{r}");
    let (_, p) = last_dialog(&s);
    assert_eq!(p["to"], "Ann Lee <ann@example.com>");
    let t = s.run("tasks.new", &json!({"subject": "Budget"})).unwrap()["id"].as_u64().unwrap();
    s.selection.items = vec![ItemId(t)];
    s.selection.focus = Some(ItemId(t));
    let r = s.run("tasks.meeting", &json!({"ids": [t]})).unwrap();
    assert_eq!(r["dialog"], MEETING_FORM, "{r}");
    assert_eq!(last_dialog(&s).1["subject"], "Budget");
    assert!(s.mailbox.appointments.is_empty());
    assert!(s.composes.is_empty());
}

#[test]
fn new_appointment_form_for_all_day_and_recurring_items() {
    let mut s = session();
    // New Items › All Day Event / Recurring Event: the window, preset, nothing saved.
    let r =
        s.run("calendar.newAppointment", &json!({"form": true, "allDay": true, "recurring": true})).unwrap();
    assert_eq!(r["dialog"], APPOINTMENT_FORM);
    let (name, p) = last_dialog(&s);
    assert_eq!(name, APPOINTMENT_FORM);
    assert_eq!(p["allDay"], true);
    assert_eq!(p["recurring"], true);
    assert_eq!(p["meeting"], false);
    assert_eq!(p["start"], "2026-10-09T00:00");
    // The inclusive last day.
    assert_eq!(p["end"], "2026-10-09T23:59");
    assert_eq!(p["showAs"], "free");
    assert_eq!(p["reminderMin"], ALL_DAY_REMINDER_MIN);
    assert!(s.mailbox.appointments.is_empty());
    // A saved all-day event gets the same reminder; an explicit one wins.
    let r = s.run("calendar.newAppointment", &json!({"subject": "Off", "allDay": true})).unwrap();
    assert_eq!(r["reminderMin"], ALL_DAY_REMINDER_MIN);
    let r = s
        .run("calendar.newAppointment", &json!({"subject": "Off", "allDay": true, "reminderMin": 30}))
        .unwrap();
    assert_eq!(r["reminderMin"], 30);
}

// ---------------------------------------------------------------------------------------------------------
// File › Options › Calendar

#[test]
fn calendar_options_set_new_item_defaults() {
    let mut s = session();
    set(&mut s, "calendar.defaultDuration", json!(60));
    set(&mut s, "calendar.reminderMinutes", json!(5));
    let r = s.run("calendar.newAppointment", &json!({"subject": "A"})).unwrap();
    assert_eq!(r["end"], "2026-10-09T11:30");
    assert_eq!(r["reminderMin"], 5);
    // Shorten appointments and meetings: 10 minutes off an hour or longer, 5 off shorter ones.
    set(&mut s, "calendar.shorten", json!(true));
    assert_eq!(default_duration_min(&s), 50);
    set(&mut s, "calendar.defaultDuration", json!(30));
    assert_eq!(default_duration_min(&s), 25);
    set(&mut s, "calendar.defaultDuration", json!(5));
    assert_eq!(default_duration_min(&s), 5, "never shortened to nothing");
    set(&mut s, "calendar.defaultDuration", json!(30));
    // Default reminders off: new items have none.
    set(&mut s, "calendar.defaultReminders", json!(false));
    s.run("calendar.newAppointment", &json!({})).unwrap();
    let (_, p) = last_dialog(&s);
    assert_eq!(p["reminderMin"], Value::Null);
    assert_eq!(p["end"], "2026-10-09T10:55");
    let r = s.run("calendar.newAppointment", &json!({"subject": "B", "allDay": true})).unwrap();
    assert_eq!(r["reminderMin"], Value::Null);
}

#[test]
fn work_time_and_weeks_come_from_the_options() {
    let mut s = session();
    assert_eq!(work_week(&mut s), vec![1, 2, 3, 4, 5]);
    assert_eq!(work_hours(&mut s), (8 * 60, 17 * 60));
    set(&mut s, "calendar.workWeek", json!(["sun", "mon", "tue", "wed", "thu"]));
    assert_eq!(work_week(&mut s), vec![0, 1, 2, 3, 4]);
    set(&mut s, "calendar.workHours", json!("09:30-18:00"));
    assert_eq!(work_hours(&mut s), (570, 1080));
    assert_eq!(views::first_day_of_week(&s), 0);
    set(&mut s, "calendar.firstDayOfWeek", json!("mon"));
    assert_eq!(views::first_day_of_week(&s), 1);
    assert_eq!(views::first_week_rule(&s), "jan1");
    let day = |y, m, d| mailcraft_model::dates::days_from_civil(y, m, d).unwrap();
    // 2026-10-09 is a Friday.
    assert_eq!(views::week_start_on(day(2026, 10, 9), 1), day(2026, 10, 5));
    assert_eq!(views::week_start_on(day(2026, 10, 9), 0), day(2026, 10, 4));
    assert_eq!(views::week_start_on(day(2026, 10, 4), 1), day(2026, 9, 28));
    assert_eq!(views::week_start_on(day(2026, 10, 9), 5), day(2026, 10, 9));
    assert_eq!(views::week_start_on(day(2026, 10, 9), 6), day(2026, 10, 3));
    // Monday weeks, first four-day week (ISO 8601): 2026 has 53 weeks.
    assert_eq!(views::week_number(day(2026, 1, 1), 1, "first4"), 1);
    assert_eq!(views::week_number(day(2025, 12, 29), 1, "first4"), 1);
    assert_eq!(views::week_number(day(2026, 10, 9), 1, "first4"), 41);
    assert_eq!(views::week_number(day(2026, 12, 31), 1, "first4"), 53);
    assert_eq!(views::week_number(day(2027, 1, 3), 1, "first4"), 53);
    assert_eq!(views::week_number(day(2027, 1, 4), 1, "first4"), 1);
    // Starts on Jan 1 (Sunday weeks): the week holding January 1 is week 1, its December days too.
    assert_eq!(views::week_number(day(2026, 12, 31), 0, "jan1"), 1);
    assert_eq!(views::week_number(day(2026, 1, 3), 0, "jan1"), 1);
    assert_eq!(views::week_number(day(2026, 1, 4), 0, "jan1"), 2);
    // First full week: January 1–3, 2026 belong to the last week of 2025.
    assert_eq!(views::week_number(day(2026, 1, 4), 0, "firstFull"), 1);
    assert_eq!(views::week_number(day(2026, 1, 2), 0, "firstFull"), 52);
    // Hostile days never panic and stay in range.
    for d in [i64::MIN, -1, 0, i64::MAX] {
        let n = views::week_number(d, 3, "first4");
        assert!((1..=53).contains(&n));
        let _ = views::week_start_on(d, 200);
    }
}

#[test]
fn time_zone_label_from_the_options() {
    let mut s = session();
    assert_eq!(views::primary_zone_label(&s), "");
    set(&mut s, "calendar.timeZoneLabel", json!("Home"));
    assert_eq!(views::primary_zone_label(&s), "Home");
    // The Change Time Zone label wins.
    s.run("calendar.changeTimeZone", &json!({"label": "Office"})).unwrap();
    assert_eq!(views::primary_zone_label(&s), "Office");
}

#[test]
fn proposing_and_counter_options() {
    let mut org = session();
    let (_, cid) = invite(&mut org);
    assert!(!ics_of(&org, &json!(cid)).contains("DISALLOW-COUNTER"));
    set(&mut org, "calendar.allowProposeNewTime", json!(false));
    let (_, cid) = invite(&mut org);
    assert!(ics_of(&org, &json!(cid)).contains("X-MICROSOFT-DISALLOW-COUNTER:TRUE\r\nEND:VEVENT\r\n"));
    let mut ann = person("ann@example.com", "Ann Lee");
    let mail = deliver(&org, cid, &mut ann);
    set(&mut ann, "calendar.proposeResponse", json!("decline"));
    let r = ann
        .run(
            "calendar.proposeNewTime",
            &json!({"id": mail.0, "start": "2026-10-14T15:00", "end": "2026-10-14T16:00", "send": "none"}),
        )
        .unwrap();
    assert_eq!(r["response"], "declined");
}

// ---------------------------------------------------------------------------------------------------------
// Share Calendar

#[test]
fn share_custom_range_and_private_items() {
    let mut s = session();
    let new = |s: &mut Session, p: Value| {
        s.run("calendar.newAppointment", &p).unwrap();
    };
    new(&mut s, json!({"subject": "Doctor", "start": "2026-10-20T09:00", "private": true}));
    new(&mut s, json!({"subject": "Review", "start": "2026-10-21T09:00", "body": "Slides"}));
    new(&mut s, json!({"subject": "Later", "start": "2026-11-21T09:00"}));
    let r = s
        .run(
            "calendar.share",
            &json!({"range": "custom", "from": "2026-10-20", "to": "2026-10-21", "detail": "full"}),
        )
        .unwrap();
    assert_eq!(r["events"], 2);
    assert_eq!(r["from"], "2026-10-20T00:00");
    assert_eq!(r["to"], "2026-10-22T00:00");
    let text = ics_of(&s, &r["compose"]);
    assert!(text.contains("SUMMARY:Review") && text.contains("Slides"), "{text}");
    // A private item shares its busy time only.
    assert!(text.contains("SUMMARY:Busy") && !text.contains("Doctor"), "{text}");
    assert!(!text.contains("BEGIN:VALARM"), "{text}");
    let r = s
        .run(
            "calendar.share",
            &json!({"range": "custom", "from": "2026-10-20", "to": "2026-10-20", "detail": "limited", "includePrivate": true}),
        )
        .unwrap();
    assert_eq!(r["events"], 1);
    let text = ics_of(&s, &r["compose"]);
    assert!(text.contains("SUMMARY:Doctor"), "{text}");
    for bad in [
        json!({"range": "custom"}),
        json!({"range": "custom", "from": "2026-10-20"}),
        json!({"range": "custom", "from": "2026-10-21", "to": "2026-10-20"}),
        json!({"range": "custom", "from": "2026-10-20", "to": "9999-12-31"}),
        json!({"includePrivate": "yes"}),
    ] {
        assert!(matches!(s.run("calendar.share", &bad), Err(CmdError::Params(_))), "{bad}");
    }
}

// ---------------------------------------------------------------------------------------------------------
// Automatic accept or decline

#[test]
fn automatic_accept_and_decline() {
    let mut org = session();
    let (meeting, cid) = invite(&mut org);
    let mut ann = person("ann@example.com", "Ann Lee");
    let mail = deliver(&org, cid, &mut ann);
    // Off: nothing is answered.
    assert!(auto_respond_candidates(&ann).is_empty());
    assert_eq!(ann.run("calendar.autoRespond", &json!({"ids": [mail.0]})).unwrap()["handled"], json!([]));
    set(&mut ann, "calendar.autoAccept", json!(true));
    assert_eq!(auto_respond_candidates(&ann), vec![mail]);
    let r = ann.run("calendar.autoRespond", &json!({})).unwrap();
    assert_eq!(r["handled"][0]["action"], "accept", "{r}");
    let info = ann.run("calendar.meetingInfo", &json!({"id": mail.0})).unwrap();
    assert_eq!(info["myResponse"], "accepted", "{info}");
    // Answered and read: later passes leave it alone.
    assert!(auto_respond_candidates(&ann).is_empty());
    assert_eq!(ann.run("calendar.autoRespond", &json!({"ids": [mail.0]})).unwrap()["handled"], json!([]));

    // Declining requests that conflict.
    let mut bob = person("bob@example.org", "Bob");
    bob.run("calendar.newAppointment", &json!({"subject": "Dentist", "start": "2026-10-13T15:30"})).unwrap();
    let mail = deliver(&org, cid, &mut bob);
    set(&mut bob, "calendar.autoAccept", json!(true));
    set(&mut bob, "calendar.declineConflicts", json!(true));
    let r = bob.run("calendar.autoRespond", &json!({})).unwrap();
    assert_eq!(r["handled"][0]["action"], "decline", "{r}");
    let uid = org.mailbox.appointments[&meeting].uid.clone();
    assert!(find_by_uid(&bob, &uid).is_none());
    assert!(bob.mailbox.mail[&mail].read);

    // Declining recurring requests (only that option on).
    let r = org
        .run(
            "calendar.newMeeting",
            &json!({"subject": "Weekly", "start": "2026-10-14T10:00", "recurrence": "FREQ=WEEKLY", "attendees": ["cy@example.net"]}),
        )
        .unwrap();
    let mut cy = person("cy@example.net", "Cy");
    let mail = deliver(&org, r["compose"].as_u64().unwrap(), &mut cy);
    set(&mut cy, "calendar.declineRecurring", json!(true));
    let r = cy.run("calendar.autoRespond", &json!({"ids": [mail.0]})).unwrap();
    assert_eq!(r["handled"][0]["action"], "decline", "{r}");

    // With automatic accept on, a cancellation removes the meeting.
    let r = org.run("calendar.cancelMeeting", &json!({"id": meeting.0, "send": "edit"})).unwrap();
    let cancel = deliver(&org, r["compose"].as_u64().unwrap(), &mut ann);
    assert!(find_by_uid(&ann, &uid).is_some());
    let r = ann.run("calendar.autoRespond", &json!({})).unwrap();
    assert_eq!(r["handled"][0]["action"], "remove", "{r}");
    assert!(find_by_uid(&ann, &uid).is_none());
    assert!(ann.mailbox.mail[&cancel].read);
    // Junk ids are skipped.
    let r = ann.run("calendar.autoRespond", &json!({"ids": [u64::MAX, 0]})).unwrap();
    assert_eq!(r["handled"], json!([]));
    assert!(ann.run("calendar.autoRespond", &json!({"ids": "all"})).is_err());
}

// ---------------------------------------------------------------------------------------------------------
// Calendar views saved by name

#[test]
fn calendar_views_saved_by_name() {
    let mut s = session();
    s.view.module = Module::Calendar;
    let mv = |s: &mut Session, id: &str, p: Value| module_view(s, id, &p).unwrap();
    // Without details the calendar's own windows open.
    assert_eq!(mv(&mut s, "view.saveViewAs", json!({})).unwrap()["dialog"], views::SAVE_VIEW_DIALOG);
    let r = mv(&mut s, "view.manageViews", json!({})).unwrap();
    assert_eq!(r["dialog"], views::MANAGE_VIEWS_DIALOG);
    assert_eq!(r["views"], json!(["Calendar", "Preview", "List", "Active"]));
    assert_eq!(mv(&mut s, "view.applyViewTo", json!({})).unwrap()["dialog"], views::APPLY_VIEW_DIALOG);
    // Through the registry too (the View tab in the Calendar runs the calendar's handling).
    assert_eq!(s.run("view.saveViewAs", &json!({})).unwrap()["dialog"], views::SAVE_VIEW_DIALOG);

    let week = CalendarViewState {
        mode: CalendarMode::Week,
        time_scale: 15,
        date: "2026-10-09".into(),
        ..Default::default()
    };
    let r = mv(&mut s, "view.saveViewAs", json!({"name": "Planning", "view": week})).unwrap();
    assert_eq!(r["saved"], "Planning");
    for bad in ["List", "x\u{7}"] {
        assert!(mv(&mut s, "view.saveViewAs", json!({"name": bad, "view": week})).is_err(), "{bad}");
    }
    assert_eq!(
        mv(&mut s, "view.saveViewAs", json!({"name": "  "})).unwrap()["dialog"],
        views::SAVE_VIEW_DIALOG
    );
    assert!(mv(&mut s, "view.saveViewAs", json!({"name": "n".repeat(65)})).is_err());
    // Change View by its name brings it back, keeping the date shown.
    let other = CalendarViewState { date: "2026-12-01".into(), ..Default::default() };
    let r = mv(&mut s, "view.changeView", json!({"name": "planning", "view": other})).unwrap();
    let back: CalendarViewState = serde_json::from_value(r["view"].clone()).unwrap();
    assert_eq!((back.mode, back.time_scale, back.date.as_str()), (CalendarMode::Week, 15, "2026-12-01"));
    assert!(mv(&mut s, "view.changeView", json!({"name": "Nope"})).is_err());

    // Rename, copy, delete, reset.
    let names = |s: &Session| views::view_names(s);
    mv(&mut s, "view.manageViews", json!({"action": "rename", "name": "Planning", "newName": "Weekly"}))
        .unwrap();
    assert!(names(&s).contains(&"Weekly".to_owned()) && !names(&s).contains(&"Planning".to_owned()));
    mv(&mut s, "view.manageViews", json!({"action": "copy", "name": "List", "newName": "My list"})).unwrap();
    let r = mv(&mut s, "view.manageViews", json!({"action": "apply", "name": "My list"})).unwrap();
    assert_eq!(r["view"]["current"], json!(CalendarCurrentView::List));
    for bad in [
        json!({"action": "rename", "name": "List", "newName": "X"}),
        json!({"action": "rename", "name": "Weekly", "newName": "My list"}),
        json!({"action": "delete", "name": "Calendar"}),
        json!({"action": "delete", "name": "Missing"}),
        json!({"action": "reset", "name": "Weekly"}),
        json!({"action": "explode", "name": "Weekly"}),
        json!({"action": "copy", "name": "Weekly", "newName": ""}),
    ] {
        assert!(mv(&mut s, "view.manageViews", bad.clone()).is_err(), "{bad}");
    }
    // Named views survive other changes to the calendar's properties.
    s.run("calendar.color", &json!({"color": "Teal"})).unwrap();
    assert_eq!(names(&s), ["Calendar", "Preview", "List", "Active", "Weekly", "My list"]);
    mv(&mut s, "view.manageViews", json!({"action": "delete", "name": "weekly"})).unwrap();
    assert_eq!(names(&s), ["Calendar", "Preview", "List", "Active", "My list"]);
    let r = mv(&mut s, "view.manageViews", json!({"action": "reset", "name": "List", "view": week})).unwrap();
    let back: CalendarViewState = serde_json::from_value(r["view"].clone()).unwrap();
    assert_eq!(
        (back.mode, back.current, back.time_scale),
        (CalendarMode::Month, CalendarCurrentView::List, 30)
    );

    // Apply the current view to another calendar.
    let team = s.run("calendar.addCalendar", &json!({"source": "blank", "name": "Team"})).unwrap()["folder"]
        .as_u64()
        .unwrap();
    let r = mv(&mut s, "view.applyViewTo", json!({"folders": ["Team"], "view": week})).unwrap();
    assert_eq!(r["applied"], json!([team]));
    let saved = props::read(&s.mailbox, FolderId(team)).view.unwrap();
    assert_eq!((saved.mode, saved.date.as_str()), (CalendarMode::Week, ""));
    assert!(mv(&mut s, "view.applyViewTo", json!({"folders": ["Inbox"]})).is_err());
    assert!(mv(&mut s, "view.applyViewTo", json!({"folders": []})).is_err());
    let inbox = role(&s, mailcraft_model::FolderRole::Inbox);
    assert!(mv(&mut s, "view.applyViewTo", json!({"folders": [inbox.0]})).is_err());
}
