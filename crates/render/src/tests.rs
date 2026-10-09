use super::*;
use mailcraft_fonts::FontId;
use mailcraft_geom::{Point, Rect};
use mailcraft_layout::DisplayItem;
use proptest::prelude::*;

const BG: Color = Color::WHITE;
const RED: Color = Color::rgb(255, 0, 0);
const BLUE: Color = Color::rgb(0, 0, 255);

fn no_images(_: &str) -> Option<(u32, u32, Vec<u8>)> {
    None
}

fn draw(items: Vec<DisplayItem>, w: u32, h: u32, scale: f32) -> Canvas {
    render(&DisplayList { items }, &FontDb::default(), w, h, scale, BG, &no_images)
}

fn fill(x: f32, y: f32, w: f32, h: f32, color: Color) -> DisplayItem {
    DisplayItem::FillRect { rect: Rect::new(x, y, w, h), color }
}

fn line(x0: f32, y0: f32, x1: f32, y1: f32, width: f32) -> DisplayItem {
    DisplayItem::Line { from: Point::new(x0, y0), to: Point::new(x1, y1), color: BLUE, width }
}

fn text(s: &str, x: f32, y: f32, size: f32, underline: bool, strike: bool) -> DisplayItem {
    DisplayItem::Text {
        origin: Point::new(x, y),
        text: s.to_string(),
        font: FontId(0),
        size_px: size,
        color: Color::BLACK,
        underline,
        strike,
        link: None,
    }
}

fn image(x: f32, y: f32, w: f32, h: f32, src: &str, alt: &str) -> DisplayItem {
    DisplayItem::Image { rect: Rect::new(x, y, w, h), src: src.to_string(), alt: alt.to_string() }
}

fn count_non_bg(c: &Canvas) -> usize {
    let mut n = 0;
    for y in 0..c.height {
        for x in 0..c.width {
            if c.pixel(x, y) != Some(BG) {
                n += 1;
            }
        }
    }
    n
}

fn row_non_bg(c: &Canvas, y: u32) -> usize {
    (0..c.width).filter(|&x| c.pixel(x, y) != Some(BG)).count()
}

// ---------------------------------------------------------------- canvas basics

#[test]
fn empty_list_is_background() {
    let c = draw(vec![], 20, 10, 1.0);
    assert_eq!((c.width, c.height), (20, 10));
    assert_eq!(c.pixels.len(), 20 * 10 * 4);
    assert_eq!(c.pixel(0, 0), Some(BG));
    assert_eq!(c.pixel(19, 9), Some(BG));
    assert_eq!(count_non_bg(&c), 0);
}

#[test]
fn background_colour_respected() {
    let bg = Color::rgb(12, 34, 56);
    let c = render(&DisplayList::default(), &FontDb::default(), 4, 4, 1.0, bg, &no_images);
    assert_eq!(c.pixel(2, 2), Some(bg));
}

#[test]
fn translucent_background_is_premultiplied() {
    let bg = Color::rgba(200, 100, 50, 128);
    let c = render(&DisplayList::default(), &FontDb::default(), 2, 2, 1.0, bg, &no_images);
    let p = c.pixel(0, 0).unwrap();
    assert_eq!(p.a, 128);
    assert!(p.r.abs_diff(200) <= 2 && p.g.abs_diff(100) <= 2 && p.b.abs_diff(50) <= 2, "{p:?}");
    // Stored premultiplied.
    assert!(c.pixels[0] <= 101);
}

#[test]
fn pixel_out_of_range_is_none() {
    let c = draw(vec![], 5, 5, 1.0);
    assert_eq!(c.pixel(5, 0), None);
    assert_eq!(c.pixel(0, 5), None);
    assert_eq!(c.pixel(u32::MAX, u32::MAX), None);
    assert_eq!(Canvas::default().pixel(0, 0), None);
}

#[test]
fn pixel_with_short_buffer_is_none() {
    let c = Canvas { width: 10, height: 10, pixels: vec![255; 8] };
    assert_eq!(c.pixel(1, 0), Some(Color::WHITE));
    assert_eq!(c.pixel(5, 5), None);
}

#[test]
fn demultiply_roundtrip() {
    assert_eq!(demultiply(0, 0, 0, 0), Color::TRANSPARENT);
    assert_eq!(demultiply(10, 20, 30, 255), Color::rgba(10, 20, 30, 255));
    assert_eq!(demultiply(64, 0, 128, 128), Color::rgba(128, 0, 255, 128));
    // Invalid premultiplied input (channel > alpha) clamps instead of overflowing.
    assert_eq!(demultiply(255, 255, 255, 1), Color::rgba(255, 255, 255, 1));
}

#[test]
fn zero_sized_canvas_is_empty() {
    assert!(draw(vec![], 0, 10, 1.0).is_empty());
    assert!(draw(vec![], 10, 0, 1.0).is_empty());
    assert!(draw(vec![], 0, 0, 1.0).to_png().is_empty());
}

