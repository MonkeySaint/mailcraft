//! Calendar depth: colours, module views, time zones, drag/resize, internet calendars, From File, blank
//! calendars, sharing details, the Daily Task List.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use serde_json::{Value, json};

use super::tests::{role, session};
use super::*;
use crate::Module;
use mailcraft_model::{FolderRole, Task};

fn cal(s: &Session) -> FolderId {
    role(s, FolderRole::Calendar)
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn ics(events: &[(&str, &str, &str)], extra: &str) -> String {
    let mut t = format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n{extra}");
    for (uid, start, summary) in events {
        t.push_str(&format!(
            "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:{start}\r\nDURATION:PT1H\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\n"
        ));
    }
    t.push_str("END:VCALENDAR\r\n");
    t
}

/// A one-thread HTTP server on 127.0.0.1 answering `count` requests with `respond(request head)`.
fn serve<F>(count: usize, respond: F) -> (String, mpsc::Receiver<String>, std::thread::JoinHandle<()>)
where
    F: Fn(&str) -> Vec<u8> + Send + 'static,
{
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let t = std::thread::spawn(move || {
        for _ in 0..count {
            let (sock, _) = l.accept().unwrap();
            let mut r = BufReader::new(sock.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            let resp = respond(&head);
            let _ = tx.send(head);
            let mut sock = sock;
            sock.write_all(&resp).unwrap();
        }
    });
    (format!("http://127.0.0.1:{port}"), rx, t)
}

fn http(status: &str, headers: &str, body: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

// ---------------------------------------------------------------------------------------------------------
// Colours

#[test]
fn calendar_colours_are_stored_and_automatic_follows_position() {
    let mut s = session();
    let main = cal(&s);
    let team = s.mailbox.create_folder(main, "Team").unwrap();
    assert_eq!(calendar_color(&s.mailbox, main).0, "Blue");
    assert_eq!(calendar_color(&s.mailbox, team).0, "Green");
    let r = run(&mut s, "calendar.color", json!({"color": "teal", "folder": team.0}));
    assert_eq!(r["shown"], "Teal");
    assert_eq!(r["rgb"], "#2a8c8c");
    assert_eq!(props::read(&s.mailbox, team).color.as_deref(), Some("Teal"));
    assert_eq!(calendar_color(&s.mailbox, team).0, "Teal");
    // Undo restores Automatic.
    s.undo_last().unwrap();
    assert_eq!(calendar_color(&s.mailbox, team).0, "Green");
    run(&mut s, "calendar.color", json!({"color": "Dark Red", "folder": team.0}));
    run(&mut s, "calendar.color", json!({"color": "Automatic", "folder": team.0}));
    assert_eq!(props::read(&s.mailbox, team).color, None);
    // Damaged stored properties never break the colour.
    s.mailbox.folders.get_mut(&team).unwrap().remote_name = Some("mailcraft-calendar:{]".into());
    assert_eq!(calendar_color(&s.mailbox, team).0, "Green");
}

#[test]
fn undo_restores_calendar_properties_with_a_server_account() {
    // With an IMAP account, undo carries synchronised folder state over; calendar properties are not that.
    let mut s = session();
    if let Some(a) = s.mailbox.accounts.first_mut() {
        a.kind = mailcraft_model::AccountKind::Imap;
    }
    let main = cal(&s);
    run(&mut s, "calendar.color", json!({"color": "Teal", "folder": main.0}));
    assert_eq!(calendar_color(&s.mailbox, main).0, "Teal");
    s.undo_last().unwrap();
    assert_eq!(props::read(&s.mailbox, main).color, None, "undo puts Automatic back");
    s.redo_last().unwrap();
    assert_eq!(props::read(&s.mailbox, main).color.as_deref(), Some("Teal"));
}

// ---------------------------------------------------------------------------------------------------------
// Module views

#[test]
fn module_view_only_answers_in_the_calendar() {
    let mut s = session();
    s.view.module = Module::Mail;
    assert!(module_view(&mut s, "view.changeView", &json!({"name": "List"})).is_none());
    s.view.module = Module::Calendar;
    assert!(module_view(&mut s, "view.zoom", &json!({})).is_none());
    assert!(module_view(&mut s, "view.changeView", &json!({"name": "List"})).is_some());
}

#[test]
fn change_view_arrange_reading_pane_and_reset() {
    let mut s = session();
    s.view.module = Module::Calendar;
    let mv = |s: &mut Session, id: &str, p: Value| module_view(s, id, &p).unwrap();
    let r = mv(&mut s, "view.changeView", json!({"name": "list"})).unwrap();
    assert_eq!(r["view"]["current"], "list");
    let v = r["view"].clone();
    assert!(mv(&mut s, "view.changeView", json!({"name": "Gantt", "view": v})).is_err());
    // Arrange By a column sorts the table; the same column again flips the order.
    let r = mv(&mut s, "view.arrangeBy", json!({"key": "Subject", "view": v})).unwrap();
    assert_eq!((r["view"]["sort"].as_str(), r["view"]["ascending"].as_bool()), (Some("subject"), Some(true)));
    let r = mv(&mut s, "view.arrangeBy", json!({"key": "subject", "view": r["view"]})).unwrap();
    assert_eq!(r["view"]["ascending"], false);
    let r = mv(&mut s, "view.arrangeBy", json!({"key": "start", "order": "descending", "view": r["view"]}))
        .unwrap();
    assert_eq!((r["view"]["sort"].as_str(), r["view"]["ascending"].as_bool()), (Some("start"), Some(false)));
    assert!(mv(&mut s, "view.arrangeBy", json!({"key": "colour"})).is_err());
    // An arrangement returns to the calendar grid.
    let r = mv(&mut s, "view.arrangeBy", json!({"key": "Work Week", "view": r["view"]})).unwrap();
    assert_eq!(
        (r["view"]["mode"].as_str(), r["view"]["current"].as_str()),
        (Some("workWeek"), Some("calendar"))
    );
    let r = mv(&mut s, "view.readingPane", json!({"position": "bottom", "view": r["view"]})).unwrap();
    assert_eq!(r["view"]["readingPane"], "Bottom");
    assert!(mv(&mut s, "view.readingPane", json!({"position": "left"})).is_err());
    let r = mv(&mut s, "view.tighterSpacing", json!({"view": r["view"]})).unwrap();
    assert_eq!(r["view"]["tighterSpacing"], true);
    let r = mv(&mut s, "view.todoBar", json!({"part": "tasks", "view": r["view"]})).unwrap();
    assert_eq!(r["todoBar"], "tasks");
    assert!(mv(&mut s, "view.todoBar", json!({"part": "mail"})).is_err());
    let d = mv(&mut s, "view.viewSettings", json!({"view": r["view"]})).unwrap();
    assert_eq!(d["dialog"], views::VIEW_SETTINGS_DIALOG);
    let r = mv(
        &mut s,
        "view.viewSettings",
        json!({"view": r["view"], "timeScale": 15, "sort": "location", "ascending": false, "dailyTaskList": "off"}),
    )
    .unwrap();
    assert_eq!(r["view"]["timeScale"], 15);
    assert_eq!(r["view"]["sort"], "location");
    assert_eq!(r["view"]["dailyTaskList"], "off");
    assert!(mv(&mut s, "view.viewSettings", json!({"timeScale": 7})).is_err());
    let date = r["view"]["date"].clone();
    let r = mv(&mut s, "view.resetView", json!({"view": r["view"]})).unwrap();
    assert_eq!(r["view"]["mode"], "month");
    assert_eq!(r["view"]["current"], "calendar");
    assert_eq!(r["view"]["readingPane"], "Off");
    assert_eq!(r["view"]["date"], date, "Reset View keeps the date");
}

#[test]
fn views_are_saved_per_calendar_with_rising_numbers() {
    let mut s = session();
    s.view.module = Module::Calendar;
    assert!(saved_view(&s).is_none());
    run(&mut s, "calendar.arrange", json!({"mode": "week"}));
    let (v1, n1) = saved_view(&s).unwrap();
    assert_eq!(v1.mode, CalendarMode::Week);
    assert!(v1.date.is_empty(), "the date is not saved");
    assert!(n1 > 0);
    // Without `view`, a View-tab command starts from the saved view (a ribbon button run directly).
    let r = module_view(&mut s, "view.changeView", &json!({"name": "Preview"})).unwrap().unwrap();
    assert_eq!(r["view"]["mode"], "week");
    let (v2, n2) = saved_view(&s).unwrap();
    assert_eq!(v2.current, CalendarCurrentView::Preview);
    assert!(n2 > n1);
    // Not an undo step.
    assert!(s.undo.undo.is_empty());
    // Another calendar has its own view.
    let team = s.mailbox.create_folder(cal(&s), "Team").unwrap();
    s.selection.folder = team;
    assert!(saved_view(&s).is_none());
}

#[test]
fn reset_view_is_enabled_once_the_calendar_view_changes() {
    let mut s = session();
    s.view.module = Module::Calendar;
    s.selection.folder = cal(&s);
    assert!(s.enabled("view.resetView").is_some(), "the default view has nothing to reset");
    // Moving to another day is not a view change.
    run(&mut s, "calendar.goToDate", json!({"date": "2026-11-02"}));
    assert!(s.enabled("view.resetView").is_some());
    run(&mut s, "calendar.arrange", json!({"mode": "week"}));
    assert_eq!(s.enabled("view.resetView"), None, "an arrangement change enables Reset View");
    run(&mut s, "view.resetView", json!({}));
    assert!(s.enabled("view.resetView").is_some(), "reset to the defaults again");
    run(&mut s, "view.viewSettings", json!({"timeScale": 15}));
    assert_eq!(s.enabled("view.resetView"), None, "a time scale change enables Reset View");
    run(&mut s, "view.resetView", json!({}));
    run(&mut s, "view.viewSettings", json!({"gridLines": false}));
    assert_eq!(s.enabled("view.resetView"), None, "a View Settings change enables Reset View");
    // Other Settings of the calendar views: end times in Month view, Date Navigator marks.
    run(&mut s, "view.resetView", json!({}));
    let r = run(&mut s, "view.viewSettings", json!({"monthEndTime": true, "navMarks": false}));
    assert_eq!(
        (r["view"]["monthEndTime"].as_bool(), r["view"]["navMarks"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(s.enabled("view.resetView"), None);
    let r = run(&mut s, "view.resetView", json!({}));
    assert_eq!(
        (r["view"]["monthEndTime"].as_bool(), r["view"]["navMarks"].as_bool()),
        (Some(false), Some(true))
    );
    assert!(s.run("view.viewSettings", &json!({"navMarks": "maybe"})).is_err());
    // Next 7 Days shows the Day arrangement.
    run(&mut s, "calendar.next7Days", json!({}));
    let (v, _) = saved_view(&s).unwrap();
    assert_eq!(v.mode, CalendarMode::Day);
    assert_eq!(s.enabled("view.resetView"), None, "Next 7 Days shows the Day arrangement");
}

// ---------------------------------------------------------------------------------------------------------
// Time zones

#[test]
fn named_time_zone_and_second_zone() {
    let mut s = session();
    let d = run(&mut s, "calendar.changeTimeZone", json!({}));
    assert_eq!(d["dialog"], "timeZone");
    // 2026-10-09: Toronto is on daylight time.
    let r = run(&mut s, "calendar.changeTimeZone", json!({"zone": "america/toronto", "label": "Home"}));
    assert_eq!(r["offsetMin"], -240);
    assert_eq!(s.tz_offset_min, -240);
    assert_eq!(r["timeZones"]["zone"], "America/Toronto");
    assert!(matches!(
        s.run("calendar.changeTimeZone", &json!({"zone": "Mars/Base"})),
        Err(CmdError::Params(_))
    ));
    assert!(matches!(
        s.run("calendar.changeTimeZone", &json!({"showSecond": true})),
        Err(CmdError::Params(_))
    ));
    run(&mut s, "calendar.changeTimeZone", json!({"showSecond": true, "secondZone": "Europe/Paris"}));
    let now = s.now().unix;
    assert_eq!(second_zone(&mut s, now), Some(("Paris".to_owned(), 120)));
    assert_eq!(views::primary_zone_label(&s), "Home");
    // In January both are on standard time.
    let jan = pim::parse_local("2027-01-15T12:00", 0).unwrap().unix;
    assert_eq!(second_zone(&mut s, jan).map(|z| z.1), Some(60));
    // Swap.
    let r = run(&mut s, "calendar.changeTimeZone", json!({"swap": true}));
    assert_eq!(r["timeZones"]["zone"], "Europe/Paris");
    assert_eq!(s.tz_offset_min, 120);
    // After a DST change the display offset follows the zone.
    s.tz_offset_min = 0;
    run(&mut s, "calendar.today", json!({}));
    assert_eq!(s.tz_offset_min, 120);
    // A bare offset that disagrees drops the named zone.
    let r = run(&mut s, "calendar.changeTimeZone", json!({"offsetMin": -300}));
    assert_eq!(r["timeZones"]["zone"], "");
    assert_eq!(s.tz_offset_min, -300);
}

#[test]
fn a_third_time_zone_shows_with_the_second() {
    let mut s = session();
    let now = s.now().unix;
    assert!(third_zone(&mut s, now).is_none());
    // A third zone needs a second one and a known zone.
    let only_third = json!({"showThird": true, "thirdZone": "Asia/Tokyo"});
    assert!(s.run("calendar.changeTimeZone", &only_third).is_err());
    assert!(s.run("calendar.changeTimeZone", &json!({"showSecond": true, "secondZone": "Europe/Paris", "showThird": true, "thirdZone": "Nowhere/City"})).is_err());
    let r = run(
        &mut s,
        "calendar.changeTimeZone",
        json!({"showSecond": true, "secondZone": "Europe/Paris", "showThird": true, "thirdZone": "Asia/Tokyo", "thirdLabel": "Tokyo office"}),
    );
    assert_eq!(r["timeZones"]["thirdZone"], "Asia/Tokyo");
    assert_eq!(third_zone(&mut s, now), Some(("Tokyo office".to_owned(), 540)));
    // Without a label the zone's city names it; hiding the second zone hides the third.
    run(&mut s, "calendar.changeTimeZone", json!({"thirdLabel": ""}));
    assert_eq!(third_zone(&mut s, now).map(|z| z.0), Some("Tokyo".to_owned()));
    run(&mut s, "calendar.changeTimeZone", json!({"showSecond": false, "showThird": false}));
    assert!(third_zone(&mut s, now).is_none());
}

#[test]
fn a_named_zone_sets_the_offset_when_the_profile_opens() {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::tempdir_in(base).unwrap();
    {
        let mut s = Session::open(dir.path()).unwrap();
        // Tokyo has no daylight saving time: +09:00 all year.
        let r = run(&mut s, "calendar.changeTimeZone", json!({"zone": "Asia/Tokyo"}));
        assert_eq!(r["offsetMin"], 540);
        s.run("file.exit", &json!({})).unwrap();
    }
    // The saved numeric offset is stale (as after a DST change while MailCraft was closed).
    let path = dir.path().join("settings.json");
    let mut f: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    f["settings"]["calendar.timeZone"] = json!(0);
    std::fs::write(&path, serde_json::to_vec(&f).unwrap()).unwrap();
    let s = Session::open(dir.path()).unwrap();
    assert_eq!(s.tz_offset_min, 540, "the named zone wins over the stored offset");
}

#[test]
fn appointments_in_a_named_zone_keep_wall_clock_time_across_dst() {
    let mut s = session();
    // Weekly 09:00 Toronto from Monday 2026-10-26 (DST ends Sunday 2026-11-01).
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"subject": "Standup", "start": "2026-10-26T09:00", "durationMin": 30,
               "timeZone": "America/Toronto", "recurrence": "FREQ=WEEKLY;COUNT=3"}),
    );
    assert_eq!(r["timeZone"], "America/Toronto");
    let id = ItemId(r["id"].as_u64().unwrap());
    let a = s.mailbox.appointments[&id].clone();
    assert_eq!(a.start.unix.rem_euclid(86_400), 13 * 3600, "09:00 EDT = 13:00 UTC");
    assert_eq!(a.start.offset_min, -240);
    let from = pim::parse_local("2026-10-01", 0).unwrap();
    let to = pim::parse_local("2026-12-01", 0).unwrap();
    let occ = occurrences(&a, from, to, 10);
    assert_eq!(occ.len(), 3);
    assert_eq!(occ[1].0.unix.rem_euclid(86_400), 14 * 3600, "09:00 EST = 14:00 UTC");
    assert_eq!(occ[1].0.offset_min, -300);
    // A deleted occurrence (EXDATE) uses the zoned instant.
    run(&mut s, "calendar.deleteOccurrence", json!({"id": id.0, "occurrence": occ[1].0.unix}));
    assert_eq!(occurrences(&s.mailbox.appointments[&id], from, to, 10).len(), 2);
    // Unknown zones are refused; null clears.
    assert!(s.run("calendar.update", &json!({"id": id.0, "timeZone": "Nowhere/Land"})).is_err());
    run(&mut s, "calendar.update", json!({"id": id.0, "timeZone": null}));
    assert_eq!(s.mailbox.appointments[&id].tz, None);
}

#[test]
fn the_calendar_zone_is_the_clock_for_new_items_and_wall_clock_input() {
    // Today is 2026-10-09 (EDT); the calendar is in Toronto, where DST ends on 2026-11-01.
    let mut s = session();
    run(&mut s, "calendar.changeTimeZone", json!({"zone": "America/Toronto"}));
    assert_eq!(s.tz_offset_min, -240);
    // A weekly meeting made the ordinary way (no `timeZone`) takes the calendar's zone.
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"subject": "Standup", "start": "2026-10-26T09:00", "durationMin": 30, "recurrence": "FREQ=WEEKLY;COUNT=3"}),
    );
    assert_eq!(r["timeZone"], "America/Toronto");
    let id = ItemId(r["id"].as_u64().unwrap());
    let a = s.mailbox.appointments[&id].clone();
    let from = pim::parse_local("2026-10-01", 0).unwrap();
    let to = pim::parse_local("2026-12-01", 0).unwrap();
    let occ = occurrences(&a, from, to, 10);
    assert_eq!(occ[1].0.unix.rem_euclid(86_400), 14 * 3600, "still 09:00 after the change (EST)");
    let list = run(&mut s, "calendar.occurrences", json!({"id": id.0}));
    assert_eq!(list["occurrences"][1]["start"], "2026-11-02T09:00", "{list}");
    // A single appointment after the change: typed 09:00, stored as 09:00 EST, shown as typed.
    let r =
        run(&mut s, "calendar.newAppointment", json!({"subject": "Dentist", "start": "2026-11-16T09:00"}));
    assert_eq!(r["start"], "2026-11-16T09:00", "{r}");
    let one = ItemId(r["id"].as_u64().unwrap());
    assert_eq!(s.mailbox.appointments[&one].start.unix.rem_euclid(86_400), 14 * 3600);
    // Editing it with the times it shows does not move it.
    let r = run(
        &mut s,
        "calendar.update",
        json!({"id": one.0, "start": "2026-11-16T09:00", "end": "2026-11-16T09:30"}),
    );
    assert_eq!(r["updated"][0]["start"], "2026-11-16T09:00");
    assert_eq!(s.mailbox.appointments[&one].start.unix.rem_euclid(86_400), 14 * 3600);
    // Dragged by wall-clock text, and edited as one occurrence of the series.
    let r = run(&mut s, "calendar.moveItem", json!({"id": one.0, "start": "2026-11-17T10:00"}));
    assert_eq!(r["start"], "2026-11-17T10:00");
    let r = run(
        &mut s,
        "calendar.editOccurrence",
        json!({"id": id.0, "occurrence": "2026-11-02T09:00", "subject": "Moved", "start": "2026-11-02T11:00", "end": "2026-11-02T11:30"}),
    );
    assert_eq!(r["start"], "2026-11-02T11:00", "{r}");
    // All-day items span the calendar's days: Sunday 2026-11-01 is 25 hours long.
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"subject": "Fall back", "start": "2026-11-01", "allDay": true}),
    );
    let day = ItemId(r["id"].as_u64().unwrap());
    let d = &s.mailbox.appointments[&day];
    assert_eq!(d.tz, None, "all-day items float");
    assert_eq!(d.end.unix - d.start.unix, 25 * 3600);
    assert_eq!(
        (r["start"].as_str(), r["end"].as_str()),
        (Some("2026-11-01T00:00"), Some("2026-11-02T00:00"))
    );
    // Moved a week earlier it is 24 hours again, still midnight to midnight.
    let r = run(&mut s, "calendar.moveItem", json!({"id": day.0, "start": "2026-10-25T12:00"}));
    assert_eq!(
        (r["start"].as_str(), r["end"].as_str()),
        (Some("2026-10-25T00:00"), Some("2026-10-26T00:00"))
    );
    let d = &s.mailbox.appointments[&day];
    assert_eq!(d.end.unix - d.start.unix, 24 * 3600);
    // Without a named zone nothing changes: the fixed offset is the clock and new items name no zone.
    let mut f = session();
    let r = run(&mut f, "calendar.newAppointment", json!({"subject": "x", "start": "2026-11-16T09:00"}));
    assert_eq!(r["timeZone"], Value::Null);
    assert_eq!(r["start"], "2026-11-16T09:00");
}

