//! Edit distances used by the suggester: plain optimal-string-alignment (restricted Damerau-Levenshtein)
//! distance to bound the candidates, and a weighted variant that makes typing slips (adjacent keys, doubled
//! letters, swapped letters, vowel confusions) cheaper so they rank first.

/// Inputs longer than this are never compared (the callers cap words well below it).
pub const MAX_LEN: usize = 64;

/// Optimal-string-alignment distance between `a` and `b`, or `None` when it exceeds `max` (or an input is
/// longer than [`MAX_LEN`]).
pub fn osa_within(a: &[char], b: &[char], max: usize) -> Option<usize> {
    if a.len() > MAX_LEN || b.len() > MAX_LEN || a.len().abs_diff(b.len()) > max {
        return None;
    }
    let d = osa_table(a, b, &|_, _| 1, &|_, _| 1, &|_, _| 1, 1);
    let d = usize::try_from(d).unwrap_or(usize::MAX);
    (d <= max).then_some(d)
}

/// Weighted costs, in tenths of an edit.
const COST_FULL: u32 = 10;
/// A letter the typist left out (inserting it into what was typed).
const COST_OMITTED: u32 = 9;
const COST_ADJACENT_KEY: u32 = 6;
const COST_VOWEL: u32 = 8;
const COST_DOUBLED: u32 = 5;
/// Swapped neighbours, the commonest typing slip: cheaper than any substitution.
const COST_SWAP: u32 = 5;

/// Weighted optimal-string-alignment distance in tenths of an edit, from what was typed (`typed`) to a
/// dictionary word (`word`).
pub fn weighted(typed: &[char], word: &[char]) -> u32 {
    if typed.len() > MAX_LEN || word.len() > MAX_LEN {
        return u32::MAX;
    }
    // Deleting a typed letter that repeats its neighbour ("lettter") or inserting a letter that doubles one
    // ("leter" → "letter") is a cheap slip; a forgotten letter costs a little less than a stray one.
    let doubled = |s: &[char], i: usize| {
        let c = s.get(i);
        i.checked_sub(1).and_then(|p| s.get(p)) == c || s.get(i + 1) == c
    };
    let del = |s: &[char], i: usize| if doubled(s, i) { COST_DOUBLED } else { COST_FULL };
    let ins = |s: &[char], i: usize| if doubled(s, i) { COST_DOUBLED } else { COST_OMITTED };
    let sub = |x: char, y: char| {
        if keys_adjacent(x, y) {
            COST_ADJACENT_KEY
        } else if is_vowel(x) && is_vowel(y) {
            COST_VOWEL
        } else {
            COST_FULL
        }
    };
    osa_table(typed, word, &del, &ins, &sub, COST_SWAP)
}

/// Generic OSA dynamic programme over three rows. `del(a, i)` is the cost of deleting `a[i]`, `ins(b, j)` of
/// inserting `b[j]`, `sub(x, y)` of replacing `x` by `y` (only called when they differ), `swap` of
/// transposing two neighbours.
fn osa_table(
    a: &[char],
    b: &[char],
    del: &dyn Fn(&[char], usize) -> u32,
    ins: &dyn Fn(&[char], usize) -> u32,
    sub: &dyn Fn(char, char) -> u32,
    swap: u32,
) -> u32 {
    let n = b.len();
    let mut prev2: Vec<u32> = vec![0; n + 1];
    let mut prev: Vec<u32> = Vec::with_capacity(n + 1);
    prev.push(0);
    for j in 0..n {
        let last = prev.last().copied().unwrap_or(0);
        prev.push(last.saturating_add(ins(b, j)));
    }
    let mut cur: Vec<u32> = vec![0; n + 1];
    for (i, &ca) in a.iter().enumerate() {
        let first = prev.first().copied().unwrap_or(0).saturating_add(del(a, i));
        if let Some(c0) = cur.first_mut() {
            *c0 = first;
        }
        for (j, &cb) in b.iter().enumerate() {
            let up = prev.get(j + 1).copied().unwrap_or(u32::MAX).saturating_add(del(a, i));
            let left = cur.get(j).copied().unwrap_or(u32::MAX).saturating_add(ins(b, j));
            let diag_cost = if ca == cb { 0 } else { sub(ca, cb) };
            let diag = prev.get(j).copied().unwrap_or(u32::MAX).saturating_add(diag_cost);
            let mut best = up.min(left).min(diag);
            if i > 0 && j > 0 && a.get(i - 1) == Some(&cb) && b.get(j - 1) == Some(&ca) && ca != cb {
                let t = prev2.get(j - 1).copied().unwrap_or(u32::MAX).saturating_add(swap);
                best = best.min(t);
            }
            if let Some(c) = cur.get_mut(j + 1) {
                *c = best;
            }
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    prev.get(n).copied().unwrap_or(u32::MAX)
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
}

const ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

fn key_pos(c: char) -> Option<(usize, usize)> {
    ROWS.iter().enumerate().find_map(|(r, row)| row.chars().position(|k| k == c).map(|col| (r, col)))
}

/// Whether two letters sit next to each other on a US QWERTY keyboard (same row, or the staggered row above
/// or below).
pub fn keys_adjacent(x: char, y: char) -> bool {
    let (Some((rx, cx)), Some((ry, cy))) = (key_pos(x), key_pos(y)) else {
        return false;
    };
    if rx == ry {
        return cx.abs_diff(cy) == 1;
    }
    // Each lower row is shifted right by about half a key: key c on the lower row touches c and c+1 above.
    let (upper_col, lower_col) = match (rx, ry) {
        (a, b) if b == a + 1 => (cx, cy),
        (a, b) if a == b + 1 => (cy, cx),
        _ => return false,
    };
    upper_col == lower_col || upper_col == lower_col + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn osa_basics() {
        assert_eq!(osa_within(&c("kitten"), &c("sitting"), 3), Some(3));
        assert_eq!(osa_within(&c("teh"), &c("the"), 2), Some(1));
        assert_eq!(osa_within(&c(""), &c("ab"), 2), Some(2));
        assert_eq!(osa_within(&c("abc"), &c("abc"), 0), Some(0));
        assert_eq!(osa_within(&c("abcdef"), &c("a"), 2), None);
    }

    #[test]
    fn adjacency() {
        assert!(keys_adjacent('q', 'w'));
        assert!(keys_adjacent('s', 'w'));
        assert!(keys_adjacent('s', 'e'));
        assert!(keys_adjacent('z', 's'));
        assert!(keys_adjacent('n', 'h'));
        assert!(!keys_adjacent('q', 'p'));
        assert!(!keys_adjacent('z', 'q'));
        assert!(!keys_adjacent('1', 'q'));
    }

    #[test]
    fn weights_prefer_slips() {
        assert!(weighted(&c("teh"), &c("the")) < weighted(&c("teh"), &c("tea")));
        assert!(weighted(&c("helo"), &c("hello")) < weighted(&c("helo"), &c("help")));
        assert!(weighted(&c("tge"), &c("the")) < weighted(&c("tme"), &c("the")));
    }
}
