//! Journal ops (storage stage S4): a change to synchronised mail journals the item and its account's sync
//! state, and a configuration change journals the parts that changed, never the whole mailbox. Measured in
//! journal bytes per change, at two mailbox sizes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use mailcraft_engine::Session;
use mailcraft_engine::model::{
    Account, AccountId, AccountKind, FolderRole, ItemId, MailItem, ServerSettings,
};
use serde_json::{Value, json};

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn journal_bytes(s: &Session) -> u64 {
    s.store.as_ref().unwrap().journal_bytes()
}

/// The op names on the journal's lines after byte `from`.
fn ops_after(dir: &std::path::Path, from: u64) -> Vec<String> {
    let bytes = std::fs::read(dir.join("journal.jsonl")).unwrap();
    let tail = bytes.get(usize::try_from(from).unwrap()..).unwrap();
    String::from_utf8_lossy(tail)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            v.as_object().and_then(|o| o.keys().next().cloned()).unwrap_or_else(|| v.to_string())
        })
        .collect()
}

/// A profile with an IMAP account whose INBOX holds `n` synchronised messages (UIDs 1..=n), saved as a
/// snapshot so the journal starts empty. No server is contacted: the changes only queue.
fn imap_profile(dir: &std::path::Path, n: u32) -> (Session, AccountId, Vec<ItemId>) {
    let mut s = open_legacy(dir).unwrap();
    s.view.auto_send_receive_minutes = 0;
    s.view.work_offline = true;
    // Never fold the journal into a snapshot while measuring.
    s.checkpoint_bytes = 0;
    let account = s.mailbox.add_account(Account {
        kind: AccountKind::Imap,
        address: "me@example.net".into(),
        display_name: "me@example.net".into(),
        incoming: Some(ServerSettings { host: "imap.example.net".into(), port: 993, ..Default::default() }),
        ..Default::default()
    });
    s.mailbox.create_default_folders(account);
    let inbox = s.mailbox.role_folder(account, FolderRole::Inbox).unwrap();
    s.mailbox.folders.get_mut(&inbox).unwrap().remote_name = Some("INBOX".into());
    let ids: Vec<ItemId> = (1..=n)
        .map(|uid| {
            s.mailbox.add_mail(MailItem {
                folder: inbox,
                subject: format!("Synchronised message {uid}"),
                preview: format!("Text of message {uid}, about as long as a short note to a colleague."),
                remote_id: Some(uid.to_string()),
                ..Default::default()
            })
        })
        .collect();
    let mb = s.mailbox.clone();
    s.store.as_mut().unwrap().checkpoint(&mb).unwrap();
    s.selection.folder = inbox;
    (s, account, ids)
}

/// Bytes journaled per flag change (mark read, then flag), for the first `changes` messages.
fn per_flag_change(s: &mut Session, dir: &std::path::Path, ids: &[ItemId], changes: usize) -> Vec<u64> {
    let mut out = Vec::new();
    for (i, id) in ids.iter().take(changes).enumerate() {
        let before = journal_bytes(s);
        if i % 2 == 0 {
            run(s, "mail.markRead", json!({"ids": [id.0]}));
        } else {
            run(s, "mail.flag", json!({"ids": [id.0]}));
        }
        let wrote = journal_bytes(s) - before;
        let ops = ops_after(dir, before);
        assert!(ops.iter().any(|o| o == "PutSync"), "the queue of server changes is journaled: {ops:?}");
        assert_eq!(ops.iter().filter(|o| *o == "PutMail").count(), 1, "the message, once: {ops:?}");
        assert!(!ops.iter().any(|o| o == "Replace" || o == "PutAccount" || o == "PutConfig"), "{ops:?}");
        out.push(wrote);
    }
    out
}

