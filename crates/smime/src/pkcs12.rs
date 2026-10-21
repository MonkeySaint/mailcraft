//! PKCS #12 (RFC 7292): import and export of a digital ID (`.pfx` / `.p12`).
//!
//! Import reads password-integrity files: the MAC (HMAC with SHA-1 or SHA-2, key from the PKCS #12 KDF) is
//! checked first, then the safes are opened: plain, or password-encrypted with PBES2 (PBKDF2 + AES or
//! triple DES; what current OpenSSL and Windows write) or the legacy PKCS #12 schemes (SHA-1 with triple DES
//! or RC2; what older programs write). Key bags and shrouded key bags give the RSA key, certificate bags the
//! certificates; the certificate matching the key becomes the ID, the rest its chain (nearest issuer first).
//!
//! Export writes the key as a PBES2-shrouded key bag (PBKDF2-HMAC-SHA256, AES-256-CBC), the certificates in a
//! plain safe, and an HMAC-SHA256 MAC: readable by current Windows, macOS, OpenSSL and NSS.

#[cfg(test)]
use cbc::cipher::BlockEncryptMut;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, InnerIvInit, KeyIvInit};
use cms::content_info::ContentInfo;
use der::asn1::{Any, BmpString, OctetString, SetOfVec};
use der::{Decode, Encode};
use hmac::Mac;
use pkcs12::cert_type::CertBag;
use pkcs12::kdf::{Pkcs12KeyType, derive_key};
use pkcs12::mac_data::MacData;
use pkcs12::pbe_params::Pkcs12PbeParams;
use pkcs12::pfx::{Pfx, Version};
use pkcs12::safe_bag::SafeBag;
use spki::AlgorithmIdentifierOwned;
use x509_cert::attr::Attribute;
use zeroize::Zeroizing;

use crate::cms::DigestAlg;
use crate::key::{MAX_KDF_ROUNDS, check_pbes};
use crate::{Cert, Identity, MAX_CERTS, PrivateKey, Result, SmimeError, ber, oid};

/// Largest `.pfx` file accepted (bytes).
pub const MAX_PFX_BYTES: usize = 4 * 1024 * 1024;
/// Most safes and bags read.
const MAX_SAFES: usize = 32;
const MAX_BAGS: usize = 256;
/// PBKDF2 and MAC rounds written on export.
const EXPORT_ROUNDS: u32 = 2048;

/// The PKCS #12 MAC with PBMAC1 (RFC 9579): recognised, not supported.
const PBMAC1: der::asn1::ObjectIdentifier = der::asn1::ObjectIdentifier::new_unwrap("1.2.840.113549.1.5.14");
const PBES2: der::asn1::ObjectIdentifier = der::asn1::ObjectIdentifier::new_unwrap("1.2.840.113549.1.5.13");

/// The password as the PKCS #12 KDF takes it: a NUL-terminated BMPString, or (OpenSSL's "no password")
/// nothing at all.
#[derive(Clone)]
struct Pass {
    utf8: Zeroizing<Vec<u8>>,
    bmp: Zeroizing<Vec<u8>>,
}

impl Pass {
    fn variants(password: &str) -> Result<Vec<Pass>> {
        let utf8 = Zeroizing::new(password.as_bytes().to_vec());
        let bmp = BmpString::from_utf8(password)
            .map_err(|_| SmimeError::Unsupported("characters in the password outside the BMP".into()))?;
        let mut b = Zeroizing::new(bmp.into_bytes().to_vec());
        b.extend([0u8, 0]);
        let mut out = vec![Pass { utf8: utf8.clone(), bmp: b }];
        if password.is_empty() {
            out.push(Pass { utf8, bmp: Zeroizing::new(Vec::new()) });
        }
        Ok(out)
    }
}

