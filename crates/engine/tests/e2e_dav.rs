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

fn add_alice(s: &mut Session, srv: &TestServers, mut dav: Value) -> AccountId {
    // The DAV server's certificate is trusted by the DAV settings themselves (never by the mail server's).
    if let Some(o) = dav.as_object_mut() {
        o.entry("extraRootPem").or_insert_with(|| json!(srv.cert_pem));
    }
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

fn puts(srv: &TestServers) -> usize {
    srv.dav_requests().iter().filter(|r| r.starts_with("PUT")).count()
}

fn account_error(s: &Session) -> String {
    s.sync_status().accounts.iter().find_map(|a| a.error.clone()).unwrap_or_default()
}

const PHONE: &str =
    "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:phone-1\r\nFN:Sam Phone\r\nN:Phone;Sam;;;\r\nEND:VCARD\r\n";

#[test]
fn a_put_without_an_etag_never_deletes_what_another_device_added() {
    // Regression: after a PUT answered without an ETag, the run listed the collection again and took every
    // resource in it as known, also one another device created meanwhile; the next run read its absence here
    // as a deletion and removed it from the server.
    let srv = servers();
    srv.set_dav_put_etags(false);
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "tasks": false, "calendar": false}));
    let contacts = role(&s, alice, FolderRole::Contacts);
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
    srv.put_dav_item_after_next_put(ALICE, "contacts", "phone-1.vcf", PHONE);
    update_folder(&mut s, contacts);
    assert_eq!(
        srv.dav_items(ALICE, "contacts").len(),
        2,
        "uploaded, and the phone's contact arrived meanwhile"
    );
    let robin = s.mailbox.contacts[&id].remote.clone().expect("linked");
    assert!(!robin.etag.is_empty(), "the ETag came from the second listing");
    for _ in 0..2 {
        update_folder(&mut s, contacts);
        let names: Vec<String> = srv.dav_items(ALICE, "contacts").into_iter().map(|(n, _)| n).collect();
        assert!(names.iter().any(|n| n == "phone-1.vcf"), "the other device's contact stays: {names:?}");
    }
    assert!(contact_named(&s, contacts, "Sam").is_some(), "and arrives here");
    assert!(!srv.dav_requests().iter().any(|r| r.starts_with("DELETE")), "{:?}", srv.dav_requests());
    assert_eq!(contacts_here(&s, contacts), 2);
}

