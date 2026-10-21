//! IMAP response parser (RFC 9051 §9 grammar, with RFC 3501 responses still accepted).
//!
//! The reader in `client.rs` assembles one complete response (a line plus any `{n}` literals and their
//! continuation lines) into a byte buffer; [`parse_response`] turns that into a [`Response`]. The parser
//! is total: any byte string yields `Ok` or `Err`, never a panic, with nesting capped at [`MAX_DEPTH`].

use super::seqset::SeqSet;
use crate::error::{NetError, Result};

/// Maximum list nesting (BODYSTRUCTURE of deeply nested multiparts, hostile input).
pub const MAX_DEPTH: usize = 64;

/// A generic IMAP data item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Nil,
    Atom(String),
    Number(u64),
    /// Quoted string or literal.
    Str(Vec<u8>),
    List(Vec<Value>),
}

impl Value {
    /// The value as bytes (`NIL` and lists give `None`).
    pub fn as_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Value::Str(b) => Some(b.clone()),
            Value::Atom(a) => Some(a.as_bytes().to_vec()),
            Value::Number(n) => Some(n.to_string().into_bytes()),
            Value::Nil | Value::List(_) => None,
        }
    }
    /// The value as text (invalid UTF-8 replaced).
    pub fn as_string(&self) -> Option<String> {
        self.as_bytes().map(|b| String::from_utf8_lossy(&b).into_owned())
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(n) => Some(*n),
            Value::Atom(_) | Value::Str(_) => self.as_string().and_then(|s| s.trim().parse().ok()),
            _ => None,
        }
    }
    pub fn as_u32(&self) -> Option<u32> {
        self.as_u64().and_then(|n| u32::try_from(n).ok())
    }
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }
    fn strings(&self) -> Vec<String> {
        match self {
            Value::List(l) => l.iter().filter_map(Value::as_string).collect(),
            other => other.as_string().into_iter().collect(),
        }
    }
}

/// Status of a tagged or untagged status response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    No,
    Bad,
    PreAuth,
    Bye,
}

/// Response codes (`[UIDVALIDITY 3]`, …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Code {
    Alert,
    Capability(Vec<String>),
    PermanentFlags(Vec<String>),
    ReadOnly,
    ReadWrite,
    TryCreate,
    UidNext(u32),
    UidValidity(u32),
    Unseen(u32),
    /// RFC 4315.
    AppendUid {
        uidvalidity: u32,
        uids: SeqSet,
    },
    /// RFC 4315.
    CopyUid {
        uidvalidity: u32,
        src: SeqSet,
        dst: SeqSet,
    },
    UidNotSticky,
    /// RFC 7162.
    HighestModSeq(u64),
    NoModSeq,
    Modified(SeqSet),
    /// RFC 7162: the previously selected mailbox was closed.
    Closed,
    Other {
        name: String,
        args: String,
    },
}

/// A LIST/LSUB entry. `name` is the wire name (modified UTF-7 unless UTF8=ACCEPT is on).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListEntry {
    pub attributes: Vec<String>,
    pub delimiter: Option<char>,
    pub name: String,
}

impl ListEntry {
    /// Case-insensitive attribute test (`\Noselect`, `\Sent`, …).
    pub fn has_attribute(&self, attr: &str) -> bool {
        self.attributes.iter().any(|a| a.eq_ignore_ascii_case(attr))
    }
}

/// An address from an ENVELOPE.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Address {
    pub name: Option<String>,
    pub adl: Option<String>,
    pub mailbox: Option<String>,
    pub host: Option<String>,
}

impl Address {
    /// `mailbox@host`, or `None` for group start/end markers.
    pub fn email(&self) -> Option<String> {
        match (&self.mailbox, &self.host) {
            (Some(m), Some(h)) => Some(format!("{m}@{h}")),
            _ => None,
        }
    }
}

/// FETCH ENVELOPE (strings are raw: RFC 2047 encoded words are left for the MIME layer).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Envelope {
    pub date: Option<String>,
    pub subject: Option<String>,
    pub from: Vec<Address>,
    pub sender: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub in_reply_to: Option<String>,
    pub message_id: Option<String>,
}

/// Content-Disposition from BODYSTRUCTURE.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Disposition {
    pub kind: String,
    pub params: Vec<(String, String)>,
}

/// A non-multipart body part.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SinglePart {
    pub media_type: String,
    pub subtype: String,
    pub params: Vec<(String, String)>,
    pub id: Option<String>,
    pub description: Option<String>,
    pub encoding: String,
    pub size: u64,
    pub lines: Option<u64>,
    /// For message/rfc822 parts: the encapsulated message's envelope and structure.
    pub message: Option<(Box<Envelope>, Box<BodyStructure>)>,
    pub md5: Option<String>,
    pub disposition: Option<Disposition>,
    pub language: Vec<String>,
    pub location: Option<String>,
}

/// FETCH BODYSTRUCTURE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyStructure {
    Single(Box<SinglePart>),
    Multipart {
        parts: Vec<BodyStructure>,
        subtype: String,
        params: Vec<(String, String)>,
        disposition: Option<Disposition>,
        language: Vec<String>,
        location: Option<String>,
    },
}

impl BodyStructure {
    /// Number of leaf parts.
    pub fn leaf_count(&self) -> usize {
        match self {
            BodyStructure::Single(_) => 1,
            BodyStructure::Multipart { parts, .. } => parts.iter().map(BodyStructure::leaf_count).sum(),
        }
    }
}

/// One FETCH response.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fetch {
    /// Message sequence number.
    pub seq: u32,
    pub uid: Option<u32>,
    pub flags: Option<Vec<String>>,
    pub internal_date: Option<String>,
    pub size: Option<u64>,
    pub modseq: Option<u64>,
    /// `BODY[]`, `BINARY[]` or `RFC822`: the whole message.
    pub body: Option<Vec<u8>>,
    /// `BODY[HEADER]` or `RFC822.HEADER`.
    pub header: Option<Vec<u8>>,
    pub envelope: Option<Envelope>,
    pub bodystructure: Option<BodyStructure>,
    /// Anything else, by item name as sent.
    pub other: Vec<(String, Value)>,
}

