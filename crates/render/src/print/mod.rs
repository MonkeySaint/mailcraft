//! Print layout (layer L3): turns print content into pages of display lists, ready for the PDF writer
//! ([`crate::pdf`]) or the print preview ([`render_page`]).
//!
//! Styles (observed names): Memo Style (one item after another: user-name band, bold-label header block, rule,
//! body laid out with `mailcraft-layout`), Table Style (the message list as columns, header repeated on every
//! page), Calendar Daily / Weekly / Monthly (grids), Card / Small Booklet / Phone Directory for contacts, and
//! the Mail Merge documents (form letters, mailing labels, envelopes).
//!
//! Units: page coordinates are logical px (1/96 in) with the origin at the top-left corner of the page, like
//! every other display list. The PDF writer converts to points.
//!
//! Every page gets the [`PageSetup`] header and footer (three slots each: left, centre, right). Slots may hold
//! the fields [`FIELD_PAGE`], [`FIELD_TOTAL`], [`FIELD_DATE`], [`FIELD_TIME`] and [`FIELD_USER`].
//!
//! Hostile input is bounded: at most [`MAX_PAGES`] pages, field text is capped and control characters are
//! blanked, wrapped fields stop after a few hundred lines.

mod calendar;
mod cards;
mod flow;
mod memo;
mod merge;
mod table;
#[cfg(test)]
mod tests;
mod text;

use mailcraft_fonts::FontDb;
use mailcraft_geom::{Color, Point, Rect};
use mailcraft_layout::{DisplayItem, DisplayList};

pub use calendar::{CalendarData, CalendarEvent, CalendarStyle};
pub use cards::{ContactCard, ContactStyle};
pub use memo::MemoItem;
pub use merge::{LABEL_SHEETS, LabelSheet, MAX_RECORDS, MergeDoc, MergeStyle, label_sheet, max_records};
pub use table::{TableColumn, TableData};

use crate::Canvas;
use text::Style;

/// Most pages one print job produces (later content is dropped).
pub const MAX_PAGES: usize = 2_000;

/// Header/footer field: the page number.
pub const FIELD_PAGE: &str = "[Page #]";
/// Header/footer field: the number of pages.
pub const FIELD_TOTAL: &str = "[Total Pages]";
/// Header/footer field: the date the job was printed.
pub const FIELD_DATE: &str = "[Date Printed]";
/// Header/footer field: the time the job was printed.
pub const FIELD_TIME: &str = "[Time Printed]";
/// Header/footer field: the user's name.
pub const FIELD_USER: &str = "[User Name]";
/// All header/footer fields, for pickers.
pub const FIELDS: [&str; 5] = [FIELD_PAGE, FIELD_TOTAL, FIELD_DATE, FIELD_TIME, FIELD_USER];

/// Logical px per inch.
const PX_PER_IN: f32 = 96.0;
/// Header / footer text size in pt.
const HF_PT: f32 = 9.0;
/// Space between a header/footer band and the body area.
const HF_GAP: f32 = 8.0;
/// Smallest body area side accepted; tighter margins are reduced to keep this much room.
const MIN_BODY: f32 = 96.0;

/// Paper sizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Paper {
    #[default]
    Letter,
    Legal,
    A4,
    A5,
    /// Envelope #10 (4 1/8 × 9 1/2 in).
    Envelope10,
    /// Envelope DL (110 × 220 mm).
    EnvelopeDl,
}

impl Paper {
    pub const ALL: [Paper; 6] =
        [Paper::Letter, Paper::Legal, Paper::A4, Paper::A5, Paper::Envelope10, Paper::EnvelopeDl];

    /// Portrait width × height in logical px.
    pub fn size_px(self) -> (f32, f32) {
        let mm = |w: f32, h: f32| (w * PX_PER_IN / 25.4, h * PX_PER_IN / 25.4);
        match self {
            Paper::Letter => (8.5 * PX_PER_IN, 11.0 * PX_PER_IN),
            Paper::Legal => (8.5 * PX_PER_IN, 14.0 * PX_PER_IN),
            Paper::A4 => mm(210.0, 297.0),
            Paper::A5 => mm(148.0, 210.0),
            Paper::Envelope10 => (4.125 * PX_PER_IN, 9.5 * PX_PER_IN),
            Paper::EnvelopeDl => mm(110.0, 220.0),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Paper::Letter => "Letter",
            Paper::Legal => "Legal",
            Paper::A4 => "A4",
            Paper::A5 => "A5",
            Paper::Envelope10 => "Envelope #10",
            Paper::EnvelopeDl => "Envelope DL",
        }
    }

