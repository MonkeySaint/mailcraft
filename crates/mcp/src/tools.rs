//! The MCP tools: their JSON Schemas (`tools/list`) and what each call does (`tools/call`).

use serde_json::{Map, Value, json};

use crate::backend::{Backend, resolve_folder};

/// Default and largest page of `list_messages` / `search` rows.
pub const DEFAULT_ROWS: usize = 100;
pub const MAX_ROWS: usize = 1000;
/// Default and largest body length returned by `read_message`, in characters.
pub const DEFAULT_BODY_CHARS: usize = 20_000;
pub const MAX_BODY_CHARS: usize = 1_000_000;

/// A tool failure: reported to the client as a result with `isError: true` (not a protocol error), so the
/// model sees the message and can correct itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError(pub String);

impl From<String> for ToolError {
    fn from(s: String) -> Self {
        ToolError(s)
    }
}

impl From<&str> for ToolError {
    fn from(s: &str) -> Self {
        ToolError(s.to_owned())
    }
}

type ToolResult = Result<Value, ToolError>;

const FOLDER_SCHEMA: &str = r#"{"description": "Folder id (number) or name/path (\"Inbox\", \"Inbox/Projects\")", "type": ["string", "integer"]}"#;

fn folder_schema() -> Value {
    serde_json::from_str(FOLDER_SCHEMA).unwrap_or_else(|_| json!({"type": ["string", "integer"]}))
}

/// Tool names, in `tools/list` order.
pub const TOOL_NAMES: [&str; 9] = [
    "list_commands",
    "run_command",
    "list_folders",
    "list_messages",
    "read_message",
    "select",
    "search",
    "status",
    "parity",
];

/// `tools/list` entries.
pub fn definitions() -> Vec<Value> {
    let read_only = |title: &str| json!({"title": title, "readOnlyHint": true, "openWorldHint": false});
    let mutating = |title: &str, destructive: bool| json!({"title": title, "readOnlyHint": false, "destructiveHint": destructive, "openWorldHint": false});
    vec![
        json!({
            "name": "list_commands",
            "title": "List commands",
            "description": "Every MailCraft command (everything the ribbon, menus and shortcuts can do): id, label, \
                UI location, shortcut, parameter description and whether it is enabled right now. Run one with \
                run_command.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": {"type": "string", "description": "Case-insensitive substring of id, label or location"},
                    "enabled_only": {"type": "boolean", "description": "Only commands enabled right now"}
                },
                "additionalProperties": false
            },
            "annotations": read_only("List commands"),
        }),
        json!({
            "name": "run_command",
            "title": "Run command",
            "description": "Runs a MailCraft command by id (e.g. mail.reply, mail.markRead, folder.new, view.readingPane) \
                with its JSON params, exactly as the ribbon or a shortcut would (undo, enabled checks). Most commands \
                act on the current selection; use select first or pass ids/folder params. Returns the command's \
                result and any notices (dialogs or info bars it asked the UI to show).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Command id from list_commands"},
                    "params": {"type": "object", "description": "Command parameters (see the command's params text); default {}"}
                },
                "required": ["id"],
                "additionalProperties": false
            },
            "annotations": mutating("Run command", true),
        }),
        json!({
            "name": "list_folders",
            "title": "List folders",
            "description": "Folder pane rows (Favorites, each account's folder tree with unread badges) for the \
                current module. With all=true, every folder with its id, path and kind instead.",
            "inputSchema": {
                "type": "object",
                "properties": {"all": {"type": "boolean", "description": "Every folder (id, name, path, kind)"}},
                "additionalProperties": false
            },
            "annotations": read_only("List folders"),
        }),
        json!({
            "name": "list_messages",
            "title": "List messages",
            "description": "Message list rows of the selected folder (or `folder`, which is then selected) with the \
                folder's view (sort, groups, filter, search): group headers and items with id, sender, subject, \
                time, preview, unread/flag/attachment state.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "folder": folder_schema(),
                    "offset": {"type": "integer", "minimum": 0, "description": "Rows to skip (default 0)"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_ROWS, "description": "Rows to return (default 100)"}
                },
                "additionalProperties": false
            },
            "annotations": mutating("List messages", false),
        }),
        json!({
            "name": "read_message",
            "title": "Read message",
            "description": "Reading pane header (subject, from, to, cc, date, info bar lines, attachments, \
                categories) and plain-text body of a message. Does not mark it read.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "integer", "minimum": 0, "description": "Message id (from list_messages)"},
                    "max_chars": {"type": "integer", "minimum": 0, "maximum": MAX_BODY_CHARS, "description": "Body length cap (default 20000)"}
                },
                "required": ["id"],
                "additionalProperties": false
            },
            "annotations": read_only("Read message"),
        }),
        json!({
            "name": "select",
            "title": "Select",
            "description": "Selects a folder and/or a message, as clicking them would. Commands without explicit \
                ids act on the selection.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "folder": folder_schema(),
                    "item": {"type": "integer", "minimum": 0, "description": "Message id to select"}
                },
                "additionalProperties": false
            },
            "annotations": mutating("Select", false),
        }),
        json!({
            "name": "search",
            "title": "Search",
            "description": "Searches the current folder (scope per the Search tab) like the title-bar search box \
                and returns the matching rows. An empty query clears the search.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Words, \"phrases\" or from:/subject:/hasattachments: terms"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_ROWS, "description": "Rows to return (default 100)"}
                },
                "required": ["query"],
                "additionalProperties": false
            },
            "annotations": mutating("Search", false),
        }),
        json!({
            "name": "status",
            "title": "Status",
            "description": "Status bar state: items and unread in view, module, selected folder, view settings, undo \
                and redo depth.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": read_only("Status"),
        }),
        json!({
            "name": "parity",
            "title": "Feature parity",
            "description": "How much of the observed feature catalog MailCraft implements: totals, percent, \
                per-area counts and missing command ids.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": read_only("Feature parity"),
        }),
    ]
}

