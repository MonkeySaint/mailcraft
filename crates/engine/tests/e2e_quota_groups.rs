//! End to end against the local test servers (`mailcraft-testmail`, 127.0.0.1 only, synthetic users):
//! the mailbox quota (IMAP GETQUOTAROOT, JMAP Quota/get) reaching the status, and user-defined Send/Receive
//! groups (define, run by name, F9, schedule).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, FolderRole, Mailbox};
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

fn message(subject: &str) -> Vec<u8> {
    format!(
        "From: {BOB}\r\nTo: {ALICE}\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
         Message-ID: <{}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Hello from the test server, with some text to take up space.\r\n",
        subject.replace(' ', "-")
    )
    .into_bytes()
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn session() -> Session {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    s
}

fn add_imap(s: &mut Session, srv: &TestServers, user: &str, pw: &str) -> AccountId {
    let r = run(
        s,
        "accounts.add",
        json!({
            "kind": "imap",
            "address": user,
            "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
            "outgoing": {"host": "127.0.0.1", "port": srv.smtp_port, "security": "starttls", "extraRootPem": srv.cert_pem},
            "password": pw,
        }),
    );
    let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    // Only the runs a test starts: no push connection fetching mail on its own.
    run(s, "accounts.options", json!({"account": id.0, "push": false}));
    id
}

fn has(s: &Session, account: AccountId, subject: &str) -> bool {
    s.mailbox
        .mail
        .values()
        .any(|m| m.subject == subject && s.mailbox.folder(m.folder).is_some_and(|f| f.account == account))
}

#[test]
fn imap_quota_reaches_the_status() {
    let srv = servers();
    srv.set_quota(ALICE, Some(1024 * 1024)).unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Quota one"), &[]).unwrap();
    let mut s = session();
    let a = add_imap(&mut s, &srv, ALICE, ALICE_PW);
    assert!(mailcraft_engine::sync::account_quota(&s, a).is_none(), "nothing known before a run");
    run(&mut s, "sendReceive.all", json!({}));
    assert!(s.wait_for_sync(WAIT), "{:?}", s.sync_status());
    let q = mailcraft_engine::sync::account_quota(&s, a).expect("quota after receiving");
    assert_eq!(q.limit, 1024 * 1024);
    assert!(q.used > 0 && q.used < q.limit);
    assert_eq!(q.percent(), Some(0));
    assert!(q.text().contains("of 1.0 MB used"), "{}", q.text());
    let st = run(&mut s, "sendReceive.status", json!({}));
    assert_eq!(st["accounts"][0]["quota"]["limit"], 1024 * 1024);
    let (name, _) = mailcraft_engine::sync::status_quota(&s).expect("status bar quota");
    assert!(!name.is_empty());
    // The limit goes away: the next run clears it.
    srv.set_quota(ALICE, None).unwrap();
    run(&mut s, "sendReceive.all", json!({}));
    assert!(s.wait_for_sync(WAIT));
    assert!(mailcraft_engine::sync::account_quota(&s, a).is_none());
}

#[test]
fn jmap_quota_reaches_the_status() {
    let srv = servers();
    srv.set_quota(ALICE, Some(10_000)).unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Over JMAP"), &[]).unwrap();
    let mut s = session();
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
    run(&mut s, "sendReceive.all", json!({}));
    assert!(s.wait_for_sync(WAIT), "{:?}", s.sync_status());
    let q = mailcraft_engine::sync::account_quota(&s, a).expect("JMAP quota");
    assert_eq!(q.limit, 10_000);
    assert!(q.percent().unwrap() > 0);
    assert!(q.messages.is_some());
}

#[test]
fn send_receive_groups_define_and_run() {
    let srv = servers();
    srv.seed_message(ALICE, "INBOX", &message("For Alice"), &[]).unwrap();
    srv.seed_message(BOB, "INBOX", &message("For Bob"), &[]).unwrap();
    let mut s = session();
    let alice = add_imap(&mut s, &srv, ALICE, ALICE_PW);
    let bob = add_imap(&mut s, &srv, BOB, BOB_PW);

    // The dialog lists the built-in group and the accounts with their folders.
    let d = run(&mut s, "sendReceive.defineGroups", json!({}));
    assert_eq!(d["dialog"], "sendReceiveGroups");
    assert_eq!(d["groups"][0]["name"], "All Accounts");
    assert_eq!(d["groups"][0]["builtIn"], true);
    assert_eq!(d["accounts"].as_array().unwrap().len(), 2);

    // A group with only Alice, receiving only.
    let g = json!({"name": "Alice only", "members": [{"account": alice.0, "send": false}], "include": false});
    run(&mut s, "sendReceive.defineGroups", json!({"action": "save", "group": g}));
    assert!(
        s.run("sendReceive.defineGroups", &json!({"action": "save", "group": {"name": "All Accounts"}}))
            .is_err()
    );
    assert!(s.run("sendReceive.defineGroups", &json!({"action": "save", "group": {"name": ""}})).is_err());
    let bad = json!({"name": "Bad", "members": [{"account": 999}]});
    assert!(s.run("sendReceive.defineGroups", &json!({"action": "save", "group": bad})).is_err());

    let r = run(&mut s, "sendReceive.group", json!({"name": "alice ONLY"}));
    assert_eq!(r["group"], "Alice only");
    assert!(s.wait_for_sync(WAIT), "{:?}", s.sync_status());
    assert!(has(&s, alice, "For Alice"));
    assert!(!has(&s, bob, "For Bob"), "Bob is not in the group");

    // Copy, rename, list, remove.
    run(
        &mut s,
        "sendReceive.defineGroups",
        json!({"action": "copy", "name": "Alice only", "newName": "Copy"}),
    );
    run(&mut s, "sendReceive.defineGroups", json!({"action": "rename", "name": "Copy", "newName": "Bob"}));
    let bob_group =
        json!({"oldName": "Bob", "group": {"name": "Bob", "members": [{"account": bob.0}], "include": true}});
    let mut p = bob_group.clone();
    p["action"] = json!("save");
    run(&mut s, "sendReceive.defineGroups", p);
    assert!(
        s.run(
            "sendReceive.defineGroups",
            &json!({"action": "rename", "name": "Bob", "newName": "Alice only"})
        )
        .is_err(),
        "names stay unique"
    );
    let l = run(&mut s, "sendReceive.defineGroups", json!({"action": "list"}));
    let names: Vec<&str> =
        l["groups"].as_array().unwrap().iter().filter_map(|g| g["name"].as_str()).collect();
    assert_eq!(names, ["All Accounts", "Alice only", "Bob"]);

    // F9 also runs groups included in it (Bob's is): take Bob out of All Accounts first.
    run(&mut s, "accounts.options", json!({"account": bob.0, "include": false}));
    let r = run(&mut s, "sendReceive.all", json!({}));
    assert_eq!(r["groups"], json!(["Bob"]));
    assert!(s.wait_for_sync(WAIT), "{:?}", s.sync_status());
    assert!(has(&s, bob, "For Bob"), "the Bob group ran with F9");

    // The menu entry for a folder: the current account's Inbox.
    s.selection.folder = s.mailbox.role_folder(alice, FolderRole::Inbox).unwrap();
    let r = run(&mut s, "sendReceive.group", json!({"name": "Inbox"}));
    assert_eq!(r["folder"], s.selection.folder.0);
    assert!(s.wait_for_sync(WAIT));

    run(&mut s, "sendReceive.defineGroups", json!({"action": "remove", "name": "Bob"}));
    assert!(
        s.run("sendReceive.group", &json!({"name": "Bob"})).is_err(),
        "no group, account or folder called Bob"
    );
    run(&mut s, "sendReceive.group", json!({"name": BOB}));
    assert!(s.wait_for_sync(WAIT));
    assert!(s.run("sendReceive.defineGroups", &json!({"action": "remove", "name": "Bob"})).is_err());
}

#[test]
fn scheduled_group_runs_when_due() {
    let srv = servers();
    let mut s = session();
    let alice = add_imap(&mut s, &srv, ALICE, ALICE_PW);
    s.fixed_now = Some(mailcraft_engine::model::DateTime::from_unix(1_800_000_000, 0));
    let g =
        json!({"name": "Every 5", "members": [{"account": alice.0}], "include": false, "everyMinutes": 5});
    run(&mut s, "sendReceive.defineGroups", json!({"action": "save", "group": g}));
    s.poll_sync(); // starts the clock
    srv.seed_message(ALICE, "INBOX", &message("Scheduled"), &[]).unwrap();
    s.fixed_now = Some(mailcraft_engine::model::DateTime::from_unix(1_800_000_000 + 4 * 60, 0));
    s.poll_sync();
    assert!(!s.sync_busy(), "not due yet");
    s.fixed_now = Some(mailcraft_engine::model::DateTime::from_unix(1_800_000_000 + 5 * 60, 0));
    s.poll_sync();
    assert!(s.wait_for_sync(WAIT), "{:?}", s.sync_status());
    assert!(has(&s, alice, "Scheduled"));
    // Bad intervals are refused.
    let g = json!({"name": "Too often", "members": [], "everyMinutes": 100_000});
    assert!(s.run("sendReceive.defineGroups", &json!({"action": "save", "group": g})).is_err());
}
