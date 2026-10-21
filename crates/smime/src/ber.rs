//! A bounded BER reader: TLV splitting, and BER → DER normalisation for CMS from other mail programs.
//!
//! Many S/MIME senders stream their CMS objects in BER (X.690): indefinite lengths ending in end-of-contents
//! octets, non-minimal length forms, and OCTET STRINGs split into constructed chunks. The `der` crate only
//! reads DER, so [`normalize`] rewrites such input first: every length becomes definite and minimal, every
//! constructed OCTET STRING becomes one primitive OCTET STRING, and the one implicitly tagged OCTET STRING of
//! CMS that can arrive chunked (`EncryptedContentInfo.encryptedContent [0]`) is joined too. Input that is
//! already DER is never rewritten by the callers (certificate bytes must stay as signed).
//!
//! Every function is total: hostile input gives an error, never a panic. Nesting is bounded by
//! [`MAX_DEPTH`] and the output by the input size.

use crate::{Result, SmimeError};

/// Deepest nesting followed.
pub const MAX_DEPTH: usize = 48;

/// Universal tag numbers used here.
pub const TAG_OCTET_STRING: u8 = 0x04;
pub const TAG_OID: u8 = 0x06;
pub const TAG_SEQUENCE: u8 = 0x30;
pub const TAG_SET: u8 = 0x31;

/// One TLV: its tag byte, the content and the whole encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub content: &'a [u8],
    pub raw: &'a [u8],
}

impl Tlv<'_> {
    pub fn constructed(&self) -> bool {
        self.tag & 0x20 != 0
    }
}

fn malformed(what: &str) -> SmimeError {
    SmimeError::Malformed(what.to_owned())
}

/// Reads a definite-length TLV at the start of `input` (single-byte tags only, as in CMS and X.509).
/// Returns the TLV and the rest of the input.
pub fn read_tlv(input: &[u8]) -> Result<(Tlv<'_>, &[u8])> {
    let (tag, len, hdr) = header(input)?;
    let Some(len) = len else { return Err(malformed("indefinite length where DER is required")) };
    let end = hdr.checked_add(len).ok_or_else(|| malformed("length overflow"))?;
    let raw = input.get(..end).ok_or_else(|| malformed("truncated value"))?;
    let content = raw.get(hdr..).ok_or_else(|| malformed("truncated value"))?;
    let rest = input.get(end..).unwrap_or_default();
    Ok((Tlv { tag, content, raw }, rest))
}

/// The TLVs inside a constructed definite-length value (at most `max`).
pub fn children(content: &[u8], max: usize) -> Result<Vec<Tlv<'_>>> {
    let mut out = Vec::new();
    let mut rest = content;
    while !rest.is_empty() {
        if out.len() >= max {
            return Err(malformed("too many elements"));
        }
        let (t, r) = read_tlv(rest)?;
        out.push(t);
        rest = r;
    }
    Ok(out)
}

/// Tag byte, length (`None` = indefinite) and header size.
fn header(input: &[u8]) -> Result<(u8, Option<usize>, usize)> {
    let tag = *input.first().ok_or_else(|| malformed("truncated tag"))?;
    if tag & 0x1f == 0x1f {
        return Err(malformed("multi-byte tags are not used by S/MIME"));
    }
    let first = *input.get(1).ok_or_else(|| malformed("truncated length"))?;
    if first < 0x80 {
        return Ok((tag, Some(usize::from(first)), 2));
    }
    if first == 0x80 {
        if tag & 0x20 == 0 {
            return Err(malformed("indefinite length on a primitive value"));
        }
        return Ok((tag, None, 2));
    }
    let n = usize::from(first & 0x7f);
    if n > 4 {
        return Err(malformed("length too large"));
    }
    let mut len = 0usize;
    for i in 0..n {
        let b = *input.get(2 + i).ok_or_else(|| malformed("truncated length"))?;
        len = len
            .checked_mul(256)
            .and_then(|l| l.checked_add(usize::from(b)))
            .ok_or_else(|| malformed("length"))?;
    }
    Ok((tag, Some(len), 2 + n))
}

/// Appends a DER header for `tag` and `len`.
pub fn push_header(out: &mut Vec<u8>, tag: u8, len: usize) {
    out.push(tag);
    if len < 0x80 {
        out.push(len as u8);
        return;
    }
    let bytes = len.to_be_bytes();
    let skip = bytes.iter().take_while(|b| **b == 0).count();
    let used = bytes.get(skip..).unwrap_or_default();
    out.push(0x80 | used.len() as u8);
    out.extend_from_slice(used);
}

