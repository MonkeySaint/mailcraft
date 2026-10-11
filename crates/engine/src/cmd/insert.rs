//! `insert.*`: tables, pictures and generated graphics, links, bookmarks, symbols, building blocks, cards and
//! calendars in the compose body. Graphics the body model cannot hold natively are inserted as inline images
//! (`cid:` parts when sent): shapes, WordArt and SmartArt are drawn to PNG with `mailcraft-render` in the
//! theme's colours and effects; icons, charts and 3D models are SVG. Commands that need an OS facility (screen
//! capture) or a picker without params open a dialog instead.
//!
//! Quick Parts / AutoText and the other building-block galleries are kept in `building-blocks.json` in the
//! profile directory (next to the data file), so they outlive the compose windows.

use std::fmt::Write as _;

use serde_json::{Value, json};

use super::compose::{
    add_bookmark, compose_id, compose_mut, file_param, has_body_selection, has_compose, mime_for, parse_when,
    pos_json, rich_compose, safe_name, spec, vcard,
};
use crate::cmd::util::{dialog, opt_bool, opt_str, req_str};
use crate::compose::{
    BuildingBlock, ComposeAttachment, ComposeState, DocPos, InlineImage, empty_table, table_cell,
};
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_geom::Color;
use mailcraft_model::dates::ListTime;
use mailcraft_model::rich::{Align, TableCell};
use mailcraft_model::{Block, BusyStatus, DateTime, Doc, FolderKind, Inline, ItemId, ParaStyle, TextStyle};

/// Largest picture / object file accepted.
pub const MAX_PICTURE_BYTES: u64 = 50 * 1024 * 1024;
/// Largest 3D model file accepted.
pub const MAX_MODEL_BYTES: u64 = 64 * 1024 * 1024;
/// Widest inserted picture (px); larger ones are scaled down.
const MAX_PICTURE_WIDTH: f32 = 600.0;
/// Table size limits.
pub const MAX_TABLE_COLS: usize = 63;
pub const MAX_TABLE_ROWS: usize = 1000;
const MAX_TABLE_CELLS: usize = 20_000;
/// Galleries building blocks can be saved to.
pub const GALLERIES: [&str; 5] = ["quickParts", "autoText", "textBoxes", "quickTables", "equations"];
/// Gallery that keeps named list styles (Define New List Style), stored with the building blocks.
pub const LIST_STYLES_GALLERY: &str = "listStyles";
/// Most named list styles kept.
const MAX_LIST_STYLES: usize = 100;
const PALETTE: [&str; 6] = ["#156082", "#e97132", "#196b24", "#0f9ed5", "#a02b93", "#4ea72e"];

fn edit(s: &mut Session, p: &Value, f: impl FnOnce(&mut ComposeState)) -> Result<(), CmdError> {
    let c = compose_mut(s, p)?;
    c.checkpoint();
    f(c);
    Ok(())
}

fn caret(c: &ComposeState) -> Value {
    pos_json(&c.selection.focus)
}

// ---------------------------------------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------------------------------------

/// MIME type of image bytes by signature (falls back to the file name).
pub fn sniff_image(data: &[u8], name: &str) -> Option<&'static str> {
    let starts = |sig: &[u8]| data.get(..sig.len()) == Some(sig);
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if starts(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Some("image/gif");
    }
    if starts(b"BM") {
        return Some("image/bmp");
    }
    if starts(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        return Some("image/webp");
    }
    let head = String::from_utf8_lossy(data.get(..data.len().min(512)).unwrap_or_default()).to_lowercase();
    if head.contains("<svg") {
        return Some("image/svg+xml");
    }
    let m = mime_for(name);
    m.starts_with("image/").then_some(m)
}

fn be16(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from(*d.get(i)?) << 8 | u32::from(*d.get(i.checked_add(1)?)?))
}

fn be32(d: &[u8], i: usize) -> Option<u32> {
    Some(be16(d, i)? << 16 | be16(d, i.checked_add(2)?)?)
}

fn le16(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from(*d.get(i)?) | u32::from(*d.get(i.checked_add(1)?)?) << 8)
}

fn le32(d: &[u8], i: usize) -> Option<u32> {
    Some(le16(d, i)? | le16(d, i.checked_add(2)?)? << 16)
}

/// Pixel size of PNG / GIF / JPEG / BMP data.
pub fn image_size(d: &[u8]) -> Option<(u32, u32)> {
    match sniff_image(d, "")? {
        "image/png" => Some((be32(d, 16)?, be32(d, 20)?)),
        "image/gif" => Some((le16(d, 6)?, le16(d, 8)?)),
        "image/bmp" => {
            let w = le32(d, 18)?;
            let h = i32::from_le_bytes(le32(d, 22)?.to_le_bytes()).unsigned_abs();
            Some((w, h))
        }
        "image/jpeg" => {
            let mut i = 2usize;
            for _ in 0..10_000 {
                if *d.get(i)? != 0xff {
                    return None;
                }
                let marker = *d.get(i.checked_add(1)?)?;
                if marker == 0xff {
                    i = i.checked_add(1)?;
                    continue;
                }
                let len = usize::try_from(be16(d, i.checked_add(2)?)?).ok()?;
                if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                    let h = be16(d, i.checked_add(5)?)?;
                    let w = be16(d, i.checked_add(7)?)?;
                    return Some((w, h));
                }
                i = i.checked_add(2)?.checked_add(len)?;
            }
            None
        }
        _ => None,
    }
}

/// Display size for a picture: natural size scaled down to the body width.
#[allow(clippy::cast_precision_loss)]
fn display_size(size: Option<(u32, u32)>) -> (Option<f32>, Option<f32>) {
    match size {
        Some((w, h)) if w > 0 && h > 0 => {
            let (w, h) = (w as f32, h as f32);
            if w > MAX_PICTURE_WIDTH {
                (Some(MAX_PICTURE_WIDTH), Some((h * MAX_PICTURE_WIDTH / w).round()))
            } else {
                (Some(w), Some(h))
            }
        }
        _ => (None, None),
    }
}

/// Embeds `data` as an inline image at the caret.
fn put_image(
    c: &mut ComposeState,
    filename: &str,
    mime: &str,
    data: Vec<u8>,
    alt: &str,
    size: (Option<f32>, Option<f32>),
) -> String {
    let cid = c.next_content_id();
    c.checkpoint();
    c.insert_image(
        InlineImage {
            content_id: cid.clone(),
            filename: filename.to_owned(),
            mime_type: mime.to_owned(),
            data,
        },
        alt,
        size.0,
        size.1,
    );
    cid
}

/// Embeds picture bytes from the clipboard (Paste / Paste Special › Picture) at the caret, sized like
/// Insert Picture. Only real pictures are taken (by signature, not by a claimed type; SVG is refused because
/// a pasted SVG can carry script and remote references). Returns the content id.
pub(crate) fn paste_picture(c: &mut ComposeState, data: Vec<u8>) -> Result<String, CmdError> {
    if data.is_empty() || data.len() as u64 > MAX_PICTURE_BYTES {
        return Err(CmdError::Params("the picture is empty or too large".into()));
    }
    let mime = match sniff_image(&data, "") {
        Some(m) if m != "image/svg+xml" => m,
        _ => {
            return Err(CmdError::Params(
                "the clipboard data is not a PNG, JPEG, GIF, BMP or WebP picture".into(),
            ));
        }
    };
    let ext = match mime {
        "image/jpeg" => "jpg",
        other => other.strip_prefix("image/").unwrap_or("img"),
    };
    let filename = format!("image{:03}.{ext}", c.inline_images.len().saturating_add(1));
    let size = display_size(image_size(&data));
    Ok(put_image(c, &filename, mime, data, "Picture", size))
}

/// Embeds generated SVG at the caret.
fn put_svg(c: &mut ComposeState, name: &str, svg: String, alt: &str, w: f32, h: f32) -> String {
    put_image(c, &format!("{name}.svg"), "image/svg+xml", svg.into_bytes(), alt, (Some(w), Some(h)))
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn svg_open(w: f32, h: f32) -> String {
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#)
}

fn picture(s: &mut Session, p: &Value) -> CmdResult {
    let Some((name, data)) = file_param(p, MAX_PICTURE_BYTES)? else {
        return dialog(s, "insertPicture", json!({"compose": compose_id(s, p)?}));
    };
    let Some(mime) = sniff_image(&data, &name) else {
        return Err(CmdError::Failed(format!("{name} is not a picture this editor can show")));
    };
    let size = display_size(image_size(&data));
    let alt = opt_str(p, "alt").unwrap_or("").to_owned();
    let c = compose_mut(s, p)?;
    let cid = put_image(c, &name, mime, data, &alt, size);
    Ok(json!({"inserted": name, "cid": cid, "width": size.0, "height": size.1}))
}

// ---------------------------------------------------------------------------------------------------------
// Shapes, icons, charts, SmartArt, 3D models
// ---------------------------------------------------------------------------------------------------------

const SHAPES: &str = "rectangle, roundedRectangle, oval, circle, triangle, rightTriangle, diamond, pentagon, hexagon, octagon, star, star6, line, arrow, leftArrow, upArrow, downArrow, parallelogram, trapezoid, plus, chevron, heart, callout, canvas";

fn shape(s: &mut Session, p: &Value) -> CmdResult {
    let kind = req_str(p, "kind")?.to_owned();
    let cid = compose_id(s, p)?;
    let theme = s
        .composes
        .get(&cid)
        .map(ComposeState::theme_resolved)
        .ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let color = match opt_str(p, "color") {
        Some(c) => mailcraft_html::parse_css_color(c)
            .ok_or_else(|| CmdError::Params(format!("`{c}` is not a colour")))?,
        None => theme.color(crate::compose::ACCENT1),
    };
    let Some((png, w, h)) = shape_png(&kind, color, theme.effects) else {
        return Err(CmdError::Params(format!("unknown shape `{kind}` (known: {SHAPES})")));
    };
    let c = compose_mut(s, p)?;
    let cid = put_png(c, &kind, png, &kind, w, h)?;
    Ok(json!({"shape": kind, "cid": cid, "format": "png", "effects": theme.effects}))
}

// ---------------------------------------------------------------------------------------------------------
// Raster graphics: shapes, WordArt and SmartArt are drawn with mailcraft-render into coverage masks,
// composited here with the theme's colours and effects, and inserted as PNG (every mail client shows PNG;
// many do not show SVG).
// ---------------------------------------------------------------------------------------------------------

/// Device pixels per logical px of generated pictures.
const RASTER_SCALE: f32 = 2.0;
/// Largest generated picture side (logical px).
const MAX_RASTER_SIDE: f32 = 1000.0;
/// Room around a drawing for shadows and glows (logical px).
const EFFECT_MARGIN: f32 = 12.0;

/// Coverage (0..=255) per device pixel.
#[derive(Clone)]
struct Mask {
    w: usize,
    h: usize,
    a: Vec<u8>,
}

impl Mask {
    fn new(w: usize, h: usize) -> Mask {
        Mask { w, h, a: vec![0; w.saturating_mul(h)] }
    }

    fn get(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 {
            return 0;
        }
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else { return 0 };
        if x >= self.w || y >= self.h {
            return 0;
        }
        self.a.get(y.saturating_mul(self.w).saturating_add(x)).copied().unwrap_or(0)
    }

    fn map(&self, f: impl Fn(i64, i64) -> u8) -> Mask {
        let mut m = Mask::new(self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                if let Some(v) = m.a.get_mut(y.saturating_mul(self.w).saturating_add(x)) {
                    *v = f(i64::try_from(x).unwrap_or(0), i64::try_from(y).unwrap_or(0));
                }
            }
        }
        m
    }

    /// The display list's ink as coverage (rendered black on transparent).
    fn from_list(
        list: &mailcraft_layout::DisplayList,
        w: usize,
        h: usize,
        fonts: &mailcraft_fonts::FontDb,
    ) -> Mask {
        let (Ok(wu), Ok(hu)) = (u32::try_from(w), u32::try_from(h)) else { return Mask::new(0, 0) };
        let canvas =
            mailcraft_render::render(list, fonts, wu, hu, RASTER_SCALE, Color::rgba(0, 0, 0, 0), &|_| None);
        let mut m = Mask::new(w, h);
        if canvas.pixels.len() == m.a.len().saturating_mul(4) {
            for (dst, px) in m.a.iter_mut().zip(canvas.pixels.as_chunks::<4>().0) {
                let [_, _, _, alpha] = *px;
                *dst = alpha;
            }
        }
        m
    }

    fn shifted(&self, dx: i64, dy: i64) -> Mask {
        self.map(|x, y| self.get(x - dx, y - dy))
    }

    /// Max filter of radius `r` (grows the shape).
    fn dilated(&self, r: i64) -> Mask {
        let horiz = self.map(|x, y| (-r..=r).map(|d| self.get(x + d, y)).max().unwrap_or(0));
        horiz.map(|x, y| (-r..=r).map(|d| horiz.get(x, y + d)).max().unwrap_or(0))
    }

    /// Min filter of radius `r` (shrinks the shape).
    fn eroded(&self, r: i64) -> Mask {
        let horiz = self.map(|x, y| (-r..=r).map(|d| self.get(x + d, y)).min().unwrap_or(0));
        horiz.map(|x, y| (-r..=r).map(|d| horiz.get(x, y + d)).min().unwrap_or(0))
    }

    /// Box blur of radius `r` (two passes).
    fn blurred(&self, r: i64) -> Mask {
        if r <= 0 {
            return self.clone();
        }
        let n = u32::try_from(2 * r + 1).unwrap_or(1);
        let pass = |m: &Mask, horizontal: bool| {
            m.map(|x, y| {
                let sum: u32 = (-r..=r)
                    .map(|d| u32::from(if horizontal { m.get(x + d, y) } else { m.get(x, y + d) }))
                    .sum();
                u8::try_from(sum / n).unwrap_or(255)
            })
        };
        let once = pass(&pass(self, true), false);
        pass(&pass(&once, true), false)
    }

    fn minus(&self, o: &Mask) -> Mask {
        self.map(|x, y| self.get(x, y).saturating_sub(o.get(x, y)))
    }

    /// Mirror image below row `axis` (reflection), fading out over `len` rows.
    fn reflected(&self, axis: i64, len: i64) -> Mask {
        self.map(|x, y| {
            let d = y - axis;
            if d < 0 || d > len {
                return 0;
            }
            let src = self.get(x, axis - d);
            let fade = 1.0 - d as f32 / len.max(1) as f32;
            (f32::from(src) * fade * 0.45) as u8
        })
    }

    /// Bounds of the covered pixels (x0, y0, x1, y1), if any.
    fn bounds(&self) -> Option<(usize, usize, usize, usize)> {
        let mut b: Option<(usize, usize, usize, usize)> = None;
        for y in 0..self.h {
            for x in 0..self.w {
                if self.a.get(y * self.w + x).copied().unwrap_or(0) > 8 {
                    b = Some(match b {
                        None => (x, y, x, y),
                        Some((a, c, d, e)) => (a.min(x), c.min(y), d.max(x), e.max(y)),
                    });
                }
            }
        }
        b
    }
}

/// An RGBA image being composited (premultiplied, 0..=1).
struct Img {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl Img {
    fn new(w: usize, h: usize) -> Img {
        Img { w, h, px: vec![[0.0; 4]; w.saturating_mul(h)] }
    }

    /// Paints `color(x, y)` through `mask` × `opacity` over the image.
    fn paint(&mut self, mask: &Mask, opacity: f32, color: &dyn Fn(usize, usize) -> Color) {
        self.paint_at(mask, (0, 0), opacity, color);
    }

