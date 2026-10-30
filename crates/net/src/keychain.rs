//! The operating system's credential store, through the `keyring` crate: Keychain Services on macOS, the
//! Credential Manager on Windows, and the Secret Service (D-Bus) on Linux and the BSDs.
//!
//! Only opened when asked ([`Keychain::open`]); nothing here runs in tests against the real store. Callers
//! fall back to another store when [`Keychain::open`] fails (no D-Bus session, no keyring daemon, locked or
//! unsupported platform).
//!
//! # Long secrets
//!
//! Windows Credential Manager keeps at most 2560 bytes per credential (1280 UTF-16 code units), and OAuth
//! access tokens are often longer. A secret longer than [`CHUNK_UNITS`] UTF-16 code units is therefore split
//! over several entries on every platform: the parts go to `<key>#<generation>.<n>` (n = 1, 2, …) and `<key>`
//! itself holds a small marker, `mailcraft-chunks/1 gen=<g> parts=<n> bytes=<len> sha256=<hex>`. Reads accept
//! both the plain single-entry form (every secret stored before, and every short one) and the chunked form.
//!
//! A replacement writes its parts under the other generation first and the marker last, so until the marker
//! is written the old value still reads back whole; old parts are deleted afterwards. A marker that does not
//! parse, a missing part, or parts that do not add up to the recorded length and SHA-256 read as an error,
//! never as a wrong secret.

use ring::digest::{SHA256, digest};

/// Longest key accepted (the stores limit attribute lengths).
const MAX_KEY_CHARS: usize = 255;
/// Most UTF-16 code units in one stored entry (Credential Manager allows 1280; this leaves room).
pub const CHUNK_UNITS: usize = 1000;
/// Most parts one secret is split into (and most read back from a hostile store).
pub const MAX_CHUNKS: usize = 64;
/// Start of the marker that stands for a chunked secret.
const MARKER_PREFIX: &str = "mailcraft-chunks/1 ";

/// A handle on the OS credential store under one service name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keychain {
    service: String,
}

fn err(e: &keyring::Error) -> String {
    e.to_string()
}

/// One entry per key, no chunking: the OS store, or an in-memory map in tests.
trait RawStore {
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, secret: &str) -> Result<(), String>;
    fn remove(&self, key: &str) -> Result<(), String>;
}

impl RawStore for Keychain {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        match self.entry(key)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(err(&e)),
        }
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), String> {
        self.entry(key)?.set_password(secret).map_err(|e| err(&e))
    }

    fn remove(&self, key: &str) -> Result<(), String> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(err(&e)),
        }
    }
}

impl Keychain {
    /// Opens the platform store for `service` and checks that it answers (a lookup of an entry that does not
    /// exist must report "no entry", not a failure).
    pub fn open(service: &str) -> Result<Keychain, String> {
        if service.trim().is_empty() || service.chars().count() > MAX_KEY_CHARS {
            return Err("invalid keychain service name".into());
        }
        if let Err(e) = keyring::Entry::store_status() {
            return Err(format!("no credential store: {}", err(e)));
        }
        let k = Keychain { service: service.to_owned() };
        k.get("mailcraft-probe/does-not-exist")?;
        Ok(k)
    }

    /// The store's name for Account Settings.
    pub fn store_name() -> &'static str {
        if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else if cfg!(target_os = "windows") {
            "Windows Credential Manager"
        } else {
            "Secret Service keyring"
        }
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, String> {
        if key.is_empty() || key.chars().count() > MAX_KEY_CHARS || key.chars().any(char::is_control) {
            return Err("invalid keychain key".into());
        }
        keyring::Entry::new(&self.service, key).map_err(|e| err(&e))
    }

    /// The secret stored under `key`, `None` when there is none. A damaged chunked secret is an error.
    pub fn get(&self, key: &str) -> Result<Option<String>, String> {
        chunked_get(self, key)
    }

    /// Stores (or replaces) the secret under `key`, split over several entries when it is long.
    pub fn set(&self, key: &str, secret: &str) -> Result<(), String> {
        chunked_set(self, key, secret)
    }

    /// Deletes the secret under `key` and its parts (no error when there is none).
    pub fn remove(&self, key: &str) -> Result<(), String> {
        chunked_remove(self, key)
    }
}

// =======================================================================================================
// Chunking
// =======================================================================================================

