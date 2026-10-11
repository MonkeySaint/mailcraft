//! POP3 client (RFC 1939) with CAPA (RFC 2449), STLS (RFC 2595), SASL AUTH (RFC 5034: PLAIN, XOAUTH2,
//! OAUTHBEARER), USER/PASS, UIDL, LIST, TOP, RETR, DELE, RSET, NOOP and QUIT.
//!
//! Blocking, over the crate's transport. Multi-line responses are dot-unstuffed and capped in size; every
//! server reply is treated as hostile input. Credentials never cross an unencrypted connection unless
//! [`ConnectOptions::allow_plaintext_auth`] says so.

use mailcraft_model::{Security, ServerSettings};

use crate::error::{NetError, Result};
use crate::sasl::{self, Credentials, Mechanism};
use crate::transport::{Conn, ConnectOptions};

/// Longest status line accepted.
const MAX_LINE: usize = 64 * 1024;
/// Most lines accepted in a CAPA, LIST or UIDL answer.
const MAX_LIST_LINES: usize = 1_000_000;
/// Largest message accepted from RETR/TOP.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Most SASL challenges answered before giving up.
const MAX_SASL_CHALLENGES: usize = 4;
/// Longest unique-id (RFC 1939 §7: 1 to 70 characters in 0x21..0x7E).
const MAX_UIDL_CHARS: usize = 70;

/// Everything needed to reach and log in to a POP3 mailbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pop3Settings {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub credentials: Credentials,
    /// SASL mechanism, "USER" for USER/PASS, or empty for automatic.
    pub auth_mechanism: String,
    pub options: ConnectOptions,
}

impl Pop3Settings {
    pub fn new(host: impl Into<String>, port: u16, security: Security, credentials: Credentials) -> Self {
        Pop3Settings {
            host: host.into(),
            port,
            security,
            credentials,
            auth_mechanism: String::new(),
            options: ConnectOptions::default(),
        }
    }

    /// From the model's server settings plus a secret.
    pub fn from_server(s: &ServerSettings, credentials: Credentials) -> Self {
        Pop3Settings {
            auth_mechanism: s.auth.clone(),
            ..Pop3Settings::new(s.host.clone(), s.port, s.security, credentials)
        }
    }
}

/// Default POP3 port for a security mode (995 for implicit TLS, else 110).
pub fn default_port(security: Security) -> u16 {
    if security == Security::Tls { 995 } else { 110 }
}

/// A logged-in POP3 session (TRANSACTION state).
pub struct Pop3Client {
    conn: Conn,
    caps: Vec<String>,
    /// The server closed the session or a reply could not be parsed: stop using it.
    poisoned: bool,
}

impl std::fmt::Debug for Pop3Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pop3Client").field("host", &self.conn.host()).field("caps", &self.caps).finish()
    }
}

/// One `+OK`/`-ERR` reply.
fn status(line: &[u8]) -> Result<(bool, String)> {
    let text = String::from_utf8_lossy(line).into_owned();
    if let Some(rest) = text.strip_prefix("+OK") {
        return Ok((true, rest.trim().to_owned()));
    }
    if let Some(rest) = text.strip_prefix("-ERR") {
        return Ok((false, rest.trim().to_owned()));
    }
    Err(NetError::Protocol(format!("unexpected POP3 reply: {}", text.chars().take(200).collect::<String>())))
}

/// Strips an RFC 2449 response code (`[AUTH] …`) for messages, returning (code, text).
fn split_code(text: &str) -> (Option<String>, String) {
    if let Some(rest) = text.strip_prefix('[')
        && let Some((code, msg)) = rest.split_once(']')
    {
        return (Some(code.to_ascii_uppercase()), msg.trim().to_owned());
    }
    (None, text.to_owned())
}

fn valid_uidl(u: &str) -> bool {
    !u.is_empty() && u.chars().count() <= MAX_UIDL_CHARS && u.chars().all(|c| ('\x21'..='\x7e').contains(&c))
}

