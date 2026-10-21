//! `insert.*`: tables, pictures and generated graphics, links, bookmarks, symbols, building blocks, cards and
//! calendars in the compose body.
//!
//! Shapes, text boxes, WordArt and drawing canvases are floating drawing objects of the body
//! ([`mailcraft_model::Drawing`], edited by `cmd::drawing`); [`drawing`] draws them and turns them into
//! pictures when the message is sent. Icons, charts, SmartArt, 3D models and object icons are inline PNG
//! pictures drawn with `mailcraft-render` (every mail program shows PNG; many do not show SVG); charts,
//! SmartArt and 3D models keep their recipe, so they can be edited again and follow theme changes. Commands
//! that need an OS facility (screen capture) or a picker without params open a dialog instead.
//!
//! Quick Parts / AutoText and the other building-block galleries are kept in `building-blocks.json` in the
//! profile directory (next to the data file), so they outlive the compose windows.

use std::fmt::Write as _;

pub mod chart;
pub mod drawing;
mod gltf;
pub mod icons;
#[cfg(test)]
mod link_tests;
mod math;
mod send_calendar;
pub mod shapes;
pub mod smartart;
pub mod symbol_fonts;
mod table_draw;

pub use math::{MAX_EQUATION, equation_png};
use send_calendar::calendar;
pub use send_calendar::{
    DETAILS as CALENDAR_DETAILS, LAYOUTS as CALENDAR_LAYOUTS, RANGES as CALENDAR_RANGES, calendar_folders,
};

use serde_json::{Value, json};

use super::compose::{
    add_bookmark, compose_id, compose_mut, file_param, has_body_selection, has_compose, mime_for, pos_json,
    rich_compose, safe_name, spec, vcard,
};
use crate::cmd::util::{dialog, opt_bool, opt_str, req_str};
use crate::compose::{
    BuildingBlock, ComposeAttachment, ComposeState, DocPos, InlineImage, empty_table, table_cell,
};
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_geom::Color;
use mailcraft_model::rich::{Align, TableCell};
use mailcraft_model::{Block, BusyStatus, DateTime, Doc, Inline, ItemId, ParaStyle, TextStyle};

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

fn picture(s: &mut Session, p: &Value) -> CmdResult {
    let Some((name, data)) = file_param(p, MAX_PICTURE_BYTES)? else {
        return dialog(s, "insertPicture", json!({"compose": compose_id(s, p)?}));
    };
    let Some(mime) = sniff_image(&data, &name) else {
        return Err(CmdError::Failed(format!("{name} is not a picture this editor can show")));
    };
    let size = display_size(image_size(&data));
    let given = opt_str(p, "alt").map(str::trim).filter(|a| !a.is_empty()).map(str::to_owned);
    // Options › Accessibility › "Automatically generate alt text for me" (from the file name, offline).
    let auto = given.is_none() && s.settings.bool("accessibility.autoAltText");
    let alt = given
        .or_else(|| auto.then(|| crate::cmd::review::auto_alt_text(&name)).flatten())
        .unwrap_or_default();
    let c = compose_mut(s, p)?;
    let cid = put_image(c, &name, mime, data, &alt, size);
    Ok(json!({"inserted": name, "cid": cid, "width": size.0, "height": size.1, "alt": alt}))
}

// ---------------------------------------------------------------------------------------------------------
// Shapes, icons, charts, SmartArt, 3D models
// ---------------------------------------------------------------------------------------------------------

/// A number param (`None` when absent or null).
fn num_param(p: &Value, k: &str) -> Result<Option<f32>, CmdError> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite())
            .map(|n| Some(n as f32))
            .ok_or_else(|| CmdError::Params(format!("`{k}` must be a number"))),
    }
}

/// `x`, `y` (where a drawing goes, px) when given.
fn point_param(p: &Value) -> Result<Option<(f32, f32)>, CmdError> {
    match (num_param(p, "x")?, num_param(p, "y")?) {
        (Some(x), Some(y)) => Ok(Some((x, y))),
        (None, None) => Ok(None),
        _ => Err(CmdError::Params("give both `x` and `y`".into())),
    }
}

/// `points`: [[x, y], …] as fractions of the box (hand-drawn freeforms and scribbles, line ends).
fn points_param(p: &Value) -> Result<Vec<(f32, f32)>, CmdError> {
    let Some(v) = p.get("points") else { return Ok(Vec::new()) };
    let arr = v.as_array().ok_or_else(|| CmdError::Params("`points` must be a list of [x, y]".into()))?;
    Ok(arr
        .iter()
        .take(drawing::MAX_POINTS)
        .filter_map(|pt| {
            let a = pt.as_array()?;
            let (x, y) = (a.first()?.as_f64()?, a.get(1)?.as_f64()?);
            (x.is_finite() && y.is_finite())
                .then_some(((x as f32).clamp(0.0, 1.0), (y as f32).clamp(0.0, 1.0)))
        })
        .collect())
}

/// Insert › Shapes: a floating shape object. With `draw` the shape becomes the drawing tool (the next drag
/// in the body draws it); with `x`, `y` (and `w`, `h`, `points`) it goes there; otherwise at the caret.
fn shape(s: &mut Session, p: &Value) -> CmdResult {
    let raw = req_str(p, "kind")?;
    let Some(kind) = shapes::canonical(raw) else {
        return Err(CmdError::Params(format!(
            "unknown shape `{raw}` (a gallery name such as `rectangle`, `oval`, `rightArrow`, or its label)"
        )));
    };
    if opt_bool(p, "draw") == Some(true) {
        let tool = if kind == "canvas" { "canvas".to_owned() } else { format!("shape:{kind}") };
        let c = compose_mut(s, p)?;
        c.draw_tool = Some(tool.clone());
        c.selected_drawing = None;
        return Ok(json!({"armed": tool}));
    }
    let at = point_param(p)?;
    let (w, h) = (num_param(p, "w")?, num_param(p, "h")?);
    let points = points_param(p)?;
    let cid = compose_id(s, p)?;
    let theme = s
        .composes
        .get(&cid)
        .map(ComposeState::theme_resolved)
        .ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let mut d = drawing::new_shape(&theme, kind);
    if let Some(col) = opt_str(p, "color") {
        let col = mailcraft_html::parse_css_color(col)
            .ok_or_else(|| CmdError::Params(format!("`{col}` is not a colour")))?;
        if shapes::is_line_shape(kind) {
            d.outline = Some(col);
        } else {
            d.fill = Some(col);
            d.outline = Some(darken(col, 0.35));
        }
    }
    if let Some(w) = w {
        d.w = w;
    }
    if let Some(h) = h {
        d.h = h;
    }
    d.points = points;
    let c = compose_mut(s, p)?;
    let id = drawing::add(c, d, at, true).map_err(CmdError::Failed)?;
    let d = c.body.drawings.iter().find(|d| d.id == id).cloned().unwrap_or_default();
    Ok(
        json!({"shape": kind, "drawing": id, "x": d.x, "y": d.y, "w": d.w, "h": d.h, "effects": theme.effects}),
    )
}

/// Records how picture `cid` was drawn, so a theme change can draw it again.
fn remember_generated(c: &mut ComposeState, cid: &str, kind: &str, spec: Value) {
    c.generated.retain(|g| g.content_id != cid);
    c.generated.push(crate::compose::GeneratedPicture {
        content_id: cid.to_owned(),
        kind: kind.to_owned(),
        spec,
    });
    trim_generated(c);
}

/// Keeps at most [`MAX_GENERATED`] recipes: recipes of pictures no longer in the body (kept for Undo) go
/// first, oldest first; then the oldest of the rest.
fn trim_generated(c: &mut ComposeState) {
    let mut extra = c.generated.len().saturating_sub(MAX_GENERATED);
    if extra == 0 {
        return;
    }
    let shown: std::collections::HashSet<String> = c
        .images()
        .into_iter()
        .filter_map(|(src, _)| src.strip_prefix("cid:").map(str::to_ascii_lowercase))
        .collect();
    c.generated.retain(|g| {
        if extra > 0 && !shown.contains(&g.content_id.to_ascii_lowercase()) {
            extra -= 1;
            return false;
        }
        true
    });
    c.generated.drain(..extra.min(c.generated.len()));
}

/// Most generated-picture recipes kept per message.
const MAX_GENERATED: usize = 200;

/// Draws generated picture `g` again with `theme`.
fn redraw(g: &crate::compose::GeneratedPicture, theme: &crate::compose::ResolvedTheme) -> Option<Vec<u8>> {
    let text = |k: &str| g.spec.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let png = match g.kind.as_str() {
        "shape" => {
            let color = g
                .spec
                .get("color")
                .and_then(Value::as_str)
                .and_then(mailcraft_html::parse_css_color)
                .unwrap_or_else(|| theme.color(crate::compose::ACCENT1));
            shape_png(&text("shape"), color, theme.effects)?.0
        }
        "smartArt" => {
            let items: Vec<String> = g
                .spec
                .get("items")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter().filter_map(Value::as_str).map(str::to_owned).take(smartart::MAX_ITEMS).collect()
                })
                .unwrap_or_default();
            smartart::png(&text("layout"), &items, theme)?.0
        }
        "wordArt" => wordart_png(&text("text"), &text("style"), theme)?.0,
        "chart" => chart::png(&serde_json::from_value::<chart::ChartSpec>(g.spec.clone()).ok()?, theme)?,
        "model3d" => model_recipe_png(&g.spec, theme)?,
        _ => return None,
    };
    (!png.is_empty()).then_some(png)
}

/// Points every `cid:old` picture in `blocks` at `cid:new`.
fn repoint_images(blocks: &mut [Block], old: &str, new: &str) {
    fn inlines(list: &mut [Inline], old: &str, new: &str) {
        for i in list {
            match i {
                Inline::Image { src, .. }
                    if src.strip_prefix("cid:").is_some_and(|c| c.eq_ignore_ascii_case(old)) =>
                {
                    *src = format!("cid:{new}");
                }
                Inline::Link { children, .. } => inlines(children, old, new),
                _ => {}
            }
        }
    }
    for b in blocks {
        match b {
            Block::Paragraph { inlines: l, .. } | Block::Heading { inlines: l, .. } => inlines(l, old, new),
            Block::List { items, .. } => items.iter_mut().for_each(|it| repoint_images(it, old, new)),
            Block::Quote { blocks } | Block::Container { blocks, .. } => repoint_images(blocks, old, new),
            Block::Table { rows, .. } => {
                rows.iter_mut().flatten().for_each(|cell| repoint_images(&mut cell.blocks, old, new))
            }
            Block::Pre { .. } | Block::Rule => {}
        }
    }
}

/// Redraws the shapes, SmartArt and WordArt of the body in the current theme (colours and effects). Each
/// redrawn picture gets a new inline image; the old image and its recipe stay, so after Undo of the change
/// (which brings back the old pictures and theme) a later theme change still redraws them. Returns how many
/// were redrawn.
pub(crate) fn redraw_generated(c: &mut ComposeState) -> usize {
    let theme = c.theme_resolved();
    let mut redrawn = 0;
    let shown: Vec<String> = c.images().into_iter().map(|(src, _)| src).collect();
    let recipes = c.generated.clone();
    for g in recipes {
        let Some(old) = c.inline_images.iter().find(|i| i.content_id == g.content_id).cloned() else {
            continue;
        };
        // Pictures no longer in the body keep their recipe (Undo can bring them back) but are not redrawn.
        let in_body = shown
            .iter()
            .any(|src| src.strip_prefix("cid:").is_some_and(|c| c.eq_ignore_ascii_case(&g.content_id)));
        let Some(png) = redraw(&g, &theme).filter(|png| in_body && *png != old.data) else {
            continue;
        };
        let cid = c.next_content_id();
        c.inline_images.push(InlineImage { content_id: cid.clone(), data: png, ..old });
        repoint_images(&mut c.body.blocks, &g.content_id, &cid);
        c.generated.push(crate::compose::GeneratedPicture { content_id: cid, ..g });
        redrawn += 1;
    }
    trim_generated(c);
    redrawn
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
    /// Soft edges: the fill fades out over this many logical px at its edge.
    soft: f32,
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
    let softened;
    let mask = if fx.soft > 0.0 {
        softened = mask.eroded(d(fx.soft / 2.0).max(1)).blurred(d(fx.soft / 2.0).max(1));
        &softened
    } else {
        mask
    };
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
    // One line per `\n`.
    let mut inlines = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            inlines.push(Inline::LineBreak);
        }
        if !line.is_empty() {
            inlines.push(Inline::Text { text: line.to_owned(), style: style.clone() });
        }
    }
    let doc =
        Doc { blocks: vec![Block::Paragraph { style: ParaStyle::default(), inlines }], ..Default::default() };
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
    /// Bevel: `Some(true)` sharp, `Some(false)` soft.
    bevel: Option<bool>,
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
        bevel: None,
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
            w.bevel = Some(h.contains("sharp"));
        } else if h.starts_with("reflection") {
            w.reflection = true;
        }
    }
    w
}

/// PNG of `text` as WordArt in `style`, set in the theme's heading font at 36 pt; (png, width, height).
fn wordart_png(
    text: &str,
    style: &str,
    theme: &crate::compose::ResolvedTheme,
) -> Option<(Vec<u8>, f32, f32)> {
    let (img, w, h) = wordart_img(text, style, theme, &TextStyle::default(), &WordArtOverrides::default())?;
    Some((img.to_png(), w, h))
}

/// `text` (lines split at `\n`) drawn as WordArt in `style` with [`EFFECT_MARGIN`] around the ink: the
/// picture and its size (logical px). `ts` overrides the font (default: the theme's heading font, bold,
/// 36 pt) and, with a colour, the style's fill.
/// What a WordArt drawing changes of its style: Text Outline and Text Effects.
#[derive(Clone, Debug, Default)]
struct WordArtOverrides {
    /// Outline colour of the letters (instead of the style's).
    outline: Option<Color>,
    /// `""` (the style's effects), `none`, `shadow`, `glow`, `reflection`, `bevel`.
    effect: String,
}

fn wordart_img(
    text: &str,
    style: &str,
    theme: &crate::compose::ResolvedTheme,
    ts_in: &TextStyle,
    over: &WordArtOverrides,
) -> Option<(Img, f32, f32)> {
    let fonts = mailcraft_fonts::FontDb::bundled();
    let size = ts_in.size_pt.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(36.0).clamp(4.0, 400.0);
    let mut ts = TextStyle {
        bold: Some(ts_in.bold.unwrap_or(true)),
        italic: ts_in.italic,
        underline: ts_in.underline,
        size_pt: Some(size),
        font_family: Some(ts_in.font_family.clone().unwrap_or_else(|| theme.heading_font.clone())),
        ..Default::default()
    };
    let (mut list, mut x0, mut y0, mut tw, mut th) = text_list(text, &ts, &fonts);
    let room = MAX_RASTER_SIDE - 2.0 * EFFECT_MARGIN;
    if tw > room || th > room {
        // Long text gets a smaller size rather than being cut off.
        ts.size_pt = Some((size * room / tw.max(th)).max(4.0));
        (list, x0, y0, tw, th) = text_list(text, &ts, &fonts);
    }
    if tw <= 0.0 || th <= 0.0 {
        return None;
    }
    let mut ws = parse_wordart(style, theme);
    if let Some(c) = ts_in.color {
        ws.fill = c;
        ws.gradient = ws.gradient.map(|_| lighten(c, 0.6));
    }
    if over.outline.is_some() {
        ws.outline = over.outline;
    }
    if !over.effect.is_empty() {
        // Text Effects replaces the style's effects with one.
        ws.shadow = over.effect == "shadow";
        ws.hard_shadow = None;
        ws.inner_shadow = false;
        ws.glow = (over.effect == "glow").then(|| theme.color(crate::compose::ACCENT1));
        ws.bevel = (over.effect == "bevel").then_some(false);
        ws.reflection = over.effect == "reflection";
    }
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
    if let Some(sharp) = ws.bevel {
        // A soft bevel shades a wide, faint edge; a sharp one a narrow, strong one.
        let (r, hi, lo) = if sharp { (0.6, 0.85, 0.6) } else { (1.4, 0.5, 0.35) };
        img.paint(&mask.minus(&mask.shifted(d(r), d(r))), hi, &|_, _| Color::WHITE);
        img.paint(&mask.minus(&mask.shifted(-d(r), -d(r))), lo, &|_, _| darken(fill, 0.5));
    }
    if ws.reflection {
        let r = mask.reflected(i64::try_from(y_bot).unwrap_or(0) + d(1.0), d(th * 0.6));
        img.paint(&r, 1.0, &|_, _| fill);
    }
    Some((img, w, h))
}

