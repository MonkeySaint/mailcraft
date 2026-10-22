//! Password protection for files MailCraft writes (a data file with a password).
//!
//! **Version 2** (what [`seal`] writes) is encrypt-then-MAC: a master key is derived from the password with
//! PBKDF2-HMAC-SHA256 (RFC 8018) over a random salt, and three keys are expanded from it with HMAC-SHA256 (an
//! AES-256 key, an HMAC-SHA256 key and a password check value). The data is encrypted with AES-256-CBC under
//! a random IV, and an HMAC-SHA256 tag covers a canonical encoding of the header (the caller's context, the
//! round count, salt and IV) followed by the ciphertext. Opening checks the password with the check value,
//! then the tag in constant time, and only then decrypts: a wrong password is [`SmimeError::Password`], and
//! a file that was altered or damaged is [`SmimeError::Malformed`].
//!
//! **Version 1** (older files, still opened) is PBES2 without a MAC: the PBKDF2 key is the AES-256-CBC key
//! and a fixed marker is encrypted in front of the data. A wrong password and a damaged file cannot be told
//! apart there ([`SmimeError::Password`] for both). Its key is never one of version 2's (those are expanded
//! from the master key), so a version 2 file relabelled as version 1 does not open.
//!
//! Opening decrypts in place: the decoded ciphertext buffer becomes the returned plain text, with no
//! further copy.

use aes::Aes256;
use cbc::cipher::block_padding::{NoPadding, Pkcs7};
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::{Result, SmimeError};

/// Encrypted in front of the data in version 1; checked when it is opened.
const MARKER: &[u8] = b"mailcraft-sealed-v1\n";
/// PBKDF2 rounds below this are refused when opening (a file asking for fewer is not ours or was altered).
pub const MIN_ROUNDS: u32 = 1_000;
/// PBKDF2 rounds above this are refused when opening: a hostile file could otherwise make every password
/// attempt run for many seconds on the UI thread. Above what MailCraft writes for a data file (600,000), so
/// files written with a somewhat stronger setting later still open.
pub const MAX_ROUNDS: u32 = 1_000_000;
/// Longest password accepted (bytes).
pub const MAX_PASSWORD: usize = 1024;
/// AES block size: the IV and every ciphertext block.
const BLOCK: usize = 16;
/// Ciphertext bytes [`check_password`] needs from a version 1 file (the blocks that hold the marker).
pub const V1_CHECK_BYTES: usize = 32;

/// What proves a version 2 file: the password check value and the tag over header and ciphertext.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auth {
    pub check: [u8; 32],
    pub mac: [u8; 32],
}

/// Data encrypted under a password, with what is needed to open it again. `auth` is `None` for a version 1
/// file (no MAC).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sealed {
    pub rounds: u32,
    pub salt: [u8; 16],
    pub iv: [u8; 16],
    pub data: Vec<u8>,
    pub auth: Option<Auth>,
}

fn check_password_len(password: &str) -> Result<()> {
    if password.is_empty() || password.len() > MAX_PASSWORD {
        return Err(SmimeError::Unsupported(format!("a password of 1 to {MAX_PASSWORD} bytes")));
    }
    Ok(())
}

fn check_rounds(rounds: u32) -> Result<()> {
    if !(MIN_ROUNDS..=MAX_ROUNDS).contains(&rounds) {
        return Err(SmimeError::Unsupported(format!("{rounds} key derivation rounds")));
    }
    Ok(())
}

/// PBKDF2-HMAC-SHA256 of the password: the AES key of version 1, the master key of version 2.
fn derive(password: &str, salt: &[u8; 16], rounds: u32) -> Zeroizing<[u8; 32]> {
    let mut key = Zeroizing::new([0u8; 32]);
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, rounds, key.as_mut());
    key
}

/// The three keys of version 2, expanded from the master key.
struct Keys {
    enc: Zeroizing<[u8; 32]>,
    mac: Zeroizing<[u8; 32]>,
    check: [u8; 32],
}

fn prf(key: &[u8], label: &[u8]) -> Result<[u8; 32]> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|e| SmimeError::Crypto(e.to_string()))?;
    m.update(label);
    Ok(m.finalize().into_bytes().into())
}