// ---------------------------------------------------------------------------------------------------------
// Drag and resize

#[test]
fn move_and_resize_with_undo() {
    let mut s = session();
    let r = run(&mut s, "calendar.newAppointment", json!({"subject": "Review", "start": "2026-10-12T10:00"}));
    let id = r["id"].as_u64().unwrap();
    let r = run(&mut s, "calendar.moveItem", json!({"id": id, "start": "2026-10-13T13:00"}));
    assert_eq!(
        (r["start"].as_str(), r["end"].as_str()),
        (Some("2026-10-13T13:00"), Some("2026-10-13T13:30"))
    );
    assert_eq!(r["sendUpdate"], false);
    let r = run(&mut s, "calendar.resize", json!({"id": id, "end": "2026-10-13T14:15"}));
    assert_eq!(r["end"], "2026-10-13T14:15");
    let r = run(&mut s, "calendar.resize", json!({"id": id, "start": "2026-10-13T12:45"}));
    assert_eq!(
        (r["start"].as_str(), r["end"].as_str()),
        (Some("2026-10-13T12:45"), Some("2026-10-13T14:15"))
    );
    assert!(s.run("calendar.resize", &json!({"id": id, "end": "2026-10-13T12:00"})).is_err());
    assert!(s.run("calendar.resize", &json!({"id": id})).is_err());
    assert!(s.run("calendar.moveItem", &json!({"id": id})).is_err());
    assert!(s.run("calendar.moveItem", &json!({"id": id, "start": "soon"})).is_err());
    s.undo_last().unwrap();
    s.undo_last().unwrap();
    s.undo_last().unwrap();
    let a = &s.mailbox.appointments[&ItemId(id)];
    assert_eq!(pim::datetime_string(a.start, 0), "2026-10-12T10:00");
    // To another calendar.
    let team = s.mailbox.create_folder(cal(&s), "Team").unwrap();
    run(&mut s, "calendar.moveItem", json!({"id": id, "start": "2026-10-12T11:00", "folder": team.0}));
    assert_eq!(s.mailbox.appointments[&ItemId(id)].folder, team);
}

