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

/// Regression (live run against a JMAP server): folders made, renamed, moved and deleted here never reached a
/// JMAP server, and mail moved into a new folder was destroyed on the server (the folder had no mailbox id).
#[test]
fn jmap_folders_made_here_reach_the_server() {
    let srv = TestServers::start(Config {
        users: vec![(ALICE.into(), ALICE_PW.into())],
        add_trace_headers: false,
        ..Config::default()
    })
    .unwrap();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let r = run(
        &mut s,
        "accounts.add",
        json!({"kind": "jmap", "address": ALICE, "incoming": {"host": srv.jmap_url(), "extraRootPem": srv.cert_pem},
               "password": ALICE_PW}),
    );
    let a = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    srv.seed_message(ALICE, "INBOX", &message("Keep me", "keep"), &[]).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let keep = find(&s, inbox, "Keep me").unwrap();

    // A new folder, a subfolder of it, and a message moved in before any send/receive.
    s.selection.folder = inbox;
    let projects =
        FolderId(run(&mut s, "folder.new", json!({"name": "Projects"}))["folder"].as_u64().unwrap());
    s.selection.folder = projects;
    let sub = FolderId(run(&mut s, "folder.new", json!({"name": "2026"}))["folder"].as_u64().unwrap());
    select(&mut s, keep);
    run(&mut s, "mail.move", json!({"ids": [keep.0], "folder": sub.0}));
    settle(&mut s);
    sync(&mut s);
    let boxes = srv.mailboxes(ALICE).unwrap();
    assert!(boxes.contains(&"INBOX/Projects/2026".to_owned()), "{boxes:?}");
    assert_eq!(subjects(&srv, ALICE, "INBOX/Projects/2026"), ["Keep me"], "moved, not destroyed");
    assert!(s.mailbox.folder(sub).unwrap().remote_name.is_some());
    let named = |s: &Session, n: &str| s.mailbox.folders.values().filter(|f| f.name == n).count();
    assert_eq!(named(&s, "Projects"), 1, "no second folder from the listing");
    assert_eq!(named(&s, "2026"), 1);

    // Rename; move below another folder.
    s.selection.folder = projects;
    run(&mut s, "folder.rename", json!({"name": "Work"}));
    settle(&mut s);
    sync(&mut s);
    let boxes = srv.mailboxes(ALICE).unwrap();
    assert!(boxes.contains(&"INBOX/Work/2026".to_owned()), "{boxes:?}");
    assert_eq!(s.mailbox.folder(projects).unwrap().name, "Work", "the rename stays");
    s.selection.folder = sub;
    run(&mut s, "folder.move", json!({"parent": "Archive"}));
    settle(&mut s);
    sync(&mut s);
    let boxes = srv.mailboxes(ALICE).unwrap();
    assert!(boxes.contains(&"Archive/2026".to_owned()), "{boxes:?}");
    assert_eq!(subjects(&srv, ALICE, "Archive/2026"), ["Keep me"]);
    assert_eq!(s.mailbox.mail.get(keep).unwrap().folder, sub, "the message stays in its folder");

    // Delete: into Deleted Items first, then for good.
    for f in [projects, sub] {
        for _ in 0..2 {
            if s.mailbox.folders.contains_key(&f) {
                s.selection.folder = f;
                run(&mut s, "folder.delete", json!({"folder": f.0, "confirm": true}));
                settle(&mut s);
                sync(&mut s);
            }
        }
    }
    let boxes = srv.mailboxes(ALICE).unwrap();
    assert!(!boxes.iter().any(|b| b.ends_with("Work") || b.ends_with("2026")), "{boxes:?}");
    assert!(!subjects(&srv, ALICE, "INBOX").contains(&"Keep me".to_owned()));
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

fn at(y: i32, mo: u8, d: u8, h: u8) -> i64 {
    mailcraft_engine::model::DateTime::from_civil(y, mo, d, h, 0, 0, 0).unwrap().unix
}

fn objects_with(srv: &TestServers, kind: &str, text: &str) -> Vec<Value> {
    srv.jmap_objects(ALICE, kind).into_iter().filter(|o| o.to_string().contains(text)).collect()
}

/// A JMAP account whose server offers contacts (RFC 9610) and calendars over JMAP keeps its Contacts and
/// Calendar there, without DAV settings: both ways, a recurring series with an exception, deletions.
#[test]
fn jmap_contacts_and_calendars_synchronise_over_jmap() {
    let srv = TestServers::start(Config {
        users: vec![(ALICE.into(), ALICE_PW.into())],
        jmap_pim: true,
        ..Config::default()
    })
    .unwrap();
    let pat = srv.put_jmap_object(
        ALICE,
        "ContactCard",
        json!({"@type": "Card", "uid": "pat-1", "name": {"components": [{"kind": "given", "value": "Pat"},
               {"kind": "surname", "value": "Example"}]}, "emails": {"e": {"address": "pat@example.net"}},
               "x-other:kept": "a property MailCraft does not map"}),
    );
    let series = srv.put_jmap_object(
        ALICE,
        "CalendarEvent",
        json!({"@type": "Event", "uid": "series-1", "title": "Weekly sync", "start": "2026-10-12T14:00:00",
               "timeZone": "Etc/UTC", "duration": "PT1H",
               "recurrenceRule": {"@type": "RecurrenceRule", "frequency": "weekly", "count": 6},
               "recurrenceOverrides": {"2026-10-19T14:00:00": {"title": "Weekly sync (moved)",
                   "start": "2026-10-19T16:00:00"}, "2026-10-26T14:00:00": {"excluded": true}}}),
    );
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let r = run(
        &mut s,
        "accounts.add",
        json!({"kind": "jmap", "address": ALICE, "incoming": {"host": srv.jmap_url(), "extraRootPem": srv.cert_pem},
               "password": ALICE_PW}),
    );
    let a = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    // The first run learns what the server offers and synchronises the contacts and calendar right after.
    sync(&mut s);
    settle(&mut s);
    let errors: Vec<String> = s.sync_status().accounts.iter().filter_map(|x| x.error.clone()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert!(s.mailbox.account(a).unwrap().sync.jmap_contacts);
    let contacts = s.mailbox.role_folder(a, FolderRole::Contacts).unwrap();
    let calendar = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    let r = run(&mut s, "sendReceive.folder", json!({"folder": contacts.0}));
    assert_eq!(r["server"], "jmap", "{r}");
    settle(&mut s);
    let pat_here = s.mailbox.contacts.values().find(|c| c.first_name == "Pat").expect("the server's contact");
    assert_eq!((pat_here.folder, pat_here.emails.clone()), (contacts, vec!["pat@example.net".to_owned()]));
    let pat_id = pat_here.id;
    let here: Vec<_> = s.mailbox.appointments.values().filter(|x| x.folder == calendar).collect();
    assert_eq!(here.len(), 2, "the series and its exception: {here:?}");
    let series_here = here.iter().find(|x| x.recurrence.is_some()).unwrap();
    assert_eq!(series_here.start.unix, at(2026, 10, 12, 14));
    assert!(series_here.recurrence.as_ref().unwrap().exdates.iter().any(|d| d.unix == at(2026, 10, 26, 14)));
    let moved = here.iter().find(|x| x.recurrence_id.is_some()).unwrap();
    assert_eq!((moved.subject.as_str(), moved.start.unix), ("Weekly sync (moved)", at(2026, 10, 19, 16)));
    let series_id = series_here.id;

    // New here: created there, in the address book / calendar.
    let r = run(
        &mut s,
        "people.new",
        json!({"firstName": "Robin", "lastName": "Sample", "email": "robin@example.org", "folder": contacts.0}),
    );
    let robin = ItemId(r["id"].as_u64().unwrap());
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": calendar.0, "subject": "Dentist", "start": "2026-10-14T09:00", "durationMin": 60,
               "importance": "high"}),
    );
    let dentist = ItemId(r["id"].as_u64().unwrap());
    sync(&mut s);
    let up = objects_with(&srv, "ContactCard", "robin@example.org");
    assert_eq!(up.len(), 1, "{up:?}");
    assert_eq!(up[0]["addressBookIds"], json!({"AB1": true}));
    assert!(s.mailbox.contacts[&robin].remote.is_some());
    let ev = objects_with(&srv, "CalendarEvent", "Dentist");
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!((ev[0]["calendarIds"].clone(), ev[0]["priority"].clone()), (json!({"CAL1": true}), json!(1)));
    assert!(s.mailbox.appointments[&dentist].remote.is_some());

    // A run without changes writes nothing.
    let before = (srv.jmap_objects(ALICE, "ContactCard"), srv.jmap_objects(ALICE, "CalendarEvent"));
    sync(&mut s);
    assert_eq!((srv.jmap_objects(ALICE, "ContactCard"), srv.jmap_objects(ALICE, "CalendarEvent")), before);
    assert_eq!(s.mailbox.contacts.values().filter(|c| c.folder == contacts).count(), 2);
    assert_eq!(s.mailbox.appointments.values().filter(|x| x.folder == calendar).count(), 3);

    // Changed here: up (what MailCraft does not map stays). Changed there: down.
    run(&mut s, "people.update", json!({"id": pat_id.0, "jobTitle": "Engineer"}));
    run(&mut s, "calendar.update", json!({"id": series_id.0, "location": "Room 2"}));
    run(
        &mut s,
        "calendar.editOccurrence",
        json!({"id": series_id.0, "occurrence": at(2026, 11, 2, 14), "subject": "Weekly sync (short)"}),
    );
    assert!(srv.update_jmap_object(
        ALICE,
        "ContactCard",
        up[0]["id"].as_str().unwrap(),
        json!({"emails": {"e1": {"address": "robin@example.com"}}})
    ));
    sync(&mut s);
    let pat_there = objects_with(&srv, "ContactCard", "pat-1");
    assert!(pat_there[0].to_string().contains("Engineer"), "{:?}", pat_there[0]);
    assert_eq!(pat_there[0]["x-other:kept"], "a property MailCraft does not map");
    let ev = objects_with(&srv, "CalendarEvent", "series-1");
    let ev = &ev[0];
    assert_eq!(ev["locations"].as_object().unwrap().values().next().unwrap()["name"], "Room 2");
    let ov = ev["recurrenceOverrides"].as_object().unwrap();
    assert_eq!(ov.values().filter(|o| o.get("excluded").is_none()).count(), 2, "{ev}");
    assert_eq!(ov["2026-10-26T14:00:00"], json!({"excluded": true}));
    assert!(ev.to_string().contains("Weekly sync (short)"), "{ev}");
    assert_eq!(s.mailbox.contacts[&robin].emails, ["robin@example.com"], "the server's edit came down");
    assert_eq!(s.mailbox.appointments.values().filter(|x| x.folder == calendar).count(), 4);

    // Deleted here: gone there. Deleted there: gone here.
    run(&mut s, "people.delete", json!({"ids": [robin.0]}));
    run(&mut s, "calendar.delete", json!({"id": dentist.0}));
    assert!(srv.delete_jmap_object(ALICE, "CalendarEvent", &series));
    srv.delete_jmap_object(ALICE, "ContactCard", &pat);
    sync(&mut s);
    assert!(srv.jmap_objects(ALICE, "ContactCard").is_empty());
    assert!(srv.jmap_objects(ALICE, "CalendarEvent").is_empty());
    assert_eq!(s.mailbox.contacts.values().filter(|c| c.folder == contacts).count(), 0);
    assert_eq!(s.mailbox.appointments.values().filter(|x| x.folder == calendar).count(), 0);
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

