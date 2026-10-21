//! The inverted index: folded token → posting list of item slots (with the fields the token occurs in and
//! its frequency), plus a forward token stream per item (phrase checks, exact removal) and the item's
//! non-text properties (flags, dates, size, folder, people addresses, categories).
//!
//! Items are added and replaced one at a time ([`Index::upsert`], [`Index::remove`]); a removed item's slot
//! goes dead and its postings are skipped until [`Index::compact`] (run automatically once dead slots
//! outnumber live ones) rewrites the posting lists. Queries combine per-term bitsets over slots, so even
//! broad prefix searches over 50,000 items take a few milliseconds.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

use crate::doc::{Doc, DocKind, Level, Person};
use crate::eval::{Context, date_of, text_in_segment};
use crate::fold::fold;
use crate::query::{CategoryMatch, Expiry, Query, Term, Text, field};
use crate::token::{for_each_token, words};

/// Most body tokens kept per item: every token of the longest body a [`Doc`] holds
/// ([`crate::doc::MAX_BODY_BYTES`] of one-letter words), so the index and direct evaluation agree.
pub const MAX_DOC_TOKENS: usize = 131_072;
/// Most tokens kept for each other field (subject, each people list, attachment names). Fields have their own
/// budgets so a giant body or recipient list never pushes the other fields out of the index.
pub const MAX_FIELD_TOKENS: usize = 16_384;
/// Most items one index holds.
pub const MAX_DOCS: usize = 4_000_000;

/// Separates people / attachment names in the token stream (no phrase spans it).
const GAP: u32 = u32::MAX;

/// Field weights for ranking, in `field::ALL` order.
const WEIGHTS: [f32; 6] = [4.0, 3.0, 2.0, 1.5, 1.0, 2.0];
/// Score of an item received just now; halves every [`RECENCY_HALF_LIFE_DAYS`].
const RECENCY_WEIGHT: f32 = 3.0;
const RECENCY_HALF_LIFE_DAYS: f32 = 14.0;

// ---------------------------------------------------------------------------------------------------------
// Small helpers: a fast non-cryptographic hasher and a bitset

/// FxHash-style hasher (keys are our own tokens and ids; no DoS concern inside one process's index).
#[derive(Default, Clone, Copy)]
struct Fx(u64);

impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        let (chunks, rest) = bytes.as_chunks::<8>();
        for c in chunks {
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(*c)).wrapping_mul(K);
        }
        for b in rest {
            self.0 = (self.0.rotate_left(5) ^ u64::from(*b)).wrapping_mul(K);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;

/// A set of slots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Bits(Vec<u64>);

impl Bits {
    fn empty(n: usize) -> Bits {
        Bits(vec![0; n.div_ceil(64)])
    }
    fn set(&mut self, i: usize) {
        if let Some(w) = self.0.get_mut(i / 64) {
            *w |= 1 << (i % 64);
        }
    }
    fn clear(&mut self, i: usize) {
        if let Some(w) = self.0.get_mut(i / 64) {
            *w &= !(1 << (i % 64));
        }
    }
    fn and_with(&mut self, o: &Bits) {
        for (a, b) in self.0.iter_mut().zip(o.0.iter().chain(std::iter::repeat(&0))) {
            *a &= b;
        }
    }
    fn or_with(&mut self, o: &Bits) {
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            *a |= b;
        }
    }
    fn and_not(&mut self, o: &Bits) {
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            *a &= !b;
        }
    }
    fn is_empty(&self) -> bool {
        self.0.iter().all(|w| *w == 0)
    }
    fn ones(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(wi, w)| {
            let mut w = *w;
            std::iter::from_fn(move || {
                if w == 0 {
                    return None;
                }
                let b = w.trailing_zeros() as usize;
                w &= w - 1;
                Some(wi * 64 + b)
            })
        })
    }
}

// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Posting {
    slot: u32,
    fields: u8,
    tf: u16,
}

/// Most properties kept per item, and the longest value kept.
const MAX_PROPS: usize = 64;
const MAX_PROP_VALUE: usize = 512;

