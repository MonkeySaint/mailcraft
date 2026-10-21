//! Block layout: paragraphs, headings, lists, quotes, pre, rules, containers (tables live in `table.rs`).

use std::cell::Cell;

use mailcraft_fonts::FontDb;
use mailcraft_geom::{Color, Point, Rect};
use mailcraft_model::rich::{Align, LineRule, ListKind};
use mailcraft_model::{Block, Inline, ParaStyle, TextStyle, UnderlineKind};

use crate::inline::{Flat, GKind, Glyph, LineRange, advance, break_lines_indented, flatten, glyphs, min_max};
use crate::style::{RStyle, merge, resolve, sane, size_pt};
use crate::table::{layout_table, table_min_max};
use crate::{CaretStop, DisplayItem, LayoutIndex, LayoutOptions, LineBox, LinkBox, RunBox};

/// Nesting deeper than this is not laid out (keeps hostile documents from exhausting the stack).
pub(crate) const MAX_DEPTH: usize = 40;
/// Total text lines laid out per document; lines past this are dropped (a hostile body at zero width would
/// otherwise get one line per grapheme).
pub(crate) const MAX_LINES: usize = 1 << 18;
/// Indent per list level (px at 100 %).
pub(crate) const LIST_INDENT: f32 = 24.0;
/// Gap between a list marker and the item text.
const MARKER_GAP: f32 = 6.0;
/// Quote bar width and the indent of quoted content after it.
pub(crate) const QUOTE_BAR: f32 = 2.0;
pub(crate) const QUOTE_INDENT: f32 = 12.0;
/// Horizontal rule: thickness and vertical margin.
const RULE_THICKNESS: f32 = 1.0;
const RULE_MARGIN: f32 = 8.0;
/// Largest explicit spacing/indent accepted from the model (px, before zoom).
const MAX_SPACING: f32 = 10_000.0;
/// Heading size factors (h1..h6) relative to the inherited size.
const HEADING_SCALE: [f32; 6] = [2.0, 1.5, 1.17, 1.0, 0.83, 0.67];
/// Default heading margins as a fraction of the heading font size (used when the style sets none).
const HEADING_MARGIN: f32 = 0.33;

pub(crate) const QUOTE_BAR_COLOR: Color = Color::hex(0xb4b4b4);
pub(crate) const RULE_COLOR: Color = Color::hex(0xa0a0a0);

/// Shared layout state.
pub(crate) struct Ctx<'a> {
    pub fonts: &'a FontDb,
    pub zoom: f32,
    pub width: f32,
    pub default_color: Color,
    pub link_color: Color,
    /// Lines that may still be laid out (see [`MAX_LINES`]).
    pub lines_left: Cell<usize>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(fonts: &'a FontDb, opts: &LayoutOptions) -> Ctx<'a> {
        let zoom = if opts.zoom.is_finite() && opts.zoom > 0.0 { opts.zoom.clamp(0.1, 10.0) } else { 1.0 };
        let width = if opts.width.is_finite() && opts.width > 0.0 { opts.width.min(1.0e6) } else { 0.0 };
        Ctx {
            fonts,
            zoom,
            width,
            default_color: opts.default_color,
            link_color: opts.link_color,
            lines_left: Cell::new(MAX_LINES),
        }
    }
    pub(crate) fn resolve(&self, ts: &TextStyle) -> RStyle {
        resolve(self.fonts, ts, self.zoom, self.default_color)
    }
    /// Model px value → layout px (sanitised, zoomed).
    pub(crate) fn px(&self, v: f32) -> f32 {
        if v.is_finite() && v > 0.0 { v.min(MAX_SPACING) * self.zoom } else { 0.0 }
    }
    pub(crate) fn measure(&self, s: &RStyle, text: &str) -> f32 {
        sane(self.fonts.measure(s.font, text, s.size_px))
    }
}

/// Where a block is placed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub depth: usize,
    pub list_depth: usize,
}

impl Frame {
    pub(crate) fn inset(self, left: f32) -> Frame {
        let left = left.clamp(0.0, self.w.max(0.0));
        Frame { x: self.x + left, w: (self.w - left).max(0.0), ..self }
    }
    /// Narrower by `right` on the right (clamped to the width).
    pub(crate) fn inset_right(self, right: f32) -> Frame {
        let right = right.clamp(0.0, self.w.max(0.0));
        Frame { w: (self.w - right).max(0.0), ..self }
    }
    /// Moved right by `dx` (left when negative) and as much narrower; `dx` is kept within the frame.
    pub(crate) fn shifted(self, dx: f32) -> Frame {
        let dx = if dx.is_finite() { dx.min(self.w.max(0.0)) } else { 0.0 };
        Frame { x: self.x + dx, w: (self.w - dx).max(0.0), ..self }
    }
    pub(crate) fn at_y(self, y: f32) -> Frame {
        Frame { y, ..self }
    }
    pub(crate) fn deeper(self) -> Frame {
        Frame { depth: self.depth.saturating_add(1), ..self }
    }
}