/// What the marker of a chunked secret records.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Marker {
    /// Which set of part keys holds the parts (0 or 1; a replacement switches).
    generation: u8,
    parts: usize,
    /// Length of the whole secret in UTF-8 bytes.
    bytes: usize,
    /// SHA-256 of the whole secret, lowercase hex.
    sha256: String,
}

impl Marker {
    fn for_secret(secret: &str, generation: u8, parts: usize) -> Marker {
        Marker { generation, parts, bytes: secret.len(), sha256: sha256_hex(secret) }
    }

    fn render(&self) -> String {
        format!(
            "{MARKER_PREFIX}gen={} parts={} bytes={} sha256={}",
            self.generation, self.parts, self.bytes, self.sha256
        )
    }

    /// Parses a marker; `None` for anything malformed or out of bounds.
    fn parse(text: &str) -> Option<Marker> {
        let rest = text.strip_prefix(MARKER_PREFIX)?;
        let mut words = rest.split(' ');
        let mut field = |name: &str| words.next()?.strip_prefix(name)?.strip_prefix('=').map(str::to_owned);
        let generation: u8 = field("gen")?.parse().ok()?;
        let parts: usize = field("parts")?.parse().ok()?;
        let bytes: usize = field("bytes")?.parse().ok()?;
        let sha256 = field("sha256")?;
        if words.next().is_some()
            || generation > 1
            || parts == 0
            || parts > MAX_CHUNKS
            || bytes > max_secret_bytes()
            || sha256.len() != 64
            || !sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        Some(Marker { generation, parts, bytes, sha256 })
    }
}

/// Largest secret in UTF-8 bytes (4 bytes per code unit at most).
fn max_secret_bytes() -> usize {
    MAX_CHUNKS.saturating_mul(CHUNK_UNITS).saturating_mul(4)
}

