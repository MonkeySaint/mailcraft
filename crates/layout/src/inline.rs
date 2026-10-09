//! Inline content: flattening the inline tree to text + spans, grapheme measurement, UAX #14 line breaking
//! and min/max-content widths.
//!
//! Offsets: the flattened text of a block is the concatenation of its `Text` contents; a `LineBreak` counts
//! as one char (`'\n'`) and an `Image` as one char (U+FFFC). [`crate::TextPos::offset`] counts chars of this text.

use mailcraft_model::{Inline, TextStyle};
use unicode_linebreak::{BreakOpportunity, linebreaks};
use unicode_segmentation::UnicodeSegmentation;

use crate::block::Ctx;
use crate::style::{RStyle, merge, sane};

/// Flattened text larger than this (bytes) is truncated: keeps hostile bodies bounded.
pub(crate) const MAX_BLOCK_TEXT: usize = 4 << 20;
/// Link nesting deeper than this is flattened into the outer link (walked without recursion).
const MAX_INLINE_DEPTH: usize = 64;
/// Tab stops every this many space widths.
const TAB_SPACES: f32 = 8.0;
/// Style interning looks back this many entries only, so hostile runs of distinct styles stay linear.
const STYLE_LOOKBACK: usize = 32;
/// Placeholder size of an image without dimensions.
pub(crate) const IMAGE_PLACEHOLDER: f32 = 16.0;
/// Min-content contribution of an image (px at 100 %); wider images scale down instead.
const IMAGE_MIN_CONTENT: f32 = 32.0;
/// Largest image dimension accepted (px, before zoom).
const MAX_IMAGE: f32 = 20_000.0;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ImageInfo {
    pub src: String,
    pub alt: String,
    /// Natural size in px (zoom applied), before fitting to the line width.
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SpanKind {
    Text,
    Break,
    Image(usize),
}

#[derive(Clone, Debug)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
    pub style: usize,
    pub link: Option<usize>,
    pub kind: SpanKind,
}

/// A block's inline content, flattened.
#[derive(Clone, Debug, Default)]
pub(crate) struct Flat {
    pub text: String,
    pub spans: Vec<Span>,
    pub styles: Vec<RStyle>,
    pub links: Vec<String>,
    pub images: Vec<ImageInfo>,
    /// Style of the block itself (the strut every line gets).
    pub strut: Option<RStyle>,
    pub zoom: f32,
}

impl Flat {
    pub(crate) fn new(ctx: &Ctx<'_>, block_ts: &TextStyle) -> Flat {
        Flat { strut: Some(ctx.resolve(block_ts)), zoom: ctx.zoom, ..Default::default() }
    }

    fn style_index(&mut self, s: RStyle) -> usize {
        let from = self.styles.len().saturating_sub(STYLE_LOOKBACK);
        if let Some(i) = self.styles.get(from..).and_then(|tail| tail.iter().rposition(|x| *x == s)) {
            return from.saturating_add(i);
        }
        self.styles.push(s);
        self.styles.len().saturating_sub(1)
    }

    fn push(&mut self, text: &str, style: RStyle, link: Option<usize>, kind: SpanKind) {
        let room = MAX_BLOCK_TEXT.saturating_sub(self.text.len());
        if room == 0 || text.is_empty() {
            return;
        }
        let mut cut = text.len().min(room);
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let Some(part) = text.get(..cut) else { return };
        if part.is_empty() {
            return;
        }
        let style = self.style_index(style);
        let start = self.text.len();
        self.text.push_str(part);
        self.spans.push(Span { start, end: self.text.len(), style, link, kind });
    }

    /// Appends plain text with an explicit style (used for `Pre`).
    pub(crate) fn push_text(&mut self, text: &str, style: RStyle) {
        self.push(text, style, None, SpanKind::Text);
    }

    pub(crate) fn style(&self, i: usize) -> Option<&RStyle> {
        self.styles.get(i)
    }
}

