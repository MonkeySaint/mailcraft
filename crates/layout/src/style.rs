//! Text style inheritance and resolution to concrete font/size/colour.

use mailcraft_fonts::{BODY_FAMILY, BODY_SIZE_PT, FontDb, FontId, LineMetrics};
use mailcraft_geom::{Color, pt_to_px};
use mailcraft_model::{TextStyle, UnderlineKind};

/// Family list used for monospace text (`Pre`, `monospace: Some(true)`).
pub(crate) const MONO_FAMILY: &str = "Consolas, Courier New, monospace";
/// Font sizes outside this range (in points) are clamped.
const MIN_PT: f32 = 1.0;
const MAX_PT: f32 = 400.0;
/// Family strings longer than this are truncated before matching.
const MAX_FAMILY_CHARS: usize = 256;
/// Superscript / subscript size factor and baseline shifts (fractions of the unreduced size).
const SCRIPT_SCALE: f32 = 0.75;
const SUPER_SHIFT: f32 = 0.35;
const SUB_SHIFT: f32 = 0.15;

fn valid_pt(v: f32) -> bool {
    v.is_finite() && v > 0.0
}

/// `child` over `parent`: every `Some` field of `child` wins.
pub(crate) fn merge(parent: &TextStyle, child: &TextStyle) -> TextStyle {
    TextStyle {
        bold: child.bold.or(parent.bold),
        italic: child.italic.or(parent.italic),
        underline: child.underline.or(parent.underline),
        strike: child.strike.or(parent.strike),
        superscript: child.superscript.or(parent.superscript),
        subscript: child.subscript.or(parent.subscript),
        monospace: child.monospace.or(parent.monospace),
        font_family: child
            .font_family
            .as_ref()
            .filter(|f| !f.trim().is_empty())
            .or(parent.font_family.as_ref())
            .cloned(),
        size_pt: child.size_pt.filter(|v| valid_pt(*v)).or(parent.size_pt),
        color: child.color.or(parent.color),
        highlight: child.highlight.or(parent.highlight),
        underline_kind: child.underline_kind.or(parent.underline_kind),
        underline_color: child.underline_color.or(parent.underline_color),
        double_strike: child.double_strike.or(parent.double_strike),
        small_caps: child.small_caps.or(parent.small_caps),
        all_caps: child.all_caps.or(parent.all_caps),
        spacing_pt: child.spacing_pt.filter(|v| v.is_finite()).or(parent.spacing_pt),
        position_pt: child.position_pt.filter(|v| v.is_finite()).or(parent.position_pt),
    }
}

/// Character spacing and baseline position are kept within this many points.
const MAX_SHIFT_PT: f32 = 200.0;

/// Letter spacing `v` kept within half an em narrower and one em wider (0 when not finite).
fn sane_spacing(v: f32, em: f32) -> f32 {
    if !v.is_finite() || !em.is_finite() {
        return 0.0;
    }
    v.max(-em * 0.5).min(em)
}

/// A finite point value within ±[`MAX_SHIFT_PT`], else 0.
fn shift_pt(v: Option<f32>) -> f32 {
    v.filter(|v| v.is_finite()).map_or(0.0, |v| v.clamp(-MAX_SHIFT_PT, MAX_SHIFT_PT))
}

/// Effective point size of a (merged) style, clamped to a sane range.
pub(crate) fn size_pt(ts: &TextStyle) -> f32 {
    ts.size_pt.filter(|v| valid_pt(*v)).unwrap_or(BODY_SIZE_PT).clamp(MIN_PT, MAX_PT)
}

/// Fully resolved character formatting.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RStyle {
    pub font: FontId,
    pub size_px: f32,
    pub color: Color,
    pub underline: bool,
    pub strike: bool,
    /// Underline style and colour (`None` = the text colour); drawn as lines by the layout unless a plain
    /// single underline in the text colour.
    pub underline_kind: UnderlineKind,
    pub underline_color: Option<Color>,
    pub double_strike: bool,
    /// Extra advance after every character in px (negative = condensed).
    pub letter_spacing: f32,
    pub highlight: Option<Color>,
    /// Baseline shift in px, positive = up (superscript).
    pub shift: f32,
    pub metrics: LineMetrics,
    /// Advance of U+0020 (used for tab stops).
    pub space_w: f32,
}

/// Sanitises a measured width: non-finite or negative values become 0, huge values are capped.
pub(crate) fn sane(v: f32) -> f32 {
    if v.is_finite() && v > 0.0 { v.min(1.0e7) } else { 0.0 }
}

/// Font metrics with a fallback for broken fonts.
pub(crate) fn safe_metrics(fonts: &FontDb, font: FontId, size_px: f32) -> LineMetrics {
    let m = fonts.metrics(font, size_px);
    let ok = |v: f32| (0.0..1.0e6).contains(&v);
    if ok(m.ascent) && ok(m.descent) && ok(m.line_gap) && m.ascent + m.descent > 0.0 {
        m
    } else {
        LineMetrics {
            ascent: size_px * 0.8,
            descent: size_px * 0.2,
            line_gap: 0.0,
            underline_pos: size_px * 0.1,
            underline_thickness: 1.0,
        }
    }
}

