//! Live tests of the EWS client against a real server (opt-in; never run in CI): every test is `#[ignore]` and
//! returns early unless these are set (see `docs/live-tests.md`):
//!
//! - `MC_LIVE_EWS_URL`: the EWS endpoint (`https://mail.example.com/EWS/Exchange.asmx`),
//! - `MC_LIVE_EWS_USER` / `MC_LIVE_EWS_PASS`: a mailbox kept for testing,
//! - `MC_LIVE_EWS_CA` (optional): a PEM file with the server's certificate, for a self-signed server,
//! - `MC_LIVE_EWS_ADDR` (optional): the server's IP address, when its name does not resolve here.
//!
//! Run: `cargo test -p mailcraft-net --test live_ews -- --ignored --test-threads=1`. Everything a test creates
//! carries a run tag and is removed at the end; mail goes only to the test mailbox itself.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_net::ConnectOptions;
use mailcraft_net::ews::autodiscover::{AutodiscoverOptions, Probe, autodiscover};
use mailcraft_net::ews::{EwsAuth, EwsClient, FlagChange, ItemChange, Signer};
use mailcraft_net::http::{HttpClient, Url};

struct Live {
    url: String,
    user: String,
    pass: String,
    opts: ConnectOptions,
}

fn live() -> Option<Live> {
    let get = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let url = get("MC_LIVE_EWS_URL")?;
    let mut opts = ConnectOptions { io_timeout: Duration::from_secs(60), ..ConnectOptions::default() };
    if let Some(ca) = get("MC_LIVE_EWS_CA") {
        let pem = std::fs::read_to_string(ca).expect("MC_LIVE_EWS_CA readable");
        let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        use base64::Engine;
        opts.tls.extra_roots_der.push(base64::engine::general_purpose::STANDARD.decode(b64).expect("PEM"));
    }
    if let Some(addr) = get("MC_LIVE_EWS_ADDR") {
        let host = Url::parse(&url).expect("URL").host;
        mailcraft_net::transport::set_host_address(&host, Some(addr.parse().expect("IP address")));
    }
    Some(Live { url, user: get("MC_LIVE_EWS_USER")?, pass: get("MC_LIVE_EWS_PASS")?, opts })
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

fn client(l: &Live) -> EwsClient {
    let auth = EwsAuth::Auto { user: l.user.clone(), password: l.pass.clone() };
    EwsClient::connect(&l.url, auth, HttpClient::new(l.opts.clone()), Some(&l.user)).expect("sign-in")
}

#[test]
#[ignore = "live server: set MC_LIVE_EWS_* (docs/live-tests.md)"]
fn live_ews_mail_round_trip() {
    let l = live_or_skip!();
    let c = client(&l);
    let d = c
        .distinguished(&[
            "msgfolderroot",
            "inbox",
            "sentitems",
            "deleteditems",
            "drafts",
            "calendar",
            "contacts",
            "tasks",
        ])
        .unwrap();
    for (name, f) in &d {
        eprintln!("{name}: {:?}", f.as_ref().map(|f| (&f.name, &f.class, f.total)));
    }
    let folders = c.folders().unwrap();
    eprintln!("{} folders, {} mail", folders.len(), folders.iter().filter(|f| f.is_mail()).count());
    let (_, h) = c.hierarchy_changed("").unwrap();
    let name = tag("Folder");
    let fid = c.create_folder("msgfolderroot", &name).unwrap();
    let cleanup = |c: &EwsClient| {
        let _ = c.delete_folder(&fid);
    };
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(c.hierarchy_changed(&h).unwrap().0, "a new folder changes the hierarchy");
        c.rename_folder(&fid, &format!("{name}x")).unwrap();
        let subject = tag("Msg");
        let raw = format!(
            "From: {u}\r\nTo: {u}\r\nSubject: {subject}\r\nMessage-ID: <{subject}@example.com>\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nlive test\r\n",
            u = l.user
        );
        let (id, _) = c.upload(&fid, raw.as_bytes(), false, false).unwrap();
        let first = c.sync_items(&fid, "", 100).unwrap();
        eprintln!("first sync: {:?}", first.changes);
        assert!(first.changes.iter().any(|ch| matches!(ch, ItemChange::Create(_))));
        let got = c.get_mime(std::slice::from_ref(&id)).unwrap();
        assert_eq!(got.len(), 1);
        assert!(String::from_utf8_lossy(got[0].mime.as_deref().unwrap()).contains(&subject));
        eprintln!(
            "item: read {:?} flag {:?} size {:?} received {:?}",
            got[0].read, got[0].flag_status, got[0].size, got[0].received
        );
        assert_eq!(got[0].read, Some(false), "an unread upload stays unread");
        let r = c
            .set_flags(&[(
                id.clone(),
                FlagChange {
                    read: Some(true),
                    flag_status: Some(2),
                    categories: Some(vec!["MCLIVE".into()]),
                    last_verb: Some(102),
                },
            )])
            .unwrap();
        assert!(r[0].is_ok(), "{:?}", r[0]);
        let again = c.sync_items(&fid, &first.state, 100).unwrap();
        eprintln!("after flags: {:?}", again.changes);
        let got = c.get_mime(std::slice::from_ref(&id)).unwrap();
        assert_eq!((got[0].read, got[0].flag_status), (Some(true), Some(2)));
        let moved = c.move_items(std::slice::from_ref(&id), "inbox").unwrap();
        let new_id = moved[0].clone().unwrap().map(|(i, _)| i).unwrap_or(id.clone());
        let moved_back = c.move_items(std::slice::from_ref(&new_id), &fid).unwrap();
        let back = moved_back[0].clone().unwrap().map(|(i, _)| i).unwrap_or(new_id);
        assert!(c.delete_items(&[back], true).unwrap()[0].is_ok());
        assert!(c.folder_total(&fid).unwrap().is_none_or(|t| t == 0));
    }));
    cleanup(&c);
    if let Err(e) = res {
        std::panic::resume_unwind(e);
    }
}

