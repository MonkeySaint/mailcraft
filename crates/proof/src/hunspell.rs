//! Hunspell-format dictionaries: a word list (`.dic`) plus affix rules (`.aff`), the format most open
//! spelling dictionaries are distributed in. MailCraft bundles only its English word lists; dictionaries for
//! other languages are read at run time from the ones installed on the computer ([`find_dictionaries`],
//! [`dictionary_dirs`]), so no third-party dictionary is shipped.
//!
//! Supported: `SET` (the files' character set), `FLAG` (single-character, `long`, `num` and `UTF-8` flags),
//! `AF` flag aliases, `PFX`/`SFX` rules with strip text, conditions (`.`, `[…]`, `[^…]`, literals) and
//! continuation flags, cross products of a prefix and a suffix, two-level suffixes, `NEEDAFFIX`
//! (`PSEUDOROOT`), `FORBIDDENWORD`, `ONLYINCOMPOUND`, `CIRCUMFIX`, `TRY` and `REP` (for suggestions). Compound
//! words (`COMPOUND…`) are not formed: a compound the list does not hold is reported. Morphological fields
//! are ignored.
//!
//! Words are checked by stripping affixes back to a listed stem, so nothing is expanded in memory. All input
//! is treated as hostile: file sizes, rule and word counts and word lengths are capped, and parsing never
//! panics (unknown or malformed lines are skipped). Checking and suggesting are bounded too: one check visits
//! at most [`MAX_CHECK_STEPS`] rules and stems, one suggestion run [`MAX_SUGGEST_STEPS`] (affix-heavy
//! dictionaries with thousands of rules would otherwise take seconds per word).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::distance::{self, MAX_LEN};

/// Largest `.dic` file read.
pub const MAX_DIC_BYTES: usize = 64 * 1024 * 1024;
/// Largest `.aff` file read.
pub const MAX_AFF_BYTES: usize = 16 * 1024 * 1024;
/// Most stems kept from one `.dic`.
pub const MAX_STEMS: usize = 3_000_000;
/// Most affix rules kept from one `.aff`.
pub const MAX_RULES: usize = 500_000;
/// Most flags read for one word or rule.
const MAX_FLAGS: usize = 512;
/// Most `REP` pairs kept.
const MAX_REP: usize = 2_000;
/// Longest condition (in character positions).
const MAX_COND: usize = 64;
/// Most affix rules and stems one check of a word visits (past it the word counts as not found).
pub const MAX_CHECK_STEPS: u64 = 10_000;
/// Most steps one [`Hunspell::candidates`] run takes over all its checks.
pub const MAX_SUGGEST_STEPS: u64 = 400_000;
/// Dictionaries with more stems than this skip the scan for listed stems within two edits.
const MAX_SCAN_STEMS: usize = 300_000;

/// Work left for a check or a suggestion run (each affix rule or stem visited is one step).
struct Budget(std::cell::Cell<u64>);

impl Budget {
    fn new(steps: u64) -> Budget {
        Budget(std::cell::Cell::new(steps))
    }

    /// Takes one step; false when none is left.
    fn spend(&self) -> bool {
        let left = self.0.get();
        if left == 0 {
            return false;
        }
        self.0.set(left - 1);
        true
    }

    fn left(&self) -> u64 {
        self.0.get()
    }

    fn take(&self, steps: u64) {
        self.0.set(self.0.get().saturating_sub(steps));
    }
}

type Flag = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlagMode {
    /// One character per flag (the default).
    Char,
    /// Two characters per flag.
    Long,
    /// Decimal numbers separated by commas.
    Num,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CondPart {
    Any,
    Char(char),
    Set(Vec<char>, bool),
}

impl CondPart {
    fn matches(&self, c: char) -> bool {
        match self {
            CondPart::Any => true,
            CondPart::Char(x) => *x == c,
            CondPart::Set(set, negated) => set.contains(&c) != *negated,
        }
    }
}

/// An affix condition: one pattern per character position.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Cond(Vec<CondPart>);

impl Cond {
    fn parse(s: &str) -> Cond {
        if s == "." {
            return Cond::default();
        }
        let mut parts = Vec::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if parts.len() >= MAX_COND {
                break;
            }
            match c {
                '.' => parts.push(CondPart::Any),
                '[' => {
                    let mut set = Vec::new();
                    let mut negated = false;
                    let mut first = true;
                    for c in chars.by_ref() {
                        if c == ']' {
                            break;
                        }
                        if first && c == '^' {
                            negated = true;
                        } else if set.len() < 256 {
                            set.push(c);
                        }
                        first = false;
                    }
                    parts.push(CondPart::Set(set, negated));
                }
                c => parts.push(CondPart::Char(c)),
            }
        }
        Cond(parts)
    }

    /// Whether the condition matches the end of `word`.
    fn matches_end(&self, word: &[char]) -> bool {
        let Some(start) = word.len().checked_sub(self.0.len()) else { return false };
        word.get(start..).is_some_and(|tail| self.0.iter().zip(tail).all(|(p, c)| p.matches(*c)))
    }

    /// Whether the condition matches the start of `word`.
    fn matches_start(&self, word: &[char]) -> bool {
        word.len() >= self.0.len() && self.0.iter().zip(word).all(|(p, c)| p.matches(*c))
    }
}

