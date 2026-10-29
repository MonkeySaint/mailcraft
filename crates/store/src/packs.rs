//! Message content in append-only pack files (storage stage S6, `plan/storage-adr.md` §2.3).
//!
//! A SQLite profile keeps message sources and body records in `packs/NNNNNN.pack`, indexed by the `bodies`
//! table of `store.db`. Each entry is one record:
//!
//! ```text
//! [magic "MCPK"][version u8][codec u8: 0 store, 1 zstd][kind u8: 0 source, 1 body record]
//! [raw_len u64][stored_len u64][sha256 32 B][crc32 u32 of the payload][payload]
//! ```
//!
//! (integers little-endian). Payloads are zstd level 3 unless that does not make them smaller. Entries are
//! de-duplicated by SHA-256 per kind; the key an item holds keeps the [`BlobKey`] grammar of the content (so
//! keys mean the same in every store and undo can put one back), probing past a different entry that owns it.
//! Packs roll over at [`PACK_ROLL_BYTES`].
//!
//! **Crash rule.** Records are appended, the pack is fsynced, and only then does one transaction insert their
//! `bodies` rows and move the pack's `committed_len` past them. Opening cuts every pack back to its
//! `committed_len`: whatever a crash left past it was never referenced. Those bytes, and pack files without a
//! row, are first moved to `packs/lost+found/` (they only hold mail when `store.db` lost rows, but then they
//! do); a pack whose row is not believable is left alone ([`Recovery`]). A batch
//! ([`PackWriter::begin_batch`]) defers the fsync and the transaction to [`PackWriter::finish_batch`], one for
//! many records; a write that may not have reached the disk fails the whole batch.
//!
//! Every read is capped by its kind's limit, checked against its row (header fields), the CRC of the stored
//! bytes and the SHA-256 of the decoded ones; a damaged entry is an error, never a panic. Dead bytes (entries
//! no longer referenced) are counted per pack; compaction comes with storage stage S10.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::blob::{self, BlobKey};
use crate::bodies::MAX_BODY_RECORD_BYTES;
use crate::sql::{Db, count, sql_len};
use crate::{MAX_BLOB_BYTES, Result, StoreError};

/// Folder of the pack files in a SQLite profile.
pub const PACKS_DIR: &str = "packs";
/// A pack takes no new record once it is this long.
pub const PACK_ROLL_BYTES: u64 = 256 << 20;
/// Record magic.
pub const MAGIC: [u8; 4] = *b"MCPK";
/// Record format version.
pub const VERSION: u8 = 1;
/// Bytes of a record header.
pub const HEADER_LEN: usize = 4 + 1 + 1 + 1 + 8 + 8 + 32 + 4;
/// Codec: the payload is the content.
pub const CODEC_STORE: u8 = 0;
/// Codec: the payload is a zstd frame of the content.
pub const CODEC_ZSTD: u8 = 1;
/// zstd compression level.
pub const ZSTD_LEVEL: i32 = 3;

/// What a pack record holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// An RFC 5322 message source (an item's `blob`).
    Source = 0,
    /// A serialised body record (an item's `body_key`).
    Body = 1,
}

impl Kind {
    fn code(self) -> u8 {
        self as u8
    }
    fn sql(self) -> i64 {
        i64::from(self.code())
    }
    /// Largest content of this kind read or written.
    fn cap(self) -> usize {
        match self {
            Kind::Source => MAX_BLOB_BYTES,
            Kind::Body => MAX_BODY_RECORD_BYTES,
        }
    }
    fn what(self) -> &'static str {
        match self {
            Kind::Source => "source",
            Kind::Body => "message body",
        }
    }
}

/// A record header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub codec: u8,
    pub kind: u8,
    pub raw_len: u64,
    pub stored_len: u64,
    pub sha256: [u8; 32],
    pub crc32: u32,
}

impl Header {
    /// The header's bytes.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        let mut w = 0usize;
        let mut put = |bytes: &[u8]| {
            for b in bytes {
                if let Some(slot) = out.get_mut(w) {
                    *slot = *b;
                }
                w = w.saturating_add(1);
            }
        };
        put(&MAGIC);
        put(&[VERSION, self.codec, self.kind]);
        put(&self.raw_len.to_le_bytes());
        put(&self.stored_len.to_le_bytes());
        put(&self.sha256);
        put(&self.crc32.to_le_bytes());
        out
    }

    /// Parses header bytes; `None` for a wrong magic, version or length.
    pub fn decode(bytes: &[u8]) -> Option<Header> {
        if bytes.len() != HEADER_LEN || bytes.get(..4)? != MAGIC || *bytes.get(4)? != VERSION {
            return None;
        }
        let u64_at =
            |at: usize| -> Option<u64> { Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?)) };
        Some(Header {
            codec: *bytes.get(5)?,
            kind: *bytes.get(6)?,
            raw_len: u64_at(7)?,
            stored_len: u64_at(15)?,
            sha256: bytes.get(23..55)?.try_into().ok()?,
            crc32: u32::from_le_bytes(bytes.get(55..59)?.try_into().ok()?),
        })
    }
}

/// SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Path of pack `id` in the profile folder `dir`.
pub fn pack_path(dir: &Path, id: i64) -> PathBuf {
    dir.join(PACKS_DIR).join(format!("{id:06}.pack"))
}

/// Where an entry is.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Location {
    pack: i64,
    off: u64,
    raw_len: u64,
    stored_len: u64,
    codec: u8,
    sha256: [u8; 32],
}

