//! Protocol edge cases against scripted servers on 127.0.0.1: QRESYNC sync, synchronising literals, SASL
//! error challenges, BYE, size caps, SMTP pipelining and enhanced status codes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;
use std::time::Duration;

use mailcraft_net::imap::{ImapClient, Limits, SeqSet};
use mailcraft_net::smtp::{SmtpClient, SmtpSettings};
use mailcraft_net::sync::{FolderState, ImapAccount, ImapSettings};
use mailcraft_net::{ConnectOptions, Credentials, NetError, Security};

/// Plays `steps`: for each (expected line prefix, reply), reads one client line (skipped when the prefix is
/// `None`), checks it, and writes the reply (CRLF-separated lines; empty = no reply). Returns what the
/// client sent.
fn script(steps: Vec<(Option<&'static str>, &'static str)>) -> (u16, JoinHandle<Vec<String>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut seen = Vec::new();
        for (expect, reply) in steps {
            if let Some(prefix) = expect {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    panic!("client closed; expected `{prefix}`");
                }
                let line = line.trim_end_matches(['\r', '\n']).to_owned();
                assert!(line.starts_with(prefix), "expected `{prefix}`, got `{line}`");
                seen.push(line);
            }
            if !reply.is_empty() {
                w.write_all(reply.replace('\n', "\r\n").as_bytes()).unwrap();
                w.write_all(b"\r\n").unwrap();
            }
        }
        seen
    });
    (port, t)
}

fn opts() -> ConnectOptions {
    ConnectOptions { io_timeout: Duration::from_secs(10), ..ConnectOptions::default() }
}

#[test]
fn qresync_incremental_sync() {
    let (port, t) = script(vec![
        (
            None,
            "* OK [CAPABILITY IMAP4rev1 QRESYNC CONDSTORE ENABLE AUTH=PLAIN SASL-IR UIDPLUS MOVE IDLE] hi",
        ),
        (
            Some("M0001 AUTHENTICATE PLAIN AHVAZXhhbXBsZS5jb20AcHc="),
            "M0001 OK [CAPABILITY IMAP4rev1 QRESYNC CONDSTORE ENABLE UIDPLUS MOVE IDLE] in",
        ),
        (Some("M0002 ENABLE QRESYNC"), "* ENABLED QRESYNC\nM0002 OK enabled"),
        (
            Some("M0003 SELECT INBOX"),
            "* 3 EXISTS\n* OK [UIDVALIDITY 7] v\n* OK [UIDNEXT 5] n\n* OK [HIGHESTMODSEQ 120] m\nM0003 OK [READ-WRITE] selected",
        ),
        (Some("M0004 UID SEARCH UID 4:*"), "* SEARCH 4\nM0004 OK done"),
        (
            Some("M0005 UID FETCH 1:3 (UID FLAGS) (CHANGEDSINCE 100 VANISHED)"),
            "* VANISHED (EARLIER) 2\n* 2 FETCH (UID 3 FLAGS (\\Seen) MODSEQ (110))\nM0005 OK done",
        ),
        (
            Some("M0006 UID FETCH 4 (UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[])"),
            "* 3 FETCH (UID 4 FLAGS () INTERNALDATE \"05-Oct-2026 10:00:00 +0000\" RFC822.SIZE 5 BODY[] {5}\nhello)\nM0006 OK done",
        ),
        (Some("M0007 LOGOUT"), "* BYE bye\nM0007 OK done"),
    ]);
    let mut set =
        ImapSettings::new("127.0.0.1", port, Security::None, Credentials::password("u@example.com", "pw"));
    set.options = plaintext_ok();
    let mut acc = ImapAccount::new(set);
    let since = FolderState { uidvalidity: 7, uidnext: 4, highest_modseq: 100, uids: vec![1, 2, 3] };
    let d = acc.sync_folder("INBOX", &since).unwrap();
    assert!(!d.reset);
    assert_eq!(d.new.len(), 1);
    assert_eq!(d.new[0].uid, 4);
    assert_eq!(d.new[0].raw, b"hello");
    assert_eq!(d.changed, vec![(3, vec!["\\Seen".to_owned()])]);
    assert_eq!(d.vanished, vec![2]);
    assert_eq!(d.state, FolderState { uidvalidity: 7, uidnext: 5, highest_modseq: 120, uids: vec![1, 3, 4] });
    acc.disconnect();
    t.join().unwrap();
}

