//! Charset decoding by label (WHATWG labels via `encoding_rs`, plus common aliases).
//!
//! Mail in the wild carries many labels that are not WHATWG labels (`utf8`, `cp936`, `x-unknown`, ...). Labels
//! are normalised (trimmed, unquoted, lower-cased, RFC 2231 language suffix removed), mapped through a small alias
//! table, then looked up with `encoding_rs`. UTF-7 (RFC 2152) is decoded here because `encoding_rs` does not
//! support it. Anything else decodes as UTF-8 with replacement characters.

use encoding_rs::Encoding;

/// Decodes `bytes` labelled `charset` (case-insensitive, e.g. "utf-8", "iso-8859-1", "windows-1252",
/// "us-ascii", "gb2312", "shift_jis"). Unknown labels decode as UTF-8 lossy.
pub fn decode(bytes: &[u8], charset: &str) -> String {
    let label = normalize_label(charset);
    if label == "utf-7" || label == "utf7" || label == "unicode-1-1-utf-7" {
        return decode_utf7(bytes);
    }
    if is_ascii_label(&label) {
        // US-ASCII is a subset of both UTF-8 and windows-1252. Mislabelled 8-bit data is common: prefer UTF-8
        // when it is valid, otherwise the Windows code page most senders actually meant.
        return match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => decode_with(encoding_rs::WINDOWS_1252, bytes),
        };
    }
    match lookup(&label) {
        Some(enc) => decode_with(enc, bytes),
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Decodes bytes of unknown charset: UTF-8 when valid, windows-1252 otherwise (the usual meaning of raw 8-bit
/// header bytes and unlabelled bodies).
pub fn decode_unlabelled(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => decode_with(encoding_rs::WINDOWS_1252, bytes),
    }
}

/// True when `charset` names an encoding this module can decode (WHATWG label, alias or UTF-7).
pub fn is_known(charset: &str) -> bool {
    let label = normalize_label(charset);
    label == "utf-7" || label == "utf7" || is_ascii_label(&label) || lookup(&label).is_some()
}

/// Canonical (WHATWG) name for a label, e.g. `"latin1"` → `"windows-1252"`, `"utf8"` → `"UTF-8"`.
pub fn canonical_name(charset: &str) -> Option<&'static str> {
    let label = normalize_label(charset);
    if is_ascii_label(&label) {
        return Some("US-ASCII");
    }
    if label == "utf-7" || label == "utf7" {
        return Some("UTF-7");
    }
    lookup(&label).map(Encoding::name)
}

/// Lower-cases, trims whitespace and quotes, and strips an RFC 2231 `*language` suffix.
pub fn normalize_label(charset: &str) -> String {
    let t = charset.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    let t = match t.find('*') {
        Some(i) => t.get(..i).unwrap_or(t),
        None => t,
    };
    t.to_ascii_lowercase()
}

fn is_ascii_label(label: &str) -> bool {
    matches!(
        label,
        "us-ascii"
            | "ascii"
            | "ansi_x3.4-1968"
            | "iso646-us"
            | "us"
            | "iso-ir-6"
            | "csascii"
            | "ansi_x3.4-1986"
            | "cp367"
    )
}

/// Maps non-WHATWG labels seen in mail to a WHATWG label.
fn alias(label: &str) -> Option<&'static str> {
    Some(match label {
        "utf8" | "utf-8-sig" | "x-utf8" | "utf_8" => "utf-8",
        "latin1" | "latin-1" | "iso8859-1" | "iso_8859_1" | "8859-1" | "iso-8859-1:1987" => "windows-1252",
        "latin2" | "latin-2" | "iso8859-2" => "iso-8859-2",
        "iso8859-15" | "latin9" | "latin-9" => "iso-8859-15",
        "cp1250" | "win-1250" => "windows-1250",
        "cp1251" | "win-1251" => "windows-1251",
        "cp1252" | "win-1252" | "windows1252" | "ms-ansi" => "windows-1252",
        "cp1253" => "windows-1253",
        "cp1254" => "windows-1254",
        "cp1255" => "windows-1255",
        "cp1256" => "windows-1256",
        "cp1257" => "windows-1257",
        "cp1258" => "windows-1258",
        "cp874" => "windows-874",
        "cp932" | "ms932" | "sjis" | "x-sjis" | "shift-jis" | "ms_kanji" => "shift_jis",
        "cp936" | "ms936" | "gb_2312" | "x-gb2312" | "gb2312-80" => "gbk",
        "cp949" | "ms949" | "uhc" | "ks_c_5601" => "euc-kr",
        "cp950" | "ms950" | "big-5" | "big5hkscs" => "big5",
        "cp866" | "ibm-866" => "ibm866",
        "koi8r" => "koi8-r",
        "koi8u" => "koi8-u",
        "eucjp" | "euc_jp" => "euc-jp",
        "euckr" | "euc_kr" => "euc-kr",
        "iso2022jp" | "iso_2022_jp" => "iso-2022-jp",
        "utf-16" | "utf16" | "ucs-2" | "unicode" => "utf-16le",
        "utf16le" => "utf-16le",
        "utf16be" => "utf-16be",
        "tis-620" | "tis620" => "windows-874",
        "x-unknown" | "unknown" | "unknown-8bit" | "x-unknown-8bit" | "default" | "charset" | "none" | "" => {
            return None;
        }
        _ => return None,
    })
}

/// Looks up an `encoding_rs` encoding for a normalised label (alias table first, then WHATWG labels).
pub fn lookup(label: &str) -> Option<&'static Encoding> {
    let label = normalize_label(label);
    if let Some(a) = alias(&label) {
        return Encoding::for_label(a.as_bytes());
    }
    // WHATWG maps "replacement" labels (e.g. iso-2022-kr) to an encoding that yields a single U+FFFD; mail
    // readers do better with UTF-8 lossy, so treat those as unknown.
    match Encoding::for_label(label.as_bytes()) {
        Some(e) if e == encoding_rs::REPLACEMENT => None,
        other => other,
    }
}

