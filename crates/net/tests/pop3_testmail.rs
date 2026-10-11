//! The POP3 client against the in-process POP3 server from `mailcraft-testmail` (127.0.0.1 only).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_net::pop3::{Pop3Client, Pop3Settings};
use mailcraft_net::{ConnectOptions, Credentials, NetError, Security};
use mailcraft_testmail::{Config, TestServers};

const USER: &str = "alice@example.com";
const PASS: &str = "correct horse";

fn servers(cfg: Config) -> TestServers {
    TestServers::start(Config { users: vec![(USER.into(), PASS.into())], add_trace_headers: false, ..cfg })
        .unwrap()
}

fn settings(s: &TestServers, security: Security, pass: &str) -> Pop3Settings {
    let port = if security == Security::Tls { s.pop3s_port } else { s.pop3_port };
    let mut set = Pop3Settings::new("127.0.0.1", port, security, Credentials::password(USER, pass));
    set.options = ConnectOptions {
        io_timeout: Duration::from_secs(20),
        ..ConnectOptions::trusting(s.cert_der.clone())
    };
    set
}

#[test]
fn receive_over_tls_and_starttls_then_delete() {
    let s = servers(Config::default());
    s.seed_message(
        USER,
        "INBOX",
        b"Subject: a\r\nMessage-ID: <a@example.org>\r\n\r\nfirst\r\n.\r\n..x\r\n",
        &[],
    )
    .unwrap();
    s.seed_message(USER, "INBOX", b"Subject: b\r\n\r\nsecond\r\n", &[]).unwrap();
    let mut c = Pop3Client::connect(&settings(&s, Security::Tls, PASS)).unwrap();
    assert!(c.is_tls());
    let ids = c.uidl().unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(
        c.retr(1).unwrap(),
        b"Subject: a\r\nMessage-ID: <a@example.org>\r\n\r\nfirst\r\n.\r\n..x\r\n".to_vec()
    );
    assert_eq!(c.top(2, 0).unwrap(), b"Subject: b\r\n\r\n".to_vec());
    c.dele(1).unwrap();
    c.quit().unwrap();
    let mut c = Pop3Client::connect(&settings(&s, Security::StartTls, PASS)).unwrap();
    assert!(c.is_tls());
    let left = c.uidl().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].1, ids[1].1, "unique ids are stable across sessions");
    c.quit().unwrap();
    assert!(matches!(Pop3Client::connect(&settings(&s, Security::Tls, "wrong")), Err(NetError::Auth(_))));
    // No password over plain text unless allowed.
    assert!(matches!(Pop3Client::connect(&settings(&s, Security::None, PASS)), Err(NetError::Insecure(_))));
}
