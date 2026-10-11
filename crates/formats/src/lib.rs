//! File formats (layer L3).
//!
//! Readers and writers for the files a desktop mail client opens, saves, imports and exports:
//! - [`eml`]: one RFC 5322 message per file (via `mailcraft-mime`; rebuilt from the item when no source is kept).
//! - [`mbox`]: mboxrd/mboxo mailboxes (the reader and writer live in `mailcraft-store`; this wraps them for
//!   in-memory import/export with caps).
//! - [`msg`]: MSG item files per [MS-OXMSG] stored in a Compound File Binary container ([MS-CFB], via the
//!   MIT-licensed `cfb` crate), compressed RTF per [MS-OXRTFCP] and HTML de-encapsulation per [MS-OXRTFEX].
//! - [`ics`]: iCalendar (RFC 5545) with recurrence expansion, time zones and iTIP methods (RFC 5546).
//! - [`vcf`]: vCard 4.0 (RFC 6350) and 3.0 (RFC 2426), lenient with 2.1.
//! - [`csv`]: RFC 4180 CSV and the contacts import/export column mapping.
//!
//! Every reader is total: hostile input produces an error or a best-effort value, never a panic. Each format
//! has an input size cap and bounded counts (components, properties, recursion, occurrences).

pub mod csv;
pub mod eml;
pub mod ics;
pub mod mbox;
pub mod msg;
pub mod vcf;

mod text;

#[cfg(test)]
mod tests;

use std::path::Path;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum FormatError {
    #[error("{format} input too large ({size} bytes; the limit is {limit})")]
    TooLarge { format: &'static str, size: usize, limit: usize },
    #[error("malformed {format}: {msg}")]
    Malformed { format: &'static str, msg: String },
    #[error("unsupported {format}: {msg}")]
    Unsupported { format: &'static str, msg: String },
    #[error("i/o: {0}")]
    Io(String),
}

impl From<std::io::Error> for FormatError {
    fn from(e: std::io::Error) -> Self {
        FormatError::Io(e.to_string())
    }
}

impl FormatError {
    pub(crate) fn malformed(format: &'static str, msg: impl Into<String>) -> Self {
        FormatError::Malformed { format, msg: msg.into() }
    }
}

pub type Result<T> = std::result::Result<T, FormatError>;

/// Rejects inputs over `limit` bytes.
pub(crate) fn check_size(format: &'static str, size: usize, limit: usize) -> Result<()> {
    if size > limit { Err(FormatError::TooLarge { format, size, limit }) } else { Ok(()) }
}

/// The file formats this crate reads and writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Eml,
    Mbox,
    Msg,
    Ics,
    Vcf,
    Csv,
}

impl Format {
    /// Usual file extension (without the dot).
    pub fn extension(self) -> &'static str {
        match self {
            Format::Eml => "eml",
            Format::Mbox => "mbox",
            Format::Msg => "msg",
            Format::Ics => "ics",
            Format::Vcf => "vcf",
            Format::Csv => "csv",
        }
    }

    /// Media type used when the file is attached to a message.
    pub fn mime_type(self) -> &'static str {
        match self {
            Format::Eml => "message/rfc822",
            Format::Mbox => "application/mbox",
            Format::Msg => "application/vnd.ms-outlook",
            Format::Ics => "text/calendar",
            Format::Vcf => "text/vcard",
            Format::Csv => "text/csv",
        }
    }

    /// Format from a file name's extension (case-insensitive).
    pub fn from_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "eml" | "mht" | "mhtml" => Format::Eml,
            "mbox" | "mbx" | "mbs" => Format::Mbox,
            "msg" => Format::Msg,
            "ics" | "ical" | "ifb" | "icalendar" | "vcs" => Format::Ics,
            "vcf" | "vcard" => Format::Vcf,
            "csv" => Format::Csv,
            _ => return None,
        })
    }

    /// Format guessed from the first bytes of a file.
    pub fn sniff(data: &[u8]) -> Option<Format> {
        if data.starts_with(&msg::CFB_SIGNATURE) {
            return Some(Format::Msg);
        }
        let head = data.get(..data.len().min(4096)).unwrap_or(data);
        let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
        let text = String::from_utf8_lossy(head);
        let trimmed = text.trim_start();
        let upper_start: String = trimmed.chars().take(16).collect::<String>().to_ascii_uppercase();
        if upper_start.starts_with("BEGIN:VCALENDAR") {
            return Some(Format::Ics);
        }
        if upper_start.starts_with("BEGIN:VCARD") {
            return Some(Format::Vcf);
        }
        if trimmed.starts_with("From ") {
            return Some(Format::Mbox);
        }
        let first_line = trimmed.lines().next().unwrap_or("");
        if let Some((name, _)) = first_line.split_once(':')
            && !name.is_empty()
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Some(Format::Eml);
        }
        if first_line.contains(',') || first_line.contains(';') || first_line.contains('\t') {
            return Some(Format::Csv);
        }
        None
    }
}
