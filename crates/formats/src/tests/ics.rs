use mailcraft_mime::Mailbox;
use mailcraft_model::calendar::{AttendeeRole, ResponseStatus};
use mailcraft_model::{
    Appointment, Attendee, BusyStatus, DateTime, Recurrence, Task, TaskPriority, TaskStatus,
};
use proptest::prelude::*;

use crate::ics::{self, ImportOptions, Method, Rrule, itip, rrule, tz, value};

fn utc(y: i32, mo: u8, d: u8, h: u8, mi: u8) -> DateTime {
    DateTime::from_civil(y, mo, d, h, mi, 0, 0).unwrap()
}

fn at(y: i32, mo: u8, d: u8, h: u8, mi: u8, off: i32) -> DateTime {
    let t = DateTime::from_civil(y, mo, d, h, mi, 0, off).unwrap();
    DateTime::from_unix(t.unix, off)
}

const MEETING: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Test//EN\r\nMETHOD:REQUEST\r\n\
BEGIN:VTIMEZONE\r\nTZID:Eastern\r\nBEGIN:STANDARD\r\nDTSTART:16011104T020000\r\n\
RRULE:FREQ=YEARLY;BYDAY=1SU;BYMONTH=11\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nEND:STANDARD\r\n\
BEGIN:DAYLIGHT\r\nDTSTART:16010311T020000\r\nRRULE:FREQ=YEARLY;BYDAY=2SU;BYMONTH=3\r\nTZOFFSETFROM:-0500\r\n\
TZOFFSETTO:-0400\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:evt-1@example.com\r\nDTSTAMP:20261001T120000Z\r\nDTSTART;TZID=Eastern:20261012T140000\r\n\
DTEND;TZID=Eastern:20261012T153000\r\nSUMMARY:Design review\\, round 2\r\nLOCATION:Room 4\r\n\
DESCRIPTION:Agenda:\\n1. Specs\r\nORGANIZER;CN=\"Lee, Ann\":mailto:ann@example.com\r\n\
ATTENDEE;CN=Bob;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:bob@example.org\r\n\
ATTENDEE;ROLE=OPT-PARTICIPANT:mailto:cy@example.net\r\nATTENDEE;CUTYPE=ROOM:MAILTO:room4@example.com\r\n\
CATEGORIES:Work,Review\r\nCLASS:PRIVATE\r\nSEQUENCE:2\r\nRRULE:FREQ=WEEKLY;COUNT=10;BYDAY=MO\r\n\
EXDATE;TZID=Eastern:20261019T140000\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";

#[test]
fn reads_meeting_with_vtimezone() {
    let cal = ics::read(MEETING.as_bytes(), ImportOptions::default()).unwrap();
    assert_eq!(cal.method, Some(Method::Request));
    assert_eq!(cal.prodid.as_deref(), Some("-//Example//Test//EN"));
    let e = &cal.events[0];
    let a = &e.appointment;
    assert_eq!(a.uid, "evt-1@example.com");
    assert_eq!(a.subject, "Design review, round 2");
    assert_eq!(a.location, "Room 4");
    assert_eq!(a.body, "Agenda:\n1. Specs");
    assert_eq!(a.start, at(2026, 10, 12, 14, 0, -240));
    assert_eq!(a.end.unix - a.start.unix, 90 * 60);
    assert_eq!(a.reminder_min, Some(15));
    assert!(a.private && a.is_meeting && !a.all_day);
    assert_eq!(a.show_as, BusyStatus::Busy);
    assert_eq!(a.organizer, Some(Mailbox::new(Some("Lee, Ann"), "ann@example.com")));
    assert_eq!(a.attendees.len(), 3);
    assert_eq!(a.attendees[0].response, ResponseStatus::Accepted);
    assert_eq!(a.attendees[1].role, AttendeeRole::Optional);
    assert_eq!(a.attendees[2].role, AttendeeRole::Resource);
    assert_eq!(a.attendees[2].mailbox.addr, "room4@example.com");
    assert_eq!(a.categories, vec!["Work", "Review"]);
    assert_eq!(e.sequence, 2);
    assert_eq!(e.tzid.as_deref(), Some("Eastern"));
    let rec = a.recurrence.as_ref().unwrap();
    assert_eq!(rec.rrule, "FREQ=WEEKLY;COUNT=10;BYDAY=MO");
    assert_eq!(rec.exdates, vec![at(2026, 10, 19, 14, 0, -240)]);

    // DST-aware expansion: 14:00 local stays 14:00 after the November change.
    let occ = e.occurrences(&cal.zones, utc(2026, 1, 1, 0, 0), utc(2027, 1, 1, 0, 0), 100);
    assert_eq!(occ.len(), 9);
    assert_eq!(occ[0].0, at(2026, 10, 12, 14, 0, -240));
    assert_eq!(occ[1].0, at(2026, 10, 26, 14, 0, -240));
    assert_eq!(occ[2].0, at(2026, 11, 2, 14, 0, -300));
    assert_eq!(occ[8].0, at(2026, 12, 14, 14, 0, -300));
    assert_eq!(occ[2].1.unix - occ[2].0.unix, 90 * 60);
    // The model-only expansion keeps the start's fixed offset.
    let fixed = ics::expand_appointment(a, utc(2026, 1, 1, 0, 0), utc(2027, 1, 1, 0, 0), 100);
    assert_eq!(fixed.len(), 9);
    assert_eq!(fixed[2].0.unix, at(2026, 11, 2, 14, 0, -240).unix);
    // Window clipping.
    let some = e.occurrences(&cal.zones, utc(2026, 11, 1, 0, 0), utc(2026, 11, 20, 0, 0), 100);
    assert_eq!(some.len(), 3);
}

