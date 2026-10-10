//! Contact styles: Card (a shaded name bar over "Label: value" lines, cards flowing down two or three
//! columns), Small Booklet (the card layout in smaller type) and Phone Directory (letter headings, then
//! name … phone lines with dot leaders, in two columns).

use mailcraft_fonts::FontDb;
use mailcraft_geom::Rect;
use mailcraft_layout::DisplayItem;

use super::text::{self, Style};
use super::{Frame, GRID, MAX_PAGES, SHADE};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContactStyle {
    #[default]
    Card,
    SmallBooklet,
    PhoneDirectory,
}

/// One contact: the "file as" name, fields for the card styles and phone numbers for the directory.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContactCard {
    pub name: String,
    /// (label, value) lines printed on the card.
    pub fields: Vec<(String, String)>,
    /// (kind, number) pairs for the Phone Directory.
    pub phones: Vec<(String, String)>,
}

/// Most contacts printed.
const MAX_CARDS: usize = 50_000;
/// Most lines per card.
const MAX_CARD_LINES: usize = 60;
const COL_GAP: f32 = 18.0;
const CARD_GAP: f32 = 10.0;

pub(crate) fn pages(
    style: ContactStyle,
    cards: &[ContactCard],
    frame: &Frame,
    fonts: &FontDb,
) -> Vec<Vec<DisplayItem>> {
    let cards = cards.get(..cards.len().min(MAX_CARDS)).unwrap_or(&[]);
    match style {
        ContactStyle::Card => card_pages(cards, frame, fonts, 10.0, 9.0),
        ContactStyle::SmallBooklet => card_pages(cards, frame, fonts, 8.0, 7.0),
        ContactStyle::PhoneDirectory => directory(cards, frame, fonts),
    }
}

/// Columns that fit the body: three on wide (landscape) pages, otherwise two.
fn columns(body: Rect) -> (usize, f32) {
    let n: usize = if body.w > 900.0 { 3 } else { 2 };
    let w = ((body.w - COL_GAP * (n as f32 - 1.0)) / n as f32).max(1.0);
    (n, w)
}

/// Places blocks of known height down columns and across pages.
struct Placer {
    body: Rect,
    n: usize,
    col_w: f32,
    col: usize,
    y: f32,
    pages: Vec<Vec<DisplayItem>>,
    cur: Vec<DisplayItem>,
}

impl Placer {
    fn new(body: Rect) -> Placer {
        let (n, col_w) = columns(body);
        Placer { body, n, col_w, col: 0, y: body.y, pages: Vec::new(), cur: Vec::new() }
    }

    /// Top-left for a block of height `h`, moving to the next column / page when it does not fit. `None` once
    /// the page cap is reached.
    fn place(&mut self, h: f32) -> Option<(f32, f32)> {
        if self.y > self.body.y && self.y + h > self.body.y + self.body.h {
            self.col += 1;
            self.y = self.body.y;
            if self.col >= self.n {
                self.col = 0;
                self.pages.push(std::mem::take(&mut self.cur));
                if self.pages.len() >= MAX_PAGES {
                    return None;
                }
            }
        }
        let x = self.body.x + self.col as f32 * (self.col_w + COL_GAP);
        let at = (x, self.y);
        self.y += h;
        Some(at)
    }

    fn finish(mut self) -> Vec<Vec<DisplayItem>> {
        if !self.cur.is_empty() || self.pages.is_empty() {
            self.pages.push(self.cur);
        }
        self.pages
    }
}

