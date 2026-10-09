//! RFC 2047 encoded words.
//!
//! Decoding is lenient in the ways real mail requires: encoded words need not be surrounded by whitespace,
//! Q-encoded text may contain stray spaces, the charset may carry an RFC 2231 `*language` suffix, and the bytes of
//! adjacent words in the same charset are concatenated before charset decoding (senders routinely split a
//! multi-byte character across two words).

use crate::transfer::{decode_base64, encode_base64_flat, hex_pair};

/// Longest encoded word produced by the encoder (RFC 2047 §2).
pub const MAX_WORD_LEN: usize = 75;
/// Longest charset label accepted when decoding.
const MAX_CHARSET_LEN: usize = 64;

/// Decodes all `=?charset?B|Q?...?=` words in an unstructured header value (adjacent encoded words separated
/// only by whitespace are joined, per RFC 2047 §6.2). Unknown charsets fall back to UTF-8 lossy.
///
/// Runs in time linear in the input, however many `=?` candidates it holds.
pub fn decode_words(input: &str) -> String {
    if !input.contains("=?") {
        return input.to_owned();
    }
    let mut scan = Scanner::new(input);
    let mut out = String::with_capacity(input.len());
    // Bytes of the current run of encoded words that share a charset.
    let mut pending: Option<(String, Vec<u8>)> = None;
    let mut literal_start = 0usize;
    let mut p = 0usize;
    while let Some(rel) = input.get(p..).and_then(|s| s.find("=?")) {
        let j = p + rel;
        let Some(word) = scan.word_at(j) else {
            p = j + 2;
            continue;
        };
        let literal = input.get(literal_start..j).unwrap_or_default();
        let only_ws = literal.chars().all(char::is_whitespace);
        if !(pending.is_some() && only_ws) {
            flush(&mut out, &mut pending);
            out.push_str(literal);
        }
        let bytes = word.decode();
        match pending.as_mut() {
            Some((cs, buf)) if *cs == word.charset => buf.extend_from_slice(&bytes),
            _ => {
                flush(&mut out, &mut pending);
                pending = Some((word.charset, bytes));
            }
        }
        literal_start = word.end;
        p = word.end;
    }
    flush(&mut out, &mut pending);
    out.push_str(input.get(literal_start..).unwrap_or_default());
    out
}

fn flush(out: &mut String, pending: &mut Option<(String, Vec<u8>)>) {
    if let Some((cs, bytes)) = pending.take() {
        out.push_str(&crate::charset::decode(&bytes, &cs));
    }
}

struct Word<'a> {
    charset: String,
    b_encoding: bool,
    payload: &'a str,
    end: usize,
}

impl Word<'_> {
    fn decode(&self) -> Vec<u8> {
        if self.b_encoding {
            decode_base64(self.payload.as_bytes())
        } else {
            decode_q(self.payload.as_bytes())
        }
    }
}

/// The first position at or after a query position where something occurs, remembered between queries. For
/// non-decreasing queries every byte is examined at most once, which keeps a scan over many encoded-word
/// candidates linear (a hostile header full of `=?` with no `?=` would otherwise rescan to the end each time).
#[derive(Default)]
struct NextMatch {
    /// (query position, first match at or after it).
    cached: Option<(usize, Option<usize>)>,
}

impl NextMatch {
    fn find(&mut self, hay: &[u8], pos: usize, is_match: impl Fn(&[u8], usize) -> bool) -> Option<usize> {
        if let Some((from, found)) = self.cached
            && pos >= from
        {
            match found {
                None => return None,
                Some(at) if at >= pos => return Some(at),
                Some(_) => {}
            }
        }
        let found = (pos..hay.len()).find(|&k| is_match(hay, k));
        self.cached = Some((pos, found));
        found
    }
}

/// Encoded-word parser over one header value.
struct Scanner<'a> {
    input: &'a str,
    close: NextMatch,
    line_break: NextMatch,
    blank: NextMatch,
}

