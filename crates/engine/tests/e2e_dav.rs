//! End to end against the local test servers (`mailcraft-testmail`, 127.0.0.1 only, synthetic users):
//! contacts and tasks synchronised with a CardDAV / CalDAV server, a calendar published to a WebDAV server,
//! and other users' / shared IMAP mailboxes opened as stores. Every wait has a timeout.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_engine::Session;
use mailcraft_engine::model::{
    AccountId, Appointment, Contact, DateTime, FolderId, FolderRole, ItemId, Mailbox, Task, TaskStatus,
};
use mailcraft_testmail::{Config, TestServers};
use serde_json::{Value, json};

const ALICE: &str = "alice@example.com";
const ALICE_PW: &str = "alice-test-pw";
const BOB: &str = "bob@example.org";
const SUPPORT: &str = "support@example.com";
const WAIT: Duration = Duration::from_secs(30);

fn servers() -> TestServers {
    TestServers::start(Config {
        users: vec![
            (ALICE.into(), ALICE_PW.into()),
            (BOB.into(), "bob-test-pw".into()),
            (SUPPORT.into(), "support-test-pw".into()),
        ],
        ..Config::default()
    })
    .unwrap()
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn settle(s: &mut Session) {
    assert!(s.wait_for_sync(WAIT), "send/receive timed out: {:?}", s.sync_status());
}

fn session() -> Session {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    s
}

fn add_alice(s: &mut Session, srv: &TestServers, dav: Value) -> AccountId {
    let r = run(
        s,
        "accounts.add",
        json!({
            "kind": "imap",
            "address": ALICE,
            "senderName": "Alice Example",
            "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
            "outgoing": {"host": "127.0.0.1", "port": srv.smtp_port, "security": "starttls", "extraRootPem": srv.cert_pem},
            "password": ALICE_PW,
            "dav": dav,
        }),
    );
    AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
}

fn role(s: &Session, account: AccountId, role: FolderRole) -> FolderId {
    s.mailbox.role_folder(account, role).unwrap()
}

fn update_folder(s: &mut Session, f: FolderId) -> Value {
    let r = run(s, "sendReceive.folder", json!({"folder": f.0}));
    settle(s);
    assert_eq!(s.sync_status().accounts.iter().find_map(|a| a.error.clone()), None, "{:?}", s.sync_status());
    r
}

fn contact_named<'a>(s: &'a Session, folder: FolderId, first: &str) -> Option<&'a Contact> {
    s.mailbox.contacts.values().find(|c| c.folder == folder && c.first_name == first)
}

const PAT: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:pat-1\r\nFN:Pat Example\r\nN:Example;Pat;;;\r\nEMAIL:pat@example.net\r\nEND:VCARD\r\n";

