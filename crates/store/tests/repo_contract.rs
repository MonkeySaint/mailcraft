//! Storage stage S6: one contract for every [`MailRepo`]. The same cases run against the in-memory
//! [`MemRepo`] (the browser build, the sample mailbox, tests) and the SQLite [`SqlRepo`] (native profiles), so
//! the two cannot drift apart (storage ADR §7, risk 3). A property test then drives both with the same random
//! writes and change records and compares every read.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use mailcraft_model::{DateTime, FolderCounts, FolderId, ItemId, MailItem, ScanFilter};
use mailcraft_store::{Db, MailRepo, MemRepo, SqlRepo, StoreError};
use proptest::prelude::*;

fn item(id: u64, folder: u64, read: bool) -> MailItem {
    MailItem {
        id: ItemId(id),
        folder: FolderId(folder),
        read,
        subject: format!("item {id}"),
        received: DateTime { unix: 1_700_000_000 + i64::try_from(id % 1_000).unwrap(), offset_min: 0 },
        ..Default::default()
    }
}

fn scan_ids(r: &dyn MailRepo, f: &ScanFilter) -> Vec<ItemId> {
    let mut out = Vec::new();
    r.scan(f, &mut |m| {
        out.push(m.id);
        ControlFlow::Continue(())
    })
    .unwrap();
    out
}

fn get(r: &dyn MailRepo, id: u64) -> Option<MailItem> {
    r.get(ItemId(id)).map(|m| (*m).clone())
}

fn fc(total: usize, unread: usize) -> FolderCounts {
    FolderCounts { total, unread }
}

/// The contract, one function per case, written against `dyn MailRepo`.
mod cases {
    use super::*;

    pub fn empty(r: &mut dyn MailRepo) {
        assert_eq!(r.len(), 0);
        assert!(r.is_empty());
        assert!(r.get(ItemId(1)).is_none());
        assert!(!r.contains(ItemId(1)));
        assert_eq!(r.counts(FolderId(1)), FolderCounts::default());
        assert!(r.all_counts().is_empty());
        assert!(r.ids_in(FolderId(1)).is_empty());
        assert!(r.by_message_id("x@example.org").is_empty());
        assert_eq!(r.by_remote_id(FolderId(1), "7"), None);
        assert!(r.older_than(FolderId(1), i64::MAX, 10).is_empty());
        assert!(scan_ids(r, &ScanFilter::All).is_empty());
        assert!(!r.update(ItemId(1), &mut |m| m.read = true).unwrap());
        assert!(r.remove(&[ItemId(1)]).unwrap().is_empty());
        assert!(r.move_to(&[ItemId(1)], FolderId(2)).unwrap().is_empty());
        assert!(r.set_read(&[ItemId(1)], true).unwrap().is_empty());
    }

    pub fn insert_and_get(r: &mut dyn MailRepo) {
        assert_eq!(r.insert(item(5, 1, false)).unwrap(), None);
        assert_eq!(get(r, 5), Some(item(5, 1, false)));
        assert!(r.contains(ItemId(5)));
        assert_eq!(r.len(), 1);
        let mut changed = item(5, 2, true);
        changed.subject = "changed".into();
        assert_eq!(r.insert(changed.clone()).unwrap(), Some(item(5, 1, false)));
        assert_eq!(get(r, 5), Some(changed));
        assert_eq!(r.len(), 1);
        assert_eq!(r.counts(FolderId(1)), FolderCounts::default());
        assert_eq!(r.counts(FolderId(2)), fc(1, 0));
    }

