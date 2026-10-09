//! [`Doc`] → HTML with inline styles only (no classes, no `<style>`), as sent in mail and stored in drafts.
//!
//! The output is designed to read back through [`crate::html_to_doc`] into the same document: every
//! paragraph carries explicit margins, alignment is written whenever it differs from what the element would
//! inherit, a paragraph ending in a line break gets one extra `<br>` (browsers swallow a trailing one), and
//! spaces that would collapse are written as `&nbsp;`.

use crate::{css, sanitize};
use mailcraft_model::rich::{Align, Block, Doc, Inline, ListKind, ParaStyle, TableCell, TextStyle};
use std::fmt::Write as _;

/// Blocks nested deeper than this are not written (documents from the importer never get close).
const MAX_DEPTH: usize = 512;

pub(crate) fn num(v: f32) -> String {
    if !v.is_finite() || v == 0.0 {
        return "0".to_owned();
    }
    format!("{v}")
}

/// A CSS pixel length (`0` without a unit).
fn px(v: f32) -> String {
    let n = num(v);
    if n == "0" { n } else { format!("{n}px") }
}

pub(crate) fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\u{A0}' => out.push_str("&nbsp;"),
            _ => out.push(c),
        }
    }
}

/// Escapes text content; `prev_space` tracks whether the previous output character was a collapsible
/// space (or the start of a line), so runs of spaces survive as `&nbsp;`.
pub(crate) fn escape_text(s: &str, prev_space: &mut bool, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\u{A0}' => out.push_str("&nbsp;"),
            '\n' => {
                out.push_str("<br>");
                *prev_space = true;
                continue;
            }
            ' ' | '\t' if *prev_space => out.push_str("&nbsp;"),
            ' ' => {
                out.push(' ');
                *prev_space = true;
                continue;
            }
            '\t' => {
                out.push('\t');
                *prev_space = true;
                continue;
            }
            '\r' | '\0' => continue,
            _ => out.push(c),
        }
        *prev_space = false;
    }
}

fn align_css(a: Align) -> &'static str {
    match a {
        Align::Start => "left",
        Align::Center => "center",
        Align::End => "right",
        Align::Justify => "justify",
    }
}

fn para_css(s: &ParaStyle, inherited: Align) -> String {
    let mut css = String::new();
    let (t, b, l) = (s.space_before_px, s.space_after_px, s.indent_px);
    if t == 0.0 && b == 0.0 && l == 0.0 {
        css.push_str("margin:0");
    } else {
        let _ = write!(css, "margin:{} 0 {} {}", px(t), px(b), px(l));
    }
    if s.align != inherited {
        let _ = write!(css, ";text-align:{}", align_css(s.align));
    }
    if let Some(lh) = s.line_height {
        let _ = write!(css, ";line-height:{}", num(lh));
    }
    if let Some(bg) = s.background {
        let _ = write!(css, ";background-color:{}", bg.to_hex());
    }
    css
}

/// Character styles that go into a `style` attribute (everything without a dedicated tag).
pub(crate) fn text_css(s: &TextStyle) -> String {
    let mut parts: Vec<String> = Vec::new();
    // Cleaned again here: a hand-built document could hold `;` or braces that would inject declarations.
    if let Some(f) = s.font_family.as_deref().and_then(css::clean_font_family) {
        parts.push(format!("font-family:{f}"));
    }
    if let Some(pt) = s.size_pt {
        parts.push(format!("font-size:{}pt", num(pt)));
    }
    if let Some(c) = s.color {
        parts.push(format!("color:{}", c.to_hex()));
    }
    if let Some(c) = s.highlight {
        parts.push(format!("background-color:{}", c.to_hex()));
    }
    if s.bold == Some(false) {
        parts.push("font-weight:normal".into());
    }
    if s.italic == Some(false) {
        parts.push("font-style:normal".into());
    }
    if s.underline == Some(false) || s.strike == Some(false) {
        parts.push("text-decoration:none".into());
    }
    if s.superscript == Some(false) || s.subscript == Some(false) {
        parts.push("vertical-align:baseline".into());
    }
    parts.join(";")
}