#[test]
fn contacts_folder_synchronises_with_carddav() {
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url()}));
    let contacts = role(&s, alice, FolderRole::Contacts);
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", PAT).unwrap();
    let id = ItemId(s.mailbox.alloc_id());
    s.mailbox.contacts.insert(
        id,
        Contact {
            id,
            folder: contacts,
            first_name: "Robin".into(),
            last_name: "Sample".into(),
            emails: vec!["robin@example.org".into()],
            ..Default::default()
        },
    );
    // Update Folder on Contacts talks to the contacts server, not the mail server.
    let r = update_folder(&mut s, contacts);
    assert_eq!(r["server"], "dav", "{r}");
    let pat = contact_named(&s, contacts, "Pat").expect("the server's contact arrived").clone();
    assert_eq!(pat.emails, ["pat@example.net"]);
    assert_eq!(pat.remote.as_ref().unwrap().uid, "pat-1");
    let items = srv.dav_items(ALICE, "contacts");
    assert_eq!(items.len(), 2, "{items:?}");
    let robin = items.iter().find(|(_, d)| d.contains("Robin")).expect("the local contact was uploaded");
    assert!(robin.1.contains("UID:"), "{}", robin.1);
    assert!(s.mailbox.contacts[&id].remote.is_some());
    let robin_name = robin.0.clone();

    // A second run with no changes moves nothing.
    let before = srv.dav_requests().iter().filter(|r| r.starts_with("PUT")).count();
    update_folder(&mut s, contacts);
    assert_eq!(srv.dav_requests().iter().filter(|r| r.starts_with("PUT")).count(), before);
    assert_eq!(s.mailbox.contacts.values().filter(|c| c.folder == contacts).count(), 2);

    // Changed here: uploaded. Changed there: downloaded (the follow-up flag, which only lives here, stays).
    s.mailbox.contacts.get_mut(&id).unwrap().job_title = "Engineer".into();
    s.mailbox.contacts.get_mut(&pat.id).unwrap().flag = mailcraft_engine::model::FlagState::Flagged {
        label: "Follow up".into(),
        start: None,
        due: None,
        reminder: None,
    };
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", &PAT.replace("pat@example.net", "pat@example.com"))
        .unwrap();
    update_folder(&mut s, contacts);
    let items = srv.dav_items(ALICE, "contacts");
    assert!(items.iter().any(|(n, d)| *n == robin_name && d.contains("Engineer")), "{items:?}");
    let pat_now = &s.mailbox.contacts[&pat.id];
    assert_eq!(pat_now.emails, ["pat@example.com"]);
    assert!(matches!(pat_now.flag, mailcraft_engine::model::FlagState::Flagged { .. }));

    // Deleted here: deleted there. Deleted there: deleted here.
    s.mailbox.contacts.remove(&id);
    srv.delete_dav_item(ALICE, "contacts", "pat-1.vcf");
    update_folder(&mut s, contacts);
    assert!(srv.dav_items(ALICE, "contacts").is_empty());
    assert_eq!(s.mailbox.contacts.values().filter(|c| c.folder == contacts).count(), 0);
}

#[test]
fn contacts_folder_without_a_server_stays_local() {
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, Value::Null);
    let contacts = role(&s, alice, FolderRole::Contacts);
    let r = run(&mut s, "sendReceive.folder", json!({"folder": contacts.0}));
    assert_eq!(r["status"], "local folder");
    assert_eq!(r["started"], 0, "no mail receive starts for a Contacts folder");
    settle(&mut s);
}

fn view_ids(s: &mut Session, folder: FolderId, view: &str) -> Vec<ItemId> {
    s.selection.folder = folder;
    run(s, "view.changeView", json!({"name": view}));
    mailcraft_engine::cmd::tasks::view_rows(s, folder).iter().map(|r| r.id).collect()
}

const TODO: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VTODO\r\nUID:todo-1\r\nSUMMARY:Order toner\r\nSTATUS:NEEDS-ACTION\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";

#[test]
fn tasks_synchronise_with_caldav_and_fill_server_tasks() {
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "contacts": false}));
    let tasks = role(&s, alice, FolderRole::Tasks);
    srv.put_dav_item(ALICE, "tasks", "todo-1.ics", TODO).unwrap();
    let id = ItemId(s.mailbox.alloc_id());
    s.mailbox
        .tasks
        .insert(id, Task { id, folder: tasks, subject: "Book the room".into(), ..Default::default() });
    // Before any synchronisation nothing here lives on a server.
    assert!(view_ids(&mut s, tasks, "Server Tasks").is_empty());
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    let toner =
        s.mailbox.tasks.values().find(|t| t.subject == "Order toner").expect("server task arrived").id;
    let mut on_server = view_ids(&mut s, tasks, "Server Tasks");
    on_server.sort();
    let mut want = vec![id, toner];
    want.sort();
    assert_eq!(on_server, want);
    let items = srv.dav_items(ALICE, "tasks");
    assert!(items.iter().any(|(_, d)| d.contains("SUMMARY:Book the room")), "{items:?}");
    // Contacts were turned off: nothing went to the address book.
    assert!(srv.dav_items(ALICE, "contacts").is_empty());
    // Completing a task here reaches the server.
    {
        let t = s.mailbox.tasks.get_mut(&toner).unwrap();
        t.status = TaskStatus::Completed;
        t.percent_complete = 100;
    }
    update_folder(&mut s, tasks);
    let (_, data) = srv.dav_items(ALICE, "tasks").into_iter().find(|(n, _)| n == "todo-1.ics").unwrap();
    assert!(data.contains("STATUS:COMPLETED"), "{data}");
    assert!(data.contains("UID:todo-1"), "the server's UID is kept: {data}");
    // A local-only task never shows in Server Tasks.
    let local = role(&s, s.mailbox.accounts[0].id, FolderRole::Tasks);
    let lid = ItemId(s.mailbox.alloc_id());
    s.mailbox
        .tasks
        .insert(lid, Task { id: lid, folder: local, subject: "Local".into(), ..Default::default() });
    assert!(view_ids(&mut s, local, "Server Tasks").is_empty());
}

