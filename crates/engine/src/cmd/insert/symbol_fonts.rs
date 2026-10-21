//! Symbol fonts: the characters the classic symbol fonts hold at each code, as the Unicode characters that
//! stand for them, so Insert › Symbol can offer them ("Wingdings: 74") without the fonts themselves (which
//! MailCraft does not ship; text sent as Unicode reads the same everywhere).
//!
//! - **Symbol**: the Adobe Symbol encoding (Greek letters on the Latin letters, mathematical operators,
//!   arrows and card suits), from its published Unicode mapping.
//! - **Wingdings**: the codes whose pictures have a Unicode counterpart (Unicode 7.0 added most of them);
//!   codes without a confident one are left out rather than guessed.

/// The symbol fonts offered, besides "(normal text)".
pub const SYMBOL_FONTS: [&str; 2] = ["Symbol", "Wingdings"];

/// Symbol font: code → character. Printable ASCII codes not listed keep their own character.
const SYMBOL: &[(u8, char)] = &[
    (0x22, '\u{2200}'),
    (0x24, '\u{2203}'),
    (0x27, '\u{220B}'),
    (0x2A, '\u{2217}'),
    (0x2D, '\u{2212}'),
    (0x40, '\u{2245}'),
    (0x41, 'Α'),
    (0x42, 'Β'),
    (0x43, 'Χ'),
    (0x44, 'Δ'),
    (0x45, 'Ε'),
    (0x46, 'Φ'),
    (0x47, 'Γ'),
    (0x48, 'Η'),
    (0x49, 'Ι'),
    (0x4A, 'ϑ'),
    (0x4B, 'Κ'),
    (0x4C, 'Λ'),
    (0x4D, 'Μ'),
    (0x4E, 'Ν'),
    (0x4F, 'Ο'),
    (0x50, 'Π'),
    (0x51, 'Θ'),
    (0x52, 'Ρ'),
    (0x53, 'Σ'),
    (0x54, 'Τ'),
    (0x55, 'Υ'),
    (0x56, 'ς'),
    (0x57, 'Ω'),
    (0x58, 'Ξ'),
    (0x59, 'Ψ'),
    (0x5A, 'Ζ'),
    (0x5C, '\u{2234}'),
    (0x5E, '\u{22A5}'),
    (0x61, 'α'),
    (0x62, 'β'),
    (0x63, 'χ'),
    (0x64, 'δ'),
    (0x65, 'ε'),
    (0x66, 'φ'),
    (0x67, 'γ'),
    (0x68, 'η'),
    (0x69, 'ι'),
    (0x6A, 'ϕ'),
    (0x6B, 'κ'),
    (0x6C, 'λ'),
    (0x6D, 'μ'),
    (0x6E, 'ν'),
    (0x6F, 'ο'),
    (0x70, 'π'),
    (0x71, 'θ'),
    (0x72, 'ρ'),
    (0x73, 'σ'),
    (0x74, 'τ'),
    (0x75, 'υ'),
    (0x76, 'ϖ'),
    (0x77, 'ω'),
    (0x78, 'ξ'),
    (0x79, 'ψ'),
    (0x7A, 'ζ'),
    (0x7E, '\u{223C}'),
    (0xA1, 'ϒ'),
    (0xA2, '\u{2032}'),
    (0xA3, '\u{2264}'),
    (0xA4, '\u{2044}'),
    (0xA5, '\u{221E}'),
    (0xA6, 'ƒ'),
    (0xA7, '\u{2663}'),
    (0xA8, '\u{2666}'),
    (0xA9, '\u{2665}'),
    (0xAA, '\u{2660}'),
    (0xAB, '\u{2194}'),
    (0xAC, '\u{2190}'),
    (0xAD, '\u{2191}'),
    (0xAE, '\u{2192}'),
    (0xAF, '\u{2193}'),
    (0xB0, '°'),
    (0xB1, '±'),
    (0xB2, '\u{2033}'),
    (0xB3, '\u{2265}'),
    (0xB4, '×'),
    (0xB5, '\u{221D}'),
    (0xB6, '\u{2202}'),
    (0xB7, '\u{2022}'),
    (0xB8, '÷'),
    (0xB9, '\u{2260}'),
    (0xBA, '\u{2261}'),
    (0xBB, '\u{2248}'),
    (0xBC, '\u{2026}'),
    (0xBF, '\u{21B5}'),
    (0xC0, '\u{2135}'),
    (0xC1, '\u{2111}'),
    (0xC2, '\u{211C}'),
    (0xC3, '\u{2118}'),
    (0xC4, '\u{2297}'),
    (0xC5, '\u{2295}'),
    (0xC6, '\u{2205}'),
    (0xC7, '\u{2229}'),
    (0xC8, '\u{222A}'),
    (0xC9, '\u{2283}'),
    (0xCA, '\u{2287}'),
    (0xCB, '\u{2284}'),
    (0xCC, '\u{2282}'),
    (0xCD, '\u{2286}'),
    (0xCE, '\u{2208}'),
    (0xCF, '\u{2209}'),
    (0xD0, '\u{2220}'),
    (0xD1, '\u{2207}'),
    (0xD2, '®'),
    (0xD3, '©'),
    (0xD4, '\u{2122}'),
    (0xD5, '\u{220F}'),
    (0xD6, '\u{221A}'),
    (0xD7, '\u{22C5}'),
    (0xD8, '¬'),
    (0xD9, '\u{2227}'),
    (0xDA, '\u{2228}'),
    (0xDB, '\u{21D4}'),
    (0xDC, '\u{21D0}'),
    (0xDD, '\u{21D1}'),
    (0xDE, '\u{21D2}'),
    (0xDF, '\u{21D3}'),
    (0xE0, '\u{25CA}'),
    (0xE1, '\u{2329}'),
    (0xE5, '\u{2211}'),
    (0xF1, '\u{232A}'),
    (0xF2, '\u{222B}'),
];

