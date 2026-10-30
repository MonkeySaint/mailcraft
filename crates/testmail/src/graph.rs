//! A fake Microsoft Graph mail service (REST v1.0 shapes from the public API reference) on the web server's
//! ports, under `/graph/v1.0`, over the same in-memory store as IMAP, POP3 and JMAP:
//!
//! - `GET /me`; mail folders: `GET /me/mailFolders` (paged with `$top`/`$skip`), `GET /me/mailFolders/{id or
//!   well-known name}`, `…/childFolders`, `POST` (create), `PATCH` (rename), `POST …/move`, `DELETE`;
//! - per-folder message delta queries (`…/messages/delta`, with `$filter=receivedDateTime ge …`), paged with
//!   `Prefer: odata.maxpagesize` (at most [`crate::Config::graph_max_page`]) through `@odata.nextLink`, ending
//!   with an `@odata.deltaLink`; an unknown delta token answers 410 `SyncStateNotFound`;
//! - messages: `GET`, `GET …/$value` (the MIME source), `PATCH` (isRead, flag, categories, importance),
//!   `POST …/move`, `POST …/permanentDelete`, `DELETE` (to Deleted Items); `POST /me/messages` and
//!   `POST /me/mailFolders/{id}/messages` from base64 MIME (filed as drafts); `POST /me/sendMail` (base64 MIME
//!   or JSON) and `POST /me/messages/{id}/send`, delivered like the SMTP server and filed in Sent;
//!   attachments `POST …/attachments` and upload sessions (`…/attachments/createUploadSession`, then `PUT`
//!   pieces with `Content-Range` to `/graph-upload/{session}` without a token);
//! - throttling on demand ([`crate::TestServers::graph_throttle`]): the next requests answer 429 with a
//!   `Retry-After`; upload sessions refused on demand ([`crate::TestServers::graph_refuse_uploads`]).
//!
//! Sign-in: a Bearer token from the OAuth test server, or the user's password as the token (test
//! convenience). Message ids are immutable: a hash of the source and internal date when first seen, kept
//! when the message moves or gets attachments. Folder ids are the store's stable mailbox ids.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{Value, json};

use crate::conn::Conn;
use crate::date::DateTime;
use crate::jmap::{Request, respond};
use crate::server::Ctx;
use crate::store::{SpecialUse, Store, Submission, append_msg};

/// Path prefix of the API.
pub const PREFIX: &str = "/graph/v1.0";
/// Path prefix of upload sessions.
pub const UPLOAD_PREFIX: &str = "/graph-upload/";
/// The id of the folder above the top-level folders.
const ROOT: &str = "gfroot";
/// Delta snapshots kept per folder.
const MAX_SNAPSHOTS: usize = 50;
/// Pending delta pages kept.
const MAX_PENDING: usize = 200;
/// Requests kept in the log.
const MAX_LOG: usize = 5000;

/// What Graph keeps about a message beyond the IMAP flags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Meta {
    categories: Vec<String>,
    /// `low` / `normal` / `high` set through the API (else read from the source).
    importance: Option<String>,
    /// The flag is complete (`\Flagged` stands for flagged).
    complete: bool,
    /// A flagged message's start and due day (`dateTime` as set, UTC).
    start: Option<String>,
    due: Option<String>,
    /// Focused Inbox: classified as other.
    other: bool,
}

/// An open upload session.
#[derive(Clone, Debug)]
struct Upload {
    user: String,
    message: String,
    name: String,
    ctype: String,
    size: usize,
    inline: bool,
    cid: Option<String>,
    data: Vec<u8>,
}

/// One delta result waiting to be read page by page.
#[derive(Clone, Debug)]
struct Pending {
    user: String,
    folder: String,
    entries: Vec<Value>,
    token: u64,
}

/// Graph-only state shared by all connections.
#[derive(Debug, Default)]
pub struct GraphState {
    /// (user, current source hash) → the message's id.
    alias: BTreeMap<(String, u64), String>,
    meta: BTreeMap<(String, String), Meta>,
    snaps: BTreeMap<(String, String), Vec<(u64, BTreeMap<String, String>)>>,
    next_token: u64,
    pending: BTreeMap<u64, Pending>,
    uploads: BTreeMap<String, Upload>,
    next_upload: u64,
    /// The next this many requests answer 429.
    pub throttle: u32,
    /// `Retry-After` of those answers (seconds).
    pub retry_after: u64,
    /// Upload sessions are refused (as for a file over the service's limit).
    pub refuse_uploads: bool,
    pub requests: Vec<String>,
}

pub type SharedGraph = Arc<Mutex<GraphState>>;

/// True for paths this module serves.
pub fn handles(path: &str) -> bool {
    path == PREFIX || path.starts_with(&format!("{PREFIX}/")) || path.starts_with(UPLOAD_PREFIX)
}

fn user_key(u: &str) -> String {
    u.trim().to_ascii_lowercase()
}

fn fnv(data: &[u8], date: &DateTime) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data.iter().chain(date.unix.to_string().as_bytes()) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl GraphState {
    fn id_of(&self, user: &str, raw: &[u8], date: &DateTime) -> String {
        let h = fnv(raw, date);
        self.alias.get(&(user_key(user), h)).cloned().unwrap_or_else(|| format!("gm{h:016x}"))
    }

    /// The message's id stays `id` although its source becomes `raw`.
    fn keep_id(&mut self, user: &str, id: &str, raw: &[u8], date: &DateTime) {
        self.alias.insert((user_key(user), fnv(raw, date)), id.to_owned());
    }

    fn meta(&self, user: &str, id: &str) -> Meta {
        self.meta.get(&(user_key(user), id.to_owned())).cloned().unwrap_or_default()
    }

    fn meta_mut(&mut self, user: &str, id: &str) -> &mut Meta {
        self.meta.entry((user_key(user), id.to_owned())).or_default()
    }
}

/// A folder's id.
fn folder_id(mb: &crate::store::Mailbox) -> String {
    format!("gf{:x}", mb.id)
}

/// The store mailbox a folder id or well-known name stands for (`None`: no such folder); `Some("")` is the
/// root above the top-level folders.
fn resolve_folder(store: &Store, user: &str, r: &str) -> Option<String> {
    let u = store.user(user)?;
    let special = |s: SpecialUse| u.mailboxes.iter().find(|m| m.special == Some(s)).map(|m| m.name.clone());
    match r.to_ascii_lowercase().as_str() {
        "msgfolderroot" | ROOT => return Some(String::new()),
        "inbox" => return Some("INBOX".into()),
        "drafts" => return special(SpecialUse::Drafts),
        "sentitems" => return special(SpecialUse::Sent),
        "deleteditems" => return special(SpecialUse::Trash),
        "junkemail" => return special(SpecialUse::Junk),
        "archive" => return special(SpecialUse::Archive),
        _ => {}
    }
    u.mailboxes.iter().find(|m| folder_id(m) == r).map(|m| m.name.clone())
}