    pub fn counts_follow_every_write(r: &mut dyn MailRepo) {
        for (id, folder, read) in [(1, 1, false), (2, 1, true), (3, 1, false), (4, 2, false)] {
            r.insert(item(id, folder, read)).unwrap();
        }
        assert_eq!(r.counts(FolderId(1)), fc(3, 2));
        assert_eq!(r.counts(FolderId(2)), fc(1, 1));
        assert_eq!(r.counts(FolderId(3)), FolderCounts::default());
        assert_eq!(r.set_read(&[ItemId(1), ItemId(2), ItemId(9)], true).unwrap(), vec![ItemId(1)]);
        assert_eq!(r.counts(FolderId(1)), fc(3, 1));
        assert_eq!(r.move_to(&[ItemId(3), ItemId(4), ItemId(9)], FolderId(2)).unwrap(), vec![ItemId(3)]);
        assert_eq!(r.counts(FolderId(1)), fc(2, 0));
        assert_eq!(r.counts(FolderId(2)), fc(2, 2));
        assert!(
            r.update(ItemId(4), &mut |m| {
                m.read = true;
                m.folder = FolderId(3);
            })
            .unwrap()
        );
        assert_eq!(r.counts(FolderId(2)), fc(1, 1));
        assert_eq!(r.counts(FolderId(3)), fc(1, 0));
        let gone = r.remove(&[ItemId(1), ItemId(2)]).unwrap();
        assert_eq!(gone.iter().map(|m| m.id).collect::<Vec<_>>(), vec![ItemId(1), ItemId(2)]);
        // A folder that emptied is missing from all_counts.
        let all = r.all_counts();
        assert_eq!(all.keys().copied().collect::<Vec<_>>(), vec![FolderId(2), FolderId(3)]);
        assert_eq!(all[&FolderId(2)], fc(1, 1));
        assert_eq!(r.len(), 2);
    }

    pub fn ids_are_ascending_and_scans_filter(r: &mut dyn MailRepo) {
        for id in [9, 3, 7, 1, 5] {
            r.insert(item(id, if id % 2 == 0 { 2 } else { 1 + id % 3 }, false)).unwrap();
        }
        let all = scan_ids(r, &ScanFilter::All);
        assert_eq!(all, [1, 3, 5, 7, 9].map(ItemId).to_vec());
        for f in 1..4 {
            let ids = r.ids_in(FolderId(f));
            let mut sorted = ids.clone();
            sorted.sort();
            assert_eq!(ids, sorted);
            assert_eq!(scan_ids(r, &ScanFilter::Folder(FolderId(f))), ids);
        }
        let set: BTreeSet<FolderId> = [FolderId(1), FolderId(3)].into_iter().collect();
        let both = scan_ids(r, &ScanFilter::Folders(set));
        let mut want: Vec<ItemId> = r.ids_in(FolderId(1));
        want.extend(r.ids_in(FolderId(3)));
        want.sort();
        assert_eq!(both, want);
        // A visitor that breaks stops the scan.
        let mut seen = 0;
        r.scan(&ScanFilter::All, &mut |_| {
            seen += 1;
            if seen == 2 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        })
        .unwrap();
        assert_eq!(seen, 2);
    }

    pub fn ids_past_i64_max_keep_their_order(r: &mut dyn MailRepo) {
        let big = [u64::MAX, i64::MAX as u64 + 1, i64::MAX as u64, 0, 1];
        for id in big {
            r.insert(item(id, 1, false)).unwrap();
        }
        let mut want: Vec<ItemId> = big.map(ItemId).to_vec();
        want.sort();
        assert_eq!(scan_ids(r, &ScanFilter::All), want);
        assert_eq!(scan_ids(r, &ScanFilter::Folder(FolderId(1))), want);
        assert_eq!(r.ids_in(FolderId(1)), want);
        assert_eq!(get(r, u64::MAX).unwrap().id, ItemId(u64::MAX));
        assert_eq!(r.counts(FolderId(1)), fc(5, 5));
        // Folder ids past i64::MAX too.
        r.move_to(&[ItemId(0)], FolderId(u64::MAX)).unwrap();
        assert_eq!(r.ids_in(FolderId(u64::MAX)), vec![ItemId(0)]);
        assert_eq!(r.all_counts().keys().last(), Some(&FolderId(u64::MAX)));
    }