/// A `bodies` row whose entry cannot be read back: storing equal content again rewrites it in place (same
/// row, key and references, a new place in a pack) instead of de-duplicating to it.
#[derive(Clone, Debug)]
struct Stale {
    row: i64,
    key: String,
    /// Pack and record bytes (header included) the row pointed at.
    pack: i64,
    record: i64,
}

/// An entry written to a pack during a batch, not yet in `bodies`.
#[derive(Clone, Debug)]
struct Pending {
    kind: Kind,
    key: String,
    loc: Location,
    /// The broken row this entry replaces.
    stale: Option<Stale>,
}

#[derive(Debug)]
struct OpenPack {
    id: i64,
    file: File,
    /// End of the file: the committed length plus records written and not yet committed.
    len: u64,
    /// What `packs.committed_len` says.
    committed: u64,
}

#[derive(Debug, Default)]
struct Batch {
    pending: Vec<Pending>,
    by_key: HashMap<(Kind, String), usize>,
    by_sha: HashMap<(Kind, [u8; 32]), usize>,
    /// Packs filled up and closed during the batch: id → (committed length, end offset).
    rolled: BTreeMap<i64, (u64, u64)>,
    /// Why the batch failed: its records were dropped, every later write of it is refused and
    /// [`PackWriter::finish_batch`] reports the failure.
    failed: Option<String>,
}

/// Faults tests inject into the pack writer.
#[derive(Debug, Default)]
struct Faults {
    /// The next fsync of a pack fails.
    #[cfg(test)]
    fail_sync: bool,
}

impl Faults {
    /// `sync_data` of a pack file (or the fault a test asked for).
    fn sync(&mut self, file: &File) -> std::io::Result<()> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_sync) {
            return Err(std::io::Error::other("injected fsync failure"));
        }
        file.sync_data()
    }
}

/// Appends records to the current pack (one per profile, shared through [`Db`]).
#[derive(Debug)]
pub struct PackWriter {
    dir: PathBuf,
    current: Option<OpenPack>,
    batch: Option<Batch>,
    /// A pack takes no new record past this length ([`PACK_ROLL_BYTES`]; lowered by tests).
    roll_bytes: u64,
    /// What opening repaired.
    recovery: Recovery,
    faults: Faults,
}

/// What opening found and repaired. Nothing is deleted on doubtful evidence: bytes the database does not
/// account for are moved to `packs/lost+found/` ([`LOST_FOUND_DIR`]) before a pack is cut back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    /// Packs cut back to their committed length, and the bytes removed (saved to `lost+found` first).
    pub truncated: Vec<(i64, u64)>,
    /// Packs left exactly as they were: shorter than their committed length, missing, with a committed
    /// length that is not a valid length, or with a tail that could not be saved. No record is appended to
    /// them, and content stored again is written afresh instead of de-duplicated to a damaged entry in them.
    pub short: Vec<i64>,
    /// Pack files the database has no row for, moved to `lost+found` (a crash cannot leave one: the row is
    /// committed before the file is created, so the database lost rows; their content is kept).
    pub orphans: usize,
}

/// Where opening keeps the pack bytes it would otherwise have to discard (inside [`PACKS_DIR`]).
pub const LOST_FOUND_DIR: &str = "lost+found";

/// Moves `path` into `lost` under its own name, or with `.1`, `.2`, … when that is taken.
fn move_to_lost_found(lost: &Path, path: &Path) -> std::io::Result<PathBuf> {
    fs::create_dir_all(lost)?;
    let name = path.file_name().map_or_else(|| "pack".to_owned(), |n| n.to_string_lossy().into_owned());
    for n in 0..1_000u32 {
        let to = if n == 0 { lost.join(&name) } else { lost.join(format!("{name}.{n}")) };
        if to.exists() {
            continue;
        }
        fs::rename(path, &to)?;
        crate::journal::sync_dir(lost);
        if let Some(parent) = path.parent() {
            crate::journal::sync_dir(parent);
        }
        return Ok(to);
    }
    Err(std::io::Error::new(ErrorKind::AlreadyExists, "lost+found is full"))
}

/// Copies the bytes of the pack at `path` past `from` into `lost` (as `NNNNNN-<from>.tail`) and syncs them.
fn save_tail(lost: &Path, path: &Path, id: i64, from: u64) -> std::io::Result<PathBuf> {
    fs::create_dir_all(lost)?;
    let to = lost.join(format!("{id:06}-{from}.tail"));
    let mut src = File::open(path)?;
    src.seek(SeekFrom::Start(from))?;
    let mut dst = OpenOptions::new().write(true).create(true).truncate(true).open(&to)?;
    std::io::copy(&mut src, &mut dst)?;
    dst.sync_all()?;
    crate::journal::sync_dir(lost);
    Ok(to)
}

impl PackWriter {
    /// Applies the crash rule to every pack of the profile in `dir` and opens the last one for appending.
    pub(crate) fn open(dir: &Path, conn: &Connection) -> Result<PackWriter> {
        let mut w = PackWriter {
            dir: dir.to_path_buf(),
            current: None,
            batch: None,
            roll_bytes: PACK_ROLL_BYTES,
            recovery: Recovery::default(),
            faults: Faults::default(),
        };
        let rec = w.recover(conn)?;
        if rec != Recovery::default() {
            log::warn!("pack files repaired on open: {rec:?}");
        }
        w.recovery = rec;
        Ok(w)
    }

    fn lost_found(&self) -> PathBuf {
        self.dir.join(PACKS_DIR).join(LOST_FOUND_DIR)
    }