#[test]
fn all_day_floating_duration_and_builtin_zones() {
    let text = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:a\nDTSTART;VALUE=DATE:20261225\nSUMMARY:Holiday\nTRANSP:TRANSPARENT\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:b\nDTSTART:20261012T090000\nDURATION:PT45M\nX-MICROSOFT-CDO-BUSYSTATUS:OOF\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:c\nDTSTART;TZID=Europe/Berlin:20260710T100000\nDTEND;TZID=Europe/Berlin:20260710T110000\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:d\nDTSTART;TZID=\"(UTC+05:30) Somewhere\":20260110T100000\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:e\nDTSTART:20260110T100000Z\nBEGIN:VALARM\nTRIGGER;RELATED=END:-PT90M\nEND:VALARM\nDTEND:20260110T110000Z\nEND:VEVENT\n\
BEGIN:VEVENT\nSUMMARY:no start\nEND:VEVENT\nEND:VCALENDAR\n";
    let cal = ics::read_str(text, ImportOptions { local_offset_min: -240 }).unwrap();
    let a = cal.appointments();
    assert_eq!(a.len(), 5);
    assert!(a[0].all_day);
    assert_eq!(a[0].start, at(2026, 12, 25, 0, 0, -240));
    assert_eq!(a[0].end, at(2026, 12, 26, 0, 0, -240));
    assert_eq!(a[0].show_as, BusyStatus::Free);
    assert_eq!(a[1].start, at(2026, 10, 12, 9, 0, -240));
    assert_eq!(a[1].end.unix - a[1].start.unix, 45 * 60);
    assert_eq!(a[1].show_as, BusyStatus::OutOfOffice);
    assert_eq!(a[2].start, at(2026, 7, 10, 10, 0, 120));
    assert_eq!(a[2].start.unix, utc(2026, 7, 10, 8, 0).unix);
    assert_eq!(a[3].start, at(2026, 1, 10, 10, 0, 330));
    assert_eq!(a[4].start, utc(2026, 1, 10, 10, 0));
    assert_eq!(a[4].reminder_min, Some(30));
}

#[test]
fn builtin_zone_offsets() {
    let ny = tz::builtin("America/New_York").unwrap();
    let jan = value::local_secs(2026, 1, 15, 12, 0, 0).unwrap();
    let jul = value::local_secs(2026, 7, 15, 12, 0, 0).unwrap();
    assert_eq!(ny.offset_at_local(jan), -300);
    assert_eq!(ny.offset_at_local(jul), -240);
    assert_eq!(ny.offset_at_utc(utc(2026, 7, 15, 12, 0).unix), -240);
    let syd = tz::builtin("australia/sydney").unwrap();
    assert_eq!(syd.offset_at_local(jan), 660);
    assert_eq!(syd.offset_at_local(jul), 600);
    let lon = tz::builtin("Europe/London").unwrap();
    assert_eq!(lon.offset_at_local(jul), 60);
    assert_eq!(lon.offset_at_local(jan), 0);
    // Transition edges (EU: last Sunday of March 2026 is the 29th, 01:00 UTC).
    assert_eq!(lon.offset_at_local(value::local_secs(2026, 3, 29, 0, 59, 0).unwrap()), 0);
    assert_eq!(lon.offset_at_local(value::local_secs(2026, 3, 29, 2, 30, 0).unwrap()), 60);
    assert_eq!(tz::fixed_from_name("Etc/GMT+5"), Some(-300));
    assert_eq!(tz::fixed_from_name("UTC+05:30"), Some(330));
    assert_eq!(tz::fixed_from_name("GMT-0800"), Some(-480));
    assert_eq!(tz::fixed_from_name("Mars/Olympus"), None);
    assert!(tz::builtin("Nowhere/Special").is_none());
}

/// Occurrence dates (`YYYYMMDD`) of `rule` from `start` (UTC wall clock) within 2000..2100.
fn dates(rule: &str, start: &str) -> Vec<String> {
    let r = Rrule::parse(rule).unwrap();
    let (s, _) = value::parse_date_time(start).unwrap();
    let ident = |l: i64| l;
    let never = |_: i64, _: i64| false;
    let x = rrule::Expand { rule: &r, dtstart: s, duration: 0, to_utc: &ident, excluded: &never };
    let from = value::local_secs(2000, 1, 1, 0, 0, 0).unwrap();
    let to = value::local_secs(2100, 1, 1, 0, 0, 0).unwrap();
    rrule::expand(&x, from, to, 50).into_iter().map(|o| value::format_date(o.local)).collect()
}

#[test]
fn first_day_from_finds_where_the_series_starts() {
    let first = |rule: &str, start: &str| {
        let (s, _) = value::parse_date_time(start).unwrap();
        Rrule::parse(rule)
            .unwrap()
            .first_day_from(s.div_euclid(value::DAY))
            .map(|d| value::format_date(d * value::DAY))
    };
    // 2026-10-14 is a Wednesday.
    assert_eq!(first("FREQ=WEEKLY;BYDAY=MO", "20261014T090000").as_deref(), Some("20261019"));
    assert_eq!(first("FREQ=WEEKLY;BYDAY=WE,FR", "20261014T090000").as_deref(), Some("20261014"));
    assert_eq!(first("FREQ=WEEKLY", "20261014T090000").as_deref(), Some("20261014"));
    assert_eq!(first("FREQ=MONTHLY;BYMONTHDAY=31", "20261012T090000").as_deref(), Some("20261031"));
    assert_eq!(first("FREQ=MONTHLY;BYMONTHDAY=31", "20261101T090000").as_deref(), Some("20261231"));
    assert_eq!(first("FREQ=MONTHLY;BYDAY=-1FR", "20261031T090000").as_deref(), Some("20261127"));
    assert_eq!(first("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29", "20260301T090000").as_deref(), Some("20280229"));
    assert_eq!(first("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR", "20261010T090000").as_deref(), Some("20261012"));
    // Impossible rules give up instead of looping.
    assert_eq!(first("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30", "20260101T090000"), None);
}

