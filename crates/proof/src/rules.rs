//! Grammar rules beyond the mechanical checks: article agreement ("a apple"), subject–verb agreement with
//! personal pronouns ("she don't"), punctuation spacing and doubled marks, the lower-case pronoun "i", and
//! commonly confused words ("could of", "their is", "better then"). The rules are MailCraft's own and
//! deliberately narrow: each fires only in a context where the fix is almost always right, so a flagged
//! phrase is worth a look and the suggestion can be applied as is.
//!
//! Every rule works on the token list of one text ([`crate::tokenize`]) and the gaps between tokens; all
//! ranges are byte ranges on char boundaries of that text.

use crate::grammar::{Issue, IssueKind, MAX_ISSUES};
use crate::speller::title_case;
use crate::tokenize::Token;

/// The word lower-cased with curly apostrophes made straight.
pub(crate) fn norm(word: &str) -> String {
    word.chars().map(|c| if c == '\u{2019}' { '\'' } else { c }).flat_map(char::to_lowercase).collect()
}

/// Whether `gap` (the text between two words) is plain spacing: one or more spaces or tabs and at most one
/// line break, nothing else.
pub(crate) fn plain_gap(gap: &str) -> bool {
    !gap.is_empty() && gap.chars().all(char::is_whitespace) && gap.matches('\n').count() <= 1
}

/// `replacement` written in the case of `source`: Capitalised when the source starts with a capital, in
/// capitals when the source is (two or more letters, all upper case).
pub(crate) fn match_case(source: &str, replacement: &str) -> String {
    let letters: Vec<char> = source.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() > 1 && letters.iter().all(|c| c.is_uppercase()) {
        return replacement.to_uppercase();
    }
    if source.chars().next().is_some_and(char::is_uppercase) {
        title_case(replacement)
    } else {
        replacement.to_owned()
    }
}

/// Lower-cased words of a token list, with whether each token is a plain word (letters and apostrophes).
struct Words {
    lower: Vec<String>,
}

