//! End to end, Microsoft Graph contacts, calendars and To Do tasks: an account on the local test server's fake
//! Graph service (HTTPS, 127.0.0.1, synthetic users; nothing reaches a real Microsoft service): contact
//! folders and contacts, To Do lists and tasks, calendars with series, exceptions, cancelled occurrences and
//! time zones, both ways and from delta links; meetings scheduled by the service (invitations, updates,
//! cancellations, answers through the event actions, no iTIP mail from MailCraft); other people's calendars
//! below Shared Calendars.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, DateTime, FolderId, FolderKind, FolderRole, ItemId, Mailbox};
use mailcraft_net::graph::GraphClient;
use mailcraft_testmail::{Config, TestServers};
use serde_json::{Value, json};

const ALICE: &str = "alice@example.com";
const ALICE_PW: &str = "alice-graph-pw";
const BOB: &str = "bob@example.org";
const BOB_PW: &str = "bob-pw";
const CAROL: &str = "carol@example.net";
const CAROL_PW: &str = "carol-pw";
const WAIT: Duration = Duration::from_secs(30);

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn errors(s: &Session) -> Vec<String> {
    s.sync_status().accounts.iter().filter_map(|x| x.error.clone()).collect()
}

fn settle(s: &mut Session) {
    assert!(s.wait_for_sync(WAIT), "timed out: {:?}", s.sync_status());
    assert!(errors(s).is_empty(), "{:?}", errors(s));
}

fn sync(s: &mut Session) {
    run(s, "sendReceive.all", json!({}));
    settle(s);
}

fn update_folder(s: &mut Session, f: FolderId) -> Value {
    let r = run(s, "sendReceive.folder", json!({"folder": f.0}));
    settle(s);
    r
}

fn servers() -> TestServers {
    TestServers::start(Config {
        users: vec![
            (ALICE.into(), ALICE_PW.into()),
            (BOB.into(), BOB_PW.into()),
            (CAROL.into(), CAROL_PW.into()),
        ],
        add_trace_headers: false,
        graph_max_page: 2,
        ..Config::default()
    })
    .unwrap()
}

fn session_with_alice(srv: &TestServers) -> (Session, AccountId) {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let r = run(
        &mut s,
        "accounts.add",
        json!({"kind": "graph", "address": ALICE, "incoming": {"host": srv.graph_url(), "extraRootPem": srv.cert_pem},
               "password": ALICE_PW}),
    );
    (s, AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap()))
}

/// Another client of `user`'s mailbox (the test's own Graph client).
fn client(srv: &TestServers, pw: &str) -> GraphClient {
    let opts = mailcraft_net::ConnectOptions::trusting(srv.cert_der.clone());
    GraphClient::new(&srv.graph_url(), pw, mailcraft_net::http::HttpClient::new(opts)).unwrap()
}

fn at(y: i32, mo: u8, d: u8, h: u8) -> i64 {
    DateTime::from_civil(y, mo, d, h, 0, 0, 0).unwrap().unix
}

fn requests_since(srv: &TestServers, n: usize) -> Vec<String> {
    srv.graph_requests().into_iter().skip(n).collect()
}

fn objects_with(srv: &TestServers, user: &str, kind: &str, key: &str, value: &str) -> Vec<Value> {
    srv.graph_objects(user, kind).into_iter().filter(|v| v[key] == value).collect()
}