#[test]
fn dragging_an_occurrence_makes_an_exception() {
    let mut s = session();
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"subject": "Daily", "start": "2026-10-12T09:00", "durationMin": 15, "recurrence": "FREQ=DAILY;COUNT=5"}),
    );
    let id = r["id"].as_u64().unwrap();
    assert!(s.run("calendar.moveItem", &json!({"id": id, "start": "2026-10-13T11:00"})).is_err());
    let r = run(
        &mut s,
        "calendar.moveItem",
        json!({"id": id, "occurrence": "2026-10-13T09:00", "start": "2026-10-13T11:00"}),
    );
    assert_eq!(r["series"], id);
    assert_eq!(r["start"], "2026-10-13T11:00");
    let ex = ItemId(r["id"].as_u64().unwrap());
    assert_ne!(ex.0, id);
    assert!(s.mailbox.appointments[&ex].recurrence_id.is_some());
    let series = &s.mailbox.appointments[&ItemId(id)];
    assert_eq!(series.recurrence.as_ref().unwrap().exdates.len(), 1);
    // No such occurrence.
    assert!(
        s.run(
            "calendar.moveItem",
            &json!({"id": id, "occurrence": "2026-10-13T10:00", "start": "2026-10-13T11:00"})
        )
        .is_err()
    );
    // One undo puts the series back whole.
    s.undo_last().unwrap();
    assert!(!s.mailbox.appointments.contains_key(&ex));
    assert!(s.mailbox.appointments[&ItemId(id)].recurrence.as_ref().unwrap().exdates.is_empty());
}

