//! Local mailbox storage (layer L2).
//!
//! On-disk layout under the profile directory:
//! - `mailbox.json`: full [`mailcraft_model::Mailbox`] snapshot without raw message sources (written atomically:
//!   temp file + fsync + rename). Wrapped as `{"mailcraft_snapshot":1,"generation":N,"mailbox":{...}}`.
//! - `journal.jsonl`: append-only [`Op`] log since the snapshot (header line carries the snapshot generation it
//!   applies to); replayed on open; a torn last line is ignored (crash recovery), garbled middle lines are
//!   skipped with a warning. Compacted into a new snapshot by [`Store::checkpoint`].
//! - `blobs/<aa>/<hash>-<len>.eml`: raw RFC 5322 sources, content-addressed (FNV-1a 64 hex + length,
//!   collision-checked byte for byte; a true collision gets a `-<n>` suffix).
//! - `recoverable.json`: items deleted for good, kept for Recover Deleted Items
//!   ([`mailcraft_model::RecoverBin`], raw sources in blobs), written atomically by [`Store::save_recoverable`].
//! - `lock`: single-writer lock file holding the owner's pid and a timestamp. A lock whose process is gone
//!   (checked via `/proc/<pid>` on Linux) or, where liveness cannot be checked, whose timestamp is more than an
//!   hour old, is taken over.
//!
//! Imports: Maildir (`cur/`, `new/`, flags from the `:2,` suffix), MBOX (mboxrd/mboxo, `From ` lines,
//! `>From` unescaping), single `.eml` files. Export: MBOX (mboxrd). Size caps on every read.
//!
//! Interface (frozen for commit 2): [`Store`], [`Op`], [`StoreError`], [`import_maildir`], [`import_mbox`],
//! [`export_mbox`]. Additions: [`Autosave`], [`Store::import_eml`], [`Store::export_mbox`],
//! [`Store::checkpoint_mut`], [`Store::load_report`], [`Store::raw_of`], [`Store::gc_blobs`], [`Op::apply`],
//! [`Store::journal_bytes`], [`Store::keep_alive`], [`Store::append_keyed`], [`MailRepo`] (storage stage S2),
//! [`Op::PutSync`], [`Op::PutConfig`] and [`SmallState`] (storage stage S4).
//!
//! Size caps (issue #25): every file is written under the same cap its reader applies, so a profile can never
//! hold a snapshot, journal or recoverable-items file it would refuse to open. A write past the cap fails with
//! [`StoreError::TooLarge`] and leaves the file on disk as it was.

pub mod autosave;
pub mod blob;
pub mod config;
pub mod import;
pub mod journal;
pub mod lock;
pub mod mbox;
pub mod repo;
mod time;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use mailcraft_model::{DateTime, DeletedItem, FolderId, Importance, ItemId, MailItem, Mailbox, RecoverBin};
use serde::{Deserialize, Serialize};

pub use autosave::Autosave;
pub use blob::BlobKey;
pub use config::SmallState;
pub use import::MaildirFlags;
pub use journal::Repair;
pub use mailcraft_model::{ChangeToken, MailChanges};
pub use repo::{MailRepo, Shared};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt store: {0}")]
    Corrupt(String),
    #[error("store is locked by another process")]
    Locked,
    /// A write was refused because the file would pass the size cap its reader applies (the file on disk is
    /// left as it was).
    #[error("{0}")]
    TooLarge(String),
    /// A change record was closed that is not open (it was closed already, or the mail it recorded was
    /// replaced as a whole).
    #[error("the change record is not open")]
    NoChangeRecord,
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// The recoverable-items bin in the profile directory.
pub const RECOVERABLE_FILE: &str = "recoverable.json";

/// Largest recoverable-items file read or written.
pub const MAX_RECOVERABLE_BYTES: u64 = 256 << 20;

/// Largest single blob (raw message) accepted.
pub const MAX_BLOB_BYTES: usize = mailcraft_mime::MAX_MESSAGE_BYTES;