#[test]
fn graph_contacts_synchronise_both_ways_from_delta_links() {
    let srv = servers();
    let other = client(&srv, ALICE_PW);
    let pat = other
        .post_json(
            "/me/contacts",
            &json!({"givenName": "Pat", "surname": "Example", "companyName": "Example Corp",
                    "emailAddresses": [{"address": "pat@example.net", "name": "Pat Example"}],
                    "businessPhones": ["+1 555 0100"], "mobilePhone": "+1 555 0101",
                    "birthday": "1990-05-01T00:00:00Z", "categories": ["Work"],
                    "homeAddress": {"street": "1 Main St", "city": "Springfield", "postalCode": "12345"}}),
        )
        .unwrap();
    let pat_id = pat["id"].as_str().unwrap().to_owned();
    let team = other.post_json("/me/contactFolders", &json!({"displayName": "Team"})).unwrap();
    let team_id = team["id"].as_str().unwrap().to_owned();
    other
        .post_json(
            &format!("/me/contactFolders/{team_id}/contacts"),
            &json!({"givenName": "Sam", "surname": "Sample"}),
        )
        .unwrap();
    let (mut s, a) = session_with_alice(&srv);
    sync(&mut s);
    sync(&mut s);
    let contacts = s.mailbox.role_folder(a, FolderRole::Contacts).unwrap();
    let here = s.mailbox.contacts.values().find(|c| c.first_name == "Pat").expect("the service's contact");
    assert_eq!(here.folder, contacts);
    assert_eq!(here.emails, vec!["pat@example.net".to_owned()]);
    assert_eq!(here.company, "Example Corp");
    assert_eq!(here.birthday.as_deref(), Some("1990-05-01"));
    assert!(here.phones.contains(&("Mobile".to_owned(), "+1 555 0101".to_owned())), "{:?}", here.phones);
    assert_eq!(here.addresses.first().map(|x| x.city.as_str()), Some("Springfield"));
    let pat_here = here.id;
    let team_folder = s
        .mailbox
        .folders
        .values()
        .find(|f| f.account == a && f.kind == FolderKind::Contacts && f.name == "Team")
        .expect("the Team folder")
        .id;
    let sam = s.mailbox.contacts.values().find(|c| c.first_name == "Sam").expect("Team's contact");
    assert_eq!(sam.folder, team_folder);
    assert_eq!(update_folder(&mut s, contacts)["server"], "graph");

    // New and changed here: made and changed there.
    let r = run(
        &mut s,
        "people.new",
        json!({"firstName": "Robin", "lastName": "Sample", "email": "robin@example.org", "folder": contacts.0}),
    );
    let robin = ItemId(r["id"].as_u64().unwrap());
    run(&mut s, "people.update", json!({"id": pat_here.0, "jobTitle": "Engineer"}));
    let n = srv.graph_requests().len();
    update_folder(&mut s, contacts);
    let up = objects_with(&srv, ALICE, "contacts", "givenName", "Robin");
    assert_eq!(up.len(), 1, "{up:?}");
    assert_eq!(up[0]["emailAddresses"][0]["address"], "robin@example.org");
    assert_eq!(up[0]["_collection"], "Contacts");
    let robin_ref = s.mailbox.contacts[&robin].remote.clone().expect("linked");
    assert!(robin_ref.url.ends_with(up[0]["id"].as_str().unwrap()), "addressed by its id: {}", robin_ref.url);
    let pat_there = objects_with(&srv, ALICE, "contacts", "givenName", "Pat");
    assert_eq!(pat_there[0]["jobTitle"], "Engineer");
    assert_eq!(pat_there[0]["companyName"], "Example Corp", "what the edit did not touch stays");
    let reqs = requests_since(&srv, n);
    assert!(reqs.iter().any(|r| r.contains("/contacts/delta") && r.contains("deltatoken")), "{reqs:?}");
    assert!(reqs.iter().any(|r| r.starts_with("PATCH") && r.contains(&pat_id)), "{reqs:?}");
    let patches = reqs.iter().filter(|r| r.starts_with("PATCH")).count();
    assert_eq!(patches, 1, "only the changed contact: {reqs:?}");

    // A run without changes writes nothing and lists nothing whole.
    let n = srv.graph_requests().len();
    update_folder(&mut s, contacts);
    let reqs = requests_since(&srv, n);
    assert!(
        !reqs.iter().any(|r| r.starts_with("PATCH") || r.starts_with("POST") || r.starts_with("DELETE")),
        "{reqs:?}"
    );
    assert_eq!(s.mailbox.contacts.values().filter(|c| c.folder == contacts).count(), 2);

    // Changed there: arrives. Deleted there: removed here. Deleted here: deleted there.
    other.patch_json(&format!("/me/contacts/{pat_id}"), &json!({"officeLocation": "Building 4"})).unwrap();
    let sam_there = objects_with(&srv, ALICE, "contacts", "givenName", "Sam");
    other.delete(&format!("/me/contacts/{}", sam_there[0]["id"].as_str().unwrap())).unwrap();
    run(&mut s, "people.delete", json!({"id": robin.0}));
    sync(&mut s);
    assert_eq!(s.mailbox.contacts[&pat_here].office, "Building 4");
    assert_eq!(s.mailbox.contacts[&pat_here].job_title, "Engineer");
    assert!(!s.mailbox.contacts.values().any(|c| c.first_name == "Sam"));
    assert!(objects_with(&srv, ALICE, "contacts", "givenName", "Robin").is_empty());

    // Changed on both sides: this copy wins, nothing is lost silently.
    other.patch_json(&format!("/me/contacts/{pat_id}"), &json!({"department": "Research"})).unwrap();
    run(&mut s, "people.update", json!({"id": pat_here.0, "jobTitle": "Lead Engineer"}));
    sync(&mut s);
    sync(&mut s);
    let pat_there = objects_with(&srv, ALICE, "contacts", "givenName", "Pat");
    assert_eq!(pat_there[0]["jobTitle"], "Lead Engineer");
    assert_eq!(s.mailbox.contacts[&pat_here].job_title, "Lead Engineer");

    // Deleted there while changed here: it goes up again, as a new contact.
    other.delete(&format!("/me/contacts/{pat_id}")).unwrap();
    run(&mut s, "people.update", json!({"id": pat_here.0, "jobTitle": "Principal"}));
    sync(&mut s);
    let again = objects_with(&srv, ALICE, "contacts", "givenName", "Pat");
    assert_eq!(again.len(), 1, "{again:?}");
    assert_eq!(again[0]["jobTitle"], "Principal");
    assert_ne!(again[0]["id"].as_str(), Some(pat_id.as_str()));

    // The service forgets every delta state: the folders are listed whole again, without duplicates.
    srv.graph_expire_deltas();
    sync(&mut s);
    assert_eq!(s.mailbox.contacts.values().filter(|c| c.folder == contacts).count(), 1);
    assert_eq!(srv.graph_objects(ALICE, "contacts").len(), 1);
}

#[test]
fn a_sign_in_without_the_pim_permissions_keeps_mail_working() {
    let srv = servers();
    srv.graph_deny_pim(true);
    srv.seed_message(
        ALICE,
        "INBOX",
        b"From: Bob <bob@example.org>\r\nTo: alice@example.com\r\nSubject: Still here\r\nMessage-ID: <s1@example.org>\r\n\r\nHi.\r\n",
        &[],
    )
    .unwrap();
    let other = client(&srv, ALICE_PW);
    let (mut s, a) = session_with_alice(&srv);
    run(&mut s, "sendReceive.all", json!({}));
    assert!(s.wait_for_sync(WAIT));
    let errs = errors(&s);
    assert!(errs.iter().any(|e| e.contains("sign in to the account again")), "{errs:?}");
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    assert!(
        s.mailbox.mail.items().any(|m| m.folder == inbox && m.subject == "Still here"),
        "mail still arrives"
    );
    // Signed in again with the permissions: the rest follows.
    srv.graph_deny_pim(false);
    other.post_json("/me/contacts", &json!({"givenName": "Pat", "surname": "Example"})).unwrap();
    sync(&mut s);
    sync(&mut s);
    assert!(s.mailbox.contacts.values().any(|c| c.first_name == "Pat"));
}

