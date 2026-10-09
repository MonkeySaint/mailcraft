//! Snapshot and journal files.
//!
//! Snapshot (`mailbox.json`): `{"mailcraft_snapshot":1,"generation":N,"mailbox":{...}}`, the prefix written
//! byte-for-byte so the generation can be peeked without parsing the whole file. A bare [`Mailbox`] JSON
//! object is also accepted (generation 0).
//!
//! Journal (`journal.jsonl`): first line `{"mailcraft_journal":1,"generation":N}`, then one [`Op`] per line.
//! A journal whose generation is older than the snapshot's was already folded into that snapshot (a crash
//! between the snapshot rename and the journal reset) and is ignored.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, ErrorKind, Read, Write};
use std::path::Path;

use mailcraft_model::Mailbox;
use serde::{Deserialize, Serialize};

use crate::{Op, Result, StoreError};

pub const SNAPSHOT_FILE: &str = "mailbox.json";
pub const JOURNAL_FILE: &str = "journal.jsonl";
/// Largest snapshot or journal this crate will read into memory.
pub const MAX_STATE_BYTES: u64 = 1 << 30;

const SNAPSHOT_PREFIX: &str = "{\"mailcraft_snapshot\":1,\"generation\":";

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotFile {
    mailcraft_snapshot: u32,
    generation: u64,
    mailbox: Mailbox,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct JournalHeader {
    pub mailcraft_journal: u32,
    pub generation: u64,
}

impl JournalHeader {
    pub fn new(generation: u64) -> Self {
        JournalHeader { mailcraft_journal: 1, generation }
    }
    pub fn parse(line: &[u8]) -> Option<JournalHeader> {
        serde_json::from_slice(line).ok()
    }
    fn line(&self) -> Vec<u8> {
        let mut v = format!("{{\"mailcraft_journal\":1,\"generation\":{}}}", self.generation).into_bytes();
        v.push(b'\n');
        v
    }
}

fn corrupt(what: &str, e: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(format!("{what}: {e}"))
}

/// Reads a whole file with a size cap; `Ok(None)` when it does not exist.
pub fn read_capped(path: &Path, cap: u64) -> Result<Option<Vec<u8>>> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let len = f.metadata()?.len();
    if len > cap {
        return Err(StoreError::Corrupt(format!("{} is too large ({len} bytes)", path.display())));
    }
    let mut buf = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    f.take(cap.saturating_add(1)).read_to_end(&mut buf)?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > cap {
        return Err(StoreError::Corrupt(format!("{} grew past the size cap", path.display())));
    }
    Ok(Some(buf))
}

