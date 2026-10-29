//! Change records on `MailTable` (storage stage S3): a commit holds exactly the items that differ from the
//! state at `begin`, a rollback restores that state, records nest, and before-images put back with `restore`
//! revert a step and hand back the step that reverts it.

use std::collections::BTreeMap;

use proptest::prelude::*;

use super::mail;
use crate::{FolderId, ItemId, MailChanges, MailItem, MailTable};

fn item(id: u64, folder: u64) -> MailItem {
    MailItem { id: ItemId(id), folder: FolderId(folder), ..mail(&format!("m{id}")) }
}

fn table(ids: &[u64]) -> MailTable {
    ids.iter().map(|id| (ItemId(*id), item(*id, 1))).collect()
}

/// The before-images as a plain map, without the touched-but-unchanged items.
fn settled(mut c: MailChanges, now: &MailTable) -> BTreeMap<ItemId, Option<MailItem>> {
    c.retain_changed(now);
    c.into_iter().collect()
}

#[test]
fn nothing_is_recorded_without_a_record() {
    let mut t = table(&[1, 2]);
    assert!(!t.is_recording());
    t.update(ItemId(1), |m| m.read = true);
    let token = t.begin();
    assert!(t.is_recording());
    let c = t.commit(token).unwrap();
    assert!(c.is_empty());
    assert!(t.commit(token).is_none(), "closed already");
    assert!(t.rollback(token).is_none());
}

#[test]
fn each_write_keeps_the_first_before_image() {
    let mut t = table(&[1, 2, 3, 4, 5]);
    let start = t.clone();
    let token = t.begin();
    t.update(ItemId(1), |m| m.read = true);
    t.update(ItemId(1), |m| m.subject = "twice".into());
    t.remove(ItemId(2));
    t.insert(ItemId(9), item(9, 2));
    t.retain(|id, _| id.0 != 3);
    t.update_where(|m| m.id.0 == 4, |m| m.folder = FolderId(7));
    let _ = t.get_mut(ItemId(5));
    assert!(!t.update(ItemId(42), |m| m.read = true), "no such item");
    let c = t.commit(token).unwrap();
    assert_eq!(c.len(), 6, "1, 2, 3, 4, 5 (touched) and 9");
    assert_eq!(c.before(ItemId(1)).flatten(), start.get(ItemId(1)));
    assert_eq!(c.before(ItemId(9)), Some(None));
    assert_eq!(c.before(ItemId(8)), None);
    assert_eq!(c.changed(&t).count(), 5, "5 was only touched");
    assert_eq!(settled(c, &t), settled(MailChanges::between(&start, &t), &t));
}

#[test]
fn rollback_restores_and_reports_what_it_undid() {
    let mut t = table(&[1, 2, 3]);
    let start = t.clone();
    let token = t.begin();
    t.clear();
    t.insert(ItemId(4), item(4, 1));
    let middle = t.clone();
    let undone = t.rollback(token).unwrap();
    assert_eq!(t, start);
    assert!(!t.is_recording());
    // What was undone leads back to the middle state.
    let mut again = t.clone();
    again.restore(undone);
    assert_eq!(again, middle);
}

#[test]
fn nested_records_leave_the_outer_one_complete() {
    let mut t = table(&[1, 2, 3]);
    let start = t.clone();
    let outer = t.begin();
    t.update(ItemId(1), |m| m.read = true);
    let inner = t.begin();
    t.update(ItemId(1), |m| m.subject = "inner".into());
    t.update(ItemId(2), |m| m.subject = "inner".into());
    let c = t.commit(inner).unwrap();
    assert_eq!(c.before(ItemId(1)).flatten().map(|m| m.read), Some(true), "inner sees its own start");
    // A rolled-back inner record leaves no trace in the outer one.
    let rolled = t.begin();
    t.remove(ItemId(3));
    t.rollback(rolled).unwrap();
    // One left open is folded into the outer record when that closes.
    let _open = t.begin();
    t.insert(ItemId(5), item(5, 1));
    let c = t.commit(outer).unwrap();
    assert!(!t.is_recording());
    assert_eq!(c.before(ItemId(1)).flatten(), start.get(ItemId(1)), "outer keeps the older image");
    assert_eq!(settled(c, &t), settled(MailChanges::between(&start, &t), &t));
}

