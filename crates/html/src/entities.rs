//! Character references: named (the HTML 4 set plus common HTML5 additions) and numeric.
//!
//! Rules follow the WHATWG tokenizer: numeric references map NUL, surrogates and out-of-range values to
//! U+FFFD and the C1 range through the windows-1252 table; the Latin-1 names (plus `amp`, `lt`, `gt`,
//! `quot`) are also recognised without a trailing semicolon, except inside attribute values when the next
//! character is `=` or alphanumeric.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Names for U+00A0..=U+00FF in code point order.
const LATIN1: [&str; 96] = [
    "nbsp", "iexcl", "cent", "pound", "curren", "yen", "brvbar", "sect", "uml", "copy", "ordf", "laquo",
    "not", "shy", "reg", "macr", "deg", "plusmn", "sup2", "sup3", "acute", "micro", "para", "middot",
    "cedil", "sup1", "ordm", "raquo", "frac14", "frac12", "frac34", "iquest", "Agrave", "Aacute", "Acirc",
    "Atilde", "Auml", "Aring", "AElig", "Ccedil", "Egrave", "Eacute", "Ecirc", "Euml", "Igrave", "Iacute",
    "Icirc", "Iuml", "ETH", "Ntilde", "Ograve", "Oacute", "Ocirc", "Otilde", "Ouml", "times", "Oslash",
    "Ugrave", "Uacute", "Ucirc", "Uuml", "Yacute", "THORN", "szlig", "agrave", "aacute", "acirc", "atilde",
    "auml", "aring", "aelig", "ccedil", "egrave", "eacute", "ecirc", "euml", "igrave", "iacute", "icirc",
    "iuml", "eth", "ntilde", "ograve", "oacute", "ocirc", "otilde", "ouml", "divide", "oslash", "ugrave",
    "uacute", "ucirc", "uuml", "yacute", "thorn", "yuml",
];

/// Greek capitals U+0391..=U+03A9 (U+03A2 is unassigned).
const GREEK_UPPER: [&str; 25] = [
    "Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta", "Eta", "Theta", "Iota", "Kappa", "Lambda", "Mu",
    "Nu", "Xi", "Omicron", "Pi", "Rho", "", "Sigma", "Tau", "Upsilon", "Phi", "Chi", "Psi", "Omega",
];

/// Greek small letters U+03B1..=U+03C9.
const GREEK_LOWER: [&str; 25] = [
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa", "lambda", "mu",
    "nu", "xi", "omicron", "pi", "rho", "sigmaf", "sigma", "tau", "upsilon", "phi", "chi", "psi", "omega",
];