/// Runs tool `name`. `Ok(None)` = no such tool (a protocol error); `Ok(Some(Err))` = the tool failed.
pub fn call(backend: &mut dyn Backend, name: &str, args: &Value) -> Option<ToolResult> {
    let empty = Map::new();
    let args = match args {
        Value::Object(m) => m,
        Value::Null => &empty,
        _ => return Some(Err("`arguments` must be an object".into())),
    };
    let r = match name {
        "list_commands" => list_commands(backend, args),
        "run_command" => run_command(backend, args),
        "list_folders" => list_folders(backend, args),
        "list_messages" => list_messages(backend, args),
        "read_message" => read_message(backend, args),
        "select" => select(backend, args),
        "search" => search(backend, args),
        "status" => backend.query("status", &json!({})).map_err(ToolError),
        "parity" => Ok(mailcraft_engine::catalog::parity(&mailcraft_engine::cmd::registry())),
        _ => return None,
    };
    Some(r)
}

// ---------------------------------------------------------------------------------------------------------
// Argument helpers

fn opt_bool(args: &Map<String, Value>, key: &str) -> Result<bool, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(ToolError(format!("`{key}` must be true or false"))),
    }
}

fn opt_usize(args: &Map<String, Value>, key: &str, default: usize, max: usize) -> Result<usize, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .map(|n| n.min(max))
            .ok_or_else(|| ToolError(format!("`{key}` must be a non-negative integer"))),
    }
}

fn req_u64(args: &Map<String, Value>, key: &str) -> Result<u64, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Err(ToolError(format!("`{key}` (integer) is required"))),
        Some(v) => v.as_u64().ok_or_else(|| ToolError(format!("`{key}` must be a non-negative integer"))),
    }
}

fn opt_u64(args: &Map<String, Value>, key: &str) -> Result<Option<u64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => req_u64(args, key).map(Some),
    }
}

fn req_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, ToolError> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| ToolError(format!("`{key}` (string) is required")))
}

fn opt_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(ToolError(format!("`{key}` must be a string"))),
    }
}

