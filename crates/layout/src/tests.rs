//! Layout tests. Font advances come from `mailcraft-fonts`, which may be a stub or real fonts, so these tests
//! assert relationships (ordering, containment, counts) rather than exact pixel widths.

use mailcraft_fonts::FontDb;
use mailcraft_geom::{Color, Point, Rect};
use mailcraft_model::rich::{Align, ListKind, TableCell};
use mailcraft_model::{Block, Doc, Inline, ParaStyle, TextStyle};
use proptest::prelude::*;

use crate::{DisplayItem, Layout, LayoutOptions, TextPos, layout_doc};

mod borders;

fn text(s: &str) -> Inline {
    Inline::Text { text: s.into(), style: TextStyle::default() }
}

fn styled(s: &str, style: TextStyle) -> Inline {
    Inline::Text { text: s.into(), style }
}

fn para(inlines: Vec<Inline>) -> Block {
    Block::Paragraph { style: ParaStyle::default(), inlines }
}

fn para_s(s: &str) -> Block {
    para(vec![text(s)])
}

fn doc(blocks: Vec<Block>) -> Doc {
    Doc { blocks, ..Default::default() }
}

fn lay(d: &Doc, width: f32) -> Layout {
    let fonts = FontDb::bundled();
    layout_doc(d, &fonts, &LayoutOptions { width, ..Default::default() })
}

fn lay_opts(d: &Doc, opts: &LayoutOptions) -> Layout {
    let fonts = FontDb::bundled();
    layout_doc(d, &fonts, opts)
}

fn cell(blocks: Vec<Block>) -> TableCell {
    TableCell { blocks, colspan: 1, rowspan: 1, background: None, width_px: None }
}

fn run_texts(l: &Layout) -> Vec<String> {
    l.text_runs().iter().map(|r| r.text.to_string()).collect()
}

fn joined(l: &Layout) -> String {
    run_texts(l).join("")
}

/// Bounds of every positioned item (text via its run box).
fn item_bounds(l: &Layout) -> Vec<Rect> {
    let mut v: Vec<Rect> = l.text_runs().iter().map(|r| r.rect).collect();
    for item in &l.display.items {
        match item {
            DisplayItem::FillRect { rect, .. }
            | DisplayItem::StrokeRect { rect, .. }
            | DisplayItem::Image { rect, .. } => v.push(*rect),
            DisplayItem::Line { from, to, .. } => v.push(Rect::new(
                from.x.min(to.x),
                from.y.min(to.y),
                (to.x - from.x).abs(),
                (to.y - from.y).abs(),
            )),
            DisplayItem::Text { .. } => {}
        }
    }
    v
}

// ---------------------------------------------------------------- basics

#[test]
fn empty_doc_has_zero_height() {
    let l = lay(&Doc::default(), 600.0);
    assert_eq!(l.height, 0.0);
    assert_eq!(l.line_count(), 0);
    assert!(l.display.items.is_empty());
    assert_eq!(l.width, 600.0);
}

#[test]
fn empty_paragraph_has_one_line() {
    let l = lay(&doc(vec![para(vec![])]), 600.0);
    assert_eq!(l.line_count(), 1);
    assert!(l.height > 0.0);
    assert!(l.text_runs().is_empty());
}

#[test]
fn single_line_paragraph() {
    let l = lay(&doc(vec![para_s("Hello world")]), 600.0);
    assert_eq!(l.line_count(), 1);
    assert_eq!(joined(&l), "Hello world");
    let r = l.text_runs();
    let first = r.first().unwrap();
    assert_eq!(first.origin.x, 0.0);
    assert!(first.origin.y > 0.0 && first.origin.y < l.height);
    assert_eq!(first.color, Color::BLACK);
}

#[test]
fn default_font_size_is_body_size() {
    let l = lay(&doc(vec![para_s("x")]), 600.0);
    let r = l.text_runs();
    assert!((r.first().unwrap().size_px - 16.0).abs() < 1e-3);
}

#[test]
fn zoom_scales_text_and_height() {
    let d = doc(vec![para_s("Hello")]);
    let a = lay_opts(&d, &LayoutOptions { zoom: 1.0, ..Default::default() });
    let b = lay_opts(&d, &LayoutOptions { zoom: 2.0, ..Default::default() });
    assert!((b.height - 2.0 * a.height).abs() < 1e-2);
    assert!((b.text_runs()[0].size_px - 32.0).abs() < 1e-3);
}

#[test]
fn doc_base_and_opts_base_inherit() {
    let mut d = doc(vec![para_s("x")]);
    d.base = TextStyle { size_pt: Some(24.0), ..Default::default() };
    let opts = LayoutOptions {
        base: TextStyle { color: Some(Color::hex(0x112233)), size_pt: Some(8.0), ..Default::default() },
        ..Default::default()
    };
    let l = lay_opts(&d, &opts);
    let r = &l.text_runs()[0];
    assert!((r.size_px - 32.0).abs() < 1e-3, "doc.base wins over opts.base");
    assert_eq!(r.color, Color::hex(0x112233));
}

#[test]
fn default_color_from_options() {
    let opts = LayoutOptions { default_color: Color::hex(0x445566), ..Default::default() };
    let l = lay_opts(&doc(vec![para_s("x")]), &opts);
    assert_eq!(l.text_runs()[0].color, Color::hex(0x445566));
}

#[test]
fn bold_and_italic_pick_fonts() {
    let fonts = FontDb::bundled();
    let bold = TextStyle { bold: Some(true), ..Default::default() };
    let it = TextStyle { italic: Some(true), ..Default::default() };
    let l = layout_doc(
        &doc(vec![para(vec![text("a "), styled("b ", bold), styled("c", it)])]),
        &fonts,
        &LayoutOptions::default(),
    );
    let r = l.text_runs();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].font, fonts.resolve("Aptos", 400, false));
    assert_eq!(r[1].font, fonts.resolve("Aptos", 700, false));
    assert_eq!(r[2].font, fonts.resolve("Aptos", 400, true));
}

#[test]
fn style_changes_split_runs_left_to_right() {
    let red = TextStyle { color: Some(Color::rgb(255, 0, 0)), ..Default::default() };
    let l = lay(&doc(vec![para(vec![text("one "), styled("two", red), text(" three")])]), 600.0);
    let r = l.text_runs();
    assert_eq!(r.len(), 3);
    assert!(r[0].origin.x < r[1].origin.x && r[1].origin.x < r[2].origin.x);
    assert_eq!(r[1].color, Color::rgb(255, 0, 0));
    assert_eq!(r[0].origin.y, r[2].origin.y);
}

// ---------------------------------------------------------------- wrapping

#[test]
fn wraps_at_width() {
    let words = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
    let wide = lay(&doc(vec![para_s(words)]), 10_000.0);
    let narrow = lay(&doc(vec![para_s(words)]), 150.0);
    assert_eq!(wide.line_count(), 1);
    assert!(narrow.line_count() > 2);
    assert!(narrow.height > wide.height);
    for r in narrow.text_runs() {
        assert!(r.rect.right() <= 150.0 + 1e-3, "{:?}", r);
        assert!(r.rect.x >= 0.0);
    }
    // All text is still there, minus the spaces that hang at wrap points.
    let all: String = run_texts(&narrow).join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(all, words.split_whitespace().collect::<Vec<_>>().join(" "));
}

#[test]
fn lines_stack_downwards() {
    let l = lay(&doc(vec![para_s("aaa bbb ccc ddd eee fff ggg hhh")]), 60.0);
    let ys: Vec<f32> = l.index.lines.iter().map(|l| l.rect.y).collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]));
    let last = l.index.lines.last().unwrap();
    assert!((last.rect.bottom() - l.height).abs() < 1e-3);
}

#[test]
fn overlong_word_breaks_by_grapheme() {
    let l = lay(&doc(vec![para_s("Supercalifragilisticexpialidocious")]), 50.0);
    assert!(l.line_count() > 1);
    for r in l.text_runs() {
        assert!(r.rect.right() <= 50.0 + 1e-3);
    }
    assert_eq!(joined(&l), "Supercalifragilisticexpialidocious");
}

