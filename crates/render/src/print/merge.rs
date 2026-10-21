//! Mail Merge documents: Form Letters (each merged letter starts on a new page and flows onto more pages when
//! it is long), Catalog (the merged records one after the other, a blank line apart, flowing from page to
//! page), Mailing Labels (one address per label, filling the cells of a label sheet row by row) and
//! Envelopes (one envelope per page: the return address in the top-left corner, the delivery address in the
//! middle). The merged text of each record comes ready-made from the caller; this module only places it.
//!
//! Label sheets are measured from the paper's edge (their own margins), so the page setup's margins and the
//! header and footer do not apply to them. Envelopes use the page setup's margins for the return address.

use mailcraft_fonts::FontDb;
use mailcraft_geom::Rect;
use mailcraft_layout::DisplayItem;

use super::text::{self, Style};
use super::{Frame, MAX_PAGES, PX_PER_IN, Paper};

/// Most records merged into one document.
pub const MAX_RECORDS: usize = 50_000;
/// Most lines of one form letter, and the characters wrapped at a time.
const MAX_LETTER_LINES: usize = 5_000;
const LETTER_CHUNK: usize = 2_000;
/// Letter text size (pt).
const LETTER_PT: f32 = 11.0;
/// Label text size (pt), and the smallest it shrinks to when an address has many lines.
const LABEL_PT: f32 = 10.0;
const LABEL_MIN_PT: f32 = 7.0;
/// Space kept inside each label's edge (px).
const LABEL_PAD: f32 = 6.0;
/// Envelope text sizes (pt): delivery address, return address.
const ADDRESS_PT: f32 = 12.0;
const RETURN_PT: f32 = 9.0;
/// Most lines printed for one address (labels, envelopes).
const MAX_ADDRESS_LINES: usize = 12;

/// A label sheet: rows × columns of equal labels at fixed places on the paper (inches).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelSheet {
    pub paper: Paper,
    pub cols: u8,
    pub rows: u8,
    /// Label width and height.
    pub w_in: f32,
    pub h_in: f32,
    /// Distance from the paper's top and left edges to the first label.
    pub top_in: f32,
    pub left_in: f32,
    /// Space between labels across and down.
    pub gap_x_in: f32,
    pub gap_y_in: f32,
}

impl LabelSheet {
    /// Labels on one sheet.
    pub fn per_sheet(&self) -> usize {
        usize::from(self.cols.max(1)).saturating_mul(usize::from(self.rows.max(1)))
    }

    /// Rectangle (page px) of label `i` on its sheet (`i` < [`Self::per_sheet`]).
    pub fn cell(&self, i: usize) -> Rect {
        let cols = usize::from(self.cols.max(1));
        let (row, col) = (i / cols, i % cols);
        let x = (self.left_in + col as f32 * (self.w_in + self.gap_x_in)) * PX_PER_IN;
        let y = (self.top_in + row as f32 * (self.h_in + self.gap_y_in)) * PX_PER_IN;
        Rect::new(x, y, self.w_in * PX_PER_IN, self.h_in * PX_PER_IN)
    }
}

const MM: f32 = 1.0 / 25.4;

/// The label sheets offered: (id, label, sheet). Common generic layouts; the ids are what the commands take.
pub const LABEL_SHEETS: [(&str, &str, LabelSheet); 4] = [
    (
        "30",
        "30 per sheet, 2 5/8 x 1 in (Letter)",
        LabelSheet {
            paper: Paper::Letter,
            cols: 3,
            rows: 10,
            w_in: 2.625,
            h_in: 1.0,
            top_in: 0.5,
            left_in: 0.1875,
            gap_x_in: 0.125,
            gap_y_in: 0.0,
        },
    ),
    (
        "14",
        "14 per sheet, 4 x 1 1/3 in (Letter)",
        LabelSheet {
            paper: Paper::Letter,
            cols: 2,
            rows: 7,
            w_in: 4.0,
            h_in: 1.333,
            top_in: 0.833,
            left_in: 0.156,
            gap_x_in: 0.1875,
            gap_y_in: 0.0,
        },
    ),
    (
        "10",
        "10 per sheet, 4 x 2 in (Letter)",
        LabelSheet {
            paper: Paper::Letter,
            cols: 2,
            rows: 5,
            w_in: 4.0,
            h_in: 2.0,
            top_in: 0.5,
            left_in: 0.156,
            gap_x_in: 0.1875,
            gap_y_in: 0.0,
        },
    ),
    (
        "21",
        "21 per sheet, 63.5 x 38.1 mm (A4)",
        LabelSheet {
            paper: Paper::A4,
            cols: 3,
            rows: 7,
            w_in: 63.5 * MM,
            h_in: 38.1 * MM,
            top_in: 15.15 * MM,
            left_in: 7.2 * MM,
            gap_x_in: 2.5 * MM,
            gap_y_in: 0.0,
        },
    ),
];

