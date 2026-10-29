use std::collections::BTreeSet;

use proptest::prelude::*;

use super::mail;
use crate::{
    Account, AccountId, AccountKind, FolderId, FolderKind, FolderRole, ItemId, MailItem, Mailbox, ModelError,
    Rule, RuleAction,
};

fn mb() -> (Mailbox, AccountId) {
    let m = Mailbox::new_local("Personal Folders");
    let a = m.accounts[0].id;
    (m, a)
}

fn role(m: &Mailbox, a: AccountId, r: FolderRole) -> FolderId {
    m.role_folder(a, r).expect("role folder")
}

fn names(m: &Mailbox, ids: &[FolderId]) -> Vec<String> {
    ids.iter().map(|id| m.folder(*id).map(|f| f.name.clone()).unwrap_or_default()).collect()
}

fn add(m: &mut Mailbox, folder: FolderId, read: bool) -> ItemId {
    m.add_mail(MailItem { folder, read, ..mail("x") })
}

#[test]
fn new_local_account() {
    let (m, a) = mb();
    assert_eq!(m.accounts.len(), 1);
    let acct = &m.accounts[0];
    assert_eq!(acct.display_name, "Personal Folders");
    assert_eq!(acct.kind, AccountKind::Local);
    assert!(acct.is_default);
    assert!(!acct.can_send());
    assert_eq!(m.default_account().map(|x| x.id), Some(a));
    assert_eq!(m.categories.len(), 6);
    assert_eq!(m.quick_steps.len(), 5);
    assert!(m.validate().is_empty(), "{:?}", m.validate());
}

#[test]
fn empty_display_name_falls_back() {
    let m = Mailbox::new_local("   ");
    assert_eq!(m.accounts[0].display_name, "Local Folders");
}

#[test]
fn local_account_has_no_sender_name() {
    let m = Mailbox::new_local("Local Folders");
    assert_eq!(m.accounts[0].sender_name, "");
}

#[test]
fn legacy_local_account_name_is_renamed() {
    use crate::mailbox::{LEGACY_LOCAL_ACCOUNT_NAME, LOCAL_ACCOUNT_FALLBACK_NAME};
    let mut m = Mailbox::new_local(LEGACY_LOCAL_ACCOUNT_NAME);
    m.accounts[0].sender_name = LEGACY_LOCAL_ACCOUNT_NAME.to_owned();
    assert_eq!(m.rename_legacy_local_account(), 1);
    assert_eq!(m.accounts[0].display_name, LOCAL_ACCOUNT_FALLBACK_NAME);
    assert_eq!(m.accounts[0].sender_name, "");
    // A second run changes nothing, and other names are left alone.
    assert_eq!(m.rename_legacy_local_account(), 0);
    let mut other = Mailbox::new_local("Archive 2024");
    assert_eq!(other.rename_legacy_local_account(), 0);
    assert_eq!(other.accounts[0].display_name, "Archive 2024");
}

#[test]
fn default_folder_order_and_kinds() {
    let (m, a) = mb();
    let roots = m.children(a, None);
    assert_eq!(
        names(&m, &roots),
        [
            "Inbox",
            "Drafts",
            "Sent Items",
            "Deleted Items",
            "Archive",
            "Conversation History",
            "Junk Email",
            "Outbox",
            "RSS Feeds",
            "Search Folders",
            "Calendar",
            "Contacts",
            "Journal",
            "Notes",
            "Tasks",
        ]
    );
    assert_eq!(m.folder(role(&m, a, FolderRole::Calendar)).unwrap().kind, FolderKind::Calendar);
    assert_eq!(m.folder(role(&m, a, FolderRole::Contacts)).unwrap().kind, FolderKind::Contacts);
    assert_eq!(m.folder(role(&m, a, FolderRole::Notes)).unwrap().kind, FolderKind::Notes);
    assert_eq!(m.folder(role(&m, a, FolderRole::Tasks)).unwrap().kind, FolderKind::Tasks);
    assert_eq!(m.folder(role(&m, a, FolderRole::Journal)).unwrap().kind, FolderKind::Journal);
    assert_eq!(m.folder(role(&m, a, FolderRole::SearchFolders)).unwrap().kind, FolderKind::SearchRoot);
    assert_eq!(m.folder(role(&m, a, FolderRole::Inbox)).unwrap().kind, FolderKind::Mail);
    assert!(m.folder(role(&m, a, FolderRole::Inbox)).unwrap().expanded);
}