#[test]
fn a_clone_keeps_the_record_and_a_foreign_table_loses_it() {
    let mut t = table(&[1, 2]);
    let token = t.begin();
    t.update(ItemId(1), |m| m.read = true);
    let saved = t.clone();
    t.update(ItemId(2), |m| m.read = true);
    // Put back from a clone taken inside the record: it still describes the changes up to the clone.
    t = saved;
    let c = t.commit(token).unwrap();
    assert_eq!(c.ids().collect::<Vec<_>>(), vec![ItemId(1)]);
    assert_eq!(t.get(ItemId(2)).map(|m| m.read), Some(false));
    // A table from elsewhere has no record.
    let token = t.begin();
    t = table(&[1, 2]);
    assert!(t.commit(token).is_none());
    // Records are not part of equality or of the stored form.
    let mut a = table(&[1]);
    let b = a.clone();
    let _ = a.begin();
    assert_eq!(a, b);
    assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
}

#[test]
fn restore_swaps_a_step_back_and_forth() {
    let mut t = table(&[1, 2, 3]);
    let before = t.clone();
    let token = t.begin();
    t.update(ItemId(1), |m| m.read = true);
    t.remove(ItemId(2));
    t.insert(ItemId(7), item(7, 3));
    let step = t.commit(token).unwrap();
    let after = t.clone();
    let redo = t.restore(step);
    assert_eq!(t, before);
    let undo = t.restore(redo);
    assert_eq!(t, after);
    assert_eq!(undo.len(), 3);
    // Inside a record, a restore is recorded like any write.
    let token = t.begin();
    t.restore(undo);
    let c = t.commit(token).unwrap();
    assert_eq!(settled(c, &t), settled(MailChanges::between(&after, &t), &t));
}

#[test]
fn absorb_keeps_the_older_image_and_sizes_add_up() {
    let mut a = MailChanges::new();
    a.record(ItemId(1), Some(item(1, 1)));
    let mut b = MailChanges::new();
    b.record(ItemId(1), Some(item(1, 9)));
    b.record(ItemId(2), None);
    let small = a.approx_bytes();
    a.absorb(b);
    assert_eq!(a.before(ItemId(1)).flatten().map(|m| m.folder), Some(FolderId(1)));
    assert_eq!(a.len(), 2);
    assert!(a.approx_bytes() > small);
    let from: MailChanges = [(ItemId(3), None), (ItemId(3), Some(item(3, 1)))].into_iter().collect();
    assert_eq!(from.before(ItemId(3)), Some(None), "the first image wins");
}

#[test]
fn deep_nesting_stays_bounded() {
    let mut t = table(&[1]);
    let outer = t.begin();
    for i in 0..500u64 {
        let _ = t.begin();
        t.insert(ItemId(100 + i), item(100 + i, 1));
    }
    let c = t.commit(outer).unwrap();
    assert_eq!(c.len(), 500);
    assert!(!t.is_recording());
}

/// Records left open with nothing below them (a panic between `begin` and `commit`) are dropped by
/// `abandon_all`, leaving the items as they are; their tokens are no longer open.
#[test]
fn abandon_all_drops_records_left_open() {
    let mut t = table(&[1, 2]);
    let stray = t.begin();
    let inner = t.begin();
    t.update(ItemId(1), |m| m.subject = "changed".into());
    assert!(t.is_open(stray) && t.is_open(inner));
    assert_eq!(t.abandon_all(), 2);
    assert!(!t.is_recording() && !t.is_open(stray));
    assert_eq!(t.get(ItemId(1)).map(|m| m.subject.as_str()), Some("changed"));
    assert!(t.commit(stray).is_none(), "the record is gone");
    // Writes made afterwards are kept by no record.
    let fresh = t.begin();
    t.update(ItemId(2), |m| m.subject = "later".into());
    assert_eq!(t.commit(fresh).map(|c| c.len()), Some(1));
    assert_eq!(t.abandon_all(), 0);
}

