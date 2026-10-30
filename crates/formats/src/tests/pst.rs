//! PST reading against synthetic files built from the [MS-PST] specification (`mailcraft_testmail::pst`).

use std::sync::Arc;

use mailcraft_model::{
    BodyFormat, BusyStatus, FlagState, FolderKind, FolderRole, Importance, NoteColor, TaskPriority,
    TaskStatus,
};
use mailcraft_testmail::pst::{
    Crypt, ItemDef, Key, PS_PUBLIC_STRINGS, PSETID_ADDRESS, PSETID_APPOINTMENT, PSETID_COMMON, PSETID_NOTE,
    PSETID_TASK, PstBuilder, Value,
};

use crate::pst::{self, Collect, Control, Options, PstItem, Report, Source};

/// 2026-03-02 09:00 UTC.
const T0: i64 = 1_772_442_000;

fn s(v: &str) -> Value {
    Value::Unicode(v.to_owned())
}

fn mail(subject: &str) -> ItemDef {
    ItemDef::new("IPM.Note")
        .tag(0x0037, s(subject))
        .tag(0x0042, s("Ada Example"))
        .tag(0x0065, s("ada@example.com"))
        .tag(0x0064, s("SMTP"))
        .tag(0x5D02, s("ada@example.com"))
        .tag(0x0E06, Value::Time(T0))
        .tag(0x0039, Value::Time(T0 - 60))
        .tag(0x1000, s(&format!("Body of {subject}.")))
        .recipient(1, "Bo Example", "bo@example.org")
}

/// A weekly recurrence pattern ([MS-OXOCAL] §2.2.1.44): every `weeks` weeks on `mask`, `count` times,
/// with `deleted` occurrences (minutes since 1601, local midnight).
fn weekly_blob(weeks: u32, mask: u32, count: u32, start_date: u32, deleted: &[u32]) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [0x3004u16, 0x3004, 0x200B, 0x0001, 0] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in [0u32, weeks, 0, mask, 0x2022, count, 1] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&(deleted.len() as u32).to_le_bytes());
    for d in deleted {
        b.extend_from_slice(&d.to_le_bytes());
    }
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&start_date.to_le_bytes());
    b.extend_from_slice(&0x5AE9_80DFu32.to_le_bytes());
    for v in [0x3006u32, 0x3009, 540, 600] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&0u16.to_le_bytes());
    b
}

/// Minutes since 1601 of a Unix time.
fn minutes(unix: i64) -> u32 {
    ((unix + 11_644_473_600) / 60) as u32
}

