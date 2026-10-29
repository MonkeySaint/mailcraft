//! The mail repository abstraction (`plan/storage-adr.md` §4.2).
//!
//! [`MailRepo`] is how the engine will reach mail once it no longer holds every item in memory. Stage S2
//! defines the read half and implements it on the in-memory [`MailTable`] (the future `MemRepo` of the
//! browser build, the sample mailbox and tests) with exactly today's behaviour. The write half (`insert`,
//! `update`, `move_to`, `begin`/`commit` with a `ChangeSet`) arrives with S3; list windows (`view`, `page`)
//! and persisted threads (`thread_members`) with S7; message sources (`raw`) with S5. A SQLite-backed
//! implementation follows in S6.
//!
//! The trait has no borrowing iterator over every item. The engine and UI still walk the whole table through
//! `MailTable::items`/`iter`/`ids`/`in_folder` in well over a hundred places; those move to [`MailRepo::scan`]
//! or to the S7 list windows before `Session` switches to the trait. The sync lookups by server id and by
//! Message-ID already go through methods the trait has ([`MailRepo::scan`], [`MailRepo::by_message_id`]).
//!
//! The trait is dyn-compatible (`Session` will hold a `Box<dyn MailRepo>`). Reads hand out a [`Shared`]:
//! a plain borrow for the in-memory table (no copy), or an `Arc` from a cache for a database backend.

use std::collections::BTreeMap;
use std::ops::{ControlFlow, Deref};
use std::sync::Arc;

use mailcraft_model::{Body, FolderCounts, FolderId, ItemId, MailItem, MailTable, ScanFilter};

use crate::Result;

/// A value read from a repository: borrowed from the repository itself, or shared from its cache.
#[derive(Debug)]
pub enum Shared<'a, T> {
    /// Borrowed from an in-memory repository.
    Borrowed(&'a T),
    /// Held in a cache (or freshly loaded) by a repository that does not keep everything in memory.
    Owned(Arc<T>),
}

impl<T> Deref for Shared<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        match self {
            Shared::Borrowed(t) => t,
            Shared::Owned(t) => t,
        }
    }
}

impl<T> Clone for Shared<'_, T> {
    fn clone(&self) -> Self {
        match self {
            Shared::Borrowed(t) => Shared::Borrowed(t),
            Shared::Owned(t) => Shared::Owned(Arc::clone(t)),
        }
    }
}

impl<T: PartialEq> PartialEq<T> for Shared<'_, T> {
    fn eq(&self, other: &T) -> bool {
        **self == *other
    }
}

/// Read access to the mail items of a profile.
///
/// Every method is total: an unknown id or folder reads as absent or empty, never as a panic.
pub trait MailRepo {
    /// The item stored under `id`.
    fn get(&self, id: ItemId) -> Option<Shared<'_, MailItem>>;
    /// Whether an item is stored under `id`.
    fn contains(&self, id: ItemId) -> bool;
    /// Number of items.
    fn len(&self) -> usize;
    /// Whether the repository holds no items.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Total and unread counts of `folder` (zero for an unknown folder).
    fn counts(&self, folder: FolderId) -> FolderCounts;
    /// The counts of every folder holding mail. Folders without mail are missing.
    fn all_counts(&self) -> BTreeMap<FolderId, FolderCounts>;
    /// Ids of the items filed in `folder`, ascending.
    fn ids_in(&self, folder: FolderId) -> Vec<ItemId>;
    /// Ids of the items whose Message-ID has the same normalised key as `msgid`, ascending. The key is
    /// [`mailcraft_model::message_id_key`]: surrounding space and angle brackets dropped, ASCII lower-case, on
    /// both the stored value and `msgid`; an empty key matches nothing. A backend indexes that key. A caller
    /// that needs the stored form exactly filters the items returned.
    fn by_message_id(&self, msgid: &str) -> Vec<ItemId>;
    /// The item in `folder` whose server id (IMAP UID, JMAP id) is `remote`; the lowest id when several are.
    fn by_remote_id(&self, folder: FolderId, remote: &str) -> Option<ItemId>;
    /// Up to `limit` ids of the items in `folder` received before `cutoff` (Unix seconds), oldest first.
    fn older_than(&self, folder: FolderId, cutoff: i64, limit: usize) -> Vec<ItemId>;
    /// Streams every item `filter` admits to `visit`, in id order, until it breaks. The escape hatch for
    /// queries without a dedicated method; a database backend reads in batches.
    fn scan(&self, filter: &ScanFilter, visit: &mut dyn FnMut(&MailItem) -> ControlFlow<()>) -> Result<()>;
    /// The decoded body of item `id` (`None` for an unknown id).
    fn body(&self, id: ItemId) -> Result<Option<Shared<'_, Body>>>;
}

impl MailRepo for MailTable {
    fn get(&self, id: ItemId) -> Option<Shared<'_, MailItem>> {
        MailTable::get(self, id).map(Shared::Borrowed)
    }
    fn contains(&self, id: ItemId) -> bool {
        MailTable::contains(self, id)
    }
    fn len(&self) -> usize {
        MailTable::len(self)
    }
    fn is_empty(&self) -> bool {
        MailTable::is_empty(self)
    }
    fn counts(&self, folder: FolderId) -> FolderCounts {
        MailTable::counts(self, folder)
    }
    fn all_counts(&self) -> BTreeMap<FolderId, FolderCounts> {
        MailTable::all_counts(self)
    }
    fn ids_in(&self, folder: FolderId) -> Vec<ItemId> {
        MailTable::ids_in(self, folder)
    }
    fn by_message_id(&self, msgid: &str) -> Vec<ItemId> {
        MailTable::by_message_id(self, msgid)
    }
    fn by_remote_id(&self, folder: FolderId, remote: &str) -> Option<ItemId> {
        MailTable::by_remote_id(self, folder, remote)
    }
    fn older_than(&self, folder: FolderId, cutoff: i64, limit: usize) -> Vec<ItemId> {
        MailTable::older_than(self, folder, cutoff, limit)
    }
    fn scan(&self, filter: &ScanFilter, visit: &mut dyn FnMut(&MailItem) -> ControlFlow<()>) -> Result<()> {
        MailTable::scan(self, filter, visit);
        Ok(())
    }
    fn body(&self, id: ItemId) -> Result<Option<Shared<'_, Body>>> {
        Ok(MailTable::body(self, id).map(Shared::Borrowed))
    }
}