#[test]
fn published_calendar_follows_changes_and_can_be_withdrawn() {
    let srv = servers();
    let mut s = session();
    let alice = add_alice(
        &mut s,
        &srv,
        json!({"url": srv.dav_url(), "publishUrl": srv.dav_collection_url(ALICE, "public")}),
    );
    let cal = role(&s, alice, FolderRole::Calendar);
    let id = ItemId(s.mailbox.alloc_id());
    s.mailbox.appointments.insert(
        id,
        Appointment {
            id,
            folder: cal,
            // With a UID of its own (an event without one gets one when the calendar first goes to the CalDAV
            // server, which changes the published file once).
            uid: "team-review@example.com".into(),
            subject: "Team review".into(),
            start: DateTime::from_unix(1_790_000_000, 0),
            end: DateTime::from_unix(1_790_003_600, 0),
            ..Default::default()
        },
    );
    let r = run(&mut s, "calendar.publish", json!({"folder": cal.0}));
    settle(&mut s);
    let url = r["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with(&srv.dav_collection_url(ALICE, "public")), "{url}");
    assert!(r["webcal"].as_str().unwrap().starts_with("webcals://"));
    let items = srv.dav_items(ALICE, "public");
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].1.contains("SUMMARY:Team review"));
    assert!(items[0].1.contains("REFRESH-INTERVAL"), "{}", items[0].1);
    // A subscriber's MailCraft reads the published file as an internet calendar.
    let mut other = session();
    let sub =
        run(&mut other, "calendar.addCalendar", json!({"source": "internet", "url": url, "fetch": false}));
    let folder = sub["folder"].as_u64().unwrap();
    run(&mut other, "calendar.refreshCalendar", json!({"folder": folder, "data": items[0].1}));
    assert!(other.mailbox.appointments.values().any(|a| a.folder.0 == folder && a.subject == "Team review"));

    // Unchanged: not uploaded again. Changed: uploaded by the next send/receive.
    let puts = || srv.dav_requests().iter().filter(|r| r.starts_with("PUT")).count();
    let before = puts();
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert_eq!(puts(), before);
    s.mailbox.appointments.get_mut(&id).unwrap().subject = "Team review (moved)".into();
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert!(srv.dav_items(ALICE, "public")[0].1.contains("Team review (moved)"));

    // The sharing invitation carries the address.
    let r = run(&mut s, "calendar.share", json!({"folder": cal.0, "publish": true, "recipients": BOB}));
    let compose = &s.composes[&r["compose"].as_u64().unwrap()];
    assert_eq!(compose.to, BOB);
    assert!(compose.body.to_plain_text().contains("webcals://"), "{}", compose.body.to_plain_text());

    // Stop publishing: the file goes.
    run(&mut s, "calendar.publish", json!({"folder": cal.0, "stop": true}));
    settle(&mut s);
    assert!(srv.dav_items(ALICE, "public").is_empty());
}

