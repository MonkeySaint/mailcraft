//! SMTP submission server and send → receive round trips, spoken to in raw protocol.

mod common;

use common::*;
use mailcraft_testmail::Config;

fn smtp_login(port: u16, user: &str, pw: &str) -> Client {
    let mut c = Client::connect(port);
    assert_eq!(c.smtp_reply().0, 220);
    c.smtp_expect("EHLO client.example.com", 250);
    c.smtp_expect(&format!("AUTH PLAIN {}", b64(format!("\0{user}\0{pw}").as_bytes())), 235);
    c
}

#[test]
fn ehlo_and_auth() {
    let s = servers();
    let mut c = Client::connect(s.smtp_port);
    let (code, greet) = c.smtp_reply();
    assert_eq!(code, 220, "{greet}");
    let ehlo = c.smtp_expect("EHLO client.example.com", 250);
    for ext in
        ["PIPELINING", "SIZE 26214400", "8BITMIME", "STARTTLS", "AUTH PLAIN LOGIN", "ENHANCEDSTATUSCODES"]
    {
        assert!(ehlo.contains(ext), "missing {ext}: {ehlo}");
    }
    c.smtp_expect("MAIL FROM:<alice@example.com>", 530);
    c.smtp_expect(&format!("AUTH PLAIN {}", b64(b"\0alice@example.com\0wrong")), 535);
    c.smtp_expect("AUTH CRAM-MD5", 504);
    // AUTH LOGIN with challenges.
    c.smtp_expect("AUTH LOGIN", 334);
    c.smtp_expect(&b64(ALICE.as_bytes()), 334);
    c.smtp_expect(&b64(b"alice-pw"), 235);
    c.smtp_expect("AUTH PLAIN", 503);
    c.smtp_expect("NOOP", 250);
    c.smtp_expect("VRFY bob", 252);
    c.smtp_expect("BOGUS", 502);
    c.smtp_expect("QUIT", 221);

    let mut c = Client::connect(s.smtp_port);
    c.smtp_reply();
    c.smtp_expect("MAIL FROM:<a@example.com>", 503);
    c.smtp_expect("EHLO x", 250);
    c.smtp_expect("AUTH PLAIN", 334);
    c.smtp_expect("*", 501);
}

#[test]
fn send_delivers_to_local_inbox_with_dot_unstuffing() {
    let s =
        servers_with(Config { reject_recipients: vec!["bounce@example.net".into()], ..Config::default() });
    let mut c = smtp_login(s.smtp_port, ALICE, "alice-pw");
    c.smtp_expect("MAIL FROM:<alice@example.com> SIZE=200 BODY=8BITMIME", 250);
    c.smtp_expect("RCPT TO:<bob@example.com>", 250);
    let r = c.smtp_expect("RCPT TO:<bounce@example.net>", 550);
    assert!(r.contains("5.1.1"), "{r}");
    c.smtp_expect("RCPT TO:<someone@example.org>", 250);
    c.smtp_expect("DATA", 354);
    c.send_str(
        "From: alice@example.com\r\nTo: bob@example.com\r\nSubject: Dots\r\n\r\nline one\r\n..leading dot\r\n.\r\n",
    );
    let (code, text) = c.smtp_reply();
    assert_eq!(code, 250, "{text}");

    let subs = s.submissions();
    assert_eq!(subs.len(), 1);
    let sub = &subs[0];
    assert_eq!(sub.auth_user.as_deref(), Some(ALICE));
    assert_eq!(sub.mail_from, ALICE);
    assert_eq!(sub.rcpt_to, vec![BOB.to_owned(), "someone@example.org".to_owned()]);
    assert_eq!(sub.delivered_to, vec![BOB.to_owned()]);
    assert!(
        sub.data.ends_with(b"\r\n\r\nline one\r\n.leading dot\r\n"),
        "{:?}",
        String::from_utf8_lossy(&sub.data)
    );

    let inbox = s.messages(BOB, "INBOX").unwrap();
    assert_eq!(inbox.len(), 1);
    let raw = String::from_utf8_lossy(&inbox[0].raw).into_owned();
    assert!(
        raw.starts_with(
            "Return-Path: <alice@example.com>\r\nReceived: from client.example.com by localhost with ESMTPA"
        ),
        "{raw}"
    );
    assert!(raw.ends_with("\r\n.leading dot\r\n"), "{raw}");
    assert!(inbox[0].flags.is_empty());
    assert!(s.messages(ALICE, "INBOX").unwrap().is_empty());

    // RSET clears the transaction; DATA without recipients fails.
    c.smtp_expect("MAIL FROM:<alice@example.com>", 250);
    c.smtp_expect("RSET", 250);
    c.smtp_expect("RCPT TO:<bob@example.com>", 503);
    c.smtp_expect("MAIL FROM:<alice@example.com>", 250);
    c.smtp_expect("DATA", 554);
}

