//! Pack file tests: round trips, de-duplication, codecs, the crash rule, batches, roll-over, collection and
//! damaged or hostile files.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};

use super::*;
use crate::BodyStore;
use crate::sql::Db;

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// A message source of about `n` bytes of text (compresses well).
fn text_source(tag: &str, n: usize) -> Vec<u8> {
    let mut s = format!("From: ann@example.com\r\nSubject: {tag}\r\n\r\n");
    while s.len() < n {
        s.push_str("The quarterly budget review is on Thursday. ");
    }
    s.into_bytes()
}

/// `n` bytes that do not compress.
fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[0]
        })
        .collect()
}

fn packs(db: &Db) -> Packs {
    Packs::new(db.clone())
}

fn rows(db: &Db) -> i64 {
    db.conn().query_row("SELECT count(*) FROM bodies", [], |r| r.get(0)).unwrap()
}

fn location(db: &Db, key: &str) -> (i64, i64, i64, i64) {
    db.conn()
        .query_row("SELECT pack, off, stored_len, codec FROM bodies WHERE key = ?1", [key], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap()
}

fn committed(db: &Db, pack: i64) -> u64 {
    let n: i64 =
        db.conn().query_row("SELECT committed_len FROM packs WHERE id = ?1", [pack], |r| r.get(0)).unwrap();
    u64::try_from(n).unwrap()
}

#[test]
fn header_round_trips_and_rejects_garbage() {
    let h = Header {
        codec: CODEC_ZSTD,
        kind: 1,
        raw_len: 12345,
        stored_len: 678,
        sha256: [7; 32],
        crc32: 0xDEAD_BEEF,
    };
    let bytes = h.encode();
    assert_eq!(bytes.len(), HEADER_LEN);
    assert_eq!(&bytes[..4], b"MCPK");
    assert_eq!(Header::decode(&bytes), Some(h));
    let mut bad = bytes;
    bad[0] = b'X';
    assert_eq!(Header::decode(&bad), None);
    let mut bad = bytes;
    bad[4] = 9;
    assert_eq!(Header::decode(&bad), None);
    assert_eq!(Header::decode(&bytes[..10]), None);
    assert_eq!(Header::decode(&[]), None);
}

#[test]
fn sources_and_bodies_round_trip_compressed_and_stored() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let text = text_source("text", 20_000);
    let random = noise(1, 5_000);
    let kt = p.put(Kind::Source, &text).unwrap();
    let kr = p.put(Kind::Source, &random).unwrap();
    let kb = p.put(Kind::Body, b"{\"text\":\"hello\"}").unwrap();
    assert_eq!(kt, BlobKey::for_data(&text).to_string(), "keys keep the content-key grammar");
    assert_eq!(p.get(Kind::Source, &kt).unwrap(), text);
    assert_eq!(p.get(Kind::Source, &kr).unwrap(), random);
    assert_eq!(p.get(Kind::Body, &kb).unwrap(), b"{\"text\":\"hello\"}");
    // Text is compressed, noise is stored as it is.
    let (_, _, stored, codec) = location(&db, &kt);
    assert_eq!(codec, i64::from(CODEC_ZSTD));
    assert!(stored < 2_000, "{stored}");
    assert_eq!(location(&db, &kr).3, i64::from(CODEC_STORE));
    // Kinds are separate: a source key is not a body.
    assert!(p.get(Kind::Body, &kt).is_err());
    assert!(p.has(Kind::Source, &kt) && !p.has(Kind::Body, &kt));
    // Heads of both codecs, shorter and longer than the content.
    assert_eq!(p.head(Kind::Source, &kt, 10).unwrap().unwrap(), &text[..10]);
    assert_eq!(p.head(Kind::Source, &kr, 100).unwrap().unwrap(), &random[..100]);
    assert_eq!(p.head(Kind::Source, &kt, usize::MAX).unwrap().unwrap(), text);
    assert_eq!(p.head(Kind::Source, "0000000000000000-1", 10).unwrap(), None);
    // Everything survives a reopen.
    drop(p);
    drop(db);
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    assert_eq!(p.get(Kind::Source, &kt).unwrap(), text);
    assert_eq!(p.get(Kind::Source, &kr).unwrap(), random);
    assert_eq!(db.integrity_check().unwrap(), "ok");
}

