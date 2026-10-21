//! Find and Replace › More, and Sort Text, through their commands.

use serde_json::{Value, json};

use crate::Session;
use crate::cmd::compose::tests::{open, session};
use crate::cmd::insert::tests::last_dialog;
use mailcraft_model::{Block, Inline};

fn with_text(text: &str) -> (Session, u64) {
    let mut s = session();
    let id = open(&mut s);
    s.run("compose.insertText", &json!({"text": text})).unwrap();
    (s, id)
}

fn caret(s: &mut Session, path: &[usize], offset: usize) {
    s.run(
        "compose.select",
        &json!({"anchor": {"path": path, "offset": offset}, "focus": {"path": path, "offset": offset}}),
    )
    .unwrap();
}

fn text(s: &Session, id: u64) -> String {
    s.composes[&id].body.to_plain_text()
}

fn sel(s: &Session, id: u64) -> (usize, usize) {
    let c = &s.composes[&id];
    (c.selection.start().offset, c.selection.end().offset)
}

#[test]
fn whole_words_wildcards_and_direction() {
    let (mut s, id) = with_text("cat concat Cat cot");
    caret(&mut s, &[0], 0);
    let r = s.run("format.find", &json!({"text": "cat", "wholeWord": true, "matchCase": true})).unwrap();
    assert_eq!(r["found"], true);
    assert_eq!(sel(&s, id), (0, 3));
    // The next whole word "cat" with Match case is the first one again (wrapping round).
    s.run("format.find", &json!({"text": "cat", "wholeWord": true, "matchCase": true})).unwrap();
    assert_eq!(sel(&s, id), (0, 3));
    // Wildcards: c?t inside "concat", then (case-sensitive, so not "Cat") "cot".
    s.run("format.find", &json!({"text": "c?t", "wildcards": true})).unwrap();
    assert_eq!(sel(&s, id), (7, 10));
    s.run("format.find", &json!({"text": "c?t", "wildcards": true})).unwrap();
    assert_eq!(sel(&s, id), (15, 18));
    // Up goes back, then on from the end.
    caret(&mut s, &[0], 7);
    s.run("format.find", &json!({"text": "cat", "direction": "up"})).unwrap();
    assert_eq!(sel(&s, id), (0, 3));
    s.run("format.find", &json!({"text": "cat", "direction": "up"})).unwrap();
    assert_eq!(sel(&s, id), (11, 14));
    s.run("format.find", &json!({"text": "cat", "direction": "up"})).unwrap();
    assert_eq!(sel(&s, id), (7, 10));
    assert!(s.run("format.find", &json!({"text": "[a", "wildcards": true})).is_err());
    assert!(s.run("format.find", &json!({"text": "x", "direction": "sideways"})).is_err());
    assert_eq!(s.run("format.find", &json!({"text": ""})).unwrap()["found"], false);
}

#[test]
fn find_in_highlight_all_and_format() {
    let (mut s, id) = with_text("one two one three one");
    let r = s.run("format.find", &json!({"text": "one", "findIn": "main"})).unwrap();
    assert_eq!(r["count"], 3);
    assert_eq!(s.composes[&id].extra_ranges.len(), 2);
    let r = s.run("format.find", &json!({"text": "one", "highlightAll": true})).unwrap();
    assert_eq!(r["highlighted"], 3);
    assert_eq!(s.composes[&id].find_marks.len(), 3);
    s.run("format.find", &json!({"highlightAll": false})).unwrap();
    assert!(s.composes[&id].find_marks.is_empty());
    // Format: only the bold "one".
    s.run(
        "compose.select",
        &json!({"anchor": {"path": [0], "offset": 8}, "focus": {"path": [0], "offset": 11}}),
    )
    .unwrap();
    s.run("format.bold", &json!({})).unwrap();
    caret(&mut s, &[0], 0);
    let r =
        s.run("format.find", &json!({"text": "one", "findIn": "main", "format": {"bold": true}})).unwrap();
    assert_eq!(r["count"], 1);
    assert_eq!(sel(&s, id), (8, 11));
    assert!(s.run("format.find", &json!({"text": "one", "format": {"bold": "yes"}})).is_err());
    // Find In › Current Selection.
    s.run(
        "compose.select",
        &json!({"anchor": {"path": [0], "offset": 0}, "focus": {"path": [0], "offset": 12}}),
    )
    .unwrap();
    assert_eq!(s.run("format.find", &json!({"text": "one", "findIn": "selection"})).unwrap()["count"], 2);
    // The dialog opens with the last text.
    s.run("format.find", &json!({})).unwrap();
    assert_eq!(last_dialog(&s)["text"], "one");
}

