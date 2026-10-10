//! Word tokens for indexing, querying and highlighting.
//!
//! A token is a run of letters and digits (`char::is_alphanumeric`, with combining marks kept inside the
//! word); everything else separates tokens, so `ann.lee@example.com` is `ann`, `lee`, `example`, `com` and
//! a phrase query for the address matches those four tokens in a row. Han, Hiragana and Katakana characters
//! are one token each (no spaces between words in those scripts), so any substring of CJK text is a
//! phrase of single-character tokens. Tokens are folded ([`crate::fold`]) and capped at
//! [`MAX_TOKEN_CHARS`] folded chars; offsets always describe the original text.

use crate::fold::fold_char_into;

/// Longest token kept (in chars of the original run); longer runs are cut (the offsets still cover the whole
/// run).
pub const MAX_TOKEN_CHARS: usize = 64;

/// One token: byte range in the original text and its folded form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// True for scripts written without spaces, indexed one character per token.
fn is_ideograph(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30FF}'   // Hiragana, Katakana
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
        | '\u{20000}'..='\u{2FA1F}')
}

fn is_mark(c: char) -> bool {
    matches!(c, '\u{0300}'..='\u{036F}' | '\u{1AB0}'..='\u{1AFF}' | '\u{1DC0}'..='\u{1DFF}' | '\u{20D0}'..='\u{20FF}' | '\u{FE20}'..='\u{FE2F}')
}

/// Calls `f(start, end, folded)` for every token of `text`, in order. `buf` is scratch space reused between
/// tokens (and calls), so indexing allocates nothing per token.
pub fn for_each_token(text: &str, buf: &mut String, mut f: impl FnMut(usize, usize, &str)) {
    let mut start: Option<usize> = None;
    let mut chars = 0usize;
    buf.clear();
    for (i, c) in text.char_indices() {
        let word = c.is_alphanumeric() || (start.is_some() && is_mark(c));
        if word && is_ideograph(c) {
            if let Some(s) = start.take() {
                if !buf.is_empty() {
                    f(s, i, buf);
                }
                buf.clear();
            }
            fold_char_into(c, buf);
            f(i, i + c.len_utf8(), buf);
            buf.clear();
            chars = 0;
            continue;
        }
        if word {
            if start.is_none() {
                start = Some(i);
                chars = 0;
            }
            if chars < MAX_TOKEN_CHARS {
                fold_char_into(c, buf);
                chars += 1;
            }
        } else if let Some(s) = start.take() {
            if !buf.is_empty() {
                f(s, i, buf);
            }
            buf.clear();
        }
    }
    if let Some(s) = start
        && !buf.is_empty()
    {
        f(s, text.len(), buf);
    }
    buf.clear();
}

/// All tokens of `text`.
pub fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for_each_token(text, &mut buf, |start, end, t| out.push(Token { start, end, text: t.to_owned() }));
    out
}

/// The folded words of `text` (no offsets).
pub fn words(text: &str) -> Vec<String> {
    tokenize(text).into_iter().map(|t| t.text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_folds() {
        let t = tokenize("Re: Café-menu for ANN.lee@Example.com!");
        let w: Vec<&str> = t.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(w, ["re", "cafe", "menu", "for", "ann", "lee", "example", "com"]);
        assert_eq!(&"Re: Café-menu"[t[1].start..t[1].end], "Café");
    }

    #[test]
    fn ideographs_are_single_tokens() {
        assert_eq!(words("会議の資料 v2"), ["会", "議", "の", "資", "料", "v2"]);
        assert_eq!(words("ab会c"), ["ab", "会", "c"]);
    }

    #[test]
    fn combining_marks_stay_in_the_word() {
        assert_eq!(words("re\u{301}sume\u{301} x"), ["resume", "x"]);
        assert_eq!(words("\u{301}abc"), ["abc"]);
    }

    #[test]
    fn long_runs_are_capped() {
        let long = "a".repeat(500);
        let t = tokenize(&long);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].text.len(), MAX_TOKEN_CHARS);
        assert_eq!(t[0].end, 500);
    }
}
