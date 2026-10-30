//! The EWS client against the fake EWS endpoint of `mailcraft-testmail` (HTTPS on 127.0.0.1, synthetic users):
//! sign-in (Basic, NTLM, Auto switching to NTLM), folders, item sync, MIME download, flags, moves, deletes,
//! uploads, sending, contacts/events/tasks, automatic replies, availability, meeting answers and Autodiscover.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_net::ews::autodiscover::{AutodiscoverOptions, Probe, autodiscover};
use mailcraft_net::ews::{
    EwsAuth, EwsClient, FlagChange, ItemChange, MeetingReply, OofSettings, Signer, is_code,
};
use mailcraft_net::http::HttpClient;
use mailcraft_net::{ConnectOptions, NetError};
use mailcraft_testmail::{Config, TestServers};

const USER: &str = "pat@example.com";
const PASS: &str = "pat-test-pw";
const BOB: &str = "bob@example.org";

fn opts(s: &TestServers) -> ConnectOptions {
    ConnectOptions { io_timeout: Duration::from_secs(20), ..ConnectOptions::trusting(s.cert_der.clone()) }
}

fn client(s: &TestServers, auth: EwsAuth) -> Result<EwsClient, NetError> {
    EwsClient::connect(&s.ews_url(), auth, HttpClient::new(opts(s)), Some(USER))
}

fn servers(ntlm_only: bool) -> TestServers {
    TestServers::start(Config {
        users: vec![(USER.into(), PASS.into()), (BOB.into(), "bob-pw".into())],
        add_trace_headers: false,
        ews_ntlm_only: ntlm_only,
        ..Config::default()
    })
    .unwrap()
}

fn msg(subject: &str) -> Vec<u8> {
    format!("From: {BOB}\r\nTo: {USER}\r\nSubject: {subject}\r\nMessage-ID: <{subject}@example.org>\r\n\r\nbody\r\n").into_bytes()
}

#[test]
fn sign_in_basic_ntlm_and_auto() {
    let s = servers(false);
    assert!(matches!(
        client(&s, EwsAuth::Basic { user: USER.into(), password: "wrong".into() }),
        Err(NetError::Auth(_))
    ));
    client(&s, EwsAuth::Basic { user: USER.into(), password: PASS.into() }).unwrap();
    let c = client(&s, EwsAuth::Ntlm { user: USER.into(), password: PASS.into() }).unwrap();
    // The NTLM connection stays signed in: later requests carry no credentials.
    c.folders().unwrap();
    c.folders().unwrap();
    let log = s.ews_log();
    assert!(log.iter().any(|l| l == "GetFolder ntlm"), "{log:?}");
    assert!(log.iter().filter(|l| l.as_str() == "FindFolder conn").count() >= 2, "{log:?}");
    assert!(matches!(
        client(&s, EwsAuth::Ntlm { user: USER.into(), password: "wrong".into() }),
        Err(NetError::Auth(_))
    ));
    // DOMAIN\user names work too (the domain is any; the user part finds the mailbox).
    client(&s, EwsAuth::Ntlm { user: "EXAMPLE\\pat".into(), password: PASS.into() }).unwrap();

    let only = servers(true);
    assert!(client(&only, EwsAuth::Basic { user: USER.into(), password: PASS.into() }).is_err());
    let auto = client(&only, EwsAuth::Auto { user: USER.into(), password: PASS.into() }).unwrap();
    auto.folders().unwrap();
    assert!(only.ews_log().iter().any(|l| l == "GetFolder ntlm"));
    // A bearer token equal to the password signs in on the test server.
    client(&s, EwsAuth::Bearer(PASS.into())).unwrap();
}

