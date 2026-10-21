//! S/MIME messages (RFC 8551): protecting a whole RFC 5322 message, and opening one.
//!
//! [`protect`] takes a finished message, splits off its body entity (the `Content-*` header fields and the
//! body), and replaces it with
//! - a clear-signed `multipart/signed` (RFC 1847: the entity unchanged as the first part, a detached
//!   `application/pkcs7-signature` as the second), or an opaque `application/pkcs7-mime; smime-type=
//!   signed-data` that only S/MIME-aware programs can read;
//! - an `application/pkcs7-mime; smime-type=enveloped-data` for encryption;
//! - for both, the signed entity encrypted (sign, then encrypt).
//!
//! The other header fields (From, To, Subject, Date…) stay outside, as RFC 8551 messages without header
//! protection carry them.
//!
//! [`open`] undoes the layers, at most [`MAX_LAYERS`] deep: each signature is verified and graded
//! ([`crate::trust`]), each encryption is opened with the user's digital IDs. The result is the message as
//! it was before it was protected (the outer header fields with the innermost entity) and what each layer
//! said. An encryption that cannot be opened stops the unwrapping there.

use base64::Engine;
use mailcraft_mime::ContentType;
use mailcraft_mime::header::Headers;

use crate::cms::{self, CmsKind};
use crate::trust::{self, Status};
use crate::{Cert, Identity, MAX_CMS_BYTES, Result, SmimeError};

/// Most S/MIME layers unwrapped (sign + encrypt + sign is three).
pub const MAX_LAYERS: usize = 4;

/// What to do to a message.
#[derive(Clone, Copy, Debug)]
pub struct Protect<'a> {
    /// Sign with this digital ID.
    pub sign: Option<&'a Identity>,
    /// Encrypt for these certificates (include the sender's own to keep the sent copy readable).
    pub encrypt_to: &'a [Cert],
    /// Clear-signed (`multipart/signed`) rather than opaque signed data.
    pub clear_signed: bool,
    /// Signing time (Unix seconds).
    pub now: i64,
}

/// A message split into its outer header fields (raw, CRLF line ends, no `Content-*`) and its body entity
/// (the `Content-*` fields, a blank line and the body, CRLF line ends).
struct Split {
    outer: Vec<u8>,
    entity: Vec<u8>,
}

/// Converts bare LF line ends to CRLF (S/MIME signs canonical text).
pub fn crlf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 32);
    let mut prev = 0u8;
    for &b in data {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    out
}

/// Header block end: the offset of the body and of the header block's end.
fn header_end(raw: &[u8]) -> (usize, usize) {
    let mut i = 0;
    loop {
        let line_end = match raw.get(i..).and_then(|r| r.iter().position(|b| *b == b'\n')) {
            Some(p) => i + p,
            None => return (raw.len(), raw.len()),
        };
        let line = raw.get(i..line_end).unwrap_or_default();
        if line.is_empty() || line == b"\r" {
            return (line_end + 1, i);
        }
        i = line_end + 1;
    }
}

fn split(raw: &[u8]) -> Split {
    let (body_at, head_end) = header_end(raw);
    let head = raw.get(..head_end).unwrap_or_default();
    let body = raw.get(body_at..).unwrap_or_default();
    let mut outer = Vec::new();
    let mut content = Vec::new();
    let mut field: Vec<u8> = Vec::new();
    let mut flush = |field: &mut Vec<u8>| {
        if field.is_empty() {
            return;
        }
        let name_end = field.iter().position(|b| *b == b':').unwrap_or(field.len());
        let name =
            String::from_utf8_lossy(field.get(..name_end).unwrap_or_default()).trim().to_ascii_lowercase();
        let target = if name.starts_with("content-") { &mut content } else { &mut outer };
        target.extend_from_slice(&crlf(field));
        if !target.ends_with(b"\r\n") {
            target.extend_from_slice(b"\r\n");
        }
        field.clear();
    };
    for line in head.split_inclusive(|b| *b == b'\n') {
        let continuation = matches!(line.first(), Some(b' ' | b'\t'));
        if !continuation {
            flush(&mut field);
        }
        field.extend_from_slice(line);
    }
    flush(&mut field);
    if content.is_empty() {
        content.extend_from_slice(b"Content-Type: text/plain; charset=us-ascii\r\n");
    }
    let mut entity = content;
    entity.extend_from_slice(b"\r\n");
    entity.extend_from_slice(&crlf(body));
    Split { outer, entity }
}