#[test]
fn shared_mailboxes_open_as_stores_and_join_locations_to_search() {
    let srv = servers();
    srv.share_mailbox("Support", SUPPORT).unwrap();
    srv.grant_access(BOB, ALICE).unwrap();
    let raw = b"From: Pat <pat@example.net>\r\nTo: support@example.com\r\nSubject: Printer jams\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\nMessage-ID: <p1@example.net>\r\n\r\nIt jams.\r\n";
    srv.seed_message(SUPPORT, "INBOX", raw, &[]).unwrap();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, Value::Null);
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    // The personal folder list does not swallow the shared ones.
    assert!(s.mailbox.folders.values().filter(|f| f.account == alice).all(|f| !f.name.contains("Support")));
    let r = run(&mut s, "accounts.sharedMailboxes", json!({"account": alice.0}));
    let names: Vec<&str> =
        r["mailboxes"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(names, [BOB, "Support"]);
    let r = run(&mut s, "accounts.openSharedMailbox", json!({"account": alice.0, "mailbox": "Support"}));
    let store = AccountId(u32::try_from(r["store"].as_u64().unwrap()).unwrap());
    settle(&mut s);
    let inbox = role(&s, store, FolderRole::Inbox);
    let msg = s.mailbox.mail.values().find(|m| m.folder == inbox).expect("the shared inbox downloaded").id;
    assert_eq!(s.mailbox.mail[&msg].subject, "Printer jams");
    // Locations to Search lists the store beside the user's own mailbox.
    let locations = mailcraft_engine::cmd::search::locations(&s);
    assert!(locations.iter().any(|(_, label, on)| label == "Support" && *on), "{locations:?}");
    run(&mut s, "search.scope", json!({"store": "Support", "on": false}));
    assert!(mailcraft_engine::cmd::search::locations(&s).iter().any(|(_, l, on)| l == "Support" && !on));
    // Reading it here marks it read in the shared mailbox.
    s.selection.folder = inbox;
    s.selection.items = vec![msg];
    s.selection.focus = Some(msg);
    run(&mut s, "mail.markRead", json!({}));
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    let flags = &srv.messages(SUPPORT, "INBOX").unwrap()[0].flags;
    assert!(flags.iter().any(|f| f == "\\Seen"), "{flags:?}");
    // The store cannot send.
    assert!(!s.mailbox.account(store).unwrap().can_send());
    // Closing it removes the local copy only.
    run(
        &mut s,
        "accounts.openSharedMailbox",
        json!({"account": alice.0, "mailbox": "Support", "open": false}),
    );
    assert!(s.mailbox.account(store).is_none());
    assert_eq!(srv.messages(SUPPORT, "INBOX").unwrap().len(), 1);
}

#[test]
fn undoing_a_delete_puts_the_contact_back_on_the_server() {
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "tasks": false}));
    let contacts = role(&s, alice, FolderRole::Contacts);
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", PAT).unwrap();
    update_folder(&mut s, contacts);
    let pat = contact_named(&s, contacts, "Pat").unwrap().id;
    // Deleted here (moved to Deleted Items): deleted on the server too.
    s.selection.folder = contacts;
    s.selection.items = vec![pat];
    s.selection.focus = Some(pat);
    run(&mut s, "people.delete", json!({"ids": [pat.0]}));
    update_folder(&mut s, contacts);
    assert!(srv.dav_items(ALICE, "contacts").is_empty());
    assert!(s.mailbox.contacts[&pat].remote.is_none(), "the moved copy no longer points at the server");
    // Undo brings it back here, and the next run brings it back there.
    run(&mut s, "app.undo", json!({}));
    assert_eq!(s.mailbox.contacts[&pat].folder, contacts);
    update_folder(&mut s, contacts);
    let items = srv.dav_items(ALICE, "contacts");
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].1.contains("Pat Example"));
    assert_eq!(s.mailbox.contacts.values().filter(|c| c.folder == contacts).count(), 1, "no duplicate");
}

fn server_uid(data: &str) -> String {
    data.lines().find_map(|l| l.strip_prefix("UID:")).unwrap_or_default().trim().to_owned()
}

