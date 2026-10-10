//! Raw-protocol test client (plain TCP or rustls) for the integration tests.
#![allow(dead_code)]
// Test-only helper module: failing loudly is the point.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use mailcraft_testmail::rustls::pki_types::ServerName;
use mailcraft_testmail::rustls::{ClientConfig, ClientConnection, StreamOwned};
use mailcraft_testmail::{Config, TestServers};

pub const ALICE: &str = "alice@example.com";
pub const BOB: &str = "bob@example.com";

pub fn servers() -> TestServers {
    servers_with(Config::default())
}

pub fn servers_with(mut cfg: Config) -> TestServers {
    cfg.users = vec![(ALICE.into(), "alice-pw".into()), (BOB.into(), "bob-pw".into())];
    TestServers::start(cfg).unwrap()
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
    Gone,
}

pub struct Client {
    s: Stream,
    buf: Vec<u8>,
    tag: u32,
}

impl Client {
    pub fn connect(port: u16) -> Client {
        let tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Client { s: Stream::Plain(tcp), buf: Vec::new(), tag: 0 }
    }

    pub fn connect_tls(port: u16, cfg: Arc<ClientConfig>) -> Client {
        let mut c = Client::connect(port);
        c.upgrade(cfg);
        c
    }

    pub fn upgrade(&mut self, cfg: Arc<ClientConfig>) {
        assert!(self.buf.is_empty(), "unread data before TLS upgrade");
        let Stream::Plain(tcp) = std::mem::replace(&mut self.s, Stream::Gone) else { panic!("not plain") };
        let conn = ClientConnection::new(cfg, ServerName::try_from("localhost").unwrap()).unwrap();
        self.s = Stream::Tls(Box::new(StreamOwned::new(conn, tcp)));
    }

    pub fn send(&mut self, data: &[u8]) {
        match &mut self.s {
            Stream::Plain(s) => s.write_all(data).unwrap(),
            Stream::Tls(s) => {
                s.write_all(data).unwrap();
                s.flush().unwrap();
            }
            Stream::Gone => panic!("gone"),
        }
    }

    pub fn send_str(&mut self, s: &str) {
        self.send(s.as_bytes());
    }

    fn fill(&mut self) -> usize {
        let mut tmp = [0u8; 8192];
        let n = match &mut self.s {
            Stream::Plain(s) => s.read(&mut tmp),
            Stream::Tls(s) => s.read(&mut tmp),
            Stream::Gone => Ok(0),
        };
        let n = match n {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
            Err(e) => panic!("read failed: {e}; buffered: {:?}", String::from_utf8_lossy(&self.buf)),
        };
        self.buf.extend_from_slice(&tmp[..n]);
        n
    }

    /// One line including CRLF; empty string on EOF.
    pub fn line(&mut self) -> String {
        loop {
            if let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=i).collect();
                return String::from_utf8_lossy(&line).into_owned();
            }
            if self.fill() == 0 {
                let rest = std::mem::take(&mut self.buf);
                return String::from_utf8_lossy(&rest).into_owned();
            }
        }
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        while self.buf.len() < n {
            assert!(self.fill() > 0, "EOF while reading literal");
        }
        self.buf.drain(..n).collect()
    }

    /// One IMAP response line with any literals it announces folded in.
    pub fn imap_line(&mut self) -> String {
        let mut out = String::new();
        loop {
            let l = self.line();
            if l.is_empty() {
                return out;
            }
            out.push_str(&l);
            let t = l.trim_end();
            if let Some(open) = t.rfind('{')
                && t.ends_with('}')
                && let Ok(n) = t[open + 1..t.len() - 1].parse::<usize>()
            {
                let data = self.bytes(n);
                out.push_str(&String::from_utf8_lossy(&data));
                continue;
            }
            return out;
        }
    }

    pub fn next_tag(&mut self) -> String {
        self.tag += 1;
        format!("a{}", self.tag)
    }

    /// Reads IMAP responses until the tagged one; returns (all text, tagged line).
    pub fn until_tagged(&mut self, tag: &str) -> (String, String) {
        let mut all = String::new();
        loop {
            let l = self.imap_line();
            assert!(!l.is_empty(), "EOF before tagged response; got: {all}");
            all.push_str(&l);
            if l.starts_with(&format!("{tag} ")) {
                return (all, l);
            }
        }
    }

    /// Sends `cmd` with a fresh tag; returns everything up to and including the tagged response.
    pub fn imap(&mut self, cmd: &str) -> String {
        let tag = self.next_tag();
        self.send_str(&format!("{tag} {cmd}\r\n"));
        self.until_tagged(&tag).0
    }

    /// Like [`Self::imap`] but asserts a tagged OK.
    pub fn ok(&mut self, cmd: &str) -> String {
        let tag = self.next_tag();
        self.send_str(&format!("{tag} {cmd}\r\n"));
        let (all, tagged) = self.until_tagged(&tag);
        assert!(tagged.starts_with(&format!("{tag} OK")), "`{cmd}` failed:\n{all}");
        all
    }

    pub fn imap_login(port: u16, user: &str, pw: &str) -> Client {
        let mut c = Client::connect(port);
        assert!(c.line().starts_with("* OK"));
        c.ok(&format!("LOGIN {user} {pw}"));
        c
    }

    /// Reads one SMTP reply (all continuation lines); returns (code, full text).
    pub fn smtp_reply(&mut self) -> (u16, String) {
        let mut all = String::new();
        loop {
            let l = self.line();
            assert!(l.len() >= 4, "short SMTP reply {l:?} after {all:?}");
            all.push_str(&l);
            if l.as_bytes()[3] == b' ' || l.trim_end().len() == 3 {
                return (l[..3].parse().unwrap(), all);
            }
        }
    }

    pub fn smtp(&mut self, cmd: &str) -> (u16, String) {
        self.send_str(&format!("{cmd}\r\n"));
        self.smtp_reply()
    }

    pub fn smtp_expect(&mut self, cmd: &str, code: u16) -> String {
        let (c, text) = self.smtp(cmd);
        assert_eq!(c, code, "`{cmd}` -> {text}");
        text
    }
}

pub fn b64(s: &[u8]) -> String {
    use std::fmt::Write as _;
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in s.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.write_char(T[((n >> (18 - 6 * i)) & 63) as usize] as char).unwrap();
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn sample(subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: Carol <carol@example.org>\r\nTo: {ALICE}\r\nSubject: {subject}\r\nDate: Sat, 10 Oct 2026 09:30:00 +0000\r\n\
         Message-ID: <{}@example.org>\r\n\r\n{body}",
        subject.replace(' ', "-")
    )
    .into_bytes()
}