/// Display items plus hit-test records under construction.
#[derive(Clone, Debug, Default)]
pub(crate) struct Out {
    pub items: Vec<DisplayItem>,
    pub index: LayoutIndex,
}

pub(crate) fn translate_item(item: &mut DisplayItem, dx: f32, dy: f32) {
    match item {
        DisplayItem::FillRect { rect, .. }
        | DisplayItem::StrokeRect { rect, .. }
        | DisplayItem::Image { rect, .. } => *rect = rect.translate(dx, dy),
        DisplayItem::Line { from, to, .. } => {
            *from = from.offset(dx, dy);
            *to = to.offset(dx, dy);
        }
        DisplayItem::Text { origin, .. } => *origin = origin.offset(dx, dy),
    }
}

impl Out {
    pub(crate) fn push(&mut self, item: DisplayItem) -> usize {
        self.items.push(item);
        self.items.len().saturating_sub(1)
    }
    /// Reserves a slot for a background whose size is known only after the content is laid out.
    pub(crate) fn placeholder(&mut self) -> usize {
        self.push(DisplayItem::FillRect { rect: Rect::ZERO, color: Color::TRANSPARENT })
    }
    pub(crate) fn set(&mut self, idx: usize, item: DisplayItem) {
        if let Some(slot) = self.items.get_mut(idx) {
            *slot = item;
        }
    }
    /// Appends `other`, translated by (dx, dy).
    pub(crate) fn append(&mut self, other: Out, dx: f32, dy: f32) {
        let base = self.items.len();
        for mut item in other.items {
            translate_item(&mut item, dx, dy);
            self.items.push(item);
        }
        for mut run in other.index.runs {
            run.item = run.item.saturating_add(base);
            run.rect = run.rect.translate(dx, dy);
            self.index.runs.push(run);
        }
        for mut line in other.index.lines {
            line.rect = line.rect.translate(dx, dy);
            line.baseline += dy;
            for s in &mut line.stops {
                s.x += dx;
            }
            self.index.lines.push(line);
        }
        for mut link in other.index.links {
            link.rect = link.rect.translate(dx, dy);
            self.index.links.push(link);
        }
    }
}

/// Lays out a sequence of blocks top to bottom; returns the height used.
pub(crate) fn layout_blocks(
    ctx: &Ctx<'_>,
    blocks: &[Block],
    ts: &TextStyle,
    path: &mut Vec<usize>,
    frame: Frame,
    out: &mut Out,
) -> f32 {
    if frame.depth > MAX_DEPTH {
        return 0.0;
    }
    let mut y = frame.y;
    for (i, block) in blocks.iter().enumerate() {
        path.push(i);
        y += layout_block(ctx, block, ts, path, frame.at_y(y), out);
        path.pop();
    }
    y - frame.y
}

fn heading_level(level: u8) -> usize {
    usize::from(level.clamp(1, 6)).saturating_sub(1)
}

/// Text style of a heading block: scaled size + bold over the inherited style.
pub(crate) fn heading_style(ts: &TextStyle, level: u8) -> TextStyle {
    let factor = HEADING_SCALE.get(heading_level(level)).copied().unwrap_or(1.0);
    merge(ts, &TextStyle { bold: Some(true), size_pt: Some(size_pt(ts) * factor), ..Default::default() })
}

pub(crate) fn pre_style(ts: &TextStyle) -> TextStyle {
    merge(ts, &TextStyle { monospace: Some(true), ..Default::default() })
}