fn kdf(
    alg: DigestAlg,
    pass: &Pass,
    salt: &[u8],
    id: Pkcs12KeyType,
    rounds: i32,
    len: usize,
) -> Result<Vec<u8>> {
    if !(1..=MAX_KDF_ROUNDS as i32).contains(&rounds) {
        return Err(SmimeError::Unsupported(format!("{rounds} key derivation rounds")));
    }
    if salt.is_empty() || salt.len() > 1024 {
        return Err(SmimeError::Malformed("bad salt".into()));
    }
    Ok(match alg {
        DigestAlg::Sha1 => derive_key::<sha1::Sha1>(&pass.bmp, salt, id, rounds, len),
        DigestAlg::Sha224 => derive_key::<sha2::Sha224>(&pass.bmp, salt, id, rounds, len),
        DigestAlg::Sha256 => derive_key::<sha2::Sha256>(&pass.bmp, salt, id, rounds, len),
        DigestAlg::Sha384 => derive_key::<sha2::Sha384>(&pass.bmp, salt, id, rounds, len),
        DigestAlg::Sha512 => derive_key::<sha2::Sha512>(&pass.bmp, salt, id, rounds, len),
    })
}

fn hmac(alg: DigestAlg, key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    fn run<M: Mac + hmac::digest::KeyInit>(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
        let mut m = <M as hmac::digest::KeyInit>::new_from_slice(key)
            .map_err(|e| SmimeError::Crypto(e.to_string()))?;
        m.update(data);
        Ok(m.finalize().into_bytes().to_vec())
    }
    match alg {
        DigestAlg::Sha1 => run::<hmac::Hmac<sha1::Sha1>>(key, data),
        DigestAlg::Sha224 => run::<hmac::Hmac<sha2::Sha224>>(key, data),
        DigestAlg::Sha256 => run::<hmac::Hmac<sha2::Sha256>>(key, data),
        DigestAlg::Sha384 => run::<hmac::Hmac<sha2::Sha384>>(key, data),
        DigestAlg::Sha512 => run::<hmac::Hmac<sha2::Sha512>>(key, data),
    }
}

fn digest_len(alg: DigestAlg) -> usize {
    match alg {
        DigestAlg::Sha1 => 20,
        DigestAlg::Sha224 => 28,
        DigestAlg::Sha256 => 32,
        DigestAlg::Sha384 => 48,
        DigestAlg::Sha512 => 64,
    }
}

/// Checks the MAC with each password variant; returns the one that fits.
fn check_mac(mac: &MacData, data: &[u8], passes: &[Pass]) -> Result<Pass> {
    if mac.mac.algorithm.oid == PBMAC1 {
        return Err(SmimeError::Unsupported("the PBMAC1 integrity check (RFC 9579)".into()));
    }
    let alg = DigestAlg::from_oid(&mac.mac.algorithm.oid)
        .ok_or_else(|| SmimeError::Unsupported(format!("MAC digest {}", mac.mac.algorithm.oid)))?;
    for p in passes {
        let key = Zeroizing::new(kdf(
            alg,
            p,
            mac.mac_salt.as_bytes(),
            Pkcs12KeyType::Mac,
            mac.iterations,
            digest_len(alg),
        )?);
        if crate::ct_eq(&hmac(alg, &key, data)?, mac.mac.digest.as_bytes()) {
            return Ok(p.clone());
        }
    }
    Err(SmimeError::Password)
}

