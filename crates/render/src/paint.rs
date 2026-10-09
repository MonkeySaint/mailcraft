//! Display-item painting on a tiny-skia pixmap. Every coordinate is validated (finite, bounded) and clipped
//! to the canvas plus a small margin before it reaches tiny-skia, so hostile display lists cannot blow up the
//! rasteriser.

use std::collections::HashMap;

use mailcraft_fonts::{FontDb, FontId, PathCmd, UI_FAMILY, is_zero_width};
use mailcraft_geom::{Color, Point, Rect};
use mailcraft_layout::DisplayItem;
use tiny_skia::{
    FillRule, FilterQuality, IntSize, LineCap, Paint, PathBuilder, Pattern, Pixmap, SpreadMode, Stroke,
    Transform,
};

/// Device-pixel margin kept around the canvas when clipping (so antialiased edges and stroke caps that start
/// just outside still land correctly).
const MARGIN: f32 = 64.0;
/// Largest text size painted, in device pixels.
const MAX_TEXT_PX: f32 = 4096.0;
/// Glyph outline points further than this many ems from the pen are treated as garbage and dropped.
const MAX_OUTLINE_EMS: f32 = 8.0;
/// Upper bound on cached outlines / face lookups before the caches are flushed.
const CACHE_LIMIT: usize = 8192;
/// Most glyph outlines painted for one text item. Positive advances already limit what fits on the canvas;
/// this bounds runs of zero-advance glyphs (stacked combining marks) and sub-pixel text, which otherwise grow
/// the path without limit.
const MAX_RUN_GLYPHS: usize = 16_384;

/// Placeholder colours (light frame, mid-grey mark and alt text).
const PLACEHOLDER_FRAME: Color = Color::rgb(0xc6, 0xc6, 0xc6);
const PLACEHOLDER_MARK: Color = Color::rgb(0x9a, 0x9a, 0x9a);
const PLACEHOLDER_TEXT: Color = Color::rgb(0x59, 0x59, 0x59);
/// Logical size of the placeholder's mark box and its inset from the frame.
const MARK_SIZE: f32 = 10.0;
const MARK_INSET: f32 = 3.0;
const ALT_TEXT_PX: f32 = 11.0;

pub(crate) fn sk_color(c: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a)
}

fn solid(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c.r, c.g, c.b, c.a);
    p.anti_alias = true;
    p
}

fn finite(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_finite())
}

/// Clips the segment `a`–`b` to the box `[x0, x1]×[y0, y1]` (Liang–Barsky). `None` when fully outside.
pub(crate) fn clip_segment(
    a: (f32, f32),
    b: (f32, f32),
    bx: (f32, f32, f32, f32),
) -> Option<((f32, f32), (f32, f32))> {
    let (x0, y0, x1, y1) = bx;
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let mut t0 = 0.0f32;
    let mut t1 = 1.0f32;
    for (p, q) in [(-dx, a.0 - x0), (dx, x1 - a.0), (-dy, a.1 - y0), (dy, y1 - a.1)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                if r > t1 {
                    return None;
                }
                t0 = t0.max(r);
            } else {
                if r < t0 {
                    return None;
                }
                t1 = t1.min(r);
            }
        }
    }
    let pa = (a.0 + t0 * dx, a.1 + t0 * dy);
    let pb = (a.0 + t1 * dx, a.1 + t1 * dy);
    if finite(&[pa.0, pa.1, pb.0, pb.1]) { Some((pa, pb)) } else { None }
}

pub(crate) struct Painter<'a> {
    pm: &'a mut Pixmap,
    fonts: &'a FontDb,
    scale: f32,
    images: &'a dyn Fn(&str) -> Option<(u32, u32, Vec<u8>)>,
    /// Canvas size in device px.
    w: f32,
    h: f32,
    outlines: HashMap<(FontId, u32, u32), Vec<PathCmd>>,
    faces: HashMap<(FontId, char), (FontId, u32)>,
}

