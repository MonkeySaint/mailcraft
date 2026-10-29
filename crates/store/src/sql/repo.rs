//! [`SqlRepo`]: the [`MailRepo`] over the `mail` table of `store.db` (storage stage S6).
//!
//! Reads go through the shared `MetaCache` ([`super::cache`]). Every write runs in a savepoint and keeps
//! `folder_counts` and the body reference counts in the same transaction ([`super::rows`]). A change record
//! ([`MailRepo::begin`]) is a savepoint too: [`MailRepo::commit`] releases it (the outermost one commits, one
//! fsync for the whole command) and [`MailRepo::rollback`] rolls the database back to it, while the
//! before-images are kept in memory exactly as [`mailcraft_model::MailTable`] keeps them, so both
//! repositories answer the same (`tests/repo_contract.rs` runs one suite against both).
//!
//! Ids are `u64` in the model and 64-bit integers in SQLite: the same bits, so ids past `i64::MAX` are
//! stored as negative numbers; every ordered answer is put back in `u64` order.
//!
//! One writer per database: change records are savepoints on the shared connection, so two repositories
//! writing through one [`Db`] would close each other's records.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::sync::Arc;

use mailcraft_model::{
    ChangeToken, FolderCounts, FolderId, ItemId, MailChanges, MailItem, ScanFilter, message_id_key,
};
use rusqlite::{Connection, OptionalExtension, params};

use super::{Db, count, from_sql_id, in_savepoint, rollback_savepoint, rows, to_sql_id};
use crate::repo::{MailRepo, Shared};
use crate::{Result, StoreError};

/// Rows read per batch while scanning.
const SCAN_BATCH: usize = 256;
/// Most records open at once (as [`mailcraft_model::MailTable`]).
const MAX_OPEN_RECORDS: usize = 64;

#[derive(Debug)]
struct Frame {
    token: ChangeToken,
    changes: MailChanges,
    /// The savepoint of the record, `None` when it could not be opened (writes then commit one by one and a
    /// rollback writes the before-images back).
    savepoint: Option<String>,
}

/// The mail of a SQLite profile.
#[derive(Debug)]
pub struct SqlRepo {
    db: Db,
    frames: Vec<Frame>,
}

impl SqlRepo {
    /// A repository over `db`'s mail.
    pub fn new(db: Db) -> SqlRepo {
        SqlRepo { db, frames: Vec::new() }
    }

    /// The database.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Whether a change record is open.
    pub fn is_recording(&self) -> bool {
        !self.frames.is_empty()
    }

    /// Closes every open record, keeping what was written (see [`mailcraft_model::MailTable::abandon_all`]).
    /// Returns how many there were.
    pub fn abandon_all(&mut self) -> usize {
        let n = self.frames.len();
        if let Some(sp) = self.frames.iter().find_map(|f| f.savepoint.clone()) {
            let conn = self.db.conn();
            if let Err(e) = conn.execute_batch(&format!("RELEASE {sp}")) {
                log::error!("store.db: could not close the open change records: {e}");
            }
        }
        self.frames.clear();
        n
    }

    /// Cached items and their estimated bytes.
    pub fn cache_stats(&self) -> (usize, usize) {
        let c = self.db.cache();
        (c.entries(), c.bytes())
    }

    /// Empties the item cache.
    pub fn clear_cache(&self) {
        self.db.cache().clear();
    }

    /// Item `id` read from the database (or the cache); errors are reported, not hidden.
    pub fn load(&self, id: ItemId) -> Result<Option<Arc<MailItem>>> {
        if let Some(m) = self.db.cache().get(id) {
            return Ok(Some(m));
        }
        let sql_id = to_sql_id(id.0);
        let meta = {
            let conn = self.db.conn();
            rows::meta(&conn, sql_id)?
        };
        let Some(meta) = meta else { return Ok(None) };
        let item = Arc::new(rows::decode(sql_id, &meta)?);
        self.db.cache().put(&item);
        Ok(Some(item))
    }

    /// Keeps the before-image of `id` in the innermost open record (once).
    fn note(&mut self, id: ItemId, before: Option<&MailItem>) {
        if let Some(fr) = self.frames.last_mut()
            && !fr.changes.contains(id)
        {
            fr.changes.record(id, before.cloned());
        }
    }

