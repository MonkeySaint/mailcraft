//! The spell checker: word lists, case rules, user dictionary, ignore list, suggestions.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, OnceLock};

use crate::dict::{Dictionary, hash_chars, lower_chars};
use crate::distance::{self, MAX_LEN};
use crate::hunspell::Hunspell;
use crate::tokenize::looks_like_address;

/// Which words the checker skips, mirroring the proofing options of the classic client.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SpellOptions {
    /// Skip words written entirely in capitals (`NASA`, `FYI`).
    pub ignore_uppercase: bool,
    /// Skip words that contain a digit (`3rd`, `B2B`, `v1`).
    pub ignore_words_with_numbers: bool,
    /// Skip Internet and file addresses (URLs, e-mail addresses, host names, file names and paths).
    pub ignore_internet_and_file_addresses: bool,
    /// Report "the the" (see [`Speller::check_text`]).
    pub flag_repeated_words: bool,
    /// Run the grammar checks (sentence capitals, double spaces, agreement, punctuation, confused words) in
    /// [`Speller::check_text`].
    pub check_grammar: bool,
    /// "Frequently confused words": with [`SpellOptions::check_grammar`], flag "could of", "their is",
    /// "better then", "your welcome" and the like.
    pub confused_words: bool,
    /// Writing refinements reported by [`Speller::check_text`] (all on by default).
    pub refinements: crate::Refinements,
    /// Suggest from the main dictionary only: user-dictionary words still pass the check but are never
    /// offered as replacements ([`Speller::suggest`]).
    pub main_dictionary_only: bool,
}

impl Default for SpellOptions {
    fn default() -> Self {
        SpellOptions {
            ignore_uppercase: true,
            ignore_words_with_numbers: true,
            ignore_internet_and_file_addresses: true,
            flag_repeated_words: true,
            check_grammar: true,
            confused_words: true,
            refinements: crate::Refinements::ALL,
            main_dictionary_only: false,
        }
    }
}

/// Words longer than this (in chars) are not looked up: they are accepted by [`Speller::check`] (nobody can
/// fix a 65-letter "word" from a suggestion list) and get no suggestions.
pub const MAX_WORD_CHARS: usize = MAX_LEN;

/// Most user-dictionary or ignore-list entries kept; later additions are dropped.
pub const MAX_USER_WORDS: usize = 100_000;

/// A spell checker for one language. Cheap to clone (the word lists are shared).
///
/// Case rules: a lower-case dictionary word is accepted in lower case, Capitalised (`Hello`) and in capitals
/// (`HELLO`); a proper noun (`Monday`, `Ottawa`) only Capitalised or in capitals; a mixed-case word (`iPhone`)
/// only when the user dictionary or the proper-noun list has that exact spelling. Words with an apostrophe
/// are accepted when they are listed, a known contraction (`don't`, `I'll`, `they've`) or a possessive of an
/// accepted word (`Sam's`, `company's`); curly apostrophes count as straight ones. Hyphenated words and
/// dotted abbreviations are checked part by part (`well-known`, `e.g`). Single letters are always accepted.
#[derive(Clone, Debug)]
pub struct Speller {
    dict: Arc<Dictionary>,
    /// A Hunspell-format dictionary checked instead of (empty) word lists ([`Speller::from_hunspell`]).
    hun: Option<Arc<Hunspell>>,
    user: BTreeSet<String>,
    ignored: HashSet<String>,
    options: SpellOptions,
}

const EN_US_COMMON: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/dictionaries/en-US.txt"));
const EN_US_PROPER: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/dictionaries/en-US-proper.txt"));
const EN_US_FREQ: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/dictionaries/en-US-freq.txt"));

static EN_US: OnceLock<Arc<Dictionary>> = OnceLock::new();

/// Entries of the bundled common list that are misspellings (the source lists carry them as words); they are
/// dropped so the checker flags them and never suggests them.
const EN_US_NOT_WORDS: &[&str] = &[
    "belive",
    "heighth",
    "heighths",
    "maintainance",
    "maintainances",
    "occurence",
    "occurences",
    "questionniare",
    "questionniares",
    "responsiblities",
    "responsiblity",
];