fn layout_block(
    ctx: &Ctx<'_>,
    block: &Block,
    ts: &TextStyle,
    path: &mut Vec<usize>,
    frame: Frame,
    out: &mut Out,
) -> f32 {
    match block {
        Block::Paragraph { style, inlines } => layout_para(ctx, inlines, style, ts, None, path, frame, out),
        Block::Heading { level, style, inlines } => {
            let hts = heading_style(ts, *level);
            let margin = ctx.resolve(&hts).size_px * HEADING_MARGIN;
            layout_para(ctx, inlines, style, &hts, Some(margin), path, frame, out)
        }
        Block::List { kind, items } => layout_list(ctx, kind, items, ts, path, frame, out),
        Block::Quote { blocks } => {
            let bar = out.placeholder();
            let inner = frame.inset(ctx.px(QUOTE_BAR + QUOTE_INDENT)).deeper();
            let h = layout_blocks(ctx, blocks, ts, path, inner, out);
            if h > 0.0 {
                let rect = Rect::new(frame.x, frame.y, ctx.px(QUOTE_BAR).min(frame.w), h);
                out.set(bar, DisplayItem::FillRect { rect, color: QUOTE_BAR_COLOR });
            }
            h
        }
        Block::Pre { text } => {
            let pts = pre_style(ts);
            let mut flat = Flat::new(ctx, &pts);
            flat.push_text(text, ctx.resolve(&pts));
            layout_flat(ctx, &mut flat, Align::Start, None, path, frame, out, false)
        }
        Block::Table { rows, border, width_px } => {
            layout_table(ctx, rows, *border, *width_px, ts, path, frame.deeper(), out)
        }
        Block::Rule => {
            let m = ctx.px(RULE_MARGIN);
            let t = ctx.px(RULE_THICKNESS);
            let ly = frame.y + m + t / 2.0;
            out.push(DisplayItem::Line {
                from: Point::new(frame.x, ly),
                to: Point::new(frame.x + frame.w, ly),
                color: RULE_COLOR,
                width: t,
            });
            m * 2.0 + t
        }
        Block::Container { style, blocks } => {
            let sb = ctx.px(style.space_before_px);
            let sa = ctx.px(style.space_after_px);
            let bg = style.background.map(|_| out.placeholder());
            let inner = frame.inset(ctx.px(style.indent_px)).at_y(frame.y + sb).deeper();
            let h = layout_blocks(ctx, blocks, ts, path, inner, out);
            if let (Some(i), Some(color)) = (bg, style.background) {
                out.set(
                    i,
                    DisplayItem::FillRect { rect: Rect::new(frame.x, frame.y + sb, frame.w, h), color },
                );
            }
            sb + h + sa
        }
    }
}

fn layout_para(
    ctx: &Ctx<'_>,
    inlines: &[Inline],
    style: &ParaStyle,
    ts: &TextStyle,
    default_margin: Option<f32>,
    path: &[usize],
    frame: Frame,
    out: &mut Out,
) -> f32 {
    let mut sb = ctx.px(style.space_before_px);
    let mut sa = ctx.px(style.space_after_px);
    if let Some(m) = default_margin
        && sb == 0.0
        && sa == 0.0
    {
        sb = m;
        sa = m;
    }
    let bg = style.background.map(|_| out.placeholder());
    let mut flat = Flat::new(ctx, ts);
    flatten(ctx, inlines, ts, None, &mut flat, 0);
    let left = ctx.px(style.indent_px);
    let inner = frame.inset(left).inset_right(ctx.px(style.right_indent_px)).at_y(frame.y + sb);
    // A hanging indent may reach back to the paragraph's own left edge, no further: by at most the indent
    // actually applied (a narrow frame clamps the indent, e.g. in a squeezed table cell).
    let first = style.first_line_px;
    let applied = (inner.x - frame.x).max(0.0);
    let first_dx = if first < 0.0 { -ctx.px(-first).min(left).min(applied) } else { ctx.px(first) };
    let spacing = match style.line_px {
        Some((px, rule)) if px.is_finite() && px > 0.0 => {
            let px = ctx.px(px.clamp(1.0, MAX_LINE_PX));
            match rule {
                LineRule::Exactly => Spacing::Exactly(px),
                LineRule::AtLeast => Spacing::AtLeast(px),
            }
        }
        _ => Spacing::multiple(style.line_height),
    };
    let h = layout_lines(ctx, &mut flat, style.align, spacing, first_dx, path, inner, out, true);
    if let (Some(i), Some(color)) = (bg, style.background) {
        out.set(i, DisplayItem::FillRect { rect: Rect::new(frame.x, frame.y + sb, frame.w, h), color });
    }
    sb + h + sa
}

/// Scales images wider than `w` down to fit (aspect preserved).
fn fit_images(flat: &mut Flat, w: f32) {
    for img in &mut flat.images {
        if img.w > w {
            let s = if img.w > 0.0 { w / img.w } else { 0.0 };
            img.w = w.max(0.0);
            img.h *= s;
        }
    }
}