/// Insert › Icons: without `name` the Insert Icons dialog (every icon with its category); with it the icon
/// as a PNG `size` px square (default 48) at the caret.
fn icon(s: &mut Session, p: &Value) -> CmdResult {
    let Some(name) = opt_str(p, "name").map(str::to_owned) else {
        let all: Vec<Value> = icons::ICONS
            .iter()
            .map(|(id, label, cat, _)| json!({"id": id, "label": label, "category": cat}))
            .collect();
        return dialog(
            s,
            "icons",
            json!({"compose": compose_id(s, p)?, "icons": all, "categories": icons::CATEGORIES}),
        );
    };
    let Some((id, label, ..)) = icons::find(&name) else {
        return Err(CmdError::Params(format!(
            "unknown icon `{name}` (bundled: {})",
            icons::ICONS.iter().map(|i| i.0).collect::<Vec<_>>().join(", ")
        )));
    };
    let size = num_param(p, "size")?.unwrap_or(48.0).clamp(16.0, 512.0);
    let png = icons::png(id, size * RASTER_SCALE)
        .ok_or_else(|| CmdError::Failed("the icon could not be drawn".into()))?;
    let c = compose_mut(s, p)?;
    let cid = put_png(c, id, png, label, size, size)?;
    Ok(json!({"icon": id, "cid": cid, "format": "png", "size": size}))
}

/// The recipe of generated picture `cid` of `kind` in compose `c`, if any.
fn recipe(c: &ComposeState, cid: &str, kind: &str) -> Option<Value> {
    c.generated
        .iter()
        .rev()
        .find(|g| g.content_id.eq_ignore_ascii_case(cid) && g.kind == kind)
        .map(|g| g.spec.clone())
}

/// Replaces generated picture `old` in the body with a new one (`png`, `w`×`h` px, `alt`) drawn from
/// `spec`, as one undo step; the old picture and recipe stay for Undo. Returns the new content id.
fn replace_picture(
    c: &mut ComposeState,
    old: &str,
    name: &str,
    png: Vec<u8>,
    alt: &str,
    (w, h): (f32, f32),
    kind: &str,
    spec: Value,
) -> String {
    c.checkpoint();
    let cid = c.next_content_id();
    c.inline_images.push(InlineImage {
        content_id: cid.clone(),
        filename: format!("{name}.png"),
        mime_type: "image/png".into(),
        data: png,
    });
    retarget_images(&mut c.body.blocks, old, &cid, alt, w, h);
    c.dirty = true;
    remember_generated(c, &cid, kind, spec);
    cid
}

/// Points every `cid:old` picture in `blocks` at `cid:new` with a new alt text and size.
fn retarget_images(blocks: &mut [Block], old: &str, new: &str, alt_text: &str, w: f32, h: f32) {
    fn inlines(list: &mut [Inline], old: &str, new: &str, alt_text: &str, w: f32, h: f32) {
        for i in list {
            match i {
                Inline::Image { src, alt, width, height }
                    if src.strip_prefix("cid:").is_some_and(|c| c.eq_ignore_ascii_case(old)) =>
                {
                    *src = format!("cid:{new}");
                    *alt = alt_text.to_owned();
                    *width = Some(w);
                    *height = Some(h);
                }
                Inline::Link { children, .. } => inlines(children, old, new, alt_text, w, h),
                _ => {}
            }
        }
    }
    for b in blocks {
        match b {
            Block::Paragraph { inlines: l, .. } | Block::Heading { inlines: l, .. } => {
                inlines(l, old, new, alt_text, w, h)
            }
            Block::List { items, .. } => {
                items.iter_mut().for_each(|it| retarget_images(it, old, new, alt_text, w, h))
            }
            Block::Quote { blocks } | Block::Container { blocks, .. } => {
                retarget_images(blocks, old, new, alt_text, w, h)
            }
            Block::Table { rows, .. } => rows
                .iter_mut()
                .flatten()
                .for_each(|cell| retarget_images(&mut cell.blocks, old, new, alt_text, w, h)),
            Block::Pre { .. } | Block::Rule => {}
        }
    }
}

/// Whether picture `cid` is still in the body.
fn in_body(c: &ComposeState, cid: &str) -> bool {
    c.images().iter().any(|(src, _)| src.strip_prefix("cid:").is_some_and(|x| x.eq_ignore_ascii_case(cid)))
}

/// Insert › Chart. Without data the chart dialog (data sheet) opens; with `sheet`, `series` or `data` the
/// chart is drawn to PNG and inserted, keeping its recipe. `edit` names a chart picture already in the body:
/// without data its data opens in the dialog, with data the chart is redrawn in place.
fn chart(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let edit = opt_str(p, "edit").map(str::to_owned);
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let theme = c.theme_resolved();
    if let Some(e) = &edit
        && (recipe(c, e, "chart").is_none() || !in_body(c, e))
    {
        return Err(CmdError::Params(format!("`{e}` is not a chart in this message")));
    }
    let spec = chart::ChartSpec::from_params(p).map_err(CmdError::Params)?;
    let Some(spec) = spec else {
        let current = edit.as_deref().and_then(|e| recipe(c, e, "chart"));
        let kind = opt_str(p, "kind").and_then(chart::ChartSpec::kind_id).unwrap_or("column");
        let kinds: Vec<Value> =
            chart::KINDS.iter().map(|(id, label)| json!({"id": id, "label": label})).collect();
        return dialog(
            s,
            "insertChart",
            json!({"compose": cid, "kind": kind, "edit": edit, "chart": current, "kinds": kinds}),
        );
    };
    let png =
        chart::png(&spec, &theme).ok_or_else(|| CmdError::Params("the chart has nothing to show".into()))?;
    let alt = if spec.title.is_empty() { format!("{} chart", spec.label()) } else { spec.title.clone() };
    let recipe_value = serde_json::to_value(&spec).unwrap_or(Value::Null);
    let c = compose_mut(s, p)?;
    let size = (spec.width, spec.height);
    let pic = match &edit {
        Some(old) => replace_picture(c, old, "chart", png, &alt, size, "chart", recipe_value),
        None => {
            let pic = put_png(c, "chart", png, &alt, size.0, size.1)?;
            remember_generated(c, &pic, "chart", recipe_value);
            pic
        }
    };
    Ok(
        json!({"chart": spec.kind, "points": spec.categories.len(), "series": spec.series.len(), "cid": pic, "format": "png", "edited": edit.is_some()}),
    )
}

/// SmartArt items from `items` (strings; leading tabs or pairs of spaces give the level).
fn smart_items(p: &Value) -> Vec<String> {
    p.get("items")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|t| !t.trim().is_empty())
                .map(|t| {
                    let level = smartart::level_of(t);
                    format!("{}{}", "\t".repeat(level), t.trim().chars().take(80).collect::<String>())
                })
                .take(smartart::MAX_ITEMS)
                .collect()
        })
        .unwrap_or_default()
}

/// Insert › SmartArt: without a layout and items the SmartArt dialog (layout gallery and text pane); with
/// them the graphic as PNG. `edit` names a SmartArt picture of the body: its layout and text open in the
/// dialog, or with new ones it is redrawn in place.
fn smart_art(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let edit = opt_str(p, "edit").map(str::to_owned);
    let layout = opt_str(p, "layout").map(str::to_owned);
    let items = smart_items(p);
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    if let Some(e) = &edit
        && (recipe(c, e, "smartArt").is_none() || !in_body(c, e))
    {
        return Err(CmdError::Params(format!("`{e}` is not a SmartArt graphic in this message")));
    }
    let (Some(layout), false) = (layout, items.is_empty()) else {
        let current = edit.as_deref().and_then(|e| recipe(c, e, "smartArt"));
        let layouts: Vec<Value> = smartart::LAYOUTS
            .iter()
            .map(|(id, label, cat)| json!({"id": id, "label": label, "category": cat}))
            .collect();
        return dialog(
            s,
            "smartArt",
            json!({"compose": cid, "layouts": layouts, "edit": edit, "current": current}),
        );
    };
    let Some(layout) = smartart::layout_id(&layout) else {
        return Err(CmdError::Params(format!(
            "unknown layout `{layout}` ({})",
            smartart::LAYOUTS.iter().map(|l| l.0).collect::<Vec<_>>().join(", ")
        )));
    };
    let theme = c.theme_resolved();
    let Some((png, w, h)) = smartart::png(layout, &items, &theme) else {
        return Err(CmdError::Failed("the SmartArt graphic could not be drawn".into()));
    };
    let alt = items.iter().map(|i| i.trim()).collect::<Vec<_>>().join(", ");
    let spec = json!({"layout": layout, "items": items});
    let c = compose_mut(s, p)?;
    let pic = match &edit {
        Some(old) => replace_picture(c, old, "smartart", png, &alt, (w, h), "smartArt", spec),
        None => {
            let pic = put_png(c, "smartart", png, &alt, w, h)?;
            remember_generated(c, &pic, "smartArt", spec);
            pic
        }
    };
    Ok(json!({"layout": layout, "items": items.len(), "cid": pic, "format": "png", "edited": edit.is_some()}))
}

/// Opens the editor of a generated picture (a double-click on it in the body): the chart's data sheet,
/// the SmartArt text pane or the 3D model's view. An error for other pictures.
fn edit_picture(s: &mut Session, p: &Value) -> CmdResult {
    let pic = req_str(p, "cid")?.trim_start_matches("cid:").to_owned();
    let cid = compose_id(s, p)?;
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let kind = c
        .generated
        .iter()
        .rev()
        .find(|g| g.content_id.eq_ignore_ascii_case(&pic))
        .map(|g| g.kind.clone())
        .filter(|_| in_body(c, &pic))
        .ok_or_else(|| CmdError::Params("this picture cannot be edited".into()))?;
    let q = json!({"compose": cid, "edit": pic});
    match kind.as_str() {
        "chart" => chart(s, &q),
        "smartArt" => smart_art(s, &q),
        "model3d" => model3d(s, &q),
        _ => Err(CmdError::Params("this picture cannot be edited".into())),
    }
}