fn keys(password: &str, salt: &[u8; 16], rounds: u32) -> Result<Keys> {
    let master = derive(password, salt, rounds);
    Ok(Keys {
        enc: Zeroizing::new(prf(master.as_ref(), b"mailcraft-seal-v2 encryption key")?),
        mac: Zeroizing::new(prf(master.as_ref(), b"mailcraft-seal-v2 authentication key")?),
        check: prf(master.as_ref(), b"mailcraft-seal-v2 password check")?,
    })
}

/// The canonical header the tag covers: a version label, the caller's context (length first), the round
/// count, salt and IV.
fn header(context: &[u8], rounds: u32, salt: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    let mut h = Vec::with_capacity(64usize.saturating_add(context.len()));
    h.extend_from_slice(b"mailcraft-seal-v2\0");
    h.extend_from_slice(&u64::try_from(context.len()).unwrap_or(u64::MAX).to_be_bytes());
    h.extend_from_slice(context);
    h.extend_from_slice(&rounds.to_be_bytes());
    h.extend_from_slice(salt);
    h.extend_from_slice(iv);
    h
}

fn tag_mac(mac_key: &[u8], header: &[u8], data: &[u8]) -> Result<Hmac<Sha256>> {
    let mut m =
        <Hmac<Sha256> as Mac>::new_from_slice(mac_key).map_err(|e| SmimeError::Crypto(e.to_string()))?;
    m.update(header);
    m.update(data);
    Ok(m)
}

/// Equal in time that does not depend on where the first difference is.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Encrypts `plain` under `password` with `rounds` PBKDF2 rounds (clamped to the accepted range), as
/// version 2. `context` names what is sealed (it is bound into the tag, so the same password and bytes
/// sealed for something else do not open here).
pub fn seal(plain: &[u8], password: &str, rounds: u32, context: &[u8]) -> Result<Sealed> {
    check_password_len(password)?;
    let rounds = rounds.clamp(MIN_ROUNDS, MAX_ROUNDS);
    let salt = crate::random_bytes::<16>()?;
    let iv = crate::random_bytes::<16>()?;
    let k = keys(password, &salt, rounds)?;
    let len = plain.len();
    let mut data = Vec::with_capacity(len.saturating_add(BLOCK));
    data.extend_from_slice(plain);
    data.resize(len.saturating_add(BLOCK - len % BLOCK), 0);
    let cipher = cbc::Encryptor::<Aes256>::new_from_slices(k.enc.as_ref(), &iv)
        .map_err(|e| SmimeError::Crypto(e.to_string()))?;
    let n = cipher
        .encrypt_padded_mut::<Pkcs7>(&mut data, len)
        .map_err(|e| SmimeError::Crypto(format!("encrypting: {e}")))?
        .len();
    data.truncate(n);
    let mac =
        tag_mac(k.mac.as_ref(), &header(context, rounds, &salt, &iv), &data)?.finalize().into_bytes().into();
    Ok(Sealed { rounds, salt, iv, data, auth: Some(Auth { check: k.check, mac }) })
}

/// Decrypts what [`seal`] wrote (or a version 1 file), in place. A wrong password is
/// [`SmimeError::Password`]; a version 2 file that was altered or damaged is [`SmimeError::Malformed`].
pub fn open(sealed: Sealed, password: &str, context: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    check_password_len(password)?;
    check_rounds(sealed.rounds)?;
    let Sealed { rounds, salt, iv, data, auth } = sealed;
    let mut data = Zeroizing::new(data);
    match auth {
        Some(auth) => {
            let k = keys(password, &salt, rounds)?;
            if !same(&k.check, &auth.check) {
                return Err(SmimeError::Password);
            }
            tag_mac(k.mac.as_ref(), &header(context, rounds, &salt, &iv), &data)?
                .verify_slice(&auth.mac)
                .map_err(|_| SmimeError::Malformed("the protected data was altered or is damaged".into()))?;
            let cipher = cbc::Decryptor::<Aes256>::new_from_slices(k.enc.as_ref(), &iv)
                .map_err(|e| SmimeError::Crypto(e.to_string()))?;
            let n = cipher
                .decrypt_padded_mut::<Pkcs7>(data.as_mut_slice())
                .map_err(|_| SmimeError::Malformed("the protected data is damaged".into()))?
                .len();
            data.truncate(n);
        }
        None => {
            let key = derive(password, &salt, rounds);
            let cipher = cbc::Decryptor::<Aes256>::new_from_slices(key.as_ref(), &iv)
                .map_err(|e| SmimeError::Crypto(e.to_string()))?;
            let n = cipher
                .decrypt_padded_mut::<Pkcs7>(data.as_mut_slice())
                .map_err(|_| SmimeError::Password)?
                .len();
            data.truncate(n);
            if !data.starts_with(MARKER) {
                return Err(SmimeError::Password);
            }
            data.drain(..MARKER.len());
        }
    }
    Ok(data)
}