/// Selects the folder named by `args.folder` (when present); returns its id.
fn select_folder_arg(backend: &mut dyn Backend, args: &Map<String, Value>) -> Result<Option<u64>, ToolError> {
    let Some(sel) = args.get("folder").filter(|v| !v.is_null()) else { return Ok(None) };
    let folders = if sel.is_u64() { Vec::new() } else { backend.folders()? };
    let id = resolve_folder(&folders, sel)?;
    backend.run("nav.selectFolder", &json!({"folder": id.0}))?;
    Ok(Some(id.0))
}

/// Message list rows flattened for agents: `{"type": "group", label, count, collapsed}` and
/// `{"type": "item", id, who, subject, …}`.
fn flatten_rows(rows: &Value) -> Vec<Value> {
    rows.as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|row| {
            let obj = row.as_object()?;
            let (tag, body) = obj.iter().next()?;
            let mut out = body.as_object().cloned().unwrap_or_default();
            out.insert("type".into(), json!(tag.to_lowercase()));
            Some(Value::Object(out))
        })
        .collect()
}

fn page(rows: Vec<Value>, offset: usize, limit: usize) -> Value {
    let total = rows.len();
    let items = rows.iter().filter(|r| r.get("type").and_then(Value::as_str) == Some("item")).count();
    let shown: Vec<Value> = rows.into_iter().skip(offset).take(limit).collect();
    let next = offset.saturating_add(shown.len());
    json!({
        "total_rows": total,
        "items": items,
        "offset": offset,
        "rows": shown,
        "next_offset": if next < total { json!(next) } else { Value::Null },
    })
}

fn truncate_chars(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((cut, _)) => (text.get(..cut).unwrap_or("").to_owned(), true),
        None => (text.to_owned(), false),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Tools

fn list_commands(backend: &mut dyn Backend, args: &Map<String, Value>) -> ToolResult {
    let filter = opt_str(args, "filter")?.map(|f| f.trim().to_lowercase()).filter(|f| !f.is_empty());
    let enabled_only = opt_bool(args, "enabled_only")?;
    let all = backend.query("commands", &json!({}))?;
    let field = |c: &Value, k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_lowercase();
    let list: Vec<Value> = all
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|c| !enabled_only || c.get("enabled").and_then(Value::as_bool) == Some(true))
        .filter(|c| {
            filter.as_deref().is_none_or(|f| {
                field(c, "id").contains(f)
                    || field(c, "label").contains(f)
                    || field(c, "location").contains(f)
            })
        })
        .cloned()
        .collect();
    Ok(json!({"count": list.len(), "commands": list}))
}

fn run_command(backend: &mut dyn Backend, args: &Map<String, Value>) -> ToolResult {
    let id = req_str(args, "id")?.trim();
    if id.is_empty() {
        return Err("`id` is empty".into());
    }
    let params = match args.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err("`params` must be an object".into()),
    };
    let result = backend.run(id, &params);
    let notices = backend.take_notices();
    match result {
        Ok(r) if notices.is_empty() => Ok(json!({"command": id, "result": r})),
        Ok(r) => Ok(json!({"command": id, "result": r, "notices": notices})),
        Err(e) => Err(ToolError(format!("{id}: {e}"))),
    }
}

fn list_folders(backend: &mut dyn Backend, args: &Map<String, Value>) -> ToolResult {
    if opt_bool(args, "all")? {
        let folders = backend.folders()?;
        return Ok(json!({"count": folders.len(), "folders": folders}));
    }
    let rows = backend.query("folder_rows", &json!({}))?;
    Ok(json!({"rows": rows}))
}

