//! SMTP submission client (RFC 5321, RFC 6409): EHLO/HELO, STARTTLS (RFC 3207), AUTH PLAIN/LOGIN/XOAUTH2/
//! OAUTHBEARER (RFC 4954), SIZE (RFC 1870), 8BITMIME (RFC 6152), SMTPUTF8 (RFC 6531), PIPELINING (RFC 2920),
//! enhanced status codes (RFC 3463), delivery status notifications (DSN, RFC 3461), dot-stuffing, and
//! per-recipient error reporting.
//!
//! Submission-only header fields: the engine asks for envelope options by writing `X-MailCraft-Submit-*`
//! header fields into the message it queues (see [`SUBMIT_HEADER_PREFIX`]). [`SmtpClient::send`] reads them,
//! turns them into envelope parameters (`X-MailCraft-Submit-DSN` → `NOTIFY`/`ORCPT`/`RET`/`ENVID` when the
//! server advertises DSN) and removes every such field before DATA, so recipients never see them. Without
//! server DSN support the request is dropped: no legacy `Return-Receipt-To` fallback.

use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use mailcraft_model::Security;

use crate::error::{NetError, Result};
use crate::sasl::{self, Credentials, Mechanism};
use crate::transport::{Conn, ConnectOptions};

/// Longest reply line accepted (RFC 5321 allows 512; servers exceed it).
const MAX_REPLY_LINE: usize = 64 * 1024;
/// Most lines in one multi-line reply.
const MAX_REPLY_LINES: usize = 1000;
/// Most SASL challenges answered after the initial response before giving up.
const MAX_SASL_CHALLENGES: usize = 4;

/// Settings for a submission server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// `None` sends without logging in.
    pub credentials: Option<Credentials>,
    /// SASL mechanism name, or empty to choose automatically.
    pub auth_mechanism: String,
    /// EHLO name; empty uses the local address literal (`[192.0.2.1]`).
    pub helo_name: String,
    pub options: ConnectOptions,
    /// How long to wait for the reply after the final `.` (RFC 5321 §4.5.3.2 suggests 10 minutes).
    pub data_timeout: Duration,
}

impl SmtpSettings {
    pub fn new(host: impl Into<String>, port: u16, security: Security) -> Self {
        SmtpSettings {
            host: host.into(),
            port,
            security,
            credentials: None,
            auth_mechanism: String::new(),
            helo_name: String::new(),
            options: ConnectOptions::default(),
            data_timeout: Duration::from_secs(600),
        }
    }
}

/// A server reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    /// RFC 3463 enhanced status code (`5.1.1`) when the server sent one.
    pub enhanced: Option<String>,
    /// Text of each line, codes removed.
    pub lines: Vec<String>,
}

impl Reply {
    pub fn is_positive(&self) -> bool {
        (200..300).contains(&self.code)
    }
    pub fn is_intermediate(&self) -> bool {
        (300..400).contains(&self.code)
    }
    pub fn is_permanent(&self) -> bool {
        self.code >= 500
    }
    pub fn text(&self) -> String {
        self.lines.join(" ")
    }
    fn describe(&self) -> String {
        format!("{} {}", self.code, self.text())
    }
}

/// EHLO keywords.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extensions {
    /// SIZE limit; `Some(0)` means advertised without a fixed limit.
    pub size: Option<u64>,
    pub eight_bit_mime: bool,
    pub smtputf8: bool,
    pub pipelining: bool,
    pub starttls: bool,
    pub enhanced_status_codes: bool,
    /// Delivery status notifications (RFC 3461).
    pub dsn: bool,
    pub auth: Vec<String>,
    /// All keyword lines as sent.
    pub raw: Vec<String>,
}

impl Extensions {
    fn parse(reply: &Reply) -> Extensions {
        let mut e = Extensions::default();
        // The first line is the greeting domain.
        for line in reply.lines.iter().skip(1) {
            e.raw.push(line.clone());
            let mut words = line.split_ascii_whitespace();
            let Some(kw) = words.next() else { continue };
            match kw.to_ascii_uppercase().as_str() {
                "SIZE" => e.size = Some(words.next().and_then(|w| w.parse().ok()).unwrap_or(0)),
                "8BITMIME" => e.eight_bit_mime = true,
                "SMTPUTF8" => e.smtputf8 = true,
                "PIPELINING" => e.pipelining = true,
                "STARTTLS" => e.starttls = true,
                "ENHANCEDSTATUSCODES" => e.enhanced_status_codes = true,
                "DSN" => e.dsn = true,
                "AUTH" => e.auth.extend(words.map(|w| w.to_ascii_uppercase())),
                k if k.starts_with("AUTH=") => {
                    // Old-style `AUTH=LOGIN PLAIN`.
                    e.auth.extend(
                        std::iter::once(k.trim_start_matches("AUTH=").to_owned())
                            .chain(words.map(|w| w.to_ascii_uppercase())),
                    );
                }
                _ => {}
            }
        }
        e.auth.sort();
        e.auth.dedup();
        e
    }
}

