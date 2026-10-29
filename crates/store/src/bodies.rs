//! Message bodies and sources out of memory (storage stage S5, `plan/storage-adr.md` §6).
//!
//! A [`mailcraft_model::MailItem`] holds no body and no source. Its content lives in a [`BodyStore`]:
//! - the RFC 5322 **source**, under the item's `blob` key; the decoded body is parsed from it on demand;
//! - a **body record** (a serialised [`Body`]), under the item's `body_key`, for an item without a source (a
//!   local draft, a message made here) or one whose body differs from its source (changed here). A body
//!   record wins over the source.
//!
//! Backends: **on disk** (a profile: the same content-addressed `blobs/<aa>/<key>.eml` files the store has
//! always written, plus `blobs/<aa>/<key>.body` for body records) and **in memory** (sessions without a
//! profile: the sample mailbox, tests and the browser build). Keys use the [`BlobKey`] grammar over the bytes
//! stored (the source, or the body record's JSON), so equal content shares one entry and a changed body gets
//! a new key: nothing is ever rewritten in place, and undo can put an old key back.
//!
//! Decoded bodies are kept in a least-recently-used cache capped in bytes ([`DEFAULT_CACHE_BYTES`]); the
//! reading pane, compose, rules, search and print read through it. Nothing else of the content stays in
//! memory, so a session's memory no longer grows with the size of its mail.
//!
//! Every read is capped and verified against its key; a missing or damaged entry is an error, never a panic.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use mailcraft_model::{Body, MailItem};
use serde::{Deserialize, Serialize};

use crate::blob::{self, BlobKey};
use crate::journal;
use crate::{MAX_BLOB_BYTES, Result, StoreError};

/// File extension of message sources under `blobs/`.
pub const SOURCE_EXT: &str = "eml";
/// File extension of body records under `blobs/`.
pub const BODY_EXT: &str = "body";
/// Default byte cap of the decoded-body cache.
pub const DEFAULT_CACHE_BYTES: usize = 64 << 20;
/// Largest body record written or read (a body decoded from a source of at most [`MAX_BLOB_BYTES`], as JSON).
pub const MAX_BODY_RECORD_BYTES: usize = 4 * MAX_BLOB_BYTES;

/// What a message's content is made of on its way into a [`BodyStore`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Content {
    /// The RFC 5322 source.
    pub source: Option<Vec<u8>>,
    /// The decoded body, when it is not (or may not be) the one the source gives.
    pub body: Option<Body>,
}

impl Content {
    /// A message known by its source only.
    pub fn source(raw: Vec<u8>) -> Content {
        Content { source: Some(raw), body: None }
    }
    /// A message known by its body only (no source).
    pub fn body(body: Body) -> Content {
        Content { source: None, body: Some(body) }
    }
}

/// Byte and entry counts of the decoded-body cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub cap: usize,
    pub hits: u64,
    pub misses: u64,
}

#[derive(Debug, Default)]
struct Memory {
    sources: BTreeMap<String, Arc<[u8]>>,
    bodies: BTreeMap<String, Arc<Body>>,
}

#[derive(Debug)]
enum Backend {
    Memory(Memory),
    Disk(PathBuf),
}

/// Least-recently-used cache of decoded bodies, capped in bytes.
///
/// A body that costs more than the whole cap (hostile mail can decode to more than its source) is not
/// cached with the others: it would push them all out. The last such body is kept on its own instead
/// (`big`), so the message being shown, which is read every frame, is decoded once and not again per frame.
#[derive(Debug, Default)]
struct Cache {
    cap: usize,
    used: usize,
    tick: u64,
    map: HashMap<String, (Arc<Body>, usize, u64)>,
    order: BTreeMap<u64, String>,
    big: Option<(String, Arc<Body>)>,
    hits: u64,
    misses: u64,
}

impl Cache {
    fn new(cap: usize) -> Cache {
        Cache { cap, ..Default::default() }
    }

