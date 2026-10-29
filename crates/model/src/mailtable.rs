//! [`MailTable`]: the mail items of a [`crate::Mailbox`], behind a read/write API instead of a bare map.
//!
//! Storage redesign stage S2 (`plan/storage-adr.md` §4.2) wraps the map: it is private, and every reader and
//! writer goes through the methods below. The table serialises exactly like the `BTreeMap<ItemId, MailItem>`
//! it replaces (`#[serde(transparent)]`), so snapshots, journals, data files and browser storage stay
//! byte-identical.
//!
//! Only part of this API is on the `MailRepo` trait (`mailcraft-store`) that a database-backed repository
//! will implement: the targeted reads ([`MailTable::get`], [`MailTable::counts`], [`MailTable::all_counts`],
//! [`MailTable::ids_in`], [`MailTable::by_message_id`], [`MailTable::by_remote_id`],
//! [`MailTable::older_than`], [`MailTable::body`]), the streaming [`MailTable::scan`], the item writes
//! ([`MailTable::insert`], [`MailTable::update`], [`MailTable::remove`]) and the change records
//! ([`MailTable::begin`], [`MailTable::commit`], [`MailTable::rollback`]). The borrowing iterators
//! ([`MailTable::items`], [`MailTable::iter`], [`MailTable::ids`], [`MailTable::in_folder`]) and the bulk
//! writes ([`MailTable::get_mut`], [`MailTable::update_where`], [`MailTable::retain`], [`MailTable::items_mut`])
//! are inherent only. Their callers (well over a hundred outside tests) still assume every item is in memory;
//! they move when the trait grows list windows and an iteration method (S7), before `Session` holds a
//! `Box<dyn MailRepo>`. New code should prefer the trait's methods.
//!
//! Message-IDs are matched by a normalised key ([`message_id_key`]: no surrounding space or angle brackets,
//! ASCII case ignored), the way the engine compares them, so an index on that key serves every caller.
//!
//! **Change records** (stage S3). [`MailTable::begin`] opens a record, and from then on the first write to
//! each item keeps its before-image: [`MailTable::commit`] hands them back as [`MailChanges`] and
//! [`MailTable::rollback`] puts them back. The engine journals, queues server changes and records undo steps
//! from that record instead of copying and comparing the whole mailbox. Records nest (a command running
//! another): a nested commit leaves a copy of its before-images with the enclosing record, which keeps its
//! own older ones. A record left open (a panic between begin and commit) is folded into the record below it
//! when that one closes. Cloning a table clones its open records with it, so a mailbox put back from a clone
//! taken inside a record still describes its changes; a table from elsewhere has none of them, and
//! [`MailTable::commit`] then reports the record as lost (`None`).

use std::collections::{BTreeMap, BTreeSet, btree_map};
use std::fmt;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::{Body, FolderCounts, FolderId, ItemId, MailItem};

/// The mail items of a mailbox, keyed by id.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MailTable {
    map: BTreeMap<ItemId, MailItem>,
    /// Open change records (never serialised, ignored by `==`).
    #[serde(skip)]
    log: ChangeLog,
}

impl PartialEq for MailTable {
    fn eq(&self, other: &Self) -> bool {
        self.map == other.map
    }
}

impl Eq for MailTable {}

/// Names an open change record ([`MailTable::begin`]). Tokens are unique in the process, so a record of one
/// table is never mistaken for one of another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChangeToken(u64);

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Most records open at once. A deeper `begin` first folds the innermost record into the one below it, so the
/// stack stays bounded even if something keeps opening records and never closes them.
const MAX_OPEN_RECORDS: usize = 64;

#[derive(Clone, Debug, Default)]
struct ChangeLog {
    frames: Vec<Frame>,
}

#[derive(Clone, Debug)]
struct Frame {
    token: ChangeToken,
    changes: MailChanges,
}

/// What a stretch of work did to a [`MailTable`]: the before-image of every item it touched (`None` for an
/// item that did not exist yet), in id order. An item may have been touched and left as it was;
/// [`MailChanges::changed`] and [`MailChanges::retain_changed`] compare with the table to tell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MailChanges {
    before: BTreeMap<ItemId, Option<MailItem>>,
}

