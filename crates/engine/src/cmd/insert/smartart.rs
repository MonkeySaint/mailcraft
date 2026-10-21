//! Insert › SmartArt: diagrams drawn from a layout and a list of items (the text pane: one item per line,
//! indented with tabs or pairs of spaces for lower levels) in the theme's colours and effects, to PNG. The
//! recipe stays with the picture, so the graphic can be opened again and edited, and is redrawn when the
//! theme changes.

use mailcraft_geom::Color;
use mailcraft_model::rich::Align;
use mailcraft_model::{Block, Doc, Inline, ParaStyle, TextStyle};

use super::{
    EFFECT_MARGIN, Effect, Img, MAX_RASTER_SIDE, Mask, RASTER_SCALE, composite, dev, effect_style, lighten,
    polygon_list, translate,
};
use crate::compose::{ACCENT1, ResolvedTheme};

/// Layouts: (id, gallery label, category).
pub const LAYOUTS: [(&str, &str, &str); 10] = [
    ("list", "Vertical Box List", "List"),
    ("blocks", "Basic Block List", "List"),
    ("process", "Basic Process", "Process"),
    ("chevron", "Basic Chevron Process", "Process"),
    ("cycle", "Basic Cycle", "Cycle"),
    ("radial", "Basic Radial", "Cycle"),
    ("hierarchy", "Organization Chart", "Hierarchy"),
    ("venn", "Basic Venn", "Relationship"),
    ("matrix", "Basic Matrix", "Matrix"),
    ("pyramid", "Basic Pyramid", "Pyramid"),
];
/// Most items drawn.
pub const MAX_ITEMS: usize = 40;
/// Deepest level read from the text pane.
const MAX_LEVEL: usize = 5;

type P = (f32, f32);

/// The layout id for an id or gallery label.
pub fn layout_id(name: &str) -> Option<&'static str> {
    let n = name.trim();
    LAYOUTS
        .iter()
        .find(|(id, label, _)| id.eq_ignore_ascii_case(n) || label.eq_ignore_ascii_case(n))
        .map(|l| l.0)
}

/// An item's level: leading tabs, or pairs of leading spaces.
pub fn level_of(item: &str) -> usize {
    let mut level = 0usize;
    let mut spaces = 0usize;
    for ch in item.chars() {
        match ch {
            '\t' => level += 1,
            ' ' => spaces += 1,
            _ => break,
        }
    }
    (level + spaces / 2).min(MAX_LEVEL)
}