    fn get(&mut self, key: &str) -> Option<Arc<Body>> {
        let Some(entry) = self.map.get_mut(key) else {
            if let Some((k, b)) = &self.big
                && k == key
            {
                self.hits = self.hits.saturating_add(1);
                return Some(Arc::clone(b));
            }
            self.misses = self.misses.saturating_add(1);
            return None;
        };
        self.hits = self.hits.saturating_add(1);
        self.tick = self.tick.wrapping_add(1);
        self.order.remove(&entry.2);
        entry.2 = self.tick;
        self.order.insert(self.tick, key.to_owned());
        Some(Arc::clone(&entry.0))
    }

    fn put(&mut self, key: &str, body: &Arc<Body>) {
        let cost = body.approx_bytes().saturating_add(key.len()).saturating_add(64);
        self.remove(key);
        if cost > self.cap {
            self.big = Some((key.to_owned(), Arc::clone(body)));
            return;
        }
        while self.used.saturating_add(cost) > self.cap {
            let Some((_, oldest)) = self.order.pop_first() else { break };
            if let Some((_, c, _)) = self.map.remove(&oldest) {
                self.used = self.used.saturating_sub(c);
            }
        }
        self.tick = self.tick.wrapping_add(1);
        self.order.insert(self.tick, key.to_owned());
        self.map.insert(key.to_owned(), (Arc::clone(body), cost, self.tick));
        self.used = self.used.saturating_add(cost);
    }

    fn remove(&mut self, key: &str) {
        if self.big.as_ref().is_some_and(|(k, _)| k == key) {
            self.big = None;
        }
        if let Some((_, c, t)) = self.map.remove(key) {
            self.order.remove(&t);
            self.used = self.used.saturating_sub(c);
        }
    }

    fn set_cap(&mut self, cap: usize) {
        self.cap = cap;
        while self.used > self.cap {
            let Some((_, oldest)) = self.order.pop_first() else { break };
            if let Some((_, c, _)) = self.map.remove(&oldest) {
                self.used = self.used.saturating_sub(c);
            }
        }
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.map.len(),
            bytes: self.used,
            cap: self.cap,
            hits: self.hits,
            misses: self.misses,
        }
    }
}

/// Where message sources and body records live, with the decoded-body cache in front of them.
#[derive(Debug)]
pub struct BodyStore {
    backend: Backend,
    cache: Mutex<Cache>,
}

impl Default for BodyStore {
    fn default() -> Self {
        BodyStore::in_memory()
    }
}

/// Cache key of a body record.
fn body_cache_key(key: &str) -> String {
    format!("b:{key}")
}

/// Cache key of a body parsed from a source.
fn source_cache_key(key: &str) -> String {
    format!("s:{key}")
}

fn not_found(what: &str, key: &str) -> StoreError {
    StoreError::Io(std::io::Error::new(ErrorKind::NotFound, format!("{what} {}", short(key))))
}

fn short(s: &str) -> String {
    s.chars().take(64).collect()
}

fn parse_key(key: &str) -> Result<BlobKey> {
    BlobKey::parse(key).ok_or_else(|| StoreError::Corrupt(format!("invalid content key {:?}", short(key))))
}

