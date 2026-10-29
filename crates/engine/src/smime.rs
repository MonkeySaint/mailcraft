//! S/MIME in the session (RFC 8551; the cryptography is `mailcraft-smime`): the user's digital IDs, other
//! people's certificates and the trusted certificates, and the glue to sending (Sign / Encrypt on Send) and
//! to reading (verify and decrypt when a message is shown).
//!
//! **Storage.** `certificates.json` next to the data file holds the certificates (public data) and each
//! digital ID's private key *sealed*: an encrypted PKCS #8 under a random secret that lives with the account
//! passwords in the session's [`crate::accounts::SecretStore`] (the OS keyring when one is in use, else the
//! labelled secrets file). Neither the keys nor decrypted mail ever enter the mailbox, its snapshot or its
//! journal.
//!
//! **Sending** ([`protect_for_send`]): Sign uses the digital ID for the sending address; Encrypt needs a
//! certificate for every recipient (from signed mail they sent, from their contact card, or imported) and
//! encrypts to the sender too, so the copy in Sent Items stays readable. A recipient without a certificate
//! stops the send with a choice: send without encryption, or cancel (`encryptionProblem`). Certificates from
//! contact cards must name the address and rank below the user's own, those from signed mail and imported
//! ones ([`cert_choice`]). The draft of a message to be encrypted, or of an answer to an encrypted one, is
//! saved encrypted to the user's own digital ID (`protect_draft`).
//!
//! **Reading** ([`view`]): a signed or encrypted message is opened on demand: the reading pane and read
//! window, replies, forwards and attachments use the opened message, and the info bar tells what the
//! signature and encryption say ([`Security`], `mail.signatureDetails`). Opened messages are cached in memory
//! only. Certificates of valid signers are remembered for later encrypted replies ([`on_new_mail`]).
//!
//! **Trust.** A signature is valid when its certificate leads to a trusted one: a root of the operating
//! system (desktop builds) or a certificate the user chose to trust (`options.trustCertificate`).
//!
//! Options › Trust Center › Email Security: `trust.smime.encrypt` and `trust.smime.sign` turn Encrypt / Sign
//! on for every new message, `trust.smime.clearSigned` sends signed mail as clear text (multipart/signed,
//! readable without S/MIME) rather than opaque signed data.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cmd::util::{dialog, opt_bool, opt_str};
use crate::compose::ComposeState;
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session};
use mailcraft_model::{Body, BodyFormat, DateTime, ItemId, ListTime, MailItem};
use mailcraft_smime::mime::{Layer, Opened, Protect};
use mailcraft_smime::{Cert, Identity, PrivateKey, SmimeError, Status};

/// Name of the certificates file in the profile (data) folder.
pub const CERTS_FILE: &str = "certificates.json";
/// Largest certificates file read back.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Largest certificate or digital ID file imported.
const MAX_IMPORT_BYTES: u64 = 4 * 1024 * 1024;
/// Most digital IDs, other people's certificates and trusted certificates kept.
const MAX_IDS: usize = 64;
const MAX_PEOPLE: usize = 10_000;
const MAX_TRUSTED: usize = 512;
/// Opened messages kept in memory.
const MAX_VIEWS: usize = 32;
/// Longest password accepted.
const MAX_PASSWORD_CHARS: usize = 1024;

const FILE_NOTE: &str = "MailCraft certificates. Private keys are sealed under secrets kept with the account \
passwords; this file alone cannot open them.";

/// The body shown for an encrypted message none of the user's digital IDs can open.
pub const LOCKED_TEXT: &str = "This message is encrypted, and none of your digital IDs can open it.\n\n\
To read it, import the digital ID (a .pfx or .p12 file) it was encrypted for: File › Options › Trust Center › \
Email Security › Digital IDs.";

/// Why Send stops when a digital ID is missing.
pub const NO_DIGITAL_ID: &str = "You don't have a digital ID for this address. Import one in File › Options › \
Trust Center › Email Security › Digital IDs, or turn off Sign and Encrypt for this message.";

// =========================================================================================================
// Stored data
// =========================================================================================================

/// One of the user's digital IDs as stored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredId {
    /// SHA-256 fingerprint of the certificate (lower-case hex).
    pub fingerprint: String,
    /// The certificate (base64 DER).
    pub cert: String,
    /// Issuing certificates (base64 DER), nearest first.
    pub chain: Vec<String>,
    /// The private key sealed under the secret `smime-key:<fingerprint>` (base64 encrypted PKCS #8).
    pub key: String,
    /// Used for signing when several IDs fit the sending address.
    pub default: bool,
    /// When it was imported (Unix seconds).
    pub added: i64,
}

/// Another person's certificate.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredCert {
    /// The address it is used for (lower-case).
    pub address: String,
    pub fingerprint: String,
    /// Base64 DER.
    pub cert: String,
    /// Where it came from: `mail` (a signed message), `file` (imported).
    pub source: String,
    pub added: i64,
}

/// The contents of `certificates.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CertBook {
    pub note: String,
    pub ids: Vec<StoredId>,
    pub people: Vec<StoredCert>,
    /// Certificates the user trusts (base64 DER): roots, or single certificates trusted directly.
    pub trusted: Vec<String>,
}

/// An opened S/MIME message.
#[derive(Clone, Debug)]
pub struct View {
    /// The item as it reads once opened: its body, attachments and preview come from the opened message.
    pub item: MailItem,
    /// The opened message (outer header fields with the innermost content).
    pub raw: Vec<u8>,
    pub security: Security,
    /// The signer's certificate when the signature is valid (trusted): only those are remembered, so a forged
    /// self-signed certificate never becomes the key mail to an address is encrypted for.
    signer: Option<Cert>,
}

/// What a message's S/MIME layers say, for the info bar and the details dialog.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Security {
    pub encrypted: bool,
    /// Encrypted, and none of the user's digital IDs could open it.
    pub locked: bool,
    pub decrypt_error: String,
    pub signed: bool,
    /// `valid`, `untrusted`, `expired`, `invalid` (empty when not signed).
    pub status: String,
    /// Why the status is not valid.
    pub reason: String,
    /// Clear-signed (readable without S/MIME) rather than opaque.
    pub clear_signed: bool,
    /// "Name <address>" of the signer, from the certificate.
    pub signer: String,
    pub signer_fingerprint: String,
    /// The certificate is issued to the sender's address.
    pub address_matches: bool,
    /// The sender's address (From, else Sender).
    pub sender: String,
    pub signing_time: String,
    pub digest: String,
    /// The signer's chain, signer first.
    pub chain: Vec<CertInfo>,
}

/// A certificate as the dialogs show it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CertInfo {
    pub name: String,
    pub subject: String,
    pub issuer: String,
    pub emails: Vec<String>,
    pub serial: String,
    pub valid_from: String,
    pub valid_to: String,
    pub expired: bool,
    pub fingerprint: String,
    pub sha1: String,
    pub key_bits: usize,
    pub can_sign: bool,
    pub can_encrypt: bool,
    pub trusted: bool,
}

/// The loaded certificates of a session and the caches derived from them.
#[derive(Default)]
pub struct SmimeSlot {
    loaded: bool,
    path: Option<PathBuf>,
    pub book: CertBook,
    save_error_shown: bool,
    cache: Mutex<Cache>,
}

impl std::fmt::Debug for SmimeSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SmimeSlot({} IDs, {} certificates)", self.book.ids.len(), self.book.people.len())
    }
}

#[derive(Default)]
struct Cache {
    ids: Option<Arc<Vec<Identity>>>,
    anchors: Option<Arc<Vec<Cert>>>,
    views: VecDeque<(ItemId, u64, Arc<View>)>,
    harvested: BTreeSet<ItemId>,
}

fn cache(s: &Session) -> std::sync::MutexGuard<'_, Cache> {
    s.smime.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Drops everything derived from the stored certificates (after a change).
fn invalidate(s: &Session) {
    let mut c = cache(s);
    c.ids = None;
    c.anchors = None;
    c.views.clear();
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn unb64(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text.trim().as_bytes()).ok()
}

fn cert_of(text: &str) -> Option<Cert> {
    Cert::from_der(&unb64(text)?).ok()
}

/// Key of the secret that seals a digital ID's private key.
pub fn secret_key(fingerprint: &str) -> String {
    format!("smime-key:{fingerprint}")
}

/// Reads `certificates.json` once (after the store is attached). A garbled file is reported and replaced on
/// the next save.
pub(crate) fn load(s: &mut Session) {
    let dir = s.store.as_ref().map(|st| st.dir.clone());
    let slot = &mut s.smime;
    if slot.loaded && (slot.path.is_some() || dir.is_none()) {
        return;
    }
    slot.loaded = true;
    let Some(dir) = dir else { return };
    let path = dir.join(CERTS_FILE);
    match mailcraft_store::journal::read_capped(&path, MAX_FILE_BYTES) {
        Ok(None) => {}
        Ok(Some(bytes)) => match serde_json::from_slice::<CertBook>(&bytes) {
            Ok(b) => {
                if slot.book == CertBook::default() {
                    slot.book = b;
                }
            }
            Err(e) => log::error!("{} is not valid ({e}); starting without certificates", path.display()),
        },
        Err(e) => log::error!("cannot read {}: {e}", path.display()),
    }
    slot.path = Some(path);
    invalidate(s);
}