#[test]
fn huge_canvas_is_refused() {
    let c = draw(vec![fill(0.0, 0.0, 10.0, 10.0, RED)], MAX_DIMENSION + 1, 1, 1.0);
    assert!(c.is_empty());
    assert_eq!((c.width, c.height), (0, 0));
    let c = draw(vec![], 1, MAX_DIMENSION + 1, 1.0);
    assert!(c.is_empty());
    let c = draw(vec![], u32::MAX, u32::MAX, 1.0);
    assert!(c.is_empty());
    // Area cap: 16384 × 16384 = 256 Mi is the limit, so this checks the arithmetic, not an allocation.
    assert!(byte_len(MAX_DIMENSION, MAX_DIMENSION).is_some());
    assert!(byte_len(MAX_DIMENSION + 1, 2).is_none());
}

#[test]
fn byte_len_checks() {
    assert_eq!(byte_len(3, 2), Some(24));
    assert_eq!(byte_len(0, 2), None);
    assert_eq!(byte_len(MAX_DIMENSION, 1), Some(MAX_DIMENSION as usize * 4));
}

#[test]
fn max_width_single_row_renders() {
    let c = draw(vec![fill(0.0, 0.0, 5.0, 1.0, RED)], MAX_DIMENSION, 1, 1.0);
    assert_eq!(c.width, MAX_DIMENSION);
    assert_eq!(c.pixel(2, 0), Some(RED));
    assert_eq!(c.pixel(MAX_DIMENSION - 1, 0), Some(BG));
}

// ---------------------------------------------------------------- PNG

#[test]
fn png_signature_and_header() {
    let c = draw(vec![fill(1.0, 1.0, 3.0, 3.0, RED)], 7, 5, 1.0);
    let png = c.to_png();
    assert!(png.len() > 33);
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[12..16], b"IHDR");
    assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 7);
    assert_eq!(u32::from_be_bytes([png[20], png[21], png[22], png[23]]), 5);
    assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
}

#[test]
fn png_roundtrip_pixels() {
    let c = draw(vec![fill(1.0, 1.0, 2.0, 2.0, RED)], 4, 4, 1.0);
    let back = tiny_skia::Pixmap::decode_png(&c.to_png()).unwrap();
    assert_eq!((back.width(), back.height()), (4, 4));
    assert_eq!(back.data(), &c.pixels[..]);
}

#[test]
fn png_of_inconsistent_canvas_is_empty() {
    assert!(Canvas { width: 4, height: 4, pixels: vec![0; 10] }.to_png().is_empty());
    assert!(Canvas { width: 4, height: 4, pixels: vec![0; 65] }.to_png().is_empty());
    assert!(Canvas::default().to_png().is_empty());
}

#[test]
fn unpremultiplied_export() {
    let c = draw(vec![fill(0.0, 0.0, 1.0, 1.0, RED)], 2, 1, 1.0);
    assert_eq!(c.to_rgba_unpremultiplied(), vec![255, 0, 0, 255, 255, 255, 255, 255]);
}

// ---------------------------------------------------------------- rectangles

#[test]
fn fill_rect_exact_pixels() {
    let c = draw(vec![fill(2.0, 3.0, 4.0, 5.0, RED)], 10, 10, 1.0);
    for y in 0..10 {
        for x in 0..10 {
            let inside = (2..6).contains(&x) && (3..8).contains(&y);
            assert_eq!(c.pixel(x, y), Some(if inside { RED } else { BG }), "({x},{y})");
        }
    }
}

#[test]
fn fill_rect_half_pixel_is_antialiased() {
    let c = draw(vec![fill(0.5, 0.0, 1.0, 1.0, Color::BLACK)], 3, 1, 1.0);
    let p = c.pixel(0, 0).unwrap();
    assert!(p.r > 100 && p.r < 155, "{p:?}");
    let p = c.pixel(1, 0).unwrap();
    assert!(p.r > 100 && p.r < 155, "{p:?}");
    assert_eq!(c.pixel(2, 0), Some(BG));
}

#[test]
fn fill_rect_translucent_blends() {
    let c = draw(vec![fill(0.0, 0.0, 2.0, 2.0, Color::rgba(0, 0, 0, 128))], 2, 2, 1.0);
    let p = c.pixel(0, 0).unwrap();
    assert_eq!(p.a, 255);
    assert!(p.r.abs_diff(127) <= 2, "{p:?}");
}

#[test]
fn transparent_fill_is_noop() {
    let c = draw(vec![fill(0.0, 0.0, 5.0, 5.0, Color::TRANSPARENT)], 5, 5, 1.0);
    assert_eq!(count_non_bg(&c), 0);
}

