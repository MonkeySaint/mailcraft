//! The crate's error type. Every protocol client returns [`NetError`]; nothing in this crate panics on
//! hostile server input.

use std::io;

/// Errors from the protocol clients.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    /// The TCP connection could not be made (DNS, refused, unreachable).
    #[error("cannot connect: {0}")]
    Connect(String),
    /// A read, write or connect did not finish in time.
    #[error("timed out: {0}")]
    Timeout(String),
    /// The peer closed the connection.
    #[error("connection closed by the server")]
    Closed,
    /// Any other socket error.
    #[error("network error: {0}")]
    Io(String),
    /// TLS setup or handshake failed (bad certificate, protocol mismatch).
    #[error("TLS error: {0}")]
    Tls(String),
    /// The server rejected the credentials, or no usable mechanism was offered.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The server sent something this client cannot parse.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The server refused a command (IMAP `NO`, SMTP 4xx/5xx outside sending).
    #[error("the server refused the command: {0}")]
    Refused(String),
    /// The server said the command was malformed (IMAP `BAD`).
    #[error("the server rejected the command as invalid: {0}")]
    Bad(String),
    /// The server lacks a required capability.
    #[error("not supported by the server: {0}")]
    Unsupported(String),
    /// A response exceeded a size cap.
    #[error("response too large: {0}")]
    TooLarge(String),
    /// An HTTP request finished with a non-success status.
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    /// A JMAP method returned an error object.
    #[error("JMAP {kind}: {description}")]
    Jmap { kind: String, description: String },
    /// A Microsoft Graph request failed with an error object (`{"error": {"code", "message"}}`).
    #[error("Microsoft Graph {code} (HTTP {status}): {message}")]
    Graph { status: u16, code: String, message: String },
    /// An Exchange Web Services operation failed: its response code (`ErrorItemNotFound`, …) and message
    /// ([MS-OXWSCDATA] ResponseCodeType), or a SOAP fault.
    #[error("Exchange {code}: {message}")]
    Ews { code: String, message: String },
    /// The OAuth server returned an error.
    #[error("OAuth {error}: {description}")]
    OAuth { error: String, description: String },
    /// Refused locally: credentials would cross an unencrypted connection. Use TLS or STARTTLS, or set
    /// [`crate::ConnectOptions::allow_plaintext_auth`] for a server the user explicitly trusts.
    #[error("refusing to send credentials over an unencrypted connection: {0}")]
    Insecure(String),
    /// A caller-supplied value cannot be sent (bad URL, CR/LF in an address, …).
    #[error("invalid input: {0}")]
    Invalid(String),
}

impl NetError {
    /// True for errors after which the connection should be dropped and re-established.
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, NetError::Closed | NetError::Io(_) | NetError::Timeout(_) | NetError::Tls(_))
    }
}

impl From<io::Error> for NetError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => NetError::Timeout(e.to_string()),
            io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe => NetError::Closed,
            io::ErrorKind::InvalidData => {
                // rustls reports TLS failures as InvalidData on the stream.
                NetError::Tls(e.to_string())
            }
            _ => NetError::Io(e.to_string()),
        }
    }
}

impl From<rustls::Error> for NetError {
    fn from(e: rustls::Error) -> Self {
        NetError::Tls(e.to_string())
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, NetError>;