#[test]
fn graph_todo_lists_synchronise_with_task_folders() {
    let srv = servers();
    let other = client(&srv, ALICE_PW);
    let lists = other.get_all("/me/todo/lists", 10).unwrap();
    let default = lists.iter().find(|l| l["wellknownListName"] == "defaultList").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let toner = other
        .post_json(
            &format!("/me/todo/lists/{default}/tasks"),
            &json!({"title": "Order toner", "importance": "high",
                    "dueDateTime": {"dateTime": "2027-02-10T00:00:00.0000000", "timeZone": "UTC"},
                    "body": {"contentType": "text", "content": "Two cartridges"}}),
        )
        .unwrap();
    let toner_id = toner["id"].as_str().unwrap().to_owned();
    let errands = other.post_json("/me/todo/lists", &json!({"displayName": "Errands"})).unwrap();
    let errands_id = errands["id"].as_str().unwrap().to_owned();
    other.post_json(&format!("/me/todo/lists/{errands_id}/tasks"), &json!({"title": "Post office"})).unwrap();
    let (mut s, a) = session_with_alice(&srv);
    sync(&mut s);
    sync(&mut s);
    let tasks = s.mailbox.role_folder(a, FolderRole::Tasks).unwrap();
    let t = s.mailbox.tasks.values().find(|t| t.subject == "Order toner").expect("the service's task");
    assert_eq!(t.folder, tasks);
    assert_eq!(t.priority, mailcraft_engine::model::task::TaskPriority::High);
    assert_eq!(t.body.trim(), "Two cartridges");
    assert!(t.due.is_some());
    let toner_here = t.id;
    let errands_folder = s
        .mailbox
        .folders
        .values()
        .find(|f| f.account == a && f.kind == FolderKind::Tasks && f.name == "Errands")
        .unwrap()
        .id;
    assert!(s.mailbox.tasks.values().any(|t| t.subject == "Post office" && t.folder == errands_folder));

    // New here: made there with its importance; completed there: complete here.
    let r = run(
        &mut s,
        "tasks.new",
        json!({"subject": "Book the room", "folder": tasks.0, "priority": "low", "due": "2027-02-12"}),
    );
    let book = ItemId(r["id"].as_u64().unwrap());
    other
        .patch_json(
            &format!("/me/todo/lists/{default}/tasks/{toner_id}"),
            &json!({"status": "completed",
        "completedDateTime": {"dateTime": "2027-02-01T10:00:00.0000000", "timeZone": "UTC"}}),
        )
        .unwrap();
    sync(&mut s);
    let up = objects_with(&srv, ALICE, "tasks", "title", "Book the room");
    assert_eq!(up.len(), 1, "{up:?}");
    assert_eq!((up[0]["importance"].as_str(), up[0]["_collection"].as_str()), (Some("low"), Some("Tasks")));
    assert!(s.mailbox.tasks[&book].remote.is_some());
    assert!(s.mailbox.tasks[&toner_here].is_complete(), "{:?}", s.mailbox.tasks[&toner_here]);

    // Deleted here: deleted there; a second run changes nothing.
    run(&mut s, "tasks.delete", json!({"id": book.0}));
    sync(&mut s);
    assert!(objects_with(&srv, ALICE, "tasks", "title", "Book the room").is_empty());
    let n = srv.graph_requests().len();
    sync(&mut s);
    let writes = requests_since(&srv, n)
        .into_iter()
        .filter(|r| {
            r.contains("/todo/")
                && (r.starts_with("PATCH") || r.starts_with("POST") || r.starts_with("DELETE"))
        })
        .count();
    assert_eq!(writes, 0);
}

fn calendar_items(s: &Session, folder: FolderId) -> Vec<&mailcraft_engine::model::Appointment> {
    s.mailbox.appointments.values().filter(|a| a.folder == folder).collect()
}

#[test]
fn graph_calendar_series_exceptions_and_zones_both_ways() {
    let srv = servers();
    let other = client(&srv, ALICE_PW);
    let series = other
        .post_json(
            "/me/calendar/events",
            &json!({"subject": "Planning",
                    "start": {"dateTime": "2027-03-01T09:00:00", "timeZone": "Eastern Standard Time"},
                    "end": {"dateTime": "2027-03-01T10:00:00", "timeZone": "Eastern Standard Time"},
                    "isReminderOn": true, "reminderMinutesBeforeStart": 30, "categories": ["Work"],
                    "recurrence": {"pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["monday"],
                                               "firstDayOfWeek": "sunday"},
                                   "range": {"type": "numbered", "startDate": "2027-03-01", "numberOfOccurrences": 4}}}),
        )
        .unwrap();
    let sid = series["id"].as_str().unwrap().to_owned();
    // One occurrence moved, another cancelled (daylight saving time starts on 14 March).
    other
        .patch_json(
            &format!("/me/events/{sid}_occ_{}", at(2027, 3, 8, 14)),
            &json!({"subject": "Planning (moved)",
                    "start": {"dateTime": "2027-03-08T11:00:00", "timeZone": "Eastern Standard Time"},
                    "end": {"dateTime": "2027-03-08T12:00:00", "timeZone": "Eastern Standard Time"}}),
        )
        .unwrap();
    other.delete(&format!("/me/events/{sid}_occ_{}", at(2027, 3, 22, 13))).unwrap();
    let (mut s, a) = session_with_alice(&srv);
    sync(&mut s);
    sync(&mut s);
    let cal = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    let here = calendar_items(&s, cal);
    assert_eq!(here.len(), 2, "the series and its exception: {here:?}");
    let master = here.iter().find(|x| x.recurrence.is_some()).unwrap();
    assert_eq!(master.start.unix, at(2027, 3, 1, 14));
    assert_eq!(master.tz.as_deref(), Some("America/New_York"));
    assert_eq!(master.reminder_min, Some(30));
    let rec = master.recurrence.as_ref().unwrap();
    assert_eq!(rec.rrule, "FREQ=WEEKLY;BYDAY=MO;COUNT=4");
    // The cancelled occurrence is an exception date (and so is the moved one's original time, as for every
    // exception here).
    assert!(rec.exdates.iter().any(|d| d.unix == at(2027, 3, 22, 13)), "{rec:?}");
    assert!(
        !rec.exdates.iter().any(|d| d.unix == at(2027, 3, 15, 13) || d.unix == at(2027, 3, 1, 14)),
        "{rec:?}"
    );
    let moved = here.iter().find(|x| x.recurrence_id.is_some()).unwrap();
    assert_eq!(moved.subject, "Planning (moved)");
    assert_eq!(moved.recurrence_id.map(|r| r.unix), Some(at(2027, 3, 8, 14)));
    assert_eq!(moved.start.unix, at(2027, 3, 8, 16));
    assert_eq!(moved.uid, master.uid);
    let master_id = master.id;

    // Changed here: the series changes there, its exception stays; one more occurrence edited here becomes
    // an exception there.
    run(&mut s, "calendar.update", json!({"id": master_id.0, "location": "Room 2"}));
    run(
        &mut s,
        "calendar.editOccurrence",
        json!({"id": master_id.0, "occurrence": at(2027, 3, 15, 13), "subject": "Planning (short)"}),
    );
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": cal.0, "subject": "Dentist", "start": at(2027, 3, 3, 15), "end": at(2027, 3, 3, 16),
               "importance": "high"}),
    );
    let dentist = ItemId(r["id"].as_u64().unwrap());
    update_folder(&mut s, cal);
    let events = srv.graph_objects(ALICE, "events");
    let m = events.iter().find(|e| e["id"] == sid.as_str()).unwrap();
    assert_eq!(m["location"]["displayName"], "Room 2");
    let exceptions: Vec<&Value> = events.iter().filter(|e| e["seriesMasterId"] == sid.as_str()).collect();
    assert_eq!(exceptions.len(), 2, "{exceptions:?}");
    assert!(exceptions.iter().any(|e| e["subject"] == "Planning (short)"));
    assert!(exceptions.iter().any(|e| e["subject"] == "Planning (moved)"));
    let d = events.iter().find(|e| e["subject"] == "Dentist").expect("the new appointment");
    assert_eq!(d["importance"], "high");
    assert_eq!(d["start"]["dateTime"], "2027-03-03T15:00:00.0000000", "{d}");
    assert!(s.mailbox.appointments[&dentist].remote.is_some());
    assert_eq!(calendar_items(&s, cal).len(), 4, "no duplicates after the uploads");

    // A run without changes writes nothing and reads the calendar from its delta link.
    let n = srv.graph_requests().len();
    update_folder(&mut s, cal);
    let reqs = requests_since(&srv, n);
    assert!(reqs.iter().any(|r| r.contains("calendarView/delta") && r.contains("deltatoken")), "{reqs:?}");
    assert!(
        !reqs.iter().any(|r| r.starts_with("PATCH") || r.starts_with("POST") || r.starts_with("DELETE")),
        "{reqs:?}"
    );
    assert!(!reqs.iter().any(|r| r.contains("/events?")), "not listed whole: {reqs:?}");

    // Cancelled there: an exception date here. The series deleted there: gone here with its exceptions.
    other.delete(&format!("/me/events/{sid}_occ_{}", at(2027, 3, 1, 14))).unwrap();
    update_folder(&mut s, cal);
    let master = &s.mailbox.appointments[&master_id];
    assert!(
        master.recurrence.as_ref().unwrap().exdates.iter().any(|d| d.unix == at(2027, 3, 1, 14)),
        "{master:?}"
    );
    assert_eq!(calendar_items(&s, cal).len(), 4, "the exceptions stay");
    other.delete(&format!("/me/events/{sid}")).unwrap();
    update_folder(&mut s, cal);
    let left: Vec<String> = calendar_items(&s, cal).iter().map(|x| x.subject.clone()).collect();
    assert_eq!(left, vec!["Dentist".to_owned()]);

    // Deleted here: deleted there.
    run(&mut s, "calendar.delete", json!({"id": dentist.0}));
    update_folder(&mut s, cal);
    assert!(srv.graph_objects(ALICE, "events").is_empty());
}