#[test]
fn publishing_never_replaces_or_removes_a_file_it_did_not_write() {
    // Regression: a publication could name any resource on the publishing server, and was written with an
    // unconditional PUT and removed with an unconditional DELETE.
    let srv = servers();
    let mut s = session();
    let public = srv.dav_collection_url(ALICE, "public");
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "publishUrl": public}));
    let cal = role(&s, alice, FolderRole::Calendar);
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", PAT).unwrap();
    // Only a file inside the publishing location: not a contact, not the address book, not the location.
    for bad in [
        srv.dav_collection_url(ALICE, "contacts") + "pat-1.vcf",
        srv.dav_collection_url(ALICE, "contacts"),
        format!("{public}../contacts/pat-1.vcf"),
        format!("{public}%2E%2E%2Fcontacts%2Fpat-1.vcf"),
        public.clone(),
    ] {
        let e = s.run("calendar.publish", &json!({"folder": cal.0, "url": bad}));
        assert!(e.is_err(), "{bad}: {e:?}");
    }
    // Someone else's file already at the address: left as it is.
    srv.put_dav_item(ALICE, "public", "taken.ics", "someone else's file").unwrap();
    run(&mut s, "calendar.publish", json!({"folder": cal.0, "url": format!("{public}taken.ics")}));
    settle(&mut s);
    let file = |n: &str| srv.dav_items(ALICE, "public").into_iter().find(|(x, _)| x == n).map(|(_, d)| d);
    assert_eq!(file("taken.ics").as_deref(), Some("someone else's file"));
    assert!(account_error(&s).contains("did not publish"), "{}", account_error(&s));
    // Stopped: nothing of MailCraft's to remove, so the file stays.
    run(&mut s, "calendar.publish", json!({"folder": cal.0, "stop": true}));
    settle(&mut s);
    assert_eq!(file("taken.ics").as_deref(), Some("someone else's file"));
    assert!(srv.dav_requests().iter().all(|r| !r.starts_with("DELETE")), "{:?}", srv.dav_requests());

    // A file of its own: replaced only while it is still the one MailCraft wrote.
    run(&mut s, "calendar.publish", json!({"folder": cal.0, "url": format!("{public}mine.ics")}));
    settle(&mut s);
    assert!(file("mine.ics").is_some_and(|d| d.contains("BEGIN:VCALENDAR")));
    srv.put_dav_item(ALICE, "public", "mine.ics", "replaced by hand").unwrap();
    let id = ItemId(s.mailbox.alloc_id());
    s.mailbox.appointments.insert(
        id,
        Appointment {
            id,
            folder: cal,
            uid: "review@example.com".into(),
            subject: "Review".into(),
            start: DateTime::from_unix(1_790_000_000, 0),
            end: DateTime::from_unix(1_790_003_600, 0),
            ..Default::default()
        },
    );
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert_eq!(file("mine.ics").as_deref(), Some("replaced by hand"), "not overwritten");
    // And not removed when publishing stops, but not quietly either: the address is still public.
    run(&mut s, "calendar.publish", json!({"folder": cal.0, "stop": true}));
    settle(&mut s);
    assert_eq!(file("mine.ics").as_deref(), Some("replaced by hand"), "not removed");
    let e = account_error(&s);
    assert!(e.contains("still published") && e.contains("mine.ics"), "{e}");
    let p = mailcraft_engine::cmd::calendar::props::read(&s.mailbox, cal).publication.expect("kept");
    assert!(p.withdraw && p.last_error.is_some(), "{p:?}");
    assert_eq!(srv.dav_items(ALICE, "contacts").len(), 1, "the address book was never touched");
    // Removed by hand: the next run sees it gone and the publication ends.
    srv.delete_dav_item(ALICE, "public", "mine.ics");
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert!(mailcraft_engine::cmd::calendar::props::read(&s.mailbox, cal).publication.is_none());
}

#[test]
fn uids_with_reserved_characters_get_resources_of_their_own() {
    // Regression: every character outside `[A-Za-z0-9._-]` became `-`, so these three tasks shared one
    // resource and two of them lost their content to the third.
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "contacts": false, "calendar": false}));
    let tasks = role(&s, alice, FolderRole::Tasks);
    for (uid, subject) in [("a@b.com", "At"), ("a#b.com", "Hash"), ("a/b.com", "Slash")] {
        let id = ItemId(s.mailbox.alloc_id());
        s.mailbox.tasks.insert(
            id,
            Task { id, folder: tasks, uid: uid.into(), subject: subject.into(), ..Default::default() },
        );
    }
    update_folder(&mut s, tasks);
    let mut names: Vec<String> = srv.dav_items(ALICE, "tasks").into_iter().map(|(n, _)| n).collect();
    names.sort();
    assert_eq!(names, ["a#b.com.ics", "a/b.com.ics", "a@b.com.ics"]);
    let before = puts(&srv);
    update_folder(&mut s, tasks);
    assert_eq!(puts(&srv), before, "nothing uploaded again");
    let mut subjects: Vec<String> =
        s.mailbox.tasks.values().filter(|t| t.folder == tasks).map(|t| t.subject.clone()).collect();
    subjects.sort();
    assert_eq!(subjects, ["At", "Hash", "Slash"]);
    assert!(s.mailbox.tasks.values().filter(|t| t.folder == tasks).all(|t| t.remote.is_some()));
}

#[test]
fn discovery_starts_from_an_address_with_escapes() {
    // Regression: `alice%40example.com` was encoded again (`alice%2540example.com`), a path that does not
    // exist, so no address book was found.
    let srv = servers();
    let mut s = session();
    let url = format!("{}/dav/alice%40example.com/", srv.web_url());
    let alice = add_alice(&mut s, &srv, json!({"url": url, "tasks": false, "calendar": false}));
    let contacts = role(&s, alice, FolderRole::Contacts);
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", PAT).unwrap();
    update_folder(&mut s, contacts);
    assert!(contact_named(&s, contacts, "Pat").is_some());
    assert!(srv.dav_requests().iter().all(|r| !r.contains("%25")), "{:?}", srv.dav_requests());
}

