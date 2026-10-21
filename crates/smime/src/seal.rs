//! Password protection for files MailCraft writes (a data file with a password): PBES2 (RFC 8018), that is a
//! key derived from the password with PBKDF2-HMAC-SHA256 over a random salt, and AES-256-CBC with a random
//! IV.
//!
//! A fixed marker is encrypted in front of the data, so a wrong password is told apart from a damaged file
//! only as far as "the password is incorrect, or the file is damaged" ([`SmimeError::Password`]).

use zeroize::Zeroizing;

use crate::{Result, SmimeError};

/// Encrypted in front of the data; checked when it is opened.
const MARKER: &[u8] = b"mailcraft-sealed-v1\n";
/// PBKDF2 rounds below this are refused when opening (a file asking for fewer is not ours or was altered).
pub const MIN_ROUNDS: u32 = 1_000;
/// PBKDF2 rounds above this are refused when opening: a hostile file could otherwise make every password
/// attempt run for many seconds on the UI thread. Five times what MailCraft writes for a data file
/// (200,000), so files written with a stronger setting later still open.
pub const MAX_ROUNDS: u32 = 1_000_000;
/// Longest password accepted (bytes).
pub const MAX_PASSWORD: usize = 1024;

/// Data encrypted under a password, with what is needed to open it again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sealed {
    pub rounds: u32,
    pub salt: [u8; 16],
    pub iv: [u8; 16],
    pub data: Vec<u8>,
}

fn check_password(password: &str) -> Result<()> {
    if password.is_empty() || password.len() > MAX_PASSWORD {
        return Err(SmimeError::Unsupported(format!("a password of 1 to {MAX_PASSWORD} bytes")));
    }
    Ok(())
}

/// Encrypts `plain` under `password` with `rounds` PBKDF2 rounds (clamped to the accepted range).
pub fn seal(plain: &[u8], password: &str, rounds: u32) -> Result<Sealed> {
    check_password(password)?;
    let rounds = rounds.clamp(MIN_ROUNDS, MAX_ROUNDS);
    let salt = crate::random_bytes::<16>()?;
    let iv = crate::random_bytes::<16>()?;
    let params = pkcs5::pbes2::Parameters::pbkdf2_sha256_aes256cbc(rounds, &salt, &iv)
        .map_err(|e| SmimeError::Crypto(e.to_string()))?;
    let mut buf = Zeroizing::new(Vec::with_capacity(MARKER.len().saturating_add(plain.len())));
    buf.extend_from_slice(MARKER);
    buf.extend_from_slice(plain);
    let data = params
        .encrypt(password.as_bytes(), &buf)
        .map_err(|e| SmimeError::Crypto(format!("encrypting: {e}")))?;
    Ok(Sealed { rounds, salt, iv, data })
}

/// Decrypts what [`seal`] wrote. A wrong password (or damaged data) is [`SmimeError::Password`].
pub fn open(sealed: &Sealed, password: &str) -> Result<Zeroizing<Vec<u8>>> {
    check_password(password)?;
    if !(MIN_ROUNDS..=MAX_ROUNDS).contains(&sealed.rounds) {
        return Err(SmimeError::Unsupported(format!("{} key derivation rounds", sealed.rounds)));
    }
    let params = pkcs5::pbes2::Parameters::pbkdf2_sha256_aes256cbc(sealed.rounds, &sealed.salt, &sealed.iv)
        .map_err(|e| SmimeError::Crypto(e.to_string()))?;
    let plain =
        Zeroizing::new(params.decrypt(password.as_bytes(), &sealed.data).map_err(|_| SmimeError::Password)?);
    if !plain.starts_with(MARKER) {
        return Err(SmimeError::Password);
    }
    Ok(Zeroizing::new(plain.get(MARKER.len()..).unwrap_or_default().to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_and_open_round_trip() {
        let data = b"{\"format\":\"x\"}".repeat(100);
        let s = seal(&data, "correct horse", MIN_ROUNDS).unwrap();
        assert_ne!(s.data, data);
        assert_eq!(open(&s, "correct horse").unwrap().as_slice(), data.as_slice());
        assert!(matches!(open(&s, "wrong"), Err(SmimeError::Password)));
        // Fresh salt and IV each time.
        let t = seal(&data, "correct horse", MIN_ROUNDS).unwrap();
        assert!(t.salt != s.salt && t.data != s.data);
        // Damaged or hostile input is an error, never a panic.
        let mut cut = s.clone();
        cut.data.truncate(5);
        assert!(open(&cut, "correct horse").is_err());
        let mut slow = s.clone();
        slow.rounds = u32::MAX;
        assert!(matches!(open(&slow, "correct horse"), Err(SmimeError::Unsupported(_))));
        // A file asking for ten million rounds is refused before any key derivation runs.
        slow.rounds = 10_000_000;
        let started = std::time::Instant::now();
        assert!(matches!(open(&slow, "correct horse"), Err(SmimeError::Unsupported(_))));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(seal(&data, "", MIN_ROUNDS).is_err());
        assert!(open(&s, &"x".repeat(MAX_PASSWORD + 1)).is_err());
        let empty = seal(b"", "pw", 1).unwrap();
        assert_eq!(empty.rounds, MIN_ROUNDS);
        assert!(open(&empty, "pw").unwrap().is_empty());
    }
}
