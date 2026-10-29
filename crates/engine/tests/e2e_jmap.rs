//! End to end, JMAP: an account on the local test server's JMAP endpoint (HTTPS, 127.0.0.1, synthetic users):
//! folders and mail from Mailbox/get and Email/query, later mail from Email/changes, local changes written
//! back with Email/set, and sending with EmailSubmission/set.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, FolderId, FolderRole, ItemId, Mailbox};
use mailcraft_testmail::{Config, TestServers};
use serde_json::{Value, json};

const ALICE: &str = "alice@example.com";
const ALICE_PW: &str = "alice-test-pw";
const BOB: &str = "bob@example.org";
const WAIT: Duration = Duration::from_secs(30);

fn message(subject: &str, id: &str) -> Vec<u8> {
    format!(
        "From: {BOB}\r\nTo: {ALICE}\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
         Message-ID: <{id}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Hello over JMAP.\r\n"
    )
    .into_bytes()
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn sync(s: &mut Session) {
    run(s, "sendReceive.all", json!({}));
    assert!(s.wait_for_sync(WAIT), "send/receive timed out: {:?}", s.sync_status());
}

fn settle(s: &mut Session) {
    assert!(s.wait_for_sync(WAIT), "timed out: {:?}", s.sync_status());
}

fn find(s: &Session, folder: FolderId, subject: &str) -> Option<ItemId> {
    s.mailbox.mail.items().find(|m| m.folder == folder && m.subject == subject).map(|m| m.id)
}

fn select(s: &mut Session, id: ItemId) {
    s.selection.folder = s.mailbox.mail.get(id).unwrap().folder;
    s.selection.items = vec![id];
    s.selection.focus = Some(id);
}

fn subjects(srv: &TestServers, user: &str, mailbox: &str) -> Vec<String> {
    srv.messages(user, mailbox)
        .unwrap()
        .iter()
        .filter_map(|m| mailcraft_mime::parse(&m.raw).ok().map(|x| x.subject()))
        .collect()
}

fn flags(srv: &TestServers, mailbox: &str, subject: &str) -> Vec<String> {
    srv.messages(ALICE, mailbox)
        .unwrap()
        .into_iter()
        .find(|m| mailcraft_mime::parse(&m.raw).map(|x| x.subject()).ok().as_deref() == Some(subject))
        .map(|m| m.flags)
        .unwrap_or_default()
}

#[test]
fn jmap_account_receives_writes_back_and_sends() {
    let srv = TestServers::start(Config {
        users: vec![(ALICE.into(), ALICE_PW.into()), (BOB.into(), "bob-pw".into())],
        add_trace_headers: false,
        ..Config::default()
    })
    .unwrap();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let r = run(
        &mut s,
        "accounts.add",
        json!({
            "kind": "jmap",
            "address": ALICE,
            "incoming": {"host": srv.jmap_url(), "extraRootPem": srv.cert_pem},
            "password": ALICE_PW,
        }),
    );
    let a = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    let t = run(&mut s, "accounts.test", json!({"account": a.0, "wait": true}));
    assert_eq!(t["ok"], true, "{t}");

    srv.seed_message(ALICE, "INBOX", &message("Hello JMAP", "hello"), &[]).unwrap();
    srv.create_mailbox(ALICE, "Projects", None).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    assert!(s.mailbox.folder(inbox).unwrap().remote_name.is_some(), "Inbox mapped to its mailbox id");
    assert!(s.mailbox.folders.values().any(|f| f.account == a && f.name == "Projects"));
    let hello = find(&s, inbox, "Hello JMAP").expect("first run downloads the mail");
    assert!(!s.mailbox.mail.get(hello).unwrap().read);

    // Later mail comes from Email/changes; flags set elsewhere arrive too.
    srv.seed_message(ALICE, "INBOX", &message("Second", "second"), &["\\Seen"]).unwrap();
    sync(&mut s);
    let second = find(&s, inbox, "Second").expect("Email/changes delivers new mail");
    assert!(s.mailbox.mail.get(second).unwrap().read);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Hello JMAP").count(), 1, "no duplicates");

    // Local changes reach the server (Email/set keywords and mailboxIds).
    select(&mut s, hello);
    run(&mut s, "mail.markRead", json!({"ids": [hello.0]}));
    select(&mut s, hello);
    run(&mut s, "mail.flag", json!({"ids": [hello.0]}));
    settle(&mut s);
    let f = flags(&srv, "INBOX", "Hello JMAP");
    assert!(f.contains(&"\\Seen".to_owned()) && f.contains(&"\\Flagged".to_owned()), "{f:?}");
    let archive = s.mailbox.role_folder(a, FolderRole::Archive).unwrap();
    select(&mut s, hello);
    run(&mut s, "mail.move", json!({"ids": [hello.0], "folder": archive.0}));
    settle(&mut s);
    assert!(subjects(&srv, ALICE, "Archive").contains(&"Hello JMAP".to_owned()));
    assert!(!subjects(&srv, ALICE, "INBOX").contains(&"Hello JMAP".to_owned()));
    sync(&mut s);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Hello JMAP").count(), 1);
    assert_eq!(s.mailbox.mail.get(hello).unwrap().folder, archive, "the moved item keeps its identity");

    // Permanent delete destroys the server copy.
    select(&mut s, second);
    run(&mut s, "mail.permanentDelete", json!({"ids": [second.0]}));
    settle(&mut s);
    assert!(!subjects(&srv, ALICE, "INBOX").contains(&"Second".to_owned()));

    // Sending: EmailSubmission/set delivers and the server files the copy in Sent.
    let c = run(&mut s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.setTo", json!({"compose": c, "value": BOB}));
    run(&mut s, "compose.setSubject", json!({"compose": c, "value": "Sent over JMAP"}));
    run(&mut s, "compose.insertText", json!({"compose": c, "text": "Hi Bob."}));
    let r = run(&mut s, "compose.send", json!({"compose": c}));
    assert_eq!(r["sent"], true, "{r}");
    sync(&mut s);
    settle(&mut s);
    let outbox = s.mailbox.role_folder(a, FolderRole::Outbox).unwrap();
    assert_eq!(s.mailbox.total_count(outbox), 0, "Outbox emptied");
    assert!(subjects(&srv, BOB, "INBOX").contains(&"Sent over JMAP".to_owned()), "Bob received it");
    assert!(subjects(&srv, ALICE, "Sent").contains(&"Sent over JMAP".to_owned()), "filed in Sent");
    let last = srv.submissions().last().cloned().unwrap();
    assert_eq!(last.rcpt_to, vec![BOB.to_owned()]);
    let sent = s.mailbox.role_folder(a, FolderRole::SentItems).unwrap();
    assert!(find(&s, sent, "Sent over JMAP").is_some(), "the server's copy came back into Sent Items");
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}
