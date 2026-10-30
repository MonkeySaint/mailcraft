//! A fake Exchange Web Services endpoint for offline tests (SOAP over HTTP/1.1 on the web server's ports, at
//! `/EWS/Exchange.asmx`), on the same in-memory mail as IMAP, POP3 and JMAP, plus plain-old-XML Autodiscover
//! (`/autodiscover/autodiscover.xml`, and `/<domain>/autodiscover/autodiscover.xml` standing in for the
//! address's own domain).
//!
//! Folders: the user's IMAP mailboxes (`F<id>`, below the root `Froot`; distinguished ids by special use:
//! `inbox`, `sentitems`, `drafts`, `deleteditems`, `junkemail`, `archive`) and three item folders `Fcal`,
//! `Fcon`, `Ftask` (calendar, contacts, tasks). Messages are `M<mailbox>.<uid>` (an id that changes when the
//! message moves, as on Exchange); read state is `\Seen`, the flag (PidTagFlagStatus 2) `\Flagged`.
//! Operations: GetFolder, FindFolder (deep), SyncFolderHierarchy, CreateFolder, UpdateFolder (rename),
//! MoveFolder, DeleteFolder, SyncFolderItems (snapshots per folder, paged), GetItem (MimeContent), FindItem,
//! CreateItem (SaveOnly into a folder; SendOnly / SendAndSaveCopy delivered like the SMTP server;
//! AcceptItem, TentativelyAcceptItem, DeclineItem recorded), UpdateItem (SetItemField / DeleteItemField on
//! simple, indexed and extended fields), MoveItem, DeleteItem, Get/SetUserOofSettings and
//! GetUserAvailability. Contacts, events and tasks are kept as the XML the client sent.
//!
//! Sign-in: HTTP Basic, Bearer (an OAuth token of the test authorization server, or the password) and NTLM
//! (NTLMv2 checked against the user's password, the connection signed in afterwards). With
//! [`crate::Config::ews_ntlm_only`] only NTLM is accepted. Connections are kept alive.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use base64::Engine;

use crate::conn::Conn;
use crate::date::DateTime;
use crate::jmap::{Request, read_request};
use crate::server::Ctx;
use crate::store::{SpecialUse, Store, Submission, append_msg};

/// Requests served on one connection.
const MAX_REQUESTS: usize = 10_000;
/// Snapshots kept per folder.
const MAX_SNAPSHOTS: usize = 400;
const ROOT: &str = "Froot";
/// The item folders (id, element, name, class).
const PIM_FOLDERS: [(&str, &str, &str, &str); 3] = [
    ("Fcal", "CalendarFolder", "Calendar", "IPF.Appointment"),
    ("Fcon", "ContactsFolder", "Contacts", "IPF.Contact"),
    ("Ftask", "TasksFolder", "Tasks", "IPF.Task"),
];

// ---------------------------------------------------------------------------------------------------
// A small XML tree (requests in, stored items out)
// ---------------------------------------------------------------------------------------------------

/// An element with its local name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    pub text: String,
}

impl Node {
    pub fn attr(&self, n: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str())
    }
    pub fn child(&self, n: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == n)
    }
    pub fn all<'a>(&'a self, n: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == n)
    }
    pub fn text_of(&self, n: &str) -> Option<&str> {
        self.child(n).map(|c| c.text.trim())
    }
    /// The first descendant named `n` (depth first).
    pub fn find(&self, n: &str) -> Option<&Node> {
        for c in &self.children {
            if c.name == n {
                return Some(c);
            }
            if let Some(f) = c.find(n) {
                return Some(f);
            }
        }
        None
    }

    /// The element as XML, every name in the `t:` prefix.
    pub fn to_xml(&self) -> String {
        let mut out = format!("<t:{}", self.name);
        for (k, v) in &self.attrs {
            out.push_str(&format!(" {k}=\"{}\"", esc(v)));
        }
        if self.children.is_empty() && self.text.is_empty() {
            out.push_str("/>");
            return out;
        }
        out.push('>');
        out.push_str(&esc(&self.text));
        for c in &self.children {
            out.push_str(&c.to_xml());
        }
        out.push_str(&format!("</t:{}>", self.name));
        out
    }
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(rest.get(..i).unwrap_or_default());
        let tail = rest.get(i..).unwrap_or_default();
        let Some(end) = tail.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = tail.get(1..).unwrap_or_default();
            continue;
        };
        let ent = tail.get(1..end).unwrap_or_default();
        let c = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => {
                u32::from_str_radix(e.get(2..).unwrap_or_default(), 16).ok().and_then(char::from_u32)
            }
            e if e.starts_with('#') => e.get(1..).unwrap_or_default().parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match c {
            Some(c) => out.push(c),
            None => out.push_str(tail.get(..=end).unwrap_or_default()),
        }
        rest = tail.get(end + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

fn local(n: &str) -> String {
    n.rsplit_once(':').map_or(n, |(_, l)| l).to_owned()
}

/// Parses a document (no DTDs; bounded depth).
pub fn parse(input: &str) -> Option<Node> {
    let mut stack: Vec<Node> = Vec::new();
    let mut pos = 0usize;
    while pos < input.len() {
        let rest = input.get(pos..)?;
        let Some(lt) = rest.find('<') else { break };
        if lt > 0 {
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&unescape(rest.get(..lt)?));
            }
            pos += lt;
            continue;
        }
        if let Some(r) = rest.strip_prefix("<?") {
            pos += 2 + r.find("?>")? + 2;
            continue;
        }
        if let Some(r) = rest.strip_prefix("<!--") {
            pos += 4 + r.find("-->")? + 3;
            continue;
        }
        if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let end = r.find("]]>")?;
            if let Some(top) = stack.last_mut() {
                top.text.push_str(r.get(..end)?);
            }
            pos += 9 + end + 3;
            continue;
        }
        if rest.starts_with("<!") {
            return None;
        }
        if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>')?;
            let el = stack.pop()?;
            if el.name != local(r.get(..end)?.trim()) {
                return None;
            }
            match stack.last_mut() {
                Some(p) => p.children.push(el),
                None => return Some(el),
            }
            pos += 2 + end + 1;
            continue;
        }
        let end = rest.find('>')?;
        let inner = rest.get(1..end)?;
        let self_closing = inner.ends_with('/');
        let inner = inner.trim_end_matches('/');
        let name_end = inner.find(char::is_whitespace).unwrap_or(inner.len());
        let mut node = Node { name: local(inner.get(..name_end)?), ..Default::default() };
        let mut a = inner.get(name_end..)?.trim();
        while !a.is_empty() {
            let eq = a.find('=')?;
            let key = local(a.get(..eq)?.trim());
            let v = a.get(eq + 1..)?.trim_start();
            let q = v.chars().next()?;
            let close = v.get(1..)?.find(q)?;
            node.attrs.push((key, unescape(v.get(1..1 + close)?)));
            a = v.get(close + 2..)?.trim_start();
        }
        pos += end + 1;
        if self_closing {
            match stack.last_mut() {
                Some(p) => p.children.push(node),
                None => return Some(node),
            }
        } else {
            if stack.len() > 40 {
                return None;
            }
            stack.push(node);
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------------
// NTLM (server side) — MD4 (RFC 1320), MD5 (RFC 1321), HMAC-MD5 (RFC 2104), NTLMv2 ([MS-NLMP])
// ---------------------------------------------------------------------------------------------------

fn md_pad(data: &[u8]) -> Vec<u8> {
    let bits = (data.len() as u64).wrapping_mul(8);
    let mut m = data.to_vec();
    m.push(0x80);
    while m.len() % 64 != 56 {
        m.push(0);
    }
    m.extend_from_slice(&bits.to_le_bytes());
    m
}

fn md_words(b: &[u8]) -> Vec<u32> {
    b.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect()
}

fn md_out(h: [u32; 4]) -> [u8; 16] {
    let mut o = [0u8; 16];
    for (d, w) in o.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *d = w.to_le_bytes();
    }
    o
}

fn md4(data: &[u8]) -> [u8; 16] {
    let mut h = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    let order: [[usize; 16]; 3] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15],
        [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15],
    ];
    let shifts = [[3u32, 7, 11, 19], [3, 5, 9, 13], [3, 9, 11, 15]];
    for block in md_pad(data).as_chunks::<64>().0 {
        let x = md_words(block);
        let mut v = h;
        for (round, (ord, sh)) in order.iter().zip(shifts).enumerate() {
            for (j, k) in ord.iter().enumerate() {
                let [a, b, c, d] = v;
                let f = match round {
                    0 => (b & c) | (!b & d),
                    1 => ((b & c) | (b & d) | (c & d)).wrapping_add(0x5a82_7999),
                    _ => (b ^ c ^ d).wrapping_add(0x6ed9_eba1),
                };
                let s = sh.get(j % 4).copied().unwrap_or(0);
                let t = a.wrapping_add(f).wrapping_add(x.get(*k).copied().unwrap_or(0)).rotate_left(s);
                v = [d, t, b, c];
            }
        }
        for (hw, vw) in h.iter_mut().zip(v) {
            *hw = hw.wrapping_add(vw);
        }
    }
    md_out(h)
}

fn md5(data: &[u8]) -> [u8; 16] {
    let s: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let mut h = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for block in md_pad(data).as_chunks::<64>().0 {
        let x = md_words(block);
        let mut v = h;
        for i in 0..64usize {
            let [a, b, c, d] = v;
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let k = ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32;
            let sh = s.get((i / 16) * 4 + i % 4).copied().unwrap_or(0);
            let t = a
                .wrapping_add(f)
                .wrapping_add(k)
                .wrapping_add(x.get(g).copied().unwrap_or(0))
                .rotate_left(sh);
            v = [d, b.wrapping_add(t), b, c];
        }
        for (hw, vw) in h.iter_mut().zip(v) {
            *hw = hw.wrapping_add(vw);
        }
    }
    md_out(h)
}