/// Untagged (`*`) responses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Untagged {
    Status {
        status: Status,
        code: Option<Code>,
        text: String,
    },
    Capability(Vec<String>),
    List(ListEntry),
    Lsub(ListEntry),
    Flags(Vec<String>),
    Exists(u32),
    Recent(u32),
    Expunge(u32),
    Fetch(Box<Fetch>),
    /// SEARCH or SORT results, with the CONDSTORE `(MODSEQ n)` suffix when present.
    Search {
        ids: Vec<u32>,
        modseq: Option<u64>,
    },
    /// STATUS response.
    MailboxStatus {
        mailbox: String,
        items: Vec<(String, u64)>,
    },
    Enabled(Vec<String>),
    /// RFC 7162 VANISHED.
    Vanished {
        earlier: bool,
        uids: SeqSet,
    },
    /// RFC 9208 QUOTAROOT: the quota roots of a mailbox.
    QuotaRoot {
        mailbox: String,
        roots: Vec<String>,
    },
    /// RFC 9208 QUOTA: a root's resources (usage and limit; STORAGE is in units of 1024 octets).
    Quota {
        root: String,
        resources: Vec<QuotaResource>,
    },
    /// Anything not modelled (ID, NAMESPACE, ESEARCH…), as text.
    Other(String),
}

/// One resource of a QUOTA response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotaResource {
    /// Upper-case resource name: STORAGE, MESSAGE, MAILBOX, ANNOTATION-STORAGE…
    pub name: String,
    pub usage: u64,
    pub limit: u64,
}

/// Most quota roots or resources kept from one response.
const MAX_QUOTA_ENTRIES: usize = 64;

/// One complete server response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Tagged {
        tag: String,
        status: Status,
        code: Option<Code>,
        text: String,
    },
    Untagged(Untagged),
    /// `+ text`: continuation request.
    Continue(String),
}

/// If `line` ends with a literal announcement (`{12}`, `{12+}`, `~{12}`), its length.
pub fn trailing_literal(line: &[u8]) -> Option<u64> {
    let body = line.strip_suffix(b"}")?;
    let open = body.iter().rposition(|&b| b == b'{')?;
    let digits = body.get(open + 1..)?;
    let digits = digits.strip_suffix(b"+").unwrap_or(digits);
    if digits.is_empty() || digits.len() > 19 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

fn err(msg: impl Into<String>) -> NetError {
    NetError::Protocol(msg.into())
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

fn is_atom_end(b: u8) -> bool {
    matches!(b, b' ' | b'(' | b')' | b'"' | b'\r' | b'\n') || b < 0x20 || b == 0x7f
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn peek_at(&self, k: usize) -> Option<u8> {
        self.s.get(self.i.saturating_add(k)).copied()
    }
    fn eof(&self) -> bool {
        self.i >= self.s.len()
    }
    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, b: u8) -> Result<()> {
        if self.eat(b) { Ok(()) } else { Err(err(format!("expected `{}` at byte {}", b as char, self.i))) }
    }
    fn skip_sp(&mut self) {
        while self.peek() == Some(b' ') {
            self.i += 1;
        }
    }
    fn rest(&self) -> &'a [u8] {
        self.s.get(self.i..).unwrap_or_default()
    }
    /// Atom characters, with `[...]` sections (which may hold spaces and parentheses) kept inside.
    fn atom_raw(&mut self) -> &'a [u8] {
        let start = self.i;
        let mut depth = 0usize;
        while let Some(b) = self.peek() {
            if b == b'\r' || b == b'\n' {
                break;
            }
            if depth == 0 && is_atom_end(b) {
                break;
            }
            if b == b'[' {
                depth = depth.saturating_add(1);
            } else if b == b']' && depth > 0 {
                depth -= 1;
            }
            self.i += 1;
        }
        self.s.get(start..self.i).unwrap_or_default()
    }
    fn word(&mut self) -> String {
        String::from_utf8_lossy(self.atom_raw()).into_owned()
    }
    fn quoted(&mut self) -> Result<Vec<u8>> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None | Some(b'\r') | Some(b'\n') => return Err(err("unterminated quoted string")),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    match self.peek() {
                        Some(c) if c != b'\r' && c != b'\n' => {
                            out.push(c);
                            self.i += 1;
                        }
                        _ => return Err(err("bad escape in quoted string")),
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }
    fn literal(&mut self) -> Result<Vec<u8>> {
        self.eat(b'~');
        self.expect(b'{')?;
        let start = self.i;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.i += 1;
        }
        let digits = self.s.get(start..self.i).unwrap_or_default();
        let n: usize = std::str::from_utf8(digits)
            .ok()
            .and_then(|d| d.parse().ok())
            .ok_or_else(|| err("bad literal length"))?;
        self.eat(b'+');
        self.expect(b'}')?;
        self.eat(b'\r');
        self.expect(b'\n')?;
        let end = self.i.checked_add(n).ok_or_else(|| err("literal length overflow"))?;
        let data = self.s.get(self.i..end).ok_or_else(|| err("literal longer than the response"))?;
        self.i = end;
        Ok(data.to_vec())
    }
    fn value(&mut self, depth: usize) -> Result<Value> {
        match self.peek() {
            None => Err(err("unexpected end of response")),
            Some(b'(') => {
                if depth >= MAX_DEPTH {
                    return Err(err("lists nested too deeply"));
                }
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_sp();
                    if self.eat(b')') {
                        return Ok(Value::List(items));
                    }
                    if self.eof() {
                        return Err(err("unterminated list"));
                    }
                    items.push(self.value(depth + 1)?);
                }
            }
            Some(b'"') => Ok(Value::Str(self.quoted()?)),
            Some(b'{') => Ok(Value::Str(self.literal()?)),
            Some(b'~') if self.peek_at(1) == Some(b'{') => Ok(Value::Str(self.literal()?)),
            Some(_) => {
                let a = self.atom_raw();
                if a.is_empty() {
                    return Err(err(format!(
                        "unexpected byte 0x{:02x} at {}",
                        self.peek().unwrap_or(0),
                        self.i
                    )));
                }
                if a.eq_ignore_ascii_case(b"NIL") {
                    return Ok(Value::Nil);
                }
                if a.len() <= 19
                    && a.iter().all(u8::is_ascii_digit)
                    && let Some(n) = std::str::from_utf8(a).ok().and_then(|s| s.parse().ok())
                {
                    return Ok(Value::Number(n));
                }
                Ok(Value::Atom(String::from_utf8_lossy(a).into_owned()))
            }
        }
    }
    /// An astring kept verbatim (a mailbox called `007` stays `007`).
    fn astring(&mut self) -> Result<String> {
        match self.peek() {
            Some(b'"' | b'{') => self.value(0)?.as_string().ok_or_else(|| err("bad string")),
            Some(b'~') if self.peek_at(1) == Some(b'{') => {
                self.value(0)?.as_string().ok_or_else(|| err("bad string"))
            }
            _ => {
                let a = self.atom_raw();
                if a.is_empty() {
                    return Err(err("missing mailbox name"));
                }
                Ok(String::from_utf8_lossy(a).into_owned())
            }
        }
    }
    /// An astring that may be empty (`""`, the usual name of a server's only quota root).
    fn astring_or_empty(&mut self) -> Result<String> {
        match self.peek() {
            Some(b'"' | b'{' | b'~') => self.value(0)?.as_string().ok_or_else(|| err("bad string")),
            _ => self.astring(),
        }
    }
    /// The rest of the line as text.
    fn text(&mut self) -> String {
        let r = self.rest();
        self.i = self.s.len();
        String::from_utf8_lossy(r).trim_end().to_owned()
    }
}

