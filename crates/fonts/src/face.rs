//! Per-face parsed data (metrics in font units, advance table, charmap) and the outline pen.
//!
//! A [`FaceInfo`] is built once per face (lazily, through a `OnceLock` slot owned by the `FontDb`) and holds
//! everything the hot paths (`advance`, `measure`, `glyph_id`) need, so they never re-parse table directories.

use skrifa::charmap::Charmap;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlineGlyphCollection, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

use crate::PathCmd;

/// Largest glyph count we index (the OpenType limit).
const MAX_GLYPHS: usize = 65_536;
/// Number of leading code points with a precomputed glyph-id table.
pub(crate) const FAST_CHARS: usize = 256;
/// Upper bound on path commands emitted for one glyph (hostile fonts cannot balloon memory).
pub(crate) const MAX_PATH_CMDS: usize = 200_000;

/// Unscaled (font-unit) facts about one face.
#[derive(Clone)]
pub(crate) struct FaceInfo {
    /// Address and length of the data this info was built from (detects a replaced `Face`).
    data_addr: usize,
    data_len: usize,
    index: u32,
    pub font: Option<FontRef<'static>>,
    pub charmap: Option<Charmap<'static>>,
    /// Outline source, built once (it parses the glyph/CFF table headers).
    pub outlines: Option<OutlineGlyphCollection<'static>>,
    pub units_per_em: f32,
    /// Ascent above the baseline (positive).
    pub ascent: f32,
    /// Descent below the baseline (positive).
    pub descent: f32,
    pub line_gap: f32,
    /// Distance from the baseline down to the top of the underline (positive = below).
    pub underline_pos: f32,
    pub underline_thickness: f32,
    /// Advance width per glyph id, font units.
    pub advances: Vec<f32>,
    /// Glyph ids for U+0000..U+00FF.
    pub fast_glyphs: Vec<u32>,
    /// The OS/2 table's embedding permissions (`fsType`; 0 when the face has no OS/2 table).
    pub fs_type: u16,
}

impl std::fmt::Debug for FaceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaceInfo")
            .field("parsed", &self.font.is_some())
            .field("units_per_em", &self.units_per_em)
            .field("ascent", &self.ascent)
            .field("descent", &self.descent)
            .field("line_gap", &self.line_gap)
            .field("glyphs", &self.advances.len())
            .finish()
    }
}

fn finite_or(v: f32, default: f32) -> f32 {
    if v.is_finite() { v } else { default }
}

impl FaceInfo {
    /// Info for data that is not a usable font: synthetic proportions, no glyphs.
    fn placeholder(data: &'static [u8], index: u32) -> FaceInfo {
        FaceInfo {
            data_addr: data.as_ptr() as usize,
            data_len: data.len(),
            index,
            font: None,
            charmap: None,
            outlines: None,
            units_per_em: 1000.0,
            ascent: 800.0,
            descent: 200.0,
            line_gap: 0.0,
            underline_pos: 100.0,
            underline_thickness: 50.0,
            advances: Vec::new(),
            fast_glyphs: Vec::new(),
            fs_type: 0,
        }
    }

    /// Parses face `index` of `data` (0 for a single font; collections hold several).
    pub fn parse(data: &'static [u8], index: u32) -> FaceInfo {
        let Ok(font) = FontRef::from_index(data, index) else {
            return FaceInfo::placeholder(data, index);
        };
        let m = font.metrics(Size::unscaled(), LocationRef::default());
        let upem = match m.units_per_em {
            16..=16_384 => f32::from(m.units_per_em),
            _ => 1000.0,
        };
        let mut ascent = finite_or(m.ascent, 0.0).clamp(-4.0 * upem, 4.0 * upem);
        let mut descent = finite_or(-m.descent, 0.0).clamp(-4.0 * upem, 4.0 * upem);
        if ascent <= 0.0 && descent <= 0.0 {
            ascent = 0.8 * upem;
            descent = 0.2 * upem;
        }
        // Negative values come from broken fonts; clamp so line height stays positive.
        ascent = ascent.max(0.0);
        descent = descent.max(0.0);
        let line_gap = finite_or(m.leading, 0.0).clamp(0.0, 2.0 * upem);
        let default_thickness = upem / 14.0;
        let (underline_pos, underline_thickness) = match m.underline {
            Some(d) if d.thickness.is_finite() && d.thickness > 0.0 && d.offset.is_finite() => {
                ((-d.offset).clamp(0.0, upem), d.thickness.min(upem / 4.0))
            }
            _ => (upem / 10.0, default_thickness),
        };
        let glyph_count = usize::from(m.glyph_count).min(MAX_GLYPHS);
        let gm = font.glyph_metrics(Size::unscaled(), LocationRef::default());
        let advances: Vec<f32> = (0..glyph_count)
            .map(|g| {
                let gid = GlyphId::new(u32::try_from(g).unwrap_or(0));
                let adv = gm.advance_width(gid).unwrap_or(0.0);
                finite_or(adv, 0.0).clamp(0.0, 64.0 * upem)
            })
            .collect();
        let charmap = font.charmap();
        let fast_glyphs = (0..FAST_CHARS)
            .map(|c| {
                let cp = u32::try_from(c).unwrap_or(0);
                charmap.map(cp).map(u32::from).unwrap_or(0)
            })
            .collect();
        let outlines = Some(font.outline_glyphs());
        let fs_type = {
            use skrifa::raw::TableProvider;
            font.os2().map(|t| t.fs_type()).unwrap_or(0)
        };
        FaceInfo {
            data_addr: data.as_ptr() as usize,
            data_len: data.len(),
            index,
            font: Some(font),
            charmap: Some(charmap),
            outlines,
            units_per_em: upem,
            ascent,
            descent,
            line_gap,
            underline_pos,
            underline_thickness,
            advances,
            fast_glyphs,
            fs_type,
        }
    }

