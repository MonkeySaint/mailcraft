//! Local mailbox storage (layer L2).
//!
//! A profile uses one of two storages ([`StoreKind`]); an existing profile always opens in the one it has, a
//! new one gets the legacy storage unless asked for SQLite (`--store sqlite` or `MAILCRAFT_STORE=sqlite`; the
//! default until storage stage S9 migrates profiles).
//!
//! **SQLite** (storage stage S6, native builds, [`sql`]): `store.db` (WAL, `synchronous=FULL`) holds the
//! mailbox as rows, each [`Store::append`] one transaction; message sources and body records are zstd
//! records in the append-only `packs/NNNNNN.pack` files ([`packs`]), de-duplicated by SHA-256 and indexed in
//! the database. `mailbox.json` is a marker older versions refuse to open. `recoverable.json` and `lock` are
//! as below. [`SqlRepo`] is the [`MailRepo`] over the mail table; [`MemRepo`] the in-memory one.
//!
//! **Legacy** on-disk layout under the profile directory:
//! - `mailbox.json`: full [`mailcraft_model::Mailbox`] snapshot without message bodies or sources (written
//!   atomically: temp file + fsync + rename). Wrapped as
//!   `{"mailcraft_snapshot":2,"generation":N,"bodies":2,"mailbox":{...}}` (format 1, written before storage
//!   stage S5, is still read).
//! - `journal.jsonl`: append-only [`Op`] log since the snapshot (header line carries the snapshot generation it
//!   applies to, and its format, [`journal::JOURNAL_VERSION`]); replayed on open; a torn last line is ignored
//!   (crash recovery), garbled middle lines are skipped with a warning. One append is one line (several ops
//!   as an [`Op::Batch`]), so a crash keeps or loses a command's changes as a whole. Compacted into a new
//!   snapshot by [`Store::checkpoint`].
//! - `blobs/<aa>/<hash>-<len>.eml`: raw RFC 5322 sources, content-addressed (FNV-1a 64 hex + length,
//!   collision-checked byte for byte; a true collision gets a `-<n>` suffix); `blobs/<aa>/<hash>-<len>.body`:
//!   body records of items without a source or with a body changed here (storage stage S5,
//!   [`bodies::BodyStore`]). Items refer to both by key; files written before S5 that carry content inline are
//!   moved out when they are read ([`legacy`]).
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
//! [`Op::PutSync`], [`Op::PutConfig`] and [`SmallState`] (storage stage S4), [`BodyStore`] and [`Content`]
//! (storage stage S5: bodies and sources out of memory), [`StoreKind`], [`Store::open_with`],
//! [`Store::body_store`], [`SqlRepo`], [`SqlStore`], [`MemRepo`] and the pack files (storage stage S6),
//! [`Op::Batch`] and [`StoreError::TooNew`] (storage stage S4 review).
//!
//! Size caps (issue #25): every file is written under the same cap its reader applies, so a profile can never
//! hold a snapshot, journal or recoverable-items file it would refuse to open. A write past the cap fails with
//! [`StoreError::TooLarge`] and leaves the file on disk as it was.

pub mod autosave;
pub mod blob;
pub mod bodies;
pub mod config;
pub mod import;
pub mod journal;
pub mod legacy;
pub mod lock;
pub mod mbox;
pub mod mem;
#[cfg(not(target_arch = "wasm32"))]
pub mod packs;
pub mod repo;
#[cfg(not(target_arch = "wasm32"))]
pub mod sql;
mod time;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use mailcraft_model::{
    Body, DateTime, DeletedItem, FolderId, Importance, ItemId, MailItem, Mailbox, RecoverBin,
};
use serde::{Deserialize, Serialize};

