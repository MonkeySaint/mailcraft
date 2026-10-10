//! Property tests for the query parser: it never panics, and its canonical text parses back to the same
//! tree.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use mailcraft_search::{Query, parse};
use proptest::prelude::*;

/// Query-ish fragments, so generated inputs exercise keys, operators, quotes and parentheses.
fn fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("AND".to_owned()),
        Just("OR".to_owned()),
        Just("NOT".to_owned()),
        Just("(".to_owned()),
        Just(")".to_owned()),
        Just("\"".to_owned()),
        Just("-".to_owned()),
        Just(":".to_owned()),
        Just("*".to_owned()),
        Just("..".to_owned()),
        Just(">=".to_owned()),
        Just("<".to_owned()),
        Just("me".to_owned()),
        Just("this".to_owned()),
        Just("week".to_owned()),
        Just("3".to_owned()),
        Just("days".to_owned()),
        Just("ago".to_owned()),
        Just("2026-10-09".to_owned()),
        Just("1MB".to_owned()),
        Just("Café".to_owned()),
        // Dates and sizes at the edges: ranges of multi-word dates, long numbers, big years.
        prop::sample::select(vec![
            "last",
            "month",
            "today",
            "ago..today",
            "week..",
            "..next",
            "1/1/",
            "99999",
            "999999999999999",
            "GB",
            "KB",
            "1.5",
        ])
        .prop_map(str::to_owned),
        prop::sample::select(vec![
            "from:",
            "to:",
            "cc:",
            "tocc:",
            "subject:",
            "body:",
            "attachments:",
            "category:",
            "folder:",
            "hasattachments:",
            "isread:",
            "isflagged:",
            "importance:",
            "received:",
            "sent:",
            "size:",
            "kind:",
            "expires:",
            "participants:",
            "foo:",
        ])
        .prop_map(str::to_owned),
        "[a-zA-Z0-9éß会@._-]{1,8}",
        " {1,3}",
    ]
}

fn query_text() -> impl Strategy<Value = String> {
    prop::collection::vec(fragment(), 0..24).prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2_000, ..ProptestConfig::default() })]

    #[test]
    fn structured_inputs_round_trip(text in query_text()) {
        let q = parse(&text);
        let shown = q.to_string();
        prop_assert_eq!(parse(&shown), q, "input {:?} shown {:?}", text, shown);
    }

    #[test]
    fn arbitrary_inputs_round_trip(text in "\\PC{0,64}") {
        let q = parse(&text);
        let shown = q.to_string();
        prop_assert_eq!(parse(&shown), q, "input {:?} shown {:?}", text, shown);
    }

    #[test]
    fn any_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let text = String::from_utf8_lossy(&bytes);
        let _ = parse(&text).to_string();
    }
}

#[test]
fn normalised_shapes() {
    // No All/Nothing inside compound nodes, no single-child compounds, no nested same-kind compounds.
    fn check(q: &Query) {
        match q {
            Query::And(v) | Query::Or(v) => {
                assert!(v.len() >= 2, "{q:?}");
                for c in v {
                    assert!(!matches!(c, Query::All | Query::Nothing), "{q:?}");
                    assert!(std::mem::discriminant(c) != std::mem::discriminant(q), "{q:?}");
                    check(c);
                }
            }
            Query::Not(c) => {
                assert!(!matches!(**c, Query::All | Query::Nothing | Query::Not(_)), "{q:?}");
                check(c);
            }
            _ => {}
        }
    }
    for text in ["a (b c) (d OR (e OR f)) NOT NOT g", "-(a) OR () x", "((a)) -(-b)", "a OR NOT ()"] {
        check(&parse(text));
    }
}
