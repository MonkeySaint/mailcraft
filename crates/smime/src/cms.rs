//! CMS (RFC 5652) for S/MIME: SignedData (sign, verify) and EnvelopedData (encrypt, decrypt).
//!
//! Written: SHA-256 digests, RSA PKCS #1 v1.5 signatures with the signed attributes content type, signing
//! time, message digest and S/MIME capabilities (RFC 8551 §2.5); AES-256-CBC content encryption (RFC 3565)
//! with RSA PKCS #1 v1.5 key transport to every recipient, identified by issuer and serial number.
//!
//! Read: SHA-1 / SHA-224 / SHA-256 / SHA-384 / SHA-512 digests, RSA PKCS #1 v1.5 and RSASSA-PSS signatures,
//! signers identified by issuer and serial number or by subject key identifier; AES-128/192/256-CBC and
//! triple-DES content, RSA PKCS #1 v1.5 and RSAES-OAEP key transport. BER input is normalised first
//! ([`crate::ber`]). Anything else is reported as unsupported; nothing panics.

use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::{CmsVersion, ContentInfo};
use cms::enveloped_data::{
    EncryptedContentInfo, EnvelopedData, KeyTransRecipientInfo, RecipientIdentifier, RecipientInfo,
    RecipientInfos,
};
use cms::signed_data::{
    CertificateSet, EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo, SignerInfos,
};
use der::asn1::{Any, ObjectIdentifier, OctetString, SetOfVec};
use der::{Decode, Encode};
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::{Oaep, Pkcs1v15Encrypt, Pss};
use sha2::Digest;
use spki::AlgorithmIdentifierOwned;
use x509_cert::attr::Attribute;
use x509_cert::time::Time;
use zeroize::Zeroizing;

use crate::ber::{self, TAG_SEQUENCE};
use crate::{Cert, Identity, MAX_CERTS, MAX_CMS_BYTES, MAX_RECIPIENTS, MAX_SIGNERS, Result, SmimeError, oid};

/// A message digest algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigestAlg {
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

impl DigestAlg {
    pub fn from_oid(o: &ObjectIdentifier) -> Option<DigestAlg> {
        Some(match *o {
            x if x == oid::SHA1 => DigestAlg::Sha1,
            x if x == oid::SHA224 => DigestAlg::Sha224,
            x if x == oid::SHA256 => DigestAlg::Sha256,
            x if x == oid::SHA384 => DigestAlg::Sha384,
            x if x == oid::SHA512 => DigestAlg::Sha512,
            _ => return None,
        })
    }

    pub fn oid(self) -> ObjectIdentifier {
        match self {
            DigestAlg::Sha1 => oid::SHA1,
            DigestAlg::Sha224 => oid::SHA224,
            DigestAlg::Sha256 => oid::SHA256,
            DigestAlg::Sha384 => oid::SHA384,
            DigestAlg::Sha512 => oid::SHA512,
        }
    }

    /// Display name ("SHA-256").
    pub fn name(self) -> &'static str {
        match self {
            DigestAlg::Sha1 => "SHA-1",
            DigestAlg::Sha224 => "SHA-224",
            DigestAlg::Sha256 => "SHA-256",
            DigestAlg::Sha384 => "SHA-384",
            DigestAlg::Sha512 => "SHA-512",
        }
    }

    /// The `micalg` parameter of multipart/signed (RFC 8551 §3.5.3.2).
    pub fn micalg(self) -> &'static str {
        match self {
            DigestAlg::Sha1 => "sha-1",
            DigestAlg::Sha224 => "sha-224",
            DigestAlg::Sha256 => "sha-256",
            DigestAlg::Sha384 => "sha-384",
            DigestAlg::Sha512 => "sha-512",
        }
    }

    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            DigestAlg::Sha1 => sha1::Sha1::digest(data).to_vec(),
            DigestAlg::Sha224 => sha2::Sha224::digest(data).to_vec(),
            DigestAlg::Sha256 => sha2::Sha256::digest(data).to_vec(),
            DigestAlg::Sha384 => sha2::Sha384::digest(data).to_vec(),
            DigestAlg::Sha512 => sha2::Sha512::digest(data).to_vec(),
        }
    }

    fn pkcs1(self) -> Pkcs1v15Sign {
        match self {
            DigestAlg::Sha1 => Pkcs1v15Sign::new::<sha1::Sha1>(),
            DigestAlg::Sha224 => Pkcs1v15Sign::new::<sha2::Sha224>(),
            DigestAlg::Sha256 => Pkcs1v15Sign::new::<sha2::Sha256>(),
            DigestAlg::Sha384 => Pkcs1v15Sign::new::<sha2::Sha384>(),
            DigestAlg::Sha512 => Pkcs1v15Sign::new::<sha2::Sha512>(),
        }
    }

    fn pss(self, salt: usize) -> Pss {
        match self {
            DigestAlg::Sha1 => Pss::new_with_salt::<sha1::Sha1>(salt),
            DigestAlg::Sha224 => Pss::new_with_salt::<sha2::Sha224>(salt),
            DigestAlg::Sha256 => Pss::new_with_salt::<sha2::Sha256>(salt),
            DigestAlg::Sha384 => Pss::new_with_salt::<sha2::Sha384>(salt),
            DigestAlg::Sha512 => Pss::new_with_salt::<sha2::Sha512>(salt),
        }
    }

    fn oaep(self) -> Oaep {
        match self {
            DigestAlg::Sha1 => Oaep::new::<sha1::Sha1>(),
            DigestAlg::Sha224 => Oaep::new::<sha2::Sha224>(),
            DigestAlg::Sha256 => Oaep::new::<sha2::Sha256>(),
            DigestAlg::Sha384 => Oaep::new::<sha2::Sha384>(),
            DigestAlg::Sha512 => Oaep::new::<sha2::Sha512>(),
        }
    }
}

