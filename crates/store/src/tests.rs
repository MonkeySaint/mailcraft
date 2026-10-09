//! Store integration tests (filesystem via tempfile).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use mailcraft_model::{Folder, FolderId, FolderRole, ItemId, MailItem, Mailbox, ResponseState};

use crate::journal::{JOURNAL_FILE, SNAPSHOT_FILE};
use crate::lock::LOCK_FILE;
use crate::{Autosave, BlobKey, Op, Store, StoreError, export_mbox, import_maildir, import_mbox};

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn base_mailbox() -> Mailbox {
    let mut mb = Mailbox::default();
    mb.folders.insert(
        FolderId(1),
        Folder { id: FolderId(1), name: "Inbox".into(), role: Some(FolderRole::Inbox), ..Default::default() },
    );
    mb.next_id = 1;
    mb
}

fn mail(id: u64, subject: &str, raw: Option<&[u8]>) -> MailItem {
    MailItem {
        id: ItemId(id),
        folder: FolderId(1),
        subject: subject.into(),
        raw: raw.map(<[u8]>::to_vec),
        ..Default::default()
    }
}

/// The form an item takes after the store moved its raw source to a blob.
fn stored(mut item: MailItem) -> MailItem {
    if let Some(raw) = item.raw.take() {
        item.blob = Some(BlobKey::for_data(&raw).to_string());
    }
    item
}

const RAW1: &[u8] = b"From: a@example.com\r\nSubject: One\r\n\r\nFirst body\r\n";
const RAW2: &[u8] = b"From: b@example.com\r\nSubject: Two\r\n\r\nSecond body\r\n";

#[test]
fn open_creates_layout_and_lock() {
    let d = tmp();
    let dir = d.path().join("profile");
    let store = Store::open(&dir).unwrap();
    assert!(dir.join(LOCK_FILE).is_file());
    assert!(dir.join(JOURNAL_FILE).is_file());
    assert!(dir.join("blobs").is_dir());
    let lock = fs::read_to_string(dir.join(LOCK_FILE)).unwrap();
    assert!(lock.contains(&format!("pid {}", std::process::id())));
    assert_eq!(store.load().unwrap(), Mailbox::default());
    drop(store);
    assert!(!dir.join(LOCK_FILE).exists(), "lock released on drop");
}

#[test]
fn journal_round_trip_moves_raw_to_blobs() {
    let d = tmp();
    let mut mb = base_mailbox();
    let a = mail(10, "One", Some(RAW1));
    let b = mail(11, "Two", Some(RAW2));
    {
        let mut store = Store::open(d.path()).unwrap();
        store
            .append(&[
                Op::PutFolder(Box::new(mb.folders.get(&FolderId(1)).cloned().unwrap())),
                Op::PutMail(Box::new(a.clone())),
                Op::PutMail(Box::new(b.clone())),
                Op::RemoveMail(ItemId(11)),
            ])
            .unwrap();
        let journal = fs::read_to_string(d.path().join(JOURNAL_FILE)).unwrap();
        assert!(!journal.contains("First body"));
        assert_eq!(journal.lines().count(), 5, "header + 4 ops");
    }
    let store = Store::open(d.path()).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.mail.len(), 1);
    let got = loaded.mail.get(&ItemId(10)).cloned().unwrap();
    assert_eq!(got, stored(a));
    assert_eq!(store.get_blob(got.blob.as_deref().unwrap()).unwrap(), RAW1);
    assert_eq!(store.raw_of(&got).unwrap().as_deref(), Some(RAW1));
    assert_eq!(loaded.next_id, 11, "replay keeps next_id ahead of replayed ids");
    mb.next_id = 11;
    assert_eq!(loaded.folders, mb.folders);
}

#[test]
fn checkpoint_snapshot_excludes_raw_and_resets_journal() {
    let d = tmp();
    let mut mb = base_mailbox();
    mb.mail.insert(ItemId(5), mail(5, "One", Some(RAW1)));
    let mut store = Store::open(d.path()).unwrap();
    store.append(&[Op::PutMail(Box::new(mail(5, "One", Some(RAW1))))]).unwrap();
    store.checkpoint(&mb).unwrap();
    assert_eq!(store.generation(), 1);
    let snap = fs::read_to_string(d.path().join(SNAPSHOT_FILE)).unwrap();
    assert!(snap.starts_with("{\"mailcraft_snapshot\":1,\"generation\":1,"));
    assert!(!snap.contains("\"raw\":["), "raw sources stay out of the snapshot");
    let journal = fs::read_to_string(d.path().join(JOURNAL_FILE)).unwrap();
    assert_eq!(journal.lines().count(), 1);
    assert!(mb.mail.get(&ItemId(5)).and_then(|m| m.raw.as_ref()).is_some(), "caller's mailbox untouched");
    drop(store);

    let store = Store::open(d.path()).unwrap();
    let (loaded, report) = store.load_report().unwrap();
    assert_eq!(report.snapshot_generation, 1);
    assert_eq!(report.replayed, 0);
    let mut expect = mb.clone();
    for item in expect.mail.values_mut() {
        *item = stored(item.clone());
    }
    assert_eq!(loaded, expect);
}