    /// [`Img::paint`] with the mask's top-left corner at device pixel `at` (mask coordinates go to
    /// `color`).
    fn paint_at(
        &mut self,
        mask: &Mask,
        at: (usize, usize),
        opacity: f32,
        color: &dyn Fn(usize, usize) -> Color,
    ) {
        for y in 0..mask.h {
            let iy = y.saturating_add(at.1);
            if iy >= self.h {
                break;
            }
            for x in 0..mask.w {
                let ix = x.saturating_add(at.0);
                if ix >= self.w {
                    break;
                }
                let cov = f32::from(mask.a.get(y * mask.w + x).copied().unwrap_or(0)) / 255.0 * opacity;
                if cov <= 0.0 {
                    continue;
                }
                let c = color(x, y);
                let a = cov * f32::from(c.a) / 255.0;
                if let Some(p) = self.px.get_mut(iy * self.w + ix) {
                    let src = [
                        f32::from(c.r) / 255.0 * a,
                        f32::from(c.g) / 255.0 * a,
                        f32::from(c.b) / 255.0 * a,
                        a,
                    ];
                    for (pk, sk) in p.iter_mut().zip(src) {
                        *pk = sk + *pk * (1.0 - a);
                    }
                }
            }
        }
    }

    fn to_png(&self) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(self.px.len() * 4);
        for p in &self.px {
            for v in p {
                pixels.push((v.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
        let canvas = mailcraft_render::Canvas {
            width: u32::try_from(self.w).unwrap_or(0),
            height: u32::try_from(self.h).unwrap_or(0),
            pixels,
        };
        canvas.to_png()
    }
}

/// Device size of a logical length.
fn dev(v: f32) -> usize {
    (v.clamp(1.0, MAX_RASTER_SIDE) * RASTER_SCALE).ceil() as usize
}

/// Display list filling the polygon `pts` (logical px): one rectangle span per device row (even-odd rule),
/// antialiased by the renderer at the span ends.
fn polygon_list(pts: &[(f32, f32)], list: &mut mailcraft_layout::DisplayList) {
    use mailcraft_geom::Rect;
    use mailcraft_layout::DisplayItem;
    if pts.len() < 3 {
        return;
    }
    let (min_y, max_y) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
    if !min_y.is_finite() || !max_y.is_finite() {
        return;
    }
    let step = 1.0 / RASTER_SCALE;
    let mut y = (min_y * RASTER_SCALE).floor() / RASTER_SCALE;
    let mut rows = 0usize;
    while y < max_y && rows < 40_000 {
        let yc = y + step / 2.0;
        let mut xs: Vec<f32> = Vec::new();
        for i in 0..pts.len() {
            let (a, b) = (
                pts.get(i).copied().unwrap_or_default(),
                pts.get((i + 1) % pts.len()).copied().unwrap_or_default(),
            );
            if (a.1 <= yc && b.1 > yc) || (b.1 <= yc && a.1 > yc) {
                let t = (yc - a.1) / (b.1 - a.1);
                xs.push(a.0 + t * (b.0 - a.0));
            }
        }
        xs.sort_by(f32::total_cmp);
        for pair in xs.chunks(2) {
            if let [x0, x1] = pair
                && x1 > x0
            {
                list.items.push(DisplayItem::FillRect {
                    rect: Rect::new(*x0, y, x1 - x0, step),
                    color: Color::BLACK,
                });
            }
        }
        y += step;
        rows += 1;
    }
}

fn ellipse_pts(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<(f32, f32)> {
    (0..96)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / 96.0;
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

fn rounded_rect_pts(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<(f32, f32)> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut out = Vec::new();
    let corners = [
        (x + w - r, y + r, -90.0f32),
        (x + w - r, y + h - r, 0.0),
        (x + r, y + h - r, 90.0),
        (x + r, y + r, 180.0),
    ];
    for (cx, cy, start) in corners {
        for k in 0..=8 {
            let a = (start + 90.0 * k as f32 / 8.0).to_radians();
            out.push((cx + r * a.cos(), cy + r * a.sin()));
        }
    }
    out
}

/// Points of a cubic Bézier from `p0` (exclusive) to `p3`.
fn bezier(out: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) {
    for i in 1..=16 {
        let t = i as f32 / 16.0;
        let u = 1.0 - t;
        let x = u * u * u * p0.0 + 3.0 * u * u * t * p1.0 + 3.0 * u * t * t * p2.0 + t * t * t * p3.0;
        let y = u * u * u * p0.1 + 3.0 * u * u * t * p1.1 + 3.0 * u * t * t * p2.1 + t * t * t * p3.1;
        out.push((x, y));
    }
}

fn regular_pts(cx: f32, cy: f32, r: f32, n: usize, inner: Option<f32>) -> Vec<(f32, f32)> {
    let k = if inner.is_some() { n * 2 } else { n };
    (0..k)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / k as f32 - std::f32::consts::FRAC_PI_2;
            let rr = match inner {
                Some(ri) if i % 2 == 1 => ri,
                _ => r,
            };
            (cx + rr * a.cos(), cy + rr * a.sin())
        })
        .collect()
}

/// Outline of shape `kind` in a 160×100 box at (`ox`, `oy`).
fn shape_points(kind: &str, ox: f32, oy: f32) -> Option<Vec<(f32, f32)>> {
    let t = |pts: &[(f32, f32)]| pts.iter().map(|(x, y)| (x + ox, y + oy)).collect::<Vec<_>>();
    Some(match kind {
        "rectangle" | "textBox" => t(&[(4.0, 4.0), (156.0, 4.0), (156.0, 96.0), (4.0, 96.0)]),
        "roundedRectangle" => t(&rounded_rect_pts(4.0, 4.0, 152.0, 92.0, 16.0)),
        "oval" | "ellipse" => t(&ellipse_pts(80.0, 50.0, 76.0, 46.0)),
        "circle" => t(&ellipse_pts(80.0, 50.0, 46.0, 46.0)),
        "triangle" => t(&[(80.0, 4.0), (156.0, 96.0), (4.0, 96.0)]),
        "rightTriangle" => t(&[(4.0, 4.0), (156.0, 96.0), (4.0, 96.0)]),
        "diamond" => t(&[(80.0, 4.0), (156.0, 50.0), (80.0, 96.0), (4.0, 50.0)]),
        "pentagon" => t(&regular_pts(80.0, 52.0, 46.0, 5, None)),
        "hexagon" => t(&regular_pts(80.0, 50.0, 46.0, 6, None)),
        "octagon" => t(&regular_pts(80.0, 50.0, 46.0, 8, None)),
        "star" => t(&regular_pts(80.0, 52.0, 48.0, 5, Some(20.0))),
        "star6" => t(&regular_pts(80.0, 50.0, 46.0, 6, Some(24.0))),
        "arrow" | "rightArrow" => t(&[
            (4.0, 32.0),
            (110.0, 32.0),
            (110.0, 8.0),
            (156.0, 50.0),
            (110.0, 92.0),
            (110.0, 68.0),
            (4.0, 68.0),
        ]),
        "leftArrow" => t(&[
            (156.0, 32.0),
            (50.0, 32.0),
            (50.0, 8.0),
            (4.0, 50.0),
            (50.0, 92.0),
            (50.0, 68.0),
            (156.0, 68.0),
        ]),
        "upArrow" => t(&[
            (62.0, 96.0),
            (62.0, 40.0),
            (40.0, 40.0),
            (80.0, 4.0),
            (120.0, 40.0),
            (98.0, 40.0),
            (98.0, 96.0),
        ]),
        "downArrow" => t(&[
            (62.0, 4.0),
            (62.0, 60.0),
            (40.0, 60.0),
            (80.0, 96.0),
            (120.0, 60.0),
            (98.0, 60.0),
            (98.0, 4.0),
        ]),
        "parallelogram" => t(&[(36.0, 4.0), (156.0, 4.0), (124.0, 96.0), (4.0, 96.0)]),
        "trapezoid" => t(&[(36.0, 4.0), (124.0, 4.0), (156.0, 96.0), (4.0, 96.0)]),
        "plus" | "cross" => t(&[
            (60.0, 4.0),
            (100.0, 4.0),
            (100.0, 32.0),
            (156.0, 32.0),
            (156.0, 68.0),
            (100.0, 68.0),
            (100.0, 96.0),
            (60.0, 96.0),
            (60.0, 68.0),
            (4.0, 68.0),
            (4.0, 32.0),
            (60.0, 32.0),
        ]),
        "chevron" => t(&[(4.0, 4.0), (116.0, 4.0), (156.0, 50.0), (116.0, 96.0), (4.0, 96.0), (44.0, 50.0)]),
        "heart" => {
            let mut v = vec![(80.0, 92.0)];
            bezier(&mut v, (80.0, 92.0), (20.0, 60.0), (4.0, 36.0), (24.0, 16.0));
            bezier(&mut v, (24.0, 16.0), (40.0, 2.0), (64.0, 6.0), (80.0, 26.0));
            bezier(&mut v, (80.0, 26.0), (96.0, 6.0), (120.0, 2.0), (136.0, 16.0));
            bezier(&mut v, (136.0, 16.0), (156.0, 36.0), (140.0, 60.0), (80.0, 92.0));
            t(&v)
        }
        "callout" => t(&[
            (8.0, 8.0),
            (152.0, 8.0),
            (152.0, 70.0),
            (60.0, 70.0),
            (36.0, 94.0),
            (40.0, 70.0),
            (8.0, 70.0),
        ]),
        "canvas" => t(&[(1.0, 1.0), (159.0, 1.0), (159.0, 99.0), (1.0, 99.0)]),
        _ => return None,
    })
}

/// How the theme's effect set draws a shape.
#[derive(Clone, Copy, Debug, Default)]
struct Effect {
    /// Lighter top → base colour bottom (0 = flat).
    gradient: f32,
    /// Soft shadow: offset (logical px), blur radius, opacity.
    shadow: Option<(f32, f32, f32)>,
    /// Glow: radius, opacity.
    glow: Option<(f32, f32)>,
    /// Darker outline width (logical px).
    outline: f32,
    /// Shine on the upper half.
    gloss: bool,
    reflection: bool,
    /// Darker inner edge.
    inset: bool,
    /// 0 none, 1 grain, 2 ribs.
    texture: u8,
    /// Fill opacity.
    opacity: f32,
}

fn effect_style(name: &str) -> Effect {
    let base = Effect { opacity: 1.0, outline: 1.0, ..Effect::default() };
    match name {
        "Office 2007 - 2010" => Effect { gradient: 0.25, shadow: Some((2.0, 2.0, 0.35)), ..base },
        "Subtle Solids" => Effect { outline: 0.0, ..base },
        "Banded Edge" => Effect { outline: 4.0, ..base },
        "Smokey Glass" => Effect { opacity: 0.7, gradient: 0.2, shadow: Some((2.0, 3.0, 0.3)), ..base },
        "Glow Edge" => Effect { glow: Some((5.0, 0.6)), ..base },
        "Grunge Texture" => Effect { texture: 1, shadow: Some((2.0, 2.0, 0.3)), ..base },
        "Frosted Glass" => Effect { opacity: 0.8, gradient: 0.45, gloss: true, ..base },
        "Top Shadow" => Effect { shadow: Some((3.0, 3.0, 0.45)), ..base },
        "Inset" => Effect { inset: true, ..base },
        "Milk Glass" => Effect { gradient: 0.55, gloss: true, ..base },
        "Riblet" => Effect { texture: 2, ..base },
        "Reflection" => Effect { reflection: true, gradient: 0.15, ..base },
        "Extreme Shadow" => Effect { shadow: Some((6.0, 4.0, 0.6)), ..base },
        "Glossy" => Effect { gloss: true, gradient: 0.3, shadow: Some((2.0, 2.0, 0.3)), ..base },
        _ => base,
    }
}

fn lighten(c: Color, t: f32) -> Color {
    c.lerp(Color::WHITE, t.clamp(0.0, 1.0))
}

fn darken(c: Color, t: f32) -> Color {
    c.lerp(Color::BLACK, t.clamp(0.0, 1.0))
}

/// Composites `mask` (the drawing's shape) filled with `color` and decorated per `fx`, onto `img` with the
/// mask's corner at device pixel `at`.
fn composite(img: &mut Img, at: (usize, usize), mask: &Mask, color: Color, fx: &Effect) {
    let s = RASTER_SCALE;
    let d = |v: f32| (v * s).round() as i64;
    if let Some((off, blur, op)) = fx.shadow {
        let sh = mask.shifted(d(off), d(off)).blurred(d(blur / 2.0).max(1));
        img.paint_at(&sh, at, op, &|_, _| Color::BLACK);
    }
    if let Some((r, op)) = fx.glow {
        let g = mask.dilated(d(r / 2.0)).blurred(d(r / 2.0).max(1));
        img.paint_at(&g, at, op, &|_, _| lighten(color, 0.4));
    }
    let (y0, y1) = mask.bounds().map_or((0, mask.h), |(_, a, _, b)| (a, b.max(a + 1)));
    let grad = fx.gradient;
    let texture = fx.texture;
    let fill = move |x: usize, y: usize| {
        let t = (y.saturating_sub(y0)) as f32 / (y1.saturating_sub(y0)).max(1) as f32;
        let mut c = lighten(color, grad * (1.0 - t));
        match texture {
            1 => {
                let n = ((x.wrapping_mul(7919) ^ y.wrapping_mul(104_729)) % 23) as f32 / 23.0;
                c = darken(c, n * 0.18);
            }
            2 if (y / 6).is_multiple_of(2) => c = darken(c, 0.12),
            _ => {}
        }
        c
    };
    img.paint_at(mask, at, fx.opacity, &fill);
    if fx.inset {
        let edge = mask.minus(&mask.shifted(d(2.0), d(2.0)));
        img.paint_at(&edge, at, 0.5, &|_, _| darken(color, 0.5));
    }
    if fx.outline > 0.0 {
        let ring = mask.minus(&mask.eroded(d(fx.outline / 2.0).max(1)));
        img.paint_at(&ring, at, 1.0, &|_, _| darken(color, 0.35));
    }
    if fx.gloss {
        let mid = y0 + (y1.saturating_sub(y0)) / 2;
        let top = mask.map(|x, y| if usize::try_from(y).is_ok_and(|y| y < mid) { mask.get(x, y) } else { 0 });
        img.paint_at(&top, at, 0.3, &|_, _| Color::WHITE);
    }
    if fx.reflection {
        let r = mask.reflected(i64::try_from(y1).unwrap_or(0), i64::try_from((y1 - y0) / 2).unwrap_or(1));
        img.paint_at(&r, at, 1.0, &|_, _| color);
    }
}

/// A display list of `text` laid out on one line (black), and its ink width / height (logical px).
fn text_list(
    text: &str,
    style: &TextStyle,
    fonts: &mailcraft_fonts::FontDb,
) -> (mailcraft_layout::DisplayList, f32, f32, f32, f32) {
    let doc = Doc {
        blocks: vec![Block::Paragraph {
            style: ParaStyle::default(),
            inlines: vec![Inline::Text { text: text.to_owned(), style: style.clone() }],
        }],
        ..Default::default()
    };
    let lo = mailcraft_layout::LayoutOptions { width: MAX_RASTER_SIDE * 4.0, ..Default::default() };
    let layout = mailcraft_layout::layout_doc(&doc, fonts, &lo);
    let mut list = mailcraft_layout::DisplayList::default();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for r in &layout.index.runs {
        x0 = x0.min(r.rect.x);
        y0 = y0.min(r.rect.y);
        x1 = x1.max(r.rect.x + r.rect.w);
        y1 = y1.max(r.rect.y + r.rect.h);
    }
    for item in layout.display.items {
        if let mailcraft_layout::DisplayItem::Text { .. } = item {
            list.items.push(item);
        }
    }
    if x0 > x1 {
        return (list, 0.0, 0.0, 0.0, 0.0);
    }
    (list, x0, y0, x1 - x0, y1 - y0)
}

/// Moves every item of `list` by (`dx`, `dy`) and recolours text.
fn translate(list: &mut mailcraft_layout::DisplayList, dx: f32, dy: f32) {
    use mailcraft_layout::DisplayItem;
    for item in &mut list.items {
        match item {
            DisplayItem::Text { origin, .. } => *origin = origin.offset(dx, dy),
            DisplayItem::FillRect { rect, .. } | DisplayItem::StrokeRect { rect, .. } => {
                *rect = rect.translate(dx, dy)
            }
            DisplayItem::Line { from, to, .. } => {
                *from = from.offset(dx, dy);
                *to = to.offset(dx, dy);
            }
            DisplayItem::Image { rect, .. } => *rect = rect.translate(dx, dy),
        }
    }
}

/// Inserts a generated PNG of `w`×`h` logical px at the caret.
fn put_png(
    c: &mut ComposeState,
    name: &str,
    png: Vec<u8>,
    alt: &str,
    w: f32,
    h: f32,
) -> Result<String, CmdError> {
    if png.is_empty() {
        return Err(CmdError::Failed("the picture could not be drawn".into()));
    }
    Ok(put_image(c, &format!("{name}.png"), "image/png", png, alt, (Some(w), Some(h))))
}

/// PNG of shape `kind` filled with `color` and drawn with effect set `effects`; (png, width, height).
fn shape_png(kind: &str, color: Color, effects: &str) -> Option<(Vec<u8>, f32, f32)> {
    let fonts = mailcraft_fonts::FontDb::bundled();
    let m = EFFECT_MARGIN;
    let (w, h) = (160.0 + 2.0 * m, 100.0 + 2.0 * m);
    let mut list = mailcraft_layout::DisplayList::default();
    let fx = effect_style(effects);
    if kind == "line" {
        list.items.push(mailcraft_layout::DisplayItem::Line {
            from: mailcraft_geom::Point::new(m + 4.0, m + 96.0),
            to: mailcraft_geom::Point::new(m + 156.0, m + 4.0),
            color: Color::BLACK,
            width: 3.0,
        });
    } else {
        polygon_list(&shape_points(kind, m, m)?, &mut list);
    }
    let mask = Mask::from_list(&list, dev(w), dev(h), &fonts);
    let mut img = Img::new(dev(w), dev(h));
    if kind == "canvas" {
        // A drawing canvas: white with a dashed-looking grey frame.
        img.paint(&mask, 1.0, &|_, _| Color::WHITE);
        let ring = mask.minus(&mask.eroded(2));
        img.paint(&ring, 1.0, &|x, y| {
            if (x + y) / 8 % 2 == 0 { Color::rgb(0xa0, 0xa0, 0xa0) } else { Color::WHITE }
        });
    } else {
        composite(&mut img, (0, 0), &mask, color, &fx);
    }
    Some((img.to_png(), w, h))
}

/// Parsed WordArt style ("Fill: Orange, Accent color 2; Outline: White, Background color 1; Hard Shadow: …").
#[derive(Clone, Debug, PartialEq)]
struct WordArtStyle {
    fill: Color,
    gradient: Option<Color>,
    outline: Option<Color>,
    shadow: bool,
    hard_shadow: Option<Color>,
    inner_shadow: bool,
    glow: Option<Color>,
    bevel: bool,
    reflection: bool,
}

/// A colour named in a WordArt style part: a theme reference ("Accent color 2", "Text color 1",
/// "Background color 2") wins over a colour word ("Orange", "Dark Teal").
fn wordart_part_color(part: &str, theme: &crate::compose::ResolvedTheme) -> Option<Color> {
    let p = part.to_lowercase();
    let slot = |label: &str, base: usize| -> Option<usize> {
        let at = p.find(label)?;
        let n: String = p.get(at + label.len()..)?.trim().chars().take_while(char::is_ascii_digit).collect();
        let n: usize = n.parse().ok()?;
        Some(base + n.saturating_sub(1))
    };
    if let Some(i) =
        slot("accent color", crate::compose::ACCENT1).filter(|i| *i <= crate::compose::ACCENT1 + 5)
    {
        return Some(theme.color(i));
    }
    if let Some(i) = slot("text color", 0) {
        return Some(theme.color(if i == 0 { 0 } else { 2 }));
    }
    if let Some(i) = slot("background color", 1) {
        return Some(theme.color(if i == 1 { 1 } else { 3 }));
    }
    let word: String = p.split(',').next().unwrap_or("").chars().filter(|c| c.is_alphabetic()).collect();
    if word.is_empty() {
        return None;
    }
    mailcraft_html::parse_css_color(&word)
}

fn parse_wordart(style: &str, theme: &crate::compose::ResolvedTheme) -> WordArtStyle {
    let accent = theme.color(crate::compose::ACCENT1);
    let mut w = WordArtStyle {
        fill: accent,
        gradient: None,
        outline: None,
        shadow: false,
        hard_shadow: None,
        inner_shadow: false,
        glow: None,
        bevel: false,
        reflection: false,
    };
    for part in style.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (head, rest) = part.split_once(':').map_or((part, ""), |(h, r)| (h.trim(), r.trim()));
        let h = head.to_lowercase();
        let col = wordart_part_color(rest, theme);
        if h.starts_with("gradient fill") {
            let base = col.unwrap_or_else(|| {
                wordart_part_color(head.trim_start_matches("Gradient Fill").trim_start_matches(','), theme)
                    .unwrap_or(Color::rgb(0x80, 0x80, 0x80))
            });
            w.fill = base;
            w.gradient = Some(lighten(base, 0.6));
        } else if h.starts_with("fill") {
            if let Some(c) = col {
                w.fill = c;
            }
        } else if h.starts_with("outline") {
            w.outline = col.or(Some(darken(w.fill, 0.4)));
        } else if h.starts_with("hard shadow") {
            w.hard_shadow = col.or(Some(Color::rgb(0x40, 0x40, 0x40)));
        } else if h.starts_with("inner shadow") {
            w.inner_shadow = true;
        } else if h.starts_with("shadow") {
            w.shadow = true;
        } else if h.starts_with("glow") {
            w.glow = col.or(Some(accent));
        } else if h.contains("bevel") {
            w.bevel = true;
        } else if h.starts_with("reflection") {
            w.reflection = true;
        }
    }
    w
}

/// PNG of `text` as WordArt in `style`, set in `font` (the theme's heading font); (png, width, height).
fn wordart_png(
    text: &str,
    style: &str,
    theme: &crate::compose::ResolvedTheme,
) -> Option<(Vec<u8>, f32, f32)> {
    let fonts = mailcraft_fonts::FontDb::bundled();
    let mut ts = TextStyle {
        bold: Some(true),
        size_pt: Some(36.0),
        font_family: Some(theme.heading_font.clone()),
        ..Default::default()
    };
    let (mut list, mut x0, mut y0, mut tw, mut th) = text_list(text, &ts, &fonts);
    let room = MAX_RASTER_SIDE - 2.0 * EFFECT_MARGIN;
    if tw > room {
        // Long text gets a smaller size rather than being cut off.
        ts.size_pt = Some((36.0 * room / tw).max(8.0));
        (list, x0, y0, tw, th) = text_list(text, &ts, &fonts);
    }
    if tw <= 0.0 || th <= 0.0 {
        return None;
    }
    let ws = parse_wordart(style, theme);
    let m = EFFECT_MARGIN;
    let extra = if ws.reflection { th * 0.6 } else { 0.0 };
    let (w, h) = ((tw + 2.0 * m).min(MAX_RASTER_SIDE), (th + 2.0 * m + extra).min(MAX_RASTER_SIDE));
    translate(&mut list, m - x0, m - y0);
    let mask = Mask::from_list(&list, dev(w), dev(h), &fonts);
    let mut img = Img::new(dev(w), dev(h));
    let s = RASTER_SCALE;
    let d = |v: f32| (v * s).round() as i64;
    if ws.shadow {
        img.paint(&mask.shifted(d(2.0), d(2.0)).blurred(d(1.5)), 0.45, &|_, _| Color::BLACK);
    }
    if let Some(hs) = ws.hard_shadow {
        img.paint(&mask.shifted(d(3.0), d(3.0)), 1.0, &|_, _| hs);
    }
    if let Some(g) = ws.glow {
        img.paint(&mask.dilated(d(3.0)).blurred(d(2.0)), 0.6, &|_, _| lighten(g, 0.3));
    }
    if let Some(o) = ws.outline {
        img.paint(&mask.dilated(d(1.0)), 1.0, &|_, _| o);
    }
    let (y_top, y_bot) = mask.bounds().map_or((0, mask.h), |(_, a, _, b)| (a, b.max(a + 1)));
    let fill = ws.fill;
    let grad = ws.gradient;
    img.paint(&mask, 1.0, &move |_, y| match grad {
        Some(top) => {
            let t = (y.saturating_sub(y_top)) as f32 / (y_bot.saturating_sub(y_top)).max(1) as f32;
            top.lerp(fill, t)
        }
        None => fill,
    });
    if ws.inner_shadow {
        img.paint(&mask.minus(&mask.shifted(d(1.5), d(1.5))), 0.5, &|_, _| darken(fill, 0.6));
    }
    if ws.bevel {
        img.paint(&mask.minus(&mask.shifted(d(1.0), d(1.0))), 0.6, &|_, _| Color::WHITE);
        img.paint(&mask.minus(&mask.shifted(-d(1.0), -d(1.0))), 0.4, &|_, _| darken(fill, 0.5));
    }
    if ws.reflection {
        let r = mask.reflected(i64::try_from(y_bot).unwrap_or(0) + d(1.0), d(th * 0.6));
        img.paint(&r, 1.0, &|_, _| fill);
    }
    Some((img.to_png(), w, h))
}

/// PNG of a SmartArt `layout` over `items` with theme colours and effects; (png, width, height).
fn smart_art_png(
    layout: &str,
    items: &[String],
    theme: &crate::compose::ResolvedTheme,
) -> Option<(Vec<u8>, f32, f32)> {
    let fonts = mailcraft_fonts::FontDb::bundled();
    // At most as many items as fit the largest picture.
    let cap = match layout {
        "list" => 19,
        "process" => 6,
        "hierarchy" => 8,
        _ => 30,
    };
    let items = items.get(..items.len().min(cap)).unwrap_or(items);
    let n = items.len();
    if n == 0 {
        return None;
    }
    // Boxes (x, y, w, h) and connector polygons in logical px.
    let mut boxes: Vec<(f32, f32, f32, f32)> = Vec::new();
    let mut connectors: Vec<Vec<(f32, f32)>> = Vec::new();
    let (w, h) = match layout {
        "list" => {
            for i in 0..n {
                boxes.push((10.0, 10.0 + 50.0 * i as f32, 340.0, 40.0));
            }
            (360.0, 20.0 + 50.0 * n as f32)
        }
        "process" => {
            for i in 0..n {
                let x = 10.0 + 150.0 * i as f32;
                boxes.push((x, 15.0, 115.0, 50.0));
                if i + 1 < n {
                    connectors.push(vec![(x + 120.0, 30.0), (x + 142.0, 40.0), (x + 120.0, 50.0)]);
                }
            }
            (20.0 + 150.0 * n as f32, 80.0)
        }
        "cycle" => {
            let ring_outer = ellipse_pts(210.0, 210.0, 142.0, 142.0);
            let mut ring_inner = ellipse_pts(210.0, 210.0, 138.0, 138.0);
            ring_inner.reverse();
            // A ring as one polygon: outer circle, bridge, inner circle reversed.
            let mut ring = ring_outer;
            ring.extend(ring_inner);
            connectors.push(ring);
            for i in 0..n {
                let a = std::f32::consts::TAU * i as f32 / n as f32 - std::f32::consts::FRAC_PI_2;
                boxes.push((210.0 + 140.0 * a.cos() - 55.0, 210.0 + 140.0 * a.sin() - 22.0, 110.0, 44.0));
            }
            (420.0, 420.0)
        }
        "hierarchy" => {
            let kids = n.saturating_sub(1).max(1);
            let w = (20.0 + 140.0 * kids as f32).max(200.0);
            boxes.push((w / 2.0 - 60.0, 10.0, 120.0, 44.0));
            for i in 1..n {
                let x = 10.0 + 140.0 * (i - 1) as f32;
                let cx = x + 60.0;
                connectors.push(vec![
                    (w / 2.0 - 1.0, 54.0),
                    (w / 2.0 + 1.0, 54.0),
                    (w / 2.0 + 1.0, 80.0),
                    (w / 2.0 - 1.0, 80.0),
                ]);
                let (l, r) = (cx.min(w / 2.0) - 1.0, cx.max(w / 2.0) + 1.0);
                connectors.push(vec![(l, 79.0), (r, 79.0), (r, 81.0), (l, 81.0)]);
                connectors.push(vec![
                    (cx - 1.0, 80.0),
                    (cx + 1.0, 80.0),
                    (cx + 1.0, 110.0),
                    (cx - 1.0, 110.0),
                ]);
                boxes.push((x, 110.0, 120.0, 44.0));
            }
            (w, 170.0)
        }
        _ => return None,
    };
    let (w, h) = (w.min(MAX_RASTER_SIDE), h.min(MAX_RASTER_SIDE));
    let fx = effect_style(theme.effects);
    let mut img = Img::new(dev(w), dev(h));
    let grey = Color::rgb(0x9e, 0x9e, 0x9e);
    if !connectors.is_empty() {
        let mut list = mailcraft_layout::DisplayList::default();
        for c in &connectors {
            polygon_list(c, &mut list);
        }
        img.paint(&Mask::from_list(&list, dev(w), dev(h), &fonts), 1.0, &|_, _| grey);
    }
    // Each box is drawn on its own small canvas (box plus room for effects) and placed on the picture.
    let m = EFFECT_MARGIN / 2.0;
    for (i, (bx, by, bw, bh)) in boxes.iter().enumerate() {
        let color = theme.color(crate::compose::ACCENT1 + i % 6);
        let (lw, lh) = (bw + 2.0 * m, bh + 2.0 * m);
        let mut list = mailcraft_layout::DisplayList::default();
        polygon_list(&rounded_rect_pts(m, m, *bw, *bh, 6.0), &mut list);
        let mask = Mask::from_list(&list, dev(lw), dev(lh), &fonts);
        let at = (((bx - m).max(0.0) * RASTER_SCALE) as usize, ((by - m).max(0.0) * RASTER_SCALE) as usize);
        composite(&mut img, at, &mask, color, &Effect { reflection: false, glow: None, ..fx });
        let label = items.get(i).map(String::as_str).unwrap_or("");
        let ts = TextStyle {
            size_pt: Some(10.0),
            font_family: Some(theme.body_font.clone()),
            ..Default::default()
        };
        let (mut tl, x0, y0, tw, th) = text_list(label, &ts, &fonts);
        if tw > 0.0 {
            translate(&mut tl, m + (bw - tw.min(*bw - 6.0)) / 2.0 - x0, m + (bh - th) / 2.0 - y0);
            let tm = Mask::from_list(&tl, dev(lw), dev(lh), &fonts);
            let clip = mask.eroded(2);
            let text_mask = tm.map(|x, y| tm.get(x, y).min(clip.get(x, y)));
            img.paint_at(&text_mask, at, 1.0, &|_, _| Color::WHITE);
        }
    }
    Some((img.to_png(), w, h))
}

/// Bundled icons: 48×48 SVG bodies drawn for MailCraft.
fn icon_svg(name: &str) -> Option<&'static str> {
    Some(match name {
        "check" => {
            r##"<path d="M8 26 L20 38 L40 12" fill="none" stroke="#196b24" stroke-width="6" stroke-linecap="round" stroke-linejoin="round"/>"##
        }
        "cross" => {
            r##"<path d="M12 12 L36 36 M36 12 L12 36" stroke="#c00000" stroke-width="6" stroke-linecap="round"/>"##
        }
        "star" => {
            r##"<polygon points="24,3 30,18 46,18 33,28 38,44 24,34 10,44 15,28 2,18 18,18" fill="#e9a800"/>"##
        }
        "heart" => {
            r##"<path d="M24 42 C8 30 2 20 8 12 C14 4 22 8 24 14 C26 8 34 4 40 12 C46 20 40 30 24 42 Z" fill="#d13438"/>"##
        }
        "warning" => {
            r##"<path d="M24 4 L46 44 H2 Z" fill="#f2c811"/><path d="M24 18 V30 M24 36 V38" stroke="#222" stroke-width="4" stroke-linecap="round"/>"##
        }
        "info" => {
            r##"<circle cx="24" cy="24" r="21" fill="#0f6cbd"/><path d="M24 21 V35 M24 13 V15" stroke="#fff" stroke-width="5" stroke-linecap="round"/>"##
        }
        "mail" => {
            r##"<rect x="3" y="10" width="42" height="28" rx="3" fill="#0f6cbd"/><path d="M5 13 L24 27 L43 13" fill="none" stroke="#fff" stroke-width="3"/>"##
        }
        "phone" => {
            r##"<rect x="14" y="3" width="20" height="42" rx="4" fill="#424242"/><rect x="17" y="8" width="14" height="28" fill="#9fd3ff"/><circle cx="24" cy="40" r="2" fill="#fff"/>"##
        }
        "calendar" => {
            r##"<rect x="4" y="8" width="40" height="36" rx="3" fill="#fff" stroke="#0f6cbd" stroke-width="3"/><rect x="4" y="8" width="40" height="10" fill="#0f6cbd"/><path d="M14 4 V12 M34 4 V12" stroke="#0f6cbd" stroke-width="3"/>"##
        }
        "clock" => {
            r##"<circle cx="24" cy="24" r="20" fill="#fff" stroke="#424242" stroke-width="3"/><path d="M24 12 V24 L32 30" fill="none" stroke="#424242" stroke-width="3" stroke-linecap="round"/>"##
        }
        "person" => {
            r##"<circle cx="24" cy="15" r="9" fill="#5b5fc7"/><path d="M6 44 C6 32 14 27 24 27 C34 27 42 32 42 44 Z" fill="#5b5fc7"/>"##
        }
        "home" => {
            r##"<path d="M4 24 L24 6 L44 24 H38 V42 H10 V24 Z" fill="#196b24"/><rect x="20" y="30" width="8" height="12" fill="#fff"/>"##
        }
        "lightbulb" => {
            r##"<path d="M24 4 C14 4 9 12 9 18 C9 26 16 29 17 35 H31 C32 29 39 26 39 18 C39 12 34 4 24 4 Z" fill="#f2c811"/><rect x="17" y="37" width="14" height="7" rx="2" fill="#616161"/>"##
        }
        "flag" => {
            r##"<path d="M10 4 V44" stroke="#424242" stroke-width="3"/><path d="M11 6 H40 L33 15 L40 24 H11 Z" fill="#c00000"/>"##
        }
        "arrowRight" => {
            r##"<path d="M4 24 H40 M28 12 L40 24 L28 36" fill="none" stroke="#0f6cbd" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>"##
        }
        "document" => {
            r##"<path d="M10 3 H30 L40 13 V45 H10 Z" fill="#fff" stroke="#616161" stroke-width="2.5"/><path d="M30 3 V13 H40" fill="none" stroke="#616161" stroke-width="2.5"/><path d="M16 22 H34 M16 29 H34 M16 36 H28" stroke="#9e9e9e" stroke-width="2.5"/>"##
        }
        _ => return None,
    })
}

const ICONS: &str = "check, cross, star, heart, warning, info, mail, phone, calendar, clock, person, home, lightbulb, flag, arrowRight, document";

fn icon(s: &mut Session, p: &Value) -> CmdResult {
    let Some(name) = opt_str(p, "name").map(str::to_owned) else {
        return dialog(s, "icons", json!({"compose": compose_id(s, p)?, "available": ICONS}));
    };
    let Some(body) = icon_svg(&name) else {
        return Err(CmdError::Params(format!("unknown icon `{name}` (bundled: {ICONS})")));
    };
    let svg = format!("{}{body}</svg>", svg_open(48.0, 48.0));
    let c = compose_mut(s, p)?;
    let cid = put_svg(c, &name, svg, &name, 48.0, 48.0);
    Ok(json!({"icon": name, "cid": cid}))
}

/// `[[label, value], …]` or `{"labels": [...], "values": [...]}`.
fn chart_data(v: &Value) -> Option<Vec<(String, f64)>> {
    let label = |x: &Value| x.as_str().map(str::to_owned).unwrap_or_else(|| x.to_string());
    let out: Vec<(String, f64)> = if let Some(rows) = v.as_array() {
        rows.iter()
            .filter_map(|r| {
                let r = r.as_array()?;
                Some((label(r.first()?), r.get(1)?.as_f64()?))
            })
            .collect()
    } else {
        let labels = v.get("labels")?.as_array()?;
        let values = v.get("values")?.as_array()?;
        labels.iter().zip(values).filter_map(|(l, x)| Some((label(l), x.as_f64()?))).collect()
    };
    let out: Vec<(String, f64)> = out.into_iter().filter(|(_, x)| x.is_finite()).take(100).collect();
    (!out.is_empty()).then_some(out)
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn chart_svg(kind: &str, title: &str, data: &[(String, f64)]) -> Option<String> {
    let (w, h) = (480.0f32, 300.0f32);
    let mut svg = svg_open(w, h);
    svg.push_str(r##"<rect width="480" height="300" fill="#ffffff"/>"##);
    let _ = write!(
        svg,
        r##"<text x="240" y="22" font-family="sans-serif" font-size="15" text-anchor="middle" fill="#222">{}</text>"##,
        esc(title)
    );
    let n = data.len().max(1) as f32;
    let max = data.iter().map(|(_, v)| *v).fold(0.0f64, f64::max).max(f64::MIN_POSITIVE) as f32;
    let (left, top, right, bottom) = (50.0f32, 36.0f32, 460.0f32, 260.0f32);
    let color = |i: usize| PALETTE.get(i % PALETTE.len()).copied().unwrap_or("#156082");
    match kind {
        "column" | "bar" => {
            let _ =
                write!(svg, r##"<path d="M{left} {top} V{bottom} H{right}" stroke="#888" fill="none"/>"##);
            if kind == "column" {
                let slot = (right - left) / n;
                for (i, (l, v)) in data.iter().enumerate() {
                    let bh = ((*v as f32).max(0.0) / max) * (bottom - top);
                    let x = left + slot * i as f32 + slot * 0.15;
                    let _ = write!(
                        svg,
                        r#"<rect x="{x:.1}" y="{:.1}" width="{:.1}" height="{bh:.1}" fill="{}"/>"#,
                        bottom - bh,
                        slot * 0.7,
                        color(0)
                    );
                    let _ = write!(
                        svg,
                        r##"<text x="{:.1}" y="{:.1}" font-family="sans-serif" font-size="11" text-anchor="middle" fill="#444">{}</text>"##,
                        x + slot * 0.35,
                        bottom + 16.0,
                        esc(l)
                    );
                }
            } else {
                let slot = (bottom - top) / n;
                for (i, (l, v)) in data.iter().enumerate() {
                    let bw = ((*v as f32).max(0.0) / max) * (right - left);
                    let y = top + slot * i as f32 + slot * 0.15;
                    let _ = write!(
                        svg,
                        r#"<rect x="{left}" y="{y:.1}" width="{bw:.1}" height="{:.1}" fill="{}"/>"#,
                        slot * 0.7,
                        color(0)
                    );
                    let _ = write!(
                        svg,
                        r##"<text x="{:.1}" y="{:.1}" font-family="sans-serif" font-size="11" text-anchor="end" fill="#444">{}</text>"##,
                        left - 4.0,
                        y + slot * 0.45,
                        esc(l)
                    );
                }
            }
        }
        "line" => {
            let _ =
                write!(svg, r##"<path d="M{left} {top} V{bottom} H{right}" stroke="#888" fill="none"/>"##);
            let step = if data.len() > 1 { (right - left) / (n - 1.0) } else { 0.0 };
            let mut d = String::new();
            for (i, (l, v)) in data.iter().enumerate() {
                let x = left + step * i as f32;
                let y = bottom - ((*v as f32).max(0.0) / max) * (bottom - top);
                let _ = write!(d, "{}{x:.1} {y:.1} ", if i == 0 { "M" } else { "L" });
                let _ = write!(svg, r#"<circle cx="{x:.1}" cy="{y:.1}" r="3" fill="{}"/>"#, color(0));
                let _ = write!(
                    svg,
                    r##"<text x="{x:.1}" y="{:.1}" font-family="sans-serif" font-size="11" text-anchor="middle" fill="#444">{}</text>"##,
                    bottom + 16.0,
                    esc(l)
                );
            }
            let _ = write!(svg, r#"<path d="{d}" stroke="{}" stroke-width="2.5" fill="none"/>"#, color(0));
        }
        "pie" => {
            let total: f64 = data.iter().map(|(_, v)| v.max(0.0)).sum();
            if total <= 0.0 {
                return None;
            }
            let (cx, cy, r) = (170.0f32, 155.0f32, 105.0f32);
            let mut a0 = -std::f32::consts::FRAC_PI_2;
            for (i, (l, v)) in data.iter().enumerate() {
                let frac = (v.max(0.0) / total) as f32;
                let a1 = a0 + frac * std::f32::consts::TAU;
                let large = i32::from(frac > 0.5);
                if frac >= 0.9999 {
                    let _ = write!(svg, r#"<circle cx="{cx}" cy="{cy}" r="{r}" fill="{}"/>"#, color(i));
                } else if frac > 0.0 {
                    let _ = write!(
                        svg,
                        r#"<path d="M{cx} {cy} L{:.1} {:.1} A{r} {r} 0 {large} 1 {:.1} {:.1} Z" fill="{}"/>"#,
                        cx + r * a0.cos(),
                        cy + r * a0.sin(),
                        cx + r * a1.cos(),
                        cy + r * a1.sin(),
                        color(i)
                    );
                }
                let ly = 60.0 + 20.0 * i as f32;
                let _ = write!(
                    svg,
                    r#"<rect x="310" y="{:.1}" width="12" height="12" fill="{}"/>"#,
                    ly - 10.0,
                    color(i)
                );
                let _ = write!(
                    svg,
                    r##"<text x="328" y="{ly:.1}" font-family="sans-serif" font-size="12" fill="#333">{}</text>"##,
                    esc(l)
                );
                a0 = a1;
            }
        }
        _ => return None,
    }
    svg.push_str("</svg>");
    Some(svg)
}

fn chart(s: &mut Session, p: &Value) -> CmdResult {
    let kind = opt_str(p, "kind").unwrap_or("column").to_owned();
    if !matches!(kind.as_str(), "column" | "bar" | "line" | "pie") {
        return Err(CmdError::Params(format!("unknown chart kind `{kind}` (column, bar, line, pie)")));
    }
    let Some(data) = p.get("data").and_then(chart_data) else {
        return dialog(s, "insertChart", json!({"compose": compose_id(s, p)?, "kind": kind}));
    };
    let title = opt_str(p, "title").unwrap_or("").to_owned();
    let svg = chart_svg(&kind, &title, &data)
        .ok_or_else(|| CmdError::Params("the chart has nothing to show".into()))?;
    let alt = if title.is_empty() { format!("{kind} chart") } else { title };
    let c = compose_mut(s, p)?;
    let cid = put_svg(c, "chart", svg, &alt, 480.0, 300.0);
    Ok(json!({"chart": kind, "points": data.len(), "cid": cid}))
}

fn smart_art(s: &mut Session, p: &Value) -> CmdResult {
    let layout = opt_str(p, "layout").map(str::to_owned);
    let items: Vec<String> = p
        .get("items")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(|t| t.chars().take(80).collect()).take(30).collect())
        .unwrap_or_default();
    let (Some(layout), false) = (layout, items.is_empty()) else {
        return dialog(
            s,
            "smartArt",
            json!({"compose": compose_id(s, p)?, "layouts": ["list", "process", "cycle", "hierarchy"]}),
        );
    };
    if !matches!(layout.as_str(), "list" | "process" | "cycle" | "hierarchy") {
        return Err(CmdError::Params(format!("unknown layout `{layout}` (list, process, cycle, hierarchy)")));
    }
    let cid = compose_id(s, p)?;
    let theme = s
        .composes
        .get(&cid)
        .map(ComposeState::theme_resolved)
        .ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let Some((png, w, h)) = smart_art_png(&layout, &items, &theme) else {
        return Err(CmdError::Failed("the SmartArt graphic could not be drawn".into()));
    };
    let alt = items.join(", ");
    let c = compose_mut(s, p)?;
    let cid = put_png(c, "smartart", png, &alt, w, h)?;
    Ok(json!({"layout": layout, "items": items.len(), "cid": cid, "format": "png"}))
}

/// Parses OBJ / ASCII STL / binary STL into (vertices, edges).
pub fn parse_model(data: &[u8]) -> Option<(Vec<[f64; 3]>, Vec<(usize, usize)>)> {
    const MAX_VERTS: usize = 300_000;
    const MAX_EDGES: usize = 600_000;
    let mut verts: Vec<[f64; 3]> = Vec::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    let text = std::str::from_utf8(data).ok();
    let nums = |it: &mut dyn Iterator<Item = &str>| -> Option<[f64; 3]> {
        let x: f64 = it.next()?.parse().ok()?;
        let y: f64 = it.next()?.parse().ok()?;
        let z: f64 = it.next()?.parse().ok()?;
        (x.is_finite() && y.is_finite() && z.is_finite()).then_some([x, y, z])
    };
    if let Some(t) = text
        && t.trim_start().starts_with("solid")
        && t.contains("vertex")
    {
        // ASCII STL: every three vertices form a facet.
        for line in t.lines() {
            let mut it = line.split_whitespace();
            if it.next() == Some("vertex")
                && let Some(v) = nums(&mut it)
                && verts.len() < MAX_VERTS
            {
                verts.push(v);
                let k = verts.len();
                if k.is_multiple_of(3) && edges.len() < MAX_EDGES {
                    let (a, b, c) = (k - 3, k - 2, k - 1);
                    edges.extend([(a, b), (b, c), (c, a)]);
                }
            }
        }
    } else if let Some(t) = text {
        for line in t.lines() {
            let mut it = line.split_whitespace();
            match it.next() {
                Some("v") if verts.len() < MAX_VERTS => {
                    if let Some(v) = nums(&mut it) {
                        verts.push(v);
                    }
                }
                Some("f" | "l") => {
                    let n = verts.len();
                    let idx: Vec<usize> = it
                        .filter_map(|tok| {
                            let i: i64 = tok.split('/').next()?.parse().ok()?;
                            let n = i64::try_from(n).ok()?;
                            let k = if i < 0 { n.checked_add(i)? } else { i.checked_sub(1)? };
                            usize::try_from(k).ok().filter(|k| *k < verts.len())
                        })
                        .collect();
                    for w in idx.windows(2) {
                        if let [a, b] = w
                            && edges.len() < MAX_EDGES
                        {
                            edges.push((*a, *b));
                        }
                    }
                    if idx.len() > 2
                        && let (Some(a), Some(b)) = (idx.last(), idx.first())
                        && edges.len() < MAX_EDGES
                    {
                        edges.push((*a, *b));
                    }
                }
                _ => {}
            }
        }
    } else {
        // Binary STL: 80-byte header, u32 count, 50 bytes per facet.
        let count = usize::try_from(le32(data, 80)?).ok()?;
        let f32_at =
            |i: usize| -> Option<f64> { Some(f64::from(f32::from_le_bytes(le32(data, i)?.to_le_bytes()))) };
        for t in 0..count.min(MAX_VERTS / 3) {
            let base = 84usize.checked_add(t.checked_mul(50)?)?.checked_add(12)?;
            let start = verts.len();
            for v in 0..3usize {
                let o = base.checked_add(v.checked_mul(12)?)?;
                let p = [f32_at(o)?, f32_at(o.checked_add(4)?)?, f32_at(o.checked_add(8)?)?];
                if p.iter().all(|x| x.is_finite()) {
                    verts.push(p);
                }
            }
            if verts.len() == start.saturating_add(3) && edges.len() < MAX_EDGES {
                edges.extend([(start, start + 1), (start + 1, start + 2), (start + 2, start)]);
            }
        }
    }
    (!verts.is_empty() && !edges.is_empty()).then_some((verts, edges))
}

/// Wireframe of a model, projected isometrically into a `size`×`size` SVG.
#[allow(clippy::cast_possible_truncation)]
pub fn model_svg(verts: &[[f64; 3]], edges: &[(usize, usize)], size: f32) -> String {
    let (ay, ax) = (35f64.to_radians(), 25f64.to_radians());
    let proj: Vec<(f64, f64)> = verts
        .iter()
        .map(|[x, y, z]| {
            let x1 = x * ay.cos() + z * ay.sin();
            let z1 = -x * ay.sin() + z * ay.cos();
            let y1 = y * ax.cos() - z1 * ax.sin();
            (x1, -y1)
        })
        .collect();
    let (mut minx, mut miny, mut maxx, mut maxy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in &proj {
        minx = minx.min(*x);
        miny = miny.min(*y);
        maxx = maxx.max(*x);
        maxy = maxy.max(*y);
    }
    let span = (maxx - minx).max(maxy - miny).max(f64::MIN_POSITIVE);
    let margin = 8.0;
    let scale = (f64::from(size) - 2.0 * margin) / span;
    let mut d = String::new();
    // Keep the SVG reasonably small: sample edges beyond 20k.
    let stride = (edges.len() / 20_000).max(1);
    for (a, b) in edges.iter().step_by(stride) {
        let (Some(pa), Some(pb)) = (proj.get(*a), proj.get(*b)) else {
            continue;
        };
        let _ = write!(
            d,
            "M{:.1} {:.1}L{:.1} {:.1}",
            margin + (pa.0 - minx) * scale,
            margin + (pa.1 - miny) * scale,
            margin + (pb.0 - minx) * scale,
            margin + (pb.1 - miny) * scale
        );
    }
    format!(
        r##"{}<rect width="{size}" height="{size}" fill="#ffffff"/><path d="{d}" stroke="#156082" stroke-width="0.8" fill="none"/></svg>"##,
        svg_open(size, size)
    )
}

fn model3d(s: &mut Session, p: &Value) -> CmdResult {
    let Some((name, data)) = file_param(p, MAX_MODEL_BYTES)? else {
        return dialog(s, "insert3dModel", json!({"compose": compose_id(s, p)?}));
    };
    let Some((verts, edges)) = parse_model(&data) else {
        return Err(CmdError::Failed(format!("{name}: only OBJ and STL models can be shown")));
    };
    let svg = model_svg(&verts, &edges, 320.0);
    let c = compose_mut(s, p)?;
    let cid = put_svg(c, &name, svg, &name, 320.0, 320.0);
    Ok(json!({"inserted": name, "vertices": verts.len(), "edges": edges.len(), "cid": cid}))
}

/// Why Screenshot inserts a file instead of capturing (no screen capture without a platform library).
pub const SCREENSHOT_UNAVAILABLE: &str = "MailCraft cannot capture the screen yet. Take the screenshot with your \
system's screenshot tool, then choose the saved picture to insert it.";

/// Screenshot: inserts a screenshot saved as a picture file (`path`, or `name` + `data_base64`); without
/// one, a dialog explains that capturing is not available and offers to pick the file.
fn screenshot(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("path").is_some() || p.get("data_base64").is_some() {
        let mut r = picture(s, p)?;
        if let Some(o) = r.as_object_mut() {
            o.insert("screenshot".into(), json!(true));
        }
        return Ok(r);
    }
    let window = opt_str(p, "window");
    let clip = opt_bool(p, "clip").unwrap_or(false);
    dialog(
        s,
        "screenshot",
        json!({"compose": compose_id(s, p)?, "window": window, "clip": clip, "capture": false, "message": SCREENSHOT_UNAVAILABLE}),
    )
}

// ---------------------------------------------------------------------------------------------------------
// Tables, rules, text boxes
// ---------------------------------------------------------------------------------------------------------

fn count_param(p: &Value, key: &str) -> Result<Option<usize>, CmdError> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n >= 1)
            .map(Some)
            .ok_or_else(|| CmdError::Params(format!("`{key}` must be a positive number"))),
    }
}

fn table(s: &mut Session, p: &Value) -> CmdResult {
    let mode = opt_str(p, "mode").unwrap_or("grid");
    let rows = count_param(p, "rows")?;
    let cols = count_param(p, "cols")?;
    match mode {
        "draw" => return dialog(s, "drawTable", json!({"compose": compose_id(s, p)?})),
        "fromText" => {
            let c = compose_mut(s, p)?;
            c.checkpoint();
            if !c.text_to_table() {
                c.undo_edit();
                c.edit_redo.clear();
                return Err(CmdError::Failed("select the paragraphs to convert first".into()));
            }
            return Ok(json!({"converted": true, "caret": caret(c)}));
        }
        "quick" => {
            let Some(name) = opt_str(p, "name") else {
                return dialog(s, "quickTables", json!({"compose": compose_id(s, p)?}));
            };
            return insert_building_block(s, p, "quickTables", name);
        }
        "grid" | "dialog" => {}
        other => return Err(CmdError::Params(format!("unknown table mode `{other}`"))),
    }
    let (Some(rows), Some(cols)) = (rows, cols) else {
        return dialog(s, "insertTable", json!({"compose": compose_id(s, p)?}));
    };
    if rows > MAX_TABLE_ROWS || cols > MAX_TABLE_COLS || rows.saturating_mul(cols) > MAX_TABLE_CELLS {
        return Err(CmdError::Params(format!(
            "tables are limited to {MAX_TABLE_ROWS} rows and {MAX_TABLE_COLS} columns"
        )));
    }
    let c = compose_mut(s, p)?;
    c.checkpoint();
    c.insert_blocks(vec![empty_table(rows, cols)]);
    Ok(json!({"rows": rows, "cols": cols, "caret": caret(c)}))
}

fn horizontal_line(s: &mut Session, p: &Value) -> CmdResult {
    edit(s, p, |c| c.insert_blocks(vec![Block::Rule]))?;
    Ok(json!({"inserted": "rule"}))
}

fn text_box(s: &mut Session, p: &Value) -> CmdResult {
    let preset = opt_str(p, "preset").unwrap_or("simple");
    if preset == "draw" {
        return dialog(s, "drawTextBox", json!({"compose": compose_id(s, p)?}));
    }
    if let Some(name) = preset.strip_prefix("saved:") {
        return insert_building_block(s, p, "textBoxes", name);
    }
    let text = opt_str(p, "text").unwrap_or("").to_owned();
    let c = compose_mut(s, p)?;
    c.checkpoint();
    let cell = TableCell { width_px: Some(240.0), ..table_cell(&text) };
    c.insert_blocks(vec![Block::Table { rows: vec![vec![cell]], border: true, width_px: Some(240.0) }]);
    Ok(json!({"textBox": preset, "caret": caret(c)}))
}

// ---------------------------------------------------------------------------------------------------------
// Links, bookmarks, symbols, equations, dates
// ---------------------------------------------------------------------------------------------------------

/// Normalises a link target: keeps known schemes, `#bookmark`; `user@host` → mailto:, bare hosts → http://.
/// Script URLs are refused.
pub fn normalize_url(url: &str) -> Option<String> {
    let u = url.trim();
    if u.is_empty() || u.chars().any(char::is_control) {
        return None;
    }
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("javascript:") || lower.starts_with("vbscript:") || lower.starts_with("data:") {
        return None;
    }
    let known = ["http://", "https://", "mailto:", "ftp://", "file:", "tel:", "#"];
    if known.iter().any(|k| lower.starts_with(k)) {
        return Some(u.to_owned());
    }
    if u.contains('@') && !u.contains('/') {
        return Some(format!("mailto:{u}"));
    }
    if u.contains(':') {
        return None;
    }
    Some(format!("http://{u}"))
}

fn link(s: &mut Session, p: &Value) -> CmdResult {
    let url = opt_str(p, "url");
    let text = opt_str(p, "text").map(str::to_owned);
    let Some(url) = url else {
        let cid = compose_id(s, p)?;
        let (sel, cur) =
            s.composes.get(&cid).map(|c| (c.selected_text(), c.link_at_caret())).unwrap_or_default();
        return dialog(s, "insertLink", json!({"compose": cid, "text": text.unwrap_or(sel), "url": cur}));
    };
    let href = if url.trim().is_empty() {
        None
    } else {
        Some(
            normalize_url(url)
                .ok_or_else(|| CmdError::Params(format!("`{url}` is not a usable link address")))?,
        )
    };
    let c = compose_mut(s, p)?;
    c.checkpoint();
    c.set_link(href.as_deref(), text.as_deref());
    Ok(json!({"url": href, "caret": caret(c)}))
}

fn valid_bookmark(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(char::is_alphabetic)
        && name.chars().count() <= 40
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn bookmark(s: &mut Session, p: &Value) -> CmdResult {
    let Some(name) = opt_str(p, "name").map(str::to_owned) else {
        let cid = compose_id(s, p)?;
        let names: Vec<String> = s
            .composes
            .get(&cid)
            .map(|c| c.bookmarks.iter().map(|b| b.name.clone()).collect())
            .unwrap_or_default();
        return dialog(s, "bookmark", json!({"compose": cid, "bookmarks": names}));
    };
    let action = opt_str(p, "action").unwrap_or("add");
    let c = compose_mut(s, p)?;
    match action {
        "add" => {
            if !valid_bookmark(&name) {
                return Err(CmdError::Params(
                    "bookmark names start with a letter and use letters, digits and _".into(),
                ));
            }
            add_bookmark(c, &name);
            Ok(json!({"bookmark": name, "at": caret(c)}))
        }
        "goTo" => {
            let pos: DocPos = c
                .bookmarks
                .iter()
                .find(|b| b.name == name)
                .map(|b| b.pos.clone())
                .ok_or_else(|| CmdError::Params(format!("no bookmark `{name}`")))?;
            c.set_caret(pos);
            Ok(json!({"caret": caret(c)}))
        }
        "delete" => {
            let before = c.bookmarks.len();
            c.bookmarks.retain(|b| b.name != name);
            Ok(json!({"deleted": before != c.bookmarks.len()}))
        }
        other => Err(CmdError::Params(format!("unknown bookmark action `{other}`"))),
    }
}

/// `"©"`, `"U+00A9"` or `"0x00A9"` → the text to insert.
fn symbol_text(v: &str) -> Option<String> {
    let t = v.trim();
    let hex = t.strip_prefix("U+").or_else(|| t.strip_prefix("u+")).or_else(|| t.strip_prefix("0x"));
    if let Some(h) = hex {
        return u32::from_str_radix(h, 16)
            .ok()
            .and_then(char::from_u32)
            .filter(|c| !c.is_control())
            .map(String::from);
    }
    let n = v.chars().count();
    (1..=8).contains(&n).then(|| v.to_owned()).filter(|s| !s.chars().any(char::is_control))
}

fn symbol(s: &mut Session, p: &Value) -> CmdResult {
    let Some(ch) = opt_str(p, "char") else {
        return dialog(s, "symbol", json!({"compose": compose_id(s, p)?}));
    };
    let text = symbol_text(ch).ok_or_else(|| CmdError::Params(format!("`{ch}` is not a symbol")))?;
    edit(s, p, |c| c.insert_text(&text))?;
    Ok(json!({"inserted": text}))
}

/// Built-in equations: key, gallery name, formula in plain math notation.
pub const EQUATION_PRESETS: [(&str, &str, &str); 6] = [
    ("areaOfCircle", "Area of Circle", "A = \u{3c0}r\u{b2}"),
    (
        "binomialTheorem",
        "Binomial Theorem",
        "(x + a)\u{207f} = \u{3a3}\u{2096}\u{208c}\u{2080}\u{207f} C(n,k) x\u{1d4f} a\u{207f}\u{207b}\u{1d4f}",
    ),
    (
        "expansionOfSum",
        "Expansion of a Sum",
        "(1 + x)\u{207f} = 1 + nx/1! + n(n \u{2212} 1)x\u{b2}/2! + \u{2026}",
    ),
    ("pythagoreanTheorem", "Pythagorean Theorem", "a\u{b2} + b\u{b2} = c\u{b2}"),
    ("quadraticFormula", "Quadratic Formula", "x = (\u{2212}b \u{b1} \u{221a}(b\u{b2} \u{2212} 4ac)) / 2a"),
    ("eulerIdentity", "Euler's Identity", "e^(i\u{3c0}) + 1 = 0"),
];

/// A built-in equation by key ("areaOfCircle") or gallery name ("Area of Circle", any case/spacing).
fn equation_preset(name: &str) -> Option<&'static str> {
    let norm = |t: &str| t.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    let n = norm(name);
    EQUATION_PRESETS.iter().find(|(k, label, _)| norm(k) == n || norm(label) == n).map(|(_, _, f)| *f)
}

fn equation_block(text: &str) -> Block {
    Block::Paragraph {
        style: ParaStyle { align: Align::Center, ..Default::default() },
        inlines: vec![Inline::Text {
            text: text.to_owned(),
            style: TextStyle {
                italic: Some(true),
                font_family: Some("Cambria Math, STIX Two Math, serif".into()),
                ..Default::default()
            },
        }],
    }
}

fn equation(s: &mut Session, p: &Value) -> CmdResult {
    let text = match (opt_str(p, "preset"), opt_str(p, "text")) {
        (_, Some(t)) if !t.trim().is_empty() => t.trim().to_owned(),
        (Some(name), _) => {
            if let Some(t) = equation_preset(name) {
                t.to_owned()
            } else {
                return insert_building_block(s, p, "equations", name);
            }
        }
        _ => {
            return dialog(
                s,
                "equation",
                json!({"compose": compose_id(s, p)?, "presets": EQUATION_PRESETS.iter().map(|(k, n, f)| json!({"key": k, "name": n, "formula": f})).collect::<Vec<_>>()}),
            );
        }
    };
    edit(s, p, |c| c.insert_blocks(vec![equation_block(&text)]))?;
    Ok(json!({"equation": text}))
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Formats `t` (in local offset `tz`) with tokens yyyy yy MMMM MMM MM M dddd ddd dd d HH H hh h mm ss tt;
/// text in single quotes is literal.
pub fn format_date(t: DateTime, tz: i32, fmt: &str) -> String {
    let c = t.civil(tz);
    let month = MONTHS.get(usize::from(c.month.saturating_sub(1))).copied().unwrap_or("January");
    let h12 = match c.hour % 12 {
        0 => 12,
        h => h,
    };
    let chars: Vec<char> = fmt.chars().take(200).collect();
    let mut out = String::new();
    let mut i = 0usize;
    while let Some(&ch) = chars.get(i) {
        if ch == '\'' {
            i = i.saturating_add(1);
            while let Some(&q) = chars.get(i) {
                i = i.saturating_add(1);
                if q == '\'' {
                    break;
                }
                out.push(q);
            }
            continue;
        }
        let run = chars.iter().skip(i).take_while(|&&x| x == ch).count();
        let token: String = std::iter::repeat_n(ch, run).collect();
        let used = match (ch, run) {
            ('y', n) if n >= 4 => {
                let _ = write!(out, "{:04}", c.year);
                n
            }
            ('y', n) => {
                let _ = write!(out, "{:02}", c.year.rem_euclid(100));
                n
            }
            ('M', n) if n >= 4 => {
                out.push_str(month);
                n
            }
            ('M', 3) => {
                out.push_str(month.get(..3).unwrap_or(month));
                3
            }
            ('M', 2) => {
                let _ = write!(out, "{:02}", c.month);
                2
            }
            ('M', 1) => {
                let _ = write!(out, "{}", c.month);
                1
            }
            ('d', n) if n >= 4 => {
                out.push_str(c.weekday.long_name());
                n
            }
            ('d', 3) => {
                out.push_str(c.weekday.short_name());
                3
            }
            ('d', 2) => {
                let _ = write!(out, "{:02}", c.day);
                2
            }
            ('d', 1) => {
                let _ = write!(out, "{}", c.day);
                1
            }
            ('H', n) => {
                if n >= 2 {
                    let _ = write!(out, "{:02}", c.hour);
                } else {
                    let _ = write!(out, "{}", c.hour);
                }
                n
            }
            ('h', n) => {
                if n >= 2 {
                    let _ = write!(out, "{h12:02}");
                } else {
                    let _ = write!(out, "{h12}");
                }
                n
            }
            ('m', n) => {
                let _ = write!(out, "{:02}", c.minute);
                n
            }
            ('s', n) => {
                let _ = write!(out, "{:02}", c.second);
                n
            }
            ('t', n) => {
                out.push_str(if c.hour < 12 { "AM" } else { "PM" });
                n
            }
            _ => {
                out.push_str(&token);
                run
            }
        };
        i = i.saturating_add(used.max(1));
    }
    out
}

fn date_time(s: &mut Session, p: &Value) -> CmdResult {
    let Some(fmt) = opt_str(p, "format").map(str::to_owned) else {
        return dialog(s, "dateTime", json!({"compose": compose_id(s, p)?}));
    };
    let text = format_date(s.now(), s.tz_offset_min, &fmt);
    edit(s, p, |c| c.insert_text(&text))?;
    Ok(json!({"inserted": text}))
}

fn drop_cap(s: &mut Session, p: &Value) -> CmdResult {
    let kind = req_str(p, "kind")?;
    if !matches!(kind, "none" | "dropped" | "inMargin") {
        return Err(CmdError::Params(format!("unknown drop cap `{kind}`")));
    }
    let c = compose_mut(s, p)?;
    let base = c.body.base.size_pt.unwrap_or(mailcraft_fonts::BODY_SIZE_PT);
    let path = c.selection.focus.path.clone();
    c.checkpoint();
    let ok = c.first_char_style_mut(&path, |st| {
        if kind == "none" {
            st.size_pt = None;
            st.bold = None;
        } else {
            st.size_pt = Some((base * 3.0).min(super::format::MAX_FONT_PT));
            st.bold = Some(true);
        }
    });
    if !ok {
        return Err(CmdError::Failed("put the caret in a paragraph with text".into()));
    }
    Ok(json!({"dropCap": kind}))
}

// ---------------------------------------------------------------------------------------------------------
// Building blocks, WordArt
// ---------------------------------------------------------------------------------------------------------

/// Building-block file in the profile directory.
pub const BUILDING_BLOCKS_FILE: &str = "building-blocks.json";
/// Largest building-block file read.
const MAX_BUILDING_BLOCKS_BYTES: u64 = 16 * 1024 * 1024;
/// Most saved building blocks.
const MAX_BUILDING_BLOCKS: usize = 1000;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct BuildingBlocksFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: Vec<BuildingBlock>,
}

fn blocks_path(s: &Session) -> Option<std::path::PathBuf> {
    s.store.as_ref().map(|st| st.dir.join(BUILDING_BLOCKS_FILE))
}

/// Saved building blocks from the profile (empty without a profile, or when the file is missing or garbled).
pub fn load_building_blocks(s: &Session) -> Vec<BuildingBlock> {
    let Some(path) = blocks_path(s) else {
        return Vec::new();
    };
    let too_big = std::fs::metadata(&path).map(|m| m.len() > MAX_BUILDING_BLOCKS_BYTES).unwrap_or(true);
    if too_big {
        return Vec::new();
    }
    std::fs::read(&path)
        .ok()
        .and_then(|d| serde_json::from_slice::<BuildingBlocksFile>(&d).ok())
        .map(|f| {
            f.entries
                .into_iter()
                .filter(|b| GALLERIES.contains(&b.gallery.as_str()) || b.gallery == LIST_STYLES_GALLERY)
                .take(MAX_BUILDING_BLOCKS)
                .collect()
        })
        .unwrap_or_default()
}

/// Writes `entries` to the profile's building-block file (atomically: temp file, then rename). Without a
/// profile (in-memory session) nothing is written.
fn save_building_blocks(s: &Session, entries: &[BuildingBlock]) -> Result<(), CmdError> {
    let Some(path) = blocks_path(s) else {
        return Ok(());
    };
    let file = BuildingBlocksFile { version: 1, entries: entries.to_vec() };
    let text = serde_json::to_vec_pretty(&file).map_err(|e| CmdError::Failed(e.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| CmdError::Failed(format!("the building blocks could not be saved: {e}")))
}

/// Every building block: the profile's, then entries only on open composes (newer entries of the same
/// gallery and name win).
fn all_blocks(s: &Session, cid: u64) -> Vec<BuildingBlock> {
    let mut out = every_block(s, cid);
    out.retain(|b| b.gallery != LIST_STYLES_GALLERY);
    out
}

/// [`all_blocks`] including the named list styles.
fn every_block(s: &Session, cid: u64) -> Vec<BuildingBlock> {
    let mut out = load_building_blocks(s);
    let own = s.composes.get(&cid).into_iter();
    let others = s.composes.iter().filter(|(k, _)| **k != cid).map(|(_, c)| c);
    for b in others.chain(own).flat_map(|c| c.building_blocks.iter()) {
        out.retain(|x| !(x.gallery == b.gallery && x.name.eq_ignore_ascii_case(&b.name)));
        out.push(b.clone());
    }
    out
}

/// Named list styles (Define New List Style): name and level formats, oldest first. Entries that do not
/// decode are skipped.
pub(crate) fn list_styles(s: &Session, cid: u64) -> Vec<(String, Vec<crate::compose::ListFormat>)> {
    let mut out: Vec<(String, Vec<crate::compose::ListFormat>)> = every_block(s, cid)
        .into_iter()
        .filter(|b| b.gallery == LIST_STYLES_GALLERY)
        .filter_map(|b| {
            let text = match b.blocks.first() {
                Some(Block::Pre { text }) => text.clone(),
                _ => return None,
            };
            let levels: Vec<crate::compose::ListFormat> = serde_json::from_str(&text).ok()?;
            (!levels.is_empty() && levels.len() <= crate::compose::MAX_LIST_LEVEL).then_some((b.name, levels))
        })
        .collect();
    // The newest ones when there are too many.
    let extra = out.len().saturating_sub(MAX_LIST_STYLES);
    out.drain(..extra);
    out
}

/// Saves a named list style for every open compose and in the profile (replacing one of the same name).
/// Returns whether it was written to the profile.
pub(crate) fn save_list_style(
    s: &mut Session,
    name: &str,
    levels: &[crate::compose::ListFormat],
) -> Result<bool, CmdError> {
    let name: String = name.trim().chars().take(64).collect();
    if name.is_empty() {
        return Err(CmdError::Params("a list style needs a name".into()));
    }
    let text = serde_json::to_string(levels).map_err(|e| CmdError::Failed(e.to_string()))?;
    let entry = BuildingBlock {
        gallery: LIST_STYLES_GALLERY.to_owned(),
        name: name.clone(),
        blocks: vec![Block::Pre { text }],
    };
    let same = |b: &BuildingBlock| b.gallery == LIST_STYLES_GALLERY && b.name.eq_ignore_ascii_case(&name);
    for c in s.composes.values_mut() {
        c.building_blocks.retain(|b| !same(b));
        c.building_blocks.push(entry.clone());
        let styles = c.building_blocks.iter().filter(|b| b.gallery == LIST_STYLES_GALLERY).count();
        if styles > MAX_LIST_STYLES
            && let Some(i) = c.building_blocks.iter().position(|b| b.gallery == LIST_STYLES_GALLERY)
        {
            c.building_blocks.remove(i);
        }
    }
    let mut saved = load_building_blocks(s);
    saved.retain(|b| !same(b));
    saved.push(entry);
    let styles = saved.iter().filter(|b| b.gallery == LIST_STYLES_GALLERY).count();
    if styles > MAX_LIST_STYLES
        && let Some(i) = saved.iter().position(|b| b.gallery == LIST_STYLES_GALLERY)
    {
        saved.remove(i);
    }
    if saved.len() > MAX_BUILDING_BLOCKS {
        let extra = saved.len() - MAX_BUILDING_BLOCKS;
        saved.drain(..extra);
    }
    save_building_blocks(s, &saved)?;
    Ok(blocks_path(s).is_some())
}

fn find_block(s: &Session, cid: u64, gallery: Option<&str>, name: &str) -> Option<BuildingBlock> {
    all_blocks(s, cid)
        .into_iter()
        .rev()
        .find(|b| b.name.eq_ignore_ascii_case(name) && gallery.is_none_or(|g| b.gallery == g))
}

fn insert_building_block(s: &mut Session, p: &Value, gallery: &str, name: &str) -> CmdResult {
    let cid = compose_id(s, p)?;
    let Some(block) = find_block(s, cid, Some(gallery), name) else {
        return Err(CmdError::Params(format!("no saved entry `{name}` in {gallery}")));
    };
    edit(s, p, |c| c.insert_blocks(block.blocks.clone()))?;
    Ok(json!({"inserted": block.name, "gallery": gallery}))
}

fn quick_part(s: &mut Session, p: &Value) -> CmdResult {
    let gallery = opt_str(p, "gallery");
    if let Some(g) = gallery
        && !GALLERIES.contains(&g)
    {
        return Err(CmdError::Params(format!("unknown gallery `{g}`")));
    }
    let Some(name) = opt_str(p, "name").map(str::to_owned) else {
        let cid = compose_id(s, p)?;
        let entries: Vec<Value> = all_blocks(s, cid)
            .iter()
            .filter(|b| gallery.is_none_or(|g| b.gallery == g))
            .map(|b| {
                let preview: String = Doc { blocks: b.blocks.clone(), ..Default::default() }
                    .text_content()
                    .chars()
                    .take(120)
                    .collect();
                json!({"gallery": b.gallery, "name": b.name, "preview": preview})
            })
            .collect();
        return dialog(s, "quickParts", json!({"compose": cid, "gallery": gallery, "entries": entries}));
    };
    let cid = compose_id(s, p)?;
    if opt_str(p, "action") == Some("delete") {
        // Building Blocks Organizer › Delete.
        let matches = |b: &BuildingBlock| {
            b.gallery != LIST_STYLES_GALLERY
                && b.name.eq_ignore_ascii_case(&name)
                && gallery.is_none_or(|g| b.gallery == g)
        };
        let mut saved = load_building_blocks(s);
        let before = saved.len();
        saved.retain(|b| !matches(b));
        let mut removed = before - saved.len();
        for c in s.composes.values_mut() {
            let n = c.building_blocks.len();
            c.building_blocks.retain(|b| !matches(b));
            removed = removed.max(n - c.building_blocks.len());
        }
        if removed == 0 {
            return Err(CmdError::Params(format!("no saved entry `{name}`")));
        }
        save_building_blocks(s, &saved)?;
        return Ok(json!({"deleted": name}));
    }
    let found = find_block(s, cid, gallery, &name)
        .or_else(|| if gallery.is_none() { None } else { find_block(s, cid, None, &name) })
        .ok_or_else(|| CmdError::Params(format!("no saved entry `{name}`")))?;
    edit(s, p, |c| {
        let single = matches!(found.blocks.as_slice(), [Block::Paragraph { .. }]);
        if single {
            c.insert_fragment(
                &Doc { blocks: found.blocks.clone(), ..Default::default() },
                crate::compose::PasteMode::KeepSource,
            );
        } else {
            c.insert_blocks(found.blocks.clone());
        }
    })?;
    Ok(json!({"inserted": found.name, "gallery": found.gallery}))
}

fn save_to_gallery(s: &mut Session, p: &Value) -> CmdResult {
    let gallery = req_str(p, "gallery")?.to_owned();
    if !GALLERIES.contains(&gallery.as_str()) {
        return Err(CmdError::Params(format!("unknown gallery `{gallery}`")));
    }
    let c = compose_mut(s, p)?;
    if c.selection.is_collapsed() {
        return Err(CmdError::Disabled("select what to save first".into()));
    }
    let frag = c.fragment();
    let name = match opt_str(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.chars().take(64).collect::<String>(),
        None => c
            .selected_text()
            .split_whitespace()
            .take(4)
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(64)
            .collect(),
    };
    if name.is_empty() {
        return Err(CmdError::Params("`name` required".into()));
    }
    let entry = BuildingBlock { gallery: gallery.clone(), name: name.clone(), blocks: frag.blocks };
    let same = |b: &BuildingBlock| b.gallery == entry.gallery && b.name.eq_ignore_ascii_case(&entry.name);
    // Every open compose shares the galleries; the profile keeps them.
    for other in s.composes.values_mut() {
        other.building_blocks.retain(|b| !same(b));
        other.building_blocks.push(entry.clone());
    }
    let mut saved = load_building_blocks(s);
    saved.retain(|b| !same(b));
    saved.push(entry.clone());
    if saved.len() > MAX_BUILDING_BLOCKS {
        let extra = saved.len() - MAX_BUILDING_BLOCKS;
        saved.drain(..extra);
    }
    let persisted = blocks_path(s).is_some();
    save_building_blocks(s, &saved)?;
    Ok(json!({"saved": name, "gallery": gallery, "persisted": persisted}))
}

/// WordArt: the selected text (or `text`, or "Your text here") drawn in the chosen style with the theme's
/// heading font and colours, inserted as a picture in place of the selection.
fn word_art(s: &mut Session, p: &Value) -> CmdResult {
    let style = req_str(p, "style")?.to_owned();
    let cid = compose_id(s, p)?;
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let theme = c.theme_resolved();
    let selected = (!c.selection.is_collapsed()).then(|| c.selected_text());
    let text: String = opt_str(p, "text")
        .map(str::to_owned)
        .or(selected)
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Your text here".to_owned())
        .chars()
        .take(200)
        .collect();
    let Some((png, w, h)) = wordart_png(&text, &style, &theme) else {
        return Err(CmdError::Failed("the WordArt could not be drawn".into()));
    };
    let c = compose_mut(s, p)?;
    c.checkpoint();
    c.delete_selection();
    let cid = put_png(c, "wordart", png, &text, w, h)?;
    Ok(json!({"wordArt": style, "text": text, "cid": cid, "format": "png"}))
}

// ---------------------------------------------------------------------------------------------------------
// Business card, calendar, object
// ---------------------------------------------------------------------------------------------------------

fn business_card(s: &mut Session, p: &Value) -> CmdResult {
    let Some(id) = p
        .get("contactId")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())))
    else {
        return dialog(s, "insertBusinessCard", json!({"compose": compose_id(s, p)?}));
    };
    let Some(contact) = s.mailbox.contacts.get(&ItemId(id)).cloned() else {
        return Err(CmdError::Params(format!("no contact with id {id}")));
    };
    let name = if contact.full_name().is_empty() { contact.file_as.clone() } else { contact.full_name() };
    let mut blocks = vec![Block::Paragraph {
        style: ParaStyle::default(),
        inlines: vec![Inline::Text {
            text: name.clone(),
            style: TextStyle { bold: Some(true), size_pt: Some(14.0), ..Default::default() },
        }],
    }];
    for line in [&contact.job_title, &contact.company].into_iter().filter(|l| !l.is_empty()) {
        blocks.push(Block::text_paragraph(line));
    }
    for e in contact.emails.iter().filter(|e| !e.is_empty()) {
        blocks.push(Block::Paragraph {
            style: ParaStyle::default(),
            inlines: vec![Inline::Link {
                href: format!("mailto:{e}"),
                children: vec![Inline::Text { text: e.clone(), style: TextStyle::default() }],
            }],
        });
    }
    for (kind, num) in &contact.phones {
        blocks.push(Block::text_paragraph(&format!("{kind}: {num}")));
    }
    let card = Block::Table {
        rows: vec![vec![TableCell {
            blocks,
            colspan: 1,
            rowspan: 1,
            background: Some(Color::rgb(0xf5, 0xf5, 0xf5)),
            width_px: Some(320.0),
        }]],
        border: true,
        width_px: Some(320.0),
    };
    let att = ComposeAttachment {
        filename: safe_name(&name, "contact", "vcf"),
        mime_type: "text/vcard".into(),
        data: vcard(&contact).into_bytes(),
    };
    let c = compose_mut(s, p)?;
    c.checkpoint();
    let file = att.filename.clone();
    c.attachments.push(att);
    c.insert_blocks(vec![card]);
    Ok(json!({"card": name, "attached": file}))
}

fn busy_label(b: BusyStatus) -> &'static str {
    match b {
        BusyStatus::Free => "Free",
        BusyStatus::WorkingElsewhere => "Working Elsewhere",
        BusyStatus::Tentative => "Tentative",
        BusyStatus::Busy => "Busy",
        BusyStatus::OutOfOffice => "Out of Office",
    }
}

fn calendar(s: &mut Session, p: &Value) -> CmdResult {
    let (Some(from), Some(to)) = (opt_str(p, "from"), opt_str(p, "to").or_else(|| opt_str(p, "from"))) else {
        return dialog(s, "insertCalendar", json!({"compose": compose_id(s, p)?}));
    };
    let start =
        parse_when(s, from).ok_or_else(|| CmdError::Params(format!("cannot read the date `{from}`")))?;
    let end_day =
        parse_when(s, to).ok_or_else(|| CmdError::Params(format!("cannot read the date `{to}`")))?;
    let tz = s.tz_offset_min;
    let (d0, d1) = (start.day_number(tz), end_day.day_number(tz));
    if d1 < d0 || d1.saturating_sub(d0) > 366 {
        return Err(CmdError::Params("the date range must run forwards and span at most a year".into()));
    }
    let detail = opt_str(p, "detail").unwrap_or("limited");
    if !matches!(detail, "availability" | "limited" | "full") {
        return Err(CmdError::Params(format!("unknown detail `{detail}`")));
    }
    let cal_filter = opt_str(p, "calendar");
    let folders: Vec<mailcraft_model::FolderId> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.kind == FolderKind::Calendar)
        .filter(|f| cal_filter.is_none_or(|c| f.name.eq_ignore_ascii_case(c) || f.id.0.to_string() == c))
        .map(|f| f.id)
        .collect();
    if folders.is_empty() {
        return Err(CmdError::Params("no such calendar".into()));
    }
    let cal_name = cal_filter.unwrap_or("Calendar").to_owned();
    let mut appts: Vec<&mailcraft_model::Appointment> = s
        .mailbox
        .appointments
        .values()
        .filter(|a| folders.contains(&a.folder))
        .filter(|a| a.end.day_number(tz) >= d0 && a.start.day_number(tz) <= d1)
        .collect();
    appts.sort_by_key(|a| a.start.unix);
    let bold = |t: String| Block::Paragraph {
        style: ParaStyle::default(),
        inlines: vec![Inline::Text { text: t, style: TextStyle { bold: Some(true), ..Default::default() } }],
    };
    let mut blocks = vec![bold(cal_name)];
    blocks.push(Block::text_paragraph(&if d0 == d1 {
        ListTime::long_date(start, tz)
    } else {
        format!("{} \u{2013} {}", ListTime::long_date(start, tz), ListTime::long_date(end_day, tz))
    }));
    for day in d0..=d1 {
        let today: Vec<_> =
            appts.iter().filter(|a| a.start.day_number(tz) <= day && a.end.day_number(tz) >= day).collect();
        if today.is_empty() {
            continue;
        }
        let day_t = DateTime::from_unix(
            day.saturating_mul(86_400).saturating_sub(i64::from(tz).saturating_mul(60)),
            tz,
        );
        blocks.push(bold(ListTime::long_date(day_t, tz)));
        let mut items = Vec::new();
        for a in today {
            let when = if a.all_day {
                "All day".to_owned()
            } else {
                format!("{} \u{2013} {}", ListTime::clock(a.start, tz), ListTime::clock(a.end, tz))
            };
            let line = match detail {
                "availability" => format!("{when}  {}", busy_label(a.show_as)),
                _ if a.private => format!("{when}  {}  Private", busy_label(a.show_as)),
                "limited" => format!("{when}  {}", a.subject),
                _ => {
                    let mut l = format!("{when}  {}", a.subject);
                    if !a.location.is_empty() {
                        let _ = write!(l, " ({})", a.location);
                    }
                    if !a.body.trim().is_empty() {
                        let _ = write!(l, " \u{2014} {}", a.body.trim().replace('\n', " "));
                    }
                    l
                }
            };
            items.push(vec![Block::text_paragraph(&line)]);
        }
        blocks.push(Block::List { kind: mailcraft_model::rich::ListKind::Bullet, items });
    }
    let n = appts.len();
    edit(s, p, |c| c.insert_blocks(blocks))?;
    Ok(json!({"appointments": n, "detail": detail}))
}

fn object(s: &mut Session, p: &Value) -> CmdResult {
    let Some((name, data)) = file_param(p, super::compose::MAX_ATTACHMENT_BYTES)? else {
        return dialog(s, "insertObject", json!({"compose": compose_id(s, p)?}));
    };
    let as_text = opt_bool(p, "asText").unwrap_or_else(|| mime_for(&name).starts_with("text/"));
    if as_text {
        let text =
            String::from_utf8(data).map_err(|_| CmdError::Failed(format!("{name} is not a text file")))?;
        if text.len() > 4 * 1024 * 1024 {
            return Err(CmdError::Failed(format!("{name} is too long to insert as text")));
        }
        edit(s, p, |c| c.insert_text(&text))?;
        return Ok(json!({"inserted": name, "asText": true}));
    }
    let svg = format!("{}{}</svg>", svg_open(48.0, 48.0), icon_svg("document").unwrap_or(""));
    let mime = mime_for(&name).to_owned();
    let c = compose_mut(s, p)?;
    c.attachments.push(ComposeAttachment { filename: name.clone(), mime_type: mime, data });
    put_svg(c, "object", svg, &name, 48.0, 48.0);
    Ok(json!({"inserted": name, "asText": false, "attached": true}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "insert.bookmark",
            "Bookmark",
            "Insert › Links",
            "",
            r#"{"name"?:string,"action"?:"add"|"goTo"|"delete"}"#,
            false,
            has_compose,
            bookmark,
        ),
        spec(
            "insert.businessCard",
            "Business Card",
            "Insert › Include; Message › Include › Attach File › Attach Item",
            "",
            r#"{"contactId"?:string}"#,
            false,
            rich_compose,
            business_card,
        ),
        spec(
            "insert.calendar",
            "Calendar",
            "Insert › Include",
            "",
            r#"{"calendar"?:string,"from"?:string,"to"?:string,"detail"?:"availability"|"limited"|"full"}"#,
            false,
            rich_compose,
            calendar,
        ),
        spec(
            "insert.chart",
            "Chart",
            "Insert › Illustrations",
            "",
            r#"{"kind"?:"column"|"bar"|"line"|"pie","data"?:[[string,number]]|{"labels":[..],"values":[..]},"title"?:string}"#,
            false,
            rich_compose,
            chart,
        ),
        spec(
            "insert.dateTime",
            "Date & Time",
            "Insert › Text",
            "",
            r#"{"format"?:string}  (tokens yyyy MMMM MMM MM M dddd ddd dd d HH hh h mm ss tt)"#,
            false,
            has_compose,
            date_time,
        ),
        spec(
            "insert.dropCap",
            "Drop Cap",
            "Insert › Text",
            "",
            r#"{"kind":"none"|"dropped"|"inMargin"}"#,
            false,
            rich_compose,
            drop_cap,
        ),
        spec(
            "insert.equation",
            "Equation",
            "Insert › Symbols",
            "",
            r#"{"preset"?:string,"text"?:string}"#,
            false,
            rich_compose,
            equation,
        ),
        spec(
            "insert.horizontalLine",
            "Horizontal Line",
            "Insert › Symbols; Format Text › Paragraph › Borders",
            "",
            "",
            false,
            rich_compose,
            horizontal_line,
        ),
        spec(
            "insert.icon",
            "Icons",
            "Insert › Illustrations",
            "",
            r#"{"name"?:string}"#,
            false,
            rich_compose,
            icon,
        ),
        spec(
            "insert.link",
            "Link",
            "Message › Include; Insert › Links",
            "Mod+K",
            r#"{"url"?:string,"text"?:string}  ("" url removes the link)"#,
            false,
            rich_compose,
            link,
        ),
        spec(
            "insert.model3d",
            "3D Models",
            "Insert › Illustrations",
            "",
            r#"{"path"?:string} | {"name":string,"data_base64":string}"#,
            false,
            rich_compose,
            model3d,
        ),
        spec(
            "insert.object",
            "Object",
            "Insert › Text",
            "",
            r#"{"path"?:string,"asText"?:bool}"#,
            false,
            rich_compose,
            object,
        ),
        spec(
            "insert.picture",
            "Pictures",
            "Insert › Illustrations",
            "",
            r#"{"path"?:string,"alt"?:string} | {"name":string,"data_base64":string}"#,
            false,
            rich_compose,
            picture,
        ),
        spec(
            "insert.quickPart",
            "Quick Parts",
            "Insert › Text",
            "",
            r#"{"name"?:string,"gallery"?:string,"action"?:"insert"|"delete"}"#,
            false,
            has_compose,
            quick_part,
        ),
        spec(
            "insert.saveToGallery",
            "Save Selection to Gallery",
            "Insert › Text › Quick Parts / Text Box; Insert › Tables › Quick Tables; Insert › Symbols › Equation",
            "",
            r#"{"gallery":"quickParts"|"autoText"|"textBoxes"|"quickTables"|"equations","name"?:string}"#,
            false,
            has_body_selection,
            save_to_gallery,
        ),
        spec(
            "insert.screenshot",
            "Screenshot",
            "Insert › Illustrations",
            "",
            r#"{"path"?:string,"name"?:string,"data_base64"?:string,"window"?:string,"clip"?:bool}"#,
            false,
            rich_compose,
            screenshot,
        ),
        spec(
            "insert.shape",
            "Shapes",
            "Insert › Illustrations",
            "",
            r#"{"kind":string,"color"?:string}"#,
            false,
            rich_compose,
            shape,
        ),
        spec(
            "insert.smartArt",
            "SmartArt",
            "Insert › Illustrations",
            "",
            r#"{"layout"?:"list"|"process"|"cycle"|"hierarchy","items"?:[string]}"#,
            false,
            rich_compose,
            smart_art,
        ),
        spec(
            "insert.symbol",
            "Symbol",
            "Insert › Symbols",
            "",
            r#"{"char"?:string}  ("©" or "U+00A9")"#,
            false,
            has_compose,
            symbol,
        ),
        spec(
            "insert.table",
            "Table",
            "Insert › Tables",
            "",
            r#"{"rows"?:number,"cols"?:number,"mode"?:"grid"|"dialog"|"draw"|"fromText"|"quick","name"?:string}"#,
            false,
            rich_compose,
            table,
        ),
        spec(
            "insert.textBox",
            "Text Box",
            "Insert › Text",
            "",
            r#"{"preset"?:string,"text"?:string}  ("draw" opens the drawing tool)"#,
            false,
            rich_compose,
            text_box,
        ),
        spec(
            "insert.wordArt",
            "WordArt",
            "Insert › Text",
            "",
            r#"{"style":string,"text"?:string}"#,
            false,
            rich_compose,
            word_art,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::compose::tests::{open, session};
    use super::*;
    use mailcraft_model::{Appointment, Contact};

    fn setup() -> (Session, u64) {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "start"})).unwrap();
        (s, id)
    }

