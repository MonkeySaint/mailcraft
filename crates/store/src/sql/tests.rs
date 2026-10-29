//! SQLite store tests: schema, the store against `Op::apply` (random op sequences), counts and reference
//! counts, the whole-store `Store` on a SQLite profile.

use std::collections::BTreeMap;

use mailcraft_model::account::AccountSync;
use mailcraft_model::{
    Account, AccountId, Appointment, Category, Folder, FolderId, ItemId, MailItem, Mailbox, Note, Task,
};
use proptest::prelude::*;

use super::*;
use crate::{Op, SmallState, Store, StoreKind};

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn item(id: u64, folder: u64, read: bool, subject: &str) -> MailItem {
    MailItem {
        id: ItemId(id),
        folder: FolderId(folder),
        read,
        subject: subject.into(),
        message_id: Some(format!("m{id}@example.org")),
        ..Default::default()
    }
}

fn folder(id: u64, name: &str) -> Folder {
    Folder { id: FolderId(id), account: AccountId(1), name: name.into(), ..Default::default() }
}

fn base() -> Mailbox {
    let mut mb = Mailbox::default();
    mb.accounts.push(Account { id: AccountId(1), display_name: "Local".into(), ..Default::default() });
    for (id, name) in [(1, "Inbox"), (2, "Archive"), (3, "Sent")] {
        mb.folders.insert(FolderId(id), folder(id, name));
    }
    mb.next_id = 100;
    mb
}

/// Counts and reference counts as stored equal the ones rebuilt from the rows.
fn assert_counts_consistent(db: &Db) {
    let conn = db.conn();
    let read = |c: &rusqlite::Connection| -> (Vec<(i64, i64, i64)>, Vec<(i64, i64)>) {
        let mut s = c
            .prepare("SELECT folder, total, unread FROM folder_counts WHERE total > 0 ORDER BY folder")
            .unwrap();
        let counts =
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect();
        let mut s = c.prepare("SELECT id, refs FROM bodies ORDER BY id").unwrap();
        let refs = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
        (counts, refs)
    };
    let before = read(&conn);
    conn.execute_batch("SAVEPOINT recount").unwrap();
    rows::recount(&conn).unwrap();
    let after = read(&conn);
    conn.execute_batch("ROLLBACK TO recount; RELEASE recount").unwrap();
    assert_eq!(before, after, "stored counts differ from the rows");
}

#[test]
fn schema_is_created_once_and_checked() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    {
        let conn = db.conn();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        let app: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0)).unwrap();
        let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        let sync: i64 = conn.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
        let fk: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!((v, app, mode.as_str(), sync, fk), (schema::VERSION, schema::APPLICATION_ID, "wal", 2, 1));
    }
    assert_eq!(db.integrity_check().unwrap(), "ok");
    drop(db);
    // Reopening keeps it.
    let db = Db::open(d.path()).unwrap();
    drop(db);
    // A newer schema or another application's database is refused, not touched.
    {
        let c = rusqlite::Connection::open(d.path().join(DB_FILE)).unwrap();
        c.execute_batch("PRAGMA user_version = 99").unwrap();
    }
    let err = Db::open(d.path()).unwrap_err();
    assert!(err.to_string().contains("newer"), "{err}");
    let other = tmp();
    {
        let c = rusqlite::Connection::open(other.path().join(DB_FILE)).unwrap();
        c.execute_batch("PRAGMA application_id = 42; CREATE TABLE t(x)").unwrap();
    }
    assert!(Db::open(other.path()).is_err());
    let foreign = tmp();
    {
        let c = rusqlite::Connection::open(foreign.path().join(DB_FILE)).unwrap();
        c.execute_batch("CREATE TABLE t(x)").unwrap();
    }
    assert!(Db::open(foreign.path()).is_err(), "tables without a MailCraft schema");
    // Not a database at all.
    let junk = tmp();
    std::fs::write(junk.path().join(DB_FILE), b"this is not sqlite").unwrap();
    assert!(Db::open(junk.path()).is_err());
}

#[test]
fn ids_round_trip_through_sql_integers() {
    for id in [0u64, 1, 42, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX - 1, u64::MAX] {
        assert_eq!(from_sql_id(to_sql_id(id)), id);
    }
    assert_eq!(count(-3), 0);
    assert_eq!(sql_len(u64::MAX), i64::MAX);
}

