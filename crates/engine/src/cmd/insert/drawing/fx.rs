//! Drawing looks beyond a flat colour: gradient fills (preset directions, light and dark variations, and
//! stop-by-stop gradients), picture fills, dashed outlines, Shape / Text Effects with options (shadow,
//! glow, soft edges, reflection, bevel, 3-D rotation) and Text Effects › Transform warps.
//!
//! Everything works on the coverage masks and premultiplied images of [`super::super`] at
//! [`super::super::RASTER_SCALE`] device pixels per px.

use mailcraft_geom::Color;
use mailcraft_model::rich::{Gradient, Reflection, ShadowKind, ShapeEffects};

use super::super::{Img, Mask, RASTER_SCALE, darken, lighten};
use super::P;

/// Gradient directions of Shape Fill › Gradient (each also as `dark:<direction>`, from the fill to a dark
/// shade of it).
pub const GRADIENT_DIRECTIONS: [&str; 9] =
    ["down", "up", "right", "left", "center", "downRight", "downLeft", "upRight", "upLeft"];

/// Whether `g` names a gradient preset (`""` = solid).
pub fn valid_gradient(g: &str) -> bool {
    let dir = g.strip_prefix("dark:").unwrap_or(g);
    g.is_empty() || GRADIENT_DIRECTIONS.contains(&dir)
}

