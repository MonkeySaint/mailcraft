//! SMTP submission server (RFC 5321, RFC 6409): EHLO/HELO, STARTTLS (RFC 3207), AUTH PLAIN/LOGIN (RFC 4954),
//! SIZE (RFC 1870), 8BITMIME, PIPELINING, ENHANCEDSTATUSCODES, MAIL/RCPT/DATA with dot-unstuffing, RSET, NOOP,
//! VRFY, QUIT. Accepted mail goes to local users' INBOX and to the submission log.

use std::io;
use std::sync::Arc;

use base64::Engine;

use crate::conn::Conn;
use crate::date::DateTime;
use crate::server::Ctx;
use crate::store::{Submission, append_msg};

const MAX_LINE: usize = 4096;
const MAX_DATA_LINE: usize = 1024 * 1024;
const MAX_RECIPIENTS: usize = 100;

struct Session<'c> {
    conn: Conn,
    ctx: &'c Ctx,
    helo: Option<String>,
    auth: Option<String>,
    from: Option<String>,
    rcpts: Vec<String>,
    queued: u64,
}

pub fn serve(conn: Conn, ctx: &Ctx) {
    let mut s = Session { conn, ctx, helo: None, auth: None, from: None, rcpts: Vec::new(), queued: 0 };
    let _ = s.run();
    s.conn.close();
}

/// Extracts the address from `<addr>` (with optional source route), returning it and the parameter text.
fn parse_path(arg: &str) -> Option<(String, &str)> {
    let arg = arg.trim_start();
    let rest = arg.strip_prefix('<')?;
    let close = rest.find('>')?;
    let mut addr = rest.get(..close)?.trim();
    if addr.starts_with('@')
        && let Some((_, a)) = addr.split_once(':')
    {
        addr = a;
    }
    if addr.len() > 320 || addr.chars().any(|c| c.is_control() || c == ' ') {
        return None;
    }
    Some((addr.to_owned(), rest.get(close + 1..).unwrap_or("").trim()))
}