impl MailChanges {
    /// No changes.
    pub fn new() -> MailChanges {
        MailChanges::default()
    }
    /// The changes turning `before` into `after`, found by comparing every item (for code that holds two
    /// whole tables, and for tests).
    pub fn between(before: &MailTable, after: &MailTable) -> MailChanges {
        let mut out = MailChanges::new();
        for (id, b) in &before.map {
            if after.map.get(id) != Some(b) {
                out.before.insert(*id, Some(b.clone()));
            }
        }
        for id in after.map.keys() {
            if !before.map.contains_key(id) {
                out.before.insert(*id, None);
            }
        }
        out
    }
    /// Whether no item was touched.
    pub fn is_empty(&self) -> bool {
        self.before.is_empty()
    }
    /// Number of items touched.
    pub fn len(&self) -> usize {
        self.before.len()
    }
    /// Whether item `id` was touched.
    pub fn contains(&self, id: ItemId) -> bool {
        self.before.contains_key(&id)
    }
    /// The before-image of item `id`: `None` when it was not touched, `Some(None)` when it did not exist.
    pub fn before(&self, id: ItemId) -> Option<Option<&MailItem>> {
        self.before.get(&id).map(Option::as_ref)
    }
    /// Ids of the items touched, ascending.
    pub fn ids(&self) -> impl Iterator<Item = ItemId> + '_ {
        self.before.keys().copied()
    }
    /// `(id, before-image)` of every item touched, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (ItemId, Option<&MailItem>)> + '_ {
        self.before.iter().map(|(id, b)| (*id, b.as_ref()))
    }
    /// `(id, before, now)` of the items that differ from their before-image in `now`, in id order.
    pub fn changed<'a>(
        &'a self,
        now: &'a MailTable,
    ) -> impl Iterator<Item = (ItemId, Option<&'a MailItem>, Option<&'a MailItem>)> + 'a {
        self.before.iter().filter_map(move |(id, b)| {
            let a = now.map.get(id);
            (b.as_ref() != a).then_some((*id, b.as_ref(), a))
        })
    }
    /// Drops the items `now` holds exactly as their before-image (touched, then left as they were).
    pub fn retain_changed(&mut self, now: &MailTable) {
        self.before.retain(|id, b| b.as_ref() != now.map.get(id));
    }
    /// Records `before` as the before-image of `id` unless one is recorded already (the older one wins).
    pub fn record(&mut self, id: ItemId, before: Option<MailItem>) {
        self.before.entry(id).or_insert(before);
    }
    /// Adds changes made after these: their before-images count only for items not touched here.
    pub fn absorb(&mut self, later: MailChanges) {
        for (id, b) in later.before {
            self.before.entry(id).or_insert(b);
        }
    }
    /// Estimated memory of the before-images ([`MailItem::approx_bytes`] plus the map's own cost).
    pub fn approx_bytes(&self) -> usize {
        const PER_ENTRY: usize = std::mem::size_of::<ItemId>() + std::mem::size_of::<Option<MailItem>>() + 16;
        self.before
            .values()
            .map(|b| b.as_ref().map_or(0, MailItem::approx_bytes).saturating_add(PER_ENTRY))
            .fold(0usize, usize::saturating_add)
    }
}

impl IntoIterator for MailChanges {
    type Item = (ItemId, Option<MailItem>);
    type IntoIter = btree_map::IntoIter<ItemId, Option<MailItem>>;
    fn into_iter(self) -> Self::IntoIter {
        self.before.into_iter()
    }
}

impl FromIterator<(ItemId, Option<MailItem>)> for MailChanges {
    /// The first image given for an id wins, like [`MailChanges::record`].
    fn from_iter<I: IntoIterator<Item = (ItemId, Option<MailItem>)>>(iter: I) -> Self {
        let mut out = MailChanges::new();
        for (id, b) in iter {
            out.record(id, b);
        }
        out
    }
}

/// Which items [`MailTable::scan`] visits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ScanFilter {
    /// Every item.
    #[default]
    All,
    /// Items filed in one folder.
    Folder(FolderId),
    /// Items filed in any of these folders.
    Folders(BTreeSet<FolderId>),
}

