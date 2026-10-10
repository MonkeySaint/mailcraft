//! Spec-conformance regressions found in review: [MS-OXMSG], [MS-OXRTFEX]/RTF, RFC 5545 recurrence examples,
//! RFC 4180 CSV.

use std::io::{Cursor, Read, Write};

use mailcraft_mime::Mailbox;
use mailcraft_model::{Appointment, DateTime, Recurrence};

use crate::ics::{self, ImportOptions, Rrule, rrule, value};
use crate::msg::{self, AttachmentContent, MsgAttachment, MsgMessage, rtfhtml};

// ---------------------------------------------------------------------------------------------------------
// .msg

fn streams_of(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut cf = cfb::CompoundFile::open(Cursor::new(bytes)).unwrap();
    let paths: Vec<String> =
        cf.walk().filter(|e| e.is_stream()).map(|e| e.path().to_string_lossy().into_owned()).collect();
    paths
        .into_iter()
        .map(|p| {
            let mut data = Vec::new();
            cf.open_stream(&p).unwrap().read_to_end(&mut data).unwrap();
            (p, data)
        })
        .collect()
}

/// [MS-OXMSG] 2.4.2.2: a PtypString value stream must hold at least one character (Outlook refuses to open a
/// file with a zero-length one).
#[test]
fn msg_writer_never_writes_empty_string_streams() {
    let m = MsgMessage {
        subject: String::new(),
        body_text: Some(String::new()),
        from: Some(Mailbox { name: None, addr: String::new() }),
        to: vec![Mailbox::new(None, "x@example.com")],
        attachments: vec![MsgAttachment {
            filename: String::new(),
            mime_type: String::new(),
            content_id: None,
            inline: false,
            content: AttachmentContent::Data(b"x".to_vec()),
        }],
        ..Default::default()
    };
    let bytes = msg::write(&m).unwrap();
    for (path, data) in streams_of(&bytes) {
        let name = path.rsplit('/').next().unwrap_or("");
        if name.starts_with("__substg1.0_") && name.len() == 20 && name.ends_with("001F") {
            assert!(!data.is_empty(), "zero-length string stream {path}");
        }
    }
    let back = msg::read(&bytes).unwrap();
    assert_eq!(back.subject, "");
    assert_eq!(back.to, m.to);
}

/// [MS-OXMSG] 2.4.2.2: the entry of an embedded message object has Size 0xFFFFFFFF and Reserved 0x01.
#[test]
fn msg_embedded_object_entry_reserved_field() {
    let inner = MsgMessage { subject: "inner".into(), ..Default::default() };
    let m = MsgMessage {
        subject: "outer".into(),
        attachments: vec![MsgAttachment {
            filename: "inner.msg".into(),
            mime_type: "message/rfc822".into(),
            content_id: None,
            inline: false,
            content: AttachmentContent::Message(Box::new(inner)),
        }],
        ..Default::default()
    };
    let bytes = msg::write(&m).unwrap();
    let streams = streams_of(&bytes);
    let (_, props) = streams
        .iter()
        .find(|(p, _)| p.ends_with("__attach_version1.0_#00000000/__properties_version1.0"))
        .unwrap();
    let entry = props[8..].chunks(16).find(|e| e[..4] == 0x3701_000Du32.to_le_bytes()).unwrap();
    assert_eq!(&entry[8..12], &0xFFFF_FFFFu32.to_le_bytes());
    assert_eq!(&entry[12..16], &1u32.to_le_bytes());
}

fn put(cf: &mut cfb::CompoundFile<Cursor<Vec<u8>>>, path: &str, data: &[u8]) {
    cf.create_stream(path).unwrap().write_all(data).unwrap();
}

fn entry(props: &mut Vec<u8>, tag: u32, value: [u8; 8]) {
    props.extend_from_slice(&tag.to_le_bytes());
    props.extend_from_slice(&6u32.to_le_bytes());
    props.extend_from_slice(&value);
}

