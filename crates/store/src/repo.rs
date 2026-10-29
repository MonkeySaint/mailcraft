//! The mail repository abstraction (`plan/storage-adr.md` §4.2).
//!
//! [`MailRepo`] is how the engine will reach mail once it no longer holds every item in memory. It is
//! implemented on the in-memory [`MailTable`] (the future `MemRepo` of the browser build, the sample mailbox
//! and tests) with exactly today's behaviour. Stage S2 defined the reads; S3 adds the writes and the change
//! records they land in ([`MailRepo::begin`], [`MailRepo::commit`], [`MailRepo::rollback`]): a commit hands
//! back the before-image of every item written since the matching begin ([`MailChanges`]), which is what the
//! engine journals, queues for the server and keeps as an undo step. List windows (`view`, `page`) and
//! persisted threads (`thread_members`) come with S7, and a SQLite-backed implementation with S6. Bodies and
//! sources are not in the repository: since S5 items refer to them by key, and the engine reads them from a
//! [`crate::BodyStore`] (`Session::body`), with its own byte-capped cache.
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

use mailcraft_model::{
    ChangeToken, FolderCounts, FolderId, ItemId, MailChanges, MailItem, MailTable, ScanFilter,
};

use crate::{Result, StoreError};

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

    // ---- writes (each recorded in the innermost open change record) ----

    /// Opens a change record. Records nest; see [`MailTable::begin`].
    fn begin(&mut self) -> ChangeToken;
    /// Closes record `token`: the before-image of every item written since it opened. An enclosing record
    /// keeps what it did not have. [`StoreError::NoChangeRecord`] when `token` is not open.
    fn commit(&mut self, token: ChangeToken) -> Result<MailChanges>;
    /// Closes record `token` and puts back every item written since it opened. Returns the items as they
    /// were just before. [`StoreError::NoChangeRecord`] when `token` is not open.
    fn rollback(&mut self, token: ChangeToken) -> Result<MailChanges>;
    /// Stores `item` under its id, returning the item it replaced.
    fn insert(&mut self, item: MailItem) -> Result<Option<MailItem>>;
    /// Changes item `id` in place; false when there is no such item.
    fn update(&mut self, id: ItemId, f: &mut dyn FnMut(&mut MailItem)) -> Result<bool>;
    /// Removes these items; returns those that existed, in the order given.
    fn remove(&mut self, ids: &[ItemId]) -> Result<Vec<MailItem>>;
    /// Files these items in `to` (the caller checks the folder accepts mail); returns the ids that moved
    /// (unknown ids and items already there are skipped).
    fn move_to(&mut self, ids: &[ItemId], to: FolderId) -> Result<Vec<ItemId>>;
    /// Marks these items read or unread; returns the ids that changed.
    fn set_read(&mut self, ids: &[ItemId], read: bool) -> Result<Vec<ItemId>>;
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
    fn begin(&mut self) -> ChangeToken {
        MailTable::begin(self)
    }
    fn commit(&mut self, token: ChangeToken) -> Result<MailChanges> {
        MailTable::commit(self, token).ok_or(StoreError::NoChangeRecord)
    }
    fn rollback(&mut self, token: ChangeToken) -> Result<MailChanges> {
        MailTable::rollback(self, token).ok_or(StoreError::NoChangeRecord)
    }
    fn insert(&mut self, item: MailItem) -> Result<Option<MailItem>> {
        Ok(MailTable::insert(self, item.id, item))
    }
    fn update(&mut self, id: ItemId, f: &mut dyn FnMut(&mut MailItem)) -> Result<bool> {
        Ok(MailTable::update(self, id, f))
    }
    fn remove(&mut self, ids: &[ItemId]) -> Result<Vec<MailItem>> {
        Ok(ids.iter().filter_map(|id| MailTable::remove(self, *id)).collect())
    }
    fn move_to(&mut self, ids: &[ItemId], to: FolderId) -> Result<Vec<ItemId>> {
        let mut moved = Vec::new();
        for id in ids {
            if MailTable::get(self, *id).is_some_and(|m| m.folder != to)
                && MailTable::update(self, *id, |m| m.folder = to)
            {
                moved.push(*id);
            }
        }
        Ok(moved)
    }
    fn set_read(&mut self, ids: &[ItemId], read: bool) -> Result<Vec<ItemId>> {
        let mut changed = Vec::new();
        for id in ids {
            if MailTable::get(self, *id).is_some_and(|m| m.read != read)
                && MailTable::update(self, *id, |m| m.read = read)
            {
                changed.push(*id);
            }
        }
        Ok(changed)
    }
}
