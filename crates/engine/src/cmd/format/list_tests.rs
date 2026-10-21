//! Set Numbering Value › carry on from the list above, and the nine-level Multilevel List definitions.

use serde_json::json;

use crate::Session;
use crate::cmd::compose::tests::{open, session};
use crate::cmd::insert::tests::last_dialog;
use mailcraft_model::Block;
use mailcraft_model::rich::ListKind;

fn with_text(text: &str) -> (Session, u64) {
    let mut s = session();
    let id = open(&mut s);
    s.run("compose.insertText", &json!({"text": text})).unwrap();
    (s, id)
}

fn select(s: &mut Session, a: usize, b: usize) {
    s.run(
        "compose.select",
        &json!({"anchor": {"path": [a], "offset": 0}, "focus": {"path": [b], "offset": 0}}),
    )
    .unwrap();
}

fn starts(s: &Session, id: u64) -> Vec<u32> {
    s.composes[&id]
        .body
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::List { kind: ListKind::Numbered { start }, .. } => Some(*start),
            _ => None,
        })
        .collect()
}

#[test]
fn continue_from_the_previous_list() {
    let (mut s, id) = with_text("one\ntwo\nbetween\nthree");
    select(&mut s, 0, 1);
    s.run("format.numbering", &json!({})).unwrap();
    // The first list has nothing before it to continue.
    assert!(s.run("format.numbering", &json!({"continue": true})).is_err());
    let last = s.composes[&id].body.blocks.len() - 1;
    select(&mut s, last, last);
    s.run("format.numbering", &json!({})).unwrap();
    assert_eq!(starts(&s, id), vec![1, 1]);
    let r = s.run("format.numbering", &json!({"continue": true})).unwrap();
    assert_eq!(r["start"], 3);
    assert_eq!(starts(&s, id), vec![1, 3]);
    // Skip ahead: a number of one's own.
    s.run("format.numbering", &json!({"start": 7})).unwrap();
    assert_eq!(starts(&s, id), vec![1, 7]);
    // The HTML numbers on from there.
    assert!(mailcraft_html::doc_to_html(&s.composes[&id].body).contains("<ol start=\"7\">"));
}

#[test]
fn define_new_multilevel_list_has_nine_levels_and_the_plain_outline() {
    let (mut s, _) = with_text("item");
    s.run("format.multilevel", &json!({"style": "define"})).unwrap();
    let d = last_dialog(&s);
    assert_eq!(d["levels"].as_array().map(Vec::len), Some(9));
    assert_eq!(d["levels"][0]["pattern"], "1.");
    // All nine levels can be defined, each with its own style.
    let levels: Vec<serde_json::Value> = (0..9)
        .map(|i| {
            if i % 2 == 0 {
                json!({"pattern": format!("({})", i + 1), "numberStyle": "decimal"})
            } else {
                json!({"glyph": "\u{2013}"})
            }
        })
        .collect();
    let r = s.run("format.multilevel", &json!({"levels": levels})).unwrap();
    assert_eq!(r["list"], "numbered");
    s.run("format.multilevel", &json!({"style": "define"})).unwrap();
    let d = last_dialog(&s);
    assert_eq!(d["levels"][1]["glyph"], "\u{2013}");
    assert_eq!(d["levels"][8]["pattern"], "(1)");
    // "Number Format: 1" (no period) is the 1 / 1.1 / 1.1.1 list, not 1. a. i.
    let r = s
        .run(
            "format.multilevel",
            &json!({"style": "Outline numbered: L1: Numbered, 1, 2, 3, …, ALIGNMENT Left, Number Format: 1, Aligned at:  0"}),
        )
        .unwrap();
    assert_eq!(r["format"]["suffix"], "");
    assert_eq!(r["format"]["sample"], "1");
    let r = s
        .run(
            "format.multilevel",
            &json!({"style": "Outline numbered: L1: Numbered, 1, 2, 3, …, ALIGNMENT Left, Number Format: 1., Aligned at:  0"}),
        )
        .unwrap();
    assert_eq!(r["format"]["suffix"], ".");
    let ten: Vec<serde_json::Value> = (0..10).map(|_| json!({"pattern": "1."})).collect();
    assert!(s.run("format.multilevel", &json!({"levels": ten})).is_err());
}