fn sample() -> PstBuilder {
    let mut b = PstBuilder::new("Archive 2024");
    let inbox = b.folder(None, "Inbox", "IPF.Note");
    let projects = b.folder(Some(inbox), "Projects", "IPF.Note");
    let sent = b.folder(None, "Sent Items", "IPF.Note");
    let deleted = b.folder(None, "Deleted Items", "IPF.Note");
    b.wastebasket = Some(deleted);
    let cal = b.folder(None, "Calendar", "IPF.Appointment");
    let contacts = b.folder(None, "Contacts", "IPF.Contact");
    let tasks = b.folder(None, "Tasks", "IPF.Task");
    let notes = b.folder(None, "Notes", "IPF.StickyNote");

    let first = mail("Quarterly numbers")
        .tag(0x0E07, Value::I32(1)) // read
        .tag(0x0017, Value::I32(2))
        .tag(0x1035, s("<q1@example.com>"))
        .tag(0x1090, Value::I32(2))
        .with(Key::Lid(PSETID_COMMON, 0x8530), s("Review"))
        .with(Key::Lid(PSETID_TASK, 0x8105), Value::Time(T0 + 86_400))
        .with(
            Key::Name(PS_PUBLIC_STRINGS, "Keywords".into()),
            Value::MvUnicode(vec!["Blue".into(), "Work".into()]),
        )
        .recipient(2, "Cy Example", "cy@example.net")
        .attach("numbers.csv", "text/csv", b"a,b\n1,2\n");
    b.item(inbox, first);
    let html = mail("Newsletter")
        .tag(0x1013, Value::Binary(b"<html><body><p>Hello <b>there</b></p></body></html>".to_vec()));
    b.item(inbox, html);
    b.item(projects, mail("Project kickoff").tag(0x1090, Value::I32(1)));
    b.item(sent, mail("Re: Quarterly numbers").tag(0x0E07, Value::I32(1)));
    b.item(deleted, mail("Old note"));

    let start = T0 + 7 * 86_400;
    let appt = ItemDef::new("IPM.Appointment")
        .tag(0x0037, s("Team sync"))
        .tag(0x1000, s("Weekly call."))
        .with(Key::Lid(PSETID_APPOINTMENT, 0x820D), Value::Time(start))
        .with(Key::Lid(PSETID_APPOINTMENT, 0x820E), Value::Time(start + 3600))
        .with(Key::Lid(PSETID_APPOINTMENT, 0x8208), s("Room 4"))
        .with(Key::Lid(PSETID_APPOINTMENT, 0x8205), Value::I32(3))
        .with(Key::Lid(PSETID_APPOINTMENT, 0x8223), Value::Bool(true))
        .with(
            Key::Lid(PSETID_APPOINTMENT, 0x8216),
            Value::Binary(weekly_blob(
                1,
                0b0000_0010,
                10,
                minutes(start - 9 * 3600),
                &[minutes(start + 7 * 86_400 - 9 * 3600)],
            )),
        )
        .with(Key::Lid(PSETID_COMMON, 0x8503), Value::Bool(true))
        .with(Key::Lid(PSETID_COMMON, 0x8501), Value::I32(10))
        .attach("agenda.txt", "text/plain", b"1. numbers");
    b.item(cal, appt);

    let contact = ItemDef::new("IPM.Contact")
        .tag(0x3A06, s("Dee"))
        .tag(0x3A11, s("Example"))
        .tag(0x3A16, s("Example Org"))
        .tag(0x3A1C, s("+1 555 0100"))
        .tag(0x3A42, Value::Time(631_152_000))
        .with(Key::Lid(PSETID_ADDRESS, 0x8083), s("dee@example.org"))
        .with(Key::Lid(PSETID_ADDRESS, 0x8046), s("Springfield"))
        .with(Key::Lid(PSETID_ADDRESS, 0x8005), s("Example, Dee"));
    b.item(contacts, contact);
    b.item(contacts, ItemDef::new("IPM.DistList").tag(0x0037, s("Team list")));

    let task = ItemDef::new("IPM.Task")
        .tag(0x0037, s("Send report"))
        .tag(0x0017, Value::I32(2))
        .with(Key::Lid(PSETID_TASK, 0x8101), Value::I32(1))
        .with(Key::Lid(PSETID_TASK, 0x8102), Value::Double(0.5))
        .with(Key::Lid(PSETID_TASK, 0x8105), Value::Time(T0 + 3 * 86_400));
    b.item(tasks, task);

    let note = ItemDef::new("IPM.StickyNote")
        .tag(0x1000, s("Buy milk"))
        .tag(0x3007, Value::Time(T0))
        .with(Key::Lid(PSETID_NOTE, 0x8B00), Value::I32(1));
    b.item(notes, note);
    b
}

fn read_bytes(file: Vec<u8>) -> (crate::Result<Report>, Collect) {
    let control = Arc::new(Control::default());
    let mut sink = Collect::default();
    let r = pst::read(Source::Bytes(file), &Options::default(), &control, &mut sink);
    (r, sink)
}