/// The body record bytes of `body` (its JSON).
fn record_bytes(body: &Body) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(body).map_err(std::io::Error::other)?;
    if bytes.len() > MAX_BODY_RECORD_BYTES {
        return Err(StoreError::TooLarge(format!(
            "the message body takes {} bytes, past the {MAX_BODY_RECORD_BYTES} byte cap",
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn check_source_size(raw: &[u8]) -> Result<()> {
    if raw.len() > MAX_BLOB_BYTES {
        return Err(StoreError::Io(std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("blob of {} bytes exceeds the {MAX_BLOB_BYTES} byte cap", raw.len()),
        )));
    }
    Ok(())
}

/// Path of content `key` with extension `ext` under the profile `dir`: `blobs/<aa>/<key>.<ext>`.
pub fn content_path(dir: &Path, key: &BlobKey, ext: &str) -> PathBuf {
    let name = key.to_string();
    let fan = name.get(..2).unwrap_or("00").to_owned();
    dir.join(blob::BLOBS_DIR).join(fan).join(format!("{name}.{ext}"))
}

/// Stores `data` under its content key in `dir` (extension `ext`), probing past a different content that owns
/// the plain key, and repairing a damaged file. Returns the key.
pub(crate) fn disk_put(dir: &Path, data: &[u8], ext: &str, cap: usize) -> Result<String> {
    let base = BlobKey::for_data(data);
    let cap = u64::try_from(cap).unwrap_or(u64::MAX);
    for probe in 0..=blob::MAX_PROBES {
        let key = base.with_probe(probe);
        let path = content_path(dir, &key, ext);
        // The key carries the length: a file of any other size is damaged (and may be too big to read).
        let existing = match fs::metadata(&path) {
            Ok(m) if m.is_file() && m.len() == key.len => journal::read_capped(&path, key.len.min(cap))?,
            Ok(_) => {
                log::warn!("blob {key} has the wrong size; rewriting it");
                None
            }
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        match existing {
            Some(existing) if existing == data => return Ok(key.to_string()),
            Some(existing) if key.matches(&existing) => {
                log::warn!("blob hash collision on {key}; probing");
                continue;
            }
            Some(_) => log::warn!("blob {key} was damaged; rewriting it"),
            None => {}
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        journal::write_atomic(&path, |w| w.write_all(data))?;
        return Ok(key.to_string());
    }
    Err(StoreError::Corrupt(format!("too many blob collisions for {base}")))
}

/// Reads and verifies content `key` (extension `ext`) from `dir`.
pub(crate) fn disk_get(dir: &Path, key: &str, ext: &str, cap: usize) -> Result<Vec<u8>> {
    let parsed = parse_key(key)?;
    let cap = u64::try_from(cap).unwrap_or(u64::MAX);
    let data = journal::read_capped(&content_path(dir, &parsed, ext), parsed.len.min(cap))?
        .ok_or_else(|| not_found("blob", key))?;
    if !parsed.matches(&data) {
        return Err(StoreError::Corrupt(format!("blob {parsed} does not match its key")));
    }
    Ok(data)
}

/// The first `cap` bytes of content `key` in `dir` (not verified: that needs the whole file). `Ok(None)` when
/// the file is missing; a file shorter than its key says is corrupt.
pub(crate) fn disk_head(dir: &Path, key: &str, ext: &str, cap: usize) -> Result<Option<Vec<u8>>> {
    let parsed = parse_key(key)?;
    let f = match File::open(content_path(dir, &parsed, ext)) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let want = parsed.len.min(u64::try_from(cap).unwrap_or(u64::MAX));
    let mut buf = Vec::with_capacity(usize::try_from(want).unwrap_or(0));
    f.take(want).read_to_end(&mut buf)?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) < want {
        return Err(StoreError::Corrupt(format!("blob {parsed} is shorter than its key says")));
    }
    Ok(Some(buf))
}

/// Stores `data` in `map` under its content key, probing past different content that owns a key.
fn mem_put<T: ?Sized>(
    map: &mut BTreeMap<String, Arc<T>>,
    bytes: &[u8],
    same: impl Fn(&T) -> bool,
    make: impl FnOnce() -> Arc<T>,
) -> Result<String> {
    let base = BlobKey::for_data(bytes);
    for probe in 0..=blob::MAX_PROBES {
        let key = base.with_probe(probe).to_string();
        match map.get(&key) {
            Some(existing) if same(existing) => return Ok(key),
            Some(_) => continue,
            None => {
                map.insert(key.clone(), make());
                return Ok(key);
            }
        }
    }
    Err(StoreError::Corrupt(format!("too many blob collisions for {base}")))
}

impl BodyStore {
    /// A store keeping everything in memory (sessions without a profile).
    pub fn in_memory() -> BodyStore {
        BodyStore {
            backend: Backend::Memory(Memory::default()),
            cache: Mutex::new(Cache::new(DEFAULT_CACHE_BYTES)),
        }
    }

    /// A store in the profile folder `dir` (`blobs/` below it).
    pub fn on_disk(dir: &Path) -> BodyStore {
        BodyStore {
            backend: Backend::Disk(dir.to_path_buf()),
            cache: Mutex::new(Cache::new(DEFAULT_CACHE_BYTES)),
        }
    }

    /// The profile folder of an on-disk store.
    pub fn dir(&self) -> Option<&Path> {
        match &self.backend {
            Backend::Disk(d) => Some(d),
            Backend::Memory(_) => None,
        }
    }

    fn cache(&self) -> MutexGuard<'_, Cache> {
        // A poisoned cache only means a reader panicked mid-way; the cache is still a valid cache.
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes the decoded-body cache's byte cap (entries past it go, least recently used first).
    pub fn set_cache_cap(&self, bytes: usize) {
        self.cache().set_cap(bytes);
    }

    /// The decoded-body cache's counters.
    pub fn cache_stats(&self) -> CacheStats {
        self.cache().stats()
    }

    /// Empties the decoded-body cache.
    pub fn clear_cache(&self) {
        let mut c = self.cache();
        let cap = c.cap;
        *c = Cache::new(cap);
    }

    // ---- writes ----

    /// Stores a message source; returns its key (an item's `blob`).
    pub fn put_source(&mut self, raw: &[u8]) -> Result<String> {
        check_source_size(raw)?;
        match &mut self.backend {
            Backend::Disk(dir) => disk_put(dir, raw, SOURCE_EXT, MAX_BLOB_BYTES),
            Backend::Memory(m) => mem_put(&mut m.sources, raw, |e: &[u8]| e == raw, || Arc::from(raw)),
        }
    }

    /// Stores a body record; returns its key (an item's `body_key`).
    pub fn put_body(&mut self, body: &Body) -> Result<String> {
        let bytes = record_bytes(body)?;
        let key = match &mut self.backend {
            Backend::Disk(dir) => disk_put(dir, &bytes, BODY_EXT, MAX_BODY_RECORD_BYTES)?,
            Backend::Memory(m) => {
                mem_put(&mut m.bodies, &bytes, |e: &Body| e == body, || Arc::new(body.clone()))?
            }
        };
        self.cache().put(&body_cache_key(&key), &Arc::new(body.clone()));
        Ok(key)
    }

    /// Stores `content` for `item`: the source under `item.blob`, and a body record under `item.body_key`
    /// when there is a body that is not the one the source gives (an empty body without a source stores
    /// nothing). Keys the content does not have are cleared.
    pub fn store(&mut self, item: &mut MailItem, content: &Content) -> Result<()> {
        let blob = match &content.source {
            Some(raw) if !raw.is_empty() => Some(self.put_source(raw)?),
            _ => None,
        };
        let body_key = match (&content.body, &content.source) {
            (None, _) => None,
            // With a source: a record only for a body the source does not give.
            (Some(body), Some(raw)) if blob.is_some() => {
                if Body::from_source(raw).as_ref() == Some(body) {
                    None
                } else {
                    Some(self.put_body(body)?)
                }
            }
            // Without one: a record unless there is nothing to keep.
            (Some(body), _) => (!body.is_empty()).then(|| self.put_body(body)).transpose()?,
        };
        item.blob = blob;
        item.body_key = body_key;
        Ok(())
    }

    /// Stores `body` as `item`'s body (a body record; the source key stays) and refreshes its preview line.
    pub fn store_body(&mut self, item: &mut MailItem, body: &Body) -> Result<()> {
        item.body_key = Some(self.put_body(body)?);
        item.preview = body.preview();
        Ok(())
    }

    // ---- reads ----

    /// The source stored under `key`, verified against it.
    pub fn source(&self, key: &str) -> Result<Vec<u8>> {
        match &self.backend {
            Backend::Disk(dir) => disk_get(dir, key, SOURCE_EXT, MAX_BLOB_BYTES),
            Backend::Memory(m) => {
                m.sources.get(key).map(|s| s.to_vec()).ok_or_else(|| not_found("source", key))
            }
        }
    }

    /// The first `cap` bytes of the source stored under `key` (`Ok(None)` when it is missing).
    pub fn source_head(&self, key: &str, cap: usize) -> Result<Option<Vec<u8>>> {
        match &self.backend {
            Backend::Disk(dir) => disk_head(dir, key, SOURCE_EXT, cap),
            Backend::Memory(m) => {
                Ok(m.sources.get(key).map(|s| s.get(..s.len().min(cap)).unwrap_or_default().to_vec()))
            }
        }
    }

    /// The body record stored under `key`.
    pub fn body_record(&self, key: &str) -> Result<Arc<Body>> {
        let ck = body_cache_key(key);
        if let Some(b) = self.cache().get(&ck) {
            return Ok(b);
        }
        let body = match &self.backend {
            Backend::Memory(m) => m.bodies.get(key).cloned().ok_or_else(|| not_found("message body", key))?,
            Backend::Disk(dir) => {
                let bytes = disk_get(dir, key, BODY_EXT, MAX_BODY_RECORD_BYTES)?;
                let body: Body = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Corrupt(format!("message body {}: {e}", short(key))))?;
                Arc::new(body)
            }
        };
        self.cache().put(&ck, &body);
        Ok(body)
    }

    /// Whether a source is stored under `key` (on disk: the file exists; not verified).
    pub fn has_source(&self, key: &str) -> bool {
        match &self.backend {
            Backend::Disk(dir) => {
                BlobKey::parse(key).is_some_and(|k| content_path(dir, &k, SOURCE_EXT).is_file())
            }
            Backend::Memory(m) => m.sources.contains_key(key),
        }
    }

    /// Whether a body record is stored under `key` (on disk: the file exists; not verified).
    pub fn has_body(&self, key: &str) -> bool {
        match &self.backend {
            Backend::Disk(dir) => {
                BlobKey::parse(key).is_some_and(|k| content_path(dir, &k, BODY_EXT).is_file())
            }
            Backend::Memory(m) => m.bodies.contains_key(key),
        }
    }

    /// Whether `item` has content in this store it can be read from (a body record or a source key).
    pub fn has_content(item: &MailItem) -> bool {
        item.body_key.is_some() || item.blob.is_some()
    }

    /// The decoded body of `item`: its body record, else the body its source gives (cached). `Ok(None)` when
    /// the item has neither (its body is not on this computer).
    pub fn body_of(&self, item: &MailItem) -> Result<Option<Arc<Body>>> {
        if let Some(key) = &item.body_key {
            return self.body_record(key).map(Some);
        }
        let Some(key) = &item.blob else { return Ok(None) };
        let ck = source_cache_key(key);
        if let Some(b) = self.cache().get(&ck) {
            return Ok(Some(b));
        }
        let raw = self.source(key)?;
        // A source the parser rejects has no body to show (the item was built from a header scan).
        let body = Arc::new(Body::from_source(&raw).unwrap_or_default());
        self.cache().put(&ck, &body);
        Ok(Some(body))
    }

    /// The source of `item` (`Ok(None)` when it has none).
    pub fn source_of(&self, item: &MailItem) -> Result<Option<Vec<u8>>> {
        match &item.blob {
            Some(key) => self.source(key).map(Some),
            None => Ok(None),
        }
    }

    /// The first `cap` bytes of `item`'s source (`Ok(None)` when it has none, or it is missing).
    pub fn head_of(&self, item: &MailItem, cap: usize) -> Result<Option<Vec<u8>>> {
        match &item.blob {
            Some(key) => self.source_head(key, cap),
            None => Ok(None),
        }
    }

    // ---- housekeeping ----

    /// Deletes stored content whose key is not in `live` (every key the mailbox, the recoverable items and
    /// the undo history still refer to), and leftover temp files. Returns how many entries went.
    pub fn collect(&mut self, live: &BTreeSet<String>) -> Result<usize> {
        let removed = match &mut self.backend {
            Backend::Memory(m) => {
                let before = m.sources.len().saturating_add(m.bodies.len());
                m.sources.retain(|k, _| live.contains(k));
                m.bodies.retain(|k, _| live.contains(k));
                before.saturating_sub(m.sources.len().saturating_add(m.bodies.len()))
            }
            Backend::Disk(dir) => disk_collect(dir, live)?,
        };
        if removed > 0 {
            self.clear_cache();
        }
        Ok(removed)
    }

    /// Deletes the content under `keys` that is not in `live` (content another item shares stays). Returns
    /// how many entries went.
    pub fn remove_unreferenced(&mut self, keys: &BTreeSet<String>, live: &BTreeSet<String>) -> Result<usize> {
        let mut removed = 0usize;
        for key in keys.iter().filter(|k| !live.contains(*k)) {
            match &mut self.backend {
                Backend::Memory(m) => {
                    removed = removed
                        .saturating_add(usize::from(m.sources.remove(key).is_some()))
                        .saturating_add(usize::from(m.bodies.remove(key).is_some()));
                }
                Backend::Disk(dir) => {
                    let Some(parsed) = BlobKey::parse(key) else { continue };
                    for ext in [SOURCE_EXT, BODY_EXT] {
                        match fs::remove_file(content_path(dir, &parsed, ext)) {
                            Ok(()) => removed = removed.saturating_add(1),
                            Err(e) if e.kind() == ErrorKind::NotFound => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
            }
            let mut c = self.cache();
            c.remove(&source_cache_key(key));
            c.remove(&body_cache_key(key));
        }
        Ok(removed)
    }

    /// Entries held (in memory: sources and body records; on disk: not counted, 0).
    pub fn memory_entries(&self) -> usize {
        match &self.backend {
            Backend::Memory(m) => m.sources.len().saturating_add(m.bodies.len()),
            Backend::Disk(_) => 0,
        }
    }

    /// The content under `keys` this store holds, to carry it somewhere else (the browser build's saved
    /// state): sources and body records. Missing keys are skipped.
    pub fn export(&self, keys: &BTreeSet<String>) -> Exported {
        let mut out = Exported::default();
        for key in keys {
            if let Ok(raw) = self.source(key) {
                out.sources.insert(key.clone(), raw);
            }
            if self.has_body(key)
                && let Ok(body) = self.body_record(key)
            {
                out.bodies.insert(key.clone(), (*body).clone());
            }
        }
        out
    }

    /// Takes in content [`BodyStore::export`] produced. Entries whose bytes do not match their key are
    /// dropped (hostile or damaged input); returns how many were taken.
    pub fn import(&mut self, content: Exported) -> Result<usize> {
        let mut n = 0usize;
        for (key, raw) in content.sources {
            if BlobKey::parse(&key).is_some_and(|k| k.matches(&raw)) && raw.len() <= MAX_BLOB_BYTES {
                match &mut self.backend {
                    Backend::Memory(m) => {
                        m.sources.insert(key, Arc::from(raw));
                    }
                    Backend::Disk(_) => {
                        self.put_source(&raw)?;
                    }
                }
                n = n.saturating_add(1);
            }
        }
        for (key, body) in content.bodies {
            let ok = record_bytes(&body).is_ok_and(|b| BlobKey::parse(&key).is_some_and(|k| k.matches(&b)));
            if ok {
                match &mut self.backend {
                    Backend::Memory(m) => {
                        m.bodies.insert(key, Arc::new(body));
                    }
                    Backend::Disk(_) => {
                        self.put_body(&body)?;
                    }
                }
                n = n.saturating_add(1);
            }
        }
        Ok(n)
    }
}

/// Content carried out of a [`BodyStore`] ([`BodyStore::export`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Exported {
    pub sources: BTreeMap<String, Vec<u8>>,
    pub bodies: BTreeMap<String, Body>,
}

/// Every content key `items` refer to (sources and body records).
pub fn keys_of<'a>(items: impl IntoIterator<Item = &'a MailItem>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for m in items {
        if let Some(k) = &m.blob {
            out.insert(k.clone());
        }
        if let Some(k) = &m.body_key {
            out.insert(k.clone());
        }
    }
    out
}

/// Deletes the files under `dir/blobs` whose key is not in `live`, and leftover temp files.
fn disk_collect(dir: &Path, live: &BTreeSet<String>) -> Result<usize> {
    let mut removed = 0usize;
    let root = dir.join(blob::BLOBS_DIR);
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
            let key = name
                .strip_suffix(&format!(".{SOURCE_EXT}"))
                .or_else(|| name.strip_suffix(&format!(".{BODY_EXT}")));
            let Some(key) = key else { continue };
            if BlobKey::parse(key).is_none() {
                continue;
            }
            if !live.contains(key) {
                fs::remove_file(entry.path())?;
                removed = removed.saturating_add(1);
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests;