#[test]
fn user_folders_sort_between_rss_and_search_folders() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let root_parent = m.folder(inbox).unwrap().parent;
    assert_eq!(root_parent, None);
    // User folders at root level: insert directly (the UI creates them under the account root).
    for name in ["trash", "Sent", "Archive 2"] {
        let id = FolderId(m.alloc_id());
        m.folders.insert(id, crate::Folder { id, account: a, name: name.into(), ..Default::default() });
    }
    let roots = names(&m, &m.children(a, None));
    let rss = roots.iter().position(|n| n == "RSS Feeds").unwrap();
    assert_eq!(&roots[rss + 1..rss + 4], ["Archive 2", "Sent", "trash"]);
    assert_eq!(roots[rss + 4], "Search Folders");
}

#[test]
fn subfolders_alphabetical_then_manual() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let z = m.create_folder(inbox, "zeta").unwrap();
    let _b = m.create_folder(inbox, "Beta").unwrap();
    let _al = m.create_folder(inbox, "alpha").unwrap();
    assert_eq!(names(&m, &m.children(a, Some(inbox))), ["alpha", "Beta", "zeta"]);
    m.folders.get_mut(&z).unwrap().manual_pos = Some(0);
    assert_eq!(names(&m, &m.children(a, Some(inbox))), ["zeta", "alpha", "Beta"]);
}

#[test]
fn create_default_folders_is_idempotent() {
    let (mut m, a) = mb();
    let before = m.folders.len();
    let ids = m.create_default_folders(a);
    assert_eq!(ids.len(), FolderRole::DEFAULTS.len());
    assert_eq!(m.folders.len(), before);
    assert_eq!(ids[0], role(&m, a, FolderRole::Inbox));
}

#[test]
fn second_account_gets_own_folders() {
    let (mut m, a) = mb();
    let b = m.add_account(Account {
        kind: AccountKind::Imap,
        display_name: "work@example.com".into(),
        address: "work@example.com".into(),
        is_default: true,
        ..Default::default()
    });
    assert_ne!(a, b);
    assert!(m.account(b).unwrap().is_default);
    assert!(!m.account(a).unwrap().is_default);
    let ids = m.create_default_folders(b);
    assert_eq!(ids.len(), 15);
    assert_ne!(m.role_folder(a, FolderRole::Inbox), m.role_folder(b, FolderRole::Inbox));
    assert_eq!(m.children(b, None).len(), 15);
    assert!(m.validate().is_empty());
}

#[test]
fn favorites_default_order_and_editing() {
    let (mut m, a) = mb();
    assert_eq!(names(&m, &m.favorites()), ["Inbox", "Sent Items", "Drafts", "Deleted Items"]);
    let junk = role(&m, a, FolderRole::JunkEmail);
    m.add_favorite(junk).unwrap();
    m.add_favorite(junk).unwrap();
    assert_eq!(names(&m, &m.favorites()), ["Inbox", "Sent Items", "Drafts", "Deleted Items", "Junk Email"]);
    m.move_favorite(junk, -2).unwrap();
    assert_eq!(names(&m, &m.favorites()), ["Inbox", "Sent Items", "Junk Email", "Drafts", "Deleted Items"]);
    m.move_favorite(junk, -100).unwrap();
    assert_eq!(names(&m, &m.favorites())[0], "Junk Email");
    m.move_favorite(junk, 100).unwrap();
    assert_eq!(names(&m, &m.favorites())[4], "Junk Email");
    m.remove_favorite(role(&m, a, FolderRole::SentItems)).unwrap();
    assert_eq!(names(&m, &m.favorites()), ["Inbox", "Drafts", "Deleted Items", "Junk Email"]);
    let positions: Vec<u32> =
        m.favorites().iter().map(|f| m.folder(*f).unwrap().favorite_pos.unwrap()).collect();
    assert_eq!(positions, [0, 1, 2, 3]);
    assert_eq!(m.add_favorite(FolderId(9999)), Err(ModelError::NoFolder(FolderId(9999))));
    assert!(m.move_favorite(role(&m, a, FolderRole::Outbox), 1).is_err());
}

