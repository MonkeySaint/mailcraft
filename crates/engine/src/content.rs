//! Message content: bodies and sources read on demand (storage stage S5, `plan/storage-adr.md` §6).
//!
//! Mail items hold neither their decoded body nor their RFC 5322 source; both live in the session's
//! [`mailcraft_store::BodyStore`] (on disk in a profile, in memory otherwise), and items refer to them by key
//! (`blob` for the source, `body_key` for a body record). Everything that shows or scans content reads it
//! here: the reading pane, compose (replies, forwards, drafts), rules and junk filtering, search indexing,
//! print, save and export. Decoded bodies go through the store's byte-capped cache.
//!
//! Whoever makes or changes content stores it first ([`Session::store_source`], [`Session::store_body`],
//! [`Session::store_content`], [`Session::set_body`]) and then changes the item; the journal never names
//! content that is not stored. Content is keyed by its bytes, so equal content is stored once and a changed
//! body gets a new key: undo puts the old key back, and nothing is rewritten in place.
//!
//! An item without content (headers only, or a body the account's offline window took off this computer,
//! see `offline.rs`) reads as `Ok(None)`.

use std::collections::BTreeSet;
use std::sync::Arc;

use mailcraft_model::{Body, ItemId, MailItem};
use mailcraft_store::{Content, StoreError};

use crate::{CmdError, Session};

/// The error a command reports when content could not be read or stored.
pub(crate) fn content_err(e: StoreError) -> CmdError {
    CmdError::Failed(format!("message content: {e}"))
}

/// Bytes of a source read for its header section (expiry dates, header conditions).
pub const HEADER_SCAN: usize = mailcraft_store::legacy::HEADER_SCAN;

/// Most remembered body tests ([`Session::body_test`]); past it they are all forgotten and found again.
pub const MAX_BODY_TESTS: usize = 1 << 18;

/// Results of body tests (the @Mentioned filter, View Settings › Filter's words in the body) by (item, content
/// keys, test). Content is keyed by its bytes, so a changed, evicted or downloaded body has other keys and is
/// tested again; the list's filters do not read every body of a folder from the store each time the list is
/// built.
#[derive(Debug, Default)]
pub(crate) struct BodyTests {
    map: std::collections::HashMap<(u64, u64, u64), bool>,
}

