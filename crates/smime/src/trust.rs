//! Certificate chains and the trust a signature gets (a simplified RFC 5280 path validation).
//!
//! From the signer's certificate, issuers are looked up by name among the certificates the message carries
//! and the trusted ones, each link's signature is checked, and the walk stops at a trusted certificate (an
//! anchor: a root the user or the system trusts, or a certificate the user trusted directly). Issuers must be
//! authorities allowed to sign certificates for e-mail, within their path length. Revocation
//! (CRL, OCSP) is not checked: there is no network access from here.

use rsa::pkcs1v15::Pkcs1v15Sign;
use sha2::Digest;

use crate::cms::DigestAlg;
use crate::{Cert, oid};

/// Longest chain followed.
pub const MAX_CHAIN: usize = 8;

/// The status of a signature, worst first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// The signature does not match the message, or cannot be checked.
    Invalid,
    /// The signature matches, but a certificate in the chain is outside its validity period.
    Expired,
    /// The signature matches, but the certificate does not lead to a trusted one.
    Untrusted,
    /// The signature matches and the certificate leads to a trusted one.
    Valid,
}

impl Status {
    /// Lower-case name ("valid", "untrusted", "expired", "invalid").
    pub fn name(self) -> &'static str {
        match self {
            Status::Valid => "valid",
            Status::Untrusted => "untrusted",
            Status::Expired => "expired",
            Status::Invalid => "invalid",
        }
    }
}

/// How far a certificate is trusted.
#[derive(Clone, Debug)]
pub struct TrustReport {
    pub status: Status,
    /// The chain from the certificate towards its root (the certificate first).
    pub chain: Vec<Cert>,
    /// Why the status is not [`Status::Valid`] (empty when it is).
    pub reason: String,
}

/// True when `child` names `parent` as its issuer and `parent`'s key made `child`'s signature.
pub fn issued_by(child: &Cert, parent: &Cert) -> bool {
    child.issuer_name() == parent.subject_name() && signature_ok(child, parent)
}

fn signature_ok(child: &Cert, parent: &Cert) -> bool {
    let Ok(key) = parent.rsa_public_key() else { return false };
    let Ok(tbs) = child.tbs_der() else { return false };
    let x = child.x509();
    let alg = match x.signature_algorithm.oid {
        o if o == oid::SHA256_WITH_RSA => DigestAlg::Sha256,
        o if o == oid::SHA384_WITH_RSA => DigestAlg::Sha384,
        o if o == oid::SHA512_WITH_RSA => DigestAlg::Sha512,
        o if o == oid::SHA224_WITH_RSA => DigestAlg::Sha224,
        o if o == oid::SHA1_WITH_RSA => DigestAlg::Sha1,
        _ => return false,
    };
    let Some(sig) = x.signature.as_bytes() else { return false };
    let hashed = alg.digest(tbs);
    let scheme = match alg {
        DigestAlg::Sha1 => Pkcs1v15Sign::new::<sha1::Sha1>(),
        DigestAlg::Sha224 => Pkcs1v15Sign::new::<sha2::Sha224>(),
        DigestAlg::Sha256 => Pkcs1v15Sign::new::<sha2::Sha256>(),
        DigestAlg::Sha384 => Pkcs1v15Sign::new::<sha2::Sha384>(),
        DigestAlg::Sha512 => Pkcs1v15Sign::new::<sha2::Sha512>(),
    };
    key.verify(scheme, &hashed, sig).is_ok()
}