    /// Runs `f` in a savepoint of its own (inside the open record's, if any).
    fn write<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let name = self.db.savepoint_name("w");
        let conn = self.db.conn();
        in_savepoint(&conn, &name, f)
    }

    /// Stores `item` and refreshes the cache.
    fn store(&self, conn: &Connection, item: &MailItem) -> Result<()> {
        let meta = rows::encode(item)?;
        rows::put(conn, item, &meta)?;
        Ok(())
    }

    fn uncache(&self, ids: impl IntoIterator<Item = ItemId>) {
        let mut c = self.db.cache();
        for id in ids {
            c.remove(id);
        }
    }

    /// Folds every record above position `pos` into the one at `pos` (records left open). Their savepoints
    /// stay nested inside the lower one's and close with it.
    fn fold_above(&mut self, pos: usize) {
        while self.frames.len() > pos.saturating_add(1) {
            let Some(top) = self.frames.pop() else { break };
            if let Some(below) = self.frames.last_mut() {
                below.changes.absorb(top.changes);
                if below.savepoint.is_none() {
                    below.savepoint = top.savepoint;
                }
            }
        }
    }

    /// Writes before-images back (a rollback whose savepoint is gone). Returns the items as they were.
    fn put_back(&mut self, images: MailChanges) -> Result<MailChanges> {
        let mut now: Vec<(ItemId, Option<MailItem>)> = Vec::with_capacity(images.len());
        for id in images.ids() {
            now.push((id, self.load(id)?.map(|m| (*m).clone())));
        }
        let ids: Vec<ItemId> = images.ids().collect();
        let res = self.write(|c| {
            for (id, b) in images {
                match b {
                    Some(item) => self.store(c, &item)?,
                    None => {
                        rows::delete(c, to_sql_id(id.0))?;
                    }
                }
            }
            Ok(())
        });
        self.uncache(ids);
        res?;
        Ok(now.into_iter().collect())
    }

    /// Ids of the rows matching `sql` (one `?1` parameter), in `u64` order.
    fn ids_where(&self, sql: &str, param: impl rusqlite::ToSql) -> Result<Vec<ItemId>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare_cached(sql)?;
        let mut out: Vec<ItemId> = Vec::new();
        let mut rows = stmt.query([param])?;
        while let Some(r) = rows.next()? {
            out.push(ItemId(from_sql_id(r.get(0)?)));
        }
        out.sort_unstable();
        Ok(out)
    }

    fn try_ids_in(&self, folder: FolderId) -> Result<Vec<ItemId>> {
        self.ids_where("SELECT id FROM mail WHERE folder = ?1", to_sql_id(folder.0))
    }

    /// Visits the items `ids` (in the order given) read in batches, without holding the connection while
    /// `visit` runs.
    fn visit_ids(&self, ids: &[ItemId], visit: &mut dyn FnMut(&MailItem) -> ControlFlow<()>) -> Result<()> {
        for chunk in ids.chunks(SCAN_BATCH) {
            let mut batch: Vec<MailItem> = Vec::with_capacity(chunk.len());
            {
                let conn = self.db.conn();
                for id in chunk {
                    let sql_id = to_sql_id(id.0);
                    if let Some(meta) = rows::meta(&conn, sql_id)? {
                        batch.push(rows::decode(sql_id, &meta)?);
                    }
                }
            }
            for m in &batch {
                if visit(m).is_break() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Visits every row with an id in `lo..=hi` (SQLite integers) in id order. Returns whether `visit` broke.
    fn visit_range(
        &self,
        lo: i64,
        hi: i64,
        visit: &mut dyn FnMut(&MailItem) -> ControlFlow<()>,
    ) -> Result<bool> {
        let mut from = lo;
        loop {
            let mut batch: Vec<MailItem> = Vec::with_capacity(SCAN_BATCH);
            {
                let conn = self.db.conn();
                let mut stmt = conn.prepare_cached(
                    "SELECT id, meta FROM mail WHERE id >= ?1 AND id <= ?2 ORDER BY id LIMIT ?3",
                )?;
                let mut rows = stmt.query(params![from, hi, SCAN_BATCH as i64])?;
                while let Some(r) = rows.next()? {
                    let id: i64 = r.get(0)?;
                    let meta: String = r.get(1)?;
                    batch.push(rows::decode(id, &meta)?);
                }
            }
            let last = batch.last().map(|m| to_sql_id(m.id.0));
            let full = batch.len() == SCAN_BATCH;
            for m in &batch {
                if visit(m).is_break() {
                    return Ok(true);
                }
            }
            match last {
                Some(l) if full && l < hi => from = l.saturating_add(1),
                _ => return Ok(false),
            }
        }
    }
}

fn counts_of(conn: &Connection, folder: FolderId) -> Result<FolderCounts> {
    let row = conn
        .prepare_cached("SELECT total, unread FROM folder_counts WHERE folder = ?1")?
        .query_row([to_sql_id(folder.0)], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .optional()?;
    Ok(row.map_or_else(FolderCounts::default, |(t, u)| FolderCounts { total: count(t), unread: count(u) }))
}

/// Reports a read error and turns it into the "absent" answer the trait's infallible reads give.
fn reported<T>(what: &str, r: Result<T>, absent: T) -> T {
    r.unwrap_or_else(|e| {
        log::error!("store.db: {what}: {e}");
        absent
    })
}

impl MailRepo for SqlRepo {
    fn get(&self, id: ItemId) -> Option<Shared<'_, MailItem>> {
        reported("reading a mail item", self.load(id), None).map(Shared::Owned)
    }

    fn contains(&self, id: ItemId) -> bool {
        if self.db.cache().get(id).is_some() {
            return true;
        }
        let conn = self.db.conn();
        let r = conn
            .prepare_cached("SELECT 1 FROM mail WHERE id = ?1")
            .and_then(|mut s| s.query_row([to_sql_id(id.0)], |_| Ok(())).optional())
            .map_err(StoreError::from);
        reported("looking up a mail item", r, None).is_some()
    }

    fn len(&self) -> usize {
        let conn = self.db.conn();
        let r = conn
            .query_row("SELECT coalesce(sum(total), 0) FROM folder_counts", [], |r| r.get::<_, i64>(0))
            .map_err(StoreError::from);
        count(reported("counting mail", r, 0))
    }

    fn counts(&self, folder: FolderId) -> FolderCounts {
        let conn = self.db.conn();
        reported("reading folder counts", counts_of(&conn, folder), FolderCounts::default())
    }

    fn all_counts(&self) -> BTreeMap<FolderId, FolderCounts> {
        let read = || -> Result<BTreeMap<FolderId, FolderCounts>> {
            let conn = self.db.conn();
            let mut stmt =
                conn.prepare_cached("SELECT folder, total, unread FROM folder_counts WHERE total > 0")?;
            let mut out = BTreeMap::new();
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                out.insert(
                    FolderId(from_sql_id(r.get(0)?)),
                    FolderCounts { total: count(r.get(1)?), unread: count(r.get(2)?) },
                );
            }
            Ok(out)
        };
        reported("reading folder counts", read(), BTreeMap::new())
    }

    fn ids_in(&self, folder: FolderId) -> Vec<ItemId> {
        reported("listing a folder", self.try_ids_in(folder), Vec::new())
    }

    fn by_message_id(&self, msgid: &str) -> Vec<ItemId> {
        let key = message_id_key(msgid);
        if key.is_empty() {
            return Vec::new();
        }
        reported(
            "looking up a Message-ID",
            self.ids_where("SELECT id FROM mail WHERE msgid_key = ?1", key),
            Vec::new(),
        )
    }

    fn by_remote_id(&self, folder: FolderId, remote: &str) -> Option<ItemId> {
        let read = || -> Result<Option<ItemId>> {
            let conn = self.db.conn();
            let mut stmt = conn.prepare_cached("SELECT id FROM mail WHERE folder = ?1 AND remote_id = ?2")?;
            let mut rows = stmt.query(params![to_sql_id(folder.0), remote])?;
            let mut best: Option<ItemId> = None;
            while let Some(r) = rows.next()? {
                let id = ItemId(from_sql_id(r.get(0)?));
                best = Some(best.map_or(id, |b| b.min(id)));
            }
            Ok(best)
        };
        reported("looking up a server id", read(), None)
    }

    fn older_than(&self, folder: FolderId, cutoff: i64, limit: usize) -> Vec<ItemId> {
        if limit == 0 {
            return Vec::new();
        }
        let read = || -> Result<Vec<ItemId>> {
            let conn = self.db.conn();
            let f = to_sql_id(folder.0);
            let mut out: Vec<(i64, ItemId)> = Vec::new();
            {
                let mut stmt = conn.prepare_cached(
                    "SELECT received, id FROM mail WHERE folder = ?1 AND received < ?2 \
                     ORDER BY received, id LIMIT ?3",
                )?;
                let mut rows = stmt.query(params![f, cutoff, i64::try_from(limit).unwrap_or(i64::MAX)])?;
                while let Some(r) = rows.next()? {
                    out.push((r.get(0)?, ItemId(from_sql_id(r.get(1)?))));
                }
            }
            // Ties on the last date are ordered by `u64` id, which SQLite orders differently past i64::MAX:
            // take every item of that date and sort here.
            if let Some(&(last, _)) = out.last() {
                out.retain(|(r, _)| *r != last);
                let mut stmt =
                    conn.prepare_cached("SELECT id FROM mail WHERE folder = ?1 AND received = ?2")?;
                let mut rows = stmt.query(params![f, last])?;
                while let Some(r) = rows.next()? {
                    out.push((last, ItemId(from_sql_id(r.get(0)?))));
                }
            }
            out.sort_unstable();
            out.truncate(limit);
            Ok(out.into_iter().map(|(_, id)| id).collect())
        };
        reported("listing old mail", read(), Vec::new())
    }

    fn scan(&self, filter: &ScanFilter, visit: &mut dyn FnMut(&MailItem) -> ControlFlow<()>) -> Result<()> {
        match filter {
            ScanFilter::All => {
                // `u64` order: the ids SQLite holds as non-negative numbers first, then those past i64::MAX.
                if !self.visit_range(0, i64::MAX, visit)? {
                    self.visit_range(i64::MIN, -1, visit)?;
                }
                Ok(())
            }
            ScanFilter::Folder(f) => {
                let ids = self.try_ids_in(*f)?;
                self.visit_ids(&ids, visit)
            }
            ScanFilter::Folders(set) => {
                let mut ids: Vec<ItemId> = Vec::new();
                for f in set {
                    ids.extend(self.try_ids_in(*f)?);
                }
                ids.sort_unstable();
                ids.dedup();
                self.visit_ids(&ids, visit)
            }
        }
    }

    fn begin(&mut self) -> ChangeToken {
        let token = ChangeToken::fresh();
        if self.frames.len() >= MAX_OPEN_RECORDS {
            self.fold_above(self.frames.len().saturating_sub(2));
        }
        let name = self.db.savepoint_name("rec");
        let savepoint = {
            let conn = self.db.conn();
            match conn.execute_batch(&format!("SAVEPOINT {name}")) {
                Ok(()) => Some(name),
                Err(e) => {
                    log::error!("store.db: could not open a change record: {e}");
                    None
                }
            }
        };
        self.frames.push(Frame { token, changes: MailChanges::new(), savepoint });
        token
    }

    fn commit(&mut self, token: ChangeToken) -> Result<MailChanges> {
        let pos = self.frames.iter().rposition(|f| f.token == token).ok_or(StoreError::NoChangeRecord)?;
        self.fold_above(pos);
        let frame = self.frames.pop().ok_or(StoreError::NoChangeRecord)?;
        if let Some(sp) = &frame.savepoint {
            let released = {
                let conn = self.db.conn();
                conn.execute_batch(&format!("RELEASE {sp}"))
            };
            if let Err(e) = released {
                // The changes did not reach the disk: undo them everywhere so memory and disk agree.
                {
                    let conn = self.db.conn();
                    rollback_savepoint(&conn, sp);
                }
                self.uncache(frame.changes.ids());
                return Err(e.into());
            }
        }
        if let Some(parent) = self.frames.last_mut() {
            for (id, b) in frame.changes.iter() {
                if !parent.changes.contains(id) {
                    parent.changes.record(id, b.cloned());
                }
            }
        }
        Ok(frame.changes)
    }

    fn rollback(&mut self, token: ChangeToken) -> Result<MailChanges> {
        let pos = self.frames.iter().rposition(|f| f.token == token).ok_or(StoreError::NoChangeRecord)?;
        self.fold_above(pos);
        let frame = self.frames.pop().ok_or(StoreError::NoChangeRecord)?;
        let Some(sp) = frame.savepoint else {
            return self.put_back(frame.changes);
        };
        let mut now: Vec<(ItemId, Option<MailItem>)> = Vec::with_capacity(frame.changes.len());
        for id in frame.changes.ids() {
            now.push((id, self.load(id)?.map(|m| (*m).clone())));
        }
        let rolled = {
            let conn = self.db.conn();
            conn.execute_batch(&format!("ROLLBACK TO {sp}; RELEASE {sp}"))
        };
        self.uncache(frame.changes.ids());
        if let Err(e) = rolled {
            log::error!("store.db: could not roll back a change record ({e}); writing the old items back");
            self.put_back(frame.changes)?;
        }
        Ok(now.into_iter().collect())
    }

    fn insert(&mut self, item: MailItem) -> Result<Option<MailItem>> {
        let id = item.id;
        let old = self.load(id)?.map(|m| (*m).clone());
        let res = self.write(|c| self.store(c, &item));
        self.uncache([id]);
        res?;
        self.note(id, old.as_ref());
        Ok(old)
    }

    fn update(&mut self, id: ItemId, f: &mut dyn FnMut(&mut MailItem)) -> Result<bool> {
        let Some(old) = self.load(id)? else { return Ok(false) };
        let mut new = (*old).clone();
        f(&mut new);
        new.id = id;
        if new != *old {
            let res = self.write(|c| self.store(c, &new));
            self.uncache([id]);
            res?;
        }
        self.note(id, Some(&old));
        Ok(true)
    }

    fn remove(&mut self, ids: &[ItemId]) -> Result<Vec<MailItem>> {
        let mut gone: Vec<MailItem> = Vec::new();
        for id in ids {
            if let Some(m) = self.load(*id)? {
                gone.push((*m).clone());
            }
        }
        let res = self.write(|c| {
            for m in &gone {
                rows::delete(c, to_sql_id(m.id.0))?;
            }
            Ok(())
        });
        self.uncache(gone.iter().map(|m| m.id));
        res?;
        for m in &gone {
            if self.frames.last().is_some_and(|f| !f.changes.contains(m.id)) {
                self.note(m.id, Some(m));
            }
        }
        // `ids` may name an item twice: only its first removal returns it.
        let mut seen = std::collections::BTreeSet::new();
        Ok(gone.into_iter().filter(|m| seen.insert(m.id)).collect())
    }

    fn move_to(&mut self, ids: &[ItemId], to: FolderId) -> Result<Vec<ItemId>> {
        self.change_each(ids, |m| m.folder != to, |m| m.folder = to)
    }

    fn set_read(&mut self, ids: &[ItemId], read: bool) -> Result<Vec<ItemId>> {
        self.change_each(ids, |m| m.read != read, |m| m.read = read)
    }
}

impl SqlRepo {
    /// Changes each item in `ids` that `want` admits with `f`, in one savepoint; returns the ids changed.
    fn change_each(
        &mut self,
        ids: &[ItemId],
        want: impl Fn(&MailItem) -> bool,
        f: impl Fn(&mut MailItem),
    ) -> Result<Vec<ItemId>> {
        let mut changed: Vec<(MailItem, MailItem)> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for id in ids {
            if !seen.insert(*id) {
                continue;
            }
            let Some(old) = self.load(*id)? else { continue };
            if !want(&old) {
                continue;
            }
            let mut new = (*old).clone();
            f(&mut new);
            changed.push(((*old).clone(), new));
        }
        let res = self.write(|c| {
            for (_, new) in &changed {
                self.store(c, new)?;
            }
            Ok(())
        });
        self.uncache(changed.iter().map(|(o, _)| o.id));
        res?;
        let mut out = Vec::with_capacity(changed.len());
        for (old, _) in &changed {
            self.note(old.id, Some(old));
            out.push(old.id);
        }
        Ok(out)
    }
}