fn hmac_md5(key: &[u8], data: &[u8]) -> [u8; 16] {
    let mut k = [0u8; 64];
    let key: Vec<u8> = if key.len() > 64 { md5(key).to_vec() } else { key.to_vec() };
    for (d, s) in k.iter_mut().zip(&key) {
        *d = *s;
    }
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.extend_from_slice(data);
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.extend_from_slice(&md5(&inner));
    md5(&outer)
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn from_utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    String::from_utf16_lossy(&units)
}

fn sec_buf(b: &[u8], at: usize) -> Option<&[u8]> {
    let len = usize::from(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?));
    let off = usize::try_from(u32::from_le_bytes(b.get(at + 4..at + 8)?.try_into().ok()?)).ok()?;
    b.get(off..off.checked_add(len)?)
}

/// A CHALLENGE message for `challenge` (base64).
fn ntlm_challenge(challenge: &[u8; 8]) -> String {
    let mut info = Vec::new();
    for (id, v) in [(2u16, "TESTMAIL"), (1u16, "LOCALHOST")] {
        let v = utf16(v);
        info.extend_from_slice(&id.to_le_bytes());
        info.extend_from_slice(&(v.len() as u16).to_le_bytes());
        info.extend_from_slice(&v);
    }
    info.extend_from_slice(&[0; 4]);
    let mut m = b"NTLMSSP\0".to_vec();
    m.extend_from_slice(&2u32.to_le_bytes());
    m.extend_from_slice(&[0, 0, 0, 0, 48, 0, 0, 0]);
    m.extend_from_slice(&0xe28a_8235u32.to_le_bytes());
    m.extend_from_slice(challenge);
    m.extend_from_slice(&[0; 8]);
    let len = info.len() as u16;
    m.extend_from_slice(&len.to_le_bytes());
    m.extend_from_slice(&len.to_le_bytes());
    m.extend_from_slice(&48u32.to_le_bytes());
    m.extend_from_slice(&info);
    base64::engine::general_purpose::STANDARD.encode(m)
}

/// Checks an AUTHENTICATE message against `challenge`; the signed-in user.
fn ntlm_verify(store: &Store, challenge: &[u8; 8], b64: &str) -> Option<String> {
    let b = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    if b.get(..8)? != b"NTLMSSP\0" || u32::from_le_bytes(b.get(8..12)?.try_into().ok()?) != 3 {
        return None;
    }
    let nt = sec_buf(&b, 20)?;
    let domain = from_utf16(sec_buf(&b, 28)?);
    let user = from_utf16(sec_buf(&b, 36)?);
    let address = if user.contains('@') {
        user.clone()
    } else {
        // DOMAIN\user: the user whose address starts with `user@`.
        let prefix = format!("{}@", user.to_ascii_lowercase());
        store.user_addresses().into_iter().find(|a| a.to_ascii_lowercase().starts_with(&prefix))?
    };
    let password = store.user(&address)?.password.clone();
    let mut who = utf16(&user.to_uppercase());
    who.extend_from_slice(&utf16(&domain));
    let key = hmac_md5(&md4(&utf16(&password)), &who);
    let (proof, temp) = (nt.get(..16)?, nt.get(16..)?);
    let mut msg = challenge.to_vec();
    msg.extend_from_slice(temp);
    (hmac_md5(&key, &msg).as_slice() == proof)
        .then(|| store.user(&address).map(|u| u.address.clone()))
        .flatten()
}

// ---------------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------------

/// A contact, event or task as stored.
#[derive(Clone, Debug, Default)]
struct PimItem {
    folder: String,
    node: Node,
    rev: u64,
}

/// Automatic-reply settings of a user.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Oof {
    pub state: String,
    pub audience: String,
    pub start: String,
    pub end: String,
    pub internal: String,
    pub external: String,
}

/// EWS-only state shared by all connections.
#[derive(Default)]
pub struct EwsState {
    /// (user, folder id) → item snapshots (item id → change key, read).
    snaps: BTreeMap<(String, String), Vec<BTreeMap<String, (String, bool)>>>,
    folder_snaps: BTreeMap<String, Vec<BTreeMap<String, (String, String)>>>,
    /// user → item id → item.
    pim: BTreeMap<String, BTreeMap<String, PimItem>>,
    next: u64,
    /// (user, mailbox id, uid) → categories.
    categories: BTreeMap<(String, u64, u32), Vec<String>>,
    pub oof: BTreeMap<String, Oof>,
    /// Meeting answers: (user, element, reference id).
    pub replies: Vec<(String, String, String)>,
    /// Operation names, oldest first ("GetFolder", …), with the scheme that signed the request in.
    pub log: Vec<String>,
    /// Domains whose Autodiscover answers.
    pub autodiscover: Vec<String>,
    /// Messages stored with CreateItem larger than this are refused (ErrorMessageSizeExceeded).
    pub max_upload: Option<usize>,
}

pub type SharedEws = Arc<Mutex<EwsState>>;

fn lower(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

/// True for requests this module serves.
pub(crate) fn handles(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.starts_with("/ews/") || p.ends_with("/autodiscover/autodiscover.xml")
}

// ---------------------------------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------------------------------

fn write(conn: &mut Conn, status: &str, headers: &[(&str, String)], body: &[u8]) -> io::Result<()> {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut data = out.into_bytes();
    data.extend_from_slice(body);
    conn.write_all(&data)
}

fn xml_reply(conn: &mut Conn, status: &str, body: &str) -> io::Result<()> {
    write(conn, status, &[("Content-Type", "text/xml; charset=utf-8".into())], body.as_bytes())
}

/// Serves EWS and Autodiscover on `conn`, starting with `first` (already read), keeping the connection.
pub(crate) fn serve(mut conn: Conn, ctx: &Ctx, first: Request) {
    let max = ctx.config.max_message_size.saturating_mul(2).saturating_add(1024 * 1024);
    let mut req = Some(first);
    let mut signed_in: Option<String> = None;
    let mut challenge: Option<[u8; 8]> = None;
    let mut seed = DateTime::now().unix as u64;
    for _ in 0..MAX_REQUESTS {
        let Some(r) = req.take() else { break };
        let close = r.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
        if handle(&mut conn, ctx, &r, &mut signed_in, &mut challenge, &mut seed).is_err()
            || close
            || conn.stopping()
        {
            break;
        }
        req = match read_request(&mut conn, max) {
            Ok(Some(r)) => Some(r),
            _ => None,
        };
    }
    conn.close();
}

fn unauthorized(conn: &mut Conn, ctx: &Ctx, extra: Option<String>) -> io::Result<()> {
    let mut h: Vec<(&str, String)> = Vec::new();
    match extra {
        Some(c) => h.push(("WWW-Authenticate", format!("NTLM {c}"))),
        None => {
            if !ctx.config.ews_ntlm_only {
                h.push(("WWW-Authenticate", "Basic realm=\"testmail\"".into()));
            }
            h.push(("WWW-Authenticate", "NTLM".into()));
        }
    }
    h.push(("Content-Type", "text/plain".into()));
    write(conn, "401 Unauthorized", &h, b"sign-in required")
}

fn handle(
    conn: &mut Conn,
    ctx: &Ctx,
    req: &Request,
    signed_in: &mut Option<String>,
    challenge: &mut Option<[u8; 8]>,
    seed: &mut u64,
) -> io::Result<()> {
    if req.path.to_ascii_lowercase().ends_with("/autodiscover/autodiscover.xml") {
        // A domain without Autodiscover has no such location at all (whoever asks).
        let body = String::from_utf8_lossy(&req.body);
        let address = parse(&body)
            .and_then(|d| d.find("EMailAddress").map(|e| e.text.trim().to_owned()))
            .unwrap_or_default();
        let domain = address.rsplit_once('@').map(|(_, d)| lower(d)).unwrap_or_default();
        if !ctx.ews.lock().unwrap_or_else(|e| e.into_inner()).autodiscover.contains(&domain) {
            return write(conn, "404 Not Found", &[("Content-Type", "text/plain".into())], b"not found");
        }
    }
    let auth = req.header("authorization").unwrap_or_default().to_owned();
    if req.path.to_ascii_lowercase().ends_with("/autodiscover/autodiscover.xml") {
        // Which requests carried credentials (a scheme word only, never the secret).
        let how = auth.split(' ').next().filter(|w| !w.is_empty()).unwrap_or("none");
        ctx.ews.lock().unwrap_or_else(|e| e.into_inner()).log.push(format!("autodiscover auth={how}"));
    }
    let mut scheme = "conn";
    if let Some(token) = auth.strip_prefix("NTLM ").or_else(|| auth.strip_prefix("Negotiate ")) {
        let raw = base64::engine::general_purpose::STANDARD.decode(token.trim()).unwrap_or_default();
        match raw.get(8..12).and_then(|t| <[u8; 4]>::try_from(t).ok()).map(u32::from_le_bytes) {
            Some(1) => {
                *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                let c = seed.to_le_bytes();
                *challenge = Some(c);
                *signed_in = None;
                return unauthorized(conn, ctx, Some(ntlm_challenge(&c)));
            }
            Some(3) => {
                let user = challenge.take().and_then(|c| ntlm_verify(&ctx.shared.lock(), &c, token));
                match user {
                    Some(u) => *signed_in = Some(u),
                    None => return unauthorized(conn, ctx, None),
                }
                scheme = "ntlm";
            }
            _ => return unauthorized(conn, ctx, None),
        }
    } else if !auth.is_empty() {
        let is_basic = auth.starts_with("Basic ");
        if is_basic && ctx.config.ews_ntlm_only {
            return unauthorized(conn, ctx, None);
        }
        *signed_in = crate::jmap::authenticate(ctx, req);
        scheme = if is_basic { "basic" } else { "bearer" };
    }
    let Some(user) = signed_in.clone() else { return unauthorized(conn, ctx, None) };
    let path = req.path.split('?').next().unwrap_or_default().to_ascii_lowercase();
    let body = String::from_utf8_lossy(&req.body).into_owned();
    let scheme_host = format!(
        "{}://{}",
        if conn.is_tls() { "https" } else { "http" },
        req.header("host").unwrap_or("127.0.0.1")
    );
    if path.ends_with("/autodiscover/autodiscover.xml") {
        return autodiscover(conn, ctx, &user, &body, &scheme_host);
    }
    let Some(doc) = parse(&body) else {
        return xml_reply(conn, "500 Internal Server Error", &fault("the request is not XML"));
    };
    let Some(op) = doc.child("Body").and_then(|b| b.children.first()) else {
        return xml_reply(conn, "500 Internal Server Error", &fault("no operation"));
    };
    ctx.ews.lock().unwrap_or_else(|e| e.into_inner()).log.push(format!("{} {scheme}", op.name));
    let answer = operation(ctx, &user, op);
    let status = if answer.starts_with("<soap:Fault") { "500 Internal Server Error" } else { "200 OK" };
    xml_reply(conn, status, &envelope(&answer))
}

fn envelope(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><soap:Header><t:ServerVersionInfo MajorVersion="14" MinorVersion="3"/></soap:Header><soap:Body>{inner}</soap:Body></soap:Envelope>"#
    )
}