fn base64_lines(der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::with_capacity(b64.len() + b64.len() / 38 + 2);
    for chunk in b64.as_bytes().chunks(76) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push_str("\r\n");
    }
    out
}

fn random_boundary() -> Result<String> {
    Ok(format!("----=_MailCraft_Signed_{}", crate::hex(&crate::random_bytes::<12>()?)))
}

fn trim_crlf(mut s: &[u8]) -> &[u8] {
    while let Some(r) = s.strip_suffix(b"\r\n") {
        s = r;
    }
    s
}

/// A clear-signed `multipart/signed` entity around `entity`.
fn clear_signed(entity: &[u8], id: &Identity, now: i64) -> Result<Vec<u8>> {
    let content = trim_crlf(entity);
    let sig = cms::sign(content, id, now, true)?;
    let b = random_boundary()?;
    let mut out = format!(
        "Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; micalg={};\r\n \
         boundary=\"{b}\"\r\n\r\nThis is a cryptographically signed message in MIME format.\r\n\r\n--{b}\r\n",
        cms::DigestAlg::Sha256.micalg()
    )
    .into_bytes();
    out.extend_from_slice(content);
    out.extend_from_slice(
        format!(
            "\r\n--{b}\r\nContent-Type: application/pkcs7-signature; name=\"smime.p7s\"\r\n\
             Content-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"smime.p7s\"\r\n\
             Content-Description: S/MIME Cryptographic Signature\r\n\r\n{}--{b}--\r\n",
            base64_lines(&sig)
        )
        .as_bytes(),
    );
    Ok(out)
}

/// An `application/pkcs7-mime` entity carrying `der`.
fn pkcs7_mime(smime_type: &str, der: &[u8]) -> Vec<u8> {
    format!(
        "Content-Type: application/pkcs7-mime; smime-type={smime_type}; name=\"smime.p7m\"\r\n\
         Content-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"smime.p7m\"\r\n\r\n{}",
        base64_lines(der)
    )
    .into_bytes()
}

/// Signs and/or encrypts `message` (see the module docs). Unchanged when there is nothing to do.
pub fn protect(message: &[u8], p: &Protect<'_>) -> Result<Vec<u8>> {
    if message.len() > MAX_CMS_BYTES {
        return Err(SmimeError::TooLarge(message.len()));
    }
    if p.sign.is_none() && p.encrypt_to.is_empty() {
        return Ok(message.to_vec());
    }
    let Split { outer, mut entity } = split(message);
    if let Some(id) = p.sign {
        entity = if p.clear_signed {
            clear_signed(&entity, id, p.now)?
        } else {
            pkcs7_mime("signed-data", &cms::sign(&entity, id, p.now, false)?)
        };
    }
    if !p.encrypt_to.is_empty() {
        entity = pkcs7_mime("enveloped-data", &cms::encrypt(&entity, p.encrypt_to)?);
    }
    let mut out = outer;
    if !out.windows(13).any(|w| w.eq_ignore_ascii_case(b"mime-version:")) {
        out.extend_from_slice(b"MIME-Version: 1.0\r\n");
    }
    out.extend_from_slice(&entity);
    Ok(out)
}

/// How a message (or entity) is protected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `multipart/signed` with a PKCS #7 signature: the boundary.
    ClearSigned(String),
    /// `application/pkcs7-mime` signed data.
    OpaqueSigned,
    /// `application/pkcs7-mime` enveloped (encrypted) data.
    Enveloped,
    /// `application/pkcs7-mime` without a usable `smime-type`: the CMS content decides.
    Pkcs7Unknown,
    /// A certificates-only message (`smime-type=certs-only`).
    CertsOnly,
}

