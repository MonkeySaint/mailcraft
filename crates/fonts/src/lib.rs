//! Bundled fonts and font matching (layer L1).
//!
//! Bundled (all SIL OFL 1.1, files under `assets/fonts/` with their licence texts): Inter (UI; stands in for
//! the system UI font and for "Aptos"), Liberation Sans / Serif / Mono (metric-compatible with Arial, Times New
//! Roman, Courier New), Carlito (metric-compatible with Calibri). Fonts are embedded with `include_bytes!`.
//!
//! Interface (frozen for commit 2):
//! - [`FontDb::bundled`], [`FontDb::resolve`] (CSS family list + weight + italic → [`FontId`]),
//!   [`FontDb::metrics`], [`FontDb::advance`], [`FontDb::measure`], [`FontDb::data`], [`FontDb::glyph_id`],
//!   [`FontDb::outline`] (glyph path for offscreen rendering).

/// Index into [`FontDb`]'s face list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontId(pub u16);

/// Vertical metrics scaled to a pixel size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineMetrics {
    pub ascent: f32,
    /// Positive distance below the baseline.
    pub descent: f32,
    pub line_gap: f32,
    /// Distance from the baseline down to the top of the underline (positive = below the baseline).
    pub underline_pos: f32,
    pub underline_thickness: f32,
}

impl LineMetrics {
    pub fn line_height(&self) -> f32 {
        self.ascent + self.descent + self.line_gap
    }
}

/// One path command of a glyph outline in pixel space (y down, origin at the pen position on the baseline).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCmd {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    QuadTo(f32, f32, f32, f32),
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Clone, Debug)]
pub struct Face {
    pub family: &'static str,
    /// CSS weight (400 regular, 600 semibold, 700 bold).
    pub weight: u16,
    pub italic: bool,
    pub data: &'static [u8],
}

/// Family names used by the UI.
pub const UI_FAMILY: &str = "Inter";
/// Compose body default (the observed client shows "Aptos (Body)" 12 pt; we map Aptos to a bundled sans).
pub const BODY_FAMILY: &str = "Aptos";
pub const BODY_SIZE_PT: f32 = 12.0;

mod face;
mod resolve;
#[cfg(test)]
mod tests;

use std::sync::OnceLock;

use face::FaceInfo;
pub use resolve::bundled_family_for;

macro_rules! font_file {
    ($name:literal) => {
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/", $name))
    };
}

/// The bundled faces in [`FontId`] order. Index 0 (Inter Regular) is the last-resort face.
const BUNDLED: &[(&str, u16, bool, &[u8])] = &[
    (resolve::INTER, 400, false, font_file!("Inter-Regular.ttf")),
    (resolve::INTER, 500, false, font_file!("Inter-Medium.ttf")),
    (resolve::INTER, 600, false, font_file!("Inter-SemiBold.ttf")),
    (resolve::LIB_SANS, 400, false, font_file!("LiberationSans-Regular.ttf")),
    (resolve::LIB_SANS, 700, false, font_file!("LiberationSans-Bold.ttf")),
    (resolve::LIB_SANS, 400, true, font_file!("LiberationSans-Italic.ttf")),
    (resolve::LIB_SANS, 700, true, font_file!("LiberationSans-BoldItalic.ttf")),
    (resolve::LIB_SERIF, 400, false, font_file!("LiberationSerif-Regular.ttf")),
    (resolve::LIB_SERIF, 700, false, font_file!("LiberationSerif-Bold.ttf")),
    (resolve::LIB_SERIF, 400, true, font_file!("LiberationSerif-Italic.ttf")),
    (resolve::LIB_MONO, 400, false, font_file!("LiberationMono-Regular.ttf")),
    (resolve::LIB_MONO, 700, false, font_file!("LiberationMono-Bold.ttf")),
    (resolve::CARLITO, 400, false, font_file!("Carlito-Regular.ttf")),
    (resolve::CARLITO, 700, false, font_file!("Carlito-Bold.ttf")),
    (resolve::CARLITO, 400, true, font_file!("Carlito-Italic.ttf")),
];

/// Licence texts shipped with the bundled fonts: (family, SIL OFL 1.1 text).
pub const LICENSES: &[(&str, &str)] = &[
    ("Inter", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/OFL-Inter.txt"))),
    (
        "Liberation",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/OFL-Liberation.txt")),
    ),
    ("Carlito", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/OFL-Carlito.txt"))),
];

/// Largest pixel size honoured by the measuring and outline functions (larger sizes are clamped).
pub const MAX_SIZE_PX: f32 = 100_000.0;

/// What a renderer must fake because the chosen face does not carry the requested style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Synthesis {
    /// Requested weight is bold-ish (>= 600) but the face is at most 500: embolden.
    pub bold: bool,
    /// Italic requested but the face is upright: slant.
    pub oblique: bool,
}

/// Cached face info, or a temporary one when the cache slot is missing or stale.
enum InfoRef<'a> {
    Cached(&'a FaceInfo),
    Owned(Box<FaceInfo>),
}

impl std::ops::Deref for InfoRef<'_> {
    type Target = FaceInfo;
    fn deref(&self) -> &FaceInfo {
        match self {
            InfoRef::Cached(i) => i,
            InfoRef::Owned(i) => i,
        }
    }
}