    pub fn message_ids_match_by_normalised_key(r: &mut dyn MailRepo) {
        let with = |id: u64, mid: Option<&str>| MailItem {
            message_id: mid.map(str::to_owned),
            ..item(id, 1, false)
        };
        r.insert(with(4, Some("<X@Example.org>"))).unwrap();
        r.insert(with(2, Some(" x@example.ORG "))).unwrap();
        r.insert(with(3, Some("y@example.org"))).unwrap();
        r.insert(with(5, Some("<>"))).unwrap();
        r.insert(with(6, None)).unwrap();
        assert_eq!(r.by_message_id("x@example.org"), vec![ItemId(2), ItemId(4)]);
        assert_eq!(r.by_message_id(" <X@EXAMPLE.ORG>"), vec![ItemId(2), ItemId(4)]);
        assert_eq!(r.by_message_id("y@example.org"), vec![ItemId(3)]);
        assert!(r.by_message_id("<>").is_empty());
        assert!(r.by_message_id("").is_empty());
        assert!(r.by_message_id("z@example.org").is_empty());
        // Changing the Message-ID moves the item in the index.
        r.update(ItemId(3), &mut |m| m.message_id = Some("x@example.org".into())).unwrap();
        assert_eq!(r.by_message_id("x@example.org"), vec![ItemId(2), ItemId(3), ItemId(4)]);
        assert!(r.by_message_id("y@example.org").is_empty());
    }

    pub fn remote_ids_are_per_folder_lowest_first(r: &mut dyn MailRepo) {
        let with = |id: u64, folder: u64, remote: &str| MailItem {
            remote_id: Some(remote.into()),
            ..item(id, folder, true)
        };
        r.insert(with(8, 1, "100")).unwrap();
        r.insert(with(3, 1, "100")).unwrap();
        r.insert(with(4, 2, "100")).unwrap();
        r.insert(with(5, 1, "101")).unwrap();
        assert_eq!(r.by_remote_id(FolderId(1), "100"), Some(ItemId(3)));
        assert_eq!(r.by_remote_id(FolderId(2), "100"), Some(ItemId(4)));
        assert_eq!(r.by_remote_id(FolderId(1), "101"), Some(ItemId(5)));
        assert_eq!(r.by_remote_id(FolderId(3), "100"), None);
        assert_eq!(r.by_remote_id(FolderId(1), "10"), None);
        r.move_to(&[ItemId(3)], FolderId(2)).unwrap();
        assert_eq!(r.by_remote_id(FolderId(1), "100"), Some(ItemId(8)));
    }

    pub fn older_than_is_oldest_first_with_ties_by_id(r: &mut dyn MailRepo) {
        let at = |id: u64, folder: u64, unix: i64| MailItem {
            received: DateTime { unix, offset_min: 0 },
            ..item(id, folder, true)
        };
        for (id, unix) in [(10, 500), (4, 100), (7, 100), (2, 300), (9, 900), (1, 100)] {
            r.insert(at(id, 1, unix)).unwrap();
        }
        r.insert(at(3, 2, 50)).unwrap();
        r.insert(at(u64::MAX, 1, 100)).unwrap();
        assert_eq!(r.older_than(FolderId(1), 600, 100), [1, 4, 7, u64::MAX, 2, 10].map(ItemId).to_vec());
        assert_eq!(r.older_than(FolderId(1), 600, 2), vec![ItemId(1), ItemId(4)]);
        assert_eq!(r.older_than(FolderId(1), 100, 10), Vec::<ItemId>::new(), "the cutoff is exclusive");
        assert!(r.older_than(FolderId(1), 600, 0).is_empty());
        assert_eq!(r.older_than(FolderId(2), i64::MAX, 10), vec![ItemId(3)]);
        assert_eq!(r.older_than(FolderId(1), i64::MIN, 10), Vec::<ItemId>::new());
    }

    pub fn removes_and_moves_take_each_item_once(r: &mut dyn MailRepo) {
        for id in 1..=4 {
            r.insert(item(id, 1, false)).unwrap();
        }
        let gone = r.remove(&[ItemId(3), ItemId(9), ItemId(1), ItemId(3)]).unwrap();
        assert_eq!(gone, vec![item(3, 1, false), item(1, 1, false)]);
        assert_eq!(
            r.move_to(&[ItemId(2), ItemId(2), ItemId(4)], FolderId(5)).unwrap(),
            vec![ItemId(2), ItemId(4)]
        );
        assert_eq!(r.move_to(&[ItemId(2)], FolderId(5)).unwrap(), Vec::<ItemId>::new());
        assert_eq!(r.set_read(&[ItemId(4), ItemId(4)], true).unwrap(), vec![ItemId(4)]);
        assert_eq!(r.set_read(&[ItemId(4)], true).unwrap(), Vec::<ItemId>::new());
        assert_eq!(r.len(), 2);
    }