    /// Whether the face's licence lets a document carry its outlines (OpenType OS/2 `fsType`): not when it is
    /// marked Restricted License embedding only (usage bits 1–3 = 2; when several are set the least
    /// restrictive wins) or bitmap embedding only (bit 9).
    pub fn embeddable(&self) -> bool {
        self.fs_type & 0x000e != 0x0002 && self.fs_type & 0x0200 == 0
    }

    /// True if this info was built from exactly face `index` of `data`.
    pub fn matches(&self, data: &'static [u8], index: u32) -> bool {
        self.data_addr == data.as_ptr() as usize && self.data_len == data.len() && self.index == index
    }

    /// Glyph id mapped for `c`, 0 when unmapped.
    pub fn glyph(&self, c: char) -> u32 {
        let cp = u32::from(c);
        if let Some(g) = usize::try_from(cp).ok().and_then(|i| self.fast_glyphs.get(i)) {
            return *g;
        }
        self.charmap.as_ref().and_then(|cm| cm.map(cp)).map(u32::from).unwrap_or(0)
    }

    /// Advance of a glyph in font units; glyphs outside the table measure as .notdef (or half an em when the
    /// face has no glyphs at all).
    pub fn glyph_advance_units(&self, glyph: u32) -> f32 {
        let idx = usize::try_from(glyph).unwrap_or(usize::MAX);
        self.advances.get(idx).or_else(|| self.advances.first()).copied().unwrap_or(self.units_per_em * 0.5)
    }

    pub fn scale(&self, size_px: f32) -> f32 {
        size_px / self.units_per_em
    }

    /// Draws `glyph` at `size_px` into y-down path commands. Empty on any failure.
    pub fn outline(&self, glyph: u32, size_px: f32) -> Vec<PathCmd> {
        let Some(outlines) = &self.outlines else {
            return Vec::new();
        };
        if usize::try_from(glyph).map_or(true, |g| g >= self.advances.len()) {
            return Vec::new();
        }
        let Some(og) = outlines.get(GlyphId::new(glyph)) else {
            return Vec::new();
        };
        let mut pen = FlipPen::default();
        let settings = DrawSettings::unhinted(Size::new(size_px), LocationRef::default());
        match og.draw(settings, &mut pen) {
            Ok(_) if !pen.overflow => pen.cmds,
            _ => Vec::new(),
        }
    }
}

/// Collects skrifa's y-up pen calls as y-down [`PathCmd`]s.
#[derive(Default)]
struct FlipPen {
    cmds: Vec<PathCmd>,
    overflow: bool,
}

impl FlipPen {
    fn push(&mut self, cmd: PathCmd) {
        if self.cmds.len() >= MAX_PATH_CMDS {
            self.overflow = true;
        } else {
            self.cmds.push(cmd);
        }
    }
}

impl OutlinePen for FlipPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.push(PathCmd::MoveTo(x, -y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.push(PathCmd::LineTo(x, -y));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.push(PathCmd::QuadTo(cx0, -cy0, x, -y));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.push(PathCmd::CubicTo(cx0, -cy0, cx1, -cy1, x, -y));
    }
    fn close(&mut self) {
        self.push(PathCmd::Close);
    }
}
