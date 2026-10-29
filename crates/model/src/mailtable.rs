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
//! [`MailTable::older_than`], [`MailTable::body`]) and the streaming [`MailTable::scan`]. The borrowing
//! iterators ([`MailTable::items`], [`MailTable::iter`], [`MailTable::ids`], [`MailTable::in_folder`]) and the
//! writes are inherent only. Their callers (well over a hundred outside tests) still assume every item is in
//! memory; they move when the trait grows list windows and an iteration method (S7) and the write half (S3),
//! before `Session` holds a `Box<dyn MailRepo>`. New code should prefer the trait's reads and `scan`.
//!
//! Message-IDs are matched by a normalised key ([`message_id_key`]: no surrounding space or angle brackets,
//! ASCII case ignored), the way the engine compares them, so an index on that key serves every caller.

use std::collections::{BTreeMap, BTreeSet, btree_map};
use std::fmt;
use std::ops::ControlFlow;

use serde::{Deserialize, Serialize};

use crate::{Body, FolderCounts, FolderId, ItemId, MailItem};

/// The mail items of a mailbox, keyed by id.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MailTable {
    map: BTreeMap<ItemId, MailItem>,
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
        MailTable { map }
    }
}

impl FromIterator<(ItemId, MailItem)> for MailTable {
    fn from_iter<I: IntoIterator<Item = (ItemId, MailItem)>>(iter: I) -> Self {
        MailTable { map: iter.into_iter().collect() }
    }
}

impl Extend<(ItemId, MailItem)> for MailTable {
    fn extend<I: IntoIterator<Item = (ItemId, MailItem)>>(&mut self, iter: I) {
        self.map.extend(iter);
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

    // ---- writes ----

    /// The item stored under `id`, to change in place.
    pub fn get_mut(&mut self, id: ItemId) -> Option<&mut MailItem> {
        self.map.get_mut(&id)
    }
    /// Stores `item` under `id`, returning the item it replaced. The key is taken as given (callers keep it
    /// equal to `item.id`; [`crate::Mailbox::validate`] reports a mismatch).
    pub fn insert(&mut self, id: ItemId, item: MailItem) -> Option<MailItem> {
        self.map.insert(id, item)
    }
    /// Removes and returns the item stored under `id`.
    pub fn remove(&mut self, id: ItemId) -> Option<MailItem> {
        self.map.remove(&id)
    }
    /// Keeps only the items for which `keep` returns true (full scan).
    pub fn retain(&mut self, mut keep: impl FnMut(ItemId, &mut MailItem) -> bool) {
        self.map.retain(|k, v| keep(*k, v));
    }
    /// Removes every item.
    pub fn clear(&mut self) {
        self.map.clear();
    }
    /// Every item, to change in place, in id order (full scan).
    pub fn items_mut(&mut self) -> ItemsMut<'_> {
        ItemsMut(self.map.values_mut())
    }
    /// Every `(id, item)` pair, to change items in place, in id order (full scan).
    pub fn iter_mut(&mut self) -> IterMut<'_> {
        IterMut(self.map.iter_mut())
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