#[test]
fn synchronising_literals_and_refusals() {
    let (port, t) = script(vec![
        (None, "* OK ready"),
        (Some("M0001 CAPABILITY"), "* CAPABILITY IMAP4rev1 UIDPLUS\nM0001 OK done"),
        (Some("M0002 LOGIN u \"p w\""), "M0002 OK [CAPABILITY IMAP4rev1 UIDPLUS] in"),
        (Some("M0003 APPEND Drafts (\\Draft) {5}"), "+ Ready"),
        (Some("hello"), "M0003 OK [APPENDUID 9 44] appended"),
        (Some("M0004 APPEND Nope {5}"), "M0004 NO [TRYCREATE] no such mailbox"),
        (Some("M0005 CREATE Caf&AOk-"), "M0005 OK created"),
        (Some("M0006 NOOP"), "* BYE server shutting down"),
    ]);
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &plaintext_ok()).unwrap();
    c.login("u", "p w").unwrap();
    assert_eq!(c.append("Drafts", &["\\Draft"], None, b"hello").unwrap(), Some((9, 44)));
    let e = c.append("Nope", &[], None, b"hello").unwrap_err();
    assert!(matches!(e, NetError::Refused(ref m) if m.contains("no such mailbox")), "{e:?}");
    assert!(c.is_usable());
    c.create("Café").unwrap();
    let e = c.noop().unwrap_err();
    assert!(matches!(e, NetError::Refused(ref m) if m.contains("shutting down")), "{e:?}");
    assert!(!c.is_usable());
    t.join().unwrap();
}

#[test]
fn xoauth2_error_challenge() {
    let (port, t) = script(vec![
        (None, "* OK [CAPABILITY IMAP4rev1 AUTH=XOAUTH2] ready"),
        (Some("M0001 AUTHENTICATE XOAUTH2"), "+ "),
        (Some("dXNlcj11QGV4YW1wbGUuY29tAWF1dGg9QmVhcmVyIHRvawEB"), "+ eyJzdGF0dXMiOiI0MDEifQ=="),
        (Some(""), "M0001 NO [AUTHENTICATIONFAILED] invalid token"),
    ]);
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &plaintext_ok()).unwrap();
    let e = c.login_with(&Credentials::oauth2("u@example.com", "tok"), "").unwrap_err();
    assert!(matches!(e, NetError::Auth(ref m) if m.contains("invalid token")), "{e:?}");
    t.join().unwrap();
}

#[test]
fn oversized_response_poisons_connection() {
    let (port, t) = script(vec![
        (None, "* PREAUTH [CAPABILITY IMAP4rev1] welcome"),
        (Some("M0001 SELECT INBOX"), "* 1 EXISTS\nM0001 OK done"),
        (Some("M0002 UID FETCH 1 (BODY.PEEK[])"), "* 1 FETCH (UID 1 BODY[] {100000}"),
    ]);
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &opts()).unwrap();
    assert!(c.is_authenticated());
    c.set_limits(Limits { max_line: 1000, max_response: 1000, ..Limits::default() });
    c.select("INBOX").unwrap();
    let e = c.uid_fetch(&SeqSet::from_ids(&[1]), "(BODY.PEEK[])").unwrap_err();
    assert!(matches!(e, NetError::TooLarge(_)), "{e:?}");
    assert!(!c.is_usable());
    assert_eq!(c.noop(), Err(NetError::Closed));
    t.join().unwrap();
}

#[test]
fn starttls_required_but_missing() {
    let (port, t) = script(vec![(None, "* OK [CAPABILITY IMAP4rev1] no tls here")]);
    let e = ImapClient::connect("127.0.0.1", port, Security::StartTls, &opts()).unwrap_err();
    assert_eq!(e, NetError::Unsupported("STARTTLS".into()));
    t.join().unwrap();
    let (port, t) = script(vec![(None, "220 mx ESMTP"), (Some("EHLO [127.0.0.1]"), "250-mx\n250 SIZE 10")]);
    let e = SmtpClient::connect(&SmtpSettings {
        options: opts(),
        ..SmtpSettings::new("127.0.0.1", port, Security::StartTls)
    })
    .unwrap_err();
    assert_eq!(e, NetError::Unsupported("STARTTLS".into()));
    t.join().unwrap();
}

