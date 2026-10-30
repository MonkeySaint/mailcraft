//! The Microsoft Graph client against the fake Graph mail service of `mailcraft-testmail` (HTTPS on
//! 127.0.0.1, synthetic users; nothing reaches a real Microsoft service).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_model::FolderRole;
use mailcraft_net::graph::GraphClient;
use mailcraft_net::graph::mail::{FileAttachment, FlagStatus, MessagePatch, UPLOAD_CHUNK};
use mailcraft_net::http::HttpClient;
use mailcraft_net::{ConnectOptions, NetError};
use mailcraft_testmail::{Config, TestServers};

const USER: &str = "alice@example.com";
const PASS: &str = "alice-graph-pw";
const BOB: &str = "bob@example.org";

fn client(s: &TestServers, token: &str) -> GraphClient {
    let opts = ConnectOptions {
        io_timeout: Duration::from_secs(20),
        ..ConnectOptions::trusting(s.cert_der.clone())
    };
    GraphClient::new(&s.graph_url(), token, HttpClient::new(opts)).unwrap()
}

fn msg(subject: &str) -> Vec<u8> {
    format!(
        "From: Bob <{BOB}>\r\nTo: {USER}\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
         Message-ID: <{}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nBody of {subject}.\r\n",
        subject.replace(' ', "-")
    )
    .into_bytes()
}

fn servers(page: usize) -> TestServers {
    TestServers::start(Config {
        users: vec![(USER.into(), PASS.into()), (BOB.into(), "bob-pw".into())],
        add_trace_headers: false,
        graph_max_page: page,
        ..Config::default()
    })
    .unwrap()
}

/// Reads a whole delta round: (messages, delta link).
fn delta_all(
    c: &GraphClient,
    folder: &str,
    link: Option<&str>,
) -> (Vec<mailcraft_net::graph::mail::GraphMessage>, String) {
    let mut out = Vec::new();
    let mut page = c.messages_delta(folder, link, None).unwrap();
    let mut rounds = 0;
    loop {
        out.extend(page.messages.clone());
        if let Some(d) = page.delta.clone() {
            return (out, d);
        }
        rounds += 1;
        assert!(rounds < 100);
        page = c.messages_delta(folder, page.next.as_deref(), None).unwrap();
    }
}

#[test]
fn folders_delta_paging_and_changes() {
    let s = servers(2);
    assert!(matches!(client(&s, "wrong").me(), Err(NetError::Auth(_))));
    let c = client(&s, PASS);
    assert_eq!(c.me().unwrap()["mail"], USER);
    s.create_mailbox(USER, "Projects/2026", None).unwrap();
    let folders = c.mail_folders().unwrap();
    let inbox = folders.iter().find(|f| f.role == Some(FolderRole::Inbox)).unwrap().clone();
    assert_eq!(inbox.display_name, "Inbox");
    assert!(folders.iter().any(|f| f.role == Some(FolderRole::SentItems)));
    let projects = folders.iter().find(|f| f.display_name == "Projects").unwrap();
    let sub = folders.iter().find(|f| f.display_name == "2026").unwrap();
    assert_eq!(
        sub.parent_folder_id.as_deref(),
        Some(projects.id.as_str()),
        "child folders under their parent"
    );
    assert_eq!(sub.depth, 1);

    for i in 0..5 {
        s.seed_message(USER, "INBOX", &msg(&format!("Hello {i}")), &[]).unwrap();
    }
    let (first, link) = delta_all(&c, &inbox.id, None);
    assert_eq!(first.len(), 5, "five messages over three pages");
    assert!(s.graph_requests().iter().any(|r| r.contains("skiptoken")), "paged");
    // Nothing changed: an empty round.
    let (none, link) = delta_all(&c, &inbox.id, Some(&link));
    assert!(none.is_empty());

    // Changes: read state, categories and a flag here; one message moved away, one new.
    let a = first[0].id.clone();
    let b = first[1].id.clone();
    let p = MessagePatch {
        is_read: Some(true),
        flag: Some((FlagStatus::Flagged, None, Some("2026-10-09T00:00:00".into()))),
        categories: Some(vec!["Blue category".into()]),
        importance: Some("high".into()),
        ..Default::default()
    };
    c.update_message(&a, &p).unwrap();
    let archive = folders.iter().find(|f| f.role == Some(FolderRole::Archive)).unwrap();
    assert_eq!(c.move_message(&b, &archive.id).unwrap(), b, "immutable ids survive moves");
    s.seed_message(USER, "INBOX", &msg("Later"), &[]).unwrap();
    let (changes, link) = delta_all(&c, &inbox.id, Some(&link));
    let changed = changes.iter().find(|m| m.id == a).unwrap();
    assert!(changed.is_read && changed.flag == FlagStatus::Flagged && changed.importance == "high");
    assert_eq!(changed.categories, vec!["Blue category".to_owned()]);
    assert!(changes.iter().any(|m| m.id == b && m.removed), "moved out: removed from the Inbox's delta");
    assert!(changes.iter().any(|m| m.subject == "Later" && !m.removed));
    let raw = c.message_mime(&a).unwrap();
    assert!(String::from_utf8_lossy(&raw).contains("Body of Hello"));
    assert_eq!(s.graph_meta(USER, &a)["categories"][0], "Blue category");

    // Delete for good; an unknown delta token asks for a new start (410).
    c.delete_message(&a).unwrap();
    c.delete_message(&a).unwrap();
    let (gone, _) = delta_all(&c, &inbox.id, Some(&link));
    assert!(gone.iter().any(|m| m.id == a && m.removed));
    let bad = link.replace("deltatoken=", "deltatoken=999");
    assert!(matches!(
        c.messages_delta(&inbox.id, Some(&bad), None),
        Err(NetError::Graph { status: 410, .. })
    ));

    // Folders made, renamed, moved and deleted.
    let made = c.create_folder(Some(&inbox.id), "Receipts").unwrap();
    assert!(s.mailboxes(USER).unwrap().contains(&"INBOX/Receipts".to_owned()));
    c.rename_folder(&made.id, "Bills").unwrap();
    c.move_folder(&made.id, "msgfolderroot").unwrap();
    assert!(s.mailboxes(USER).unwrap().contains(&"Bills".to_owned()));
    c.delete_folder(&made.id).unwrap();
    assert!(!s.mailboxes(USER).unwrap().contains(&"Bills".to_owned()));
}