fn decode_with(enc: &'static Encoding, bytes: &[u8]) -> String {
    let (text, _) = enc.decode_with_bom_removal(bytes);
    text.into_owned()
}

/// Decodes UTF-7 (RFC 2152). Malformed shifted sequences are decoded as far as possible; never fails.
pub fn decode_utf7(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut units: Vec<u16> = Vec::new();
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b != b'+' {
            out.push(if b.is_ascii() { char::from(b) } else { '\u{FFFD}' });
            i += 1;
            continue;
        }
        i += 1;
        if bytes.get(i) == Some(&b'-') {
            out.push('+');
            i += 1;
            continue;
        }
        let mut acc: u32 = 0;
        let mut bits: u32 = 0;
        units.clear();
        while let Some(&c) = bytes.get(i) {
            let Some(v) = b64_value(c) else { break };
            acc = ((acc << 6) | u32::from(v)) & 0x00FF_FFFF;
            bits += 6;
            if bits >= 16 {
                bits -= 16;
                units.push(((acc >> bits) & 0xFFFF) as u16);
            }
            i += 1;
        }
        out.push_str(&String::from_utf16_lossy(&units));
        if bytes.get(i) == Some(&b'-') {
            i += 1;
        }
    }
    out
}

/// Value of a standard base64 alphabet character.
pub(crate) fn b64_value(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_and_aliases() {
        assert_eq!(decode("caf\u{e9}".as_bytes(), "UTF-8"), "café");
        assert_eq!(decode("caf\u{e9}".as_bytes(), "utf8"), "café");
        assert_eq!(decode("caf\u{e9}".as_bytes(), " \"utf-8\" "), "café");
        assert_eq!(decode(b"caf\xe9", "latin1"), "café");
        assert_eq!(decode(b"caf\xe9", "ISO-8859-1"), "café");
        assert_eq!(decode(b"\x93hi\x94", "cp1252"), "\u{201c}hi\u{201d}");
        assert_eq!(decode(b"\x93hi\x94", "windows-1252"), "\u{201c}hi\u{201d}");
    }

    #[test]
    fn ascii_label_with_8bit_data() {
        assert_eq!(decode(b"plain", "us-ascii"), "plain");
        assert_eq!(decode("naïve".as_bytes(), "ascii"), "naïve");
        assert_eq!(decode(b"na\xefve", "us-ascii"), "naïve");
    }

    #[test]
    fn unknown_labels_fall_back_to_utf8() {
        assert_eq!(decode("ok ✓".as_bytes(), "x-unknown"), "ok ✓");
        assert_eq!(decode(b"a\xffb", "no-such-charset"), "a\u{FFFD}b");
        assert_eq!(decode(b"abc", ""), "abc");
        assert_eq!(decode(b"abc", "iso-2022-kr"), "abc");
    }

    #[test]
    fn cjk_and_cyrillic() {
        assert_eq!(decode(b"\x82\xa0", "shift_jis"), "あ");
        assert_eq!(decode(b"\x82\xa0", "cp932"), "あ");
        assert_eq!(decode(b"\xc4\xe3\xba\xc3", "gb2312"), "你好");
        assert_eq!(decode(b"\xc4\xe3\xba\xc3", "cp936"), "你好");
        assert_eq!(decode(b"\xef\xf0\xe8\xe2\xe5\xf2", "windows-1251"), "привет");
        assert_eq!(decode(b"\xd0\xd2\xc9\xd7\xc5\xd4", "koi8-r"), "привет");
        assert_eq!(decode(b"\x1b$B$\"\x1b(B", "iso-2022-jp"), "あ");
        assert_eq!(decode(b"\xb0\xa1", "ks_c_5601-1987"), "가");
    }

    #[test]
    fn utf7() {
        assert_eq!(decode(b"Hi Mom -+Jjo--!", "utf-7"), "Hi Mom -☺-!");
        assert_eq!(decode(b"A+ImIDkQ.", "UTF-7"), "A≢Α.");
        assert_eq!(decode(b"1 +- 1", "utf-7"), "1 + 1");
        assert_eq!(decode(b"+ZeVnLIqe-", "utf-7"), "日本語");
        assert_eq!(decode_utf7(b"+"), "");
        assert_eq!(decode_utf7(b"+\xff"), "\u{FFFD}");
    }

    #[test]
    fn names_and_known() {
        assert_eq!(canonical_name("latin1"), Some("windows-1252"));
        assert_eq!(canonical_name("utf8"), Some("UTF-8"));
        assert_eq!(canonical_name("ascii"), Some("US-ASCII"));
        assert_eq!(canonical_name("utf-8*en"), Some("UTF-8"));
        assert_eq!(canonical_name("bogus"), None);
        assert!(is_known("cp1252"));
        assert!(!is_known("x-unknown"));
        assert_eq!(normalize_label(" 'UTF-8*en-US' "), "utf-8");
    }

    #[test]
    fn hostile_inputs_never_panic() {
        let labels =
            ["utf-8", "utf-7", "shift_jis", "utf-16", "utf-16be", "gb18030", "x-unknown", "*", "''", "\u{0}"];
        let mut seed: u32 = 12345;
        for len in 0..200usize {
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                bytes.push((seed >> 16) as u8);
            }
            for l in labels {
                let _ = decode(&bytes, l);
            }
            let _ = decode_utf7(&bytes);
            let _ = decode_unlabelled(&bytes);
        }
    }
}
