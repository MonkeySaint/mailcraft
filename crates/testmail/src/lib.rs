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
//! Each protocol listens on a plain port (with STARTTLS) and an implicit-TLS port. TLS uses a self-signed
//! certificate for `localhost`/`127.0.0.1` generated at start; [`TestServers::cert_pem`] /
//! [`TestServers::cert_der`] let test clients trust it. All state is in memory and shared by both servers.
//! Dropping [`TestServers`] stops every thread.

mod conn;
mod date;
mod imap;
mod mime;
mod server;
mod smtp;
mod store;
mod tls;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

pub use date::DateTime;
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
    /// Users created at start: (address, password).
    pub users: Vec<(String, String)>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hostname: "localhost".to_owned(),
            imap_port: 0,
            imaps_port: 0,
            smtp_port: 0,
            smtps_port: 0,
            max_message_size: 25 * 1024 * 1024,
            reject_recipients: Vec::new(),
            smtp_require_auth: true,
            auth_requires_tls: false,
            add_trace_headers: true,
            idle_timeout: Duration::from_secs(30 * 60),
            users: Vec::new(),
        }
    }
}

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
        let running = server::Running::start(config, Arc::clone(&shared), identity.server_config()?)?;
        Ok(TestServers {
            imap_port: running.imap.port(),
            imaps_port: running.imaps.port(),
            smtp_port: running.smtp.port(),
            smtps_port: running.smtps.port(),
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

    /// Stops all threads now (also done on drop).
    pub fn shutdown(self) {
        drop(self);
    }
}