impl<'a> Painter<'a> {
    pub(crate) fn new(
        pm: &'a mut Pixmap,
        fonts: &'a FontDb,
        scale: f32,
        images: &'a dyn Fn(&str) -> Option<(u32, u32, Vec<u8>)>,
    ) -> Self {
        let (w, h) = (pm.width() as f32, pm.height() as f32);
        Painter { pm, fonts, scale, images, w, h, outlines: HashMap::new(), faces: HashMap::new() }
    }

    pub(crate) fn item(&mut self, item: &DisplayItem) {
        match item {
            DisplayItem::FillRect { rect, color } => self.fill_rect(*rect, *color),
            DisplayItem::StrokeRect { rect, color, width } => self.stroke_rect(*rect, *color, *width),
            DisplayItem::Line { from, to, color, width } => self.line(*from, *to, *color, *width),
            DisplayItem::Text { origin, text, font, size_px, color, underline, strike, .. } => {
                self.text(*origin, text, *font, *size_px, *color, *underline, *strike);
            }
            DisplayItem::Image { rect, src, alt } => self.image(*rect, src, alt),
        }
    }

    /// Logical rect → device `(left, top, right, bottom)`; `None` for non-finite or empty rects.
    fn dev_rect(&self, r: Rect) -> Option<(f32, f32, f32, f32)> {
        let s = self.scale;
        let (l, t) = (r.x * s, r.y * s);
        let (rr, b) = ((r.x + r.w) * s, (r.y + r.h) * s);
        if !finite(&[l, t, rr, b]) || rr <= l || b <= t {
            return None;
        }
        Some((l, t, rr, b))
    }

    /// The device-space box everything is clipped to.
    fn clip_box(&self, extra: f32) -> (f32, f32, f32, f32) {
        let m = MARGIN + extra;
        (-m, -m, self.w + m, self.h + m)
    }

    /// Fills a device-space rect (clipped, antialiased).
    fn fill_dev(&mut self, l: f32, t: f32, r: f32, b: f32, color: Color, paint: Option<&Paint>) {
        let (cl, ct, cr, cb) = self.clip_box(0.0);
        let (l, t, r, b) = (l.max(cl), t.max(ct), r.min(cr), b.min(cb));
        if !finite(&[l, t, r, b]) || r <= l || b <= t {
            return;
        }
        let Some(rect) = tiny_skia::Rect::from_ltrb(l, t, r, b) else {
            return;
        };
        let own;
        let paint = match paint {
            Some(p) => p,
            None => {
                own = solid(color);
                &own
            }
        };
        self.pm.fill_rect(rect, paint, Transform::identity(), None);
    }

    fn fill_rect(&mut self, rect: Rect, color: Color) {
        if color.a == 0 {
            return;
        }
        if let Some((l, t, r, b)) = self.dev_rect(rect) {
            self.fill_dev(l, t, r, b, color, None);
        }
    }

    /// Stroke thickness in whole device pixels (at least 1) for a logical width; `None` for unusable widths.
    fn snapped_width(&self, width: f32) -> Option<f32> {
        let d = width * self.scale;
        if !d.is_finite() || d <= 0.0 {
            return None;
        }
        Some(d.round().clamp(1.0, 2.0 * self.w.max(self.h) + 2.0 * MARGIN))
    }

    /// Border drawn inside `rect`, snapped to whole device pixels.
    fn stroke_rect(&mut self, rect: Rect, color: Color, width: f32) {
        if color.a == 0 {
            return;
        }
        let (Some((l, t, r, b)), Some(tw)) = (self.dev_rect(rect), self.snapped_width(width)) else {
            return;
        };
        self.frame_dev(l.round(), t.round(), r.round(), b.round(), tw, color);
    }

    fn frame_dev(&mut self, l: f32, t: f32, r: f32, b: f32, tw: f32, color: Color) {
        if r <= l || b <= t {
            return;
        }
        if 2.0 * tw >= r - l || 2.0 * tw >= b - t {
            self.fill_dev(l, t, r, b, color, None);
            return;
        }
        self.fill_dev(l, t, r, t + tw, color, None);
        self.fill_dev(l, b - tw, r, b, color, None);
        self.fill_dev(l, t + tw, l + tw, b - tw, color, None);
        self.fill_dev(r - tw, t + tw, r, b - tw, color, None);
    }

