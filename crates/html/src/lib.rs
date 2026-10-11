//! HTML ↔ rich text (layer L2).
//!
//! A tolerant tokenizer + tree builder for the HTML found in mail (WHATWG HTML used as the reference for
//! tokenizing, named character references and implied end tags). Output is a sanitised
//! [`mailcraft_model::Doc`]: no scripts, forms, iframes, objects, event handlers or `javascript:` URLs; CSS
//! limited to inline `style` attributes plus simple `<style>` rules (type, `.class`, `#id`, `tag.class`
//! selectors) mapped onto `TextStyle`/`ParaStyle`. Remote images are kept as URLs; blocking happens at
//! display time. Size caps: input ≤ 16 MiB, nesting ≤ 256.
//!
//! Interface (frozen for commit 2): [`html_to_doc`], [`doc_to_html`], [`html_to_text`], [`text_to_html`],
//! [`remote_resources`]. Additions: [`doc_to_text`], [`parse_css_color`], [`MAX_INPUT_BYTES`], [`MAX_DEPTH`].
//!
//! Mapping notes for consumers (layout, compose):
//! - Paragraph spacing comes from CSS margins; a bare `<p>` gets `space_after_px` of 1em (16 px at 12 pt),
//!   `div`s get none. An empty `Paragraph` (no inlines) is a blank line (`<div><br></div>`, `<p><br></p>`).
//! - Text styles hold only what the markup sets (`None` inherits from `Doc::base`, which comes from `<body>`).
//! - A `div`/`p` with a top border (the desktop-client reply header box) is preceded by a `Block::Rule`.
//! - `display:none` / `mso-hide:all` content (newsletter preheaders) is dropped.

mod convert;
mod css;
mod dom;
mod entities;
mod resources;
mod sanitize;
mod serialize;
mod text;
mod tokenizer;

#[cfg(test)]
mod tests;

use mailcraft_geom::Color;
use mailcraft_model::Doc;
use std::borrow::Cow;

/// Inputs longer than this are truncated (at a character boundary) before parsing.
pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum element nesting kept by the tree builder; deeper start tags are ignored (their text is kept).
pub const MAX_DEPTH: usize = 256;

/// Applies the input cap and normalises CR / CRLF to LF.
fn prepare(html: &str) -> Cow<'_, str> {
    let mut end = html.len().min(MAX_INPUT_BYTES);
    while !html.is_char_boundary(end) {
        end -= 1;
    }
    let s = html.get(..end).unwrap_or("");
    if s.contains('\r') { Cow::Owned(s.replace("\r\n", "\n").replace('\r', "\n")) } else { Cow::Borrowed(s) }
}

/// Parses and sanitises HTML into a document.
pub fn html_to_doc(html: &str) -> Doc {
    let input = prepare(html);
    let dom = dom::parse(&input);
    convert::dom_to_doc(&dom)
}

/// Serialises a document to clean HTML (inline styles only) for sending and drafts.
pub fn doc_to_html(doc: &Doc) -> String {
    serialize::doc_to_html(doc)
}

/// Plain text from HTML (block elements become line breaks, lists get bullets, links keep text; used for list
/// previews, search indexing and plain-text replies).
pub fn html_to_text(html: &str) -> String {
    doc_to_text(&html_to_doc(html))
}

/// Plain text of a document, with the same rules as [`html_to_text`]: one line per paragraph line, a blank
/// line after paragraphs with a margin of 8 px or more, list markers `• ` / `1. `, tab-separated table cells,
/// a rule as a line of dashes, at most one consecutive blank line.
pub fn doc_to_text(doc: &Doc) -> String {
    text::doc_to_text(doc)
}

/// Escapes plain text into HTML paragraphs, turning URLs into links.
pub fn text_to_html(text: &str) -> String {
    let input = prepare(text);
    text::text_to_html(&input)
}

/// Remote (http/https) resource URLs referenced by images, backgrounds and stylesheets (for the
/// "pictures not downloaded" info bar).
pub fn remote_resources(html: &str) -> Vec<String> {
    let input = prepare(html);
    resources::remote_resources(&input)
}

/// Parses a CSS colour value (`#rgb[a]`, `#rrggbb[aa]`, `rgb[a]()`, `hsl[a]()`, named colours, `transparent`).
pub fn parse_css_color(s: &str) -> Option<Color> {
    css::parse_color(s)
}