#[derive(Clone, Debug, Default)]
struct Slot {
    id: u64,
    live: bool,
    kind: DocKind,
    folder: u64,
    received: i64,
    sent: Option<i64>,
    size: u64,
    read: bool,
    flagged: bool,
    has_attachments: bool,
    importance: Level,
    expires: Option<i64>,
    /// (field bit, folded address) for `from:me` and friends.
    people: Box<[(u8, Box<str>)]>,
    /// Folded word tokens of each category name.
    categories: Box<[Box<[Box<str>]>]>,
    /// Item properties for form-field conditions (`Doc::props`).
    props: Box<[(Box<str>, Box<str>)]>,
    /// Term ids of every field in `field::ALL` order, `GAP` between segments.
    tokens: Box<[u32]>,
    /// End offset in `tokens` of each field.
    ends: [u32; 6],
}

impl Slot {
    fn field_tokens(&self, fi: usize) -> &[u32] {
        let start = if fi == 0 { 0 } else { self.ends.get(fi - 1).copied().unwrap_or(0) as usize };
        let end = self.ends.get(fi).copied().unwrap_or(0) as usize;
        self.tokens.get(start..end).unwrap_or(&[])
    }
}

/// A search result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub id: u64,
    /// Relevance: field-weighted term frequency × rarity, plus a recency bonus.
    pub score: f32,
    /// Received (or start / due / modified) time, unix seconds.
    pub received: i64,
}

/// Restricts a search to some folders and kinds.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Only items in these folders (`None` = all).
    pub folders: Option<HashSet<u64>>,
    /// Only these kinds (empty = all).
    pub kinds: Vec<DocKind>,
}

impl Filter {
    /// Mail in the given folders.
    pub fn mail_in(folders: impl IntoIterator<Item = u64>) -> Filter {
        Filter { folders: Some(folders.into_iter().collect()), kinds: vec![DocKind::Mail] }
    }
}

/// Item counts of a query.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub total: usize,
    pub unread: usize,
}

/// Index size, for Indexing Status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub items: usize,
    pub terms: usize,
    pub postings: usize,
    pub dead_slots: usize,
}

/// The search index.
#[derive(Clone, Debug, Default)]
pub struct Index {
    dict: FxMap<Box<str>, u32>,
    sorted: BTreeMap<Box<str>, u32>,
    postings: Vec<Vec<Posting>>,
    slots: Vec<Slot>,
    by_id: FxMap<u64, u32>,
    live: Bits,
    dead: usize,
}

impl Index {
    pub fn new() -> Index {
        Index::default()
    }