#[test]
fn create_folder_of_kind_picks_what_it_contains() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let cal = m.create_folder_of_kind(inbox, "Rota", Some(FolderKind::Calendar)).unwrap();
    assert_eq!(m.folder(cal).unwrap().kind, FolderKind::Calendar);
    let sub = m.create_folder_of_kind(cal, "Sub", None).unwrap();
    assert_eq!(m.folder(sub).unwrap().kind, FolderKind::Calendar, "inherits without a kind");
    let notes = m.create_folder_of_kind(cal, "Jots", Some(FolderKind::Notes)).unwrap();
    assert_eq!(m.folder(notes).unwrap().kind, FolderKind::Notes);
    // Search folders keep their own rules.
    assert!(m.create_folder_of_kind(inbox, "S", Some(FolderKind::Search)).is_err());
    assert!(m.create_folder_of_kind(inbox, "R", Some(FolderKind::SearchRoot)).is_err());
}

#[test]
fn create_folder_validation() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let p = m.create_folder(inbox, "  Projects  ").unwrap();
    assert_eq!(m.folder(p).unwrap().name, "Projects");
    assert_eq!(m.folder(p).unwrap().parent, Some(inbox));
    assert_eq!(m.folder(p).unwrap().account, a);
    assert!(!m.folder(p).unwrap().is_default());
    assert!(matches!(m.create_folder(inbox, "projects"), Err(ModelError::Invalid(_))));
    assert!(matches!(m.create_folder(inbox, ""), Err(ModelError::Invalid(_))));
    assert!(matches!(m.create_folder(inbox, "   "), Err(ModelError::Invalid(_))));
    assert!(matches!(m.create_folder(inbox, "a\\b"), Err(ModelError::Invalid(_))));
    assert!(matches!(m.create_folder(inbox, "a\nb"), Err(ModelError::Invalid(_))));
    assert!(matches!(m.create_folder(inbox, &"x".repeat(256)), Err(ModelError::Invalid(_))));
    assert!(m.create_folder(inbox, &"x".repeat(255)).is_ok());
    assert_eq!(m.create_folder(FolderId(424242), "x"), Err(ModelError::NoFolder(FolderId(424242))));
    // Same name elsewhere is fine.
    assert!(m.create_folder(p, "Projects").is_ok());
}

#[test]
fn folder_kinds_inherit() {
    let (mut m, a) = mb();
    let cal = m.create_folder(role(&m, a, FolderRole::Calendar), "Holidays").unwrap();
    assert_eq!(m.folder(cal).unwrap().kind, FolderKind::Calendar);
    let sf = m.create_folder(role(&m, a, FolderRole::SearchFolders), "Unread Mail").unwrap();
    assert_eq!(m.folder(sf).unwrap().kind, FolderKind::Search);
    assert!(matches!(m.create_folder(sf, "nested"), Err(ModelError::NotAllowed(_))));
}

#[test]
fn rename_folder_rules() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let p = m.create_folder(inbox, "Projects").unwrap();
    let _q = m.create_folder(inbox, "Queue").unwrap();
    m.rename_folder(p, "projects").unwrap(); // case change of itself is fine
    assert_eq!(m.folder(p).unwrap().name, "projects");
    assert!(matches!(m.rename_folder(p, "QUEUE"), Err(ModelError::Invalid(_))));
    assert!(matches!(m.rename_folder(p, ""), Err(ModelError::Invalid(_))));
    assert!(matches!(m.rename_folder(inbox, "In"), Err(ModelError::NotAllowed(_))));
    assert_eq!(m.rename_folder(FolderId(999_999), "x"), Err(ModelError::NoFolder(FolderId(999_999))));
}