    fn recover(&mut self, conn: &Connection) -> Result<Recovery> {
        let packs_dir = self.dir.join(PACKS_DIR);
        fs::create_dir_all(&packs_dir)?;
        let lost = self.lost_found();
        let mut rec = Recovery::default();
        // (id, committed length: None when the row's value is not a length)
        let mut known: Vec<(i64, Option<u64>)> = Vec::new();
        {
            let mut stmt = conn.prepare("SELECT id, committed_len FROM packs ORDER BY id")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, rusqlite::types::Value>(1)?)))?;
            for r in rows {
                let (id, committed) = r?;
                let committed = match committed {
                    rusqlite::types::Value::Integer(n) => u64::try_from(n).ok(),
                    _ => None,
                };
                known.push((id, committed));
            }
        }
        for (id, committed) in &known {
            let path = pack_path(&self.dir, *id);
            let Some(committed) = *committed else {
                log::error!("pack {id:06}: its committed length is not a length; the pack is left as it is");
                rec.short.push(*id);
                continue;
            };
            match fs::metadata(&path) {
                Ok(m) if m.len() > committed => {
                    // Normally a crash's uncommitted tail; but when store.db lost rows (restored from a backup,
                    // copied without its log) it is mail, so it is saved before the pack is cut back.
                    let extra = m.len().saturating_sub(committed);
                    match save_tail(&lost, &path, *id, committed) {
                        Ok(to) => {
                            log::warn!(
                                "pack {id:06}: {extra} bytes past its committed length moved to {}",
                                to.display()
                            );
                            let f = OpenOptions::new().write(true).open(&path)?;
                            f.set_len(committed)?;
                            f.sync_all()?;
                            rec.truncated.push((*id, extra));
                        }
                        Err(e) => {
                            log::error!(
                                "pack {id:06}: the {extra} bytes past its committed length could not be saved \
                                 ({e}); the pack is left as it is"
                            );
                            rec.short.push(*id);
                        }
                    }
                }
                Ok(m) if m.len() < committed => {
                    log::error!("pack {id:06} is shorter than recorded ({} of {committed} bytes)", m.len());
                    rec.short.push(*id);
                }
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    if committed > 0 {
                        log::error!("pack {id:06} is missing ({committed} bytes recorded)");
                        rec.short.push(*id);
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        // A file without a row: the database lost rows (a row is committed before its file is created). It
        // is kept, out of the way of a new pack that takes its number.
        if let Ok(rd) = fs::read_dir(&packs_dir) {
            for entry in rd.flatten().take(100_000) {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(stem) = name.strip_suffix(".pack") else { continue };
                if stem.len() < 6 || !stem.bytes().all(|b| b.is_ascii_digit()) {
                    continue;
                }
                let Ok(id) = stem.parse::<i64>() else { continue };
                if known.iter().any(|(k, _)| *k == id) {
                    continue;
                }
                match move_to_lost_found(&lost, &entry.path()) {
                    Ok(to) => {
                        log::warn!("pack file {name} has no row in store.db; moved to {}", to.display());
                        rec.orphans = rec.orphans.saturating_add(1);
                    }
                    Err(e) => {
                        log::error!("pack file {name} has no row in store.db and could not be moved: {e}")
                    }
                }
            }
        }
        // Append to the last pack while it has room.
        if let Some((id, Some(committed))) = known.last().copied()
            && committed < self.roll_bytes
            && !rec.short.contains(&id)
        {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(pack_path(&self.dir, id))?;
            self.current = Some(OpenPack { id, file, len: committed, committed });
        }
        Ok(rec)
    }

    /// Starts a batch: records are written but neither fsynced nor indexed until
    /// [`PackWriter::finish_batch`]. Reads see them meanwhile. A batch already open stays open.
    pub fn begin_batch(&mut self) {
        if self.batch.is_none() {
            self.batch = Some(Batch::default());
        }
    }

    /// Whether a batch is open.
    pub fn in_batch(&self) -> bool {
        self.batch.is_some()
    }

    /// Fsyncs the packs a batch wrote and indexes its records in one transaction. On failure (here, or of an
    /// earlier write of the batch that may not have reached the disk) every record of the batch is dropped
    /// (cut off the packs) and their keys refer to nothing. Nothing to do without a batch.
    pub fn finish_batch(&mut self, conn: &Connection) -> Result<usize> {
        let Some(batch) = self.batch.take() else { return Ok(0) };
        if let Some(why) = &batch.failed {
            return Err(batch_failed(why));
        }
        if batch.pending.is_empty() {
            return Ok(0);
        }
        let n = batch.pending.len();
        let mut ends: BTreeMap<i64, u64> = batch.rolled.iter().map(|(id, (_, end))| (*id, *end)).collect();
        if let Some(p) = self.current.as_ref() {
            ends.insert(p.id, p.len);
        }
        let synced = match self.current.as_ref() {
            Some(p) => self.faults.sync(&p.file).map_err(StoreError::from),
            None => Ok(()),
        };
        let res = synced.and_then(|()| {
            crate::sql::in_savepoint(conn, "packs_batch", |c| {
                for p in &batch.pending {
                    match &p.stale {
                        Some(s) => rewrite_row(c, p.kind, s, &p.loc)?,
                        None => insert_row(c, p.kind, &p.key, &p.loc)?,
                    }
                }
                for (id, end) in &ends {
                    c.execute(
                        "UPDATE packs SET committed_len = ?1 WHERE id = ?2",
                        params![sql_len(*end), id],
                    )?;
                }
                Ok(())
            })
        });
        match res {
            Ok(()) => {
                if let Some(p) = self.current.as_mut() {
                    p.committed = p.len;
                }
                Ok(n)
            }
            Err(e) => {
                self.cut_rolled(&batch.rolled);
                self.cut_back();
                Err(e)
            }
        }
    }

    /// Cuts the current pack back to its committed length (after a failed write or commit).
    fn cut_back(&mut self) {
        if let Some(p) = self.current.as_mut() {
            if let Err(e) = p.file.set_len(p.committed) {
                log::warn!("pack {:06}: could not cut off an uncommitted tail: {e}", p.id);
            }
            p.len = p.committed;
        }
    }

    /// Cuts the packs a failed batch rolled past back to their committed lengths (the next open would too).
    fn cut_rolled(&self, rolled: &BTreeMap<i64, (u64, u64)>) {
        for (id, (committed, _)) in rolled {
            let cut = OpenOptions::new()
                .write(true)
                .open(pack_path(&self.dir, *id))
                .and_then(|f| f.set_len(*committed));
            if let Err(e) = cut {
                log::warn!("pack {id:06}: could not cut off an uncommitted tail: {e}");
            }
        }
    }

    /// Marks the open batch failed: its records are dropped and cut off the packs, later writes of it are
    /// refused, and [`PackWriter::finish_batch`] reports `why`.
    fn fail_batch(&mut self, why: String) {
        let Some(b) = self.batch.as_mut() else { return };
        log::error!("pack batch failed, its {} records are dropped: {why}", b.pending.len());
        b.pending.clear();
        b.by_key.clear();
        b.by_sha.clear();
        let rolled = std::mem::take(&mut b.rolled);
        if b.failed.is_none() {
            b.failed = Some(why);
        }
        self.cut_rolled(&rolled);
        self.cut_back();
    }

    /// Makes sure there is a current pack to append `need` bytes to: the open one, or a new one when that is
    /// full (or there is none).
    fn pack_for(&mut self, conn: &Connection, need: u64) -> Result<()> {
        let full =
            self.current.as_ref().is_none_or(|p| p.len > 0 && p.len.saturating_add(need) > self.roll_bytes);
        if !full {
            return Ok(());
        }
        if let Some(old) = self.current.take()
            && old.len > old.committed
        {
            match self.batch.as_mut() {
                Some(batch) => {
                    // Recorded before anything can fail, so finish_batch commits (or drops) its records.
                    batch.rolled.insert(old.id, (old.committed, old.len));
                    if let Err(e) = self.faults.sync(&old.file) {
                        // Records of the batch in it may not be on disk: none of them can be indexed.
                        self.fail_batch(format!("pack {:06} could not be synced: {e}", old.id));
                        return Err(e.into());
                    }
                }
                // Outside a batch every record is committed as it is written; never leave uncommitted bytes.
                None => {
                    if let Err(e) = old.file.set_len(old.committed) {
                        log::warn!("pack {:06}: could not cut off an uncommitted tail: {e}", old.id);
                    }
                }
            }
        }
        conn.execute("INSERT INTO packs(name, committed_len) VALUES ('', 0)", [])?;
        let id = conn.last_insert_rowid();
        let path = pack_path(&self.dir, id);
        conn.execute(
            "UPDATE packs SET name = ?1 WHERE id = ?2",
            params![path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), id],
        )?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let create = || OpenOptions::new().read(true).write(true).create_new(true).open(&path);
        let file = match create() {
            Ok(f) => f,
            // A file the database has no row for (it lost rows): kept, never overwritten.
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let to = move_to_lost_found(&self.lost_found(), &path)?;
                log::warn!("pack file {} had no row in store.db; moved to {}", path.display(), to.display());
                create()?
            }
            Err(e) => return Err(e.into()),
        };
        crate::journal::sync_dir(&self.dir.join(PACKS_DIR));
        self.current = Some(OpenPack { id, file, len: 0, committed: 0 });
        Ok(())
    }

    fn pending(&self, kind: Kind, key: &str) -> Option<&Pending> {
        let b = self.batch.as_ref()?;
        b.by_key.get(&(kind, key.to_owned())).and_then(|i| b.pending.get(*i))
    }

    /// The committed entry of `kind` with hash `sha`, when there is one: where it is (an error for a row
    /// whose fields are not valid) and the row it would be repaired in.
    fn existing(
        &self,
        conn: &Connection,
        kind: Kind,
        sha: &[u8; 32],
    ) -> Result<Option<(Stale, Result<Location>)>> {
        let row = conn
            .prepare_cached(
                "SELECT id, key, pack, off, raw_len, stored_len, codec FROM bodies \
                 WHERE kind = ?1 AND sha256 = ?2",
            )?
            .query_row(params![kind.sql(), &sha[..]], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            })
            .optional()?;
        let Some((row, key, pack, off, raw_len, stored_len, codec)) = row else { return Ok(None) };
        let loc = location_of(kind, &key, [pack, off, raw_len, stored_len, codec], &sha[..]);
        let stale = Stale { row, key, pack, record: stored_len.saturating_add(HEADER_LEN as i64) };
        Ok(Some((stale, loc)))
    }

    /// Whether the committed entry at `loc` can be read back: its record header matches its row, and in a
    /// pack opening flagged, its whole content verifies.
    fn readable(&self, kind: Kind, key: &str, loc: &Location) -> bool {
        if self.recovery.short.contains(&loc.pack) {
            read_entry(&self.dir, kind, key, loc).is_ok()
        } else {
            open_record(&self.dir, kind, key, loc).is_ok()
        }
    }

    /// Stores `data` as an entry of `kind`; returns its key (that of the entry holding equal content when it
    /// can be read back; one that cannot is written again under its own key and row).
    pub(crate) fn put(&mut self, conn: &Connection, kind: Kind, data: &[u8]) -> Result<String> {
        if data.len() > kind.cap() {
            return Err(StoreError::TooLarge(format!(
                "{} of {} bytes exceeds the {} byte cap",
                kind.what(),
                data.len(),
                kind.cap()
            )));
        }
        if let Some(why) = self.batch.as_ref().and_then(|b| b.failed.as_deref()) {
            return Err(batch_failed(why));
        }
        let sha = sha256(data);
        if let Some(b) = self.batch.as_ref()
            && let Some(p) = b.by_sha.get(&(kind, sha)).and_then(|i| b.pending.get(*i))
        {
            return Ok(p.key.clone());
        }
        let stale = match self.existing(conn, kind, &sha)? {
            Some((stale, loc)) => {
                if loc.as_ref().is_ok_and(|l| self.readable(kind, &stale.key, l)) {
                    return Ok(stale.key);
                }
                log::warn!("{} {} cannot be read back; storing it again", kind.what(), stale.key);
                Some(stale)
            }
            None => None,
        };
        let key = match &stale {
            Some(s) => s.key.clone(),
            None => self.free_key(conn, kind, data)?,
        };
        let compressed = zstd::bulk::compress(data, ZSTD_LEVEL).ok().filter(|c| c.len() < data.len());
        let (codec, payload) = match &compressed {
            Some(c) => (CODEC_ZSTD, c.as_slice()),
            None => (CODEC_STORE, data),
        };
        let raw_len = u64::try_from(data.len()).unwrap_or(u64::MAX);
        let stored_len = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        let header = Header {
            codec,
            kind: kind.code(),
            raw_len,
            stored_len,
            sha256: sha,
            crc32: crc32fast::hash(payload),
        };
        let need = stored_len.saturating_add(HEADER_LEN as u64);
        let in_batch = self.batch.is_some();
        self.pack_for(conn, need)?;
        let faults = &mut self.faults;
        let pack = self.current.as_mut().ok_or_else(|| StoreError::Corrupt("no pack to write to".into()))?;
        let off = pack.len;
        let written = (|| -> std::io::Result<()> {
            pack.file.seek(SeekFrom::Start(off))?;
            pack.file.write_all(&header.encode())?;
            pack.file.write_all(payload)?;
            if !in_batch {
                faults.sync(&pack.file)?;
            }
            Ok(())
        })();
        if let Err(e) = written {
            // Only this record is cut off: the batch's earlier records stay where they are.
            if let Err(e) = pack.file.set_len(off) {
                log::warn!("pack {:06}: could not cut off a failed record: {e}", pack.id);
            }
            pack.len = off;
            return Err(e.into());
        }
        pack.len = off.saturating_add(need);
        let pack_id = pack.id;
        let loc = Location { pack: pack_id, off, raw_len, stored_len, codec, sha256: sha };
        if let Some(batch) = self.batch.as_mut() {
            let i = batch.pending.len();
            batch.by_key.insert((kind, key.clone()), i);
            batch.by_sha.insert((kind, sha), i);
            batch.pending.push(Pending { kind, key: key.clone(), loc, stale });
            return Ok(key);
        }
        let end = off.saturating_add(need);
        let committed = crate::sql::in_savepoint(conn, "packs_put", |c| {
            match &stale {
                Some(s) => rewrite_row(c, kind, s, &loc)?,
                None => insert_row(c, kind, &key, &loc)?,
            }
            c.prepare_cached("UPDATE packs SET committed_len = ?1 WHERE id = ?2")?
                .execute(params![sql_len(end), pack_id])?;
            Ok(())
        });
        match committed {
            Ok(()) => {
                if let Some(p) = self.current.as_mut() {
                    p.committed = p.len;
                }
                Ok(key)
            }
            Err(e) => {
                self.cut_back();
                Err(e)
            }
        }
    }

    /// The key for `data`: its plain [`BlobKey`], or the first probe no other entry of `kind` owns.
    fn free_key(&self, conn: &Connection, kind: Kind, data: &[u8]) -> Result<String> {
        let base = BlobKey::for_data(data);
        let mut stmt = conn.prepare_cached("SELECT 1 FROM bodies WHERE kind = ?1 AND key = ?2")?;
        for probe in 0..=blob::MAX_PROBES {
            let key = base.with_probe(probe).to_string();
            let taken = self.pending(kind, &key).is_some()
                || stmt.query_row(params![kind.sql(), &key], |_| Ok(())).optional()?.is_some();
            if !taken {
                return Ok(key);
            }
            log::warn!("content key collision on {key}; probing");
        }
        Err(StoreError::Corrupt(format!("too many content key collisions for {base}")))
    }

    /// Where entry `key` of `kind` is: pending in the open batch, or committed.
    fn locate(&self, conn: &Connection, kind: Kind, key: &str) -> Result<Option<Location>> {
        if let Some(p) = self.pending(kind, key) {
            return Ok(Some(p.loc.clone()));
        }
        locate_row(conn, kind, key)
    }

    /// Whether an entry `key` of `kind` is stored (pending or committed; not verified).
    pub(crate) fn has(&self, conn: &Connection, kind: Kind, key: &str) -> Result<bool> {
        Ok(self.locate(conn, kind, key)?.is_some())
    }
}