pub use autosave::Autosave;
pub use blob::BlobKey;
pub use bodies::{BodyStore, Content};
pub use config::SmallState;
pub use import::MaildirFlags;
pub use journal::Repair;
pub use mailcraft_model::{ChangeToken, MailChanges};
pub use mem::MemRepo;
pub use repo::{MailRepo, Shared};
#[cfg(not(target_arch = "wasm32"))]
pub use sql::{Db, SqlRepo, SqlStore};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt store: {0}")]
    Corrupt(String),
    #[error("store is locked by another process")]
    Locked,
    /// The data folder was written by a newer MailCraft in a format this build cannot read (opening it anyway
    /// would lose what the newer build saved).
    #[error("the data folder was written by a newer version of MailCraft: {0}")]
    TooNew(String),
    /// A write was refused because the file would pass the size cap its reader applies (the file on disk is
    /// left as it was).
    #[error("{0}")]
    TooLarge(String),
    /// A change record was closed that is not open (it was closed already, or the mail it recorded was
    /// replaced as a whole).
    #[error("the change record is not open")]
    NoChangeRecord,
    /// The SQLite database refused an operation (storage stage S6).
    #[error("database: {0}")]
    Db(String),
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
    /// Upsert a mail item (its source and body are in the body store, by key).
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
    /// Several ops on one journal line, replayed in order: [`Store::append`] writes every batch of more than
    /// one op this way, so a batch is replayed whole or not at all (a line torn by a crash is dropped whole,
    /// never a prefix of the batch: an account removed while its folders and mail stay behind, or a server
    /// change queued for a move that is not there). Journal format 2.
    Batch(Vec<Op>),
}

impl Op {
    /// Applies this op to `mb`. Upserts keep `mb.next_id` ahead of every id they insert so later allocations
    /// never collide with replayed items. A [`Op::Batch`] is unrolled without recursion (a nested batch from
    /// a hostile journal cannot exhaust the stack).
    pub fn apply(self, mb: &mut Mailbox) {
        let mut stack: Vec<std::vec::IntoIter<Op>> = vec![vec![self].into_iter()];
        while let Some(top) = stack.last_mut() {
            match top.next() {
                None => {
                    stack.pop();
                }
                Some(Op::Batch(ops)) => stack.push(ops.into_iter()),
                Some(op) => op.apply_one(mb),
            }
        }
    }

    /// Calls `f` on this op, or on each op of a [`Op::Batch`] in order (nested batches unrolled without
    /// recursion); `f` never sees a batch.
    pub fn for_each_leaf<'a>(&'a self, mut f: impl FnMut(&'a Op)) {
        let mut stack: Vec<std::slice::Iter<'a, Op>> = vec![std::slice::from_ref(self).iter()];
        while let Some(top) = stack.last_mut() {
            match top.next() {
                None => {
                    stack.pop();
                }
                Some(Op::Batch(ops)) => stack.push(ops.iter()),
                Some(op) => f(op),
            }
        }
    }

    fn apply_one(self, mb: &mut Mailbox) {
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
            // `apply` unrolls batches before they get here; a batch is still applied whole if one does.
            Op::Batch(ops) => {
                for op in ops {
                    op.apply(mb);
                }
            }
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
    /// The snapshot or journal was written before storage stage S5 and carried message content inline: it was
    /// moved to the body store, and the next snapshot should be written soon (it no longer holds it).
    pub legacy_content: bool,
    /// Items whose inline content was moved to the body store.
    pub migrated_bodies: usize,
}

/// Which storage a profile uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StoreKind {
    /// `mailbox.json` + `journal.jsonl` + `blobs/` (the default until storage stage S9).
    #[default]
    Legacy,
    /// `store.db` + `packs/` (storage stage S6; native builds only).
    Sqlite,
}

impl StoreKind {
    /// Environment variable choosing the storage of new profiles (`legacy` or `sqlite`).
    pub const ENV: &'static str = "MAILCRAFT_STORE";

