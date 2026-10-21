//! Find and Replace with the More options: match case, whole words, prefix / suffix, wildcards, the Special
//! codes (`^t`, `^l`, `^?`, `^#`, `^$`, `^w`, `^^`, and `^&` in Replace with), formatting to find and to
//! replace with, direction, finding every match (Find In) and Highlight All.
//!
//! Matches stay inside one paragraph. The wildcard matcher backtracks with a step budget, so a hostile
//! pattern ends as "not found" instead of running away.

use mailcraft_geom::Color;
use mailcraft_model::TextStyle;

use super::{
    Atom, AtomKind, BodySelection, ComposeState, DocPos, Flat, LeafKind, MAX_REPLACEMENTS, RunAttr,
    clean_text, leaf, leaves, lower, store_leaf,
};

/// Longest find pattern accepted (chars).
pub const MAX_FIND_CHARS: usize = 255;
/// Matches collected by Find All / Highlight All at most.
pub const MAX_FIND_ALL: usize = 10_000;
/// Matcher steps (calls, and chars examined by the `^w` / `@` / `{n,m}` scans) one find or replace command
/// may take over the whole body; past them it stops with an error, so a hostile wildcard pattern against a
/// long body cannot freeze the editor.
const MAX_STEPS: usize = 20_000_000;
/// Longest Replace with text accepted (chars), as for Find what.
pub const MAX_REPLACE_CHARS: usize = 255;
/// Longest text one replacement may put in (chars), after `^&` is expanded.
pub const MAX_REPLACED_CHARS: usize = 65_536;
/// Chars Replace All may put in over the whole command.
const MAX_REPLACE_ALL_CHARS: usize = 4_000_000;

const TOO_LONG_SEARCH: &str =
    "This search takes too long to finish; try a simpler pattern or search a smaller selection.";

/// Counts one matcher step; false once the command's budget is spent.
fn step(steps: &mut usize) -> bool {
    *steps = steps.saturating_add(1);
    *steps <= MAX_STEPS
}
/// Largest `{n,m}` count.
const MAX_REPEAT: usize = 1_000;

/// Search direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FindDirection {
    /// Towards the end, then on from the start.
    #[default]
    Down,
    /// Towards the start, then on from the end.
    Up,
    /// The whole message (from the selection on, wrapping round).
    All,
}

impl FindDirection {
    pub fn from_name(n: &str) -> Option<FindDirection> {
        match n.trim().to_ascii_lowercase().as_str() {
            "down" => Some(FindDirection::Down),
            "up" => Some(FindDirection::Up),
            "all" => Some(FindDirection::All),
            _ => None,
        }
    }
}

/// Formatting a match must have (Find › Format), or that replaced text gets (Replace › Format). `None`
/// fields do not matter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FindFormat {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    /// Highlighted at all (find) / the highlight colour or none (replace).
    pub highlight: Option<bool>,
    pub highlight_color: Option<Color>,
    pub font: Option<String>,
    pub size_pt: Option<f32>,
    pub color: Option<Color>,
}

impl FindFormat {
    pub fn is_empty(&self) -> bool {
        *self == FindFormat::default()
    }

    fn matches(&self, st: &TextStyle) -> bool {
        let flag = |want: Option<bool>, have: Option<bool>| want.is_none_or(|w| have.unwrap_or(false) == w);
        flag(self.bold, st.bold)
            && flag(self.italic, st.italic)
            && flag(self.underline, st.underline)
            && flag(self.strike, st.strike)
            && self.highlight.is_none_or(|w| st.highlight.is_some() == w)
            && self.font.as_deref().is_none_or(|f| {
                st.font_family.as_deref().is_some_and(|have| {
                    have.split(',')
                        .next()
                        .is_some_and(|h| h.trim().trim_matches(['"', '\'']).eq_ignore_ascii_case(f.trim()))
                })
            })
            && self.size_pt.is_none_or(|s| st.size_pt.is_some_and(|h| (h - s).abs() < 0.05))
            && self.color.is_none_or(|c| st.color == Some(c))
    }