/// A journaled change. The engine emits these after each command; replay must be idempotent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Op {
    /// Upsert a mail item (blob stored separately).
    PutMail(Box<MailItem>),
    RemoveMail(ItemId),
    /// Upsert the whole folder table entry.
    PutFolder(Box<mailcraft_model::Folder>),
    RemoveFolder(FolderId),
    /// Replace one account, matched by id (appended when it is missing). No longer written (storage stage S4
    /// journals an account's settings through `PutConfig` and its sync state through `PutSync`); still
    /// replayed from journals written before.
    PutAccount(Box<mailcraft_model::Account>),
    /// Replace the whole mailbox (a mailbox opened or replaced as a whole, and calendar, contact, task and
    /// note changes until storage stage S12).
    Replace(Box<Mailbox>),
    /// Replace one account's synchronisation bookkeeping (server folder states, the queue of server changes),
    /// matched by id; skipped when the account is missing. Every command on synchronised mail journals this
    /// next to its per-item ops.
    PutSync(mailcraft_model::AccountId, Box<mailcraft_model::account::AccountSync>),
    /// Replace the configuration parts that changed ([`SmallState`]: accounts without their sync state,
    /// categories, rules, quick steps, views, signatures, Auto-Complete, dictionary, junk lists).
    PutConfig(Box<SmallState>),
}

impl Op {
    /// Applies this op to `mb`. Upserts keep `mb.next_id` ahead of every id they insert so later allocations
    /// never collide with replayed items.
    pub fn apply(self, mb: &mut Mailbox) {
        match self {
            Op::PutMail(item) => {
                mb.next_id = mb.next_id.max(item.id.0);
                mb.mail.insert(item.id, *item);
            }
            Op::RemoveMail(id) => {
                mb.mail.remove(id);
            }
            Op::PutFolder(folder) => {
                mb.next_id = mb.next_id.max(folder.id.0);
                mb.folders.insert(folder.id, *folder);
            }
            Op::RemoveFolder(id) => {
                mb.folders.remove(&id);
                mb.views.remove(&id);
            }
            Op::PutAccount(account) => match mb.accounts.iter_mut().find(|a| a.id == account.id) {
                Some(a) => *a = *account,
                None => mb.accounts.push(*account),
            },
            Op::Replace(new) => *mb = *new,
            Op::PutSync(id, sync) => config::put_sync(mb, id, *sync),
            Op::PutConfig(state) => state.apply(mb),
        }
    }
}

/// Outcome of [`Store::load_report`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadReport {
    pub snapshot_generation: u64,
    /// Ops replayed from the journal.
    pub replayed: usize,
    /// Garbled journal lines skipped (1-based line numbers).
    pub skipped_lines: Vec<usize>,
    /// A torn last journal line was ignored.
    pub torn_tail: bool,
    /// The journal predated the snapshot and was ignored.
    pub stale_journal: bool,
    /// Repairs made when the store was opened.
    pub open_repair: Repair,
}

#[derive(Debug)]
pub struct Store {
    pub dir: PathBuf,
    lock: lock::LockGuard,
    journal: File,
    generation: u64,
    /// The snapshot for `generation` was written but the journal still belongs to an older one.
    journal_stale: bool,
    repair: Repair,
    /// Largest journal [`Store::append`] will produce (what `open` / `load` read back).
    journal_cap: u64,
    /// Largest snapshot [`Store::checkpoint`] will write (what `open` / `load` read back).
    snapshot_cap: u64,
}

impl Store {
    /// Opens (creating if missing) a store at `dir`, taking the lock.
    pub fn open(dir: &Path) -> Result<Store> {
        fs::create_dir_all(dir)?;
        let lock = lock::LockGuard::acquire(dir)?;
        fs::create_dir_all(dir.join(blob::BLOBS_DIR))?;
        remove_temp_files(dir);
        let snapshot_gen = journal::peek_snapshot_generation(dir)?;
        let (journal, generation, repair) = journal::prepare_journal(dir, snapshot_gen)?;
        Ok(Store {
            dir: dir.to_path_buf(),
            lock,
            journal,
            generation: generation.max(snapshot_gen),
            journal_stale: false,
            repair,
            journal_cap: journal::MAX_STATE_BYTES,
            snapshot_cap: journal::MAX_STATE_BYTES,
        })
    }