/// Parses one complete response.
pub fn parse_response(bytes: &[u8]) -> Result<Response> {
    let mut p = P { s: bytes, i: 0 };
    if p.eat(b'+') {
        p.eat(b' ');
        return Ok(Response::Continue(p.text()));
    }
    if p.eat(b'*') {
        p.expect(b' ')?;
        return Ok(Response::Untagged(untagged(&mut p, bytes)?));
    }
    let tag = p.word();
    if tag.is_empty() {
        return Err(err("missing tag"));
    }
    p.expect(b' ')?;
    let status = match p.word().to_ascii_uppercase().as_str() {
        "OK" => Status::Ok,
        "NO" => Status::No,
        "BAD" => Status::Bad,
        "BYE" => Status::Bye,
        "PREAUTH" => Status::PreAuth,
        other => return Err(err(format!("unknown status `{other}` in tagged response"))),
    };
    let (code, text) = resp_text(&mut p);
    Ok(Response::Tagged { tag, status, code, text })
}

fn resp_text(p: &mut P<'_>) -> (Option<Code>, String) {
    p.skip_sp();
    let mut code = None;
    if p.peek() == Some(b'[')
        && let Some(close) = p.rest().iter().position(|&b| b == b']')
    {
        let inner = p.rest().get(1..close).unwrap_or_default();
        code = Some(parse_code(inner));
        p.i += close + 1;
        p.skip_sp();
    }
    (code, p.text())
}

fn parse_code(inner: &[u8]) -> Code {
    let s = String::from_utf8_lossy(inner);
    let (name, args) = match s.split_once(' ') {
        Some((n, a)) => (n.to_ascii_uppercase(), a.trim().to_owned()),
        None => (s.trim().to_ascii_uppercase(), String::new()),
    };
    let other = || Code::Other { name: name.clone(), args: args.clone() };
    let words: Vec<&str> = args.split_ascii_whitespace().collect();
    let num32 = |i: usize| words.get(i).and_then(|w| w.parse::<u32>().ok());
    match name.as_str() {
        "ALERT" => Code::Alert,
        "READ-ONLY" => Code::ReadOnly,
        "READ-WRITE" => Code::ReadWrite,
        "TRYCREATE" => Code::TryCreate,
        "UIDNOTSTICKY" => Code::UidNotSticky,
        "NOMODSEQ" => Code::NoModSeq,
        "CLOSED" => Code::Closed,
        "CAPABILITY" => Code::Capability(words.iter().map(|w| (*w).to_owned()).collect()),
        "PERMANENTFLAGS" => {
            let mut p = P { s: args.as_bytes(), i: 0 };
            match p.value(0) {
                Ok(v) => Code::PermanentFlags(v.strings()),
                Err(_) => other(),
            }
        }
        "UIDNEXT" => num32(0).map_or_else(other, Code::UidNext),
        "UIDVALIDITY" => num32(0).map_or_else(other, Code::UidValidity),
        "UNSEEN" => num32(0).map_or_else(other, Code::Unseen),
        "HIGHESTMODSEQ" => words.first().and_then(|w| w.parse().ok()).map_or_else(other, Code::HighestModSeq),
        "MODIFIED" => words.first().and_then(|w| SeqSet::parse(w)).map_or_else(other, Code::Modified),
        "APPENDUID" => match (num32(0), words.get(1).and_then(|w| SeqSet::parse(w))) {
            (Some(uidvalidity), Some(uids)) => Code::AppendUid { uidvalidity, uids },
            _ => other(),
        },
        "COPYUID" => match (
            num32(0),
            words.get(1).and_then(|w| SeqSet::parse(w)),
            words.get(2).and_then(|w| SeqSet::parse(w)),
        ) {
            (Some(uidvalidity), Some(src), Some(dst)) => Code::CopyUid { uidvalidity, src, dst },
            _ => other(),
        },
        _ => other(),
    }
}