impl Pop3Client {
    /// Connects, reads the greeting, upgrades with STLS when asked, and logs in.
    pub fn connect(s: &Pop3Settings) -> Result<Pop3Client> {
        let implicit = s.security == Security::Tls;
        let conn = Conn::connect(&s.host, s.port, implicit, &s.options)?;
        let mut c = Pop3Client { conn, caps: Vec::new(), poisoned: false };
        let greeting = c.conn.read_line(MAX_LINE)?;
        let (ok, text) = status(&greeting)?;
        if !ok {
            return Err(NetError::Refused(format!("POP3 greeting: {text}")));
        }
        c.capa();
        if s.security == Security::StartTls {
            c.stls()?;
        }
        c.login(&s.credentials, &s.auth_mechanism)?;
        // Capabilities may change after login (RFC 2449 §5).
        c.capa();
        Ok(c)
    }

    /// The capabilities last read with CAPA (upper case keywords with their arguments).
    pub fn capabilities(&self) -> &[String] {
        &self.caps
    }

    /// Whether CAPA listed `cap` (first word, any case).
    pub fn has_capability(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c.split_whitespace().next().is_some_and(|w| w.eq_ignore_ascii_case(cap)))
    }

    fn sasl_mechanisms(&self) -> Vec<String> {
        self.caps
            .iter()
            .filter_map(|c| {
                let mut w = c.split_whitespace();
                w.next().filter(|k| k.eq_ignore_ascii_case("SASL"))?;
                Some(w.map(str::to_ascii_uppercase).collect::<Vec<_>>())
            })
            .flatten()
            .collect()
    }

    /// Whether the session can still be used.
    pub fn is_usable(&self) -> bool {
        !self.poisoned
    }

    pub fn is_tls(&self) -> bool {
        self.conn.is_tls()
    }

    fn send(&mut self, line: &str) -> Result<()> {
        if line.contains(['\r', '\n', '\0']) {
            return Err(NetError::Invalid("POP3 command with a line break".into()));
        }
        self.conn.write_all(format!("{line}\r\n").as_bytes()).inspect_err(|_| self.poisoned = true)
    }

    fn reply(&mut self) -> Result<(bool, String)> {
        let line = self.conn.read_line(MAX_LINE).inspect_err(|_| self.poisoned = true)?;
        status(&line).inspect_err(|_| self.poisoned = true)
    }

    /// Sends a command and expects `+OK`; `-ERR` is [`NetError::Refused`].
    fn simple(&mut self, line: &str) -> Result<String> {
        self.send(line)?;
        match self.reply()? {
            (true, t) => Ok(t),
            (false, t) => Err(NetError::Refused(t)),
        }
    }

    /// Reads a multi-line body after `+OK`: dot-unstuffed lines joined with CRLF, at most `max` bytes.
    fn multiline(&mut self, max: usize, max_lines: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut lines = 0usize;
        loop {
            let line = self
                .conn
                .read_line(MAX_MESSAGE_BYTES.min(max.saturating_add(2)))
                .inspect_err(|_| self.poisoned = true)?;
            if line == b"." {
                return Ok(out);
            }
            let body = line.strip_prefix(b".").unwrap_or(&line);
            out.extend_from_slice(body);
            out.extend_from_slice(b"\r\n");
            lines = lines.saturating_add(1);
            if out.len() > max || lines > max_lines {
                self.poisoned = true;
                return Err(NetError::TooLarge(format!("POP3 answer larger than {max} bytes")));
            }
        }
    }

    /// CAPA (RFC 2449). A server without CAPA simply has no listed capabilities.
    fn capa(&mut self) {
        let res = self.send("CAPA").and_then(|()| self.reply());
        self.caps = match res {
            Ok((true, _)) => match self.multiline(1024 * 1024, 10_000) {
                Ok(body) => String::from_utf8_lossy(&body)
                    .lines()
                    .map(|l| l.trim().to_owned())
                    .filter(|l| !l.is_empty())
                    .collect(),
                Err(_) => Vec::new(),
            },
            _ => Vec::new(),
        };
    }

    /// STLS (RFC 2595 §4); capabilities are read again over TLS.
    pub fn stls(&mut self) -> Result<()> {
        if self.conn.is_tls() {
            return Ok(());
        }
        if !self.caps.is_empty() && !self.has_capability("STLS") {
            return Err(NetError::Unsupported("STLS".into()));
        }
        self.simple("STLS")?;
        self.conn.start_tls().inspect_err(|_| self.poisoned = true)?;
        // Nothing learned before the handshake survives it.
        self.caps.clear();
        self.capa();
        Ok(())
    }

    fn require_secure_auth(&self) -> Result<()> {
        if self.conn.is_tls() || self.conn.options().allow_plaintext_auth {
            Ok(())
        } else {
            Err(NetError::Insecure(format!("POP3 login to {} without TLS", self.conn.host())))
        }
    }

    fn auth_error(text: String) -> NetError {
        let (code, msg) = split_code(&text);
        match code.as_deref() {
            Some("SYS/TEMP") | Some("IN-USE") => NetError::Refused(msg),
            _ => NetError::Auth(if msg.is_empty() { "the server rejected the login".into() } else { msg }),
        }
    }

    /// Logs in: SASL when the credentials need it or the server lists a fitting mechanism, else USER/PASS.
    pub fn login(&mut self, creds: &Credentials, preferred: &str) -> Result<()> {
        self.require_secure_auth()?;
        let offered = self.sasl_mechanisms();
        let user_pass = preferred.eq_ignore_ascii_case("USER") || preferred.eq_ignore_ascii_case("LOGIN");
        let mech = if user_pass { None } else { sasl::choose(creds, &offered, preferred) };
        match (creds, mech) {
            (Credentials::Password { user, password }, None | Some(Mechanism::Login)) => {
                self.send(&format!("USER {user}"))?;
                if let (false, t) = self.reply()? {
                    return Err(Self::auth_error(t));
                }
                self.send(&format!("PASS {password}"))?;
                match self.reply()? {
                    (true, _) => Ok(()),
                    (false, t) => Err(Self::auth_error(t)),
                }
            }
            (Credentials::OAuth2 { .. }, None) => Err(NetError::Unsupported(
                "the server offers no OAuth 2.0 login (XOAUTH2/OAUTHBEARER)".into(),
            )),
            (_, Some(m)) => {
                let host = self.conn.host().to_owned();
                let initial = sasl::initial_response(m, creds, &host, 0).unwrap_or_default();
                self.authenticate(m, &initial)
            }
        }
    }

    /// AUTH (RFC 5034) with an initial response.
    fn authenticate(&mut self, mech: Mechanism, initial: &[u8]) -> Result<()> {
        let encoded = if initial.is_empty() { "=".to_owned() } else { sasl::b64(initial) };
        self.send(&format!("AUTH {} {encoded}", mech.name()))?;
        let mut challenges = 0usize;
        loop {
            let line = self.conn.read_line(MAX_LINE).inspect_err(|_| self.poisoned = true)?;
            if line.first() == Some(&b'+') && line.get(1) == Some(&b' ') || line == b"+" {
                challenges += 1;
                if challenges > MAX_SASL_CHALLENGES {
                    self.poisoned = true;
                    return Err(NetError::Protocol("the server keeps sending SASL challenges".into()));
                }
                let reply = sasl::error_challenge_reply(mech);
                self.send(reply)?;
                continue;
            }
            return match status(&line).inspect_err(|_| self.poisoned = true)? {
                (true, _) => Ok(()),
                (false, t) => Err(Self::auth_error(t)),
            };
        }
    }

    /// STAT: (message count, total octets).
    pub fn stat(&mut self) -> Result<(u32, u64)> {
        let t = self.simple("STAT")?;
        let mut w = t.split_whitespace();
        let n = w.next().and_then(|x| x.parse().ok());
        let size = w.next().and_then(|x| x.parse().ok());
        match (n, size) {
            (Some(n), Some(s)) => Ok((n, s)),
            _ => Err(NetError::Protocol(format!("bad STAT reply: {t}"))),
        }
    }

    /// LIST: (message number, octets) of every message not marked deleted.
    pub fn list(&mut self) -> Result<Vec<(u32, u64)>> {
        self.simple("LIST")?;
        let body = self.multiline(64 * 1024 * 1024, MAX_LIST_LINES)?;
        Ok(String::from_utf8_lossy(&body)
            .lines()
            .filter_map(|l| {
                let mut w = l.split_whitespace();
                Some((w.next()?.parse().ok()?, w.next()?.parse().ok()?))
            })
            .collect())
    }

    /// UIDL (RFC 1939 §7): (message number, unique id). Malformed ids are skipped.
    pub fn uidl(&mut self) -> Result<Vec<(u32, String)>> {
        self.simple("UIDL")?;
        let body = self.multiline(128 * 1024 * 1024, MAX_LIST_LINES)?;
        let mut out: Vec<(u32, String)> = String::from_utf8_lossy(&body)
            .lines()
            .filter_map(|l| {
                let mut w = l.split_whitespace();
                let n: u32 = w.next()?.parse().ok()?;
                let id = w.next()?.to_owned();
                (n > 0 && valid_uidl(&id)).then_some((n, id))
            })
            .collect();
        out.sort_by_key(|(n, _)| *n);
        out.dedup_by(|a, b| a.0 == b.0 || a.1 == b.1);
        Ok(out)
    }

    /// RETR: the whole message (dot-unstuffed, CRLF line endings).
    pub fn retr(&mut self, n: u32) -> Result<Vec<u8>> {
        self.simple(&format!("RETR {n}"))?;
        self.multiline(MAX_MESSAGE_BYTES, usize::MAX)
    }

    /// TOP: the header plus the first `lines` body lines (`0` = header only).
    pub fn top(&mut self, n: u32, lines: u32) -> Result<Vec<u8>> {
        self.simple(&format!("TOP {n} {lines}"))?;
        self.multiline(MAX_MESSAGE_BYTES, usize::MAX)
    }

    /// DELE: marks a message deleted (removed when QUIT succeeds).
    pub fn dele(&mut self, n: u32) -> Result<()> {
        self.simple(&format!("DELE {n}")).map(|_| ())
    }

    /// RSET: unmarks deleted messages.
    pub fn rset(&mut self) -> Result<()> {
        self.simple("RSET").map(|_| ())
    }

    pub fn noop(&mut self) -> Result<()> {
        self.simple("NOOP").map(|_| ())
    }

    /// QUIT: commits deletions (UPDATE state) and closes the connection.
    pub fn quit(mut self) -> Result<()> {
        let res = self.simple("QUIT").map(|_| ());
        self.conn.shutdown();
        res
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// A scripted POP3 server: answers each received line with the next scripted reply.
    fn scripted(
        greeting: &'static str,
        replies: Vec<(&'static str, &'static str)>,
    ) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut w = s.try_clone().unwrap();
            let mut r = BufReader::new(s);
            w.write_all(greeting.as_bytes()).unwrap();
            let mut seen = Vec::new();
            for (expect, reply) in replies {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let line = line.trim_end().to_owned();
                assert!(line.starts_with(expect), "expected {expect}, got {line}");
                seen.push(line);
                w.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        (port, h)
    }

    fn plain_settings(port: u16) -> Pop3Settings {
        let mut s = Pop3Settings::new("127.0.0.1", port, Security::None, Credentials::password("u", "p"));
        s.options.allow_plaintext_auth = true;
        s
    }

    #[test]
    fn user_pass_uidl_retr_dele_quit() {
        let (port, h) = scripted(
            "+OK ready\r\n",
            vec![
                ("CAPA", "+OK\r\nUIDL\r\nTOP\r\nUSER\r\n.\r\n"),
                ("USER u", "+OK\r\n"),
                ("PASS p", "+OK logged in\r\n"),
                ("CAPA", "+OK\r\nUIDL\r\nTOP\r\n.\r\n"),
                ("UIDL", "+OK\r\n1 abc\r\n2 d\x01ef\r\n3 xyz\r\n.\r\n"),
                ("STAT", "+OK 2 300\r\n"),
                ("RETR 1", "+OK\r\nSubject: hi\r\n\r\n..dot\r\nend\r\n.\r\n"),
                ("TOP 3 0", "+OK\r\nSubject: three\r\n\r\n.\r\n"),
                ("DELE 1", "+OK\r\n"),
                ("QUIT", "+OK bye\r\n"),
            ],
        );
        let mut c = Pop3Client::connect(&plain_settings(port)).unwrap();
        assert!(c.has_capability("uidl"));
        assert_eq!(c.uidl().unwrap(), vec![(1, "abc".to_owned()), (3, "xyz".to_owned())]);
        assert_eq!(c.stat().unwrap(), (2, 300));
        assert_eq!(c.retr(1).unwrap(), b"Subject: hi\r\n\r\n.dot\r\nend\r\n".to_vec());
        assert_eq!(c.top(3, 0).unwrap(), b"Subject: three\r\n\r\n".to_vec());
        c.dele(1).unwrap();
        c.quit().unwrap();
        assert_eq!(h.join().unwrap().len(), 10);
    }

    #[test]
    fn refuses_plaintext_login_and_reports_bad_password() {
        let (port, _h) = scripted("+OK ready\r\n", vec![("CAPA", "-ERR no\r\n")]);
        let s = Pop3Settings::new("127.0.0.1", port, Security::None, Credentials::password("u", "p"));
        assert!(matches!(Pop3Client::connect(&s), Err(NetError::Insecure(_))));

        let (port, _h) = scripted(
            "+OK ready\r\n",
            vec![("CAPA", "-ERR\r\n"), ("USER", "+OK\r\n"), ("PASS", "-ERR [AUTH] wrong password\r\n")],
        );
        match Pop3Client::connect(&plain_settings(port)) {
            Err(NetError::Auth(m)) => assert_eq!(m, "wrong password"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sasl_plain_and_hostile_replies() {
        let (port, _h) = scripted(
            "+OK ready\r\n",
            vec![
                ("CAPA", "+OK\r\nSASL PLAIN XOAUTH2\r\n.\r\n"),
                ("AUTH PLAIN AHUAcA==", "+OK\r\n"),
                ("CAPA", "+OK\r\n.\r\n"),
                ("LIST", "+OK\r\n1 10\r\njunk\r\n2 x\r\n.\r\n"),
                ("STAT", "+OK nope\r\n"),
            ],
        );
        let mut c = Pop3Client::connect(&plain_settings(port)).unwrap();
        assert_eq!(c.list().unwrap(), vec![(1, 10)]);
        assert!(matches!(c.stat(), Err(NetError::Protocol(_))));
        // Bad greeting.
        let (port, _h) = scripted("* OK imap\r\n", vec![]);
        assert!(matches!(Pop3Client::connect(&plain_settings(port)), Err(NetError::Protocol(_))));
        assert!(!valid_uidl("") && !valid_uidl("a b") && valid_uidl("A1.b"));
        assert_eq!(split_code("[IN-USE] locked"), (Some("IN-USE".into()), "locked".into()));
        assert_eq!(default_port(Security::Tls), 995);
    }
}