fn card_pages(
    cards: &[ContactCard],
    frame: &Frame,
    fonts: &FontDb,
    name_pt: f32,
    text_pt: f32,
) -> Vec<Vec<DisplayItem>> {
    let mut p = Placer::new(frame.body);
    let name = Style::new(fonts, name_pt, true);
    let label = Style::new(fonts, text_pt, true);
    let value = Style::new(fonts, text_pt, false);
    let lh = value.line_height(fonts);
    let bar = name.line_height(fonts) + 6.0;
    let max_lines = (((frame.body.h - bar - 8.0) / lh).floor().max(1.0) as usize).min(MAX_CARD_LINES);
    for c in cards {
        let w = p.col_w;
        let label_w = (w * 0.32).min(110.0);
        // Lay the lines out first to know the card height.
        let mut lines: Vec<(Option<String>, String)> = Vec::new();
        for (l, v) in &c.fields {
            if v.trim().is_empty() {
                continue;
            }
            for (k, s) in text::wrap(fonts, &value, v, w - label_w - 8.0, 8).into_iter().enumerate() {
                lines.push((if k == 0 { Some(format!("{}:", text::clean(l))) } else { None }, s));
            }
        }
        lines.truncate(max_lines);
        let h = bar + lines.len() as f32 * lh + 6.0;
        let Some((x, y)) = p.place(h + CARD_GAP) else { break };
        p.cur.push(DisplayItem::FillRect { rect: Rect::new(x, y, w, bar), color: SHADE });
        let n = if c.name.trim().is_empty() { "(No name)".to_owned() } else { c.name.clone() };
        p.cur.push(name.text_at(fonts, x + 4.0, y + 3.0, &text::ellipsize(fonts, &name, &n, w - 8.0)));
        p.cur.push(DisplayItem::StrokeRect { rect: Rect::new(x, y, w, h), color: GRID, width: 0.5 });
        let mut ly = y + bar + 3.0;
        for (l, s) in &lines {
            if let Some(l) = l {
                p.cur.push(label.text_at(
                    fonts,
                    x + 4.0,
                    ly,
                    &text::ellipsize(fonts, &label, l, label_w - 4.0),
                ));
            }
            p.cur.push(value.text_at(fonts, x + label_w + 4.0, ly, s));
            ly += lh;
        }
    }
    p.finish()
}

fn directory(cards: &[ContactCard], frame: &Frame, fonts: &FontDb) -> Vec<Vec<DisplayItem>> {
    let mut p = Placer::new(frame.body);
    let letter = Style::new(fonts, 12.0, true);
    let name = Style::new(fonts, 9.0, false);
    let lh = name.line_height(fonts) + 2.0;
    let lbar = letter.line_height(fonts) + 6.0;
    let dot_w = name.measure(fonts, ".").max(0.5);
    let mut current: Option<char> = None;
    for c in cards {
        let n = if c.name.trim().is_empty() { "(No name)".to_owned() } else { text::clean(&c.name) };
        let initial =
            n.chars().find(|ch| ch.is_alphanumeric()).map(|ch| ch.to_uppercase().next().unwrap_or(ch));
        let initial = initial.unwrap_or('#');
        let w = p.col_w;
        if current != Some(initial) {
            current = Some(initial);
            let Some((x, y)) = p.place(lbar + 4.0) else { break };
            p.cur.push(DisplayItem::FillRect { rect: Rect::new(x, y, w, lbar), color: SHADE });
            p.cur.push(letter.text_at(fonts, x + 4.0, y + 3.0, &initial.to_string()));
        }
        let phones: Vec<String> = if c.phones.is_empty() {
            vec![String::new()]
        } else {
            c.phones
                .iter()
                .take(8)
                .map(|(k, v)| {
                    if k.trim().is_empty() {
                        text::clean(v)
                    } else {
                        format!("{}: {}", text::clean(k), text::clean(v))
                    }
                })
                .collect()
        };
        for (i, ph) in phones.iter().enumerate() {
            let Some((x, y)) = p.place(lh) else { return p.finish() };
            let ph = text::ellipsize(fonts, &name, ph, w * 0.55);
            let pw = name.measure(fonts, &ph);
            let left = if i == 0 {
                text::ellipsize(fonts, &name, &n, (w - pw - 12.0).max(w * 0.3))
            } else {
                String::new()
            };
            let nw = name.measure(fonts, &left);
            if !left.is_empty() {
                p.cur.push(name.text_at(fonts, x + 4.0, y, &left));
            }
            if !ph.is_empty() {
                p.cur.push(name.text_at(fonts, x + w - pw, y, &ph));
                let gap = w - pw - nw - 16.0;
                if !left.is_empty() && gap > dot_w * 3.0 {
                    let dots = ((gap / dot_w).floor() as usize).min(400);
                    p.cur.push(name.text_at(fonts, x + 4.0 + nw + 6.0, y, &".".repeat(dots)));
                }
            }
        }
    }
    p.finish()
}
