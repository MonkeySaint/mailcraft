//! End-to-end tests against the in-process servers from `mailcraft-testmail` (127.0.0.1 only).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use mailcraft_model::FolderRole;
use mailcraft_net::imap::{IdleEvent, ImapClient, SeqSet, StoreMode};
use mailcraft_net::smtp::{SmtpClient, SmtpSettings};
use mailcraft_net::sync::{FolderState, ImapAccount, ImapSettings};
use mailcraft_net::{ConnectOptions, Credentials, NetError, Security, TlsOptions};
use mailcraft_testmail::{Config, TestServers};

const USER: &str = "alice@example.com";
const PASS: &str = "correct horse";

fn servers(cfg: Config) -> TestServers {
    let cfg = Config {
        users: vec![(USER.into(), PASS.into()), ("bob@example.org".into(), "pw-bob".into())],
        add_trace_headers: false,
        ..cfg
    };
    TestServers::start(cfg).unwrap()
}

fn opts(s: &TestServers) -> ConnectOptions {
    ConnectOptions { io_timeout: Duration::from_secs(20), ..ConnectOptions::trusting(s.cert_der.clone()) }
}

fn msg(subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: Bob <bob@example.org>\r\nTo: Alice <alice@example.com>\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\nMessage-ID: <{subject}@example.org>\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

fn account(s: &TestServers, security: Security) -> ImapAccount {
    let port = if security == Security::Tls { s.imaps_port } else { s.imap_port };
    let mut set = ImapSettings::new("127.0.0.1", port, security, Credentials::password(USER, PASS));
    set.options = opts(s);
    set.fetch_batch = 2;
    ImapAccount::new(set)
}

#[test]
fn imap_starttls_login_and_folder_roles() {
    let s = servers(Config::default());
    let mut c = ImapClient::connect("127.0.0.1", s.imap_port, Security::StartTls, &opts(&s)).unwrap();
    assert!(c.is_tls());
    c.login_with(&Credentials::password(USER, PASS), "").unwrap();
    assert!(c.is_authenticated());
    assert!(c.has_capability("IMAP4rev1") || c.has_capability("IMAP4rev2"));
    c.logout().unwrap();

    s.create_mailbox(USER, "Projects/Ünïcode & more", None).unwrap();
    let mut acc = account(&s, Security::Tls);
    let folders = acc.sync_folder_list().unwrap();
    let role = |r: FolderRole| folders.iter().find(|f| f.role == Some(r)).map(|f| f.name.clone());
    assert_eq!(role(FolderRole::Inbox).as_deref(), Some("INBOX"));
    assert!(role(FolderRole::SentItems).is_some());
    assert!(role(FolderRole::Drafts).is_some());
    assert!(role(FolderRole::DeletedItems).is_some());
    assert!(role(FolderRole::JunkEmail).is_some());
    assert!(role(FolderRole::Archive).is_some());
    let uni = folders.iter().find(|f| f.name.ends_with("Ünïcode & more")).expect("unicode folder listed");
    assert_eq!(uni.display_name, "Ünïcode & more");
    assert_eq!(uni.parent.as_deref(), Some("Projects"));
    assert!(folders.iter().any(|f| f.name == "Projects"));
    acc.disconnect();
}

#[test]
fn bad_password_and_untrusted_certificate() {
    let s = servers(Config::default());
    let mut c = ImapClient::connect("127.0.0.1", s.imap_port, Security::StartTls, &opts(&s)).unwrap();
    let e = c.login_with(&Credentials::password(USER, "wrong"), "").unwrap_err();
    assert!(matches!(e, NetError::Auth(_)), "{e:?}");
    let e = c.login_with(&Credentials::password(USER, "wrong"), "LOGIN").unwrap_err();
    assert!(matches!(e, NetError::Auth(_)), "{e:?}");

    // The self-signed certificate is not in an empty trust store.
    let strict = ConnectOptions {
        tls: TlsOptions { use_system_roots: true, extra_roots_der: Vec::new(), accept_invalid_certs: false },
        ..ConnectOptions::default()
    };
    let e = ImapClient::connect("127.0.0.1", s.imaps_port, Security::Tls, &strict).unwrap_err();
    assert!(matches!(e, NetError::Tls(_)), "{e:?}");
    // …unless the user accepted it anyway.
    let lax = ConnectOptions {
        tls: TlsOptions { use_system_roots: false, extra_roots_der: Vec::new(), accept_invalid_certs: true },
        ..ConnectOptions::default()
    };
    ImapClient::connect("127.0.0.1", s.imaps_port, Security::Tls, &lax).unwrap();
    // A token the server never issued is refused (XOAUTH2 error challenge, then NO).
    let mut c = ImapClient::connect("127.0.0.1", s.imap_port, Security::StartTls, &opts(&s)).unwrap();
    let e = c.login_with(&Credentials::oauth2(USER, "token"), "").unwrap_err();
    assert!(matches!(e, NetError::Auth(_)), "{e:?}");
}

#[test]
fn login_disabled_without_tls() {
    let s = servers(Config { auth_requires_tls: true, ..Config::default() });
    let mut c = ImapClient::connect("127.0.0.1", s.imap_port, Security::None, &opts(&s)).unwrap();
    assert!(c.has_capability("LOGINDISABLED"));
    assert!(c.login(USER, PASS).is_err());
    c.starttls().unwrap();
    c.login_with(&Credentials::password(USER, PASS), "").unwrap();
}

#[test]
fn incremental_folder_sync() {
    let s = servers(Config::default());
    let u1 = s.seed_message(USER, "INBOX", &msg("one", "first"), &[]).unwrap();
    let u2 = s.seed_message(USER, "INBOX", &msg("two", "second"), &["\\Seen"]).unwrap();
    let u3 = s.seed_message(USER, "INBOX", &msg("three", ".dotted line"), &["\\Flagged"]).unwrap();

    let mut acc = account(&s, Security::StartTls);
    let d1 = acc.sync_folder("INBOX", &FolderState::default()).unwrap();
    assert!(!d1.reset);
    assert_eq!(d1.new.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![u1, u2, u3]);
    assert_eq!(d1.new[0].raw, msg("one", "first"));
    assert!(d1.new[1].flags.iter().any(|f| f == "\\Seen"));
    assert!(d1.new[2].internal_date.is_some());
    assert_eq!(d1.new[2].size, Some(msg("three", ".dotted line").len() as u64));
    assert!(d1.changed.is_empty() && d1.vanished.is_empty());
    assert_eq!(d1.state.uids, vec![u1, u2, u3]);
    assert!(d1.state.uidvalidity != 0);

    // Nothing changed.
    let d2 = acc.sync_folder("INBOX", &d1.state).unwrap();
    assert!(d2.new.is_empty() && d2.changed.is_empty() && d2.vanished.is_empty(), "{d2:?}");

    // Flag change through this account, a new message and an expunge from elsewhere.
    acc.push_flags("INBOX", &[u1], &["\\Seen", "$Important"], &[]).unwrap();
    let u4 = s.seed_message(USER, "INBOX", &msg("four", "fourth"), &[]).unwrap();
    {
        let mut other = ImapClient::connect("127.0.0.1", s.imap_port, Security::StartTls, &opts(&s)).unwrap();
        other.login(USER, PASS).unwrap();
        other.select("INBOX").unwrap();
        other.uid_store(&SeqSet::from_ids(&[u2]), StoreMode::Add, &["\\Deleted"], None).unwrap();
        other.expunge().unwrap();
        other.logout().unwrap();
    }
    let d3 = acc.sync_folder("INBOX", &d1.state).unwrap();
    assert_eq!(d3.new.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![u4]);
    assert_eq!(d3.vanished, vec![u2]);
    let ch1 = d3.changed.iter().find(|(u, _)| *u == u1).expect("u1 changed");
    assert!(ch1.1.iter().any(|f| f == "\\Seen") && ch1.1.iter().any(|f| f == "$Important"));
    assert_eq!(d3.state.uids, vec![u1, u3, u4]);

    // Full-scan path (no modseq known): every known message's flags, and vanished by absence.
    let no_modseq = FolderState { highest_modseq: 0, ..d1.state.clone() };
    let d4 = acc.sync_folder("INBOX", &no_modseq).unwrap();
    assert_eq!(d4.vanished, vec![u2]);
    assert_eq!(d4.changed.iter().map(|(u, _)| *u).collect::<Vec<_>>(), vec![u1, u3]);
    assert_eq!(d4.new.len(), 1);

    // UIDVALIDITY change resets.
    let stale = FolderState { uidvalidity: d1.state.uidvalidity.wrapping_add(7), ..d3.state.clone() };
    let d5 = acc.sync_folder("INBOX", &stale).unwrap();
    assert!(d5.reset);
    assert_eq!(d5.new.len(), 3);

    // Cap on new messages per call.
    let mut capped = account(&s, Security::Tls);
    let mut set = capped.settings().clone();
    set.max_new_per_sync = 2;
    capped = ImapAccount::new(set);
    let p1 = capped.sync_folder("INBOX", &FolderState::default()).unwrap();
    assert!(p1.more);
    assert_eq!(p1.new.len(), 2);
    let p2 = capped.sync_folder("INBOX", &p1.state).unwrap();
    assert!(!p2.more);
    assert_eq!(p2.new.len(), 1);
    assert_eq!(p2.state.uids, vec![u1, u3, u4]);
}

#[test]
fn write_back_operations() {
    let s = servers(Config::default());
    let u1 = s.seed_message(USER, "INBOX", &msg("a", "x"), &[]).unwrap();
    let u2 = s.seed_message(USER, "INBOX", &msg("b", "y"), &[]).unwrap();
    let u3 = s.seed_message(USER, "INBOX", &msg("c", "z"), &[]).unwrap();
    let mut acc = account(&s, Security::Tls);

    let moved = acc.move_messages("INBOX", &[u1], "Archive").unwrap();
    if let Some(m) = moved {
        assert_eq!(m.pairs.len(), 1);
        assert_eq!(m.pairs[0].0, u1);
    }
    assert_eq!(s.messages(USER, "Archive").unwrap().len(), 1);
    assert_eq!(s.messages(USER, "INBOX").unwrap().len(), 2);

    acc.copy_messages("INBOX", &[u2], "Archive").unwrap();
    assert_eq!(s.messages(USER, "Archive").unwrap().len(), 2);

    acc.expunge("INBOX", &[u3]).unwrap();
    let inbox = s.messages(USER, "INBOX").unwrap();
    assert_eq!(inbox.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![u2]);

    let raw = msg("draft", "draft body\r\n.\r\nafter dot");
    let uid = acc.append("Drafts", &raw, &["\\Draft", "\\Seen"]).unwrap();
    let drafts = s.messages(USER, "Drafts").unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].raw, raw);
    assert!(drafts[0].flags.iter().any(|f| f == "\\Draft"));
    if let Some(uid) = uid {
        assert_eq!(uid, drafts[0].uid);
    }

    acc.create_folder("Clients/Acme").unwrap();
    acc.rename_folder("Clients/Acme", "Clients/Acme Ltd").unwrap();
    let names = s.mailboxes(USER).unwrap();
    assert!(names.iter().any(|n| n == "Clients/Acme Ltd"), "{names:?}");
    acc.delete_folder("Clients/Acme Ltd").unwrap();
    assert!(!s.mailboxes(USER).unwrap().iter().any(|n| n == "Clients/Acme Ltd"));

    // Errors from the server come back as Refused, not panics.
    let e = acc.move_messages("INBOX", &[u2], "No Such Folder").unwrap_err();
    assert!(matches!(e, NetError::Refused(_) | NetError::Bad(_)), "{e:?}");
    // The account is still usable afterwards.
    assert_eq!(acc.sync_folder("INBOX", &FolderState::default()).unwrap().new.len(), 1);
}

