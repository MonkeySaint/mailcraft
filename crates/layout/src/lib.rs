//! Rich-text layout (layer L2): [`mailcraft_model::Doc`] + fonts → positioned [`DisplayList`].
//!
//! Line breaking follows UAX #14 (`unicode-linebreak`); runs are measured with `mailcraft-fonts` advances (no
//! complex shaping in v0). Blocks: paragraphs, headings (h1 2em … h6 0.67em), lists (bullets / numbers, 24 px
//! indent per level), quotes (left bar + indent), pre, tables (auto column widths, colspan), rules, containers.
//! Images reserve their box (given size or 16×16 placeholder when unknown) and are emitted as
//! [`DisplayItem::Image`].
//!
//! Interface (frozen for commit 2): [`LayoutOptions`], [`layout_doc`], [`Layout`], [`DisplayList`],
//! [`DisplayItem`], [`Layout::hit`], [`Layout::caret_rect`] (for the compose editor, by [`TextPos`]).
//!
//! Style inheritance: `opts.base` → `doc.base` → block (headings: size × factor + bold; pre: monospace) →
//! inline (links: `opts.link_color` + underline unless the text sets its own). Unset family/size fall back to
//! [`mailcraft_fonts::BODY_FAMILY`] / [`mailcraft_fonts::BODY_SIZE_PT`]; every px value is multiplied by the zoom.
//!
//! Block paths ([`TextPos::block_path`]): the index of the block in its parent sequence, prefixed by the path of
//! the parent. Lists add `[item, block_in_item]`, quotes and containers `[child]`, tables
//! `[row, cell_in_row, block_in_cell]`; e.g. `[2, 0, 1]` is block 1 of item 0 of the list at top-level index 2.
//!
//! Offsets ([`TextPos::offset`]) count chars of the block's flattened inline text, where a line break counts as
//! one char (`'\n'`) and an image as one char (U+FFFC). For `Pre` blocks they count chars of `text`.
//!
//! A paragraph or heading whose text ends with a hard break gets an empty last line (offset = text length),
//! matching the extra `<br>` the HTML serializer writes for it; `Pre` follows HTML and does not.
//!
//! Limits (hostile input): block text is truncated at 4 MiB, nesting past 40 block levels is not laid out,
//! inline link nesting past 64 levels collapses into the outer link, and at most 2^18 text lines are laid out
//! per document (later lines are dropped).

mod block;
mod inline;
mod style;
mod table;
#[cfg(test)]
mod tests;

use mailcraft_fonts::{FontDb, FontId};
use mailcraft_geom::{Color, Point, Rect};
use mailcraft_model::{Doc, TextStyle};

use crate::block::{Ctx, Frame, Out, layout_blocks};

#[derive(Clone, Debug)]
pub struct LayoutOptions {
    /// Available width in px.
    pub width: f32,
    /// Zoom factor (1.0 = 100 %).
    pub zoom: f32,
    /// Defaults for text without explicit style (family, size, colour).
    pub base: TextStyle,
    pub default_color: Color,
    pub link_color: Color,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            width: 600.0,
            zoom: 1.0,
            base: TextStyle::default(),
            default_color: Color::BLACK,
            link_color: Color::hex(0x0563c1),
        }
    }
}

/// Position in the document: block path (indices through nested blocks) + char offset within the flattened
/// inline text of that block.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub block_path: Vec<usize>,
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DisplayItem {
    FillRect {
        rect: Rect,
        color: Color,
    },
    StrokeRect {
        rect: Rect,
        color: Color,
        width: f32,
    },
    Line {
        from: Point,
        to: Point,
        color: Color,
        width: f32,
    },
    /// A run of text on one line. `origin` is the left end of the baseline.
    Text {
        origin: Point,
        text: String,
        font: FontId,
        size_px: f32,
        color: Color,
        underline: bool,
        strike: bool,
        /// Link target when this run is (inside) a link.
        link: Option<String>,
    },
    Image {
        rect: Rect,
        src: String,
        alt: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayList {
    pub items: Vec<DisplayItem>,
}

/// A caret position on a line: char `offset` sits at absolute `x`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CaretStop {
    pub offset: usize,
    pub x: f32,
}

/// One laid-out line of an inline block (paragraph, heading, pre).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LineBox {
    pub block_path: Vec<usize>,
    /// Line box: x/width of the block's content area (wider for overflowing pre lines), y/height of the line.
    pub rect: Rect,
    /// Absolute baseline y.
    pub baseline: f32,
    /// Char range `[start, end)` of the block's flattened text on this line.
    pub start: usize,
    pub end: usize,
    /// Caret stops, ascending: one per grapheme start, plus the block end on the block's last line.
    pub stops: Vec<CaretStop>,
}

/// Clickable area of a link (text run or image).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinkBox {
    pub rect: Rect,
    pub href: String,
}

/// Bounds of a [`DisplayItem::Text`] item (`item` indexes [`DisplayList::items`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RunBox {
    pub item: usize,
    /// Ink box: run x/width, ascent..descent around the baseline.
    pub rect: Rect,
}