#[test]
fn completing_or_copying_a_synced_task_keeps_both_items() {
    // Regression: the completed occurrence (and a copy in the same folder) kept the series' UID and server
    // link, so the next run overwrote it with the server copy of the series.
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "contacts": false}));
    let tasks = role(&s, alice, FolderRole::Tasks);
    let r = run(
        &mut s,
        "tasks.new",
        json!({"subject": "Weekly report", "folder": tasks.0, "due": "2026-10-09", "recurrence": {"rrule": "FREQ=WEEKLY"}}),
    );
    let series = ItemId(r["id"].as_u64().unwrap());
    assert_eq!(s.mailbox.tasks[&series].folder, tasks);
    assert!(s.mailbox.tasks[&series].recurrence.is_some());
    update_folder(&mut s, tasks);
    assert_eq!(srv.dav_items(ALICE, "tasks").len(), 1);
    run(&mut s, "tasks.markComplete", json!({"ids": [series.0]}));
    let done: Vec<ItemId> =
        s.mailbox.tasks.values().filter(|t| t.folder == tasks && t.is_complete()).map(|t| t.id).collect();
    assert_eq!(done.len(), 1, "the completed occurrence is kept as a task of its own");
    let done = done[0];
    update_folder(&mut s, tasks);
    update_folder(&mut s, tasks);
    assert!(s.mailbox.tasks[&done].is_complete(), "the completed copy survives the next runs");
    assert!(s.mailbox.tasks[&done].recurrence.is_none());
    assert!(!s.mailbox.tasks[&series].is_complete());
    assert_ne!(s.mailbox.tasks[&done].uid, s.mailbox.tasks[&series].uid);
    let items = srv.dav_items(ALICE, "tasks");
    assert_eq!(items.len(), 2, "{items:?}");
    assert!(items.iter().any(|(_, d)| d.contains("STATUS:COMPLETED") && !d.contains("RRULE")), "{items:?}");
    let uids: std::collections::BTreeSet<String> = items.iter().map(|(_, d)| server_uid(d)).collect();
    assert_eq!(uids.len(), 2, "{items:?}");
    // A copy in the same folder is a new task there too.
    let copy = run(&mut s, "tasks.copy", json!({"id": series.0, "folder": tasks.0}));
    let copy = ItemId(copy["copies"][0].as_u64().unwrap());
    s.mailbox.tasks.get_mut(&copy).unwrap().subject = "Weekly report (copy)".into();
    update_folder(&mut s, tasks);
    update_folder(&mut s, tasks);
    assert_eq!(s.mailbox.tasks[&copy].subject, "Weekly report (copy)");
    assert_eq!(s.mailbox.tasks[&series].subject, "Weekly report");
    assert_eq!(srv.dav_items(ALICE, "tasks").len(), 3);
    assert_eq!(s.mailbox.tasks.values().filter(|t| t.folder == tasks).count(), 3);
}

fn contacts_here(s: &Session, folder: FolderId) -> usize {
    s.mailbox.contacts.values().filter(|c| c.folder == folder).count()
}

fn linked(s: &Session, folder: FolderId) -> usize {
    s.mailbox.contacts.values().filter(|c| c.folder == folder && c.remote.is_some()).count()
}