/// Evaluates `cert` at time `now` (Unix seconds): `pool` holds certificates that may be intermediates (those
/// a message carries), `anchors` the trusted ones.
///
/// Every issuer in the chain, trusted or not, must be a certificate authority allowed to sign certificates
/// (basic constraints `cA`, key usage keyCertSign), allowed to issue e-mail certificates (extended key usage
/// absent or with emailProtection), and within its path length constraint. A certificate the user trusted
/// directly that is not an authority vouches only for itself.
pub fn evaluate(cert: &Cert, pool: &[Cert], anchors: &[Cert], now: i64) -> TrustReport {
    let mut chain = vec![cert.clone()];
    let is_anchor = |c: &Cert| anchors.iter().any(|a| a == c);
    let mut trusted = is_anchor(cert);
    // Why an issuer that signed the last certificate could not continue the chain.
    let mut refused: Option<String> = None;
    while !trusted && chain.len() < MAX_CHAIN {
        let Some(cur) = chain.last() else { break };
        if cur.self_issued() {
            // A root (or a self-signed certificate) that is not trusted: the end of the walk.
            break;
        }
        // Intermediate authorities the next issuer would have below it (RFC 5280 §4.2.1.9 counts the ones
        // that are not self-issued; the certificate itself does not count).
        let below = chain.iter().skip(1).filter(|c| !c.self_issued()).count();
        let fits = |p: &Cert| {
            p.can_issue() && p.email_protection() && p.path_len().is_none_or(|n| below <= usize::from(n))
        };
        // Trusted issuers first, so a chain ends at an anchor whenever one fits.
        let mut issuers =
            anchors.iter().chain(pool.iter()).filter(|p| !chain.contains(p) && issued_by(cur, p));
        let next = issuers.clone().find(|p| fits(p)).cloned();
        match next {
            Some(n) => {
                trusted = is_anchor(&n);
                chain.push(n);
            }
            None => {
                refused = issuers.next().map(|p| {
                    let name = p.display_name();
                    if !p.can_issue() {
                        format!("The certificate was issued by {name}, which is not a certificate authority.")
                    } else if !p.email_protection() {
                        format!("The issuer {name} is not allowed to issue certificates for e-mail.")
                    } else {
                        format!("The chain of authorities is longer than the issuer {name} allows.")
                    }
                });
                break;
            }
        }
    }
    let expired = chain.iter().find(|c| !c.valid_at(now));
    if let Some(c) = expired {
        let when = if now < c.not_before() { "is not valid yet" } else { "has expired" };
        let whose = if c == cert {
            "The certificate".to_string()
        } else {
            format!("The issuer {}", c.display_name())
        };
        return TrustReport {
            status: Status::Expired,
            chain: chain.clone(),
            reason: format!("{whose} {when}."),
        };
    }
    if !trusted {
        let reason = match chain.last() {
            _ if refused.is_some() => refused.unwrap_or_default(),
            Some(last) if last.self_issued() && chain.len() == 1 => {
                "The certificate is self-signed and you have not chosen to trust it.".to_string()
            }
            Some(last) if last.self_issued() => {
                format!(
                    "The certificate was issued by {}, which you have not chosen to trust.",
                    last.display_name()
                )
            }
            Some(last) => format!(
                "The certificate of the issuer {} is not available, so the certificate cannot be traced to a \
                 trusted authority.",
                last.issuer_display_name()
            ),
            None => "The certificate cannot be traced to a trusted authority.".to_string(),
        };
        return TrustReport { status: Status::Untrusted, chain, reason };
    }
    TrustReport { status: Status::Valid, chain, reason: String::new() }
}