#[test]
fn move_folder_rules() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let archive = role(&m, a, FolderRole::Archive);
    let p = m.create_folder(inbox, "P").unwrap();
    let c = m.create_folder(p, "C").unwrap();
    let g = m.create_folder(c, "G").unwrap();
    assert!(matches!(m.move_folder(p, g), Err(ModelError::NotAllowed(_))));
    assert!(matches!(m.move_folder(p, p), Err(ModelError::NotAllowed(_))));
    assert!(matches!(m.move_folder(inbox, archive), Err(ModelError::NotAllowed(_))));
    m.move_folder(c, archive).unwrap();
    assert_eq!(m.folder_path(g), "Archive\\C\\G");
    m.move_folder(c, archive).unwrap(); // no-op
    let _dup = m.create_folder(inbox, "C").unwrap();
    assert!(matches!(m.move_folder(c, inbox), Err(ModelError::Invalid(_))));
    let sf = m.create_folder(role(&m, a, FolderRole::SearchFolders), "S").unwrap();
    assert!(matches!(m.move_folder(sf, inbox), Err(ModelError::NotAllowed(_))));
    assert!(matches!(
        m.move_folder(p, role(&m, a, FolderRole::SearchFolders)),
        Err(ModelError::NotAllowed(_))
    ));
    assert!(m.validate().is_empty());
}

#[test]
fn move_folder_across_accounts_updates_subtree() {
    let (mut m, a) = mb();
    let b = m.add_account(Account { display_name: "B".into(), ..Default::default() });
    m.create_default_folders(b);
    let p = m.create_folder(role(&m, a, FolderRole::Inbox), "P").unwrap();
    let c = m.create_folder(p, "C").unwrap();
    m.move_folder(p, role(&m, b, FolderRole::Inbox)).unwrap();
    assert_eq!(m.folder(p).unwrap().account, b);
    assert_eq!(m.folder(c).unwrap().account, b);
    assert!(m.validate().is_empty());
}

#[test]
fn delete_folder_moves_then_purges() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let deleted = role(&m, a, FolderRole::DeletedItems);
    let p = m.create_folder(inbox, "Projects").unwrap();
    let c = m.create_folder(p, "Child").unwrap();
    let item = add(&mut m, c, false);
    m.add_favorite(c).unwrap();
    assert!(matches!(m.delete_folder(inbox), Err(ModelError::NotAllowed(_))));
    assert!(matches!(m.delete_folder(deleted), Err(ModelError::NotAllowed(_))));

    m.delete_folder(p).unwrap();
    assert_eq!(m.folder(p).unwrap().parent, Some(deleted));
    assert_eq!(m.folder_path(c), "Deleted Items\\Projects\\Child");
    assert!(m.mail.contains(item));
    assert!(m.folder(c).unwrap().favorite_pos.is_none());
    assert_eq!(m.favorites().len(), 4);

    // A second "Projects" deleted gets a numbered name.
    let p2 = m.create_folder(inbox, "Projects").unwrap();
    m.delete_folder(p2).unwrap();
    assert_eq!(m.folder(p2).unwrap().name, "Projects (2)");

    // Deleting inside Deleted Items is permanent, items included.
    m.delete_folder(p).unwrap();
    assert!(m.folder(p).is_none());
    assert!(m.folder(c).is_none());
    assert!(!m.mail.contains(item));
    assert!(m.validate().is_empty());
    assert_eq!(m.delete_folder(p), Err(ModelError::NoFolder(p)));
}

