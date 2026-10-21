//! X.509 certificates (RFC 5280): parsing (DER, PEM, PKCS #7 certificate bundles) and the facts S/MIME needs
//! from them: subject and issuer names, e-mail addresses, validity, key usage, fingerprints, the RSA key.

use base64::Engine;
use der::asn1::{BmpString, Ia5String, PrintableStringRef, TeletexStringRef, Utf8StringRef};
use der::{Decode, Encode, Tag, Tagged};
use rsa::RsaPublicKey;
use rsa::pkcs8::DecodePublicKey;
use sha2::Digest;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{
    BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAltName, SubjectKeyIdentifier,
};
use x509_cert::name::Name;

use crate::{MAX_CERT_BYTES, MAX_CERTS, Result, SmimeError, ber, oid};

/// A parsed certificate with its original DER bytes (kept as signed: fingerprints and signatures use them).
#[derive(Clone)]
pub struct Cert {
    der: Vec<u8>,
    inner: x509_cert::Certificate,
}

impl std::fmt::Debug for Cert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cert({}, {})", self.subject(), self.fingerprint())
    }
}

impl PartialEq for Cert {
    fn eq(&self, other: &Self) -> bool {
        self.der == other.der
    }
}

impl Eq for Cert {}

impl Cert {
    /// One DER certificate.
    pub fn from_der(der: &[u8]) -> Result<Cert> {
        if der.len() > MAX_CERT_BYTES {
            return Err(SmimeError::TooLarge(der.len()));
        }
        let (tlv, rest) = ber::read_tlv(der)?;
        if tlv.tag != ber::TAG_SEQUENCE || !rest.is_empty() {
            return Err(SmimeError::Malformed("not a certificate".into()));
        }
        let inner = x509_cert::Certificate::from_der(der)?;
        Ok(Cert { der: der.to_vec(), inner })
    }

    /// From an already decoded certificate (re-encoded to DER).
    pub(crate) fn from_parsed(inner: x509_cert::Certificate) -> Result<Cert> {
        let der = inner.to_der()?;
        Ok(Cert { der, inner })
    }

    /// Every certificate in `bytes`: PEM text (any number of `CERTIFICATE` blocks), one DER certificate, or a
    /// PKCS #7 certificate bundle (`.p7b`, `.p7c`: a SignedData with certificates only), in DER or PEM.
    pub fn parse_any(bytes: &[u8]) -> Result<Vec<Cert>> {
        if bytes.len() > crate::MAX_CMS_BYTES {
            return Err(SmimeError::TooLarge(bytes.len()));
        }
        let text = std::str::from_utf8(bytes).ok();
        if let Some(t) = text.filter(|t| t.contains("-----BEGIN")) {
            let mut out = Vec::new();
            for (label, der) in pem_blocks(t)? {
                match label.as_str() {
                    "CERTIFICATE" | "X509 CERTIFICATE" | "TRUSTED CERTIFICATE" => {
                        out.push(Cert::from_der(&der)?)
                    }
                    "PKCS7" | "CMS" => out.extend(crate::cms::certificates_of(&der)?),
                    _ => {}
                }
                if out.len() > MAX_CERTS {
                    return Err(SmimeError::Malformed("too many certificates".into()));
                }
            }
            if out.is_empty() {
                return Err(SmimeError::Malformed("no certificate found in the text".into()));
            }
            return Ok(out);
        }
        match Cert::from_der(bytes) {
            Ok(c) => Ok(vec![c]),
            Err(first) => match crate::cms::certificates_of(bytes) {
                Ok(v) if !v.is_empty() => Ok(v),
                _ => Err(first),
            },
        }
    }

    pub fn der(&self) -> &[u8] {
        &self.der
    }

    pub(crate) fn x509(&self) -> &x509_cert::Certificate {
        &self.inner
    }

    /// The subject as an RFC 4514 string (`CN=Ann Lee,O=Example`).
    pub fn subject(&self) -> String {
        self.inner.tbs_certificate.subject.to_string()
    }

    /// The issuer as an RFC 4514 string.
    pub fn issuer(&self) -> String {
        self.inner.tbs_certificate.issuer.to_string()
    }

    pub(crate) fn subject_name(&self) -> &Name {
        &self.inner.tbs_certificate.subject
    }