/// Image natural size (zoom applied): both dims given → as is; one given → square; none → placeholder.
pub(crate) fn image_size(width: Option<f32>, height: Option<f32>, zoom: f32) -> (f32, f32) {
    let ok = |v: Option<f32>| v.filter(|x| x.is_finite() && *x > 0.0).map(|x| x.min(MAX_IMAGE));
    let (w, h) = match (ok(width), ok(height)) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w),
        (None, Some(h)) => (h, h),
        (None, None) => (IMAGE_PLACEHOLDER, IMAGE_PLACEHOLDER),
    };
    (w * zoom, h * zoom)
}

/// Flattens `inlines` (with inherited style `parent`) into `flat`.
pub(crate) fn flatten(
    ctx: &Ctx<'_>,
    inlines: &[Inline],
    parent: &TextStyle,
    link: Option<usize>,
    flat: &mut Flat,
    depth: usize,
) {
    for inline in inlines {
        if flat.text.len() >= MAX_BLOCK_TEXT {
            return;
        }
        match inline {
            Inline::Link { href, children } if depth < MAX_INLINE_DEPTH => {
                flat.links.push(href.clone());
                let idx = Some(flat.links.len().saturating_sub(1));
                flatten(ctx, children, parent, idx, flat, depth.saturating_add(1));
            }
            // Too deep to recurse: the rest of the subtree joins the outer link, walked with a heap stack.
            Inline::Link { children, .. } => flatten_flat(ctx, children, parent, link, flat),
            leaf => push_leaf(ctx, leaf, parent, link, flat),
        }
    }
}

/// Non-recursive walk of an over-deep inline subtree; nested links collapse into `link`.
fn flatten_flat(ctx: &Ctx<'_>, inlines: &[Inline], parent: &TextStyle, link: Option<usize>, flat: &mut Flat) {
    let mut stack: Vec<std::slice::Iter<'_, Inline>> = vec![inlines.iter()];
    while let Some(top) = stack.last_mut() {
        if flat.text.len() >= MAX_BLOCK_TEXT {
            return;
        }
        match top.next() {
            None => {
                stack.pop();
            }
            Some(Inline::Link { children, .. }) => stack.push(children.iter()),
            Some(leaf) => push_leaf(ctx, leaf, parent, link, flat),
        }
    }
}