#[test]
fn all_day_items_move_by_whole_days() {
    let mut s = session();
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"subject": "Trip", "start": "2026-10-12", "allDay": true}),
    );
    let id = r["id"].as_u64().unwrap();
    let r = run(&mut s, "calendar.moveItem", json!({"id": id, "start": "2026-10-14T15:30"}));
    assert_eq!(
        (r["start"].as_str(), r["end"].as_str()),
        (Some("2026-10-14T00:00"), Some("2026-10-15T00:00"))
    );
    let r = run(&mut s, "calendar.resize", json!({"id": id, "end": "2026-10-16T10:00"}));
    assert_eq!(r["end"], "2026-10-17T00:00");
}

// ---------------------------------------------------------------------------------------------------------
// Internet calendars

#[test]
fn subscription_from_data_is_read_only() {
    let mut s = session();
    let data = ics(
        &[("a@example.org", "20261012T090000Z", "Holiday A")],
        "X-WR-CALNAME:Holidays\r\nX-PUBLISHED-TTL:PT12H\r\n",
    )
    .replace("END:VEVENT", "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT");
    let r = run(
        &mut s,
        "calendar.addCalendar",
        json!({"source": "internet", "url": "https://cal.example.org/h.ics", "data": data}),
    );
    assert_eq!(r["name"], "Holidays");
    assert_eq!(r["events"], 1);
    let f = FolderId(r["folder"].as_u64().unwrap());
    let p = props::read(&s.mailbox, f);
    assert!(p.read_only);
    assert_eq!(p.source, CalendarSource::Internet);
    assert_eq!(p.subscription.as_ref().unwrap().refresh_min, 12 * 60);
    let id = s.mailbox.appointments.values().find(|a| a.folder == f).unwrap().id;
    assert_eq!(s.mailbox.appointments[&id].reminder_min, None, "internet calendars do not remind");
    for (cmd, params) in [
        ("calendar.update", json!({"id": id.0, "subject": "x"})),
        ("calendar.delete", json!({"id": id.0})),
        ("calendar.moveItem", json!({"id": id.0, "start": "2026-10-12T10:00"})),
        ("calendar.categorize", json!({"id": id.0, "clear": true})),
        ("calendar.copy", json!({"id": id.0, "folder": f.0})),
    ] {
        assert!(matches!(s.run(cmd, &params), Err(CmdError::Disabled(_))), "{cmd}");
    }
    // Copying out of it is fine; new items made while it is selected go to the default calendar.
    let main = cal(&s);
    run(&mut s, "calendar.copy", json!({"id": id.0, "folder": main.0}));
    s.selection.folder = f;
    let r = run(&mut s, "calendar.newAppointment", json!({"subject": "Mine"}));
    assert_eq!(r["folder"], cal(&s).0);
    assert!(matches!(
        s.run("calendar.newAppointment", &json!({"subject": "Mine", "folder": f.0})),
        Err(CmdError::Disabled(_))
    ));
}