fn sha256_hex(text: &str) -> String {
    use std::fmt::Write;
    let d = digest(&SHA256, text.as_bytes());
    let mut out = String::with_capacity(64);
    for b in d.as_ref() {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Key of part `n` (1-based) of generation `generation` of the secret under `key`.
fn part_key(key: &str, generation: u8, n: usize) -> String {
    format!("{key}#{generation}.{n}")
}

/// Splits `secret` into pieces of at most [`CHUNK_UNITS`] UTF-16 code units, on character boundaries.
fn split_units(secret: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut units = 0;
    for (i, c) in secret.char_indices() {
        let u = c.len_utf16();
        if units + u > CHUNK_UNITS {
            out.push(secret.get(start..i).unwrap_or_default());
            start = i;
            units = 0;
        }
        units += u;
    }
    if start < secret.len() {
        out.push(secret.get(start..).unwrap_or_default());
    }
    out
}

/// Whether `secret` must be chunked: too long for one entry, or it would read back as a marker.
fn needs_chunks(secret: &str) -> bool {
    secret.encode_utf16().nth(CHUNK_UNITS).is_some() || secret.starts_with(MARKER_PREFIX)
}

/// The marker under `key`, if the stored value is one (`Err` for a damaged marker).
fn stored_marker(store: &dyn RawStore, key: &str) -> Result<Option<Marker>, String> {
    match store.get(key)? {
        Some(v) if v.starts_with(MARKER_PREFIX) => Marker::parse(&v)
            .map(Some)
            .ok_or_else(|| "the stored secret is damaged (bad part list)".to_owned()),
        _ => Ok(None),
    }
}

fn chunked_get(store: &dyn RawStore, key: &str) -> Result<Option<String>, String> {
    let Some(value) = store.get(key)? else { return Ok(None) };
    if !value.starts_with(MARKER_PREFIX) {
        return Ok(Some(value));
    }
    let m = Marker::parse(&value).ok_or_else(|| "the stored secret is damaged (bad part list)".to_owned())?;
    let mut out = String::new();
    for n in 1..=m.parts {
        let part = store
            .get(&part_key(key, m.generation, n))?
            .ok_or_else(|| format!("the stored secret is damaged (part {n} of {} is missing)", m.parts))?;
        if out.len().saturating_add(part.len()) > m.bytes {
            return Err("the stored secret is damaged (its parts are too long)".into());
        }
        out.push_str(&part);
    }
    if out.len() != m.bytes || sha256_hex(&out) != m.sha256 {
        return Err("the stored secret is damaged (its parts do not match)".into());
    }
    Ok(Some(out))
}

/// Deletes parts `1..=parts` of `generation` under `key`, stopping at the first failure.
fn remove_parts(store: &dyn RawStore, key: &str, generation: u8, parts: usize) -> Result<(), String> {
    for n in 1..=parts.min(MAX_CHUNKS) {
        store.remove(&part_key(key, generation, n))?;
    }
    Ok(())
}

fn chunked_set(store: &dyn RawStore, key: &str, secret: &str) -> Result<(), String> {
    // A damaged old marker is replaced, but its parts are unknown and stay behind.
    let old = stored_marker(store, key).unwrap_or_else(|e| {
        log::warn!("keychain: replacing a secret whose old value cannot be read: {e}");
        None
    });
    if needs_chunks(secret) {
        let pieces = split_units(secret);
        if pieces.len() > MAX_CHUNKS {
            return Err(format!(
                "the secret is too long for the credential store ({} parts, at most {MAX_CHUNKS})",
                pieces.len()
            ));
        }
        let generation = old.as_ref().map_or(0, |m| u8::from(m.generation == 0));
        for (i, piece) in pieces.iter().enumerate() {
            store.set(&part_key(key, generation, i + 1), piece)?;
        }
        store.set(key, &Marker::for_secret(secret, generation, pieces.len()).render())?;
        // Parts of this generation left over by an earlier value whose cleanup failed.
        if let Err(e) = remove_parts_after(store, key, generation, pieces.len()) {
            log::warn!("keychain: cannot delete old parts of a secret: {e}");
        }
    } else {
        store.set(key, secret)?;
    }
    if let Some(m) = old
        && let Err(e) = remove_parts(store, key, m.generation, m.parts)
    {
        log::warn!("keychain: cannot delete old parts of a secret: {e}");
    }
    Ok(())
}

/// After a chunked write of `written` parts under `generation`, deletes any further parts of that generation
/// (probed in order until one is absent).
fn remove_parts_after(store: &dyn RawStore, key: &str, generation: u8, written: usize) -> Result<(), String> {
    for n in written.saturating_add(1)..=MAX_CHUNKS {
        let k = part_key(key, generation, n);
        if store.get(&k)?.is_none() {
            break;
        }
        store.remove(&k)?;
    }
    Ok(())
}

fn chunked_remove(store: &dyn RawStore, key: &str) -> Result<(), String> {
    // Parts first: if a deletion fails, the marker stays and the next removal finds them again.
    match stored_marker(store, key) {
        Ok(Some(m)) => remove_parts(store, key, m.generation, m.parts)?,
        Ok(None) => {}
        Err(e) => log::warn!("keychain: removing a damaged secret: {e}"),
    }
    store.remove(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    /// An in-memory store with the Credential Manager's size limit; `fail_after` makes the n-th next write
    /// (or delete) fail.
    #[derive(Default)]
    struct MapStore {
        map: RefCell<BTreeMap<String, String>>,
        fail_after: Cell<Option<usize>>,
    }

    impl MapStore {
        fn tick(&self) -> Result<(), String> {
            match self.fail_after.get() {
                Some(0) => {
                    self.fail_after.set(None);
                    Err("store failure".into())
                }
                Some(n) => {
                    self.fail_after.set(Some(n - 1));
                    Ok(())
                }
                None => Ok(()),
            }
        }
        fn keys(&self) -> Vec<String> {
            self.map.borrow().keys().cloned().collect()
        }
        fn raw(&self, k: &str) -> Option<String> {
            self.map.borrow().get(k).cloned()
        }
        fn put(&self, k: &str, v: &str) {
            self.map.borrow_mut().insert(k.to_owned(), v.to_owned());
        }
    }

    impl RawStore for MapStore {
        fn get(&self, key: &str) -> Result<Option<String>, String> {
            Ok(self.raw(key))
        }
        fn set(&self, key: &str, secret: &str) -> Result<(), String> {
            self.tick()?;
            if secret.encode_utf16().count() * 2 > 2560 {
                return Err("longer than the platform limit".into());
            }
            self.put(key, secret);
            Ok(())
        }
        fn remove(&self, key: &str) -> Result<(), String> {
            self.tick()?;
            self.map.borrow_mut().remove(key);
            Ok(())
        }
    }

    fn token(len: usize) -> String {
        (0..len).map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap())).collect()
    }

    #[test]
    fn rejects_bad_names_without_touching_the_store() {
        assert!(Keychain::open("").is_err());
        assert!(Keychain::open(&"x".repeat(300)).is_err());
        let k = Keychain { service: "MailCraft test".into() };
        assert!(k.entry("").is_err());
        assert!(k.entry("a\nb").is_err());
        assert!(!Keychain::store_name().is_empty());
        assert_eq!(k.service(), "MailCraft test");
    }

    #[test]
    fn short_secrets_stay_in_one_entry() {
        let st = MapStore::default();
        chunked_set(&st, "k", "pw").unwrap();
        assert_eq!(st.keys(), ["k"]);
        assert_eq!(st.raw("k").as_deref(), Some("pw"));
        assert_eq!(chunked_get(&st, "k").unwrap().as_deref(), Some("pw"));
        let edge = token(CHUNK_UNITS);
        chunked_set(&st, "k", &edge).unwrap();
        assert_eq!(st.keys(), ["k"]);
        assert_eq!(chunked_get(&st, "k").unwrap(), Some(edge));
        chunked_remove(&st, "k").unwrap();
        assert!(st.keys().is_empty());
        assert_eq!(chunked_get(&st, "k").unwrap(), None);
        chunked_remove(&st, "k").unwrap();
    }

    #[test]
    fn old_single_entry_values_read_back() {
        // A value stored by an earlier version (no limit on this platform) is read as it is.
        let st = MapStore::default();
        let long = token(3000);
        st.put("k", &long);
        assert_eq!(chunked_get(&st, "k").unwrap(), Some(long));
    }

    #[test]
    fn long_secrets_are_split_and_reassembled() {
        let st = MapStore::default();
        let long = token(2500);
        chunked_set(&st, "k", &long).unwrap();
        assert_eq!(st.keys(), ["k", "k#0.1", "k#0.2", "k#0.3"]);
        assert!(st.raw("k").unwrap().starts_with(MARKER_PREFIX));
        assert_eq!(chunked_get(&st, "k").unwrap().as_deref(), Some(long.as_str()));

        // A shorter long value switches generation and drops the old parts.
        let mid = token(1500);
        chunked_set(&st, "k", &mid).unwrap();
        assert_eq!(st.keys(), ["k", "k#1.1", "k#1.2"]);
        assert_eq!(chunked_get(&st, "k").unwrap(), Some(mid));

        // Back to generation 0 with a longer value.
        chunked_set(&st, "k", &long).unwrap();
        assert_eq!(st.keys(), ["k", "k#0.1", "k#0.2", "k#0.3"]);
        assert_eq!(chunked_get(&st, "k").unwrap().as_deref(), Some(long.as_str()));

        // A short value replaces the marker and the parts go.
        chunked_set(&st, "k", "short").unwrap();
        assert_eq!(st.keys(), ["k"]);
        assert_eq!(chunked_get(&st, "k").unwrap().as_deref(), Some("short"));

        chunked_set(&st, "k", &long).unwrap();
        chunked_remove(&st, "k").unwrap();
        assert!(st.keys().is_empty());
    }

    #[test]
    fn splits_on_utf16_units_and_char_boundaries() {
        let s = format!("{}{}", "é".repeat(999), "😀".repeat(600));
        let pieces = split_units(&s);
        assert!(pieces.iter().all(|p| p.encode_utf16().count() <= CHUNK_UNITS));
        assert_eq!(pieces.concat(), s);
        assert_eq!(pieces.len(), 3);
        assert!(split_units("").is_empty());

        let st = MapStore::default();
        chunked_set(&st, "k", &s).unwrap();
        assert_eq!(chunked_get(&st, "k").unwrap(), Some(s));
    }

    #[test]
    fn a_value_that_looks_like_a_marker_round_trips() {
        let st = MapStore::default();
        let tricky = format!("{MARKER_PREFIX}gen=0 parts=1 bytes=1 sha256={}", "0".repeat(64));
        chunked_set(&st, "k", &tricky).unwrap();
        assert_eq!(chunked_get(&st, "k").unwrap(), Some(tricky));
    }

    #[test]
    fn too_many_parts_is_an_error() {
        let st = MapStore::default();
        assert!(chunked_set(&st, "k", &token(CHUNK_UNITS * MAX_CHUNKS + 1)).is_err());
        assert!(st.keys().is_empty());
        chunked_set(&st, "k", &token(CHUNK_UNITS * MAX_CHUNKS)).unwrap();
        assert_eq!(st.keys().len(), MAX_CHUNKS + 1);
    }

    #[test]
    fn interrupted_writes_never_read_back_wrong() {
        let long = token(2500);
        let other = token(2600);
        for fail_at in 0..8 {
            let st = MapStore::default();
            chunked_set(&st, "k", &long).unwrap();
            st.fail_after.set(Some(fail_at));
            let r = chunked_set(&st, "k", &other);
            let got = chunked_get(&st, "k").unwrap();
            if r.is_ok() {
                assert_eq!(got.as_deref(), Some(other.as_str()), "fail_at {fail_at}");
            } else {
                // Until the marker is written the old value is intact.
                assert_eq!(got.as_deref(), Some(long.as_str()), "fail_at {fail_at}");
            }
            st.fail_after.set(None);
        }
        // An interrupted removal leaves the marker, which still reads back whole or fails; never garbage.
        for fail_at in 0..4 {
            let st = MapStore::default();
            chunked_set(&st, "k", &long).unwrap();
            st.fail_after.set(Some(fail_at));
            if chunked_remove(&st, "k").is_err() {
                assert!(
                    chunked_get(&st, "k").is_err() || chunked_get(&st, "k").unwrap() == Some(long.clone())
                );
                st.fail_after.set(None);
                chunked_remove(&st, "k").unwrap();
            }
            assert!(st.keys().is_empty(), "fail_at {fail_at}: {:?}", st.keys());
        }
    }

    #[test]
    fn hostile_store_contents_are_errors() {
        let st = MapStore::default();
        let long = token(2500);
        chunked_set(&st, "k", &long).unwrap();

        // A missing part.
        let saved = st.raw("k#0.2").unwrap();
        st.map.borrow_mut().remove("k#0.2");
        assert!(chunked_get(&st, "k").is_err());
        // A changed part.
        st.put("k#0.2", &saved.to_uppercase());
        assert!(chunked_get(&st, "k").is_err());
        // A part that is too long.
        st.put("k#0.2", &format!("{saved}{saved}"));
        assert!(chunked_get(&st, "k").is_err());
        st.put("k#0.2", &saved);
        assert_eq!(chunked_get(&st, "k").unwrap(), Some(long.clone()));

        let marker = st.raw("k").unwrap();
        let hash = sha256_hex(&long);
        for bad in [
            MARKER_PREFIX.to_owned(),
            format!("{MARKER_PREFIX}gen=0 parts=3"),
            format!("{MARKER_PREFIX}gen=2 parts=3 bytes=2500 sha256={hash}"),
            format!("{MARKER_PREFIX}gen=0 parts=0 bytes=2500 sha256={hash}"),
            format!("{MARKER_PREFIX}gen=0 parts=99999999999 bytes=2500 sha256={hash}"),
            format!("{MARKER_PREFIX}gen=0 parts=3 bytes=99999999999 sha256={hash}"),
            format!("{MARKER_PREFIX}gen=0 parts=3 bytes=2500 sha256=xyz"),
            format!("{MARKER_PREFIX}gen=0 parts=3 bytes=2500 sha256={hash} extra"),
            format!("{MARKER_PREFIX}parts=3 gen=0 bytes=2500 sha256={hash}"),
            format!("{MARKER_PREFIX}gen=0 parts=-1 bytes=2500 sha256={hash}"),
        ] {
            st.put("k", &bad);
            assert!(chunked_get(&st, "k").is_err(), "{bad}");
        }
        // More parts recorded than exist, or fewer.
        st.put("k", &format!("{MARKER_PREFIX}gen=0 parts=4 bytes=2500 sha256={hash}"));
        assert!(chunked_get(&st, "k").is_err());
        st.put("k", &format!("{MARKER_PREFIX}gen=0 parts=2 bytes=2500 sha256={hash}"));
        assert!(chunked_get(&st, "k").is_err());
        st.put("k", &marker);
        assert_eq!(chunked_get(&st, "k").unwrap(), Some(long));

        // A damaged marker can still be replaced and removed.
        st.put("k", &format!("{MARKER_PREFIX}garbage"));
        chunked_set(&st, "k", "pw").unwrap();
        assert_eq!(chunked_get(&st, "k").unwrap().as_deref(), Some("pw"));
        st.put("k", &format!("{MARKER_PREFIX}garbage"));
        chunked_remove(&st, "k").unwrap();
        assert_eq!(st.raw("k"), None);
    }

    #[test]
    fn marker_round_trip() {
        let m = Marker::for_secret("abc", 1, 1);
        assert_eq!(Marker::parse(&m.render()), Some(m));
        assert_eq!(sha256_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