/// One `PFX` or `SFX` rule.
#[derive(Clone, Debug)]
struct Affix {
    flag: Flag,
    cross: bool,
    strip: Vec<char>,
    add: Vec<char>,
    cond: Cond,
    /// Continuation flags (sorted).
    cont: Vec<Flag>,
}

impl Affix {
    fn has(&self, f: Option<Flag>) -> bool {
        f.is_some_and(|f| self.cont.binary_search(&f).is_ok())
    }
}

/// A Hunspell-format dictionary (see the module docs).
#[derive(Debug, Default)]
pub struct Hunspell {
    /// Language tag of the dictionary ("fr-FR"), when known.
    pub language: String,
    /// Stem → indices into `flag_sets` (one per homonym).
    words: HashMap<Box<str>, Box<[u32]>>,
    flag_sets: Vec<Box<[Flag]>>,
    prefixes: Vec<Affix>,
    suffixes: Vec<Affix>,
    /// Added text → rule indices.
    pfx_by_add: HashMap<Box<[char]>, Vec<u32>>,
    sfx_by_add: HashMap<Box<[char]>, Vec<u32>>,
    /// Continuation flag → suffix rules that allow it (the inner suffix of a two-level suffix).
    sfx_by_cont: HashMap<Flag, Vec<u32>>,
    max_pfx: usize,
    max_sfx: usize,
    need_affix: Option<Flag>,
    forbidden: Option<Flag>,
    only_in_compound: Option<Flag>,
    circumfix: Option<Flag>,
    /// Letters tried for suggestions, most frequent first.
    try_chars: Vec<char>,
    rep: Vec<(String, String)>,
}

/// Why a dictionary could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunspellError {
    TooBig,
    Unreadable(String),
    NoWords,
}

impl std::fmt::Display for HunspellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HunspellError::TooBig => f.write_str("the dictionary is too large"),
            HunspellError::Unreadable(e) => write!(f, "the dictionary cannot be read: {e}"),
            HunspellError::NoWords => f.write_str("the dictionary has no words"),
        }
    }
}

impl std::error::Error for HunspellError {}

/// The character set an `.aff` file names on its `SET` line (UTF-8 when there is none or it is unknown).
fn encoding_of(aff: &[u8]) -> &'static encoding_rs::Encoding {
    for line in aff.split(|b| *b == b'\n').take(2_000) {
        let line = String::from_utf8_lossy(line);
        let mut words = line.split_whitespace();
        if words.next() == Some("SET") {
            let label = words.next().unwrap_or("").to_ascii_lowercase();
            let label = match label.as_str() {
                "microsoft-cp1251" => "windows-1251",
                "iscii-devanagari" | "tis620-2533" => "utf-8",
                other => other,
            };
            return encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
        }
    }
    encoding_rs::UTF_8
}

fn decode(bytes: &[u8], enc: &'static encoding_rs::Encoding) -> String {
    // A UTF-8 byte-order mark is dropped whatever the declared set.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(bytes);
    enc.decode_without_bom_handling(bytes).0.into_owned()
}

/// Parser state for flags.
struct Flags {
    mode: FlagMode,
    aliases: Vec<Vec<Flag>>,
}

impl Flags {
    fn parse(&self, s: &str) -> Vec<Flag> {
        let mut out: Vec<Flag> = match self.mode {
            FlagMode::Char => s.chars().map(u32::from).take(MAX_FLAGS).collect(),
            FlagMode::Long => {
                let chars: Vec<char> = s.chars().take(MAX_FLAGS * 2).collect();
                chars
                    .chunks(2)
                    .map(|p| match p {
                        [a, b] => u32::from(*a).saturating_mul(65_536).saturating_add(u32::from(*b)),
                        [a] => u32::from(*a).saturating_mul(65_536),
                        _ => 0,
                    })
                    .collect()
            }
            FlagMode::Num => {
                s.split(',').filter_map(|n| n.trim().parse::<u32>().ok()).take(MAX_FLAGS).collect()
            }
        };
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Flags of a word or rule: an alias number when the `.aff` has `AF` lines, else flags.
    fn parse_or_alias(&self, s: &str) -> Vec<Flag> {
        if !self.aliases.is_empty()
            && let Ok(n) = s.parse::<usize>()
        {
            return n.checked_sub(1).and_then(|i| self.aliases.get(i)).cloned().unwrap_or_default();
        }
        self.parse(s)
    }

    fn one(&self, s: &str) -> Option<Flag> {
        self.parse(s).first().copied()
    }
}

/// `"0"` is the empty string in affix fields.
fn field_chars(s: &str) -> Vec<char> {
    if s == "0" { Vec::new() } else { s.chars().collect() }
}

/// Splits `word/flags` at the first unescaped slash.
fn split_flags(s: &str) -> (String, Option<&str>) {
    let mut word = String::new();
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' if chars.peek().is_some_and(|(_, n)| *n == '/') => {
                word.push('/');
                chars.next();
            }
            '/' if !word.is_empty() => return (word, s.get(i + 1..)),
            c => word.push(c),
        }
    }
    (word, None)
}