#[test]
fn client_level_fetch_search_store() {
    let s = servers(Config::default());
    let u1 = s.seed_message(USER, "INBOX", &msg("hello", "body text"), &["\\Seen"]).unwrap();
    let mut c = ImapClient::connect("127.0.0.1", s.imaps_port, Security::Tls, &opts(&s)).unwrap();
    c.login_with(&Credentials::password(USER, PASS), "PLAIN").unwrap();
    let info = c.select("INBOX").unwrap();
    assert_eq!(info.exists, 1);
    assert!(info.uidnext > u1);
    let f = c
        .uid_fetch(
            &SeqSet::from_ids(&[u1]),
            "(UID FLAGS ENVELOPE BODYSTRUCTURE RFC822.SIZE BODY.PEEK[HEADER])",
        )
        .unwrap();
    assert_eq!(f.len(), 1);
    let env = f[0].envelope.as_ref().unwrap();
    assert_eq!(env.subject.as_deref(), Some("hello"));
    assert_eq!(env.from[0].email().as_deref(), Some("bob@example.org"));
    assert!(f[0].bodystructure.is_some());
    assert!(f[0].header.as_ref().unwrap().starts_with(b"From: Bob"));
    assert_eq!(c.uid_search("SEEN").unwrap(), vec![u1]);
    assert!(c.uid_search("UNSEEN").unwrap().is_empty());
    let (_, modified) = c.uid_store(&SeqSet::from_ids(&[u1]), StoreMode::Remove, &["\\Seen"], None).unwrap();
    assert!(modified.is_none());
    assert_eq!(c.uid_search("UNSEEN").unwrap(), vec![u1]);
    assert!(c.uid_store(&SeqSet::from_ids(&[u1]), StoreMode::Add, &["bad flag"], None).is_err());
    let st = c.status("Archive", "MESSAGES UIDNEXT").unwrap();
    assert!(st.iter().any(|(k, v)| k == "MESSAGES" && *v == 0));
    assert!(c.noop().is_ok());
    c.unselect().unwrap();
    assert!(c.uid_search("ALL").is_err());
    c.logout().unwrap();
    assert!(matches!(c.noop(), Err(NetError::Closed)));
}

