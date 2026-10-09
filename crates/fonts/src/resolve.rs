//! CSS `font-family` list parsing, the alias table and CSS-style weight/style matching.

/// Bundled family names.
pub(crate) const INTER: &str = "Inter";
pub(crate) const LIB_SANS: &str = "Liberation Sans";
pub(crate) const LIB_SERIF: &str = "Liberation Serif";
pub(crate) const LIB_MONO: &str = "Liberation Mono";
pub(crate) const CARLITO: &str = "Carlito";

/// Most family entries considered from one list, and longest list text parsed.
const MAX_ENTRIES: usize = 64;
const MAX_LIST_LEN: usize = 8 * 1024;
const MAX_NAME_LEN: usize = 128;

/// Lower-case family name (or generic keyword) → bundled family. Names are compared after whitespace collapsing.
const ALIASES: &[(&str, &str)] = &[
    // UI / modern sans → Inter
    ("inter", INTER),
    ("aptos", INTER),
    ("aptos display", INTER),
    ("aptos narrow", INTER),
    ("segoe ui", INTER),
    ("segoe ui variable", INTER),
    ("segoe ui variable text", INTER),
    ("segoe ui variable display", INTER),
    ("system-ui", INTER),
    ("-apple-system", INTER),
    ("blinkmacsystemfont", INTER),
    ("ui-sans-serif", INTER),
    ("ui-rounded", INTER),
    ("sf pro", INTER),
    ("sf pro text", INTER),
    ("sf pro display", INTER),
    ("san francisco", INTER),
    ("roboto", INTER),
    ("open sans", INTER),
    ("noto sans", INTER),
    ("ubuntu", INTER),
    ("cantarell", INTER),
    ("selawik", INTER),
    // Calibri family → Carlito (metric-compatible)
    ("carlito", CARLITO),
    ("calibri", CARLITO),
    ("candara", CARLITO),
    ("corbel", CARLITO),
    // Arial family → Liberation Sans (metric-compatible)
    ("liberation sans", LIB_SANS),
    ("arimo", LIB_SANS),
    ("arial", LIB_SANS),
    ("arial nova", LIB_SANS),
    ("arial narrow", LIB_SANS),
    ("arial unicode ms", LIB_SANS),
    ("helvetica", LIB_SANS),
    ("helvetica neue", LIB_SANS),
    ("verdana", LIB_SANS),
    ("tahoma", LIB_SANS),
    ("trebuchet ms", LIB_SANS),
    ("microsoft sans serif", LIB_SANS),
    ("ms sans serif", LIB_SANS),
    ("lucida sans", LIB_SANS),
    ("lucida sans unicode", LIB_SANS),
    ("lucida grande", LIB_SANS),
    ("century gothic", LIB_SANS),
    ("franklin gothic", LIB_SANS),
    ("franklin gothic book", LIB_SANS),
    ("gill sans", LIB_SANS),
    ("gill sans mt", LIB_SANS),
    ("dejavu sans", LIB_SANS),
    ("sans-serif", LIB_SANS),
    ("sans serif", LIB_SANS),
    // Times family → Liberation Serif (metric-compatible)
    ("liberation serif", LIB_SERIF),
    ("tinos", LIB_SERIF),
    ("times", LIB_SERIF),
    ("times new roman", LIB_SERIF),
    ("georgia", LIB_SERIF),
    ("cambria", LIB_SERIF),
    ("constantia", LIB_SERIF),
    ("garamond", LIB_SERIF),
    ("book antiqua", LIB_SERIF),
    ("palatino", LIB_SERIF),
    ("palatino linotype", LIB_SERIF),
    ("bookman old style", LIB_SERIF),
    ("century schoolbook", LIB_SERIF),
    ("baskerville", LIB_SERIF),
    ("aptos serif", LIB_SERIF),
    ("aptos slab", LIB_SERIF),
    ("rockwell", LIB_SERIF),
    ("dejavu serif", LIB_SERIF),
    ("serif", LIB_SERIF),
    ("ui-serif", LIB_SERIF),
    // Courier family → Liberation Mono (metric-compatible)
    ("liberation mono", LIB_MONO),
    ("cousine", LIB_MONO),
    ("courier", LIB_MONO),
    ("courier new", LIB_MONO),
    ("consolas", LIB_MONO),
    ("lucida console", LIB_MONO),
    ("cascadia code", LIB_MONO),
    ("cascadia mono", LIB_MONO),
    ("aptos mono", LIB_MONO),
    ("menlo", LIB_MONO),
    ("monaco", LIB_MONO),
    ("sf mono", LIB_MONO),
    ("dejavu sans mono", LIB_MONO),
    ("source code pro", LIB_MONO),
    ("monospace", LIB_MONO),
    ("ui-monospace", LIB_MONO),
];