/// Parses OBJ, STL (text or binary), PLY (text) or glTF / GLB into (vertices, edges).
pub fn parse_model(data: &[u8]) -> Option<(Vec<[f64; 3]>, Vec<(usize, usize)>)> {
    if data.starts_with(b"glTF")
        || (data.trim_ascii_start().starts_with(b"{") && data.windows(7).any(|w| w == b"\"asset\""))
    {
        return gltf::parse(data);
    }
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
    } else if let Some(t) = text
        && t.starts_with("ply")
    {
        // PLY (text form): a header naming the vertex and face counts, then one vertex per line (x y z
        // first) and one face per line (count, then vertex indexes).
        let mut lines = t.lines();
        let (mut nv, mut nf) = (0usize, 0usize);
        for line in lines.by_ref() {
            let mut it = line.split_whitespace();
            match (it.next(), it.next(), it.next()) {
                (Some("format"), Some(f), _) if f != "ascii" => return None,
                (Some("element"), Some("vertex"), Some(n)) => nv = n.parse().ok()?,
                (Some("element"), Some("face"), Some(n)) => nf = n.parse().ok()?,
                (Some("end_header"), ..) => break,
                _ => {}
            }
        }
        for line in lines.by_ref().take(nv.min(MAX_VERTS)) {
            if let Some(v) = nums(&mut line.split_whitespace()) {
                verts.push(v);
            }
        }
        for line in lines.take(nf) {
            let idx: Vec<usize> = line
                .split_whitespace()
                .skip(1)
                .filter_map(|tok| tok.parse::<usize>().ok().filter(|k| *k < verts.len()))
                .collect();
            // Each side of the face (a two-index face is one edge, not two).
            let sides = if idx.len() > 2 { idx.len() } else { idx.len().saturating_sub(1) };
            for k in 0..sides {
                if let (Some(a), Some(b)) = (idx.get(k), idx.get((k + 1) % idx.len()))
                    && edges.len() < MAX_EDGES
                    && a != b
                {
                    edges.push((*a, *b));
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

/// Most model edges kept with a 3D model picture (so it can be turned later); more are sampled.
const MAX_MODEL_EDGES: usize = 3_000;

/// The model fitted into a unit cube around the origin, with at most [`MAX_MODEL_EDGES`] edges (sampled
/// evenly) and only the vertices they use.
fn model_recipe(verts: &[[f64; 3]], edges: &[(usize, usize)]) -> (Vec<[f64; 3]>, Vec<(usize, usize)>) {
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for v in verts {
        for ((l, h), x) in lo.iter_mut().zip(hi.iter_mut()).zip(v) {
            *l = l.min(*x);
            *h = h.max(*x);
        }
    }
    let span = lo.iter().zip(&hi).map(|(l, h)| h - l).fold(0.0f64, f64::max).max(f64::MIN_POSITIVE);
    let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
    let stride = edges.len().div_ceil(MAX_MODEL_EDGES).max(1);
    let mut map: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut out_v: Vec<[f64; 3]> = Vec::new();
    let mut out_e = Vec::new();
    let mut keep = |i: usize, out_v: &mut Vec<[f64; 3]>| -> Option<usize> {
        if let Some(k) = map.get(&i) {
            return Some(*k);
        }
        let v = verts.get(i)?;
        let round = |x: f64| (x * 1000.0).round() / 1000.0;
        out_v.push([
            round((v[0] - mid[0]) / span),
            round((v[1] - mid[1]) / span),
            round((v[2] - mid[2]) / span),
        ]);
        map.insert(i, out_v.len() - 1);
        Some(out_v.len() - 1)
    };
    for (a, b) in edges.iter().step_by(stride) {
        if let (Some(a), Some(b)) = (keep(*a, &mut out_v), keep(*b, &mut out_v)) {
            out_e.push((a, b));
        }
    }
    (out_v, out_e)
}

/// Wireframe of a model turned `yaw` degrees about the vertical axis and tilted `pitch` degrees towards the
/// viewer, drawn in `color` on white as a `size`-px square PNG.
pub fn model_png(
    verts: &[[f64; 3]],
    edges: &[(usize, usize)],
    size: f32,
    yaw: f32,
    pitch: f32,
    color: Color,
) -> Option<Vec<u8>> {
    let size = if size.is_finite() { size.clamp(64.0, 800.0) } else { 320.0 };
    let (ay, ax) = (f64::from(yaw).to_radians(), f64::from(pitch).to_radians());
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
    let margin = 8.0f64;
    let scale = (f64::from(size) - 2.0 * margin) / span;
    let (ox, oy) = (
        margin + (f64::from(size) - 2.0 * margin - (maxx - minx) * scale) / 2.0,
        margin + (f64::from(size) - 2.0 * margin - (maxy - miny) * scale) / 2.0,
    );
    let mut list = mailcraft_layout::DisplayList::default();
    let stride = (edges.len() / 20_000).max(1);
    for (a, b) in edges.iter().step_by(stride) {
        let (Some(pa), Some(pb)) = (proj.get(*a), proj.get(*b)) else {
            continue;
        };
        let pt = |p: &(f64, f64)| {
            mailcraft_geom::Point::new((ox + (p.0 - minx) * scale) as f32, (oy + (p.1 - miny) * scale) as f32)
        };
        list.items.push(mailcraft_layout::DisplayItem::Line { from: pt(pa), to: pt(pb), color, width: 0.8 });
    }
    let fonts = mailcraft_fonts::FontDb::bundled();
    let d = u32::try_from(dev(size)).ok()?;
    let png = mailcraft_render::render(&list, &fonts, d, d, RASTER_SCALE, Color::WHITE, &|_| None).to_png();
    (!png.is_empty()).then_some(png)
}

/// Draws a 3D model recipe (`verts`, `edges`, `yaw`, `pitch`, `size`) in the theme's first accent.
fn model_recipe_png(spec: &Value, theme: &crate::compose::ResolvedTheme) -> Option<Vec<u8>> {
    let verts: Vec<[f64; 3]> = serde_json::from_value(spec.get("verts")?.clone()).ok()?;
    let edges: Vec<(usize, usize)> = serde_json::from_value(spec.get("edges")?.clone()).ok()?;
    let f = |k: &str, d: f32| spec.get(k).and_then(Value::as_f64).map_or(d, |v| v as f32);
    model_png(
        &verts,
        &edges,
        f("size", 320.0),
        f("yaw", 35.0),
        f("pitch", 25.0),
        theme.color(crate::compose::ACCENT1),
    )
}

/// Insert › 3D Models: an OBJ, STL, PLY or glTF model as a wireframe picture (PNG) that can be turned later:
/// `edit` names a model picture of the body; without `yaw` / `pitch` its view opens in the 3D Model Views
/// dialog, with them it is drawn again turned that way.
fn model3d(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    if let Some(old) = opt_str(p, "edit").map(str::to_owned) {
        let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
        let mut spec = recipe(c, &old, "model3d")
            .filter(|_| in_body(c, &old))
            .ok_or_else(|| CmdError::Params(format!("`{old}` is not a 3D model in this message")))?;
        let (yaw, pitch) = (num_param(p, "yaw")?, num_param(p, "pitch")?);
        if yaw.is_none() && pitch.is_none() {
            let f = |k: &str, d: f64| spec.get(k).and_then(Value::as_f64).unwrap_or(d);
            return dialog(
                s,
                "model3dView",
                json!({"compose": cid, "edit": old, "yaw": f("yaw", 35.0), "pitch": f("pitch", 25.0)}),
            );
        }
        if let Some(o) = spec.as_object_mut() {
            if let Some(y) = yaw {
                o.insert("yaw".into(), json!(y.rem_euclid(360.0)));
            }
            if let Some(pt) = pitch {
                o.insert("pitch".into(), json!(pt.clamp(-90.0, 90.0)));
            }
        }
        let theme = c.theme_resolved();
        let alt = c
            .images()
            .into_iter()
            .find(|(src, _)| src.strip_prefix("cid:").is_some_and(|x| x.eq_ignore_ascii_case(&old)))
            .map(|(_, alt)| alt)
            .unwrap_or_default();
        let png = model_recipe_png(&spec, &theme)
            .ok_or_else(|| CmdError::Failed("the model could not be drawn".into()))?;
        let size = spec.get("size").and_then(Value::as_f64).unwrap_or(320.0) as f32;
        let c = compose_mut(s, p)?;
        let pic = replace_picture(c, &old, "model", png, &alt, (size, size), "model3d", spec);
        return Ok(json!({"cid": pic, "edited": true}));
    }
    let Some((name, data)) = file_param(p, MAX_MODEL_BYTES)? else {
        return dialog(s, "insert3dModel", json!({"compose": cid}));
    };
    let Some((verts, edges)) = parse_model(&data) else {
        return Err(CmdError::Failed(format!("{name}: only OBJ, STL, PLY and glTF models can be shown")));
    };
    let (rv, re) = model_recipe(&verts, &edges);
    let spec = json!({"verts": rv, "edges": re, "yaw": 35.0, "pitch": 25.0, "size": 320.0});
    let theme = s
        .composes
        .get(&cid)
        .map(ComposeState::theme_resolved)
        .ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let png = model_recipe_png(&spec, &theme)
        .ok_or_else(|| CmdError::Failed("the model could not be drawn".into()))?;
    let c = compose_mut(s, p)?;
    let pic = put_png(c, "model", png, &name, 320.0, 320.0)?;
    remember_generated(c, &pic, "model3d", spec);
    Ok(json!({"inserted": name, "vertices": verts.len(), "edges": edges.len(), "cid": pic, "format": "png"}))
}

/// What Screenshot offers when nothing can capture the screen (the engine cannot; the desktop app captures
/// through its platform backend and passes the picture in).
pub const SCREENSHOT_UNAVAILABLE: &str = "Take the screenshot with your system's screenshot tool, then choose \
the saved picture to insert it.";

/// Screenshot: inserts a captured or saved screenshot (`name` + `data_base64` from the app's capture
/// backend, or a picture file `path`; `window` is only reported back, never put in the alt text). Without
/// a picture a dialog offers a saved file instead, with the reason capture failed (`unavailable`) when known.
fn screenshot(s: &mut Session, p: &Value) -> CmdResult {
    let window = opt_str(p, "window").map(|w| w.trim().chars().take(120).collect::<String>());
    if p.get("path").is_some() || p.get("data_base64").is_some() {
        let mut q = p.clone();
        // The alt text goes to the recipients but is not seen by the sender, and window titles often carry
        // addresses, paths or other documents' names: a neutral alt text unless one is given. The window
        // stays in the result only.
        if opt_str(p, "alt").is_none_or(|a| a.trim().is_empty())
            && let Some(o) = q.as_object_mut()
        {
            o.insert("alt".into(), json!("Screenshot"));
        }
        let mut r = picture(s, &q)?;
        if let Some(o) = r.as_object_mut() {
            o.insert("screenshot".into(), json!(true));
            o.insert("window".into(), json!(window));
        }
        return Ok(r);
    }
    let clip = opt_bool(p, "clip").unwrap_or(false);
    let message = match opt_str(p, "unavailable").map(str::trim).filter(|r| !r.is_empty()) {
        Some(why) => format!("{why} {SCREENSHOT_UNAVAILABLE}"),
        // Insert a Saved Screenshot…: nothing failed, just pick the file.
        None if !clip && window.is_none() => "Choose a screenshot saved as a picture file.".to_owned(),
        None => format!("Screen capture is not available here. {SCREENSHOT_UNAVAILABLE}"),
    };
    dialog(
        s,
        "screenshot",
        json!({"compose": compose_id(s, p)?, "window": window, "clip": clip, "capture": false, "message": message}),
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
        "draw" => return draw_table(s, p),
        "fromText" => return convert_text_to_table(s, p),
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

/// Draw Table: the drawing pad (dialog) sends the column widths it drew (`widths`, px) and the number of
/// rows; this makes that table, each column as wide as drawn.
fn draw_table(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(action) = opt_str(p, "action") {
        // The table pencil and eraser in the body (see `table_draw`).
        let erase = match action {
            "stroke" => false,
            "erase" => true,
            other => {
                return Err(CmdError::Params(format!("unknown Draw Table action `{other}` (stroke, erase)")));
            }
        };
        let path: Vec<usize> = p
            .get("path")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter().filter_map(Value::as_u64).filter_map(|n| usize::try_from(n).ok()).take(64).collect()
            })
            .ok_or_else(|| CmdError::Params("`path` (where the stroke started) required".into()))?;
        let offset =
            p.get("offset").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
        let (dx, dy) = (num_param(p, "dx")?.unwrap_or(0.0), num_param(p, "dy")?.unwrap_or(0.0));
        let c = compose_mut(s, p)?;
        let done = table_draw::stroke(c, DocPos::new(path, offset), dx, dy, erase);
        let what = match done {
            table_draw::Drawn::NewTable => "newTable",
            table_draw::Drawn::Split => "split",
            table_draw::Drawn::Merged => "merged",
            table_draw::Drawn::Nothing => "nothing",
        };
        return Ok(json!({"drawn": what, "caret": caret(c)}));
    }
    let Some(widths) = p.get("widths") else {
        if opt_bool(p, "pad") == Some(true) {
            return dialog(s, "drawTable", json!({"compose": compose_id(s, p)?}));
        }
        // The pencil: strokes in the body draw tables and split cells (Esc puts it away).
        let c = compose_mut(s, p)?;
        c.draw_tool = Some("tablePencil".into());
        c.selected_drawing = None;
        return Ok(json!({"armed": "tablePencil"}));
    };
    let widths: Vec<f32> = widths
        .as_array()
        .ok_or_else(|| CmdError::Params("`widths` must be a list of column widths".into()))?
        .iter()
        .map(|w| w.as_f64().filter(|w| w.is_finite() && *w > 0.0).map(|w| (w as f32).clamp(8.0, 2000.0)))
        .collect::<Option<Vec<f32>>>()
        .ok_or_else(|| CmdError::Params("column widths must be positive numbers".into()))?;
    let rows = count_param(p, "rows")?.unwrap_or(1);
    if widths.is_empty() || widths.len() > MAX_TABLE_COLS || rows > MAX_TABLE_ROWS {
        return Err(CmdError::Params(format!(
            "a drawn table has 1 to {MAX_TABLE_COLS} columns and at most {MAX_TABLE_ROWS} rows"
        )));
    }
    let total: f32 = widths.iter().sum();
    let table = Block::Table {
        borders: None,
        rows: (0..rows)
            .map(|_| {
                widths.iter().map(|w| TableCell { width_px: Some(w.round()), ..table_cell("") }).collect()
            })
            .collect(),
        border: true,
        width_px: Some(total.round()),
    };
    let c = compose_mut(s, p)?;
    c.checkpoint();
    c.insert_blocks(vec![table]);
    Ok(json!({"rows": rows, "cols": widths.len(), "drawn": true, "caret": caret(c)}))
}

/// Convert Text to Table: without `separator` a dialog proposes one (tabs when the paragraphs have any,
/// else commas) and the column count; with it the selected paragraphs become a table.
fn convert_text_to_table(s: &mut Session, p: &Value) -> CmdResult {
    use crate::compose::TextTableSplit;
    let cid = compose_id(s, p)?;
    let lines = s
        .composes
        .get(&cid)
        .and_then(ComposeState::text_table_lines)
        .ok_or_else(|| CmdError::Failed("select the paragraphs to convert first".into()))?;
    let cols = count_param(p, "cols")?;
    let split = match opt_str(p, "separator") {
        None => {
            let tabs = lines.iter().any(|l| l.contains('\t'));
            let sep = if tabs { '\t' } else { ',' };
            let found = lines.iter().map(|l| l.split(sep).count()).max().unwrap_or(1);
            return dialog(
                s,
                "convertTextToTable",
                json!({"compose": cid, "separator": if tabs { "tabs" } else { "commas" }, "cols": found, "rows": lines.len()}),
            );
        }
        Some("tabs") => TextTableSplit::At('\t'),
        Some("commas") => TextTableSplit::At(','),
        Some("semicolons") => TextTableSplit::At(';'),
        Some("paragraphs") => TextTableSplit::Paragraphs,
        Some("auto") => TextTableSplit::Auto,
        Some(other) => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_control() => TextTableSplit::At(c),
                _ => {
                    return Err(CmdError::Params(format!(
                        "unknown separator `{other}` (tabs, commas, semicolons, paragraphs or one character)"
                    )));
                }
            }
        }
    };
    if cols.is_some_and(|n| n > crate::compose::MAX_TEXT_TABLE_COLS) {
        return Err(CmdError::Params(format!("at most {} columns", crate::compose::MAX_TEXT_TABLE_COLS)));
    }
    let c = compose_mut(s, p)?;
    c.checkpoint();
    if !c.text_to_table_with(split, cols) {
        c.undo_edit();
        c.edit_redo.clear();
        return Err(CmdError::Failed("select the paragraphs to convert first".into()));
    }
    Ok(json!({"converted": true, "caret": caret(c)}))
}

fn horizontal_line(s: &mut Session, p: &Value) -> CmdResult {
    edit(s, p, |c| c.insert_blocks(vec![Block::Rule]))?;
    Ok(json!({"inserted": "rule"}))
}

/// Insert › Text Box: a floating text box. `simple` and `sidebar` are the built-in boxes; `draw` arms the
/// text box tool (the next drag in the body draws one) unless `x`, `y` (and `w`, `h`) say where it goes;
/// `saved:<name>` inserts a box saved to the Text Box gallery.
fn text_box(s: &mut Session, p: &Value) -> CmdResult {
    let preset = opt_str(p, "preset").unwrap_or("simple");
    if let Some(name) = preset.strip_prefix("saved:") {
        return insert_building_block(s, p, "textBoxes", name);
    }
    if !matches!(preset, "simple" | "sidebar" | "draw") {
        return Err(CmdError::Params(format!(
            "unknown text box `{preset}` (simple, sidebar, draw, saved:<name>)"
        )));
    }
    let at = point_param(p)?;
    let (w, h) = (num_param(p, "w")?, num_param(p, "h")?);
    let vertical = opt_bool(p, "vertical") == Some(true);
    if preset == "draw" && at.is_none() {
        let tool = if vertical { "verticalTextBox" } else { "textBox" };
        let c = compose_mut(s, p)?;
        c.draw_tool = Some(tool.into());
        c.selected_drawing = None;
        return Ok(json!({"armed": tool}));
    }
    let text = opt_str(p, "text").unwrap_or("").replace("\r\n", "\n");
    let cid = compose_id(s, p)?;
    let theme = s
        .composes
        .get(&cid)
        .map(ComposeState::theme_resolved)
        .ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let mut d = drawing::new_text_box(&theme, &text);
    d.vertical = vertical;
    if preset == "sidebar" {
        // A shaded, borderless side panel in a light tint of the accent colour, its text in the accent.
        let accent = theme.color(crate::compose::ACCENT1);
        d.fill = Some(lighten(accent, 0.85));
        d.outline = None;
        d.text_style = TextStyle { italic: Some(true), color: Some(accent), ..Default::default() };
        d.w = 200.0;
        d.h = 240.0;
        if d.text.is_empty() {
            d.text = "Sidebar".into();
        }
    }
    if let Some(w) = w {
        d.w = w;
    }
    if let Some(h) = h {
        d.h = h;
    }
    let c = compose_mut(s, p)?;
    let id = drawing::add(c, d, at, true).map_err(CmdError::Failed)?;
    let d = c.body.drawings.iter().find(|d| d.id == id).cloned().unwrap_or_default();
    // The editor puts the caret in a new box.
    Ok(json!({"textBox": preset, "drawing": id, "editText": true, "x": d.x, "y": d.y, "w": d.w, "h": d.h}))
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

/// `file:` URL of a local path (percent-encoding what a URL cannot hold), for links to recent files.
pub fn file_url(path: &str) -> String {
    let mut out = String::from("file://");
    let path = path.replace('\\', "/");
    if !path.starts_with('/') {
        out.push('/');
    }
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(char::from(b))
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Percent-encodes a `mailto:` header value (RFC 6068: everything but unreserved characters).
fn mailto_encode(v: &str) -> String {
    let mut out = String::new();
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(char::from(b)),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Decodes `%XX` escapes (invalid ones are kept as they are).
fn percent_decode(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes.get(i).copied().unwrap_or(0);
        if b == b'%'
            && let Some(h) = bytes.get(i.saturating_add(1)..i.saturating_add(3))
            && let Ok(n) = u8::from_str_radix(std::str::from_utf8(h).unwrap_or("x"), 16)
        {
            out.push(n);
            i = i.saturating_add(3);
            continue;
        }
        out.push(b);
        i = i.saturating_add(1);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `mailto:` link to `address` with an optional subject.
pub fn mailto_url(address: &str, subject: &str) -> Option<String> {
    let a = address.trim().trim_start_matches("mailto:").trim();
    if a.is_empty() || a.chars().any(|c| c.is_control() || c.is_whitespace()) || !a.contains('@') {
        return None;
    }
    let subject = subject.trim();
    Some(if subject.is_empty() {
        format!("mailto:{a}")
    } else {
        format!("mailto:{a}?subject={}", mailto_encode(subject))
    })
}

/// The address and subject of a `mailto:` link.
fn mailto_parts(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("mailto:")?;
    let (addr, query) = rest.split_once('?').unwrap_or((rest, ""));
    let subject = query
        .split('&')
        .find_map(|kv| kv.split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case("subject")).map(|(_, v)| v))
        .map(percent_decode)
        .unwrap_or_default();
    Some((percent_decode(addr), subject))
}

/// A hidden bookmark name for a heading (`_` + its words joined by `_`), unique in `c` unless it already
/// marks `pos`.
fn heading_bookmark_name(c: &ComposeState, text: &str, pos: &DocPos) -> String {
    let words: Vec<String> =
        text.split(|ch: char| !ch.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_owned).collect();
    let mut base: String = format!("_{}", words.join("_")).chars().take(40).collect();
    if base == "_" {
        base = "_Heading".into();
    }
    let free = |n: &str| c.bookmarks.iter().all(|b| b.name != n || b.pos == *pos);
    if free(&base) {
        return base;
    }
    (2..1000).map(|k| format!("{base}_{k}")).find(|n| free(n)).unwrap_or(base)
}

/// Insert Hyperlink's other targets: a bookmark (`bookmark`), a heading (`heading`: 1-based, marked with a
/// hidden bookmark), the top of the message (`top`) or an e-mail address (`email` + `subject`). `Ok(None)`
/// when none is given.
fn link_target(s: &mut Session, p: &Value) -> Result<Option<String>, CmdError> {
    if let Some(name) = opt_str(p, "bookmark").map(str::trim).filter(|n| !n.is_empty()) {
        let c = compose_mut(s, p)?;
        if !c.bookmarks.iter().any(|b| b.name == name) {
            return Err(CmdError::Params(format!("no bookmark `{name}`")));
        }
        return Ok(Some(format!("#{name}")));
    }
    if let Some(v) = p.get("heading") {
        let n = v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .and_then(|n| n.checked_sub(1))
            .ok_or_else(|| CmdError::Params("`heading` is the heading's number, from 1".into()))?;
        let c = compose_mut(s, p)?;
        let (path, _, text) = c
            .headings()
            .into_iter()
            .nth(n)
            .ok_or_else(|| CmdError::Params(format!("no heading {}", n + 1)))?;
        let pos = DocPos::new(path, 0);
        let name = heading_bookmark_name(c, &text, &pos);
        super::compose::add_bookmark_at(c, &name, pos);
        return Ok(Some(format!("#{name}")));
    }
    if opt_bool(p, "top") == Some(true) {
        let c = compose_mut(s, p)?;
        let start = c.body_start();
        super::compose::add_bookmark_at(c, "_top", start);
        return Ok(Some("#_top".into()));
    }
    if let Some(addr) = opt_str(p, "email") {
        let subject = opt_str(p, "subject").unwrap_or("");
        return mailto_url(addr, subject)
            .map(Some)
            .ok_or_else(|| CmdError::Params(format!("`{addr}` is not an e-mail address")));
    }
    Ok(None)
}

/// What the Insert Hyperlink dialog opens with: the text, the link and ScreenTip at the caret, and the
/// places it can link to in this message.
fn link_dialog_state(c: &ComposeState, text: Option<String>) -> Value {
    let url = c.link_at_caret();
    let (email, subject) = url.as_deref().and_then(mailto_parts).unwrap_or_default();
    let bookmarks: Vec<&str> =
        c.bookmarks.iter().map(|b| b.name.as_str()).filter(|n| !n.starts_with('_')).collect();
    let headings: Vec<Value> = c
        .headings()
        .into_iter()
        .enumerate()
        .map(|(i, (_, level, text))| json!({"index": i + 1, "level": level, "text": text}))
        .collect();
    let mode = match url.as_deref() {
        Some(u) if u.starts_with("mailto:") => "email",
        Some(u) if u.starts_with('#') => "place",
        _ => "web",
    };
    json!({
        "compose": c.id,
        "text": text.unwrap_or_else(|| c.selected_text()),
        "url": url,
        "screenTip": c.link_tip_at_caret(),
        "mode": mode,
        "email": email,
        "subject": subject,
        "bookmarks": bookmarks,
        "headings": headings,
    })
}

/// A recent "file" that is a web address (a document on a web site or in cloud storage), which the people
/// the message goes to can open: returned as the link address. Files on this computer or a network share
/// are not links (`file:` links are dropped from sent HTML: they would not open for the recipients).
pub fn web_location(path: &str) -> Option<String> {
    let t = path.trim();
    let lower = t.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")).then(|| t.replace(' ', "%20"))
}

fn link(s: &mut Session, p: &Value) -> CmdResult {
    // Link ▾ › a recent file: a document on the web is linked (its name as the text); a file on this
    // computer or a share is attached instead (a link to it would not open for the people the message goes
    // to).
    if let Some(path) = opt_str(p, "path").map(str::trim).filter(|t| !t.is_empty()) {
        if let Some(href) = web_location(path) {
            let href = normalize_url(&href)
                .ok_or_else(|| CmdError::Params(format!("`{path}` is not a usable link address")))?;
            let text = path.rsplit(['/', '\\']).find(|x| !x.is_empty()).unwrap_or(path).to_owned();
            let c = compose_mut(s, p)?;
            c.checkpoint();
            let text = if c.selection.is_collapsed() { Some(text) } else { None };
            c.set_link(Some(&href), text.as_deref());
            let caret = caret(c);
            super::compose::remember_file(s, path);
            return Ok(json!({"url": href, "linked": true, "caret": caret}));
        }
        let (name, data) = super::compose::read_recent_file(path, super::compose::MAX_ATTACHMENT_BYTES)?;
        let c = compose_mut(s, p)?;
        if c.attachments.len() >= super::compose::MAX_ATTACHMENTS {
            return Err(CmdError::Failed("too many attachments".into()));
        }
        let i = c.attach_bytes(&name, None, data).map_err(CmdError::Failed)?;
        let name = c.attachments.get(i).map(|a| a.filename.clone()).unwrap_or(name);
        let count = c.attachments.len();
        super::compose::remember_file(s, path);
        return Ok(json!({"attached": name, "index": i, "count": count}));
    }
    let text = opt_str(p, "text").map(str::to_owned);
    let tip = opt_str(p, "screenTip").map(str::to_owned);
    let target = link_target(s, p)?;
    let url = target.as_deref().or_else(|| opt_str(p, "url")).map(str::to_owned);
    let Some(url) = url else {
        let cid = compose_id(s, p)?;
        let state = s.composes.get(&cid).map(|c| link_dialog_state(c, text)).unwrap_or(Value::Null);
        return dialog(s, "insertLink", state);
    };
    let href = if url.trim().is_empty() {
        None
    } else {
        Some(
            normalize_url(&url)
                .ok_or_else(|| CmdError::Params(format!("`{url}` is not a usable link address")))?,
        )
    };
    if tip.as_deref().is_some_and(|t| t.chars().count() > MAX_SCREEN_TIP) {
        return Err(CmdError::Params(format!("a ScreenTip has at most {MAX_SCREEN_TIP} characters")));
    }
    let c = compose_mut(s, p)?;
    c.checkpoint();
    if p.get("screenTip").is_some() {
        c.set_link_with_tip(href.as_deref(), text.as_deref(), tip.as_deref());
    } else {
        c.set_link(href.as_deref(), text.as_deref());
    }
    let out = json!({"url": href, "screenTip": c.link_tip_at_caret(), "caret": caret(c)});
    // A web address linked joins Link ▾'s recent items.
    if let Some(h) = href.as_deref().filter(|h| web_location(h).is_some()) {
        super::compose::remember_file(s, h);
    }
    Ok(out)
}

/// Longest ScreenTip accepted (characters).
const MAX_SCREEN_TIP: usize = 255;

fn valid_bookmark(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(char::is_alphabetic)
        && name.chars().count() <= 40
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn bookmark(s: &mut Session, p: &Value) -> CmdResult {
    let Some(name) = opt_str(p, "name").map(str::to_owned) else {
        let cid = compose_id(s, p)?;
        // Hidden bookmarks (`_…`, made for links to headings and the top) are not listed.
        let names: Vec<String> = s
            .composes
            .get(&cid)
            .map(|c| c.bookmarks.iter().map(|b| b.name.clone()).filter(|n| !n.starts_with('_')).collect())
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
    // A symbol font's character by its code ("Wingdings: 74"), as the Unicode character it stands for.
    if let Some(font) = opt_str(p, "font").filter(|f| !f.trim().is_empty() && *f != "(normal text)") {
        let code = p
            .get("code")
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())))
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| CmdError::Params("`code` is the character code (0-255)".into()))?;
        let ch = symbol_fonts::map(font, code).ok_or_else(|| {
            CmdError::Params(format!("{font} has no character at {code} that MailCraft can insert"))
        })?;
        let text = ch.to_string();
        edit(s, p, |c| c.insert_text(&text))?;
        return Ok(json!({"inserted": text, "font": font, "code": code}));
    }
    let Some(ch) = opt_str(p, "char") else {
        return dialog(
            s,
            "symbol",
            json!({"compose": compose_id(s, p)?, "fonts": symbol_fonts::SYMBOL_FONTS}),
        );
    };
    let text = symbol_text(ch).ok_or_else(|| CmdError::Params(format!("`{ch}` is not a symbol")))?;
    edit(s, p, |c| c.insert_text(&text))?;
    Ok(json!({"inserted": text}))
}

/// Built-in equations: key, gallery name, formula in plain math notation.
pub const EQUATION_PRESETS: [(&str, &str, &str); 6] = [
    ("areaOfCircle", "Area of Circle", "A=\u{3c0}r^2"),
    (
        "binomialTheorem",
        "Binomial Theorem",
        "(x+a)^n=\u{2211}_(k=0)^n (n!/(k!(n\u{2212}k)!))x^k a^(n\u{2212}k)",
    ),
    ("expansionOfSum", "Expansion of a Sum", "(1+x)^n=1+(nx)/(1!)+(n(n\u{2212}1)x^2)/(2!)+\u{2026}"),
    ("pythagoreanTheorem", "Pythagorean Theorem", "a^2+b^2=c^2"),
    ("quadraticFormula", "Quadratic Formula", "x=(\u{2212}b\u{b1}\u{221a}(b^2\u{2212}4ac))/(2a)"),
    ("eulerIdentity", "Euler's Identity", "e^(i\u{3c0})+1=0"),
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
    if text.chars().count() > MAX_EQUATION {
        return Err(CmdError::Params(format!("an equation has at most {MAX_EQUATION} characters")));
    }
    // Drawn in two dimensions as a picture (alt text = the linear form); text that cannot be drawn goes in
    // as a centred math-font paragraph.
    if let Some((png, w, h)) = equation_png(&text) {
        let c = compose_mut(s, p)?;
        let cid = put_png(c, "equation", png, &text, w, h)?;
        return Ok(json!({"equation": text, "cid": cid, "format": "png"}));
    }
    edit(s, p, |c| c.insert_blocks(vec![equation_block(&text)]))?;
    Ok(json!({"equation": text, "format": "text"}))
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

/// Parsed building-block file of the profile (galleries are listed every frame while a menu or the organizer
/// is open).
static BLOCKS_CACHE: crate::cmd::util::FileCache<Vec<BuildingBlock>> = crate::cmd::util::FileCache::new();

/// Saved building blocks from the profile (empty without a profile, or when the file is missing or garbled).
pub fn load_building_blocks(s: &Session) -> Vec<BuildingBlock> {
    profile_blocks(s).as_ref().clone()
}

/// [`load_building_blocks`], shared with the cache (parsed again only when the file changed).
fn profile_blocks(s: &Session) -> std::sync::Arc<Vec<BuildingBlock>> {
    let Some(path) = blocks_path(s) else {
        return std::sync::Arc::default();
    };
    BLOCKS_CACHE.get(&path, read_building_blocks)
}

fn read_building_blocks(path: &std::path::Path) -> Vec<BuildingBlock> {
    let too_big = std::fs::metadata(path).map(|m| m.len() > MAX_BUILDING_BLOCKS_BYTES).unwrap_or(true);
    if too_big {
        return Vec::new();
    }
    std::fs::read(path)
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
    let written = std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| CmdError::Failed(format!("the building blocks could not be saved: {e}")));
    BLOCKS_CACHE.forget(&path);
    written
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
    merged_blocks(s, cid, |_| true)
}

/// The profile's building blocks that pass `keep`, then those only on open composes (newer entries of the
/// same gallery and name win). Only the kept entries are copied.
fn merged_blocks(s: &Session, cid: u64, keep: impl Fn(&BuildingBlock) -> bool) -> Vec<BuildingBlock> {
    let mut out: Vec<BuildingBlock> = builtin_auto_text(s).into_iter().filter(|b| keep(b)).collect();
    for b in profile_blocks(s).iter().filter(|b| keep(b)) {
        out.retain(|x| !(x.gallery == b.gallery && x.name.eq_ignore_ascii_case(&b.name)));
        out.push(b.clone());
    }
    let own = s.composes.get(&cid).into_iter();
    let others = s.composes.iter().filter(|(k, _)| **k != cid).map(|(_, c)| c);
    for b in others.chain(own).flat_map(|c| c.building_blocks.iter()).filter(|b| keep(b)) {
        out.retain(|x| !(x.gallery == b.gallery && x.name.eq_ignore_ascii_case(&b.name)));
        out.push(b.clone());
    }
    out
}

/// The user's name: General › User name, else the default account's sender name.
pub fn user_name(s: &Session) -> String {
    let name = s.settings.text("general.userName");
    if !name.trim().is_empty() {
        return name.trim().to_owned();
    }
    s.mailbox
        .accounts
        .iter()
        .find(|a| a.is_default)
        .or_else(|| s.mailbox.accounts.first())
        .map(|a| a.sender_name.trim().to_owned())
        .unwrap_or_default()
}

/// The user's initials: General › Initials, else the first letter of each word of [`user_name`].
pub fn user_initials(s: &Session) -> String {
    let initials = s.settings.text("general.initials");
    if !initials.trim().is_empty() {
        return initials.trim().to_owned();
    }
    user_name(s)
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .flat_map(char::to_uppercase)
        .take(8)
        .collect()
}

/// Setting that lists the built-in AutoText entries the user deleted or renamed (a JSON list of names).
const HIDDEN_BUILTINS_KEY: &str = "state.autoText.hiddenBuiltins";

/// Names of the built-in AutoText entries the user deleted or renamed.
fn hidden_builtins(s: &Session) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(&s.settings.text(HIDDEN_BUILTINS_KEY)).unwrap_or_default()
}

/// Hides the built-in AutoText entry `name` (Building Blocks Organizer › Delete or Edit Properties of it), so
/// it does not come back. Returns whether there was one to hide.
fn hide_builtin(s: &mut Session, name: &str) -> Result<bool, CmdError> {
    if !builtin_auto_text(s).iter().any(|b| b.name.eq_ignore_ascii_case(name)) {
        return Ok(false);
    }
    let mut hidden = hidden_builtins(s);
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    hidden.push(name.trim().to_owned());
    // The list keeps the newest names when it runs long (the names follow General › User name).
    let text = loop {
        let text = serde_json::to_string(&hidden).unwrap_or_default();
        if text.chars().count() <= MAX_HIDDEN_BUILTINS_CHARS || hidden.is_empty() {
            break text;
        }
        hidden.remove(0);
    };
    s.settings.set(HIDDEN_BUILTINS_KEY, &json!(text))?;
    Ok(true)
}

/// Longest [`HIDDEN_BUILTINS_KEY`] value (chars).
const MAX_HIDDEN_BUILTINS_CHARS: usize = 512;

/// AutoText entries every profile starts with: the user's name and initials (General › User name and
/// Initials). Saving an entry of the same name replaces one; deleting or renaming one hides it for good.
fn builtin_auto_text(s: &Session) -> Vec<BuildingBlock> {
    let hidden = hidden_builtins(s);
    let mut out = Vec::new();
    for text in [user_name(s), user_initials(s)] {
        let text: String = text.chars().take(MAX_BLOCK_NAME).collect();
        if text.is_empty()
            || out.iter().any(|b: &BuildingBlock| b.name.eq_ignore_ascii_case(&text))
            || hidden.iter().any(|h| h.eq_ignore_ascii_case(text.trim()))
        {
            continue;
        }
        out.push(BuildingBlock {
            gallery: "autoText".into(),
            name: text.clone(),
            blocks: vec![Block::text_paragraph(&text)],
            category: DEFAULT_CATEGORY.into(),
            description: String::new(),
        });
    }
    out
}

/// Shortest typed start that finds an AutoText entry by the beginning of its name.
pub const AUTO_TEXT_MIN_PREFIX: usize = 4;

/// The AutoText entry the word before the caret names: the entry of that name, else the only entry whose
/// name starts with it (at least [`AUTO_TEXT_MIN_PREFIX`] chars typed).
pub fn auto_text_match(s: &Session, cid: u64) -> Option<(usize, String, BuildingBlock)> {
    let (offset, before) = s.composes.get(&cid)?.text_before_caret(MAX_BLOCK_NAME)?;
    let before: Vec<char> = before.chars().collect();
    let entries = gallery_entries(s, cid, "autoText");
    // For each entry, the longest run of typed text that starts a word and begins the entry's name (the
    // whole name, or at least AUTO_TEXT_MIN_PREFIX chars of it).
    let mut best: Option<(usize, &BuildingBlock)> = None;
    let mut tie = false;
    for b in entries.iter().rev() {
        let name: Vec<char> = b.name.chars().flat_map(char::to_lowercase).collect();
        let fits = (1..=name.len().min(before.len())).rev().find(|&k| {
            let start = before.len().saturating_sub(k);
            let at_word =
                start == 0 || before.get(start.saturating_sub(1)).is_some_and(|c| !c.is_alphanumeric());
            let typed: Vec<char> =
                before.get(start..).unwrap_or_default().iter().flat_map(|c| c.to_lowercase()).collect();
            at_word
                && name.get(..typed.len()) == Some(typed.as_slice())
                && (k == name.len() || k >= AUTO_TEXT_MIN_PREFIX)
        });
        let Some(k) = fits else { continue };
        match best {
            Some((bk, _)) if bk > k => {}
            Some((bk, _)) if bk == k => tie = true,
            _ => {
                best = Some((k, b));
                tie = false;
            }
        }
    }
    let (k, b) = best?;
    // Two entries that start the same way are ambiguous unless one is typed out in full.
    if tie && b.name.chars().count() != k {
        return None;
    }
    let start = before.len().saturating_sub(k);
    let word: String = before.get(start..).unwrap_or_default().iter().collect();
    Some((offset.saturating_add(start), word, b.clone()))
}

/// Insert › Quick Parts › AutoText while typing (F3): the word before the caret becomes the AutoText entry
/// it names. `peek` only reports the entry (for a typing tip).
fn auto_text(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let Some((start, word, block)) = auto_text_match(s, cid) else {
        return Err(CmdError::Failed("no AutoText entry matches the word before the cursor".into()));
    };
    if opt_bool(p, "peek") == Some(true) {
        return Ok(json!({"word": word, "name": block.name}));
    }
    let c = compose_mut(s, p)?;
    c.checkpoint();
    let focus = c.selection.focus.clone();
    c.selection = crate::compose::BodySelection { anchor: DocPos::new(focus.path.clone(), start), focus };
    c.delete_selection();
    let single = matches!(block.blocks.as_slice(), [Block::Paragraph { .. }]);
    if single {
        c.insert_fragment(
            &Doc { blocks: block.blocks.clone(), ..Default::default() },
            crate::compose::PasteMode::KeepSource,
        );
    } else {
        c.insert_blocks(block.blocks.clone());
    }
    c.dirty = true;
    Ok(json!({"word": word, "inserted": block.name, "caret": caret(c)}))
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
        ..Default::default()
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
                let category = if b.category.is_empty() { DEFAULT_CATEGORY } else { b.category.as_str() };
                json!({"gallery": b.gallery, "name": b.name, "preview": preview, "category": category,
                       "description": b.description})
            })
            .collect();
        return dialog(s, "quickParts", json!({"compose": cid, "gallery": gallery, "entries": entries}));
    };
    let cid = compose_id(s, p)?;
    if opt_str(p, "action") == Some("edit") {
        return edit_block(s, p, cid, gallery, &name);
    }
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
        let mut removed = before.saturating_sub(saved.len());
        for c in s.composes.values_mut() {
            let n = c.building_blocks.len();
            c.building_blocks.retain(|b| !matches(b));
            removed = removed.max(n.saturating_sub(c.building_blocks.len()));
        }
        // A built-in entry of that name (the user's name or initials) goes too, and stays gone.
        let builtin = gallery.is_none_or(|g| g == "autoText") && hide_builtin(s, &name)?;
        if removed == 0 && !builtin {
            return Err(CmdError::Params(format!("no saved entry `{name}`")));
        }
        if removed > 0 {
            save_building_blocks(s, &saved)?;
        }
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

