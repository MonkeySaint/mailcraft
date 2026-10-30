//! Live tests of Exchange accounts (EWS) against a real server (opt-in; never run in CI). They drive the
//! engine like a user: add the account, receive, write changes back (read, flag, move, delete, folders),
//! send to the test mailbox itself, and synchronise contacts, calendar and tasks both ways; and the existing
//! CardDAV / CalDAV synchronisation against the same server's DAV service.
//!
//! Every test is `#[ignore]` and returns early unless these are set (see `docs/live-tests.md`):
//!
//! - `MC_LIVE_EWS_URL`: the EWS endpoint (`https://mail.example.com/EWS/Exchange.asmx`),
//! - `MC_LIVE_EWS_USER` / `MC_LIVE_EWS_PASS`: a mailbox kept for testing (mail goes only to itself),
//! - `MC_LIVE_EWS_CA` (optional): a PEM file with the server's certificate, for a self-signed server (trusted
//!   through the account's extra root certificate setting, never by turning checks off),
//! - `MC_LIVE_EWS_ADDR` (optional): the server's IP address, when its name does not resolve here,
//! - `MC_LIVE_DAV_URL` (optional): the same server's CardDAV / CalDAV address, for the DAV test.
//!
//! Run: `cargo test -p mailcraft-engine --test live_ews -- --ignored --test-threads=1`. Everything a test
//! creates carries a run tag (`MCLIVE…`) and is removed at the end, also when the test fails.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::{Duration, Instant};

use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, Contact, FolderId, FolderRole, ItemId, Mailbox, Task};
use mailcraft_net::ConnectOptions;
use mailcraft_net::ews::{EwsAuth, EwsClient};
use mailcraft_net::http::{HttpClient, Url};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(90);

#[derive(Clone)]
struct Live {
    url: String,
    user: String,
    pass: String,
    pem: Option<String>,
    dav: Option<String>,
}

fn live() -> Option<Live> {
    let get = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let url = get("MC_LIVE_EWS_URL")?;
    if let Some(addr) = get("MC_LIVE_EWS_ADDR") {
        let host = Url::parse(&url).expect("URL").host;
        mailcraft_net::transport::set_host_address(&host, Some(addr.parse().expect("IP address")));
    }
    let pem = get("MC_LIVE_EWS_CA").map(|p| std::fs::read_to_string(p).expect("MC_LIVE_EWS_CA readable"));
    Some(Live {
        url,
        user: get("MC_LIVE_EWS_USER")?,
        pass: get("MC_LIVE_EWS_PASS")?,
        pem,
        dav: get("MC_LIVE_DAV_URL"),
    })
}

macro_rules! live_or_skip {
    () => {
        match live() {
            Some(l) => l,
            None => {
                eprintln!("skipped: set MC_LIVE_EWS_URL, _USER and _PASS to run the live tests");
                return;
            }
        }
    };
}

fn tag(what: &str) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("MCLIVE{}{what}", now.as_millis() % 10_000_000_000)
}

/// Direct access to the server, to check and to clean up.
fn server(l: &Live) -> EwsClient {
    let mut o = ConnectOptions { io_timeout: Duration::from_secs(60), ..ConnectOptions::default() };
    if let Some(pem) = &l.pem {
        o.tls.extra_roots_der = mailcraft_engine::accounts::pem_certificates(pem).expect("PEM");
    }
    let auth = EwsAuth::Auto { user: l.user.clone(), password: l.pass.clone() };
    EwsClient::connect(&l.url, auth, HttpClient::new(o), Some(&l.user)).expect("sign-in")
}

/// Removes everything carrying `tag` (mail in the usual folders, folders, contacts, events, tasks).
struct Cleanup {
    live: Live,
    tag: String,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let c = server(&self.live);
        for f in ["inbox", "sentitems", "deleteditems", "drafts", "contacts", "calendar", "tasks"] {
            let items =
                c.find_items(f, "<t:BaseShape>AllProperties</t:BaseShape>", 50_000).unwrap_or_default();
            for it in items {
                let text = format!("{:?}", it);
                if text.contains(&self.tag)
                    && let Some(id) = it.child("ItemId").and_then(|i| i.attr("Id"))
                {
                    let _ = c.delete_item(id);
                }
            }
        }
        for f in c.folders().unwrap_or_default() {
            if f.name.contains(&self.tag) {
                let _ = c.delete_folder(&f.id);
            }
        }
    }
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
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

