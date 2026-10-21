//! Internet message format (layer L0).
//!
//! Parses and builds RFC 5322 messages with MIME bodies. Specs: RFC 5322 (message format, addresses, dates),
//! RFC 2045–2049 (MIME), RFC 2047 (encoded words), RFC 2231 (parameter continuations / charsets), RFC 2392
//! (Content-ID), RFC 6532 (UTF-8 headers). Every parser is total: hostile input yields an error or a best-effort
//! value, never a panic. Size caps: [`MAX_MESSAGE_BYTES`], [`MAX_PARTS`], [`MAX_DEPTH`].
//!
//! Interface (frozen for commit 2; implementations may add items but not rename or remove these):
//! - [`parse`] bytes → [`Message`]; [`Message`] accessors for the common headers and bodies.
//! - [`addr`]: [`Mailbox`], [`Address`], `parse_address_list`, `format_address_list`.
//! - [`date`]: [`DateTime`] (UTC instant + original offset), RFC 5322 parse/format, civil-date helpers.
//! - [`encoded`]: RFC 2047 decode/encode; [`charset`]: bytes + label → `String`.
//! - [`build`]: [`MessageBuilder`] → RFC 5322 bytes (CRLF, folded, encoded words, base64 / quoted-printable).

pub mod addr;
pub mod build;
pub mod charset;
pub mod date;
pub mod encoded;
pub mod header;
pub mod idn;
pub mod message;
pub mod params;
pub mod transfer;

pub use addr::{Address, Mailbox, format_address_list, parse_address_list};
pub use build::{AttachmentSpec, MessageBuilder};
pub use date::{Civil, DateTime, Weekday};
pub use header::{Header, Headers};
pub use message::{Attachment, ContentType, Disposition, Message, Part, PartBody, parse};

/// Largest message the parser accepts (bytes).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Largest number of MIME parts in one message.
pub const MAX_PARTS: usize = 2_000;
/// Deepest multipart / message/rfc822 nesting.
pub const MAX_DEPTH: usize = 32;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum MimeError {
    #[error("message too large ({0} bytes)")]
    TooLarge(usize),
    #[error("malformed message: {0}")]
    Malformed(String),
    #[error("invalid address: {0}")]
    InvalidAddress(String),
    #[error("invalid date: {0}")]
    InvalidDate(String),
}

pub type Result<T> = std::result::Result<T, MimeError>;

#[cfg(test)]
mod tests;