#[test]
fn equal_content_is_stored_once() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let a = text_source("same", 3_000);
    let k1 = p.put(Kind::Source, &a).unwrap();
    let len = fs::metadata(pack_path(d.path(), 1)).unwrap().len();
    let k2 = p.put(Kind::Source, &a).unwrap();
    assert_eq!(k1, k2);
    assert_eq!(rows(&db), 1);
    assert_eq!(fs::metadata(pack_path(d.path(), 1)).unwrap().len(), len, "nothing appended for a duplicate");
    // The same bytes as a body record are another entry.
    p.put(Kind::Body, &a).unwrap();
    assert_eq!(rows(&db), 2);
    // In a batch too.
    p.begin_batch();
    let b = text_source("batch", 3_000);
    let k3 = p.put(Kind::Source, &b).unwrap();
    assert_eq!(p.put(Kind::Source, &b).unwrap(), k3);
    assert_eq!(p.finish_batch().unwrap(), 1);
    assert_eq!(rows(&db), 3);
}

#[test]
fn a_key_owned_by_other_content_is_probed_past() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let data = text_source("probe", 1_000);
    let plain = BlobKey::for_data(&data).to_string();
    // Another entry (different hash) already holds the plain key.
    let other = p.put(Kind::Source, b"other content").unwrap();
    db.conn().execute("UPDATE bodies SET key = ?1 WHERE key = ?2", [&plain, &other]).unwrap();
    let key = p.put(Kind::Source, &data).unwrap();
    assert_eq!(key, format!("{plain}-1"));
    assert_eq!(p.get(Kind::Source, &key).unwrap(), data);
    assert_eq!(p.get(Kind::Source, &plain).unwrap(), b"other content");
}

#[test]
fn opening_cuts_an_uncommitted_tail() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let a = text_source("kept", 5_000);
    let ka = p.put(Kind::Source, &a).unwrap();
    let end = committed(&db, 1);
    drop(p);
    drop(db);
    // A crash after appending, before the commit: bytes past committed_len.
    let mut f = OpenOptions::new().append(true).open(pack_path(d.path(), 1)).unwrap();
    f.write_all(b"MCPK half a record").unwrap();
    drop(f);
    let db = Db::open(d.path()).unwrap();
    assert_eq!(fs::metadata(pack_path(d.path(), 1)).unwrap().len(), end);
    let p = packs(&db);
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), a);
    // New records go right after the committed ones.
    let b = noise(9, 700);
    let kb = p.put(Kind::Source, &b).unwrap();
    assert_eq!(location(&db, &kb).1, i64::try_from(end).unwrap());
    assert_eq!(p.get(Kind::Source, &kb).unwrap(), b);
}

#[test]
fn an_unfinished_batch_leaves_nothing_behind() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let a = text_source("before", 4_000);
    let ka = p.put(Kind::Source, &a).unwrap();
    let end = committed(&db, 1);
    p.begin_batch();
    let b = text_source("in the batch", 4_000);
    let kb = p.put(Kind::Source, &b).unwrap();
    // Readable while the batch is open, not yet indexed.
    assert_eq!(p.get(Kind::Source, &kb).unwrap(), b);
    assert!(p.has(Kind::Source, &kb));
    assert_eq!(rows(&db), 1);
    assert_eq!(committed(&db, 1), end);
    // The process ends without finishing the batch.
    drop(p);
    drop(db);
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    assert_eq!(fs::metadata(pack_path(d.path(), 1)).unwrap().len(), end);
    assert!(p.get(Kind::Source, &kb).is_err());
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), a);
    // A finished batch is one commit.
    p.begin_batch();
    let keys: Vec<String> =
        (0..20).map(|i| p.put(Kind::Source, &text_source(&format!("m{i}"), 2_000)).unwrap()).collect();
    assert_eq!(p.finish_batch().unwrap(), 20);
    assert!(!db.packs().in_batch());
    drop(p);
    drop(db);
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    for (i, k) in keys.iter().enumerate() {
        assert_eq!(p.get(Kind::Source, k).unwrap(), text_source(&format!("m{i}"), 2_000));
    }
}

