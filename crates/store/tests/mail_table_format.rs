//! Storage stage S2: `Mailbox.mail` became a `MailTable` instead of a `BTreeMap<ItemId, MailItem>`. Every
//! persisted form (snapshot, journal, and the data-file and browser-storage documents, which serialise the
//! same `Mailbox` and `MailItem`) must stay byte for byte what the map produced, so old profiles open and new
//! ones stay readable by older builds. A mirror of the pre-S2 `Mailbox` with the bare map proves it.
//!
//! Also checks the `MailRepo` trait object reads exactly what the table's own methods read.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use mailcraft_model::{
    Account, AccountSignatures, Address, Appointment, Category, Contact, DateTime, Folder, FolderId,
    FolderRole, ItemId, JunkSettings, MailItem, Mailbox, Note, QuickStep, RecentRecipient, Rule, ScanFilter,
    Signature, Task, ViewSettings,
};
use mailcraft_store::{MailRepo, Op, Store, journal};
use serde::Serialize;

/// `Mailbox` as it was declared before S2, field for field, with the bare map.
#[derive(Serialize)]
struct LegacyMailbox<'a> {
    accounts: &'a Vec<Account>,
    folders: &'a BTreeMap<FolderId, Folder>,
    mail: BTreeMap<ItemId, MailItem>,
    appointments: &'a BTreeMap<ItemId, Appointment>,
    contacts: &'a BTreeMap<ItemId, Contact>,
    tasks: &'a BTreeMap<ItemId, Task>,
    notes: &'a BTreeMap<ItemId, Note>,
    categories: &'a Vec<Category>,
    rules: &'a Vec<Rule>,
    quick_steps: &'a Vec<QuickStep>,
    views: &'a BTreeMap<FolderId, ViewSettings>,
    next_id: u64,
    signatures: &'a Vec<Signature>,
    signature_defaults: &'a Vec<AccountSignatures>,
    recent_recipients: &'a Vec<RecentRecipient>,
    custom_dictionary: &'a Vec<String>,
    junk: &'a JunkSettings,
}

fn legacy(mb: &Mailbox) -> LegacyMailbox<'_> {
    // Exhaustive on purpose: a new `Mailbox` field must be added to the mirror too.
    let Mailbox {
        accounts,
        folders,
        mail,
        appointments,
        contacts,
        tasks,
        notes,
        categories,
        rules,
        quick_steps,
        views,
        next_id,
        signatures,
        signature_defaults,
        recent_recipients,
        custom_dictionary,
        junk,
    } = mb;
    LegacyMailbox {
        accounts,
        folders,
        mail: mail.iter().map(|(k, v)| (k, v.clone())).collect(),
        appointments,
        contacts,
        tasks,
        notes,
        categories,
        rules,
        quick_steps,
        views,
        next_id: *next_id,
        signatures,
        signature_defaults,
        recent_recipients,
        custom_dictionary,
        junk,
    }
}

fn fixture() -> Mailbox {
    let mut mb = Mailbox::new_local("Local Folders");
    let account = mb.accounts[0].id;
    let inbox = mb.role_folder(account, FolderRole::Inbox).unwrap();
    let sent = mb.role_folder(account, FolderRole::SentItems).unwrap();
    for (i, (folder, subject)) in
        [(inbox, "Plain"), (inbox, "Grüße \u{1F4E7} \"quoted\" \\ back\nslash"), (sent, "RE: Plain")]
            .into_iter()
            .enumerate()
    {
        let n = i64::try_from(i).unwrap();
        mb.add_mail(MailItem {
            folder,
            subject: subject.into(),
            message_id: Some(format!("m{i}@example.com")),
            from: Some(Address { name: Some("Ana".into()), addr: "ana@example.com".into() }),
            to: vec![Address { name: None, addr: "bo@example.org".into() }],
            received: DateTime { unix: 1_700_000_000 + n, offset_min: -240 },
            read: i == 1,
            categories: vec!["Blue category".into()],
            preview: "preview".into(),
            pinned: i == 2,
            ..Default::default()
        });
    }
    let id = ItemId(mb.alloc_id());
    let calendar = mb.role_folder(account, FolderRole::Calendar).unwrap();
    mb.appointments
        .insert(id, Appointment { id, folder: calendar, subject: "Standup".into(), ..Default::default() });
    mb.custom_dictionary.push("MailCraft".into());
    mb
}