    /// Params of the most recent dialog a command opened.
    pub(crate) fn last_dialog(s: &Session) -> Value {
        s.notices
            .iter()
            .rev()
            .find_map(|n| match n {
                crate::Notice::OpenDialog(_, p) => Some(p.clone()),
                _ => None,
            })
            .unwrap_or(Value::Null)
    }

    const PNG_1X1: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    #[test]
    fn picture_from_data_and_file() {
        let (mut s, id) = setup();
        let r = s.run("insert.picture", &json!({"name": "dot.png", "data_base64": PNG_1X1})).unwrap();
        assert_eq!(r["width"], 1.0);
        let c = &s.composes[&id];
        assert_eq!(c.inline_images.len(), 1);
        assert_eq!(c.images()[0].0, format!("cid:{}", c.inline_images[0].content_id));
        assert!(s.run("insert.picture", &json!({"name": "x.txt", "data_base64": "aGVsbG8="})).is_err());
        assert_eq!(s.run("insert.picture", &json!({})).unwrap()["dialog"], "insertPicture");
        // Sending embeds it as a related part.
        s.run("compose.setTo", &json!({"value": "a@example.com"})).unwrap();
        assert_eq!(s.run("compose.send", &json!({})).unwrap()["sent"], true);
        let sent = s.mailbox.mail.values().find(|m| m.subject.is_empty() && !m.is_draft).unwrap();
        assert!(String::from_utf8_lossy(sent.raw.as_ref().unwrap()).contains("Content-ID"));
    }

