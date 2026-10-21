//! RSA private keys and digital IDs.
//!
//! A [`PrivateKey`] reads PKCS #8 (`PRIVATE KEY`), PKCS #1 (`RSA PRIVATE KEY`) and password-encrypted PKCS #8
//! (`ENCRYPTED PRIVATE KEY`), as DER or PEM. For storage at rest a key is [`PrivateKey::seal`]ed: written as
//! an encrypted PKCS #8 (PBES2: PBKDF2-HMAC-SHA256 and AES-256-CBC, RFC 8018) under a secret that the caller
//! keeps elsewhere (MailCraft keeps it with the account passwords, in the OS keyring where there is one).
//!
//! An [`Identity`] (a digital ID) is a key with its certificate and the issuing chain.

use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use zeroize::Zeroizing;

use crate::{Cert, Result, SmimeError};

/// Largest key file accepted (bytes).
const MAX_KEY_BYTES: usize = 64 * 1024;
/// Smallest RSA key accepted (bits).
pub const MIN_RSA_BITS: usize = 1024;
/// Largest RSA key accepted (bits): bigger keys make every operation slow enough to stall the window.
pub const MAX_RSA_BITS: usize = 8192;
/// PBKDF2 rounds for sealed keys (the secret is random, so few rounds suffice).
const SEAL_ROUNDS: u32 = 10_000;
/// Most PBKDF2 rounds accepted from a file (a hostile file could ask for billions).
pub(crate) const MAX_KDF_ROUNDS: u32 = 4_000_000;

/// An RSA private key.
#[derive(Clone)]
pub struct PrivateKey(pub(crate) RsaPrivateKey);

impl std::fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PrivateKey(RSA {} bits)", self.bits())
    }
}

impl PrivateKey {
    pub(crate) fn new(k: RsaPrivateKey) -> Result<PrivateKey> {
        let bits = k.size().saturating_mul(8);
        if !(MIN_RSA_BITS..=MAX_RSA_BITS).contains(&bits) {
            return Err(SmimeError::Unsupported(format!("RSA key of {bits} bits")));
        }
        Ok(PrivateKey(k))
    }

    /// From unencrypted PKCS #8 DER.
    pub fn from_pkcs8_der(der: &[u8]) -> Result<PrivateKey> {
        if der.len() > MAX_KEY_BYTES {
            return Err(SmimeError::TooLarge(der.len()));
        }
        match RsaPrivateKey::from_pkcs8_der(der) {
            Ok(k) => PrivateKey::new(k),
            Err(e) => Err(key_error(e)),
        }
    }

    /// From a key file: PEM (`PRIVATE KEY`, `RSA PRIVATE KEY`, `ENCRYPTED PRIVATE KEY`) or DER (PKCS #8,
    /// encrypted PKCS #8 or PKCS #1). `password` opens encrypted keys.
    pub fn from_file(bytes: &[u8], password: &str) -> Result<PrivateKey> {
        if bytes.len() > MAX_KEY_BYTES {
            return Err(SmimeError::TooLarge(bytes.len()));
        }
        if let Some(text) = std::str::from_utf8(bytes).ok().filter(|t| t.contains("-----BEGIN")) {
            if text.contains("Proc-Type: 4,ENCRYPTED") {
                return Err(SmimeError::Unsupported(
                    "keys encrypted in the old OpenSSL format (convert it to PKCS #8 first)".into(),
                ));
            }
            for (label, der) in crate::cert::pem_blocks(text)? {
                let der = Zeroizing::new(der);
                match label.as_str() {
                    "PRIVATE KEY" => return PrivateKey::from_pkcs8_der(&der),
                    "RSA PRIVATE KEY" => return PrivateKey::from_pkcs1_der(&der),
                    "ENCRYPTED PRIVATE KEY" => return PrivateKey::from_encrypted_der(&der, password),
                    _ => {}
                }
            }
            return Err(SmimeError::Malformed("no private key found in the text".into()));
        }
        PrivateKey::from_pkcs8_der(bytes)
            .or_else(|_| PrivateKey::from_pkcs1_der(bytes))
            .or_else(|_| PrivateKey::from_encrypted_der(bytes, password))
    }

