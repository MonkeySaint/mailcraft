//! Test support (the `testkit` feature, used by dev-dependencies only): synthetic certificate authorities and
//! digital IDs created at test time, for example.com / example.org / example.net identities only.
//!
//! Keys are generated once per test process and cached (RSA key generation is slow), so every call with the
//! same name and address returns the same digital ID. Nothing here is used by shipped code.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use der::Encode;
use der::asn1::{BitString, Ia5String, OctetString, UtcTime};
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::pkcs8::EncodePublicKey;
use sha2::{Digest, Sha256};
use x509_cert::certificate::{TbsCertificate, Version};
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{
    BasicConstraints, ExtendedKeyUsage, KeyUsage, KeyUsages, SubjectAltName, SubjectKeyIdentifier,
};
use x509_cert::ext::{AsExtension, Extension};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};
use x509_cert::time::{Time, Validity};

use crate::{Cert, Identity, PrivateKey, oid};

/// Name of the test certificate authority.
pub const CA_NAME: &str = "MailCraft Test CA";
/// A fixed "now" inside every default validity period (2026-10-01 00:00 UTC).
pub const NOW: i64 = 1_790_812_800;
/// Default validity: 2020-01-01 to 2049-12-31.
pub const VALID: (i64, i64) = (1_577_836_800, 2_524_521_600);

const KEY_COUNT: usize = 12;

fn keys() -> &'static [OnceLock<PrivateKey>; KEY_COUNT] {
    static KEYS: [OnceLock<PrivateKey>; KEY_COUNT] = [const { OnceLock::new() }; KEY_COUNT];
    &KEYS
}

/// Test key number `i` (0 is the CA's), generated on first use.
pub fn key(i: usize) -> PrivateKey {
    let slot = keys().get(i % KEY_COUNT).expect("key slot");
    slot.get_or_init(|| PrivateKey::generate(2048).expect("RSA key generation")).clone()
}

/// The test CA certificate (self-signed with key 0).
pub fn ca() -> Cert {
    static CA: OnceLock<Cert> = OnceLock::new();
    CA.get_or_init(|| issue(&format!("CN={CA_NAME},O=MailCraft Tests"), None, &key(0), None, VALID, true))
        .clone()
}

/// The digital ID for `name` <`email`>, issued by [`ca`], valid for [`VALID`]. Each new identity gets its
/// own key (up to the key pool size).
pub fn identity(name: &str, email: &str) -> Identity {
    static IDS: OnceLock<Mutex<BTreeMap<String, Identity>>> = OnceLock::new();
    let ids = IDS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut map = ids.lock().unwrap_or_else(|p| p.into_inner());
    let k = format!("{name}\u{0}{}", email.to_ascii_lowercase());
    if let Some(id) = map.get(&k) {
        return id.clone();
    }
    let index = 1 + map.len() % (KEY_COUNT - 1);
    let id = identity_with(name, email, index, VALID, Some((&ca(), &key(0))));
    map.insert(k, id.clone());
    id
}

/// A digital ID with key `index`, `validity` (Unix seconds) and `issuer` (None = self-signed).
pub fn identity_with(
    name: &str,
    email: &str,
    index: usize,
    validity: (i64, i64),
    issuer: Option<(&Cert, &PrivateKey)>,
) -> Identity {
    let key = key(index);
    let subject = format!("CN={name},O=Example");
    let cert = issue(&subject, Some(email), &key, issuer, validity, false);
    let chain = issuer.map(|(c, _)| vec![c.clone()]).unwrap_or_default();
    Identity::new(key, cert, chain).expect("key matches")
}

/// Optional certificate contents for [`issue_with`].
#[derive(Clone, Debug, Default)]
pub struct Extras {
    /// Extended key usages instead of the default (emailProtection for a certificate with an address, none
    /// for an authority).
    pub eku: Option<Vec<der::asn1::ObjectIdentifier>>,
    /// An authority's path length constraint.
    pub path_len: Option<u8>,
    /// Leaves keyCertSign out of an authority's key usage.
    pub no_cert_sign: bool,
}