#[test]
fn packs_roll_over() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    p.set_roll_bytes(10_000);
    let mut keys = Vec::new();
    for i in 0..12 {
        keys.push(p.put(Kind::Source, &noise(i, 3_000)).unwrap());
    }
    // Rolled packs in a batch, too.
    p.begin_batch();
    for i in 12..24 {
        keys.push(p.put(Kind::Source, &noise(i, 3_000)).unwrap());
    }
    p.finish_batch().unwrap();
    let packs_n: i64 = db.conn().query_row("SELECT count(*) FROM packs", [], |r| r.get(0)).unwrap();
    assert!(packs_n >= 6, "{packs_n}");
    for id in 1..=packs_n {
        assert!(fs::metadata(pack_path(d.path(), id)).unwrap().len() <= 10_000 + 3_000 + HEADER_LEN as u64);
        assert_eq!(fs::metadata(pack_path(d.path(), id)).unwrap().len(), committed(&db, id));
    }
    drop(p);
    drop(db);
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    for (i, k) in keys.iter().enumerate() {
        assert_eq!(p.get(Kind::Source, k).unwrap(), noise(i as u64, 3_000));
    }
}

#[test]
fn damage_is_an_error_never_a_panic() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let text = text_source("damage", 10_000);
    let random = noise(3, 2_000);
    let kt = p.put(Kind::Source, &text).unwrap();
    let kr = p.put(Kind::Source, &random).unwrap();
    let path = pack_path(d.path(), 1);
    let flip = |at: u64| {
        let mut f = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(at)).unwrap();
        let mut b = [0u8; 1];
        std::io::Read::read_exact(&mut f, &mut b).unwrap();
        f.seek(SeekFrom::Start(at)).unwrap();
        f.write_all(&[b[0] ^ 0x5a]).unwrap();
    };
    // A payload byte (CRC), then a header byte.
    let (_, off_r, _, _) = location(&db, &kr);
    flip(u64::try_from(off_r).unwrap() + HEADER_LEN as u64 + 10);
    assert!(matches!(p.get(Kind::Source, &kr), Err(StoreError::Corrupt(_))));
    flip(u64::try_from(off_r).unwrap() + 8);
    assert!(matches!(p.get(Kind::Source, &kr), Err(StoreError::Corrupt(_))));
    assert!(p.head(Kind::Source, &kr, 10).is_err());
    // The other entry is untouched.
    assert_eq!(p.get(Kind::Source, &kt).unwrap(), text);
    // Hostile index rows: offsets past the end or inside a record, lengths past the caps, unknown codecs,
    // bad hashes.
    for sql in [
        "UPDATE bodies SET off = 999999999 WHERE key = ?1",
        "UPDATE bodies SET off = -5 WHERE key = ?1",
        "UPDATE bodies SET off = off + 1 WHERE key = ?1",
        "UPDATE bodies SET raw_len = 9223372036854775807 WHERE key = ?1",
        "UPDATE bodies SET stored_len = raw_len + 1 WHERE key = ?1",
        "UPDATE bodies SET stored_len = stored_len - 1 WHERE key = ?1",
        "UPDATE bodies SET raw_len = raw_len - 1 WHERE key = ?1",
        "UPDATE bodies SET codec = 7 WHERE key = ?1",
        "UPDATE bodies SET codec = 300 WHERE key = ?1",
        "UPDATE bodies SET sha256 = x'00' WHERE key = ?1",
    ] {
        {
            let conn = db.conn();
            conn.execute_batch("SAVEPOINT t").unwrap();
            conn.execute(sql, [&kt]).unwrap();
        }
        assert!(p.get(Kind::Source, &kt).is_err(), "{sql}");
        let _ = p.head(Kind::Source, &kt, 100);
        db.conn().execute_batch("ROLLBACK TO t; RELEASE t").unwrap();
        assert_eq!(p.get(Kind::Source, &kt).unwrap(), text, "{sql} restored");
    }
    // A pack cut short: the entry past the end is an error.
    let f = OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(30).unwrap();
    assert!(p.get(Kind::Source, &kt).is_err());
}

