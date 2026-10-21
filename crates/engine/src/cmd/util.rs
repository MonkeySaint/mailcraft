//! Shared helpers for command implementations (parameter parsing, results). Owned by the core agent; others
//! may add functions at the end of the file only if they coordinate by appending (no edits to existing ones).

use serde_json::{Value, json};

use crate::CmdError;
use mailcraft_model::{FolderId, ItemId};

/// Reads an optional u64 param.
pub fn opt_u64(p: &Value, key: &str) -> Option<u64> {
    p.get(key).and_then(Value::as_u64)
}

/// Reads a required string param.
pub fn req_str<'a>(p: &'a Value, key: &str) -> Result<&'a str, CmdError> {
    p.get(key).and_then(Value::as_str).ok_or_else(|| CmdError::Params(format!("`{key}` (string) required")))
}

/// Reads an optional string param.
pub fn opt_str<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str)
}

/// Reads an optional bool param.
pub fn opt_bool(p: &Value, key: &str) -> Option<bool> {
    p.get(key).and_then(Value::as_bool)
}

/// `{"folder": n}` → FolderId.
pub fn opt_folder(p: &Value) -> Option<FolderId> {
    opt_u64(p, "folder").map(FolderId)
}

/// `{"ids": [n, …]}` → item ids (non-numbers ignored).
pub fn opt_ids(p: &Value) -> Option<Vec<ItemId>> {
    p.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(ItemId).collect())
}

/// `{"ok": true}`.
pub fn ok() -> Value {
    json!({"ok": true})
}

/// Queues `Notice::OpenDialog(name, params)` and returns `{"dialog": name}` (commands whose effect is a dialog).
pub fn dialog(s: &mut crate::Session, name: &str, params: Value) -> crate::CmdResult {
    s.notify(crate::Notice::OpenDialog(name.to_owned(), params));
    Ok(json!({"dialog": name}))
}

/// Reads a required u64 param.
pub fn req_u64(p: &Value, key: &str) -> Result<u64, CmdError> {
    opt_u64(p, key).ok_or_else(|| CmdError::Params(format!("`{key}` (non-negative integer) required")))
}

/// Reads an optional i64 param.
pub fn opt_i64(p: &Value, key: &str) -> Option<i64> {
    p.get(key).and_then(Value::as_i64)
}

/// `{"folder": n}` → an existing folder, else a `Params` error.
pub fn req_folder(s: &crate::Session, p: &Value) -> Result<FolderId, CmdError> {
    let id = FolderId(req_u64(p, "folder")?);
    if s.mailbox.folders.contains_key(&id) {
        Ok(id)
    } else {
        Err(CmdError::Params(format!("no folder with id {}", id.0)))
    }
}

/// `{"id": n}` → an existing mail item, else a `Params` error.
pub fn req_mail(s: &crate::Session, p: &Value) -> Result<ItemId, CmdError> {
    let id = ItemId(req_u64(p, "id")?);
    if s.mailbox.mail.contains_key(&id) {
        Ok(id)
    } else {
        Err(CmdError::Params(format!("no message with id {}", id.0)))
    }
}

/// Enabled predicate: something is selected in the message list (or focused).
pub fn needs_selection(s: &crate::Session) -> Option<&'static str> {
    let any =
        s.selection.items.iter().chain(s.selection.focus.iter()).any(|id| s.mailbox.mail.contains_key(id));
    if any { None } else { Some("select a message first") }
}

/// Existing mail ids among [`crate::Session::target_ids`] (unknown ids dropped, order kept, no duplicates).
pub fn mail_targets(s: &crate::Session, p: &Value) -> Vec<ItemId> {
    let mut seen = std::collections::BTreeSet::new();
    s.target_ids(p).into_iter().filter(|id| s.mailbox.mail.contains_key(id) && seen.insert(*id)).collect()
}

/// A parsed profile file kept between reads (building blocks, saved themes): menus and dialogs list their
/// entries every frame, and reading and parsing the file each time would stutter the UI. The file is read
/// again when its path, size or modification time change, or after [`FileCache::forget`] (call it after
/// writing the file, in case the time stamp is too coarse to show the change).
pub(crate) struct FileCache<T> {
    slot: std::sync::Mutex<Option<CachedFile<T>>>,
}

struct CachedFile<T> {
    path: std::path::PathBuf,
    stamp: Option<(u64, Option<std::time::SystemTime>)>,
    value: std::sync::Arc<T>,
}

impl<T> FileCache<T> {
    pub(crate) const fn new() -> Self {
        FileCache { slot: std::sync::Mutex::new(None) }
    }

    /// The parsed contents of `path`, from `load` when the file changed since it was last parsed.
    pub(crate) fn get(
        &self,
        path: &std::path::Path,
        load: impl FnOnce(&std::path::Path) -> T,
    ) -> std::sync::Arc<T> {
        let stamp = std::fs::metadata(path).ok().map(|m| (m.len(), m.modified().ok()));
        let mut slot = self.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(c) = slot.as_ref()
            && c.path == path
            && c.stamp == stamp
        {
            return c.value.clone();
        }
        let value = std::sync::Arc::new(load(path));
        *slot = Some(CachedFile { path: path.to_owned(), stamp, value: value.clone() });
        value
    }

    /// Forgets what was read from `path` (the next [`FileCache::get`] reads it again).
    pub(crate) fn forget(&self, path: &std::path::Path) {
        let mut slot = self.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.as_ref().is_some_and(|c| c.path == path) {
            *slot = None;
        }
    }
}

#[cfg(test)]
mod file_cache_tests {
    use super::FileCache;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn reads_again_only_when_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.json");
        std::fs::write(&path, "one").unwrap();
        let cache: FileCache<String> = FileCache::new();
        let loads = AtomicUsize::new(0);
        let read = |p: &std::path::Path| {
            loads.fetch_add(1, Ordering::Relaxed);
            std::fs::read_to_string(p).unwrap_or_default()
        };
        assert_eq!(*cache.get(&path, read), "one");
        for _ in 0..10 {
            assert_eq!(*cache.get(&path, read), "one");
        }
        assert_eq!(loads.load(Ordering::Relaxed), 1, "an unchanged file is parsed once");
        std::fs::write(&path, "three").unwrap();
        assert_eq!(*cache.get(&path, read), "three", "a new size is noticed");
        std::fs::write(&path, "seven").unwrap();
        cache.forget(&path);
        assert_eq!(*cache.get(&path, read), "seven", "forget reads again even with the same size");
        let other = dir.path().join("g.json");
        assert_eq!(*cache.get(&other, read), "", "another path is its own entry; a missing file is empty");
        std::fs::write(&other, "x").unwrap();
        assert_eq!(*cache.get(&other, read), "x", "a file that appears is read");
    }
}
