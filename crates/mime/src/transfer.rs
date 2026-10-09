//! Content-Transfer-Encoding (RFC 2045 §6): base64, quoted-printable, 7bit/8bit/binary.
//!
//! Decoders are lenient (they never fail): base64 skips characters outside the alphabet and stops at padding
//! only for the current quantum; quoted-printable keeps malformed `=` sequences literally. `x-uuencode` is also
//! accepted on input because some older clients still send it.

use base64::Engine as _;

use crate::charset::b64_value;

/// Canonical name of a transfer encoding label (case-insensitive, quotes and whitespace ignored, common aliases
/// accepted): `"base64"`, `"quoted-printable"` or `"x-uuencode"`; `""` for the identity encodings (7bit, 8bit,
/// binary) and anything unrecognised.
pub fn canonical_encoding(encoding: &str) -> &'static str {
    let label = encoding.trim().trim_matches('"').trim().to_ascii_lowercase();
    match label.as_str() {
        "base64" | "b" => "base64",
        "quoted-printable" | "quoted_printable" | "qp" => "quoted-printable",
        "x-uuencode" | "uuencode" | "x-uue" | "uue" => "x-uuencode",
        _ => "",
    }
}

/// Decodes a body with the given transfer encoding label (see [`canonical_encoding`]). Lenient: ignores invalid
/// base64 characters, keeps malformed `=` sequences in quoted-printable literally.
pub fn decode(body: &[u8], encoding: &str) -> Vec<u8> {
    decode_cow(body, encoding).into_owned()
}

/// [`decode`] without copying when the encoding is an identity one.
pub fn decode_cow<'a>(body: &'a [u8], encoding: &str) -> std::borrow::Cow<'a, [u8]> {
    match canonical_encoding(encoding) {
        "base64" => decode_base64(body).into(),
        "quoted-printable" => decode_qp(body).into(),
        "x-uuencode" => decode_uuencode(body).into(),
        _ => body.into(),
    }
}

/// Re-encodes `data` with a transfer encoding label (see [`canonical_encoding`]); identity encodings return the
/// data unchanged. `name` is used for the uuencode `begin` line.
pub fn encode(data: &[u8], encoding: &str, name: &str) -> Vec<u8> {
    match canonical_encoding(encoding) {
        "base64" => encode_base64(data).into_bytes(),
        "quoted-printable" => encode_qp(data).into_bytes(),
        "x-uuencode" => encode_uuencode(data, name).into_bytes(),
        _ => data.to_vec(),
    }
}

/// Lenient base64 decoding: characters outside the alphabet (line breaks, spaces, garbage) are skipped, `=`
/// padding ends the current 4-character quantum, and a trailing partial quantum is decoded as far as it goes.
pub fn decode_base64(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 3);
    let mut acc: u32 = 0;
    let mut n = 0u32;
    for &c in input {
        if c == b'=' {
            flush_quantum(&mut out, acc, n);
            acc = 0;
            n = 0;
            continue;
        }
        let Some(v) = b64_value(c) else { continue };
        acc = (acc << 6) | u32::from(v);
        n += 1;
        if n == 4 {
            out.push((acc >> 16) as u8);
            out.push((acc >> 8) as u8);
            out.push(acc as u8);
            acc = 0;
            n = 0;
        }
    }
    flush_quantum(&mut out, acc, n);
    out
}