/// Decrypts data protected by a password-based scheme (PBES2 or a PKCS #12 PBE).
fn pbe_decrypt(alg: &AlgorithmIdentifierOwned, pass: &Pass, data: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if alg.oid == PBES2 {
        let der = alg.to_der()?;
        let scheme = pkcs5::EncryptionScheme::try_from(der.as_slice())
            .map_err(|e| SmimeError::Unsupported(format!("password encryption: {e}")))?;
        check_pbes(&scheme)?;
        return scheme
            .decrypt(pass.utf8.as_slice(), data)
            .map(Zeroizing::new)
            .map_err(|_| SmimeError::Password);
    }
    let params: Pkcs12PbeParams = alg
        .parameters
        .as_ref()
        .ok_or_else(|| SmimeError::Malformed("missing PBE parameters".into()))?
        .decode_as()?;
    let salt = params.salt.as_bytes();
    let rounds = params.iterations;
    let key_iv = |klen: usize| -> Result<(Zeroizing<Vec<u8>>, Vec<u8>)> {
        let k = kdf(DigestAlg::Sha1, pass, salt, Pkcs12KeyType::EncryptionKey, rounds, klen)?;
        let iv = kdf(DigestAlg::Sha1, pass, salt, Pkcs12KeyType::Iv, rounds, 8)?;
        Ok((Zeroizing::new(k), iv))
    };
    let bad = |_| SmimeError::Password;
    let out = match alg.oid {
        o if o == pkcs12::PKCS_12_PBE_WITH_SHAAND3_KEY_TRIPLE_DES_CBC => {
            let (k, iv) = key_iv(24)?;
            cbc::Decryptor::<des::TdesEde3>::new_from_slices(&k, &iv)
                .map_err(|e| SmimeError::Crypto(e.to_string()))?
                .decrypt_padded_vec_mut::<Pkcs7>(data)
                .map_err(bad)?
        }
        o if o == pkcs12::PKCS_12_PBE_WITH_SHAAND2_KEY_TRIPLE_DES_CBC => {
            let (k, iv) = key_iv(16)?;
            cbc::Decryptor::<des::TdesEde2>::new_from_slices(&k, &iv)
                .map_err(|e| SmimeError::Crypto(e.to_string()))?
                .decrypt_padded_vec_mut::<Pkcs7>(data)
                .map_err(bad)?
        }
        o if o == pkcs12::PKCS_12_PBEWITH_SHAAND40_BIT_RC2_CBC
            || o == pkcs12::PKCS_12_PBE_WITH_SHAAND128_BIT_RC2_CBC =>
        {
            let bits = if o == pkcs12::PKCS_12_PBEWITH_SHAAND40_BIT_RC2_CBC { 40 } else { 128 };
            let (k, iv) = key_iv(bits / 8)?;
            let cipher = rc2::Rc2::new_with_eff_key_len(&k, bits);
            cbc::Decryptor::<rc2::Rc2>::inner_iv_slice_init(cipher, &iv)
                .map_err(|e| SmimeError::Crypto(e.to_string()))?
                .decrypt_padded_vec_mut::<Pkcs7>(data)
                .map_err(bad)?
        }
        other => return Err(SmimeError::Unsupported(format!("password encryption {other}"))),
    };
    Ok(Zeroizing::new(out))
}

/// What the bags of a file hold.
#[derive(Default)]
struct Found {
    keys: Vec<PrivateKey>,
    certs: Vec<Cert>,
    bags: usize,
}

/// The value of a decoded bag. The decoder keeps the `[0] EXPLICIT` wrapper around it (the encoder adds one
/// itself), so it is taken off here.
fn bag_value(raw: &[u8]) -> Result<&[u8]> {
    let (t, _) = ber::read_tlv(raw)?;
    if t.tag == 0xa0 {
        let (inner, _) = ber::read_tlv(t.content)?;
        Ok(inner.raw)
    } else {
        Ok(t.raw)
    }
}

fn read_bags(bytes: &[u8], pass: &Pass, found: &mut Found, depth: usize) -> Result<()> {
    if depth > 3 {
        return Err(SmimeError::Malformed("safe contents nested too deep".into()));
    }
    let bags = Vec::<SafeBag>::from_der(bytes)?;
    for bag in bags {
        found.bags = found.bags.saturating_add(1);
        if found.bags > MAX_BAGS {
            return Err(SmimeError::Malformed("too many bags".into()));
        }
        let value = bag_value(&bag.bag_value)?;
        match bag.bag_id {
            o if o == pkcs12::PKCS_12_KEY_BAG_OID => found.keys.push(PrivateKey::from_pkcs8_der(value)?),
            o if o == pkcs12::PKCS_12_PKCS8_KEY_BAG_OID => {
                let epki = pkcs12::pbe_params::EncryptedPrivateKeyInfo::from_der(value)?;
                let plain = pbe_decrypt(&epki.encryption_algorithm, pass, epki.encrypted_data.as_bytes())?;
                found.keys.push(PrivateKey::from_pkcs8_der(&plain)?);
            }
            o if o == pkcs12::PKCS_12_CERT_BAG_OID => {
                let cb = CertBag::from_der(value)?;
                if cb.cert_id == oid::X509_CERT && found.certs.len() < MAX_CERTS {
                    found.certs.push(Cert::from_der(cb.cert_value.as_bytes())?);
                }
            }
            o if o == pkcs12::PKCS_12_SAFE_CONTENTS_BAG_OID => read_bags(value, pass, found, depth + 1)?,
            _ => {}
        }
    }
    Ok(())
}