#[test]
fn dav_does_not_borrow_the_mail_servers_connection_settings() {
    // Regression: the DAV connection took the incoming server's trusted certificate (and plain-text consent).
    let srv = servers();
    let mut s = session();
    let alice = add_alice(
        &mut s,
        &srv,
        json!({"url": srv.dav_url(), "tasks": false, "calendar": false, "extraRootPem": null}),
    );
    let contacts = role(&s, alice, FolderRole::Contacts);
    run(&mut s, "sendReceive.folder", json!({"folder": contacts.0}));
    settle(&mut s);
    let error = account_error(&s);
    assert!(
        error.to_ascii_lowercase().contains("tls"),
        "the mail server's certificate is not trusted here: {error}"
    );
    assert!(srv.dav_requests().is_empty(), "{:?}", srv.dav_requests());
    // Trusted in the DAV settings: works.
    run(&mut s, "accounts.edit", json!({"account": alice.0, "dav": {"extraRootPem": srv.cert_pem}}));
    update_folder(&mut s, contacts);
}

fn poll_until(s: &mut Session, what: &str, mut done: impl FnMut(&mut Session) -> bool) {
    let end = std::time::Instant::now() + WAIT;
    loop {
        s.poll_sync();
        if done(s) {
            return;
        }
        assert!(std::time::Instant::now() < end, "timed out waiting for {what}: {:?}", s.sync_status());
        std::thread::sleep(Duration::from_millis(20));
    }
}

const LEE: &str =
    "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:lee-1\r\nFN:Lee Example\r\nN:Example;Lee;;;\r\nEND:VCARD\r\n";