#[test]
fn later_items_paint_over_earlier() {
    let c = draw(vec![fill(0.0, 0.0, 4.0, 4.0, RED), fill(1.0, 1.0, 2.0, 2.0, BLUE)], 4, 4, 1.0);
    assert_eq!(c.pixel(0, 0), Some(RED));
    assert_eq!(c.pixel(1, 1), Some(BLUE));
    assert_eq!(c.pixel(3, 3), Some(RED));
}

#[test]
fn negative_and_partial_rects() {
    let c = draw(
        vec![
            fill(-5.0, -5.0, 7.0, 7.0, RED),
            fill(3.0, 3.0, -2.0, 2.0, BLUE),
            fill(8.0, 8.0, 100.0, 100.0, BLUE),
        ],
        10,
        10,
        1.0,
    );
    assert_eq!(c.pixel(0, 0), Some(RED));
    assert_eq!(c.pixel(1, 1), Some(RED));
    assert_eq!(c.pixel(2, 2), Some(BG));
    assert_eq!(c.pixel(9, 9), Some(BLUE));
    assert_eq!(c.pixel(7, 7), Some(BG));
}

#[test]
fn stroke_rect_draws_inside_border() {
    let c = draw(
        vec![DisplayItem::StrokeRect { rect: Rect::new(1.0, 1.0, 6.0, 5.0), color: RED, width: 1.0 }],
        10,
        10,
        1.0,
    );
    // Border pixels.
    for x in 1..7 {
        assert_eq!(c.pixel(x, 1), Some(RED), "top {x}");
        assert_eq!(c.pixel(x, 5), Some(RED), "bottom {x}");
    }
    for y in 1..6 {
        assert_eq!(c.pixel(1, y), Some(RED), "left {y}");
        assert_eq!(c.pixel(6, y), Some(RED), "right {y}");
    }
    // Interior and exterior untouched.
    assert_eq!(c.pixel(3, 3), Some(BG));
    assert_eq!(c.pixel(0, 0), Some(BG));
    assert_eq!(c.pixel(7, 3), Some(BG));
    assert_eq!(count_non_bg(&c), 2 * 6 + 2 * 3);
}

#[test]
fn thick_stroke_rect_fills_when_border_covers_it() {
    let c = draw(
        vec![DisplayItem::StrokeRect { rect: Rect::new(0.0, 0.0, 4.0, 4.0), color: RED, width: 3.0 }],
        6,
        6,
        1.0,
    );
    assert_eq!(c.pixel(2, 2), Some(RED));
    assert_eq!(count_non_bg(&c), 16);
}

#[test]
fn stroke_rect_zero_width_is_noop() {
    let c = draw(
        vec![DisplayItem::StrokeRect { rect: Rect::new(0.0, 0.0, 4.0, 4.0), color: RED, width: 0.0 }],
        6,
        6,
        1.0,
    );
    assert_eq!(count_non_bg(&c), 0);
}

// ---------------------------------------------------------------- lines

#[test]
fn horizontal_line_is_crisp() {
    let c = draw(vec![line(2.0, 5.0, 8.0, 5.0, 1.0)], 10, 10, 1.0);
    // Exactly one fully coloured row (5 - 0.5 rounds to the row containing y = 5).
    let rows: Vec<u32> = (0..10).filter(|&y| row_non_bg(&c, y) > 0).collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let y = rows[0];
    for x in 2..8 {
        assert_eq!(c.pixel(x, y), Some(BLUE));
    }
    assert_eq!(c.pixel(1, y), Some(BG));
    assert_eq!(c.pixel(8, y), Some(BG));
}

#[test]
fn vertical_line_is_crisp() {
    let c = draw(vec![line(4.5, 1.0, 4.5, 9.0, 1.0)], 10, 10, 1.0);
    for y in 1..9 {
        assert_eq!(c.pixel(4, y), Some(BLUE), "{y}");
        assert_eq!(c.pixel(3, y), Some(BG));
        assert_eq!(c.pixel(5, y), Some(BG));
    }
    assert_eq!(c.pixel(4, 0), Some(BG));
    assert_eq!(c.pixel(4, 9), Some(BG));
}

#[test]
fn thick_horizontal_line_centred() {
    let c = draw(vec![line(0.0, 5.0, 10.0, 5.0, 4.0)], 10, 10, 1.0);
    for y in 0..10 {
        let expect = if (3..7).contains(&y) { BLUE } else { BG };
        assert_eq!(c.pixel(5, y), Some(expect), "{y}");
    }
}

#[test]
fn reversed_line_endpoints() {
    let c = draw(vec![line(8.0, 5.5, 2.0, 5.5, 1.0)], 10, 10, 1.0);
    assert_eq!(c.pixel(5, 5), Some(BLUE));
    assert_eq!(row_non_bg(&c, 5), 6);
}