/// Prefix of submission-only header fields (removed by [`SmtpClient::send`] before DATA).
pub const SUBMIT_HEADER_PREFIX: &str = "X-MailCraft-Submit-";
/// Submission-only field asking for a delivery status notification, e.g.
/// `X-MailCraft-Submit-DSN: NOTIFY=SUCCESS,FAILURE; RET=HDRS`.
pub const DSN_HEADER: &str = "X-MailCraft-Submit-DSN";
/// Longest ENVID (RFC 3461 section 4.4: 100 characters).
const MAX_ENVID: usize = 100;

/// Delivery status notification request (RFC 3461) for one message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DsnRequest {
    pub success: bool,
    pub failure: bool,
    pub delay: bool,
    /// RET=HDRS (only the headers come back) rather than RET=FULL.
    pub headers_only: bool,
    /// ENVID: identifies the message in the reports (the Message-ID without brackets, when short enough).
    pub envid: Option<String>,
}

impl DsnRequest {
    /// Parses the value of [`DSN_HEADER`] (`NOTIFY=SUCCESS,FAILURE; RET=HDRS`). Unknown words are ignored;
    /// `None` when nothing is requested.
    pub fn parse(value: &str) -> Option<DsnRequest> {
        let mut d = DsnRequest { headers_only: true, ..DsnRequest::default() };
        // Whitespace next to `,` and `=` (a folded list) does not separate words.
        let mut norm = String::with_capacity(value.len());
        for ch in value.chars() {
            if ch.is_whitespace() && norm.ends_with([',', '=']) {
                continue;
            }
            if matches!(ch, ',' | '=') {
                let trimmed = norm.trim_end().len();
                norm.truncate(trimmed);
            }
            norm.push(ch);
        }
        for part in norm.split([';', ' ', '\t']).map(str::trim).filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            match k.to_ascii_uppercase().as_str() {
                "NOTIFY" => {
                    for w in v.split(',') {
                        match w.trim().to_ascii_uppercase().as_str() {
                            "SUCCESS" => d.success = true,
                            "FAILURE" => d.failure = true,
                            "DELAY" => d.delay = true,
                            _ => {}
                        }
                    }
                }
                "RET" => d.headers_only = !v.trim().eq_ignore_ascii_case("FULL"),
                _ => {}
            }
        }
        (d.success || d.failure || d.delay).then_some(d)
    }

    /// The NOTIFY keyword list.
    pub(crate) fn notify(&self) -> String {
        let mut v = Vec::new();
        if self.success {
            v.push("SUCCESS");
        }
        if self.failure {
            v.push("FAILURE");
        }
        if self.delay {
            v.push("DELAY");
        }
        if v.is_empty() { "NEVER".to_owned() } else { v.join(",") }
    }
}

/// RFC 3461 section 4 xtext: `+` followed by two upper-case hex digits for `+`, `=`, and bytes outside
/// `!`..`~`.
pub fn xtext(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if (33..=126).contains(&b) && b != b'+' && b != b'=' {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("+{b:02X}"));
        }
    }
    out
}

/// Splits `raw` into the submission options it asks for and the message to transmit: every
/// `X-MailCraft-Submit-*` field (with its folded continuation lines) is removed from the header section;
/// the body is untouched. The ENVID comes from the Message-ID.
pub fn take_submit_options(raw: &[u8]) -> (Option<DsnRequest>, Vec<u8>) {
    let head_len = header_section(raw).len();
    let (head, body) = raw.split_at(head_len.min(raw.len()));
    let mut out = Vec::with_capacity(raw.len());
    let mut dsn = None;
    let mut message_id = None;
    // Header fields with their continuation lines.
    let mut fields: Vec<Vec<u8>> = Vec::new();
    for line in head.split_inclusive(|&b| b == b'\n') {
        let folded = line.first().is_some_and(|&b| b == b' ' || b == b'\t');
        match fields.last_mut() {
            Some(last) if folded => last.extend_from_slice(line),
            _ => fields.push(line.to_vec()),
        }
    }
    for f in fields {
        let text = String::from_utf8_lossy(&f).into_owned();
        let (name, value) = text.split_once(':').unwrap_or((text.as_str(), ""));
        let name = name.trim();
        let value: String =
            value.split(['\r', '\n']).map(str::trim).filter(|v| !v.is_empty()).collect::<Vec<_>>().join(" ");
        let skipping = name.len() >= SUBMIT_HEADER_PREFIX.len()
            && name
                .get(..SUBMIT_HEADER_PREFIX.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(SUBMIT_HEADER_PREFIX));
        if skipping {
            if name.eq_ignore_ascii_case(DSN_HEADER) {
                dsn = DsnRequest::parse(&value);
            }
            continue;
        }
        if name.eq_ignore_ascii_case("Message-ID") {
            message_id = Some(value.trim().trim_start_matches('<').trim_end_matches('>').to_owned());
        }
        out.extend_from_slice(&f);
    }
    out.extend_from_slice(body);
    if let Some(d) = dsn.as_mut()
        && let Some(id) = message_id.filter(|i| !i.is_empty() && i.is_ascii())
    {
        let x = xtext(&id);
        if x.len() <= MAX_ENVID {
            d.envid = Some(x);
        }
    }
    (dsn, out)
}