/// Everything else, as (name, code point).
#[rustfmt::skip]
const OTHER: &[(&str, u32)] = &[
    ("quot", 0x22), ("amp", 0x26), ("apos", 0x27), ("lt", 0x3C), ("gt", 0x3E), ("QUOT", 0x22), ("AMP", 0x26),
    ("LT", 0x3C), ("GT", 0x3E), ("COPY", 0xA9), ("REG", 0xAE), ("Tab", 0x09), ("NewLine", 0x0A),
    ("excl", 0x21), ("num", 0x23), ("dollar", 0x24), ("percnt", 0x25), ("lpar", 0x28), ("rpar", 0x29),
    ("ast", 0x2A), ("plus", 0x2B), ("comma", 0x2C), ("period", 0x2E), ("sol", 0x2F), ("colon", 0x3A),
    ("semi", 0x3B), ("equals", 0x3D), ("quest", 0x3F), ("commat", 0x40), ("lsqb", 0x5B), ("bsol", 0x5C),
    ("rsqb", 0x5D), ("Hat", 0x5E), ("lowbar", 0x5F), ("grave", 0x60), ("lcub", 0x7B), ("verbar", 0x7C),
    ("vert", 0x7C), ("rcub", 0x7D), ("NonBreakingSpace", 0xA0), ("half", 0xBD), ("centerdot", 0xB7),
    ("OElig", 0x152), ("oelig", 0x153), ("Scaron", 0x160), ("scaron", 0x161), ("Yuml", 0x178),
    ("fnof", 0x192), ("circ", 0x2C6), ("tilde", 0x2DC), ("thetasym", 0x3D1), ("upsih", 0x3D2), ("piv", 0x3D6),
    ("ensp", 0x2002), ("emsp", 0x2003), ("emsp13", 0x2004), ("emsp14", 0x2005), ("numsp", 0x2007),
    ("puncsp", 0x2008), ("thinsp", 0x2009), ("hairsp", 0x200A), ("ZeroWidthSpace", 0x200B), ("zwnj", 0x200C),
    ("zwj", 0x200D), ("lrm", 0x200E), ("rlm", 0x200F), ("hyphen", 0x2010), ("dash", 0x2010),
    ("ndash", 0x2013), ("mdash", 0x2014), ("horbar", 0x2015), ("Verbar", 0x2016), ("lsquo", 0x2018),
    ("rsquo", 0x2019), ("rsquor", 0x2019), ("sbquo", 0x201A), ("lsquor", 0x201A), ("ldquo", 0x201C),
    ("rdquo", 0x201D), ("rdquor", 0x201D), ("bdquo", 0x201E), ("ldquor", 0x201E), ("dagger", 0x2020),
    ("Dagger", 0x2021), ("bull", 0x2022), ("bullet", 0x2022), ("nldr", 0x2025), ("hellip", 0x2026),
    ("mldr", 0x2026), ("permil", 0x2030), ("prime", 0x2032), ("Prime", 0x2033), ("lsaquo", 0x2039),
    ("rsaquo", 0x203A), ("oline", 0x203E), ("frasl", 0x2044), ("euro", 0x20AC), ("image", 0x2111),
    ("weierp", 0x2118), ("real", 0x211C), ("trade", 0x2122), ("TRADE", 0x2122), ("alefsym", 0x2135),
    ("copysr", 0x2117), ("incare", 0x2105), ("numero", 0x2116), ("larr", 0x2190), ("uarr", 0x2191),
    ("rarr", 0x2192), ("darr", 0x2193), ("harr", 0x2194), ("crarr", 0x21B5), ("lArr", 0x21D0),
    ("uArr", 0x21D1), ("rArr", 0x21D2), ("dArr", 0x21D3), ("hArr", 0x21D4), ("forall", 0x2200),
    ("part", 0x2202), ("exist", 0x2203), ("empty", 0x2205), ("nabla", 0x2207), ("isin", 0x2208),
    ("notin", 0x2209), ("ni", 0x220B), ("prod", 0x220F), ("sum", 0x2211), ("minus", 0x2212),
    ("lowast", 0x2217), ("radic", 0x221A), ("prop", 0x221D), ("infin", 0x221E), ("ang", 0x2220),
    ("and", 0x2227), ("or", 0x2228), ("cap", 0x2229), ("cup", 0x222A), ("int", 0x222B), ("there4", 0x2234),
    ("sim", 0x223C), ("cong", 0x2245), ("asymp", 0x2248), ("ne", 0x2260), ("equiv", 0x2261), ("le", 0x2264),
    ("ge", 0x2265), ("sub", 0x2282), ("sup", 0x2283), ("nsub", 0x2284), ("sube", 0x2286), ("supe", 0x2287),
    ("oplus", 0x2295), ("otimes", 0x2297), ("perp", 0x22A5), ("sdot", 0x22C5), ("lceil", 0x2308),
    ("rceil", 0x2309), ("lfloor", 0x230A), ("rfloor", 0x230B), ("lang", 0x27E8), ("rang", 0x27E9),
    ("loz", 0x25CA), ("squ", 0x25A1), ("square", 0x25A1), ("blacksquare", 0x25AA), ("star", 0x2606),
    ("starf", 0x2605), ("phone", 0x260E), ("female", 0x2640), ("male", 0x2642), ("spades", 0x2660),
    ("clubs", 0x2663), ("hearts", 0x2665), ("diams", 0x2666), ("sung", 0x266A), ("flat", 0x266D),
    ("natural", 0x266E), ("sharp", 0x266F), ("check", 0x2713), ("checkmark", 0x2713), ("cross", 0x2717),
    ("malt", 0x2720), ("sext", 0x2736), ("VerticalLine", 0x7C),
];