#[test]
fn reads_the_folder_tree_and_every_kind_of_item() {
    let (report, sink) = read_bytes(sample().build().unwrap());
    let report = report.unwrap();
    assert_eq!(report.name, "Archive 2024");
    let names: Vec<(&str, Option<usize>, FolderKind, Option<FolderRole>)> =
        sink.folders.iter().map(|f| (f.name.as_str(), f.parent, f.kind, f.role)).collect();
    assert_eq!(
        names,
        vec![
            ("Inbox", None, FolderKind::Mail, Some(FolderRole::Inbox)),
            ("Projects", Some(0), FolderKind::Mail, None),
            ("Sent Items", None, FolderKind::Mail, Some(FolderRole::SentItems)),
            ("Deleted Items", None, FolderKind::Mail, Some(FolderRole::DeletedItems)),
            ("Calendar", None, FolderKind::Calendar, Some(FolderRole::Calendar)),
            ("Contacts", None, FolderKind::Contacts, Some(FolderRole::Contacts)),
            ("Tasks", None, FolderKind::Tasks, Some(FolderRole::Tasks)),
            ("Notes", None, FolderKind::Notes, Some(FolderRole::Notes)),
        ]
    );
    assert_eq!(
        (report.mail, report.appointments, report.contacts, report.tasks, report.notes),
        (5, 1, 1, 1, 1)
    );
    assert_eq!(report.skipped_count, 1, "{:?}", report.skipped);
    assert!(report.skipped[0].reason.contains("contact groups"), "{:?}", report.skipped);
    assert_eq!(report.stopped, None);

    let mails: Vec<&(mailcraft_model::MailItem, mailcraft_store::Content)> = sink
        .items
        .iter()
        .filter_map(|(_, i)| match i {
            PstItem::Mail(m) => Some(m.as_ref()),
            _ => None,
        })
        .collect();
    let (q, content) = mails.iter().find(|(m, _)| m.subject == "Quarterly numbers").copied().unwrap();
    assert!(q.read);
    assert_eq!(q.importance, Importance::High);
    assert_eq!(q.message_id.as_deref(), Some("q1@example.com"));
    assert_eq!(q.categories, vec!["Blue".to_owned(), "Work".to_owned()]);
    assert_eq!(q.from.as_ref().map(|f| f.addr.as_str()), Some("ada@example.com"));
    assert_eq!(q.to.len(), 1);
    assert_eq!(q.cc.len(), 1);
    assert_eq!(q.received.unix, T0);
    match &q.flag {
        FlagState::Flagged { label, due, .. } => {
            assert_eq!(label, "Review");
            assert_eq!(due.map(|d| d.unix), Some(T0 + 86_400));
        }
        other => panic!("flag {other:?}"),
    }
    let raw = content.source.as_ref().unwrap();
    let parsed = mailcraft_mime::parse(raw).unwrap();
    assert_eq!(parsed.attachments().len(), 1);
    assert_eq!(parsed.attachments()[0].filename, "numbers.csv");
    assert_eq!(parsed.attachments()[0].data, b"a,b\n1,2\n");
    let text = String::from_utf8_lossy(raw);
    assert!(text.contains("Body of Quarterly numbers."));

    let (n, content) = mails.iter().find(|(m, _)| m.subject == "Newsletter").copied().unwrap();
    assert!(!n.read);
    assert_eq!(content.body.as_ref().unwrap().format, BodyFormat::Html);
    assert!(content.body.as_ref().unwrap().html.as_deref().unwrap().contains("<b>there</b>"));
    let (k, _) = mails.iter().find(|(m, _)| m.subject == "Project kickoff").copied().unwrap();
    assert!(matches!(k.flag, FlagState::Complete { .. }));

    for (_, item) in &sink.items {
        match item {
            PstItem::Appointment(a) => {
                assert_eq!(a.subject, "Team sync");
                assert_eq!(a.location, "Room 4");
                assert_eq!(a.start.unix, T0 + 7 * 86_400);
                assert_eq!(a.end.unix, T0 + 7 * 86_400 + 3600);
                assert_eq!(a.show_as, BusyStatus::OutOfOffice);
                assert_eq!(a.reminder_min, Some(10));
                let r = a.recurrence.as_ref().unwrap();
                assert_eq!(r.rrule, "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO;WKST=MO;COUNT=10");
                assert_eq!(r.exdates.len(), 1);
                assert_eq!(r.exdates[0].unix, T0 + 14 * 86_400);
                assert_eq!(a.attachments.len(), 1);
                assert_eq!(&*a.attachments[0].data, b"1. numbers");
            }
            PstItem::Contact(c) => {
                assert_eq!((c.first_name.as_str(), c.last_name.as_str()), ("Dee", "Example"));
                assert_eq!(c.file_as, "Example, Dee");
                assert_eq!(c.company, "Example Org");
                assert_eq!(c.emails, vec!["dee@example.org".to_owned()]);
                assert_eq!(c.phones, vec![("Mobile".to_owned(), "+1 555 0100".to_owned())]);
                assert_eq!(c.addresses.len(), 1);
                assert_eq!(c.addresses[0].city, "Springfield");
                assert_eq!(c.birthday.as_deref(), Some("1990-01-01"));
            }
            PstItem::Task(t) => {
                assert_eq!(t.subject, "Send report");
                assert_eq!(t.status, TaskStatus::InProgress);
                assert_eq!(t.percent_complete, 50);
                assert_eq!(t.priority, TaskPriority::High);
                assert_eq!(t.due.map(|d| d.unix), Some(T0 + 3 * 86_400));
            }
            PstItem::Note(n) => {
                assert_eq!(n.text, "Buy milk");
                assert_eq!(n.color, NoteColor::Green);
                assert_eq!(n.created.unix, T0);
            }
            PstItem::Mail(_) => {}
        }
    }
}

