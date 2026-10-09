//! Plain text ↔ HTML: [`doc_to_text`] (used by `html_to_text`) and [`text_to_html`] with auto-linking.

use crate::serialize::{escape_attr, escape_text};
use mailcraft_model::rich::{Block, Doc, Inline, ListKind};

/// Text line used for a horizontal rule.
pub(crate) const RULE_TEXT: &str = "------------------------------";
/// Paragraph spacing (px) at or above which a blank line separates paragraphs in plain text.
const BLANK_LINE_SPACING: f32 = 8.0;
const MAX_DEPTH: usize = 512;
/// Nested list levels that add indentation in plain text.
const MAX_INDENT_LEVELS: usize = 16;

fn inline_text(inlines: &[Inline], out: &mut String, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for i in inlines {
        match i {
            Inline::Text { text, .. } => {
                out.extend(text.chars().map(|c| if c == '\u{A0}' { ' ' } else { c }))
            }
            Inline::Link { children, .. } => inline_text(children, out, depth + 1),
            Inline::LineBreak => out.push('\n'),
            Inline::Image { alt, .. } => out.push_str(alt),
        }
    }
}

fn para_lines(inlines: &[Inline], lines: &mut Vec<String>) {
    let mut s = String::new();
    inline_text(inlines, &mut s, 0);
    lines.extend(s.split('\n').map(str::to_owned));
}

/// `lists` is the number of enclosing list items (for indentation).
fn blocks_to_lines(blocks: &[Block], lines: &mut Vec<String>, depth: usize, lists: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for b in blocks {
        match b {
            Block::Paragraph { style, inlines } | Block::Heading { style, inlines, .. } => {
                if style.space_before_px >= BLANK_LINE_SPACING {
                    lines.push(String::new());
                }
                para_lines(inlines, lines);
                if style.space_after_px >= BLANK_LINE_SPACING {
                    lines.push(String::new());
                }
            }
            Block::List { kind, items } => {
                for (n, item) in items.iter().enumerate() {
                    let marker = match kind {
                        ListKind::Bullet => "• ".to_owned(),
                        ListKind::Numbered { start } => {
                            let k = u64::from(*start).saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
                            format!("{k}. ")
                        }
                    };
                    let mut sub = Vec::new();
                    blocks_to_lines(item, &mut sub, depth + 1, lists + 1);
                    let lead = sub.iter().take_while(|l| l.trim().is_empty()).count();
                    // Deeply nested lists stop indenting further: re-padding every line at every level would
                    // copy the text once per level.
                    let pad = if lists < MAX_INDENT_LEVELS {
                        " ".repeat(marker.chars().count())
                    } else {
                        String::new()
                    };
                    let mut first = true;
                    for l in sub.into_iter().skip(lead) {
                        if first {
                            lines.push(format!("{marker}{l}"));
                            first = false;
                        } else if l.trim().is_empty() {
                            lines.push(String::new());
                        } else if pad.is_empty() {
                            lines.push(l);
                        } else {
                            lines.push(format!("{pad}{l}"));
                        }
                    }
                    if first {
                        lines.push(marker.trim_end().to_owned());
                    }
                }
            }
            Block::Quote { blocks } => blocks_to_lines(blocks, lines, depth + 1, lists),
            Block::Pre { text } => lines.extend(text.split('\n').map(str::to_owned)),
            Block::Table { rows, .. } => {
                for row in rows {
                    let cells: Vec<Vec<String>> = row
                        .iter()
                        .map(|c| {
                            let mut l = Vec::new();
                            blocks_to_lines(&c.blocks, &mut l, depth + 1, lists);
                            l.retain(|s| !s.trim().is_empty());
                            l
                        })
                        .collect();
                    if cells.iter().all(|c| c.len() <= 1) {
                        let line = cells
                            .iter()
                            .map(|c| c.first().map_or("", |s| s.trim()))
                            .collect::<Vec<_>>()
                            .join("\t");
                        let line = line.trim_end_matches('\t');
                        if !line.trim().is_empty() {
                            lines.push(line.to_owned());
                        }
                    } else {
                        for c in cells {
                            lines.extend(c);
                        }
                    }
                }
            }
            Block::Rule => lines.push(RULE_TEXT.to_owned()),
            Block::Container { style, blocks } => {
                if style.space_before_px >= BLANK_LINE_SPACING {
                    lines.push(String::new());
                }
                blocks_to_lines(blocks, lines, depth + 1, lists);
                if style.space_after_px >= BLANK_LINE_SPACING {
                    lines.push(String::new());
                }
            }
        }
    }
}