struct Writer {
    out: String,
    /// Collapsible-space state inside the current paragraph.
    prev_space: bool,
}

impl Writer {
    fn attr_value(&mut self, v: &str) {
        self.out.push('"');
        escape_attr(v, &mut self.out);
        self.out.push('"');
    }

    fn blocks(&mut self, blocks: &[Block], inherited: Align, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        for b in blocks {
            self.block(b, inherited, depth);
        }
    }

    fn para_open(&mut self, tag: &str, style: &ParaStyle, inherited: Align) {
        let _ = write!(self.out, "<{tag} style=");
        self.attr_value(&para_css(style, inherited));
        self.out.push('>');
        self.prev_space = true;
    }

    fn para_body(&mut self, inlines: &[Inline]) {
        self.inlines(inlines);
        if inlines.is_empty() || matches!(inlines.last(), Some(Inline::LineBreak)) {
            self.out.push_str("<br>");
        }
    }

    fn block(&mut self, b: &Block, inherited: Align, depth: usize) {
        match b {
            Block::Paragraph { style, inlines } => {
                self.para_open("p", style, inherited);
                self.para_body(inlines);
                self.out.push_str("</p>\n");
            }
            Block::Heading { level, style, inlines } => {
                let tag = format!("h{}", (*level).clamp(1, 6));
                self.para_open(&tag, style, inherited);
                self.para_body(inlines);
                let _ = writeln!(self.out, "</{tag}>");
            }
            Block::List { kind, items } => {
                let tag = match kind {
                    ListKind::Bullet => {
                        self.out.push_str("<ul>\n");
                        "ul"
                    }
                    ListKind::Numbered { start } => {
                        if *start == 1 {
                            self.out.push_str("<ol>\n");
                        } else {
                            let _ = writeln!(self.out, "<ol start=\"{start}\">");
                        }
                        "ol"
                    }
                };
                for item in items {
                    self.out.push_str("<li>");
                    self.blocks(item, inherited, depth + 1);
                    self.out.push_str("</li>\n");
                }
                let _ = writeln!(self.out, "</{tag}>");
            }
            Block::Quote { blocks } => {
                self.out.push_str(
                    "<blockquote style=\"margin:0 0 0 12px;padding-left:8px;border-left:2px solid #c8c8c8\">\n",
                );
                self.blocks(blocks, inherited, depth + 1);
                self.out.push_str("</blockquote>\n");
            }
            Block::Pre { text } => {
                self.out.push_str("<pre>");
                if text.starts_with('\n') {
                    self.out.push('\n');
                }
                for c in text.chars() {
                    match c {
                        '&' => self.out.push_str("&amp;"),
                        '<' => self.out.push_str("&lt;"),
                        '>' => self.out.push_str("&gt;"),
                        '\r' | '\0' => {}
                        _ => self.out.push(c),
                    }
                }
                self.out.push_str("</pre>\n");
            }
            Block::Table { rows, border, width_px } => {
                self.out.push_str("<table");
                if *border {
                    self.out.push_str(" border=\"1\"");
                }
                self.out.push_str(" cellspacing=\"0\" cellpadding=\"4\" style=\"border-collapse:collapse");
                if let Some(w) = width_px {
                    let _ = write!(self.out, ";width:{}px", num(*w));
                }
                self.out.push_str("\">\n");
                for row in rows {
                    self.out.push_str("<tr>");
                    for cell in row {
                        self.cell(cell, inherited, depth);
                    }
                    self.out.push_str("</tr>\n");
                }
                self.out.push_str("</table>\n");
            }
            Block::Rule => self.out.push_str("<hr>\n"),
            Block::Container { style, blocks } => {
                self.para_open("div", style, inherited);
                self.out.push('\n');
                self.blocks(blocks, style.align, depth + 1);
                self.out.push_str("</div>\n");
            }
        }
    }