#[test]
fn checkpoint_mut_records_blob_keys() {
    let d = tmp();
    let mut mb = base_mailbox();
    mb.mail.insert(ItemId(5), mail(5, "One", Some(RAW1)));
    let mut store = Store::open(d.path()).unwrap();
    store.checkpoint_mut(&mut mb, false).unwrap();
    let item = mb.mail.get(&ItemId(5)).cloned().unwrap();
    assert_eq!(item.raw.as_deref(), Some(RAW1));
    assert_eq!(item.blob, Some(BlobKey::for_data(RAW1).to_string()));
    store.checkpoint_mut(&mut mb, true).unwrap();
    assert_eq!(store.generation(), 2);
    assert!(mb.mail.get(&ItemId(5)).and_then(|m| m.raw.as_ref()).is_none());
    let loaded = store.load().unwrap();
    assert_eq!(loaded, mb);
}

#[test]
fn crash_recovery_truncated_last_line() {
    let d = tmp();
    {
        let mut store = Store::open(d.path()).unwrap();
        for i in 1..=3 {
            store.append(&[Op::PutMail(Box::new(mail(i, &format!("m{i}"), None)))]).unwrap();
        }
    }
    let path = d.path().join(JOURNAL_FILE);
    let len = fs::metadata(&path).unwrap().len();
    let f = OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(len - 15).unwrap();
    drop(f);

    let mut store = Store::open(d.path()).unwrap();
    assert!(store.open_repair().torn_bytes_removed > 0);
    let loaded = store.load().unwrap();
    assert_eq!(loaded.mail.keys().copied().collect::<Vec<_>>(), vec![ItemId(1), ItemId(2)]);
    // Appending after the repair produces a clean journal.
    store.append(&[Op::PutMail(Box::new(mail(4, "m4", None)))]).unwrap();
    let (loaded, report) = store.load_report().unwrap();
    assert_eq!(loaded.mail.len(), 3);
    assert!(!report.torn_tail);
    assert!(report.skipped_lines.is_empty());
}

#[test]
fn torn_tail_seen_by_load_is_ignored() {
    let d = tmp();
    let mut store = Store::open(d.path()).unwrap();
    store.append(&[Op::PutMail(Box::new(mail(1, "a", None)))]).unwrap();
    let mut f = OpenOptions::new().append(true).open(d.path().join(JOURNAL_FILE)).unwrap();
    f.write_all(b"{\"PutMail\":{\"id\":2,\"fold").unwrap();
    let (loaded, report) = store.load_report().unwrap();
    assert!(report.torn_tail);
    assert_eq!(loaded.mail.len(), 1);
}

#[test]
fn complete_tail_missing_newline_is_kept() {
    let d = tmp();
    {
        let mut store = Store::open(d.path()).unwrap();
        store.append(&[Op::PutMail(Box::new(mail(1, "a", None)))]).unwrap();
    }
    let path = d.path().join(JOURNAL_FILE);
    let mut bytes = fs::read(&path).unwrap();
    assert_eq!(bytes.pop(), Some(b'\n'));
    fs::write(&path, &bytes).unwrap();
    let store = Store::open(d.path()).unwrap();
    assert!(store.open_repair().newline_added);
    assert_eq!(store.load().unwrap().mail.len(), 1);
}

#[test]
fn garbled_middle_line_is_skipped() {
    let d = tmp();
    {
        let mut store = Store::open(d.path()).unwrap();
        store.append(&[Op::PutMail(Box::new(mail(1, "a", None)))]).unwrap();
    }
    let mut f = OpenOptions::new().append(true).open(d.path().join(JOURNAL_FILE)).unwrap();
    f.write_all(b"\x00\xffnot json at all\n").unwrap();
    drop(f);
    let mut store = Store::open(d.path()).unwrap();
    store.append(&[Op::PutMail(Box::new(mail(2, "b", None)))]).unwrap();
    let (loaded, report) = store.load_report().unwrap();
    assert_eq!(report.skipped_lines, vec![3]);
    assert_eq!(loaded.mail.len(), 2);
}

#[test]
fn stale_journal_after_crash_mid_checkpoint_is_ignored() {
    let d = tmp();
    let mut mb = base_mailbox();
    let mut store = Store::open(d.path()).unwrap();
    store.append(&[Op::Replace(Box::default())]).unwrap();
    let old_journal = fs::read(d.path().join(JOURNAL_FILE)).unwrap();
    mb.mail.insert(ItemId(7), mail(7, "kept", None));
    store.checkpoint(&mb).unwrap();
    drop(store);
    // Simulate a crash after the snapshot rename but before the journal reset.
    fs::write(d.path().join(JOURNAL_FILE), &old_journal).unwrap();
    let store = Store::open(d.path()).unwrap();
    assert!(store.open_repair().stale_discarded);
    assert_eq!(store.load().unwrap(), mb);
}