    /// Loads snapshot + replays the journal.
    pub fn load(&self) -> Result<Mailbox> {
        self.load_report().map(|(mb, _)| mb)
    }

    /// [`Store::load`] plus what recovery did.
    pub fn load_report(&self) -> Result<(Mailbox, LoadReport)> {
        let (snap_gen, mut mb) = journal::read_snapshot(&self.dir)?;
        let mut report = LoadReport {
            snapshot_generation: snap_gen,
            open_repair: self.repair.clone(),
            ..Default::default()
        };
        let Some(bytes) =
            journal::read_capped(&self.dir.join(journal::JOURNAL_FILE), journal::MAX_STATE_BYTES)?
        else {
            return Ok((mb, report));
        };
        let scan = journal::scan_journal(&bytes);
        if scan.generation.is_some_and(|g| g < snap_gen) {
            log::warn!("journal predates the snapshot; ignoring it");
            report.stale_journal = true;
            return Ok((mb, report));
        }
        report.replayed = scan.ops.len();
        report.skipped_lines = scan.skipped_lines;
        report.torn_tail = scan.torn_tail;
        for op in scan.ops {
            op.apply(&mut mb);
        }
        Ok((mb, report))
    }

    /// Appends ops to the journal (fsync'd).
    ///
    /// Raw sources inside the ops are moved to blobs first; the journal only ever holds blob keys.
    pub fn append(&mut self, ops: &[Op]) -> Result<()> {
        self.append_keyed(ops).map(|_| ())
    }

    /// [`Store::append`] that also returns where the raw sources inside the ops went: (item, blob key) for
    /// every mail item whose source the journal now refers to by key. The caller may then drop those
    /// sources from memory ([`MailItem::raw`]), setting `blob` to the key.
    pub fn append_keyed(&mut self, ops: &[Op]) -> Result<Vec<(ItemId, String)>> {
        let mut keys = Vec::new();
        if ops.is_empty() {
            return Ok(keys);
        }
        if self.journal_stale {
            // A previous checkpoint wrote its snapshot but could not reset the journal.
            self.reset_journal()?;
        }
        let before = self.journal.metadata()?.len();
        // Open and load refuse a journal past the cap; writing it would make the whole store unopenable. The
        // batch is serialised through a writer capped at the room left, so a refused batch (a whole-mailbox
        // `Replace` can be as large as the cap) is never built in memory first.
        let mut w = journal::CapWriter::new(Vec::new(), self.journal_cap.saturating_sub(before));
        for op in ops {
            let stripped = self.externalize(op, &mut keys)?;
            let op = stripped.as_ref().unwrap_or(op);
            let res = serde_json::to_writer(&mut w, op)
                .map_err(std::io::Error::from)
                .and_then(|()| w.write_all(b"\n"));
            match res {
                Ok(()) => {}
                Err(e) if journal::is_cap_error(&e) => {
                    return Err(StoreError::TooLarge(format!(
                        "the journal would grow past the {} byte cap; checkpoint first",
                        self.journal_cap
                    )));
                }
                Err(e) => return Err(e.into()),
            }
        }
        let buf = w.into_inner();
        let written = self.journal.write_all(&buf).and_then(|()| self.journal.sync_data());
        if let Err(e) = written {
            // Cut off a partial batch so the next append does not glue its first op onto a torn line.
            if let Err(e2) = self.journal.set_len(before) {
                log::warn!("could not roll back a failed journal append: {e2}");
            }
            return Err(e.into());
        }
        self.lock.refresh_if_due();
        Ok(keys)
    }

    /// Writes a fresh snapshot of `mb` and truncates the journal.
    ///
    /// Raw sources are written to blobs and left out of the snapshot (the snapshot copy gets `blob` set; use
    /// [`Store::checkpoint_mut`] to also record the keys on `mb` itself).
    pub fn checkpoint(&mut self, mb: &Mailbox) -> Result<()> {
        let mut copy = mb.clone();
        self.strip_raws(&mut copy)?;
        self.write_checkpoint(&copy)
    }

