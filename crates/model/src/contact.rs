use serde::{Deserialize, Serialize};

use crate::{FlagState, FolderId, ItemId};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostalAddress {
    /// "Business", "Home", "Other".
    pub kind: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
}

/// Largest contact picture kept (bytes of the encoded image).
pub const MAX_PICTURE_BYTES: usize = 512 * 1024;

/// A contact's picture (the contact window's Picture button): the encoded image as added.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactPicture {
    /// "image/png", "image/jpeg" or "image/gif".
    pub mime: String,
    /// The encoded image (stored as base64 text).
    #[serde(with = "b64")]
    pub data: Vec<u8>,
}

impl ContactPicture {
    /// Recognises a PNG, JPEG or GIF image by its signature; `None` for anything else, an empty or an
    /// oversized image.
    pub fn sniff(data: &[u8]) -> Option<ContactPicture> {
        if data.is_empty() || data.len() > MAX_PICTURE_BYTES {
            return None;
        }
        let mime = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
            "image/jpeg"
        } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
            "image/gif"
        } else {
            return None;
        };
        Some(ContactPicture { mime: mime.to_owned(), data: data.to_vec() })
    }

    /// Pixel size from the image header (PNG IHDR, GIF logical screen, JPEG start of frame).
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        let d = &self.data;
        let two = |i: usize| -> Option<[u8; 2]> { d.get(i..i.checked_add(2)?)?.try_into().ok() };
        let be16 = |i: usize| two(i).map(|b| u32::from(u16::from_be_bytes(b)));
        let le16 = |i: usize| two(i).map(|b| u32::from(u16::from_le_bytes(b)));
        match self.mime.as_str() {
            "image/png" => {
                let w: [u8; 4] = d.get(16..20)?.try_into().ok()?;
                let h: [u8; 4] = d.get(20..24)?.try_into().ok()?;
                Some((u32::from_be_bytes(w), u32::from_be_bytes(h)))
            }
            "image/gif" => Some((le16(6)?, le16(8)?)),
            "image/jpeg" => {
                // Walk the markers to the first SOFn segment.
                let mut i = 2usize;
                for _ in 0..1_000 {
                    if *d.get(i)? != 0xff {
                        return None;
                    }
                    let marker = *d.get(i.checked_add(1)?)?;
                    if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                        return Some((be16(i.checked_add(7)?)?, be16(i.checked_add(5)?)?));
                    }
                    let len = usize::try_from(be16(i.checked_add(2)?)?).ok()?;
                    i = i.checked_add(2)?.checked_add(len)?;
                }
                None
            }
            _ => None,
        }
    }
}

/// Base64 (RFC 4648, standard alphabet, padded) for the picture bytes, so the stored mailbox stays compact.
pub(crate) mod b64 {
    use serde::{Deserialize, Deserializer, Serializer};

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [
                chunk.first().copied().unwrap_or(0),
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for (k, shift) in [18u32, 12, 6, 0].iter().enumerate() {
                if k <= chunk.len() {
                    let idx = usize::try_from((n >> shift) & 63).unwrap_or(0);
                    out.push(char::from(ALPHABET.get(idx).copied().unwrap_or(b'A')));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub fn decode(text: &str) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(text.len() / 4 * 3);
        let mut acc: u32 = 0;
        let mut bits = 0u32;
        for c in text.bytes() {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' | b'\r' | b'\n' | b' ' => continue,
                _ => return None,
            };
            acc = (acc << 6) | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(u8::try_from((acc >> bits) & 0xff).unwrap_or(0));
            }
            acc &= (1 << bits) - 1;
        }
        Some(out)
    }

