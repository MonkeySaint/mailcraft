//! Server copies kept across an edit, inline file names, and long content lines.

use mailcraft_model::calendar::ItemAttachment;
use mailcraft_model::{Appointment, DateTime};

use crate::ics::{self, ImportOptions};
use crate::keep;
use crate::text::{unfold, unfold_capped};

fn utc(y: i32, mo: u8, d: u8, h: u8) -> DateTime {
    DateTime::from_civil(y, mo, d, h, 0, 0, 0).unwrap()
}

fn unfolded(text: &str) -> String {
    text.replace("\r\n ", "")
}

#[test]
fn unfold_drops_overlong_lines_instead_of_cutting_them() {
    // Regression: lines were cut at 1 MiB, so a larger inline file was read (and uploaded again) corrupted.
    let lines = unfold_capped("A:1\r\nB:22\r\n 33\r\n 44\r\nC:3\r\nD:4444444444\r\n", 6);
    assert_eq!(lines, ["A:1", "C:3"], "B and D are longer than 6 bytes unfolded");
    let lines = unfold_capped("B:22\r\n 3\r\n", 6);
    assert_eq!(lines, ["B:223"]);
    let long = format!("X:{}", "y".repeat(3 << 20));
    assert_eq!(unfold(&long), std::slice::from_ref(&long), "valid input is never cut");
}

