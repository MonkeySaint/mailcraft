//! Where tool calls go: a headless [`Session`] owned by the server, or a running MailCraft app reached over
//! its control channel (`docs/control-protocol.md`).
//!
//! Both backends speak the control channel's vocabulary: [`Backend::run`] runs a registry command by id and
//! [`Backend::query`] reads state by name (`list_rows`, `folder_rows`, `status`, `selection`,
//! `reading_header`, `commands`), with the same JSON shapes, so the tools in `tools.rs` do not care which one
//! they talk to.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use mailcraft_engine::model::{FolderId, ItemId};
use mailcraft_engine::{Session, queries};

/// A folder as the tools see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FolderInfo {
    pub id: u64,
    pub name: String,
    /// `Inbox\Projects` (backslash-separated, as in the Move dialog).
    pub path: String,
    /// `Mail`, `Calendar`, `Contacts`, … (empty when the backend does not know).
    pub kind: String,
}

/// The state a tool call acts on.
pub trait Backend {
    /// One-line description for logs and `initialize` instructions ("headless: sample mailbox").
    fn describe(&self) -> String;
    /// Runs a registry command by id (undo, journaling and enabled checks included).
    fn run(&mut self, id: &str, params: &Value) -> Result<Value, String>;
    /// Reads state by name, with the control channel's names and shapes.
    fn query(&mut self, what: &str, args: &Value) -> Result<Value, String>;
    /// Every folder the backend can name (headless: all of them; bridged: those in the folder pane).
    fn folders(&mut self) -> Result<Vec<FolderInfo>, String>;
    /// Plain-text body of a mail item; `Ok(None)` when the backend cannot provide bodies.
    fn body_text(&mut self, item: u64) -> Result<Option<String>, String>;
    /// Notices (dialogs, info bars, alerts) queued by the commands run so far, drained.
    fn take_notices(&mut self) -> Vec<Value> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------------------------------------
// Headless

/// Owns a [`Session`]: the sample mailbox or a profile folder on disk.
pub struct HeadlessBackend {
    session: Session,
    label: String,
}

impl std::fmt::Debug for HeadlessBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeadlessBackend").field("label", &self.label).finish()
    }
}

/// Most notices kept between two [`Backend::take_notices`] calls.
const MAX_NOTICES: usize = 200;

impl HeadlessBackend {
    pub fn new(session: Session, label: impl Into<String>) -> Self {
        Self { session, label: label.into() }
    }

    /// The deterministic sample mailbox (`mailcraft_engine::sample`), in memory.
    pub fn sample() -> Self {
        Self::new(mailcraft_engine::sample::sample_session(), "sample mailbox (in memory)")
    }

    /// A profile folder (created when missing); changes are journaled to it.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let session = Session::open(dir).map_err(|e| e.to_string())?;
        Ok(Self::new(session, format!("profile {}", dir.display())))
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    fn trim_notices(&mut self) {
        let n = self.session.notices.len();
        if n > MAX_NOTICES {
            self.session.notices.drain(..n.saturating_sub(MAX_NOTICES));
        }
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

/// `query` for a session: the control channel's `query` minus the UI-only names (`ui`, `composes`…).
pub fn session_query(s: &Session, what: &str, args: &Value) -> Result<Value, String> {
    match what {
        "list_rows" => to_value(&queries::list_rows(s)),
        "folder_rows" => to_value(&queries::folder_rows(s)),
        "status" => {
            let (items, unread) = queries::status_counts(s);
            Ok(json!({
                "items": items,
                "unread": unread,
                "module": format!("{:?}", s.view.module),
                "folder": s.selection.folder.0,
                "folderName": s.mailbox.folder(s.selection.folder).map(|f| f.name.clone()),
                "view": to_value(&s.view)?,
                "undo": s.undo.undo.len(),
                "redo": s.undo.redo.len(),
            }))
        }
        "selection" => to_value(&s.selection),
        "reading_header" => {
            let item = args
                .get("item")
                .and_then(Value::as_u64)
                .or_else(|| s.selection.focus.map(|i| i.0))
                .ok_or("`item` (number) required (nothing is focused)")?;
            to_value(&queries::reading_header(s, ItemId(item)))
        }
        "commands" => Ok(Value::Array(
            s.registry
                .specs
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "label": c.label,
                        "location": c.location,
                        "shortcut": c.shortcut,
                        "params": c.params,
                        "enabled": (c.enabled)(s).is_none(),
                    })
                })
                .collect(),
        )),
        other => Err(format!(
            "unknown query `{other}` (list_rows, folder_rows, status, selection, reading_header, commands)"
        )),
    }
}