fn kind_of(ct: &ContentType, filename: Option<&str>) -> Option<Kind> {
    let t = ct.mime_type.to_ascii_lowercase();
    let protocol = ct.param("protocol").map(str::to_ascii_lowercase).unwrap_or_default();
    if t == "multipart/signed"
        && (protocol == "application/pkcs7-signature" || protocol == "application/x-pkcs7-signature")
    {
        return ct.param("boundary").filter(|b| !b.is_empty()).map(|b| Kind::ClearSigned(b.to_owned()));
    }
    let p7m_name = filename.or(ct.param("name")).is_some_and(|n| {
        let n = n.trim().to_ascii_lowercase();
        n.ends_with(".p7m") || n.ends_with(".p7c") || n.ends_with(".p7z")
    });
    let pkcs7 = t == "application/pkcs7-mime" || t == "application/x-pkcs7-mime";
    if !pkcs7 && !(t == "application/octet-stream" && p7m_name) {
        return None;
    }
    Some(match ct.param("smime-type").map(str::to_ascii_lowercase).as_deref() {
        Some("enveloped-data") => Kind::Enveloped,
        Some("signed-data") => Kind::OpaqueSigned,
        Some("certs-only") => Kind::CertsOnly,
        _ => Kind::Pkcs7Unknown,
    })
}

/// How the entity with these header fields is protected, if at all.
fn entity_kind(headers: &Headers) -> Option<Kind> {
    let ct = ContentType::parse(headers.get("Content-Type")?);
    let filename = headers
        .get("Content-Disposition")
        .map(mailcraft_mime::Disposition::parse)
        .and_then(|(_, p)| p.into_iter().find(|(k, _)| k == "filename").map(|(_, v)| v));
    kind_of(&ct, filename.as_deref())
}

/// How `message` is protected at its top level (None for ordinary mail).
pub fn detect(message: &[u8]) -> Option<Kind> {
    let (headers, _) = Headers::parse(message);
    entity_kind(&headers)
}

/// A verified signature, graded.
#[derive(Clone, Debug)]
pub struct Signature {
    pub status: Status,
    /// Why the status is not valid (empty when it is).
    pub reason: String,
    /// The signer's certificate (None when the signature does not carry it).
    pub signer: Option<Cert>,
    /// The signer's chain towards its root (signer first).
    pub chain: Vec<Cert>,
    pub signing_time: Option<i64>,
    /// Digest algorithm name ("SHA-256").
    pub digest: Option<&'static str>,
    /// Every certificate the signature carries (for harvesting).
    pub certs: Vec<Cert>,
    /// Clear-signed (readable without S/MIME) rather than opaque.
    pub clear: bool,
}

/// One S/MIME layer, outermost first.
#[derive(Clone, Debug)]
pub enum Layer {
    Signed(Box<Signature>),
    /// An encryption layer: `Ok` when it was opened, otherwise why not.
    Encrypted(std::result::Result<(), String>),
}

/// What [`open`] found.
#[derive(Clone, Debug)]
pub struct Opened {
    /// The message as it was before it was protected (as far as the layers could be opened).
    pub message: Vec<u8>,
    pub layers: Vec<Layer>,
}

impl Opened {
    /// True when the message was encrypted.
    pub fn encrypted(&self) -> bool {
        self.layers.iter().any(|l| matches!(l, Layer::Encrypted(_)))
    }

    /// True when an encryption layer could not be opened.
    pub fn locked(&self) -> bool {
        self.layers.iter().any(|l| matches!(l, Layer::Encrypted(Err(_))))
    }

    /// The first (outermost) signature.
    pub fn signature(&self) -> Option<&Signature> {
        self.layers.iter().find_map(|l| match l {
            Layer::Signed(s) => Some(s.as_ref()),
            Layer::Encrypted(_) => None,
        })
    }

    /// Why an encryption layer could not be opened.
    pub fn decrypt_error(&self) -> Option<&str> {
        self.layers.iter().find_map(|l| match l {
            Layer::Encrypted(Err(e)) => Some(e.as_str()),
            _ => None,
        })
    }
}