/// One write to the table, picked by proptest.
#[derive(Clone, Debug)]
enum W {
    Insert(u64, u64),
    Remove(u64),
    Read(u64),
    Touch(u64),
    MoveWhere(u64, u64),
    RetainNot(u64),
    Clear,
    /// Opens a nested record.
    Begin,
    /// Closes the innermost nested record (commit or rollback).
    End(bool),
}

fn write() -> impl Strategy<Value = W> {
    prop_oneof![
        4 => (0u64..12, 0u64..4).prop_map(|(i, f)| W::Insert(i, f)),
        3 => (0u64..12).prop_map(W::Remove),
        4 => (0u64..12).prop_map(W::Read),
        2 => (0u64..12).prop_map(W::Touch),
        2 => (0u64..4, 0u64..4).prop_map(|(a, b)| W::MoveWhere(a, b)),
        1 => (0u64..4).prop_map(W::RetainNot),
        1 => Just(W::Clear),
        2 => Just(W::Begin),
        2 => any::<bool>().prop_map(W::End),
    ]
}

proptest! {
    /// Whatever the writes, nesting and rollbacks, the outer commit holds exactly the items that differ from
    /// the start (as a full comparison finds them), each with its image from the start, and a rollback of
    /// the outer record brings the start back.
    #[test]
    fn commits_match_a_full_comparison(
        start in proptest::collection::btree_set(0u64..12, 0..10),
        writes in proptest::collection::vec(write(), 0..40),
        roll_outer in any::<bool>(),
    ) {
        let ids: Vec<u64> = start.into_iter().collect();
        let mut t = table(&ids);
        let initial = t.clone();
        let outer = t.begin();
        let mut nested: Vec<(crate::ChangeToken, MailTable)> = Vec::new();
        for w in writes {
            match w {
                W::Insert(i, f) => {
                    t.insert(ItemId(i), item(i, f));
                }
                W::Remove(i) => {
                    t.remove(ItemId(i));
                }
                W::Read(i) => {
                    t.update(ItemId(i), |m| m.read = !m.read);
                }
                W::Touch(i) => {
                    let _ = t.get_mut(ItemId(i));
                }
                W::MoveWhere(a, b) => {
                    t.update_where(|m| m.folder == FolderId(a), |m| m.folder = FolderId(b));
                }
                W::RetainNot(f) => t.retain(|_, m| m.folder != FolderId(f)),
                W::Clear => t.clear(),
                W::Begin => {
                    let snap = t.clone();
                    nested.push((t.begin(), snap));
                }
                W::End(commit) => {
                    if let Some((token, snap)) = nested.pop() {
                        if commit {
                            let c = t.commit(token).unwrap();
                            prop_assert_eq!(settled(c, &t), settled(MailChanges::between(&snap, &t), &t));
                        } else {
                            t.rollback(token).unwrap();
                            prop_assert_eq!(&t, &snap);
                        }
                    }
                }
            }
        }
        // Nested records still open fold into the outer one.
        if roll_outer {
            let now = t.clone();
            let undone = t.rollback(outer).unwrap();
            prop_assert_eq!(&t, &initial);
            let mut again = t.clone();
            again.restore(undone);
            prop_assert_eq!(again, now);
        } else {
            let c = t.commit(outer).unwrap();
            let want = settled(MailChanges::between(&initial, &t), &t);
            prop_assert_eq!(settled(c.clone(), &t), want);
            // The images revert the whole stretch.
            let now = t.clone();
            let back = t.restore(c);
            prop_assert_eq!(&t, &initial);
            t.restore(back);
            prop_assert_eq!(t, now);
        }
    }
}
