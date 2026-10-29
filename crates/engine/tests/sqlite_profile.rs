//! Storage stage S6: a session on a SQLite profile (`--store sqlite`). Commands, undo and redo, bodies and
//! sources, restarts and the legacy profile next to it behave as on the legacy storage, whatever
//! `MAILCRAFT_STORE` says.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use mailcraft_engine::Session;
use mailcraft_engine::model::{DateTime, FolderRole, ItemId, MailItem};
use mailcraft_store::{Content, Op, StoreKind};
use serde_json::json;

fn now() -> DateTime {
    DateTime { unix: 1_760_000_000, offset_min: 0 }
}

/// A SQLite profile holding the sample mailbox (its content copied into the profile's packs).
fn sample_profile(dir: &std::path::Path) -> Session {
    let sample = mailcraft_engine::sample::sample_session_at(now());
    let mut s = Session::open_with(dir, StoreKind::Sqlite).unwrap();
    assert_eq!(s.store.as_ref().unwrap().kind(), StoreKind::Sqlite);
    let keys = mailcraft_store::bodies::keys_of(sample.mailbox.mail.items());
    let n = s.bodies.import(sample.bodies.export(&keys)).unwrap();
    assert_eq!(n, keys.len());
    s.mailbox = sample.mailbox.clone();
    s.journal(vec![Op::Replace(Box::new(s.mailbox.clone()))]);
    s.flush_pending();
    assert!(s.pending_ops.is_empty());
    s.fixed_now = Some(now());
    s
}

/// The JSON paths where two mailboxes differ (a readable failure instead of two whole mailboxes).
fn differences(a: &mailcraft_engine::model::Mailbox, b: &mailcraft_engine::model::Mailbox) -> Vec<String> {
    fn walk(path: String, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
        use serde_json::Value;
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                    let (va, vb) = (x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null));
                    walk(format!("{path}.{k}"), va, vb, out);
                }
            }
            (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
                for (i, (va, vb)) in x.iter().zip(y).enumerate() {
                    walk(format!("{path}[{i}]"), va, vb, out);
                }
            }
            _ if a != b => out.push(format!("{path}: {a} != {b}")),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(String::new(), &serde_json::to_value(a).unwrap(), &serde_json::to_value(b).unwrap(), &mut out);
    out.truncate(20);
    out
}

fn inbox_ids(s: &Session) -> Vec<ItemId> {
    let acct = s.mailbox.default_account().unwrap().id;
    let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
    s.mailbox.mail.ids_in(inbox)
}

#[test]
fn commands_undo_and_restarts_on_a_sqlite_profile() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = sample_profile(dir.path());
    assert!(dir.path().join(mailcraft_store::SQL_DB_FILE).is_file());
    assert!(dir.path().join(mailcraft_store::packs::PACKS_DIR).is_dir());
    let ids = inbox_ids(&s);
    assert!(ids.len() >= 3, "the sample Inbox has mail");
    let (a, b, c) = (ids[0], ids[1], ids[2]);
    let acct = s.mailbox.default_account().unwrap().id;
    let archive = s.mailbox.role_folder(acct, FolderRole::Archive).unwrap();
    s.run("mail.markRead", &json!({"ids": [a.0]})).unwrap();
    s.run("mail.move", &json!({"ids": [b.0], "folder": archive.0})).unwrap();
    s.run("mail.delete", &json!({"ids": [c.0]})).unwrap();
    s.run("app.undo", &json!({})).unwrap();
    s.run("app.undo", &json!({})).unwrap();
    s.run("app.redo", &json!({})).unwrap();
    // A message with a source of its own.
    let raw =
        b"From: ann@example.com\r\nTo: bob@example.org\r\nSubject: Kept\r\n\r\nKept in a pack\r\n".to_vec();
    let inbox = s.mailbox.mail.get(a).unwrap().folder;
    let added = s
        .add_message(
            MailItem { folder: inbox, subject: "Kept".into(), ..Default::default() },
            &Content::source(raw.clone()),
        )
        .unwrap();
    // Outside a command: journaled by hand.
    s.journal(vec![Op::PutMail(Box::new(s.mailbox.mail.get(added).unwrap().clone()))]);
    s.flush_pending();
    let before = s.mailbox.clone();
    let body_a = s.body(a).unwrap().map(|b| (*b).clone());
    drop(s);

    // Reopened (no storage asked for): the same mailbox, the same content.
    let s = Session::open(dir.path()).unwrap();
    assert_eq!(s.store.as_ref().unwrap().kind(), StoreKind::Sqlite);
    assert_eq!(differences(&s.mailbox, &before), Vec::<String>::new());
    assert!(s.mailbox.mail.get(a).unwrap().read);
    assert_eq!(s.mailbox.mail.get(b).unwrap().folder, archive);
    assert_eq!(s.source(added).unwrap().as_deref(), Some(&raw[..]));
    assert_eq!(s.body(a).unwrap().map(|b| (*b).clone()), body_a);
    // No notice about the storage: the profile has the one it asked for.
    assert!(s.notices.is_empty(), "{:?}", s.notices);
}

#[test]
fn a_legacy_profile_stays_legacy_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    drop(Session::open_with(dir.path(), StoreKind::Legacy).unwrap());
    let s = Session::open_with(dir.path(), StoreKind::Sqlite).unwrap();
    assert_eq!(s.store.as_ref().unwrap().kind(), StoreKind::Legacy);
    assert!(!dir.path().join(mailcraft_store::SQL_DB_FILE).exists());
    assert!(
        s.notices.iter().any(|n| format!("{n:?}").contains("SQLite storage is only used for new profiles")),
        "{:?}",
        s.notices
    );
}

#[test]
fn exit_folds_the_log_into_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = sample_profile(dir.path());
    let ids = inbox_ids(&s);
    for id in &ids {
        s.run("mail.markRead", &json!({"ids": [id.0]})).unwrap();
    }
    s.run("file.exit", &json!({})).unwrap();
    let wal = dir.path().join(format!("{}-wal", mailcraft_store::SQL_DB_FILE));
    assert_eq!(std::fs::metadata(&wal).map_or(0, |m| m.len()), 0, "the write-ahead log is folded in on exit");
    let before = s.mailbox.clone();
    drop(s);
    assert_eq!(differences(&Session::open(dir.path()).unwrap().mailbox, &before), Vec::<String>::new());
}
