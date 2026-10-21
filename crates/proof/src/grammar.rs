//! Whole-text checking: spelling issues, the mechanical checks (repeated words, a capital letter at the start
//! of a sentence, more than one space between words), the grammar rules of [`crate::rules`] (agreement,
//! punctuation, commonly confused words) and the writing refinements of [`crate::refine`].

use crate::speller::{Case, Speller, title_case};
use crate::tokenize::{Token, looks_like_address, tokenize};

/// What an [`Issue`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IssueKind {
    /// A word not in the dictionary (red squiggle).
    Spelling,
    /// The same word twice in a row ("the the").
    RepeatedWord,
    /// A sentence that starts with a lower-case letter.
    Capitalization,
    /// Two or more spaces between words.
    DoubleSpace,
    /// Article or subject-verb agreement ("a apple", "she don't").
    Agreement,
    /// Spacing around punctuation, doubled marks ("word ,", "one,two", ",,").
    Punctuation,
    /// A commonly confused word ("could of", "their is", "better then").
    ConfusedWord,
    /// Refinement (conciseness): a wordy phrase ("in order to").
    Wordiness,
    /// Refinement (clarity): passive voice ("was sent by").
    PassiveVoice,
    /// Refinement: a cliché ("at the end of the day").
    Cliche,
    /// Refinement: an informal word or texting abbreviation ("gonna", "thx", "!!").
    Formality,
    /// Refinement: wording that leaves people out ("chairman", "manpower").
    Inclusive,
}

impl IssueKind {
    /// Every kind, in Editor pane order.
    pub const ALL: [IssueKind; 12] = [
        IssueKind::Spelling,
        IssueKind::RepeatedWord,
        IssueKind::Capitalization,
        IssueKind::DoubleSpace,
        IssueKind::Agreement,
        IssueKind::Punctuation,
        IssueKind::ConfusedWord,
        IssueKind::PassiveVoice,
        IssueKind::Wordiness,
        IssueKind::Cliche,
        IssueKind::Formality,
        IssueKind::Inclusive,
    ];

    /// Whether the editor draws it as a grammar (blue) rather than a spelling (red) squiggle: every
    /// correction that is not a misspelling. Refinements are neither ([`IssueKind::is_refinement`]).
    pub fn is_grammar(self) -> bool {
        !matches!(self, IssueKind::Spelling) && !self.is_refinement()
    }

    /// Whether it is a writing refinement (advice) rather than a correction.
    pub fn is_refinement(self) -> bool {
        matches!(
            self,
            IssueKind::Wordiness
                | IssueKind::PassiveVoice
                | IssueKind::Cliche
                | IssueKind::Formality
                | IssueKind::Inclusive
        )
    }

    /// Stable identifier ("spelling", "repeated", "agreement", "passive"…).
    pub fn id(self) -> &'static str {
        match self {
            IssueKind::Spelling => "spelling",
            IssueKind::RepeatedWord => "repeated",
            IssueKind::Capitalization => "capitalization",
            IssueKind::DoubleSpace => "doubleSpace",
            IssueKind::Agreement => "agreement",
            IssueKind::Punctuation => "punctuation",
            IssueKind::ConfusedWord => "confused",
            IssueKind::Wordiness => "wordiness",
            IssueKind::PassiveVoice => "passive",
            IssueKind::Cliche => "cliche",
            IssueKind::Formality => "formality",
            IssueKind::Inclusive => "inclusive",
        }
    }

    /// The kind with identifier `id` ([`IssueKind::id`]).
    pub fn from_id(id: &str) -> Option<IssueKind> {
        IssueKind::ALL.into_iter().find(|k| k.id() == id)
    }

    /// The Editor pane group: "spelling" and "grammar" (corrections), "clarity", "conciseness", "cliches",
    /// "formality", "inclusiveness" (refinements).
    pub fn category(self) -> &'static str {
        match self {
            IssueKind::Spelling | IssueKind::RepeatedWord => "spelling",
            IssueKind::Capitalization
            | IssueKind::DoubleSpace
            | IssueKind::Agreement
            | IssueKind::Punctuation
            | IssueKind::ConfusedWord => "grammar",
            IssueKind::PassiveVoice => "clarity",
            IssueKind::Wordiness => "conciseness",
            IssueKind::Cliche => "cliches",
            IssueKind::Formality => "formality",
            IssueKind::Inclusive => "inclusiveness",
        }
    }
}