#[test]
fn delete_search_folder_is_permanent_and_rules_disabled() {
    let (mut m, a) = mb();
    let sf = m.create_folder(role(&m, a, FolderRole::SearchFolders), "Big mail").unwrap();
    m.delete_folder(sf).unwrap();
    assert!(m.folder(sf).is_none());

    let deleted = role(&m, a, FolderRole::DeletedItems);
    let target = m.create_folder(deleted, "Old").unwrap();
    m.rules.push(Rule { enabled: true, actions: vec![RuleAction::MoveTo(target)], ..Default::default() });
    m.rules.push(Rule { enabled: true, actions: vec![RuleAction::MarkAsRead], ..Default::default() });
    m.delete_folder(target).unwrap();
    assert!(!m.rules[0].enabled);
    assert!(m.rules[1].enabled);
}

#[test]
fn badges() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let drafts = role(&m, a, FolderRole::Drafts);
    let junk = role(&m, a, FolderRole::JunkEmail);
    let outbox = role(&m, a, FolderRole::Outbox);
    assert_eq!(m.badge(inbox), None);
    assert_eq!(m.badge(drafts), None);
    add(&mut m, inbox, true);
    assert_eq!(m.badge(inbox), None);
    for _ in 0..3 {
        add(&mut m, inbox, false);
    }
    assert_eq!(m.badge(inbox).as_deref(), Some("3"));
    for _ in 0..8 {
        add(&mut m, drafts, true);
    }
    assert_eq!(m.badge(drafts).as_deref(), Some("[8]"));
    add(&mut m, junk, true);
    assert_eq!(m.badge(junk).as_deref(), Some("[1]"));
    add(&mut m, outbox, false);
    assert_eq!(m.badge(outbox).as_deref(), Some("[1]"));
    assert_eq!(m.badge(FolderId(123_456)), None);
    assert_eq!(m.folder_accessible_name(inbox), "Inbox: 3 unread messages");
    assert_eq!(m.folder_accessible_name(drafts), "Drafts: 8 items");
    assert_eq!(m.folder_accessible_name(junk), "Junk Email: 1 item");
    assert_eq!(m.folder_accessible_name(role(&m, a, FolderRole::Archive)), "Archive");
}

#[test]
fn move_and_delete_items() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let archive = role(&m, a, FolderRole::Archive);
    let deleted = role(&m, a, FolderRole::DeletedItems);
    let x = add(&mut m, inbox, false);
    let y = add(&mut m, inbox, true);
    let z = add(&mut m, archive, true);
    let moved = m.move_items(&[x, z, ItemId(999_999), x], archive).unwrap();
    assert_eq!(moved, vec![x]);
    assert_eq!(m.mail.get(x).unwrap().folder, archive);
    assert_eq!(m.move_items(&[y], FolderId(888_888)), Err(ModelError::NoFolder(FolderId(888_888))));
    assert!(matches!(m.move_items(&[y], role(&m, a, FolderRole::Calendar)), Err(ModelError::NotAllowed(_))));

    let snaps = m.delete_items(&[x, y]).unwrap();
    assert_eq!(snaps.len(), 2);
    assert_eq!(snaps[0].folder, archive);
    assert_eq!(snaps[1].folder, inbox);
    assert_eq!(m.mail.get(x).unwrap().folder, deleted);
    assert_eq!(m.mail.get(y).unwrap().folder, deleted);

    let snaps = m.delete_items(&[x, ItemId(777_777)]).unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].folder, deleted);
    assert!(!m.mail.contains(x));
    assert!(m.mail.contains(y));

    // Items in a subfolder of Deleted Items are purged too.
    let sub = m.create_folder(deleted, "Old").unwrap();
    m.move_items(&[y], sub).unwrap();
    m.delete_items(&[y]).unwrap();
    assert!(!m.mail.contains(y));
}

#[test]
fn read_state_and_counts() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let x = add(&mut m, inbox, false);
    let y = add(&mut m, inbox, true);
    assert_eq!(m.unread_count(inbox), 1);
    assert_eq!(m.total_count(inbox), 2);
    assert_eq!(m.set_read(&[x, y], true), vec![x]);
    assert_eq!(m.unread_count(inbox), 0);
    assert_eq!(m.items_in(inbox).len(), 2);
}