#[test]
#[ignore = "live server: set MC_LIVE_EWS_* (docs/live-tests.md)"]
fn live_ews_items_oof_availability_and_autodiscover() {
    let l = live_or_skip!();
    let c = client(&l);
    let t = tag("Contact");
    let (id, ck) = c
        .create_item(
            "contacts",
            &format!("<t:Contact><t:GivenName>{t}</t:GivenName><t:Surname>Example</t:Surname><t:EmailAddresses><t:Entry Key=\"EmailAddress1\">robin@example.net</t:Entry></t:EmailAddresses></t:Contact>"),
            "",
        )
        .unwrap();
    let set = r#"<t:SetItemField><t:FieldURI FieldURI="contacts:JobTitle"/><t:Contact><t:JobTitle>Tester</t:JobTitle></t:Contact></t:SetItemField>"#;
    let upd = c.update_item(&id, Some(&ck), set, "");
    eprintln!("contact update: {upd:?}");
    let got = c.get_items(std::slice::from_ref(&id), "<t:BaseShape>AllProperties</t:BaseShape>").unwrap();
    eprintln!(
        "contact: {:?}",
        got[0].as_ref().map(|e| e.children.iter().map(|c| c.name.clone()).collect::<Vec<_>>())
    );
    c.delete_item(&id).unwrap();

    let ev = tag("Event");
    let (eid, _) = c
        .create_item(
            "calendar",
            &format!("<t:CalendarItem><t:Subject>{ev}</t:Subject><t:Start>2026-11-03T15:00:00Z</t:Start><t:End>2026-11-03T16:00:00Z</t:End><t:LegacyFreeBusyStatus>Busy</t:LegacyFreeBusyStatus><t:Recurrence><t:WeeklyRecurrence><t:Interval>1</t:Interval><t:DaysOfWeek>Tuesday</t:DaysOfWeek></t:WeeklyRecurrence><t:NumberedRecurrence><t:StartDate>2026-11-03</t:StartDate><t:NumberOfOccurrences>3</t:NumberOfOccurrences></t:NumberedRecurrence></t:Recurrence></t:CalendarItem>"),
            r#"SendMeetingInvitations="SendToNone""#,
        )
        .unwrap();
    let got = c.get_items(std::slice::from_ref(&eid), "<t:BaseShape>AllProperties</t:BaseShape>").unwrap();
    let e = got[0].as_ref().unwrap();
    eprintln!("event children: {:?}", e.children.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    eprintln!("recurrence: {:?}", e.child("Recurrence"));
    let a =
        c.availability(std::slice::from_ref(&l.user), "2026-11-03T00:00:00", "2026-11-04T00:00:00").unwrap();
    eprintln!("availability: {a:?}");
    c.delete_item(&eid).unwrap();

    let task = tag("Task");
    let (tid, _) = c.create_item("tasks", &format!("<t:Task><t:Subject>{task}</t:Subject><t:DueDate>2026-11-05T00:00:00Z</t:DueDate><t:Status>InProgress</t:Status></t:Task>"), "").unwrap();
    c.delete_item(&tid).unwrap();

    let oof = c.oof(&l.user).unwrap();
    eprintln!("oof: {oof:?}");
    c.set_oof(&l.user, &oof).unwrap();

    let host = Url::parse(&l.url).unwrap();
    let o = AutodiscoverOptions {
        urls: vec![format!("https://{}/autodiscover/autodiscover.xml", host.host)],
        redirect_probe: None,
        resolver: None,
        http: l.opts.clone(),
        // A test server whose host may be outside the address's domain.
        confirmed_hosts: vec![host.host.clone()],
    };
    let signer = Signer::new(EwsAuth::Auto { user: l.user.clone(), password: l.pass.clone() });
    let found = autodiscover(&l.user, Some(&signer), &o).unwrap();
    eprintln!("autodiscover: {found:?}");
    assert!(matches!(found, Probe::Found(_)));
}