/// Trailing style words in a family name ("Segoe UI Semibold", "Arial Black", "Calibri Light Italic").
const STYLE_WORDS: &[(&str, Option<u16>, bool)] = &[
    ("thin", Some(100), false),
    ("hairline", Some(100), false),
    ("extralight", Some(200), false),
    ("ultralight", Some(200), false),
    ("light", Some(300), false),
    ("semilight", Some(350), false),
    ("regular", Some(400), false),
    ("normal", Some(400), false),
    ("book", Some(400), false),
    ("medium", Some(500), false),
    ("semibold", Some(600), false),
    ("demibold", Some(600), false),
    ("bold", Some(700), false),
    ("extrabold", Some(800), false),
    ("ultrabold", Some(800), false),
    ("black", Some(900), false),
    ("heavy", Some(900), false),
    ("extrablack", Some(950), false),
    ("ultrablack", Some(950), false),
    ("italic", None, true),
    ("oblique", None, true),
];

/// A resolved family entry plus the style hints carried by its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FamilyMatch {
    pub family: &'static str,
    pub weight_hint: Option<u16>,
    pub italic_hint: bool,
}

/// Modifiers that combine with the following style word ("Semi Bold" = "semibold").
const STYLE_MODIFIERS: &[&str] = &["semi", "demi", "extra", "ultra"];

/// Splits a CSS family list on top-level commas, honouring single/double quotes and CSS backslash escapes
/// (CSS Syntax 3, "consume an escaped code point"). Returns normalised (lower-case, whitespace-collapsed)
/// names; empty entries are dropped.
pub(crate) fn split_families(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = list.chars().take(MAX_LIST_LEN).peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (_, '\\') => {
                if let Some(e) = unescape(&mut chars) {
                    cur.push(e);
                }
            }
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => quote = Some(c),
            (None, ',') => {
                push_name(&mut out, &cur);
                cur.clear();
                if out.len() >= MAX_ENTRIES {
                    return out;
                }
            }
            (None, c) => cur.push(c),
        }
        if cur.len() > MAX_LIST_LEN * 4 {
            break;
        }
    }
    push_name(&mut out, &cur);
    out
}

/// Decodes the escape after a backslash. `None` for an escaped newline (a line continuation) or end of input.
fn unescape(chars: &mut std::iter::Peekable<impl Iterator<Item = char>>) -> Option<char> {
    let first = chars.next()?;
    let Some(d) = first.to_digit(16) else {
        return (!matches!(first, '\n' | '\r' | '\u{0C}')).then_some(first);
    };
    let mut cp = d;
    for _ in 1..6 {
        let Some(d) = chars.peek().and_then(|c| c.to_digit(16)) else { break };
        cp = cp.saturating_mul(16).saturating_add(d);
        chars.next();
    }
    if chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
    }
    Some(match cp {
        0 => char::REPLACEMENT_CHARACTER,
        cp => char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER),
    })
}

fn push_name(out: &mut Vec<String>, raw: &str) {
    let name = collapse(raw, false);
    if !name.is_empty() && out.len() < MAX_ENTRIES {
        out.push(name);
    }
}