#[test]
fn large_inline_files_round_trip_whole() {
    // Regression: a file over about 768 KiB came back cut and corrupted.
    let data: Vec<u8> =
        (0..2 * 1024 * 1024u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    let a = Appointment {
        uid: "big@example.com".into(),
        subject: "Plans".into(),
        start: utc(2026, 11, 2, 9),
        end: utc(2026, 11, 2, 10),
        attachments: vec![ItemAttachment {
            filename: "plans.bin".into(),
            mime_type: "application/octet-stream".into(),
            data: data.clone().into(),
        }],
        ..Default::default()
    };
    let text = ics::write_stored(&[&a], utc(2026, 11, 1, 0));
    let back = ics::read_str(&text, ImportOptions::default()).unwrap();
    let files = &back.events[0].appointment.attachments;
    assert_eq!(files.len(), 1);
    assert!(files[0].data[..] == data[..]);
}

#[test]
fn inline_file_names_are_made_safe() {
    // Regression: `X-FILENAME=../../x` was kept as is and proposed as the path to save to.
    let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:n@example.com\r\nDTSTART:20261102T090000Z\r\n\
        ATTACH;ENCODING=BASE64;VALUE=BINARY;X-FILENAME=../../x:eA==\r\n\
        ATTACH;ENCODING=BASE64;VALUE=BINARY;X-APPLE-FILENAME=..\\\\..\\\\.profile:eA==\r\n\
        ATTACH;ENCODING=BASE64;VALUE=BINARY;FILENAME=NUL.txt:eA==\r\n\
        END:VEVENT\r\nEND:VCALENDAR\r\n";
    let cal = ics::read_str(ics, ImportOptions::default()).unwrap();
    let names: Vec<&str> =
        cal.events[0].appointment.attachments.iter().map(|f| f.filename.as_str()).collect();
    assert_eq!(names, ["x", "profile", "_NUL.txt"]);
}

const CARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:urn:uuid:kim-1\r\nFN:Kim Example\r\nN:Example;Kim;;;\r\n\
    EMAIL:kim@example.com\r\nNOTE:old note\r\nX-OTHER:42\r\nEND:VCARD\r\n";

#[test]
fn cards_keep_unmodelled_properties_and_take_changes() {
    let base = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:urnuuidkim-1\r\nFN:Kim Example\r\nN:Example;Kim;;;\r\n\
        EMAIL;TYPE=INTERNET:kim@example.com\r\nNOTE:old note\r\nEND:VCARD\r\n";
    // Here: a title added, the note cleared, the address unchanged.
    let mine = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:urnuuidkim-1\r\nFN:Kim Example\r\nN:Example;Kim;;;\r\n\
        EMAIL;TYPE=INTERNET:kim@example.com\r\nTITLE:Engineer\r\nEND:VCARD\r\n";
    let out = unfolded(&keep::patch_card(CARD, base, mine).unwrap());
    assert!(out.contains("TITLE:Engineer"), "{out}");
    assert!(!out.contains("NOTE"), "cleared here: {out}");
    assert!(out.contains("X-OTHER:42"), "{out}");
    assert!(out.contains("UID:urn:uuid:kim-1"), "an unchanged property keeps the server's form: {out}");
    assert!(out.contains("EMAIL:kim@example.com"), "{out}");
    assert_eq!(keep::card_version(CARD), crate::vcf::Version::V3);
    // vCard 2.1 (quoted-printable soft breaks) and junk are not patched.
    let old = CARD.replace("VERSION:3.0", "VERSION:2.1");
    assert!(keep::patch_card(&old, base, mine).is_none());
    assert!(keep::patch_card("junk", base, mine).is_none());
}

const SERVER_CAL: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nX-WR-CALNAME:Work\r\n\
    BEGIN:VTIMEZONE\r\nTZID:Example/Zone\r\nBEGIN:STANDARD\r\nDTSTART:19700101T000000\r\nTZOFFSETFROM:-0500\r\n\
    TZOFFSETTO:-0500\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
    BEGIN:VEVENT\r\nUID:s-1\r\nDTSTAMP:20261001T000000Z\r\nDTSTART;TZID=Example/Zone:20261012T090000\r\n\
    DURATION:PT1H\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nSUMMARY:Sync\r\nX-OTHER:keep\r\n\
    BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n\
    BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT1H\r\nEND:VALARM\r\nEND:VEVENT\r\n\
    BEGIN:VEVENT\r\nUID:s-1\r\nRECURRENCE-ID;TZID=Example/Zone:20261019T090000\r\n\
    DTSTART;TZID=Example/Zone:20261019T110000\r\nDURATION:PT1H\r\nSUMMARY:Sync (moved)\r\nEND:VEVENT\r\n\
    BEGIN:VEVENT\r\nUID:s-1\r\nRECURRENCE-ID;TZID=Example/Zone:20261026T090000\r\n\
    DTSTART;TZID=Example/Zone:20261026T100000\r\nDURATION:PT1H\r\nSUMMARY:Sync (late)\r\nEND:VEVENT\r\n\
    END:VCALENDAR\r\n";

/// MailCraft's copy of SERVER_CAL as read (times in UTC, DTEND, one alarm).
fn base_cal(series_extra: &str, keep_late: bool, moved_start: &str) -> String {
    let mut s = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//MailCraft//EN\r\nBEGIN:VEVENT\r\nUID:s-1\r\n\
         DTSTAMP:20261101T000000Z\r\nDTSTART:20261012T140000Z\r\nDTEND:20261012T150000Z\r\n\
         RRULE:FREQ=WEEKLY;COUNT=4\r\nSUMMARY:Sync\r\n",
    );
    s.push_str(series_extra);
    s.push_str("BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\n");
    s.push_str(&format!(
        "BEGIN:VEVENT\r\nUID:s-1\r\nRECURRENCE-ID:20261019T140000Z\r\nDTSTART:{moved_start}\r\n\
         DTEND:20261019T170000Z\r\nSUMMARY:Sync (moved)\r\nEND:VEVENT\r\n"
    ));
    if keep_late {
        s.push_str(
            "BEGIN:VEVENT\r\nUID:s-1\r\nRECURRENCE-ID:20261026T140000Z\r\nDTSTART:20261026T150000Z\r\n\
             DTEND:20261026T160000Z\r\nSUMMARY:Sync (late)\r\nEND:VEVENT\r\n",
        );
    }
    s.push_str("END:VCALENDAR\r\n");
    s
}

