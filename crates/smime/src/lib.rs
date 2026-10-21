//! S/MIME for MailCraft (layer L3): sign, verify, encrypt and decrypt mail.
//!
//! Specs: RFC 8551 (S/MIME 4.0 message format), RFC 5652 (CMS: SignedData, EnvelopedData), RFC 5280 (X.509
//! certificates and path validation, simplified: no revocation checking), RFC 7292 (PKCS #12), RFC 5958 /
//! RFC 8018 (PKCS #8 and PBES2), RFC 8017 (RSA: PKCS #1 v1.5 signatures and key transport, RSAES-OAEP
//! decryption), RFC 3565 (AES in CMS), RFC 1847 (multipart/signed).
//!
//! - [`cert`]: X.509 certificates ([`Cert`]): names, addresses, validity, key usage, fingerprints.
//! - [`key`]: RSA private keys ([`PrivateKey`]) from PKCS #8 / PKCS #1 / PEM, and sealed (encrypted) storage.
//! - [`pkcs12`]: `.pfx` / `.p12` import and export of a digital ID ([`Identity`]).
//! - [`cms`]: SignedData and EnvelopedData build / verify / decrypt.
//! - [`trust`]: certificate chains and the status a signature gets (valid, untrusted, expired, invalid).
//! - [`mime`]: whole messages: [`mime::protect`] (sign, encrypt, both) and [`mime::open`] (unwrap the layers).
//! - [`ber`]: the BER reader that turns streamed CMS from other programs into DER.
//! - [`seal`]: password protection (PBES2) for files MailCraft writes, such as a data file with a password.
//!
//! Everything is pure Rust (RustCrypto), so the browser build has it too. Every entry point is total: hostile
//! input (messages, certificates, `.pfx` files) yields an error, never a panic. Sizes are capped
//! ([`MAX_CMS_BYTES`], [`MAX_CERTS`], [`MAX_RECIPIENTS`], [`MAX_SIGNERS`]) and nesting is bounded
//! ([`ber::MAX_DEPTH`], [`mime::MAX_LAYERS`]).

pub mod ber;
pub mod cert;
pub mod cms;
pub mod key;
pub mod mime;
pub mod pkcs12;
pub mod seal;
pub mod trust;

pub use cert::Cert;
pub use key::{Identity, PrivateKey};
pub use trust::{Status, TrustReport};

/// Largest CMS object handled (bytes, after transfer decoding).
pub const MAX_CMS_BYTES: usize = 48 * 1024 * 1024;
/// Most certificates read from one CMS object or `.pfx` file.
pub const MAX_CERTS: usize = 64;
/// Most recipients one EnvelopedData may name (read or written).
pub const MAX_RECIPIENTS: usize = 512;
/// Most signers of one SignedData that are checked.
pub const MAX_SIGNERS: usize = 8;
/// Largest certificate accepted (bytes).
pub const MAX_CERT_BYTES: usize = 256 * 1024;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum SmimeError {
    /// The data is not what it claims to be (bad DER, truncated, wrong structure).
    #[error("malformed data: {0}")]
    Malformed(String),
    /// Valid, but uses something MailCraft does not support (an algorithm, a key type, a format).
    #[error("not supported: {0}")]
    Unsupported(String),
    /// The password is wrong (or the file is damaged).
    #[error("the password is incorrect, or the file is damaged")]
    Password,
    /// None of the user's digital IDs can open the message.
    #[error("none of your digital IDs can decrypt this message")]
    NoKey,
    /// A cryptographic operation failed.
    #[error("{0}")]
    Crypto(String),
    #[error("the data is too large ({0} bytes)")]
    TooLarge(usize),
}

pub type Result<T> = std::result::Result<T, SmimeError>;

impl From<der::Error> for SmimeError {
    fn from(e: der::Error) -> Self {
        SmimeError::Malformed(e.to_string())
    }
}

/// Object identifiers used by S/MIME (RFC 5652, RFC 8551, RFC 5280, RFC 8017, RFC 3565, RFC 7292).
pub mod oid {
    use const_oid::ObjectIdentifier as Oid;