fn flush_quantum(out: &mut Vec<u8>, acc: u32, n: u32) {
    match n {
        2 => out.push((acc >> 4) as u8),
        3 => {
            out.push((acc >> 10) as u8);
            out.push((acc >> 2) as u8);
        }
        _ => {}
    }
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Decodes `=XX` (either case) at `input[i..]`; returns the byte.
pub(crate) fn hex_pair(input: &[u8], i: usize) -> Option<u8> {
    let h = hex_value(*input.get(i)?)?;
    let l = hex_value(*input.get(i.checked_add(1)?)?)?;
    Some(h << 4 | l)
}

/// Quoted-printable decoding (RFC 2045 §6.7). Soft line breaks (`=` CRLF, `=` LF, `=` + trailing whitespace)
/// are removed; trailing whitespace on hard lines is dropped (rule 3); malformed escapes are kept literally.
pub fn decode_qp(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0usize;
    // Start of trailing-whitespace run in `out` that must be dropped if a line break follows.
    let mut ws_start: Option<usize> = None;
    while let Some(&c) = input.get(i) {
        match c {
            b'=' => {
                ws_start = None;
                if let Some(b) = hex_pair(input, i + 1) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                // Soft line break: '=' followed by optional whitespace then a line ending (or end of input).
                let mut j = i + 1;
                while matches!(input.get(j), Some(b' ' | b'\t')) {
                    j += 1;
                }
                match input.get(j) {
                    Some(b'\r') if input.get(j + 1) == Some(&b'\n') => i = j + 2,
                    Some(b'\n') => i = j + 1,
                    Some(b'\r') => i = j + 1,
                    None => i = j,
                    Some(_) => {
                        out.push(b'=');
                        i += 1;
                    }
                }
            }
            b' ' | b'\t' => {
                if ws_start.is_none() {
                    ws_start = Some(out.len());
                }
                out.push(c);
                i += 1;
            }
            b'\r' | b'\n' => {
                if let Some(ws) = ws_start.take() {
                    out.truncate(ws);
                }
                out.push(c);
                i += 1;
            }
            _ => {
                ws_start = None;
                out.push(c);
                i += 1;
            }
        }
    }
    // The end of the data ends the last line too.
    if let Some(ws) = ws_start {
        out.truncate(ws);
    }
    out
}

/// Decodes a uuencoded body (`begin mode name` ... `end`); lines outside the begin/end block are ignored.
/// Without a `begin` line the whole input is treated as encoded lines.
pub fn decode_uuencode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let has_begin = input.split(|&b| b == b'\n').any(|l| l.starts_with(b"begin "));
    let mut inside = !has_begin;
    for raw in input.split(|&b| b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if !inside {
            if line.starts_with(b"begin ") {
                inside = true;
            }
            continue;
        }
        if line == b"end" {
            break;
        }
        let Some((&len_ch, rest)) = line.split_first() else { continue };
        let len = usize::from(len_ch.wrapping_sub(b' ') & 0x3F);
        if len == 0 {
            continue;
        }
        let mut decoded = Vec::with_capacity(len + 3);
        for chunk in rest.chunks(4) {
            let v = |k: usize| u32::from(chunk.get(k).map_or(0, |c| c.wrapping_sub(b' ') & 0x3F));
            let acc = v(0) << 18 | v(1) << 12 | v(2) << 6 | v(3);
            decoded.push((acc >> 16) as u8);
            decoded.push((acc >> 8) as u8);
            decoded.push(acc as u8);
        }
        decoded.truncate(len);
        out.extend_from_slice(&decoded);
    }
    out
}

/// uuencode (`begin 644 name` … `end`, CRLF line endings, 45 bytes per line, `` ` `` for zero). The name has
/// control characters removed and falls back to `file`.
pub fn encode_uuencode(data: &[u8], name: &str) -> String {
    let uu = |v: u32| char::from(if v & 0x3F == 0 { b'`' } else { (v & 0x3F) as u8 + b' ' });
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let name = if name.trim().is_empty() { "file" } else { name.trim() };
    let mut out = format!("begin 644 {name}\r\n");
    for line in data.chunks(45) {
        out.push(uu(line.len() as u32));
        for chunk in line.chunks(3) {
            let b = |k: usize| u32::from(chunk.get(k).copied().unwrap_or(0));
            let acc = b(0) << 16 | b(1) << 8 | b(2);
            for shift in [18, 12, 6, 0] {
                out.push(uu(acc >> shift));
            }
        }
        out.push_str("\r\n");
    }
    out.push_str("`\r\nend\r\n");
    out
}

/// Base64 with CRLF line breaks every 76 chars. Lines are separated by CRLF; there is no trailing CRLF
/// (empty input gives an empty string).
pub fn encode_base64(data: &[u8]) -> String {
    let flat = base64::engine::general_purpose::STANDARD.encode(data);
    let mut out = String::with_capacity(flat.len() + flat.len() / 76 * 2 + 2);
    let bytes = flat.as_bytes();
    for (k, chunk) in bytes.chunks(76).enumerate() {
        if k > 0 {
            out.push_str("\r\n");
        }
        // Base64 output is pure ASCII.
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
    }
    out
}