#[test]
fn graph_meetings_are_scheduled_by_the_service() {
    let srv = servers();
    let (mut s, a) = session_with_alice(&srv);
    sync(&mut s);
    let cal = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    let n = srv.graph_requests().len();
    let r = run(
        &mut s,
        "calendar.newMeeting",
        json!({"folder": cal.0, "subject": "Kickoff", "start": at(2027, 4, 5, 15), "end": at(2027, 4, 5, 16),
               "attendees": BOB, "form": false, "send": "now"}),
    );
    assert_eq!((r["sent"].as_bool(), r["via"].as_str()), (Some(true), Some("service")), "{r}");
    assert!(r.get("compose").is_none_or(Value::is_null), "no invitation mail: {r}");
    let meeting = ItemId(r["id"].as_u64().unwrap());
    settle(&mut s);
    sync(&mut s);
    let inv = srv.graph_invitations();
    assert!(
        inv.iter().any(|i| i["attendee"] == BOB && i["kind"] == "request" && i["subject"] == "Kickoff"),
        "{inv:?}"
    );
    let reqs = requests_since(&srv, n);
    assert!(!reqs.iter().any(|r| r.contains("sendMail")), "no iTIP mail went from MailCraft: {reqs:?}");
    assert!(
        srv.messages(BOB, "INBOX")
            .unwrap()
            .iter()
            .any(|m| String::from_utf8_lossy(&m.raw).contains("METHOD:REQUEST"))
    );

    // Bob accepts on the service: the organizer's copy here records it.
    let bob = client(&srv, BOB_PW);
    let his = objects_with(&srv, BOB, "events", "subject", "Kickoff");
    assert_eq!(his.len(), 1, "{his:?}");
    bob.post_json(
        &format!("/me/events/{}/accept", his[0]["id"].as_str().unwrap()),
        &json!({"sendResponse": true}),
    )
    .unwrap();
    sync(&mut s);
    let t = run(&mut s, "calendar.tracking", json!({"id": meeting.0}));
    assert_eq!(t["accepted"], 1, "{t}");

    // Changed and cancelled here: the service sends the update and the cancellation.
    run(&mut s, "calendar.update", json!({"id": meeting.0, "location": "Room 7"}));
    let r = run(&mut s, "calendar.sendUpdate", json!({"id": meeting.0, "send": "now"}));
    assert_eq!(r["via"], "service", "{r}");
    settle(&mut s);
    sync(&mut s);
    assert!(srv.graph_invitations().iter().any(|i| i["kind"] == "update" && i["attendee"] == BOB));
    let r = run(&mut s, "calendar.cancelMeeting", json!({"id": meeting.0, "send": "now"}));
    assert_eq!(r["via"], "service", "{r}");
    settle(&mut s);
    sync(&mut s);
    assert!(srv.graph_invitations().iter().any(|i| i["kind"] == "cancel" && i["attendee"] == BOB));
    assert!(objects_with(&srv, ALICE, "events", "subject", "Kickoff").is_empty());

    // Bob invites Alice: she answers through the service, which tells Bob; no reply mail goes from here.
    bob.post_json(
        "/me/calendar/events",
        &json!({"subject": "Design review",
                "start": {"dateTime": "2027-04-07T14:00:00", "timeZone": "UTC"},
                "end": {"dateTime": "2027-04-07T15:00:00", "timeZone": "UTC"},
                "attendees": [{"type": "required", "emailAddress": {"address": ALICE, "name": "Alice"}}]}),
    )
    .unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let invite = s
        .mailbox
        .mail
        .items()
        .find(|m| m.folder == inbox && m.subject == "Design review")
        .expect("the service's invitation")
        .id;
    let n = srv.graph_requests().len();
    let r = run(
        &mut s,
        "calendar.respond",
        json!({"id": invite.0, "response": "accept", "comment": "See you there"}),
    );
    assert_eq!(r["via"], "service", "{r}");
    settle(&mut s);
    sync(&mut s);
    let answers = srv.graph_responses();
    assert!(
        answers
            .iter()
            .any(|x| x["user"] == ALICE && x["action"] == "accept" && x["comment"] == "See you there"),
        "{answers:?}"
    );
    let reqs = requests_since(&srv, n);
    assert!(!reqs.iter().any(|r| r.contains("sendMail")), "{reqs:?}");
    let theirs = objects_with(&srv, BOB, "events", "subject", "Design review");
    assert_eq!(theirs[0]["attendees"][0]["status"]["response"], "accepted", "{theirs:?}");
    let mine = calendar_items(&s, cal).into_iter().filter(|x| x.subject == "Design review").count();
    assert_eq!(mine, 1, "the invitation's event, once");
    assert_eq!(
        objects_with(&srv, ALICE, "events", "subject", "Design review").len(),
        1,
        "nothing uploaded twice"
    );
    assert!(s.mailbox.account(a).unwrap().sync.graph_responses.is_empty(), "the answer went");
}

