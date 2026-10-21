//! Text wrapping around floating drawings ([`mailcraft_model::Drawing::wrap`]).
//!
//! Each drawing that the text wraps around (Square, Tight, Through, Top and Bottom; not behind the text)
//! blocks horizontal bands of the page: Square blocks its box (turned with the drawing, plus a gap),
//! Tight and Through block the extent of the wrap outline in every 4 px band, Top and Bottom blocks the
//! whole width over its height. A line of top-level text (paragraphs, headings, list items, quotes; not
//! table cells) goes in the widest free stretch of its band; a band with no stretch wide enough is skipped
//! down to where the drawing ends. Drawings in a group or a canvas wrap with their group or canvas.

use mailcraft_model::Doc;
use mailcraft_model::rich::WrapStyle;

/// Gap between a wrapped drawing and the text beside it / above and below it (px at 100 %).
pub const WRAP_GAP_X: f32 = 9.0;
pub const WRAP_GAP_Y: f32 = 2.0;
/// Narrowest stretch of a line that text goes into beside a drawing (px at 100 %).
pub const MIN_STRETCH: f32 = 36.0;
/// Height of the bands a tight outline is cut into (px at 100 %).
const TIGHT_BAND: f32 = 4.0;
/// Most drawings that make the text wrap, and most outline points read per drawing (hostile documents).
const MAX_WRAPPING: usize = 200;
const MAX_WRAP_POINTS: usize = 512;
/// Largest coordinate taken from a drawing (px).
const MAX_COORD: f32 = 1.0e6;

/// The blocked bands of a page: `[top, bottom, left, right]` in layout px (zoom applied).
#[derive(Clone, Debug, Default)]
pub(crate) struct Floats {
    bands: Vec<[f32; 4]>,
    min_stretch: f32,
}

fn sane(v: f32) -> Option<f32> {
    (v.is_finite() && v.abs() <= MAX_COORD).then_some(v)
}

/// `p` turned `deg` degrees clockwise about `c`.
fn turn(p: (f32, f32), c: (f32, f32), deg: f32) -> (f32, f32) {
    if deg.abs() < 0.01 {
        return p;
    }
    let (s, co) = deg.to_radians().sin_cos();
    let (dx, dy) = (p.0 - c.0, p.1 - c.1);
    (c.0 + dx * co - dy * s, c.1 + dx * s + dy * co)
}