#[test]
fn idle_reports_new_mail() {
    let s = std::sync::Arc::new(servers(Config::default()));
    let mut c = ImapClient::connect("127.0.0.1", s.imap_port, Security::StartTls, &opts(&s)).unwrap();
    c.login(USER, PASS).unwrap();
    c.select("INBOX").unwrap();
    // Nothing happens: timeout.
    assert_eq!(c.idle(Duration::from_millis(300), None).unwrap(), IdleEvent::Timeout);
    // A stop flag ends the wait.
    let stop = AtomicBool::new(true);
    assert_eq!(c.idle(Duration::from_secs(10), Some(&stop)).unwrap(), IdleEvent::Stopped);
    // New mail arrives while idling.
    let s2 = s.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        s2.seed_message(USER, "INBOX", &msg("ping", "x"), &[]).unwrap();
    });
    let ev = c.idle(Duration::from_secs(10), None).unwrap();
    t.join().unwrap();
    let IdleEvent::Changed(changes) = ev else { panic!("expected a change, got {ev:?}") };
    assert!(changes.iter().any(|u| matches!(u, mailcraft_net::imap::Untagged::Exists(1))));
    assert_eq!(c.selected().unwrap().exists, 1);
    // The session is still in sync afterwards.
    assert_eq!(c.uid_search("ALL").unwrap().len(), 1);

    let mut acc = account(&s, Security::Tls);
    let s3 = s.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        s3.seed_message(USER, "INBOX", &msg("pong", "y"), &[]).unwrap();
    });
    assert!(acc.wait_for_changes("INBOX", Duration::from_secs(10), None).unwrap());
    t.join().unwrap();
}