/// The colour of preset gradient `kind` at device pixel (`x`, `y`) of the box (`x0`, `y0`)–(`x1`, `y1`):
/// light variations run from a light tint of `color` to `color` in the named direction, dark variations
/// from `color` to a dark shade of it.
pub fn preset_gradient(
    kind: &str,
    color: Color,
    (x0, y0, x1, y1): (f32, f32, f32, f32),
    x: f32,
    y: f32,
) -> Color {
    let (dark, dir) = match kind.strip_prefix("dark:") {
        Some(d) => (true, d),
        None => (false, kind),
    };
    let along = |v: f32, lo: f32, hi: f32| ((v - lo) / (hi - lo).max(1.0)).clamp(0.0, 1.0);
    let (tx, ty) = (along(x, x0, x1), along(y, y0, y1));
    let t = match dir {
        "down" => ty,
        "up" => 1.0 - ty,
        "right" => tx,
        "left" => 1.0 - tx,
        "downRight" => (tx + ty) / 2.0,
        "downLeft" => (1.0 - tx + ty) / 2.0,
        "upRight" => (tx + 1.0 - ty) / 2.0,
        "upLeft" => (2.0 - tx - ty) / 2.0,
        "center" => {
            let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            let (rx, ry) = (((x1 - x0) / 2.0).max(1.0), ((y1 - y0) / 2.0).max(1.0));
            (((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2)).sqrt().clamp(0.0, 1.0)
        }
        _ => return color,
    };
    if dark { color.lerp(darken(color, 0.45), t) } else { lighten(color, 0.65).lerp(color, t) }
}

/// The colour of a stop-by-stop gradient at device pixel (`x`, `y`) of the box.
pub fn stops_gradient(g: &Gradient, (x0, y0, x1, y1): (f32, f32, f32, f32), x: f32, y: f32) -> Color {
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    let (u, v) = ((x - x0) / w, (y - y0) / h);
    let t = if g.radial {
        (((u - 0.5) * 2.0).powi(2) + ((v - 0.5) * 2.0).powi(2)).sqrt() / std::f32::consts::SQRT_2
    } else {
        let a = if g.angle.is_finite() { g.angle.to_radians() } else { 0.0 };
        let (s, c) = a.sin_cos();
        // Projection on the direction, scaled so the box's corners span 0..1.
        let p = (u - 0.5) * c + (v - 0.5) * s;
        let half = (c.abs() + s.abs()) / 2.0;
        p / (2.0 * half.max(0.01)) + 0.5
    }
    .clamp(0.0, 1.0);
    stop_color(&g.stops, t)
}

/// The colour at `t` (0..=1) of `stops` (sorted or not).
pub fn stop_color(stops: &[mailcraft_model::rich::GradientStop], t: f32) -> Color {
    let mut s: Vec<_> = stops.iter().filter(|s| s.pos.is_finite()).take(16).copied().collect();
    s.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    let (Some(first), Some(last)) = (s.first(), s.last()) else { return Color::TRANSPARENT };
    if t <= first.pos {
        return first.color;
    }
    if t >= last.pos {
        return last.color;
    }
    for pair in s.windows(2) {
        if let [a, b] = pair
            && t >= a.pos
            && t <= b.pos
        {
            let k = (t - a.pos) / (b.pos - a.pos).max(1e-4);
            return a.color.lerp(b.color, k);
        }
    }
    last.color
}

/// A decoded picture fill (straight RGBA8).
#[derive(Clone, Debug)]
pub struct Picture {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Picture {
    /// The PNG in `b64` (base64), when it decodes.
    pub fn decode(b64: &str) -> Option<Picture> {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
        let canvas = mailcraft_render::svg::decode_png(&data)?;
        let (w, h) = (usize::try_from(canvas.width).ok()?, usize::try_from(canvas.height).ok()?);
        if w == 0 || h == 0 || canvas.pixels.len() != w.checked_mul(h)?.checked_mul(4)? {
            return None;
        }
        Some(Picture { w, h, px: canvas.pixels })
    }

    /// The colour at (`u`, `v`) (fractions of the picture), stretched.
    pub fn sample(&self, u: f32, v: f32) -> Color {
        let x = ((u.clamp(0.0, 1.0) * self.w as f32) as usize).min(self.w.saturating_sub(1));
        let y = ((v.clamp(0.0, 1.0) * self.h as f32) as usize).min(self.h.saturating_sub(1));
        let i = (y * self.w + x) * 4;
        match self.px.get(i..i + 4) {
            // Premultiplied: back to straight colour.
            Some([r, g, b, a]) if *a > 0 => {
                let k = 255.0 / f32::from(*a);
                let un = |c: u8| (f32::from(c) * k).round().clamp(0.0, 255.0) as u8;
                Color::rgba(un(*r), un(*g), un(*b), *a)
            }
            _ => Color::TRANSPARENT,
        }
    }
}

/// A picture fill as stored on a drawing: `data` (PNG, or another picture the caller turned into PNG)
/// scaled down to at most `max` px a side, base64.
pub fn store_picture(data: &[u8], max: u32) -> Option<String> {
    use base64::Engine as _;
    let canvas = mailcraft_render::svg::decode_png(data)?;
    let (w, h) = (canvas.width, canvas.height);
    if w == 0 || h == 0 {
        return None;
    }
    let k = (max as f32 / w.max(h) as f32).min(1.0);
    let (nw, nh) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
    let small = if k < 1.0 {
        let mut px = vec![0u8; (nw as usize).checked_mul(nh as usize)?.checked_mul(4)?];
        for y in 0..nh as usize {
            for x in 0..nw as usize {
                let sx = ((x as f32 + 0.5) / k) as usize;
                let sy = ((y as f32 + 0.5) / k) as usize;
                let si = (sy.min(h as usize - 1) * w as usize + sx.min(w as usize - 1)) * 4;
                let di = (y * nw as usize + x) * 4;
                if let (Some(src), Some(dst)) = (canvas.pixels.get(si..si + 4), px.get_mut(di..di + 4)) {
                    dst.copy_from_slice(src);
                }
            }
        }
        mailcraft_render::Canvas { width: nw, height: nh, pixels: px }
    } else {
        canvas
    };
    let png = small.to_png();
    (!png.is_empty()).then(|| base64::engine::general_purpose::STANDARD.encode(png))
}

/// Outline dash styles (`""` = solid).
pub const DASHES: [&str; 8] =
    ["", "roundDot", "squareDot", "dash", "dashDot", "longDash", "longDashDot", "longDashDotDot"];

/// Dash and gap lengths of `dash`, in line widths.
fn pattern(dash: &str) -> &'static [f32] {
    match dash {
        "roundDot" => &[1.0, 1.5],
        "squareDot" => &[1.0, 1.0],
        "dash" => &[4.0, 3.0],
        "dashDot" => &[4.0, 3.0, 1.0, 3.0],
        "longDash" => &[8.0, 3.0],
        "longDashDot" => &[8.0, 3.0, 1.0, 3.0],
        "longDashDotDot" => &[8.0, 3.0, 1.0, 3.0, 1.0, 3.0],
        _ => &[],
    }
}

/// The dashes of `paths` (closed when `closed`) for a `width` px line: the drawn pieces as open lines.
pub fn dashed(paths: &[Vec<P>], closed: bool, dash: &str, width: f32) -> Vec<Vec<P>> {
    let pat = pattern(dash);
    if pat.is_empty() {
        return paths.to_vec();
    }
    let unit = width.max(1.5);
    let mut out: Vec<Vec<P>> = Vec::new();
    for path in paths {
        let mut pts: Vec<P> = path.clone();
        if closed && let Some(first) = path.first() {
            pts.push(*first);
        }
        let mut k = 0usize;
        let mut left = pat.first().copied().unwrap_or(1.0) * unit;
        let mut on = true;
        let mut cur: Vec<P> = Vec::new();
        for pair in pts.windows(2) {
            let [a, b] = pair else { continue };
            let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
            if !len.is_finite() {
                continue;
            }
            let mut at = 0.0f32;
            if on && cur.is_empty() {
                cur.push(*a);
            }
            let mut guard = 0;
            while len - at > left && guard < 10_000 && out.len() < 50_000 {
                guard += 1;
                at += left;
                let t = at / len.max(1e-6);
                let p = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                if on {
                    cur.push(p);
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur = vec![p];
                }
                on = !on;
                k = (k + 1) % pat.len().max(1);
                left = pat.get(k).copied().unwrap_or(1.0) * unit;
            }
            left -= len - at;
            if on {
                cur.push(*b);
            }
        }
        if on && cur.len() >= 2 {
            out.push(cur);
        }
    }
    out
}

fn d(v: f32) -> i64 {
    (v * RASTER_SCALE).round() as i64
}

fn opacity(transparency: f32) -> f32 {
    if transparency.is_finite() { (1.0 - transparency).clamp(0.0, 1.0) } else { 1.0 }
}

/// Shadow offset (device px) of a shadow falling `distance` px at `angle` degrees.
fn offset(angle: f32, distance: f32) -> (i64, i64) {
    let a = if angle.is_finite() { angle.to_radians() } else { 0.0 };
    let dist = if distance.is_finite() { distance.clamp(0.0, 200.0) } else { 0.0 };
    (d(a.cos() * dist), d(a.sin() * dist))
}

/// The mask after Soft Edges.
pub(in crate::cmd::insert) fn soften(mask: &Mask, fx: &ShapeEffects) -> Mask {
    let r = if fx.soft_edges.is_finite() { fx.soft_edges.clamp(0.0, 50.0) } else { 0.0 };
    if r <= 0.0 {
        return mask.clone();
    }
    mask.eroded(d(r / 2.0).max(1)).blurred(d(r / 2.0).max(1))
}

/// Effects painted under the drawing: the outer or perspective shadow and the glow.
pub(in crate::cmd::insert) fn paint_under(img: &mut Img, mask: &Mask, base: Color, fx: &ShapeEffects) {
    if let Some(sh) = fx.shadow.filter(|s| s.kind != ShadowKind::Inner) {
        let color = sh.color.unwrap_or(Color::BLACK);
        let blur = d(if sh.blur.is_finite() { sh.blur.clamp(0.0, 60.0) } else { 4.0 } / 2.0).max(1);
        let shadow = if sh.kind == ShadowKind::Perspective {
            // Cast on the ground: squashed to a third of the height at the drawing's foot and slanted.
            let foot =
                mask.bounds().map_or(i64::try_from(mask.h).unwrap_or(0), |b| i64::try_from(b.3).unwrap_or(0));
            let (ox, _) = offset(sh.angle, sh.distance);
            mask.map(|x, y| {
                let up = foot - y;
                if up < 0 {
                    return 0;
                }
                let sy = foot - up * 3;
                let sx = x - up * 2 + ox;
                mask.get(sx, sy)
            })
        } else {
            let (ox, oy) = offset(sh.angle, sh.distance);
            mask.shifted(ox, oy)
        };
        img.paint(&shadow.blurred(blur), opacity(sh.transparency), &|_, _| color);
    }
    if let Some(g) = fx.glow {
        let size = if g.size.is_finite() { g.size.clamp(0.0, 60.0) } else { 8.0 };
        let color = g.color.unwrap_or_else(|| lighten(base, 0.4));
        let glow = mask.dilated(d(size / 2.0)).blurred(d(size / 2.0).max(1));
        img.paint(&glow, opacity(g.transparency), &|_, _| color);
    }
}

/// Bevel kinds (`""` = none).
pub const BEVELS: [&str; 13] = [
    "",
    "circle",
    "relaxedInset",
    "cross",
    "coolSlant",
    "angle",
    "softRound",
    "convex",
    "slope",
    "divot",
    "riblet",
    "hardEdge",
    "artDeco",
];

/// Effects painted over the drawing: the inner shadow and the bevel.
pub(in crate::cmd::insert) fn paint_over(img: &mut Img, mask: &Mask, base: Color, fx: &ShapeEffects) {
    if let Some(sh) = fx.shadow.filter(|s| s.kind == ShadowKind::Inner) {
        let color = sh.color.unwrap_or(Color::BLACK);
        let (ox, oy) = offset(sh.angle, sh.distance.max(1.0));
        let blur = d(if sh.blur.is_finite() { sh.blur.clamp(0.0, 60.0) } else { 4.0 } / 2.0).max(1);
        // Dark where the shape's edge casts into it, kept inside the shape.
        let edge = mask.minus(&mask.shifted(ox, oy)).blurred(blur);
        let inside = edge.map(|x, y| {
            let m = u32::from(mask.get(x, y));
            u8::try_from(u32::from(edge.get(x, y)) * m / 255).unwrap_or(255)
        });
        img.paint(&inside, opacity(sh.transparency), &|_, _| color);
    }
    if !fx.bevel.is_empty() {
        let (width, hi, lo, smooth, inverted) = match fx.bevel.as_str() {
            "circle" => (5.0, 0.55, 0.45, true, false),
            "relaxedInset" => (3.0, 0.4, 0.35, true, false),
            "cross" => (2.5, 0.6, 0.6, false, true),
            "coolSlant" => (4.0, 0.75, 0.5, false, false),
            "angle" => (3.0, 0.85, 0.6, false, false),
            "softRound" => (7.0, 0.4, 0.3, true, false),
            "convex" => (6.0, 0.6, 0.5, true, false),
            "slope" => (4.0, 0.6, 0.55, false, false),
            "divot" => (3.5, 0.5, 0.5, true, true),
            "riblet" => (2.0, 0.6, 0.6, false, false),
            "hardEdge" => (2.5, 0.9, 0.7, false, false),
            _ => (5.0, 0.6, 0.6, false, false),
        };
        let width =
            if fx.bevel_size.is_finite() && fx.bevel_size > 0.0 { fx.bevel_size.min(30.0) } else { width };
        let r = d(width).max(1);
        let soft = |m: Mask| if smooth { m.blurred((r / 2).max(1)) } else { m };
        let light = soft(mask.minus(&mask.shifted(r, r)));
        let shade = soft(mask.minus(&mask.shifted(-r, -r)));
        let (light, shade) = if inverted { (shade, light) } else { (light, shade) };
        let clip = |m: &Mask| {
            m.map(|x, y| {
                u8::try_from(u32::from(m.get(x, y)) * u32::from(mask.get(x, y)) / 255).unwrap_or(255)
            })
        };
        img.paint(&clip(&light), hi, &|_, _| Color::WHITE);
        img.paint(&clip(&shade), lo, &|_, _| darken(base, 0.55));
        if fx.bevel == "riblet" {
            let ribs = mask.map(|x, y| if (y / d(3.0).max(1)) % 2 == 0 { mask.get(x, y) / 4 } else { 0 });
            img.paint(&ribs, 1.0, &|_, _| darken(base, 0.3));
        }
        if fx.bevel == "convex" {
            let top = mask.map(|x, y| {
                let b = mask.bounds().map_or(0, |b| b.1 + (b.3.saturating_sub(b.1)) / 2);
                if usize::try_from(y).is_ok_and(|y| y < b) { mask.get(x, y) } else { 0 }
            });
            img.paint(&top, 0.2, &|_, _| Color::WHITE);
        }
    }
}

/// Extra height (px) a reflection needs below a drawing `h` px tall.
pub fn reflection_room(r: &Reflection, h: f32) -> f32 {
    let size = if r.size.is_finite() { r.size.clamp(0.0, 1.0) } else { 0.5 };
    let gap = if r.distance.is_finite() { r.distance.clamp(0.0, 100.0) } else { 0.0 };
    h * size + gap
}

/// Mirrors what is painted between device rows `top` and `axis` of `img` below `axis` (after a gap),
/// fading out (Reflection).
pub(in crate::cmd::insert) fn reflect(img: &mut Img, top: usize, axis: usize, r: &Reflection) {
    let size = if r.size.is_finite() { r.size.clamp(0.0, 1.0) } else { 0.5 };
    let gap = usize::try_from(d(if r.distance.is_finite() { r.distance.clamp(0.0, 100.0) } else { 0.0 }))
        .unwrap_or(0);
    let span = axis.saturating_sub(top);
    let len = ((span as f32) * size) as usize;
    if len == 0 {
        return;
    }
    let start = opacity(r.transparency);
    let w = img.w;
    let blur =
        usize::try_from(d(if r.blur.is_finite() { r.blur.clamp(0.0, 20.0) } else { 0.0 })).unwrap_or(0);
    for k in 0..len {
        let src_y = axis.saturating_sub(1).saturating_sub(k);
        let dst_y = axis.saturating_add(gap).saturating_add(k);
        if dst_y >= img.h || src_y < top {
            break;
        }
        let fade = start * (1.0 - k as f32 / len as f32);
        for x in 0..w {
            // A little horizontal blur for a soft reflection.
            let mut acc = [0.0f32; 4];
            let mut n = 0.0f32;
            for dx in 0..=(2 * blur) {
                let sx = (x + dx).checked_sub(blur);
                if let Some(p) = sx.filter(|sx| *sx < w).and_then(|sx| img.px.get(src_y * w + sx)) {
                    for (a, v) in acc.iter_mut().zip(p) {
                        *a += v;
                    }
                    n += 1.0;
                }
            }
            if n <= 0.0 {
                continue;
            }
            let src = acc.map(|v| v / n * fade);
            if let Some(dst) = img.px.get_mut(dst_y * w + x) {
                let a = src[3];
                for (dk, sk) in dst.iter_mut().zip(src) {
                    *dk = sk + *dk * (1.0 - a);
                }
            }
        }
    }
}

/// Solves the 8 unknowns of the homography mapping `from` to `to` (four point pairs).
fn homography(from: [(f64, f64); 4], to: [(f64, f64); 4]) -> Option<[f64; 9]> {
    let mut m: Vec<[f64; 9]> = Vec::with_capacity(8);
    for ((x, y), (u, v)) in from.iter().zip(to.iter()) {
        m.push([*x, *y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, *u]);
        m.push([0.0, 0.0, 0.0, *x, *y, 1.0, -v * x, -v * y, *v]);
    }
    let at = |m: &[[f64; 9]], r: usize, c: usize| m.get(r).and_then(|row| row.get(c)).copied().unwrap_or(0.0);
    // Gaussian elimination with partial pivoting.
    for col in 0..8 {
        let pivot = (col..8).max_by(|a, b| at(&m, *a, col).abs().total_cmp(&at(&m, *b, col).abs()))?;
        let p = at(&m, pivot, col);
        if p.abs() < 1e-9 {
            return None;
        }
        m.swap(col, pivot);
        let pivot_row = *m.get(col)?;
        for (r, row) in m.iter_mut().enumerate() {
            if r != col {
                let k = row.get(col).copied().unwrap_or(0.0) / p;
                for (cell, pv) in row.iter_mut().zip(pivot_row.iter()).skip(col) {
                    *cell -= k * pv;
                }
            }
        }
    }
    let mut h = [1.0f64; 9];
    for (i, slot) in h.iter_mut().take(8).enumerate() {
        *slot = at(&m, i, 8) / at(&m, i, i);
    }
    h.iter().all(|v| v.is_finite()).then_some(h)
}

/// `img` turned in 3-D (`fx.rot_x` about the vertical axis, `fx.rot_y` about the horizontal one, seen with
/// `fx.perspective` degrees of field of view, or in parallel) about device point (`cx`, `cy`): the new
/// image and where its corner lies relative to the old one (device px). `None` when there is no 3-D
/// rotation or it leaves nothing to see.
pub(in crate::cmd::insert) fn rotate_3d(
    img: &Img,
    fx: &ShapeEffects,
    cx: f32,
    cy: f32,
) -> Option<(Img, f32, f32)> {
    let fin = |v: f32| if v.is_finite() { v } else { 0.0 };
    let (rx, ry) = (fin(fx.rot_x).rem_euclid(360.0), fin(fx.rot_y).rem_euclid(360.0));
    if rx.abs() < 0.01 && ry.abs() < 0.01 {
        return None;
    }
    let (a, b) = (f64::from(rx).to_radians(), f64::from(ry).to_radians());
    let persp = f64::from(fin(fx.perspective).clamp(0.0, 120.0));
    let (w, h) = (img.w as f64, img.h as f64);
    let (cx, cy) = (f64::from(cx), f64::from(cy));
    let f = if persp > 0.5 { (w.max(h) / 2.0) / (persp.to_radians() / 2.0).tan().max(0.05) } else { 0.0 };
    let project = |(x, y): (f64, f64)| -> (f64, f64) {
        let (x, y) = (x - cx, y - cy);
        let (x1, z1) = (x * a.cos(), x * a.sin());
        let (y2, z2) = (y * b.cos() - z1 * b.sin(), y * b.sin() + z1 * b.cos());
        let k = if f > 0.0 { f / (f + z2).max(f * 0.1) } else { 1.0 };
        (cx + x1 * k, cy + y2 * k)
    };
    let src = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
    let dst = src.map(project);
    let (x0, y0, x1, y1) = dst.iter().fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |acc, p| {
        (acc.0.min(p.0), acc.1.min(p.1), acc.2.max(p.0), acc.3.max(p.1))
    });
    let (nw, nh) = (x1 - x0, y1 - y0);
    let max = f64::from(super::super::MAX_RASTER_SIDE * RASTER_SCALE * 1.5);
    if !(nw.is_finite() && nh.is_finite()) || nw < 2.0 || nh < 2.0 {
        return None;
    }
    let (ow, oh) = (nw.ceil().min(max) as usize, nh.ceil().min(max) as usize);
    // Output pixel → source pixel.
    let shifted = dst.map(|(x, y)| (x - x0, y - y0));
    let hm = homography(shifted, src)?;
    let mut out = Img::new(ow, oh);
    for oy in 0..oh {
        for ox in 0..ow {
            let (px, py) = (ox as f64 + 0.5, oy as f64 + 0.5);
            let den = hm[6] * px + hm[7] * py + hm[8];
            if den.abs() < 1e-9 {
                continue;
            }
            let sx = (hm[0] * px + hm[1] * py + hm[2]) / den;
            let sy = (hm[3] * px + hm[4] * py + hm[5]) / den;
            if sx < 0.0 || sy < 0.0 || sx >= w || sy >= h {
                continue;
            }
            let (ix, iy) = (sx as usize, sy as usize);
            if let (Some(p), Some(dst)) = (img.px.get(iy * img.w + ix).copied(), out.px.get_mut(oy * ow + ox))
            {
                *dst = p;
            }
        }
    }
    Some((out, x0 as f32, y0 as f32))
}