#[test]
fn diagonal_line_is_antialiased() {
    let c = draw(vec![line(0.0, 0.0, 20.0, 20.0, 2.0)], 20, 20, 1.0);
    // Pixels on the diagonal are coloured, far corners are not.
    for i in 2..18 {
        let p = c.pixel(i, i).unwrap();
        assert!(p.b > 200 && p.r < 120, "({i},{i}) {p:?}");
    }
    assert_eq!(c.pixel(19, 0), Some(BG));
    assert_eq!(c.pixel(0, 19), Some(BG));
    // Some partially covered pixels exist (antialiasing).
    let partial = (0..20u32)
        .flat_map(|y| (0..20u32).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let p = c.pixel(x, y).unwrap();
            p != BG && p != BLUE
        })
        .count();
    assert!(partial > 0);
}

#[test]
fn zero_length_and_zero_width_lines_are_noops() {
    let c = draw(vec![line(3.0, 3.0, 3.0, 3.0, 2.0), line(0.0, 1.0, 9.0, 1.0, 0.0)], 10, 10, 1.0);
    assert_eq!(count_non_bg(&c), 0);
}

#[test]
fn line_far_outside_is_clipped() {
    let c = draw(vec![line(-1e9, -1e9 + 1.0, 1e9, 1e9, 2.0)], 16, 16, 1.0);
    // The near-diagonal still crosses the canvas.
    assert!(count_non_bg(&c) > 0);
    let c = draw(vec![line(1e9, 5.0, 2e9, 50.0, 2.0)], 16, 16, 1.0);
    assert_eq!(count_non_bg(&c), 0);
}

#[test]
fn clip_segment_cases() {
    let bx = (0.0, 0.0, 10.0, 10.0);
    assert_eq!(paint::clip_segment((1.0, 1.0), (2.0, 2.0), bx), Some(((1.0, 1.0), (2.0, 2.0))));
    assert_eq!(paint::clip_segment((-10.0, 5.0), (20.0, 5.0), bx), Some(((0.0, 5.0), (10.0, 5.0))));
    assert_eq!(paint::clip_segment((-10.0, -10.0), (-5.0, 20.0), bx), None);
    assert_eq!(paint::clip_segment((20.0, 20.0), (30.0, 30.0), bx), None);
}

// ---------------------------------------------------------------- scale

#[test]
fn scale_two_doubles_size() {
    let c1 = draw(vec![fill(1.0, 1.0, 3.0, 2.0, RED)], 20, 20, 1.0);
    let c2 = draw(vec![fill(1.0, 1.0, 3.0, 2.0, RED)], 20, 20, 2.0);
    assert_eq!(count_non_bg(&c1), 6);
    assert_eq!(count_non_bg(&c2), 24);
    assert_eq!(c2.pixel(2, 2), Some(RED));
    assert_eq!(c2.pixel(7, 5), Some(RED));
    assert_eq!(c2.pixel(8, 5), Some(BG));
    assert_eq!(c2.pixel(7, 6), Some(BG));
    assert_eq!(c2.pixel(1, 1), Some(BG));
}

#[test]
fn scale_two_doubles_line_width() {
    let c = draw(vec![line(0.0, 5.0, 10.0, 5.0, 1.0)], 20, 20, 2.0);
    let rows: Vec<u32> = (0..20).filter(|&y| row_non_bg(&c, y) > 0).collect();
    assert_eq!(rows, vec![9, 10]);
    assert_eq!(row_non_bg(&c, 9), 20);
}

#[test]
fn scale_two_doubles_stroke_rect() {
    let c = draw(
        vec![DisplayItem::StrokeRect { rect: Rect::new(1.0, 1.0, 4.0, 4.0), color: RED, width: 1.0 }],
        12,
        12,
        2.0,
    );
    assert_eq!(c.pixel(2, 2), Some(RED));
    assert_eq!(c.pixel(3, 3), Some(RED));
    assert_eq!(c.pixel(4, 4), Some(BG));
    assert_eq!(c.pixel(9, 9), Some(RED));
    assert_eq!(c.pixel(10, 10), Some(BG));
}

#[test]
fn bad_scale_falls_back() {
    for s in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
        let c = draw(vec![fill(0.0, 0.0, 2.0, 2.0, RED)], 4, 4, s);
        assert_eq!(count_non_bg(&c), 4, "scale {s}");
    }
    assert_eq!(sanitize_scale(1000.0), 64.0);
    assert_eq!(sanitize_scale(1.5), 1.5);
}

// ---------------------------------------------------------------- text

fn bundled_has_outlines() -> Option<FontDb> {
    let db = FontDb::bundled();
    let font = db.resolve(mailcraft_fonts::UI_FAMILY, 400, false);
    let g = db.glyph_id(font, 'H');
    if g != 0 && !db.outline(font, g, 16.0).is_empty() { Some(db) } else { None }
}