/// Every folder of a session, default account first, then by path.
pub fn session_folders(s: &Session) -> Vec<FolderInfo> {
    let mb = &s.mailbox;
    let default_account = mb.default_account().map(|a| a.id);
    let mut out: Vec<(bool, FolderInfo)> = mb
        .folders
        .values()
        .map(|f| {
            let info = FolderInfo {
                id: f.id.0,
                name: f.name.clone(),
                path: mb.folder_path(f.id),
                kind: format!("{:?}", f.kind),
            };
            (Some(f.account) != default_account, info)
        })
        .collect();
    out.sort_by(|a, b| (a.0, &a.1.path, a.1.id).cmp(&(b.0, &b.1.path, b.1.id)));
    out.into_iter().map(|(_, f)| f).collect()
}

impl Backend for HeadlessBackend {
    fn describe(&self) -> String {
        format!("headless: {}", self.label)
    }

    fn run(&mut self, id: &str, params: &Value) -> Result<Value, String> {
        let r = self.session.run(id, params).map_err(|e| e.to_string());
        self.trim_notices();
        r
    }

    fn query(&mut self, what: &str, args: &Value) -> Result<Value, String> {
        session_query(&self.session, what, args)
    }

    fn folders(&mut self) -> Result<Vec<FolderInfo>, String> {
        Ok(session_folders(&self.session))
    }

    fn body_text(&mut self, item: u64) -> Result<Option<String>, String> {
        match self.session.mailbox.mail.get(&ItemId(item)) {
            Some(m) => Ok(Some(m.body_text())),
            None => Err(format!("no message with id {item}")),
        }
    }

    fn take_notices(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.session.notices)
            .iter()
            .map(|n| serde_json::to_value(n).unwrap_or(Value::Null))
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------------------
// Bridged

/// Largest response line accepted from the app (message lists of big folders are long).
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Talks to a running app's control channel (`--control-port N`), one JSON line per request.
#[derive(Debug)]
pub struct BridgedBackend {
    addr: String,
    conn: Option<(BufReader<TcpStream>, TcpStream)>,
    next_id: u64,
    timeout: Duration,
}

impl BridgedBackend {
    /// Connects to `host:port` (the app binds 127.0.0.1 only).
    pub fn connect(addr: &str) -> Result<Self, String> {
        let mut b = Self { addr: addr.to_owned(), conn: None, next_id: 0, timeout: Duration::from_secs(35) };
        b.ensure_connected()?;
        Ok(b)
    }

    /// Changes the per-request timeout (the app answers within 30 s or reports a timeout itself).
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    fn ensure_connected(&mut self) -> Result<(), String> {
        if self.conn.is_some() {
            return Ok(());
        }
        let addrs: Vec<_> =
            self.addr.to_socket_addrs().map_err(|e| format!("bad address `{}`: {e}", self.addr))?.collect();
        let mut last = format!("`{}` resolved to no address", self.addr);
        for a in addrs {
            match TcpStream::connect_timeout(&a, Duration::from_secs(5)) {
                Ok(stream) => {
                    let reader = stream.try_clone().map_err(|e| e.to_string())?;
                    // Timeouts bound how long a stuck app can hold a tool call.
                    stream.set_read_timeout(Some(self.timeout)).map_err(|e| e.to_string())?;
                    reader.set_read_timeout(Some(self.timeout)).map_err(|e| e.to_string())?;
                    let _ = stream.set_nodelay(true);
                    self.conn = Some((BufReader::new(reader), stream));
                    return Ok(());
                }
                Err(e) => last = format!("cannot connect to the MailCraft control channel at {a}: {e}"),
            }
        }
        Err(last)
    }

    /// Sends one request object (an `id` is added) and returns its `result`, or the app's `error` text.
    pub fn request(&mut self, mut req: Value) -> Result<Value, String> {
        self.ensure_connected()?;
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        if let Some(obj) = req.as_object_mut() {
            obj.insert("id".into(), json!(id));
        }
        let outcome = self.exchange(&req);
        if outcome.is_err() {
            // The connection is in an unknown state; reconnect on the next request.
            self.conn = None;
        }
        let resp = outcome?;
        if resp.get("id").and_then(Value::as_u64) != Some(id) {
            self.conn = None;
            return Err(format!(
                "the app answered request {id} with id {}",
                resp.get("id").unwrap_or(&Value::Null)
            ));
        }
        if resp.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(resp.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(resp
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the app reported an error without a message")
                .to_owned())
        }
    }