fn untagged(p: &mut P<'_>, whole: &[u8]) -> Result<Untagged> {
    let first = p.word();
    if !first.is_empty() && first.bytes().all(|b| b.is_ascii_digit()) {
        let n: u32 = first.parse().map_err(|_| err("message number out of range"))?;
        p.expect(b' ')?;
        let kw = p.word().to_ascii_uppercase();
        return match kw.as_str() {
            "EXISTS" => Ok(Untagged::Exists(n)),
            "RECENT" => Ok(Untagged::Recent(n)),
            "EXPUNGE" => Ok(Untagged::Expunge(n)),
            "FETCH" => {
                p.skip_sp();
                let v = p.value(0)?;
                let items = v.as_list().ok_or_else(|| err("FETCH data is not a list"))?;
                Ok(Untagged::Fetch(Box::new(build_fetch(n, items))))
            }
            _ => Ok(Untagged::Other(String::from_utf8_lossy(whole).into_owned())),
        };
    }
    let kw = first.to_ascii_uppercase();
    let status = match kw.as_str() {
        "OK" => Some(Status::Ok),
        "NO" => Some(Status::No),
        "BAD" => Some(Status::Bad),
        "PREAUTH" => Some(Status::PreAuth),
        "BYE" => Some(Status::Bye),
        _ => None,
    };
    if let Some(status) = status {
        let (code, text) = resp_text(p);
        return Ok(Untagged::Status { status, code, text });
    }
    match kw.as_str() {
        "CAPABILITY" => {
            Ok(Untagged::Capability(p.text().split_ascii_whitespace().map(str::to_owned).collect()))
        }
        "ENABLED" => Ok(Untagged::Enabled(p.text().split_ascii_whitespace().map(str::to_owned).collect())),
        "LIST" | "XLIST" => Ok(Untagged::List(list_entry(p)?)),
        "LSUB" => Ok(Untagged::Lsub(list_entry(p)?)),
        "FLAGS" => {
            p.skip_sp();
            Ok(Untagged::Flags(p.value(0)?.strings()))
        }
        "SEARCH" | "SORT" => {
            let mut ids = Vec::new();
            let mut modseq = None;
            loop {
                p.skip_sp();
                if p.eof() {
                    break;
                }
                match p.value(0)? {
                    Value::Number(n) => ids.push(u32::try_from(n).map_err(|_| err("id out of range"))?),
                    Value::List(l) => {
                        let is_modseq = l
                            .first()
                            .and_then(Value::as_string)
                            .is_some_and(|s| s.eq_ignore_ascii_case("MODSEQ"));
                        if is_modseq {
                            modseq = l.get(1).and_then(Value::as_u64);
                        }
                    }
                    _ => return Err(err("bad SEARCH result")),
                }
            }
            Ok(Untagged::Search { ids, modseq })
        }
        "STATUS" => {
            p.skip_sp();
            let mailbox = p.astring()?;
            p.skip_sp();
            let v = p.value(0)?;
            let l = v.as_list().unwrap_or_default();
            let mut items = Vec::new();
            for pair in l.chunks(2) {
                if let [k, v] = pair
                    && let (Some(k), Some(v)) = (k.as_string(), v.as_u64())
                {
                    items.push((k.to_ascii_uppercase(), v));
                }
            }
            Ok(Untagged::MailboxStatus { mailbox, items })
        }
        "QUOTAROOT" => {
            p.skip_sp();
            let mailbox = p.astring()?;
            let mut roots = Vec::new();
            loop {
                p.skip_sp();
                if p.eof() || roots.len() >= MAX_QUOTA_ENTRIES {
                    break;
                }
                roots.push(p.astring_or_empty()?);
            }
            Ok(Untagged::QuotaRoot { mailbox, roots })
        }
        "QUOTA" => {
            p.skip_sp();
            let root = p.astring_or_empty()?;
            p.skip_sp();
            let v = p.value(0)?;
            let list = v.as_list().ok_or_else(|| err("QUOTA resources are not a list"))?;
            let resources = list
                .chunks(3)
                .filter_map(|c| match c {
                    [n, u, l] => Some(QuotaResource {
                        name: n.as_string()?.to_ascii_uppercase(),
                        usage: u.as_u64()?,
                        limit: l.as_u64()?,
                    }),
                    _ => None,
                })
                .take(MAX_QUOTA_ENTRIES)
                .collect();
            Ok(Untagged::Quota { root, resources })
        }
        "VANISHED" => {
            p.skip_sp();
            let mut earlier = false;
            if p.peek() == Some(b'(') {
                let v = p.value(0)?;
                earlier = v.strings().iter().any(|s| s.eq_ignore_ascii_case("EARLIER"));
                p.skip_sp();
            }
            let set = p.word();
            let uids = SeqSet::parse(&set).ok_or_else(|| err("bad VANISHED set"))?;
            Ok(Untagged::Vanished { earlier, uids })
        }
        _ => Ok(Untagged::Other(String::from_utf8_lossy(whole).into_owned())),
    }
}

fn list_entry(p: &mut P<'_>) -> Result<ListEntry> {
    p.skip_sp();
    let attrs = p.value(0)?;
    let attributes = match &attrs {
        Value::List(_) => attrs.strings(),
        _ => return Err(err("LIST attributes are not a list")),
    };
    p.skip_sp();
    let delimiter = match p.value(0)? {
        Value::Nil => None,
        v => v.as_string().and_then(|s| s.chars().next()),
    };
    p.skip_sp();
    let name = p.astring()?;
    // Extended data (CHILDINFO, OLDNAME) is ignored.
    Ok(ListEntry { attributes, delimiter, name })
}

