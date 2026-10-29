//! End to end: an IMAP/SMTP account set up through commands against the local test servers
//! (`mailcraft-testmail`, 127.0.0.1 only, synthetic users), then receive, rules, reply, send, and local
//! changes reaching the server. Every wait has a timeout.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_engine::Session;
use mailcraft_engine::model::account::RemoteOp;
use mailcraft_engine::model::{AccountId, FolderId, FolderRole, ItemId, Mailbox, ResponseState};
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
        ..Config::default()
    })
    .unwrap()
}

fn message(from: &str, to: &str, subject: &str, id: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
         Message-ID: <{id}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Hello from the test server.\r\n"
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
    assert!(s.wait_for_sync(WAIT), "queued changes timed out: {:?}", s.sync_status());
}

/// Selects one message (mail commands act on the selection; `ids` must also be selected to be enabled).
fn select(s: &mut Session, id: ItemId) {
    let folder = s.mailbox.mail.get(id).unwrap().folder;
    s.selection.folder = folder;
    s.selection.items = vec![id];
    s.selection.focus = Some(id);
}

fn role(s: &Session, account: AccountId, role: FolderRole) -> FolderId {
    s.mailbox.role_folder(account, role).unwrap()
}

fn find(s: &Session, folder: FolderId, subject: &str) -> Option<ItemId> {
    s.mailbox.mail.items().find(|m| m.folder == folder && m.subject == subject).map(|m| m.id)
}

fn server_subjects(srv: &TestServers, user: &str, mailbox: &str) -> Vec<String> {
    srv.messages(user, mailbox)
        .unwrap()
        .iter()
        .filter_map(|m| {
            let msg = mailcraft_mime::parse(&m.raw).ok()?;
            Some(msg.subject())
        })
        .collect()
}

fn server_flags(srv: &TestServers, user: &str, mailbox: &str, subject: &str) -> Vec<String> {
    srv.messages(user, mailbox)
        .unwrap()
        .into_iter()
        .find(|m| mailcraft_mime::parse(&m.raw).map(|x| x.subject()).ok().as_deref() == Some(subject))
        .map(|m| m.flags)
        .unwrap_or_else(|| panic!("{subject} not in {mailbox}"))
}

fn add_alice(s: &mut Session, srv: &TestServers, password: &str) -> AccountId {
    let r = run(
        s,
        "accounts.add",
        json!({
            "kind": "imap",
            "address": ALICE,
            "senderName": "Alice Example",
            "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
            "outgoing": {"host": "127.0.0.1", "port": srv.smtp_port, "security": "starttls", "extraRootPem": srv.cert_pem},
            "password": password,
        }),
    );
    AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
}

fn session() -> Session {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    // Only explicit send/receive runs in these tests.
    s.view.auto_send_receive_minutes = 0;
    s
}

