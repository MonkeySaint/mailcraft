//! POP3 server (RFC 1939) over the shared store's INBOX: CAPA (RFC 2449), STLS (RFC 2595), USER/PASS,
//! AUTH PLAIN (RFC 5034, with an initial response or a continuation), STAT, LIST, UIDL, RETR, TOP, DELE,
//! NOOP, RSET, QUIT. The maildrop is the INBOX as it was at login; deletions happen at QUIT. Unique ids are
//! `<uidvalidity>.<uid>`, so they stay the same across sessions.

use std::io;

use base64::Engine;

use crate::conn::Conn;
use crate::server::Ctx;

const MAX_LINE: usize = 4096;

struct Msg {
    uid: u32,
    uidl: String,
    raw: std::sync::Arc<[u8]>,
    deleted: bool,
}

struct Session<'c> {
    conn: Conn,
    ctx: &'c Ctx,
    user_arg: Option<String>,
    user: Option<String>,
    msgs: Vec<Msg>,
}

pub fn serve(conn: Conn, ctx: &Ctx) {
    let mut s = Session { conn, ctx, user_arg: None, user: None, msgs: Vec::new() };
    let _ = s.run();
    s.conn.close();
}

/// Dot-stuffs `data` and terminates it with the end-of-answer line.
fn stuffed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    let text = data.strip_suffix(b"\r\n").or_else(|| data.strip_suffix(b"\n")).unwrap_or(data);
    if !text.is_empty() || !data.is_empty() {
        for line in text.split(|&b| b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.first() == Some(&b'.') {
                out.push(b'.');
            }
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
        }
    }
    out.extend_from_slice(b".\r\n");
    out
}

/// The header (through the blank line) and the first `n` body lines of `raw`.
fn top(raw: &[u8], n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut in_body = false;
    let mut body_lines = 0usize;
    for line in raw.split_inclusive(|&b| b == b'\n') {
        if in_body {
            if body_lines >= n {
                break;
            }
            body_lines += 1;
            out.extend_from_slice(line);
            continue;
        }
        out.extend_from_slice(line);
        if line == b"\r\n" || line == b"\n" {
            in_body = true;
        }
    }
    out
}