    fn apply(&self, st: &mut TextStyle) {
        if let Some(v) = self.bold {
            st.bold = Some(v);
        }
        if let Some(v) = self.italic {
            st.italic = Some(v);
        }
        if let Some(v) = self.underline {
            st.underline = Some(v);
        }
        if let Some(v) = self.strike {
            st.strike = Some(v);
        }
        match (self.highlight, self.highlight_color) {
            (Some(false), _) => st.highlight = None,
            (_, Some(c)) => st.highlight = Some(c),
            (Some(true), None) => st.highlight = Some(Color::rgb(0xff, 0xff, 0x00)),
            (None, None) => {}
        }
        if let Some(f) = &self.font {
            st.font_family = Some(f.clone());
        }
        if let Some(s) = self.size_pt {
            st.size_pt = Some(s);
        }
        if let Some(c) = self.color {
            st.color = Some(c);
        }
    }
}

/// Find options (Find and Replace › More).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FindOptions {
    pub match_case: bool,
    pub whole_word: bool,
    pub wildcards: bool,
    /// Match the start / the end of words.
    pub prefix: bool,
    pub suffix: bool,
    pub direction: FindDirection,
    /// Only matches inside the selection (Find In › Current Selection; Replace All in a selection).
    pub in_selection: bool,
    pub format: FindFormat,
    pub replace_format: FindFormat,
}

impl FindOptions {
    /// Just Match case (the old find).
    pub fn case(match_case: bool) -> FindOptions {
        FindOptions { match_case, ..FindOptions::default() }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Lit(char),
    Any,
    Digit,
    Letter,
    /// One or more white-space chars.
    Space,
    /// Any run of chars (wildcard `*`), shortest first.
    Star,
    Class {
        ranges: Vec<(char, char)>,
        negated: bool,
    },
    WordStart,
    WordEnd,
    Repeat {
        tok: Box<Tok>,
        min: usize,
        max: usize,
    },
}

/// A compiled find pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    toks: Vec<Tok>,
    match_case: bool,
}

fn special(c: char) -> Result<Tok, String> {
    Ok(match c {
        't' => Tok::Lit('\t'),
        'l' | 'L' => Tok::Lit('\n'),
        '?' => Tok::Any,
        '#' => Tok::Digit,
        '$' => Tok::Letter,
        'w' | 'W' => Tok::Space,
        '^' => Tok::Lit('^'),
        'p' | 'P' => {
            return Err(
                "Paragraph marks (^p) cannot be searched for: a match stays inside one paragraph.".into()
            );
        }
        other => return Err(format!("^{other} is not a special character MailCraft knows.")),
    })
}

