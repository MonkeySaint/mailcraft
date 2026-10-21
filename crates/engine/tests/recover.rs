//! Recover Deleted Items: the recoverable-items store keeps permanently deleted items across restarts and
//! beyond the undo history, and purges them after the retention period.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use mailcraft_engine::model::{DateTime, FolderRole, ItemId, MailItem, Note};
use mailcraft_engine::{Notice, Session};
use serde_json::{Value, json};

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

const T0: i64 = 1_800_000_000;
const DAY: i64 = 86_400;

fn at(s: &mut Session, t: i64) {
    s.fixed_now = Some(DateTime::from_unix(t, 0));
}

/// Puts a message in Deleted Items and empties it (a permanent delete); returns its id.
fn delete_for_good(s: &mut Session, subject: &str) -> ItemId {
    let account = s.mailbox.accounts[0].id;
    let deleted = s.mailbox.role_folder(account, FolderRole::DeletedItems).unwrap();
    let id = s.mailbox.add_mail(MailItem { folder: deleted, subject: subject.into(), ..Default::default() });
    run(s, "folder.emptyDeleted", json!({}));
    assert!(!s.mailbox.mail.contains_key(&id));
    id
}

fn listed(s: &mut Session) -> Vec<Value> {
    let r = run(s, "folder.recoverDeleted", json!({}));
    assert_eq!(r["dialog"], "recoverDeletedItems");
    s.notices
        .iter()
        .rev()
        .find_map(|n| match n {
            Notice::OpenDialog(d, p) if d == "recoverDeletedItems" => {
                Some(p["items"].as_array().unwrap().clone())
            }
            _ => None,
        })
        .unwrap()
}

#[test]
fn survives_restart_and_the_undo_limit_then_purges() {
    let dir = tempfile::tempdir().unwrap();
    let id;
    {
        let mut s = Session::open(dir.path()).unwrap();
        at(&mut s, T0);
        id = delete_for_good(&mut s, "Quarterly figures");
        // More than the undo history holds.
        for i in 0..110 {
            run(&mut s, "folder.new", json!({"name": format!("F{i}")}));
        }
        assert!(s.undo.undo.iter().all(|e| !e.before.mail.contains_key(&id)), "gone from the undo history");
        let l = listed(&mut s);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0]["subject"], "Quarterly figures");
        assert_eq!(l[0]["deleted"], T0);
        assert_eq!(l[0]["folder"], "Deleted Items");
    }
    // A restart.
    let mut s = Session::open(dir.path()).unwrap();
    at(&mut s, T0 + DAY);
    assert!(s.undo.undo.is_empty());
    let l = listed(&mut s);
    assert_eq!(l.len(), 1, "still recoverable after a restart");
    let r = run(&mut s, "folder.recoverDeleted", json!({"ids": [id.0]}));
    assert_eq!(r["restored"], 1);
    let back = r["ids"][0].as_u64().unwrap();
    assert_eq!(s.mailbox.mail[&ItemId(back)].subject, "Quarterly figures");
    assert!(listed(&mut s).is_empty(), "a recovered item leaves the list");
    // Undoing the recovery makes it recoverable again.
    run(&mut s, "app.undo", json!({}));
    assert_eq!(listed(&mut s).len(), 1);

    // Purge after the retention period (14 days by default), on the hourly schedule.
    at(&mut s, T0 + 15 * DAY);
    s.poll_sync();
    assert!(listed(&mut s).is_empty(), "purged after 14 days");
    drop(s);
    let mut s = Session::open(dir.path()).unwrap();
    at(&mut s, T0 + 15 * DAY);
    assert!(listed(&mut s).is_empty(), "the purge reached the disk");
}

