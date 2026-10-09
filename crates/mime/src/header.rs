//! Header fields (RFC 5322 §2.2): unfolding, lookup, decoding.

use serde::{Deserialize, Serialize};

/// Most header fields kept from one header block; the rest are dropped.
pub const MAX_HEADERS: usize = 10_000;
/// Longest unfolded header value kept (bytes); longer values are truncated.
pub const MAX_HEADER_VALUE: usize = 1024 * 1024;

/// One header field. `value` is the raw unfolded value (CRLF+WSP folded to WSP), not RFC 2047-decoded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

/// Ordered header list with case-insensitive lookup.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Headers(pub Vec<Header>);

impl Headers {
    /// First value of `name` (raw, unfolded).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().find(|h| h.name.eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
    }
    /// All values of `name` in order.
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.0.iter().filter(move |h| h.name.eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
    }
    /// First value with RFC 2047 words decoded and surrounding whitespace trimmed.
    pub fn text(&self, name: &str) -> Option<String> {
        self.get(name).map(|v| crate::encoded::decode_words(v.trim()))
    }
    pub fn push(&mut self, name: &str, value: &str) {
        self.0.push(Header { name: name.to_owned(), value: value.to_owned() });
    }
    /// True when at least one field named `name` exists.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
    /// Replaces the first field named `name` (removing any others) or appends it.
    pub fn set(&mut self, name: &str, value: &str) {
        match self.0.iter().position(|h| h.name.eq_ignore_ascii_case(name)) {
            Some(i) => {
                if let Some(h) = self.0.get_mut(i) {
                    h.value = value.to_owned();
                }
                let mut k = 0usize;
                self.0.retain(|h| {
                    let keep = k <= i || !h.name.eq_ignore_ascii_case(name);
                    k += 1;
                    keep
                });
            }
            None => self.push(name, value),
        }
    }
    /// Removes every field named `name`; returns how many were removed.
    pub fn remove(&mut self, name: &str) -> usize {
        let before = self.0.len();
        self.0.retain(|h| !h.name.eq_ignore_ascii_case(name));
        before - self.0.len()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = &Header> {
        self.0.iter()
    }
    /// Writes the block as `Name: value` lines folded at 78 columns, CRLF-terminated, without the blank line.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        for h in &self.0 {
            out.push_str(&fold(&h.name, &h.value));
            out.push_str("\r\n");
        }
        out.into_bytes()
    }

    /// Parses a header block (up to the first empty line). Returns the headers and the byte offset where the body
    /// starts. Accepts CRLF and bare LF.
    ///
    /// Leniency: an mbox `From ` line before the first field is skipped; whitespace before the colon (obsolete
    /// syntax) is accepted; continuation lines before any field are ignored; a line that is not a field at all
    /// ends the header block and starts the body. Non-UTF-8 bytes are decoded as windows-1252.
    pub fn parse(input: &[u8]) -> (Headers, usize) {
        let mut headers = Headers::default();
        let mut pos = 0usize;
        // Byte ranges of the current field: name, then value pieces.
        let mut current: Option<(String, Vec<u8>)> = None;
        let mut first_line = true;
        let body_start = loop {
            if pos >= input.len() {
                break input.len();
            }
            let rest = input.get(pos..).unwrap_or_default();
            let (line, next) = match rest.iter().position(|&b| b == b'\n') {
                Some(nl) => (rest.get(..nl).unwrap_or_default(), pos + nl + 1),
                None => (rest, input.len()),
            };
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                break next;
            }
            if matches!(line.first(), Some(b' ' | b'\t')) {
                if let Some((_, value)) = current.as_mut() {
                    // Unfold: drop the line break, keep the leading whitespace.
                    if value.len() < MAX_HEADER_VALUE {
                        value.extend_from_slice(line);
                    }
                }
                pos = next;
                first_line = false;
                continue;
            }
            if first_line && is_mbox_from_line(line) {
                pos = next;
                first_line = false;
                continue;
            }
            first_line = false;
            let Some(colon) = line.iter().position(|&b| b == b':') else { break pos };
            let raw_name = line.get(..colon).unwrap_or_default();
            let name_trimmed = trim_ws(raw_name);
            if name_trimmed.is_empty() || !name_trimmed.iter().all(|&b| (33..=126).contains(&b) && b != b':')
            {
                // Also accept the mbox ">From " escapes and similar junk silently by ending the block.
                break pos;
            }
            if let Some((n, v)) = current.take() {
                push_field(&mut headers, n, &v);
            }
            let name = String::from_utf8_lossy(name_trimmed).into_owned();
            let mut value = line.get(colon + 1..).unwrap_or_default().to_vec();
            value.truncate(MAX_HEADER_VALUE);
            current = Some((name, value));
            pos = next;
        };
        if let Some((n, v)) = current.take() {
            push_field(&mut headers, n, &v);
        }
        (headers, body_start.min(input.len()))
    }
}