/// Text Effects › Transform warps: (id, label).
pub const WARPS: [(&str, &str); 19] = [
    ("", "No Transform"),
    ("archUp", "Arch Up"),
    ("archDown", "Arch Down"),
    ("wave1", "Wave"),
    ("wave2", "Double Wave"),
    ("chevronUp", "Chevron Up"),
    ("chevronDown", "Chevron Down"),
    ("inflate", "Inflate"),
    ("deflate", "Deflate"),
    ("inflateTop", "Inflate Top"),
    ("deflateBottom", "Deflate Bottom"),
    ("triangleUp", "Triangle Up"),
    ("triangleDown", "Triangle Down"),
    ("slantUp", "Slant Up"),
    ("slantDown", "Slant Down"),
    ("fadeRight", "Fade Right"),
    ("fadeLeft", "Fade Left"),
    ("canUp", "Can Up"),
    ("canDown", "Can Down"),
];

/// Top and bottom of the warped text band at `u` (0..=1 across), as fractions of the text's height.
fn envelope(kind: &str, u: f32) -> Option<(f32, f32)> {
    use std::f32::consts::PI;
    let s = (PI * u).sin();
    let tri = 1.0 - (2.0 * u - 1.0).abs();
    let band = |t: f32| (t, t + 0.7);
    Some(match kind {
        "archUp" => band(0.3 * (1.0 - s)),
        "archDown" => band(0.3 * s),
        "wave1" => band(0.15 + 0.15 * (2.0 * PI * u).sin()),
        "wave2" => band(0.15 + 0.15 * (4.0 * PI * u).sin()),
        "chevronUp" => band(0.3 * (1.0 - tri)),
        "chevronDown" => band(0.3 * tri),
        "inflate" => (0.25 * (1.0 - s), 1.0 - 0.25 * (1.0 - s)),
        "deflate" => (0.25 * s, 1.0 - 0.25 * s),
        "inflateTop" => (0.35 * (1.0 - s), 1.0),
        "deflateBottom" => (0.0, 1.0 - 0.35 * s),
        "triangleUp" => (0.6 * (1.0 - tri), 1.0),
        "triangleDown" => (0.0, 1.0 - 0.6 * (1.0 - tri)),
        "slantUp" => band(0.3 * (1.0 - u)),
        "slantDown" => band(0.3 * u),
        "fadeRight" => (0.3 * u, 1.0 - 0.3 * u),
        "fadeLeft" => (0.3 * (1.0 - u), 1.0 - 0.3 * (1.0 - u)),
        "canUp" => (0.25 * (1.0 - s), 1.0 - 0.25 * s),
        "canDown" => (0.25 * s, 1.0 - 0.25 * (1.0 - s)),
        _ => return None,
    })
}

