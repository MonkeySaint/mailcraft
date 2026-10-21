//! Paragraph and table borders, bookmarks (anchors) and link ScreenTips through HTML.

use mailcraft_model::{BorderEdge, BorderLine, ParaBorders, TableBorders};

use super::*;

fn text(s: &str) -> Inline {
    Inline::Text { text: s.into(), style: TextStyle::default() }
}

fn para(style: ParaStyle, inlines: Vec<Inline>) -> Block {
    Block::Paragraph { style, inlines }
}

fn round_trip(blocks: Vec<Block>) -> (String, Doc) {
    let d = Doc { blocks, ..Doc::default() };
    let html = doc_to_html(&d);
    let back = html_to_doc(&html);
    (html, back)
}

fn edge(line: BorderLine, color: Option<Color>, pt: f32) -> BorderEdge {
    BorderEdge::new(line, color, pt)
}

fn styles(d: &Doc) -> Vec<ParaStyle> {
    d.blocks
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph { style, .. } | Block::Heading { style, .. } => Some(style.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn paragraph_borders_round_trip() {
    let red = Color::rgb(0xc0, 0, 0);
    let borders = ParaBorders {
        between: None,
        top: Some(edge(BorderLine::Dashed, Some(red), 1.5)),
        bottom: Some(edge(BorderLine::Double, None, 0.75)),
        left: Some(edge(BorderLine::Single, Some(red), 3.0)),
        right: Some(edge(BorderLine::Groove, Some(Color::rgb(0, 0x70, 0xc0)), 2.25)),
        shadow: true,
    };
    let style = ParaStyle {
        indent_px: 24.0,
        right_indent_px: 12.0,
        space_before_px: 6.0,
        space_after_px: 8.0,
        borders: Some(Box::new(borders)),
        ..ParaStyle::default()
    };
    let (html, back) = round_trip(vec![para(style.clone(), vec![text("Boxed")])]);
    assert!(html.contains("border-top:2px dashed #c00000"), "{html}");
    assert!(html.contains("border-bottom:3px double"), "{html}");
    assert!(html.contains("box-shadow"), "{html}");
    assert_eq!(styles(&back), vec![style]);
}

#[test]
fn headings_keep_their_borders() {
    let style = ParaStyle {
        borders: Some(Box::new(ParaBorders {
            bottom: Some(edge(BorderLine::Single, Some(Color::rgb(0x44, 0x72, 0xc4)), 1.0)),
            ..ParaBorders::default()
        })),
        space_before_px: 12.0,
        space_after_px: 4.0,
        ..ParaStyle::default()
    };
    let (html, back) =
        round_trip(vec![Block::Heading { level: 2, style: style.clone(), inlines: vec![text("Title")] }]);
    assert!(html.contains("<h2 style=") && html.contains("border-bottom:"), "{html}");
    assert_eq!(styles(&back), vec![style]);
}

#[test]
fn paragraphs_sharing_a_box_round_trip() {
    let style = ParaStyle {
        space_before_px: 4.0,
        space_after_px: 10.0,
        borders: Some(Box::new(ParaBorders::boxed(&edge(BorderLine::Single, None, 0.5)))),
        ..ParaStyle::default()
    };
    let blocks: Vec<Block> =
        ["one", "two", "three"].iter().map(|t| para(style.clone(), vec![text(t)])).collect();
    let (html, back) = round_trip(blocks);
    // One top line and one bottom line for the three paragraphs; the others are kept aside.
    assert_eq!(html.matches("border-top:").count(), 1, "{html}");
    assert_eq!(html.matches("border-bottom:").count(), 1, "{html}");
    assert_eq!(html.matches("mso-border-top-alt:").count(), 2, "{html}");
    assert_eq!(styles(&back), vec![style.clone(), style.clone(), style]);
}

#[test]
fn lines_between_paragraphs_round_trip() {
    // A between line only (no box), and a box with a between line and no bottom.
    for borders in [
        ParaBorders { between: Some(edge(BorderLine::Dashed, None, 0.75)), ..ParaBorders::default() },
        ParaBorders {
            top: Some(edge(BorderLine::Single, None, 1.5)),
            left: Some(edge(BorderLine::Single, None, 1.5)),
            right: Some(edge(BorderLine::Single, None, 1.5)),
            between: Some(edge(BorderLine::Dotted, Some(Color::rgb(9, 9, 9)), 0.5)),
            ..ParaBorders::default()
        },
    ] {
        let style =
            ParaStyle { space_after_px: 6.0, borders: Some(Box::new(borders)), ..ParaStyle::default() };
        let blocks: Vec<Block> = ["a", "b", "c"].iter().map(|t| para(style.clone(), vec![text(t)])).collect();
        let (html, back) = round_trip(blocks);
        assert_eq!(html.matches("mso-border-between").count(), 3, "{html}");
        assert_eq!(styles(&back), vec![style.clone(), style.clone(), style], "{html}");
    }
}

#[test]
fn foreign_paragraph_borders_are_read() {
    let d = doc(r#"<p style="border:1px solid #cccccc;padding:4px 8px">a</p>
           <p style="border-left:thick double red;border-left-width:6px">b</p>
           <p style="border-bottom-style:dotted;border-bottom-color:#00f">c</p>
           <p style="border:0;padding:0">d</p>"#);
    let s = styles(&d);
    let a = s[0].borders.as_deref().unwrap();
    assert!(a.top.is_some() && a.bottom.is_some() && a.left.is_some() && a.right.is_some());
    assert_eq!(a.top.as_ref().unwrap().width_pt, 0.75);
    assert_eq!(a.top.as_ref().unwrap().color, Some(Color::rgb(0xcc, 0xcc, 0xcc)));
    // The padding past the border padding becomes indent and space.
    assert_eq!(s[0].indent_px, 3.0);
    let b = s[1].borders.as_deref().unwrap();
    assert_eq!(b.left.as_ref().map(|e| (e.line, e.width_pt)), Some((BorderLine::Double, 1.5)));
    assert!(b.top.is_none());
    let c = s[2].borders.as_deref().unwrap();
    assert_eq!(
        c.bottom.as_ref().map(|e| (e.line, e.color)),
        Some((BorderLine::Dotted, Some(Color::rgb(0, 0, 255))))
    );
    assert!(s[3].borders.is_none());
}

#[test]
fn hostile_border_values_are_contained() {
    let d = doc(
        r#"<p style="border:99999px solid red">a</p><p style="border-width:NaNpx;border-style:solid">b</p>
           <p style="border:solid solid solid;border-color:rgb(1,2,3) x y z w">c</p>
           <p style="border-top:-5px dotted">d</p><p style="mso-border-top-alt:1px bogus">e</p>"#,
    );
    for st in styles(&d) {
        if let Some(b) = st.borders.as_deref() {
            for e in [&b.top, &b.bottom, &b.left, &b.right].into_iter().flatten() {
                assert!(e.width_pt.is_finite() && e.width_pt <= BorderEdge::MAX_WIDTH_PT);
            }
        }
    }
    let _ = doc_to_html(&d);
}

#[test]
fn table_borders_round_trip() {
    let grey = Color::rgb(0x80, 0x80, 0x80);
    let borders = TableBorders {
        top: Some(edge(BorderLine::Single, Some(grey), 1.5)),
        bottom: Some(edge(BorderLine::Single, Some(grey), 1.5)),
        inside_h: Some(edge(BorderLine::Dotted, None, 0.5)),
        ..TableBorders::default()
    };
    let cell = |t: &str| TableCell {
        blocks: vec![para(ParaStyle::default(), vec![text(t)])],
        colspan: 1,
        rowspan: 1,
        background: None,
        width_px: None,
    };
    let table = Block::Table {
        rows: vec![vec![cell("a"), cell("b")], vec![cell("c"), cell("d")], vec![cell("e"), cell("f")]],
        border: false,
        width_px: None,
        borders: Some(Box::new(borders.clone())),
    };
    let (html, back) = round_trip(vec![table]);
    assert!(!html.contains("border=\"1\""), "{html}");
    let got = back.blocks.iter().find_map(|b| match b {
        Block::Table { borders, border, .. } => Some((borders.clone(), *border)),
        _ => None,
    });
    assert_eq!(got, Some((Some(Box::new(borders)), false)));
    // A plain grid stays a plain grid.
    let plain = Block::Table { rows: vec![vec![cell("x")]], border: true, width_px: None, borders: None };
    let (html, back) = round_trip(vec![plain]);
    assert!(html.contains("border=\"1\""), "{html}");
    assert!(matches!(&back.blocks[0], Block::Table { border: true, borders: None, .. }));
}

#[test]
fn bookmarks_are_written_as_anchors_and_read_back() {
    let blocks = vec![para(
        ParaStyle::default(),
        vec![text("Intro "), Inline::Anchor { name: "Details".into() }, text("here")],
    )];
    let (html, back) = round_trip(blocks);
    assert!(html.contains(r#"<a id="Details" name="Details"></a>"#), "{html}");
    let (_, inl) = first_para(&back);
    assert_eq!(inline_plain(inl), "Intro [#Details]here");
    // An anchor between blocks goes with the next paragraph instead of making an empty one.
    let d = doc(r#"<a name="top"></a><p>Hello</p><div><a id="end"></a></div>"#);
    assert_eq!(d.blocks.len(), 1, "{d:#?}");
    let (_, inl) = first_para(&d);
    assert_eq!(inline_plain(inl), "[#top]Hello");
    // Names are cleaned: no spaces or control characters, bounded length.
    let d = doc(&format!("<p>x<a name=\"a b\u{1}c\"></a><a id=\"{}\"></a></p>", "n".repeat(500)));
    let (_, inl) = first_para(&d);
    let names: Vec<&str> = inl
        .iter()
        .filter_map(|i| match i {
            Inline::Anchor { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names[0], "abc");
    assert_eq!(names[1].len(), mailcraft_model::rich::MAX_ANCHOR_NAME);
}

#[test]
fn link_screentips_round_trip() {
    let link = Inline::Link {
        href: "https://example.com/plan".into(),
        children: vec![text("the plan")],
        title: Some("Opens the \"plan\" page".into()),
    };
    let (html, back) = round_trip(vec![para(ParaStyle::default(), vec![link.clone()])]);
    assert!(html.contains("title=\"Opens the &quot;plan&quot; page\""), "{html}");
    let (_, inl) = first_para(&back);
    assert_eq!(inl, std::slice::from_ref(&link));
    // Two links to the same place with different tips stay apart.
    let d = doc(
        r#"<p><a href="https://example.com/" title="a">x</a><a href="https://example.com/" title="b">y</a></p>"#,
    );
    let (_, inl) = first_para(&d);
    assert_eq!(inl.len(), 2);
}