    #[test]
    fn image_sizes() {
        let png = mailcraft_mime::transfer::decode_base64(PNG_1X1.as_bytes());
        assert_eq!(image_size(&png), Some((1, 1)));
        let gif = b"GIF89a\x10\x00\x20\x00rest";
        assert_eq!(image_size(gif), Some((16, 32)));
        let jpeg =
            [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0, 0, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x30, 0x00, 0x40];
        assert_eq!(image_size(&jpeg), Some((64, 48)));
        assert_eq!(image_size(&[0xff, 0xd8, 0xff]), None);
        assert_eq!(display_size(Some((1200, 600))), (Some(600.0), Some(300.0)));
    }

    #[test]
    fn graphics() {
        let (mut s, id) = setup();
        s.run("insert.shape", &json!({"kind": "star"})).unwrap();
        assert!(s.run("insert.shape", &json!({"kind": "blob"})).is_err());
        s.run("insert.icon", &json!({"name": "check"})).unwrap();
        assert_eq!(s.run("insert.icon", &json!({})).unwrap()["dialog"], "icons");
        s.run("insert.chart", &json!({"kind": "pie", "data": [["a", 1], ["b", 3]], "title": "Share <1>"}))
            .unwrap();
        s.run("insert.chart", &json!({"kind": "line", "data": {"labels": ["x", "y"], "values": [1, 2]}}))
            .unwrap();
        s.run("insert.chart", &json!({"kind": "bar", "data": [["a", 1]]})).unwrap();
        assert_eq!(s.run("insert.chart", &json!({"kind": "column"})).unwrap()["dialog"], "insertChart");
        for layout in ["list", "process", "cycle", "hierarchy"] {
            s.run("insert.smartArt", &json!({"layout": layout, "items": ["A", "B", "C"]})).unwrap();
        }
        assert_eq!(s.run("insert.smartArt", &json!({})).unwrap()["dialog"], "smartArt");
        let c = &s.composes[&id];
        assert_eq!(c.inline_images.len(), 9);
        let chart = String::from_utf8(c.inline_images[2].data.clone()).unwrap();
        assert!(chart.contains("Share &lt;1&gt;"));
        // Shapes and SmartArt are PNG; icons and charts stay SVG.
        let mimes: Vec<&str> = c.inline_images.iter().map(|i| i.mime_type.as_str()).collect();
        assert_eq!(mimes[0], "image/png");
        assert!(mimes[5..].iter().all(|m| *m == "image/png"), "{mimes:?}");
        assert!(
            c.inline_images
                .iter()
                .filter(|i| i.mime_type == "image/png")
                .all(|i| i.data.starts_with(b"\x89PNG"))
        );
        let r = s.run("insert.screenshot", &json!({"clip": true})).unwrap();
        assert_eq!(r["dialog"], "screenshot");
        assert_eq!(last_dialog(&s)["capture"], false);
    }