#[test]
fn settings_changes_never_duplicate_contacts() {
    // Regression: changing the mail server, or the DAV address in any way, dropped every link, and the next
    // run uploaded each contact again and downloaded each server copy as a new contact.
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "tasks": false}));
    let contacts = role(&s, alice, FolderRole::Contacts);
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", PAT).unwrap();
    let id = ItemId(s.mailbox.alloc_id());
    s.mailbox.contacts.insert(
        id,
        Contact {
            id,
            folder: contacts,
            first_name: "Robin".into(),
            last_name: "Sample".into(),
            ..Default::default()
        },
    );
    update_folder(&mut s, contacts);
    assert_eq!((contacts_here(&s, contacts), srv.dav_items(ALICE, "contacts").len()), (2, 2));

    // Another mail server port: the contacts server is untouched.
    let r = run(
        &mut s,
        "accounts.edit",
        json!({"account": alice.0, "incoming": {"host": "127.0.0.1", "port": srv.imap_port, "security": "starttls", "extraRootPem": srv.cert_pem}}),
    );
    assert_eq!(r["resetSync"], true, "{r}");
    assert_eq!(linked(&s, contacts), 2);
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert_eq!((contacts_here(&s, contacts), srv.dav_items(ALICE, "contacts").len()), (2, 2));

    // The same DAV address written differently.
    let bare = srv.dav_url().trim_end_matches('/').to_owned();
    run(&mut s, "accounts.edit", json!({"account": alice.0, "dav": {"url": bare}}));
    assert_eq!(linked(&s, contacts), 2);

    // Really another address: the links go, and undo brings them back with the old address.
    let book = srv.dav_collection_url(ALICE, "contacts");
    run(&mut s, "accounts.edit", json!({"account": alice.0, "dav": {"url": book}}));
    assert_eq!(linked(&s, contacts), 0);
    run(&mut s, "app.undo", json!({}));
    assert_eq!(linked(&s, contacts), 2);
    assert_eq!(s.mailbox.account(alice).unwrap().dav.as_ref().unwrap().url, bare);

    // Links lost for good: the next run recognises the contacts already on the server.
    run(&mut s, "accounts.edit", json!({"account": alice.0, "dav": {"url": book}}));
    let puts = srv.dav_requests().iter().filter(|r| r.starts_with("PUT")).count();
    update_folder(&mut s, contacts);
    assert_eq!((contacts_here(&s, contacts), srv.dav_items(ALICE, "contacts").len()), (2, 2));
    assert_eq!(linked(&s, contacts), 2);
    assert_eq!(
        srv.dav_requests().iter().filter(|r| r.starts_with("PUT")).count(),
        puts,
        "nothing uploaded again"
    );
}

#[test]
fn an_unreadable_server_item_is_not_deleted() {
    // Regression: a copy that could not be read was remembered as seen, so the next run took its absence
    // here for a deletion and removed it from the server.
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "tasks": false}));
    let contacts = role(&s, alice, FolderRole::Contacts);
    srv.put_dav_item(ALICE, "contacts", "odd.vcf", "this is not a vCard").unwrap();
    for _ in 0..2 {
        run(&mut s, "sendReceive.folder", json!({"folder": contacts.0}));
        settle(&mut s);
    }
    assert_eq!(srv.dav_items(ALICE, "contacts").len(), 1, "the server copy stays");
    assert_eq!(contacts_here(&s, contacts), 0);
}

#[test]
fn publishing_stays_on_the_publishing_server_and_moves_cleanly() {
    let srv = servers();
    let mut s = session();
    let public = srv.dav_collection_url(ALICE, "public");
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "publishUrl": public}));
    let cal = role(&s, alice, FolderRole::Calendar);
    // The account's login never goes to another server.
    let e =
        s.run("calendar.publish", &json!({"folder": cal.0, "url": "https://elsewhere.example.net/cal.ics"}));
    assert!(e.is_err(), "{e:?}");
    let e = s.run(
        "calendar.publish",
        &json!({"folder": cal.0, "url": format!("https://x@{}", public.trim_start_matches("https://"))}),
    );
    assert!(e.is_err(), "{e:?}");
    run(&mut s, "calendar.publish", json!({"folder": cal.0, "url": format!("{public}first.ics")}));
    settle(&mut s);
    let names = || srv.dav_items(ALICE, "public").into_iter().map(|(n, _)| n).collect::<Vec<_>>();
    assert_eq!(names(), ["first.ics"]);
    // Published again at another address: the old file is removed, not left public.
    run(
        &mut s,
        "calendar.publish",
        json!({"folder": cal.0, "url": format!("{public}second.ics"), "detail": "availability"}),
    );
    settle(&mut s);
    assert_eq!(names(), ["second.ics"]);
    run(&mut s, "calendar.publish", json!({"folder": cal.0, "stop": true}));
    settle(&mut s);
    assert!(names().is_empty());
}

const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:series-1\r\n\
DTSTAMP:20261001T000000Z\r\nDTSTART:20261012T140000Z\r\nDTEND:20261012T150000Z\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\n\
SUMMARY:Weekly sync\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:series-1\r\nDTSTAMP:20261001T000000Z\r\n\
RECURRENCE-ID:20261019T140000Z\r\nDTSTART:20261019T160000Z\r\nDTEND:20261019T170000Z\r\nSUMMARY:Weekly sync (moved)\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";

