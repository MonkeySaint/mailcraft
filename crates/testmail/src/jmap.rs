//! JMAP server (RFC 8620 core, RFC 8621 mail) over HTTP/1.1, on the same in-memory store as IMAP and POP3:
//! the session resource (`/.well-known/jmap`), the API endpoint with Mailbox/get, Mailbox/changes,
//! Email/query, Email/get, Email/changes, Email/set (keywords, mailboxIds, destroy), Email/import,
//! Identity/get and EmailSubmission/set (with `onSuccessUpdateEmail`; delivery like the SMTP server), plus blob
//! upload and download.
//!
//! One account (`A`) per user; HTTP Basic authentication with the user's password, or a Bearer token equal to
//! it (test convenience). Email ids are a hash of the message source and its internal date, so they survive
//! moves; mailbox ids a hash of the mailbox name. State strings name snapshots taken whenever the store
//! changed, and `*/changes` compare the snapshot of the given state with the current one (unknown or too old
//! states give `cannotCalculateChanges`). One request per connection (`Connection: close`).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{Map, Value, json};

use crate::conn::Conn;
use crate::date::DateTime;
use crate::server::Ctx;
use crate::store::{SpecialUse, Store, Submission, append_msg};

const MAX_HEADER_LINE: usize = 16 * 1024;
const MAX_HEADERS: usize = 100;
/// Snapshots kept per user for `*/changes`.
const MAX_SNAPSHOTS: usize = 200;
const ACCOUNT: &str = "A";

/// What `*/changes` compare: per email (mailboxes, keywords) and per mailbox (name, parent).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Snapshot {
    emails: BTreeMap<String, (Vec<String>, Vec<String>)>,
    mailboxes: BTreeMap<String, (String, Option<String>)>,
}

/// JMAP-only state shared by all connections: uploaded blobs and per-user snapshots.
#[derive(Default)]
pub struct JmapState {
    blobs: BTreeMap<String, Vec<u8>>,
    snaps: BTreeMap<String, Vec<(u64, Snapshot)>>,
    next_state: u64,
}

pub type SharedJmap = Arc<Mutex<JmapState>>;

fn fnv(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn mailbox_id(name: &str) -> String {
    format!("m{:016x}", fnv(name.as_bytes()))
}

fn email_id(raw: &[u8], date: &DateTime) -> String {
    let mut d = raw.to_vec();
    d.extend_from_slice(date.unix.to_string().as_bytes());
    format!("e{:016x}", fnv(&d))
}

fn role(name: &str, special: Option<SpecialUse>) -> Option<&'static str> {
    if name.eq_ignore_ascii_case("INBOX") {
        return Some("inbox");
    }
    Some(match special? {
        SpecialUse::Sent => "sent",
        SpecialUse::Drafts => "drafts",
        SpecialUse::Trash => "trash",
        SpecialUse::Junk => "junk",
        SpecialUse::Archive => "archive",
        SpecialUse::All => "all",
        SpecialUse::Flagged => "flagged",
    })
}

fn flag_to_keyword(f: &str) -> String {
    match f.to_ascii_lowercase().as_str() {
        "\\seen" => "$seen".into(),
        "\\flagged" => "$flagged".into(),
        "\\answered" => "$answered".into(),
        "\\draft" => "$draft".into(),
        other => other.trim_start_matches('\\').to_owned(),
    }
}

fn keyword_to_flag(k: &str) -> String {
    match k.to_ascii_lowercase().as_str() {
        "$seen" => "\\Seen".into(),
        "$flagged" => "\\Flagged".into(),
        "$answered" => "\\Answered".into(),
        "$draft" => "\\Draft".into(),
        "$forwarded" => "$Forwarded".into(),
        other => other.to_owned(),
    }
}

/// Where an email lives: (mailbox name, uid).
fn locate(store: &Store, user: &str, id: &str) -> Option<(String, u32)> {
    let u = store.user(user)?;
    for mb in &u.mailboxes {
        for m in &mb.messages {
            if email_id(&m.raw, &m.internal_date) == id {
                return Some((mb.name.clone(), m.uid));
            }
        }
    }
    None
}

fn snapshot(store: &Store, user: &str) -> Snapshot {
    let mut s = Snapshot::default();
    let Some(u) = store.user(user) else { return s };
    for mb in &u.mailboxes {
        let parent = mb.name.rsplit_once('/').map(|(p, _)| mailbox_id(p));
        let leaf = mb.name.rsplit_once('/').map_or(mb.name.as_str(), |(_, l)| l).to_owned();
        s.mailboxes.insert(mailbox_id(&mb.name), (leaf, parent));
        for m in &mb.messages {
            let e = s.emails.entry(email_id(&m.raw, &m.internal_date)).or_default();
            e.0.push(mailbox_id(&mb.name));
            let mut k: Vec<String> = m.flags.iter().map(|f| flag_to_keyword(f)).collect();
            k.sort();
            e.1 = k;
        }
    }
    s
}

pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub(crate) fn read_request(conn: &mut Conn, max_body: usize) -> io::Result<Option<Request>> {
    let Some(line) = conn.read_line(MAX_HEADER_LINE)? else { return Ok(None) };
    let line = String::from_utf8_lossy(&line).trim_end().to_owned();
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else { return Ok(None) };
    let (method, path) = (method.to_owned(), path.to_owned());
    let mut headers = Vec::new();
    loop {
        let Some(h) = conn.read_line(MAX_HEADER_LINE)? else { return Ok(None) };
        let h = String::from_utf8_lossy(&h).trim_end().to_owned();
        if h.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Ok(None);
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if len > max_body {
        return Ok(None);
    }
    let body = if len > 0 {
        match conn.read_bytes(len)? {
            Some(b) => b,
            None => return Ok(None),
        }
    } else {
        Vec::new()
    };
    Ok(Some(Request { method, path, headers, body }))
}

pub(crate) fn respond(conn: &mut Conn, status: &str, ctype: &str, body: &[u8]) -> io::Result<()> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    conn.write_all(&out)
}

fn respond_json(conn: &mut Conn, status: &str, v: &Value) -> io::Result<()> {
    respond(conn, status, "application/json", v.to_string().as_bytes())
}

/// The user the request authenticates as.
pub(crate) fn authenticate(ctx: &Ctx, req: &Request) -> Option<String> {
    let h = req.header("authorization")?;
    if let Some(b64) = h.strip_prefix("Basic ") {
        let raw = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
        let text = String::from_utf8(raw).ok()?;
        let (user, pass) = text.split_once(':')?;
        return ctx.shared.lock().check_password(user, pass);
    }
    if let Some(token) = h.strip_prefix("Bearer ") {
        let store = ctx.shared.lock();
        return store.oauth.user_of(token.trim()).or_else(|| store.user_by_password(token.trim()));
    }
    None
}

pub fn serve(mut conn: Conn, ctx: &Ctx, jmap: &SharedJmap) {
    let max = ctx.config.max_message_size.saturating_add(1024 * 1024);
    if let Ok(Some(req)) = read_request(&mut conn, max) {
        let _ = handle(&mut conn, ctx, jmap, &req);
    }
    conn.close();
}

fn handle(conn: &mut Conn, ctx: &Ctx, jmap: &SharedJmap, req: &Request) -> io::Result<()> {
    let Some(user) = authenticate(ctx, req) else {
        return respond(conn, "401 Unauthorized", "text/plain", b"authentication required");
    };
    let path = req.path.split('?').next().unwrap_or("");
    match (req.method.as_str(), path) {
        ("GET", "/.well-known/jmap") | ("GET", "/jmap/session") => {
            let events = if ctx.config.jmap_push {
                "/jmap/eventsource?types={types}&closeafter={closeafter}&ping={ping}"
            } else {
                ""
            };
            let v = json!({
                "capabilities": {
                    "urn:ietf:params:jmap:core": {
                        "maxSizeUpload": ctx.config.max_message_size, "maxConcurrentUpload": 4,
                        "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4, "maxCallsInRequest": 32,
                        "maxObjectsInGet": 500, "maxObjectsInSet": 500, "collationAlgorithms": [],
                    },
                    "urn:ietf:params:jmap:mail": {},
                    "urn:ietf:params:jmap:submission": {},
                    "urn:ietf:params:jmap:quota": {},
                },
                "accounts": { ACCOUNT: { "name": user, "isPersonal": true, "isReadOnly": false,
                    "accountCapabilities": { "urn:ietf:params:jmap:mail": {}, "urn:ietf:params:jmap:submission": {},
                        "urn:ietf:params:jmap:quota": {} } } },
                "primaryAccounts": { "urn:ietf:params:jmap:mail": ACCOUNT, "urn:ietf:params:jmap:submission": ACCOUNT,
                    "urn:ietf:params:jmap:quota": ACCOUNT },
                "username": user,
                "apiUrl": "/jmap/api",
                "downloadUrl": "/jmap/download/{accountId}/{blobId}/{name}?type={type}",
                "uploadUrl": "/jmap/upload/{accountId}/",
                "eventSourceUrl": events,
                "state": "s0",
            });
            respond_json(conn, "200 OK", &v)
        }
        ("GET", "/jmap/eventsource") if ctx.config.jmap_push => event_source(conn, ctx, jmap, &user, req),
        ("POST", "/jmap/api") => {
            let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
                return respond_json(
                    conn,
                    "400 Bad Request",
                    &json!({"type": "urn:ietf:params:jmap:error:notJSON", "status": 400, "detail": "not JSON"}),
                );
            };
            let out = api(ctx, jmap, &user, &body);
            respond_json(conn, "200 OK", &out)
        }
        ("POST", p) if p.starts_with("/jmap/upload/") => {
            let id = format!("B{:016x}", fnv(&req.body));
            let size = req.body.len();
            jmap.lock().unwrap_or_else(|e| e.into_inner()).blobs.insert(id.clone(), req.body.clone());
            let ctype = req.header("content-type").unwrap_or("application/octet-stream").to_owned();
            respond_json(
                conn,
                "201 Created",
                &json!({"accountId": ACCOUNT, "blobId": id, "type": ctype, "size": size}),
            )
        }
        ("GET", p) if p.starts_with("/jmap/download/") => {
            let blob = p.trim_start_matches("/jmap/download/").split('/').nth(1).unwrap_or("").to_owned();
            let data = if let Some(eid) = blob.strip_prefix("raw-") {
                let store = ctx.shared.lock();
                locate(&store, &user, eid)
                    .and_then(|(mb, uid)| store.user(&user)?.mailbox(&mb)?.find(uid).map(|m| m.raw.to_vec()))
            } else {
                jmap.lock().unwrap_or_else(|e| e.into_inner()).blobs.get(&blob).cloned()
            };
            match data {
                Some(d) => respond(conn, "200 OK", "message/rfc822", &d),
                None => respond(conn, "404 Not Found", "text/plain", b"no such blob"),
            }
        }
        _ => respond(conn, "404 Not Found", "text/plain", b"not found"),
    }
}