impl Pattern {
    /// Compiles `needle` (with the Special `^` codes; wildcard syntax when `wildcards`).
    pub fn compile(needle: &str, opts: &FindOptions) -> Result<Pattern, String> {
        if needle.is_empty() {
            return Err("Type what to find.".into());
        }
        if needle.chars().count() > MAX_FIND_CHARS {
            return Err(format!("Find what can hold at most {MAX_FIND_CHARS} characters."));
        }
        let chars: Vec<char> = needle.chars().collect();
        let mut toks: Vec<Tok> = Vec::new();
        let mut i = 0usize;
        while let Some(&c) = chars.get(i) {
            i += 1;
            if c == '^' {
                let next =
                    chars.get(i).copied().ok_or("A ^ needs a code after it (^t, ^l, ^?, ^#, ^$, ^w, ^^).")?;
                i += 1;
                toks.push(special(next)?);
                continue;
            }
            if !opts.wildcards {
                toks.push(Tok::Lit(c));
                continue;
            }
            match c {
                '?' => toks.push(Tok::Any),
                '*' => toks.push(Tok::Star),
                '<' => toks.push(Tok::WordStart),
                '>' => toks.push(Tok::WordEnd),
                '(' | ')' => {}
                '\\' => {
                    let next = chars.get(i).copied().ok_or("A \\ needs a character after it.")?;
                    i += 1;
                    toks.push(Tok::Lit(next));
                }
                '[' => {
                    let close = chars
                        .iter()
                        .skip(i)
                        .position(|&x| x == ']')
                        .map(|p| p + i)
                        .ok_or("A [ has no matching ].")?;
                    let body: Vec<char> = chars.get(i..close).map(<[char]>::to_vec).unwrap_or_default();
                    i = close + 1;
                    let (negated, body) = match body.split_first() {
                        Some(('!', rest)) => (true, rest.to_vec()),
                        _ => (false, body),
                    };
                    let mut ranges = Vec::new();
                    let mut k = 0usize;
                    while let Some(&a) = body.get(k) {
                        if body.get(k + 1) == Some(&'-')
                            && let Some(&b) = body.get(k + 2)
                        {
                            if b < a {
                                return Err(format!("[{a}-{b}] is not a valid range."));
                            }
                            ranges.push((a, b));
                            k += 3;
                        } else {
                            ranges.push((a, a));
                            k += 1;
                        }
                    }
                    if ranges.is_empty() {
                        return Err("[] needs at least one character.".into());
                    }
                    toks.push(Tok::Class { ranges, negated });
                }
                '@' => {
                    let prev = toks.pop().ok_or("@ repeats the character before it.")?;
                    toks.push(Tok::Repeat { tok: Box::new(prev), min: 1, max: MAX_REPEAT });
                }
                '{' => {
                    let close = chars
                        .iter()
                        .skip(i)
                        .position(|&x| x == '}')
                        .map(|p| p + i)
                        .ok_or("A { has no matching }.")?;
                    let body: String = chars.get(i..close).map(|s| s.iter().collect()).unwrap_or_default();
                    i = close + 1;
                    let num = |t: &str| t.trim().parse::<usize>().ok().filter(|n| *n <= MAX_REPEAT);
                    let (min, max) = match body.split_once(',') {
                        None => {
                            let n = num(&body).ok_or("{n} needs a count.")?;
                            (n, n)
                        }
                        Some((a, b)) if b.trim().is_empty() => {
                            (num(a).ok_or("{n,} needs a count.")?, MAX_REPEAT)
                        }
                        Some((a, b)) => {
                            (num(a).ok_or("{n,m} needs counts.")?, num(b).ok_or("{n,m} needs counts.")?)
                        }
                    };
                    if max < min || max == 0 {
                        return Err(format!("{{{body}}} is not a valid count."));
                    }
                    let prev = toks.pop().ok_or("{n} repeats the character before it.")?;
                    toks.push(Tok::Repeat { tok: Box::new(prev), min, max });
                }
                other => toks.push(Tok::Lit(other)),
            }
        }
        if toks.is_empty() {
            return Err("Type what to find.".into());
        }
        // Wildcard searches are case-sensitive, as they are in word processors.
        Ok(Pattern { toks, match_case: opts.match_case || opts.wildcards })
    }

    fn one(&self, t: &Tok, c: char) -> bool {
        match t {
            Tok::Lit(x) => {
                if self.match_case {
                    *x == c
                } else {
                    lower(*x) == lower(c)
                }
            }
            Tok::Any => c != '\n',
            Tok::Digit => c.is_ascii_digit(),
            Tok::Letter => c.is_alphabetic(),
            Tok::Space => c.is_whitespace() && c != '\n',
            Tok::Class { ranges, negated } => {
                let hit = ranges.iter().any(|(a, b)| (*a..=*b).contains(&c));
                hit != *negated
            }
            _ => false,
        }
    }

    /// End of a match of tokens `ti..` starting at `ci`, if any.
    fn at(&self, ti: usize, chars: &[char], ci: usize, steps: &mut usize) -> Option<usize> {
        if !step(steps) {
            return None;
        }
        let Some(t) = self.toks.get(ti) else {
            return Some(ci);
        };
        let word = |i: usize| chars.get(i).is_some_and(|c| c.is_alphanumeric() || *c == '_');
        match t {
            Tok::Star => {
                let mut k = ci;
                loop {
                    if let Some(e) = self.at(ti + 1, chars, k, steps) {
                        return Some(e);
                    }
                    match chars.get(k) {
                        Some(c) if *c != '\n' => k += 1,
                        _ => return None,
                    }
                }
            }
            Tok::Space => {
                let mut n = 0usize;
                while chars.get(ci + n).is_some_and(|c| self.one(t, *c)) {
                    if !step(steps) {
                        return None;
                    }
                    n += 1;
                }
                (1..=n).rev().find_map(|k| self.at(ti + 1, chars, ci + k, steps))
            }
            Tok::WordStart => {
                let ok = word(ci) && (ci == 0 || !word(ci - 1));
                if ok { self.at(ti + 1, chars, ci, steps) } else { None }
            }
            Tok::WordEnd => {
                let ok = ci > 0 && word(ci - 1) && !word(ci);
                if ok { self.at(ti + 1, chars, ci, steps) } else { None }
            }
            Tok::Repeat { tok, min, max } => {
                let mut n = 0usize;
                while n < *max && chars.get(ci + n).is_some_and(|c| self.one(tok, *c)) {
                    if !step(steps) {
                        return None;
                    }
                    n += 1;
                }
                if n < *min {
                    return None;
                }
                (*min..=n).rev().find_map(|k| self.at(ti + 1, chars, ci + k, steps))
            }
            single => {
                let c = chars.get(ci)?;
                if self.one(single, *c) { self.at(ti + 1, chars, ci + 1, steps) } else { None }
            }
        }
    }