#[test]
fn folder_paths() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let p = m.create_folder(inbox, "Projects").unwrap();
    let q = m.create_folder(p, "2026").unwrap();
    assert_eq!(m.folder_path(inbox), "Inbox");
    assert_eq!(m.folder_path(q), "Inbox\\Projects\\2026");
    assert_eq!(m.folder_path(FolderId(31_337)), "");
    assert_eq!(m.subtree(p), vec![p, q]);
    assert!(m.is_in_subtree(q, inbox));
    assert!(!m.is_in_subtree(inbox, q));
}

#[test]
fn corrupt_cycle_does_not_hang() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let p = m.create_folder(inbox, "P").unwrap();
    let c = m.create_folder(p, "C").unwrap();
    // Corrupt: make P a child of C.
    m.folders.get_mut(&p).unwrap().parent = Some(c);
    let _ = m.folder_path(c);
    let _ = m.subtree(p);
    assert!(!m.validate().is_empty());
    assert!(!m.is_in_subtree(c, inbox));
}

#[test]
fn alloc_id_skips_used_ids() {
    let (mut m, _) = mb();
    let used: BTreeSet<u64> = m.folders.keys().map(|f| f.0).collect();
    m.next_id = 0;
    let id = m.alloc_id();
    assert!(!used.contains(&id));
    m.next_id = u64::MAX;
    let free = m.alloc_id();
    assert!(!used.contains(&free), "exhausted counter falls back to a free id");
    assert_eq!(free, id, "nothing was inserted in between, so the same lowest free id comes back");
    assert_eq!(free, 1 + used.len() as u64, "folders occupy 1..=n, so the lowest free id follows them");
}

#[test]
fn serde_round_trip() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    add(&mut m, inbox, false);
    m.rules.push(Rule { name: "r".into(), actions: vec![RuleAction::MoveTo(inbox)], ..Default::default() });
    let json = serde_json::to_string(&m).unwrap();
    let back: Mailbox = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