#[test]
fn graphemes_are_not_split() {
    // e + combining acute repeated: every line break must fall between clusters.
    let s = "e\u{301}".repeat(40);
    let l = lay(&doc(vec![para_s(&s)]), 40.0);
    for r in l.text_runs() {
        assert!(!r.text.starts_with('\u{301}'));
    }
    assert_eq!(joined(&l), s);
}

#[test]
fn mandatory_breaks_are_honoured() {
    let l = lay(&doc(vec![para(vec![text("one"), Inline::LineBreak, text("two\nthree")])]), 600.0);
    assert_eq!(l.line_count(), 3);
    assert_eq!(run_texts(&l), vec!["one", "two", "three"]);
}

#[test]
fn consecutive_breaks_make_empty_lines() {
    let l = lay(&doc(vec![para(vec![text("a"), Inline::LineBreak, Inline::LineBreak, text("b")])]), 600.0);
    assert_eq!(l.line_count(), 3);
}

#[test]
fn trailing_spaces_not_drawn() {
    let l = lay(&doc(vec![para_s("word     ")]), 600.0);
    assert_eq!(run_texts(&l), vec!["word"]);
}

#[test]
fn zero_width_does_not_hang() {
    let l = lay(&doc(vec![para_s("abc def")]), 0.0);
    assert!(l.line_count() >= 6);
    assert!(l.height.is_finite());
}

#[test]
fn hostile_options_are_sanitised() {
    let d = doc(vec![para_s("hello world")]);
    for (w, z) in [(f32::NAN, f32::NAN), (-5.0, -1.0), (f32::INFINITY, 0.0), (1.0e30, 1.0e30)] {
        let l = lay_opts(&d, &LayoutOptions { width: w, zoom: z, ..Default::default() });
        assert!(l.width.is_finite() && l.width >= 0.0);
        assert!(l.height.is_finite());
    }
}

#[test]
fn mixed_sizes_share_baseline() {
    let big = TextStyle { size_pt: Some(36.0), ..Default::default() };
    let l = lay(&doc(vec![para(vec![text("small "), styled("BIG", big), text(" small")])]), 600.0);
    let r = l.text_runs();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].origin.y, r[1].origin.y);
    assert_eq!(r[1].origin.y, r[2].origin.y);
    let small_only = lay(&doc(vec![para_s("small small")]), 600.0);
    assert!(l.height > small_only.height);
    assert!(r[0].origin.y > small_only.text_runs()[0].origin.y);
}

#[test]
fn superscript_raised() {
    let sup = TextStyle { superscript: Some(true), ..Default::default() };
    let l = lay(&doc(vec![para(vec![text("x"), styled("2", sup)])]), 600.0);
    let r = l.text_runs();
    assert!(r[1].origin.y < r[0].origin.y);
    assert!(r[1].size_px < r[0].size_px);
}

#[test]
fn line_height_multiplier() {
    let base = lay(&doc(vec![para_s("x")]), 600.0);
    let style = ParaStyle { line_height: Some(2.0), ..Default::default() };
    let l = lay(&doc(vec![Block::Paragraph { style, inlines: vec![text("x")] }]), 600.0);
    assert!((l.height - 2.0 * base.height).abs() < 1e-2);
}

// ---------------------------------------------------------------- alignment & spacing

fn aligned(align: Align, s: &str, width: f32) -> Layout {
    let style = ParaStyle { align, ..Default::default() };
    lay(&doc(vec![Block::Paragraph { style, inlines: vec![text(s)] }]), width)
}

#[test]
fn alignment_start_center_end() {
    let s = aligned(Align::Start, "hi", 400.0).text_runs()[0].rect;
    let c = aligned(Align::Center, "hi", 400.0).text_runs()[0].rect;
    let e = aligned(Align::End, "hi", 400.0).text_runs()[0].rect;
    assert_eq!(s.x, 0.0);
    assert!((c.x + c.w / 2.0 - 200.0).abs() < 1e-3);
    assert!((e.right() - 400.0).abs() < 1e-3);
}

#[test]
fn justify_fills_all_but_last_line() {
    let s = "aa bb cc dd ee ff gg hh ii jj kk ll mm nn oo pp";
    let l = aligned(Align::Justify, s, 130.0);
    assert!(l.line_count() >= 3);
    let lines = &l.index.lines;
    let runs = l.text_runs();
    // First line: its last run ends at the right edge.
    let first = &lines[0];
    let first_line_runs: Vec<_> =
        runs.iter().filter(|r| r.origin.y > first.rect.y && r.origin.y < first.rect.bottom()).collect();
    let right = first_line_runs.iter().map(|r| r.rect.right()).fold(0.0f32, f32::max);
    assert!((right - 130.0).abs() < 0.01, "right edge {right}");
    // Last line is start-aligned and not stretched.
    let last = lines.last().unwrap();
    let last_runs: Vec<_> = runs.iter().filter(|r| r.origin.y > last.rect.y).collect();
    let right_last = last_runs.iter().map(|r| r.rect.right()).fold(0.0f32, f32::max);
    assert!(right_last < 130.0 - 1.0);
}

#[test]
fn paragraph_spacing_and_indent() {
    let plain = lay(&doc(vec![para_s("a"), para_s("b")]), 600.0);
    let style =
        ParaStyle { space_before_px: 10.0, space_after_px: 5.0, indent_px: 30.0, ..Default::default() };
    let spaced = lay(&doc(vec![Block::Paragraph { style, inlines: vec![text("a")] }, para_s("b")]), 600.0);
    assert!((spaced.height - plain.height - 15.0).abs() < 1e-3);
    let r = spaced.text_runs();
    assert_eq!(r[0].rect.x, 30.0);
    assert_eq!(r[1].rect.x, 0.0);
    assert!((r[1].origin.y - plain.text_runs()[1].origin.y - 15.0).abs() < 1e-3);
}

#[test]
fn paragraph_background_behind_text() {
    let style = ParaStyle { background: Some(Color::rgb(1, 2, 3)), ..Default::default() };
    let l = lay(&doc(vec![Block::Paragraph { style, inlines: vec![text("x")] }]), 300.0);
    match &l.display.items[0] {
        DisplayItem::FillRect { rect, color } => {
            assert_eq!(*color, Color::rgb(1, 2, 3));
            assert_eq!(rect.w, 300.0);
            assert!((rect.h - l.height).abs() < 1e-3);
        }
        other => panic!("expected background first, got {other:?}"),
    }
}

/// Text in the automatic colour stays readable on dark shading (the Shading button's face is black).
#[test]
fn automatic_text_colour_turns_white_on_dark_shading() {
    let colors = |bg: Color, inlines: Vec<Inline>| -> Vec<Color> {
        let style = ParaStyle { background: Some(bg), ..Default::default() };
        let l = lay(&doc(vec![Block::Paragraph { style, inlines }, para_s("after")]), 300.0);
        l.display
            .items
            .iter()
            .filter_map(|i| match i {
                DisplayItem::Text { color, .. } => Some(*color),
                _ => None,
            })
            .collect()
    };
    let red = TextStyle { color: Some(Color::rgb(0xee, 0, 0)), ..Default::default() };
    let c = colors(Color::BLACK, vec![text("auto "), styled("red", red)]);
    assert_eq!(
        c,
        [Color::WHITE, Color::rgb(0xee, 0, 0), Color::BLACK],
        "own colours and later paragraphs keep theirs"
    );
    assert_eq!(colors(Color::rgb(0xd9, 0xd9, 0xd9), vec![text("x")]), [Color::BLACK, Color::BLACK]);
    assert_eq!(crate::block::auto_text_color(Some(Color::WHITE), Color::WHITE), Color::BLACK);
    assert_eq!(crate::block::auto_text_color(None, Color::WHITE), Color::WHITE);
}

#[test]
fn highlight_drawn_before_text() {
    let hl = TextStyle { highlight: Some(Color::rgb(255, 255, 0)), ..Default::default() };
    let l = lay(&doc(vec![para(vec![styled("marked", hl)])]), 300.0);
    let fill = l.display.items.iter().position(|i| matches!(i, DisplayItem::FillRect { .. })).unwrap();
    let txt = l.display.items.iter().position(|i| matches!(i, DisplayItem::Text { .. })).unwrap();
    assert!(fill < txt);
    if let DisplayItem::FillRect { rect, color } = &l.display.items[fill] {
        assert_eq!(*color, Color::rgb(255, 255, 0));
        assert_eq!(*rect, l.text_runs()[0].rect);
    }
}