fn grade(v: &cms::Verified, anchors: &[Cert], now: i64, clear: bool) -> Signature {
    let first = v.signers.first();
    let signer = first.and_then(|s| s.cert.clone());
    let mut sig = Signature {
        status: Status::Invalid,
        reason: String::new(),
        signer: signer.clone(),
        chain: signer.iter().cloned().collect(),
        signing_time: first.and_then(|s| s.signing_time),
        digest: first.and_then(|s| s.digest).map(cms::DigestAlg::name),
        certs: v.certs.clone(),
        clear,
    };
    match (first.map(|s| &s.result), signer) {
        (Some(Ok(())), Some(cert)) => {
            let t = trust::evaluate(&cert, &v.certs, anchors, now);
            sig.chain = t.chain;
            if cert.can_sign() {
                sig.status = t.status;
                sig.reason = t.reason;
            } else {
                // Its key usage or extended key usage does not allow signing mail (a web server's
                // certificate, say): whatever it chains to, it does not vouch for a message.
                sig.reason = "The digital signature is not valid: the certificate is not meant for signing \
                              e-mail."
                    .into();
            }
        }
        (Some(Err(e)), _) => sig.reason = format!("The digital signature is not valid: {e}."),
        _ => sig.reason = "The digital signature cannot be checked.".into(),
    }
    sig
}

fn invalid(reason: String, clear: bool) -> Signature {
    Signature {
        status: Status::Invalid,
        reason,
        signer: None,
        chain: Vec::new(),
        signing_time: None,
        digest: None,
        certs: Vec::new(),
        clear,
    }
}

/// The signed content and the signature of a `multipart/signed` entity.
fn clear_parts(entity: &[u8], boundary: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let (body_at, _) = header_end(entity);
    let body = entity.get(body_at..)?;
    let parts = mailcraft_mime::message::split_multipart(body, boundary)?;
    let content = parts.first()?.to_vec();
    let sig_part = parts.get(1)?;
    let (h, off) = Headers::parse(sig_part);
    let cte = h.get("Content-Transfer-Encoding").unwrap_or_default().to_owned();
    let der = mailcraft_mime::transfer::decode(sig_part.get(off..).unwrap_or_default(), &cte);
    Some((content, der))
}

