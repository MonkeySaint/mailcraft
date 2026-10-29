//! The mailbox as one JSON document in browser storage.
//!
//! The browser build has no file system, so the whole [`Mailbox`] is written as a versioned JSON envelope under
//! [`STORAGE_KEY`], together with the message content its items refer to (sources and body records, which
//! live in the session's [`BodyStore`] rather than in the items). Pure functions only (no browser calls), so
//! they are tested on the native target.
//!
//! - `mailcraft-web/2` (written): `{"format":…,"mailbox":{…},"content":{"sources":{key: text},"bodies":{…}}}`.
//!   A source is kept as text with one character per byte (U+0000 to U+00FF), so 8-bit mail survives JSON
//!   without growing the way a list of numbers would.
//! - `mailcraft-web/1` (read): the mailbox with each item's body and source inline. They move into the body
//!   store when it is read.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use mailcraft_engine::model::{Body, Mailbox};
use mailcraft_store::BodyStore;
use mailcraft_store::bodies::{Exported, keys_of};

/// Browser storage key holding the mailbox.
pub const STORAGE_KEY: &str = "mailcraft.mailbox";
/// Envelope format tag written; a stored document with an unknown tag is ignored (the sample mailbox opens
/// instead).
pub const FORMAT: &str = "mailcraft-web/2";
/// The format before message content moved out of the items (still read).
pub const FORMAT_V1: &str = "mailcraft-web/1";
/// Largest stored document read back (characters). Browsers cap storage at a few megabytes per site anyway;
/// anything bigger did not come from us.
pub const MAX_STORED_CHARS: usize = 32 << 20;

#[derive(Serialize)]
struct EnvelopeRef<'a> {
    format: &'a str,
    mailbox: &'a Mailbox,
    content: Stored,
}

#[derive(Deserialize)]
struct Envelope {
    format: String,
    mailbox: Mailbox,
    #[serde(default)]
    content: Stored,
}

/// Message content as stored: sources as byte text (see the module notes), body records as they are.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Stored {
    sources: BTreeMap<String, String>,
    bodies: BTreeMap<String, Body>,
}

/// One character per byte.
fn bytes_to_text(raw: &[u8]) -> String {
    raw.iter().map(|&b| char::from(b)).collect()
}

/// The bytes [`bytes_to_text`] made; `None` for text it cannot have made (a character past U+00FF).
fn text_to_bytes(text: &str) -> Option<Vec<u8>> {
    text.chars().map(|c| u8::try_from(u32::from(c)).ok()).collect()
}

/// The stored form of `mailbox` and the content of its items in `bodies`. Refused when [`decode`] would refuse
/// it (larger than [`MAX_STORED_CHARS`]): the document already stored stays rather than being replaced by one
/// that cannot be read back (issue #25).
pub fn encode(mailbox: &Mailbox, bodies: &BodyStore) -> Result<String, String> {
    let exported = bodies.export(&keys_of(mailbox.mail.items()));
    let content = Stored {
        sources: exported.sources.into_iter().map(|(k, raw)| (k, bytes_to_text(&raw))).collect(),
        bodies: exported.bodies,
    };
    let text = serde_json::to_string(&EnvelopeRef { format: FORMAT, mailbox, content })
        .map_err(|e| format!("could not encode the mailbox: {e}"))?;
    check_size(&text, MAX_STORED_CHARS)?;
    Ok(text)
}

fn check_size(text: &str, cap: usize) -> Result<(), String> {
    if text.len() > cap {
        return Err(format!(
            "the mailbox is too large to keep in browser storage ({} bytes, at most {cap})",
            text.len()
        ));
    }
    Ok(())
}