#[test]
fn flags_and_keys_of_an_item() {
    let mut m = item(1, 1, true, "RE: Fwd: Budget  review");
    m.importance = mailcraft_model::Importance::High;
    m.focus = mailcraft_model::FocusClass::Other;
    m.from = Some(mailcraft_mime::Mailbox { name: Some("Ann Lee".into()), addr: "ann@example.com".into() });
    let f = rows::flags_of(&m);
    assert_eq!(f & rows::FLAG_READ, rows::FLAG_READ);
    assert_eq!(f & rows::FLAG_FOCUS_OTHER, rows::FLAG_FOCUS_OTHER);
    assert_eq!((f >> rows::IMPORTANCE_SHIFT) & 3, 2);
    assert_eq!(rows::subject_key(&m), "budget review");
    assert_eq!(rows::sender_key(&m).as_deref(), Some("ann lee"));
}

#[test]
fn store_round_trips_a_mailbox_and_its_ops() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let mut st = SqlStore::new(db.clone());
    assert_eq!(st.load().unwrap(), Mailbox::default());
    let mut mb = base();
    mb.mail.insert(ItemId(5), item(5, 1, false, "hello"));
    mb.mail.insert(ItemId(6), item(6, 2, true, "world"));
    mb.notes.insert(
        ItemId(7),
        Note { id: ItemId(7), folder: FolderId(3), text: "note".into(), ..Default::default() },
    );
    mb.categories.push(Category { name: "Red".into(), ..Default::default() });
    mb.accounts[0].sync.next_op = 9;
    st.write_all(&mb, u64::MAX).unwrap();
    assert_eq!(st.generation(), 1);
    assert_eq!(SqlStore::new(db.clone()).load().unwrap(), mb);
    // Ops, as the journal gets them.
    let ops = vec![
        Op::PutMail(Box::new(item(8, 3, false, "new"))),
        Op::RemoveMail(ItemId(5)),
        Op::PutFolder(Box::new(folder(150, "Lists"))),
        Op::PutSync(AccountId(1), Box::new(AccountSync { next_op: 11, ..Default::default() })),
        Op::PutSync(AccountId(9), Box::new(AccountSync { next_op: 12, ..Default::default() })),
    ];
    for op in ops.clone() {
        op.apply(&mut mb);
    }
    st.append(&ops, u64::MAX).unwrap();
    let loaded = SqlStore::new(db.clone()).load().unwrap();
    assert_eq!(loaded, mb);
    assert_eq!(loaded.next_id, 150);
    assert_counts_consistent(&db);
    // A batch (journal format 2) applies its ops in order, nested ones too, as `Op::apply` does.
    let batch = Op::Batch(vec![
        Op::PutMail(Box::new(item(10, 1, false, "batched"))),
        Op::Batch(vec![
            Op::RemoveMail(ItemId(6)),
            Op::PutSync(AccountId(1), Box::new(AccountSync { next_op: 13, ..Default::default() })),
        ]),
        Op::RemoveMail(ItemId(10)),
        Op::PutMail(Box::new(item(11, 2, true, "kept"))),
    ]);
    batch.clone().apply(&mut mb);
    st.append(&[batch], u64::MAX).unwrap();
    assert_eq!(SqlStore::new(db.clone()).load().unwrap(), mb);
    assert!(mb.mail.get(ItemId(11)).is_some() && mb.mail.get(ItemId(6)).is_none());
    assert_counts_consistent(&db);
    // A cap refuses the batch and writes nothing.
    let err = st.append(&[Op::PutMail(Box::new(item(9, 1, false, "too big")))], 10).unwrap_err();
    assert!(matches!(err, StoreError::TooLarge(_)), "{err}");
    assert_eq!(SqlStore::new(db).load().unwrap(), mb);
}