    /// The match starting at `i` (non-empty, within the word rules of `opts`), as its end. `steps` is the
    /// matcher budget shared by one search.
    fn match_at(&self, chars: &[char], i: usize, opts: &FindOptions, steps: &mut usize) -> Option<usize> {
        let word = |k: usize| chars.get(k).is_some_and(|c| c.is_alphanumeric() || *c == '_');
        let starts_word = i == 0 || !word(i - 1);
        if (opts.whole_word || opts.prefix) && !starts_word {
            return None;
        }
        let e = self.at(0, chars, i, steps).filter(|e| *e > i)?;
        let ends_word = !word(e);
        if (opts.whole_word || opts.suffix) && !ends_word {
            return None;
        }
        Some(e)
    }
}

/// Whether atoms `a..b` of `flat` all have the formatting `opts` looks for.
fn formatted(flat: &Flat, a: usize, b: usize, f: &FindFormat) -> bool {
    if f.is_empty() {
        return true;
    }
    flat.atoms.get(a..b).unwrap_or_default().iter().all(|atom| {
        !matches!(atom.kind, AtomKind::Char(_))
            || flat.attrs.get(atom.attr).is_some_and(|r| f.matches(&r.style))
    })
}

/// The first match in `flat` (whose text is `chars`) starting in `from..` (forward) or ending at or before
/// `before` (backward). `steps` is the budget of the whole command; `Err` once it is spent.
fn search(
    flat: &Flat,
    chars: &[char],
    pat: &Pattern,
    opts: &FindOptions,
    from: usize,
    before: Option<usize>,
    steps: &mut usize,
) -> Result<Option<(usize, usize)>, String> {
    let try_at = |i: usize, limit: usize, steps: &mut usize| {
        pat.match_at(chars, i, opts, steps)
            .filter(|e| *e <= limit && formatted(flat, i, *e, &opts.format))
            .map(|e| (i, e))
    };
    let (starts, limit): (Box<dyn Iterator<Item = usize>>, usize) = match before {
        None => (Box::new(from..chars.len()), usize::MAX),
        Some(limit) => (Box::new((0..limit.min(chars.len())).rev()), limit),
    };
    for i in starts {
        let hit = try_at(i, limit, steps);
        if *steps > MAX_STEPS {
            return Err(TOO_LONG_SEARCH.into());
        }
        if hit.is_some() {
            return Ok(hit);
        }
    }
    Ok(None)
}

/// The chars of `flat`, one per atom (what the matcher runs over).
fn chars_of(flat: &Flat) -> Vec<char> {
    flat.text().chars().collect()
}

/// Checks a find pattern (and a Replace with text) before anything is changed.
pub fn check_find(needle: &str, with: Option<&str>, opts: &FindOptions) -> Result<(), String> {
    Pattern::compile(needle, opts)?;
    if let Some(w) = with {
        expand_replacement(w, "")?;
    }
    Ok(())
}

/// Replace with: refuses a text longer than [`MAX_REPLACE_CHARS`].
fn check_replace_len(with: &str) -> Result<(), String> {
    if with.chars().count() > MAX_REPLACE_CHARS {
        return Err(format!("Replace with can hold at most {MAX_REPLACE_CHARS} characters."));
    }
    Ok(())
}

