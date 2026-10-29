//! The benchmark harness on a 2,000-message profile: every measurement runs and reports sane numbers. No
//! thresholds (timings depend on the machine); this only keeps the harness from rotting.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use mailcraft_bench::{Backend, Options, QUERIES};
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
    std::fs::remove_dir_all(&work).unwrap();
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