#[test]
fn text_produces_some_non_background_pixels() {
    // Only meaningful once the fonts crate returns real outlines.
    let Some(db) = bundled_has_outlines() else {
        return;
    };
    let font = db.resolve(mailcraft_fonts::UI_FAMILY, 400, false);
    let mut item = text("Hello", 2.0, 20.0, 16.0, false, false);
    if let DisplayItem::Text { font: f, .. } = &mut item {
        *f = font;
    }
    let c = render(&DisplayList { items: vec![item] }, &db, 80, 30, 1.0, BG, &no_images);
    let n = count_non_bg(&c);
    assert!(n > 20, "only {n} pixels painted");
    // Ink stays near the line: nothing below descent depth.
    assert_eq!(row_non_bg(&c, 29), 0);
}

#[test]
fn text_without_outlines_does_not_paint_glyphs() {
    // Default FontDb has no faces: glyphs are empty, nothing is drawn, nothing panics.
    let c = draw(vec![text("Hello world", 2.0, 20.0, 16.0, false, false)], 80, 30, 1.0);
    assert_eq!(count_non_bg(&c), 0);
}

#[test]
fn underline_uses_metrics() {
    let db = FontDb::default();
    let m = db.metrics(FontId(0), 20.0);
    let width = db.measure(FontId(0), "abcd", 20.0);
    let c = draw(vec![text("abcd", 4.0, 20.0, 20.0, true, false)], 100, 40, 1.0);
    let rows: Vec<u32> = (0..40).filter(|&y| row_non_bg(&c, y) > 0).collect();
    assert!(!rows.is_empty());
    let center = 20.0 + m.underline_pos.abs();
    for &y in &rows {
        assert!((y as f32 - center).abs() <= m.underline_thickness.max(1.0) + 1.0, "row {y} vs {center}");
    }
    let painted = row_non_bg(&c, rows[0]);
    assert_eq!(painted as f32, width.round());
    assert_eq!(c.pixel(4, rows[0]), Some(Color::BLACK));
    assert_eq!(c.pixel(3, rows[0]), Some(BG));
}

#[test]
fn strike_is_above_baseline_and_underline_below() {
    let c = draw(vec![text("abcd", 4.0, 30.0, 20.0, false, true)], 100, 40, 1.0);
    let rows: Vec<u32> = (0..40).filter(|&y| row_non_bg(&c, y) > 0).collect();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|&y| y < 30 && y > 10), "{rows:?}");
    let c = draw(vec![text("abcd", 4.0, 30.0, 20.0, true, true)], 100, 40, 1.0);
    let rows: Vec<u32> = (0..40).filter(|&y| row_non_bg(&c, y) > 0).collect();
    assert!(rows.iter().any(|&y| y < 30) && rows.iter().any(|&y| y >= 30), "{rows:?}");
}

#[test]
fn decorations_scale() {
    let c1 = draw(vec![text("ab", 0.0, 10.0, 10.0, true, false)], 60, 40, 1.0);
    let c2 = draw(vec![text("ab", 0.0, 10.0, 10.0, true, false)], 60, 40, 2.0);
    let w1 = (0..40).map(|y| row_non_bg(&c1, y)).max().unwrap();
    let w2 = (0..40).map(|y| row_non_bg(&c2, y)).max().unwrap();
    assert_eq!(w2, w1 * 2);
}

#[test]
fn hostile_text_items() {
    let items = vec![
        text("x", f32::NAN, 10.0, 12.0, true, true),
        text("x", 10.0, f32::INFINITY, 12.0, true, true),
        text("x", 10.0, 10.0, f32::NAN, true, true),
        text("x", 10.0, 10.0, -12.0, true, true),
        text("x", 10.0, 10.0, 1e30, true, true),
        text("x", 1e30, 1e30, 12.0, true, true),
        text("\u{0}\u{7}\n\t\u{feff}\u{10ffff}", 1.0, 10.0, 12.0, true, true),
        text(&"W".repeat(20_000), -1e6, 10.0, 12.0, true, true),
        text("", 1.0, 10.0, 12.0, true, true),
    ];
    let c = draw(items, 40, 20, 1.0);
    assert_eq!(c.width, 40);
}

// ---------------------------------------------------------------- images

fn checker(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if (x + y) % 2 == 0 {
                v.extend_from_slice(&[255, 0, 0, 255]);
            } else {
                v.extend_from_slice(&[0, 0, 255, 255]);
            }
        }
    }
    v
}

#[test]
fn image_drawn_at_native_size() {
    let resolve = |src: &str| if src == "cid:a" { Some((2, 2, checker(2, 2))) } else { None };
    let list = DisplayList { items: vec![image(1.0, 1.0, 2.0, 2.0, "cid:a", "")] };
    let c = render(&list, &FontDb::default(), 4, 4, 1.0, BG, &resolve);
    assert_eq!(c.pixel(1, 1), Some(RED));
    assert_eq!(c.pixel(2, 1), Some(BLUE));
    assert_eq!(c.pixel(1, 2), Some(BLUE));
    assert_eq!(c.pixel(2, 2), Some(RED));
    assert_eq!(c.pixel(0, 0), Some(BG));
    assert_eq!(c.pixel(3, 3), Some(BG));
}

