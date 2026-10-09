//! A tolerant HTML tokenizer modelled on the WHATWG states: data, tag open, tag name, attribute name and
//! value (double-quoted, single-quoted, unquoted), comments, DOCTYPE, bogus comments, CDATA sections and the
//! raw-text / escapable-raw-text content of `script`, `style`, `textarea`, `title` and friends.
//!
//! Works on bytes and only ever splits the input at ASCII delimiters, so every slice is on a char boundary.
//! A tag cut off by the end of input is dropped, as the specification requires.

use crate::entities;

/// Maximum attributes kept per tag (the rest are parsed and discarded).
const MAX_ATTRS: usize = 128;
/// Maximum bytes kept of a tag or attribute name.
const MAX_NAME_LEN: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Token {
    Start { name: String, attrs: Vec<(String, String)>, self_closing: bool },
    End { name: String },
    Text(String),
    Comment,
    Doctype,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RawKind {
    /// Content is literal (script, style, xmp, iframe, noembed, noframes).
    Raw,
    /// Content has character references decoded (textarea, title).
    Escapable,
    /// Everything to end of input is text.
    Plaintext,
}

fn raw_kind(name: &str) -> Option<RawKind> {
    match name {
        "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes" => Some(RawKind::Raw),
        "textarea" | "title" => Some(RawKind::Escapable),
        "plaintext" => Some(RawKind::Plaintext),
        _ => None,
    }
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0C)
}

pub(crate) struct Tokenizer<'a> {
    src: &'a str,
    pos: usize,
    raw: Option<(String, RawKind)>,
}

impl<'a> Tokenizer<'a> {
    pub(crate) fn new(src: &'a str) -> Self {
        Self { src, pos: 0, raw: None }
    }

