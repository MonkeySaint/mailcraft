use mailcraft_mime::Mailbox;
use mailcraft_model::{DateTime, FolderId, MailItem};

use crate::{eml, mbox};

const MSG: &[u8] = b"From: Ann Lee <ann@example.com>\r\nTo: bob@example.org\r\nSubject: Quarterly plan\r\n\
Date: Thu, 08 Oct 2026 09:30:00 -0400\r\nMessage-ID: <q1@example.com>\r\n\r\nHello Bob,\r\nFrom the plan...\r\n";

#[test]
fn eml_read_and_write_keep_source() {
    let item = eml::read(MSG, FolderId(4), DateTime::default()).unwrap();
    assert_eq!(item.subject, "Quarterly plan");
    assert_eq!(item.folder, FolderId(4));
    assert_eq!(item.message_id.as_deref(), Some("q1@example.com"));
    assert_eq!(eml::write(&item, Some(MSG), None), MSG);
}

#[test]
fn eml_rebuilds_without_source() {
    let item = MailItem {
        subject: "Rebuilt".into(),
        from: Some(Mailbox::new(Some("Ann"), "ann@example.com")),
        to: vec![Mailbox::new(None, "bob@example.org")],
        sent: Some(DateTime::from_unix(1_791_000_000, 0)),
        ..Default::default()
    };
    let body = mailcraft_model::Body { text: Some("Body text".into()), ..Default::default() };
    let bytes = eml::write(&item, None, Some(&body));
    let back = eml::read(&bytes, FolderId(1), DateTime::default()).unwrap();
    assert_eq!(back.subject, "Rebuilt");
    assert_eq!(back.to, item.to);
    let back_body = mailcraft_model::Body::from_source(&bytes).unwrap();
    assert_eq!(back_body.text.as_deref().map(str::trim), Some("Body text"));
}

#[test]
fn eml_file_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.eml");
    let item = eml::read(MSG, FolderId(1), DateTime::default()).unwrap();
    eml::write_file(&item, Some(MSG), None, &path).unwrap();
    let (back, source) = eml::read_file(&path, FolderId(2)).unwrap();
    assert_eq!(back.subject, item.subject);
    assert_eq!(source, MSG);
    assert!(eml::read_file(&dir.path().join("missing.eml"), FolderId(2)).is_err());
}

#[test]
fn mbox_round_trip_quotes_from_lines() {
    let a = eml::read(MSG, FolderId(1), DateTime::from_unix(5, 0)).unwrap();
    let b = MailItem { id: mailcraft_model::ItemId(2), ..a.clone() };
    let second: &[u8] = b"Subject: second\r\n\r\nFrom here on\r\n>From quoted\r\n";
    let bytes = mbox::write_bytes(&[&a, &b], |m| if m.id == b.id { second.to_vec() } else { MSG.to_vec() });
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.starts_with("From ann@example.com "));
    assert!(text.contains("\n>From here on"));
    assert!(text.contains("\n>>From quoted"));
    let back = mbox::read_bytes(&bytes, FolderId(9), DateTime::default()).unwrap();
    assert_eq!(back.items.len(), 2);
    assert!(!back.truncated);
    assert_eq!(back.items[0].0.subject, "Quarterly plan");
    let raw1 = String::from_utf8_lossy(&back.items[1].1).into_owned();
    assert!(raw1.contains("\nFrom here on"));
    assert!(raw1.contains("\n>From quoted"));
}

#[test]
fn mbox_cap_truncates() {
    let mut data = Vec::new();
    for i in 0..5 {
        data.extend_from_slice(
            format!("From x@example.com Thu Oct  8 04:30:00 2026\nSubject: m{i}\n\nbody\n\n").as_bytes(),
        );
    }
    let r = mbox::read_capped(&data[..], FolderId(1), DateTime::default(), 60).unwrap();
    assert!(r.truncated);
    assert!(r.items.len() < 5);
    let all = mbox::read_bytes(&data, FolderId(1), DateTime::default()).unwrap();
    assert_eq!(all.items.len(), 5);
    assert_eq!(all.items[4].0.subject, "m4");
    assert_eq!(
        Some(all.items[0].0.received.unix),
        DateTime::from_civil(2026, 10, 8, 4, 30, 0, 0).map(|d| d.unix)
    );
}

proptest::proptest! {
    #[test]
    fn eml_and_mbox_never_panic(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..1024)) {
        let item = eml::read(&data, FolderId(1), DateTime::default()).unwrap();
        let _ = eml::write(&item, Some(&data), None);
        let _ = eml::write(&item, None, None);
        let r = mbox::read_bytes(&data, FolderId(1), DateTime::default()).unwrap();
        let refs: Vec<&MailItem> = r.items.iter().map(|(m, _)| m).collect();
        let _ = mbox::write_bytes(&refs, |m| eml::write(m, None, None));
    }
}
