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
//! - a fake Microsoft Graph mail service (REST v1.0 shapes: mail folders, per-folder message delta queries with
//!   paging, `$value`, sendMail, drafts, attachments with upload sessions, throttling on demand) on the web
//!   server's ports under `/graph/v1.0` (`graph.rs`), over the same mail.
//!
//! - an OAuth 2.0 authorization server (device grant, authorization code with PKCE, refresh tokens) whose
//!   access tokens sign in to IMAP and SMTP (XOAUTH2, OAUTHBEARER) and JMAP (Bearer), plus autoconfig
//!   documents and tenant OpenID configurations, all on a small web server (HTTP and HTTPS); and a fake DNS
//!   responder (UDP) answering SRV and MX questions from records the test sets. Nothing is ever forwarded to
//!   a real server.
//!
//! - a fake Exchange Web Services endpoint (SOAP: folders, SyncFolderItems, MIME items, sending, contacts,
//!   events, tasks, automatic replies, availability; Basic, Bearer and NTLM sign-in) and Autodiscover on
//!   the web server's ports (`ews.rs`).
//!
//! Each protocol listens on a plain port (with STARTTLS) and an implicit-TLS port. TLS uses a self-signed
//! certificate for `localhost`/`127.0.0.1` generated at start; [`TestServers::cert_pem`] /
//! [`TestServers::cert_der`] let test clients trust it. All state is in memory and shared by both servers.
//! Dropping [`TestServers`] stops every thread.