#[test]
fn collection_drops_unreferenced_entries_and_counts_dead_bytes() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let ka = p.put(Kind::Source, &text_source("a", 2_000)).unwrap();
    let kb = p.put(Kind::Source, &text_source("b", 2_000)).unwrap();
    let kc = p.put(Kind::Body, b"{\"text\":\"c\"}").unwrap();
    // b is not live but a mail row still refers to it: it stays.
    db.conn().execute("UPDATE bodies SET refs = 1 WHERE key = ?1", [&kb]).unwrap();
    let live: BTreeSet<String> = [ka.clone()].into_iter().collect();
    let (pack_c, off_c, stored_c, _) = location(&db, &kc);
    assert_eq!(p.collect(&live, None).unwrap(), 1);
    // The removed record is overwritten in its pack (not readable until compaction).
    let bytes = fs::read(pack_path(d.path(), pack_c)).unwrap();
    let span = usize::try_from(off_c).unwrap()..usize::try_from(off_c + stored_c).unwrap() + HEADER_LEN;
    assert!(bytes[span].iter().all(|b| *b == 0));
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), text_source("a", 2_000), "its neighbours stay");
    assert!(p.has(Kind::Source, &ka) && p.has(Kind::Source, &kb) && !p.has(Kind::Body, &kc));
    let stats = p.stats().unwrap();
    assert_eq!(stats.entries, 2);
    assert!(stats.dead_bytes > 0 && stats.live_bytes > 0, "{stats:?}");
    // Only the keys asked about.
    db.conn().execute("UPDATE bodies SET refs = 0", []).unwrap();
    let only: BTreeSet<String> = [kb.clone()].into_iter().collect();
    assert_eq!(p.collect(&BTreeSet::new(), Some(&only)).unwrap(), 1);
    assert!(p.has(Kind::Source, &ka));
    assert!(!p.has(Kind::Source, &kb));
}

#[test]
fn body_store_over_packs() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let mut bodies = BodyStore::packs(db.clone());
    assert!(bodies.is_packs());
    let raw = text_source("store", 5_000);
    let key = bodies.put_source(&raw).unwrap();
    assert_eq!(bodies.source(&key).unwrap(), raw);
    assert_eq!(bodies.source_head(&key, 4).unwrap().unwrap(), b"From");
    assert!(bodies.has_source(&key));
    let body = mailcraft_model::Body { text: Some("hello".into()), ..Default::default() };
    let bkey = bodies.put_body(&body).unwrap();
    bodies.clear_cache();
    assert_eq!(*bodies.body_record(&bkey).unwrap(), body);
    assert!(bodies.has_body(&bkey) && !bodies.has_body(&key));
    // Collection keeps what is live.
    let live: BTreeSet<String> = [bkey.clone()].into_iter().collect();
    assert_eq!(bodies.collect(&live).unwrap(), 1);
    assert!(!bodies.has_source(&key));
    assert!(bodies.has_body(&bkey));
    // Export and import between a pack store and memory.
    let exported = bodies.export(&live);
    let mut mem = BodyStore::in_memory();
    assert_eq!(mem.import(exported).unwrap(), 1);
    assert_eq!(*mem.body_record(&bkey).unwrap(), body);
    assert!(bodies.pack_stats().unwrap().unwrap().entries >= 1);
}