    /// Like [`Store::checkpoint`] without cloning the mailbox: raw sources are moved to blobs, `item.blob` is set
    /// on `mb`, and the raws are put back afterwards (or dropped when `drop_raw`).
    pub fn checkpoint_mut(&mut self, mb: &mut Mailbox, drop_raw: bool) -> Result<()> {
        let raws = self.strip_raws(mb)?;
        let res = self.write_checkpoint(mb);
        if !drop_raw {
            for (id, raw) in raws {
                if let Some(item) = mb.mail.get_mut(id) {
                    item.raw = Some(raw);
                }
            }
        }
        res
    }

    fn write_checkpoint(&mut self, stripped: &Mailbox) -> Result<()> {
        let generation = self.generation.saturating_add(1);
        // Past the cap the snapshot is refused (the old snapshot and the journal stay as they are): `load`
        // could not read it back.
        journal::write_snapshot_capped(&self.dir, generation, stripped, self.snapshot_cap)?;
        // The snapshot now holds everything; from here on the old journal is stale (open/load ignore it), so
        // nothing may be appended to it even if the reset below fails.
        self.generation = generation;
        self.journal_stale = true;
        self.reset_journal()?;
        self.lock.refresh_if_due();
        Ok(())
    }

    /// Replaces the journal with an empty one for the current generation.
    fn reset_journal(&mut self) -> Result<()> {
        self.journal = journal::reset_journal(&self.dir, self.generation)?;
        self.journal_stale = false;
        Ok(())
    }

    /// Moves every raw source in `mb` into blobs, setting `item.blob`; returns the removed raws.
    fn strip_raws(&mut self, mb: &mut Mailbox) -> Result<Vec<(ItemId, Vec<u8>)>> {
        let mut raws = Vec::new();
        let mut failure = None;
        for (id, item) in mb.mail.iter_mut() {
            if let Some(raw) = item.raw.take() {
                match self.blob_for(&raw, item.blob.as_deref()) {
                    Ok(key) => item.blob = Some(key),
                    Err(e) => {
                        item.raw = Some(raw);
                        failure = Some(e);
                        break;
                    }
                }
                raws.push((id, raw));
            }
        }
        if let Some(e) = failure {
            // Leave the caller's mailbox as it was.
            for (id, raw) in raws {
                if let Some(item) = mb.mail.get_mut(id) {
                    item.raw = Some(raw);
                }
            }
            return Err(e);
        }
        Ok(raws)
    }

    /// Op with raw sources moved to blobs, or `None` when `op` holds none. The (item, key) of every source
    /// moved is added to `keys`.
    fn externalize(&mut self, op: &Op, keys: &mut Vec<(ItemId, String)>) -> Result<Option<Op>> {
        match op {
            Op::PutMail(item) if item.raw.is_some() => {
                let mut copy = (**item).clone();
                if let Some(raw) = copy.raw.take() {
                    let key = self.blob_for(&raw, copy.blob.as_deref())?;
                    keys.push((copy.id, key.clone()));
                    copy.blob = Some(key);
                }
                Ok(Some(Op::PutMail(Box::new(copy))))
            }
            Op::Replace(mb) if mb.mail.items().any(|m| m.raw.is_some()) => {
                let mut copy = (**mb).clone();
                let moved = self.strip_raws(&mut copy)?;
                for (id, _) in moved {
                    if let Some(key) = copy.mail.get(id).and_then(|m| m.blob.clone()) {
                        keys.push((id, key));
                    }
                }
                Ok(Some(Op::Replace(Box::new(copy))))
            }
            _ => Ok(None),
        }
    }