fn fault(why: &str) -> String {
    format!(
        "<soap:Fault><faultcode>soap:Client</faultcode><faultstring>{}</faultstring><detail><t:ResponseCode>ErrorInvalidRequest</t:ResponseCode></detail></soap:Fault>",
        esc(why)
    )
}

fn autodiscover(conn: &mut Conn, ctx: &Ctx, user: &str, body: &str, base: &str) -> io::Result<()> {
    let address = parse(body)
        .and_then(|d| d.find("EMailAddress").map(|e| e.text.trim().to_owned()))
        .unwrap_or_default();
    let domain = address.rsplit_once('@').map(|(_, d)| lower(d)).unwrap_or_default();
    let enabled = ctx.ews.lock().unwrap_or_else(|e| e.into_inner()).autodiscover.contains(&domain);
    if !enabled {
        return write(conn, "404 Not Found", &[("Content-Type", "text/plain".into())], b"not found");
    }
    let known = ctx.shared.lock().user(&address).is_some() && lower(&address) == lower(user);
    let response = if known {
        format!(
            r#"<Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a"><User><DisplayName>{}</DisplayName></User><Account><AccountType>email</AccountType><Action>settings</Action><Protocol><Type>EXCH</Type><EwsUrl>{base}/EWS/Exchange.asmx</EwsUrl></Protocol><Protocol><Type>EXPR</Type><EwsUrl>{base}/EWS/Exchange.asmx</EwsUrl></Protocol></Account></Response>"#,
            esc(&address)
        )
    } else {
        r#"<Response><Error><ErrorCode>500</ErrorCode><Message>The e-mail address cannot be found.</Message></Error></Response>"#.to_owned()
    };
    let doc = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">{response}</Autodiscover>"#
    );
    xml_reply(conn, "200 OK", &doc)
}

// ---------------------------------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------------------------------

fn ok_msg(op: &str, inner: &str) -> String {
    format!(
        r#"<m:{op}ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>{inner}</m:{op}ResponseMessage>"#
    )
}

fn err_msg(op: &str, code: &str, text: &str) -> String {
    format!(
        r#"<m:{op}ResponseMessage ResponseClass="Error"><m:MessageText>{}</m:MessageText><m:ResponseCode>{code}</m:ResponseCode></m:{op}ResponseMessage>"#,
        esc(text)
    )
}

fn wrap(op: &str, msgs: &[String]) -> String {
    format!("<m:{op}Response><m:ResponseMessages>{}</m:ResponseMessages></m:{op}Response>", msgs.concat())
}

fn mailbox_id(mb: &crate::store::Mailbox) -> String {
    format!("F{:x}", mb.id)
}

/// The mailbox name of folder id `fid` (a distinguished id or `F<hex>`).
fn mailbox_name(store: &Store, user: &str, fid: &str) -> Option<String> {
    let u = store.user(user)?;
    let special = |s: SpecialUse| u.mailboxes.iter().find(|m| m.special == Some(s)).map(|m| m.name.clone());
    match fid {
        "inbox" => Some("INBOX".into()),
        "sentitems" => special(SpecialUse::Sent),
        "drafts" => special(SpecialUse::Drafts),
        "deleteditems" => special(SpecialUse::Trash),
        "junkemail" => special(SpecialUse::Junk),
        "archive" => special(SpecialUse::Archive),
        _ => u.mailboxes.iter().find(|m| mailbox_id(m) == fid).map(|m| m.name.clone()),
    }
}

/// The folder id a `FolderId` / `DistinguishedFolderId` element names, resolved to `F…` ids.
fn folder_of(store: &Store, user: &str, n: &Node) -> Option<String> {
    let id = n.attr("Id")?;
    match id {
        "msgfolderroot" | "root" => Some(ROOT.into()),
        "calendar" => Some("Fcal".into()),
        "contacts" => Some("Fcon".into()),
        "tasks" => Some("Ftask".into()),
        x if x == ROOT || PIM_FOLDERS.iter().any(|(f, ..)| *f == x) => Some(x.to_owned()),
        x => {
            let name = mailbox_name(store, user, x)?;
            store.user(user)?.mailbox(&name).map(mailbox_id)
        }
    }
}

fn folder_refs(n: &Node) -> Vec<&Node> {
    n.children.iter().filter(|c| c.name == "FolderId" || c.name == "DistinguishedFolderId").collect()
}

/// The folder element of folder id `fid`.
fn folder_xml(store: &Store, user: &str, fid: &str) -> Option<String> {
    if fid == ROOT {
        return Some(format!(
            r#"<t:Folder><t:FolderId Id="{ROOT}" ChangeKey="K"/><t:DisplayName>Top of Information Store</t:DisplayName><t:FolderClass>IPF.Note</t:FolderClass><t:TotalCount>0</t:TotalCount></t:Folder>"#
        ));
    }
    if let Some((id, el, name, class)) = PIM_FOLDERS.iter().find(|(f, ..)| *f == fid) {
        return Some(format!(
            r#"<t:{el}><t:FolderId Id="{id}" ChangeKey="K"/><t:ParentFolderId Id="{ROOT}"/><t:DisplayName>{name}</t:DisplayName><t:FolderClass>{class}</t:FolderClass></t:{el}>"#
        ));
    }
    let u = store.user(user)?;
    let mb = u.mailboxes.iter().find(|m| mailbox_id(m) == fid)?;
    let (parent, leaf) = match mb.name.rsplit_once('/') {
        Some((p, l)) => (u.mailbox(p).map(mailbox_id).unwrap_or_else(|| ROOT.into()), l.to_owned()),
        None => (ROOT.to_owned(), if mb.name == "INBOX" { "Inbox".to_owned() } else { mb.name.clone() }),
    };
    let prefix = format!("{}/", mb.name);
    let children = u
        .mailboxes
        .iter()
        .filter(|m| m.name.strip_prefix(&prefix).is_some_and(|rest| !rest.is_empty() && !rest.contains('/')))
        .count();
    Some(format!(
        r#"<t:Folder><t:FolderId Id="{fid}" ChangeKey="K{}"/><t:ParentFolderId Id="{parent}"/><t:FolderClass>IPF.Note</t:FolderClass><t:DisplayName>{}</t:DisplayName><t:TotalCount>{}</t:TotalCount><t:ChildFolderCount>{children}</t:ChildFolderCount><t:UnreadCount>{}</t:UnreadCount></t:Folder>"#,
        mb.highest_modseq,
        esc(&leaf),
        mb.messages.len(),
        mb.unseen()
    ))
}

fn all_folder_ids(store: &Store, user: &str) -> Vec<String> {
    let mut ids: Vec<String> =
        store.user(user).map(|u| u.mailboxes.iter().map(mailbox_id).collect()).unwrap_or_default();
    ids.extend(PIM_FOLDERS.iter().map(|(f, ..)| (*f).to_owned()));
    ids
}

fn iso(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (y, m, d) = crate::date::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, (secs / 60) % 60, secs % 60)
}

/// Unix time of an xs:dateTime (`Z`, an offset, or none = UTC; fractions ignored).
fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once('T').unwrap_or((s, "00:00:00Z"));
    let mut d = date.split('-');
    let (y, mo, da): (i64, u32, u32) =
        (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let (clock, offset) = if let Some(c) = time.strip_suffix('Z') {
        (c, 0i64)
    } else if let Some(i) = time.rfind(['+', '-']).filter(|i| *i > 5) {
        let (c, z) = time.split_at(i);
        let sign = if z.starts_with('-') { -1 } else { 1 };
        let (h, m) = z.get(1..)?.split_once(':')?;
        (c, sign * (h.parse::<i64>().ok()? * 60 + m.parse::<i64>().ok()?))
    } else {
        (time, 0)
    };
    let mut t = clock.split('.').next()?.split(':');
    let (h, mi, se): (i64, i64, i64) =
        (t.next()?.parse().ok()?, t.next()?.parse().ok()?, t.next().unwrap_or("0").parse().ok()?);
    Some(crate::date::days_from_civil(y, mo, da) * 86_400 + h * 3600 + mi * 60 + se - offset * 60)
}

