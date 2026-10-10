//! Never-panic checks: clients talking to servers that send random bytes (127.0.0.1 only).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use mailcraft_net::http::HttpClient;
use mailcraft_net::imap::{ImapClient, SeqSet};
use mailcraft_net::smtp::{SmtpClient, SmtpSettings};
use mailcraft_net::{ConnectOptions, Security};
use proptest::prelude::*;

/// Serves `prefix` then `junk` to one client, draining whatever the client sends, then closes.
fn junk_server(prefix: &'static [u8], junk: Vec<u8>) -> (u16, std::thread::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        let Ok((mut s, _)) = l.accept() else { return };
        let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
        let _ = s.write_all(prefix);
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
        let _ = s.write_all(&junk);
        let _ = s.read(&mut buf);
    });
    (port, t)
}

fn opts() -> ConnectOptions {
    ConnectOptions { io_timeout: Duration::from_secs(2), ..ConnectOptions::default() }
}

fn junk() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(
        prop_oneof![
            Just(b"* ".to_vec()),
            Just(b"M0001 OK done\r\n".to_vec()),
            Just(b"M0001 NO no\r\n".to_vec()),
            Just(b"{5}\r\n".to_vec()),
            Just(b"{99999999}\r\n".to_vec()),
            Just(b"\r\n".to_vec()),
            Just(b"(".to_vec()),
            Just(b")".to_vec()),
            Just(b"FETCH (UID 1 BODY[] ".to_vec()),
            Just(b"+ go\r\n".to_vec()),
            Just(b"250-x\r\n".to_vec()),
            Just(b"250 ok\r\n".to_vec()),
            Just(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc".to_vec()),
            Just(b"Transfer-Encoding: chunked\r\n\r\nzz\r\n".to_vec()),
            proptest::collection::vec(any::<u8>(), 0..20),
        ],
        0..20,
    )
    .prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn imap_client_survives_junk(j in junk()) {
        let (port, t) = junk_server(b"* PREAUTH [CAPABILITY IMAP4rev1 IDLE] hi\r\n", j);
        if let Ok(mut c) = ImapClient::connect("127.0.0.1", port, Security::None, &opts()) {
            let _ = c.select("INBOX");
            let _ = c.uid_fetch(&SeqSet::from_ids(&[1]), "(UID BODY.PEEK[])");
            let _ = c.noop();
        }
        t.join().unwrap();
    }

    #[test]
    fn smtp_client_survives_junk(j in junk()) {
        let (port, t) = junk_server(b"220 mx ESMTP\r\n", j);
        let set = SmtpSettings { options: opts(), ..SmtpSettings::new("127.0.0.1", port, Security::None) };
        if let Ok(mut c) = SmtpClient::connect(&set) {
            let _ = c.send("a@example.com", &["b@example.org"], b"hi\r\n");
        }
        t.join().unwrap();
    }

    #[test]
    fn http_client_survives_junk(j in junk()) {
        let (port, t) = junk_server(b"", j);
        let c = HttpClient { max_body: 4096, ..HttpClient::new(opts()) };
        let _ = c.get(&format!("http://127.0.0.1:{port}/"), &[]);
        t.join().unwrap();
    }
}