fn batch_failed(why: &str) -> StoreError {
    StoreError::Io(std::io::Error::other(format!(
        "a write of this batch failed and none of it was stored: {why}"
    )))
}

fn not_found(kind: Kind, key: &str) -> StoreError {
    StoreError::Io(std::io::Error::new(
        ErrorKind::NotFound,
        format!("{} {}", kind.what(), key.chars().take(64).collect::<String>()),
    ))
}

fn corrupt(kind: Kind, key: &str, why: &str) -> StoreError {
    StoreError::Corrupt(format!("{} {}: {why}", kind.what(), key.chars().take(64).collect::<String>()))
}

fn insert_row(conn: &Connection, kind: Kind, key: &str, loc: &Location) -> Result<()> {
    let record = sql_len(loc.stored_len.saturating_add(HEADER_LEN as u64));
    conn.prepare_cached(
        "INSERT INTO bodies(kind, key, sha256, raw_len, pack, off, stored_len, codec, refs) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
    )?
    .execute(params![
        kind.sql(),
        key,
        &loc.sha256[..],
        sql_len(loc.raw_len),
        loc.pack,
        sql_len(loc.off),
        sql_len(loc.stored_len),
        i64::from(loc.codec)
    ])?;
    conn.prepare_cached("UPDATE packs SET live = live + ?1 WHERE id = ?2")?
        .execute(params![record, loc.pack])?;
    Ok(())
}