#[derive(Clone, Debug)]
enum Op {
    Create(usize, u8),
    Rename(usize, u8),
    Move(usize, usize),
    Delete(usize),
    AddMail(usize),
    DeleteMail(usize),
    MoveMail(usize, usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (any::<usize>(), 0u8..6).prop_map(|(a, b)| Op::Create(a, b)),
        (any::<usize>(), 0u8..6).prop_map(|(a, b)| Op::Rename(a, b)),
        (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Op::Move(a, b)),
        any::<usize>().prop_map(Op::Delete),
        any::<usize>().prop_map(Op::AddMail),
        any::<usize>().prop_map(Op::DeleteMail),
        (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Op::MoveMail(a, b)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn random_folder_operations_keep_invariants(ops in prop::collection::vec(op(), 0..80)) {
        let (mut m, a) = mb();
        let pick = |m: &Mailbox, i: usize| -> FolderId {
            let ids: Vec<FolderId> = m.folders.keys().copied().collect();
            ids[i % ids.len()]
        };
        let pick_item = |m: &Mailbox, i: usize| -> Option<ItemId> {
            let ids: Vec<ItemId> = m.mail.ids().collect();
            (!ids.is_empty()).then(|| ids[i % ids.len()])
        };
        let names = ["a", "B", "c", "A", "b", "deep"];
        for o in ops {
            match o {
                Op::Create(p, n) => { let _ = m.create_folder(pick(&m, p), names[n as usize]); }
                Op::Rename(f, n) => { let _ = m.rename_folder(pick(&m, f), names[n as usize]); }
                Op::Move(f, p) => { let (f, p) = (pick(&m, f), pick(&m, p)); let _ = m.move_folder(f, p); }
                Op::Delete(f) => { let _ = m.delete_folder(pick(&m, f)); }
                Op::AddMail(f) => { let f = pick(&m, f); m.add_mail(MailItem { folder: f, ..Default::default() }); }
                Op::DeleteMail(i) => { if let Some(i) = pick_item(&m, i) { let _ = m.delete_items(&[i]); } }
                Op::MoveMail(i, f) => {
                    if let Some(i) = pick_item(&m, i) { let f = pick(&m, f); let _ = m.move_items(&[i], f); }
                }
            }
            prop_assert!(m.validate().is_empty(), "{:?}", m.validate());
        }
        // Every default folder survives, at the root.
        for r in FolderRole::DEFAULTS {
            let id = m.role_folder(a, r);
            prop_assert!(id.is_some());
            prop_assert_eq!(m.folder(id.unwrap()).unwrap().parent, None);
        }
        // Ids are unique across folders and items, and keys match the stored ids.
        let folder_ids: BTreeSet<u64> = m.folders.keys().map(|f| f.0).collect();
        for (k, f) in &m.folders { prop_assert_eq!(*k, f.id); }
        for (k, item) in &m.mail {
            prop_assert_eq!(k, item.id);
            prop_assert!(!folder_ids.contains(&k.0));
        }
        // Sibling names stay unique (case-insensitive).
        for f in m.folders.values() {
            let kids = m.children(f.account, Some(f.id));
            let lower: BTreeSet<String> = kids.iter().map(|k| m.folder(*k).unwrap().name.to_lowercase()).collect();
            prop_assert_eq!(lower.len(), kids.len());
        }
        // Every folder's path walks back to a root.
        for id in m.folders.keys() {
            prop_assert!(!m.folder_path(*id).is_empty());
        }
    }
}

#[test]
fn exhausted_id_counter_never_reuses_ids() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    m.next_id = u64::MAX;
    let x = add(&mut m, inbox, false);
    let y = add(&mut m, inbox, true);
    let f = m.create_folder(inbox, "After exhaustion").unwrap();
    assert_ne!(x, y);
    assert_eq!(m.mail.len(), 2, "second insert must not overwrite the first");
    assert!(!m.mail.contains(ItemId(f.0)));
    assert!(m.validate().is_empty());
}

#[test]
fn account_ids_stay_unique_at_the_limit() {
    let (mut m, a) = mb();
    m.accounts[0].id = AccountId(u32::MAX);
    let _ = a;
    let b = m.add_account(Account { display_name: "B".into(), ..Default::default() });
    let c = m.add_account(Account { display_name: "C".into(), ..Default::default() });
    let ids: BTreeSet<AccountId> = m.accounts.iter().map(|x| x.id).collect();
    assert_eq!(ids.len(), 3, "{b:?} {c:?}");
}

#[test]
fn renaming_a_root_user_folder_checks_root_siblings() {
    let (mut m, a) = mb();
    let id = FolderId(m.alloc_id());
    m.folders.insert(id, crate::Folder { id, account: a, name: "Sent".into(), ..Default::default() });
    assert!(matches!(m.rename_folder(id, "inbox"), Err(ModelError::Invalid(_))));
    assert!(matches!(m.rename_folder(id, "SEARCH FOLDERS"), Err(ModelError::Invalid(_))));
    m.rename_folder(id, "Trash").unwrap();
    // Another account's roots do not clash.
    let b = m.add_account(Account { display_name: "B".into(), ..Default::default() });
    let other = FolderId(m.alloc_id());
    m.folders
        .insert(other, crate::Folder { id: other, account: b, name: "Old".into(), ..Default::default() });
    m.rename_folder(other, "Trash").unwrap();
}

#[test]
fn numbered_name_in_deleted_items_respects_length_limit() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let long = "n".repeat(255);
    let first = m.create_folder(inbox, &long).unwrap();
    m.delete_folder(first).unwrap();
    let second = m.create_folder(inbox, &long).unwrap();
    m.delete_folder(second).unwrap();
    let name = &m.folder(second).unwrap().name;
    assert!(name.ends_with(" (2)"), "{name}");
    assert_eq!(name.chars().count(), 255);
    // The numbered folder can still be renamed (its name passes validation).
    m.rename_folder(second, name.clone().as_str()).unwrap();
}