#[test]
fn garbage_server_is_a_protocol_error() {
    let (port, t) = script(vec![(None, "HTTP/1.1 400 Bad Request")]);
    assert!(ImapClient::connect("127.0.0.1", port, Security::None, &opts()).is_err());
    t.join().unwrap();
    let (port, t) = script(vec![(None, "hello there")]);
    let e = SmtpClient::connect(&SmtpSettings {
        options: opts(),
        ..SmtpSettings::new("127.0.0.1", port, Security::None)
    })
    .unwrap_err();
    assert!(matches!(e, NetError::Protocol(_)), "{e:?}");
    t.join().unwrap();
}

#[test]
fn smtp_pipelining_and_partial_temporary_failure() {
    let (port, t) = script(vec![
        (None, "220-mx.example.org ESMTP\n220 welcome"),
        (
            Some("EHLO client.example.com"),
            "250-mx.example.org\n250-PIPELINING\n250-ENHANCEDSTATUSCODES\n250-8BITMIME\n250 SIZE 100000",
        ),
        (Some("MAIL FROM:<a@example.com> SIZE=13 BODY=8BITMIME"), "250 2.1.0 ok"),
        (Some("RCPT TO:<b@example.org>"), "250 2.1.5 ok"),
        (Some("RCPT TO:<c@example.org>"), "450 4.2.0 mailbox busy"),
        (Some("DATA"), "354 go ahead"),
        (Some("x"), ""),
        (Some("..y"), ""),
        (Some(""), ""),
        (Some("é"), ""),
        (Some("."), "250 2.0.0 queued as 1"),
        (Some("QUIT"), "221 2.0.0 bye"),
    ]);
    let set = SmtpSettings {
        helo_name: "client.example.com".into(),
        options: opts(),
        ..SmtpSettings::new("127.0.0.1", port, Security::None)
    };
    let mut c = SmtpClient::connect(&set).unwrap();
    assert!(c.extensions().pipelining && c.extensions().enhanced_status_codes);
    assert_eq!(c.greeting(), "mx.example.org ESMTP welcome");
    let e =
        c.send("a@example.com", &["b@example.org", "c@example.org"], "x\n.y\n\né\n".as_bytes()).unwrap_err();
    assert!(e.partially_sent());
    assert!(!e.permanent);
    assert_eq!(e.recipients[0].address, "c@example.org");
    assert_eq!(e.recipients[0].enhanced.as_deref(), Some("4.2.0"));
    assert_eq!(e.recipients[0].message, "mailbox busy");
    assert!(e.to_string().contains("c@example.org: 450"));
    c.quit().unwrap();
    t.join().unwrap();
}

#[test]
fn smtp_xoauth2_failure() {
    let (port, t) = script(vec![
        (None, "220 mx ESMTP"),
        (Some("EHLO "), "250-mx\n250 AUTH LOGIN XOAUTH2"),
        (
            Some("AUTH XOAUTH2 dXNlcj11QGV4YW1wbGUuY29tAWF1dGg9QmVhcmVyIHRvawEB"),
            "334 eyJzdGF0dXMiOiI0MDEifQ==",
        ),
        (Some(""), "535 5.7.8 Username and Password not accepted"),
    ]);
    let set = SmtpSettings {
        credentials: Some(Credentials::oauth2("u@example.com", "tok")),
        options: plaintext_ok(),
        ..SmtpSettings::new("127.0.0.1", port, Security::None)
    };
    let e = SmtpClient::connect(&set).unwrap_err();
    assert!(matches!(e, NetError::Auth(ref m) if m.starts_with("535")), "{e:?}");
    t.join().unwrap();
}

/// Options that let tests log in over a plain socket to a scripted server.
fn plaintext_ok() -> ConnectOptions {
    ConnectOptions { allow_plaintext_auth: true, ..opts() }
}

#[test]
fn credentials_are_never_sent_over_plaintext_by_default() {
    // IMAP: no LOGIN or AUTHENTICATE line may reach the server.
    let (port, t) = script(vec![(None, "* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN SASL-IR] hi")]);
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &opts()).unwrap();
    let e = c.login_with(&Credentials::password("u@example.com", "pw"), "").unwrap_err();
    assert!(matches!(e, NetError::Insecure(_)), "{e:?}");
    assert!(matches!(c.login("u@example.com", "pw"), Err(NetError::Insecure(_))));
    assert!(matches!(c.login_with(&Credentials::oauth2("u", "tok"), "XOAUTH2"), Err(NetError::Insecure(_))));
    drop(c);
    assert!(t.join().unwrap().is_empty());
    // SMTP: AUTH is refused locally after EHLO.
    let (port, t) = script(vec![(None, "220 mx ESMTP"), (Some("EHLO "), "250-mx\n250 AUTH PLAIN LOGIN")]);
    let e = SmtpClient::connect(&SmtpSettings {
        credentials: Some(Credentials::password("u@example.com", "pw")),
        options: opts(),
        ..SmtpSettings::new("127.0.0.1", port, Security::None)
    })
    .unwrap_err();
    assert!(matches!(e, NetError::Insecure(_)), "{e:?}");
    assert_eq!(t.join().unwrap().len(), 1);
}