fn span_style<'f>(flat: &'f Flat, g: &Glyph) -> Option<&'f RStyle> {
    flat.spans.get(g.span).and_then(|s| flat.style(s.style))
}

fn span_link(flat: &Flat, g: &Glyph) -> Option<String> {
    flat.spans.get(g.span).and_then(|s| s.link).and_then(|l| flat.links.get(l)).cloned()
}

fn glyph_chars(flat: &Flat, g: &Glyph) -> usize {
    flat.text.get(g.start..g.end).map(|t| t.chars().count()).unwrap_or(0)
}

/// Largest fixed line height accepted from the model (px, before zoom).
const MAX_LINE_PX: f32 = 2_000.0;

/// How tall the lines of a block are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Spacing {
    /// The fonts' own line height.
    Auto,
    /// A multiple of it (never less than the ink).
    Multiple(f32),
    /// Exactly this many px.
    Exactly(f32),
    /// At least this many px.
    AtLeast(f32),
}

impl Spacing {
    /// A line-height multiple (`None`, zero, negative or non-finite = the fonts' own).
    pub(crate) fn multiple(m: Option<f32>) -> Spacing {
        match m.filter(|m| m.is_finite() && *m > 0.0) {
            Some(m) => Spacing::Multiple(m.clamp(0.25, 10.0)),
            None => Spacing::Auto,
        }
    }
}

/// Lays out the lines of one inline block at `frame`; returns the height used.
pub(crate) fn layout_flat(
    ctx: &Ctx<'_>,
    flat: &mut Flat,
    align: Align,
    line_height: Option<f32>,
    path: &[usize],
    frame: Frame,
    out: &mut Out,
    wrap: bool,
) -> f32 {
    layout_lines(ctx, flat, align, Spacing::multiple(line_height), 0.0, path, frame, out, wrap)
}

/// [`layout_flat`] with a line spacing rule and the first line moved by `first_dx` (first-line indent,
/// negative = hanging).
pub(crate) fn layout_lines(
    ctx: &Ctx<'_>,
    flat: &mut Flat,
    align: Align,
    spacing: Spacing,
    first_dx: f32,
    path: &[usize],
    frame: Frame,
    out: &mut Out,
    wrap: bool,
) -> f32 {
    let budget = ctx.lines_left.get();
    if budget == 0 {
        return 0.0;
    }
    // A first-line indent takes at most half the line; pictures fit the narrower first line too.
    let first_dx = if first_dx.is_finite() { first_dx.min(frame.w * 0.5) } else { 0.0 };
    let first_frame = frame.shifted(first_dx);
    fit_images(flat, frame.w.min(first_frame.w));
    let gl = glyphs(ctx, flat);
    let mut lines = break_lines_indented(flat, &gl, frame.w, first_frame.w, wrap, budget);
    // A final hard break opens an empty last line (where the caret goes after it). Preformatted text follows
    // HTML, where a trailing newline does not.
    if wrap && lines.len() < budget && ends_with_hard_break(flat, &gl) {
        if let Some(last) = lines.last_mut() {
            last.hard = true;
        }
        lines.push(LineRange { start: gl.len(), end: gl.len(), hard: false });
    }
    ctx.lines_left.set(budget.saturating_sub(lines.len()));
    let nlines = lines.len();
    let mut y = frame.y;
    for (li, lr) in lines.iter().enumerate() {
        let is_last = li.saturating_add(1) == nlines;
        let f = if li == 0 { first_frame } else { frame };
        y += emit_line(flat, &gl, *lr, is_last, align, spacing, path, f, y, out, wrap);
    }
    y - frame.y
}

/// Whether the block text ends with a mandatory line break character (UAX #14 BK, CR, LF, NL).
fn ends_with_hard_break(flat: &Flat, gl: &[Glyph]) -> bool {
    gl.last()
        .filter(|g| g.kind == GKind::Control)
        .and_then(|g| flat.text.get(g.start..g.end))
        .is_some_and(|t| t.ends_with(['\n', '\r', '\u{0b}', '\u{0c}', '\u{85}', '\u{2028}', '\u{2029}']))
}

struct Pending {
    item: DisplayItem,
    run_rect: Option<Rect>,
}