fn parent_name(name: &str) -> Option<&str> {
    name.rsplit_once('/').map(|(p, _)| p)
}

fn leaf(name: &str) -> &str {
    name.rsplit_once('/').map_or(name, |(_, l)| l)
}

fn folder_json(u: &crate::store::User, mb: &crate::store::Mailbox) -> Value {
    let parent = parent_name(&mb.name).and_then(|p| u.mailbox(p)).map_or(ROOT.to_owned(), folder_id);
    let prefix = format!("{}/", mb.name);
    let children = u
        .mailboxes
        .iter()
        .filter(|m| m.name.strip_prefix(&prefix).is_some_and(|rest| !rest.contains('/')))
        .count();
    let display = if mb.name == "INBOX" { "Inbox".to_owned() } else { leaf(&mb.name).to_owned() };
    json!({
        "id": folder_id(mb),
        "displayName": display,
        "parentFolderId": parent,
        "childFolderCount": children,
        "totalItemCount": mb.messages.len(),
        "unreadItemCount": mb.unseen(),
    })
}

/// ISO 8601 UTC (`2026-10-05T10:00:00Z`).
fn iso(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (y, m, d) = crate::date::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff]Z` (UTC).
fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T').unwrap_or((s, "00:00:00"));
    let mut d = date.split('-');
    let (y, mo, da) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let time = time.split('.').next().unwrap_or("00:00:00");
    let mut t = time.split(':');
    let (h, mi, se) = (
        t.next()?.parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
    );
    DateTime::from_civil(y, mo, da, h, mi, se, 0).map(|d| d.unix)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        if c == b'%'
            && let Some(h) = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok())
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if c == b'+' { b' ' } else { c });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query(q: &str) -> BTreeMap<String, String> {
    q.split('&')
        .filter_map(|kv| kv.split_once('=').or(Some((kv, ""))))
        .filter(|(k, _)| !k.is_empty())
        .map(|(k, v)| (percent_decode(k), percent_decode(v)))
        .collect()
}

fn header(raw: &[u8], name: &str) -> String {
    let (h, _) = crate::mime::split_message(raw);
    crate::mime::header_value(h, name).map(|v| String::from_utf8_lossy(&v).into_owned()).unwrap_or_default()
}

fn addresses(raw: &[u8], name: &str) -> Vec<Value> {
    let (h, _) = crate::mime::split_message(raw);
    let Some(v) = crate::mime::header_value(h, name) else { return Vec::new() };
    crate::mime::parse_addresses(&v)
        .into_iter()
        .filter_map(|a| match a {
            crate::mime::Addr::Mailbox { name, local, host, .. } => {
                let address =
                    format!("{}@{}", String::from_utf8_lossy(&local), String::from_utf8_lossy(&host));
                let name = name.map(|n| String::from_utf8_lossy(&n).into_owned()).unwrap_or_default();
                Some(json!({"emailAddress": {"name": name, "address": address}}))
            }
            _ => None,
        })
        .collect()
}

fn importance_of(raw: &[u8]) -> &'static str {
    let imp = header(raw, "Importance").to_ascii_lowercase();
    let pri = header(raw, "X-Priority");
    if imp == "high" || pri.starts_with('1') || pri.starts_with('2') {
        "high"
    } else if imp == "low" || pri.starts_with('4') || pri.starts_with('5') {
        "low"
    } else {
        "normal"
    }
}

fn message_json(g: &GraphState, user: &str, folder: &str, m: &crate::store::Msg) -> Value {
    let id = g.id_of(user, &m.raw, &m.internal_date);
    let meta = g.meta(user, &id);
    let flagged = m.has_flag("\\Flagged");
    let flag = if flagged {
        "flagged"
    } else if meta.complete {
        "complete"
    } else {
        "notFlagged"
    };
    let mut flag_v = json!({"flagStatus": flag});
    if flagged {
        for (key, d) in [("startDateTime", &meta.start), ("dueDateTime", &meta.due)] {
            if let Some(d) = d {
                put(&mut flag_v, key, json!({"dateTime": format!("{d}.0000000"), "timeZone": "UTC"}));
            }
        }
    }
    let (_, body) = crate::mime::split_message(&m.raw);
    let preview: String = String::from_utf8_lossy(body).split_whitespace().collect::<Vec<_>>().join(" ");
    let first = |name: &str| addresses(&m.raw, name).into_iter().next().unwrap_or(Value::Null);
    let sent = crate::date::DateTime::parse_rfc5322(&header(&m.raw, "Date")).map(|d| iso(d.unix));
    let lower = String::from_utf8_lossy(&m.raw).to_ascii_lowercase();
    json!({
        "id": id,
        "subject": header(&m.raw, "Subject"),
        "from": first("From"),
        "sender": first("Sender"),
        "toRecipients": addresses(&m.raw, "To"),
        "ccRecipients": addresses(&m.raw, "Cc"),
        "bccRecipients": addresses(&m.raw, "Bcc"),
        "replyTo": addresses(&m.raw, "Reply-To"),
        "receivedDateTime": iso(m.internal_date.unix),
        "sentDateTime": sent,
        "isRead": m.has_flag("\\Seen"),
        "isDraft": m.has_flag("\\Draft"),
        "flag": flag_v,
        "importance": meta.importance.clone().unwrap_or_else(|| importance_of(&m.raw).to_owned()),
        "categories": meta.categories,
        "parentFolderId": folder,
        "internetMessageId": header(&m.raw, "Message-ID"),
        "hasAttachments": lower.contains("content-disposition: attachment"),
        "bodyPreview": preview.chars().take(255).collect::<String>(),
        "inferenceClassification": if meta.other { "other" } else { "focused" },
        "conversationId": format!("c{:x}", fnv(header(&m.raw, "Subject").as_bytes(), &DateTime::from_unix(0))),
    })
}

/// What a delta query compares for a message.
fn fingerprint(v: &Value) -> String {
    let g = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    let flag = v.get("flag").cloned().unwrap_or(Value::Null);
    format!("{}|{}|{}|{}|{}", g("isRead"), g("isDraft"), flag, g("categories"), g("importance"))
}

/// Sets `key` of the JSON object `v`.
fn put(v: &mut Value, key: &str, x: Value) {
    if let Some(m) = v.as_object_mut() {
        m.insert(key.to_owned(), x);
    }
}

/// Where message `id` of `user` is: (mailbox name, uid).
fn locate(store: &Store, g: &GraphState, user: &str, id: &str) -> Option<(String, u32)> {
    let u = store.user(user)?;
    u.mailboxes.iter().find_map(|mb| {
        mb.messages
            .iter()
            .find(|m| g.id_of(user, &m.raw, &m.internal_date) == id)
            .map(|m| (mb.name.clone(), m.uid))
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "200 OK",
        201 => "201 Created",
        202 => "202 Accepted",
        204 => "204 No Content",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        404 => "404 Not Found",
        409 => "409 Conflict",
        410 => "410 Gone",
        416 => "416 Range Not Satisfiable",
        429 => "429 Too Many Requests",
        _ => "500 Internal Server Error",
    }
}

fn send_json(conn: &mut Conn, status: u16, v: &Value) -> io::Result<()> {
    if status == 204 {
        return conn.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
    respond(conn, reason(status), "application/json", v.to_string().as_bytes())
}

fn err(status: u16, code: &str, message: &str) -> (u16, Value) {
    (status, json!({"error": {"code": code, "message": message}}))
}

/// The base64 MIME of a request body (whitespace ignored).
fn mime_body(req: &Request) -> Option<Vec<u8>> {
    let text: Vec<u8> = req.body.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(text).ok().filter(|r| !r.is_empty())
}

/// A message built from Graph JSON (`subject`, `body`, recipients) as `from`.
fn mime_from_json(from: &str, m: &Value) -> Vec<u8> {
    let list = |k: &str| {
        m.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|r| r.get("emailAddress")?.get("address")?.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    let body = m.get("body");
    let html = body.and_then(|b| b.get("contentType")).and_then(Value::as_str) == Some("html");
    let content = body.and_then(|b| b.get("content")).and_then(Value::as_str).unwrap_or("");
    let now = DateTime::now();
    let mut out = format!(
        "From: {from}\r\nTo: {}\r\nSubject: {}\r\nDate: {}\r\nMessage-ID: <g{}@graph.test>\r\nMIME-Version: 1.0\r\n",
        list("toRecipients"),
        m.get("subject").and_then(Value::as_str).unwrap_or(""),
        now.to_rfc5322(),
        now.unix
    );
    let cc = list("ccRecipients");
    if !cc.is_empty() {
        out.push_str(&format!("Cc: {cc}\r\n"));
    }
    out.push_str(if html {
        "Content-Type: text/html; charset=utf-8\r\n\r\n"
    } else {
        "Content-Type: text/plain; charset=utf-8\r\n\r\n"
    });
    out.push_str(content);
    out.push_str("\r\n");
    out.into_bytes()
}

/// `raw` with a file attached (a multipart/mixed message gets one more part; any other is wrapped).
fn with_attachment(
    raw: &[u8],
    name: &str,
    ctype: &str,
    data: &[u8],
    inline: bool,
    cid: Option<&str>,
) -> Vec<u8> {
    let (head, body) = crate::mime::split_message(raw);
    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    let lines: Vec<&str> =
        encoded.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap_or("")).collect();
    let disposition = if inline { "inline" } else { "attachment" };
    let mut part = format!(
        "Content-Type: {ctype}; name=\"{name}\"\r\nContent-Disposition: {disposition}; filename=\"{name}\"\r\n\
         Content-Transfer-Encoding: base64\r\n"
    );
    if let Some(c) = cid {
        part.push_str(&format!("Content-ID: <{c}>\r\n"));
    }
    part.push_str("\r\n");
    part.push_str(&lines.join("\r\n"));
    part.push_str("\r\n");
    let ct = header(raw, "Content-Type");
    let boundary = ct
        .split(';')
        .filter_map(|p| p.trim().strip_prefix("boundary="))
        .next()
        .map(|b| b.trim_matches('"').to_owned());
    if ct.to_ascii_lowercase().starts_with("multipart/mixed")
        && let Some(b) = boundary
    {
        let end = format!("--{b}--");
        let text = body.to_vec();
        if let Some(pos) = text.windows(end.len()).rposition(|w| w == end.as_bytes()) {
            let mut out = head.to_vec();
            out.extend_from_slice(text.get(..pos).unwrap_or(&[]));
            out.extend_from_slice(format!("--{b}\r\n{part}").as_bytes());
            out.extend_from_slice(text.get(pos..).unwrap_or(&[]));
            return out;
        }
    }
    let b = format!("=_mc_graph_{:x}", fnv(raw, &DateTime::from_unix(0)));
    let mut top = String::new();
    let mut inner = String::new();
    for f in crate::mime::fields(head) {
        let n = String::from_utf8_lossy(f.name).to_ascii_lowercase();
        let full = String::from_utf8_lossy(f.full);
        if n.starts_with("content-") {
            inner.push_str(&full);
        } else if n != "mime-version" {
            top.push_str(&full);
        }
    }
    if inner.is_empty() {
        inner.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    }
    let mut out = top.into_bytes();
    out.extend_from_slice(
        format!(
            "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{b}\"\r\n\r\n--{b}\r\n{inner}\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(body);
    out.extend_from_slice(format!("\r\n--{b}\r\n{part}--{b}--\r\n").as_bytes());
    out
}

/// Delivers `raw` from `user` to the local recipients of its header (like the SMTP server) and records the
/// submission; files a copy in Sent when `save`.
fn deliver(ctx: &Ctx, user: &str, raw: &[u8], save: bool) -> Result<(), (u16, Value)> {
    let rcpts = crate::jmap::header_recipients(raw);
    if rcpts.is_empty() {
        return Err(err(400, "ErrorInvalidRecipients", "the message has no recipients"));
    }
    let now = DateTime::now();
    let mut delivered = Vec::new();
    {
        let mut st = ctx.shared.lock();
        // The Bcc field never reaches the recipients.
        let (head, body) = crate::mime::split_message(raw);
        let mut public = Vec::new();
        for f in crate::mime::fields(head) {
            if !f.name.eq_ignore_ascii_case(b"bcc") {
                public.extend_from_slice(f.full);
            }
        }
        public.extend_from_slice(b"\r\n");
        public.extend_from_slice(body);
        for r in &rcpts {
            if delivered.iter().any(|d: &String| d.eq_ignore_ascii_case(r)) {
                continue;
            }
            if let Some(mb) = st.user_mut(r).and_then(|u| u.mailbox_mut("INBOX")) {
                append_msg(mb, Arc::from(public.as_slice()), Vec::new(), now);
                delivered.push(r.clone());
            }
        }
        if save
            && let Some(sent) = st
                .user(user)
                .and_then(|u| u.mailboxes.iter().find(|m| m.special == Some(SpecialUse::Sent)))
                .map(|m| m.name.clone())
            && let Some(mb) = st.user_mut(user).and_then(|u| u.mailbox_mut(&sent))
        {
            append_msg(mb, Arc::from(raw), vec!["\\Seen".to_owned()], now);
        }
        st.submissions.push(Submission {
            auth_user: Some(user.to_owned()),
            mail_from: user.to_owned(),
            rcpt_to: rcpts,
            data: raw.to_vec(),
            delivered_to: delivered,
        });
    }
    ctx.shared.notify();
    Ok(())
}

pub fn handle(conn: &mut Conn, ctx: &Ctx, req: &Request) -> io::Result<()> {
    let (path, q) = req.path.split_once('?').unwrap_or((req.path.as_str(), ""));
    let path = path.to_owned();
    let q = query(q);
    {
        let mut g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
        if g.requests.len() < MAX_LOG {
            g.requests.push(format!("{} {}", req.method, req.path));
        }
    }
    let scheme = if conn.is_tls() { "https" } else { "http" };
    let origin = format!("{scheme}://{}", req.header("host").unwrap_or("127.0.0.1"));
    if let Some(sid) = path.strip_prefix(UPLOAD_PREFIX) {
        let (st, v) = upload_piece(ctx, sid, req);
        return send_json(conn, st, &v);
    }
    let Some(user) = crate::jmap::authenticate(ctx, req) else {
        let (st, v) = err(401, "InvalidAuthenticationToken", "Access token is empty or invalid.");
        return send_json(conn, st, &v);
    };
    {
        let mut g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
        if g.throttle > 0 {
            g.throttle -= 1;
            let after = g.retry_after;
            drop(g);
            let body =
                json!({"error": {"code": "TooManyRequests", "message": "Please retry later."}}).to_string();
            let out = format!(
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: {after}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            return conn.write_all(out.as_bytes());
        }
    }
    let rest = path.strip_prefix(PREFIX).unwrap_or("").to_owned();
    let segs: Vec<String> = rest.split('/').filter(|s| !s.is_empty()).map(percent_decode).collect();
    let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let page_pref = req
        .header("prefer")
        .and_then(|p| p.split(',').find_map(|x| x.trim().strip_prefix("odata.maxpagesize=")))
        .and_then(|n| n.trim().parse::<usize>().ok())
        .unwrap_or(100);
    let page = page_pref.clamp(1, ctx.config.graph_max_page.max(1));
    if req.method == "GET"
        && let ["me", "messages", id, "$value"] = segs.as_slice()
    {
        let raw = {
            let store = ctx.shared.lock();
            let g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
            locate(&store, &g, &user, id)
                .and_then(|(mb, uid)| store.user(&user)?.mailbox(&mb)?.find(uid).map(|m| m.raw.to_vec()))
        };
        return match raw {
            Some(r) => respond(conn, "200 OK", "text/plain", &r),
            None => {
                let (st, v) =
                    err(404, "ErrorItemNotFound", "The specified object was not found in the store.");
                send_json(conn, st, &v)
            }
        };
    }
    let base = format!("{origin}{PREFIX}");
    let (st, v) = route(ctx, &user, req, &segs, &q, &body, page, &base, &origin);
    send_json(conn, st, &v)
}

fn route(
    ctx: &Ctx,
    user: &str,
    req: &Request,
    segs: &[&str],
    q: &BTreeMap<String, String>,
    body: &Value,
    page: usize,
    base: &str,
    origin: &str,
) -> (u16, Value) {
    let m = req.method.as_str();
    match (m, segs) {
        ("GET", ["me"]) => (200, json!({"displayName": user, "mail": user, "userPrincipalName": user})),
        ("GET", ["me", "mailFolders"]) => list_folders(ctx, user, None, q, page, base),
        ("GET", ["me", "mailFolders", f, "childFolders"]) => list_folders(ctx, user, Some(f), q, page, base),
        ("GET", ["me", "mailFolders", f]) => {
            let store = ctx.shared.lock();
            let Some(name) = resolve_folder(&store, user, f).filter(|n| !n.is_empty()) else {
                return err(
                    404,
                    "ErrorItemNotFound",
                    "The specified folder could not be found in the store.",
                );
            };
            match store.user(user).and_then(|u| Some((u, u.mailbox(&name)?))) {
                Some((u, mb)) => (200, folder_json(u, mb)),
                None => {
                    err(404, "ErrorItemNotFound", "The specified folder could not be found in the store.")
                }
            }
        }
        ("POST", ["me", "mailFolders"]) => create_folder(ctx, user, None, body),
        ("POST", ["me", "mailFolders", f, "childFolders"]) => create_folder(ctx, user, Some(f), body),
        ("PATCH", ["me", "mailFolders", f]) => {
            let Some(new) = body.get("displayName").and_then(Value::as_str) else {
                return err(400, "ErrorInvalidRequest", "displayName required");
            };
            let mut store = ctx.shared.lock();
            let Some(name) = resolve_folder(&store, user, f).filter(|n| !n.is_empty()) else {
                return err(404, "ErrorItemNotFound", "no such folder");
            };
            let to = match parent_name(&name) {
                Some(p) => format!("{p}/{new}"),
                None => new.to_owned(),
            };
            if let Err(e) = store.rename_mailbox(user, &name, &to) {
                return err(400, "ErrorFolderSave", &e.to_string());
            }
            let v =
                store.user(user).and_then(|u| Some(folder_json(u, u.mailbox(&to)?))).unwrap_or(Value::Null);
            drop(store);
            ctx.shared.notify();
            (200, v)
        }
        ("POST", ["me", "mailFolders", f, "move"]) => {
            let dest = body.get("destinationId").and_then(Value::as_str).unwrap_or("");
            let mut store = ctx.shared.lock();
            let (Some(name), Some(dest)) = (
                resolve_folder(&store, user, f).filter(|n| !n.is_empty()),
                resolve_folder(&store, user, dest),
            ) else {
                return err(404, "ErrorItemNotFound", "no such folder");
            };
            let to = if dest.is_empty() { leaf(&name).to_owned() } else { format!("{dest}/{}", leaf(&name)) };
            if let Err(e) = store.rename_mailbox(user, &name, &to) {
                return err(400, "ErrorMoveCopyFailed", &e.to_string());
            }
            let v =
                store.user(user).and_then(|u| Some(folder_json(u, u.mailbox(&to)?))).unwrap_or(Value::Null);
            drop(store);
            ctx.shared.notify();
            (201, v)
        }
        ("DELETE", ["me", "mailFolders", f]) => {
            let mut store = ctx.shared.lock();
            let Some(name) = resolve_folder(&store, user, f).filter(|n| !n.is_empty() && n != "INBOX") else {
                return err(404, "ErrorItemNotFound", "no such folder");
            };
            let prefix = format!("{name}/");
            let mut names: Vec<String> = store
                .user(user)
                .map(|u| {
                    u.mailboxes
                        .iter()
                        .filter(|m| m.name.starts_with(&prefix))
                        .map(|m| m.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            names.push(name);
            for n in names {
                let _ = store.delete_mailbox(user, &n);
            }
            drop(store);
            ctx.shared.notify();
            (204, Value::Null)
        }
        ("GET", ["me", "mailFolders", f, "messages", "delta"]) => delta(ctx, user, f, q, page, base),
        ("POST", ["me", "mailFolders", f, "messages"]) => create_message(ctx, user, Some(f), req, body),
        ("POST", ["me", "messages"]) => create_message(ctx, user, None, req, body),
        ("GET", ["me", "messages", id]) => {
            with_message(ctx, user, id, |g, folder, m| (200, message_json(g, user, folder, m)))
        }
        ("PATCH", ["me", "messages", id]) => patch_message(ctx, user, id, body),
        ("POST", ["me", "messages", id, "move"]) => {
            let dest = body.get("destinationId").and_then(Value::as_str).unwrap_or("");
            move_message(ctx, user, id, dest)
        }
        ("POST", ["me", "messages", id, "permanentDelete"]) => remove_message(ctx, user, id, true),
        ("DELETE", ["me", "messages", id]) => remove_message(ctx, user, id, false),
        ("POST", ["me", "sendMail"]) => {
            let ctype = req.header("content-type").unwrap_or("").to_ascii_lowercase();
            let (raw, save) = if ctype.starts_with("text/plain") {
                match mime_body(req) {
                    Some(r) => (r, true),
                    None => {
                        return err(
                            400,
                            "ErrorMimeContentInvalidBase64String",
                            "invalid base64 MIME content",
                        );
                    }
                }
            } else {
                let Some(msg) = body.get("message") else {
                    return err(400, "ErrorInvalidRequest", "message required");
                };
                (
                    mime_from_json(user, msg),
                    body.get("saveToSentItems").and_then(Value::as_bool).unwrap_or(true),
                )
            };
            match deliver(ctx, user, &raw, save) {
                Ok(()) => (202, Value::Null),
                Err(e) => e,
            }
        }
        ("POST", ["me", "messages", id, "send"]) => send_draft(ctx, user, id),
        ("POST", ["me", "messages", id, "attachments"]) => {
            let name = body.get("name").and_then(Value::as_str).unwrap_or("attachment");
            let ctype = body.get("contentType").and_then(Value::as_str).unwrap_or("application/octet-stream");
            let Some(data) = body
                .get("contentBytes")
                .and_then(Value::as_str)
                .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
            else {
                return err(400, "ErrorInvalidRequest", "contentBytes required");
            };
            let inline = body.get("isInline").and_then(Value::as_bool).unwrap_or(false);
            let cid = body.get("contentId").and_then(Value::as_str);
            match attach(ctx, user, id, name, ctype, &data, inline, cid) {
                Ok(()) => (201, json!({"id": format!("att-{}", data.len()), "name": name})),
                Err(e) => e,
            }
        }
        ("POST", ["me", "messages", id, "attachments", "createUploadSession"]) => {
            let item = body.get("AttachmentItem").cloned().unwrap_or(Value::Null);
            let size =
                item.get("size").and_then(Value::as_u64).and_then(|s| usize::try_from(s).ok()).unwrap_or(0);
            if size == 0 || size > ctx.config.max_message_size {
                return err(400, "ErrorAttachmentSizeShouldNotBeLessThanMinimumSize", "bad attachment size");
            }
            if ctx.graph.lock().unwrap_or_else(|e| e.into_inner()).refuse_uploads {
                return err(413, "ErrorAttachmentSizeLimitExceeded", "the attachment is over the size limit");
            }
            {
                let store = ctx.shared.lock();
                let g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
                if locate(&store, &g, user, id).is_none() {
                    return err(404, "ErrorItemNotFound", "no such message");
                }
            }
            let mut g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
            g.next_upload = g.next_upload.saturating_add(1);
            let sid = format!("u{}", g.next_upload);
            g.uploads.insert(
                sid.clone(),
                Upload {
                    user: user.to_owned(),
                    message: (*id).to_owned(),
                    name: item.get("name").and_then(Value::as_str).unwrap_or("attachment").to_owned(),
                    ctype: item
                        .get("contentType")
                        .and_then(Value::as_str)
                        .unwrap_or("application/octet-stream")
                        .to_owned(),
                    size,
                    inline: item.get("isInline").and_then(Value::as_bool).unwrap_or(false),
                    cid: item.get("contentId").and_then(Value::as_str).map(str::to_owned),
                    data: Vec::new(),
                },
            );
            (200, json!({"uploadUrl": format!("{origin}{UPLOAD_PREFIX}{sid}"), "nextExpectedRanges": ["0-"]}))
        }
        _ => err(400, "BadRequest", &format!("unsupported request {m} /{}", segs.join("/"))),
    }
}

fn list_folders(
    ctx: &Ctx,
    user: &str,
    parent: Option<&str>,
    q: &BTreeMap<String, String>,
    page: usize,
    base: &str,
) -> (u16, Value) {
    let store = ctx.shared.lock();
    let Some(u) = store.user(user) else { return err(404, "ErrorItemNotFound", "no such user") };
    let parent = match parent {
        Some(p) => match resolve_folder(&store, user, p) {
            Some(n) => Some(n).filter(|n| !n.is_empty()),
            None => return err(404, "ErrorItemNotFound", "no such folder"),
        },
        None => None,
    };
    let all: Vec<Value> = u
        .mailboxes
        .iter()
        .filter(|m| parent_name(&m.name).map(str::to_owned) == parent)
        .map(|m| folder_json(u, m))
        .collect();
    let top = q.get("$top").and_then(|t| t.parse::<usize>().ok()).unwrap_or(10).clamp(1, page.max(1));
    let skip = q.get("$skip").and_then(|t| t.parse::<usize>().ok()).unwrap_or(0);
    let value: Vec<Value> = all.iter().skip(skip).take(top).cloned().collect();
    let mut v = json!({"value": value});
    if skip.saturating_add(top) < all.len() {
        let path = match &parent {
            Some(p) => {
                let id = u.mailbox(p).map(folder_id).unwrap_or_default();
                format!("/me/mailFolders/{id}/childFolders")
            }
            None => "/me/mailFolders".to_owned(),
        };
        put(
            &mut v,
            "@odata.nextLink",
            json!(format!("{base}{path}?$top={top}&$skip={}", skip.saturating_add(top))),
        );
    }
    (200, v)
}

fn create_folder(ctx: &Ctx, user: &str, parent: Option<&str>, body: &Value) -> (u16, Value) {
    let Some(name) = body.get("displayName").and_then(Value::as_str).filter(|n| !n.trim().is_empty()) else {
        return err(400, "ErrorInvalidRequest", "displayName required");
    };
    let mut store = ctx.shared.lock();
    let full = match parent {
        Some(p) => match resolve_folder(&store, user, p) {
            Some(pn) if pn.is_empty() => name.to_owned(),
            Some(pn) => format!("{pn}/{name}"),
            None => return err(404, "ErrorItemNotFound", "no such folder"),
        },
        None => name.to_owned(),
    };
    match store.create_mailbox(user, &full, None) {
        Ok(()) => {}
        Err(crate::Error::MailboxExists(_)) => {
            return err(409, "ErrorFolderExists", "A folder with the specified name already exists.");
        }
        Err(e) => return err(400, "ErrorInvalidRequest", &e.to_string()),
    }
    let v = store.user(user).and_then(|u| Some(folder_json(u, u.mailbox(&full)?))).unwrap_or(Value::Null);
    drop(store);
    ctx.shared.notify();
    (201, v)
}

fn delta(
    ctx: &Ctx,
    user: &str,
    folder_ref: &str,
    q: &BTreeMap<String, String>,
    page: usize,
    base: &str,
) -> (u16, Value) {
    let ukey = user_key(user);
    if let Some(skip) = q.get("$skiptoken") {
        let mut g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
        let Some(p) = skip.parse::<u64>().ok().and_then(|k| g.pending.remove(&k)) else {
            return err(410, "SyncStateNotFound", "The sync state was not found.");
        };
        if p.user != ukey {
            return err(410, "SyncStateNotFound", "The sync state was not found.");
        }
        return delta_page(&mut g, p, page, base);
    }
    let store = ctx.shared.lock();
    let Some(name) = resolve_folder(&store, user, folder_ref).filter(|n| !n.is_empty()) else {
        return err(404, "ErrorItemNotFound", "no such folder");
    };
    let Some(mb) = store.user(user).and_then(|u| u.mailbox(&name)) else {
        return err(404, "ErrorItemNotFound", "no such folder");
    };
    let fid = folder_id(mb);
    let mut g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
    let key = (ukey.clone(), fid.clone());
    let old = match q.get("$deltatoken") {
        Some(t) => {
            let n = t.parse::<u64>().ok();
            match g
                .snaps
                .get(&key)
                .and_then(|l| l.iter().find(|(k, _)| Some(*k) == n))
                .map(|(_, s)| s.clone())
            {
                Some(s) => Some(s),
                None => return err(410, "SyncStateNotFound", "The sync state was not found; sync again."),
            }
        }
        None => None,
    };
    let after =
        q.get("$filter").and_then(|f| f.trim().strip_prefix("receivedDateTime ge ")).and_then(parse_iso);
    let mut cur: BTreeMap<String, String> = BTreeMap::new();
    let mut entries = Vec::new();
    for m in &mb.messages {
        let v = message_json(&g, user, &fid, m);
        let id = v.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let fp = fingerprint(&v);
        let changed = match &old {
            Some(o) => o.get(&id) != Some(&fp),
            None => after.is_none_or(|a| m.internal_date.unix >= a),
        };
        if changed {
            entries.push(v);
        }
        cur.insert(id, fp);
    }
    if let Some(o) = &old {
        for id in o.keys().filter(|id| !cur.contains_key(*id)) {
            entries.push(json!({"id": id, "@removed": {"reason": "deleted"}}));
        }
    }
    drop(store);
    g.next_token = g.next_token.saturating_add(1);
    let token = g.next_token;
    let list = g.snaps.entry(key).or_default();
    list.push((token, cur));
    if list.len() > MAX_SNAPSHOTS {
        list.remove(0);
    }
    let p = Pending { user: ukey, folder: fid, entries, token };
    delta_page(&mut g, p, page, base)
}

fn delta_page(g: &mut GraphState, mut p: Pending, page: usize, base: &str) -> (u16, Value) {
    let rest = if p.entries.len() > page { p.entries.split_off(page) } else { Vec::new() };
    let mut v = json!({"value": p.entries});
    let path = format!("{base}/me/mailFolders/{}/messages/delta", p.folder);
    if rest.is_empty() {
        put(&mut v, "@odata.deltaLink", json!(format!("{path}?$deltatoken={}", p.token)));
    } else {
        g.next_token = g.next_token.saturating_add(1);
        let k = g.next_token;
        put(&mut v, "@odata.nextLink", json!(format!("{path}?$skiptoken={k}")));
        g.pending.insert(k, Pending { entries: rest, ..p });
        while g.pending.len() > MAX_PENDING {
            let Some(first) = g.pending.keys().next().copied() else { break };
            g.pending.remove(&first);
        }
    }
    (200, v)
}

fn with_message(
    ctx: &Ctx,
    user: &str,
    id: &str,
    f: impl FnOnce(&GraphState, &str, &crate::store::Msg) -> (u16, Value),
) -> (u16, Value) {
    let store = ctx.shared.lock();
    let g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
    let found = locate(&store, &g, user, id).and_then(|(mb, uid)| {
        let mbox = store.user(user)?.mailbox(&mb)?;
        Some((folder_id(mbox), mbox.find(uid)?))
    });
    match found {
        Some((folder, m)) => f(&g, &folder, m),
        None => err(404, "ErrorItemNotFound", "The specified object was not found in the store."),
    }
}

fn patch_message(ctx: &Ctx, user: &str, id: &str, body: &Value) -> (u16, Value) {
    let Some(obj) = body.as_object() else { return err(400, "ErrorInvalidRequest", "JSON object required") };
    let mut store = ctx.shared.lock();
    let mut g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
    let Some((mb_name, uid)) = locate(&store, &g, user, id) else {
        return err(404, "ErrorItemNotFound", "The specified object was not found in the store.");
    };
    for k in obj.keys() {
        if !["isRead", "flag", "categories", "importance", "inferenceClassification"].contains(&k.as_str()) {
            return err(400, "ErrorInvalidProperty", &format!("cannot change {k} here"));
        }
    }
    let Some(mb) = store.user_mut(user).and_then(|u| u.mailbox_mut(&mb_name)) else {
        return err(404, "ErrorItemNotFound", "no such folder");
    };
    let modseq = mb.next_modseq();
    let folder = folder_id(mb);
    let Some(m) = mb.find_mut(uid) else { return err(404, "ErrorItemNotFound", "gone") };
    let set_flag = |m: &mut crate::store::Msg, f: &str, on: bool| {
        m.flags.retain(|x| !x.eq_ignore_ascii_case(f));
        if on {
            m.flags.push(f.to_owned());
        }
    };
    if let Some(r) = obj.get("isRead").and_then(Value::as_bool) {
        set_flag(m, "\\Seen", r);
    }
    let meta = g.meta_mut(user, id);
    if let Some(f) = obj.get("flag") {
        if let Some(s) = f.get("flagStatus").and_then(Value::as_str) {
            set_flag(m, "\\Flagged", s == "flagged");
            meta.complete = s == "complete";
            if s != "flagged" {
                meta.start = None;
                meta.due = None;
            }
        }
        // A date given (or null, which clears it); left out, it stays.
        let day = |key: &str| {
            f.get(key).map(|d| {
                d.get("dateTime").and_then(Value::as_str).map(|t| t.get(..19).unwrap_or(t).to_owned())
            })
        };
        if let Some(d) = day("startDateTime") {
            meta.start = d;
        }
        if let Some(d) = day("dueDateTime") {
            meta.due = d;
        }
    }
    if let Some(c) = obj.get("categories").and_then(Value::as_array) {
        meta.categories = c.iter().filter_map(Value::as_str).map(str::to_owned).collect();
    }
    if let Some(i) = obj.get("importance").and_then(Value::as_str) {
        meta.importance = Some(i.to_ascii_lowercase());
    }
    if let Some(i) = obj.get("inferenceClassification").and_then(Value::as_str) {
        meta.other = i == "other";
    }
    m.modseq = modseq;
    let v = message_json(&g, user, &folder, m);
    drop(g);
    drop(store);
    ctx.shared.notify();
    (200, v)
}

fn move_message(ctx: &Ctx, user: &str, id: &str, dest: &str) -> (u16, Value) {
    let mut store = ctx.shared.lock();
    let g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
    let Some((from, uid)) = locate(&store, &g, user, id) else {
        return err(404, "ErrorItemNotFound", "The specified object was not found in the store.");
    };
    let Some(to) = resolve_folder(&store, user, dest).filter(|n| !n.is_empty()) else {
        return err(404, "ErrorItemNotFound", "The destination folder could not be found.");
    };
    let Some(u) = store.user_mut(user) else { return err(404, "ErrorItemNotFound", "no such user") };
    let Some(msg) = u.mailbox_mut(&from).and_then(|mb| {
        let m = mb.find(uid).cloned()?;
        mb.messages.retain(|x| x.uid != uid);
        let _ = mb.next_modseq();
        Some(m)
    }) else {
        return err(404, "ErrorItemNotFound", "gone");
    };
    let Some(dst) = u.mailbox_mut(&to) else { return err(404, "ErrorItemNotFound", "no such folder") };
    let (_, new_uid) = append_msg(dst, Arc::clone(&msg.raw), msg.flags.clone(), msg.internal_date);
    let folder = folder_id(dst);
    let v = dst.find(new_uid).map(|m| message_json(&g, user, &folder, m)).unwrap_or(Value::Null);
    drop(g);
    drop(store);
    ctx.shared.notify();
    (201, v)
}

fn remove_message(ctx: &Ctx, user: &str, id: &str, permanent: bool) -> (u16, Value) {
    let trash = {
        let store = ctx.shared.lock();
        let g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
        let Some((mb, _)) = locate(&store, &g, user, id) else {
            return err(404, "ErrorItemNotFound", "The specified object was not found in the store.");
        };
        let t = resolve_folder(&store, user, "deleteditems");
        t.filter(|t| !permanent && *t != mb)
    };
    if let Some(t) = trash {
        let (st, v) = move_message(ctx, user, id, &t_id(ctx, user, &t));
        return if st == 201 { (204, Value::Null) } else { (st, v) };
    }
    let mut store = ctx.shared.lock();
    let g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((mb, uid)) = locate(&store, &g, user, id)
        && let Some(m) = store.user_mut(user).and_then(|u| u.mailbox_mut(&mb))
    {
        m.messages.retain(|x| x.uid != uid);
        let _ = m.next_modseq();
    }
    drop(g);
    drop(store);
    ctx.shared.notify();
    (204, Value::Null)
}

/// The folder id of mailbox `name`.
fn t_id(ctx: &Ctx, user: &str, name: &str) -> String {
    ctx.shared.lock().user(user).and_then(|u| u.mailbox(name)).map(folder_id).unwrap_or_default()
}

fn create_message(ctx: &Ctx, user: &str, folder: Option<&str>, req: &Request, body: &Value) -> (u16, Value) {
    let ctype = req.header("content-type").unwrap_or("").to_ascii_lowercase();
    let raw = if ctype.starts_with("text/plain") {
        match mime_body(req) {
            Some(r) => r,
            None => return err(400, "ErrorMimeContentInvalidBase64String", "invalid base64 MIME content"),
        }
    } else if body.is_object() {
        mime_from_json(user, body)
    } else {
        return err(400, "ErrorInvalidRequest", "a message is required");
    };
    let mut store = ctx.shared.lock();
    let target = match folder {
        Some(f) => resolve_folder(&store, user, f).filter(|n| !n.is_empty()),
        None => resolve_folder(&store, user, "drafts"),
    };
    let Some(target) = target else { return err(404, "ErrorItemNotFound", "no such folder") };
    let Some(mb) = store.user_mut(user).and_then(|u| u.mailbox_mut(&target)) else {
        return err(404, "ErrorItemNotFound", "no such folder");
    };
    let (_, uid) = append_msg(
        mb,
        Arc::from(raw.as_slice()),
        vec!["\\Draft".to_owned(), "\\Seen".to_owned()],
        DateTime::now(),
    );
    let fid = folder_id(mb);
    let g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
    let v = mb.find(uid).map(|m| message_json(&g, user, &fid, m)).unwrap_or(Value::Null);
    drop(g);
    drop(store);
    ctx.shared.notify();
    (201, v)
}

fn send_draft(ctx: &Ctx, user: &str, id: &str) -> (u16, Value) {
    let found = {
        let store = ctx.shared.lock();
        let g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
        locate(&store, &g, user, id).and_then(|(mb, uid)| {
            let m = store.user(user)?.mailbox(&mb)?.find(uid)?;
            Some((mb, uid, m.raw.to_vec()))
        })
    };
    let Some((mb, uid, raw)) = found else {
        return err(404, "ErrorItemNotFound", "The specified object was not found in the store.");
    };
    if let Err(e) = deliver(ctx, user, &raw, true) {
        return e;
    }
    if let Some(m) = ctx.shared.lock().user_mut(user).and_then(|u| u.mailbox_mut(&mb)) {
        m.messages.retain(|x| x.uid != uid);
        let _ = m.next_modseq();
    }
    ctx.shared.notify();
    (202, Value::Null)
}

fn attach(
    ctx: &Ctx,
    user: &str,
    id: &str,
    name: &str,
    ctype: &str,
    data: &[u8],
    inline: bool,
    cid: Option<&str>,
) -> Result<(), (u16, Value)> {
    let mut store = ctx.shared.lock();
    let mut g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
    let Some((mb_name, uid)) = locate(&store, &g, user, id) else {
        return Err(err(404, "ErrorItemNotFound", "no such message"));
    };
    let Some(mb) = store.user_mut(user).and_then(|u| u.mailbox_mut(&mb_name)) else {
        return Err(err(404, "ErrorItemNotFound", "no such folder"));
    };
    let modseq = mb.next_modseq();
    let Some(m) = mb.find_mut(uid) else { return Err(err(404, "ErrorItemNotFound", "gone")) };
    let raw = with_attachment(&m.raw, name, ctype, data, inline, cid);
    g.keep_id(user, id, &raw, &m.internal_date);
    m.raw = Arc::from(raw.as_slice());
    m.modseq = modseq;
    drop(g);
    drop(store);
    ctx.shared.notify();
    Ok(())
}

fn upload_piece(ctx: &Ctx, sid: &str, req: &Request) -> (u16, Value) {
    if req.method != "PUT" {
        return err(400, "BadRequest", "PUT the pieces");
    }
    if req.header("authorization").is_some() {
        return err(401, "InvalidAuthenticationToken", "upload URLs take no token");
    }
    let range = req.header("content-range").unwrap_or("");
    let start = range
        .strip_prefix("bytes ")
        .and_then(|r| r.split('-').next())
        .and_then(|s| s.trim().parse::<usize>().ok());
    let mut g = ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
    let Some(up) = g.uploads.get_mut(sid) else {
        return err(404, "ErrorItemNotFound", "no such upload session");
    };
    if start != Some(up.data.len()) || up.data.len().saturating_add(req.body.len()) > up.size {
        return err(416, "InvalidRange", "the range does not continue the upload");
    }
    up.data.extend_from_slice(&req.body);
    if up.data.len() < up.size {
        let next = up.data.len();
        return (200, json!({"nextExpectedRanges": [format!("{next}-")]}));
    }
    let Some(up) = g.uploads.remove(sid) else { return err(404, "ErrorItemNotFound", "gone") };
    drop(g);
    match attach(ctx, &up.user, &up.message, &up.name, &up.ctype, &up.data, up.inline, up.cid.as_deref()) {
        Ok(()) => (201, json!({})),
        Err(e) => e,
    }
}

impl GraphState {
    /// The id the service gives the message of `user` with this source and internal date.
    pub(crate) fn message_id(&self, user: &str, raw: &[u8], date: &DateTime) -> String {
        self.id_of(user, raw, date)
    }

    /// Sets what Graph keeps beyond the IMAP flags, as another client would: `categories`, `importance`,
    /// `flag` (`complete`), `inferenceClassification` (`other`).
    pub(crate) fn set_meta(&mut self, user: &str, id: &str, patch: &Value) {
        let meta = self.meta_mut(user, id);
        if let Some(c) = patch.get("categories").and_then(Value::as_array) {
            meta.categories = c.iter().filter_map(Value::as_str).map(str::to_owned).collect();
        }
        if let Some(i) = patch.get("importance").and_then(Value::as_str) {
            meta.importance = Some(i.to_owned());
        }
        if let Some(f) = patch.get("flag").and_then(Value::as_str) {
            meta.complete = f == "complete";
        }
        if let Some(i) = patch.get("inferenceClassification").and_then(Value::as_str) {
            meta.other = i == "other";
        }
    }

    /// Categories and importance the service keeps for message `id`.
    pub(crate) fn meta_json(&self, user: &str, id: &str) -> Value {
        let m = self.meta(user, id);
        json!({"categories": m.categories, "importance": m.importance, "complete": m.complete, "other": m.other,
               "start": m.start, "due": m.due})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_queries() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(parse_iso("2026-10-05T10:00:00Z"), parse_iso("2026-10-05T10:00:00.0000000Z"));
        assert_eq!(parse_iso(&iso(1_791_000_000)), Some(1_791_000_000));
        assert!(parse_iso("junk").is_none());
        let q = query("$filter=receivedDateTime%20ge%202026-01-01T00:00:00Z&$top=5&x");
        assert_eq!(q["$filter"], "receivedDateTime ge 2026-01-01T00:00:00Z");
        assert_eq!(q["$top"], "5");
    }

    #[test]
    fn attachments_wrap_or_extend_the_message() {
        let raw = b"From: a@example.com\r\nSubject: s\r\nContent-Type: text/plain\r\n\r\nhi\r\n";
        let one = with_attachment(raw, "a.txt", "text/plain", b"AAA", false, None);
        let text = String::from_utf8_lossy(&one).into_owned();
        assert!(
            text.contains("multipart/mixed") && text.contains("filename=\"a.txt\"") && text.contains("hi")
        );
        let two = with_attachment(&one, "b.bin", "application/octet-stream", &[1, 2, 3], false, None);
        let t2 = String::from_utf8_lossy(&two).into_owned();
        assert_eq!(t2.matches("multipart/mixed").count(), 1, "{t2}");
        assert!(t2.contains("a.txt") && t2.contains("b.bin"));
    }
}