#[test]
fn replace_rewrites_only_what_changed() {
    let d = tmp();
    let db = Db::open(d.path()).unwrap();
    let mut st = SqlStore::new(db.clone());
    st.load().unwrap();
    let mut mb = base();
    for i in 0..50 {
        mb.mail.insert(ItemId(1000 + i), item(1000 + i, 1 + i % 3, i % 2 == 0, "x"));
    }
    st.append(&[Op::Replace(Box::new(mb.clone()))], u64::MAX).unwrap();
    // One appointment more: the mail rows are not written again.
    mb.appointments.insert(
        ItemId(5),
        Appointment { id: ItemId(5), folder: FolderId(3), subject: "a".into(), ..Default::default() },
    );
    let changes_before: i64 = db.conn().query_row("SELECT total_changes()", [], |r| r.get(0)).unwrap();
    st.append(&[Op::Replace(Box::new(mb.clone()))], u64::MAX).unwrap();
    let changes_after: i64 = db.conn().query_row("SELECT total_changes()", [], |r| r.get(0)).unwrap();
    // Configuration, folders and sync rows are rewritten (about 30 changes); 50 mail rows would add 100 more.
    assert!(changes_after - changes_before < 50, "{} rows changed", changes_after - changes_before);
    assert_eq!(SqlStore::new(db.clone()).load().unwrap(), mb);
    // Items gone from the mailbox go from the database.
    mb.mail.remove(ItemId(1001));
    mb.appointments.clear();
    mb.tasks.insert(
        ItemId(6),
        Task { id: ItemId(6), folder: FolderId(3), subject: "t".into(), ..Default::default() },
    );
    st.append(&[Op::Replace(Box::new(mb.clone()))], u64::MAX).unwrap();
    assert_eq!(SqlStore::new(db.clone()).load().unwrap(), mb);
    assert_counts_consistent(&db);
}

#[test]
fn whole_store_on_a_sqlite_profile() {
    let d = tmp();
    let mut store = Store::open_with(d.path(), StoreKind::Sqlite).unwrap();
    assert_eq!(store.kind(), StoreKind::Sqlite);
    assert!(d.path().join(DB_FILE).is_file());
    assert!(crate::journal::is_sqlite_marker(d.path()).unwrap());
    assert!(!d.path().join(crate::blob::BLOBS_DIR).exists());
    let mut mb = base();
    let raw = b"From: ann@example.com\r\nSubject: hi\r\n\r\nbody\r\n".to_vec();
    let key = store.put_blob(&raw).unwrap();
    assert!(store.has_blob(&key));
    let mut m = item(5, 1, false, "hi");
    m.blob = Some(key.clone());
    assert_eq!(store.raw_of(&m).unwrap().unwrap(), raw);
    assert_eq!(store.head_of(&m, 4).unwrap().unwrap(), b"From");
    mb.mail.insert(m.id, m.clone());
    store.checkpoint(&mb).unwrap();
    store.append(&[Op::PutMail(Box::new(item(6, 2, true, "second")))]).unwrap();
    Op::PutMail(Box::new(item(6, 2, true, "second"))).apply(&mut mb);
    // The source's reference count follows the mail row.
    let db = store.db().unwrap();
    let refs: i64 =
        db.conn().query_row("SELECT refs FROM bodies WHERE key = ?1", [&key], |r| r.get(0)).unwrap();
    assert_eq!(refs, 1);
    drop(db);
    drop(store);
    // Reopened without asking for SQLite: the profile keeps its storage.
    let store = Store::open(d.path()).unwrap();
    assert_eq!(store.kind(), StoreKind::Sqlite);
    assert_eq!(store.load().unwrap(), mb);
    assert_eq!(store.get_blob(&key).unwrap(), raw);
    // Collection keeps what the mailbox refers to.
    drop(store);
    let mut store = Store::open(d.path()).unwrap();
    assert_eq!(store.gc_blobs(&mb, &Default::default()).unwrap(), 0);
    assert!(store.has_blob(&key));
    // A legacy profile stays legacy even when SQLite is asked for.
    let l = tmp();
    let mut legacy = Store::open(l.path()).unwrap();
    legacy.checkpoint(&base()).unwrap();
    drop(legacy);
    assert_eq!(Store::open_with(l.path(), StoreKind::Sqlite).unwrap().kind(), StoreKind::Legacy);
    assert!(!l.path().join(DB_FILE).exists());
}

