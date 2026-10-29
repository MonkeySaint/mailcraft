//! The SQLite store of a native profile (storage stage S6, `plan/storage-adr.md` §2 and §3.2).
//!
//! One database per profile, `store.db`, opened with `journal_mode=WAL`, `synchronous=FULL`,
//! `foreign_keys=ON`, a 64 MiB page cache and a 256 MiB memory map. It holds the mail metadata
//! ([`SqlRepo`]), the folders, per-folder counts, configuration, sync state, calendar, contact, task and note
//! items ([`SqlStore`]), and the index of the message content kept in the zstd pack files next to it
//! ([`crate::packs`]). Schema: [`schema`].
//!
//! [`Db`] is the shared handle: the store, its repository and every [`crate::BodyStore`] over its packs use
//! one connection behind a mutex, so their writes never contend for SQLite's write lock. Lock order when both
//! are needed: the pack writer first, then the connection.

pub mod cache;
pub mod repo;
pub mod rows;
pub mod schema;
pub mod state;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::packs::PackWriter;
use crate::{Result, StoreError};

pub use repo::SqlRepo;
pub use state::SqlStore;

/// The database file of a SQLite profile.
pub const DB_FILE: &str = crate::SQL_DB_FILE;
/// Page cache size (`PRAGMA cache_size`, negative = KiB).
pub const CACHE_KIB: i64 = 64 * 1024;
/// Size the write-ahead log is cut back to after a checkpoint (`PRAGMA journal_size_limit`).
pub const JOURNAL_SIZE_LIMIT: i64 = 32 << 20;
/// Memory-mapped I/O window (`PRAGMA mmap_size`).
pub const MMAP_BYTES: i64 = 256 << 20;

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Db(e.to_string())
    }
}

/// `u64` ids as SQLite integers: the same 64 bits (ids past `i64::MAX` read back as negative numbers and are
/// turned back on the way out). Never fails.
pub(crate) fn to_sql_id(id: u64) -> i64 {
    i64::from_ne_bytes(id.to_ne_bytes())
}

/// The inverse of [`to_sql_id`].
pub(crate) fn from_sql_id(v: i64) -> u64 {
    u64::from_ne_bytes(v.to_ne_bytes())
}

/// A non-negative SQLite integer as a count (0 for a negative or hostile value).
pub(crate) fn count(v: i64) -> usize {
    usize::try_from(v).unwrap_or(0)
}

/// A byte count or size as a SQLite integer (saturating).
pub(crate) fn sql_len(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

pub(crate) struct DbInner {
    dir: PathBuf,
    conn: Mutex<Connection>,
    packs: Mutex<PackWriter>,
    cache: Mutex<cache::MetaCache>,
    /// Names savepoints uniquely on the shared connection.
    next_savepoint: std::sync::atomic::AtomicU64,
}

/// A shared handle on a profile's `store.db` and its pack files. Cloning shares the connection.
#[derive(Clone)]
pub struct Db(Arc<DbInner>);

impl fmt::Debug for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Db").field("dir", &self.0.dir).finish()
    }
}