#[test]
fn reads_permute_encoded_files_large_values_and_attached_messages() {
    let mut b = PstBuilder::new("Encoded");
    b.crypt = Crypt::Permute;
    let inbox = b.folder(None, "Inbox", "IPF.Note");
    let long: String = "Lorem ipsum dolor sit amet. ".repeat(600); // over one block: a data tree
    let big: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
    let inner = mail("Inner message").tag(0x1000, s("Inside."));
    let outer = mail("Outer")
        .tag(0x1000, s(&long))
        .attach("big.bin", "application/octet-stream", &big)
        .attach_message("Inner message", inner);
    b.item(inbox, outer);
    let (report, sink) = read_bytes(b.build().unwrap());
    let report = report.unwrap();
    assert_eq!(report.mail, 1, "{:?}", report.skipped);
    let PstItem::Mail(m) = &sink.items[0].1 else { panic!("not mail") };
    let raw = m.1.source.as_ref().unwrap();
    let parsed = mailcraft_mime::parse(raw).unwrap();
    let atts = parsed.attachments();
    assert_eq!(
        atts.len(),
        1,
        "{:?} {:?}",
        atts.iter().map(|a| &a.filename).collect::<Vec<_>>(),
        report.skipped
    );
    assert_eq!(atts[0].data, big);
    // outlook-pst 1.2 cannot read a PtypObject property, which every attached message is stored as: the
    // message comes in without it and the report says so.
    assert_eq!(report.skipped_count, 1);
    assert_eq!(report.skipped[0].item, "Outer");
    assert!(
        report.skipped[0].reason.contains("attached message \"Inner message\" was left out"),
        "{:?}",
        report.skipped
    );
    let body = m.1.body.as_ref().unwrap().text.as_deref().unwrap();
    assert!(body.trim_end().ends_with("Lorem ipsum dolor sit amet."), "{}", body.len());
    assert!(body.len() >= long.trim_end().len());
}

#[test]
fn password_protected_and_foreign_files_are_refused_clearly() {
    let mut b = sample();
    b.store_props.push((Key::Tag(0x67FF), Value::I32(0x1234_5678)));
    let (r, sink) = read_bytes(b.build().unwrap());
    let e = r.unwrap_err().to_string();
    assert!(e.contains("password"), "{e}");
    assert!(sink.items.is_empty());

    let (r, _) = read_bytes(b"From: a@example.com\r\n\r\nhi".to_vec());
    assert!(r.unwrap_err().to_string().contains("not a PST file"));
    let mut ost = sample().build().unwrap();
    ost[8..10].copy_from_slice(b"SO");
    let (r, _) = read_bytes(ost);
    assert!(r.unwrap_err().to_string().contains("offline"));
    assert_eq!(pst::sniff(b"!BDN\0\0\0\0SM\x17\0"), Some(Ok(pst::Version::Unicode)));
    assert_eq!(pst::sniff(b"!BDN\0\0\0\0SM\x0e\0"), Some(Ok(pst::Version::Ansi)));
    assert!(matches!(pst::sniff(b"!BDN\0\0\0\0SM\x24\0"), Some(Err(_))));
    assert_eq!(pst::sniff(b"PK\x03\x04"), None);
}

