//! Border lines for paragraphs and tables: single, dotted, dashed, double and the 3-D styles, drawn as
//! [`DisplayItem::Line`]s so every renderer paints them without knowing about borders.

use mailcraft_geom::{Color, Point};
use mailcraft_model::{BorderEdge, BorderJoin, BorderLine, ParaBorders, TableBorders};

use crate::DisplayItem;
use crate::block::{Ctx, Out};

/// Dots and dashes drawn per edge at most; a longer edge is drawn solid.
const MAX_SEGMENTS: usize = 4096;
/// Colour of a box's shadow.
const SHADOW_COLOR: Color = Color::hex(0x808080);

/// The room an edge takes when drawn (px at 100 %): its thickness, but at least enough for every line of
/// the style to show (1 px a line, so a double line takes 3 and a groove or ridge 2).
pub(crate) fn drawn_thickness(e: &BorderEdge) -> f32 {
    let t = e.thickness_px();
    match e.line {
        BorderLine::Double => t.max(3.0),
        BorderLine::Groove | BorderLine::Ridge => t.max(2.0),
        _ => t.max(1.0),
    }
}

/// Which side of a box an edge is on (decides the shades of the 3-D styles).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

impl Side {
    fn horizontal(self) -> bool {
        matches!(self, Side::Top | Side::Bottom)
    }
    /// Top and left edges are in the light for `inset` / `groove` (drawn darker).
    fn lit(self) -> bool {
        matches!(self, Side::Top | Side::Left)
    }
}

/// Draws `edge` along one side: a stroke `t` px thick whose outer face starts at `at` (y for a horizontal
/// edge, x for a vertical one) and runs from `a` to `b` along the edge.
pub(crate) fn draw_edge(
    ctx: &Ctx<'_>,
    out: &mut Out,
    edge: &BorderEdge,
    side: Side,
    at: f32,
    a: f32,
    b: f32,
    t: f32,
) {
    if !(at.is_finite() && a.is_finite() && b.is_finite() && t.is_finite()) || b <= a || t <= 0.0 {
        return;
    }
    let color = edge.color.unwrap_or(ctx.default_color);
    let dark = color.lerp(Color::BLACK, 0.45);
    let light = color.lerp(Color::WHITE, 0.55);
    // A stroke of thickness `w` whose centre is `off` px in from the outer face.
    let mut stroke = |off: f32, w: f32, col: Color, pattern: Option<(f32, f32)>| {
        let c = at + off;
        let mk = |p: f32, q: f32| {
            if side.horizontal() {
                DisplayItem::Line { from: Point::new(p, c), to: Point::new(q, c), color: col, width: w }
            } else {
                DisplayItem::Line { from: Point::new(c, p), to: Point::new(c, q), color: col, width: w }
            }
        };
        match pattern {
            Some((on, off_len))
                if on > 0.0 && off_len > 0.0 && (b - a) / (on + off_len) < MAX_SEGMENTS as f32 =>
            {
                // Step by an integer segment index from `a`: adding `on + off` to an absolute coordinate
                // stops moving once the coordinate is large enough (f32), which would never end.
                let step = on + off_len;
                let n = ((b - a) / step).ceil().clamp(0.0, MAX_SEGMENTS as f32) as usize;
                for k in 0..n {
                    let p = a + k as f32 * step;
                    if p >= b {
                        break;
                    }
                    let q = (p + on).min(b);
                    out.push(mk(p, q));
                }
            }
            _ => {
                out.push(mk(a, b));
            }
        }
    };
    let half = t / 2.0;
    match edge.line {
        BorderLine::Single => stroke(half, t, color, None),
        BorderLine::Dotted => stroke(half, t, color, Some((t.max(1.0), t.max(1.0)))),
        BorderLine::Dashed => stroke(half, t, color, Some(((t * 3.0).max(3.0), (t * 2.0).max(2.0)))),
        BorderLine::Double => {
            let w = (t / 3.0).max(ctx.zoom.min(1.0));
            stroke(w / 2.0, w, color, None);
            stroke(t - w / 2.0, w, color, None);
        }
        BorderLine::Groove | BorderLine::Ridge => {
            // Two halves. `at` is the outer face of the top and left edges and the inner face of the bottom
            // and right ones, so a groove (dark outside top-left, dark inside bottom-right) always starts
            // dark from `at`; a ridge is the other way round.
            let (first, second) = if edge.line == BorderLine::Groove { (dark, light) } else { (light, dark) };
            let w = t / 2.0;
            stroke(w / 2.0, w, first, None);
            stroke(t - w / 2.0, w, second, None);
        }
        BorderLine::Inset | BorderLine::Outset => {
            let darker = (edge.line == BorderLine::Inset) == side.lit();
            stroke(half, t, if darker { dark } else { light }, None);
        }
    }
}

/// The parts of a paragraph border laid out around its text (px, zoomed).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BoxInsets {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