    pub const DATA: Oid = Oid::new_unwrap("1.2.840.113549.1.7.1");
    pub const SIGNED_DATA: Oid = Oid::new_unwrap("1.2.840.113549.1.7.2");
    pub const ENVELOPED_DATA: Oid = Oid::new_unwrap("1.2.840.113549.1.7.3");
    pub const ENCRYPTED_DATA: Oid = Oid::new_unwrap("1.2.840.113549.1.7.6");
    pub const AUTH_ENVELOPED_DATA: Oid = Oid::new_unwrap("1.2.840.113549.1.9.16.1.23");
    pub const CONTENT_TYPE: Oid = Oid::new_unwrap("1.2.840.113549.1.9.3");
    pub const MESSAGE_DIGEST: Oid = Oid::new_unwrap("1.2.840.113549.1.9.4");
    pub const SIGNING_TIME: Oid = Oid::new_unwrap("1.2.840.113549.1.9.5");
    pub const SMIME_CAPABILITIES: Oid = Oid::new_unwrap("1.2.840.113549.1.9.15");
    pub const ENCRYPTION_KEY_PREFERENCE: Oid = Oid::new_unwrap("1.2.840.113549.1.9.16.2.11");
    pub const EMAIL_ADDRESS: Oid = Oid::new_unwrap("1.2.840.113549.1.9.1");
    pub const FRIENDLY_NAME: Oid = Oid::new_unwrap("1.2.840.113549.1.9.20");
    pub const LOCAL_KEY_ID: Oid = Oid::new_unwrap("1.2.840.113549.1.9.21");
    pub const X509_CERT: Oid = Oid::new_unwrap("1.2.840.113549.1.9.22.1");
    pub const COMMON_NAME: Oid = Oid::new_unwrap("2.5.4.3");

    pub const RSA_ENCRYPTION: Oid = Oid::new_unwrap("1.2.840.113549.1.1.1");
    pub const RSAES_OAEP: Oid = Oid::new_unwrap("1.2.840.113549.1.1.7");
    pub const MGF1: Oid = Oid::new_unwrap("1.2.840.113549.1.1.8");
    pub const RSASSA_PSS: Oid = Oid::new_unwrap("1.2.840.113549.1.1.10");
    pub const SHA1_WITH_RSA: Oid = Oid::new_unwrap("1.2.840.113549.1.1.5");
    pub const SHA256_WITH_RSA: Oid = Oid::new_unwrap("1.2.840.113549.1.1.11");
    pub const SHA384_WITH_RSA: Oid = Oid::new_unwrap("1.2.840.113549.1.1.12");
    pub const SHA512_WITH_RSA: Oid = Oid::new_unwrap("1.2.840.113549.1.1.13");
    pub const SHA224_WITH_RSA: Oid = Oid::new_unwrap("1.2.840.113549.1.1.14");

    pub const SHA1: Oid = Oid::new_unwrap("1.3.14.3.2.26");
    pub const SHA256: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.2.1");
    pub const SHA384: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.2.2");
    pub const SHA512: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.2.3");
    pub const SHA224: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.2.4");

    pub const AES128_CBC: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.1.2");
    pub const AES192_CBC: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.1.22");
    pub const AES256_CBC: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.1.42");
    pub const DES_EDE3_CBC: Oid = Oid::new_unwrap("1.2.840.113549.3.7");

    pub const EMAIL_PROTECTION: Oid = Oid::new_unwrap("1.3.6.1.5.5.7.3.4");
    pub const ANY_EXTENDED_KEY_USAGE: Oid = Oid::new_unwrap("2.5.29.37.0");
}

/// A random-number source that has been checked to work: the RSA and cipher code fills from it without a
/// way to report failure, so it is tried once first.
pub(crate) fn rng() -> Result<rand_core::OsRng> {
    use rand_core::RngCore;
    let mut probe = [0u8; 8];
    rand_core::OsRng
        .try_fill_bytes(&mut probe)
        .map_err(|e| SmimeError::Crypto(format!("no random-number source: {e}")))?;
    Ok(rand_core::OsRng)
}

/// `N` random bytes.
pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    use rand_core::RngCore;
    let mut out = [0u8; N];
    rand_core::OsRng
        .try_fill_bytes(&mut out)
        .map_err(|e| SmimeError::Crypto(format!("no random-number source: {e}")))?;
    Ok(out)
}

/// A fresh random secret (256 bits as 64 hex digits), e.g. to seal a key with [`PrivateKey::seal`].
pub fn random_secret() -> Result<String> {
    Ok(hex(&random_bytes::<32>()?))
}

/// Lower-case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Constant-time equality of two byte strings (lengths are not secret).
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