#[test]
fn calendars_keep_unmodelled_parts_and_take_changes() {
    let base = base_cal("", true, "20261019T160000Z");
    // Here: a location on the series, the moved instance moved again, the late one deleted.
    let mine = base_cal("LOCATION:Room 2\r\n", false, "20261019T150000Z");
    let out = unfolded(&keep::patch_calendar(SERVER_CAL, &base, &mine, 0).unwrap());
    let series = out.split("BEGIN:VEVENT").nth(1).unwrap();
    assert!(series.contains("LOCATION:Room 2"), "{out}");
    assert!(series.contains("X-OTHER:keep"), "{out}");
    assert_eq!(series.matches("BEGIN:VALARM").count(), 2, "alarms unchanged here are the server's: {out}");
    assert!(series.contains("DTSTART;TZID=Example/Zone:20261012T090000") && series.contains("DURATION:PT1H"));
    assert!(!series.contains("DTEND"), "start and end are kept together: {out}");
    assert!(series.contains("DTSTAMP:20261101T000000Z"), "stamps are MailCraft's: {out}");
    assert!(out.contains("X-WR-CALNAME:Work") && out.contains("TZID:Example/Zone"), "{out}");
    // The moved instance (matched across time zone forms) takes the new times, all of them.
    let moved = out.split("BEGIN:VEVENT").find(|e| e.contains("Sync (moved)")).unwrap();
    assert!(moved.contains("DTSTART:20261019T150000Z") && moved.contains("DTEND:20261019T170000Z"), "{out}");
    assert!(!moved.contains("DURATION"), "{out}");
    assert!(!out.contains("Sync (late)"), "deleted here: {out}");
    assert_eq!(out.matches("BEGIN:VEVENT").count(), 2, "{out}");

    // An instance MailCraft did not read stays.
    let other = SERVER_CAL.replace(
        "END:VCALENDAR",
        "BEGIN:VEVENT\r\nUID:s-1\r\nRECURRENCE-ID:20261102T140000Z\r\nDTSTART:20261102T150000Z\r\n\
         SUMMARY:Unread\r\nEND:VEVENT\r\nEND:VCALENDAR",
    );
    let out = keep::patch_calendar(&other, &base, &mine, 0).unwrap();
    assert!(out.contains("Unread"), "{out}");
    assert!(keep::patch_calendar("junk", &base, &mine, 0).is_none());
}

#[test]
fn kept_calendars_leave_out_the_files_the_item_holds() {
    let server = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:f@example.com\r\nDTSTART:20261102T090000Z\r\n\
        ATTACH;ENCODING=BASE64;VALUE=BINARY;X-FILENAME=a.txt:eA==\r\n\
        ATTACH:https://files.example.com/b.pdf\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let kept = keep::kept_calendar(server, 2_000);
    assert!(!kept.contains("a.txt") && kept.contains("b.pdf"), "{kept}");
    // The item's file goes back with the server's file by URI.
    let mine = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:f@example.com\r\nDTSTART:20261102T090000Z\r\n\
        ATTACH;ENCODING=BASE64;VALUE=BINARY;X-FILENAME=a.txt:eA==\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let out = keep::patch_calendar(&kept, &kept, mine, 0).unwrap();
    assert!(out.contains("a.txt") && out.contains("b.pdf"), "{out}");
    assert_eq!(out.matches("ATTACH").count(), 2, "{out}");
    assert!(keep::kept_calendar(&"x".repeat(keep::MAX_KEPT_BYTES + 1), 2_000).is_empty());
}

#[test]
fn kept_calendars_keep_the_files_of_events_the_item_does_not_hold() {
    // Regression: files were left out of every event, so the first edit deleted the files of the events
    // MailCraft did not read (another UID, past the group limit) from the server.
    let server = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a@example.com\r\nDTSTART:20261102T090000Z\r\n\
        ATTACH;ENCODING=BASE64;VALUE=BINARY;X-FILENAME=a.txt:eA==\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:a@example.com\r\nRECURRENCE-ID:20261109T090000Z\r\nDTSTART:20261109T100000Z\r\n\
        ATTACH;ENCODING=BASE64;VALUE=BINARY;X-FILENAME=late.txt:eA==\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:b@example.com\r\nDTSTART:20261103T090000Z\r\n\
        ATTACH;ENCODING=BASE64;VALUE=BINARY;X-FILENAME=b.txt:eA==\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:a@example.com\r\nDTSTART:not a time\r\n\
        ATTACH;ENCODING=BASE64;VALUE=BINARY;X-FILENAME=unread.txt:eA==\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let kept = keep::kept_calendar(server, 2_000);
    assert!(!kept.contains("a.txt") && !kept.contains("late.txt"), "the item holds these: {kept}");
    assert!(kept.contains("b.txt"), "another UID: {kept}");
    assert!(kept.contains("unread.txt"), "an event the reader skips: {kept}");
    // Past the group limit: only the first event's files are held.
    let kept = keep::kept_calendar(server, 1);
    assert!(!kept.contains("a.txt") && kept.contains("late.txt"), "{kept}");
    // A tasks folder holds no event's files.
    assert!(keep::kept_calendar(server, 0).contains("a.txt"));
    // An edit of the item keeps the files of the events it does not hold on the server.
    let mine = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a@example.com\r\nDTSTART:20261102T090000Z\r\n\
        LOCATION:Room 2\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let base = mine.replace("LOCATION:Room 2\r\n", "");
    let out = keep::patch_calendar(&keep::kept_calendar(server, 1), &base, mine, 0).unwrap();
    assert!(out.contains("b.txt") && out.contains("late.txt"), "{out}");
}