/// The label sheet with id `id` ("30", "14", "10", "21").
pub fn label_sheet(id: &str) -> Option<LabelSheet> {
    LABEL_SHEETS.iter().find(|(k, _, _)| k.eq_ignore_ascii_case(id.trim())).map(|(_, _, s)| *s)
}

/// What a merge document is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MergeStyle {
    Letters,
    /// Every record follows the previous one on the same page.
    Catalog,
    Labels(LabelSheet),
    Envelopes,
}

/// A merge document: the style and the merged text of each record (lines separated by `\n`).
#[derive(Clone, Debug, PartialEq)]
pub struct MergeDoc {
    pub style: MergeStyle,
    pub records: Vec<String>,
    /// Envelopes: the sender's address printed in the top-left corner (may be empty).
    pub return_address: String,
}

/// Most records a document of `style` can hold: one page each for letters (more when a letter runs long)
/// and envelopes, a sheet of labels per page; never more than [`MAX_RECORDS`].
pub fn max_records(style: &MergeStyle) -> usize {
    let n = match style {
        MergeStyle::Letters | MergeStyle::Envelopes => MAX_PAGES,
        MergeStyle::Catalog => MAX_RECORDS,
        MergeStyle::Labels(sheet) => sheet.per_sheet().max(1).saturating_mul(MAX_PAGES),
    };
    n.min(MAX_RECORDS)
}

/// The pages of `doc` and how many of its records they hold in full (fewer than all only when the document
/// reached [`MAX_PAGES`]).
pub(crate) fn pages(doc: &MergeDoc, frame: &Frame, fonts: &FontDb) -> (Vec<Vec<DisplayItem>>, usize) {
    let records = doc.records.get(..doc.records.len().min(MAX_RECORDS)).unwrap_or(&[]);
    match doc.style {
        MergeStyle::Letters => letters(records, frame, fonts),
        MergeStyle::Catalog => catalog(records, frame, fonts),
        MergeStyle::Labels(sheet) => labels(records, &sheet, fonts),
        MergeStyle::Envelopes => envelopes(records, &doc.return_address, frame, fonts),
    }
}

fn letters(records: &[String], frame: &Frame, fonts: &FontDb) -> (Vec<Vec<DisplayItem>>, usize) {
    let body = frame.body;
    let st = Style::new(fonts, LETTER_PT, false);
    let lh = st.line_height(fonts);
    let mut pages: Vec<Vec<DisplayItem>> = Vec::new();
    let mut placed = 0usize;
    for rec in records {
        if pages.len() >= MAX_PAGES {
            break;
        }
        let mut cur = Vec::new();
        let mut y = body.y;
        for line in letter_lines(fonts, &st, rec, body.w) {
            if y + lh > body.y + body.h && y > body.y {
                pages.push(std::mem::take(&mut cur));
                if pages.len() >= MAX_PAGES {
                    // This letter is cut off: it does not count as merged.
                    return (pages, placed);
                }
                y = body.y;
            }
            if !line.is_empty() {
                cur.push(st.text_at(fonts, body.x, y, &line));
            }
            y += lh;
        }
        pages.push(cur);
        placed = placed.saturating_add(1);
    }
    (pages, placed)
}

/// Catalog: the records flow one after the other, a blank line between two, onto as many pages as they take.
/// A record counts as placed once its last line is on a page.
fn catalog(records: &[String], frame: &Frame, fonts: &FontDb) -> (Vec<Vec<DisplayItem>>, usize) {
    let body = frame.body;
    let st = Style::new(fonts, LETTER_PT, false);
    let lh = st.line_height(fonts);
    let mut pages: Vec<Vec<DisplayItem>> = Vec::new();
    let mut cur = Vec::new();
    let mut y = body.y;
    let mut placed = 0usize;
    for (i, rec) in records.iter().enumerate() {
        let mut lines = letter_lines(fonts, &st, rec, body.w);
        // Trailing blank lines of a record would double the gap.
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if i > 0 {
            lines.insert(0, String::new());
        }
        for line in lines {
            if y + lh > body.y + body.h && y > body.y {
                pages.push(std::mem::take(&mut cur));
                if pages.len() >= MAX_PAGES {
                    return (pages, placed);
                }
                y = body.y;
                if line.is_empty() {
                    // No blank line at the top of a page.
                    continue;
                }
            }
            if !line.is_empty() {
                cur.push(st.text_at(fonts, body.x, y, &line));
            }
            y += lh;
        }
        placed = placed.saturating_add(1);
    }
    pages.push(cur);
    (pages, placed)
}