/// `From addr date` (mbox separator), as opposed to a `From:` / `From :` field.
fn is_mbox_from_line(line: &[u8]) -> bool {
    match line.strip_prefix(b"From ") {
        Some(rest) => trim_ws(rest).first() != Some(&b':'),
        None => false,
    }
}

fn push_field(headers: &mut Headers, name: String, value: &[u8]) {
    if headers.0.len() >= MAX_HEADERS {
        return;
    }
    let text = crate::charset::decode_unlabelled(trim_ws(value));
    headers.0.push(Header { name, value: text });
}

fn trim_ws(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|c| !matches!(c, b' ' | b'\t')).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !matches!(c, b' ' | b'\t')).map_or(start, |e| e + 1);
    b.get(start..end).unwrap_or_default()
}

/// Folds `Name: value` at whitespace so lines stay within 78 columns where possible (RFC 5322 §2.2.3). Words
/// longer than a line are left intact. CR and LF in `value` are replaced by spaces (no header injection).
/// Returns the folded field without a trailing CRLF.
pub fn fold(name: &str, value: &str) -> String {
    fold_width(name, value, 78)
}

/// [`fold`] with an explicit line width.
pub fn fold_width(name: &str, value: &str, width: usize) -> String {
    let clean: String = value.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect();
    let mut out = String::with_capacity(name.len() + clean.len() + 8);
    out.push_str(name);
    out.push(':');
    // Chunks: each starts with its whitespace run (except the first) and ends before the next whitespace run.
    let mut chunks: Vec<&str> = Vec::new();
    let bytes = clean.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_word = false;
    while let Some(&b) = bytes.get(i) {
        let ws = b == b' ' || b == b'\t';
        if ws && in_word {
            if let Some(c) = clean.get(start..i) {
                chunks.push(c);
            }
            start = i;
        }
        in_word = !ws;
        i += 1;
    }
    if let Some(c) = clean.get(start..)
        && !c.is_empty()
    {
        chunks.push(c);
    }
    let mut line_len = out.len();
    for (k, chunk) in chunks.iter().enumerate() {
        let piece_len = chunk.chars().count();
        if k == 0 {
            // The first chunk follows "Name:" and needs a separating space unless it already starts with one.
            if !chunk.starts_with([' ', '\t']) {
                out.push(' ');
                line_len += 1;
            }
            out.push_str(chunk);
            line_len += piece_len;
            continue;
        }
        // Never fold before a whitespace-only chunk (trailing whitespace): that would emit a line of only
        // whitespace, which RFC 5322 §3.2.2 forbids and which some parsers take as the end of the header block.
        let ws_only = chunk.bytes().all(|b| b == b' ' || b == b'\t');
        if line_len + piece_len > width && line_len > 0 && !ws_only {
            out.push_str("\r\n");
            line_len = 0;
        }
        out.push_str(chunk);
        line_len += piece_len;
    }
    if chunks.is_empty() {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_crlf_and_lf() {
        let (h, off) = Headers::parse(b"Subject: Hi\r\nFrom: a@b.c\r\n\r\nBody");
        assert_eq!(h.get("subject"), Some("Hi"));
        assert_eq!(h.get("FROM"), Some("a@b.c"));
        assert_eq!(off, 28);
        let (h, off) = Headers::parse(b"Subject: Hi\nFrom: a@b.c\n\nBody");
        assert_eq!(h.len(), 2);
        assert_eq!(off, 25);
    }

    #[test]
    fn unfolds() {
        let (h, _) = Headers::parse(b"Subject: a long\r\n subject\r\n\tline\r\n\r\n");
        assert_eq!(h.get("Subject"), Some("a long subject\tline"));
    }

    #[test]
    fn obsolete_space_before_colon_and_empty_values() {
        let (h, _) = Headers::parse(b"Subject : spaced\r\nX-Empty:\r\nX-Blank:   \r\n\r\n");
        assert_eq!(h.get("Subject"), Some("spaced"));
        assert_eq!(h.get("X-Empty"), Some(""));
        assert_eq!(h.get("X-Blank"), Some(""));
    }

    #[test]
    fn mbox_from_line_skipped() {
        let (h, _) = Headers::parse(b"From someone@example.com Mon Jan  1 00:00:00 2024\nSubject: x\n\n");
        assert_eq!(h.get("Subject"), Some("x"));
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn non_field_line_starts_body() {
        let input = b"Subject: x\nthis is not a header\nmore\n";
        let (h, off) = Headers::parse(input);
        assert_eq!(h.len(), 1);
        assert_eq!(input.get(off..), Some(&b"this is not a header\nmore\n"[..]));
        let (h, off) = Headers::parse(b"Hello there\n");
        assert!(h.is_empty());
        assert_eq!(off, 0);
    }

    #[test]
    fn no_blank_line_means_all_headers() {
        let (h, off) = Headers::parse(b"Subject: x\r\nTo: y");
        assert_eq!(h.len(), 2);
        assert_eq!(off, 17);
        let (h, off) = Headers::parse(b"");
        assert!(h.is_empty());
        assert_eq!(off, 0);
    }

    #[test]
    fn eight_bit_values() {
        let (h, _) = Headers::parse("Subject: café ✓\n\n".as_bytes());
        assert_eq!(h.get("Subject"), Some("café ✓"));
        let (h, _) = Headers::parse(b"Subject: caf\xe9\n\n");
        assert_eq!(h.get("Subject"), Some("café"));
    }

    #[test]
    fn text_decodes_encoded_words() {
        let (h, _) = Headers::parse(b"Subject: =?utf-8?q?caf=C3=A9?=\n\n");
        assert_eq!(h.text("subject").as_deref(), Some("café"));
    }

    #[test]
    fn set_remove_get_all() {
        let mut h = Headers::default();
        h.push("Received", "a");
        h.push("X", "1");
        h.push("received", "b");
        assert_eq!(h.get_all("RECEIVED").collect::<Vec<_>>(), vec!["a", "b"]);
        h.set("Received", "c");
        assert_eq!(h.get_all("received").collect::<Vec<_>>(), vec!["c"]);
        assert_eq!(h.len(), 2);
        assert_eq!(h.remove("x"), 1);
        assert!(!h.contains("X"));
        h.set("New", "v");
        assert_eq!(h.get("new"), Some("v"));
    }

    #[test]
    fn folding() {
        let v = "word ".repeat(40);
        let f = fold("Subject", v.trim());
        for line in f.split("\r\n") {
            assert!(line.len() <= 78, "{line:?}");
        }
        let (h, _) = Headers::parse(format!("{f}\r\n\r\n").as_bytes());
        assert_eq!(h.get("Subject"), Some(v.trim()));
        assert_eq!(fold("X", "short"), "X: short");
        assert_eq!(fold("X", ""), "X: ");
        assert_eq!(fold("X", "a\r\nInjected: yes"), "X: a  Injected: yes");
        let long = "x".repeat(100);
        assert_eq!(fold("X", &long), format!("X: {long}"));
    }

    #[test]
    fn to_bytes_round_trip() {
        let mut h = Headers::default();
        h.push("Subject", "hello world");
        h.push("To", "a@b.c");
        let bytes = h.to_bytes();
        let (back, _) = Headers::parse(&bytes);
        assert_eq!(back, h);
    }

    proptest! {
        #[test]
        fn never_panics(data in proptest::collection::vec(any::<u8>(), 0..500)) {
            let (h, off) = Headers::parse(&data);
            prop_assert!(off <= data.len());
            let _ = h.text("subject");
        }

        #[test]
        fn fold_unfold_round_trip(words in proptest::collection::vec("[a-zA-Z0-9!#.,é]{1,30}", 1..30)) {
            let value = words.join(" ");
            let folded = fold("Subject", &value);
            for line in folded.split("\r\n") {
                prop_assert!(line.chars().count() <= 78 || !line.trim_start().contains(' '));
            }
            let (h, _) = Headers::parse(format!("{folded}\r\n\r\n").as_bytes());
            prop_assert_eq!(h.get("Subject"), Some(value.as_str()));
        }
    }
}
