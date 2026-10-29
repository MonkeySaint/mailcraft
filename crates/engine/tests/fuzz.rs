//! Hostile-parameter fuzzing of every registered command: nothing may panic (the panic guard in
//! `Session::run` must never be what saves a command) and the mailbox must stay structurally valid.

use mailcraft_engine::compose::ComposeState;
use mailcraft_engine::sample::sample_session;
use mailcraft_engine::{CmdError, Session};
use serde_json::{Value, json};

/// Parameter keys commands use (ids, folders, names, sizes, modes …).
const KEYS: &[&str] = &[
    "id", "ids", "folder", "name", "text", "pt", "rows", "cols", "kind", "level", "mode", "key", "value",
    "query", "to", "cc", "bcc", "subject", "compose", "index", "color", "date", "start", "end", "due", "pct",
    "lines", "days", "family", "add", "remove", "group", "save", "char", "label", "lines", "scope", "page",
    "rule", "step", "category", "shortcut", "new_name", "parent", "position", "order", "on", "enabled",
    "count", "minutes", "item", "items", "address", "url", "body", "html", "format", "style", "size",
    "delta",
];

/// Keys that may name files on disk: never given a writable path.
const PATH_KEYS: &[&str] = &["path", "file", "dir", "folder_path", "filename", "dataFile"];

fn long_string() -> String {
    "A\u{1F600}\0é".repeat(2_500)
}

fn hostile_values() -> Vec<Value> {
    vec![
        Value::Null,
        json!({}),
        json!([]),
        json!(true),
        json!(0),
        json!(-1),
        json!(u64::MAX),
        json!(i64::MIN),
        json!(1e308),
        json!(-1e308),
        json!(0.5),
        json!(""),
        json!("\0"),
        json!("\u{1F600}\u{200D}\u{1F4A9}"),
        json!(long_string()),
        json!([u64::MAX, -1, "x", null, {}]),
        json!({"nested": {"deeper": [1, 2, 3]}}),
        json!("Inbox"),
        json!("Red category"),
        json!(999_999_999),
    ]
}

/// Parameter objects: the bare hostile values, then objects with every key set to one hostile value.
fn hostile_params(missing_dir: &str) -> Vec<Value> {
    let mut out = hostile_values();
    for v in hostile_values() {
        let mut obj = serde_json::Map::new();
        for k in KEYS {
            obj.insert((*k).to_owned(), v.clone());
        }
        for k in PATH_KEYS {
            let path =
                if v.is_string() { json!(format!("{missing_dir}/no/such/dir/x.eml")) } else { v.clone() };
            obj.insert((*k).to_owned(), path);
        }
        out.push(Value::Object(obj));
    }
    out.push(json!({"id": u64::MAX}));
    out.push(json!({"ids": [u64::MAX, 0, 1]}));
    out.push(json!({"folder": u64::MAX}));
    out.push(json!({"folder": 0}));
    out.push(json!({"name": long_string()}));
    out.push(json!({"text": long_string()}));
    out.push(json!({"pt": -1}));
    out.push(json!({"rows": u64::MAX, "cols": u64::MAX}));
    out.push(json!({"rows": 0, "cols": 0}));
    out
}

/// A sample session with an item selected and a compose window open, so selection- and compose-dependent
/// commands are enabled.
fn prepared() -> Session {
    let mut s = sample_session();
    let first = mailcraft_engine::queries::visible_item_ids(&s).first().copied();
    if let Some(id) = first {
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
    }
    s.open_compose(ComposeState {
        to: "Alice Rivera <alice@example.com>".into(),
        subject: "Fuzz".into(),
        dirty: true,
        ..Default::default()
    });
    s
}

#[test]
fn no_command_panics_on_hostile_params() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().display().to_string();
    let params = hostile_params(&missing);
    let ids: Vec<&'static str> = sample_session().registry.specs.iter().map(|s| s.id).collect();
    assert!(!ids.is_empty());
    let mut failures: Vec<String> = Vec::new();
    for id in &ids {
        for empty_selection in [false, true] {
            let mut s = prepared();
            if empty_selection {
                s.selection.items.clear();
                s.selection.focus = None;
                s.composes.clear();
            }
            for p in &params {
                if let Err(CmdError::Failed(msg)) = s.run(id, p)
                    && msg.contains("panicked")
                {
                    let shown: String = p.to_string().chars().take(120).collect();
                    failures.push(format!("{id} with {shown}: {msg}"));
                }
                let problems = s.mailbox.validate();
                if !problems.is_empty() {
                    failures.push(format!("{id} left an invalid mailbox: {problems:?}"));
                    break;
                }
                // Read-only views must cope with whatever state the command left.
                let _ = mailcraft_engine::queries::list_rows(&s);
                let _ = mailcraft_engine::queries::folder_rows(&s);
                let _ = mailcraft_engine::queries::status_counts(&s);
                // The memoised folder counts must follow whatever the command did (issue #25).
                if *s.all_folder_counts() != s.mailbox.folder_counts() {
                    failures.push(format!("{id} left stale folder counts"));
                    break;
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} failure(s):\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn unknown_commands_are_rejected() {
    let mut s = sample_session();
    for id in ["", "nope", "app.", ".undo", "app.undo\0", &"x".repeat(10_000)] {
        assert!(matches!(s.run(id, &Value::Null), Err(CmdError::Unknown(_))), "{id:?}");
    }
}

#[test]
fn every_command_has_metadata() {
    let s = sample_session();
    for spec in &s.registry.specs {
        let (area, name) = spec.id.split_once('.').unwrap_or(("", ""));
        assert!(!area.is_empty() && !name.is_empty(), "bad id {}", spec.id);
        assert!(!spec.label.trim().is_empty(), "{} has no label", spec.id);
        assert!(!spec.location.trim().is_empty(), "{} has no location", spec.id);
    }
}