#[test]
fn graph_shared_calendars_open_below_shared_calendars() {
    let srv = servers();
    let bob = client(&srv, BOB_PW);
    bob.post_json(
        "/me/calendar/events",
        &json!({"subject": "Bob's day off", "isAllDay": true,
                "start": {"dateTime": "2027-05-03T00:00:00", "timeZone": "UTC"},
                "end": {"dateTime": "2027-05-04T00:00:00", "timeZone": "UTC"}}),
    )
    .unwrap();
    srv.graph_share_calendar(BOB, ALICE, false);
    srv.graph_share_calendar(CAROL, ALICE, true);
    let (mut s, a) = session_with_alice(&srv);
    // Opened by address (Open Shared Calendar) before the first run.
    let r = run(&mut s, "calendar.addCalendar", json!({"source": "shared", "address": BOB, "name": "Bob"}));
    let bob_folder = FolderId(r["folder"].as_u64().unwrap());
    settle(&mut s);
    sync(&mut s);
    sync(&mut s);
    let shared_root = s
        .mailbox
        .folders
        .values()
        .find(|f| f.account == a && f.name == "Shared Calendars")
        .expect("the Shared Calendars folder")
        .id;
    assert_eq!(s.mailbox.folder(bob_folder).unwrap().parent, Some(shared_root));
    let off = calendar_items(&s, bob_folder);
    assert_eq!(off.len(), 1, "{off:?}");
    assert!(off[0].all_day);
    assert_eq!(off[0].end.unix - off[0].start.unix, 86_400);
    // Bob lets Alice only read it: read-only here, and listed once although the service lists it too.
    assert!(s.run("calendar.newAppointment", &json!({"folder": bob_folder.0, "subject": "x", "start": at(2027, 5, 5, 10), "end": at(2027, 5, 5, 11)})).is_err());
    let bobs: Vec<_> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.parent == Some(shared_root))
        .map(|f| f.name.clone())
        .collect();
    assert_eq!(bobs.iter().filter(|n| n.contains("bob") || n.as_str() == "Bob").count(), 1, "{bobs:?}");
    // Carol's calendar, shared with editing: below Shared Calendars too, and changes go there.
    let carol_folder = s
        .mailbox
        .folders
        .values()
        .find(|f| f.parent == Some(shared_root) && f.name.contains("carol"))
        .unwrap_or_else(|| panic!("{bobs:?}"))
        .id;
    run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": carol_folder.0, "subject": "Covering for Carol", "start": at(2027, 5, 6, 10), "end": at(2027, 5, 6, 11)}),
    );
    update_folder(&mut s, carol_folder);
    assert_eq!(objects_with(&srv, CAROL, "events", "subject", "Covering for Carol").len(), 1);
    // Someone who shared nothing: the folder says why.
    let r = run(&mut s, "calendar.addCalendar", json!({"source": "shared", "address": "dana@example.com"}));
    assert!(r["folder"].as_u64().is_some());
    assert!(s.wait_for_sync(WAIT));
    let errs = errors(&s);
    assert!(errs.iter().any(|e| e.contains("dana") || e.contains("403") || e.contains("denied")), "{errs:?}");
    // The own address and junk are refused.
    assert!(s.run("calendar.addCalendar", &json!({"source": "shared", "address": ALICE})).is_err());
    assert!(
        s.run("calendar.addCalendar", &json!({"source": "shared", "address": "not an address"})).is_err()
    );
    // From Address Book without a person: the contacts to pick from.
    assert_eq!(
        run(&mut s, "calendar.addCalendar", json!({"source": "addressBook"}))["dialog"],
        "chooseContact"
    );
}

/// Bob's client invites Alice (and Carol) to `subject` on day `day` of April 2027 at 14:00 UTC.
fn bob_invites(bob: &GraphClient, subject: &str, day: u8) -> Value {
    bob.post_json(
        "/me/calendar/events",
        &json!({"subject": subject,
                "start": {"dateTime": format!("2027-04-{day:02}T14:00:00"), "timeZone": "UTC"},
                "end": {"dateTime": format!("2027-04-{day:02}T15:00:00"), "timeZone": "UTC"},
                "attendees": [{"type": "required", "emailAddress": {"address": ALICE, "name": "Alice"}},
                              {"type": "required", "emailAddress": {"address": CAROL, "name": "Carol"}}]}),
    )
    .unwrap()
}

/// Invitations, updates and cancellations the service sent in Alice's name.
fn sent_by_alice(srv: &TestServers) -> Vec<Value> {
    srv.graph_invitations().into_iter().filter(|i| i["organizer"] == ALICE).collect()
}

/// Events made on the service since request `n`.
fn posted_events(srv: &TestServers, n: usize) -> Vec<String> {
    requests_since(srv, n).into_iter().filter(|r| r.starts_with("POST") && r.ends_with("/events")).collect()
}