    /// Number of items.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn contains(&self, id: u64) -> bool {
        self.by_id.contains_key(&id)
    }

    /// Ids of every indexed item (any order).
    pub fn ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.by_id.keys().copied()
    }

    pub fn stats(&self) -> Stats {
        Stats {
            items: self.len(),
            terms: self.dict.len(),
            postings: self.postings.iter().map(Vec::len).sum(),
            dead_slots: self.dead,
        }
    }

    fn term_id(&mut self, token: &str) -> Option<u32> {
        if let Some(id) = self.dict.get(token) {
            return Some(*id);
        }
        let id = u32::try_from(self.postings.len()).ok().filter(|id| *id < GAP)?;
        let key: Box<str> = token.into();
        self.dict.insert(key.clone(), id);
        self.sorted.insert(key, id);
        self.postings.push(Vec::new());
        Some(id)
    }

    /// Adds `doc`, replacing any item with the same id.
    pub fn upsert(&mut self, doc: &Doc) {
        self.remove(doc.id);
        if self.slots.len() >= MAX_DOCS {
            self.compact();
            if self.slots.len() >= MAX_DOCS {
                return;
            }
        }
        let Ok(slot_no) = u32::try_from(self.slots.len()) else { return };

        let mut tokens: Vec<u32> = Vec::new();
        let mut ends = [0u32; 6];
        let mut pairs: Vec<(u32, u8)> = Vec::new();
        let mut buf = String::new();
        for (fi, bit) in field::ALL.iter().enumerate() {
            let budget = if *bit == field::BODY { MAX_DOC_TOKENS } else { MAX_FIELD_TOKENS };
            let limit = tokens.len().saturating_add(budget);
            for (si, seg) in crate::eval::segments(doc, *bit).iter().enumerate() {
                if si > 0 && tokens.last().is_some_and(|t| *t != GAP) && tokens.len() < limit {
                    tokens.push(GAP);
                }
                if tokens.len() >= limit {
                    break;
                }
                for_each_token(seg, &mut buf, |_, _, t| {
                    if tokens.len() < limit
                        && let Some(id) = self.term_id(t)
                    {
                        tokens.push(id);
                        pairs.push((id, *bit));
                    }
                });
            }
            if let Some(e) = ends.get_mut(fi) {
                *e = u32::try_from(tokens.len()).unwrap_or(u32::MAX);
            }
        }
        pairs.sort_unstable();
        let mut i = 0;
        while let Some(&(term, _)) = pairs.get(i) {
            let mut fields = 0u8;
            let mut tf = 0u16;
            while let Some(&(t, b)) = pairs.get(i) {
                if t != term {
                    break;
                }
                fields |= b;
                tf = tf.saturating_add(1);
                i += 1;
            }
            if let Some(list) = self.postings.get_mut(term as usize) {
                list.push(Posting { slot: slot_no, fields, tf });
            }
        }

        let mut people: Vec<(u8, Box<str>)> = Vec::new();
        for (bit, list) in [(field::FROM, &doc.from), (field::TO, &doc.to), (field::CC, &doc.cc)] {
            for Person { addr, .. } in list {
                let a = fold(addr.trim());
                if !a.is_empty() {
                    people.push((bit, a.into()));
                }
            }
        }
        let categories: Vec<Box<[Box<str>]>> = doc
            .categories
            .iter()
            .map(|c| words(c).into_iter().map(String::into_boxed_str).collect::<Vec<_>>().into_boxed_slice())
            .collect();
        self.slots.push(Slot {
            id: doc.id,
            live: true,
            kind: doc.kind,
            folder: doc.folder,
            received: doc.received,
            sent: doc.sent,
            size: doc.size,
            read: doc.read,
            flagged: doc.flagged,
            has_attachments: doc.has_attachments,
            importance: doc.importance,
            expires: doc.expires,
            people: people.into_boxed_slice(),
            categories: categories.into_boxed_slice(),
            props: doc
                .props
                .iter()
                .take(MAX_PROPS)
                .map(|(k, v)| (k.as_str().into(), v.chars().take(MAX_PROP_VALUE).collect::<String>().into()))
                .collect(),
            tokens: tokens.into_boxed_slice(),
            ends,
        });
        if self.live.0.len() < self.slots.len().div_ceil(64) {
            self.live.0.push(0);
        }
        self.live.set(slot_no as usize);
        self.by_id.insert(doc.id, slot_no);
    }

    /// Removes an item; false when it was not indexed.
    pub fn remove(&mut self, id: u64) -> bool {
        let Some(slot) = self.by_id.remove(&id) else { return false };
        let i = slot as usize;
        if let Some(s) = self.slots.get_mut(i) {
            s.live = false;
            s.tokens = Box::new([]);
            s.people = Box::new([]);
            s.categories = Box::new([]);
        }
        self.live.clear(i);
        self.dead = self.dead.saturating_add(1);
        if self.dead > 4_096 && self.dead > self.by_id.len() {
            self.compact();
        }
        true
    }

    /// Drops dead slots and their postings.
    pub fn compact(&mut self) {
        if self.dead == 0 {
            return;
        }
        let mut map: Vec<u32> = vec![u32::MAX; self.slots.len()];
        let mut slots = Vec::with_capacity(self.by_id.len());
        for (old, s) in std::mem::take(&mut self.slots).into_iter().enumerate() {
            if s.live {
                if let (Some(m), Ok(n)) = (map.get_mut(old), u32::try_from(slots.len())) {
                    *m = n;
                }
                slots.push(s);
            }
        }
        for list in &mut self.postings {
            list.retain_mut(|p| match map.get(p.slot as usize) {
                Some(&n) if n != u32::MAX => {
                    p.slot = n;
                    true
                }
                _ => false,
            });
        }
        self.by_id.clear();
        let mut live = Bits::empty(slots.len());
        for (i, s) in slots.iter().enumerate() {
            if let Ok(n) = u32::try_from(i) {
                self.by_id.insert(s.id, n);
            }
            live.set(i);
        }
        self.slots = slots;
        self.live = live;
        self.dead = 0;
    }
}

// ---------------------------------------------------------------------------------------------------------
// Queries