fn locate_row(conn: &Connection, kind: Kind, key: &str) -> Result<Option<Location>> {
    let row = conn
        .prepare_cached(
            "SELECT pack, off, raw_len, stored_len, codec, sha256 FROM bodies WHERE kind = ?1 AND key = ?2",
        )?
        .query_row(params![kind.sql(), key], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Vec<u8>>(5)?,
            ))
        })
        .optional()?;
    let Some((pack, off, raw_len, stored_len, codec, sha)) = row else { return Ok(None) };
    location_of(kind, key, [pack, off, raw_len, stored_len, codec], &sha).map(Some)
}

/// A [`Location`] from the `[pack, off, raw_len, stored_len, codec]` and `sha256` columns of a `bodies` row
/// (an error for fields that are not valid).
fn location_of(kind: Kind, key: &str, cols: [i64; 5], sha: &[u8]) -> Result<Location> {
    let [pack, off, raw_len, stored_len, codec] = cols;
    let sha256: [u8; 32] = sha.try_into().map_err(|_| corrupt(kind, key, "bad hash in the index"))?;
    Ok(Location {
        pack,
        off: u64::try_from(off).map_err(|_| corrupt(kind, key, "bad offset in the index"))?,
        raw_len: u64::try_from(raw_len).map_err(|_| corrupt(kind, key, "bad length in the index"))?,
        stored_len: u64::try_from(stored_len).map_err(|_| corrupt(kind, key, "bad length in the index"))?,
        codec: u8::try_from(codec).map_err(|_| corrupt(kind, key, "bad codec in the index"))?,
        sha256,
    })
}