    fn bytes(&self) -> &'a [u8] {
        self.src.as_bytes()
    }

    fn at(&self, i: usize) -> Option<u8> {
        self.bytes().get(i).copied()
    }

    fn slice(&self, a: usize, b: usize) -> &'a str {
        self.src.get(a..b).unwrap_or("")
    }

    fn starts_with_ci(&self, i: usize, pat: &str) -> bool {
        self.bytes()
            .get(i..i.saturating_add(pat.len()))
            .is_some_and(|s| s.eq_ignore_ascii_case(pat.as_bytes()))
    }

    fn find_from(&self, from: usize, pat: &str) -> Option<usize> {
        self.src.get(from..).and_then(|s| s.find(pat)).map(|p| p + from)
    }

    /// Finds `</name` followed by whitespace, `/` or `>` (ASCII case-insensitive), starting at `from`.
    fn find_raw_end(&self, from: usize, name: &str) -> Option<usize> {
        let mut i = from;
        while let Some(p) = self.find_from(i, "</") {
            if self.starts_with_ci(p + 2, name) {
                let after = self.at(p + 2 + name.len());
                if after.is_none_or(|b| is_ws(b) || b == b'/' || b == b'>') {
                    return Some(p);
                }
            }
            i = p + 2;
        }
        None
    }

    fn raw_text(&mut self, name: &str, kind: RawKind) -> Option<Token> {
        let start = self.pos;
        let end = match kind {
            RawKind::Plaintext => self.src.len(),
            _ => self.find_raw_end(start, name).unwrap_or(self.src.len()),
        };
        self.pos = end;
        let text = self.slice(start, end);
        if text.is_empty() {
            return None;
        }
        Some(Token::Text(match kind {
            RawKind::Escapable => entities::decode(text, false),
            _ => text.to_owned(),
        }))
    }

    /// Reads a name made of anything but whitespace, `/`, `>` (and `=` for attributes).
    fn read_name(&mut self, stop_at_eq: bool) -> String {
        let start = self.pos;
        while let Some(b) = self.at(self.pos) {
            if is_ws(b) || b == b'/' || b == b'>' || (stop_at_eq && b == b'=' && self.pos > start) {
                break;
            }
            self.pos += 1;
        }
        let raw = self.slice(start, self.pos);
        let mut name = String::with_capacity(raw.len().min(MAX_NAME_LEN));
        for c in raw.chars() {
            if name.len() >= MAX_NAME_LEN {
                break;
            }
            name.push(if c == '\0' { '\u{FFFD}' } else { c.to_ascii_lowercase() });
        }
        name
    }

    fn skip_ws(&mut self) {
        while self.at(self.pos).is_some_and(is_ws) {
            self.pos += 1;
        }
    }

    /// Attribute list up to and including `>`. Returns `None` at end of input (tag is dropped).
    fn read_attrs(&mut self) -> Option<(Vec<(String, String)>, bool)> {
        let mut attrs: Vec<(String, String)> = Vec::new();
        loop {
            self.skip_ws();
            match self.at(self.pos)? {
                b'>' => {
                    self.pos += 1;
                    return Some((attrs, false));
                }
                b'/' => {
                    self.pos += 1;
                    if self.at(self.pos) == Some(b'>') {
                        self.pos += 1;
                        return Some((attrs, true));
                    }
                }
                _ => {
                    let name = self.read_name(true);
                    self.skip_ws();
                    let mut value = String::new();
                    if self.at(self.pos) == Some(b'=') {
                        self.pos += 1;
                        self.skip_ws();
                        match self.at(self.pos)? {
                            q @ (b'"' | b'\'') => {
                                let start = self.pos + 1;
                                let rel = self.bytes().get(start..)?.iter().position(|&b| b == q)?;
                                value = entities::decode(self.slice(start, start + rel), true);
                                self.pos = start + rel + 1;
                            }
                            b'>' => {}
                            _ => {
                                let start = self.pos;
                                while self.at(self.pos).is_some_and(|b| !is_ws(b) && b != b'>') {
                                    self.pos += 1;
                                }
                                value = entities::decode(self.slice(start, self.pos), true);
                            }
                        }
                    }
                    if !name.is_empty() && attrs.len() < MAX_ATTRS && !attrs.iter().any(|(n, _)| *n == name) {
                        attrs.push((name, value));
                    }
                }
            }
        }
    }

    /// At `<`: returns the markup token, or `None` if this `<` is literal text.
    /// `Some(None)` means the input ended inside the construct (nothing to emit).
    fn markup(&mut self) -> Option<Option<Token>> {
        let lt = self.pos;
        match self.at(lt + 1) {
            Some(b) if b.is_ascii_alphabetic() => {
                self.pos = lt + 1;
                let name = self.read_name(false);
                let Some((attrs, self_closing)) = self.read_attrs() else {
                    self.pos = self.src.len();
                    return Some(None);
                };
                if let Some(kind) = raw_kind(&name) {
                    self.raw = Some((name.clone(), kind));
                }
                Some(Some(Token::Start { name, attrs, self_closing }))
            }
            Some(b'/') => match self.at(lt + 2) {
                Some(b) if b.is_ascii_alphabetic() => {
                    self.pos = lt + 2;
                    let name = self.read_name(false);
                    if self.read_attrs().is_none() {
                        self.pos = self.src.len();
                        return Some(None);
                    }
                    Some(Some(Token::End { name }))
                }
                Some(b'>') => {
                    self.pos = lt + 3;
                    Some(Some(Token::Comment))
                }
                None => None,
                _ => Some(Some(self.bogus_comment(lt + 2))),
            },
            Some(b'!') => {
                if self.starts_with_ci(lt + 2, "--") {
                    Some(Some(self.comment(lt + 4)))
                } else if self.starts_with_ci(lt + 2, "[CDATA[") {
                    let start = lt + 9;
                    let end = self.find_from(start, "]]>");
                    let text = self.slice(start, end.unwrap_or(self.src.len())).to_owned();
                    self.pos = end.map_or(self.src.len(), |e| e + 3);
                    Some(Some(Token::Text(text)))
                } else if self.starts_with_ci(lt + 2, "doctype") {
                    let _ = self.bogus_comment(lt + 2);
                    Some(Some(Token::Doctype))
                } else {
                    Some(Some(self.bogus_comment(lt + 2)))
                }
            }
            Some(b'?') => Some(Some(self.bogus_comment(lt + 1))),
            _ => None,
        }
    }

    fn bogus_comment(&mut self, from: usize) -> Token {
        self.pos = self.find_from(from, ">").map_or(self.src.len(), |p| p + 1);
        Token::Comment
    }

    /// `from` is just after `<!--`.
    fn comment(&mut self, from: usize) -> Token {
        if self.at(from) == Some(b'>') {
            self.pos = from + 1;
        } else if self.starts_with_ci(from, "->") {
            self.pos = from + 2;
        } else {
            let mut i = from;
            self.pos = self.src.len();
            while let Some(p) = self.find_from(i, "--") {
                if self.at(p + 2) == Some(b'>') {
                    self.pos = p + 3;
                    break;
                }
                if self.starts_with_ci(p + 2, "!>") {
                    self.pos = p + 4;
                    break;
                }
                i = p + 1;
            }
        }
        Token::Comment
    }

    fn text(&mut self) -> Token {
        let start = self.pos;
        // A literal '<' at the current position belongs to this text run.
        let from = if self.at(start) == Some(b'<') { start + 1 } else { start };
        let end = self.find_from(from, "<").unwrap_or(self.src.len());
        self.pos = end;
        Token::Text(entities::decode(self.slice(start, end), false))
    }
}