fn smtp_settings(s: &TestServers, security: Security, creds: Option<Credentials>) -> SmtpSettings {
    let port = if security == Security::Tls { s.smtps_port } else { s.smtp_port };
    SmtpSettings { credentials: creds, options: opts(s), ..SmtpSettings::new("127.0.0.1", port, security) }
}

#[test]
fn smtp_send_and_receive() {
    let s = servers(Config { reject_recipients: vec!["nobody@example.net".into()], ..Config::default() });
    let mut c =
        SmtpClient::connect(&smtp_settings(&s, Security::StartTls, Some(Credentials::password(USER, PASS))))
            .unwrap();
    assert!(c.is_tls() && c.is_authenticated());
    let raw = b"From: alice@example.com\r\nTo: bob@example.org\r\nSubject: dots\r\n\r\n.leading dot\r\n..two\r\n.\r\nend\n";
    c.send(USER, &["bob@example.org"], raw).unwrap();
    let subs = s.submissions();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].mail_from, USER);
    assert_eq!(subs[0].rcpt_to, vec!["bob@example.org"]);
    assert_eq!(subs[0].auth_user.as_deref(), Some(USER));
    let expected: &[u8] = b"From: alice@example.com\r\nTo: bob@example.org\r\nSubject: dots\r\n\r\n.leading dot\r\n..two\r\n.\r\nend\r\n";
    assert_eq!(subs[0].data, expected);
    let inbox = s.messages("bob@example.org", "INBOX").unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].raw, expected);

    // Partial failure: one refused recipient, the other delivered.
    let e = c.send(USER, &["bob@example.org", "nobody@example.net"], raw).unwrap_err();
    assert!(e.partially_sent());
    assert_eq!(e.accepted, vec!["bob@example.org"]);
    assert_eq!(e.recipients.len(), 1);
    assert_eq!(e.recipients[0].address, "nobody@example.net");
    assert_eq!(e.recipients[0].code, 550);
    assert!(e.permanent);
    // Every recipient refused: nothing sent, connection still usable.
    let e = c.send(USER, &["nobody@example.net"], raw).unwrap_err();
    assert!(!e.partially_sent() && e.permanent && e.recipients.len() == 1);
    // Header injection through an address is refused locally.
    let e = c.send(USER, &["x@example.org>\r\nRCPT TO:<y@example.org"], raw).unwrap_err();
    assert!(e.permanent);
    c.send(USER, &[USER], raw).unwrap();
    assert_eq!(s.submissions().len(), 3);
    c.quit().unwrap();
}