/// Lower-case, whitespace-collapsed characters of `raw` (also splitting on quotes when `strip_quotes`), capped
/// at [`MAX_NAME_LEN`] characters.
fn normalized_chars(raw: &str, strip_quotes: bool) -> impl Iterator<Item = char> + '_ {
    raw.split(move |c: char| c.is_whitespace() || (strip_quotes && (c == '"' || c == '\'')))
        .filter(|w| !w.is_empty())
        .enumerate()
        .flat_map(|(i, w)| (i > 0).then_some(' ').into_iter().chain(w.chars().flat_map(char::to_lowercase)))
        .take(MAX_NAME_LEN)
}

fn collapse(raw: &str, strip_quotes: bool) -> String {
    normalized_chars(raw, strip_quotes).collect()
}

/// Lower-cases, collapses whitespace, strips stray quotes and caps the length.
pub(crate) fn normalize(raw: &str) -> String {
    collapse(raw, true)
}

/// True when two family names are the same after case folding and whitespace collapsing.
pub(crate) fn same_family(a: &str, b: &str) -> bool {
    a == b || normalized_chars(a, false).eq(normalized_chars(b, false))
}

/// Strips a trailing parenthetical such as "(Body)" or "(Headings)".
fn strip_parenthetical(name: &str) -> &str {
    match (name.ends_with(')'), name.rfind('(')) {
        (true, Some(i)) => name.get(..i).map(str::trim_end).unwrap_or(name),
        _ => name,
    }
}

/// Looks a normalised name up directly in the alias table or among the db's own family names.
fn lookup(name: &str, extra: &[&'static str]) -> Option<&'static str> {
    if let Some(f) = extra.iter().find(|f| normalized_chars(f, false).eq(name.chars())) {
        return Some(f);
    }
    ALIASES.iter().find(|(alias, _)| *alias == name).map(|(_, fam)| *fam)
}

/// Maps one normalised family name to a bundled family, peeling style words off the end when needed.
pub(crate) fn match_family(name: &str, extra: &[&'static str]) -> Option<FamilyMatch> {
    let name = strip_parenthetical(name);
    if let Some(family) = lookup(name, extra) {
        return Some(FamilyMatch { family, weight_hint: None, italic_hint: false });
    }
    let mut words: Vec<&str> = name.split(' ').collect();
    let mut weight_hint = None;
    let mut italic_hint = false;
    while words.len() > 1 {
        let Some(&last) = words.last() else { break };
        let Some((_, mut w, it)) = STYLE_WORDS.iter().find(|(word, _, _)| *word == last).copied() else {
            break;
        };
        words.pop();
        // "Semi Bold", "Extra Light": fold a preceding modifier in, keeping at least one family word.
        if words.len() > 1
            && let Some(m) = words.last().copied().filter(|m| STYLE_MODIFIERS.contains(m))
            && let Some((_, mw, _)) =
                STYLE_WORDS.iter().find(|(word, _, _)| word.strip_prefix(m) == Some(last))
        {
            w = *mw;
            words.pop();
        }
        if weight_hint.is_none() {
            weight_hint = w;
        }
        italic_hint |= it;
        let base = words.join(" ");
        if let Some(family) = lookup(&base, extra) {
            return Some(FamilyMatch { family, weight_hint, italic_hint });
        }
    }
    None
}

/// Public wrapper: bundled family for a single (unnormalised) family name.
pub fn bundled_family_for(name: &str) -> Option<&'static str> {
    match_family(&normalize(name), &[]).map(|m| m.family)
}

/// CSS Fonts weight-matching order: lower is better. `want` is the requested weight, `have` a candidate.
pub(crate) fn weight_rank(want: u16, have: u16) -> (u8, u16) {
    let want = want.clamp(1, 1000);
    let diff = want.abs_diff(have);
    if (400..=500).contains(&want) {
        if have >= want && have <= 500 {
            (0, diff)
        } else if have < want {
            (1, diff)
        } else {
            (2, diff)
        }
    } else if want < 400 {
        if have <= want { (0, diff) } else { (1, diff) }
    } else if have >= want {
        (0, diff)
    } else {
        (1, diff)
    }
}
