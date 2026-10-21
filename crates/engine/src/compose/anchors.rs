//! Keeps bookmarks on the text they mark while the body is edited.
//!
//! Bookmarks live beside the body ([`ComposeState::bookmarks`]) as (leaf, offset) places, so every edit
//! that moves text would leave them behind. Instead of teaching each edit about them, the compose keeps the
//! text of every leaf as it was when the bookmarks were last placed ([`ComposeState::bookmark_base`]) and,
//! after an edit, compares it with the body now: text before and after the stretch that changed keeps its
//! bookmarks (shifted by what was put in or taken out), and a bookmark inside a changed stretch that spans
//! paragraphs follows its paragraph when that paragraph is still there exactly once (moved or sorted).

use super::{Bookmark, ComposeState, DocPos, leaf, leaves};

/// The text of each leaf of a body, by path (offsets in a leaf count its chars).
pub(crate) type LeafTexts = Vec<(Vec<usize>, Vec<char>)>;

/// Stands between leaves in the flattened text (not a char a leaf can hold for this purpose: a leaf's text
/// is compared as a whole, so a stray one only makes the comparison a little less precise).
const SEP: char = '\u{2029}';

pub(crate) fn leaf_texts(blocks: &[mailcraft_model::Block]) -> LeafTexts {
    leaves(blocks)
        .into_iter()
        .filter_map(|p| leaf(blocks, &p).map(|(_, f)| (p, f.text().chars().collect())))
        .collect()
}

/// Start of each leaf in the flattened text, and that text's length.
fn starts(leaves: &LeafTexts) -> (Vec<usize>, usize) {
    let mut out = Vec::with_capacity(leaves.len());
    let mut at = 0usize;
    for (k, (_, t)) in leaves.iter().enumerate() {
        if k > 0 {
            at = at.saturating_add(1);
        }
        out.push(at);
        at = at.saturating_add(t.len());
    }
    (out, at)
}

fn flat_chars(leaves: &LeafTexts) -> impl DoubleEndedIterator<Item = char> + '_ {
    leaves
        .iter()
        .enumerate()
        .flat_map(|(k, (_, t))| (k > 0).then_some(SEP).into_iter().chain(t.iter().copied()))
}

/// How places in one body map to places in an edited copy of it.
struct PosMap<'a> {
    old: &'a LeafTexts,
    new: &'a LeafTexts,
    old_starts: Vec<usize>,
    new_starts: Vec<usize>,
    old_len: usize,
    new_len: usize,
    /// Chars the two share at the start and at the end (not overlapping).
    prefix: usize,
    suffix: usize,
    /// The change crosses a paragraph boundary.
    multi: bool,
}

impl<'a> PosMap<'a> {
    fn new(old: &'a LeafTexts, new: &'a LeafTexts) -> PosMap<'a> {
        let (old_starts, old_len) = starts(old);
        let (new_starts, new_len) = starts(new);
        let prefix = flat_chars(old).zip(flat_chars(new)).take_while(|(a, b)| a == b).count();
        let room = old_len.min(new_len).saturating_sub(prefix);
        let suffix =
            flat_chars(old).rev().zip(flat_chars(new).rev()).take(room).take_while(|(a, b)| a == b).count();
        let has_sep = |leaves: &LeafTexts, from: usize, to: usize| {
            flat_chars(leaves).skip(from).take(to.saturating_sub(from)).any(|c| c == SEP)
        };
        let multi = has_sep(old, prefix, old_len.saturating_sub(suffix))
            || has_sep(new, prefix, new_len.saturating_sub(suffix));
        PosMap { old, new, old_starts, new_starts, old_len, new_len, prefix, suffix, multi }
    }

    /// The place in the new body for `pos` in the old one.
    fn map(&self, pos: &DocPos) -> DocPos {
        let Some(k) = self.old.iter().position(|(p, _)| *p == pos.path) else {
            return pos.clone();
        };
        let (Some(start), Some((_, text))) = (self.old_starts.get(k), self.old.get(k)) else {
            return pos.clone();
        };
        let off = pos.offset.min(text.len());
        let g = start.saturating_add(off);
        let end = start.saturating_add(text.len());
        let changed_from = self.old_len.saturating_sub(self.suffix);
        let untouched = end < self.prefix || *start >= changed_from;
        if !untouched && self.multi {
            // Its paragraph was moved, split, merged or sorted: follow its text when that is still there
            // exactly once among the paragraphs the change touched.
            let touched_to = self.new_len.saturating_sub(self.suffix);
            let mut same = self.new.iter().zip(&self.new_starts).filter(|((_, t), s)| {
                s.saturating_add(t.len()) >= self.prefix && **s <= touched_to && t == text
            });
            if let (Some(((p, _), _)), None) = (same.next(), same.next()) {
                return DocPos::new(p.clone(), off);
            }
        }
        let ng = if g >= changed_from {
            g.saturating_add(self.new_len).saturating_sub(self.old_len)
        } else if g <= self.prefix {
            g
        } else {
            // Inside text that was taken out or replaced: the start of the change.
            self.prefix
        };
        self.at(ng)
    }

    /// The new body's place at flattened index `g`.
    fn at(&self, g: usize) -> DocPos {
        let j = self.new_starts.iter().rposition(|s| *s <= g).unwrap_or(0);
        match (self.new.get(j), self.new_starts.get(j)) {
            (Some((p, t)), Some(s)) => DocPos::new(p.clone(), g.saturating_sub(*s).min(t.len())),
            _ => DocPos::default(),
        }
    }
}