#[test]
fn rrule_expansion() {
    assert_eq!(dates("FREQ=DAILY;COUNT=3", "20261030T090000"), ["20261030", "20261031", "20261101"]);
    assert_eq!(
        dates("FREQ=WEEKLY;INTERVAL=2;BYDAY=TU,TH;UNTIL=20261031T235959Z", "20261006T090000"),
        ["20261006", "20261008", "20261020", "20261022"]
    );
    assert_eq!(
        dates("FREQ=MONTHLY;BYDAY=2TU;COUNT=3", "20261013T090000"),
        ["20261013", "20261110", "20261208"]
    );
    assert_eq!(
        dates("FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=3", "20260131T090000"),
        ["20260131", "20260228", "20260331"]
    );
    assert_eq!(dates("FREQ=MONTHLY;COUNT=3", "20260131T090000"), ["20260131", "20260331", "20260531"]);
    assert_eq!(
        dates("FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU;COUNT=2", "20260329T010000"),
        ["20260329", "20270328"]
    );
    assert_eq!(dates("FREQ=YEARLY;COUNT=3", "20240229T090000"), ["20240229", "20280229", "20320229"]);
    assert_eq!(
        dates("FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;COUNT=3", "20261030T090000"),
        ["20261030", "20261130", "20261231"]
    );
    assert_eq!(
        dates("FREQ=DAILY;BYDAY=MO,WE;COUNT=4", "20261012T090000"),
        ["20261012", "20261014", "20261019", "20261021"]
    );
    assert_eq!(
        dates("FREQ=YEARLY;BYYEARDAY=1,-1;COUNT=4", "20260101T090000"),
        ["20260101", "20261231", "20270101", "20271231"]
    );
    assert_eq!(
        dates("FREQ=YEARLY;BYMONTH=1,7;COUNT=3", "20260115T090000"),
        ["20260115", "20260715", "20270115"]
    );
    assert_eq!(dates("FREQ=YEARLY;BYDAY=1MO;COUNT=2", "20260105T090000"), ["20260105", "20270104"]);
    assert_eq!(dates("FREQ=WEEKLY;UNTIL=20261020", "20261006T090000"), ["20261006", "20261013", "20261020"]);
    // Never matches: stops at the period cap without hanging.
    assert_eq!(dates("FREQ=MONTHLY;BYMONTH=2;BYMONTHDAY=30", "20260101T090000"), ["20260101"]);
}

#[test]
fn rrule_window_skips_ahead_and_formats() {
    let r = Rrule::parse("FREQ=DAILY").unwrap();
    let start = value::local_secs(1900, 1, 1, 9, 0, 0).unwrap();
    let ident = |l: i64| l;
    let never = |_: i64, _: i64| false;
    let x = rrule::Expand { rule: &r, dtstart: start, duration: 3600, to_utc: &ident, excluded: &never };
    let from = value::local_secs(2026, 10, 10, 0, 0, 0).unwrap();
    let occ = rrule::expand(&x, from, from + 3 * 86_400, 100);
    assert_eq!(occ.len(), 3);
    assert_eq!(value::format_date_time(occ[0].local), "20261010T090000");
    for s in [
        "FREQ=WEEKLY;INTERVAL=2;COUNT=5;BYDAY=-1FR,MO;WKST=SU",
        "FREQ=YEARLY;UNTIL=20301231T000000Z;BYMONTH=2,3;BYMONTHDAY=1,-1;BYSETPOS=1",
        "FREQ=MONTHLY;UNTIL=20301231;BYYEARDAY=100",
    ] {
        let r = Rrule::parse(s).unwrap();
        assert_eq!(Rrule::parse(&r.to_value()).unwrap(), r, "{s}");
    }
    assert!(Rrule::parse("FREQ=HOURLY").is_err());
    assert!(Rrule::parse("BYDAY=MO").is_err());
    assert!(Rrule::parse("FREQ=DAILY;BYDAY=XX").is_err());
}

#[test]
fn durations_and_offsets() {
    assert_eq!(value::parse_duration("-PT15M"), Some(-900));
    assert_eq!(value::parse_duration("P1W"), Some(604_800));
    assert_eq!(value::parse_duration("P1DT2H3M4S"), Some(93_784));
    assert_eq!(value::parse_duration("PT"), None);
    assert_eq!(value::parse_duration("P1H"), None);
    assert_eq!(value::format_duration(-900), "-PT15M");
    assert_eq!(value::format_duration(86_400), "P1D");
    assert_eq!(value::format_duration(0), "PT0S");
    assert_eq!(value::format_duration(93_784), "P1DT2H3M4S");
    assert_eq!(value::parse_utc_offset("-0430"), Some(-270));
    assert_eq!(value::format_utc_offset(-270), "-0430");
}

