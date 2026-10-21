//! Borders, anchors (bookmarks) and their helpers.

use crate::rich::{BorderJoin, TableCell, clean_anchor_name};
use crate::{Block, BorderEdge, BorderLine, Doc, Inline, ParaBorders, ParaStyle, TextStyle};

fn text(s: &str) -> Inline {
    Inline::Text { text: s.to_string(), style: TextStyle::default() }
}

fn anchors(inlines: &[Inline]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    fn walk(inlines: &[Inline], off: &mut usize, out: &mut Vec<(usize, String)>) {
        for i in inlines {
            match i {
                Inline::Text { text, .. } => *off += text.chars().count(),
                Inline::Link { children, .. } => walk(children, off, out),
                Inline::LineBreak | Inline::Image { .. } => *off += 1,
                Inline::Anchor { name } => out.push((*off, name.clone())),
            }
        }
    }
    walk(inlines, &mut off, &mut out);
    out
}

#[test]
fn border_names_widths_and_thickness() {
    assert_eq!(BorderLine::from_name("solid"), Some(BorderLine::Single));
    assert_eq!(BorderLine::from_name("DOUBLE"), Some(BorderLine::Double));
    assert_eq!(BorderLine::from_name("wavy"), None);
    for (l, name, css) in BorderLine::ALL {
        assert_eq!(l.name(), name);
        assert_eq!(l.css(), css);
    }
    assert_eq!(BorderEdge::new(BorderLine::Single, None, 99.0).width_pt, BorderEdge::MAX_WIDTH_PT);
    assert_eq!(BorderEdge::new(BorderLine::Single, None, f32::NAN).width_pt, BorderEdge::DEFAULT_WIDTH_PT);
    let e = BorderEdge::new(BorderLine::Double, None, 0.75);
    assert_eq!(e.thickness_px(), 3.0);
    // from_thickness undoes thickness_px for every width the dialog offers.
    for w in BorderEdge::WIDTHS_PT {
        for (l, _, _) in BorderLine::ALL {
            let e = BorderEdge::new(l, None, w);
            assert_eq!(BorderEdge::from_thickness(l, None, e.thickness_px()), e, "{l:?} {w}");
        }
    }
}

#[test]
fn paragraphs_with_equal_borders_join() {
    let boxed = ParaStyle {
        borders: Some(Box::new(ParaBorders::boxed(&BorderEdge::default()))),
        ..ParaStyle::default()
    };
    let p = |style: &ParaStyle| Block::Paragraph { style: style.clone(), inlines: vec![text("x")] };
    let indented = ParaStyle { indent_px: 10.0, ..boxed.clone() };
    let blocks = vec![p(&boxed), p(&boxed), p(&indented), p(&ParaStyle::default())];
    assert_eq!(BorderJoin::of(&blocks, 0), BorderJoin { above: false, below: true });
    assert_eq!(BorderJoin::of(&blocks, 1), BorderJoin { above: true, below: false });
    assert_eq!(BorderJoin::of(&blocks, 2), BorderJoin::default());
    assert_eq!(BorderJoin::of(&blocks, 3), BorderJoin::default());
    assert_eq!(BorderJoin::of(&blocks, 9), BorderJoin::default());
    // Empty borders are no borders.
    let empty = ParaStyle { borders: Some(Box::new(ParaBorders::default())), ..ParaStyle::default() };
    assert!(p(&empty).para_borders().is_none());
}

#[test]
fn anchors_go_in_and_come_out_at_their_offsets() {
    let link =
        Inline::Link { href: "https://example.com/".into(), children: vec![text("link")], title: None };
    let mut d = Doc {
        blocks: vec![
            Block::Paragraph {
                style: ParaStyle::default(),
                inlines: vec![text("Hello "), link, text(" end")],
            },
            Block::List {
                kind: crate::rich::ListKind::Bullet,
                items: vec![vec![Block::text_paragraph("item")]],
            },
            Block::Table {
                rows: vec![vec![TableCell {
                    blocks: vec![Block::text_paragraph("cell")],
                    colspan: 1,
                    rowspan: 1,
                    background: None,
                    width_px: None,
                }]],
                border: true,
                width_px: None,
                borders: None,
            },
        ],
        ..Doc::default()
    };
    assert!(d.insert_anchor(&[0], 0, "Start"));
    assert!(d.insert_anchor(&[0], 3, "Mid"));
    assert!(d.insert_anchor(&[0], 8, "InLink"));
    assert!(d.insert_anchor(&[0], 999, "End"));
    assert!(d.insert_anchor(&[1, 0, 0], 2, "Item"));
    assert!(d.insert_anchor(&[2, 0, 0, 0], 4, "Cell"));
    assert!(!d.insert_anchor(&[7], 0, "Nowhere"));
    assert!(!d.insert_anchor(&[0], 0, " \n"));
    let Block::Paragraph { inlines, .. } = &d.blocks[0] else { panic!() };
    assert_eq!(
        anchors(inlines),
        vec![(0, "Start".into()), (3, "Mid".into()), (8, "InLink".into()), (14, "End".into())]
    );
    // The text is untouched.
    assert_eq!(d.to_plain_text().lines().next(), Some("Hello link end"));
    let got = d.take_anchors();
    assert_eq!(
        got,
        vec![
            (vec![0], 0, "Start".to_owned()),
            (vec![0], 3, "Mid".to_owned()),
            (vec![0], 8, "InLink".to_owned()),
            (vec![0], 14, "End".to_owned()),
            (vec![1, 0, 0], 2, "Item".to_owned()),
            (vec![2, 0, 0, 0], 4, "Cell".to_owned()),
        ]
    );
    assert!(d.take_anchors().is_empty());
}

#[test]
fn anchor_names_are_cleaned() {
    assert_eq!(clean_anchor_name(" a b\tc "), Some("abc".into()));
    assert_eq!(clean_anchor_name("\u{0}\n"), None);
    assert_eq!(clean_anchor_name(&"x".repeat(1000)).map(|n| n.len()), Some(crate::rich::MAX_ANCHOR_NAME));
}
