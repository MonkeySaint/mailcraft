//! End to end S/MIME: two people with digital IDs (synthetic, created at test time for example.com /
//! example.org addresses) exchange signed and encrypted mail through the local test servers (SMTP submission,
//! IMAP download; `mailcraft-testmail`, 127.0.0.1 only). Every wait has a timeout.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use base64::Engine;
use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, FolderId, FolderRole, ItemId, Mailbox};
use mailcraft_smime::{Identity, testkit};
use mailcraft_testmail::{Config, TestServers};
use serde_json::{Value, json};

const ALICE: &str = "alice@example.com";
const ALICE_PW: &str = "alice-test-pw";
const BOB: &str = "bob@example.org";
const BOB_PW: &str = "bob-test-pw";
const WAIT: Duration = Duration::from_secs(30);

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn sync(s: &mut Session) {
    run(s, "sendReceive.all", json!({}));
    assert!(s.wait_for_sync(WAIT), "send/receive timed out: {:?}", s.sync_status());
}

fn role(s: &Session, account: AccountId, role: FolderRole) -> FolderId {
    s.mailbox.role_folder(account, role).unwrap()
}

fn find(s: &Session, folder: FolderId, subject: &str) -> Option<ItemId> {
    s.mailbox.mail.items().find(|m| m.folder == folder && m.subject == subject).map(|m| m.id)
}

/// A session for `address` on the test servers, holding the digital ID `id` (its CA trusted).
fn person(
    srv: &TestServers,
    address: &str,
    password: &str,
    name: &str,
    id: &Identity,
) -> (Session, AccountId) {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let r = run(
        &mut s,
        "accounts.add",
        json!({
            "kind": "imap",
            "address": address,
            "senderName": name,
            "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
            "outgoing": {"host": "127.0.0.1", "port": srv.smtp_port, "security": "starttls", "extraRootPem": srv.cert_pem},
            "password": password,
        }),
    );
    let acct = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    let pfx = mailcraft_smime::pkcs12::build(id, "pfx", name).unwrap();
    let data = base64::engine::general_purpose::STANDARD.encode(pfx);
    run(&mut s, "options.importDigitalId", json!({"data": data, "password": "pfx", "trustIssuer": true}));
    (s, acct)
}

#[test]
fn signed_and_encrypted_mail_through_smtp_and_imap() {
    let srv = TestServers::start(Config {
        users: vec![(ALICE.into(), ALICE_PW.into()), (BOB.into(), BOB_PW.into())],
        ..Config::default()
    })
    .unwrap();
    let alice_id = testkit::identity("Alice Example", ALICE);
    let bob_id = testkit::identity("Bob Example", BOB);
    let (mut alice, alice_acct) = person(&srv, ALICE, ALICE_PW, "Alice Example", &alice_id);
    let (mut bob, bob_acct) = person(&srv, BOB, BOB_PW, "Bob Example", &bob_id);

    // 1. Bob sends Alice a signed message: Alice sees a valid signature and learns Bob's certificate.
    let c = run(&mut bob, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut bob, "compose.setTo", json!({"compose": c, "value": ALICE}));
    run(&mut bob, "compose.setSubject", json!({"compose": c, "value": "Signed hello"}));
    run(&mut bob, "compose.insertText", json!({"compose": c, "text": "This one is signed."}));
    run(&mut bob, "compose.sign", json!({"compose": c, "on": true}));
    assert_eq!(run(&mut bob, "compose.send", json!({"compose": c}))["sent"], true);
    sync(&mut bob);
    let sub = srv.submissions();
    let wire = String::from_utf8_lossy(&sub.last().unwrap().data).into_owned();
    assert!(wire.contains("multipart/signed") && wire.contains("application/pkcs7-signature"), "{wire}");
    sync(&mut alice);
    let inbox = role(&alice, alice_acct, FolderRole::Inbox);
    let signed = find(&alice, inbox, "Signed hello").expect("signed message received over IMAP");
    let sec = mailcraft_engine::smime::security(&alice, signed).unwrap();
    assert_eq!(sec.status, "valid", "{sec:?}");
    assert!(sec.address_matches && !sec.encrypted);
    assert!(mailcraft_engine::smime::cert_for(&alice, BOB).is_some(), "Bob's certificate remembered");

    // 2. Alice replies signed and encrypted: nothing readable on the wire, Bob reads it, so does Alice's
    //    Sent Items copy.
    alice.selection.folder = inbox;
    alice.selection.items = vec![signed];
    alice.selection.focus = Some(signed);
    let c = run(&mut alice, "mail.reply", json!({"ids": [signed.0]}))["compose"].as_u64().unwrap();
    run(&mut alice, "compose.insertText", json!({"compose": c, "text": "Secret answer: blue."}));
    run(&mut alice, "compose.sign", json!({"compose": c, "on": true}));
    let r = run(&mut alice, "compose.encrypt", json!({"compose": c, "on": true}));
    assert_eq!(r["missing"], json!([]), "{r}");
    assert_eq!(run(&mut alice, "compose.send", json!({"compose": c}))["sent"], true);
    sync(&mut alice);
    let sub = srv.submissions();
    let last = sub.last().unwrap();
    assert_eq!(last.rcpt_to, vec![BOB.to_owned()]);
    let wire = String::from_utf8_lossy(&last.data).into_owned();
    assert!(wire.contains("smime-type=enveloped-data"), "{wire}");
    assert!(!wire.contains("blue"), "the text is encrypted in transit");
    let sent = role(&alice, alice_acct, FolderRole::SentItems);
    let copy = find(&alice, sent, "RE: Signed hello").expect("filed in Sent Items");
    let shown = mailcraft_engine::smime::display_item(&alice, copy).unwrap();
    let text = mailcraft_engine::smime::display_body(&alice, &shown).plain_text();
    assert!(text.contains("blue"), "Alice can read her own sent copy");

    sync(&mut bob);
    let bob_inbox = role(&bob, bob_acct, FolderRole::Inbox);
    let got = find(&bob, bob_inbox, "RE: Signed hello").expect("encrypted reply received over IMAP");
    let sec = mailcraft_engine::smime::security(&bob, got).unwrap();
    assert!(sec.encrypted && !sec.locked, "{sec:?}");
    assert_eq!(sec.status, "valid", "{sec:?}");
    assert!(sec.signer.contains(ALICE));
    let shown = mailcraft_engine::smime::display_item(&bob, got).unwrap();
    assert!(
        mailcraft_engine::smime::display_body(&bob, &shown).plain_text().contains("Secret answer: blue.")
    );
    // The stored item keeps the encrypted form: the text is not in the mailbox or its body store.
    assert!(!bob.body_text(bob.mailbox.mail.get(got).unwrap()).contains("blue"));
}
