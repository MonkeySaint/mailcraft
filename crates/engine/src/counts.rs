//! Memoised folder counts (issue #25).
//!
//! The folder pane shows a badge for every visible folder on every frame. Counting a folder scans all mail,
//! so the pane used to cost (visible folders × messages) per frame. The counts of every folder are now
//! worked out in one pass and kept until the mailbox changes: [`Session::mailbox_revision`] goes up whenever
//! the session journals a change, runs a mutating command or restores the mailbox (undo, redo, a failed
//! command, a failed sync batch). Code that changes `Session::mailbox` directly, outside those paths, calls
//! [`Session::mailbox_changed`]. As a backstop the memo is also dropped when the number of messages changed.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use mailcraft_model::{FolderCounts, FolderId};

use crate::Session;

/// The counts of every folder for one mailbox revision.
#[derive(Debug)]
struct Memo {
    revision: u64,
    mail_len: usize,
    counts: Arc<BTreeMap<FolderId, FolderCounts>>,
}

/// Folder counts kept between frames (interior mutability: queries take `&Session`).
#[derive(Debug, Default)]
pub struct CountsMemo {
    memo: Mutex<Option<Memo>>,
    /// Times the counts were worked out (a full pass over all mail), for tests.
    passes: std::sync::atomic::AtomicU64,
}

impl Session {
    /// A number that changes whenever the mailbox may have changed (see [`crate::counts`]).
    pub fn mailbox_revision(&self) -> u64 {
        self.revision
    }

    /// Notes that the mailbox changed outside a command, the journal or a restore, so memoised views of it
    /// (the folder counts) are worked out again.
    pub fn mailbox_changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Mail counts of every folder, worked out once per mailbox revision.
    pub fn all_folder_counts(&self) -> Arc<BTreeMap<FolderId, FolderCounts>> {
        let mut slot = self.counts.memo.lock().unwrap_or_else(PoisonError::into_inner);
        let mail_len = self.mailbox.mail.len();
        if let Some(m) = slot.as_ref()
            && m.revision == self.revision
            && m.mail_len == mail_len
        {
            return Arc::clone(&m.counts);
        }
        let counts = Arc::new(self.mailbox.folder_counts());
        self.counts.passes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *slot = Some(Memo { revision: self.revision, mail_len, counts: Arc::clone(&counts) });
        counts
    }

    /// Mail counts of one folder (zero for a folder without mail or one that does not exist).
    pub fn folder_counts(&self, folder: FolderId) -> FolderCounts {
        self.all_folder_counts().get(&folder).copied().unwrap_or_default()
    }

    /// How many times the folder counts were worked out from scratch (tests and benchmarks).
    pub fn folder_count_passes(&self) -> u64 {
        self.counts.passes.load(std::sync::atomic::Ordering::Relaxed)
    }
}