fn sample_appointment() -> Appointment {
    Appointment {
        uid: "round-trip@example.com".into(),
        subject: "Plan; review, and more".into(),
        location: "Room \"A\"".into(),
        start: at(2026, 10, 12, 14, 0, -240),
        end: at(2026, 10, 12, 15, 0, -240),
        show_as: BusyStatus::OutOfOffice,
        reminder_min: Some(30),
        organizer: Some(Mailbox::new(Some("Lee, Ann"), "ann@example.com")),
        attendees: vec![
            Attendee { mailbox: Mailbox::new(Some("Bob"), "bob@example.org"), ..Default::default() },
            Attendee {
                mailbox: Mailbox::new(None, "cy@example.net"),
                role: AttendeeRole::Optional,
                response: ResponseStatus::Tentative,
            },
            Attendee {
                mailbox: Mailbox::new(None, "room@example.com"),
                role: AttendeeRole::Resource,
                ..Default::default()
            },
        ],
        recurrence: Some(Recurrence {
            rrule: "FREQ=WEEKLY;BYDAY=MO;COUNT=5".into(),
            exdates: vec![at(2026, 10, 19, 14, 0, -240)],
        }),
        categories: vec!["Work".into(), "A,B".into()],
        private: true,
        body: "Line 1\nLine 2; with, punctuation\\".into(),
        is_meeting: true,
        ..Default::default()
    }
}

#[test]
fn stored_events_leave_scheduling_to_the_client() {
    // Regression: events kept on a CalDAV server carried plain ORGANIZER / ATTENDEE, so a server that
    // schedules (RFC 6638) mailed the attendees itself, next to the invitations MailCraft sends.
    let a = sample_appointment();
    let now = utc(2026, 10, 1, 0, 0);
    let unfold = |t: String| t.replace("\r\n ", "");
    let stored = unfold(ics::write_stored(&[&a], now));
    let people: Vec<&str> =
        stored.split("\r\n").filter(|l| l.starts_with("ORGANIZER") || l.starts_with("ATTENDEE")).collect();
    assert_eq!(people.len(), 4, "{stored}");
    assert!(people.iter().all(|l| l.contains(";SCHEDULE-AGENT=CLIENT")), "{people:?}");
    // It reads back the same; exports and invitations do not carry the parameter.
    let back = ics::read(stored.as_bytes(), ImportOptions::default()).unwrap();
    let e = &back.events[0].appointment;
    assert_eq!(e.organizer.as_ref().unwrap().addr, "ann@example.com");
    assert_eq!(e.attendees.len(), 3);
    assert!(!unfold(ics::write(&[&a], &[], None, now)).contains("SCHEDULE-AGENT"));
    assert!(!unfold(itip::request(&a, 1, now)).contains("SCHEDULE-AGENT"));
}

#[test]
fn appointments_round_trip() {
    let a = sample_appointment();
    let all_day = Appointment {
        uid: "allday@example.com".into(),
        subject: "Holiday".into(),
        start: at(2026, 12, 25, 0, 0, 120),
        end: at(2026, 12, 26, 0, 0, 120),
        all_day: true,
        show_as: BusyStatus::Free,
        ..Default::default()
    };
    let utc_one = Appointment {
        uid: "u@example.com".into(),
        start: utc(2026, 1, 2, 3, 4),
        end: utc(2026, 1, 2, 4, 4),
        ..Default::default()
    };
    let text = ics::write(&[&a, &all_day, &utc_one], &[], None, utc(2026, 10, 1, 0, 0));
    for line in text.split("\r\n") {
        assert!(line.len() <= 75, "{line}");
    }
    assert!(text.contains("TZID=\"UTC-04:00\""));
    assert!(text.contains("DTSTART;VALUE=DATE:20261225"));
    assert!(text.contains("DTSTART:20260102T030400Z"));
    let back = ics::read_str(&text, ImportOptions { local_offset_min: 120 }).unwrap();
    assert_eq!(back.appointments(), vec![a, all_day, utc_one], "{text}");
}

#[test]
fn long_text_folds_and_unfolds() {
    let mut a = sample_appointment();
    a.body = "\u{4e2d}\u{6587} long text ".repeat(40);
    let text = ics::write(&[&a], &[], None, DateTime::default());
    assert!(text.split("\r\n").all(|l| l.len() <= 75));
    let back = ics::read_str(&text, ImportOptions::default()).unwrap();
    assert_eq!(back.events[0].appointment.body, a.body);
}

#[test]
fn tasks_round_trip() {
    let t = Task {
        subject: "File report".into(),
        start: Some(at(2026, 10, 1, 9, 0, -240)),
        due: Some(at(2026, 10, 9, 17, 0, -240)),
        status: TaskStatus::WaitingOnSomeoneElse,
        priority: TaskPriority::High,
        percent_complete: 50,
        reminder: Some(utc(2026, 10, 9, 13, 0)),
        body: "Numbers, then text".into(),
        categories: vec!["Finance".into()],
        ..Default::default()
    };
    let done = Task {
        subject: "Done".into(),
        status: TaskStatus::Completed,
        percent_complete: 100,
        completed: Some(utc(2026, 10, 2, 8, 0)),
        priority: TaskPriority::Low,
        ..Default::default()
    };
    let deferred = Task { subject: "Later".into(), status: TaskStatus::Deferred, ..Default::default() };
    let text = ics::write(&[], &[&t, &done, &deferred], None, DateTime::default());
    let back = ics::read_str(&text, ImportOptions::default()).unwrap();
    assert_eq!(back.tasks, vec![t, done, deferred]);
}