/// What a CMS object is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmsKind {
    Signed,
    Enveloped,
    /// AuthEnvelopedData (AES-GCM, RFC 5083): recognised, not supported.
    AuthEnveloped,
    Other,
}

/// The kind of the CMS `ContentInfo` in `input` (DER or BER).
pub fn kind(input: &[u8]) -> Result<CmsKind> {
    let ci = content_info(input)?;
    Ok(match ci.content_type {
        t if t == oid::SIGNED_DATA => CmsKind::Signed,
        t if t == oid::ENVELOPED_DATA => CmsKind::Enveloped,
        t if t == oid::AUTH_ENVELOPED_DATA => CmsKind::AuthEnveloped,
        _ => CmsKind::Other,
    })
}

fn content_info(input: &[u8]) -> Result<ContentInfo> {
    if input.len() > MAX_CMS_BYTES {
        return Err(SmimeError::TooLarge(input.len()));
    }
    let der = ber::to_der(input)?;
    Ok(ContentInfo::from_der(&der)?)
}

fn wrap(content_type: ObjectIdentifier, value: &impl Encode) -> Result<Vec<u8>> {
    let content = Any::from_der(&value.to_der()?)?;
    Ok(ContentInfo { content_type, content }.to_der()?)
}

fn issuer_and_serial(c: &Cert) -> IssuerAndSerialNumber {
    let tbs = &c.x509().tbs_certificate;
    IssuerAndSerialNumber { issuer: tbs.issuer.clone(), serial_number: tbs.serial_number.clone() }
}

fn null_params(o: ObjectIdentifier) -> AlgorithmIdentifierOwned {
    AlgorithmIdentifierOwned { oid: o, parameters: Some(der::asn1::Null.into()) }
}

fn attribute(o: ObjectIdentifier, value: Any) -> Result<Attribute> {
    let mut values = SetOfVec::new();
    values.insert(value)?;
    Ok(Attribute { oid: o, values })
}

/// A CMS time: UTCTime through 2049, GeneralizedTime after (RFC 5652 §11.3).
fn cms_time(unix: i64) -> Result<Time> {
    let secs = u64::try_from(unix.max(0)).unwrap_or(0);
    let d = std::time::Duration::from_secs(secs);
    if secs < 2_524_608_000 {
        Ok(Time::UtcTime(der::asn1::UtcTime::from_unix_duration(d)?))
    } else {
        Ok(Time::GeneralTime(der::asn1::GeneralizedTime::from_unix_duration(d)?))
    }
}

/// The certificates of `id` as a CMS certificate set (the signer's first, then the chain).
fn certificate_set(certs: &[&Cert]) -> Result<CertificateSet> {
    let mut set = SetOfVec::new();
    for c in certs.iter().take(MAX_CERTS) {
        let choice = CertificateChoices::Certificate(c.x509().clone());
        if !set.iter().any(|x| *x == choice) {
            set.insert(choice)?;
        }
    }
    Ok(CertificateSet(set))
}

// =========================================================================================================
// SignedData
// =========================================================================================================

/// Signs `content` with `id` at time `now` (Unix seconds): a SignedData `ContentInfo` in DER. `detached`
/// leaves the content out (for multipart/signed); otherwise it is carried inside (application/pkcs7-mime).
pub fn sign(content: &[u8], id: &Identity, now: i64, detached: bool) -> Result<Vec<u8>> {
    if content.len() > MAX_CMS_BYTES {
        return Err(SmimeError::TooLarge(content.len()));
    }
    let alg = DigestAlg::Sha256;
    let digest = alg.digest(content);
    let caps = smime_capabilities()?;
    let attrs = vec![
        attribute(oid::CONTENT_TYPE, Any::encode_from(&oid::DATA)?)?,
        attribute(oid::SIGNING_TIME, Any::from_der(&cms_time(now)?.to_der()?)?)?,
        attribute(oid::MESSAGE_DIGEST, Any::encode_from(&OctetString::new(digest)?)?)?,
        attribute(oid::SMIME_CAPABILITIES, Any::from_der(&caps)?)?,
        attribute(
            oid::ENCRYPTION_KEY_PREFERENCE,
            Any::from_der(&implicit0(&issuer_and_serial(&id.cert).to_der()?)?)?,
        )?,
    ];
    let signed_attrs = SetOfVec::try_from(attrs)?;
    let to_sign = signed_attrs.to_der()?;
    let mut rng = crate::rng()?;
    let signature = id
        .key
        .0
        .sign_with_rng(&mut rng, alg.pkcs1(), &alg.digest(&to_sign))
        .map_err(|e| SmimeError::Crypto(format!("signing failed: {e}")))?;
    let si = SignerInfo {
        version: CmsVersion::V1,
        sid: SignerIdentifier::IssuerAndSerialNumber(issuer_and_serial(&id.cert)),
        digest_alg: AlgorithmIdentifierOwned { oid: alg.oid(), parameters: None },
        signed_attrs: Some(signed_attrs),
        signature_algorithm: null_params(oid::RSA_ENCRYPTION),
        signature: OctetString::new(signature)?,
        unsigned_attrs: None,
    };
    let mut digests = SetOfVec::new();
    digests.insert(AlgorithmIdentifierOwned { oid: alg.oid(), parameters: None })?;
    let econtent = if detached { None } else { Some(Any::encode_from(&OctetString::new(content)?)?) };
    let mut certs: Vec<&Cert> = vec![&id.cert];
    certs.extend(id.chain.iter().filter(|c| !c.self_issued() || c.is_ca()));
    let sd = SignedData {
        version: CmsVersion::V1,
        digest_algorithms: digests,
        encap_content_info: EncapsulatedContentInfo { econtent_type: oid::DATA, econtent },
        certificates: Some(certificate_set(&certs)?),
        crls: None,
        signer_infos: SignerInfos(SetOfVec::try_from(vec![si])?),
    };
    wrap(oid::SIGNED_DATA, &sd)
}

