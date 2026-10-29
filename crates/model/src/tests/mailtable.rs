//! `MailTable`: the read/write API over a mailbox's mail behaves exactly like the map it wraps, and
//! serialises byte for byte like it.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use proptest::prelude::*;

use super::mail;
use crate::{DateTime, FolderCounts, FolderId, ItemId, MailItem, MailTable, ScanFilter};

fn item(id: u64, folder: u64, read: bool, received: i64) -> MailItem {
    MailItem {
        id: ItemId(id),
        folder: FolderId(folder),
        read,
        received: DateTime { unix: received, offset_min: 0 },
        ..mail(&format!("m{id}"))
    }
}

fn table(items: &[MailItem]) -> (MailTable, BTreeMap<ItemId, MailItem>) {
    let map: BTreeMap<ItemId, MailItem> = items.iter().map(|m| (m.id, m.clone())).collect();
    (MailTable::from(map.clone()), map)
}

#[test]
fn serialises_like_the_map_it_replaces() {
    let mut odd = item(9, 2, false, 5);
    odd.subject = "Grüße \u{1F600} \"quoted\" \\ \n".into();
    odd.raw = Some(vec![0, 1, 255]);
    odd.message_id = Some("a@example.com".into());
    // Stored under a key other than its id: the format keeps the key as given.
    let (t, map) = table(&[item(1, 2, true, 10), odd, item(3, 4, false, 7)]);
    let mut t = t;
    let mut map = map;
    t.insert(ItemId(40), item(41, 2, true, 1));
    map.insert(ItemId(40), item(41, 2, true, 1));
    assert_eq!(serde_json::to_vec(&t).unwrap(), serde_json::to_vec(&map).unwrap());
    assert_eq!(serde_json::to_string_pretty(&t).unwrap(), serde_json::to_string_pretty(&map).unwrap());
    let back: MailTable = serde_json::from_slice(&serde_json::to_vec(&map).unwrap()).unwrap();
    assert_eq!(back, t);
    assert_eq!(format!("{t:?}"), format!("{map:?}"));
    let empty = MailTable::new();
    assert_eq!(serde_json::to_string(&empty).unwrap(), "{}");
    assert_eq!(serde_json::from_str::<MailTable>("{}").unwrap(), empty);
}

#[test]
fn hostile_json_is_refused_not_panicked() {
    for bad in ["[]", "{\"x\":{}}", "{\"1\":5}", "null", "{\"1\":{\"id\":\"no\"}}"] {
        assert!(serde_json::from_str::<MailTable>(bad).is_err(), "{bad}");
    }
}

#[test]
fn targeted_reads() {
    let mut a = item(1, 10, false, 300);
    a.message_id = Some("x@example.org".into());
    a.remote_id = Some("7".into());
    let mut b = item(2, 10, true, 100);
    b.message_id = Some("x@example.org".into());
    let mut c = item(3, 11, false, 50);
    c.remote_id = Some("7".into());
    let (t, _) = table(&[a, b, c, item(4, 10, false, 100)]);
    assert_eq!(t.len(), 4);
    assert!(!t.is_empty() && t.contains(ItemId(3)) && !t.contains(ItemId(99)));
    assert_eq!(t.get(ItemId(2)).map(|m| m.subject.as_str()), Some("m2"));
    assert!(t.get(ItemId(99)).is_none());
    assert_eq!(t.ids_in(FolderId(10)), vec![ItemId(1), ItemId(2), ItemId(4)]);
    assert!(t.ids_in(FolderId(99)).is_empty());
    assert_eq!(t.counts(FolderId(10)), FolderCounts { total: 3, unread: 2 });
    assert_eq!(t.counts(FolderId(99)), FolderCounts::default());
    let all = t.all_counts();
    assert_eq!(all.len(), 2);
    assert_eq!(all.get(&FolderId(11)), Some(&FolderCounts { total: 1, unread: 1 }));
    assert_eq!(t.by_message_id("x@example.org"), vec![ItemId(1), ItemId(2)]);
    assert!(t.by_message_id("").is_empty());
    assert!(t.by_message_id("y@example.org").is_empty());
    assert_eq!(t.by_remote_id(FolderId(10), "7"), Some(ItemId(1)));
    assert_eq!(t.by_remote_id(FolderId(11), "7"), Some(ItemId(3)));
    assert_eq!(t.by_remote_id(FolderId(12), "7"), None);
    // Oldest first, ties by id, capped.
    assert_eq!(t.older_than(FolderId(10), 301, 10), vec![ItemId(2), ItemId(4), ItemId(1)]);
    assert_eq!(t.older_than(FolderId(10), 300, 10), vec![ItemId(2), ItemId(4)]);
    assert_eq!(t.older_than(FolderId(10), 301, 1), vec![ItemId(2)]);
    assert!(t.older_than(FolderId(10), i64::MIN, 10).is_empty());
    assert!(t.older_than(FolderId(10), i64::MAX, 0).is_empty());
    assert_eq!(t.body(ItemId(1)), Some(&t.get(ItemId(1)).unwrap().body));
    assert!(t.body(ItemId(99)).is_none());
    let folder: Vec<ItemId> = t.in_folder(FolderId(10)).rev().map(|m| m.id).collect();
    assert_eq!(folder, vec![ItemId(4), ItemId(2), ItemId(1)]);
}