#[test]
fn sasl_error_challenges_follow_the_mechanism() {
    // RFC 7628 section 3.2.3: an OAUTHBEARER error challenge is answered with a single %x01 ("AQ==").
    let (port, t) = script(vec![
        (None, "* OK [CAPABILITY IMAP4rev1 AUTH=OAUTHBEARER SASL-IR] ready"),
        (Some("M0001 AUTHENTICATE OAUTHBEARER "), "+ eyJzdGF0dXMiOiJpbnZhbGlkX3Rva2VuIn0="),
        (Some("AQ=="), "M0001 NO [AUTHENTICATIONFAILED] invalid token"),
    ]);
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &plaintext_ok()).unwrap();
    let e = c.login_with(&Credentials::oauth2("u@example.com", "tok"), "").unwrap_err();
    assert!(matches!(e, NetError::Auth(ref m) if m.contains("invalid token")), "{e:?}");
    t.join().unwrap();
    // Any other unexpected challenge cancels the exchange with "*" (RFC 9051 section 6.2.2, RFC 4954).
    let (port, t) = script(vec![
        (None, "* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN SASL-IR] ready"),
        (Some("M0001 AUTHENTICATE PLAIN "), "+ more?"),
        (Some("*"), "M0001 BAD cancelled"),
    ]);
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &plaintext_ok()).unwrap();
    assert!(matches!(c.login_with(&Credentials::password("u", "p"), ""), Err(NetError::Auth(_))));
    let seen = t.join().unwrap();
    assert_eq!(seen.last().map(String::as_str), Some("*"));
    // SMTP, same rules.
    let (port, t) = script(vec![
        (None, "220 mx ESMTP"),
        (Some("EHLO "), "250-mx\n250 AUTH OAUTHBEARER"),
        (Some("AUTH OAUTHBEARER "), "334 eyJzdGF0dXMiOiJpbnZhbGlkX3Rva2VuIn0="),
        (Some("AQ=="), "535 5.7.8 invalid token"),
    ]);
    let e = SmtpClient::connect(&SmtpSettings {
        credentials: Some(Credentials::oauth2("u@example.com", "tok")),
        options: plaintext_ok(),
        ..SmtpSettings::new("127.0.0.1", port, Security::None)
    })
    .unwrap_err();
    assert!(matches!(e, NetError::Auth(ref m) if m.starts_with("535")), "{e:?}");
    t.join().unwrap();
    let (port, t) = script(vec![
        (None, "220 mx ESMTP"),
        (Some("EHLO "), "250-mx\n250 AUTH PLAIN"),
        (Some("AUTH PLAIN "), "334 what?"),
        (Some("*"), "501 5.7.0 cancelled"),
    ]);
    let e = SmtpClient::connect(&SmtpSettings {
        credentials: Some(Credentials::password("u", "p")),
        options: plaintext_ok(),
        ..SmtpSettings::new("127.0.0.1", port, Security::None)
    })
    .unwrap_err();
    assert!(matches!(e, NetError::Auth(_)), "{e:?}");
    t.join().unwrap();
}

#[test]
fn endless_untagged_responses_are_capped() {
    let flood: &'static str = Box::leak("* 1 EXISTS\n".repeat(50).trim_end().to_owned().into_boxed_str());
    let (port, t) = script(vec![(None, "* PREAUTH [CAPABILITY IMAP4rev1] hi"), (Some("M0001 NOOP"), flood)]);
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &opts()).unwrap();
    c.set_limits(Limits { max_untagged: 10, ..Limits::default() });
    assert!(matches!(c.noop(), Err(NetError::TooLarge(_))));
    assert!(!c.is_usable());
    t.join().unwrap();
}