/// Plain text of a document: one line per paragraph line, list markers, tab-separated table cells, at most
/// one blank line in a row, no leading or trailing blank lines.
pub(crate) fn doc_to_text(doc: &Doc) -> String {
    let mut lines = Vec::new();
    blocks_to_lines(&doc.blocks, &mut lines, 0, 0);
    let mut out = String::new();
    let mut blank_pending = false;
    for l in lines {
        let l = l.trim_end();
        if l.trim().is_empty() {
            blank_pending = !out.is_empty();
            continue;
        }
        if blank_pending {
            out.push('\n');
            blank_pending = false;
        }
        out.push_str(l);
        out.push('\n');
    }
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Segment<'a> {
    Text(&'a str),
    Link { href: String, text: &'a str },
}

fn url_char(b: u8) -> bool {
    !b.is_ascii_whitespace() && !matches!(b, b'<' | b'>' | b'"' | b'`') && !b.is_ascii_control()
}

fn starts_with_ci(b: &[u8], i: usize, pat: &str) -> bool {
    b.get(i..i.saturating_add(pat.len())).is_some_and(|s| s.eq_ignore_ascii_case(pat.as_bytes()))
}

/// Trims trailing punctuation that is almost never part of a URL, and unbalanced closing brackets.
/// Linear: bracket balances are counted once and updated as characters are dropped.
fn trim_url_end(s: &str) -> usize {
    let count = |c: char| s.chars().filter(|&x| x == c).count();
    // (open, close) counts of the prefix still kept.
    let (mut paren, mut square) = ((count('('), count(')')), (count('['), count(']')));
    let mut end = s.len();
    loop {
        let Some(last) = s.get(..end).and_then(|t| t.chars().last()) else { return end };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' => true,
            ')' if paren.0 < paren.1 => {
                paren.1 -= 1;
                true
            }
            ']' if square.0 < square.1 => {
                square.1 -= 1;
                true
            }
            _ => false,
        };
        if !drop {
            return end;
        }
        end -= last.len_utf8();
    }
}

/// A URL starting at byte `i`: returns its end and href.
fn url_at(line: &str, i: usize) -> Option<(usize, String)> {
    let b = line.as_bytes();
    if i > 0
        && b.get(i - 1).is_some_and(|p| p.is_ascii_alphanumeric() || matches!(p, b'@' | b'.' | b'/' | b'-'))
    {
        return None;
    }
    let (prefix_len, www) = if starts_with_ci(b, i, "https://") {
        (8, false)
    } else if starts_with_ci(b, i, "http://") {
        (7, false)
    } else if starts_with_ci(b, i, "www.") {
        (4, true)
    } else {
        return None;
    };
    // Something linkable must follow the prefix ("http://" alone is not a link). Checking before the scan
    // keeps a line full of failed candidates such as "http://(http://(…" linear.
    if !line.get(i + prefix_len..).is_some_and(|r| r.starts_with(|c: char| c.is_alphanumeric() || c == '[')) {
        return None;
    }
    let mut end = i + prefix_len;
    while b.get(end).is_some_and(|&c| url_char(c)) {
        end += 1;
    }
    let text = line.get(i..end)?;
    let end = i + trim_url_end(text);
    let text = line.get(i..end)?;
    // Require something after the prefix ("http://" alone is not a link; "www." needs a dotted host).
    let rest = text.get(prefix_len..)?;
    if rest.is_empty() || !rest.starts_with(|c: char| c.is_alphanumeric() || c == '[') {
        return None;
    }
    if www && !rest.contains('.') {
        return None;
    }
    Some((end, if www { format!("http://{text}") } else { text.to_owned() }))
}

fn email_local(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-')
}

/// An e-mail address around the `@` at byte `at`, not starting before `min`.
fn email_at(line: &str, at: usize, min: usize) -> Option<(usize, usize)> {
    let b = line.as_bytes();
    let mut start = at;
    while start > min && b.get(start - 1).is_some_and(|&c| email_local(c)) {
        start -= 1;
    }
    while b.get(start) == Some(&b'.') {
        start += 1;
    }
    if start >= at {
        return None;
    }
    let mut end = at + 1;
    while b.get(end).is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-') {
        end += 1;
    }
    while end > at + 1 && b.get(end - 1).is_some_and(|&c| c == b'.' || c == b'-') {
        end -= 1;
    }
    let domain = line.get(at + 1..end)?;
    let tld = domain.rsplit('.').next()?;
    if !domain.contains('.')
        || domain.starts_with('.')
        || tld.len() < 2
        || !tld.chars().all(|c| c.is_ascii_alphabetic())
    {
        return None;
    }
    Some((start, end))
}

