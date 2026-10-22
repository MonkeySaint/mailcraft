//! The web pages View in Browser writes (`mail.viewInBrowser`).
//!
//! Each session keeps its pages in a folder of its own: a new, uniquely named folder that only this user
//! can open (mode 0700 on Unix), created on first use under the profile's `browser` folder, or the system
//! temporary folder without a saved profile. A page is deleted when its message is gone for good (deleted
//! permanently, Deleted Items emptied, removed by a sync), and the whole folder when the session ends. A
//! session that did not end cleanly (a crash, a kill, a power cut) leaves its folder behind: opening the
//! profile again removes such leftovers ([`sweep_leftovers`]), since the profile is only ever open in one
//! session at a time (the store's lock).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mailcraft_model::ItemId;

/// Name prefix of a session's page folder.
const PREFIX: &str = "mailcraft-pages-";

/// Where the page folders of the profile in `profile_dir` go.
pub(crate) fn profile_base(profile_dir: &Path) -> PathBuf {
    profile_dir.join("browser")
}

/// Removes the page folders left in `base` by sessions that did not end cleanly: only directories (never a
/// symbolic link, nor what it points to) whose name starts with the page-folder prefix. Returns how many
/// were removed. Call it only on a folder no running session uses (the profile's, once its lock is held).
pub(crate) fn sweep_leftovers(base: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(base) else { return 0 };
    let mut removed = 0usize;
    for e in entries.flatten() {
        if !e.file_name().to_str().is_some_and(|n| n.starts_with(PREFIX)) {
            continue;
        }
        let path = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.file_type().is_dir() {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => removed = removed.saturating_add(1),
            Err(err) => log::warn!("View in Browser: cannot remove {}: {err}", path.display()),
        }
    }
    removed
}

/// The session's View in Browser pages.
#[derive(Debug, Default)]
pub struct BrowserPages {
    #[cfg(not(target_arch = "wasm32"))]
    dir: Option<tempfile::TempDir>,
    /// Page written per message.
    pages: BTreeMap<ItemId, PathBuf>,
}

impl BrowserPages {
    /// The session's private folder for pages, created under `base` on first use.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn dir(&mut self, base: &Path) -> Result<PathBuf, String> {
        if let Some(d) = &self.dir
            && d.path().is_dir()
        {
            return Ok(d.path().to_path_buf());
        }
        std::fs::create_dir_all(base).map_err(|e| format!("{}: {e}", base.display()))?;
        // A randomly named folder that did not exist before (so nobody can prepare it), that only this user
        // can open (on Windows the temporary folder is the user's own already).
        let mut b = tempfile::Builder::new();
        b.prefix(PREFIX);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            b.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let d = b.tempdir_in(base).map_err(|e| format!("{}: {e}", base.display()))?;
        let path = d.path().to_path_buf();
        self.dir = Some(d);
        self.pages.clear();
        Ok(path)
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn dir(&mut self, _base: &Path) -> Result<PathBuf, String> {
        Err("pages cannot be written in the browser".into())
    }

    /// Notes that the page of message `id` is at `path` (inside [`BrowserPages::dir`]).
    pub(crate) fn written(&mut self, id: ItemId, path: PathBuf) {
        self.pages.insert(id, path);
    }

    /// The page written for message `id`, if any.
    pub fn page(&self, id: ItemId) -> Option<&Path> {
        self.pages.get(&id).map(PathBuf::as_path)
    }

    /// Deletes the pages of messages that no longer exist.
    pub(crate) fn prune(&mut self, exists: impl Fn(ItemId) -> bool) {
        if self.pages.is_empty() {
            return;
        }
        self.pages.retain(|id, path| {
            if exists(*id) {
                return true;
            }
            if let Err(e) = std::fs::remove_file(path.as_path())
                && e.kind() != std::io::ErrorKind::NotFound
            {
                log::warn!("View in Browser: {}: {e}", path.display());
            }
            false
        });
    }
}

/// A `file:` URL for `path`, percent-encoding everything a URL path cannot hold as is (`#`, `%`, `?`,
/// spaces, non-ASCII), so the browser opens exactly that file.
pub fn file_url(path: &Path) -> String {
    crate::cmd::insert::file_url(&path.display().to_string())
}
