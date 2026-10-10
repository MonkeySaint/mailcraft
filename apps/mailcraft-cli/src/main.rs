//! `mailcraft-cli`: list and run MailCraft commands, report parity, import/export mail, and serve MCP.

mod args;

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};

use args::{Cli, McpTarget, Source};
use mailcraft_engine::{Session, catalog, cmd, queries::ListRow};
use mailcraft_mcp::{
    BridgedBackend, HeadlessBackend, Server, resolve_folder, session_folders, session_query,
};

fn main() -> ExitCode {
    let cli = match args::parse(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mailcraft-cli: {e}");
            return ExitCode::from(2);
        }
    };
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mailcraft-cli: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Prints to stdout; a closed pipe (`| head`) ends quietly instead of failing.
fn out(text: &str) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(text.as_bytes()).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(format!("cannot write output: {e}")),
    }
}

fn print_json(v: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    out(&format!("{text}\n"))
}

fn open(source: &Source) -> Result<Session, String> {
    match source {
        Source::Sample => Ok(mailcraft_engine::sample::sample_session()),
        Source::Profile(dir) => Session::open(dir).map_err(|e| e.to_string()),
    }
}

/// Folder id for `--folder NAME` (default: the selected folder, i.e. the Inbox).
fn folder_id(s: &Session, folder: Option<&str>) -> Result<u64, String> {
    match folder {
        Some(name) => resolve_folder(&session_folders(s), &json!(name)).map(|f| f.0),
        None => Ok(s.selection.folder.0),
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli {
        Cli::Help => out(args::USAGE),
        Cli::Version => out(&format!("mailcraft-cli {}\n", env!("CARGO_PKG_VERSION"))),
        Cli::Commands { json, filter } => commands(json, filter.as_deref()),
        Cli::Parity { json } => parity(json),
        Cli::Run { source, cmds, queries } => {
            let mut s = open(&source)?;
            let mut results = Vec::new();
            let mut failed = None;
            for (id, params) in &cmds {
                match s.run(id, params) {
                    Ok(r) => results.push(json!({"command": id, "ok": true, "result": r})),
                    Err(e) => {
                        results.push(json!({"command": id, "ok": false, "error": e.to_string()}));
                        failed = Some(format!("`{id}` failed: {e}"));
                        break;
                    }
                }
            }
            let mut report = json!({"results": results});
            if !queries.is_empty() {
                let mut q = serde_json::Map::new();
                for query in queries {
                    q.insert(query.key().to_owned(), session_query(&s, query.name(), &json!({}))?);
                }
                if let Some(obj) = report.as_object_mut() {
                    obj.insert("query".into(), Value::Object(q));
                }
            }
            print_json(&report)?;
            match failed {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
        Cli::Show { source, folder, limit } => {
            let mut s = open(&source)?;
            let id = folder_id(&s, folder.as_deref())?;
            s.run("nav.selectFolder", &json!({"folder": id})).map_err(|e| e.to_string())?;
            out(&show(&s, limit))
        }
        Cli::ImportEml { source, folder, files } => {
            let mut s = open(&source)?;
            let id = folder_id(&s, folder.as_deref())?;
            let mut n = 0usize;
            for file in &files {
                file_action(&mut s, "importEml", file, id)?;
                n = n.saturating_add(1);
            }
            out(&format!("Imported {n} message(s) into {}.\n", folder_name(&s, id)))
        }
        Cli::ImportMbox { source, folder, file } => {
            let mut s = open(&source)?;
            let id = folder_id(&s, folder.as_deref())?;
            let r = file_action(&mut s, "importMbox", &file, id)?;
            let n = r.get("imported").and_then(Value::as_u64).unwrap_or(0);
            out(&format!("Imported {n} message(s) into {}.\n", folder_name(&s, id)))
        }
        Cli::ExportMbox { source, folder, file } => {
            let mut s = open(&source)?;
            let id = folder_id(&s, Some(&folder))?;
            let r = file_action(&mut s, "exportMbox", &file, id)?;
            let n = r.get("exported").and_then(Value::as_u64).unwrap_or(0);
            out(&format!("Exported {n} message(s) from {} to {}.\n", folder_name(&s, id), file.display()))
        }
        Cli::Mcp { target } => mcp(target),
    }
}

fn folder_name(s: &Session, id: u64) -> String {
    s.mailbox.folder(mailcraft_engine::model::FolderId(id)).map(|f| f.name.clone()).unwrap_or_default()
}

fn file_action(s: &mut Session, action: &str, file: &Path, folder: u64) -> Result<Value, String> {
    let path = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
    let path = path.to_str().ok_or_else(|| format!("{}: the path is not valid UTF-8", file.display()))?;
    s.run("file.openExport", &json!({"action": action, "path": path, "folder": folder}))
        .map_err(|e| format!("{}: {e}", file.display()))
}

fn commands(json_out: bool, filter: Option<&str>) -> Result<(), String> {
    let s = Session::new(mailcraft_engine::model::Mailbox::default());
    let list = session_query(&s, "commands", &json!({}))?;
    let needle = filter.map(str::to_lowercase);
    let field = |c: &Value, k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let rows: Vec<&Value> = list
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|c| {
            needle.as_deref().is_none_or(|n| {
                ["id", "label", "location"].iter().any(|k| field(c, k).to_lowercase().contains(n))
            })
        })
        .collect();
    if json_out {
        return print_json(&Value::Array(rows.into_iter().cloned().collect()));
    }
    let width = |k: &str| rows.iter().map(|c| field(c, k).chars().count()).max().unwrap_or(0);
    let (wi, ws, wl) = (width("id").max(2), width("shortcut").max(8), width("location").max(8));
    let mut text = format!("{:wi$}  {:ws$}  {:wl$}  LABEL\n", "ID", "SHORTCUT", "LOCATION");
    for c in &rows {
        text.push_str(&format!(
            "{:wi$}  {:ws$}  {:wl$}  {}\n",
            field(c, "id"),
            field(c, "shortcut"),
            field(c, "location"),
            field(c, "label")
        ));
    }
    text.push_str(&format!("{} command(s)\n", rows.len()));
    out(&text)
}

fn parity(json_out: bool) -> Result<(), String> {
    let p = catalog::parity(&cmd::registry());
    if json_out {
        return print_json(&p);
    }
    let n = |k: &str| p.get(k).and_then(Value::as_u64).unwrap_or(0);
    let mut text = format!(
        "Outlook parity: {} of {} mapped features implemented ({}%); {} catalog rows, {} not applicable.\n\n",
        n("implemented_rows"),
        n("mapped_rows"),
        p.get("percent").and_then(Value::as_f64).unwrap_or(0.0),
        n("total_rows"),
        n("not_applicable"),
    );
    text.push_str(&format!("{:10}  {:>6}  {:>11}  {:>7}\n", "AREA", "MAPPED", "IMPLEMENTED", "MISSING"));
    if let Some(areas) = p.get("by_area").and_then(Value::as_object) {
        for (area, a) in areas {
            let get = |k: &str| a.get(k).and_then(Value::as_u64).unwrap_or(0);
            let missing = a.get("missing").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
            text.push_str(&format!(
                "{area:10}  {:>6}  {:>11}  {missing:>7}\n",
                get("mapped"),
                get("implemented")
            ));
        }
    }
    out(&text)
}

/// Pads or cuts `s` to exactly `width` characters.
fn fit(s: &str, width: usize) -> String {
    let s: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let n = s.chars().count();
    if n <= width {
        format!("{s}{}", " ".repeat(width.saturating_sub(n)))
    } else {
        let cut: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// The message list as text.
fn show(s: &Session, limit: usize) -> String {
    let (items, unread) = mailcraft_engine::queries::status_counts(s);
    let name = folder_name(s, s.selection.folder.0);
    let mut text = format!("{name}: {items} item(s), {unread} unread\n");
    let rows = mailcraft_engine::queries::list_rows(s);
    let mut shown = 0usize;
    for row in &rows {
        if shown >= limit {
            text.push_str(&format!("… {} more row(s)\n", rows.len().saturating_sub(shown)));
            break;
        }
        shown = shown.saturating_add(1);
        match row {
            ListRow::Group { label, count, collapsed } => {
                let mark = if *collapsed { "+" } else { "-" };
                text.push_str(&format!("{mark} {label} ({count})\n"));
            }
            ListRow::Item(m) => {
                let marks = format!(
                    "{}{}{}",
                    if m.unread { '*' } else { ' ' },
                    if m.flagged { 'F' } else { ' ' },
                    if m.has_attachments { '@' } else { ' ' }
                );
                let who = if m.draft_marker { format!("[Draft] {}", m.who) } else { m.who.clone() };
                text.push_str(&format!(
                    "  {marks} {:>6}  {}  {}  {}\n",
                    format!("#{}", m.id.0),
                    fit(&who, 24),
                    fit(&m.subject, 48),
                    m.time
                ));
            }
        }
    }
    text
}

fn mcp(target: McpTarget) -> Result<(), String> {
    match target {
        McpTarget::Headless(source) => {
            let backend = match &source {
                Source::Sample => HeadlessBackend::sample(),
                Source::Profile(dir) => HeadlessBackend::open(dir)?,
            };
            eprintln!("mailcraft-cli mcp: serving {} on stdio", mailcraft_mcp::Backend::describe(&backend));
            mailcraft_mcp::serve_stdio(&mut Server::new(backend)).map_err(|e| e.to_string())
        }
        McpTarget::Connect(addr) => {
            let backend = BridgedBackend::connect(&addr)?;
            eprintln!("mailcraft-cli mcp: serving the app at {addr} on stdio");
            mailcraft_mcp::serve_stdio(&mut Server::new(backend)).map_err(|e| e.to_string())
        }
    }
}