/// Points the broken row `stale` at its new record `loc` (same row, key, hash and references), moving its
/// bytes from live to dead in the pack it pointed at.
fn rewrite_row(conn: &Connection, kind: Kind, stale: &Stale, loc: &Location) -> Result<()> {
    let changed = conn
        .prepare_cached(
            "UPDATE bodies SET raw_len = ?1, pack = ?2, off = ?3, stored_len = ?4, codec = ?5, sha256 = ?6 \
         WHERE id = ?7",
        )?
        .execute(params![
            sql_len(loc.raw_len),
            loc.pack,
            sql_len(loc.off),
            sql_len(loc.stored_len),
            i64::from(loc.codec),
            &loc.sha256[..],
            stale.row
        ])?;
    if changed == 0 {
        // Collected meanwhile: the entry gets a row of its own.
        return insert_row(conn, kind, &stale.key, loc);
    }
    conn.prepare_cached("UPDATE packs SET dead = dead + ?1, live = max(live - ?1, 0) WHERE id = ?2")?
        .execute(params![stale.record.max(0), stale.pack])?;
    conn.prepare_cached("UPDATE packs SET live = live + ?1 WHERE id = ?2")?
        .execute(params![sql_len(loc.stored_len.saturating_add(HEADER_LEN as u64)), loc.pack])?;
    Ok(())
}

/// Opens the pack of `loc` and reads the record header, checked against the index. Leaves the file at the
/// start of the payload and returns the payload's CRC.
fn open_record(dir: &Path, kind: Kind, key: &str, loc: &Location) -> Result<(File, u32)> {
    let cap = u64::try_from(kind.cap()).unwrap_or(u64::MAX);
    if loc.raw_len > cap || loc.stored_len > loc.raw_len {
        return Err(corrupt(kind, key, "recorded length past the cap"));
    }
    let mut f = File::open(pack_path(dir, loc.pack))?;
    f.seek(SeekFrom::Start(loc.off))?;
    let mut head = [0u8; HEADER_LEN];
    f.read_exact(&mut head).map_err(|e| match e.kind() {
        ErrorKind::UnexpectedEof => corrupt(kind, key, "the pack ends before the record"),
        _ => e.into(),
    })?;
    let h = Header::decode(&head).ok_or_else(|| corrupt(kind, key, "bad record header"))?;
    let matches = h.codec == loc.codec
        && h.kind == kind.code()
        && h.raw_len == loc.raw_len
        && h.stored_len == loc.stored_len
        && h.sha256 == loc.sha256;
    if !matches {
        return Err(corrupt(kind, key, "the record does not match the index"));
    }
    Ok((f, h.crc32))
}