/// Category of a building block saved without one.
pub const DEFAULT_CATEGORY: &str = "General";
/// Longest building-block name, category and description kept.
const MAX_BLOCK_NAME: usize = 64;
const MAX_BLOCK_DESCRIPTION: usize = 255;

fn trimmed(p: &Value, key: &str, max: usize) -> Option<String> {
    opt_str(p, key).map(str::trim).filter(|t| !t.is_empty()).map(|t| t.chars().take(max).collect())
}

/// The saved entries of `gallery`, oldest first (what its menu lists).
pub fn gallery_entries(s: &Session, cid: u64, gallery: &str) -> Vec<BuildingBlock> {
    merged_blocks(s, cid, |b| b.gallery == gallery)
}

/// Categories used in `gallery` ("General" first).
fn gallery_categories(s: &Session, cid: u64, gallery: &str) -> Vec<String> {
    let mut out = vec![DEFAULT_CATEGORY.to_owned()];
    for b in gallery_entries(s, cid, gallery) {
        if !b.category.is_empty() && !out.iter().any(|c| c.eq_ignore_ascii_case(&b.category)) {
            out.push(b.category);
        }
    }
    out
}

/// Stores `entry` for every open compose and in the profile, replacing one of the same gallery and name.
/// Returns (replaced an existing one, written to the profile).
fn store_block(s: &mut Session, entry: BuildingBlock) -> Result<(bool, bool), CmdError> {
    let same = |b: &BuildingBlock| b.gallery == entry.gallery && b.name.eq_ignore_ascii_case(&entry.name);
    let mut replaced = false;
    // Every open compose shares the galleries; the profile keeps them.
    for other in s.composes.values_mut() {
        let n = other.building_blocks.len();
        other.building_blocks.retain(|b| !same(b));
        replaced |= other.building_blocks.len() != n;
        other.building_blocks.push(entry.clone());
    }
    let mut saved = load_building_blocks(s);
    let n = saved.len();
    saved.retain(|b| !same(b));
    replaced |= saved.len() != n;
    saved.push(entry);
    if saved.len() > MAX_BUILDING_BLOCKS {
        let extra = saved.len() - MAX_BUILDING_BLOCKS;
        saved.drain(..extra);
    }
    let persisted = blocks_path(s).is_some();
    save_building_blocks(s, &saved)?;
    Ok((replaced, persisted))
}

