//! IMAP sequence sets (`1:4,7,9:*`), used for UIDs and message numbers (RFC 9051 §9 `sequence-set`).

use std::fmt;

/// `*` in a sequence set: the largest value.
pub const STAR: u32 = u32::MAX;

/// A set of inclusive ranges, normalised (sorted, merged) when built with [`SeqSet::from_ids`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SeqSet(pub Vec<(u32, u32)>);

impl SeqSet {
    /// The set `lo:*`.
    pub fn from(lo: u32) -> Self {
        SeqSet(vec![(lo, STAR)])
    }

    /// The set `lo:hi`.
    pub fn range(lo: u32, hi: u32) -> Self {
        SeqSet(vec![(lo.min(hi), lo.max(hi))])
    }

    /// Builds a compact set from individual ids (any order, duplicates allowed). Zero is skipped.
    pub fn from_ids(ids: &[u32]) -> Self {
        let mut v: Vec<u32> = ids.iter().copied().filter(|&i| i != 0).collect();
        v.sort_unstable();
        v.dedup();
        let mut out: Vec<(u32, u32)> = Vec::new();
        for id in v {
            match out.last_mut() {
                Some((_, hi)) if hi.checked_add(1) == Some(id) => *hi = id,
                _ => out.push((id, id)),
            }
        }
        SeqSet(out)
    }

    /// Parses `1:4,7,9:*`. Returns `None` for malformed input (empty parts, zero, junk).
    pub fn parse(s: &str) -> Option<SeqSet> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for part in s.split(',') {
            let num = |t: &str| -> Option<u32> {
                if t == "*" {
                    Some(STAR)
                } else if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
                    t.parse::<u32>().ok().filter(|&n| n != 0)
                } else {
                    None
                }
            };
            match part.split_once(':') {
                Some((a, b)) => {
                    let (a, b) = (num(a)?, num(b)?);
                    out.push((a.min(b), a.max(b)));
                }
                None => {
                    let a = num(part)?;
                    out.push((a, a));
                }
            }
        }
        Some(SeqSet(out))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `id` is in the set.
    pub fn contains(&self, id: u32) -> bool {
        self.0.iter().any(|&(lo, hi)| lo <= id && id <= hi)
    }

    /// Number of ids in the set (saturating). Reversed ranges (only possible when built by hand) are empty.
    pub fn count(&self) -> u64 {
        self.0.iter().filter(|(lo, hi)| lo <= hi).fold(0u64, |acc, &(lo, hi)| {
            acc.saturating_add(u64::from(hi.saturating_sub(lo)).saturating_add(1))
        })
    }

    /// Expands to individual ids, at most `cap` of them (ranges ending in `*` stop at the cap too).
    pub fn expand(&self, cap: usize) -> Vec<u32> {
        let mut out = Vec::new();
        for &(lo, hi) in self.0.iter().filter(|(lo, hi)| lo <= hi) {
            let mut i = lo;
            loop {
                if out.len() >= cap {
                    return out;
                }
                out.push(i);
                if i >= hi {
                    break;
                }
                i = i.saturating_add(1);
            }
        }
        out
    }

    /// Splits into sets of at most `chunk` ids each (for batching large fetches).
    pub fn chunks(ids: &[u32], chunk: usize) -> Vec<SeqSet> {
        let chunk = chunk.max(1);
        let mut v: Vec<u32> = ids.to_vec();
        v.sort_unstable();
        v.dedup();
        v.chunks(chunk).map(SeqSet::from_ids).collect()
    }
}

impl fmt::Display for SeqSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = |v: u32| if v == STAR { "*".to_owned() } else { v.to_string() };
        for (i, &(lo, hi)) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            if lo == hi {
                f.write_str(&n(lo))?;
            } else {
                write!(f, "{}:{}", n(lo), n(hi))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn compact_and_display() {
        let s = SeqSet::from_ids(&[5, 1, 2, 3, 9, 3, 10, 0]);
        assert_eq!(s.to_string(), "1:3,5,9:10");
        assert_eq!(SeqSet::from(7).to_string(), "7:*");
        assert_eq!(SeqSet::parse("1:3,5,9:*").unwrap().0, vec![(1, 3), (5, 5), (9, STAR)]);
        assert_eq!(SeqSet::parse("4:2").unwrap().0, vec![(2, 4)]);
        assert!(SeqSet::parse("").is_none());
        assert!(SeqSet::parse("1,,2").is_none());
        assert!(SeqSet::parse("0").is_none());
        assert!(SeqSet::parse("a").is_none());
        assert_eq!(SeqSet::parse("1:*").unwrap().expand(3), vec![1, 2, 3]);
        assert_eq!(SeqSet::parse("1:*").unwrap().count(), u64::from(u32::MAX));
        assert!(SeqSet::parse("3:5").unwrap().contains(4));
        // The field is public: a hand-built reversed range must not overflow.
        let odd = SeqSet(vec![(5, 3), (u32::MAX, 0)]);
        assert_eq!(odd.count(), 0);
        assert!(odd.expand(10).len() <= 10);
    }

    proptest! {
        #[test]
        fn roundtrip(ids in proptest::collection::vec(1u32..500, 0..60)) {
            let s = SeqSet::from_ids(&ids);
            let mut want: Vec<u32> = ids.clone();
            want.sort_unstable();
            want.dedup();
            prop_assert_eq!(s.expand(10_000), want.clone());
            if !want.is_empty() {
                prop_assert_eq!(SeqSet::parse(&s.to_string()).unwrap(), s);
            }
        }

        #[test]
        fn parse_never_panics(s in "\\PC{0,40}") {
            let _ = SeqSet::parse(&s).map(|x| (x.to_string(), x.count(), x.expand(5)));
        }
    }
}