fn sanitize_size(size_px: f32) -> f32 {
    if size_px.is_finite() && size_px > 0.0 { size_px.min(MAX_SIZE_PX) } else { 0.0 }
}

/// Characters that never take horizontal space in measurement: C0/C1 controls (tab and newline are the layout
/// engine's business), format characters such as ZWSP/ZWJ, bidi marks, embeddings and isolates, soft hyphen,
/// interlinear annotation controls, tag characters and variation selectors.
pub fn is_zero_width(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
                | '\u{E0100}'..='\u{E01EF}'
        )
}

#[derive(Clone, Debug, Default)]
pub struct FontDb {
    pub faces: Vec<Face>,
    /// Lazily parsed per-face data, parallel to `faces` (see [`FontDb::add_face`]).
    cache: Vec<OnceLock<FaceInfo>>,
}

impl FontDb {
    /// All bundled faces.
    pub fn bundled() -> FontDb {
        let mut db = FontDb::default();
        for &(family, weight, italic, data) in BUNDLED {
            let _ = db.add_face(Face { family, weight, italic, data });
        }
        db
    }

    /// Appends a face (keeping the metrics cache in step). `None` once 65 535 faces exist.
    pub fn add_face(&mut self, face: Face) -> Option<FontId> {
        let id = u16::try_from(self.faces.len()).ok().filter(|&i| i < u16::MAX)?;
        self.faces.push(face);
        self.cache.resize_with(self.faces.len(), OnceLock::new);
        Some(FontId(id))
    }

    /// Distinct family names, in face order.
    pub fn families(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for f in &self.faces {
            if !out.iter().any(|x| resolve::same_family(x, f.family)) {
                out.push(f.family);
            }
        }
        out
    }

    /// Ids of every face paired with its face, skipping any beyond the `u16` id range.
    fn indexed(&self) -> impl Iterator<Item = (FontId, &Face)> {
        self.faces.iter().enumerate().filter_map(|(i, f)| u16::try_from(i).ok().map(|i| (FontId(i), f)))
    }

