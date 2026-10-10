//! The mailbox as one JSON document in browser storage.
//!
//! The browser build has no file system, so the whole [`Mailbox`] is written as a versioned JSON envelope under
//! [`STORAGE_KEY`]. Pure functions only (no browser calls), so they are tested on the native target.

use serde::{Deserialize, Serialize};

use mailcraft_engine::model::Mailbox;

/// Browser storage key holding the mailbox.
pub const STORAGE_KEY: &str = "mailcraft.mailbox";
/// Envelope format tag; a stored document with another tag is ignored (the sample mailbox opens instead).
pub const FORMAT: &str = "mailcraft-web/1";
/// Largest stored document read back (characters). Browsers cap storage at a few megabytes per site anyway;
/// anything bigger did not come from us.
pub const MAX_STORED_CHARS: usize = 32 << 20;

#[derive(Serialize)]
struct EnvelopeRef<'a> {
    format: &'a str,
    mailbox: &'a Mailbox,
}

#[derive(Deserialize)]
struct Envelope {
    format: String,
    mailbox: Mailbox,
}

/// The stored form of `mailbox`.
pub fn encode(mailbox: &Mailbox) -> Result<String, String> {
    serde_json::to_string(&EnvelopeRef { format: FORMAT, mailbox })
        .map_err(|e| format!("could not encode the mailbox: {e}"))
}

/// Reads a stored document back. Errors describe why it was not usable (too big, not JSON, other format).
pub fn decode(text: &str) -> Result<Mailbox, String> {
    if text.len() > MAX_STORED_CHARS {
        return Err(format!("the saved mailbox is too large ({} bytes)", text.len()));
    }
    let env: Envelope =
        serde_json::from_str(text).map_err(|e| format!("the saved mailbox could not be read: {e}"))?;
    if env.format != FORMAT {
        let shown: String = env.format.chars().take(40).collect();
        return Err(format!("the saved mailbox has an unknown format `{shown}`"));
    }
    Ok(env.mailbox)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_engine::sample::{sample_mailbox, sample_now};

    #[test]
    fn round_trip() {
        let mb = sample_mailbox(sample_now());
        let text = encode(&mb).unwrap();
        assert_eq!(decode(&text).unwrap(), mb);
    }

    #[test]
    fn sample_fits_in_browser_storage() {
        // Browsers allow roughly 5 million characters per site; keep the sample well under that.
        let text = encode(&sample_mailbox(sample_now())).unwrap();
        assert!(text.len() < 4_000_000, "sample mailbox is {} bytes", text.len());
    }

    #[test]
    fn rejects_junk() {
        assert!(decode("").is_err());
        assert!(decode("not json").is_err());
        assert!(decode("{}").is_err());
        assert!(decode(r#"{"format":"other/9","mailbox":{}}"#).is_err());
        assert!(decode(&"[".repeat(10_000)).is_err());
        let huge = " ".repeat(MAX_STORED_CHARS + 1);
        assert!(decode(&huge).is_err());
        // Truncated documents (a write cut short) are refused, not half-loaded.
        let text = encode(&sample_mailbox(sample_now())).unwrap();
        let cut = text.get(..text.len() / 2).unwrap();
        assert!(decode(cut).is_err());
    }
}