fn add_alice_jmap(s: &mut Session, srv: &TestServers) -> AccountId {
    let r = run(
        s,
        "accounts.add",
        json!({"kind": "jmap", "address": ALICE, "incoming": {"host": srv.jmap_url(), "extraRootPem": srv.cert_pem},
               "password": ALICE_PW}),
    );
    AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
}

/// Regression (live run): on a JMAP server without an archive mailbox, Archive destroyed the server copy (the
/// local Archive had no mailbox). It is now made there (role `archive`) and the message moved into it; a
/// mailbox already named "Archive" without a role is taken as the archive.
#[test]
fn jmap_archive_on_a_server_without_one_makes_it() {
    let srv = TestServers::start(Config {
        users: vec![(ALICE.into(), ALICE_PW.into())],
        add_trace_headers: false,
        ..Config::default()
    })
    .unwrap();
    srv.delete_mailbox(ALICE, "Archive").unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Keep me", "keep"), &[]).unwrap();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let a = add_alice_jmap(&mut s, &srv);
    sync(&mut s);
    let archive = s.mailbox.role_folder(a, FolderRole::Archive).unwrap();
    assert!(s.mailbox.folder(archive).unwrap().remote_name.is_none());
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let keep = find(&s, inbox, "Keep me").unwrap();
    select(&mut s, keep);
    run(&mut s, "mail.archive", json!({"ids": [keep.0]}));
    settle(&mut s);
    sync(&mut s);
    assert_eq!(subjects(&srv, ALICE, "Archive"), ["Keep me"], "moved into the new archive, not destroyed");
    assert_eq!(srv.special_use(ALICE, "Archive"), Some(mailcraft_testmail::SpecialUse::Archive));
    assert!(s.mailbox.folder(archive).unwrap().remote_name.is_some());
    assert_eq!(s.mailbox.folders.values().filter(|f| f.account == a && f.name == "Archive").count(), 1);

    // A server whose "Archive" has no role: taken as the archive, not shown twice.
    let srv2 =
        TestServers::start(Config { users: vec![(ALICE.into(), ALICE_PW.into())], ..Config::default() })
            .unwrap();
    srv2.delete_mailbox(ALICE, "Archive").unwrap();
    srv2.create_mailbox(ALICE, "Archive", None).unwrap();
    let mut s2 = Session::new(Mailbox::new_local("Local Folders"));
    s2.view.auto_send_receive_minutes = 0;
    let b = add_alice_jmap(&mut s2, &srv2);
    sync(&mut s2);
    let archive2 = s2.mailbox.role_folder(b, FolderRole::Archive).unwrap();
    assert!(s2.mailbox.folder(archive2).unwrap().remote_name.is_some());
    assert_eq!(
        s2.mailbox.folders.values().filter(|f| f.account == b && f.name.starts_with("Archive")).count(),
        1
    );
}