/// Entries of the bundled common list that are only proper nouns (accepted Capitalised or in capitals).
const EN_US_PROPER_ONLY: &[&str] = &["september", "wednesday"];

/// Contractions accepted as whole words (lower case; `i'…` forms must be written with a capital I).
const CONTRACTIONS: &[&str] = &[
    "ain't",
    "aren't",
    "can't",
    "couldn't",
    "daren't",
    "didn't",
    "doesn't",
    "don't",
    "hadn't",
    "hasn't",
    "haven't",
    "isn't",
    "mightn't",
    "mustn't",
    "needn't",
    "oughtn't",
    "shan't",
    "shouldn't",
    "wasn't",
    "weren't",
    "won't",
    "wouldn't",
    "i'm",
    "i've",
    "i'll",
    "i'd",
    "you're",
    "you've",
    "you'll",
    "you'd",
    "he'll",
    "he'd",
    "she'll",
    "she'd",
    "it'll",
    "it'd",
    "we're",
    "we've",
    "we'll",
    "we'd",
    "they're",
    "they've",
    "they'll",
    "they'd",
    "who're",
    "who've",
    "who'll",
    "who'd",
    "what're",
    "what've",
    "what'll",
    "what'd",
    "that'll",
    "that'd",
    "there'll",
    "there'd",
    "there're",
    "where'd",
    "how'd",
    "could've",
    "would've",
    "should've",
    "might've",
    "must've",
    "let's",
    "y'all",
    "ma'am",
    "o'clock",
];

impl Speller {
    /// The bundled US English dictionary (Moby word lists, public domain). The lists are parsed once per
    /// process; the suggestion index is built on the first [`Speller::suggest`].
    pub fn bundled_en_us() -> Self {
        let dict = EN_US.get_or_init(|| {
            let mut proper = String::with_capacity(EN_US_PROPER.len().saturating_add(64));
            proper.push_str(EN_US_PROPER);
            for w in EN_US_PROPER_ONLY {
                proper.push('\n');
                proper.push_str(&title_case(w));
            }
            let mut dict = Dictionary::new(Cow::Borrowed(EN_US_COMMON), Cow::Owned(proper), EN_US_FREQ);
            dict.common.remove_words(EN_US_NOT_WORDS.iter().chain(EN_US_PROPER_ONLY));
            Arc::new(dict)
        });
        Speller::with_dictionary(Arc::clone(dict))
    }

    /// A speller over custom word lists, one word per line: `common` (lower case, accepted in any case),
    /// `proper` (accepted Capitalised or in capitals) and `frequent` (most frequent first, used to rank
    /// suggestions; may be empty).
    pub fn from_word_lists(common: String, proper: String, frequent: &str) -> Self {
        let common = Cow::Owned(common.to_lowercase());
        Speller::with_dictionary(Arc::new(Dictionary::new(common, Cow::Owned(proper), frequent)))
    }

    fn with_dictionary(dict: Arc<Dictionary>) -> Self {
        Speller {
            dict,
            hun: None,
            user: BTreeSet::new(),
            ignored: HashSet::new(),
            options: SpellOptions::default(),
        }
    }

    /// A speller over a Hunspell-format dictionary (another language's, installed on the computer). Words are
    /// accepted as the dictionary spells them (with the same capitalisation rules), the English contraction
    /// and possessive rules do not apply, and [`Speller::check_text`] runs only the language-neutral checks.
    pub fn from_hunspell(dict: Arc<Hunspell>) -> Self {
        static EMPTY: OnceLock<Arc<Dictionary>> = OnceLock::new();
        let empty = EMPTY.get_or_init(|| Arc::new(Dictionary::new(Cow::Borrowed(""), Cow::Borrowed(""), "")));
        Speller { hun: Some(dict), ..Speller::with_dictionary(Arc::clone(empty)) }
    }

    /// The language of a Hunspell-format dictionary ("fr-FR"); `None` for the bundled English lists.
    pub fn language(&self) -> Option<&str> {
        self.hun.as_deref().map(|h| h.language.as_str())
    }

