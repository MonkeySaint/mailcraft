//! Blocking IMAP client (RFC 9051, with RFC 3501 servers supported) plus the extensions mail sync needs:
//! STARTTLS, SASL-IR, LITERAL+/-, SPECIAL-USE (RFC 6154), UIDPLUS (RFC 4315), MOVE (RFC 6851), IDLE
//! (RFC 2177), ENABLE (RFC 5161), CONDSTORE/QRESYNC (RFC 7162), UNSELECT (RFC 3691), UTF8=ACCEPT (RFC 6855).
//!
//! Mailbox names in this API are Unicode; they are encoded to modified UTF-7 on the wire unless UTF8=ACCEPT
//! is enabled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mailcraft_model::Security;

use super::namespace::{self, Namespaces};
use super::parse::{
    Code, Fetch, ListEntry, QuotaResource, Response, Status, Untagged, parse_response, trailing_literal,
};
use super::seqset::SeqSet;
use super::utf7;
use crate::error::{NetError, Result};
use crate::sasl::{self, Credentials, Mechanism};
use crate::transport::{Conn, ConnectOptions};

/// Size caps for server responses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Longest line between literals.
    pub max_line: usize,
    /// Largest complete response, literals included (bounds a single message fetch).
    pub max_response: usize,
    /// Most untagged responses collected for one command (a server cannot stream them forever).
    pub max_untagged: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_line: 8 * 1024 * 1024, max_response: 256 * 1024 * 1024, max_untagged: 2_000_000 }
    }
}

/// Most SASL challenges answered after the initial response before giving up.
const MAX_SASL_CHALLENGES: usize = 4;
/// Most unrequested `+` continuation requests tolerated during one command.
const MAX_STRAY_CONTINUATIONS: usize = 8;

/// State of the selected mailbox, from SELECT/EXAMINE and later unsolicited responses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailboxInfo {
    pub name: String,
    pub exists: u32,
    pub recent: u32,
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub unseen: Option<u32>,
    pub flags: Vec<String>,
    pub permanent_flags: Vec<String>,
    /// RFC 7162; `None` when the server sent NOMODSEQ or lacks CONDSTORE.
    pub highest_modseq: Option<u64>,
    pub read_only: bool,
}

/// What a completed command returned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Done {
    pub untagged: Vec<Untagged>,
    pub code: Option<Code>,
    pub text: String,
}

impl Done {
    /// All FETCH responses.
    pub fn fetches(&self) -> Vec<Fetch> {
        self.untagged
            .iter()
            .filter_map(|u| match u {
                Untagged::Fetch(f) => Some((**f).clone()),
                _ => None,
            })
            .collect()
    }
    /// Response codes from the tagged response and untagged status responses.
    pub fn codes(&self) -> impl Iterator<Item = &Code> {
        self.untagged
            .iter()
            .filter_map(|u| match u {
                Untagged::Status { code: Some(c), .. } => Some(c),
                _ => None,
            })
            .chain(self.code.iter())
    }
    /// UIDs reported as VANISHED (RFC 7162), capped expansion is left to the caller.
    pub fn vanished(&self) -> Vec<SeqSet> {
        self.untagged
            .iter()
            .filter_map(|u| match u {
                Untagged::Vanished { uids, .. } => Some(uids.clone()),
                _ => None,
            })
            .collect()
    }
}

/// UIDs assigned by COPY/MOVE (RFC 4315 COPYUID).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyUid {
    pub uidvalidity: u32,
    /// (source UID, destination UID) pairs.
    pub pairs: Vec<(u32, u32)>,
}

/// How UID STORE changes flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreMode {
    Add,
    Remove,
    Replace,
}

/// Result of an IDLE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdleEvent {
    /// The server reported changes (EXISTS, EXPUNGE, FETCH, VANISHED…).
    Changed(Vec<Untagged>),
    /// Nothing happened before the wait ended.
    Timeout,
    /// The stop flag was raised.
    Stopped,
}

enum Seg {
    Text(Vec<u8>),
    Literal { data: Vec<u8>, binary: bool },
}

/// A command under construction.
pub struct Command {
    segs: Vec<Seg>,
    cur: Vec<u8>,
    /// The next item follows without a space (just after an opening parenthesis).
    tight: bool,
}

fn atom_safe(s: &[u8]) -> bool {
    !s.is_empty()
        && s.iter().all(|&b| {
            (0x21..0x7f).contains(&b) && !matches!(b, b'(' | b')' | b'{' | b'"' | b'\\' | b'%' | b'*' | b']')
        })
}

impl Command {
    pub fn new(name: &str) -> Command {
        Command { segs: Vec::new(), cur: name.as_bytes().to_vec(), tight: false }
    }
    /// The space before the next item (none right after [`Command::open`]).
    fn space(&mut self) {
        if !std::mem::take(&mut self.tight) {
            self.cur.push(b' ');
        }
    }
    /// Appends ` raw` verbatim (atoms, sets, parenthesised lists). CR/LF is rejected when sent.
    pub fn raw(mut self, s: &str) -> Command {
        self.space();
        self.cur.extend_from_slice(s.as_bytes());
        self
    }
    /// Opens a parenthesised list: ` (`; the next item follows without a space.
    pub fn open(mut self) -> Command {
        self.space();
        self.cur.push(b'(');
        self.tight = true;
        self
    }
    /// Closes a parenthesised list: `)` right after the last item.
    pub fn close(mut self) -> Command {
        self.tight = false;
        self.cur.push(b')');
        self
    }
    /// The command as it would go on the wire (each literal inline as `{n}` CRLF data), for tests and logs.
    pub fn wire_preview(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for s in &self.segs {
            match s {
                Seg::Text(t) => out.extend_from_slice(t),
                Seg::Literal { data, binary } => {
                    if *binary {
                        out.push(b'~');
                    }
                    out.extend_from_slice(format!("{{{}}}\r\n", data.len()).as_bytes());
                    out.extend_from_slice(data);
                }
            }
        }
        out.extend_from_slice(&self.cur);
        out
    }
    /// Appends a string as an atom, quoted string or literal, whichever is valid.
    pub fn string(mut self, s: &[u8], utf8: bool) -> Command {
        let quotable = s.len() < 1024
            && s.iter().all(|&b| b != 0 && b != b'\r' && b != b'\n')
            && (s.is_ascii() || (utf8 && std::str::from_utf8(s).is_ok()));
        if atom_safe(s) {
            self.space();
            self.cur.extend_from_slice(s);
        } else if quotable {
            self.space();
            self.cur.push(b'"');
            for &b in s {
                if b == b'"' || b == b'\\' {
                    self.cur.push(b'\\');
                }
                self.cur.push(b);
            }
            self.cur.push(b'"');
        } else {
            self = self.literal(s, false);
        }
        self
    }
    /// Appends a literal (`{n}` or, for `binary`, `~{n}`).
    pub fn literal(mut self, data: &[u8], binary: bool) -> Command {
        self.space();
        let text = std::mem::take(&mut self.cur);
        self.segs.push(Seg::Text(text));
        self.segs.push(Seg::Literal { data: data.to_vec(), binary });
        self
    }
    fn into_segs(mut self) -> Vec<Seg> {
        if !self.cur.is_empty() {
            self.segs.push(Seg::Text(self.cur));
        }
        self.segs
    }
}