impl<'a> Scanner<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            close: NextMatch::default(),
            line_break: NextMatch::default(),
            blank: NextMatch::default(),
        }
    }

    /// Parses an encoded word starting at byte `start` (which points at `=?`).
    fn word_at(&mut self, start: usize) -> Option<Word<'a>> {
        let input = self.input;
        let hay = input.as_bytes();
        let cs_start = start.checked_add(2)?;
        // The charset ends at the next '?', which must come within MAX_CHARSET_LEN bytes.
        let q1 = hay.get(cs_start..)?.iter().take(MAX_CHARSET_LEN + 1).position(|&b| b == b'?')?;
        let charset = input.get(cs_start..cs_start + q1)?;
        if charset.is_empty()
            || !charset
                .bytes()
                .all(|b| b.is_ascii_graphic() && !matches!(b, b'?' | b'=' | b'"' | b'(' | b')'))
        {
            return None;
        }
        let enc_at = cs_start + q1 + 1;
        let b_encoding = match hay.get(enc_at)? {
            b'B' | b'b' => true,
            b'Q' | b'q' => false,
            _ => return None,
        };
        if hay.get(enc_at + 1) != Some(&b'?') {
            return None;
        }
        let payload_start = enc_at + 2;
        let close = self.close.find(hay, payload_start, |h, k| h.get(k..k + 2) == Some(b"?="))?;
        let before_close = |found: Option<usize>| found.is_some_and(|at| at < close);
        if before_close(
            self.line_break.find(hay, payload_start, |h, k| matches!(h.get(k), Some(b'\r' | b'\n'))),
        ) {
            return None;
        }
        if b_encoding
            && before_close(
                self.blank.find(hay, payload_start, |h, k| matches!(h.get(k), Some(b' ' | b'\t'))),
            )
        {
            return None;
        }
        let payload = input.get(payload_start..close)?;
        Some(Word { charset: crate::charset::normalize_label(charset), b_encoding, payload, end: close + 2 })
    }
}