#[test]
fn replace_with_specials_format_and_selection() {
    let (mut s, id) = with_text("a1 b22 c333");
    let r = s.run("format.replace", &json!({"find": "^#", "replace": "[^&]", "all": true})).unwrap();
    assert_eq!(r["replaced"], 6);
    assert_eq!(text(&s, id), "a[1] b[2][2] c[3][3][3]");
    s.run("format.replace", &json!({"find": "] b", "replace": "]^lb", "all": true})).unwrap();
    // ^l is a line break inside the paragraph, not a new paragraph.
    assert_eq!(s.composes[&id].body.blocks.len(), 1);
    match &s.composes[&id].body.blocks[0] {
        Block::Paragraph { inlines, .. } => assert!(inlines.iter().any(|i| matches!(i, Inline::LineBreak))),
        other => panic!("{other:?}"),
    }
    // Replace formatting: the replaced words come out italic.
    let (mut s, id) = with_text("red green red");
    s.run(
        "format.replace",
        &json!({"find": "red", "replace": "blue", "all": true, "replaceFormat": {"italic": true}}),
    )
    .unwrap();
    assert_eq!(text(&s, id), "blue green blue");
    let italic: Vec<bool> = match &s.composes[&id].body.blocks[0] {
        Block::Paragraph { inlines, .. } => inlines
            .iter()
            .filter_map(|i| match i {
                Inline::Text { text, style } => Some((text.clone(), style.italic == Some(true))),
                _ => None,
            })
            .map(|(_, it)| it)
            .collect(),
        _ => Vec::new(),
    };
    assert_eq!(italic, vec![true, false, true]);
    // Replace All within the selection only.
    let (mut s, id) = with_text("x x x x");
    s.run(
        "compose.select",
        &json!({"anchor": {"path": [0], "offset": 0}, "focus": {"path": [0], "offset": 3}}),
    )
    .unwrap();
    s.run("format.replace", &json!({"find": "x", "replace": "yy", "all": true, "inSelection": true}))
        .unwrap();
    assert_eq!(text(&s, id), "yy yy x x");
    // Replace one by one with whole words.
    let (mut s, id) = with_text("art part art");
    caret(&mut s, &[0], 0);
    s.run("format.replace", &json!({"find": "art", "replace": "ART", "wholeWord": true})).unwrap();
    s.run("format.replace", &json!({"find": "art", "replace": "ART", "wholeWord": true})).unwrap();
    s.run("format.replace", &json!({"find": "art", "replace": "ART", "wholeWord": true})).unwrap();
    assert_eq!(text(&s, id), "ART part ART");
    assert!(s.run("format.replace", &json!({"find": "a", "replace": "^p", "all": true})).is_err());
    // One undo step for Replace All.
    s.run("compose.editUndo", &json!({})).unwrap();
    assert_ne!(text(&s, id), "ART part ART");
}