/// Plain base64 (no line breaks), as used inside RFC 2047 `B` words.
pub fn encode_base64_flat(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn push_escape(out: &mut String, b: u8) {
    out.push('=');
    out.push(char::from(HEX.get(usize::from(b >> 4)).copied().unwrap_or(b'0')));
    out.push(char::from(HEX.get(usize::from(b & 15)).copied().unwrap_or(b'0')));
}

/// Quoted-printable (RFC 2045 §6.7), soft line breaks at 76 chars, CRLF line endings.
///
/// Input CRLF and bare LF are hard line breaks (written as CRLF); a bare CR is escaped as `=0D`. Whitespace
/// before a line break or at the end of the data is escaped so it survives transport.
pub fn encode_qp(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() + data.len() / 8);
    let mut line_len = 0usize;
    let mut i = 0usize;
    while let Some(&b) = data.get(i) {
        // Hard line breaks.
        if b == b'\n' || (b == b'\r' && data.get(i + 1) == Some(&b'\n')) {
            out.push_str("\r\n");
            line_len = 0;
            i += if b == b'\r' { 2 } else { 1 };
            continue;
        }
        let next = data.get(i + 1).copied();
        let at_line_end =
            matches!(next, None | Some(b'\n')) || (next == Some(b'\r') && data.get(i + 2) == Some(&b'\n'));
        let mut escape = match b {
            b' ' | b'\t' => at_line_end,
            b'=' => true,
            33..=126 => false,
            _ => true,
        };
        // Keep room for the soft-break '=' unless this is the last char before a hard break/end.
        let limit = if at_line_end { 76 } else { 75 };
        if line_len + if escape { 3 } else { 1 } > limit {
            out.push_str("=\r\n");
            line_len = 0;
        }
        // A leading '.' or "From " can be mangled by transports; escaping them is harmless.
        if line_len == 0 && (b == b'.' || (b == b'F' && data.get(i..i + 5) == Some(b"From "))) {
            escape = true;
        }
        let width = if escape { 3 } else { 1 };
        if escape {
            push_escape(&mut out, b);
        } else {
            out.push(char::from(b));
        }
        line_len += width;
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn base64_lenient() {
        assert_eq!(decode(b"SGVsbG8=", "base64"), b"Hello");
        assert_eq!(decode(b"SGVs\r\nbG8=\r\n", "BASE64"), b"Hello");
        assert_eq!(decode(b"SGVsbG8", " base64 "), b"Hello");
        assert_eq!(decode(b"SG!!Vs*bG8=junk", "base64"), b"Hello\x8e\xe9\xe4");
        assert_eq!(decode(b"", "base64"), b"");
        assert_eq!(decode(b"====", "base64"), b"");
        assert_eq!(decode(b"QQ==QUI=", "base64"), b"AAB");
    }

    #[test]
    fn identity_encodings() {
        assert_eq!(decode(b"abc=41", "7bit"), b"abc=41");
        assert_eq!(decode(b"abc", "8bit"), b"abc");
        assert_eq!(decode(b"\x00\xff", "binary"), b"\x00\xff");
        assert_eq!(decode(b"abc", "x-token"), b"abc");
    }

    #[test]
    fn qp_decode() {
        assert_eq!(decode(b"caf=C3=A9", "quoted-printable"), "café".as_bytes());
        assert_eq!(decode(b"caf=c3=a9", "Quoted-Printable"), "café".as_bytes());
        assert_eq!(decode_qp(b"long=\r\nline"), b"longline");
        assert_eq!(decode_qp(b"long=\nline"), b"longline");
        assert_eq!(decode_qp(b"long= \t\r\nline"), b"longline");
        assert_eq!(decode_qp(b"a=\r\n"), b"a");
        assert_eq!(decode_qp(b"end="), b"end");
        assert_eq!(decode_qp(b"bad=ZZ seq"), b"bad=ZZ seq");
        assert_eq!(decode_qp(b"bad=4"), b"bad=4");
        assert_eq!(decode_qp(b"trail   \r\nnext"), b"trail\r\nnext");
        assert_eq!(decode_qp(b"keep=20\r\n"), b"keep \r\n");
        assert_eq!(decode_qp(b"1+1=3D2"), b"1+1=2");
    }

    #[test]
    fn qp_trailing_whitespace_at_end_of_data_is_dropped() {
        // RFC 2045 §6.7 rule 3: the end of the encoded data also ends a line.
        assert_eq!(decode_qp(b"last line  \t"), b"last line");
        assert_eq!(decode_qp(b"keep=20"), b"keep ");
        assert_eq!(decode_qp(b"a =\r\n b"), b"a  b");
    }

    #[test]
    fn uuencode_round_trip() {
        for data in [&b""[..], b"Cat", b"\x00\x01\x02\xff binary", &[7u8; 200][..]] {
            let enc = encode_uuencode(data, "a.bin");
            assert!(enc.starts_with("begin 644 a.bin\r\n"), "{enc}");
            assert!(enc.split("\r\n").all(|l| l.len() <= 61), "{enc}");
            assert_eq!(decode(enc.as_bytes(), "x-uuencode"), data);
        }
    }

    #[test]
    fn uuencode() {
        let body = b"begin 644 cat.txt\n#0V%T\n`\nend\n";
        assert_eq!(decode(body, "x-uuencode"), b"Cat");
        assert_eq!(decode_uuencode(b"#0V%T\n"), b"Cat");
        assert_eq!(decode_uuencode(b""), b"");
    }

    #[test]
    fn base64_encode_lines() {
        let data = vec![0u8; 200];
        let enc = encode_base64(&data);
        for line in enc.split("\r\n") {
            assert!(line.len() <= 76);
        }
        assert!(!enc.ends_with("\r\n"));
        assert_eq!(decode_base64(enc.as_bytes()), data);
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"Hello"), "SGVsbG8=");
    }

    #[test]
    fn qp_encode_basics() {
        assert_eq!(encode_qp(b"hello"), "hello");
        assert_eq!(encode_qp("café".as_bytes()), "caf=C3=A9");
        assert_eq!(encode_qp(b"a=b"), "a=3Db");
        assert_eq!(encode_qp(b"line \nnext"), "line=20\r\nnext");
        assert_eq!(encode_qp(b"tab\t"), "tab=09");
        assert_eq!(encode_qp(b"a\r\nb"), "a\r\nb");
        assert_eq!(encode_qp(b"a\rb"), "a=0Db");
        assert_eq!(encode_qp(b".\nFrom me"), "=2E\r\n=46rom me");
        let long = "x".repeat(200);
        let enc = encode_qp(long.as_bytes());
        for line in enc.split("\r\n") {
            assert!(line.len() <= 76, "{line}");
        }
        assert_eq!(decode_qp(enc.as_bytes()), long.as_bytes());
    }

    #[test]
    fn qp_encode_never_splits_escapes() {
        let s = "é".repeat(100);
        let enc = encode_qp(s.as_bytes());
        for line in enc.split("\r\n") {
            assert!(line.len() <= 76);
            let body = line.strip_suffix('=').unwrap_or(line);
            assert_eq!(body.len() % 3, 0, "{line}");
        }
        assert_eq!(decode_qp(enc.as_bytes()), s.as_bytes());
    }

    proptest! {
        #[test]
        fn base64_round_trip(data in proptest::collection::vec(any::<u8>(), 0..600)) {
            let enc = encode_base64(&data);
            prop_assert!(enc.split("\r\n").all(|l| l.len() <= 76));
            prop_assert_eq!(decode(enc.as_bytes(), "base64"), data);
        }

        #[test]
        fn qp_round_trip(data in proptest::collection::vec(any::<u8>(), 0..600)) {
            let enc = encode_qp(&data);
            prop_assert!(enc.split("\r\n").all(|l| l.len() <= 76));
            prop_assert!(enc.bytes().all(|b| b == b'\r' || b == b'\n' || b == b'\t' || (32..=126).contains(&b)));
            // Bare LF becomes CRLF; compare after normalising the input the same way.
            let mut norm = Vec::new();
            let mut i = 0;
            while let Some(&b) = data.get(i) {
                if b == b'\n' { norm.extend_from_slice(b"\r\n"); }
                else if b == b'\r' && data.get(i + 1) == Some(&b'\n') { norm.extend_from_slice(b"\r\n"); i += 1; }
                else { norm.push(b); }
                i += 1;
            }
            prop_assert_eq!(decode(enc.as_bytes(), "quoted-printable"), norm);
        }

        #[test]
        fn decoders_never_panic(data in proptest::collection::vec(any::<u8>(), 0..400)) {
            let _ = decode(&data, "base64");
            let _ = decode(&data, "quoted-printable");
            let _ = decode(&data, "x-uuencode");
        }
    }
}
