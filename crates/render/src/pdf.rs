//! PDF output for print jobs (layer L3): writes [`crate::print::Page`]s to a PDF 1.7 file with `krilla`, which
//! embeds the bundled TrueType faces subset to the glyphs used (CID fonts with a ToUnicode map, so text can be
//! selected and searched).
//!
//! Page coordinates are logical px (1/96 in); the writer scales them to PDF points (1/72 in). Text runs are
//! split by fallback face exactly as the rasteriser does ([`FontDb::fallback_for`]), so the PDF and the print
//! preview show the same glyphs. Images are drawn as placeholders (frame + alt text), like the preview.
//!
//! Hostile display lists are tolerated: non-finite or absurd coordinates and sizes are skipped, runs are capped.

use std::collections::HashMap;

use krilla::Document;
use krilla::color::rgb;
use krilla::geom::{PathBuilder, Point as KPoint, Rect as KRect, Transform};
use krilla::metadata::Metadata;
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::{Fill, Stroke};
use krilla::surface::Surface;
use krilla::text::{Font, GlyphId, KrillaGlyph};
use mailcraft_fonts::{FontDb, FontId, is_zero_width};
use mailcraft_geom::{Color, Point, Rect};
use mailcraft_layout::DisplayItem;

use crate::print::Page;

/// PDF points per logical px.
const PT_PER_PX: f32 = 0.75;
/// Coordinates beyond this many px from the origin are skipped.
const MAX_COORD: f32 = 1.0e6;
/// Largest text size written, in px.
const MAX_TEXT_PX: f32 = 4096.0;
/// Most glyphs written for one text item.
const MAX_RUN_GLYPHS: usize = 16_384;
/// Most pages written.
pub const MAX_PDF_PAGES: usize = crate::print::MAX_PAGES;

/// Document information.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PdfInfo {
    pub title: String,
    pub author: String,
}

/// Why a PDF could not be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfError(pub String);

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not write the PDF: {}", self.0)
    }
}

impl std::error::Error for PdfError {}

/// Writes `pages` as a PDF document.
pub fn write_pdf(pages: &[Page], fonts: &FontDb, info: &PdfInfo) -> Result<Vec<u8>, PdfError> {
    let mut doc = Document::new();
    let mut meta = Metadata::new().creator("MailCraft".to_owned());
    if !info.title.trim().is_empty() {
        meta = meta.title(info.title.chars().take(1_000).collect());
    }
    if !info.author.trim().is_empty() {
        meta = meta.authors(vec![info.author.chars().take(1_000).collect()]);
    }
    doc.set_metadata(meta);
    let mut cache: HashMap<FontId, Option<Font>> = HashMap::new();
    for page in pages.iter().take(MAX_PDF_PAGES) {
        let ok = |v: f32| v.is_finite() && v > 0.0 && v <= MAX_COORD;
        let (w, h) =
            if ok(page.width) && ok(page.height) { (page.width, page.height) } else { (816.0, 1056.0) };
        let Some(settings) = PageSettings::from_wh(w * PT_PER_PX, h * PT_PER_PX) else {
            return Err(PdfError("bad page size".into()));
        };
        let mut p = doc.start_page_with(settings);
        let mut surface = p.surface();
        surface.push_transform(&Transform::from_scale(PT_PER_PX, PT_PER_PX));
        let mut painter = Painter { s: &mut surface, fonts, cache: &mut cache };
        for item in &page.display.items {
            painter.item(item);
        }
        surface.pop();
        surface.finish();
        p.finish();
    }
    doc.finish().map_err(|e| PdfError(format!("{e:?}").chars().take(500).collect()))
}

fn finite(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_finite() && x.abs() <= MAX_COORD)
}

fn fill_of(c: Color) -> Fill {
    Fill {
        paint: rgb::Color::new(c.r, c.g, c.b).into(),
        opacity: NormalizedF32::new(f32::from(c.a) / 255.0).unwrap_or(NormalizedF32::ONE),
        ..Fill::default()
    }
}

fn stroke_of(c: Color, width: f32) -> Stroke {
    Stroke {
        paint: rgb::Color::new(c.r, c.g, c.b).into(),
        width,
        opacity: NormalizedF32::new(f32::from(c.a) / 255.0).unwrap_or(NormalizedF32::ONE),
        ..Stroke::default()
    }
}

struct Painter<'a, 'b, 'c> {
    s: &'a mut Surface<'b>,
    fonts: &'c FontDb,
    cache: &'a mut HashMap<FontId, Option<Font>>,
}