#[test]
fn sending_drafts_attachments_and_throttling() {
    let s = servers(50);
    let c = client(&s, PASS);
    // sendMail with MIME: delivered, filed in Sent.
    let out = format!(
        "From: {USER}\r\nTo: {BOB}\r\nBcc: carol@example.net\r\nSubject: Over Graph\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain\r\n\r\nHi.\r\n"
    );
    c.send_mime(out.as_bytes()).unwrap();
    let bob = s.messages(BOB, "INBOX").unwrap();
    assert_eq!(bob.len(), 1);
    assert!(!String::from_utf8_lossy(&bob[0].raw).contains("carol"), "Bcc never reaches recipients");
    assert_eq!(s.messages(USER, "Sent").unwrap().len(), 1);

    // A draft from MIME, a large attachment through an upload session, then sent.
    let draft = c.create_from_mime(None, &msg("With file")).unwrap();
    assert!(draft.is_draft);
    let big = FileAttachment {
        name: "big.bin".into(),
        content_type: "application/octet-stream".into(),
        data: vec![42u8; UPLOAD_CHUNK + 4096],
        ..Default::default()
    };
    c.add_attachment(&draft.id, &big).unwrap();
    let small = FileAttachment {
        name: "note.txt".into(),
        content_type: "text/plain".into(),
        data: b"x".to_vec(),
        ..Default::default()
    };
    c.add_attachment(&draft.id, &small).unwrap();
    let with = c.message(&draft.id).unwrap();
    assert!(with.has_attachments, "the draft keeps its id while it grows");
    c.send_draft(&draft.id).unwrap();
    assert!(s.messages(USER, "Drafts").unwrap().is_empty());
    let sent = String::from_utf8_lossy(&s.messages(USER, "Sent").unwrap()[1].raw).into_owned();
    assert!(sent.contains("filename=\"big.bin\"") && sent.contains("filename=\"note.txt\""), "both attached");
    assert!(sent.len() > UPLOAD_CHUNK, "the whole large file arrived");

    // Throttling: waited out as asked, then the request goes through.
    s.graph_throttle(2, 0);
    assert_eq!(c.me().unwrap()["mail"], USER);
    assert_eq!(c.throttled(), 2);
    let mut impatient = client(&s, PASS);
    impatient.max_retries = 0;
    s.graph_throttle(1, 0);
    assert!(matches!(impatient.me(), Err(NetError::Graph { status: 429, .. })));
}