    fn exchange(&mut self, req: &Value) -> Result<Value, String> {
        let (reader, writer) = self.conn.as_mut().ok_or("not connected")?;
        let mut line = serde_json::to_vec(req).map_err(|e| e.to_string())?;
        line.push(b'\n');
        writer.write_all(&line).map_err(|e| format!("lost the connection to the app: {e}"))?;
        writer.flush().map_err(|e| format!("lost the connection to the app: {e}"))?;
        let mut buf = Vec::new();
        let n = reader
            .by_ref()
            .take(u64::try_from(MAX_RESPONSE_BYTES).unwrap_or(u64::MAX).saturating_add(1))
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("no answer from the app: {e}"))?;
        if n == 0 {
            return Err("the app closed the control connection".into());
        }
        if buf.len() > MAX_RESPONSE_BYTES {
            return Err(format!("the app's answer is larger than {MAX_RESPONSE_BYTES} bytes"));
        }
        serde_json::from_slice(&buf).map_err(|e| format!("the app sent invalid JSON: {e}"))
    }
}

impl Backend for BridgedBackend {
    fn describe(&self) -> String {
        format!("bridged: running app at {}", self.addr)
    }

    fn run(&mut self, id: &str, params: &Value) -> Result<Value, String> {
        self.request(json!({"cmd": "run", "command": id, "params": params}))
    }

    fn query(&mut self, what: &str, args: &Value) -> Result<Value, String> {
        let mut req = json!({"cmd": "query", "what": what});
        if let (Some(obj), Some(extra)) = (req.as_object_mut(), args.as_object()) {
            for (k, v) in extra {
                if k != "cmd" && k != "what" && k != "id" {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
        self.request(req)
    }

    fn folders(&mut self) -> Result<Vec<FolderInfo>, String> {
        let rows = self.query("folder_rows", &json!({}))?;
        Ok(folders_from_rows(&rows))
    }

    fn body_text(&mut self, item: u64) -> Result<Option<String>, String> {
        // Newer apps answer `query body`; older ones say the query is unknown.
        match self.query("body", &json!({"item": item})) {
            Ok(v) => Ok(v.get("text").and_then(Value::as_str).or_else(|| v.as_str()).map(str::to_owned)),
            Err(e) if e.contains("unknown query") => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Folders from folder pane rows (`queries::folder_rows` JSON): section headers and Favorites are skipped,
/// paths come from the indentation depth.
pub fn folders_from_rows(rows: &Value) -> Vec<FolderInfo> {
    let mut out = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    for row in rows.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
        let name = row.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
        if kind == "AccountRoot" {
            stack.clear();
            continue;
        }
        if kind != "Folder" {
            continue;
        }
        let Some(id) = row.get("id").and_then(Value::as_u64) else { continue };
        let depth = row.get("depth").and_then(Value::as_u64).unwrap_or(0);
        let depth = usize::try_from(depth.saturating_sub(1)).unwrap_or(0).min(stack.len());
        stack.truncate(depth);
        stack.push(name.clone());
        out.push(FolderInfo { id, name, path: stack.join("\\"), kind: String::new() });
    }
    out
}

/// Resolves a folder selector: a folder id (number or digit string), a path (`Inbox\Projects` or
/// `Inbox/Projects`) or a name, case-insensitively. Mail folders win over other kinds with the same name,
/// earlier folders (the default account) over later ones.
pub fn resolve_folder(folders: &[FolderInfo], sel: &Value) -> Result<FolderId, String> {
    if let Some(n) = sel.as_u64() {
        return Ok(FolderId(n));
    }
    let Some(text) = sel.as_str() else {
        return Err("`folder` must be a folder id (number) or a folder name/path (string)".into());
    };
    let want = text.trim().replace('/', "\\").trim_matches('\\').to_lowercase();
    if want.is_empty() {
        return Err("`folder` is empty".into());
    }
    let is_mail = |f: &&FolderInfo| f.kind.is_empty() || f.kind == "Mail";
    let by = |pick: &dyn Fn(&FolderInfo) -> bool| -> Option<u64> {
        let hits: Vec<&FolderInfo> = folders.iter().filter(|f| pick(f)).collect();
        hits.iter().copied().find(|f| is_mail(f)).or_else(|| hits.first().copied()).map(|f| f.id)
    };
    if let Some(id) =
        by(&|f| f.path.to_lowercase() == want).or_else(|| by(&|f| f.name.to_lowercase() == want))
    {
        return Ok(FolderId(id));
    }
    if let Ok(n) = want.parse::<u64>() {
        return Ok(FolderId(n));
    }
    let mut names: Vec<&str> = folders.iter().filter(is_mail).map(|f| f.path.as_str()).collect();
    names.truncate(40);
    Err(format!("no folder named `{text}` (folders: {})", names.join(", ")))
}
