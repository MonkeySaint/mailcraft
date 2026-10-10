//! Match ranges for highlighting search hits in the message list and reading pane.

use crate::eval::word_matches;
use crate::query::{Query, Term, field};
use crate::token::tokenize;

/// A field shown with highlights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Subject,
    From,
    To,
    Cc,
    Body,
    Attachments,
}

impl Field {
    fn bit(self) -> u8 {
        match self {
            Field::Subject => field::SUBJECT,
            Field::From => field::FROM,
            Field::To => field::TO,
            Field::Cc => field::CC,
            Field::Body => field::BODY,
            Field::Attachments => field::ATTACHMENTS,
        }
    }
}

/// Most ranges returned for one text.
pub const MAX_HIGHLIGHTS: usize = 1_000;

/// Byte ranges of `text` (shown as `field`) that the query's words match, sorted and merged. Words under a
/// NOT are not highlighted; a phrase is highlighted as one range from its first to its last word.
pub fn highlights(q: &Query, f: Field, text: &str) -> Vec<(usize, usize)> {
    let bit = f.bit();
    let texts: Vec<_> = q
        .terms()
        .into_iter()
        .filter(|(_, neg)| !neg)
        .filter_map(|(t, _)| match t {
            Term::Text(tf, t) if tf.mask() & bit != 0 => Some(t),
            _ => None,
        })
        .collect();
    if texts.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let toks = tokenize(text);
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for t in texts {
        let k = t.words.len();
        if k == 0 || toks.len() < k {
            continue;
        }
        for i in 0..=toks.len() - k {
            let hit = t.words.iter().enumerate().all(|(j, w)| {
                toks.get(i + j).is_some_and(|tok| word_matches(w, &tok.text, t.prefix && j + 1 == k))
            });
            if hit && let (Some(a), Some(b)) = (toks.get(i), toks.get(i + k - 1)) {
                ranges.push((a.start, b.end));
            }
            if ranges.len() >= MAX_HIGHLIGHTS * 4 {
                break;
            }
        }
    }
    ranges.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (s, e) in ranges {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out.truncate(MAX_HIGHLIGHTS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse;

    fn marked(q: &str, f: Field, text: &str) -> Vec<String> {
        highlights(&parse(q), f, text).into_iter().map(|(s, e)| text[s..e].to_owned()).collect()
    }

    #[test]
    fn marks_words_and_phrases() {
        assert_eq!(marked("bud", Field::Subject, "Budget review: budgets"), ["Budget", "budgets"]);
        assert_eq!(marked("\"budget review\"", Field::Subject, "The Budget  review"), ["Budget  review"]);
        assert_eq!(marked("cafe", Field::Body, "Le Café, café"), ["Café", "café"]);
        assert_eq!(marked("subject:plan", Field::Body, "plan"), Vec::<String>::new());
        assert_eq!(marked("subject:plan", Field::Subject, "plan"), ["plan"]);
        assert_eq!(marked("-plan other", Field::Subject, "plan other"), ["other"]);
        assert_eq!(marked("from:ann", Field::From, "Ann Lee <ann@example.com>"), ["Ann", "ann"]);
        assert_eq!(marked("a ab", Field::Body, "abc"), ["abc"]);
    }
}