/// A non-Unicode item in code page 1251 with one recipient and one attachment whose PidTagAttachmentHidden
/// value byte is 0 but whose (ignored) reserved bytes are not.
fn ansi_1251_item() -> Vec<u8> {
    let mut cf = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    let mut props = vec![0u8; 32];
    entry(&mut props, 0x3FFD_0003, 1251u64.to_le_bytes());
    put(&mut cf, "/__properties_version1.0", &props);
    put(&mut cf, "/__substg1.0_0037001E", b"\xcf\xf0\xe8\xe2\xe5\xf2"); // "Привет"
    cf.create_storage("/__recip_version1.0_#00000000").unwrap();
    let mut rp = vec![0u8; 8];
    entry(&mut rp, 0x0C15_0003, 1u64.to_le_bytes());
    put(&mut cf, "/__recip_version1.0_#00000000/__properties_version1.0", &rp);
    put(&mut cf, "/__recip_version1.0_#00000000/__substg1.0_3001001E", b"\xc8\xe2\xe0\xed"); // "Иван"
    put(&mut cf, "/__recip_version1.0_#00000000/__substg1.0_3003001E", b"ivan@example.com");
    cf.create_storage("/__attach_version1.0_#00000000").unwrap();
    let mut ap = vec![0u8; 8];
    entry(&mut ap, 0x3705_0003, 1u64.to_le_bytes());
    entry(&mut ap, 0x7FFE_000B, [0, 1, 0, 0, 0, 0, 0, 0]);
    put(&mut cf, "/__attach_version1.0_#00000000/__properties_version1.0", &ap);
    put(&mut cf, "/__attach_version1.0_#00000000/__substg1.0_3707001E", b"\xf4\xe0\xe9\xeb.txt"); // "файл.txt"
    put(&mut cf, "/__attach_version1.0_#00000000/__substg1.0_3712001E", b"part1@example.com");
    put(&mut cf, "/__attach_version1.0_#00000000/__substg1.0_37010102", b"data");
    cf.flush().unwrap();
    cf.into_inner().into_inner()
}

/// 8-bit strings in recipient and attachment objects use the message's code page; a PtypBoolean is one byte
/// ([MS-OXCDATA] 2.11.1) and the rest of the 8-byte value is reserved ([MS-OXMSG] 2.4.2.1.1).
#[test]
fn msg_sub_objects_use_message_code_page_and_boolean_is_one_byte() {
    let m = msg::read(&ansi_1251_item()).unwrap();
    assert_eq!(m.subject, "Привет");
    assert_eq!(m.to, vec![Mailbox::new(Some("Иван"), "ivan@example.com")]);
    let a = &m.attachments[0];
    assert_eq!(a.filename, "файл.txt");
    assert!(!a.inline, "PidTagAttachmentHidden is false");
}

// ---------------------------------------------------------------------------------------------------------
// RTF

#[test]
fn rtf_unicode_surrogate_pairs_and_binary_data() {
    // U+1F600 as a UTF-16 surrogate pair of signed \uN keywords, each with a one-byte fallback.
    let text = rtfhtml::extract_text(b"{\\rtf1\\ansi\\uc1 a\\u-10179?\\u-8704? b}");
    assert_eq!(text, "a\u{1F600} b");
    // A lone surrogate is dropped, not mis-paired with a later character.
    assert_eq!(rtfhtml::extract_text(b"{\\rtf1 x\\u-10179?y}"), "xy");
    // \binN: N bytes of binary data that may contain braces.
    let text = rtfhtml::extract_text(b"{\\rtf1 A{\\pict\\bin4 }{{x}B}");
    assert_eq!(text, "AB");
    let html = rtfhtml::extract_html(
        b"{\\rtf1\\fromhtml1 {\\*\\htmltag0 <p>}\\htmlrtf{\\pict\\bin2 {{}\\htmlrtf0 \\u-10179?\\u-8704?}",
    )
    .unwrap();
    assert_eq!(html, "<p>\u{1F600}");
}

// ---------------------------------------------------------------------------------------------------------
// RFC 5545 §3.8.5.3 recurrence examples (dates only; time zone is the identity).

fn occ(rule: &str, start: &str, max: usize, excluded: &[&str]) -> Vec<String> {
    let r = Rrule::parse(rule).unwrap();
    let (s, _) = value::parse_date_time(start).unwrap();
    let ex: Vec<i64> = excluded.iter().map(|e| value::parse_date_time(e).unwrap().0).collect();
    let ident = |l: i64| l;
    let excl = |l: i64, _: i64| ex.contains(&l);
    let x = rrule::Expand { rule: &r, dtstart: s, duration: 0, to_utc: &ident, excluded: &excl };
    let from = value::local_secs(1990, 1, 1, 0, 0, 0).unwrap();
    let to = value::local_secs(2100, 1, 1, 0, 0, 0).unwrap();
    rrule::expand(&x, from, to, max).into_iter().map(|o| value::format_date(o.local)).collect()
}