    /// `legacy` or `sqlite` (ASCII case ignored).
    pub fn parse(s: &str) -> Option<StoreKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "legacy" => Some(StoreKind::Legacy),
            "sqlite" => Some(StoreKind::Sqlite),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            StoreKind::Legacy => "legacy",
            StoreKind::Sqlite => "sqlite",
        }
    }

    /// The kind [`StoreKind::ENV`] asks for: legacy when it is unset, empty or not understood (with a
    /// warning).
    pub fn from_env() -> StoreKind {
        let Some(v) = std::env::var_os(StoreKind::ENV) else { return StoreKind::Legacy };
        let v = v.to_string_lossy();
        if v.trim().is_empty() {
            return StoreKind::Legacy;
        }
        StoreKind::parse(&v).unwrap_or_else(|| {
            log::warn!("{}={v:?} is not `legacy` or `sqlite`; using legacy storage", StoreKind::ENV);
            StoreKind::Legacy
        })
    }

    /// The storage of the existing profile in `dir`: `Ok(None)` for a folder holding no profile yet. A
    /// SQLite profile whose `store.db` is missing is an error (its `mailbox.json` is only a marker).
    pub fn of_profile(dir: &Path) -> Result<Option<StoreKind>> {
        let marker = journal::is_sqlite_marker(dir)?;
        if dir.join(SQL_DB_FILE).is_file() {
            if !marker && let Some(legacy) = legacy_part(dir) {
                // Legacy mail next to a database: opening either one would hide the other's mail (and a new
                // SQLite marker would make that permanent).
                return Err(StoreError::Corrupt(format!(
                    "{} holds both a legacy {legacy} and a {SQL_DB_FILE}; move one of them away",
                    dir.display()
                )));
            }
            return Ok(Some(StoreKind::Sqlite));
        }
        if marker {
            return Err(StoreError::Corrupt(format!(
                "{} keeps its mail in {SQL_DB_FILE}, which is missing",
                dir.display()
            )));
        }
        let legacy = [journal::SNAPSHOT_FILE, journal::JOURNAL_FILE].iter().any(|f| dir.join(f).exists());
        Ok(legacy.then_some(StoreKind::Legacy))
    }
}

/// The first part of a legacy profile in `dir`, if any: its snapshot, its journal, or a `blobs/` folder that
/// holds something (a SQLite profile has none of them, apart from its marker snapshot).
fn legacy_part(dir: &Path) -> Option<&'static str> {
    if dir.join(journal::SNAPSHOT_FILE).exists() {
        return Some(journal::SNAPSHOT_FILE);
    }
    if dir.join(journal::JOURNAL_FILE).exists() {
        return Some(journal::JOURNAL_FILE);
    }
    let blobs = dir.join(blob::BLOBS_DIR);
    match fs::read_dir(&blobs) {
        Ok(mut rd) => rd.next().map(|_| "blobs folder"),
        // There, but unreadable: count it as mail rather than guess.
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Some("blobs folder"),
        Err(_) => None,
    }
}

/// The database file of a SQLite profile.
pub const SQL_DB_FILE: &str = "store.db";

/// The snapshot and journal of a legacy profile.
#[derive(Debug)]
struct LegacyFiles {
    journal: File,
    generation: u64,
    /// The snapshot for `generation` was written but the journal still belongs to an older one.
    journal_stale: bool,
}

#[derive(Debug)]
enum Backend {
    Legacy(LegacyFiles),
    #[cfg(not(target_arch = "wasm32"))]
    Sql(Box<sql::SqlStore>),
}

#[derive(Debug)]
pub struct Store {
    pub dir: PathBuf,
    lock: lock::LockGuard,
    backend: Backend,
    repair: Repair,
    /// Largest journal [`Store::append`] will produce (what `open` / `load` read back); for a SQLite
    /// profile, the most one append writes (no limit unless lowered).
    journal_cap: u64,
    /// Largest snapshot [`Store::checkpoint`] will write (what `open` / `load` read back); for a SQLite
    /// profile, the most one checkpoint writes (no limit unless lowered).
    snapshot_cap: u64,
}

