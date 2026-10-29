//! Local test mail servers (tools layer; dev/test infrastructure, never shipped in the app).
//!
//! [`TestServers::start`] runs, on `127.0.0.1` only, in background threads (std::net, no async runtime):
//! - an IMAP server speaking enough of IMAP4rev1 (RFC 3501) and IMAP4rev2 (RFC 9051) for a mail client:
//!   CAPABILITY, STARTTLS, LOGIN, AUTHENTICATE PLAIN (with SASL-IR), ENABLE, ID, NAMESPACE, LIST/LSUB (with
//!   SPECIAL-USE attributes, RFC 6154, and basic LIST-EXTENDED/LIST-STATUS), SELECT/EXAMINE (with CONDSTORE),
//!   CREATE/DELETE/RENAME, SUBSCRIBE/UNSUBSCRIBE, STATUS, APPEND (sync and non-sync literals, APPENDUID),
//!   CHECK/CLOSE/UNSELECT, EXPUNGE/UID EXPUNGE, [UID] SEARCH (incl. ESEARCH RETURN), [UID] FETCH (FLAGS,
//!   INTERNALDATE, RFC822.SIZE, ENVELOPE, BODY/BODYSTRUCTURE, BODY[.PEEK][section]<partial>, RFC822*, MODSEQ,
//!   CHANGEDSINCE), [UID] STORE (±FLAGS, .SILENT, UNCHANGEDSINCE), [UID] COPY and MOVE (COPYUID), IDLE with
//!   notifications from other sessions and from SMTP delivery, NOOP, LOGOUT;
//! - an SMTP submission server (RFC 5321/6409): EHLO/HELO, STARTTLS, AUTH PLAIN/LOGIN, MAIL/RCPT/DATA with
//!   dot-unstuffing, SIZE limit, per-recipient rejection, RSET/NOOP/VRFY/QUIT. Accepted messages are delivered
//!   into the INBOX of local users, so send → receive round trips work, and every accepted transaction is
//!   recorded ([`TestServers::submissions`]).
//!
//! - a POP3 server (RFC 1939 with CAPA, STLS, USER/PASS, AUTH PLAIN, UIDL and TOP) over each user's INBOX, so
//!   POP3 accounts can be tested against the same mail (deletions at QUIT reach IMAP sessions too).
//!
//! - a JMAP server (RFC 8620/8621 over HTTP/1.1: session, Mailbox/Email get/changes/query/set/import,
//!   Identity/get, EmailSubmission/set, blob upload and download, push over an event source) on the same
//!   mail, for JMAP accounts.
//!
//! - a WebDAV server with CardDAV (RFC 6352) and CalDAV (RFC 4791) collections per user (contacts, calendar,
//!   tasks and a public collection for published calendars) on the web server's ports (`dav.rs`);
//!   and the RFC 2342 other-users and shared IMAP namespaces ([`TestServers::grant_access`],
//!   [`TestServers::share_mailbox`]).
//!
//! - an OAuth 2.0 authorization server (device grant, authorization code with PKCE, refresh tokens) whose
//!   access tokens sign in to IMAP and SMTP (XOAUTH2, OAUTHBEARER) and JMAP (Bearer), plus autoconfig
//!   documents, both on a small web server (HTTP and HTTPS); and a fake DNS responder (UDP) answering SRV
//!   questions from records the test sets. Nothing is ever forwarded to a real server.
//!
//! Each protocol listens on a plain port (with STARTTLS) and an implicit-TLS port. TLS uses a self-signed
//! certificate for `localhost`/`127.0.0.1` generated at start; [`TestServers::cert_pem`] /
//! [`TestServers::cert_der`] let test clients trust it. All state is in memory and shared by both servers.
//! Dropping [`TestServers`] stops every thread.

mod conn;
mod date;
mod dav;
mod dns;
mod imap;
mod jmap;
mod mime;
mod oauth;
mod pop3;
mod server;
mod smtp;
mod store;
pub mod synth;
mod tls;
mod web;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

pub use date::DateTime;
pub use dns::Srv;
/// The rustls version the servers use, for building test clients.
pub use rustls;
pub use store::{SpecialUse, StoredMessage, Submission};