/// Reads and verifies a whole entry.
fn read_entry(dir: &Path, kind: Kind, key: &str, loc: &Location) -> Result<Vec<u8>> {
    let (f, crc) = open_record(dir, kind, key, loc)?;
    let stored = usize::try_from(loc.stored_len).map_err(|_| corrupt(kind, key, "record too large"))?;
    let raw = usize::try_from(loc.raw_len).map_err(|_| corrupt(kind, key, "record too large"))?;
    let mut payload = Vec::with_capacity(stored);
    f.take(loc.stored_len).read_to_end(&mut payload)?;
    if payload.len() != stored {
        return Err(corrupt(kind, key, "the pack ends inside the record"));
    }
    if crc32fast::hash(&payload) != crc {
        return Err(corrupt(kind, key, "checksum mismatch"));
    }
    let data = match loc.codec {
        CODEC_STORE => payload,
        CODEC_ZSTD => {
            zstd::bulk::decompress(&payload, raw).map_err(|_| corrupt(kind, key, "cannot decompress"))?
        }
        _ => return Err(corrupt(kind, key, "unknown codec")),
    };
    if data.len() != raw || sha256(&data) != loc.sha256 {
        return Err(corrupt(kind, key, "content does not match its hash"));
    }
    Ok(data)
}

/// The first `cap` bytes of an entry (header checked, content not verified).
fn read_head(dir: &Path, kind: Kind, key: &str, loc: &Location, cap: usize) -> Result<Vec<u8>> {
    let (f, _) = open_record(dir, kind, key, loc)?;
    let want = loc.raw_len.min(u64::try_from(cap).unwrap_or(u64::MAX));
    let mut out = Vec::with_capacity(usize::try_from(want).unwrap_or(0));
    let payload = f.take(loc.stored_len);
    match loc.codec {
        CODEC_STORE => {
            payload.take(want).read_to_end(&mut out)?;
        }
        CODEC_ZSTD => {
            let dec = zstd::stream::read::Decoder::new(payload)
                .map_err(|_| corrupt(kind, key, "cannot decompress"))?;
            dec.take(want).read_to_end(&mut out).map_err(|_| corrupt(kind, key, "cannot decompress"))?;
        }
        _ => return Err(corrupt(kind, key, "unknown codec")),
    }
    if u64::try_from(out.len()).unwrap_or(u64::MAX) < want {
        return Err(corrupt(kind, key, "shorter than recorded"));
    }
    Ok(out)
}

// ---- the pack backend of a BodyStore ----

/// The pack files of a SQLite profile as a [`crate::BodyStore`] backend.
#[derive(Clone, Debug)]
pub struct Packs {
    db: Db,
}

impl Packs {
    pub fn new(db: Db) -> Packs {
        Packs { db }
    }

    /// The database the packs are indexed in.
    pub fn db(&self) -> &Db {
        &self.db
    }

    pub(crate) fn put(&self, kind: Kind, data: &[u8]) -> Result<String> {
        let mut w = self.db.packs();
        let conn = self.db.conn();
        w.put(&conn, kind, data)
    }

    pub(crate) fn get(&self, kind: Kind, key: &str) -> Result<Vec<u8>> {
        let (dir, loc) = self.locate(kind, key)?;
        let loc = loc.ok_or_else(|| not_found(kind, key))?;
        read_entry(&dir, kind, key, &loc)
    }

    pub(crate) fn head(&self, kind: Kind, key: &str, cap: usize) -> Result<Option<Vec<u8>>> {
        let (dir, loc) = self.locate(kind, key)?;
        match loc {
            Some(loc) => read_head(&dir, kind, key, &loc, cap).map(Some),
            None => Ok(None),
        }
    }

    /// The profile folder and where entry `key` of `kind` is (the locks are released before any file read).
    fn locate(&self, kind: Kind, key: &str) -> Result<(PathBuf, Option<Location>)> {
        let w = self.db.packs();
        let conn = self.db.conn();
        Ok((w.dir.clone(), w.locate(&conn, kind, key)?))
    }

    pub(crate) fn has(&self, kind: Kind, key: &str) -> bool {
        let w = self.db.packs();
        let conn = self.db.conn();
        w.has(&conn, kind, key).unwrap_or(false)
    }

    /// What opening the profile repaired in its pack files.
    pub fn recovery(&self) -> Recovery {
        self.db.packs().recovery.clone()
    }

    /// Lowers the length at which packs roll over ([`PACK_ROLL_BYTES`]), so tests reach it.
    #[doc(hidden)]
    pub fn set_roll_bytes(&self, bytes: u64) {
        self.db.packs().roll_bytes = bytes;
    }

    /// See [`PackWriter::begin_batch`].
    pub fn begin_batch(&self) {
        self.db.packs().begin_batch();
    }

    /// See [`PackWriter::finish_batch`].
    pub fn finish_batch(&self) -> Result<usize> {
        let mut w = self.db.packs();
        let conn = self.db.conn();
        w.finish_batch(&conn)
    }

