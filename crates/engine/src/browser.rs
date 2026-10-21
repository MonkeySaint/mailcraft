//! The web pages View in Browser writes (`mail.viewInBrowser`).
//!
//! Each session keeps its pages in a folder of its own: a new, uniquely named folder that only this user
//! can open (mode 0700 on Unix), created on first use under the profile's `browser` folder, or the system
//! temporary folder without a saved profile. A page is deleted when its message is gone for good (deleted
//! permanently, Deleted Items emptied, removed by a sync), and the whole folder when the session ends.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mailcraft_model::ItemId;

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
        b.prefix("mailcraft-pages-");
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