/// Re-tags a DER SEQUENCE as `[0] IMPLICIT` (constructed context tag 0).
fn implicit0(der: &[u8]) -> Result<Vec<u8>> {
    let (t, _) = ber::read_tlv(der)?;
    Ok(ber::tlv(0xa0, t.content))
}

/// SMIMECapabilities (RFC 8551 §2.5.2): AES-256-CBC, AES-192-CBC, AES-128-CBC, RSA key transport and the
/// SHA-2 digests, in order of preference.
fn smime_capabilities() -> Result<Vec<u8>> {
    let mut body = Vec::new();
    for o in
        [oid::AES256_CBC, oid::AES192_CBC, oid::AES128_CBC, oid::RSA_ENCRYPTION, oid::SHA256, oid::SHA512]
    {
        body.extend(ber::tlv(TAG_SEQUENCE, &o.to_der()?));
    }
    Ok(ber::tlv(TAG_SEQUENCE, &body))
}

/// One signer's check.
#[derive(Clone, Debug)]
pub struct SignerCheck {
    /// The signer's certificate, when the SignedData carries it.
    pub cert: Option<Cert>,
    pub digest: Option<DigestAlg>,
    /// The signing-time attribute (Unix seconds), when present.
    pub signing_time: Option<i64>,
    /// `Ok` when the signature and the message digest match; otherwise why not.
    pub result: std::result::Result<(), String>,
}

/// What [`verify`] found.
#[derive(Clone, Debug)]
pub struct Verified {
    /// The signed content (the detached content, or the one carried inside).
    pub content: Vec<u8>,
    /// One entry per signer (at most [`MAX_SIGNERS`]).
    pub signers: Vec<SignerCheck>,
    /// Every certificate the SignedData carries.
    pub certs: Vec<Cert>,
}

/// Checks a SignedData `ContentInfo` (DER or BER). `detached` is the signed content of a multipart/signed
/// message; without it the content must be inside. Errors only when the structure itself is unusable; bad
/// signatures are reported per signer.
pub fn verify(input: &[u8], detached: Option<&[u8]>) -> Result<Verified> {
    let der = ber::to_der(input)?;
    if der.len() > MAX_CMS_BYTES {
        return Err(SmimeError::TooLarge(der.len()));
    }
    let ci = ContentInfo::from_der(&der)?;
    if ci.content_type != oid::SIGNED_DATA {
        return Err(SmimeError::Malformed("the signature is not CMS signed data".into()));
    }
    let sd: SignedData = ci.content.decode_as()?;
    let content = match (detached, &sd.encap_content_info.econtent) {
        (Some(d), _) => d.to_vec(),
        (None, Some(e)) => e.decode_as::<OctetString>()?.into_bytes(),
        (None, None) => return Err(SmimeError::Malformed("the signed content is missing".into())),
    };
    let certs = signed_data_certs(&sd);
    let econtent_type = sd.encap_content_info.econtent_type;
    // Each signer is checked from its own encoding, in the order sent: the decoded set is sorted, and nothing
    // of one SignerInfo (its attributes, its signature) may be used to check another.
    let signers = raw_signers(ci.content.value())?
        .iter()
        .map(|raw| match SignerInfo::from_der(raw.info) {
            Ok(si) => check_signer(&si, raw.attrs.as_deref(), &content, econtent_type, &certs),
            Err(e) => SignerCheck {
                cert: None,
                digest: None,
                signing_time: None,
                result: Err(format!("the signer information is damaged ({e})")),
            },
        })
        .collect::<Vec<_>>();
    if signers.is_empty() {
        return Err(SmimeError::Malformed("the signature has no signers".into()));
    }
    Ok(Verified { content, signers, certs })
}

fn signed_data_certs(sd: &SignedData) -> Vec<Cert> {
    sd.certificates
        .as_ref()
        .map(|set| {
            set.0
                .iter()
                .filter_map(|c| match c {
                    CertificateChoices::Certificate(x) => Cert::from_parsed(x.clone()).ok(),
                    CertificateChoices::Other(_) => None,
                })
                .take(MAX_CERTS)
                .collect()
        })
        .unwrap_or_default()
}

