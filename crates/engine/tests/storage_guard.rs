//! Storage guard rails (issue #25): a profile never becomes unopenable because its snapshot grew past what
//! the store reads back, folder counts are memoised but stay right across commands, undo, redo and sync,
//! and message sources leave memory once the store holds them (sync, import).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::time::Duration;

use mailcraft_engine::model::{AccountId, FolderId, FolderRole, ItemId, Mailbox};
use mailcraft_engine::sample::sample_session;
use mailcraft_engine::{Notice, Session};
use mailcraft_testmail::{Config, TestServers};
use serde_json::{Value, json};

const ALICE: &str = "alice@example.com";
const ALICE_PW: &str = "alice-test-pw";
const WAIT: Duration = Duration::from_secs(30);

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn select(s: &mut Session, ids: &[ItemId]) {
    if let Some(first) = ids.first() {
        s.selection.folder = s.mailbox.mail.get(*first).unwrap().folder;
    }
    s.selection.items = ids.to_vec();
    s.selection.focus = ids.first().copied();
}

/// The memoised counts equal a fresh count, and the folder pane shows them.
fn assert_counts_right(s: &Session, when: &str) {
    let fresh = s.mailbox.folder_counts();
    assert_eq!(*s.all_folder_counts(), fresh, "stale folder counts after {when}");
    for row in mailcraft_engine::queries::folder_rows(s) {
        let Some(id) = row.id else { continue };
        let Some(f) = s.mailbox.folder(id) else { continue };
        if f.kind != mailcraft_engine::model::FolderKind::Mail || f.show_total.is_some() {
            continue;
        }
        let want = s.mailbox.badge(id).unwrap_or_default();
        assert_eq!(row.badge, want, "badge of {} after {when}", f.name);
    }
}