/// Unwraps the S/MIME layers of `message` (see the module docs). None when the message is not S/MIME.
/// `ids` are the user's digital IDs, `anchors` the trusted certificates, `now` the time signatures are
/// graded at.
pub fn open(message: &[u8], ids: &[Identity], anchors: &[Cert], now: i64) -> Option<Opened> {
    if message.len() > MAX_CMS_BYTES {
        return None;
    }
    let Split { outer, mut entity } = split(message);
    let mut layers = Vec::new();
    for _ in 0..MAX_LAYERS {
        let (headers, off) = Headers::parse(&entity);
        let Some(mut kind) = entity_kind(&headers) else { break };
        let payload = || {
            let cte = headers.get("Content-Transfer-Encoding").unwrap_or_default().to_owned();
            mailcraft_mime::transfer::decode(entity.get(off..).unwrap_or_default(), &cte)
        };
        let mut der = Vec::new();
        if kind == Kind::Pkcs7Unknown || kind == Kind::CertsOnly {
            der = payload();
            kind = match cms::kind(&der) {
                Ok(CmsKind::Enveloped | CmsKind::AuthEnveloped) => Kind::Enveloped,
                Ok(CmsKind::Signed) => Kind::OpaqueSigned,
                _ => break,
            };
        }
        match kind {
            Kind::ClearSigned(boundary) => {
                let Some((content, sig)) = clear_parts(&entity, &boundary) else {
                    layers.push(Layer::Signed(Box::new(invalid(
                        "The signed message is damaged.".into(),
                        true,
                    ))));
                    break;
                };
                let mut v = cms::verify(&sig, Some(&content));
                // Content stored with bare line feeds was signed as CRLF text.
                if v.as_ref().is_ok_and(|v| v.signers.first().is_some_and(|s| s.result.is_err())) {
                    let canon = crlf(&content);
                    if canon != content
                        && let Ok(v2) = cms::verify(&sig, Some(&canon))
                        && v2.signers.first().is_some_and(|s| s.result.is_ok())
                    {
                        v = Ok(v2);
                    }
                }
                let s = match &v {
                    Ok(v) => grade(v, anchors, now, true),
                    Err(e) => invalid(format!("The digital signature cannot be read: {e}."), true),
                };
                layers.push(Layer::Signed(Box::new(s)));
                entity = crlf(&content);
            }
            Kind::OpaqueSigned => {
                if der.is_empty() {
                    der = payload();
                }
                match cms::verify(&der, None) {
                    Ok(v) => {
                        layers.push(Layer::Signed(Box::new(grade(&v, anchors, now, false))));
                        entity = crlf(&v.content);
                    }
                    Err(e) => {
                        layers.push(Layer::Signed(Box::new(invalid(
                            format!("The signed message cannot be read: {e}."),
                            false,
                        ))));
                        break;
                    }
                }
            }
            Kind::Enveloped => {
                if der.is_empty() {
                    der = payload();
                }
                match cms::decrypt(&der, ids) {
                    Ok(plain) => {
                        layers.push(Layer::Encrypted(Ok(())));
                        entity = crlf(&plain);
                    }
                    Err(e) => {
                        layers.push(Layer::Encrypted(Err(e.to_string())));
                        break;
                    }
                }
            }
            Kind::Pkcs7Unknown | Kind::CertsOnly => break,
        }
    }
    if layers.is_empty() {
        return None;
    }
    let mut out = outer;
    out.extend_from_slice(&entity);
    Some(Opened { message: out, layers })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    const MSG: &str = "From: Ann Lee <ann@example.com>\r\nTo: Bob Day <bob@example.org>\r\nSubject: Plans\r\n\
        Date: Thu, 01 Oct 2026 09:00:00 +0000\r\nMessage-ID: <plans.1@example.com>\r\nMIME-Version: 1.0\r\n\
        Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n\
        Meet at noon.=0D\r\nBring the map.\r\n";

    fn text(msg: &[u8]) -> String {
        mailcraft_mime::parse(msg).unwrap().text_body().unwrap_or_default()
    }

    #[test]
    fn clear_signed_round_trip() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let p = Protect { sign: Some(&ann), encrypt_to: &[], clear_signed: true, now: testkit::NOW };
        let signed = protect(MSG.as_bytes(), &p).unwrap();
        let s = String::from_utf8_lossy(&signed);
        assert!(s.contains("multipart/signed") && s.contains("smime.p7s") && s.contains("micalg=sha-256"));
        assert!(s.starts_with("From: Ann Lee"));
        assert!(matches!(detect(&signed), Some(Kind::ClearSigned(_))));
        // Readable without S/MIME: the text is the first part.
        assert!(text(&signed).contains("Meet at noon."));
        let o = open(&signed, &[], &[testkit::ca()], testkit::NOW).unwrap();
        let sig = o.signature().unwrap();
        assert_eq!(sig.status, Status::Valid, "{}", sig.reason);
        assert_eq!(sig.signer.as_ref(), Some(&ann.cert));
        assert_eq!(sig.signing_time, Some(testkit::NOW));
        assert!(sig.clear && !o.encrypted());
        assert!(text(&o.message).contains("Bring the map."));
        assert_eq!(mailcraft_mime::parse(&o.message).unwrap().subject(), "Plans");
        // Not trusting the CA: untrusted, still readable.
        let o = open(&signed, &[], &[], testkit::NOW).unwrap();
        assert_eq!(o.signature().unwrap().status, Status::Untrusted);
        // Stored with bare line feeds: still valid.
        let lf = String::from_utf8_lossy(&signed).replace("\r\n", "\n");
        let o = open(lf.as_bytes(), &[], &[testkit::ca()], testkit::NOW).unwrap();
        assert_eq!(o.signature().unwrap().status, Status::Valid, "{}", o.signature().unwrap().reason);
    }

    #[test]
    fn tampered_body_is_invalid() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let p = Protect { sign: Some(&ann), encrypt_to: &[], clear_signed: true, now: testkit::NOW };
        let signed = String::from_utf8(protect(MSG.as_bytes(), &p).unwrap()).unwrap();
        let tampered = signed.replace("Meet at noon.", "Meet at nine.");
        assert_ne!(tampered, signed);
        let o = open(tampered.as_bytes(), &[], &[testkit::ca()], testkit::NOW).unwrap();
        let sig = o.signature().unwrap();
        assert_eq!(sig.status, Status::Invalid);
        assert!(sig.reason.contains("changed"), "{}", sig.reason);
        // Header fields outside the signed part may change (S/MIME does not cover them).
        let resubject = signed.replace("Subject: Plans", "Subject: Other");
        let o = open(resubject.as_bytes(), &[], &[testkit::ca()], testkit::NOW).unwrap();
        assert_eq!(o.signature().unwrap().status, Status::Valid);
    }

    #[test]
    fn only_email_certificates_sign_mail() {
        // A web server's certificate (extended key usage serverAuth only) under a trusted authority: its
        // signature matches, but it does not vouch for mail.
        let server_auth = der::asn1::ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.1");
        let key = testkit::key(10);
        let extras = testkit::Extras { eku: Some(vec![server_auth]), ..Default::default() };
        let web = testkit::issue_with(
            "CN=www.example.com,O=Example",
            Some("ann@example.com"),
            &key,
            Some((&testkit::ca(), &testkit::key(0))),
            testkit::VALID,
            false,
            &extras,
        );
        assert!(!web.can_sign());
        let id = Identity::new(key, web, vec![testkit::ca()]).unwrap();
        let p = Protect { sign: Some(&id), encrypt_to: &[], clear_signed: true, now: testkit::NOW };
        let signed = protect(MSG.as_bytes(), &p).unwrap();
        let o = open(&signed, &[], &[testkit::ca()], testkit::NOW).unwrap();
        let sig = o.signature().unwrap();
        assert_eq!(sig.status, Status::Invalid);
        assert!(sig.reason.contains("not meant for signing"), "{}", sig.reason);
        // The same message signed with an e-mail certificate from that authority is valid.
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let p = Protect { sign: Some(&ann), ..p };
        let o = open(&protect(MSG.as_bytes(), &p).unwrap(), &[], &[testkit::ca()], testkit::NOW).unwrap();
        assert_eq!(o.signature().unwrap().status, Status::Valid);
    }

    #[test]
    fn opaque_signed_round_trip() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let p = Protect { sign: Some(&ann), encrypt_to: &[], clear_signed: false, now: testkit::NOW };
        let signed = protect(MSG.as_bytes(), &p).unwrap();
        assert_eq!(detect(&signed), Some(Kind::OpaqueSigned));
        let o = open(&signed, &[], &[testkit::ca()], testkit::NOW).unwrap();
        let sig = o.signature().unwrap();
        assert_eq!(sig.status, Status::Valid, "{}", sig.reason);
        assert!(!sig.clear);
        assert!(text(&o.message).contains("Meet at noon."));
    }

    #[test]
    fn encrypt_sign_and_both() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let bob = testkit::identity("Bob Day", "bob@example.org");
        let eve = testkit::identity("Eve Ray", "eve@example.net");
        let to = [bob.cert.clone(), ann.cert.clone()];
        let p = Protect { sign: Some(&ann), encrypt_to: &to, clear_signed: true, now: testkit::NOW };
        let sealed = protect(MSG.as_bytes(), &p).unwrap();
        let s = String::from_utf8_lossy(&sealed);
        assert!(s.contains("smime-type=enveloped-data"));
        assert!(!s.contains("Meet at noon"), "the text is not readable in transit");
        assert_eq!(detect(&sealed), Some(Kind::Enveloped));
        // Bob reads it and sees Ann's valid signature inside.
        let o = open(&sealed, std::slice::from_ref(&bob), &[testkit::ca()], testkit::NOW).unwrap();
        assert!(o.encrypted() && !o.locked());
        assert_eq!(o.signature().unwrap().status, Status::Valid);
        assert!(text(&o.message).contains("Meet at noon."));
        // Ann (encrypted to self) reads her sent copy.
        let o = open(&sealed, std::slice::from_ref(&ann), &[testkit::ca()], testkit::NOW).unwrap();
        assert!(text(&o.message).contains("Bring the map."));
        // Eve cannot: the layer stays locked, nothing panics.
        let o = open(&sealed, std::slice::from_ref(&eve), &[testkit::ca()], testkit::NOW).unwrap();
        assert!(o.locked());
        assert!(o.signature().is_none());
        assert!(o.decrypt_error().unwrap().contains("none of your digital IDs"));
        // Encrypt only.
        let p = Protect { sign: None, encrypt_to: &to, clear_signed: true, now: testkit::NOW };
        let enc = protect(MSG.as_bytes(), &p).unwrap();
        let o = open(&enc, std::slice::from_ref(&bob), &[], testkit::NOW).unwrap();
        assert!(o.encrypted() && o.signature().is_none());
        assert!(text(&o.message).contains("Meet at noon."));
        // Nothing to do: unchanged.
        let p = Protect { sign: None, encrypt_to: &[], clear_signed: true, now: 0 };
        assert_eq!(protect(MSG.as_bytes(), &p).unwrap(), MSG.as_bytes());
        assert!(open(MSG.as_bytes(), &[], &[], 0).is_none());
    }

    #[test]
    fn multipart_messages_keep_attachments() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let mut b = mailcraft_mime::MessageBuilder::new();
        b.from = Some(mailcraft_mime::Mailbox::new(Some("Ann Lee"), "ann@example.com"));
        b.to = mailcraft_mime::parse_address_list("bob@example.org");
        b.subject = "Report".into();
        b.text = Some("See attached.".into());
        b.html = Some("<p>See <b>attached</b>.</p>".into());
        b.attachments.push(mailcraft_mime::AttachmentSpec {
            filename: "report.pdf".into(),
            mime_type: "application/pdf".into(),
            data: vec![1, 2, 3, 0, 255],
            content_id: None,
        });
        let raw = b.build();
        let to = [ann.cert.clone()];
        let p = Protect { sign: Some(&ann), encrypt_to: &to, clear_signed: true, now: testkit::NOW };
        let sealed = protect(&raw, &p).unwrap();
        let o = open(&sealed, std::slice::from_ref(&ann), &[testkit::ca()], testkit::NOW).unwrap();
        assert_eq!(o.signature().unwrap().status, Status::Valid);
        let m = mailcraft_mime::parse(&o.message).unwrap();
        let att = m.attachments();
        assert_eq!(att.len(), 1);
        assert_eq!(att[0].filename, "report.pdf");
        assert_eq!(att[0].data, vec![1, 2, 3, 0, 255]);
        assert!(m.html_body().unwrap().contains("attached"));
    }

    #[test]
    fn hostile_messages_never_panic() {
        let ann = testkit::identity("Ann Lee", "ann@example.com");
        let ids = [ann.clone()];
        let to = [ann.cert.clone()];
        let p = Protect { sign: Some(&ann), encrypt_to: &to, clear_signed: true, now: testkit::NOW };
        let sealed = protect(MSG.as_bytes(), &p).unwrap();
        let signed = protect(MSG.as_bytes(), &Protect { encrypt_to: &[], ..p }).unwrap();
        for m in [&sealed, &signed] {
            for n in (0..m.len()).step_by(17) {
                let _ = open(m.get(..n).unwrap(), &ids, &[], testkit::NOW);
            }
            let mut x = m.clone();
            for i in (0..x.len()).step_by(29) {
                let keep = x[i];
                x[i] = b'A';
                let _ = open(&x, &ids, &[], testkit::NOW);
                x[i] = keep;
            }
        }
        let junk = [
            "Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; boundary=b\r\n\r\n--b\r\nx\r\n--b--",
            "Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; boundary=b\r\n\r\n",
            "Content-Type: application/pkcs7-mime; smime-type=enveloped-data\r\n\r\nMIIB",
            "Content-Type: application/pkcs7-mime\r\nContent-Transfer-Encoding: base64\r\n\r\n////",
            "Content-Type: application/octet-stream; name=smime.p7m\r\n\r\n\u{0}\u{1}",
            "Content-Type: application/pkcs7-mime; smime-type=signed-data\r\n\r\n",
        ];
        for j in junk {
            if let Some(o) = open(j.as_bytes(), &ids, &[], testkit::NOW) {
                assert!(!o.layers.is_empty());
            }
        }
        // Nested layers stop at the limit.
        let mut nested = MSG.as_bytes().to_vec();
        let p = Protect { sign: Some(&ann), encrypt_to: &[], clear_signed: false, now: testkit::NOW };
        for _ in 0..(MAX_LAYERS + 2) {
            nested = protect(&nested, &p).unwrap();
        }
        let o = open(&nested, &ids, &[], testkit::NOW).unwrap();
        assert_eq!(o.layers.len(), MAX_LAYERS);
    }
}