/// A connected IMAP session.
pub struct ImapClient {
    conn: Conn,
    port: u16,
    next_tag: u32,
    caps: Vec<String>,
    enabled: Vec<String>,
    selected: Option<MailboxInfo>,
    limits: Limits,
    authenticated: bool,
    poisoned: bool,
    bye: Option<String>,
}

impl std::fmt::Debug for ImapClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapClient")
            .field("conn", &self.conn)
            .field("caps", &self.caps)
            .field("selected", &self.selected.as_ref().map(|s| &s.name))
            .finish()
    }
}

impl ImapClient {
    /// Connects, reads the greeting, and (for [`Security::StartTls`]) upgrades with STARTTLS. The session
    /// is then in the not-authenticated state (or authenticated after a PREAUTH greeting).
    pub fn connect(host: &str, port: u16, security: Security, opts: &ConnectOptions) -> Result<ImapClient> {
        let conn = Conn::connect(host, port, security == Security::Tls, opts)?;
        let mut c = ImapClient {
            conn,
            port,
            next_tag: 1,
            caps: Vec::new(),
            enabled: Vec::new(),
            selected: None,
            limits: Limits::default(),
            authenticated: false,
            poisoned: false,
            bye: None,
        };
        match c.read_response()? {
            Response::Untagged(Untagged::Status { status, code, text }) => {
                match status {
                    Status::Ok => {}
                    Status::PreAuth => c.authenticated = true,
                    _ => return Err(NetError::Refused(format!("server greeting: {text}"))),
                }
                if let Some(Code::Capability(caps)) = code {
                    c.caps = caps;
                }
            }
            other => return Err(NetError::Protocol(format!("unexpected greeting: {other:?}"))),
        }
        if security == Security::StartTls {
            if c.authenticated {
                return Err(NetError::Tls(
                    "server pre-authenticated a plain connection; STARTTLS not possible".into(),
                ));
            }
            c.starttls()?;
        }
        if c.caps.is_empty() {
            c.capability()?;
        }
        Ok(c)
    }

    /// Changes the response size caps.
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    pub fn is_tls(&self) -> bool {
        self.conn.is_tls()
    }

    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// False after a transport or framing error: the connection must be replaced.
    pub fn is_usable(&self) -> bool {
        !self.poisoned
    }

    /// Advertised capabilities as sent.
    pub fn capabilities(&self) -> &[String] {
        &self.caps
    }

