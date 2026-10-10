//! Accounts (File › Info › Add Account / Account Settings, `plan/outlook/backstage.md`): the `accounts.*`
//! commands, the credentials store, and the connection settings built from an account plus its secret.
//!
//! Secrets (passwords, OAuth access tokens) never enter the mailbox, its snapshot or its journal. They live in
//! a [`SecretStore`] on the session: in memory by default, or in `secrets.json` next to the data file (owner
//! read/write only). That file is **not encrypted yet**; it says so inside.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::cmd::util::opt_bool;
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session, always};
use mailcraft_model::account::CredentialKind;
use mailcraft_model::{
    Account, AccountId, AccountKind, FolderId, FolderRole, QuickStepAction, RuleAction, Security,
    ServerSettings,
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
    /// Where the secrets live ("memory" or a file path), for Account Settings.
    fn location(&self) -> String;
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
}

/// Secrets in a JSON file in the profile folder, readable and writable by the owner only (0600 on Unix).
///
/// The file is not encrypted yet (an OS keychain backend comes later); its `note` field says so.
pub struct FileSecretStore {
    path: PathBuf,
    map: BTreeMap<String, String>,
}

const SECRETS_NOTE: &str = "MailCraft account secrets. NOT ENCRYPTED YET: protect this file like a password list. \
     It is kept out of the mailbox data files on purpose.";

impl FileSecretStore {
    /// Opens `dir/secrets.json` (missing = empty). A garbled file is reported, not overwritten silently.
    pub fn open(dir: &Path) -> Result<FileSecretStore, String> {
        let path = dir.join(SECRETS_FILE);
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
        self.path.display().to_string()
    }
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
            r#"{"kind": "imap"|"jmap", "address": string, "displayName"?: string, "senderName"?: string, "incoming": server, "outgoing"?: server, "password"?: string, "outgoingPassword"?: string, "oauthToken"?: string, "default"?: bool, "serverSavesSent"?: bool} where server = {"host", "port"?, "security"?, "username"?, "auth"?, "extraRootPem"?, "allowPlaintextAuth"?} (JMAP: host = session URL)"#,
            true,
            always,
            add,
        ),
        spec(
            "accounts.edit",
            "Change Account",
            r#"{"account": number|string, "displayName"?, "senderName"?, "address"?, "incoming"?: server, "outgoing"?: server|null, "password"?, "outgoingPassword"?, "oauthToken"?, "serverSavesSent"?: bool}"#,
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
        "kind": kind_name(a.kind),
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
    })
}

fn list(s: &mut Session, _: &Value) -> CmdResult {
    let accounts: Vec<Value> = s.mailbox.accounts.iter().map(|a| account_settings_json(s, a)).collect();
    Ok(json!({"accounts": accounts, "secrets": s.secrets.location()}))
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

/// Server settings from params; `smtp` picks the default ports. `old` supplies values not given (edit).
fn parse_server(
    v: &Value,
    kind: AccountKind,
    smtp: bool,
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

fn parse_kind(p: &Value) -> Result<AccountKind, CmdError> {
    match str_field(p, &["kind", "type"])?.map(|k| k.trim().to_ascii_lowercase()).as_deref() {
        Some("imap") => Ok(AccountKind::Imap),
        Some("jmap") => Ok(AccountKind::Jmap),
        Some(other) => Err(CmdError::Params(format!("unknown account kind `{other}` (imap or jmap)"))),
        None => Err(CmdError::Params("`kind` (\"imap\" or \"jmap\") required".into())),
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
    let kind = parse_kind(p)?;
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
    let incoming = parse_server(incoming_v, kind, false, &address, None)?;
    let outgoing = match field(p, &["outgoing"]) {
        Some(v) => Some(parse_server(v, kind, true, &address, None)?),
        None => None,
    };
    let (secret_in, secret_out, cred) = parse_secrets(p)?;
    // The first email account becomes the default unless told otherwise (a local data file never stays the
    // default sender once a real account exists).
    let default_is_local = s.mailbox.default_account().is_none_or(|a| a.kind == AccountKind::Local);
    let make_default = opt_bool(p, "default").unwrap_or(default_is_local);
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
        ..Default::default()
    });
    s.mailbox.create_default_folders(id);
    if let Some(inbox) = s.mailbox.role_folder(id, FolderRole::Inbox)
        && let Some(f) = s.mailbox.folders.get_mut(&inbox)
    {
        f.expanded = true;
    }
    store_secrets(s, id, secret_in, secret_out)?;
    s.refresh_me();
    Ok(json!({"account": id.0, "name": display_name, "address": address, "default": make_default}))
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
    if let Some(v) = field(p, &["incoming"]) {
        a.incoming = Some(parse_server(v, a.kind, false, &a.address, old.incoming.as_ref())?);
    }
    match p.get("outgoing") {
        Some(Value::Null) => a.outgoing = None,
        Some(v) => a.outgoing = Some(parse_server(v, a.kind, true, &a.address, old.outgoing.as_ref())?),
        None => {}
    }
    if let Some(b) = opt_bool(p, "serverSavesSent") {
        a.server_saves_sent = b;
    }
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
        a.sync = Default::default();
    }
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
        for m in s.mailbox.mail.values_mut().filter(|m| folders.contains(&m.folder)) {
            m.remote_id = None;
        }
        // A run against the old server may still be going: its results must not land in the new mapping.
        crate::sync::forget_account(s, id);
    }
    let secrets_changed = secret_in.is_some() || secret_out.is_some();
    store_secrets(s, id, secret_in, secret_out)?;
    s.refresh_me();
    Ok(json!({"account": id.0, "changed": changed || secrets_changed, "resetSync": reset}))
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
            if let QuickStepAction::MoveTo(target) | QuickStepAction::CopyTo(target) = action
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
    crate::sync::forget_account(s, id);
    let folders = remove_account_data(s, id);
    for outgoing in [false, true] {
        if let Err(e) = s.secrets.remove(&secret_key(id, outgoing)) {
            log::warn!("could not remove the secret of account {}: {e}", id.0);
        }
    }
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
            json!({"kind": "pop", "address": "a@example.com", "incoming": {"host": "h"}}),
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
        assert!(text.contains("NOT ENCRYPTED YET"));
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
}