#[test]
fn bare_mailbox_snapshot_is_accepted() {
    let d = tmp();
    let mut mb = base_mailbox();
    mb.mail.insert(ItemId(3), mail(3, "legacy", None));
    fs::write(d.path().join(SNAPSHOT_FILE), serde_json::to_vec(&mb).unwrap()).unwrap();
    let store = Store::open(d.path()).unwrap();
    assert_eq!(store.load().unwrap(), mb);
}

#[test]
fn garbled_snapshot_is_corrupt_not_panic() {
    let d = tmp();
    fs::write(d.path().join(SNAPSHOT_FILE), b"{\"mailcraft_snapshot\":1,\"generation\":4,\"mailbox\":{tru")
        .unwrap();
    let store = Store::open(d.path()).unwrap();
    assert_eq!(store.generation(), 4);
    assert!(matches!(store.load(), Err(StoreError::Corrupt(_))));
}

#[test]
fn lock_contention_and_release() {
    let d = tmp();
    let first = Store::open(d.path()).unwrap();
    assert!(matches!(Store::open(d.path()), Err(StoreError::Locked)));
    drop(first);
    let again = Store::open(d.path());
    assert!(again.is_ok());
}

#[test]
fn stale_lock_takeover() {
    let d = tmp();
    let lock = d.path().join(LOCK_FILE);
    let now = crate::time::now_secs();
    // Old timestamp, no pid: stale on every platform.
    fs::write(&lock, format!("mailcraft-lock 1\ntime {}\n", now - 7200)).unwrap();
    drop(Store::open(d.path()).unwrap());
    // Fresh timestamp, no pid: respected.
    fs::write(&lock, format!("mailcraft-lock 1\ntime {now}\n")).unwrap();
    assert!(matches!(Store::open(d.path()), Err(StoreError::Locked)));
    // Dead pid (where liveness can be checked).
    if crate::lock::pid_alive(u32::MAX - 1) == Some(false) {
        fs::write(&lock, format!("mailcraft-lock 1\npid {}\ntime {now}\n", u32::MAX - 1)).unwrap();
        let s = Store::open(d.path()).unwrap();
        let text = fs::read_to_string(&lock).unwrap();
        assert!(text.contains(&format!("pid {}", std::process::id())));
        drop(s);
    }
    // Garbled and fresh: wait; garbled and old: take over.
    fs::write(&lock, b"\x00\x01").unwrap();
    assert!(matches!(Store::open(d.path()), Err(StoreError::Locked)));
    let f = OpenOptions::new().write(true).open(&lock).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(120)).unwrap();
    drop(f);
    assert!(Store::open(d.path()).is_ok());
}

#[test]
fn blob_dedupe_and_verification() {
    let d = tmp();
    let mut store = Store::open(d.path()).unwrap();
    let k1 = store.put_blob(RAW1).unwrap();
    let k2 = store.put_blob(RAW1).unwrap();
    let k3 = store.put_blob(RAW2).unwrap();
    assert_eq!(k1, k2);
    assert_ne!(k1, k3);
    assert!(store.has_blob(&k1));
    let files: usize = fs::read_dir(d.path().join("blobs"))
        .unwrap()
        .map(|fan| fs::read_dir(fan.unwrap().path()).unwrap().count())
        .sum();
    assert_eq!(files, 2);
    assert_eq!(store.get_blob(&k1).unwrap(), RAW1);
    assert_eq!(store.put_blob(b"").map(|k| store.get_blob(&k).ok()).ok().flatten(), Some(Vec::new()));

    // Tampered blob is detected on read and repaired on the next put.
    let path = BlobKey::parse(&k1).unwrap().path(d.path());
    fs::write(&path, b"tampered").unwrap();
    assert!(matches!(store.get_blob(&k1), Err(StoreError::Corrupt(_))));
    assert_eq!(store.put_blob(RAW1).unwrap(), k1);
    assert_eq!(store.get_blob(&k1).unwrap(), RAW1);

    assert!(matches!(store.get_blob("../../etc/passwd"), Err(StoreError::Corrupt(_))));
    let missing = BlobKey::for_data(b"never stored").to_string();
    assert!(matches!(store.get_blob(&missing), Err(StoreError::Io(_))));
    assert!(!store.has_blob("nonsense"));
}

#[test]
fn gc_removes_unreferenced_blobs() {
    let d = tmp();
    let mut store = Store::open(d.path()).unwrap();
    let keep = store.put_blob(RAW1).unwrap();
    let drop_key = store.put_blob(RAW2).unwrap();
    let mut mb = base_mailbox();
    mb.mail.insert(ItemId(2), MailItem { id: ItemId(2), blob: Some(keep.clone()), ..Default::default() });
    assert_eq!(store.gc_blobs(&mb).unwrap(), 1);
    assert!(store.has_blob(&keep));
    assert!(!store.has_blob(&drop_key));
}