/// Replace with: the Special codes expanded (`^&` = what was found, `^t` tab, `^l` line break, `^^`).
pub fn expand_replacement(with: &str, found: &str) -> Result<String, String> {
    check_replace_len(with)?;
    let mut out = String::new();
    let mut len = 0usize;
    let found_len = found.chars().count();
    let mut chars = with.chars();
    while let Some(c) = chars.next() {
        // Every code below adds one char, except `^&`.
        let adds = if c == '^' && chars.clone().next() == Some('&') { found_len } else { 1 };
        len = len.saturating_add(adds);
        if len > MAX_REPLACED_CHARS {
            return Err(format!(
                "The replacement would be longer than {MAX_REPLACED_CHARS} characters; use ^& fewer times."
            ));
        }
        if c != '^' {
            out.push(if c == '\n' { ' ' } else { c });
            continue;
        }
        match chars.next() {
            Some('&') => out.push_str(found),
            Some('t') => out.push('\t'),
            Some('l' | 'L') => out.push('\n'),
            Some('^') => out.push('^'),
            Some('p' | 'P') => {
                return Err(
                    "Paragraph marks (^p) cannot be put in by Replace; use ^l for a line break.".into()
                );
            }
            Some(other) => return Err(format!("^{other} is not a special character MailCraft knows.")),
            None => return Err("A ^ needs a code after it.".into()),
        }
    }
    Ok(clean_text(&out))
}

impl Flat {
    /// Replaces each range `a..b` of `hits` (in order, not overlapping) with its text (`\n` = a line break)
    /// in the style of the text it replaces, then `fmt`, in one pass over the paragraph.
    fn replace_many(&mut self, hits: &[(usize, usize, String)], fmt: &FindFormat) {
        let old = std::mem::take(&mut self.atoms);
        let mut out: Vec<Atom> = Vec::with_capacity(old.len());
        let mut restyled: Vec<(usize, usize)> = Vec::new();
        let mut prev = 0usize;
        for (a, b, text) in hits {
            let a = (*a).clamp(prev, old.len());
            let b = (*b).clamp(a, old.len());
            out.extend(old.get(prev..a).unwrap_or_default().iter().cloned());
            let base = old.get(a).or_else(|| a.checked_sub(1).and_then(|j| old.get(j))).map(|x| x.attr);
            let base = match base {
                Some(x) => x,
                None => self.intern(RunAttr::default()),
            };
            let attr = if fmt.is_empty() {
                base
            } else if let Some((_, n)) = restyled.iter().find(|(from, _)| *from == base) {
                *n
            } else {
                let mut ra = self.attrs.get(base).cloned().unwrap_or_default();
                fmt.apply(&mut ra.style);
                let n = self.intern(ra);
                restyled.push((base, n));
                n
            };
            out.extend(
                text.chars().map(|c| Atom {
                    kind: if c == '\n' { AtomKind::Break } else { AtomKind::Char(c) },
                    attr,
                }),
            );
            prev = b;
        }
        out.extend(old.get(prev..).unwrap_or_default().iter().cloned());
        self.atoms = out;
    }

    /// Replaces atoms `a..b` with `text` (`\n` = a line break) in the style of the replaced text.
    fn replace_rich(&mut self, a: usize, b: usize, text: &str) {
        self.replace_range(a, b, text);
        let end = a.saturating_add(text.chars().count()).min(self.atoms.len());
        for atom in self.atoms.get_mut(a..end).unwrap_or_default() {
            if atom.kind == AtomKind::Char('\n') {
                atom.kind = AtomKind::Break;
            }
        }
    }
}

impl ComposeState {
    /// The leaves and char ranges a search covers: everything, or the selection.
    fn search_scope(&self, opts: &FindOptions) -> Vec<(Vec<usize>, usize, usize)> {
        if opts.in_selection && self.has_selection() {
            super::leaf_ranges(&self.body.blocks, self.selection.start(), self.selection.end())
        } else {
            leaves(&self.body.blocks).into_iter().map(|p| (p, 0, usize::MAX)).collect()
        }
    }