/// Windows-1252 interpretation of numeric references in U+0080..=U+009F.
const C1: [u32; 32] = [
    0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152,
    0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161,
    0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
];

/// Longest entity name we look at.
const MAX_NAME: usize = 32;
/// Longest name honoured without a semicolon (`frac12`, `Agrave`, …).
const MAX_LEGACY_NAME: usize = 6;

fn find_in(names: &[&str], base: u32, name: &str) -> Option<char> {
    let i = names.iter().position(|n| !n.is_empty() && *n == name)?;
    char::from_u32(base + u32::try_from(i).ok()?)
}

fn table() -> &'static HashMap<&'static str, char> {
    static TABLE: OnceLock<HashMap<&'static str, char>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut m = HashMap::with_capacity(LATIN1.len() + 64 + OTHER.len());
        let seq = [(&LATIN1[..], 0xA0u32), (&GREEK_UPPER[..], 0x391), (&GREEK_LOWER[..], 0x3B1)];
        for (names, base) in seq {
            for (i, n) in names.iter().enumerate() {
                if let (false, Ok(i)) = (n.is_empty(), u32::try_from(i))
                    && let Some(c) = char::from_u32(base + i)
                {
                    m.entry(*n).or_insert(c);
                }
            }
        }
        for (n, cp) in OTHER {
            if let Some(c) = char::from_u32(*cp) {
                m.entry(*n).or_insert(c);
            }
        }
        m
    })
}

/// Looks up a named reference (without `&` and `;`).
pub(crate) fn lookup(name: &str) -> Option<char> {
    table().get(name).copied()
}

/// Names that are honoured without a trailing semicolon (legacy behaviour).
fn lookup_legacy(name: &str) -> Option<char> {
    if let Some(c) = find_in(&LATIN1, 0xA0, name) {
        return Some(c);
    }
    match name {
        "amp" | "AMP" => Some('&'),
        "lt" | "LT" => Some('<'),
        "gt" | "GT" => Some('>'),
        "quot" | "QUOT" => Some('"'),
        "COPY" => Some('\u{A9}'),
        "REG" => Some('\u{AE}'),
        _ => None,
    }
}

/// Maps a numeric reference value to the character it produces.
pub(crate) fn numeric_char(v: u32) -> char {
    match v {
        0 => '\u{FFFD}',
        0x80..=0x9F => {
            let idx = usize::try_from(v - 0x80).unwrap_or(0);
            C1.get(idx).and_then(|&cp| char::from_u32(cp)).unwrap_or('\u{FFFD}')
        }
        _ => char::from_u32(v).unwrap_or('\u{FFFD}'),
    }
}

/// Decodes a character reference. `s` is the text immediately after the `&`. Returns the character and
/// the number of bytes of `s` consumed, or `None` when the `&` should be kept literally.
pub(crate) fn decode_ref(s: &str, in_attr: bool) -> Option<(char, usize)> {
    let b = s.as_bytes();
    if b.first() == Some(&b'#') {
        let (hex, start) = match b.get(1) {
            Some(b'x' | b'X') => (true, 2),
            _ => (false, 1),
        };
        let mut i = start;
        let mut v: u32 = 0;
        while let Some(&c) = b.get(i) {
            let d = match (hex, c) {
                (_, b'0'..=b'9') => u32::from(c - b'0'),
                (true, b'a'..=b'f') => u32::from(c - b'a' + 10),
                (true, b'A'..=b'F') => u32::from(c - b'A' + 10),
                _ => break,
            };
            v = v.saturating_mul(if hex { 16 } else { 10 }).saturating_add(d).min(0x11_0000);
            i += 1;
        }
        if i == start {
            return None;
        }
        if b.get(i) == Some(&b';') {
            i += 1;
        }
        return Some((numeric_char(v), i));
    }
    let run = b.iter().take(MAX_NAME).take_while(|c| c.is_ascii_alphanumeric()).count();
    if run == 0 {
        return None;
    }
    let name = s.get(..run)?;
    if b.get(run) == Some(&b';')
        && let Some(c) = lookup(name)
    {
        return Some((c, run + 1));
    }
    // Legacy: longest known prefix without a semicolon.
    for len in (2..=run.min(MAX_LEGACY_NAME)).rev() {
        let prefix = s.get(..len)?;
        if let Some(c) = lookup_legacy(prefix) {
            let next = b.get(len).copied();
            if b.get(len) == Some(&b';') {
                return Some((c, len + 1));
            }
            if in_attr && next.is_some_and(|n| n == b'=' || n.is_ascii_alphanumeric()) {
                return None;
            }
            return Some((c, len));
        }
    }
    None
}