#[test]
fn sort_text_dialog_fields_types_and_header() {
    let (mut s, id) = with_text("Name\tAge\nZoe\t9\nAnn\t30\nBob\t4");
    s.run("compose.select", &json!({"all": true})).unwrap();
    assert_eq!(s.run("format.sort", &json!({})).unwrap()["dialog"], "sortText");
    let d = last_dialog(&s);
    assert_eq!(
        (d["target"].as_str(), d["fields"].as_u64(), d["separator"].as_str()),
        (Some("paragraphs"), Some(2), Some("tab"))
    );
    // By field 2 as numbers, descending, keeping the header row.
    s.run(
        "format.sort",
        &json!({"by": "field", "field": 2, "type": "number", "order": "descending", "header": true}),
    )
    .unwrap();
    assert_eq!(text(&s, id), "Name\tAge\nAnn\t30\nZoe\t9\nBob\t4");
    // By the whole paragraph as text.
    s.run("compose.select", &json!({"all": true})).unwrap();
    s.run("format.sort", &json!({"order": "ascending", "header": true})).unwrap();
    assert_eq!(text(&s, id), "Name\tAge\nAnn\t30\nBob\t4\nZoe\t9");
    // Dates.
    let (mut s, id) = with_text("Party March 3, 2026\nLaunch 2025-12-01\nReview 01/15/2026");
    s.run("compose.select", &json!({"all": true})).unwrap();
    s.run("format.sort", &json!({"type": "date"})).unwrap();
    assert_eq!(text(&s, id), "Launch 2025-12-01\nReview 01/15/2026\nParty March 3, 2026");
    // Commas, case.
    let (mut s, id) = with_text("b,2\nB,1\na,3");
    s.run("compose.select", &json!({"all": true})).unwrap();
    s.run("format.sort", &json!({"separator": "comma", "field": 2})).unwrap();
    assert_eq!(text(&s, id), "B,1\nb,2\na,3");
    for bad in [
        json!({"type": "colour"}),
        json!({"field": 0}),
        json!({"by": "shape"}),
        json!({"separator": "ab"}),
        json!({"order": "sideways"}),
    ] {
        assert!(s.run("format.sort", &bad).is_err(), "{bad}");
    }
}

#[test]
fn sort_text_then_by_breaks_ties() {
    let (mut s, id) = with_text("Smith,Anna,40\nJones,Bob,25\nSmith,Carl,31\nSmith,Anna,22");
    s.run("compose.select", &json!({"all": true})).unwrap();
    s.run(
        "format.sort",
        &json!({"separator": "comma", "field": 1, "thenBy": [{"field": 2}, {"field": 3, "type": "number", "order": "descending"}]}),
    )
    .unwrap();
    assert_eq!(text(&s, id), "Jones,Bob,25\nSmith,Anna,40\nSmith,Anna,22\nSmith,Carl,31");
    // Table rows too: by column 1, then column 2 as numbers.
    let (mut s, _) = with_text("x");
    s.run("insert.table", &json!({"rows": 3, "cols": 2})).unwrap();
    for bad in [
        json!({"thenBy": "field"}),
        json!({"thenBy": [{"field": 0}]}),
        json!({"thenBy": [{"type": "colour"}]}),
        json!({"thenBy": [{"order": "up"}]}),
        json!({"thenBy": [{}, {}, {}]}),
    ] {
        assert!(s.run("format.sort", &bad).is_err(), "{bad}");
    }
}

#[test]
fn sort_a_table_by_a_column() {
    let (mut s, id) = with_text("");
    s.run("insert.table", &json!({"rows": 3, "cols": 2})).unwrap();
    let fill = |s: &mut Session, r: usize, c: usize, t: &str| {
        let table = s.composes[&id].table_at_caret().unwrap();
        let mut path = table.clone();
        path.extend([r, c, 0]);
        s.run(
            "compose.select",
            &json!({"anchor": {"path": path, "offset": 0}, "focus": {"path": path, "offset": 0}}),
        )
        .unwrap();
        s.run("compose.insertText", &json!({"text": t})).unwrap();
    };
    for (r, row) in [["Item", "Qty"], ["pear", "12"], ["fig", "3"]].iter().enumerate() {
        for (c, t) in row.iter().enumerate() {
            fill(&mut s, r, c, t);
        }
    }
    assert_eq!(s.run("format.sort", &json!({})).unwrap()["dialog"], "sortText");
    assert_eq!(last_dialog(&s)["target"], "table");
    s.run("format.sort", &json!({"by": "column", "field": 2, "type": "number", "header": true})).unwrap();
    let rows: Vec<String> = s.composes[&id]
        .body
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Table { rows, .. } => Some(
                rows.iter()
                    .map(|r| {
                        r.iter()
                            .map(|c| {
                                mailcraft_model::Doc { blocks: c.blocks.clone(), ..Default::default() }
                                    .to_plain_text()
                            })
                            .collect::<Vec<_>>()
                            .join("|")
                    })
                    .collect(),
            ),
            _ => None,
        })
        .unwrap();
    assert_eq!(rows, vec!["Item|Qty", "fig|3", "pear|12"]);
}