    /// Decoded RGBA of a generated PNG via the renderer's own decoder path is not available here, so check
    /// pixels through the mask helpers instead: a shape covers its middle and leaves its corner empty.
    #[test]
    fn shapes_are_drawn_in_theme_colours_with_effects() {
        let theme = crate::compose::ThemeState::default().resolve();
        let accent = theme.color(crate::compose::ACCENT1);
        let fonts = mailcraft_fonts::FontDb::bundled();
        let mut list = mailcraft_layout::DisplayList::default();
        polygon_list(&shape_points("rectangle", 0.0, 0.0).unwrap(), &mut list);
        let mask = Mask::from_list(&list, dev(160.0), dev(100.0), &fonts);
        assert!(mask.get(160, 100) > 250, "inside");
        assert_eq!(mask.get(2, 2), 0, "outside the 4 px inset");
        let mut img = Img::new(mask.w, mask.h);
        composite(&mut img, (0, 0), &mask, accent, &effect_style("Subtle Solids"));
        let mid = img.px[100 * mask.w + 160];
        assert!((mid[0] * 255.0 - f32::from(accent.r)).abs() < 2.0 && (mid[3] - 1.0).abs() < 0.01);
        // Effects change the drawing.
        let png = |fx: &str| shape_png("oval", accent, fx).unwrap().0;
        assert_ne!(png("Subtle Solids"), png("Top Shadow"));
        assert_ne!(png("Glow Edge"), png("Glossy"));
        for kind in SHAPES.split(", ") {
            let (data, w, h) = shape_png(kind, accent, "Office").unwrap();
            assert!(data.starts_with(b"\x89PNG") && w > 100.0 && h > 50.0, "{kind}");
        }
    }