fn ds(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

#[test]
fn rfc5545_recurrence_examples() {
    let t = "T090000";
    let st = |d: &str| format!("{d}{t}");
    assert_eq!(
        occ("FREQ=DAILY;INTERVAL=10;COUNT=5", &st("19970902"), 50, &[]),
        ds("19970902 19970912 19970922 19971002 19971012")
    );
    let jan = occ(
        "FREQ=YEARLY;UNTIL=20000131T140000Z;BYMONTH=1;BYDAY=SU,MO,TU,WE,TH,FR,SA",
        &st("19980101"),
        200,
        &[],
    );
    assert_eq!(jan.len(), 93);
    assert_eq!(jan, occ("FREQ=DAILY;UNTIL=20000131T140000Z;BYMONTH=1", &st("19980101"), 200, &[]));
    assert_eq!(
        occ("FREQ=WEEKLY;INTERVAL=2;UNTIL=19971224T000000Z;WKST=SU;BYDAY=MO,WE,FR", &st("19970901"), 50, &[]),
        ds("19970901 19970903 19970905 19970915 19970917 19970919 19970929 19971001 19971003 19971013 \
            19971015 19971017 19971027 19971029 19971031 19971110 19971112 19971114 19971124 19971126 \
            19971128 19971208 19971210 19971212 19971222")
    );
    assert_eq!(
        occ("FREQ=MONTHLY;COUNT=10;BYDAY=1FR", &st("19970905"), 50, &[]),
        ds("19970905 19971003 19971107 19971205 19980102 19980206 19980306 19980403 19980501 19980605")
    );
    assert_eq!(
        occ("FREQ=MONTHLY;INTERVAL=2;COUNT=10;BYDAY=1SU,-1SU", &st("19970907"), 50, &[]),
        ds("19970907 19970928 19971102 19971130 19980104 19980125 19980301 19980329 19980503 19980531")
    );
    assert_eq!(
        occ("FREQ=MONTHLY;COUNT=6;BYDAY=-2MO", &st("19970922"), 50, &[]),
        ds("19970922 19971020 19971117 19971222 19980119 19980216")
    );
    assert_eq!(
        occ("FREQ=MONTHLY;BYMONTHDAY=-3", &st("19970928"), 6, &[]),
        ds("19970928 19971029 19971128 19971229 19980129 19980226")
    );
    assert_eq!(
        occ("FREQ=MONTHLY;COUNT=10;BYMONTHDAY=1,-1", &st("19970930"), 50, &[]),
        ds("19970930 19971001 19971031 19971101 19971130 19971201 19971231 19980101 19980131 19980201")
    );
    assert_eq!(
        occ("FREQ=MONTHLY;INTERVAL=18;COUNT=10;BYMONTHDAY=10,11,12,13,14,15", &st("19970910"), 50, &[]),
        ds("19970910 19970911 19970912 19970913 19970914 19970915 19990310 19990311 19990312 19990313")
    );
    assert_eq!(
        occ("FREQ=YEARLY;INTERVAL=2;COUNT=10;BYMONTH=1,2,3", &st("19970310"), 50, &[]),
        ds("19970310 19990110 19990210 19990310 20010110 20010210 20010310 20030110 20030210 20030310")
    );
    assert_eq!(
        occ("FREQ=YEARLY;INTERVAL=3;COUNT=10;BYYEARDAY=1,100,200", &st("19970101"), 50, &[]),
        ds("19970101 19970410 19970719 20000101 20000409 20000718 20030101 20030410 20030719 20060101")
    );
    assert_eq!(occ("FREQ=YEARLY;BYDAY=20MO", &st("19970519"), 3, &[]), ds("19970519 19980518 19990517"));
    assert_eq!(
        occ("FREQ=YEARLY;BYWEEKNO=20;BYDAY=MO", &st("19970512"), 3, &[]),
        ds("19970512 19980511 19990517")
    );
    assert_eq!(
        occ("FREQ=YEARLY;BYMONTH=3;BYDAY=TH", &st("19970313"), 8, &[]),
        ds("19970313 19970320 19970327 19980305 19980312 19980319 19980326 19990304")
    );
    assert_eq!(
        occ("FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13", &st("19970902"), 5, &[&st("19970902")]),
        ds("19980213 19980313 19981113 19990813 20001013")
    );
    assert_eq!(
        occ("FREQ=MONTHLY;BYDAY=SA;BYMONTHDAY=7,8,9,10,11,12,13", &st("19970913"), 5, &[]),
        ds("19970913 19971011 19971108 19971213 19980110")
    );
    assert_eq!(
        occ("FREQ=YEARLY;INTERVAL=4;BYMONTH=11;BYDAY=TU;BYMONTHDAY=2,3,4,5,6,7,8", &st("19961105"), 3, &[]),
        ds("19961105 20001107 20041102")
    );
    assert_eq!(
        occ("FREQ=MONTHLY;COUNT=3;BYDAY=TU,WE,TH;BYSETPOS=3", &st("19970904"), 50, &[]),
        ds("19970904 19971007 19971106")
    );
    assert_eq!(
        occ("FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-2", &st("19970929"), 7, &[]),
        ds("19970929 19971030 19971127 19971230 19980129 19980226 19980330")
    );
    assert_eq!(
        occ("FREQ=MONTHLY;BYMONTHDAY=15,30;COUNT=5", &st("20070115"), 50, &[]),
        ds("20070115 20070130 20070215 20070315 20070330")
    );
    assert_eq!(
        occ("FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=MO", &st("19970805"), 50, &[]),
        ds("19970805 19970810 19970819 19970824")
    );
    assert_eq!(
        occ("FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU", &st("19970805"), 50, &[]),
        ds("19970805 19970817 19970819 19970831")
    );
    // Week 1 can start in the previous year (ISO 8601 numbering with WKST=MO).
    assert_eq!(
        occ("FREQ=YEARLY;BYWEEKNO=1;BYDAY=MO;COUNT=3", &st("20141229"), 50, &[]),
        ds("20141229 20160104 20170102")
    );
    assert_eq!(
        occ("FREQ=YEARLY;BYWEEKNO=-1;BYDAY=SU;COUNT=3", &st("20201227"), 50, &[]),
        ds("20201227 20210103 20220102")
    );
    let r = Rrule::parse("FREQ=YEARLY;BYWEEKNO=20,-1;BYDAY=MO").unwrap();
    assert_eq!(Rrule::parse(&r.to_value()).unwrap(), r);
}

/// Daily and weekly rules stop at the supported calendar range like monthly and yearly ones.
#[test]
fn rrule_daily_weekly_stay_in_supported_years() {
    let last = value::local_secs(9999, 12, 31, 23, 59, 59).unwrap();
    for rule in ["FREQ=DAILY;INTERVAL=10000", "FREQ=WEEKLY;INTERVAL=10000"] {
        let r = Rrule::parse(rule).unwrap();
        let start = value::local_secs(2026, 1, 1, 9, 0, 0).unwrap();
        let ident = |l: i64| l;
        let never = |_: i64, _: i64| false;
        let x = rrule::Expand { rule: &r, dtstart: start, duration: 0, to_utc: &ident, excluded: &never };
        let all = rrule::expand(&x, start, i64::MAX / 4, 100_000);
        assert!(all.len() > 10, "{rule}");
        assert!(all.iter().all(|o| o.local <= last), "{rule} ran past year 9999");
    }
}

/// RFC 5545 §3.8.5.1: EXDATE removes RDATE instances too, and the recurrence set has no duplicates.
#[test]
fn rdates_are_excluded_and_deduplicated() {
    let cal = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:r@example.com\r\n\
DTSTART:20261001T090000Z\r\nDTEND:20261001T100000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\n\
RDATE:20261002T090000Z,20261010T090000Z,20261011T090000Z\r\nEXDATE:20261010T090000Z\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";
    let c = ics::read(cal.as_bytes(), ImportOptions::default()).unwrap();
    let e = &c.events[0];
    let from = DateTime::from_unix(0, 0);
    let to = DateTime::from_unix(4_000_000_000, 0);
    let got: Vec<String> =
        e.occurrences(&c.zones, from, to, 100).iter().map(|(s, _)| value::format_date(s.unix)).collect();
    assert_eq!(got, ds("20261001 20261002 20261003 20261011"));
    // The model-only expansion still works from the appointment alone.
    let a = Appointment {
        recurrence: Some(Recurrence { rrule: "FREQ=DAILY;COUNT=2".into(), exdates: vec![] }),
        ..e.appointment.clone()
    };
    assert_eq!(ics::expand_appointment(&a, from, to, 100).len(), 2);
}

// ---------------------------------------------------------------------------------------------------------
// CSV

#[test]
fn csv_delimiter_detection_ignores_quoted_text() {
    assert_eq!(crate::csv::detect_delimiter("\"Name, full, with, commas\";Email;Phone\r\na;b;c\r\n"), ';');
    assert_eq!(crate::csv::detect_delimiter("\"a;b;c;d\",x,y\n"), ',');
}

#[test]
fn csv_single_empty_field_rows_round_trip() {
    let rows = vec![vec!["a".to_owned()], vec![String::new()], vec!["b".to_owned()]];
    let text = crate::csv::write(&rows, ',');
    assert_eq!(crate::csv::parse(&text, ','), rows);
    // A truly blank line is still skipped.
    assert_eq!(crate::csv::parse("a\r\n\r\nb\r\n", ','), vec![vec!["a".to_owned()], vec!["b".to_owned()]]);
}

/// A zone of 64 observances with COUNT rules used to cost thousands of recurrence periods per lookup (about
/// a minute for one year of a daily event in a debug build); onset lists are now built once per expansion.
#[test]
fn hostile_zone_lookups_are_memoised() {
    let mut text = String::from("BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:Bad\n");
    for i in 0..64 {
        text.push_str(&format!(
            "BEGIN:STANDARD\nDTSTART:16010101T000000\nRRULE:FREQ=YEARLY;COUNT=4000000000;BYMONTH={}\n\
TZOFFSETFROM:+{:02}{:02}\nTZOFFSETTO:-{:02}{:02}\nEND:STANDARD\n",
            i % 12 + 1,
            i / 6,
            (i % 6) * 10,
            i / 6,
            (i % 6) * 10 + 5
        ));
    }
    text.push_str("END:VTIMEZONE\n");
    for i in 0..500 {
        text.push_str(&format!(
            "BEGIN:VEVENT\nUID:e{i}\nDTSTART;TZID=Bad:20260105T090000\nDTEND;TZID=Bad:20260105T100000\n\
RRULE:FREQ=DAILY\nEND:VEVENT\n"
        ));
    }
    text.push_str("END:VCALENDAR\n");
    let t0 = std::time::Instant::now();
    let cal = ics::read_str(&text, ImportOptions::default()).unwrap();
    assert_eq!(cal.events.len(), 500);
    let e = &cal.events[0];
    let occ = e.occurrences(
        &cal.zones,
        DateTime::from_unix(1_767_225_600, 0),
        DateTime::from_unix(1_798_761_600, 0),
        400,
    );
    assert_eq!(occ.len(), 361);
    assert!(t0.elapsed() < std::time::Duration::from_secs(20), "took {:?}", t0.elapsed());
}

// ---------------------------------------------------------------------------------------------------------
// vCard

/// vCard 2.1 quoted-printable line breaks (`=0D=0A`) decode to the model's `\n` line breaks.
#[test]
fn vcard_qp_line_breaks_are_normalised() {
    let card = "BEGIN:VCARD\r\nVERSION:2.1\r\nN:Lee;Ann\r\nNOTE;ENCODING=QUOTED-PRINTABLE:line one=0D=0A=\r\nline two=0Dthree\r\n\
END:VCARD\r\n";
    let c = crate::vcf::read_str(card);
    assert_eq!(c[0].notes, "line one\nline two\nthree");
}

// ---------------------------------------------------------------------------------------------------------
// Review round: time-zone gaps, hostile recurrence lists, organisation vCards, CSV phone cap.

/// RFC 5545 §3.3.5: a local time that does not exist (skipped by a forward shift) is interpreted with the
/// UTC offset in force before the transition. New York, 2026-03-08: 02:30 local is 07:30Z (03:30 EDT).
#[test]
fn nonexistent_local_time_uses_offset_before_transition() {
    let ny = ics::tz::builtin("America/New_York").unwrap();
    // The RFC's own examples: 20070311T023000 is 03:30 EDT; 20071104T013000 is 01:30 EDT.
    let rfc_gap = value::local_secs(2007, 3, 11, 2, 30, 0).unwrap();
    assert_eq!(ny.to_utc(rfc_gap), DateTime::from_civil(2007, 3, 11, 7, 30, 0, 0).unwrap().unix);
    let rfc_fold = value::local_secs(2007, 11, 4, 1, 30, 0).unwrap();
    assert_eq!(ny.to_utc(rfc_fold), DateTime::from_civil(2007, 11, 4, 5, 30, 0, 0).unwrap().unix);
    let gap = value::local_secs(2026, 3, 8, 2, 30, 0).unwrap();
    let want = DateTime::from_civil(2026, 3, 8, 7, 30, 0, 0).unwrap().unix;
    assert_eq!(ny.to_utc(gap), want);
    // Times either side of the gap are unaffected.
    let before = value::local_secs(2026, 3, 8, 1, 30, 0).unwrap();
    assert_eq!(ny.to_utc(before), DateTime::from_civil(2026, 3, 8, 6, 30, 0, 0).unwrap().unix);
    let after = value::local_secs(2026, 3, 8, 3, 30, 0).unwrap();
    assert_eq!(ny.to_utc(after), want);
    // Ambiguous (repeated) local time: the first occurrence (EDT).
    let fold = value::local_secs(2026, 11, 1, 1, 30, 0).unwrap();
    assert_eq!(ny.to_utc(fold), DateTime::from_civil(2026, 11, 1, 5, 30, 0, 0).unwrap().unix);

    let cal = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:g@example.com\r\n\
DTSTART;TZID=America/New_York:20260307T023000\r\nDTEND;TZID=America/New_York:20260307T033000\r\n\
RRULE:FREQ=DAILY;COUNT=3\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let c = ics::read(cal.as_bytes(), ImportOptions::default()).unwrap();
    let e = &c.events[0];
    let occ = e.occurrences(&c.zones, DateTime::from_unix(0, 0), DateTime::from_unix(4_000_000_000, 0), 10);
    let starts: Vec<(i64, i32)> = occ.iter().map(|(s, _)| (s.unix, s.offset_min)).collect();
    assert_eq!(
        starts,
        vec![
            (DateTime::from_civil(2026, 3, 7, 7, 30, 0, 0).unwrap().unix, -300),
            (want, -240),
            (DateTime::from_civil(2026, 3, 9, 6, 30, 0, 0).unwrap().unix, -240),
        ]
    );
    let gap_ev = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:h@example.com\r\n\
DTSTART;TZID=America/New_York:20260308T023000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let c = ics::read(gap_ev.as_bytes(), ImportOptions::default()).unwrap();
    assert_eq!(c.events[0].appointment.start.unix, want);
    assert_eq!(c.events[0].appointment.start.offset_min, -240);
}

/// A VTIMEZONE whose observance rules ended (UNTIL) long before the looked-up time still uses each rule's
/// last onset, not its first.
#[test]
fn ended_observance_rules_use_their_last_onset() {
    let text = "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:Old\r\n\
BEGIN:STANDARD\r\nDTSTART:19801102T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU;UNTIL=20001105T070000Z\r\n\
TZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nEND:STANDARD\r\n\
BEGIN:DAYLIGHT\r\nDTSTART:19800302T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=1SU;UNTIL=20010304T070000Z\r\n\
TZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\nEND:VCALENDAR\r\n";
    let c = ics::read(text.as_bytes(), ImportOptions::default()).unwrap();
    let z = c.zones.resolve("Old").unwrap();
    // The last change (March 2001) was to daylight time, which stays in force afterwards.
    assert_eq!(z.offset_at_local(value::local_secs(2026, 1, 15, 12, 0, 0).unwrap()), -240);
    assert_eq!(z.offset_at_local(value::local_secs(2000, 12, 1, 12, 0, 0).unwrap()), -300);
}

/// Long BY* lists (each capped at 400 values) used to cost |BYMONTHDAY| × |BYDAY days| per period; with COUNT
/// the expansion walks every period from DTSTART, so a year-1 DTSTART took minutes.
#[test]
fn long_by_lists_expand_quickly() {
    let days: Vec<String> = (0..400).map(|i| format!("{}", i % 28 + 1)).collect();
    let wds: Vec<String> = (0..400).map(|i| ["MO", "TU", "WE"][i % 3].to_owned()).collect();
    let rule = format!("FREQ=MONTHLY;COUNT=4000000000;BYMONTHDAY={};BYDAY={}", days.join(","), wds.join(","));
    let r = Rrule::parse(&rule).unwrap();
    let start = value::local_secs(1, 1, 1, 9, 0, 0).unwrap();
    let ident = |l: i64| l;
    let never = |_: i64, _: i64| false;
    let x = rrule::Expand { rule: &r, dtstart: start, duration: 0, to_utc: &ident, excluded: &never };
    let from = value::local_secs(9990, 1, 1, 0, 0, 0).unwrap();
    let to = value::local_secs(9990, 2, 1, 0, 0, 0).unwrap();
    let t0 = std::time::Instant::now();
    let got = rrule::expand(&x, from, to, 100);
    assert!(t0.elapsed() < std::time::Duration::from_secs(2), "took {:?}", t0.elapsed());
    // January 9990: Mondays, Tuesdays and Wednesdays on days 1..=28.
    assert_eq!(got.len(), 12);
    // Repeated values collapse; the written rule keeps the first-seen order.
    let r = Rrule::parse("FREQ=MONTHLY;BYDAY=TU,MO,TU,1MO;BYMONTHDAY=3,1,3").unwrap();
    assert_eq!(r.to_value(), "FREQ=MONTHLY;BYDAY=TU,MO,1MO;BYMONTHDAY=3,1");
}

/// An organisation card (no person name) reads back without its name split into first/middle/last.
#[test]
fn organisation_vcard_round_trips() {
    let c = mailcraft_model::Contact {
        company: "Acme Widgets Inc".into(),
        emails: vec!["sales@example.com".into()],
        file_as: "Acme Widgets Inc".into(),
        ..Default::default()
    };
    // A card with only an address: FN falls back to it.
    let e = mailcraft_model::Contact {
        emails: vec!["info@example.org".into()],
        file_as: "info@example.org".into(),
        ..Default::default()
    };
    for c in [c, e] {
        for v in [crate::vcf::Version::V3, crate::vcf::Version::V4] {
            let text = crate::vcf::write(std::slice::from_ref(&c), v);
            let back = crate::vcf::read(text.as_bytes()).unwrap();
            assert_eq!(back, vec![c.clone()], "{v:?}:\n{text}");
        }
    }
    // RFC 6350 KIND:org, and the FN = ORG convention of version 3 cards.
    for card in [
        "BEGIN:VCARD\r\nVERSION:4.0\r\nKIND:org\r\nFN:Example Holdings\r\nEND:VCARD\r\n",
        "BEGIN:VCARD\r\nVERSION:3.0\r\nN:;;;;\r\nFN:Example Holdings\r\nORG:Example Holdings\r\nEND:VCARD\r\n",
    ] {
        let c = crate::vcf::read_str(card);
        assert_eq!((c[0].first_name.as_str(), c[0].last_name.as_str()), ("", ""), "{card}");
        assert_eq!(c[0].company, "Example Holdings", "{card}");
    }
    // A person's FN without N is still split.
    let c = crate::vcf::read_str("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ann B. Lee\r\nEND:VCARD\r\n");
    assert_eq!(
        (c[0].first_name.as_str(), c[0].middle_name.as_str(), c[0].last_name.as_str()),
        ("Ann", "B.", "Lee")
    );
}

/// Phone cells holding many " ::: "-separated numbers stay within the per-contact phone cap.
#[test]
fn csv_phone_list_is_capped() {
    let numbers: Vec<String> = (0..5000).map(|i| format!("+1 555 {i:04}")).collect();
    let text = format!("Name,Phone\r\nAnn Lee,\"{}\"\r\n", numbers.join(" ::: "));
    let imp = crate::csv::import_contacts(text.as_bytes()).unwrap();
    assert_eq!(imp.contacts.len(), 1);
    assert_eq!(imp.contacts[0].phones.len(), 64);
}

/// Saved items use the 512-byte-sector compound file version (3), the form Outlook items use and every
/// [MS-CFB] reader accepts.
#[test]
fn msg_writer_uses_cfb_version_3() {
    let m = MsgMessage { subject: "Hi".into(), ..Default::default() };
    let bytes = msg::write(&m).unwrap();
    // Header: major version at offset 26, sector shift at 30 ([MS-CFB] 2.2).
    assert_eq!(bytes.get(26..28), Some(&[3u8, 0][..]));
    assert_eq!(bytes.get(30..32), Some(&[9u8, 0][..]));
    assert_eq!(msg::read(&bytes).unwrap().subject, "Hi");
}

/// A file defining ~100k time zones made every event's TZID lookup scan them all (minutes for 100k events);
/// only the first definitions of a TZID, up to a cap, are kept.
#[test]
fn many_vtimezones_are_capped() {
    let n = 99_000;
    let mut text = String::from("BEGIN:VCALENDAR\n");
    for i in 0..n {
        text.push_str(&format!("BEGIN:VTIMEZONE\nTZID:(UTC+01:00) City{i:06}\nEND:VTIMEZONE\n"));
    }
    let last = format!("(UTC+01:00) City{:06}", n - 1);
    for i in 0..n {
        text.push_str(&format!(
            "BEGIN:VEVENT\nUID:e{i}\nDTSTART;TZID=\"{last}\":20260105T090000\nEND:VEVENT\n"
        ));
    }
    text.push_str("END:VCALENDAR\n");
    let t0 = std::time::Instant::now();
    let cal = ics::read_str(&text, ImportOptions::default()).unwrap();
    assert!(t0.elapsed() < std::time::Duration::from_secs(5), "took {:?}", t0.elapsed());
    assert!(cal.zones.defined.len() <= ics::MAX_ZONES);
    assert_eq!(cal.events.len(), n);
    // The zone is still resolved (here from its fixed-offset name).
    assert_eq!(cal.events[0].appointment.start, DateTime::from_civil(2026, 1, 5, 9, 0, 0, 60).unwrap());
    // A repeated TZID keeps its first definition.
    let dup = "BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:X\nBEGIN:STANDARD\nDTSTART:19700101T000000\n\
TZOFFSETFROM:+0200\nTZOFFSETTO:+0200\nEND:STANDARD\nEND:VTIMEZONE\nBEGIN:VTIMEZONE\nTZID:x\nBEGIN:STANDARD\n\
DTSTART:19700101T000000\nTZOFFSETFROM:+0300\nTZOFFSETTO:+0300\nEND:STANDARD\nEND:VTIMEZONE\nEND:VCALENDAR\n";
    let cal = ics::read_str(dup, ImportOptions::default()).unwrap();
    assert_eq!(cal.zones.defined.len(), 1);
    assert_eq!(cal.zones.resolve("X").unwrap().offset_at_local(0), 120);
}

/// Linking overridden instances to their series compared every override with every event (20k × 50k long
/// UIDs took tens of seconds); series are now found through an index.
#[test]
fn many_overrides_link_quickly() {
    let prefix = "p".repeat(300);
    let mut text = String::from("BEGIN:VCALENDAR\n");
    for i in 0..50_000 {
        text.push_str(&format!(
            "BEGIN:VEVENT\nUID:{prefix}{i:06}\nDTSTART:20260105T090000Z\nRRULE:FREQ=DAILY\nEND:VEVENT\n"
        ));
    }
    for i in 0..20_000 {
        text.push_str(&format!(
            "BEGIN:VEVENT\nUID:{prefix}{:06}\nRECURRENCE-ID:20260106T090000Z\nDTSTART:20260106T100000Z\n\
END:VEVENT\n",
            49_999 - i % 2
        ));
    }
    text.push_str("END:VCALENDAR\n");
    let t0 = std::time::Instant::now();
    let cal = ics::read_str(&text, ImportOptions::default()).unwrap();
    assert!(t0.elapsed() < std::time::Duration::from_secs(5), "took {:?}", t0.elapsed());
    assert_eq!(cal.events.len(), 70_000);
    let last = cal.events[49_999].appointment.recurrence.as_ref().unwrap();
    assert_eq!(last.exdates, vec![DateTime::from_civil(2026, 1, 6, 9, 0, 0, 0).unwrap()]);
    assert!(cal.events[0].appointment.recurrence.as_ref().unwrap().exdates.is_empty());
}

/// Category lists are capped and deduplicated without rescanning the list for every value: a card with
/// thousands of CATEGORIES lines grew without bound (quadratic time), and an event's lines cost
/// |values| × 256 comparisons.
#[test]
fn category_lists_are_capped_and_fast() {
    let line = |n: usize, tag: &str| {
        let cats: Vec<String> = (0..n).map(|i| format!("{tag}{i:05}")).collect();
        cats.join(",")
    };
    let mut card = String::from("BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ann Lee\r\n");
    for l in 0..400 {
        card.push_str(&format!("CATEGORIES:{}\r\n", line(64, &format!("c{l:03}-"))));
    }
    card.push_str("END:VCARD\r\n");
    let c = crate::vcf::read_str(&card);
    assert_eq!(c[0].categories.len(), 256);
    assert_eq!(c[0].categories[0], "c000-00000");

    let mut ev = String::from("BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:c\nDTSTART:20260105T090000Z\n");
    for _ in 0..9_000 {
        ev.push_str(&format!("CATEGORIES:{}\n", line(255, "x")));
    }
    ev.push_str("CATEGORIES:Late,x00001\nEND:VEVENT\nEND:VCALENDAR\n");
    let t0 = std::time::Instant::now();
    let cal = ics::read_str(&ev, ImportOptions::default()).unwrap();
    let cats = &cal.events[0].appointment.categories;
    assert!(t0.elapsed() < std::time::Duration::from_secs(2), "took {:?}", t0.elapsed());
    assert_eq!(cats.len(), 256);
    assert_eq!(cats.last().map(String::as_str), Some("Late"));
}

/// Table cells and rows of a plain RTF body stay apart in the extracted text.
#[test]
fn rtf_table_text_keeps_cells_apart() {
    let rtf = b"{\\rtf1\\ansi\\trowd\\cellx1000\\cellx2000 Name\\cell Room\\cell\\row \\trowd\\cellx1000\\cellx2000 Ann\\cell 4\\cell\\row \\pard After\\par}";
    assert_eq!(rtfhtml::extract_text(rtf), "Name\tRoom\nAnn\t4\nAfter");
}