#[test]
fn subscribe_and_refresh_over_http_with_etag() {
    let v1 = ics(
        &[("a@example.org", "20261012T090000Z", "One"), ("b@example.org", "20261013T090000Z", "Two")],
        "",
    );
    let v2 = ics(
        &[("a@example.org", "20261012T100000Z", "One moved"), ("c@example.org", "20261014T090000Z", "Three")],
        "REFRESH-INTERVAL;VALUE=DURATION:PT30M\r\n",
    );
    let (base, seen, t) = serve(4, move |head| {
        if head.contains("/gone") {
            http("404 Not Found", "", "")
        } else if head.contains("/v2") {
            http("200 OK", "ETag: \"v2\"\r\nContent-Type: text/calendar\r\n", &v2)
        } else if head.to_ascii_lowercase().contains("if-none-match: \"v1\"") {
            http("304 Not Modified", "ETag: \"v1\"\r\n", "")
        } else {
            http("200 OK", "ETag: \"v1\"\r\nContent-Type: text/calendar\r\n", &v1)
        }
    });
    let mut s = session();
    let url = format!("{base}/cal.ics").replacen("http://", "webcal://", 1);
    let r = run(&mut s, "calendar.addCalendar", json!({"source": "internet", "url": url, "name": "Feed"}));
    assert_eq!(r["events"], 2, "{r}");
    let f = FolderId(r["folder"].as_u64().unwrap());
    assert_eq!(props::read(&s.mailbox, f).subscription.unwrap().etag.as_deref(), Some("\"v1\""));
    let first_req = seen.recv().unwrap();
    assert!(first_req.starts_with("GET /cal.ics HTTP/1.1"), "{first_req}");
    let a_id = s.mailbox.appointments.values().find(|a| a.uid == "a@example.org").unwrap().id;
    // Unchanged: 304.
    let r = run(&mut s, "calendar.refreshCalendar", json!({"folder": f.0}));
    assert_eq!(r["refreshed"][0]["notModified"], true, "{r}");
    assert!(seen.recv().unwrap().to_ascii_lowercase().contains("if-none-match: \"v1\""));
    // Changed: items replaced, ids kept for the same UID.
    props::update(&mut s.mailbox, f, |p| p.subscription.as_mut().unwrap().url = format!("{base}/v2"));
    let r = run(&mut s, "calendar.refreshCalendar", json!({"folder": f.0}));
    assert_eq!(r["refreshed"][0]["events"], 2, "{r}");
    let _ = seen.recv();
    let a = s.mailbox.appointments.values().find(|a| a.uid == "a@example.org").unwrap();
    assert_eq!(a.id, a_id);
    assert_eq!(a.subject, "One moved");
    assert!(!s.mailbox.appointments.values().any(|a| a.uid == "b@example.org"));
    let sub = props::read(&s.mailbox, f).subscription.unwrap();
    assert_eq!(sub.refresh_min, 30);
    assert_eq!(sub.last_error, None);
    // A failing server is noted, items kept.
    props::update(&mut s.mailbox, f, |p| p.subscription.as_mut().unwrap().url = format!("{base}/gone"));
    let r = run(&mut s, "calendar.refreshCalendar", json!({"folder": f.0}));
    assert!(r["refreshed"][0]["error"].as_str().unwrap().contains("404"), "{r}");
    assert!(props::read(&s.mailbox, f).subscription.unwrap().last_error.is_some());
    assert_eq!(s.mailbox.appointments.values().filter(|a| a.folder == f).count(), 2);
    t.join().unwrap();
}

#[test]
fn calendar_properties_change_colour_address_interval_and_read_only() {
    let mut s = session();
    let r = run(
        &mut s,
        "calendar.addCalendar",
        json!({"source": "internet", "url": "https://h.example.net/x.ics", "fetch": false}),
    );
    let f = FolderId(r["folder"].as_u64().unwrap());
    let data = ics(&[("z@example.net", "20261020T080000Z", "Z")], "");
    run(&mut s, "calendar.refreshCalendar", json!({"folder": f.0, "data": data, "etag": "\"e1\""}));
    assert!(subscribe::subscriptions(&s, false).is_empty(), "just fetched");
    // Colour and update interval (kept within the allowed range).
    let r = run(&mut s, "calendar.setProperties", json!({"folder": f.0, "color": "teal", "refreshMin": 5}));
    assert_eq!(r["color"], "Teal");
    assert_eq!(r["refreshMin"], props::MIN_REFRESH_MIN);
    assert!(r["lastUpdate"].is_i64());
    // A new address is downloaded on the next update (it is due at once), webcal is accepted.
    let r = run(
        &mut s,
        "calendar.setProperties",
        json!({"folder": f.0, "url": "webcal://feeds.example.org/b.ics"}),
    );
    assert_eq!(r["url"], "http://feeds.example.org/b.ics");
    assert!(r["lastUpdate"].is_null());
    assert_eq!(props::read(&s.mailbox, f).subscription.unwrap().etag, None);
    assert_eq!(subscribe::subscriptions(&s, false).len(), 1);
    assert!(s.run("calendar.setProperties", &json!({"folder": f.0, "url": "ftp://x.example.org/"})).is_err());
    // An internet calendar stays read-only.
    assert!(s.run("calendar.setProperties", &json!({"folder": f.0, "readOnly": false})).is_err());
    // Update Now queues the download for the UI's workers.
    s.sync.background_calendars = true;
    let r = run(&mut s, "calendar.setProperties", json!({"folder": f.0, "updateNow": true}));
    assert_eq!(r["update"], "updating");
    assert_eq!(s.sync.calendar_requests.len(), 1);
    // A local calendar can be made read-only (and back); its items then cannot change.
    let c = cal(&s);
    let a =
        run(&mut s, "calendar.newAppointment", json!({"subject": "Dentist", "start": "2026-10-12T09:00"}));
    assert!(
        s.run("calendar.setProperties", &json!({"folder": c.0, "url": "https://x.example.org/"})).is_err()
    );
    assert!(s.run("calendar.setProperties", &json!({"folder": c.0, "updateNow": true})).is_err());
    let r = run(&mut s, "calendar.setProperties", json!({"folder": c.0, "readOnly": true}));
    assert_eq!(r["readOnly"], true);
    let e = s.run("calendar.update", &json!({"id": a["id"], "subject": "Moved"})).unwrap_err();
    assert!(e.to_string().contains("read-only"), "{e}");
    run(&mut s, "calendar.setProperties", json!({"folder": c.0, "readOnly": false}));
    run(&mut s, "calendar.update", json!({"id": a["id"], "subject": "Moved"}));
    // Folder Properties shows the same details.
    run(&mut s, "folder.properties", json!({"folder": f.0}));
    let shown = s.notices.iter().rev().find_map(|n| match n {
        crate::Notice::OpenDialog(d, p) if d == "folderProperties" => Some(p.clone()),
        _ => None,
    });
    assert_eq!(shown.unwrap()["calendar"]["color"], "Teal");
    assert!(s.run("calendar.setProperties", &json!({"folder": f.0, "color": "plaid"})).is_err());
}

#[test]
fn refresh_with_a_worker_result_and_due_list() {
    let mut s = session();
    let r = run(
        &mut s,
        "calendar.addCalendar",
        json!({"source": "internet", "url": "https://h.example.net/x.ics", "fetch": false}),
    );
    let f = FolderId(r["folder"].as_u64().unwrap());
    assert_eq!(subscribe::subscriptions(&s, false).len(), 1, "never fetched = due");
    let data = ics(&[("z@example.net", "20261020T080000Z", "Z")], "");
    let r = run(&mut s, "calendar.refreshCalendar", json!({"folder": f.0, "data": data, "etag": "\"e1\""}));
    assert_eq!(r["refreshed"][0]["events"], 1);
    assert!(subscribe::subscriptions(&s, false).is_empty(), "just fetched");
    assert_eq!(subscribe::subscriptions(&s, true).len(), 1);
    // An hour later it is due again.
    s.fixed_now = Some(pim::add_secs(s.now(), 3600));
    assert_eq!(subscribe::subscriptions(&s, false).len(), 1);
    run(&mut s, "calendar.refreshCalendar", json!({"folder": f.0, "notModified": true}));
    assert!(subscribe::subscriptions(&s, false).is_empty());
    let r = run(&mut s, "calendar.refreshCalendar", json!({"folder": f.0, "error": "offline"}));
    assert_eq!(r["refreshed"][0]["error"], "offline");
    // A worker result needs a subscription folder; garbage data is noted, not fatal.
    assert!(s.run("calendar.refreshCalendar", &json!({"folder": cal(&s).0, "data": "x"})).is_err());
    let r = run(&mut s, "calendar.refreshCalendar", json!({"folder": f.0, "data": "not a calendar"}));
    assert!(r["refreshed"][0]["error"].is_string());
    assert_eq!(s.mailbox.appointments.values().filter(|a| a.folder == f).count(), 1);
}