/// Push (RFC 8620 §7.3): a `text/event-stream` with a `state` event now and whenever the user's mail
/// changes, and `ping` events at the asked interval. Runs until the client goes away or the servers stop.
fn event_source(conn: &mut Conn, ctx: &Ctx, jmap: &SharedJmap, user: &str, req: &Request) -> io::Result<()> {
    let query = req.path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let q = crate::oauth::parse_form(query);
    let ping = q.get("ping").and_then(|p| p.parse::<u64>().ok()).unwrap_or(0).min(3600);
    let close_after_state = q.get("closeafter").is_some_and(|c| c == "state");
    conn.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
          Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
    )?;
    let chunk = |conn: &mut Conn, text: &str| -> io::Result<()> {
        conn.write_all(format!("{:x}\r\n{text}\r\n", text.len()).as_bytes())
    };
    let mut last_state = String::new();
    let mut last_gen = None;
    let mut last_write = std::time::Instant::now();
    loop {
        if ctx.stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        let generation = ctx.shared.generation();
        if last_gen != Some(generation) {
            last_gen = Some(generation);
            let (state, _) = current_state(ctx, jmap, user);
            if state != last_state {
                last_state.clone_from(&state);
                let data =
                    json!({"@type": "StateChange", "changed": {ACCOUNT: {"Email": state, "Mailbox": state}}});
                chunk(conn, &format!("event: state\ndata: {data}\n\n"))?;
                last_write = std::time::Instant::now();
                if close_after_state {
                    return conn.write_all(b"0\r\n\r\n");
                }
            }
        }
        if ping > 0 && last_write.elapsed() >= std::time::Duration::from_secs(ping) {
            chunk(conn, &format!("event: ping\ndata: {{\"interval\":{ping}}}\n\n"))?;
            last_write = std::time::Instant::now();
        }
        std::thread::sleep(crate::conn::POLL);
    }
}

/// The current state string of `user`, taking a new snapshot when the store changed.
fn current_state(ctx: &Ctx, jmap: &SharedJmap, user: &str) -> (String, Snapshot) {
    let snap = snapshot(&ctx.shared.lock(), user);
    let mut j = jmap.lock().unwrap_or_else(|e| e.into_inner());
    let st = &mut *j;
    let list = st.snaps.entry(user.to_owned()).or_default();
    if let Some((n, s)) = list.last()
        && *s == snap
    {
        return (n.to_string(), snap);
    }
    st.next_state = st.next_state.saturating_add(1);
    let n = st.next_state;
    list.push((n, snap.clone()));
    if list.len() > MAX_SNAPSHOTS {
        list.remove(0);
    }
    (n.to_string(), snap)
}

fn old_snapshot(jmap: &SharedJmap, user: &str, state: &str) -> Option<Snapshot> {
    let n: u64 = state.parse().ok()?;
    let j = jmap.lock().unwrap_or_else(|e| e.into_inner());
    j.snaps.get(user)?.iter().find(|(k, _)| *k == n).map(|(_, s)| s.clone())
}

fn error(kind: &str, desc: &str) -> (String, Value) {
    ("error".into(), json!({"type": kind, "description": desc}))
}