impl Session<'_> {
    fn reply(&mut self, text: &str) -> io::Result<()> {
        self.conn.write_all(format!("{text}\r\n").as_bytes())
    }

    fn auth_allowed(&self) -> bool {
        !self.ctx.config.auth_requires_tls || self.conn.is_tls()
    }

    fn login(&mut self, user: &str) {
        let key = user.trim().to_ascii_lowercase();
        let store = self.ctx.shared.lock();
        let msgs = store
            .user(&key)
            .and_then(|u| u.mailbox("INBOX"))
            .map(|mb| {
                mb.messages
                    .iter()
                    .map(|m| Msg {
                        uid: m.uid,
                        uidl: format!("{}.{}", mb.uidvalidity, m.uid),
                        raw: m.raw.clone(),
                        deleted: false,
                    })
                    .collect()
            })
            .unwrap_or_default();
        drop(store);
        self.msgs = msgs;
        self.user = Some(key);
    }

    fn msg(&self, arg: &str) -> Option<usize> {
        let n: usize = arg.trim().parse().ok()?;
        let i = n.checked_sub(1)?;
        self.msgs.get(i).filter(|m| !m.deleted).map(|_| i)
    }

    fn capa(&mut self) -> io::Result<()> {
        let mut lines = vec!["+OK Capability list follows", "TOP", "UIDL", "RESP-CODES", "PIPELINING"];
        if self.user.is_none() {
            if !self.conn.is_tls() {
                lines.push("STLS");
            }
            if self.auth_allowed() {
                lines.push("USER");
                lines.push("SASL PLAIN");
            }
        }
        lines.push(".");
        let text = lines.join("\r\n");
        self.reply(&text)
    }

    fn check(&mut self, user: &str, pass: &str) -> io::Result<()> {
        let ok = self.ctx.shared.lock().check_password(user, pass);
        match ok {
            Some(addr) => {
                self.login(&addr);
                let n = self.msgs.len();
                self.reply(&format!("+OK maildrop has {n} messages"))
            }
            None => self.reply("-ERR [AUTH] invalid user name or password"),
        }
    }

    fn auth(&mut self, arg: &str) -> io::Result<()> {
        let mut w = arg.split_whitespace();
        let mech = w.next().unwrap_or("").to_ascii_uppercase();
        if mech != "PLAIN" {
            return self.reply("-ERR unsupported mechanism");
        }
        let initial = match w.next() {
            Some(i) => i.to_owned(),
            None => {
                self.reply("+ ")?;
                let Some(line) = self.conn.read_line(MAX_LINE)? else { return Ok(()) };
                String::from_utf8_lossy(&line).trim().to_owned()
            }
        };
        if initial == "*" {
            return self.reply("-ERR authentication cancelled");
        }
        let decoded = if initial == "=" {
            Vec::new()
        } else {
            match base64::engine::general_purpose::STANDARD.decode(initial.as_bytes()) {
                Ok(d) => d,
                Err(_) => return self.reply("-ERR bad base64"),
            }
        };
        let parts: Vec<&[u8]> = decoded.split(|&b| b == 0).collect();
        let (Some(user), Some(pass)) = (parts.get(1), parts.get(2)) else {
            return self.reply("-ERR malformed PLAIN response");
        };
        let user = String::from_utf8_lossy(user).into_owned();
        let pass = String::from_utf8_lossy(pass).into_owned();
        self.check(&user, &pass)
    }

    /// UPDATE state: removes the messages marked deleted from the INBOX.
    fn update(&mut self) {
        let Some(user) = self.user.clone() else { return };
        let gone: Vec<u32> = self.msgs.iter().filter(|m| m.deleted).map(|m| m.uid).collect();
        if gone.is_empty() {
            return;
        }
        let mut store = self.ctx.shared.lock();
        if let Some(mb) = store.user_mut(&user).and_then(|u| u.mailbox_mut("INBOX")) {
            mb.messages.retain(|m| !gone.contains(&m.uid));
            let _ = mb.next_modseq();
        }
        drop(store);
        self.ctx.shared.notify();
    }

    fn run(&mut self) -> io::Result<()> {
        let host = self.ctx.config.hostname.clone();
        self.reply(&format!("+OK {host} MailCraft test POP3 server ready"))?;
        loop {
            let Some(line) = (match self.conn.read_line(MAX_LINE) {
                Ok(l) => l,
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    self.reply("-ERR line too long")?;
                    return Ok(());
                }
                Err(e) => return Err(e),
            }) else {
                return Ok(());
            };
            let line = String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_owned();
            let (verb, arg) = match line.split_once(' ') {
                Some((v, a)) => (v.to_ascii_uppercase(), a.to_owned()),
                None => (line.to_ascii_uppercase(), String::new()),
            };
            let authed = self.user.is_some();
            match (authed, verb.as_str()) {
                (_, "CAPA") => self.capa()?,
                (_, "QUIT") => {
                    self.update();
                    self.reply("+OK bye")?;
                    return Ok(());
                }
                (_, "NOOP") if authed => self.reply("+OK")?,
                (false, "STLS") => {
                    if self.conn.is_tls() {
                        self.reply("-ERR TLS already active")?;
                    } else if self.conn.has_buffered() {
                        self.reply("-ERR no pipelining with STLS")?;
                    } else {
                        self.reply("+OK begin TLS negotiation")?;
                        if self.conn.start_tls().is_err() {
                            return Ok(());
                        }
                        self.user_arg = None;
                    }
                }
                (false, "USER") => {
                    if !self.auth_allowed() {
                        self.reply("-ERR [AUTH] use STLS first")?;
                    } else {
                        self.user_arg = Some(arg.trim().to_owned());
                        self.reply("+OK send PASS")?;
                    }
                }
                (false, "PASS") => match self.user_arg.take() {
                    Some(u) if self.auth_allowed() => self.check(&u, &arg)?,
                    _ => self.reply("-ERR send USER first")?,
                },
                (false, "AUTH") => {
                    if self.auth_allowed() {
                        self.auth(&arg)?;
                    } else {
                        self.reply("-ERR [AUTH] use STLS first")?;
                    }
                }
                (true, "STAT") => {
                    let live: Vec<&Msg> = self.msgs.iter().filter(|m| !m.deleted).collect();
                    let size: usize = live.iter().map(|m| m.raw.len()).sum();
                    let n = live.len();
                    self.reply(&format!("+OK {n} {size}"))?;
                }
                (true, "LIST") | (true, "UIDL") => {
                    let uidl = verb == "UIDL";
                    let show = |i: usize, m: &Msg| {
                        if uidl {
                            format!("{} {}", i + 1, m.uidl)
                        } else {
                            format!("{} {}", i + 1, m.raw.len())
                        }
                    };
                    if arg.trim().is_empty() {
                        let mut out = String::from("+OK\r\n");
                        for (i, m) in self.msgs.iter().enumerate().filter(|(_, m)| !m.deleted) {
                            out.push_str(&show(i, m));
                            out.push_str("\r\n");
                        }
                        out.push('.');
                        self.reply(&out)?;
                    } else {
                        match self.msg(&arg).and_then(|i| self.msgs.get(i).map(|m| show(i, m))) {
                            Some(t) => self.reply(&format!("+OK {t}"))?,
                            None => self.reply("-ERR no such message")?,
                        }
                    }
                }
                (true, "RETR") | (true, "TOP") => {
                    let mut w = arg.split_whitespace();
                    let n = w.next().unwrap_or("");
                    let lines: Option<usize> = if verb == "TOP" {
                        match w.next().and_then(|x| x.parse().ok()) {
                            Some(l) => Some(l),
                            None => {
                                self.reply("-ERR TOP needs a line count")?;
                                continue;
                            }
                        }
                    } else {
                        None
                    };
                    match self.msg(n).and_then(|i| self.msgs.get(i)) {
                        Some(m) => {
                            let data = match lines {
                                Some(l) => top(&m.raw, l),
                                None => m.raw.to_vec(),
                            };
                            let mut out = format!("+OK {} octets\r\n", m.raw.len()).into_bytes();
                            out.extend_from_slice(&stuffed(&data));
                            self.conn.write_all(&out)?;
                        }
                        None => self.reply("-ERR no such message")?,
                    }
                }
                (true, "DELE") => match self.msg(&arg) {
                    Some(i) => {
                        if let Some(m) = self.msgs.get_mut(i) {
                            m.deleted = true;
                        }
                        self.reply("+OK marked deleted")?;
                    }
                    None => self.reply("-ERR no such message")?,
                },
                (true, "RSET") => {
                    for m in &mut self.msgs {
                        m.deleted = false;
                    }
                    self.reply("+OK")?;
                }
                (_, "") => self.reply("-ERR empty command")?,
                _ => self.reply("-ERR command not valid in this state")?,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuffing_and_top() {
        assert_eq!(stuffed(b"a\r\n.b\r\n"), b"a\r\n..b\r\n.\r\n".to_vec());
        assert_eq!(stuffed(b""), b".\r\n".to_vec());
        assert_eq!(stuffed(b"x"), b"x\r\n.\r\n".to_vec());
        let raw = b"Subject: s\r\nFrom: f\r\n\r\nl1\r\nl2\r\nl3\r\n";
        assert_eq!(top(raw, 0), b"Subject: s\r\nFrom: f\r\n\r\n".to_vec());
        assert_eq!(top(raw, 2), b"Subject: s\r\nFrom: f\r\n\r\nl1\r\nl2\r\n".to_vec());
        assert_eq!(top(b"no body", 3), b"no body".to_vec());
    }
}