/// The error for a journal batch the writer refused: [`StoreError::TooLarge`] past the `cap`.
fn append_error(e: std::io::Error, cap: u64) -> StoreError {
    if journal::is_cap_error(&e) {
        StoreError::TooLarge(format!("the journal would grow past the {cap} byte cap; checkpoint first"))
    } else {
        e.into()
    }
}

impl Store {
    /// Opens (creating if missing) a store at `dir`, taking the lock. An existing profile opens in the
    /// storage it has; a new one gets legacy storage.
    pub fn open(dir: &Path) -> Result<Store> {
        Store::open_with(dir, StoreKind::Legacy)
    }

    /// Opens (creating if missing) a store at `dir`, taking the lock. An existing profile opens in the
    /// storage it has ([`Store::kind`] tells which); a new one gets `new_kind`. A new SQLite profile also
    /// gets a marker `mailbox.json` that older MailCraft versions refuse to open, instead of starting an
    /// empty profile next to the database.
    pub fn open_with(dir: &Path, new_kind: StoreKind) -> Result<Store> {
        fs::create_dir_all(dir)?;
        let lock = lock::LockGuard::acquire(dir)?;
        remove_temp_files(dir);
        let kind = StoreKind::of_profile(dir)?.unwrap_or(new_kind);
        match kind {
            StoreKind::Legacy => {
                fs::create_dir_all(dir.join(blob::BLOBS_DIR))?;
                let snapshot_gen = journal::peek_snapshot_generation(dir)?;
                let (journal, generation, repair) = journal::prepare_journal(dir, snapshot_gen)?;
                Ok(Store {
                    dir: dir.to_path_buf(),
                    lock,
                    backend: Backend::Legacy(LegacyFiles {
                        journal,
                        generation: generation.max(snapshot_gen),
                        journal_stale: false,
                    }),
                    repair,
                    journal_cap: journal::MAX_STATE_BYTES,
                    snapshot_cap: journal::MAX_STATE_BYTES,
                })
            }
            #[cfg(not(target_arch = "wasm32"))]
            StoreKind::Sqlite => {
                let db = sql::Db::open(dir)?;
                if !dir.join(journal::SNAPSHOT_FILE).exists() {
                    journal::write_sqlite_marker(dir)?;
                }
                Ok(Store {
                    dir: dir.to_path_buf(),
                    lock,
                    backend: Backend::Sql(Box::new(sql::SqlStore::new(db))),
                    repair: Repair::default(),
                    journal_cap: u64::MAX,
                    snapshot_cap: u64::MAX,
                })
            }
            #[cfg(target_arch = "wasm32")]
            StoreKind::Sqlite => {
                Err(StoreError::Db("SQLite storage is not available in the browser build".into()))
            }
        }
    }

    /// The storage this profile uses.
    pub fn kind(&self) -> StoreKind {
        match &self.backend {
            Backend::Legacy(_) => StoreKind::Legacy,
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Sql(_) => StoreKind::Sqlite,
        }
    }

