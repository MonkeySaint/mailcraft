//! MBOX (RFC 4155 family). The streaming reader/writer live in `mailcraft-store`; this module re-exports them
//! and adds in-memory import/export with a total size cap.

use std::io::{BufRead, Write};

use mailcraft_model::{DateTime, FolderId, MailItem};

pub use mailcraft_store::mbox::{
    MAX_MESSAGE_BYTES, MAX_MESSAGES, MboxMessage, MboxReader, parse_from_line, write_message,
};

use crate::Result;

/// Most message source (bytes) held by one [`read`].
pub const MAX_MBOX_BYTES: u64 = mailcraft_store::import::MAX_IMPORT_BYTES;

/// Result of reading an MBOX stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MboxImport {
    pub items: Vec<MailItem>,
    /// Messages skipped because they exceeded [`MAX_MESSAGE_BYTES`].
    pub skipped_oversized: usize,
    /// Reading stopped early at a count or byte cap.
    pub truncated: bool,
}

/// Reads every message; `received` comes from the `From ` line, read/flag state from `Status`/`X-Status`.
pub fn read<R: BufRead>(input: R, folder: FolderId, now: DateTime) -> Result<MboxImport> {
    read_capped(input, folder, now, MAX_MBOX_BYTES)
}

/// [`read`] holding at most `max_bytes` of message source.
pub fn read_capped<R: BufRead>(
    input: R,
    folder: FolderId,
    now: DateTime,
    max_bytes: u64,
) -> Result<MboxImport> {
    let mut reader = MboxReader::new(input);
    let mut out = MboxImport::default();
    let mut held = 0u64;
    while let Some(msg) = reader.next_message()? {
        if msg.oversized {
            out.skipped_oversized += 1;
            continue;
        }
        let len = u64::try_from(msg.data.len()).unwrap_or(u64::MAX);
        if held.saturating_add(len) > max_bytes {
            out.truncated = true;
            break;
        }
        held = held.saturating_add(len);
        let (_, delivered) = msg.envelope();
        let mut item = mailcraft_store::import::item_from_raw(&msg.data, folder, now);
        if let Some(t) = delivered {
            item.received = t;
        }
        mailcraft_store::import::apply_status_headers(&msg.data, &mut item);
        out.items.push(item);
    }
    if reader.hit_message_limit() {
        out.truncated = true;
    }
    Ok(out)
}

/// Reads MBOX bytes held in memory.
pub fn read_bytes(data: &[u8], folder: FolderId, now: DateTime) -> Result<MboxImport> {
    read(data, folder, now)
}

/// Writes items as mboxrd (source when kept, else rebuilt from fields).
pub fn write<W: Write>(out: &mut W, items: &[&MailItem]) -> Result<()> {
    for item in items {
        let data = crate::eml::write(item);
        let sender = item.from.as_ref().or(item.sender.as_ref()).map(|m| m.addr.as_str()).unwrap_or("");
        let date = if item.received.unix != 0 { item.received } else { item.sent.unwrap_or(item.received) };
        write_message(out, sender, date, &data)?;
    }
    Ok(())
}

/// [`write`] into a byte vector.
pub fn write_bytes(items: &[&MailItem]) -> Vec<u8> {
    let mut out = Vec::new();
    // Writing to a Vec cannot fail.
    let _ = write(&mut out, items);
    out
}