#[test]
fn replace_op_strips_raws() {
    let d = tmp();
    let mut mb = base_mailbox();
    mb.mail.insert(ItemId(4), mail(4, "x", Some(RAW2)));
    let mut store = Store::open(d.path()).unwrap();
    store.append(&[Op::Replace(Box::new(mb.clone()))]).unwrap();
    let journal = fs::read_to_string(d.path().join(JOURNAL_FILE)).unwrap();
    assert!(!journal.contains("\"raw\":["));
    let loaded = store.load().unwrap();
    assert_eq!(loaded.mail.get(&ItemId(4)).cloned(), Some(stored(mail(4, "x", Some(RAW2)))));
}

fn write_file(path: &Path, data: &[u8]) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(path, data).unwrap();
}

#[test]
fn maildir_import_with_flags() {
    let d = tmp();
    let md = d.path().join("Maildir");
    write_file(&md.join("new/1791433800.M1P1.host"), b"Subject: new one\n\nhello\n");
    write_file(&md.join("cur/1791433801.M2P1.host:2,S"), b"Subject: seen\n\nx\n");
    write_file(&md.join("cur/1791433802.M3P1.host:2,FRS"), b"Subject: flagged\n\nx\n");
    write_file(&md.join("cur/1791433803.M4P1.host:2,DS"), b"Subject: draft\n\nx\n");
    write_file(&md.join("cur/1791433804.M5P1.host:2,ST"), b"Subject: trashed\n\nx\n");
    write_file(&md.join("cur/.hidden"), b"Subject: hidden\n\nx\n");
    write_file(&md.join("tmp/1791433805.M6P1.host"), b"Subject: in delivery\n\nx\n");
    fs::create_dir_all(md.join("cur/subdir")).unwrap();

    let mut mb = base_mailbox();
    let ids = import_maildir(&mut mb, &md, FolderId(1)).unwrap();
    assert_eq!(ids.len(), 4);
    let items: Vec<&MailItem> = ids.iter().filter_map(|id| mb.mail.get(id)).collect();
    assert_eq!(items.len(), 4);
    let first = items.first().copied().unwrap();
    assert!(!first.read);
    assert_eq!(first.received.unix, 1_791_433_800);
    assert_eq!(first.raw.as_deref(), Some(&b"Subject: new one\n\nhello\n"[..]));
    assert!(items.get(1).unwrap().read);
    let flagged = items.get(2).copied().unwrap();
    assert!(flagged.is_flagged() && flagged.read);
    assert!(matches!(flagged.response, ResponseState::Replied(_)));
    assert!(items.get(3).unwrap().is_draft);
    assert!(items.iter().all(|m| m.folder == FolderId(1) && m.size > 0));
    assert!(items.iter().all(|m| !m.subject.is_empty()), "subject from parser or fallback scan");

    assert!(import_maildir(&mut mb, &d.path().join("missing"), FolderId(1)).is_err());
    fs::create_dir_all(d.path().join("empty")).unwrap();
    assert!(import_maildir(&mut mb, &d.path().join("empty"), FolderId(1)).is_err());
}

#[test]
fn mbox_import_fixture() {
    let d = tmp();
    let file = d.path().join("inbox.mbox");
    let content = b"From alice@example.com Thu Oct  8 04:30:00 2026\n\
Subject: First\n\
Status: RO\n\
\n\
>From the archive\n\
\n\
From bob@example.com Fri Oct  9 10:00:00 2026\n\
Subject: Second\n\
X-Status: F\n\
\n\
body two\n";
    write_file(&file, content);
    let mut mb = base_mailbox();
    let ids = import_mbox(&mut mb, &file, FolderId(1)).unwrap();
    assert_eq!(ids.len(), 2);
    let a = mb.mail.get(ids.first().unwrap()).unwrap();
    assert_eq!(a.raw.as_deref(), Some(&b"Subject: First\nStatus: RO\n\nFrom the archive\n"[..]));
    assert!(a.read);
    assert_eq!(a.received.unix, 1_791_433_800);
    let b = mb.mail.get(ids.get(1).unwrap()).unwrap();
    assert!(!b.read && b.is_flagged());
    assert!(import_mbox(&mut mb, &d.path().join("nope.mbox"), FolderId(1)).is_err());
}

#[test]
fn mbox_export_import_round_trip() {
    let d = tmp();
    let file = d.path().join("out.mbox");
    let raws: [&[u8]; 3] =
        [b"Subject: a\n\nFrom the top\n>From quoted\n", b"Subject: b\n\nplain\n", b"Subject: c\n\nlast\n\n"];
    let items: Vec<MailItem> = raws
        .iter()
        .enumerate()
        .map(|(i, r)| MailItem {
            from: Some(mailcraft_mime::Mailbox::new(Some("A Person"), "a@example.com")),
            received: mailcraft_model::DateTime { unix: 1_791_433_800 + i as i64, offset_min: 0 },
            ..mail(i as u64 + 1, "s", Some(r))
        })
        .collect();
    let refs: Vec<&MailItem> = items.iter().collect();
    export_mbox(&refs, &file).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("From a@example.com Thu Oct  8 04:30:00 2026\n"));
    assert!(text.contains("\n>From the top\n>>From quoted\n"));

    let mut mb = base_mailbox();
    let ids = import_mbox(&mut mb, &file, FolderId(1)).unwrap();
    let back: Vec<Vec<u8>> =
        ids.iter().filter_map(|id| mb.mail.get(id).and_then(|m| m.raw.clone())).collect();
    assert_eq!(back, raws.iter().map(|r| r.to_vec()).collect::<Vec<_>>());
    let received: Vec<i64> = ids.iter().filter_map(|id| mb.mail.get(id)).map(|m| m.received.unix).collect();
    assert_eq!(received, vec![1_791_433_800, 1_791_433_801, 1_791_433_802]);
}