#[test]
fn an_unreadable_recover_file_is_set_aside_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    drop(Session::open(dir.path()).unwrap());
    let file = dir.path().join(mailcraft_store::RECOVERABLE_FILE);
    std::fs::write(&file, b"{\"items\": [garbled").unwrap();
    let mut s = Session::open(dir.path()).unwrap();
    assert!(!file.exists(), "the unreadable file is moved away");
    let bad = dir.path().join(format!("{}.bad", mailcraft_store::RECOVERABLE_FILE));
    assert_eq!(std::fs::read(&bad).unwrap(), b"{\"items\": [garbled", "kept as .bad for inspection");
    assert!(
        s.notices.iter().any(|n| matches!(n, Notice::Error(t) if t.contains("recoverable deleted items"))),
        "the user is told: {:?}",
        s.notices
    );
    assert!(listed(&mut s).is_empty());
    // The bin works again from empty and reaches the disk.
    at(&mut s, T0);
    delete_for_good(&mut s, "After the damage");
    assert!(file.exists());
    drop(s);
    let mut s = Session::open(dir.path()).unwrap();
    at(&mut s, T0);
    assert_eq!(listed(&mut s).len(), 1);
}

#[test]
fn removing_an_account_does_not_fill_the_bin() {
    use mailcraft_engine::model::{Account, AccountKind, ServerSettings};
    let mut s = Session::new(mailcraft_engine::model::Mailbox::new_local("Local Folders"));
    at(&mut s, T0);
    let kept = delete_for_good(&mut s, "Deleted on purpose");
    let id = s.mailbox.add_account(Account {
        kind: AccountKind::Imap,
        address: "old@example.net".into(),
        display_name: "old@example.net".into(),
        incoming: Some(ServerSettings::default()),
        ..Default::default()
    });
    s.mailbox.create_default_folders(id);
    let inbox = s.mailbox.role_folder(id, FolderRole::Inbox).unwrap();
    for i in 0..50 {
        s.mailbox.add_mail(MailItem {
            folder: inbox,
            subject: format!("Offline copy {i}"),
            ..Default::default()
        });
    }
    let r = run(&mut s, "accounts.remove", json!({"account": id.0, "confirm": true}));
    assert_eq!(r["removed"], true);
    let l = listed(&mut s);
    assert_eq!(l.len(), 1, "only the item deleted on purpose: {l:?}");
    assert_eq!(l[0]["id"], kept.0);
}

#[test]
fn retention_option_and_purge_selected() {
    let mut s = Session::new(mailcraft_engine::model::Mailbox::new_local("Local Folders"));
    at(&mut s, T0);
    let a = delete_for_good(&mut s, "One");
    let b = delete_for_good(&mut s, "Two");
    assert_eq!(listed(&mut s).len(), 2);
    let r = run(&mut s, "folder.recoverDeleted", json!({"purge": [a.0]}));
    assert_eq!(r["purged"], 1);
    let l = listed(&mut s);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0]["id"], b.0);
    assert!(s.run("folder.recoverDeleted", &json!({"purge": "x"})).is_err());
    // A shorter retention applies at once; 0 keeps nothing.
    at(&mut s, T0 + 3 * DAY);
    run(&mut s, "options.set", json!({"key": "advanced.recoverDays", "value": 2}));
    assert!(listed(&mut s).is_empty());
    run(&mut s, "options.set", json!({"key": "advanced.recoverDays", "value": 0}));
    delete_for_good(&mut s, "Three");
    assert!(listed(&mut s).is_empty(), "nothing kept with 0 days");
    assert!(s.run("options.set", &json!({"key": "advanced.recoverDays", "value": 400})).is_err());
}

#[test]
fn notes_are_recoverable_too() {
    let mut s = Session::new(mailcraft_engine::model::Mailbox::new_local("Local Folders"));
    at(&mut s, T0);
    let account = s.mailbox.accounts[0].id;
    let notes = s.mailbox.role_folder(account, FolderRole::Notes).unwrap();
    let deleted = s.mailbox.role_folder(account, FolderRole::DeletedItems).unwrap();
    let note = s.mailbox.alloc_id();
    s.mailbox.notes.insert(
        ItemId(note),
        Note { id: ItemId(note), folder: notes, text: "Call the plumber".into(), ..Default::default() },
    );
    run(&mut s, "notes.delete", json!({"ids": [note]}));
    if s.mailbox.notes.contains_key(&ItemId(note)) {
        assert_eq!(s.mailbox.notes[&ItemId(note)].folder, deleted);
        run(&mut s, "folder.emptyDeleted", json!({}));
    }
    let l = listed(&mut s);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0]["kind"], "note");
    assert_eq!(l[0]["subject"], "Call the plumber");
}
