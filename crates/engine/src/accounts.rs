//! Accounts (File › Info › Add Account / Account Settings, `plan/reference/backstage.md`): the `accounts.*`
//! commands, the credentials store, and the connection settings built from an account plus its secret.
//!
//! Secrets (passwords, OAuth access and refresh tokens) never enter the mailbox, its snapshot or its journal.
//! They live in a [`SecretStore`] on the session: in memory by default, in `secrets.json` next to the data file
//! (owner read/write only, **not encrypted**, labelled so inside and in Account Settings), or, after
//! [`Session::use_os_keyring`], in the operating system's credential store ([`KeyringSecretStore`]: Keychain,
//! Credential Manager or the Secret Service), with the file as the fallback where no keyring exists.
//!
//! Per-account options that the mailbox model has no field for yet (incoming protocol POP3, download mode,
//! Send/Receive group membership and interval, push, POP3 server copies, OAuth registration) and the
//! bookkeeping of header-only downloads, POP3 unique ids and JMAP states live in [`AccountPrefs`], kept in
//! `account-options.json` next to the data file (see [`ACCOUNT_OPTIONS_FILE`]).
//!
//! OAuth 2.0 sign-in (`accounts.oauthSignIn`) runs the device authorization grant (RFC 8628) or the
//! authorization-code flow with PKCE through a loopback redirect (RFC 7636, RFC 8252) on a background thread,
//! for providers from the bundled public metadata (`mailcraft_net::oauth::builtin_providers`) or custom
//! endpoints. The user supplies the client id; none is built in.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use serde_json::{Map, Value, json};

use crate::cmd::util::opt_bool;
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session, always};
use mailcraft_model::account::CredentialKind;
use mailcraft_model::{
    Account, AccountId, AccountKind, DavSettings, FolderId, FolderRole, QuickStepAction, RuleAction,
    Security, ServerSettings,
};

/// Name of the secrets file in the profile (data) folder.
pub const SECRETS_FILE: &str = "secrets.json";
/// Largest secrets file read back.
const MAX_SECRETS_BYTES: u64 = 4 * 1024 * 1024;
/// Longest secret accepted.
const MAX_SECRET_CHARS: usize = 16 * 1024;
/// Longest host / user name accepted.
const MAX_FIELD_CHARS: usize = 1024;
/// Largest PEM text accepted for an extra root certificate.
const MAX_PEM_BYTES: usize = 256 * 1024;
/// Most certificates taken from one PEM text.
const MAX_PEM_CERTS: usize = 16;
/// Most accounts in one profile.
const MAX_ACCOUNTS: usize = 256;

/// Where account secrets are kept. Implementations never print secrets in `Debug` output.
pub trait SecretStore: Send {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&mut self, key: &str, secret: &str) -> Result<(), String>;
    fn remove(&mut self, key: &str) -> Result<(), String>;
    /// Where the secrets live ("memory", a file path, or the OS keyring), for Account Settings.
    fn location(&self) -> String;
    /// Keys this store knows (for moving secrets to another store). Stores that cannot list return what they
    /// have seen.
    fn keys(&self) -> Vec<String> {
        Vec::new()
    }
    /// True when the secrets are stored encrypted (the OS keyring); false for the plain file and memory.
    fn encrypted(&self) -> bool {
        false
    }
}

impl std::fmt::Debug for dyn SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretStore({})", self.location())
    }
}

/// Secrets held only for the life of the session.
#[derive(Default)]
pub struct MemorySecretStore {
    map: BTreeMap<String, String>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }
    fn set(&mut self, key: &str, secret: &str) -> Result<(), String> {
        self.map.insert(key.to_owned(), secret.to_owned());
        Ok(())
    }
    fn remove(&mut self, key: &str) -> Result<(), String> {
        self.map.remove(key);
        Ok(())
    }
    fn location(&self) -> String {
        "memory".into()
    }
    fn keys(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }
}

/// Secrets in a JSON file in the profile folder, readable and writable by the owner only (0600 on Unix).
///
/// The file is not encrypted (it is the fallback where no OS keyring exists, see [`KeyringSecretStore`]); its
/// `note` field and [`SecretStore::location`] say so.
pub struct FileSecretStore {
    path: PathBuf,
    map: BTreeMap<String, String>,
}

const SECRETS_NOTE: &str = "MailCraft account secrets. NOT ENCRYPTED: protect this file like a password list. \
     It is kept out of the mailbox data files on purpose, and only used where no OS keyring is available.";

impl FileSecretStore {
    /// Opens `dir/secrets.json` (missing = empty). A garbled file is reported, not overwritten silently.
    pub fn open(dir: &Path) -> Result<FileSecretStore, String> {
        let path = dir.join(SECRETS_FILE);
        tighten_permissions(&path);
        let map = match mailcraft_store::journal::read_capped(&path, MAX_SECRETS_BYTES) {
            Ok(None) => BTreeMap::new(),
            Ok(Some(bytes)) => {
                let v: Value = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
                v.get("secrets")
                    .and_then(Value::as_object)
                    .map(|o| {
                        o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned()))).collect()
                    })
                    .unwrap_or_default()
            }
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        Ok(FileSecretStore { path, map })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn save(&self) -> Result<(), String> {
        let secrets: Map<String, Value> =
            self.map.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
        let doc = json!({"note": SECRETS_NOTE, "encrypted": false, "secrets": secrets});
        let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
        write_private(&self.path, &bytes).map_err(|e| format!("cannot write {}: {e}", self.path.display()))
    }
}

impl SecretStore for FileSecretStore {
    fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }
    fn set(&mut self, key: &str, secret: &str) -> Result<(), String> {
        let old = self.map.insert(key.to_owned(), secret.to_owned());
        self.save().inspect_err(|_| {
            // Keep memory and disk in step when the write fails.
            match &old {
                Some(o) => self.map.insert(key.to_owned(), o.clone()),
                None => self.map.remove(key),
            };
        })
    }
    fn remove(&mut self, key: &str) -> Result<(), String> {
        if self.map.remove(key).is_some() { self.save() } else { Ok(()) }
    }
    fn location(&self) -> String {
        format!("{} (unencrypted file)", self.path.display())
    }
    fn keys(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }
}

/// Makes an existing secrets file owner-only (Unix): one copied in or restored from a backup may be readable by
/// others. Best effort; a missing file is fine.
fn tighten_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path)
            && meta.is_file()
            && meta.permissions().mode() & 0o077 != 0
            && let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        {
            log::warn!("cannot make {} private: {e}", path.display());
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Writes `data` to `path` through a temporary file created with owner-only permissions, then renames it.
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    {
        let mut f = opts.open(&tmp)?;
        // `mode` only applies to a new file: tighten a leftover one before any secret is written to it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Secret-store key of an account's incoming (`false`) or outgoing (`true`) server secret.
pub fn secret_key(account: AccountId, outgoing: bool) -> String {
    format!("account/{}/{}", account.0, if outgoing { "outgoing" } else { "incoming" })
}

/// Secret-store key of an account's OAuth refresh token.
pub fn refresh_key(account: AccountId) -> String {
    format!("account/{}/refresh", account.0)
}

/// Secret-store key of the OAuth client secret the user entered for an account (some providers issue one to
/// installed applications).
pub fn client_secret_key(account: AccountId) -> String {
    format!("account/{}/oauth-client-secret", account.0)
}

/// Secret-store key of the password for an account's contacts, tasks and calendar (DAV) server, when it has one
/// of its own (an OAuth account's mail sign-in only goes to the mail provider's servers).
pub fn dav_secret_key(account: AccountId) -> String {
    format!("account/{}/dav", account.0)
}

/// The secret for an account's server: the outgoing one falls back to the incoming one.
pub fn account_secret(s: &Session, account: AccountId, outgoing: bool) -> Option<String> {
    let own = s.secrets.get(&secret_key(account, outgoing));
    if outgoing { own.or_else(|| s.secrets.get(&secret_key(account, false))) } else { own }
}

/// DER certificates from PEM text (`-----BEGIN CERTIFICATE-----` blocks).
pub fn pem_certificates(pem: &str) -> Result<Vec<Vec<u8>>, String> {
    use base64::Engine;
    if pem.len() > MAX_PEM_BYTES {
        return Err("the certificate text is too large".into());
    }
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in pem.lines() {
        let l = line.trim();
        if l.starts_with("-----BEGIN CERTIFICATE-----") {
            cur = Some(String::new());
        } else if l.starts_with("-----END CERTIFICATE-----") {
            if let Some(b64) = cur.take() {
                let der = base64::engine::general_purpose::STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|e| format!("the certificate is not valid base64: {e}"))?;
                out.push(der);
                if out.len() >= MAX_PEM_CERTS {
                    break;
                }
            }
        } else if let Some(c) = cur.as_mut() {
            c.push_str(l);
        }
    }
    if out.is_empty() { Err("no certificate found in the PEM text".into()) } else { Ok(out) }
}

// =======================================================================================================
// OS keyring
// =======================================================================================================

/// Service name under which secrets are kept in the OS credential store.
pub const KEYRING_SERVICE: &str = "MailCraft";

/// Where a [`KeyringSecretStore`] keeps secrets: the OS credential store, or a stand-in in tests.
pub trait KeyBackend: Send {
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, secret: &str) -> Result<(), String>;
    fn remove(&self, key: &str) -> Result<(), String>;
    /// Name for Account Settings ("Secret Service keyring").
    fn name(&self) -> String;
}

#[cfg(not(target_arch = "wasm32"))]
impl KeyBackend for mailcraft_net::keychain::Keychain {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        mailcraft_net::keychain::Keychain::get(self, key)
    }
    fn set(&self, key: &str, secret: &str) -> Result<(), String> {
        mailcraft_net::keychain::Keychain::set(self, key, secret)
    }
    fn remove(&self, key: &str) -> Result<(), String> {
        mailcraft_net::keychain::Keychain::remove(self, key)
    }
    fn name(&self) -> String {
        mailcraft_net::keychain::Keychain::store_name().to_owned()
    }
}

/// Secrets in the OS credential store, one entry per key, named `<profile>/<key>` so two profiles never share
/// an entry. Reads are cached (the store may be a D-Bus round trip away).
pub struct KeyringSecretStore {
    backend: Box<dyn KeyBackend>,
    profile: String,
    cache: std::sync::Mutex<BTreeMap<String, Option<String>>>,
}

/// A short stable name for a profile folder (FNV-1a of its path).
fn profile_tag(dir: &Path) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in dir.to_string_lossy().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("profile-{h:016x}")
}