#[test]
fn overrides_become_exdates() {
    let text = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:s\nDTSTART:20261005T150000Z\nDTEND:20261005T160000Z\nRRULE:FREQ=DAILY;COUNT=5\n\
END:VEVENT\nBEGIN:VEVENT\nUID:s\nRECURRENCE-ID:20261007T150000Z\nDTSTART:20261007T170000Z\nDTEND:20261007T180000Z\n\
SUMMARY:moved\nEND:VEVENT\nEND:VCALENDAR\n";
    let cal = ics::read_str(text, ImportOptions::default()).unwrap();
    assert_eq!(cal.events.len(), 2);
    let master = &cal.events[0].appointment;
    assert_eq!(master.recurrence.as_ref().unwrap().exdates, vec![utc(2026, 10, 7, 15, 0)]);
    let occ = ics::expand_appointment(master, utc(2026, 10, 1, 0, 0), utc(2026, 11, 1, 0, 0), 10);
    assert_eq!(occ.len(), 4);
    assert_eq!(cal.events[1].recurrence_id, Some(utc(2026, 10, 7, 15, 0)));
}

#[test]
fn overrides_keep_their_recurrence_id_and_round_trip() {
    let text = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:s\nDTSTART:20261005T150000Z\nDTEND:20261005T160000Z\nRRULE:FREQ=DAILY;COUNT=5\n\
END:VEVENT\nBEGIN:VEVENT\nUID:s\nRECURRENCE-ID:20261007T150000Z\nDTSTART:20261007T170000Z\nDTEND:20261007T180000Z\n\
SUMMARY:moved\nEND:VEVENT\nEND:VCALENDAR\n";
    let cal = ics::read_str(text, ImportOptions::default()).unwrap();
    let moved = &cal.events[1].appointment;
    assert_eq!(moved.recurrence_id, Some(utc(2026, 10, 7, 15, 0)));
    assert!(moved.recurrence.is_none());
    assert_eq!(cal.events[0].appointment.recurrence_id, None);
    // Written back, the override still names the instance it replaces.
    let master = &cal.events[0].appointment;
    let out = ics::write(&[master, moved], &[], None, utc(2026, 10, 1, 0, 0));
    assert!(out.contains("RECURRENCE-ID:20261007T150000Z"), "{out}");
    let again = ics::read_str(&out, ImportOptions::default()).unwrap();
    assert_eq!(again.events[1].appointment.recurrence_id, Some(utc(2026, 10, 7, 15, 0)));
    assert_eq!(
        again.events[0].appointment.recurrence.as_ref().unwrap().exdates,
        vec![utc(2026, 10, 7, 15, 0)]
    );
}

#[test]
fn itip_message_prefers_the_series_over_an_override_listed_first() {
    let text = "BEGIN:VCALENDAR\nMETHOD:REQUEST\nBEGIN:VEVENT\nUID:s\nRECURRENCE-ID:20261007T150000Z\n\
DTSTART:20261007T170000Z\nDTEND:20261007T180000Z\nSUMMARY:moved\nEND:VEVENT\nBEGIN:VEVENT\nUID:s\n\
DTSTART:20261005T150000Z\nDTEND:20261005T160000Z\nRRULE:FREQ=DAILY;COUNT=5\nSUMMARY:series\nEND:VEVENT\n\
END:VCALENDAR\n";
    let msg = itip::read(text.as_bytes(), ImportOptions::default()).unwrap();
    assert_eq!(msg.event.appointment.subject, "series");
    assert!(msg.event.recurrence_id.is_none());
    assert_eq!(msg.overrides.len(), 1);
    assert_eq!(msg.overrides[0].appointment.subject, "moved");
    // A message about one instance only: that instance is the event.
    let one = "BEGIN:VCALENDAR\nMETHOD:CANCEL\nBEGIN:VEVENT\nUID:s\nRECURRENCE-ID:20261007T150000Z\n\
DTSTART:20261007T150000Z\nDTEND:20261007T160000Z\nEND:VEVENT\nEND:VCALENDAR\n";
    let msg = itip::read(one.as_bytes(), ImportOptions::default()).unwrap();
    assert_eq!(msg.event.recurrence_id, Some(utc(2026, 10, 7, 15, 0)));
    assert!(msg.overrides.is_empty());
}

#[test]
fn itip_request_reply_cancel() {
    let mut organizer_copy = sample_appointment();
    let now = utc(2026, 10, 1, 0, 0);
    let req = itip::request(&organizer_copy, 1, now);
    assert!(req.contains("METHOD:REQUEST"));
    assert!(req.contains("RSVP=TRUE"));
    assert!(!req.contains("BEGIN:VALARM"));
    let got = itip::read(req.as_bytes(), ImportOptions::default()).unwrap();
    assert_eq!(got.method, Method::Request);
    assert_eq!(got.event.sequence, 1);
    assert_eq!(got.event.status.as_deref(), Some("CONFIRMED"));
    // Bob accepts.
    let bob = Mailbox::new(None, "BOB@example.org");
    let reply = itip::reply(&got.event.appointment, &bob, ResponseStatus::Accepted, 1, now);
    assert_eq!(reply.matches("ATTENDEE").count(), 1);
    let parsed = itip::read(reply.as_bytes(), ImportOptions::default()).unwrap();
    assert_eq!(parsed.method, Method::Reply);
    assert_eq!(itip::apply_reply(&mut organizer_copy, &parsed), 1);
    assert_eq!(organizer_copy.attendees[0].response, ResponseStatus::Accepted);
    // A reply for another meeting changes nothing.
    let mut other = sample_appointment();
    other.uid = "different".into();
    assert_eq!(itip::apply_reply(&mut other, &parsed), 0);
    // Cancel.
    let cancel = itip::cancel(&organizer_copy, 2, now);
    let c = itip::read(cancel.as_bytes(), ImportOptions::default()).unwrap();
    assert!(itip::cancels(&organizer_copy, &c));
    assert_eq!(c.event.status.as_deref(), Some("CANCELLED"));
    // Not a scheduling message.
    let plain = ics::write(&[&organizer_copy], &[], None, now);
    assert!(itip::read(plain.as_bytes(), ImportOptions::default()).is_err());
    assert_eq!(Method::parse("x-custom").as_str(), "X-CUSTOM");
}