#[test]
fn size_limits() {
    let s = servers_with(Config { max_message_size: 1000, smtp_require_auth: false, ..Config::default() });
    let mut c = Client::connect(s.smtp_port);
    c.smtp_reply();
    let ehlo = c.smtp_expect("EHLO x", 250);
    assert!(ehlo.contains("SIZE 1000"), "{ehlo}");
    c.smtp_expect("MAIL FROM:<a@example.org> SIZE=5000", 552);
    c.smtp_expect("MAIL FROM:<a@example.org>", 250);
    c.smtp_expect("RCPT TO:<bob@example.com>", 250);
    c.smtp_expect("DATA", 354);
    let big = "x".repeat(200);
    for _ in 0..10 {
        c.send_str(&format!("{big}\r\n"));
    }
    c.send_str(".\r\n");
    assert_eq!(c.smtp_reply().0, 552);
    assert!(s.messages(BOB, "INBOX").unwrap().is_empty());
    assert!(s.submissions().is_empty());
    // The session is still usable.
    c.smtp_expect("MAIL FROM:<>", 250);
    c.smtp_expect("RCPT TO:<bob@example.com>", 250);
    c.smtp_expect("DATA", 354);
    c.send_str("Subject: small\r\n\r\nok\r\n.\r\n");
    assert_eq!(c.smtp_reply().0, 250);
    assert_eq!(s.messages(BOB, "INBOX").unwrap().len(), 1);
}

#[test]
fn starttls_and_smtps() {
    let s = servers_with(Config { auth_requires_tls: true, ..Config::default() });
    let tls = s.client_tls_config().unwrap();
    let mut c = Client::connect(s.smtp_port);
    c.smtp_reply();
    let ehlo = c.smtp_expect("EHLO x", 250);
    assert!(ehlo.contains("STARTTLS") && !ehlo.contains("AUTH"), "{ehlo}");
    c.smtp_expect(&format!("AUTH PLAIN {}", b64(b"\0alice@example.com\0alice-pw")), 538);
    c.smtp_expect("STARTTLS", 220);
    c.upgrade(tls.clone());
    let ehlo = c.smtp_expect("EHLO x", 250);
    assert!(!ehlo.contains("STARTTLS") && ehlo.contains("AUTH PLAIN LOGIN"), "{ehlo}");
    c.smtp_expect(&format!("AUTH PLAIN {}", b64(b"\0alice@example.com\0alice-pw")), 235);

    let mut c = Client::connect_tls(s.smtps_port, tls);
    assert_eq!(c.smtp_reply().0, 220);
    c.smtp_expect("EHLO x", 250);
    c.smtp_expect(&format!("AUTH PLAIN {}", b64(b"\0bob@example.com\0bob-pw")), 235);
    c.smtp_expect("MAIL FROM:<bob@example.com>", 250);
    c.smtp_expect("RCPT TO:<alice@example.com>", 250);
    c.smtp_expect("DATA", 354);
    c.send_str("Subject: over TLS\r\n\r\nhi\r\n.\r\n");
    assert_eq!(c.smtp_reply().0, 250);
    let raw = s.messages(ALICE, "INBOX").unwrap()[0].raw.clone();
    assert!(String::from_utf8_lossy(&raw).contains("with ESMTPSA"));
}