impl KeyringSecretStore {
    pub fn new(backend: Box<dyn KeyBackend>, profile_dir: &Path) -> KeyringSecretStore {
        KeyringSecretStore {
            backend,
            profile: profile_tag(profile_dir),
            cache: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    fn full(&self, key: &str) -> String {
        format!("{}/{key}", self.profile)
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Option<String>>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, key: &str) -> Option<String> {
        if let Some(v) = self.cache().get(key) {
            return v.clone();
        }
        match self.backend.get(&self.full(key)) {
            Ok(v) => {
                self.cache().insert(key.to_owned(), v.clone());
                v
            }
            Err(e) => {
                log::warn!("keyring: cannot read a secret: {e}");
                None
            }
        }
    }
    fn set(&mut self, key: &str, secret: &str) -> Result<(), String> {
        self.backend.set(&self.full(key), secret)?;
        self.cache().insert(key.to_owned(), Some(secret.to_owned()));
        Ok(())
    }
    fn remove(&mut self, key: &str) -> Result<(), String> {
        self.backend.remove(&self.full(key))?;
        self.cache().insert(key.to_owned(), None);
        Ok(())
    }
    fn location(&self) -> String {
        format!("{} (encrypted by the operating system)", self.backend.name())
    }
    fn keys(&self) -> Vec<String> {
        self.cache().iter().filter(|(_, v)| v.is_some()).map(|(k, _)| k.clone()).collect()
    }
    fn encrypted(&self) -> bool {
        true
    }
}

/// Every secret key an account can have.
fn account_secret_keys(id: AccountId) -> [String; 5] {
    [secret_key(id, false), secret_key(id, true), refresh_key(id), client_secret_key(id), dav_secret_key(id)]
}

impl Session {
    /// Moves account secrets into the OS credential store and keeps them there from now on. Needs a data
    /// folder (secrets of an in-memory session stay in memory). On any failure (no keyring daemon, no D-Bus
    /// session, locked store) nothing changes and the reason is returned: the unencrypted file stays in use.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn use_os_keyring(&mut self) -> Result<String, String> {
        if self.store.is_none() {
            return Err("an in-memory profile keeps its secrets in memory".into());
        }
        if self.secrets.encrypted() {
            return Ok(self.secrets.location());
        }
        let kc = mailcraft_net::keychain::Keychain::open(KEYRING_SERVICE)?;
        self.move_secrets_to(Box::new(kc))
    }

    /// [`Session::use_os_keyring`] with any backend (tests use a stand-in). Copies every known secret, then
    /// removes it from the old store; the old store keeps everything when a copy fails.
    pub fn move_secrets_to(&mut self, backend: Box<dyn KeyBackend>) -> Result<String, String> {
        let Some(dir) = self.store.as_ref().map(|st| st.dir.clone()) else {
            return Err("an in-memory profile keeps its secrets in memory".into());
        };
        if self.secrets.encrypted() {
            return Ok(self.secrets.location());
        }
        let mut ks = KeyringSecretStore::new(backend, &dir);
        let mut keys = self.secrets.keys();
        for a in &self.mailbox.accounts {
            keys.extend(account_secret_keys(a.id));
        }
        // The translation server's API key (Options › Language).
        keys.push(crate::translate::KEY_SECRET.to_owned());
        keys.sort();
        keys.dedup();
        let mut moved = Vec::new();
        for k in &keys {
            if let Some(v) = self.secrets.get(k) {
                ks.set(k, &v).map_err(|e| format!("cannot store a secret in the keyring: {e}"))?;
                moved.push(k.clone());
            }
        }
        for k in &moved {
            if let Err(e) = self.secrets.remove(k) {
                log::warn!("could not remove a moved secret from {}: {e}", self.secrets.location());
            }
        }
        self.secrets = Box::new(ks);
        Ok(self.secrets.location())
    }
}

// =======================================================================================================
// Account options and sync bookkeeping (sidecar file)
// =======================================================================================================

/// File next to the data file holding [`AccountPrefs`].
pub const ACCOUNT_OPTIONS_FILE: &str = "account-options.json";
/// Largest options file read back.
const MAX_OPTIONS_BYTES: u64 = 64 * 1024 * 1024;
/// Longest interval accepted (a day).
const MAX_INTERVAL_MINUTES: u64 = 1440;
/// Longest "remove from server after" period accepted.
const MAX_KEEP_DAYS: u64 = 3650;
/// Longest "download mail from the past N months" period accepted (100 years; `null` = all mail).
const MAX_DOWNLOAD_MONTHS: u64 = 1200;
/// "Download mail from the past N months" for new accounts.
pub const DEFAULT_DOWNLOAD_MONTHS: u32 = 12;

/// Send / Receive › Download Preferences.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadMode {
    /// Download Full Items.
    #[default]
    Full,
    /// Download Headers and Then Full Items.
    HeadersThenFull,
    /// Download Headers.
    Headers,
}

impl DownloadMode {
    pub fn name(self) -> &'static str {
        match self {
            DownloadMode::Full => "full",
            DownloadMode::HeadersThenFull => "headersThenFull",
            DownloadMode::Headers => "headers",
        }
    }
    pub fn parse(s: &str) -> Option<DownloadMode> {
        match s.trim() {
            "full" => Some(DownloadMode::Full),
            "headersThenFull" => Some(DownloadMode::HeadersThenFull),
            "headers" => Some(DownloadMode::Headers),
            _ => None,
        }
    }
    /// New messages arrive as headers first.
    pub fn headers_first(self) -> bool {
        self != DownloadMode::Full
    }
}

/// The OAuth registration an account signs in with (public data; the client secret, if any, is in the secret
/// store).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OAuthBinding {
    pub provider: String,
    pub name: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub device_authorization_endpoint: Option<String>,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub extra_auth_params: Vec<(String, String)>,
    /// Unix seconds when the access token expires (unknown = `None`).
    pub expires_at: Option<u64>,
}

/// Options of one account that the mailbox model has no field for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AccountOptions {
    /// The address when last saved (diagnostics).
    pub address: String,
    /// Incoming mail over POP3 (the account's kind is `Imap` until the model has a POP3 kind).
    pub pop3: bool,
    pub download: DownloadMode,
    /// On slow connections download only headers (Download Preferences): while the last measured transfer
    /// rate is below the slow-connection threshold, new mail arrives as headers (see `sync::slow_connection`).
    pub slow_headers_only: bool,
    /// The first download of a folder takes mail from the past N months only (`None` = all mail). Later
    /// send/receives download every new message.
    pub download_months: Option<u32>,
    /// Member of the "All Accounts" Send/Receive group (F9 and scheduled runs).
    pub include_in_send_receive: bool,
    /// Scheduled send/receive every N minutes (`None` = Options › Advanced; 0 = never).
    pub interval_minutes: Option<u32>,
    /// New mail is pushed (IMAP IDLE) instead of waiting for the next scheduled run.
    pub push: bool,
    /// Inbox check interval when push is not available.
    pub poll_minutes: u32,
    /// POP3: leave a copy of messages on the server.
    pub leave_on_server: bool,
    /// POP3: remove server copies after this many days (with `leave_on_server`).
    pub remove_after_days: Option<u32>,
    /// POP3: remove the server copy when the message is deleted here for good.
    pub remove_when_deleted: bool,
    pub oauth: Option<OAuthBinding>,
}

impl Default for AccountOptions {
    fn default() -> Self {
        AccountOptions {
            address: String::new(),
            pop3: false,
            download: DownloadMode::Full,
            slow_headers_only: true,
            download_months: Some(DEFAULT_DOWNLOAD_MONTHS),
            include_in_send_receive: true,
            interval_minutes: None,
            push: true,
            poll_minutes: 5,
            leave_on_server: true,
            remove_after_days: Some(14),
            remove_when_deleted: false,
            oauth: None,
        }
    }
}

/// POP3 bookkeeping of one account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Pop3State {
    /// Unique ids (UIDL) already downloaded, with the Unix time first seen.
    pub seen: BTreeMap<String, i64>,
    /// Unique ids to delete from the server on the next run.
    pub delete: BTreeSet<String>,
}

/// What the client last knew of a JMAP email (to find local changes).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct JmapShadow {
    pub mailbox: String,
    pub keywords: Vec<String>,
}

/// JMAP bookkeeping of one account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct JmapState {
    pub email_state: String,
    pub mailbox_state: String,
    pub shadow: BTreeMap<String, JmapShadow>,
}

/// Everything in [`ACCOUNT_OPTIONS_FILE`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AccountPrefs {
    pub note: String,
    /// Options for accounts without their own entry (and new accounts).
    pub defaults: AccountOptions,
    pub accounts: BTreeMap<u32, AccountOptions>,
    /// Items downloaded as headers only.
    pub headers_only: BTreeSet<u64>,
    /// Items marked to download (`true` = Mark to Download Message Copy: the server copy stays).
    pub marked: BTreeMap<u64, bool>,
    pub pop3: BTreeMap<u32, Pop3State>,
    pub jmap: BTreeMap<u32, JmapState>,
    /// User-defined Send/Receive groups (`sync/groups.rs`).
    pub groups: Vec<crate::sync::groups::SendReceiveGroup>,
}

const OPTIONS_NOTE: &str = "MailCraft account options and send/receive bookkeeping. No secrets.";

/// Loaded [`AccountPrefs`] of a session (read lazily from the data folder).
#[derive(Debug, Default)]
pub struct PrefsSlot {
    loaded: bool,
    path: Option<PathBuf>,
    pub data: AccountPrefs,
    save_error_shown: bool,
}

/// The session's account options, reading them from the data folder the first time.
pub fn prefs(s: &mut Session) -> &mut AccountPrefs {
    load_prefs(s);
    &mut s.sync.prefs.data
}

/// Reads the options file once (after the store is attached). A garbled file is reported and replaced by
/// defaults on the next save.
pub(crate) fn load_prefs(s: &mut Session) {
    let dir = s.store.as_ref().map(|st| st.dir.clone());
    let slot = &mut s.sync.prefs;
    if slot.loaded && (slot.path.is_some() || dir.is_none()) {
        return;
    }
    slot.loaded = true;
    let Some(dir) = dir else { return };
    let path = dir.join(ACCOUNT_OPTIONS_FILE);
    match mailcraft_store::journal::read_capped(&path, MAX_OPTIONS_BYTES) {
        Ok(None) => {}
        Ok(Some(bytes)) => match serde_json::from_slice::<AccountPrefs>(&bytes) {
            Ok(p) => {
                // In-memory changes made before the store was attached win over the file.
                if slot.data == AccountPrefs::default() {
                    slot.data = p;
                }
            }
            Err(e) => log::error!("{} is not valid ({e}); using defaults", path.display()),
        },
        Err(e) => log::error!("cannot read {}: {e}", path.display()),
    }
    slot.path = Some(path);
    let ids: BTreeSet<u32> = s.mailbox.accounts.iter().map(|a| a.id.0).collect();
    let d = &mut s.sync.prefs.data;
    d.accounts.retain(|k, _| ids.contains(k));
    d.pop3.retain(|k, _| ids.contains(k));
    d.jmap.retain(|k, _| ids.contains(k));
    d.groups.truncate(crate::sync::groups::MAX_GROUPS);
    for g in &mut d.groups {
        g.members.retain(|m| ids.contains(&m.account));
    }
}