#[test]
fn image_scaled_into_rect() {
    let solid = |_: &str| Some((1, 1, vec![0, 255, 0, 255]));
    let list = DisplayList { items: vec![image(2.0, 2.0, 6.0, 4.0, "x", "")] };
    let c = render(&list, &FontDb::default(), 10, 10, 1.0, BG, &solid);
    let green = Color::rgb(0, 255, 0);
    for y in 2..6 {
        for x in 2..8 {
            assert_eq!(c.pixel(x, y), Some(green), "({x},{y})");
        }
    }
    assert_eq!(count_non_bg(&c), 24);
    // At scale 2 the image covers four times the pixels.
    let c = render(&list, &FontDb::default(), 20, 20, 2.0, BG, &solid);
    assert_eq!(count_non_bg(&c), 96);
}

#[test]
fn image_straight_alpha_is_composited() {
    let half = |_: &str| Some((1, 1, vec![0, 0, 0, 128]));
    let list = DisplayList { items: vec![image(0.0, 0.0, 2.0, 2.0, "x", "")] };
    let c = render(&list, &FontDb::default(), 2, 2, 1.0, BG, &half);
    let p = c.pixel(0, 0).unwrap();
    assert_eq!(p.a, 255);
    assert!(p.r.abs_diff(127) <= 2, "{p:?}");
}

#[test]
fn unresolved_image_draws_placeholder_frame() {
    let c = draw(vec![image(2.0, 2.0, 30.0, 20.0, "missing", "")], 40, 30, 1.0);
    // Frame corners and edges.
    assert_ne!(c.pixel(2, 2), Some(BG));
    assert_ne!(c.pixel(31, 21), Some(BG));
    assert_ne!(c.pixel(16, 2), Some(BG));
    assert_ne!(c.pixel(2, 12), Some(BG));
    // Outside untouched; bottom-right interior untouched.
    assert_eq!(c.pixel(1, 1), Some(BG));
    assert_eq!(c.pixel(32, 22), Some(BG));
    assert_eq!(c.pixel(25, 15), Some(BG));
    // Diagonal mark inside the small box near the top-left.
    assert_ne!(c.pixel(10, 10), Some(BG));
    let frame = c.pixel(16, 2).unwrap();
    assert!(frame.r > 150, "frame should be light: {frame:?}");
}

#[test]
fn tiny_placeholder_has_frame_only() {
    let c = draw(vec![image(1.0, 1.0, 8.0, 8.0, "missing", "alt text")], 12, 12, 1.0);
    assert_eq!(count_non_bg(&c), 4 * 8 - 4);
}

#[test]
fn bad_image_data_falls_back_to_placeholder() {
    let bad: Vec<Box<dyn Fn(&str) -> Option<(u32, u32, Vec<u8>)>>> = vec![
        Box::new(|_| Some((2, 2, vec![255; 15]))),
        Box::new(|_| Some((0, 2, vec![]))),
        Box::new(|_| Some((u32::MAX, u32::MAX, vec![1, 2, 3]))),
        Box::new(|_| Some((MAX_IMAGE_DIMENSION + 1, 1, vec![0; (MAX_IMAGE_DIMENSION as usize + 1) * 4]))),
    ];
    let list = DisplayList { items: vec![image(0.0, 0.0, 20.0, 20.0, "x", "")] };
    let reference = draw(list.items.clone(), 24, 24, 1.0);
    for resolve in &bad {
        let c = render(&list, &FontDb::default(), 24, 24, 1.0, BG, resolve.as_ref());
        assert_eq!(c.pixels, reference.pixels);
    }
}

#[test]
fn offscreen_image_does_not_query_resolver() {
    let calls = std::cell::Cell::new(0);
    let resolve = |_: &str| {
        calls.set(calls.get() + 1);
        None
    };
    let list = DisplayList {
        items: vec![image(5000.0, 5000.0, 10.0, 10.0, "x", ""), image(0.0, 0.0, 4.0, 4.0, "y", "")],
    };
    let _ = render(&list, &FontDb::default(), 10, 10, 1.0, BG, &resolve);
    assert_eq!(calls.get(), 1);
}

// ---------------------------------------------------------------- hostile display lists

fn hostile_floats() -> Vec<f32> {
    vec![
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        f32::MIN,
        f32::MIN_POSITIVE,
        -0.0,
        0.0,
        1e-30,
        1e20,
        -1e20,
        0.5,
        3.0,
        -3.0,
    ]
}