/// The certificates carried by a SignedData (a `.p7b` / `.p7c` bundle, or a signature).
pub fn certificates_of(input: &[u8]) -> Result<Vec<Cert>> {
    let ci = content_info(input)?;
    if ci.content_type != oid::SIGNED_DATA {
        return Err(SmimeError::Malformed("not a certificate bundle".into()));
    }
    let sd: SignedData = ci.content.decode_as()?;
    Ok(signed_data_certs(&sd))
}

/// A certificates-only SignedData (a `.p7b` bundle) holding `certs`.
pub fn certificate_bundle(certs: &[Cert]) -> Result<Vec<u8>> {
    let refs: Vec<&Cert> = certs.iter().collect();
    let sd = SignedData {
        version: CmsVersion::V1,
        digest_algorithms: SetOfVec::new(),
        encap_content_info: EncapsulatedContentInfo { econtent_type: oid::DATA, econtent: None },
        certificates: Some(certificate_set(&refs)?),
        crls: None,
        signer_infos: SignerInfos(SetOfVec::new()),
    };
    wrap(oid::SIGNED_DATA, &sd)
}

/// One SignerInfo as sent: its whole encoding, and its signed attributes re-tagged as a SET (the bytes the
/// signature covers; the decoder sorts sets, so a signature over attributes that a sender did not sort can
/// only be checked against the bytes as they arrived).
struct RawSigner<'a> {
    info: &'a [u8],
    attrs: Option<Vec<u8>>,
}

/// The SignerInfos of the encoded SignedData value, in the order sent (at most [`MAX_SIGNERS`]).
fn raw_signers(signed_data: &[u8]) -> Result<Vec<RawSigner<'_>>> {
    let damaged = || SmimeError::Malformed("the signer information is damaged".into());
    let fields = ber::children(signed_data, 16)?;
    let infos = fields.last().filter(|f| f.tag == ber::TAG_SET).ok_or_else(damaged)?;
    let list = ber::children(infos.content, MAX_SIGNERS)
        .map_err(|_| SmimeError::Unsupported(format!("more than {MAX_SIGNERS} signers")))?;
    let mut out = Vec::new();
    for si in &list {
        if si.tag != TAG_SEQUENCE {
            return Err(damaged());
        }
        let parts = ber::children(si.content, 16)?;
        let attrs = parts.iter().find(|p| p.tag == 0xa0).map(|a| ber::tlv(ber::TAG_SET, a.content));
        out.push(RawSigner { info: si.raw, attrs });
    }
    Ok(out)
}

fn find_signer<'a>(sid: &SignerIdentifier, certs: &'a [Cert]) -> Option<&'a Cert> {
    certs.iter().find(|c| match sid {
        SignerIdentifier::IssuerAndSerialNumber(ias) => {
            let tbs = &c.x509().tbs_certificate;
            tbs.issuer == ias.issuer && tbs.serial_number == ias.serial_number
        }
        SignerIdentifier::SubjectKeyIdentifier(ski) => {
            c.subject_key_id().is_some_and(|k| k.as_slice() == ski.0.as_bytes())
        }
    })
}

/// Checks one signer. `raw_attrs` are the signed attributes exactly as this SignerInfo carried them
/// ([`RawSigner`]); `si` is decoded from the same SignerInfo.
fn check_signer(
    si: &SignerInfo,
    raw_attrs: Option<&[u8]>,
    content: &[u8],
    econtent_type: ObjectIdentifier,
    certs: &[Cert],
) -> SignerCheck {
    let cert = find_signer(&si.sid, certs).cloned();
    let digest = DigestAlg::from_oid(&si.digest_alg.oid);
    let mut check = SignerCheck { cert: cert.clone(), digest, signing_time: None, result: Ok(()) };
    let Some(alg) = digest else {
        check.result = Err(format!("the digest algorithm {} is not supported", si.digest_alg.oid));
        return check;
    };
    let actual = alg.digest(content);
    let mut to_verify: Vec<Vec<u8>> = Vec::new();
    if let Some(attrs) = &si.signed_attrs {
        // RFC 5652 §5.3: with signed attributes there is exactly one content type and one message digest,
        // each with a single value.
        let single = |o: ObjectIdentifier| {
            let mut found = attrs.iter().filter(|a| a.oid == o);
            match (found.next(), found.next()) {
                (Some(a), None) if a.values.len() == 1 => a.values.iter().next(),
                _ => None,
            }
        };
        let type_ok = single(oid::CONTENT_TYPE)
            .is_some_and(|v| v.decode_as::<ObjectIdentifier>().is_ok_and(|t| t == econtent_type));
        let digest_ok = single(oid::MESSAGE_DIGEST)
            .is_some_and(|v| v.decode_as::<OctetString>().is_ok_and(|d| crate::ct_eq(d.as_bytes(), &actual)));
        if let Some(v) = single(oid::SIGNING_TIME)
            && let Ok(t) = v.to_der().and_then(|d| Time::from_der(&d))
        {
            check.signing_time = i64::try_from(t.to_unix_duration().as_secs()).ok();
        }
        if !type_ok {
            check.result = Err("the signed content type does not match".into());
            return check;
        }
        if !digest_ok {
            check.result =
                Err("the message was changed after it was signed (the digest does not match)".into());
            return check;
        }
        // This signer's attributes as sent, then their DER form (the same attributes, sorted).
        if let Some(raw) = raw_attrs {
            to_verify.push(raw.to_vec());
        }
        match attrs.to_der() {
            Ok(d) if !to_verify.contains(&d) => to_verify.push(d),
            _ => {}
        }
    } else {
        to_verify.push(content.to_vec());
    }
    let Some(cert) = cert else {
        check.result =
            Err("the signer's certificate is not included, so the signature cannot be checked".into());
        return check;
    };
    let key = match cert.rsa_public_key() {
        Ok(k) => k,
        Err(e) => {
            check.result = Err(e.to_string());
            return check;
        }
    };
    let sig = si.signature.as_bytes();
    let sig_oid = si.signature_algorithm.oid;
    let verify_one = |data: &[u8]| -> std::result::Result<(), String> {
        let hashed = alg.digest(data);
        let res = if sig_oid == oid::RSASSA_PSS {
            let (pss_alg, salt) = pss_params(si.signature_algorithm.parameters.as_ref())
                .ok_or_else(|| "the RSASSA-PSS parameters are not supported".to_string())?;
            if pss_alg != alg {
                return Err("the RSASSA-PSS digest does not match the signer's digest".into());
            }
            key.verify(alg.pss(salt), &hashed, sig)
        } else if [
            oid::RSA_ENCRYPTION,
            oid::SHA1_WITH_RSA,
            oid::SHA224_WITH_RSA,
            oid::SHA256_WITH_RSA,
            oid::SHA384_WITH_RSA,
            oid::SHA512_WITH_RSA,
        ]
        .contains(&sig_oid)
        {
            key.verify(alg.pkcs1(), &hashed, sig)
        } else {
            return Err(format!("the signature algorithm {sig_oid} is not supported"));
        };
        res.map_err(|_| "the signature does not match the message".to_string())
    };
    let mut last = Err("the signature does not match the message".to_string());
    for data in &to_verify {
        last = verify_one(data);
        if last.is_ok() {
            break;
        }
    }
    check.result = last;
    check
}