/// Alice's account signed in with OAuth (device flow against the test authorization server), its DAV server
/// set up with `dav`.
fn oauth_alice(srv: &TestServers, s: &mut Session, mut dav: Value) -> AccountId {
    s.sync.oauth_http = Some(mailcraft_engine::sync::ConnectOptions::trusting(srv.cert_der.clone()));
    if let Some(o) = dav.as_object_mut() {
        o.insert("url".into(), json!(srv.dav_url()));
        o.insert("extraRootPem".into(), json!(srv.cert_pem));
    }
    let r = run(
        s,
        "accounts.add",
        json!({
            "kind": "imap",
            "address": ALICE,
            "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
            "dav": dav,
        }),
    );
    let alice = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    let (_, token, device) = srv.oauth_endpoints();
    run(
        s,
        "accounts.oauthSignIn",
        json!({"account": alice.0, "provider": "custom", "clientId": mailcraft_testmail::OAUTH_TEST_CLIENT,
               "flow": "device", "tokenEndpoint": token, "deviceAuthorizationEndpoint": device, "scopes": ["mail"]}),
    );
    let mut code = String::new();
    poll_until(s, "the device code", |s| {
        code = s.run("accounts.oauthStatus", &json!({"account": alice.0})).unwrap()["userCode"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        !code.is_empty()
    });
    srv.oauth_approve(&code, ALICE).unwrap();
    poll_until(s, "sign-in", |s| {
        s.run("accounts.oauthStatus", &json!({"account": alice.0})).unwrap()["status"] == "done"
    });
    alice
}

fn refreshes(srv: &TestServers) -> usize {
    srv.oauth_grants().iter().filter(|g| *g == "refresh_token").count()
}

#[test]
fn a_token_only_the_dav_server_refuses_is_renewed_once_and_dav_tried_again() {
    // The mail server still takes the token, the DAV server refuses it (revoked on its side): the run renews
    // the token once and the DAV part goes through with the new one.
    let srv = servers();
    let mut s = session();
    let alice = oauth_alice(&srv, &mut s, json!({"tasks": false, "calendar": false}));
    let contacts = role(&s, alice, FolderRole::Contacts);
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", PAT).unwrap();
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert!(contact_named(&s, contacts, "Pat").is_some(), "{:?}", s.sync_status());
    let before = refreshes(&srv);
    srv.refuse_dav_sign_in_once("/dav/");
    srv.put_dav_item(ALICE, "contacts", "lee-1.vcf", LEE).unwrap();
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert_eq!(account_error(&s), "", "{:?}", s.sync_status());
    assert_eq!(refreshes(&srv), before + 1, "renewed exactly once");
    assert!(contact_named(&s, contacts, "Lee").is_some(), "the second DAV pass went through");
    assert_eq!(contacts_here(&s, contacts), 2);
}

#[test]
fn a_refusal_after_folders_were_updated_is_not_retried_from_the_start() {
    // A retry runs the whole DAV job again from the state it was built with; once a folder's result went out,
    // that state is stale, so the refusal is reported instead (the next run starts afresh).
    let srv = servers();
    let mut s = session();
    let alice = oauth_alice(&srv, &mut s, json!({"calendar": false}));
    let contacts = role(&s, alice, FolderRole::Contacts);
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert_eq!(account_error(&s), "", "{:?}", s.sync_status());
    let before = refreshes(&srv);
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", PAT).unwrap();
    srv.refuse_dav_sign_in_once(&format!("/dav/{ALICE}/tasks/"));
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert!(!account_error(&s).is_empty(), "the refusal is reported");
    assert_eq!(refreshes(&srv), before, "not retried");
    assert!(contact_named(&s, contacts, "Pat").is_some(), "the contacts folder was updated before");
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert_eq!(account_error(&s), "", "{:?}", s.sync_status());
    assert_eq!(contacts_here(&s, contacts), 1);
}

/// `url` with every `%XX` escape decoded, as an earlier version stored links.
fn decoded(url: &str) -> String {
    let b = url.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) =
                b.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// Rewrites what the account stored for `folder` (collection, known resources, the items' links) the way an
/// earlier version stored it: every path fully decoded.
fn store_like_an_earlier_version(s: &mut Session, account: AccountId, folder: FolderId) {
    let a = s.mailbox.accounts.iter_mut().find(|a| a.id == account).unwrap();
    let st = a.sync.dav.get_mut(&folder).unwrap();
    st.collection = decoded(&st.collection);
    st.known = st.known.iter().map(|(u, e)| (decoded(u), e.clone())).collect();
    for c in s.mailbox.contacts.values_mut().filter(|c| c.folder == folder) {
        if let Some(r) = c.remote.as_mut() {
            r.url = decoded(&r.url);
        }
    }
}

const SAM: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:sam-2\r\nFN:Sam Plus\r\nN:Plus;Sam;;;\r\nEND:VCARD\r\n";

#[test]
fn links_stored_by_an_earlier_version_still_match_after_the_upgrade() {
    // Regression: an earlier version stored links with their paths decoded. Read back, a name with a space
    // or `+` (`%2B` on the server) and an address book with `#` in its name matched nothing on the server:
    // every unchanged contact looked deleted there and was removed here (local-only details lost), then
    // imported again as a new contact.
    let srv = servers();
    let mut s = session();
    // Sorted first, so the Contacts folder takes it.
    srv.add_dav_collection(ALICE, "My Book #1", false, "My Book");
    srv.put_dav_item(ALICE, "My Book #1", "John Smith.vcf", PAT).unwrap();
    srv.put_dav_item(ALICE, "My Book #1", "a+b.vcf", SAM).unwrap();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "tasks": false, "calendar": false}));
    let contacts = role(&s, alice, FolderRole::Contacts);
    update_folder(&mut s, contacts);
    let collection = s.mailbox.account(alice).unwrap().sync.dav[&contacts].collection.clone();
    assert!(collection.ends_with("/My%20Book%20%231/"), "{collection}");
    let ids: Vec<ItemId> =
        s.mailbox.contacts.values().filter(|c| c.folder == contacts).map(|c| c.id).collect();
    assert_eq!(ids.len(), 2);
    let pat = contact_named(&s, contacts, "Pat").unwrap().id;
    s.mailbox.contacts.get_mut(&pat).unwrap().flag = mailcraft_engine::model::FlagState::Flagged {
        label: "Follow up".into(),
        start: None,
        due: None,
        reminder: None,
    };
    let canonical: Vec<String> =
        ids.iter().map(|id| s.mailbox.contacts[id].remote.as_ref().unwrap().url.clone()).collect();

    for full in [false, true] {
        store_like_an_earlier_version(&mut s, alice, contacts);
        assert!(s.mailbox.contacts[&pat].remote.as_ref().unwrap().url.contains("#1/"));
        let before = srv.dav_requests().len();
        if full {
            run(&mut s, "sendReceive.all", json!({}));
            settle(&mut s);
            assert_eq!(account_error(&s), "", "{:?}", s.sync_status());
        } else {
            update_folder(&mut s, contacts);
        }
        let now: Vec<ItemId> =
            s.mailbox.contacts.values().filter(|c| c.folder == contacts).map(|c| c.id).collect();
        assert_eq!(now, ids, "the same contacts, none removed or imported again (full run: {full})");
        assert!(matches!(s.mailbox.contacts[&pat].flag, mailcraft_engine::model::FlagState::Flagged { .. }));
        let sent: Vec<String> = srv.dav_requests()[before..].to_vec();
        assert!(sent.iter().all(|r| !r.starts_with("PUT") && !r.starts_with("DELETE")), "{sent:?}");
        // The links take the server's form again.
        let links: Vec<String> =
            ids.iter().map(|id| s.mailbox.contacts[id].remote.as_ref().unwrap().url.clone()).collect();
        assert_eq!(links, canonical);
        assert_eq!(s.mailbox.account(alice).unwrap().sync.dav[&contacts].collection, collection);
    }
    assert_eq!(srv.dav_items(ALICE, "My Book #1").len(), 2);
}

