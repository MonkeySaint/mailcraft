//! Style sets that capitalise, number or rule their headings.

use serde_json::json;

use crate::Session;
use crate::cmd::compose::tests::{open, session};
use mailcraft_model::{Block, Inline};

fn doc_with_headings() -> (Session, u64) {
    let mut s = session();
    let id = open(&mut s);
    s.run("compose.insertText", &json!({"text": "Intro\nbody\nDetails\n2026 Plans\nmore"})).unwrap();
    for (path, style) in [(0, "Heading 1"), (2, "Heading 2"), (3, "Heading 1")] {
        s.run(
            "compose.select",
            &json!({"anchor": {"path": [path], "offset": 0}, "focus": {"path": [path], "offset": 0}}),
        )
        .unwrap();
        s.run("format.style", &json!({"name": style})).unwrap();
    }
    (s, id)
}

fn headings(s: &Session, id: u64) -> Vec<(String, bool, bool)> {
    s.composes[&id]
        .body
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Heading { style, inlines, .. } => {
                let text: String = inlines
                    .iter()
                    .filter_map(|i| match i {
                        Inline::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                let caps = inlines.iter().all(|i| match i {
                    Inline::Text { style, .. } => style.all_caps == Some(true),
                    _ => true,
                });
                let ruled = style.borders.as_deref().is_some_and(|b| b.bottom.is_some());
                Some((text, caps, ruled))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn capitalized_numbered_and_lines_sets_differ_as_named() {
    let (mut s, id) = doc_with_headings();
    s.run("format.styleSet", &json!({"name": "Black & White (Capitalized)"})).unwrap();
    assert!(headings(&s, id).iter().all(|(_, caps, ruled)| *caps && !ruled));
    s.run("format.styleSet", &json!({"name": "Lines (Simple)"})).unwrap();
    assert!(headings(&s, id).iter().all(|(_, caps, ruled)| !*caps && *ruled));
    let html = mailcraft_html::doc_to_html(&s.composes[&id].body);
    assert!(html.contains("border-bottom:"), "{html}");
    s.run("format.styleSet", &json!({"name": "Black & White (Numbered)"})).unwrap();
    let h: Vec<String> = headings(&s, id)
        .into_iter()
        .map(|(t, _, ruled)| {
            assert!(!ruled, "another set takes the heading lines off");
            t
        })
        .collect();
    assert_eq!(h, vec!["1 Intro", "1.1 Details", "2 2026 Plans"]);
    // Applying it again renumbers instead of numbering twice.
    s.run("format.styleSet", &json!({"name": "Black & White (Numbered)"})).unwrap();
    let h: Vec<String> = headings(&s, id).into_iter().map(|(t, ..)| t).collect();
    assert_eq!(h, vec!["1 Intro", "1.1 Details", "2 2026 Plans"]);
    // Another set takes the numbers off, and a heading's own number stays.
    s.run("format.styleSet", &json!({"name": "Default"})).unwrap();
    let h: Vec<String> = headings(&s, id).into_iter().map(|(t, ..)| t).collect();
    assert_eq!(h, vec!["Intro", "Details", "2026 Plans"]);
}

#[test]
fn style_sets_take_off_only_the_heading_lines_they_drew() {
    // Regression: a Lines set replaced a heading's box with a bottom rule, and every other set (also run on
    // each theme change) removed any bottom-only border, the user's own included.
    use mailcraft_model::BorderLine;
    let (mut s, id) = doc_with_headings();
    let at = |s: &mut Session, path: usize| {
        s.run(
            "compose.select",
            &json!({"anchor": {"path": [path], "offset": 0}, "focus": {"path": [path], "offset": 0}}),
        )
        .unwrap();
    };
    // "Intro" gets a dotted bottom border of the user's, "Details" a box.
    at(&mut s, 0);
    s.run("format.borders", &json!({"kind": "bottom", "style": "dotted", "width": 1.5})).unwrap();
    at(&mut s, 2);
    s.run("format.borders", &json!({"kind": "outside"})).unwrap();
    let borders = |s: &Session| -> Vec<Option<mailcraft_model::ParaBorders>> {
        s.composes[&id]
            .body
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Heading { style, .. } => Some(style.borders.as_deref().cloned()),
                _ => None,
            })
            .collect()
    };
    let before = borders(&s);
    s.run("format.styleSet", &json!({"name": "Lines (Simple)"})).unwrap();
    let lined = borders(&s);
    // The user's borders stay; the plain heading gets the set's line.
    assert_eq!(lined[0], before[0]);
    assert_eq!(lined[1], before[1]);
    assert!(lined[2].as_ref().and_then(|b| b.bottom.as_ref()).is_some_and(|e| e.line == BorderLine::Single));
    // A theme change redraws the set's line in the new colours and leaves the user's borders alone.
    s.run("compose.theme", &json!({"part": "colors", "name": "Blue"})).unwrap();
    let themed = borders(&s);
    assert_eq!(themed[0], before[0]);
    assert_eq!(themed[1], before[1]);
    assert!(themed[2].as_ref().is_some_and(|b| b.bottom.is_some()));
    // Another set takes off its line only.
    s.run("format.styleSet", &json!({"name": "Default"})).unwrap();
    let after = borders(&s);
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1], before[1]);
    assert_eq!(after[2], None);
}