/// One problem in a text. `start..end` is a byte range of the checked text; applying a suggestion replaces
/// exactly that range (an empty suggestion deletes it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub start: usize,
    pub end: usize,
    pub kind: IssueKind,
    pub message: String,
    pub suggestions: Vec<String>,
}

/// At most this many issues are reported for one text; the rest are dropped.
pub const MAX_ISSUES: usize = 10_000;

/// Words that are fine twice in a row ("I had had enough", "the fact that that happened").
const REPEAT_OK: &[&str] = &["had", "that"];

/// Abbreviations whose trailing period does not end a sentence (lower case, without the final period).
const ABBREVIATIONS: &[&str] = &[
    "e.g", "i.e", "etc", "vs", "cf", "approx", "dept", "est", "fig", "inc", "ltd", "co", "corp", "mr", "mrs",
    "ms", "dr", "prof", "st", "jr", "sr", "no", "nos", "vol", "pp", "p", "ca", "al", "a.m", "p.m", "ext",
];

impl Speller {
    /// Spelling and grammar issues in `text`, sorted by position. Spelling issues carry no suggestions (call
    /// [`Speller::suggest`] when the user asks, or use [`Speller::check_text_with_suggestions`]); grammar
    /// issues carry their fix. Repeated words and the grammar checks follow [`crate::SpellOptions`].
    pub fn check_text(&self, text: &str) -> Vec<Issue> {
        self.check_text_inner(text, 0)
    }

    /// Like [`Speller::check_text`], with up to `n` suggestions on each spelling issue (the Spelling and
    /// Grammar pane, F7).
    pub fn check_text_with_suggestions(&self, text: &str, n: usize) -> Vec<Issue> {
        self.check_text_inner(text, n)
    }

    /// Only the misspelled words (`start`, `end`), for drawing squiggles as the user types.
    pub fn misspellings<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Token<'a>> + 'a {
        tokenize(text).filter(move |(_, _, w)| !self.check(w))
    }

    fn check_text_inner(&self, text: &str, n: usize) -> Vec<Issue> {
        let tokens: Vec<Token<'_>> = tokenize(text).collect();
        let mut issues = Vec::new();
        for &(start, end, word) in &tokens {
            if issues.len() >= MAX_ISSUES {
                break;
            }
            if !self.check(word) {
                let suggestions = if n > 0 { self.suggest(word, n) } else { Vec::new() };
                issues.push(Issue {
                    start,
                    end,
                    kind: IssueKind::Spelling,
                    message: format!("\u{201C}{word}\u{201D} may be misspelled."),
                    suggestions,
                });
            }
        }
        if self.options().flag_repeated_words {
            repeated_words(text, &tokens, &mut issues);
        }
        // The grammar rules and refinements are English; other languages get the language-neutral checks.
        let english = self.is_english();
        if self.options().check_grammar {
            sentence_capitals(text, &tokens, &mut issues);
            double_spaces(text, &mut issues);
            if english {
                crate::rules::check(text, &tokens, self.options().confused_words, &mut issues);
            }
        }
        if english {
            crate::refine::check(text, &tokens, self.options().refinements, &mut issues);
        }
        issues.truncate(MAX_ISSUES);
        dedupe(issues)
    }
}

/// Sorts issues by position and drops a grammar issue that overlaps a kept one, and a refinement that
/// overlaps a kept correction or refinement (a misspelling inside either is kept: both are useful).
fn dedupe(mut issues: Vec<Issue>) -> Vec<Issue> {
    let rank = |k: IssueKind| match k {
        IssueKind::Spelling => 0,
        k if k.is_refinement() => 2,
        _ => 1,
    };
    issues
        .sort_by(|a, b| rank(a.kind).cmp(&rank(b.kind)).then(a.start.cmp(&b.start)).then(a.end.cmp(&b.end)));
    let mut kept: Vec<Issue> = Vec::with_capacity(issues.len());
    for issue in issues {
        let clash = rank(issue.kind) > 0
            && kept.iter().any(|k| {
                rank(k.kind) > 0
                    && k.start < issue.end.max(issue.start.saturating_add(1))
                    && issue.start < k.end.max(k.start.saturating_add(1))
            });
        if !clash {
            kept.push(issue);
        }
    }
    kept.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    kept
}

fn is_word(w: &str) -> bool {
    w.chars().any(char::is_alphabetic) && !w.chars().any(char::is_numeric) && !looks_like_address(w)
}