/// Appends a text, line break or image inline (links are handled by the callers).
fn push_leaf(ctx: &Ctx<'_>, inline: &Inline, parent: &TextStyle, link: Option<usize>, flat: &mut Flat) {
    match inline {
        Inline::Text { text, style } => {
            let mut ts = merge(parent, style);
            if link.is_some() {
                ts.color = Some(style.color.unwrap_or(ctx.link_color));
                ts.underline = Some(style.underline.unwrap_or(true));
            }
            let r = ctx.resolve(&ts);
            flat.push(text, r, link, SpanKind::Text);
        }
        Inline::LineBreak => {
            let r = ctx.resolve(parent);
            flat.push("\n", r, link, SpanKind::Break);
        }
        Inline::Image { src, alt, width, height } => {
            if MAX_BLOCK_TEXT.saturating_sub(flat.text.len()) < '\u{FFFC}'.len_utf8() {
                return;
            }
            let (w, h) = image_size(*width, *height, ctx.zoom);
            flat.images.push(ImageInfo { src: src.clone(), alt: alt.clone(), w, h });
            let idx = flat.images.len().saturating_sub(1);
            let r = ctx.resolve(parent);
            flat.push("\u{FFFC}", r, link, SpanKind::Image(idx));
        }
        Inline::Link { .. } => {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GKind {
    Normal,
    Space,
    Tab,
    /// Newlines and other control chars: zero width, never drawn.
    Control,
    Image(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Brk {
    None,
    Allowed,
    Mandatory,
}

/// One grapheme cluster (or image) of the flattened text.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Glyph {
    pub start: usize,
    pub end: usize,
    /// Char offset of the cluster start.
    pub offset: usize,
    pub span: usize,
    pub width: f32,
    pub kind: GKind,
    /// Break opportunity *before* this glyph.
    pub brk: Brk,
}

impl Glyph {
    pub(crate) fn is_space(&self) -> bool {
        matches!(self.kind, GKind::Space | GKind::Control)
    }
}

fn classify(g: &str) -> GKind {
    let mut chars = g.chars();
    match chars.next() {
        Some('\t') => GKind::Tab,
        Some(c) if c.is_control() || c == '\u{2028}' || c == '\u{2029}' => GKind::Control,
        Some(_) if g.chars().all(|c| c.is_whitespace() && c != '\u{a0}' && c != '\u{202f}') => GKind::Space,
        _ => GKind::Normal,
    }
}

/// Segments `flat` into measured glyphs with break opportunities.
pub(crate) fn glyphs(ctx: &Ctx<'_>, flat: &Flat) -> Vec<Glyph> {
    let mut out: Vec<Glyph> = Vec::new();
    let mut offset = 0usize;
    for (si, span) in flat.spans.iter().enumerate() {
        let Some(text) = flat.text.get(span.start..span.end) else { continue };
        let Some(style) = flat.style(span.style) else { continue };
        if let SpanKind::Image(ii) = span.kind {
            let w = flat.images.get(ii).map(|i| i.w).unwrap_or(0.0);
            out.push(Glyph {
                start: span.start,
                end: span.end,
                offset,
                span: si,
                width: w,
                kind: GKind::Image(ii),
                brk: Brk::Allowed,
            });
            offset = offset.saturating_add(text.chars().count());
            continue;
        }
        for (bi, g) in text.grapheme_indices(true) {
            let kind = if span.kind == SpanKind::Break { GKind::Control } else { classify(g) };
            let width = match kind {
                GKind::Normal | GKind::Space => sane(ctx.fonts.measure(style.font, g, style.size_px)),
                _ => 0.0,
            };
            let start = span.start.saturating_add(bi);
            out.push(Glyph {
                start,
                end: start.saturating_add(g.len()),
                offset,
                span: si,
                width,
                kind,
                brk: Brk::None,
            });
            offset = offset.saturating_add(g.chars().count());
        }
    }
    // Break opportunities (UAX #14). Positions are byte offsets *before* which a break may occur.
    let mut gi = 0usize;
    for (pos, opp) in linebreaks(&flat.text) {
        while out.get(gi).is_some_and(|g| g.start < pos) {
            gi = gi.saturating_add(1);
        }
        let Some(g) = out.get_mut(gi) else { break };
        let b = match opp {
            BreakOpportunity::Mandatory => Brk::Mandatory,
            BreakOpportunity::Allowed => Brk::Allowed,
        };
        if (g.start == pos || b == Brk::Mandatory) && g.brk != Brk::Mandatory {
            g.brk = b;
        }
    }
    // Images and the glyph after them are always break opportunities.
    let mut prev_image = false;
    for g in out.iter_mut() {
        if (prev_image || matches!(g.kind, GKind::Image(_))) && g.brk == Brk::None {
            g.brk = Brk::Allowed;
        }
        prev_image = matches!(g.kind, GKind::Image(_));
    }
    if let Some(first) = out.first_mut() {
        first.brk = Brk::None;
    }
    out
}

/// Advance of glyph `g` placed at `x` (relative to the line start). Tabs jump to the next stop.
pub(crate) fn advance(flat: &Flat, g: &Glyph, x: f32) -> f32 {
    if g.kind != GKind::Tab {
        return g.width;
    }
    let space = flat.spans.get(g.span).and_then(|s| flat.style(s.style)).map(|s| s.space_w).unwrap_or(0.0);
    let stop = space * TAB_SPACES;
    if stop <= 0.0 || !x.is_finite() {
        return 0.0;
    }
    let next = ((x / stop).floor() + 1.0) * stop;
    sane(next - x)
}

/// A line: glyph index range and whether it ends with a hard (mandatory) break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineRange {
    pub start: usize,
    pub end: usize,
    pub hard: bool,
}

/// Greedy line breaking. `wrap = false` honours only mandatory breaks (preformatted text). At most `max_lines`
/// lines are returned; the text after them is dropped.
pub(crate) fn break_lines(
    flat: &Flat,
    glyphs: &[Glyph],
    avail: f32,
    wrap: bool,
    max_lines: usize,
) -> Vec<LineRange> {
    let mut lines = Vec::new();
    let n = glyphs.len();
    if max_lines == 0 {
        return lines;
    }
    if n == 0 {
        lines.push(LineRange { start: 0, end: 0, hard: false });
        return lines;
    }
    let avail = if avail.is_finite() { avail.max(0.0) } else { f32::MAX };
    let mut line_start = 0usize;
    let mut i = 0usize;
    let mut x = 0.0f32;
    let mut last_break: Option<usize> = None;
    while i < n {
        let Some(g) = glyphs.get(i) else { break };
        if i > line_start {
            if g.brk == Brk::Mandatory {
                lines.push(LineRange { start: line_start, end: i, hard: true });
                if lines.len() >= max_lines {
                    return lines;
                }
                line_start = i;
                x = 0.0;
                last_break = None;
                continue;
            }
            if g.brk == Brk::Allowed {
                last_break = Some(i);
            }
        }
        let w = advance(flat, g, x);
        if g.is_space() || !wrap {
            x += w;
            i += 1;
            continue;
        }
        if x + w > avail && i > line_start {
            let end = last_break.filter(|&b| b > line_start).unwrap_or(i);
            lines.push(LineRange { start: line_start, end, hard: false });
            if lines.len() >= max_lines {
                return lines;
            }
            line_start = end;
            i = end;
            x = 0.0;
            last_break = None;
            continue;
        }
        x += w;
        i += 1;
    }
    lines.push(LineRange { start: line_start, end: n, hard: false });
    lines
}

/// Min-content (widest unbreakable segment) and max-content (widest hard line) widths.
pub(crate) fn min_max(flat: &Flat, glyphs: &[Glyph]) -> (f32, f32) {
    let mut min = 0.0f32;
    let mut max = 0.0f32;
    let mut line_x = 0.0f32;
    let mut line_visible = 0.0f32;
    let mut word = 0.0f32;
    for g in glyphs {
        match g.brk {
            Brk::Mandatory => {
                max = max.max(line_visible);
                line_x = 0.0;
                line_visible = 0.0;
                word = 0.0;
            }
            Brk::Allowed => word = 0.0,
            Brk::None => {}
        }
        let w = advance(flat, g, line_x);
        line_x += w;
        if !g.is_space() {
            // Images shrink to fit their line, so they only demand a small minimum.
            word += match g.kind {
                GKind::Image(_) => w.min(IMAGE_MIN_CONTENT * flat.zoom),
                _ => w,
            };
            min = min.max(word);
            line_visible = line_x;
        }
    }
    max = max.max(line_visible);
    (min, max.max(min))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LayoutOptions;
    use mailcraft_fonts::FontDb;

    fn with_ctx<R>(f: impl FnOnce(&Ctx<'_>) -> R) -> R {
        let fonts = FontDb::bundled();
        let opts = LayoutOptions::default();
        let ctx = Ctx::new(&fonts, &opts);
        f(&ctx)
    }

    fn flat_of(ctx: &Ctx<'_>, s: &str) -> Flat {
        let mut flat = Flat::new(ctx, &TextStyle::default());
        let inl = vec![Inline::Text { text: s.into(), style: TextStyle::default() }];
        flatten(ctx, &inl, &TextStyle::default(), None, &mut flat, 0);
        flat
    }

    #[test]
    fn graphemes_and_offsets() {
        with_ctx(|ctx| {
            let flat = flat_of(ctx, "ae\u{301}b");
            let g = glyphs(ctx, &flat);
            assert_eq!(g.len(), 3);
            assert_eq!(g.iter().map(|g| g.offset).collect::<Vec<_>>(), vec![0, 1, 3]);
        });
    }

    #[test]
    fn breaks_after_spaces() {
        with_ctx(|ctx| {
            let flat = flat_of(ctx, "ab cd");
            let g = glyphs(ctx, &flat);
            assert_eq!(g.get(3).map(|g| g.brk), Some(Brk::Allowed));
            assert_eq!(g.get(1).map(|g| g.brk), Some(Brk::None));
        });
    }

    #[test]
    fn newline_is_mandatory() {
        with_ctx(|ctx| {
            let flat = flat_of(ctx, "ab\ncd");
            let g = glyphs(ctx, &flat);
            let lines = break_lines(&flat, &g, f32::MAX, true, usize::MAX);
            assert_eq!(lines.len(), 2);
            assert!(lines.first().is_some_and(|l| l.hard));
        });
    }

    #[test]
    fn overlong_word_breaks_by_grapheme() {
        with_ctx(|ctx| {
            let flat = flat_of(ctx, "aaaaaaaaaa");
            let g = glyphs(ctx, &flat);
            let w = g.first().map(|g| g.width).unwrap_or(1.0);
            let lines = break_lines(&flat, &g, w * 3.5, true, usize::MAX);
            assert_eq!(lines.len(), 4);
            for l in &lines {
                assert!(l.end > l.start);
            }
        });
    }

    #[test]
    fn zero_width_still_progresses() {
        with_ctx(|ctx| {
            let flat = flat_of(ctx, "abc def");
            let g = glyphs(ctx, &flat);
            let lines = break_lines(&flat, &g, 0.0, true, usize::MAX);
            assert!(lines.len() >= 6);
            assert_eq!(lines.last().map(|l| l.end), Some(g.len()));
            let capped = break_lines(&flat, &g, 0.0, true, 3);
            assert_eq!(capped.len(), 3);
            assert!(break_lines(&flat, &g, 0.0, true, 0).is_empty());
        });
    }

    #[test]
    fn min_max_widths() {
        with_ctx(|ctx| {
            let flat = flat_of(ctx, "aaaa bb");
            let g = glyphs(ctx, &flat);
            let (mn, mx) = min_max(&flat, &g);
            let one = g.first().map(|g| g.width).unwrap_or(0.0);
            assert!((mn - 4.0 * one).abs() < 1e-3);
            assert!(mx > mn);
        });
    }

    #[test]
    fn tabs_jump_to_stops() {
        with_ctx(|ctx| {
            let flat = flat_of(ctx, "a\tb");
            let g = glyphs(ctx, &flat);
            let tab = g.get(1).copied();
            assert_eq!(tab.map(|t| t.kind), Some(GKind::Tab));
            if let Some(t) = tab {
                let a = advance(&flat, &t, 3.0);
                let b = advance(&flat, &t, 0.0);
                assert!(a > 0.0 && b > a);
            }
        });
    }

    #[test]
    fn styles_are_interned() {
        with_ctx(|ctx| {
            let mut flat = Flat::new(ctx, &TextStyle::default());
            let red = TextStyle { color: Some(mailcraft_geom::Color::rgb(255, 0, 0)), ..Default::default() };
            let inl = vec![
                Inline::Text { text: "a".into(), style: TextStyle::default() },
                Inline::Text { text: "b".into(), style: red },
                Inline::Text { text: "c".into(), style: TextStyle::default() },
            ];
            flatten(ctx, &inl, &TextStyle::default(), None, &mut flat, 0);
            assert_eq!(flat.styles.len(), 2);
            assert_eq!(flat.spans.iter().map(|s| s.style).collect::<Vec<_>>(), vec![0, 1, 0]);
        });
    }

    #[test]
    fn image_sizes() {
        assert_eq!(image_size(None, None, 1.0), (16.0, 16.0));
        assert_eq!(image_size(Some(10.0), None, 2.0), (20.0, 20.0));
        assert_eq!(image_size(Some(f32::NAN), Some(-1.0), 1.0), (16.0, 16.0));
        assert_eq!(image_size(Some(30.0), Some(20.0), 1.0), (30.0, 20.0));
    }

    #[test]
    fn huge_text_is_truncated() {
        with_ctx(|ctx| {
            let big = "é".repeat(MAX_BLOCK_TEXT);
            let flat = flat_of(ctx, &big);
            assert!(flat.text.len() <= MAX_BLOCK_TEXT);
        });
    }
}
