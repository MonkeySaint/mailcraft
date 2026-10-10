//! Modified UTF-7 mailbox names (RFC 3501 §5.1.3), used unless the server enabled UTF8=ACCEPT (RFC 6855).

use base64::Engine;
use base64::alphabet::IMAP_MUTF7;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};

const ENGINE: GeneralPurpose = GeneralPurpose::new(
    &IMAP_MUTF7,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Encodes a mailbox name to modified UTF-7.
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        out.push('&');
        out.push_str(&ENGINE.encode(bytes));
        out.push('-');
        pending.clear();
    };
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Decodes a modified UTF-7 mailbox name. Malformed shift sequences are kept verbatim, so this never fails.
pub fn decode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or_default());
        let after = rest.get(amp + 1..).unwrap_or_default();
        let Some(dash) = after.find('-') else {
            out.push_str(rest.get(amp..).unwrap_or_default());
            return out;
        };
        let b64 = after.get(..dash).unwrap_or_default();
        if b64.is_empty() {
            out.push('&');
        } else {
            match ENGINE.decode(b64) {
                Ok(bytes) if bytes.len() % 2 == 0 => {
                    let units: Vec<u16> =
                        bytes.as_chunks::<2>().0.iter().map(|p| u16::from_be_bytes(*p)).collect();
                    out.extend(char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)));
                }
                _ => {
                    out.push('&');
                    out.push_str(b64);
                    out.push('-');
                }
            }
        }
        rest = after.get(dash + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rfc_examples() {
        assert_eq!(encode("~peter/mail/台北/日本語"), "~peter/mail/&U,BTFw-/&ZeVnLIqe-");
        assert_eq!(decode("~peter/mail/&U,BTFw-/&ZeVnLIqe-"), "~peter/mail/台北/日本語");
        assert_eq!(encode("Tom & Jerry"), "Tom &- Jerry");
        assert_eq!(decode("Tom &- Jerry"), "Tom & Jerry");
        assert_eq!(encode("Entwürfe"), "Entw&APw-rfe");
        assert_eq!(decode("Entw&APw-rfe"), "Entwürfe");
        assert_eq!(decode("broken &abc"), "broken &abc");
        assert_eq!(decode("&!!!-"), "&!!!-");
        assert_eq!(encode("😀"), "&2D3eAA-");
        assert_eq!(decode("&2D3eAA-"), "😀");
    }

    proptest! {
        #[test]
        fn roundtrip(s in "\\PC{0,30}") {
            prop_assert_eq!(decode(&encode(&s)), s);
        }
        #[test]
        fn decode_never_panics(s in "[&a-zA-Z0-9+,\\-]{0,40}") {
            let _ = decode(&s);
        }
    }
}