    /// Case-insensitive capability test.
    pub fn has_capability(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c.eq_ignore_ascii_case(cap))
    }

    /// Whether an extension was turned on with ENABLE.
    pub fn is_enabled(&self, ext: &str) -> bool {
        self.enabled.iter().any(|c| c.eq_ignore_ascii_case(ext))
    }

    /// SASL mechanisms from `AUTH=` capabilities.
    pub fn auth_mechanisms(&self) -> Vec<String> {
        self.caps
            .iter()
            .filter_map(|c| {
                let (k, v) = c.split_once('=')?;
                k.eq_ignore_ascii_case("AUTH").then(|| v.to_ascii_uppercase())
            })
            .collect()
    }

    /// The selected mailbox.
    pub fn selected(&self) -> Option<&MailboxInfo> {
        self.selected.as_ref()
    }

    /// UTF-8 mailbox names and quoted strings: UTF8=ACCEPT (RFC 6855) or IMAP4rev2 (RFC 9051 section 5.1)
    /// enabled. Merely advertising IMAP4rev2 leaves the session in IMAP4rev1 mode.
    pub fn utf8(&self) -> bool {
        self.is_enabled("UTF8=ACCEPT") || self.is_enabled("UTF8=ONLY") || self.is_enabled("IMAP4rev2")
    }

    /// Encodes a mailbox name for the wire.
    pub fn encode_mailbox(&self, name: &str) -> Vec<u8> {
        if self.utf8() { name.as_bytes().to_vec() } else { utf7::encode(name).into_bytes() }
    }

    /// Decodes a wire mailbox name.
    pub fn decode_mailbox(&self, raw: &str) -> String {
        if self.utf8() { raw.to_owned() } else { utf7::decode(raw) }
    }

    fn mailbox_arg(&self, cmd: Command, name: &str) -> Command {
        let enc = self.encode_mailbox(name);
        cmd.string(&enc, self.utf8())
    }

    // ---- framing --------------------------------------------------------------------------------------

    fn read_response(&mut self) -> Result<Response> {
        if self.poisoned {
            return Err(NetError::Closed);
        }
        let mut raw: Vec<u8> = Vec::new();
        let res = self.read_response_into(&mut raw);
        match res {
            Ok(r) => Ok(r),
            // A timeout before anything arrived leaves the stream in sync (IDLE relies on that).
            Err(NetError::Timeout(m)) if raw.is_empty() => Err(NetError::Timeout(m)),
            Err(e) => {
                self.poisoned = true;
                if matches!(e, NetError::Closed)
                    && let Some(bye) = &self.bye
                {
                    return Err(NetError::Refused(format!("the server closed the connection: {bye}")));
                }
                Err(e)
            }
        }
    }

    fn read_response_into(&mut self, raw: &mut Vec<u8>) -> Result<Response> {
        loop {
            let line = self.conn.read_line(self.limits.max_line)?;
            let first = raw.is_empty();
            raw.extend_from_slice(&line);
            if raw.len() > self.limits.max_response {
                return Err(NetError::TooLarge(format!("response over {} bytes", self.limits.max_response)));
            }
            // Status responses and continuation requests end in resp-text, which never holds a literal
            // (RFC 9051 section 9): text that happens to end in `{3}` is just text.
            if first && is_text_response(&line) {
                break;
            }
            let Some(n) = trailing_literal(&line) else { break };
            let n = usize::try_from(n).map_err(|_| NetError::TooLarge("literal".into()))?;
            if raw.len().saturating_add(n) > self.limits.max_response {
                return Err(NetError::TooLarge(format!("literal of {n} bytes")));
            }
            raw.extend_from_slice(b"\r\n");
            let data = self.conn.read_exact(n)?;
            raw.extend_from_slice(&data);
        }
        let r = parse_response(raw)?;
        Ok(r)
    }

    fn absorb(&mut self, u: &Untagged) {
        match u {
            Untagged::Capability(c) => self.caps = c.clone(),
            Untagged::Enabled(e) => {
                for x in e {
                    if !self.is_enabled(x) {
                        self.enabled.push(x.clone());
                    }
                }
            }
            Untagged::Status { status: Status::Bye, text, .. } => self.bye = Some(text.clone()),
            Untagged::Status { code: Some(code), .. } => self.absorb_code(code),
            Untagged::Exists(n) => {
                if let Some(s) = &mut self.selected {
                    s.exists = *n;
                }
            }
            Untagged::Recent(n) => {
                if let Some(s) = &mut self.selected {
                    s.recent = *n;
                }
            }
            Untagged::Expunge(_) => {
                if let Some(s) = &mut self.selected {
                    s.exists = s.exists.saturating_sub(1);
                }
            }
            Untagged::Vanished { earlier: false, uids } => {
                if let Some(s) = &mut self.selected {
                    let n = u32::try_from(uids.count()).unwrap_or(u32::MAX);
                    s.exists = s.exists.saturating_sub(n);
                }
            }
            Untagged::Flags(f) => {
                if let Some(s) = &mut self.selected {
                    s.flags = f.clone();
                }
            }
            Untagged::Fetch(f) => {
                if let (Some(s), Some(m)) = (&mut self.selected, f.modseq)
                    && s.highest_modseq.is_some_and(|h| m > h)
                {
                    s.highest_modseq = Some(m);
                }
            }
            _ => {}
        }
    }

    fn absorb_code(&mut self, code: &Code) {
        if let Code::Capability(c) = code {
            self.caps = c.clone();
            return;
        }
        let Some(s) = &mut self.selected else { return };
        match code {
            Code::UidValidity(v) => s.uidvalidity = *v,
            Code::UidNext(v) => s.uidnext = *v,
            Code::Unseen(v) => s.unseen = Some(*v),
            Code::PermanentFlags(f) => s.permanent_flags = f.clone(),
            Code::HighestModSeq(m) => s.highest_modseq = Some(*m),
            Code::NoModSeq => s.highest_modseq = None,
            Code::ReadOnly => s.read_only = true,
            Code::ReadWrite => s.read_only = false,
            _ => {}
        }
    }

    fn use_nonsync(&self, len: usize) -> bool {
        self.has_capability("LITERAL+")
            || ((self.has_capability("LITERAL-") || self.has_capability("IMAP4rev2")) && len <= 4096)
    }

    /// Waits for a `+` continuation, collecting untagged responses; a tagged reply ends the command.
    fn await_continue(&mut self, tag: &str, done: &mut Done) -> Result<Option<String>> {
        loop {
            match self.read_response()? {
                Response::Continue(t) => return Ok(Some(t)),
                Response::Untagged(u) => {
                    self.absorb(&u);
                    self.collect(done, u)?;
                }
                Response::Tagged { tag: t, status, code, text } if t == tag => {
                    if let Some(c) = &code {
                        self.absorb_code(c);
                    }
                    done.code = code;
                    done.text = text.clone();
                    return match status {
                        Status::Ok => Ok(None),
                        Status::No => Err(NetError::Refused(text)),
                        _ => Err(NetError::Bad(text)),
                    };
                }
                Response::Tagged { .. } => {}
            }
        }
    }

    /// Reads until the tagged reply for `tag`.
    fn finish(&mut self, tag: &str, mut done: Done) -> Result<Done> {
        let mut stray = 0usize;
        loop {
            match self.read_response()? {
                Response::Untagged(u) => {
                    self.absorb(&u);
                    self.collect(&mut done, u)?;
                }
                Response::Continue(_) => {
                    // Not expected here; answering with an empty line keeps the server moving, a few times.
                    stray += 1;
                    if stray > MAX_STRAY_CONTINUATIONS {
                        self.poisoned = true;
                        return Err(NetError::Protocol(
                            "the server keeps sending continuation requests".into(),
                        ));
                    }
                    self.write(b"\r\n")?;
                }
                Response::Tagged { tag: t, status, code, text } if t == tag => {
                    if let Some(c) = &code {
                        self.absorb_code(c);
                    }
                    done.code = code;
                    done.text = text.clone();
                    return match status {
                        Status::Ok => Ok(done),
                        Status::No => Err(NetError::Refused(text)),
                        _ => Err(NetError::Bad(text)),
                    };
                }
                Response::Tagged { tag: t, .. } => log::debug!("imap: ignoring reply for stale tag {t}"),
            }
        }
    }

    /// Keeps an untagged response for the caller, up to [`Limits::max_untagged`] per command.
    fn collect(&mut self, done: &mut Done, u: Untagged) -> Result<()> {
        if done.untagged.len() >= self.limits.max_untagged {
            self.poisoned = true;
            return Err(NetError::TooLarge(format!(
                "more than {} untagged responses to one command",
                self.limits.max_untagged
            )));
        }
        done.untagged.push(u);
        Ok(())
    }

    /// Refuses to send credentials over a plain connection unless the options allow it.
    fn require_secure_auth(&self) -> Result<()> {
        if self.conn.is_tls() || self.conn.options().allow_plaintext_auth {
            Ok(())
        } else {
            Err(NetError::Insecure(format!("IMAP login to {} without TLS", self.conn.host())))
        }
    }

    fn tag(&mut self) -> String {
        let t = format!("M{:04}", self.next_tag);
        self.next_tag = self.next_tag.wrapping_add(1);
        t
    }

    /// Sends a command and waits for its completion. `NO` is [`NetError::Refused`], `BAD` [`NetError::Bad`].
    pub fn run(&mut self, cmd: Command) -> Result<Done> {
        if self.poisoned {
            return Err(NetError::Closed);
        }
        let segs = cmd.into_segs();
        for s in &segs {
            if let Seg::Text(t) = s
                && t.iter().any(|&b| b == b'\r' || b == b'\n' || b == 0)
            {
                return Err(NetError::Invalid("command text contains CR, LF or NUL".into()));
            }
        }
        let tag = self.tag();
        let mut out = tag.clone().into_bytes();
        out.push(b' ');
        let mut done = Done::default();
        for seg in segs {
            match seg {
                Seg::Text(t) => out.extend_from_slice(&t),
                Seg::Literal { data, binary } => {
                    if binary {
                        out.push(b'~');
                    }
                    if self.use_nonsync(data.len()) {
                        out.extend_from_slice(format!("{{{}+}}\r\n", data.len()).as_bytes());
                    } else {
                        out.extend_from_slice(format!("{{{}}}\r\n", data.len()).as_bytes());
                        self.write(&out)?;
                        out.clear();
                        if self.await_continue(&tag, &mut done)?.is_none() {
                            return Err(NetError::Protocol(
                                "server completed the command before a literal".into(),
                            ));
                        }
                    }
                    out.extend_from_slice(&data);
                }
            }
        }
        out.extend_from_slice(b"\r\n");
        self.write(&out)?;
        self.finish(&tag, done)
    }

    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.conn.write_all(data).inspect_err(|_| self.poisoned = true)
    }

    // ---- connection state -----------------------------------------------------------------------------

    /// CAPABILITY.
    pub fn capability(&mut self) -> Result<Vec<String>> {
        let done = self.run(Command::new("CAPABILITY"))?;
        for u in &done.untagged {
            if let Untagged::Capability(c) = u {
                self.caps = c.clone();
            }
        }
        Ok(self.caps.clone())
    }

    /// STARTTLS (RFC 9051 §6.2.1); capabilities are re-read afterwards.
    pub fn starttls(&mut self) -> Result<()> {
        if self.conn.is_tls() {
            return Ok(());
        }
        if self.caps.is_empty() {
            self.capability()?;
        }
        if !self.has_capability("STARTTLS") {
            return Err(NetError::Unsupported("STARTTLS".into()));
        }
        self.run(Command::new("STARTTLS"))?;
        self.conn.start_tls().inspect_err(|_| self.poisoned = true)?;
        // Nothing learned over the plain connection survives the handshake (RFC 9051 section 6.2.1): a man
        // in the middle could have injected it.
        self.caps.clear();
        self.enabled.clear();
        self.bye = None;
        self.capability()?;
        Ok(())
    }

    /// LOGIN with a user name and password.
    pub fn login(&mut self, user: &str, password: &str) -> Result<()> {
        self.require_secure_auth()?;
        if self.has_capability("LOGINDISABLED") {
            return Err(NetError::Unsupported("LOGIN is disabled on this connection".into()));
        }
        // UTF-8 in quoted strings needs IMAP4rev2 or UTF8=ACCEPT *enabled* (merely advertising IMAP4rev2
        // leaves the session in IMAP4rev1 mode); otherwise non-ASCII goes in a literal.
        let utf8 = self.utf8();
        let cmd = Command::new("LOGIN").string(user.as_bytes(), utf8).string(password.as_bytes(), utf8);
        let done = self.run(cmd).map_err(auth_err)?;
        self.after_auth(&done)
    }

    fn after_auth(&mut self, done: &Done) -> Result<()> {
        self.authenticated = true;
        let fresh = done.codes().any(|c| matches!(c, Code::Capability(_)))
            || done.untagged.iter().any(|u| matches!(u, Untagged::Capability(_)));
        if !fresh {
            self.capability()?;
        }
        Ok(())
    }

    /// AUTHENTICATE with `mech` and an initial response (SASL-IR when offered).
    pub fn authenticate(&mut self, mech: Mechanism, initial: &[u8]) -> Result<()> {
        self.require_secure_auth()?;
        let tag = self.tag();
        let mut done = Done::default();
        if self.has_capability("SASL-IR") {
            // RFC 4959: `=` stands for an empty initial response.
            let encoded = if initial.is_empty() { "=".to_owned() } else { sasl::b64(initial) };
            self.write(format!("{tag} AUTHENTICATE {} {encoded}\r\n", mech.name()).as_bytes())?;
        } else {
            self.write(format!("{tag} AUTHENTICATE {}\r\n", mech.name()).as_bytes())?;
            if self.await_continue(&tag, &mut done).map_err(auth_err)?.is_none() {
                return self.after_auth(&done);
            }
            self.write(format!("{}\r\n", sasl::b64(initial)).as_bytes())?;
        }
        let mut challenges = 0usize;
        loop {
            match self.read_response()? {
                // A further challenge after the initial response is an error report: answer as the
                // mechanism requires and let the server fail the command.
                Response::Continue(_) => {
                    challenges += 1;
                    if challenges > MAX_SASL_CHALLENGES {
                        self.poisoned = true;
                        return Err(NetError::Protocol("the server keeps sending SASL challenges".into()));
                    }
                    let reply = sasl::error_challenge_reply(mech);
                    self.write(format!("{reply}\r\n").as_bytes())?;
                }
                Response::Untagged(u) => {
                    self.absorb(&u);
                    self.collect(&mut done, u)?;
                }
                Response::Tagged { tag: t, status, code, text } if t == tag => {
                    if let Some(c) = &code {
                        self.absorb_code(c);
                    }
                    done.code = code;
                    return match status {
                        Status::Ok => self.after_auth(&done),
                        _ => Err(NetError::Auth(text)),
                    };
                }
                Response::Tagged { .. } => {}
            }
        }
    }

    /// Logs in with `creds`, choosing a mechanism (`preferred` = "PLAIN", "LOGIN", "XOAUTH2", … or empty).
    pub fn login_with(&mut self, creds: &Credentials, preferred: &str) -> Result<()> {
        if self.authenticated {
            return Ok(());
        }
        let offered = self.auth_mechanisms();
        let want_login = preferred.eq_ignore_ascii_case("LOGIN");
        match (creds, sasl::choose(creds, &offered, preferred)) {
            (Credentials::Password { user, password }, None | Some(Mechanism::Login)) => {
                self.login(user, password)
            }
            (Credentials::Password { user, password }, Some(_)) if want_login => self.login(user, password),
            (_, Some(mech)) => {
                let host = self.conn.host().to_owned();
                let initial = sasl::initial_response(mech, creds, &host, self.port).unwrap_or_default();
                self.authenticate(mech, &initial)
            }
            (Credentials::OAuth2 { .. }, None) => Err(NetError::Unsupported(
                "the server offers no OAuth 2.0 login (XOAUTH2/OAUTHBEARER)".into(),
            )),
        }
    }

    /// ENABLE (RFC 5161); returns what the server enabled.
    pub fn enable(&mut self, exts: &[&str]) -> Result<Vec<String>> {
        if !self.has_capability("ENABLE") && !self.has_capability("IMAP4rev2") {
            return Ok(Vec::new());
        }
        let mut cmd = Command::new("ENABLE");
        for e in exts {
            cmd = cmd.raw(e);
        }
        let done = self.run(cmd)?;
        Ok(done
            .untagged
            .iter()
            .filter_map(|u| match u {
                Untagged::Enabled(e) => Some(e.clone()),
                _ => None,
            })
            .flatten()
            .collect())
    }

    /// NOOP: returns any unsolicited updates.
    pub fn noop(&mut self) -> Result<Vec<Untagged>> {
        Ok(self.run(Command::new("NOOP"))?.untagged)
    }

    /// LOGOUT and close the socket. Errors after the command was sent are ignored.
    pub fn logout(&mut self) -> Result<()> {
        if !self.poisoned {
            let r = self.run(Command::new("LOGOUT"));
            if let Err(e) = r
                && !matches!(e, NetError::Closed | NetError::Refused(_))
            {
                self.conn.shutdown();
                return Err(e);
            }
        }
        self.poisoned = true;
        self.conn.shutdown();
        Ok(())
    }

    // ---- mailboxes ------------------------------------------------------------------------------------

    /// LIST `reference` `pattern`; names are decoded. Special-use attributes (RFC 6154) arrive in the
    /// attribute list when the server supports them.
    pub fn list(&mut self, reference: &str, pattern: &str) -> Result<Vec<ListEntry>> {
        self.list_cmd("LIST", reference, pattern)
    }

    /// LSUB (subscribed mailboxes).
    pub fn lsub(&mut self, reference: &str, pattern: &str) -> Result<Vec<ListEntry>> {
        self.list_cmd("LSUB", reference, pattern)
    }

    fn list_cmd(&mut self, verb: &str, reference: &str, pattern: &str) -> Result<Vec<ListEntry>> {
        let utf8 = self.utf8();
        let mut cmd = Command::new(verb);
        cmd = cmd.string(&self.encode_mailbox(reference), utf8);
        let pat = self.encode_mailbox(pattern);
        let pat_ok = !pat.is_empty()
            && pat
                .iter()
                .all(|&b| (0x21..0x7f).contains(&b) && !matches!(b, b'(' | b')' | b'{' | b'"' | b'\\'));
        cmd = if pat_ok { cmd.raw(&String::from_utf8_lossy(&pat)) } else { cmd.string(&pat, utf8) };
        let done = self.run(cmd)?;
        let mut out = Vec::new();
        for u in done.untagged {
            match u {
                Untagged::List(mut e) | Untagged::Lsub(mut e) => {
                    e.name = self.decode_mailbox(&e.name);
                    out.push(e);
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// NAMESPACE (RFC 2342; part of IMAP4rev2), prefixes decoded. A server without it has one personal
    /// namespace with an empty prefix.
    pub fn namespace(&mut self) -> Result<Namespaces> {
        if !self.has_capability("NAMESPACE") && !self.has_capability("IMAP4rev2") {
            return Ok(Namespaces::personal_only(None));
        }
        // A server (or proxy) that advertises NAMESPACE but refuses it has only the personal namespace as
        // far as the client can tell; only a lost connection is an error.
        let done = match self.run(Command::new("NAMESPACE")) {
            Ok(done) => done,
            Err(e) if e.is_connection_lost() => return Err(e),
            Err(e) => {
                log::info!("imap: NAMESPACE refused ({e}); using the personal namespace only");
                return Ok(Namespaces::personal_only(None));
            }
        };
        for u in &done.untagged {
            if let Untagged::Other(line) = u
                && let Some(mut n) = namespace::parse(line)
            {
                for ns in n.personal.iter_mut().chain(n.other_users.iter_mut()).chain(n.shared.iter_mut()) {
                    ns.prefix = self.decode_mailbox(&ns.prefix);
                }
                return Ok(n);
            }
        }
        Ok(Namespaces::personal_only(None))
    }

    pub fn create(&mut self, mailbox: &str) -> Result<()> {
        let cmd = self.mailbox_arg(Command::new("CREATE"), mailbox);
        self.run(cmd).map(|_| ())
    }

    pub fn delete(&mut self, mailbox: &str) -> Result<()> {
        let cmd = self.mailbox_arg(Command::new("DELETE"), mailbox);
        self.run(cmd).map(|_| ())
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let cmd = self.mailbox_arg(Command::new("RENAME"), from);
        let cmd = self.mailbox_arg(cmd, to);
        self.run(cmd).map(|_| ())
    }

    pub fn subscribe(&mut self, mailbox: &str) -> Result<()> {
        let cmd = self.mailbox_arg(Command::new("SUBSCRIBE"), mailbox);
        self.run(cmd).map(|_| ())
    }

    pub fn unsubscribe(&mut self, mailbox: &str) -> Result<()> {
        let cmd = self.mailbox_arg(Command::new("UNSUBSCRIBE"), mailbox);
        self.run(cmd).map(|_| ())
    }

    /// STATUS `mailbox` (`items` such as "MESSAGES UIDNEXT UIDVALIDITY UNSEEN HIGHESTMODSEQ").
    pub fn status(&mut self, mailbox: &str, items: &str) -> Result<Vec<(String, u64)>> {
        let cmd = self.mailbox_arg(Command::new("STATUS"), mailbox).raw(&format!("({items})"));
        let done = self.run(cmd)?;
        Ok(done
            .untagged
            .into_iter()
            .filter_map(|u| match u {
                Untagged::MailboxStatus { items, .. } => Some(items),
                _ => None,
            })
            .flatten()
            .collect())
    }

    /// GETQUOTAROOT `mailbox` (RFC 9208 / RFC 2087): the resources of every quota root the mailbox is under.
    /// `Ok(empty)` when the server does not announce QUOTA.
    pub fn get_quota_root(&mut self, mailbox: &str) -> Result<Vec<(String, Vec<QuotaResource>)>> {
        if !self.has_capability("QUOTA")
            && !self.caps.iter().any(|c| c.to_ascii_uppercase().starts_with("QUOTA="))
        {
            return Ok(Vec::new());
        }
        let cmd = self.mailbox_arg(Command::new("GETQUOTAROOT"), mailbox);
        let done = self.run(cmd)?;
        Ok(done
            .untagged
            .into_iter()
            .filter_map(|u| match u {
                Untagged::Quota { root, resources } => Some((root, resources)),
                _ => None,
            })
            .collect())
    }

    /// SELECT (read-write). Asks for CONDSTORE data when the server supports it.
    pub fn select(&mut self, mailbox: &str) -> Result<MailboxInfo> {
        self.select_impl("SELECT", mailbox)
    }

    /// EXAMINE (read-only).
    pub fn examine(&mut self, mailbox: &str) -> Result<MailboxInfo> {
        self.select_impl("EXAMINE", mailbox)
    }

    fn select_impl(&mut self, verb: &str, mailbox: &str) -> Result<MailboxInfo> {
        self.selected = Some(MailboxInfo { name: mailbox.to_owned(), ..MailboxInfo::default() });
        let mut cmd = self.mailbox_arg(Command::new(verb), mailbox);
        if self.has_capability("CONDSTORE") && !self.is_enabled("CONDSTORE") && !self.is_enabled("QRESYNC") {
            cmd = cmd.raw("(CONDSTORE)");
        }
        match self.run(cmd) {
            Ok(done) => {
                let info = self.selected.clone().unwrap_or_default();
                let read_only = verb == "EXAMINE" || matches!(done.code, Some(Code::ReadOnly));
                let info = MailboxInfo { read_only, ..info };
                self.selected = Some(info.clone());
                Ok(info)
            }
            Err(e) => {
                self.selected = None;
                Err(e)
            }
        }
    }

    /// UNSELECT (RFC 3691) or, without it, CLOSE (which also expunges `\Deleted` messages).
    pub fn unselect(&mut self) -> Result<()> {
        if self.selected.is_none() {
            return Ok(());
        }
        let verb = if self.has_capability("UNSELECT") || self.has_capability("IMAP4rev2") {
            "UNSELECT"
        } else {
            "CLOSE"
        };
        self.run(Command::new(verb))?;
        self.selected = None;
        Ok(())
    }

    fn require_selected(&self) -> Result<()> {
        if self.selected.is_some() { Ok(()) } else { Err(NetError::Invalid("no mailbox is selected".into())) }
    }

    // ---- messages -------------------------------------------------------------------------------------

    /// UID FETCH `set` `items` (e.g. `(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[])`).
    pub fn uid_fetch(&mut self, set: &SeqSet, items: &str) -> Result<Vec<Fetch>> {
        self.require_selected()?;
        if set.is_empty() {
            return Ok(Vec::new());
        }
        let done = self.run(Command::new("UID FETCH").raw(&set.to_string()).raw(items))?;
        Ok(done.fetches())
    }

    /// UID FETCH with CHANGEDSINCE (RFC 7162), and VANISHED when QRESYNC is enabled. Returns the fetches
    /// and the vanished UID sets.
    pub fn uid_fetch_changed_since(
        &mut self,
        set: &SeqSet,
        items: &str,
        modseq: u64,
    ) -> Result<(Vec<Fetch>, Vec<SeqSet>)> {
        self.require_selected()?;
        if set.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let modifier = if self.is_enabled("QRESYNC") {
            format!("(CHANGEDSINCE {modseq} VANISHED)")
        } else {
            format!("(CHANGEDSINCE {modseq})")
        };
        let done = self.run(Command::new("UID FETCH").raw(&set.to_string()).raw(items).raw(&modifier))?;
        Ok((done.fetches(), done.vanished()))
    }

    /// FETCH by message sequence numbers.
    pub fn fetch(&mut self, set: &SeqSet, items: &str) -> Result<Vec<Fetch>> {
        self.require_selected()?;
        if set.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.run(Command::new("FETCH").raw(&set.to_string()).raw(items))?.fetches())
    }

    /// UID SEARCH with raw criteria (`ALL`, `UNSEEN`, `UID 10:*`, `MODSEQ 1234`…).
    pub fn uid_search(&mut self, criteria: &str) -> Result<Vec<u32>> {
        self.uid_search_command(Command::new("UID SEARCH").raw(criteria))
    }

    /// A prepared `UID SEARCH` command, its strings already quoted or sent as literals (see
    /// `crate::search::Criterion::imap_command`).
    pub fn uid_search_command(&mut self, cmd: Command) -> Result<Vec<u32>> {
        self.require_selected()?;
        let done = self.run(cmd)?;
        let mut ids: Vec<u32> = done
            .untagged
            .into_iter()
            .filter_map(|u| match u {
                Untagged::Search { ids, .. } => Some(ids),
                _ => None,
            })
            .flatten()
            .collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// UID STORE. `unchanged_since` adds the RFC 7162 UNCHANGEDSINCE modifier; UIDs that failed it are in
    /// the returned MODIFIED set.
    pub fn uid_store(
        &mut self,
        set: &SeqSet,
        mode: StoreMode,
        flags: &[&str],
        unchanged_since: Option<u64>,
    ) -> Result<(Vec<Fetch>, Option<SeqSet>)> {
        self.require_selected()?;
        if set.is_empty() {
            return Ok((Vec::new(), None));
        }
        for f in flags {
            if f.is_empty() || f.bytes().any(|b| b <= b' ' || matches!(b, b'(' | b')' | b'"' | b'{' | 0x7f)) {
                return Err(NetError::Invalid(format!("bad flag `{f}`")));
            }
        }
        let item = match mode {
            StoreMode::Add => "+FLAGS",
            StoreMode::Remove => "-FLAGS",
            StoreMode::Replace => "FLAGS",
        };
        let mut cmd = Command::new("UID STORE").raw(&set.to_string());
        if let Some(m) = unchanged_since {
            cmd = cmd.raw(&format!("(UNCHANGEDSINCE {m})"));
        }
        cmd = cmd.raw(item).raw(&format!("({})", flags.join(" ")));
        let done = self.run(cmd)?;
        let modified = done.codes().find_map(|c| match c {
            Code::Modified(s) => Some(s.clone()),
            _ => None,
        });
        Ok((done.fetches(), modified))
    }

    fn copy_uid(done: &Done) -> Option<CopyUid> {
        done.codes().find_map(|c| match c {
            Code::CopyUid { uidvalidity, src, dst } => {
                let s = src.expand(1_000_000);
                let d = dst.expand(1_000_000);
                (s.len() == d.len())
                    .then(|| CopyUid { uidvalidity: *uidvalidity, pairs: s.into_iter().zip(d).collect() })
            }
            _ => None,
        })
    }

    /// UID COPY; returns COPYUID data when the server has UIDPLUS.
    pub fn uid_copy(&mut self, set: &SeqSet, dest: &str) -> Result<Option<CopyUid>> {
        self.require_selected()?;
        if set.is_empty() {
            return Ok(None);
        }
        let cmd = self.mailbox_arg(Command::new("UID COPY").raw(&set.to_string()), dest);
        let done = self.run(cmd)?;
        Ok(Self::copy_uid(&done))
    }

    /// UID MOVE (RFC 6851); without MOVE, falls back to COPY + `\Deleted` + UID EXPUNGE (UIDPLUS) or EXPUNGE.
    pub fn uid_move(&mut self, set: &SeqSet, dest: &str) -> Result<Option<CopyUid>> {
        self.require_selected()?;
        if set.is_empty() {
            return Ok(None);
        }
        if self.has_capability("MOVE") || self.has_capability("IMAP4rev2") {
            let cmd = self.mailbox_arg(Command::new("UID MOVE").raw(&set.to_string()), dest);
            let done = self.run(cmd)?;
            return Ok(Self::copy_uid(&done));
        }
        let copied = self.uid_copy(set, dest)?;
        self.uid_store(set, StoreMode::Add, &["\\Deleted"], None)?;
        if self.has_capability("UIDPLUS") {
            self.uid_expunge(set)?;
        } else {
            self.expunge()?;
        }
        Ok(copied)
    }

    /// APPEND a message; returns (UIDVALIDITY, UID) when the server has UIDPLUS.
    pub fn append(
        &mut self,
        mailbox: &str,
        flags: &[&str],
        internal_date: Option<&str>,
        message: &[u8],
    ) -> Result<Option<(u32, u32)>> {
        for f in flags {
            if f.is_empty() || f.bytes().any(|b| b <= b' ' || matches!(b, b'(' | b')' | b'"' | b'{' | 0x7f)) {
                return Err(NetError::Invalid(format!("bad flag `{f}`")));
            }
        }
        let mut cmd = self.mailbox_arg(Command::new("APPEND"), mailbox);
        if !flags.is_empty() {
            cmd = cmd.raw(&format!("({})", flags.join(" ")));
        }
        if let Some(d) = internal_date {
            cmd = cmd.string(d.as_bytes(), false);
        }
        // A NUL may only travel in a literal8 (RFC 3516 BINARY); a plain literal cannot carry it.
        let binary = message.contains(&0);
        if binary && !self.has_capability("BINARY") {
            return Err(NetError::Invalid(
                "the message contains NUL bytes and the server lacks BINARY".into(),
            ));
        }
        cmd = cmd.literal(message, binary);
        let done = self.run(cmd)?;
        Ok(done.codes().find_map(|c| match c {
            Code::AppendUid { uidvalidity, uids } => uids.expand(1).first().map(|u| (*uidvalidity, *u)),
            _ => None,
        }))
    }

    /// EXPUNGE all `\Deleted` messages. Returns expunged sequence numbers, or UIDs when QRESYNC is on.
    pub fn expunge(&mut self) -> Result<Vec<u32>> {
        self.require_selected()?;
        let done = self.run(Command::new("EXPUNGE"))?;
        Ok(expunged(&done))
    }

    /// UID EXPUNGE (RFC 4315): only `\Deleted` messages in `set`.
    pub fn uid_expunge(&mut self, set: &SeqSet) -> Result<Vec<u32>> {
        self.require_selected()?;
        if !self.has_capability("UIDPLUS") && !self.has_capability("IMAP4rev2") {
            return Err(NetError::Unsupported("UIDPLUS (UID EXPUNGE)".into()));
        }
        if set.is_empty() {
            return Ok(Vec::new());
        }
        let done = self.run(Command::new("UID EXPUNGE").raw(&set.to_string()))?;
        Ok(expunged(&done))
    }

    /// IDLE (RFC 2177) until the server reports a change, `max_wait` passes, or `stop` is raised (checked
    /// every half second). Servers drop idle clients after 30 minutes, so keep `max_wait` under that.
    pub fn idle(&mut self, max_wait: Duration, stop: Option<&AtomicBool>) -> Result<IdleEvent> {
        self.require_selected()?;
        if !self.has_capability("IDLE") && !self.has_capability("IMAP4rev2") {
            return Err(NetError::Unsupported("IDLE".into()));
        }
        let tag = self.tag();
        self.write(format!("{tag} IDLE\r\n").as_bytes())?;
        let mut pre = Done::default();
        if self.await_continue(&tag, &mut pre)?.is_none() {
            return Ok(if pre.untagged.is_empty() {
                IdleEvent::Timeout
            } else {
                IdleEvent::Changed(pre.untagged)
            });
        }
        let deadline = crate::transport::deadline_after(max_wait);
        let mut events = pre.untagged;
        let mut stopped = false;
        let wait_result: Result<()> = loop {
            if stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
                stopped = true;
                break Ok(());
            }
            if events.iter().any(is_change) {
                break Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                break Ok(());
            }
            let slice = deadline.saturating_duration_since(now).min(Duration::from_millis(500));
            if let Err(e) = self.conn.set_read_timeout(Some(slice)) {
                break Err(e);
            }
            match self.read_response() {
                Ok(Response::Untagged(u)) => {
                    self.absorb(&u);
                    if events.len() >= self.limits.max_untagged {
                        self.poisoned = true;
                        let _ = self.conn.reset_read_timeout();
                        return Err(NetError::TooLarge("too many untagged responses during IDLE".into()));
                    }
                    events.push(u);
                }
                Ok(Response::Tagged { tag: t, status, text, .. }) if t == tag => {
                    // The server ended IDLE on its own.
                    let _ = self.conn.reset_read_timeout();
                    return match status {
                        Status::Ok => Ok(if events.is_empty() {
                            IdleEvent::Timeout
                        } else {
                            IdleEvent::Changed(events)
                        }),
                        Status::No => Err(NetError::Refused(text)),
                        _ => Err(NetError::Bad(text)),
                    };
                }
                Ok(_) => {}
                Err(NetError::Timeout(_)) => {}
                Err(e) => break Err(e),
            }
        };
        self.conn.reset_read_timeout()?;
        wait_result?;
        self.write(b"DONE\r\n")?;
        let done = self.finish(&tag, Done::default())?;
        events.extend(done.untagged);
        Ok(if stopped && !events.iter().any(is_change) {
            IdleEvent::Stopped
        } else if events.iter().any(is_change) {
            IdleEvent::Changed(events)
        } else {
            IdleEvent::Timeout
        })
    }
}

/// Whether `line` (the first line of a response) is a continuation request or a tagged/untagged status
/// response (`OK`, `NO`, `BAD`, `BYE`, `PREAUTH`): those end in resp-text and carry no literal.
fn is_text_response(line: &[u8]) -> bool {
    if line.first() == Some(&b'+') {
        return true;
    }
    let mut words = line.splitn(3, |&b| b == b' ');
    let _tag = words.next();
    let Some(kw) = words.next() else { return false };
    ["OK", "NO", "BAD", "BYE", "PREAUTH"].iter().any(|s| kw.eq_ignore_ascii_case(s.as_bytes()))
}

fn is_change(u: &Untagged) -> bool {
    matches!(
        u,
        Untagged::Exists(_)
            | Untagged::Expunge(_)
            | Untagged::Fetch(_)
            | Untagged::Vanished { .. }
            | Untagged::Recent(_)
    )
}

fn expunged(done: &Done) -> Vec<u32> {
    let mut out = Vec::new();
    for u in &done.untagged {
        match u {
            Untagged::Expunge(n) => out.push(*n),
            Untagged::Vanished { uids, .. } => out.extend(uids.expand(1_000_000)),
            _ => {}
        }
    }
    out
}

fn auth_err(e: NetError) -> NetError {
    match e {
        NetError::Refused(m) | NetError::Bad(m) => NetError::Auth(m),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(c: Command) -> String {
        c.into_segs()
            .iter()
            .map(|s| match s {
                Seg::Text(t) => String::from_utf8_lossy(t).into_owned(),
                Seg::Literal { data, .. } => format!("<lit {}>", data.len()),
            })
            .collect()
    }

    #[test]
    fn text_responses() {
        assert!(is_text_response(b"+ go {5}"));
        assert!(is_text_response(b"M0001 NO exists: a{3}"));
        assert!(is_text_response(b"* ok [ALERT] x {9}"));
        assert!(is_text_response(b"* BYE"));
        assert!(!is_text_response(b"* 1 FETCH (BODY[] {3}"));
        assert!(!is_text_response(b"* LIST () \"/\" {3}"));
        assert!(!is_text_response(b"* OKAY {3}"));
        assert!(!is_text_response(b""));
    }

    #[test]
    fn argument_encoding() {
        assert_eq!(
            text(Command::new("LOGIN").string(b"joe", false).string(b"p w", false)),
            "LOGIN joe \"p w\""
        );
        assert_eq!(text(Command::new("X").string(b"", false)), "X \"\"");
        assert_eq!(text(Command::new("X").string(b"a\"b\\c", false)), "X \"a\\\"b\\\\c\"");
        assert_eq!(text(Command::new("X").string(b"line\r\nbreak", false)), "X <lit 11>");
        assert_eq!(text(Command::new("X").string("é".as_bytes(), false)), "X <lit 2>");
        assert_eq!(text(Command::new("X").string("é".as_bytes(), true)), "X \"é\"");
        assert_eq!(text(Command::new("X").string(b"a*", false)), "X \"a*\"");
    }
}