/// Issues a certificate for `subject` with the public half of `key`, signed by `issuer` (or self-signed).
pub fn issue(
    subject: &str,
    email: Option<&str>,
    key: &PrivateKey,
    issuer: Option<(&Cert, &PrivateKey)>,
    validity: (i64, i64),
    ca: bool,
) -> Cert {
    issue_with(subject, email, key, issuer, validity, ca, &Extras::default())
}

/// [`issue`] with [`Extras`].
pub fn issue_with(
    subject: &str,
    email: Option<&str>,
    key: &PrivateKey,
    issuer: Option<(&Cert, &PrivateKey)>,
    validity: (i64, i64),
    ca: bool,
    extras: &Extras,
) -> Cert {
    let subject = Name::from_str(subject).expect("subject");
    let issuer_name = issuer.map(|(c, _)| c.subject_name().clone()).unwrap_or_else(|| subject.clone());
    let spki_der = key.public_key().to_public_key_der().expect("spki");
    let spki = SubjectPublicKeyInfoOwned::try_from(spki_der.as_bytes()).expect("spki");
    let ski = Sha256::digest(spki_der.as_bytes());
    let serial_seed = Sha256::digest([subject.to_string().as_bytes(), &validity.0.to_be_bytes()].concat());
    let mut serial = serial_seed.get(..12).expect("serial").to_vec();
    if let Some(b) = serial.first_mut() {
        *b &= 0x7f;
        *b |= 0x01;
    }
    let alg =
        AlgorithmIdentifierOwned { oid: oid::SHA256_WITH_RSA, parameters: Some(der::asn1::Null.into()) };
    let time = |t: i64| {
        Time::UtcTime(
            UtcTime::from_unix_duration(Duration::from_secs(u64::try_from(t).expect("time"))).expect("time"),
        )
    };
    let usage = match (ca, extras.no_cert_sign) {
        (true, false) => KeyUsages::KeyCertSign | KeyUsages::CRLSign | KeyUsages::DigitalSignature,
        (true, true) => KeyUsages::CRLSign | KeyUsages::DigitalSignature,
        (false, _) => KeyUsages::DigitalSignature | KeyUsages::KeyEncipherment | KeyUsages::NonRepudiation,
    };
    let mut extensions = vec![
        ext(&subject, &BasicConstraints { ca, path_len_constraint: extras.path_len }, true),
        ext(&subject, &KeyUsage(usage), true),
        ext(
            &subject,
            &SubjectKeyIdentifier(OctetString::new(ski.get(..20).expect("ski")).expect("ski")),
            false,
        ),
    ];
    if let Some(e) = email {
        let san = SubjectAltName(vec![GeneralName::Rfc822Name(Ia5String::new(e).expect("ia5"))]);
        extensions.push(ext(&subject, &san, false));
    }
    let eku = match &extras.eku {
        Some(list) => list.clone(),
        None if email.is_some() => vec![oid::EMAIL_PROTECTION],
        None => Vec::new(),
    };
    if !eku.is_empty() {
        extensions.push(ext(&subject, &ExtendedKeyUsage(eku), false));
    }
    let tbs = TbsCertificate {
        version: Version::V3,
        serial_number: SerialNumber::new(&serial).expect("serial"),
        signature: alg.clone(),
        issuer: issuer_name,
        validity: Validity { not_before: time(validity.0), not_after: time(validity.1) },
        subject,
        subject_public_key_info: spki,
        issuer_unique_id: None,
        subject_unique_id: None,
        extensions: Some(extensions),
    };
    let tbs_der = tbs.to_der().expect("tbs");
    let signer = issuer.map(|(_, k)| k).unwrap_or(key);
    let sig = signer.0.sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(&tbs_der)).expect("sign");
    let cert = x509_cert::Certificate {
        tbs_certificate: tbs,
        signature_algorithm: alg,
        signature: BitString::from_bytes(&sig).expect("bits"),
    };
    Cert::from_parsed(cert).expect("cert")
}

fn ext<T: AsExtension>(subject: &Name, e: &T, critical: bool) -> Extension {
    let mut x = e.to_extension(subject, &[]).expect("extension");
    x.critical = critical;
    x
}