/// Writes the options file (when the session has a data folder). Failures are logged and shown once.
pub(crate) fn save_prefs(s: &mut Session) {
    load_prefs(s);
    let Some(path) = s.sync.prefs.path.clone() else { return };
    let mut data = s.sync.prefs.data.clone();
    data.note = OPTIONS_NOTE.into();
    let res = serde_json::to_vec_pretty(&data)
        .map_err(|e| e.to_string())
        .and_then(|b| write_private(&path, &b).map_err(|e| e.to_string()));
    match res {
        Ok(()) => s.sync.prefs.save_error_shown = false,
        Err(e) => {
            log::error!("cannot write {}: {e}", path.display());
            if !s.sync.prefs.save_error_shown {
                s.sync.prefs.save_error_shown = true;
                s.notify(Notice::Error(format!("Account options could not be saved ({e}).")));
            }
        }
    }
}

/// An account's options (the defaults when it has none of its own).
pub fn account_options(s: &Session, id: AccountId) -> AccountOptions {
    let d = &s.sync.prefs.data;
    d.accounts.get(&id.0).cloned().unwrap_or_else(|| d.defaults.clone())
}

/// Changes an account's options and saves them.
pub(crate) fn update_options(s: &mut Session, id: AccountId, f: impl FnOnce(&mut AccountOptions)) {
    let address = s.mailbox.account(id).map(|a| a.address.clone()).unwrap_or_default();
    let p = prefs(s);
    let defaults = p.defaults.clone();
    let o = p.accounts.entry(id.0).or_insert(defaults);
    o.address = address;
    f(o);
    save_prefs(s);
}

/// True when the account fetches its mail over POP3.
pub fn is_pop3(s: &Session, id: AccountId) -> bool {
    s.sync.prefs.data.accounts.get(&id.0).is_some_and(|o| o.pop3)
}

/// The account kind as shown and accepted by the commands ("pop3" for POP3 accounts).
pub fn kind_label(s: &Session, a: &Account) -> &'static str {
    if a.kind == AccountKind::Imap && is_pop3(s, a.id) { "pop3" } else { kind_name(a.kind) }
}

/// Drops the options and bookkeeping of a removed account.
pub(crate) fn forget_options(s: &mut Session, id: AccountId) {
    let p = prefs(s);
    let mut had =
        p.accounts.remove(&id.0).is_some() | p.pop3.remove(&id.0).is_some() | p.jmap.remove(&id.0).is_some();
    for g in &mut p.groups {
        let n = g.members.len();
        g.members.retain(|m| m.account != id.0);
        had |= g.members.len() != n;
    }
    if had {
        save_prefs(s);
    }
}

fn options_json(o: &AccountOptions) -> Value {
    json!({
        "pop3": o.pop3,
        "download": o.download.name(),
        "slowHeadersOnly": o.slow_headers_only,
        "downloadMonths": o.download_months,
        "include": o.include_in_send_receive,
        "intervalMinutes": o.interval_minutes,
        "push": o.push,
        "pollMinutes": o.poll_minutes,
        "leaveOnServer": o.leave_on_server,
        "removeAfterDays": o.remove_after_days,
        "removeWhenDeleted": o.remove_when_deleted,
        "oauth": o.oauth.as_ref().map(|b| json!({
            "provider": b.provider,
            "name": b.name,
            "clientId": b.client_id,
            "expiresAt": b.expires_at,
        })),
    })
}

fn opt_minutes(p: &Value, key: &str, max: u64) -> Result<Option<Option<u32>>, CmdError> {
    match p.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(v) => v
            .as_u64()
            .filter(|n| *n <= max)
            .and_then(|n| u32::try_from(n).ok())
            .map(|n| Some(Some(n)))
            .ok_or_else(|| CmdError::Params(format!("`{key}` must be a number from 0 to {max}"))),
    }
}

/// Applies option params (`download`, `slowHeadersOnly`, `downloadMonths`, `include`, `intervalMinutes`,
/// `push`, `pollMinutes`, `leaveOnServer`, `removeAfterDays`, `removeWhenDeleted`) to `o`. Validates
/// everything before changing anything.
pub(crate) fn apply_option_params(o: &mut AccountOptions, p: &Value) -> Result<bool, CmdError> {
    let download = match p.get("download").or_else(|| p.get("mode")) {
        None | Some(Value::Null) => None,
        Some(Value::String(m)) => Some(DownloadMode::parse(m).ok_or_else(|| {
            CmdError::Params(format!("unknown mode `{m}` (full, headersThenFull, headers)"))
        })?),
        Some(_) => return Err(CmdError::Params("`mode` must be a string".into())),
    };
    let interval = opt_minutes(p, "intervalMinutes", MAX_INTERVAL_MINUTES)?;
    let poll = opt_minutes(p, "pollMinutes", MAX_INTERVAL_MINUTES)?;
    if poll == Some(None) || poll == Some(Some(0)) {
        return Err(CmdError::Params("`pollMinutes` must be from 1 to 1440".into()));
    }
    let keep = opt_minutes(p, "removeAfterDays", MAX_KEEP_DAYS)?;
    let months = opt_minutes(p, "downloadMonths", MAX_DOWNLOAD_MONTHS)?;
    let bools = ["slowHeadersOnly", "include", "push", "leaveOnServer", "removeWhenDeleted"];
    for b in bools {
        if p.get(b).is_some_and(|v| !v.is_boolean() && !v.is_null()) {
            return Err(CmdError::Params(format!("`{b}` must be true or false")));
        }
    }
    let before = o.clone();
    if let Some(d) = download {
        o.download = d;
    }
    if let Some(i) = interval {
        o.interval_minutes = i;
    }
    if let Some(Some(n)) = poll {
        o.poll_minutes = n;
    }
    if let Some(k) = keep {
        o.remove_after_days = k.filter(|d| *d > 0);
    }
    if let Some(m) = months {
        o.download_months = m.filter(|m| *m > 0);
    }
    if let Some(b) = opt_bool(p, "slowHeadersOnly") {
        o.slow_headers_only = b;
    }
    if let Some(b) = opt_bool(p, "include") {
        o.include_in_send_receive = b;
    }
    if let Some(b) = opt_bool(p, "push") {
        o.push = b;
    }
    if let Some(b) = opt_bool(p, "leaveOnServer") {
        o.leave_on_server = b;
    }
    if let Some(b) = opt_bool(p, "removeWhenDeleted") {
        o.remove_when_deleted = b;
    }
    Ok(*o != before)
}

// =======================================================================================================
// Commands
// =======================================================================================================

fn spec(
    id: &'static str,
    label: &'static str,
    params: &'static str,
    mutates: bool,
    enabled: fn(&Session) -> Option<&'static str>,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec {
        id,
        label,
        location: "File › Info › Account Settings",
        shortcut: "",
        params,
        mutates,
        enabled,
        run,
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec("accounts.list", "Account Settings...", "", false, always, list),
        spec(
            "accounts.add",
            "Add Account",
            r#"{"kind": "imap"|"jmap", "address": string, "displayName"?: string, "senderName"?: string, "incoming": server, "outgoing"?: server, "password"?: string, "outgoingPassword"?: string, "oauthToken"?: string, "default"?: bool, "serverSavesSent"?: bool, "dav"?: {"url": "https://… (server, principal or collection)", "contacts"?: bool, "tasks"?: bool, "calendar"?: bool (events, CalDAV VEVENT), "publishUrl"?: "https://… (WebDAV collection for published calendars)", "username"?: string, "password"?: string (a DAV password of its own; empty removes it), "extraRootPem"?: string, "allowPlaintextAuth"?: bool}|null} where server = {"host", "port"?, "security"?, "username"?, "auth"?, "extraRootPem"?, "allowPlaintextAuth"?} (JMAP: host = session URL)"#,
            true,
            always,
            add,
        ),
        spec(
            "accounts.edit",
            "Change Account",
            r#"{"account": number|string, "displayName"?, "senderName"?, "address"?, "incoming"?: server, "outgoing"?: server|null, "password"?, "outgoingPassword"?, "oauthToken"?, "serverSavesSent"?: bool, "dav"?: {"url": "https://… (server, principal or collection)", "contacts"?: bool, "tasks"?: bool, "calendar"?: bool (events, CalDAV VEVENT), "publishUrl"?: "https://… (WebDAV collection for published calendars)", "username"?: string, "password"?: string (a DAV password of its own; empty removes it), "extraRootPem"?: string, "allowPlaintextAuth"?: bool}|null}"#,
            true,
            has_server_account,
            edit,
        ),
        spec(
            "accounts.remove",
            "Remove Account",
            r#"{"account": number|string, "confirm"?: bool (without it the command only asks)}"#,
            true,
            has_server_account,
            remove,
        ),
        spec(
            "accounts.setDefault",
            "Set as Default",
            r#"{"account": number|string}"#,
            true,
            has_accounts,
            set_default,
        ),
        spec(
            "accounts.test",
            "Test Account Settings...",
            r#"{"account"?: number|string, "wait"?: bool (block until done; default false = report through the status)}"#,
            false,
            has_server_account,
            test,
        ),
        spec(
            "accounts.options",
            "More Settings",
            r#"{"account"?: number|string, "mode"?: "full"|"headersThenFull"|"headers", "slowHeadersOnly"?: bool, "downloadMonths"?: number|null (first download: mail from the past N months; null or 0 = all), "include"?: bool (in the All Accounts Send/Receive group), "intervalMinutes"?: number|null (null = Options › Advanced), "push"?: bool, "pollMinutes"?: number, "leaveOnServer"?: bool, "removeAfterDays"?: number|null, "removeWhenDeleted"?: bool} (no option given: returns the options)"#,
            false,
            has_server_account,
            options_cmd,
        ),
        spec(
            "accounts.useKeyring",
            "Store passwords in the system keyring",
            r#"{"confirm": true (required: the secrets move out of the profile folder)}"#,
            false,
            always,
            use_keyring,
        ),
        spec(
            "accounts.discover",
            "Find server settings",
            r#"{"address": string, "wait"?: bool (block until done), "refresh"?: bool (look up again)} → {"status": "running"|"done", "found", "sources", "imap"?, "pop3"?, "smtp"?, "jmap"?, "notes"} (autoconfig on the mail domain, then DNS SRV records, RFC 6186)"#,
            false,
            always,
            discover,
        ),
        spec(
            "accounts.sharedMailboxes",
            "Shared mailboxes",
            r#"{"account"?: number|string} → {"mailboxes": [{"name", "root", "kind": "otherUser"|"shared", "open", "store"?}]} (other users' and shared IMAP mailboxes the login can open, as listed by the last full send/receive)"#,
            false,
            has_server_account,
            shared_mailboxes,
        ),
        spec(
            "accounts.openSharedMailbox",
            "Open shared mailbox",
            r#"{"account"?: number|string (the IMAP account), "mailbox": string (name or server root), "open"?: bool (false closes it; default true)} → {"store", "open"}"#,
            true,
            has_server_account,
            open_shared_mailbox,
        ),
        spec("accounts.oauthProviders", "Sign in with OAuth (providers)", "", false, always, oauth_providers),
        spec(
            "accounts.oauthSignIn",
            "Sign in with OAuth",
            r#"{"account"?: number|string, "provider": string (id from accounts.oauthProviders, or "custom"), "clientId": string, "clientSecret"?: string, "flow"?: "device"|"browser", "authorizationEndpoint"?, "tokenEndpoint"?, "deviceAuthorizationEndpoint"?, "scopes"?: [string] (custom provider)}"#,
            false,
            has_server_account,
            oauth_sign_in,
        ),
        spec(
            "accounts.oauthStatus",
            "Sign in with OAuth (status)",
            r#"{"account"?: number|string}"#,
            false,
            always,
            oauth_status,
        ),
        spec(
            "accounts.oauthCancel",
            "Cancel OAuth sign-in",
            r#"{"account"?: number|string}"#,
            false,
            always,
            oauth_cancel,
        ),
    ]
}