fn emit_line(
    flat: &Flat,
    gl: &[Glyph],
    lr: LineRange,
    is_last: bool,
    align: Align,
    spacing: Spacing,
    path: &[usize],
    frame: Frame,
    top: f32,
    out: &mut Out,
    wrap: bool,
) -> f32 {
    let slice: &[Glyph] = gl.get(lr.start..lr.end).unwrap_or(&[]);
    // Vertical metrics: strut + every run on the line.
    let (mut asc, mut desc, mut gap) = flat
        .strut
        .as_ref()
        .map(|s| (s.metrics.ascent, s.metrics.descent, s.metrics.line_gap))
        .unwrap_or((0.0, 0.0, 0.0));
    for g in slice {
        if let GKind::Image(ii) = g.kind {
            if let Some(img) = flat.images.get(ii) {
                asc = asc.max(img.h);
            }
        } else if let Some(s) = span_style(flat, g) {
            asc = asc.max(s.metrics.ascent + s.shift);
            desc = desc.max(s.metrics.descent - s.shift);
            gap = gap.max(s.metrics.line_gap);
        }
    }
    let content = asc + desc;
    let total = match spacing {
        // Tight spacing never shrinks the box below its ink so lines cannot overlap.
        Spacing::Multiple(m) => ((content + gap) * m).max(content),
        Spacing::Auto => content + gap,
        // Exactly: that tall, but (as for tight multiples) never below the ink, so lines cannot overlap.
        Spacing::Exactly(px) => px.max(content),
        Spacing::AtLeast(px) => (content + gap).max(px),
    };
    let baseline = top + (total - content) / 2.0 + asc;

    // Natural positions.
    let mut nat = Vec::with_capacity(slice.len().saturating_add(1));
    let mut x = 0.0f32;
    for g in slice {
        nat.push(x);
        x += advance(flat, g, x);
    }
    nat.push(x);
    let last_visible = slice.iter().rposition(|g| !g.is_space());
    let visible_w = last_visible.and_then(|k| nat.get(k.saturating_add(1))).copied().unwrap_or(0.0);
    let spaces =
        last_visible.map(|k| slice.iter().take(k).filter(|g| g.kind == GKind::Space).count()).unwrap_or(0);
    let slack = (frame.w - visible_w).max(0.0);
    let justify = align == Align::Justify && !is_last && !lr.hard && spaces > 0 && wrap_ok(slack);
    let extra = if justify { slack / spaces as f32 } else { 0.0 };
    let offset = match align {
        Align::Center => slack / 2.0,
        Align::End => slack,
        Align::Start | Align::Justify => 0.0,
    };
    let base_x = frame.x + offset;

    // Final positions (justification widens interior spaces).
    let mut pos = Vec::with_capacity(nat.len());
    let mut add = 0.0f32;
    for (j, g) in slice.iter().enumerate() {
        pos.push(nat.get(j).copied().unwrap_or(0.0) + add);
        if justify && g.kind == GKind::Space && last_visible.is_some_and(|k| j < k) {
            add += extra;
        }
    }
    pos.push(nat.last().copied().unwrap_or(0.0) + add);

    let mut bg: Vec<DisplayItem> = Vec::new();
    let mut fg: Vec<Pending> = Vec::new();
    let line_rect_h = total;
    let mut j = 0usize;
    while j < slice.len() {
        let Some(g) = slice.get(j) else { break };
        match g.kind {
            GKind::Control | GKind::Tab => {
                j += 1;
            }
            GKind::Image(ii) => {
                if let Some(img) = flat.images.get(ii) {
                    let gx = base_x + pos.get(j).copied().unwrap_or(0.0);
                    let rect = Rect::new(gx, baseline - img.h, img.w, img.h);
                    if let Some(href) = span_link(flat, g) {
                        out.index.links.push(LinkBox { rect, href });
                    }
                    fg.push(Pending {
                        item: DisplayItem::Image { rect, src: img.src.clone(), alt: img.alt.clone() },
                        run_rect: None,
                    });
                }
                j += 1;
            }
            GKind::Normal | GKind::Space => {
                // Extend the run while the span and kind allow.
                let from = j;
                let mut to = j + 1;
                // Spaced (or condensed) characters are drawn one by one at their own positions.
                let spaced = span_style(flat, g).is_some_and(|s| s.letter_spacing != 0.0);
                let ends_here =
                    |k: usize| spaced || (justify && slice.get(k).is_some_and(|g| g.kind == GKind::Space));
                if !ends_here(from) {
                    while let Some(n) = slice.get(to) {
                        if n.span != g.span || !matches!(n.kind, GKind::Normal | GKind::Space) {
                            break;
                        }
                        to += 1;
                        if ends_here(to - 1) {
                            break;
                        }
                    }
                }
                j = to;
                // Trailing spaces at the end of the line are not drawn.
                let vis_to = match last_visible {
                    Some(k) => to.min(k + 1),
                    None => from,
                };
                if vis_to <= from {
                    continue;
                }
                let (Some(first), Some(last)) = (slice.get(from), slice.get(vis_to - 1)) else { continue };
                let Some(text) = flat.text.get(first.start..last.end) else { continue };
                let Some(s) = span_style(flat, g) else { continue };
                let width: f32 =
                    slice.get(from..vis_to).map(|r| r.iter().map(|g| g.width).sum()).unwrap_or(0.0);
                let gx = base_x + pos.get(from).copied().unwrap_or(0.0);
                let by = baseline - s.shift;
                let rect = Rect::new(gx, by - s.metrics.ascent, width, s.metrics.ascent + s.metrics.descent);
                if let Some(h) = s.highlight {
                    bg.push(DisplayItem::FillRect { rect, color: h });
                }
                let link = span_link(flat, g);
                if let Some(href) = &link {
                    out.index
                        .links
                        .push(LinkBox { rect: Rect::new(gx, top, width, line_rect_h), href: href.clone() });
                }
                // Plain single underlines in the text colour are the renderers' own; other underline styles,
                // underline colours and double strikethrough are drawn as lines here.
                let drawn_underline =
                    s.underline && (s.underline_kind != UnderlineKind::Single || s.underline_color.is_some());
                fg.push(Pending {
                    item: DisplayItem::Text {
                        origin: Point::new(gx, by),
                        text: text.to_string(),
                        font: s.font,
                        size_px: s.size_px,
                        color: s.color,
                        underline: s.underline && !drawn_underline,
                        strike: s.strike,
                        link,
                    },
                    run_rect: Some(rect),
                });
                // Letter spacing: the run is one character, so the decoration spans its spaced advance.
                let deco_w = width;
                if drawn_underline {
                    let color = s.underline_color.unwrap_or(s.color);
                    for item in decoration(s, s.underline_kind, gx, by, deco_w, color, true) {
                        fg.push(Pending { item, run_rect: None });
                    }
                }
                if s.double_strike {
                    for item in decoration(s, UnderlineKind::Double, gx, by, deco_w, s.color, false) {
                        fg.push(Pending { item, run_rect: None });
                    }
                }
            }
        }
    }
    for item in bg {
        out.push(item);
    }
    for p in fg {
        let idx = out.push(p.item);
        if let Some(rect) = p.run_rect {
            out.index.runs.push(RunBox { item: idx, rect });
        }
    }

    // Caret stops. Wrapped text keeps the caret inside the content box even where trailing spaces hang past
    // the right edge; preformatted lines may overflow.
    let right = frame.x + frame.w;
    let clamp = |x: f32| if wrap { x.min(right) } else { x };
    let mut stops: Vec<CaretStop> = slice
        .iter()
        .enumerate()
        .map(|(j, g)| CaretStop { offset: g.offset, x: clamp(base_x + pos.get(j).copied().unwrap_or(0.0)) })
        .collect();
    // An empty line after a final hard break starts where the previous glyph ends.
    let line_start = match slice.first() {
        Some(g) => g.offset,
        None => lr
            .start
            .checked_sub(1)
            .and_then(|k| gl.get(k))
            .map(|g| g.offset.saturating_add(glyph_chars(flat, g)))
            .unwrap_or(0),
    };
    let line_end = slice.last().map(|g| g.offset.saturating_add(glyph_chars(flat, g))).unwrap_or(line_start);
    if is_last || slice.is_empty() {
        let end_x = match slice.last() {
            Some(g) if g.kind == GKind::Control => pos.get(slice.len().saturating_sub(1)).copied(),
            _ => pos.last().copied(),
        }
        .unwrap_or(0.0);
        stops.push(CaretStop { offset: line_end, x: clamp(base_x + end_x) });
    }
    let natural = pos.last().copied().unwrap_or(0.0);
    let line_w = if wrap { frame.w } else { frame.w.max(natural) };
    out.index.lines.push(LineBox {
        block_path: path.to_vec(),
        rect: Rect::new(frame.x, top, line_w, total),
        baseline,
        start: line_start,
        end: line_end,
        stops,
    });
    total
}