impl ComposeState {
    /// Moves the bookmarks with the text edited since they were last placed, and remembers the body's text
    /// as it is now. Called around every edit and before bookmarks are added or read.
    pub fn sync_bookmarks(&mut self) {
        if self.bookmarks.is_empty() {
            self.bookmark_base = None;
            return;
        }
        let now = leaf_texts(&self.body.blocks);
        if let Some(base) = self.bookmark_base.take()
            && base != now
        {
            let map = PosMap::new(&base, &now);
            for b in &mut self.bookmarks {
                b.pos = map.map(&b.pos);
            }
        }
        self.bookmark_base = Some(now);
    }

    /// The bookmarks where they are in the body now (see [`ComposeState::sync_bookmarks`]).
    pub fn current_bookmarks(&self) -> Vec<Bookmark> {
        let Some(base) = &self.bookmark_base else {
            return self.bookmarks.clone();
        };
        let now = leaf_texts(&self.body.blocks);
        if *base == now {
            return self.bookmarks.clone();
        }
        let map = PosMap::new(base, &now);
        self.bookmarks.iter().map(|b| Bookmark { name: b.name.clone(), pos: map.map(&b.pos) }).collect()
    }

    /// Where the bookmark called `name` is now (names compared ignoring ASCII case).
    pub fn bookmark_pos(&self, name: &str) -> Option<DocPos> {
        self.current_bookmarks().into_iter().find(|b| b.name.eq_ignore_ascii_case(name)).map(|b| b.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[&str]) -> LeafTexts {
        lines.iter().enumerate().map(|(i, l)| (vec![i], l.chars().collect())).collect()
    }

    fn map(old: &[&str], new: &[&str], path: usize, offset: usize) -> (usize, usize) {
        let (o, n) = (texts(old), texts(new));
        let p = PosMap::new(&o, &n).map(&DocPos::new(vec![path], offset));
        (p.path.first().copied().unwrap_or(99), p.offset)
    }

    #[test]
    fn places_follow_their_text() {
        // A paragraph typed above.
        assert_eq!(map(&["A", "Heading"], &["A", "New", "Heading"], 1, 0), (2, 0));
        // An empty paragraph made by Enter at the start of the heading.
        assert_eq!(map(&["A", "Heading"], &["A", "", "Heading"], 1, 0), (2, 0));
        // Typing before the place in the same paragraph.
        assert_eq!(map(&["abcdef"], &["abXcdef"], 0, 3), (0, 4));
        // Typing after it.
        assert_eq!(map(&["abcdef"], &["abcdeXf"], 0, 3), (0, 3));
        // Deleting text before it.
        assert_eq!(map(&["abcXdef"], &["abcdef"], 0, 4), (0, 3));
        // Splitting the paragraph before it.
        assert_eq!(map(&["abcdef"], &["abc", "def"], 0, 4), (1, 1));
        // Joining it to the paragraph above.
        assert_eq!(map(&["abc", "def"], &["abcdef"], 1, 1), (0, 4));
        // Sorting paragraphs, even ones that start alike.
        assert_eq!(map(&["b1", "a1", "c1"], &["a1", "b1", "c1"], 0, 1), (1, 1));
        assert_eq!(map(&["apple", "apricot"], &["apricot", "apple"], 0, 2), (1, 2));
        assert_eq!(map(&["apple", "apricot"], &["apricot", "apple"], 1, 0), (0, 0));
        // Its text deleted: the place where the deletion was.
        assert_eq!(map(&["one", "two", "four"], &["one", "four"], 1, 2), (1, 0));
        // Everything deleted.
        assert_eq!(map(&["one", "two"], &[""], 1, 2), (0, 0));
    }
}
