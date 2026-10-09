use std::collections::BTreeSet;

use proptest::prelude::*;

use super::mail;
use crate::thread::{fnv1a, thread, topic_key};
use crate::{DateTime, ItemId, MailItem};

fn msg(id: u64, subject: &str, mid: &str, parent: &[&str], t: i64) -> MailItem {
    MailItem {
        id: ItemId(id),
        message_id: (!mid.is_empty()).then(|| mid.to_string()),
        in_reply_to: parent.last().map(|p| vec![p.to_string()]).unwrap_or_default(),
        references: parent.iter().map(|p| p.to_string()).collect(),
        received: DateTime { unix: t, offset_min: 0 },
        ..mail(subject)
    }
}

#[test]
fn reply_chain_by_references() {
    let a = msg(1, "Plan", "a@x", &[], 100);
    let b = msg(2, "Re: Plan (revised)", "b@x", &["a@x"], 200);
    let c = msg(3, "Totally different", "c@x", &["a@x", "b@x"], 300);
    let other = msg(4, "Lunch", "d@x", &[], 250);
    let convs = thread(&[&a, &b, &c, &other]);
    assert_eq!(convs.len(), 2);
    assert_eq!(convs[0].items, vec![ItemId(3), ItemId(2), ItemId(1)]);
    assert_eq!(convs[0].topic, "Plan");
    assert_eq!(convs[0].latest.unix, 300);
    assert_eq!(convs[0].unread, 3);
    assert_eq!(convs[1].items, vec![ItemId(4)]);
    assert_eq!(convs[1].newest(), Some(ItemId(4)));
}

#[test]
fn siblings_with_missing_parent_join() {
    let b = msg(1, "x1", "b@x", &["missing@x"], 10);
    let c = msg(2, "x2", "c@x", &["<missing@x>"], 20);
    let convs = thread(&[&b, &c]);
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].len(), 2);
}

#[test]
fn forward_joins_by_topic() {
    let a = msg(1, "Quarterly numbers", "a@x", &[], 100);
    let f = msg(2, "FW: Quarterly  NUMBERS", "f@y", &[], 200);
    let r = msg(3, "RE: Quarterly numbers", "", &[], 150);
    let convs = thread(&[&f, &a, &r]);
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].items, vec![ItemId(2), ItemId(3), ItemId(1)]);
    assert_eq!(convs[0].topic, "Quarterly numbers");
    assert_eq!(convs[0].key, topic_key("Quarterly numbers"));
}

#[test]
fn empty_subjects_do_not_merge() {
    let a = msg(1, "", "", &[], 1);
    let b = msg(2, "RE:", "", &[], 2);
    let convs = thread(&[&a, &b]);
    assert_eq!(convs.len(), 2);
    assert_ne!(convs[0].key, convs[1].key);
    assert_eq!(topic_key(""), 0);
}

#[test]
fn conversations_sorted_by_latest() {
    let a = msg(1, "A", "", &[], 500);
    let b = msg(2, "B", "", &[], 100);
    let b2 = msg(3, "RE: B", "", &[], 900);
    let convs = thread(&[&a, &b, &b2]);
    assert_eq!(convs.iter().map(|c| c.topic.as_str()).collect::<Vec<_>>(), ["B", "A"]);
}

#[test]
fn duplicate_items_counted_once() {
    let a = msg(1, "A", "a@x", &[], 1);
    let convs = thread(&[&a, &a]);
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].items, vec![ItemId(1)]);
}

#[test]
fn read_items_reduce_unread() {
    let mut a = msg(1, "A", "", &[], 1);
    a.read = true;
    let b = msg(2, "Re: A", "", &[], 2);
    assert_eq!(thread(&[&a, &b])[0].unread, 1);
}

#[test]
fn fnv_is_stable() {
    assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert!(thread(&[]).is_empty());
}

proptest! {
    #[test]
    fn every_item_lands_in_exactly_one_conversation(
        specs in prop::collection::vec((0u8..6, 0u8..6, prop::collection::vec(0u8..6, 0..3), any::<i32>()), 0..40)
    ) {
        let items: Vec<MailItem> = specs.iter().enumerate().map(|(i, (subj, mid, refs, t))| {
            let refs: Vec<String> = refs.iter().map(|r| format!("m{r}@x")).collect();
            MailItem {
                id: ItemId(i as u64 + 1),
                message_id: Some(format!("m{mid}@x")),
                references: refs,
                received: DateTime { unix: i64::from(*t), offset_min: 0 },
                ..mail(["", "a", "RE: a", "b", "FW: B", "c"][*subj as usize])
            }
        }).collect();
        let refs: Vec<&MailItem> = items.iter().collect();
        let convs = thread(&refs);
        let mut seen = BTreeSet::new();
        for c in &convs {
            prop_assert!(!c.items.is_empty());
            for w in c.items.windows(2) {
                let a = items.iter().find(|m| m.id == w[0]).unwrap();
                let b = items.iter().find(|m| m.id == w[1]).unwrap();
                prop_assert!(a.received.unix >= b.received.unix);
            }
            for id in &c.items { prop_assert!(seen.insert(*id)); }
        }
        prop_assert_eq!(seen.len(), items.len());
        for w in convs.windows(2) { prop_assert!(w[0].latest.unix >= w[1].latest.unix); }
    }
}