fn api(ctx: &Ctx, jmap: &SharedJmap, user: &str, body: &Value) -> Value {
    let calls = body.get("methodCalls").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut responses: Vec<Value> = Vec::new();
    for call in calls {
        let Some(a) = call.as_array() else { continue };
        let name = a.first().and_then(Value::as_str).unwrap_or("").to_owned();
        let mut args = a.get(1).cloned().unwrap_or(Value::Null);
        let id = a.get(2).and_then(Value::as_str).unwrap_or("").to_owned();
        // Result references (RFC 8620 §3.7): only `#ids` with path `/ids` or `/list/*/id`.
        if let Some(r) = args.get("#ids").cloned() {
            let of = r.get("resultOf").and_then(Value::as_str).unwrap_or("");
            let found = responses
                .iter()
                .find(|x| x.get(2).and_then(Value::as_str) == Some(of))
                .and_then(|x| x.get(1));
            let ids = found.and_then(|v| v.get("ids")).cloned().unwrap_or(json!([]));
            if let Some(m) = args.as_object_mut() {
                m.remove("#ids");
                m.insert("ids".into(), ids);
            }
        }
        let extra;
        let (rname, rargs) = match name.as_str() {
            "Mailbox/get" => (name.clone(), mailbox_get(ctx, jmap, user)),
            "Mailbox/changes" | "Email/changes" => (name.clone(), changes(ctx, jmap, user, &name, &args)),
            "Email/query" => (name.clone(), email_query(ctx, jmap, user, &args)),
            "Email/get" => (name.clone(), email_get(ctx, jmap, user, &args)),
            "Email/set" => (name.clone(), email_set(ctx, jmap, user, &args)),
            "Email/import" => (name.clone(), email_import(ctx, jmap, user, &args)),
            "Quota/get" => (name.clone(), quota_get(ctx, user)),
            "Identity/get" => (
                name.clone(),
                json!({"accountId": ACCOUNT, "state": "i0", "notFound": [],
                    "list": [{"id": "I1", "name": "", "email": user, "replyTo": null, "bcc": null, "mayDelete": false}]}),
            ),
            "EmailSubmission/set" => {
                let (sub, upd) = submission_set(ctx, jmap, user, &args);
                extra = upd;
                if let Some(u) = extra {
                    responses.push(json!(["EmailSubmission/set", sub, id]));
                    responses.push(json!(["Email/set", u, id]));
                    continue;
                }
                (name.clone(), sub)
            }
            _ => error("unknownMethod", &name),
        };
        let (rname, rargs) = match rargs.get("__error") {
            Some(e) => error(e.as_str().unwrap_or("serverFail"), ""),
            None => (rname, rargs),
        };
        responses.push(json!([rname, rargs, id]));
    }
    json!({"methodResponses": responses, "sessionState": "s0"})
}

/// RFC 9425 Quota/get: one account-scoped octets quota for mail when the user has a limit.
fn quota_get(ctx: &Ctx, user: &str) -> Value {
    let store = ctx.shared.lock();
    let list: Vec<Value> = store
        .user(user)
        .and_then(|u| {
            let limit = u.quota?;
            Some(vec![
                json!({
                    "id": "Q1", "resourceType": "octets", "used": u.used(), "hardLimit": limit,
                    "scope": "account", "name": "Mail storage", "types": ["Mail"],
                }),
                json!({
                    "id": "Q2", "resourceType": "count", "used": u.message_count(), "hardLimit": 100_000,
                    "scope": "account", "name": "Messages", "types": ["Mail"],
                }),
            ])
        })
        .unwrap_or_default();
    json!({"accountId": ACCOUNT, "state": "q0", "list": list, "notFound": []})
}