/// Most line segments one decoration is drawn with (dotted, dashed and wavy lines on very wide runs).
const MAX_DECORATION_SEGMENTS: usize = 2_000;

/// Lines that draw an underline (`under`) or strikethrough of `kind` under / through a run of width `w` at
/// (`x`, baseline `by`). Positions match the renderers' own underline (12 % of the size below the
/// baseline) and strikethrough (30 % above it).
fn decoration(
    s: &RStyle,
    kind: UnderlineKind,
    x: f32,
    by: f32,
    w: f32,
    color: Color,
    under: bool,
) -> Vec<DisplayItem> {
    let size = if s.size_px.is_finite() { s.size_px.clamp(0.0, 400.0) } else { 0.0 };
    let w = if w.is_finite() { w.clamp(0.0, 1.0e6) } else { 0.0 };
    if size <= 0.0 || w <= 0.0 {
        return Vec::new();
    }
    let thick = (size / 14.0).max(1.0);
    let y = if under { by + (size * 0.12).max(1.0) } else { by - size * 0.3 };
    let line = |x0: f32, y0: f32, x1: f32, y1: f32| DisplayItem::Line {
        from: Point::new(x0, y0),
        to: Point::new(x1, y1),
        color,
        width: thick,
    };
    let dashes = |on: f32, off: f32| -> Vec<DisplayItem> {
        let mut v = Vec::new();
        let mut at = x;
        while at < x + w && v.len() < MAX_DECORATION_SEGMENTS {
            v.push(line(at, y, (at + on).min(x + w), y));
            at += on + off;
        }
        v
    };
    match kind {
        UnderlineKind::Single => vec![line(x, y, x + w, y)],
        UnderlineKind::Double => {
            let gap = thick * 1.5;
            if under {
                vec![line(x, y, x + w, y), line(x, y + gap, x + w, y + gap)]
            } else {
                vec![
                    line(x, y - gap / 2.0, x + w, y - gap / 2.0),
                    line(x, y + gap / 2.0, x + w, y + gap / 2.0),
                ]
            }
        }
        UnderlineKind::Dotted => dashes(thick, thick),
        UnderlineKind::Dashed => dashes(thick * 4.0, thick * 2.0),
        UnderlineKind::Wavy => {
            let step = (thick * 2.0).max(1.5);
            let amp = thick;
            let mut v = Vec::new();
            let mut at = x;
            let mut up = true;
            while at < x + w && v.len() < MAX_DECORATION_SEGMENTS {
                let next = (at + step).min(x + w);
                let (y0, y1) = if up { (y + amp, y - amp) } else { (y - amp, y + amp) };
                v.push(line(at, y0, next, y1));
                at = next;
                up = !up;
            }
            v
        }
    }
}