/// Errors from the test servers' API.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls: {0}")]
    Tls(String),
    #[error("certificate: {0}")]
    Cert(String),
    #[error("unknown user `{0}`")]
    UnknownUser(String),
    #[error("unknown mailbox `{0}`")]
    UnknownMailbox(String),
    #[error("mailbox `{0}` already exists")]
    MailboxExists(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Server configuration. Ports of 0 (the default) pick a free ephemeral port.
#[derive(Clone, Debug)]
pub struct Config {
    /// Name used in greetings, EHLO and trace headers.
    pub hostname: String,
    /// IMAP with STARTTLS.
    pub imap_port: u16,
    /// IMAP over implicit TLS.
    pub imaps_port: u16,
    /// SMTP submission with STARTTLS.
    pub smtp_port: u16,
    /// SMTP submission over implicit TLS.
    pub smtps_port: u16,
    /// POP3 with STLS.
    pub pop3_port: u16,
    /// POP3 over implicit TLS.
    pub pop3s_port: u16,
    /// JMAP over plain HTTP (clients must allow plain-text credentials for it).
    pub jmap_port: u16,
    /// JMAP over HTTPS.
    pub jmaps_port: u16,
    /// SMTP SIZE limit in octets (also caps IMAP APPEND).
    pub max_message_size: usize,
    /// Recipients the SMTP server rejects with 550 (case-insensitive addresses).
    pub reject_recipients: Vec<String>,
    /// SMTP: MAIL FROM requires a prior AUTH (submission behaviour).
    pub smtp_require_auth: bool,
    /// IMAP LOGIN/AUTHENTICATE and SMTP AUTH only after TLS (advertises LOGINDISABLED until then).
    pub auth_requires_tls: bool,
    /// SMTP delivery prepends `Return-Path:` and `Received:` trace fields to delivered messages.
    pub add_trace_headers: bool,
    /// Connections idle (no client data) for longer than this are closed. IDLE counts as activity-free time.
    pub idle_timeout: Duration,
    /// IMAP advertises IDLE (and IMAP4rev2, which includes it). Off to test clients facing a server without
    /// push.
    pub imap_idle: bool,
    /// IMAP commands answered NO although advertised (upper case, e.g. `NAMESPACE`), as some proxies do.
    pub imap_refuse: Vec<String>,
    /// IMAP SEARCH with a `TEXT` or `BODY` key is answered NO (a server without full-text search).
    pub imap_refuse_text_search: bool,
    /// Users created at start: (address, password).
    pub users: Vec<(String, String)>,
    /// The web server (autoconfig, OAuth) over plain HTTP.
    pub web_port: u16,
    /// The web server over HTTPS.
    pub webs_port: u16,
    /// The fake DNS responder (UDP).
    pub dns_port: u16,
    /// OAuth client ids the authorization server accepts (public clients, no secret).
    pub oauth_clients: Vec<String>,
    /// Lifetime of issued access tokens.
    pub oauth_token_lifetime: Duration,
    /// JMAP offers push (an `eventSourceUrl`). Off to test clients falling back to polling.
    pub jmap_push: bool,
    /// JMAP filter conditions (`text`, `subject`, …) that Email/query refuses with `unsupportedFilter`
    /// (RFC 8621 section 4.4), wherever they appear in the filter.
    pub jmap_unsupported_filters: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hostname: "localhost".to_owned(),
            imap_port: 0,
            imaps_port: 0,
            smtp_port: 0,
            smtps_port: 0,
            pop3_port: 0,
            pop3s_port: 0,
            jmap_port: 0,
            jmaps_port: 0,
            max_message_size: 25 * 1024 * 1024,
            reject_recipients: Vec::new(),
            smtp_require_auth: true,
            auth_requires_tls: false,
            add_trace_headers: true,
            idle_timeout: Duration::from_secs(30 * 60),
            imap_idle: true,
            imap_refuse: Vec::new(),
            imap_refuse_text_search: false,
            users: Vec::new(),
            web_port: 0,
            webs_port: 0,
            dns_port: 0,
            oauth_clients: vec![OAUTH_TEST_CLIENT.to_owned()],
            oauth_token_lifetime: Duration::from_secs(3600),
            jmap_push: true,
            jmap_unsupported_filters: Vec::new(),
        }
    }
}