#[test]
fn folders_items_flags_moves_uploads_and_sending() {
    let s = servers(false);
    let c = client(&s, EwsAuth::Basic { user: USER.into(), password: PASS.into() }).unwrap();
    let d = c.distinguished(&["msgfolderroot", "inbox", "sentitems", "outbox", "calendar"]).unwrap();
    assert!(d[1].1.is_some() && d[2].1.is_some());
    assert!(d[3].1.is_none(), "a folder the server lacks is None");
    let inbox = d[1].1.clone().unwrap().id;
    let folders = c.folders().unwrap();
    assert!(folders.iter().any(|f| f.name == "Inbox" && f.is_mail()));
    assert!(folders.iter().any(|f| f.kind == "CalendarFolder" && !f.is_mail()));
    let (_, h0) = c.hierarchy_changed("").unwrap();
    assert!(!c.hierarchy_changed(&h0).unwrap().0);
    let work = c.create_folder("msgfolderroot", "Work").unwrap();
    let (changed, _) = c.hierarchy_changed(&h0).unwrap();
    assert!(changed);
    c.rename_folder(&work, "Projects").unwrap();
    assert!(s.mailboxes(USER).unwrap().contains(&"Projects".to_owned()));
    let sub = c.create_folder(&work, "Sub").unwrap();
    c.move_folder(&sub, &inbox).unwrap();
    assert!(s.mailboxes(USER).unwrap().contains(&"INBOX/Sub".to_owned()));

    // Item sync: first everything as creations, then changes only.
    s.seed_message(USER, "INBOX", &msg("one"), &[]).unwrap();
    s.seed_message(USER, "INBOX", &msg("two"), &["\\Seen"]).unwrap();
    let first = c.sync_items("inbox", "", 1).unwrap();
    assert_eq!(first.changes.len(), 1);
    assert!(!first.last);
    let rest = c.sync_items("inbox", &first.state, 100).unwrap();
    assert!(rest.last && rest.changes.len() == 1);
    let ids: Vec<String> = first
        .changes
        .iter()
        .chain(&rest.changes)
        .filter_map(|ch| match ch {
            ItemChange::Create(i) => Some(i.id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2);
    let got = c.get_mime(&ids).unwrap();
    assert!(got.iter().any(|i| String::from_utf8_lossy(i.mime.as_deref().unwrap()).contains("Subject: one")));
    assert!(got.iter().any(|i| i.read == Some(true)));
    let one = got.iter().find(|i| i.read == Some(false)).unwrap().id.clone();
    let r = c
        .set_flags(&[(
            one.clone(),
            FlagChange {
                read: Some(true),
                flag_status: Some(2),
                categories: Some(vec!["Blue".into()]),
                last_verb: None,
            },
        )])
        .unwrap();
    assert!(r[0].is_ok());
    let flags = s
        .messages(USER, "INBOX")
        .unwrap()
        .into_iter()
        .find(|m| m.raw.starts_with(&msg("one")[..10]))
        .unwrap()
        .flags;
    assert!(flags.contains(&"\\Seen".to_owned()) && flags.contains(&"\\Flagged".to_owned()), "{flags:?}");
    let ch = c.sync_items("inbox", &rest.state, 100).unwrap();
    assert!(
        matches!(&ch.changes[..], [ItemChange::Update(i)] if i.categories == vec!["Blue".to_owned()] && i.flag_status == Some(2))
    );
    // Read state alone: a read-flag change.
    c.set_flags(&[(one.clone(), FlagChange { read: Some(false), ..Default::default() })]).unwrap();
    let ch2 = c.sync_items("inbox", &ch.state, 100).unwrap();
    assert!(matches!(&ch2.changes[..], [ItemChange::ReadFlag { read: false, .. }]), "{:?}", ch2.changes);
    // A move gives the item a new id.
    let moved = c.move_items(std::slice::from_ref(&one), &work).unwrap();
    let new_id = moved[0].clone().unwrap().unwrap().0;
    assert_ne!(new_id, one);
    let ch3 = c.sync_items("inbox", &ch2.state, 100).unwrap();
    assert!(matches!(&ch3.changes[..], [ItemChange::Delete(id)] if *id == one));
    assert_eq!(s.messages(USER, "Projects").unwrap().len(), 1);
    // Deleting an item already gone is fine.
    let del = c.delete_items(&[new_id.clone(), "M999.1".into()], true).unwrap();
    assert!(del.iter().all(Result::is_ok));
    assert!(s.messages(USER, "Projects").unwrap().is_empty());
    // An unknown sync state is the server's error code.
    let e = c.sync_items("inbox", "S999", 10).unwrap_err();
    assert!(is_code(&e, "ErrorInvalidSyncStateData"), "{e}");
    // Upload (read, not a draft) and send with a blind copy.
    let (up, _) = c.upload("inbox", &msg("uploaded"), true, false).unwrap();
    assert!(up.starts_with('M'));
    let m = s
        .messages(USER, "INBOX")
        .unwrap()
        .into_iter()
        .find(|m| String::from_utf8_lossy(&m.raw).contains("uploaded"))
        .unwrap();
    assert!(m.flags.contains(&"\\Seen".to_owned()) && !m.flags.contains(&"\\Draft".to_owned()));
    // Regression: a server that takes the read state from IsRead only kept an unread upload as read.
    c.upload("inbox", &msg("unread upload"), false, false).unwrap();
    let m = s
        .messages(USER, "INBOX")
        .unwrap()
        .into_iter()
        .find(|m| String::from_utf8_lossy(&m.raw).contains("unread upload"))
        .unwrap();
    assert!(!m.flags.contains(&"\\Seen".to_owned()), "{:?}", m.flags);
    let out = format!("From: {USER}\r\nTo: {BOB}\r\nSubject: sent\r\n\r\nhello\r\n");
    c.send(out.as_bytes(), &["carol@example.net".into()], true).unwrap();
    let sub = s.submissions();
    let last = sub.last().unwrap();
    assert!(last.rcpt_to.contains(&BOB.to_owned()) && last.rcpt_to.contains(&"carol@example.net".to_owned()));
    assert_eq!(s.messages(BOB, "INBOX").unwrap().len(), 1);
    assert!(
        s.messages(USER, "Sent")
            .unwrap()
            .iter()
            .any(|m| String::from_utf8_lossy(&m.raw).contains("Subject: sent"))
    );
    c.delete_folder(&work).unwrap();
    assert!(!s.mailboxes(USER).unwrap().contains(&"Projects".to_owned()));
}

#[test]
fn items_oof_availability_and_meeting_answers() {
    let s = servers(false);
    let c = client(&s, EwsAuth::Basic { user: USER.into(), password: PASS.into() }).unwrap();
    let (id, ck) = c
        .create_item(
            "contacts",
            "<t:Contact><t:GivenName>Robin</t:GivenName><t:EmailAddresses><t:Entry Key=\"EmailAddress1\">robin@example.net</t:Entry></t:EmailAddresses></t:Contact>",
            "",
        )
        .unwrap();
    let found = c.find_items("contacts", "<t:BaseShape>AllProperties</t:BaseShape>", 100).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].child_text("GivenName"), Some("Robin"));
    let set = r#"<t:SetItemField><t:FieldURI FieldURI="contacts:GivenName"/><t:Contact><t:GivenName>Rob</t:GivenName></t:Contact></t:SetItemField>"#;
    let (_, ck2) = c.update_item(&id, Some(&ck), set, "").unwrap();
    // The old change key no longer matches: a conflict.
    let e = c.update_item(&id, Some(&ck), set, "").unwrap_err();
    assert!(is_code(&e, "ErrorIrresolvableConflict"));
    let got = c.get_items(&[id.clone(), "P404".into()], "<t:BaseShape>AllProperties</t:BaseShape>").unwrap();
    assert_eq!(got[0].as_ref().unwrap().child_text("GivenName"), Some("Rob"));
    assert!(got[1].is_none());
    assert_ne!(ck, ck2);
    c.delete_item(&id).unwrap();
    assert!(s.ews_items(USER, "contacts").is_empty());

    // Automatic replies round trip.
    assert_eq!(c.oof(USER).unwrap().state, "Disabled");
    let o = OofSettings {
        state: "Scheduled".into(),
        external_audience: "Known".into(),
        start: Some("2026-11-02T00:00:00Z".into()),
        end: Some("2026-11-06T00:00:00Z".into()),
        internal_reply: "Away <this week>".into(),
        external_reply: "Out".into(),
    };
    c.set_oof(USER, &o).unwrap();
    assert_eq!(c.oof(USER).unwrap(), o);
    assert_eq!(s.ews_oof(USER).unwrap().internal, "Away <this week>");

    // Availability: Bob's calendar, an unknown address.
    s.ews_put_item(BOB, "calendar", "<CalendarItem><Subject>Busy</Subject><Start>2026-11-03T15:00:00Z</Start><End>2026-11-03T16:00:00Z</End><LegacyFreeBusyStatus>Busy</LegacyFreeBusyStatus></CalendarItem>").unwrap();
    let a = c
        .availability(
            &[BOB.into(), "nobody@example.net".into()],
            "2026-11-03T00:00:00",
            "2026-11-04T00:00:00",
        )
        .unwrap();
    let blocks = a[0].as_ref().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!((blocks[0].start.as_str(), blocks[0].busy_type.as_str()), ("2026-11-03T15:00:00", "Busy"));
    assert!(a[1].is_err());

    c.reply_to_meeting(MeetingReply::Tentative, "M1.1", "", true, "maybe").unwrap();
    assert_eq!(
        s.ews_replies(),
        vec![(USER.to_owned(), "TentativelyAcceptItem".to_owned(), "M1.1".to_owned())]
    );
    // Regression: an answer not sent (SaveOnly) left its response message behind as a draft.
    let drafts = s.messages(USER, "Drafts").unwrap().len();
    c.reply_to_meeting(MeetingReply::Accept, "M1.1", "", false, "").unwrap();
    assert_eq!(s.ews_replies().len(), 2, "the answer is recorded");
    assert_eq!(s.messages(USER, "Drafts").unwrap().len(), drafts, "no stray draft");
}