/// A DER TLV from a tag and its content.
pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len().saturating_add(6));
    push_header(&mut out, tag, content.len());
    out.extend_from_slice(content);
    out
}

/// True when `input` is exactly one TLV that reads with definite lengths all the way down (constructed
/// values are followed, primitive ones are not looked into).
pub fn is_definite(input: &[u8]) -> bool {
    fn walk(input: &[u8], depth: usize) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        let mut rest = input;
        while !rest.is_empty() {
            let Ok((t, r)) = read_tlv(rest) else { return false };
            if t.constructed() && !walk(t.content, depth + 1) {
                return false;
            }
            rest = r;
        }
        true
    }
    match read_tlv(input) {
        Ok((t, rest)) => rest.is_empty() && (!t.constructed() || walk(t.content, 1)),
        Err(_) => false,
    }
}

/// Rewrites one BER value (with possibly trailing data ignored) as DER-style definite lengths; see the module
/// docs. Fails on malformed input, nesting deeper than [`MAX_DEPTH`] or unsupported forms.
pub fn normalize(input: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    let (_, _) = value(input, 0, &mut out)?;
    Ok(out)
}

/// One normalised value: (bytes consumed, tag written).
fn value(input: &[u8], depth: usize, out: &mut Vec<u8>) -> Result<(usize, u8)> {
    if depth > MAX_DEPTH {
        return Err(malformed("nesting too deep"));
    }
    let (tag, len, hdr) = header(input)?;
    let body = input.get(hdr..).ok_or_else(|| malformed("truncated value"))?;
    if tag & 0x20 == 0 {
        let len = len.ok_or_else(|| malformed("indefinite primitive"))?;
        let content = body.get(..len).ok_or_else(|| malformed("truncated value"))?;
        push_header(out, tag, len);
        out.extend_from_slice(content);
        return Ok((hdr + len, tag));
    }
    // Constructed: normalise the children into a buffer, then write the header.
    let mut inner = Vec::new();
    let mut child_tags = Vec::new();
    let consumed = match len {
        Some(len) => {
            let content = body.get(..len).ok_or_else(|| malformed("truncated value"))?;
            let mut off = 0;
            while off < content.len() {
                let rest = content.get(off..).unwrap_or_default();
                let (n, t) = value(rest, depth + 1, &mut inner)?;
                child_tags.push(t);
                off = off.checked_add(n).ok_or_else(|| malformed("length"))?;
            }
            hdr + len
        }
        None => {
            let mut off = 0;
            loop {
                let rest = body.get(off..).ok_or_else(|| malformed("missing end-of-contents"))?;
                match rest {
                    [0, 0, ..] => {
                        off += 2;
                        break;
                    }
                    [] | [_] => return Err(malformed("missing end-of-contents")),
                    _ => {
                        let (n, t) = value(rest, depth + 1, &mut inner)?;
                        child_tags.push(t);
                        off = off.checked_add(n).ok_or_else(|| malformed("length"))?;
                    }
                }
            }
            hdr + off
        }
    };
    if tag == 0x24 {
        // Constructed OCTET STRING: join the chunks into one primitive string.
        let joined = join_octets(&inner)?;
        push_header(out, TAG_OCTET_STRING, joined.len());
        out.extend_from_slice(&joined);
        return Ok((consumed, TAG_OCTET_STRING));
    }
    if tag == TAG_SEQUENCE && child_tags.len() == 3 && child_tags.first() == Some(&TAG_OID) {
        // EncryptedContentInfo: OID, AlgorithmIdentifier, [0] IMPLICIT OCTET STRING sent as chunks.
        if child_tags.get(1) == Some(&TAG_SEQUENCE) && child_tags.get(2) == Some(&0xa0) {
            let parts = children(&inner, 3)?;
            if let (Some(a), Some(b), Some(c)) = (parts.first(), parts.get(1), parts.get(2))
                && let Ok(joined) = join_octets(c.content)
            {
                let mut body = Vec::with_capacity(inner.len());
                body.extend_from_slice(a.raw);
                body.extend_from_slice(b.raw);
                push_header(&mut body, 0x80, joined.len());
                body.extend_from_slice(&joined);
                push_header(out, tag, body.len());
                out.extend_from_slice(&body);
                return Ok((consumed, tag));
            }
        }
    }
    push_header(out, tag, inner.len());
    out.extend_from_slice(&inner);
    Ok((consumed, tag))
}

