//! MailCraft protocol clients: IMAP (RFC 9051/3501 and sync extensions), SMTP submission (RFC 5321/6409),
//! JMAP (RFC 8620/8621) with push over server-sent events, Microsoft Graph (REST v1.0: mail folders, messages,
//! delta queries, sending), OAuth 2.0 (RFC 6749/8628/7636), an IMAP folder
//! synchroniser, WebDAV with CardDAV (RFC 6352) and CalDAV (RFC 4791) for contacts and tasks, and account
//! autodiscovery (autoconfig documents, DNS SRV per RFC 6186, well-known providers by domain or DNS MX),
//! server-side search criteria (IMAP SEARCH, JMAP Email/query filters) and a LibreTranslate-compatible
//! translation client.
//!
//! Blocking I/O over `std::net` with rustls (ring provider) for TLS; no async runtime. Every function
//! returns [`NetError`] on failure; hostile server input never panics.

pub mod autoconfig;
pub mod dav;
pub mod discover;
pub mod dns;
pub mod error;
pub mod eventsource;
pub mod ews;
pub mod graph;
pub mod http;
pub mod image_fetch;
pub mod imap;
pub mod jmap;
pub mod keychain;
pub mod oauth;
pub mod pop3;
pub mod providers;
pub mod sasl;
pub mod search;
pub mod smtp;
pub mod sync;
pub mod translate;
pub mod transport;

pub use error::{NetError, Result};
pub use mailcraft_model::Security;
pub use sasl::Credentials;
pub use transport::{ConnectOptions, TlsOptions};
