//! End to end, accounts: IMAP IDLE push, POP3 accounts (receive, leave on server, delete, headers), header-only
//! download then full download, Send/Receive group membership, and account options kept in the data folder.
//! Against the local test servers only (`mailcraft-testmail`, 127.0.0.1, synthetic users); every wait has a
//! timeout.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::{Duration, Instant};

use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, FolderId, FolderRole, ItemId, Mailbox};
use mailcraft_engine::sync::{PushMode, cmds};
use mailcraft_testmail::{Config, TestServers};
use serde_json::{Value, json};

const ALICE: &str = "alice@example.com";
const ALICE_PW: &str = "alice-test-pw";
const BOB: &str = "bob@example.org";
const BOB_PW: &str = "bob-test-pw";
const WAIT: Duration = Duration::from_secs(30);

fn servers() -> TestServers {
    TestServers::start(Config {
        users: vec![(ALICE.into(), ALICE_PW.into()), (BOB.into(), BOB_PW.into())],
        add_trace_headers: false,
        ..Config::default()
    })
    .unwrap()
}

fn message(subject: &str, id: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {BOB}\r\nTo: {ALICE}\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
         Message-ID: <{id}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         {body}\r\n"
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

fn session() -> Session {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    s
}

fn id_of(v: &Value) -> AccountId {
    AccountId(u32::try_from(v["account"].as_u64().unwrap()).unwrap())
}

fn add_imap(s: &mut Session, srv: &TestServers, extra: Value) -> AccountId {
    let mut p = json!({
        "kind": "imap",
        "address": ALICE,
        "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
        "outgoing": {"host": "127.0.0.1", "port": srv.smtp_port, "security": "starttls", "extraRootPem": srv.cert_pem},
        "password": ALICE_PW,
    });
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    id_of(&run(s, "accounts.add", p))
}

fn add_pop3(s: &mut Session, srv: &TestServers, extra: Value) -> AccountId {
    let mut p = json!({
        "kind": "pop3",
        "address": ALICE,
        "incoming": {"host": "127.0.0.1", "port": srv.pop3s_port, "security": "tls", "extraRootPem": srv.cert_pem},
        "outgoing": {"host": "127.0.0.1", "port": srv.smtp_port, "security": "starttls", "extraRootPem": srv.cert_pem},
        "password": ALICE_PW,
    });
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    id_of(&run(s, "accounts.add", p))
}

fn inbox(s: &Session, a: AccountId) -> FolderId {
    s.mailbox.role_folder(a, FolderRole::Inbox).unwrap()
}

fn find(s: &Session, folder: FolderId, subject: &str) -> Option<ItemId> {
    s.mailbox.mail.items().find(|m| m.folder == folder && m.subject == subject).map(|m| m.id)
}

fn body(s: &Session, id: ItemId) -> String {
    s.body(id).unwrap().and_then(|b| b.text.clone()).unwrap_or_default()
}

/// Polls the session until `done` holds (or panics after `WAIT`).
fn poll_until(s: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    let end = Instant::now() + WAIT;
    loop {
        s.poll_sync();
        if done(s) {
            return;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}: {:?}", s.sync_status());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn idle_push_delivers_new_mail_without_send_receive() {
    let srv = servers();
    let mut s = session();
    let a = add_imap(&mut s, &srv, json!({}));
    sync(&mut s);
    // After a run that worked, the worker IDLEs on the Inbox.
    poll_until(&mut s, "push to start", |s| s.push_state(a).0 == PushMode::Idle);
    assert_eq!(s.sync_status().accounts[0].push, PushMode::Idle);
    srv.seed_message(ALICE, "INBOX", &message("Pushed", "pushed", "arrived by itself"), &[]).unwrap();
    let ib = inbox(&s, a);
    poll_until(&mut s, "the pushed message", |s| find(s, ib, "Pushed").is_some());
    // Push stops when working offline and starts again online.
    run(&mut s, "sendReceive.workOffline", json!({}));
    poll_until(&mut s, "push to stop", |s| s.push_state(a).0 == PushMode::Off);
    srv.seed_message(ALICE, "INBOX", &message("While offline", "offline", "waits"), &[]).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    s.poll_sync();
    assert!(find(&s, ib, "While offline").is_none(), "nothing arrives offline");
    run(&mut s, "sendReceive.workOffline", json!({}));
    poll_until(&mut s, "push to restart", |s| s.push_state(a).0 == PushMode::Idle);
    srv.seed_message(ALICE, "INBOX", &message("Back online", "online", "pushed again"), &[]).unwrap();
    poll_until(&mut s, "mail after going online", |s| find(s, ib, "Back online").is_some());
    // Turning push off in the account options stops it.
    run(&mut s, "accounts.options", json!({"account": a.0, "push": false}));
    poll_until(&mut s, "push off", |s| s.push_state(a).0 == PushMode::Off);
}

/// Issue #16: mail that reaches the Inbox after the run synced it but before push starts IDLE (here: right
/// after the run, before the worker even switches to push) arrives by push, not at the next scheduled check.
#[test]
fn push_delivers_mail_that_arrived_before_idle_started() {
    let srv = servers();
    let mut s = session();
    let a = add_imap(&mut s, &srv, json!({}));
    sync(&mut s);
    srv.seed_message(ALICE, "INBOX", &message("In the gap", "gap", "between sync and IDLE"), &[]).unwrap();
    let ib = inbox(&s, a);
    poll_until(&mut s, "mail from the gap", |s| find(s, ib, "In the gap").is_some());
    assert_eq!(s.push_state(a).0, PushMode::Idle);
    // And push keeps working afterwards.
    srv.seed_message(ALICE, "INBOX", &message("After the gap", "gap2", "pushed"), &[]).unwrap();
    poll_until(&mut s, "the next pushed message", |s| find(s, ib, "After the gap").is_some());
}

#[test]
fn pop3_receive_keeps_server_copies_then_removes_them() {
    let srv = servers();
    let mut s = session();
    let a = add_pop3(&mut s, &srv, json!({}));
    let listed = run(&mut s, "accounts.list", json!({}));
    let row = listed["accounts"].as_array().unwrap().iter().find(|r| r["id"] == a.0).unwrap().clone();
    assert_eq!(row["kind"], "pop3");
    assert_eq!(row["incoming"]["port"], srv.pop3s_port);
    assert_eq!(row["options"]["leaveOnServer"], true);
    let t = run(&mut s, "accounts.test", json!({"account": a.0, "wait": true}));
    assert_eq!(t["ok"], true, "{t}");
    assert!(t["incoming"].as_str().unwrap().contains("POP3"));

    srv.seed_message(ALICE, "INBOX", &message("First", "p1", "one"), &[]).unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Second", "p2", "two"), &[]).unwrap();
    sync(&mut s);
    let ib = inbox(&s, a);
    let first = find(&s, ib, "First").expect("POP3 message received");
    assert!(body(&s, first).contains("one"));
    assert!(find(&s, ib, "Second").is_some());
    assert_eq!(srv.messages(ALICE, "INBOX").unwrap().len(), 2, "copies stay on the server");
    // Nothing reaches the server for local changes of a POP3 account.
    run(&mut s, "folder.new", json!({"name": "Local only", "parent": ib.0}));
    assert!(s.mailbox.account(a).unwrap().sync.pending.is_empty());
    // No duplicates, and a message deleted here does not come back.
    s.selection.folder = ib;
    s.selection.items = vec![first];
    run(&mut s, "mail.permanentDelete", json!({"ids": [first.0]}));
    sync(&mut s);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Second").count(), 1);
    assert!(find(&s, ib, "First").is_none(), "a deleted POP3 message came back");
    // Without "leave a copy on the server" the next run deletes what is already here.
    run(&mut s, "accounts.options", json!({"account": a.0, "leaveOnServer": false}));
    sync(&mut s);
    assert!(srv.messages(ALICE, "INBOX").unwrap().is_empty(), "server copies removed");
    assert!(find(&s, ib, "Second").is_some(), "the local copy stays");
    // Sending goes out over SMTP.
    srv.seed_message(ALICE, "INBOX", &message("Third", "p3", "three"), &[]).unwrap();
    sync(&mut s);
    assert!(find(&s, ib, "Third").is_some());
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

#[test]
fn pop3_headers_then_marked_download() {
    let srv = servers();
    let mut s = session();
    let a = add_pop3(&mut s, &srv, json!({"mode": "headers"}));
    srv.seed_message(ALICE, "INBOX", &message("Big one", "big", "the large body"), &[]).unwrap();
    sync(&mut s);
    let ib = inbox(&s, a);
    let big = find(&s, ib, "Big one").expect("header received");
    assert!(cmds::is_header_only(&s, big));
    assert!(!body(&s, big).contains("large body"));
    s.selection.folder = ib;
    s.selection.items = vec![big];
    let r = cmds::mark(&mut s, &json!({})).unwrap();
    assert_eq!(r["marked"], json!([big.0]));
    let r = cmds::process_marked(&mut s, &json!({})).unwrap();
    assert_eq!(r["requested"], 1);
    assert!(s.wait_for_sync(WAIT));
    assert!(!cmds::is_header_only(&s, big));
    assert!(body(&s, big).contains("the large body"));
    assert_eq!(cmds::marked(&s, big), None);
    // Marked without "copy": the server copy goes with the next run.
    assert_eq!(srv.messages(ALICE, "INBOX").unwrap().len(), 1);
    sync(&mut s);
    assert!(srv.messages(ALICE, "INBOX").unwrap().is_empty());
}

#[test]
fn imap_headers_first_then_full_download() {
    let srv = servers();
    let mut s = session();
    let a = add_imap(&mut s, &srv, json!({}));
    let r = cmds::preferences(&mut s, &json!({"account": a.0, "mode": "headers"})).unwrap();
    assert_eq!(r["mode"], "headers");
    srv.seed_message(ALICE, "INBOX", &message("Report", "report", "quarterly numbers"), &["\\Seen"]).unwrap();
    sync(&mut s);
    let ib = inbox(&s, a);
    let report = find(&s, ib, "Report").expect("header downloaded");
    assert!(cmds::is_header_only(&s, report));
    assert!(s.mailbox.mail.get(report).unwrap().read, "flags come with the header");
    assert!(!body(&s, report).contains("quarterly"));
    assert!(s.mailbox.mail.get(report).unwrap().size > 0);
    // Mark (copy) and process: the whole message replaces the header in place.
    let r = cmds::mark(&mut s, &json!({"ids": [report.0], "copy": true})).unwrap();
    assert_eq!(r["copy"], true);
    cmds::process_marked(&mut s, &json!({"allFolders": true})).unwrap();
    assert!(s.wait_for_sync(WAIT));
    assert!(body(&s, report).contains("quarterly numbers"));
    assert!(!cmds::is_header_only(&s, report));
    assert!(s.mailbox.mail.get(report).unwrap().read);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Report").count(), 1);
    // Download Headers with the full preference: one header-only run.
    cmds::preferences(&mut s, &json!({"account": a.0, "mode": "full"})).unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Second", "second", "second body"), &[]).unwrap();
    cmds::download_headers(&mut s, &json!({"folder": ib.0})).unwrap();
    assert!(s.wait_for_sync(WAIT));
    let second = find(&s, ib, "Second").unwrap();
    assert!(cmds::is_header_only(&s, second));
    // Headers and then full items: both in one run.
    cmds::preferences(&mut s, &json!({"account": a.0, "mode": "headersThenFull"})).unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Third", "third", "third body"), &[]).unwrap();
    sync(&mut s);
    let third = find(&s, ib, "Third").unwrap();
    assert!(body(&s, third).contains("third body"));
    assert!(!cmds::is_header_only(&s, third));
    assert!(cmds::is_header_only(&s, second), "older header-only items wait for a mark");
}

#[test]
fn send_receive_group_membership() {
    let srv = servers();
    let mut s = session();
    let alice = add_imap(&mut s, &srv, json!({}));
    let bob = id_of(&run(
        &mut s,
        "accounts.add",
        json!({
            "kind": "imap",
            "address": BOB,
            "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
            "password": BOB_PW,
            "include": false,
        }),
    ));
    srv.seed_message(BOB, "INBOX", &message("For Bob", "bob", "hi"), &[]).unwrap();
    let r = run(&mut s, "sendReceive.all", json!({}));
    assert_eq!(r["started"], 1, "only Alice is in All Accounts: {r}");
    assert!(s.wait_for_sync(WAIT));
    assert!(find(&s, inbox(&s, bob), "For Bob").is_none());
    assert!(s.mailbox.folder(inbox(&s, alice)).unwrap().remote_name.is_some());
    // Bob's own group still runs.
    run(&mut s, "sendReceive.group", json!({"name": BOB}));
    assert!(s.wait_for_sync(WAIT));
    assert!(find(&s, inbox(&s, bob), "For Bob").is_some());
    assert_eq!(mailcraft_engine::sync::group_accounts(&s), vec![alice]);
}

#[test]
fn account_options_survive_reopening_and_hold_no_secrets() {
    let srv = servers();
    let dir = tempfile::tempdir().unwrap();
    let a;
    {
        let mut s = Session::open(dir.path()).unwrap();
        s.view.auto_send_receive_minutes = 0;
        a = add_pop3(&mut s, &srv, json!({"removeAfterDays": 3, "intervalMinutes": 7}));
        srv.seed_message(ALICE, "INBOX", &message("Kept", "kept", "x"), &[]).unwrap();
        sync(&mut s);
        run(&mut s, "file.exit", json!({}));
    }
    let text = std::fs::read_to_string(dir.path().join("account-options.json")).unwrap();
    assert!(!text.contains(ALICE_PW));
    let mut s = Session::open(dir.path()).unwrap();
    s.view.auto_send_receive_minutes = 0;
    let o = run(&mut s, "accounts.options", json!({"account": a.0}));
    assert_eq!(o["pop3"], true);
    assert_eq!(o["removeAfterDays"], 3);
    assert_eq!(o["intervalMinutes"], 7);
    // The unique ids seen before are remembered: no second copy.
    sync(&mut s);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Kept").count(), 1);
}

#[test]
fn without_idle_the_inbox_is_polled_and_lost_push_connections_retry() {
    let srv = TestServers::start(Config {
        users: vec![(ALICE.into(), ALICE_PW.into())],
        imap_idle: false,
        ..Config::default()
    })
    .unwrap();
    let mut s = session();
    let start = mailcraft_engine::model::DateTime::now_utc();
    s.fixed_now = Some(start);
    let a = add_imap(&mut s, &srv, json!({"pollMinutes": 2}));
    sync(&mut s);
    poll_until(&mut s, "polling mode", |s| s.push_state(a).0 == PushMode::Polling);
    srv.seed_message(ALICE, "INBOX", &message("Polled", "polled", "found by polling"), &[]).unwrap();
    let ib = inbox(&s, a);
    for _ in 0..10 {
        s.poll_sync();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(find(&s, ib, "Polled").is_none(), "not before the poll interval");
    s.fixed_now = Some(mailcraft_engine::model::DateTime::from_unix(start.unix + 121, 0));
    poll_until(&mut s, "the polled message", |s| find(s, ib, "Polled").is_some());

    // A push connection that drops is retried (and the Inbox polled meanwhile).
    let srv2 = servers();
    let mut s2 = session();
    let b = add_imap(&mut s2, &srv2, json!({}));
    sync(&mut s2);
    poll_until(&mut s2, "push", |s| s.push_state(b).0 == PushMode::Idle);
    srv2.shutdown();
    poll_until(&mut s2, "reconnecting", |s| s.push_state(b).0 == PushMode::Reconnecting);
    assert!(s2.push_state(b).1.is_some(), "the push error is kept for the status");
}