/// Whether `password` opens `sealed`, without decrypting the data: version 2 compares the check value (no
/// data needed), version 1 decrypts only the first [`V1_CHECK_BYTES`] of the data (which may be just that
/// prefix) and compares them with the marker. A wrong password is [`SmimeError::Password`].
pub fn check_password(sealed: &Sealed, password: &str) -> Result<()> {
    check_password_len(password)?;
    check_rounds(sealed.rounds)?;
    match &sealed.auth {
        Some(auth) => {
            let k = keys(password, &sealed.salt, sealed.rounds)?;
            if same(&k.check, &auth.check) { Ok(()) } else { Err(SmimeError::Password) }
        }
        None => {
            let Some(head) = sealed.data.get(..V1_CHECK_BYTES) else {
                return Err(SmimeError::Password);
            };
            let mut head = Zeroizing::new(head.to_vec());
            let key = derive(password, &sealed.salt, sealed.rounds);
            cbc::Decryptor::<Aes256>::new_from_slices(key.as_ref(), &sealed.iv)
                .map_err(|e| SmimeError::Crypto(e.to_string()))?
                .decrypt_padded_mut::<NoPadding>(head.as_mut_slice())
                .map_err(|_| SmimeError::Password)?;
            if head.starts_with(MARKER) { Ok(()) } else { Err(SmimeError::Password) }
        }
    }
}