impl Index {
    /// Term ids a query word stands for (sorted): the exact token, or every token starting with it.
    fn terms_for(&self, word: &str, prefix: bool) -> Vec<u32> {
        let mut out: Vec<u32> = if prefix {
            self.sorted
                .range::<str, _>((std::ops::Bound::Included(word), std::ops::Bound::Unbounded))
                .take_while(|(k, _)| k.starts_with(word))
                .map(|(_, v)| *v)
                .collect()
        } else {
            self.dict.get(word).copied().into_iter().collect()
        };
        out.sort_unstable();
        out
    }

    fn empty_bits(&self) -> Bits {
        Bits::empty(self.slots.len())
    }

    /// Live slots for which `f` holds.
    fn scan(&self, f: impl Fn(&Slot) -> bool) -> Bits {
        let mut b = self.empty_bits();
        for i in self.live.ones() {
            if self.slots.get(i).is_some_and(&f) {
                b.set(i);
            }
        }
        b
    }

    /// Word sets of a text term (`None` when some word occurs nowhere).
    fn word_sets(&self, t: &Text) -> Option<Vec<Vec<u32>>> {
        let k = t.words.len();
        let sets: Vec<Vec<u32>> =
            t.words.iter().enumerate().map(|(j, w)| self.terms_for(w, t.prefix && j + 1 == k)).collect();
        if sets.is_empty() || sets.iter().any(Vec::is_empty) { None } else { Some(sets) }
    }

    fn text_bits(&self, mask: u8, t: &Text) -> Bits {
        let Some(sets) = self.word_sets(t) else { return self.empty_bits() };
        let mut acc: Option<Bits> = None;
        for set in &sets {
            let mut b = self.empty_bits();
            for term in set {
                for p in self.postings.get(*term as usize).map_or(&[][..], Vec::as_slice) {
                    if p.fields & mask != 0 {
                        b.set(p.slot as usize);
                    }
                }
            }
            match acc.as_mut() {
                Some(a) => a.and_with(&b),
                None => acc = Some(b),
            }
            if acc.as_ref().is_some_and(Bits::is_empty) {
                break;
            }
        }
        let mut acc = acc.unwrap_or_else(|| self.empty_bits());
        acc.and_with(&self.live);
        if sets.len() > 1 {
            let failed: Vec<usize> = acc
                .ones()
                .filter(|i| !self.slots.get(*i).is_some_and(|s| phrase_in_slot(s, mask, &sets)))
                .collect();
            for i in failed {
                acc.clear(i);
            }
        }
        acc
    }

    fn term_bits(&self, t: &Term, ctx: &Context, me: &[String]) -> Bits {
        match t {
            Term::Text(f, text) => self.text_bits(f.mask(), text),
            Term::Me(f) => {
                let mask = f.mask();
                self.scan(|s| s.people.iter().any(|(b, a)| b & mask != 0 && me.iter().any(|m| **m == **a)))
            }
            Term::Category(CategoryMatch::Any) => self.scan(|s| !s.categories.is_empty()),
            Term::Category(CategoryMatch::None) => self.scan(|s| s.categories.is_empty()),
            Term::Category(CategoryMatch::Named(text)) => {
                self.scan(|s| s.categories.iter().any(|c| text_in_segment(text, c)))
            }
            Term::Folder(text) => {
                let set = ctx.folders_matching(text);
                self.scan(|s| set.binary_search(&s.folder).is_ok())
            }
            Term::HasAttachments(b) => self.scan(|s| s.has_attachments == *b),
            Term::Read(b) => self.scan(|s| s.read == *b),
            Term::Flagged(b) => self.scan(|s| s.flagged == *b),
            Term::Importance(l) => self.scan(|s| s.importance == *l),
            Term::Date(f, r) => {
                let (lo, hi) = ctx.day_bounds(r);
                self.scan(|s| {
                    let day = ctx.day_of(date_of(s.received, s.sent, *f));
                    day >= lo && day <= hi
                })
            }
            Term::Size(spec) => self.scan(|s| spec.contains(s.size)),
            Term::Kind(k) => self.scan(|s| s.kind == *k),
            Term::Prop(p) => self.scan(|s| p.matches(&s.props)),
            Term::Expires(v) => match Expiry::parse(v) {
                Some(e) => self.scan(|s| ctx.expiry_matches(&e, s.expires)),
                None => self.empty_bits(),
            },
            Term::Invalid(..) => self.empty_bits(),
        }
    }