impl Iterator for Tokenizer<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        loop {
            if self.pos >= self.src.len() {
                return None;
            }
            if let Some((name, kind)) = self.raw.take() {
                if let Some(t) = self.raw_text(&name, kind) {
                    return Some(t);
                }
                continue;
            }
            if self.at(self.pos) == Some(b'<') {
                match self.markup() {
                    Some(Some(tok)) => return Some(tok),
                    Some(None) => continue,
                    None => {}
                }
            }
            let t = self.text();
            if matches!(&t, Token::Text(s) if s.is_empty()) {
                continue;
            }
            return Some(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Token> {
        Tokenizer::new(s).collect()
    }

    fn start(name: &str, attrs: &[(&str, &str)]) -> Token {
        Token::Start {
            name: name.into(),
            attrs: attrs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
            self_closing: false,
        }
    }

    #[test]
    fn basic_tags_and_text() {
        assert_eq!(
            toks("<P Class=x>Hi &amp; bye</p>"),
            vec![
                start("p", &[("class", "x")]),
                Token::Text("Hi & bye".into()),
                Token::End { name: "p".into() }
            ]
        );
    }

    #[test]
    fn attribute_forms() {
        let t = toks(r#"<a href="x?a=1&amp;b=2" title='it&apos;s' data-x=plain checked id=a id=b>"#);
        assert_eq!(
            t,
            vec![start(
                "a",
                &[
                    ("href", "x?a=1&b=2"),
                    ("title", "it's"),
                    ("data-x", "plain"),
                    ("checked", ""),
                    ("id", "a")
                ]
            )]
        );
        let t = toks("<img src=a.png/>");
        assert_eq!(t, vec![start("img", &[("src", "a.png/")])]);
        let t = toks("<br/>");
        assert_eq!(t, vec![Token::Start { name: "br".into(), attrs: vec![], self_closing: true }]);
    }

    #[test]
    fn comments_doctype_cdata() {
        assert_eq!(
            toks("<!DOCTYPE html><!-- c -- d --><!--[if mso]><b>x</b><![endif]-->y<![CDATA[a<b]]><?xml x?>"),
            vec![
                Token::Doctype,
                Token::Comment,
                Token::Comment,
                Token::Text("y".into()),
                Token::Text("a<b".into()),
                Token::Comment
            ]
        );
        assert_eq!(
            toks("<!-->a<!--->b"),
            vec![Token::Comment, Token::Text("a".into()), Token::Comment, Token::Text("b".into())]
        );
        assert_eq!(toks("<!-- never closed <b>"), vec![Token::Comment]);
        assert_eq!(
            toks("a<!--x--!>b"),
            vec![Token::Text("a".into()), Token::Comment, Token::Text("b".into())]
        );
    }

    #[test]
    fn raw_text_elements() {
        let t = toks("<script>if (a<b) { x='</div>' }</script >after");
        assert_eq!(
            t,
            vec![
                start("script", &[]),
                Token::Text("if (a<b) { x='</div>' }".into()),
                Token::End { name: "script".into() },
                Token::Text("after".into())
            ]
        );
        let t = toks("<title>A &amp; B</TITLE>");
        assert_eq!(
            t,
            vec![start("title", &[]), Token::Text("A & B".into()), Token::End { name: "title".into() }]
        );
        let t = toks("<style>p{}</styles></style>");
        assert_eq!(t.get(1), Some(&Token::Text("p{}</styles>".into())));
        let t = toks("<plaintext>a</plaintext>b");
        assert_eq!(t.get(1), Some(&Token::Text("a</plaintext>b".into())));
    }

    #[test]
    fn literal_lt_and_unterminated() {
        assert_eq!(
            toks("a < b <3 <"),
            vec![
                Token::Text("a ".into()),
                Token::Text("< b ".into()),
                Token::Text("<3 ".into()),
                Token::Text("<".into())
            ]
        );
        assert_eq!(toks("x<div class=\"unterminated"), vec![Token::Text("x".into())]);
        assert_eq!(toks("<div"), vec![]);
        assert_eq!(
            toks("</>a</ x>b"),
            vec![Token::Comment, Token::Text("a".into()), Token::Comment, Token::Text("b".into())]
        );
    }

    #[test]
    fn names_are_lowercased_and_capped() {
        let long = "a".repeat(500);
        let t = toks(&format!("<{long} {long}=1>"));
        match t.first() {
            Some(Token::Start { name, attrs, .. }) => {
                assert_eq!(name.len(), MAX_NAME_LEN);
                assert_eq!(attrs.first().map(|a| a.0.len()), Some(MAX_NAME_LEN));
            }
            other => panic!("{other:?}"),
        }
        let many: String = (0..1000).map(|i| format!(" a{i}=1")).collect();
        match toks(&format!("<p{many}>")).first() {
            Some(Token::Start { attrs, .. }) => assert_eq!(attrs.len(), MAX_ATTRS),
            other => panic!("{other:?}"),
        }
    }
}