fn save(s: &mut Session) {
    invalidate(s);
    load(s);
    let Some(path) = s.smime.path.clone() else { return };
    let mut data = s.smime.book.clone();
    data.note = FILE_NOTE.into();
    let res = serde_json::to_vec_pretty(&data)
        .map_err(|e| e.to_string())
        .and_then(|b| write_atomic(&path, &b).map_err(|e| e.to_string()));
    match res {
        Ok(()) => s.smime.save_error_shown = false,
        Err(e) => {
            log::error!("cannot write {}: {e}", path.display());
            if !s.smime.save_error_shown {
                s.smime.save_error_shown = true;
                s.notify(Notice::Error(format!("Certificates could not be saved ({e}).")));
            }
        }
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

fn book(s: &mut Session) -> &mut CertBook {
    load(s);
    &mut s.smime.book
}

// =========================================================================================================
// Identities, certificates, trust
// =========================================================================================================

/// The user's digital IDs with their keys unsealed (IDs whose secret is missing are left out).
pub fn identities(s: &Session) -> Arc<Vec<Identity>> {
    if let Some(ids) = cache(s).ids.clone() {
        return ids;
    }
    let mut out = Vec::new();
    for id in &s.smime.book.ids {
        let Some(cert) = cert_of(&id.cert) else { continue };
        let Some(secret) = s.secrets.get(&secret_key(&id.fingerprint)) else {
            log::warn!("the key of digital ID {} is missing from the secrets store", id.fingerprint);
            continue;
        };
        let Some(sealed) = unb64(&id.key) else { continue };
        match PrivateKey::unseal(&sealed, &secret) {
            Ok(key) => {
                let chain = id.chain.iter().filter_map(|c| cert_of(c)).collect();
                if let Ok(ident) = Identity::new(key, cert, chain) {
                    out.push(ident);
                }
            }
            Err(e) => log::warn!("digital ID {} cannot be opened: {e}", id.fingerprint),
        }
    }
    let ids = Arc::new(out);
    cache(s).ids = Some(ids.clone());
    ids
}

/// The operating system's root certificates (desktop builds; none in the browser).
fn system_roots() -> &'static [Cert] {
    static ROOTS: OnceLock<Vec<Cert>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        #[cfg(not(target_arch = "wasm32"))]
        {
            mailcraft_net::transport::system_root_ders()
                .iter()
                .filter_map(|d| Cert::from_der(d).ok())
                .collect()
        }
        #[cfg(target_arch = "wasm32")]
        {
            Vec::new()
        }
    })
}

/// Certificates a signature may lead to: those the user trusts, then the system's roots.
pub fn anchors(s: &Session) -> Arc<Vec<Cert>> {
    if let Some(a) = cache(s).anchors.clone() {
        return a;
    }
    let mut out: Vec<Cert> = s.smime.book.trusted.iter().filter_map(|c| cert_of(c)).collect();
    out.extend(system_roots().iter().cloned());
    let a = Arc::new(out);
    cache(s).anchors = Some(a.clone());
    a
}

fn user_trusts(s: &Session, c: &Cert) -> bool {
    s.smime.book.trusted.iter().any(|t| cert_of(t).is_some_and(|x| x == *c))
}

/// What a digital ID is wanted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Sign,
    Encrypt,
}

/// The digital ID to use for `address`: one whose certificate names the address and allows the purpose,
/// valid now; the default one first. Errors say why none fits.
pub fn identity_for(s: &Session, address: &str, purpose: Purpose) -> Result<Identity, String> {
    let now = s.now().unix;
    let ids = identities(s);
    let fits = |i: &&Identity| match purpose {
        Purpose::Sign => i.cert.can_sign(),
        Purpose::Encrypt => i.cert.can_encrypt(),
    };
    let default_fp: Vec<&str> =
        s.smime.book.ids.iter().filter(|i| i.default).map(|i| i.fingerprint.as_str()).collect();
    let mut matching: Vec<&Identity> =
        ids.iter().filter(|i| i.cert.has_email(address)).filter(fits).collect();
    if matching.is_empty() {
        return Err(format!("You don't have a digital ID for {address}. {}", NO_DIGITAL_ID_HINT));
    }
    matching.sort_by_key(|i| {
        (!default_fp.contains(&i.cert.fingerprint().as_str()), std::cmp::Reverse(i.cert.not_after()))
    });
    match matching.iter().find(|i| i.cert.valid_at(now)) {
        Some(i) => Ok((*i).clone()),
        None => Err(format!(
            "Your digital ID for {address} is not valid (it expired on {}). Import a new one in File › Options › \
             Trust Center › Email Security.",
            date_text(matching.first().map_or(0, |i| i.cert.not_after()), s.tz_offset_min)
        )),
    }
}

const NO_DIGITAL_ID_HINT: &str = "Import one in File › Options › Trust Center › Email Security › Digital IDs, or turn off Sign and Encrypt.";

/// Where the certificate used for an address came from, most trusted first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CertSource {
    /// One of the user's own digital IDs.
    Own,
    /// Remembered from a validly signed message from that address.
    Mail,
    /// Imported by the user (Digital IDs › Import Certificate).
    Imported,
    /// On a contact card, and it leads to a trusted certificate.
    TrustedContact,
    /// On a contact card, not traced to anything trusted (a business card someone sent, say).
    Contact,
}

/// A certificate to encrypt for `address` and where it came from: the user's own digital IDs, then
/// certificates remembered from validly signed mail, then imported ones, then those on contact cards (trusted
/// ones first); within each, the newest. Only certificates valid now and allowing encryption count, and
/// certificates from mail and contact cards must name the address (a card's KEY alone never redirects mail).
pub fn cert_choice(s: &Session, address: &str) -> Option<(Cert, CertSource)> {
    let now = s.now().unix;
    let a = address.trim().to_ascii_lowercase();
    if a.is_empty() {
        return None;
    }
    let mut found: Vec<(Cert, CertSource)> = Vec::new();
    for id in identities(s).iter() {
        if id.cert.has_email(&a) {
            found.push((id.cert.clone(), CertSource::Own));
        }
    }
    for p in s.smime.book.people.iter().filter(|p| p.address == a) {
        let Some(c) = cert_of(&p.cert) else { continue };
        // An imported certificate may be assigned to an address it does not name (the user said so when
        // importing it); anything else must name the address.
        if p.source == "file" {
            found.push((c, CertSource::Imported));
        } else if c.has_email(&a) {
            found.push((c, CertSource::Mail));
        }
    }
    let mut anchors_cache: Option<Arc<Vec<Cert>>> = None;
    for c in s.mailbox.contacts.values() {
        if !c.emails.iter().any(|e| e.trim().eq_ignore_ascii_case(&a)) {
            continue;
        }
        let carried: Vec<Cert> =
            c.certificates.iter().filter_map(|t| unb64(t)).filter_map(|d| Cert::from_der(&d).ok()).collect();
        for cert in carried.iter().filter(|x| x.has_email(&a)) {
            let anchors = anchors_cache.get_or_insert_with(|| anchors(s));
            let trusted =
                mailcraft_smime::trust::evaluate(cert, &carried, anchors, now).status == Status::Valid;
            let source = if trusted { CertSource::TrustedContact } else { CertSource::Contact };
            found.push((cert.clone(), source));
        }
    }
    found.retain(|(c, _)| c.valid_at(now) && c.can_encrypt());
    found.sort_by_key(|(c, src)| (*src, std::cmp::Reverse(c.not_after())));
    found.into_iter().next()
}

/// A certificate to encrypt for `address` (see [`cert_choice`]).
pub fn cert_for(s: &Session, address: &str) -> Option<Cert> {
    cert_choice(s, address).map(|(c, _)| c)
}

fn date_text(unix: i64, tz: i32) -> String {
    ListTime::header(DateTime { unix, offset_min: 0 }, tz)
}

/// A certificate for the dialogs.
pub fn cert_info(s: &Session, c: &Cert) -> CertInfo {
    let now = s.now().unix;
    CertInfo {
        name: c.display_name(),
        subject: c.subject(),
        issuer: c.issuer(),
        emails: c.emails(),
        serial: c.serial_hex(),
        valid_from: date_text(c.not_before(), s.tz_offset_min),
        valid_to: date_text(c.not_after(), s.tz_offset_min),
        expired: !c.valid_at(now),
        fingerprint: c.fingerprint(),
        sha1: c.sha1_thumbprint(),
        key_bits: c.key_bits(),
        can_sign: c.can_sign(),
        can_encrypt: c.can_encrypt(),
        trusted: user_trusts(s, c) || system_roots().contains(c),
    }
}

/// The Digital IDs dialog state: the user's IDs, other people's certificates and the trusted certificates.
pub fn summary(s: &Session) -> Value {
    let ids: Vec<Value> = s
        .smime
        .book
        .ids
        .iter()
        .filter_map(|i| {
            let c = cert_of(&i.cert)?;
            let usable = identities(s).iter().any(|x| x.cert == c);
            let mut v = serde_json::to_value(cert_info(s, &c)).ok()?;
            if let Some(o) = v.as_object_mut() {
                o.insert("default".into(), json!(i.default));
                o.insert("usable".into(), json!(usable));
                o.insert("chain".into(), json!(i.chain.len()));
            }
            Some(v)
        })
        .collect();
    let people: Vec<Value> = s
        .smime
        .book
        .people
        .iter()
        .filter_map(|p| {
            let c = cert_of(&p.cert)?;
            let mut v = serde_json::to_value(cert_info(s, &c)).ok()?;
            if let Some(o) = v.as_object_mut() {
                o.insert("address".into(), json!(p.address));
                o.insert("source".into(), json!(p.source));
            }
            Some(v)
        })
        .collect();
    let trusted: Vec<Value> = s
        .smime
        .book
        .trusted
        .iter()
        .filter_map(|t| serde_json::to_value(cert_info(s, &cert_of(t)?)).ok())
        .collect();
    json!({"ids": ids, "people": people, "trusted": trusted, "secrets": s.secrets.location()})
}