#[test]
fn hostile_display_lists_never_panic() {
    let fs = hostile_floats();
    let fonts = FontDb::bundled();
    let solid = |_: &str| Some((2, 2, checker(2, 2)));
    let mut items = Vec::new();
    for &a in &fs {
        for &b in &fs {
            items.push(fill(a, b, b, a, RED));
            items.push(DisplayItem::StrokeRect { rect: Rect::new(a, a, b, b), color: RED, width: b });
            items.push(DisplayItem::Line {
                from: Point::new(a, b),
                to: Point::new(b, a),
                color: BLUE,
                width: a,
            });
            items.push(text("Ag", a, b, b, true, true));
            items.push(image(a, b, a, b, "x", "alt"));
        }
    }
    let list = DisplayList { items };
    for scale in [1.0, 2.0, f32::NAN, 1e-6, 64.0] {
        let c = render(&list, &fonts, 32, 32, scale, BG, &solid);
        assert_eq!(c.pixels.len(), 32 * 32 * 4);
        let c = render(&list, &fonts, 32, 32, scale, BG, &no_images);
        assert_eq!(c.pixels.len(), 32 * 32 * 4);
    }
}

fn arb_f32() -> impl Strategy<Value = f32> {
    prop_oneof![
        -200.0f32..200.0,
        any::<f32>(),
        Just(f32::NAN),
        Just(f32::INFINITY),
        Just(f32::NEG_INFINITY),
        Just(0.0f32),
    ]
}

fn arb_color() -> impl Strategy<Value = Color> {
    any::<(u8, u8, u8, u8)>().prop_map(|(r, g, b, a)| Color::rgba(r, g, b, a))
}