/// The Message-ID lookup uses one normalised key (the engine's receipt, vote and Focused matching), on the
/// stored value and on the argument alike, so a database index on that key answers every caller.
#[test]
fn message_id_lookup_ignores_brackets_space_and_case() {
    let mut a = item(1, 10, false, 0);
    a.message_id = Some("<X@Example.org>".into());
    let mut b = item(2, 11, false, 0);
    b.message_id = Some("x@example.org".into());
    let mut c = item(3, 10, false, 0);
    c.message_id = Some(" x@EXAMPLE.ORG ".into());
    let mut d = item(4, 10, false, 0);
    d.message_id = Some("x@example.org.net".into());
    let mut e = item(5, 10, false, 0);
    e.message_id = Some("<>".into());
    let (t, _) = table(&[a, b, c, d, e, item(6, 10, false, 0)]);
    let all = vec![ItemId(1), ItemId(2), ItemId(3)];
    for q in ["x@example.org", "<X@Example.org>", "  <x@example.ORG>  ", "<<x@example.org>>"] {
        assert_eq!(t.by_message_id(q), all, "{q:?}");
    }
    // Only ASCII letters fold; other text compares as written.
    assert!(t.by_message_id("x@exämple.org").is_empty());
    // An empty key matches nothing, not the items whose Message-ID is empty or just brackets.
    for q in ["", "  ", "<>", "< >"] {
        assert!(t.by_message_id(q).is_empty(), "{q:?}");
    }
    assert_eq!(crate::message_id_key(" <X@Example.org> "), "x@example.org");
    assert_eq!(crate::message_id_key("<>"), "");
    assert_eq!(crate::message_id_key("Grüße@Example.ORG"), "grüße@example.org");
}

#[test]
fn scan_visits_in_order_and_stops_on_break() {
    let (t, _) =
        table(&[item(1, 1, false, 0), item(2, 2, false, 0), item(3, 1, false, 0), item(4, 3, false, 0)]);
    let mut seen = Vec::new();
    t.scan(&ScanFilter::All, &mut |m| {
        seen.push(m.id.0);
        ControlFlow::Continue(())
    });
    assert_eq!(seen, [1, 2, 3, 4]);
    seen.clear();
    t.scan(&ScanFilter::Folder(FolderId(1)), &mut |m| {
        seen.push(m.id.0);
        ControlFlow::Continue(())
    });
    assert_eq!(seen, [1, 3]);
    seen.clear();
    let set: BTreeSet<FolderId> = [FolderId(2), FolderId(3)].into();
    t.scan(&ScanFilter::Folders(set), &mut |m| {
        seen.push(m.id.0);
        ControlFlow::Break(())
    });
    assert_eq!(seen, [2]);
}