impl Floats {
    /// The bands the wrapping drawings of `doc` block, at `zoom`.
    pub(crate) fn of(doc: &Doc, zoom: f32) -> Floats {
        let mut bands = Vec::new();
        // A group's members wrap as the group; a canvas's drawings as the canvas.
        let wrapping = doc
            .drawings
            .iter()
            .filter(|d| !d.behind_text && d.wrap.wraps() && d.group.is_none() && d.canvas.is_none());
        for d in wrapping.take(MAX_WRAPPING) {
            let (Some(x), Some(y), Some(w), Some(h)) = (sane(d.x), sane(d.y), sane(d.w), sane(d.h)) else {
                continue;
            };
            let (w, h) = (w.max(0.0), h.max(0.0));
            let rot = if d.rotation.is_finite() { d.rotation } else { 0.0 };
            let c = (x + w / 2.0, y + h / 2.0);
            let tight = matches!(d.wrap, WrapStyle::Tight | WrapStyle::Through) && d.wrap_points.len() >= 3;
            if d.wrap == WrapStyle::TopBottom {
                let corners = [(x, y), (x + w, y), (x, y + h), (x + w, y + h)].map(|p| turn(p, c, rot));
                let top = corners.iter().map(|p| p.1).fold(f32::MAX, f32::min);
                let bottom = corners.iter().map(|p| p.1).fold(f32::MIN, f32::max);
                bands.push([
                    (top - WRAP_GAP_Y) * zoom,
                    (bottom + WRAP_GAP_Y) * zoom,
                    f32::NEG_INFINITY,
                    f32::INFINITY,
                ]);
            } else if tight {
                let pts: Vec<(f32, f32)> = d
                    .wrap_points
                    .iter()
                    .take(MAX_WRAP_POINTS)
                    .filter(|(a, b)| a.is_finite() && b.is_finite())
                    .map(|(fx, fy)| {
                        let fx = if d.flip_h { 1.0 - fx } else { *fx };
                        let fy = if d.flip_v { 1.0 - fy } else { *fy };
                        turn((x + fx.clamp(0.0, 1.0) * w, y + fy.clamp(0.0, 1.0) * h), c, rot)
                    })
                    .collect();
                tight_bands(&pts, zoom, &mut bands);
            } else {
                let corners = [(x, y), (x + w, y), (x, y + h), (x + w, y + h)].map(|p| turn(p, c, rot));
                let (x0, y0, x1, y1) =
                    corners.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |a, p| {
                        (a.0.min(p.0), a.1.min(p.1), a.2.max(p.0), a.3.max(p.1))
                    });
                bands.push([
                    (y0 - WRAP_GAP_Y) * zoom,
                    (y1 + WRAP_GAP_Y) * zoom,
                    (x0 - WRAP_GAP_X) * zoom,
                    (x1 + WRAP_GAP_X) * zoom,
                ]);
            }
        }
        bands.retain(|b| b[0].is_finite() && b[1].is_finite() && b[1] > b[0]);
        Floats { bands, min_stretch: MIN_STRETCH * zoom }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    /// Where a line from `top` to `bottom` goes within `[x0, x1]`: `Ok((x, w))`, the widest free stretch
    /// (the leftmost of equals), or `Err(y)`: no stretch is wide enough before `y`, where the next drawing
    /// in the way ends.
    pub(crate) fn stretch(&self, x0: f32, x1: f32, top: f32, bottom: f32) -> Result<(f32, f32), f32> {
        let full = (x0, (x1 - x0).max(0.0));
        let mut blocked: Vec<(f32, f32)> = self
            .bands
            .iter()
            .filter(|b| b[0] < bottom && b[1] > top && b[2] < x1 && b[3] > x0)
            .map(|b| (b[2], b[3]))
            .collect();
        if blocked.is_empty() {
            return Ok(full);
        }
        blocked.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut best: Option<(f32, f32)> = None;
        let mut at = x0;
        let consider = |from: f32, to: f32, best: &mut Option<(f32, f32)>| {
            let w = to.min(x1) - from.max(x0);
            if w > 0.0 && best.is_none_or(|(_, bw)| w > bw + 0.01) {
                *best = Some((from.max(x0), w));
            }
        };
        for (a, b) in &blocked {
            if *a > at {
                consider(at, *a, &mut best);
            }
            at = at.max(*b);
        }
        if at < x1 {
            consider(at, x1, &mut best);
        }
        let need = self.min_stretch.min(full.1);
        match best {
            Some((x, w)) if w >= need && w > 0.0 => Ok((x, w)),
            _ => {
                let next = self
                    .bands
                    .iter()
                    .filter(|b| b[0] < bottom && b[1] > top && b[2] < x1 && b[3] > x0)
                    .map(|b| b[1])
                    .fold(f32::MAX, f32::min);
                Err(if next.is_finite() && next > top { next } else { top + 1.0 })
            }
        }
    }
}

