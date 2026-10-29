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
    let mut want = b"{\"mailcraft_snapshot\":1,\"generation\":7,\"mailbox\":".to_vec();
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
    store.append(&[Op::PutMail(Box::new(first.clone())), Op::Replace(Box::new(mb.clone()))]).unwrap();
    drop(store);
    let text = std::fs::read_to_string(dir.path().join(journal::JOURNAL_FILE)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "header and two ops");
    assert_eq!(lines[1], format!("{{\"PutMail\":{}}}", serde_json::to_string(&first).unwrap()));
    assert_eq!(lines[2], format!("{{\"Replace\":{}}}", serde_json::to_string(&legacy(&mb)).unwrap()));
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
        assert_eq!(repo.body(id).unwrap().unwrap(), m.body);
    }
    assert!(repo.get(ItemId(u64::MAX)).is_none() && !repo.contains(ItemId(u64::MAX)));
    assert!(repo.body(ItemId(u64::MAX)).unwrap().is_none());
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