impl Words {
    fn new(tokens: &[Token<'_>]) -> Self {
        Words { lower: tokens.iter().map(|(_, _, w)| norm(w)).collect() }
    }

    fn get(&self, i: usize) -> &str {
        self.lower.get(i).map_or("", String::as_str)
    }
}

fn gap<'a>(text: &'a str, tokens: &[Token<'_>], i: usize) -> &'a str {
    // The text between token i and token i + 1.
    match (tokens.get(i), tokens.get(i.saturating_add(1))) {
        (Some(&(_, a_end, _)), Some(&(b_start, _, _))) if a_end <= b_start => {
            text.get(a_end..b_start).unwrap_or("x")
        }
        _ => "x",
    }
}

fn push(
    issues: &mut Vec<Issue>,
    start: usize,
    end: usize,
    kind: IssueKind,
    message: &str,
    fix: Option<String>,
) {
    if issues.len() >= MAX_ISSUES || end < start {
        return;
    }
    issues.push(Issue {
        start,
        end,
        kind,
        message: message.to_owned(),
        suggestions: fix.into_iter().collect(),
    });
}

/// Runs every rule of this module on `text` (the confused-word rules only with `confused`).
pub(crate) fn check(text: &str, tokens: &[Token<'_>], confused: bool, issues: &mut Vec<Issue>) {
    let words = Words::new(tokens);
    articles(text, tokens, &words, issues);
    pronoun_agreement(text, tokens, &words, issues);
    lower_case_i(text, tokens, &words, issues);
    if confused {
        confused_words(text, tokens, &words, issues);
    }
    punctuation(text, issues);
}

// ---------------------------------------------------------------------------------------------------------
// a / an

/// Words that start with a vowel letter but a consonant sound ("a user", "a one-time fee", "a European").
const CONSONANT_SOUND_PREFIXES: &[&str] =
    &["one", "once", "use", "usu", "uti", "uri", "ura", "unani", "eu", "ewe", "uni", "ubiq", "uku"];
/// Exceptions to `uni`: "an unimportant", "an uninformed", "an unidentified".
const VOWEL_SOUND_UNI: &[&str] = &["unin", "unim", "unid"];
/// Words that start with a silent h ("an hour").
const SILENT_H: &[&str] = &["hour", "honest", "honor", "honour", "heir", "herb"];
/// Words after "a" that show it is a letter or label ("plan a or b", "option a is"), not the article.
const NOT_AFTER_ARTICLE: &[&str] = &[
    "and", "or", "is", "as", "at", "if", "in", "of", "on", "to", "an", "a", "it", "its", "by", "b", "c", "d",
    "e", "etc", "was", "were", "are", "be",
];

fn vowel_sound(word: &str) -> Option<bool> {
    let first = word.chars().next()?;
    if !first.is_alphabetic() {
        return None;
    }
    if SILENT_H.iter().any(|h| word.starts_with(h)) {
        return Some(true);
    }
    if !matches!(first, 'a' | 'e' | 'i' | 'o' | 'u') {
        return Some(false);
    }
    if VOWEL_SOUND_UNI.iter().any(|p| word.starts_with(p)) {
        return Some(true);
    }
    if CONSONANT_SOUND_PREFIXES.iter().any(|p| word.starts_with(p)) {
        return None;
    }
    // "u" words not listed are uncertain ("a unit", "an umbrella"): only "un", "up", "us", "ug", "ul", "um"
    // count as vowel sounds.
    if first == 'u' {
        let ok = ["un", "up", "us", "ug", "ul", "um"].iter().any(|p| word.starts_with(p));
        return ok.then_some(true);
    }
    Some(true)
}

fn articles(text: &str, tokens: &[Token<'_>], words: &Words, issues: &mut Vec<Issue>) {
    for i in 0..tokens.len().saturating_sub(1) {
        let art = words.get(i);
        if art != "a" && art != "an" {
            continue;
        }
        let (Some(&(start, end, src)), Some(&(_, _, next_raw))) = (tokens.get(i), tokens.get(i + 1)) else {
            continue;
        };
        if !plain_gap(gap(text, tokens, i)) {
            continue;
        }
        let next = words.get(i + 1);
        // Abbreviations and all-capital words are read letter by letter ("an MBA", "a URL"): skipped.
        let letters = next_raw.chars().filter(|c| c.is_alphabetic()).count();
        let all_caps = letters > 1 && next_raw.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase);
        if all_caps
            || !next_raw.chars().all(|c| c.is_alphabetic() || c == '\'' || c == '\u{2019}')
            || NOT_AFTER_ARTICLE.contains(&next)
            || letters < 2
        {
            continue;
        }
        // A capital "A" in the middle of a sentence is likely a label ("Plan A works").
        if src == "A"
            && start > 0
            && !text.get(..start).is_some_and(|b| b.trim_end().ends_with(['.', '!', '?']))
        {
            continue;
        }
        match (art, vowel_sound(next)) {
            ("a", Some(true)) => push(
                issues,
                start,
                end,
                IssueKind::Agreement,
                "Use \u{201C}an\u{201D} before a word that starts with a vowel sound.",
                Some(match_case(src, "an")),
            ),
            ("an", Some(false)) => push(
                issues,
                start,
                end,
                IssueKind::Agreement,
                "Use \u{201C}a\u{201D} before a word that starts with a consonant sound.",
                Some(match_case(src, "a")),
            ),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Subject–verb agreement with personal pronouns

/// Words before a pronoun that make it the object or a question ("did she have", "to you is", "let it
/// have"): the rule is skipped after them.
const NOT_SUBJECT_AFTER: &[&str] = &[
    "did",
    "does",
    "do",
    "would",
    "should",
    "could",
    "will",
    "can",
    "may",
    "might",
    "must",
    "shall",
    "let",
    "make",
    "made",
    "help",
    "helped",
    "to",
    "for",
    "with",
    "of",
    "at",
    "by",
    "from",
    "about",
    "like",
    "than",
    "as",
    "give",
    "gave",
    "tell",
    "told",
    "thank",
    "see",
    "saw",
    "watch",
    "hear",
    "heard",
    "had",
    "have",
    "has",
    "why",
    "how",
    "where",
    "what",
    "when",
    "who",
    "which",
    "whom",
    "is",
    "was",
    "are",
    "were",
    "am",
    "into",
    "onto",
    "upon",
    "without",
    "behind",
    "between",
    "among",
    "toward",
    "towards",
    "doesn't",
    "don't",
    "didn't",
    "isn't",
    "wasn't",
    "aren't",
    "weren't",
    "won't",
    "wouldn't",
    "can't",
    "couldn't",
    "shouldn't",
    "makes",
    "lets",
    "helps",
    "keep",
    "keeps",
    "found",
    "find",
    "want",
    "wants",
    "wanted",
    "need",
    "needs",
    "needed",
    "expect",
    "expected",
    "ask",
    "asked",
    "get",
    "got",
    "bring",
    "brought",
    "send",
    "sent",
    "show",
    "showed",
    "call",
    "called",
    "pay",
    "paid",
    "but",
    "except",
    "all",
    "both",
    "each",
];

/// (pronoun, wrong verb, right verb).
const AGREEMENT: &[(&str, &str, &str)] = &[
    ("he", "don't", "doesn't"),
    ("she", "don't", "doesn't"),
    ("it", "don't", "doesn't"),
    ("he", "are", "is"),
    ("she", "are", "is"),
    ("it", "are", "is"),
    ("he", "have", "has"),
    ("she", "have", "has"),
    ("he", "do", "does"),
    ("she", "do", "does"),
    ("he", "weren't", "wasn't"),
    ("she", "weren't", "wasn't"),
    ("it", "weren't", "wasn't"),
    ("i", "is", "am"),
    ("i", "are", "am"),
    ("i", "has", "have"),
    ("i", "does", "do"),
    ("i", "doesn't", "don't"),
    ("i", "isn't", "am not"),
    ("i", "aren't", "am not"),
    ("i", "were", "was"),
    ("i", "weren't", "wasn't"),
    ("we", "is", "are"),
    ("we", "was", "were"),
    ("we", "has", "have"),
    ("we", "does", "do"),
    ("we", "doesn't", "don't"),
    ("we", "isn't", "aren't"),
    ("we", "wasn't", "weren't"),
    ("they", "is", "are"),
    ("they", "was", "were"),
    ("they", "has", "have"),
    ("they", "does", "do"),
    ("they", "doesn't", "don't"),
    ("they", "isn't", "aren't"),
    ("they", "wasn't", "weren't"),
    ("you", "is", "are"),
    ("you", "was", "were"),
    ("you", "has", "have"),
    ("you", "doesn't", "don't"),
    ("you", "isn't", "aren't"),
    ("you", "wasn't", "weren't"),
];

/// Pronouns that are subjects only after a clause start ("you" and "it" are objects too).
const OBJECT_TOO: &[&str] = &["you", "it"];
/// Words after which a clause starts.
const CLAUSE_START: &[&str] = &[
    "and", "but", "or", "so", "if", "when", "because", "that", "since", "while", "then", "although",
    "though", "unless", "until", "whether", "after", "before", "once", "yet", "hope", "think", "know",
    "believe", "guess", "sure", "now",
];

fn pronoun_agreement(text: &str, tokens: &[Token<'_>], words: &Words, issues: &mut Vec<Issue>) {
    for i in 0..tokens.len().saturating_sub(1) {
        let pron = words.get(i);
        let verb = words.get(i + 1);
        let Some((_, _, right)) = AGREEMENT.iter().find(|(p, v, _)| *p == pron && *v == verb) else {
            continue;
        };
        if !plain_gap(gap(text, tokens, i)) {
            continue;
        }
        // What comes before the pronoun: nothing / a sentence end / a comma, or a word.
        let before_word = i.checked_sub(1).filter(|&b| {
            let g = gap(text, tokens, b);
            plain_gap(g)
        });
        let clause_start = match before_word {
            None => true,
            Some(b) => CLAUSE_START.contains(&words.get(b)),
        };
        if let Some(b) = before_word
            && NOT_SUBJECT_AFTER.contains(&words.get(b))
        {
            continue;
        }
        if OBJECT_TOO.contains(&pron) && !clause_start {
            continue;
        }
        // "If I were you" (subjunctive) and "I were" after "as if" / "wish" are fine.
        if pron == "i" && verb == "were" {
            continue;
        }
        let Some(&(v_start, v_end, v_raw)) = tokens.get(i + 1) else { continue };
        push(
            issues,
            v_start,
            v_end,
            IssueKind::Agreement,
            "The verb does not agree with its subject.",
            Some(match_case(v_raw, right)),
        );
    }
}

/// A lone lower-case "i" used as the pronoun.
fn lower_case_i(text: &str, tokens: &[Token<'_>], words: &Words, issues: &mut Vec<Issue>) {
    for (i, &(start, end, raw)) in tokens.iter().enumerate() {
        if raw != "i" {
            continue;
        }
        // Not "i.e." or a list label "i)": the next char must be whitespace (or an apostrophe form, which
        // the speller already checks), and a word must follow or precede on the line.
        let after = text.get(end..).and_then(|t| t.chars().next());
        if !after.is_some_and(char::is_whitespace) {
            continue;
        }
        let before = text.get(..start).and_then(|t| t.chars().next_back());
        if before.is_some_and(|c| !c.is_whitespace()) {
            continue;
        }
        let next_is_word = tokens.get(i + 1).is_some_and(|(_, _, w)| w.chars().all(char::is_alphabetic))
            && plain_gap(gap(text, tokens, i));
        if !next_is_word || words.get(i + 1).len() < 2 {
            continue;
        }
        push(
            issues,
            start,
            end,
            IssueKind::Capitalization,
            "The pronoun \u{201C}I\u{201D} is written in capitals.",
            Some("I".into()),
        );
    }
}

// ---------------------------------------------------------------------------------------------------------
// Commonly confused words

const MODALS: &[&str] = &["could", "would", "should", "must", "might", "may", "will", "shall"];
const BE_FORMS: &[&str] = &[
    "am", "is", "are", "was", "were", "be", "been", "being", "i'm", "you're", "we're", "they're", "he's",
    "she's", "it's",
];
/// Comparatives that take "than".
const COMPARATIVES: &[&str] = &[
    "more",
    "less",
    "better",
    "worse",
    "rather",
    "other",
    "greater",
    "fewer",
    "larger",
    "smaller",
    "higher",
    "lower",
    "faster",
    "slower",
    "bigger",
    "longer",
    "shorter",
    "cheaper",
    "easier",
    "harder",
    "older",
    "younger",
    "sooner",
    "further",
    "farther",
    "different",
];
/// Words after "better then" that start what is compared ("better then that", "more then ever"), so "then"
/// is "than". A verb after it ("once more then send", "a bit longer then retry") is a sequence.
const AFTER_THAN: &[&str] = &[
    "a",
    "an",
    "the",
    "that",
    "this",
    "these",
    "those",
    "i",
    "you",
    "he",
    "she",
    "we",
    "they",
    "it",
    "me",
    "him",
    "her",
    "us",
    "them",
    "my",
    "your",
    "his",
    "our",
    "their",
    "its",
    "ever",
    "before",
    "usual",
    "expected",
    "planned",
    "anticipated",
    "needed",
    "necessary",
    "anyone",
    "anything",
    "anybody",
    "everyone",
    "everything",
    "everybody",
    "most",
    "last",
    "yesterday",
    "today",
    "average",
    "normal",
    "one",
    "two",
    "three",
    "four",
    "five",
    "ten",
    "twice",
    "half",
];
/// Words before "than" that show it should be "then" ("since then", "by then").
const BEFORE_THEN: &[&str] = &["since", "until", "till", "by", "back", "just", "and"];
/// Words after "your" that show it should be "you're". "Your right" and "your welcome" also name things
/// ("your right to vote", "your welcome pack"), so they count only at the end of a sentence (see
/// [`YOURE_AT_END`]).
const AFTER_YOURE: &[&str] = &["a", "an", "the", "not", "so", "too"];
/// Words after "your" that make it "you're" only when the sentence ends there ("I think your right.").
const YOURE_AT_END: &[&str] = &["right", "welcome"];
/// Words after "its" that show it should be "it's" ("its very existence", "its true value", "its all-time
/// high" are fine, so those words are not here).
const AFTER_ITS: &[&str] = &["a", "an", "the", "been", "not", "going", "just", "so", "too", "ok", "okay"];
/// Verbs or auxiliaries before "loose" that show it should be "lose" (not "not": "the cable is not loose").
const BEFORE_LOSE: &[&str] = &[
    "to", "will", "would", "could", "might", "may", "can", "don't", "didn't", "never", "cannot", "won't",
    "can't", "gonna",
];
/// Words after "lets" that show it should be "let's".
const AFTER_LETS: &[&str] = &[
    "go", "see", "meet", "talk", "get", "start", "try", "discuss", "keep", "make", "do", "have", "move",
    "look", "take", "chat", "plan",
];

/// Whether `word` is a number ("more then 20").
fn next_is_number(word: &str) -> bool {
    word.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn confused_words(text: &str, tokens: &[Token<'_>], words: &Words, issues: &mut Vec<Issue>) {
    let n = tokens.len();
    for i in 0..n {
        let w = words.get(i);
        let Some(&(start, end, raw)) = tokens.get(i) else { continue };
        let next_plain = i + 1 < n && plain_gap(gap(text, tokens, i));
        let next = if next_plain { words.get(i + 1) } else { "" };
        let prev_plain = i > 0 && plain_gap(gap(text, tokens, i.saturating_sub(1)));
        let prev = if prev_plain { words.get(i.saturating_sub(1)) } else { "" };
        let (fix, message): (&str, &str) = match w {
            // "could of" → "could have"
            "of" if MODALS.contains(&prev) => (
                "have",
                "After \u{201C}could\u{201D}, \u{201C}would\u{201D} or \u{201C}should\u{201D}, use \u{201C}have\u{201D}.",
            ),
            "their"
                if matches!(
                    next,
                    "is" | "are" | "was" | "were" | "isn't" | "aren't" | "wasn't" | "weren't"
                ) =>
            {
                ("there", "\u{201C}Their\u{201D} shows ownership; use \u{201C}there\u{201D} here.")
            }
            "there" if next == "own" => ("their", "Use \u{201C}their\u{201D} to show ownership."),
            "then"
                if COMPARATIVES.contains(&prev) && (AFTER_THAN.contains(&next) || next_is_number(next)) =>
            {
                ("than", "Use \u{201C}than\u{201D} in a comparison.")
            }
            "than" if BEFORE_THEN.contains(&prev) => {
                ("then", "Use \u{201C}then\u{201D} for time or sequence.")
            }
            "your"
                if AFTER_YOURE.contains(&next)
                    || (YOURE_AT_END.contains(&next) && {
                        let next_end = tokens.get(i + 1).map_or(end, |t| t.1);
                        let rest = text.get(next_end..).unwrap_or("");
                        rest.trim_start().is_empty()
                            || rest.trim_start().starts_with(['.', '!', ',', ';', '?'])
                    }) =>
            {
                ("you're", "Use \u{201C}you're\u{201D} (you are) here.")
            }
            "its" if AFTER_ITS.contains(&next) => {
                ("it's", "Use \u{201C}it's\u{201D} (it is or it has) here.")
            }
            "it's" if next == "own" => ("its", "Use \u{201C}its\u{201D} to show ownership."),
            "loose" if BEFORE_LOSE.contains(&prev) => {
                ("lose", "Use \u{201C}lose\u{201D} for the verb (to lose something).")
            }
            "lets"
                if AFTER_LETS.contains(&next) && (i == 0 || !prev_plain || CLAUSE_START.contains(&prev)) =>
            {
                ("let's", "Use \u{201C}let's\u{201D} (let us) here.")
            }
            "affect" if matches!(prev, "the" | "an") && matches!(next, "of" | "on") => {
                ("effect", "The noun is usually \u{201C}effect\u{201D}.")
            }
            "suppose" | "use" if next == "to" && BE_FORMS.contains(&prev) => (
                if w == "use" { "used" } else { "supposed" },
                "Use the past participle here (\u{201C}used to\u{201D}, \u{201C}supposed to\u{201D}).",
            ),
            "alot" => ("a lot", "\u{201C}A lot\u{201D} is two words."),
            "irregardless" => ("regardless", "Use \u{201C}regardless\u{201D}."),
            _ => continue,
        };
        push(issues, start, end, IssueKind::ConfusedWord, message, Some(match_case(raw, fix)));
    }
}

// ---------------------------------------------------------------------------------------------------------
// Punctuation

fn punctuation(text: &str, issues: &mut Vec<Issue>) {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let byte = |i: usize| chars.get(i).map_or(text.len(), |&(b, _)| b);
    let mut addr = AddressChunks::default();
    let mut i = 0usize;
    while i < chars.len() {
        if issues.len() >= MAX_ISSUES {
            return;
        }
        let Some(c) = at(i) else { break };
        // Space before , . ; : ! ? after a word ("word ,"): the spaces are removed. Not before an ellipsis or
        // a leading-dot name (" .NET"), not an emoticon (" :)").
        if c == ' ' {
            let run_start = i;
            let mut j = i;
            while at(j) == Some(' ') {
                j += 1;
            }
            let before = run_start.checked_sub(1).and_then(at);
            let mark = at(j);
            let after_mark = at(j + 1);
            if before.is_some_and(char::is_alphanumeric)
                && mark.is_some_and(|m| matches!(m, ',' | '.' | ';' | ':' | '!' | '?'))
                && after_mark.is_none_or(char::is_whitespace)
            {
                push(
                    issues,
                    byte(run_start),
                    byte(j),
                    IssueKind::Punctuation,
                    "Remove the space before the punctuation mark.",
                    Some(String::new()),
                );
            }
            // "( word" / "word )"
            if before == Some('(') && j > run_start && at(j).is_some_and(char::is_alphanumeric) {
                push(
                    issues,
                    byte(run_start),
                    byte(j),
                    IssueKind::Punctuation,
                    "Remove the space after the opening bracket.",
                    Some(String::new()),
                );
            }
            if before.is_some_and(char::is_alphanumeric) && mark == Some(')') && run_start > 0 {
                push(
                    issues,
                    byte(run_start),
                    byte(j),
                    IssueKind::Punctuation,
                    "Remove the space before the closing bracket.",
                    Some(String::new()),
                );
            }
            i = j.max(i + 1);
            continue;
        }
        // Doubled comma or semicolon; two periods that are not an ellipsis.
        if matches!(c, ',' | ';') && at(i + 1) == Some(c) {
            let mut j = i + 1;
            while at(j) == Some(c) {
                j += 1;
            }
            push(
                issues,
                byte(i),
                byte(j),
                IssueKind::Punctuation,
                "Repeated punctuation mark.",
                Some(c.to_string()),
            );
            i = j;
            continue;
        }
        if c == '.'
            && at(i + 1) == Some('.')
            && at(i + 2) != Some('.')
            && i.checked_sub(1).and_then(at) != Some('.')
        {
            let prev_word = i.checked_sub(1).and_then(at).is_some_and(char::is_alphanumeric);
            if prev_word && at(i + 2).is_none_or(char::is_whitespace) {
                push(
                    issues,
                    byte(i),
                    byte(i + 2),
                    IssueKind::Punctuation,
                    "Two periods: use one, or three for an ellipsis.",
                    Some(".".into()),
                );
            }
            i += 2;
            continue;
        }
        // No space after a comma or semicolon between words ("one,two"), not in numbers ("1,000").
        if matches!(c, ',' | ';')
            && i.checked_sub(1).and_then(at).is_some_and(char::is_alphabetic)
            && at(i + 1).is_some_and(char::is_alphabetic)
            && !addr.contains(text, &chars, i)
        {
            push(
                issues,
                byte(i),
                byte(i + 1),
                IssueKind::Punctuation,
                "Add a space after the punctuation mark.",
                Some(format!("{c} ")),
            );
        }
        // "end.Next": a sentence end with no space before a capitalised word.
        if matches!(c, '.' | '!' | '?')
            && i >= 2
            && i.checked_sub(1).and_then(at).is_some_and(char::is_lowercase)
            && i.checked_sub(2).and_then(at).is_some_and(char::is_lowercase)
            && at(i + 1).is_some_and(char::is_uppercase)
            && at(i + 2).is_some_and(char::is_lowercase)
            && !addr.contains(text, &chars, i)
        {
            push(
                issues,
                byte(i),
                byte(i + 1),
                IssueKind::Punctuation,
                "Add a space after the end of the sentence.",
                Some(format!("{c} ")),
            );
        }
        i += 1;
    }
}

/// The whitespace-separated chunk last looked at by [`AddressChunks::contains`]: (first char index, char
/// index past its end, looks like an address). The punctuation pass walks forward, so each chunk is measured
/// once and the pass stays linear even for a long paragraph without spaces.
#[derive(Default)]
struct AddressChunks {
    last: Option<(usize, usize, bool)>,
}

impl AddressChunks {
    /// Whether char `i` of `text` (`chars` = its char indices) lies in a whitespace-separated chunk that
    /// looks like an address (URL, e-mail, path).
    fn contains(&mut self, text: &str, chars: &[(usize, char)], i: usize) -> bool {
        if let Some((start, end, is_addr)) = self.last
            && start <= i
            && i < end
        {
            return is_addr;
        }
        let is_space = |k: usize| chars.get(k).is_some_and(|&(_, c)| c.is_whitespace());
        // Back to the chunk start: never further than the previous chunk's end.
        let floor = match self.last {
            Some((_, end, _)) if end <= i => end,
            _ => 0,
        };
        let mut start = i;
        while start > floor && !is_space(start.saturating_sub(1)) {
            start -= 1;
        }
        let mut end = i;
        while end < chars.len() && !is_space(end) {
            end += 1;
        }
        let byte = |k: usize| chars.get(k).map_or(text.len(), |&(b, _)| b);
        let chunk = text.get(byte(start)..byte(end)).unwrap_or("");
        let is_addr = chunk.contains("://")
            || chunk.contains('@')
            || chunk.contains('/')
            || chunk.contains('\\')
            || chunk.starts_with("www.");
        self.last = Some((start, end, is_addr));
        is_addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vowel_sounds() {
        assert_eq!(vowel_sound("apple"), Some(true));
        assert_eq!(vowel_sound("hour"), Some(true));
        assert_eq!(vowel_sound("user"), None);
        assert_eq!(vowel_sound("unimportant"), Some(true));
        assert_eq!(vowel_sound("university"), None);
        assert_eq!(vowel_sound("book"), Some(false));
        assert_eq!(vowel_sound("one"), None);
    }

    #[test]
    fn case_matching() {
        assert_eq!(match_case("A", "an"), "An");
        assert_eq!(match_case("THEIR", "there"), "THERE");
        assert_eq!(match_case("their", "there"), "there");
    }
}