/// Save Selection to … Gallery: without a `name`, the Create New Building Block dialog (name proposed from
/// the selection's first words, gallery, category, description); with one, saves the selection.
fn save_to_gallery(s: &mut Session, p: &Value) -> CmdResult {
    let gallery = req_str(p, "gallery")?.to_owned();
    if !GALLERIES.contains(&gallery.as_str()) {
        return Err(CmdError::Params(format!("unknown gallery `{gallery}`")));
    }
    let cid = compose_id(s, p)?;
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    if c.selection.is_collapsed() {
        return Err(CmdError::Disabled("select what to save first".into()));
    }
    let Some(name) = trimmed(p, "name", MAX_BLOCK_NAME) else {
        let suggested: String = c
            .selected_text()
            .split_whitespace()
            .take(4)
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MAX_BLOCK_NAME)
            .collect();
        let existing: Vec<String> = gallery_entries(s, cid, &gallery).into_iter().map(|b| b.name).collect();
        let categories = gallery_categories(s, cid, &gallery);
        return dialog(
            s,
            "createBuildingBlock",
            json!({"compose": cid, "gallery": gallery, "galleries": GALLERIES, "name": suggested,
                   "category": DEFAULT_CATEGORY, "categories": categories, "existing": existing}),
        );
    };
    let category = trimmed(p, "category", MAX_BLOCK_NAME).unwrap_or_else(|| DEFAULT_CATEGORY.to_owned());
    let description = trimmed(p, "description", MAX_BLOCK_DESCRIPTION).unwrap_or_default();
    let frag = c.fragment();
    let entry = BuildingBlock {
        gallery: gallery.clone(),
        name: name.clone(),
        blocks: frag.blocks,
        category,
        description,
    };
    let (replaced, persisted) = store_block(s, entry)?;
    Ok(json!({"saved": name, "gallery": gallery, "persisted": persisted, "replaced": replaced}))
}

/// Building Blocks Organizer › Edit Properties: renames an entry, moves it to another gallery or changes its
/// category and description.
fn edit_block(s: &mut Session, p: &Value, cid: u64, gallery: Option<&str>, name: &str) -> CmdResult {
    let found = find_block(s, cid, gallery, name)
        .ok_or_else(|| CmdError::Params(format!("no saved entry `{name}`")))?;
    let new_gallery = opt_str(p, "newGallery").unwrap_or(&found.gallery).to_owned();
    if !GALLERIES.contains(&new_gallery.as_str()) {
        return Err(CmdError::Params(format!("unknown gallery `{new_gallery}`")));
    }
    let new_name = trimmed(p, "newName", MAX_BLOCK_NAME).unwrap_or_else(|| found.name.clone());
    let category = trimmed(p, "category", MAX_BLOCK_NAME).unwrap_or_else(|| {
        if found.category.is_empty() { DEFAULT_CATEGORY.to_owned() } else { found.category.clone() }
    });
    let description = match p.get("description") {
        Some(_) => trimmed(p, "description", MAX_BLOCK_DESCRIPTION).unwrap_or_default(),
        None => found.description.clone(),
    };
    let moved = new_gallery != found.gallery || !new_name.eq_ignore_ascii_case(&found.name);
    if moved && find_block(s, cid, Some(&new_gallery), &new_name).is_some() {
        return Err(CmdError::Params(format!("an entry named `{new_name}` is already in that gallery")));
    }
    let old = (found.gallery.clone(), found.name.clone());
    // Renaming or moving a built-in entry replaces it: the built-in one is hidden, not left as a duplicate.
    if moved && old.0 == "autoText" {
        hide_builtin(s, &old.1)?;
    }
    let is_old = |b: &BuildingBlock| b.gallery == old.0 && b.name.eq_ignore_ascii_case(&old.1);
    let mut saved = load_building_blocks(s);
    saved.retain(|b| !is_old(b));
    save_building_blocks(s, &saved)?;
    for c in s.composes.values_mut() {
        c.building_blocks.retain(|b| !is_old(b));
    }
    let entry = BuildingBlock {
        gallery: new_gallery.clone(),
        name: new_name.clone(),
        blocks: found.blocks,
        category,
        description,
    };
    store_block(s, entry)?;
    Ok(json!({"edited": new_name, "gallery": new_gallery}))
}