#[test]
fn store_kind_parses_and_detects() {
    assert_eq!(StoreKind::parse(" SQLite "), Some(StoreKind::Sqlite));
    assert_eq!(StoreKind::parse("legacy"), Some(StoreKind::Legacy));
    assert_eq!(StoreKind::parse("lmdb"), None);
    let d = tmp();
    assert_eq!(StoreKind::of_profile(d.path()).unwrap(), None);
    std::fs::write(d.path().join(crate::journal::JOURNAL_FILE), b"").unwrap();
    assert_eq!(StoreKind::of_profile(d.path()).unwrap(), Some(StoreKind::Legacy));
    // A legacy snapshot next to a database is refused (either one would hide the other).
    let mut legacy = Store::open(d.path()).unwrap();
    legacy.checkpoint(&base()).unwrap();
    drop(legacy);
    std::fs::write(d.path().join(DB_FILE), b"").unwrap();
    let err = Store::open_with(d.path(), StoreKind::Sqlite).unwrap_err();
    assert!(err.to_string().contains("both"), "{err}");
    // So is a legacy journal (its snapshot lost) or a blobs folder with content, and no marker is written.
    for (legacy, what) in [(crate::journal::JOURNAL_FILE, "journal.jsonl"), ("blobs/ab/x.eml", "blobs")] {
        let j = tmp();
        let path = j.path().join(legacy);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{}\n").unwrap();
        std::fs::write(j.path().join(DB_FILE), b"").unwrap();
        let err = Store::open_with(j.path(), StoreKind::Sqlite).unwrap_err();
        assert!(err.to_string().contains("both") && err.to_string().contains(what), "{err}");
        assert!(!j.path().join(crate::journal::SNAPSHOT_FILE).exists(), "no marker hides the {what}");
        assert!(StoreKind::of_profile(j.path()).is_err());
    }
    // An empty blobs folder is not mail: the database opens.
    let e = tmp();
    std::fs::create_dir(e.path().join(crate::blob::BLOBS_DIR)).unwrap();
    std::fs::write(e.path().join(DB_FILE), b"").unwrap();
    assert_eq!(Store::open(e.path()).unwrap().kind(), StoreKind::Sqlite);
    // The marker without its database is refused too, never taken for a new profile.
    let s = tmp();
    drop(Store::open_with(s.path(), StoreKind::Sqlite).unwrap());
    std::fs::remove_file(s.path().join(DB_FILE)).unwrap();
    assert!(Store::open(s.path()).unwrap_err().to_string().contains("missing"));
}

// ---- random op sequences against Op::apply ----

#[derive(Clone, Debug)]
enum Act {
    Put { id: u64, folder: u64, read: bool, subject: u8, conv: Option<u8> },
    Remove(u64),
    PutFolder(u64, u8),
    RemoveFolder(u64),
    PutAccount(u32, u8),
    Sync(u32, u8),
    Categories(u8),
    Accounts(Vec<u32>),
    Views(u64),
    Pim(u8, u64, bool),
    Replace,
    Checkpoint,
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        6 => (1u64..30, 1u64..6, any::<bool>(), any::<u8>(), proptest::option::of(0u8..4))
            .prop_map(|(id, folder, read, subject, conv)| Act::Put { id, folder, read, subject, conv }),
        3 => (1u64..30).prop_map(Act::Remove),
        1 => (1u64..6, any::<u8>()).prop_map(|(id, n)| Act::PutFolder(id, n)),
        1 => (1u64..6).prop_map(Act::RemoveFolder),
        1 => (1u32..4, any::<u8>()).prop_map(|(id, n)| Act::PutAccount(id, n)),
        2 => (1u32..4, any::<u8>()).prop_map(|(id, n)| Act::Sync(id, n)),
        1 => any::<u8>().prop_map(Act::Categories),
        1 => proptest::collection::vec(1u32..4, 0..4).prop_map(Act::Accounts),
        1 => (1u64..6).prop_map(Act::Views),
        2 => (0u8..4, 30u64..40, any::<bool>()).prop_map(|(k, id, on)| Act::Pim(k, id, on)),
        1 => Just(Act::Replace),
        1 => Just(Act::Checkpoint),
    ]
}