fn mailbox_get(ctx: &Ctx, jmap: &SharedJmap, user: &str) -> Value {
    let (state, _) = current_state(ctx, jmap, user);
    let store = ctx.shared.lock();
    let list: Vec<Value> = store
        .user(user)
        .map(|u| {
            u.mailboxes
                .iter()
                .enumerate()
                .map(|(i, mb)| {
                    let parent = mb.name.rsplit_once('/').map(|(p, _)| mailbox_id(p));
                    let leaf = mb.name.rsplit_once('/').map_or(mb.name.as_str(), |(_, l)| l);
                    json!({
                        "id": mailbox_id(&mb.name), "name": leaf, "parentId": parent,
                        "role": role(&mb.name, mb.special), "sortOrder": i,
                        "totalEmails": mb.messages.len(), "unreadEmails": mb.unseen(),
                        "isSubscribed": mb.subscribed,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({"accountId": ACCOUNT, "state": state, "list": list, "notFound": []})
}

fn changes(ctx: &Ctx, jmap: &SharedJmap, user: &str, name: &str, args: &Value) -> Value {
    let since = args.get("sinceState").and_then(Value::as_str).unwrap_or("");
    let Some(old) = old_snapshot(jmap, user, since) else {
        return json!({"__error": "cannotCalculateChanges"});
    };
    let (state, cur) = current_state(ctx, jmap, user);
    let (created, updated, destroyed) = if name == "Email/changes" {
        diff(&old.emails, &cur.emails)
    } else {
        diff(&old.mailboxes, &cur.mailboxes)
    };
    let mut v = json!({"accountId": ACCOUNT, "oldState": since, "newState": state, "hasMoreChanges": false,
        "created": created, "updated": updated, "destroyed": destroyed});
    if name == "Mailbox/changes"
        && let Some(m) = v.as_object_mut()
    {
        m.insert("updatedProperties".into(), Value::Null);
    }
    v
}

fn diff<T: PartialEq>(
    old: &BTreeMap<String, T>,
    cur: &BTreeMap<String, T>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let created = cur.keys().filter(|k| !old.contains_key(*k)).cloned().collect();
    let destroyed = old.keys().filter(|k| !cur.contains_key(*k)).cloned().collect();
    let updated =
        cur.iter().filter(|(k, v)| old.get(*k).is_some_and(|o| o != *v)).map(|(k, _)| k.clone()).collect();
    (created, updated, destroyed)
}

/// True when filter `f` (or a FilterOperator below it) has one of the conditions `names`.
fn uses_condition(f: &Value, names: &[String], depth: usize) -> bool {
    if names.is_empty() || depth > 16 {
        return false;
    }
    if let Some(list) = f.get("conditions").and_then(Value::as_array) {
        return list.iter().any(|c| uses_condition(c, names, depth + 1));
    }
    f.as_object().is_some_and(|o| o.keys().any(|k| names.iter().any(|n| n == k)))
}

fn email_query(ctx: &Ctx, jmap: &SharedJmap, user: &str, args: &Value) -> Value {
    let _ = current_state(ctx, jmap, user);
    let in_mailbox =
        args.get("filter").and_then(|f| f.get("inMailbox")).and_then(Value::as_str).map(str::to_owned);
    // RFC 8621 §4.4.1: `after` (receivedAt >= date) and `before` (receivedAt < date).
    let date =
        |k: &str| args.get("filter").and_then(|f| f.get(k)).and_then(Value::as_str).map(parse_utc_date);
    let (after, before) = (date("after"), date("before"));
    if after.is_some_and(|a| a.is_none()) || before.is_some_and(|b| b.is_none()) {
        return json!({"__error": "invalidArguments"});
    }
    let (after, before) = (after.flatten(), before.flatten());
    if let Some(f) = args.get("filter")
        && uses_condition(f, &ctx.config.jmap_unsupported_filters, 0)
    {
        return json!({"__error": "unsupportedFilter"});
    }
    let store = ctx.shared.lock();
    let mut all: Vec<(i64, String)> = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(u) = store.user(user) {
        for mb in &u.mailboxes {
            if in_mailbox.as_ref().is_some_and(|m| *m != mailbox_id(&mb.name)) {
                continue;
            }
            for m in &mb.messages {
                let t = m.internal_date.unix;
                if after.is_some_and(|a| t < a) || before.is_some_and(|b| t >= b) {
                    continue;
                }
                if let Some(f) = args.get("filter")
                    && !filter_matches(f, m, &mailbox_id(&mb.name), 0)
                {
                    continue;
                }
                let id = email_id(&m.raw, &m.internal_date);
                if seen.insert(id.clone()) {
                    all.push((m.internal_date.unix, id));
                }
            }
        }
    }
    let asc = args
        .get("sort")
        .and_then(Value::as_array)
        .and_then(|s| s.first())
        .and_then(|s| s.get("isAscending"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    all.sort();
    if !asc {
        all.reverse();
    }
    let total = all.len();
    let pos = args.get("position").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(u64::MAX).min(10_000) as usize;
    let ids: Vec<String> = all.into_iter().skip(pos).take(limit).map(|(_, i)| i).collect();
    json!({"accountId": ACCOUNT, "queryState": "q0", "canCalculateChanges": false, "position": pos,
        "ids": ids, "total": total})
}

/// RFC 8621 section 4.4.1 filters, as far as tests need them: FilterOperator (AND, OR, NOT) and the
/// conditions text, from, to, cc, subject, body (case-insensitive substring of the header field or the
/// whole message), hasKeyword / notKeyword, inMailbox, minSize / maxSize. `after` / `before` are applied by
/// the caller. Unknown conditions match.
fn filter_matches(f: &Value, m: &crate::store::Msg, mailbox: &str, depth: usize) -> bool {
    if depth > 16 {
        return false;
    }
    if let Some(op) = f.get("operator").and_then(Value::as_str) {
        let list: Vec<&Value> =
            f.get("conditions").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
        let mut hits = list.iter().map(|c| filter_matches(c, m, mailbox, depth + 1));
        return match op {
            "AND" => hits.all(|x| x),
            "OR" => hits.any(|x| x),
            "NOT" => !hits.any(|x| x),
            _ => false,
        };
    }
    let raw = String::from_utf8_lossy(&m.raw).to_lowercase();
    let header_end = raw.find("\r\n\r\n").or_else(|| raw.find("\n\n")).unwrap_or(raw.len());
    let (head, body) = raw.split_at(header_end);
    let field = |name: &str| -> String {
        head.lines()
            .filter(|l| l.starts_with(&format!("{name}:")))
            .map(|l| l.get(name.len() + 1..).unwrap_or("").to_owned())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let has_kw = |k: &str| m.has_flag(&keyword_to_flag(k));
    let obj = f.as_object().cloned().unwrap_or_default();
    obj.iter().all(|(k, v)| {
        let text = v.as_str().map(str::to_lowercase).unwrap_or_default();
        match k.as_str() {
            "text" => raw.contains(&text),
            "from" | "to" | "cc" | "subject" => field(k).contains(&text),
            "body" => body.contains(&text),
            "hasKeyword" => has_kw(&text),
            "notKeyword" => !has_kw(&text),
            "inMailbox" => v.as_str() == Some(mailbox),
            "minSize" => v.as_u64().is_none_or(|n| m.raw.len() as u64 >= n),
            "maxSize" => v.as_u64().is_none_or(|n| (m.raw.len() as u64) < n),
            _ => true,
        }
    })
}

/// A JMAP `UTCDate` (`2026-10-05T00:00:00Z`) as Unix seconds.
fn parse_utc_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 || b.get(10) != Some(&b'T') || b.get(19) != Some(&b'Z') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let days = crate::date::days_from_civil(y, u32::try_from(mo).ok()?, u32::try_from(d).ok()?);
    Some(days * 86_400 + h * 3600 + mi * 60 + se)
}

fn email_get(ctx: &Ctx, jmap: &SharedJmap, user: &str, args: &Value) -> Value {
    let (state, _) = current_state(ctx, jmap, user);
    let ids: Vec<String> = args
        .get("ids")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    let store = ctx.shared.lock();
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    for id in ids {
        let mut mailboxes = Map::new();
        let mut found: Option<(Vec<String>, usize, i64)> = None;
        if let Some(u) = store.user(user) {
            for mb in &u.mailboxes {
                for m in &mb.messages {
                    if email_id(&m.raw, &m.internal_date) == id {
                        mailboxes.insert(mailbox_id(&mb.name), json!(true));
                        found = Some((m.flags.clone(), m.raw.len(), m.internal_date.unix));
                    }
                }
            }
        }
        match found {
            Some((flags, size, date)) => {
                let kw: Map<String, Value> =
                    flags.iter().map(|f| (flag_to_keyword(f), json!(true))).collect();
                list.push(
                    json!({"id": id, "blobId": format!("raw-{id}"), "threadId": id, "mailboxIds": mailboxes,
                    "keywords": kw, "size": size, "receivedAt": date}),
                );
            }
            None => not_found.push(id),
        }
    }
    json!({"accountId": ACCOUNT, "state": state, "list": list, "notFound": not_found})
}

/// Applies an Email/set update patch to one email (keywords and mailbox membership).
fn update_email(store: &mut Store, user: &str, id: &str, patch: &Value) -> Result<(), &'static str> {
    let (mb_name, uid) = locate(store, user, id).ok_or("notFound")?;
    let Some(obj) = patch.as_object() else { return Err("invalidPatch") };
    let mut add: Vec<String> = Vec::new();
    let mut remove: Vec<String> = Vec::new();
    let mut replace: Option<Vec<String>> = None;
    let mut target: Option<String> = None;
    let names: BTreeMap<String, String> = store
        .user(user)
        .map(|u| u.mailboxes.iter().map(|m| (mailbox_id(&m.name), m.name.clone())).collect())
        .unwrap_or_default();
    for (k, v) in obj {
        if k == "keywords" {
            let list = v.as_object().ok_or("invalidPatch")?;
            replace = Some(list.keys().map(|k| keyword_to_flag(k)).collect());
        } else if let Some(kw) = k.strip_prefix("keywords/") {
            let kw = kw.replace("~1", "/").replace("~0", "~");
            if v.as_bool() == Some(true) {
                add.push(keyword_to_flag(&kw))
            } else {
                remove.push(keyword_to_flag(&kw))
            }
        } else if k == "mailboxIds" {
            let first = v.as_object().and_then(|m| m.keys().next().cloned()).ok_or("invalidPatch")?;
            target = Some(names.get(&first).cloned().ok_or("invalidProperties")?);
        } else if let Some(mbid) = k.strip_prefix("mailboxIds/") {
            if v.as_bool() == Some(true) {
                target = Some(names.get(mbid).cloned().ok_or("invalidProperties")?);
            }
        } else {
            return Err("invalidProperties");
        }
    }
    let u = store.user_mut(user).ok_or("notFound")?;
    let mb = u.mailbox_mut(&mb_name).ok_or("notFound")?;
    let modseq = mb.next_modseq();
    let msg = mb.find_mut(uid).ok_or("notFound")?;
    if let Some(r) = replace {
        msg.flags = r;
    }
    msg.flags.retain(|f| !remove.iter().any(|x| x.eq_ignore_ascii_case(f)));
    for a in add {
        if !msg.flags.iter().any(|f| f.eq_ignore_ascii_case(&a)) {
            msg.flags.push(a);
        }
    }
    msg.modseq = modseq;
    if let Some(t) = target.filter(|t| *t != mb_name) {
        let m = msg.clone();
        mb.messages.retain(|x| x.uid != uid);
        let dest = u.mailbox_mut(&t).ok_or("notFound")?;
        append_msg(dest, m.raw, m.flags, m.internal_date);
    }
    Ok(())
}

fn email_set(ctx: &Ctx, jmap: &SharedJmap, user: &str, args: &Value) -> Value {
    let (old_state, _) = current_state(ctx, jmap, user);
    let mut updated = Map::new();
    let mut not_updated = Map::new();
    let mut destroyed = Vec::new();
    let mut not_destroyed = Map::new();
    {
        let mut store = ctx.shared.lock();
        if let Some(u) = args.get("update").and_then(Value::as_object) {
            for (id, patch) in u {
                match update_email(&mut store, user, id, patch) {
                    Ok(()) => {
                        updated.insert(id.clone(), Value::Null);
                    }
                    Err(e) => {
                        not_updated.insert(id.clone(), json!({"type": e}));
                    }
                }
            }
        }
        if let Some(d) = args.get("destroy").and_then(Value::as_array) {
            for id in d.iter().filter_map(Value::as_str) {
                match locate(&store, user, id) {
                    Some((mb, uid)) => {
                        if let Some(m) = store.user_mut(user).and_then(|u| u.mailbox_mut(&mb)) {
                            m.messages.retain(|x| x.uid != uid);
                            let _ = m.next_modseq();
                        }
                        destroyed.push(id.to_owned());
                    }
                    None => {
                        not_destroyed.insert(id.to_owned(), json!({"type": "notFound"}));
                    }
                }
            }
        }
    }
    ctx.shared.notify();
    let (new_state, _) = current_state(ctx, jmap, user);
    let not_created: Map<String, Value> = args
        .get("create")
        .and_then(Value::as_object)
        .map(|c| c.keys().map(|k| (k.clone(), json!({"type": "forbidden"}))).collect())
        .unwrap_or_default();
    json!({"accountId": ACCOUNT, "oldState": old_state, "newState": new_state, "created": {},
        "updated": updated, "destroyed": destroyed, "notCreated": not_created, "notUpdated": not_updated,
        "notDestroyed": not_destroyed})
}

fn email_import(ctx: &Ctx, jmap: &SharedJmap, user: &str, args: &Value) -> Value {
    let (old_state, _) = current_state(ctx, jmap, user);
    let mut created = Map::new();
    let mut not_created = Map::new();
    let emails = args.get("emails").and_then(Value::as_object).cloned().unwrap_or_default();
    for (cid, e) in emails {
        let blob = e.get("blobId").and_then(Value::as_str).unwrap_or("");
        let data = jmap.lock().unwrap_or_else(|x| x.into_inner()).blobs.get(blob).cloned();
        let Some(data) = data else {
            not_created.insert(cid, json!({"type": "blobNotFound"}));
            continue;
        };
        let mb_id = e.get("mailboxIds").and_then(Value::as_object).and_then(|m| m.keys().next().cloned());
        let flags: Vec<String> = e
            .get("keywords")
            .and_then(Value::as_object)
            .map(|k| k.keys().map(|k| keyword_to_flag(k)).collect())
            .unwrap_or_default();
        let now = DateTime::now();
        let mut store = ctx.shared.lock();
        let name = store.user(user).and_then(|u| {
            u.mailboxes.iter().find(|m| Some(mailbox_id(&m.name)) == mb_id).map(|m| m.name.clone())
        });
        let Some(name) = name else {
            not_created.insert(cid, json!({"type": "invalidProperties"}));
            continue;
        };
        if let Some(mb) = store.user_mut(user).and_then(|u| u.mailbox_mut(&name)) {
            append_msg(mb, Arc::from(data.as_slice()), flags, now);
        }
        let id = email_id(&data, &now);
        created.insert(
            cid,
            json!({"id": id, "blobId": format!("raw-{id}"), "threadId": id, "size": data.len()}),
        );
    }
    ctx.shared.notify();
    let (new_state, _) = current_state(ctx, jmap, user);
    json!({"accountId": ACCOUNT, "oldState": old_state, "newState": new_state, "created": created,
        "notCreated": not_created})
}

/// Addresses in the To, Cc and Bcc fields of `raw` (simple `<addr>` / bare address scan).
fn header_recipients(raw: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(raw);
    let head = text.split("\r\n\r\n").next().unwrap_or("");
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    let mut fields = Vec::new();
    for line in head.split("\r\n") {
        if line.starts_with([' ', '\t']) {
            if let Some(c) = current.as_mut() {
                c.push_str(line);
            }
            continue;
        }
        if let Some(c) = current.take() {
            fields.push(c);
        }
        current = Some(line.to_owned());
    }
    fields.extend(current);
    for f in fields {
        let Some((name, value)) = f.split_once(':') else { continue };
        if !["to", "cc", "bcc"].contains(&name.trim().to_ascii_lowercase().as_str()) {
            continue;
        }
        for part in value.split(',') {
            let a = match (part.find('<'), part.find('>')) {
                (Some(i), Some(j)) if i < j => part.get(i + 1..j).unwrap_or(""),
                _ => part.trim(),
            };
            if a.contains('@') {
                out.push(a.trim().to_owned());
            }
        }
    }
    out
}

fn submission_set(ctx: &Ctx, jmap: &SharedJmap, user: &str, args: &Value) -> (Value, Option<Value>) {
    let mut created = Map::new();
    let mut not_created = Map::new();
    let mut moved: Vec<(String, String)> = Vec::new();
    let creates = args.get("create").and_then(Value::as_object).cloned().unwrap_or_default();
    for (cid, sub) in creates {
        let email = sub.get("emailId").and_then(Value::as_str).unwrap_or("").to_owned();
        let raw = {
            let store = ctx.shared.lock();
            locate(&store, user, &email)
                .and_then(|(mb, uid)| store.user(user)?.mailbox(&mb)?.find(uid).map(|m| m.raw.to_vec()))
        };
        let Some(raw) = raw else {
            not_created.insert(cid, json!({"type": "invalidProperties", "description": "no such email"}));
            continue;
        };
        let env = sub.get("envelope");
        let from = env
            .and_then(|e| e.get("mailFrom"))
            .and_then(|m| m.get("email"))
            .and_then(Value::as_str)
            .unwrap_or(user)
            .to_owned();
        let rcpts: Vec<String> = env
            .and_then(|e| e.get("rcptTo"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|r| r.get("email")?.as_str().map(str::to_owned)).collect())
            .unwrap_or_else(|| header_recipients(&raw));
        if rcpts.is_empty() {
            not_created.insert(cid, json!({"type": "noRecipients"}));
            continue;
        }
        let now = DateTime::now();
        let mut delivered = Vec::new();
        {
            let mut st = ctx.shared.lock();
            for r in &rcpts {
                if let Some(mb) = st.user_mut(r).and_then(|u| u.mailbox_mut("INBOX")) {
                    append_msg(mb, Arc::from(raw.as_slice()), Vec::new(), now);
                    delivered.push(r.clone());
                }
            }
            st.submissions.push(Submission {
                auth_user: Some(user.to_owned()),
                mail_from: from,
                rcpt_to: rcpts,
                data: raw,
                delivered_to: delivered,
            });
        }
        let sid = format!("S{}", created.len().saturating_add(1));
        created.insert(cid.clone(), json!({"id": sid, "undoStatus": "final"}));
        moved.push((cid, email));
    }
    let mut extra = None;
    if let Some(on) = args.get("onSuccessUpdateEmail").and_then(Value::as_object) {
        let (old_state, _) = current_state(ctx, jmap, user);
        let mut updated = Map::new();
        let mut store = ctx.shared.lock();
        for (key, patch) in on {
            let Some(cid) = key.strip_prefix('#') else { continue };
            let Some((_, email)) = moved.iter().find(|(c, _)| c == cid) else { continue };
            if update_email(&mut store, user, email, patch).is_ok() {
                updated.insert(email.clone(), Value::Null);
            }
        }
        drop(store);
        ctx.shared.notify();
        let (new_state, _) = current_state(ctx, jmap, user);
        extra = Some(
            json!({"accountId": ACCOUNT, "oldState": old_state, "newState": new_state, "updated": updated}),
        );
    }
    ctx.shared.notify();
    (json!({"accountId": ACCOUNT, "created": created, "notCreated": not_created}), extra)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_keywords_and_recipients() {
        assert_eq!(mailbox_id("INBOX"), mailbox_id("INBOX"));
        assert_ne!(mailbox_id("INBOX"), mailbox_id("Sent"));
        let d = DateTime::now();
        assert_eq!(email_id(b"x", &d), email_id(b"x", &d));
        assert_eq!(flag_to_keyword("\\Seen"), "$seen");
        assert_eq!(keyword_to_flag("$flagged"), "\\Flagged");
        assert_eq!(role("inbox", None), Some("inbox"));
        assert_eq!(role("Sent", Some(SpecialUse::Sent)), Some("sent"));
        let raw = b"To: A <a@example.com>, b@example.org\r\nCc: c@example.net\r\nSubject: x\r\n\r\nTo: no@example.com\r\n";
        assert_eq!(header_recipients(raw), vec!["a@example.com", "b@example.org", "c@example.net"]);
        let (c, u, d) = diff(
            &BTreeMap::from([("a".to_owned(), 1), ("b".to_owned(), 2)]),
            &BTreeMap::from([("b".to_owned(), 3), ("c".to_owned(), 4)]),
        );
        assert_eq!((c, u, d), (vec!["c".to_owned()], vec!["b".to_owned()], vec!["a".to_owned()]));
    }

    #[test]
    fn utc_dates() {
        assert_eq!(parse_utc_date("1970-01-02T00:00:01Z"), Some(86_401));
        assert_eq!(parse_utc_date("2026-10-10T12:00:00Z"), Some(1_791_633_600));
        for bad in
            ["", "2026-10-10", "2026-13-01T00:00:00Z", "2026-10-10T12:00:00+01:00", "x026-10-10T12:00:00Z"]
        {
            assert_eq!(parse_utc_date(bad), None, "{bad}");
        }
    }
}