    fn line(&mut self, from: Point, to: Point, color: Color, width: f32) {
        if color.a == 0 {
            return;
        }
        let s = self.scale;
        let (ax, ay, bx, by) = (from.x * s, from.y * s, to.x * s, to.y * s);
        let wd = width * s;
        if !finite(&[ax, ay, bx, by, wd]) || wd <= 0.0 {
            return;
        }
        let Some(tw) = self.snapped_width(width) else {
            return;
        };
        if ay == by {
            // Horizontal rule: crisp rect centred on the line.
            let (l, r) = (ax.min(bx).round(), ax.max(bx).round());
            let top = (ay - tw / 2.0).round();
            self.fill_dev(l, top, r, top + tw, color, None);
        } else if ax == bx {
            let (t, b) = (ay.min(by).round(), ay.max(by).round());
            let left = (ax - tw / 2.0).round();
            self.fill_dev(left, t, left + tw, b, color, None);
        } else {
            let wd = wd.min(2.0 * self.w.max(self.h) + 2.0 * MARGIN);
            let Some((a, b)) = clip_segment((ax, ay), (bx, by), self.clip_box(wd)) else {
                return;
            };
            let mut pb = PathBuilder::new();
            pb.move_to(a.0, a.1);
            pb.line_to(b.0, b.1);
            let Some(path) = pb.finish() else {
                return;
            };
            let stroke = Stroke { width: wd, line_cap: LineCap::Butt, ..Stroke::default() };
            self.pm.stroke_path(&path, &solid(color), &stroke, Transform::identity(), None);
        }
    }

    /// Face and glyph used for `c`: the run font when it maps the char, else the face
    /// [`FontDb::fallback_for`] picks (the same face layout measured it with), else the run font's .notdef.
    fn face_for(&mut self, font: FontId, c: char) -> (FontId, u32) {
        if let Some(v) = self.faces.get(&(font, c)) {
            return *v;
        }
        let face = self.fonts.fallback_for(font, c);
        let found = (face, self.fonts.glyph_id(face, c));
        if self.faces.len() >= CACHE_LIMIT {
            self.faces.clear();
        }
        self.faces.insert((font, c), found);
        found
    }

    /// Appends glyph `glyph` of `font` at logical pen `(px, py)` to `pb` (device space).
    fn append_glyph(&mut self, pb: &mut PathBuilder, font: FontId, glyph: u32, size: f32, px: f32, py: f32) {
        let key = (font, glyph, size.to_bits());
        if !self.outlines.contains_key(&key) {
            if self.outlines.len() >= CACHE_LIMIT {
                self.outlines.clear();
            }
            let o = self.fonts.outline(font, glyph, size);
            self.outlines.insert(key, o);
        }
        let Some(cmds) = self.outlines.get(&key) else {
            return;
        };
        let s = self.scale;
        let lim = size * MAX_OUTLINE_EMS;
        let ok = |v: &[f32]| v.iter().all(|x| x.is_finite() && x.abs() <= lim);
        let tx = |x: f32| (px + x) * s;
        let ty = |y: f32| (py + y) * s;
        let mut open = false;
        for cmd in cmds {
            match *cmd {
                PathCmd::MoveTo(x, y) if ok(&[x, y]) => {
                    pb.move_to(tx(x), ty(y));
                    open = true;
                }
                PathCmd::LineTo(x, y) if open && ok(&[x, y]) => pb.line_to(tx(x), ty(y)),
                PathCmd::QuadTo(x1, y1, x, y) if open && ok(&[x1, y1, x, y]) => {
                    pb.quad_to(tx(x1), ty(y1), tx(x), ty(y));
                }
                PathCmd::CubicTo(x1, y1, x2, y2, x, y) if open && ok(&[x1, y1, x2, y2, x, y]) => {
                    pb.cubic_to(tx(x1), ty(y1), tx(x2), ty(y2), tx(x), ty(y));
                }
                PathCmd::Close if open => {
                    pb.close();
                    open = false;
                }
                _ => {}
            }
        }
        if open {
            pb.close();
        }
    }