fn build_fetch(seq: u32, items: &[Value]) -> Fetch {
    let mut f = Fetch { seq, ..Fetch::default() };
    let mut it = items.iter();
    while let Some(key) = it.next() {
        let Some(key) = key.as_string() else { continue };
        let Some(val) = it.next() else { break };
        let up = key.to_ascii_uppercase();
        let section =
            up.find('[').and_then(|o| up.get(o + 1..).and_then(|r| r.find(']').and_then(|c| r.get(..c))));
        match (up.as_str(), section) {
            ("UID", _) => f.uid = val.as_u32(),
            ("FLAGS", _) => f.flags = Some(val.strings()),
            ("INTERNALDATE", _) => f.internal_date = val.as_string(),
            ("RFC822.SIZE", _) => f.size = val.as_u64(),
            ("MODSEQ", _) => f.modseq = val.as_list().and_then(|l| l.first()).and_then(Value::as_u64),
            ("ENVELOPE", _) => f.envelope = val.as_list().map(envelope),
            ("BODYSTRUCTURE", None) => f.bodystructure = body_structure(val, 0),
            // Non-extensible BODY: only when BODYSTRUCTURE was not also sent.
            ("BODY", None) => {
                if f.bodystructure.is_none() {
                    f.bodystructure = body_structure(val, 0);
                }
            }
            ("RFC822", _) => f.body = val.as_bytes(),
            ("RFC822.HEADER", _) => f.header = val.as_bytes(),
            (k, Some("")) if k.starts_with("BODY[") || k.starts_with("BINARY[") => f.body = val.as_bytes(),
            (k, Some("HEADER")) if k.starts_with("BODY[") => f.header = val.as_bytes(),
            _ => f.other.push((key, val.clone())),
        }
    }
    f
}

fn opt_string(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_string)
}

fn addresses(v: Option<&Value>) -> Vec<Address> {
    let Some(list) = v.and_then(Value::as_list) else { return Vec::new() };
    list.iter()
        .filter_map(Value::as_list)
        .map(|a| Address {
            name: opt_string(a.first()),
            adl: opt_string(a.get(1)),
            mailbox: opt_string(a.get(2)),
            host: opt_string(a.get(3)),
        })
        .collect()
}

fn envelope(l: &[Value]) -> Envelope {
    Envelope {
        date: opt_string(l.first()),
        subject: opt_string(l.get(1)),
        from: addresses(l.get(2)),
        sender: addresses(l.get(3)),
        reply_to: addresses(l.get(4)),
        to: addresses(l.get(5)),
        cc: addresses(l.get(6)),
        bcc: addresses(l.get(7)),
        in_reply_to: opt_string(l.get(8)),
        message_id: opt_string(l.get(9)),
    }
}

fn params(v: Option<&Value>) -> Vec<(String, String)> {
    let Some(l) = v.and_then(Value::as_list) else { return Vec::new() };
    l.chunks(2)
        .filter_map(|c| match c {
            [k, v] => Some((k.as_string()?, v.as_string().unwrap_or_default())),
            _ => None,
        })
        .collect()
}

fn disposition(v: Option<&Value>) -> Option<Disposition> {
    let l = v?.as_list()?;
    Some(Disposition { kind: l.first()?.as_string()?, params: params(l.get(1)) })
}

fn language(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Nil) | None => Vec::new(),
        Some(v) => v.strings(),
    }
}

