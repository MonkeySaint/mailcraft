//! Content-addressed blob keys.
//!
//! Key grammar: `<hash>-<len>[-<n>]` where `hash` is the FNV-1a 64-bit hash of the content as 16 lower-case
//! hex digits, `len` the content length in lower-case hex (no leading zeros), and `n` a decimal collision
//! counter (1..=[`MAX_PROBES`]) used only when a different content already owns the plain key. Keys are
//! validated strictly before they touch the filesystem, so a hostile key can never escape the blob directory.

use std::path::{Path, PathBuf};

/// Directory (under the store) holding blobs.
pub const BLOBS_DIR: &str = "blobs";
/// Collision counter limit; real FNV-64 collisions with equal length are vanishingly rare.
pub const MAX_PROBES: u32 = 64;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64-bit hash.
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// A parsed blob key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlobKey {
    pub hash: u64,
    pub len: u64,
    /// Collision counter (0 = the plain key).
    pub probe: u32,
}

impl BlobKey {
    /// The plain (probe 0) key for `data`.
    pub fn for_data(data: &[u8]) -> BlobKey {
        BlobKey { hash: fnv1a64(data), len: u64::try_from(data.len()).unwrap_or(u64::MAX), probe: 0 }
    }

    /// True when `data` has this key's hash and length (the probe counter is not content-derived).
    pub fn matches(&self, data: &[u8]) -> bool {
        u64::try_from(data.len()).ok() == Some(self.len) && fnv1a64(data) == self.hash
    }

    pub fn with_probe(self, probe: u32) -> BlobKey {
        BlobKey { probe, ..self }
    }

    /// Parses a key string; `None` for anything that is not exactly the canonical form.
    pub fn parse(s: &str) -> Option<BlobKey> {
        if s.len() > 48 || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-') {
            return None;
        }
        let mut parts = s.split('-');
        let hash_s = parts.next()?;
        let len_s = parts.next()?;
        let probe_s = parts.next();
        if parts.next().is_some() || hash_s.len() != 16 || len_s.is_empty() || len_s.len() > 16 {
            return None;
        }
        if len_s.len() > 1 && len_s.starts_with('0') {
            return None;
        }
        let hash = u64::from_str_radix(hash_s, 16).ok()?;
        let len = u64::from_str_radix(len_s, 16).ok()?;
        let probe = match probe_s {
            None => 0,
            Some(p) => {
                if p.is_empty() || p.starts_with('0') || !p.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let n = p.parse::<u32>().ok()?;
                if n > MAX_PROBES {
                    return None;
                }
                n
            }
        };
        Some(BlobKey { hash, len, probe })
    }

    /// Path of this blob under the store directory: `blobs/<aa>/<key>.eml`.
    pub fn path(&self, store_dir: &Path) -> PathBuf {
        let key = self.to_string();
        let fan = key.get(..2).unwrap_or("00").to_owned();
        store_dir.join(BLOBS_DIR).join(fan).join(format!("{key}.eml"))
    }
}

impl std::fmt::Display for BlobKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.probe == 0 {
            write!(f, "{:016x}-{:x}", self.hash, self.len)
        } else {
            write!(f, "{:016x}-{:x}-{}", self.hash, self.len, self.probe)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_reference_values() {
        // Published FNV-1a 64 test vectors.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn key_round_trip() {
        let k = BlobKey::for_data(b"hello");
        let s = k.to_string();
        assert_eq!(BlobKey::parse(&s), Some(k));
        let k3 = k.with_probe(3);
        assert_eq!(BlobKey::parse(&k3.to_string()), Some(k3));
        assert!(k.matches(b"hello"));
        assert!(!k.matches(b"hellO"));
        let p = k.path(Path::new("/x"));
        assert!(p.ends_with(format!("blobs/{}/{s}.eml", s.get(..2).unwrap_or(""))));
    }

    #[test]
    fn hostile_keys_rejected() {
        for bad in [
            "",
            "../../etc/passwd",
            "0123456789abcdef",
            "0123456789abcdef-",
            "0123456789ABCDEF-5",
            "0123456789abcdef-05",
            "0123456789abcdef-5-0",
            "0123456789abcdef-5-65",
            "0123456789abcdef-5-1-2",
            "0123456789abcde-5",
            "0123456789abcdef-5/..",
            "0123456789abcdef-11111111111111111",
        ] {
            assert_eq!(BlobKey::parse(bad), None, "{bad}");
        }
        assert!(BlobKey::parse("0123456789abcdef-0").is_some());
    }
}