    pub(crate) fn issuer_name(&self) -> &Name {
        &self.inner.tbs_certificate.issuer
    }

    /// The subject's common name, else its first e-mail address, else the whole subject.
    pub fn display_name(&self) -> String {
        name_attr(self.subject_name(), oid::COMMON_NAME)
            .or_else(|| self.emails().into_iter().next())
            .unwrap_or_else(|| self.subject())
    }

    /// The issuer's common name, else the whole issuer.
    pub fn issuer_display_name(&self) -> String {
        name_attr(self.issuer_name(), oid::COMMON_NAME).unwrap_or_else(|| self.issuer())
    }

    /// E-mail addresses of the subject (subject alternative name `rfc822Name`s, then the legacy subject
    /// `emailAddress` attribute), lower-case, without duplicates.
    pub fn emails(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut add = |a: &str| {
            let a = a.trim().to_ascii_lowercase();
            if !a.is_empty() && a.contains('@') && !out.contains(&a) {
                out.push(a);
            }
        };
        if let Ok(Some((_, san))) = self.inner.tbs_certificate.get::<SubjectAltName>() {
            for n in &san.0 {
                if let GeneralName::Rfc822Name(s) = n {
                    add(s.as_str());
                }
            }
        }
        for rdn in &self.subject_name().0 {
            for atv in rdn.0.iter() {
                if atv.oid == oid::EMAIL_ADDRESS
                    && let Some(s) = any_string(&atv.value)
                {
                    add(&s);
                }
            }
        }
        out
    }

    /// True when `address` is one of the certificate's addresses (case-insensitive).
    pub fn has_email(&self, address: &str) -> bool {
        let a = address.trim().to_ascii_lowercase();
        !a.is_empty() && self.emails().contains(&a)
    }

    /// Serial number in hex (as stored, leading zero bytes dropped).
    pub fn serial_hex(&self) -> String {
        crate::hex(self.inner.tbs_certificate.serial_number.as_bytes())
    }

    /// Start of the validity period (Unix seconds).
    pub fn not_before(&self) -> i64 {
        unix(self.inner.tbs_certificate.validity.not_before)
    }

    /// End of the validity period (Unix seconds).
    pub fn not_after(&self) -> i64 {
        unix(self.inner.tbs_certificate.validity.not_after)
    }

    /// True when `at` (Unix seconds) is inside the validity period.
    pub fn valid_at(&self, at: i64) -> bool {
        self.not_before() <= at && at <= self.not_after()
    }

    /// SHA-256 fingerprint (lower-case hex): the certificate's id in MailCraft.
    pub fn fingerprint(&self) -> String {
        crate::hex(&sha2::Sha256::digest(&self.der))
    }

    /// SHA-1 thumbprint (lower-case hex), as many programs show it.
    pub fn sha1_thumbprint(&self) -> String {
        crate::hex(&sha1::Sha1::digest(&self.der))
    }

    /// True for a certificate authority (basic constraints `cA`).
    pub fn is_ca(&self) -> bool {
        matches!(self.inner.tbs_certificate.get::<BasicConstraints>(), Ok(Some((_, bc))) if bc.ca)
    }

    /// May issue certificates: a certificate authority whose key usage (when present) allows keyCertSign.
    pub fn can_issue(&self) -> bool {
        let ku = match self.inner.tbs_certificate.get::<KeyUsage>() {
            Ok(Some((_, ku))) => ku.key_cert_sign(),
            Ok(None) => true,
            Err(_) => false,
        };
        self.is_ca() && ku
    }

    /// The basic constraints path length: how many intermediate authorities may follow this one (None: no
    /// limit).
    pub fn path_len(&self) -> Option<u8> {
        match self.inner.tbs_certificate.get::<BasicConstraints>() {
            Ok(Some((_, bc))) => bc.path_len_constraint,
            _ => None,
        }
    }

    /// True when subject and issuer are the same name (a root or a self-signed certificate).
    pub fn self_issued(&self) -> bool {
        self.subject_name() == self.issuer_name()
    }

