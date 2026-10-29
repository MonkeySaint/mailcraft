//! The benchmark harness on a 2,000-message profile: every measurement runs and reports sane numbers. No
//! thresholds (timings depend on the machine); this only keeps the harness from rotting.

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
    let work = work_dir("smoke");
    let mut opts = Options::new(2_000);
    opts.small_bodies = true;
    opts.sync_messages = 600;
    opts.bulk_move = 300;
    opts.windows = 10;
    let r = mailcraft_bench::run(&work, &opts).unwrap();
    assert_eq!(r["backend"], "legacy");
    assert_eq!(num(&r, "/generate/messages"), 2_000.0);
    let kinds = &r["generate"]["kinds"];
    let sum: u64 = ["text", "html", "attachments"].iter().map(|k| kinds[k].as_u64().unwrap()).sum();
    assert_eq!(sum, 2_000);
    let per_folder: u64 =
        r["generate"]["folders"].as_object().unwrap().values().map(|n| n.as_u64().unwrap()).sum();
    assert_eq!(per_folder, 2_000);
    assert!(num(&r, "/generate/disk/parts/mailbox.json") > 0.0);
    assert!(num(&r, "/generate/disk/parts/blobs") > 0.0);
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
        "undo_bytes",
        "small_state_bytes",
    ] {
        assert!(r["measure"]["commands"][k].is_number(), "{k}");
    }
    // A command on synchronised mail journals the item and its account, not the whole mailbox.
    let whole = num(&r, "/measure/open/mailbox_approx_bytes");
    for k in ["imap_mark_read_1_journal_bytes", "imap_move_1_journal_bytes"] {
        let wrote = num(&r, &format!("/measure/commands/{k}"));
        assert!(wrote > 0.0 && wrote < whole / 10.0, "{k}: {wrote} of {whole}");
    }
    assert!(num(&r, "/measure/commands/journal_bytes") > 0.0);
    std::fs::remove_dir_all(&work).unwrap();
}

#[test]
fn the_sqlite_backend_is_not_there_yet() {
    let mut opts = Options::new(10);
    opts.backend = Backend::Sqlite;
    let work = work_dir("sqlite");
    assert!(mailcraft_bench::run(&work, &opts).unwrap_err().contains("S6"));
    assert!(!work.exists());
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
