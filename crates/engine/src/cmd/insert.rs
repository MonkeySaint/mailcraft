//! `insert.*`: tables, pictures and generated graphics, links, bookmarks, symbols, building blocks, cards and
//! calendars in the compose body. Graphics the body model cannot hold natively (shapes, icons, charts,
//! SmartArt, 3D models) are rendered to inline SVG images (`cid:` parts when sent). Commands that need an
//! OS facility (screen capture) or a picker without params open a dialog instead.

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

fn star_points(cx: f32, cy: f32, r_out: f32, r_in: f32, n: usize) -> String {
    let mut pts = String::new();
    let total = n.saturating_mul(2);
    for i in 0..total {
        #[allow(clippy::cast_precision_loss)]
        let a = std::f32::consts::PI * (i as f32) / (n as f32) - std::f32::consts::FRAC_PI_2;
        let r = if i % 2 == 0 { r_out } else { r_in };
        let _ = write!(pts, "{:.1},{:.1} ", cx + r * a.cos(), cy + r * a.sin());
    }
    pts
}

fn polygon_points(cx: f32, cy: f32, r: f32, n: usize) -> String {
    let mut pts = String::new();
    for i in 0..n {
        #[allow(clippy::cast_precision_loss)]
        let a = 2.0 * std::f32::consts::PI * (i as f32) / (n as f32) - std::f32::consts::FRAC_PI_2;
        let _ = write!(pts, "{:.1},{:.1} ", cx + r * a.cos(), cy + r * a.sin());
    }
    pts
}