/// One shape of the diagram.
struct Node {
    pts: Vec<P>,
    /// Text area (x, y, w, h).
    text: (f32, f32, f32, f32),
    label: String,
    color: Color,
    opacity: f32,
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<P> {
    super::rounded_rect_pts(x, y, w, h, r)
}

fn circle(cx: f32, cy: f32, r: f32) -> Vec<P> {
    super::ellipse_pts(cx, cy, r, r)
}

fn node(pts: Vec<P>, text: (f32, f32, f32, f32), label: &str, color: Color) -> Node {
    Node { pts, text, label: label.to_owned(), color, opacity: 1.0 }
}

/// A rectangle node with its text inside.
fn boxed(x: f32, y: f32, w: f32, h: f32, label: &str, color: Color) -> Node {
    node(rounded(x, y, w, h, 6.0), (x + 4.0, y + 2.0, w - 8.0, h - 4.0), label, color)
}

/// Lays out a tree (levels from the text pane) as an organisation chart: boxes and elbow connectors.
fn hierarchy(items: &[(usize, String)], theme: &ResolvedTheme) -> (Vec<Node>, Vec<Vec<P>>, f32, f32) {
    let (bw, bh, gap, vgap) = (120.0f32, 44.0f32, 16.0f32, 34.0f32);
    // Parent of each item: the nearest earlier item with a lower level.
    let mut parent: Vec<Option<usize>> = Vec::with_capacity(items.len());
    for (i, (lvl, _)) in items.iter().enumerate() {
        let p = (0..i).rev().find(|j| items.get(*j).is_some_and(|(l, _)| l < lvl));
        parent.push(p);
    }
    let depth = |mut i: usize| {
        let mut d = 0usize;
        while let Some(Some(p)) = parent.get(i) {
            i = *p;
            d += 1;
            if d > MAX_LEVEL {
                break;
            }
        }
        d
    };
    let children = |i: Option<usize>| -> Vec<usize> {
        (0..items.len()).filter(|j| parent.get(*j).copied().flatten() == i).collect()
    };
    // Leaves take slots left to right; a parent sits over the middle of its children.
    let mut x = vec![0.0f32; items.len()];
    let mut next = 0.0f32;
    fn place(
        i: usize,
        x: &mut [f32],
        next: &mut f32,
        kids: &dyn Fn(Option<usize>) -> Vec<usize>,
        slot: f32,
        guard: usize,
    ) {
        let ks = kids(Some(i));
        if ks.is_empty() || guard > MAX_LEVEL {
            if let Some(v) = x.get_mut(i) {
                *v = *next;
            }
            *next += slot;
            return;
        }
        for k in &ks {
            place(*k, x, next, kids, slot, guard + 1);
        }
        let (a, b) = (
            ks.first().and_then(|k| x.get(*k)).copied().unwrap_or(0.0),
            ks.last().and_then(|k| x.get(*k)).copied().unwrap_or(0.0),
        );
        if let Some(v) = x.get_mut(i) {
            *v = (a + b) / 2.0;
        }
    }
    for r in children(None) {
        place(r, &mut x, &mut next, &children, bw + gap, 0);
    }
    let width = (next - gap + 20.0).max(bw + 20.0);
    let mut nodes = Vec::new();
    let mut lines = Vec::new();
    let mut max_depth = 0usize;
    for (i, (_, label)) in items.iter().enumerate() {
        let d = depth(i);
        max_depth = max_depth.max(d);
        let (nx, ny) = (10.0 + x.get(i).copied().unwrap_or(0.0), 10.0 + d as f32 * (bh + vgap));
        nodes.push(boxed(nx, ny, bw, bh, label, theme.color(ACCENT1 + d % 6)));
        if let Some(Some(p)) = parent.get(i) {
            let (px, py) = (
                10.0 + x.get(*p).copied().unwrap_or(0.0) + bw / 2.0,
                10.0 + depth(*p) as f32 * (bh + vgap) + bh,
            );
            let mid = py + vgap / 2.0;
            lines.push(vec![(px, py), (px, mid), (nx + bw / 2.0, mid), (nx + bw / 2.0, ny)]);
        }
    }
    (nodes, lines, width, 20.0 + (max_depth + 1) as f32 * (bh + vgap) - vgap)
}

/// PNG of a SmartArt `layout` over `items` with the theme's colours and effects; (png, width, height).
pub fn png(layout: &str, items: &[String], theme: &ResolvedTheme) -> Option<(Vec<u8>, f32, f32)> {
    let items: Vec<(usize, String)> = items
        .iter()
        .filter(|t| !t.trim().is_empty())
        .take(MAX_ITEMS)
        .map(|t| (level_of(t), t.trim().to_owned()))
        .collect();
    let n = items.len();
    if n == 0 {
        return None;
    }
    let accent = |i: usize| theme.color(ACCENT1 + i % 6);
    let mut nodes: Vec<Node> = Vec::new();
    // Connectors: filled polygons and lines (grey).
    let mut polys: Vec<Vec<P>> = Vec::new();
    let mut lines: Vec<Vec<P>> = Vec::new();
    let (w, h) = match layout {
        "list" => {
            let mut y = 10.0;
            for (i, (lvl, t)) in items.iter().enumerate() {
                let indent = 30.0 * (*lvl).min(3) as f32;
                let bh = if *lvl == 0 { 40.0 } else { 30.0 };
                let col = if *lvl == 0 { accent(i) } else { lighten(theme.color(ACCENT1), 0.35) };
                nodes.push(boxed(10.0 + indent, y, 340.0 - indent, bh, t, col));
                y += bh + 8.0;
            }
            (360.0, y + 2.0)
        }
        "blocks" => {
            let cols = n.clamp(1, 3);
            for (i, (_, t)) in items.iter().enumerate() {
                let (cx, cy) = ((i % cols) as f32, (i / cols) as f32);
                nodes.push(boxed(10.0 + cx * 142.0, 10.0 + cy * 82.0, 130.0, 70.0, t, accent(i)));
            }
            (20.0 + cols as f32 * 142.0 - 12.0, 20.0 + n.div_ceil(cols) as f32 * 82.0 - 12.0)
        }
        "process" => {
            let n = n.min(8);
            for (i, (_, t)) in items.iter().take(n).enumerate() {
                let x = 10.0 + 150.0 * i as f32;
                nodes.push(boxed(x, 15.0, 115.0, 50.0, t, accent(i)));
                if i + 1 < n {
                    polys.push(vec![(x + 120.0, 30.0), (x + 142.0, 40.0), (x + 120.0, 50.0)]);
                }
            }
            (20.0 + 150.0 * n as f32 - 35.0, 80.0)
        }
        "chevron" => {
            let n = n.min(8);
            for (i, (_, t)) in items.iter().take(n).enumerate() {
                let x = 10.0 + 120.0 * i as f32;
                let notch = if i == 0 { 0.0 } else { 20.0 };
                let pts = vec![
                    (x, 10.0),
                    (x + 120.0, 10.0),
                    (x + 140.0, 38.0),
                    (x + 120.0, 66.0),
                    (x, 66.0),
                    (x + notch, 38.0),
                ];
                nodes.push(node(pts, (x + notch + 4.0, 12.0, 110.0 - notch, 52.0), t, accent(i)));
            }
            (20.0 + 120.0 * n as f32 + 20.0, 76.0)
        }
        "cycle" => {
            let n = n.min(12);
            let mut ring = super::ellipse_pts(210.0, 210.0, 142.0, 142.0);
            let mut inner = super::ellipse_pts(210.0, 210.0, 138.0, 138.0);
            inner.reverse();
            ring.extend(inner);
            polys.push(ring);
            for (i, (_, t)) in items.iter().take(n).enumerate() {
                let a = std::f32::consts::TAU * i as f32 / n as f32 - std::f32::consts::FRAC_PI_2;
                nodes.push(boxed(
                    210.0 + 140.0 * a.cos() - 55.0,
                    210.0 + 140.0 * a.sin() - 22.0,
                    110.0,
                    44.0,
                    t,
                    accent(i),
                ));
            }
            (420.0, 420.0)
        }
        "radial" => {
            let (cx, cy) = (220.0, 200.0);
            let others = n.saturating_sub(1).min(12);
            for i in 0..others {
                let a = std::f32::consts::TAU * i as f32 / others.max(1) as f32 - std::f32::consts::FRAC_PI_2;
                let (ox, oy) = (cx + 145.0 * a.cos(), cy + 145.0 * a.sin());
                lines.push(vec![(cx, cy), (ox, oy)]);
            }
            if let Some((_, t)) = items.first() {
                nodes.push(node(circle(cx, cy, 58.0), (cx - 44.0, cy - 30.0, 88.0, 60.0), t, accent(0)));
            }
            for (i, (_, t)) in items.iter().skip(1).take(others).enumerate() {
                let a = std::f32::consts::TAU * i as f32 / others.max(1) as f32 - std::f32::consts::FRAC_PI_2;
                let (ox, oy) = (cx + 145.0 * a.cos(), cy + 145.0 * a.sin());
                nodes.push(node(circle(ox, oy, 42.0), (ox - 32.0, oy - 24.0, 64.0, 48.0), t, accent(i + 1)));
            }
            (440.0, 400.0)
        }
        "hierarchy" => {
            let (ns, ls, w, h) = hierarchy(&items, theme);
            nodes = ns;
            lines = ls;
            (w, h)
        }
        "venn" => {
            let n = n.min(6);
            let (cx, cy) = (200.0, 170.0);
            let (dist, r) = match n {
                1 => (0.0, 110.0),
                2 => (60.0, 100.0),
                _ => (70.0, 95.0),
            };
            for (i, (_, t)) in items.iter().take(n).enumerate() {
                let a = std::f32::consts::TAU * i as f32 / n as f32 - std::f32::consts::FRAC_PI_2
                    + if n == 2 { std::f32::consts::FRAC_PI_2 } else { 0.0 };
                let (ox, oy) = (cx + dist * a.cos(), cy + dist * a.sin());
                let (tx, ty) = (cx + (dist + r * 0.45) * a.cos(), cy + (dist + r * 0.45) * a.sin());
                let mut nd = node(circle(ox, oy, r), (tx - 45.0, ty - 22.0, 90.0, 44.0), t, accent(i));
                nd.opacity = 0.6;
                nodes.push(nd);
            }
            (400.0, 340.0)
        }
        "matrix" => {
            let cols = (n as f32).sqrt().ceil().max(1.0) as usize;
            for (i, (_, t)) in items.iter().enumerate() {
                let (c, r) = ((i % cols) as f32, (i / cols) as f32);
                nodes.push(boxed(10.0 + c * 156.0, 10.0 + r * 96.0, 150.0, 90.0, t, accent(i)));
            }
            (20.0 + cols as f32 * 156.0 - 6.0, 20.0 + n.div_ceil(cols) as f32 * 96.0 - 6.0)
        }
        "pyramid" => {
            let n = n.min(10);
            let (base, lh) = (380.0f32, 56.0f32);
            let cx = 10.0 + base / 2.0;
            for (i, (_, t)) in items.iter().take(n).enumerate() {
                let (top_w, bot_w) = (base * i as f32 / n as f32, base * (i + 1) as f32 / n as f32);
                let (y0, y1) = (10.0 + lh * i as f32, 10.0 + lh * (i + 1) as f32 - 3.0);
                let pts = vec![
                    (cx - top_w / 2.0, y0),
                    (cx + top_w / 2.0, y0),
                    (cx + bot_w / 2.0, y1),
                    (cx - bot_w / 2.0, y1),
                ];
                let tw = (top_w + bot_w) / 2.0 * 0.8;
                nodes.push(node(
                    pts,
                    (cx - tw.max(60.0) / 2.0, y0 + 4.0, tw.max(60.0), lh - 10.0),
                    t,
                    accent(i),
                ));
            }
            (base + 20.0, 20.0 + lh * n as f32)
        }
        _ => return None,
    };
    let (w, h) = (w.clamp(40.0, MAX_RASTER_SIDE), h.clamp(40.0, MAX_RASTER_SIDE));
    draw(&nodes, &polys, &lines, w, h, theme)
}

/// Coverage of `label` wrapped and centred in `rect` on a `dw`×`dh` device canvas.
fn label_mask(
    label: &str,
    size: f32,
    font: &str,
    rect: (f32, f32, f32, f32),
    dw: usize,
    dh: usize,
    fonts: &mailcraft_fonts::FontDb,
) -> Mask {
    let (x, y, w, h) = rect;
    let style = TextStyle { size_pt: Some(size), font_family: Some(font.to_owned()), ..TextStyle::default() };
    let doc = Doc {
        blocks: vec![Block::Paragraph {
            style: ParaStyle { align: Align::Center, ..ParaStyle::default() },
            inlines: vec![Inline::Text { text: label.to_owned(), style }],
        }],
        ..Doc::default()
    };
    let lo = mailcraft_layout::LayoutOptions { width: w.max(8.0), ..Default::default() };
    let layout = mailcraft_layout::layout_doc(&doc, fonts, &lo);
    let mut list = layout.display;
    translate(&mut list, x, y + ((h - layout.height) / 2.0).max(0.0));
    let m = Mask::from_list(&list, dw, dh, fonts);
    let s = RASTER_SCALE;
    let (x0, y0, x1, y1) =
        ((x * s) as i64, (y * s) as i64, ((x + w) * s).ceil() as i64, ((y + h) * s).ceil() as i64);
    m.map(|px, py| if px >= x0 && px < x1 && py >= y0 && py < y1 { m.get(px, py) } else { 0 })
}

fn draw(
    nodes: &[Node],
    polys: &[Vec<P>],
    lines: &[Vec<P>],
    w: f32,
    h: f32,
    theme: &ResolvedTheme,
) -> Option<(Vec<u8>, f32, f32)> {
    let fonts = mailcraft_fonts::FontDb::bundled();
    let fx = effect_style(theme.effects);
    let mut img = Img::new(dev(w), dev(h));
    let grey = Color::rgb(0x9e, 0x9e, 0x9e);
    if !polys.is_empty() || !lines.is_empty() {
        let mut list = mailcraft_layout::DisplayList::default();
        for p in polys {
            polygon_list(p, &mut list);
        }
        for l in lines {
            for pair in l.windows(2) {
                if let [a, b] = pair {
                    list.items.push(mailcraft_layout::DisplayItem::Line {
                        from: mailcraft_geom::Point::new(a.0, a.1),
                        to: mailcraft_geom::Point::new(b.0, b.1),
                        color: Color::BLACK,
                        width: 2.0,
                    });
                }
            }
        }
        img.paint(&Mask::from_list(&list, dev(w), dev(h), &fonts), 1.0, &|_, _| grey);
    }
    // Each shape is drawn on its own small canvas (shape plus room for effects) and placed on the picture.
    let m = EFFECT_MARGIN / 2.0;
    for nd in nodes {
        let (x0, y0, x1, y1) =
            nd.pts.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |(a, b, c, d), p| {
                (a.min(p.0), b.min(p.1), c.max(p.0), d.max(p.1))
            });
        if !(x0.is_finite() && y0.is_finite()) {
            continue;
        }
        let (bx, by) = ((x0 - m).max(0.0), (y0 - m).max(0.0));
        let (lw, lh) = (x1 - bx + m, y1 - by + m);
        let local: Vec<P> = nd.pts.iter().map(|(x, y)| (x - bx, y - by)).collect();
        let mut list = mailcraft_layout::DisplayList::default();
        polygon_list(&local, &mut list);
        let mask = Mask::from_list(&list, dev(lw), dev(lh), &fonts);
        let at = ((bx * RASTER_SCALE) as usize, (by * RASTER_SCALE) as usize);
        composite(
            &mut img,
            at,
            &mask,
            nd.color,
            &Effect { reflection: false, glow: None, opacity: fx.opacity * nd.opacity, ..fx },
        );
        let (tx, ty, tw, th) = nd.text;
        let tm = label_mask(
            &nd.label,
            10.0,
            &theme.body_font,
            (tx - bx, ty - by, tw, th),
            dev(lw),
            dev(lh),
            &fonts,
        );
        img.paint_at(&tm, at, 1.0, &|_, _| Color::WHITE);
    }
    Some((img.to_png(), w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> ResolvedTheme {
        crate::compose::ThemeState::default().resolve()
    }

    #[test]
    fn every_layout_draws_with_its_own_look() {
        let items: Vec<String> = ["Plan", "\tScope", "\tBudget", "Build", "\tCode", "Ship"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut seen = std::collections::HashSet::new();
        for (id, label, _) in LAYOUTS {
            let (png, w, h) = png(id, &items, &theme()).unwrap_or_else(|| panic!("{id}"));
            assert!(png.starts_with(b"\x89PNG") && w > 0.0 && h > 0.0, "{id}");
            assert!(seen.insert(png), "{id} looks like another layout");
            assert_eq!(layout_id(label), Some(id));
        }
        assert!(png("list", &[], &theme()).is_none());
        assert!(png("nope", &items, &theme()).is_none());
    }

    #[test]
    fn levels_shape_the_organisation_chart() {
        assert_eq!(level_of("\t\tx"), 2);
        assert_eq!(level_of("    x"), 2);
        assert_eq!(level_of("\t\t\t\t\t\t\t\tx"), MAX_LEVEL);
        let items: Vec<(usize, String)> =
            vec![(0, "CEO".into()), (1, "A".into()), (2, "A1".into()), (1, "B".into())];
        let (nodes, lines, w, h) = hierarchy(&items, &theme());
        assert_eq!(nodes.len(), 4);
        assert_eq!(lines.len(), 3, "a connector to every box but the top");
        assert!(h > 150.0, "three levels deep: {h}");
        assert!(w >= 2.0 * 120.0, "two boxes side by side at the second level");
        // Many items stay within the picture limits.
        let many: Vec<String> = (0..200).map(|i| format!("{}Item {i}", "\t".repeat(i % 4))).collect();
        for (id, ..) in LAYOUTS {
            let (_, w, h) = png(id, &many, &theme()).unwrap();
            assert!(w <= MAX_RASTER_SIDE && h <= MAX_RASTER_SIDE, "{id}");
        }
    }
}