    fn text(
        &mut self,
        origin: Point,
        text: &str,
        font: FontId,
        size: f32,
        color: Color,
        underline: bool,
        strike: bool,
    ) {
        let s = self.scale;
        let size_dev = size * s;
        if color.a == 0 || text.is_empty() || !finite(&[origin.x, origin.y, size_dev]) {
            return;
        }
        if size <= 0.0 || size_dev > MAX_TEXT_PX {
            return;
        }
        let (ox, oy) = (origin.x * s, origin.y * s);
        if !finite(&[ox, oy]) {
            return;
        }
        let reach = 2.0 * size_dev;
        if oy - reach > self.h + MARGIN || oy + reach < -MARGIN {
            return;
        }
        let max_adv = size * 16.0;
        let mut pb = PathBuilder::new();
        let mut pen = 0.0f32;
        let mut drawn = 0usize;
        for c in text.chars() {
            // Zero-width chars (controls, joiners, variation selectors, soft hyphens) take no space and are
            // never drawn, even when some face maps them to a visible glyph.
            if is_zero_width(c) {
                continue;
            }
            // Advance by exactly what layout measured (`FontDb::advance` on the run font applies the same
            // fallback as `face_for`), so glyphs line up with carets, selections and decorations.
            let adv = self.fonts.advance(font, c, size);
            let adv = if adv.is_finite() { adv.clamp(0.0, max_adv) } else { 0.0 };
            let x_dev = (origin.x + pen) * s;
            let visible = x_dev.is_finite() && x_dev <= self.w + MARGIN && x_dev + reach * 4.0 >= -MARGIN;
            if visible && !c.is_whitespace() && drawn < MAX_RUN_GLYPHS {
                drawn = drawn.saturating_add(1);
                let (face, glyph) = self.face_for(font, c);
                self.append_glyph(&mut pb, face, glyph, size, origin.x + pen, origin.y);
            }
            pen += adv;
            if !pen.is_finite() {
                break;
            }
        }
        let paint = solid(color);
        if let Some(path) = pb.finish() {
            self.pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
        if !(underline || strike) || pen <= 0.0 {
            return;
        }
        let m = self.fonts.metrics(font, size);
        let thick = if m.underline_thickness.is_finite() && m.underline_thickness > 0.0 {
            m.underline_thickness
        } else {
            size / 14.0
        };
        let Some(tw) = self.snapped_width(thick) else {
            return;
        };
        let (l, r) = (ox.round(), ((origin.x + pen) * s).round());
        if underline {
            let pos = if m.underline_pos.is_finite() { m.underline_pos.abs() } else { size * 0.1 };
            let pos = pos.min(size);
            // `underline_pos` is the distance from the baseline down to the line's top edge.
            let top = ((origin.y + pos) * s).round();
            self.fill_dev(l, top, r, top + tw, color, Some(&paint));
        }
        if strike {
            let asc =
                if m.ascent.is_finite() && m.ascent > 0.0 { m.ascent.min(size * 2.0) } else { size * 0.8 };
            let top = ((origin.y - asc * 0.3) * s - tw / 2.0).round();
            self.fill_dev(l, top, r, top + tw, color, Some(&paint));
        }
    }

    fn image(&mut self, rect: Rect, src: &str, alt: &str) {
        let Some((l, t, r, b)) = self.dev_rect(rect) else {
            return;
        };
        let (cl, ct, cr, cb) = self.clip_box(0.0);
        if r < cl || l > cr || b < ct || t > cb {
            return;
        }
        let decoded = (self.images)(src);
        if let Some((iw, ih, data)) = decoded
            && self.draw_image(l, t, r, b, iw, ih, data)
        {
            return;
        }
        self.placeholder(rect, l, t, r, b, alt);
    }

    /// Paints straight-alpha RGBA `data` (`iw`×`ih`) scaled into the device rect. `false` when the data is
    /// unusable.
    fn draw_image(&mut self, l: f32, t: f32, r: f32, b: f32, iw: u32, ih: u32, mut data: Vec<u8>) -> bool {
        if iw == 0 || ih == 0 || iw > crate::MAX_IMAGE_DIMENSION || ih > crate::MAX_IMAGE_DIMENSION {
            return false;
        }
        let Some(px) = u64::from(iw).checked_mul(u64::from(ih)) else {
            return false;
        };
        let Some(len) = px.checked_mul(4).and_then(|v| usize::try_from(v).ok()) else {
            return false;
        };
        if px > crate::MAX_IMAGE_PIXELS || data.len() != len {
            return false;
        }
        for p in data.as_chunks_mut::<4>().0 {
            let [r, g, b, a] = p;
            let al = u16::from(*a);
            if al != 255 {
                let pre = |c: u8| u8::try_from((u16::from(c) * al + 127) / 255).unwrap_or(255);
                *r = pre(*r);
                *g = pre(*g);
                *b = pre(*b);
            }
        }
        let Some(size) = IntSize::from_wh(iw, ih) else {
            return false;
        };
        let Some(img) = Pixmap::from_vec(data, size) else {
            return false;
        };
        let (sx, sy) = ((r - l) / iw as f32, (b - t) / ih as f32);
        if !finite(&[sx, sy]) || sx <= 0.0 || sy <= 0.0 {
            return false;
        }
        let exact = sx == 1.0 && sy == 1.0 && l.fract() == 0.0 && t.fract() == 0.0;
        let quality = if exact { FilterQuality::Nearest } else { FilterQuality::Bilinear };
        let shader = Pattern::new(
            img.as_ref(),
            SpreadMode::Pad,
            quality,
            1.0,
            Transform::from_row(sx, 0.0, 0.0, sy, l, t),
        );
        let paint = Paint { shader, anti_alias: true, ..Paint::default() };
        self.fill_dev(l, t, r, b, Color::BLACK, Some(&paint));
        true
    }

    /// Light frame, a small boxed diagonal mark and (when it fits) the alt text.
    fn placeholder(&mut self, rect: Rect, l: f32, t: f32, r: f32, b: f32, alt: &str) {
        let one = self.scale.round().max(1.0);
        self.frame_dev(l.round(), t.round(), r.round(), b.round(), one, PLACEHOLDER_FRAME);
        let need = MARK_SIZE + 2.0 * MARK_INSET;
        if rect.w < need || rect.h < need {
            return;
        }
        let mx = rect.x + MARK_INSET;
        let my = rect.y + MARK_INSET;
        let mark = Rect::new(mx, my, MARK_SIZE, MARK_SIZE);
        self.stroke_rect(mark, PLACEHOLDER_MARK, 1.0);
        self.line(Point::new(mx, my), Point::new(mx + MARK_SIZE, my + MARK_SIZE), PLACEHOLDER_MARK, 1.0);
        let avail = rect.w - need - MARK_INSET;
        if alt.is_empty() || avail <= ALT_TEXT_PX {
            return;
        }
        let font = self.fonts.resolve(UI_FAMILY, 400, false);
        let mut shown = String::new();
        let mut used = 0.0f32;
        for c in alt.chars().filter(|c| !c.is_control()) {
            let a = self.fonts.advance(font, c, ALT_TEXT_PX);
            let a = if a.is_finite() { a.max(0.0) } else { 0.0 };
            if used + a > avail {
                break;
            }
            used += a;
            shown.push(c);
        }
        if !shown.is_empty() {
            let baseline = my + MARK_SIZE;
            let x = mx + MARK_SIZE + MARK_INSET;
            self.text(Point::new(x, baseline), &shown, font, ALT_TEXT_PX, PLACEHOLDER_TEXT, false, false);
        }
    }
}