    fn from_pkcs1_der(der: &[u8]) -> Result<PrivateKey> {
        match RsaPrivateKey::from_pkcs1_der(der) {
            Ok(k) => PrivateKey::new(k),
            Err(e) => Err(SmimeError::Malformed(format!("RSA private key: {e}"))),
        }
    }

    /// From a password-encrypted PKCS #8 (PBES2) key.
    pub fn from_encrypted_der(der: &[u8], password: &str) -> Result<PrivateKey> {
        let info = pkcs8::EncryptedPrivateKeyInfo::try_from(der)
            .map_err(|e| SmimeError::Malformed(format!("encrypted private key: {e}")))?;
        check_pbes(&info.encryption_algorithm)?;
        let doc = info.decrypt(password.as_bytes()).map_err(|_| SmimeError::Password)?;
        PrivateKey::from_pkcs8_der(doc.as_bytes())
    }

    /// Unencrypted PKCS #8 DER (wiped from memory when dropped).
    pub fn to_pkcs8_der(&self) -> Result<Zeroizing<Vec<u8>>> {
        let doc = self.0.to_pkcs8_der().map_err(|e| SmimeError::Crypto(format!("encoding the key: {e}")))?;
        Ok(Zeroizing::new(doc.as_bytes().to_vec()))
    }

    /// Encrypted PKCS #8 DER under `password` (PBES2, PBKDF2-HMAC-SHA256 with `rounds`, AES-256-CBC).
    pub fn to_encrypted_der(&self, password: &str, rounds: u32) -> Result<Vec<u8>> {
        let salt = crate::random_bytes::<16>()?;
        let iv = crate::random_bytes::<16>()?;
        let params = pkcs5::pbes2::Parameters::pbkdf2_sha256_aes256cbc(rounds, &salt, &iv)
            .map_err(|e| SmimeError::Crypto(e.to_string()))?;
        let der = self.to_pkcs8_der()?;
        let info = pkcs8::PrivateKeyInfo::try_from(der.as_slice())
            .map_err(|e| SmimeError::Crypto(format!("encoding the key: {e}")))?;
        let doc = info
            .encrypt_with_params(params, password.as_bytes())
            .map_err(|e| SmimeError::Crypto(format!("encrypting the key: {e}")))?;
        Ok(doc.as_bytes().to_vec())
    }

    /// The key encrypted for storage under `secret` (see the module docs).
    pub fn seal(&self, secret: &str) -> Result<Vec<u8>> {
        self.to_encrypted_der(secret, SEAL_ROUNDS)
    }

    /// Opens a key written by [`PrivateKey::seal`].
    pub fn unseal(sealed: &[u8], secret: &str) -> Result<PrivateKey> {
        PrivateKey::from_encrypted_der(sealed, secret)
    }

    pub fn public_key(&self) -> RsaPublicKey {
        self.0.to_public_key()
    }

    pub fn bits(&self) -> usize {
        self.0.size().saturating_mul(8)
    }

    /// True when `cert` carries this key's public half.
    pub fn matches(&self, cert: &Cert) -> bool {
        cert.rsa_public_key().is_ok_and(|p| p == self.public_key())
    }

    /// A new random RSA key of `bits` bits (slow: seconds for 4096 bits).
    pub fn generate(bits: usize) -> Result<PrivateKey> {
        if !(2048..=MAX_RSA_BITS).contains(&bits) {
            return Err(SmimeError::Unsupported(format!("RSA key of {bits} bits")));
        }
        let mut rng = crate::rng()?;
        let k = RsaPrivateKey::new(&mut rng, bits).map_err(|e| SmimeError::Crypto(e.to_string()))?;
        PrivateKey::new(k)
    }
}

fn key_error(e: pkcs8::Error) -> SmimeError {
    match e {
        pkcs8::Error::KeyMalformed | pkcs8::Error::Asn1(_) => {
            SmimeError::Malformed(format!("private key: {e}"))
        }
        other => SmimeError::Unsupported(format!("private key: {other} (only RSA keys are supported)")),
    }
}