/// Wingdings: code → character (the codes with a well-known Unicode counterpart).
const WINGDINGS: &[(u8, char)] = &[
    (0x22, '\u{2702}'),
    (0x41, '\u{270C}'),
    (0x42, '\u{1F44C}'),
    (0x43, '\u{1F44D}'),
    (0x44, '\u{1F44E}'),
    (0x45, '\u{261C}'),
    (0x46, '\u{261E}'),
    (0x47, '\u{261D}'),
    (0x48, '\u{261F}'),
    (0x49, '\u{270B}'),
    (0x4A, '\u{263A}'),
    (0x4B, '\u{1F610}'),
    (0x4C, '\u{2639}'),
    (0x4D, '\u{1F4A3}'),
    (0x4E, '\u{2620}'),
    (0x51, '\u{2708}'),
    (0x52, '\u{263C}'),
    (0x53, '\u{1F4A7}'),
    (0x54, '\u{2744}'),
    (0x56, '\u{271E}'),
    (0x58, '\u{2720}'),
    (0x59, '\u{2721}'),
    (0x5A, '\u{262A}'),
    (0x5B, '\u{262F}'),
    (0x5C, '\u{0950}'),
    (0x5D, '\u{2638}'),
    (0x5E, '\u{2648}'),
    (0x5F, '\u{2649}'),
    (0x60, '\u{264A}'),
    (0x61, '\u{264B}'),
    (0x62, '\u{264C}'),
    (0x63, '\u{264D}'),
    (0x64, '\u{264E}'),
    (0x65, '\u{264F}'),
    (0x66, '\u{2650}'),
    (0x67, '\u{2651}'),
    (0x68, '\u{2652}'),
    (0x69, '\u{2653}'),
    (0x6C, '\u{25CF}'),
    (0x6E, '\u{25A0}'),
    (0x6F, '\u{25A1}'),
    (0x71, '\u{2751}'),
    (0x72, '\u{2752}'),
    (0x75, '\u{25C6}'),
    (0x76, '\u{2756}'),
    (0x7B, '\u{2740}'),
    (0x7C, '\u{273F}'),
    (0x7D, '\u{275D}'),
    (0x7E, '\u{275E}'),
    (0xA7, '\u{25AA}'),
    (0xAB, '\u{2605}'),
    (0xD8, '\u{27A2}'),
    (0xE8, '\u{2794}'),
    (0xEF, '\u{21E6}'),
    (0xF0, '\u{21E8}'),
    (0xF1, '\u{21E7}'),
    (0xF2, '\u{21E9}'),
    (0xFB, '\u{2717}'),
    (0xFC, '\u{2713}'),
    (0xFD, '\u{2612}'),
    (0xFE, '\u{2611}'),
];

fn table(font: &str) -> Option<&'static [(u8, char)]> {
    if font.eq_ignore_ascii_case("Symbol") {
        Some(SYMBOL)
    } else if font.eq_ignore_ascii_case("Wingdings") {
        Some(WINGDINGS)
    } else {
        None
    }
}

/// The character code `code` of symbol font `font` stands for. Symbol keeps unlisted printable ASCII codes
/// as they are; Wingdings has only the listed codes.
pub fn map(font: &str, code: u32) -> Option<char> {
    let t = table(font)?;
    let code = u8::try_from(code).ok()?;
    if let Some((_, c)) = t.iter().find(|(k, _)| *k == code) {
        return Some(*c);
    }
    let symbol = font.eq_ignore_ascii_case("Symbol");
    (symbol && (0x21..=0x7E).contains(&code) && code != 0x60).then(|| char::from(code))
}

/// Every (code, character) of `font`, by code.
pub fn characters(font: &str) -> Vec<(u8, char)> {
    (0x20u8..=0xFF).filter_map(|k| map(font, u32::from(k)).map(|c| (k, c))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_fonts_map_to_unicode() {
        assert_eq!(map("Wingdings", 74), Some('\u{263A}'));
        assert_eq!(map("wingdings", 0xFC), Some('\u{2713}'));
        assert_eq!(map("Wingdings", 0x30), None, "codes without a known counterpart are left out");
        assert_eq!(map("Symbol", u32::from(b'a')), Some('α'));
        assert_eq!(map("Symbol", u32::from(b'W')), Some('Ω'));
        assert_eq!(map("Symbol", u32::from(b'1')), Some('1'));
        assert_eq!(map("Symbol", 0x60), None);
        assert_eq!(map("Symbol", 999), None);
        assert_eq!(map("Arial", 65), None);
        for f in SYMBOL_FONTS {
            let chars = characters(f);
            assert!(chars.len() > 50, "{f}");
            assert!(chars.windows(2).all(|w| w[0].0 < w[1].0));
            assert!(chars.iter().all(|(_, c)| !c.is_control()));
        }
    }
}