#[test]
fn autodiscover_signed_in_and_probing() {
    let s = servers(false);
    let o = AutodiscoverOptions {
        urls: s.autodiscover_url_templates(),
        redirect_probe: None,
        resolver: None,
        http: opts(&s),
        // The test server is at 127.0.0.1, outside example.com.
        confirmed_hosts: vec!["127.0.0.1".into()],
    };
    // Not set up for the domain: nothing.
    assert!(matches!(autodiscover(USER, None, &o).unwrap(), Probe::Nothing { .. }));
    s.set_autodiscover("example.com", true);
    // Without credentials: the server asks for a sign-in.
    assert!(matches!(autodiscover(USER, None, &o).unwrap(), Probe::NeedsSignIn { .. }));
    let signer = Signer::new(EwsAuth::Auto { user: USER.into(), password: PASS.into() });
    match autodiscover(USER, Some(&signer), &o).unwrap() {
        Probe::Found(f) => {
            assert_eq!(f.ews_url, s.ews_url());
            assert!(f.from.contains("/example.com/autodiscover/"));
        }
        other => panic!("{other:?}"),
    }
    let bad = Signer::new(EwsAuth::Basic { user: USER.into(), password: "nope".into() });
    assert!(matches!(autodiscover(USER, Some(&bad), &o), Err(NetError::Auth(_))));
}