#[test]
fn rejects_non_calendar_and_tolerates_bare_components() {
    assert!(ics::read_str("hello", ImportOptions::default()).is_err());
    let cal = ics::read_str("BEGIN:VEVENT\nDTSTART:20260101T000000Z\nEND:VEVENT", ImportOptions::default())
        .unwrap();
    assert_eq!(cal.events.len(), 1);
    // Unbalanced and deeply nested input.
    let deep = "BEGIN:VCALENDAR\n".to_owned() + &"BEGIN:X\n".repeat(100) + "END:VCALENDAR\nEND:NOTHING\n";
    assert!(ics::read_str(&deep, ImportOptions::default()).is_ok());
}

proptest! {
    #[test]
    fn ics_never_panics(data in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = ics::read(&data, ImportOptions::default());
        let _ = itip::read(&data, ImportOptions { local_offset_min: 330 });
    }

    #[test]
    fn ics_structured_never_panics(
        lines in proptest::collection::vec(
            prop_oneof![
                "(DTSTART|DTEND|DUE|EXDATE|RDATE|RECURRENCE-ID)(;TZID=[A-Za-z/+:0-9-]{0,12}|;VALUE=DATE)?:[0-9TZ]{0,16}",
                "RRULE:FREQ=(DAILY|WEEKLY|MONTHLY|YEARLY)(;(COUNT|INTERVAL|BYMONTHDAY|BYYEARDAY|BYSETPOS)=-?[0-9]{1,3}|;BYDAY=-?[0-9]?(MO|SU)|;BYMONTH=[0-9]{1,2}|;UNTIL=[0-9TZ]{8,16}){0,4}",
                "(TRIGGER|DURATION)(;RELATED=END)?:-?P[0-9WDTHMS]{0,8}",
                "(BEGIN|END):(VEVENT|VTODO|VALARM|VTIMEZONE|STANDARD|DAYLIGHT)",
                "(TZOFFSETFROM|TZOFFSETTO|TZID|ATTENDEE|ORGANIZER|SUMMARY|PRIORITY|STATUS)[;=A-Z\"]{0,8}:[ -~]{0,20}",
            ],
            0..30,
        )
    ) {
        let text = format!("BEGIN:VCALENDAR\n{}\nEND:VCALENDAR\n", lines.join("\n"));
        if let Ok(cal) = ics::read_str(&text, ImportOptions::default()) {
            for e in &cal.events {
                let _ = e.occurrences(&cal.zones, DateTime::from_unix(0, 0), DateTime::from_unix(4_000_000_000, 0), 200);
                let _ = ics::expand_appointment(&e.appointment, DateTime::from_unix(-1_000_000_000_000, 0), DateTime::from_unix(i64::MAX / 2, 0), 200);
            }
            let appts = cal.appointments();
            let refs: Vec<&Appointment> = appts.iter().collect();
            let trefs: Vec<&Task> = cal.tasks.iter().collect();
            let _ = ics::write(&refs, &trefs, Some(&Method::Publish), DateTime::default());
        }
    }

    #[test]
    fn rrule_parse_and_expand_never_panic(rule in "[A-Z=;,0-9+-]{0,60}", start in -10_000_000_000i64..300_000_000_000i64, span in 0i64..10_000_000_000) {
        if let Ok(r) = Rrule::parse(&rule) {
            let ident = |l: i64| l;
            let never = |_: i64, _: i64| false;
            let x = rrule::Expand { rule: &r, dtstart: start, duration: 3600, to_utc: &ident, excluded: &never };
            let occ = rrule::expand(&x, start, start.saturating_add(span), 500);
            prop_assert!(occ.len() <= 500);
            prop_assert!(occ.windows(2).all(|w| w[0].local < w[1].local));
        }
        let _ = value::parse_duration(&rule);
        let _ = tz::fixed_from_name(&rule);
        let _ = value::parse_time_value(&rule, None, Some(&rule));
    }

    #[test]
    fn appointment_text_round_trips(subject in "[ -~\u{e9}\u{1F600}\n]{0,80}", loc in "[ -~]{0,30}", off in -720i32..840) {
        let start = DateTime::from_unix(1_790_000_000 - 1_790_000_000 % 60, off);
        let a = Appointment {
            uid: "p@example.com".into(),
            subject: subject.replace('\r', ""),
            location: loc,
            start,
            end: DateTime::from_unix(start.unix + 1800, off),
            ..Default::default()
        };
        let text = ics::write(&[&a], &[], None, DateTime::default());
        let back = ics::read_str(&text, ImportOptions::default()).unwrap();
        prop_assert_eq!(back.appointments(), vec![a]);
    }
}