fn has_accounts(s: &Session) -> Option<&'static str> {
    if s.mailbox.accounts.is_empty() { Some("no accounts") } else { None }
}

fn has_server_account(s: &Session) -> Option<&'static str> {
    if s.mailbox.accounts.iter().any(|a| a.kind != AccountKind::Local) {
        None
    } else {
        Some("no email account is set up")
    }
}

/// Account named by `p[key]` (id, numeric string, address or display name); the current account when absent.
pub(crate) fn find_account(s: &Session, p: &Value, key: &str) -> Result<AccountId, CmdError> {
    let accounts = &s.mailbox.accounts;
    let found = match p.get(key) {
        None | Some(Value::Null) => s.current_account(),
        Some(Value::Number(n)) => {
            n.as_u64().and_then(|n| accounts.iter().find(|a| u64::from(a.id.0) == n)).map(|a| a.id)
        }
        Some(Value::String(t)) => {
            let t = t.trim();
            accounts
                .iter()
                .find(|a| t.parse::<u64>().is_ok_and(|n| u64::from(a.id.0) == n))
                .or_else(|| accounts.iter().find(|a| !t.is_empty() && a.address.eq_ignore_ascii_case(t)))
                .or_else(|| accounts.iter().find(|a| !t.is_empty() && a.display_name.eq_ignore_ascii_case(t)))
                .map(|a| a.id)
        }
        Some(_) => None,
    };
    found.ok_or_else(|| CmdError::Params(format!("no such account {}", p.get(key).unwrap_or(&Value::Null))))
}

pub(crate) fn kind_name(k: AccountKind) -> &'static str {
    match k {
        AccountKind::Local => "local",
        AccountKind::Imap => "imap",
        AccountKind::Jmap => "jmap",
        AccountKind::Exchange => "exchange",
    }
}

fn security_name(s: Security) -> &'static str {
    match s {
        Security::None => "none",
        Security::StartTls => "starttls",
        Security::Tls => "tls",
    }
}

fn server_json(srv: &ServerSettings) -> Value {
    json!({
        "host": srv.host,
        "port": srv.port,
        "security": security_name(srv.security),
        "username": srv.username,
        "auth": srv.auth,
        "extraRootCertificate": srv.extra_root_pem.is_some(),
        "allowPlaintextAuth": srv.allow_plaintext_auth,
    })
}

/// Account settings for the UI and agents. Never includes secrets, only whether one is stored.
pub(crate) fn account_settings_json(s: &Session, a: &Account) -> Value {
    json!({
        "id": a.id.0,
        "kind": kind_label(s, a),
        "options": options_json(&account_options(s, a.id)),
        "displayName": a.display_name,
        "address": a.address,
        "senderName": a.sender_name,
        "default": a.is_default,
        "canSend": a.can_send(),
        "incoming": a.incoming.as_ref().map(server_json),
        "outgoing": a.outgoing.as_ref().map(server_json),
        "credential": match a.credential { CredentialKind::Password => "password", CredentialKind::OAuthToken => "oauthToken" },
        "hasSecret": s.secrets.get(&secret_key(a.id, false)).is_some(),
        "serverSavesSent": a.server_saves_sent,
        "pendingChanges": a.sync.pending.len(),
        "dav": a.dav.as_ref().map(dav_json),
        "hasDavPassword": s.secrets.get(&dav_secret_key(a.id)).is_some(),
        "sharedMailbox": a.shared.as_ref().map(|sh| json!({
            "owner": sh.owner.0,
            "root": sh.root,
            "kind": shared_kind_name(sh.kind),
        })),
        "sharedMailboxesOffered": a.sync.shared_roots.len(),
    })
}

fn shared_kind_name(k: mailcraft_model::account::SharedKind) -> &'static str {
    match k {
        mailcraft_model::account::SharedKind::OtherUser => "otherUser",
        mailcraft_model::account::SharedKind::Shared => "shared",
    }
}

fn dav_json(d: &DavSettings) -> Value {
    json!({
        "url": d.url,
        "contacts": d.contacts,
        "tasks": d.tasks,
        "calendar": d.calendar,
        "publishUrl": d.publish_url,
        "username": d.username,
        "extraRootCertificate": d.extra_root_pem.is_some(),
        "allowPlaintextAuth": d.allow_plaintext_auth,
    })
}

/// A web address for DAV settings: empty, or http(s) without spaces.
fn dav_url(what: &str, v: &str) -> Result<String, CmdError> {
    let v = clean_line(what, v)?;
    if v.is_empty() {
        return Ok(v);
    }
    let lower = v.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) || v.contains(char::is_whitespace) {
        return Err(CmdError::Params(format!("{what} must be an http(s) address, not `{v}`")));
    }
    Ok(v)
}

/// DAV settings from `p.dav` (`old` supplies values not given). `Some(None)`: turned off; `None`: not given.
fn parse_dav(p: &Value, old: Option<&DavSettings>) -> Result<Option<Option<DavSettings>>, CmdError> {
    let Some(v) = p.get("dav") else { return Ok(None) };
    if v.is_null() {
        return Ok(Some(None));
    }
    if !v.is_object() {
        return Err(CmdError::Params("`dav` must be an object or null".into()));
    }
    let mut d = old.cloned().unwrap_or(DavSettings {
        contacts: true,
        tasks: true,
        calendar: true,
        ..Default::default()
    });
    if let Some(u) = str_field(v, &["url"])? {
        d.url = dav_url("the contacts and tasks server", u)?;
    }
    if let Some(u) = str_field(v, &["publishUrl", "publish_url"])? {
        d.publish_url = dav_url("the publishing location", u)?;
    }
    if let Some(u) = str_field(v, &["username", "user"])? {
        d.username = clean_line("the user name", u)?;
    }
    if let Some(b) = opt_bool(v, "contacts") {
        d.contacts = b;
    }
    if let Some(b) = opt_bool(v, "tasks") {
        d.tasks = b;
    }
    if let Some(b) = opt_bool(v, "calendar") {
        d.calendar = b;
    }
    // The DAV servers' own connection settings (the incoming mail server's are not used for them).
    match field(v, &["extraRootPem", "extra_root_pem"]) {
        Some(Value::String(pem)) if pem.trim().is_empty() => d.extra_root_pem = None,
        Some(Value::String(pem)) => {
            pem_certificates(pem).map_err(CmdError::Params)?;
            d.extra_root_pem = Some(pem.clone());
        }
        Some(Value::Null) => d.extra_root_pem = None,
        Some(_) => return Err(CmdError::Params("`dav.extraRootPem` must be a string".into())),
        None => {}
    }
    if let Some(b) = opt_bool(v, "allowPlaintextAuth").or_else(|| opt_bool(v, "allow_plaintext_auth")) {
        d.allow_plaintext_auth = b;
    }
    if d.url.is_empty() && d.publish_url.is_empty() {
        return Ok(Some(None));
    }
    Ok(Some(Some(d)))
}

/// The DAV password from `p.dav.password`: `Some("")` removes a stored one, `None` leaves it as it is.
fn parse_dav_password(p: &Value) -> Result<Option<String>, CmdError> {
    let Some(v) = p.get("dav").filter(|v| v.is_object()) else { return Ok(None) };
    let Some(pw) = str_field(v, &["password"])? else { return Ok(None) };
    if pw.chars().count() > MAX_SECRET_CHARS || pw.contains(['\r', '\n', '\0']) {
        return Err(CmdError::Params("the DAV password is not valid".into()));
    }
    Ok(Some(pw.to_owned()))
}

/// Stores (or with an empty one, removes) an account's DAV password.
fn store_dav_password(s: &mut Session, id: AccountId, pw: Option<String>) -> Result<(), CmdError> {
    match pw {
        Some(pw) if pw.is_empty() => {
            if s.secrets.get(&dav_secret_key(id)).is_some() {
                s.secrets.remove(&dav_secret_key(id)).map_err(CmdError::Failed)?;
            }
        }
        Some(pw) => s.secrets.set(&dav_secret_key(id), &pw).map_err(CmdError::Failed)?,
        None => {}
    }
    Ok(())
}

/// Forgets what was synchronised with an account's contacts and tasks server (its address changed or it was
/// turned off): the items stay, as local items.
fn forget_dav(s: &mut Session, id: AccountId) {
    let folders: Vec<FolderId> = match s.mailbox.account(id) {
        Some(a) => a.sync.dav.keys().copied().collect(),
        None => return,
    };
    let own: Vec<FolderId> = s.mailbox.folders.values().filter(|f| f.account == id).map(|f| f.id).collect();
    for c in s.mailbox.contacts.values_mut().filter(|c| own.contains(&c.folder)) {
        c.remote = None;
    }
    for t in s.mailbox.tasks.values_mut().filter(|t| own.contains(&t.folder)) {
        t.remote = None;
    }
    for a in s.mailbox.appointments.values_mut().filter(|a| own.contains(&a.folder)) {
        a.remote = None;
    }
    if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == id) {
        a.sync.dav.clear();
    }
    let _ = folders;
}

fn shared_mailboxes(s: &mut Session, p: &Value) -> CmdResult {
    let id = find_account(s, p, "account")?;
    let Some(a) = s.mailbox.account(id) else { return Err(CmdError::Params("no such account".into())) };
    let stores = crate::sync::shared::stores_of(s, id);
    let list: Vec<Value> = a
        .sync
        .shared_roots
        .iter()
        .map(|r| {
            let store = stores.iter().find(|x| x.shared.as_ref().is_some_and(|sh| sh.root == r.root));
            json!({
                "name": r.name,
                "root": r.root,
                "kind": shared_kind_name(r.kind),
                "open": store.is_some(),
                "store": store.map(|x| x.id.0),
            })
        })
        .collect();
    Ok(json!({ "account": id.0, "mailboxes": list }))
}