#[test]
fn a_meeting_someone_else_organized_is_never_made_on_the_service() {
    let srv = servers();
    let bob = client(&srv, BOB_PW);
    let alice = client(&srv, ALICE_PW);
    alice.post_json("/me/calendars", &json!({"name": "Team"})).unwrap();
    bob_invites(&bob, "Roadmap", 12);
    let (mut s, a) = session_with_alice(&srv);
    sync(&mut s);
    sync(&mut s);
    let cal = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    let roadmap =
        calendar_items(&s, cal).into_iter().find(|x| x.subject == "Roadmap").expect("the invitation").id;
    let team_folder = s
        .mailbox
        .folders
        .values()
        .find(|f| f.account == a && f.kind == FolderKind::Calendar && f.name == "Team")
        .expect("the Team calendar")
        .id;

    // Copied into another calendar of the account: kept here, never made there (the service would invite
    // Bob's attendees in Alice's name).
    let n = srv.graph_requests().len();
    let r = run(&mut s, "calendar.copy", json!({"id": roadmap.0, "folder": team_folder.0}));
    let copy = ItemId(r["copies"][0].as_u64().unwrap());
    update_folder(&mut s, team_folder);
    sync(&mut s);
    assert!(sent_by_alice(&srv).is_empty(), "{:?}", srv.graph_invitations());
    assert!(posted_events(&srv, n).is_empty(), "{:?}", requests_since(&srv, n));
    assert_eq!(objects_with(&srv, ALICE, "events", "subject", "Roadmap").len(), 1, "only the service's copy");
    assert!(objects_with(&srv, ALICE, "events", "_collection", "Team").is_empty());
    assert_eq!(s.mailbox.appointments[&copy].folder, team_folder, "the copy stays here");

    // Answered from its mail after the service's copy went (removed on another device): the answer goes to
    // Bob by mail, and the copy placed here is not made on the service.
    bob_invites(&bob, "Offsite", 14);
    sync(&mut s);
    let mine = objects_with(&srv, ALICE, "events", "subject", "Offsite");
    assert_eq!(mine.len(), 1, "{mine:?}");
    alice.delete(&format!("/me/events/{}", mine[0]["id"].as_str().unwrap())).unwrap();
    sync(&mut s);
    assert!(!calendar_items(&s, cal).iter().any(|x| x.subject == "Offsite"));
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let invite = s.mailbox.mail.items().find(|m| m.folder == inbox && m.subject == "Offsite").unwrap().id;
    let n = srv.graph_requests().len();
    let r = run(&mut s, "calendar.respond", json!({"id": invite.0, "response": "accept", "send": "now"}));
    assert_eq!(r["via"], "service", "{r}");
    settle(&mut s);
    sync(&mut s);
    assert!(sent_by_alice(&srv).is_empty(), "{:?}", srv.graph_invitations());
    assert!(posted_events(&srv, n).is_empty(), "{:?}", requests_since(&srv, n));
    assert!(objects_with(&srv, ALICE, "events", "subject", "Offsite").is_empty());
    assert!(calendar_items(&s, cal).iter().any(|x| x.subject == "Offsite"), "accepted here");
    assert!(s.mailbox.account(a).unwrap().sync.graph_responses.is_empty());
    let replies = srv
        .messages(BOB, "INBOX")
        .unwrap()
        .into_iter()
        .filter(|m| String::from_utf8_lossy(&m.raw).contains("method=REPLY"))
        .count();
    assert_eq!(replies, 1, "the answer reached the organizer by mail");

    // Changed here, declined on another device (the service removes it): it goes here too, it is never made
    // again with its attendees.
    bob_invites(&bob, "Retro", 16);
    sync(&mut s);
    let retro = calendar_items(&s, cal).into_iter().find(|x| x.subject == "Retro").expect("Retro").id;
    run(&mut s, "calendar.update", json!({"id": retro.0, "categories": ["Work"]}));
    let there = objects_with(&srv, ALICE, "events", "subject", "Retro");
    alice
        .post_json(
            &format!("/me/events/{}/decline", there[0]["id"].as_str().unwrap()),
            &json!({"sendResponse": true}),
        )
        .unwrap();
    let n = srv.graph_requests().len();
    sync(&mut s);
    sync(&mut s);
    assert!(posted_events(&srv, n).is_empty(), "{:?}", requests_since(&srv, n));
    assert!(objects_with(&srv, ALICE, "events", "subject", "Retro").is_empty());
    assert!(!s.mailbox.appointments.contains_key(&retro), "declined there: gone here");
    assert!(sent_by_alice(&srv).is_empty(), "{:?}", srv.graph_invitations());

    // Her own meeting still goes up with its attendees.
    run(
        &mut s,
        "calendar.newMeeting",
        json!({"folder": cal.0, "subject": "Her own", "start": at(2027, 4, 20, 15), "end": at(2027, 4, 20, 16),
               "attendees": BOB, "form": false, "send": "now"}),
    );
    settle(&mut s);
    sync(&mut s);
    assert!(sent_by_alice(&srv).iter().any(|i| i["subject"] == "Her own" && i["attendee"] == BOB));
}