    /// The database of a SQLite profile (shared: a [`sql::SqlRepo`] or [`BodyStore::packs`] over it uses the
    /// store's own connection).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn db(&self) -> Option<sql::Db> {
        match &self.backend {
            Backend::Sql(s) => Some(s.db().clone()),
            Backend::Legacy(_) => None,
        }
    }

    /// The body store of this profile: the `blobs/` files of a legacy profile, the pack files of a SQLite one.
    pub fn body_store(&self) -> BodyStore {
        match &self.backend {
            Backend::Legacy(_) => BodyStore::on_disk(&self.dir),
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Sql(s) => BodyStore::packs(s.db().clone()),
        }
    }

    /// Loads snapshot + replays the journal.
    pub fn load(&self) -> Result<Mailbox> {
        self.load_report().map(|(mb, _)| mb)
    }

    /// [`Store::load`] plus what recovery did. Message content that a snapshot or journal written before
    /// storage stage S5 carries inline is moved to the profile's body store on the way (idempotent: the
    /// content is keyed by its bytes), so the mailbox returned refers to it by key. A SQLite profile reads its
    /// database.
    pub fn load_report(&self) -> Result<(Mailbox, LoadReport)> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Backend::Sql(s) = &self.backend {
            let mb = s.load()?;
            let report = LoadReport {
                snapshot_generation: s.generation(),
                open_repair: self.repair.clone(),
                ..Default::default()
            };
            return Ok((mb, report));
        }
        let snap = journal::read_snapshot_full(&self.dir)?;
        let snap_gen = snap.generation;
        let mut mb = snap.mailbox;
        let mut report = LoadReport {
            snapshot_generation: snap_gen,
            open_repair: self.repair.clone(),
            legacy_content: snap.legacy.is_some(),
            ..Default::default()
        };
        let mut pending = match &snap.legacy {
            Some(bytes) => legacy::snapshot_inline(bytes)?,
            None => std::collections::BTreeMap::new(),
        };
        drop(snap.legacy);
        let journal_bytes =
            journal::read_capped(&self.dir.join(journal::JOURNAL_FILE), journal::MAX_STATE_BYTES)?;
        if let Some(bytes) = journal_bytes {
            let scan = journal::scan_journal(&bytes);
            drop(bytes);
            if let Some(v) = scan.version.filter(|v| *v > journal::JOURNAL_VERSION) {
                return Err(journal::too_new_error(v));
            }
            if scan.generation.is_some_and(|g| g < snap_gen) {
                log::warn!("journal predates the snapshot; ignoring it");
                report.stale_journal = true;
            } else {
                report.replayed = scan.ops.len();
                report.skipped_lines = scan.skipped_lines;
                report.torn_tail = scan.torn_tail;
                report.legacy_content |= !scan.inline.is_empty();
                let mut inline = scan.inline.into_iter().peekable();
                for (i, op) in scan.ops.into_iter().enumerate() {
                    // What the op replaces loses the content an earlier line carried for it (each op of a
                    // batch in turn).
                    op.for_each_leaf(|op| match op {
                        Op::PutMail(m) => {
                            pending.remove(&m.id);
                        }
                        Op::RemoveMail(id) => {
                            pending.remove(id);
                        }
                        Op::Replace(_) => pending.clear(),
                        _ => {}
                    });
                    while let Some((_, c)) = inline.next_if(|(at, _)| *at == i) {
                        match c {
                            legacy::InlineOp::Mail(id, c) => {
                                pending.insert(id, c);
                            }
                            legacy::InlineOp::Mailbox(map) => pending.extend(map),
                        }
                    }
                    op.apply(&mut mb);
                }
            }
        }
        if report.legacy_content {
            let mut bodies = self.body_store();
            report.migrated_bodies = legacy::apply(&mut mb, pending, &mut bodies)?;
            let dated = legacy::fill_expiry(&mut mb, &bodies);
            log::info!(
                "moved the message content of {} item(s) out of the snapshot and journal ({dated} expiry \
                 date(s) read from sources)",
                report.migrated_bodies
            );
        }
        Ok((mb, report))
    }

    /// Appends ops to the journal (fsync'd). Items in the ops refer to their content by key: whoever made it
    /// stored it in the body store first, so the journal never names content that is not on disk. A SQLite
    /// profile writes them in one transaction.
    pub fn append(&mut self, ops: &[Op]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let cap = self.journal_cap;
        let lf = match &mut self.backend {
            Backend::Legacy(lf) => lf,
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Sql(s) => {
                s.append(ops, cap)?;
                self.lock.refresh_if_due();
                return Ok(());
            }
        };
        if lf.journal_stale {
            // A previous checkpoint wrote its snapshot but could not reset the journal.
            lf.journal = journal::reset_journal(&self.dir, lf.generation)?;
            lf.journal_stale = false;
        }
        let before = lf.journal.metadata()?.len();
        // Open and load refuse a journal past the cap; writing it would make the whole store unopenable. The
        // batch is serialised through a writer capped at the room left, so a refused batch (a whole-mailbox
        // `Replace` can be as large as the cap) is never built in memory first.
        let mut w = journal::CapWriter::new(Vec::new(), cap.saturating_sub(before));
        // More than one op go on one line, as an `Op::Batch` (its serde form, written op by op so the ops
        // are not copied into one first): a crash mid-write then loses the whole batch, not a part of it.
        let batch = ops.len() > 1;
        if batch && let Err(e) = w.write_all(b"{\"Batch\":[") {
            return Err(append_error(e, cap));
        }
        for (i, op) in ops.iter().enumerate() {
            let sep: &[u8] = if batch && i > 0 { b"," } else { b"" };
            let res = w
                .write_all(sep)
                .and_then(|()| serde_json::to_writer(&mut w, op).map_err(std::io::Error::from));
            if let Err(e) = res {
                return Err(append_error(e, cap));
            }
        }
        let end: &[u8] = if batch { b"]}\n" } else { b"\n" };
        if let Err(e) = w.write_all(end) {
            return Err(append_error(e, cap));
        }
        let buf = w.into_inner();
        let written = lf.journal.write_all(&buf).and_then(|()| lf.journal.sync_data());
        if let Err(e) = written {
            // Cut off a partial batch so the next append does not glue its first op onto a torn line.
            if let Err(e2) = lf.journal.set_len(before) {
                log::warn!("could not roll back a failed journal append: {e2}");
            }
            return Err(e.into());
        }
        self.lock.refresh_if_due();
        Ok(())
    }

    /// Writes a fresh snapshot of `mb` and truncates the journal. Items hold no content, so the mailbox is
    /// written as it is (no copy). A SQLite profile writes the rows of `mb` that differ from the database and
    /// folds its write-ahead log into the database file.
    pub fn checkpoint(&mut self, mb: &Mailbox) -> Result<()> {
        let cap = self.snapshot_cap;
        let lf = match &mut self.backend {
            Backend::Legacy(lf) => lf,
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Sql(s) => {
                s.write_all(mb, cap)?;
                if let Err(e) = s.db().checkpoint_wal() {
                    log::warn!("store.db: write-ahead log not folded in: {e}");
                }
                self.lock.refresh_if_due();
                return Ok(());
            }
        };
        let generation = lf.generation.saturating_add(1);
        // Past the cap the snapshot is refused (the old snapshot and the journal stay as they are): `load`
        // could not read it back.
        journal::write_snapshot_capped(&self.dir, generation, mb, cap)?;
        // The snapshot now holds everything; from here on the old journal is stale (open/load ignore it), so
        // nothing may be appended to it even if the reset below fails.
        lf.generation = generation;
        lf.journal_stale = true;
        lf.journal = journal::reset_journal(&self.dir, lf.generation)?;
        lf.journal_stale = false;
        self.lock.refresh_if_due();
        Ok(())
    }

    /// Stores a raw message; returns its blob key.
    pub fn put_blob(&mut self, raw: &[u8]) -> Result<String> {
        self.body_store().put_source(raw)
    }

    pub fn get_blob(&self, key: &str) -> Result<Vec<u8>> {
        match &self.backend {
            Backend::Legacy(_) => bodies::disk_get(&self.dir, key, bodies::SOURCE_EXT, MAX_BLOB_BYTES),
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Sql(_) => self.body_store().source(key),
        }
    }

    /// True when the blob exists on disk (not verified).
    pub fn has_blob(&self, key: &str) -> bool {
        match &self.backend {
            Backend::Legacy(_) => BlobKey::parse(key).is_some_and(|k| k.path(&self.dir).is_file()),
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Sql(_) => self.body_store().has_source(key),
        }
    }

    /// The raw source of `item` from its blob. `Ok(None)` when it has none.
    pub fn raw_of(&self, item: &MailItem) -> Result<Option<Vec<u8>>> {
        match &item.blob {
            Some(key) => self.get_blob(key).map(Some),
            None => Ok(None),
        }
    }

    /// The first `cap` bytes of `item`'s source, for a header scan that should not read whole messages.
    /// Unlike [`Store::raw_of`] the bytes are not checked against the blob key (that needs the whole file); a
    /// blob shorter than its key says is reported as corrupt. `Ok(None)` when the item has no source.
    pub fn head_of(&self, item: &MailItem, cap: usize) -> Result<Option<Vec<u8>>> {
        let Some(key) = &item.blob else { return Ok(None) };
        match &self.backend {
            Backend::Legacy(_) => bodies::disk_head(&self.dir, key, bodies::SOURCE_EXT, cap),
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Sql(_) => self.body_store().source_head(key, cap),
        }
    }

    /// Current size of the journal file in bytes (0 when it cannot be read); for a SQLite profile, of its
    /// write-ahead log.
    pub fn journal_bytes(&self) -> u64 {
        match &self.backend {
            Backend::Legacy(lf) => lf.journal.metadata().map_or(0, |m| m.len()),
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Sql(s) => s.db().wal_bytes(),
        }
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

    /// Generation of the last snapshot written or found (for a SQLite profile, of the last checkpoint).
    pub fn generation(&self) -> u64 {
        match &self.backend {
            Backend::Legacy(lf) => lf.generation,
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Sql(s) => s.generation(),
        }
    }

    /// Repairs made to the journal by [`Store::open`].
    pub fn open_repair(&self) -> &Repair {
        &self.repair
    }

    /// Imports one `.eml` file into `folder`: the raw source goes to the profile's body store (`item.blob`
    /// set). Not journaled: the caller appends `Op::PutMail` (or checkpoints) as for any other change.
    pub fn import_eml(&mut self, mb: &mut Mailbox, path: &Path, folder: FolderId) -> Result<ItemId> {
        let mut bodies = self.body_store();
        import::import_eml(mb, &mut bodies, path, folder)
    }

    /// [`export_mbox`] reading the content from the profile's body store.
    pub fn export_mbox(&self, items: &[&MailItem], file: &Path) -> Result<()> {
        export_mbox(items, &self.body_store(), file)
    }

    /// Reads the recoverable-items bin (`Ok(default)` when there is none yet). Content a bin written before
    /// storage stage S5 carries inline is moved to the profile's body store on the way (the file itself is
    /// rewritten by the next [`Store::save_recoverable`]).
    pub fn load_recoverable(&self) -> Result<RecoverBin> {
        let path = self.dir.join(RECOVERABLE_FILE);
        let Some(bytes) = journal::read_capped(&path, MAX_RECOVERABLE_BYTES)? else {
            return Ok(RecoverBin::default());
        };
        let mut bin: RecoverBin = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Corrupt(format!("{}: {e}", path.display())))?;
        if legacy::has_inline(&bytes) {
            let inline = legacy::recoverable_inline(&bytes)?;
            drop(bytes);
            let mut bodies = self.body_store();
            for (i, id, content) in inline {
                if let Some(e) = bin.items.get_mut(i)
                    && let DeletedItem::Mail(m) = &mut e.item
                    && m.id == id
                {
                    legacy::store_inline(m, content, &mut bodies)?;
                }
            }
        }
        Ok(bin)
    }

    /// Writes the recoverable-items bin atomically. Its mail entries refer to their content in the body store
    /// by key (the store's collection keeps what they refer to).
    pub fn save_recoverable(&mut self, bin: &mut RecoverBin) -> Result<()> {
        let bytes = serde_json::to_vec(&*bin).map_err(std::io::Error::other)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECOVERABLE_BYTES {
            return Err(StoreError::TooLarge(format!(
                "the recoverable items would take {} bytes, past the {MAX_RECOVERABLE_BYTES} byte cap",
                bytes.len()
            )));
        }
        journal::write_atomic(&self.dir.join(RECOVERABLE_FILE), |w| w.write_all(&bytes))?;
        Ok(())
    }

    /// Deletes stored content (sources and body records) referred to neither by `mb` (which must be the
    /// complete current state), nor by the recoverable-items bin, nor by a key in `also` (the undo history),
    /// and leftover temp files. Returns the number of entries removed.
    pub fn gc_blobs(&mut self, mb: &Mailbox, also: &BTreeSet<String>) -> Result<usize> {
        // A bin that cannot be read keeps every blob (nothing it may still need is removed).
        let bin = self.load_recoverable()?;
        let mut live = bodies::keys_of(mb.mail.items());
        live.extend(bodies::keys_of(bin.items.iter().filter_map(|e| match &e.item {
            DeletedItem::Mail(m) => Some(&**m),
            _ => None,
        })));
        live.extend(also.iter().cloned());
        self.body_store().collect(&live)
    }
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
/// the file's mtime. Each source goes straight to `bodies` (items hold its key, not the bytes). Unreadable or
/// oversized files are skipped; the import stops (with a warning) after [`import::MAX_IMPORT_BYTES`] of
/// source.
pub fn import_maildir(
    mb: &mut Mailbox,
    bodies: &mut BodyStore,
    dir: &Path,
    folder: FolderId,
) -> Result<Vec<ItemId>> {
    import::import_maildir(mb, bodies, dir, folder)
}