#[test]
fn mailbox_serialises_byte_for_byte_like_before() {
    for mb in [Mailbox::default(), Mailbox::new_local("x"), fixture()] {
        let new = serde_json::to_vec(&mb).unwrap();
        let old = serde_json::to_vec(&legacy(&mb)).unwrap();
        assert_eq!(new, old);
        assert_eq!(
            serde_json::to_string_pretty(&mb).unwrap(),
            serde_json::to_string_pretty(&legacy(&mb)).unwrap()
        );
        // And what the old layout wrote reads back as the same mailbox.
        let back: Mailbox = serde_json::from_slice(&old).unwrap();
        assert_eq!(back, mb);
    }
}

#[test]
fn snapshot_file_is_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let mb = fixture();
    journal::write_snapshot(dir.path(), 7, &mb).unwrap();
    let written = std::fs::read(dir.path().join(journal::SNAPSHOT_FILE)).unwrap();
    // Since storage stage S5 the envelope marks that items carry no content inline.
    let mut want = b"{\"mailcraft_snapshot\":2,\"generation\":7,\"bodies\":2,\"mailbox\":".to_vec();
    want.extend(serde_json::to_vec(&legacy(&mb)).unwrap());
    want.push(b'}');
    assert_eq!(written, want);
    let (generation, back) = journal::read_snapshot(dir.path()).unwrap();
    assert_eq!((generation, back), (7, mb));
}

#[test]
fn journal_lines_are_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let mb = fixture();
    let first = mb.mail.items().next().cloned().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(&[Op::PutMail(Box::new(first.clone()))]).unwrap();
    store.append(&[Op::Replace(Box::new(mb.clone()))]).unwrap();
    // Two ops appended together share a line (storage S4 review: a batch replays whole or not at all).
    let both = [Op::PutMail(Box::new(first.clone())), Op::Replace(Box::new(mb.clone()))];
    store.append(&both).unwrap();
    drop(store);
    let text = std::fs::read_to_string(dir.path().join(journal::JOURNAL_FILE)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "header, two ops, one batch");
    let put = format!("{{\"PutMail\":{}}}", serde_json::to_string(&first).unwrap());
    let replace = format!("{{\"Replace\":{}}}", serde_json::to_string(&legacy(&mb)).unwrap());
    assert_eq!(lines[1], put);
    assert_eq!(lines[2], replace);
    assert_eq!(lines[3], format!("{{\"Batch\":[{put},{replace}]}}"));
    assert_eq!(lines[3], serde_json::to_string(&Op::Batch(both.to_vec())).unwrap());
    let back = Store::open(dir.path()).unwrap().load().unwrap();
    assert_eq!(back.mail, mb.mail);
}