impl Session<'_> {
    fn reply(&mut self, text: &str) -> io::Result<()> {
        self.conn.write_all(format!("{text}\r\n").as_bytes())
    }

    fn auth_allowed(&self) -> bool {
        !self.ctx.config.auth_requires_tls || self.conn.is_tls()
    }

    fn reset(&mut self) {
        self.from = None;
        self.rcpts.clear();
    }

    fn run(&mut self) -> io::Result<()> {
        let host = self.ctx.config.hostname.clone();
        self.reply(&format!("220 {host} ESMTP MailCraft test server ready"))?;
        loop {
            let Some(line) = (match self.conn.read_line(MAX_LINE) {
                Ok(l) => l,
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    self.reply("500 5.5.2 Line too long")?;
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
            match verb.as_str() {
                "EHLO" | "HELO" => self.ehlo(&verb, &arg)?,
                "STARTTLS" => {
                    if !arg.trim().is_empty() {
                        self.reply("501 5.5.4 Syntax error (no parameters allowed)")?;
                    } else if self.conn.is_tls() {
                        self.reply("503 5.5.1 TLS already active")?;
                    } else if self.conn.has_buffered() {
                        // Pipelined commands after STARTTLS would be a plaintext injection.
                        self.reply("501 5.5.4 No pipelining with STARTTLS")?;
                    } else {
                        self.reply("220 2.0.0 Ready to start TLS")?;
                        if self.conn.start_tls().is_err() {
                            return Ok(());
                        }
                        self.helo = None;
                        self.auth = None;
                        self.reset();
                    }
                }
                "AUTH" => self.auth_cmd(&arg)?,
                "MAIL" => self.mail(&arg)?,
                "RCPT" => self.rcpt(&arg)?,
                "DATA" => {
                    if !self.data()? {
                        return Ok(());
                    }
                }
                "BDAT" => self.reply("502 5.5.1 CHUNKING not supported")?,
                "RSET" => {
                    self.reset();
                    self.reply("250 2.0.0 OK")?;
                }
                "NOOP" => self.reply("250 2.0.0 OK")?,
                "VRFY" => self.reply("252 2.1.5 Cannot verify, will attempt delivery")?,
                "HELP" => self.reply("214 2.0.0 See RFC 5321")?,
                "QUIT" => {
                    self.reply("221 2.0.0 Bye")?;
                    return Ok(());
                }
                "" => self.reply("500 5.5.2 Empty command")?,
                _ => self.reply("502 5.5.1 Command not implemented")?,
            }
        }
    }

    fn ehlo(&mut self, verb: &str, arg: &str) -> io::Result<()> {
        let client = arg.trim();
        if client.is_empty() {
            return self.reply("501 5.5.4 Domain required");
        }
        // The name is copied into the Received trace field: no controls (a bare CR) or spaces.
        if client.len() > 255 || client.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return self.reply("501 5.5.4 Invalid domain name");
        }
        self.helo = Some(client.to_owned());
        self.reset();
        let host = self.ctx.config.hostname.clone();
        if verb == "HELO" {
            return self.reply(&format!("250 {host}"));
        }
        let mut lines = vec![
            format!("{host} greets {client}"),
            "PIPELINING".to_owned(),
            format!("SIZE {}", self.ctx.config.max_message_size),
            "8BITMIME".to_owned(),
            "ENHANCEDSTATUSCODES".to_owned(),
            "SMTPUTF8".to_owned(),
        ];
        if !self.conn.is_tls() {
            lines.push("STARTTLS".to_owned());
        }
        if self.auth_allowed() {
            lines.push("AUTH PLAIN LOGIN XOAUTH2 OAUTHBEARER".to_owned());
        }
        let n = lines.len();
        let mut out = String::new();
        for (i, l) in lines.iter().enumerate() {
            out.push_str(&format!("250{}{l}\r\n", if i + 1 == n { ' ' } else { '-' }));
        }
        self.conn.write_all(out.as_bytes())
    }

    /// Reads one SASL response line; `None` if the client cancelled with `*`.
    fn sasl_line(&mut self, challenge: &str) -> io::Result<Option<Vec<u8>>> {
        self.reply(&format!("334 {challenge}"))?;
        let Some(line) = self.conn.read_line(MAX_LINE)? else {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        };
        let line = crate::mime::trim(&line).to_vec();
        if line == b"*" {
            return Ok(None);
        }
        Ok(Some(line))
    }

    fn auth_cmd(&mut self, arg: &str) -> io::Result<()> {
        if self.helo.is_none() {
            return self.reply("503 5.5.1 EHLO first");
        }
        if self.auth.is_some() {
            return self.reply("503 5.5.1 Already authenticated");
        }
        if self.from.is_some() {
            return self.reply("503 5.5.1 AUTH not allowed during a mail transaction");
        }
        if !self.auth_allowed() {
            return self.reply("538 5.7.11 Encryption required for requested authentication mechanism");
        }
        let mut parts = arg.split_whitespace();
        let mech = parts.next().unwrap_or("").to_ascii_uppercase();
        let initial = parts.next().map(|s| s.as_bytes().to_vec());
        let b64 = base64::engine::general_purpose::STANDARD;
        let decode =
            |v: &[u8]| -> Option<Vec<u8>> { if v == b"=" { Some(Vec::new()) } else { b64.decode(v).ok() } };
        let (user, pass) = match mech.as_str() {
            "PLAIN" => {
                let resp = match initial {
                    Some(r) => r,
                    None => match self.sasl_line("")? {
                        Some(r) => r,
                        None => return self.reply("501 5.7.0 Authentication cancelled"),
                    },
                };
                let Some(dec) = decode(&resp) else {
                    return self.reply("501 5.5.2 Invalid base64");
                };
                let mut it = dec.split(|&b| b == 0);
                match (it.next(), it.next(), it.next(), it.next()) {
                    (Some(authz), Some(u), Some(p), None)
                        if authz.is_empty() || authz.eq_ignore_ascii_case(u) =>
                    {
                        (String::from_utf8_lossy(u).into_owned(), String::from_utf8_lossy(p).into_owned())
                    }
                    _ => return self.reply("535 5.7.8 Authentication credentials invalid"),
                }
            }
            "LOGIN" => {
                let u = match initial {
                    Some(r) => r,
                    None => match self.sasl_line("VXNlcm5hbWU6")? {
                        Some(r) => r,
                        None => return self.reply("501 5.7.0 Authentication cancelled"),
                    },
                };
                let p = match self.sasl_line("UGFzc3dvcmQ6")? {
                    Some(r) => r,
                    None => return self.reply("501 5.7.0 Authentication cancelled"),
                };
                let (Some(u), Some(p)) = (decode(&u), decode(&p)) else {
                    return self.reply("501 5.5.2 Invalid base64");
                };
                (String::from_utf8_lossy(&u).into_owned(), String::from_utf8_lossy(&p).into_owned())
            }
            "XOAUTH2" | "OAUTHBEARER" => {
                let resp = match initial {
                    Some(r) => r,
                    None => match self.sasl_line("")? {
                        Some(r) => r,
                        None => return self.reply("501 5.7.0 Authentication cancelled"),
                    },
                };
                let found =
                    decode(&resp).and_then(|d| crate::oauth::sasl_bearer(&mech, &d)).and_then(|(u, t)| {
                        let owner = self.ctx.shared.lock().oauth.user_of(&t)?;
                        owner.eq_ignore_ascii_case(&u).then_some(owner)
                    });
                if let Some(addr) = found {
                    self.auth = Some(addr);
                    return self.reply("235 2.7.0 Authentication successful");
                }
                // RFC 7628 section 3.2.2: an error challenge, the client's dummy answer, then the failure.
                let err = b64.encode(br#"{"status":"invalid_token","schemes":"bearer","scope":"mail"}"#);
                let _ = self.sasl_line(&err)?;
                return self.reply("535 5.7.8 Authentication credentials invalid");
            }
            _ => return self.reply("504 5.5.4 Unrecognized authentication type"),
        };
        let found = self.ctx.shared.lock().check_password(&user, &pass);
        match found {
            Some(addr) => {
                self.auth = Some(addr);
                self.reply("235 2.7.0 Authentication successful")
            }
            None => self.reply("535 5.7.8 Authentication credentials invalid"),
        }
    }

    fn mail(&mut self, arg: &str) -> io::Result<()> {
        if self.helo.is_none() {
            return self.reply("503 5.5.1 EHLO first");
        }
        if self.from.is_some() {
            return self.reply("503 5.5.1 Nested MAIL command");
        }
        if self.ctx.config.smtp_require_auth && self.auth.is_none() {
            return self.reply("530 5.7.0 Authentication required");
        }
        let Some(rest) = strip_keyword(arg, "FROM:") else {
            return self.reply("501 5.5.4 Syntax: MAIL FROM:<address>");
        };
        let Some((addr, params)) = parse_path(rest) else {
            return self.reply("501 5.1.7 Bad sender address syntax");
        };
        for p in params.split_whitespace() {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            match k.to_ascii_uppercase().as_str() {
                "SIZE" => match v.parse::<u64>() {
                    Ok(n) if n > self.ctx.config.max_message_size as u64 => {
                        return self.reply("552 5.3.4 Message size exceeds fixed limit");
                    }
                    Ok(_) => {}
                    Err(_) => return self.reply("501 5.5.4 Bad SIZE parameter"),
                },
                "BODY" | "AUTH" | "SMTPUTF8" | "RET" | "ENVID" => {}
                _ => return self.reply("555 5.5.4 Unsupported MAIL parameter"),
            }
        }
        self.from = Some(addr);
        self.reply("250 2.1.0 Sender OK")
    }

    fn rcpt(&mut self, arg: &str) -> io::Result<()> {
        if self.from.is_none() {
            return self.reply("503 5.5.1 MAIL first");
        }
        let Some(rest) = strip_keyword(arg, "TO:") else {
            return self.reply("501 5.5.4 Syntax: RCPT TO:<address>");
        };
        let Some((addr, _params)) = parse_path(rest) else {
            return self.reply("501 5.1.3 Bad recipient address syntax");
        };
        if addr.is_empty() || !addr.contains('@') {
            return self.reply("501 5.1.3 Bad recipient address syntax");
        }
        if self.ctx.config.reject_recipients.iter().any(|r| r.trim().eq_ignore_ascii_case(&addr)) {
            return self.reply(&format!("550 5.1.1 <{addr}>: Recipient address rejected"));
        }
        if self.rcpts.len() >= MAX_RECIPIENTS {
            return self.reply("452 4.5.3 Too many recipients");
        }
        self.rcpts.push(addr);
        self.reply("250 2.1.5 Recipient OK")
    }

    /// Returns false when the connection should close.
    fn data(&mut self) -> io::Result<bool> {
        if self.from.is_none() {
            self.reply("503 5.5.1 MAIL first")?;
            return Ok(true);
        }
        if self.rcpts.is_empty() {
            self.reply("554 5.5.1 No valid recipients")?;
            return Ok(true);
        }
        self.reply("354 End data with <CR><LF>.<CR><LF>")?;
        let limit = self.ctx.config.max_message_size;
        let mut data = Vec::new();
        let mut too_big = false;
        let mut bare_line_ending = false;
        loop {
            let line = match self.conn.read_line(MAX_DATA_LINE) {
                Ok(Some(l)) => l,
                Ok(None) => return Ok(false),
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    self.reply("500 5.5.2 Line too long")?;
                    return Ok(false);
                }
                Err(e) => return Err(e),
            };
            // Only CRLF ends a line (RFC 5321 section 2.3.8). A bare LF or CR never ends the data (that would
            // allow SMTP smuggling); the message is refused once the real terminator arrives.
            let Some(content) = line.strip_suffix(b"\r\n") else {
                bare_line_ending = true;
                continue;
            };
            if content.contains(&b'\r') || content.contains(&b'\n') {
                bare_line_ending = true;
                continue;
            }
            if content == b"." {
                break;
            }
            let content = content.strip_prefix(b".").unwrap_or(content);
            if data.len().saturating_add(content.len()).saturating_add(2) > limit {
                too_big = true;
                data.clear();
            }
            if !too_big {
                data.extend_from_slice(content);
                data.extend_from_slice(b"\r\n");
            }
        }
        if bare_line_ending {
            self.reset();
            self.reply("554 5.6.0 Bare CR or LF in message data")?;
            return Ok(true);
        }
        if too_big {
            self.reset();
            self.reply("552 5.3.4 Message size exceeds fixed limit")?;
            return Ok(true);
        }
        let from = self.from.take().unwrap_or_default();
        let rcpts = std::mem::take(&mut self.rcpts);
        self.queued = self.queued.saturating_add(1);
        self.deliver(from, rcpts, data);
        self.reply(&format!("250 2.0.0 OK queued as MC{:06}", self.queued))?;
        Ok(true)
    }

    fn deliver(&mut self, from: String, rcpts: Vec<String>, data: Vec<u8>) {
        let now = DateTime::now();
        let stored: Arc<[u8]> = if self.ctx.config.add_trace_headers {
            let mut msg = format!(
                "Return-Path: <{from}>\r\nReceived: from {} by {} with {} id MC{:06}; {}\r\n",
                self.helo.as_deref().unwrap_or("unknown"),
                self.ctx.config.hostname,
                match (self.conn.is_tls(), self.auth.is_some()) {
                    (true, true) => "ESMTPSA",
                    (true, false) => "ESMTPS",
                    (false, true) => "ESMTPA",
                    (false, false) => "ESMTP",
                },
                self.queued,
                now.to_rfc5322()
            )
            .into_bytes();
            msg.extend_from_slice(&data);
            Arc::from(msg)
        } else {
            Arc::from(data.as_slice())
        };
        let mut delivered = Vec::new();
        {
            let mut st = self.ctx.shared.lock();
            for r in &rcpts {
                if delivered.iter().any(|d: &String| d.eq_ignore_ascii_case(r)) {
                    continue;
                }
                if let Some(mb) = st.user_mut(r).and_then(|u| u.mailbox_mut("INBOX")) {
                    append_msg(mb, Arc::clone(&stored), Vec::new(), now);
                    delivered.push(r.clone());
                }
            }
            st.submissions.push(Submission {
                auth_user: self.auth.clone(),
                mail_from: from,
                rcpt_to: rcpts,
                data,
                delivered_to: delivered,
            });
        }
        self.ctx.shared.notify();
    }
}

fn strip_keyword<'a>(arg: &'a str, kw: &str) -> Option<&'a str> {
    let head = arg.get(..kw.len())?;
    if head.eq_ignore_ascii_case(kw) { arg.get(kw.len()..) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(parse_path("<a@example.com> SIZE=10"), Some(("a@example.com".to_owned(), "SIZE=10")));
        assert_eq!(parse_path("<>"), Some((String::new(), "")));
        assert_eq!(
            parse_path("<@relay.example:a@example.com>").map(|p| p.0),
            Some("a@example.com".to_owned())
        );
        assert_eq!(parse_path("a@example.com"), None);
        assert_eq!(parse_path("<a b@example.com>"), None);
        assert_eq!(strip_keyword("from:<x>", "FROM:"), Some("<x>"));
        assert_eq!(strip_keyword("TO", "FROM:"), None);
    }
}
