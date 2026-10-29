//! Live tests against a real mail server (opt-in; never run in CI). They drive the engine like a user: add an
//! account (autodiscovery or manual settings), receive over IMAP (folders, flags, move, delete, IDLE push) and
//! POP3 (leave on server), send over SMTP (implicit TLS) and JMAP (EmailSubmission), JMAP push, CardDAV and
//! CalDAV both ways (contacts, events with a recurring series and an exception, tasks), JMAP Contacts and
//! Calendars when the server offers them, Archive on a server without an archive mailbox, quota, server search
//! (Include Older Results) and shared folders.
//!
//! Every test is `#[ignore]` and returns early unless these are set (see `docs/live-tests.md`):
//!
//! - `MC_LIVE_STALWART_HOST`: the server's host name (IMAPS 993, POP3S 995, SMTPS 465, HTTPS 443),
//! - `MC_LIVE_STALWART_USER`: the login and address of a mailbox kept for testing,
//! - `MC_LIVE_STALWART_PASS`: its password.
//!
//! Run: `cargo test -p mailcraft-engine --test live_servers -- --ignored --test-threads=1`.
//!
//! Rules the tests keep: mail goes only to the test mailbox itself; everything a test creates carries a run tag
//! (`MCLIVE…`) and is removed at the end, even when the test fails (a drop guard); nothing else in the mailbox
//! is changed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::{Duration, Instant};

use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, DateTime, FolderId, FolderRole, ItemId, Mailbox};
use mailcraft_engine::sync::PushMode;
use mailcraft_net::ConnectOptions;
use mailcraft_net::dav::{DavAuth, DavClient, Precondition, Service};
use mailcraft_net::http::HttpClient;
use mailcraft_net::jmap::{JmapAuth, JmapClient};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(90);

// ---------------------------------------------------------------------------------------------------------
// Settings and the server-side helper
// ---------------------------------------------------------------------------------------------------------

#[derive(Clone)]
struct Live {
    host: String,
    user: String,
    pass: String,
}

fn live() -> Option<Live> {
    let get = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    Some(Live {
        host: get("MC_LIVE_STALWART_HOST")?,
        user: get("MC_LIVE_STALWART_USER")?,
        pass: get("MC_LIVE_STALWART_PASS")?,
    })
}

macro_rules! live_or_skip {
    () => {
        match live() {
            Some(l) => l,
            None => {
                eprintln!("skipped: set MC_LIVE_STALWART_HOST, _USER and _PASS to run the live tests");
                return;
            }
        }
    };
}

/// A tag unique to this run; everything a test creates carries it.
fn tag(what: &str) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("MCLIVE{}{what}", now.as_millis() % 10_000_000_000)
}

/// Direct access to the server (JMAP for mail, contacts and calendars; WebDAV for the DAV collections), to
/// seed what "another device" does and to check and clean up what the client did.
struct Server {
    live: Live,
    jmap: JmapClient,
    dav: DavClient,
}

impl Server {
    fn connect(live: &Live) -> Server {
        let auth = JmapAuth::Basic { user: live.user.clone(), password: live.pass.clone() };
        let jmap = JmapClient::connect(
            &format!("https://{}/", live.host),
            auth,
            HttpClient::new(ConnectOptions::default()),
        )
        .expect("JMAP session");
        let dav = DavClient::new(
            ConnectOptions::default(),
            DavAuth::Basic { user: live.user.clone(), password: live.pass.clone() },
        )
        .trust(&format!("https://{}/", live.host));
        Server { live: live.clone(), jmap, dav }
    }

    fn account(&self) -> String {
        self.jmap.account_id().to_owned()
    }

    fn call(&self, name: &str, mut args: Value) -> Value {
        if args.get("accountId").is_none() {
            args["accountId"] = json!(self.account());
        }
        let using = if name.starts_with("AddressBook/") || name.starts_with("ContactCard/") {
            "urn:ietf:params:jmap:contacts"
        } else if name.starts_with("Calendar") {
            "urn:ietf:params:jmap:calendars"
        } else {
            ""
        };
        let r = if using.is_empty() {
            self.jmap.call_one(name, args.clone())
        } else {
            self.jmap.call_with(&[using], name, args.clone())
        };
        r.unwrap_or_else(|e| panic!("{name} {args}: {e}"))
    }

    fn mailboxes(&self) -> Vec<Value> {
        self.call("Mailbox/get", json!({"ids": null}))["list"].as_array().cloned().unwrap_or_default()
    }

    fn mailbox_id(&self, role: &str) -> String {
        self.mailboxes()
            .iter()
            .find(|m| m["role"].as_str() == Some(role))
            .and_then(|m| m["id"].as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("no {role} mailbox"))
    }

    fn mailbox_named(&self, name: &str) -> Option<String> {
        self.mailboxes()
            .iter()
            .find(|m| m["name"].as_str() == Some(name))
            .and_then(|m| m["id"].as_str().map(str::to_owned))
    }

    fn raw_message(&self, subject: &str, body: &str) -> Vec<u8> {
        let user = &self.live.user;
        format!(
            "From: MailCraft Live Test <{user}>\r\nTo: {user}\r\nSubject: {subject}\r\n\
             Date: {}\r\nMessage-ID: <{}@example.com>\r\nMIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n",
            DateTime::now_utc().to_rfc5322(),
            subject.replace(' ', "."),
        )
        .into_bytes()
    }

    /// Puts a message in the mailbox with `role` (as if it had arrived), optionally received in the past.
    fn seed(&self, role: &str, subject: &str, body: &str, received_at: Option<i64>) -> String {
        let blob = self.jmap.upload(&self.raw_message(subject, body), "message/rfc822").expect("upload");
        let mut email = json!({"blobId": blob, "mailboxIds": {self.mailbox_id(role): true}, "keywords": {}});
        if let Some(t) = received_at {
            email["receivedAt"] = json!(DateTime::from_unix(t, 0).to_rfc3339());
        }
        let r = self.call("Email/import", json!({"emails": {"i0": email}}));
        r["created"]["i0"]["id"].as_str().unwrap_or_else(|| panic!("Email/import: {r}")).to_owned()
    }

