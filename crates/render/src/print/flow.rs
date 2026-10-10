//! A vertical flow of display items (header blocks, laid-out bodies) and its pagination: page breaks fall
//! between text lines and never through an image, unless one line or image is taller than a whole page.

use mailcraft_geom::{Point, Rect};
use mailcraft_layout::{DisplayItem, Layout};

/// Most unbreakable bands scanned backwards from a page limit when looking for a straddling line.
const MAX_BACKSCAN: usize = 4_096;
/// Breaks closer than this to the slice start are not taken (prevents zero-height pages).
const MIN_SLICE: f32 = 1.0;
/// Most times a break is moved up past overlapping bands.
const MAX_REFINE: usize = 64;

#[derive(Clone, Debug, Default)]
pub(crate) struct Flow {
    pub items: Vec<DisplayItem>,
    /// Unbreakable vertical bands `(top, bottom)` (text lines, images, keep-together blocks).
    bands: Vec<(f32, f32)>,
    /// Forced page breaks (flow y where a new page must start).
    forced: Vec<f32>,
    pub height: f32,
}

impl Flow {
    /// Appends a layout (its own coordinates start at 0) at the current height, shifted right by `dx`.
    pub(crate) fn push_layout(&mut self, layout: &Layout, dx: f32) {
        let dy = self.height;
        for item in &layout.display.items {
            if let DisplayItem::Image { rect, .. } = item {
                self.bands.push((rect.y + dy, rect.y + rect.h + dy));
            }
            self.items.push(translate(item, dx, dy));
        }
        for l in &layout.index.lines {
            self.bands.push((l.rect.y + dy, l.rect.y + l.rect.h + dy));
        }
        let h = if layout.height.is_finite() { layout.height.max(0.0) } else { 0.0 };
        self.height += h;
    }

    /// Appends items drawn in a local box of height `h` (y from 0) as one unbreakable band.
    pub(crate) fn push_block(&mut self, items: Vec<DisplayItem>, h: f32) {
        let dy = self.height;
        self.items.extend(items.iter().map(|i| translate(i, 0.0, dy)));
        let h = if h.is_finite() { h.max(0.0) } else { 0.0 };
        self.bands.push((dy, dy + h));
        self.height += h;
    }

    pub(crate) fn gap(&mut self, h: f32) {
        if h.is_finite() && h > 0.0 {
            self.height += h;
        }
    }

    /// Forces the next content onto a new page.
    pub(crate) fn page_break(&mut self) {
        if self.height > 0.0 {
            self.forced.push(self.height);
        }
    }

    /// Splits the flow into `[start, end)` slices no taller than `avail` (at most `max` slices).
    pub(crate) fn slices(&mut self, avail: f32, max: usize) -> Vec<(f32, f32)> {
        let mut out = Vec::new();
        if !avail.is_finite() || avail <= MIN_SLICE {
            return out;
        }
        self.bands.retain(|(a, b)| a.is_finite() && b.is_finite());
        self.bands.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.forced.sort_by(f32::total_cmp);
        let max_h = self.bands.iter().map(|(a, b)| b - a).fold(0.0f32, f32::max);
        let mut start = 0.0f32;
        while start < self.height && out.len() < max {
            let mut limit = start + avail;
            if let Some(f) = self.forced.iter().copied().find(|f| *f > start + MIN_SLICE)
                && f < limit
            {
                limit = f;
                out.push((start, limit));
                start = limit;
                continue;
            }
            if limit >= self.height {
                out.push((start, self.height));
                break;
            }
            // Move the break up to the top of the bands straddling the limit, then again while that new
            // break is inside another band (side-by-side table cells overlap), as long as the page stays at
            // least half full; with no clean break that close, the first move is kept.
            let first = self.straddle_top(limit, start, max_h);
            let mut cut = first;
            for _ in 0..MAX_REFINE {
                let next = self.straddle_top(cut, start, max_h);
                if next >= cut {
                    break;
                }
                if next < start + avail * 0.5 {
                    cut = first;
                    break;
                }
                cut = next;
            }
            out.push((start, cut));
            start = cut;
        }
        out
    }

    /// The highest top of the bands that straddle `y` (start above it, end below it) and start more than
    /// [`MIN_SLICE`] below `start`; `y` when there are none. Bands must be sorted by top; `max_h` is the
    /// tallest band, which bounds how far back a straddling band can start.
    fn straddle_top(&self, y: f32, start: f32, max_h: f32) -> f32 {
        let idx = self.bands.partition_point(|b| b.0 < y);
        let mut cut = y;
        for &(top, bottom) in self.bands.get(..idx).unwrap_or(&[]).iter().rev().take(MAX_BACKSCAN) {
            if top + max_h < y {
                break;
            }
            if top < y && bottom > y && top > start + MIN_SLICE {
                cut = cut.min(top);
            }
        }
        cut
    }
}

