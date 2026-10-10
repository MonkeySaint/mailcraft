//! Word lists and the lazily built deletion index used for suggestions.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::distance::MAX_LEN;

/// A sorted, de-duplicated list of words stored in one string.
#[derive(Debug, Default)]
pub(crate) struct WordList {
    blob: Cow<'static, str>,
    /// (start, end) byte ranges into `blob`, sorted by the word they point at.
    spans: Vec<(u32, u32)>,
}

impl WordList {
    /// Parses one word per line (trimmed; empty lines and `#` comments skipped). Lines longer than
    /// [`MAX_LEN`] chars and lists over 4 GiB are not indexed.
    pub(crate) fn parse(blob: Cow<'static, str>) -> Self {
        let mut spans = Vec::new();
        let mut pos = 0usize;
        for line in blob.split_inclusive('\n') {
            let start = pos;
            pos = pos.saturating_add(line.len());
            let trimmed_start = line.len() - line.trim_start().len();
            let word = line.trim();
            if word.is_empty() || word.starts_with('#') || word.chars().count() > MAX_LEN {
                continue;
            }
            let s = start.saturating_add(trimmed_start);
            let e = s.saturating_add(word.len());
            if let (Ok(s), Ok(e)) = (u32::try_from(s), u32::try_from(e)) {
                spans.push((s, e));
            }
        }
        let mut list = WordList { blob, spans };
        let mut spans = std::mem::take(&mut list.spans);
        spans.sort_by(|a, b| list.text(*a).cmp(list.text(*b)));
        spans.dedup_by(|a, b| list.text(*a) == list.text(*b));
        list.spans = spans;
        list
    }

    fn text(&self, (s, e): (u32, u32)) -> &str {
        self.blob.get(s as usize..e as usize).unwrap_or("")
    }

    pub(crate) fn len(&self) -> usize {
        self.spans.len()
    }

    pub(crate) fn get(&self, i: usize) -> &str {
        self.spans.get(i).map_or("", |&span| self.text(span))
    }

    /// Drops `words` from the list.
    pub(crate) fn remove_words<'a>(&mut self, words: impl IntoIterator<Item = &'a &'a str>) {
        for w in words {
            if let Ok(i) = self.spans.binary_search_by(|&span| self.text(span).cmp(w)) {
                self.spans.remove(i);
            }
        }
    }

    pub(crate) fn contains(&self, word: &str) -> bool {
        self.spans.binary_search_by(|&span| self.text(span).cmp(word)).is_ok()
    }
}

/// One language's word lists: common (lower-case) words, proper nouns (accepted only when capitalised) and
/// a frequency ranking used to order suggestions.
#[derive(Debug)]
pub(crate) struct Dictionary {
    pub(crate) common: WordList,
    pub(crate) proper: WordList,
    freq: HashMap<String, u32>,
    index: OnceLock<DeletionIndex>,
}

impl Dictionary {
    pub(crate) fn new(common: Cow<'static, str>, proper: Cow<'static, str>, freq: &str) -> Self {
        let mut ranks = HashMap::new();
        for (rank, w) in
            freq.lines().map(str::trim).filter(|w| !w.is_empty() && !w.starts_with('#')).enumerate()
        {
            ranks.entry(w.to_lowercase()).or_insert(u32::try_from(rank).unwrap_or(u32::MAX));
        }
        Dictionary {
            common: WordList::parse(common),
            proper: WordList::parse(proper),
            freq: ranks,
            index: OnceLock::new(),
        }
    }

    /// 0-based frequency rank of a lower-case word, if it is among the ranked words.
    pub(crate) fn rank(&self, lower: &str) -> Option<u32> {
        self.freq.get(lower).copied()
    }

    /// Entry `id` of the combined id space (common words first, then proper nouns): (display form, proper?).
    pub(crate) fn entry(&self, id: u32) -> (&str, bool) {
        let id = id as usize;
        let n = self.common.len();
        if id < n { (self.common.get(id), false) } else { (self.proper.get(id - n), true) }
    }

    pub(crate) fn index(&self) -> &DeletionIndex {
        self.index.get_or_init(|| DeletionIndex::build(self))
    }
}

/// Deletion index (the "symmetric delete" idea): for every dictionary word, the hashes of the word and of each
/// string obtained by deleting one of its characters, mapped to the word's id. A query variant matches every
/// word within one edit when the variant's own 0- and 1-deletions are looked up; the suggester looks up the
/// query and its single-edit variants (see `Speller::suggest`) to reach distance 2. Hash collisions only add
/// candidates, which are verified with the real distance.
#[derive(Debug, Default)]
pub(crate) struct DeletionIndex {
    /// (hash, id) sorted by hash.
    pairs: Vec<(u32, u32)>,
}

pub(crate) fn hash_chars(chars: impl Iterator<Item = char>) -> u32 {
    // FNV-1a over the chars' scalar values.
    let mut h: u32 = 0x811c_9dc5;
    for c in chars {
        for b in u32::from(c).to_le_bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    h
}

pub(crate) fn lower_chars(s: &str) -> Vec<char> {
    s.chars().flat_map(char::to_lowercase).collect()
}

impl DeletionIndex {
    fn build(dict: &Dictionary) -> Self {
        let total = dict.common.len().saturating_add(dict.proper.len());
        let mut pairs = Vec::with_capacity(total.saturating_mul(10));
        for id in 0..total {
            let Ok(id32) = u32::try_from(id) else { break };
            let chars = lower_chars(dict.entry(id32).0);
            pairs.push((hash_chars(chars.iter().copied()), id32));
            for skip in 0..chars.len() {
                // Deleting either letter of a doubled pair gives the same string; index it once.
                if skip > 0 && chars.get(skip - 1) == chars.get(skip) {
                    continue;
                }
                let h = hash_chars(chars.iter().enumerate().filter(|&(i, _)| i != skip).map(|(_, &c)| c));
                pairs.push((h, id32));
            }
        }
        pairs.sort_unstable();
        pairs.dedup();
        DeletionIndex { pairs }
    }

    /// Ids of the words indexed under `hash`.
    pub(crate) fn lookup(&self, hash: u32) -> impl Iterator<Item = u32> + '_ {
        let from = self.pairs.partition_point(|&(h, _)| h < hash);
        self.pairs.get(from..).unwrap_or(&[]).iter().take_while(move |&&(h, _)| h == hash).map(|&(_, id)| id)
    }
}