#[test]
fn imap_account_receive_rules_reply_send_and_server_changes() {
    let srv = servers();
    let mut s = session();
    let acct = add_alice(&mut s, &srv, ALICE_PW);
    assert!(s.mailbox.account(acct).unwrap().is_default);

    // Test Account Settings logs in to both servers.
    let t = run(&mut s, "accounts.test", json!({"account": acct.0, "wait": true}));
    assert_eq!(t["ok"], true, "{t}");

    // Receive.
    srv.seed_message(ALICE, "INBOX", &message(BOB, ALICE, "Lunch?", "lunch"), &[]).unwrap();
    sync(&mut s);
    let inbox = role(&s, acct, FolderRole::Inbox);
    let lunch = find(&s, inbox, "Lunch?").expect("seeded message received");
    assert!(!s.mailbox.mail.get(lunch).unwrap().read);
    assert_eq!(s.sync_status().text, "All folders are up to date.");
    assert_eq!(s.sync_status().connection_text, "Connected");
    for (r, name) in [
        (FolderRole::SentItems, "Sent"),
        (FolderRole::DeletedItems, "Trash"),
        (FolderRole::Drafts, "Drafts"),
        (FolderRole::JunkEmail, "Junk"),
        (FolderRole::Archive, "Archive"),
    ] {
        assert_eq!(s.mailbox.folder(role(&s, acct, r)).unwrap().remote_name.as_deref(), Some(name), "{r:?}");
    }
    assert!(s.mailbox.folder(role(&s, acct, FolderRole::Outbox)).unwrap().remote_name.is_none());
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());

    // A new folder reaches the server; a rule files new mail into it, on the server too.
    s.selection.folder = inbox;
    let projects =
        FolderId(run(&mut s, "folder.new", json!({"name": "Projects"}))["folder"].as_u64().unwrap());
    settle(&mut s);
    assert!(srv.mailboxes(ALICE).unwrap().contains(&"INBOX/Projects".to_owned()));
    run(
        &mut s,
        "rules.create",
        json!({"name": "Projects", "conditions": [{"SubjectContains": ["Project X"]}], "actions": [{"MoveTo": projects.0}]}),
    );
    srv.seed_message(ALICE, "INBOX", &message(BOB, ALICE, "Project X status", "px"), &[]).unwrap();
    sync(&mut s);
    settle(&mut s);
    let px = find(&s, projects, "Project X status").expect("rule moved the new message");
    assert!(server_subjects(&srv, ALICE, "INBOX/Projects").contains(&"Project X status".to_owned()));
    assert!(!server_subjects(&srv, ALICE, "INBOX").contains(&"Project X status".to_owned()));
    assert!(s.mailbox.mail.get(px).unwrap().remote_id.is_some(), "the moved message knows its new UID");

    // Reply and send.
    s.selection.items = vec![lunch];
    s.selection.focus = Some(lunch);
    let c = run(&mut s, "mail.reply", json!({"ids": [lunch.0]}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.insertText", json!({"compose": c, "text": "Sounds good."}));
    let r = run(&mut s, "compose.send", json!({"compose": c}));
    assert_eq!(r["sent"], true, "{r}");
    sync(&mut s);
    settle(&mut s);
    let outbox = role(&s, acct, FolderRole::Outbox);
    assert_eq!(s.mailbox.total_count(outbox), 0, "Outbox emptied");
    let sent = role(&s, acct, FolderRole::SentItems);
    let reply = find(&s, sent, "RE: Lunch?").expect("reply filed in Sent Items");
    assert!(s.mailbox.mail.get(reply).unwrap().remote_id.is_some());
    assert!(server_subjects(&srv, BOB, "INBOX").contains(&"RE: Lunch?".to_owned()), "Bob received the reply");
    assert!(server_subjects(&srv, ALICE, "Sent").contains(&"RE: Lunch?".to_owned()), "uploaded to Sent");
    let sub = srv.submissions();
    let last = sub.last().unwrap();
    assert_eq!(last.mail_from, ALICE);
    assert_eq!(last.rcpt_to, vec![BOB.to_owned()]);
    assert!(matches!(s.mailbox.mail.get(lunch).unwrap().response, ResponseState::Replied(_)));
    assert!(server_flags(&srv, ALICE, "INBOX", "Lunch?").contains(&"\\Answered".to_owned()));

    // Flag, unread, move, delete, permanent delete.
    select(&mut s, lunch);
    run(&mut s, "mail.markRead", json!({"ids": [lunch.0]}));
    select(&mut s, lunch);
    run(&mut s, "mail.flag", json!({"ids": [lunch.0]}));
    settle(&mut s);
    let flags = server_flags(&srv, ALICE, "INBOX", "Lunch?");
    assert!(flags.contains(&"\\Flagged".to_owned()) && flags.contains(&"\\Seen".to_owned()), "{flags:?}");
    select(&mut s, lunch);
    run(&mut s, "mail.markUnread", json!({"ids": [lunch.0]}));
    settle(&mut s);
    assert!(!server_flags(&srv, ALICE, "INBOX", "Lunch?").contains(&"\\Seen".to_owned()));
    let archive = role(&s, acct, FolderRole::Archive);
    select(&mut s, lunch);
    run(&mut s, "mail.move", json!({"ids": [lunch.0], "folder": archive.0}));
    settle(&mut s);
    assert!(server_subjects(&srv, ALICE, "Archive").contains(&"Lunch?".to_owned()));
    assert!(!server_subjects(&srv, ALICE, "INBOX").contains(&"Lunch?".to_owned()));
    select(&mut s, px);
    run(&mut s, "mail.delete", json!({"ids": [px.0]}));
    settle(&mut s);
    assert!(server_subjects(&srv, ALICE, "Trash").contains(&"Project X status".to_owned()));
    assert!(server_subjects(&srv, ALICE, "INBOX/Projects").is_empty());
    let trash = role(&s, acct, FolderRole::DeletedItems);
    let px = find(&s, trash, "Project X status").unwrap();
    select(&mut s, px);
    run(&mut s, "mail.permanentDelete", json!({"ids": [px.0]}));
    settle(&mut s);
    assert!(!server_subjects(&srv, ALICE, "Trash").contains(&"Project X status".to_owned()));

    // Undo of the move goes back to the server too.
    select(&mut s, lunch);
    run(&mut s, "mail.move", json!({"ids": [lunch.0], "folder": inbox.0}));
    settle(&mut s);
    assert!(server_subjects(&srv, ALICE, "INBOX").contains(&"Lunch?".to_owned()));
    run(&mut s, "app.undo", json!({}));
    settle(&mut s);
    assert!(server_subjects(&srv, ALICE, "Archive").contains(&"Lunch?".to_owned()));

    // Folder rename and delete reach the server.
    s.selection.folder = projects;
    run(&mut s, "folder.rename", json!({"name": "Work"}));
    settle(&mut s);
    let boxes = srv.mailboxes(ALICE).unwrap();
    assert!(
        boxes.contains(&"INBOX/Work".to_owned()) && !boxes.contains(&"INBOX/Projects".to_owned()),
        "{boxes:?}"
    );

    // Changes made elsewhere come back: a message vanishes from the server, another arrives.
    srv.seed_message(ALICE, "INBOX", &message(BOB, ALICE, "Second", "second"), &["\\Seen"]).unwrap();
    sync(&mut s);
    let second = find(&s, inbox, "Second").unwrap();
    assert!(s.mailbox.mail.get(second).unwrap().read, "server flags on new mail");
    assert!(s.mailbox.account(acct).unwrap().sync.pending.is_empty(), "queue drained");
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

#[test]
fn send_failure_keeps_message_in_outbox_and_retries() {
    let srv = servers();
    let mut s = session();
    let acct = add_alice(&mut s, &srv, "wrong password");
    let t = run(&mut s, "accounts.test", json!({"account": acct.0, "wait": true}));
    assert_eq!(t["ok"], false);
    assert_eq!(t["incomingOk"], false);

    // Sending fails (SMTP AUTH refused): the message stays in the Outbox with an error notice.
    let c = run(&mut s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.setTo", json!({"compose": c, "value": BOB}));
    run(&mut s, "compose.setSubject", json!({"compose": c, "value": "Retry me"}));
    assert_eq!(run(&mut s, "compose.send", json!({"compose": c}))["sent"], true);
    s.notices.clear();
    assert!(s.wait_for_sync(WAIT));
    let outbox = role(&s, acct, FolderRole::Outbox);
    assert!(find(&s, outbox, "Retry me").is_some());
    assert!(
        s.notices
            .iter()
            .any(|n| matches!(n, mailcraft_engine::Notice::Error(t) if t.contains("could not be sent"))),
        "{:?}",
        s.notices
    );
    assert!(srv.submissions().is_empty());

    // Fix the password; the next send/receive sends it.
    run(&mut s, "accounts.edit", json!({"account": acct.0, "password": ALICE_PW}));
    sync(&mut s);
    settle(&mut s);
    assert!(find(&s, outbox, "Retry me").is_none());
    assert!(find(&s, role(&s, acct, FolderRole::SentItems), "Retry me").is_some());
    assert_eq!(srv.submissions().len(), 1);
    assert!(server_subjects(&srv, BOB, "INBOX").contains(&"Retry me".to_owned()));
}

#[test]
fn offline_and_queue_persist_in_the_data_file() {
    let srv = servers();
    let dir = tempfile::tempdir().unwrap();
    let acct;
    let lunch;
    {
        let mut s = Session::open(dir.path()).unwrap();
        s.view.auto_send_receive_minutes = 0;
        acct = add_alice(&mut s, &srv, ALICE_PW);
        srv.seed_message(ALICE, "INBOX", &message(BOB, ALICE, "Lunch?", "lunch"), &[]).unwrap();
        sync(&mut s);
        let inbox = role(&s, acct, FolderRole::Inbox);
        lunch = find(&s, inbox, "Lunch?").unwrap();
        // Offline: the change is queued, not sent.
        run(&mut s, "sendReceive.workOffline", json!({}));
        assert_eq!(s.sync_status().connection_text, "Working offline");
        select(&mut s, lunch);
        run(&mut s, "mail.flag", json!({"ids": [lunch.0]}));
        assert!(s.wait_for_sync(WAIT));
        let pending = &s.mailbox.account(acct).unwrap().sync.pending;
        assert!(matches!(pending.first().map(|p| &p.op), Some(RemoteOp::Flags { .. })));
        assert!(!server_flags(&srv, ALICE, "INBOX", "Lunch?").contains(&"\\Flagged".to_owned()));
        run(&mut s, "file.exit", json!({}));
        // The password is in the secrets file, not in the data file.
        let secrets = std::fs::read_to_string(dir.path().join("secrets.json")).unwrap();
        assert!(secrets.contains(ALICE_PW));
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let p = entry.unwrap().path();
            if p.is_file() && p.file_name().is_some_and(|n| n != "secrets.json") {
                let bytes = std::fs::read(&p).unwrap();
                assert!(
                    !String::from_utf8_lossy(&bytes).contains(ALICE_PW),
                    "{} holds the password",
                    p.display()
                );
            }
        }
    }
    // Reopened: the queue, the folder mapping and the password survive; online again, the change goes out.
    let mut s = Session::open(dir.path()).unwrap();
    s.view.auto_send_receive_minutes = 0;
    assert_eq!(s.mailbox.account(acct).unwrap().sync.pending.len(), 1);
    sync(&mut s);
    settle(&mut s);
    assert!(server_flags(&srv, ALICE, "INBOX", "Lunch?").contains(&"\\Flagged".to_owned()));
    assert_eq!(
        s.mailbox.mail.items().filter(|m| m.subject == "Lunch?").count(),
        1,
        "no duplicate after reopening"
    );
    assert!(s.mailbox.mail.contains(lunch));
}

fn add_account(s: &mut Session, srv: &TestServers, address: &str, password: &str) -> AccountId {
    let r = run(
        s,
        "accounts.add",
        json!({
            "kind": "imap",
            "address": address,
            "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
            "outgoing": {"host": "127.0.0.1", "port": srv.smtp_port, "security": "starttls", "extraRootPem": srv.cert_pem},
            "password": password,
        }),
    );
    AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
}

fn count(s: &Session, folder: FolderId, subject: &str) -> usize {
    s.mailbox.mail.items().filter(|m| m.folder == folder && m.subject == subject).count()
}

/// Holds every queued server change of `acct` back (as if waiting out a retry delay), or releases them.
fn hold_queue(s: &mut Session, acct: AccountId, hold: bool) {
    let a = s.mailbox.accounts.iter_mut().find(|a| a.id == acct).unwrap();
    for p in &mut a.sync.pending {
        p.not_before = if hold { i64::MAX / 2 } else { 0 };
    }
}

#[test]
fn queued_delete_and_move_are_not_downloaded_again() {
    let srv = servers();
    let mut s = session();
    let acct = add_alice(&mut s, &srv, ALICE_PW);
    for (subject, id) in [("Oldest", "m1"), ("Middle", "m2"), ("Newest", "m3")] {
        srv.seed_message(ALICE, "INBOX", &message(BOB, ALICE, subject, id), &[]).unwrap();
    }
    sync(&mut s);
    let inbox = role(&s, acct, FolderRole::Inbox);
    let archive = role(&s, acct, FolderRole::Archive);
    let newest = find(&s, inbox, "Newest").unwrap();
    let middle = find(&s, inbox, "Middle").unwrap();
    // Permanently delete the newest message and move the next one; the server changes wait (retry delay).
    select(&mut s, newest);
    run(&mut s, "mail.permanentDelete", json!({"ids": [newest.0]}));
    select(&mut s, middle);
    run(&mut s, "mail.move", json!({"ids": [middle.0], "folder": archive.0}));
    assert_eq!(s.mailbox.account(acct).unwrap().sync.pending.len(), 2);
    hold_queue(&mut s, acct, true);
    // An automatic update meanwhile must not bring them back into the Inbox.
    let plan = mailcraft_engine::Plan { send: false, receive: true, folders: None, user: false };
    assert_eq!(s.send_receive(&[acct], plan).unwrap(), 1);
    assert!(s.wait_for_sync(WAIT));
    assert_eq!(count(&s, inbox, "Newest"), 0, "a permanently deleted message came back");
    assert_eq!(count(&s, inbox, "Middle"), 0, "a moved message came back");
    assert_eq!(count(&s, archive, "Middle"), 1);
    assert_eq!(s.mailbox.account(acct).unwrap().sync.pending.len(), 2, "changes still queued");
    // Released, the changes reach the server and nothing is duplicated.
    hold_queue(&mut s, acct, false);
    sync(&mut s);
    settle(&mut s);
    assert_eq!(server_subjects(&srv, ALICE, "INBOX"), vec!["Oldest".to_owned()]);
    assert_eq!(server_subjects(&srv, ALICE, "Archive"), vec!["Middle".to_owned()]);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Middle").count(), 1);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Newest").count(), 0);
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

#[test]
fn results_of_a_removed_account_never_reach_a_new_one() {
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, ALICE_PW);
    srv.seed_message(ALICE, "INBOX", &message(BOB, ALICE, "For Alice only", "alice-only"), &[]).unwrap();
    run(&mut s, "sendReceive.all", json!({}));
    // Let Alice's worker finish without applying its results, then remove her account.
    std::thread::sleep(Duration::from_millis(1500));
    run(&mut s, "accounts.remove", json!({"account": alice.0, "confirm": true}));
    // A new account gets the same id (the highest id is reused).
    let bob = add_account(&mut s, &srv, BOB, BOB_PW);
    assert_eq!(bob, alice, "the test needs the id to be reused");
    assert!(s.wait_for_sync(WAIT));
    assert!(
        !s.mailbox.mail.items().any(|m| m.subject == "For Alice only"),
        "Alice's mail was filed in Bob's account"
    );
    assert!(s.mailbox.folders.values().filter(|f| f.account == bob).all(|f| f.remote_name.is_none()));
    // Bob's own send/receive works.
    srv.seed_message(BOB, "INBOX", &message(ALICE, BOB, "For Bob", "bob-mail"), &[]).unwrap();
    sync(&mut s);
    assert!(find(&s, role(&s, bob, FolderRole::Inbox), "For Bob").is_some());
    assert!(!s.mailbox.mail.items().any(|m| m.subject == "For Alice only"));
}