#[test]
fn attendees_go_up_scheduled_by_the_client() {
    // Regression: the server's ATTENDEE lines (without SCHEDULE-AGENT=CLIENT) were kept when MailCraft's were
    // unchanged, so a scheduling server mailed its own invitations on a location change.
    let server = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:m@example.com\r\n\
        DTSTAMP:20261001T000000Z\r\nDTSTART:20261102T090000Z\r\nDTEND:20261102T100000Z\r\nSUMMARY:Plan\r\n\
        ORGANIZER;CN=Pat:mailto:pat@example.com\r\n\
        ATTENDEE;CN=Kim;PARTSTAT=ACCEPTED;SCHEDULE-STATUS=2.0:mailto:kim@example.com\r\n\
        END:VEVENT\r\nEND:VCALENDAR\r\n";
    let now = utc(2026, 11, 1, 0);
    let read = ics::read_str(server, ImportOptions::default()).unwrap();
    let a = read.events[0].appointment.clone();
    let base = ics::write_stored(&[&a], now);
    let edited = Appointment { location: "Room 2".into(), ..a };
    let mine = ics::write_stored(&[&edited], now);
    let out = unfolded(&keep::patch_calendar(server, &base, &mine, 0).unwrap());
    let people: Vec<&str> =
        out.split("\r\n").filter(|l| l.starts_with("ORGANIZER") || l.starts_with("ATTENDEE")).collect();
    assert_eq!(people.len(), 2, "{out}");
    assert!(people.iter().all(|l| l.contains("SCHEDULE-AGENT=CLIENT")), "{people:?}");
    assert!(out.contains("LOCATION:Room 2"), "{out}");
    // People MailCraft does not model here (a task's) stay, marked as scheduled by the client.
    let task = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:t@example.com\r\nSUMMARY:Report\r\n\
        ATTENDEE;SCHEDULE-AGENT=SERVER;SCHEDULE-FORCE-SEND=REQUEST:mailto:kim@example.com\r\n\
        END:VTODO\r\nEND:VCALENDAR\r\n";
    let base = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:t@example.com\r\nSUMMARY:Report\r\nEND:VTODO\r\n\
        END:VCALENDAR\r\n";
    let mine = base.replace("Report", "Report v2");
    let out = keep::patch_calendar(task, base, &mine, 0).unwrap();
    assert!(out.contains("ATTENDEE;SCHEDULE-AGENT=CLIENT:mailto:kim@example.com"), "{out}");
    assert!(!out.contains("SERVER") && !out.contains("FORCE-SEND"), "{out}");
}

/// `server` as read, then edited with `edit`, written in 3.0 and merged back on `server`.
fn edit_card(server: &str, edit: impl Fn(&mut mailcraft_model::Contact)) -> String {
    let read = crate::vcf::read_str(server).remove(0);
    let mut base = String::new();
    crate::vcf::write_one(&read, crate::vcf::Version::V3, &mut base);
    let mut edited = read;
    edit(&mut edited);
    let mut mine = String::new();
    crate::vcf::write_one(&edited, crate::vcf::Version::V3, &mut mine);
    unfolded(&keep::patch_card(server, &base, &mine).unwrap())
}