/// SHA-256 fingerprint helper for callers comparing raw DER.
pub fn fingerprint(der: &[u8]) -> String {
    crate::hex(&sha2::Sha256::digest(der))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    #[test]
    fn chains_to_a_trusted_root() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let ca = testkit::ca();
        assert!(issued_by(&ann.cert, &ca));
        assert!(!issued_by(&ca, &ann.cert));
        let r = evaluate(&ann.cert, &[], std::slice::from_ref(&ca), testkit::NOW);
        assert_eq!(r.status, Status::Valid, "{}", r.reason);
        assert_eq!(r.chain, vec![ann.cert.clone(), ca.clone()]);
        // Without the root trusted, it is untrusted (the root is still found in the pool).
        let r = evaluate(&ann.cert, std::slice::from_ref(&ca), &[], testkit::NOW);
        assert_eq!(r.status, Status::Untrusted);
        assert!(r.reason.contains(testkit::CA_NAME), "{}", r.reason);
        // Without the issuer at all.
        let r = evaluate(&ann.cert, &[], &[], testkit::NOW);
        assert_eq!(r.status, Status::Untrusted);
        assert!(r.reason.contains("not available"));
        // Trusting the certificate itself.
        let r = evaluate(&ann.cert, &[], std::slice::from_ref(&ann.cert), testkit::NOW);
        assert_eq!(r.status, Status::Valid);
    }

    #[test]
    fn a_trusted_certificate_that_is_not_an_authority_vouches_only_for_itself() {
        // Regression: trusting a self-signed signer let its key issue certificates for any address.
        let ann = testkit::identity_with("Ann Self", "ann@example.com", 7, testkit::VALID, None);
        let boss = testkit::identity_with(
            "Boss",
            "boss@example.com",
            8,
            testkit::VALID,
            Some((&ann.cert, &ann.key)),
        );
        assert!(issued_by(&boss.cert, &ann.cert));
        let trusted = std::slice::from_ref(&ann.cert);
        assert_eq!(evaluate(&ann.cert, &[], trusted, testkit::NOW).status, Status::Valid);
        for pool in [&[][..], trusted] {
            let r = evaluate(&boss.cert, pool, trusted, testkit::NOW);
            assert_eq!(r.status, Status::Untrusted);
            assert!(r.reason.contains("not a certificate authority"), "{}", r.reason);
            assert_eq!(r.chain, vec![boss.cert.clone()]);
        }
    }

    #[test]
    fn issuers_need_key_usage_email_and_path_length() {
        let root = testkit::ca();
        let root_key = testkit::key(0);
        let anchors = std::slice::from_ref(&root);
        let leaf_under = |issuer: &Cert, key: &crate::PrivateKey| {
            testkit::identity_with("Cy Moss", "cy@example.net", 9, testkit::VALID, Some((issuer, key))).cert
        };
        let authority =
            |name: &str, n: usize, issuer: (&Cert, &crate::PrivateKey), extras: testkit::Extras| {
                let key = testkit::key(n);
                let cert = testkit::issue_with(name, None, &key, Some(issuer), testkit::VALID, true, &extras);
                (cert, key)
            };
        // A trusted authority whose key usage leaves out keyCertSign.
        let (no_sign, k) = authority(
            "CN=No Sign CA",
            10,
            (&root, &root_key),
            testkit::Extras { no_cert_sign: true, ..Default::default() },
        );
        let r = evaluate(&leaf_under(&no_sign, &k), &[], std::slice::from_ref(&no_sign), testkit::NOW);
        assert_eq!(r.status, Status::Untrusted, "{}", r.reason);
        // An intermediate limited to web servers.
        let server_auth = der::asn1::ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.1");
        let (web, k) = authority(
            "CN=Web CA",
            10,
            (&root, &root_key),
            testkit::Extras { eku: Some(vec![server_auth]), ..Default::default() },
        );
        let r = evaluate(&leaf_under(&web, &k), std::slice::from_ref(&web), anchors, testkit::NOW);
        assert_eq!(r.status, Status::Untrusted);
        assert!(r.reason.contains("not allowed to issue certificates for e-mail"), "{}", r.reason);
        // An intermediate with path length 0 may issue end certificates, not other authorities.
        let (zero, zk) = authority(
            "CN=Zero CA",
            11,
            (&root, &root_key),
            testkit::Extras { path_len: Some(0), ..Default::default() },
        );
        let r = evaluate(&leaf_under(&zero, &zk), std::slice::from_ref(&zero), anchors, testkit::NOW);
        assert_eq!(r.status, Status::Valid, "{}", r.reason);
        let (sub, sk) = authority("CN=Sub CA", 10, (&zero, &zk), testkit::Extras::default());
        let r = evaluate(&leaf_under(&sub, &sk), &[sub.clone(), zero.clone()], anchors, testkit::NOW);
        assert_eq!(r.status, Status::Untrusted);
        assert!(r.reason.contains("longer than"), "{}", r.reason);
        // Without the limit the same chain is valid.
        let (open_ca, ok) = authority("CN=Open CA", 11, (&root, &root_key), testkit::Extras::default());
        let (sub, sk) = authority("CN=Sub CA 2", 10, (&open_ca, &ok), testkit::Extras::default());
        let r = evaluate(&leaf_under(&sub, &sk), &[sub.clone(), open_ca], anchors, testkit::NOW);
        assert_eq!(r.status, Status::Valid, "{}", r.reason);
        assert_eq!(r.chain.len(), 4);
    }

    #[test]
    fn expired_and_self_signed() {
        let old =
            testkit::identity_with("Old Key", "old@example.com", 5, (1_500_000_000, 1_600_000_000), None);
        let r = evaluate(&old.cert, &[], &[], testkit::NOW);
        assert_eq!(r.status, Status::Expired);
        assert!(r.reason.contains("expired"));
        let me = testkit::identity_with("Self Made", "self@example.com", 6, testkit::VALID, None);
        let r = evaluate(&me.cert, &[], &[], testkit::NOW);
        assert_eq!(r.status, Status::Untrusted);
        assert!(r.reason.contains("self-signed"));
        assert!(Status::Valid > Status::Untrusted && Status::Untrusted > Status::Expired);
        assert_eq!(Status::Expired.name(), "expired");
    }
}