/// Reads a `.pfx` / `.p12` file protected by `password` (see the module docs).
pub fn parse(bytes: &[u8], password: &str) -> Result<Identity> {
    if bytes.len() > MAX_PFX_BYTES {
        return Err(SmimeError::TooLarge(bytes.len()));
    }
    let der = ber::to_der(bytes)?;
    let pfx = Pfx::from_der(&der).map_err(|e| SmimeError::Malformed(format!("not a PKCS #12 file: {e}")))?;
    if pfx.auth_safe.content_type != oid::DATA {
        return Err(SmimeError::Unsupported("PKCS #12 files protected by a public key".into()));
    }
    let data = pfx.auth_safe.content.decode_as::<OctetString>()?;
    let data = data.as_bytes();
    let passes = Pass::variants(password)?;
    let pass = match &pfx.mac_data {
        Some(mac) => check_mac(mac, data, &passes)?,
        None => passes.first().cloned().ok_or(SmimeError::Password)?,
    };
    let safes = Vec::<ContentInfo>::from_der(data)?;
    let mut found = Found::default();
    for ci in safes.iter().take(MAX_SAFES) {
        if ci.content_type == oid::DATA {
            let inner = ci.content.decode_as::<OctetString>()?;
            read_bags(inner.as_bytes(), &pass, &mut found, 0)?;
        } else if ci.content_type == oid::ENCRYPTED_DATA {
            let ed: cms::encrypted_data::EncryptedData = ci.content.decode_as()?;
            let eci = &ed.enc_content_info;
            let Some(ct) = &eci.encrypted_content else { continue };
            let plain = pbe_decrypt(&eci.content_enc_alg, &pass, ct.as_bytes())?;
            read_bags(&plain, &pass, &mut found, 0)?;
        }
    }
    let Some(key) = found.keys.iter().find(|k| found.certs.iter().any(|c| k.matches(c))).cloned() else {
        return Err(if found.keys.is_empty() {
            SmimeError::Malformed("the file has no private key".into())
        } else {
            SmimeError::Malformed("the file has no certificate for its private key".into())
        });
    };
    let id = Identity::from_key_and_certs(key, found.certs)?;
    Ok(Identity { chain: order_chain(&id.cert, id.chain), ..id })
}

/// Orders `rest` from `leaf`'s issuer upwards; certificates outside the chain follow.
fn order_chain(leaf: &Cert, mut rest: Vec<Cert>) -> Vec<Cert> {
    let mut out = Vec::new();
    let mut cur = leaf.clone();
    while let Some(i) = rest.iter().position(|c| !cur.self_issued() && crate::trust::issued_by(&cur, c)) {
        let next = rest.remove(i);
        cur = next.clone();
        out.push(next);
    }
    out.extend(rest);
    out
}

fn bag(id: der::asn1::ObjectIdentifier, value: Vec<u8>, attrs: Vec<Attribute>) -> Result<SafeBag> {
    let bag_attributes = if attrs.is_empty() { None } else { Some(SetOfVec::try_from(attrs)?) };
    Ok(SafeBag { bag_id: id, bag_value: value, bag_attributes })
}

fn bag_attrs(local_id: &[u8], friendly: &str) -> Result<Vec<Attribute>> {
    let mut out = Vec::new();
    let mut v = SetOfVec::new();
    v.insert(Any::encode_from(&OctetString::new(local_id)?)?)?;
    out.push(Attribute { oid: oid::LOCAL_KEY_ID, values: v });
    if !friendly.trim().is_empty() {
        let mut v = SetOfVec::new();
        let name = BmpString::from_utf8(friendly.trim()).map_err(|e| SmimeError::Malformed(e.to_string()))?;
        v.insert(Any::encode_from(&name)?)?;
        out.push(Attribute { oid: oid::FRIENDLY_NAME, values: v });
    }
    Ok(out)
}