    /// Reuses `existing` when it is the key of `raw` and its file is present with the right length (a cheap
    /// check that catches truncation without re-reading every blob on every checkpoint); otherwise stores
    /// `raw`, which verifies and repairs the file.
    fn blob_for(&mut self, raw: &[u8], existing: Option<&str>) -> Result<String> {
        if let Some(key) = existing.and_then(BlobKey::parse)
            && key.matches(raw)
            && fs::metadata(key.path(&self.dir)).is_ok_and(|m| m.is_file() && m.len() == key.len)
        {
            return Ok(key.to_string());
        }
        self.put_blob(raw)
    }

    /// Stores a raw message; returns its blob key.
    pub fn put_blob(&mut self, raw: &[u8]) -> Result<String> {
        if raw.len() > MAX_BLOB_BYTES {
            return Err(StoreError::Io(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("blob of {} bytes exceeds the {MAX_BLOB_BYTES} byte cap", raw.len()),
            )));
        }
        let base = BlobKey::for_data(raw);
        for probe in 0..=blob::MAX_PROBES {
            let key = base.with_probe(probe);
            let path = key.path(&self.dir);
            // The key carries the length: a file of any other size is damaged (and may be too big to read).
            let existing = match fs::metadata(&path) {
                Ok(m) if m.is_file() && m.len() == key.len => journal::read_capped(&path, key.len)?,
                Ok(_) => {
                    log::warn!("blob {key} has the wrong size; rewriting it");
                    None
                }
                Err(e) if e.kind() == ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
            match existing {
                Some(existing) if existing == raw => return Ok(key.to_string()),
                Some(existing) if key.matches(&existing) => {
                    log::warn!("blob hash collision on {key}; probing");
                    continue;
                }
                Some(_) => {
                    log::warn!("blob {key} was damaged; rewriting it");
                }
                None => {}
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            journal::write_atomic(&path, |w| w.write_all(raw))?;
            return Ok(key.to_string());
        }
        Err(StoreError::Corrupt(format!("too many blob collisions for {base}")))
    }

    pub fn get_blob(&self, key: &str) -> Result<Vec<u8>> {
        let parsed = BlobKey::parse(key)
            .ok_or_else(|| StoreError::Corrupt(format!("invalid blob key {:?}", trunc(key))))?;
        let data =
            journal::read_capped(&parsed.path(&self.dir), parsed.len.min(blob_cap()))?.ok_or_else(|| {
                StoreError::Io(std::io::Error::new(ErrorKind::NotFound, format!("blob {parsed}")))
            })?;
        if !parsed.matches(&data) {
            return Err(StoreError::Corrupt(format!("blob {parsed} does not match its key")));
        }
        Ok(data)
    }

    /// True when the blob exists on disk (not verified).
    pub fn has_blob(&self, key: &str) -> bool {
        BlobKey::parse(key).is_some_and(|k| k.path(&self.dir).is_file())
    }

    /// The raw source of `item`: its in-memory `raw`, else its blob. `Ok(None)` when it has neither.
    pub fn raw_of(&self, item: &MailItem) -> Result<Option<Vec<u8>>> {
        if let Some(raw) = &item.raw {
            return Ok(Some(raw.clone()));
        }
        match &item.blob {
            Some(key) => self.get_blob(key).map(Some),
            None => Ok(None),
        }
    }

    /// The first `cap` bytes of `item`'s raw source (its in-memory `raw`, else its blob), for a header scan
    /// that should not read whole messages. Unlike [`Store::raw_of`] the bytes are not checked against the
    /// blob key (that needs the whole file); a blob shorter than its key says is reported as corrupt.
    /// `Ok(None)` when the item has no source.
    pub fn head_of(&self, item: &MailItem, cap: usize) -> Result<Option<Vec<u8>>> {
        if let Some(raw) = &item.raw {
            return Ok(Some(raw.get(..raw.len().min(cap)).unwrap_or_default().to_vec()));
        }
        let Some(key) = &item.blob else { return Ok(None) };
        let parsed = BlobKey::parse(key)
            .ok_or_else(|| StoreError::Corrupt(format!("invalid blob key {:?}", trunc(key))))?;
        let f = match File::open(parsed.path(&self.dir)) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let want = parsed.len.min(u64::try_from(cap).unwrap_or(u64::MAX));
        let mut buf = Vec::with_capacity(usize::try_from(want).unwrap_or(0));
        std::io::Read::read_to_end(&mut std::io::Read::take(f, want), &mut buf)?;
        if u64::try_from(buf.len()).unwrap_or(u64::MAX) < want {
            return Err(StoreError::Corrupt(format!("blob {parsed} is shorter than its key says")));
        }
        Ok(Some(buf))
    }

    /// Current size of the journal file in bytes (0 when it cannot be read).
    pub fn journal_bytes(&self) -> u64 {
        self.journal.metadata().map_or(0, |m| m.len())
    }

    /// Keeps the store lock fresh while the store is idle; call on an idle tick (done by
    /// [`Autosave::commit`]). Cheap: the lock file is rewritten at most every few minutes.
    pub fn keep_alive(&mut self) {
        self.lock.refresh_if_due();
    }

    /// Lowers the journal size cap ([`journal::MAX_STATE_BYTES`] by default): appends past it fail. Tests use
    /// it to make a store write fail.
    #[doc(hidden)]
    pub fn set_journal_cap(&mut self, cap: u64) {
        self.journal_cap = cap;
    }

    /// Lowers the snapshot size cap ([`journal::MAX_STATE_BYTES`] by default, never above it): checkpoints
    /// past it are refused. Tests use it to reach the cap with a small mailbox.
    #[doc(hidden)]
    pub fn set_snapshot_cap(&mut self, cap: u64) {
        self.snapshot_cap = cap.min(journal::MAX_STATE_BYTES);
    }

    #[cfg(test)]
    pub(crate) fn backdate_lock_refresh(&mut self) {
        self.lock.backdate_refresh();
    }

    /// Generation of the last snapshot written or found.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Repairs made to the journal by [`Store::open`].
    pub fn open_repair(&self) -> &Repair {
        &self.repair
    }

    /// Imports one `.eml` file into `folder`: the raw source goes to the blob store (`item.blob` set, `item.raw`
    /// kept). Not journaled: the caller appends `Op::PutMail` (or checkpoints) as for any other change.
    pub fn import_eml(&mut self, mb: &mut Mailbox, path: &Path, folder: FolderId) -> Result<ItemId> {
        let mut item = import::read_eml(path, folder)?;
        if let Some(raw) = &item.raw {
            item.blob = Some(self.put_blob(raw)?);
        }
        import::insert_item(mb, item)
    }

    /// [`export_mbox`] that also reads sources from the blob store.
    pub fn export_mbox(&self, items: &[&MailItem], file: &Path) -> Result<()> {
        write_mbox(items, file, |item| match self.raw_of(item) {
            Ok(raw) => raw,
            Err(e) => {
                log::warn!("export: source of item {:?} unavailable ({e}); rebuilding from fields", item.id);
                None
            }
        })
    }

    /// Reads the recoverable-items bin (`Ok(default)` when there is none yet).
    pub fn load_recoverable(&self) -> Result<RecoverBin> {
        let path = self.dir.join(RECOVERABLE_FILE);
        match journal::read_capped(&path, MAX_RECOVERABLE_BYTES)? {
            None => Ok(RecoverBin::default()),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Corrupt(format!("{}: {e}", path.display()))),
        }
    }