/// WordArt: the selected text (or `text`, or "Your text here") drawn in the chosen style with the theme's
/// heading font and colours, inserted as a picture in place of the selection.
/// Insert › WordArt: the selected text (or "Your text here", ready to type over) as a floating WordArt
/// object in `style`. Its text stays editable; the selection leaves the body in the same undo step.
fn word_art(s: &mut Session, p: &Value) -> CmdResult {
    let style: String = req_str(p, "style")?.trim().chars().take(300).collect();
    let at = point_param(p)?;
    let cid = compose_id(s, p)?;
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let theme = c.theme_resolved();
    let selected = (!c.selection.is_collapsed()).then(|| c.selected_text());
    let given = opt_str(p, "text").map(str::to_owned).or(selected);
    let placeholder = given.as_deref().is_none_or(|t| t.trim().is_empty());
    let text: String = given
        .map(|t| {
            t.lines()
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Your text here".to_owned())
        .chars()
        .take(drawing::MAX_DRAWING_TEXT)
        .collect();
    let d = drawing::new_word_art(&theme, &text, &style);
    let c = compose_mut(s, p)?;
    // Refused before anything changes: the selected text is only replaced when the WordArt fits.
    if c.body.drawings.len() >= drawing::MAX_DRAWINGS {
        return Err(CmdError::Failed(format!("a message holds at most {} drawings", drawing::MAX_DRAWINGS)));
    }
    c.checkpoint();
    let at = match at {
        Some(at) => Some(at),
        None => Some(drawing::caret_point(c)),
    };
    c.delete_selection();
    let id = drawing::add(c, d, at, false).map_err(CmdError::Failed)?;
    Ok(json!({"wordArt": style, "text": text, "drawing": id, "editText": placeholder}))
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
                title: None,
                href: format!("mailto:{e}"),
                children: vec![Inline::Text { text: e.clone(), style: TextStyle::default() }],
            }],
        });
    }
    for (kind, num) in &contact.phones {
        blocks.push(Block::text_paragraph(&format!("{kind}: {num}")));
    }
    let card = Block::Table {
        borders: None,
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

/// A file shown as an icon with its name under it (PNG, logical width × height).
fn object_icon_png(name: &str) -> Option<(Vec<u8>, f32, f32)> {
    let fonts = mailcraft_fonts::FontDb::bundled();
    let icon = icons::png("document", 48.0 * RASTER_SCALE)?;
    let canvas = mailcraft_render::svg::decode_png(&icon)?;
    let rgba = canvas.to_rgba_unpremultiplied();
    let caption: String = {
        let n: Vec<char> = name.chars().collect();
        if n.len() > 28 {
            let head: String = n.iter().take(12).collect();
            let tail: String = n.iter().skip(n.len().saturating_sub(12)).collect();
            format!("{head}…{tail}")
        } else {
            name.to_owned()
        }
    };
    let ts = TextStyle { size_pt: Some(8.0), ..Default::default() };
    let (mut list, x0, y0, tw, th) = text_list(&caption, &ts, &fonts);
    let w = tw.clamp(56.0, MAX_RASTER_SIDE) + 8.0;
    let h = 48.0 + 6.0 + th + 4.0;
    translate(&mut list, (w - tw) / 2.0 - x0, 54.0 - y0);
    list.items.push(mailcraft_layout::DisplayItem::Image {
        rect: mailcraft_geom::Rect::new((w - 48.0) / 2.0, 0.0, 48.0, 48.0),
        src: "icon".into(),
        alt: String::new(),
    });
    let (cw, ch) = (canvas.width, canvas.height);
    let images = move |src: &str| (src == "icon").then(|| (cw, ch, rgba.clone()));
    let (dw, dh) = (u32::try_from(dev(w)).ok()?, u32::try_from(dev(h)).ok()?);
    let png = mailcraft_render::render(&list, &fonts, dw, dh, RASTER_SCALE, Color::rgba(0, 0, 0, 0), &images)
        .to_png();
    (!png.is_empty()).then_some((png, w, h))
}

/// Insert › Object: a file put into the message. As text its text is inserted; otherwise it is shown as an
/// icon with its name and attached (`link`: the icon links to the file instead and nothing is attached).
/// Without a file the Object dialog opens (`path` pre-filled when given alone with `ask`).
fn object(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "ask") == Some(true) {
        let path = opt_str(p, "path").unwrap_or("").to_owned();
        let text_like = mime_for(&path).starts_with("text/");
        return dialog(
            s,
            "insertObject",
            json!({"compose": compose_id(s, p)?, "path": path, "asText": text_like}),
        );
    }
    let link = opt_bool(p, "link").unwrap_or(false);
    if link {
        let path = opt_str(p, "path")
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| CmdError::Params("`path` (the file to link to) required".into()))?
            .to_owned();
        if !std::path::Path::new(&path).is_file() {
            return Err(CmdError::Failed(format!("{path} was not found")));
        }
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let (png, w, h) =
            object_icon_png(&name).ok_or_else(|| CmdError::Failed("the icon could not be drawn".into()))?;
        let href = file_url(&path);
        let c = compose_mut(s, p)?;
        let cid = c.next_content_id();
        c.checkpoint();
        c.inline_images.push(InlineImage {
            content_id: cid.clone(),
            filename: "object.png".into(),
            mime_type: "image/png".into(),
            data: png,
        });
        let frag = Doc {
            blocks: vec![Block::Paragraph {
                style: ParaStyle::default(),
                inlines: vec![Inline::Link {
                    href: href.clone(),
                    title: None,
                    children: vec![Inline::Image {
                        src: format!("cid:{cid}"),
                        alt: name.clone(),
                        width: Some(w),
                        height: Some(h),
                    }],
                }],
            }],
            ..Default::default()
        };
        c.insert_fragment(&frag, crate::compose::PasteMode::KeepSource);
        return Ok(json!({"inserted": name, "asText": false, "attached": false, "linked": href, "cid": cid}));
    }
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
    let (png, w, h) =
        object_icon_png(&name).ok_or_else(|| CmdError::Failed("the icon could not be drawn".into()))?;
    let mime = mime_for(&name).to_owned();
    let c = compose_mut(s, p)?;
    c.attachments.push(ComposeAttachment { filename: name.clone(), mime_type: mime, data });
    let cid = put_png(c, "object", png, &name, w, h)?;
    Ok(json!({"inserted": name, "asText": false, "attached": true, "cid": cid}))
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
            "insert.autoText",
            "AutoText",
            "Insert › Quick Parts › AutoText (typing)",
            "F3",
            r#"{"peek"?:bool}"#,
            false,
            has_compose,
            auto_text,
        ),
        spec(
            "insert.calendar",
            "Calendar",
            "Insert › Include",
            "",
            r#"{"calendar"?:string|number,"range"?:"today"|"tomorrow"|"next7"|"next14"|"next30"|"thisMonth"|"nextMonth","from"?:string,"to"?:string,"detail"?:"availability"|"limited"|"full","layout"?:"schedule"|"list","workingHours"?:bool,"includePrivate"?:bool,"ics"?:bool}"#,
            false,
            rich_compose,
            calendar,
        ),
        spec(
            "insert.chart",
            "Chart",
            "Insert › Illustrations",
            "",
            r#"{"kind"?:"column"|"stackedColumn"|"bar"|"stackedBar"|"line"|"area"|"pie"|"doughnut"|"scatter"|"radar","sheet"?:[[..]],"categories"?:[string],"series"?:[{"name","values"}],"data"?:[[string,number]],"title"?:string,"legend"?:bool,"width"?:number,"height"?:number,"edit"?:string}  (no data: the data sheet dialog; edit: a chart picture's content id)"#,
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
            "insert.editPicture",
            "Edit Picture",
            "Compose body (double-click a chart, SmartArt graphic or 3D model)",
            "",
            r#"{"cid":string}"#,
            false,
            rich_compose,
            edit_picture,
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
            r#"{"name"?:string,"size"?:number}  (no name: the Insert Icons dialog)"#,
            false,
            rich_compose,
            icon,
        ),
        spec(
            "insert.link",
            "Link",
            "Message › Include; Insert › Links",
            "Mod+K",
            r#"{"url"?:string,"text"?:string,"screenTip"?:string,"bookmark"?:string,"heading"?:number,"top"?:bool,"email"?:string,"subject"?:string,"path"?:string}  ("" url removes the link; bookmark / heading / top: a place in this message; email + subject: a mailto link; path: a recent file, linked when it is a web address, else attached)"#,
            false,
            rich_compose,
            link,
        ),
        spec(
            "insert.model3d",
            "3D Models",
            "Insert › Illustrations",
            "",
            r#"{"path"?:string} | {"name":string,"data_base64":string} | {"edit":string,"yaw"?:number,"pitch"?:number}  (OBJ, STL, PLY, glTF, GLB)"#,
            false,
            rich_compose,
            model3d,
        ),
        spec(
            "insert.object",
            "Object",
            "Insert › Text",
            "",
            r#"{"path"?:string,"asText"?:bool,"link"?:bool,"ask"?:bool}  (link: an icon linking to the file; ask: the dialog for that path)"#,
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
            r#"{"name"?:string,"gallery"?:string,"action"?:"insert"|"delete"|"edit","newName"?:string,"newGallery"?:string,"category"?:string,"description"?:string}  (newName, newGallery, category and description with "edit")"#,
            false,
            has_compose,
            quick_part,
        ),
        spec(
            "insert.saveToGallery",
            "Save Selection to Gallery",
            "Insert › Text › Quick Parts / Text Box; Insert › Tables › Quick Tables; Insert › Symbols › Equation",
            "",
            r#"{"gallery":"quickParts"|"autoText"|"textBoxes"|"quickTables"|"equations","name"?:string,"category"?:string,"description"?:string}  (no name: the Create New Building Block dialog)"#,
            false,
            has_body_selection,
            save_to_gallery,
        ),
        spec(
            "insert.screenshot",
            "Screenshot",
            "Insert › Illustrations",
            "",
            r#"{"path"?:string,"name"?:string,"data_base64"?:string,"alt"?:string,"window"?:string,"clip"?:bool,"unavailable"?:string}  (no picture: the saved-picture dialog, with the `unavailable` reason)"#,
            false,
            rich_compose,
            screenshot,
        ),
        spec(
            "insert.shape",
            "Shapes",
            "Insert › Illustrations",
            "",
            r#"{"kind":string,"draw"?:bool,"x"?:number,"y"?:number,"w"?:number,"h"?:number,"points"?:[[number,number]],"color"?:string}  (draw: arm the drawing tool)"#,
            false,
            rich_compose,
            shape,
        ),
        spec(
            "insert.smartArt",
            "SmartArt",
            "Insert › Illustrations",
            "",
            r#"{"layout"?:"list"|"blocks"|"process"|"chevron"|"cycle"|"radial"|"hierarchy"|"venn"|"matrix"|"pyramid","items"?:[string],"edit"?:string}  (tabs before an item lower its level)"#,
            false,
            rich_compose,
            smart_art,
        ),
        spec(
            "insert.symbol",
            "Symbol",
            "Insert › Symbols",
            "",
            r#"{"char"?:string} | {"font":"Symbol"|"Wingdings","code":number}  ("©" or "U+00A9"; a symbol font's character is inserted as the Unicode character it stands for)"#,
            false,
            has_compose,
            symbol,
        ),
        spec(
            "insert.table",
            "Table",
            "Insert › Tables",
            "",
            r#"{"rows"?:number,"cols"?:number,"mode"?:"grid"|"dialog"|"draw"|"fromText"|"quick","name"?:string,"widths"?:[number],"pad"?:bool,"action"?:"stroke"|"erase","path"?:[number],"offset"?:number,"dx"?:number,"dy"?:number,"separator"?:"tabs"|"commas"|"semicolons"|"paragraphs"|"auto"|string}  ("draw": the table pencil, or the drawing pad with pad; action: a pencil stroke or eraser click at path; widths: the pad's columns)"#,
            false,
            rich_compose,
            table,
        ),
        spec(
            "insert.textBox",
            "Text Box",
            "Insert › Text",
            "",
            r#"{"preset"?:"simple"|"sidebar"|"draw"|"saved:<name>","text"?:string,"x"?:number,"y"?:number,"w"?:number,"h"?:number,"vertical"?:bool}  ("draw" without x/y arms the text box tool; vertical: the text runs top to bottom)"#,
            false,
            rich_compose,
            text_box,
        ),
        spec(
            "insert.wordArt",
            "WordArt",
            "Insert › Text",
            "",
            r#"{"style":string,"text"?:string,"x"?:number,"y"?:number}"#,
            false,
            rich_compose,
            word_art,
        ),
    ]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::compose::tests::{open, session};
    use super::*;
    use mailcraft_model::{Appointment, Contact};

    fn setup() -> (Session, u64) {
        let mut s = session();
        // No user name: the AutoText gallery starts empty.
        for a in &mut s.mailbox.accounts {
            a.sender_name.clear();
        }
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
        assert!(s.run("insert.icon", &json!({"name": "unicorn"})).is_err());
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
        // The star is a floating drawing; the rest are pictures in the text.
        assert_eq!(c.body.drawings.len(), 1);
        assert_eq!(c.inline_images.len(), 8);
        assert!(c.generated.iter().any(|g| g.kind == "chart" && g.spec["title"] == "Share <1>"));
        // Every generated picture is PNG (many mail programs show no SVG).
        assert!(c.inline_images.iter().all(|i| i.mime_type == "image/png" && i.data.starts_with(b"\x89PNG")));
        let r = s.run("insert.screenshot", &json!({"clip": true})).unwrap();
        assert_eq!(r["dialog"], "screenshot");
        assert_eq!(last_dialog(&s)["capture"], false);
    }

    /// Pictures drawn before shapes became drawing objects are still redrawn from their recipes: a shape covers
    /// its middle and leaves its corner empty, in the theme's colour and effects.
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
        for kind in ["rectangle", "oval", "star", "line", "arrow", "heart", "callout", "canvas"] {
            let (data, w, h) = shape_png(kind, accent, "Office").unwrap();
            assert!(data.starts_with(b"\x89PNG") && w > 100.0 && h > 50.0, "{kind}");
        }
    }

    #[test]
    fn shape_colour_follows_the_theme() {
        let (mut s, id) = setup();
        s.run("compose.theme", &json!({"part": "colors", "name": "Red"})).unwrap();
        s.run("insert.shape", &json!({"kind": "rectangle"})).unwrap();
        let red = s.composes[&id].body.drawings[0].fill;
        s.run("compose.theme", &json!({"part": "colors", "name": "Green"})).unwrap();
        // The shape drawn in the old theme's accent takes the new accent.
        let c = &s.composes[&id];
        assert_ne!(c.body.drawings[0].fill, red);
        assert_eq!(c.body.drawings[0].fill, Some(c.theme_resolved().color(crate::compose::ACCENT1)));
        assert!(s.run("insert.shape", &json!({"kind": "rectangle", "color": "nope"})).is_err());
        s.run("insert.shape", &json!({"kind": "rectangle", "color": "#123456"})).unwrap();
        assert_eq!(s.composes[&id].body.drawings[1].fill, Some(Color::rgb(0x12, 0x34, 0x56)));
    }

    /// Regression: with the drawing limit reached, WordArt from selected text deleted the text and then
    /// failed, leaving the edit half done.
    #[test]
    fn word_art_over_the_drawing_limit_leaves_the_selected_text() {
        let (mut s, id) = setup();
        s.run("compose.insertText", &json!({"text": "Keep me"})).unwrap();
        let c = s.composes.get_mut(&id).unwrap();
        for n in 0..drawing::MAX_DRAWINGS {
            let d = mailcraft_model::rich::Drawing { id: n as u32 + 1, ..Default::default() };
            c.body.drawings.push(d);
        }
        c.select_all();
        let (undo, text) = (c.edit_undo.len(), c.body.to_plain_text());
        assert!(text.contains("Keep me"));
        assert!(s.run("insert.wordArt", &json!({"style": "Fill: Black, Text color 1"})).is_err());
        let c = &s.composes[&id];
        assert_eq!(c.body.to_plain_text(), text);
        assert_eq!(c.edit_undo.len(), undo, "no undo step for a refused insert");
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
        assert!(parse_wordart("Fill: Dark Teal, Accent color 1; Soft Bevel", &theme).bevel == Some(false));
        assert!(parse_wordart("Fill: Dark Green, Accent color 3; Sharp Bevel", &theme).bevel == Some(true));
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
    fn models_obj_stl_and_ply_turn() {
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
        let ply = b"ply\nformat ascii 1.0\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\nelement face 2\nproperty list uchar int vertex_indices\nend_header\n0 0 0\n1 0 0\n1 1 0\n0 1 1\n3 0 1 2\n3 0 2 99\n";
        let (pv, pe) = parse_model(ply).unwrap();
        assert_eq!(pv.len(), 4);
        assert_eq!(pe.len(), 4, "edges to a vertex that does not exist are dropped");
        assert!(parse_model(b"ply\nformat binary_little_endian 1.0\nend_header\n").is_none());
        let png = model_png(&v, &e, 100.0, 35.0, 25.0, Color::BLACK).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        assert_ne!(
            png,
            model_png(&v, &e, 100.0, 120.0, 25.0, Color::BLACK).unwrap(),
            "turning changes the view"
        );
        let (mut s, id) = setup();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.obj");
        std::fs::write(&path, obj).unwrap();
        let r = s.run("insert.model3d", &json!({"path": path.to_string_lossy()})).unwrap();
        let cid = r["cid"].as_str().unwrap().to_owned();
        assert_eq!(s.composes[&id].inline_images[0].mime_type, "image/png");
        // Double-click opens the 3D Model Views dialog; turning draws it again in place, undoably.
        assert_eq!(s.run("insert.editPicture", &json!({"cid": cid})).unwrap()["dialog"], "model3dView");
        assert_eq!(last_dialog(&s)["yaw"], 35.0);
        let r = s.run("insert.model3d", &json!({"edit": cid, "yaw": 400, "pitch": 10})).unwrap();
        let new = r["cid"].as_str().unwrap().to_owned();
        assert_ne!(new, cid);
        let c = &s.composes[&id];
        assert_eq!(image_srcs(c), vec![format!("cid:{new}")]);
        let spec = &c.generated.iter().find(|g| g.content_id == new).unwrap().spec;
        assert_eq!((spec["yaw"].as_f64(), spec["pitch"].as_f64()), (Some(40.0), Some(10.0)));
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(image_srcs(&s.composes[&id]), vec![format!("cid:{cid}")]);
        assert!(s.run("insert.model3d", &json!({"edit": "nope@x", "yaw": 1})).is_err());
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
        assert_eq!(
            s.run("insert.table", &json!({"mode": "draw", "pad": true})).unwrap()["dialog"],
            "drawTable"
        );
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("compose.insertText", &json!({"text": "a\tb\nc\td"})).unwrap();
        s.run("compose.select", &json!({"all": true})).unwrap();
        // Convert Text to Table first asks (tabs proposed, two columns), then converts.
        assert_eq!(
            s.run("insert.table", &json!({"mode": "fromText"})).unwrap()["dialog"],
            "convertTextToTable"
        );
        let d = last_dialog(&s);
        assert_eq!(
            (d["separator"].as_str(), d["cols"].as_u64(), d["rows"].as_u64()),
            (Some("tabs"), Some(2), Some(2))
        );
        s.run("insert.table", &json!({"mode": "fromText", "separator": "tabs"})).unwrap();
        assert!(
            matches!(&s.composes[&id].body.blocks[0], Block::Table { rows, .. } if rows.len() == 2 && rows[1].len() == 2)
        );
        s.run("insert.horizontalLine", &json!({})).unwrap();
        assert!(s.composes[&id].body.to_plain_text().contains("------"), "rule inside the cell");
        // A text box floats over the text; Draw Text Box arms the tool, or draws where it is told.
        let r = s.run("insert.textBox", &json!({"text": "boxed"})).unwrap();
        assert_eq!(r["editText"], true);
        let c = &s.composes[&id];
        assert_eq!(c.body.drawings[0].kind, mailcraft_model::DrawingKind::TextBox);
        assert_eq!(c.body.drawings[0].text, "boxed");
        assert!(!c.body.to_plain_text().contains("boxed"), "not in the text flow");
        assert_eq!(s.run("insert.textBox", &json!({"preset": "draw"})).unwrap()["armed"], "textBox");
        s.run("insert.textBox", &json!({"preset": "draw", "x": 10, "y": 20, "w": 90, "h": 40})).unwrap();
        let d = &s.composes[&id].body.drawings[1];
        assert_eq!((d.x, d.y, d.w, d.h), (10.0, 20.0, 90.0, 40.0));
        assert!(s.run("insert.textBox", &json!({"preset": "draw", "x": 10})).is_err());
        assert!(s.run("insert.textBox", &json!({"preset": "nope"})).is_err());
    }

    fn tables(s: &Session, id: u64) -> Vec<Vec<Vec<TableCell>>> {
        fn walk(blocks: &[Block], out: &mut Vec<Vec<Vec<TableCell>>>) {
            for b in blocks {
                match b {
                    Block::Table { rows, .. } => {
                        out.push(rows.clone());
                        rows.iter().flatten().for_each(|c| walk(&c.blocks, out));
                    }
                    Block::Quote { blocks } | Block::Container { blocks, .. } => walk(blocks, out),
                    Block::List { items, .. } => items.iter().for_each(|i| walk(i, out)),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&s.composes[&id].body.blocks, &mut out);
        out
    }

    #[test]
    fn convert_text_to_table_separators_and_columns() {
        let (mut s, id) = setup();
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("compose.insertText", &json!({"text": "a;b;c\nd;e"})).unwrap();
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("insert.table", &json!({"mode": "fromText", "separator": "semicolons"})).unwrap();
        let t = tables(&s, id);
        assert_eq!(t[0].len(), 2);
        assert_eq!(t[0][1].len(), 3, "short rows are padded");
        s.run("compose.undo", &json!({})).ok();
        s.run("format.undo", &json!({})).ok();
        // One cell per paragraph, three to a row.
        let (mut s, id) = setup();
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("compose.insertText", &json!({"text": "1\n2\n3\n4"})).unwrap();
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("insert.table", &json!({"mode": "fromText", "separator": "paragraphs", "cols": 3})).unwrap();
        let t = tables(&s, id);
        assert_eq!((t[0].len(), t[0][0].len()), (2, 3));
        // Any single character, and columns beyond what the text has.
        let (mut s, id) = setup();
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("compose.insertText", &json!({"text": "x-y"})).unwrap();
        s.run("compose.select", &json!({"all": true})).unwrap();
        assert!(s.run("insert.table", &json!({"mode": "fromText", "separator": "ab"})).is_err());
        assert!(s.run("insert.table", &json!({"mode": "fromText", "separator": "-", "cols": 99})).is_err());
        s.run("insert.table", &json!({"mode": "fromText", "separator": "-", "cols": 4})).unwrap();
        assert_eq!(tables(&s, id)[0][0].len(), 4);
    }

    #[test]
    fn draw_table_uses_the_drawn_widths() {
        let (mut s, id) = setup();
        assert_eq!(
            s.run("insert.table", &json!({"mode": "draw", "pad": true})).unwrap()["dialog"],
            "drawTable"
        );
        let r = s.run("insert.table", &json!({"mode": "draw", "widths": [100.0, 50.5], "rows": 3})).unwrap();
        assert_eq!((r["rows"].as_u64(), r["cols"].as_u64()), (Some(3), Some(2)));
        let t = tables(&s, id);
        assert_eq!(t[0].len(), 3);
        assert_eq!(t[0][0][1].width_px, Some(51.0));
        for bad in [json!([]), json!([0]), json!([-3.0]), json!(["x"]), json!(5)] {
            let r = s.run("insert.table", &json!({"mode": "draw", "widths": bad}));
            assert!(r.is_err(), "widths {bad} are refused");
        }
        // A huge width is clamped, not refused.
        let r = s.run("insert.table", &json!({"mode": "draw", "widths": [f64::MAX]})).unwrap();
        assert_eq!(r["drawn"], true);
        assert!(tables(&s, id).iter().any(|t| t[0][0].width_px == Some(2000.0)));
        assert!(s.run("insert.table", &json!({"mode": "draw", "widths": [10.0], "rows": 5000})).is_err());
        assert!(s.run("insert.table", &json!({"mode": "draw", "widths": vec![10.0; 64]})).is_err());
    }

    #[test]
    fn table_pencil_draws_splits_and_erases_in_the_body() {
        let (mut s, id) = setup();
        assert_eq!(s.run("insert.table", &json!({"mode": "draw"})).unwrap()["armed"], "tablePencil");
        assert_eq!(s.composes[&id].draw_tool.as_deref(), Some("tablePencil"));
        let stroke = |s: &mut Session, path: Vec<usize>, dx: f32, dy: f32| {
            s.run(
                "insert.table",
                &json!({"mode": "draw", "action": "stroke", "path": path, "offset": 0, "dx": dx, "dy": dy}),
            )
            .unwrap()["drawn"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        // Outside a table: a sideways stroke draws a one-cell table that wide.
        assert_eq!(stroke(&mut s, vec![0], 300.0, 40.0), "newTable");
        let t = tables(&s, id);
        assert_eq!((t.len(), t[0].len(), t[0][0].len()), (1, 1, 1));
        assert_eq!(t[0][0][0].width_px, Some(300.0));
        let at = s.composes[&id].table_starts()[0].path.clone();
        // Down the cell: two columns; across a cell: two rows.
        assert_eq!(stroke(&mut s, at.clone(), 2.0, 60.0), "split");
        assert_eq!(tables(&s, id)[0][0].len(), 2);
        assert_eq!(stroke(&mut s, at.clone(), 80.0, 1.0), "split");
        let t = tables(&s, id);
        assert_eq!(t[0].len(), 2);
        assert_eq!(t[0][0][1].rowspan, 2, "the cell beside it now spans both rows");
        // A tiny stroke changes nothing; the eraser merges the split cells back.
        assert_eq!(stroke(&mut s, at.clone(), 1.0, 1.0), "nothing");
        let r = s
            .run("insert.table", &json!({"mode": "draw", "action": "erase", "path": at, "offset": 0}))
            .unwrap();
        assert_eq!(r["drawn"], "merged");
        assert_eq!(
            tables(&s, id)[0][0][0].rowspan,
            2,
            "with the cell below it (the one beside spans two rows)"
        );
        // Each change is one undo step.
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(tables(&s, id)[0][0][0].rowspan, 1);
        // Junk paths and actions are refused or do nothing.
        assert_eq!(stroke(&mut s, vec![99, 3], 300.0, 0.0), "nothing");
        assert!(s.run("insert.table", &json!({"mode": "draw", "action": "zap", "path": [0]})).is_err());
        assert!(s.run("insert.table", &json!({"mode": "draw", "action": "stroke"})).is_err());
        assert_eq!(
            s.run("insert.table", &json!({"mode": "draw", "pad": true})).unwrap()["dialog"],
            "drawTable"
        );
    }

    #[test]
    fn sidebar_text_box_differs_from_simple() {
        let (mut s, id) = setup();
        s.run("insert.textBox", &json!({"preset": "simple", "text": "a"})).unwrap();
        s.run("insert.textBox", &json!({"preset": "sidebar", "text": "b"})).unwrap();
        let d = &s.composes[&id].body.drawings;
        let accent = s.composes[&id].theme_resolved().color(crate::compose::ACCENT1);
        assert_eq!((d[0].fill, d[0].outline.is_some()), (Some(Color::WHITE), true));
        assert_eq!((d[1].fill, d[1].outline), (Some(lighten(accent, 0.85)), None));
        assert_eq!((d[1].text_style.italic, d[1].text_style.color), (Some(true), Some(accent)));
    }

    #[test]
    fn theme_changes_redraw_charts_smart_art_and_recolour_drawings() {
        let (mut s, id) = setup();
        s.run("insert.smartArt", &json!({"layout": "process", "items": ["a", "b"]})).unwrap();
        s.run("insert.chart", &json!({"kind": "column", "data": [["a", 1], ["b", 2]]})).unwrap();
        s.run("insert.shape", &json!({"kind": "oval"})).unwrap();
        s.run("insert.shape", &json!({"kind": "rectangle", "color": "#ff0000"})).unwrap();
        s.run("insert.wordArt", &json!({"style": "Fill: Dark Teal, Accent color 1; Shadow", "text": "Hi"}))
            .unwrap();
        assert_eq!(s.composes[&id].generated.len(), 2, "drawings are objects, not pictures with recipes");
        let before = image_srcs(&s.composes[&id]);
        let data = |s: &Session, src: &str| {
            let cid = src.trim_start_matches("cid:");
            s.composes[&id]
                .inline_images
                .iter()
                .find(|i| i.content_id == cid)
                .map(|i| i.data.clone())
                .unwrap()
        };
        let old_smart = data(&s, &before[0]);
        let r = s.run("compose.theme", &json!({"part": "effects", "name": "Glossy"})).unwrap();
        assert_eq!(r["redrawn"], 1, "SmartArt takes the new effects (a chart has none)");
        let after = image_srcs(&s.composes[&id]);
        assert_eq!(after.len(), 2);
        assert_ne!(after[0], before[0], "a new picture replaces the old one");
        assert_eq!(after[1], before[1]);
        assert_ne!(data(&s, &after[0]), old_smart);
        // Colours: both pictures and the accent-coloured drawing change; the red shape keeps its fill.
        let accent_before = s.composes[&id].theme_resolved().color(crate::compose::ACCENT1);
        assert_eq!(s.composes[&id].body.drawings[0].fill, Some(accent_before));
        let r = s.run("compose.theme", &json!({"part": "colors", "name": "Green"})).unwrap();
        assert_eq!(r["redrawn"], 2, "{r}");
        let c = &s.composes[&id];
        let accent = c.theme_resolved().color(crate::compose::ACCENT1);
        assert_ne!(accent, accent_before);
        assert_eq!(c.body.drawings[0].fill, Some(accent));
        assert_eq!(c.body.drawings[1].fill, Some(Color::rgb(255, 0, 0)));
        // Undo of the body still finds the old pictures.
        assert!(
            before.iter().all(|src| c.inline_images.iter().any(|i| format!("cid:{}", i.content_id) == *src))
        );
        // A deleted picture is no longer redrawn.
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("compose.insertText", &json!({"text": "plain"})).unwrap();
        let r = s.run("compose.theme", &json!({"part": "colors", "name": "Red"})).unwrap();
        assert_eq!(r["redrawn"], 0, "pictures no longer in the body are not redrawn");
        assert_eq!(s.composes[&id].generated.len(), 5, "every recipe stays for Undo (2 + 1 + 2 redrawn)");
    }

    #[test]
    fn theme_change_then_undo_still_redraws_on_the_next_change() {
        // Regression: the redraw moved each recipe to the new picture, so after Undo (old pictures back in
        // the body) no recipe matched the body and later theme changes redrew nothing.
        let (mut s, id) = setup();
        s.run("insert.chart", &json!({"kind": "pie", "data": [["a", 1], ["b", 2]]})).unwrap();
        s.run("insert.smartArt", &json!({"layout": "list", "items": ["a"]})).unwrap();
        let before = image_srcs(&s.composes[&id]);
        let theme_before = s.composes[&id].theme.clone();
        let r = s.run("compose.theme", &json!({"part": "colors", "name": "Green"})).unwrap();
        assert_eq!(r["redrawn"], 2);
        assert_ne!(image_srcs(&s.composes[&id]), before);
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(image_srcs(&s.composes[&id]), before, "Undo brings back the old pictures");
        assert_eq!(s.composes[&id].theme, theme_before, "and the theme they were drawn in");
        let r = s.run("compose.theme", &json!({"part": "colors", "name": "Red"})).unwrap();
        assert_eq!(r["redrawn"], 2, "the old pictures are redrawn again: {r}");
        // Redo of the first change after a new change is gone; Undo twice returns to the start.
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(image_srcs(&s.composes[&id]), before);
    }

    #[test]
    fn many_redraws_keep_the_recipes_of_pictures_in_the_body() {
        let (mut s, id) = setup();
        s.run("insert.smartArt", &json!({"layout": "list", "items": ["a"]})).unwrap();
        // The SmartArt is the oldest recipe; many newer ones are of pictures no longer in the body.
        let c = s.composes.get_mut(&id).unwrap();
        for i in 0..MAX_GENERATED + 20 {
            c.generated.push(crate::compose::GeneratedPicture {
                content_id: format!("gone{i}@example.com"),
                kind: "shape".into(),
                spec: json!({"shape": "oval"}),
            });
        }
        trim_generated(c);
        assert_eq!(c.generated.len(), MAX_GENERATED);
        let r = s.run("compose.theme", &json!({"part": "colors", "name": "Green"})).unwrap();
        assert_eq!(r["redrawn"], 1, "the SmartArt in the body kept its recipe");
        assert!(s.composes[&id].generated.len() <= MAX_GENERATED);
    }

    #[test]
    fn charts_and_smart_art_are_edited_in_place() {
        let (mut s, id) = setup();
        let r = s
            .run(
                "insert.chart",
                &json!({"kind": "column", "sheet": [["", "A", "B"], ["x", 1, 2]], "title": "T"}),
            )
            .unwrap();
        let cid = r["cid"].as_str().unwrap().to_owned();
        // Double-click: the data sheet opens with the chart's data.
        assert_eq!(
            s.run("insert.editPicture", &json!({"cid": format!("cid:{cid}")})).unwrap()["dialog"],
            "insertChart"
        );
        let d = last_dialog(&s);
        assert_eq!(d["edit"], cid.as_str());
        assert_eq!(d["chart"]["series"][1]["name"], "B");
        // New data redraws it in place (one undo step).
        let r = s.run("insert.chart", &json!({"edit": cid, "kind": "line", "sheet": [["", "A"], ["x", 3], ["y", 4]], "title": "New"})).unwrap();
        let new = r["cid"].as_str().unwrap().to_owned();
        let c = &s.composes[&id];
        assert_eq!(image_srcs(c), vec![format!("cid:{new}")]);
        assert!(c.body.to_plain_text().contains("New"), "the alt text follows the title");
        s.run("compose.editUndo", &json!({})).unwrap();
        assert_eq!(image_srcs(&s.composes[&id]), vec![format!("cid:{cid}")]);
        assert!(s.run("insert.chart", &json!({"edit": "missing@x", "data": [["a", 1]]})).is_err());
        // SmartArt: layout and text pane, edited the same way.
        let r = s
            .run("insert.smartArt", &json!({"layout": "hierarchy", "items": ["Lead", "\tA", "\tB"]}))
            .unwrap();
        let sa = r["cid"].as_str().unwrap().to_owned();
        assert_eq!(s.run("insert.editPicture", &json!({"cid": sa})).unwrap()["dialog"], "smartArt");
        assert_eq!(last_dialog(&s)["current"]["items"][1], "\tA");
        s.run("insert.smartArt", &json!({"edit": sa, "layout": "Basic Pyramid", "items": ["1", "2", "3"]}))
            .unwrap();
        assert!(
            s.composes[&id].generated.iter().any(|g| g.kind == "smartArt" && g.spec["layout"] == "pyramid")
        );
        assert!(s.run("insert.smartArt", &json!({"layout": "spiral", "items": ["a"]})).is_err());
        // Plain pictures cannot be edited this way.
        let r = s.run("insert.icon", &json!({"name": "star"})).unwrap();
        assert!(s.run("insert.editPicture", &json!({"cid": r["cid"]})).is_err());
    }

    #[test]
    fn sent_messages_carry_drawings_as_pictures_and_no_svg() {
        let (mut s, id) = setup();
        s.run("compose.insertText", &json!({"text": "First\nSecond\nThird"})).unwrap();
        // A canvas with a shape inside it, a lone text box lower down, and an SVG picture.
        s.run("insert.shape", &json!({"kind": "canvas", "x": 0, "y": 0, "w": 200, "h": 120})).unwrap();
        s.run("insert.shape", &json!({"kind": "oval", "x": 20, "y": 20, "w": 60, "h": 40})).unwrap();
        s.run("insert.textBox", &json!({"text": "Note", "x": 300, "y": 40, "w": 80, "h": 30})).unwrap();
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="red"/></svg>"#;
        {
            let c = s.composes.get_mut(&id).unwrap();
            c.insert_image(
                InlineImage {
                    content_id: "pic@example.com".into(),
                    filename: "pic.svg".into(),
                    mime_type: "image/svg+xml".into(),
                    data: svg.to_vec(),
                },
                "pic",
                Some(8.0),
                Some(8.0),
            );
        }
        let c = s.composes[&id].clone();
        let flat = drawing::flattened(&c).unwrap();
        assert!(flat.body.drawings.is_empty());
        assert!(
            flat.inline_images.iter().all(|i| i.mime_type == "image/png" && i.data.starts_with(b"\x89PNG"))
        );
        assert!(
            flat.inline_images.iter().any(|i| i.content_id == "pic@example.com" && i.filename == "pic.png")
        );
        // Two groups (the canvas with its oval; the text box), each a picture paragraph indented to its left
        // edge before the text it was drawn over.
        let pics: Vec<(usize, f32, String)> = flat
            .body
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(i, b)| match b {
                Block::Paragraph { style, inlines } => match inlines.as_slice() {
                    [Inline::Image { alt, src, .. }] if src.contains("@mailcraft.invalid") => {
                        Some((i, style.indent_px, alt.clone()))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(pics.len(), 2, "{:?}", flat.body.blocks);
        assert_eq!(pics[0].0, 0, "the canvas starts at the top");
        assert!(pics.iter().any(|(_, indent, alt)| *indent > 250.0 && alt == "Note"));
        assert!(flat.body.to_plain_text().contains("Note"), "the plain text gets the drawing's text");
        // Nothing to flatten: no copy.
        let (s2, id2) = setup();
        assert!(drawing::flattened(&s2.composes[&id2]).is_none());
        // Sent: the message has PNG parts only and the drawings' text in its plain-text part.
        s.run("compose.setTo", &json!({"value": "a@example.com"})).unwrap();
        assert_eq!(s.run("compose.send", &json!({})).unwrap()["sent"], true);
        let sent = s.mailbox.mail.values().find(|m| !m.is_draft && m.subject.is_empty()).unwrap();
        let raw = String::from_utf8_lossy(sent.raw.as_ref().unwrap()).into_owned();
        assert!(!raw.contains("image/svg+xml"), "no SVG is sent");
        assert!(raw.matches("image/png").count() >= 3, "{raw}");
    }
    #[test]
    fn links_to_recent_files() {
        let (mut s, id) = setup();
        assert_eq!(file_url("/home/u/My Report.pdf"), "file:///home/u/My%20Report.pdf");
        assert_eq!(file_url("C:\\Docs\\a#b.txt"), "file:///C:/Docs/a%23b.txt");
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("compose.insertText", &json!({"text": ""})).unwrap();
        // A recent file is attached (a file: link would not open for the recipients).
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        let tmp = tempfile::tempdir_in(base).unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Plan 2.txt");
        std::fs::write(&path, b"plan").unwrap();
        let r = s.run("insert.link", &json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(r["attached"], "Plan 2.txt");
        let c = &s.composes[&id];
        assert_eq!(c.attachments.last().map(|a| a.data.clone()), Some(b"plan".to_vec()));
        assert!(!mailcraft_html::doc_to_html(&c.body).contains("file:"), "no link to a local file");
        assert!(s.run("insert.link", &json!({"path": dir.join("missing.txt").to_string_lossy()})).is_err());
        // The file joins the recent files (kept in the settings); files that are gone drop out of the list
        // (checked in the background, or at once when one fails to open).
        let recent = crate::cmd::compose::recent_files(&s);
        assert_eq!(recent.first().map(String::as_str), path.to_str());
        std::fs::remove_file(&path).unwrap();
        crate::cmd::compose::recheck_recent_files(&s);
        assert!(crate::cmd::compose::recent_files(&s).is_empty());
        // Picking a file that has gone since takes it off the list as well.
        let gone = dir.join("Gone.txt");
        std::fs::write(&gone, b"x").unwrap();
        s.run("compose.attachFile", &json!({"path": gone.to_string_lossy()})).unwrap();
        std::fs::remove_file(&gone).unwrap();
        assert_eq!(crate::cmd::compose::recent_files(&s).len(), 1, "not checked again yet");
        assert!(s.run("insert.link", &json!({"path": gone.to_string_lossy()})).is_err());
        assert!(crate::cmd::compose::recent_files(&s).is_empty());
        // A document on the web is linked, named after the file.
        let r = s.run("insert.link", &json!({"path": "https://www.example.com/team/Q3 plan.docx"})).unwrap();
        assert_eq!(
            (r["url"].as_str(), r["linked"].as_bool()),
            (Some("https://www.example.com/team/Q3%20plan.docx"), Some(true))
        );
        let html = mailcraft_html::doc_to_html(&s.composes[&id].body);
        assert!(
            html.contains("Q3 plan.docx</a>") && html.contains("https://www.example.com/team/Q3%20plan.docx"),
            "{html}"
        );
        assert_eq!(web_location("C:\\Docs\\c.txt"), None);
        assert_eq!(web_location("\\\\files.example.com\\team\\a.txt"), None);
        assert_eq!(web_location("file:///home/u/x"), None);
        drop(tmp);
    }

    #[test]
    fn recent_files_are_kept_across_sessions_in_the_settings() {
        let (mut s, _) = setup();
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        let tmp = tempfile::tempdir_in(base).unwrap();
        let mut paths = Vec::new();
        for i in 0..30 {
            let p = tmp.path().join(format!("f{i}.txt"));
            std::fs::write(&p, b"x").unwrap();
            s.run("compose.attachFile", &json!({"path": p.to_string_lossy()})).unwrap();
            paths.push(p.to_string_lossy().into_owned());
        }
        let recent = crate::cmd::compose::recent_files(&s);
        assert_eq!(recent.len(), crate::cmd::compose::MAX_RECENT_FILES);
        assert_eq!(recent[0], paths[29]);
        // The list lives in the settings file, so a restart keeps it.
        assert_eq!(s.settings.list(crate::cmd::compose::RECENT_FILES_KEY).first(), Some(&paths[29]));
        drop(tmp);
    }

    /// The `cid:` sources of the body's pictures.
    fn image_srcs(c: &ComposeState) -> Vec<String> {
        c.images().into_iter().map(|(src, ..)| src).collect()
    }

    #[test]
    fn create_building_block_dialog_categories_and_edit() {
        let (mut s, id) = setup();
        s.run("compose.select", &json!({"compose": id, "all": true})).unwrap();
        // Without a name: the Create New Building Block dialog, the name proposed from the selection.
        let r = s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "autoText"})).unwrap();
        assert_eq!(r["dialog"], "createBuildingBlock");
        let d = last_dialog(&s);
        assert_eq!((d["name"].as_str(), d["category"].as_str()), (Some("start"), Some(DEFAULT_CATEGORY)));
        let r = s
            .run(
                "insert.saveToGallery",
                &json!({"compose": id, "gallery": "autoText", "name": "Sig", "category": "Closings", "description": "short"}),
            )
            .unwrap();
        assert_eq!(r["replaced"], false);
        let r = s
            .run("insert.saveToGallery", &json!({"compose": id, "gallery": "autoText", "name": "SIG"}))
            .unwrap();
        assert_eq!(r["replaced"], true);
        let entries = gallery_entries(&s, id, "autoText");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].category, DEFAULT_CATEGORY,
            "replacing without a category files it under General"
        );
        s.run(
            "insert.saveToGallery",
            &json!({"compose": id, "gallery": "autoText", "name": "Other", "category": "Closings"}),
        )
        .unwrap();
        s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "autoText"})).unwrap();
        assert_eq!(last_dialog(&s)["categories"], json!(["General", "Closings"]));
        // Edit Properties: rename and move to another gallery; a clash is refused.
        assert!(
            s.run("insert.quickPart", &json!({"compose": id, "action": "edit", "name": "Other", "gallery": "autoText", "newName": "sig"}))
                .is_err()
        );
        s.run(
            "insert.quickPart",
            &json!({"compose": id, "action": "edit", "name": "Other", "gallery": "autoText", "newName": "Bye", "newGallery": "quickParts", "description": "d"}),
        )
        .unwrap();
        let qp = gallery_entries(&s, id, "quickParts");
        assert_eq!(
            (qp[0].name.as_str(), qp[0].category.as_str(), qp[0].description.as_str()),
            ("Bye", "Closings", "d")
        );
        assert_eq!(gallery_entries(&s, id, "autoText").len(), 1);
        s.run("insert.quickPart", &json!({"compose": id, "name": "Bye", "gallery": "quickParts"})).unwrap();
        assert!(
            s.run("insert.quickPart", &json!({"compose": id, "action": "edit", "name": "Nope"})).is_err()
        );
        // Nothing selected: disabled.
        s.run("compose.select", &json!({"compose": id, "anchor": {"path": [0], "offset": 0}})).unwrap();
        assert!(s.composes[&id].selection.is_collapsed());
        assert!(s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "autoText"})).is_err());
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
        // Symbol fonts: "Wingdings: 74" is the smiling face.
        let r = s.run("insert.symbol", &json!({"font": "Wingdings", "code": 74})).unwrap();
        assert_eq!(r["inserted"], "\u{263A}");
        assert!(s.run("insert.symbol", &json!({"font": "Wingdings", "code": 48})).is_err());
        assert!(s.run("insert.symbol", &json!({"font": "Wingdings"})).is_err());
        assert_eq!(
            s.run("insert.symbol", &json!({"font": "Symbol", "code": "97"})).unwrap()["inserted"],
            "α"
        );
        s.run("insert.equation", &json!({"preset": "pythagoreanTheorem"})).unwrap();
        assert!(s.composes[&id].images().iter().any(|(_, alt)| alt == "a^2+b^2=c^2"));
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
        assert_eq!(r["editText"], false, "given text is not a placeholder");
        let c = &s.composes[&id];
        let d = c.body.drawings.last().unwrap();
        assert_eq!(
            (d.kind, d.text.as_str(), d.alt.as_str()),
            (mailcraft_model::DrawingKind::WordArt, "Big", "Big")
        );
        // The selected text becomes the WordArt, and leaves the text in the same undo step.
        s.run("compose.select", &json!({"all": true})).unwrap();
        let r = s.run("insert.wordArt", &json!({"style": "Fill: Black, Text color 1; Shadow"})).unwrap();
        assert!(r["text"].as_str().unwrap().contains("tart"));
        assert!(!s.composes[&id].body.to_plain_text().contains("tart"));
        s.run("compose.editUndo", &json!({})).unwrap();
        assert!(s.composes[&id].body.to_plain_text().contains("tart"));
        assert_eq!(s.composes[&id].body.drawings.len(), 1);
        // Without a selection: a placeholder the editor selects for typing over.
        s.run("compose.select", &json!({"anchor": {"path": [0], "offset": 0}})).ok();
        let r = s.run("insert.wordArt", &json!({"style": "Fill: Black, Text color 1; Shadow"})).unwrap();
        assert_eq!((r["text"].as_str(), r["editText"].as_bool()), (Some("Your text here"), Some(true)));
    }

    #[test]
    fn auto_text_starts_with_the_user_and_expands_while_typing() {
        let mut s = session();
        s.settings.set("general.userName", &json!("Ann Example")).unwrap();
        let id = open(&mut s);
        let names: Vec<String> = gallery_entries(&s, id, "autoText").into_iter().map(|b| b.name).collect();
        assert_eq!(names, ["Ann Example", "AE"], "seeded from General > User name and initials");
        // F3 after the start of an entry name expands it; the exact name too.
        s.run("compose.insertText", &json!({"text": "Regards, An"})).unwrap();
        assert!(s.run("insert.autoText", &json!({})).is_err(), "too short to pick by its start");
        s.run("compose.insertText", &json!({"text": "n Exa"})).unwrap();
        let r = s.run("insert.autoText", &json!({"peek": true})).unwrap();
        assert_eq!(r["name"], "Ann Example");
        assert_eq!(r["word"], "Ann Exa", "a name of several words is matched across them");
        assert!(s.composes[&id].body.to_plain_text().trim_end().ends_with("Ann Exa"), "peek inserts nothing");
        s.run("insert.autoText", &json!({})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text().trim_end(), "Regards, Ann Example");
        s.run("compose.insertText", &json!({"text": " ae"})).unwrap();
        s.run("insert.autoText", &json!({})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text().trim_end(), "Regards, Ann Example AE");
        // Undo puts the typed word back.
        s.run("compose.editUndo", &json!({})).unwrap();
        assert!(s.composes[&id].body.to_plain_text().trim_end().ends_with(" ae"));
        // Nothing before the caret.
        s.run("compose.insertText", &json!({"text": " "})).unwrap();
        assert!(s.run("insert.autoText", &json!({})).is_err());
    }

    #[test]
    fn built_in_auto_text_can_be_deleted_and_renamed() {
        let mut s = session();
        s.settings.set("general.userName", &json!("Ann Example")).unwrap();
        let id = open(&mut s);
        let names = |s: &Session| -> Vec<String> {
            gallery_entries(s, id, "autoText").into_iter().map(|b| b.name).collect()
        };
        // Delete a seeded entry: it goes, and stays gone.
        let r = s.run("insert.quickPart", &json!({"name": "AE", "gallery": "autoText", "action": "delete"}));
        assert_eq!(r.unwrap()["deleted"], "AE");
        assert_eq!(names(&s), ["Ann Example"]);
        // Saved under the same name and deleted again: the seeded one does not come back.
        s.run("compose.insertText", &json!({"text": "AE"})).unwrap();
        s.run("compose.select", &json!({"compose": id, "all": true})).unwrap();
        s.run("insert.saveToGallery", &json!({"gallery": "autoText", "name": "AE"})).unwrap();
        assert_eq!(names(&s), ["Ann Example", "AE"]);
        s.run("insert.quickPart", &json!({"name": "AE", "action": "delete"})).unwrap();
        assert_eq!(names(&s), ["Ann Example"]);
        // Rename the other: one entry under the new name, no duplicate under the old one.
        s.run(
            "insert.quickPart",
            &json!({"name": "Ann Example", "gallery": "autoText", "action": "edit", "newName": "Sign-off"}),
        )
        .unwrap();
        assert_eq!(names(&s), ["Sign-off"]);
        // Nothing of that name left to delete.
        assert!(s.run("insert.quickPart", &json!({"name": "Ann Example", "action": "delete"})).is_err());
        // A hostile setting value is ignored, not trusted.
        s.settings.set("state.autoText.hiddenBuiltins", &json!("not a list")).unwrap();
        assert_eq!(names(&s), ["Ann Example", "AE", "Sign-off"]);
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
        for a in &mut s.mailbox.accounts {
            a.sender_name.clear();
        }
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
        let alts: Vec<String> = s.composes[&id].images().into_iter().map(|(_, alt)| alt).collect();
        assert!(alts.contains(&"A=\u{3c0}r^2".to_owned()), "{alts:?}");
        assert_eq!(alts.len(), 5, "each preset is drawn as a picture");
        let r = s
            .run("insert.equation", &json!({"text": "x=(\u{2212}b\u{b1}\u{221a}(b^2\u{2212}4ac))/(2a)"}))
            .unwrap();
        assert_eq!(r["format"], "png");
        assert!(s.run("insert.equation", &json!({"text": "x".repeat(MAX_EQUATION + 1)})).is_err());
        s.run("insert.equation", &json!({})).unwrap();
        assert_eq!(last_dialog(&s)["presets"][0]["name"], "Area of Circle");
        // Screenshot inserts a saved picture.
        let pictures = s.composes[&id].inline_images.len();
        let r = s.run("insert.screenshot", &json!({"name": "shot.png", "data_base64": PNG_1X1})).unwrap();
        assert_eq!(r["screenshot"], true);
        assert_eq!(s.composes[&id].inline_images.len(), pictures + 1);
        assert_eq!(r["alt"], "Screenshot");
        let r = s
            .run(
                "insert.screenshot",
                &json!({"name": "shot.png", "data_base64": PNG_1X1, "window": "Notes - Editor"}),
            )
            .unwrap();
        assert_eq!(r["alt"], "Screenshot", "the window title is not sent in the alt text");
        assert_eq!(r["window"], "Notes - Editor");
        s.run("insert.screenshot", &json!({"window": "Inbox"})).unwrap();
        assert!(last_dialog(&s)["message"].as_str().unwrap().contains("not available here"));
        s.run("insert.screenshot", &json!({})).unwrap();
        assert!(last_dialog(&s)["message"].as_str().unwrap().starts_with("Choose a screenshot"));
        s.run("insert.screenshot", &json!({"clip": true, "unavailable": "The portal said no."})).unwrap();
        let d = last_dialog(&s);
        assert!(d["message"].as_str().unwrap().starts_with("The portal said no."));
        assert_eq!(d["clip"], true);
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

        let cal = s
            .mailbox
            .folders
            .values()
            .find(|f| f.kind == mailcraft_model::FolderKind::Calendar)
            .map(|f| f.id)
            .unwrap();
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
        // The icon is a PNG with the file name under it.
        let icon = s.composes[&id].inline_images.last().unwrap();
        assert_eq!(icon.mime_type, "image/png");
        assert_eq!(s.composes[&id].images().last().unwrap().1, "a.bin");
        // Linked: an icon that links to the file, nothing attached.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.pdf");
        std::fs::write(&path, b"%PDF").unwrap();
        let attached = s.composes[&id].attachments.len();
        let r = s.run("insert.object", &json!({"path": path.to_string_lossy(), "link": true})).unwrap();
        assert!(r["linked"].as_str().unwrap().starts_with("file://"));
        assert_eq!(s.composes[&id].attachments.len(), attached);
        assert!(s.run("insert.object", &json!({"path": "/no/such/file.pdf", "link": true})).is_err());
        // From the native picker: the dialog asks how, with the path filled in.
        let r = s.run("insert.object", &json!({"path": "/x/notes.txt", "ask": true})).unwrap();
        assert_eq!(r["dialog"], "insertObject");
        assert_eq!(
            (last_dialog(&s)["path"].as_str(), last_dialog(&s)["asText"].as_bool()),
            (Some("/x/notes.txt"), Some(true))
        );
    }
}