#[test]
fn cancel_stops_the_walk_and_says_so() {
    let control = Arc::new(Control::default());
    struct StopAfterOne<'a>(&'a Control, Collect);
    impl pst::Sink for StopAfterOne<'_> {
        fn folder(&mut self, f: pst::PstFolder) -> bool {
            self.1.folder(f)
        }
        fn item(&mut self, folder: usize, item: PstItem) -> bool {
            self.0.cancel();
            self.1.item(folder, item)
        }
    }
    let mut sink = StopAfterOne(&control, Collect::default());
    let report =
        pst::read(Source::Bytes(sample().build().unwrap()), &Options::default(), &control, &mut sink)
            .unwrap();
    assert_eq!(report.stopped.as_deref(), Some("cancelled"));
    assert_eq!(sink.1.items.len(), 1);
    assert!(control.items_done.load(std::sync::atomic::Ordering::Relaxed) >= 1);
    assert!(control.bytes_read.load(std::sync::atomic::Ordering::Relaxed) > 0);
}

#[test]
fn the_item_cap_ends_the_read_with_a_note() {
    let control = Arc::new(Control::default());
    let mut sink = Collect::default();
    let options = Options { max_items: 2, ..Options::default() };
    let report = pst::read(Source::Bytes(sample().build().unwrap()), &options, &control, &mut sink).unwrap();
    assert_eq!(report.imported(), 2);
    assert_eq!(report.stopped.as_deref(), Some("too many items"));
}

#[test]
fn reads_from_a_file_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.pst");
    std::fs::write(&path, sample().build().unwrap()).unwrap();
    let control = Arc::new(Control::default());
    let mut sink = Collect::default();
    let report = pst::read(Source::Path(path), &Options::default(), &control, &mut sink).unwrap();
    assert_eq!(report.imported(), 9);
    assert!(
        pst::read(Source::Path(dir.path().to_path_buf()), &Options::default(), &control, &mut sink).is_err()
    );
}

/// Truncated and corrupted files: never a panic, never a hang; whatever is read is consistent.
#[test]
fn truncated_and_corrupted_files_never_panic() {
    let file = sample().build().unwrap();
    let started = std::time::Instant::now();
    let mut cuts: Vec<usize> = (0..file.len()).step_by(331).collect();
    cuts.extend([0, 1, 4, 12, 563, 564, 600, 0x4400, file.len() - 1]);
    for cut in cuts {
        let (r, sink) = read_bytes(file[..cut.min(file.len())].to_vec());
        if let Ok(report) = r {
            assert_eq!(report.imported(), sink.items.len());
        }
    }
    // Flip bytes across the file (header CRCs make most header flips fail cleanly; the rest reach the
    // B-trees, blocks, heaps and tables).
    let mut seed: u64 = 0x5eed_1234_abcd_0001;
    for i in 0..1500usize {
        let mut f = file.clone();
        for _ in 0..(1 + i % 4) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let at = 0x4600 + (seed as usize) % (f.len() - 0x4600);
            f[at] ^= (seed >> 32) as u8 | 1;
        }
        let (r, sink) = read_bytes(f);
        if let Ok(report) = r {
            assert_eq!(report.imported(), sink.items.len());
        }
    }
    // Wild values in every 4-byte slot of a small region (counts, sizes, ids).
    for at in (0x4600..file.len().min(0x4600 + 4096)).step_by(4) {
        for v in [0u32, 0xFFFF_FFFF, 0x8000_0000, 0x0000_FFFF] {
            let mut f = file.clone();
            f[at..at + 4].copy_from_slice(&v.to_le_bytes());
            let _ = read_bytes(f);
        }
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(240), "took {:?}", started.elapsed());
}