/// Seals `plain` the way the earlier MailCraft did (version 1: PBES2 through the pkcs5 crate, no MAC), so
/// tests of other crates can check that such files still open. Never used to write a file.
#[cfg(any(test, feature = "testkit"))]
pub fn seal_v1(plain: &[u8], password: &str, rounds: u32) -> Result<Sealed> {
    check_password_len(password)?;
    let salt = crate::random_bytes::<16>()?;
    let iv = crate::random_bytes::<16>()?;
    let params = pkcs5::pbes2::Parameters::pbkdf2_sha256_aes256cbc(rounds, &salt, &iv)
        .map_err(|e| SmimeError::Crypto(e.to_string()))?;
    let mut buf = Zeroizing::new(MARKER.to_vec());
    buf.extend_from_slice(plain);
    let data = params.encrypt(password.as_bytes(), &buf).map_err(|e| SmimeError::Crypto(e.to_string()))?;
    Ok(Sealed { rounds, salt, iv, data, auth: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTX: &[u8] = b"test-context";

    fn seal_v1(plain: &[u8], password: &str, rounds: u32) -> Sealed {
        super::seal_v1(plain, password, rounds).unwrap()
    }

    #[test]
    fn seal_and_open_round_trip() {
        let data = b"{\"format\":\"x\"}".repeat(100);
        let s = seal(&data, "correct horse", MIN_ROUNDS, CTX).unwrap();
        assert_ne!(s.data, data);
        assert!(s.auth.is_some(), "version 2 is authenticated");
        assert_eq!(open(s.clone(), "correct horse", CTX).unwrap().as_slice(), data.as_slice());
        assert!(matches!(open(s.clone(), "wrong", CTX), Err(SmimeError::Password)));
        // Fresh salt and IV each time.
        let t = seal(&data, "correct horse", MIN_ROUNDS, CTX).unwrap();
        assert!(t.salt != s.salt && t.data != s.data);
        // Damaged or hostile input is an error, never a panic.
        let mut cut = s.clone();
        cut.data.truncate(5);
        assert!(open(cut, "correct horse", CTX).is_err());
        let mut slow = s.clone();
        slow.rounds = u32::MAX;
        assert!(matches!(open(slow.clone(), "correct horse", CTX), Err(SmimeError::Unsupported(_))));
        // A file asking for ten million rounds is refused before any key derivation runs.
        slow.rounds = 10_000_000;
        let started = std::time::Instant::now();
        assert!(matches!(open(slow, "correct horse", CTX), Err(SmimeError::Unsupported(_))));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(seal(&data, "", MIN_ROUNDS, CTX).is_err());
        assert!(open(s.clone(), &"x".repeat(MAX_PASSWORD + 1), CTX).is_err());
        let empty = seal(b"", "pw", 1, CTX).unwrap();
        assert_eq!(empty.rounds, MIN_ROUNDS);
        assert!(open(empty, "pw", CTX).unwrap().is_empty());
    }

    /// Regression: the sealed format had no MAC, so CBC bit flips and a changed header opened without any
    /// integrity error. Every change to a version 2 file is now refused as damaged, never as plain text.
    #[test]
    fn altered_version_2_data_or_header_is_refused_as_damaged() {
        let data = b"0123456789abcdef".repeat(8);
        let s = seal(&data, "pw-1", MIN_ROUNDS, CTX).unwrap();
        let damaged = |r: Result<Zeroizing<Vec<u8>>>| matches!(r, Err(SmimeError::Malformed(_)));
        // Flipping a bit of ciphertext block 0 would flip the same bit of plain block 1 under bare CBC.
        let mut flip = s.clone();
        flip.data[0] ^= 1;
        assert!(damaged(open(flip, "pw-1", CTX)));
        // So would changing the IV (plain block 0).
        let mut iv = s.clone();
        iv.iv[3] ^= 0x80;
        assert!(damaged(open(iv, "pw-1", CTX)));
        // Dropping the last block (a truncation with the previous block as the new last one).
        let mut short = s.clone();
        short.data.truncate(short.data.len() - BLOCK);
        assert!(damaged(open(short, "pw-1", CTX)));
        // A tag that was changed, or a context it was not sealed for.
        let mut tag = s.clone();
        if let Some(a) = tag.auth.as_mut() {
            a.mac[31] ^= 1;
        }
        assert!(damaged(open(tag, "pw-1", CTX)));
        assert!(damaged(open(s.clone(), "pw-1", b"another-context")));
        // A changed salt or round count is a different key: the password no longer matches.
        let mut salt = s.clone();
        salt.salt[0] ^= 1;
        assert!(matches!(open(salt, "pw-1", CTX), Err(SmimeError::Password)));
        // Relabelled as version 1 (MAC dropped), it does not open: the keys differ.
        let mut v1 = s.clone();
        v1.auth = None;
        assert!(matches!(open(v1, "pw-1", CTX), Err(SmimeError::Password)));
        assert_eq!(open(s, "pw-1", CTX).unwrap().as_slice(), data.as_slice());
    }

    #[test]
    fn version_1_files_still_open() {
        let data = b"{\"format\":\"mailcraft-data-file\"}".repeat(10);
        let v1 = seal_v1(&data, "old-pw", MIN_ROUNDS);
        assert_eq!(open(v1.clone(), "old-pw", CTX).unwrap().as_slice(), data.as_slice());
        assert!(matches!(open(v1.clone(), "nope", CTX), Err(SmimeError::Password)));
        check_password(&v1, "old-pw").unwrap();
        assert!(matches!(check_password(&v1, "nope"), Err(SmimeError::Password)));
        // Only the first blocks are needed to check the password.
        let mut head = v1.clone();
        head.data.truncate(V1_CHECK_BYTES);
        check_password(&head, "old-pw").unwrap();
        head.data.truncate(V1_CHECK_BYTES - 1);
        assert!(check_password(&head, "old-pw").is_err());
        let empty = seal_v1(b"", "pw", MIN_ROUNDS);
        assert!(open(empty, "pw", CTX).unwrap().is_empty());
    }

    #[test]
    fn checking_the_password_needs_no_data() {
        let s = seal(&[7u8; 4096], "pw-2", MIN_ROUNDS, CTX).unwrap();
        let mut bare = s.clone();
        bare.data.clear();
        check_password(&bare, "pw-2").unwrap();
        assert!(matches!(check_password(&bare, "pw-3"), Err(SmimeError::Password)));
        bare.rounds = 10_000_000;
        assert!(matches!(check_password(&bare, "pw-2"), Err(SmimeError::Unsupported(_))));
    }
}