fn message(subject: &str, id: &str, extra: &str) -> Vec<u8> {
    format!(
        "From: Robin <robin@example.org>\r\nTo: {ALICE}\r\nSubject: {subject}\r\n\
         Date: Mon, 5 Oct 2026 10:00:00 +0000\r\nMessage-ID: <{id}@example.org>\r\n{extra}MIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nBody of {subject}.\r\n"
    )
    .into_bytes()
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// Regression test for issue #25: once the mailbox outgrows the snapshot cap, the snapshot is refused (never
/// written past what `Session::open` reads back), the journal keeps every change, the user is told once,
/// and the profile still opens with everything in it.
#[test]
fn a_mailbox_past_the_snapshot_cap_keeps_its_journal_and_still_opens() {
    let dir = tempdir();
    let (inbox, ids) = {
        let mut s = open_legacy(dir.path()).unwrap();
        let inbox = s.selection.folder;
        let snapshot = std::fs::metadata(dir.path().join("mailbox.json")).unwrap().len();
        // Fold the journal on every write, and allow a snapshot only a little larger than the empty one.
        s.checkpoint_bytes = 1;
        s.store.as_mut().unwrap().set_snapshot_cap(snapshot + 2_000);
        let mbox = dir.path().join("in.mbox");
        let mut data = Vec::new();
        for i in 0..20 {
            data.extend_from_slice(b"From robin@example.org Mon Oct  5 10:00:00 2026\n");
            data.extend_from_slice(&message(&format!("Imported {i}"), &format!("imp{i}"), ""));
            data.extend_from_slice(b"\n");
        }
        std::fs::write(&mbox, &data).unwrap();
        let r = run(
            &mut s,
            "file.openExport",
            json!({"action": "importMbox", "path": mbox.to_str().unwrap(), "folder": inbox.0}),
        );
        assert_eq!(r["imported"], 20);
        let ids: Vec<ItemId> = s.mailbox.items_in(inbox).iter().map(|m| m.id).collect();
        let too_large = |s: &Session| {
            s.notices
                .iter()
                .filter(|n| matches!(n, Notice::Error(t) if t == mailcraft_engine::SNAPSHOT_TOO_LARGE))
                .count()
        };
        assert_eq!(too_large(&s), 1, "the user is told: {:?}", s.notices);
        assert!(s.pending_ops.is_empty(), "every change is in the journal");
        // Later changes still reach the journal; the notice is not repeated.
        select(&mut s, &ids[..3]);
        run(&mut s, "mail.markRead", json!({}));
        assert_eq!(too_large(&s), 1);
        assert!(s.pending_ops.is_empty());
        // The snapshot on disk is the old, readable one.
        assert!(std::fs::metadata(dir.path().join("mailbox.json")).unwrap().len() <= snapshot + 2_000);
        (inbox, ids)
    };
    let s = open_legacy(dir.path()).unwrap();
    assert_eq!(s.mailbox.items_in(inbox).len(), 20);
    for (n, id) in ids.iter().enumerate() {
        assert_eq!(s.mailbox.mail.get(*id).unwrap().read, n < 3, "item {n}");
    }
}

/// The memo is recomputed once per change, not per folder per frame, and stays right through every kind
/// of change a command, undo or redo makes.
#[test]
fn folder_counts_are_memoised_and_stay_right_across_commands_undo_and_redo() {
    let mut s = sample_session();
    assert_counts_right(&s, "start");
    let passes = s.folder_count_passes();
    for _ in 0..20 {
        let _ = mailcraft_engine::queries::folder_rows(&s);
    }
    assert!(s.folder_count_passes() <= passes + 1, "one pass for many frames");

    let account = s.mailbox.default_account().unwrap().id;
    let inbox = s.mailbox.role_folder(account, FolderRole::Inbox).unwrap();
    let mut items: Vec<ItemId> = s.mailbox.items_in(inbox).iter().map(|m| m.id).collect();
    items.sort();
    assert!(items.len() >= 3);
    let unread_before = s.folder_counts(inbox).unread;

    select(&mut s, &items[..2]);
    run(&mut s, "mail.markUnread", json!({}));
    assert_counts_right(&s, "mark unread");
    run(&mut s, "mail.markRead", json!({}));
    assert_counts_right(&s, "mark read");
    let after_read = s.folder_counts(inbox);
    run(&mut s, "app.undo", json!({}));
    assert_counts_right(&s, "undo");
    run(&mut s, "app.redo", json!({}));
    assert_counts_right(&s, "redo");
    assert_eq!(s.folder_counts(inbox), after_read);

    let r = run(&mut s, "folder.new", json!({"name": "Counted", "parent": inbox.0}));
    let target = FolderId(r["folder"].as_u64().unwrap());
    assert_counts_right(&s, "new folder");
    select(&mut s, &items[..2]);
    run(&mut s, "mail.move", json!({"folder": target.0}));
    assert_counts_right(&s, "move");
    assert_eq!(s.folder_counts(target).total, 2);
    run(&mut s, "app.undo", json!({}));
    assert_counts_right(&s, "undo move");
    assert_eq!(s.folder_counts(target).total, 0);

    select(&mut s, &items[2..3]);
    run(&mut s, "mail.delete", json!({}));
    assert_counts_right(&s, "delete");
    run(&mut s, "mail.markAllRead", json!({"folder": inbox.0}));
    assert_counts_right(&s, "mark all read");
    assert_eq!(s.folder_counts(inbox).unread, 0);
    run(&mut s, "app.undo", json!({}));
    assert_counts_right(&s, "undo mark all read");
    assert!(s.folder_counts(inbox).unread > 0 || unread_before == 0);

    // A change made directly to the mailbox (tests, tools) is seen once it is announced.
    if let Some(m) = s.mailbox.mail.get_mut(items[0]) {
        m.read = !m.read;
    }
    s.mailbox_changed();
    assert_counts_right(&s, "a direct change");
    // A failed command restores the mailbox and the counts with it.
    assert!(s.run("mail.move", &json!({"ids": [items[1].0], "folder": u64::MAX})).is_err());
    assert_counts_right(&s, "a failed command");
}

fn servers() -> TestServers {
    TestServers::start(Config { users: vec![(ALICE.into(), ALICE_PW.into())], ..Config::default() }).unwrap()
}

fn add_alice(s: &mut Session, srv: &TestServers) -> AccountId {
    let r = run(
        s,
        "accounts.add",
        json!({
            "kind": "imap",
            "address": ALICE,
            "senderName": "Alice Example",
            "incoming": {"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls", "extraRootPem": srv.cert_pem},
            "outgoing": {"host": "127.0.0.1", "port": srv.smtp_port, "security": "starttls", "extraRootPem": srv.cert_pem},
            "password": ALICE_PW,
        }),
    );
    AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
}

fn sync(s: &mut Session) {
    run(s, "sendReceive.all", json!({}));
    assert!(s.wait_for_sync(WAIT), "send/receive timed out: {:?}", s.sync_status());
}

/// Downloaded mail keeps its source in the store only; the folder counts follow what sync brings.
#[test]
fn synced_mail_leaves_its_source_in_the_store_and_counts_follow_sync() {
    let srv = servers();
    let dir = tempdir();
    let mut s = open_legacy(dir.path()).unwrap();
    s.view.auto_send_receive_minutes = 0;
    let acct = add_alice(&mut s, &srv);
    let list = "List-Post: <mailto:team@lists.example.org>\r\n";
    let raws = [message("First", "s1", ""), message("Second", "s2", list), message("Third", "s3", "")];
    srv.seed_message(ALICE, "INBOX", &raws[0], &[]).unwrap();
    srv.seed_message(ALICE, "INBOX", &raws[1], &[]).unwrap();
    srv.seed_message(ALICE, "INBOX", &raws[2], &["\\Seen"]).unwrap();
    sync(&mut s);
    let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
    assert_counts_right(&s, "sync");
    assert_eq!((s.folder_counts(inbox).total, s.folder_counts(inbox).unread), (3, 2));
    let store = s.store.as_ref().unwrap();
    for m in s.mailbox.items_in(inbox) {
        let key = m.blob.as_deref().unwrap_or_else(|| panic!("{} has no blob", m.subject));
        let want = raws.iter().find(|r| String::from_utf8_lossy(r).contains(&m.subject)).unwrap();
        assert_eq!(&store.get_blob(key).unwrap(), want, "{}", m.subject);
    }
    // Features that read the source still find it in the store.
    let second = s.mailbox.items_in(inbox).iter().find(|m| m.subject == "Second").map(|m| m.id).unwrap();
    assert_eq!(
        mailcraft_engine::junk::list_address(&s, s.mailbox.mail.get(second).unwrap()).as_deref(),
        Some("team@lists.example.org")
    );
    select(&mut s, &[second]);
    let src = run(&mut s, "mail.viewSource", json!({}));
    assert!(src["source"].as_str().unwrap().contains("Message-ID: <s2@example.org>"), "{src}");

    run(&mut s, "mail.markRead", json!({}));
    assert_counts_right(&s, "mark read");
    assert_eq!(s.folder_counts(inbox).unread, 1);
    srv.seed_message(ALICE, "INBOX", &message("Fourth", "s4", ""), &[]).unwrap();
    sync(&mut s);
    assert_counts_right(&s, "second sync");
    assert_eq!((s.folder_counts(inbox).total, s.folder_counts(inbox).unread), (4, 2));
    assert!(s.mailbox.items_in(inbox).iter().all(|m| m.blob.is_some()));
    run(&mut s, "app.undo", json!({}));
    assert_counts_right(&s, "undo after sync");
}

/// Imported mail keeps its source in the body store on disk; a session without a store keeps it in its memory
/// body store (it has nowhere else to go).
#[test]
fn imported_mail_leaves_its_source_in_the_store() {
    let dir = tempdir();
    let eml = dir.path().join("one.eml");
    let raw = message("Imported", "i1", "");
    std::fs::write(&eml, &raw).unwrap();
    let mbox = dir.path().join("two.mbox");
    let mut data = Vec::new();
    for i in 0..2 {
        data.extend_from_slice(b"From robin@example.org Mon Oct  5 10:00:00 2026\n");
        data.extend_from_slice(&message(&format!("Boxed {i}"), &format!("b{i}"), ""));
        data.extend_from_slice(b"\n");
    }
    std::fs::write(&mbox, &data).unwrap();
    let profile = dir.path().join("profile");
    let mut s = open_legacy(&profile).unwrap();
    let inbox = s.selection.folder;
    let r = run(
        &mut s,
        "file.openExport",
        json!({"action": "importEml", "path": eml.to_str().unwrap(), "folder": inbox.0}),
    );
    let id = ItemId(r["ids"][0].as_u64().unwrap());
    run(
        &mut s,
        "file.openExport",
        json!({"action": "importMbox", "path": mbox.to_str().unwrap(), "folder": inbox.0}),
    );
    assert_eq!(s.mailbox.items_in(inbox).len(), 3);
    for m in s.mailbox.items_in(inbox) {
        assert!(m.blob.is_some(), "{}", m.subject);
    }
    assert_eq!(
        s.store.as_ref().unwrap().raw_of(s.mailbox.mail.get(id).unwrap()).unwrap().as_deref(),
        Some(&raw[..])
    );
    assert_counts_right(&s, "import");
    // Undo the import, redo it: the sources come back from the store.
    run(&mut s, "app.undo", json!({}));
    assert_counts_right(&s, "undo import");
    run(&mut s, "app.redo", json!({}));
    assert_eq!(
        s.store.as_ref().unwrap().raw_of(s.mailbox.mail.get(id).unwrap()).unwrap().as_deref(),
        Some(&raw[..])
    );

    let mut mem = Session::new(Mailbox::new_local("Local Folders"));
    let inbox = mem.selection.folder;
    let r = run(
        &mut mem,
        "file.openExport",
        json!({"action": "importEml", "path": eml.to_str().unwrap(), "folder": inbox.0}),
    );
    let id = ItemId(r["ids"][0].as_u64().unwrap());
    assert_eq!(
        mem.source(id).unwrap().as_deref(),
        Some(&raw[..]),
        "no store: the source is in the memory body store"
    );
}

/// Imports `raw` as an .eml file into `folder`; returns the new item.
fn import_eml(s: &mut Session, dir: &std::path::Path, name: &str, raw: &[u8], folder: FolderId) -> ItemId {
    let path = dir.join(format!("{name}.eml"));
    std::fs::write(&path, raw).unwrap();
    let r = run(
        s,
        "file.openExport",
        json!({"action": "importEml", "path": path.to_str().unwrap(), "folder": folder.0}),
    );
    ItemId(r["ids"][0].as_u64().unwrap())
}

/// A message with a body of about `kb` KiB (so a few of them make a snapshot measurably larger).
fn large_message(subject: &str, id: &str, kb: usize) -> Vec<u8> {
    let mut raw = message(subject, id, "");
    let body = "lorem ipsum dolor sit amet ".repeat(kb * 1024 / 27 + 1);
    raw.extend_from_slice(body.as_bytes());
    raw.extend_from_slice(b"\r\n");
    raw
}

fn notices_of(s: &Session, text: &str) -> usize {
    s.notices.iter().filter(|n| matches!(n, Notice::Error(t) if t == text)).count()
}

/// Regression test (issue #25 review): conditions and list features that read the message source still see
/// it once the source left memory for the store: header words and automatic replies in rules (Run Rules
/// Now), the expiry date (list strike-through and the search index after a later change), and opaque-signed
/// S/MIME mail is not taken for encrypted.
#[test]
fn source_readers_see_the_stored_source_after_it_left_memory() {
    let dir = tempdir();
    let mut s = open_legacy(&dir.path().join("profile")).unwrap();
    // 2026-10-15: after the offer below expired.
    s.fixed_now = Some(mailcraft_engine::model::DateTime { unix: 1_792_000_000, offset_min: 0 });
    let inbox = s.selection.folder;
    let list_id = "List-Id: Team news <team.lists.example.org>\r\n";
    let list = import_eml(&mut s, dir.path(), "list", &message("Newsletter", "n1", list_id), inbox);
    let auto = "Auto-Submitted: auto-replied\r\n";
    let away = import_eml(&mut s, dir.path(), "away", &message("Away", "a1", auto), inbox);
    let plain = import_eml(&mut s, dir.path(), "plain", &message("Plain", "p1", ""), inbox);
    let expires = "Expires: Mon, 5 Oct 2026 12:00:00 +0000\r\n";
    let offer = import_eml(&mut s, dir.path(), "offer", &message("Offer", "e1", expires), inbox);
    let p7m = |kind: &str, id: &str| {
        format!(
            "From: Robin <robin@example.org>\r\nTo: {ALICE}\r\nSubject: S/MIME {kind}\r\n\
             Date: Mon, 5 Oct 2026 10:00:00 +0000\r\nMessage-ID: <{id}@example.org>\r\nMIME-Version: 1.0\r\n\
             Content-Type: application/pkcs7-mime; smime-type={kind}; name=\"smime.p7m\"\r\n\
             Content-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"smime.p7m\"\r\n\r\n\
             MIAGCSqGSIb3DQEHAqCAMIACAQEx\r\n"
        )
        .into_bytes()
    };
    let signed = import_eml(&mut s, dir.path(), "signed", &p7m("signed-data", "o1"), inbox);
    let sealed = import_eml(&mut s, dir.path(), "sealed", &p7m("enveloped-data", "o2"), inbox);
    for id in [list, away, plain, offer, signed, sealed] {
        let m = s.mailbox.mail.get(id).unwrap();
        assert!(m.blob.is_some(), "{} is in the body store", m.subject);
    }
    // Another command in between: nothing brings the sources back.
    select(&mut s, &[plain]);
    run(&mut s, "mail.markUnread", json!({}));

    // Rules: header words and automatic replies.
    let r = run(&mut s, "folder.new", json!({"name": "Lists", "parent": inbox.0}));
    let lists = FolderId(r["folder"].as_u64().unwrap());
    run(
        &mut s,
        "rules.create",
        json!({"name": "Lists", "conditions": [{"HeaderContains": ["list-id"]}], "actions": [{"MoveTo": lists.0}]}),
    );
    run(
        &mut s,
        "rules.create",
        json!({"name": "Away", "conditions": ["AutomaticReply"], "actions": ["MarkAsRead"]}),
    );
    let r = run(&mut s, "rules.runNow", json!({"folder": inbox.0}));
    assert_eq!(r["applied"], 2, "{r}");
    assert_eq!(s.mailbox.mail.get(list).unwrap().folder, lists, "the List-Id header matched");
    assert!(s.mailbox.mail.get(away).unwrap().read, "the Auto-Submitted header matched");
    assert_eq!(s.mailbox.mail.get(plain).unwrap().folder, inbox);
    assert!(!s.mailbox.mail.get(plain).unwrap().read);

    // Expiry: the list strikes the message through, and search finds it, also after it changed.
    let expired =
        |s: &Session, id| mailcraft_engine::cmd::view::mail::is_expired(s, s.mailbox.mail.get(id).unwrap());
    assert!(expired(&s, offer));
    assert!(!expired(&s, plain));
    s.selection.folder = inbox;
    let found = |s: &Session| {
        let filter = mailcraft_engine::search_index::scope_filter(s);
        let hits = mailcraft_engine::search_index::hits(s, "expires:any", &filter);
        hits.iter().map(|h| ItemId(h.id)).collect::<Vec<_>>()
    };
    assert_eq!(found(&s), vec![offer]);
    select(&mut s, &[offer]);
    run(&mut s, "mail.markRead", json!({}));
    assert_eq!(found(&s), vec![offer], "re-indexed with its expiry date");
    assert!(expired(&s, offer));
    assert!(mailcraft_engine::search_index::item_matches(
        &s,
        s.mailbox.mail.get(offer).unwrap(),
        "expires:any"
    ));

    // S/MIME: opaque-signed mail is readable, so a reply need not be encrypted; enveloped data is encrypted.
    assert!(!mailcraft_engine::smime::is_encrypted(&s, s.mailbox.mail.get(signed).unwrap()), "opaque-signed");
    assert!(mailcraft_engine::smime::is_encrypted(&s, s.mailbox.mail.get(sealed).unwrap()), "enveloped");
}

/// Regression test (issue #25 review): when the journal refuses a batch for its size, the same state is
/// saved as a snapshot (a new generation) and nothing stays queued; the profile reopens with the change.
#[test]
fn a_batch_the_journal_refuses_is_saved_as_a_snapshot() {
    let dir = tempdir();
    let (id, generation) = {
        let mut s = open_legacy(dir.path()).unwrap();
        let inbox = s.selection.folder;
        let id = import_eml(&mut s, dir.path(), "one", &message("One", "j1", ""), inbox);
        let store = s.store.as_mut().unwrap();
        let generation = store.generation();
        // Room for a few bytes only: the PutMail of the next change does not fit.
        store.set_journal_cap(store.journal_bytes() + 10);
        select(&mut s, &[id]);
        run(&mut s, "mail.markRead", json!({}));
        assert!(s.pending_ops.is_empty(), "saved as a snapshot: {} op(s) queued", s.pending_ops.len());
        assert!(s.store.as_ref().unwrap().generation() > generation, "a new snapshot was written");
        assert_eq!(notices_of(&s, mailcraft_engine::UNSAVED_TOO_LARGE), 0);
        (id, generation)
    };
    let s = open_legacy(dir.path()).unwrap();
    assert!(s.mailbox.mail.get(id).unwrap().read, "the change was saved");
    assert!(s.store.as_ref().unwrap().generation() > generation);
}

/// Regression test (issue #25 review): when the journal and the snapshot both refuse, the user is told once
/// however many commands follow, every change stays queued, and all of it is saved once there is room.
#[test]
fn changes_neither_journal_nor_snapshot_can_take_stay_queued_until_there_is_room() {
    let dir = tempdir();
    let ids = {
        let mut s = open_legacy(dir.path()).unwrap();
        let inbox = s.selection.folder;
        let mut ids = Vec::new();
        for i in 0..3 {
            let raw = message(&format!("M{i}"), &format!("q{i}"), "");
            ids.push(import_eml(&mut s, dir.path(), &format!("m{i}"), &raw, inbox));
        }
        let store = s.store.as_mut().unwrap();
        store.set_journal_cap(store.journal_bytes() + 10);
        store.set_snapshot_cap(1);
        select(&mut s, &ids[..1]);
        run(&mut s, "mail.markRead", json!({}));
        select(&mut s, &ids[1..2]);
        run(&mut s, "mail.markRead", json!({}));
        assert_eq!(notices_of(&s, mailcraft_engine::UNSAVED_TOO_LARGE), 1, "told once: {:?}", s.notices);
        assert!(s.pending_ops.len() >= 2, "both changes stay queued");
        // Room again: the next write saves everything queued.
        let store = s.store.as_mut().unwrap();
        store.set_journal_cap(u64::MAX);
        store.set_snapshot_cap(u64::MAX);
        select(&mut s, &ids[2..3]);
        run(&mut s, "mail.markRead", json!({}));
        assert!(s.pending_ops.is_empty());
        assert_eq!(notices_of(&s, mailcraft_engine::UNSAVED_TOO_LARGE), 1);
        ids
    };
    let s = open_legacy(dir.path()).unwrap();
    assert!(ids.iter().all(|id| s.mailbox.mail.get(*id).unwrap().read), "every queued change was saved");
}

/// Regression test (issue #25 review): while both refuse, the snapshot is not retried on every command, but
/// it is once the mailbox shrank (mail deleted for good), and then everything queued is saved.
#[test]
fn a_refused_mailbox_is_saved_once_it_shrinks() {
    let dir = tempdir();
    let (keep, gone) = {
        let mut s = open_legacy(dir.path()).unwrap();
        let inbox = s.selection.folder;
        let mut ids = Vec::new();
        for i in 0..8 {
            let raw = large_message(&format!("Big {i}"), &format!("g{i}"), 8);
            ids.push(import_eml(&mut s, dir.path(), &format!("big{i}"), &raw, inbox));
        }
        // The snapshot of the mailbox as it is now does not fit; one without half its mail does.
        let store = s.store.as_mut().unwrap();
        store.checkpoint(&s.mailbox).unwrap();
        let size = std::fs::metadata(dir.path().join("mailbox.json")).unwrap().len();
        store.set_snapshot_cap(size - 100);
        store.set_journal_cap(store.journal_bytes() + 10);
        let generation = store.generation();
        select(&mut s, &ids[..1]);
        run(&mut s, "mail.markRead", json!({}));
        assert_eq!(notices_of(&s, mailcraft_engine::UNSAVED_TOO_LARGE), 1);
        select(&mut s, &ids[4..]);
        run(&mut s, "mail.delete", json!({}));
        assert!(!s.pending_ops.is_empty(), "moving to Deleted Items makes no room");
        assert_eq!(s.store.as_ref().unwrap().generation(), generation, "no snapshot retried");
        run(&mut s, "folder.emptyDeleted", json!({}));
        assert!(s.pending_ops.is_empty(), "saved once the mailbox shrank");
        assert!(s.store.as_ref().unwrap().generation() > generation);
        assert_eq!(notices_of(&s, mailcraft_engine::UNSAVED_TOO_LARGE), 1);
        (ids[..4].to_vec(), ids[4..].to_vec())
    };
    let s = open_legacy(dir.path()).unwrap();
    assert!(s.mailbox.mail.get(keep[0]).unwrap().read);
    assert!(keep.iter().all(|id| s.mailbox.mail.contains(*id)));
    assert!(gone.iter().all(|id| !s.mailbox.mail.contains(*id)));
}

/// Timings for the folder pane and the undo estimate on a large synthetic mailbox (run by hand:
/// `cargo test -p mailcraft-engine --test storage_guard -- --ignored --nocapture`).
#[test]
#[ignore = "benchmark"]
fn bench_folder_pane_and_undo_estimate() {
    use std::time::Instant;
    let mut s = sample_session();
    let account = s.mailbox.default_account().unwrap().id;
    let inbox = s.mailbox.role_folder(account, FolderRole::Inbox).unwrap();
    let mut folders = vec![inbox];
    for i in 0..30 {
        folders.push(s.mailbox.create_folder(inbox, &format!("Bench {i}")).unwrap());
    }
    let n = std::env::var("BENCH_ITEMS").ok().and_then(|v| v.parse().ok()).unwrap_or(40_000usize);
    for i in 0..n {
        let folder = folders[i % folders.len()];
        s.mailbox.add_mail(mailcraft_engine::model::MailItem {
            folder,
            subject: format!("Bench message {i}"),
            read: i % 5 != 0,
            preview: "x".repeat(200),
            ..Default::default()
        });
    }
    s.mailbox_changed();
    let rows = mailcraft_engine::queries::folder_rows(&s).len();
    let time = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        t.elapsed()
    };
    // The old way: every visible folder scanned all mail for its badge and accessible name.
    let legacy = time(&mut || {
        for f in &folders {
            std::hint::black_box(s.mailbox.badge(*f));
            std::hint::black_box(s.mailbox.folder_accessible_name(*f));
        }
    });
    s.mailbox_changed();
    let cold = time(&mut || {
        std::hint::black_box(mailcraft_engine::queries::folder_rows(&s));
    });
    let warm = time(&mut || {
        for _ in 0..100 {
            std::hint::black_box(mailcraft_engine::queries::folder_rows(&s));
        }
    });
    let mut bytes = 0;
    let estimate = time(&mut || bytes = s.mailbox.approx_bytes());
    let clone = time(&mut || {
        std::hint::black_box(s.mailbox.clone());
    });
    println!(
        "{n} messages, {rows} folder rows: legacy badges {legacy:?} per frame; memo: first frame {cold:?}, \
         later frames {:?}; undo estimate {estimate:?} ({} MiB) vs the clone it sizes {clone:?}; undo keeps {} \
         step(s) of this mailbox",
        warm / 100,
        bytes >> 20,
        (mailcraft_engine::undo::DEFAULT_MAX_BYTES / bytes.max(1))
            .clamp(1, mailcraft_engine::undo::DEFAULT_LIMIT)
    );

    // Sources held in memory after an import into a profile (they used to stay until the next start).
    let dir = tempdir();
    let mbox = dir.path().join("bench.mbox");
    let mut data = Vec::new();
    let count = n / 20;
    for i in 0..count {
        data.extend_from_slice(b"From robin@example.org Mon Oct  5 10:00:00 2026\n");
        data.extend_from_slice(&message(&format!("Bench {i}"), &format!("bench{i}"), ""));
        data.extend_from_slice("z".repeat(20_000).as_bytes());
        data.extend_from_slice(b"\n\n");
    }
    std::fs::write(&mbox, &data).unwrap();
    let mut p = open_legacy(&dir.path().join("profile")).unwrap();
    let inbox = p.selection.folder;
    let t = Instant::now();
    run(
        &mut p,
        "file.openExport",
        json!({"action": "importMbox", "path": mbox.to_str().unwrap(), "folder": inbox.0}),
    );
    let took = t.elapsed();
    let held = p.bodies.cache_stats().bytes;
    println!(
        "import of {count} messages ({} MiB of mbox) took {took:?}; content cached in memory afterwards: {held} bytes",
        data.len() >> 20
    );
}

/// These tests measure the legacy journal and snapshot: their profiles use legacy storage whatever
/// `MAILCRAFT_STORE` says.
fn open_legacy(dir: &std::path::Path) -> Result<Session, mailcraft_engine::CmdError> {
    Session::open_with(dir, mailcraft_store::StoreKind::Legacy)
}
