//! Subject prefixes ("RE: ", "FW: ") and conversation topic normalisation (`plan/outlook/reply-forward.md`).

/// Reply/forward markers recognised at the start of a subject (compared ASCII case-insensitively).
/// Longer tokens first so "Fwd" is tried before "Fw".
const PREFIX_TOKENS: [&str; 7] = ["fwd", "fw", "re", "aw", "wg", "sv", "vs"];

/// Upper bound on prefixes removed (hostile subjects like "re:re:re:…" stay linear anyway; this just
/// bounds the loop explicitly).
const MAX_PREFIXES: usize = 10_000;

/// Tries to match one prefix ("Re:", "RE[2]:", "Fwd :", "AW(3):") at the very start of `s`; returns the
/// byte length consumed (including whitespace after the colon).
fn prefix_len(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let token_len = PREFIX_TOKENS.iter().find_map(|tok| {
        let head = bytes.get(..tok.len())?;
        head.eq_ignore_ascii_case(tok.as_bytes()).then_some(tok.len())
    })?;
    let mut i = token_len;
    // Optional counter: "[2]" or "(2)".
    if let Some(&open) = bytes.get(i)
        && (open == b'[' || open == b'(')
    {
        let close = if open == b'[' { b']' } else { b')' };
        let mut j = i + 1;
        let mut digits = 0usize;
        while let Some(b) = bytes.get(j) {
            if b.is_ascii_digit() {
                digits += 1;
                j += 1;
            } else {
                break;
            }
        }
        if digits == 0 || bytes.get(j) != Some(&close) {
            return None;
        }
        i = j + 1;
    }
    // Optional spaces before the colon ("Re :" as some clients write it).
    while bytes.get(i) == Some(&b' ') {
        i += 1;
    }
    // ASCII colon or the full-width colon U+FF1A.
    let rest = s.get(i..)?;
    let after_colon = rest.strip_prefix(':').or_else(|| rest.strip_prefix('\u{FF1A}'))?;
    let trimmed = after_colon.trim_start();
    Some(s.len() - trimmed.len())
}

/// Removes any leading reply/forward prefixes ("Re:", "RE:", "Fw:", "FW:", "Fwd:", "AW:", "WG:", "SV:", "VS:",
/// with optional "[n]" counters and surrounding whitespace), repeatedly. Used for conversation topics.
pub fn strip_prefixes(subject: &str) -> &str {
    let mut s = subject.trim();
    for _ in 0..MAX_PREFIXES {
        match prefix_len(s).and_then(|n| s.get(n..)) {
            Some(rest) => s = rest,
            None => break,
        }
    }
    s.trim()
}

/// True when the subject starts with a reply/forward prefix.
pub fn has_prefix(subject: &str) -> bool {
    prefix_len(subject.trim_start()).is_some()
}

/// Reply subject: existing prefix replaced, not stacked: "Re: Lunch" → "RE: Lunch"; "Lunch" → "RE: Lunch".
pub fn reply_subject(subject: &str) -> String {
    format!("RE: {}", strip_prefixes(subject))
}

/// Forward subject: "FW: <topic>".
pub fn forward_subject(subject: &str) -> String {
    format!("FW: {}", strip_prefixes(subject))
}

/// Conversation topic key (prefix-stripped, case-folded, whitespace collapsed).
pub fn conversation_topic(subject: &str) -> String {
    let stripped = strip_prefixes(subject);
    let mut out = String::with_capacity(stripped.len());
    for word in stripped.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out.to_lowercase()
}