/// Imports an MBOX file into `folder`; returns new item ids.
///
/// `received` comes from the `From ` line date; `Status`/`X-Status` headers set read/flag/replied state. Each
/// source goes straight to `bodies`. Messages over the size cap are skipped with a warning; the import stops
/// (with a warning) after [`mbox::MAX_MESSAGES`] messages or [`import::MAX_IMPORT_BYTES`] of source.
pub fn import_mbox(
    mb: &mut Mailbox,
    bodies: &mut BodyStore,
    file: &Path,
    folder: FolderId,
) -> Result<Vec<ItemId>> {
    import::import_mbox(mb, bodies, file, folder)
}

/// Exports items as mboxrd: each item's source from `bodies`, else a message rebuilt from its fields and body.
pub fn export_mbox(items: &[&MailItem], bodies: &BodyStore, file: &Path) -> Result<()> {
    journal::write_atomic(file, |w| {
        for item in items {
            let data = match bodies.source_of(item) {
                Ok(Some(raw)) => raw,
                other => {
                    if let Err(e) = other {
                        log::warn!(
                            "export: source of item {:?} unavailable ({e}); rebuilding from fields",
                            item.id
                        );
                    }
                    let body = bodies.body_of(item).ok().flatten();
                    rebuild_message(item, body.as_deref())
                }
            };
            let sender = item.from.as_ref().or(item.sender.as_ref()).map(|m| m.addr.as_str()).unwrap_or("");
            let date =
                if item.received.unix != 0 { item.received } else { item.sent.unwrap_or(item.received) };
            mbox::write_message(w, sender, date, &data)?;
        }
        Ok(())
    })
}

/// Rebuilds RFC 5322 bytes from an item's fields and its body (the message builder, with a minimal fallback).
pub fn rebuild_message(item: &MailItem, body: Option<&Body>) -> Vec<u8> {
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
        text: body.and_then(|b| b.text.clone()),
        html: body.and_then(|b| b.html.clone()),
        keep_bcc: true,
        ..Default::default()
    };
    let built = builder.build();
    if built.is_empty() { minimal_message(item, body) } else { built }
}

fn header_safe(s: &str) -> String {
    s.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect()
}

/// Plain UTF-8 message (RFC 6532 headers, 8bit body) used when the builder produced nothing.
fn minimal_message(item: &MailItem, body: Option<&Body>) -> Vec<u8> {
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
    let none = (None, None);
    let parts = body.map_or(none, |b| (b.text.as_ref(), b.html.as_ref()));
    let (ctype, body) = match parts {
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