#[test]
fn non_ascii_login_uses_a_literal_unless_utf8_is_enabled() {
    // Advertising IMAP4rev2 is not enough: until it is enabled the session is IMAP4rev1, where quoted
    // strings are 7-bit (RFC 9051 appendix E).
    let (port, t) = script(vec![
        (None, "* OK [CAPABILITY IMAP4rev1 IMAP4rev2 LITERAL+] hi"),
        (Some("M0001 LOGIN u {5+}"), ""),
        (Some("päss"), "M0001 OK [CAPABILITY IMAP4rev1 IMAP4rev2] in"),
    ]);
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &plaintext_ok()).unwrap();
    c.login("u", "päss").unwrap();
    t.join().unwrap();
}

#[test]
fn append_with_nul_needs_binary() {
    let (port, t) = script(vec![(None, "* PREAUTH [CAPABILITY IMAP4rev1] hi")]);
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &opts()).unwrap();
    assert!(matches!(c.append("INBOX", &[], None, b"a\0b"), Err(NetError::Invalid(_))));
    assert!(c.is_usable());
    drop(c);
    assert!(t.join().unwrap().is_empty());
}

#[test]
fn smtp_refuses_8bit_data_without_8bitmime() {
    // RFC 6152: 8-bit content may only go to a server that advertised 8BITMIME.
    let (port, t) = script(vec![(None, "220 mx ESMTP"), (Some("EHLO "), "250-mx\n250 SIZE 1000")]);
    let mut c = SmtpClient::connect(&SmtpSettings {
        options: opts(),
        ..SmtpSettings::new("127.0.0.1", port, Security::None)
    })
    .unwrap();
    let e = c.send("a@example.com", &["b@example.org"], "Subject: x\r\n\r\ncafé\r\n".as_bytes()).unwrap_err();
    assert!(e.permanent && !e.partially_sent(), "{e:?}");
    drop(c);
    assert_eq!(t.join().unwrap().len(), 1);
}

#[test]
fn data_injected_after_starttls_reply_is_refused() {
    // CVE-2011-0411 class: plaintext a man in the middle appends after the STARTTLS reply must not be read
    // as if it came over TLS.
    let (port, t) = script(vec![
        (None, "* OK [CAPABILITY IMAP4rev1 STARTTLS] hi"),
        (Some("M0001 STARTTLS"), "M0001 OK begin TLS\n* CAPABILITY IMAP4rev1 AUTH=PLAIN"),
    ]);
    let e = ImapClient::connect("127.0.0.1", port, Security::StartTls, &opts()).unwrap_err();
    assert!(matches!(e, NetError::Protocol(_)), "{e:?}");
    t.join().unwrap();
    let (port, t) = script(vec![
        (None, "220 mx ESMTP"),
        (Some("EHLO "), "250-mx\n250 STARTTLS"),
        (Some("STARTTLS"), "220 go ahead\n250-mx\n250 AUTH PLAIN"),
    ]);
    let e = SmtpClient::connect(&SmtpSettings {
        options: opts(),
        ..SmtpSettings::new("127.0.0.1", port, Security::StartTls)
    })
    .unwrap_err();
    assert!(matches!(e, NetError::Protocol(_)), "{e:?}");
    t.join().unwrap();
}

#[test]
fn status_text_ending_in_braces_is_not_a_literal() {
    // RFC 9051 section 9: resp-text is plain text and never carries a literal, so a NO or OK whose text
    // happens to end in `{3}` must not make the client wait for 3 more bytes.
    let (port, t) = script(vec![
        (None, "* PREAUTH [CAPABILITY IMAP4rev1] hi"),
        (Some("M0001 CREATE \"Box{3}\""), "M0001 NO [ALREADYEXISTS] Mailbox exists: Box{3}"),
        (Some("M0002 NOOP"), "* OK still here {2}\n+ odd {4}\nM0002 OK done"),
    ]);
    let quick = ConnectOptions { io_timeout: Duration::from_secs(3), ..ConnectOptions::default() };
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &quick).unwrap();
    let e = c.create("Box{3}").unwrap_err();
    assert!(matches!(e, NetError::Refused(ref m) if m.ends_with("Box{3}")), "{e:?}");
    assert!(c.is_usable());
    c.noop().unwrap();
    t.join().unwrap();
}