    pub fn commit_hands_back_before_images(r: &mut dyn MailRepo) {
        r.insert(item(1, 1, false)).unwrap();
        r.insert(item(2, 1, false)).unwrap();
        r.insert(item(3, 1, false)).unwrap();
        let t = r.begin();
        r.set_read(&[ItemId(1)], true).unwrap();
        r.set_read(&[ItemId(1)], false).unwrap();
        r.insert(item(7, 2, false)).unwrap();
        r.remove(&[ItemId(2)]).unwrap();
        r.move_to(&[ItemId(3)], FolderId(4)).unwrap();
        let ch = r.commit(t).unwrap();
        assert_eq!(ch.ids().collect::<Vec<_>>(), [1, 2, 3, 7].map(ItemId).to_vec());
        assert_eq!(ch.before(ItemId(1)), Some(Some(&item(1, 1, false))), "the oldest image wins");
        assert_eq!(ch.before(ItemId(2)), Some(Some(&item(2, 1, false))));
        assert_eq!(ch.before(ItemId(3)), Some(Some(&item(3, 1, false))));
        assert_eq!(ch.before(ItemId(7)), Some(None), "a new item had no before-image");
        // The record is closed.
        assert!(matches!(r.commit(t), Err(StoreError::NoChangeRecord)));
        assert!(matches!(r.rollback(t), Err(StoreError::NoChangeRecord)));
        // The writes stay.
        assert_eq!(get(r, 3).unwrap().folder, FolderId(4));
        assert!(get(r, 2).is_none());
        assert_eq!(r.counts(FolderId(4)), fc(1, 1));
    }

    pub fn rollback_puts_everything_back(r: &mut dyn MailRepo) {
        r.insert(item(1, 1, false)).unwrap();
        r.insert(item(2, 1, true)).unwrap();
        let before_counts = r.all_counts();
        let t = r.begin();
        r.set_read(&[ItemId(1)], true).unwrap();
        r.remove(&[ItemId(2)]).unwrap();
        r.insert(item(3, 2, false)).unwrap();
        r.update(ItemId(1), &mut |m| m.subject = "edited".into()).unwrap();
        let undone = r.rollback(t).unwrap();
        // What was undone: the items as they were just before.
        assert_eq!(undone.before(ItemId(1)).unwrap().unwrap().subject, "edited");
        assert_eq!(undone.before(ItemId(2)), Some(None));
        assert_eq!(undone.before(ItemId(3)), Some(Some(&item(3, 2, false))));
        assert_eq!(get(r, 1), Some(item(1, 1, false)));
        assert_eq!(get(r, 2), Some(item(2, 1, true)));
        assert_eq!(get(r, 3), None);
        assert_eq!(r.all_counts(), before_counts);
        assert_eq!(r.len(), 2);
        assert!(matches!(r.commit(t), Err(StoreError::NoChangeRecord)));
    }

    pub fn records_nest(r: &mut dyn MailRepo) {
        r.insert(item(1, 1, false)).unwrap();
        r.insert(item(2, 1, false)).unwrap();
        let outer = r.begin();
        r.set_read(&[ItemId(1)], true).unwrap();
        let inner = r.begin();
        r.set_read(&[ItemId(1), ItemId(2)], false).unwrap();
        r.set_read(&[ItemId(2)], true).unwrap();
        let ch = r.commit(inner).unwrap();
        // The inner record saw item 1 already read.
        assert!(ch.before(ItemId(1)).unwrap().unwrap().read);
        assert_eq!(ch.len(), 2);
        // The outer one keeps its own older images and gains the inner one's others.
        let ch = r.commit(outer).unwrap();
        assert!(!ch.before(ItemId(1)).unwrap().unwrap().read);
        assert!(!ch.before(ItemId(2)).unwrap().unwrap().read);
        // Rolling back an outer record undoes a committed inner one too.
        let outer = r.begin();
        let inner = r.begin();
        r.remove(&[ItemId(1)]).unwrap();
        r.commit(inner).unwrap();
        r.insert(item(9, 3, false)).unwrap();
        r.rollback(outer).unwrap();
        assert!(get(r, 1).is_some());
        assert!(get(r, 9).is_none());
        // A record left open above is folded into the one closed below it.
        let outer = r.begin();
        let _left_open = r.begin();
        r.move_to(&[ItemId(2)], FolderId(7)).unwrap();
        let ch = r.commit(outer).unwrap();
        assert_eq!(ch.ids().collect::<Vec<_>>(), vec![ItemId(2)]);
        assert_eq!(get(r, 2).unwrap().folder, FolderId(7));
        // Deep nesting stays bounded and still commits.
        let tokens: Vec<_> = (0..100).map(|_| r.begin()).collect();
        r.set_read(&[ItemId(2)], false).unwrap();
        let ch = r.commit(tokens[0]).unwrap();
        assert_eq!(ch.len(), 1);
    }
}