#[test]
fn a_collection_the_server_moved_keeps_its_items() {
    // Regression: a collection whose listing was redirected listed nothing (its members are under the new
    // path), so every item looked deleted on the server and was removed here.
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "tasks": false, "calendar": false}));
    let contacts = role(&s, alice, FolderRole::Contacts);
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", PAT).unwrap();
    srv.put_dav_item(ALICE, "contacts", "lee-1.vcf", LEE).unwrap();
    update_folder(&mut s, contacts);
    let ids: Vec<ItemId> =
        s.mailbox.contacts.values().filter(|c| c.folder == contacts).map(|c| c.id).collect();
    assert_eq!(ids.len(), 2);
    assert!(srv.move_dav_collection(ALICE, "contacts", "address-book"));
    let before = srv.dav_requests().len();
    update_folder(&mut s, contacts);
    let now: Vec<ItemId> =
        s.mailbox.contacts.values().filter(|c| c.folder == contacts).map(|c| c.id).collect();
    assert_eq!(now, ids);
    let sent: Vec<String> = srv.dav_requests()[before..].to_vec();
    assert!(sent.iter().all(|r| !r.starts_with("PUT") && !r.starts_with("DELETE")), "{sent:?}");
    assert!(s.mailbox.account(alice).unwrap().sync.dav[&contacts].collection.ends_with("/address-book/"));
    for id in &ids {
        let url = &s.mailbox.contacts[id].remote.as_ref().unwrap().url;
        assert!(url.contains("/address-book/"), "{url}");
    }
    // A full run finds it at its new address too.
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert_eq!(account_error(&s), "", "{:?}", s.sync_status());
    let now: Vec<ItemId> =
        s.mailbox.contacts.values().filter(|c| c.folder == contacts).map(|c| c.id).collect();
    assert_eq!(now, ids);
    assert_eq!(srv.dav_items(ALICE, "address-book").len(), 2);
}

#[test]
fn an_incomplete_listing_removes_nothing_here() {
    // A listing with responses that are not members of the collection may be missing members: an item it
    // does not show is not taken as deleted on the server that time.
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "tasks": false, "calendar": false}));
    let contacts = role(&s, alice, FolderRole::Contacts);
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", PAT).unwrap();
    srv.put_dav_item(ALICE, "contacts", "lee-1.vcf", LEE).unwrap();
    update_folder(&mut s, contacts);
    assert_eq!(contacts_here(&s, contacts), 2);
    srv.delete_dav_item(ALICE, "contacts", "lee-1.vcf");
    srv.set_stray_dav_href(Some("/elsewhere/x.vcf"));
    update_folder(&mut s, contacts);
    assert_eq!(contacts_here(&s, contacts), 2, "kept while the listing is incomplete");
    srv.set_stray_dav_href(None);
    update_folder(&mut s, contacts);
    assert_eq!(contacts_here(&s, contacts), 1, "gone once the listing is complete");
    assert!(contact_named(&s, contacts, "Lee").is_none());
}