fn wrap_ok(slack: f32) -> bool {
    slack.is_finite() && slack > 0.0
}

const BULLETS: [&str; 3] = ["\u{2022}", "\u{25e6}", "\u{25aa}"];

/// Marker text for item `idx` of a list at `list_depth`.
pub(crate) fn marker_text(kind: &ListKind, idx: usize, list_depth: usize) -> String {
    match kind {
        ListKind::Bullet => {
            BULLETS.get(list_depth % BULLETS.len()).copied().unwrap_or("\u{2022}").to_string()
        }
        ListKind::Numbered { start } => {
            let n = u64::from(*start).saturating_add(u64::try_from(idx).unwrap_or(u64::MAX));
            format!("{n}.")
        }
    }
}

/// Marker formatting follows the size and colour of the item's first text (like a paragraph mark).
fn marker_ts(ts: &TextStyle, item: &[Block]) -> TextStyle {
    let first_text = |inlines: &[Inline]| {
        inlines.iter().find_map(|i| match i {
            Inline::Text { style, .. } => Some(style.clone()),
            _ => None,
        })
    };
    let (base, inline) = match item.first() {
        Some(Block::Heading { level, inlines, .. }) => (heading_style(ts, *level), first_text(inlines)),
        Some(Block::Paragraph { inlines, .. }) => (ts.clone(), first_text(inlines)),
        _ => (ts.clone(), None),
    };
    let size = merge(&base, &inline.clone().unwrap_or_default());
    let colour = inline.and_then(|s| s.color);
    merge(ts, &TextStyle { size_pt: Some(size_pt(&size)), color: colour, ..Default::default() })
}