/// Regression for issue #25 (storage stage S4): one flag change on an IMAP message journaled the whole
/// mailbox (its account's queue of server changes changed, so the account list did). Now it journals the
/// message and the account's sync state: a few hundred bytes, the same at 300 and at 3,000 messages.
#[test]
fn a_flag_change_on_synchronised_mail_journals_bytes_independent_of_the_mailbox() {
    const CHANGES: usize = 6;
    let mut per_size = Vec::new();
    for n in [300_u32, 3_000] {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, account, ids) = imap_profile(dir.path(), n);
        let snapshot = std::fs::metadata(dir.path().join("mailbox.json")).unwrap().len();
        let bytes = per_flag_change(&mut s, dir.path(), &ids, CHANGES);
        let undo_before = journal_bytes(&s);
        run(&mut s, "app.undo", json!({}));
        let undo = journal_bytes(&s) - undo_before;
        eprintln!("{n} messages: snapshot {snapshot} B, journal per flag change {bytes:?} B, undo {undo} B");
        let max = bytes.iter().copied().max().unwrap();
        assert!(max < 4_096, "{n} messages: {bytes:?}");
        assert!(undo < 4_096, "{n} messages: undo wrote {undo}");
        assert!(max * 50 < snapshot, "{n} messages: {max} of a {snapshot} byte mailbox");
        // Everything replays: the queue and the flags survive a restart.
        let queued = s.mailbox.account(account).unwrap().sync.pending.clone();
        assert!(!queued.is_empty());
        let mail: Vec<MailItem> =
            ids.iter().take(CHANGES).map(|id| s.mailbox.mail.get(*id).unwrap().clone()).collect();
        let accounts = s.mailbox.accounts.clone();
        drop(s);
        let s = open_legacy(dir.path()).unwrap();
        assert_eq!(s.mailbox.account(account).unwrap().sync.pending, queued);
        assert_eq!(s.mailbox.accounts, accounts);
        for m in &mail {
            assert_eq!(s.mailbox.mail.get(m.id), Some(m));
        }
        per_size.push(bytes);
    }
    // Ten times the mail, the same bytes per change (the item and the queue, not the mailbox).
    let (small, large) = (&per_size[0], &per_size[1]);
    for (a, b) in small.iter().zip(large) {
        assert!(b.abs_diff(*a) < 64, "300 vs 3,000 messages: {small:?} vs {large:?}");
    }
}

/// A new category, a changed view and a renamed account journal the configuration part that changed (the
/// accounts without their sync state), not the whole mailbox; and they replay.
#[test]
fn configuration_changes_journal_only_the_part_that_changed() {
    let dir = tempfile::tempdir().unwrap();
    // Under the 2,000 queued ops above which a change is saved whole (removing the account below).
    let (mut s, account, _) = imap_profile(dir.path(), 1_000);
    let snapshot = std::fs::metadata(dir.path().join("mailbox.json")).unwrap().len();
    let measure = |s: &mut Session, id: &str, p: Value| {
        let before = journal_bytes(s);
        run(s, id, p);
        let ops = ops_after(dir.path(), before);
        let wrote = journal_bytes(s) - before;
        eprintln!("{id}: {wrote} B {ops:?}");
        assert_eq!(ops, ["PutConfig"], "{id}");
        assert!(wrote * 50 < snapshot, "{id}: {wrote} of {snapshot}");
    };
    measure(&mut s, "categories.new", json!({"name": "Quarterly"}));
    measure(&mut s, "view.sortBy", json!({"field": "subject"}));
    measure(&mut s, "accounts.edit", json!({"account": account.0, "senderName": "Me, renamed"}));
    // A new server forgets the sync state: the settings and the sync state, still not the mailbox.
    s.mailbox.accounts.iter_mut().find(|a| a.id == account).unwrap().sync.next_op = 7;
    let before = journal_bytes(&s);
    let r = run(
        &mut s,
        "accounts.edit",
        json!({"account": account.0, "incoming": {"host": "other.example.net"}}),
    );
    assert_eq!(r["resetSync"], true);
    let ops = ops_after(dir.path(), before);
    assert_eq!(ops.get(..2), Some(&["PutConfig".to_owned(), "PutSync".to_owned()][..]), "{:?}", ops.get(..4));
    assert!(!ops.iter().any(|o| o == "Replace"));
    assert_eq!(ops.iter().filter(|o| *o == "PutMail").count(), 1_000, "the UIDs are forgotten");
    let memory = s.mailbox.clone();
    drop(s);
    let mut s = open_legacy(dir.path()).unwrap();
    assert_eq!(s.mailbox.categories, memory.categories);
    assert_eq!(s.mailbox.views, memory.views);
    assert_eq!(s.mailbox.accounts, memory.accounts);
    // Removing the account takes its folders and mail with it, one op each, and the account list.
    let before = journal_bytes(&s);
    s.checkpoint_bytes = 0;
    run(&mut s, "accounts.remove", json!({"account": account.0, "confirm": true}));
    let ops = ops_after(dir.path(), before);
    assert_eq!(ops.first().map(String::as_str), Some("PutConfig"), "{:?}", ops.first());
    assert!(!ops.iter().any(|o| o == "Replace"));
    assert_eq!(ops.iter().filter(|o| *o == "RemoveMail").count(), 1_000);
    let memory = s.mailbox.clone();
    drop(s);
    let s = open_legacy(dir.path()).unwrap();
    assert!(s.mailbox.account(account).is_none());
    assert_eq!(s.mailbox.accounts, memory.accounts);
    assert_eq!(s.mailbox.folders, memory.folders);
    assert_eq!(s.mailbox.mail.len(), memory.mail.len());
}

/// These tests measure the legacy journal and snapshot: their profiles use legacy storage whatever
/// `MAILCRAFT_STORE` says.
fn open_legacy(dir: &std::path::Path) -> Result<Session, mailcraft_engine::CmdError> {
    Session::open_with(dir, mailcraft_store::StoreKind::Legacy)
}