fn data_info(bytes: &[u8]) -> Result<ContentInfo> {
    Ok(ContentInfo { content_type: oid::DATA, content: Any::encode_from(&OctetString::new(bytes)?)? })
}

/// Writes `id` as a `.pfx` file protected by `password` (see the module docs). `friendly` names the ID in
/// other programs' certificate lists.
pub fn build(id: &Identity, password: &str, friendly: &str) -> Result<Vec<u8>> {
    use sha1::Digest;
    let local_id = sha1::Sha1::digest(id.cert.der()).to_vec();
    let mut cert_bags = Vec::new();
    for (i, c) in std::iter::once(&id.cert).chain(id.chain.iter()).enumerate() {
        let cb = CertBag { cert_id: oid::X509_CERT, cert_value: OctetString::new(c.der())? };
        let attrs = if i == 0 { bag_attrs(&local_id, friendly)? } else { Vec::new() };
        cert_bags.push(bag(pkcs12::PKCS_12_CERT_BAG_OID, cb.to_der()?, attrs)?);
    }
    let shrouded = id.key.to_encrypted_der(password, EXPORT_ROUNDS)?;
    let key_bags = vec![bag(pkcs12::PKCS_12_PKCS8_KEY_BAG_OID, shrouded, bag_attrs(&local_id, friendly)?)?];
    finish(&[cert_bags.to_der()?, key_bags.to_der()?], password)
}

/// Wraps safe contents (plain) into a PFX with an HMAC-SHA256 MAC.
fn finish(safes: &[Vec<u8>], password: &str) -> Result<Vec<u8>> {
    let infos = safes.iter().map(|s| data_info(s)).collect::<Result<Vec<_>>>()?;
    finish_infos(infos, password, DigestAlg::Sha256)
}

fn finish_infos(infos: Vec<ContentInfo>, password: &str, mac_alg: DigestAlg) -> Result<Vec<u8>> {
    let auth_safe = infos.to_der()?;
    let salt = crate::random_bytes::<16>()?;
    let pass = Pass::variants(password)?.into_iter().next().ok_or(SmimeError::Password)?;
    let rounds = i32::try_from(EXPORT_ROUNDS).unwrap_or(2048);
    let key = Zeroizing::new(kdf(mac_alg, &pass, &salt, Pkcs12KeyType::Mac, rounds, digest_len(mac_alg))?);
    let mac = hmac(mac_alg, &key, &auth_safe)?;
    let pfx = Pfx {
        version: Version::V3,
        auth_safe: data_info(&auth_safe)?,
        mac_data: Some(MacData {
            mac: pkcs12::digest_info::DigestInfo {
                algorithm: AlgorithmIdentifierOwned {
                    oid: mac_alg.oid(),
                    parameters: Some(der::asn1::Null.into()),
                },
                digest: OctetString::new(mac)?,
            },
            mac_salt: OctetString::new(salt.to_vec())?,
            iterations: rounds,
        }),
    };
    Ok(pfx.to_der()?)
}

