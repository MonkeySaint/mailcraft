//! `.eml`: one RFC 5322 message per file.
//!
//! Reading goes through `mailcraft-mime` (via the store's import helper, which falls back to a header scan so
//! no message is lost). Writing emits the message's source when it has one, else rebuilds the message from
//! the item's fields and its body (attachment content is not kept on items and cannot be rebuilt). Items hold
//! neither source nor body (storage stage S5): callers pass what the body store holds for them.

use std::path::Path;

use mailcraft_model::{Body, DateTime, FolderId, MailItem};

use crate::{Result, check_size};

/// Largest `.eml` accepted (the MIME parser's own cap).
pub const MAX_EML_BYTES: usize = mailcraft_mime::MAX_MESSAGE_BYTES;

/// Builds an item from message bytes (`data` is its source; the caller stores it). `now` is the received time
/// when the message carries no date.
pub fn read(data: &[u8], folder: FolderId, now: DateTime) -> Result<MailItem> {
    check_size("eml", data.len(), MAX_EML_BYTES)?;
    Ok(mailcraft_store::import::item_from_raw(data, folder, now))
}

/// Reads an `.eml` file (size-capped): its item and its source. Without a Date or Received header the file
/// time is used.
pub fn read_file(path: &Path, folder: FolderId) -> Result<(MailItem, Vec<u8>)> {
    let len = std::fs::metadata(path)?.len();
    check_size("eml", usize::try_from(len).unwrap_or(usize::MAX), MAX_EML_BYTES)?;
    mailcraft_store::import::read_eml(path, folder).map_err(|e| crate::FormatError::Io(e.to_string()))
}

/// RFC 5322 bytes for an item: its `source` when it has one, else rebuilt from its fields and `body`.
pub fn write(item: &MailItem, source: Option<&[u8]>, body: Option<&Body>) -> Vec<u8> {
    match source {
        Some(raw) if !raw.is_empty() => raw.to_vec(),
        _ => mailcraft_store::rebuild_message(item, body),
    }
}

/// Writes [`write`]'s bytes to `path`.
pub fn write_file(item: &MailItem, source: Option<&[u8]>, body: Option<&Body>, path: &Path) -> Result<()> {
    std::fs::write(path, write(item, source, body))?;
    Ok(())
}