const OTHER_TODO: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VTODO\r\nUID:other-9\r\nSUMMARY:From the phone\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";

#[test]
fn an_item_whose_address_another_item_took_goes_up_under_another_name() {
    // Regression: when another device's item appeared at a new item's address during the run, that copy was
    // remembered as known without being imported (deleting the item here later removed the other device's
    // item from the server), and the item here retried the same address on every run.
    let srv = servers();
    let mut s = session();
    let alice = add_alice(&mut s, &srv, json!({"url": srv.dav_url(), "contacts": false, "calendar": false}));
    let tasks = role(&s, alice, FolderRole::Tasks);
    let mut ids = Vec::new();
    for (uid, subject) in [("first-1", "First"), ("dup-1", "Mine")] {
        let id = ItemId(s.mailbox.alloc_id());
        s.mailbox.tasks.insert(
            id,
            Task { id, folder: tasks, uid: uid.into(), subject: subject.into(), ..Default::default() },
        );
        ids.push(id);
    }
    // The first upload lets the phone's task in at the second one's address.
    srv.put_dav_item_after_next_put(ALICE, "tasks", "dup-1.ics", OTHER_TODO);
    update_folder(&mut s, tasks);
    let names = |srv: &TestServers| {
        let mut n: Vec<String> = srv.dav_items(ALICE, "tasks").into_iter().map(|(n, _)| n).collect();
        n.sort();
        n
    };
    let on_server = names(&srv);
    assert_eq!(on_server.len(), 3, "{on_server:?}");
    assert!(srv.dav_items(ALICE, "tasks").iter().any(|(n, d)| n == "dup-1.ics" && d.contains("other-9")));
    let mine = s.mailbox.tasks[&ids[1]].remote.clone().expect("the task here went up under another name");
    assert!(!mine.url.ends_with("/dup-1.ics"), "{}", mine.url);
    assert!(
        s.mailbox.tasks.values().any(|t| t.folder == tasks && t.subject == "From the phone"),
        "the phone's task arrived"
    );
    let before = puts(&srv);
    update_folder(&mut s, tasks);
    assert_eq!(puts(&srv), before, "nothing retried");
    // Deleting the task here removes its own copy only.
    s.mailbox.tasks.remove(&ids[1]);
    update_folder(&mut s, tasks);
    let left = names(&srv);
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left.iter().any(|n| n == "dup-1.ics"), "the phone's task stays: {left:?}");
}

#[test]
fn a_withdrawal_saved_by_an_earlier_version_removes_its_file() {
    // Regression: a pending removal saved by an earlier version (no ETag) was dropped without removing the
    // file, and one outside the publishing location was dropped silently while the file stayed public.
    use mailcraft_engine::cmd::calendar::props;
    use mailcraft_engine::model::calendar::Withdrawal;
    let srv = servers();
    let mut s = session();
    let public = srv.dav_collection_url(ALICE, "public");
    let alice = add_alice(
        &mut s,
        &srv,
        json!({"url": srv.dav_url(), "publishUrl": public, "contacts": false, "tasks": false, "calendar": false}),
    );
    let cal = role(&s, alice, FolderRole::Calendar);
    srv.put_dav_item(ALICE, "public", "old.ics", "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();
    srv.put_dav_item(ALICE, "calendar", "elsewhere.ics", "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();
    let mut p = props::read(&s.mailbox, cal);
    // As an earlier version's entries read back (see the model: no ETag field reads as `*`).
    for url in [format!("{public}old.ics"), srv.dav_collection_url(ALICE, "calendar") + "elsewhere.ics"] {
        p.withdrawals.push(Withdrawal { url, account: alice, etag: "*".into() });
    }
    props::write(&mut s.mailbox, cal, &p);
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert!(srv.dav_items(ALICE, "public").is_empty(), "the file inside the publishing location is removed");
    // Outside it: left in place, but reported, and kept to look at again.
    assert!(account_error(&s).contains("elsewhere.ics"), "{}", account_error(&s));
    assert_eq!(srv.dav_items(ALICE, "calendar").len(), 1);
    let left = props::read(&s.mailbox, cal).withdrawals;
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].url.ends_with("elsewhere.ics"));
    // Removed by hand: the next run sees it gone and forgets it.
    srv.delete_dav_item(ALICE, "calendar", "elsewhere.ics");
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert_eq!(account_error(&s), "", "{:?}", s.sync_status());
    assert!(props::read(&s.mailbox, cal).withdrawals.is_empty());
}