fn layout_list(
    ctx: &Ctx<'_>,
    kind: &ListKind,
    items: &[Vec<Block>],
    ts: &TextStyle,
    path: &mut Vec<usize>,
    frame: Frame,
    out: &mut Out,
) -> f32 {
    let indent = ctx.px(LIST_INDENT);
    let inner = Frame { list_depth: frame.list_depth.saturating_add(1), ..frame.inset(indent).deeper() };
    let gap = ctx.px(MARKER_GAP);
    let mut y = frame.y;
    for (idx, item) in items.iter().enumerate() {
        let marker_style = ctx.resolve(&marker_ts(ts, item));
        let m = marker_style.metrics;
        path.push(idx);
        let marker_slot = out.placeholder();
        let lines_before = out.index.lines.len();
        let mut h = layout_blocks(ctx, item, ts, path, inner.at_y(y), out);
        path.pop();
        // Sit on the first line's baseline, but never poke out of the item's box.
        let baseline = out
            .index
            .lines
            .get(lines_before)
            .map(|l| l.baseline)
            .unwrap_or(y + m.line_gap / 2.0 + m.ascent)
            .max(y + m.ascent);
        h = h.max(baseline + m.descent - y);
        let text = marker_text(kind, idx, frame.list_depth);
        let mw = ctx.measure(&marker_style, &text);
        // Right of the gap before the content, but never past the list's own box (deep nesting at narrow
        // widths squeezes the content box to nothing).
        let mut mx = (inner.x - gap - mw).max(frame.x);
        let right = frame.x + frame.w;
        if mx + mw > right {
            mx = (right - mw).max(0.0);
        }
        let rect = Rect::new(mx, baseline - m.ascent, mw, m.ascent + m.descent);
        out.set(
            marker_slot,
            DisplayItem::Text {
                origin: Point::new(mx, baseline),
                text,
                font: marker_style.font,
                size_px: marker_style.size_px,
                color: marker_style.color,
                underline: false,
                strike: false,
                link: None,
            },
        );
        out.index.runs.push(RunBox { item: marker_slot, rect });
        y += h;
    }
    y - frame.y
}

/// Min/max-content widths of a block sequence.
pub(crate) fn blocks_min_max(ctx: &Ctx<'_>, blocks: &[Block], ts: &TextStyle, depth: usize) -> (f32, f32) {
    if depth > MAX_DEPTH {
        return (0.0, 0.0);
    }
    blocks.iter().fold((0.0f32, 0.0f32), |(mn, mx), b| {
        let (a, c) = block_min_max(ctx, b, ts, depth);
        (mn.max(a), mx.max(c))
    })
}

fn inline_min_max(ctx: &Ctx<'_>, inlines: &[Inline], ts: &TextStyle) -> (f32, f32) {
    let mut flat = Flat::new(ctx, ts);
    flatten(ctx, inlines, ts, None, &mut flat, 0);
    let gl = glyphs(ctx, &flat);
    min_max(&flat, &gl)
}

fn block_min_max(ctx: &Ctx<'_>, block: &Block, ts: &TextStyle, depth: usize) -> (f32, f32) {
    let d = depth.saturating_add(1);
    let plus = |(a, b): (f32, f32), v: f32| (a + v, b + v);
    match block {
        Block::Paragraph { style, inlines } => {
            plus(inline_min_max(ctx, inlines, ts), ctx.px(style.indent_px) + ctx.px(style.right_indent_px))
        }
        Block::Heading { level, style, inlines } => {
            plus(inline_min_max(ctx, inlines, &heading_style(ts, *level)), ctx.px(style.indent_px))
        }
        Block::List { kind, items } => {
            let (mut mn, mut mx) = (0.0f32, 0.0f32);
            for item in items {
                let (a, b) = blocks_min_max(ctx, item, ts, d);
                mn = mn.max(a);
                mx = mx.max(b);
            }
            // The widest marker must fit even when the content is empty.
            let last = marker_text(kind, items.len().saturating_sub(1), 0);
            let marker = ctx.measure(&ctx.resolve(ts), &last);
            let (mn, mx) = plus((mn, mx), ctx.px(LIST_INDENT));
            (mn.max(marker), mx.max(marker))
        }
        Block::Quote { blocks } => plus(blocks_min_max(ctx, blocks, ts, d), ctx.px(QUOTE_BAR + QUOTE_INDENT)),
        Block::Pre { text } => {
            let pts = pre_style(ts);
            let mut flat = Flat::new(ctx, &pts);
            flat.push_text(text, ctx.resolve(&pts));
            let gl = glyphs(ctx, &flat);
            let (_, mx) = min_max(&flat, &gl);
            (mx, mx)
        }
        Block::Table { rows, border, width_px } => table_min_max(ctx, rows, *border, *width_px, ts, d),
        Block::Rule => (0.0, 0.0),
        Block::Container { style, blocks } => {
            plus(blocks_min_max(ctx, blocks, ts, d), ctx.px(style.indent_px))
        }
    }
}