    /// Deletes the committed entries whose key is not in `live` and no mail row refers to; counts their bytes
    /// as dead in their packs. With `only`, just those keys are considered (each looked up through the
    /// `(kind, key)` index, as a source and as a body record). Returns how many went.
    pub(crate) fn collect(
        &self,
        live: &std::collections::BTreeSet<String>,
        only: Option<&std::collections::BTreeSet<String>>,
    ) -> Result<usize> {
        let w = self.db.packs();
        let conn = self.db.conn();
        let mut gone: Vec<Gone> = Vec::new();
        {
            let mut consider = |r: &rusqlite::Row<'_>| -> rusqlite::Result<()> {
                let key: String = r.get(1)?;
                let refs: i64 = r.get(6)?;
                if refs <= 0 && !live.contains(&key) {
                    gone.push(Gone {
                        row: r.get(0)?,
                        kind: r.get(2)?,
                        pack: r.get(3)?,
                        off: r.get(4)?,
                        stored: r.get(5)?,
                        sha256: r.get(7)?,
                    });
                }
                Ok(())
            };
            match only {
                None => {
                    let mut stmt = conn.prepare(&format!("SELECT {GONE_COLUMNS} FROM bodies"))?;
                    let mut rows = stmt.query([])?;
                    while let Some(r) = rows.next()? {
                        consider(r)?;
                    }
                }
                Some(keys) => {
                    let mut stmt = conn.prepare_cached(COLLECT_KEY_SQL)?;
                    for k in keys.iter().filter(|k| !live.contains(*k)) {
                        for kind in [Kind::Source, Kind::Body] {
                            let mut rows = stmt.query(params![kind.sql(), k])?;
                            while let Some(r) = rows.next()? {
                                consider(r)?;
                            }
                        }
                    }
                }
            }
        }
        if gone.is_empty() {
            return Ok(0);
        }
        crate::sql::in_savepoint(&conn, "packs_collect", |c| {
            let mut del = c.prepare_cached("DELETE FROM bodies WHERE id = ?1")?;
            let mut dead = c.prepare_cached(
                "UPDATE packs SET dead = dead + ?1, live = max(live - ?1, 0) WHERE id = ?2",
            )?;
            for g in &gone {
                del.execute([g.row])?;
                dead.execute(params![g.stored.saturating_add(HEADER_LEN as i64), g.pack])?;
            }
            Ok(())
        })?;
        drop(conn);
        // Nothing refers to these records any more: overwrite them, so mail deleted for good (or that of a
        // closed protected data file) does not stay readable in the pack until compaction.
        let mut by_pack: BTreeMap<i64, Vec<&Gone>> = BTreeMap::new();
        for g in &gone {
            by_pack.entry(g.pack).or_default().push(g);
        }
        for (pack, records) in by_pack {
            if let Err(e) = zero_records(&pack_path(&w.dir, pack), &records) {
                log::warn!("pack {pack:06}: could not overwrite removed records: {e}");
            }
        }
        Ok(gone.len())
    }

    /// Committed entries, and the live and dead bytes of all packs.
    pub fn stats(&self) -> Result<PackStats> {
        let conn = self.db.conn();
        let (entries, raw): (i64, i64) =
            conn.query_row("SELECT count(*), coalesce(sum(raw_len), 0) FROM bodies", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        let (packs, live, dead): (i64, i64, i64) = conn.query_row(
            "SELECT count(*), coalesce(sum(live), 0), coalesce(sum(dead), 0) FROM packs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(PackStats {
            entries: count(entries),
            raw_bytes: count(raw),
            packs: count(packs),
            live_bytes: count(live),
            dead_bytes: count(dead),
        })
    }
}

/// The columns of a `bodies` row [`Packs::collect`] reads: [`Gone`]'s, with the key and refs.
const GONE_COLUMNS: &str = "id, key, kind, pack, off, stored_len, refs, sha256";

/// The rows of one kind and key, looked up through the `UNIQUE(kind, key)` index.
pub(crate) const COLLECT_KEY_SQL: &str =
    "SELECT id, key, kind, pack, off, stored_len, refs, sha256 FROM bodies WHERE kind = ?1 AND key = ?2";

/// A `bodies` row [`Packs::collect`] removes.
#[derive(Debug)]
struct Gone {
    row: i64,
    kind: i64,
    pack: i64,
    off: i64,
    stored: i64,
    sha256: Vec<u8>,
}

/// Overwrites the records of `gone` in the pack at `path` with zeros and syncs it. A record is overwritten
/// only when the header at its offset is the one its row describes (kind, stored length and hash): a damaged
/// row never makes collection zero another entry. Records past the end of the file are cut to it (the file
/// is never extended).
fn zero_records(path: &Path, gone: &[&Gone]) -> std::io::Result<()> {
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    let len = f.metadata()?.len();
    let zeros = [0u8; 64 * 1024];
    for g in gone {
        let (Ok(off), Ok(stored)) = (u64::try_from(g.off), u64::try_from(g.stored)) else {
            log::warn!("{}: bad offset or length in the index; record not overwritten", path.display());
            continue;
        };
        if off >= len {
            continue;
        }
        let mut head = [0u8; HEADER_LEN];
        f.seek(SeekFrom::Start(off))?;
        let ours = match f.read_exact(&mut head) {
            Ok(()) => Header::decode(&head).is_some_and(|h| {
                i64::from(h.kind) == g.kind && h.stored_len == stored && h.sha256[..] == g.sha256[..]
            }),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => false,
            Err(e) => return Err(e),
        };
        if !ours {
            log::warn!(
                "{}: the record at {off} is not the one its index row describes; not overwritten",
                path.display()
            );
            continue;
        }
        let mut left = stored.saturating_add(HEADER_LEN as u64).min(len.saturating_sub(off));
        f.seek(SeekFrom::Start(off))?;
        while left > 0 {
            let chunk = usize::try_from(left.min(zeros.len() as u64)).unwrap_or(zeros.len());
            f.write_all(zeros.get(..chunk).unwrap_or(&zeros))?;
            left = left.saturating_sub(chunk as u64);
        }
    }
    f.sync_data()
}

/// `(pack id, committed length)` of every pack of `db`'s profile.
pub fn committed_lengths(db: &Db) -> Result<Vec<(i64, u64)>> {
    let conn = db.conn();
    let mut stmt = conn.prepare("SELECT id, committed_len FROM packs ORDER BY id")?;
    let mut out = Vec::new();
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        out.push((r.get::<_, i64>(0)?, u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0)));
    }
    Ok(out)
}

/// Counters of a profile's packs ([`Packs::stats`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PackStats {
    pub entries: usize,
    /// Content bytes before compression.
    pub raw_bytes: usize,
    pub packs: usize,
    pub live_bytes: usize,
    pub dead_bytes: usize,
}

#[cfg(test)]
mod tests;
