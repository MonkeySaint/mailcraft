//! Small text helpers for the hand-drawn print styles (table, calendar, cards): resolve a face, emit a text
//! item, measure, ellipsize and greedily wrap. Body text goes through `mailcraft-layout` instead.

use mailcraft_fonts::{FontDb, FontId};
use mailcraft_geom::{Color, Point, pt_to_px};
use mailcraft_layout::DisplayItem;

/// Family used for print chrome (headers, labels, grids). Resolves to a bundled sans.
pub(crate) const PRINT_FAMILY: &str = "Aptos";
/// Most characters kept from one field before measuring (hostile input can be megabytes long).
const MAX_FIELD_CHARS: usize = 4_096;
/// Most wrapped lines produced for one field.
pub(crate) const MAX_WRAP_LINES: usize = 400;

/// A resolved face at a size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Style {
    pub font: FontId,
    pub size_px: f32,
    pub color: Color,
}

impl Style {
    pub(crate) fn new(fonts: &FontDb, size_pt: f32, bold: bool) -> Style {
        let font = fonts.resolve(PRINT_FAMILY, if bold { 700 } else { 400 }, false);
        Style { font, size_px: pt_to_px(size_pt), color: Color::BLACK }
    }

    pub(crate) fn color(mut self, color: Color) -> Style {
        self.color = color;
        self
    }

    /// Ascent in px (baseline offset from the top of a line).
    pub(crate) fn ascent(&self, fonts: &FontDb) -> f32 {
        fonts.metrics(self.font, self.size_px).ascent
    }

    /// Line height in px.
    pub(crate) fn line_height(&self, fonts: &FontDb) -> f32 {
        let h = fonts.metrics(self.font, self.size_px).line_height();
        if h.is_finite() && h > 0.0 { h } else { self.size_px * 1.2 }
    }

    pub(crate) fn measure(&self, fonts: &FontDb, s: &str) -> f32 {
        fonts.measure(self.font, s, self.size_px)
    }

    /// A text item with its baseline at `y`.
    pub(crate) fn text(&self, x: f32, baseline: f32, s: &str) -> DisplayItem {
        DisplayItem::Text {
            origin: Point::new(x, baseline),
            text: s.to_owned(),
            font: self.font,
            size_px: self.size_px,
            color: self.color,
            underline: false,
            strike: false,
            link: None,
        }
    }

    /// Text whose line box starts at `top`.
    pub(crate) fn text_at(&self, fonts: &FontDb, x: f32, top: f32, s: &str) -> DisplayItem {
        self.text(x, top + self.ascent(fonts), s)
    }
}

/// `s` with control characters turned into spaces and capped at [`MAX_FIELD_CHARS`].
pub(crate) fn clean(s: &str) -> String {
    s.chars().take(MAX_FIELD_CHARS).map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// `s` cut to fit `max_w`, ending in "…" when shortened.
pub(crate) fn ellipsize(fonts: &FontDb, st: &Style, s: &str, max_w: f32) -> String {
    let s = clean(s);
    if max_w <= 0.0 || !max_w.is_finite() {
        return String::new();
    }
    if st.measure(fonts, &s) <= max_w {
        return s;
    }
    let ell = st.measure(fonts, "…");
    let mut out = String::new();
    let mut w = 0.0;
    for c in s.chars() {
        let cw = fonts.advance(st.font, c, st.size_px);
        if w + cw + ell > max_w {
            break;
        }
        w += cw;
        out.push(c);
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.push('…');
    out
}

/// Greedy word wrap of `s` (newlines kept) into lines no wider than `max_w`; words wider than a line are
/// broken between characters. At most `max_lines` lines (the last one ellipsized when text remains).
pub(crate) fn wrap(fonts: &FontDb, st: &Style, s: &str, max_w: f32, max_lines: usize) -> Vec<String> {
    let max_lines = max_lines.min(MAX_WRAP_LINES);
    let mut lines: Vec<String> = Vec::new();
    if max_lines == 0 || max_w <= 0.0 || !max_w.is_finite() {
        return lines;
    }
    let text: String = s.chars().take(MAX_FIELD_CHARS).collect();
    let space = st.measure(fonts, " ");
    let mut truncated = false;
    'paras: for para in text.split('\n') {
        let mut cur = String::new();
        let mut cur_w = 0.0f32;
        for word in para.split_whitespace() {
            let word = clean(word);
            let ww = st.measure(fonts, &word);
            let need = if cur.is_empty() { ww } else { cur_w + space + ww };
            if need <= max_w {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(&word);
                cur_w = need;
                continue;
            }
            if !cur.is_empty() {
                if lines.len() >= max_lines {
                    truncated = true;
                    break 'paras;
                }
                lines.push(std::mem::take(&mut cur));
                cur_w = 0.0;
            }
            if ww <= max_w {
                cur = word;
                cur_w = ww;
                continue;
            }
            // Break a long word between characters.
            for c in word.chars() {
                let cw = fonts.advance(st.font, c, st.size_px);
                if cur_w + cw > max_w && !cur.is_empty() {
                    if lines.len() >= max_lines {
                        truncated = true;
                        break 'paras;
                    }
                    lines.push(std::mem::take(&mut cur));
                    cur_w = 0.0;
                }
                cur.push(c);
                cur_w += cw;
            }
        }
        if lines.len() >= max_lines {
            truncated = truncated || !cur.is_empty();
            break;
        }
        lines.push(cur);
    }
    while lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        truncated = true;
    }
    if truncated && let Some(last) = lines.last_mut() {
        let mut l = std::mem::take(last);
        l.push_str(" …");
        *last = ellipsize(fonts, st, &l, max_w);
    }
    lines
}
