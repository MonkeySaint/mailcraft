//! Paragraph and table borders.

use mailcraft_model::{BorderEdge, BorderLine, ParaBorders, TableBorders};

use super::*;

const RED: Color = Color::rgb(200, 0, 0);

fn boxed(edge: &BorderEdge) -> ParaStyle {
    ParaStyle { borders: Some(Box::new(ParaBorders::boxed(edge))), ..Default::default() }
}

fn with_borders(b: ParaBorders) -> ParaStyle {
    ParaStyle { borders: Some(Box::new(b)), ..Default::default() }
}

fn lines_in(l: &Layout, color: Color) -> Vec<(Point, Point, f32)> {
    l.display
        .items
        .iter()
        .filter_map(|i| match i {
            DisplayItem::Line { from, to, color: c, width } if *c == color => Some((*from, *to, *width)),
            _ => None,
        })
        .collect()
}

fn single(width_pt: f32) -> BorderEdge {
    BorderEdge::new(BorderLine::Single, Some(RED), width_pt)
}

fn bpara(style: ParaStyle, s: &str) -> Block {
    Block::Paragraph { style, inlines: vec![text(s)] }
}

#[test]
fn paragraph_box_border_draws_four_sides_and_insets_the_text() {
    let plain = lay(&doc(vec![para_s("Boxed text")]), 400.0);
    let l = lay(&doc(vec![bpara(boxed(&single(1.5)), "Boxed text")]), 400.0);
    let lines = lines_in(&l, RED);
    assert_eq!(lines.len(), 4, "{lines:?}");
    let horizontal = lines.iter().filter(|(a, b, _)| a.y == b.y).count();
    assert_eq!(horizontal, 2);
    // 1.5 pt = 2 px lines.
    assert!(lines.iter().all(|(_, _, w)| (*w - 2.0).abs() < 1e-3));
    // The text moved right past the left line and its padding, and the paragraph got taller.
    let x0 = plain.text_runs().first().unwrap().rect.x;
    let x1 = l.text_runs().first().unwrap().rect.x;
    assert!(x1 >= x0 + 2.0 + ParaBorders::PAD_X_PX - 1e-3);
    assert!(l.height > plain.height);
    for (a, b, _) in &lines {
        assert!(a.x >= -1e-3 && b.x <= 400.0 + 1e-3);
    }
}

#[test]
fn bottom_border_only_draws_one_line_under_the_text() {
    let style = with_borders(ParaBorders { bottom: Some(single(0.5)), ..Default::default() });
    let l = lay(&doc(vec![bpara(style, "Over the line")]), 300.0);
    let lines = lines_in(&l, RED);
    assert_eq!(lines.len(), 1);
    let (a, b, _) = lines[0];
    assert_eq!(a.y, b.y);
    assert!(a.y > l.text_runs()[0].rect.bottom() - 1e-3);
    assert!((b.x - a.x - 300.0).abs() < 1e-3);
}

#[test]
fn borders_run_between_the_indents() {
    let mut style = boxed(&single(0.75));
    style.indent_px = 40.0;
    style.right_indent_px = 60.0;
    let l = lay(&doc(vec![bpara(style, "indented")]), 400.0);
    let lines = lines_in(&l, RED);
    let xs: Vec<f32> = lines.iter().flat_map(|(a, b, _)| [a.x, b.x]).collect();
    let min = xs.iter().copied().fold(f32::MAX, f32::min);
    let max = xs.iter().copied().fold(f32::MIN, f32::max);
    assert!(min >= 40.0 - 1e-3 && max <= 340.0 + 1e-3, "{min}..{max}");
}

#[test]
fn consecutive_paragraphs_with_equal_borders_share_one_box() {
    let style = boxed(&single(0.75));
    let d = doc(vec![bpara(style.clone(), "one"), bpara(style.clone(), "two"), bpara(style, "three")]);
    let l = lay(&d, 300.0);
    let lines = lines_in(&l, RED);
    let horizontal: Vec<_> = lines.iter().filter(|(a, b, _)| a.y == b.y).collect();
    assert_eq!(horizontal.len(), 2, "only the top and the bottom of the box: {horizontal:?}");
    // The left side runs without gaps from the top line to the bottom line.
    let mut left: Vec<(f32, f32)> =
        lines.iter().filter(|(a, b, _)| a.x == b.x && a.x < 150.0).map(|(a, b, _)| (a.y, b.y)).collect();
    left.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert_eq!(left.len(), 3);
    for w in left.windows(2) {
        assert!((w[0].1 - w[1].0).abs() < 1e-3, "gap in the side line: {left:?}");
    }
}

#[test]
fn lines_between_paragraphs_of_one_box() {
    let style = with_borders(ParaBorders { between: Some(single(0.75)), ..Default::default() });
    let d = doc(vec![bpara(style.clone(), "one"), bpara(style.clone(), "two"), bpara(style, "three")]);
    let l = lay(&d, 300.0);
    let lines = lines_in(&l, RED);
    // Two lines: under the first and the second paragraph, none under the last.
    assert_eq!(lines.len(), 2, "{lines:?}");
    let runs = l.text_runs();
    let (one, two, three) = (runs[0].rect, runs[1].rect, runs[2].rect);
    assert!(lines[0].0.y > one.bottom() - 1e-3 && lines[0].0.y < two.y + 1e-3);
    assert!(lines[1].0.y > two.bottom() - 1e-3 && lines[1].0.y < three.y + 1e-3);
}