#[test]
fn the_mail_token_goes_only_to_the_providers_dav_server_and_is_renewed_for_it() {
    // Regressions: the DAV part kept the access token the job was built with after the run renewed it (every
    // DAV request then failed to sign in), and the token went to any DAV server, the provider's or not.
    let srv = servers();
    let mut s = session();
    s.sync.oauth_http = Some(mailcraft_engine::sync::ConnectOptions::trusting(srv.cert_der.clone()));
    let r = run(
        &mut s,
        "accounts.add",
        json!({
            "kind": "imap",
            "address": ALICE,
            "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
            "dav": {"url": srv.dav_url(), "tasks": false, "calendar": false, "extraRootPem": srv.cert_pem},
        }),
    );
    let alice = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    let (_, token, device) = srv.oauth_endpoints();
    run(
        &mut s,
        "accounts.oauthSignIn",
        json!({"account": alice.0, "provider": "custom", "clientId": mailcraft_testmail::OAUTH_TEST_CLIENT,
               "flow": "device", "tokenEndpoint": token, "deviceAuthorizationEndpoint": device, "scopes": ["mail"]}),
    );
    let mut code = String::new();
    poll_until(&mut s, "the device code", |s| {
        code = s.run("accounts.oauthStatus", &json!({"account": alice.0})).unwrap()["userCode"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        !code.is_empty()
    });
    srv.oauth_approve(&code, ALICE).unwrap();
    poll_until(&mut s, "sign-in", |s| {
        s.run("accounts.oauthStatus", &json!({"account": alice.0})).unwrap()["status"] == "done"
    });
    let contacts = role(&s, alice, FolderRole::Contacts);
    srv.put_dav_item(ALICE, "contacts", "pat-1.vcf", PAT).unwrap();
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert!(contact_named(&s, contacts, "Pat").is_some(), "{:?}", s.sync_status());
    // Every access token expires: the run renews it at the mail sign-in, and the DAV part uses the new one.
    srv.oauth_expire_access_tokens();
    srv.put_dav_item(ALICE, "contacts", "lee-1.vcf", LEE).unwrap();
    run(&mut s, "sendReceive.all", json!({}));
    settle(&mut s);
    assert_eq!(account_error(&s), "", "{:?}", s.sync_status());
    assert!(contact_named(&s, contacts, "Lee").is_some());

    // A DAV server that is not the provider's: no token for it, it needs a password of its own.
    let other = srv.dav_url().replace("127.0.0.1", "localhost");
    run(&mut s, "accounts.edit", json!({"account": alice.0, "dav": {"url": other}}));
    let before = srv.dav_requests().len();
    run(&mut s, "sendReceive.folder", json!({"folder": contacts.0}));
    settle(&mut s);
    assert!(account_error(&s).contains("password of its own"), "{}", account_error(&s));
    assert!(srv.dav_requests().len() > before, "the server was asked, without the token");
    run(&mut s, "accounts.edit", json!({"account": alice.0, "dav": {"password": ALICE_PW}}));
    let acct = run(&mut s, "accounts.list", json!({}))["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == alice.0)
        .cloned()
        .unwrap();
    assert_eq!(acct["hasDavPassword"], true, "{acct}");
    update_folder(&mut s, contacts);
    assert!(contact_named(&s, contacts, "Lee").is_some());
}