/// Refuses PBES2 parameters that would take too long (huge PBKDF2 round counts or scrypt costs).
pub(crate) fn check_pbes(alg: &pkcs5::EncryptionScheme<'_>) -> Result<()> {
    let Some(p) = alg.pbes2() else {
        return Err(SmimeError::Unsupported("PBES1 key encryption".into()));
    };
    if let Some(k) = p.kdf.pbkdf2() {
        if k.iteration_count > MAX_KDF_ROUNDS {
            return Err(SmimeError::Unsupported(format!("{} key derivation rounds", k.iteration_count)));
        }
    } else if let Some(s) = p.kdf.scrypt() {
        let cost = s.cost_parameter;
        let work = cost.saturating_mul(u64::from(s.block_size)).saturating_mul(u64::from(s.parallelization));
        if cost > (1 << 20) || work > (1 << 24) {
            return Err(SmimeError::Unsupported("scrypt parameters too expensive".into()));
        }
    }
    Ok(())
}

/// A digital ID: a private key, its certificate and the certificates that issued it (nearest first).
#[derive(Clone, Debug)]
pub struct Identity {
    pub key: PrivateKey,
    pub cert: Cert,
    pub chain: Vec<Cert>,
}

impl Identity {
    /// Pairs a key with its certificate (refused when they do not belong together).
    pub fn new(key: PrivateKey, cert: Cert, chain: Vec<Cert>) -> Result<Identity> {
        if !key.matches(&cert) {
            return Err(SmimeError::Malformed("the private key does not belong to the certificate".into()));
        }
        Ok(Identity { key, cert, chain })
    }

    /// A key file plus certificates (PEM or DER, any order): the certificate matching the key is the ID's,
    /// the others are its chain.
    pub fn from_key_and_certs(key: PrivateKey, certs: Vec<Cert>) -> Result<Identity> {
        let pos = certs.iter().position(|c| key.matches(c)).ok_or_else(|| {
            SmimeError::Malformed("none of the certificates belongs to the private key".into())
        })?;
        let mut rest = certs;
        let cert = rest.remove(pos);
        Ok(Identity { key, cert, chain: rest })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    #[test]
    fn seal_and_unseal() {
        let id = testkit::identity("Ann Lee", "ann@example.com");
        let sealed = id.key.seal("s3cret-wrap").unwrap();
        let back = PrivateKey::unseal(&sealed, "s3cret-wrap").unwrap();
        assert!(back.matches(&id.cert));
        assert_eq!(PrivateKey::unseal(&sealed, "wrong").unwrap_err(), SmimeError::Password);
        assert!(PrivateKey::unseal(b"junk", "x").is_err());
    }

    #[test]
    fn key_files_in_every_form() {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        let id = testkit::identity("Ann Lee", "ann@example.com");
        let der = id.key.to_pkcs8_der().unwrap();
        let pem8 = crate::cert::pem("PRIVATE KEY", &der);
        let pem1 = id.key.0.to_pkcs1_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        let enc = id.key.to_encrypted_der("pw", 2048).unwrap();
        let enc_pem = crate::cert::pem("ENCRYPTED PRIVATE KEY", &enc);
        for (bytes, pw) in [
            (der.as_slice(), ""),
            (pem8.as_bytes(), ""),
            (pem1.as_bytes(), ""),
            (enc.as_slice(), "pw"),
            (enc_pem.as_bytes(), "pw"),
        ] {
            assert!(PrivateKey::from_file(bytes, pw).unwrap().matches(&id.cert));
        }
        assert_eq!(PrivateKey::from_file(enc_pem.as_bytes(), "nope").unwrap_err(), SmimeError::Password);
        assert!(
            PrivateKey::from_file(b"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----", "")
                .is_err()
        );
    }

    #[test]
    fn identity_pairs_key_and_certificate() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let bob = testkit::identity("Bob Day", "bob@example.org");
        assert!(Identity::new(ann.key.clone(), bob.cert.clone(), vec![]).is_err());
        let id =
            Identity::from_key_and_certs(ann.key.clone(), vec![testkit::ca(), ann.cert.clone()]).unwrap();
        assert_eq!(id.cert, ann.cert);
        assert_eq!(id.chain, vec![testkit::ca()]);
        assert!(Identity::from_key_and_certs(ann.key.clone(), vec![bob.cert.clone()]).is_err());
    }
}