    /// May sign mail: key usage absent or with digitalSignature / nonRepudiation, and the extended key usage
    /// absent or allowing e-mail protection.
    pub fn can_sign(&self) -> bool {
        let ku = match self.inner.tbs_certificate.get::<KeyUsage>() {
            Ok(Some((_, ku))) => ku.digital_signature() || ku.non_repudiation(),
            Ok(None) => true,
            Err(_) => false,
        };
        ku && self.email_protection()
    }

    /// May receive encrypted mail: key usage absent or with keyEncipherment, the extended key usage absent or
    /// allowing e-mail protection, and an RSA key.
    pub fn can_encrypt(&self) -> bool {
        let ku = match self.inner.tbs_certificate.get::<KeyUsage>() {
            Ok(Some((_, ku))) => ku.key_encipherment(),
            Ok(None) => true,
            Err(_) => false,
        };
        ku && self.email_protection() && self.rsa_public_key().is_ok()
    }

    /// The extended key usage is absent or allows e-mail protection (for authorities: may issue e-mail
    /// certificates).
    pub fn email_protection(&self) -> bool {
        match self.inner.tbs_certificate.get::<ExtendedKeyUsage>() {
            Ok(Some((_, eku))) => {
                eku.0.iter().any(|o| *o == oid::EMAIL_PROTECTION || *o == oid::ANY_EXTENDED_KEY_USAGE)
            }
            Ok(None) => true,
            Err(_) => false,
        }
    }

    /// The subject key identifier extension, when present.
    pub fn subject_key_id(&self) -> Option<Vec<u8>> {
        match self.inner.tbs_certificate.get::<SubjectKeyIdentifier>() {
            Ok(Some((_, ski))) => Some(ski.0.as_bytes().to_vec()),
            _ => None,
        }
    }

    /// The RSA public key (other key types are not supported).
    pub fn rsa_public_key(&self) -> Result<RsaPublicKey> {
        let spki = &self.inner.tbs_certificate.subject_public_key_info;
        if spki.algorithm.oid != oid::RSA_ENCRYPTION {
            return Err(SmimeError::Unsupported(format!(
                "certificate key type {} (only RSA keys are supported)",
                spki.algorithm.oid
            )));
        }
        let der = spki.to_der()?;
        RsaPublicKey::from_public_key_der(&der).map_err(|e| SmimeError::Malformed(format!("RSA key: {e}")))
    }

    /// Size of the public key in bits (0 when it is not an RSA key).
    pub fn key_bits(&self) -> usize {
        use rsa::traits::PublicKeyParts;
        self.rsa_public_key().map(|k| k.size().saturating_mul(8)).unwrap_or(0)
    }

    /// The signed part of the certificate (`tbsCertificate`), as it appears in the DER.
    pub(crate) fn tbs_der(&self) -> Result<&[u8]> {
        let (outer, _) = ber::read_tlv(&self.der)?;
        let (tbs, _) = ber::read_tlv(outer.content)?;
        Ok(tbs.raw)
    }

    /// The certificate as PEM text.
    pub fn to_pem(&self) -> String {
        pem("CERTIFICATE", &self.der)
    }
}

/// Seconds since the Unix epoch of a certificate time.
fn unix(t: x509_cert::time::Time) -> i64 {
    i64::try_from(t.to_unix_duration().as_secs()).unwrap_or(i64::MAX)
}

/// The first value of attribute `oid` in `name`, as text.
pub(crate) fn name_attr(name: &Name, want: der::asn1::ObjectIdentifier) -> Option<String> {
    name.0.iter().flat_map(|rdn| rdn.0.iter()).find(|a| a.oid == want).and_then(|a| any_string(&a.value))
}

/// A directory string (UTF8String, PrintableString, IA5String, TeletexString or BMPString) as text.
fn any_string(v: &der::Any) -> Option<String> {
    let bytes = v.value();
    match v.tag() {
        Tag::Utf8String => Utf8StringRef::new(bytes).ok().map(|s| s.as_str().to_owned()),
        Tag::PrintableString => PrintableStringRef::new(bytes).ok().map(|s| s.as_str().to_owned()),
        Tag::Ia5String => Ia5String::new(bytes).ok().map(|s| s.as_str().to_owned()),
        Tag::TeletexString => TeletexStringRef::new(bytes).ok().map(|s| s.as_str().to_owned()),
        Tag::BmpString => BmpString::from_ucs2(bytes.to_vec()).ok().map(|s| s.to_string()),
        _ => None,
    }
}