    /// Whether this speller checks English (the bundled lists or an English Hunspell dictionary), so the
    /// English grammar rules and writing refinements apply.
    pub fn is_english(&self) -> bool {
        self.language().is_none_or(|l| {
            let l = l.to_ascii_lowercase();
            l == "en" || l.starts_with("en-") || l.starts_with("en_")
        })
    }

    /// Number of words in the main list plus proper nouns (stems for a Hunspell-format dictionary).
    pub fn dictionary_len(&self) -> usize {
        let hun = self.hun.as_deref().map_or(0, Hunspell::len);
        self.dict.common.len().saturating_add(self.dict.proper.len()).saturating_add(hun)
    }

    pub fn options(&self) -> &SpellOptions {
        &self.options
    }

    pub fn options_mut(&mut self) -> &mut SpellOptions {
        &mut self.options
    }

    pub fn set_options(&mut self, options: SpellOptions) {
        self.options = options;
    }

    // ---- user dictionary and ignore list ----

    /// Adds a word to the user dictionary ("Add to Dictionary"). Lower-case entries follow the dictionary
    /// case rules; entries with capitals must be matched exactly (or in all capitals). Returns false when the
    /// word is empty, too long, contains whitespace, or the list is full.
    pub fn add_word(&mut self, word: &str) -> bool {
        let word = normalize_apostrophes(word.trim());
        if !valid_entry(&word) || self.user.len() >= MAX_USER_WORDS {
            return false;
        }
        self.user.insert(word.into_owned());
        true
    }

    /// Removes a word from the user dictionary; true if it was there.
    pub fn remove_word(&mut self, word: &str) -> bool {
        self.user.remove(normalize_apostrophes(word.trim()).as_ref())
    }

    /// The user dictionary, sorted, for the engine to persist.
    pub fn user_words(&self) -> impl Iterator<Item = &str> {
        self.user.iter().map(String::as_str)
    }

    /// Replaces the user dictionary (e.g. loaded from the profile). Invalid entries are skipped.
    pub fn set_user_words<I, S>(&mut self, words: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.user.clear();
        for w in words {
            self.add_word(w.as_ref());
        }
    }

    /// Ignores a word for this checking session ("Ignore All"): it is accepted in any case.
    pub fn ignore(&mut self, word: &str) -> bool {
        let word = normalize_apostrophes(word.trim()).to_lowercase();
        if !valid_entry(&word) || self.ignored.len() >= MAX_USER_WORDS {
            return false;
        }
        self.ignored.insert(word);
        true
    }

    /// The ignore list (lower case, unordered), for the engine to persist with the draft if it wants to.
    pub fn ignored_words(&self) -> impl Iterator<Item = &str> {
        self.ignored.iter().map(String::as_str)
    }

    pub fn clear_ignored(&mut self) {
        self.ignored.clear();
    }

    // ---- checking ----

    /// Whether `word` is spelled correctly (or is skipped under the current options). Accepts a token from
    /// [`crate::tokenize`]; surrounding whitespace and quotes are ignored. See [`Speller`] for the case rules.
    pub fn check(&self, word: &str) -> bool {
        let word = word.trim().trim_matches(|c: char| matches!(c, '"' | '\u{201C}' | '\u{201D}'));
        let word = normalize_apostrophes(word);
        let word = word.trim_matches('\'');
        if word.is_empty() || !word.chars().any(char::is_alphabetic) {
            return true;
        }
        if word.chars().count() > MAX_WORD_CHARS {
            return true;
        }
        if self.options.ignore_internet_and_file_addresses && looks_like_address(word) {
            return true;
        }
        if self.options.ignore_words_with_numbers && word.chars().any(|c| c.is_numeric()) {
            return true;
        }
        if self.is_ignored(word) || self.in_user(word) {
            return true;
        }
        // Hyphenated words and dotted abbreviations: every part must pass.
        if word.contains(['-', '\u{2010}', '\u{2011}', '.']) {
            return word
                .split(['-', '\u{2010}', '\u{2011}', '.'])
                .filter(|p| !p.is_empty())
                .all(|p| self.check_part(p));
        }
        self.check_part(word)
    }