impl Hunspell {
    /// Reads a dictionary from the bytes of its `.aff` and `.dic` files.
    pub fn parse(aff: &[u8], dic: &[u8]) -> Result<Hunspell, HunspellError> {
        if aff.len() > MAX_AFF_BYTES || dic.len() > MAX_DIC_BYTES {
            return Err(HunspellError::TooBig);
        }
        let enc = encoding_of(aff);
        let aff = decode(aff, enc);
        let dic = decode(dic, enc);
        let mut h = Hunspell::default();
        let mut flags = Flags { mode: FlagMode::Char, aliases: Vec::new() };
        // Affix headers announce how many rule lines follow; the rule lines carry the details.
        let mut cross: HashMap<(bool, Flag), bool> = HashMap::new();
        let mut af_header = false;
        for line in aff.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().take(8).collect();
            match f.as_slice() {
                ["FLAG", mode, ..] => {
                    flags.mode = match mode.to_ascii_lowercase().as_str() {
                        "long" => FlagMode::Long,
                        "num" => FlagMode::Num,
                        _ => FlagMode::Char,
                    }
                }
                ["AF", a, ..] => {
                    // The first "AF <count>" line is the header; each later "AF <flags>" is alias 1, 2, …
                    if !af_header {
                        af_header = true;
                    } else if flags.aliases.len() < 65_536 {
                        let set = flags.parse(a);
                        flags.aliases.push(set);
                    }
                }
                ["NEEDAFFIX" | "PSEUDOROOT", x, ..] => h.need_affix = flags.one(x),
                ["FORBIDDENWORD", x, ..] => h.forbidden = flags.one(x),
                ["ONLYINCOMPOUND", x, ..] => h.only_in_compound = flags.one(x),
                ["CIRCUMFIX", x, ..] => h.circumfix = flags.one(x),
                ["TRY", x, ..] => h.try_chars = x.chars().take(128).collect(),
                ["LANG", x, ..] => h.language = x.replace('_', "-").chars().take(35).collect(),
                ["REP", from, to, ..] if h.rep.len() < MAX_REP => {
                    h.rep.push((from.replace('_', " "), to.replace('_', " ")));
                }
                [kind @ ("PFX" | "SFX"), flag, c, n] if *c == "Y" || *c == "N" => {
                    // Header: PFX <flag> <cross product> <count>.
                    if n.parse::<usize>().is_ok()
                        && let Some(fl) = flags.one(flag)
                    {
                        cross.insert((*kind == "PFX", fl), *c == "Y");
                    }
                }
                [kind @ ("PFX" | "SFX"), flag, strip, add, rest @ ..] => {
                    let is_pfx = *kind == "PFX";
                    let Some(fl) = flags.one(flag) else { continue };
                    if h.prefixes.len().saturating_add(h.suffixes.len()) >= MAX_RULES {
                        continue;
                    }
                    let (add, cont) = split_flags(add);
                    let cont = cont.map(|c| flags.parse_or_alias(c)).unwrap_or_default();
                    let cond = Cond::parse(rest.first().copied().unwrap_or("."));
                    let rule = Affix {
                        flag: fl,
                        cross: cross.get(&(is_pfx, fl)).copied().unwrap_or(false),
                        strip: field_chars(strip),
                        add: field_chars(&add),
                        cond,
                        cont,
                    };
                    if rule.add.len() > MAX_LEN || rule.strip.len() > MAX_LEN {
                        continue;
                    }
                    if is_pfx {
                        h.prefixes.push(rule);
                    } else {
                        h.suffixes.push(rule);
                    }
                }
                _ => {}
            }
        }
        for (i, r) in h.prefixes.iter().enumerate() {
            let Ok(i) = u32::try_from(i) else { break };
            h.max_pfx = h.max_pfx.max(r.add.len());
            h.pfx_by_add.entry(r.add.clone().into_boxed_slice()).or_default().push(i);
        }
        for (i, r) in h.suffixes.iter().enumerate() {
            let Ok(i) = u32::try_from(i) else { break };
            h.max_sfx = h.max_sfx.max(r.add.len());
            h.sfx_by_add.entry(r.add.clone().into_boxed_slice()).or_default().push(i);
            for f in &r.cont {
                h.sfx_by_cont.entry(*f).or_default().push(i);
            }
        }
        let mut interned: HashMap<Vec<Flag>, u32> = HashMap::new();
        let mut letters: HashMap<char, usize> = HashMap::new();
        for (n, line) in dic.lines().enumerate() {
            if h.words.len() >= MAX_STEMS {
                break;
            }
            // The first line is the (approximate) word count.
            if n == 0 && line.trim().chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let entry = line.split(['\t', ' ']).next().unwrap_or("").trim();
            if entry.is_empty() || entry.starts_with('#') {
                continue;
            }
            let (word, fl) = split_flags(entry);
            if word.is_empty() || word.chars().count() > MAX_LEN {
                continue;
            }
            let set = fl.map(|f| flags.parse_or_alias(f)).unwrap_or_default();
            let next = u32::try_from(h.flag_sets.len()).unwrap_or(u32::MAX);
            let id = *interned.entry(set.clone()).or_insert_with(|| {
                h.flag_sets.push(set.into_boxed_slice());
                next
            });
            if h.try_chars.is_empty() {
                for c in word.chars().flat_map(char::to_lowercase).filter(|c| c.is_alphabetic()) {
                    *letters.entry(c).or_default() += 1;
                }
            }
            let slot = h.words.entry(word.into_boxed_str()).or_default();
            if !slot.contains(&id) && slot.len() < 16 {
                let mut v = slot.to_vec();
                v.push(id);
                *slot = v.into_boxed_slice();
            }
        }
        if h.words.is_empty() {
            return Err(HunspellError::NoWords);
        }
        if h.try_chars.is_empty() {
            let mut by_freq: Vec<(char, usize)> = letters.into_iter().collect();
            by_freq.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            h.try_chars = by_freq.into_iter().take(64).map(|(c, _)| c).collect();
        }
        Ok(h)
    }

    /// Reads a dictionary from its files.
    pub fn load(aff: &Path, dic: &Path) -> Result<Hunspell, HunspellError> {
        let read = |p: &Path, max: usize| -> Result<Vec<u8>, HunspellError> {
            let len = std::fs::metadata(p).map_err(|e| HunspellError::Unreadable(e.to_string()))?.len();
            if usize::try_from(len).map_or(true, |l| l > max) {
                return Err(HunspellError::TooBig);
            }
            std::fs::read(p).map_err(|e| HunspellError::Unreadable(e.to_string()))
        };
        let mut h = Hunspell::parse(&read(aff, MAX_AFF_BYTES)?, &read(dic, MAX_DIC_BYTES)?)?;
        if h.language.is_empty()
            && let Some(stem) = dic.file_stem().and_then(|s| s.to_str())
        {
            h.language = stem.replace('_', "-");
        }
        Ok(h)
    }

    /// Number of stems in the word list.
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// The flag sets of `stem`'s homonyms.
    fn homonyms(&self, stem: &str) -> impl Iterator<Item = &[Flag]> {
        self.words
            .get(stem)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|i| self.flag_sets.get(*i as usize).map(|b| &**b))
    }

    fn has(flags: &[Flag], f: Option<Flag>) -> bool {
        f.is_some_and(|f| flags.binary_search(&f).is_ok())
    }

    /// A homonym usable as a stem (not forbidden, not compound-only).
    fn usable(&self, flags: &[Flag]) -> bool {
        !Hunspell::has(flags, self.forbidden) && !Hunspell::has(flags, self.only_in_compound)
    }

    /// Whether `word` is a word of the dictionary exactly as written (case included): listed, or a listed
    /// stem with its affixes.
    pub fn check_exact(&self, word: &str) -> bool {
        self.check_within(word, &Budget::new(MAX_CHECK_STEPS))
    }

    /// [`Hunspell::check_exact`] within `budget` (false once it runs out).
    fn check_within(&self, word: &str, budget: &Budget) -> bool {
        let chars: Vec<char> = word.chars().collect();
        if chars.is_empty() || chars.len() > MAX_LEN {
            return false;
        }
        let mut forbidden = false;
        for flags in self.homonyms(word) {
            if Hunspell::has(flags, self.forbidden) {
                forbidden = true;
            } else if self.usable(flags) && !Hunspell::has(flags, self.need_affix) {
                return true;
            }
        }
        if forbidden {
            return false;
        }
        self.suffixed(&chars, None, budget) || self.prefixed(&chars, budget)
    }

    /// Rules in `index` whose added text ends (`end`) or starts `word`, longest first.
    fn matching<'a>(
        index: &'a HashMap<Box<[char]>, Vec<u32>>,
        max: usize,
        word: &'a [char],
        end: bool,
    ) -> impl Iterator<Item = u32> + 'a {
        (0..=max.min(word.len())).rev().flat_map(move |n| {
            let part = if end { word.get(word.len() - n..) } else { word.get(..n) };
            part.and_then(|p| index.get(p)).into_iter().flatten().copied()
        })
    }

    /// `word` as a stem with a suffix. With `prefix`, the stem must also take that prefix (cross product).
    fn suffixed(&self, word: &[char], prefix: Option<&Affix>, budget: &Budget) -> bool {
        for i in Hunspell::matching(&self.sfx_by_add, self.max_sfx, word, true) {
            if !budget.spend() {
                return false;
            }
            let Some(s) = self.suffixes.get(i as usize) else { continue };
            if prefix.is_some() && !s.cross {
                continue;
            }
            // A circumfix needs both halves; a suffix that needs another affix cannot end a word alone.
            let circ_s = s.has(self.circumfix);
            let circ_p = prefix.is_some_and(|p| p.has(self.circumfix));
            if circ_s != circ_p || (prefix.is_none() && s.has(self.need_affix)) {
                continue;
            }
            let Some(root) = strip_end(word, s) else { continue };
            let root_s: String = root.iter().collect();
            let pfx_ok = |flags: &[Flag]| match prefix {
                None => true,
                Some(p) => Hunspell::has(flags, Some(p.flag)) || s.has(Some(p.flag)),
            };
            if self.homonyms(&root_s).any(|f| self.usable(f) && Hunspell::has(f, Some(s.flag)) && pfx_ok(f)) {
                return true;
            }
            // Two suffixes: the root takes an inner suffix whose continuation allows this one.
            if prefix.is_none() && self.twofold(&root, s.flag, budget) {
                return true;
            }
        }
        false
    }

    /// `word` as a stem plus an inner suffix whose continuation flags include `outer`.
    fn twofold(&self, word: &[char], outer: Flag, budget: &Budget) -> bool {
        // Only the rules that allow `outer` after them can be the inner suffix.
        let Some(inner) = self.sfx_by_cont.get(&outer) else { return false };
        for &i in inner {
            if !budget.spend() {
                return false;
            }
            let Some(s) = self.suffixes.get(i as usize) else { continue };
            if !word.ends_with(&s.add) {
                continue;
            }
            let Some(root) = strip_end(word, s) else { continue };
            let root_s: String = root.iter().collect();
            if self.homonyms(&root_s).any(|f| self.usable(f) && Hunspell::has(f, Some(s.flag))) {
                return true;
            }
        }
        false
    }

    /// `word` as a stem with a prefix (and possibly a suffix as well).
    fn prefixed(&self, word: &[char], budget: &Budget) -> bool {
        for i in Hunspell::matching(&self.pfx_by_add, self.max_pfx, word, false) {
            if !budget.spend() {
                return false;
            }
            let Some(p) = self.prefixes.get(i as usize) else { continue };
            let Some(rest) = word.get(p.add.len()..) else { continue };
            let mut root: Vec<char> = p.strip.clone();
            root.extend_from_slice(rest);
            if root.is_empty() || !p.cond.matches_start(&root) {
                continue;
            }
            if !p.has(self.circumfix) && !p.has(self.need_affix) {
                let root_s: String = root.iter().collect();
                if self.homonyms(&root_s).any(|f| self.usable(f) && Hunspell::has(f, Some(p.flag))) {
                    return true;
                }
            }
            if p.cross && self.suffixed(&root, Some(p), budget) {
                return true;
            }
        }
        false
    }

    /// Candidate corrections for `word` (as typed): the `REP` replacements, words one edit away (letters from
    /// `TRY`), two words run together, and listed stems within two edits. Unordered; the speller ranks them.
    pub fn candidates(&self, word: &str) -> Vec<String> {
        self.candidates_counted(word).0
    }

    /// [`Hunspell::candidates`] and the steps they took (at most [`MAX_SUGGEST_STEPS`]).
    fn candidates_counted(&self, word: &str) -> (Vec<String>, u64) {
        let chars: Vec<char> = word.chars().collect();
        let mut out: HashSet<String> = HashSet::new();
        if chars.is_empty() || chars.len() > MAX_LEN {
            return (Vec::new(), 0);
        }
        let total = Budget::new(MAX_SUGGEST_STEPS);
        // One check within what is left of the run's budget.
        let check = |w: &str| {
            let one = Budget::new(MAX_CHECK_STEPS.min(total.left()));
            let start = one.left();
            let found = start > 0 && self.check_within(w, &one);
            total.take(start - one.left());
            found
        };
        let ok = |w: &str| check(w) || check(&crate::speller::title_case(w));
        let add = |w: String, out: &mut HashSet<String>| {
            if w != word && out.len() < 256 {
                let w = if w.contains(' ') || check(&w) { w } else { crate::speller::title_case(&w) };
                out.insert(w);
            }
        };
        for (from, to) in &self.rep {
            if from.is_empty() {
                continue;
            }
            for (i, _) in word.match_indices(from.as_str()).take(8) {
                let mut cand = String::with_capacity(word.len() + to.len());
                cand.push_str(word.get(..i).unwrap_or(""));
                cand.push_str(to);
                cand.push_str(word.get(i + from.len()..).unwrap_or(""));
                if cand.split(' ').all(|p| !p.is_empty() && ok(p)) {
                    add(cand, &mut out);
                }
            }
        }
        let lower: Vec<char> = chars.iter().flat_map(|c| c.to_lowercase()).collect();
        let lower_s: String = lower.iter().collect();
        if lower_s != word && ok(&lower_s) {
            add(lower_s.clone(), &mut out);
        }
        let mut v: Vec<char> = Vec::with_capacity(lower.len() + 1);
        let try_one = |v: &[char], out: &mut HashSet<String>| {
            let s: String = v.iter().collect();
            if ok(&s) {
                add(s, out);
            }
        };
        for i in 0..lower.len() {
            v.clear();
            v.extend(lower.iter().enumerate().filter(|&(j, _)| j != i).map(|(_, c)| *c));
            try_one(&v, &mut out);
            if i + 1 < lower.len() {
                v.clear();
                v.extend_from_slice(&lower);
                v.swap(i, i + 1);
                try_one(&v, &mut out);
            }
        }
        for &c in &self.try_chars {
            if total.left() == 0 {
                break;
            }
            for i in 0..=lower.len() {
                v.clear();
                v.extend_from_slice(lower.get(..i).unwrap_or(&[]));
                v.push(c);
                v.extend_from_slice(lower.get(i..).unwrap_or(&[]));
                try_one(&v, &mut out);
                if i < lower.len() && lower.get(i) != Some(&c) {
                    v.clear();
                    v.extend_from_slice(&lower);
                    if let Some(slot) = v.get_mut(i) {
                        *slot = c;
                    }
                    try_one(&v, &mut out);
                }
            }
        }
        // Two words run together.
        for i in 2..lower.len().saturating_sub(1) {
            let (Some(l), Some(r)) = (lower.get(..i), lower.get(i..)) else { continue };
            let (l, r): (String, String) = (l.iter().collect(), r.iter().collect());
            if r.chars().count() >= 2 && ok(&l) && ok(&r) {
                add(format!("{l} {r}"), &mut out);
            }
        }
        // Few close forms: listed stems within two edits (not in very large lists: too slow).
        if out.len() < 5 && self.words.len() <= MAX_SCAN_STEMS {
            for stem in self.words.keys() {
                if !total.spend() {
                    break;
                }
                let n = stem.chars().count();
                if n.abs_diff(lower.len()) > 2 {
                    continue;
                }
                let cand: Vec<char> = stem.chars().flat_map(char::to_lowercase).collect();
                if distance::osa_within(&lower, &cand, 2).is_some() && check(stem) {
                    out.insert(stem.to_string());
                    if out.len() >= 64 {
                        break;
                    }
                }
            }
        }
        (out.into_iter().collect(), MAX_SUGGEST_STEPS - total.left())
    }
}