fn open_shared_mailbox(s: &mut Session, p: &Value) -> CmdResult {
    let id = find_account(s, p, "account")?;
    let name = str_field(p, &["mailbox", "name"])?
        .map(str::to_owned)
        .ok_or_else(|| CmdError::Params("`mailbox` (name) required".into()))?;
    if opt_bool(p, "open") == Some(false) {
        let store = crate::sync::shared::stores_of(s, id)
            .into_iter()
            .find(|a| {
                a.display_name.eq_ignore_ascii_case(name.trim())
                    || a.shared.as_ref().is_some_and(|sh| sh.root == name.trim())
            })
            .map(|a| a.id)
            .ok_or_else(|| CmdError::Params(format!("\"{}\" is not open", name.trim())))?;
        crate::sync::shared::close(s, store)?;
        s.refresh_me();
        return Ok(json!({ "store": store.0, "open": false }));
    }
    let store = crate::sync::shared::open(s, id, &name)?;
    // Download it now (it opens empty).
    let started =
        if s.view.work_offline { 0 } else { s.send_receive(&[store], crate::sync::Plan::all()).unwrap_or(0) };
    s.refresh_me();
    Ok(json!({ "store": store.0, "open": true, "started": started }))
}

fn list(s: &mut Session, _: &Value) -> CmdResult {
    load_prefs(s);
    let accounts: Vec<Value> = s.mailbox.accounts.iter().map(|a| account_settings_json(s, a)).collect();
    Ok(
        json!({"accounts": accounts, "secrets": s.secrets.location(), "secretsEncrypted": s.secrets.encrypted()}),
    )
}

/// First present key among `keys` (camelCase and snake_case spellings).
fn field<'a>(p: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| p.get(*k)).filter(|v| !v.is_null())
}

fn str_field<'a>(p: &'a Value, keys: &[&str]) -> Result<Option<&'a str>, CmdError> {
    match field(p, keys) {
        None => Ok(None),
        Some(Value::String(t)) => Ok(Some(t.as_str())),
        Some(_) => Err(CmdError::Params(format!("`{}` must be a string", keys.first().unwrap_or(&"")))),
    }
}

/// A trimmed single-line value without control characters, at most `MAX_FIELD_CHARS` long.
fn clean_line(what: &str, v: &str) -> Result<String, CmdError> {
    let t = v.trim();
    if t.chars().any(char::is_control) {
        return Err(CmdError::Params(format!("{what} contains control characters")));
    }
    if t.chars().count() > MAX_FIELD_CHARS {
        return Err(CmdError::Params(format!("{what} is too long")));
    }
    Ok(t.to_owned())
}

fn valid_address(addr: &str) -> bool {
    let Some((local, domain)) = addr.rsplit_once('@') else { return false };
    !local.is_empty()
        && !domain.is_empty()
        && !addr.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';'))
}

fn parse_security(v: Option<&Value>, default: Security) -> Result<Security, CmdError> {
    match v {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(t)) => match t.trim().to_ascii_lowercase().as_str() {
            "none" | "plain" | "off" => Ok(Security::None),
            "starttls" | "start_tls" => Ok(Security::StartTls),
            "tls" | "ssl" | "ssl/tls" | "implicit" => Ok(Security::Tls),
            other => Err(CmdError::Params(format!("unknown security `{other}` (none, starttls or tls)"))),
        },
        Some(_) => Err(CmdError::Params("`security` must be a string".into())),
    }
}

/// Server settings from params; `smtp` picks the default ports (`pop3` the POP3 ones). `old` supplies values
/// not given (edit).
fn parse_server(
    v: &Value,
    kind: AccountKind,
    smtp: bool,
    pop3: bool,
    address: &str,
    old: Option<&ServerSettings>,
) -> Result<ServerSettings, CmdError> {
    if !v.is_object() {
        return Err(CmdError::Params("server settings must be an object".into()));
    }
    let host = match (str_field(v, &["host", "url"])?, old) {
        (Some(h), _) => clean_line("the server name", h)?,
        (None, Some(o)) => o.host.clone(),
        (None, None) => String::new(),
    };
    if host.is_empty() {
        return Err(CmdError::Params("`host` (server name) required".into()));
    }
    if kind == AccountKind::Imap && host.contains(char::is_whitespace) {
        return Err(CmdError::Params("the server name cannot contain spaces".into()));
    }
    let security = parse_security(v.get("security"), old.map_or(Security::Tls, |o| o.security))?;
    let default_port = match (smtp, security) {
        (false, Security::Tls) if pop3 => 995,
        (false, _) if pop3 => 110,
        (false, Security::Tls) => 993,
        (false, _) => 143,
        (true, Security::Tls) => 465,
        (true, _) => 587,
    };
    let port = match v.get("port") {
        None | Some(Value::Null) => {
            old.filter(|o| o.security == security && o.port != 0).map_or(default_port, |o| o.port)
        }
        Some(p) => p
            .as_u64()
            .filter(|p| (1..=65_535).contains(p))
            .and_then(|p| u16::try_from(p).ok())
            .ok_or_else(|| CmdError::Params("`port` must be a number from 1 to 65535".into()))?,
    };
    let username = match (str_field(v, &["username", "user"])?, old) {
        (Some(u), _) => clean_line("the user name", u)?,
        (None, Some(o)) => o.username.clone(),
        (None, None) => String::new(),
    };
    let username = if username.is_empty() { address.to_owned() } else { username };
    let auth = match (str_field(v, &["auth"])?, old) {
        (Some(a), _) => clean_line("the sign-in method", a)?.to_ascii_uppercase(),
        (None, Some(o)) => o.auth.clone(),
        (None, None) => String::new(),
    };
    let extra_root_pem = match field(v, &["extraRootPem", "extra_root_pem"]) {
        Some(Value::String(pem)) if pem.trim().is_empty() => None,
        Some(Value::String(pem)) => {
            pem_certificates(pem).map_err(CmdError::Params)?;
            Some(pem.clone())
        }
        Some(_) => return Err(CmdError::Params("`extraRootPem` must be a string".into())),
        None => old.and_then(|o| o.extra_root_pem.clone()),
    };
    let allow_plaintext_auth = opt_bool(v, "allowPlaintextAuth")
        .or_else(|| opt_bool(v, "allow_plaintext_auth"))
        .unwrap_or_else(|| old.is_some_and(|o| o.allow_plaintext_auth));
    Ok(ServerSettings { host, port, security, username, auth, extra_root_pem, allow_plaintext_auth })
}

/// The account kind and whether incoming mail is POP3 (stored as an `Imap` account plus the POP3 option).
fn parse_kind(p: &Value) -> Result<(AccountKind, bool), CmdError> {
    match str_field(p, &["kind", "type"])?.map(|k| k.trim().to_ascii_lowercase()).as_deref() {
        Some("imap") => Ok((AccountKind::Imap, false)),
        Some("pop3" | "pop") => Ok((AccountKind::Imap, true)),
        Some("jmap") => Ok((AccountKind::Jmap, false)),
        Some(other) => Err(CmdError::Params(format!("unknown account kind `{other}` (imap, pop3 or jmap)"))),
        None => Err(CmdError::Params("`kind` (\"imap\", \"pop3\" or \"jmap\") required".into())),
    }
}

/// Secrets from params: (incoming, outgoing, kind). `oauthToken` wins over `password`.
fn parse_secrets(p: &Value) -> Result<(Option<String>, Option<String>, Option<CredentialKind>), CmdError> {
    let check = |what: &str, v: &str| -> Result<String, CmdError> {
        if v.chars().count() > MAX_SECRET_CHARS || v.contains(['\r', '\n', '\0']) {
            return Err(CmdError::Params(format!("the {what} is not valid")));
        }
        Ok(v.to_owned())
    };
    let token = str_field(p, &["oauthToken", "oauth_token"])?.map(|t| check("token", t)).transpose()?;
    let password = str_field(p, &["password"])?.map(|t| check("password", t)).transpose()?;
    let out_pw = str_field(p, &["outgoingPassword", "outgoing_password"])?
        .map(|t| check("password", t))
        .transpose()?;
    Ok(match token {
        Some(t) => (Some(t), out_pw, Some(CredentialKind::OAuthToken)),
        None => {
            let kind = password.as_ref().map(|_| CredentialKind::Password);
            (password, out_pw, kind)
        }
    })
}

fn store_secrets(
    s: &mut Session,
    id: AccountId,
    incoming: Option<String>,
    outgoing: Option<String>,
) -> Result<(), CmdError> {
    if let Some(sec) = incoming {
        s.secrets.set(&secret_key(id, false), &sec).map_err(CmdError::Failed)?;
    }
    if let Some(sec) = outgoing {
        s.secrets.set(&secret_key(id, true), &sec).map_err(CmdError::Failed)?;
    }
    Ok(())
}

fn add(s: &mut Session, p: &Value) -> CmdResult {
    if s.mailbox.accounts.len() >= MAX_ACCOUNTS {
        return Err(CmdError::Failed(format!("at most {MAX_ACCOUNTS} accounts")));
    }
    let (kind, pop3) = parse_kind(p)?;
    let address = clean_line("the email address", str_field(p, &["address", "email"])?.unwrap_or(""))?;
    if !valid_address(&address) {
        return Err(CmdError::Params("`address` must be an email address (name@example.com)".into()));
    }
    if s.mailbox
        .accounts
        .iter()
        .any(|a| a.kind != AccountKind::Local && a.address.eq_ignore_ascii_case(&address))
    {
        return Err(CmdError::Failed(format!("An account for {address} already exists.")));
    }
    let display_name = match str_field(p, &["displayName", "display_name"])? {
        Some(n) if !n.trim().is_empty() => clean_line("the account name", n)?,
        _ => address.clone(),
    };
    let sender_name = match str_field(p, &["senderName", "sender_name", "name"])? {
        Some(n) => clean_line("your name", n)?,
        None => String::new(),
    };
    let incoming_v =
        field(p, &["incoming"]).ok_or_else(|| CmdError::Params("`incoming` server required".into()))?;
    let incoming = parse_server(incoming_v, kind, false, pop3, &address, None)?;
    let outgoing = match field(p, &["outgoing"]) {
        Some(v) => Some(parse_server(v, kind, true, false, &address, None)?),
        None => None,
    };
    let (secret_in, secret_out, cred) = parse_secrets(p)?;
    load_prefs(s);
    let mut opts = s.sync.prefs.data.defaults.clone();
    opts.pop3 = pop3;
    apply_option_params(&mut opts, p)?;
    // The first email account becomes the default unless told otherwise (a local data file never stays the
    // default sender once a real account exists).
    let default_is_local = s.mailbox.default_account().is_none_or(|a| a.kind == AccountKind::Local);
    let make_default = opt_bool(p, "default").unwrap_or(default_is_local);
    let dav = parse_dav(p, None)?.flatten();
    let dav_password = parse_dav_password(p)?;
    let id = s.mailbox.add_account(Account {
        kind,
        display_name: display_name.clone(),
        address: address.clone(),
        sender_name,
        incoming: Some(incoming),
        outgoing,
        is_default: make_default,
        credential: cred.unwrap_or_default(),
        server_saves_sent: opt_bool(p, "serverSavesSent").unwrap_or(false),
        dav,
        ..Default::default()
    });
    s.mailbox.create_default_folders(id);
    if let Some(inbox) = s.mailbox.role_folder(id, FolderRole::Inbox)
        && let Some(f) = s.mailbox.folders.get_mut(&inbox)
    {
        f.expanded = true;
    }
    // Options of an earlier account with the same id are gone with it.
    prefs(s).pop3.remove(&id.0);
    prefs(s).jmap.remove(&id.0);
    update_options(s, id, |o| *o = opts);
    store_secrets(s, id, secret_in, secret_out)?;
    store_dav_password(s, id, dav_password)?;
    s.refresh_me();
    Ok(
        json!({"account": id.0, "name": display_name, "address": address, "default": make_default, "kind": if pop3 { "pop3" } else { kind_name(kind) }}),
    )
}