    /// Selects the next match of `needle` (in `opts.direction`, wrapping round). `Ok(false)` when there is
    /// none; `Err` for a pattern that cannot be used.
    pub fn find_with(&mut self, needle: &str, opts: &FindOptions) -> Result<bool, String> {
        self.ensure_body();
        let pat = Pattern::compile(needle, opts)?;
        let all = leaves(&self.body.blocks);
        let n = all.len();
        if n == 0 {
            return Ok(false);
        }
        let up = opts.direction == FindDirection::Up;
        let mut steps = 0usize;
        let at = if up { self.selection.start().clone() } else { self.selection.end().clone() };
        let k0 = all.iter().position(|p| *p == at.path).unwrap_or(0);
        for step in 0..=n {
            let k = if up {
                k0.saturating_add(n.saturating_mul(2)).saturating_sub(step) % n
            } else {
                k0.saturating_add(step) % n
            };
            let Some(path) = all.get(k) else { continue };
            let Some((kind, flat)) = leaf(&self.body.blocks, path) else { continue };
            if kind == LeafKind::Rule {
                continue;
            }
            let chars = chars_of(&flat);
            let hit = if up {
                let limit = if step == 0 { at.offset } else { flat.len() };
                search(&flat, &chars, &pat, opts, 0, Some(limit), &mut steps)?
            } else {
                search(&flat, &chars, &pat, opts, if step == 0 { at.offset } else { 0 }, None, &mut steps)?
            };
            if let Some((a, b)) = hit {
                // The wrapped-round pass of the starting paragraph only covers what the first pass did not.
                if step == n && ((!up && a >= at.offset) || (up && b <= at.offset)) {
                    break;
                }
                self.selection = BodySelection {
                    anchor: DocPos::new(path.clone(), a),
                    focus: DocPos::new(path.clone(), b),
                };
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Every match of `needle` (in the selection with `in_selection`), at most [`MAX_FIND_ALL`].
    pub fn find_all_with(&self, needle: &str, opts: &FindOptions) -> Result<Vec<BodySelection>, String> {
        let pat = Pattern::compile(needle, opts)?;
        let mut out = Vec::new();
        let mut steps = 0usize;
        for (path, from, to) in self.search_scope(opts) {
            let Some((kind, flat)) = leaf(&self.body.blocks, &path) else { continue };
            if kind == LeafKind::Rule {
                continue;
            }
            let chars = chars_of(&flat);
            let mut i = from;
            while out.len() < MAX_FIND_ALL {
                let Some((a, b)) = search(&flat, &chars, &pat, opts, i, None, &mut steps)? else { break };
                if b > to {
                    break;
                }
                out.push(BodySelection {
                    anchor: DocPos::new(path.clone(), a),
                    focus: DocPos::new(path.clone(), b),
                });
                i = b;
            }
        }
        Ok(out)
    }

    /// Find In: selects every match (the first as the selection, the rest as more ranges). The count.
    pub fn select_all_matches(&mut self, needle: &str, opts: &FindOptions) -> Result<usize, String> {
        let hits = self.find_all_with(needle, opts)?;
        let n = hits.len();
        let mut it = hits.into_iter();
        if let Some(first) = it.next() {
            self.selection = first;
            self.extra_ranges = it.collect();
        }
        Ok(n)
    }

    /// Replace All with options; the count.
    pub fn replace_all_with(
        &mut self,
        needle: &str,
        with: &str,
        opts: &FindOptions,
    ) -> Result<usize, String> {
        self.ensure_body();
        let pat = Pattern::compile(needle, opts)?;
        expand_replacement(with, "")?;
        let mut count = 0usize;
        let mut steps = 0usize;
        let mut added = 0usize;
        let mut failed: Option<String> = None;
        for (path, from, to) in self.search_scope(opts) {
            if failed.is_some() {
                break;
            }
            let Some((kind, mut flat)) = leaf(&self.body.blocks, &path) else { continue };
            if matches!(kind, LeafKind::Rule | LeafKind::Pre) {
                continue;
            }
            // The matches are found in the paragraph as it was, then put in with one pass over it (a
            // splice per match made a long paragraph quadratic).
            let chars = chars_of(&flat);
            let mut hits: Vec<(usize, usize, String)> = Vec::new();
            let mut i = from;
            while count < MAX_REPLACEMENTS {
                let (a, b) = match search(&flat, &chars, &pat, opts, i, None, &mut steps) {
                    Ok(Some(hit)) => hit,
                    Ok(None) => break,
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                };
                if b > to {
                    break;
                }
                let found: String = chars.get(a..b).map(|s| s.iter().collect()).unwrap_or_default();
                let rep = match expand_replacement(with, &found) {
                    Ok(r) => r,
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                };
                added = added.saturating_add(rep.chars().count());
                if added > MAX_REPLACE_ALL_CHARS {
                    failed = Some(format!(
                        "Replace All stopped after {count} replacements: it would add more than \
                         {MAX_REPLACE_ALL_CHARS} characters."
                    ));
                    break;
                }
                hits.push((a, b, rep));
                count += 1;
                i = b;
            }
            if !hits.is_empty() {
                flat.replace_many(&hits, &opts.replace_format);
                store_leaf(&mut self.body.blocks, &path, &flat);
            }
        }
        if count > 0 {
            self.dirty = true;
            self.ensure_body();
        }
        // What was replaced before a stop stays (one undo step takes it all back).
        match failed {
            Some(e) => Err(e),
            None => Ok(count),
        }
    }

    /// Replace: replaces the selection when it is a match, then finds the next one. (replaced, found).
    pub fn replace_next_with(
        &mut self,
        needle: &str,
        with: &str,
        opts: &FindOptions,
    ) -> Result<(bool, bool), String> {
        self.ensure_body();
        let pat = Pattern::compile(needle, opts)?;
        let start = self.selection.start().clone();
        let end = self.selection.end().clone();
        let mut replaced = false;
        if start.path == end.path
            && start.offset < end.offset
            && let Some((kind, mut flat)) = leaf(&self.body.blocks, &start.path)
            && !matches!(kind, LeafKind::Rule | LeafKind::Pre)
        {
            let chars = chars_of(&flat);
            let mut steps = 0usize;
            let is_match = pat.match_at(&chars, start.offset, opts, &mut steps) == Some(end.offset)
                && formatted(&flat, start.offset, end.offset, &opts.format);
            if is_match {
                let found: String =
                    chars.get(start.offset..end.offset).map(|s| s.iter().collect()).unwrap_or_default();
                let rep = expand_replacement(with, &found)?;
                let n = rep.chars().count();
                flat.replace_rich(start.offset, end.offset, &rep);
                if !opts.replace_format.is_empty() {
                    let f = opts.replace_format.clone();
                    flat.restyle(start.offset, start.offset + n, &mut |r| f.apply(&mut r.style));
                }
                store_leaf(&mut self.body.blocks, &start.path, &flat);
                let caret = if opts.direction == FindDirection::Up { start.offset } else { start.offset + n };
                self.selection = BodySelection::caret(DocPos::new(start.path.clone(), caret));
                self.dirty = true;
                replaced = true;
            }
        }
        let found = self.find_with(needle, opts)?;
        Ok((replaced, found))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(hay: &str, needle: &str, opts: &FindOptions) -> Vec<(usize, usize)> {
        let pat = Pattern::compile(needle, opts).unwrap();
        let flat = Flat::from_text(hay);
        let chars = chars_of(&flat);
        let mut out = Vec::new();
        let mut i = 0;
        let mut steps = 0;
        while let Ok(Some((a, b))) = search(&flat, &chars, &pat, opts, i, None, &mut steps) {
            out.push((a, b));
            i = b;
        }
        out
    }

    #[test]
    fn options_change_what_matches() {
        let plain = FindOptions::default();
        assert_eq!(find("Cat cat concat", "cat", &plain).len(), 3);
        assert_eq!(find("Cat cat concat", "cat", &FindOptions::case(true)).len(), 2);
        let whole = FindOptions { whole_word: true, ..FindOptions::default() };
        assert_eq!(find("Cat cat concat", "cat", &whole), vec![(0, 3), (4, 7)]);
        let prefix = FindOptions { prefix: true, ..FindOptions::default() };
        assert_eq!(find("concat catalog", "cat", &prefix), vec![(7, 10)]);
        let suffix = FindOptions { suffix: true, ..FindOptions::default() };
        assert_eq!(find("concat catalog", "cat", &suffix), vec![(3, 6)]);
    }

    #[test]
    fn specials_and_wildcards() {
        let o = FindOptions::default();
        assert_eq!(find("a\tb", "a^tb", &o), vec![(0, 3)]);
        assert_eq!(find("x1 y22", "^#", &o).len(), 3);
        assert_eq!(find("ab 12", "^$^$", &o), vec![(0, 2)]);
        assert_eq!(find("a   b", "a^wb", &o), vec![(0, 5)]);
        assert_eq!(find("2^3", "2^^3", &o), vec![(0, 3)]);
        assert!(Pattern::compile("x^p", &o).is_err());
        assert!(Pattern::compile("x^", &o).is_err());
        assert!(Pattern::compile("^z", &o).is_err());
        let w = FindOptions { wildcards: true, ..FindOptions::default() };
        assert_eq!(find("cat cot cut", "c?t", &w).len(), 3);
        assert_eq!(find("cat cot cut", "c[ao]t", &w).len(), 2);
        assert_eq!(find("cat cot cut", "c[!ao]t", &w), vec![(8, 11)]);
        assert_eq!(find("b1 b22 b333", "b[0-9]{2}", &w), vec![(3, 6), (7, 10)]);
        assert_eq!(find("b1 b22 b333", "b[0-9]@", &w).len(), 3);
        assert_eq!(find("start middle end", "s*e", &w), vec![(0, 12)]);
        assert_eq!(find("the then other", "<the>", &w), vec![(0, 3)]);
        assert_eq!(find("a*b", "a\\*b", &w), vec![(0, 3)]);
        // Wildcards are case-sensitive.
        assert_eq!(find("Cat cat", "cat", &w), vec![(4, 7)]);
        for bad in ["[a", "a{", "{2}", "@", "[z-a]", "x{0}", "x{5,2}", "\\"] {
            assert!(Pattern::compile(bad, &w).is_err(), "{bad}");
        }
    }

    #[test]
    fn replacement_codes() {
        assert_eq!(expand_replacement("[^&]", "x").unwrap(), "[x]");
        assert_eq!(expand_replacement("a^tb^lc^^", "").unwrap(), "a\tb\nc^");
        assert!(expand_replacement("^p", "").is_err());
        assert!(expand_replacement("^", "").is_err());
    }

    #[test]
    fn hostile_patterns_stay_bounded() {
        let w = FindOptions { wildcards: true, ..FindOptions::default() };
        let hay = "a".repeat(3000);
        let started = std::time::Instant::now();
        let _ = find(&hay, "*a*a*a*a*a*a*b", &w);
        let _ = find(&hay, "[a]{1,1000}[a]{1,1000}b", &w);
        assert!(started.elapsed().as_secs() < 20);
        assert!(Pattern::compile(&"x".repeat(300), &w).is_err());
        assert!(Pattern::compile("a{99999}", &w).is_err());
    }

    fn state(lines: &[String]) -> ComposeState {
        use mailcraft_model::{Block, Doc};
        let mut c = ComposeState {
            body: Doc {
                blocks: lines.iter().map(|l| Block::text_paragraph(l)).collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        c.ensure_body();
        c
    }

    #[test]
    fn one_step_budget_covers_the_whole_command() {
        // Regression: the budget restarted for every paragraph and every match, and the ^w scan was not
        // counted, so `^w^wx` over a long run of spaces took minutes.
        let o = FindOptions::default();
        let mut c = state(&[" ".repeat(100_000)]);
        let started = std::time::Instant::now();
        assert!(c.find_with("^w^wx", &o).is_err());
        assert!(c.find_all_with("^w^wx", &o).is_err());
        assert!(c.replace_all_with("^w^wx", "y", &o).is_err());
        assert!(started.elapsed().as_secs() < 30, "{:?}", started.elapsed());
        // Many paragraphs share one budget too.
        let w = FindOptions { wildcards: true, ..FindOptions::default() };
        let lines: Vec<String> = (0..200).map(|_| "a".repeat(2000)).collect();
        let c = state(&lines);
        let started = std::time::Instant::now();
        assert!(c.find_all_with("[a]{1,1000}[a]{1,1000}b", &w).is_err());
        assert!(started.elapsed().as_secs() < 30, "{:?}", started.elapsed());
        // An ordinary search over the same body still works.
        assert_eq!(c.find_all_with("aaaa", &o).map(|v| v.len()), Ok(MAX_FIND_ALL));
    }

    #[test]
    fn replacements_are_capped() {
        // Regression: `^&` many times over a long match built huge strings, once per replacement.
        assert!(expand_replacement(&"x".repeat(MAX_REPLACE_CHARS + 1), "").is_err());
        let found = "f".repeat(1000);
        assert!(expand_replacement(&"^&".repeat(100), &found).is_err());
        assert_eq!(expand_replacement(&"^&".repeat(60), &found).map(|s| s.len()), Ok(60_000));
        assert!(check_find("a", Some(&"^&".repeat(200)), &FindOptions::default()).is_err());
        // Replace All stops once it would add too much text in all.
        let mut c = state(&["a".repeat(100_000)]);
        let r = c.replace_all_with("a", &"^&".repeat(60), &FindOptions::default());
        assert!(r.is_err(), "{r:?}");
        let mut c = state(&["one two one".to_owned()]);
        assert_eq!(c.replace_all_with("one", "[^&]", &FindOptions::default()), Ok(2));
        assert_eq!(c.plain_text_with_list_formats().trim_end(), "[one] two [one]");
    }
}