#[test]
fn export_rebuilds_items_without_source() {
    let d = tmp();
    let file = d.path().join("rebuilt.mbox");
    let item = MailItem {
        subject: "Hello there".into(),
        from: Some(mailcraft_mime::Mailbox::new(None, "me@example.com")),
        to: vec![mailcraft_mime::Mailbox::new(Some("You"), "you@example.com")],
        body: mailcraft_model::Body { text: Some("Line one\nFrom line two".into()), ..Default::default() },
        ..Default::default()
    };
    export_mbox(&[&item], &file).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("From me@example.com "));
    assert!(text.contains("Hello there"));
    let mut mb = base_mailbox();
    assert_eq!(import_mbox(&mut mb, &file, FolderId(1)).unwrap().len(), 1);
}

#[test]
fn store_export_reads_blobs() {
    let d = tmp();
    let mut store = Store::open(&d.path().join("s")).unwrap();
    let key = store.put_blob(b"Subject: from blob\n\nbody\n").unwrap();
    let item = MailItem { blob: Some(key), ..Default::default() };
    let file = d.path().join("x.mbox");
    store.export_mbox(&[&item], &file).unwrap();
    assert!(fs::read_to_string(&file).unwrap().contains("Subject: from blob\n\nbody\n"));
}

#[test]
fn import_eml_stores_blob() {
    let d = tmp();
    let eml = d.path().join("msg.eml");
    write_file(&eml, RAW1);
    let mut store = Store::open(&d.path().join("s")).unwrap();
    let mut mb = base_mailbox();
    let id = store.import_eml(&mut mb, &eml, FolderId(1)).unwrap();
    let item = mb.mail.get(&id).cloned().unwrap();
    assert_eq!(item.folder, FolderId(1));
    assert_eq!(item.raw.as_deref(), Some(RAW1));
    assert_eq!(store.get_blob(item.blob.as_deref().unwrap()).unwrap(), RAW1);
    assert!(store.import_eml(&mut mb, &d.path().join("missing.eml"), FolderId(1)).is_err());
}

#[test]
fn autosave_batches_and_checkpoints() {
    let d = tmp();
    let mut store = Store::open(d.path()).unwrap();
    let mut mb = base_mailbox();
    let mut auto = Autosave::new(3);
    for i in 1..=2u64 {
        let item = mail(i, "x", None);
        mb.mail.insert(item.id, item.clone());
        auto.push(Op::PutMail(Box::new(item)));
    }
    assert_eq!(auto.pending(), 2);
    assert!(!auto.commit(&mut store, &mb).unwrap());
    assert_eq!(auto.pending(), 0);
    assert_eq!(auto.since_checkpoint(), 2);
    assert_eq!(store.generation(), 0);

    let item = mail(3, "y", Some(RAW1));
    mb.mail.insert(item.id, item.clone());
    assert!(auto.record(&mut store, &mb, &[Op::PutMail(Box::new(item))]).unwrap());
    assert_eq!(store.generation(), 1);
    assert_eq!(auto.since_checkpoint(), 0);
    assert_eq!(auto.checkpoints(), 1);

    mb.mail.remove(&ItemId(1));
    auto.push(Op::RemoveMail(ItemId(1)));
    auto.finish(&mut store, &mb).unwrap();
    assert_eq!(store.generation(), 2);
    drop(store);

    let store = Store::open(d.path()).unwrap();
    let loaded = store.load().unwrap();
    let mut expect = mb.clone();
    for item in expect.mail.values_mut() {
        *item = stored(item.clone());
    }
    assert_eq!(loaded, expect);
    assert_eq!(Autosave::default().checkpoint_every, Autosave::DEFAULT_CHECKPOINT_EVERY);
}

#[test]
fn op_apply_semantics() {
    let mut mb = base_mailbox();
    Op::PutMail(Box::new(mail(40, "a", None))).apply(&mut mb);
    assert_eq!(mb.next_id, 40);
    Op::PutMail(Box::new(mail(40, "b", None))).apply(&mut mb);
    assert_eq!(mb.mail.get(&ItemId(40)).map(|m| m.subject.as_str()), Some("b"));
    Op::RemoveMail(ItemId(40)).apply(&mut mb);
    Op::RemoveMail(ItemId(40)).apply(&mut mb);
    assert!(mb.mail.is_empty());
    mb.views.insert(FolderId(1), Default::default());
    Op::RemoveFolder(FolderId(1)).apply(&mut mb);
    assert!(mb.folders.is_empty() && mb.views.is_empty());
    Op::Replace(Box::new(base_mailbox())).apply(&mut mb);
    assert_eq!(mb, base_mailbox());
}