    /// Parses a paper name (case-insensitive).
    pub fn parse(s: &str) -> Option<Paper> {
        Paper::ALL.into_iter().find(|p| p.name().eq_ignore_ascii_case(s.trim()))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    #[default]
    Portrait,
    Landscape,
}

/// Page margins in logical px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Margins {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

impl Default for Margins {
    /// Half an inch all round.
    fn default() -> Self {
        let m = 0.5 * PX_PER_IN;
        Margins { top: m, bottom: m, left: m, right: m }
    }
}

/// Page Setup: paper, orientation, margins, header and footer.
#[derive(Clone, Debug, PartialEq)]
pub struct PageSetup {
    pub paper: Paper,
    pub orientation: Orientation,
    pub margins: Margins,
    /// Header slots: left, centre, right (text with fields).
    pub header: [String; 3],
    /// Footer slots: left, centre, right.
    pub footer: [String; 3],
    /// Memo Style: start each item on a new page.
    pub new_page_per_item: bool,
}

impl Default for PageSetup {
    fn default() -> Self {
        PageSetup {
            paper: Paper::Letter,
            orientation: Orientation::Portrait,
            margins: Margins::default(),
            header: Default::default(),
            footer: [String::new(), FIELD_PAGE.to_owned(), String::new()],
            new_page_per_item: true,
        }
    }
}

impl PageSetup {
    /// Page width × height in logical px, after orientation.
    pub fn page_size(&self) -> (f32, f32) {
        let (w, h) = self.paper.size_px();
        match self.orientation {
            Orientation::Portrait => (w, h),
            Orientation::Landscape => (h, w),
        }
    }

    /// Margins made finite, non-negative and small enough to leave a usable body area.
    fn margins(&self) -> Margins {
        let (w, h) = self.page_size();
        let ok = |v: f32| if v.is_finite() { v.clamp(0.0, 4.0 * PX_PER_IN) } else { 0.5 * PX_PER_IN };
        let mut m = Margins {
            top: ok(self.margins.top),
            bottom: ok(self.margins.bottom),
            left: ok(self.margins.left),
            right: ok(self.margins.right),
        };
        let fit = |a: &mut f32, b: &mut f32, total: f32| {
            let room = total - MIN_BODY;
            if *a + *b > room {
                let k = room.max(0.0) / (*a + *b).max(1.0);
                *a *= k;
                *b *= k;
            }
        };
        fit(&mut m.left, &mut m.right, w);
        fit(&mut m.top, &mut m.bottom, h);
        m
    }
}

/// What the header/footer fields expand to, plus the job title (PDF metadata).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrintContext {
    pub user_name: String,
    pub date_printed: String,
    pub time_printed: String,
    pub title: String,
}

/// The content of one print job.
#[derive(Clone, Debug)]
pub enum PrintContent {
    Memo(Vec<MemoItem>),
    Table(TableData),
    Calendar(CalendarData),
    Contacts {
        style: ContactStyle,
        cards: Vec<ContactCard>,
    },
    /// Mail Merge: form letters, mailing labels or envelopes.
    Merge(MergeDoc),
}

/// One laid-out page (logical px, origin top-left).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub width: f32,
    pub height: f32,
    pub display: DisplayList,
}

/// The body area of every page (inside margins, header and footer bands), in page coordinates.
pub(crate) struct Frame {
    pub body: Rect,
}

/// Lays `content` out into pages.
pub fn paginate(content: &PrintContent, setup: &PageSetup, ctx: &PrintContext, fonts: &FontDb) -> Vec<Page> {
    let frame = frame(setup, fonts);
    let bodies = match content {
        PrintContent::Memo(items) => memo::pages(items, setup, ctx, &frame, fonts),
        PrintContent::Table(t) => table::pages(t, &frame, fonts),
        PrintContent::Calendar(c) => calendar::pages(c, &frame, fonts),
        PrintContent::Contacts { style, cards } => cards::pages(*style, cards, &frame, fonts),
        PrintContent::Merge(doc) => merge::pages(doc, &frame, fonts).0,
    };
    finish(bodies, setup, ctx, fonts)
}

/// Lays a Mail Merge document out into pages; also returns how many of its records the pages hold in full
/// (fewer than all when the document reached [`MAX_PAGES`]).
pub fn paginate_merge(
    doc: &MergeDoc,
    setup: &PageSetup,
    ctx: &PrintContext,
    fonts: &FontDb,
) -> (Vec<Page>, usize) {
    let (bodies, placed) = merge::pages(doc, &frame(setup, fonts), fonts);
    (finish(bodies, setup, ctx, fonts), placed)
}