/// Regression: a batch of messages whose answer was over the response limit failed the whole download, and
/// with it every later run. Now the batch is fetched again one by one and only the message too large on its
/// own is left out.
#[test]
fn a_message_too_large_to_download_is_left_out_not_fatal() {
    let s = servers(false);
    s.seed_message(USER, "INBOX", &msg("small"), &[]).unwrap();
    let mut big = msg("big");
    big.extend(std::iter::repeat_n(b'x', 60_000));
    big.extend_from_slice(b"\r\n");
    s.seed_message(USER, "INBOX", &big, &[]).unwrap();
    let mut c = client(&s, EwsAuth::Basic { user: USER.into(), password: PASS.into() }).unwrap();
    let page = c.sync_items("inbox", "", 50).unwrap();
    let items: Vec<(String, Option<u64>)> = page
        .changes
        .into_iter()
        .filter_map(|ch| match ch {
            ItemChange::Create(i) => Some((i.id, i.size)),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 2);
    c.set_max_response(30_000);
    assert!(matches!(
        c.get_mime(&items.iter().map(|(i, _)| i.clone()).collect::<Vec<_>>()),
        Err(NetError::TooLarge(_))
    ));
    let (got, skipped) = c.get_mime_sized(&items).unwrap();
    assert_eq!(got.len(), 1);
    assert!(String::from_utf8_lossy(got[0].mime.as_deref().unwrap()).contains("Subject: small"));
    assert_eq!(skipped.len(), 1);
    // The client works on afterwards.
    assert!(c.folder("inbox").unwrap().is_some());
    // Batches follow the sizes: ten at most, and a few MB each.
    use mailcraft_net::ews::{MIME_BATCH_BYTES, mime_batches};
    let many: Vec<(String, Option<u64>)> = (0..25).map(|i| (format!("i{i}"), Some(1000))).collect();
    assert_eq!(mime_batches(&many).iter().map(Vec::len).collect::<Vec<_>>(), vec![10, 10, 5]);
    let large = vec![
        ("a".to_owned(), Some(MIME_BATCH_BYTES / 2)),
        ("b".to_owned(), Some(MIME_BATCH_BYTES / 2 + 1)),
        ("c".to_owned(), Some(MIME_BATCH_BYTES * 3)),
        ("d".to_owned(), None),
    ];
    assert_eq!(
        mime_batches(&large),
        vec![vec!["a".to_owned()], vec!["b".to_owned()], vec!["c".to_owned()], vec!["d".to_owned()]]
    );
}

/// Regression: the HTTPS location named by the unauthenticated plain-HTTP redirect probe is in another domain
/// (here 127.0.0.1, standing in for a hostile host): the password must not go there until the user confirmed
/// the host.
#[test]
fn autodiscover_never_signs_in_to_an_unconfirmed_host() {
    use std::io::{Read, Write};
    let s = servers(false);
    s.set_autodiscover("example.com", true);
    // A plain-HTTP responder that redirects every request to the test server's HTTPS Autodiscover location.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let target = format!("{}/example.com/autodiscover/autodiscover.xml", s.web_url());
    std::thread::spawn(move || {
        for conn in listener.incoming().take(4) {
            let Ok(mut c) = conn else { continue };
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let _ = write!(
                c,
                "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    let mut o = AutodiscoverOptions {
        // The domain's own locations do not answer (as on a hostile network).
        urls: Vec::new(),
        redirect_probe: Some(format!("http://127.0.0.1:{port}/autodiscover/autodiscover.xml")),
        resolver: None,
        http: opts(&s),
        confirmed_hosts: Vec::new(),
    };
    let signer = Signer::new(EwsAuth::Auto { user: USER.into(), password: PASS.into() });
    match autodiscover(USER, Some(&signer), &o).unwrap() {
        Probe::Unconfirmed { url, host } => {
            assert_eq!(host, "127.0.0.1");
            assert!(url.contains("/example.com/autodiscover/"), "{url}");
        }
        other => panic!("{other:?}"),
    }
    let asked: Vec<String> = s.ews_log().into_iter().filter(|l| l.starts_with("autodiscover ")).collect();
    assert!(!asked.is_empty(), "the location was asked (without credentials)");
    assert!(asked.iter().all(|l| l == "autodiscover auth=none"), "no credentials sent: {asked:?}");
    // Once the user confirmed the host, the lookup signs in there.
    o.confirmed_hosts = vec!["127.0.0.1".into()];
    match autodiscover(USER, Some(&signer), &o).unwrap() {
        Probe::Found(f) => assert_eq!(f.ews_url, s.ews_url()),
        other => panic!("{other:?}"),
    }
    assert!(s.ews_log().iter().any(|l| l == "autodiscover auth=Basic"), "{:?}", s.ews_log());
}