#[test]
fn hostile_timezone_rules_are_bounded() {
    let text = "BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:Bad\nBEGIN:STANDARD\nDTSTART:16010101T000000\n\
RRULE:FREQ=DAILY;COUNT=4000000000\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0200\nEND:STANDARD\n\
BEGIN:DAYLIGHT\nDTSTART:16010101T000000\nRRULE:FREQ=MONTHLY;BYMONTHDAY=31;BYMONTH=2\nTZOFFSETFROM:+0200\nTZOFFSETTO:+0100\n\
END:DAYLIGHT\nEND:VTIMEZONE\nBEGIN:VEVENT\nUID:x\nDTSTART;TZID=Bad:20260105T090000\nRRULE:FREQ=DAILY\n\
EXDATE;TZID=Bad:20260106T090000,20260107T090000\nEND:VEVENT\nEND:VCALENDAR\n";
    let cal = ics::read_str(text, ImportOptions::default()).unwrap();
    let e = &cal.events[0];
    let occ = e.occurrences(&cal.zones, utc(2026, 1, 1, 0, 0), utc(2027, 1, 1, 0, 0), 400);
    // 5 Jan … 31 Dec is 361 days, two of them excluded.
    assert_eq!(occ.len(), 359);
}

#[test]
fn zoned_recurring_appointment_exports_its_named_zone() {
    // 09:00 Toronto every Monday from October into November (DST ends 2026-11-01).
    let start = at(2026, 10, 19, 9, 0, -240);
    let a = Appointment {
        uid: "weekly@example.com".into(),
        subject: "Stand-up".into(),
        start,
        end: DateTime::from_unix(start.unix + 3600, -240),
        recurrence: Some(Recurrence {
            rrule: "FREQ=WEEKLY;COUNT=4".into(),
            exdates: vec![at(2026, 10, 26, 9, 0, -240)],
        }),
        tz: Some("America/Toronto".into()),
        ..Default::default()
    };
    let text = ics::write(&[&a], &[], None, utc(2026, 10, 1, 0, 0));
    assert!(text.contains("DTSTART;TZID=America/Toronto:20261019T090000"), "{text}");
    assert!(text.contains("EXDATE;TZID=America/Toronto:20261026T090000"), "{text}");
    assert!(text.contains("BEGIN:VTIMEZONE\r\nTZID:America/Toronto\r\n"), "{text}");
    assert!(text.contains("RRULE:FREQ=YEARLY;BYDAY=2SU;BYMONTH=3"), "{text}");
    assert!(!text.contains("UTC-04:00"), "{text}");
    // Read back (here and by any other calendar): the occurrence after the change stays at 09:00 local.
    let cal = ics::read_str(&text, ImportOptions::default()).unwrap();
    let e = &cal.events[0];
    assert_eq!(e.appointment.tz.as_deref(), Some("America/Toronto"));
    let occ = e.occurrences(&cal.zones, utc(2026, 1, 1, 0, 0), utc(2027, 1, 1, 0, 0), 10);
    assert_eq!(occ.len(), 3);
    assert_eq!(occ[1].0, at(2026, 11, 2, 9, 0, -300));
    // An invitation carries the zone too.
    let req = itip::request(&a, 0, utc(2026, 10, 1, 0, 0));
    assert!(req.contains("DTSTART;TZID=America/Toronto:20261019T090000"), "{req}");
    assert!(req.contains("TZID:America/Toronto"), "{req}");
    // All-day and zone-less appointments are written as before.
    let plain = Appointment { tz: None, ..a.clone() };
    assert!(ics::write(&[&plain], &[], None, utc(2026, 10, 1, 0, 0)).contains("TZID=\"UTC-04:00\""));
}

#[test]
fn imported_zones_name_their_iana_zone() {
    // A file's own definition under a non-IANA name (as some desktop clients write them) maps to the built-in zone with
    // the same rules; IANA names are kept; fixed offsets and floating times name no zone.
    let cal = ics::read_str(MEETING, ImportOptions::default()).unwrap();
    assert_eq!(cal.events[0].appointment.tz.as_deref(), Some("America/New_York"));
    let text = "BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:W. Europe\nBEGIN:STANDARD\nDTSTART:16011028T030000\n\
RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10\nTZOFFSETFROM:+0200\nTZOFFSETTO:+0100\nEND:STANDARD\nBEGIN:DAYLIGHT\n\
DTSTART:16010325T020000\nRRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0200\n\
END:DAYLIGHT\nEND:VTIMEZONE\nBEGIN:VTIMEZONE\nTZID:India\nBEGIN:STANDARD\nDTSTART:16010101T000000\n\
TZOFFSETFROM:+0530\nTZOFFSETTO:+0530\nEND:STANDARD\nEND:VTIMEZONE\n\
BEGIN:VEVENT\nUID:a\nDTSTART;TZID=W. Europe:20260710T100000\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:b\nDTSTART;TZID=India:20260710T100000\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:c\nDTSTART;TZID=europe/berlin:20260710T100000\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:d\nDTSTART;TZID=\"UTC+02:00\":20260710T100000\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:e\nDTSTART:20260710T100000\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:f\nDTSTART;VALUE=DATE;TZID=Europe/Berlin:20260710\nEND:VEVENT\nEND:VCALENDAR\n";
    let cal = ics::read_str(text, ImportOptions::default()).unwrap();
    let zones: Vec<Option<&str>> = cal.events.iter().map(|e| e.appointment.tz.as_deref()).collect();
    assert_eq!(
        zones,
        vec![Some("Europe/Paris"), None, Some("Europe/Berlin"), None, None, None],
        "{:?}",
        cal.events.iter().map(|e| &e.appointment.uid).collect::<Vec<_>>()
    );
}