#[test]
fn send_then_receive_via_imap_idle() {
    let s = servers();
    let mut bob = Client::imap_login(s.imap_port, BOB, "bob-pw");
    bob.ok("SELECT INBOX");
    bob.send_str("i1 IDLE\r\n");
    assert!(bob.line().starts_with("+ "));

    let mut c = smtp_login(s.smtp_port, ALICE, "alice-pw");
    c.smtp_expect("MAIL FROM:<alice@example.com>", 250);
    c.smtp_expect("RCPT TO:<BOB@example.com>", 250);
    c.smtp_expect("DATA", 354);
    c.send_str("From: Alice <alice@example.com>\r\nTo: bob@example.com\r\nSubject: Round trip\r\n\r\nHello Bob\r\n.\r\n");
    assert_eq!(c.smtp_reply().0, 250);
    c.smtp_expect("QUIT", 221);

    assert_eq!(bob.line(), "* 1 EXISTS\r\n");
    bob.send_str("DONE\r\n");
    bob.until_tagged("i1");
    let r = bob.ok("UID FETCH 1 (ENVELOPE BODY.PEEK[TEXT])");
    assert!(r.contains("\"Round trip\" ((\"Alice\" NIL \"alice\" \"example.com\"))"), "{r}");
    assert!(r.contains("BODY[TEXT] {11}\r\nHello Bob\r\n"), "{r}");
    // Alice files her copy in Sent with APPEND, like a client would.
    let mut alice = Client::imap_login(s.imap_port, ALICE, "alice-pw");
    alice.send_str("s1 APPEND Sent (\\Seen) {12+}\r\nSubject: x\r\n\r\n");
    assert!(alice.until_tagged("s1").1.starts_with("s1 OK [APPENDUID"));
    assert_eq!(s.messages(ALICE, "Sent").unwrap().len(), 1);
}

#[test]
fn data_ends_only_at_crlf_dot_crlf_and_bare_line_endings_are_refused() {
    // SMTP smuggling: a bare-LF (or bare-CR) "." line must not end the message, and a message carrying bare
    // line endings is refused rather than delivered (RFC 5321 sections 2.3.8 and 4.1.1.4).
    let s = servers();
    let mut c = smtp_login(s.smtp_port, ALICE, "alice-pw");
    c.smtp_expect("MAIL FROM:<alice@example.com>", 250);
    c.smtp_expect("RCPT TO:<bob@example.com>", 250);
    c.smtp_expect("DATA", 354);
    c.send_str("Subject: smuggle\r\n\r\nhello\n.\nMAIL FROM:<evil@example.com>\r\nx\r.\rRCPT TO:<x@example.com>\r\n.\r\n");
    let (code, text) = c.smtp_reply();
    assert!((500..600).contains(&code), "bare line endings must be refused: {text}");
    assert!(s.submissions().is_empty(), "nothing delivered");
    // The session is still in sync: exactly one reply was sent for the whole DATA.
    c.smtp_expect("NOOP", 250);
    c.smtp_expect("MAIL FROM:<alice@example.com>", 250);
    c.smtp_expect("RCPT TO:<bob@example.com>", 250);
    c.smtp_expect("DATA", 354);
    c.send_str("Subject: fine\r\n\r\nhello\r\n.\r\n");
    assert_eq!(c.smtp_reply().0, 250);
    assert_eq!(s.submissions().len(), 1);
}

#[test]
fn starttls_takes_no_parameters_and_helo_names_are_checked() {
    // RFC 3207 section 4: "501 Syntax error (no parameters allowed)".
    let s = servers();
    let mut c = Client::connect(s.smtp_port);
    c.smtp_reply();
    c.smtp_expect("EHLO x", 250);
    c.smtp_expect("STARTTLS now", 501);
    // A HELO name ends up in the Received trace field; control characters (a bare CR) are refused.
    c.smtp_expect("EHLO bad\rReceived: forged", 501);
    c.smtp_expect("HELO bad\x07name", 501);
    c.smtp_expect("EHLO [127.0.0.1]", 250);
    c.smtp_expect("STARTTLS", 220);
}