/// `item` moved by (dx, dy).
pub(crate) fn translate(item: &DisplayItem, dx: f32, dy: f32) -> DisplayItem {
    let mv = |r: &Rect| Rect::new(r.x + dx, r.y + dy, r.w, r.h);
    let mp = |p: &Point| Point::new(p.x + dx, p.y + dy);
    match item {
        DisplayItem::FillRect { rect, color } => DisplayItem::FillRect { rect: mv(rect), color: *color },
        DisplayItem::StrokeRect { rect, color, width } => {
            DisplayItem::StrokeRect { rect: mv(rect), color: *color, width: *width }
        }
        DisplayItem::Line { from, to, color, width } => {
            DisplayItem::Line { from: mp(from), to: mp(to), color: *color, width: *width }
        }
        DisplayItem::Text { origin, text, font, size_px, color, underline, strike, link } => {
            DisplayItem::Text {
                origin: mp(origin),
                text: text.clone(),
                font: *font,
                size_px: *size_px,
                color: *color,
                underline: *underline,
                strike: *strike,
                link: link.clone(),
            }
        }
        DisplayItem::Image { rect, src, alt } => {
            DisplayItem::Image { rect: mv(rect), src: src.clone(), alt: alt.clone() }
        }
    }
}

/// The items of `items` that belong to the flow slice `[y0, y1)`, moved so that `y0` lands at `top` and shifted
/// right by `left`. Text and images go to the slice holding their baseline / top edge; rectangles and vertical
/// lines are clipped to the slice.
pub(crate) fn slice(items: &[DisplayItem], y0: f32, y1: f32, left: f32, top: f32) -> Vec<DisplayItem> {
    let dy = top - y0;
    let inside = |y: f32| y >= y0 && y < y1;
    let clip = |r: &Rect| -> Option<Rect> {
        let a = r.y.max(y0);
        let b = (r.y + r.h).min(y1);
        (b > a).then(|| Rect::new(r.x + left, a + dy, r.w, b - a))
    };
    let mut out = Vec::new();
    for item in items {
        match item {
            DisplayItem::Text { origin, .. } => {
                if inside(origin.y) {
                    out.push(translate(item, left, dy));
                }
            }
            DisplayItem::Image { rect, .. } => {
                if inside(rect.y) {
                    out.push(translate(item, left, dy));
                }
            }
            DisplayItem::FillRect { rect, color } => {
                if let Some(r) = clip(rect) {
                    out.push(DisplayItem::FillRect { rect: r, color: *color });
                }
            }
            DisplayItem::StrokeRect { rect, color, width } => {
                if let Some(r) = clip(rect) {
                    out.push(DisplayItem::StrokeRect { rect: r, color: *color, width: *width });
                }
            }
            DisplayItem::Line { from, to, color, width } => {
                let (ya, yb) = (from.y.min(to.y), from.y.max(to.y));
                if (yb - ya).abs() < 0.01 {
                    if inside(ya) {
                        out.push(translate(item, left, dy));
                    }
                } else if (to.x - from.x).abs() < 0.01 {
                    let a = ya.max(y0);
                    let b = yb.min(y1);
                    if b > a {
                        out.push(DisplayItem::Line {
                            from: Point::new(from.x + left, a + dy),
                            to: Point::new(to.x + left, b + dy),
                            color: *color,
                            width: *width,
                        });
                    }
                } else if inside(ya) {
                    out.push(translate(item, left, dy));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Side-by-side table cells give overlapping line bands. Moving a break up to the top of the line that
    /// straddles the page limit must not leave the break inside a neighbouring cell's line (that line's text
    /// would land on the next page with its top above the body area).
    #[test]
    fn breaks_never_fall_inside_any_line() {
        let mut f = Flow { height: 400.0, ..Flow::default() };
        // Left cell: lines at 180..197 and 197..214; right cell (offset by padding): 190..205 and 205..220.
        f.bands = vec![(180.0, 197.0), (197.0, 214.0), (190.0, 205.0), (205.0, 220.0)];
        let slices = f.slices(210.0, 10);
        assert!(slices.len() >= 2);
        for (start, end) in &slices {
            for (top, bottom) in &f.bands {
                assert!(
                    !(top < end && bottom > end && *top > start + MIN_SLICE),
                    "break {end} inside {top}..{bottom}"
                );
            }
        }
        assert_eq!(slices[0], (0.0, 180.0));
        // Chained overlaps resolve too, and the flow still advances.
        let mut g = Flow { height: 1000.0, ..Flow::default() };
        g.bands = (0..40).map(|i| (100.0 + i as f32 * 10.0, 115.0 + i as f32 * 10.0)).collect();
        let s = g.slices(300.0, 100);
        assert_eq!(s.first().map(|x| x.0), Some(0.0));
        assert_eq!(s.last().map(|x| x.1), Some(1000.0));
        for w in s.windows(2) {
            assert_eq!(w[0].1, w[1].0);
            assert!(w[0].1 > w[0].0);
        }
    }
}
