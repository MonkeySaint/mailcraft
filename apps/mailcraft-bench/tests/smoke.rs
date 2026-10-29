//! The benchmark harness on a 2,000-message profile: every measurement runs and reports sane numbers. No
//! thresholds (timings depend on the machine); this only keeps the harness from rotting.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use mailcraft_bench::{Backend, MARKER, Options, QUERIES};
use serde_json::Value;

fn work_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mailcraft-bench-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn num(r: &Value, p: &str) -> f64 {
    let v = r.pointer(p).and_then(Value::as_f64);
    assert!(v.is_some(), "{p} missing in {r:#}");
    v.unwrap_or_default()
}

#[test]
fn two_thousand_messages_smoke() {
    smoke(Backend::Legacy);
}

#[test]
fn two_thousand_messages_smoke_on_sqlite() {
    smoke(Backend::Sqlite);
}

fn smoke(backend: Backend) {
    let work = work_dir(&format!("smoke-{}", backend.name()));
    let mut opts = Options::new(2_000);
    opts.backend = backend;
    opts.small_bodies = true;
    opts.sync_messages = 600;
    opts.bulk_move = 300;
    opts.windows = 10;
    let r = mailcraft_bench::run(&work, &opts).unwrap();
    assert_eq!(r["backend"], backend.name());
    assert_eq!(num(&r, "/generate/messages"), 2_000.0);
    let kinds = &r["generate"]["kinds"];
    let sum: u64 = ["text", "html", "attachments"].iter().map(|k| kinds[k].as_u64().unwrap()).sum();
    assert_eq!(sum, 2_000);
    let per_folder: u64 =
        r["generate"]["folders"].as_object().unwrap().values().map(|n| n.as_u64().unwrap()).sum();
    assert_eq!(per_folder, 2_000);
    assert!(num(&r, "/generate/disk/parts/mailbox.json") > 0.0);
    match backend {
        Backend::Legacy => assert!(num(&r, "/generate/disk/parts/blobs") > 0.0),
        Backend::Sqlite => {
            assert!(num(&r, "/generate/disk/parts/store.db") > 0.0);
            assert!(num(&r, "/generate/disk/parts/packs") > 0.0);
            assert!(r["generate"]["disk"]["parts"].get("blobs").is_none());
        }
    }
    // The opened profile holds every message, and the first window is full.
    assert_eq!(num(&r, "/measure/open/mail_items"), 2_000.0);
    assert_eq!(num(&r, "/measure/open/window_rows"), 50.0);
    assert!(num(&r, "/measure/open/ms") >= 0.0);
    assert!(num(&r, "/measure/folder_switch/items") > 400.0, "the Inbox is the largest folder");
    assert_eq!(r["measure"]["folder_switch"]["folder"], "Inbox");
    assert!(num(&r, "/measure/folder_switch/conversation_rows") > 0.0);
    assert_eq!(num(&r, "/measure/scroll/windows"), 10.0);
    // Every query ran; the rare term and the common ones are found.
    let queries = r["measure"]["search"]["queries"].as_array().unwrap();
    assert_eq!(queries.len(), QUERIES.len());
    let hits = |q: &str| queries.iter().find(|x| x["query"] == q).and_then(|x| x["hits"].as_u64()).unwrap();
    assert!(hits(mailcraft_testmail::synth::RARE_TERM) >= 1);
    assert!(hits(mailcraft_testmail::synth::RARE_TERM) < 20);
    assert!(hits("meeting") > 100);
    assert!(hits("hasattachments:yes") > 100);
    assert!(hits("isread:no") > 100);
    assert!(num(&r, "/measure/search/search_folder_total") > 100.0);
    // Synchronised mail landed: whole messages in INBOX, headers then bodies in Headers.
    assert_eq!(num(&r, "/measure/sync/inbox_items"), 600.0);
    assert_eq!(num(&r, "/measure/sync/headers_items"), 600.0);
    assert!(num(&r, "/measure/sync/full_per_sec") > 0.0);
    // Commands ran and journaled.
    assert_eq!(num(&r, "/measure/commands/bulk_move_items"), 300.0);
    for k in [
        "mark_read_1_ms",
        "move_1_ms",
        "bulk_move_ms",
        "undo_bulk_move_ms",
        "redo_bulk_move_ms",
        "delete_1_ms",
        "imap_mark_read_1_ms",
        "imap_undo_1_ms",
        "imap_move_1_ms",
        "imap_undo_move_1_ms",
        "config_new_category_ms",
        "config_sort_ms",
        "undo_bytes",
        "small_state_bytes",
    ] {
        assert!(r["measure"]["commands"][k].is_number(), "{k}");
    }
    // A command on synchronised mail journals the item and its sync state, a configuration change the part
    // that changed: never the whole mailbox.
    let whole = num(&r, "/measure/open/mailbox_approx_bytes");
    for k in [
        "imap_mark_read_1_journal_bytes",
        "imap_move_1_journal_bytes",
        "config_new_category_journal_bytes",
        "config_sort_journal_bytes",
    ] {
        let wrote = num(&r, &format!("/measure/commands/{k}"));
        assert!(wrote > 0.0 && wrote < whole / 10.0, "{k}: {wrote} of {whole}");
    }
    assert!(num(&r, "/measure/commands/journal_bytes") > 0.0);
    assert_sizes_are_explicit(&r, 2_000, 600);
    std::fs::remove_dir_all(&work).unwrap();
}