fn message_xml(
    ewsst: &EwsState,
    user: &str,
    mb: &crate::store::Mailbox,
    m: &crate::store::Msg,
    with_mime: bool,
) -> String {
    let id = format!("M{:x}.{}", mb.id, m.uid);
    let cats = ewsst.categories.get(&(lower(user), mb.id, m.uid)).cloned().unwrap_or_default();
    let cats_xml = if cats.is_empty() {
        String::new()
    } else {
        format!(
            "<t:Categories>{}</t:Categories>",
            cats.iter().map(|c| format!("<t:String>{}</t:String>", esc(c))).collect::<String>()
        )
    };
    let mime = if with_mime {
        format!(
            r#"<t:MimeContent CharacterSet="UTF-8">{}</t:MimeContent>"#,
            base64::engine::general_purpose::STANDARD.encode(&*m.raw)
        )
    } else {
        String::new()
    };
    let flag = if m.has_flag("\\Flagged") { 2 } else { 0 };
    let verb = if m.has_flag("\\Answered") {
        Some(102)
    } else if m.has_flag("$Forwarded") {
        Some(104)
    } else {
        None
    };
    let verb_xml = verb
        .map(|v| format!(r#"<t:ExtendedProperty><t:ExtendedFieldURI PropertyTag="0x1081" PropertyType="Integer"/><t:Value>{v}</t:Value></t:ExtendedProperty>"#))
        .unwrap_or_default();
    let mid = hdr(&m.raw, "message-id").unwrap_or_default();
    format!(
        r#"<t:Message><t:ItemId Id="{id}" ChangeKey="K{}"/><t:ParentFolderId Id="{}"/><t:ItemClass>IPM.Note</t:ItemClass>{mime}<t:DateTimeReceived>{}</t:DateTimeReceived><t:Size>{}</t:Size>{cats_xml}<t:IsRead>{}</t:IsRead><t:InternetMessageId>{}</t:InternetMessageId><t:ExtendedProperty><t:ExtendedFieldURI PropertyTag="0x1090" PropertyType="Integer"/><t:Value>{flag}</t:Value></t:ExtendedProperty>{verb_xml}</t:Message>"#,
        m.modseq,
        mailbox_id(mb),
        iso(m.internal_date.unix),
        m.raw.len(),
        m.has_flag("\\Seen"),
        esc(&mid)
    )
}

/// (mailbox name, uid) of message id `M<hex>.<uid>`.
fn locate_msg(store: &Store, user: &str, id: &str) -> Option<(String, u32)> {
    let (mbx, uid) = id.strip_prefix('M')?.split_once('.')?;
    let mbid = u64::from_str_radix(mbx, 16).ok()?;
    let uid: u32 = uid.parse().ok()?;
    let mb = store.user(user)?.mailboxes.iter().find(|m| m.id == mbid)?;
    mb.find(uid)?;
    Some((mb.name.clone(), uid))
}

fn item_ids(n: &Node) -> Vec<(String, Option<String>)> {
    n.children
        .iter()
        .filter(|c| c.name == "ItemId" || c.name == "ReferenceItemId")
        .filter_map(|c| Some((c.attr("Id")?.to_owned(), c.attr("ChangeKey").map(str::to_owned))))
        .collect()
}

fn operation(ctx: &Ctx, user: &str, op: &Node) -> String {
    match op.name.as_str() {
        "GetFolder" => get_folder(ctx, user, op),
        "FindFolder" => find_folder(ctx, user, op),
        "SyncFolderHierarchy" => sync_hierarchy(ctx, user, op),
        "CreateFolder" => create_folder(ctx, user, op),
        "UpdateFolder" | "MoveFolder" | "DeleteFolder" => change_folder(ctx, user, op),
        "SyncFolderItems" => sync_items(ctx, user, op),
        "GetItem" => get_item(ctx, user, op),
        "FindItem" => find_item(ctx, user, op),
        "CreateItem" => create_item(ctx, user, op),
        "UpdateItem" => update_item(ctx, user, op),
        "MoveItem" => move_item(ctx, user, op),
        "DeleteItem" => delete_item(ctx, user, op),
        "GetUserOofSettingsRequest" => get_oof(ctx, user),
        "SetUserOofSettingsRequest" => set_oof(ctx, user, op),
        "GetUserAvailabilityRequest" => availability(ctx, op),
        other => fault(&format!("{other} is not supported by the test server")),
    }
}

fn get_folder(ctx: &Ctx, user: &str, op: &Node) -> String {
    let store = ctx.shared.lock();
    let mut msgs = Vec::new();
    for r in op.child("FolderIds").map(folder_refs).unwrap_or_default() {
        match folder_of(&store, user, r).and_then(|f| folder_xml(&store, user, &f)) {
            Some(x) => msgs.push(ok_msg("GetFolder", &format!("<m:Folders>{x}</m:Folders>"))),
            None => msgs.push(err_msg(
                "GetFolder",
                "ErrorFolderNotFound",
                "The specified folder could not be found in the store.",
            )),
        }
    }
    wrap("GetFolder", &msgs)
}

fn page_view(op: &Node, view: &str) -> (usize, usize) {
    let v = op.child(view);
    let max = v.and_then(|v| v.attr("MaxEntriesReturned")).and_then(|s| s.parse().ok()).unwrap_or(1000usize);
    let off = v.and_then(|v| v.attr("Offset")).and_then(|s| s.parse().ok()).unwrap_or(0usize);
    (max.max(1), off)
}

fn find_folder(ctx: &Ctx, user: &str, op: &Node) -> String {
    let store = ctx.shared.lock();
    let (max, off) = page_view(op, "IndexedPageFolderView");
    let ids = all_folder_ids(&store, user);
    let page: Vec<String> =
        ids.iter().skip(off).take(max).filter_map(|f| folder_xml(&store, user, f)).collect();
    let last = off.saturating_add(max) >= ids.len();
    let next = off.saturating_add(page.len());
    wrap(
        "FindFolder",
        &[ok_msg(
            "FindFolder",
            &format!(
                r#"<m:RootFolder IndexedPagingOffset="{next}" TotalItemsInView="{}" IncludesLastItemInRange="{last}"><t:Folders>{}</t:Folders></m:RootFolder>"#,
                ids.len(),
                page.concat()
            ),
        )],
    )
}

fn folder_snapshot(store: &Store, user: &str) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    if let Some(u) = store.user(user) {
        for m in &u.mailboxes {
            let parent =
                m.name.rsplit_once('/').and_then(|(p, _)| u.mailbox(p)).map(mailbox_id).unwrap_or_default();
            out.insert(mailbox_id(m), (m.name.clone(), parent));
        }
    }
    out
}

fn sync_hierarchy(ctx: &Ctx, user: &str, op: &Node) -> String {
    let now = folder_snapshot(&ctx.shared.lock(), user);
    let mut st = ctx.ews.lock().unwrap_or_else(|e| e.into_inner());
    let snaps = st.folder_snaps.entry(lower(user)).or_default();
    let state = op.text_of("SyncState").unwrap_or_default();
    let before = match state.strip_prefix('H').and_then(|n| n.parse::<usize>().ok()) {
        Some(n) => match snaps.get(n) {
            Some(s) => s.clone(),
            None => {
                return wrap(
                    "SyncFolderHierarchy",
                    &[err_msg("SyncFolderHierarchy", "ErrorInvalidSyncStateData", "bad state")],
                );
            }
        },
        None if state.is_empty() => BTreeMap::new(),
        None => {
            return wrap(
                "SyncFolderHierarchy",
                &[err_msg("SyncFolderHierarchy", "ErrorInvalidSyncStateData", "bad state")],
            );
        }
    };
    let mut changes = String::new();
    for (id, v) in &now {
        match before.get(id) {
            None => changes
                .push_str(&format!(r#"<t:Create><t:Folder><t:FolderId Id="{id}"/></t:Folder></t:Create>"#)),
            Some(b) if b != v => changes
                .push_str(&format!(r#"<t:Update><t:Folder><t:FolderId Id="{id}"/></t:Folder></t:Update>"#)),
            Some(_) => {}
        }
    }
    for id in before.keys().filter(|k| !now.contains_key(*k)) {
        changes.push_str(&format!(r#"<t:Delete><t:FolderId Id="{id}"/></t:Delete>"#));
    }
    if snaps.len() >= MAX_SNAPSHOTS {
        snaps.remove(0);
    }
    snaps.push(now);
    let n = snaps.len() - 1;
    wrap(
        "SyncFolderHierarchy",
        &[ok_msg(
            "SyncFolderHierarchy",
            &format!(
                "<m:SyncState>H{n}</m:SyncState><m:IncludesLastFolderInRange>true</m:IncludesLastFolderInRange><m:Changes>{changes}</m:Changes>"
            ),
        )],
    )
}

fn create_folder(ctx: &Ctx, user: &str, op: &Node) -> String {
    let mut store = ctx.shared.lock();
    let parent = op
        .child("ParentFolderId")
        .and_then(|p| folder_refs(p).first().copied())
        .and_then(|r| folder_of(&store, user, r));
    let mut msgs = Vec::new();
    for f in op.child("Folders").map(|f| f.children.clone()).unwrap_or_default() {
        let name = f.text_of("DisplayName").unwrap_or_default().to_owned();
        let full = match parent.as_deref() {
            Some(ROOT) => Some(name.clone()),
            Some(p) => mailbox_name(&store, user, p).map(|pn| format!("{pn}/{name}")),
            None => None,
        };
        let made = full
            .filter(|_| !name.is_empty() && !name.contains('/'))
            .map(|full| (store.create_mailbox(user, &full, None), full));
        match made {
            Some((Ok(()), full)) => {
                let id = store.user(user).and_then(|u| u.mailbox(&full)).map(mailbox_id).unwrap_or_default();
                msgs.push(ok_msg(
                    "CreateFolder",
                    &format!(
                        r#"<m:Folders><t:Folder><t:FolderId Id="{id}" ChangeKey="K"/></t:Folder></m:Folders>"#
                    ),
                ));
            }
            Some((Err(crate::Error::MailboxExists(_)), _)) => msgs.push(err_msg(
                "CreateFolder",
                "ErrorFolderExists",
                "A folder with that name already exists.",
            )),
            _ => msgs.push(err_msg("CreateFolder", "ErrorInvalidRequest", "The folder cannot be created.")),
        }
    }
    drop(store);
    ctx.shared.notify();
    wrap("CreateFolder", &msgs)
}

fn change_folder(ctx: &Ctx, user: &str, op: &Node) -> String {
    let name = op.name.clone();
    let mut store = ctx.shared.lock();
    let mut msgs = Vec::new();
    let leaf = |n: &str| n.rsplit_once('/').map_or(n, |(_, l)| l).to_owned();
    let parent_of = |n: &str| n.rsplit_once('/').map(|(p, _)| p.to_owned());
    match name.as_str() {
        "UpdateFolder" => {
            for ch in op.child("FolderChanges").map(|c| c.children.clone()).unwrap_or_default() {
                let fid = folder_refs(&ch).first().and_then(|r| folder_of(&store, user, r));
                let new = ch.find("DisplayName").map(|d| d.text.trim().to_owned()).unwrap_or_default();
                let old = fid.as_deref().and_then(|f| mailbox_name(&store, user, f));
                let r = match old {
                    Some(o) if !new.is_empty() && !new.contains('/') => {
                        let to = match parent_of(&o) {
                            Some(p) => format!("{p}/{new}"),
                            None => new,
                        };
                        store.rename_mailbox(user, &o, &to).is_ok()
                    }
                    _ => false,
                };
                msgs.push(if r {
                    ok_msg("UpdateFolder", "")
                } else {
                    err_msg("UpdateFolder", "ErrorFolderNotFound", "not renamed")
                });
            }
        }
        "MoveFolder" => {
            let to = op
                .child("ToFolderId")
                .and_then(|t| folder_refs(t).first().copied())
                .and_then(|r| folder_of(&store, user, r));
            for r in op.child("FolderIds").map(folder_refs).unwrap_or_default() {
                let old = folder_of(&store, user, r).and_then(|f| mailbox_name(&store, user, &f));
                let dest = match to.as_deref() {
                    Some(ROOT) => Some(String::new()),
                    Some(t) => mailbox_name(&store, user, t),
                    None => None,
                };
                let ok = match (old, dest) {
                    (Some(o), Some(d)) => {
                        let target = if d.is_empty() { leaf(&o) } else { format!("{d}/{}", leaf(&o)) };
                        store.rename_mailbox(user, &o, &target).is_ok()
                    }
                    _ => false,
                };
                msgs.push(if ok {
                    ok_msg("MoveFolder", "")
                } else {
                    err_msg("MoveFolder", "ErrorFolderNotFound", "not moved")
                });
            }
        }
        _ => {
            for r in op.child("FolderIds").map(folder_refs).unwrap_or_default() {
                let old = folder_of(&store, user, r).and_then(|f| mailbox_name(&store, user, &f));
                let ok = match old {
                    Some(o) => {
                        let names: Vec<String> = store
                            .mailbox_names(user)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|n| *n == o || n.starts_with(&format!("{o}/")))
                            .collect();
                        names.iter().rev().all(|n| store.delete_mailbox(user, n).is_ok())
                    }
                    None => false,
                };
                msgs.push(if ok {
                    ok_msg("DeleteFolder", "")
                } else {
                    err_msg("DeleteFolder", "ErrorFolderNotFound", "not deleted")
                });
            }
        }
    }
    drop(store);
    ctx.shared.notify();
    wrap(&name, &msgs)
}

fn sync_items(ctx: &Ctx, user: &str, op: &Node) -> String {
    let store = ctx.shared.lock();
    let Some(fid) = op
        .child("SyncFolderId")
        .and_then(|s| folder_refs(s).first().copied())
        .and_then(|r| folder_of(&store, user, r))
    else {
        return wrap(
            "SyncFolderItems",
            &[err_msg("SyncFolderItems", "ErrorFolderNotFound", "no such folder")],
        );
    };
    let max = op.text_of("MaxChangesReturned").and_then(|s| s.parse().ok()).unwrap_or(512usize).max(1);
    let mut st = ctx.ews.lock().unwrap_or_else(|e| e.into_inner());
    // The folder's items now: mail of its mailbox.
    let mut now: BTreeMap<String, (String, bool)> = BTreeMap::new();
    let mut xml_of: BTreeMap<String, String> = BTreeMap::new();
    if let Some(name) = mailbox_name(&store, user, &fid)
        && let Some(mb) = store.user(user).and_then(|u| u.mailbox(&name))
    {
        for m in &mb.messages {
            let id = format!("M{:x}.{}", mb.id, m.uid);
            let cats = st.categories.get(&(lower(user), mb.id, m.uid)).cloned().unwrap_or_default();
            let mut flags: Vec<String> = m
                .flags
                .iter()
                .filter(|f| !f.eq_ignore_ascii_case("\\Seen"))
                .map(|f| f.to_ascii_lowercase())
                .collect();
            flags.sort();
            now.insert(id.clone(), (format!("{}|{}", flags.join(" "), cats.join(",")), m.has_flag("\\Seen")));
            xml_of.insert(id, message_xml(&st, user, mb, m, false));
        }
    }
    let key = (lower(user), fid.clone());
    let state = op.text_of("SyncState").unwrap_or_default().to_owned();
    let before = match state.strip_prefix('S').and_then(|n| n.parse::<usize>().ok()) {
        Some(n) => match st.snaps.get(&key).and_then(|s| s.get(n)) {
            Some(s) => s.clone(),
            None => {
                return wrap(
                    "SyncFolderItems",
                    &[err_msg("SyncFolderItems", "ErrorInvalidSyncStateData", "unknown sync state")],
                );
            }
        },
        None if state.is_empty() => BTreeMap::new(),
        None => {
            return wrap(
                "SyncFolderItems",
                &[err_msg("SyncFolderItems", "ErrorInvalidSyncStateData", "bad sync state")],
            );
        }
    };
    let mut changes = Vec::new();
    let mut next = before.clone();
    for (id, v) in &now {
        if changes.len() >= max {
            break;
        }
        match before.get(id) {
            None => {
                changes.push(format!("<t:Create>{}</t:Create>", xml_of.get(id).cloned().unwrap_or_default()))
            }
            Some(b) if b.0 != v.0 => {
                changes.push(format!("<t:Update>{}</t:Update>", xml_of.get(id).cloned().unwrap_or_default()))
            }
            Some(b) if b.1 != v.1 => changes.push(format!(
                r#"<t:ReadFlagChange><t:ItemId Id="{id}"/><t:IsRead>{}</t:IsRead></t:ReadFlagChange>"#,
                v.1
            )),
            Some(_) => continue,
        }
        next.insert(id.clone(), v.clone());
    }
    for id in before.keys().filter(|k| !now.contains_key(*k)) {
        if changes.len() >= max {
            break;
        }
        changes.push(format!(r#"<t:Delete><t:ItemId Id="{id}"/></t:Delete>"#));
        next.remove(id);
    }
    let last = next == now;
    let snaps = st.snaps.entry(key).or_default();
    if snaps.len() >= MAX_SNAPSHOTS {
        // Old states stop working, as a server's do when they expire.
        snaps.clear();
    }
    snaps.push(next);
    let n = snaps.len() - 1;
    wrap(
        "SyncFolderItems",
        &[ok_msg(
            "SyncFolderItems",
            &format!(
                "<m:SyncState>S{n}</m:SyncState><m:IncludesLastItemInRange>{last}</m:IncludesLastItemInRange><m:Changes>{}</m:Changes>",
                changes.concat()
            ),
        )],
    )
}

/// An item element with its id and folder first.
fn pim_xml(id: &str, it: &PimItem) -> String {
    let mut n = it.node.clone();
    n.children.retain(|c| c.name != "ItemId" && c.name != "ParentFolderId");
    n.children.insert(
        0,
        Node {
            name: "ParentFolderId".into(),
            attrs: vec![("Id".into(), it.folder.clone())],
            ..Default::default()
        },
    );
    n.children.insert(
        0,
        Node {
            name: "ItemId".into(),
            attrs: vec![("Id".into(), id.to_owned()), ("ChangeKey".into(), format!("K{}", it.rev))],
            ..Default::default()
        },
    );
    n.to_xml()
}

fn get_item(ctx: &Ctx, user: &str, op: &Node) -> String {
    let with_mime = op
        .find("AdditionalProperties")
        .is_some_and(|a| a.all("FieldURI").any(|f| f.attr("FieldURI") == Some("item:MimeContent")))
        || op.find("IncludeMimeContent").is_some_and(|i| i.text.trim() == "true");
    let store = ctx.shared.lock();
    let st = ctx.ews.lock().unwrap_or_else(|e| e.into_inner());
    let mut msgs = Vec::new();
    for (id, _) in op.child("ItemIds").map(item_ids).unwrap_or_default() {
        if let Some((name, uid)) = locate_msg(&store, user, &id)
            && let Some(mb) = store.user(user).and_then(|u| u.mailbox(&name))
            && let Some(m) = mb.find(uid)
        {
            msgs.push(ok_msg(
                "GetItem",
                &format!("<m:Items>{}</m:Items>", message_xml(&st, user, mb, m, with_mime)),
            ));
        } else if let Some(it) = st.pim.get(&lower(user)).and_then(|p| p.get(&id)) {
            msgs.push(ok_msg("GetItem", &format!("<m:Items>{}</m:Items>", pim_xml(&id, it))));
        } else {
            msgs.push(err_msg(
                "GetItem",
                "ErrorItemNotFound",
                "The specified object was not found in the store.",
            ));
        }
    }
    wrap("GetItem", &msgs)
}

fn find_item(ctx: &Ctx, user: &str, op: &Node) -> String {
    let store = ctx.shared.lock();
    let st = ctx.ews.lock().unwrap_or_else(|e| e.into_inner());
    let fid = op
        .child("ParentFolderIds")
        .and_then(|p| folder_refs(p).first().copied())
        .and_then(|r| folder_of(&store, user, r));
    let Some(fid) = fid else {
        return wrap("FindItem", &[err_msg("FindItem", "ErrorFolderNotFound", "no such folder")]);
    };
    let mut all: Vec<String> = Vec::new();
    if let Some(p) = st.pim.get(&lower(user)) {
        for (id, it) in p.iter().filter(|(_, it)| it.folder == fid) {
            let kind = it.node.text_of("CalendarItemType").unwrap_or_default();
            if kind == "Exception" || kind == "Occurrence" {
                continue;
            }
            all.push(pim_xml(id, it));
        }
    }
    if let Some(name) = mailbox_name(&store, user, &fid)
        && let Some(mb) = store.user(user).and_then(|u| u.mailbox(&name))
    {
        all.extend(mb.messages.iter().map(|m| message_xml(&st, user, mb, m, false)));
    }
    let (max, off) = page_view(op, "IndexedPageItemView");
    let page: Vec<String> = all.iter().skip(off).take(max).cloned().collect();
    let last = off.saturating_add(max) >= all.len();
    wrap(
        "FindItem",
        &[ok_msg(
            "FindItem",
            &format!(
                r#"<m:RootFolder IndexedPagingOffset="{}" TotalItemsInView="{}" IncludesLastItemInRange="{last}"><t:Items>{}</t:Items></m:RootFolder>"#,
                off.saturating_add(page.len()),
                all.len(),
                page.concat()
            ),
        )],
    )
}

/// A header field of a message source (unfolded).
fn hdr(raw: &[u8], name: &str) -> Option<String> {
    let (head, _) = crate::mime::split_message(raw);
    crate::mime::header_value(head, name).map(|v| String::from_utf8_lossy(&v).into_owned())
}

/// Recipients a message source names (To, Cc, Bcc headers).
fn recipients(raw: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for h in ["to", "cc", "bcc"] {
        if let Some(v) = hdr(raw, h) {
            for part in v.split(',') {
                let a = match (part.find('<'), part.find('>')) {
                    (Some(s), Some(e)) if e > s => part.get(s + 1..e).unwrap_or_default(),
                    _ => part,
                };
                let a = a.trim();
                if a.contains('@') && !out.iter().any(|x: &String| x.eq_ignore_ascii_case(a)) {
                    out.push(a.to_owned());
                }
            }
        }
    }
    out
}

fn create_item(ctx: &Ctx, user: &str, op: &Node) -> String {
    let disposition = op.attr("MessageDisposition").unwrap_or("SaveOnly").to_owned();
    let items = op.child("Items").map(|i| i.children.clone()).unwrap_or_default();
    let mut msgs = Vec::new();
    for item in items {
        if matches!(item.name.as_str(), "AcceptItem" | "TentativelyAcceptItem" | "DeclineItem") {
            let reference =
                item.child("ReferenceItemId").and_then(|r| r.attr("Id")).unwrap_or_default().to_owned();
            ctx.ews.lock().unwrap_or_else(|e| e.into_inner()).replies.push((
                lower(user),
                item.name.clone(),
                reference,
            ));
            if disposition == "SaveOnly" {
                // Like Exchange: the answer not sent is kept as a draft response message in Drafts.
                let raw = format!(
                    "From: {user}\r\nSubject: {}: meeting\r\nDate: {}\r\nMIME-Version: 1.0\r\n\
                     Content-Type: text/plain; charset=utf-8\r\n\r\nResponse not sent.\r\n",
                    item.name.trim_end_matches("Item"),
                    DateTime::now().to_rfc5322()
                );
                let mut store = ctx.shared.lock();
                let made = store.mbox_mut(user, "Drafts").map(|mb| {
                    (
                        mb.id,
                        append_msg(mb, Arc::from(raw.as_bytes()), vec!["\\Draft".into()], DateTime::now()).1,
                    )
                });
                drop(store);
                ctx.shared.notify();
                if let Some((mbid, uid)) = made {
                    msgs.push(ok_msg(
                        "CreateItem",
                        &format!(
                            r#"<m:Items><t:{0}><t:ItemId Id="M{mbid:x}.{uid}" ChangeKey="K"/></t:{0}></m:Items>"#,
                            item.name
                        ),
                    ));
                    continue;
                }
            }
            msgs.push(ok_msg("CreateItem", "<m:Items/>"));
            continue;
        }
        if item.name == "Message" {
            let mime = item.child("MimeContent").and_then(|m| {
                base64::engine::general_purpose::STANDARD
                    .decode(m.text.trim().replace(['\r', '\n', ' '], ""))
                    .ok()
            });
            let Some(raw) = mime else {
                msgs.push(err_msg("CreateItem", "ErrorInvalidRequest", "a message needs MimeContent here"));
                continue;
            };
            let now = DateTime::now();
            let limit = ctx.ews.lock().unwrap_or_else(|e| e.into_inner()).max_upload;
            if disposition == "SaveOnly" && limit.is_some_and(|n| raw.len() > n) {
                msgs.push(err_msg("CreateItem", "ErrorMessageSizeExceeded", "the message is too large"));
                continue;
            }
            if disposition == "SaveOnly" {
                let mut store = ctx.shared.lock();
                let fid = op
                    .child("SavedItemFolderId")
                    .and_then(|s| folder_refs(s).first().copied())
                    .and_then(|r| folder_of(&store, user, r));
                let name =
                    fid.and_then(|f| mailbox_name(&store, user, &f)).unwrap_or_else(|| "Drafts".into());
                let flags = item
                    .all("ExtendedProperty")
                    .find(|p| {
                        p.child("ExtendedFieldURI").and_then(|u| u.attr("PropertyTag")) == Some("0x0E07")
                    })
                    .and_then(|p| p.text_of("Value")?.parse::<i64>().ok())
                    .unwrap_or(8);
                // Like some servers, the read state comes from IsRead only (the read bit of the message flags
                // is ignored).
                let read = item.text_of("IsRead").is_some_and(|r| r.trim() == "true");
                let mut f = Vec::new();
                if read {
                    f.push("\\Seen".to_owned());
                }
                if flags & 8 != 0 {
                    f.push("\\Draft".to_owned());
                }
                let made = store
                    .mbox_mut(user, &name)
                    .map(|mb| (mb.id, append_msg(mb, Arc::from(raw.as_slice()), f, now).1));
                drop(store);
                ctx.shared.notify();
                match made {
                    Some((mbid, uid)) => msgs.push(ok_msg("CreateItem", &format!(r#"<m:Items><t:Message><t:ItemId Id="M{mbid:x}.{uid}" ChangeKey="K"/></t:Message></m:Items>"#))),
                    None => msgs.push(err_msg("CreateItem", "ErrorFolderNotFound", "no such folder")),
                }
                continue;
            }
            // Sending: delivered to local users, recorded like an SMTP submission.
            let mut rcpts = recipients(&raw);
            if let Some(b) = item.child("BccRecipients") {
                for a in b.all("Mailbox").filter_map(|m| m.text_of("EmailAddress")) {
                    if !rcpts.iter().any(|x| x.eq_ignore_ascii_case(a)) {
                        rcpts.push(a.to_owned());
                    }
                }
            }
            let mut st = ctx.shared.lock();
            let mut delivered = Vec::new();
            for r in &rcpts {
                if let Some(mb) = st.user_mut(r).and_then(|u| u.mailbox_mut("INBOX")) {
                    append_msg(mb, Arc::from(raw.as_slice()), Vec::new(), now);
                    delivered.push(r.clone());
                }
            }
            if disposition == "SendAndSaveCopy" {
                let sent = st.user(user).and_then(|u| {
                    u.mailboxes.iter().find(|m| m.special == Some(SpecialUse::Sent)).map(|m| m.name.clone())
                });
                if let Some(mb) = sent.and_then(|n| st.mbox_mut(user, &n)) {
                    append_msg(mb, Arc::from(raw.as_slice()), vec!["\\Seen".into()], now);
                }
            }
            st.submissions.push(Submission {
                auth_user: Some(user.to_owned()),
                mail_from: user.to_owned(),
                rcpt_to: rcpts,
                data: raw,
                delivered_to: delivered,
            });
            drop(st);
            ctx.shared.notify();
            msgs.push(ok_msg("CreateItem", "<m:Items/>"));
            continue;
        }
        // Contacts, events, tasks: kept as sent.
        let store = ctx.shared.lock();
        let fid = op
            .child("SavedItemFolderId")
            .and_then(|s| folder_refs(s).first().copied())
            .and_then(|r| folder_of(&store, user, r));
        drop(store);
        let fid = fid.unwrap_or_else(|| match item.name.as_str() {
            "Contact" => "Fcon".into(),
            "Task" => "Ftask".into(),
            _ => "Fcal".into(),
        });
        let mut st = ctx.ews.lock().unwrap_or_else(|e| e.into_inner());
        st.next += 1;
        let id = format!("P{}", st.next);
        st.pim
            .entry(lower(user))
            .or_default()
            .insert(id.clone(), PimItem { folder: fid, node: item.clone(), rev: 1 });
        msgs.push(ok_msg(
            "CreateItem",
            &format!(
                r#"<m:Items><t:{}><t:ItemId Id="{id}" ChangeKey="K1"/></t:{}></m:Items>"#,
                item.name, item.name
            ),
        ));
    }
    wrap("CreateItem", &msgs)
}

/// The element names a FieldURI value maps to (`calendar:Start` → `Start`).
fn field_name(uri: &str) -> String {
    uri.rsplit(':').next().unwrap_or(uri).to_owned()
}

fn same_ext(a: &Node, b: &Node) -> bool {
    let mut x = a.attrs.clone();
    let mut y = b.attrs.clone();
    x.sort();
    y.sort();
    x == y
}

/// Applies one SetItemField / DeleteItemField to a stored item.
fn apply_field(node: &mut Node, change: &Node) {
    let set = change.name == "SetItemField";
    // The value element (inside the item element of a SetItemField).
    let value = change
        .children
        .iter()
        .find(|c| !c.name.ends_with("FieldURI"))
        .and_then(|item| item.children.first())
        .cloned();
    if let Some(uri) = change.child("ExtendedFieldURI") {
        node.children.retain(|c| {
            !(c.name == "ExtendedProperty" && c.child("ExtendedFieldURI").is_some_and(|u| same_ext(u, uri)))
        });
        if set && let Some(v) = value {
            node.children.push(v);
        }
        return;
    }
    if let Some(uri) = change.child("IndexedFieldURI") {
        let path = uri.attr("FieldURI").unwrap_or_default();
        let key = uri.attr("FieldIndex").unwrap_or_default().to_owned();
        // contacts:EmailAddress → EmailAddresses; contacts:PhysicalAddress:Street → PhysicalAddresses/Street.
        let mut parts = path.split(':').skip(1);
        let base = parts.next().unwrap_or_default();
        let sub = parts.next().map(str::to_owned);
        let container = match base {
            "EmailAddress" => "EmailAddresses",
            "PhoneNumber" => "PhoneNumbers",
            "PhysicalAddress" => "PhysicalAddresses",
            "ImAddress" => "ImAddresses",
            other => other,
        }
        .to_owned();
        if node.child(&container).is_none() {
            node.children.push(Node { name: container.clone(), ..Default::default() });
        }
        let Some(c) = node.children.iter_mut().find(|c| c.name == container) else { return };
        let new_entry = value.and_then(|v| v.child("Entry").cloned());
        match (&sub, set) {
            (None, true) => {
                c.children.retain(|e| e.attr("Key") != Some(key.as_str()));
                c.children.extend(new_entry);
            }
            (None, false) => c.children.retain(|e| e.attr("Key") != Some(key.as_str())),
            (Some(s), _) => {
                if !c.children.iter().any(|e| e.attr("Key") == Some(key.as_str())) {
                    c.children.push(Node {
                        name: "Entry".into(),
                        attrs: vec![("Key".into(), key.clone())],
                        ..Default::default()
                    });
                }
                if let Some(e) = c.children.iter_mut().find(|e| e.attr("Key") == Some(key.as_str())) {
                    e.children.retain(|x| x.name != *s);
                    if set && let Some(v) = new_entry.and_then(|n| n.child(s).cloned()) {
                        e.children.push(v);
                    }
                }
            }
        }
        return;
    }
    let Some(uri) = change.child("FieldURI").and_then(|u| u.attr("FieldURI")) else { return };
    let name = field_name(uri);
    node.children.retain(|c| c.name != name);
    if set && let Some(v) = value {
        node.children.push(v);
    }
}

fn update_item(ctx: &Ctx, user: &str, op: &Node) -> String {
    let conflict = op.attr("ConflictResolution").unwrap_or("AutoResolve").to_owned();
    let mut msgs = Vec::new();
    for ch in op.child("ItemChanges").map(|c| c.children.clone()).unwrap_or_default() {
        let Some((id, ck)) = item_ids(&ch).into_iter().next() else { continue };
        let updates = ch.child("Updates").map(|u| u.children.clone()).unwrap_or_default();
        // Mail: read state, flag, categories.
        let mut store = ctx.shared.lock();
        if let Some((name, uid)) = locate_msg(&store, user, &id) {
            let mut st = ctx.ews.lock().unwrap_or_else(|e| e.into_inner());
            let Some(mb) = store.mbox_mut(user, &name) else { continue };
            let mbid = mb.id;
            let modseq = mb.next_modseq();
            let mut cats: Option<Vec<String>> = None;
            if let Some(m) = mb.find_mut(uid) {
                for u in &updates {
                    let set = u.name == "SetItemField";
                    let uri = u.child("FieldURI").and_then(|f| f.attr("FieldURI")).unwrap_or_default();
                    let tag =
                        u.child("ExtendedFieldURI").and_then(|f| f.attr("PropertyTag")).unwrap_or_default();
                    let value = u.find("Value").map(|v| v.text.trim().to_owned()).unwrap_or_default();
                    let mut flag = |name: &str, on: bool| {
                        m.flags.retain(|f| !f.eq_ignore_ascii_case(name));
                        if on {
                            m.flags.push(name.to_owned());
                        }
                    };
                    match (uri, tag) {
                        ("message:IsRead", _) => {
                            flag("\\Seen", set && u.find("IsRead").is_some_and(|r| r.text.trim() == "true"))
                        }
                        ("item:Categories", _) => {
                            cats = Some(if set {
                                u.find("Categories")
                                    .map(|c| c.all("String").map(|s| s.text.trim().to_owned()).collect())
                                    .unwrap_or_default()
                            } else {
                                Vec::new()
                            });
                        }
                        (_, "0x1090") => flag("\\Flagged", set && value == "2"),
                        (_, "0x1081") => {
                            flag("\\Answered", set && (value == "102" || value == "103"));
                            flag("$Forwarded", set && value == "104");
                        }
                        _ => {}
                    }
                }
                m.modseq = modseq;
            }
            if let Some(c) = cats {
                st.categories.insert((lower(user), mbid, uid), c);
            }
            drop(st);
            drop(store);
            ctx.shared.notify();
            msgs.push(ok_msg(
                "UpdateItem",
                &format!(
                    r#"<m:Items><t:Message><t:ItemId Id="{id}" ChangeKey="K{modseq}"/></t:Message></m:Items>"#
                ),
            ));
            continue;
        }
        drop(store);
        let mut st = ctx.ews.lock().unwrap_or_else(|e| e.into_inner());
        let Some(it) = st.pim.get_mut(&lower(user)).and_then(|p| p.get_mut(&id)) else {
            msgs.push(err_msg(
                "UpdateItem",
                "ErrorItemNotFound",
                "The specified object was not found in the store.",
            ));
            continue;
        };
        if conflict == "NeverOverwrite" && ck.as_deref().is_some_and(|c| *c != format!("K{}", it.rev)) {
            msgs.push(err_msg(
                "UpdateItem",
                "ErrorIrresolvableConflict",
                "The item changed since it was read.",
            ));
            continue;
        }
        for u in &updates {
            apply_field(&mut it.node, u);
        }
        it.rev += 1;
        let kind = it.node.name.clone();
        msgs.push(ok_msg(
            "UpdateItem",
            &format!(
                r#"<m:Items><t:{kind}><t:ItemId Id="{id}" ChangeKey="K{}"/></t:{kind}></m:Items>"#,
                it.rev
            ),
        ));
    }
    wrap("UpdateItem", &msgs)
}

fn move_item(ctx: &Ctx, user: &str, op: &Node) -> String {
    let mut store = ctx.shared.lock();
    let to = op
        .child("ToFolderId")
        .and_then(|t| folder_refs(t).first().copied())
        .and_then(|r| folder_of(&store, user, r));
    let to_name = to.as_deref().and_then(|t| mailbox_name(&store, user, t));
    let mut st = ctx.ews.lock().unwrap_or_else(|e| e.into_inner());
    let mut msgs = Vec::new();
    for (id, _) in op.child("ItemIds").map(item_ids).unwrap_or_default() {
        let found = locate_msg(&store, user, &id);
        let (Some((from, uid)), Some(dest)) = (found, to_name.clone()) else {
            msgs.push(err_msg(
                "MoveItem",
                "ErrorItemNotFound",
                "The specified object was not found in the store.",
            ));
            continue;
        };
        let Some(src) = store.mbox_mut(user, &from) else { continue };
        let src_id = src.id;
        let Some(pos) = src.messages.iter().position(|m| m.uid == uid) else { continue };
        let m = src.messages.remove(pos);
        src.next_modseq();
        let cats = st.categories.remove(&(lower(user), src_id, uid));
        match store.mbox_mut(user, &dest) {
            Some(dst) => {
                let dst_id = dst.id;
                let (_, nuid) = append_msg(dst, m.raw.clone(), m.flags.clone(), m.internal_date);
                if let Some(c) = cats {
                    st.categories.insert((lower(user), dst_id, nuid), c);
                }
                msgs.push(ok_msg("MoveItem", &format!(r#"<m:Items><t:Message><t:ItemId Id="M{dst_id:x}.{nuid}" ChangeKey="K"/></t:Message></m:Items>"#)));
            }
            None => msgs.push(err_msg("MoveItem", "ErrorFolderNotFound", "no such folder")),
        }
    }
    drop(st);
    drop(store);
    ctx.shared.notify();
    wrap("MoveItem", &msgs)
}

fn delete_item(ctx: &Ctx, user: &str, op: &Node) -> String {
    let hard = op.attr("DeleteType") != Some("MoveToDeletedItems");
    let mut store = ctx.shared.lock();
    let mut st = ctx.ews.lock().unwrap_or_else(|e| e.into_inner());
    let trash = store.user(user).and_then(|u| {
        u.mailboxes.iter().find(|m| m.special == Some(SpecialUse::Trash)).map(|m| m.name.clone())
    });
    let mut msgs = Vec::new();
    for (id, _) in op.child("ItemIds").map(item_ids).unwrap_or_default() {
        if let Some((from, uid)) = locate_msg(&store, user, &id) {
            let taken = store.mbox_mut(user, &from).and_then(|mb| {
                let pos = mb.messages.iter().position(|m| m.uid == uid)?;
                mb.next_modseq();
                Some(mb.messages.remove(pos))
            });
            if let (Some(m), false, Some(t)) = (taken, hard, trash.as_deref())
                && let Some(dst) = store.mbox_mut(user, t)
            {
                append_msg(dst, m.raw.clone(), m.flags.clone(), m.internal_date);
            }
            msgs.push(ok_msg("DeleteItem", ""));
        } else if st.pim.get_mut(&lower(user)).and_then(|p| p.remove(&id)).is_some() {
            msgs.push(ok_msg("DeleteItem", ""));
        } else {
            msgs.push(err_msg(
                "DeleteItem",
                "ErrorItemNotFound",
                "The specified object was not found in the store.",
            ));
        }
    }
    drop(st);
    drop(store);
    ctx.shared.notify();
    wrap("DeleteItem", &msgs)
}

fn get_oof(ctx: &Ctx, user: &str) -> String {
    let o =
        ctx.ews.lock().unwrap_or_else(|e| e.into_inner()).oof.get(&lower(user)).cloned().unwrap_or_default();
    let state = if o.state.is_empty() { "Disabled" } else { o.state.as_str() };
    let audience = if o.audience.is_empty() { "None" } else { o.audience.as_str() };
    let duration = if o.start.is_empty() {
        String::new()
    } else {
        format!(
            "<t:Duration><t:StartTime>{}</t:StartTime><t:EndTime>{}</t:EndTime></t:Duration>",
            esc(&o.start),
            esc(&o.end)
        )
    };
    format!(
        r#"<m:GetUserOofSettingsResponse><m:ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:ResponseMessage><t:OofSettings><t:OofState>{state}</t:OofState><t:ExternalAudience>{audience}</t:ExternalAudience>{duration}<t:InternalReply><t:Message>{}</t:Message></t:InternalReply><t:ExternalReply><t:Message>{}</t:Message></t:ExternalReply></t:OofSettings><m:AllowExternalOof>All</m:AllowExternalOof></m:GetUserOofSettingsResponse>"#,
        esc(&o.internal),
        esc(&o.external)
    )
}

fn set_oof(ctx: &Ctx, user: &str, op: &Node) -> String {
    let Some(s) = op.child("UserOofSettings") else {
        return r#"<m:SetUserOofSettingsResponse><m:ResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorInvalidRequest</m:ResponseCode></m:ResponseMessage></m:SetUserOofSettingsResponse>"#.into();
    };
    let reply = |n: &str| s.child(n).and_then(|r| r.text_of("Message")).unwrap_or_default().to_owned();
    let o = Oof {
        state: s.text_of("OofState").unwrap_or("Disabled").to_owned(),
        audience: s.text_of("ExternalAudience").unwrap_or("None").to_owned(),
        start: s.child("Duration").and_then(|d| d.text_of("StartTime")).unwrap_or_default().to_owned(),
        end: s.child("Duration").and_then(|d| d.text_of("EndTime")).unwrap_or_default().to_owned(),
        internal: reply("InternalReply"),
        external: reply("ExternalReply"),
    };
    ctx.ews.lock().unwrap_or_else(|e| e.into_inner()).oof.insert(lower(user), o);
    r#"<m:SetUserOofSettingsResponse><m:ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:ResponseMessage></m:SetUserOofSettingsResponse>"#.into()
}

fn availability(ctx: &Ctx, op: &Node) -> String {
    let window = op.find("TimeWindow");
    let start = window.and_then(|w| w.text_of("StartTime")).and_then(parse_iso).unwrap_or(0);
    let end = window.and_then(|w| w.text_of("EndTime")).and_then(parse_iso).unwrap_or(0);
    let store = ctx.shared.lock();
    let st = ctx.ews.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = String::new();
    for mbx in op.child("MailboxDataArray").map(|a| a.children.clone()).unwrap_or_default() {
        let addr = mbx.find("Address").map(|a| a.text.trim().to_owned()).unwrap_or_default();
        if store.user(&addr).is_none() {
            out.push_str(r#"<m:FreeBusyResponse><m:ResponseMessage ResponseClass="Error"><m:MessageText>No mailbox with that address.</m:MessageText><m:ResponseCode>ErrorMailRecipientNotFound</m:ResponseCode></m:ResponseMessage></m:FreeBusyResponse>"#);
            continue;
        }
        let mut events = String::new();
        for it in st.pim.get(&lower(&addr)).map(|p| p.values().collect::<Vec<_>>()).unwrap_or_default() {
            if it.folder != "Fcal" {
                continue;
            }
            let (Some(s), Some(e)) =
                (it.node.text_of("Start").and_then(parse_iso), it.node.text_of("End").and_then(parse_iso))
            else {
                continue;
            };
            if e <= start || s >= end {
                continue;
            }
            let busy = it.node.text_of("LegacyFreeBusyStatus").unwrap_or("Busy");
            if busy == "Free" {
                continue;
            }
            events.push_str(&format!(
                "<t:CalendarEvent><t:StartTime>{}</t:StartTime><t:EndTime>{}</t:EndTime><t:BusyType>{}</t:BusyType></t:CalendarEvent>",
                iso(s).trim_end_matches('Z'),
                iso(e).trim_end_matches('Z'),
                esc(busy)
            ));
        }
        out.push_str(&format!(
            r#"<m:FreeBusyResponse><m:ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:ResponseMessage><m:FreeBusyView><t:FreeBusyViewType>FreeBusy</t:FreeBusyViewType><t:CalendarEventArray>{events}</t:CalendarEventArray></m:FreeBusyView></m:FreeBusyResponse>"#
        ));
    }
    format!(
        "<m:GetUserAvailabilityResponse><m:FreeBusyResponseArray>{out}</m:FreeBusyResponseArray></m:GetUserAvailabilityResponse>"
    )
}

// ---------------------------------------------------------------------------------------------------
// Test API helpers
// ---------------------------------------------------------------------------------------------------

/// Stores an item (XML of a `CalendarItem`, `Contact` or `Task` element) in folder `folder` (`calendar`,
/// `contacts`, `tasks`); its id.
pub fn put_item(st: &mut EwsState, user: &str, folder: &str, xml: &str) -> Option<String> {
    let node = parse(xml)?;
    let fid = match folder {
        "contacts" | "Fcon" => "Fcon",
        "tasks" | "Ftask" => "Ftask",
        _ => "Fcal",
    };
    st.next += 1;
    let id = format!("P{}", st.next);
    st.pim.entry(lower(user)).or_default().insert(id.clone(), PimItem { folder: fid.into(), node, rev: 1 });
    Some(id)
}

/// The items of `folder` as XML, by id.
pub fn items(st: &EwsState, user: &str, folder: &str) -> Vec<(String, String)> {
    let fid = match folder {
        "contacts" | "Fcon" => "Fcon",
        "tasks" | "Ftask" => "Ftask",
        _ => "Fcal",
    };
    st.pim
        .get(&lower(user))
        .map(|p| {
            p.iter()
                .filter(|(_, it)| it.folder == fid)
                .map(|(id, it)| (id.clone(), pim_xml(id, it)))
                .collect()
        })
        .unwrap_or_default()
}

/// Changes a stored item's field `name` (a direct child) to `text`; false when there is no such item.
pub fn set_item_field(st: &mut EwsState, user: &str, id: &str, name: &str, text: &str) -> bool {
    let Some(it) = st.pim.get_mut(&lower(user)).and_then(|p| p.get_mut(id)) else { return false };
    it.node.children.retain(|c| c.name != name);
    it.node.children.push(Node { name: name.into(), text: text.into(), ..Default::default() });
    it.rev += 1;
    true
}

pub fn remove_item(st: &mut EwsState, user: &str, id: &str) -> bool {
    st.pim.get_mut(&lower(user)).and_then(|p| p.remove(id)).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_round_trip_and_ntlm_hashes() {
        let n = parse(
            r#"<?xml version="1.0"?><a:Root xmlns:a="x"><a:B Id="1 &amp; 2">t &lt; u</a:B><C/></a:Root>"#,
        )
        .unwrap();
        assert_eq!(n.name, "Root");
        assert_eq!(n.child("B").unwrap().attr("Id"), Some("1 & 2"));
        assert_eq!(n.text_of("B"), Some("t < u"));
        assert!(n.to_xml().contains(r#"<t:B Id="1 &amp; 2">t &lt; u</t:B>"#));
        assert!(parse("<a><b></a>").is_none());
        assert!(parse("<!DOCTYPE x><a/>").is_none());
        assert_eq!(
            md4(b"abc").iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "a448017aaf21d8525fc10ae87aa6729d"
        );
        assert_eq!(
            hmac_md5(&[0x0b; 16], b"Hi There").iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "9294727a3638bb1c13f48ef8158bfc9d"
        );
        assert_eq!(parse_iso("2026-10-05T10:00:00Z"), Some(1_791_194_400));
        assert_eq!(parse_iso("2026-10-05T06:00:00-04:00"), Some(1_791_194_400));
        assert_eq!(iso(1_791_194_400), "2026-10-05T10:00:00Z");
    }

    #[test]
    fn fields_are_set_and_deleted() {
        let mut item = parse("<Contact><GivenName>Pat</GivenName><EmailAddresses><Entry Key=\"EmailAddress1\">a@example.com</Entry></EmailAddresses></Contact>").unwrap();
        apply_field(&mut item, &parse(r#"<SetItemField><FieldURI FieldURI="contacts:GivenName"/><Contact><GivenName>Sam</GivenName></Contact></SetItemField>"#).unwrap());
        apply_field(&mut item, &parse(r#"<SetItemField><IndexedFieldURI FieldURI="contacts:EmailAddress" FieldIndex="EmailAddress1"/><Contact><EmailAddresses><Entry Key="EmailAddress1">b@example.com</Entry></EmailAddresses></Contact></SetItemField>"#).unwrap());
        apply_field(&mut item, &parse(r#"<SetItemField><IndexedFieldURI FieldURI="contacts:PhysicalAddress:City" FieldIndex="Home"/><Contact><PhysicalAddresses><Entry Key="Home"><City>Springfield</City></Entry></PhysicalAddresses></Contact></SetItemField>"#).unwrap());
        assert_eq!(item.text_of("GivenName"), Some("Sam"));
        assert_eq!(item.child("EmailAddresses").unwrap().children[0].text, "b@example.com");
        assert_eq!(item.find("City").unwrap().text, "Springfield");
        apply_field(&mut item, &parse(r#"<DeleteItemField><IndexedFieldURI FieldURI="contacts:EmailAddress" FieldIndex="EmailAddress1"/></DeleteItemField>"#).unwrap());
        apply_field(
            &mut item,
            &parse(r#"<DeleteItemField><FieldURI FieldURI="contacts:GivenName"/></DeleteItemField>"#)
                .unwrap(),
        );
        assert!(
            item.child("GivenName").is_none() && item.child("EmailAddresses").unwrap().children.is_empty()
        );
    }
}