fn at(y: i32, mo: u8, d: u8, h: u8) -> i64 {
    DateTime::from_civil(y, mo, d, h, 0, 0, 0).unwrap().unix
}

fn calendar_items(s: &Session, folder: FolderId) -> Vec<&Appointment> {
    s.mailbox.appointments.values().filter(|a| a.folder == folder).collect()
}

#[test]
fn calendar_events_synchronise_with_caldav() {
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "contacts": false, "tasks": false}));
    let cal = role(&s, alice, FolderRole::Calendar);
    srv.put_dav_item(ALICE, "calendar", "series-1.ics", SERIES).unwrap();
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": cal.0, "subject": "Dentist", "start": at(2026, 10, 14, 9), "end": at(2026, 10, 14, 10),
               "importance": "high"}),
    );
    let dentist = ItemId(r["id"].as_u64().unwrap());

    // Update Folder on the calendar talks to the CalDAV server.
    let r = update_folder(&mut s, cal);
    assert_eq!(r["server"], "dav", "{r}");
    let here = calendar_items(&s, cal);
    let series = here.iter().find(|a| a.recurrence.is_some()).expect("the series arrived");
    let moved = here.iter().find(|a| a.recurrence_id.is_some()).expect("its exception arrived");
    assert_eq!((series.uid.as_str(), moved.uid.as_str()), ("series-1", "series-1"));
    assert_eq!(moved.subject, "Weekly sync (moved)");
    assert_eq!(moved.start.unix, at(2026, 10, 19, 16));
    assert!(series.recurrence.as_ref().unwrap().exdates.iter().any(|x| x.unix == at(2026, 10, 19, 14)));
    assert!(series.remote.is_some() && moved.remote.is_none(), "the series holds the server link");
    let (series_id, moved_id) = (series.id, moved.id);
    let items = srv.dav_items(ALICE, "calendar");
    assert_eq!(items.len(), 2, "{items:?}");
    let (dentist_name, dentist_data) =
        items.iter().find(|(_, d)| d.contains("SUMMARY:Dentist")).cloned().expect("the local event went up");
    assert!(dentist_data.contains("PRIORITY:1"), "{dentist_data}");
    assert!(s.mailbox.appointments[&dentist].remote.is_some());
    // Nothing reached the task list.
    assert!(srv.dav_items(ALICE, "tasks").is_empty());

    // A second run with no changes moves nothing.
    let puts = || srv.dav_requests().iter().filter(|r| r.starts_with("PUT")).count();
    let before = puts();
    update_folder(&mut s, cal);
    assert_eq!(puts(), before);
    assert_eq!(calendar_items(&s, cal).len(), 3);

    // Changed here: the series goes up with its exception, which is no EXDATE of it.
    run(&mut s, "calendar.update", json!({"id": series_id.0, "location": "Room 2"}));
    update_folder(&mut s, cal);
    let (_, data) = srv.dav_items(ALICE, "calendar").into_iter().find(|(n, _)| n == "series-1.ics").unwrap();
    assert!(data.contains("LOCATION:Room 2"), "{data}");
    assert!(data.contains("RECURRENCE-ID"), "{data}");
    assert!(!data.contains("EXDATE"), "an overridden instance is not excluded: {data}");
    assert!(data.contains("UID:series-1"), "{data}");

    // Editing one more occurrence here adds it to the same resource.
    run(
        &mut s,
        "calendar.editOccurrence",
        json!({"id": series_id.0, "occurrence": at(2026, 10, 26, 14), "subject": "Weekly sync (short)"}),
    );
    update_folder(&mut s, cal);
    let (_, data) = srv.dav_items(ALICE, "calendar").into_iter().find(|(n, _)| n == "series-1.ics").unwrap();
    assert_eq!(data.matches("RECURRENCE-ID").count(), 2, "{data}");
    assert!(data.contains("Weekly sync (short)"), "{data}");
    assert_eq!(calendar_items(&s, cal).len(), 4, "no duplicates after the upload");

    // Changed there: downloaded, keeping the item ids.
    let edited = SERIES.replace("SUMMARY:Weekly sync (moved)", "SUMMARY:Weekly sync (moved again)");
    srv.put_dav_item(ALICE, "calendar", "series-1.ics", &edited).unwrap();
    update_folder(&mut s, cal);
    assert_eq!(s.mailbox.appointments[&moved_id].subject, "Weekly sync (moved again)");
    assert!(s.mailbox.appointments.contains_key(&series_id));
    assert_eq!(calendar_items(&s, cal).len(), 3, "the server's copy has one exception now");

    // Deleted here (with its exceptions): deleted there. Deleted there: deleted here.
    run(&mut s, "calendar.delete", json!({"id": series_id.0}));
    srv.delete_dav_item(ALICE, "calendar", &dentist_name);
    update_folder(&mut s, cal);
    assert!(srv.dav_items(ALICE, "calendar").is_empty(), "{:?}", srv.dav_items(ALICE, "calendar"));
    assert!(calendar_items(&s, cal).is_empty());

    // Undo brings the series back here, and the next run brings it back there.
    run(&mut s, "app.undo", json!({}));
    assert!(s.mailbox.appointments.contains_key(&series_id));
    assert!(!s.mailbox.appointments.contains_key(&dentist), "the server's deletion is not undone");
    update_folder(&mut s, cal);
    let items = srv.dav_items(ALICE, "calendar");
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].1.contains("UID:series-1"));
}