/// RSASSA-PSS-params (RFC 4055): the hash and salt length (defaults SHA-1 and 20).
fn pss_params(params: Option<&Any>) -> Option<(DigestAlg, usize)> {
    let Some(p) = params else { return Some((DigestAlg::Sha1, 20)) };
    let mut alg = DigestAlg::Sha1;
    let mut salt = 20usize;
    for f in ber::children(p.value(), 4).ok()? {
        match f.tag {
            0xa0 => {
                let (inner, _) = ber::read_tlv(f.content).ok()?;
                let (o, _) = ber::read_tlv(inner.content).ok()?;
                alg = DigestAlg::from_oid(&ObjectIdentifier::from_bytes(o.content).ok()?)?;
            }
            0xa2 => {
                let (i, _) = ber::read_tlv(f.content).ok()?;
                salt = i.content.iter().try_fold(0usize, |acc, b| {
                    acc.checked_mul(256).and_then(|a| a.checked_add(usize::from(*b)))
                })?;
                if salt > 1024 {
                    return None;
                }
            }
            _ => {}
        }
    }
    Some((alg, salt))
}

// =========================================================================================================
// EnvelopedData
// =========================================================================================================

/// Encrypts `content` for `recipients` (AES-256-CBC, RSA key transport): an EnvelopedData `ContentInfo` in
/// DER. Duplicate certificates are named once.
pub fn encrypt(content: &[u8], recipients: &[Cert]) -> Result<Vec<u8>> {
    if content.len() > MAX_CMS_BYTES {
        return Err(SmimeError::TooLarge(content.len()));
    }
    if recipients.is_empty() {
        return Err(SmimeError::Crypto("there is nobody to encrypt the message for".into()));
    }
    if recipients.len() > MAX_RECIPIENTS {
        return Err(SmimeError::Unsupported(format!("more than {MAX_RECIPIENTS} recipients")));
    }
    let key = Zeroizing::new(crate::random_bytes::<32>()?);
    let iv = crate::random_bytes::<16>()?;
    let enc = cbc::Encryptor::<aes::Aes256>::new_from_slices(key.as_slice(), &iv)
        .map_err(|e| SmimeError::Crypto(e.to_string()))?;
    let ciphertext = enc.encrypt_padded_vec_mut::<Pkcs7>(content);
    let mut rng = crate::rng()?;
    let mut infos = SetOfVec::new();
    let mut seen: Vec<String> = Vec::new();
    for r in recipients {
        let fp = r.fingerprint();
        if seen.contains(&fp) {
            continue;
        }
        seen.push(fp);
        let pk = r.rsa_public_key()?;
        let ek = pk
            .encrypt(&mut rng, Pkcs1v15Encrypt, key.as_slice())
            .map_err(|e| SmimeError::Crypto(format!("encrypting for {}: {e}", r.display_name())))?;
        infos.insert(RecipientInfo::Ktri(KeyTransRecipientInfo {
            version: CmsVersion::V0,
            rid: RecipientIdentifier::IssuerAndSerialNumber(issuer_and_serial(r)),
            key_enc_alg: null_params(oid::RSA_ENCRYPTION),
            enc_key: OctetString::new(ek)?,
        }))?;
    }
    let ed = EnvelopedData {
        version: CmsVersion::V0,
        originator_info: None,
        recip_infos: RecipientInfos(infos),
        encrypted_content: EncryptedContentInfo {
            content_type: oid::DATA,
            content_enc_alg: AlgorithmIdentifierOwned {
                oid: oid::AES256_CBC,
                parameters: Some(Any::encode_from(&OctetString::new(iv.to_vec())?)?),
            },
            encrypted_content: Some(OctetString::new(ciphertext)?),
        },
        unprotected_attrs: None,
    };
    wrap(oid::ENVELOPED_DATA, &ed)
}