/// The commands ran on the profile as generated, before the synchronised mail grew it, and every phase
/// says how many mail items it started with.
fn assert_sizes_are_explicit(r: &Value, messages: usize, sync: usize) {
    let (messages, sync) = (messages as f64, sync as f64);
    for section in ["folder_switch", "scroll", "search", "commands"] {
        assert_eq!(num(r, &format!("/measure/{section}/mail_items")), messages, "{section}");
    }
    assert_eq!(num(r, "/measure/sync/mail_items_before_full"), messages);
    assert_eq!(num(r, "/measure/sync/mail_items_before_headers"), messages + sync);
    assert_eq!(num(r, "/measure/sync/mail_items_before_bodies"), messages + 2.0 * sync);
    assert_eq!(num(r, "/measure/sync/mail_items_after"), messages + 2.0 * sync);
    // Only the commands on synchronised mail run after the sync (they need its messages).
    assert_eq!(num(r, "/measure/commands/imap_mail_items"), messages + 2.0 * sync);
    assert!(r.pointer("/measure/memory_before_sync").is_some_and(Value::is_object));
    assert!(num(r, "/generate/estimate/memory_bytes") > 0.0);
}

/// The default `cargo xtask bench` path: realistic bodies (HTML newsletters, base64 attachments of up to
/// 5 MB, header-only downloads of large sources, then their bodies).
#[test]
fn full_bodies_smoke() {
    let work = work_dir("full");
    let mut opts = Options::new(200);
    opts.sync_messages = 100;
    opts.bulk_move = 50;
    opts.windows = 5;
    let r = mailcraft_bench::run(&work, &opts).unwrap();
    assert_eq!(r["small_bodies"], false);
    assert!(num(&r, "/generate/avg_source_bytes") > 10_000.0, "realistic bodies");
    assert!(num(&r, "/generate/kinds/attachments") > 0.0);
    assert!(num(&r, "/generate/kinds/html") > 0.0);
    assert_eq!(num(&r, "/measure/open/mail_items"), 200.0);
    assert_eq!(num(&r, "/measure/sync/inbox_items"), 100.0);
    assert_eq!(num(&r, "/measure/sync/headers_items"), 100.0);
    assert_eq!(num(&r, "/measure/commands/bulk_move_items"), 50.0);
    assert!(r["measure"]["search"]["queries"].as_array().is_some_and(|q| q.len() == QUERIES.len()));
    assert_sizes_are_explicit(&r, 200, 100);
    std::fs::remove_dir_all(&work).unwrap();
}