#[test]
fn different_borders_do_not_join() {
    let d = doc(vec![bpara(boxed(&single(0.75)), "one"), bpara(boxed(&single(3.0)), "two")]);
    let l = lay(&d, 300.0);
    let horizontal = lines_in(&l, RED).iter().filter(|(a, b, _)| a.y == b.y).count();
    assert_eq!(horizontal, 4);
}

fn top_only(line: BorderLine, width_pt: f32) -> Layout {
    let e = BorderEdge::new(line, Some(RED), width_pt);
    lay(&doc(vec![bpara(with_borders(ParaBorders { top: Some(e), ..Default::default() }), "x")]), 300.0)
}

#[test]
fn dotted_dashed_double_and_3d_borders() {
    let count = |line| {
        let l = top_only(line, 1.0);
        lines_in(&l, RED).len()
    };
    assert_eq!(count(BorderLine::Single), 1);
    assert_eq!(count(BorderLine::Double), 2);
    assert!(count(BorderLine::Dotted) > 20);
    assert!(count(BorderLine::Dashed) > 10);
    assert!(count(BorderLine::Dotted) > count(BorderLine::Dashed));
    // The 3-D styles draw in shades of the colour.
    for line in [BorderLine::Groove, BorderLine::Ridge] {
        let l = top_only(line, 1.5);
        let shades: std::collections::HashSet<String> = l
            .display
            .items
            .iter()
            .filter_map(|i| match i {
                DisplayItem::Line { color, .. } => Some(color.to_hex()),
                _ => None,
            })
            .collect();
        assert_eq!(shades.len(), 2, "{line:?}");
    }
}

#[test]
fn shadow_adds_lines_right_and_below() {
    let mut b = ParaBorders::boxed(&single(0.75));
    let without = lay(&doc(vec![bpara(with_borders(b.clone()), "x")]), 300.0).display.items.len();
    b.shadow = true;
    let with = lay(&doc(vec![bpara(with_borders(b), "x")]), 300.0).display.items.len();
    assert_eq!(with, without + 2);
}

#[test]
fn hostile_border_widths_are_sanitised() {
    for w in [f32::NAN, f32::INFINITY, -5.0, 1.0e9] {
        let e = BorderEdge { line: BorderLine::Dotted, color: None, width_pt: w };
        let l = lay(&doc(vec![bpara(boxed(&e), "x")]), 1.0e6);
        assert!(l.height.is_finite());
        assert!(l.display.items.len() < 40_000);
    }
}

#[test]
fn table_per_edge_borders_replace_the_grid() {
    let rows = vec![
        vec![cell(vec![para_s("a")]), cell(vec![para_s("b")])],
        vec![cell(vec![para_s("c")]), cell(vec![para_s("d")])],
    ];
    // Only the line between the rows.
    let borders = TableBorders { inside_h: Some(single(1.0)), ..Default::default() };
    let d = doc(vec![Block::Table {
        rows: rows.clone(),
        border: true,
        width_px: None,
        borders: Some(Box::new(borders)),
    }]);
    let l = lay(&d, 300.0);
    assert!(stroke_rects(&l).is_empty(), "the plain grid is not drawn");
    let lines = lines_in(&l, RED);
    assert_eq!(lines.len(), 2, "one line over each second-row cell: {lines:?}");
    assert!(lines.iter().all(|(a, b, _)| a.y == b.y));
    // Outside box plus the column line.
    let borders = TableBorders {
        top: Some(single(1.0)),
        bottom: Some(single(1.0)),
        left: Some(single(1.0)),
        right: Some(single(1.0)),
        inside_v: Some(single(1.0)),
        ..Default::default()
    };
    let d = doc(vec![Block::Table { rows, border: false, width_px: None, borders: Some(Box::new(borders)) }]);
    let lines = lines_in(&lay(&d, 300.0), RED);
    let vertical = lines.iter().filter(|(a, b, _)| a.x == b.x).count();
    assert_eq!(vertical, 4, "left, right and one column line per row: {lines:?}");
}

#[test]
fn dotted_edge_far_down_a_long_document_ends() {
    // Regression: the dot loop added `on + off` to an absolute f32 coordinate, which stops moving near
    // y = 2^26, so a dotted left edge that far down never finished and filled memory.
    use crate::block::{Ctx, Out};
    use crate::border::{Side, draw_edge};
    let fonts = FontDb::bundled();
    let ctx = Ctx::new(&fonts, &LayoutOptions { width: 400.0, ..Default::default() });
    let mut out = Out::default();
    let edge = BorderEdge::new(BorderLine::Dotted, Some(RED), 0.75);
    let y = 1.0e8_f32;
    draw_edge(&ctx, &mut out, &edge, Side::Left, 10.0, y, y + 200.0, 1.0);
    assert!(!out.items.is_empty());
    assert!(out.items.len() <= 4096, "{}", out.items.len());
    // The same far down, horizontally (x is capped, but `a` can still be large).
    let mut out = Out::default();
    draw_edge(&ctx, &mut out, &edge, Side::Top, y, y, y + 100.0, 1.0);
    assert!(out.items.len() <= 4096);
}