/// The client id the OAuth test server accepts by default.
pub const OAUTH_TEST_CLIENT: &str = "mailcraft-test-client";

/// Running test servers. Stops all listener and connection threads on drop.
pub struct TestServers {
    /// IMAP plain-text port (STARTTLS available).
    pub imap_port: u16,
    /// IMAP implicit-TLS port.
    pub imaps_port: u16,
    /// SMTP plain-text port (STARTTLS available).
    pub smtp_port: u16,
    /// SMTP implicit-TLS port.
    pub smtps_port: u16,
    /// POP3 plain-text port (STLS available).
    pub pop3_port: u16,
    /// POP3 implicit-TLS port.
    pub pop3s_port: u16,
    /// JMAP plain HTTP port.
    pub jmap_port: u16,
    /// JMAP HTTPS port.
    pub jmaps_port: u16,
    /// Web server (autoconfig, OAuth) plain HTTP port.
    pub web_port: u16,
    /// Web server HTTPS port.
    pub webs_port: u16,
    /// Fake DNS responder (UDP) port.
    pub dns_port: u16,
    /// Self-signed server certificate (PEM) for `localhost` and `127.0.0.1`.
    pub cert_pem: String,
    /// The same certificate, DER-encoded.
    pub cert_der: Vec<u8>,
    inner: server::Running,
    shared: Arc<store::Shared>,
}

impl TestServers {
    /// Binds every listener on 127.0.0.1, generates the certificate and starts the accept threads.
    pub fn start(config: Config) -> Result<TestServers> {
        let identity = tls::Identity::generate(&config.hostname)?;
        let shared = Arc::new(store::Shared::new());
        for (addr, pw) in &config.users {
            shared.lock().add_user(addr, pw)?;
        }
        let lifetime = i64::try_from(config.oauth_token_lifetime.as_secs()).unwrap_or(i64::MAX);
        shared.lock().oauth = oauth::OAuthState::new(config.oauth_clients.clone(), lifetime);
        let running = server::Running::start(config, Arc::clone(&shared), identity.server_config()?)?;
        Ok(TestServers {
            imap_port: running.imap.port(),
            imaps_port: running.imaps.port(),
            smtp_port: running.smtp.port(),
            smtps_port: running.smtps.port(),
            pop3_port: running.pop3.port(),
            pop3s_port: running.pop3s.port(),
            jmap_port: running.jmap.port(),
            jmaps_port: running.jmaps.port(),
            web_port: running.web.port(),
            webs_port: running.webs.port(),
            dns_port: running.dns.port(),
            cert_pem: identity.cert_pem,
            cert_der: identity.cert_der,
            inner: running,
            shared,
        })
    }

    /// `127.0.0.1:<imap_port>`.
    pub fn imap_addr(&self) -> SocketAddr {
        self.inner.imap
    }
    /// `127.0.0.1:<imaps_port>`.
    pub fn imaps_addr(&self) -> SocketAddr {
        self.inner.imaps
    }
    /// `127.0.0.1:<smtp_port>`.
    pub fn smtp_addr(&self) -> SocketAddr {
        self.inner.smtp
    }
    /// `127.0.0.1:<smtps_port>`.
    pub fn smtps_addr(&self) -> SocketAddr {
        self.inner.smtps
    }

    /// `127.0.0.1:<pop3_port>`.
    pub fn pop3_addr(&self) -> SocketAddr {
        self.inner.pop3
    }
    /// `127.0.0.1:<pop3s_port>`.
    pub fn pop3s_addr(&self) -> SocketAddr {
        self.inner.pop3s
    }

    /// The JMAP session URL over HTTPS (`https://127.0.0.1:<jmaps_port>/.well-known/jmap`).
    pub fn jmap_url(&self) -> String {
        format!("https://127.0.0.1:{}/.well-known/jmap", self.jmaps_port)
    }