/// Splits a line into text and auto-detected links (http/https URLs, `www.` hosts, e-mail addresses).
pub(crate) fn autolink(line: &str) -> Vec<Segment<'_>> {
    let b = line.as_bytes();
    let mut segs = Vec::new();
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        if let Some((end, href)) = url_at(line, i) {
            if i > last {
                segs.push(Segment::Text(line.get(last..i).unwrap_or("")));
            }
            segs.push(Segment::Link { href, text: line.get(i..end).unwrap_or("") });
            last = end;
            i = end;
            continue;
        }
        if b.get(i) == Some(&b'@')
            && let Some((s, e)) = email_at(line, i, last)
        {
            // Accept an explicit "mailto:" prefix as part of the link text.
            let s = if s >= last + 7 && starts_with_ci(b, s - 7, "mailto:") { s - 7 } else { s };
            if s > last {
                segs.push(Segment::Text(line.get(last..s).unwrap_or("")));
            }
            let text = line.get(s..e).unwrap_or("");
            let href = if starts_with_ci(text.as_bytes(), 0, "mailto:") {
                text.to_owned()
            } else {
                format!("mailto:{text}")
            };
            segs.push(Segment::Link { href, text });
            last = e;
            i = e;
            continue;
        }
        i += 1;
    }
    if last < b.len() {
        segs.push(Segment::Text(line.get(last..).unwrap_or("")));
    }
    segs
}

fn line_html(line: &str, out: &mut String) {
    let mut prev_space = true;
    for seg in autolink(line) {
        match seg {
            Segment::Text(t) => escape_text(t, &mut prev_space, out),
            Segment::Link { href, text } => {
                out.push_str("<a href=\"");
                escape_attr(&href, out);
                out.push_str("\">");
                escape_text(text, &mut prev_space, out);
                out.push_str("</a>");
            }
        }
    }
}

/// Plain text → HTML: paragraphs at blank lines, `<br>` for single line breaks, runs of spaces kept,
/// URLs and e-mail addresses linked.
pub(crate) fn text_to_html(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(text.len() + text.len() / 4 + 16);
    let mut para: Vec<&str> = Vec::new();
    let flush = |para: &mut Vec<&str>, out: &mut String| {
        if para.is_empty() {
            return;
        }
        out.push_str("<p>");
        for (i, l) in para.iter().enumerate() {
            if i > 0 {
                out.push_str("<br>\n");
            }
            line_html(l.trim_end(), out);
        }
        out.push_str("</p>\n");
        para.clear();
    };
    for line in text.split('\n') {
        if line.trim().is_empty() {
            flush(&mut para, &mut out);
        } else {
            para.push(line);
        }
    }
    flush(&mut para, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(s: &str) -> Vec<String> {
        autolink(s)
            .into_iter()
            .filter_map(|s| match s {
                Segment::Link { href, .. } => Some(href),
                Segment::Text(_) => None,
            })
            .collect()
    }

    #[test]
    fn autolink_urls() {
        assert_eq!(links("see https://example.com/a?b=1."), vec!["https://example.com/a?b=1"]);
        assert_eq!(links("(http://x.org/wiki/A_(b))"), vec!["http://x.org/wiki/A_(b)"]);
        assert_eq!(links("(http://x.org/a)"), vec!["http://x.org/a"]);
        assert_eq!(links("visit www.example.com, now"), vec!["http://www.example.com"]);
        assert_eq!(links("HTTP://CAPS.example"), vec!["HTTP://CAPS.example"]);
        assert!(links("http:// nothing").is_empty());
        assert!(links("wwwx.com awww.b.c").is_empty());
        assert!(links("www.").is_empty());
        assert_eq!(links("<https://a.b/c>"), vec!["https://a.b/c"]);
    }

    #[test]
    fn autolink_emails() {
        assert_eq!(links("mail john.doe+x@example.co.uk."), vec!["mailto:john.doe+x@example.co.uk"]);
        assert_eq!(links("mailto:a@b.cd"), vec!["mailto:a@b.cd"]);
        assert!(links("a@b, @x.com, a@b.c1, user@localhost").is_empty());
        assert_eq!(links("x: .a@b.com"), vec!["mailto:a@b.com"]);
    }

    #[test]
    fn autolink_segments_cover_input() {
        let s = "a http://x.y b c@d.ef é www.q.rs";
        let joined: String = autolink(s)
            .into_iter()
            .map(|seg| match seg {
                Segment::Text(t) => t,
                Segment::Link { text, .. } => text,
            })
            .collect();
        assert_eq!(joined, s);
    }

    #[test]
    fn text_to_html_paragraphs() {
        let h = text_to_html("Hi <you> & co,\r\nline two\n\n\n  indented  twice\nhttps://a.b/?x=1&y=2");
        assert_eq!(
            h,
            "<p>Hi &lt;you&gt; &amp; co,<br>\nline two</p>\n<p>&nbsp; indented &nbsp;twice<br>\n<a href=\"https://a.b/?x=1&amp;y=2\">https://a.b/?x=1&amp;y=2</a></p>\n"
        );
        assert_eq!(text_to_html(""), "");
        assert_eq!(text_to_html("\n\n"), "");
    }
}