#[test]
fn imap4rev2_enabled_uses_utf8_mailbox_names() {
    // RFC 9051 section 5.1: once IMAP4rev2 is enabled, mailbox names are UTF-8 on the wire, not modified
    // UTF-7 (and `&` is an ordinary character).
    let (port, t) = script(vec![
        (None, "* PREAUTH [CAPABILITY IMAP4rev2 ENABLE] hi"),
        (Some("M0001 ENABLE IMAP4rev2"), "* ENABLED IMAP4rev2\nM0001 OK enabled"),
        (Some("M0002 CREATE \"Café\""), "M0002 OK created"),
        (Some("M0003 LIST \"\" *"), "* LIST () \"/\" \"Entwürfe\"\n* LIST () \"/\" \"A&-B\"\nM0003 OK done"),
    ]);
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &opts()).unwrap();
    c.enable(&["IMAP4rev2"]).unwrap();
    c.create("Café").unwrap();
    let names: Vec<String> = c.list("", "*").unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, vec!["Entwürfe".to_owned(), "A&-B".to_owned()]);
    t.join().unwrap();
}

#[test]
fn unexpected_continuations_are_capped() {
    let flood: &'static str = Box::leak("+ more\n".repeat(50).trim_end().to_owned().into_boxed_str());
    let mut steps = vec![(None, "* PREAUTH [CAPABILITY IMAP4rev1] hi"), (Some("M0001 NOOP"), flood)];
    // The client answers a few stray `+` with empty lines before giving up; read them so the socket closes
    // cleanly.
    steps.extend(std::iter::repeat_n((Some(""), ""), 8));
    let (port, t) = script(steps);
    let mut c = ImapClient::connect("127.0.0.1", port, Security::None, &opts()).unwrap();
    assert!(matches!(c.noop(), Err(NetError::Protocol(_))));
    assert!(!c.is_usable());
    t.join().unwrap();
}

#[test]
fn smtp_refuses_an_empty_recipient() {
    // RFC 5321 section 4.1.1.3: RCPT TO needs a forward-path; `<>` is only valid as a reverse-path.
    let (port, t) = script(vec![(None, "220 mx ESMTP"), (Some("EHLO "), "250-mx\n250 PIPELINING")]);
    let mut c = SmtpClient::connect(&SmtpSettings {
        options: opts(),
        ..SmtpSettings::new("127.0.0.1", port, Security::None)
    })
    .unwrap();
    let e = c.send("", &["b@example.org", ""], b"Subject: x\r\n\r\nhi\r\n").unwrap_err();
    assert!(e.permanent && !e.partially_sent(), "{e:?}");
    drop(c);
    assert_eq!(t.join().unwrap().len(), 1);
}

#[test]
fn smtp_utf8_headers_need_smtputf8() {
    // RFC 6531 section 3.4 / RFC 6532: a message with UTF-8 header fields goes only to an SMTPUTF8 server,
    // and with the SMTPUTF8 parameter on MAIL FROM.
    let (port, t) = script(vec![(None, "220 mx ESMTP"), (Some("EHLO "), "250-mx\n250 8BITMIME")]);
    let mut c = SmtpClient::connect(&SmtpSettings {
        options: opts(),
        ..SmtpSettings::new("127.0.0.1", port, Security::None)
    })
    .unwrap();
    let msg = "Subject: café\r\n\r\nhi\r\n".as_bytes();
    let e = c.send("a@example.com", &["b@example.org"], msg).unwrap_err();
    assert!(e.permanent && !e.partially_sent(), "{e:?}");
    drop(c);
    assert_eq!(t.join().unwrap().len(), 1);

    let (port, t) = script(vec![
        (None, "220 mx ESMTP"),
        (Some("EHLO "), "250-mx\n250-8BITMIME\n250-SMTPUTF8\n250 SIZE 1000"),
        (Some("MAIL FROM:<a@example.com> SIZE=22 BODY=8BITMIME SMTPUTF8"), "250 ok"),
        (Some("RCPT TO:<b@example.org>"), "250 ok"),
        (Some("DATA"), "354 go"),
        (Some("Subject: café"), ""),
        (Some(""), ""),
        (Some("hi"), ""),
        (Some("."), "250 queued"),
    ]);
    let mut c = SmtpClient::connect(&SmtpSettings {
        options: opts(),
        ..SmtpSettings::new("127.0.0.1", port, Security::None)
    })
    .unwrap();
    // Bare LF line endings: SIZE counts the CRLF form that goes on the wire (RFC 1870 section 6).
    c.send("a@example.com", &["b@example.org"], "Subject: café\n\nhi\n".as_bytes()).unwrap();
    drop(c);
    t.join().unwrap();
}