/// Hit-testing records produced by [`layout_doc`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayoutIndex {
    pub lines: Vec<LineBox>,
    pub links: Vec<LinkBox>,
    pub runs: Vec<RunBox>,
}

/// A text run with its bounds (see [`Layout::text_runs`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextRun<'a> {
    pub text: &'a str,
    pub rect: Rect,
    pub origin: Point,
    pub font: FontId,
    pub size_px: f32,
    pub color: Color,
    pub underline: bool,
    pub strike: bool,
    pub link: Option<&'a str>,
}

/// Layout result.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    pub display: DisplayList,
    /// Line / link / run records for hit testing and caret placement.
    pub index: LayoutIndex,
}

/// Distance from `v` to the interval `[lo, hi]` (0 inside).
fn interval_dist(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo - v
    } else if v > hi {
        v - hi
    } else {
        0.0
    }
}

impl Layout {
    /// Link under `p`, if any.
    pub fn hit(&self, p: Point) -> Option<&str> {
        self.index.links.iter().rev().find(|l| l.rect.contains(p)).map(|l| l.href.as_str())
    }

    /// Text position under `p` (nearest line, nearest char boundary).
    pub fn hit_pos(&self, p: Point) -> Option<TextPos> {
        if !p.x.is_finite() || !p.y.is_finite() {
            return None;
        }
        let mut best: Option<(f32, f32, &LineBox)> = None;
        for line in &self.index.lines {
            let dy = interval_dist(p.y, line.rect.y, line.rect.bottom());
            let dx = interval_dist(p.x, line.rect.x, line.rect.right());
            let better = match best {
                None => true,
                Some((by, bx, _)) => dy < by || (dy == by && dx < bx),
            };
            if better {
                best = Some((dy, dx, line));
            }
        }
        let (_, _, line) = best?;
        // Nearest stop. Stops can share an x (hanging spaces clamped to the right edge): a click right of them
        // takes the last, a click left of them the first.
        let mut nearest: Option<(f32, usize)> = None;
        for s in &line.stops {
            let d = (s.x - p.x).abs();
            let better = match nearest {
                None => true,
                Some((bd, _)) => d < bd || (d == bd && s.x <= p.x),
            };
            if better {
                nearest = Some((d, s.offset));
            }
        }
        let offset = nearest.map(|(_, o)| o).unwrap_or(line.start);
        Some(TextPos { block_path: line.block_path.clone(), offset })
    }

    /// Caret rectangle (1 px wide) for a text position.
    pub fn caret_rect(&self, pos: &TextPos) -> Option<Rect> {
        let mut last: Option<&LineBox> = None;
        let mut found: Option<&LineBox> = None;
        for line in self.index.lines.iter().filter(|l| l.block_path == pos.block_path) {
            if found.is_none() && pos.offset >= line.start && pos.offset < line.end {
                found = Some(line);
            }
            last = Some(line);
        }
        let line = found.or(last)?;
        let stop = line.stops.iter().rfind(|s| s.offset <= pos.offset).or_else(|| line.stops.first())?;
        Some(Rect::new(stop.x, line.rect.y, 1.0, line.rect.h))
    }

    /// Number of laid-out text lines (paragraph, heading and pre lines; table cells included).
    pub fn line_count(&self) -> usize {
        self.index.lines.len()
    }

    /// All text runs (document text and list markers) in paint order, with their bounds.
    pub fn text_runs(&self) -> Vec<TextRun<'_>> {
        self.index
            .runs
            .iter()
            .filter_map(|r| match self.display.items.get(r.item)? {
                DisplayItem::Text { origin, text, font, size_px, color, underline, strike, link } => {
                    Some(TextRun {
                        text: text.as_str(),
                        rect: r.rect,
                        origin: *origin,
                        font: *font,
                        size_px: *size_px,
                        color: *color,
                        underline: *underline,
                        strike: *strike,
                        link: link.as_deref(),
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// Lines of the block at `path`, top to bottom.
    pub fn lines_of(&self, path: &[usize]) -> Vec<&LineBox> {
        self.index.lines.iter().filter(|l| l.block_path == path).collect()
    }
}

/// Lays out `doc` within `opts.width`.
pub fn layout_doc(doc: &Doc, fonts: &FontDb, opts: &LayoutOptions) -> Layout {
    let ctx = Ctx::new(fonts, opts);
    let mut out = Out::default();
    let bg = doc.background.map(|_| out.placeholder());
    let base = style::merge(&opts.base, &doc.base);
    let mut path = Vec::new();
    let frame = Frame { x: 0.0, y: 0.0, w: ctx.width, depth: 0, list_depth: 0 };
    let height = layout_blocks(&ctx, &doc.blocks, &base, &mut path, frame, &mut out);
    if let (Some(i), Some(color)) = (bg, doc.background) {
        out.set(i, DisplayItem::FillRect { rect: Rect::new(0.0, 0.0, ctx.width, height), color });
    }
    Layout { width: ctx.width, height, display: DisplayList { items: out.items }, index: out.index }
}