#[test]
fn background_refresh_is_not_an_undo_step() {
    let mut s = session();
    let r = run(
        &mut s,
        "calendar.addCalendar",
        json!({"source": "internet", "url": "https://h.example.net/x.ics", "fetch": false}),
    );
    let f = FolderId(r["folder"].as_u64().unwrap());
    // The user's own last step: a new appointment.
    run(&mut s, "calendar.newAppointment", json!({"subject": "Mine", "start": "2026-10-20T09:00"}));
    let undo_before = s.undo.undo.len();
    let data = ics(&[("z@example.net", "20261020T080000Z", "Z")], "");
    let fetched = subscribe::Fetched::Data { body: data.into_bytes(), etag: None, last_modified: None };
    let out = subscribe::apply_background(&mut s, f, Ok(fetched));
    assert_eq!(out["events"], 1, "{out}");
    assert_eq!(s.undo.undo.len(), undo_before, "a refresh records no undo step");
    assert!(s.mailbox.appointments.values().any(|a| a.folder == f && a.subject == "Z"));
    // Undo undoes the appointment, not the refresh, and keeps what the refresh brought.
    let z = s.mailbox.appointments.values().find(|a| a.subject == "Z").unwrap().id;
    s.undo_last().unwrap();
    assert!(!s.mailbox.appointments.values().any(|a| a.subject == "Mine"));
    assert_eq!(s.mailbox.appointments.get(&z).map(|a| a.folder), Some(f), "the refreshed item stays");
    assert!(props::read(&s.mailbox, f).subscription.unwrap().last_update.is_some());
    s.redo_last().unwrap();
    assert!(s.mailbox.appointments.values().any(|a| a.subject == "Mine"));
    assert!(s.mailbox.appointments.contains_key(&z));
    // Undoing the subscription itself removes it with its items.
    s.undo_last().unwrap();
    s.undo_last().unwrap();
    assert!(s.mailbox.folder(f).is_none());
    assert!(!s.mailbox.appointments.contains_key(&z));
    s.redo_last().unwrap();
    // Errors are noted the same way; a non-subscription folder is refused.
    let out = subscribe::apply_background(&mut s, f, Err("offline".into()));
    assert_eq!(out["error"], "offline");
    let main = cal(&s);
    let out = subscribe::apply_background(&mut s, main, Ok(subscribe::Fetched::NotModified));
    assert!(out["error"].is_string());
}

#[test]
fn calendar_meta_reads_name_and_refresh() {
    let t = ics(&[], "NAME:Team\r\nREFRESH-INTERVAL;VALUE=DURATION:P1D\r\n");
    assert_eq!(subscribe::calendar_meta(t.as_bytes()), (Some("Team".into()), Some(24 * 60)));
    let t = ics(&[], "X-PUBLISHED-TTL:PT1M\r\n");
    assert_eq!(subscribe::calendar_meta(t.as_bytes()).1, Some(props::MIN_REFRESH_MIN));
    assert_eq!(subscribe::calendar_meta(b"junk"), (None, None));
}

// ---------------------------------------------------------------------------------------------------------
// From File, blank, other sources

#[test]
fn from_file_and_blank_calendars() {
    let mut s = session();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("team-events.ics");
    std::fs::write(&path, ics(&[("f1@example.com", "20261015T090000Z", "Offsite")], "")).unwrap();
    let r = run(&mut s, "calendar.addCalendar", json!({"source": "file", "path": path.to_string_lossy()}));
    assert_eq!((r["name"].as_str(), r["events"].as_u64()), (Some("team-events"), Some(1)));
    let f = FolderId(r["folder"].as_u64().unwrap());
    assert!(!props::is_read_only(&s.mailbox, f), "a file copy is editable");
    assert_eq!(props::read(&s.mailbox, f).source, CalendarSource::File);
    // Not a calendar: nothing created.
    let bad = dir.path().join("notes.ics");
    std::fs::write(&bad, "hello").unwrap();
    let before = s.mailbox.folders.len();
    assert!(
        s.run("calendar.addCalendar", &json!({"source": "file", "path": bad.to_string_lossy()})).is_err()
    );
    assert!(
        s.run(
            "calendar.addCalendar",
            &json!({"source": "file", "path": dir.path().join("missing.ics").to_string_lossy()})
        )
        .is_err()
    );
    assert!(
        s.run("calendar.addCalendar", &json!({"source": "file", "path": dir.path().to_string_lossy()}))
            .is_err()
    );
    assert_eq!(s.mailbox.folders.len(), before);
    assert_eq!(run(&mut s, "calendar.addCalendar", json!({"source": "file"}))["dialog"], "addCalendar");
    // Blank calendars; a taken name gets a number.
    assert_eq!(run(&mut s, "calendar.addCalendar", json!({"source": "blank"}))["dialog"], "addCalendar");
    let r = run(&mut s, "calendar.addCalendar", json!({"source": "blank", "name": "Kids"}));
    assert_eq!(r["name"], "Kids");
    let r = run(&mut s, "calendar.addCalendar", json!({"source": "blank", "name": "Kids"}));
    assert_eq!(r["name"], "Kids (2)");
    let kids = FolderId(r["folder"].as_u64().unwrap());
    assert_eq!(s.mailbox.folder(kids).unwrap().kind, FolderKind::Calendar);
    // Like Create New Folder, a blank calendar may go under a mail folder (it still holds calendar items).
    let inbox = role(&s, FolderRole::Inbox);
    let r = run(&mut s, "calendar.addCalendar", json!({"source": "blank", "name": "X", "parent": inbox.0}));
    let x = FolderId(r["folder"].as_u64().unwrap());
    assert_eq!(s.mailbox.folder(x).unwrap().kind, FolderKind::Calendar);
}

// ---------------------------------------------------------------------------------------------------------
// Sharing

#[test]
fn share_with_range_and_detail() {
    let mut s = session();
    run(
        &mut s,
        "calendar.newAppointment",
        json!({"subject": "Dentist", "location": "Clinic", "start": "2026-10-09T15:00"}),
    );
    run(&mut s, "calendar.newAppointment", json!({"subject": "Later", "start": "2026-12-01T15:00"}));
    let r = run(&mut s, "calendar.share", json!({"range": "today", "detail": "availability"}));
    assert_eq!(r["events"], 1);
    let c = r["compose"].as_u64().unwrap();
    let text = String::from_utf8(s.composes[&c].attachments[0].data.clone()).unwrap();
    assert!(text.contains("SUMMARY:Busy"), "{text}");
    assert!(!text.contains("Dentist") && !text.contains("Clinic"));
    let r = run(&mut s, "calendar.share", json!({"range": "next30", "detail": "limited"}));
    assert_eq!(r["events"], 1);
    let r = run(&mut s, "calendar.share", json!({}));
    assert_eq!(r["events"], 2);
    assert!(s.run("calendar.share", &json!({"range": "forever"})).is_err());
    assert!(s.run("calendar.share", &json!({"detail": "secret"})).is_err());
}

// ---------------------------------------------------------------------------------------------------------
// Daily Task List, work week