// =========================================================================================================
// Import, export, remove, trust
// =========================================================================================================

fn read_file(path: &str) -> Result<Vec<u8>, CmdError> {
    let p = Path::new(path);
    let meta = std::fs::metadata(p).map_err(|e| CmdError::Failed(format!("cannot read {path}: {e}")))?;
    if !meta.is_file() {
        return Err(CmdError::Params(format!("{path} is not a file")));
    }
    if meta.len() > MAX_IMPORT_BYTES {
        return Err(CmdError::Params(format!("{path} is too large for a certificate file")));
    }
    std::fs::read(p).map_err(|e| CmdError::Failed(format!("cannot read {path}: {e}")))
}

/// The bytes named by `path`, or given inline as base64 `data` or as `pem` text.
fn input_bytes(
    p: &Value,
    path_key: &str,
    data_key: &str,
    pem_key: &str,
) -> Result<Option<Vec<u8>>, CmdError> {
    if let Some(path) = opt_str(p, path_key).filter(|x| !x.trim().is_empty()) {
        return read_file(path.trim()).map(Some);
    }
    if let Some(d) = opt_str(p, data_key).filter(|x| !x.trim().is_empty()) {
        if d.len() as u64 > MAX_IMPORT_BYTES * 2 {
            return Err(CmdError::Params("the data is too large".into()));
        }
        return unb64(d)
            .map(Some)
            .ok_or_else(|| CmdError::Params(format!("`{data_key}` is not valid base64")));
    }
    if let Some(t) = opt_str(p, pem_key).filter(|x| !x.trim().is_empty()) {
        if t.len() as u64 > MAX_IMPORT_BYTES {
            return Err(CmdError::Params("the text is too large".into()));
        }
        return Ok(Some(t.as_bytes().to_vec()));
    }
    Ok(None)
}

fn smime_err(e: SmimeError) -> CmdError {
    match e {
        SmimeError::Password => CmdError::Failed("The password is incorrect.".into()),
        other => CmdError::Failed(format!("The file cannot be used: {other}.")),
    }
}

fn password(p: &Value) -> Result<String, CmdError> {
    let pw = opt_str(p, "password").unwrap_or_default();
    if pw.chars().count() > MAX_PASSWORD_CHARS {
        return Err(CmdError::Params("the password is too long".into()));
    }
    Ok(pw.to_owned())
}

/// Adds a digital ID (key and certificate) to the profile; replaces one with the same certificate.
pub fn add_identity(s: &mut Session, id: &Identity, trust_issuer: bool) -> Result<Value, CmdError> {
    let fp = id.cert.fingerprint();
    if book(s).ids.len() >= MAX_IDS && !book(s).ids.iter().any(|i| i.fingerprint == fp) {
        return Err(CmdError::Failed(format!("At most {MAX_IDS} digital IDs can be kept.")));
    }
    let secret = mailcraft_smime::random_secret().map_err(smime_err)?;
    let sealed = id.key.seal(&secret).map_err(smime_err)?;
    s.secrets
        .set(&secret_key(&fp), &secret)
        .map_err(|e| CmdError::Failed(format!("cannot store the key: {e}")))?;
    let now = s.now().unix;
    let first = book(s).ids.is_empty();
    let stored = StoredId {
        fingerprint: fp.clone(),
        cert: b64(id.cert.der()),
        chain: id.chain.iter().map(|c| b64(c.der())).collect(),
        key: b64(&sealed),
        default: first,
        added: now,
    };
    let b = book(s);
    match b.ids.iter_mut().find(|i| i.fingerprint == fp) {
        Some(existing) => {
            let default = existing.default;
            *existing = StoredId { default, ..stored };
        }
        None => b.ids.push(stored),
    }
    if trust_issuer {
        let root = id.chain.iter().rev().find(|c| c.self_issued()).or(id.chain.last()).cloned();
        if let Some(r) = root {
            add_trusted(s, &r);
        }
    }
    save(s);
    Ok(json!({"fingerprint": fp, "name": id.cert.display_name(), "emails": id.cert.emails()}))
}

fn add_trusted(s: &mut Session, c: &Cert) {
    let der = b64(c.der());
    let b = book(s);
    if !b.trusted.contains(&der) && b.trusted.len() < MAX_TRUSTED {
        b.trusted.push(der);
    }
}

/// Remembers `cert` for each of its addresses (or for `address` only); a newer certificate for an address
/// replaces an older one from the same source. Returns the addresses it was stored for.
pub fn add_person_cert(s: &mut Session, cert: &Cert, address: Option<&str>, source: &str) -> Vec<String> {
    let now = s.now().unix;
    let addrs: Vec<String> = match address.map(|a| a.trim().to_ascii_lowercase()).filter(|a| !a.is_empty()) {
        Some(a) => vec![a],
        None => cert.emails(),
    };
    let fp = cert.fingerprint();
    let b = book(s);
    let mut stored = Vec::new();
    for a in addrs {
        if b.people.iter().any(|p| p.address == a && p.fingerprint == fp) {
            continue;
        }
        if b.people.len() >= MAX_PEOPLE {
            break;
        }
        b.people.push(StoredCert {
            address: a.clone(),
            fingerprint: fp.clone(),
            cert: b64(cert.der()),
            source: source.into(),
            added: now,
        });
        stored.push(a);
    }
    stored
}

// =========================================================================================================
// Sending
// =========================================================================================================

/// Applies Sign / Encrypt of compose `cid` to the built message `raw` from `from` to `recipients` (every
/// To, Cc and Bcc address). `Err` is the result Send returns instead (a missing digital ID or certificates;
/// the user was told). `p.encryption == "skip"` sends without encryption (the answer to the missing
/// certificates question).
pub(crate) fn protect_for_send(
    s: &mut Session,
    cid: u64,
    c: &ComposeState,
    from: &str,
    recipients: &[String],
    raw: Vec<u8>,
    p: &Value,
) -> Result<Vec<u8>, Value> {
    let skip = opt_str(p, "encryption") == Some("skip");
    let encrypt = c.encrypt && !skip;
    if !c.sign && !encrypt {
        return Ok(raw);
    }
    load(s);
    let stop = |s: &mut Session, reason: &str, msg: String| -> Value {
        s.notify(Notice::Error(msg.clone()));
        json!({"sent": false, "compose": cid, "reason": reason, "error": msg})
    };
    let signer = if c.sign {
        match identity_for(s, from, Purpose::Sign) {
            Ok(i) => Some(i),
            Err(e) => return Err(stop(s, "noDigitalId", e)),
        }
    } else {
        None
    };
    let mut to: Vec<Cert> = Vec::new();
    // Recipients encrypted for with an untrusted certificate from their contact card (the user is told).
    let mut from_card: Vec<String> = Vec::new();
    if encrypt {
        let mine = match identity_for(s, from, Purpose::Encrypt) {
            Ok(i) => i.cert,
            Err(e) => return Err(stop(s, "noDigitalId", e)),
        };
        let mut missing = Vec::new();
        for r in recipients {
            match cert_choice(s, r) {
                Some((cert, source)) => {
                    if source == CertSource::Contact && !from_card.contains(r) {
                        from_card.push(r.clone());
                    }
                    to.push(cert);
                }
                None => {
                    if !missing.contains(r) {
                        missing.push(r.clone());
                    }
                }
            }
        }
        if !missing.is_empty() {
            let state = json!({"compose": cid, "missing": missing, "sign": c.sign});
            let _ = dialog(s, "encryptionProblem", state);
            return Err(json!({
                "sent": false, "compose": cid, "reason": "missingCertificates", "missing": missing,
                "choices": ["sendUnencrypted", "cancel"],
            }));
        }
        to.push(mine);
    }
    let opts = Protect {
        sign: signer.as_ref(),
        encrypt_to: &to,
        clear_signed: s.settings.bool("trust.smime.clearSigned"),
        now: s.now().unix,
    };
    match mailcraft_smime::mime::protect(&raw, &opts) {
        Ok(out) => {
            if !from_card.is_empty() {
                s.notify(Notice::Info(format!(
                    "Encrypted for {} with the certificate from the contact card, which is not traced to a \
                     trusted authority.",
                    from_card.join(", ")
                )));
            }
            Ok(out)
        }
        Err(e) => Err(stop(s, "smimeFailed", format!("The message could not be signed or encrypted: {e}."))),
    }
}

/// True when a draft of `c` must be stored encrypted: Encrypt is on, or it answers or forwards an encrypted
/// message (its quoted text came from the opened message).
pub(crate) fn draft_must_be_encrypted(s: &Session, c: &ComposeState) -> bool {
    c.encrypt || c.source.and_then(|id| s.mailbox.mail.get(&id)).is_some_and(|m| is_encrypted(s, m))
}