#[test]
fn smtp_auth_modes_and_limits() {
    let s = servers(Config { max_message_size: 2000, ..Config::default() });
    // Implicit TLS with AUTH LOGIN.
    let mut set = smtp_settings(&s, Security::Tls, Some(Credentials::password(USER, PASS)));
    set.auth_mechanism = "LOGIN".into();
    let mut c = SmtpClient::connect(&set).unwrap();
    assert!(c.is_authenticated());
    assert_eq!(c.extensions().size, Some(2000));
    let big = vec![b'a'; 5000];
    let e = c.send(USER, &["bob@example.org"], &big).unwrap_err();
    assert!(e.permanent && e.accepted.is_empty());
    c.send(USER, &["bob@example.org"], b"Subject: small\r\n\r\nhi\r\n").unwrap();

    // Wrong password.
    let e = SmtpClient::connect(&smtp_settings(
        &s,
        Security::StartTls,
        Some(Credentials::password(USER, "nope")),
    ))
    .unwrap_err();
    assert!(matches!(e, NetError::Auth(_)), "{e:?}");
    // Without login, the submission server refuses MAIL FROM.
    let mut anon = SmtpClient::connect(&smtp_settings(&s, Security::StartTls, None)).unwrap();
    let e = anon.send(USER, &["bob@example.org"], b"x\r\n").unwrap_err();
    assert!(!e.partially_sent());
    // A token the server never issued is refused.
    let e = SmtpClient::connect(&smtp_settings(&s, Security::StartTls, Some(Credentials::oauth2(USER, "t"))))
        .unwrap_err();
    assert!(matches!(e, NetError::Auth(_)), "{e:?}");
}