    pub fn serialize<S: Serializer>(data: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode(data))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        decode(&text).ok_or_else(|| serde::de::Error::custom("invalid base64 picture"))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub id: ItemId,
    pub folder: FolderId,
    pub first_name: String,
    pub middle_name: String,
    pub last_name: String,
    pub title: String,
    pub suffix: String,
    /// "File as" ("Last, First" by default).
    pub file_as: String,
    pub company: String,
    pub job_title: String,
    pub department: String,
    /// Up to three addresses (Email, Email 2, Email 3).
    pub emails: Vec<String>,
    /// (kind, number) e.g. ("Business", "+1 555 0100"), ("Mobile", …).
    pub phones: Vec<(String, String)>,
    pub addresses: Vec<PostalAddress>,
    pub web_page: String,
    pub im_address: String,
    /// "YYYY-MM-DD".
    pub birthday: Option<String>,
    pub notes: String,
    pub categories: Vec<String>,
    /// Follow-up flag (Home › Tags › Follow Up): contacts can be flagged like messages. Add Reminder puts a
    /// reminder time on it.
    #[serde(default)]
    pub flag: FlagState,
    /// Private (Home › Tags › Private).
    #[serde(default)]
    pub private: bool,
    /// Details page: office location.
    #[serde(default)]
    pub office: String,
    /// Details page: profession.
    #[serde(default)]
    pub profession: String,
    /// Details page: manager's name.
    #[serde(default)]
    pub manager: String,
    /// Details page: assistant's name.
    #[serde(default)]
    pub assistant: String,
    /// Details page: nickname.
    #[serde(default)]
    pub nickname: String,
    /// Details page: spouse/partner.
    #[serde(default)]
    pub spouse: String,
    /// Details page: anniversary, "YYYY-MM-DD".
    #[serde(default)]
    pub anniversary: Option<String>,
    /// The contact's picture.
    #[serde(default)]
    pub picture: Option<ContactPicture>,
    /// S/MIME certificates (base64 DER X.509), e.g. from a vCard `KEY`: encrypted mail to the contact uses
    /// them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub certificates: Vec<String>,
    /// The server copy (CardDAV), when the contact is synchronised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<crate::account::RemoteRef>,
}

impl Contact {
    /// "Title First Middle Last Suffix" with empty parts skipped.
    pub fn full_name(&self) -> String {
        [&self.title, &self.first_name, &self.middle_name, &self.last_name, &self.suffix]
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0, 3, 0,
        0, 0, 2, 8, 6, 0, 0, 0,
    ];

    #[test]
    fn base64_round_trips() {
        for n in 0..40usize {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 % 256) as u8).collect();
            let text = b64::encode(&data);
            assert_eq!(text.len() % 4, 0);
            assert_eq!(b64::decode(&text).unwrap(), data, "{n}");
        }
        assert_eq!(b64::encode(b"Man"), "TWFu");
        assert_eq!(b64::encode(b"Ma"), "TWE=");
        assert!(b64::decode("@@@").is_none());
    }

    #[test]
    fn picture_sniffing_and_size() {
        let p = ContactPicture::sniff(PNG_1X1).unwrap();
        assert_eq!(p.mime, "image/png");
        assert_eq!(p.dimensions(), Some((3, 2)));
        assert!(ContactPicture::sniff(b"not an image").is_none());
        assert!(ContactPicture::sniff(&[]).is_none());
        let gif = ContactPicture::sniff(b"GIF89a\x10\x00\x20\x00").unwrap();
        assert_eq!(gif.dimensions(), Some((16, 32)));
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 11, 8, 0, 48, 0, 64];
        assert_eq!(ContactPicture::sniff(&jpeg).unwrap().dimensions(), Some((64, 48)));
        // Truncated and hostile headers give no size instead of panicking.
        assert_eq!(ContactPicture::sniff(&[0xff, 0xd8, 0xff]).unwrap().dimensions(), None);
        assert_eq!(ContactPicture::sniff(&PNG_1X1[..12]).unwrap().dimensions(), None);
        let big = vec![0x89; MAX_PICTURE_BYTES + 1];
        assert!(ContactPicture::sniff(&big).is_none());
    }

    #[test]
    fn contact_with_picture_serialises_compactly() {
        let c =
            Contact { picture: ContactPicture::sniff(PNG_1X1), nickname: "Bo".into(), ..Default::default() };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"data\":\"iVBORw0KGgo"), "{json}");
        let back: Contact = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
        // Contacts saved before the Details fields existed still load.
        let old = r#"{"id":1,"folder":2,"first_name":"A","middle_name":"","last_name":"","title":"",
            "suffix":"","file_as":"","company":"","job_title":"","department":"","emails":[],"phones":[],
            "addresses":[],"web_page":"","im_address":"","birthday":null,"notes":"","categories":[]}"#;
        let c: Contact = serde_json::from_str(old).unwrap();
        assert!(c.picture.is_none() && c.anniversary.is_none() && c.office.is_empty());
    }
}