#[test]
fn graph_meeting_answers_wait_out_passing_failures() {
    let srv = servers();
    let bob = client(&srv, BOB_PW);
    bob_invites(&bob, "Planning poker", 21);
    let (mut s, a) = session_with_alice(&srv);
    sync(&mut s);
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let invite =
        s.mailbox.mail.items().find(|m| m.folder == inbox && m.subject == "Planning poker").unwrap().id;
    // The service is busy for the answer: it stays queued and the status says it is tried again.
    srv.graph_fail_actions(1);
    run(
        &mut s,
        "calendar.respond",
        json!({"id": invite.0, "response": "tentative", "send": "now", "comment": "Maybe"}),
    );
    assert!(s.wait_for_sync(WAIT));
    let waiting = s.mailbox.account(a).unwrap().sync.graph_responses.clone();
    assert_eq!(waiting.len(), 1, "kept for the next run");
    assert_eq!(waiting[0].attempts, 1);
    assert!(errors(&s).iter().any(|e| e.contains("tried again")), "{:?}", errors(&s));
    assert!(srv.graph_responses().is_empty());
    // The next run sends it, with its note.
    sync(&mut s);
    let answers = srv.graph_responses();
    assert!(
        answers.iter().any(|x| x["action"] == "tentativelyAccept" && x["comment"] == "Maybe"),
        "{answers:?}"
    );
    assert!(s.mailbox.account(a).unwrap().sync.graph_responses.is_empty());
    let theirs = objects_with(&srv, BOB, "events", "subject", "Planning poker");
    let me = theirs[0]["attendees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["emailAddress"]["address"] == ALICE)
        .unwrap()
        .clone();
    assert_eq!(me["status"]["response"], "tentativelyAccepted");
}

#[test]
fn graph_meeting_notes_are_written_before_the_service_sends() {
    let srv = servers();
    let (mut s, a) = session_with_alice(&srv);
    sync(&mut s);
    let cal = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    // New Meeting with its invitation to review: the meeting window opens, nothing is made yet.
    let before = calendar_items(&s, cal).len();
    let meeting = json!({"folder": cal.0, "subject": "Budget", "start": at(2027, 6, 1, 15), "end": at(2027, 6, 1, 16),
                         "attendees": BOB, "form": false, "send": "edit"});
    let r = run(&mut s, "calendar.newMeeting", meeting.clone());
    assert_eq!(r["dialog"], "newMeeting", "{r}");
    assert_eq!(calendar_items(&s, cal).len(), before);
    settle(&mut s);
    assert!(srv.graph_invitations().is_empty());
    let mut now = meeting;
    now["send"] = json!("now");
    let r = run(&mut s, "calendar.newMeeting", now);
    let budget = ItemId(r["id"].as_u64().unwrap());
    settle(&mut s);
    sync(&mut s);
    assert_eq!(srv.graph_invitations().len(), 1);

    // Cancel Meeting to edit first: the note window, and the meeting stays until it is sent.
    let r = run(&mut s, "calendar.cancelMeeting", json!({"id": budget.0, "send": "edit"}));
    assert_eq!(r["dialog"], "serviceMeetingNote", "{r}");
    assert!(s.mailbox.appointments.get(&budget).is_some_and(|x| x.folder == cal));
    settle(&mut s);
    assert!(!srv.graph_invitations().iter().any(|i| i["kind"] == "cancel"));
    assert_eq!(objects_with(&srv, ALICE, "events", "subject", "Budget").len(), 1);
    // Sent with a note: the service's cancel action carries it.
    run(&mut s, "calendar.cancelMeeting", json!({"id": budget.0, "send": "now", "comment": "Moved to July"}));
    settle(&mut s);
    sync(&mut s);
    let cancels: Vec<Value> = srv.graph_invitations().into_iter().filter(|i| i["kind"] == "cancel").collect();
    assert_eq!(cancels.len(), 1, "{cancels:?}");
    assert_eq!(cancels[0]["comment"], "Moved to July");
    assert!(objects_with(&srv, ALICE, "events", "subject", "Budget").is_empty());
    assert!(s.mailbox.account(a).unwrap().sync.graph_cancels.is_empty(), "the note went");
    assert!(requests_since(&srv, 0).iter().any(|r| r.starts_with("POST") && r.ends_with("/cancel")));

    // An answer to edit first: the note window; nothing is answered until it is sent.
    let bob = client(&srv, BOB_PW);
    bob_invites(&bob, "Lunch", 23);
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let invite = s.mailbox.mail.items().find(|m| m.folder == inbox && m.subject == "Lunch").unwrap().id;
    let lunch = calendar_items(&s, cal).into_iter().find(|x| x.subject == "Lunch").unwrap().clone();
    let r = run(&mut s, "calendar.respond", json!({"id": invite.0, "response": "decline", "send": "edit"}));
    assert_eq!(r["dialog"], "serviceMeetingNote", "{r}");
    assert!(
        s.mailbox.appointments.get(&lunch.id).is_some_and(|x| x.attendees == lunch.attendees),
        "unchanged"
    );
    assert!(s.mailbox.account(a).unwrap().sync.graph_responses.is_empty());
    run(
        &mut s,
        "calendar.respond",
        json!({"id": invite.0, "response": "declined", "send": "now", "comment": "Away"}),
    );
    settle(&mut s);
    sync(&mut s);
    assert!(srv.graph_responses().iter().any(|x| x["action"] == "decline" && x["comment"] == "Away"));
}

#[test]
fn graph_contacts_and_tasks_keep_what_the_service_does_not_hold() {
    let srv = servers();
    let other = client(&srv, ALICE_PW);
    let (mut s, a) = session_with_alice(&srv);
    sync(&mut s);
    let contacts = s.mailbox.role_folder(a, FolderRole::Contacts).unwrap();
    let r = run(
        &mut s,
        "people.new",
        json!({"firstName": "Pat", "lastName": "Example", "folder": contacts.0,
               "emails": ["pat@example.com", "pat2@example.com"],
               "phones": {"Business": "+1 555 0100", "Business Fax": "+1 555 0199", "Pager": "+1 555 0198"},
               "anniversary": "2015-06-20"}),
    );
    let pat = ItemId(r["id"].as_u64().unwrap());
    let tasks = s.mailbox.role_folder(a, FolderRole::Tasks).unwrap();
    let r = run(
        &mut s,
        "tasks.new",
        json!({"subject": "Draft the plan", "folder": tasks.0, "status": "inProgress", "percent": 50}),
    );
    let plan = ItemId(r["id"].as_u64().unwrap());
    sync(&mut s);
    let up = objects_with(&srv, ALICE, "contacts", "givenName", "Pat");
    assert_eq!(up.len(), 1);
    assert_eq!(up[0]["businessPhones"], json!(["+1 555 0100"]));
    // Changed on the service: its numbers arrive, the fax and pager numbers and the anniversary stay.
    other
        .patch_json(
            &format!("/me/contacts/{}", up[0]["id"].as_str().unwrap()),
            &json!({"businessPhones": ["+1 555 0111"]}),
        )
        .unwrap();
    let todo = objects_with(&srv, ALICE, "tasks", "title", "Draft the plan");
    let lists = other.get_all("/me/todo/lists", 10).unwrap();
    let default = lists.iter().find(|l| l["wellknownListName"] == "defaultList").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    other
        .patch_json(
            &format!("/me/todo/lists/{default}/tasks/{}", todo[0]["id"].as_str().unwrap()),
            &json!({"title": "Draft the plan (v2)"}),
        )
        .unwrap();
    sync(&mut s);
    let c = &s.mailbox.contacts[&pat];
    assert!(c.phones.contains(&("Business".to_owned(), "+1 555 0111".to_owned())), "{:?}", c.phones);
    assert!(!c.phones.contains(&("Business".to_owned(), "+1 555 0100".to_owned())), "{:?}", c.phones);
    assert!(c.phones.contains(&("Business Fax".to_owned(), "+1 555 0199".to_owned())), "{:?}", c.phones);
    assert!(c.phones.contains(&("Pager".to_owned(), "+1 555 0198".to_owned())), "{:?}", c.phones);
    assert_eq!(c.emails.len(), 2, "{:?}", c.emails);
    assert_eq!(c.anniversary.as_deref(), Some("2015-06-20"));
    let t = &s.mailbox.tasks[&plan];
    assert_eq!(t.subject, "Draft the plan (v2)");
    assert_eq!(t.percent_complete, 50, "To Do keeps no percentage: it stays while the status does");
    // A run after that changes nothing there.
    let n = srv.graph_requests().len();
    sync(&mut s);
    assert!(!requests_since(&srv, n).iter().any(|r| r.starts_with("PATCH") || r.starts_with("POST")));
}

#[test]
fn graph_nested_contact_folders_and_lists_that_are_not_task_folders() {
    let srv = servers();
    let other = client(&srv, ALICE_PW);
    let team = other.post_json("/me/contactFolders", &json!({"displayName": "Team"})).unwrap();
    let team_id = team["id"].as_str().unwrap().to_owned();
    let sub = other
        .post_json(
            &format!("/me/contactFolders/{team_id}/childFolders"),
            &json!({"displayName": "Suppliers"}),
        )
        .unwrap();
    other
        .post_json(
            &format!("/me/contactFolders/{}/contacts", sub["id"].as_str().unwrap()),
            &json!({"givenName": "Sam", "surname": "Supplier"}),
        )
        .unwrap();
    let (mut s, a) = session_with_alice(&srv);
    sync(&mut s);
    sync(&mut s);
    let suppliers = s
        .mailbox
        .folders
        .values()
        .find(|f| f.account == a && f.kind == FolderKind::Contacts && f.name == "Suppliers")
        .expect("the folder below Team")
        .id;
    assert!(s.mailbox.contacts.values().any(|c| c.first_name == "Sam" && c.folder == suppliers));
    // The service's flagged e-mail list is no task folder.
    assert!(
        !s.mailbox.folders.values().any(|f| f.account == a && f.name == "Flagged email"),
        "{:?}",
        s.mailbox.folders.values().map(|f| f.name.clone()).collect::<Vec<_>>()
    );
    // Contacts made in the nested folder go there.
    run(&mut s, "people.new", json!({"firstName": "Robin", "lastName": "Vendor", "folder": suppliers.0}));
    sync(&mut s);
    assert_eq!(objects_with(&srv, ALICE, "contacts", "givenName", "Robin")[0]["_collection"], "Suppliers");
}

#[test]
fn graph_all_day_events_keep_their_days_east_of_utc() {
    let srv = servers();
    let other = client(&srv, ALICE_PW);
    // Made in Berlin: the service gives its midnights in UTC, 22:00 the evening before.
    other
        .post_json(
            "/me/calendar/events",
            &json!({"subject": "Berlin holiday", "isAllDay": true,
                    "start": {"dateTime": "2027-05-10T00:00:00", "timeZone": "W. Europe Standard Time"},
                    "end": {"dateTime": "2027-05-11T00:00:00", "timeZone": "W. Europe Standard Time"}}),
        )
        .unwrap();
    let listed = other.get_all("/me/calendar/events", 10).unwrap();
    assert_eq!(listed[0]["start"]["dateTime"], "2027-05-09T22:00:00.0000000", "as the service lists it");
    let (mut s, a) = session_with_alice(&srv);
    sync(&mut s);
    sync(&mut s);
    let cal = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    let h = calendar_items(&s, cal)
        .into_iter()
        .find(|x| x.subject == "Berlin holiday")
        .expect("the event")
        .clone();
    assert!(h.all_day);
    let local = h.start.unix + i64::from(h.start.offset_min) * 60;
    assert_eq!(local, at(2027, 5, 10, 0), "{h:?}");
    assert_eq!(h.end.unix - h.start.unix, 86_400);
    // Made here: midnights in the mailbox's zone (Pacific on the fake service), read back on the same days.
    let r = run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": cal.0, "subject": "Away", "allDay": true, "start": "2027-05-12", "end": "2027-05-13"}),
    );
    let away = ItemId(r["id"].as_u64().unwrap());
    update_folder(&mut s, cal);
    let up = objects_with(&srv, ALICE, "events", "subject", "Away");
    assert_eq!(up[0]["originalStartTimeZone"], "Pacific Standard Time", "{up:?}");
    assert_eq!(up[0]["start"]["dateTime"], "2027-05-12T07:00:00.0000000", "Pacific midnight, in UTC");
    let n = srv.graph_requests().len();
    sync(&mut s);
    let back = &s.mailbox.appointments[&away];
    assert!(back.all_day);
    assert_eq!(back.start.unix + i64::from(back.start.offset_min) * 60, at(2027, 5, 12, 0));
    assert!(!requests_since(&srv, n).iter().any(|r| r.starts_with("PATCH")), "nothing differs");
}

