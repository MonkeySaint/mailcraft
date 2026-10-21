//! End to end: Options › Advanced › "Perform an automatic send/receive when exiting" against the local test
//! servers (`mailcraft-testmail`, 127.0.0.1 only, synthetic users). Every wait has a timeout.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, FolderRole, Mailbox};
use mailcraft_testmail::{Config, TestServers};
use serde_json::{Value, json};

const ALICE: &str = "alice@example.com";
const ALICE_PW: &str = "alice-test-pw";
const BOB: &str = "bob@example.org";

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

/// A session with Alice's IMAP/SMTP account that does not send by itself (no automatic send/receive, no
/// "Send immediately when connected"), and one message waiting in its Outbox.
fn waiting_message(srv: &TestServers, subject: &str) -> (Session, AccountId) {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    s.view.send_immediately = false;
    let r = run(
        &mut s,
        "accounts.add",
        json!({
            "kind": "imap",
            "address": ALICE,
            "senderName": "Alice Example",
            "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
            "outgoing": {"host": "127.0.0.1", "port": srv.smtp_port, "security": "starttls", "extraRootPem": srv.cert_pem},
            "password": ALICE_PW,
        }),
    );
    let acct = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    queue(&mut s, subject);
    let outbox = s.mailbox.role_folder(acct, FolderRole::Outbox).unwrap();
    assert_eq!(s.mailbox.total_count(outbox), 1, "waiting in the Outbox");
    (s, acct)
}

/// Sends a message to Bob, which waits in the Outbox.
fn queue(s: &mut Session, subject: &str) {
    let c = run(s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(s, "compose.setTo", json!({"compose": c, "value": BOB}));
    run(s, "compose.setSubject", json!({"compose": c, "value": subject}));
    assert_eq!(run(s, "compose.send", json!({"compose": c}))["sent"], true);
    assert!(s.wait_for_sync(std::time::Duration::from_secs(30)));
}

#[test]
fn exiting_sends_the_outbox_only_when_the_option_is_on() {
    let srv = TestServers::start(Config {
        users: vec![(ALICE.into(), ALICE_PW.into()), (BOB.into(), "bob-test-pw".into())],
        ..Config::default()
    })
    .unwrap();

    // Off (default): exiting leaves the message in the Outbox.
    let (mut s, acct) = waiting_message(&srv, "Left behind");
    run(&mut s, "file.exit", json!({}));
    assert!(srv.submissions().is_empty());
    assert_eq!(s.mailbox.total_count(s.mailbox.role_folder(acct, FolderRole::Outbox).unwrap()), 1);

    // On: exiting runs a send/receive first and waits for it.
    let (mut s, acct) = waiting_message(&srv, "Sent on exit");
    run(&mut s, "options.set", json!({"key": "advanced.sendReceiveOnExit", "value": true}));
    run(&mut s, "file.exit", json!({}));
    let sub = srv.submissions();
    assert_eq!(sub.len(), 1, "sent while exiting");
    assert_eq!(sub[0].rcpt_to, vec![BOB.to_owned()]);
    assert_eq!(s.mailbox.total_count(s.mailbox.role_folder(acct, FolderRole::Outbox).unwrap()), 0);
    // File › Exit closes the window, whose close runs `file.exit` again for the final save: that one must not
    // send and receive (and wait) a second time.
    queue(&mut s, "After the first exit");
    run(&mut s, "file.exit", json!({}));
    assert_eq!(srv.submissions().len(), 1, "one send/receive per exit");
    assert_eq!(s.mailbox.total_count(s.mailbox.role_folder(acct, FolderRole::Outbox).unwrap()), 1);

    // Offline: nothing is attempted.
    let (mut s, _) = waiting_message(&srv, "Offline");
    run(&mut s, "options.set", json!({"key": "advanced.sendReceiveOnExit", "value": true}));
    s.view.work_offline = true;
    run(&mut s, "file.exit", json!({}));
    assert_eq!(srv.submissions().len(), 1);
}