#[test]
fn headers_first_then_whole_messages() {
    let s = servers(Config::default());
    let u1 = s.seed_message(USER, "INBOX", &msg("big", "the body text"), &["\\Seen"]).unwrap();
    let mut acc = account(&s, Security::Tls);
    let d = acc.sync_folder_headers("INBOX", &FolderState::default()).unwrap();
    assert_eq!(d.new.len(), 1);
    let h = &d.new[0];
    assert!(h.headers_only);
    assert_eq!(h.uid, u1);
    let text = String::from_utf8_lossy(&h.raw).into_owned();
    assert!(text.contains("Subject: big") && !text.contains("the body text"), "{text}");
    assert!(h.size.unwrap() > h.raw.len() as u64);
    // The header-only message counts as known: the next sync finds nothing new.
    let again = acc.sync_folder("INBOX", &d.state).unwrap();
    assert!(again.new.is_empty());
    let full = acc.fetch_messages("INBOX", &[u1, 999, 0]).unwrap();
    assert_eq!(full.len(), 1);
    assert!(!full[0].headers_only);
    assert!(String::from_utf8_lossy(&full[0].raw).contains("the body text"));
    assert!(full[0].flags.iter().any(|f| f == "\\Seen"));
}

#[test]
fn idle_once_reports_new_mail_and_stops_on_request() {
    let s = servers(Config::default());
    let mut acc = account(&s, Security::Tls);
    // Nothing happens: the stop flag ends the wait quickly.
    let stop = AtomicBool::new(true);
    let t = std::time::Instant::now();
    assert!(!acc.idle_once("INBOX", Duration::from_secs(30), Some(&stop)).unwrap());
    assert!(t.elapsed() < Duration::from_secs(10));
    // Mail delivered during IDLE is reported.
    let seeder = std::thread::spawn({
        let raw = msg("pushed", "hello");
        let srv = std::sync::Arc::new(s);
        let srv2 = std::sync::Arc::clone(&srv);
        move || {
            std::thread::sleep(Duration::from_millis(300));
            srv2.seed_message(USER, "INBOX", &raw, &[]).unwrap();
            srv
        }
    });
    assert!(acc.idle_once("INBOX", Duration::from_secs(20), None).unwrap());
    let _srv = seeder.join().unwrap();
}

#[test]
fn sync_reports_download_progress_per_batch() {
    use mailcraft_net::sync::FetchProgress;
    let s = servers(Config::default());
    for i in 0..7 {
        s.seed_message(USER, "INBOX", &msg(&format!("p{i}"), "x"), &[]).unwrap();
    }
    // Batches of 2, at most 5 per call: 7 waiting, 5 now and 2 next time.
    let mut set = account(&s, Security::Tls).settings().clone();
    set.max_new_per_sync = 5;
    let mut acc = ImapAccount::new(set);
    let mut seen: Vec<FetchProgress> = Vec::new();
    let d1 = acc
        .sync_folder_with_progress("INBOX", &FolderState::default(), false, &mut |p| seen.push(p))
        .unwrap();
    let done: Vec<usize> = seen.iter().map(|p| p.done).collect();
    assert_eq!(done, [0, 2, 4, 5]);
    assert!(seen.iter().all(|p| p.batch == 5 && p.waiting == 7), "{seen:?}");
    assert_eq!((d1.new.len(), d1.waiting, d1.more), (5, 7, true));

    seen.clear();
    let d2 = acc.sync_folder_with_progress("INBOX", &d1.state, true, &mut |p| seen.push(p)).unwrap();
    assert_eq!(seen.last(), Some(&FetchProgress { done: 2, batch: 2, waiting: 2 }));
    assert_eq!((d2.new.len(), d2.waiting, d2.more), (2, 2, false));
    assert!(d2.new.iter().all(|m| m.headers_only));

    // Up to date: one report, nothing waiting.
    seen.clear();
    let d3 = acc.sync_folder_with_progress("INBOX", &d2.state, false, &mut |p| seen.push(p)).unwrap();
    assert_eq!(seen, [FetchProgress::default()]);
    assert_eq!(d3.waiting, 0);
}