    /// Writes the recoverable-items bin atomically. Raw message sources go to the blob store (the file keeps
    /// blob keys only). Once the file is written, the mail entries of `bin` hold their blob key instead of
    /// the source, so later saves do not store it again (a failed write leaves `bin` as it was).
    pub fn save_recoverable(&mut self, bin: &mut RecoverBin) -> Result<()> {
        let mut copy = bin.clone();
        for e in &mut copy.items {
            if let DeletedItem::Mail(m) = &mut e.item
                && let Some(raw) = m.raw.take()
            {
                m.blob = Some(self.blob_for(&raw, m.blob.as_deref())?);
            }
        }
        let bytes = serde_json::to_vec(&copy).map_err(std::io::Error::other)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECOVERABLE_BYTES {
            return Err(StoreError::TooLarge(format!(
                "the recoverable items would take {} bytes, past the {MAX_RECOVERABLE_BYTES} byte cap",
                bytes.len()
            )));
        }
        journal::write_atomic(&self.dir.join(RECOVERABLE_FILE), |w| w.write_all(&bytes))?;
        // The file now references these blobs (so collection keeps them): the sources can leave memory.
        for (e, saved) in bin.items.iter_mut().zip(copy.items) {
            if let (DeletedItem::Mail(m), DeletedItem::Mail(saved)) = (&mut e.item, saved.item)
                && m.raw.is_some()
                && m.id == saved.id
                && saved.blob.is_some()
            {
                m.raw = None;
                m.blob = saved.blob;
            }
        }
        Ok(())
    }