fn edit(s: &mut Session, p: &Value) -> CmdResult {
    let id = find_account(s, p, "account")?;
    let Some(old) = s.mailbox.account(id).cloned() else {
        return Err(CmdError::Params("no such account".into()));
    };
    if old.kind == AccountKind::Local {
        return Err(CmdError::Disabled("a data file has no server settings".into()));
    }
    let mut a = old.clone();
    if let Some(addr) = str_field(p, &["address", "email"])? {
        let addr = clean_line("the email address", addr)?;
        if !valid_address(&addr) {
            return Err(CmdError::Params("`address` must be an email address".into()));
        }
        a.address = addr;
    }
    if let Some(n) = str_field(p, &["displayName", "display_name"])? {
        let n = clean_line("the account name", n)?;
        a.display_name = if n.is_empty() { a.address.clone() } else { n };
    }
    if let Some(n) = str_field(p, &["senderName", "sender_name"])? {
        a.sender_name = clean_line("your name", n)?;
    }
    load_prefs(s);
    let pop3 = is_pop3(s, id);
    if let Some(v) = field(p, &["incoming"]) {
        a.incoming = Some(parse_server(v, a.kind, false, pop3, &a.address, old.incoming.as_ref())?);
    }
    match p.get("outgoing") {
        Some(Value::Null) => a.outgoing = None,
        Some(v) => {
            a.outgoing = Some(parse_server(v, a.kind, true, false, &a.address, old.outgoing.as_ref())?)
        }
        None => {}
    }
    let mut opts = account_options(s, id);
    let opts_changed = apply_option_params(&mut opts, p)?;
    if let Some(b) = opt_bool(p, "serverSavesSent") {
        a.server_saves_sent = b;
    }
    if let Some(d) = parse_dav(p, old.dav.as_ref())? {
        a.dav = d;
    }
    let dav_password = parse_dav_password(p)?;
    let (secret_in, secret_out, cred) = parse_secrets(p)?;
    if let Some(c) = cred {
        a.credential = c;
    }
    // A different server or mailbox: forget what was synchronised (the next send/receive maps folders again;
    // messages already here are matched by Message-ID instead of downloaded twice).
    let server_changed = |x: &Option<ServerSettings>, y: &Option<ServerSettings>| {
        x.as_ref().map(|s| (&s.host, s.port, &s.username))
            != y.as_ref().map(|s| (&s.host, s.port, &s.username))
    };
    let reset = server_changed(&old.incoming, &a.incoming);
    if reset {
        // The contacts and tasks server is a separate one: its links stay unless it changed too (below).
        let dav = std::mem::take(&mut a.sync.dav);
        a.sync = Default::default();
        a.sync.dav = dav;
    }
    // Only another contacts and tasks server (or login) makes the links stale: a trailing `/`, a different
    // spelling of the same address or a new mail server keep them.
    let dav_reset = crate::sync::pim::dav_identity(&old) != crate::sync::pim::dav_identity(&a);
    let changed = a != old;
    if let Some(slot) = s.mailbox.accounts.iter_mut().find(|x| x.id == id) {
        *slot = a;
    }
    if reset {
        let folders: Vec<FolderId> =
            s.mailbox.folders.values().filter(|f| f.account == id).map(|f| f.id).collect();
        for f in &folders {
            if let Some(f) = s.mailbox.folders.get_mut(f) {
                f.remote_name = None;
            }
        }
        for m in s.mailbox.mail.items_mut().filter(|m| folders.contains(&m.folder)) {
            m.remote_id = None;
        }
        // A run against the old server may still be going: its results must not land in the new mapping.
        crate::sync::forget_account(s, id);
    }
    if reset {
        prefs(s).pop3.remove(&id.0);
        prefs(s).jmap.remove(&id.0);
    }
    if dav_reset {
        forget_dav(s, id);
    }
    if opts_changed || reset {
        update_options(s, id, |o| *o = opts);
    }
    let secrets_changed = secret_in.is_some() || secret_out.is_some() || dav_password.is_some();
    store_secrets(s, id, secret_in, secret_out)?;
    store_dav_password(s, id, dav_password)?;
    s.refresh_me();
    Ok(json!({"account": id.0, "changed": changed || secrets_changed || opts_changed, "resetSync": reset}))
}

/// Removes an account's folders and items, turning off rules and resetting quick steps that file into them,
/// and fixes the default account. Returns the removed folder count.
pub(crate) fn remove_account_data(s: &mut Session, id: AccountId) -> usize {
    let folders: Vec<FolderId> =
        s.mailbox.folders.values().filter(|f| f.account == id).map(|f| f.id).collect();
    let mb = &mut s.mailbox;
    mb.mail.retain(|_, m| !folders.contains(&m.folder));
    mb.appointments.retain(|_, x| !folders.contains(&x.folder));
    mb.contacts.retain(|_, x| !folders.contains(&x.folder));
    mb.tasks.retain(|_, x| !folders.contains(&x.folder));
    mb.notes.retain(|_, x| !folders.contains(&x.folder));
    for f in &folders {
        mb.folders.remove(f);
        mb.views.remove(f);
    }
    let was_default = mb.account(id).is_some_and(|a| a.is_default);
    mb.accounts.retain(|a| a.id != id);
    if was_default {
        let next = mb
            .accounts
            .iter()
            .position(|a| a.kind != AccountKind::Local)
            .or(if mb.accounts.is_empty() { None } else { Some(0) });
        if let Some(i) = next
            && let Some(a) = mb.accounts.get_mut(i)
        {
            a.is_default = true;
        }
    }
    mb.signature_defaults.retain(|d| d.account != id);
    let gone = |f: &FolderId| folders.contains(f);
    for rule in &mut mb.rules {
        if rule.actions.iter().any(|a| matches!(a, RuleAction::MoveTo(f) | RuleAction::CopyTo(f) if gone(f)))
        {
            rule.enabled = false;
        }
    }
    for step in &mut mb.quick_steps {
        for action in &mut step.actions {
            if let QuickStepAction::MoveTo(target)
            | QuickStepAction::CopyTo(target)
            | QuickStepAction::AlwaysMoveConversation(target) = action
                && target.as_ref().is_some_and(gone)
            {
                *target = None;
            }
        }
    }
    let order = mb.favorites();
    for (i, fid) in order.into_iter().enumerate() {
        if let Some(f) = mb.folders.get_mut(&fid) {
            f.favorite_pos = u32::try_from(i).ok();
        }
    }
    s.view.collapsed_groups.retain(|f, _| !folders.contains(f));
    if folders.contains(&s.selection.folder) {
        s.select_default_folder();
    }
    folders.len()
}

fn remove(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("account").is_none() {
        return Err(CmdError::Params("`account` (id, name or address) required".into()));
    }
    let id = find_account(s, p, "account")?;
    let Some(a) = s.mailbox.account(id).cloned() else {
        return Err(CmdError::Params("no such account".into()));
    };
    if a.kind == AccountKind::Local {
        return Err(CmdError::Disabled("use Remove on the data file instead".into()));
    }
    if s.mailbox.accounts.len() <= 1 {
        return Err(CmdError::Disabled("the only account cannot be removed".into()));
    }
    if opt_bool(p, "confirm") != Some(true) {
        let message = format!(
            "Removing {} deletes its offline copy of the mail (folders, messages and queued changes) from this \
             computer. Mail on the server is not affected.",
            a.address
        );
        s.notify(Notice::OpenDialog(
            "confirmRemoveAccount".into(),
            json!({"account": id.0, "message": message}),
        ));
        return Ok(json!({"removed": false, "needsConfirm": true, "message": message}));
    }
    // Shared mailboxes opened through this account's login go with it.
    let stores: Vec<AccountId> = crate::sync::shared::stores_of(s, id).iter().map(|a| a.id).collect();
    for store in stores {
        let _ = crate::sync::shared::close(s, store);
    }
    crate::sync::forget_account(s, id);
    let folders = remove_account_data(s, id);
    for key in account_secret_keys(id) {
        if s.secrets.get(&key).is_some()
            && let Err(e) = s.secrets.remove(&key)
        {
            log::warn!("could not remove a secret of account {}: {e}", id.0);
        }
    }
    forget_options(s, id);
    s.refresh_me();
    Ok(json!({"removed": true, "account": id.0, "folders": folders}))
}

fn set_default(s: &mut Session, p: &Value) -> CmdResult {
    let id = find_account(s, p, "account")?;
    for a in &mut s.mailbox.accounts {
        a.is_default = a.id == id;
    }
    Ok(json!({"default": id.0}))
}

fn test(s: &mut Session, p: &Value) -> CmdResult {
    let id = find_account(s, p, "account")?;
    let Some(a) = s.mailbox.account(id) else {
        return Err(CmdError::Params("no such account".into()));
    };
    if a.kind == AccountKind::Local {
        return Err(CmdError::Disabled("a data file has no mail server".into()));
    }
    let wait = opt_bool(p, "wait").unwrap_or(false);
    let report = crate::sync::test_account(s, id, wait)?;
    match report {
        Some(r) => Ok(r.to_json()),
        None => Ok(json!({"account": id.0, "started": true})),
    }
}

/// Moves the profile's secrets into the OS keyring (the unencrypted file stays in use when there is none).
fn use_keyring(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "confirm") != Some(true) {
        return Err(CmdError::Params("`confirm: true` required".into()));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        match s.use_os_keyring() {
            Ok(location) => Ok(json!({"encrypted": true, "location": location})),
            Err(e) => Err(CmdError::Failed(format!(
                "The system keyring is not available ({e}). Passwords stay in {}.",
                s.secrets.location()
            ))),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = s;
        Err(CmdError::Disabled("there is no system keyring in the browser".into()))
    }
}