impl ScanFilter {
    /// Whether an item filed in `folder` passes.
    pub fn admits(&self, folder: FolderId) -> bool {
        match self {
            ScanFilter::All => true,
            ScanFilter::Folder(f) => *f == folder,
            ScanFilter::Folders(set) => set.contains(&folder),
        }
    }
}

impl fmt::Debug for MailTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.map.fmt(f)
    }
}

impl From<BTreeMap<ItemId, MailItem>> for MailTable {
    fn from(map: BTreeMap<ItemId, MailItem>) -> Self {
        MailTable { map, log: ChangeLog::default() }
    }
}

impl FromIterator<(ItemId, MailItem)> for MailTable {
    fn from_iter<I: IntoIterator<Item = (ItemId, MailItem)>>(iter: I) -> Self {
        MailTable { map: iter.into_iter().collect(), log: ChangeLog::default() }
    }
}

impl Extend<(ItemId, MailItem)> for MailTable {
    fn extend<I: IntoIterator<Item = (ItemId, MailItem)>>(&mut self, iter: I) {
        for (id, item) in iter {
            self.insert(id, item);
        }
    }
}

impl<'a> IntoIterator for &'a MailTable {
    type Item = (ItemId, &'a MailItem);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl IntoIterator for MailTable {
    type Item = (ItemId, MailItem);
    type IntoIter = btree_map::IntoIter<ItemId, MailItem>;
    fn into_iter(self) -> Self::IntoIter {
        self.map.into_iter()
    }
}

/// Declares a borrowing iterator newtype. Concrete types (not `impl Iterator`) so a temporary iterator in a
/// call's arguments ends its borrow as early as the map's own iterators do.
macro_rules! table_iter {
    ($(#[$doc:meta])* $name:ident, $inner:ty, $item:ty, $map:expr) => {
        $(#[$doc])*
        pub struct $name<'a>($inner);

        impl<'a> Iterator for $name<'a> {
            type Item = $item;
            fn next(&mut self) -> Option<$item> {
                self.0.next().map($map)
            }
            fn size_hint(&self) -> (usize, Option<usize>) {
                self.0.size_hint()
            }
        }

        impl<'a> DoubleEndedIterator for $name<'a> {
            fn next_back(&mut self) -> Option<$item> {
                self.0.next_back().map($map)
            }
        }

        impl ExactSizeIterator for $name<'_> {}
        impl std::iter::FusedIterator for $name<'_> {}
    };
}

table_iter!(
    /// `(id, item)` pairs in id order ([`MailTable::iter`]).
    #[derive(Clone)]
    Iter,
    btree_map::Iter<'a, ItemId, MailItem>,
    (ItemId, &'a MailItem),
    |(k, v)| (*k, v)
);
table_iter!(
    /// Ids in ascending order ([`MailTable::ids`]).
    #[derive(Clone)]
    Ids,
    btree_map::Keys<'a, ItemId, MailItem>,
    ItemId,
    |k| *k
);
table_iter!(
    /// Items in id order ([`MailTable::items`]).
    #[derive(Clone)]
    Items,
    btree_map::Values<'a, ItemId, MailItem>,
    &'a MailItem,
    |v| v
);
table_iter!(
    /// Items to change in place, in id order ([`MailTable::items_mut`]).
    ItemsMut,
    btree_map::ValuesMut<'a, ItemId, MailItem>,
    &'a mut MailItem,
    |v| v
);
table_iter!(
    /// `(id, item)` pairs to change items in place, in id order ([`MailTable::iter_mut`]).
    IterMut,
    btree_map::IterMut<'a, ItemId, MailItem>,
    (ItemId, &'a mut MailItem),
    |(k, v)| (*k, v)
);

/// Items filed in one folder, in id order ([`MailTable::in_folder`]).
#[derive(Clone)]
pub struct InFolder<'a> {
    items: btree_map::Values<'a, ItemId, MailItem>,
    folder: FolderId,
}

impl<'a> Iterator for InFolder<'a> {
    type Item = &'a MailItem;
    fn next(&mut self) -> Option<&'a MailItem> {
        let folder = self.folder;
        self.items.find(|m| m.folder == folder)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, self.items.size_hint().1)
    }
}

impl DoubleEndedIterator for InFolder<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let folder = self.folder;
        self.items.rfind(|m| m.folder == folder)
    }
}