/// A hash identifying a body test (what it looks for), for [`Session::body_test`].
pub(crate) fn test_key(v: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

impl Session {
    /// The decoded body of item `id`: its body record, else the body its source gives. `Ok(None)` for an
    /// unknown item or one whose body is not on this computer.
    pub fn body(&self, id: ItemId) -> Result<Option<Arc<Body>>, CmdError> {
        match self.mailbox.mail.get(id) {
            Some(m) => self.body_of(m),
            None => Ok(None),
        }
    }

    /// The decoded body of `m` (see [`Session::body`]).
    pub fn body_of(&self, m: &MailItem) -> Result<Option<Arc<Body>>, CmdError> {
        self.bodies.body_of(m).map_err(content_err)
    }

    /// The body of `m`, or an empty one when it has none or it cannot be read (logged): for places that only
    /// show or scan its text.
    pub fn body_or_empty(&self, m: &MailItem) -> Arc<Body> {
        match self.bodies.body_of(m) {
            Ok(Some(b)) => b,
            Ok(None) => Arc::new(Body::default()),
            Err(e) => {
                log::warn!("cannot read the body of message {}: {e}", m.id.0);
                Arc::new(Body::default())
            }
        }
    }

    /// Whether the body of `m` passes `test` (`key`: what the test looks for, see [`test_key`]). The result is
    /// remembered for the item's content (see [`BodyTests`]), so the body is read once, not each time a list
    /// is built.
    pub(crate) fn body_test(&self, m: &MailItem, key: u64, test: impl FnOnce(&Body) -> bool) -> bool {
        let content = test_key(&(m.blob.as_deref(), m.body_key.as_deref()));
        let k = (m.id.0, content, key);
        let memo = self.body_tests.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(r) = memo.map.get(&k) {
            return *r;
        }
        drop(memo);
        let r = test(&self.body_or_empty(m));
        let mut memo = self.body_tests.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if memo.map.len() >= MAX_BODY_TESTS {
            memo.map.clear();
        }
        memo.map.insert(k, r);
        r
    }

    /// Plain text of `m`'s body ([`Body::plain_text`]; empty when it has none).
    pub fn body_text(&self, m: &MailItem) -> String {
        self.body_or_empty(m).plain_text()
    }

    /// The RFC 5322 source of item `id` (`Ok(None)` for an unknown item or one without a stored source).
    pub fn source(&self, id: ItemId) -> Result<Option<Vec<u8>>, CmdError> {
        match self.mailbox.mail.get(id) {
            Some(m) => self.source_of(m),
            None => Ok(None),
        }
    }

    /// The RFC 5322 source of `m` (`Ok(None)` when it has none).
    pub fn source_of(&self, m: &MailItem) -> Result<Option<Vec<u8>>, CmdError> {
        self.bodies.source_of(m).map_err(content_err)
    }

    /// The first `cap` bytes of `m`'s source, for a header scan (`None` when it has none or it cannot be
    /// read, which is logged).
    pub fn source_head(&self, m: &MailItem, cap: usize) -> Option<Vec<u8>> {
        match self.bodies.head_of(m, cap) {
            Ok(h) => h,
            Err(e) => {
                log::warn!("cannot read the source of message {}: {e}", m.id.0);
                None
            }
        }
    }

    /// Stores `content` for `item` (not yet in the mailbox, or a copy): its source under `item.blob` (the
    /// expiry date read from it) and a body record when the body is not the one the source gives. The preview
    /// line is the caller's.
    pub fn store_content(&mut self, item: &mut MailItem, content: &Content) -> Result<(), CmdError> {
        if let Some(raw) = &content.source
            && item.expires.is_none()
        {
            item.expires = mailcraft_store::legacy::expiry_in(raw);
        }
        self.bodies.store(item, content).map_err(content_err)?;
        item.body_evicted = false;
        Ok(())
    }

    /// Stores `raw` as the source of `item` (the body is the one it gives; a body record is dropped).
    pub fn store_source(&mut self, item: &mut MailItem, raw: &[u8]) -> Result<(), CmdError> {
        self.store_content(item, &Content { source: Some(raw.to_vec()), body: None })
    }

    /// Stores `body` as the body of `item` (a body record; its source key, if any, stays for View Source)
    /// and refreshes its preview line.
    pub fn store_body(&mut self, item: &mut MailItem, body: &Body) -> Result<(), CmdError> {
        self.bodies.store_body(item, body).map_err(content_err)?;
        item.body_evicted = false;
        Ok(())
    }

    /// Stores `body` as the body of the mailbox item `id` (recorded as a change of that item); false when
    /// there is no such item.
    pub fn set_body(&mut self, id: ItemId, body: &Body) -> Result<bool, CmdError> {
        let Some(mut item) = self.mailbox.mail.get(id).cloned() else { return Ok(false) };
        self.store_body(&mut item, body)?;
        let (key, preview) = (item.body_key, item.preview);
        crate::offline::arrived(self, id);
        Ok(self.mailbox.mail.update(id, |m| {
            m.body_key = key;
            m.preview = preview;
            m.body_evicted = false;
        }))
    }

    /// Stores `raw` as the source of the mailbox item `id` (its body is then the one the source gives), or
    /// with `None` takes its source and body away (recorded as a change of that item). False when there is no
    /// such item.
    pub fn set_source(&mut self, id: ItemId, raw: Option<&[u8]>) -> Result<bool, CmdError> {
        let Some(mut item) = self.mailbox.mail.get(id).cloned() else { return Ok(false) };
        match raw {
            Some(raw) => self.store_source(&mut item, raw)?,
            None => {
                item.blob = None;
                item.body_key = None;
            }
        }
        let (blob, key, expires) = (item.blob, item.body_key, item.expires);
        crate::offline::arrived(self, id);
        Ok(self.mailbox.mail.update(id, |m| {
            m.blob = blob;
            m.body_key = key;
            m.expires = expires;
            m.body_evicted = false;
        }))
    }

    /// Changes the body of the mailbox item `id` with `f` (starting from its current body, or an empty one)
    /// and stores the result as a body record; the preview line follows. False when there is no such item.
    pub fn edit_body(&mut self, id: ItemId, f: impl FnOnce(&mut Body)) -> Result<bool, CmdError> {
        let Some(m) = self.mailbox.mail.get(id) else { return Ok(false) };
        let mut body = (*self.body_or_empty(m)).clone();
        f(&mut body);
        self.set_body(id, &body)
    }

    /// Adds `item` to the mailbox with `content` stored for it (see [`Session::store_content`]); its preview
    /// line comes from the body when it has none. Returns its new id.
    pub fn add_message(&mut self, mut item: MailItem, content: &Content) -> Result<ItemId, CmdError> {
        self.store_content(&mut item, content)?;
        if item.preview.is_empty()
            && let Some(b) = &content.body
        {
            item.preview = b.preview();
        }
        Ok(self.mailbox.add_mail(item))
    }

    /// The source of item `id` as text (tests).
    #[cfg(test)]
    pub(crate) fn source_text(&self, id: ItemId) -> String {
        self.source(id).ok().flatten().map(|r| String::from_utf8_lossy(&r).into_owned()).unwrap_or_default()
    }

    /// The body of item `id`, or an empty one (tests).
    #[cfg(test)]
    pub(crate) fn body_now(&self, id: ItemId) -> Arc<Body> {
        self.body(id).ok().flatten().unwrap_or_default()
    }

    /// Every content key something in this session still refers to: the mailbox, the recoverable items, the
    /// undo and redo history. Content outside this set can go.
    pub fn content_keys_in_use(&self) -> BTreeSet<String> {
        let mut keys = mailcraft_store::bodies::keys_of(self.mailbox.mail.items());
        keys.extend(mailcraft_store::bodies::keys_of(self.recover.bin.items.iter().filter_map(|e| {
            match &e.item {
                mailcraft_model::DeletedItem::Mail(m) => Some(&**m),
                _ => None,
            }
        })));
        keys.extend(self.undo.content_keys());
        keys
    }
}

#[cfg(test)]
mod tests;