/// Fsyncs a directory so a rename inside it is durable (best effort where directories cannot be opened).
pub fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Writes `path` atomically: temp file in the same directory, fsync, rename, fsync the directory.
pub fn write_atomic(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<&File>) -> std::io::Result<()>,
) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let res = (|| -> Result<()> {
        let file = File::create(&tmp)?;
        {
            let mut w = BufWriter::new(&file);
            write(&mut w)?;
            w.flush()?;
        }
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res?;
    sync_dir(dir);
    Ok(())
}

/// Generation written in the snapshot prefix (0 when missing or a bare mailbox).
pub fn peek_snapshot_generation(dir: &Path) -> Result<u64> {
    let mut f = match File::open(dir.join(SNAPSHOT_FILE)) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let mut head = [0u8; 64];
    let mut filled = 0usize;
    while filled < head.len() {
        let n = f.read(head.get_mut(filled..).unwrap_or(&mut []))?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(parse_generation_prefix(head.get(..filled).unwrap_or(&[])).unwrap_or(0))
}

pub fn parse_generation_prefix(head: &[u8]) -> Option<u64> {
    let rest = head.strip_prefix(SNAPSHOT_PREFIX.as_bytes())?;
    let digits: Vec<u8> = rest.iter().copied().take_while(u8::is_ascii_digit).take(20).collect();
    std::str::from_utf8(&digits).ok()?.parse().ok()
}

/// Reads the snapshot: (generation, mailbox). Missing file = (0, empty mailbox).
pub fn read_snapshot(dir: &Path) -> Result<(u64, Mailbox)> {
    let Some(bytes) = read_capped(&dir.join(SNAPSHOT_FILE), MAX_STATE_BYTES)? else {
        return Ok((0, Mailbox::default()));
    };
    if bytes.starts_with(b"{\"mailcraft_snapshot\"") {
        let snap: SnapshotFile = serde_json::from_slice(&bytes).map_err(|e| corrupt("snapshot", e))?;
        if snap.mailcraft_snapshot != 1 {
            return Err(StoreError::Corrupt(format!("unknown snapshot format {}", snap.mailcraft_snapshot)));
        }
        Ok((snap.generation, snap.mailbox))
    } else {
        let mb: Mailbox = serde_json::from_slice(&bytes).map_err(|e| corrupt("snapshot", e))?;
        Ok((0, mb))
    }
}

/// Writes a snapshot atomically. `mb` must already be stripped of raw sources.
pub fn write_snapshot(dir: &Path, generation: u64, mb: &Mailbox) -> Result<()> {
    write_atomic(&dir.join(SNAPSHOT_FILE), |w| {
        w.write_all(SNAPSHOT_PREFIX.as_bytes())?;
        write!(w, "{generation},\"mailbox\":")?;
        serde_json::to_writer(&mut *w, mb).map_err(std::io::Error::other)?;
        w.write_all(b"}")
    })
}

/// Replaces the journal with an empty one carrying `generation`; returns the append handle.
pub fn reset_journal(dir: &Path, generation: u64) -> Result<File> {
    let path = dir.join(JOURNAL_FILE);
    let header = JournalHeader::new(generation).line();
    write_atomic(&path, |w| w.write_all(&header))?;
    Ok(OpenOptions::new().append(true).open(&path)?)
}

/// What [`prepare_journal`] found and fixed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Repair {
    /// The journal predated the snapshot and was discarded.
    pub stale_discarded: bool,
    /// Bytes of a torn last line removed.
    pub torn_bytes_removed: u64,
    /// A complete last op missing only its newline was kept.
    pub newline_added: bool,
}

/// Opens the journal for appending after crash repair: creates it if missing, discards it when older than the
/// snapshot, removes a torn tail (or completes a valid tail missing its newline). Returns (handle, generation).
pub fn prepare_journal(dir: &Path, snapshot_gen: u64) -> Result<(File, u64, Repair)> {
    let path = dir.join(JOURNAL_FILE);
    let mut repair = Repair::default();
    let Some(bytes) = read_capped(&path, MAX_STATE_BYTES)? else {
        return Ok((reset_journal(dir, snapshot_gen)?, snapshot_gen, repair));
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok((reset_journal(dir, snapshot_gen)?, snapshot_gen, repair));
    }
    let first_line = bytes.split(|&b| b == b'\n').next().unwrap_or(&[]);
    let header = JournalHeader::parse(first_line);
    if let Some(h) = &header {
        if h.generation < snapshot_gen {
            log::warn!(
                "journal generation {} predates snapshot generation {snapshot_gen}; discarding it",
                h.generation
            );
            repair.stale_discarded = true;
            return Ok((reset_journal(dir, snapshot_gen)?, snapshot_gen, repair));
        }
    } else {
        log::warn!("journal has no valid header; replaying its lines against the snapshot");
    }
    let generation = header.map_or(snapshot_gen, |h| h.generation);
    if bytes.last() != Some(&b'\n') {
        let keep = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
        let tail = bytes.get(keep..).unwrap_or(&[]);
        let tail_ok = serde_json::from_slice::<Op>(tail).is_ok();
        if tail_ok {
            let mut f = OpenOptions::new().append(true).open(&path)?;
            f.write_all(b"\n")?;
            f.sync_all()?;
            repair.newline_added = true;
        } else if keep == 0 {
            log::warn!("journal holds only a torn line ({} bytes); resetting it", tail.len());
            repair.torn_bytes_removed = u64::try_from(tail.len()).unwrap_or(0);
            return Ok((reset_journal(dir, generation)?, generation, repair));
        } else {
            log::warn!("journal ends with a torn line ({} bytes); truncating it", tail.len());
            let f = OpenOptions::new().write(true).open(&path)?;
            f.set_len(u64::try_from(keep).unwrap_or(0))?;
            f.sync_all()?;
            repair.torn_bytes_removed = u64::try_from(tail.len()).unwrap_or(0);
        }
    }
    Ok((OpenOptions::new().append(true).open(&path)?, generation, repair))
}

/// Result of reading the journal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JournalScan {
    pub generation: Option<u64>,
    pub ops: Vec<Op>,
    /// Lines that failed to parse and were skipped (1-based line numbers).
    pub skipped_lines: Vec<usize>,
    /// The last line was incomplete (no newline and not parseable).
    pub torn_tail: bool,
}

/// Parses journal bytes. Never fails: unparseable lines are skipped with a warning.
pub fn scan_journal(bytes: &[u8]) -> JournalScan {
    let mut scan = JournalScan::default();
    let ends_with_newline = bytes.last() == Some(&b'\n');
    let lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    let count = lines.len();
    for (idx, raw) in lines.into_iter().enumerate() {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if idx == 0
            && let Some(h) = JournalHeader::parse(line)
        {
            scan.generation = Some(h.generation);
            continue;
        }
        match serde_json::from_slice::<Op>(line) {
            Ok(op) => scan.ops.push(op),
            Err(e) => {
                let is_last = idx + 1 == count;
                if is_last && !ends_with_newline {
                    log::warn!("ignoring torn last journal line ({} bytes): {e}", line.len());
                    scan.torn_tail = true;
                } else {
                    log::warn!("skipping garbled journal line {}: {e}", idx + 1);
                    scan.skipped_lines.push(idx + 1);
                }
            }
        }
    }
    scan
}