/// A JMAP server without contacts or calendars: those folders stay local, and nothing fails.
#[test]
fn jmap_without_contacts_keeps_them_local() {
    let srv =
        TestServers::start(Config { users: vec![(ALICE.into(), ALICE_PW.into())], ..Config::default() })
            .unwrap();
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let r = run(
        &mut s,
        "accounts.add",
        json!({"kind": "jmap", "address": ALICE, "incoming": {"host": srv.jmap_url(), "extraRootPem": srv.cert_pem},
               "password": ALICE_PW}),
    );
    let a = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    let contacts = s.mailbox.role_folder(a, FolderRole::Contacts).unwrap();
    run(
        &mut s,
        "people.new",
        json!({"firstName": "Robin", "email": "robin@example.org", "folder": contacts.0}),
    );
    sync(&mut s);
    settle(&mut s);
    assert!(!s.mailbox.account(a).unwrap().sync.jmap_contacts);
    assert!(s.sync_status().accounts.iter().all(|x| x.error.is_none()), "{:?}", s.sync_status());
    assert!(s.mailbox.contacts.values().all(|c| c.remote.is_none()));
}

fn alice_only(cfg: Config) -> TestServers {
    TestServers::start(Config {
        users: vec![(ALICE.into(), ALICE_PW.into())],
        add_trace_headers: false,
        ..cfg
    })
    .unwrap()
}