#[test]
fn junk_find_and_sort_params_never_panic() {
    let (mut s, _) = with_text("abc abc\nxyz");
    let junk: Vec<Value> = vec![
        json!({"text": "*****************", "wildcards": true}),
        json!({"text": "<>", "wildcards": true}),
        json!({"text": "[!-]", "wildcards": true}),
        json!({"text": "\u{0}^", "wildcards": false}),
        json!({"text": "a", "format": 5}),
        json!({"text": "a", "format": {"size": -1}}),
        json!({"text": "a", "findIn": 3}),
        json!({"text": "a", "highlightAll": "yes"}),
    ];
    for p in &junk {
        let _ = s.run("format.find", p);
        let _ = s.run(
            "format.replace",
            &json!({"find": p.get("text"), "replace": "^&^&", "all": true, "wildcards": p.get("wildcards")}),
        );
    }
    for p in [json!({"field": 999}), json!({"field": "x"}), json!({"header": 1, "descending": "no"})] {
        let _ = s.run("format.sort", &p);
    }
}

#[test]
fn go_to_lines_next_previous_and_relative() {
    let (mut s, id) = with_text("one\ntwo\nthree");
    // A line break inside a paragraph starts a line too.
    if let Some(mailcraft_model::Block::Paragraph { inlines, .. }) =
        s.composes.get_mut(&id).unwrap().body.blocks.get_mut(0)
    {
        inlines.push(mailcraft_model::Inline::LineBreak);
        inlines.push(mailcraft_model::Inline::Text { text: "more".into(), style: Default::default() });
    }
    let lines = s.composes[&id].line_starts();
    assert_eq!(lines.len(), 4, "{lines:?}");
    let r = s.run("format.goTo", &json!({"target": "line:2"})).unwrap();
    assert_eq!(r["caret"], json!({"path": [0], "offset": 4}));
    let r = s.run("format.goTo", &json!({"target": "line:3"})).unwrap();
    assert_eq!(r["caret"], json!({"path": [1], "offset": 0}));
    // Next / Previous paragraph from the cursor.
    let r = s.run("format.goTo", &json!({"target": "next:paragraph"})).unwrap();
    assert_eq!(r["caret"]["path"], json!([2]));
    let r = s.run("format.goTo", &json!({"target": "previous:paragraph"})).unwrap();
    assert_eq!(r["caret"]["path"], json!([1]));
    // +n / -n.
    let r = s.run("format.goTo", &json!({"target": "paragraph:+1"})).unwrap();
    assert_eq!(r["caret"]["path"], json!([2]));
    let r = s.run("format.goTo", &json!({"target": "paragraph:-2"})).unwrap();
    assert_eq!(r["caret"]["path"], json!([0]));
    assert!(
        s.run("format.goTo", &json!({"target": "previous:paragraph"})).is_err(),
        "nothing before the first"
    );
    assert!(s.run("format.goTo", &json!({"target": "paragraph:-5"})).is_err());
    assert!(s.run("format.goTo", &json!({"target": "next:table"})).is_err());
    assert!(s.run("format.goTo", &json!({"target": "next:widget"})).is_err());
}