/// The body area of `setup`'s pages.
fn frame(setup: &PageSetup, fonts: &FontDb) -> Frame {
    let (w, h) = setup.page_size();
    let m = setup.margins();
    let band = Style::new(fonts, HF_PT, false).line_height(fonts);
    let has = |slots: &[String; 3]| slots.iter().any(|s| !s.trim().is_empty());
    let head = if has(&setup.header) { band + HF_GAP } else { 0.0 };
    let foot = if has(&setup.footer) { band + HF_GAP } else { 0.0 };
    let body_h = (h - m.top - m.bottom - head - foot).max(MIN_BODY / 2.0);
    Frame { body: Rect::new(m.left, m.top + head, (w - m.left - m.right).max(1.0), body_h) }
}

/// Page bodies made into pages: at most [`MAX_PAGES`], each with the header and footer.
fn finish(
    mut bodies: Vec<Vec<DisplayItem>>,
    setup: &PageSetup,
    ctx: &PrintContext,
    fonts: &FontDb,
) -> Vec<Page> {
    let (w, h) = setup.page_size();
    let m = setup.margins();
    let hf = Style::new(fonts, HF_PT, false).color(Color::rgb(0x40, 0x40, 0x40));
    let band = hf.line_height(fonts);
    bodies.truncate(MAX_PAGES);
    if bodies.is_empty() {
        bodies.push(Vec::new());
    }
    let total = bodies.len();
    bodies
        .into_iter()
        .enumerate()
        .map(|(i, mut items)| {
            let n = i + 1;
            let top = m.top + hf.ascent(fonts);
            let bottom = h - m.bottom - band + hf.ascent(fonts);
            slots(&mut items, &setup.header, top, m.left, w - m.right, &hf, fonts, ctx, n, total);
            slots(&mut items, &setup.footer, bottom, m.left, w - m.right, &hf, fonts, ctx, n, total);
            Page { width: w, height: h, display: DisplayList { items } }
        })
        .collect()
}

/// Draws the three header/footer slots on one baseline.
fn slots(
    items: &mut Vec<DisplayItem>,
    slots: &[String; 3],
    baseline: f32,
    left: f32,
    right: f32,
    st: &Style,
    fonts: &FontDb,
    ctx: &PrintContext,
    page: usize,
    total: usize,
) {
    let width = (right - left).max(0.0);
    for (i, raw) in slots.iter().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        let s = text::ellipsize(fonts, st, &expand_fields(raw, ctx, page, total), width / 3.0 + width / 6.0);
        let tw = st.measure(fonts, &s);
        let x = match i {
            0 => left,
            1 => left + (width - tw) / 2.0,
            _ => right - tw,
        };
        items.push(st.text(x, baseline, &s));
    }
}

/// Replaces the header/footer fields in `s`.
pub fn expand_fields(s: &str, ctx: &PrintContext, page: usize, total: usize) -> String {
    s.replace(FIELD_PAGE, &page.to_string())
        .replace(FIELD_TOTAL, &total.to_string())
        .replace(FIELD_DATE, &ctx.date_printed)
        .replace(FIELD_TIME, &ctx.time_printed)
        .replace(FIELD_USER, &ctx.user_name)
}

/// Renders `page` for the print preview at `scale` device px per logical px (white paper; images as
/// placeholders). An empty canvas for unusable scales.
pub fn render_page(page: &Page, fonts: &FontDb, scale: f32) -> Canvas {
    if !scale.is_finite() || scale <= 0.0 {
        return Canvas::default();
    }
    let dim = |v: f32| -> u32 {
        let d = (v * scale).ceil();
        if d.is_finite() && d >= 1.0 { d.min(crate::MAX_DIMENSION as f32) as u32 } else { 0 }
    };
    let (w, h) = (dim(page.width), dim(page.height));
    crate::render(&page.display, fonts, w, h, scale, Color::WHITE, &|_| None)
}

/// A horizontal rule item.
pub(crate) fn hrule(x0: f32, x1: f32, y: f32, width: f32, color: Color) -> DisplayItem {
    DisplayItem::FillRect { rect: Rect::new(x0, y, (x1 - x0).max(0.0), width), color }
}

/// A line item.
pub(crate) fn line(a: (f32, f32), b: (f32, f32), width: f32, color: Color) -> DisplayItem {
    DisplayItem::Line { from: Point::new(a.0, a.1), to: Point::new(b.0, b.1), color, width }
}

/// Grid line colour for tables and calendars.
pub(crate) const GRID: Color = Color::rgb(0x80, 0x80, 0x80);
/// Light shading (header rows, day titles).
pub(crate) const SHADE: Color = Color::rgb(0xe6, 0xe6, 0xe6);