impl Db {
    /// Opens (creating when missing) `store.db` in the profile folder `dir`: settings, schema (created or
    /// brought up to [`schema::VERSION`]), and the crash rule on the pack files (each cut back to the length
    /// the database committed).
    pub fn open(dir: &Path) -> Result<Db> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(DB_FILE);
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&path, flags)?;
        configure(&conn)?;
        schema::migrate(&conn)?;
        let packs = PackWriter::open(dir, &conn)?;
        Ok(Db(Arc::new(DbInner {
            dir: dir.to_path_buf(),
            conn: Mutex::new(conn),
            packs: Mutex::new(packs),
            cache: Mutex::new(cache::MetaCache::new(cache::DEFAULT_CACHE_BYTES)),
            next_savepoint: std::sync::atomic::AtomicU64::new(1),
        })))
    }

    /// The profile folder.
    pub fn dir(&self) -> &Path {
        &self.0.dir
    }

    /// The connection. A poisoned lock (a panic while it was held) still hands out the connection: SQLite
    /// rolled back whatever statement was running, and `Session::run` restores the mailbox.
    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.0.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The pack writer (take it before [`Db::conn`] when both are needed).
    pub(crate) fn packs(&self) -> MutexGuard<'_, PackWriter> {
        self.0.packs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The shared cache of mail items read (never held while taking another lock).
    pub(crate) fn cache(&self) -> MutexGuard<'_, cache::MetaCache> {
        self.0.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A savepoint name no other open savepoint on this connection has.
    pub(crate) fn savepoint_name(&self, prefix: &str) -> String {
        let n = self.0.next_savepoint.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("{prefix}_{n}")
    }

    /// Whether two handles share one database.
    pub fn same(&self, other: &Db) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Size of the write-ahead log in bytes (0 when there is none).
    pub fn wal_bytes(&self) -> u64 {
        std::fs::metadata(self.0.dir.join(format!("{DB_FILE}-wal"))).map_or(0, |m| m.len())
    }

    /// Copies the write-ahead log into the database and empties it (`PRAGMA wal_checkpoint(TRUNCATE)`).
    pub fn checkpoint_wal(&self) -> Result<()> {
        let conn = self.conn();
        // Returns (busy, log frames, checkpointed frames); busy only with other connections, which this
        // process does not open.
        let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
        if busy != 0 {
            log::warn!("store.db: the write-ahead log could not be checkpointed completely");
        }
        Ok(())
    }

    /// Runs raw SQL on the database (tests damage rows with it).
    #[doc(hidden)]
    pub fn execute_raw(&self, sql: &str) -> Result<()> {
        self.conn().execute_batch(sql)?;
        Ok(())
    }

    /// `PRAGMA integrity_check` ("ok" when the database is sound), for tests and diagnostics.
    pub fn integrity_check(&self) -> Result<String> {
        let conn = self.conn();
        let mut stmt = conn.prepare("PRAGMA integrity_check")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows.take(100) {
            out.push(r?);
        }
        Ok(out.join("\n"))
    }
}

/// Connection settings (storage ADR, Decision).
fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        // A file system without shared memory (some network shares) refuses WAL; the rollback journal is
        // still crash safe, just slower.
        log::warn!("store.db: write-ahead logging unavailable, using journal mode {mode}");
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    // A large transaction (a whole mailbox written at once) grows the log; checkpoints cut it back to this.
    conn.query_row(&format!("PRAGMA journal_size_limit={JOURNAL_SIZE_LIMIT}"), [], |_| Ok(()))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Deleted rows are overwritten, not left in free pages: mail deleted for good (or that of a closed
    // protected data file) does not stay readable in the file.
    conn.pragma_update(None, "secure_delete", "ON")?;
    conn.pragma_update(None, "cache_size", -CACHE_KIB)?;
    // mmap is an optimisation; a platform without it keeps working.
    if let Err(e) = conn.query_row(&format!("PRAGMA mmap_size={MMAP_BYTES}"), [], |_| Ok(())) {
        log::debug!("store.db: mmap_size not set: {e}");
    }
    conn.set_prepared_statement_cache_capacity(64);
    Ok(())
}

/// Runs `f` inside a savepoint named `name`: released (committed when it is the outermost one) when `f`
/// succeeds, rolled back when it fails. Nothing `f` wrote survives an error.
pub(crate) fn in_savepoint<T>(
    conn: &Connection,
    name: &str,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    conn.execute_batch(&format!("SAVEPOINT {name}"))?;
    match f(conn) {
        Ok(v) => match conn.execute_batch(&format!("RELEASE {name}")) {
            Ok(()) => Ok(v),
            Err(e) => {
                rollback_savepoint(conn, name);
                Err(e.into())
            }
        },
        Err(e) => {
            rollback_savepoint(conn, name);
            Err(e)
        }
    }
}

/// Rolls back and closes savepoint `name` (best effort: an error leaves SQLite's own rollback in charge).
pub(crate) fn rollback_savepoint(conn: &Connection, name: &str) {
    if let Err(e) = conn.execute_batch(&format!("ROLLBACK TO {name}; RELEASE {name}")) {
        log::error!("store.db: could not roll back savepoint {name}: {e}");
    }
}

#[cfg(test)]
mod tests;