/// A legacy-format file (key under SHA-1 + triple DES, certificates in an RC2-40 encrypted safe, SHA-1
/// MAC), as older programs write it: for tests of the import path.
#[cfg(test)]
fn build_legacy(id: &Identity, password: &str) -> Result<Vec<u8>> {
    let pass = Pass::variants(password)?.into_iter().next().ok_or(SmimeError::Password)?;
    let pbe = |o: der::asn1::ObjectIdentifier,
               klen: usize,
               data: &[u8]|
     -> Result<(AlgorithmIdentifierOwned, Vec<u8>)> {
        let salt = crate::random_bytes::<8>()?;
        let k = kdf(DigestAlg::Sha1, &pass, &salt, Pkcs12KeyType::EncryptionKey, 2048, klen)?;
        let iv = kdf(DigestAlg::Sha1, &pass, &salt, Pkcs12KeyType::Iv, 2048, 8)?;
        let ct = if klen == 24 {
            cbc::Encryptor::<des::TdesEde3>::new_from_slices(&k, &iv)
                .map_err(|e| SmimeError::Crypto(e.to_string()))?
                .encrypt_padded_vec_mut::<Pkcs7>(data)
        } else {
            cbc::Encryptor::<rc2::Rc2>::inner_iv_slice_init(rc2::Rc2::new_with_eff_key_len(&k, klen * 8), &iv)
                .map_err(|e| SmimeError::Crypto(e.to_string()))?
                .encrypt_padded_vec_mut::<Pkcs7>(data)
        };
        let params = Pkcs12PbeParams { salt: OctetString::new(salt.to_vec())?, iterations: 2048 };
        Ok((AlgorithmIdentifierOwned { oid: o, parameters: Some(Any::encode_from(&params)?) }, ct))
    };
    let key_der = id.key.to_pkcs8_der()?;
    let (alg, ct) = pbe(pkcs12::PKCS_12_PBE_WITH_SHAAND3_KEY_TRIPLE_DES_CBC, 24, &key_der)?;
    let epki = pkcs12::pbe_params::EncryptedPrivateKeyInfo {
        encryption_algorithm: alg,
        encrypted_data: OctetString::new(ct)?,
    };
    let key_bags = vec![bag(pkcs12::PKCS_12_PKCS8_KEY_BAG_OID, epki.to_der()?, Vec::new())?];
    let mut cert_bags = Vec::new();
    for c in std::iter::once(&id.cert).chain(id.chain.iter()) {
        let cb = CertBag { cert_id: oid::X509_CERT, cert_value: OctetString::new(c.der())? };
        cert_bags.push(bag(pkcs12::PKCS_12_CERT_BAG_OID, cb.to_der()?, Vec::new())?);
    }
    let (alg, ct) = pbe(pkcs12::PKCS_12_PBEWITH_SHAAND40_BIT_RC2_CBC, 5, &cert_bags.to_der()?)?;
    let ed = cms::encrypted_data::EncryptedData {
        version: cms::content_info::CmsVersion::V0,
        enc_content_info: cms::enveloped_data::EncryptedContentInfo {
            content_type: oid::DATA,
            content_enc_alg: alg,
            encrypted_content: Some(OctetString::new(ct)?),
        },
        unprotected_attrs: None,
    };
    let enc_info = ContentInfo { content_type: oid::ENCRYPTED_DATA, content: Any::from_der(&ed.to_der()?)? };
    finish_infos(vec![enc_info, data_info(&key_bags.to_der()?)?], password, DigestAlg::Sha1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    #[test]
    fn export_and_import_round_trip() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let pfx = build(&ann, "correct horse", "Ann's ID").unwrap();
        let back = parse(&pfx, "correct horse").unwrap();
        assert_eq!(back.cert, ann.cert);
        assert_eq!(back.chain, vec![testkit::ca()]);
        assert!(back.key.matches(&ann.cert));
        assert_eq!(parse(&pfx, "wrong").unwrap_err(), SmimeError::Password);
        // An empty password works both ways.
        let open = build(&ann, "", "").unwrap();
        assert!(parse(&open, "").is_ok());
    }

    #[test]
    fn legacy_files_import() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let pfx = build_legacy(&ann, "pa55").unwrap();
        let back = parse(&pfx, "pa55").unwrap();
        assert_eq!(back.cert, ann.cert);
        assert!(back.key.matches(&ann.cert));
        assert_eq!(parse(&pfx, "nope").unwrap_err(), SmimeError::Password);
    }

    #[test]
    fn chain_is_ordered_from_the_leaf() {
        let ca = testkit::ca();
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let other = testkit::identity_with("Zed Q", "zed@example.net", 7, testkit::VALID, None).cert;
        let got = order_chain(&ann.cert, vec![other.clone(), ca.clone()]);
        assert_eq!(got, vec![ca, other]);
    }

    #[test]
    fn junk_files_never_panic() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let pfx = build(&ann, "pw", "").unwrap();
        for n in (0..pfx.len()).step_by(11) {
            let _ = parse(pfx.get(..n).unwrap(), "pw");
        }
        let mut m = pfx.clone();
        for i in (0..m.len()).step_by(13) {
            m[i] ^= 0x20;
            assert!(parse(&m, "pw").is_err() || parse(&m, "pw").is_ok());
            m[i] ^= 0x20;
        }
        assert!(parse(b"", "").is_err());
        assert!(parse(&[0x30; 100], "").is_err());
    }
}