/// Room the borders take on each side of the text, padding included (px, zoomed).
pub(crate) fn para_insets(ctx: &Ctx<'_>, b: &ParaBorders, joined: BorderJoin) -> BoxInsets {
    let side =
        |e: &Option<BorderEdge>, pad: f32| e.as_ref().map_or(0.0, |e| (drawn_thickness(e) + pad) * ctx.zoom);
    BoxInsets {
        top: if joined.above { 0.0 } else { side(&b.top, ParaBorders::PAD_Y_PX) },
        bottom: if joined.below { 0.0 } else { side(&b.bottom, ParaBorders::PAD_Y_PX) },
        left: side(&b.left, ParaBorders::PAD_X_PX),
        right: side(&b.right, ParaBorders::PAD_X_PX),
    }
}

/// Draws a paragraph's borders around the box `x0..x1` × `y0..y1` (and its shadow).
pub(crate) fn draw_para_box(
    ctx: &Ctx<'_>,
    out: &mut Out,
    b: &ParaBorders,
    joined: BorderJoin,
    (x0, x1): (f32, f32),
    (y0, y1): (f32, f32),
    between_y: Option<f32>,
) {
    let th = |e: &BorderEdge| drawn_thickness(e) * ctx.zoom;
    if let Some(e) = b.top.as_ref().filter(|_| !joined.above) {
        draw_edge(ctx, out, e, Side::Top, y0, x0, x1, th(e));
    }
    if let Some(e) = b.bottom.as_ref().filter(|_| !joined.below) {
        let t = th(e);
        draw_edge(ctx, out, e, Side::Bottom, y1 - t, x0, x1, t);
    }
    // The line between paragraphs sharing the box: across the middle of the space below this one.
    if let (Some(e), Some(mid)) = (b.between.as_ref(), between_y.filter(|_| joined.below)) {
        let t = th(e);
        draw_edge(ctx, out, e, Side::Bottom, mid - t / 2.0, x0, x1, t);
    }
    if let Some(e) = &b.left {
        draw_edge(ctx, out, e, Side::Left, x0, y0, y1, th(e));
    }
    if let Some(e) = &b.right {
        let t = th(e);
        draw_edge(ctx, out, e, Side::Right, x1 - t, y0, y1, t);
    }
    if b.shadow && b.right.is_some() && b.bottom.is_some() {
        let s = 3.0 * ctx.zoom;
        let w = s;
        // Right shadow along the whole box after the last paragraph; the bottom one only there.
        let top = if joined.above { y0 } else { y0 + s };
        out.push(DisplayItem::Line {
            from: Point::new(x1 + w / 2.0, top),
            to: Point::new(x1 + w / 2.0, y1 + if joined.below { 0.0 } else { s }),
            color: SHADOW_COLOR,
            width: w,
        });
        if !joined.below {
            out.push(DisplayItem::Line {
                from: Point::new(x0 + s, y1 + w / 2.0),
                to: Point::new(x1 + s, y1 + w / 2.0),
                color: SHADOW_COLOR,
                width: w,
            });
        }
    }
}

/// Draws a table's borders over its grid: `col_x` / `row_y` are the column and row starts (relative to
/// `(x, y)`), `w` / `h` the table size; `cells` lists each placed cell as (row, col, rowspan, colspan).
pub(crate) fn draw_table_grid(
    ctx: &Ctx<'_>,
    out: &mut Out,
    b: &TableBorders,
    (x, y): (f32, f32),
    (col_x, row_y): (&[f32], &[f32]),
    (w, h): (f32, f32),
    cells: &[(usize, usize, usize, usize)],
) {
    let th = |e: &BorderEdge| drawn_thickness(e) * ctx.zoom;
    let at = |v: &[f32], i: usize, end: f32| v.get(i).copied().unwrap_or(end);
    // Outside edges: centred on the table's outline.
    if let Some(e) = &b.top {
        let t = th(e);
        draw_edge(ctx, out, e, Side::Top, y - t / 2.0, x, x + w, t);
    }
    if let Some(e) = &b.bottom {
        let t = th(e);
        draw_edge(ctx, out, e, Side::Bottom, y + h - t / 2.0, x, x + w, t);
    }
    if let Some(e) = &b.left {
        let t = th(e);
        draw_edge(ctx, out, e, Side::Left, x - t / 2.0, y, y + h, t);
    }
    if let Some(e) = &b.right {
        let t = th(e);
        draw_edge(ctx, out, e, Side::Right, x + w - t / 2.0, y, y + h, t);
    }
    // Inside lines: along each cell's top and left edge that is not on the outline (merged cells have no
    // line through them).
    for &(row, col, rowspan, colspan) in cells {
        let cx0 = x + at(col_x, col, w);
        let cx1 = x + at(col_x, col.saturating_add(colspan), w);
        let cy0 = y + at(row_y, row, h);
        let cy1 = y + at(row_y, row.saturating_add(rowspan), h);
        if row > 0
            && let Some(e) = &b.inside_h
        {
            let t = th(e);
            draw_edge(ctx, out, e, Side::Top, cy0 - t / 2.0, cx0, cx1, t);
        }
        if col > 0
            && let Some(e) = &b.inside_v
        {
            let t = th(e);
            draw_edge(ctx, out, e, Side::Left, cx0 - t / 2.0, cy0, cy1, t);
        }
    }
}