    fn check_part(&self, word: &str) -> bool {
        if word.chars().filter(|c| c.is_alphabetic()).count() <= 1 && !word.contains('\'') {
            return true;
        }
        if self.options.ignore_words_with_numbers && word.chars().any(|c| c.is_numeric()) {
            return true;
        }
        let case = Case::of(word);
        if case == Case::Upper && self.options.ignore_uppercase {
            return true;
        }
        if self.is_ignored(word) || self.in_user(word) || self.known(word, case) {
            return true;
        }
        if word.contains('\'') {
            return self.check_apostrophe(word, case);
        }
        false
    }

    /// Dictionary lookup with the case rules.
    fn known(&self, word: &str, case: Case) -> bool {
        if let Some(h) = &self.hun {
            return match case {
                Case::Lower | Case::Mixed => h.check_exact(word),
                Case::Title | Case::Upper => {
                    let lower = word.to_lowercase();
                    h.check_exact(word) || h.check_exact(&lower) || h.check_exact(&title_case(&lower))
                }
            };
        }
        match case {
            Case::Lower => self.dict.common.contains(word),
            Case::Title | Case::Upper => {
                let lower = word.to_lowercase();
                self.dict.common.contains(&lower) || self.dict.proper.contains(&title_case(&lower))
            }
            Case::Mixed => self.dict.proper.contains(word),
        }
    }

    fn check_apostrophe(&self, word: &str, case: Case) -> bool {
        if self.hun.is_some() {
            // Other languages' elisions ("l'homme") are in their dictionaries; the English rules do not apply.
            return false;
        }
        let lower = word.to_lowercase();
        if CONTRACTIONS.contains(&lower.as_str()) {
            // "I'm", never "i'm"; other contractions follow the normal case rules.
            return case != Case::Mixed && (!lower.starts_with("i'") || word.starts_with('I'));
        }
        // Possessive: <accepted word>'s.
        if let Some(stem) = word.strip_suffix("'s").or_else(|| word.strip_suffix("'S")) {
            return !stem.is_empty() && self.check_part(stem);
        }
        false
    }

    fn is_ignored(&self, word: &str) -> bool {
        !self.ignored.is_empty() && self.ignored.contains(&word.to_lowercase())
    }

    fn in_user(&self, word: &str) -> bool {
        if self.user.is_empty() {
            return false;
        }
        if self.user.contains(word) {
            return true;
        }
        let lower = word.to_lowercase();
        match Case::of(word) {
            Case::Title | Case::Upper => {
                self.user.contains(&lower) || self.user.contains(&title_case(&lower)) || {
                    // An all-caps spelling of an entry with capitals (`MCDONALD` for `McDonald`).
                    Case::of(word) == Case::Upper && self.user.iter().any(|u| u.to_lowercase() == lower)
                }
            }
            _ => false,
        }
    }

    // ---- suggestions ----