#[test]
fn builtin_zones_follow_current_rules() {
    let off = |name: &str, y: i64, m: u8, d: u8, h: u8| {
        tz::builtin(name).unwrap().offset_at_utc(value::local_secs(y, m, d, h, 0, 0).unwrap())
    };
    // Egypt (daylight saving time again since 2023): last Friday of April to the last Thursday of October.
    assert_eq!(off("Africa/Cairo", 2026, 1, 15, 12), 120);
    assert_eq!(off("Africa/Cairo", 2026, 7, 15, 12), 180);
    assert_eq!(off("Africa/Cairo", 2026, 4, 23, 12), 120);
    assert_eq!(off("Africa/Cairo", 2026, 4, 24, 12), 180);
    // 2024: the last Thursday of October was the 31st; the change came at its midnight (21:00 UTC).
    assert_eq!(off("Africa/Cairo", 2024, 10, 31, 20), 180);
    assert_eq!(off("Africa/Cairo", 2024, 10, 31, 22), 120);
    assert_eq!(off("Africa/Cairo", 2025, 10, 30, 22), 120);
    assert_eq!(off("Africa/Cairo", 2025, 10, 30, 20), 180);
    assert_eq!(off("Africa/Cairo", 2022, 7, 15, 12), 120);
    // Kazakhstan: one zone, UTC+05:00, since 2024.
    assert_eq!(off("Asia/Almaty", 2026, 1, 15, 12), 300);
    assert_eq!(tz::builtin_name("america/toronto"), Some("America/Toronto"));
    assert_eq!(tz::builtin_name("UTC+02:00"), None);
    assert_eq!(tz::builtin_name("Nowhere/Special"), None);
}

#[test]
fn named_zone_vtimezone_round_trips() {
    for name in ["America/New_York", "Europe/London", "Australia/Sydney", "Pacific/Auckland", "Africa/Cairo"]
    {
        let z = tz::builtin(name).unwrap();
        let mut text = String::new();
        z.to_component().write(&mut text);
        let parsed = ics::parse::parse(&text);
        let back = tz::TimeZone::from_component(parsed.first().unwrap()).unwrap();
        for day in 0..(3 * 366) {
            let u = value::local_secs(2026, 1, 1, 12, 0, 0).unwrap() + day * 86_400;
            assert_eq!(back.offset_at_utc(u), z.offset_at_utc(u), "{name} day {day}");
        }
        assert_eq!(tz::equivalent_builtin(&back, 2026).map(tz::builtin_name), Some(tz::builtin_name(name)));
    }
}

#[test]
fn item_window_details_round_trip() {
    use mailcraft_model::Importance;
    use mailcraft_model::calendar::ItemAttachment;
    // A flight: leaves Toronto, lands in London (an end in another zone), with formatted notes, a file and
    // high importance.
    let a = Appointment {
        uid: "flight@example.com".into(),
        subject: "Flight".into(),
        start: utc(2026, 11, 2, 23, 0),
        end: utc(2026, 11, 3, 6, 0),
        tz: Some("America/Toronto".into()),
        end_tz: Some("Europe/London".into()),
        importance: Importance::High,
        body: "Gate 4".into(),
        body_html: Some("<p>Gate <b>4</b>; seat 12A</p>".into()),
        attachments: vec![ItemAttachment {
            filename: "boarding pass, final.pdf".into(),
            mime_type: "application/pdf".into(),
            data: b"%PDF-1.4 synthetic".as_slice().into(),
        }],
        ..Default::default()
    };
    let low = Appointment {
        uid: "low@example.com".into(),
        start: utc(2026, 1, 2, 3, 4),
        end: utc(2026, 1, 2, 4, 4),
        importance: Importance::Low,
        ..Default::default()
    };
    let text = ics::write(&[&a, &low], &[], None, utc(2026, 10, 1, 0, 0));
    assert!(text.contains("DTSTART;TZID=America/Toronto:20261102T180000"), "{text}");
    assert!(text.contains("DTEND;TZID=Europe/London:20261103T060000"), "{text}");
    assert!(text.contains("PRIORITY:1"), "{text}");
    assert!(text.contains("PRIORITY:9"), "{text}");
    assert!(text.contains("X-ALT-DESC;FMTTYPE=text/html:"), "{text}");
    let back = ics::read_str(&text, ImportOptions::default()).unwrap().appointments();
    let b = back.iter().find(|x| x.uid == "flight@example.com").unwrap();
    assert_eq!((b.start.unix, b.end.unix), (a.start.unix, a.end.unix));
    assert_eq!(b.tz.as_deref(), Some("America/Toronto"));
    assert_eq!(b.end_tz.as_deref(), Some("Europe/London"));
    assert_eq!(b.end_zone(), Some("Europe/London"));
    assert_eq!(b.importance, Importance::High);
    assert_eq!(b.body_html, a.body_html);
    assert_eq!(b.attachments, a.attachments);
    let l = back.iter().find(|x| x.uid == "low@example.com").unwrap();
    assert_eq!(l.importance, Importance::Low);
    assert_eq!(l.end_tz, None);
    // Invitations carry the files as message attachments, not inside the calendar data.
    let invite = itip::request(&a, 0, utc(2026, 10, 1, 0, 0));
    assert!(!invite.contains("ATTACH"), "{invite}");
    assert!(invite.contains("PRIORITY:1"), "{invite}");
    // Hostile input: junk base64 and notes that are not HTML are dropped, never a panic.
    let junk = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:j\r\nDTSTART:20260101T000000Z\r\n\
ATTACH;ENCODING=BASE64;VALUE=BINARY:@@@@\r\nATTACH:https://example.com/x.pdf\r\nPRIORITY:x\r\n\
X-ALT-DESC;FMTTYPE=text/plain:not html\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let j = ics::read_str(junk, ImportOptions::default()).unwrap().appointments();
    assert!(j[0].attachments.is_empty(), "{:?}", j[0].attachments);
    assert_eq!(j[0].body_html, None);
    assert_eq!(j[0].importance, Importance::Normal);
}