#[test]
fn doc_background_covers_doc() {
    let mut d = doc(vec![para_s("x"), para_s("y")]);
    d.background = Some(Color::WHITE);
    let l = lay(&d, 320.0);
    assert_eq!(
        l.display.items[0],
        DisplayItem::FillRect { rect: Rect::new(0.0, 0.0, 320.0, l.height), color: Color::WHITE }
    );
}

#[test]
fn underline_and_strike_flags() {
    let u = TextStyle { underline: Some(true), strike: Some(true), ..Default::default() };
    let l = lay(&doc(vec![para(vec![styled("x", u)])]), 300.0);
    let r = l.text_runs()[0];
    assert!(r.underline && r.strike);
}

// ---------------------------------------------------------------- headings

#[test]
fn heading_sizes_decrease() {
    let mut sizes = Vec::new();
    for level in 1..=6u8 {
        let l = lay(
            &doc(vec![Block::Heading { level, style: ParaStyle::default(), inlines: vec![text("H")] }]),
            600.0,
        );
        sizes.push(l.text_runs()[0].size_px);
    }
    assert!((sizes[0] - 32.0).abs() < 1e-3);
    assert!((sizes[3] - 16.0).abs() < 1e-3);
    assert!(sizes.windows(2).all(|w| w[0] > w[1]));
}

#[test]
fn heading_is_bold_and_level_clamped() {
    let fonts = FontDb::bundled();
    let l = layout_doc(
        &doc(vec![Block::Heading { level: 0, style: ParaStyle::default(), inlines: vec![text("H")] }]),
        &fonts,
        &LayoutOptions::default(),
    );
    let r = l.text_runs()[0];
    assert_eq!(r.font, fonts.resolve("Aptos", 700, false));
    assert!((r.size_px - 32.0).abs() < 1e-3);
    let l = lay(
        &doc(vec![Block::Heading { level: 200, style: ParaStyle::default(), inlines: vec![text("H")] }]),
        600.0,
    );
    assert!((l.text_runs()[0].size_px - 16.0 * 0.67).abs() < 1e-3);
}

// ---------------------------------------------------------------- lists

fn list(kind: ListKind, items: Vec<Vec<Block>>) -> Block {
    Block::List { kind, items }
}

#[test]
fn numbered_list_markers() {
    let l = lay(
        &doc(vec![list(
            ListKind::Numbered { start: 1 },
            vec![vec![para_s("a")], vec![para_s("b")], vec![para_s("c")]],
        )]),
        600.0,
    );
    let t = run_texts(&l);
    for m in ["1.", "2.", "3."] {
        assert!(t.contains(&m.to_string()), "{t:?}");
    }
    assert_eq!(l.line_count(), 3);
}

#[test]
fn numbered_list_custom_start() {
    let l = lay(
        &doc(vec![list(ListKind::Numbered { start: 7 }, vec![vec![para_s("a")], vec![para_s("b")]])]),
        600.0,
    );
    let t = run_texts(&l);
    assert!(t.contains(&"7.".to_string()) && t.contains(&"8.".to_string()));
    let l = lay(
        &doc(vec![list(ListKind::Numbered { start: u32::MAX }, vec![vec![para_s("a")], vec![para_s("b")]])]),
        600.0,
    );
    assert!(run_texts(&l).contains(&"4294967296.".to_string()));
}

#[test]
fn bullets_by_depth_and_indent() {
    let inner = list(ListKind::Bullet, vec![vec![para_s("inner")]]);
    let innermost = list(ListKind::Bullet, vec![vec![para_s("deepest")]]);
    let mid = list(ListKind::Bullet, vec![vec![para_s("mid"), innermost]]);
    let d = doc(vec![list(ListKind::Bullet, vec![vec![para_s("outer"), inner], vec![para_s("two"), mid]])]);
    let l = lay(&d, 600.0);
    let runs = l.text_runs();
    let t = run_texts(&l);
    assert!(t.contains(&"\u{2022}".to_string()));
    assert!(t.contains(&"\u{25e6}".to_string()));
    assert!(t.contains(&"\u{25aa}".to_string()));
    let x_of = |s: &str| runs.iter().find(|r| r.text == s).unwrap().rect.x;
    assert_eq!(x_of("outer"), 24.0);
    assert_eq!(x_of("inner"), 48.0);
    assert_eq!(x_of("deepest"), 72.0);
    // Markers sit left of their item text, on the same baseline.
    let outer = runs.iter().find(|r| r.text == "outer").unwrap();
    let bullet = runs.iter().find(|r| r.text == "\u{2022}").unwrap();
    assert!(bullet.rect.right() <= outer.rect.x);
    assert_eq!(bullet.origin.y, outer.origin.y);
}

#[test]
fn list_paths_index_items() {
    let d = doc(vec![
        para_s("x"),
        list(ListKind::Bullet, vec![vec![para_s("a")], vec![para_s("b"), para_s("c")]]),
    ]);
    let l = lay(&d, 600.0);
    let paths: Vec<Vec<usize>> = l.index.lines.iter().map(|l| l.block_path.clone()).collect();
    assert_eq!(paths, vec![vec![0], vec![1, 0, 0], vec![1, 1, 0], vec![1, 1, 1]]);
}

#[test]
fn empty_list_item_still_has_marker_and_height() {
    let l = lay(&doc(vec![list(ListKind::Numbered { start: 1 }, vec![vec![], vec![para_s("b")]])]), 600.0);
    assert!(run_texts(&l).contains(&"1.".to_string()));
    let b = l.text_runs().into_iter().find(|r| r.text == "b").unwrap();
    assert!(b.rect.y > 0.0);
}

// ---------------------------------------------------------------- quote, pre, rule, container

#[test]
fn quote_bar_and_indent() {
    let l = lay(&doc(vec![Block::Quote { blocks: vec![para_s("quoted"), para_s("more")] }]), 400.0);
    let bar = l
        .display
        .items
        .iter()
        .find_map(|i| match i {
            DisplayItem::FillRect { rect, .. } => Some(*rect),
            _ => None,
        })
        .unwrap();
    assert_eq!(bar.x, 0.0);
    assert_eq!(bar.w, 2.0);
    assert!((bar.h - l.height).abs() < 1e-3);
    assert_eq!(l.text_runs()[0].rect.x, 14.0);
    assert_eq!(l.index.lines[1].block_path, vec![0, 1]);
}

#[test]
fn nested_quotes_indent_further() {
    let d = doc(vec![Block::Quote { blocks: vec![Block::Quote { blocks: vec![para_s("deep")] }] }]);
    let l = lay(&d, 400.0);
    assert_eq!(l.text_runs()[0].rect.x, 28.0);
}

#[test]
fn pre_is_monospace_and_unwrapped() {
    let fonts = FontDb::bundled();
    let long = "x".repeat(200);
    let d = doc(vec![Block::Pre { text: format!("{long}\nline two\n") }]);
    let l = layout_doc(&d, &fonts, &LayoutOptions { width: 100.0, ..Default::default() });
    assert_eq!(l.line_count(), 2);
    let r = l.text_runs();
    assert_eq!(r[0].text, long);
    assert_eq!(r[0].font, fonts.resolve("Consolas, Courier New, monospace", 400, false));
    assert!(r[0].rect.right() > 100.0, "pre overflows instead of wrapping");
}

#[test]
fn pre_tabs_expand() {
    let l = lay(&doc(vec![Block::Pre { text: "a\tb".into() }]), 600.0);
    let r = l.text_runs();
    assert_eq!(run_texts(&l), vec!["a", "b"]);
    assert!(r[1].rect.x >= r[0].rect.right() + r[0].rect.w);
}