/// A recipient the server refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipientError {
    pub address: String,
    pub code: u16,
    pub enhanced: Option<String>,
    pub message: String,
    pub permanent: bool,
}

/// Why sending failed (fully or for some recipients).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendError {
    /// Retrying the same message will fail again (5xx, invalid input). Otherwise retry later.
    pub permanent: bool,
    pub message: String,
    /// Recipients the server refused at RCPT TO.
    pub recipients: Vec<RecipientError>,
    /// Recipients the message was delivered to (non-empty means it was sent to some of them).
    pub accepted: Vec<String>,
    /// The transport error behind a temporary failure, if any.
    pub net: Option<Box<NetError>>,
}

impl SendError {
    fn whole(permanent: bool, message: impl Into<String>) -> SendError {
        SendError {
            permanent,
            message: message.into(),
            recipients: Vec::new(),
            accepted: Vec::new(),
            net: None,
        }
    }
    fn from_reply(r: &Reply, context: &str) -> SendError {
        SendError::whole(r.is_permanent(), format!("{context}: {}", r.describe()))
    }
    fn from_net(e: NetError) -> SendError {
        let permanent = matches!(
            e,
            NetError::Invalid(_) | NetError::Auth(_) | NetError::Unsupported(_) | NetError::Insecure(_)
        );
        SendError {
            permanent,
            message: e.to_string(),
            recipients: Vec::new(),
            accepted: Vec::new(),
            net: Some(Box::new(e)),
        }
    }
    /// True when the message went out to at least one recipient.
    pub fn partially_sent(&self) -> bool {
        !self.accepted.is_empty()
    }
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        for r in &self.recipients {
            write!(f, "; {}: {} {}", r.address, r.code, r.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for SendError {}

impl From<NetError> for SendError {
    fn from(e: NetError) -> Self {
        SendError::from_net(e)
    }
}

/// Dot-stuffs a message for DATA (RFC 5321 §4.5.2), normalising bare CR and LF to CRLF, and appends the
/// terminating `.` line.
pub fn dot_stuff(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 64 + 8);
    let mut line_start = true;
    let mut it = raw.iter().copied().peekable();
    while let Some(b) = it.next() {
        if line_start && b == b'.' {
            out.push(b'.');
        }
        match b {
            b'\r' => {
                if it.peek() == Some(&b'\n') {
                    it.next();
                }
                out.extend_from_slice(b"\r\n");
                line_start = true;
            }
            b'\n' => {
                out.extend_from_slice(b"\r\n");
                line_start = true;
            }
            _ => {
                out.push(b);
                line_start = false;
            }
        }
    }
    if !out.is_empty() && !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

/// Octets of `raw` once bare CR and LF are turned into CRLF and a final CRLF is added (what DATA sends,
/// before dot-stuffing).
fn wire_size(raw: &[u8]) -> u64 {
    let mut n = 0u64;
    let mut it = raw.iter().copied().peekable();
    let mut last_was_eol = false;
    while let Some(b) = it.next() {
        match b {
            b'\r' => {
                if it.peek() == Some(&b'\n') {
                    it.next();
                }
                n = n.saturating_add(2);
                last_was_eol = true;
            }
            b'\n' => {
                n = n.saturating_add(2);
                last_was_eol = true;
            }
            _ => {
                n = n.saturating_add(1);
                last_was_eol = false;
            }
        }
    }
    if n > 0 && !last_was_eol {
        n = n.saturating_add(2);
    }
    n
}

/// The header section of a message: everything before the first empty line (all of it when there is none).
fn header_section(raw: &[u8]) -> &[u8] {
    let mut line_start = 0usize;
    for (i, &b) in raw.iter().enumerate() {
        if b == b'\n' {
            let line = raw.get(line_start..i).unwrap_or_default();
            if line.is_empty() || line == b"\r" {
                return raw.get(..line_start).unwrap_or_default();
            }
            line_start = i + 1;
        }
    }
    raw
}

fn valid_address(a: &str) -> bool {
    a.len() <= 512 && !a.bytes().any(|b| b < 0x20 || b == 0x7f || matches!(b, b'<' | b'>' | b' '))
}

fn address_literal(addr: Option<SocketAddr>) -> String {
    match addr {
        Some(SocketAddr::V4(a)) => format!("[{}]", a.ip()),
        Some(SocketAddr::V6(a)) => format!("[IPv6:{}]", a.ip()),
        None => "localhost".into(),
    }
}

/// A connected SMTP session.
#[derive(Debug)]
pub struct SmtpClient {
    conn: Conn,
    settings: SmtpSettings,
    ext: Extensions,
    greeting: String,
    authenticated: bool,
    broken: bool,
}

impl SmtpClient {
    /// Connects, reads the greeting, says EHLO, upgrades with STARTTLS when configured, and logs in when
    /// credentials are set.
    pub fn connect(settings: &SmtpSettings) -> Result<SmtpClient> {
        let conn = Conn::connect(
            &settings.host,
            settings.port,
            settings.security == Security::Tls,
            &settings.options,
        )?;
        let mut c = SmtpClient {
            conn,
            settings: settings.clone(),
            ext: Extensions::default(),
            greeting: String::new(),
            authenticated: false,
            broken: false,
        };
        let g = c.read_reply()?;
        if g.code != 220 {
            return Err(NetError::Refused(format!("server greeting: {}", g.describe())));
        }
        c.greeting = g.text();
        c.ehlo()?;
        if settings.security == Security::StartTls {
            c.starttls()?;
        }
        if let Some(creds) = settings.credentials.clone() {
            c.authenticate(&creds, &settings.auth_mechanism.clone())?;
        }
        Ok(c)
    }

    pub fn extensions(&self) -> &Extensions {
        &self.ext
    }

    pub fn greeting(&self) -> &str {
        &self.greeting
    }

    pub fn is_tls(&self) -> bool {
        self.conn.is_tls()
    }

    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// False after a transport error: reconnect.
    pub fn is_usable(&self) -> bool {
        !self.broken
    }

    fn read_reply(&mut self) -> Result<Reply> {
        let r = self.read_reply_inner();
        if r.is_err() {
            self.broken = true;
        }
        r
    }

    fn read_reply_inner(&mut self) -> Result<Reply> {
        let mut lines = Vec::new();
        let mut code = 0u16;
        loop {
            let line = self.conn.read_line(MAX_REPLY_LINE)?;
            let line = String::from_utf8_lossy(&line).into_owned();
            let digits =
                line.get(..3).ok_or_else(|| NetError::Protocol(format!("short reply line `{line}`")))?;
            let c: u16 = digits
                .parse()
                .ok()
                .filter(|c| (200..600).contains(c))
                .ok_or_else(|| NetError::Protocol(format!("bad reply code in `{line}`")))?;
            if lines.is_empty() {
                code = c;
            } else if c != code {
                return Err(NetError::Protocol(format!("reply code changed mid-reply: {code} then {c}")));
            }
            let sep = line.as_bytes().get(3).copied();
            lines.push(line.get(4..).unwrap_or_default().to_owned());
            if lines.len() > MAX_REPLY_LINES {
                return Err(NetError::TooLarge("reply has too many lines".into()));
            }
            if sep != Some(b'-') {
                break;
            }
        }
        let split = lines.first().and_then(|first| {
            let (head, rest) = first.split_once(' ').unwrap_or((first.as_str(), ""));
            is_enhanced(head, code).then(|| (head.to_owned(), rest.to_owned()))
        });
        let mut enhanced = None;
        if let (Some((head, rest)), Some(first)) = (split, lines.first_mut()) {
            enhanced = Some(head);
            *first = rest;
        }
        Ok(Reply { code, enhanced, lines })
    }

    fn send_line(&mut self, line: &str) -> Result<()> {
        if line.bytes().any(|b| b == b'\r' || b == b'\n') {
            return Err(NetError::Invalid("CR or LF in an SMTP command".into()));
        }
        let mut data = line.as_bytes().to_vec();
        data.extend_from_slice(b"\r\n");
        self.conn.write_all(&data).inspect_err(|_| self.broken = true)
    }

    /// Sends a command and reads one reply.
    pub fn command(&mut self, line: &str) -> Result<Reply> {
        if self.broken {
            return Err(NetError::Closed);
        }
        self.send_line(line)?;
        self.read_reply()
    }

    fn helo_name(&self) -> String {
        if self.settings.helo_name.is_empty() {
            address_literal(self.conn.local_addr())
        } else {
            self.settings.helo_name.clone()
        }
    }

    /// EHLO, falling back to HELO for old servers.
    pub fn ehlo(&mut self) -> Result<()> {
        let name = self.helo_name();
        let r = self.command(&format!("EHLO {name}"))?;
        if r.is_positive() {
            self.ext = Extensions::parse(&r);
            return Ok(());
        }
        let r = self.command(&format!("HELO {name}"))?;
        if !r.is_positive() {
            return Err(NetError::Refused(format!("HELO: {}", r.describe())));
        }
        self.ext = Extensions::default();
        Ok(())
    }

    /// STARTTLS (RFC 3207), then EHLO again.
    pub fn starttls(&mut self) -> Result<()> {
        if self.conn.is_tls() {
            return Ok(());
        }
        if !self.ext.starttls {
            return Err(NetError::Unsupported("STARTTLS".into()));
        }
        let r = self.command("STARTTLS")?;
        if r.code != 220 {
            return Err(NetError::Tls(format!("STARTTLS refused: {}", r.describe())));
        }
        self.conn.start_tls().inspect_err(|_| self.broken = true)?;
        self.ehlo()
    }

    /// AUTH (RFC 4954) with `creds`; `preferred` names a mechanism or is empty for automatic.
    pub fn authenticate(&mut self, creds: &Credentials, preferred: &str) -> Result<()> {
        if !self.conn.is_tls() && !self.settings.options.allow_plaintext_auth {
            return Err(NetError::Insecure(format!("SMTP AUTH to {} without TLS", self.settings.host)));
        }
        let Some(mech) = sasl::choose(creds, &self.ext.auth, preferred) else {
            return Err(NetError::Unsupported(format!(
                "no usable login mechanism (server offers: {})",
                if self.ext.auth.is_empty() { "none".to_owned() } else { self.ext.auth.join(" ") }
            )));
        };
        let r = match (mech, creds) {
            (Mechanism::Login, Credentials::Password { user, password }) => {
                let r = self.command("AUTH LOGIN")?;
                if r.code != 334 {
                    return Err(NetError::Auth(r.describe()));
                }
                let r = self.command(&sasl::b64(user.as_bytes()))?;
                if r.code != 334 {
                    return Err(NetError::Auth(r.describe()));
                }
                self.command(&sasl::b64(password.as_bytes()))?
            }
            _ => {
                let host = self.settings.host.clone();
                let initial =
                    sasl::initial_response(mech, creds, &host, self.settings.port).unwrap_or_default();
                let enc = if initial.is_empty() { "=".to_owned() } else { sasl::b64(&initial) };
                let mut r = self.command(&format!("AUTH {} {enc}", mech.name()))?;
                let mut challenges = 0usize;
                while r.code == 334 {
                    // A challenge after the initial response reports an error; answer as the mechanism
                    // requires (empty, %x01 or `*`) to get the final reply.
                    challenges += 1;
                    if challenges > MAX_SASL_CHALLENGES {
                        self.broken = true;
                        return Err(NetError::Protocol("the server keeps sending SASL challenges".into()));
                    }
                    r = self.command(sasl::error_challenge_reply(mech))?;
                }
                r
            }
        };
        if r.code == 235 {
            self.authenticated = true;
            Ok(())
        } else {
            Err(NetError::Auth(r.describe()))
        }
    }

    /// RSET.
    pub fn reset(&mut self) -> Result<()> {
        let r = self.command("RSET")?;
        if r.is_positive() { Ok(()) } else { Err(NetError::Refused(r.describe())) }
    }

    /// NOOP.
    pub fn noop(&mut self) -> Result<()> {
        let r = self.command("NOOP")?;
        if r.is_positive() { Ok(()) } else { Err(NetError::Refused(r.describe())) }
    }

    /// QUIT and close.
    pub fn quit(&mut self) -> Result<()> {
        if !self.broken {
            let _ = self.command("QUIT");
        }
        self.broken = true;
        self.conn.shutdown();
        Ok(())
    }

    /// Sends `raw` (a complete RFC 5322 message) from `from` to `rcpts`. Succeeds only when every recipient
    /// was accepted and the server took the message; otherwise the error lists refused recipients and
    /// whether the message went out to the others.
    ///
    /// Submission-only `X-MailCraft-Submit-*` header fields are removed first; a DSN request among them
    /// becomes the RFC 3461 envelope parameters when the server advertises DSN.
    pub fn send(&mut self, from: &str, rcpts: &[&str], raw: &[u8]) -> std::result::Result<(), SendError> {
        let (dsn, clean) = take_submit_options(raw);
        self.send_with(from, rcpts, &clean, dsn.as_ref())
    }

    /// [`SmtpClient::send`] with an explicit DSN request (ignored when the server has no DSN extension);
    /// `raw` is sent as is.
    pub fn send_with(
        &mut self,
        from: &str,
        rcpts: &[&str],
        raw: &[u8],
        dsn: Option<&DsnRequest>,
    ) -> std::result::Result<(), SendError> {
        if self.broken {
            return Err(SendError::from_net(NetError::Closed));
        }
        if rcpts.is_empty() {
            return Err(SendError::whole(true, "no recipients"));
        }
        for a in std::iter::once(&from).chain(rcpts.iter()) {
            if !valid_address(a) {
                return Err(SendError::whole(true, format!("invalid address `{}`", a.escape_debug())));
            }
        }
        // `<>` is a valid reverse-path (bounces) but never a forward-path (RFC 5321 section 4.1.1.3).
        if rcpts.iter().any(|r| r.is_empty()) {
            return Err(SendError::whole(true, "empty recipient address"));
        }
        // RFC 1870 section 6: the size is the message as sent, CRLF line endings included, without the
        // stuffing dots and the terminating `.` line.
        let stuffed = dot_stuff(raw);
        let size = wire_size(raw);
        if let Some(max) = self.ext.size
            && max > 0
            && size > max
        {
            return Err(SendError::whole(
                true,
                format!("message is {size} bytes; the server accepts at most {max}"),
            ));
        }
        if !raw.is_ascii() && !self.ext.eight_bit_mime && !self.ext.smtputf8 {
            // RFC 6152 section 3: 8-bit data only to a server that advertised 8BITMIME.
            return Err(SendError::whole(
                true,
                "the message contains 8-bit data and the server does not accept it (no 8BITMIME)",
            ));
        }
        let non_ascii_addr = !from.is_ascii() || rcpts.iter().any(|r| !r.is_ascii());
        if non_ascii_addr && !self.ext.smtputf8 {
            return Err(SendError::whole(
                true,
                "the server does not accept internationalised addresses (SMTPUTF8)",
            ));
        }
        // RFC 6531 section 3.4 / RFC 6532: UTF-8 header fields make an internationalised message, which
        // only an SMTPUTF8 server may receive, and then with the SMTPUTF8 parameter.
        let utf8_headers = !header_section(raw).is_ascii();
        if utf8_headers && !self.ext.smtputf8 {
            return Err(SendError::whole(
                true,
                "the message has UTF-8 header fields and the server does not accept them (no SMTPUTF8)",
            ));
        }
        let mut mail = format!("MAIL FROM:<{from}>");
        if self.ext.size.is_some() {
            mail.push_str(&format!(" SIZE={size}"));
        }
        if !raw.is_ascii() && self.ext.eight_bit_mime {
            mail.push_str(" BODY=8BITMIME");
        }
        if non_ascii_addr || utf8_headers {
            mail.push_str(" SMTPUTF8");
        }
        let dsn = dsn.filter(|_| self.ext.dsn);
        if let Some(d) = dsn {
            mail.push_str(if d.headers_only { " RET=HDRS" } else { " RET=FULL" });
            if let Some(id) = &d.envid {
                mail.push_str(&format!(" ENVID={id}"));
            }
        }
        let rcpt_lines: Vec<String> = rcpts
            .iter()
            .map(|r| match dsn {
                // ORCPT carries the address as typed (RFC 3461 section 4.2); utf-8 addresses would need
                // RFC 6533's utf-8-addr-xtext, so they go without it.
                Some(d) if r.is_ascii() => {
                    format!("RCPT TO:<{r}> NOTIFY={} ORCPT=rfc822;{}", d.notify(), xtext(r))
                }
                Some(d) => format!("RCPT TO:<{r}> NOTIFY={}", d.notify()),
                None => format!("RCPT TO:<{r}>"),
            })
            .collect();

        let (mail_reply, rcpt_replies) = if self.ext.pipelining {
            let mut batch = Vec::new();
            for l in std::iter::once(&mail).chain(rcpt_lines.iter()) {
                batch.extend_from_slice(l.as_bytes());
                batch.extend_from_slice(b"\r\n");
            }
            self.conn.write_all(&batch).inspect_err(|_| self.broken = true)?;
            let m = self.read_reply()?;
            let mut rs = Vec::with_capacity(rcpts.len());
            for _ in rcpts {
                rs.push(self.read_reply()?);
            }
            (m, rs)
        } else {
            let m = self.command(&mail)?;
            let mut rs = Vec::new();
            if m.is_positive() {
                for l in &rcpt_lines {
                    rs.push(self.command(l)?);
                }
            }
            (m, rs)
        };
        if !mail_reply.is_positive() {
            let _ = self.reset();
            return Err(SendError::from_reply(&mail_reply, "sender refused"));
        }
        let mut accepted = Vec::new();
        let mut refused = Vec::new();
        for (addr, r) in rcpts.iter().zip(rcpt_replies.iter()) {
            if r.is_positive() {
                accepted.push((*addr).to_owned());
            } else {
                refused.push(RecipientError {
                    address: (*addr).to_owned(),
                    code: r.code,
                    enhanced: r.enhanced.clone(),
                    message: r.text(),
                    permanent: r.is_permanent(),
                });
            }
        }
        if accepted.is_empty() {
            let _ = self.reset();
            let permanent = refused.iter().all(|r| r.permanent);
            return Err(SendError {
                permanent,
                message: "the server refused every recipient".into(),
                recipients: refused,
                accepted: Vec::new(),
                net: None,
            });
        }
        let r = self.command("DATA")?;
        if r.code != 354 {
            let _ = self.reset();
            return Err(SendError::from_reply(&r, "DATA refused"));
        }
        self.conn.write_all(&stuffed).inspect_err(|_| self.broken = true)?;
        let t = self.settings.data_timeout;
        self.conn.set_read_timeout(Some(t))?;
        let r = self.read_reply();
        let _ = self.conn.reset_read_timeout();
        let r = r?;
        if !r.is_positive() {
            return Err(SendError::from_reply(&r, "message refused"));
        }
        if refused.is_empty() {
            Ok(())
        } else {
            let permanent = refused.iter().all(|r| r.permanent);
            Err(SendError {
                permanent,
                message: format!("sent to {} of {} recipients", accepted.len(), rcpts.len()),
                recipients: refused,
                accepted,
                net: None,
            })
        }
    }
}

fn is_enhanced(s: &str, code: u16) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    let class_ok = parts.first().and_then(|c| c.parse::<u16>().ok()) == Some(code / 100);
    parts.len() == 3
        && class_ok
        && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn stuffing() {
        assert_eq!(dot_stuff(b"a\r\n.b\r\n"), b"a\r\n..b\r\n.\r\n");
        assert_eq!(dot_stuff(b".start\nmid\n.\nend"), b"..start\r\nmid\r\n..\r\nend\r\n.\r\n");
        assert_eq!(dot_stuff(b""), b".\r\n");
        assert_eq!(dot_stuff(b"x\ry"), b"x\r\ny\r\n.\r\n");
        assert_eq!(dot_stuff(b"x\r\n"), b"x\r\n.\r\n");
    }

    #[test]
    fn sizes_and_header_sections() {
        assert_eq!(wire_size(b""), 0);
        assert_eq!(wire_size(b"a\nb"), 6);
        assert_eq!(wire_size(b"a\r\n.b\r\n"), 7);
        assert_eq!(wire_size(b"x\ry"), 6);
        assert_eq!(header_section(b"A: 1\r\nB: 2\r\n\r\nbody"), b"A: 1\r\nB: 2\r\n");
        assert_eq!(header_section(b"A: 1\n\nbody\n\nmore"), b"A: 1\n");
        assert_eq!(header_section(b"A: 1"), b"A: 1");
        assert_eq!(header_section(b"\r\nbody"), b"");
    }

    #[test]
    fn enhanced_codes() {
        assert!(is_enhanced("5.1.1", 550));
        assert!(is_enhanced("2.0.0", 250));
        assert!(!is_enhanced("4.1.1", 550));
        assert!(!is_enhanced("5.1", 550));
        assert!(!is_enhanced("OK", 250));
    }

    #[test]
    fn ehlo_keywords() {
        let r = Reply {
            code: 250,
            enhanced: None,
            lines: vec![
                "mail.example.org hello".into(),
                "SIZE 1000".into(),
                "8BITMIME".into(),
                "PIPELINING".into(),
                "AUTH PLAIN login".into(),
                "AUTH=XOAUTH2".into(),
                "STARTTLS".into(),
            ],
        };
        let e = Extensions::parse(&r);
        assert_eq!(e.size, Some(1000));
        assert!(e.eight_bit_mime && e.pipelining && e.starttls && !e.smtputf8);
        assert_eq!(e.auth, vec!["LOGIN", "PLAIN", "XOAUTH2"]);
        assert!(!e.dsn);
    }

    #[test]
    fn addresses() {
        assert!(valid_address("a@example.com"));
        assert!(valid_address(""));
        assert!(!valid_address("a@b>\r\nRCPT TO:<x"));
        assert!(!valid_address("a b@c"));
    }

    #[test]
    fn dsn_requests_and_xtext() {
        let d = DsnRequest::parse("NOTIFY=SUCCESS,FAILURE; RET=HDRS").unwrap();
        assert!(d.success && d.failure && !d.delay && d.headers_only);
        assert_eq!(d.notify(), "SUCCESS,FAILURE");
        assert!(DsnRequest::parse("RET=FULL").is_none());
        assert!(!DsnRequest::parse("NOTIFY=FAILURE RET=FULL").unwrap().headers_only);
        assert_eq!(xtext("a+b=c d@example.com"), "a+2Bb+3Dc+20d@example.com");
        assert_eq!(xtext("é"), "+C3+A9");
    }

    #[test]
    fn submit_headers_are_removed_with_their_folds() {
        let raw = b"From: a@example.com\r\nX-MailCraft-Submit-DSN: NOTIFY=SUCCESS,\r\n FAILURE; RET=HDRS\r\n\
Message-ID: <id.1@example.com>\r\nx-mailcraft-submit-save-sent: none\r\nSubject: Hi\r\n\r\nX-MailCraft-Submit-DSN: body\r\n";
        let (dsn, clean) = take_submit_options(raw);
        let d = dsn.unwrap();
        assert!(d.success && d.failure);
        assert_eq!(d.envid.as_deref(), Some("id.1@example.com"));
        assert_eq!(
            clean,
            b"From: a@example.com\r\nMessage-ID: <id.1@example.com>\r\nSubject: Hi\r\n\r\nX-MailCraft-Submit-DSN: body\r\n"
        );
        // Nothing to remove: unchanged.
        let plain = b"Subject: x\n\nbody";
        assert_eq!(take_submit_options(plain), (None, plain.to_vec()));
        // Hostile input never panics.
        let _ = take_submit_options(b"X-MailCraft-Submit-DSN\r\n \r\n\r\n");
        let _ = take_submit_options(b"\xff\xfe: \x00\n\tfold");
    }

    /// A one-connection SMTP server on 127.0.0.1 that advertises `ehlo` lines and records what it is sent.
    fn fake_server(ehlo: &'static str) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut w = s;
            let mut seen = Vec::new();
            w.write_all(b"220 test ESMTP\r\n").unwrap();
            let mut in_data = false;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let line = line.trim_end_matches(['\r', '\n']).to_owned();
                seen.push(line.clone());
                if in_data {
                    if line == "." {
                        in_data = false;
                        w.write_all(b"250 2.0.0 queued\r\n").unwrap();
                    }
                    continue;
                }
                let verb = line.split(' ').next().unwrap_or("").to_ascii_uppercase();
                let reply: String = match verb.as_str() {
                    "EHLO" => format!("250-test\r\n{ehlo}250 8BITMIME\r\n"),
                    "DATA" => {
                        in_data = true;
                        "354 go\r\n".into()
                    }
                    "QUIT" => {
                        w.write_all(b"221 bye\r\n").unwrap();
                        break;
                    }
                    _ => "250 2.1.0 ok\r\n".into(),
                };
                w.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        (port, t)
    }

    fn deliver(ehlo: &'static str) -> Vec<String> {
        let (port, t) = fake_server(ehlo);
        let mut set = SmtpSettings::new("127.0.0.1", port, Security::None);
        set.options.io_timeout = Duration::from_secs(10);
        let mut c = SmtpClient::connect(&set).unwrap();
        let raw = b"From: a@example.com\r\nTo: b@example.org\r\nMessage-ID: <m1@example.com>\r\n\
X-MailCraft-Submit-DSN: NOTIFY=SUCCESS,FAILURE; RET=HDRS\r\nSubject: s\r\n\r\nbody\r\n";
        c.send("a@example.com", &["b@example.org"], raw).unwrap();
        c.quit().unwrap();
        t.join().unwrap()
    }

    #[test]
    fn dsn_envelope_parameters_when_the_server_offers_dsn() {
        let seen = deliver("250-DSN\r\n");
        assert!(
            seen.contains(&"MAIL FROM:<a@example.com> RET=HDRS ENVID=m1@example.com".to_owned()),
            "{seen:?}"
        );
        assert!(
            seen.contains(
                &"RCPT TO:<b@example.org> NOTIFY=SUCCESS,FAILURE ORCPT=rfc822;b@example.org".to_owned()
            ),
            "{seen:?}"
        );
        assert!(!seen.iter().any(|l| l.contains("X-MailCraft-Submit")), "{seen:?}");
        assert!(!seen.iter().any(|l| l.starts_with("Return-Receipt-To")));
    }

    #[test]
    fn no_dsn_parameters_without_server_support() {
        let seen = deliver("");
        assert!(seen.contains(&"MAIL FROM:<a@example.com>".to_owned()), "{seen:?}");
        assert!(seen.contains(&"RCPT TO:<b@example.org>".to_owned()), "{seen:?}");
        assert!(!seen.iter().any(|l| l.contains("X-MailCraft-Submit") || l.contains("Return-Receipt-To")));
    }

    proptest! {
        #[test]
        fn stuffed_output_is_well_formed(raw in proptest::collection::vec(prop_oneof![
            Just(b'.'), Just(b'\r'), Just(b'\n'), Just(b'a'), any::<u8>()], 0..300)) {
            let out = dot_stuff(&raw);
            prop_assert!(out.ends_with(b".\r\n"));
            let body = &out[..out.len() - 3];
            // Every CR is followed by LF and every LF preceded by CR.
            for (i, &b) in body.iter().enumerate() {
                if b == b'\r' { prop_assert_eq!(body.get(i + 1), Some(&b'\n')); }
                if b == b'\n' { prop_assert!(i > 0 && body[i - 1] == b'\r'); }
            }
            // No line in the body is a lone dot; un-stuffing restores the CRLF-normalised input.
            let mut unstuffed = Vec::new();
            for line in body.split(|&b| b == b'\n') {
                prop_assert!(line != b".\r");
                let l = line.strip_prefix(b".").unwrap_or(line);
                unstuffed.extend_from_slice(l);
                unstuffed.push(b'\n');
            }
            unstuffed.pop();
            let normalised: Vec<u8> = {
                let s = raw.clone();
                let mut o = Vec::new();
                let mut i = 0;
                while i < s.len() {
                    match s[i] {
                        b'\r' => { o.extend_from_slice(b"\r\n"); if s.get(i + 1) == Some(&b'\n') { i += 1; } }
                        b'\n' => o.extend_from_slice(b"\r\n"),
                        b => o.push(b),
                    }
                    i += 1;
                }
                if !o.is_empty() && !o.ends_with(b"\r\n") { o.extend_from_slice(b"\r\n"); }
                o
            };
            prop_assert_eq!(wire_size(&raw), normalised.len() as u64);
            prop_assert_eq!(unstuffed, normalised);
        }
    }
}