fn arb_item() -> impl Strategy<Value = DisplayItem> {
    prop_oneof![
        (arb_f32(), arb_f32(), arb_f32(), arb_f32(), arb_color())
            .prop_map(|(x, y, w, h, color)| DisplayItem::FillRect { rect: Rect::new(x, y, w, h), color }),
        (arb_f32(), arb_f32(), arb_f32(), arb_f32(), arb_color(), arb_f32()).prop_map(
            |(x, y, w, h, color, width)| {
                DisplayItem::StrokeRect { rect: Rect::new(x, y, w, h), color, width }
            }
        ),
        (arb_f32(), arb_f32(), arb_f32(), arb_f32(), arb_color(), arb_f32()).prop_map(
            |(a, b, c, d, color, width)| {
                DisplayItem::Line { from: Point::new(a, b), to: Point::new(c, d), color, width }
            }
        ),
        (arb_f32(), arb_f32(), ".{0,12}", any::<u16>(), arb_f32(), arb_color(), any::<bool>(), any::<bool>())
            .prop_map(|(x, y, s, f, size_px, color, underline, strike)| DisplayItem::Text {
                origin: Point::new(x, y),
                text: s,
                font: FontId(f % 16),
                size_px,
                color,
                underline,
                strike,
                link: None,
            }),
        (arb_f32(), arb_f32(), arb_f32(), arb_f32(), ".{0,8}").prop_map(|(x, y, w, h, alt)| {
            DisplayItem::Image { rect: Rect::new(x, y, w, h), src: alt.clone(), alt }
        }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn random_display_lists_never_panic(
        items in proptest::collection::vec(arb_item(), 0..12),
        w in 0u32..48,
        h in 0u32..48,
        scale in arb_f32(),
        img in proptest::option::of((0u32..5, 0u32..5, proptest::collection::vec(any::<u8>(), 0..120))),
    ) {
        let fonts = FontDb::bundled();
        let resolve = |_: &str| img.clone();
        let c = render(&DisplayList { items }, &fonts, w, h, scale, BG, &resolve);
        if w > 0 && h > 0 {
            prop_assert_eq!(c.pixels.len(), (w * h * 4) as usize);
            let png = c.to_png();
            prop_assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        } else {
            prop_assert!(c.is_empty());
        }
    }

    #[test]
    fn random_canvas_bytes_pixel_and_png_never_panic(
        w in 0u32..20,
        h in 0u32..20,
        bytes in proptest::collection::vec(any::<u8>(), 0..2000),
        x in 0u32..25,
        y in 0u32..25,
    ) {
        let c = Canvas { width: w, height: h, pixels: bytes };
        let _ = c.pixel(x, y);
        let _ = c.to_png();
        let _ = c.to_rgba_unpremultiplied();
    }
}

// ---------------------------------------------------------------- review regressions

/// Rows (y) that contain any non-background pixel.
fn ink_rows(c: &Canvas) -> Vec<u32> {
    (0..c.height).filter(|&y| row_non_bg(c, y) > 0).collect()
}

#[test]
fn underline_top_sits_at_metric_position() {
    // `LineMetrics::underline_pos` is the distance from the baseline down to the *top* of the underline.
    // Default FontDb at 40 px: pos 4, thickness 40/14 ≈ 2.86 → 3 device px. Baseline 40 → rows 44, 45, 46.
    let db = FontDb::default();
    let m = db.metrics(FontId(0), 40.0);
    assert_eq!(m.underline_pos, 4.0);
    let c = draw(vec![text("ab", 2.0, 40.0, 40.0, true, false)], 80, 60, 1.0);
    assert_eq!(ink_rows(&c), vec![44, 45, 46]);
    // At scale 2 everything doubles: top at 88, 6 px thick.
    let c = draw(vec![text("ab", 2.0, 40.0, 40.0, true, false)], 160, 120, 2.0);
    assert_eq!(ink_rows(&c), (88..94).collect::<Vec<_>>());
}

#[test]
fn pen_advance_matches_layout_measure_with_fallback() {
    // Layout measures runs with `FontDb::measure` (fallback chosen by `FontDb::fallback_for`); the renderer
    // must advance the pen by the same amounts or glyphs drift away from carets, selections and decorations.
    let db = FontDb::bundled();
    let n = u16::try_from(db.faces.len()).unwrap();
    let mut checked = 0;
    'outer: for cp in 0x80u32..0x3000 {
        let Some(c) = char::from_u32(cp) else { continue };
        if mailcraft_fonts::is_zero_width(c) {
            continue;
        }
        for f in 0..n {
            let font = FontId(f);
            if db.has_glyph(font, c) || db.fallback_for(font, c) == font {
                continue;
            }
            let s: String = ['a', c, 'a', c, 'a'].iter().collect();
            let want = db.measure(font, &s, 20.0);
            let mut item = text(&s, 2.0, 20.0, 20.0, true, false);
            if let DisplayItem::Text { font: ff, .. } = &mut item {
                *ff = font;
            }
            // The underline spans the pen's travel, so its right end shows where the pen stopped.
            let c2 = render(&DisplayList { items: vec![item] }, &db, 200, 40, 1.0, BG, &no_images);
            let m = db.metrics(font, 20.0);
            let row = (20.0 + m.underline_pos).round() as u32;
            let painted = (0..200).filter(|&x| c2.pixel(x, row) != Some(BG)).max().map_or(0, |x| x + 1);
            let expect_right = (2.0 + want).round() as u32;
            assert_eq!(painted, expect_right, "font {f} char U+{cp:04X}: underline ends at {painted}");
            checked += 1;
            if checked >= 40 {
                break 'outer;
            }
        }
    }
    assert!(checked > 0, "no fallback chars found in bundled fonts");
}

#[test]
fn zero_width_chars_paint_nothing() {
    // Variation selectors, word joiners, soft hyphens etc. take no space; even when no face maps them (or a
    // face maps them to a visible glyph such as the soft hyphen), nothing may be drawn.
    let Some(db) = bundled_has_outlines() else {
        return;
    };
    let n = u16::try_from(db.faces.len()).unwrap();
    for f in 0..n {
        let font = FontId(f);
        let mut item =
            text("\u{FE0F}\u{2060}\u{00AD}\u{E0100}\u{200D}\u{FEFF}\u{200B}", 4.0, 20.0, 16.0, false, false);
        if let DisplayItem::Text { font: ff, .. } = &mut item {
            *ff = font;
        }
        let c = render(&DisplayList { items: vec![item] }, &db, 60, 30, 1.0, BG, &no_images);
        assert_eq!(count_non_bg(&c), 0, "face {f} drew ink for zero-width chars");
    }
}

#[test]
fn missing_glyph_draws_notdef_in_requested_face() {
    // A visible char no bundled face has is drawn as the run font's .notdef (some ink, not nothing).
    let Some(db) = bundled_has_outlines() else {
        return;
    };
    let font = db.resolve(mailcraft_fonts::UI_FAMILY, 400, false);
    let c = '\u{10FFFD}';
    if !db.outline(font, 0, 16.0).is_empty()
        && (0..db.faces.len()).all(|i| db.glyph_id(FontId(i as u16), c) == 0)
    {
        let mut item = text(&c.to_string(), 4.0, 20.0, 16.0, false, false);
        if let DisplayItem::Text { font: ff, .. } = &mut item {
            *ff = font;
        }
        let canvas = render(&DisplayList { items: vec![item] }, &db, 40, 30, 1.0, BG, &no_images);
        assert!(count_non_bg(&canvas) > 0);
    }
}

#[test]
fn stacked_combining_marks_are_bounded() {
    // 200k zero-advance marks on one spot: must finish quickly and keep the underline as long as the base.
    let db = FontDb::bundled();
    let font = db.resolve(mailcraft_fonts::UI_FAMILY, 400, false);
    let mut s = String::from("a");
    s.extend(std::iter::repeat_n('\u{0301}', 200_000));
    let mut item = text(&s, 2.0, 20.0, 16.0, true, true);
    if let DisplayItem::Text { font: ff, .. } = &mut item {
        *ff = font;
    }
    let start = std::time::Instant::now();
    let c = render(&DisplayList { items: vec![item] }, &db, 40, 30, 1.0, BG, &no_images);
    assert_eq!(c.pixels.len(), 40 * 30 * 4);
    assert!(start.elapsed().as_secs() < 20);
}