#[test]
fn repo_trait_object_reads_what_the_table_reads() {
    let mb = fixture();
    let t = &mb.mail;
    let repo: &dyn MailRepo = t;
    let account = mb.accounts[0].id;
    let inbox = mb.role_folder(account, FolderRole::Inbox).unwrap();
    assert_eq!(repo.len(), t.len());
    assert!(!repo.is_empty());
    for (id, m) in t {
        assert!(repo.contains(id));
        assert_eq!(repo.get(id).unwrap(), *m);
    }
    assert!(repo.get(ItemId(u64::MAX)).is_none() && !repo.contains(ItemId(u64::MAX)));
    assert_eq!(repo.counts(inbox), t.counts(inbox));
    assert_eq!(repo.counts(inbox).total, 2);
    assert_eq!(repo.all_counts(), t.all_counts());
    assert_eq!(repo.ids_in(inbox), t.ids_in(inbox));
    assert_eq!(repo.by_message_id("m1@example.com"), t.by_message_id("m1@example.com"));
    assert_eq!(repo.by_message_id("m1@example.com").len(), 1);
    // The trait's key is normalised (brackets, space, ASCII case), like the table's.
    assert_eq!(repo.by_message_id(" <M1@Example.COM> "), t.by_message_id("m1@example.com"));
    assert!(repo.by_message_id("<>").is_empty());
    assert_eq!(repo.by_remote_id(inbox, "1"), None);
    assert_eq!(repo.older_than(inbox, i64::MAX, 1), t.older_than(inbox, i64::MAX, 1));
    let mut seen = Vec::new();
    repo.scan(&ScanFilter::Folder(inbox), &mut |m| {
        seen.push(m.id);
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(seen, t.ids_in(inbox));
    let empty = mailcraft_model::MailTable::new();
    let repo: &dyn MailRepo = &empty;
    assert!(repo.is_empty() && repo.all_counts().is_empty() && repo.ids_in(inbox).is_empty());
}

/// Storage stage S3: the trait's writes land in change records, which commit to the before-images of what
/// they wrote and roll back to the state at `begin`; the stored form never carries a record.
#[test]
fn repo_trait_object_writes_into_change_records() {
    let mut mb = fixture();
    let account = mb.accounts[0].id;
    let inbox = mb.role_folder(account, FolderRole::Inbox).unwrap();
    let sent = mb.role_folder(account, FolderRole::SentItems).unwrap();
    let start = mb.mail.clone();
    let ids = mb.mail.ids_in(inbox);
    let repo: &mut dyn MailRepo = &mut mb.mail;
    let token = repo.begin();
    assert_eq!(repo.set_read(&ids, true).unwrap().len(), 1, "one of the two was unread");
    assert_eq!(repo.move_to(&ids, sent).unwrap(), ids);
    assert!(repo.move_to(&ids, sent).unwrap().is_empty(), "already there");
    let new = MailItem { id: ItemId(900), folder: inbox, subject: "new".into(), ..Default::default() };
    assert!(repo.insert(new).unwrap().is_none());
    assert!(repo.update(ItemId(900), &mut |m| m.read = true).unwrap());
    assert!(!repo.update(ItemId(901), &mut |m| m.read = true).unwrap());
    let gone = repo.remove(&[ids[0], ItemId(902)]).unwrap();
    assert_eq!(gone.len(), 1);
    assert_eq!(repo.get(ids[1]).unwrap().folder, sent);
    let changes = repo.commit(token).unwrap();
    assert_eq!(changes.ids().collect::<Vec<_>>(), vec![ids[0], ids[1], ItemId(900)]);
    assert_eq!(changes.before(ItemId(900)), Some(None));
    assert_eq!(changes.before(ids[0]).flatten(), start.get(ids[0]));
    assert!(matches!(repo.commit(token), Err(mailcraft_store::StoreError::NoChangeRecord)));
    // A rollback puts back what was written since its begin.
    let after = mb.mail.clone();
    let repo: &mut dyn MailRepo = &mut mb.mail;
    let token = repo.begin();
    repo.remove(&ids).unwrap();
    repo.set_read(&[ItemId(900)], false).unwrap();
    let undone = repo.rollback(token).unwrap();
    assert_eq!(undone.len(), 2, "ids[1] and 900 (ids[0] went in the first record)");
    assert_eq!(mb.mail, after);
    assert!(mb.mail.rollback(token).is_none());
    // An open record changes nothing in the stored forms.
    let open = mb.mail.begin();
    let journal = serde_json::to_string(&Op::Replace(Box::new(mb.clone()))).unwrap();
    assert_eq!(journal, format!("{{\"Replace\":{}}}", serde_json::to_string(&legacy(&mb)).unwrap()));
    assert!(mb.mail.commit(open).is_some_and(|c| c.is_empty()));
}