/// Decodes all character references in `s`. NUL becomes U+FFFD in attributes and is dropped in text.
pub(crate) fn decode(s: &str, in_attr: bool) -> String {
    if !s.contains('&') && !s.contains('\0') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find(['&', '\0']) {
        out.push_str(rest.get(..pos).unwrap_or(""));
        let tail = rest.get(pos + 1..).unwrap_or("");
        if rest.as_bytes().get(pos) == Some(&0) {
            if in_attr {
                out.push('\u{FFFD}');
            }
            rest = tail;
            continue;
        }
        match decode_ref(tail, in_attr) {
            Some((c, used)) => {
                out.push(c);
                rest = tail.get(used..).unwrap_or("");
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named() {
        assert_eq!(decode("a &amp; b &lt;x&gt; &quot;q&quot; &apos;", false), "a & b <x> \"q\" '");
        assert_eq!(decode("&nbsp;&copy;&eacute;&yuml;&euro;&hellip;&mdash;", false), "\u{a0}©éÿ€…—");
        assert_eq!(decode("&Alpha;&Omega;&alpha;&sigmaf;&omega;", false), "ΑΩαςω");
        assert_eq!(decode("&rsquo;&ldquo;&rdquo;&bull;&trade;&rarr;", false), "’“”•™→");
        assert_eq!(decode("&unknown; &", false), "&unknown; &");
        assert_eq!(decode("&#;&#x;&", false), "&#;&#x;&");
    }

    #[test]
    fn legacy_without_semicolon() {
        assert_eq!(decode("&amp &copy2020 &notit;", false), "& ©2020 ¬it;");
        assert_eq!(decode("?a=1&copy=2", true), "?a=1&copy=2");
        assert_eq!(decode("?a=1&copy 2", true), "?a=1© 2");
        assert_eq!(decode("&ampx", true), "&ampx");
        assert_eq!(decode("&rsquo", false), "&rsquo");
    }

    #[test]
    fn numeric() {
        assert_eq!(decode("&#65;&#x42;&#X43;&#0068", false), "ABCD");
        assert_eq!(decode("&#0;&#xD800;&#x110000;&#99999999999;", false), "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}");
        assert_eq!(decode("&#x80;&#150;&#x9F;&#x81;", false), "€–Ÿ\u{81}");
        assert_eq!(decode("&#x1F600;", false), "😀");
    }

    #[test]
    fn table_is_consistent() {
        assert_eq!(lookup("nbsp"), Some('\u{A0}'));
        assert_eq!(lookup("yuml"), Some('ÿ'));
        assert_eq!(lookup("Sigma"), Some('Σ'));
        assert_eq!(lookup("Omega"), Some('Ω'));
        assert_eq!(lookup(""), None);
        let count = LATIN1.len() + GREEK_UPPER.len() - 1 + GREEK_LOWER.len() + OTHER.len();
        assert!(count >= 250, "{count}");
    }

    #[test]
    fn nul_handling() {
        assert_eq!(decode("a\0b", false), "ab");
        assert_eq!(decode("a\0b", true), "a\u{FFFD}b");
    }
}