/// A letter's text wrapped to `w`, paragraph by paragraph (long paragraphs in pieces, so a letter is not cut
/// at the per-field limit of the wrapping helper); at most [`MAX_LETTER_LINES`] lines.
fn letter_lines(fonts: &FontDb, st: &Style, rec: &str, w: f32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for para in rec.split('\n') {
        if out.len() >= MAX_LETTER_LINES {
            break;
        }
        if para.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let chars: Vec<char> = para.chars().collect();
        let mut start = 0usize;
        while start < chars.len() && out.len() < MAX_LETTER_LINES {
            let mut end = start.saturating_add(LETTER_CHUNK).min(chars.len());
            if end < chars.len()
                && let Some(space) =
                    chars.get(start..end).and_then(|c| c.iter().rposition(|c| c.is_whitespace()))
                && space > 0
            {
                end = start + space + 1;
            }
            let piece: String = chars.get(start..end).unwrap_or(&[]).iter().collect();
            out.extend(text::wrap(fonts, st, &piece, w, text::MAX_WRAP_LINES));
            start = end;
        }
    }
    out.truncate(MAX_LETTER_LINES);
    out
}

/// Lines of `s` wrapped to `w` in the largest size from `pt` down to `min_pt` that fits `h` (else the
/// smallest, cut to fit). Returns the style and the lines.
fn fit(fonts: &FontDb, s: &str, w: f32, h: f32, pt: f32, min_pt: f32) -> (Style, Vec<String>) {
    let mut size = pt;
    loop {
        let st = Style::new(fonts, size, false);
        let lh = st.line_height(fonts).max(1.0);
        let room = ((h / lh).floor().max(1.0) as usize).min(MAX_ADDRESS_LINES);
        let all = text::wrap(fonts, &st, s, w, MAX_ADDRESS_LINES.saturating_add(1));
        if all.len() <= room || size <= min_pt {
            let lines = if all.len() <= room { all } else { text::wrap(fonts, &st, s, w, room) };
            return (st, lines);
        }
        size = (size - 1.0).max(min_pt);
    }
}

fn labels(records: &[String], sheet: &LabelSheet, fonts: &FontDb) -> (Vec<Vec<DisplayItem>>, usize) {
    let per = sheet.per_sheet().max(1);
    let mut pages: Vec<Vec<DisplayItem>> = Vec::new();
    let mut placed = 0usize;
    for (i, rec) in records.iter().enumerate() {
        let slot = i % per;
        if slot == 0 {
            if pages.len() >= MAX_PAGES {
                break;
            }
            pages.push(Vec::new());
        }
        placed = placed.saturating_add(1);
        let cell = sheet.cell(slot);
        let inner = Rect::new(
            cell.x + LABEL_PAD,
            cell.y + LABEL_PAD,
            (cell.w - 2.0 * LABEL_PAD).max(1.0),
            (cell.h - 2.0 * LABEL_PAD).max(1.0),
        );
        let (st, lines) = fit(fonts, rec, inner.w, inner.h, LABEL_PT, LABEL_MIN_PT);
        let lh = st.line_height(fonts);
        // Centred down the label.
        let used = lines.len() as f32 * lh;
        let mut y = inner.y + ((inner.h - used) / 2.0).max(0.0);
        if let Some(page) = pages.last_mut() {
            for l in &lines {
                if !l.is_empty() {
                    page.push(st.text_at(fonts, inner.x, y, l));
                }
                y += lh;
            }
        }
    }
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    (pages, placed)
}

fn envelopes(
    records: &[String],
    from: &str,
    frame: &Frame,
    fonts: &FontDb,
) -> (Vec<Vec<DisplayItem>>, usize) {
    let body = frame.body;
    let ret = Style::new(fonts, RETURN_PT, false);
    let rlh = ret.line_height(fonts);
    let ret_lines = text::wrap(fonts, &ret, from, (body.w * 0.45).max(1.0), MAX_ADDRESS_LINES / 2);
    let mut pages = Vec::new();
    for rec in records.iter().take(MAX_PAGES) {
        let mut page = Vec::new();
        let mut y = body.y;
        for l in &ret_lines {
            if !l.is_empty() {
                page.push(ret.text_at(fonts, body.x, y, l));
            }
            y += rlh;
        }
        // The delivery address starts a little left of the middle, a little above the middle.
        let x = body.x + body.w * 0.40;
        let top = body.y + body.h * 0.40;
        let room = Rect::new(x, top, (body.x + body.w - x).max(1.0), (body.y + body.h - top).max(1.0));
        let (st, lines) = fit(fonts, rec, room.w, room.h, ADDRESS_PT, RETURN_PT);
        let lh = st.line_height(fonts);
        let mut y = room.y;
        for l in &lines {
            if !l.is_empty() {
                page.push(st.text_at(fonts, room.x, y, l));
            }
            y += lh;
        }
        pages.push(page);
    }
    let placed = pages.len();
    (pages, placed)
}