#[test]
fn hostile_journal_bytes_never_panic() {
    let d = tmp();
    let samples: Vec<Vec<u8>> = {
        let mut v: Vec<Vec<u8>> = vec![
            b"".to_vec(),
            b"\n\n\n".to_vec(),
            b"{\"mailcraft_journal\":1,\"generation\":18446744073709551615}\n".to_vec(),
            b"{\"mailcraft_journal\":1,\"generation\":-1}\n{\"RemoveMail\":3}\n".to_vec(),
            b"{\"RemoveMail\":3}".to_vec(),
            b"{\"PutMail\":null}\n{\"Replace\":{}}\n".to_vec(),
            vec![0xff; 300],
        ];
        let mut seed = 0x1234_5678_u64;
        for _ in 0..40 {
            let mut buf = Vec::new();
            for _ in 0..200 {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let b = (seed >> 33) as u8;
                buf.push(match b % 8 {
                    0 => b'\n',
                    1 => b'{',
                    2 => b'"',
                    _ => b,
                });
            }
            v.push(buf);
        }
        v
    };
    for (i, bytes) in samples.iter().enumerate() {
        let dir = d.path().join(format!("j{i}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(JOURNAL_FILE), bytes).unwrap();
        if i % 2 == 0 {
            fs::write(dir.join(SNAPSHOT_FILE), bytes).unwrap();
            fs::write(dir.join(LOCK_FILE), bytes).unwrap();
        }
        if let Ok(mut store) = Store::open(&dir) {
            let _ = store.load_report();
            let _ = store.append(&[Op::RemoveMail(ItemId(1))]);
            let _ = store.load();
        }
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(48))]

    #[test]
    fn journal_scan_total(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
        let scan = crate::journal::scan_journal(&data);
        proptest::prop_assert!(scan.ops.len() <= data.len());
        let _ = crate::journal::parse_generation_prefix(&data);
    }

    #[test]
    fn random_ops_survive_reopen(ops in proptest::collection::vec((0u8..4, 1u64..20), 0..30)) {
        let d = tmp();
        let mut mb = base_mailbox();
        let mut journaled = Vec::new();
        for (kind, id) in ops {
            let op = match kind {
                0 => Op::PutMail(Box::new(mail(id, "p", None))),
                1 => Op::PutMail(Box::new(mail(id, "q", Some(RAW1)))),
                2 => Op::RemoveMail(ItemId(id)),
                _ => Op::PutFolder(Box::new(Folder { id: FolderId(id + 100), ..Default::default() })),
            };
            op.clone().apply(&mut mb);
            journaled.push(op);
        }
        {
            let mut store = Store::open(d.path()).unwrap();
            store.checkpoint(&base_mailbox()).unwrap();
            store.append(&journaled).unwrap();
        }
        let store = Store::open(d.path()).unwrap();
        let loaded = store.load().unwrap();
        let mut expect = mb;
        for item in expect.mail.values_mut() {
            *item = stored(item.clone());
        }
        proptest::prop_assert_eq!(loaded, expect);
    }
}

// ---- Review regressions -------------------------------------------------------------------------------------

/// `gc_blobs` must not delete blobs that the journal or snapshot reference for items the caller still holds
/// with `raw` set and `blob` unset (the normal state after `append` / `checkpoint(&mb)`).
#[test]
fn gc_keeps_blobs_of_items_still_holding_raw() {
    let d = tmp();
    let mut mb = base_mailbox();
    let item = mail(9, "kept", Some(RAW1));
    mb.mail.insert(item.id, item.clone());
    {
        let mut store = Store::open(d.path()).unwrap();
        store.append(&[Op::PutMail(Box::new(item))]).unwrap();
        assert_eq!(store.gc_blobs(&mb).unwrap(), 0, "blob is live through the item's raw source");
        store.checkpoint(&mb).unwrap();
        assert_eq!(store.gc_blobs(&mb).unwrap(), 0);
    }
    let store = Store::open(d.path()).unwrap();
    let loaded = store.load().unwrap();
    let got = loaded.mail.get(&ItemId(9)).cloned().unwrap();
    assert_eq!(store.raw_of(&got).unwrap().as_deref(), Some(RAW1));
}

/// A failed journal reset after a successful snapshot write must not leave later appends in a journal that
/// the next open discards as stale.
#[test]
fn appends_survive_failed_journal_reset() {
    let d = tmp();
    let mut mb = base_mailbox();
    let mut store = Store::open(d.path()).unwrap();
    // Block the journal's temp file name so the atomic reset fails (snapshot uses a different temp name).
    let blocker = d.path().join(format!(".{JOURNAL_FILE}.tmp-{}", std::process::id()));
    fs::create_dir_all(&blocker).unwrap();
    mb.mail.insert(ItemId(1), mail(1, "in snapshot", None));
    assert!(store.checkpoint(&mb).is_err(), "the blocked journal reset fails");
    assert_eq!(store.generation(), 1, "the snapshot itself was written");
    fs::remove_dir(&blocker).unwrap();
    let after = mail(2, "after reset failure", None);
    store.append(&[Op::PutMail(Box::new(after.clone()))]).unwrap();
    mb.mail.insert(after.id, after);
    drop(store);
    let store = Store::open(d.path()).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.mail.keys().copied().collect::<Vec<_>>(), vec![ItemId(1), ItemId(2)]);
}

