//! POP3 server: raw protocol (RFC 1939, RFC 2449 CAPA, RFC 2595 STLS) against the in-process server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use common::{ALICE, Client, b64, servers, servers_with};
use mailcraft_testmail::Config;

fn ok(c: &mut Client, cmd: &str) -> String {
    c.send_str(&format!("{cmd}\r\n"));
    let l = c.line();
    assert!(l.starts_with("+OK"), "{cmd}: {l}");
    l
}

fn multiline(c: &mut Client) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let l = c.line();
        assert!(!l.is_empty(), "EOF in multi-line answer");
        if l == ".\r\n" {
            return out;
        }
        out.push(l.trim_end_matches(['\r', '\n']).to_owned());
    }
}

#[test]
fn login_list_uidl_retr_top_dele_quit() {
    let srv = servers();
    srv.seed_message(ALICE, "INBOX", b"Subject: one\r\n\r\n.leading dot\r\nbody\r\n", &[]).unwrap();
    srv.seed_message(ALICE, "INBOX", b"Subject: two\r\n\r\nsecond\r\n", &["\\Seen"]).unwrap();
    let mut c = Client::connect(srv.pop3_port);
    assert!(c.line().starts_with("+OK"));
    ok(&mut c, "CAPA");
    let caps = multiline(&mut c);
    assert!(caps.iter().any(|l| l == "UIDL") && caps.iter().any(|l| l == "STLS"), "{caps:?}");
    c.send_str("STAT\r\n");
    assert!(c.line().starts_with("-ERR"), "STAT needs a login");
    ok(&mut c, "USER alice@example.com");
    c.send_str("PASS wrong\r\n");
    assert!(c.line().starts_with("-ERR [AUTH]"));
    ok(&mut c, "USER alice@example.com");
    ok(&mut c, "PASS alice-pw");
    assert!(ok(&mut c, "STAT").starts_with("+OK 2 "));
    ok(&mut c, "UIDL");
    let uidl = multiline(&mut c);
    assert_eq!(uidl.len(), 2);
    assert!(uidl[0].starts_with("1 ") && uidl[1].starts_with("2 "));
    ok(&mut c, "RETR 1");
    assert_eq!(multiline(&mut c), vec!["Subject: one", "", "..leading dot", "body"]);
    ok(&mut c, "TOP 2 0");
    assert_eq!(multiline(&mut c), vec!["Subject: two", ""]);
    ok(&mut c, "DELE 1");
    c.send_str("RETR 1\r\n");
    assert!(c.line().starts_with("-ERR"), "deleted message is gone");
    ok(&mut c, "RSET");
    ok(&mut c, "DELE 1");
    // Same unique ids in a second session until QUIT commits the deletion.
    let mut c2 = Client::connect(srv.pop3_port);
    c2.line();
    ok(&mut c2, "USER alice@example.com");
    ok(&mut c2, "PASS alice-pw");
    ok(&mut c2, "UIDL");
    assert_eq!(multiline(&mut c2), uidl);
    ok(&mut c2, "QUIT");
    ok(&mut c, "QUIT");
    assert_eq!(srv.messages(ALICE, "INBOX").unwrap().len(), 1);
}

#[test]
fn stls_and_auth_plain() {
    let srv = servers_with(Config { auth_requires_tls: true, ..Config::default() });
    let mut c = Client::connect(srv.pop3_port);
    c.line();
    c.send_str("USER alice@example.com\r\n");
    assert!(c.line().starts_with("-ERR"), "no login before TLS");
    ok(&mut c, "STLS");
    c.upgrade(srv.client_tls_config().unwrap());
    let cred = b64(b"\0alice@example.com\0alice-pw");
    ok(&mut c, &format!("AUTH PLAIN {cred}"));
    ok(&mut c, "LIST");
    assert!(multiline(&mut c).is_empty());
    ok(&mut c, "QUIT");

    let mut t = Client::connect_tls(srv.pop3s_port, srv.client_tls_config().unwrap());
    assert!(t.line().starts_with("+OK"));
    t.send_str("AUTH PLAIN\r\n");
    assert!(t.line().starts_with("+ "));
    t.send_str(&format!("{cred}\r\n"));
    assert!(t.line().starts_with("+OK"));
}
