use proptest::prelude::*;

use crate::rich::{ListKind, TableCell};
use crate::{Block, Doc, Inline, ParaStyle, TextStyle};

fn text(s: &str) -> Inline {
    Inline::Text { text: s.to_string(), style: TextStyle::default() }
}

fn para(s: &str) -> Block {
    Block::text_paragraph(s)
}

fn cell(s: &str) -> TableCell {
    TableCell { blocks: vec![para(s)], colspan: 1, rowspan: 1, background: None, width_px: None }
}

#[test]
fn plain_text_one_paragraph_per_line() {
    let d = Doc::from_plain_text("Hello\nWorld\n\nBye");
    assert_eq!(d.blocks.len(), 4);
    assert_eq!(d.blocks[0], para("Hello"));
    assert_eq!(d.blocks[2], Block::Paragraph { style: ParaStyle::default(), inlines: vec![] });
    assert_eq!(d.to_plain_text(), "Hello\nWorld\n\nBye");
}

#[test]
fn plain_text_line_endings() {
    let d = Doc::from_plain_text("a\r\nb\rc\n");
    assert_eq!(d.blocks.len(), 3);
    assert_eq!(d.to_plain_text(), "a\nb\nc");
    assert!(Doc::from_plain_text("").is_empty());
    let one_empty = Doc::from_plain_text("\n");
    assert_eq!(one_empty.blocks.len(), 1);
    assert_eq!(Doc::from_plain_text("\n\n").blocks.len(), 2);
}

#[test]
fn urls_stay_text() {
    let d = Doc::from_plain_text("see https://example.com now");
    assert_eq!(d.blocks, vec![para("see https://example.com now")]);
}

#[test]
fn lists_render_with_markers() {
    let d = Doc {
        blocks: vec![
            Block::List { kind: ListKind::Bullet, items: vec![vec![para("one")], vec![para("two")]] },
            Block::List {
                kind: ListKind::Numbered { start: 3 },
                items: vec![vec![para("three")], vec![para("four"), para("more")]],
            },
        ],
        ..Default::default()
    };
    assert_eq!(d.to_plain_text(), "\u{2022} one\n\u{2022} two\n3. three\n4. four\n   more");
}

#[test]
fn nested_lists_indent() {
    let inner = Block::List { kind: ListKind::Bullet, items: vec![vec![para("inner")]] };
    let d = Doc {
        blocks: vec![Block::List {
            kind: ListKind::Numbered { start: 1 },
            items: vec![vec![para("outer"), inner]],
        }],
        ..Default::default()
    };
    assert_eq!(d.to_plain_text(), "1. outer\n   \u{2022} inner");
}

#[test]
fn empty_list_item_still_has_marker() {
    let d = Doc {
        blocks: vec![Block::List { kind: ListKind::Bullet, items: vec![vec![]] }],
        ..Default::default()
    };
    assert_eq!(d.to_plain_text(), "\u{2022} ");
}

#[test]
fn tables_rules_quotes_pre() {
    let d = Doc {
        blocks: vec![
            Block::Table {
                borders: None,
                rows: vec![vec![cell("Name"), cell("Qty")], vec![cell("Apples"), cell("3")]],
                border: true,
                width_px: None,
            },
            Block::Rule,
            Block::Quote { blocks: vec![para("quoted")] },
            Block::Pre { text: "  code\r\n  more".into() },
            Block::Container { style: ParaStyle::default(), blocks: vec![para("boxed")] },
        ],
        ..Default::default()
    };
    let want = format!("Name\tQty\nApples\t3\n{}\nquoted\n  code\n  more\nboxed", "-".repeat(30));
    assert_eq!(d.to_plain_text(), want);
}

#[test]
fn table_cell_with_tabs_and_paragraphs_flattens() {
    let c = TableCell {
        blocks: vec![para("a\tb"), para("c")],
        colspan: 1,
        rowspan: 1,
        background: None,
        width_px: None,
    };
    let d = Doc {
        blocks: vec![Block::Table {
            borders: None,
            rows: vec![vec![c, cell("d")]],
            border: false,
            width_px: None,
        }],
        ..Default::default()
    };
    assert_eq!(d.to_plain_text(), "a b c\td");
}

#[test]
fn inline_kinds() {
    let d = Doc {
        blocks: vec![
            Block::Paragraph {
                style: ParaStyle::default(),
                inlines: vec![
                    text("Go to "),
                    Inline::Link {
                        title: None,
                        href: "https://a.example".into(),
                        children: vec![text("our site")],
                    },
                    text(" or "),
                    Inline::Link { title: None, href: "https://b.example".into(), children: vec![] },
                    Inline::LineBreak,
                    Inline::Image { src: "cid:x".into(), alt: "[logo]".into(), width: None, height: None },
                ],
            },
            Block::Heading { level: 1, style: ParaStyle::default(), inlines: vec![text("Title")] },
        ],
        ..Default::default()
    };
    assert_eq!(d.to_plain_text(), "Go to our site or https://b.example\n[logo]\nTitle");
    assert_eq!(d.text_content(), "Go to our site or https://b.example [logo] Title");
}

#[test]
fn deep_nesting_is_capped() {
    let mut b = para("deep");
    for _ in 0..500 {
        b = Block::Quote { blocks: vec![b] };
    }
    let d = Doc { blocks: vec![b], ..Default::default() };
    // Content below the cap is dropped rather than overflowing the stack.
    assert_eq!(d.to_plain_text(), "");
    let mut shallow = para("ok");
    for _ in 0..10 {
        shallow = Block::Quote { blocks: vec![shallow] };
    }
    assert_eq!(Doc { blocks: vec![shallow], ..Default::default() }.to_plain_text(), "ok");
}

proptest! {
    #[test]
    fn plain_text_round_trip(s in "[^\r]{0,200}") {
        let d = Doc::from_plain_text(&s);
        let want = s.strip_suffix('\n').unwrap_or(&s);
        prop_assert_eq!(d.to_plain_text(), want);
    }

    #[test]
    fn any_text_never_panics(s in any::<String>()) {
        let d = Doc::from_plain_text(&s);
        let _ = d.to_plain_text();
        let _ = d.text_content();
    }
}