    fn cell(&mut self, cell: &TableCell, inherited: Align, depth: usize) {
        self.out.push_str("<td");
        if cell.colspan > 1 {
            let _ = write!(self.out, " colspan=\"{}\"", cell.colspan);
        }
        if cell.rowspan > 1 {
            let _ = write!(self.out, " rowspan=\"{}\"", cell.rowspan);
        }
        let mut css = String::from("vertical-align:top");
        if let Some(bg) = cell.background {
            let _ = write!(css, ";background-color:{}", bg.to_hex());
        }
        if let Some(w) = cell.width_px {
            let _ = write!(css, ";width:{}px", num(w));
        }
        self.out.push_str(" style=");
        self.attr_value(&css);
        self.out.push('>');
        self.blocks(&cell.blocks, inherited, depth + 1);
        self.out.push_str("</td>");
    }

    fn inlines(&mut self, inlines: &[Inline]) {
        for i in inlines {
            self.inline(i, true, 0);
        }
    }

    fn inline(&mut self, i: &Inline, allow_link: bool, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match i {
            Inline::Text { text, style } => self.text(text, style),
            Inline::Link { href, children } => {
                let safe = if allow_link { sanitize::safe_href(href) } else { None };
                if let Some(href) = &safe {
                    self.out.push_str("<a href=");
                    self.attr_value(href);
                    self.out.push('>');
                }
                for c in children {
                    self.inline(c, false, depth + 1);
                }
                if safe.is_some() {
                    self.out.push_str("</a>");
                }
            }
            Inline::LineBreak => {
                self.out.push_str("<br>");
                self.prev_space = true;
            }
            Inline::Image { src, alt, width, height } => {
                let Some(src) = sanitize::safe_image_src(src) else { return };
                self.out.push_str("<img src=");
                self.attr_value(&src);
                self.out.push_str(" alt=");
                self.attr_value(alt);
                for (name, v) in [("width", width), ("height", height)] {
                    if let Some(v) = v.filter(|v| v.is_finite() && *v >= 0.0) {
                        let _ = write!(self.out, " {name}=\"{}\"", num(v));
                    }
                }
                self.out.push('>');
                self.prev_space = false;
            }
        }
    }

    fn text(&mut self, text: &str, style: &TextStyle) {
        let css = text_css(style);
        let mut tags: Vec<&str> = Vec::new();
        for (on, tag) in [
            (style.bold, "b"),
            (style.italic, "i"),
            (style.underline, "u"),
            (style.strike, "s"),
            (style.superscript, "sup"),
            (style.subscript, "sub"),
            (style.monospace, "code"),
        ] {
            if on == Some(true) {
                tags.push(tag);
            }
        }
        // The span goes outermost so tags inside it can re-enable what it switches off.
        if !css.is_empty() {
            self.out.push_str("<span style=");
            self.attr_value(&css);
            self.out.push('>');
        }
        for t in &tags {
            let _ = write!(self.out, "<{t}>");
        }
        escape_text(text, &mut self.prev_space, &mut self.out);
        for t in tags.iter().rev() {
            let _ = write!(self.out, "</{t}>");
        }
        if !css.is_empty() {
            self.out.push_str("</span>");
        }
    }
}

pub(crate) fn doc_to_html(doc: &Doc) -> String {
    let mut w = Writer { out: String::with_capacity(1024), prev_space: true };
    w.out.push_str("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n</head>\n<body");
    // A body background is the page background, so the base style's highlight cannot be expressed there.
    let mut body = text_css(&TextStyle { highlight: None, ..doc.base.clone() });
    for (on, css) in [
        (doc.base.bold, "font-weight:bold"),
        (doc.base.italic, "font-style:italic"),
        (doc.base.underline, "text-decoration:underline"),
    ] {
        if on == Some(true) {
            if !body.is_empty() {
                body.push(';');
            }
            body.push_str(css);
        }
    }
    if let Some(bg) = doc.background {
        if !body.is_empty() {
            body.push(';');
        }
        let _ = write!(body, "background-color:{}", bg.to_hex());
    }
    if !body.is_empty() {
        w.out.push_str(" style=");
        w.attr_value(&body);
    }
    w.out.push_str(">\n");
    w.blocks(&doc.blocks, Align::Start, 0);
    w.out.push_str("</body>\n</html>\n");
    w.out
}