#[test]
fn daily_task_list_days() {
    let mut s = session();
    let folder = role(&s, FolderRole::Tasks);
    let tz = 0;
    let day = |t: &str| pim::parse_local(t, 0).unwrap().day_number(0);
    let mut add = |subject: &str, due: Option<&str>, done: bool| {
        let id = ItemId(s.mailbox.alloc_id());
        let t = Task {
            id,
            folder,
            subject: subject.into(),
            due: due.map(|d| pim::parse_local(d, 0).unwrap()),
            status: if done { mailcraft_model::TaskStatus::Completed } else { Default::default() },
            ..Default::default()
        };
        s.mailbox.tasks.insert(id, t);
        id
    };
    let overdue = add("Overdue", Some("2026-10-01"), false);
    let soon = add("Soon", Some("2026-10-12"), false);
    let _done = add("Done", Some("2026-10-12"), true);
    let _undated = add("Undated", None, false);
    let today = day("2026-10-09");
    let list = tasks_for_days(&s.mailbox, tz, today, day("2026-10-05"), day("2026-10-19"), 100);
    assert_eq!(list, vec![(today, overdue), (day("2026-10-12"), soon)]);
    assert!(tasks_for_days(&s.mailbox, tz, today, day("2026-11-01"), day("2026-11-08"), 100).is_empty());
    assert_eq!(tasks_for_days(&s.mailbox, tz, today, i64::MIN, i64::MAX, 1).len(), 1);
}

#[test]
fn work_week_and_hours() {
    let mut s = session();
    // Without the option the work week is Monday to Friday, 8 to 5.
    if views::option(&mut s, "calendar.workWeek").is_none() {
        assert_eq!(work_week(&mut s), vec![1, 2, 3, 4, 5]);
        assert_eq!(work_hours(&mut s), (8 * 60, 17 * 60));
    }
    assert_eq!(views::parse_work_week(&json!(["sun", "Monday", "TU", "sat", "mon"])), Some(vec![0, 1, 2, 6]));
    assert_eq!(views::parse_work_week(&json!("mon, wed;fri")), Some(vec![1, 3, 5]));
    assert_eq!(views::parse_work_week(&json!([0, 6, 9, -1])), Some(vec![0, 6]));
    assert_eq!(views::parse_work_week(&json!([])), None);
    assert_eq!(views::parse_work_week(&json!(["x", "m"])), None);
    assert_eq!(views::parse_work_week(&json!(5)), None);
}

// ---------------------------------------------------------------------------------------------------------
// Update Folder and Send/Receive on internet calendars, Create New Folder kinds, View Settings of the
// List / Active tables, Manage Views

fn subscribed(s: &mut Session, url: &str) -> FolderId {
    let r = run(
        s,
        "calendar.addCalendar",
        json!({"source": "internet", "url": url, "name": "Feed", "fetch": false}),
    );
    FolderId(r["folder"].as_u64().unwrap())
}

#[test]
fn update_folder_refreshes_an_internet_calendar() {
    let body = ics(&[("u1@example.org", "20261012T090000Z", "Standup")], "");
    let (base, seen, t) = serve(1, move |_| http("200 OK", "Content-Type: text/calendar\r\n", &body));
    let mut s = session();
    let f = subscribed(&mut s, &format!("{base}/team.ics"));
    s.selection.folder = f;
    let undo_before = s.undo.undo_label().map(str::to_owned);
    // Without a UI the command fetches straight away.
    let r = run(&mut s, "sendReceive.folder", json!({}));
    assert_eq!(r["status"], "updated", "{r}");
    assert_eq!(r["calendar"]["refreshed"], 1);
    assert!(seen.recv().unwrap().starts_with("GET /team.ics"));
    assert_eq!(s.mailbox.appointments.values().filter(|a| a.folder == f).count(), 1);
    // A refresh is synchronisation, not an editing step.
    assert_eq!(s.undo.undo_label().map(str::to_owned), undo_before);
    t.join().unwrap();
    // Offline: nothing is fetched.
    let r = run(&mut s, "sendReceive.folder", json!({"offline": true}));
    assert_eq!(r["status"], "offline");
    // A calendar that cannot be fetched is reported.
    props::update(&mut s.mailbox, f, |p| {
        if let Some(sub) = p.subscription.as_mut() {
            sub.url = "http://127.0.0.1:1/x.ics".into();
        }
    });
    let r = run(&mut s, "sendReceive.folder", json!({"folder": f.0}));
    assert_eq!(r["status"], "failed", "{r}");
    assert!(props::read(&s.mailbox, f).subscription.unwrap().last_error.is_some());
}

#[test]
fn send_receive_queues_calendar_refreshes_for_the_ui() {
    let mut s = session();
    s.sync.background_calendars = true;
    let a = subscribed(&mut s, "https://cal.example.com/a.ics");
    let b = subscribed(&mut s, "https://cal.example.com/b.ics");
    s.selection.folder = a;
    let r = run(&mut s, "sendReceive.folder", json!({}));
    assert_eq!(r["status"], "updating");
    assert_eq!(r["calendar"]["queued"], 1);
    // Send/Receive All Folders queues every internet calendar, each once.
    let r = run(&mut s, "sendReceive.all", json!({}));
    assert_eq!(r["calendars"]["queued"], 1, "{r}");
    let mut queued: Vec<FolderId> =
        crate::cmd::send_receive::take_calendar_requests(&mut s).into_iter().map(|d| d.folder).collect();
    queued.sort();
    assert_eq!(queued, vec![a, b]);
    assert!(crate::cmd::send_receive::take_calendar_requests(&mut s).is_empty());
    // A local calendar is not an internet calendar: the mail rules apply.
    s.selection.folder = cal(&s);
    let r = run(&mut s, "sendReceive.folder", json!({}));
    assert_eq!(r["status"], "local folder");
}

#[test]
fn new_folders_choose_what_they_contain() {
    let mut s = session();
    let inbox = role(&s, FolderRole::Inbox);
    s.selection.folder = inbox;
    // In the Calendar the dialog offers Calendar Items first.
    s.view.module = Module::Calendar;
    run(&mut s, "folder.new", json!({}));
    let Some(crate::Notice::OpenDialog(name, p)) = s.notices.last() else { panic!("no dialog") };
    assert_eq!((name.as_str(), p["kind"].as_str()), ("createFolder", Some("calendar")));
    assert_eq!(p["kinds"].as_array().unwrap().len(), 6);
    // A calendar under a mail folder, a task list under the calendar.
    let r = run(&mut s, "folder.new", json!({"name": "Rota", "parent": inbox.0, "kind": "calendar"}));
    let rota = FolderId(r["folder"].as_u64().unwrap());
    assert_eq!(s.mailbox.folder(rota).unwrap().kind, FolderKind::Calendar);
    let r = run(&mut s, "folder.new", json!({"name": "Chores", "parent": rota.0, "kind": "Task Items"}));
    assert_eq!(s.mailbox.folder(FolderId(r["folder"].as_u64().unwrap())).unwrap().kind, FolderKind::Tasks);
    // Without a kind the parent's is kept.
    let r = run(&mut s, "folder.new", json!({"name": "Sub", "parent": rota.0}));
    assert_eq!(s.mailbox.folder(FolderId(r["folder"].as_u64().unwrap())).unwrap().kind, FolderKind::Calendar);
    assert!(matches!(
        s.run("folder.new", &json!({"name": "X", "parent": inbox.0, "kind": "fax"})),
        Err(CmdError::Params(_))
    ));
    // Create New Blank Calendar can go under any folder.
    let r =
        run(&mut s, "calendar.addCalendar", json!({"source": "blank", "name": "Club", "parent": inbox.0}));
    let club = FolderId(r["folder"].as_u64().unwrap());
    assert_eq!(s.mailbox.folder(club).unwrap().parent, Some(inbox));
    assert_eq!(s.mailbox.folder(club).unwrap().kind, FolderKind::Calendar);
    assert!(
        s.run("calendar.addCalendar", &json!({"source": "blank", "name": "Y", "parent": 999_999})).is_err()
    );
}