/// The root of `word` before suffix `s` (strip text restored), when the suffix and its condition fit.
fn strip_end(word: &[char], s: &Affix) -> Option<Vec<char>> {
    let keep = word.len().checked_sub(s.add.len())?;
    let mut root: Vec<char> = word.get(..keep)?.to_vec();
    root.extend_from_slice(&s.strip);
    (!root.is_empty() && s.cond.matches_end(&root)).then_some(root)
}

/// One installed dictionary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictionaryFile {
    /// Language tag from the file name ("fr-FR" for `fr_FR.dic`).
    pub tag: String,
    pub aff: PathBuf,
    pub dic: PathBuf,
}

/// Folders where the platform keeps Hunspell-format dictionaries.
pub fn dictionary_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/Library/Spelling"));
        if let Some(h) = &home {
            dirs.push(h.join("Library").join("Spelling"));
        }
        dirs.push(PathBuf::from("/opt/homebrew/share/hunspell"));
        dirs.push(PathBuf::from("/usr/local/share/hunspell"));
    } else if cfg!(target_os = "windows") {
        if let Some(a) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(a).join("MailCraft").join("dictionaries"));
        }
    } else {
        for d in [
            "/usr/share/hunspell",
            "/usr/share/myspell",
            "/usr/share/myspell/dicts",
            "/usr/local/share/hunspell",
            "/usr/local/share/myspell",
            "/app/share/hunspell",
        ] {
            dirs.push(PathBuf::from(d));
        }
        if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
            dirs.push(PathBuf::from(d).join("hunspell"));
        }
        if let Some(h) = &home {
            dirs.push(h.join(".local").join("share").join("hunspell"));
        }
    }
    dirs
}