/// Every `bodies` row reads back, and every pack is exactly as long as its committed length.
fn assert_consistent(db: &Db, dir: &std::path::Path) {
    let p = packs(db);
    let keys: Vec<(i64, String)> = {
        let conn = db.conn();
        let mut stmt = conn.prepare("SELECT kind, key FROM bodies").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    for (kind, key) in keys {
        let kind = if kind == 0 { Kind::Source } else { Kind::Body };
        p.get(kind, &key).unwrap_or_else(|e| panic!("{key}: {e}"));
    }
    for (id, committed) in committed_lengths(db).unwrap() {
        let on_disk = fs::metadata(pack_path(dir, id)).map_or(0, |m| m.len());
        assert_eq!(on_disk, committed, "pack {id}");
    }
}

/// Points the current pack's handle at a read-only one (writes fail), or back at a writable one.
fn set_writable(db: &Db, writable: bool) {
    let mut w = db.packs();
    let dir = w.dir.clone();
    let pack = w.current.as_mut().unwrap();
    pack.file = OpenOptions::new().read(true).write(writable).open(pack_path(&dir, pack.id)).unwrap();
}

#[test]
fn a_failed_write_in_a_batch_drops_only_that_record() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let before = p.put(Kind::Source, &text_source("committed", 3_000)).unwrap();
    p.begin_batch();
    let a = text_source("a", 3_000);
    let b = noise(2, 2_000);
    let ka = p.put(Kind::Source, &a).unwrap();
    let kb = p.put(Kind::Source, &b).unwrap();
    // The disk refuses the next record.
    set_writable(&db, false);
    assert!(p.put(Kind::Source, &noise(3, 2_000)).is_err());
    set_writable(&db, true);
    // Later records go after the earlier ones, never over them; equal content still de-duplicates.
    let c = text_source("c", 4_000);
    let kc = p.put(Kind::Source, &c).unwrap();
    assert_eq!(p.put(Kind::Source, &a).unwrap(), ka);
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), a);
    assert_eq!(p.finish_batch().unwrap(), 3);
    drop(p);
    drop(db);
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), a);
    assert_eq!(p.get(Kind::Source, &kb).unwrap(), b);
    assert_eq!(p.get(Kind::Source, &kc).unwrap(), c);
    assert!(p.get(Kind::Source, &before).is_ok());
    assert!(!p.has(Kind::Source, &BlobKey::for_data(&noise(3, 2_000)).to_string()));
    assert_eq!(rows(&db), 4);
    assert_consistent(&db, d.path());
}

#[test]
fn a_failed_write_outside_a_batch_leaves_the_pack_as_it_was() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let a = text_source("a", 3_000);
    let ka = p.put(Kind::Source, &a).unwrap();
    let end = committed(&db, 1);
    set_writable(&db, false);
    assert!(p.put(Kind::Source, &noise(4, 1_000)).is_err());
    set_writable(&db, true);
    db.packs().faults.fail_sync = true;
    assert!(p.put(Kind::Source, &noise(5, 1_000)).is_err());
    assert_eq!(fs::metadata(pack_path(d.path(), 1)).unwrap().len(), end);
    let kb = p.put(Kind::Source, &noise(6, 1_000)).unwrap();
    assert_eq!(location(&db, &kb).1, i64::try_from(end).unwrap());
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), a);
    assert_eq!(rows(&db), 2);
    assert_consistent(&db, d.path());
}

#[test]
fn a_failed_fsync_at_the_end_of_a_batch_drops_the_batch() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    p.set_roll_bytes(10_000);
    let a = noise(10, 2_000);
    let ka = p.put(Kind::Source, &a).unwrap();
    p.begin_batch();
    // Enough to roll over into new packs.
    let in_batch: Vec<String> = (0..8).map(|i| p.put(Kind::Source, &noise(20 + i, 3_000)).unwrap()).collect();
    db.packs().faults.fail_sync = true;
    assert!(p.finish_batch().is_err());
    assert!(!db.packs().in_batch());
    for k in &in_batch {
        assert!(!p.has(Kind::Source, k), "{k} is not indexed");
    }
    assert_eq!(rows(&db), 1);
    assert_consistent(&db, d.path());
    // The profile keeps working, and storing the same content again works.
    p.begin_batch();
    let again: Vec<String> = (0..8).map(|i| p.put(Kind::Source, &noise(20 + i, 3_000)).unwrap()).collect();
    assert_eq!(p.finish_batch().unwrap(), 8);
    drop(p);
    drop(db);
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), a);
    for (i, k) in again.iter().enumerate() {
        assert_eq!(p.get(Kind::Source, k).unwrap(), noise(20 + i as u64, 3_000));
    }
    assert_consistent(&db, d.path());
}