    #[test]
    fn shape_colour_follows_the_theme() {
        let (mut s, id) = setup();
        s.run("compose.theme", &json!({"part": "colors", "name": "Red"})).unwrap();
        s.run("insert.shape", &json!({"kind": "rectangle"})).unwrap();
        s.run("compose.theme", &json!({"part": "colors", "name": "Green"})).unwrap();
        s.run("insert.shape", &json!({"kind": "rectangle"})).unwrap();
        let c = &s.composes[&id];
        assert_ne!(c.inline_images[0].data, c.inline_images[1].data);
        assert!(s.run("insert.shape", &json!({"kind": "rectangle", "color": "nope"})).is_err());
    }

    #[test]
    fn word_art_styles_parse_and_draw() {
        let theme = crate::compose::ThemeState::default().resolve();
        let w = parse_wordart("Fill: Orange, Accent color 2; Outline: Orange, Accent color 2", &theme);
        assert_eq!(w.fill, theme.color(crate::compose::ACCENT1 + 1));
        assert_eq!(w.outline, Some(theme.color(crate::compose::ACCENT1 + 1)));
        let w = parse_wordart("Fill: White; Outline: Plum, Accent color 5; Shadow", &theme);
        assert_eq!(w.fill, Color::WHITE);
        assert!(w.shadow);
        let w = parse_wordart(
            "Fill: Black, Text color 1; Outline: White, Background color 1; Hard Shadow: Plum, Accent color 5",
            &theme,
        );
        assert_eq!(w.fill, theme.color(0));
        assert_eq!(w.outline, Some(theme.color(1)));
        assert_eq!(w.hard_shadow, Some(theme.color(crate::compose::ACCENT1 + 4)));
        let w = parse_wordart("Gradient Fill: Plum, Accent color 5; Reflection", &theme);
        assert!(w.gradient.is_some() && w.reflection);
        assert!(parse_wordart("Fill: Dark Teal, Accent color 1; Soft Bevel", &theme).bevel);
        assert!(parse_wordart("Fill: Light Gray, Background color 2; Inner Shadow", &theme).inner_shadow);
        let w = parse_wordart(
            "Fill: White; Outline: Dark Teal, Accent color 1; Glow: Dark Teal, Accent color 1",
            &theme,
        );
        assert_eq!(w.glow, Some(theme.color(crate::compose::ACCENT1)));
        // Different styles give different pictures.
        let a = wordart_png("Hi", "Fill: Black, Text color 1; Shadow", &theme).unwrap();
        let b = wordart_png("Hi", "Gradient Fill: Plum, Accent color 5; Reflection", &theme).unwrap();
        assert_ne!(a.0, b.0);
        assert!(b.2 > a.2, "the reflection adds height");
        assert!(wordart_png("", "Fill: Black", &theme).is_none());
    }