fn options_cmd(s: &mut Session, p: &Value) -> CmdResult {
    load_prefs(s);
    let id = find_account(s, p, "account")?;
    let Some(a) = s.mailbox.account(id) else {
        return Err(CmdError::Params("no such account".into()));
    };
    if a.kind == AccountKind::Local {
        return Err(CmdError::Disabled("a data file has no mail server".into()));
    }
    let mut o = account_options(s, id);
    let changed = apply_option_params(&mut o, p)?;
    if changed {
        update_options(s, id, |x| *x = o.clone());
        crate::sync::options_changed(s, id);
    }
    let mut v = options_json(&o);
    if let Some(m) = v.as_object_mut() {
        m.insert("account".into(), json!(id.0));
        m.insert("changed".into(), json!(changed));
    }
    Ok(v)
}

/// Whether `url` may carry tokens: https, or http to this computer only in tests.
fn token_url_ok(url: &str) -> bool {
    let u = url.trim();
    u.starts_with("https://") && !u.contains(char::is_whitespace) && u.len() <= MAX_FIELD_CHARS
}

fn oauth_providers(_: &mut Session, _: &Value) -> CmdResult {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let list: Vec<Value> = mailcraft_net::oauth::builtin_providers()
            .iter()
            .map(|p| {
                let mut flows = vec!["browser"];
                if p.device_authorization_endpoint.is_some() {
                    flows.insert(0, "device");
                }
                json!({"id": p.id, "name": p.name, "flows": flows, "domains": p.domains, "scopes": p.scopes})
            })
            .collect();
        Ok(json!({
            "providers": list,
            "custom": true,
            "note": "Enter the client id of an application you registered with the provider; MailCraft has none built in.",
        }))
    }
    #[cfg(target_arch = "wasm32")]
    {
        Ok(json!({"providers": [], "custom": false}))
    }
}

/// The OAuth registration for a sign-in: a bundled provider (or `custom` endpoints) plus the user's client id.
fn oauth_binding(p: &Value) -> Result<OAuthBinding, CmdError> {
    let provider = str_field(p, &["provider"])?.map(str::trim).unwrap_or("").to_owned();
    if provider.is_empty() {
        return Err(CmdError::Params("`provider` required (see accounts.oauthProviders)".into()));
    }
    let client_id = clean_line("the client id", str_field(p, &["clientId", "client_id"])?.unwrap_or(""))?;
    if client_id.is_empty() {
        return Err(CmdError::Params(
            "`clientId` required: register an application with the provider and enter its client id".into(),
        ));
    }
    let mut b = OAuthBinding { provider: provider.clone(), client_id, ..Default::default() };
    if provider == "custom" {
        let url = |k: &str| -> Result<Option<String>, CmdError> {
            match str_field(p, &[k])? {
                Some(u) if token_url_ok(u) => Ok(Some(u.trim().to_owned())),
                Some(_) => Err(CmdError::Params(format!("`{k}` must be an https URL"))),
                None => Ok(None),
            }
        };
        b.name = "Custom".into();
        b.authorization_endpoint = url("authorizationEndpoint")?.unwrap_or_default();
        b.token_endpoint = url("tokenEndpoint")?
            .ok_or_else(|| CmdError::Params("`tokenEndpoint` required for a custom provider".into()))?;
        b.device_authorization_endpoint = url("deviceAuthorizationEndpoint")?;
        b.scopes = match p.get("scopes") {
            Some(Value::Array(a)) => a
                .iter()
                .map(|x| x.as_str().map(|t| clean_line("a scope", t)).transpose())
                .collect::<Result<Option<Vec<String>>, _>>()?
                .ok_or_else(|| CmdError::Params("`scopes` must be strings".into()))?,
            None | Some(Value::Null) => Vec::new(),
            Some(_) => return Err(CmdError::Params("`scopes` must be a list".into())),
        };
        return Ok(b);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let list = mailcraft_net::oauth::builtin_providers();
        let Some(bp) = list.iter().find(|x| x.id == provider) else {
            return Err(CmdError::Params(format!(
                "unknown provider `{provider}` (see accounts.oauthProviders)"
            )));
        };
        b.name = bp.name.clone();
        b.authorization_endpoint = bp.authorization_endpoint.clone();
        b.token_endpoint = bp.token_endpoint.clone();
        b.device_authorization_endpoint = bp.device_authorization_endpoint.clone();
        b.scopes = bp.scopes.clone();
        b.extra_auth_params = bp.extra_auth_params.clone();
        Ok(b)
    }
    #[cfg(target_arch = "wasm32")]
    {
        Err(CmdError::Disabled("OAuth sign-in is not available in the browser".into()))
    }
}

fn oauth_sign_in(s: &mut Session, p: &Value) -> CmdResult {
    load_prefs(s);
    let id = find_account(s, p, "account")?;
    let Some(a) = s.mailbox.account(id).cloned() else {
        return Err(CmdError::Params("no such account".into()));
    };
    if a.kind == AccountKind::Local {
        return Err(CmdError::Disabled("a data file has no mail server".into()));
    }
    let binding = oauth_binding(p)?;
    let client_secret = match str_field(p, &["clientSecret", "client_secret"])? {
        Some(t) if !t.trim().is_empty() => Some(clean_line("the client secret", t)?),
        _ => None,
    };
    let flow = match str_field(p, &["flow"])?.map(str::trim) {
        Some("device") => "device",
        Some("browser") => "browser",
        Some(other) => return Err(CmdError::Params(format!("unknown flow `{other}` (device or browser)"))),
        None if binding.device_authorization_endpoint.is_some() => "device",
        None => "browser",
    };
    if flow == "device" && binding.device_authorization_endpoint.is_none() {
        return Err(CmdError::Params(format!(
            "{} offers no device sign-in; use the browser flow",
            binding.name
        )));
    }
    if flow == "browser" && !token_url_ok(&binding.authorization_endpoint) {
        return Err(CmdError::Params("the provider has no https authorization endpoint".into()));
    }
    match &client_secret {
        Some(sec) => s.secrets.set(&client_secret_key(id), sec).map_err(CmdError::Failed)?,
        None => {
            if s.secrets.get(&client_secret_key(id)).is_some() {
                s.secrets.remove(&client_secret_key(id)).map_err(CmdError::Failed)?;
            }
        }
    }
    update_options(s, id, |o| o.oauth = Some(binding.clone()));
    let state = crate::sync::start_oauth(s, id, &a.address, flow == "device").map_err(CmdError::Failed)?;
    Ok(state)
}

fn oauth_status(s: &mut Session, p: &Value) -> CmdResult {
    let id = find_account(s, p, "account")?;
    s.poll_sync();
    Ok(crate::sync::oauth_status(s, id))
}

/// Looks up an address's server settings (see `sync::discover`): autoconfig, then DNS SRV. Without `wait`
/// the lookup runs in the background; call again with the same address for the result.
fn discover(s: &mut Session, p: &Value) -> CmdResult {
    let address = clean_line("the email address", str_field(p, &["address", "email"])?.unwrap_or(""))?;
    if !valid_address(&address) {
        return Err(CmdError::Params("`address` must be an email address (name@example.com)".into()));
    }
    let wait = opt_bool(p, "wait").unwrap_or(false);
    let refresh = opt_bool(p, "refresh").unwrap_or(false);
    crate::sync::discover(s, &address, wait, refresh)
}