#[test]
fn a_failed_fsync_during_a_roll_over_fails_the_batch() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    p.set_roll_bytes(10_000);
    let a = noise(30, 2_000);
    let ka = p.put(Kind::Source, &a).unwrap();
    p.begin_batch();
    let returned = vec![
        p.put(Kind::Source, &noise(31, 3_000)).unwrap(),
        p.put(Kind::Source, &noise(32, 3_000)).unwrap(),
    ];
    // The next record does not fit: the full pack is synced before a new one starts, and that fails.
    db.packs().faults.fail_sync = true;
    assert!(p.put(Kind::Source, &noise(33, 6_000)).is_err());
    // Nothing more of the batch is taken, nothing of it is indexed.
    assert!(p.put(Kind::Source, &noise(34, 100)).is_err());
    assert!(p.finish_batch().is_err());
    for k in &returned {
        assert!(!p.has(Kind::Source, k));
    }
    assert_eq!(rows(&db), 1);
    assert_consistent(&db, d.path());
    // Outside the failed batch everything works again.
    let kb = p.put(Kind::Source, &noise(35, 3_000)).unwrap();
    drop(p);
    drop(db);
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), a);
    assert_eq!(p.get(Kind::Source, &kb).unwrap(), noise(35, 3_000));
    assert_consistent(&db, d.path());
}

#[test]
fn a_roll_over_that_cannot_start_its_pack_keeps_the_batch() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    p.set_roll_bytes(10_000);
    p.begin_batch();
    let mut kept = Vec::new();
    for i in 0..3 {
        kept.push((40 + i, p.put(Kind::Source, &noise(40 + i, 3_000)).unwrap()));
    }
    // The next pack file cannot be created: a directory holds its name, and lost+found cannot be made to
    // move it away.
    let blocked = pack_path(d.path(), 2);
    fs::create_dir(&blocked).unwrap();
    let lost = d.path().join(PACKS_DIR).join(LOST_FOUND_DIR);
    fs::write(&lost, b"not a directory").unwrap();
    assert!(p.put(Kind::Source, &noise(43, 3_000)).is_err());
    fs::remove_dir(&blocked).unwrap();
    fs::remove_file(&lost).unwrap();
    // The batch goes on in another pack, and the full one is committed with it.
    for i in 44..48 {
        kept.push((i, p.put(Kind::Source, &noise(i, 3_000)).unwrap()));
    }
    assert_eq!(p.finish_batch().unwrap(), kept.len());
    drop(p);
    drop(db);
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    assert_eq!(p.recovery().truncated, Vec::new(), "nothing referenced was cut off");
    for (i, k) in &kept {
        assert_eq!(p.get(Kind::Source, k).unwrap(), noise(*i, 3_000), "record {i}");
    }
    assert_consistent(&db, d.path());
}