    fn info(&self, id: FontId) -> Option<InfoRef<'_>> {
        let face = self.face(id)?;
        if let Some(slot) = self.cache.get(usize::from(id.0)) {
            let info = slot.get_or_init(|| FaceInfo::parse(face.data));
            if info.matches(face.data) {
                return Some(InfoRef::Cached(info));
            }
        }
        Some(InfoRef::Owned(Box::new(FaceInfo::parse(face.data))))
    }

    /// Resolves a CSS `font-family` list ("Aptos, Calibri, sans-serif") with weight/italic to the best bundled
    /// face. Aliases: Aptos/Segoe UI/system-ui/-apple-system → Inter; Calibri → Carlito; Arial/Helvetica/
    /// Verdana/Tahoma/sans-serif → Liberation Sans; Times/Times New Roman/Georgia/Cambria/serif → Liberation Serif;
    /// Courier/Courier New/Consolas/monospace → Liberation Mono. Unknown → Inter.
    pub fn resolve(&self, families: &str, weight: u16, italic: bool) -> FontId {
        let weight = if weight == 0 { 400 } else { weight.min(1000) };
        let known = self.families();
        for name in resolve::split_families(families) {
            let Some(m) = resolve::match_family(&name, &known) else {
                continue;
            };
            let w = match m.weight_hint {
                Some(hint) if weight == 400 => hint,
                _ => weight,
            };
            if let Some(id) = self.best_in(Some(m.family), w, italic || m.italic_hint) {
                return id;
            }
        }
        self.best_in(Some(UI_FAMILY), weight, italic)
            .or_else(|| self.best_in(None, weight, italic))
            .unwrap_or_default()
    }

    /// CSS-style match within `family` (or across all faces for `None`): style first, then weight.
    fn best_in(&self, family: Option<&str>, weight: u16, italic: bool) -> Option<FontId> {
        self.indexed()
            .filter(|(_, f)| family.is_none_or(|fam| resolve::same_family(f.family, fam)))
            .min_by_key(|(id, f)| (f.italic != italic, resolve::weight_rank(weight, f.weight), id.0))
            .map(|(id, _)| id)
    }

    /// Styling the renderer has to synthesise when drawing `id` for a request of `weight`/`italic`.
    pub fn synthesis(&self, id: FontId, weight: u16, italic: bool) -> Synthesis {
        match self.face(id) {
            Some(f) => Synthesis { bold: weight >= 600 && f.weight <= 500, oblique: italic && !f.italic },
            None => Synthesis::default(),
        }
    }

    pub fn face(&self, id: FontId) -> Option<&Face> {
        self.faces.get(id.0 as usize)
    }
    pub fn data(&self, id: FontId) -> &'static [u8] {
        self.face(id).map(|f| f.data).unwrap_or(&[])
    }
    pub fn metrics(&self, id: FontId, size_px: f32) -> LineMetrics {
        let s = sanitize_size(size_px);
        match self.info(id) {
            Some(info) => {
                let k = info.scale(s);
                LineMetrics {
                    ascent: info.ascent * k,
                    descent: info.descent * k,
                    line_gap: info.line_gap * k,
                    underline_pos: info.underline_pos * k,
                    underline_thickness: info.underline_thickness * k,
                }
            }
            None => LineMetrics {
                ascent: s * 0.8,
                descent: s * 0.2,
                line_gap: 0.0,
                underline_pos: s * 0.1,
                underline_thickness: s / 14.0,
            },
        }
    }
    /// Units per em of a face (1000 for unknown ids or unparsable data).
    pub fn units_per_em(&self, id: FontId) -> f32 {
        self.info(id).map_or(1000.0, |i| i.units_per_em)
    }
    /// Glyph id for `c` (0 = .notdef).
    pub fn glyph_id(&self, id: FontId, c: char) -> u32 {
        self.info(id).map_or(0, |i| i.glyph(c))
    }
    /// True if the face maps `c` to a real glyph.
    pub fn has_glyph(&self, id: FontId, c: char) -> bool {
        self.glyph_id(id, c) != 0
    }
    /// The face to draw `c` with when the run's font is `id`: `id` itself if it has the glyph (or `c` is
    /// zero-width), otherwise the closest-styled other face that does (same italic first, then nearest weight,
    /// then face order), and `id` again when no face has it (the caller then draws .notdef).
    pub fn fallback_for(&self, id: FontId, c: char) -> FontId {
        if is_zero_width(c) || self.has_glyph(id, c) {
            return id;
        }
        let (weight, italic) = self.face(id).map_or((400, false), |f| (f.weight, f.italic));
        self.indexed()
            .filter(|(i, _)| *i != id && self.has_glyph(*i, c))
            .min_by_key(|(i, f)| (f.italic != italic, f.weight.abs_diff(weight), i.0))
            .map_or(id, |(i, _)| i)
    }
    /// Advance of a glyph id in px.
    pub fn glyph_advance(&self, id: FontId, glyph: u32, size_px: f32) -> f32 {
        let s = sanitize_size(size_px);
        self.info(id).map_or(s * 0.5, |i| i.glyph_advance_units(glyph) * i.scale(s))
    }
    /// Horizontal advance of one char in px (no shaping/kerning). Chars missing from `id` are measured in their
    /// fallback face ([`FontDb::fallback_for`]); chars no face has measure as `id`'s .notdef. Zero-width chars
    /// ([`is_zero_width`]) measure 0.
    pub fn advance(&self, id: FontId, c: char, size_px: f32) -> f32 {
        let s = sanitize_size(size_px);
        match self.info(id) {
            Some(info) => self.advance_with(&info, id, c, s),
            None => {
                if is_zero_width(c) {
                    0.0
                } else {
                    s * 0.5
                }
            }
        }
    }
    /// `advance` with the primary face's info already looked up.
    fn advance_with(&self, info: &FaceInfo, id: FontId, c: char, s: f32) -> f32 {
        if is_zero_width(c) {
            return 0.0;
        }
        let g = info.glyph(c);
        if g != 0 {
            return info.glyph_advance_units(g) * info.scale(s);
        }
        let fb = self.fallback_for(id, c);
        if fb != id
            && let Some(other) = self.info(fb)
        {
            let g = other.glyph(c);
            return other.glyph_advance_units(g) * other.scale(s);
        }
        info.glyph_advance_units(0) * info.scale(s)
    }
    /// Width of a string in px (sum of advances).
    pub fn measure(&self, id: FontId, text: &str, size_px: f32) -> f32 {
        let s = sanitize_size(size_px);
        match self.info(id) {
            Some(info) => text.chars().map(|c| self.advance_with(&info, id, c, s)).sum(),
            None => text.chars().map(|c| self.advance(id, c, s)).sum(),
        }
    }
    /// Glyph outline scaled to `size_px`.
    pub fn outline(&self, id: FontId, glyph: u32, size_px: f32) -> Vec<PathCmd> {
        let s = sanitize_size(size_px);
        if s <= 0.0 {
            return Vec::new();
        }
        self.info(id).map_or_else(Vec::new, |i| i.outline(glyph, s))
    }
}