/// "the the": the second word and the whitespace before it, with the fix of deleting them.
fn repeated_words(text: &str, tokens: &[Token<'_>], issues: &mut Vec<Issue>) {
    for pair in tokens.windows(2) {
        if issues.len() >= MAX_ISSUES {
            return;
        }
        let [(_, prev_end, prev), (start, end, word)] = pair else { continue };
        let gap = text.get(*prev_end..*start).unwrap_or("x");
        // Only spaces/tabs and at most one line break between them; no punctuation.
        if gap.is_empty() || !gap.chars().all(char::is_whitespace) || gap.matches('\n').count() > 1 {
            continue;
        }
        if !is_word(word) || prev.to_lowercase() != word.to_lowercase() {
            continue;
        }
        if REPEAT_OK.contains(&word.to_lowercase().as_str()) {
            continue;
        }
        issues.push(Issue {
            start: *prev_end,
            end: *end,
            kind: IssueKind::RepeatedWord,
            message: format!("\u{201C}{word}\u{201D} is repeated."),
            suggestions: vec![String::new()],
        });
    }
}

/// Whether the text between two tokens ends a sentence: a `.`, `!` or `?` (not an ellipsis, not the period of
/// an abbreviation), or a blank line.
fn ends_sentence(gap: &str, prev: Option<&str>) -> bool {
    let mut lines = gap.split('\n');
    lines.next();
    if gap.matches('\n').count() >= 2 && lines.any(|l| l.trim().is_empty()) {
        return true;
    }
    let punct: String = gap.chars().filter(|c| !c.is_whitespace()).collect();
    if punct.contains("..") || punct.contains('\u{2026}') {
        return false;
    }
    let Some(first) = punct.chars().find(|c| matches!(c, '.' | '!' | '?')) else { return false };
    if first == '.'
        && punct.starts_with('.')
        && let Some(p) = prev
    {
        let p = p.to_lowercase();
        // An abbreviation, or an initial ("J. Smith").
        if ABBREVIATIONS.contains(&p.as_str()) || p.chars().count() == 1 {
            return false;
        }
    }
    true
}

/// A lower-case word starting the text or a sentence: suggest the capitalised word.
fn sentence_capitals(text: &str, tokens: &[Token<'_>], issues: &mut Vec<Issue>) {
    let mut prev: Option<&Token<'_>> = None;
    for tok in tokens {
        if issues.len() >= MAX_ISSUES {
            return;
        }
        let &(start, end, word) = tok;
        let starts_sentence = match prev {
            None => text.get(..start).is_some_and(|before| !before.chars().any(|c| c.is_alphanumeric())),
            Some(&(_, prev_end, prev_word)) => {
                text.get(prev_end..start).is_some_and(|gap| ends_sentence(gap, Some(prev_word)))
            }
        };
        prev = Some(tok);
        if !starts_sentence || !is_word(word) || Case::of(word) != Case::Lower {
            continue;
        }
        if !word.chars().next().is_some_and(char::is_lowercase) {
            continue;
        }
        issues.push(Issue {
            start,
            end,
            kind: IssueKind::Capitalization,
            message: "A sentence should start with a capital letter.".to_owned(),
            suggestions: vec![title_case(word)],
        });
    }
}

/// Runs of two or more U+0020 spaces between two non-space characters on one line.
fn double_spaces(text: &str, issues: &mut Vec<Issue>) {
    let mut run_start: Option<usize> = None;
    let mut prev_char: Option<char> = None;
    for (i, c) in text.char_indices() {
        if c == ' ' {
            if run_start.is_none() {
                run_start = Some(i);
            }
            continue;
        }
        if let Some(s) = run_start.take()
            && i - s >= 2
            && !c.is_whitespace()
            && prev_char.is_some_and(|p| !p.is_whitespace())
        {
            if issues.len() >= MAX_ISSUES {
                return;
            }
            issues.push(Issue {
                start: s,
                end: i,
                kind: IssueKind::DoubleSpace,
                message: "More than one space between words.".to_owned(),
                suggestions: vec![" ".to_owned()],
            });
        }
        prev_char = Some(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentence_end_detection() {
        assert!(ends_sentence(". ", Some("done")));
        assert!(ends_sentence("! ", Some("done")));
        assert!(!ends_sentence(". ", Some("e.g")));
        assert!(!ends_sentence(". ", Some("J")));
        assert!(!ends_sentence("... ", Some("well")));
        assert!(!ends_sentence(", ", Some("well")));
        assert!(ends_sentence(",\n\n", Some("Hi")));
        assert!(!ends_sentence(",\n", Some("Hi")));
    }
}