/// Who an EnvelopedData is encrypted for: (issuer, serial hex) of each key-transport recipient.
pub fn recipients_of(input: &[u8]) -> Result<Vec<(String, String)>> {
    let ed = enveloped(input)?;
    Ok(ed
        .recip_infos
        .0
        .iter()
        .take(MAX_RECIPIENTS)
        .filter_map(|ri| match ri {
            RecipientInfo::Ktri(k) => match &k.rid {
                RecipientIdentifier::IssuerAndSerialNumber(ias) => {
                    Some((ias.issuer.to_string(), crate::hex(ias.serial_number.as_bytes())))
                }
                RecipientIdentifier::SubjectKeyIdentifier(ski) => {
                    Some((String::new(), crate::hex(ski.0.as_bytes())))
                }
            },
            _ => None,
        })
        .collect())
}

fn enveloped(input: &[u8]) -> Result<EnvelopedData> {
    let ci = content_info(input)?;
    if ci.content_type == oid::AUTH_ENVELOPED_DATA {
        return Err(SmimeError::Unsupported("authenticated encryption (AES-GCM, RFC 5083)".into()));
    }
    if ci.content_type != oid::ENVELOPED_DATA {
        return Err(SmimeError::Malformed("not an encrypted message".into()));
    }
    Ok(ci.content.decode_as()?)
}

fn recipient_matches(rid: &RecipientIdentifier, c: &Cert) -> bool {
    match rid {
        RecipientIdentifier::IssuerAndSerialNumber(ias) => {
            let tbs = &c.x509().tbs_certificate;
            tbs.issuer == ias.issuer && tbs.serial_number == ias.serial_number
        }
        RecipientIdentifier::SubjectKeyIdentifier(ski) => {
            c.subject_key_id().is_some_and(|k| k.as_slice() == ski.0.as_bytes())
        }
    }
}

/// Decrypts an EnvelopedData `ContentInfo` (DER or BER) with whichever of `ids` it is encrypted for.
/// [`SmimeError::NoKey`] when it names none of them; a wrong key or damaged data gives one uniform error.
pub fn decrypt(input: &[u8], ids: &[Identity]) -> Result<Zeroizing<Vec<u8>>> {
    let ed = enveloped(input)?;
    let eci = &ed.encrypted_content;
    let ciphertext = eci
        .encrypted_content
        .as_ref()
        .ok_or_else(|| SmimeError::Malformed("the encrypted content is missing".into()))?
        .as_bytes();
    let mut candidates = Vec::new();
    for ri in ed.recip_infos.0.iter().take(MAX_RECIPIENTS) {
        if let RecipientInfo::Ktri(k) = ri {
            for id in ids {
                if recipient_matches(&k.rid, &id.cert) {
                    candidates.push((k, id));
                }
            }
        }
    }
    if candidates.is_empty() {
        if ed.recip_infos.0.iter().any(|ri| !matches!(ri, RecipientInfo::Ktri(_))) && !ids.is_empty() {
            return Err(SmimeError::Unsupported(
                "the message uses a key-agreement or password recipient (only RSA key transport is supported)"
                    .into(),
            ));
        }
        return Err(SmimeError::NoKey);
    }
    let fail = || {
        SmimeError::Crypto("the message could not be decrypted (wrong key, or the message is damaged)".into())
    };
    let mut last = fail();
    let mut rng = crate::rng()?;
    for (k, id) in candidates {
        let cek = if k.key_enc_alg.oid == oid::RSA_ENCRYPTION {
            id.key.0.decrypt_blinded(&mut rng, Pkcs1v15Encrypt, k.enc_key.as_bytes())
        } else if k.key_enc_alg.oid == oid::RSAES_OAEP {
            let Some(alg) = oaep_params(k.key_enc_alg.parameters.as_ref()) else {
                last = SmimeError::Unsupported("these RSAES-OAEP parameters".into());
                continue;
            };
            id.key.0.decrypt_blinded(&mut rng, alg.oaep(), k.enc_key.as_bytes())
        } else {
            last = SmimeError::Unsupported(format!("key transport algorithm {}", k.key_enc_alg.oid));
            continue;
        };
        let Ok(cek) = cek.map(Zeroizing::new) else {
            last = fail();
            continue;
        };
        match decrypt_content(&eci.content_enc_alg, &cek, ciphertext) {
            Ok(plain) => return Ok(plain),
            Err(e @ SmimeError::Unsupported(_)) => return Err(e),
            Err(_) => last = fail(),
        }
    }
    Err(last)
}