fn list_messages(backend: &mut dyn Backend, args: &Map<String, Value>) -> ToolResult {
    let offset = opt_usize(args, "offset", 0, usize::MAX)?;
    let limit = opt_usize(args, "limit", DEFAULT_ROWS, MAX_ROWS)?.max(1);
    select_folder_arg(backend, args)?;
    let status = backend.query("status", &json!({}))?;
    let rows = flatten_rows(&backend.query("list_rows", &json!({}))?);
    let mut out = page(rows, offset, limit);
    if let Some(obj) = out.as_object_mut() {
        obj.insert("folder".into(), status.get("folder").cloned().unwrap_or(Value::Null));
        obj.insert("folderName".into(), status.get("folderName").cloned().unwrap_or(Value::Null));
        obj.insert("search".into(), status.pointer("/view/search").cloned().unwrap_or(Value::Null));
    }
    Ok(out)
}

fn read_message(backend: &mut dyn Backend, args: &Map<String, Value>) -> ToolResult {
    let id = req_u64(args, "id")?;
    let max = opt_usize(args, "max_chars", DEFAULT_BODY_CHARS, MAX_BODY_CHARS)?;
    let header = backend.query("reading_header", &json!({"item": id}))?;
    if header.is_null() {
        return Err(ToolError(format!("no message with id {id}")));
    }
    let mut out = json!({"id": id, "header": header});
    match backend.body_text(id)? {
        Some(text) => {
            let (body, truncated) = truncate_chars(&text, max);
            if let Some(obj) = out.as_object_mut() {
                obj.insert("body".into(), json!(body));
                obj.insert("truncated".into(), json!(truncated));
            }
        }
        None => {
            if let Some(obj) = out.as_object_mut() {
                obj.insert("body".into(), Value::Null);
                obj.insert("note".into(), json!("this app version does not report message bodies"));
            }
        }
    }
    Ok(out)
}

fn select(backend: &mut dyn Backend, args: &Map<String, Value>) -> ToolResult {
    let item = opt_u64(args, "item")?;
    let folder = select_folder_arg(backend, args)?;
    if folder.is_none() && item.is_none() {
        return Err("give `folder`, `item` or both".into());
    }
    if let Some(id) = item {
        backend.run("nav.selectItem", &json!({"id": id}))?;
    }
    Ok(backend.query("selection", &json!({}))?)
}

fn search(backend: &mut dyn Backend, args: &Map<String, Value>) -> ToolResult {
    let query = req_str(args, "query")?;
    let limit = opt_usize(args, "limit", DEFAULT_ROWS, MAX_ROWS)?.max(1);
    let summary = if query.trim().is_empty() {
        backend.run("search.close", &json!({}))?
    } else {
        backend.run("search.query", &json!({"text": query}))?
    };
    let rows = flatten_rows(&backend.query("list_rows", &json!({}))?);
    let mut out = page(rows, 0, limit);
    if let Some(obj) = out.as_object_mut() {
        obj.insert("search".into(), summary);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_definitions() {
        let defs = definitions();
        let names: Vec<&str> = defs.iter().filter_map(|d| d["name"].as_str()).collect();
        assert_eq!(names, TOOL_NAMES);
        for d in &defs {
            assert_eq!(d["inputSchema"]["type"], "object", "{d}");
            assert!(d["description"].as_str().is_some_and(|s| s.len() > 20));
        }
    }

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate_chars("héllo", 2), ("hé".to_owned(), true));
        assert_eq!(truncate_chars("hé", 2), ("hé".to_owned(), false));
        assert_eq!(truncate_chars("", 0), (String::new(), false));
    }

    #[test]
    fn flattens_rows() {
        let rows =
            json!([{"Group": {"label": "Today", "count": 1, "collapsed": false}}, {"Item": {"id": 3}}, 7]);
        let flat = flatten_rows(&rows);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0]["type"], "group");
        assert_eq!(flat[1]["type"], "item");
        assert_eq!(flat[1]["id"], 3);
    }

    #[test]
    fn pages() {
        let rows: Vec<Value> = (0..5).map(|i| json!({"type": "item", "id": i})).collect();
        let p = page(rows, 3, 10);
        assert_eq!(p["rows"].as_array().map(Vec::len), Some(2));
        assert_eq!(p["next_offset"], Value::Null);
        assert_eq!(p["items"], 5);
    }
}