macro_rules! contract {
    ($name:ident, $make:expr) => {
        mod $name {
            use super::*;

            macro_rules! case {
                ($case:ident) => {
                    #[test]
                    fn $case() {
                        let (mut repo, _guard) = $make;
                        cases::$case(&mut repo);
                    }
                };
            }

            case!(empty);
            case!(insert_and_get);
            case!(counts_follow_every_write);
            case!(ids_are_ascending_and_scans_filter);
            case!(ids_past_i64_max_keep_their_order);
            case!(message_ids_match_by_normalised_key);
            case!(remote_ids_are_per_folder_lowest_first);
            case!(older_than_is_oldest_first_with_ties_by_id);
            case!(removes_and_moves_take_each_item_once);
            case!(commit_hands_back_before_images);
            case!(rollback_puts_everything_back);
            case!(records_nest);
        }
    };
}

fn sql_repo() -> (SqlRepo, tempfile::TempDir) {
    let d = tempfile::tempdir().unwrap();
    let db = Db::open(d.path()).unwrap();
    (SqlRepo::new(db), d)
}

contract!(mem, (MemRepo::new(), ()));
contract!(sql, sql_repo());

#[test]
fn sql_repo_writes_are_durable() {
    let (mut r, d) = sql_repo();
    r.insert(item(1, 1, false)).unwrap();
    let t = r.begin();
    r.insert(item(2, 1, false)).unwrap();
    r.set_read(&[ItemId(1)], true).unwrap();
    r.commit(t).unwrap();
    let t = r.begin();
    r.insert(item(3, 1, false)).unwrap();
    // Dropped with the record open: never committed.
    drop(r);
    let r = SqlRepo::new(Db::open(d.path()).unwrap());
    assert_eq!(get(&r, 1), Some(item(1, 1, true)));
    assert_eq!(get(&r, 2), Some(item(2, 1, false)));
    assert_eq!(get(&r, 3), None);
    assert_eq!(r.counts(FolderId(1)), fc(2, 1));
    let _ = t;
}

#[test]
fn sql_repo_reports_a_damaged_row_instead_of_panicking() {
    let (mut r, _d) = sql_repo();
    r.insert(item(1, 1, false)).unwrap();
    r.insert(item(2, 1, false)).unwrap();
    r.clear_cache();
    r.db().execute_raw("UPDATE mail SET meta = '{not json' WHERE id = 1").unwrap();
    assert!(r.get(ItemId(1)).is_none());
    assert!(r.get(ItemId(2)).is_some());
    let res = r.scan(&ScanFilter::All, &mut |_| ControlFlow::Continue(()));
    assert!(matches!(res, Err(StoreError::Corrupt(_))), "{res:?}");
    assert!(r.update(ItemId(1), &mut |m| m.read = true).is_err());
}

// ---- both repositories under the same random writes ----

#[derive(Clone, Debug)]
enum Step {
    Insert(u64, u64, bool, Option<u8>, Option<u8>),
    Update(u64, u64),
    Remove(Vec<u64>),
    Move(Vec<u64>, u64),
    Read(Vec<u64>, bool),
    Begin,
    Commit,
    Rollback,
}