#[test]
fn graph_own_calendars_are_told_by_the_service_not_the_typed_address() {
    let srv = servers();
    let other = client(&srv, ALICE_PW);
    other
        .post_json(
            "/me/calendar/events",
            &json!({"subject": "Dentist", "start": {"dateTime": "2027-03-03T15:00:00", "timeZone": "UTC"},
                    "end": {"dateTime": "2027-03-03T16:00:00", "timeZone": "UTC"}}),
        )
        .unwrap();
    // Signed in with an alias: the service knows Alice by her primary address.
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    let r = run(
        &mut s,
        "accounts.add",
        json!({"kind": "graph", "address": "a.alias@example.com",
               "incoming": {"host": srv.graph_url(), "extraRootPem": srv.cert_pem}, "password": ALICE_PW}),
    );
    let a = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    sync(&mut s);
    sync(&mut s);
    let cal = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    assert!(calendar_items(&s, cal).iter().any(|x| x.subject == "Dentist"), "the default calendar is hers");
    assert!(!s.mailbox.folders.values().any(|f| f.account == a && f.name == "Shared Calendars"));
    // And it synchronises both ways.
    run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": cal.0, "subject": "Gym", "start": at(2027, 3, 4, 15), "end": at(2027, 3, 4, 16)}),
    );
    update_folder(&mut s, cal);
    assert_eq!(objects_with(&srv, ALICE, "events", "subject", "Gym").len(), 1);
}

#[test]
fn graph_removals_that_name_no_series_check_each_series_cheaply() {
    let srv = servers();
    let other = client(&srv, ALICE_PW);
    let weekly = |subject: &str, day: &str| {
        other
            .post_json(
                "/me/calendar/events",
                &json!({"subject": subject,
                        "start": {"dateTime": format!("2027-03-{day}T09:00:00"), "timeZone": "Eastern Standard Time"},
                        "end": {"dateTime": format!("2027-03-{day}T10:00:00"), "timeZone": "Eastern Standard Time"},
                        "recurrence": {"pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["monday"],
                                                   "firstDayOfWeek": "sunday"},
                                       "range": {"type": "numbered", "startDate": format!("2027-03-{day}"),
                                                 "numberOfOccurrences": 4}}}),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let standup = weekly("Standup", "01");
    let review = weekly("Review", "01");
    // One of Standup's occurrences moved: an exception.
    let moved = other
        .patch_json(
            &format!("/me/events/{standup}_occ_{}", at(2027, 3, 8, 14)),
            &json!({"subject": "Standup (late)"}),
        )
        .unwrap();
    let (mut s, a) = session_with_alice(&srv);
    sync(&mut s);
    sync(&mut s);
    let cal = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    assert_eq!(calendar_items(&s, cal).len(), 3, "two series and an exception");
    // The exception removed there: the delta names only the removed id.
    other.delete(&format!("/me/events/{}", moved["id"].as_str().unwrap())).unwrap();
    let n = srv.graph_requests().len();
    update_folder(&mut s, cal);
    let reqs = requests_since(&srv, n);
    assert!(reqs.iter().any(|r| r.contains(&review) && r.contains("$select=id,changeKey")), "{reqs:?}");
    assert!(
        !reqs.iter().any(|r| r.contains(&format!("{review}/instances"))),
        "Review is not read again: {reqs:?}"
    );
    assert!(reqs.iter().any(|r| r.contains(&format!("{standup}/instances"))), "{reqs:?}");
    let left: Vec<String> = calendar_items(&s, cal).iter().map(|x| x.subject.clone()).collect();
    assert_eq!(left.len(), 2, "{left:?}");
    let master = calendar_items(&s, cal).into_iter().find(|x| x.subject == "Standup").unwrap().clone();
    assert!(
        master.recurrence.as_ref().unwrap().exdates.iter().any(|d| d.unix == at(2027, 3, 8, 14)),
        "{master:?}"
    );
}