/// The draft `raw` encrypted to the user's own digital ID for `from` (only the user can open it; nothing is
/// signed). `Err` says why it cannot be: then the draft is not saved at all rather than saved readable.
pub(crate) fn protect_draft(s: &mut Session, from: &str, raw: Vec<u8>) -> Result<Vec<u8>, String> {
    load(s);
    let mine = identity_for(s, from, Purpose::Encrypt).map_err(|e| {
        format!(
            "The draft was not saved: this message is encrypted, so its draft is kept encrypted with your own \
             digital ID. {e}"
        )
    })?;
    let opts = Protect {
        sign: None,
        encrypt_to: std::slice::from_ref(&mine.cert),
        clear_signed: false,
        now: s.now().unix,
    };
    mailcraft_smime::mime::protect(&raw, &opts).map_err(|e| format!("The draft could not be encrypted: {e}."))
}

/// Settings and the original decide Sign / Encrypt for a new compose: Options › Trust Center › Email Security
/// turns them on for every new message; a reply to or forward of an encrypted message stays encrypted.
pub(crate) fn compose_defaults(s: &Session, c: &mut ComposeState) {
    c.sign |= s.settings.bool("trust.smime.sign");
    c.encrypt |= s.settings.bool("trust.smime.encrypt");
    if let Some(src) = c.source.and_then(|id| s.mailbox.mail.get(&id))
        && is_encrypted(s, src)
    {
        c.encrypt = true;
    }
}

// =========================================================================================================
// Reading
// =========================================================================================================

/// True when the item looks signed or encrypted (cheap: its attachment list names an S/MIME part).
pub fn is_smime(m: &MailItem) -> bool {
    m.attachments.iter().any(|a| {
        let t = a.mime_type.to_ascii_lowercase();
        let n = a.filename.trim().to_ascii_lowercase();
        t.contains("pkcs7-signature")
            || t.contains("pkcs7-mime")
            || n.ends_with(".p7m")
            || n.ends_with(".p7s")
    })
}

/// Most bytes of a source read back from the store to tell opaque-signed from encrypted (the top-level
/// header says which).
const KIND_SCAN: usize = 64 * 1024;

/// True when the item looks encrypted (an `application/pkcs7-mime` part that is not signed data). The
/// source is read back from the store when the item no longer keeps it (issue #25), so an opaque-signed
/// message never counts as encrypted.
pub fn is_encrypted(s: &Session, m: &MailItem) -> bool {
    m.attachments.iter().any(|a| a.mime_type.to_ascii_lowercase().contains("pkcs7-mime"))
        && !m.attachments.iter().any(|a| a.mime_type.to_ascii_lowercase().contains("pkcs7-signature"))
        && crate::cmd::compose::stored_head(s, m, KIND_SCAN).as_deref().is_none_or(|r| {
            !matches!(mailcraft_smime::mime::detect(r), Some(mailcraft_smime::mime::Kind::OpaqueSigned))
        })
}

fn source_key(m: &MailItem) -> u64 {
    let mut bytes = format!("{}:{}:{:?}", m.size, m.received.unix, m.blob).into_bytes();
    if let Some(r) = &m.raw {
        // The length and the end of the source: a re-saved draft of the same length still differs there (the
        // signature or the ciphertext comes last).
        bytes.extend_from_slice(format!(":{}:", r.len()).as_bytes());
        bytes.extend_from_slice(r.get(r.len().saturating_sub(512)..).unwrap_or_default());
    }
    mailcraft_model::thread::fnv1a(&bytes)
}

/// The opened form of S/MIME item `id` (None for other mail), cached in memory.
pub fn view(s: &Session, id: ItemId) -> Option<Arc<View>> {
    let m = s.mailbox.mail.get(&id)?;
    if !is_smime(m) {
        return None;
    }
    let key = source_key(m);
    if let Some(v) = cache(s).views.iter().find(|(i, k, _)| *i == id && *k == key).map(|(_, _, v)| v.clone())
    {
        return Some(v);
    }
    let raw = crate::cmd::file::raw_source(s, m);
    let ids = identities(s);
    let anchors = anchors(s);
    let opened = mailcraft_smime::mime::open(&raw, &ids, &anchors, s.now().unix)?;
    let v = Arc::new(build_view(s, m, opened));
    let mut c = cache(s);
    c.views.retain(|(i, _, _)| *i != id);
    c.views.push_back((id, key, v.clone()));
    while c.views.len() > MAX_VIEWS {
        c.views.pop_front();
    }
    Some(v)
}

fn build_view(s: &Session, m: &MailItem, opened: Opened) -> View {
    let mut item = m.clone();
    if opened.locked() {
        item.body = Body { format: BodyFormat::PlainText, text: Some(LOCKED_TEXT.into()), html: None };
        item.preview = "Encrypted message".into();
    } else if let Ok(msg) = mailcraft_mime::parse(&opened.message) {
        let fresh = MailItem::from_message(&msg, &opened.message, m.received);
        item.body = fresh.body;
        item.attachments = fresh.attachments;
        item.preview = fresh.preview;
    }
    item.raw = Some(opened.message.clone());
    item.blob = None;
    let sender =
        m.from.as_ref().or(m.sender.as_ref()).map(|b| b.addr.trim().to_ascii_lowercase()).unwrap_or_default();
    let mut sec = Security { sender: sender.clone(), ..Security::default() };
    let mut signer = None;
    for layer in &opened.layers {
        match layer {
            Layer::Encrypted(r) => {
                sec.encrypted = true;
                if let Err(e) = r {
                    sec.locked = true;
                    sec.decrypt_error = e.clone();
                }
            }
            Layer::Signed(sig) if !sec.signed => {
                sec.signed = true;
                sec.status = sig.status.name().into();
                sec.reason = sig.reason.clone();
                sec.clear_signed = sig.clear;
                sec.digest = sig.digest.unwrap_or_default().into();
                sec.signing_time =
                    sig.signing_time.map(|t| date_text(t, s.tz_offset_min)).unwrap_or_default();
                sec.chain = sig.chain.iter().map(|c| cert_info(s, c)).collect();
                if let Some(c) = &sig.signer {
                    let addr = c.emails().into_iter().next().unwrap_or_default();
                    sec.signer = if addr.is_empty() || c.display_name() == addr {
                        c.display_name()
                    } else {
                        format!("{} <{addr}>", c.display_name())
                    };
                    sec.signer_fingerprint = c.fingerprint();
                    sec.address_matches = !sender.is_empty() && c.has_email(&sender);
                    if sig.status == Status::Valid {
                        signer = Some(c.clone());
                    }
                }
            }
            Layer::Signed(_) => {}
        }
    }
    View { item, raw: opened.message, security: sec, signer }
}

/// The item as it reads: the opened form of an S/MIME message, else the stored item.
pub fn display_item(s: &Session, id: ItemId) -> Option<std::borrow::Cow<'_, MailItem>> {
    match view(s, id) {
        Some(v) => Some(std::borrow::Cow::Owned(v.item.clone())),
        None => s.mailbox.mail.get(&id).map(std::borrow::Cow::Borrowed),
    }
}

/// The message source with its S/MIME layers opened (the stored source for other mail).
pub fn open_source(s: &Session, m: &MailItem) -> Vec<u8> {
    match view(s, m.id) {
        Some(v) => v.raw.clone(),
        None => crate::cmd::file::raw_source(s, m),
    }
}

/// What the message's S/MIME layers say (None for other mail).
pub fn security(s: &Session, id: ItemId) -> Option<Security> {
    view(s, id).map(|v| v.security.clone())
}

/// One info bar line: (kind, text) with kind `ok`, `warning`, `error` or `info`.
pub fn info_lines(sec: &Security) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if sec.locked {
        out.push((
            "error",
            format!("This message is encrypted and cannot be opened: {}.", sec.decrypt_error),
        ));
    } else if sec.encrypted {
        out.push(("info", "Encrypted: only the recipients of this message can read it.".to_owned()));
    }
    if sec.signed {
        let who = if sec.signer.is_empty() { "an unknown signer".to_owned() } else { sec.signer.clone() };
        let line = match sec.status.as_str() {
            "valid" if sec.address_matches => {
                ("ok", format!("Signed by {who}. The digital signature is valid."))
            }
            "valid" => (
                "warning",
                format!(
                    "Signed by {who}, but the certificate is not issued to the sender's address ({}).",
                    if sec.sender.is_empty() { "unknown" } else { sec.sender.as_str() }
                ),
            ),
            "untrusted" => {
                ("warning", format!("Signed by {who}, but the signature is not trusted. {}", sec.reason))
            }
            "expired" => {
                ("warning", format!("Signed by {who}, but the certificate is not valid. {}", sec.reason))
            }
            _ => ("error", format!("The digital signature on this message is not valid. {}", sec.reason)),
        };
        out.push(line);
    }
    out
}

/// Remembers the certificate of a valid signer of item `id` for its sender's address (once per item).
pub fn on_view(s: &mut Session, id: ItemId) {
    if cache(s).harvested.contains(&id) {
        return;
    }
    let Some(v) = view(s, id) else { return };
    cache(s).harvested.insert(id);
    let Some(cert) = v.signer.clone() else { return };
    let sender = v.security.sender.clone();
    if sender.is_empty() || !cert.has_email(&sender) || !cert.can_encrypt() {
        return;
    }
    if !add_person_cert(s, &cert, Some(&sender), "mail").is_empty() {
        save(s);
    }
}

/// New mail: signed messages teach the senders' certificates (for encrypted replies later).
pub(crate) fn on_new_mail(s: &mut Session, ids: &[ItemId]) {
    let signed: Vec<ItemId> =
        ids.iter().copied().filter(|i| s.mailbox.mail.get(i).is_some_and(is_smime)).take(500).collect();
    if signed.is_empty() {
        return;
    }
    load(s);
    for id in signed {
        on_view(s, id);
    }
}