#[test]
fn orphan_packs_and_cut_tails_are_kept_in_lost_and_found() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let a = text_source("kept", 3_000);
    let ka = p.put(Kind::Source, &a).unwrap();
    drop(p);
    drop(db);
    // A pack the database has no row for (store.db restored from an older copy), and a tail.
    let orphan = pack_path(d.path(), 7);
    fs::write(&orphan, b"MCPK mail of a lost row").unwrap();
    let end = fs::metadata(pack_path(d.path(), 1)).unwrap().len();
    let mut f = OpenOptions::new().append(true).open(pack_path(d.path(), 1)).unwrap();
    f.write_all(b"MCPK an uncommitted tail").unwrap();
    drop(f);
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let rec = p.recovery();
    assert_eq!(rec.orphans, 1);
    assert_eq!(rec.truncated, vec![(1, 24)]);
    let lost = d.path().join(PACKS_DIR).join(LOST_FOUND_DIR);
    assert!(!orphan.exists());
    assert_eq!(fs::read(lost.join("000007.pack")).unwrap(), b"MCPK mail of a lost row");
    assert_eq!(fs::read(lost.join(format!("000001-{end}.tail"))).unwrap(), b"MCPK an uncommitted tail");
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), a);
    // A new pack that takes a stray file's number does not overwrite it either.
    fs::write(pack_path(d.path(), 2), b"another stray").unwrap();
    p.set_roll_bytes(1);
    let kb = p.put(Kind::Source, &noise(50, 500)).unwrap();
    assert_eq!(location(&db, &kb).0, 2);
    assert_eq!(fs::read(lost.join("000002.pack")).unwrap(), b"another stray");
    assert_eq!(p.get(Kind::Source, &kb).unwrap(), noise(50, 500));
}

#[test]
fn a_garbled_committed_length_leaves_the_pack_alone() {
    for bad in ["-1", "'many'", "1.5", "x'00'"] {
        let d = tmp();
        let db = Db::open(d.path()).unwrap();
        let p = packs(&db);
        let a = text_source("a", 3_000);
        let ka = p.put(Kind::Source, &a).unwrap();
        // A tail past the committed length too: still nothing is cut.
        let mut f = OpenOptions::new().append(true).open(pack_path(d.path(), 1)).unwrap();
        f.write_all(b"more bytes").unwrap();
        drop(f);
        let len = fs::metadata(pack_path(d.path(), 1)).unwrap().len();
        db.execute_raw(&format!("UPDATE packs SET committed_len = {bad} WHERE id = 1")).unwrap();
        drop(p);
        drop(db);
        let db = Db::open(d.path()).unwrap();
        let p = packs(&db);
        assert_eq!(fs::metadata(pack_path(d.path(), 1)).unwrap().len(), len, "{bad}: the pack is untouched");
        assert_eq!(p.recovery().short, vec![1], "{bad}");
        assert!(p.recovery().truncated.is_empty());
        assert_eq!(p.get(Kind::Source, &ka).unwrap(), a, "{bad}: its entries still read");
        // Nothing is appended to it: a new record starts a new pack.
        let kb = p.put(Kind::Source, &noise(60, 700)).unwrap();
        assert_eq!(location(&db, &kb).0, 2, "{bad}");
        assert_eq!(fs::metadata(pack_path(d.path(), 1)).unwrap().len(), len);
    }
}

