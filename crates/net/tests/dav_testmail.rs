//! WebDAV (CardDAV, CalDAV, publishing) and IMAP shared namespaces against the in-process test servers
//! (127.0.0.1 only, synthetic data).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_model::FolderRole;
use mailcraft_net::dav::{DavAuth, DavClient, Precondition, Service};
use mailcraft_net::http::HttpClient;
use mailcraft_net::sync::{FolderState, ImapAccount, ImapSettings, ListScope, SharedKind};
use mailcraft_net::{ConnectOptions, Credentials, NetError, Security};
use mailcraft_testmail::{Config, TestServers};

const USER: &str = "alice@example.com";
const PASS: &str = "correct horse";
const BOB: &str = "bob@example.org";
const SUPPORT: &str = "support@example.com";

fn servers() -> TestServers {
    TestServers::start(Config {
        users: vec![
            (USER.into(), PASS.into()),
            (BOB.into(), "pw-bob".into()),
            (SUPPORT.into(), "pw-support".into()),
        ],
        add_trace_headers: false,
        ..Config::default()
    })
    .unwrap()
}

fn opts(s: &TestServers) -> ConnectOptions {
    ConnectOptions { io_timeout: Duration::from_secs(20), ..ConnectOptions::trusting(s.cert_der.clone()) }
}

fn dav(s: &TestServers, password: &str) -> DavClient {
    DavClient::new(opts(s), DavAuth::Basic { user: USER.into(), password: password.into() })
        .trust(&s.web_url())
}

const CARD: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:c1\r\nFN:Pat Example\r\nN:Example;Pat;;;\r\nEMAIL:pat@example.net\r\nEND:VCARD\r\n";

#[test]
fn carddav_discovery_and_conditional_writes() {
    let s = servers();
    let c = dav(&s, PASS);
    // From the DAV root and from the bare server (through the well-known redirect).
    for start in [s.dav_url(), s.web_url()] {
        let books = c.discover(&start, Service::Contacts).unwrap();
        assert_eq!(books.len(), 1, "{start}: {books:?}");
        assert_eq!(books[0].name, "Contacts");
        assert_eq!(books[0].url, s.dav_collection_url(USER, "contacts"));
    }
    let book = s.dav_collection_url(USER, "contacts");
    assert!(c.list(&book).unwrap().is_empty());
    let url = format!("{book}c1.vcf");
    let etag = c.put(&url, CARD.as_bytes(), "text/vcard", &Precondition::Create).unwrap().unwrap();
    // Creating again fails: something is there now.
    let e = c.put(&url, CARD.as_bytes(), "text/vcard", &Precondition::Create).unwrap_err();
    assert!(matches!(e, NetError::Http { status: 412, .. }), "{e:?}");
    let listed = c.list(&book).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].url, url);
    assert_eq!(listed[0].etag, etag);
    // Another client changes the card: the old ETag no longer matches.
    let theirs =
        s.put_dav_item(USER, "contacts", "c1.vcf", &CARD.replace("Pat Example", "Pat Changed")).unwrap();
    let e = c.put(&url, CARD.as_bytes(), "text/vcard", &Precondition::Match(etag.clone())).unwrap_err();
    assert!(matches!(e, NetError::Http { status: 412, .. }), "{e:?}");
    let got = c.multiget(&book, std::slice::from_ref(&url), Service::Contacts).unwrap();
    assert_eq!(got.len(), 1);
    assert!(got[0].data.contains("Pat Changed"), "{}", got[0].data);
    assert_eq!(got[0].etag, theirs);
    // A stale delete is refused, a current one works, and a second one is harmless.
    assert!(matches!(c.delete(&url, Some(&etag)), Err(NetError::Http { status: 412, .. })));
    c.delete(&url, Some(&theirs)).unwrap();
    c.delete(&url, Some(&theirs)).unwrap();
    assert!(s.dav_items(USER, "contacts").is_empty());
    // Wrong password: an authentication error, not an empty book.
    assert!(matches!(dav(&s, "nope").list(&book), Err(NetError::Auth(_))));
    // Someone else's collections are off limits.
    let theirs = s.dav_collection_url(BOB, "contacts");
    assert!(matches!(c.list(&theirs), Err(NetError::Http { status: 403, .. })));
}

#[test]
fn caldav_task_lists_and_publishing() {
    let s = servers();
    let c = dav(&s, PASS);
    let cals = c.discover(&s.dav_url(), Service::Calendars).unwrap();
    let tasks: Vec<_> = cals.iter().filter(|x| x.holds_tasks()).collect();
    assert_eq!(tasks.len(), 1, "{cals:?}");
    assert_eq!(tasks[0].name, "Tasks");
    assert!(cals.iter().any(|x| x.holds_events() && !x.holds_tasks()));
    let todo = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VTODO\r\nUID:t1\r\nSUMMARY:File the report\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
    let url = format!("{}t1.ics", tasks[0].url);
    c.put(&url, todo.as_bytes(), "text/calendar", &Precondition::Create).unwrap();
    // The task list takes only tasks.
    let event = todo.replace("VTODO", "VEVENT");
    let e =
        c.put(&format!("{}e1.ics", tasks[0].url), event.as_bytes(), "text/calendar", &Precondition::Create);
    assert!(matches!(e, Err(NetError::Http { status: 403, .. })), "{e:?}");
    // Published files are readable without signing in.
    let public = format!("{}team.ics", s.dav_collection_url(USER, "public"));
    c.put(&public, event.as_bytes(), "text/calendar", &Precondition::Any).unwrap();
    let anon = HttpClient::new(opts(&s)).get(&public, &[]).unwrap();
    assert_eq!(anon.status, 200);
    assert!(String::from_utf8_lossy(&anon.body).contains("BEGIN:VEVENT"));
}