#[test]
fn validate_reports_duplicate_roles_and_orphans() {
    let (mut m, a) = mb();
    let id = FolderId(m.alloc_id());
    m.folders.insert(
        id,
        crate::Folder {
            id,
            account: a,
            name: "Inbox".into(),
            role: Some(FolderRole::Inbox),
            ..Default::default()
        },
    );
    m.mail.insert(ItemId(99_999), MailItem { id: ItemId(99_999), folder: FolderId(77_777), ..mail("x") });
    let problems = m.validate();
    assert!(problems.iter().any(|p| p.contains("Inbox appears 2 times")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("missing folder")), "{problems:?}");
}

#[test]
fn subtree_is_preorder_in_display_order() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    let p = m.create_folder(inbox, "P").unwrap();
    let z = m.create_folder(p, "zeta").unwrap();
    let al = m.create_folder(p, "Alpha").unwrap();
    let al_child = m.create_folder(al, "inner").unwrap();
    assert_eq!(m.subtree(p), vec![p, al, al_child, z]);
    assert!(m.subtree(FolderId(55_555)).is_empty());
}

#[test]
fn add_favorite_appends_even_with_gapped_positions() {
    // Loaded data may carry gaps in the favourite positions; a new favourite still goes to the end.
    let (mut m, a) = mb();
    let deleted = role(&m, a, FolderRole::DeletedItems);
    m.folders.get_mut(&deleted).unwrap().favorite_pos = Some(10);
    let junk = role(&m, a, FolderRole::JunkEmail);
    m.add_favorite(junk).unwrap();
    assert_eq!(names(&m, &m.favorites()), ["Inbox", "Sent Items", "Drafts", "Deleted Items", "Junk Email"]);
    let positions: Vec<u32> =
        m.favorites().iter().map(|f| m.folder(*f).unwrap().favorite_pos.unwrap()).collect();
    assert_eq!(positions, [0, 1, 2, 3, 4]);
}

#[test]
fn validate_reports_id_mismatches_and_collisions() {
    let (mut m, a) = mb();
    let inbox = role(&m, a, FolderRole::Inbox);
    assert!(m.validate().is_empty());
    // A map key that disagrees with the stored id.
    m.mail.insert(ItemId(5_000), MailItem { id: ItemId(5_001), folder: inbox, ..mail("x") });
    let problems = m.validate();
    assert!(problems.iter().any(|p| p.contains("stored under key")), "{problems:?}");
    m.mail.clear();
    // An item id that is also a folder id.
    m.mail.insert(ItemId(inbox.0), MailItem { id: ItemId(inbox.0), folder: inbox, ..mail("x") });
    let problems = m.validate();
    assert!(problems.iter().any(|p| p.contains("used more than once")), "{problems:?}");
    m.mail.clear();
    // Non-mail items in a missing folder.
    m.notes.insert(
        ItemId(6_000),
        crate::Note { id: ItemId(6_000), folder: FolderId(77_777), ..Default::default() },
    );
    let problems = m.validate();
    assert!(problems.iter().any(|p| p.contains("missing folder")), "{problems:?}");
}

#[test]
fn purging_a_folder_resets_quick_steps_that_target_it() {
    let (mut m, a) = mb();
    let deleted = role(&m, a, FolderRole::DeletedItems);
    let target = m.create_folder(deleted, "Old").unwrap();
    let keep = role(&m, a, FolderRole::Archive);
    m.quick_steps[0].actions = vec![
        crate::QuickStepAction::MoveTo(Some(target)),
        crate::QuickStepAction::CopyTo(Some(keep)),
        crate::QuickStepAction::MarkRead,
    ];
    assert!(!m.quick_steps[0].needs_setup());
    m.delete_folder(target).unwrap();
    assert_eq!(
        m.quick_steps[0].actions,
        vec![
            crate::QuickStepAction::MoveTo(None),
            crate::QuickStepAction::CopyTo(Some(keep)),
            crate::QuickStepAction::MarkRead
        ]
    );
    assert!(m.quick_steps[0].needs_setup(), "the step asks for a folder again");
}