#[test]
fn recurrence_patterns_map_to_rules() {
    let start = minutes(T0 - 9 * 3600);
    let r = pst::recurrence_rule(&weekly_blob(2, 0b0010_1010, 5, start, &[]), 0).unwrap();
    assert_eq!(r.rrule, "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE,FR;WKST=MO;COUNT=5");
    assert!(r.exdates.is_empty());
    // Daily every 3 days, never ending.
    let mut b = Vec::new();
    for v in [0x3004u16, 0x3004, 0x200A, 0x0000, 0] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in [0u32, 3 * 1440, 0, 0x2023, 0, 0, 0, 0, start, 0x5AE9_80DF] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    assert_eq!(pst::recurrence_rule(&b, 0).unwrap().rrule, "FREQ=DAILY;INTERVAL=3");
    // Monthly on the second Tuesday, until a date.
    let mut b = Vec::new();
    for v in [0x3004u16, 0x3004, 0x200C, 0x0003, 0] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    let end = minutes(T0 + 90 * 86_400);
    for v in [0u32, 1, 0, 0b100, 2, 0x2021, 0, 0, 0, 0, start, end] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    let rule = pst::recurrence_rule(&b, 0).unwrap().rrule;
    assert!(rule.starts_with("FREQ=MONTHLY;INTERVAL=1;BYDAY=TU;BYSETPOS=2;UNTIL=2026"), "{rule}");
    // Damage is an error, not a panic.
    assert!(pst::recurrence_rule(&[], 0).is_err());
    assert!(pst::recurrence_rule(&b[..10], 0).is_err());
    let mut huge = weekly_blob(1, 1, 1, start, &[]);
    huge[38..42].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(pst::recurrence_rule(&huge, 0).is_err());
    let mut lunar = weekly_blob(1, 1, 1, start, &[]);
    lunar[6..8].copy_from_slice(&0x000Au16.to_le_bytes());
    assert!(pst::recurrence_rule(&lunar, 0).is_err());
}

/// Regression: a stop asked for before the header was read came back as an i/o error ("the import was
/// cancelled") instead of a stopped read.
#[test]
fn cancel_before_the_file_is_open_is_a_stop_not_an_error() {
    let control = Arc::new(Control::default());
    control.cancel();
    let mut sink = Collect::default();
    let report =
        pst::read(Source::Bytes(sample().build().unwrap()), &Options::default(), &control, &mut sink)
            .unwrap();
    assert_eq!(report.stopped.as_deref(), Some("cancelled"));
    assert!(sink.items.is_empty() && sink.folders.is_empty());
}

/// Regression: deleted occurrences were matched against the changed ones with a scan per date, so a pattern
/// listing 100,000 of each (about 800 KB) took some 10^10 comparisons, beyond any cancel or read budget.
#[test]
fn a_pattern_with_huge_deleted_and_changed_lists_is_read_in_linear_time() {
    let start = minutes(T0 - 9 * 3600);
    let dates: Vec<u32> = (0..100_000u32).map(|i| start + i * 1440).collect();
    let mut b = weekly_blob(1, 1, 5, start, &dates);
    // The changed list (after the deleted one): the same dates.
    let at = 42 + dates.len() * 4;
    let mut changed = (dates.len() as u32).to_le_bytes().to_vec();
    for d in &dates {
        changed.extend_from_slice(&d.to_le_bytes());
    }
    b.splice(at..at + 4, changed);
    let t = std::time::Instant::now();
    let r = pst::recurrence_rule(&b, 0).unwrap();
    assert!(r.exdates.is_empty(), "every deleted date was changed, not deleted");
    assert!(t.elapsed() < std::time::Duration::from_secs(20), "{:?}", t.elapsed());
    // Only deleted dates: at most 10,000 are kept.
    let r = pst::recurrence_rule(&weekly_blob(1, 1, 5, start, &dates), 0).unwrap();
    assert_eq!(r.exdates.len(), 10_000);
}

/// PSETID_Meeting.
const PSETID_MEETING: [u8; 16] =
    [0x90, 0xDA, 0xD8, 0x6E, 0x0B, 0x45, 0x1B, 0x10, 0x98, 0xDA, 0x00, 0xAA, 0x00, 0x3F, 0x13, 0x05];