/// Interprets a BODYSTRUCTURE value.
pub fn body_structure(v: &Value, depth: usize) -> Option<BodyStructure> {
    if depth > MAX_DEPTH {
        return None;
    }
    let l = v.as_list()?;
    if matches!(l.first(), Some(Value::List(_))) {
        let n = l.iter().take_while(|x| matches!(x, Value::List(_))).count();
        let parts: Vec<BodyStructure> =
            l.iter().take(n).filter_map(|p| body_structure(p, depth + 1)).collect();
        let subtype = opt_string(l.get(n)).unwrap_or_else(|| "MIXED".into());
        return Some(BodyStructure::Multipart {
            parts,
            subtype,
            params: params(l.get(n + 1)),
            disposition: disposition(l.get(n + 2)),
            language: language(l.get(n + 3)),
            location: opt_string(l.get(n + 4)),
        });
    }
    let media_type = opt_string(l.first()).unwrap_or_else(|| "APPLICATION".into());
    let subtype = opt_string(l.get(1)).unwrap_or_else(|| "OCTET-STREAM".into());
    let mut part = SinglePart {
        params: params(l.get(2)),
        id: opt_string(l.get(3)),
        description: opt_string(l.get(4)),
        encoding: opt_string(l.get(5)).unwrap_or_else(|| "7BIT".into()),
        size: l.get(6).and_then(Value::as_u64).unwrap_or(0),
        ..SinglePart::default()
    };
    let mut i = 7;
    let is_message = media_type.eq_ignore_ascii_case("MESSAGE")
        && (subtype.eq_ignore_ascii_case("RFC822") || subtype.eq_ignore_ascii_case("GLOBAL"));
    if is_message && matches!(l.get(7), Some(Value::List(_))) {
        let env = l.get(7).and_then(Value::as_list).map(envelope).unwrap_or_default();
        if let Some(inner) = l.get(8).and_then(|b| body_structure(b, depth + 1)) {
            part.message = Some((Box::new(env), Box::new(inner)));
        }
        part.lines = l.get(9).and_then(Value::as_u64);
        i = 10;
    } else if media_type.eq_ignore_ascii_case("TEXT") {
        part.lines = l.get(7).and_then(Value::as_u64);
        i = 8;
    }
    part.md5 = opt_string(l.get(i));
    part.disposition = disposition(l.get(i + 1));
    part.language = language(l.get(i + 2));
    part.location = opt_string(l.get(i + 3));
    part.media_type = media_type;
    part.subtype = subtype;
    Some(BodyStructure::Single(Box::new(part)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn untagged_of(s: &[u8]) -> Untagged {
        match parse_response(s).unwrap() {
            Response::Untagged(u) => u,
            other => panic!("not untagged: {other:?}"),
        }
    }

    #[test]
    fn quota_responses() {
        assert_eq!(
            untagged_of(b"* QUOTAROOT INBOX \"\" user"),
            Untagged::QuotaRoot { mailbox: "INBOX".into(), roots: vec![String::new(), "user".into()] }
        );
        assert_eq!(
            untagged_of(b"* QUOTA \"\" (STORAGE 10 512 message 3 1000)"),
            Untagged::Quota {
                root: String::new(),
                resources: vec![
                    QuotaResource { name: "STORAGE".into(), usage: 10, limit: 512 },
                    QuotaResource { name: "MESSAGE".into(), usage: 3, limit: 1000 },
                ],
            }
        );
        // Garbled triples are skipped; a missing list is an error, never a panic.
        assert!(matches!(
            untagged_of(b"* QUOTA \"\" (STORAGE x 5 MESSAGE 1)"),
            Untagged::Quota { resources, .. } if resources.is_empty()
        ));
        assert!(parse_response(b"* QUOTA \"\" STORAGE").is_err());
        assert!(
            matches!(untagged_of(b"* QUOTAROOT INBOX"), Untagged::QuotaRoot { roots, .. } if roots.is_empty())
        );
    }

    #[test]
    fn tagged_and_codes() {
        let r = parse_response(b"a1 OK [READ-WRITE] SELECT completed").unwrap();
        assert_eq!(
            r,
            Response::Tagged {
                tag: "a1".into(),
                status: Status::Ok,
                code: Some(Code::ReadWrite),
                text: "SELECT completed".into()
            }
        );
        let r = parse_response(b"A2 OK [APPENDUID 38505 3955] APPEND completed").unwrap();
        let Response::Tagged { code: Some(Code::AppendUid { uidvalidity, uids }), .. } = r else { panic!() };
        assert_eq!((uidvalidity, uids.to_string()), (38505, "3955".into()));
        let r = parse_response(b"A3 OK [COPYUID 38505 304,319:320 3956:3958] Done").unwrap();
        let Response::Tagged { code: Some(Code::CopyUid { src, dst, .. }), .. } = r else { panic!() };
        assert_eq!(src.expand(10), vec![304, 319, 320]);
        assert_eq!(dst.expand(10), vec![3956, 3957, 3958]);
        let r = parse_response(b"A4 NO [TRYCREATE] No such mailbox").unwrap();
        assert!(matches!(r, Response::Tagged { status: Status::No, code: Some(Code::TryCreate), .. }));
        assert!(parse_response(b"A5 MAYBE x").is_err());
        assert_eq!(parse_response(b"+ ").unwrap(), Response::Continue(String::new()));
        assert_eq!(parse_response(b"+").unwrap(), Response::Continue(String::new()));
        assert_eq!(parse_response(b"+ idling").unwrap(), Response::Continue("idling".into()));
    }

    #[test]
    fn untagged_status_codes() {
        let u = untagged_of(b"* OK [UIDVALIDITY 3857529045] UIDs valid");
        assert_eq!(
            u,
            Untagged::Status {
                status: Status::Ok,
                code: Some(Code::UidValidity(3857529045)),
                text: "UIDs valid".into()
            }
        );
        let u = untagged_of(b"* OK [PERMANENTFLAGS (\\Deleted \\Seen \\*)] Limited");
        let Untagged::Status { code: Some(Code::PermanentFlags(f)), .. } = u else { panic!() };
        assert_eq!(f, vec!["\\Deleted", "\\Seen", "\\*"]);
        let u = untagged_of(b"* OK [HIGHESTMODSEQ 715194045007]");
        assert!(matches!(u, Untagged::Status { code: Some(Code::HighestModSeq(715194045007)), .. }));
        let u = untagged_of(b"* OK [CAPABILITY IMAP4rev1 IDLE] hi");
        assert!(matches!(u, Untagged::Status { code: Some(Code::Capability(ref c)), .. } if c.len() == 2));
        let u = untagged_of(b"* BYE logging out");
        assert!(matches!(u, Untagged::Status { status: Status::Bye, .. }));
        let u = untagged_of(b"* OK [X-WEIRD a b] ok");
        assert!(matches!(u, Untagged::Status { code: Some(Code::Other { .. }), .. }));
    }

    #[test]
    fn simple_untagged() {
        assert_eq!(untagged_of(b"* 172 EXISTS"), Untagged::Exists(172));
        assert_eq!(untagged_of(b"* 1 RECENT"), Untagged::Recent(1));
        assert_eq!(untagged_of(b"* 44 EXPUNGE"), Untagged::Expunge(44));
        assert_eq!(
            untagged_of(b"* CAPABILITY IMAP4rev1 STARTTLS AUTH=PLAIN"),
            Untagged::Capability(vec!["IMAP4rev1".into(), "STARTTLS".into(), "AUTH=PLAIN".into()])
        );
        assert_eq!(
            untagged_of(b"* FLAGS (\\Answered \\Flagged $Forwarded)"),
            Untagged::Flags(vec!["\\Answered".into(), "\\Flagged".into(), "$Forwarded".into()])
        );
        assert_eq!(
            untagged_of(b"* SEARCH 2 84 882"),
            Untagged::Search { ids: vec![2, 84, 882], modseq: None }
        );
        assert_eq!(untagged_of(b"* SEARCH"), Untagged::Search { ids: vec![], modseq: None });
        assert_eq!(
            untagged_of(b"* SEARCH 2 5 6 (MODSEQ 917162500)"),
            Untagged::Search { ids: vec![2, 5, 6], modseq: Some(917162500) }
        );
        assert_eq!(
            untagged_of(b"* STATUS blurdybloop (MESSAGES 231 UIDNEXT 44292)"),
            Untagged::MailboxStatus {
                mailbox: "blurdybloop".into(),
                items: vec![("MESSAGES".into(), 231), ("UIDNEXT".into(), 44292)]
            }
        );
        assert_eq!(
            untagged_of(b"* VANISHED (EARLIER) 300:310,405,411"),
            Untagged::Vanished { earlier: true, uids: SeqSet::parse("300:310,405,411").unwrap() }
        );
        assert_eq!(
            untagged_of(b"* VANISHED 405"),
            Untagged::Vanished { earlier: false, uids: SeqSet::from_ids(&[405]) }
        );
        assert_eq!(
            untagged_of(b"* ENABLED CONDSTORE QRESYNC"),
            Untagged::Enabled(vec!["CONDSTORE".into(), "QRESYNC".into()])
        );
        assert!(matches!(untagged_of(b"* NAMESPACE ((\"\" \"/\")) NIL NIL"), Untagged::Other(_)));
    }

    #[test]
    fn list_entries() {
        let u = untagged_of(b"* LIST (\\HasNoChildren \\Sent) \"/\" \"Sent Items\"");
        assert_eq!(
            u,
            Untagged::List(ListEntry {
                attributes: vec!["\\HasNoChildren".into(), "\\Sent".into()],
                delimiter: Some('/'),
                name: "Sent Items".into()
            })
        );
        let Untagged::List(e) = untagged_of(b"* LIST () NIL INBOX") else { panic!() };
        assert_eq!((e.delimiter, e.name.as_str()), (None, "INBOX"));
        let Untagged::List(e) = untagged_of(b"* LIST () \".\" {5}\r\na b c") else { panic!() };
        assert_eq!(e.name, "a b c");
        let Untagged::List(e) = untagged_of(b"* LIST () \"\\\\\" 007") else { panic!() };
        assert_eq!((e.delimiter, e.name.as_str()), (Some('\\'), "007"));
        assert!(e.attributes.is_empty());
        let Untagged::Lsub(e) = untagged_of(b"* LSUB (\\Noselect) \"/\" ~/Mail/foo") else { panic!() };
        assert!(e.has_attribute("\\noselect"));
        assert!(parse_response(b"* LIST \"/\" x").is_err());
    }

    #[test]
    fn fetch_items() {
        let raw = b"* 12 FETCH (FLAGS (\\Seen) INTERNALDATE \"17-Jul-1996 02:44:25 -0700\" RFC822.SIZE 4286 UID 99 MODSEQ (12121231000) BODY[] {11}\r\nhello\r\nbody BODY[HEADER] {4}\r\nH: v)";
        let Untagged::Fetch(f) = untagged_of(raw) else { panic!() };
        assert_eq!(f.seq, 12);
        assert_eq!(f.uid, Some(99));
        assert_eq!(f.flags, Some(vec!["\\Seen".into()]));
        assert_eq!(f.internal_date.as_deref(), Some("17-Jul-1996 02:44:25 -0700"));
        assert_eq!(f.size, Some(4286));
        assert_eq!(f.modseq, Some(12121231000));
        assert_eq!(f.body.as_deref(), Some(&b"hello\r\nbody"[..]));
        assert_eq!(f.header.as_deref(), Some(&b"H: v"[..]));
        let Untagged::Fetch(f) =
            untagged_of(b"* 1 FETCH (UID 5 BODY[]<0> \"abc\" BODY[HEADER.FIELDS (DATE FROM)] {0}\r\n)")
        else {
            panic!()
        };
        assert_eq!(f.body.as_deref(), Some(&b"abc"[..]));
        assert_eq!(f.other.len(), 1);
        assert_eq!(f.other[0].0, "BODY[HEADER.FIELDS (DATE FROM)]");
        let Untagged::Fetch(f) = untagged_of(b"* 2 FETCH (BODY[] NIL)") else { panic!() };
        assert_eq!(f.body, None);
    }

    #[test]
    fn envelope_rfc_example() {
        let raw = br#"* 12 FETCH (ENVELOPE ("Wed, 17 Jul 1996 02:23:25 -0700 (PDT)" "IMAP4rev1 WG mtg summary and minutes" (("Terry Gray" NIL "gray" "cac.washington.edu")) (("Terry Gray" NIL "gray" "cac.washington.edu")) (("Terry Gray" NIL "gray" "cac.washington.edu")) ((NIL NIL "imap" "cac.washington.edu")) ((NIL NIL "minutes" "CNRI.Reston.VA.US")("John Klensin" NIL "KLENSIN" "MIT.EDU")) NIL NIL "<B27397-0100000@cac.washington.edu>"))"#;
        let Untagged::Fetch(f) = untagged_of(raw) else { panic!() };
        let e = f.envelope.unwrap();
        assert_eq!(e.subject.as_deref(), Some("IMAP4rev1 WG mtg summary and minutes"));
        assert_eq!(e.from[0].email().as_deref(), Some("gray@cac.washington.edu"));
        assert_eq!(e.from[0].name.as_deref(), Some("Terry Gray"));
        assert_eq!(e.cc.len(), 2);
        assert_eq!(e.cc[1].email().as_deref(), Some("KLENSIN@MIT.EDU"));
        assert!(e.bcc.is_empty());
        assert_eq!(e.message_id.as_deref(), Some("<B27397-0100000@cac.washington.edu>"));
    }

    #[test]
    fn bodystructure_examples() {
        let raw = br#"* 1 FETCH (BODYSTRUCTURE (("TEXT" "PLAIN" ("CHARSET" "US-ASCII") NIL NIL "7BIT" 1152 23)("TEXT" "PLAIN" ("CHARSET" "US-ASCII" "NAME" "cc.diff") "<960723163407.20117h@cac.washington.edu>" "Compiler diff" "BASE64" 4554 73) "MIXED"))"#;
        let Untagged::Fetch(f) = untagged_of(raw) else { panic!() };
        let BodyStructure::Multipart { parts, subtype, .. } = f.bodystructure.unwrap() else { panic!() };
        assert_eq!(subtype, "MIXED");
        assert_eq!(parts.len(), 2);
        let BodyStructure::Single(p) = &parts[1] else { panic!() };
        assert_eq!(p.encoding, "BASE64");
        assert_eq!(p.size, 4554);
        assert_eq!(p.lines, Some(73));
        assert_eq!(p.params[1], ("NAME".into(), "cc.diff".into()));
        let raw = br#"* 3 FETCH (BODYSTRUCTURE ("TEXT" "PLAIN" ("CHARSET" "utf-8") NIL NIL "QUOTED-PRINTABLE" 10 1 NIL ("inline" NIL) NIL NIL))"#;
        let Untagged::Fetch(f) = untagged_of(raw) else { panic!() };
        let BodyStructure::Single(p) = f.bodystructure.unwrap() else { panic!() };
        assert_eq!(p.disposition.unwrap().kind, "inline");
        let raw = br#"* 4 FETCH (BODYSTRUCTURE ("MESSAGE" "RFC822" NIL NIL NIL "7BIT" 300 (NIL "inner" NIL NIL NIL NIL NIL NIL NIL NIL) ("TEXT" "PLAIN" NIL NIL NIL "7BIT" 20 2) 12) BODY ("APPLICATION" "PDF" NIL NIL NIL "BASE64" 9))"#;
        let Untagged::Fetch(f) = untagged_of(raw) else { panic!() };
        let BodyStructure::Single(p) = f.bodystructure.unwrap() else { panic!() };
        let (env, inner) = p.message.unwrap();
        assert_eq!(env.subject.as_deref(), Some("inner"));
        assert_eq!(inner.leaf_count(), 1);
        assert_eq!(p.lines, Some(12));
    }

    #[test]
    fn hostile_inputs() {
        let deep = format!("* 1 FETCH ({}{})", "(".repeat(200), ")".repeat(200));
        assert!(parse_response(deep.as_bytes()).is_err());
        assert!(parse_response(b"* 1 FETCH (BODY[] {99999}\r\nshort)").is_err());
        assert!(parse_response(b"* 1 FETCH (BODY[] {18446744073709551615}\r\n)").is_err());
        assert!(parse_response(b"* 99999999999 EXISTS").is_err());
        assert!(parse_response(b"* 1 FETCH (FLAGS (\\Seen)").is_err());
        assert!(parse_response(b"* 1 FETCH \"x").is_err());
        assert!(parse_response(b"").is_err());
        assert!(parse_response(b"*").is_err());
        assert!(parse_response(b"* SEARCH x").is_err());
        assert!(parse_response(b"* VANISHED").is_err());
        assert!(parse_response(b"* OK [UIDNEXT").is_ok());
    }

    #[test]
    fn literal_detection() {
        assert_eq!(trailing_literal(b"* 1 FETCH (BODY[] {123}"), Some(123));
        assert_eq!(trailing_literal(b"A1 LOGIN {5+}"), Some(5));
        assert_eq!(trailing_literal(b"x ~{7}"), Some(7));
        assert_eq!(trailing_literal(b"x {}"), None);
        assert_eq!(trailing_literal(b"x {a}"), None);
        assert_eq!(trailing_literal(b"x }"), None);
        assert_eq!(trailing_literal(b"x {99999999999999999999999}"), None);
    }

    proptest! {
        #[test]
        fn never_panics_on_bytes(b in proptest::collection::vec(any::<u8>(), 0..300)) {
            let _ = parse_response(&b);
            let _ = trailing_literal(&b);
        }

        #[test]
        fn never_panics_on_structured(
            parts in proptest::collection::vec(prop_oneof![
                Just("("), Just(")"), Just(" "), Just("\""), Just("\\"), Just("{3}\r\n"), Just("{"), Just("}"),
                Just("NIL"), Just("12"), Just("FETCH"), Just("BODY[]"), Just("UID"), Just("ENVELOPE"),
                Just("BODYSTRUCTURE"), Just("["), Just("]"), Just("FLAGS"), Just("\r\n"), Just("abc"),
                Just("~{2}\r\n"), Just("MODSEQ"), Just("LIST"), Just("VANISHED"), Just("SEARCH"), Just("STATUS"),
            ], 0..60),
            prefix in prop_oneof![Just("* 1 FETCH "), Just("* "), Just("a1 OK "), Just("+ "), Just("* OK ")],
        ) {
            let s = format!("{prefix}{}", parts.concat());
            let _ = parse_response(s.as_bytes());
        }

        #[test]
        fn quoted_roundtrip(s in "[ -~]{0,40}") {
            let q = s.replace('\\', "\\\\").replace('"', "\\\"");
            let raw = format!("* LIST () \"/\" \"{q}\"");
            let Response::Untagged(Untagged::List(e)) = parse_response(raw.as_bytes()).unwrap() else { panic!() };
            prop_assert_eq!(e.name, s);
        }

        #[test]
        fn literal_roundtrip(body in proptest::collection::vec(any::<u8>(), 0..200)) {
            let mut raw = format!("* 1 FETCH (UID 7 BODY[] {{{}}}\r\n", body.len()).into_bytes();
            raw.extend_from_slice(&body);
            raw.extend_from_slice(b" FLAGS ())");
            let Response::Untagged(Untagged::Fetch(f)) = parse_response(&raw).unwrap() else { panic!() };
            prop_assert_eq!(f.body, Some(body));
            prop_assert_eq!(f.uid, Some(7));
        }
    }
}