    /// (id, subject, mailbox ids, keywords) of every email whose subject holds `tag`.
    fn emails(&self, tag: &str) -> Vec<(String, String, Vec<String>, Vec<String>)> {
        let q = self.call("Email/query", json!({"filter": {"subject": tag}, "limit": 200}));
        let ids: Vec<Value> = q["ids"].as_array().cloned().unwrap_or_default();
        if ids.is_empty() {
            return Vec::new();
        }
        let g = self.call(
            "Email/get",
            json!({"ids": ids, "properties": ["id", "subject", "mailboxIds", "keywords"]}),
        );
        let keys = |v: &Value| -> Vec<String> {
            v.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default()
        };
        g["list"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter(|e| e["subject"].as_str().is_some_and(|s| s.contains(tag)))
            .map(|e| {
                (
                    e["id"].as_str().unwrap_or_default().to_owned(),
                    e["subject"].as_str().unwrap_or_default().to_owned(),
                    keys(&e["mailboxIds"]),
                    keys(&e["keywords"]),
                )
            })
            .collect()
    }

    fn subjects_in(&self, tag: &str, mailbox: &str) -> Vec<String> {
        self.emails(tag).into_iter().filter(|e| e.2.iter().any(|m| m == mailbox)).map(|e| e.1).collect()
    }

    fn keywords(&self, subject: &str) -> Vec<String> {
        self.emails(subject).into_iter().find(|e| e.1 == subject).map(|e| e.3).unwrap_or_default()
    }

    // ----- DAV -----

    fn dav_start(&self) -> String {
        format!("https://{}/", self.live.host)
    }

    fn dav_collection(&self, service: Service, events: bool) -> String {
        let list = self.dav.discover(&self.dav_start(), service).expect("DAV discovery");
        list.iter()
            .find(|c| match service {
                Service::Contacts => c.addressbook,
                Service::Calendars if events => c.holds_events(),
                Service::Calendars => c.holds_tasks(),
            })
            .map(|c| c.url.clone())
            .unwrap_or_else(|| panic!("no {service:?} collection in {list:?}"))
    }

    /// (url, etag, data) of the resources in `collection` whose text holds `tag`.
    fn dav_items(&self, collection: &str, tag: &str) -> Vec<(String, String, String)> {
        let list = self.dav.list(collection).expect("DAV listing");
        let urls: Vec<String> = list.iter().map(|r| r.url.clone()).collect();
        if urls.is_empty() {
            return Vec::new();
        }
        let service = if collection.contains("card") { Service::Contacts } else { Service::Calendars };
        let mut out = Vec::new();
        for chunk in urls.chunks(50) {
            let got = self.dav.multiget(collection, chunk, service).unwrap_or_default();
            out.extend(got.into_iter().filter(|f| f.data.contains(tag)).map(|f| (f.url, f.etag, f.data)));
        }
        out
    }

    fn dav_put(&self, collection: &str, name: &str, data: &str, content_type: &str) -> String {
        let url = format!("{}/{name}", collection.trim_end_matches('/'));
        self.dav.put(&url, data.as_bytes(), content_type, &Precondition::Any).expect("DAV PUT");
        url
    }

    // ----- Clean-up -----

    /// Removes everything carrying `tag`: emails, mailboxes, DAV resources, JMAP contacts and events.
    fn cleanup(&self, tag: &str) {
        let ids: Vec<String> = self.emails(tag).into_iter().map(|e| e.0).collect();
        if !ids.is_empty() {
            let _ = self.jmap.call_one("Email/set", json!({"accountId": self.account(), "destroy": ids}));
        }
        let boxes: Vec<String> = self
            .mailboxes()
            .iter()
            .filter(|m| m["name"].as_str().is_some_and(|n| n.contains(tag)))
            .filter_map(|m| m["id"].as_str().map(str::to_owned))
            .collect();
        if !boxes.is_empty() {
            let _ = self.jmap.call_one(
                "Mailbox/set",
                json!({"accountId": self.account(), "destroy": boxes, "onDestroyRemoveEmails": true}),
            );
        }
        for service in [Service::Contacts, Service::Calendars] {
            let Ok(list) = self.dav.discover(&self.dav_start(), service) else { continue };
            for c in list {
                for (url, _, _) in self.dav_items(&c.url, tag) {
                    let _ = self.dav.delete(&url, None);
                }
            }
        }
        if self.jmap.session().has_capability("urn:ietf:params:jmap:contacts") {
            let r = self.jmap.call_with(
                &["urn:ietf:params:jmap:contacts"],
                "ContactCard/get",
                json!({"accountId": self.account(), "ids": null}),
            );
            let ids: Vec<Value> = r
                .map(|r| r["list"].as_array().cloned().unwrap_or_default())
                .unwrap_or_default()
                .into_iter()
                .filter(|c| c.to_string().contains(tag))
                .filter_map(|c| c.get("id").cloned())
                .collect();
            if !ids.is_empty() {
                let _ = self.jmap.call_with(
                    &["urn:ietf:params:jmap:contacts"],
                    "ContactCard/set",
                    json!({"accountId": self.account(), "destroy": ids}),
                );
            }
        }
        if self.jmap.session().has_capability("urn:ietf:params:jmap:calendars") {
            let r = self.jmap.call_with(
                &["urn:ietf:params:jmap:calendars"],
                "CalendarEvent/get",
                json!({"accountId": self.account(), "ids": null}),
            );
            let ids: Vec<Value> = r
                .map(|r| r["list"].as_array().cloned().unwrap_or_default())
                .unwrap_or_default()
                .into_iter()
                .filter(|c| c.to_string().contains(tag))
                .filter_map(|c| c.get("id").cloned())
                .collect();
            if !ids.is_empty() {
                let _ = self.jmap.call_with(
                    &["urn:ietf:params:jmap:calendars"],
                    "CalendarEvent/set",
                    json!({"accountId": self.account(), "destroy": ids}),
                );
            }
        }
    }
}

/// Cleans up a run's tag when the test ends, passed or not.
struct Cleanup<'a> {
    srv: &'a Server,
    tag: String,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        self.srv.cleanup(&self.tag);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Client-side helpers
// ---------------------------------------------------------------------------------------------------------

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn session() -> Session {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    s
}

fn no_errors(s: &Session) {
    let errors: Vec<String> = s.sync_status().accounts.iter().filter_map(|a| a.error.clone()).collect();
    assert!(errors.is_empty(), "account errors: {errors:?}");
}

fn sync(s: &mut Session) {
    run(s, "sendReceive.all", json!({}));
    assert!(s.wait_for_sync(WAIT), "send/receive timed out: {:?}", s.sync_status());
    no_errors(s);
}

fn settle(s: &mut Session) {
    assert!(s.wait_for_sync(WAIT), "timed out: {:?}", s.sync_status());
    no_errors(s);
}

fn update_folder(s: &mut Session, f: FolderId) -> Value {
    let r = run(s, "sendReceive.folder", json!({"folder": f.0}));
    settle(s);
    r
}

/// Deletes a folder like a user: into Deleted Items, then from there for good.
fn delete_folder(s: &mut Session, folder: FolderId) {
    for _ in 0..2 {
        if s.mailbox.folders.contains_key(&folder) {
            s.selection.folder = folder;
            run(s, "folder.delete", json!({"folder": folder.0, "confirm": true}));
            settle(s);
            sync(s);
        }
    }
    assert!(!s.mailbox.folders.contains_key(&folder));
}

fn id_of(v: &Value) -> AccountId {
    AccountId(u32::try_from(v["account"].as_u64().unwrap()).unwrap())
}

fn role(s: &Session, a: AccountId, r: FolderRole) -> FolderId {
    s.mailbox.role_folder(a, r).unwrap_or_else(|| panic!("no {r:?} folder"))
}

fn find(s: &Session, folder: FolderId, subject: &str) -> Option<ItemId> {
    s.mailbox.mail.items().find(|m| m.folder == folder && m.subject == subject).map(|m| m.id)
}

fn find_any(s: &Session, subject: &str) -> Option<ItemId> {
    s.mailbox.mail.items().find(|m| m.subject == subject).map(|m| m.id)
}

fn select(s: &mut Session, id: ItemId) {
    s.selection.folder = s.mailbox.mail.get(id).unwrap().folder;
    s.selection.items = vec![id];
    s.selection.focus = Some(id);
}

fn poll_until(s: &mut Session, what: &str, mut done: impl FnMut(&mut Session) -> bool) {
    let end = Instant::now() + WAIT;
    loop {
        s.poll_sync();
        if done(s) {
            return;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}: {:?}", s.sync_status());
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Retries a server-side check for a while (servers index and deliver asynchronously).
fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + WAIT;
    while !ok() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn add_imap(s: &mut Session, l: &Live, extra: Value) -> AccountId {
    let mut p = json!({
        "kind": "imap",
        "address": l.user,
        "senderName": "MailCraft Live Test",
        "incoming": {"host": l.host, "port": 993, "security": "tls"},
        "outgoing": {"host": l.host, "port": 465, "security": "tls"},
        "password": l.pass,
    });
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    id_of(&run(s, "accounts.add", p))
}

fn add_jmap(s: &mut Session, l: &Live, extra: Value) -> AccountId {
    let mut p = json!({
        "kind": "jmap",
        "address": l.user,
        "senderName": "MailCraft Live Test",
        "incoming": {"host": format!("https://{}/", l.host)},
        "password": l.pass,
    });
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    id_of(&run(s, "accounts.add", p))
}

fn send_to_self(s: &mut Session, l: &Live, subject: &str) {
    let c = run(s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(s, "compose.setTo", json!({"compose": c, "value": l.user}));
    run(s, "compose.setSubject", json!({"compose": c, "value": subject}));
    run(s, "compose.insertText", json!({"compose": c, "text": "Sent by the MailCraft live tests."}));
    let r = run(s, "compose.send", json!({"compose": c}));
    assert_eq!(r["sent"], true, "{r}");
}

// ---------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------

#[test]
#[ignore = "live server: set MC_LIVE_STALWART_* (docs/live-tests.md)"]
fn live_discovery_finds_the_servers() {
    let l = live_or_skip!();
    let mut s = session();
    let r = run(&mut s, "accounts.discover", json!({"address": l.user, "wait": true}));
    eprintln!("discovery sources: {} notes: {}", r["sources"], r["notes"]);
    assert_eq!(r["found"], true, "{r}");
    assert_eq!(
        r["imap"]["host"].as_str().map(str::to_ascii_lowercase),
        Some(l.host.to_ascii_lowercase()),
        "{r}"
    );
    assert_eq!(r["imap"]["port"], 993, "{r}");
    assert_eq!(r["imap"]["security"], "tls", "{r}");
    assert_eq!(r["smtp"]["port"], 465, "{r}");
    assert_eq!(r["smtp"]["security"], "tls", "{r}");
    assert!(r["jmap"].is_object(), "the JMAP session is found too: {r}");
}

/// Adds the account from discovered settings, then exercises IMAP and SMTP like a user.
#[test]
#[ignore = "live server: set MC_LIVE_STALWART_* (docs/live-tests.md)"]
fn live_imap_and_smtp() {
    let l = live_or_skip!();
    let srv = Server::connect(&l);
    let t = tag("imap");
    let _guard = Cleanup { srv: &srv, tag: t.clone() };
    let mut s = session();

    // Add the account from what discovery found.
    let d = run(&mut s, "accounts.discover", json!({"address": l.user, "wait": true}));
    assert_eq!(d["found"], true, "{d}");
    let server = |v: &Value| json!({"host": v["host"], "port": v["port"], "security": v["security"]});
    let a = id_of(&run(
        &mut s,
        "accounts.add",
        json!({"kind": "imap", "address": l.user, "senderName": "MailCraft Live Test",
               "incoming": server(&d["imap"]), "outgoing": server(&d["smtp"]), "password": l.pass}),
    ));
    let r = run(&mut s, "accounts.test", json!({"account": a.0, "wait": true}));
    assert_eq!(r["ok"], true, "{r}");

    // Receive: folders map onto the special-use mailboxes, mail arrives.
    let first = format!("{t} first");
    srv.seed("inbox", &first, "Hello over IMAP.", None);
    sync(&mut s);
    for r in [FolderRole::SentItems, FolderRole::DeletedItems, FolderRole::Drafts, FolderRole::JunkEmail] {
        let f = role(&s, a, r);
        assert!(s.mailbox.folder(f).unwrap().remote_name.is_some(), "{r:?} is mapped to a server mailbox");
    }
    let inbox = role(&s, a, FolderRole::Inbox);
    let first_id = find(&s, inbox, &first).expect("the seeded message arrived");
    assert!(!s.mailbox.mail.get(first_id).unwrap().read);
    assert!(s.body_text(s.mailbox.mail.get(first_id).unwrap()).contains("Hello over IMAP"));

    // Flags go up.
    select(&mut s, first_id);
    run(&mut s, "mail.markRead", json!({"ids": [first_id.0]}));
    select(&mut s, first_id);
    run(&mut s, "mail.flag", json!({"ids": [first_id.0]}));
    settle(&mut s);
    eventually("flags on the server", || {
        let k = srv.keywords(&first);
        k.contains(&"$seen".to_owned()) && k.contains(&"$flagged".to_owned())
    });

    // A new folder reaches the server; moving into it moves the server copy.
    s.selection.folder = inbox;
    let name = format!("{t}-folder");
    let folder = FolderId(run(&mut s, "folder.new", json!({"name": name}))["folder"].as_u64().unwrap());
    settle(&mut s);
    eventually("the new mailbox", || srv.mailbox_named(&name).is_some());
    let box_id = srv.mailbox_named(&name).unwrap();
    select(&mut s, first_id);
    run(&mut s, "mail.move", json!({"ids": [first_id.0], "folder": folder.0}));
    settle(&mut s);
    eventually("the move", || srv.subjects_in(&t, &box_id).contains(&first));
    assert!(!srv.subjects_in(&t, &srv.mailbox_id("inbox")).contains(&first));

    // Delete goes to the server's Trash; permanent delete removes it.
    let first_id = find(&s, folder, &first).expect("moved here");
    select(&mut s, first_id);
    run(&mut s, "mail.delete", json!({"ids": [first_id.0]}));
    settle(&mut s);
    let trash = srv.mailbox_id("trash");
    eventually("the deleted message in Trash", || srv.subjects_in(&t, &trash).contains(&first));
    let in_trash = find(&s, role(&s, a, FolderRole::DeletedItems), &first).expect("in Deleted Items");
    select(&mut s, in_trash);
    run(&mut s, "mail.permanentDelete", json!({"ids": [in_trash.0]}));
    settle(&mut s);
    eventually("the permanent delete", || srv.emails(&first).is_empty());

    // The folder goes too (into Deleted Items, then for good).
    delete_folder(&mut s, folder);
    assert!(srv.mailbox_named(&name).is_none(), "the folder is gone from the server");

    // IDLE push: mail arrives without a send/receive.
    poll_until(&mut s, "IDLE", |s| s.push_state(a).0 == PushMode::Idle);
    let pushed = format!("{t} pushed");
    srv.seed("inbox", &pushed, "Arrived by itself.", None);
    poll_until(&mut s, "the pushed message", |s| find(s, inbox, &pushed).is_some());

    // SMTP (implicit TLS on 465): send to ourselves; the copy lands in Sent and the message in the Inbox.
    let sent_subject = format!("{t} sent over SMTP");
    send_to_self(&mut s, &l, &sent_subject);
    sync(&mut s);
    settle(&mut s);
    assert_eq!(s.mailbox.total_count(role(&s, a, FolderRole::Outbox)), 0, "Outbox emptied");
    let sent_box = srv.mailbox_id("sent");
    let inbox_box = srv.mailbox_id("inbox");
    eventually("delivery to ourselves", || srv.subjects_in(&t, &inbox_box).contains(&sent_subject));
    eventually("the copy in Sent", || srv.subjects_in(&t, &sent_box).contains(&sent_subject));
    sync(&mut s);
    let sent_here = role(&s, a, FolderRole::SentItems);
    let copies =
        s.mailbox.mail.items().filter(|m| m.folder == sent_here && m.subject == sent_subject).count();
    assert_eq!(copies, 1, "one copy in Sent Items");
    poll_until(&mut s, "the message sent to ourselves", |s| find(s, inbox, &sent_subject).is_some());

    // Quota and shared folders: reported when the server has them.
    let q = mailcraft_engine::sync::account_quota(&s, a);
    eprintln!("IMAP quota: {:?}", q.as_ref().map(|q| q.text()));
    let shared = run(&mut s, "accounts.sharedMailboxes", json!({"account": a.0}));
    eprintln!("shared mailboxes: {}", shared["mailboxes"]);
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

/// Archive on a server that has no archive mailbox yet: the message stays on the server (IMAP and JMAP).
#[test]
#[ignore = "live server: set MC_LIVE_STALWART_* (docs/live-tests.md)"]
fn live_archive_keeps_mail_on_the_server() {
    let l = live_or_skip!();
    let srv = Server::connect(&l);
    let t = tag("arch");
    // Declared first, so it runs after the tag's clean-up: an archive mailbox this test made goes again.
    let archive_guard = RemoveArchive { srv: &srv, had: srv.archive_mailbox().is_some() };
    let _guard = Cleanup { srv: &srv, tag: t.clone() };
    for kind in ["imap", "jmap"] {
        let subject = format!("{t} {kind} archived");
        srv.seed("inbox", &subject, "Keep me.", None);
        let mut s = session();
        let a =
            if kind == "imap" { add_imap(&mut s, &l, json!({})) } else { add_jmap(&mut s, &l, json!({})) };
        sync(&mut s);
        let id = find(&s, role(&s, a, FolderRole::Inbox), &subject).expect("received");
        select(&mut s, id);
        run(&mut s, "mail.archive", json!({"ids": [id.0]}));
        settle(&mut s);
        sync(&mut s);
        let archive = role(&s, a, FolderRole::Archive);
        assert_eq!(s.mailbox.mail.get(id).unwrap().folder, archive);
        let there = srv.emails(&subject);
        assert_eq!(there.len(), 1, "{kind}: the archived message is still on the server: {there:?}");
        let mailbox = srv.archive_mailbox().expect("an archive mailbox on the server");
        assert_eq!(there[0].2, std::slice::from_ref(&mailbox.0), "{kind}: in the archive mailbox");
        assert_eq!(mailbox.1.as_deref(), Some("archive"), "{kind}: made as the archive (special use / role)");
        assert!(s.mailbox.folder(archive).unwrap().remote_name.is_some());
        // The next kind starts from a server without one again.
        srv.cleanup(&t);
        if !archive_guard.had {
            srv.remove_archive();
        }
    }
}

impl Server {
    /// (id, role) of the mailbox named "Archive" or with the archive role.
    fn archive_mailbox(&self) -> Option<(String, Option<String>)> {
        self.mailboxes()
            .iter()
            .find(|m| {
                m["role"] == "archive"
                    || m["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case("archive"))
            })
            .map(|m| (m["id"].as_str().unwrap_or_default().to_owned(), m["role"].as_str().map(str::to_owned)))
    }

    fn remove_archive(&self) {
        if let Some((id, _)) = self.archive_mailbox() {
            let _ = self.jmap.call_one("Mailbox/set", json!({"accountId": self.account(), "destroy": [id]}));
        }
    }
}

/// Removes an archive mailbox a test made (`had`: there was one before the test).
struct RemoveArchive<'a> {
    srv: &'a Server,
    had: bool,
}

impl Drop for RemoveArchive<'_> {
    fn drop(&mut self) {
        if !self.had {
            self.srv.remove_archive();
        }
    }
}

/// Search › Include Older Results finds mail older than the download window on the server (IMAP and JMAP).
#[test]
#[ignore = "live server: set MC_LIVE_STALWART_* (docs/live-tests.md)"]
fn live_include_older_results() {
    let l = live_or_skip!();
    let srv = Server::connect(&l);
    let t = tag("older");
    let _guard = Cleanup { srv: &srv, tag: t.clone() };
    let now = DateTime::now_utc().unix;
    let old = format!("{t} archived budget");
    srv.seed("inbox", &old, "The archived figures.", Some(now - 200 * 86_400));
    let fresh = format!("{t} fresh");
    srv.seed("inbox", &fresh, "From today.", None);

    for kind in ["imap", "jmap"] {
        let mut s = session();
        let a = if kind == "imap" {
            add_imap(&mut s, &l, json!({"downloadMonths": 3}))
        } else {
            add_jmap(&mut s, &l, json!({"downloadMonths": 3}))
        };
        sync(&mut s);
        let inbox = role(&s, a, FolderRole::Inbox);
        assert!(find(&s, inbox, &fresh).is_some(), "{kind}: fresh mail downloaded");
        assert!(find(&s, inbox, &old).is_none(), "{kind}: old mail stays on the server");
        s.selection.folder = inbox;
        run(&mut s, "search.scope", json!({"scope": "currentFolder"}));
        run(&mut s, "search.query", json!({"text": t, "commit": true}));
        let r = run(&mut s, "search.includeOlderResults", json!({"on": true}));
        assert_eq!(r["started"], 1, "{kind}: {r}");
        assert!(s.wait_for_sync(WAIT), "{kind}: server search timed out");
        no_errors(&s);
        let id = find(&s, inbox, &old).unwrap_or_else(|| panic!("{kind}: the old match was added"));
        assert!(mailcraft_engine::search_index::results(&s).contains(&id), "{kind}");
        run(&mut s, "search.includeOlderResults", json!({"on": false}));
    }
}

/// POP3 (implicit TLS on 995): receive, leave a copy on the server.
#[test]
#[ignore = "live server: set MC_LIVE_STALWART_* (docs/live-tests.md)"]
fn live_pop3_leaves_mail_on_the_server() {
    let l = live_or_skip!();
    let srv = Server::connect(&l);
    let t = tag("pop");
    let _guard = Cleanup { srv: &srv, tag: t.clone() };
    let subject = format!("{t} over POP3");
    srv.seed("inbox", &subject, "Hello over POP3.", None);
    let mut s = session();
    let a = id_of(&run(
        &mut s,
        "accounts.add",
        json!({"kind": "pop3", "address": l.user,
               "incoming": {"host": l.host, "port": 995, "security": "tls"},
               "outgoing": {"host": l.host, "port": 465, "security": "tls"},
               "password": l.pass}),
    ));
    let r = run(&mut s, "accounts.test", json!({"account": a.0, "wait": true}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(run(&mut s, "accounts.options", json!({"account": a.0}))["leaveOnServer"], true);
    sync(&mut s);
    let inbox = role(&s, a, FolderRole::Inbox);
    let id = find(&s, inbox, &subject).expect("POP3 message received");
    assert!(s.body_text(s.mailbox.mail.get(id).unwrap()).contains("Hello over POP3"));
    assert_eq!(srv.emails(&subject).len(), 1, "the server keeps its copy");
    // No duplicates on the next run.
    sync(&mut s);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == subject).count(), 1);
}

/// A JMAP account: mail, flags, move, delete, push over the event source, sending with EmailSubmission,
/// quota.
#[test]
#[ignore = "live server: set MC_LIVE_STALWART_* (docs/live-tests.md)"]
fn live_jmap_mail_push_and_submission() {
    let l = live_or_skip!();
    let srv = Server::connect(&l);
    let t = tag("jmap");
    let _guard = Cleanup { srv: &srv, tag: t.clone() };
    let mut s = session();
    let a = add_jmap(&mut s, &l, json!({}));
    let r = run(&mut s, "accounts.test", json!({"account": a.0, "wait": true}));
    assert_eq!(r["ok"], true, "{r}");
    let first = format!("{t} first");
    srv.seed("inbox", &first, "Hello over JMAP.", None);
    sync(&mut s);
    let inbox = role(&s, a, FolderRole::Inbox);
    let id = find(&s, inbox, &first).expect("the seeded message arrived");
    for r in [FolderRole::SentItems, FolderRole::DeletedItems, FolderRole::Drafts, FolderRole::JunkEmail] {
        assert!(s.mailbox.folder(role(&s, a, r)).unwrap().remote_name.is_some(), "{r:?} mapped");
    }

    select(&mut s, id);
    run(&mut s, "mail.markRead", json!({"ids": [id.0]}));
    select(&mut s, id);
    run(&mut s, "mail.flag", json!({"ids": [id.0]}));
    settle(&mut s);
    eventually("keywords on the server", || {
        let k = srv.keywords(&first);
        k.contains(&"$seen".to_owned()) && k.contains(&"$flagged".to_owned())
    });

    // New folder, move into it, delete, permanent delete.
    s.selection.folder = inbox;
    let name = format!("{t}-folder");
    let folder = FolderId(run(&mut s, "folder.new", json!({"name": name}))["folder"].as_u64().unwrap());
    settle(&mut s);
    sync(&mut s);
    eventually("the new mailbox", || srv.mailbox_named(&name).is_some());
    let box_id = srv.mailbox_named(&name).unwrap();
    select(&mut s, id);
    run(&mut s, "mail.move", json!({"ids": [id.0], "folder": folder.0}));
    settle(&mut s);
    eventually("the move", || srv.subjects_in(&t, &box_id).contains(&first));
    let id = find(&s, folder, &first).expect("moved here");
    select(&mut s, id);
    run(&mut s, "mail.delete", json!({"ids": [id.0]}));
    settle(&mut s);
    let trash = srv.mailbox_id("trash");
    eventually("Trash on the server", || srv.subjects_in(&t, &trash).contains(&first));
    let id = find(&s, role(&s, a, FolderRole::DeletedItems), &first).expect("in Deleted Items");
    select(&mut s, id);
    run(&mut s, "mail.permanentDelete", json!({"ids": [id.0]}));
    settle(&mut s);
    eventually("the permanent delete", || srv.emails(&first).is_empty());
    delete_folder(&mut s, folder);
    assert!(srv.mailbox_named(&name).is_none(), "the folder is gone from the server");

    // Push.
    poll_until(&mut s, "JMAP push", |s| s.push_state(a).0 == PushMode::Idle);
    let pushed = format!("{t} pushed");
    srv.seed("inbox", &pushed, "Arrived by itself.", None);
    poll_until(&mut s, "the pushed message", |s| find(s, inbox, &pushed).is_some());

    // Sending: EmailSubmission/set to ourselves.
    let sent_subject = format!("{t} sent over JMAP");
    send_to_self(&mut s, &l, &sent_subject);
    sync(&mut s);
    settle(&mut s);
    assert_eq!(s.mailbox.total_count(role(&s, a, FolderRole::Outbox)), 0, "Outbox emptied");
    let sent_box = srv.mailbox_id("sent");
    let inbox_box = srv.mailbox_id("inbox");
    eventually("delivery to ourselves", || srv.subjects_in(&t, &inbox_box).contains(&sent_subject));
    eventually("the copy in Sent", || srv.subjects_in(&t, &sent_box).contains(&sent_subject));
    sync(&mut s);
    let sent_here = role(&s, a, FolderRole::SentItems);
    poll_until(&mut s, "the Sent copy here", |s| find(s, sent_here, &sent_subject).is_some());
    assert_eq!(
        s.mailbox.mail.items().filter(|m| m.folder == sent_here && m.subject == sent_subject).count(),
        1
    );
    poll_until(&mut s, "the message sent to ourselves", |s| find(s, inbox, &sent_subject).is_some());

    let q = mailcraft_engine::sync::account_quota(&s, a);
    eprintln!("JMAP quota: {:?}", q.as_ref().map(|q| q.text()));
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
    let _ = find_any(&s, &first);
}

fn vcard(uid: &str, first: &str, last: &str, email: &str) -> String {
    format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{first} {last}\r\nN:{last};{first};;;\r\n\
         EMAIL;TYPE=INTERNET:{email}\r\nEND:VCARD\r\n"
    )
}

fn series(uid: &str, subject: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Live test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
         DTSTAMP:20261001T000000Z\r\nDTSTART:20261112T140000Z\r\nDTEND:20261112T150000Z\r\n\
         RRULE:FREQ=WEEKLY;COUNT=6\r\nSUMMARY:{subject}\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
         DTSTAMP:20261001T000000Z\r\nRECURRENCE-ID:20261119T140000Z\r\nDTSTART:20261119T160000Z\r\n\
         DTEND:20261119T170000Z\r\nSUMMARY:{subject} (moved)\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn todo(uid: &str, subject: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Live test//EN\r\nBEGIN:VTODO\r\nUID:{uid}\r\n\
         DTSTAMP:20261001T000000Z\r\nSUMMARY:{subject}\r\nSTATUS:NEEDS-ACTION\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
    )
}

fn at(y: i32, mo: u8, d: u8, h: u8) -> i64 {
    DateTime::from_civil(y, mo, d, h, 0, 0, 0).unwrap().unix
}

/// CardDAV and CalDAV for an IMAP account: contacts, events (a recurring series with an exception) and
/// tasks, both ways.
#[test]
#[ignore = "live server: set MC_LIVE_STALWART_* (docs/live-tests.md)"]
fn live_carddav_and_caldav() {
    let l = live_or_skip!();
    let srv = Server::connect(&l);
    let t = tag("dav");
    let _guard = Cleanup { srv: &srv, tag: t.clone() };
    let mut s = session();
    let a = add_imap(&mut s, &l, json!({"dav": {"url": format!("https://{}/", l.host)}}));
    let book = srv.dav_collection(Service::Contacts, false);
    let cal = srv.dav_collection(Service::Calendars, true);
    let tasks_col = srv.dav_collection(Service::Calendars, false);

    // Server side first: a contact, a recurring series with an exception and a task.
    let pat_uid = format!("{t}-pat");
    let pat_url = srv.dav_put(
        &book,
        &format!("{pat_uid}.vcf"),
        &vcard(&pat_uid, "Pat", &t, "pat@example.net"),
        "text/vcard",
    );
    let series_uid = format!("{t}-series");
    let series_url = srv.dav_put(
        &cal,
        &format!("{series_uid}.ics"),
        &series(&series_uid, &format!("{t} weekly")),
        "text/calendar",
    );
    let todo_uid = format!("{t}-todo");
    let todo_url = srv.dav_put(
        &tasks_col,
        &format!("{todo_uid}.ics"),
        &todo(&todo_uid, &format!("{t} order toner")),
        "text/calendar",
    );

    // Local side: a contact, an event and a task.
    let contacts = role(&s, a, FolderRole::Contacts);
    let r = run(
        &mut s,
        "people.new",
        json!({"firstName": "Robin", "lastName": t, "email": "robin@example.org", "folder": contacts.0}),
    );
    let robin = ItemId(r["id"].as_u64().unwrap());
    let calendar = role(&s, a, FolderRole::Calendar);
    assert_eq!(update_folder(&mut s, calendar)["server"], "dav");
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": calendar.0, "subject": format!("{t} dentist"), "start": "2026-11-20T09:00", "durationMin": 60}),
    );
    let dentist = ItemId(r["id"].as_u64().unwrap());
    let tasks = role(&s, a, FolderRole::Tasks);
    let r = run(&mut s, "tasks.new", json!({"subject": format!("{t} book the room"), "folder": tasks.0}));
    let book_task = ItemId(r["id"].as_u64().unwrap());

    sync(&mut s);

    // Down: the server's items are here.
    let pat = s
        .mailbox
        .contacts
        .values()
        .find(|c| c.first_name == "Pat" && c.last_name == t)
        .expect("contact down");
    assert_eq!(pat.emails, ["pat@example.net"]);
    let pat_id = pat.id;
    let here: Vec<_> =
        s.mailbox.appointments.values().filter(|x| x.folder == calendar && x.subject.contains(&t)).collect();
    let series_here = here.iter().find(|x| x.recurrence.is_some()).expect("the series arrived");
    let moved = here.iter().find(|x| x.recurrence_id.is_some()).expect("its exception arrived");
    assert_eq!(moved.start.unix, at(2026, 11, 19, 16));
    let series_id = series_here.id;
    assert!(s.mailbox.tasks.values().any(|x| x.subject == format!("{t} order toner")), "task down");

    // Up: the local items are on the server.
    eventually("the contact up", || srv.dav_items(&book, "Robin").iter().any(|i| i.2.contains(&t)));
    assert!(s.mailbox.contacts[&robin].remote.is_some());
    eventually("the event up", || !srv.dav_items(&cal, &format!("{t} dentist")).is_empty());
    assert!(s.mailbox.appointments[&dentist].remote.is_some());
    eventually("the task up", || !srv.dav_items(&tasks_col, &format!("{t} book the room")).is_empty());
    assert!(s.mailbox.tasks[&book_task].remote.is_some());

    // Edit here: goes up. Edit there: comes down.
    run(&mut s, "people.update", json!({"id": robin.0, "jobTitle": "Engineer"}));
    run(&mut s, "calendar.update", json!({"id": series_id.0, "location": "Room 2"}));
    run(
        &mut s,
        "calendar.editOccurrence",
        json!({"id": series_id.0, "occurrence": at(2026, 11, 26, 14), "subject": format!("{t} weekly (short)")}),
    );
    run(&mut s, "tasks.update", json!({"id": book_task.0, "complete": true}));
    srv.dav_put(
        &book,
        &format!("{pat_uid}.vcf"),
        &vcard(&pat_uid, "Pat", &t, "pat@example.com"),
        "text/vcard",
    );
    let _ = &pat_url;
    sync(&mut s);
    eventually("the contact edit up", || srv.dav_items(&book, "Engineer").iter().any(|i| i.2.contains(&t)));
    let data = srv.dav_items(&cal, &series_uid).into_iter().map(|i| i.2).next().unwrap_or_default();
    assert!(data.contains("Room 2"), "{data}");
    assert_eq!(data.matches("RECURRENCE-ID").count(), 2, "{data}");
    let data = srv
        .dav_items(&tasks_col, &format!("{t} book the room"))
        .into_iter()
        .map(|i| i.2)
        .next()
        .unwrap_or_default();
    assert!(data.contains("COMPLETED"), "{data}");
    assert_eq!(s.mailbox.contacts[&pat_id].emails, ["pat@example.com"], "the server's edit came down");

    // Delete here: gone there. Delete there: gone here.
    run(&mut s, "people.delete", json!({"ids": [robin.0]}));
    run(&mut s, "calendar.delete", json!({"id": dentist.0}));
    run(&mut s, "tasks.delete", json!({"ids": [book_task.0]}));
    let _ = srv.dav.delete(&series_url, None);
    let _ = srv.dav.delete(&todo_url, None);
    sync(&mut s);
    eventually("the contact deleted there", || {
        srv.dav_items(&book, "Robin").iter().all(|i| !i.2.contains(&t))
    });
    assert!(srv.dav_items(&cal, &format!("{t} dentist")).is_empty());
    assert!(srv.dav_items(&tasks_col, &format!("{t} book the room")).is_empty(), "the task deleted there");
    assert!(!s.mailbox.appointments.contains_key(&series_id), "the series deleted there is gone here");
    assert!(
        !s.mailbox.tasks.values().any(|x| x.subject == format!("{t} order toner")),
        "the task deleted there is gone here"
    );
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

impl Server {
    /// The JMAP objects of `kind` ("ContactCard", "CalendarEvent") whose JSON holds `tag`.
    fn objects(&self, kind: &str, tag: &str) -> Vec<Value> {
        self.call(&format!("{kind}/get"), json!({"ids": null}))["list"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|o| o.to_string().contains(tag))
            .collect()
    }

    /// The default address book or calendar ("AddressBook", "Calendar").
    fn default_collection(&self, kind: &str) -> String {
        let list = self.call(&format!("{kind}/get"), json!({"ids": null}))["list"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        list.iter()
            .find(|c| c["isDefault"] == true)
            .or_else(|| list.first())
            .and_then(|c| c["id"].as_str())
            .unwrap_or_else(|| panic!("no {kind}"))
            .to_owned()
    }

    fn create(&self, kind: &str, obj: Value) -> String {
        let mut args = json!({"create": {"k": obj}});
        if kind == "CalendarEvent" {
            args["sendSchedulingMessages"] = json!(false);
        }
        let r = self.call(&format!("{kind}/set"), args);
        r["created"]["k"]["id"].as_str().unwrap_or_else(|| panic!("{kind}/set: {r}")).to_owned()
    }

    fn update(&self, kind: &str, id: &str, patch: Value) {
        let r = self.call(&format!("{kind}/set"), json!({"update": {id: patch}}));
        assert!(
            r["notUpdated"].is_null() || r["notUpdated"].as_object().is_some_and(|m| m.is_empty()),
            "{r}"
        );
    }

    fn destroy(&self, kind: &str, id: &str) {
        self.call(&format!("{kind}/set"), json!({"destroy": [id]}));
    }
}

/// A JMAP account whose server offers contacts and calendars over JMAP keeps them there (no DAV settings):
/// contacts and events (a recurring series with an exception) both ways.
#[test]
#[ignore = "live server: set MC_LIVE_STALWART_* (docs/live-tests.md)"]
fn live_jmap_contacts_and_calendars() {
    let l = live_or_skip!();
    let srv = Server::connect(&l);
    if !srv.jmap.session().has_capability("urn:ietf:params:jmap:contacts") {
        eprintln!("skipped: the server offers no JMAP contacts");
        return;
    }
    let t = tag("jpim");
    let _guard = Cleanup { srv: &srv, tag: t.clone() };
    let book = srv.default_collection("AddressBook");
    let cal = srv.default_collection("Calendar");
    let pat_uid = format!("{t}-pat");
    let pat = srv.create(
        "ContactCard",
        json!({"@type": "Card", "version": "1.0", "uid": pat_uid, "addressBookIds": {book.clone(): true},
               "name": {"components": [{"kind": "given", "value": "Pat"}, {"kind": "surname", "value": t}]},
               "emails": {"e1": {"address": "pat@example.net"}}}),
    );
    let series_uid = format!("{t}-series");
    let series = srv.create(
        "CalendarEvent",
        json!({"@type": "Event", "uid": series_uid, "calendarIds": {cal.clone(): true},
               "title": format!("{t} weekly"), "start": "2026-11-12T14:00:00", "timeZone": "Etc/UTC",
               "duration": "PT1H",
               "recurrenceRule": {"@type": "RecurrenceRule", "frequency": "weekly", "count": 6},
               "recurrenceOverrides": {"2026-11-19T14:00:00": {"title": format!("{t} weekly (moved)"),
                   "start": "2026-11-19T16:00:00"}}}),
    );

    let mut s = session();
    let a = add_jmap(&mut s, &l, json!({}));
    sync(&mut s);
    settle(&mut s);
    no_errors(&s);
    assert!(s.mailbox.account(a).unwrap().sync.jmap_contacts, "the server's JMAP contacts are known");
    let contacts = role(&s, a, FolderRole::Contacts);
    let calendar = role(&s, a, FolderRole::Calendar);
    assert_eq!(update_folder(&mut s, contacts)["server"], "jmap", "Update Folder talks to the JMAP server");
    let r = run(
        &mut s,
        "people.new",
        json!({"firstName": "Robin", "lastName": t, "email": "robin@example.org", "folder": contacts.0}),
    );
    let robin = ItemId(r["id"].as_u64().unwrap());
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": calendar.0, "subject": format!("{t} dentist"), "start": "2026-11-20T09:00", "durationMin": 60}),
    );
    let dentist = ItemId(r["id"].as_u64().unwrap());
    sync(&mut s);

    // Down.
    let pat_here = s
        .mailbox
        .contacts
        .values()
        .find(|c| c.first_name == "Pat" && c.last_name == t)
        .expect("contact down");
    assert_eq!(pat_here.emails, ["pat@example.net"]);
    let pat_id = pat_here.id;
    let here: Vec<_> =
        s.mailbox.appointments.values().filter(|x| x.folder == calendar && x.subject.contains(&t)).collect();
    let series_here = here.iter().find(|x| x.recurrence.is_some()).expect("the series arrived");
    let moved = here.iter().find(|x| x.recurrence_id.is_some()).expect("its exception arrived");
    assert_eq!(moved.start.unix, at(2026, 11, 19, 16));
    let series_id = series_here.id;

    // Up.
    let up = srv.objects("ContactCard", "robin@example.org");
    assert_eq!(up.len(), 1, "{up:?}");
    assert!(s.mailbox.contacts[&robin].remote.is_some());
    let ev = srv.objects("CalendarEvent", &format!("{t} dentist"));
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert!(s.mailbox.appointments[&dentist].remote.is_some());

    // A second run changes nothing.
    sync(&mut s);
    assert_eq!(srv.objects("ContactCard", &t).len(), 2);
    assert_eq!(srv.objects("CalendarEvent", &t).len(), 2);

    // Edit here: up. Edit there: down.
    run(&mut s, "people.update", json!({"id": robin.0, "jobTitle": "Engineer"}));
    run(&mut s, "calendar.update", json!({"id": series_id.0, "location": "Room 2"}));
    run(
        &mut s,
        "calendar.editOccurrence",
        json!({"id": series_id.0, "occurrence": at(2026, 11, 26, 14), "subject": format!("{t} weekly (short)")}),
    );
    srv.update("ContactCard", &pat, json!({"emails": {"e1": {"address": "pat@example.com"}}}));
    sync(&mut s);
    let up = srv.objects("ContactCard", "robin@example.org");
    assert!(up[0].to_string().contains("Engineer"), "{up:?}");
    let ev = srv.objects("CalendarEvent", &series_uid);
    let ev = &ev[0];
    assert!(ev.to_string().contains("Room 2"), "{ev}");
    let overrides = ev["recurrenceOverrides"].as_object().cloned().unwrap_or_default();
    assert_eq!(overrides.values().filter(|o| o.get("excluded").is_none()).count(), 2, "{ev}");
    assert!(ev.to_string().contains("weekly (short)"), "{ev}");
    assert_eq!(s.mailbox.contacts[&pat_id].emails, ["pat@example.com"], "the server's edit came down");

    // Delete here: gone there. Delete there: gone here.
    run(&mut s, "people.delete", json!({"ids": [robin.0]}));
    run(&mut s, "calendar.delete", json!({"id": dentist.0}));
    srv.destroy("CalendarEvent", &series);
    sync(&mut s);
    assert!(srv.objects("ContactCard", "robin@example.org").is_empty());
    assert!(srv.objects("CalendarEvent", &format!("{t} dentist")).is_empty());
    assert!(!s.mailbox.appointments.contains_key(&series_id), "the series deleted there is gone here");
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}