#[test]
fn content_that_cannot_be_read_back_is_stored_again_under_its_key() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let a = text_source("a", 6_000);
    let b = noise(70, 4_000);
    let ka = p.put(Kind::Source, &a).unwrap();
    let kb = p.put(Kind::Source, &b).unwrap();
    fn row_of(db: &Db, k: &str) -> i64 {
        db.conn().query_row("SELECT id FROM bodies WHERE key = ?1", [k], |r| r.get(0)).unwrap()
    }
    let (row_a, row_b) = (row_of(&db, &ka), row_of(&db, &kb));
    // The pack loses b's last bytes; opening flags it.
    let (_, off_b, stored_b, _) = location(&db, &kb);
    drop(p);
    let f = OpenOptions::new().write(true).open(pack_path(d.path(), 1)).unwrap();
    f.set_len(u64::try_from(off_b + stored_b).unwrap()).unwrap();
    drop(f);
    drop(db);
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    assert_eq!(p.recovery().short, vec![1]);
    assert!(p.get(Kind::Source, &kb).is_err());
    // a is intact: still de-duplicated. b is written afresh, same key and row (mail rows name the row).
    assert_eq!(p.put(Kind::Source, &a).unwrap(), ka);
    assert_eq!(location(&db, &ka).0, 1);
    assert_eq!(p.put(Kind::Source, &b).unwrap(), kb);
    assert_eq!(row_of(&db, &kb), row_b);
    assert_eq!(location(&db, &kb).0, 2);
    assert_eq!(p.get(Kind::Source, &kb).unwrap(), b);
    assert_eq!(rows(&db), 2);
    // In a batch too, for a record whose header no longer matches (in a pack opening did not flag).
    db.packs().recovery.short.clear();
    let (_, off_a, _, _) = location(&db, &ka);
    let mut f = OpenOptions::new().write(true).open(pack_path(d.path(), 1)).unwrap();
    f.seek(SeekFrom::Start(u64::try_from(off_a).unwrap())).unwrap();
    f.write_all(b"XXXX").unwrap();
    drop(f);
    p.begin_batch();
    assert_eq!(p.put(Kind::Source, &a).unwrap(), ka);
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), a, "the batch's record is read meanwhile");
    assert_eq!(p.finish_batch().unwrap(), 1);
    assert_eq!(row_of(&db, &ka), row_a);
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), a);
    let stats = p.stats().unwrap();
    assert!(stats.dead_bytes > 0, "{stats:?}");
    drop(p);
    drop(db);
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    assert_eq!(p.get(Kind::Source, &ka).unwrap(), a);
    assert_eq!(p.get(Kind::Source, &kb).unwrap(), b);
    assert_eq!(db.integrity_check().unwrap(), "ok");
}

#[test]
fn collection_by_key_uses_the_kind_and_key_index() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let plan: Vec<String> = {
        let conn = db.conn();
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {COLLECT_KEY_SQL}")).unwrap();
        stmt.query_map(params![0, "k"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert!(plan.iter().any(|l| l.contains("sqlite_autoindex_bodies_1")), "{plan:?}");
    assert!(!plan.iter().any(|l| l.starts_with("SCAN")), "{plan:?}");
    // Both kinds of a key are considered.
    let p = packs(&db);
    let data = text_source("both", 1_000);
    let ks = p.put(Kind::Source, &data).unwrap();
    let kb = p.put(Kind::Body, &data).unwrap();
    assert_eq!(ks, kb);
    let only: BTreeSet<String> = [ks.clone()].into_iter().collect();
    assert_eq!(p.collect(&BTreeSet::new(), Some(&only)).unwrap(), 2);
    assert_eq!(rows(&db), 0);
}

#[test]
fn collection_never_zeroes_a_record_its_row_does_not_describe() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let p = packs(&db);
    let a = text_source("a", 2_000);
    let b = text_source("b", 2_000);
    let ka = p.put(Kind::Source, &a).unwrap();
    let kb = p.put(Kind::Source, &b).unwrap();
    // A damaged row: a's row points at b's record.
    let (_, off_b, _, _) = location(&db, &kb);
    db.conn().execute("UPDATE bodies SET off = ?1 WHERE key = ?2", params![off_b, &ka]).unwrap();
    let live: BTreeSet<String> = [kb.clone()].into_iter().collect();
    assert_eq!(p.collect(&live, None).unwrap(), 1);
    assert_eq!(p.get(Kind::Source, &kb).unwrap(), b, "b's record is not overwritten");
    // A row at an offset inside its record (no header there) does not zero it either.
    let len = fs::metadata(pack_path(d.path(), 1)).unwrap().len();
    db.conn().execute("UPDATE bodies SET off = off + 10 WHERE key = ?1", [&kb]).unwrap();
    assert_eq!(p.collect(&BTreeSet::new(), None).unwrap(), 1);
    let bytes = fs::read(pack_path(d.path(), 1)).unwrap();
    assert_eq!(bytes.len() as u64, len);
    let at = usize::try_from(off_b).unwrap();
    assert_eq!(&bytes[at..at + 4], &MAGIC, "b's record is still there");
}