/// Most dictionaries listed from one folder.
const MAX_DICTIONARIES: usize = 1_000;

/// The dictionaries in `dirs` (each `.dic` with an `.aff` beside it; the first folder wins for a tag),
/// sorted by tag.
pub fn find_dictionaries(dirs: &[PathBuf]) -> Vec<DictionaryFile> {
    let mut out: Vec<DictionaryFile> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten().take(MAX_DICTIONARIES * 4) {
            let path = e.path();
            let is_dic =
                path.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("dic"));
            if !is_dic || !path.is_file() {
                continue;
            }
            let aff = path.with_extension("aff");
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            // Hyphenation and thesaurus files share the folder ("hyph_fr.dic", "th_fr_v2.dat").
            if !aff.is_file() || stem.starts_with("hyph_") || stem.starts_with("th_") {
                continue;
            }
            let tag = stem.replace('_', "-");
            if tag.is_empty() || tag.len() > 35 || out.iter().any(|d| d.tag.eq_ignore_ascii_case(&tag)) {
                continue;
            }
            out.push(DictionaryFile { tag, aff, dic: path });
        }
    }
    out.sort_by_key(|d| d.tag.to_ascii_lowercase());
    out
}

/// The dictionary for language `tag` among `found`: the same tag, else one for the same language (`fr-CA`
/// uses `fr-FR` when there is no Canadian one; `fr` takes the first French one).
pub fn dictionary_for<'a>(found: &'a [DictionaryFile], tag: &str) -> Option<&'a DictionaryFile> {
    let tag = tag.trim();
    if let Some(d) = found.iter().find(|d| d.tag.eq_ignore_ascii_case(tag)) {
        return Some(d);
    }
    let lang = tag.split('-').next().unwrap_or("").to_ascii_lowercase();
    if lang.is_empty() {
        return None;
    }
    let same = |d: &&DictionaryFile| d.tag.split('-').next().is_some_and(|l| l.eq_ignore_ascii_case(&lang));
    // Prefer the main variety ("fr-FR", "de-DE") over regional ones.
    let main = format!("{lang}-{}", lang.to_ascii_uppercase());
    found.iter().find(|d| d.tag.eq_ignore_ascii_case(&main)).or_else(|| found.iter().find(same))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Speller;
    use proptest::prelude::*;
    use std::sync::Arc;

    /// A small synthetic French-like dictionary exercising the supported features.
    const AFF: &str = "SET UTF-8\nLANG fr_FR\nTRY esianrtolcdugmphbyfvkwz\nNEEDAFFIX !\nFORBIDDENWORD *\n\
        REP 1\nREP f ph\n\
        PFX R Y 1\nPFX R 0 re .\n\
        SFX S Y 2\nSFX S 0 s [^sxz]\nSFX S 0 x [au]\n\
        SFX E Y 1\nSFX E er ent/S er\n\
        SFX M N 1\nSFX M e ement/S [^aeiou]e\n";
    const DIC: &str =
        "8\nchat/S\nmange/RS\nparler/E\nfleur/!S\nchats/*\nbateau/S\nParis\nrapide/M\ntéléphone/S\n";

    fn dict() -> Hunspell {
        Hunspell::parse(AFF.as_bytes(), DIC.as_bytes()).unwrap()
    }

    #[test]
    fn stems_and_affixes() {
        let h = dict();
        assert_eq!(h.language, "fr-FR");
        assert_eq!(h.len(), 9);
        for w in ["chat", "mange", "manges", "remange", "remanges", "parler", "parlent", "bateaux", "Paris"] {
            assert!(h.check_exact(w), "{w}");
        }
        // Strip and condition: "rapide" → "rapidement"; a two-level suffix adds "s".
        assert!(h.check_exact("rapidement") && h.check_exact("rapidements"));
        assert!(h.check_exact("parlents"), "the inner suffix's continuation allows S");
        for w in ["chats", "fleur", "paris", "mangez", "rechat", "parlerent", "", "x"] {
            assert!(!h.check_exact(w), "{w}");
        }
        // NEEDAFFIX: the bare stem is not a word, its forms are.
        assert!(h.check_exact("fleurs"));
    }

    #[test]
    fn speller_over_hunspell() {
        let sp = Speller::from_hunspell(Arc::new(dict()));
        assert!(!sp.is_english());
        assert_eq!(sp.language(), Some("fr-FR"));
        assert!(sp.check("manges") && sp.check("Manges") && sp.check("MANGES") && sp.check("PARIS"));
        assert!(!sp.check("paris") && !sp.check("mangez") && !sp.check("chats"));
        assert!(sp.check("chat-bateau") && !sp.check("re-mange"), "hyphenated parts are checked one by one");
        assert!(!sp.check("chat's"), "the English possessive rule is off");
        assert!(sp.suggest("mnage", 5).contains(&"mange".to_owned()), "{:?}", sp.suggest("mnage", 5));
        assert!(sp.suggest("Mangse", 5).contains(&"Manges".to_owned()));
        assert!(sp.suggest("télé", 5).is_empty() || !sp.suggest("télé", 5).contains(&"télé".to_owned()));
        assert!(sp.suggest("téléfone", 5).contains(&"téléphone".to_owned()), "REP f → ph");
        assert!(sp.suggest("chatmange", 5).contains(&"chat mange".to_owned()));
        assert!(sp.suggest("pariss", 5).contains(&"Paris".to_owned()));
        // English grammar rules do not run on another language; spelling and repeated words do.
        let issues = sp.check_text("Mange their is mangez chat chat.");
        let kinds: Vec<_> = issues.iter().map(|i| i.kind.id()).collect();
        assert!(kinds.contains(&"repeated") && kinds.contains(&"spelling"), "{kinds:?}");
        assert!(!kinds.iter().any(|k| *k == "confused" || *k == "agreement"), "{kinds:?}");
        assert!(Speller::bundled_en_us().is_english());
    }

    #[test]
    fn flag_modes_aliases_and_encodings() {
        let h = Hunspell::parse(
            b"FLAG long\nAF 2\nAF ZzYy\nAF Yy\nSFX Zz Y 1\nSFX Zz 0 s .\n",
            b"2\nmot/1\nnom/2\n",
        )
        .unwrap();
        assert!(h.check_exact("mots") && h.check_exact("mot") && !h.check_exact("noms"));
        let h = Hunspell::parse(b"FLAG num\nSFX 101 Y 1\nSFX 101 0 s .\n", b"1\nmot/101,7\n").unwrap();
        assert!(h.check_exact("mots"));
        let h = Hunspell::parse(b"SET ISO8859-1\nSFX S Y 1\nSFX S 0 s .\n", b"1\ncaf\xe9/S\n").unwrap();
        assert!(h.check_exact("caf\u{e9}s"));
        let h = Hunspell::parse(
            b"FLAG UTF-8\nSFX \xc3\xa9 Y 1\nSFX \xc3\xa9 0 s .\n",
            "1\nmot/\u{e9}\n".as_bytes(),
        )
        .unwrap();
        assert!(h.check_exact("mots"));
        // Escaped slashes are part of the word.
        let h = Hunspell::parse(b"", b"1\nkm\\/h\n").unwrap();
        assert!(h.check_exact("km/h"));
        assert_eq!(Hunspell::parse(b"", b"0\n").unwrap_err(), HunspellError::NoWords);
    }

    /// Regression: an affix-heavy dictionary (thousands of suffix rules that add nothing, each allowing the
    /// others after it) cannot make one check or one suggestion run unbounded.
    #[test]
    fn affix_heavy_dictionaries_have_bounded_work() {
        let rules = 3_000;
        let mut aff = String::from("SET UTF-8\nFLAG num\nTRY uhosegardnwi\n");
        let all: Vec<String> = (1..=rules).map(|f| f.to_string()).collect();
        // Each rule allows the first 512 (the most flags read for one rule) after it.
        let cont = all[..512].join(",");
        for f in 1..=rules {
            aff.push_str(&format!("SFX {f} Y 1\nSFX {f} 0 0/{cont} .\n"));
        }
        let mut dic = String::from("3\n");
        for w in ["house", "garden", "window"] {
            dic.push_str(&format!("{w}/{cont}\n"));
        }
        let h = Hunspell::parse(aff.as_bytes(), dic.as_bytes()).unwrap();
        assert!(h.check_exact("house"));
        let started = std::time::Instant::now();
        assert!(!h.check_exact("hoose"), "a word not in the list");
        let (found, steps) = h.candidates_counted("hoose");
        assert!(steps <= MAX_SUGGEST_STEPS, "{steps}");
        assert!(found.iter().any(|w| w == "house"), "{found:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(30), "{:?}", started.elapsed());
        // One check never takes more than its own budget either.
        let budget = Budget::new(MAX_CHECK_STEPS);
        let _ = h.check_within("windowz", &budget);
        assert!(MAX_CHECK_STEPS - budget.left() <= MAX_CHECK_STEPS);
    }

    #[test]
    fn finds_installed_dictionaries() {
        let dir = std::env::temp_dir().join(format!("mailcraft-hunspell-{}", std::process::id()));
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(dir.join("fr_FR.aff"), AFF).unwrap();
        std::fs::write(dir.join("fr_FR.dic"), DIC).unwrap();
        std::fs::write(dir.join("de_DE.dic"), "1\nHaus\n").unwrap();
        std::fs::write(dir.join("hyph_fr.dic"), "x").unwrap();
        std::fs::write(dir.join("hyph_fr.aff"), "x").unwrap();
        std::fs::write(other.join("fr_FR.aff"), "").unwrap();
        std::fs::write(other.join("fr_FR.dic"), "1\nautre\n").unwrap();
        std::fs::write(other.join("es_MX.aff"), "").unwrap();
        std::fs::write(other.join("es_MX.dic"), "1\nhola\n").unwrap();
        let found = find_dictionaries(&[dir.clone(), other.clone(), dir.join("missing")]);
        let tags: Vec<&str> = found.iter().map(|d| d.tag.as_str()).collect();
        assert_eq!(tags, ["es-MX", "fr-FR"]);
        let fr = dictionary_for(&found, "fr-CA").unwrap();
        assert_eq!(fr.dic, dir.join("fr_FR.dic"), "the first folder wins");
        assert_eq!(dictionary_for(&found, "FR").unwrap().tag, "fr-FR");
        assert_eq!(dictionary_for(&found, "es").unwrap().tag, "es-MX");
        assert!(dictionary_for(&found, "de-DE").is_none() && dictionary_for(&found, "").is_none());
        let h = Hunspell::load(&fr.aff, &fr.dic).unwrap();
        assert!(h.check_exact("manges"));
        let es = dictionary_for(&found, "es").unwrap();
        assert_eq!(Hunspell::load(&es.aff, &es.dic).unwrap().language, "es-MX");
        assert!(Hunspell::load(&dir.join("no.aff"), &dir.join("no.dic")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn hostile_dictionaries_never_panic(
            aff in proptest::collection::vec(any::<u8>(), 0..600),
            dic in proptest::collection::vec(any::<u8>(), 0..300),
            word in ".{0,20}",
        ) {
            if let Ok(h) = Hunspell::parse(&aff, &dic) {
                let _ = h.check_exact(&word);
                let _ = h.candidates(&word);
            }
        }

        #[test]
        fn rule_like_lines_never_panic(
            lines in proptest::collection::vec("(PFX|SFX|AF|FLAG|REP|TRY) [A-Za-z0-9/\\[\\]^.]{0,6} [A-Za-z0-9/\\[\\]^.]{0,6} [A-Za-z0-9/\\[\\]^.]{0,6} [A-Za-z0-9\\[\\]^.]{0,6}", 0..20),
            words in proptest::collection::vec("[a-z]{1,6}(/[A-Za-z0-9,]{0,4})?", 1..20),
            word in "[a-z]{0,10}",
        ) {
            let aff = lines.join("\n");
            let dic = words.join("\n");
            if let Ok(h) = Hunspell::parse(aff.as_bytes(), dic.as_bytes()) {
                let _ = h.check_exact(&word);
                let _ = h.candidates(&word);
            }
        }
    }
}
