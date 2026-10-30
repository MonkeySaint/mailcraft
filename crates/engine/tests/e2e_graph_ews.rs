//! End to end, a Microsoft 365 (Graph) account and an Exchange (EWS) account in one profile, both on the
//! local test servers (HTTPS, 127.0.0.1, synthetic users; nothing reaches a Microsoft service): each receives
//! its own mail, each writes its own changes back, and mail copied from one to the other is uploaded to the
//! other's server once and stays where it was on the first.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, FolderId, FolderRole, ItemId, Mailbox};
use mailcraft_testmail::{Config, TestServers};
use serde_json::{Value, json};

const ALICE: &str = "alice@example.com";
const ALICE_PW: &str = "alice-graph-pw";
const PAT: &str = "pat@example.net";
const PAT_PW: &str = "pat-test-pw";
const BOB: &str = "bob@example.org";
const WAIT: Duration = Duration::from_secs(30);

fn message(to: &str, subject: &str, id: &str) -> Vec<u8> {
    format!(
        "From: Bob <{BOB}>\r\nTo: {to}\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
         Message-ID: <{id}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Hello: {subject}.\r\n"
    )
    .into_bytes()
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn settle(s: &mut Session) {
    assert!(s.wait_for_sync(WAIT), "timed out: {:?}", s.sync_status());
    let errors: Vec<String> = s.sync_status().accounts.iter().filter_map(|x| x.error.clone()).collect();
    assert!(errors.is_empty(), "{errors:?}");
}

fn sync(s: &mut Session) {
    run(s, "sendReceive.all", json!({}));
    settle(s);
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

fn flags(srv: &TestServers, user: &str, mailbox: &str, subject: &str) -> Vec<String> {
    srv.messages(user, mailbox)
        .unwrap()
        .into_iter()
        .find(|m| mailcraft_mime::parse(&m.raw).map(|x| x.subject()).ok().as_deref() == Some(subject))
        .map(|m| m.flags)
        .unwrap_or_default()
}

fn account(r: &Value) -> AccountId {
    AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
}

#[test]
fn graph_and_exchange_accounts_work_side_by_side() {
    let srv = TestServers::start(Config {
        users: vec![
            (ALICE.into(), ALICE_PW.into()),
            (PAT.into(), PAT_PW.into()),
            (BOB.into(), "bob-pw".into()),
        ],
        add_trace_headers: false,
        ..Config::default()
    })
    .unwrap();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let r = run(
        &mut s,
        "accounts.add",
        json!({"kind": "graph", "address": ALICE, "password": ALICE_PW,
               "incoming": {"host": srv.graph_url(), "extraRootPem": srv.cert_pem}}),
    );
    assert_eq!(r["kind"], "graph");
    let graph = account(&r);
    let r = run(
        &mut s,
        "accounts.add",
        json!({"kind": "exchange", "address": PAT, "password": PAT_PW,
               "incoming": {"host": srv.ews_url(), "extraRootPem": srv.cert_pem}}),
    );
    assert_eq!(r["kind"], "exchange");
    let exchange = account(&r);

    srv.seed_message(ALICE, "INBOX", &message(ALICE, "For Alice", "alice-1"), &[]).unwrap();
    srv.seed_message(PAT, "INBOX", &message(PAT, "For Pat", "pat-1"), &[]).unwrap();
    sync(&mut s);
    let alice_inbox = s.mailbox.role_folder(graph, FolderRole::Inbox).unwrap();
    let pat_inbox = s.mailbox.role_folder(exchange, FolderRole::Inbox).unwrap();
    let for_alice = find(&s, alice_inbox, "For Alice").expect("the Graph account's mail");
    let for_pat = find(&s, pat_inbox, "For Pat").expect("the Exchange account's mail");
    assert!(find(&s, alice_inbox, "For Pat").is_none() && find(&s, pat_inbox, "For Alice").is_none());

    // Each account writes its own changes back.
    select(&mut s, for_alice);
    run(&mut s, "mail.markRead", json!({"ids": [for_alice.0]}));
    select(&mut s, for_pat);
    run(&mut s, "mail.flag", json!({"ids": [for_pat.0]}));
    settle(&mut s);
    sync(&mut s);
    assert!(flags(&srv, ALICE, "INBOX", "For Alice").contains(&"\\Seen".to_owned()));
    assert!(flags(&srv, PAT, "INBOX", "For Pat").contains(&"\\Flagged".to_owned()));
    assert!(!flags(&srv, PAT, "INBOX", "For Pat").contains(&"\\Seen".to_owned()), "only what changed");

    // Copied from the Graph account to the Exchange account: uploaded to Exchange once, kept on Graph.
    select(&mut s, for_alice);
    run(&mut s, "mail.copy", json!({"ids": [for_alice.0], "folder": pat_inbox.0}));
    settle(&mut s);
    sync(&mut s);
    sync(&mut s);
    let mut pat = subjects(&srv, PAT, "INBOX");
    pat.sort();
    assert_eq!(pat, ["For Alice", "For Pat"], "one copy on the Exchange server");
    assert_eq!(subjects(&srv, ALICE, "INBOX"), ["For Alice"], "the Graph service keeps its message");
    assert_eq!(
        s.mailbox.mail.items().filter(|m| m.folder == pat_inbox && m.subject == "For Alice").count(),
        1,
        "no second item from the next update"
    );
    assert!(s.mailbox.mail.get(for_alice).is_some_and(|m| m.folder == alice_inbox));
}