#[test]
fn table_view_settings_round_trip() {
    let mut s = session();
    s.view.module = Module::Calendar;
    let r = run(
        &mut s,
        "view.viewSettings",
        json!({
            "columns": ["subject", "start", "location", "start"],
            "groupBy": "Location",
            "filter": {"words": "standup"},
            "rules": [{"name": "Team", "on": true, "filter": {"category": "Red category"}, "color": "#107C10"}],
            "gridLines": false,
            "labels": {"start": "Begins", "end": ""},
        }),
    );
    let v: CalendarViewState = serde_json::from_value(r["view"].clone()).unwrap();
    assert_eq!(v.columns, vec![CalendarColumn::Subject, CalendarColumn::Start, CalendarColumn::Location]);
    assert_eq!(v.group_by, Some(CalendarColumn::Location));
    assert_eq!(v.filter.words, "standup");
    assert_eq!(v.rules[0].color, "#107c10");
    assert!(v.no_grid);
    assert_eq!(v.column_label(CalendarColumn::Start), "Begins");
    assert_eq!(v.column_label(CalendarColumn::End), "End");
    // Saved on the calendar: the next command without a view starts from it.
    let r = run(&mut s, "view.viewSettings", json!({"groupBy": null, "gridLines": true}));
    let v: CalendarViewState = serde_json::from_value(r["view"].clone()).unwrap();
    assert_eq!((v.group_by, v.no_grid, v.filter.words.as_str()), (None, false, "standup"));
    for bad in [
        json!({"columns": ["start"]}),
        json!({"columns": "subject"}),
        json!({"groupBy": "size"}),
        json!({"filter": 3}),
        json!({"labels": {"nope": "x"}}),
        json!({"labels": {"start": 5}}),
    ] {
        assert!(matches!(s.run("view.viewSettings", &bad), Err(CmdError::Params(_))), "{bad}");
    }
    // Stored state is not trusted.
    let mut hostile = CalendarViewState {
        columns: vec![CalendarColumn::Start; 50],
        rules: vec![mailcraft_model::view::ItemRule::default(); 100],
        ..CalendarViewState::default()
    };
    hostile.labels.insert("bogus".into(), "x".into());
    hostile.sanitize_table();
    assert_eq!(hostile.columns, vec![CalendarColumn::Start]);
    assert!(hostile.labels.is_empty() && hostile.rules.is_empty());
}

#[test]
fn calendar_manage_views_new_modify_and_scope() {
    let mut s = session();
    s.view.module = Module::Calendar;
    let main = cal(&s);
    let team = s.mailbox.create_folder(main, "Team").unwrap();
    // New: a List view with the standard settings, for every calendar.
    run(&mut s, "view.manageViews", json!({"action": "new", "name": "Agenda", "base": "List"}));
    let r = run(&mut s, "view.changeView", json!({"name": "agenda"}));
    assert_eq!(r["view"]["current"], "list");
    // Modify: View Settings, then the saved view takes them.
    run(&mut s, "view.viewSettings", json!({"groupBy": "categories"}));
    run(&mut s, "view.manageViews", json!({"action": "update", "name": "Agenda"}));
    let (_, saved) = views::named_all(&s).into_iter().find(|(_, v)| v.name == "Agenda").unwrap();
    assert_eq!(saved.view.group_by, Some(CalendarColumn::Categories));
    assert!(
        views::describe(&saved.view).contains("grouped by Categories"),
        "{}",
        views::describe(&saved.view)
    );
    // A view for one calendar only.
    s.selection.folder = team;
    run(&mut s, "view.saveViewAs", json!({"name": "Team only", "scope": "folder"}));
    let rows = views::view_rows(&s);
    let team_row = rows.iter().find(|r| r["name"] == "Team only").unwrap();
    assert_eq!((team_row["scope"].as_str(), team_row["usedOn"].as_str()), (Some("folder"), Some("Team")));
    assert_eq!(rows.iter().find(|r| r["name"] == "Agenda").unwrap()["usedOn"], "All Calendar folders");
    s.selection.folder = main;
    assert!(!views::view_names(&s).iter().any(|n| n == "Team only"), "only on its own calendar");
    s.selection.folder = team;
    run(&mut s, "view.manageViews", json!({"action": "rename", "name": "Team only", "newName": "Crew"}));
    run(&mut s, "view.manageViews", json!({"action": "delete", "name": "Crew"}));
    assert!(!views::view_names(&s).iter().any(|n| n == "Crew"));
    assert!(s.run("view.manageViews", &json!({"action": "update", "name": "List"})).is_err());
    assert!(s.run("view.manageViews", &json!({"action": "new", "name": "X", "base": "Grid"})).is_err());
    assert!(s.run("view.saveViewAs", &json!({"name": "Y", "scope": "galaxy"})).is_err());
}

/// Regression: "This calendar" on the default calendar used to save the view for every calendar.
#[test]
fn calendar_view_for_the_default_calendar_alone() {
    let mut s = session();
    s.view.module = Module::Calendar;
    let main = cal(&s);
    let team = s.mailbox.create_folder(main, "Team").unwrap();
    s.selection.folder = main;
    run(&mut s, "view.saveViewAs", json!({"name": "Mine", "scope": "folder"}));
    let rows = views::view_rows(&s);
    let row = rows.iter().find(|r| r["name"] == "Mine").unwrap();
    assert_eq!(row["scope"], "folder");
    assert_ne!(row["usedOn"], "All Calendar folders");
    s.selection.folder = team;
    assert!(!views::view_names(&s).iter().any(|n| n == "Mine"), "not offered on other calendars");
    // A shared view and the default calendar's own view of the same name both survive.
    run(&mut s, "view.viewSettings", json!({"groupBy": "categories"}));
    run(&mut s, "view.manageViews", json!({"action": "new", "name": "Both", "base": "List"}));
    s.selection.folder = main;
    run(&mut s, "view.saveViewAs", json!({"name": "Both", "scope": "folder"}));
    let both: Vec<_> = views::view_rows(&s).into_iter().filter(|r| r["name"] == "Both").collect();
    assert_eq!(both.len(), 1);
    assert_eq!(both[0]["scope"], "folder", "the calendar's own wins here");
    s.selection.folder = team;
    let rows = views::view_rows(&s);
    assert_eq!(
        rows.iter().find(|r| r["name"] == "Both").unwrap()["scope"],
        "all",
        "others keep the shared one"
    );
    // Deleting the own view on the default calendar leaves the shared one.
    s.selection.folder = main;
    run(&mut s, "view.manageViews", json!({"action": "delete", "name": "Both"}));
    let rows = views::view_rows(&s);
    assert_eq!(rows.iter().find(|r| r["name"] == "Both").unwrap()["scope"], "all");
    // Renaming the own view keeps its scope.
    run(&mut s, "view.manageViews", json!({"action": "rename", "name": "Mine", "newName": "Still mine"}));
    let rows = views::view_rows(&s);
    assert_eq!(rows.iter().find(|r| r["name"] == "Still mine").unwrap()["scope"], "folder");
    assert!(
        s.run("view.manageViews", &json!({"action": "rename", "name": "Still mine", "newName": "Both"}))
            .is_err()
    );
}