/// A PidLidGlobalObjectId ([MS-OXOCAL] §2.2.1.27) with an instance date and `data`.
fn object_id(data: &[u8]) -> Vec<u8> {
    let mut b =
        vec![0x04, 0x00, 0x00, 0x00, 0x82, 0x00, 0xE0, 0x00, 0x74, 0xC5, 0xB7, 0x10, 0x1A, 0x82, 0xE0, 0x08];
    b.extend_from_slice(&[0x07, 0xEA, 0x03, 0x02]); // instance date 2026-03-02
    b.extend_from_slice(&[0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80]);
    b.extend_from_slice(&[0; 8]);
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(data);
    b
}

fn appointment(subject: &str) -> ItemDef {
    ItemDef::new("IPM.Appointment")
        .tag(0x0037, s(subject))
        .with(Key::Lid(PSETID_APPOINTMENT, 0x820D), Value::Time(T0))
        .with(Key::Lid(PSETID_APPOINTMENT, 0x820E), Value::Time(T0 + 1800))
}

fn uids(file: Vec<u8>) -> Vec<(String, String)> {
    let (r, sink) = read_bytes(file);
    r.unwrap();
    sink.items
        .iter()
        .filter_map(|(_, i)| match i {
            PstItem::Appointment(a) => Some((a.subject.clone(), a.uid.clone())),
            _ => None,
        })
        .collect()
}

/// Regression: calendar items got `pst-<node id>-<folder count>` UIDs, the same in every PST file (node ids
/// are handed out in order) and on every run, so CalDAV sync matched an item of one file with the server
/// copy of another's and replaced it; meetings lost their own UID.
#[test]
fn calendar_items_keep_their_object_id_as_uid_and_others_never_repeat() {
    let mut b = PstBuilder::new("Meetings");
    let cal = b.folder(None, "Calendar", "IPF.Appointment");
    let raw = object_id(&[0xAB, 0xCD]);
    b.item(cal, appointment("Clean id").with(Key::Lid(PSETID_MEETING, 0x0023), Value::Binary(raw.clone())));
    b.item(cal, appointment("Global id").with(Key::Lid(PSETID_MEETING, 0x0003), Value::Binary(raw.clone())));
    let mut vcal = b"vCal-Uid\x01\x00\x00\x00".to_vec();
    vcal.extend_from_slice(b"meeting-42@example.com\0");
    b.item(
        cal,
        appointment("Carried uid").with(Key::Lid(PSETID_MEETING, 0x0003), Value::Binary(object_id(&vcal))),
    );
    b.item(
        cal,
        appointment("Damaged id").with(Key::Lid(PSETID_MEETING, 0x0003), Value::Binary(vec![1, 2, 3])),
    );
    b.item(cal, appointment("No id"));
    let got = uids(b.build().unwrap());
    let uid = |subject: &str| got.iter().find(|(s, _)| s == subject).map(|(_, u)| u.clone()).unwrap();
    let hex = |b: &[u8]| b.iter().map(|b| format!("{b:02X}")).collect::<String>();
    assert_eq!(uid("Clean id"), hex(&raw));
    // The instance date (bytes 16..20) is cleared, as in the clean id.
    let mut clean = raw.clone();
    clean[16..20].fill(0);
    assert_eq!(uid("Global id"), hex(&clean));
    assert_eq!(uid("Carried uid"), "meeting-42@example.com");
    assert!(uid("Damaged id").ends_with("@mailcraft.invalid"));
    assert!(uid("No id").ends_with("@mailcraft.invalid"));

    // Two different files (same node ids, same layout), and one file read twice: no UID repeats.
    let file = |subject: &str| {
        let mut b = PstBuilder::new("Calendar file");
        let cal = b.folder(None, "Calendar", "IPF.Appointment");
        b.item(cal, appointment(subject));
        b.item(cal, appointment(&format!("{subject} again")));
        b.build().unwrap()
    };
    let mut all: Vec<String> = Vec::new();
    for f in [file("First file"), file("Second file"), file("First file")] {
        all.extend(uids(f).into_iter().map(|(_, u)| u));
    }
    assert_eq!(all.len(), 6);
    let distinct: std::collections::HashSet<&String> = all.iter().collect();
    assert_eq!(distinct.len(), all.len(), "{all:?}");
}
