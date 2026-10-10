//! Table Style: the list as columns. A title line on the first page, a shaded header row repeated on every
//! page, one single-line row per item (cells ellipsized), light rules between rows.

use mailcraft_fonts::FontDb;
use mailcraft_layout::DisplayItem;

use super::text::{self, Style};
use super::{Frame, GRID, MAX_PAGES, SHADE, hrule};

/// A column: title and relative width (weights are normalised over all columns).
#[derive(Clone, Debug, PartialEq)]
pub struct TableColumn {
    pub title: String,
    pub weight: f32,
}

/// Table Style content.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableData {
    /// Printed above the table on the first page (folder name); may be empty.
    pub title: String,
    pub columns: Vec<TableColumn>,
    /// Cell text per row (missing cells print empty).
    pub rows: Vec<Vec<String>>,
    /// Rows printed in bold (unread items), by index into `rows`.
    pub bold: Vec<bool>,
}

const TITLE_PT: f32 = 13.0;
const CELL_PT: f32 = 9.0;
const PAD: f32 = 4.0;
/// Most columns printed.
const MAX_COLUMNS: usize = 32;
/// Most rows printed (the page cap applies too).
const MAX_ROWS: usize = 200_000;

pub(crate) fn pages(t: &TableData, frame: &Frame, fonts: &FontDb) -> Vec<Vec<DisplayItem>> {
    let body = frame.body;
    let title = Style::new(fonts, TITLE_PT, true);
    let head = Style::new(fonts, CELL_PT, true);
    let cell = Style::new(fonts, CELL_PT, false);
    let row_h = cell.line_height(fonts) + 2.0 * PAD;

    let cols: Vec<&TableColumn> = t.columns.iter().take(MAX_COLUMNS).collect();
    let weight = |c: &TableColumn| if c.weight.is_finite() && c.weight > 0.0 { c.weight } else { 1.0 };
    let total: f32 = cols.iter().map(|c| weight(c)).sum::<f32>().max(f32::EPSILON);
    let mut xs = Vec::with_capacity(cols.len() + 1);
    let mut x = body.x;
    for c in &cols {
        xs.push(x);
        x += body.w * weight(c) / total;
    }
    xs.push(body.x + body.w);
    let col_box = |i: usize| -> (f32, f32) {
        let a = xs.get(i).copied().unwrap_or(body.x);
        let b = xs.get(i + 1).copied().unwrap_or(body.x + body.w);
        (a + PAD, (b - a - 2.0 * PAD).max(0.0))
    };

    let header_row = |items: &mut Vec<DisplayItem>, top: f32| {
        items.push(DisplayItem::FillRect {
            rect: mailcraft_geom::Rect::new(body.x, top, body.w, row_h),
            color: SHADE,
        });
        for (i, c) in cols.iter().enumerate() {
            let (cx, cw) = col_box(i);
            items.push(head.text_at(fonts, cx, top + PAD, &text::ellipsize(fonts, &head, &c.title, cw)));
        }
        items.push(hrule(body.x, body.x + body.w, top + row_h - 1.0, 1.0, GRID));
    };

    let mut pages: Vec<Vec<DisplayItem>> = Vec::new();
    let mut items = Vec::new();
    let mut y = body.y;
    if !t.title.trim().is_empty() {
        items.push(title.text_at(fonts, body.x, y, &text::ellipsize(fonts, &title, &t.title, body.w)));
        y += title.line_height(fonts) + 8.0;
    }
    header_row(&mut items, y);
    y += row_h;
    for (r, row) in t.rows.iter().take(MAX_ROWS).enumerate() {
        if y + row_h > body.y + body.h {
            pages.push(std::mem::take(&mut items));
            if pages.len() >= MAX_PAGES {
                return pages;
            }
            y = body.y;
            header_row(&mut items, y);
            y += row_h;
        }
        let st = if t.bold.get(r).copied().unwrap_or(false) { &head } else { &cell };
        for i in 0..cols.len() {
            let (cx, cw) = col_box(i);
            let s = row.get(i).map(String::as_str).unwrap_or("");
            if !s.is_empty() {
                items.push(st.text_at(fonts, cx, y + PAD, &text::ellipsize(fonts, st, s, cw)));
            }
        }
        items.push(hrule(body.x, body.x + body.w, y + row_h - 0.5, 0.5, SHADE));
        y += row_h;
    }
    pages.push(items);
    pages
}