/// Reads a stored document back: the mailbox and an in-memory body store with its content. Errors describe
/// why it was not usable (too big, not JSON, other format). Content whose bytes do not match its key is
/// dropped (the message then shows without a body) rather than refusing the whole mailbox.
pub fn decode(text: &str) -> Result<(Mailbox, BodyStore), String> {
    if text.len() > MAX_STORED_CHARS {
        return Err(format!("the saved mailbox is too large ({} bytes)", text.len()));
    }
    let env: Envelope =
        serde_json::from_str(text).map_err(|e| format!("the saved mailbox could not be read: {e}"))?;
    let mut mailbox = env.mailbox;
    let mut bodies = BodyStore::in_memory();
    match env.format.as_str() {
        FORMAT => {
            let sources = env
                .content
                .sources
                .into_iter()
                .filter_map(|(k, t)| text_to_bytes(&t).map(|raw| (k, raw)))
                .collect();
            let exported = Exported { sources, bodies: env.content.bodies };
            bodies.import(exported).map_err(|e| format!("the saved messages could not be read: {e}"))?;
        }
        FORMAT_V1 => {
            if mailcraft_store::legacy::has_inline(text.as_bytes()) {
                let pending = mailcraft_store::legacy::envelope_inline(text.as_bytes())
                    .map_err(|e| format!("the saved messages could not be read: {e}"))?;
                mailcraft_store::legacy::apply(&mut mailbox, pending, &mut bodies)
                    .map_err(|e| format!("the saved messages could not be read: {e}"))?;
            }
        }
        other => {
            let shown: String = other.chars().take(40).collect();
            return Err(format!("the saved mailbox has an unknown format `{shown}`"));
        }
    }
    Ok((mailbox, bodies))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_engine::model::ItemId;
    use mailcraft_engine::sample::{sample_now, sample_session_at};

    fn sample_text() -> String {
        let s = sample_session_at(sample_now());
        encode(&s.mailbox, &s.bodies).unwrap()
    }

    #[test]
    fn round_trip_keeps_the_mailbox_and_its_messages() {
        let s = sample_session_at(sample_now());
        let text = encode(&s.mailbox, &s.bodies).unwrap();
        let (mb, bodies) = decode(&text).unwrap();
        assert_eq!(mb, s.mailbox);
        let mut with_content = 0;
        for m in mb.mail.items() {
            assert_eq!(bodies.body_of(m).unwrap(), s.bodies.body_of(m).unwrap(), "{}", m.subject);
            assert_eq!(bodies.source_of(m).unwrap(), s.bodies.source_of(m).unwrap(), "{}", m.subject);
            with_content += usize::from(m.blob.is_some() || m.body_key.is_some());
        }
        assert!(with_content > 10, "the sample's messages have content: {with_content}");
    }

    #[test]
    fn eight_bit_sources_survive() {
        let raw: Vec<u8> = (0u8..=255).collect();
        assert_eq!(text_to_bytes(&bytes_to_text(&raw)).unwrap(), raw);
        assert!(text_to_bytes("\u{100}").is_none());
    }

    #[test]
    fn sample_fits_in_browser_storage() {
        // Browsers allow roughly 5 million characters per site; keep the sample well under that.
        let text = sample_text();
        assert!(text.len() < 4_000_000, "sample mailbox is {} bytes", text.len());
    }

    /// A document saved before message content moved out of the items opens with its bodies and sources.
    #[test]
    fn version_1_documents_move_their_content_into_the_body_store() {
        let raw = "From: ann@example.com\r\nSubject: Old\r\n\r\nKept source\r\n";
        let mut mb = Mailbox::new_local("Local Folders");
        let inbox = mb.folders.values().next().map(|f| f.id).unwrap();
        let id = mb.add_mail(mailcraft_engine::model::MailItem { folder: inbox, ..Default::default() });
        let local = mb.add_mail(mailcraft_engine::model::MailItem { folder: inbox, ..Default::default() });
        let mut json: serde_json::Value =
            serde_json::to_value(serde_json::json!({"format": FORMAT_V1, "mailbox": mb})).unwrap();
        let mail = json["mailbox"]["mail"].as_object_mut().unwrap();
        mail.get_mut(&id.0.to_string()).unwrap()["raw"] = serde_json::json!(raw.as_bytes());
        mail.get_mut(&local.0.to_string()).unwrap()["body"] =
            serde_json::json!({"format": "PlainText", "text": "Typed here"});
        let text = json.to_string();
        let (back, bodies) = decode(&text).unwrap();
        let m = back.mail.get(id).unwrap();
        assert_eq!(bodies.source_of(m).unwrap().as_deref(), Some(raw.as_bytes()));
        let l = back.mail.get(local).unwrap();
        assert_eq!(bodies.body_of(l).unwrap().unwrap().text.as_deref(), Some("Typed here"));
        // Saved again, it is a version 2 document.
        let again = encode(&back, &bodies).unwrap();
        assert!(again.contains(FORMAT));
        let (_, bodies) = decode(&again).unwrap();
        assert!(bodies.source_of(back.mail.get(id).unwrap()).unwrap().is_some());
    }

    /// Content that does not match its key is dropped; the mailbox still opens.
    #[test]
    fn hostile_content_is_dropped() {
        let s = sample_session_at(sample_now());
        let text = encode(&s.mailbox, &s.bodies).unwrap();
        let mut json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let sources = json["content"]["sources"].as_object_mut().unwrap();
        let first = sources.keys().next().cloned().unwrap();
        sources.insert(first.clone(), serde_json::json!("tampered"));
        sources.insert("../../etc".into(), serde_json::json!("x"));
        sources.insert("zz".into(), serde_json::json!("\u{2603}"));
        json["content"]["bodies"]["nonsense"] =
            serde_json::json!({"format": "Html", "text": "x", "html": null});
        let (mb, bodies) = decode(&json.to_string()).unwrap();
        let m = mb.mail.items().find(|m| m.blob.as_deref() == Some(first.as_str())).unwrap();
        assert_eq!(bodies.source_of(m).ok().flatten(), None, "a tampered source is not used");
        assert!(mb.mail.contains(ItemId(m.id.0)));
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
        let text = sample_text();
        let cut = text.get(..text.len() / 2).unwrap();
        assert!(decode(cut).is_err());
    }

    /// The writer refuses what the reader refuses.
    #[test]
    fn write_and_read_limits_agree() {
        let text = sample_text();
        assert!(check_size(&text, text.len()).is_ok());
        assert!(check_size(&text, text.len() - 1).is_err());
        let huge = " ".repeat(MAX_STORED_CHARS + 1);
        assert!(check_size(&huge, MAX_STORED_CHARS).is_err());
        assert!(decode(&huge).is_err());
    }
}