mod conn;
mod date;
mod dav;
mod dns;
pub mod ews;
mod graph;
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
pub use dns::{Mx, Srv};
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
    /// JMAP offers contacts (RFC 9610: one address book `AB1`, ContactCard/get, /query, /set) and calendars
    /// (JMAP for Calendars: one calendar `CAL1`, CalendarEvent/get, /query, /set). The objects are kept as
    /// given (JSContact / JSCalendar JSON); tests read and change them through [`TestServers`].
    pub jmap_pim: bool,
    /// JMAP Mailbox/set refuses a create that sets a `role` (`invalidProperties`), as servers that leave
    /// roles to themselves.
    pub jmap_refuse_roles: bool,
    /// Largest page the fake Graph service sends (the client's `Prefer: odata.maxpagesize` is honoured up to
    /// this), so paging is exercised with little mail.
    pub graph_max_page: usize,
    /// Exchange Web Services accept only NTLM sign-ins (Basic is refused and not offered).
    pub ews_ntlm_only: bool,
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
            jmap_pim: false,
            jmap_refuse_roles: false,
            graph_max_page: 50,
            ews_ntlm_only: false,
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

    /// The EWS endpoint over HTTPS (`https://127.0.0.1:<webs_port>/EWS/Exchange.asmx`).
    pub fn ews_url(&self) -> String {
        format!("{}/EWS/Exchange.asmx", self.web_url())
    }

    /// Autodiscover location templates (`{domain}` placeholder) pointing at this server, standing in for
    /// `https://<domain>/autodiscover/autodiscover.xml` and `https://autodiscover.<domain>/…`.
    pub fn autodiscover_url_templates(&self) -> Vec<String> {
        let base = self.web_url();
        vec![
            format!("{base}/{{domain}}/autodiscover/autodiscover.xml"),
            format!("{base}/autodiscover.{{domain}}/autodiscover/autodiscover.xml"),
        ]
    }

    /// Makes Autodiscover answer (signed in) for addresses of `domain`, or stop answering (404).
    pub fn set_autodiscover(&self, domain: &str, on: bool) {
        let mut st = self.ews_state();
        let d = domain.trim().to_ascii_lowercase();
        st.autodiscover.retain(|x| *x != d);
        if on {
            st.autodiscover.push(d);
        }
    }

    fn ews_state(&self) -> std::sync::MutexGuard<'_, ews::EwsState> {
        self.inner.ctx.ews.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stores an EWS item (the XML of a `CalendarItem`, `Contact` or `Task` element) in `user`'s `folder`
    /// (`calendar`, `contacts` or `tasks`); its item id, `None` when the XML does not parse.
    pub fn ews_put_item(&self, user: &str, folder: &str, xml: &str) -> Option<String> {
        ews::put_item(&mut self.ews_state(), user, folder, xml)
    }

    /// The EWS items of `user`'s `folder`: (item id, XML).
    pub fn ews_items(&self, user: &str, folder: &str) -> Vec<(String, String)> {
        ews::items(&self.ews_state(), user, folder)
    }

    /// Sets field `name` of an EWS item to `text` (as another client's edit); false for an unknown item.
    pub fn ews_set_item_field(&self, user: &str, id: &str, name: &str, text: &str) -> bool {
        ews::set_item_field(&mut self.ews_state(), user, id, name, text)
    }

    /// Removes an EWS item (as another client would).
    pub fn ews_remove_item(&self, user: &str, id: &str) -> bool {
        ews::remove_item(&mut self.ews_state(), user, id)
    }

    /// `user`'s automatic-reply settings as last set over EWS.
    pub fn ews_oof(&self, user: &str) -> Option<ews::Oof> {
        self.ews_state().oof.get(&user.trim().to_ascii_lowercase()).cloned()
    }

    /// Meeting answers sent over EWS: (user, `AcceptItem`/`TentativelyAcceptItem`/`DeclineItem`, item id).
    pub fn ews_replies(&self) -> Vec<(String, String, String)> {
        self.ews_state().replies.clone()
    }

    /// Makes EWS refuse to store messages (CreateItem SaveOnly) larger than `bytes` with
    /// ErrorMessageSizeExceeded (`None`: no limit).
    pub fn ews_set_max_upload(&self, bytes: Option<usize>) {
        self.ews_state().max_upload = bytes;
    }

    /// EWS operations served, oldest first, each with how it was signed in ("GetFolder basic",
    /// "SyncFolderItems ntlm", "GetItem conn" for a request on an NTLM-signed-in connection; Autodiscover
    /// requests as "autodiscover auth=<scheme or none>").
    pub fn ews_log(&self) -> Vec<String> {
        self.ews_state().log.clone()
    }

    /// Requests the web server received ("GET /path?query"), oldest first.
    pub fn web_requests(&self) -> Vec<String> {
        self.inner.ctx.web.lock().unwrap_or_else(|e| e.into_inner()).requests.clone()
    }

    /// Sets the SRV records the fake DNS responder answers for `name` (e.g. `_imaps._tcp.example.com`).
    pub fn set_srv(&self, name: &str, records: Vec<Srv>) {
        self.dns_entry(name, |e| e.srv = records);
    }

    /// Sets the MX records the fake DNS responder answers for the domain `name` (an exchange of "" is the
    /// RFC 7505 null MX).
    pub fn set_mx(&self, name: &str, records: Vec<Mx>) {
        self.dns_entry(name, |e| e.mx = records);
    }

    /// Makes the fake DNS responder answer `name` with a truncated (TC) empty message.
    pub fn set_srv_truncated(&self, name: &str) {
        self.dns_entry(name, |e| e.truncated = true);
    }

    fn dns_entry(&self, name: &str, f: impl FnOnce(&mut dns::Entry)) {
        let key = name.trim().trim_end_matches('.').to_ascii_lowercase();
        f(self.inner.ctx.dns.lock().unwrap_or_else(|e| e.into_inner()).entry(key).or_default());
    }

    /// The tenant lookup URL template (`{domain}` placeholder) pointing at this server, standing in for
    /// `https://login.microsoftonline.com/{domain}/v2.0/.well-known/openid-configuration`.
    pub fn tenant_url_template(&self) -> String {
        format!("{}/tenant/{{domain}}/v2.0/.well-known/openid-configuration", self.web_url())
    }

    /// Makes `domain` a Microsoft 365-style tenant's with the tenant id `tenant` (a GUID): its OpenID
    /// configuration is served at [`Self::tenant_url_template`]. `None` removes it (the server then answers
    /// 400 with an `invalid_tenant` error, as the real one does for unknown domains).
    pub fn set_tenant(&self, domain: &str, tenant: Option<&str>) {
        let mut w = self.inner.ctx.web.lock().unwrap_or_else(|e| e.into_inner());
        let key = domain.trim().to_ascii_lowercase();
        match tenant {
            Some(t) => w.tenants.insert(key, t.to_owned()),
            None => w.tenants.remove(&key),
        };
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

    /// `user`'s JMAP contacts or events (`kind` = `ContactCard` or `CalendarEvent`), as stored (with their
    /// `id`).
    pub fn jmap_objects(&self, user: &str, kind: &str) -> Vec<serde_json::Value> {
        self.inner.ctx.jmap.lock().unwrap_or_else(|e| e.into_inner()).objects(user, kind)
    }

    /// Stores a JMAP contact or event for `user` as another client would (its address book / calendar
    /// membership is set to the only one when missing); returns its id.
    pub fn put_jmap_object(&self, user: &str, kind: &str, obj: serde_json::Value) -> String {
        self.inner.ctx.jmap.lock().unwrap_or_else(|e| e.into_inner()).put_object(user, kind, obj)
    }

    /// Replaces top-level properties of a stored JMAP object (`null` removes one). False when there is no
    /// such object.
    pub fn update_jmap_object(&self, user: &str, kind: &str, id: &str, patch: serde_json::Value) -> bool {
        self.inner.ctx.jmap.lock().unwrap_or_else(|e| e.into_inner()).update_object(user, kind, id, &patch)
    }

    /// Removes a stored JMAP object. False when there was none.
    pub fn delete_jmap_object(&self, user: &str, kind: &str, id: &str) -> bool {
        self.inner.ctx.jmap.lock().unwrap_or_else(|e| e.into_inner()).delete_object(user, kind, id)
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

    /// Deletes `user`'s mailbox `name` (with its messages), as a server set up without it would be.
    pub fn delete_mailbox(&self, user: &str, name: &str) -> Result<()> {
        self.shared.lock().delete_mailbox(user, name)?;
        self.shared.notify();
        Ok(())
    }

    /// The RFC 6154 special use of `user`'s mailbox `name` (`None`: none, or no such mailbox).
    pub fn special_use(&self, user: &str, name: &str) -> Option<SpecialUse> {
        self.shared.lock().user(user)?.mailbox(name)?.special
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

    /// Makes the DAV server store each new resource under a name of its own choosing (`srv-<n>.vcf`) instead of
    /// the one the client PUT, as some servers do.
    pub fn set_dav_rename_new(&self, on: bool) {
        self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner()).rename_new = on;
    }

    /// Gives every resource written the same ETag `etag` (`None`: a new one per write), as servers whose ETag
    /// is a version counter starting alike for each new resource.
    pub fn set_dav_constant_etag(&self, etag: Option<&str>) {
        self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner()).constant_etag = etag.map(str::to_owned);
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

    /// Gives every user one calendar for events and tasks (`calendar`, taking VEVENT and VTODO) and no
    /// `tasks` collection, as servers that set up one default calendar per user. Call before the users' DAV
    /// collections are first used.
    pub fn set_single_dav_calendar(&self, on: bool) {
        self.inner.ctx.dav.lock().unwrap_or_else(|e| e.into_inner()).single_calendar = on;
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

    /// The fake Microsoft Graph API base over HTTPS (`https://127.0.0.1:<webs_port>/graph/v1.0`).
    pub fn graph_url(&self) -> String {
        format!("{}/graph/v1.0", self.web_url())
    }

    /// Makes the next `count` Graph requests answer 429 Too Many Requests with `Retry-After: retry_after`
    /// (seconds).
    pub fn graph_throttle(&self, count: u32, retry_after: u64) {
        let mut g = self.inner.ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
        g.throttle = count;
        g.retry_after = retry_after;
    }

    /// While `on`, Graph refuses to open attachment upload sessions (413, as for a file over its limit).
    pub fn graph_refuse_uploads(&self, on: bool) {
        self.inner.ctx.graph.lock().unwrap_or_else(|e| e.into_inner()).refuse_uploads = on;
    }

    /// Graph requests received ("METHOD /path?query"), oldest first.
    pub fn graph_requests(&self) -> Vec<String> {
        self.inner.ctx.graph.lock().unwrap_or_else(|e| e.into_inner()).requests.clone()
    }

    /// The Graph message id of the message with `uid` in `user`'s `mailbox`.
    pub fn graph_message_id(&self, user: &str, mailbox: &str, uid: u32) -> Option<String> {
        let store = self.shared.lock();
        let m = store.user(user)?.mailbox(mailbox)?.find(uid)?;
        let g = self.inner.ctx.graph.lock().unwrap_or_else(|e| e.into_inner());
        Some(g.message_id(user, &m.raw, &m.internal_date))
    }

    /// Changes what Graph keeps for message `id` beyond the IMAP flags, as another client would:
    /// `{"categories": [..], "importance": "high", "flag": "complete", "inferenceClassification": "other"}`.
    pub fn graph_set_meta(&self, user: &str, id: &str, patch: serde_json::Value) {
        self.inner.ctx.graph.lock().unwrap_or_else(|e| e.into_inner()).set_meta(user, id, &patch);
        self.shared.notify();
    }

    /// What Graph keeps for message `id` beyond the IMAP flags: `{"categories", "importance", "complete",
    /// "other"}`.
    pub fn graph_meta(&self, user: &str, id: &str) -> serde_json::Value {
        self.inner.ctx.graph.lock().unwrap_or_else(|e| e.into_inner()).meta_json(user, id)
    }

    /// Stops all threads now (also done on drop).
    pub fn shutdown(self) {
        drop(self);
    }
}