    /// Up to `n` replacement suggestions for `word`, best first: words within two edits (insertions,
    /// deletions, substitutions, swaps of neighbours), ranked by a weighted distance that makes keyboard
    /// slips cheap, then by word frequency. Suggestions follow the case of `word` (`Teh` → `The`,
    /// `TEH` → `THE`); proper nouns keep their capital. User-dictionary words are suggested too, unless
    /// [`SpellOptions::main_dictionary_only`] is set.
    pub fn suggest(&self, word: &str, n: usize) -> Vec<String> {
        let word = normalize_apostrophes(word.trim());
        let word = word.trim_matches('\'');
        let n = n.min(64);
        if n == 0 || word.is_empty() || word.chars().count() > MAX_WORD_CHARS {
            return Vec::new();
        }
        let query = lower_chars(word);
        let case = Case::of(word);
        let mut ids: HashSet<u32> = HashSet::new();
        let index = self.dict.index();
        for h in variant_hashes(&query) {
            ids.extend(index.lookup(h));
        }

        let mut scored: Vec<(u32, u32, String)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut consider = |display: &str, proper: bool, scored: &mut Vec<(u32, u32, String)>| {
            let cand = lower_chars(display);
            if distance::osa_within(&query, &cand, 2).is_none() {
                return;
            }
            let lower: String = cand.iter().collect();
            let rank = self.dict.rank(&lower);
            let mut score = distance::weighted(&query, &cand);
            if let Some(r) = rank {
                score = score.saturating_sub(4 - (r / 250).min(3));
            }
            // A wrong first letter is a rare slip (swapping the first two letters is not).
            let swapped_start = query.len() > 1
                && cand.len() > 1
                && query.first() == cand.get(1)
                && query.get(1) == cand.first();
            if query.first() != cand.first() && !swapped_start {
                score = score.saturating_add(5);
            }
            // Lower-case typing rarely means a proper noun (the list is full of rare surnames).
            if proper && case == Case::Lower {
                score = score.saturating_add(6);
            }
            let out = apply_case(display, proper, case);
            if out != word && seen.insert(out.clone()) {
                scored.push((score, rank.unwrap_or(u32::MAX), out));
            }
        };
        for id in ids {
            let (display, proper) = self.dict.entry(id);
            consider(display, proper, &mut scored);
        }
        if let Some(h) = &self.hun {
            for cand in h.candidates(word) {
                let proper = cand.chars().next().is_some_and(char::is_uppercase);
                consider(&cand, proper, &mut scored);
            }
        }
        if !self.options.main_dictionary_only {
            for u in &self.user {
                consider(u, u.chars().any(char::is_uppercase), &mut scored);
            }
        }
        // Two words run together ("alot" → "a lot", "thankyou" → "thank you").
        let splits = if self.hun.is_some() { Vec::new() } else { self.splits(&query) };
        for (score, rank, split) in splits {
            let out = apply_case(&split, false, case);
            if seen.insert(out.clone()) {
                scored.push((score, rank.unwrap_or(u32::MAX), out));
            }
        }
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.cmp(&b.1))
                .then(
                    a.2.chars().count().abs_diff(query.len()).cmp(&b.2.chars().count().abs_diff(query.len())),
                )
                .then(a.2.cmp(&b.2))
        });
        scored.into_iter().take(n).map(|(_, _, w)| w).collect()
    }

    /// Ways to read `query` (lower case) as two words with a space left out, as `(score, rank, "left right")`.
    /// Each part must be "a", a frequent word, or a listed word of three letters or more, so rare short words
    /// do not turn every typo into a split ("helo" is not "he lo").
    fn splits(&self, query: &[char]) -> Vec<(u32, Option<u32>, String)> {
        let mut out = Vec::new();
        if query.len() < 3 || query.len() > MAX_WORD_CHARS {
            return out;
        }
        let part = |p: &[char]| -> Option<(String, Option<u32>)> {
            let w: String = p.iter().collect();
            let rank = if w == "a" { Some(0) } else { self.dict.rank(&w) };
            let ok = w == "a"
                || (p.len() >= 2 && rank.is_some())
                || (p.len() >= 3 && (self.dict.common.contains(&w) || self.user.contains(&w)));
            ok.then_some((w, rank))
        };
        for i in 1..query.len() {
            let (Some(l), Some(r)) = (query.get(..i), query.get(i..)) else {
                continue;
            };
            let (Some((lw, lr)), Some((rw, rr))) = (part(l), part(r)) else {
                continue;
            };
            // A left-out space between two frequent words costs a little less than a wrong letter; other
            // pairs ("sup rise", "vis able") only come after the one- and two-letter fixes.
            let rank = lr.zip(rr).map(|(a, b)| a.max(b));
            let score = match rank {
                Some(r) => COST_SPLIT.saturating_sub(4 - (r / 250).min(3)),
                None => COST_RARE_SPLIT,
            };
            out.push((score, rank, format!("{lw} {rw}")));
        }
        out
    }
}

/// Weighted cost (tenths of an edit) of a missing space between two frequent words ("a" counts as one).
const COST_SPLIT: u32 = 7;
/// Weighted cost of a missing space between other listed words.
const COST_RARE_SPLIT: u32 = 16;