#[test]
fn edited_card_fields_replace_every_name_they_are_read_from() {
    // Regression: a field read from another client's name (X-EVOLUTION-SPOUSE, X-SKYPE) kept the old value
    // next to MailCraft's new line, so other clients went on showing it.
    let server = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Pat Example\r\nN:Example;Pat;;;\r\n\
        X-EVOLUTION-SPOUSE:Alex\r\nX-MS-SPOUSE:Alex\r\nRELATED;TYPE=child:Robin\r\n\
        X-SKYPE:alexim\r\nIMPP:skype:alexim\r\nX-AIM:other-account\r\n\
        item1.X-ABRELATEDNAMES:Alex\r\nitem1.X-ABLABEL:_$!<Spouse>!$_\r\nEND:VCARD\r\n";
    let out = edit_card(server, |c| {
        c.spouse = "Sam".into();
        c.im_address = "samim".into();
    });
    assert!(!out.contains("Alex") && !out.contains("alexim"), "the old values are gone: {out}");
    assert!(!out.contains("X-ABLABEL"), "the label of a removed line goes with it: {out}");
    assert!(out.contains("X-MAILCRAFT-SPOUSE:Sam") && out.contains("IMPP:samim"), "{out}");
    assert!(out.contains("RELATED;TYPE=child:Robin"), "another relation stays: {out}");
    assert!(out.contains("X-AIM:other-account"), "another IM account stays: {out}");
    let back = crate::vcf::read_str(&out).remove(0);
    assert_eq!((back.spouse.as_str(), back.im_address.as_str()), ("Sam", "samim"));
    // Unchanged fields keep the server's lines, whatever their name.
    let out = edit_card(server, |c| c.job_title = "Engineer".into());
    assert!(out.contains("X-EVOLUTION-SPOUSE:Alex") && out.contains("X-SKYPE:alexim"), "{out}");
    assert!(out.contains("item1.X-ABLABEL"), "{out}");
}

#[test]
fn lines_past_what_is_shown_survive_an_edit_of_the_ones_shown() {
    // Regression: editing the first e-mail address or the web page deleted the fourth address and the other
    // web pages, which MailCraft does not show.
    let server = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Pat Example\r\nN:Example;Pat;;;\r\n\
        EMAIL:one@example.com\r\nEMAIL:two@example.com\r\nEMAIL:three@example.com\r\nEMAIL:four@example.com\r\n\
        URL:https://www.example.com/pat\r\nURL:https://blog.example.org/\r\nEND:VCARD\r\n";
    let out = edit_card(server, |c| {
        if let Some(e) = c.emails.first_mut() {
            *e = "first@example.com".into();
        }
        c.web_page = "https://pat.example.net/".into();
    });
    assert!(!out.contains("one@example.com") && out.contains("first@example.com"), "{out}");
    assert!(out.contains("four@example.com"), "{out}");
    assert!(out.contains("https://blog.example.org/") && !out.contains("www.example.com"), "{out}");
    let back = crate::vcf::read_str(&out).remove(0);
    assert_eq!(back.emails, ["first@example.com", "two@example.com", "three@example.com"]);
    assert_eq!(back.web_page, "https://pat.example.net/");
    assert!(out.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\n"), "{out}");
}

#[test]
fn merging_many_distinct_names_takes_linear_time() {
    // Regression: properties and sub-components were matched by scanning every one for every name, so a
    // server copy with ~100,000 distinct names froze the session for minutes on each sync.
    let mut server = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n");
    for i in 0..60_000 {
        server.push_str(&format!("BEGIN:X-PART-{i}\r\nX-A:1\r\nEND:X-PART-{i}\r\n"));
    }
    server.push_str("BEGIN:VEVENT\r\nUID:p@example.com\r\nDTSTART:20261102T090000Z\r\n");
    for i in 0..9_000 {
        server.push_str(&format!("X-PROP-{i}:v\r\n"));
    }
    server.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
    let base = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:p@example.com\r\nDTSTART:20261102T090000Z\r\n\
        END:VEVENT\r\nEND:VCALENDAR\r\n";
    let mine = base.replace("END:VEVENT", "LOCATION:Room 2\r\nEND:VEVENT");
    let started = std::time::Instant::now();
    let out = keep::patch_calendar(&server, base, &mine, 0).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(30), "{:?}", started.elapsed());
    assert!(out.contains("BEGIN:X-PART-59999") && out.contains("X-PROP-8999:v") && out.contains("Room 2"));
}