/// The base64 blocks of PEM text as (label, DER), at most [`MAX_CERTS`] + 1.
pub fn pem_blocks(text: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for line in text.lines() {
        let l = line.trim();
        if let Some(label) = l.strip_prefix("-----BEGIN ").and_then(|r| r.strip_suffix("-----")) {
            cur = Some((label.trim().to_owned(), String::new()));
        } else if l.starts_with("-----END ") {
            if let Some((label, b64)) = cur.take() {
                let der = base64::engine::general_purpose::STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|e| SmimeError::Malformed(format!("PEM base64: {e}")))?;
                out.push((label, der));
                if out.len() > MAX_CERTS {
                    break;
                }
            }
        } else if let Some((_, b64)) = cur.as_mut() {
            // Encapsulated headers (Proc-Type, DEK-Info) are not base64.
            if !l.contains(':') {
                b64.push_str(l);
            }
        }
    }
    Ok(out)
}

/// PEM text for `der` under `label`.
pub fn pem(label: &str, der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    #[test]
    fn facts_of_a_user_certificate() {
        let id = testkit::identity("Ann Lee", "Ann@Example.com");
        let c = &id.cert;
        assert_eq!(c.display_name(), "Ann Lee");
        assert_eq!(c.emails(), vec!["ann@example.com".to_string()]);
        assert!(c.has_email("ANN@example.com") && !c.has_email("bob@example.com"));
        assert!(c.can_sign() && c.can_encrypt());
        assert!(!c.is_ca() && !c.self_issued());
        assert_eq!(c.issuer_display_name(), testkit::CA_NAME);
        assert_eq!(c.fingerprint().len(), 64);
        assert_eq!(c.sha1_thumbprint().len(), 40);
        assert_eq!(c.key_bits(), 2048);
        assert!(c.valid_at(testkit::NOW) && !c.valid_at(c.not_after() + 1));
        assert!(!c.serial_hex().is_empty());
        let ca = testkit::ca();
        assert!(ca.is_ca() && ca.self_issued());
    }

    #[test]
    fn pem_der_and_bundles_parse() {
        let id = testkit::identity("Ann Lee", "ann@example.com");
        let ca = testkit::ca();
        let pem_text = format!("junk before\n{}{}", id.cert.to_pem(), ca.to_pem());
        let got = Cert::parse_any(pem_text.as_bytes()).unwrap();
        assert_eq!(got, vec![id.cert.clone(), ca.clone()]);
        assert_eq!(Cert::parse_any(id.cert.der()).unwrap(), vec![id.cert.clone()]);
        // A PKCS #7 bundle (certificates-only SignedData).
        let p7 = crate::cms::certificate_bundle(&[id.cert.clone(), ca.clone()]).unwrap();
        let got = Cert::parse_any(&p7).unwrap();
        assert_eq!(got.len(), 2);
        let p7_pem = pem("PKCS7", &p7);
        assert_eq!(Cert::parse_any(p7_pem.as_bytes()).unwrap().len(), 2);
    }

    #[test]
    fn junk_is_refused_without_panicking() {
        for junk in [
            &b""[..],
            b"\x30",
            b"\x30\x03\x02\x01\x01",
            b"-----BEGIN CERTIFICATE-----\n!!\n-----END CERTIFICATE-----",
        ] {
            assert!(Cert::parse_any(junk).is_err());
            assert!(Cert::from_der(junk).is_err());
        }
        let id = testkit::identity("Ann Lee", "ann@example.com");
        let mut der = id.cert.der().to_vec();
        for i in (0..der.len()).step_by(7) {
            let keep = der.get(i).copied().unwrap_or(0);
            if let Some(b) = der.get_mut(i) {
                *b ^= 0x5a;
            }
            if let Ok(c) = Cert::from_der(&der) {
                let _ = (c.emails(), c.display_name(), c.can_sign(), c.can_encrypt(), c.subject_key_id());
            }
            if let Some(b) = der.get_mut(i) {
                *b = keep;
            }
        }
        for n in 0..der.len() {
            let _ = Cert::from_der(der.get(..n).unwrap());
        }
    }
}
