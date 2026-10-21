//! The index answers every query exactly like direct evaluation of the query against each item.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use mailcraft_search::{Context, Doc, DocKind, Filter, Index, Level, Person, matches, parse};
use proptest::prelude::*;

const NOW: i64 = 1_791_547_200;
const WORDS: [&str; 12] =
    ["budget", "budgets", "review", "lunch", "café", "cafe", "plan", "résumé", "q3", "report", "ann", "lee"];

fn word() -> impl Strategy<Value = String> {
    prop::sample::select(WORDS.to_vec()).prop_map(str::to_owned)
}

fn text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(word(), 0..max).prop_map(|v| v.join(" "))
}

fn person() -> impl Strategy<Value = Person> {
    (
        prop::sample::select(vec!["", "Ann Lee", "Bob", "Café Owner"]),
        prop::sample::select(vec!["ann@example.com", "me@example.com", "bob.lee@example.org", ""]),
    )
        .prop_map(|(n, a)| Person::new(n, a))
}

fn doc() -> impl Strategy<Value = Doc> {
    (
        (0u64..4, text(4), prop::collection::vec(person(), 0..3), prop::collection::vec(person(), 0..3)),
        (
            text(10),
            prop::collection::vec(text(2), 0..2),
            prop::collection::vec(prop::sample::select(vec!["Red category", "Blue", "Plan"]), 0..2),
        ),
        (0i64..60, 0u64..3_000_000, any::<bool>(), any::<bool>(), 0u8..3, any::<bool>(), 0u8..2),
    )
        .prop_map(
            |(
                (folder, subject, from, to),
                (body, attachments, cats),
                (days, size, read, flagged, imp, has_att, kind),
            )| Doc {
                id: 0,
                kind: if kind == 0 { DocKind::Mail } else { DocKind::Note },
                folder,
                subject,
                from,
                to,
                cc: Vec::new(),
                body,
                attachments,
                categories: cats.into_iter().map(str::to_owned).collect(),
                received: NOW - days * 86_400 + 3_600,
                sent: if read { Some(NOW - days * 86_400) } else { None },
                size,
                read,
                flagged,
                importance: [Level::Low, Level::Normal, Level::High][usize::from(imp)],
                has_attachments: has_att,
                // Some expire (a few days back up to a few weeks ahead), the rest never.
                expires: flagged.then(|| NOW + (days - 10) * 86_400 + 1_800),
                props: vec![
                    ("sensitivity".to_owned(), if read { "Private" } else { "Normal" }.to_owned()),
                    ("duedate".to_owned(), format!("2026-10-{:02}", days % 28 + 1)),
                ],
                // Near midnight UTC, so the local day (UTC-4 in `ctx`) differs from the UTC one.
                date_props: vec![("received".to_owned(), NOW - days * 86_400 + 3_600)],
            },
        )
}

fn query() -> impl Strategy<Value = String> {
    let atom = prop_oneof![
        word(),
        word().prop_map(|w| w.chars().take(3).collect()),
        (word(), word()).prop_map(|(a, b)| format!("\"{a} {b}\"")),
        (
            prop::sample::select(vec![
                "from:",
                "to:",
                "subject:",
                "body:",
                "attachments:",
                "participants:",
                "tocc:"
            ]),
            word()
        )
            .prop_map(|(k, w)| format!("{k}{w}")),
        Just("from:me".to_owned()),
        Just("to:me".to_owned()),
        Just("from:example.com".to_owned()),
        Just("category:any".to_owned()),
        Just("category:none".to_owned()),
        Just("category:red".to_owned()),
        Just("folder:inbox".to_owned()),
        Just("hasattachments:yes".to_owned()),
        Just("isread:no".to_owned()),
        Just("isflagged:yes".to_owned()),
        Just("importance:high".to_owned()),
        Just("received:\"this week\"".to_owned()),
        Just("received:\"last month\"".to_owned()),
        Just("sent:<=yesterday".to_owned()),
        Just("received:>\"10 days ago\"".to_owned()),
        Just("size:large".to_owned()),
        Just("size:>1MB".to_owned()),
        Just("kind:note".to_owned()),
        Just("expires:soon".to_owned()),
        Just("expires:expired".to_owned()),
        Just("expires:no".to_owned()),
        Just("expires:<=tomorrow".to_owned()),
        Just("prop:\"Sensitivity=private\"".to_owned()),
        Just("prop:\"Due Date<2026-10-15\"".to_owned()),
        Just("prop:\"Received<2026-10-15\"".to_owned()),
        Just("prop:\"Received=2026-10-10\"".to_owned()),
        Just("prop:\"company!=\"".to_owned()),
    ];
    prop::collection::vec(
        (atom, prop::sample::select(vec![" ", " OR ", " -", " AND NOT ", " (", ") "])),
        1..6,
    )
    .prop_map(|v| v.into_iter().map(|(a, op)| format!("{a}{op}")).collect::<String>())
}

fn ctx() -> Context {
    Context {
        now: NOW,
        tz_offset_min: -240,
        me: vec!["ME@example.com".into()],
        folders: vec![(0, "Inbox".into()), (1, "Café plans".into()), (2, "Archive".into())],
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    #[test]
    fn index_equals_direct(docs in prop::collection::vec(doc(), 1..25), queries in prop::collection::vec(query(), 1..6), removals in prop::collection::vec(any::<prop::sample::Index>(), 0..4)) {
        let mut docs: Vec<Doc> = docs.into_iter().enumerate().map(|(i, d)| Doc { id: i as u64 * 7 + 1, ..d }).collect();
        let mut ix = Index::new();
        for d in &docs {
            ix.upsert(d);
        }
        // Replace and remove a few, so dead slots are in play.
        for r in &removals {
            let i = r.index(docs.len());
            if i % 2 == 0 {
                ix.remove(docs[i].id);
                docs.remove(i);
            } else {
                docs[i].subject.push_str(" lunch");
                ix.upsert(&docs[i]);
            }
            if docs.is_empty() {
                break;
            }
        }
        let c = ctx();
        for qt in &queries {
            let q = parse(qt);
            let mut want: Vec<u64> = docs.iter().filter(|d| matches(&q, d, &c)).map(|d| d.id).collect();
            want.sort_unstable();
            let mut got = ix.matching(&q, &c, &Filter::default());
            got.sort_unstable();
            prop_assert_eq!(&got, &want, "query {:?} parsed {}", qt, q);
            let hits = ix.search(&q, &c, &Filter::default());
            prop_assert_eq!(hits.len(), want.len());
            let unread = docs.iter().filter(|d| !d.read && matches(&q, d, &c)).count();
            prop_assert_eq!(ix.count(&q, &c, &Filter::default()).unread, unread);
        }
        ix.compact();
        for qt in &queries {
            let q = parse(qt);
            let mut want: Vec<u64> = docs.iter().filter(|d| matches(&q, d, &c)).map(|d| d.id).collect();
            want.sort_unstable();
            let mut got = ix.matching(&q, &c, &Filter::default());
            got.sort_unstable();
            prop_assert_eq!(got, want);
        }
    }
}