    fn eval(&self, q: &Query, ctx: &Context, me: &[String]) -> Bits {
        match q {
            Query::All => self.live.clone(),
            Query::Nothing => self.empty_bits(),
            Query::And(v) => {
                let mut it = v.iter();
                let Some(first) = it.next() else { return self.live.clone() };
                let mut acc = self.eval(first, ctx, me);
                for c in it {
                    if acc.is_empty() {
                        break;
                    }
                    acc.and_with(&self.eval(c, ctx, me));
                }
                acc
            }
            Query::Or(v) => {
                let mut acc = self.empty_bits();
                for c in v {
                    acc.or_with(&self.eval(c, ctx, me));
                }
                acc
            }
            Query::Not(c) => {
                let mut acc = self.live.clone();
                acc.and_not(&self.eval(c, ctx, me));
                acc
            }
            Query::Term(t) => self.term_bits(t, ctx, me),
        }
    }

    fn filter_bits(&self, filter: &Filter) -> Bits {
        if filter.folders.is_none() && filter.kinds.is_empty() {
            return self.live.clone();
        }
        let kinds: u8 = filter.kinds.iter().fold(0, |m, k| m | k.bit());
        self.scan(|s| {
            filter.folders.as_ref().is_none_or(|f| f.contains(&s.folder))
                && (kinds == 0 || kinds & s.kind.bit() != 0)
        })
    }

    fn result_bits(&self, q: &Query, ctx: &Context, filter: &Filter) -> Bits {
        let me = ctx.me_folded();
        let mut bits = self.filter_bits(filter);
        if !bits.is_empty() {
            bits.and_with(&self.eval(q, ctx, &me));
        }
        bits
    }

    /// Ids of the items matching `q` within `filter` (index order).
    pub fn matching(&self, q: &Query, ctx: &Context, filter: &Filter) -> Vec<u64> {
        let bits = self.result_bits(q, ctx, filter);
        bits.ones().filter_map(|i| self.slots.get(i)).map(|s| s.id).collect()
    }

    /// Total and unread counts of the items matching `q` within `filter`.
    pub fn count(&self, q: &Query, ctx: &Context, filter: &Filter) -> Counts {
        let bits = self.result_bits(q, ctx, filter);
        let mut c = Counts::default();
        for s in bits.ones().filter_map(|i| self.slots.get(i)) {
            c.total += 1;
            if !s.read {
                c.unread += 1;
            }
        }
        c
    }

    /// Items matching `q` within `filter`, best first: field-weighted matches of the (non-negated) words,
    /// plus a recency bonus; ties go to the newer item.
    pub fn search(&self, q: &Query, ctx: &Context, filter: &Filter) -> Vec<Hit> {
        let bits = self.result_bits(q, ctx, filter);
        let mut scores: FxMap<usize, f32> = bits.ones().map(|i| (i, 0.0)).collect();
        if scores.is_empty() {
            return Vec::new();
        }
        let n = self.len().max(1) as f32;
        for (term, negated) in q.terms() {
            let Term::Text(f, text) = term else { continue };
            if negated {
                continue;
            }
            let mask = f.mask();
            let Some(sets) = self.word_sets(text) else { continue };
            for term_id in sets.iter().flatten() {
                let list = self.postings.get(*term_id as usize).map_or(&[][..], Vec::as_slice);
                let idf = (1.0 + n / list.len().max(1) as f32).ln();
                for p in list {
                    let hit = p.fields & mask;
                    if hit == 0 {
                        continue;
                    }
                    if let Some(sc) = scores.get_mut(&(p.slot as usize)) {
                        let w = field::ALL
                            .iter()
                            .zip(WEIGHTS)
                            .filter(|(b, _)| hit & **b != 0)
                            .map(|(_, w)| w)
                            .fold(0.0f32, f32::max);
                        *sc += w * (1.0 + f32::from(p.tf).ln()) * idf;
                    }
                }
            }
        }
        let mut hits: Vec<Hit> = scores
            .into_iter()
            .filter_map(|(i, sc)| {
                let s = self.slots.get(i)?;
                let age_days = (ctx.now.saturating_sub(s.received).max(0) / 86_400) as f32;
                let recency = RECENCY_WEIGHT * 0.5f32.powf(age_days / RECENCY_HALF_LIFE_DAYS);
                Some(Hit { id: s.id, score: sc + recency, received: s.received })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score.total_cmp(&a.score).then(b.received.cmp(&a.received)).then(b.id.cmp(&a.id))
        });
        hits
    }
}