    /// Deletes blobs not referenced by `mb` (which must be the complete current state) or by the
    /// recoverable-items bin, and leftover temp files. Returns the number of blobs removed.
    pub fn gc_blobs(&mut self, mb: &Mailbox) -> Result<usize> {
        // A bin that cannot be read keeps every blob (nothing it may still need is removed).
        let bin = self.load_recoverable()?;
        let mut live: BTreeSet<&str> = mb.mail.items().filter_map(|m| m.blob.as_deref()).collect();
        live.extend(bin.items.iter().filter_map(|e| match &e.item {
            DeletedItem::Mail(m) => m.blob.as_deref(),
            _ => None,
        }));
        // Items still holding their source may have been journaled or snapshotted with a blob key the caller
        // never saw (`append` / `checkpoint` set it on their own copy): every probe of that content is live.
        let live_content: BTreeSet<(u64, u64)> = mb
            .mail
            .items()
            .filter_map(|m| m.raw.as_deref())
            .map(|raw| {
                let k = BlobKey::for_data(raw);
                (k.hash, k.len)
            })
            .collect();
        let mut removed = 0usize;
        let root = self.dir.join(blob::BLOBS_DIR);
        let fans = match fs::read_dir(&root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        for fan in fans {
            let fan = fan?;
            if !fan.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(fan.path())? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && name.contains(".tmp-") {
                    let _ = fs::remove_file(entry.path());
                    continue;
                }
                let Some(key) = name.strip_suffix(".eml") else { continue };
                let Some(parsed) = BlobKey::parse(key) else { continue };
                if !live.contains(key) && !live_content.contains(&(parsed.hash, parsed.len)) {
                    fs::remove_file(entry.path())?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

fn blob_cap() -> u64 {
    u64::try_from(MAX_BLOB_BYTES).unwrap_or(u64::MAX)
}

fn trunc(s: &str) -> String {
    s.chars().take(64).collect()
}

/// Removes temp files left by a crash during an atomic write (only our own naming pattern).
fn remove_temp_files(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten().take(10_000) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = name.starts_with(&format!(".{}.tmp-", journal::SNAPSHOT_FILE))
            || name.starts_with(&format!(".{}.tmp-", journal::JOURNAL_FILE))
            || name.starts_with(&format!(".{RECOVERABLE_FILE}.tmp-"))
            || name.starts_with(&format!("{}.stale-", lock::LOCK_FILE));
        if ours {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Imports a Maildir into `folder`; returns new item ids.
///
/// Reads `new/` (unread) and `cur/` (flags from the `:2,` info: S read, F flagged, R replied, P forwarded,
/// D draft; T trashed messages are skipped). `received` is the delivery time from the unique file name, else
/// the file's mtime. Items keep their `raw` source; a later checkpoint moves it to the blob store.
/// Unreadable or oversized files are skipped; the import stops (with a warning) once
/// [`import::MAX_IMPORT_BYTES`] of source is held.
pub fn import_maildir(mb: &mut Mailbox, dir: &Path, folder: FolderId) -> Result<Vec<ItemId>> {
    import::import_maildir(mb, dir, folder)
}

/// Imports an MBOX file into `folder`; returns new item ids.
///
/// `received` comes from the `From ` line date; `Status`/`X-Status` headers set read/flag/replied state.
/// Messages over the size cap are skipped with a warning; the import stops (with a warning) after
/// [`mbox::MAX_MESSAGES`] messages or once [`import::MAX_IMPORT_BYTES`] of source is held.
pub fn import_mbox(mb: &mut Mailbox, file: &Path, folder: FolderId) -> Result<Vec<ItemId>> {
    import::import_mbox(mb, file, folder)
}

/// Exports items (with `raw` when present, else rebuilt from fields) as mboxrd.
pub fn export_mbox(items: &[&MailItem], file: &Path) -> Result<()> {
    write_mbox(items, file, |item| item.raw.clone())
}

fn write_mbox(
    items: &[&MailItem],
    file: &Path,
    resolve: impl Fn(&MailItem) -> Option<Vec<u8>>,
) -> Result<()> {
    journal::write_atomic(file, |w| {
        for item in items {
            let data = resolve(item).unwrap_or_else(|| rebuild_message(item));
            let sender = item.from.as_ref().or(item.sender.as_ref()).map(|m| m.addr.as_str()).unwrap_or("");
            let date =
                if item.received.unix != 0 { item.received } else { item.sent.unwrap_or(item.received) };
            mbox::write_message(w, sender, date, &data)?;
        }
        Ok(())
    })
}

/// Rebuilds RFC 5322 bytes from an item's fields (the message builder, with a minimal fallback).
pub fn rebuild_message(item: &MailItem) -> Vec<u8> {
    use mailcraft_mime::{Address, MessageBuilder};
    let addrs = |v: &[mailcraft_mime::Mailbox]| v.iter().cloned().map(Address::Mailbox).collect::<Vec<_>>();
    let builder = MessageBuilder {
        from: item.from.clone(),
        sender: item.sender.clone(),
        to: addrs(&item.to),
        cc: addrs(&item.cc),
        bcc: addrs(&item.bcc),
        reply_to: addrs(&item.reply_to),
        subject: item.subject.clone(),
        date: Some(item.sent.unwrap_or(item.received)),
        message_id: item.message_id.clone(),
        in_reply_to: item.in_reply_to.clone(),
        references: item.references.clone(),
        importance: match item.importance {
            Importance::High => Some("high".into()),
            Importance::Low => Some("low".into()),
            Importance::Normal => None,
        },
        text: item.body.text.clone(),
        html: item.body.html.clone(),
        keep_bcc: true,
        ..Default::default()
    };
    let built = builder.build();
    if built.is_empty() { minimal_message(item) } else { built }
}

fn header_safe(s: &str) -> String {
    s.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect()
}

/// Plain UTF-8 message (RFC 6532 headers, 8bit body) used when the builder produced nothing.
fn minimal_message(item: &MailItem) -> Vec<u8> {
    let list = |v: &[mailcraft_mime::Mailbox]| {
        v.iter().map(|m| header_safe(&m.to_display_string())).collect::<Vec<_>>().join(", ")
    };
    let mut out = String::new();
    if let Some(from) = &item.from {
        out.push_str(&format!("From: {}\r\n", header_safe(&from.to_display_string())));
    }
    for (name, v) in [("To", &item.to), ("Cc", &item.cc)] {
        if !v.is_empty() {
            out.push_str(&format!("{name}: {}\r\n", list(v)));
        }
    }
    out.push_str(&format!("Subject: {}\r\n", header_safe(&item.subject)));
    let date: DateTime = item.sent.unwrap_or(item.received);
    out.push_str(&format!("Date: {}\r\n", time::format_rfc5322_utc(date)));
    if let Some(id) = &item.message_id {
        out.push_str(&format!("Message-ID: <{}>\r\n", header_safe(id)));
    }
    out.push_str("MIME-Version: 1.0\r\n");
    let (ctype, body) = match (&item.body.text, &item.body.html) {
        (Some(t), _) => ("text/plain", t.as_str()),
        (None, Some(h)) => ("text/html", h.as_str()),
        (None, None) => ("text/plain", item.preview.as_str()),
    };
    out.push_str(&format!("Content-Type: {ctype}; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n"));
    for line in body.lines() {
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.into_bytes()
}