// =========================================================================================================
// Commands
// =========================================================================================================

fn spec(
    id: &'static str,
    label: &'static str,
    location: &'static str,
    params: &'static str,
    enabled: fn(&Session) -> Option<&'static str>,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec { id, label, location, shortcut: "", params, mutates: false, enabled, run }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "options.digitalIds",
            "Digital IDs...",
            "File › Options › Trust Center › Email Security",
            r#"{"show"?:bool} (false: the list only, no dialog)"#,
            crate::always,
            digital_ids,
        ),
        spec(
            "options.importDigitalId",
            "Import Digital ID",
            "Digital IDs › Import",
            r#"{"path"?:string, "data"?:base64 (.pfx/.p12), "password"?:string, "keyPath"?:string, "keyPem"?:string, "certPath"?:string, "certPem"?:string, "trustIssuer"?:bool}"#,
            crate::always,
            import_digital_id,
        ),
        spec(
            "options.exportDigitalId",
            "Export Digital ID",
            "Digital IDs › Export",
            r#"{"fingerprint":string, "path":string, "password":string}"#,
            has_ids,
            export_digital_id,
        ),
        spec(
            "options.removeDigitalId",
            "Remove Digital ID",
            "Digital IDs › Remove",
            r#"{"fingerprint":string}"#,
            has_ids,
            remove_digital_id,
        ),
        spec(
            "options.setDefaultDigitalId",
            "Set as Default",
            "Digital IDs › Set as Default",
            r#"{"fingerprint":string}"#,
            has_ids,
            set_default_digital_id,
        ),
        spec(
            "options.importCertificate",
            "Import Certificate",
            "Digital IDs › Other People's Certificates › Import",
            r#"{"path"?:string, "data"?:base64, "pem"?:string, "address"?:string}"#,
            crate::always,
            import_certificate,
        ),
        spec(
            "options.removeCertificate",
            "Remove Certificate",
            "Digital IDs › Other People's Certificates › Remove",
            r#"{"fingerprint":string, "address"?:string}"#,
            crate::always,
            remove_certificate,
        ),
        spec(
            "options.trustCertificate",
            "Trust Certificate",
            "Digital IDs › Trusted Certificates; Signature Details › Trust",
            r#"{"fingerprint"?:string, "id"?:number (trust the root of this message's signer), "path"?:string, "pem"?:string, "trust"?:bool}"#,
            crate::always,
            trust_certificate,
        ),
        spec(
            "mail.signatureDetails",
            "Signature Details",
            "Reading Pane › Security info bar › Details",
            r#"{"id"?:number}"#,
            crate::always,
            signature_details,
        ),
    ]
}

fn has_ids(s: &Session) -> Option<&'static str> {
    if s.smime.book.ids.is_empty() { Some("no digital IDs are installed") } else { None }
}

fn digital_ids(s: &mut Session, p: &Value) -> CmdResult {
    load(s);
    let state = summary(s);
    if opt_bool(p, "show") == Some(false) {
        return Ok(state);
    }
    dialog(s, "digitalIds", state.clone())?;
    Ok(state)
}

fn import_digital_id(s: &mut Session, p: &Value) -> CmdResult {
    load(s);
    let pw = password(p)?;
    let trust = opt_bool(p, "trustIssuer").unwrap_or(false);
    let id = if let Some(bytes) = input_bytes(p, "path", "data", "")? {
        mailcraft_smime::pkcs12::parse(&bytes, &pw).map_err(smime_err)?
    } else {
        let key = input_bytes(p, "keyPath", "", "keyPem")?.ok_or_else(|| {
            CmdError::Params("a .pfx file (`path` or `data`) or a key (`keyPath`, `keyPem`) is needed".into())
        })?;
        let certs = input_bytes(p, "certPath", "", "certPem")?
            .ok_or_else(|| CmdError::Params("the certificate (`certPath` or `certPem`) is needed".into()))?;
        let key = PrivateKey::from_file(&key, &pw).map_err(smime_err)?;
        let certs = Cert::parse_any(&certs).map_err(smime_err)?;
        Identity::from_key_and_certs(key, certs).map_err(smime_err)?
    };
    let r = add_identity(s, &id, trust)?;
    s.notify(Notice::Info(format!("The digital ID for {} was imported.", id.cert.display_name())));
    Ok(r)
}

fn find_id(s: &mut Session, p: &Value) -> Result<String, CmdError> {
    let fp = opt_str(p, "fingerprint").map(|f| f.trim().to_ascii_lowercase()).unwrap_or_default();
    if book(s).ids.iter().any(|i| i.fingerprint == fp) {
        Ok(fp)
    } else {
        Err(CmdError::Params("no digital ID with that fingerprint".into()))
    }
}

fn export_digital_id(s: &mut Session, p: &Value) -> CmdResult {
    let fp = find_id(s, p)?;
    let path = opt_str(p, "path")
        .filter(|x| !x.trim().is_empty())
        .ok_or_else(|| CmdError::Params("`path` required".into()))?;
    let pw = password(p)?;
    if pw.is_empty() {
        return Err(CmdError::Params("a password is required to protect the exported key".into()));
    }
    let id = identities(s)
        .iter()
        .find(|i| i.cert.fingerprint() == fp)
        .cloned()
        .ok_or_else(|| CmdError::Failed("the key of this digital ID is not available".into()))?;
    let pfx = mailcraft_smime::pkcs12::build(&id, &pw, &id.cert.display_name()).map_err(smime_err)?;
    std::fs::write(path.trim(), &pfx).map_err(|e| CmdError::Failed(format!("cannot write {path}: {e}")))?;
    Ok(json!({"path": path.trim(), "bytes": pfx.len()}))
}

fn remove_digital_id(s: &mut Session, p: &Value) -> CmdResult {
    let fp = find_id(s, p)?;
    let b = book(s);
    let was_default = b.ids.iter().any(|i| i.fingerprint == fp && i.default);
    b.ids.retain(|i| i.fingerprint != fp);
    if was_default && let Some(first) = b.ids.first_mut() {
        first.default = true;
    }
    if let Err(e) = s.secrets.remove(&secret_key(&fp)) {
        log::warn!("removing the key of digital ID {fp}: {e}");
    }
    save(s);
    Ok(json!({"removed": fp}))
}

fn set_default_digital_id(s: &mut Session, p: &Value) -> CmdResult {
    let fp = find_id(s, p)?;
    for i in &mut book(s).ids {
        i.default = i.fingerprint == fp;
    }
    save(s);
    Ok(json!({"default": fp}))
}

fn import_certificate(s: &mut Session, p: &Value) -> CmdResult {
    load(s);
    let bytes = input_bytes(p, "path", "data", "pem")?
        .ok_or_else(|| CmdError::Params("a certificate (`path`, `data` or `pem`) is needed".into()))?;
    let certs = Cert::parse_any(&bytes).map_err(smime_err)?;
    let address = opt_str(p, "address").map(str::trim).filter(|a| !a.is_empty());
    if let Some(a) = address
        && (!a.contains('@') || a.chars().count() > 320)
    {
        return Err(CmdError::Params(format!("`{a}` is not an e-mail address")));
    }
    let mut stored = Vec::new();
    for c in certs.iter().filter(|c| !c.is_ca() && (address.is_some() || !c.emails().is_empty())) {
        stored.extend(add_person_cert(s, c, address, "file"));
    }
    if stored.is_empty() && certs.iter().all(|c| c.emails().is_empty() || c.is_ca()) {
        return Err(CmdError::Failed(
            "The certificate names no e-mail address. Give the address it is for, or trust it as an authority."
                .into(),
        ));
    }
    save(s);
    Ok(json!({"addresses": stored}))
}

fn remove_certificate(s: &mut Session, p: &Value) -> CmdResult {
    let fp = opt_str(p, "fingerprint").map(|f| f.trim().to_ascii_lowercase()).unwrap_or_default();
    let address = opt_str(p, "address").map(|a| a.trim().to_ascii_lowercase());
    let b = book(s);
    let before = b.people.len();
    b.people.retain(|c| !(c.fingerprint == fp && address.as_ref().is_none_or(|a| *a == c.address)));
    let removed = before - b.people.len();
    if removed == 0 {
        return Err(CmdError::Params("no such certificate".into()));
    }
    save(s);
    Ok(json!({"removed": removed}))
}

fn trust_certificate(s: &mut Session, p: &Value) -> CmdResult {
    load(s);
    let on = opt_bool(p, "trust").unwrap_or(true);
    let cert: Cert = if let Some(id) = p.get("id").and_then(Value::as_u64) {
        // The end of the signer's chain: its root, or the signer itself when nothing issued it.
        signer_chain(s, ItemId(id))
            .pop()
            .ok_or_else(|| CmdError::Params("the message has no signer's certificate".into()))?
    } else if let Some(fp) =
        opt_str(p, "fingerprint").map(|f| f.trim().to_ascii_lowercase()).filter(|f| !f.is_empty())
    {
        let b = &s.smime.book;
        b.trusted
            .iter()
            .chain(b.people.iter().map(|c| &c.cert))
            .chain(b.ids.iter().flat_map(|i| std::iter::once(&i.cert).chain(i.chain.iter())))
            .filter_map(|t| cert_of(t))
            .find(|c| c.fingerprint() == fp)
            .ok_or_else(|| CmdError::Params("no certificate with that fingerprint".into()))?
    } else {
        let bytes = input_bytes(p, "path", "data", "pem")?.ok_or_else(|| {
            CmdError::Params("a certificate (`fingerprint`, `id`, `path` or `pem`) is needed".into())
        })?;
        Cert::parse_any(&bytes)
            .map_err(smime_err)?
            .into_iter()
            .next()
            .ok_or_else(|| CmdError::Failed("no certificate".into()))?
    };
    let der = b64(cert.der());
    if on {
        add_trusted(s, &cert);
    } else {
        book(s).trusted.retain(|t| *t != der);
    }
    save(s);
    Ok(json!({"fingerprint": cert.fingerprint(), "trusted": on, "name": cert.display_name()}))
}