    #[test]
    fn models_obj_and_stl() {
        let obj = b"v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nf 1 2 3\nf 1/1/1 2 4\nf -1 -2 -3\n";
        let (v, e) = parse_model(obj).unwrap();
        assert_eq!(v.len(), 4);
        assert_eq!(e.len(), 9);
        let stl = b"solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n";
        assert_eq!(parse_model(stl).unwrap().1.len(), 3);
        let mut bin = vec![0u8; 84];
        bin[80] = 1;
        bin.extend([0u8; 12]);
        for v in [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]] {
            for x in v {
                bin.extend(x.to_le_bytes());
            }
        }
        bin.extend([0u8; 2]);
        assert_eq!(parse_model(&bin).unwrap().0.len(), 3);
        assert!(parse_model(b"\x00\x01garbage").is_none());
        let svg = model_svg(&v, &e, 100.0);
        assert!(svg.starts_with("<svg") && svg.contains("<path"));
        let (mut s, id) = setup();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.obj");
        std::fs::write(&path, obj).unwrap();
        s.run("insert.model3d", &json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(s.composes[&id].inline_images.len(), 1);
    }

    #[test]
    fn tables_rules_textboxes() {
        let (mut s, id) = setup();
        assert_eq!(s.run("insert.table", &json!({})).unwrap()["dialog"], "insertTable");
        s.run("insert.table", &json!({"rows": 2, "cols": 3})).unwrap();
        assert!(
            s.composes[&id]
                .body
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Table { rows, .. } if rows.len() == 2 && rows[0].len() == 3))
        );
        assert!(s.run("insert.table", &json!({"rows": 2, "cols": 64})).is_err());
        assert_eq!(s.run("insert.table", &json!({"mode": "draw"})).unwrap()["dialog"], "drawTable");
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("compose.insertText", &json!({"text": "a\tb\nc\td"})).unwrap();
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("insert.table", &json!({"mode": "fromText"})).unwrap();
        assert!(
            matches!(&s.composes[&id].body.blocks[0], Block::Table { rows, .. } if rows.len() == 2 && rows[1].len() == 2)
        );
        s.run("insert.horizontalLine", &json!({})).unwrap();
        assert!(s.composes[&id].body.to_plain_text().contains("------"), "rule inside the cell");
        s.run("insert.textBox", &json!({"text": "boxed"})).unwrap();
        assert!(s.composes[&id].body.to_plain_text().contains("boxed"));
        assert_eq!(s.run("insert.textBox", &json!({"preset": "draw"})).unwrap()["dialog"], "drawTextBox");
    }

    #[test]
    fn links_bookmarks_symbols() {
        let (mut s, id) = setup();
        assert_eq!(s.run("insert.link", &json!({})).unwrap()["dialog"], "insertLink");
        s.run("insert.link", &json!({"url": "example.com", "text": " site"})).unwrap();
        let html = mailcraft_html::doc_to_html(&s.composes[&id].body);
        assert!(html.contains("href=\"http://example.com\""), "{html}");
        assert!(s.run("insert.link", &json!({"url": "javascript:alert(1)"})).is_err());
        assert_eq!(normalize_url("bob@example.com").as_deref(), Some("mailto:bob@example.com"));
        s.run("compose.select", &json!({"anchor": {"path": [0], "offset": 6}})).unwrap();
        s.run("insert.link", &json!({"url": ""})).unwrap();
        assert!(!mailcraft_html::doc_to_html(&s.composes[&id].body).contains("href"));
        s.run("insert.bookmark", &json!({"name": "top"})).unwrap();
        assert!(s.run("insert.bookmark", &json!({"name": "1bad"})).is_err());
        s.run("insert.bookmark", &json!({"name": "top", "action": "goTo"})).unwrap();
        assert_eq!(
            s.run("insert.bookmark", &json!({"name": "top", "action": "delete"})).unwrap()["deleted"],
            true
        );
        s.run("insert.symbol", &json!({"char": "U+00A9"})).unwrap();
        s.run("insert.symbol", &json!({"char": "€"})).unwrap();
        assert!(s.composes[&id].body.to_plain_text().contains("©€"));
        assert_eq!(s.run("insert.symbol", &json!({})).unwrap()["dialog"], "symbol");
        s.run("insert.equation", &json!({"preset": "pythagoreanTheorem"})).unwrap();
        assert!(s.composes[&id].body.to_plain_text().contains("a² + b² = c²"));
        assert_eq!(s.run("insert.equation", &json!({})).unwrap()["dialog"], "equation");
    }

    #[test]
    fn date_time_formats() {
        let t = DateTime::from_civil(2026, 10, 9, 15, 4, 5, 0).unwrap();
        assert_eq!(format_date(t, 0, "M/d/yyyy"), "10/9/2026");
        assert_eq!(format_date(t, 0, "dddd, MMMM dd, yyyy"), "Friday, October 09, 2026");
        assert_eq!(format_date(t, 0, "h:mm tt"), "3:04 PM");
        assert_eq!(format_date(t, 0, "HH:mm:ss 'at' yy"), "15:04:05 at 26");
        assert_eq!(format_date(t, 60, "H"), "16");
        let (mut s, id) = setup();
        s.run("insert.dateTime", &json!({"format": "yyyy-MM-dd"})).unwrap();
        assert!(s.composes[&id].body.to_plain_text().ends_with("2026-10-09"));
        assert_eq!(s.run("insert.dateTime", &json!({})).unwrap()["dialog"], "dateTime");
    }

    #[test]
    fn drop_cap_and_word_art() {
        let (mut s, id) = setup();
        s.run("insert.dropCap", &json!({"kind": "dropped"})).unwrap();
        let first = |s: &Session| match &s.composes[&id].body.blocks[0] {
            Block::Paragraph { inlines, .. } => match &inlines[0] {
                Inline::Text { style, text } => (text.clone(), style.size_pt),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        };
        assert_eq!(first(&s), ("s".to_owned(), Some(36.0)));
        s.run("insert.dropCap", &json!({"kind": "none"})).unwrap();
        assert_eq!(first(&s).1, None);
        let r = s
            .run(
                "insert.wordArt",
                &json!({"style": "Fill: Orange, Accent color 2; Outline: Orange, Accent color 2", "text": "Big"}),
            )
            .unwrap();
        assert_eq!(r["format"], "png");
        let c = &s.composes[&id];
        let img = c.inline_images.last().unwrap();
        assert_eq!(img.mime_type, "image/png");
        assert!(img.data.starts_with(b"\x89PNG"));
        assert_eq!(c.images().last().unwrap().1, "Big", "alt text is the WordArt text");
        // The selected text becomes the WordArt.
        s.run("compose.select", &json!({"all": true})).unwrap();
        let r = s.run("insert.wordArt", &json!({"style": "Fill: Black, Text color 1; Shadow"})).unwrap();
        assert!(r["text"].as_str().unwrap().contains("tart"));
    }

    #[test]
    fn building_blocks_roundtrip() {
        let (mut s, id) = setup();
        let other = open(&mut s);
        s.run("compose.select", &json!({"compose": id, "all": true})).unwrap();
        assert!(s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "nope"})).is_err());
        s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "quickParts", "name": "Greeting"}))
            .unwrap();
        s.run("insert.quickPart", &json!({"compose": other, "name": "greeting"})).unwrap();
        assert_eq!(s.composes[&other].body.to_plain_text(), "start");
        assert!(s.run("insert.quickPart", &json!({"name": "missing"})).is_err());
        assert_eq!(s.run("insert.quickPart", &json!({})).unwrap()["dialog"], "quickParts");
        s.run("compose.select", &json!({"compose": id, "all": true})).unwrap();
        s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "quickTables", "name": "T"}))
            .unwrap();
        s.run("insert.table", &json!({"compose": other, "mode": "quick", "name": "T"})).unwrap();
    }

    #[test]
    fn building_blocks_persist_in_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        {
            let (mut s, id) = setup();
            s.store = Some(mailcraft_store::Store::open(dir.path()).unwrap());
            s.run("compose.select", &json!({"compose": id, "all": true})).unwrap();
            let r = s
                .run("insert.saveToGallery", &json!({"compose": id, "gallery": "autoText", "name": "Sig"}))
                .unwrap();
            assert_eq!(r["persisted"], true);
            s.composes.clear();
        }
        assert!(dir.path().join(BUILDING_BLOCKS_FILE).exists());
        // A new session (all compose windows closed) still has the entry.
        let mut s = session();
        s.store = Some(mailcraft_store::Store::open(dir.path()).unwrap());
        let id = open(&mut s);
        s.run("insert.quickPart", &json!({"gallery": "autoText"})).unwrap();
        let d = last_dialog(&s);
        assert_eq!(d["entries"][0]["name"], "Sig");
        assert_eq!(d["entries"][0]["preview"], "start");
        s.run("insert.quickPart", &json!({"compose": id, "name": "sig", "gallery": "autoText"})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text(), "start");
        s.run("insert.quickPart", &json!({"name": "Sig", "action": "delete"})).unwrap();
        assert!(s.run("insert.quickPart", &json!({"name": "Sig"})).is_err());
        assert!(load_building_blocks(&s).is_empty());
        // A garbled file reads as empty instead of failing.
        std::fs::write(dir.path().join(BUILDING_BLOCKS_FILE), b"{not json").unwrap();
        assert!(load_building_blocks(&s).is_empty());
    }

    #[test]
    fn equation_presets_by_key_or_name_and_screenshot_files() {
        let (mut s, id) = setup();
        for name in
            ["Area of Circle", "area of circle", "areaOfCircle", "Pythagorean Theorem", "Euler's Identity"]
        {
            s.run("insert.equation", &json!({"preset": name})).unwrap();
        }
        assert!(s.composes[&id].body.to_plain_text().contains("A = \u{3c0}r\u{b2}"));
        s.run("insert.equation", &json!({})).unwrap();
        assert_eq!(last_dialog(&s)["presets"][0]["name"], "Area of Circle");
        // Screenshot inserts a saved picture.
        let r = s.run("insert.screenshot", &json!({"name": "shot.png", "data_base64": PNG_1X1})).unwrap();
        assert_eq!(r["screenshot"], true);
        assert_eq!(s.composes[&id].inline_images.len(), 1);
        s.run("insert.screenshot", &json!({"window": "Inbox"})).unwrap();
        assert!(last_dialog(&s)["message"].as_str().unwrap().contains("cannot capture"));
    }

    #[test]
    fn business_card_and_calendar_and_object() {
        let (mut s, id) = setup();
        let cid = s.mailbox.alloc_id();
        s.mailbox.contacts.insert(
            ItemId(cid),
            Contact {
                id: ItemId(cid),
                first_name: "Ann".into(),
                last_name: "Lee".into(),
                company: "Acme".into(),
                emails: vec!["ann@example.org".into()],
                ..Default::default()
            },
        );
        s.run("insert.businessCard", &json!({"contactId": cid.to_string()})).unwrap();
        let c = &s.composes[&id];
        assert!(c.attachments.iter().any(|a| a.filename == "Ann Lee.vcf"));
        assert!(c.body.to_plain_text().contains("Acme"));
        assert_eq!(s.run("insert.businessCard", &json!({})).unwrap()["dialog"], "insertBusinessCard");

        let cal = s.mailbox.folders.values().find(|f| f.kind == FolderKind::Calendar).map(|f| f.id).unwrap();
        let aid = s.mailbox.alloc_id();
        let st = DateTime::from_civil(2026, 10, 12, 9, 0, 0, 0).unwrap();
        s.mailbox.appointments.insert(
            ItemId(aid),
            Appointment {
                id: ItemId(aid),
                folder: cal,
                subject: "Standup".into(),
                location: "Room 1".into(),
                start: st,
                end: DateTime::from_unix(st.unix + 1800, 0),
                ..Default::default()
            },
        );
        let r = s
            .run("insert.calendar", &json!({"from": "2026-10-12", "to": "2026-10-13", "detail": "full"}))
            .unwrap();
        assert_eq!(r["appointments"], 1);
        let t = s.composes[&id].body.to_plain_text();
        assert!(t.contains("Standup (Room 1)") && t.contains("Monday, October 12, 2026"), "{t}");
        s.run("insert.calendar", &json!({"from": "2026-10-12", "detail": "availability"})).unwrap();
        assert!(s.composes[&id].body.to_plain_text().contains("9:00 AM \u{2013} 9:30 AM  Busy"));
        assert!(s.run("insert.calendar", &json!({"from": "2026-10-12", "to": "2026-10-01"})).is_err());
        assert_eq!(s.run("insert.calendar", &json!({})).unwrap()["dialog"], "insertCalendar");

        s.run("insert.object", &json!({"name": "a.txt", "data_base64": "aGVsbG8gb2JqZWN0"})).unwrap();
        assert!(s.composes[&id].body.to_plain_text().contains("hello object"));
        s.run("insert.object", &json!({"name": "a.bin", "data_base64": "AAEC"})).unwrap();
        assert!(s.composes[&id].attachments.iter().any(|a| a.filename == "a.bin"));
    }
}