fn jmap_session() -> Session {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    s
}

/// Regression (review): on a profile whose only server account is JMAP, undo put the snapshot back without
/// what synchronisation brought meanwhile, so the next run destroyed a mailbox another client had made and
/// mail that had arrived since the undone command.
#[test]
fn jmap_undo_keeps_what_synchronisation_brought() {
    let srv = alice_only(Config::default());
    srv.seed_message(ALICE, "INBOX", &message("Hello JMAP", "hello"), &[]).unwrap();
    let mut s = jmap_session();
    let a = add_alice_jmap(&mut s, &srv);
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let archive = s.mailbox.role_folder(a, FolderRole::Archive).unwrap();
    let hello = find(&s, inbox, "Hello JMAP").unwrap();
    select(&mut s, hello);
    run(&mut s, "mail.move", json!({"ids": [hello.0], "folder": archive.0}));
    settle(&mut s);
    sync(&mut s);
    assert_eq!(subjects(&srv, ALICE, "Archive"), ["Hello JMAP"]);

    // Another client makes a mailbox and new mail arrives; then the move is undone.
    srv.create_mailbox(ALICE, "Elsewhere", None).unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Later", "later"), &[]).unwrap();
    sync(&mut s);
    assert!(s.mailbox.folders.values().any(|f| f.account == a && f.name == "Elsewhere"));
    assert!(find(&s, inbox, "Later").is_some());
    run(&mut s, "app.undo", json!({}));
    settle(&mut s);
    sync(&mut s);
    assert!(
        srv.mailboxes(ALICE).unwrap().contains(&"Elsewhere".to_owned()),
        "the other client's mailbox stays"
    );
    let mut inbox_now = subjects(&srv, ALICE, "INBOX");
    inbox_now.sort();
    assert_eq!(inbox_now, ["Hello JMAP", "Later"], "the undo moved the message back; the new mail stays");
    assert!(s.mailbox.folders.values().any(|f| f.account == a && f.name == "Elsewhere"));
    assert!(find(&s, inbox, "Later").is_some(), "the new mail is still here");
    assert_eq!(s.mailbox.mail.get(hello).unwrap().folder, inbox);
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

/// Regression (review): undoing "new folder" and "move into it" removed the folder's mailbox; redo then
/// brought the folder back still naming that mailbox, the next listing dropped it with the message, and the
/// run after destroyed the message on the server.
#[test]
fn jmap_undo_and_redo_of_a_move_into_a_new_folder_keep_the_mail() {
    let srv = alice_only(Config::default());
    srv.seed_message(ALICE, "INBOX", &message("Keep me", "keep"), &[]).unwrap();
    let mut s = jmap_session();
    let a = add_alice_jmap(&mut s, &srv);
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let keep = find(&s, inbox, "Keep me").unwrap();
    s.selection.folder = inbox;
    let projects =
        FolderId(run(&mut s, "folder.new", json!({"name": "Projects"}))["folder"].as_u64().unwrap());
    select(&mut s, keep);
    run(&mut s, "mail.move", json!({"ids": [keep.0], "folder": projects.0}));
    settle(&mut s);
    sync(&mut s);
    assert_eq!(subjects(&srv, ALICE, "INBOX/Projects"), ["Keep me"]);

    run(&mut s, "app.undo", json!({}));
    run(&mut s, "app.undo", json!({}));
    settle(&mut s);
    sync(&mut s);
    assert!(!srv.mailboxes(ALICE).unwrap().contains(&"INBOX/Projects".to_owned()));
    assert_eq!(subjects(&srv, ALICE, "INBOX"), ["Keep me"]);

    run(&mut s, "app.redo", json!({}));
    run(&mut s, "app.redo", json!({}));
    settle(&mut s);
    sync(&mut s);
    sync(&mut s);
    assert_eq!(subjects(&srv, ALICE, "INBOX/Projects"), ["Keep me"], "made again, with the message in it");
    assert!(subjects(&srv, ALICE, "INBOX").is_empty());
    assert_eq!(s.mailbox.mail.get(keep).map(|m| m.folder), Some(projects), "the message is still here");
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Keep me").count(), 1);
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

/// Regression (review): an edit here rewrote every mapped property from MailCraft's model, so what it cannot
/// show inside them (a second organization, a photo stored as a blob, a second alert) was removed on the
/// server. Only the properties the edit changed are written now.
#[test]
fn jmap_edits_keep_what_mailcraft_does_not_show() {
    let srv = alice_only(Config { jmap_pim: true, ..Config::default() });
    srv.put_jmap_object(
        ALICE,
        "ContactCard",
        json!({"@type": "Card", "uid": "pat-2", "name": {"components": [{"kind": "given", "value": "Pat"},
               {"kind": "surname", "value": "Example"}]}, "emails": {"e": {"address": "pat@example.net"}},
               "organizations": {"o1": {"name": "Example Corp"}, "o2": {"name": "Example Labs"}},
               "media": {"m1": {"kind": "photo", "blobId": "B123", "mediaType": "image/png"}}}),
    );
    srv.put_jmap_object(
        ALICE,
        "CalendarEvent",
        json!({"@type": "Event", "uid": "review-1", "title": "Review", "start": "2026-10-20T10:00:00",
               "timeZone": "Etc/UTC", "duration": "PT1H",
               "alerts": {"a1": {"@type": "Alert", "trigger": {"@type": "OffsetTrigger", "offset": "-PT15M"}},
                          "a2": {"@type": "Alert", "trigger": {"@type": "OffsetTrigger", "offset": "-PT1H"}}}}),
    );
    let mut s = jmap_session();
    let a = add_alice_jmap(&mut s, &srv);
    sync(&mut s);
    settle(&mut s);
    let contacts = s.mailbox.role_folder(a, FolderRole::Contacts).unwrap();
    let calendar = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    let pat = s.mailbox.contacts.values().find(|c| c.folder == contacts && c.first_name == "Pat").unwrap().id;
    let review =
        s.mailbox.appointments.values().find(|x| x.folder == calendar && x.subject == "Review").unwrap().id;
    run(&mut s, "people.update", json!({"id": pat.0, "jobTitle": "Engineer"}));
    run(&mut s, "calendar.update", json!({"id": review.0, "location": "Room 3"}));
    sync(&mut s);
    let card = objects_with(&srv, "ContactCard", "pat-2").remove(0);
    assert!(card["titles"].to_string().contains("Engineer"), "{card}");
    assert_eq!(card["organizations"]["o1"]["name"], "Example Corp", "{card}");
    assert_eq!(card["organizations"]["o2"]["name"], "Example Labs", "{card}");
    assert_eq!(card["media"]["m1"]["blobId"], "B123", "{card}");
    let ev = objects_with(&srv, "CalendarEvent", "review-1").remove(0);
    assert_eq!(ev["locations"].as_object().unwrap().values().next().unwrap()["name"], "Room 3", "{ev}");
    assert_eq!(ev["alerts"].as_object().map(|m| m.len()), Some(2), "{ev}");
    assert_eq!(ev["alerts"]["a2"]["trigger"]["offset"], "-PT1H", "{ev}");
    assert_eq!(ev["timeZone"], "Etc/UTC");
}

const PAT_VCARD: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:pat-1\r\nFN:Pat Example\r\nN:Example;Pat;;;\r\nEMAIL:pat@example.net\r\nEND:VCARD\r\n";

/// Regression (review): a JMAP account with CardDAV / CalDAV settings was switched to JMAP as soon as its
/// server offered contacts and calendars there, and its contacts went up a second time. It keeps its DAV
/// server.
#[test]
fn jmap_account_with_dav_settings_keeps_using_dav() {
    let srv = alice_only(Config { jmap_pim: true, ..Config::default() });
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", PAT_VCARD).unwrap();
    let mut s = jmap_session();
    let r = run(
        &mut s,
        "accounts.add",
        json!({"kind": "jmap", "address": ALICE, "incoming": {"host": srv.jmap_url(), "extraRootPem": srv.cert_pem},
               "password": ALICE_PW, "dav": {"url": srv.dav_url(), "extraRootPem": srv.cert_pem}}),
    );
    let a = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    let contacts = s.mailbox.role_folder(a, FolderRole::Contacts).unwrap();
    run(
        &mut s,
        "people.new",
        json!({"firstName": "Robin", "email": "robin@example.org", "folder": contacts.0}),
    );
    sync(&mut s);
    settle(&mut s);
    assert!(s.mailbox.account(a).unwrap().sync.jmap_contacts, "the server offers JMAP contacts");
    let r = run(&mut s, "sendReceive.folder", json!({"folder": contacts.0}));
    assert_eq!(r["server"], "dav", "{r}");
    settle(&mut s);
    sync(&mut s);
    settle(&mut s);
    let errors: Vec<String> = s.sync_status().accounts.iter().filter_map(|x| x.error.clone()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(srv.dav_items(ALICE, "contacts").len(), 2, "Pat and Robin, once each");
    assert!(srv.jmap_objects(ALICE, "ContactCard").is_empty(), "nothing went up over JMAP");
    assert_eq!(s.mailbox.contacts.values().filter(|c| c.folder == contacts).count(), 2);
}

/// Regression (review): JMAP mail moved into a folder below a default folder the server has no mailbox for
/// (Archive/2024 without an archive) was destroyed on the server. The archive is made, then the subfolder.
#[test]
fn jmap_mail_moved_below_a_missing_archive_stays_on_the_server() {
    let srv = alice_only(Config::default());
    srv.delete_mailbox(ALICE, "Archive").unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Keep me", "keep"), &[]).unwrap();
    let mut s = jmap_session();
    let a = add_alice_jmap(&mut s, &srv);
    sync(&mut s);
    let archive = s.mailbox.role_folder(a, FolderRole::Archive).unwrap();
    assert!(s.mailbox.folder(archive).unwrap().remote_name.is_none());
    s.selection.folder = archive;
    let sub = FolderId(run(&mut s, "folder.new", json!({"name": "2024"}))["folder"].as_u64().unwrap());
    settle(&mut s);
    sync(&mut s);
    assert!(!srv.mailboxes(ALICE).unwrap().iter().any(|b| b.contains("2024")), "local until it holds mail");
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let keep = find(&s, inbox, "Keep me").unwrap();
    select(&mut s, keep);
    run(&mut s, "mail.move", json!({"ids": [keep.0], "folder": sub.0}));
    settle(&mut s);
    sync(&mut s);
    assert_eq!(subjects(&srv, ALICE, "Archive/2024"), ["Keep me"], "moved, not destroyed");
    assert_eq!(srv.special_use(ALICE, "Archive"), Some(mailcraft_testmail::SpecialUse::Archive));
    sync(&mut s);
    assert_eq!(s.mailbox.mail.get(keep).unwrap().folder, sub);
    assert_eq!(s.mailbox.folders.values().filter(|f| f.account == a && f.name == "2024").count(), 1);
    assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
}

/// Regression (review): a user's role-less "Deleted Items" listed before the server's trash took the
/// Deleted Items folder, so deleted mail went into it instead of the server's trash.
#[test]
fn jmap_name_guess_never_takes_the_place_of_a_server_role() {
    let srv = alice_only(Config::default());
    srv.delete_mailbox(ALICE, "Trash").unwrap();
    srv.create_mailbox(ALICE, "Deleted Items", None).unwrap();
    srv.create_mailbox(ALICE, "Trash", Some(mailcraft_testmail::SpecialUse::Trash)).unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Bin me", "bin"), &[]).unwrap();
    let mut s = jmap_session();
    let a = add_alice_jmap(&mut s, &srv);
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let bin = find(&s, inbox, "Bin me").unwrap();
    select(&mut s, bin);
    run(&mut s, "mail.delete", json!({"ids": [bin.0]}));
    settle(&mut s);
    sync(&mut s);
    assert_eq!(subjects(&srv, ALICE, "Trash"), ["Bin me"], "into the server's trash");
    assert!(subjects(&srv, ALICE, "Deleted Items").is_empty());
}

/// Regression (review): a server that refused the role of a default folder it was asked to make left the
/// archived mail out of step for good, silently. The mailbox is made without the role; a folder the server
/// will not make at all shows in the account's status.
#[test]
fn jmap_refused_folders_are_retried_and_reported() {
    let srv = alice_only(Config { jmap_refuse_roles: true, ..Config::default() });
    srv.delete_mailbox(ALICE, "Archive").unwrap();
    srv.seed_message(ALICE, "INBOX", &message("Keep me", "keep"), &[]).unwrap();
    let mut s = jmap_session();
    let a = add_alice_jmap(&mut s, &srv);
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let archive = s.mailbox.role_folder(a, FolderRole::Archive).unwrap();
    let keep = find(&s, inbox, "Keep me").unwrap();
    select(&mut s, keep);
    run(&mut s, "mail.archive", json!({"ids": [keep.0]}));
    settle(&mut s);
    sync(&mut s);
    assert_eq!(subjects(&srv, ALICE, "Archive"), ["Keep me"], "made without the role");
    assert!(s.mailbox.folder(archive).unwrap().remote_name.is_some());
    assert!(s.sync_status().accounts.iter().all(|x| x.error.is_none()), "{:?}", s.sync_status());
    sync(&mut s);
    assert_eq!(
        s.mailbox.folders.values().filter(|f| f.account == a && f.name.starts_with("Archive")).count(),
        1
    );

    // A folder the server refuses (the test server takes no "/" in a name): the status says so.
    s.selection.folder = inbox;
    run(&mut s, "folder.new", json!({"name": "a/b"}));
    settle(&mut s);
    sync(&mut s);
    let errors: Vec<String> = s.sync_status().accounts.iter().filter_map(|x| x.error.clone()).collect();
    assert!(errors.iter().any(|e| e.contains("a/b") && e.contains("did not create")), "{errors:?}");
}