#[test]
fn writes_behave_like_the_map() {
    let (mut t, mut map) = table(&[item(1, 1, false, 0), item(2, 2, true, 0), item(3, 1, false, 0)]);
    assert_eq!(t.insert(ItemId(2), item(2, 5, false, 0)), map.insert(ItemId(2), item(2, 5, false, 0)));
    assert_eq!(t.remove(ItemId(1)), map.remove(&ItemId(1)));
    assert_eq!(t.remove(ItemId(1)), None);
    if let Some(m) = t.get_mut(ItemId(3)) {
        m.read = true;
    }
    if let Some(m) = map.get_mut(&ItemId(3)) {
        m.read = true;
    }
    t.retain(|id, m| id.0 != 2 || !m.read);
    map.retain(|id, m| id.0 != 2 || !m.read);
    for m in t.items_mut() {
        m.subject.push('!');
    }
    for m in map.values_mut() {
        m.subject.push('!');
    }
    for (id, m) in t.iter_mut() {
        m.size = id.0;
    }
    for (id, m) in map.iter_mut() {
        m.size = id.0;
    }
    assert_eq!(t, MailTable::from(map.clone()));
    t.extend([(ItemId(8), item(8, 1, false, 0))]);
    assert!(t.contains(ItemId(8)));
    t.clear();
    assert!(t.is_empty());
    let collected: MailTable = map.clone().into_iter().collect();
    assert_eq!(collected.into_iter().collect::<BTreeMap<_, _>>(), map);
}

proptest! {
    /// Every read agrees with the same question asked of a plain map.
    #[test]
    fn reads_agree_with_a_plain_map(
        spec in proptest::collection::vec((0u64..60, 0u64..4, any::<bool>(), -5i64..5, 0u8..3), 0..40),
        folder in 0u64..5,
        cutoff in -6i64..6,
        limit in 0usize..6,
    ) {
        let items: Vec<MailItem> = spec
            .iter()
            .map(|(id, f, read, rec, tag)| {
                let mut m = item(*id, *f, *read, *rec);
                // Stored forms that share a key: bare, bracketed, upper-case.
                m.message_id = match (*tag, id % 3) {
                    (0, _) => None,
                    (_, 0) => Some(format!("id{tag}@example.net")),
                    (_, 1) => Some(format!("<id{tag}@example.net>")),
                    _ => Some(format!("ID{tag}@Example.NET")),
                };
                m.remote_id = (*tag < 2).then(|| tag.to_string());
                m
            })
            .collect();
        let (t, map) = table(&items);
        let f = FolderId(folder);
        prop_assert_eq!(t.len(), map.len());
        prop_assert_eq!(t.ids().collect::<Vec<_>>(), map.keys().copied().collect::<Vec<_>>());
        prop_assert_eq!(t.items().collect::<Vec<_>>(), map.values().collect::<Vec<_>>());
        prop_assert_eq!(t.iter().map(|(k, _)| k).collect::<Vec<_>>(), map.keys().copied().collect::<Vec<_>>());
        prop_assert_eq!(t.ids().rev().collect::<Vec<_>>(), map.keys().rev().copied().collect::<Vec<_>>());
        let in_f: Vec<&MailItem> = map.values().filter(|m| m.folder == f).collect();
        prop_assert_eq!(t.in_folder(f).collect::<Vec<_>>(), in_f.clone());
        prop_assert_eq!(t.ids_in(f), in_f.iter().map(|m| m.id).collect::<Vec<_>>());
        let unread = in_f.iter().filter(|m| !m.read).count();
        prop_assert_eq!(t.counts(f), FolderCounts { total: in_f.len(), unread });
        prop_assert_eq!(t.all_counts().get(&f).copied().unwrap_or_default(), t.counts(f));
        for msgid in ["id1@example.net", "<ID2@example.net>"] {
            let key = crate::message_id_key(msgid);
            let want: Vec<ItemId> = map
                .values()
                .filter(|m| m.message_id.as_deref().map(crate::message_id_key) == Some(key.clone()))
                .map(|m| m.id)
                .collect();
            prop_assert_eq!(t.by_message_id(msgid), want);
        }
        let want_remote = in_f.iter().find(|m| m.remote_id.as_deref() == Some("0")).map(|m| m.id);
        prop_assert_eq!(t.by_remote_id(f, "0"), want_remote);
        let mut old: Vec<(i64, ItemId)> =
            in_f.iter().filter(|m| m.received.unix < cutoff).map(|m| (m.received.unix, m.id)).collect();
        old.sort();
        let want_old: Vec<ItemId> = old.into_iter().take(limit).map(|(_, id)| id).collect();
        prop_assert_eq!(t.older_than(f, cutoff, limit), want_old);
        prop_assert_eq!(serde_json::to_vec(&t).unwrap(), serde_json::to_vec(&map).unwrap());
    }
}