/// The signer's chain of message `id` (signer first), from its signature.
fn signer_chain(s: &Session, id: ItemId) -> Vec<Cert> {
    let Some(m) = s.mailbox.mail.get(&id) else { return Vec::new() };
    let raw = crate::cmd::file::raw_source(s, m);
    let ids = identities(s);
    let anchors = anchors(s);
    let Some(o) = mailcraft_smime::mime::open(&raw, &ids, &anchors, s.now().unix) else { return Vec::new() };
    o.signature().map(|sig| sig.chain.clone()).unwrap_or_default()
}

fn signature_details(s: &mut Session, p: &Value) -> CmdResult {
    let id = p
        .get("id")
        .and_then(Value::as_u64)
        .map(ItemId)
        .or(s.selection.focus)
        .ok_or_else(|| CmdError::Disabled("No message is selected.".into()))?;
    load(s);
    let sec = security(s, id)
        .ok_or_else(|| CmdError::Disabled("This message is not signed or encrypted.".into()))?;
    let mut state = serde_json::to_value(&sec).map_err(|e| CmdError::Failed(e.to_string()))?;
    if let Some(o) = state.as_object_mut() {
        o.insert("id".into(), json!(id.0));
        o.insert(
            "lines".into(),
            json!(
                info_lines(&sec).into_iter().map(|(k, t)| json!({"kind": k, "text": t})).collect::<Vec<_>>()
            ),
        );
    }
    dialog(s, "messageSecurity", state.clone())?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::compose::tests::session;
    use mailcraft_model::{Contact, FolderRole};
    use mailcraft_smime::testkit;

    const PW: &str = "pfx-pw";

    fn pfx_of(id: &Identity) -> String {
        b64(&mailcraft_smime::pkcs12::build(id, PW, "").unwrap())
    }

    /// `s` gets the digital ID of `id`, its CA trusted.
    fn import(s: &mut Session, id: &Identity) {
        let r = s
            .run("options.importDigitalId", &json!({"data": pfx_of(id), "password": PW, "trustIssuer": true}))
            .unwrap();
        assert_eq!(r["fingerprint"], id.cert.fingerprint());
    }

    /// A session for `addr` with its digital ID and the test CA trusted.
    fn person(addr: &str, name: &str) -> (Session, Identity) {
        let mut s = session();
        if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.address == "me@example.com") {
            a.address = addr.into();
            a.sender_name = name.into();
        }
        s.refresh_me();
        let id = testkit::identity(name, addr);
        import(&mut s, &id);
        (s, id)
    }

    fn inbox(s: &Session) -> mailcraft_model::FolderId {
        let a = s.mailbox.accounts.iter().find(|a| a.can_send()).unwrap().id;
        s.mailbox.role_folder(a, FolderRole::Inbox).unwrap()
    }

    /// Files `raw` in `s`'s Inbox as downloaded mail and runs the new-mail steps.
    fn deliver(s: &mut Session, raw: &[u8]) -> ItemId {
        let msg = mailcraft_mime::parse(raw).unwrap();
        let mut item = MailItem::from_message(&msg, raw, s.now());
        item.folder = inbox(s);
        let id = s.mailbox.add_mail(item);
        crate::sync::arrival::on_new_mail(s, &[id]);
        id
    }

    /// Sends a message from `s` to `to` with Sign / Encrypt as given; the Outbox item.
    fn send(s: &mut Session, to: &str, sign: bool, encrypt: bool, extra: Value) -> (Value, Option<ItemId>) {
        let cid = s.open_compose(ComposeState::default());
        s.run("compose.setTo", &json!({"compose": cid, "value": to})).unwrap();
        s.run("compose.setSubject", &json!({"compose": cid, "value": "Plans"})).unwrap();
        s.run("compose.insertText", &json!({"compose": cid, "text": "Meet at noon by the old mill."}))
            .unwrap();
        s.run("compose.sign", &json!({"compose": cid, "on": sign})).unwrap();
        s.run("compose.encrypt", &json!({"compose": cid, "on": encrypt})).unwrap();
        let mut p = json!({"compose": cid});
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        let r = s.run("compose.send", &p).unwrap();
        let item = r["item"].as_u64().map(ItemId);
        (r, item)
    }

    fn raw_of(s: &Session, id: ItemId) -> Vec<u8> {
        s.mailbox.mail[&id].raw.clone().unwrap()
    }

    #[test]
    fn digital_ids_import_list_export_and_remove() {
        let mut s = session();
        let me = testkit::identity("Me Example", "me@example.com");
        let bad = s.run("options.importDigitalId", &json!({"data": pfx_of(&me), "password": "wrong"}));
        assert!(bad.unwrap_err().to_string().contains("incorrect"));
        import(&mut s, &me);
        let list = s.run("options.digitalIds", &json!({"show": false})).unwrap();
        assert_eq!(list["ids"].as_array().unwrap().len(), 1);
        assert_eq!(list["ids"][0]["usable"], true);
        assert_eq!(list["ids"][0]["default"], true);
        assert_eq!(list["ids"][0]["emails"], json!(["me@example.com"]));
        assert_eq!(list["trusted"][0]["name"], testkit::CA_NAME, "the issuer is trusted on request");
        assert!(s.notices.iter().any(|n| matches!(n, Notice::Info(t) if t.contains("imported"))));
        // The dialog opens with the same state.
        s.run("options.digitalIds", &json!({})).unwrap();
        assert!(s.notices.iter().any(|n| matches!(n, Notice::OpenDialog(d, _) if d == "digitalIds")));
        // The key is sealed under a secret kept with the passwords.
        let fp = me.cert.fingerprint();
        let secret = s.secrets.get(&secret_key(&fp)).expect("sealing secret stored");
        let sealed = unb64(&s.smime.book.ids[0].key).unwrap();
        assert!(PrivateKey::unseal(&sealed, &secret).unwrap().matches(&me.cert));
        assert!(PrivateKey::unseal(&sealed, "guess").is_err());
        let der = me.key.to_pkcs8_der().unwrap();
        assert!(!s.smime.book.ids[0].key.contains(&b64(&der[..32])), "no plain key in the file");
        // Export and read back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("me.pfx");
        let path_s = path.display().to_string();
        assert!(
            s.run("options.exportDigitalId", &json!({"fingerprint": fp, "path": path_s, "password": ""}))
                .is_err()
        );
        s.run("options.exportDigitalId", &json!({"fingerprint": fp, "path": path_s, "password": "x1"}))
            .unwrap();
        let back = mailcraft_smime::pkcs12::parse(&std::fs::read(&path).unwrap(), "x1").unwrap();
        assert_eq!(back.cert, me.cert);
        // A PEM key and certificate import too.
        let other = testkit::identity("Me Again", "me@example.com");
        let key_pem = mailcraft_smime::cert::pem("PRIVATE KEY", &other.key.to_pkcs8_der().unwrap());
        s.run("options.importDigitalId", &json!({"keyPem": key_pem, "certPem": other.cert.to_pem()}))
            .unwrap();
        assert_eq!(identities(&s).len(), 2);
        s.run("options.setDefaultDigitalId", &json!({"fingerprint": other.cert.fingerprint()})).unwrap();
        assert_eq!(identity_for(&s, "me@example.com", Purpose::Sign).unwrap().cert, other.cert);
        // Remove.
        s.run("options.removeDigitalId", &json!({"fingerprint": fp})).unwrap();
        assert!(s.secrets.get(&secret_key(&fp)).is_none());
        assert_eq!(identities(&s).len(), 1);
        assert!(s.run("options.removeDigitalId", &json!({"fingerprint": fp})).is_err());
    }

    #[test]
    fn signed_and_encrypted_mail_round_trip_between_two_people() {
        let (mut me, my_id) = person("me@example.com", "Me Example");
        let (mut bob, bob_id) = person("bob@example.org", "Bob Day");
        // Me knows Bob's certificate from a file.
        let r = me.run("options.importCertificate", &json!({"pem": bob_id.cert.to_pem()})).unwrap();
        assert_eq!(r["addresses"], json!(["bob@example.org"]));
        let (r, out) = send(&mut me, "Bob Day <bob@example.org>", true, true, json!({}));
        assert_eq!(r["sent"], true, "{r}");
        let out = out.unwrap();
        let raw = raw_of(&me, out);
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("smime-type=enveloped-data"), "{text}");
        assert!(!text.contains("old mill"), "nothing readable in transit");
        assert!(text.contains("Subject: Plans"));
        // The sender's own copy is readable (encrypted to self) and shows a valid signature.
        let sec = security(&me, out).unwrap();
        assert!(sec.encrypted && !sec.locked && sec.signed, "{sec:?}");
        assert_eq!(sec.status, "valid", "{sec:?}");
        assert!(display_item(&me, out).unwrap().body_text().contains("old mill"));
        // Bob receives it: decrypted, signed by Me, and Me's certificate is remembered.
        let got = deliver(&mut bob, &raw);
        let sec = security(&bob, got).unwrap();
        assert_eq!((sec.encrypted, sec.locked, sec.status.as_str()), (true, false, "valid"), "{sec:?}");
        assert!(sec.address_matches && sec.signer.contains("me@example.com"), "{sec:?}");
        assert_eq!(sec.digest, "SHA-256");
        assert_eq!(sec.chain.len(), 2);
        let lines = info_lines(&sec);
        assert_eq!(lines[0].0, "info");
        assert_eq!(lines[1].0, "ok", "{lines:?}");
        assert!(lines[1].1.starts_with("Signed by Me Example <me@example.com>"));
        let shown = display_item(&bob, got).unwrap();
        assert!(shown.body_text().contains("old mill"));
        assert!(shown.attachments.is_empty(), "no smime.p7m chip once opened");
        assert_eq!(
            cert_for(&bob, "me@example.com"),
            Some(my_id.cert.clone()),
            "harvested from the signature"
        );
        // The header and the reply use the opened message.
        let h = crate::queries::reading_header(&bob, got).unwrap();
        assert!(h.attachments.is_empty());
        let reply = crate::cmd::respond::build_response(&bob, got, crate::compose::ComposeKind::Reply, false)
            .unwrap();
        assert!(reply.body.to_plain_text().contains("old mill"));
        // Bob replies encrypted without importing anything: the reply to an encrypted message starts encrypted.
        bob.selection.items = vec![got];
        bob.selection.focus = Some(got);
        let cid = bob.run("mail.reply", &json!({"ids": [got.0]})).unwrap()["compose"].as_u64().unwrap();
        assert!(bob.composes[&cid].encrypt);
        let r = bob.run("compose.send", &json!({"compose": cid})).unwrap();
        assert_eq!(r["sent"], true, "{r}");
        let reply = deliver(&mut me, &raw_of(&bob, ItemId(r["item"].as_u64().unwrap())));
        let sec = security(&me, reply).unwrap();
        assert!(sec.encrypted && !sec.locked && !sec.signed);
        // Somebody else cannot open it, and nothing breaks.
        let (mut eve, _) = person("eve@example.net", "Eve Ray");
        let stolen = deliver(&mut eve, &raw);
        let sec = security(&eve, stolen).unwrap();
        assert!(sec.locked && !sec.signed, "{sec:?}");
        assert!(sec.decrypt_error.contains("none of your digital IDs"));
        assert_eq!(display_item(&eve, stolen).unwrap().body.text.as_deref(), Some(LOCKED_TEXT));
        assert_eq!(info_lines(&sec)[0].0, "error");
        assert!(cert_for(&eve, "me@example.com").is_none());
    }

    #[test]
    fn missing_certificates_offer_sending_unencrypted() {
        let (mut me, _) = person("me@example.com", "Me Example");
        let (r, item) = send(&mut me, "stranger@example.net", true, true, json!({}));
        assert_eq!(r["sent"], false);
        assert_eq!(r["reason"], "missingCertificates");
        assert_eq!(r["missing"], json!(["stranger@example.net"]));
        assert!(item.is_none());
        assert!(me.notices.iter().any(|n| matches!(n, Notice::OpenDialog(d, p)
            if d == "encryptionProblem" && p["missing"] == json!(["stranger@example.net"]))));
        let cid = r["compose"].as_u64().unwrap();
        assert!(me.composes.contains_key(&cid), "the message stays open");
        assert_eq!(
            me.run("compose.encrypt", &json!({"compose": cid, "on": true})).unwrap()["missing"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // "Send unencrypted": signed only.
        let r = me.run("compose.send", &json!({"compose": cid, "encryption": "skip"})).unwrap();
        assert_eq!(r["sent"], true, "{r}");
        let raw = raw_of(&me, ItemId(r["item"].as_u64().unwrap()));
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/signed") && !text.contains("enveloped-data"));
        assert!(text.contains("old mill"), "clear-signed text is readable");
    }

    #[test]
    fn email_security_settings_take_effect() {
        let (mut me, _) = person("me@example.com", "Me Example");
        // Off by default.
        let c = me.open_compose(ComposeState::default());
        assert!(!me.composes[&c].sign && !me.composes[&c].encrypt);
        me.run("options.set", &json!({"key": "trust.smime.sign", "value": true})).unwrap();
        me.run("options.set", &json!({"key": "trust.smime.encrypt", "value": true})).unwrap();
        let c = me.open_compose(ComposeState::default());
        assert!(me.composes[&c].sign && me.composes[&c].encrypt);
        // Clear-signed by default; off sends opaque signed data.
        let (_, item) = send(&mut me, "me@example.com", true, false, json!({}));
        assert!(String::from_utf8_lossy(&raw_of(&me, item.unwrap())).contains("multipart/signed"));
        me.run("options.set", &json!({"key": "trust.smime.clearSigned", "value": false})).unwrap();
        let (_, item) = send(&mut me, "me@example.com", true, false, json!({}));
        let item = item.unwrap();
        let text = String::from_utf8_lossy(&raw_of(&me, item)).into_owned();
        assert!(text.contains("smime-type=signed-data") && !text.contains("old mill"), "{text}");
        let sec = security(&me, item).unwrap();
        assert_eq!((sec.status.as_str(), sec.clear_signed), ("valid", false));
        assert!(display_item(&me, item).unwrap().body_text().contains("old mill"));
    }

    #[test]
    fn tampered_untrusted_and_mismatched_signatures() {
        let (mut me, _) = person("me@example.com", "Me Example");
        let (_, item) = send(&mut me, "me@example.com", true, false, json!({}));
        let raw = String::from_utf8(raw_of(&me, item.unwrap())).unwrap();
        // Bob trusts nothing: the signature is good but untrusted.
        let mut bob = session();
        let got = deliver(&mut bob, raw.as_bytes());
        let sec = security(&bob, got).unwrap();
        assert_eq!(sec.status, "untrusted", "{sec:?}");
        assert_eq!(info_lines(&sec)[0].0, "warning");
        assert!(cert_for(&bob, "me@example.com").is_none(), "untrusted signers are not remembered");
        // Trusting the root from the message makes it valid.
        bob.run("options.trustCertificate", &json!({"id": got.0})).unwrap();
        assert_eq!(security(&bob, got).unwrap().status, "valid");
        // A changed body is invalid.
        let tampered = raw.replace("old mill", "new mill");
        let bad = deliver(&mut bob, tampered.as_bytes());
        let sec = security(&bob, bad).unwrap();
        assert_eq!(sec.status, "invalid");
        assert_eq!(info_lines(&sec)[0].0, "error");
        // Signed by Me, but sent as someone else: the address does not match.
        let forged = raw.replace("From: Me Example <me@example.com>", "From: Boss <boss@example.com>");
        let f = deliver(&mut bob, forged.as_bytes());
        let sec = security(&bob, f).unwrap();
        assert!(sec.status == "valid" && !sec.address_matches, "{sec:?}");
        assert_eq!(info_lines(&sec)[0].0, "warning");
        // Signature details open the dialog with the same facts.
        let d = bob.run("mail.signatureDetails", &json!({"id": f.0})).unwrap();
        assert_eq!(d["status"], "valid");
        assert_eq!(d["chain"].as_array().unwrap().len(), 2);
        assert!(bob.run("mail.signatureDetails", &json!({"id": u64::MAX})).is_err());
        // Untrusting again.
        let root = d["chain"][1]["fingerprint"].as_str().unwrap().to_owned();
        bob.run("options.trustCertificate", &json!({"fingerprint": root, "trust": false})).unwrap();
        assert_eq!(security(&bob, got).unwrap().status, "untrusted");
    }

    #[test]
    fn contact_certificates_are_used_for_encryption() {
        let (mut me, _) = person("me@example.com", "Me Example");
        let pat = testkit::identity("Pat Kim", "pat@example.org");
        let cid = ItemId(me.mailbox.alloc_id());
        me.mailbox.contacts.insert(
            cid,
            Contact {
                id: cid,
                first_name: "Pat".into(),
                emails: vec!["Pat@Example.org".into()],
                certificates: vec![b64(pat.cert.der())],
                ..Default::default()
            },
        );
        assert_eq!(cert_for(&me, "pat@example.org"), Some(pat.cert.clone()));
        let (r, item) = send(&mut me, "pat@example.org", false, true, json!({}));
        assert_eq!(r["sent"], true, "{r}");
        let raw = raw_of(&me, item.unwrap());
        let (mut pat_s, _) = person("pat@example.org", "Pat Kim");
        let got = deliver(&mut pat_s, &raw);
        assert!(display_item(&pat_s, got).unwrap().body_text().contains("old mill"));
    }

    #[test]
    fn hostile_contact_cards_cannot_redirect_encryption() {
        // Regression: a card's KEY was taken without checking that it names the address, and the newest
        // certificate from any source won, so a received business card could silently replace the key mail
        // to a correspondent is encrypted for.
        let (mut me, _) = person("me@example.com", "Me Example");
        let (mut pat_s, pat) = person("pat@example.org", "Pat Kim");
        let (_, signed) = send(&mut pat_s, "me@example.com", true, false, json!({}));
        deliver(&mut me, &raw_of(&pat_s, signed.unwrap()));
        assert_eq!(cert_choice(&me, "pat@example.org"), Some((pat.cert.clone(), CertSource::Mail)));
        // A card for Pat with a self-signed certificate for Pat's address that outlives the real one, and one
        // naming somebody else.
        let fake =
            testkit::identity_with("Pat Kim", "pat@example.org", 11, (testkit::VALID.0, 2_524_600_000), None);
        assert!(fake.cert.not_after() > pat.cert.not_after());
        let other = testkit::identity("Mal Dee", "mal@example.net");
        let card = ItemId(me.mailbox.alloc_id());
        me.mailbox.contacts.insert(
            card,
            Contact {
                id: card,
                first_name: "Pat".into(),
                emails: vec!["pat@example.org".into(), "pat2@example.org".into()],
                certificates: vec![b64(fake.cert.der()), b64(other.cert.der())],
                ..Default::default()
            },
        );
        assert_eq!(cert_for(&me, "pat@example.org"), Some(pat.cert.clone()), "mail wins over the card");
        assert_eq!(cert_for(&me, "pat2@example.org"), None, "no card certificate names that address");
        // Without the certificate from mail the card's is used, and the user is told it is not trusted.
        me.run("options.removeCertificate", &json!({"fingerprint": pat.cert.fingerprint()})).unwrap();
        assert_eq!(cert_choice(&me, "pat@example.org"), Some((fake.cert.clone(), CertSource::Contact)));
        me.notices.clear();
        let (r, _) = send(&mut me, "pat@example.org", false, true, json!({}));
        assert_eq!(r["sent"], true, "{r}");
        assert!(me.notices.iter().any(|n| matches!(n, Notice::Info(t)
            if t.contains("pat@example.org") && t.contains("not traced to a trusted authority"))));
        // A card certificate from a trusted authority ranks above an untrusted one, and needs no notice.
        if let Some(c) = me.mailbox.contacts.get_mut(&card) {
            c.certificates.push(b64(pat.cert.der()));
        }
        assert_eq!(cert_choice(&me, "pat@example.org"), Some((pat.cert.clone(), CertSource::TrustedContact)));
    }

    /// True when `m` holds `secret` readably (body, preview or source).
    fn holds(m: &MailItem, secret: &str) -> bool {
        m.body_text().contains(secret)
            || m.body.html.as_deref().is_some_and(|h| h.contains(secret))
            || m.preview.contains(secret)
            || m.raw.as_deref().is_some_and(|r| String::from_utf8_lossy(r).contains(secret))
    }

    #[test]
    fn drafts_of_encrypted_messages_stay_encrypted() {
        // Regression: a reply to an encrypted message quotes its decrypted text, and auto-save wrote that draft
        // to the mailbox, the journal and the server upload queue in the clear.
        let (mut me, _) = person("me@example.com", "Me Example");
        let (mut bob, bob_id) = person("bob@example.org", "Bob Day");
        me.run("options.importCertificate", &json!({"pem": bob_id.cert.to_pem()})).unwrap();
        let (_, out) = send(&mut me, "bob@example.org", false, true, json!({}));
        let got = deliver(&mut bob, &raw_of(&me, out.unwrap()));
        bob.run("options.set", &json!({"key": "mail.save.autoSaveMinutes", "value": 1})).unwrap();
        bob.selection.items = vec![got];
        bob.selection.focus = Some(got);
        let cid = bob.run("mail.reply", &json!({"ids": [got.0]})).unwrap()["compose"].as_u64().unwrap();
        assert!(bob.composes[&cid].body.to_plain_text().contains("old mill"));
        bob.run("compose.insertText", &json!({"compose": cid, "text": "Noon works. "})).unwrap();
        // Encrypt turned off: the draft still answers an encrypted message, so it stays encrypted.
        bob.run("compose.encrypt", &json!({"compose": cid, "on": false})).unwrap();
        bob.pending_ops.clear();
        let now = bob.now();
        bob.fixed_now = Some(DateTime { unix: now.unix + 120, offset_min: now.offset_min });
        let r = bob.run("compose.autoSaveTick", &json!({})).unwrap();
        let draft = ItemId(r["saved"][0]["draft"].as_u64().unwrap());
        let stored = &bob.mailbox.mail[&draft];
        assert!(stored.is_draft);
        assert!(String::from_utf8_lossy(&raw_of(&bob, draft)).contains("smime-type=enveloped-data"));
        for secret in ["old mill", "Noon works"] {
            assert!(!holds(stored, secret), "the stored draft holds `{secret}`");
            for op in &bob.pending_ops {
                let leaked = match op {
                    mailcraft_store::Op::PutMail(m) => holds(m, secret),
                    mailcraft_store::Op::Replace(mb) => mb.mail.values().any(|m| holds(m, secret)),
                    _ => false,
                };
                assert!(!leaked, "the journal holds `{secret}`");
            }
        }
        // Its writer can still read it.
        assert!(display_item(&bob, draft).unwrap().body_text().contains("Noon works"));
        // Saving explicitly, and Close All Items, keep it encrypted too.
        bob.run("compose.insertText", &json!({"compose": cid, "text": "See you."})).unwrap();
        bob.run("compose.saveDraft", &json!({"compose": cid})).unwrap();
        assert!(!holds(&bob.mailbox.mail[&draft], "See you."));
        bob.run("compose.insertText", &json!({"compose": cid, "text": " Bye."})).unwrap();
        bob.run("app.closeAllItems", &json!({"save": true})).unwrap();
        assert!(!holds(&bob.mailbox.mail[&draft], "Bye."));
        assert!(display_item(&bob, draft).unwrap().body_text().contains("Bye."));

        // Without a digital ID to keep it encrypted with, the draft is not saved at all (the window stays
        // open with the text).
        let mut s = session();
        let c = s.open_compose(ComposeState::default());
        s.run("compose.insertText", &json!({"compose": c, "text": "Merger terms"})).unwrap();
        s.run("compose.encrypt", &json!({"compose": c, "on": true})).unwrap();
        let e = s.run("compose.saveDraft", &json!({"compose": c})).unwrap_err();
        assert!(e.to_string().contains("not saved"), "{e}");
        assert!(s.mailbox.mail.values().all(|m| !holds(m, "Merger terms")));
        assert!(s.composes[&c].dirty);
    }

    #[test]
    fn no_digital_id_stops_sign() {
        let mut s = session();
        let (r, _) = send(&mut s, "a@example.org", true, false, json!({}));
        assert_eq!(r["reason"], "noDigitalId");
        assert!(s.notices.iter().any(|n| matches!(n, Notice::Error(t) if t.contains("digital ID"))));
        // An expired ID is refused with its date.
        let old =
            testkit::identity_with("Me Example", "me@example.com", 9, (1_500_000_000, 1_600_000_000), None);
        import(&mut s, &old);
        let (r, _) = send(&mut s, "a@example.org", true, false, json!({}));
        assert!(r["error"].as_str().unwrap().contains("expired"), "{r}");
    }

    #[test]
    fn certificates_persist_in_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let me = testkit::identity("Me Example", "me@example.com");
        let bob = testkit::identity("Bob Day", "bob@example.org");
        {
            let mut s = Session::open(dir.path()).unwrap();
            import(&mut s, &me);
            s.run("options.importCertificate", &json!({"pem": bob.cert.to_pem()})).unwrap();
        }
        let text = std::fs::read_to_string(dir.path().join(CERTS_FILE)).unwrap();
        assert!(text.contains("cannot open them") && text.contains(&me.cert.fingerprint()));
        let s = Session::open(dir.path()).unwrap();
        assert_eq!(identities(&s).len(), 1);
        assert_eq!(cert_for(&s, "bob@example.org"), Some(bob.cert.clone()));
        assert_eq!(anchors(&s).first(), Some(&testkit::ca()));
    }

    #[test]
    fn hostile_input_never_panics() {
        let (mut s, _) = person("me@example.com", "Me Example");
        for p in [
            json!({"data": "!!!"}),
            json!({"data": b64(b"\x30\x80\x30\x80")}),
            json!({"keyPem": "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----", "certPem": "x"}),
            json!({"path": "/no/such/file.pfx"}),
            json!({}),
        ] {
            assert!(s.run("options.importDigitalId", &p).is_err(), "{p}");
        }
        for p in [json!({"pem": "junk"}), json!({"data": b64(b"junk")}), json!({})] {
            assert!(s.run("options.importCertificate", &p).is_err(), "{p}");
        }
        assert!(s.run("options.trustCertificate", &json!({"fingerprint": "00"})).is_err());
        // Junk where S/MIME parts should be.
        for body in [
            "Content-Type: application/pkcs7-mime; smime-type=enveloped-data; name=smime.p7m\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nMIIBAAAA////\r\n",
            "Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; boundary=b\r\n\r\n\
             --b\r\nContent-Type: text/plain\r\n\r\nhi\r\n--b\r\nContent-Type: application/pkcs7-signature; \
             name=smime.p7s\r\n\r\n\u{1}\u{2}\r\n--b--\r\n",
        ] {
            let raw = format!("From: x@example.com\r\nSubject: junk\r\nMIME-Version: 1.0\r\n{body}");
            let id = deliver(&mut s, raw.as_bytes());
            let sec = security(&s, id);
            assert!(sec.is_some());
            let _ = display_item(&s, id);
            let _ = crate::queries::reading_header(&s, id);
            let _ = s.run("mail.signatureDetails", &json!({"id": id.0}));
        }
    }
}