/// Hashes to look up in the deletion index for `query`: the query and its single-edit variants (letters a–z
/// inserted or substituted, one letter deleted, neighbours swapped), each with its own one-letter deletions.
/// Every dictionary word within two edits shares one of these hashes with its own index entries. Long queries
/// (over 24 chars) only get the query's own variants (one edit plus deletions).
fn variant_hashes(query: &[char]) -> HashSet<u32> {
    let mut hashes = HashSet::new();
    let add_with_deletes = |v: &[char], hashes: &mut HashSet<u32>| {
        hashes.insert(hash_chars(v.iter().copied()));
        for skip in 0..v.len() {
            hashes.insert(hash_chars(v.iter().enumerate().filter(|&(i, _)| i != skip).map(|(_, &c)| c)));
        }
    };
    add_with_deletes(query, &mut hashes);
    let len = query.len();
    if len > 24 {
        return hashes;
    }
    let mut v: Vec<char> = Vec::with_capacity(len + 1);
    for i in 0..len {
        // deletion
        v.clear();
        v.extend(query.iter().enumerate().filter(|&(j, _)| j != i).map(|(_, &c)| c));
        add_with_deletes(&v, &mut hashes);
        // transposition
        if i + 1 < len {
            v.clear();
            v.extend_from_slice(query);
            v.swap(i, i + 1);
            add_with_deletes(&v, &mut hashes);
        }
    }
    for c in 'a'..='z' {
        for i in 0..=len {
            // insertion before position i
            v.clear();
            v.extend_from_slice(query.get(..i).unwrap_or(&[]));
            v.push(c);
            v.extend_from_slice(query.get(i..).unwrap_or(&[]));
            add_with_deletes(&v, &mut hashes);
            // substitution at i
            if i < len && query.get(i) != Some(&c) {
                v.clear();
                v.extend_from_slice(query);
                if let Some(slot) = v.get_mut(i) {
                    *slot = c;
                }
                add_with_deletes(&v, &mut hashes);
            }
        }
    }
    hashes
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Case {
    /// No upper-case letters.
    Lower,
    /// First letter upper case, the rest lower case.
    Title,
    /// Two or more letters, all upper case.
    Upper,
    Mixed,
}

impl Case {
    pub(crate) fn of(word: &str) -> Case {
        let mut letters = word.chars().filter(|c| c.is_alphabetic());
        let Some(first) = letters.next() else { return Case::Lower };
        let (mut upper, mut lower) = (0usize, 0usize);
        for c in letters {
            if c.is_uppercase() {
                upper += 1;
            } else if c.is_lowercase() {
                lower += 1;
            }
        }
        match (first.is_uppercase(), upper, lower) {
            (false, 0, _) => Case::Lower,
            (true, 0, _) if lower > 0 => Case::Title,
            (true, _, 0) if upper > 0 => Case::Upper,
            (true, 0, 0) => Case::Title,
            _ => Case::Mixed,
        }
    }
}

/// `hello` → `Hello`.
pub(crate) fn title_case(lower: &str) -> String {
    let mut chars = lower.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn apply_case(display: &str, proper: bool, case: Case) -> String {
    match case {
        Case::Upper => display.to_uppercase(),
        Case::Title if !proper => title_case(display),
        _ => display.to_owned(),
    }
}

pub(crate) fn normalize_apostrophes(s: &str) -> Cow<'_, str> {
    if s.contains(['\u{2019}', '\u{02BC}']) {
        Cow::Owned(s.replace(['\u{2019}', '\u{02BC}'], "'"))
    } else {
        Cow::Borrowed(s)
    }
}

fn valid_entry(w: &str) -> bool {
    !w.is_empty() && w.chars().count() <= MAX_WORD_CHARS && !w.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_classes() {
        assert_eq!(Case::of("hello"), Case::Lower);
        assert_eq!(Case::of("Hello"), Case::Title);
        assert_eq!(Case::of("HELLO"), Case::Upper);
        assert_eq!(Case::of("hELLO"), Case::Mixed);
        assert_eq!(Case::of("iPhone"), Case::Mixed);
        assert_eq!(Case::of("I"), Case::Title);
        assert_eq!(Case::of("123"), Case::Lower);
    }

    #[test]
    fn title_and_apply_case() {
        assert_eq!(title_case("ottawa"), "Ottawa");
        assert_eq!(apply_case("the", false, Case::Title), "The");
        assert_eq!(apply_case("Ottawa", true, Case::Upper), "OTTAWA");
        assert_eq!(apply_case("Ottawa", true, Case::Lower), "Ottawa");
    }
}