/// `mask` with its ink warped by `kind` within the ink's own bounds (Text Effects › Transform).
pub(in crate::cmd::insert) fn warp(mask: &Mask, kind: &str) -> Mask {
    if envelope(kind, 0.5).is_none() {
        return mask.clone();
    }
    let Some((bx0, by0, bx1, by1)) = mask.bounds() else { return mask.clone() };
    let (x0, y0) = (bx0 as f32, by0 as f32);
    let (w, h) = ((bx1 + 1).saturating_sub(bx0) as f32, (by1 + 1).saturating_sub(by0) as f32);
    if w < 2.0 || h < 2.0 {
        return mask.clone();
    }
    mask.map(|x, y| {
        let (xf, yf) = (x as f32, y as f32);
        if xf < x0 || xf >= x0 + w || yf < y0 || yf >= y0 + h {
            return 0;
        }
        let u = (xf - x0) / w;
        let Some((t, b)) = envelope(kind, u) else { return 0 };
        let v = ((yf - y0) / h - t) / (b - t).max(0.05);
        if !(0.0..1.0).contains(&v) {
            return 0;
        }
        mask.get(x, (y0 + v * h) as i64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::rich::{GradientStop, Shadow};

    fn square(n: usize, inset: usize) -> Mask {
        let mut m = Mask::new(n, n);
        for y in inset..n - inset {
            for x in inset..n - inset {
                m.a[y * n + x] = 255;
            }
        }
        m
    }

    #[test]
    fn gradients_light_dark_and_by_stops() {
        let red = Color::rgb(200, 0, 0);
        let b = (0.0, 0.0, 100.0, 100.0);
        let top = preset_gradient("down", red, b, 50.0, 0.0);
        let bottom = preset_gradient("down", red, b, 50.0, 100.0);
        assert!(top.g > bottom.g, "light at the top");
        let dark = preset_gradient("dark:down", red, b, 50.0, 100.0);
        assert!(dark.r < red.r, "dark variations go darker");
        assert!(valid_gradient("dark:upLeft") && !valid_gradient("dark:spiral"));
        let g = Gradient {
            radial: false,
            angle: 0.0,
            stops: vec![
                GradientStop { pos: 0.0, color: Color::rgb(0, 0, 0) },
                GradientStop { pos: 1.0, color: Color::rgb(255, 255, 255) },
            ],
        };
        assert!(stops_gradient(&g, b, 0.0, 50.0).r < 20);
        assert!(stops_gradient(&g, b, 100.0, 50.0).r > 235);
        assert_eq!(stop_color(&[], 0.5), Color::TRANSPARENT);
    }

    #[test]
    fn dashes_cut_a_line_into_pieces() {
        let line = vec![vec![(0.0, 0.0), (100.0, 0.0)]];
        let pieces = dashed(&line, false, "dash", 2.0);
        assert!(pieces.len() >= 6, "{}", pieces.len());
        assert_eq!(dashed(&line, false, "", 2.0), line);
        let closed = dashed(&[vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]], true, "squareDot", 1.0);
        assert!(closed.len() > 3);
    }

    #[test]
    fn shadows_glows_and_bevels_paint_around_and_on_the_shape() {
        let m = square(40, 10);
        let mut img = Img::new(40, 40);
        let fx = ShapeEffects {
            shadow: Some(Shadow { distance: 4.0, ..Shadow::default() }),
            glow: Some(Default::default()),
            ..ShapeEffects::default()
        };
        paint_under(&mut img, &m, Color::rgb(0, 0, 200), &fx);
        assert!(img.px[35 * 40 + 35][3] > 0.0, "the shadow falls down and right");
        let mut over = Img::new(40, 40);
        let bevel = ShapeEffects { bevel: "angle".into(), ..ShapeEffects::default() };
        paint_over(&mut over, &m, Color::rgb(0, 0, 200), &bevel);
        assert!(over.px[11 * 40 + 11][3] > 0.0, "the bevel lights the top-left edge");
        assert_eq!(over.px[2 * 40 + 2][3], 0.0, "nothing outside the shape");
    }

    #[test]
    fn three_d_rotation_narrows_the_picture() {
        let mut img = Img::new(100, 50);
        img.px.fill([1.0, 0.0, 0.0, 1.0]);
        let fx = ShapeEffects { rot_x: 60.0, perspective: 45.0, ..ShapeEffects::default() };
        let (out, _, _) = rotate_3d(&img, &fx, 50.0, 25.0).unwrap();
        assert!(out.w < 80, "{}", out.w);
        assert!(rotate_3d(&img, &ShapeEffects::default(), 50.0, 25.0).is_none());
        // Edge-on: nothing to see, no crash.
        let edge = ShapeEffects { rot_x: 90.0, ..ShapeEffects::default() };
        let _ = rotate_3d(&img, &edge, 50.0, 25.0);
    }

    #[test]
    fn warps_move_the_ink() {
        let m = square(60, 10);
        let w = warp(&m, "archUp");
        // The middle column is raised: its top row has ink higher up than at the sides.
        let top_ink = |m: &Mask, x: i64| (0..60).find(|y| m.get(x, *y) > 0);
        assert!(top_ink(&w, 30) < top_ink(&w, 12));
        assert_eq!(warp(&m, "nonsense").a, m.a);
        for (id, _) in WARPS {
            let _ = warp(&m, id);
        }
    }

    #[test]
    fn reflections_mirror_below() {
        let mut img = Img::new(10, 40);
        for y in 0..20 {
            for x in 0..10 {
                img.px[y * 10 + x] = [0.0, 0.0, 1.0, 1.0];
            }
        }
        reflect(&mut img, 0, 20, &Reflection::default());
        assert!(img.px[21 * 10 + 5][3] > 0.0);
        assert_eq!(img.px[39 * 10 + 5][3], 0.0, "half size");
    }
}