/// A damaged blob that an item still points at is rewritten from the in-memory source before the source is
/// dropped, instead of being trusted because the key matches.
#[test]
fn checkpoint_rewrites_damaged_blob_before_dropping_raw() {
    let d = tmp();
    let mut mb = base_mailbox();
    mb.mail.insert(ItemId(5), mail(5, "One", Some(RAW1)));
    let mut store = Store::open(d.path()).unwrap();
    store.checkpoint_mut(&mut mb, false).unwrap();
    let key = mb.mail.get(&ItemId(5)).and_then(|m| m.blob.clone()).unwrap();
    let path = BlobKey::parse(&key).unwrap().path(d.path());
    fs::write(&path, &RAW1[..10]).unwrap();
    store.checkpoint_mut(&mut mb, true).unwrap();
    assert_eq!(store.get_blob(&key).unwrap(), RAW1);
}

#[cfg(unix)]
#[test]
fn maildir_unreadable_file_is_skipped_not_fatal() {
    use std::os::unix::fs::PermissionsExt;
    let d = tmp();
    let md = d.path().join("Maildir");
    write_file(&md.join("cur/1791433800.M1P1.host:2,S"), b"Subject: ok\n\nx\n");
    let bad = md.join("cur/1791433801.M2P1.host:2,S");
    write_file(&bad, b"Subject: unreadable\n\nx\n");
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&bad).is_ok() {
        return; // running with privileges that ignore file modes
    }
    let mut mb = base_mailbox();
    let ids = import_maildir(&mut mb, &md, FolderId(1)).unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(mb.mail.len(), 1);
}

// ---- Review regressions (second pass) -----------------------------------------------------------------------

/// A lock left behind by an earlier run that happened to have our pid (pid namespaces such as Flatpak or
/// containers hand out the same low pid every run) must not lock the store forever.
#[test]
fn lock_from_earlier_process_with_our_pid_is_taken_over() {
    let d = tmp();
    let boot = crate::lock::current_boot_id().map(|b| format!("boot {b}\n")).unwrap_or_default();
    let now = crate::time::now_secs();
    fs::write(
        d.path().join(LOCK_FILE),
        format!("mailcraft-lock 1\npid {}\ntime {now}\ntoken earlier-run\n{boot}", std::process::id()),
    )
    .unwrap();
    let store = Store::open(d.path()).unwrap();
    // A lock held by a live guard in this process is still respected.
    assert!(matches!(Store::open(d.path()), Err(StoreError::Locked)));
    drop(store);
    assert!(Store::open(d.path()).is_ok());
}

/// An idle holder (no appends) must keep its lock fresh through the autosave tick, or a second instance on a
/// platform without a liveness check takes the lock over after an hour.
#[test]
fn autosave_tick_refreshes_idle_lock() {
    let d = tmp();
    let mut store = Store::open(d.path()).unwrap();
    let path = d.path().join(LOCK_FILE);
    let text = fs::read_to_string(&path).unwrap();
    let old = crate::time::now_secs() - 2 * crate::lock::STALE_AFTER_SECS;
    let backdated: String = text
        .lines()
        .map(|l| if l.starts_with("time ") { format!("time {old}\n") } else { format!("{l}\n") })
        .collect();
    fs::write(&path, backdated).unwrap();
    store.backdate_lock_refresh();
    let mut auto = Autosave::default();
    assert!(!auto.commit(&mut store, &base_mailbox()).unwrap());
    let info = crate::lock::LockInfo::parse(&fs::read_to_string(&path).unwrap());
    assert!(info.time.unwrap() > old, "idle tick rewrote the timestamp");
}