/// A folder without the marker (a real profile, a home folder handed over by mistake) is never deleted
/// or opened; an empty folder or an earlier benchmark profile is replaced.
#[test]
fn only_benchmark_profiles_are_replaced_or_measured() {
    let work = work_dir("marker");
    let dir = work.join("not-a-bench-profile");
    std::fs::create_dir_all(dir.join("blobs")).unwrap();
    std::fs::write(dir.join("mailbox.json"), "{}").unwrap();
    std::fs::write(dir.join("blobs").join("keep"), "mail").unwrap();
    let opts = Options { sync_messages: 5, bulk_move: 3, windows: 1, ..Options::new(20) };
    let err = mailcraft_bench::generate(&dir, &opts).unwrap_err();
    assert!(err.contains("not a benchmark profile"), "{err}");
    assert_eq!(std::fs::read_to_string(dir.join("mailbox.json")).unwrap(), "{}");
    assert_eq!(std::fs::read_to_string(dir.join("blobs").join("keep")).unwrap(), "mail");
    assert!(!dir.join(MARKER).exists());
    // `measure` refuses it too and leaves it as it was.
    let err = mailcraft_bench::measure(&dir, &opts).unwrap_err();
    assert!(err.contains("not a benchmark profile"), "{err}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    assert!(mailcraft_bench::remove_profile(&dir).is_err());
    assert!(dir.join("blobs").join("keep").exists());
    // `run --work` with such a folder at work/profile refuses as well.
    let run_work = work.join("run");
    std::fs::create_dir_all(mailcraft_bench::profile_dir(&run_work)).unwrap();
    std::fs::write(mailcraft_bench::profile_dir(&run_work).join("notes.txt"), "mine").unwrap();
    assert!(mailcraft_bench::run(&run_work, &opts).unwrap_err().contains("not a benchmark profile"));
    assert!(mailcraft_bench::profile_dir(&run_work).join("notes.txt").exists());
    // A file is not replaced either.
    let file = work.join("file");
    std::fs::write(&file, "x").unwrap();
    assert!(mailcraft_bench::generate(&file, &opts).is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "x");
    // An empty folder is fine, and a benchmark profile is replaced by the next run.
    let bench = work.join("bench");
    std::fs::create_dir_all(&bench).unwrap();
    mailcraft_bench::generate(&bench, &opts).unwrap();
    assert!(mailcraft_bench::is_bench_profile(&bench));
    std::fs::write(bench.join("stale"), "old run").unwrap();
    mailcraft_bench::generate(&bench, &opts).unwrap();
    assert!(!bench.join("stale").exists());
    mailcraft_bench::remove_profile(&bench).unwrap();
    assert!(!bench.exists());
    std::fs::remove_dir_all(&work).unwrap();
}

/// Sizes that would not fit (full bodies above 50k, small above 2m, more memory or disk than the machine
/// has) are refused before anything is written.
#[test]
fn oversized_runs_are_refused() {
    use mailcraft_bench::{check_budget, estimate};
    let full = Options::new(100_000);
    let err = check_budget(&full, &estimate(&full), None, None).unwrap_err();
    assert!(err.contains("--small-bodies"), "{err}");
    let small = Options { small_bodies: true, ..Options::new(1_000_000) };
    check_budget(&small, &estimate(&small), None, None).unwrap();
    let huge = Options { small_bodies: true, ..Options::new(5_000_000) };
    assert!(check_budget(&huge, &estimate(&huge), None, None).is_err());
    let forced = Options { force_large: true, ..huge.clone() };
    check_budget(&forced, &estimate(&forced), None, None).unwrap();
    // Memory: a 10k full run (30k items with the synchronised mail) needs over a GB, more than three
    // quarters of a 1.5 GB machine (unless forced).
    let ten = Options::new(10_000);
    let est = estimate(&ten);
    assert_eq!(est.items, 30_000);
    assert!((1_000_000_000..3_000_000_000).contains(&est.memory_bytes), "{est:?}");
    assert!((1_000_000_000..3_000_000_000).contains(&est.disk_bytes), "{est:?}");
    assert!(check_budget(&ten, &est, None, Some(1_500_000_000)).is_err());
    check_budget(&ten, &est, None, Some(64_000_000_000)).unwrap();
    let ten_forced = Options { force_large: true, ..ten.clone() };
    check_budget(&ten_forced, &est, None, Some(1_500_000_000)).unwrap();
    // The largest allowed runs fit a 64 GB machine.
    let most = Options { small_bodies: true, ..Options::new(2_000_000) };
    check_budget(&most, &estimate(&most), None, Some(64_000_000_000)).unwrap();
    let most_full = Options::new(50_000);
    check_budget(&most_full, &estimate(&most_full), None, Some(64_000_000_000)).unwrap();
    // Disk: never above the free space, forced or not.
    assert!(check_budget(&ten_forced, &est, Some(est.disk_bytes - 1), None).is_err());
    check_budget(&ten, &est, Some(est.disk_bytes), None).unwrap();
    // `generate` refuses before touching the folder.
    let work = work_dir("budget");
    let err = mailcraft_bench::generate(&work.join("p"), &full).unwrap_err();
    assert!(err.contains("--small-bodies"), "{err}");
    assert!(!work.exists());
}

#[test]
fn measuring_a_profile_of_the_other_backend_is_refused() {
    let work = work_dir("mismatch");
    let mut opts = Options::new(50);
    opts.small_bodies = true;
    mailcraft_bench::generate(&work, &opts).unwrap();
    opts.backend = Backend::Sqlite;
    assert!(mailcraft_bench::measure(&work, &opts).unwrap_err().contains("not a sqlite profile"));
    std::fs::remove_dir_all(&work).unwrap();
}

/// `generate_batch = 1` writes a SQLite profile the way the app stores sources today (one fsync and commit
/// each), for a like-for-like comparison with legacy; the report says which was used.
#[test]
fn a_sqlite_profile_can_be_generated_without_batching() {
    let work = work_dir("unbatched");
    let mut opts = Options::new(120);
    opts.small_bodies = true;
    opts.backend = Backend::Sqlite;
    opts.generate_batch = 1;
    let g = mailcraft_bench::generate(&work, &opts).unwrap();
    assert_eq!(g["batch"], 1);
    assert_eq!(g["messages"], 120);
    assert!(num(&g, "/disk/parts/packs") > 0.0);
    let store = mailcraft_store::Store::open(&work).unwrap();
    assert_eq!(store.load().unwrap().mail.len(), 120);
    drop(store);
    std::fs::remove_dir_all(&work).unwrap();
}

#[test]
fn sizes_parse() {
    assert_eq!(mailcraft_bench::parse_size("10k").unwrap(), 10_000);
    assert_eq!(mailcraft_bench::parse_size("1M").unwrap(), 1_000_000);
    assert_eq!(mailcraft_bench::parse_size("2000").unwrap(), 2_000);
    for bad in ["", "k", "0", "ten", "99999999999999999999m", "20m"] {
        assert!(mailcraft_bench::parse_size(bad).is_err(), "{bad}");
    }
}

/// The synthetic Maildir imports with the generator's read and flagged state: flagged unread mail sits in
/// `cur/` with its flags (an importer reads no flags from `new/`).
#[test]
fn synthetic_maildir_imports_with_its_flags() {
    use mailcraft_engine::model::{FlagState, FolderRole, Mailbox};
    use mailcraft_testmail::synth::{self, Config};
    let work = work_dir("maildir");
    let cfg = Config::small(300);
    let msgs: Vec<_> = cfg.generate().collect();
    assert!(msgs.iter().any(|m| m.flagged && !m.read), "the case that needs cur/");
    assert_eq!(synth::write_maildir(&work, &cfg).unwrap(), 300);
    let mut mb = Mailbox::new_local("Bench");
    let account = mb.default_account().unwrap().id;
    let inbox = mb.role_folder(account, FolderRole::Inbox).unwrap();
    let mut bodies = mailcraft_store::BodyStore::in_memory();
    for i in 0..synth::FOLDERS.len() {
        let dir = work.join(synth::folder_path(i));
        mailcraft_store::import_maildir(&mut mb, &mut bodies, &dir, inbox).unwrap();
    }
    let items = mb.items_in(inbox);
    assert_eq!(items.len(), 300);
    let read = items.iter().filter(|m| m.read).count();
    let flagged = items.iter().filter(|m| matches!(m.flag, FlagState::Flagged { .. })).count();
    assert_eq!(read, msgs.iter().filter(|m| m.read).count());
    assert_eq!(flagged, msgs.iter().filter(|m| m.flagged).count());
    std::fs::remove_dir_all(&work).unwrap();
}
