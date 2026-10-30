//! End to end, Exchange (EWS): an account on the local test server's fake EWS endpoint (HTTPS, 127.0.0.1,
//! synthetic users): folders from FindFolder, mail from SyncFolderItems (new mail downloaded as MIME), local
//! changes written back (read, flag, categories, moves, deletes, folders), sending with SendAndSaveCopy,
//! NTLM sign-in, contacts, calendar (a series with an exception) and tasks both ways, Autodiscover during
//! account setup, automatic replies, availability and meeting answers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_engine::Session;
use mailcraft_engine::model::{AccountId, Contact, FolderId, FolderRole, ItemId, Mailbox, Task, TaskStatus};
use mailcraft_testmail::{Config, TestServers};
use serde_json::{Value, json};

const PAT: &str = "pat@example.com";
const PAT_PW: &str = "pat-test-pw";
const BOB: &str = "bob@example.org";
const WAIT: Duration = Duration::from_secs(30);

fn servers(ntlm_only: bool) -> TestServers {
    TestServers::start(Config {
        users: vec![(PAT.into(), PAT_PW.into()), (BOB.into(), "bob-pw".into())],
        add_trace_headers: false,
        ews_ntlm_only: ntlm_only,
        ..Config::default()
    })
    .unwrap()
}

fn message(subject: &str, id: &str) -> Vec<u8> {
    format!(
        "From: {BOB}\r\nTo: {PAT}\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
         Message-ID: <{id}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Hello over EWS.\r\n"
    )
    .into_bytes()
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn no_errors(s: &Session) {
    assert_eq!(s.sync_status().accounts.iter().find_map(|a| a.error.clone()), None, "{:?}", s.sync_status());
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

fn update_folder(s: &mut Session, f: FolderId) {
    run(s, "sendReceive.folder", json!({"folder": f.0}));
    settle(s);
}

fn session() -> Session {
    let mut s = Session::new(Mailbox::new_local("Local Folders"));
    s.view.auto_send_receive_minutes = 0;
    s
}

fn add(s: &mut Session, srv: &TestServers, auth: &str) -> AccountId {
    let r = run(
        s,
        "accounts.add",
        json!({
            "kind": "exchange",
            "address": PAT,
            "senderName": "Pat Example",
            "incoming": {"host": srv.ews_url(), "auth": auth, "extraRootPem": srv.cert_pem},
            "password": PAT_PW,
        }),
    );
    assert_eq!(r["kind"], "exchange");
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

fn subjects(srv: &TestServers, user: &str, mailbox: &str) -> Vec<String> {
    srv.messages(user, mailbox)
        .unwrap()
        .iter()
        .filter_map(|m| mailcraft_mime::parse(&m.raw).ok().map(|x| x.subject()))
        .collect()
}

fn flags(srv: &TestServers, mailbox: &str, subject: &str) -> Vec<String> {
    srv.messages(PAT, mailbox)
        .unwrap()
        .into_iter()
        .find(|m| mailcraft_mime::parse(&m.raw).map(|x| x.subject()).ok().as_deref() == Some(subject))
        .map(|m| m.flags)
        .unwrap_or_default()
}

#[test]
fn exchange_account_receives_writes_back_and_sends() {
    let srv = servers(false);
    let mut s = session();
    let a = add(&mut s, &srv, "");
    let t = run(&mut s, "accounts.test", json!({"account": a.0, "wait": true}));
    assert_eq!(t["ok"], true, "{t}");
    assert!(t.to_string().contains("Exchange"), "{t}");

    srv.seed_message(PAT, "INBOX", &message("Hello EWS", "hello"), &[]).unwrap();
    srv.create_mailbox(PAT, "Projects", None).unwrap();
    srv.seed_message(PAT, "Projects", &message("Plan", "plan"), &["\\Seen"]).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    assert!(s.mailbox.folder(inbox).unwrap().remote_name.is_some(), "Inbox mapped to its folder id");
    let projects =
        s.mailbox.folders.values().find(|f| f.account == a && f.name == "Projects").map(|f| f.id).unwrap();
    let plan = find(&s, projects, "Plan").expect("mail of other folders arrives too");
    assert!(s.mailbox.mail.get(plan).unwrap().read);
    let hello = find(&s, inbox, "Hello EWS").expect("first run downloads the mail");
    assert!(!s.mailbox.mail.get(hello).unwrap().read);
    let outbox = s.mailbox.role_folder(a, FolderRole::Outbox).unwrap();
    assert!(s.mailbox.folder(outbox).unwrap().remote_name.is_none(), "the Outbox stays local");

    // Later mail and changes made elsewhere arrive (SyncFolderItems from the kept state).
    srv.seed_message(PAT, "INBOX", &message("Second", "second"), &["\\Seen", "\\Flagged"]).unwrap();
    sync(&mut s);
    let second = find(&s, inbox, "Second").expect("new mail from the sync state");
    assert!(s.mailbox.mail.get(second).unwrap().read && s.mailbox.mail.get(second).unwrap().is_flagged());
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Hello EWS").count(), 1, "no duplicates");

    // Local changes reach the server.
    select(&mut s, hello);
    run(&mut s, "mail.markRead", json!({"ids": [hello.0]}));
    select(&mut s, hello);
    run(&mut s, "mail.flag", json!({"ids": [hello.0]}));
    settle(&mut s);
    let f = flags(&srv, "INBOX", "Hello EWS");
    assert!(f.contains(&"\\Seen".to_owned()) && f.contains(&"\\Flagged".to_owned()), "{f:?}");
    let archive = s.mailbox.role_folder(a, FolderRole::Archive).unwrap();
    select(&mut s, hello);
    run(&mut s, "mail.move", json!({"ids": [hello.0], "folder": archive.0}));
    settle(&mut s);
    assert!(subjects(&srv, PAT, "Archive").contains(&"Hello EWS".to_owned()));
    assert!(!subjects(&srv, PAT, "INBOX").contains(&"Hello EWS".to_owned()));
    sync(&mut s);
    assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "Hello EWS").count(), 1);
    assert_eq!(s.mailbox.mail.get(hello).unwrap().folder, archive, "the moved item keeps its identity");
    assert!(flags(&srv, "Archive", "Hello EWS").contains(&"\\Flagged".to_owned()), "flags move along");

    // Permanent delete removes the server copy; a message removed on the server goes here too.
    select(&mut s, second);
    run(&mut s, "mail.permanentDelete", json!({"ids": [second.0]}));
    settle(&mut s);
    assert!(!subjects(&srv, PAT, "INBOX").contains(&"Second".to_owned()));
    srv.delete_mailbox(PAT, "Projects").unwrap();
    sync(&mut s);
    assert!(s.mailbox.mail.get(plan).is_none(), "the folder removed on the server goes here too");
    assert!(!s.mailbox.folders.values().any(|f| f.account == a && f.name == "Projects"));

    // Folders made, renamed and removed here.
    s.selection.folder = inbox;
    let clients = FolderId(run(&mut s, "folder.new", json!({"name": "Clients"}))["folder"].as_u64().unwrap());
    settle(&mut s);
    sync(&mut s);
    assert!(srv.mailboxes(PAT).unwrap().contains(&"INBOX/Clients".to_owned()), "{:?}", srv.mailboxes(PAT));
    assert_eq!(s.mailbox.folders.values().filter(|f| f.name == "Clients").count(), 1, "no second folder");
    s.selection.folder = clients;
    run(&mut s, "folder.rename", json!({"name": "Customers"}));
    settle(&mut s);
    sync(&mut s);
    assert!(srv.mailboxes(PAT).unwrap().contains(&"INBOX/Customers".to_owned()), "{:?}", srv.mailboxes(PAT));
    for _ in 0..2 {
        if s.mailbox.folders.contains_key(&clients) {
            s.selection.folder = clients;
            run(&mut s, "folder.delete", json!({"folder": clients.0, "confirm": true}));
            settle(&mut s);
            sync(&mut s);
        }
    }
    assert!(!srv.mailboxes(PAT).unwrap().iter().any(|m| m.contains("Customers")), "{:?}", srv.mailboxes(PAT));

    // Sending: CreateItem SendAndSaveCopy; the server files the copy, which arrives once.
    s.selection.folder = inbox;
    let c = run(&mut s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.setTo", json!({"compose": c, "value": BOB}));
    run(&mut s, "compose.setBcc", json!({"compose": c, "value": "carol@example.net"}));
    run(&mut s, "compose.setSubject", json!({"compose": c, "value": "Over EWS"}));
    run(&mut s, "compose.insertText", json!({"compose": c, "text": "Sent through EWS."}));
    run(&mut s, "compose.send", json!({"compose": c}));
    settle(&mut s);
    sync(&mut s);
    let sub = srv.submissions();
    let last = sub.last().expect("submitted");
    assert!(
        last.rcpt_to.contains(&BOB.to_owned()) && last.rcpt_to.contains(&"carol@example.net".to_owned()),
        "{:?}",
        last.rcpt_to
    );
    assert!(subjects(&srv, BOB, "INBOX").contains(&"Over EWS".to_owned()));
    let sent = s.mailbox.role_folder(a, FolderRole::SentItems).unwrap();
    assert_eq!(
        s.mailbox.mail.items().filter(|m| m.folder == sent && m.subject == "Over EWS").count(),
        1,
        "one sent copy"
    );
    assert_eq!(s.mailbox.mail.items().filter(|m| m.folder == outbox).count(), 0, "the Outbox is empty");
}

#[test]
fn ntlm_only_server_and_autodiscover_setup() {
    let srv = servers(true);
    let mut s = session();
    srv.set_autodiscover("example.com", true);
    s.sync.discovery = mailcraft_engine::sync::DiscoverOptions {
        resolver: mailcraft_engine::sync::ResolverChoice::Off,
        autoconfig_urls: Vec::new(),
        tenant_url: None,
        allow_reserved: true,
        autodiscover_urls: srv.autodiscover_url_templates(),
        autodiscover_probe: None,
        http: mailcraft_engine::sync::ConnectOptions::trusting(srv.cert_der.clone()),
        ..Default::default()
    };
    // Without a password the lookup only learns that an Exchange server answers here.
    let d = run(&mut s, "accounts.discover", json!({"address": PAT, "wait": true}));
    assert_eq!(d["ews"]["needsSignIn"], true, "{d}");
    // The test server (127.0.0.1) is outside example.com: the password goes there only once the host is
    // confirmed. Until then nothing is added and no request carried credentials.
    let accounts = s.mailbox.accounts.len();
    let ask = run(
        &mut s,
        "accounts.add",
        json!({"kind": "exchange", "address": PAT, "password": PAT_PW, "autodiscover": true,
               "incoming": {"extraRootPem": srv.cert_pem}}),
    );
    assert_eq!(ask["needsConfirm"], true, "{ask}");
    assert_eq!(ask["confirmHost"], "127.0.0.1");
    assert_eq!(s.mailbox.accounts.len(), accounts, "not added yet");
    let asked: Vec<String> = srv.ews_log().into_iter().filter(|l| l.starts_with("autodiscover ")).collect();
    assert!(!asked.is_empty() && asked.iter().all(|l| l == "autodiscover auth=none"), "{asked:?}");
    // Setting the account up finds the EWS address with the password (NTLM, as the server only offers it).
    let r = run(
        &mut s,
        "accounts.add",
        json!({"kind": "exchange", "address": PAT, "password": PAT_PW, "autodiscover": true,
               "autodiscoverHosts": ["127.0.0.1"], "incoming": {"extraRootPem": srv.cert_pem}}),
    );
    let a = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
    assert_eq!(s.mailbox.account(a).unwrap().incoming.as_ref().unwrap().host, srv.ews_url());
    srv.seed_message(PAT, "INBOX", &message("Via NTLM", "ntlm"), &[]).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    assert!(find(&s, inbox, "Via NTLM").is_some());
    assert!(srv.ews_log().iter().any(|l| l.ends_with(" ntlm")), "{:?}", srv.ews_log());
    assert!(!srv.ews_log().iter().any(|l| l.ends_with(" basic")));
}

#[test]
fn contacts_calendar_and_tasks_synchronise_over_ews() {
    let srv = servers(false);
    let mut s = session();
    let a = add(&mut s, &srv, "basic");
    let contacts = s.mailbox.role_folder(a, FolderRole::Contacts).unwrap();
    let calendar = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    let tasks = s.mailbox.role_folder(a, FolderRole::Tasks).unwrap();
    // On the server: a contact, a weekly series with one moved occurrence and one deleted, a task.
    srv.ews_put_item(PAT, "contacts", "<Contact><GivenName>Robin</GivenName><Surname>Server</Surname><EmailAddresses><Entry Key=\"EmailAddress1\">robin@example.net</Entry></EmailAddresses><JobTitle>Editor</JobTitle></Contact>").unwrap();
    let occ = srv.ews_put_item(PAT, "calendar", "<CalendarItem><Subject>Weekly sync (moved)</Subject><Start>2026-11-10T16:00:00Z</Start><End>2026-11-10T17:00:00Z</End><CalendarItemType>Exception</CalendarItemType><UID>series-1</UID></CalendarItem>").unwrap();
    srv.ews_put_item(PAT, "calendar", &format!("<CalendarItem><Subject>Weekly sync</Subject><Start>2026-11-03T15:00:00Z</Start><End>2026-11-03T16:00:00Z</End><UID>series-1</UID><CalendarItemType>RecurringMaster</CalendarItemType><Recurrence><WeeklyRecurrence><Interval>1</Interval><DaysOfWeek>Tuesday</DaysOfWeek></WeeklyRecurrence><NumberedRecurrence><StartDate>2026-11-03</StartDate><NumberOfOccurrences>6</NumberOfOccurrences></NumberedRecurrence></Recurrence><ModifiedOccurrences><Occurrence><ItemId Id=\"{occ}\"/><Start>2026-11-10T16:00:00Z</Start><End>2026-11-10T17:00:00Z</End><OriginalStart>2026-11-10T15:00:00Z</OriginalStart></Occurrence></ModifiedOccurrences><DeletedOccurrences><DeletedOccurrence><Start>2026-11-17T15:00:00Z</Start></DeletedOccurrence></DeletedOccurrences></CalendarItem>")).unwrap();
    srv.ews_put_item(PAT, "tasks", "<Task><Subject>Server task</Subject><Status>InProgress</Status><PercentComplete>25</PercentComplete></Task>").unwrap();
    // Here: a contact and a task.
    let robin_here = ItemId(s.mailbox.alloc_id());
    s.mailbox.contacts.insert(
        robin_here,
        Contact {
            id: robin_here,
            folder: contacts,
            first_name: "Sam".into(),
            last_name: "Local".into(),
            emails: vec!["sam@example.org".into()],
            ..Default::default()
        },
    );
    let task_here = ItemId(s.mailbox.alloc_id());
    s.mailbox.tasks.insert(
        task_here,
        Task { id: task_here, folder: tasks, subject: "Local task".into(), ..Default::default() },
    );
    sync(&mut s);

    let robin = s
        .mailbox
        .contacts
        .values()
        .find(|c| c.folder == contacts && c.first_name == "Robin")
        .expect("the server's contact arrived")
        .clone();
    assert_eq!(
        (robin.emails.clone(), robin.job_title.as_str()),
        (vec!["robin@example.net".to_owned()], "Editor")
    );
    let up = srv.ews_items(PAT, "contacts");
    assert_eq!(up.len(), 2, "{up:?}");
    assert!(up.iter().any(|(_, x)| x.contains("Sam") && x.contains("MailCraftUid")), "{up:?}");
    let series = s
        .mailbox
        .appointments
        .values()
        .find(|x| x.folder == calendar && x.subject == "Weekly sync")
        .expect("the series arrived")
        .clone();
    assert_eq!(series.recurrence.as_ref().unwrap().rrule, "FREQ=WEEKLY;BYDAY=TU;COUNT=6");
    assert!(
        series.recurrence.as_ref().unwrap().exdates.iter().any(|d| d.unix == 1_794_927_600),
        "{:?}",
        series.recurrence
    );
    let moved = s
        .mailbox
        .appointments
        .values()
        .find(|x| x.subject == "Weekly sync (moved)")
        .expect("the exception arrived");
    assert_eq!(moved.recurrence_id.map(|r| r.unix), Some(1_794_322_800));
    assert!(s.mailbox.tasks.values().any(|t| t.folder == tasks
        && t.subject == "Server task"
        && t.status == TaskStatus::InProgress
        && t.percent_complete == 25));
    assert!(srv.ews_items(PAT, "tasks").iter().any(|(_, x)| x.contains("Local task")));

    // A second run with no changes writes nothing.
    let writes = || {
        srv.ews_log().iter().filter(|l| l.starts_with("CreateItem") || l.starts_with("UpdateItem")).count()
    };
    let before = writes();
    sync(&mut s);
    assert_eq!(writes(), before, "{:?}", srv.ews_log());

    // Regression: one occurrence removed here was reported as saved while the server kept it. Until single
    // occurrences can be written, the update is refused and the user sees why; the server is untouched.
    let server_series =
        || srv.ews_items(PAT, "calendar").into_iter().find(|(_, x)| x.contains("Weekly sync"));
    let kept = server_series();
    let next_week = mailcraft_engine::model::DateTime::from_unix(1_794_927_600 + 7 * 86_400, 0);
    s.mailbox.appointments.get_mut(&series.id).unwrap().recurrence.as_mut().unwrap().exdates.push(next_week);
    let before = writes();
    run(&mut s, "sendReceive.folder", json!({"folder": calendar.0}));
    assert!(s.wait_for_sync(WAIT));
    let error = s.sync_status().accounts.iter().find_map(|a| a.error.clone()).unwrap_or_default();
    assert!(error.contains("single occurrences"), "{error}");
    assert_eq!(writes(), before, "nothing written");
    assert_eq!(server_series(), kept);
    // Undone here: nothing left to write, no error.
    s.mailbox.appointments.get_mut(&series.id).unwrap().recurrence.as_mut().unwrap().exdates.pop();
    update_folder(&mut s, calendar);

    // Changed here: only the changed property goes up. Changed there: it comes down.
    s.mailbox.contacts.get_mut(&robin.id).unwrap().job_title = "Chief editor".into();
    update_folder(&mut s, contacts);
    let (rid, rxml) = srv.ews_items(PAT, "contacts").into_iter().find(|(_, x)| x.contains("Robin")).unwrap();
    assert!(rxml.contains("Chief editor"), "{rxml}");
    assert!(srv.ews_set_item_field(PAT, &rid, "CompanyName", "Example Press"));
    update_folder(&mut s, contacts);
    assert_eq!(s.mailbox.contacts[&robin.id].company, "Example Press");

    // A new event here goes up as a calendar item, a weekly series keeps its pattern.
    run(
        &mut s,
        "calendar.newAppointment",
        json!({"folder": calendar.0, "subject": "Planning", "start": "2026-11-05T14:00", "durationMin": 60}),
    );
    update_folder(&mut s, calendar);
    let cal = srv.ews_items(PAT, "calendar");
    assert!(cal.iter().any(|(_, x)| x.contains("Planning")), "{cal:?}");

    // Deleted here: deleted there.
    s.mailbox.tasks.remove(&task_here);
    update_folder(&mut s, tasks);
    assert!(!srv.ews_items(PAT, "tasks").iter().any(|(_, x)| x.contains("Local task")));
    // Removed there: removed here.
    let (tid, _) = srv.ews_items(PAT, "tasks").into_iter().find(|(_, x)| x.contains("Server task")).unwrap();
    assert!(srv.ews_remove_item(PAT, &tid));
    update_folder(&mut s, tasks);
    assert!(!s.mailbox.tasks.values().any(|t| t.subject == "Server task"));
}

fn meeting_request() -> Vec<u8> {
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Test//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
               UID:review-1@example.org\r\nSEQUENCE:0\r\nDTSTAMP:20261020T090000Z\r\nDTSTART:20261105T140000Z\r\n\
               DTEND:20261105T150000Z\r\nSUMMARY:Design review\r\nORGANIZER;CN=Bob:mailto:bob@example.org\r\n\
               ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:pat@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    format!(
        "From: {BOB}\r\nTo: {PAT}\r\nSubject: Design review\r\nDate: Tue, 20 Oct 2026 09:00:00 +0000\r\n\
         Message-ID: <review-1@example.org>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\n{ics}"
    )
    .into_bytes()
}

#[test]
fn automatic_replies_free_busy_and_meeting_answers_go_through_the_server() {
    let srv = servers(false);
    let mut s = session();
    s.tz_offset_min = 0;
    // Without an Exchange account the command is unavailable.
    assert!(s.run("file.automaticReplies", &json!({})).is_err());
    let a = add(&mut s, &srv, "");

    // Automatic replies: read (opens the dialog), turn on with a schedule, read back.
    let r = run(&mut s, "file.automaticReplies", json!({}));
    assert_eq!(r["dialog"], "automaticReplies", "{r}");
    assert_eq!(r["state"], "off", "{r}");
    let r = run(
        &mut s,
        "file.automaticReplies",
        json!({"state": "scheduled", "start": "2026-11-02T08:00", "end": "2026-11-06T18:00",
               "insideReply": "Away this week.", "outsideReply": "Out of the office.", "outside": "contacts"}),
    );
    assert_eq!(r["saved"], true, "{r}");
    let o = srv.ews_oof(PAT).expect("the server has the settings");
    assert_eq!((o.state.as_str(), o.audience.as_str()), ("Scheduled", "Known"), "{o:?}");
    assert!(o.internal.contains("Away this week.") && o.external.contains("Out of the office."), "{o:?}");
    assert!(o.start.starts_with("2026-11-02T08:00"), "{o:?}");
    let r = run(&mut s, "file.automaticReplies", json!({"account": a.0, "dialog": false}));
    assert!(r.get("dialog").is_none(), "{r}");
    assert_eq!(
        (r["start"].as_str(), r["end"].as_str()),
        (Some("2026-11-02T08:00"), Some("2026-11-06T18:00")),
        "{r}"
    );
    assert_eq!((r["state"].as_str(), r["outside"].as_str()), (Some("scheduled"), Some("contacts")), "{r}");
    assert!(r["insideReply"].as_str().unwrap().contains("Away this week."), "{r}");
    // Bad params are refused before anything is sent.
    assert!(s.run("file.automaticReplies", &json!({"state": "sometimes"})).is_err());
    assert!(
        s.run(
            "file.automaticReplies",
            &json!({"state": "scheduled", "start": "2026-11-06T08:00", "end": "2026-11-02T08:00"})
        )
        .is_err()
    );
    run(&mut s, "file.automaticReplies", json!({"state": "off"}));
    assert_eq!(srv.ews_oof(PAT).unwrap().state, "Disabled");

    // Other people's busy times for the Scheduling Assistant.
    srv.ews_put_item(BOB, "calendar", "<CalendarItem><Subject>Busy</Subject><Start>2026-11-05T15:00:00Z</Start><End>2026-11-05T16:00:00Z</End><LegacyFreeBusyStatus>Tentative</LegacyFreeBusyStatus></CalendarItem>").unwrap();
    let r = run(
        &mut s,
        "calendar.freeBusy",
        json!({"start": "2026-11-05T00:00", "end": "2026-11-06T00:00", "attendees": [BOB, "nobody@example.net"]}),
    );
    let people = r["attendees"].as_array().unwrap_or_else(|| panic!("{r}"));
    assert_eq!(people.len(), 2, "{r}");
    assert_eq!(people[0]["address"], BOB);
    let busy = people[0]["busy"].as_array().unwrap();
    assert_eq!(busy.len(), 1, "{r}");
    assert_eq!(busy[0]["start"], 1_793_890_800, "{r}");
    assert_eq!(busy[0]["showAs"], "tentative");
    assert!(people[1]["error"].is_string(), "an unknown address reports an error: {r}");

    // A meeting request kept on the server is answered there (AcceptItem), with no reply mail.
    srv.seed_message(PAT, "INBOX", &meeting_request(), &[]).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let req = find(&s, inbox, "Design review").expect("the request arrives");
    let remote = s.mailbox.mail.get(req).unwrap().remote_id.clone().expect("with its item id");
    // Regression: the server was told before the answer was made here; when that failed (a read-only
    // calendar) the organizer had the answer anyway, and a retry sent a second one.
    let cal = s.mailbox.role_folder(a, FolderRole::Calendar).unwrap();
    let saved = s.mailbox.folder(cal).unwrap().remote_name.clone();
    mailcraft_engine::cmd::calendar::props::update(&mut s.mailbox, cal, |p| p.read_only = true);
    assert!(s.run("calendar.respond", &json!({"id": req.0, "response": "accept"})).is_err());
    assert!(srv.ews_replies().is_empty(), "nothing reached the server: {:?}", srv.ews_replies());
    s.mailbox.folders.get_mut(&cal).unwrap().remote_name = saved;
    let r = run(&mut s, "calendar.respond", json!({"id": req.0, "response": "accept"}));
    assert_eq!((r["response"].as_str(), r["sent"].as_bool()), (Some("accepted"), Some(true)), "{r}");
    assert!(r["compose"].is_null(), "{r}");
    assert!(r["appointment"].is_u64(), "the meeting is on the calendar here too: {r}");
    assert_eq!(srv.ews_replies(), vec![(PAT.to_owned(), "AcceptItem".to_owned(), remote)]);
    sync(&mut s);
    assert!(subjects(&srv, BOB, "INBOX").is_empty(), "the server answers, no second reply mail");
}

fn account_error(s: &Session) -> String {
    s.sync_status().accounts.iter().find_map(|a| a.error.clone()).unwrap_or_default()
}

/// Regression: mail moved into a folder below an Archive the server lacks had no server folder to go to, so
/// the run took it as deleted here and removed it from the server for good.
#[test]
fn mail_moved_below_an_archive_the_server_lacks_stays_on_the_server() {
    let srv = servers(false);
    srv.delete_mailbox(PAT, "Archive").unwrap();
    srv.seed_message(PAT, "INBOX", &message("Keep me", "keep"), &[]).unwrap();
    let mut s = session();
    let a = add(&mut s, &srv, "");
    sync(&mut s);
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    let archive = s.mailbox.role_folder(a, FolderRole::Archive).unwrap();
    assert!(s.mailbox.folder(archive).unwrap().remote_name.is_none(), "the server has no archive");
    let keep = find(&s, inbox, "Keep me").unwrap();
    let year = run(&mut s, "folder.new", json!({"name": "2024", "parent": archive.0}));
    let year = FolderId(year["folder"].as_u64().unwrap());
    select(&mut s, keep);
    run(&mut s, "mail.move", json!({"ids": [keep.0], "folder": year.0}));
    settle(&mut s);
    sync(&mut s);
    let boxes = srv.mailboxes(PAT).unwrap();
    assert!(
        boxes.contains(&"Archive/2024".to_owned()),
        "the archive and its folder are made there: {boxes:?}"
    );
    assert!(subjects(&srv, PAT, "Archive/2024").contains(&"Keep me".to_owned()), "{boxes:?}");
    assert_eq!(s.mailbox.mail.get(keep).unwrap().folder, year);
}

/// Regression: deleting an empty folder whose subfolder was kept (it still held mail the run had not
/// downloaded) removed the subfolder and its mail from the server along with the parent.
#[test]
fn deleting_an_empty_folder_keeps_a_subfolder_that_still_holds_mail() {
    let srv = servers(false);
    srv.create_mailbox(PAT, "Parent", None).unwrap();
    srv.create_mailbox(PAT, "Parent/Child", None).unwrap();
    let mut s = session();
    let a = add(&mut s, &srv, "");
    sync(&mut s);
    let parent =
        s.mailbox.folders.values().find(|f| f.account == a && f.name == "Parent").map(|f| f.id).unwrap();
    let mut last = String::new();
    for attempt in 0..2 {
        if !s.mailbox.folders.contains_key(&parent) {
            break;
        }
        // Arrives on the server after the last download: not here when the folder is deleted.
        let child = srv.mailboxes(PAT).unwrap().into_iter().find(|m| m.ends_with("Child")).unwrap();
        last = format!("Late {attempt}");
        srv.seed_message(PAT, &child, &message(&last, &format!("late{attempt}")), &[]).unwrap();
        s.selection.folder = parent;
        run(&mut s, "folder.delete", json!({"folder": parent.0, "confirm": true}));
        settle(&mut s);
    }
    sync(&mut s);
    let boxes = srv.mailboxes(PAT).unwrap();
    let child = boxes.iter().find(|m| m.ends_with("Child")).unwrap_or_else(|| panic!("kept: {boxes:?}"));
    assert!(subjects(&srv, PAT, child).contains(&last), "{boxes:?}");
}

/// Regression: one message the server would not take (too large here) failed the whole run before the
/// Outbox and receiving, on every run after.
#[test]
fn a_message_the_server_refuses_does_not_stop_sending_or_receiving() {
    let srv = servers(false);
    let mut s = session();
    let a = add(&mut s, &srv, "");
    sync(&mut s);
    srv.ews_set_max_upload(Some(64));
    let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
    s.selection.folder = inbox;
    let draft = run(&mut s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.setSubject", json!({"compose": draft, "value": "Too large for the server"}));
    run(
        &mut s,
        "compose.insertText",
        json!({"compose": draft, "text": "A draft the server refuses to store."}),
    );
    let saved = run(&mut s, "compose.saveDraft", json!({"compose": draft}))["draft"].as_u64().unwrap();
    let c = run(&mut s, "mail.newEmail", json!({}))["compose"].as_u64().unwrap();
    run(&mut s, "compose.setTo", json!({"compose": c, "value": BOB}));
    run(&mut s, "compose.setSubject", json!({"compose": c, "value": "Still sent"}));
    run(&mut s, "compose.send", json!({"compose": c}));
    srv.seed_message(PAT, "INBOX", &message("Still received", "still"), &[]).unwrap();
    for _ in 0..2 {
        run(&mut s, "sendReceive.all", json!({}));
        assert!(s.wait_for_sync(WAIT));
    }
    assert!(account_error(&s).contains("did not take it"), "{}", account_error(&s));
    assert!(subjects(&srv, BOB, "INBOX").contains(&"Still sent".to_owned()));
    assert!(find(&s, inbox, "Still received").is_some());
    let d = s.mailbox.mail.get(ItemId(saved)).expect("the draft stays here");
    assert!(d.remote_id.is_none());
}