/// A damaged blob file too large to read must be replaced, not block storing that content forever.
#[test]
fn put_blob_replaces_oversized_damaged_file() {
    let d = tmp();
    let mut store = Store::open(d.path()).unwrap();
    let path = BlobKey::for_data(RAW1).path(d.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let f = fs::File::create(&path).unwrap();
    f.set_len(crate::MAX_BLOB_BYTES as u64 + 10).unwrap(); // sparse: cheap
    drop(f);
    let key = store.put_blob(RAW1).unwrap();
    assert_eq!(store.get_blob(&key).unwrap(), RAW1);
}

/// An `.eml` with no Date or Received header takes the file's modification time.
#[test]
fn import_eml_without_date_uses_mtime() {
    let d = tmp();
    let eml = d.path().join("nodate.eml");
    write_file(&eml, b"Subject: no date\r\n\r\nbody\r\n");
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    OpenOptions::new().write(true).open(&eml).unwrap().set_modified(when).unwrap();
    let mut store = Store::open(&d.path().join("s")).unwrap();
    let mut mb = base_mailbox();
    let id = store.import_eml(&mut mb, &eml, FolderId(1)).unwrap();
    assert_eq!(mb.mail.get(&id).map(|m| m.received.unix), Some(1_700_000_000));
    // A dated message keeps its own date.
    write_file(&eml, b"Subject: dated\r\nDate: Thu, 08 Oct 2026 04:30:00 +0000\r\n\r\nbody\r\n");
    let id = store.import_eml(&mut mb, &eml, FolderId(1)).unwrap();
    assert_eq!(mb.mail.get(&id).map(|m| m.received.unix), Some(1_791_433_800));
}

/// The journal never grows past what `open` / `load` will read back.
#[test]
fn append_refuses_to_grow_journal_past_cap() {
    let d = tmp();
    let mut store = Store::open(d.path()).unwrap();
    store.append(&[Op::PutMail(Box::new(mail(1, "fits", None)))]).unwrap();
    store.set_journal_cap(store.journal_bytes() + 200);
    let before = fs::read(d.path().join(JOURNAL_FILE)).unwrap();
    let big = "x".repeat(500);
    assert!(store.append(&[Op::PutMail(Box::new(mail(2, &big, None)))]).is_err());
    assert_eq!(fs::read(d.path().join(JOURNAL_FILE)).unwrap(), before, "journal untouched");
    drop(store);
    let store = Store::open(d.path()).unwrap();
    assert_eq!(store.load().unwrap().mail.len(), 1);
}

/// Autosave folds the journal into a snapshot once it is large, whatever the op count.
#[test]
fn autosave_checkpoints_on_journal_size() {
    let d = tmp();
    let mut store = Store::open(d.path()).unwrap();
    let mut mb = base_mailbox();
    let mut auto = Autosave::new(0);
    auto.checkpoint_bytes = 300;
    let item = mail(1, &"y".repeat(400), None);
    mb.mail.insert(item.id, item.clone());
    assert!(auto.record(&mut store, &mb, &[Op::PutMail(Box::new(item))]).unwrap());
    assert_eq!(store.generation(), 1);
    assert!(store.journal_bytes() < 300);
    assert_eq!(Autosave::default().checkpoint_bytes, Autosave::DEFAULT_CHECKPOINT_BYTES);
}

/// MBOX import stops at the aggregate byte cap instead of holding an unbounded amount of source in memory.
#[test]
fn mbox_import_stops_at_byte_cap() {
    let d = tmp();
    let file = d.path().join("big.mbox");
    let mut content = Vec::new();
    for i in 0..10 {
        content.extend_from_slice(
            format!("From a@x Thu Oct  8 04:30:00 2026\nSubject: m{i}\n\n{}\n\n", "z".repeat(80)).as_bytes(),
        );
    }
    write_file(&file, &content);
    let mut mb = base_mailbox();
    let ids = crate::import::import_mbox_capped(&mut mb, &file, FolderId(1), 300).unwrap();
    assert!(!ids.is_empty() && ids.len() < 10, "{}", ids.len());
    let held: usize = mb.mail.values().filter_map(|m| m.raw.as_ref()).map(Vec::len).sum();
    assert!(held <= 300);
    let ids = import_mbox(&mut mb, &file, FolderId(1)).unwrap();
    assert_eq!(ids.len(), 10);
}

/// Maildir import stops at the aggregate byte cap too.
#[test]
fn maildir_import_stops_at_byte_cap() {
    let d = tmp();
    let md = d.path().join("Maildir");
    for i in 0..10 {
        write_file(&md.join(format!("cur/17914338{i:02}.M{i}.host:2,S")), &[b'a'; 100]);
    }
    let mut mb = base_mailbox();
    let ids = crate::import::import_maildir_capped(&mut mb, &md, FolderId(1), 350).unwrap();
    assert_eq!(ids.len(), 3);
}

/// When the journal cannot take a batch (here: it would pass the cap), `commit` saves the same state as a
/// snapshot instead of failing on every tick.
#[test]
fn autosave_falls_back_to_checkpoint_when_append_is_refused() {
    let d = tmp();
    let mut store = Store::open(d.path()).unwrap();
    store.set_journal_cap(store.journal_bytes() + 50);
    let mut mb = base_mailbox();
    let mut auto = Autosave::new(0);
    let item = mail(1, &"w".repeat(200), None);
    mb.mail.insert(item.id, item.clone());
    assert!(auto.record(&mut store, &mb, &[Op::PutMail(Box::new(item))]).unwrap());
    assert_eq!(auto.pending(), 0);
    assert_eq!(store.generation(), 1);
    drop(store);
    assert_eq!(Store::open(d.path()).unwrap().load().unwrap(), mb);
}
