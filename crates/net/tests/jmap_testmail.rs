//! The JMAP client against the in-process JMAP server from `mailcraft-testmail` (HTTPS on 127.0.0.1).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_net::http::HttpClient;
use mailcraft_net::jmap::{JmapAuth, JmapClient};
use mailcraft_net::{ConnectOptions, NetError};
use mailcraft_testmail::{Config, TestServers};
use serde_json::{Map, json};

const USER: &str = "alice@example.com";
const PASS: &str = "correct horse";

fn client(s: &TestServers, pass: &str) -> Result<JmapClient, NetError> {
    let opts = ConnectOptions {
        io_timeout: Duration::from_secs(20),
        ..ConnectOptions::trusting(s.cert_der.clone())
    };
    JmapClient::connect(
        &s.jmap_url(),
        JmapAuth::Basic { user: USER.into(), password: pass.into() },
        HttpClient::new(opts),
    )
}

#[test]
fn mailboxes_query_changes_set_import_and_submit() {
    let s = TestServers::start(Config {
        users: vec![(USER.into(), PASS.into()), ("bob@example.org".into(), "pw-bob".into())],
        add_trace_headers: false,
        ..Config::default()
    })
    .unwrap();
    assert!(matches!(client(&s, "wrong"), Err(NetError::Auth(_))));
    let c = client(&s, PASS).unwrap();
    assert_eq!(c.account_id(), "A");
    let (boxes, mstate) = c.mailboxes_with_state().unwrap();
    let inbox = boxes.iter().find(|b| b.role.as_deref() == Some("inbox")).unwrap().id.clone();
    let archive = boxes.iter().find(|b| b.role.as_deref() == Some("archive")).unwrap().id.clone();
    let (_, state0) = c.get_emails_with_state(&[], &["id"]).unwrap();
    s.seed_message(USER, "INBOX", b"Subject: one\r\nMessage-ID: <one@example.org>\r\n\r\nbody one\r\n", &[])
        .unwrap();
    let q = c.query_emails(json!({}), json!([]), 0, 10).unwrap();
    assert_eq!(q.ids.len(), 1);
    let id = q.ids[0].clone();
    let ch = c.changes("Email", &state0, None).unwrap();
    assert_eq!(ch.created, vec![id.clone()]);
    let got =
        c.get_emails(std::slice::from_ref(&id), Some(&["id", "blobId", "mailboxIds", "keywords"])).unwrap();
    let blob = got[0]["blobId"].as_str().unwrap().to_owned();
    assert!(
        String::from_utf8_lossy(&c.download(&blob, "m.eml", "message/rfc822").unwrap()).contains("body one")
    );
    // Keywords and a move keep the email id.
    c.set_keyword(std::slice::from_ref(&id), "$seen", true).unwrap();
    assert!(s.messages(USER, "INBOX").unwrap()[0].flags.iter().any(|f| f == "\\Seen"));
    c.move_emails(std::slice::from_ref(&id), &inbox, &archive).unwrap();
    assert_eq!(s.messages(USER, "Archive").unwrap().len(), 1);
    assert_eq!(c.query_emails(json!({}), json!([]), 0, 10).unwrap().ids, vec![id.clone()]);
    let ch = c.changes("Email", &ch.new_state, None).unwrap();
    assert_eq!(ch.updated, vec![id.clone()]);
    assert!(matches!(c.changes("Email", "999999", None), Err(NetError::Jmap { .. })));
    assert!(c.changes("Mailbox", &mstate, None).unwrap().created.is_empty());
    // Import, then send (delivered to Bob, filed in Sent).
    let raw = b"From: alice@example.com\r\nTo: bob@example.org\r\nSubject: hi bob\r\n\r\nhello\r\n";
    let ids = c.identities().unwrap();
    let drafts = boxes.iter().find(|b| b.role.as_deref() == Some("drafts")).unwrap().id.clone();
    let sent = boxes.iter().find(|b| b.role.as_deref() == Some("sent")).unwrap().id.clone();
    c.send_raw(&ids[0].id, raw, &drafts, &sent, None).unwrap();
    assert_eq!(s.messages("bob@example.org", "INBOX").unwrap().len(), 1);
    assert_eq!(s.messages(USER, "Sent").unwrap().len(), 1);
    assert!(s.messages(USER, "Drafts").unwrap().is_empty());
    assert!(!s.messages(USER, "Sent").unwrap()[0].flags.iter().any(|f| f == "\\Draft"));
    // Destroy.
    c.destroy_emails(&[id]).unwrap();
    assert!(s.messages(USER, "Archive").unwrap().is_empty());
    let r = c.set_emails(Map::new(), Map::new(), &["nope".to_owned()]).unwrap();
    assert!(r.not_destroyed.contains_key("nope"));
}

#[test]
fn quota_get_reports_mail_storage() {
    let s = TestServers::start(Config {
        users: vec![(USER.into(), PASS.into())],
        add_trace_headers: false,
        ..Config::default()
    })
    .unwrap();
    let c = client(&s, PASS).unwrap();
    assert_eq!(c.quota().unwrap(), None, "no limit set");
    s.set_quota(USER, Some(1_000_000)).unwrap();
    let raw = b"Subject: q\r\n\r\nbody\r\n";
    s.seed_message(USER, "INBOX", raw, &[]).unwrap();
    let q = c.quota().unwrap().expect("quota");
    assert_eq!(q.limit, 1_000_000);
    assert_eq!(q.used, raw.len() as u64);
    assert_eq!(q.messages, Some((1, 100_000)));
}

#[test]
fn quota_list_parsing_is_defensive() {
    use mailcraft_net::jmap::quota_from_list;
    assert_eq!(quota_from_list(&[]), None);
    let list = vec![
        json!({"resourceType": "octets", "used": 5, "hardLimit": 10, "types": ["Calendar"]}),
        json!({"resourceType": "octets", "used": "x", "hardLimit": 10}),
        json!({"resourceType": "octets", "used": 1, "hardLimit": 0}),
        json!({"resourceType": "octets", "used": 90, "hardLimit": 100, "types": ["Mail"]}),
        json!({"resourceType": "octets", "used": 10, "hardLimit": 1000, "types": []}),
        json!("garbage"),
    ];
    let q = quota_from_list(&list).unwrap();
    assert_eq!((q.used, q.limit), (90, 100), "the tightest mail quota wins");
    assert_eq!(q.messages, None);
}