#[test]
fn imap_quota_root_reports_storage() {
    let s = servers(Config::default());
    let mut acc = account(&s, Security::Tls);
    assert_eq!(acc.quota().unwrap(), None, "no limit set: nothing to report");
    s.set_quota(USER, Some(10 * 1024 * 1024)).unwrap();
    let raw = msg("quota", &"x".repeat(5000));
    s.seed_message(USER, "INBOX", &raw, &[]).unwrap();
    let q = acc.quota().unwrap().expect("quota");
    assert_eq!(q.limit, 10 * 1024 * 1024);
    assert_eq!(q.used, (raw.len() as u64).div_ceil(1024) * 1024);
    assert!(q.fraction().unwrap() > 0.0);
    // GETQUOTA on the root and on an unknown root.
    let mut c = ImapClient::connect("127.0.0.1", s.imaps_port, Security::Tls, &opts(&s)).unwrap();
    c.login_with(&Credentials::password(USER, PASS), "").unwrap();
    assert!(c.has_capability("QUOTA"));
    let roots = c.get_quota_root("INBOX").unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].1[0].name, "STORAGE");
    assert!(c.get_quota_root("No such folder").is_err());
    c.logout().unwrap();
    acc.disconnect();
}

/// Issue #16: mail that reached the Inbox after its last sync but before IDLE began (while the run synced
/// other folders, or between reporting push and sending IDLE) was taken in by the SELECT before IDLE, so
/// IDLE had nothing to report and the message waited for the next scheduled check.
#[test]
fn idle_once_reports_mail_that_arrived_between_sync_and_idle() {
    let s = servers(Config::default());
    s.create_mailbox(USER, "Projects", None).unwrap();
    s.seed_message(USER, "INBOX", &msg("old", "already here"), &[]).unwrap();
    let mut acc = account(&s, Security::Tls);
    let d = acc.sync_folder("INBOX", &FolderState::default()).unwrap();
    assert_eq!(d.new.len(), 1);
    // The run goes on with another folder; meanwhile new mail arrives in the Inbox.
    acc.sync_folder("Projects", &FolderState::default()).unwrap();
    s.seed_message(USER, "INBOX", &msg("in-the-gap", "arrived before IDLE"), &[]).unwrap();
    let t = std::time::Instant::now();
    assert!(acc.idle_once("INBOX", Duration::from_secs(3), None).unwrap(), "mail in the gap was missed");
    assert!(t.elapsed() < Duration::from_secs(2), "reported at once, not after the wait");
    // Reported once: the next IDLE waits for real news instead of repeating it in a busy loop.
    assert!(!acc.idle_once("INBOX", Duration::from_millis(800), None).unwrap());
    // The sync that follows picks the message up, and push is back to plain IDLE.
    let d2 = acc.sync_folder("INBOX", &d.state).unwrap();
    assert_eq!(d2.new.len(), 1);
    assert!(!acc.idle_once("INBOX", Duration::from_millis(800), None).unwrap());
    // Mail during that IDLE is still reported the usual way.
    s.seed_message(USER, "INBOX", &msg("later", "pushed"), &[]).unwrap();
    assert!(acc.idle_once("INBOX", Duration::from_secs(10), None).unwrap());
}

/// Issue #16, second window: the Inbox stays selected after its sync and a later command of the same run
/// takes in the server's EXISTS for new mail; IDLE must still report it.
#[test]
fn idle_once_reports_mail_announced_during_the_run() {
    let s = servers(Config::default());
    s.seed_message(USER, "INBOX", &msg("first", "one"), &[]).unwrap();
    let mut acc = account(&s, Security::Tls);
    let d = acc.sync_folder("INBOX", &FolderState::default()).unwrap();
    let uid = d.new.first().unwrap().uid;
    s.seed_message(USER, "INBOX", &msg("second", "two"), &[]).unwrap();
    // Still on the Inbox: this fetch hears about the new message as an untagged EXISTS.
    assert_eq!(acc.fetch_messages("INBOX", &[uid]).unwrap().len(), 1);
    assert!(acc.idle_once("INBOX", Duration::from_secs(3), None).unwrap(), "announced mail was missed");
}