/// Picks the font for a merged style.
pub(crate) fn font_for(fonts: &FontDb, ts: &TextStyle) -> FontId {
    let weight = if ts.bold.unwrap_or(false) { 700 } else { 400 };
    let italic = ts.italic.unwrap_or(false);
    if ts.monospace.unwrap_or(false) {
        return fonts.resolve(MONO_FAMILY, weight, italic);
    }
    match ts.font_family.as_deref() {
        Some(f) if f.chars().count() > MAX_FAMILY_CHARS => {
            let cut: String = f.chars().take(MAX_FAMILY_CHARS).collect();
            fonts.resolve(&cut, weight, italic)
        }
        Some(f) => fonts.resolve(f, weight, italic),
        None => fonts.resolve(BODY_FAMILY, weight, italic),
    }
}

/// Resolves a merged style at `zoom`.
pub(crate) fn resolve(fonts: &FontDb, ts: &TextStyle, zoom: f32, default_color: Color) -> RStyle {
    let font = font_for(fonts, ts);
    let mut size_px = pt_to_px(size_pt(ts)) * zoom;
    let mut shift = 0.0;
    if ts.superscript.unwrap_or(false) {
        shift = size_px * SUPER_SHIFT;
        size_px *= SCRIPT_SCALE;
    } else if ts.subscript.unwrap_or(false) {
        shift = -size_px * SUB_SHIFT;
        size_px *= SCRIPT_SCALE;
    }
    shift += pt_to_px(shift_pt(ts.position_pt)) * zoom;
    let metrics = safe_metrics(fonts, font, size_px);
    let space_w = sane(fonts.advance(font, ' ', size_px));
    RStyle {
        font,
        size_px,
        color: ts.color.unwrap_or(default_color),
        underline: ts.underline.unwrap_or(false),
        strike: ts.strike.unwrap_or(false) && !ts.double_strike.unwrap_or(false),
        underline_kind: ts.underline_kind.unwrap_or_default(),
        underline_color: ts.underline_color,
        double_strike: ts.double_strike.unwrap_or(false),
        letter_spacing: sane_spacing(pt_to_px(shift_pt(ts.spacing_pt)) * zoom, size_px),
        highlight: ts.highlight,
        shift,
        metrics,
        space_w,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_child_wins() {
        let parent = TextStyle { bold: Some(true), size_pt: Some(10.0), ..Default::default() };
        let child = TextStyle { size_pt: Some(20.0), italic: Some(true), ..Default::default() };
        let m = merge(&parent, &child);
        assert_eq!(m.bold, Some(true));
        assert_eq!(m.italic, Some(true));
        assert_eq!(m.size_pt, Some(20.0));
    }

    #[test]
    fn invalid_sizes_ignored() {
        let parent = TextStyle { size_pt: Some(10.0), ..Default::default() };
        for bad in [f32::NAN, -3.0, 0.0, f32::INFINITY] {
            let child = TextStyle { size_pt: Some(bad), ..Default::default() };
            assert_eq!(merge(&parent, &child).size_pt, Some(10.0));
        }
        assert_eq!(size_pt(&TextStyle::default()), BODY_SIZE_PT);
        assert_eq!(size_pt(&TextStyle { size_pt: Some(1.0e9), ..Default::default() }), MAX_PT);
    }

    #[test]
    fn empty_family_inherits() {
        let parent = TextStyle { font_family: Some("Arial".into()), ..Default::default() };
        let child = TextStyle { font_family: Some("  ".into()), ..Default::default() };
        assert_eq!(merge(&parent, &child).font_family.as_deref(), Some("Arial"));
    }

    #[test]
    fn default_size_is_16px_at_100_percent() {
        let fonts = FontDb::bundled();
        let r = resolve(&fonts, &TextStyle::default(), 1.0, Color::BLACK);
        assert!((r.size_px - 16.0).abs() < 1e-4);
        let z = resolve(&fonts, &TextStyle::default(), 2.0, Color::BLACK);
        assert!((z.size_px - 32.0).abs() < 1e-4);
    }

    #[test]
    fn superscript_is_smaller_and_raised() {
        let fonts = FontDb::bundled();
        let sup = TextStyle { superscript: Some(true), ..Default::default() };
        let r = resolve(&fonts, &sup, 1.0, Color::BLACK);
        assert!(r.size_px < 16.0);
        assert!(r.shift > 0.0);
        let sub = TextStyle { subscript: Some(true), ..Default::default() };
        assert!(resolve(&fonts, &sub, 1.0, Color::BLACK).shift < 0.0);
    }
}