    /// The web server's HTTPS base URL (`https://127.0.0.1:<webs_port>`).
    pub fn web_url(&self) -> String {
        format!("https://127.0.0.1:{}", self.webs_port)
    }

    /// The fake DNS responder's address (UDP).
    pub fn dns_addr(&self) -> SocketAddr {
        self.inner.dns
    }

    /// Autoconfig URL templates (`{domain}`, `{address}` placeholders) pointing at this server, in the order a
    /// client tries the real locations: the `autoconfig.<domain>` host, then the domain's well-known path.
    pub fn autoconfig_url_templates(&self) -> Vec<String> {
        let base = self.web_url();
        vec![
            format!("{base}/autoconfig/{{domain}}/mail/config-v1.1.xml?emailaddress={{address}}"),
            format!("{base}/{{domain}}/.well-known/autoconfig/mail/config-v1.1.xml"),
        ]
    }

    /// Serves `xml` as the autoconfig document of `domain` at both locations (`None` removes it).
    pub fn set_autoconfig(&self, domain: &str, xml: Option<&str>) {
        self.put_autoconfig(domain, xml, true);
    }

    /// Serves `xml` as the autoconfig document of `domain` only at the domain's well-known location.
    pub fn set_autoconfig_well_known(&self, domain: &str, xml: &str) {
        self.put_autoconfig(domain, Some(xml), false);
    }

    fn put_autoconfig(&self, domain: &str, xml: Option<&str>, both: bool) {
        let mut w = self.inner.ctx.web.lock().unwrap_or_else(|e| e.into_inner());
        let key = domain.trim().to_ascii_lowercase();
        match xml {
            Some(x) => w.autoconfig.insert(key, (x.to_owned(), both)),
            None => w.autoconfig.remove(&key),
        };
    }

    /// Requests the web server received ("GET /path?query"), oldest first.
    pub fn web_requests(&self) -> Vec<String> {
        self.inner.ctx.web.lock().unwrap_or_else(|e| e.into_inner()).requests.clone()
    }

    /// Sets the SRV records the fake DNS responder answers for `name` (e.g. `_imaps._tcp.example.com`).
    pub fn set_srv(&self, name: &str, records: Vec<Srv>) {
        let key = name.trim().trim_end_matches('.').to_ascii_lowercase();
        self.inner.ctx.dns.lock().unwrap_or_else(|e| e.into_inner()).insert(key, Some(records));
    }

    /// Makes the fake DNS responder answer `name` with a truncated (TC) empty message.
    pub fn set_srv_truncated(&self, name: &str) {
        let key = name.trim().trim_end_matches('.').to_ascii_lowercase();
        self.inner.ctx.dns.lock().unwrap_or_else(|e| e.into_inner()).insert(key, None);
    }

    /// OAuth endpoints of the test authorization server: (authorization, token, device authorization).
    pub fn oauth_endpoints(&self) -> (String, String, String) {
        let base = self.web_url();
        (format!("{base}/oauth/authorize"), format!("{base}/oauth/token"), format!("{base}/oauth/device"))
    }

    /// The user at the verification page approves the device code `user_code` for `user`. Errors when no
    /// sign-in is waiting for that code.
    pub fn oauth_approve(&self, user_code: &str, user: &str) -> Result<()> {
        let mut store = self.shared.lock();
        if store.user(user).is_none() {
            return Err(Error::UnknownUser(user.to_owned()));
        }
        if store.oauth.decide(user_code, Some(user)) {
            Ok(())
        } else {
            Err(Error::Invalid(format!("no sign-in waits for code {user_code}")))
        }
    }

    /// The user denies the device code `user_code`.
    pub fn oauth_deny(&self, user_code: &str) -> bool {
        self.shared.lock().oauth.decide(user_code, None)
    }

    /// Device codes waiting for the user.
    pub fn oauth_pending_codes(&self) -> Vec<String> {
        self.shared.lock().oauth.pending_user_codes()
    }

    /// Expires every issued access token (refresh tokens keep working).
    pub fn oauth_expire_access_tokens(&self) {
        self.shared.lock().oauth.expire_access_tokens();
    }