/// RFC 2047 §4.2 "Q" decoding: `_` is a space, `=XX` a byte; anything else is literal (malformed escapes kept).
pub fn decode_q(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0usize;
    while let Some(&c) = input.get(i) {
        match c {
            b'_' => {
                out.push(b' ');
                i += 1;
            }
            b'=' => match hex_pair(input, i + 1) {
                Some(b) => {
                    out.push(b);
                    i += 3;
                }
                None => {
                    out.push(b'=');
                    i += 1;
                }
            },
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// True when `s` cannot appear literally in a header: non-printable-ASCII characters, or text that would be
/// mistaken for an encoded word.
pub fn needs_encoding(s: &str) -> bool {
    s.chars().any(|c| !(' '..='~').contains(&c)) || s.contains("=?") || s.contains("?=")
}

/// Encodes `input` for an unstructured header if it is not plain printable ASCII (UTF-8, B or Q, whichever is
/// shorter; words ≤ 75 chars). Returns the input unchanged when no encoding is needed.
///
/// Words (space-separated) that are plain ASCII stay readable; only runs of words that need encoding are
/// encoded, so `Re: Café` becomes `Re: =?UTF-8?Q?Caf=C3=A9?=`.
pub fn encode_words(input: &str) -> String {
    if !needs_encoding(input) {
        return input.to_owned();
    }
    let words: Vec<&str> = input.split(' ').collect();
    let mut enc: Vec<bool> = words.iter().map(|w| needs_encoding(w)).collect();
    // Empty words (runs of spaces) between two encoded words must be encoded too, or the decoder would drop the
    // whitespace between the adjacent encoded words.
    let n = words.len();
    for i in 0..n {
        if words.get(i).is_some_and(|w| w.is_empty()) {
            let before = (0..i).rev().find(|&k| words.get(k).is_some_and(|w| !w.is_empty()));
            let after = (i + 1..n).find(|&k| words.get(k).is_some_and(|w| !w.is_empty()));
            let encoded = |k: Option<usize>| k.and_then(|k| enc.get(k).copied()).unwrap_or(false);
            if encoded(before)
                && encoded(after)
                && let Some(e) = enc.get_mut(i)
            {
                *e = true;
            }
        }
    }
    let mut pieces: Vec<String> = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    for (w, e) in words.iter().zip(enc.iter()) {
        if *e {
            run.push(w);
        } else {
            if !run.is_empty() {
                pieces.push(encode_run(&run.join(" ")));
                run.clear();
            }
            pieces.push((*w).to_owned());
        }
    }
    if !run.is_empty() {
        pieces.push(encode_run(&run.join(" ")));
    }
    pieces.join(" ")
}

/// Encodes a display-name phrase: unchanged when plain printable ASCII, otherwise the whole phrase becomes
/// encoded words (so specials such as `,` cannot leak into an address list).
pub fn encode_phrase(input: &str) -> String {
    if !needs_encoding(input) {
        return input.to_owned();
    }
    encode_run(input)
}

/// Characters that may appear unescaped in a Q-encoded word in any context (RFC 2047 §5 rule 3).
fn q_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'!' | b'*' | b'+' | b'-' | b'/')
}

fn q_len(b: u8) -> usize {
    if q_safe(b) || b == b' ' { 1 } else { 3 }
}

/// Encodes `text` as one or more `=?UTF-8?…?=` words separated by single spaces, never splitting a character.
pub fn encode_run(text: &str) -> String {
    encode_run_limited(text, MAX_WORD_LEN)
}

/// [`encode_run`] with a smaller maximum word length (clamped to 16..=75), for contexts such as a quoted
/// parameter value where the surrounding syntax also has to fit on the line.
pub fn encode_run_limited(text: &str, max_word_len: usize) -> String {
    const PREFIX_LEN: usize = "=?UTF-8?Q?".len() + "?=".len();
    let budget = max_word_len.clamp(16, MAX_WORD_LEN) - PREFIX_LEN;
    let q_total: usize = text.bytes().map(q_len).sum();
    let b_total = text.len().div_ceil(3) * 4;
    let use_b = b_total < q_total;
    let mut words: Vec<String> = Vec::new();
    let mut chunk = String::new();
    let mut chunk_len = 0usize;
    for c in text.chars() {
        let mut buf = [0u8; 4];
        let bytes = c.encode_utf8(&mut buf).as_bytes();
        let add = if use_b {
            (chunk.len() + bytes.len()).div_ceil(3) * 4 - chunk.len().div_ceil(3) * 4
        } else {
            bytes.iter().copied().map(q_len).sum()
        };
        if chunk_len + add > budget && !chunk.is_empty() {
            words.push(encode_one(&chunk, use_b));
            chunk.clear();
            chunk_len = 0;
        }
        chunk.push(c);
        chunk_len = if use_b { chunk.len().div_ceil(3) * 4 } else { chunk_len + add };
    }
    if !chunk.is_empty() || words.is_empty() {
        words.push(encode_one(&chunk, use_b));
    }
    words.join(" ")
}

fn encode_one(chunk: &str, use_b: bool) -> String {
    if use_b {
        return format!("=?UTF-8?B?{}?=", encode_base64_flat(chunk.as_bytes()));
    }
    let mut s = String::from("=?UTF-8?Q?");
    for b in chunk.bytes() {
        if b == b' ' {
            s.push('_');
        } else if q_safe(b) {
            s.push(char::from(b));
        } else {
            s.push_str(&format!("={b:02X}"));
        }
    }
    s.push_str("?=");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn decodes_b_and_q() {
        assert_eq!(decode_words("=?UTF-8?B?Y2Fmw6k=?="), "café");
        assert_eq!(decode_words("=?utf-8?q?caf=C3=A9?="), "café");
        assert_eq!(decode_words("=?iso-8859-1?q?caf=E9_cr=E8me?="), "café crème");
        assert_eq!(decode_words("=?ISO-8859-1?Q?a?= b"), "a b");
        assert_eq!(decode_words("plain text"), "plain text");
    }

    #[test]
    fn rfc2047_section8_examples() {
        assert_eq!(decode_words("(=?ISO-8859-1?Q?a?=)"), "(a)");
        assert_eq!(decode_words("(=?ISO-8859-1?Q?a?= b)"), "(a b)");
        assert_eq!(decode_words("(=?ISO-8859-1?Q?a?= =?ISO-8859-1?Q?b?=)"), "(ab)");
        assert_eq!(decode_words("(=?ISO-8859-1?Q?a?=  =?ISO-8859-1?Q?b?=)"), "(ab)");
        assert_eq!(decode_words("(=?ISO-8859-1?Q?a?=\r\n    =?ISO-8859-1?Q?b?=)"), "(ab)");
        assert_eq!(decode_words("(=?ISO-8859-1?Q?a_b?=)"), "(a b)");
        assert_eq!(decode_words("(=?ISO-8859-1?Q?a?= =?ISO-8859-2?Q?_b?=)"), "(a b)");
        assert_eq!(decode_words("=?US-ASCII?Q?Keith_Moore?="), "Keith Moore");
    }

    #[test]
    fn split_multibyte_character_is_rejoined() {
        // "é" = C3 A9 split across two words.
        assert_eq!(decode_words("=?utf-8?q?caf=C3?= =?utf-8?q?=A9?="), "café");
        assert_eq!(decode_words("=?UTF-8?B?w6nDqQ==?= =?UTF-8?B?w6k=?="), "ééé");
    }

    #[test]
    fn several_charsets() {
        assert_eq!(decode_words("=?Shift_JIS?B?gqA=?="), "あ");
        assert_eq!(decode_words("=?gb2312?B?xOO6ww==?="), "你好");
        assert_eq!(decode_words("=?KOI8-R?B?0NLJ18XU?="), "привет");
        assert_eq!(decode_words("=?windows-1251?Q?=EF=F0=E8=E2=E5=F2?="), "привет");
        assert_eq!(decode_words("=?ISO-2022-JP?B?GyRCJCIbKEI=?="), "あ");
        assert_eq!(decode_words("=?utf-8*en?q?hi?="), "hi");
        assert_eq!(decode_words("=?x-unknown?q?hi?="), "hi");
    }

    #[test]
    fn mixed_and_malformed() {
        assert_eq!(decode_words("Re: =?utf-8?q?caf=C3=A9?= ok"), "Re: café ok");
        assert_eq!(decode_words("x=?utf-8?q?y?=z"), "xyz");
        assert_eq!(decode_words("=?utf-8?x?bad?="), "=?utf-8?x?bad?=");
        assert_eq!(decode_words("=?utf-8?q?unterminated"), "=?utf-8?q?unterminated");
        assert_eq!(decode_words("=??q?nocharset?="), "=??q?nocharset?=");
        assert_eq!(decode_words("=?utf-8?q?with space?="), "with space");
        assert_eq!(decode_words("=?utf-8?b?d2l0aC Bzc?="), "=?utf-8?b?d2l0aC Bzc?=");
        assert_eq!(decode_words("a =?utf-8?q?=ZZ?= b"), "a =ZZ b");
        assert_eq!(decode_words("=?"), "=?");
        assert_eq!(decode_words("=?utf-8?q?"), "=?utf-8?q?");
    }

    #[test]
    fn encode_leaves_ascii_alone() {
        assert_eq!(encode_words("Hello world"), "Hello world");
        assert_eq!(encode_phrase("John Doe"), "John Doe");
    }

    #[test]
    fn encode_only_needed_words() {
        let e = encode_words("Re: Café meeting");
        assert!(e.starts_with("Re: =?UTF-8?"), "{e}");
        assert!(e.ends_with(" meeting"), "{e}");
        assert_eq!(decode_words(&e), "Re: Café meeting");
    }

    #[test]
    fn encode_long_text_splits_words() {
        let s = "日本語のテキスト".repeat(10);
        let e = encode_words(&s);
        for w in e.split(' ') {
            assert!(w.len() <= MAX_WORD_LEN, "{w}");
        }
        assert_eq!(decode_words(&e), s);
        let s = "é".repeat(80);
        let e = encode_phrase(&s);
        assert!(e.split(' ').all(|w| w.len() <= MAX_WORD_LEN));
        assert_eq!(decode_words(&e), s);
    }

    #[test]
    fn encode_literal_encoded_word_lookalike() {
        let s = "see =?utf-8?q?x?= here";
        let e = encode_words(s);
        assert_ne!(e, s);
        assert_eq!(decode_words(&e), s);
    }

    #[test]
    fn encode_spaces_between_encoded_words() {
        for s in ["é  é", "é   é x", " é", "é ", "a  é  b", "é\té", "\u{0}"] {
            assert_eq!(decode_words(&encode_words(s)), s, "{s:?}");
        }
    }

    proptest! {
        #[test]
        fn round_trip(s in "\\PC{0,120}") {
            let e = encode_words(&s);
            prop_assert_eq!(decode_words(&e), s.clone());
            let p = encode_phrase(&s);
            prop_assert_eq!(decode_words(&p), s);
        }

        #[test]
        fn round_trip_spaces(s in "[a-zé ✓\t]{0,60}") {
            prop_assert_eq!(decode_words(&encode_words(&s)), s);
        }

        #[test]
        fn decode_never_panics(s in "(=\\?|\\?=|[a-zA-Z0-9_=?\\-* ]|\\PC){0,80}") {
            let _ = decode_words(&s);
        }
    }
}