/// Bands of the outline `pts` (layout px before zoom): the outline's extent in every [`TIGHT_BAND`] px,
/// widened by the gap.
fn tight_bands(pts: &[(f32, f32)], zoom: f32, out: &mut Vec<[f32; 4]>) {
    let (lo, hi) = pts.iter().fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    if !(lo.is_finite() && hi.is_finite()) || hi <= lo {
        return;
    }
    let n = pts.len();
    let mut y = lo;
    let mut guard = 0usize;
    while y < hi && guard < 4_000 {
        let y1 = (y + TIGHT_BAND).min(hi);
        let (mut a, mut b) = (f32::MAX, f32::MIN);
        for i in 0..n {
            let (Some(p), Some(q)) = (pts.get(i), pts.get((i + 1) % n)) else { continue };
            // The part of edge p→q inside the band.
            let (ylo, yhi) = (p.1.min(q.1), p.1.max(q.1));
            if yhi < y || ylo > y1 {
                continue;
            }
            let at = |yy: f32| {
                if (q.1 - p.1).abs() < 1e-6 { p.0 } else { p.0 + (q.0 - p.0) * (yy - p.1) / (q.1 - p.1) }
            };
            let (c0, c1) = (ylo.max(y), yhi.min(y1));
            for x in [at(c0), at(c1)] {
                a = a.min(x);
                b = b.max(x);
            }
        }
        if a <= b {
            out.push([
                (y - WRAP_GAP_Y) * zoom,
                (y1 + WRAP_GAP_Y) * zoom,
                (a - WRAP_GAP_X) * zoom,
                (b + WRAP_GAP_X) * zoom,
            ]);
        }
        y = y1;
        guard += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::rich::Drawing;

    fn doc(ds: Vec<Drawing>) -> Doc {
        Doc { drawings: ds, ..Doc::default() }
    }

    #[test]
    fn a_square_drawing_leaves_the_wider_side() {
        let d = Drawing {
            id: 1,
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 50.0,
            wrap: WrapStyle::Square,
            ..Drawing::default()
        };
        let f = Floats::of(&doc(vec![d]), 1.0);
        let (x, w) = f.stretch(0.0, 400.0, 10.0, 20.0).unwrap();
        assert_eq!((x, w), (109.0, 291.0));
        assert_eq!(f.stretch(0.0, 400.0, 60.0, 70.0), Ok((0.0, 400.0)), "below it");
        // Too narrow beside it: skip to below the drawing.
        assert_eq!(f.stretch(0.0, 130.0, 10.0, 20.0), Err(52.0));
    }

    #[test]
    fn top_and_bottom_and_front_and_behind() {
        let tb = Drawing {
            id: 1,
            x: 300.0,
            y: 10.0,
            w: 20.0,
            h: 20.0,
            wrap: WrapStyle::TopBottom,
            ..Drawing::default()
        };
        let front = Drawing { id: 2, x: 0.0, y: 100.0, w: 20.0, h: 20.0, ..Drawing::default() };
        let behind =
            Drawing { id: 3, y: 200.0, wrap: WrapStyle::Square, behind_text: true, ..Drawing::default() };
        let f = Floats::of(&doc(vec![tb, front, behind]), 1.0);
        assert_eq!(f.stretch(0.0, 400.0, 15.0, 25.0), Err(32.0));
        assert_eq!(f.stretch(0.0, 400.0, 105.0, 115.0), Ok((0.0, 400.0)));
        assert_eq!(f.stretch(0.0, 400.0, 205.0, 215.0), Ok((0.0, 400.0)));
    }

    #[test]
    fn a_tight_outline_is_narrower_than_its_box() {
        // A diamond: at its top the text comes close.
        let d = Drawing {
            id: 1,
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
            wrap: WrapStyle::Tight,
            wrap_points: vec![(0.5, 0.0), (1.0, 0.5), (0.5, 1.0), (0.0, 0.5)],
            ..Drawing::default()
        };
        let f = Floats::of(&doc(vec![d]), 1.0);
        let (x, _) = f.stretch(0.0, 400.0, 0.0, 4.0).unwrap();
        assert!(x < 70.0, "{x}");
        let (x, _) = f.stretch(0.0, 400.0, 48.0, 52.0).unwrap();
        assert!(x > 105.0, "{x}");
    }

    fn laid(wrap: WrapStyle, text: &str) -> crate::Layout {
        let fonts = mailcraft_fonts::FontDb::bundled();
        let d = Drawing { id: 1, x: 0.0, y: 0.0, w: 120.0, h: 80.0, wrap, ..Drawing::default() };
        let mut doc = doc(vec![d]);
        doc.blocks.push(mailcraft_model::Block::text_paragraph(text));
        crate::layout_doc(&doc, &fonts, &crate::LayoutOptions { width: 500.0, ..Default::default() })
    }

    #[test]
    fn paragraph_lines_flow_around_a_square_drawing() {
        let text = "word ".repeat(200);
        let l = laid(WrapStyle::Square, &text);
        let lines = &l.index.lines;
        let first = lines.first().unwrap();
        assert!(first.rect.x >= 128.0 && first.rect.w <= 372.0, "{:?}", first.rect);
        let below = lines.iter().find(|l| l.rect.y > 90.0).unwrap();
        assert_eq!(below.rect.x, 0.0);
        assert_eq!(below.rect.w, 500.0);
        // Every character is still laid out, in order, once.
        assert_eq!(lines.first().unwrap().start, 0);
        for pair in lines.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        assert_eq!(lines.last().unwrap().end, text.chars().count());
        // In front of the text: no wrapping.
        let front = laid(WrapStyle::Front, &text);
        assert_eq!(front.index.lines.first().unwrap().rect.x, 0.0);
        assert!(front.height < l.height);
    }

    #[test]
    fn top_and_bottom_starts_the_text_below_the_drawing() {
        let l = laid(WrapStyle::TopBottom, "Some text");
        let first = l.index.lines.first().unwrap();
        assert!(first.rect.y >= 80.0, "{:?}", first.rect);
        assert_eq!(first.rect.x, 0.0);
        // A hard break at the end still opens an empty last line.
        let l = laid(WrapStyle::Square, "Line\n");
        assert_eq!(l.index.lines.len(), 2);
    }

    #[test]
    fn hostile_numbers_do_not_wrap() {
        let d = Drawing {
            id: 1,
            x: f32::NAN,
            y: 1e30,
            w: 5.0,
            h: 5.0,
            wrap: WrapStyle::Square,
            ..Drawing::default()
        };
        let f = Floats::of(&doc(vec![d]), 1.0);
        assert!(f.is_empty());
    }
}