/// RSAES-OAEP-params (RFC 4055 / RFC 8017): only the hash is read; the mask generation must use the same
/// hash and the label must be empty (the common forms).
fn oaep_params(params: Option<&Any>) -> Option<DigestAlg> {
    let Some(p) = params else { return Some(DigestAlg::Sha1) };
    if der::Tagged::tag(p) == der::Tag::Null {
        return Some(DigestAlg::Sha1);
    }
    let mut alg = DigestAlg::Sha1;
    let mut mgf: Option<DigestAlg> = None;
    for f in ber::children(p.value(), 4).ok()? {
        match f.tag {
            0xa0 => {
                let (inner, _) = ber::read_tlv(f.content).ok()?;
                let (o, _) = ber::read_tlv(inner.content).ok()?;
                alg = DigestAlg::from_oid(&ObjectIdentifier::from_bytes(o.content).ok()?)?;
            }
            0xa1 => {
                let (inner, _) = ber::read_tlv(f.content).ok()?;
                let parts = ber::children(inner.content, 2).ok()?;
                let hash = parts.get(1)?;
                let (o, _) = ber::read_tlv(hash.content).ok()?;
                mgf = Some(DigestAlg::from_oid(&ObjectIdentifier::from_bytes(o.content).ok()?)?);
            }
            _ => return None,
        }
    }
    match mgf {
        Some(m) if m != alg => None,
        _ => Some(alg),
    }
}