    /// Grant types redeemed at the token endpoint so far, oldest first.
    pub fn oauth_grants(&self) -> Vec<String> {
        self.shared.lock().oauth.grants.clone()
    }

    /// A rustls client configuration (ring provider) that trusts only this server's certificate.
    pub fn client_tls_config(&self) -> Result<Arc<rustls::ClientConfig>> {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(self.cert_der.clone()))
            .map_err(|e| Error::Tls(e.to_string()))?;
        let cfg =
            rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|e| Error::Tls(e.to_string()))?
                .with_root_certificates(roots)
                .with_no_client_auth();
        Ok(Arc::new(cfg))
    }

    /// Adds a local user (login name = address) with the default mailboxes INBOX, Drafts, Sent, Trash, Junk
    /// and Archive. An existing user gets the new password and keeps their mail.
    pub fn add_user(&self, address: &str, password: &str) -> Result<()> {
        self.shared.lock().add_user(address, password)?;
        self.shared.notify();
        Ok(())
    }

    /// Sets `user`'s storage limit in octets (`None` = unlimited), reported by IMAP GETQUOTAROOT (RFC 9208)
    /// and JMAP Quota/get (RFC 9425). Usage is the size of every stored message.
    pub fn set_quota(&self, user: &str, limit: Option<u64>) -> Result<()> {
        let mut st = self.shared.lock();
        let u = st.user_mut(user).ok_or_else(|| Error::UnknownUser(user.to_owned()))?;
        u.quota = limit;
        Ok(())
    }

    /// Creates a mailbox (and missing parents, `/` separated) for `user`.
    pub fn create_mailbox(&self, user: &str, name: &str, special: Option<SpecialUse>) -> Result<()> {
        self.shared.lock().create_mailbox(user, name, special)?;
        self.shared.notify();
        Ok(())
    }

    /// Stores a raw RFC 5322 message in `user`'s `mailbox` with the given flags (e.g. `\Seen`, `$Label1`) and
    /// an internal date of now. Returns the new UID. Sessions in IDLE are notified.
    pub fn seed_message(&self, user: &str, mailbox: &str, raw: &[u8], flags: &[&str]) -> Result<u32> {
        self.seed_message_at(user, mailbox, raw, flags, DateTime::now())
    }

    /// [`Self::seed_message`] with an explicit internal date.
    pub fn seed_message_at(
        &self,
        user: &str,
        mailbox: &str,
        raw: &[u8],
        flags: &[&str],
        internal_date: DateTime,
    ) -> Result<u32> {
        let flags: Vec<String> = flags.iter().map(|f| (*f).to_owned()).collect();
        let (_, uid) = self.shared.lock().append(user, mailbox, raw, &flags, internal_date)?;
        self.shared.notify();
        Ok(uid)
    }

    /// Snapshot of the messages in `user`'s `mailbox`, in UID order.
    pub fn messages(&self, user: &str, mailbox: &str) -> Result<Vec<StoredMessage>> {
        self.shared.lock().messages(user, mailbox)
    }

    /// Mailbox names of `user`, in creation order.
    pub fn mailboxes(&self, user: &str) -> Result<Vec<String>> {
        self.shared.lock().mailbox_names(user)
    }

    /// Every message the SMTP server accepted, oldest first.
    pub fn submissions(&self) -> Vec<Submission> {
        self.shared.lock().submissions.clone()
    }

    /// Lets `grantee` open `owner`'s mailboxes over IMAP as `Other Users/<owner>/…` (RFC 2342).
    pub fn grant_access(&self, owner: &str, grantee: &str) -> Result<()> {
        self.shared.lock().grant(owner, grantee)?;
        self.shared.notify();
        Ok(())
    }

    /// Makes `owner`'s mailboxes a shared mailbox every user can open as `Shared/<label>/…` (RFC 2342).
    pub fn share_mailbox(&self, label: &str, owner: &str) -> Result<()> {
        self.shared.lock().share(label, owner)?;
        self.shared.notify();
        Ok(())
    }

    /// The WebDAV root over HTTPS (`https://127.0.0.1:<webs_port>/dav/`): CardDAV and CalDAV discovery
    /// start here.
    pub fn dav_url(&self) -> String {
        format!("{}/dav/", self.web_url())
    }

    /// The HTTPS URL of `user`'s DAV collection `collection` (`contacts`, `calendar`, `tasks`, `public`).
    pub fn dav_collection_url(&self, user: &str, collection: &str) -> String {
        format!("{}/dav/{user}/{collection}/", self.web_url())
    }

    /// The resources of `user`'s DAV collection: (name, data), by name. Empty for an unknown collection.
    pub fn dav_items(&self, user: &str, collection: &str) -> Vec<(String, String)> {
        let mut d = self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner());
        d.ensure(user);
        d.collection(user, collection)
            .map(|c| {
                c.items
                    .iter()
                    .map(|(n, i)| (n.clone(), String::from_utf8_lossy(&i.data).into_owned()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Stores a resource in `user`'s DAV collection (as if another client wrote it). Returns its ETag.
    pub fn put_dav_item(&self, user: &str, collection: &str, name: &str, data: &str) -> Result<String> {
        let ctype = match collection {
            "contacts" => "text/vcard; charset=utf-8",
            "public" => "application/octet-stream",
            _ => "text/calendar; charset=utf-8",
        };
        let mut d = self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner());
        d.put(user, collection, name, data.as_bytes().to_vec(), ctype)
            .ok_or_else(|| Error::Invalid(format!("no DAV collection `{collection}`")))
    }

    /// Removes a resource from `user`'s DAV collection. True when it was there.
    pub fn delete_dav_item(&self, user: &str, collection: &str, name: &str) -> bool {
        let mut d = self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner());
        d.ensure(user);
        d.collection_mut(user, collection).is_some_and(|c| c.items.remove(name).is_some())
    }

    /// Makes the DAV server answer PUT with (`true`, the default) or without an ETag.
    pub fn set_dav_put_etags(&self, on: bool) {
        self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner()).put_without_etag = !on;
    }

    /// Stores a resource in `user`'s DAV collection right after the next PUT any client makes, as if another
    /// device created it while that client was synchronising.
    pub fn put_dav_item_after_next_put(&self, user: &str, collection: &str, name: &str, data: &str) {
        self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner()).after_next_put =
            Some((user.to_owned(), collection.to_owned(), name.to_owned(), data.to_owned()));
    }

    /// Adds an address book (`events: false`) or a calendar of events to `user`'s DAV home, under `name` (any
    /// characters: hrefs escape them).
    pub fn add_dav_collection(&self, user: &str, name: &str, events: bool, display: &str) {
        let kind = if events { dav::Kind::Calendar(&["VEVENT"]) } else { dav::Kind::AddressBook };
        self.inner
            .ctx
            .dav
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .add_collection(user, name, kind, display);
    }

    /// Moves `user`'s DAV collection `from` to `to`, with its items; requests for `from` are then redirected
    /// (301) to `to`, as by a server that moved its DAV tree. False when there is no such collection.
    pub fn move_dav_collection(&self, user: &str, from: &str, to: &str) -> bool {
        self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner()).move_collection(user, from, to)
    }

    /// Refuses (401) the next signed-in DAV request whose path starts with `path_prefix`, as a server that
    /// revoked the sign-in on its side only (the mail servers keep accepting it).
    pub fn refuse_dav_sign_in_once(&self, path_prefix: &str) {
        self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner()).refuse_once =
            Some(path_prefix.to_owned());
    }

    /// Adds `href` (`None`: nothing) to every DAV collection listing, as a server listing something that is
    /// not one of the collection's own resources.
    pub fn set_stray_dav_href(&self, href: Option<&str>) {
        self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner()).stray_href = href.map(str::to_owned);
    }

    /// DAV requests received ("METHOD /path"), oldest first.
    pub fn dav_requests(&self) -> Vec<String> {
        self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner()).requests.clone()
    }

    /// Stops all threads now (also done on drop).
    pub fn shutdown(self) {
        drop(self);
    }
}