/// SVG body (without the outer element) for a shape kind in a 160×100 box.
fn shape_svg(kind: &str) -> Option<String> {
    let st = r##"fill="#156082" stroke="#0e3f56" stroke-width="2""##;
    let line = r##"stroke="#156082" stroke-width="3" fill="none""##;
    Some(match kind {
        "rectangle" => format!(r#"<rect x="4" y="4" width="152" height="92" {st}/>"#),
        "roundedRectangle" => format!(r#"<rect x="4" y="4" width="152" height="92" rx="16" {st}/>"#),
        "oval" | "ellipse" => format!(r#"<ellipse cx="80" cy="50" rx="76" ry="46" {st}/>"#),
        "circle" => format!(r#"<circle cx="80" cy="50" r="46" {st}/>"#),
        "triangle" => format!(r#"<polygon points="80,4 156,96 4,96" {st}/>"#),
        "rightTriangle" => format!(r#"<polygon points="4,4 156,96 4,96" {st}/>"#),
        "diamond" => format!(r#"<polygon points="80,4 156,50 80,96 4,50" {st}/>"#),
        "pentagon" => format!(r#"<polygon points="{}" {st}/>"#, polygon_points(80.0, 52.0, 46.0, 5)),
        "hexagon" => format!(r#"<polygon points="{}" {st}/>"#, polygon_points(80.0, 50.0, 46.0, 6)),
        "star" => format!(r#"<polygon points="{}" {st}/>"#, star_points(80.0, 52.0, 48.0, 20.0, 5)),
        "line" => format!(r#"<line x1="4" y1="96" x2="156" y2="4" {line}/>"#),
        "arrow" | "rightArrow" => format!(r#"<polygon points="4,32 110,32 110,8 156,50 110,92 110,68 4,68" {st}/>"#),
        "leftArrow" => format!(r#"<polygon points="156,32 50,32 50,8 4,50 50,92 50,68 156,68" {st}/>"#),
        "heart" => format!(r#"<path d="M80 92 C20 60 4 36 24 16 C40 2 64 6 80 26 C96 6 120 2 136 16 C156 36 140 60 80 92 Z" {st}/>"#),
        "callout" => format!(r#"<path d="M8 8 H152 V70 H60 L36 94 L40 70 H8 Z" {st}/>"#),
        "canvas" => r##"<rect x="1" y="1" width="158" height="98" fill="#ffffff" stroke="#a0a0a0" stroke-dasharray="4 3"/>"##.to_owned(),
        _ => return None,
    })
}

const SHAPES: &str = "rectangle, roundedRectangle, oval, circle, triangle, rightTriangle, diamond, pentagon, hexagon, star, line, arrow, leftArrow, heart, callout, canvas";

fn shape(s: &mut Session, p: &Value) -> CmdResult {
    let kind = req_str(p, "kind")?;
    let Some(body) = shape_svg(kind) else {
        return Err(CmdError::Params(format!("unknown shape `{kind}` (known: {SHAPES})")));
    };
    let svg = format!("{}{body}</svg>", svg_open(160.0, 100.0));
    let kind = kind.to_owned();
    let c = compose_mut(s, p)?;
    let cid = put_svg(c, &kind, svg, &kind, 160.0, 100.0);
    Ok(json!({"shape": kind, "cid": cid}))
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

#[allow(clippy::cast_precision_loss)]
fn smart_art_svg(layout: &str, items: &[String]) -> Option<(String, f32, f32)> {
    let n = items.len();
    let box_ = |svg: &mut String, x: f32, y: f32, w: f32, h: f32, i: usize, t: &str| {
        let col = PALETTE.get(i % PALETTE.len()).copied().unwrap_or("#156082");
        let _ = write!(
            svg,
            r#"<rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" rx="6" fill="{col}"/>"#
        );
        let _ = write!(
            svg,
            r##"<text x="{:.1}" y="{:.1}" font-family="sans-serif" font-size="13" text-anchor="middle" fill="#fff">{}</text>"##,
            x + w / 2.0,
            y + h / 2.0 + 4.0,
            esc(t)
        );
    };
    match layout {
        "list" => {
            let (w, h) = (360.0, 20.0 + 50.0 * n as f32);
            let mut svg = svg_open(w, h);
            for (i, t) in items.iter().enumerate() {
                box_(&mut svg, 10.0, 10.0 + 50.0 * i as f32, 340.0, 40.0, i, t);
            }
            svg.push_str("</svg>");
            Some((svg, w, h))
        }
        "process" => {
            let (w, h) = (20.0 + 150.0 * n as f32, 80.0);
            let mut svg = svg_open(w, h);
            for (i, t) in items.iter().enumerate() {
                let x = 10.0 + 150.0 * i as f32;
                box_(&mut svg, x, 15.0, 115.0, 50.0, i, t);
                if i.saturating_add(1) < n {
                    let _ = write!(
                        svg,
                        r##"<polygon points="{:.1},32 {:.1},40 {:.1},48" fill="#9e9e9e"/>"##,
                        x + 120.0,
                        x + 142.0,
                        x + 120.0
                    );
                }
            }
            svg.push_str("</svg>");
            Some((svg, w, h))
        }
        "cycle" => {
            let (w, h) = (420.0, 420.0);
            let mut svg = svg_open(w, h);
            svg.push_str(
                r##"<circle cx="210" cy="210" r="140" fill="none" stroke="#bdbdbd" stroke-width="4"/>"##,
            );
            for (i, t) in items.iter().enumerate() {
                let a = std::f32::consts::TAU * i as f32 / n.max(1) as f32 - std::f32::consts::FRAC_PI_2;
                box_(
                    &mut svg,
                    210.0 + 140.0 * a.cos() - 55.0,
                    210.0 + 140.0 * a.sin() - 22.0,
                    110.0,
                    44.0,
                    i,
                    t,
                );
            }
            svg.push_str("</svg>");
            Some((svg, w, h))
        }
        "hierarchy" => {
            let kids = n.saturating_sub(1).max(1);
            let w = (20.0 + 140.0 * kids as f32).max(200.0);
            let h = 170.0;
            let mut svg = svg_open(w, h);
            let top = items.first()?;
            box_(&mut svg, w / 2.0 - 60.0, 10.0, 120.0, 44.0, 0, top);
            for (i, t) in items.iter().enumerate().skip(1) {
                let x = 10.0 + 140.0 * (i - 1) as f32;
                let _ = write!(
                    svg,
                    r##"<path d="M{:.1} 54 V80 H{:.1} V110" stroke="#9e9e9e" fill="none" stroke-width="2"/>"##,
                    w / 2.0,
                    x + 60.0
                );
                box_(&mut svg, x, 110.0, 120.0, 44.0, i, t);
            }
            svg.push_str("</svg>");
            Some((svg, w, h))
        }
        _ => None,
    }
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
    let Some((svg, w, h)) = smart_art_svg(&layout, &items) else {
        return Err(CmdError::Params(format!("unknown layout `{layout}` (list, process, cycle, hierarchy)")));
    };
    let alt = items.join(", ");
    let c = compose_mut(s, p)?;
    let cid = put_svg(c, "smartart", svg, &alt, w, h);
    Ok(json!({"layout": layout, "items": items.len(), "cid": cid}))
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

fn screenshot(s: &mut Session, p: &Value) -> CmdResult {
    // Capturing the screen is the UI's job; it inserts the capture with insert.picture {name, data_base64}.
    let window = opt_str(p, "window");
    let clip = opt_bool(p, "clip").unwrap_or(false);
    dialog(s, "screenshot", json!({"compose": compose_id(s, p)?, "window": window, "clip": clip}))
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

/// Built-in equations (standard formulas written in plain math notation).
fn equation_preset(name: &str) -> Option<&'static str> {
    Some(match name {
        "areaOfCircle" => "A = \u{3c0}r\u{b2}",
        "pythagoreanTheorem" => "a\u{b2} + b\u{b2} = c\u{b2}",
        "quadraticFormula" => "x = (\u{2212}b \u{b1} \u{221a}(b\u{b2} \u{2212} 4ac)) / 2a",
        "binomialTheorem" => {
            "(x + a)\u{207f} = \u{3a3}\u{2096}\u{208c}\u{2080}\u{207f} C(n,k) x\u{1d4f} a\u{207f}\u{207b}\u{1d4f}"
        }
        "expansionOfSum" => "(1 + x)\u{207f} = 1 + nx/1! + n(n \u{2212} 1)x\u{b2}/2! + \u{2026}",
        "eulerIdentity" => "e^(i\u{3c0}) + 1 = 0",
        _ => return None,
    })
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
                json!({"compose": compose_id(s, p)?, "presets": ["areaOfCircle", "pythagoreanTheorem", "quadraticFormula", "binomialTheorem", "expansionOfSum", "eulerIdentity"]}),
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

fn find_block(s: &Session, cid: u64, gallery: Option<&str>, name: &str) -> Option<BuildingBlock> {
    let own = s.composes.get(&cid).into_iter();
    let others = s.composes.iter().filter(|(k, _)| **k != cid).map(|(_, c)| c);
    own.chain(others)
        .flat_map(|c| c.building_blocks.iter().rev())
        .find(|b| b.name.eq_ignore_ascii_case(name) && gallery.is_none_or(|g| b.gallery == g))
        .cloned()
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
    let Some(name) = opt_str(p, "name").map(str::to_owned) else {
        let cid = compose_id(s, p)?;
        let entries: Vec<Value> = s
            .composes
            .get(&cid)
            .map(|c| {
                c.building_blocks.iter().map(|b| json!({"gallery": b.gallery, "name": b.name})).collect()
            })
            .unwrap_or_default();
        return dialog(s, "quickParts", json!({"compose": cid, "entries": entries}));
    };
    let gallery = opt_str(p, "gallery");
    if let Some(g) = gallery
        && !GALLERIES.contains(&g)
    {
        return Err(CmdError::Params(format!("unknown gallery `{g}`")));
    }
    let cid = compose_id(s, p)?;
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
    // Every open compose shares the galleries.
    for other in s.composes.values_mut() {
        other
            .building_blocks
            .retain(|b| !(b.gallery == entry.gallery && b.name.eq_ignore_ascii_case(&entry.name)));
        other.building_blocks.push(entry.clone());
    }
    Ok(json!({"saved": name, "gallery": gallery}))
}

/// Text colour of a WordArt preset from the colour named after "Fill" (falls back to the accent).
fn wordart_color(style: &str) -> Color {
    let after = style.split_once("Fill").map_or(style, |(_, r)| r);
    let word: String = after
        .trim_start_matches([':', ',', ' '])
        .split([',', ';'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_alphabetic())
        .collect::<String>()
        .to_lowercase();
    mailcraft_html::parse_css_color(&word).unwrap_or(Color::rgb(0x15, 0x60, 0x82))
}

fn word_art(s: &mut Session, p: &Value) -> CmdResult {
    let style = req_str(p, "style")?.to_owned();
    let color = wordart_color(&style);
    let text = opt_str(p, "text").map(str::to_owned);
    let c = compose_mut(s, p)?;
    c.checkpoint();
    if c.selection.is_collapsed() {
        let shown = text.unwrap_or_else(|| "WordArt".to_owned());
        let start = c.selection.focus.clone();
        c.insert_text(&shown);
        let end = c.selection.focus.clone();
        c.selection = crate::compose::BodySelection { anchor: start, focus: end };
    }
    c.apply_text_style(|st| {
        st.size_pt = Some(36.0);
        st.bold = Some(true);
        st.color = Some(color);
    });
    Ok(json!({"wordArt": style, "color": color.to_hex()}))
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
            r#"{"name"?:string,"gallery"?:string}"#,
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
            r#"{"window"?:string,"clip"?:bool}"#,
            false,
            rich_compose,
            screenshot,
        ),
        spec(
            "insert.shape",
            "Shapes",
            "Insert › Illustrations",
            "",
            r#"{"kind":string}"#,
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
        assert!(c.inline_images.iter().all(|i| i.mime_type == "image/svg+xml"));
        assert_eq!(s.run("insert.screenshot", &json!({"clip": true})).unwrap()["dialog"], "screenshot");
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
        s.run(
            "insert.wordArt",
            &json!({"style": "Fill: Orange, Accent color 2; Outline: Orange, Accent color 2", "text": "Big"}),
        )
        .unwrap();
        assert!(s.composes[&id].body.to_plain_text().contains("Big"));
        assert!(
            s.composes[&id]
                .selection_all(|st, _| st.size_pt == Some(36.0) && st.color == Some(Color::rgb(255, 165, 0)))
        );
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