fn decrypt_content(
    alg: &AlgorithmIdentifierOwned,
    key: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let iv = alg
        .parameters
        .as_ref()
        .and_then(|p| p.decode_as::<OctetString>().ok())
        .ok_or_else(|| SmimeError::Malformed("the content encryption parameters are missing".into()))?;
    let iv = iv.as_bytes();
    let bad = |_| SmimeError::Crypto("decryption failed".into());
    let plain = match alg.oid {
        o if o == oid::AES256_CBC => cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv)
            .map_err(|e| SmimeError::Crypto(e.to_string()))?
            .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
            .map_err(bad)?,
        o if o == oid::AES192_CBC => cbc::Decryptor::<aes::Aes192>::new_from_slices(key, iv)
            .map_err(|e| SmimeError::Crypto(e.to_string()))?
            .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
            .map_err(bad)?,
        o if o == oid::AES128_CBC => cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv)
            .map_err(|e| SmimeError::Crypto(e.to_string()))?
            .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
            .map_err(bad)?,
        o if o == oid::DES_EDE3_CBC => cbc::Decryptor::<des::TdesEde3>::new_from_slices(key, iv)
            .map_err(|e| SmimeError::Crypto(e.to_string()))?
            .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
            .map_err(bad)?,
        other => return Err(SmimeError::Unsupported(format!("content encryption algorithm {other}"))),
    };
    Ok(Zeroizing::new(plain))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    #[test]
    fn sign_and_verify_detached_and_inside() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let body = b"Content-Type: text/plain\r\n\r\nHello Bob\r\n";
        let det = sign(body, &ann, testkit::NOW, true).unwrap();
        assert_eq!(kind(&det).unwrap(), CmsKind::Signed);
        let v = verify(&det, Some(body)).unwrap();
        assert_eq!(v.signers.len(), 1);
        let s = &v.signers[0];
        assert_eq!(s.result, Ok(()));
        assert_eq!(s.cert.as_ref(), Some(&ann.cert));
        assert_eq!(s.signing_time, Some(testkit::NOW));
        assert_eq!(s.digest, Some(DigestAlg::Sha256));
        assert!(v.certs.contains(&testkit::ca()));
        // Detached signature without its content.
        assert!(verify(&det, None).is_err());
        let inside = sign(body, &ann, testkit::NOW, false).unwrap();
        let v = verify(&inside, None).unwrap();
        assert_eq!(v.content, body);
        assert_eq!(v.signers[0].result, Ok(()));
    }

    #[test]
    fn tampering_is_detected() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let body = b"Content-Type: text/plain\r\n\r\nPay 10 dollars\r\n";
        let det = sign(body, &ann, testkit::NOW, true).unwrap();
        let v = verify(&det, Some(b"Content-Type: text/plain\r\n\r\nPay 99 dollars\r\n")).unwrap();
        assert!(v.signers[0].result.as_ref().unwrap_err().contains("changed"));
        // A flipped bit in the signature value itself.
        let mut bad = det.clone();
        let sig = v.signers[0].cert.as_ref().map(|_| ()).is_some();
        assert!(sig);
        let n = bad.len();
        bad[n - 10] ^= 1;
        let v = verify(&bad, Some(body)).unwrap();
        assert!(v.signers[0].result.is_err());
    }

    /// A SignedData `ContentInfo` like `genuine` with `infos` (encoded SignerInfos, in this order) as its
    /// signer infos, spliced in as sent (not re-sorted).
    fn with_signer_infos(genuine: &[u8], infos: &[Vec<u8>]) -> Vec<u8> {
        let ci = ContentInfo::from_der(genuine).unwrap();
        let mut fields: Vec<Vec<u8>> =
            ber::children(ci.content.value(), 16).unwrap().iter().map(|f| f.raw.to_vec()).collect();
        fields.pop();
        fields.push(ber::tlv(ber::TAG_SET, &infos.concat()));
        let sd = ber::tlv(TAG_SEQUENCE, &fields.concat());
        let body = [oid::SIGNED_DATA.to_der().unwrap(), ber::tlv(0xa0, &sd)].concat();
        ber::tlv(TAG_SEQUENCE, &body)
    }

    #[test]
    fn a_signature_cannot_be_moved_to_other_attributes() {
        // Regression: a second SignerInfo with Ann's identifier and Ann's signature, but attributes naming the
        // digest of other content, used to pass: its digest was checked, then Ann's signature was verified
        // over the genuine SignerInfo's attributes (found by signature value).
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let body = b"Content-Type: text/plain\r\n\r\nPay 10 dollars\r\n";
        let forged_body = b"Content-Type: text/plain\r\n\r\nPay 9999 dollars to Mallory\r\n";
        let genuine = sign(body, &ann, testkit::NOW, true).unwrap();
        let sd: SignedData = ContentInfo::from_der(&genuine).unwrap().content.decode_as().unwrap();
        let a = sd.signer_infos.0.iter().next().unwrap().clone();
        let mut b = a.clone();
        let digest = DigestAlg::Sha256.digest(forged_body);
        b.signed_attrs = Some(
            SetOfVec::try_from(vec![
                attribute(oid::CONTENT_TYPE, Any::encode_from(&oid::DATA).unwrap()).unwrap(),
                attribute(oid::MESSAGE_DIGEST, Any::encode_from(&OctetString::new(digest).unwrap()).unwrap())
                    .unwrap(),
            ])
            .unwrap(),
        );
        let (a, b) = (a.to_der().unwrap(), b.to_der().unwrap());
        // The forged SignerInfo is shorter, so the decoder sorts it first whatever order it was sent in.
        assert!(b.len() < a.len());
        for infos in [vec![a.clone(), b.clone()], vec![b.clone(), a.clone()]] {
            let forged = with_signer_infos(&genuine, &infos);
            let v = verify(&forged, Some(forged_body)).unwrap();
            assert_eq!(v.signers.len(), 2);
            assert!(v.signers.iter().all(|s| s.result.is_err()), "{:?}", v.signers);
            // The genuine content still verifies for the genuine signer only.
            let v = verify(&forged, Some(body)).unwrap();
            assert_eq!(v.signers.iter().filter(|s| s.result.is_ok()).count(), 1);
        }
        // Two message digests in one signer's attributes are refused.
        let mut twice = sd.signer_infos.0.iter().next().unwrap().clone();
        let mut attrs: Vec<Attribute> = twice.signed_attrs.clone().unwrap().into_vec();
        attrs.push(
            attribute(
                oid::MESSAGE_DIGEST,
                Any::encode_from(&OctetString::new(DigestAlg::Sha256.digest(forged_body)).unwrap()).unwrap(),
            )
            .unwrap(),
        );
        twice.signed_attrs = Some(SetOfVec::try_from(attrs).unwrap());
        let v = verify(&with_signer_infos(&genuine, &[twice.to_der().unwrap()]), Some(body)).unwrap();
        assert!(v.signers[0].result.is_err());
    }

    #[test]
    fn encrypt_and_decrypt() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let bob = testkit::identity("Bob Day", "bob@example.org");
        let eve = testkit::identity("Eve Ray", "eve@example.net");
        let body = b"Content-Type: text/plain\r\n\r\nsecret plans\r\n";
        let env = encrypt(body, &[bob.cert.clone(), ann.cert.clone(), bob.cert.clone()]).unwrap();
        assert_eq!(kind(&env).unwrap(), CmsKind::Enveloped);
        assert_eq!(recipients_of(&env).unwrap().len(), 2);
        assert_eq!(decrypt(&env, std::slice::from_ref(&bob)).unwrap().as_slice(), body);
        assert_eq!(decrypt(&env, &[eve.clone(), ann.clone()]).unwrap().as_slice(), body);
        assert_eq!(decrypt(&env, std::slice::from_ref(&eve)).unwrap_err(), SmimeError::NoKey);
        assert_eq!(decrypt(&env, &[]).unwrap_err(), SmimeError::NoKey);
        // Eve's key under Bob's certificate: the key transport fails cleanly.
        let impostor = Identity { key: eve.key.clone(), cert: bob.cert.clone(), chain: vec![] };
        assert!(matches!(decrypt(&env, &[impostor]).unwrap_err(), SmimeError::Crypto(_)));
        assert!(encrypt(body, &[]).is_err());
    }

    #[test]
    fn bundles_and_kinds() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let p7 = certificate_bundle(&[ann.cert.clone(), testkit::ca()]).unwrap();
        assert_eq!(certificates_of(&p7).unwrap().len(), 2);
        assert!(kind(b"junk").is_err());
        assert!(certificates_of(&encrypt(b"x", std::slice::from_ref(&ann.cert)).unwrap()).is_err());
    }

    #[test]
    fn junk_der_never_panics() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let body = b"hello";
        let samples = [
            sign(body, &ann, testkit::NOW, true).unwrap(),
            sign(body, &ann, testkit::NOW, false).unwrap(),
            encrypt(body, std::slice::from_ref(&ann.cert)).unwrap(),
        ];
        let ids = [ann.clone()];
        for s in &samples {
            for n in (0..s.len()).step_by(3) {
                let cut = s.get(..n).unwrap();
                let _ = verify(cut, Some(body));
                let _ = decrypt(cut, &ids);
                let _ = kind(cut);
            }
            let mut m = s.clone();
            for i in (0..m.len()).step_by(5) {
                let keep = m[i];
                m[i] = keep.wrapping_add(0x41);
                let _ = verify(&m, Some(body));
                let _ = verify(&m, None);
                let _ = decrypt(&m, &ids);
                let _ = recipients_of(&m);
                let _ = certificates_of(&m);
                m[i] = keep;
            }
        }
        let mut x: u32 = 7;
        for _ in 0..2_000 {
            let mut junk = vec![0x30u8, 0x80];
            for _ in 0..64 {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                junk.push(x as u8);
            }
            let _ = verify(&junk, Some(body));
            let _ = decrypt(&junk, &ids);
        }
    }
}