fn oauth_cancel(s: &mut Session, p: &Value) -> CmdResult {
    let id = find_account(s, p, "account")?;
    let cancelled = crate::sync::cancel_oauth(s, id);
    Ok(json!({"account": id.0, "cancelled": cancelled}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rules::tests::session;

    fn add_imap(s: &mut Session) -> AccountId {
        let r = s
            .run(
                "accounts.add",
                &json!({
                    "kind": "imap",
                    "address": "me@example.net",
                    "senderName": "Me",
                    "incoming": {"host": "imap.example.net", "security": "tls"},
                    "outgoing": {"host": "smtp.example.net", "security": "starttls"},
                    "password": "s3cret",
                }),
            )
            .unwrap();
        AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
    }

    #[test]
    fn add_edit_default_remove() {
        let (mut s, _, _) = session();
        let id = add_imap(&mut s);
        let a = s.mailbox.account(id).unwrap().clone();
        assert_eq!(a.kind, AccountKind::Imap);
        assert!(a.is_default, "first email account becomes the default");
        let inc = a.incoming.as_ref().unwrap();
        assert_eq!((inc.port, inc.username.as_str()), (993, "me@example.net"));
        assert_eq!(a.outgoing.as_ref().unwrap().port, 587);
        assert!(s.mailbox.role_folder(id, FolderRole::Outbox).is_some());
        assert_eq!(account_secret(&s, id, true).as_deref(), Some("s3cret"), "outgoing falls back");
        // Secrets never reach the mailbox.
        let snapshot = serde_json::to_string(&s.mailbox).unwrap();
        assert!(!snapshot.contains("s3cret"));
        let listed = s.run("accounts.list", &json!({})).unwrap().to_string();
        assert!(!listed.contains("s3cret") && listed.contains("\"hasSecret\":true"));
        // Edit: new server forgets sync state.
        s.mailbox.accounts.iter_mut().find(|a| a.id == id).unwrap().sync.next_op = 7;
        let r = s
            .run(
                "accounts.edit",
                &json!({"account": "me@example.net", "incoming": {"host": "other.example.net"}}),
            )
            .unwrap();
        assert_eq!(r["resetSync"], true);
        assert_eq!(s.mailbox.account(id).unwrap().sync.next_op, 0);
        s.run("accounts.setDefault", &json!({"account": 1})).unwrap();
        assert!(!s.mailbox.account(id).unwrap().is_default);
        // Remove asks first.
        let r = s.run("accounts.remove", &json!({"account": id.0})).unwrap();
        assert_eq!(r["needsConfirm"], true);
        assert!(s.mailbox.account(id).is_some());
        s.run("accounts.remove", &json!({"account": id.0, "confirm": true})).unwrap();
        assert!(s.mailbox.account(id).is_none());
        assert!(s.mailbox.folders.values().all(|f| f.account != id));
        assert!(s.secrets.get(&secret_key(id, false)).is_none());
        assert!(s.mailbox.validate().is_empty());
    }

    #[test]
    fn add_validates() {
        let (mut s, _, _) = session();
        for bad in [
            json!({"kind": "exchange", "address": "a@example.com", "incoming": {"host": "h"}}),
            json!({"kind": "imap", "address": "nope", "incoming": {"host": "h"}}),
            json!({"kind": "imap", "address": "a@example.com"}),
            json!({"kind": "imap", "address": "a@example.com", "incoming": {"host": ""}}),
            json!({"kind": "imap", "address": "a@example.com", "incoming": {"host": "h", "port": 0}}),
            json!({"kind": "imap", "address": "a@example.com", "incoming": {"host": "h", "security": "x"}}),
            json!({"kind": "imap", "address": "a@example.com", "incoming": {"host": "h", "extraRootPem": "junk"}}),
            json!({"kind": "imap", "address": "a@example.com", "incoming": {"host": "h"}, "password": "a\nb"}),
        ] {
            assert!(matches!(s.run("accounts.add", &bad), Err(CmdError::Params(_))), "{bad}");
        }
        add_imap(&mut s);
        assert!(matches!(
            s.run(
                "accounts.add",
                &json!({"kind": "imap", "address": "ME@example.net", "incoming": {"host": "h"}})
            ),
            Err(CmdError::Failed(_))
        ));
    }

    #[test]
    fn pem_parsing() {
        let pem = "junk\n-----BEGIN CERTIFICATE-----\nAAEC\nAw==\n-----END CERTIFICATE-----\n";
        assert_eq!(pem_certificates(pem).unwrap(), vec![vec![0, 1, 2, 3]]);
        assert!(pem_certificates("-----BEGIN CERTIFICATE-----\n!!\n-----END CERTIFICATE-----").is_err());
        assert!(pem_certificates("").is_err());
    }

    #[test]
    fn file_secret_store_is_private_and_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = FileSecretStore::open(dir.path()).unwrap();
        st.set("account/1/incoming", "pw").unwrap();
        let text = std::fs::read_to_string(dir.path().join(SECRETS_FILE)).unwrap();
        assert!(text.contains("NOT ENCRYPTED"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(SECRETS_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let st2 = FileSecretStore::open(dir.path()).unwrap();
        assert_eq!(st2.get("account/1/incoming").as_deref(), Some("pw"));
        let mut st2 = st2;
        st2.remove("account/1/incoming").unwrap();
        assert!(FileSecretStore::open(dir.path()).unwrap().get("account/1/incoming").is_none());
        std::fs::write(dir.path().join(SECRETS_FILE), "{not json").unwrap();
        assert!(FileSecretStore::open(dir.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn file_secret_store_tightens_a_readable_file_on_open() {
        // A secrets file copied in or restored from a backup with group/world read bits is made owner-only as
        // soon as the profile opens, not only on the next password change.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SECRETS_FILE);
        std::fs::write(&path, r#"{"secrets":{"account/1/incoming":"pw"}}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let st = FileSecretStore::open(dir.path()).unwrap();
        assert_eq!(st.get("account/1/incoming").as_deref(), Some("pw"));
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    /// A stand-in OS keyring.
    #[derive(Default, Clone)]
    struct FakeKeyring(std::sync::Arc<std::sync::Mutex<BTreeMap<String, String>>>);

    impl KeyBackend for FakeKeyring {
        fn get(&self, key: &str) -> Result<Option<String>, String> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        fn set(&self, key: &str, secret: &str) -> Result<(), String> {
            self.0.lock().unwrap().insert(key.to_owned(), secret.to_owned());
            Ok(())
        }
        fn remove(&self, key: &str) -> Result<(), String> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
        fn name(&self) -> String {
            "Test keyring".into()
        }
    }

    #[test]
    fn secrets_move_to_the_keyring_and_leave_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path()).unwrap();
        assert!(s.secrets.location().contains("unencrypted"));
        let id = add_imap(&mut s);
        let ring = FakeKeyring::default();
        let loc = s.move_secrets_to(Box::new(ring.clone())).unwrap();
        assert!(loc.contains("Test keyring") && loc.contains("encrypted"));
        assert!(s.secrets.encrypted());
        assert_eq!(account_secret(&s, id, false).as_deref(), Some("s3cret"));
        let file = std::fs::read_to_string(dir.path().join(SECRETS_FILE)).unwrap();
        assert!(!file.contains("s3cret"), "the password left the unencrypted file");
        let stored = ring.0.lock().unwrap().clone();
        assert_eq!(stored.len(), 1);
        let (k, v) = stored.iter().next().unwrap();
        assert!(
            k.starts_with("profile-") && k.ends_with("/account/1/incoming")
                || k.ends_with(&format!("/account/{}/incoming", id.0))
        );
        assert_eq!(v, "s3cret");
        // New secrets go to the keyring; removal clears them there.
        s.run("accounts.edit", &json!({"account": id.0, "password": "n3w"})).unwrap();
        assert_eq!(ring.0.lock().unwrap().values().next().map(String::as_str), Some("n3w"));
        s.run("accounts.remove", &json!({"account": id.0, "confirm": true})).unwrap();
        assert!(ring.0.lock().unwrap().is_empty());
        // Moving again is a no-op.
        assert!(s.move_secrets_to(Box::new(FakeKeyring::default())).is_ok());
        let mut mem = session().0;
        assert!(
            mem.move_secrets_to(Box::new(FakeKeyring::default())).is_err(),
            "in-memory sessions keep memory"
        );
        // The command refuses an in-memory profile before it ever contacts the system keyring.
        assert!(matches!(
            mem.run("accounts.useKeyring", &json!({"confirm": true})),
            Err(CmdError::Failed(_))
        ));
        assert!(matches!(s.run("accounts.useKeyring", &json!({})), Err(CmdError::Params(_))));
        // Already encrypted: nothing to do (the fake keyring is in use, the system one is never opened).
        assert_eq!(s.run("accounts.useKeyring", &json!({"confirm": true})).unwrap()["encrypted"], true);
    }

    #[test]
    fn pop3_accounts_and_options() {
        let (mut s, _, _) = session();
        let r = s
            .run(
                "accounts.add",
                &json!({"kind": "pop3", "address": "pop@example.net", "incoming": {"host": "pop.example.net"},
                    "password": "pw", "leaveOnServer": false, "mode": "headers"}),
            )
            .unwrap();
        assert_eq!(r["kind"], "pop3");
        let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        assert!(is_pop3(&s, id));
        let a = s.mailbox.account(id).unwrap();
        assert_eq!(a.incoming.as_ref().unwrap().port, 995);
        let o = s.run("accounts.options", &json!({"account": id.0})).unwrap();
        assert_eq!(
            (o["pop3"].clone(), o["leaveOnServer"].clone(), o["download"].clone()),
            (json!(true), json!(false), json!("headers"))
        );
        let o = s
            .run("accounts.options", &json!({"account": id.0, "removeAfterDays": null, "pollMinutes": 3}))
            .unwrap();
        assert_eq!(o["removeAfterDays"], Value::Null);
        assert_eq!(o["changed"], true);
        for bad in [
            json!({"account": id.0, "pollMinutes": 0}),
            json!({"account": id.0, "push": "yes"}),
            json!({"account": id.0, "intervalMinutes": -1}),
            json!({"account": id.0, "mode": "most"}),
        ] {
            assert!(matches!(s.run("accounts.options", &bad), Err(CmdError::Params(_))), "{bad}");
        }
        let plain = s
            .run("accounts.add", &json!({"kind": "pop3", "address": "p2@example.net", "incoming": {"host": "h", "security": "none"}}))
            .unwrap();
        let p2 = AccountId(u32::try_from(plain["account"].as_u64().unwrap()).unwrap());
        assert_eq!(s.mailbox.account(p2).unwrap().incoming.as_ref().unwrap().port, 110);
        // Removing the account drops its options.
        s.run("accounts.remove", &json!({"account": id.0, "confirm": true})).unwrap();
        assert!(!is_pop3(&s, id));
    }

    #[test]
    fn oauth_sign_in_validates_its_registration() {
        let (mut s, _, _) = session();
        let id = add_imap(&mut s);
        let providers = s.run("accounts.oauthProviders", &json!({})).unwrap();
        assert!(providers["providers"].as_array().unwrap().iter().any(|p| p["id"] == "microsoft"));
        let google =
            providers["providers"].as_array().unwrap().iter().find(|p| p["id"] == "google").unwrap().clone();
        assert_eq!(google["flows"], json!(["browser"]));
        for bad in [
            json!({"account": id.0, "provider": "microsoft"}),
            json!({"account": id.0, "provider": "nobody", "clientId": "x"}),
            json!({"account": id.0, "provider": "google", "clientId": "x", "flow": "device"}),
            json!({"account": id.0, "provider": "custom", "clientId": "x", "tokenEndpoint": "http://example.com/t"}),
            json!({"account": id.0, "provider": "custom", "clientId": "x"}),
            json!({"account": id.0, "provider": "microsoft", "clientId": "x", "flow": "carrier pigeon"}),
        ] {
            assert!(matches!(s.run("accounts.oauthSignIn", &bad), Err(CmdError::Params(_))), "{bad}");
        }
        // A browser sign-in starts at once with the provider's page (nothing is contacted until then).
        let v = s
            .run(
                "accounts.oauthSignIn",
                &json!({"account": id.0, "provider": "google", "clientId": "my-app", "clientSecret": "shh"}),
            )
            .unwrap();
        assert_eq!(v["flow"], "browser");
        let url = v["authUrl"].as_str().unwrap();
        assert!(url.starts_with("https://accounts.google.com/") && url.contains("client_id=my-app"), "{url}");
        assert!(!url.contains("shh"), "the client secret never goes into the page address");
        assert_eq!(s.secrets.get(&client_secret_key(id)).as_deref(), Some("shh"));
        assert_eq!(account_options(&s, id).oauth.unwrap().client_id, "my-app");
        assert_eq!(s.run("accounts.oauthStatus", &json!({"account": id.0})).unwrap()["status"], "waiting");
        assert_eq!(s.run("accounts.oauthCancel", &json!({"account": id.0})).unwrap()["cancelled"], true);
        assert_eq!(s.run("accounts.oauthStatus", &json!({"account": id.0})).unwrap()["status"], "cancelled");
    }

    #[test]
    fn account_options_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let id;
        {
            let mut s = Session::open(dir.path()).unwrap();
            id = add_imap(&mut s);
            s.run("accounts.options", &json!({"account": id.0, "push": false, "intervalMinutes": 12}))
                .unwrap();
            prefs(&mut s).headers_only.insert(77);
            save_prefs(&mut s);
        }
        let text = std::fs::read_to_string(dir.path().join(ACCOUNT_OPTIONS_FILE)).unwrap();
        assert!(!text.contains("s3cret"));
        let mut s = Session::open(dir.path()).unwrap();
        load_prefs(&mut s);
        let o = account_options(&s, id);
        assert!(!o.push);
        assert_eq!(o.interval_minutes, Some(12));
        assert!(s.sync.prefs.data.headers_only.contains(&77));
        // A garbled file is not fatal.
        drop(s);
        std::fs::write(dir.path().join(ACCOUNT_OPTIONS_FILE), "{nope").unwrap();
        let mut s = Session::open(dir.path()).unwrap();
        load_prefs(&mut s);
        assert!(account_options(&s, id).push, "defaults after a garbled file");
    }
}