/// The contents of a run of primitive OCTET STRING TLVs, concatenated.
fn join_octets(content: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(content.len());
    for c in children(content, usize::MAX)? {
        if c.tag != TAG_OCTET_STRING {
            return Err(malformed("unexpected element in a chunked string"));
        }
        out.extend_from_slice(c.content);
    }
    Ok(out)
}

/// Parses `input` as DER when it already is, else as normalised BER: the bytes to hand to the DER decoder.
pub fn to_der(input: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>> {
    if is_definite(input) && !has_chunked_strings(input) {
        Ok(std::borrow::Cow::Borrowed(input))
    } else {
        normalize(input).map(std::borrow::Cow::Owned)
    }
}

/// True when a definite-length encoding still holds a constructed OCTET STRING.
fn has_chunked_strings(input: &[u8]) -> bool {
    fn walk(input: &[u8], depth: usize) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        let mut rest = input;
        while let Ok((t, r)) = read_tlv(rest) {
            if t.tag == 0x24 || (t.constructed() && walk(t.content, depth + 1)) {
                return true;
            }
            if r.is_empty() {
                break;
            }
            rest = r;
        }
        false
    }
    walk(input, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_lengths() {
        for len in [0usize, 1, 127, 128, 255, 256, 65_535, 65_536, 1 << 20] {
            let content = vec![7u8; len];
            let enc = tlv(TAG_OCTET_STRING, &content);
            let (t, rest) = read_tlv(&enc).unwrap();
            assert_eq!(t.content.len(), len);
            assert!(rest.is_empty());
            assert!(is_definite(&enc));
        }
    }

    #[test]
    fn indefinite_and_chunked_become_der() {
        // SEQUENCE (indefinite) { OCTET STRING constructed (indefinite) { "ab", "cd" }, NULL }
        let ber = [0x30, 0x80, 0x24, 0x80, 0x04, 2, b'a', b'b', 0x04, 2, b'c', b'd', 0, 0, 0x05, 0, 0, 0];
        assert!(!is_definite(&ber));
        let der = normalize(&ber).unwrap();
        assert_eq!(der, vec![0x30, 8, 0x04, 4, b'a', b'b', b'c', b'd', 0x05, 0]);
        assert_eq!(to_der(&ber).unwrap().as_ref(), der.as_slice());
        // Already DER: borrowed unchanged.
        assert!(matches!(to_der(&der).unwrap(), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn non_minimal_lengths_are_rewritten() {
        let ber = [0x30, 0x82, 0x00, 0x02, 0x05, 0x00];
        assert_eq!(normalize(&ber).unwrap(), vec![0x30, 2, 0x05, 0]);
    }

    #[test]
    fn encrypted_content_chunks_are_joined() {
        // EncryptedContentInfo { OID, SEQUENCE {}, [0] constructed { OCTET "xy", OCTET "z" } }
        let ber =
            [0x30, 0x80, 0x06, 1, 0x2a, 0x30, 0, 0xa0, 0x80, 0x04, 2, b'x', b'y', 0x04, 1, b'z', 0, 0, 0, 0];
        let der = normalize(&ber).unwrap();
        assert_eq!(der, vec![0x30, 10, 0x06, 1, 0x2a, 0x30, 0, 0x80, 3, b'x', b'y', b'z']);
    }

    #[test]
    fn junk_never_panics() {
        let samples: [&[u8]; 9] = [
            &[],
            &[0x30],
            &[0x30, 0x80],
            &[0x30, 0x85, 1, 2, 3, 4, 5],
            &[0x04, 0x80, 0, 0],
            &[0x1f, 0x01, 0x00],
            &[0x30, 0x84, 0xff, 0xff, 0xff, 0xff],
            &[0x24, 0x80, 0x30, 0, 0, 0],
            &[0x30, 0x80, 0x30, 0x80, 0x30, 0x80],
        ];
        for s in samples {
            let _ = normalize(s);
            let _ = to_der(s);
            let _ = is_definite(s);
            let _ = read_tlv(s);
        }
        // Deep nesting is refused, not followed.
        let mut deep = Vec::new();
        for _ in 0..10_000 {
            deep.extend_from_slice(&[0x30, 0x80]);
        }
        assert!(normalize(&deep).is_err());
        assert!(!is_definite(&deep));
    }
}