impl std::iter::FusedIterator for InFolder<'_> {}

impl MailTable {
    /// An empty table.
    pub fn new() -> MailTable {
        MailTable::default()
    }

    // ---- reads ----

    /// The item stored under `id`.
    pub fn get(&self, id: ItemId) -> Option<&MailItem> {
        self.map.get(&id)
    }
    /// Whether an item is stored under `id`.
    pub fn contains(&self, id: ItemId) -> bool {
        self.map.contains_key(&id)
    }
    /// Number of items.
    pub fn len(&self) -> usize {
        self.map.len()
    }
    /// Whether the table holds no items.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    /// Every id, in ascending order (full scan).
    pub fn ids(&self) -> Ids<'_> {
        Ids(self.map.keys())
    }
    /// Every item, in id order (full scan).
    pub fn items(&self) -> Items<'_> {
        Items(self.map.values())
    }
    /// Every `(id, item)` pair, in id order (full scan).
    pub fn iter(&self) -> Iter<'_> {
        Iter(self.map.iter())
    }
    /// Items filed in `folder`, in id order.
    pub fn in_folder(&self, folder: FolderId) -> InFolder<'_> {
        InFolder { items: self.map.values(), folder }
    }
    /// Ids of the items filed in `folder`, ascending.
    pub fn ids_in(&self, folder: FolderId) -> Vec<ItemId> {
        self.in_folder(folder).map(|m| m.id).collect()
    }
    /// Total and unread counts of `folder`.
    pub fn counts(&self, folder: FolderId) -> FolderCounts {
        let mut c = FolderCounts::default();
        for m in self.in_folder(folder) {
            c.add(m.read);
        }
        c
    }
    /// The counts of every folder holding mail, in one pass. Folders without mail are missing.
    pub fn all_counts(&self) -> BTreeMap<FolderId, FolderCounts> {
        let mut out: BTreeMap<FolderId, FolderCounts> = BTreeMap::new();
        for m in self.map.values() {
            out.entry(m.folder).or_default().add(m.read);
        }
        out
    }
    /// Ids of the items whose Message-ID has the same [`message_id_key`] as `msgid`, ascending: surrounding
    /// space and angle brackets are ignored on both sides and ASCII letters compare case-insensitively. An
    /// empty key matches nothing. A caller that needs the stored form exactly compares the `message_id` of
    /// the items returned.
    pub fn by_message_id(&self, msgid: &str) -> Vec<ItemId> {
        let want = message_id_core(msgid);
        if want.is_empty() {
            return Vec::new();
        }
        self.map
            .values()
            .filter(|m| {
                m.message_id.as_deref().is_some_and(|mid| message_id_core(mid).eq_ignore_ascii_case(want))
            })
            .map(|m| m.id)
            .collect()
    }
    /// The item in `folder` whose server id (IMAP UID, JMAP id) is `remote`; the lowest id when several are.
    pub fn by_remote_id(&self, folder: FolderId, remote: &str) -> Option<ItemId> {
        self.in_folder(folder).find(|m| m.remote_id.as_deref() == Some(remote)).map(|m| m.id)
    }
    /// Up to `limit` ids of the items in `folder` received before `cutoff` (Unix seconds), oldest first (ties
    /// by id).
    pub fn older_than(&self, folder: FolderId, cutoff: i64, limit: usize) -> Vec<ItemId> {
        let mut old: Vec<(i64, ItemId)> = self
            .in_folder(folder)
            .filter(|m| m.received.unix < cutoff)
            .map(|m| (m.received.unix, m.id))
            .collect();
        old.sort_unstable();
        old.truncate(limit);
        old.into_iter().map(|(_, id)| id).collect()
    }
    /// Calls `visit` on every item `filter` admits, in id order, until it breaks.
    pub fn scan(&self, filter: &ScanFilter, visit: &mut dyn FnMut(&MailItem) -> ControlFlow<()>) {
        for m in self.map.values().filter(|m| filter.admits(m.folder)) {
            if visit(m).is_break() {
                break;
            }
        }
    }
    /// The decoded body of item `id`.
    pub fn body(&self, id: ItemId) -> Option<&Body> {
        self.map.get(&id).map(|m| &m.body)
    }

    // ---- writes (each one recorded in the innermost open change record, if any) ----

    /// The item stored under `id`, to change in place. Recorded as touched even if the caller then changes
    /// nothing.
    pub fn get_mut(&mut self, id: ItemId) -> Option<&mut MailItem> {
        if self.map.contains_key(&id) {
            self.note(id);
        }
        self.map.get_mut(&id)
    }
    /// Changes item `id` in place with `f`; false when there is no such item.
    pub fn update(&mut self, id: ItemId, f: impl FnOnce(&mut MailItem)) -> bool {
        match self.get_mut(id) {
            Some(m) => {
                f(m);
                true
            }
            None => false,
        }
    }
    /// Changes every item `filter` admits with `f` (full scan; only the admitted items are recorded).
    /// Returns how many were admitted.
    pub fn update_where(
        &mut self,
        mut filter: impl FnMut(&MailItem) -> bool,
        mut f: impl FnMut(&mut MailItem),
    ) -> usize {
        let mut n = 0usize;
        let mut frame = self.log.frames.last_mut();
        for (id, m) in self.map.iter_mut() {
            if !filter(m) {
                continue;
            }
            if let Some(fr) = frame.as_mut()
                && !fr.changes.contains(*id)
            {
                fr.changes.record(*id, Some(m.clone()));
            }
            f(m);
            n = n.saturating_add(1);
        }
        n
    }
    /// Stores `item` under `id`, returning the item it replaced. The key is taken as given (callers keep it
    /// equal to `item.id`; [`crate::Mailbox::validate`] reports a mismatch).
    pub fn insert(&mut self, id: ItemId, item: MailItem) -> Option<MailItem> {
        self.note(id);
        self.map.insert(id, item)
    }
    /// Removes and returns the item stored under `id`.
    pub fn remove(&mut self, id: ItemId) -> Option<MailItem> {
        let old = self.map.remove(&id);
        if let Some(fr) = self.log.frames.last_mut()
            && old.is_some()
            && !fr.changes.contains(id)
        {
            fr.changes.record(id, old.clone());
        }
        old
    }
    /// Keeps only the items for which `keep` returns true (full scan; only the removed items are recorded).
    pub fn retain(&mut self, mut keep: impl FnMut(ItemId, &MailItem) -> bool) {
        let Some(fr) = self.log.frames.last_mut() else {
            self.map.retain(|k, v| keep(*k, v));
            return;
        };
        let gone: Vec<ItemId> = self.map.iter().filter(|(k, v)| !keep(**k, v)).map(|(k, _)| *k).collect();
        for id in gone {
            let old = self.map.remove(&id);
            fr.changes.record(id, old);
        }
    }
    /// Removes every item.
    pub fn clear(&mut self) {
        let map = std::mem::take(&mut self.map);
        if let Some(fr) = self.log.frames.last_mut() {
            for (id, m) in map {
                fr.changes.record(id, Some(m));
            }
        }
    }
    /// Every item, to change in place, in id order (full scan). While a change record is open every item is
    /// recorded as touched (a copy of each): prefer [`MailTable::update_where`] or [`MailTable::update`].
    pub fn items_mut(&mut self) -> ItemsMut<'_> {
        self.note_all();
        ItemsMut(self.map.values_mut())
    }
    /// Every `(id, item)` pair, to change items in place, in id order (full scan). Recorded like
    /// [`MailTable::items_mut`].
    pub fn iter_mut(&mut self) -> IterMut<'_> {
        self.note_all();
        IterMut(self.map.iter_mut())
    }

    // ---- change records ----

    /// Opens a change record: from now on the first write to each item keeps its before-image.
    pub fn begin(&mut self) -> ChangeToken {
        let token = ChangeToken(NEXT_TOKEN.fetch_add(1, Ordering::Relaxed));
        if self.log.frames.len() >= MAX_OPEN_RECORDS {
            self.fold_above(self.log.frames.len().saturating_sub(2));
        }
        self.log.frames.push(Frame { token, changes: MailChanges::new() });
        token
    }
    /// Whether a change record is open.
    pub fn is_recording(&self) -> bool {
        !self.log.frames.is_empty()
    }
    /// Whether record `token` is open here.
    pub fn is_open(&self, token: ChangeToken) -> bool {
        self.log.frames.iter().any(|f| f.token == token)
    }
    /// Drops every open record without putting anything back, and returns how many there were. For records
    /// left open by code that never closed them (a panic between `begin` and `commit` with no enclosing
    /// record): without this the oldest of them would keep a copy of every item written from then on.
    pub fn abandon_all(&mut self) -> usize {
        let n = self.log.frames.len();
        self.log.frames.clear();
        n
    }
    /// Closes record `token` and returns what was written since it opened. Records opened after it and left
    /// open are folded into it first. The enclosing record, if any, keeps a copy of the before-images it did
    /// not have. `None` when `token` is not open here (the table was replaced by one without that record).
    pub fn commit(&mut self, token: ChangeToken) -> Option<MailChanges> {
        let pos = self.log.frames.iter().rposition(|f| f.token == token)?;
        self.fold_above(pos);
        let frame = self.log.frames.pop()?;
        if let Some(parent) = self.log.frames.last_mut() {
            for (id, b) in frame.changes.iter() {
                if !parent.changes.contains(id) {
                    parent.changes.record(id, b.cloned());
                }
            }
        }
        Some(frame.changes)
    }
    /// Closes record `token` and puts every item it touched back as it was when the record opened. Returns
    /// the items as they were just before (the changes undone). `None` when `token` is not open here.
    pub fn rollback(&mut self, token: ChangeToken) -> Option<MailChanges> {
        let pos = self.log.frames.iter().rposition(|f| f.token == token)?;
        self.fold_above(pos);
        let frame = self.log.frames.pop()?;
        // Back to the state at `begin`: the enclosing record, if any, already describes that state.
        let mut undone = MailChanges::new();
        for (id, b) in frame.changes {
            let now = match b {
                Some(item) => self.map.insert(id, item),
                None => self.map.remove(&id),
            };
            undone.before.insert(id, now);
        }
        Some(undone)
    }
    /// Puts before-images back (undo, redo): each item becomes its image, or goes when the image is `None`.
    /// Returns the items as they were just before, so the step can be reverted in turn. Recorded in the open
    /// change record, if any, like any other write.
    pub fn restore(&mut self, images: MailChanges) -> MailChanges {
        let mut replaced = MailChanges::new();
        for (id, b) in images {
            let now = match b {
                Some(item) => self.map.insert(id, item),
                None => self.map.remove(&id),
            };
            if let Some(fr) = self.log.frames.last_mut()
                && !fr.changes.contains(id)
            {
                fr.changes.record(id, now.clone());
            }
            replaced.before.insert(id, now);
        }
        replaced
    }

    /// Keeps the before-image of `id` in the innermost open record (once).
    fn note(&mut self, id: ItemId) {
        if let Some(fr) = self.log.frames.last_mut()
            && !fr.changes.contains(id)
        {
            fr.changes.record(id, self.map.get(&id).cloned());
        }
    }
    /// Records every item as touched (bulk mutable access).
    fn note_all(&mut self) {
        if let Some(fr) = self.log.frames.last_mut() {
            for (id, m) in &self.map {
                if !fr.changes.contains(*id) {
                    fr.changes.record(*id, Some(m.clone()));
                }
            }
        }
    }
    /// Folds every record above position `pos` into the one at `pos` (records left open by code that never
    /// closed them). The lower record's older before-images win.
    fn fold_above(&mut self, pos: usize) {
        while self.log.frames.len() > pos.saturating_add(1) {
            let Some(top) = self.log.frames.pop() else { break };
            if let Some(below) = self.log.frames.last_mut() {
                below.changes.absorb(top.changes);
            }
        }
    }
}

/// `msgid` without surrounding space and angle brackets (any number of `<` before and `>` after).
fn message_id_core(msgid: &str) -> &str {
    msgid.trim().trim_start_matches('<').trim_end_matches('>').trim()
}

/// The key Message-IDs are matched by ([`MailTable::by_message_id`]): trimmed, without angle brackets, ASCII
/// lower-case. `" <X@Example.org> "` and `"x@example.org"` have the same key.
pub fn message_id_key(msgid: &str) -> String {
    message_id_core(msgid).to_ascii_lowercase()
}