#[test]
fn rule_is_full_width_line_with_margins() {
    let l = lay(&doc(vec![Block::Rule]), 333.0);
    assert!((l.height - 17.0).abs() < 1e-3);
    match &l.display.items[0] {
        DisplayItem::Line { from, to, width, .. } => {
            assert_eq!(from.x, 0.0);
            assert_eq!(to.x, 333.0);
            assert!((from.y - 8.5).abs() < 1e-3);
            assert_eq!(*width, 1.0);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn container_background_and_children() {
    let style = ParaStyle {
        background: Some(Color::rgb(9, 9, 9)),
        indent_px: 10.0,
        space_before_px: 4.0,
        ..Default::default()
    };
    let d = doc(vec![Block::Container { style, blocks: vec![para_s("in"), para_s("side")] }]);
    let l = lay(&d, 300.0);
    match &l.display.items[0] {
        DisplayItem::FillRect { rect, color } => {
            assert_eq!(*color, Color::rgb(9, 9, 9));
            assert_eq!(rect.y, 4.0);
            assert!((rect.bottom() - l.height).abs() < 1e-3);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(l.text_runs()[0].rect.x, 10.0);
    assert_eq!(l.index.lines[1].block_path, vec![0, 1]);
}

// ---------------------------------------------------------------- images

#[test]
fn image_placeholder_and_given_size() {
    let d = doc(vec![para(vec![
        Inline::Image { src: "cid:a".into(), alt: "A".into(), width: None, height: None },
        Inline::Image { src: "cid:b".into(), alt: "B".into(), width: Some(40.0), height: Some(30.0) },
    ])]);
    let l = lay(&d, 600.0);
    let imgs: Vec<Rect> = l
        .display
        .items
        .iter()
        .filter_map(|i| match i {
            DisplayItem::Image { rect, .. } => Some(*rect),
            _ => None,
        })
        .collect();
    assert_eq!(imgs.len(), 2);
    assert_eq!((imgs[0].w, imgs[0].h), (16.0, 16.0));
    assert_eq!((imgs[1].w, imgs[1].h), (40.0, 30.0));
    assert!(imgs[1].x >= imgs[0].right());
    // Bottoms sit on the shared baseline.
    assert_eq!(imgs[0].bottom(), imgs[1].bottom());
    assert!(l.height >= 30.0);
}

#[test]
fn wide_image_scaled_to_fit() {
    let d = doc(vec![para(vec![Inline::Image {
        src: "https://x/y.png".into(),
        alt: String::new(),
        width: Some(1200.0),
        height: Some(600.0),
    }])]);
    let l = lay(&d, 300.0);
    let rect = l
        .display
        .items
        .iter()
        .find_map(|i| match i {
            DisplayItem::Image { rect, .. } => Some(*rect),
            _ => None,
        })
        .unwrap();
    assert!((rect.w - 300.0).abs() < 1e-3);
    assert!((rect.h - 150.0).abs() < 1e-3);
}

// ---------------------------------------------------------------- links & hit testing

#[test]
fn links_coloured_underlined_and_hittable() {
    let d = doc(vec![para(vec![
        text("see "),
        Inline::Link { title: None, href: "https://example.com/".into(), children: vec![text("example")] },
        text(" here"),
    ])]);
    let opts = LayoutOptions { link_color: Color::rgb(0, 0, 200), ..Default::default() };
    let l = lay_opts(&d, &opts);
    let r = l.text_runs();
    let link = r.iter().find(|r| r.text == "example").unwrap();
    assert_eq!(link.color, Color::rgb(0, 0, 200));
    assert!(link.underline);
    assert_eq!(link.link, Some("https://example.com/"));
    assert!(r.iter().find(|r| r.text == "see ").unwrap().link.is_none());
    assert_eq!(l.hit(link.rect.center()), Some("https://example.com/"));
    let plain = r.iter().find(|r| r.text == " here").unwrap();
    assert_eq!(l.hit(plain.rect.center()), None);
    assert_eq!(l.hit(Point::new(-5.0, -5.0)), None);
}

#[test]
fn link_explicit_colour_wins() {
    let style = TextStyle { color: Some(Color::rgb(1, 1, 1)), underline: Some(false), ..Default::default() };
    let d = doc(vec![para(vec![Inline::Link {
        title: None,
        href: "x".into(),
        children: vec![styled("t", style)],
    }])]);
    let l = lay(&d, 300.0);
    let r = l.text_runs()[0];
    assert_eq!(r.color, Color::rgb(1, 1, 1));
    assert!(!r.underline);
}

#[test]
fn wrapped_link_hittable_on_every_line() {
    let d = doc(vec![para(vec![Inline::Link {
        title: None,
        href: "mailto:a@b".into(),
        children: vec![text("one two three four five six seven")],
    }])]);
    let l = lay(&d, 80.0);
    assert!(l.line_count() > 1);
    for r in l.text_runs() {
        assert_eq!(l.hit(r.rect.center()), Some("mailto:a@b"));
    }
}

#[test]
fn image_link_hittable() {
    let d = doc(vec![para(vec![Inline::Link {
        title: None,
        href: "https://img".into(),
        children: vec![Inline::Image {
            src: "cid:x".into(),
            alt: String::new(),
            width: Some(50.0),
            height: Some(50.0),
        }],
    }])]);
    let l = lay(&d, 300.0);
    assert_eq!(l.hit(Point::new(25.0, 25.0)), Some("https://img"));
}

#[test]
fn hit_pos_finds_offsets() {
    let l = lay(&doc(vec![para_s("abcdef"), para_s("xyz")]), 600.0);
    let first = l.text_runs()[0].rect;
    assert_eq!(
        l.hit_pos(Point::new(-10.0, first.center().y)),
        Some(TextPos { block_path: vec![0], offset: 0 })
    );
    assert_eq!(
        l.hit_pos(Point::new(5000.0, first.center().y)),
        Some(TextPos { block_path: vec![0], offset: 6 })
    );
    let mid = l.hit_pos(Point::new(first.center().x, first.center().y)).unwrap();
    assert!(mid.offset > 0 && mid.offset < 6);
    let second = l.text_runs()[1].rect;
    assert_eq!(l.hit_pos(Point::new(-1.0, second.center().y)).unwrap().block_path, vec![1]);
    // Below everything: last line.
    assert_eq!(l.hit_pos(Point::new(0.0, 10_000.0)).unwrap().block_path, vec![1]);
    assert_eq!(l.hit_pos(Point::new(f32::NAN, 0.0)), None);
    assert_eq!(lay(&Doc::default(), 100.0).hit_pos(Point::new(1.0, 1.0)), None);
}

#[test]
fn hit_pos_round_trips_caret() {
    let l = lay(&doc(vec![para_s("hello wide world of text")]), 90.0);
    for off in 0..=24 {
        let pos = TextPos { block_path: vec![0], offset: off };
        let c = l.caret_rect(&pos).unwrap();
        let back = l.hit_pos(Point::new(c.x, c.y + c.h / 2.0)).unwrap();
        assert_eq!(l.caret_rect(&back).unwrap(), c, "offset {off}");
    }
}

#[test]
fn caret_rects_monotonic() {
    let s = "The quick brown fox jumps over the lazy dog again and again";
    let l = lay(&doc(vec![para_s(s)]), 120.0);
    assert!(l.line_count() > 2);
    let mut prev: Option<Rect> = None;
    for off in 0..=s.chars().count() {
        let c = l.caret_rect(&TextPos { block_path: vec![0], offset: off }).unwrap();
        assert_eq!(c.w, 1.0);
        assert!(c.h > 0.0);
        if let Some(p) = prev {
            assert!(c.y > p.y || (c.y == p.y && c.x >= p.x), "offset {off}: {p:?} -> {c:?}");
        }
        prev = Some(c);
    }
}

#[test]
fn caret_in_empty_paragraph_and_unknown_path() {
    let l = lay(&doc(vec![para(vec![])]), 300.0);
    let c = l.caret_rect(&TextPos { block_path: vec![0], offset: 0 }).unwrap();
    assert_eq!(c.x, 0.0);
    assert!(c.h > 0.0);
    assert!(l.caret_rect(&TextPos { block_path: vec![5], offset: 0 }).is_none());
    // Offsets past the end clamp to the end.
    let l = lay(&doc(vec![para_s("ab")]), 300.0);
    let end = l.caret_rect(&TextPos { block_path: vec![0], offset: 2 }).unwrap();
    let past = l.caret_rect(&TextPos { block_path: vec![0], offset: 99 }).unwrap();
    assert_eq!(end, past);
}

#[test]
fn caret_after_line_break_on_next_line() {
    let l = lay(&doc(vec![para(vec![text("ab"), Inline::LineBreak, text("cd")])]), 300.0);
    let before = l.caret_rect(&TextPos { block_path: vec![0], offset: 2 }).unwrap();
    let after = l.caret_rect(&TextPos { block_path: vec![0], offset: 3 }).unwrap();
    assert!(after.y > before.y);
    assert_eq!(after.x, 0.0);
}

#[test]
fn caret_offsets_count_images_and_breaks() {
    let d = doc(vec![para(vec![
        Inline::Image { src: String::new(), alt: String::new(), width: Some(20.0), height: Some(10.0) },
        text("a"),
    ])]);
    let l = lay(&d, 300.0);
    let c1 = l.caret_rect(&TextPos { block_path: vec![0], offset: 1 }).unwrap();
    assert!((c1.x - 20.0).abs() < 1e-3);
    let line = &l.index.lines[0];
    assert_eq!((line.start, line.end), (0, 2));
}

#[test]
fn caret_with_center_alignment_offset() {
    let l = aligned(Align::Center, "ab", 400.0);
    let c = l.caret_rect(&TextPos { block_path: vec![0], offset: 0 }).unwrap();
    assert!(c.x > 100.0);
}

// ---------------------------------------------------------------- tables

fn simple_table(border: bool, width_px: Option<f32>) -> Block {
    Block::Table {
        borders: None,
        rows: vec![
            vec![cell(vec![para_s("Name")]), cell(vec![para_s("A much longer description cell")])],
            vec![cell(vec![para_s("x")]), cell(vec![para_s("y")])],
        ],
        border,
        width_px,
    }
}

fn stroke_rects(l: &Layout) -> Vec<Rect> {
    l.display
        .items
        .iter()
        .filter_map(|i| match i {
            DisplayItem::StrokeRect { rect, .. } => Some(*rect),
            _ => None,
        })
        .collect()
}

#[test]
fn table_columns_fit_width() {
    for width in [50.0f32, 120.0, 300.0, 1000.0] {
        let l = lay(&doc(vec![simple_table(true, None)]), width);
        let strokes = stroke_rects(&l);
        let outer = strokes.last().unwrap();
        assert!(outer.w <= width + 1e-3, "table {} > {}", outer.w, width);
        for r in &strokes {
            assert!(r.x >= -1e-3 && r.right() <= width + 1e-3);
        }
    }
}

#[test]
fn table_auto_width_shrinks_to_content() {
    let l = lay(&doc(vec![simple_table(true, None)]), 2000.0);
    let outer = *stroke_rects(&l).last().unwrap();
    assert!(outer.w < 2000.0);
    // The long column is wider than the short one.
    let cells = stroke_rects(&l);
    assert!(cells[1].w > cells[0].w);
    // Cells in a row share y and height; second row is below.
    assert_eq!(cells[0].y, cells[1].y);
    assert_eq!(cells[0].h, cells[1].h);
    assert!(cells[2].y >= cells[0].bottom() - 1e-3);
    assert_eq!(cells[0].x, cells[2].x);
}

#[test]
fn table_explicit_width_fills() {
    let l = lay(&doc(vec![simple_table(true, Some(500.0))]), 800.0);
    let outer = *stroke_rects(&l).last().unwrap();
    assert!((outer.w - 500.0).abs() < 0.01);
    let l = lay(&doc(vec![simple_table(true, Some(5000.0))]), 300.0);
    assert!(stroke_rects(&l).last().unwrap().w <= 300.0 + 1e-3);
}

#[test]
fn table_cell_padding_and_paths() {
    let l = lay(&doc(vec![simple_table(false, None)]), 600.0);
    assert!(stroke_rects(&l).is_empty());
    let r = l.text_runs();
    let name = r.iter().find(|r| r.text == "Name").unwrap();
    assert!((name.rect.x - 4.0).abs() < 1e-3);
    let paths: Vec<Vec<usize>> = l.index.lines.iter().map(|l| l.block_path.clone()).collect();
    assert!(paths.contains(&vec![0, 0, 0, 0]));
    assert!(paths.contains(&vec![0, 1, 1, 0]));
}

#[test]
fn table_wraps_when_narrow() {
    let wide = lay(&doc(vec![simple_table(true, None)]), 1000.0);
    let narrow = lay(&doc(vec![simple_table(true, None)]), 150.0);
    assert!(narrow.line_count() > wide.line_count());
    assert!(narrow.height > wide.height);
    for r in narrow.text_runs() {
        assert!(r.rect.right() <= 150.0 + 1e-3, "{r:?}");
    }
}

#[test]
fn table_colspan() {
    let mut wide_cell = cell(vec![para_s("spanning header")]);
    wide_cell.colspan = 2;
    let d = doc(vec![Block::Table {
        borders: None,
        rows: vec![vec![wide_cell], vec![cell(vec![para_s("a")]), cell(vec![para_s("b")])]],
        border: true,
        width_px: None,
    }]);
    let l = lay(&d, 600.0);
    let s = stroke_rects(&l);
    assert_eq!(s.len(), 4);
    assert!((s[0].w - (s[1].w + s[2].w)).abs() < 1e-3);
    assert!((s[0].right() - s[2].right()).abs() < 1e-3);
}

#[test]
fn table_rowspan() {
    let mut tall = cell(vec![para_s("tall")]);
    tall.rowspan = 2;
    let d = doc(vec![Block::Table {
        borders: None,
        rows: vec![vec![tall, cell(vec![para_s("r1")])], vec![cell(vec![para_s("r2")])]],
        border: true,
        width_px: None,
    }]);
    let l = lay(&d, 600.0);
    let s = stroke_rects(&l);
    let r2 = l.text_runs().into_iter().find(|r| r.text == "r2").unwrap();
    // "r2" lands in the second column, not under "tall".
    assert!(r2.rect.x > s[0].right() - 1e-3);
    assert!((s[0].h - (s[1].h + s[2].h)).abs() < 1e-3);
}

#[test]
fn table_cell_background_and_width_hint() {
    let mut c = cell(vec![para_s("x")]);
    c.background = Some(Color::rgb(7, 7, 7));
    c.width_px = Some(200.0);
    let d = doc(vec![Block::Table {
        borders: None,
        rows: vec![vec![c, cell(vec![para_s("y")])]],
        border: false,
        width_px: None,
    }]);
    let l = lay(&d, 600.0);
    let bg = l
        .display
        .items
        .iter()
        .find_map(|i| match i {
            DisplayItem::FillRect { rect, color } if *color == Color::rgb(7, 7, 7) => Some(*rect),
            _ => None,
        })
        .unwrap();
    assert!((bg.w - 200.0).abs() < 1e-3);
}

#[test]
fn nested_table_fits() {
    let inner = simple_table(true, None);
    let d = doc(vec![Block::Table {
        borders: None,
        rows: vec![vec![cell(vec![inner]), cell(vec![para_s("side")])]],
        border: true,
        width_px: None,
    }]);
    for width in [80.0f32, 200.0, 700.0] {
        let l = lay(&d, width);
        for r in stroke_rects(&l) {
            assert!(r.right() <= width + 1e-3);
        }
    }
}

#[test]
fn empty_table_and_ragged_rows() {
    let l =
        lay(&doc(vec![Block::Table { borders: None, rows: vec![], border: true, width_px: None }]), 300.0);
    assert_eq!(l.height, 0.0);
    let d = doc(vec![Block::Table {
        borders: None,
        rows: vec![vec![cell(vec![para_s("a")])], vec![], vec![cell(vec![]), cell(vec![para_s("c")])]],
        border: true,
        width_px: Some(f32::NAN),
    }]);
    let l = lay(&d, 300.0);
    assert!(l.height > 0.0);
}

#[test]
fn hit_testing_inside_table_picks_column() {
    let l = lay(&doc(vec![simple_table(true, None)]), 600.0);
    let r = l.text_runs();
    let y = r.iter().find(|r| r.text == "y").unwrap();
    let pos = l.hit_pos(y.rect.center()).unwrap();
    assert_eq!(pos.block_path, vec![0, 1, 1, 0]);
}

// ---------------------------------------------------------------- robustness

#[test]
fn deep_nesting_does_not_overflow() {
    let mut b = para_s("core");
    for i in 0..5000 {
        b = if i % 2 == 0 {
            Block::Quote { blocks: vec![b] }
        } else {
            Block::Container { style: ParaStyle::default(), blocks: vec![b] }
        };
    }
    let l = lay(&doc(vec![b]), 600.0);
    assert!(l.height.is_finite());
}

#[test]
fn deep_link_nesting_keeps_content() {
    // Content below the inline nesting limit is still laid out, styled as a link and hittable.
    let mut inl = Inline::Link {
        title: None,
        href: "x".into(),
        children: vec![
            text("deep"),
            Inline::LineBreak,
            Inline::Image { src: "cid:i".into(), alt: String::new(), width: Some(10.0), height: Some(10.0) },
            Inline::Link { title: None, href: "y".into(), children: vec![text("er")] },
        ],
    };
    for _ in 0..1000 {
        inl = Inline::Link { title: None, href: "x".into(), children: vec![inl] };
    }
    let l = lay(&doc(vec![para(vec![inl])]), 600.0);
    assert_eq!(joined(&l), "deeper");
    assert_eq!(l.line_count(), 2);
    assert!(l.display.items.iter().any(|i| matches!(i, DisplayItem::Image { .. })));
    let r = l.text_runs();
    let deep = r.iter().find(|r| r.text == "deep").unwrap();
    assert!(deep.underline);
    assert_eq!(deep.color, LayoutOptions::default().link_color);
    assert!(l.hit(deep.rect.center()).is_some());
}

#[test]
fn control_chars_not_drawn() {
    let l = lay(&doc(vec![para_s("a\u{0}b\r\nc\u{2028}d")]), 600.0);
    for r in l.text_runs() {
        assert!(!r.text.chars().any(|c| c.is_control() || c == '\u{2028}'), "{:?}", r.text);
    }
}

#[test]
fn huge_font_sizes_clamped() {
    let big = TextStyle { size_pt: Some(1.0e30), ..Default::default() };
    let l = lay(&doc(vec![para(vec![styled("x", big)])]), 600.0);
    assert!(l.height.is_finite() && l.height < 10_000.0);
}

// ---------------------------------------------------------------- property tests

fn arb_style() -> impl Strategy<Value = TextStyle> {
    (
        proptest::option::of(any::<bool>()),
        proptest::option::of(any::<bool>()),
        proptest::option::of(any::<bool>()),
        proptest::option::of(prop_oneof![Just(-1.0f32), Just(f32::NAN), 4.0f32..24.0]),
        proptest::option::of(any::<bool>()),
        proptest::option::of(any::<u32>().prop_map(Color::hex)),
        (
            proptest::option::of(any::<bool>()),
            proptest::option::of(0usize..5),
            proptest::option::of(any::<bool>()),
            proptest::option::of(any::<bool>()),
            proptest::option::of(prop_oneof![Just(f32::NAN), Just(f32::INFINITY), -50.0f32..30.0]),
            proptest::option::of(prop_oneof![Just(f32::NAN), -300.0f32..300.0]),
        ),
    )
        .prop_map(
            |(
                bold,
                italic,
                superscript,
                size_pt,
                monospace,
                highlight,
                (u, kind, small, caps, spacing, pos),
            )| {
                TextStyle {
                    bold,
                    italic,
                    superscript,
                    size_pt,
                    monospace,
                    highlight,
                    underline: u,
                    underline_kind: kind
                        .and_then(|k| mailcraft_model::UnderlineKind::ALL.get(k))
                        .map(|(k, _)| *k),
                    double_strike: small.map(|s| !s),
                    small_caps: small,
                    all_caps: caps,
                    spacing_pt: spacing,
                    position_pt: pos,
                    ..Default::default()
                }
            },
        )
}

fn arb_text() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z ]{0,40}",
        "[a-zA-Z0-9 .,\\-\n\t]{0,60}",
        any::<String>().prop_map(|s| s.chars().take(30).collect()),
        Just("e\u{301}\u{1f600}\u{05d0}\u{05d1} \u{4e2d}\u{6587}".to_string()),
    ]
}

fn arb_inline() -> impl Strategy<Value = Inline> {
    let leaf = prop_oneof![
        4 => (arb_text(), arb_style()).prop_map(|(text, style)| Inline::Text { text, style }),
        1 => Just(Inline::LineBreak),
        1 => (proptest::option::of(-10.0f32..900.0), proptest::option::of(-10.0f32..900.0))
            .prop_map(|(width, height)| Inline::Image { src: "cid:x".into(), alt: "x".into(), width, height }),
    ];
    leaf.prop_recursive(2, 8, 3, |inner| {
        proptest::collection::vec(inner, 0..3).prop_map(|children| Inline::Link {
            title: None,
            href: "https://l/".into(),
            children,
        })
    })
}

fn arb_para_style() -> impl Strategy<Value = ParaStyle> {
    (
        (0u8..4, -5.0f32..60.0, -5.0f32..20.0, proptest::option::of(0.0f32..3.0), any::<bool>()),
        (-5.0f32..400.0, -400.0f32..400.0, proptest::option::of((-5.0f32..80.0, any::<bool>()))),
    )
        .prop_map(|((a, indent_px, space, line_height, bg), (right, first, fixed))| ParaStyle {
            align: match a {
                0 => Align::Start,
                1 => Align::Center,
                2 => Align::End,
                _ => Align::Justify,
            },
            indent_px,
            space_before_px: space,
            space_after_px: space,
            line_height,
            background: bg.then_some(Color::WHITE),
            right_indent_px: right,
            first_line_px: first,
            line_px: fixed.map(|(px, exact)| {
                (
                    px,
                    if exact {
                        mailcraft_model::LineRule::Exactly
                    } else {
                        mailcraft_model::LineRule::AtLeast
                    },
                )
            }),
            ..ParaStyle::default()
        })
}

fn arb_block() -> impl Strategy<Value = Block> {
    let leaf = prop_oneof![
        4 => (arb_para_style(), proptest::collection::vec(arb_inline(), 0..5))
            .prop_map(|(style, inlines)| Block::Paragraph { style, inlines }),
        1 => (any::<u8>(), proptest::collection::vec(arb_inline(), 0..3))
            .prop_map(|(level, inlines)| Block::Heading { level, style: ParaStyle::default(), inlines }),
        1 => Just(Block::Rule),
    ];
    leaf.prop_recursive(2, 24, 4, |inner| {
        prop_oneof![
            (
                any::<bool>(),
                0u32..1000,
                proptest::collection::vec(proptest::collection::vec(inner.clone(), 0..3), 0..4)
            )
                .prop_map(|(numbered, start, items)| Block::List {
                    kind: if numbered { ListKind::Numbered { start } } else { ListKind::Bullet },
                    items,
                }),
            proptest::collection::vec(inner.clone(), 0..3).prop_map(|blocks| Block::Quote { blocks }),
            (arb_para_style(), proptest::collection::vec(inner.clone(), 0..3))
                .prop_map(|(style, blocks)| Block::Container { style, blocks }),
            (
                proptest::collection::vec(
                    proptest::collection::vec(
                        (proptest::collection::vec(inner, 0..2), 0u32..4, 0u32..4, any::<bool>()).prop_map(
                            |(blocks, colspan, rowspan, bg)| TableCell {
                                blocks,
                                colspan,
                                rowspan,
                                background: bg.then_some(Color::WHITE),
                                width_px: None,
                            }
                        ),
                        0..4
                    ),
                    0..4
                ),
                any::<bool>(),
                proptest::option::of(-10.0f32..2000.0)
            )
                .prop_map(|(rows, border, width_px)| Block::Table {
                    borders: None,
                    rows,
                    border,
                    width_px
                }),
        ]
    })
}

fn arb_doc() -> impl Strategy<Value = Doc> {
    (proptest::collection::vec(arb_block(), 0..6), any::<bool>()).prop_map(|(blocks, bg)| Doc {
        blocks,
        base: TextStyle::default(),
        background: bg.then_some(Color::WHITE),
        drawings: Vec::new(),
    })
}

const EPS: f32 = 0.5;

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn random_docs_stay_in_bounds(d in arb_doc(), width in 400.0f32..900.0, zoom in prop_oneof![Just(1.0f32), 0.5f32..1.5]) {
        let fonts = FontDb::bundled();
        let l = layout_doc(&d, &fonts, &LayoutOptions { width, zoom, ..Default::default() });
        prop_assert!(l.height.is_finite() && l.height >= 0.0);
        for r in item_bounds(&l) {
            prop_assert!(r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite());
            prop_assert!(r.x >= -EPS && r.right() <= width + EPS, "{:?} outside 0..{}", r, width);
            prop_assert!(r.y >= -EPS && r.bottom() <= l.height + EPS, "{:?} below {}", r, l.height);
        }
        // Caret rects exist for every line start and hit_pos round-trips to some line.
        for line in &l.index.lines {
            let pos = TextPos { block_path: line.block_path.clone(), offset: line.start };
            prop_assert!(l.caret_rect(&pos).is_some());
            prop_assert!(l.hit_pos(Point::new(line.rect.x, line.rect.y + line.rect.h / 2.0)).is_some());
        }
    }

    #[test]
    fn random_docs_never_panic_any_width(d in arb_doc(), width in prop_oneof![Just(0.0f32), Just(f32::NAN), -100.0f32..3000.0]) {
        let l = lay(&d, width);
        prop_assert!(l.height.is_finite());
        let _ = l.hit(Point::new(10.0, 10.0));
        let _ = l.hit_pos(Point::new(10.0, 10.0));
        let _ = l.text_runs();
    }

    #[test]
    fn pre_random_text_never_panics(s in any::<String>(), width in 0.0f32..500.0) {
        let l = lay(&doc(vec![Block::Pre { text: s.clone() }]), width);
        prop_assert!(l.height.is_finite());
        let n = s.chars().count();
        let end = TextPos { block_path: vec![0], offset: n };
        prop_assert!(l.caret_rect(&end).is_some());
    }

    #[test]
    fn caret_monotonic_random_text(s in "[a-z ]{1,80}", width in 30.0f32..400.0) {
        let l = lay(&doc(vec![para_s(&s)]), width);
        let mut prev: Option<Rect> = None;
        for off in 0..=s.chars().count() {
            let c = l.caret_rect(&TextPos { block_path: vec![0], offset: off }).unwrap();
            if let Some(p) = prev {
                prop_assert!(c.y > p.y || (c.y == p.y && c.x >= p.x));
            }
            prev = Some(c);
        }
    }

    #[test]
    fn table_widths_sum_within(ncols in 1usize..6, nrows in 1usize..4, width in 0.0f32..800.0, words in "[a-z]{1,30}( [a-z]{1,12}){0,6}") {
        let rows = (0..nrows).map(|_| (0..ncols).map(|_| cell(vec![para_s(&words)])).collect()).collect();
        let l = lay(&doc(vec![Block::Table { borders: None, rows, border: true, width_px: None }]), width);
        let outer = *stroke_rects(&l).last().unwrap();
        prop_assert!(outer.w <= width + 1e-3);
    }
}

// ---------------------------------------------------------------- review regressions

#[test]
fn many_distinct_styles_lay_out_quickly() {
    // One inline per colour: style interning must not be quadratic in the number of styles.
    let inlines: Vec<Inline> = (0..60_000u32)
        .map(|i| styled("a", TextStyle { color: Some(Color::hex(i)), ..Default::default() }))
        .collect();
    let start = std::time::Instant::now();
    let l = lay(&doc(vec![para(inlines)]), 600.0);
    assert!(l.line_count() > 1);
    assert!(start.elapsed() < std::time::Duration::from_secs(5), "took {:?}", start.elapsed());
}

#[test]
fn deep_list_markers_stay_in_bounds() {
    let width = 300.0;
    let mut b = list(ListKind::Numbered { start: 100 }, vec![vec![]]);
    for _ in 0..20 {
        b = list(ListKind::Numbered { start: 100 }, vec![vec![b]]);
    }
    let l = lay(&doc(vec![b]), width);
    let runs = l.text_runs();
    assert!(!runs.is_empty());
    for r in runs {
        assert!(r.rect.x >= 0.0 && r.rect.right() <= width + 1e-3, "{r:?}");
    }
}

#[test]
fn caret_after_hanging_spaces_stays_in_box() {
    for align in [Align::Start, Align::Center, Align::End, Align::Justify] {
        let l = aligned(align, "ab          ", 100.0);
        let n = "ab          ".chars().count();
        for off in 0..=n {
            let c = l.caret_rect(&TextPos { block_path: vec![0], offset: off }).unwrap();
            assert!(c.x >= 0.0 && c.x <= 100.0, "{align:?} offset {off}: {c:?}");
        }
    }
}

#[test]
fn click_past_hanging_spaces_lands_at_end() {
    let s = "ab          ";
    let n = s.chars().count();
    for align in [Align::Start, Align::Center, Align::End] {
        let l = aligned(align, s, 100.0);
        let y = l.index.lines[0].rect.center().y;
        assert_eq!(l.hit_pos(Point::new(500.0, y)).unwrap().offset, n, "{align:?}");
        assert_eq!(l.hit_pos(Point::new(-500.0, y)).unwrap().offset, 0, "{align:?}");
    }
}

#[test]
fn trailing_line_break_adds_empty_line() {
    // A paragraph ending in a line break shows an empty last line (the HTML serializer adds a bogus <br> for
    // the same reason); the caret after the break sits at the start of that line.
    let plain = lay(&doc(vec![para_s("ab")]), 300.0);
    let l = lay(&doc(vec![para(vec![text("ab"), Inline::LineBreak])]), 300.0);
    assert_eq!(l.line_count(), 2);
    assert!(l.height > plain.height);
    let before = l.caret_rect(&TextPos { block_path: vec![0], offset: 2 }).unwrap();
    let after = l.caret_rect(&TextPos { block_path: vec![0], offset: 3 }).unwrap();
    assert!(after.y > before.y);
    assert_eq!(after.x, 0.0);
    let last = l.index.lines.last().unwrap();
    assert_eq!((last.start, last.end), (3, 3));
    assert_eq!(l.hit_pos(Point::new(50.0, last.rect.center().y)).unwrap().offset, 3);
    // Same for a newline inside text and for a lone break.
    assert_eq!(lay(&doc(vec![para_s("ab\n")]), 300.0).line_count(), 2);
    assert_eq!(lay(&doc(vec![para(vec![Inline::LineBreak])]), 300.0).line_count(), 2);
    // Preformatted text keeps HTML behaviour: a final newline does not open a line.
    assert_eq!(lay(&doc(vec![Block::Pre { text: "ab\n".into() }]), 300.0).line_count(), 1);
}

#[test]
fn hostile_zero_width_text_has_bounded_lines() {
    // An indent wider than the page leaves zero width: every grapheme would get its own line. The total line
    // count is capped so a hostile body cannot exhaust memory.
    let style = ParaStyle { indent_px: 10_000.0, ..Default::default() };
    let big = "a".repeat(crate::block::MAX_LINES + 1000);
    let block = Block::Paragraph { style, inlines: vec![text(&big)] };
    let l = lay(&doc(vec![block.clone(), block, para_s("after")]), 600.0);
    assert!(l.line_count() <= crate::block::MAX_LINES);
    assert!(l.line_count() > 1000);
    assert!(l.height.is_finite());
}

#[test]
fn table_borders_drawn_over_all_cell_backgrounds() {
    let mut a = cell(vec![para_s("a")]);
    a.background = Some(Color::rgb(1, 1, 1));
    let mut b = cell(vec![para_s("b")]);
    b.background = Some(Color::rgb(2, 2, 2));
    let l = lay(
        &doc(vec![Block::Table { borders: None, rows: vec![vec![a, b]], border: true, width_px: None }]),
        300.0,
    );
    let items = &l.display.items;
    let last_fill = items.iter().rposition(|i| matches!(i, DisplayItem::FillRect { .. })).unwrap();
    let first_stroke = items.iter().position(|i| matches!(i, DisplayItem::StrokeRect { .. })).unwrap();
    assert!(first_stroke > last_fill, "a later cell background paints over an earlier cell border");
}

// ---------------------------------------------------------------- Font dialog effects, Paragraph dialog

fn lines_of(l: &Layout) -> Vec<&crate::LineBox> {
    l.index.lines.iter().collect()
}

#[test]
fn small_caps_and_all_caps_keep_offsets() {
    let sc = TextStyle { small_caps: Some(true), ..Default::default() };
    let l = lay(&doc(vec![para(vec![styled("Hello", sc)])]), 400.0);
    let runs = l.text_runs();
    assert_eq!(joined(&l), "HELLO");
    let sizes: Vec<f32> = runs.iter().map(|r| r.size_px).collect();
    assert!(sizes.len() >= 2 && sizes[0] > sizes[1], "{sizes:?}");
    // Caret stops still count the stored text's chars.
    assert_eq!(l.index.lines[0].end, 5);
    let caps = TextStyle { all_caps: Some(true), ..Default::default() };
    let l = lay(&doc(vec![para(vec![styled("straße", caps)])]), 400.0);
    assert_eq!(joined(&l), "STRAßE", "one char stays one char");
    assert_eq!(l.index.lines[0].end, 6);
}

#[test]
fn letter_spacing_widens_and_position_raises() {
    let plain = lay(&doc(vec![para_s("spacing")]), 600.0);
    let wide_style = TextStyle { spacing_pt: Some(3.0), ..Default::default() };
    let wide = lay(&doc(vec![para(vec![styled("spacing", wide_style)])]), 600.0);
    let w = |l: &Layout| l.text_runs().iter().map(|r| r.rect.x + r.rect.w).fold(0.0f32, f32::max);
    assert!(w(&wide) > w(&plain) + 20.0, "{} vs {}", w(&wide), w(&plain));
    assert_eq!(wide.text_runs().len(), 7, "spaced characters are drawn one by one");
    let up = TextStyle { position_pt: Some(6.0), ..Default::default() };
    let raised = lay(&doc(vec![para(vec![text("a"), styled("b", up)])]), 600.0);
    let runs = raised.text_runs();
    assert!(runs[1].origin.y < runs[0].origin.y - 5.0);
}

#[test]
fn underline_styles_and_double_strike_draw_lines() {
    let count_lines =
        |l: &Layout| l.display.items.iter().filter(|i| matches!(i, DisplayItem::Line { .. })).count();
    for (kind, min) in [
        (mailcraft_model::UnderlineKind::Double, 2),
        (mailcraft_model::UnderlineKind::Dotted, 3),
        (mailcraft_model::UnderlineKind::Dashed, 2),
        (mailcraft_model::UnderlineKind::Wavy, 3),
    ] {
        let st = TextStyle { underline: Some(true), underline_kind: Some(kind), ..Default::default() };
        let l = lay(&doc(vec![para(vec![styled("underlined words", st)])]), 600.0);
        assert!(count_lines(&l) >= min, "{kind:?}");
        assert!(!l.text_runs()[0].underline, "drawn as lines instead");
    }
    let coloured = TextStyle {
        underline: Some(true),
        underline_color: Some(Color::rgb(255, 0, 0)),
        ..Default::default()
    };
    let l = lay(&doc(vec![para(vec![styled("red line", coloured)])]), 600.0);
    assert!(
        l.display
            .items
            .iter()
            .any(|i| matches!(i, DisplayItem::Line { color, .. } if *color == Color::rgb(255, 0, 0)))
    );
    let ds = TextStyle { double_strike: Some(true), strike: Some(true), ..Default::default() };
    let l = lay(&doc(vec![para(vec![styled("struck", ds)])]), 600.0);
    assert_eq!(count_lines(&l), 2);
    assert!(!l.text_runs()[0].strike);
}

#[test]
fn indents_first_line_and_fixed_line_heights() {
    let long = "word ".repeat(40);
    let base = lay(&doc(vec![para_s(&long)]), 300.0);
    let base_lines = lines_of(&base);
    let style = ParaStyle {
        indent_px: 40.0,
        right_indent_px: 30.0,
        first_line_px: -20.0,
        line_px: Some((40.0, mailcraft_model::LineRule::Exactly)),
        ..Default::default()
    };
    let d = doc(vec![Block::Paragraph { style, inlines: vec![text(&long)] }]);
    let l = lay(&d, 300.0);
    let lines = lines_of(&l);
    assert!(lines.len() > base_lines.len(), "narrower, so more lines");
    // Hanging: the first line starts 20 px left of the others.
    assert!((lines[0].rect.x - 20.0).abs() < 0.5, "{}", lines[0].rect.x);
    assert!((lines[1].rect.x - 40.0).abs() < 0.5, "{}", lines[1].rect.x);
    assert!(lines[1].rect.x + lines[1].rect.w <= 270.5);
    // Exactly 40 px per line.
    for ln in &lines {
        assert!((ln.rect.h - 40.0).abs() < 0.01, "{}", ln.rect.h);
    }
    // At least: never smaller than the text, at least the value.
    let at_least =
        ParaStyle { line_px: Some((4.0, mailcraft_model::LineRule::AtLeast)), ..Default::default() };
    let l = lay(&doc(vec![Block::Paragraph { style: at_least, inlines: vec![text("x")] }]), 300.0);
    assert!((l.index.lines[0].rect.h - base_lines[0].rect.h).abs() < 0.01);
    // A first-line indent bigger than the width does not break layout.
    let silly = ParaStyle { first_line_px: 1.0e9, right_indent_px: f32::NAN, ..Default::default() };
    let l = lay(&doc(vec![Block::Paragraph { style: silly, inlines: vec![text("x y z")] }]), 300.0);
    assert!(!l.index.lines.is_empty());
}

/// Regression (found by `random_docs_stay_in_bounds`): a numbered list in the last cell of a table whose
/// other cells hold paragraphs with large right indents drew its marker past the table's right edge.
#[test]
fn list_marker_in_a_squeezed_table_cell_stays_in_bounds() {
    let indented = |left: f32, right: f32| Block::Paragraph {
        style: ParaStyle { indent_px: left, right_indent_px: right, ..Default::default() },
        inlines: Vec::new(),
    };
    let list = Block::List { kind: ListKind::Numbered { start: 99 }, items: vec![Vec::new(), Vec::new()] };
    let mut cells = vec![cell(vec![indented(0.0, 363.61353)]), cell(vec![indented(35.99469, 243.26234)])];
    cells.push(cell(vec![list]));
    for c in &mut cells {
        c.colspan = 0;
        c.rowspan = 0;
    }
    let d = doc(vec![Block::Table { borders: None, rows: vec![cells], border: false, width_px: None }]);
    for width in [400.0f32, 250.0, 120.0] {
        let l = lay(&d, width);
        for r in item_bounds(&l) {
            assert!(r.x >= -EPS && r.right() <= width + EPS, "{r:?} outside 0..{width}");
        }
    }
}

/// Regression (found by `random_docs_stay_in_bounds`): a hanging first line in a frame narrower than the
/// paragraph's left indent reached back by the whole indent, past the frame's left edge.
#[test]
fn hanging_first_line_in_a_narrow_cell_stays_in_bounds() {
    let style = ParaStyle { indent_px: 47.207813, first_line_px: -138.05956, ..Default::default() };
    let image = Inline::Image { src: "cid:x".into(), alt: "x".into(), width: None, height: None };
    let para = Block::Paragraph { style, inlines: vec![image, text("word")] };
    let table = Block::Table {
        borders: None,
        rows: vec![vec![cell(vec![para])]],
        border: false,
        width_px: Some(34.061012),
    };
    let l = lay(&doc(vec![table]), 400.0);
    for r in item_bounds(&l) {
        assert!(r.x >= -EPS && r.right() <= 400.0 + EPS, "{r:?} outside 0..400");
    }
    for line in &l.index.lines {
        assert!(line.rect.x >= -EPS, "{:?}", line.rect);
    }
}