/// True when the word sets occur in a row in one of the masked fields of `s`.
fn phrase_in_slot(s: &Slot, mask: u8, sets: &[Vec<u32>]) -> bool {
    let k = sets.len();
    field::ALL.iter().enumerate().filter(|(_, b)| mask & **b != 0).any(|(fi, _)| {
        let toks = s.field_tokens(fi);
        toks.len() >= k
            && (0..=toks.len() - k).any(|i| {
                sets.iter()
                    .enumerate()
                    .all(|(j, set)| toks.get(i + j).is_some_and(|t| set.binary_search(t).is_ok()))
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Person;
    use crate::eval::matches;
    use crate::query::parse;

    const NOW: i64 = 1_791_547_200;

    fn docs() -> Vec<Doc> {
        let mk = |id: u64, subject: &str, from: &str, body: &str, days_ago: i64| Doc {
            id,
            folder: id % 3,
            subject: subject.into(),
            from: vec![Person::new("", from)],
            to: vec![Person::new("Me", "me@example.com")],
            body: body.into(),
            received: NOW - days_ago * 86_400,
            read: id.is_multiple_of(2),
            ..Doc::default()
        };
        vec![
            mk(1, "Budget review", "ann@example.com", "numbers attached", 0),
            mk(2, "Lunch", "bob@example.org", "the budget can wait", 3),
            mk(3, "Re: Budget review", "carol@example.net", "looks fine", 40),
            mk(4, "Café opening", "dan@example.com", "résumé inside", 1),
        ]
    }

    fn ctx() -> Context {
        Context {
            now: NOW,
            me: vec!["me@example.com".into()],
            folders: vec![(0, "Inbox".into())],
            ..Context::default()
        }
    }

    fn index() -> Index {
        let mut ix = Index::new();
        for d in docs() {
            ix.upsert(&d);
        }
        ix
    }

    fn ids(ix: &Index, q: &str) -> Vec<u64> {
        let mut v = ix.matching(&parse(q), &ctx(), &Filter::default());
        v.sort_unstable();
        v
    }

    #[test]
    fn property_conditions_agree_with_direct_evaluation() {
        let mut ix = index();
        let mut docs = docs();
        let props = [
            vec![("sensitivity", "Private"), ("duedate", "2026-11-02"), ("percentcomplete", "50")],
            vec![("sensitivity", "Normal"), ("conversation", "lunch")],
            vec![("sensitivity", "Private"), ("company", "Example Café")],
            vec![("conversation", "lunch plans")],
        ];
        for (d, p) in docs.iter_mut().zip(props) {
            d.props = p.into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
            ix.upsert(d);
        }
        for (q, want) in [
            ("prop:\"Sensitivity=private\"", vec![1, 3]),
            ("prop:\"Sensitivity!=Private\"", vec![2, 4]),
            ("prop:\"Due Date<2026-12-01\"", vec![1]),
            ("prop:\"Due Date>2026-12-01\"", vec![]),
            ("prop:\"Percent Complete>9\"", vec![1]),
            ("prop:\"Company~cafe\"", vec![3]),
            ("prop:\"Company!~cafe\"", vec![1, 2, 4]),
            ("prop:\"Company=\"", vec![1, 2, 4]),
            ("prop:\"Company!=\"", vec![3]),
            ("conversation:\"RE: Lunch\"", vec![2]),
            ("prop:\"no operator\"", vec![]),
            ("budget prop:\"sensitivity=private\"", vec![1, 3]),
        ] {
            assert_eq!(ids(&ix, q), want, "{q}");
            let direct: Vec<u64> =
                docs.iter().filter(|d| crate::eval::matches(&parse(q), d, &ctx())).map(|d| d.id).collect();
            assert_eq!(direct, want, "{q} (direct)");
        }
        // Canonical text round-trips.
        for q in ["prop:\"Due Date<2026-12-01\"", "conversation:\"lunch\"", "prop:\"Company!=\""] {
            assert_eq!(parse(&parse(q).to_string()), parse(q), "{q}");
        }
    }

    #[test]
    fn expiring_soon_agrees_with_direct_evaluation() {
        let mut ix = index();
        let mut docs = docs();
        for (d, days) in docs.iter_mut().zip([Some(2), Some(-1), None, Some(20)]) {
            d.expires = days.map(|n| NOW + n * 86_400);
            ix.upsert(d);
        }
        for (q, want) in [
            ("expires:soon", vec![1]),
            ("expires:expired", vec![2]),
            ("expires:yes", vec![1, 2, 4]),
            ("expires:no", vec![3]),
            ("budget expires:soon", vec![1]),
            ("expires:whenever", vec![]),
        ] {
            assert_eq!(ids(&ix, q), want, "{q}");
            let direct: Vec<u64> =
                docs.iter().filter(|d| crate::eval::matches(&parse(q), d, &ctx())).map(|d| d.id).collect();
            assert_eq!(direct, want, "{q} (direct)");
        }
    }

    #[test]
    fn finds_and_ranks() {
        let ix = index();
        assert_eq!(ids(&ix, "budget"), [1, 2, 3]);
        assert_eq!(ids(&ix, "subject:budget"), [1, 3]);
        assert_eq!(ids(&ix, "\"budget review\""), [1, 3]);
        assert_eq!(ids(&ix, "\"review budget\""), Vec::<u64>::new());
        assert_eq!(ids(&ix, "cafe resume"), [4]);
        assert_eq!(ids(&ix, "from:example.com"), [1, 4]);
        assert_eq!(ids(&ix, "to:me isread:no"), [1, 3]);
        assert_eq!(ids(&ix, "folder:inbox"), [3]);
        assert_eq!(ids(&ix, "-budget"), [4]);
        let hits = ix.search(&parse("budget"), &ctx(), &Filter::default());
        let order: Vec<u64> = hits.iter().map(|h| h.id).collect();
        // Subject matches beat the body match; the newer subject match comes first.
        assert_eq!(order, [1, 3, 2]);
        let c = ix.count(&parse("budget"), &ctx(), &Filter::default());
        assert_eq!(c, Counts { total: 3, unread: 2 });
        let f = Filter { folders: Some([1].into_iter().collect()), kinds: vec![] };
        assert_eq!(ix.matching(&parse(""), &ctx(), &f), [1, 4]);
    }

    #[test]
    fn upsert_remove_compact() {
        let mut ix = index();
        let mut d = docs().remove(1);
        d.subject = "Dinner".into();
        ix.upsert(&d);
        assert_eq!(ids(&ix, "lunch"), Vec::<u64>::new());
        assert_eq!(ids(&ix, "dinner"), [2]);
        assert!(ix.remove(1));
        assert!(!ix.remove(1));
        assert_eq!(ids(&ix, "budget"), [2, 3]);
        assert_eq!(ix.stats().dead_slots, 2);
        ix.compact();
        assert_eq!(ix.stats().dead_slots, 0);
        assert_eq!(ids(&ix, "budget"), [2, 3]);
        assert_eq!(ids(&ix, ""), [2, 3, 4]);
        assert_eq!(ix.len(), 3);
    }

    #[test]
    fn a_giant_body_does_not_hide_the_attachments() {
        // The longest body `Doc::from_mail` keeps, written as one-letter words: as many tokens as fit.
        let body = "a ".repeat(crate::doc::MAX_BODY_BYTES / 2);
        let d = Doc {
            id: 1,
            subject: "Logs".into(),
            body,
            attachments: vec!["report.pdf".into()],
            ..Doc::default()
        };
        let mut ix = Index::new();
        ix.upsert(&d);
        for q in ["attachments:report", "logs", "\"a a a\""] {
            assert!(matches(&parse(q), &d, &ctx()), "{q}");
            assert_eq!(ix.matching(&parse(q), &ctx(), &Filter::default()), [1], "{q}");
        }
    }

    #[test]
    fn agrees_with_direct_evaluation() {
        let ix = index();
        for q in [
            "bud",
            "\"budget\"",
            "re review",
            "from:me",
            "to:me",
            "NOT (a OR b)",
            "received:today",
            "x OR budget",
        ] {
            let mut want: Vec<u64> =
                docs().iter().filter(|d| matches(&parse(q), d, &ctx())).map(|d| d.id).collect();
            want.sort_unstable();
            assert_eq!(ids(&ix, q), want, "{q}");
        }
    }
}