fn step() -> impl Strategy<Value = Step> {
    let id = || prop_oneof![8 => 0u64..24, 1 => Just(u64::MAX), 1 => Just(i64::MAX as u64 + 1)];
    prop_oneof![
        5 => (id(), 0u64..4, any::<bool>(), proptest::option::of(0u8..5), proptest::option::of(0u8..5))
            .prop_map(|(i, f, r, m, x)| Step::Insert(i, f, r, m, x)),
        2 => (id(), 0u64..4).prop_map(|(i, f)| Step::Update(i, f)),
        2 => proptest::collection::vec(id(), 0..4).prop_map(Step::Remove),
        2 => (proptest::collection::vec(id(), 0..5), 0u64..4).prop_map(|(v, f)| Step::Move(v, f)),
        2 => (proptest::collection::vec(id(), 0..5), any::<bool>()).prop_map(|(v, r)| Step::Read(v, r)),
        2 => Just(Step::Begin),
        2 => Just(Step::Commit),
        1 => Just(Step::Rollback),
    ]
}

/// Every read, for comparing two repositories.
fn reads(r: &dyn MailRepo) -> String {
    let mut items = Vec::new();
    r.scan(&ScanFilter::All, &mut |m| {
        items.push(m.clone());
        ControlFlow::Continue(())
    })
    .unwrap();
    let per_folder: Vec<_> = (0..4)
        .map(|f| {
            let f = FolderId(f);
            (r.counts(f), r.ids_in(f), r.older_than(f, 1_700_000_010, 5), r.by_remote_id(f, "r1"))
        })
        .collect();
    let mids: Vec<_> = (0..5).map(|m| r.by_message_id(&format!("<M{m}@example.org>"))).collect();
    format!("{:?}\n{:?}\n{:?}\n{:?}\n{}", items, per_folder, mids, r.all_counts(), r.len())
}

fn run(r: &mut dyn MailRepo, steps: &[Step]) -> Vec<String> {
    let mut open = Vec::new();
    let mut out = Vec::new();
    for s in steps {
        let note = match s {
            Step::Insert(id, f, read, mid, remote) => {
                let mut m = item(*id, *f, *read);
                m.message_id = mid.map(|x| format!("m{x}@Example.org"));
                m.remote_id = remote.map(|x| format!("r{}", x % 2));
                format!("{:?}", r.insert(m).unwrap())
            }
            Step::Update(id, f) => {
                format!(
                    "{:?}",
                    r.update(ItemId(*id), &mut |m| {
                        m.folder = FolderId(*f);
                        m.subject.push('!');
                    })
                    .unwrap()
                )
            }
            Step::Remove(ids) => {
                format!("{:?}", r.remove(&ids.iter().copied().map(ItemId).collect::<Vec<_>>()).unwrap())
            }
            Step::Move(ids, f) => {
                format!(
                    "{:?}",
                    r.move_to(&ids.iter().copied().map(ItemId).collect::<Vec<_>>(), FolderId(*f)).unwrap()
                )
            }
            Step::Read(ids, read) => {
                format!(
                    "{:?}",
                    r.set_read(&ids.iter().copied().map(ItemId).collect::<Vec<_>>(), *read).unwrap()
                )
            }
            Step::Begin => {
                open.push(r.begin());
                String::new()
            }
            Step::Commit => match open.pop() {
                Some(t) => format!("{:?}", r.commit(t).unwrap()),
                None => String::new(),
            },
            Step::Rollback => match open.pop() {
                Some(t) => format!("{:?}", r.rollback(t).unwrap()),
                None => String::new(),
            },
        };
        out.push(note);
        out.push(reads(r));
    }
    while let Some(t) = open.pop() {
        out.push(format!("{:?}", r.commit(t).unwrap()));
    }
    out.push(reads(r));
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn both_repositories_answer_the_same(steps in proptest::collection::vec(step(), 1..40)) {
        let mut mem = MemRepo::new();
        let (mut sql, d) = sql_repo();
        let a = run(&mut mem, &steps);
        let b = run(&mut sql, &steps);
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            prop_assert_eq!(x, y, "step {} of {:?}", i / 2, steps);
        }
        // What the SQLite repository committed is what a fresh connection reads.
        drop(sql);
        let again = SqlRepo::new(Db::open(d.path()).unwrap());
        prop_assert_eq!(reads(&mem), reads(&again));
    }
}