/// The op for `a` given the mailbox as it is (whole-mailbox changes are a `Replace` of a changed copy).
fn op_of(a: &Act, mb: &Mailbox) -> Option<Op> {
    Some(match a {
        Act::Put { id, folder, read, subject, conv } => {
            let mut m = item(*id, *folder, *read, &format!("s{subject}"));
            m.conversation = conv.map(u64::from);
            m.remote_id = (subject % 3 == 0).then(|| format!("{subject}"));
            Op::PutMail(Box::new(m))
        }
        Act::Remove(id) => Op::RemoveMail(ItemId(*id)),
        Act::PutFolder(id, n) => Op::PutFolder(Box::new(folder(*id, &format!("f{n}")))),
        Act::RemoveFolder(id) => Op::RemoveFolder(FolderId(*id)),
        Act::PutAccount(id, n) => Op::PutAccount(Box::new(Account {
            id: AccountId(*id),
            display_name: format!("a{n}"),
            sync: AccountSync { next_op: u64::from(*n), ..Default::default() },
            ..Default::default()
        })),
        Act::Sync(id, n) => Op::PutSync(
            AccountId(*id),
            Box::new(AccountSync { next_op: u64::from(*n), ..Default::default() }),
        ),
        Act::Categories(n) => Op::PutConfig(Box::new(SmallState {
            categories: Some(vec![Category { name: format!("c{n}"), ..Default::default() }]),
            ..Default::default()
        })),
        Act::Accounts(ids) => Op::PutConfig(Box::new(SmallState {
            accounts: Some(
                ids.iter()
                    .map(|id| Account {
                        id: AccountId(*id),
                        display_name: format!("acc{id}"),
                        ..Default::default()
                    })
                    .collect(),
            ),
            ..Default::default()
        })),
        Act::Views(f) => {
            let mut views = mb.views.clone();
            views.insert(FolderId(*f), mailcraft_model::ViewSettings::default());
            Op::PutConfig(Box::new(SmallState { views: Some(views), ..Default::default() }))
        }
        Act::Pim(kind, id, on) => {
            let mut next = mb.clone();
            let (id, f) = (ItemId(*id), FolderId(3));
            match (kind, on) {
                (0, true) => {
                    next.appointments.insert(id, Appointment { id, folder: f, ..Default::default() });
                }
                (1, true) => {
                    next.contacts
                        .insert(id, mailcraft_model::Contact { id, folder: f, ..Default::default() });
                }
                (2, true) => {
                    next.tasks.insert(id, Task { id, folder: f, ..Default::default() });
                }
                (_, true) => {
                    next.notes.insert(id, Note { id, folder: f, text: "n".into(), ..Default::default() });
                }
                (_, false) => {
                    next.appointments.remove(&id);
                    next.contacts.remove(&id);
                    next.tasks.remove(&id);
                    next.notes.remove(&id);
                }
            }
            Op::Replace(Box::new(next))
        }
        Act::Replace => {
            let mut next = mb.clone();
            next.mail.retain(|id, _| id.0 % 3 != 0);
            next.next_id = next.next_id.saturating_add(1);
            Op::Replace(Box::new(next))
        }
        Act::Checkpoint => return None,
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn sqlite_store_agrees_with_op_apply(acts in proptest::collection::vec(act(), 1..40), batch in 1usize..5) {
        let d = tmp();
        let db = Db::open(d.path()).unwrap();
        let mut st = SqlStore::new(db.clone());
        st.load().unwrap();
        let mut oracle = base();
        st.write_all(&oracle, u64::MAX).unwrap();
        let mut queued: Vec<Op> = Vec::new();
        for a in &acts {
            match op_of(a, &oracle) {
                Some(op) => {
                    op.clone().apply(&mut oracle);
                    queued.push(op);
                }
                None => {
                    st.append(&queued, u64::MAX).unwrap();
                    queued.clear();
                    st.write_all(&oracle, u64::MAX).unwrap();
                }
            }
            if queued.len() >= batch {
                st.append(&queued, u64::MAX).unwrap();
                queued.clear();
            }
        }
        st.append(&queued, u64::MAX).unwrap();
        let loaded = SqlStore::new(db.clone()).load().unwrap();
        prop_assert_eq!(&loaded, &oracle);
        assert_counts_consistent(&db);
        // The repository over the same rows answers like the mailbox's table.
        let repo = SqlRepo::new(db.clone());
        for f in 0..7u64 {
            prop_assert_eq!(crate::MailRepo::counts(&repo, FolderId(f)), oracle.mail.counts(FolderId(f)));
            prop_assert_eq!(crate::MailRepo::ids_in(&repo, FolderId(f)), oracle.mail.ids_in(FolderId(f)));
        }
        let all: BTreeMap<_, _> = crate::MailRepo::all_counts(&repo);
        prop_assert_eq!(all, oracle.mail.all_counts());
        // And it all survives closing and reopening the database.
        drop(repo);
        drop(st);
        drop(db);
        let db = Db::open(d.path()).unwrap();
        prop_assert_eq!(SqlStore::new(db).load().unwrap(), oracle);
    }
}