impl Painter<'_, '_, '_> {
    fn item(&mut self, item: &DisplayItem) {
        match item {
            DisplayItem::FillRect { rect, color } => self.fill_rect(*rect, *color),
            DisplayItem::StrokeRect { rect, color, width } => self.stroke_rect(*rect, *color, *width),
            DisplayItem::Line { from, to, color, width } => self.line(*from, *to, *color, *width),
            DisplayItem::Text { origin, text, font, size_px, color, underline, strike, .. } => {
                self.text(*origin, text, *font, *size_px, *color, *underline, *strike);
            }
            DisplayItem::Image { rect, alt, .. } => self.placeholder(*rect, alt),
        }
    }

    fn fill_rect(&mut self, r: Rect, color: Color) {
        if color.a == 0 || !finite(&[r.x, r.y, r.w, r.h]) || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let Some(kr) = KRect::from_xywh(r.x, r.y, r.w, r.h) else { return };
        let mut pb = PathBuilder::new();
        pb.push_rect(kr);
        let Some(path) = pb.finish() else { return };
        self.s.set_stroke(None);
        self.s.set_fill(Some(fill_of(color)));
        self.s.draw_path(&path);
    }

    /// Border painted inside the rectangle (as four filled bands, like the rasteriser).
    fn stroke_rect(&mut self, r: Rect, color: Color, width: f32) {
        if !finite(&[r.x, r.y, r.w, r.h, width]) || width <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let t = width.min(r.w / 2.0).min(r.h / 2.0);
        self.fill_rect(Rect::new(r.x, r.y, r.w, t), color);
        self.fill_rect(Rect::new(r.x, r.y + r.h - t, r.w, t), color);
        self.fill_rect(Rect::new(r.x, r.y + t, t, r.h - 2.0 * t), color);
        self.fill_rect(Rect::new(r.x + r.w - t, r.y + t, t, r.h - 2.0 * t), color);
    }

    fn line(&mut self, a: Point, b: Point, color: Color, width: f32) {
        if color.a == 0 || !finite(&[a.x, a.y, b.x, b.y, width]) || width <= 0.0 {
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(a.x, a.y);
        pb.line_to(b.x, b.y);
        let Some(path) = pb.finish() else { return };
        self.s.set_fill(None);
        self.s.set_stroke(Some(stroke_of(color, width)));
        self.s.draw_path(&path);
        self.s.set_stroke(None);
    }

    fn font(&mut self, id: FontId) -> Option<Font> {
        let fonts = self.fonts;
        self.cache
            .entry(id)
            .or_insert_with(|| {
                let data = fonts.data(id);
                if data.is_empty() { None } else { Font::new(data.into(), 0) }
            })
            .clone()
    }

    fn text(
        &mut self,
        origin: Point,
        text: &str,
        font: FontId,
        size_px: f32,
        color: Color,
        underline: bool,
        strike: bool,
    ) {
        if color.a == 0 || text.is_empty() || !finite(&[origin.x, origin.y, size_px]) || size_px <= 0.0 {
            return;
        }
        let size = size_px.min(MAX_TEXT_PX);
        // Split into runs of one face each: (face, run start byte, glyphs with ranges relative to the run).
        let mut runs: Vec<(FontId, usize, usize, Vec<KrillaGlyph>)> = Vec::new();
        let mut count = 0usize;
        for (i, c) in text.char_indices() {
            let end = i + c.len_utf8();
            if is_zero_width(c) {
                // Attach to the previous glyph so the text mapping keeps the character.
                if let Some(r) = runs.last_mut() {
                    r.2 = end;
                    let base = r.1;
                    if let Some(g) = r.3.last_mut() {
                        g.text_range.end = end - base;
                    }
                }
                continue;
            }
            count += 1;
            if count > MAX_RUN_GLYPHS {
                break;
            }
            let face = self.fonts.fallback_for(font, c);
            let gid = self.fonts.glyph_id(face, c);
            let adv = self.fonts.glyph_advance(face, gid, size) / size;
            let adv = if adv.is_finite() { adv } else { 0.0 };
            let start_new = runs.last().is_none_or(|r| r.0 != face);
            if start_new {
                runs.push((face, i, end, Vec::new()));
            }
            if let Some(r) = runs.last_mut() {
                r.2 = end;
                let base = r.1;
                r.3.push(KrillaGlyph::new(GlyphId::new(gid), adv, 0.0, 0.0, 0.0, i - base..end - base, None));
            }
        }
        self.s.set_stroke(None);
        self.s.set_fill(Some(fill_of(color)));
        let mut x = origin.x;
        for (face, start, end, glyphs) in &runs {
            let Some(f) = self.font(*face) else { continue };
            let Some(run_text) = text.get(*start..*end) else { continue };
            self.s.draw_glyphs(KPoint::from_xy(x, origin.y), glyphs, f, run_text, size, false);
            x += glyphs.iter().map(|g| g.x_advance * size).sum::<f32>();
        }
        let width = x - origin.x;
        if (underline || strike) && width > 0.0 {
            let m = self.fonts.metrics(font, size);
            let t = m.underline_thickness.max(size / 18.0);
            if underline {
                self.fill_rect(
                    Rect::new(origin.x, origin.y + m.underline_pos.abs().max(1.0), width, t),
                    color,
                );
            }
            if strike {
                self.fill_rect(Rect::new(origin.x, origin.y - m.ascent * 0.3, width, t), color);
            }
        }
    }

    fn placeholder(&mut self, r: Rect, alt: &str) {
        let frame = Color::rgb(0xc6, 0xc6, 0xc6);
        self.stroke_rect(r, frame, 1.0);
        if !alt.trim().is_empty() && r.w > 20.0 && r.h > 16.0 {
            let font = self.fonts.resolve(mailcraft_fonts::UI_FAMILY, 400, false);
            let size = 11.0;
            let mut s = String::new();
            let mut w = 0.0;
            for c in alt.chars().take(400) {
                let cw = self.fonts.advance(font, c, size);
                if w + cw > r.w - 8.0 {
                    break;
                }
                w += cw;
                s.push(c);
            }
            self.text(
                Point::new(r.x + 4.0, r.y + 4.0 + size),
                &s,
                font,
                size,
                Color::rgb(0x59, 0x59, 0x59),
                false,
                false,
            );
        }
    }
}
