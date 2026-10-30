//! End to end, Microsoft Graph: an account on the local test server's fake Graph mail service (HTTPS,
//! 127.0.0.1, synthetic users; nothing reaches a real Microsoft service): the folder tree with child
//! folders, mail from paged per-folder delta queries with MIME sources from `$value`, later changes from the
//! saved delta links, local changes written back (read, flag, categories, importance, moves, permanent
//! deletes, folders), sending with `sendMail` (and a large attachment through an upload session), throttling,
//! headers-first downloads and the sign-in renewed when the service refuses a token.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, FlagState, FolderId, FolderRole, Importance, ItemId, Mailbox};
use mailcraft_engine::sync::cmds;
use mailcraft_net::graph::GraphClient;
use mailcraft_net::graph::mail::MessagePatch;
use mailcraft_testmail::{Config, TestServers};
use serde_json::{Value, json};

const ALICE: &str = "alice@example.com";
const ALICE_PW: &str = "alice-graph-pw";
const BOB: &str = "bob@example.org";
const WAIT: Duration = Duration::from_secs(30);

fn message(subject: &str, id: &str) -> Vec<u8> {
    format!(
        "From: Bob <{BOB}>\r\nTo: {ALICE}\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
         Message-ID: <{id}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Hello over Graph: {subject}.\r\n"
    )
    .into_bytes()
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn sync(s: &mut Session) {
    run(s, "sendReceive.all", json!({}));
    settle(s);
}

fn settle(s: &mut Session) {
    assert!(s.wait_for_sync(WAIT), "timed out: {:?}", s.sync_status());
    let errors: Vec<String> = s.sync_status().accounts.iter().filter_map(|x| x.error.clone()).collect();
    assert!(errors.is_empty(), "{errors:?}");
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

/// The server's copy of `subject` in `mailbox`: (uid, flags).
fn server_msg(srv: &TestServers, mailbox: &str, subject: &str) -> Option<(u32, Vec<String>)> {
    srv.messages(ALICE, mailbox)
        .unwrap()
        .into_iter()
        .find(|m| mailcraft_mime::parse(&m.raw).map(|x| x.subject()).ok().as_deref() == Some(subject))
        .map(|m| (m.uid, m.flags))
}

fn body(s: &Session, id: ItemId) -> String {
    s.body(id).unwrap().and_then(|b| b.text.clone()).unwrap_or_default()
}

fn servers() -> TestServers {
    TestServers::start(Config {
        users: vec![(ALICE.into(), ALICE_PW.into()), (BOB.into(), "bob-pw".into())],
        add_trace_headers: false,
        graph_max_page: 2,
        ..Config::default()
    })
    .unwrap()
}

fn add_graph(s: &mut Session, srv: &TestServers, extra: Value) -> AccountId {
    let mut p = json!({
        "kind": "graph",
        "address": ALICE,
        "incoming": {"host": srv.graph_url(), "extraRootPem": srv.cert_pem},
        "password": ALICE_PW,
    });
    for (k, v) in extra.as_object().unwrap() {
        if v.is_null() {
            p.as_object_mut().unwrap().remove(k);
        } else {
            p[k] = v.clone();
        }
    }
    let r = run(s, "accounts.add", p);
    assert_eq!(r["kind"], "graph");
    AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
}

/// Another client of the same mailbox (the test's own Graph client).
fn other_client(srv: &TestServers) -> GraphClient {
    let opts = mailcraft_net::ConnectOptions::trusting(srv.cert_der.clone());
    GraphClient::new(&srv.graph_url(), ALICE_PW, mailcraft_net::http::HttpClient::new(opts)).unwrap()
}

#[test]
fn graph_account_receives_writes_back_and_sends() {
    let srv = servers();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let a = add_graph(&mut s, &srv, json!({}));
    assert!(s.mailbox.account(a).unwrap().outgoing.is_none(), "sending goes through Graph");
    assert_eq!(s.push_state(a).0, mailcraft_engine::sync::PushMode::Off, "nothing before the first run");
    let t = run(&mut s, "accounts.test", json!({"account": a.0, "wait": true}));
    assert_eq!(t["ok"], true, "{t}");

    for i in 0..5 {
        srv.seed_message(ALICE, "INBOX", &message(&format!("Hello {i}"), &format!("h{i}")), &[]).unwrap();
    }
    srv.create_mailbox(ALICE, "Projects/2026", None).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    assert!(s.mailbox.folder(inbox).unwrap().remote_name.is_some(), "Inbox mapped to its folder id");
    for role in [FolderRole::SentItems, FolderRole::DeletedItems, FolderRole::JunkEmail, FolderRole::Drafts] {
        let f = s.mailbox.role_folder(a, role).unwrap();
        assert!(s.mailbox.folder(f).unwrap().remote_name.is_some(), "{role:?} mapped");
    }
    let projects = s.mailbox.folders.values().find(|f| f.account == a && f.name == "Projects").unwrap().id;
    let year = s.mailbox.folders.values().find(|f| f.account == a && f.name == "2026").unwrap();
    assert_eq!(year.parent, Some(projects), "child folders keep their parent");
    assert_eq!(s.mailbox.items_in(inbox).len(), 5, "every page of the delta arrived");
    let hello = find(&s, inbox, "Hello 0").unwrap();
    assert!(body(&s, hello).contains("Hello over Graph"), "the MIME source was downloaded");
    assert!(!s.mailbox.mail.get(hello).unwrap().read);
    assert!(srv.graph_requests().iter().any(|r| r.contains("$value")));
    s.poll_sync();
    assert_eq!(s.push_state(a).0, mailcraft_engine::sync::PushMode::Polling, "Graph: the Inbox is polled");

    // Later: new mail and changes made elsewhere arrive through the saved delta link.
    let other = other_client(&srv);
    let (uid1, _) = server_msg(&srv, "INBOX", "Hello 1").unwrap();
    let id1 = srv.graph_message_id(ALICE, "INBOX", uid1).unwrap();
    other
        .update_message(
            &id1,
            &MessagePatch {
                is_read: Some(true),
                categories: Some(vec!["Blue category".into()]),
                importance: Some("high".into()),
                ..Default::default()
            },
        )
        .unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Later", "later"), &["\\Seen"]).unwrap();
    let before = srv.graph_requests().len();
    sync(&mut s);
    let later = find(&s, inbox, "Later").expect("new mail from the delta");
    assert!(s.mailbox.mail.get(later).unwrap().read);
    let one = find(&s, inbox, "Hello 1").unwrap();
    let m = s.mailbox.mail.get(one).unwrap();
    assert!(m.read && m.importance == Importance::High && m.categories == ["Blue category"], "{m:?}");
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Hello 0").count(), 1, "no duplicates");
    let new_requests: Vec<String> = srv.graph_requests()[before..].to_vec();
    assert!(new_requests.iter().any(|r| r.contains("deltatoken")), "continued from the delta link");
    assert_eq!(
        new_requests.iter().filter(|r| r.contains("$value")).count(),
        1,
        "only the new message's source"
    );

    // Local changes reach the service: read, flag, categories, then a move to Archive.
    select(&mut s, hello);
    run(&mut s, "mail.markRead", json!({"ids": [hello.0]}));
    select(&mut s, hello);
    run(&mut s, "mail.flag", json!({"ids": [hello.0]}));
    select(&mut s, hello);
    run(&mut s, "mail.categorize", json!({"ids": [hello.0], "name": "Red category"}));
    settle(&mut s);
    let (uid0, flags) = server_msg(&srv, "INBOX", "Hello 0").unwrap();
    assert!(flags.contains(&"\\Seen".to_owned()) && flags.contains(&"\\Flagged".to_owned()), "{flags:?}");
    let id0 = srv.graph_message_id(ALICE, "INBOX", uid0).unwrap();
    assert_eq!(srv.graph_meta(ALICE, &id0)["categories"], json!(["Red category"]));
    let archive = s.mailbox.role_folder(a, FolderRole::Archive).unwrap();
    select(&mut s, hello);
    run(&mut s, "mail.move", json!({"ids": [hello.0], "folder": archive.0}));
    settle(&mut s);
    assert!(subjects(&srv, ALICE, "Archive").contains(&"Hello 0".to_owned()));
    assert!(!subjects(&srv, ALICE, "INBOX").contains(&"Hello 0".to_owned()));
    sync(&mut s);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Hello 0").count(), 1);
    assert_eq!(s.mailbox.mail.get(hello).unwrap().folder, archive, "the moved item keeps its identity");
    assert!(matches!(s.mailbox.mail.get(hello).unwrap().flag, FlagState::Flagged { .. }));

    // Moved and deleted elsewhere: the item follows, or goes.
    let (uid2, _) = server_msg(&srv, "INBOX", "Hello 2").unwrap();
    let id2 = srv.graph_message_id(ALICE, "INBOX", uid2).unwrap();
    let projects_id = s.mailbox.folder(projects).unwrap().remote_name.clone().unwrap();
    other.move_message(&id2, &projects_id).unwrap();
    let (uid3, _) = server_msg(&srv, "INBOX", "Hello 3").unwrap();
    other.delete_message(&srv.graph_message_id(ALICE, "INBOX", uid3).unwrap()).unwrap();
    let two = find(&s, inbox, "Hello 2").unwrap();
    sync(&mut s);
    assert_eq!(s.mailbox.mail.get(two).map(|m| m.folder), Some(projects), "moved there: moved here");
    assert!(find(&s, inbox, "Hello 3").is_none(), "deleted there: gone here");

    // Permanent delete here removes the service's copy.
    let four = find(&s, inbox, "Hello 4").unwrap();
    select(&mut s, four);
    run(&mut s, "mail.permanentDelete", json!({"ids": [four.0]}));
    settle(&mut s);
    assert!(!subjects(&srv, ALICE, "INBOX").contains(&"Hello 4".to_owned()));

    // Folders made, renamed and removed here.
    s.selection.folder = inbox;
    let receipts =
        FolderId(run(&mut s, "folder.new", json!({"name": "Receipts"}))["folder"].as_u64().unwrap());
    select(&mut s, later);
    run(&mut s, "mail.move", json!({"ids": [later.0], "folder": receipts.0}));
    settle(&mut s);
    sync(&mut s);
    assert!(srv.mailboxes(ALICE).unwrap().contains(&"INBOX/Receipts".to_owned()));
    assert_eq!(subjects(&srv, ALICE, "INBOX/Receipts"), ["Later"], "moved into the new folder, not lost");
    s.selection.folder = receipts;
    run(&mut s, "folder.rename", json!({"name": "Bills"}));
    settle(&mut s);
    sync(&mut s);
    assert!(srv.mailboxes(ALICE).unwrap().contains(&"INBOX/Bills".to_owned()));
    assert_eq!(s.mailbox.folders.values().filter(|f| f.name == "Bills").count(), 1);

    // Throttled: the run waits as asked and still finishes.
    srv.graph_throttle(2, 1);
    srv.seed_message(ALICE, "INBOX", &message("After a pause", "pause"), &[]).unwrap();
    sync(&mut s);
    assert!(find(&s, inbox, "After a pause").is_some());

    // Sending: sendMail delivers and the service files the copy in Sent Items.
    let c = run(&mut s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.setTo", json!({"compose": c, "value": BOB}));
    run(&mut s, "compose.setBcc", json!({"compose": c, "value": "carol@example.net"}));
    run(&mut s, "compose.setSubject", json!({"compose": c, "value": "Sent over Graph"}));
    run(&mut s, "compose.insertText", json!({"compose": c, "text": "Hi Bob."}));
    let r = run(&mut s, "compose.send", json!({"compose": c}));
    assert_eq!(r["sent"], true, "{r}");
    sync(&mut s);
    sync(&mut s);
    let outbox = s.mailbox.role_folder(a, FolderRole::Outbox).unwrap();
    assert_eq!(s.mailbox.total_count(outbox), 0, "Outbox emptied");
    assert!(subjects(&srv, BOB, "INBOX").contains(&"Sent over Graph".to_owned()), "Bob received it");
    let last = srv.submissions().last().cloned().unwrap();
    assert!(
        last.rcpt_to.contains(&"carol@example.net".to_owned()),
        "Bcc reached the service: {:?}",
        last.rcpt_to
    );
    let sent = s.mailbox.role_folder(a, FolderRole::SentItems).unwrap();
    assert!(find(&s, sent, "Sent over Graph").is_some(), "the service's copy came back into Sent Items");
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Sent over Graph").count(), 1);
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

#[test]
fn graph_large_attachment_goes_through_an_upload_session() {
    let srv = servers();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let a = add_graph(&mut s, &srv, json!({}));
    sync(&mut s);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plans.bin");
    std::fs::write(&file, vec![0x5au8; 4 * 1024 * 1024]).unwrap();
    let c = run(&mut s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.setTo", json!({"compose": c, "value": BOB}));
    run(&mut s, "compose.setSubject", json!({"compose": c, "value": "Big plans"}));
    run(&mut s, "compose.attachFile", json!({"compose": c, "path": file.to_string_lossy()}));
    let r = run(&mut s, "compose.send", json!({"compose": c}));
    assert_eq!(r["sent"], true, "{r}");
    sync(&mut s);
    let outbox = s.mailbox.role_folder(a, FolderRole::Outbox).unwrap();
    assert_eq!(s.mailbox.total_count(outbox), 0, "{:?}", s.sync_status());
    let req = srv.graph_requests();
    assert!(req.iter().any(|r| r.contains("createUploadSession")), "{req:?}");
    assert!(req.iter().any(|r| r.ends_with("/send")), "sent as a draft");
    let got = srv.messages(BOB, "INBOX").unwrap();
    assert_eq!(got.len(), 1);
    let parsed = mailcraft_mime::parse(&got[0].raw).unwrap();
    let files = parsed.attachments();
    assert_eq!(files.len(), 1);
    assert_eq!((files[0].filename.as_str(), files[0].data.len()), ("plans.bin", 4 * 1024 * 1024));
}

#[test]
fn graph_headers_first_then_marked_download() {
    let srv = servers();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let a = add_graph(&mut s, &srv, json!({"mode": "headers"}));
    srv.seed_message(ALICE, "INBOX", &message("Report", "report"), &["\\Seen"]).unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Keep", "keep"), &[]).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let report = find(&s, inbox, "Report").expect("the header arrived");
    assert!(cmds::is_header_only(&s, report));
    assert!(s.mailbox.mail.get(report).unwrap().read, "state comes with the header");
    assert!(s.mailbox.mail.get(report).unwrap().from.as_ref().is_some_and(|f| f.addr == BOB));
    assert!(!srv.graph_requests().iter().any(|r| r.contains("$value")), "no source downloaded yet");
    // Moving one out of the account would delete the only whole copy on the service: refused, and its
    // download starts so the move works afterwards.
    let keep = find(&s, inbox, "Keep").unwrap();
    let local = s
        .mailbox
        .folders
        .values()
        .find(|f| f.account != a && f.kind == mailcraft_engine::model::FolderKind::Mail)
        .map(|f| f.id)
        .unwrap();
    select(&mut s, keep);
    let e = s.run("mail.move", &json!({"ids": [keep.0], "folder": local.0})).unwrap_err();
    assert!(e.to_string().contains("only its header"), "{e}");
    assert_eq!(s.mailbox.mail.get(keep).unwrap().folder, inbox, "it stays where it was");
    settle(&mut s);
    assert!(!cmds::is_header_only(&s, keep) && body(&s, keep).contains("Hello over Graph: Keep"));
    assert!(cmds::is_header_only(&s, report));
    s.selection.folder = inbox;
    let r = cmds::mark(&mut s, &json!({"ids": [report.0]})).unwrap();
    assert_eq!(r["marked"], json!([report.0]));
    cmds::process_marked(&mut s, &json!({})).unwrap();
    settle(&mut s);
    assert!(!cmds::is_header_only(&s, report));
    assert!(body(&s, report).contains("Hello over Graph: Report"));
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Report").count(), 1);
    // Headers and then whole messages, in one run.
    cmds::preferences(&mut s, &json!({"account": a.0, "mode": "headersThenFull"})).unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Second", "second"), &[]).unwrap();
    sync(&mut s);
    let second = find(&s, inbox, "Second").unwrap();
    assert!(!cmds::is_header_only(&s, second));
    assert!(body(&s, second).contains("Hello over Graph: Second"));
}

#[test]
fn graph_oauth_sign_in_uses_graph_scopes_and_renews_a_refused_token() {
    let srv = servers();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let a = add_graph(&mut s, &srv, json!({"password": null}));
    // The Microsoft provider asks for Graph's delegated permissions, not the IMAP/SMTP ones.
    let v = run(
        &mut s,
        "accounts.oauthSignIn",
        json!({"account": a.0, "provider": "microsoft", "clientId": "my-app", "flow": "browser"}),
    );
    let url = v["authUrl"].as_str().unwrap();
    assert!(url.contains("graph.microsoft.com%2FMail.ReadWrite") && url.contains("Mail.Send"), "{url}");
    // Contacts, calendars (shared ones too), To Do tasks and the mailbox's time zone.
    for scope in
        ["Contacts.ReadWrite", "Calendars.ReadWrite.Shared", "Tasks.ReadWrite", "MailboxSettings.Read"]
    {
        assert!(url.contains(scope), "{scope}: {url}");
    }
    assert!(!url.contains("IMAP.AccessAsUser"), "{url}");
    run(&mut s, "accounts.oauthCancel", json!({"account": a.0}));

    // A custom provider (the test authorization server) signs in with the device flow.
    let (auth, token, device) = srv.oauth_endpoints();
    s.sync.oauth_http = Some(mailcraft_net::ConnectOptions::trusting(srv.cert_der.clone()));
    let v = run(
        &mut s,
        "accounts.oauthSignIn",
        json!({"account": a.0, "provider": "custom", "clientId": mailcraft_testmail::OAUTH_TEST_CLIENT,
               "flow": "device", "tokenEndpoint": token, "deviceAuthorizationEndpoint": device,
               "authorizationEndpoint": auth}),
    );
    assert_eq!(v["flow"], "device", "{v}");
    let end = std::time::Instant::now() + WAIT;
    let code = loop {
        let st = run(&mut s, "accounts.oauthStatus", json!({"account": a.0}));
        if let Some(c) = st["userCode"].as_str().filter(|c| !c.is_empty()) {
            break c.to_owned();
        }
        assert!(std::time::Instant::now() < end, "no device code: {st}");
        std::thread::sleep(Duration::from_millis(20));
    };
    srv.oauth_approve(&code, ALICE).unwrap();
    while run(&mut s, "accounts.oauthStatus", json!({"account": a.0}))["status"] != "done" {
        assert!(std::time::Instant::now() < end, "sign-in did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
    srv.seed_message(ALICE, "INBOX", &message("Signed in", "signed"), &[]).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    assert!(find(&s, inbox, "Signed in").is_some());
    // The service refuses the token (expired early): the run renews it and goes on.
    srv.oauth_expire_access_tokens();
    srv.seed_message(ALICE, "INBOX", &message("After renewal", "renewed"), &[]).unwrap();
    sync(&mut s);
    assert!(find(&s, inbox, "After renewal").is_some());
    assert!(srv.oauth_grants().iter().any(|g| g == "refresh_token"), "{:?}", srv.oauth_grants());
}

/// Regression (found while building the Graph account): mail moved into a default folder the service had no
/// folder for was deleted from the service, as the folder stayed local; and every Inbox poll read the whole
/// folder tree again.
#[test]
fn graph_archive_is_made_on_a_service_without_one_and_polls_stay_small() {
    let srv = servers();
    srv.delete_mailbox(ALICE, "Archive").unwrap();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let a = add_graph(&mut s, &srv, json!({}));
    srv.seed_message(ALICE, "INBOX", &message("Keep me", "keep"), &[]).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let archive = s.mailbox.role_folder(a, FolderRole::Archive).unwrap();
    assert!(s.mailbox.folder(archive).unwrap().remote_name.is_none(), "no Archive on the service yet");
    let keep = find(&s, inbox, "Keep me").unwrap();
    select(&mut s, keep);
    run(&mut s, "mail.move", json!({"ids": [keep.0], "folder": archive.0}));
    settle(&mut s);
    sync(&mut s);
    assert_eq!(subjects(&srv, ALICE, "Archive"), ["Keep me"], "moved into a new Archive, not deleted");
    assert!(s.mailbox.folder(archive).unwrap().remote_name.is_some());
    assert_eq!(s.mailbox.folders.values().filter(|f| f.account == a && f.name == "Archive").count(), 1);

    // Updating only the Inbox reads its delta, not the folder tree.
    let before = srv.graph_requests().len();
    srv.seed_message(ALICE, "INBOX", &message("Polled", "polled"), &[]).unwrap();
    run(&mut s, "sendReceive.folder", json!({"folder": inbox.0}));
    settle(&mut s);
    assert!(find(&s, inbox, "Polled").is_some());
    let reqs = srv.graph_requests()[before..].to_vec();
    assert!(
        !reqs.iter().any(|r| r.contains("childFolders") || r.ends_with("mailFolders?$top=100")),
        "{reqs:?}"
    );
}

#[test]
fn graph_accounts_refuse_settings_they_cannot_use() {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    for bad in [
        json!({"kind": "graph", "address": ALICE, "incoming": {"host": "graph.example.com"}}),
        json!({"kind": "graph", "address": ALICE, "outgoing": {"host": "smtp.example.com"}}),
        json!({"kind": "graph", "address": ALICE, "incoming": {"host": "http://127.0.0.1:9/v1.0"}}),
    ] {
        assert!(s.run("accounts.add", &bad).is_err(), "{bad}");
    }
    let r = run(&mut s, "accounts.add", json!({"kind": "graph", "address": ALICE}));
    let a = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    let acct = s.mailbox.account(a).unwrap();
    assert_eq!(
        acct.incoming.as_ref().unwrap().host,
        "https://graph.microsoft.com/v1.0",
        "the global endpoint"
    );
    assert!(acct.can_send());
    let listed = run(&mut s, "accounts.list", json!({}));
    assert!(listed.to_string().contains("\"graph\""), "{listed}");
}

/// A message in the Inbox of Local Folders (read, as mail received there).
fn local_message(s: &mut Session, a: AccountId, raw: &[u8]) -> ItemId {
    let inbox = s
        .mailbox
        .folders
        .values()
        .find(|f| f.account != a && f.role == Some(FolderRole::Inbox))
        .map(|f| f.id)
        .unwrap();
    let mut item = mailcraft_store::import::item_from_raw(raw, inbox, s.now());
    item.read = true;
    s.add_message(item, &mailcraft_engine::Content { source: Some(raw.to_vec()), body: None }).unwrap()
}

/// A message with a 3 MB file: too large to upload in one request.
fn big_message(subject: &str, id: &str) -> Vec<u8> {
    let data = "WlpaWlpa".repeat(500_000);
    let mut lines = String::new();
    for chunk in data.as_bytes().chunks(76) {
        lines.push_str(std::str::from_utf8(chunk).unwrap());
        lines.push_str("\r\n");
    }
    format!(
        "From: Bob <{BOB}>\r\nTo: {ALICE}\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
         Message-ID: <{id}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n\
         --b1\r\nContent-Type: text/plain\r\n\r\nThe plans are attached.\r\n--b1\r\n\
         Content-Type: application/octet-stream; name=\"plans.bin\"\r\n\
         Content-Disposition: attachment; filename=\"plans.bin\"\r\nContent-Transfer-Encoding: base64\r\n\r\n\
         {lines}--b1--\r\n"
    )
    .into_bytes()
}

/// Regression: a run of the Inbox only (the poll) took a message removed from the Inbox for gone, so a
/// message moved (here or elsewhere) to a folder that run did not read lost its item.
#[test]
fn graph_moved_mail_keeps_its_item_through_inbox_only_runs() {
    let srv = servers();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let a = add_graph(&mut s, &srv, json!({}));
    for (subject, id) in [("Move me", "mv"), ("Moved there", "mt"), ("Deleted there", "dt")] {
        srv.seed_message(ALICE, "INBOX", &message(subject, id), &[]).unwrap();
    }
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let archive = s.mailbox.role_folder(a, FolderRole::Archive).unwrap();
    let archive_id = s.mailbox.folder(archive).unwrap().remote_name.clone().unwrap();
    let mv = find(&s, inbox, "Move me").unwrap();
    select(&mut s, mv);
    run(&mut s, "mail.move", json!({"ids": [mv.0], "folder": archive.0}));
    settle(&mut s);
    assert!(subjects(&srv, ALICE, "Archive").contains(&"Move me".to_owned()));

    // Another client moves one and deletes one.
    let other = other_client(&srv);
    let (uid, _) = server_msg(&srv, "INBOX", "Moved there").unwrap();
    other.move_message(&srv.graph_message_id(ALICE, "INBOX", uid).unwrap(), &archive_id).unwrap();
    let (uid, _) = server_msg(&srv, "INBOX", "Deleted there").unwrap();
    other.delete_message(&srv.graph_message_id(ALICE, "INBOX", uid).unwrap()).unwrap();
    let mt = find(&s, inbox, "Moved there").unwrap();

    run(&mut s, "sendReceive.folder", json!({"folder": inbox.0}));
    settle(&mut s);
    assert_eq!(s.mailbox.mail.get(mv).map(|m| m.folder), Some(archive), "the moved item is still here");
    assert_eq!(s.mailbox.mail.get(mt).map(|m| m.folder), Some(archive), "moved there: follows here");
    assert!(s.mailbox.mail.items().all(|m| m.subject != "Deleted there"), "deleted there: gone");
    // A full run later finds nothing new.
    sync(&mut s);
    for subject in ["Move me", "Moved there"] {
        assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == subject).count(), 1, "{subject}");
    }
    assert_eq!(s.mailbox.mail.get(mv).map(|m| m.folder), Some(archive));
}

/// Regression: mail copied in from another account was uploaded and then became a draft here (the service
/// files uploads as drafts); no test covered uploads.
#[test]
fn graph_mail_copied_in_is_uploaded_once_and_stays_mail() {
    let srv = servers();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let a = add_graph(&mut s, &srv, json!({}));
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let orig = local_message(&mut s, a, &message("Copied in", "copied"));
    select(&mut s, orig);
    run(&mut s, "mail.copy", json!({"ids": [orig.0], "folder": inbox.0}));
    settle(&mut s);
    sync(&mut s);
    sync(&mut s);
    assert_eq!(subjects(&srv, ALICE, "INBOX"), ["Copied in"], "one copy on the service");
    let (_, flags) = server_msg(&srv, "INBOX", "Copied in").unwrap();
    assert!(flags.contains(&"\\Seen".to_owned()), "its state went with it: {flags:?}");
    let here: Vec<_> = s.mailbox.mail.items().filter(|m| m.folder == inbox).cloned().collect();
    assert_eq!(here.len(), 1, "no second item from the delta");
    assert!(here[0].remote_id.is_some(), "the upload is the item's copy on the service");
    assert!(!here[0].is_draft, "received mail stays mail");
    assert!(here[0].read);
    assert!(s.mailbox.mail.get(orig).is_some(), "the original stays in Local Folders");
    assert!(body(&s, here[0].id).contains("Hello over Graph: Copied in"));
}

/// Regression: when a large file of an upload or a send failed, the message made without it stayed on the
/// service, and each later run made another one.
#[test]
fn graph_failed_attachment_uploads_leave_nothing_behind() {
    let srv = servers();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let a = add_graph(&mut s, &srv, json!({}));
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    srv.graph_refuse_uploads(true);
    let orig = local_message(&mut s, a, &big_message("Big copy", "bigcopy"));
    select(&mut s, orig);
    run(&mut s, "mail.copy", json!({"ids": [orig.0], "folder": inbox.0}));
    settle(&mut s);
    sync(&mut s);
    sync(&mut s);
    assert!(srv.graph_requests().iter().any(|r| r.contains("createUploadSession")));
    assert!(subjects(&srv, ALICE, "INBOX").is_empty(), "no message without its file");
    assert!(subjects(&srv, ALICE, "Drafts").is_empty());
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Big copy").count(), 2, "original and copy");

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plans.bin");
    std::fs::write(&file, vec![0x5au8; 3 * 1024 * 1024]).unwrap();
    let c = run(&mut s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.setTo", json!({"compose": c, "value": BOB}));
    run(&mut s, "compose.setSubject", json!({"compose": c, "value": "Big plans"}));
    run(&mut s, "compose.attachFile", json!({"compose": c, "path": file.to_string_lossy()}));
    assert_eq!(run(&mut s, "compose.send", json!({"compose": c}))["sent"], true);
    for _ in 0..2 {
        run(&mut s, "sendReceive.all", json!({}));
        assert!(s.wait_for_sync(WAIT));
    }
    let outbox = s.mailbox.role_folder(a, FolderRole::Outbox).unwrap();
    assert_eq!(s.mailbox.total_count(outbox), 1, "not sent: it waits in the Outbox");
    assert!(subjects(&srv, ALICE, "Drafts").is_empty(), "no half-made draft left on the service");
    assert!(srv.messages(BOB, "INBOX").unwrap().is_empty());

    // Once the service takes the files, both go through once.
    srv.graph_refuse_uploads(false);
    sync(&mut s);
    sync(&mut s);
    assert_eq!(subjects(&srv, ALICE, "INBOX"), ["Big copy"]);
    assert_eq!(s.mailbox.total_count(outbox), 0);
    assert_eq!(subjects(&srv, BOB, "INBOX"), ["Big plans"]);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.folder == inbox && m.subject == "Big copy").count(), 1);
}

/// Regression: a message moved into a folder of the account that stays here (below Conversation History,
/// which the service does not have) was deleted for good on the service; a header-only one lost its body.
#[test]
fn graph_mail_moved_below_a_local_default_folder_stays_on_the_service() {
    let srv = servers();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let a = add_graph(&mut s, &srv, json!({"mode": "headers"}));
    srv.seed_message(ALICE, "INBOX", &message("Talk", "talk"), &[]).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let history = s.mailbox.role_folder(a, FolderRole::ConversationHistory).unwrap();
    assert!(s.mailbox.folder(history).unwrap().remote_name.is_none(), "it stays here");
    s.selection.folder = history;
    let chats = FolderId(run(&mut s, "folder.new", json!({"name": "Chats"}))["folder"].as_u64().unwrap());
    let talk = find(&s, inbox, "Talk").unwrap();
    assert!(cmds::is_header_only(&s, talk));
    select(&mut s, talk);
    run(&mut s, "mail.move", json!({"ids": [talk.0], "folder": chats.0}));
    settle(&mut s);
    sync(&mut s);
    assert_eq!(subjects(&srv, ALICE, "INBOX"), ["Talk"], "the service keeps its copy");
    assert_eq!(s.mailbox.mail.get(talk).map(|m| m.folder), Some(chats));
    // Changed on the service: the state comes, the item stays where it was put.
    let (uid, _) = server_msg(&srv, "INBOX", "Talk").unwrap();
    let id = srv.graph_message_id(ALICE, "INBOX", uid).unwrap();
    other_client(&srv)
        .update_message(&id, &MessagePatch { is_read: Some(true), ..Default::default() })
        .unwrap();
    sync(&mut s);
    let m = s.mailbox.mail.get(talk).unwrap();
    assert!(m.folder == chats && m.read, "{m:?}");
    // Its body can still be downloaded.
    s.selection.folder = chats;
    cmds::mark(&mut s, &json!({"ids": [talk.0]})).unwrap();
    cmds::process_marked(&mut s, &json!({})).unwrap();
    settle(&mut s);
    assert!(body(&s, talk).contains("Hello over Graph: Talk"));
    assert_eq!(subjects(&srv, ALICE, "INBOX"), ["Talk"]);
}

/// Regression: new start or due days of a message already flagged never reached the service, and days set
/// there never came here.
#[test]
fn graph_flag_days_follow_changes_both_ways() {
    let srv = servers();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let a = add_graph(&mut s, &srv, json!({}));
    srv.seed_message(ALICE, "INBOX", &message("Due", "due"), &[]).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let due = find(&s, inbox, "Due").unwrap();
    let (uid, _) = server_msg(&srv, "INBOX", "Due").unwrap();
    let id = srv.graph_message_id(ALICE, "INBOX", uid).unwrap();
    let day = |d: &str| mailcraft_mime::DateTime::parse_rfc5322(d).unwrap().unix;
    select(&mut s, due);
    run(&mut s, "mail.flagCustom", json!({"ids": [due.0], "due": day("Fri, 9 Oct 2026 17:00:00 +0000")}));
    settle(&mut s);
    assert_eq!(srv.graph_meta(ALICE, &id)["due"], "2026-10-09T17:00:00");
    select(&mut s, due);
    run(&mut s, "mail.flagCustom", json!({"ids": [due.0], "due": day("Tue, 13 Oct 2026 17:00:00 +0000")}));
    settle(&mut s);
    assert_eq!(srv.graph_meta(ALICE, &id)["due"], "2026-10-13T17:00:00", "a new day for a flagged message");

    // Set on the service by another client: it comes here.
    let patch = MessagePatch {
        flag: Some((
            mailcraft_net::graph::mail::FlagStatus::Flagged,
            Some("2026-10-19T08:00:00".into()),
            Some("2026-10-20T17:00:00".into()),
        )),
        ..Default::default()
    };
    other_client(&srv).update_message(&id, &patch).unwrap();
    sync(&mut s);
    let FlagState::Flagged { due: d, .. } = &s.mailbox.mail.get(due).unwrap().flag else {
        panic!("not flagged")
    };
    assert_eq!(d.map(|d| d.unix), Some(day("Tue, 20 Oct 2026 17:00:00 +0000")));
    sync(&mut s);
    assert_eq!(srv.graph_meta(ALICE, &id)["due"], "2026-10-20T17:00:00", "nothing sent back over it");

    // Flagged without a day: cleared there too.
    select(&mut s, due);
    run(&mut s, "mail.flag", json!({"ids": [due.0], "kind": "noDate"}));
    settle(&mut s);
    assert!(srv.graph_meta(ALICE, &id)["due"].is_null(), "{}", srv.graph_meta(ALICE, &id));
    let (_, flags) = server_msg(&srv, "INBOX", "Due").unwrap();
    assert!(flags.contains(&"\\Flagged".to_owned()));
}