fn session() -> Session {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    s
}

fn add(s: &mut Session, l: &Live) -> AccountId {
    let mut incoming = json!({"host": l.url});
    if let Some(pem) = &l.pem {
        incoming["extraRootPem"] = json!(pem);
    }
    let r = run(
        s,
        "accounts.add",
        json!({"kind": "exchange", "address": l.user, "senderName": "MailCraft Live Test", "incoming": incoming, "password": l.pass}),
    );
    AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
}

fn find(s: &Session, folder: FolderId, subject: &str) -> Option<ItemId> {
    s.mailbox.mail.items().find(|m| m.folder == folder && m.subject == subject).map(|m| m.id)
}

fn select(s: &mut Session, id: ItemId) {
    s.selection.folder = s.mailbox.mail.get(id).unwrap().folder;
    s.selection.items = vec![id];
    s.selection.focus = Some(id);
}

fn eventually(s: &mut Session, what: &str, mut done: impl FnMut(&mut Session) -> bool) {
    let end = Instant::now() + WAIT;
    loop {
        sync(s);
        if done(s) {
            return;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The server's items of `folder` whose properties mention `text`.
fn server_items(c: &EwsClient, folder: &str, text: &str) -> Vec<mailcraft_net::autoconfig::Element> {
    c.find_items(folder, "<t:BaseShape>AllProperties</t:BaseShape>", 50_000)
        .unwrap()
        .into_iter()
        .filter(|e| format!("{e:?}").contains(text))
        .collect()
}

#[test]
#[ignore = "live server: set MC_LIVE_EWS_* (docs/live-tests.md)"]
fn live_ews_mail() {
    let l = live_or_skip!();
    let t = tag("Mail");
    let _cleanup = Cleanup { live: l.clone(), tag: t.clone() };
    let c = server(&l);
    let mut s = session();
    let a = add(&mut s, &l);
    let test = run(&mut s, "accounts.test", json!({"account": a.0, "wait": true}));
    assert_eq!(test["ok"], true, "{test}");
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    assert!(s.mailbox.folder(inbox).unwrap().remote_name.is_some());
    let in_inbox = s.mailbox.mail.items().filter(|m| m.folder == inbox).count();
    eprintln!("{in_inbox} messages in the Inbox");

    // Send to the test mailbox itself; it arrives, and the sent copy is filed once.
    s.selection.folder = inbox;
    let cid = run(&mut s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.setTo", json!({"compose": cid, "value": l.user}));
    run(&mut s, "compose.setSubject", json!({"compose": cid, "value": t}));
    run(&mut s, "compose.insertText", json!({"compose": cid, "text": "Sent by the MailCraft live tests."}));
    run(&mut s, "compose.send", json!({"compose": cid}));
    settle(&mut s);
    let tt = t.clone();
    eventually(&mut s, "the message to arrive", move |s| find(s, inbox, &tt).is_some());
    let got = find(&s, inbox, &t).unwrap();
    let sent = s.mailbox.role_folder(a, FolderRole::SentItems).unwrap();
    assert_eq!(
        s.mailbox.mail.items().filter(|m| m.folder == sent && m.subject == t).count(),
        1,
        "one sent copy"
    );

    // Read, flag and categories go to the server.
    select(&mut s, got);
    run(&mut s, "mail.markRead", json!({"ids": [got.0]}));
    select(&mut s, got);
    run(&mut s, "mail.flag", json!({"ids": [got.0]}));
    settle(&mut s);
    let remote = s.mailbox.mail.get(got).unwrap().remote_id.clone().unwrap();
    let info = c.get_mime(std::slice::from_ref(&remote)).unwrap();
    assert_eq!((info[0].read, info[0].flag_status), (Some(true), Some(2)), "{:?}", info[0]);

    // A folder made here, the message moved into it, then deleted for good; the folder removed.
    s.selection.folder = inbox;
    let folder_name = format!("{t}Folder");
    let f = FolderId(run(&mut s, "folder.new", json!({"name": folder_name}))["folder"].as_u64().unwrap());
    settle(&mut s);
    sync(&mut s);
    assert!(c.folders().unwrap().iter().any(|x| x.name == folder_name), "the folder was made on the server");
    select(&mut s, got);
    run(&mut s, "mail.move", json!({"ids": [got.0], "folder": f.0}));
    settle(&mut s);
    sync(&mut s);
    assert_eq!(s.mailbox.mail.get(got).unwrap().folder, f);
    assert_eq!(
        s.mailbox.mail.items().filter(|m| m.subject == t && m.folder != sent).count(),
        1,
        "no duplicate after the move"
    );
    let fid = s.mailbox.folder(f).unwrap().remote_name.clone().unwrap();
    assert_eq!(c.folder_total(&fid).unwrap(), Some(1));
    select(&mut s, got);
    run(&mut s, "mail.permanentDelete", json!({"ids": [got.0]}));
    settle(&mut s);
    assert_eq!(c.folder_total(&fid).unwrap(), Some(0));
    for _ in 0..2 {
        if s.mailbox.folders.contains_key(&f) {
            s.selection.folder = f;
            run(&mut s, "folder.delete", json!({"folder": f.0, "confirm": true}));
            settle(&mut s);
            sync(&mut s);
        }
    }
    assert!(
        !c.folders().unwrap().iter().any(|x| x.name == folder_name),
        "the folder was removed on the server"
    );
}

#[test]
#[ignore = "live server: set MC_LIVE_EWS_* (docs/live-tests.md)"]
fn live_ews_contacts_calendar_tasks() {
    let l = live_or_skip!();
    let t = tag("Pim");
    let _cleanup = Cleanup { live: l.clone(), tag: t.clone() };
    let c = server(&l);
    let mut s = session();
    let a = add(&mut s, &l);
    let contacts = s.mailbox.role_folder(a, FolderRole::Contacts).unwrap();
    let calendar = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    let tasks = s.mailbox.role_folder(a, FolderRole::Tasks).unwrap();
    // From the server: a contact made there.
    c.create_item("contacts", &format!("<t:Contact><t:GivenName>{t}Server</t:GivenName><t:Surname>Example</t:Surname><t:EmailAddresses><t:Entry Key=\"EmailAddress1\">server@example.net</t:Entry></t:EmailAddresses></t:Contact>"), "").unwrap();
    // From here: a contact, a weekly appointment and a task.
    let cid = ItemId(s.mailbox.alloc_id());
    s.mailbox.contacts.insert(
        cid,
        Contact {
            id: cid,
            folder: contacts,
            first_name: format!("{t}Local"),
            last_name: "Example".into(),
            emails: vec!["local@example.org".into()],
            ..Default::default()
        },
    );
    let tid = ItemId(s.mailbox.alloc_id());
    s.mailbox
        .tasks
        .insert(tid, Task { id: tid, folder: tasks, subject: format!("{t} task"), ..Default::default() });
    run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": calendar.0, "subject": format!("{t} weekly"), "start": "2026-11-03T14:00", "durationMin": 60, "recurrence": "FREQ=WEEKLY;BYDAY=TU;COUNT=4"}),
    );
    sync(&mut s);
    assert!(
        s.mailbox.contacts.values().any(|x| x.first_name == format!("{t}Server")),
        "the server's contact arrived"
    );
    assert_eq!(server_items(&c, "contacts", &format!("{t}Local")).len(), 1, "the local contact went up");
    assert_eq!(server_items(&c, "tasks", &format!("{t} task")).len(), 1, "the task went up");
    let evs = server_items(&c, "calendar", &format!("{t} weekly"));
    assert_eq!(evs.len(), 1, "the appointment went up");
    eprintln!("recurrence on the server: {:?}", evs[0].child("Recurrence"));
    // No changes: nothing is written again.
    sync(&mut s);
    assert_eq!(server_items(&c, "contacts", &format!("{t}Local")).len(), 1);
    // Edited here: the change goes up.
    s.mailbox.contacts.get_mut(&cid).unwrap().job_title = "Tester".into();
    sync(&mut s);
    let up = server_items(&c, "contacts", &format!("{t}Local"));
    assert_eq!(up[0].child_text("JobTitle"), Some("Tester"));
    // Deleted here: deleted there.
    s.mailbox.tasks.remove(&tid);
    sync(&mut s);
    assert!(server_items(&c, "tasks", &format!("{t} task")).is_empty());
}

#[test]
#[ignore = "live server: set MC_LIVE_EWS_* and MC_LIVE_DAV_URL (docs/live-tests.md)"]
fn live_dav_against_the_same_server() {
    let l = live_or_skip!();
    let Some(dav) = l.dav.clone() else {
        eprintln!("skipped: set MC_LIVE_DAV_URL to run the DAV test");
        return;
    };
    let t = tag("Dav");
    let _cleanup = Cleanup { live: l.clone(), tag: t.clone() };
    let mut s = session();
    // An IMAP-less account whose contacts and calendar go to the DAV service: an Exchange account with a DAV
    // server set keeps them there.
    let mut incoming = json!({"host": l.url});
    let mut dav_settings = json!({"url": dav, "contacts": true, "calendar": true, "tasks": true});
    if let Some(pem) = &l.pem {
        incoming["extraRootPem"] = json!(pem);
        dav_settings["extraRootPem"] = json!(pem);
    }
    let r = run(
        &mut s,
        "accounts.add",
        json!({"kind": "exchange", "address": l.user, "incoming": incoming, "password": l.pass, "dav": dav_settings}),
    );
    let a = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    let contacts = s.mailbox.role_folder(a, FolderRole::Contacts).unwrap();
    let cid = ItemId(s.mailbox.alloc_id());
    s.mailbox.contacts.insert(
        cid,
        Contact {
            id: cid,
            folder: contacts,
            first_name: format!("{t}Dav"),
            last_name: "Example".into(),
            emails: vec!["dav@example.org".into()],
            ..Default::default()
        },
    );
    let calendar = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    let tasks = s.mailbox.role_folder(a, FolderRole::Tasks).unwrap();
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": calendar.0, "subject": format!("{t} dav weekly"), "start": "2026-11-04T10:00", "durationMin": 30, "recurrence": "FREQ=WEEKLY;COUNT=3"}),
    );
    let ev = ItemId(r["id"].as_u64().unwrap());
    let tid = ItemId(s.mailbox.alloc_id());
    s.mailbox
        .tasks
        .insert(tid, Task { id: tid, folder: tasks, subject: format!("{t} dav task"), ..Default::default() });
    sync(&mut s);
    assert!(s.mailbox.contacts[&cid].remote.is_some(), "the contact went up over CardDAV");
    assert!(s.mailbox.appointments[&ev].remote.is_some(), "the series went up over CalDAV");
    assert!(s.mailbox.tasks[&tid].remote.is_some(), "the task went up over CalDAV");
    let c = server(&l);
    assert_eq!(server_items(&c, "contacts", &format!("{t}Dav")).len(), 1, "and the Exchange side sees it");
    let evs = server_items(&c, "calendar", &format!("{t} dav weekly"));
    assert_eq!(evs.len(), 1, "one series on the Exchange side");
    assert!(evs[0].child("Recurrence").is_some(), "with its recurrence");
    assert_eq!(server_items(&c, "tasks", &format!("{t} dav task")).len(), 1);
    // A run without changes: one copy of each here.
    sync(&mut s);
    assert_eq!(s.mailbox.contacts.values().filter(|x| x.first_name == format!("{t}Dav")).count(), 1);
    assert_eq!(s.mailbox.appointments.values().filter(|x| x.subject == format!("{t} dav weekly")).count(), 1);
    assert_eq!(s.mailbox.tasks.values().filter(|x| x.subject == format!("{t} dav task")).count(), 1);
    // Deleted here: deleted there.
    s.mailbox.contacts.remove(&cid);
    s.mailbox.appointments.remove(&ev);
    s.mailbox.tasks.remove(&tid);
    sync(&mut s);
    assert!(server_items(&c, "contacts", &format!("{t}Dav")).is_empty());
    assert!(server_items(&c, "calendar", &format!("{t} dav weekly")).is_empty());
    assert!(server_items(&c, "tasks", &format!("{t} dav task")).is_empty());
}
