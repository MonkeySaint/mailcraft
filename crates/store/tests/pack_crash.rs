//! Storage stage S6, the pack crash rule (storage ADR §2.3 and §7, risk 4): a child process writes batches of
//! message sources to the pack files and mail rows that refer to them, and is killed at a different moment
//! each round. After every kill the profile must open, cut each pack back to its committed length, and hold
//! every batch the child reported finished, with every entry and row intact and consistent. Packs roll over
//! at 64 KiB in the child, so batches cross pack boundaries and kills land during roll-overs too.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use mailcraft_model::{FolderId, ItemId, MailItem};
use mailcraft_store::packs::{self, pack_path};
use mailcraft_store::{BodyStore, Db, Op, SqlStore};

const CHILD_ENV: &str = "MAILCRAFT_PACK_CRASH_DIR";
const PROGRESS: &str = "progress.txt";
const PER_ROUND: u64 = 25;
/// Packs roll over this small in the child, so most batches cross into a new pack (and some kills land
/// during a roll-over: the full pack's fsync, the new pack's row and file).
const ROLL_BYTES: u64 = 64 << 10;

/// The source of message `j` of round `r` (2–40 KB of text, with a varying part so nothing de-duplicates).
fn source(r: u64, j: u64) -> Vec<u8> {
    let mut s = format!("From: ann@example.com\r\nSubject: round {r} message {j}\r\n\r\n");
    let n = 2_000 + ((r * 7919 + j * 104_729) % 38_000) as usize;
    let mut k = r ^ (j << 20);
    while s.len() < n {
        k = k.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        s.push_str(&format!("line {k:x} of the quarterly budget review\r\n"));
    }
    s.into_bytes()
}

fn rounds_done(dir: &Path) -> Vec<u64> {
    fs::read_to_string(dir.join(PROGRESS))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// The child: writes rounds until it is killed.
#[test]
#[ignore = "run by pack_writes_survive_being_killed in a child process"]
fn child_writer() {
    let Some(dir) = std::env::var_os(CHILD_ENV) else { return };
    let dir = Path::new(&dir);
    let db = Db::open(dir).unwrap();
    packs::Packs::new(db.clone()).set_roll_bytes(ROLL_BYTES);
    let mut bodies = BodyStore::packs(db.clone());
    let mut st = SqlStore::new(db.clone());
    st.load().unwrap();
    let start = rounds_done(dir).last().map_or(0, |r| r + 1);
    for r in start..start + 10_000 {
        bodies.begin_batch();
        let mut ops = Vec::new();
        for j in 0..PER_ROUND {
            let key = bodies.put_source(&source(r, j)).unwrap();
            let item = MailItem {
                id: ItemId(r * 100 + j + 1),
                folder: FolderId(1 + j % 3),
                read: j % 2 == 0,
                blob: Some(key),
                ..Default::default()
            };
            ops.push(Op::PutMail(Box::new(item)));
        }
        bodies.finish_batch().unwrap();
        // Rows after the content they name; then one write outside a batch.
        st.append(&ops, u64::MAX).unwrap();
        bodies.put_source(&source(r, 99)).unwrap();
        let mut f = OpenOptions::new().create(true).append(true).open(dir.join(PROGRESS)).unwrap();
        writeln!(f, "{r}").unwrap();
        f.sync_data().unwrap();
    }
}

/// Checks the profile after a kill; returns how many packs opening cut back and how many packs there are.
fn verify(dir: &Path) -> (usize, usize) {
    let db = Db::open(dir).unwrap();
    let cut = packs::Packs::new(db.clone()).recovery().truncated.len();
    assert_eq!(db.integrity_check().unwrap(), "ok");
    // The crash rule: every pack is exactly as long as committed.
    let stats = BodyStore::packs(db.clone()).pack_stats().unwrap().unwrap();
    for (id, committed) in packs::committed_lengths(&db).unwrap() {
        let on_disk = fs::metadata(pack_path(dir, id)).map(|m| m.len()).unwrap_or(0);
        assert_eq!(on_disk, committed, "pack {id}");
    }
    // Every finished round is there, content and rows.
    let bodies = BodyStore::packs(db.clone());
    let mb = SqlStore::new(db.clone()).load().unwrap();
    for r in rounds_done(dir) {
        for j in 0..PER_ROUND {
            let m = mb.mail.get(ItemId(r * 100 + j + 1)).unwrap_or_else(|| panic!("round {r} item {j}"));
            assert_eq!(bodies.source(m.blob.as_ref().unwrap()).unwrap(), source(r, j));
        }
        assert!(bodies.has_source(&mailcraft_store::BlobKey::for_data(&source(r, 99)).to_string()));
    }
    // Every row of every round, finished or not, names content that is there and intact.
    for m in mb.mail.items() {
        let raw = bodies.source(m.blob.as_ref().unwrap()).unwrap();
        let r = (m.id.0 - 1) / 100;
        let j = (m.id.0 - 1) % 100;
        assert_eq!(raw, source(r, j), "item {}", m.id.0);
    }
    // Counts agree with the rows.
    for f in 1..=3 {
        assert_eq!(
            mailcraft_store::MailRepo::counts(&mailcraft_store::SqlRepo::new(db.clone()), FolderId(f)),
            mb.mail.counts(FolderId(f))
        );
    }
    assert!(stats.entries >= mb.mail.len());
    (cut, stats.packs)
}

#[test]
fn pack_writes_survive_being_killed() {
    let d = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut finished = 0usize;
    let mut cuts = 0usize;
    let mut packs_n = 0usize;
    // At least 10 kills; more (up to 40) until one of them left an uncommitted tail to cut off.
    for round in 0..40u64 {
        if round >= 10 && cuts > 0 {
            break;
        }
        let mut child = Command::new(&exe)
            .args(["child_writer", "--exact", "--ignored", "--test-threads=1"])
            .env(CHILD_ENV, d.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // A different moment each time: during open, mid-batch, between the batch and its rows.
        std::thread::sleep(Duration::from_millis(40 + (round * 67) % 260));
        let _ = child.kill();
        let _ = child.wait();
        let (cut, n) = verify(d.path());
        cuts += cut;
        packs_n = n;
        finished = rounds_done(d.path()).len();
    }
    assert!(finished > 0, "the child never finished a round; the test proved nothing");
    assert!(cuts > 0, "no kill landed between a write and its commit; the test proved nothing");
    assert!(
        packs_n > finished,
        "batches did not roll over into new packs ({packs_n} packs, {finished} rounds)"
    );
}
