//! MailCraft protocol clients: IMAP (RFC 9051/3501 and sync extensions), SMTP submission (RFC 5321/6409),
//! JMAP (RFC 8620/8621), OAuth 2.0 (RFC 6749/8628/7636), and an IMAP folder synchroniser.
//!
//! Blocking I/O over `std::net` with rustls (ring provider) for TLS; no async runtime. Every function
//! returns [`NetError`] on failure; hostile server input never panics.

pub mod error;
pub mod http;
pub mod imap;
pub mod jmap;
pub mod keychain;
pub mod oauth;
pub mod pop3;
pub mod sasl;
pub mod smtp;
pub mod sync;
pub mod transport;

pub use error::{NetError, Result};
pub use mailcraft_model::Security;
pub use sasl::Credentials;
pub use transport::{ConnectOptions, TlsOptions};