#[test]
fn a_copied_event_is_a_server_item_of_its_own() {
    // Regression: a copy kept the original's server link, so once it was the only item holding it (the
    // original deleted here) it went up over the original's resource under the original's UID.
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "contacts": false, "tasks": false}));
    let cal = role(&s, alice, FolderRole::Calendar);
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": cal.0, "subject": "Dentist", "start": at(2026, 10, 14, 9), "end": at(2026, 10, 14, 10)}),
    );
    let original = ItemId(r["id"].as_u64().unwrap());
    update_folder(&mut s, cal);
    let first = srv.dav_items(ALICE, "calendar");
    assert_eq!(first.len(), 1, "{first:?}");
    let original_uid = server_uid(&first[0].1);
    let r = run(&mut s, "calendar.copy", json!({"id": original.0, "folder": cal.0}));
    let copy = ItemId(r["copies"][0].as_u64().unwrap());
    assert!(s.mailbox.appointments[&copy].remote.is_none());
    update_folder(&mut s, cal);
    let both = srv.dav_items(ALICE, "calendar");
    assert_eq!(both.len(), 2, "{both:?}");
    let copy_uid = s.mailbox.appointments[&copy].remote.as_ref().unwrap().uid.clone();
    assert_ne!(copy_uid, original_uid);
    // The original goes; the copy stays on the server under its own UID.
    run(&mut s, "calendar.delete", json!({"id": original.0}));
    update_folder(&mut s, cal);
    update_folder(&mut s, cal);
    let left = srv.dav_items(ALICE, "calendar");
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(server_uid(&left[0].1), copy_uid, "{left:?}");
    assert!(s.mailbox.appointments.contains_key(&copy));
}

#[test]
fn a_collection_with_events_and_tasks_is_shared_by_calendar_and_tasks() {
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "contacts": false}));
    let cal = role(&s, alice, FolderRole::Calendar);
    // A task stored in the events calendar (as some servers allow) is neither an error nor deleted.
    srv.put_dav_item(ALICE, "calendar", "todo-x.ics", TODO).unwrap();
    srv.put_dav_item(ALICE, "calendar", "series-1.ics", SERIES).unwrap();
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert_eq!(s.sync_status().accounts.iter().find_map(|a| a.error.clone()), None);
    assert_eq!(calendar_items(&s, cal).len(), 2);
    update_folder(&mut s, cal);
    assert_eq!(srv.dav_items(ALICE, "calendar").len(), 2, "the task stays on the server");
}