#[test]
fn first_sync_status_counts_the_messages() {
    let srv = servers();
    for i in 0..180 {
        let raw = message(BOB, ALICE, &format!("Bulk {i}"), &format!("bulk-{i}"));
        srv.seed_message(ALICE, "INBOX", &raw, &[]).unwrap();
    }
    let mut s = session();
    let acct = add_alice(&mut s, &srv, ALICE_PW);
    run(&mut s, "sendReceive.all", json!({}));
    // Watch the status as the UI does: every report seen counts towards the 180 messages of the Inbox.
    let end = std::time::Instant::now() + WAIT;
    let mut seen = Vec::new();
    while s.sync_busy() {
        s.poll_sync();
        let st = s.sync_status();
        if let Some(d) = st.detail.clone() {
            assert!(st.busy && st.text == d.text(), "{st:?}");
            seen.push(d);
        }
        assert!(std::time::Instant::now() < end, "timed out: {:?}", s.sync_status());
        std::thread::sleep(Duration::from_millis(1));
    }
    settle(&mut s);
    for d in seen.iter().filter(|d| d.folder == "Inbox" && d.total > 0) {
        assert_eq!(d.total, 180, "{d:?}");
        assert!(d.done <= 180 && d.folder_index == 1 && d.folder_count >= 1, "{d:?}");
    }
    assert!(seen.windows(2).all(|w| w[0].folder_index <= w[1].folder_index), "folders in order: {seen:?}");
    let inbox = role(&s, acct, FolderRole::Inbox);
    assert_eq!(
        s.mailbox.mail.items().filter(|m| m.folder == inbox && m.subject.starts_with("Bulk ")).count(),
        180
    );
    let st = s.sync_status();
    assert!(st.detail.is_none() && st.accounts.iter().all(|a| a.detail.is_none()), "{st:?}");
    assert_eq!(st.text, "All folders are up to date.");
    // The control channel and MCP get the same fields (empty once the run is over).
    let v = run(&mut s, "sendReceive.status", json!({}));
    assert!(v["detail"].is_null() && v["accounts"][0]["detail"].is_null(), "{v}");
    assert!(v.as_object().unwrap().contains_key("detail"), "{v}");
}