fn imap(s: &TestServers) -> ImapAccount {
    let mut set =
        ImapSettings::new("127.0.0.1", s.imaps_port, Security::Tls, Credentials::password(USER, PASS));
    set.options = opts(s);
    ImapAccount::new(set)
}

#[test]
fn imap_other_users_and_shared_mailboxes() {
    let s = servers();
    s.grant_access(BOB, USER).unwrap();
    s.share_mailbox("Support", SUPPORT).unwrap();
    s.create_mailbox(SUPPORT, "Cases", None).unwrap();
    let raw = b"From: Pat <pat@example.net>\r\nTo: support@example.com\r\nSubject: Printer\r\nMessage-ID: <p1@example.net>\r\n\r\nIt jams.\r\n";
    s.seed_message(SUPPORT, "INBOX", raw, &[]).unwrap();
    let mut acc = imap(&s);
    // The personal folder list leaves the other namespaces out.
    let mine = acc.sync_folder_list().unwrap();
    assert!(
        mine.iter().all(|f| !f.name.starts_with("Other Users") && !f.name.starts_with("Shared")),
        "{mine:?}"
    );
    assert!(mine.iter().any(|f| f.role == Some(FolderRole::Inbox)));
    let roots = acc.shared_roots().unwrap();
    let names: Vec<(&str, SharedKind)> = roots.iter().map(|r| (r.name.as_str(), r.kind)).collect();
    assert_eq!(names, [(BOB, SharedKind::OtherUser), ("Support", SharedKind::Shared)]);
    let support = roots.iter().find(|r| r.name == "Support").unwrap();
    assert_eq!(support.root, "Shared/Support");
    let scope = ListScope::Store { root: support.root.clone(), delimiter: support.delimiter };
    let folders = acc.sync_folder_list_in(&scope).unwrap();
    let inbox = folders.iter().find(|f| f.role == Some(FolderRole::Inbox)).unwrap();
    assert_eq!(inbox.name, "Shared/Support/INBOX");
    assert_eq!(inbox.parent, None);
    let cases = folders.iter().find(|f| f.display_name == "Cases").unwrap();
    assert_eq!(cases.parent, None, "children of the root are top-level folders of the store");
    assert!(folders.iter().any(|f| f.role == Some(FolderRole::SentItems)));
    assert!(folders.iter().all(|f| f.name != "Shared/Support"), "the root container is not a folder");
    let delta = acc.sync_folder(&inbox.name, &FolderState::default()).unwrap();
    assert_eq!(delta.new.len(), 1);
    assert!(String::from_utf8_lossy(&delta.new[0].raw).contains("Subject: Printer"));
    // Writing into the shared mailbox lands in the owner's mail.
    let uid = s.messages(SUPPORT, "INBOX").unwrap()[0].uid;
    acc.connect().unwrap();
    let c = acc.client().unwrap();
    c.select("Shared/Support/INBOX").unwrap();
    c.uid_store(
        &mailcraft_net::imap::SeqSet::from_ids(&[uid]),
        mailcraft_net::imap::StoreMode::Add,
        &["\\Seen"],
        None,
    )
    .unwrap();
    assert!(s.messages(SUPPORT, "INBOX").unwrap()[0].flags.iter().any(|f| f == "\\Seen"));
    // Another user's mailbox, only for users it was shared with.
    let scope = ListScope::Store { root: format!("Other Users/{BOB}"), delimiter: Some('/') };
    let bobs = acc.sync_folder_list_in(&scope).unwrap();
    assert!(bobs.iter().any(|f| f.role == Some(FolderRole::Inbox)));
    acc.disconnect();
    let mut set =
        ImapSettings::new("127.0.0.1", s.imaps_port, Security::Tls, Credentials::password(BOB, "pw-bob"));
    set.options = opts(&s);
    let mut bob = ImapAccount::new(set);
    let theirs = bob.shared_roots().unwrap();
    assert_eq!(theirs.len(), 1, "bob was granted nothing: {theirs:?}");
}

#[test]
fn refused_namespace_command_still_lists_folders() {
    // Regression: a server that advertises NAMESPACE but answers NO used to stop the whole folder list.
    let s = TestServers::start(Config {
        users: vec![(USER.into(), PASS.into())],
        imap_refuse: vec!["NAMESPACE".into()],
        ..Config::default()
    })
    .unwrap();
    let mut acc = imap(&s);
    let folders = acc.sync_folder_list().unwrap();
    assert!(folders.iter().any(|f| f.role == Some(FolderRole::Inbox)), "{folders:?}");
    assert!(acc.shared_roots().unwrap().is_empty());
}