/// Regression (live run): on a server without an archive mailbox, Archive moved the message into a folder
/// kept on this computer only and expunged the server copy, so the mail left the server. The archive
/// mailbox is now made there first (with the `\Archive` special use, RFC 6154 CREATE-SPECIAL-USE).
#[test]
fn archiving_on_a_server_without_an_archive_mailbox_makes_one() {
    let srv = servers();
    srv.delete_mailbox(ALICE, "Archive").unwrap();
    srv.seed_message(ALICE, "INBOX", &message(BOB, ALICE, "Keep me", "keep"), &[]).unwrap();
    let mut s = session();
    let acct = add_alice(&mut s, &srv, ALICE_PW);
    sync(&mut s);
    let archive = role(&s, acct, FolderRole::Archive);
    assert!(s.mailbox.folder(archive).unwrap().remote_name.is_none(), "the server has none yet");
    let inbox = role(&s, acct, FolderRole::Inbox);
    let keep = find(&s, inbox, "Keep me").unwrap();
    select(&mut s, keep);
    run(&mut s, "mail.archive", json!({"ids": [keep.0]}));
    settle(&mut s);
    assert_eq!(server_subjects(&srv, ALICE, "Archive"), ["Keep me"], "the message stays on the server");
    assert!(server_subjects(&srv, ALICE, "INBOX").is_empty());
    assert_eq!(srv.special_use(ALICE, "Archive"), Some(mailcraft_testmail::SpecialUse::Archive));
    assert_eq!(s.mailbox.folder(archive).unwrap().remote_name.as_deref(), Some("Archive"));
    sync(&mut s);
    assert_eq!(count(&s, archive, "Keep me"), 1, "no second copy after the next run");
    assert_eq!(s.mailbox.folders.values().filter(|f| f.account == acct && f.name == "Archive").count(), 1);
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

/// Regression (review): a server message moved into a folder below a default folder the server has no mailbox
/// for (Archive/2024 on a server without an archive) was expunged there. The archive mailbox is made first,
/// then the subfolder, and the message is moved into it.
#[test]
fn moving_into_a_folder_below_a_missing_archive_keeps_the_mail_on_the_server() {
    let srv = servers();
    srv.delete_mailbox(ALICE, "Archive").unwrap();
    srv.seed_message(ALICE, "INBOX", &message(BOB, ALICE, "Keep me", "keep"), &[]).unwrap();
    let mut s = session();
    let acct = add_alice(&mut s, &srv, ALICE_PW);
    sync(&mut s);
    let archive = role(&s, acct, FolderRole::Archive);
    assert!(s.mailbox.folder(archive).unwrap().remote_name.is_none(), "the server has none yet");
    s.selection.folder = archive;
    let sub = FolderId(run(&mut s, "folder.new", json!({"name": "2024"}))["folder"].as_u64().unwrap());
    settle(&mut s);
    assert!(s.mailbox.folder(sub).unwrap().remote_name.is_none(), "below a folder the server lacks");
    let inbox = role(&s, acct, FolderRole::Inbox);
    let keep = find(&s, inbox, "Keep me").unwrap();
    select(&mut s, keep);
    run(&mut s, "mail.move", json!({"ids": [keep.0], "folder": sub.0}));
    settle(&mut s);
    assert_eq!(server_subjects(&srv, ALICE, "Archive/2024"), ["Keep me"], "the message stays on the server");
    assert!(server_subjects(&srv, ALICE, "INBOX").is_empty());
    assert_eq!(srv.special_use(ALICE, "Archive"), Some(mailcraft_testmail::SpecialUse::Archive));
    sync(&mut s);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Keep me").count(), 1);
    assert_eq!(s.mailbox.mail.get(keep).unwrap().folder, sub);
    assert_eq!(s.mailbox.folders.values().filter(|f| f.account == acct && f.name == "2024").count(), 1);
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}
