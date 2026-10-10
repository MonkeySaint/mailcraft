//! Signatures (File › Options › Mail › Signatures; Message › Include › Signature), the per-account choice of
//! default signature for new messages and for replies/forwards, the Auto-Complete list of recently used
//! recipients, and the user's custom spelling dictionary.
//!
//! Every operation here is total: names are trimmed and length-capped, lists are capped, nothing panics.

use serde::{Deserialize, Serialize};

use crate::{AccountId, DateTime, Doc, Mailbox, ModelError, Result};

/// Most signatures kept.
pub const MAX_SIGNATURES: usize = 200;
/// Longest signature name (chars).
pub const MAX_SIGNATURE_NAME_CHARS: usize = 64;
/// Auto-Complete list length (most recently used first).
pub const MAX_RECENT_RECIPIENTS: usize = 500;
/// Most words in the custom dictionary.
pub const MAX_DICTIONARY_WORDS: usize = 10_000;
/// Longest custom dictionary word (chars).
pub const MAX_DICTIONARY_WORD_CHARS: usize = 64;
/// Longest address / display name kept in the Auto-Complete list (chars).
const MAX_RECIPIENT_CHARS: usize = 320;

/// A saved signature.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Signature {
    pub name: String,
    /// Rich body (converted to plain text when inserted into a plain-text message).
    pub body: Doc,
    /// Default for new messages (accounts without their own choice).
    pub default_new: bool,
    /// Default for replies and forwards (accounts without their own choice).
    pub default_reply: bool,
}

/// One account's default signature choice. `None` = "(none)". An account without an entry uses the
/// signatures flagged `default_new` / `default_reply`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountSignatures {
    pub account: AccountId,
    pub new_messages: Option<String>,
    pub replies: Option<String>,
}

/// One Auto-Complete entry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecentRecipient {
    /// Display name as last used (may be empty).
    pub name: String,
    pub address: String,
    /// Last time a message was sent to this address.
    pub last_used: Option<DateTime>,
    /// Number of messages sent to this address.
    pub uses: u32,
}

/// A signature name trimmed, without control chars, capped; `None` when empty.
pub fn clean_signature_name(name: &str) -> Option<String> {
    let n: String = name.trim().chars().filter(|c| !c.is_control()).take(MAX_SIGNATURE_NAME_CHARS).collect();
    let n = n.trim().to_owned();
    (!n.is_empty() && !n.eq_ignore_ascii_case("(none)")).then_some(n)
}

impl Mailbox {
    /// Signature by name (case-insensitive).
    pub fn signature(&self, name: &str) -> Option<&Signature> {
        let n = name.trim();
        self.signatures.iter().find(|s| s.name.eq_ignore_ascii_case(n))
    }

    /// Adds a new signature. Fails when the name is empty or already used, or the list is full.
    pub fn add_signature(&mut self, name: &str, body: Doc) -> Result<String> {
        let name = clean_signature_name(name)
            .ok_or_else(|| ModelError::Invalid("a signature needs a name".into()))?;
        if self.signature(&name).is_some() {
            return Err(ModelError::NotAllowed(format!("a signature named \"{name}\" already exists")));
        }
        if self.signatures.len() >= MAX_SIGNATURES {
            return Err(ModelError::NotAllowed("too many signatures".into()));
        }
        self.signatures.push(Signature { name: name.clone(), body, ..Default::default() });
        Ok(name)
    }

    /// Replaces the body of signature `name` (and renames it when `new_name` is given). Default choices that
    /// name it follow the rename.
    pub fn edit_signature(
        &mut self,
        name: &str,
        new_name: Option<&str>,
        body: Option<Doc>,
    ) -> Result<String> {
        let idx = self
            .signatures
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| ModelError::Invalid(format!("no signature named \"{}\"", name.trim())))?;
        let old = self.signatures.get(idx).map(|s| s.name.clone()).unwrap_or_default();
        let mut final_name = old.clone();
        if let Some(nn) = new_name {
            let nn = clean_signature_name(nn)
                .ok_or_else(|| ModelError::Invalid("a signature needs a name".into()))?;
            let clash =
                self.signatures.iter().enumerate().any(|(i, s)| i != idx && s.name.eq_ignore_ascii_case(&nn));
            if clash {
                return Err(ModelError::NotAllowed(format!("a signature named \"{nn}\" already exists")));
            }
            final_name = nn;
        }
        if let Some(sig) = self.signatures.get_mut(idx) {
            sig.name = final_name.clone();
            if let Some(b) = body {
                sig.body = b;
            }
        }
        if final_name != old {
            for d in &mut self.signature_defaults {
                for slot in [&mut d.new_messages, &mut d.replies] {
                    if slot.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&old)) {
                        *slot = Some(final_name.clone());
                    }
                }
            }
        }
        Ok(final_name)
    }

    /// Deletes signature `name`; account choices naming it fall back to "(none)".
    pub fn delete_signature(&mut self, name: &str) -> Result<Signature> {
        let idx = self
            .signatures
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| ModelError::Invalid(format!("no signature named \"{}\"", name.trim())))?;
        let removed = self.signatures.remove(idx);
        for d in &mut self.signature_defaults {
            for slot in [&mut d.new_messages, &mut d.replies] {
                if slot.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&removed.name)) {
                    *slot = None;
                }
            }
        }
        Ok(removed)
    }

    /// Sets the default signatures. With `account`, that account's own choice (an entry is created from the
    /// current effective defaults first); without, the global `default_new` / `default_reply` flags. `Some(None)`
    /// = "(none)", `None` = leave unchanged. Unknown signature names fail.
    pub fn set_signature_defaults(
        &mut self,
        account: Option<AccountId>,
        new_messages: Option<Option<&str>>,
        replies: Option<Option<&str>>,
    ) -> Result<()> {
        let resolve = |mb: &Mailbox, v: Option<Option<&str>>| -> Result<Option<Option<String>>> {
            match v {
                None => Ok(None),
                Some(None) => Ok(Some(None)),
                Some(Some(n)) if n.trim().is_empty() || n.trim().eq_ignore_ascii_case("(none)") => {
                    Ok(Some(None))
                }
                Some(Some(n)) => mb
                    .signature(n)
                    .map(|s| Some(Some(s.name.clone())))
                    .ok_or_else(|| ModelError::Invalid(format!("no signature named \"{}\"", n.trim()))),
            }
        };
        let new_messages = resolve(self, new_messages)?;
        let replies = resolve(self, replies)?;
        match account {
            Some(acc) => {
                if self.account(acc).is_none() {
                    return Err(ModelError::NoAccount(acc));
                }
                let current_new = self.default_signature(Some(acc), false).map(|s| s.name.clone());
                let current_reply = self.default_signature(Some(acc), true).map(|s| s.name.clone());
                let pos = match self.signature_defaults.iter().position(|d| d.account == acc) {
                    Some(p) => p,
                    None => {
                        self.signature_defaults.push(AccountSignatures {
                            account: acc,
                            new_messages: current_new,
                            replies: current_reply,
                        });
                        self.signature_defaults.len().saturating_sub(1)
                    }
                };
                if let Some(d) = self.signature_defaults.get_mut(pos) {
                    if let Some(v) = new_messages {
                        d.new_messages = v;
                    }
                    if let Some(v) = replies {
                        d.replies = v;
                    }
                }
            }
            None => {
                if let Some(v) = new_messages {
                    for s in &mut self.signatures {
                        s.default_new = v.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&s.name));
                    }
                }
                if let Some(v) = replies {
                    for s in &mut self.signatures {
                        s.default_reply = v.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&s.name));
                    }
                }
            }
        }
        Ok(())
    }

    /// The signature inserted automatically: for a new message (`reply == false`) or a reply/forward, for
    /// `account` (its own choice when it has one, else the global default).
    pub fn default_signature(&self, account: Option<AccountId>, reply: bool) -> Option<&Signature> {
        if let Some(d) = account.and_then(|a| self.signature_defaults.iter().find(|d| d.account == a)) {
            let choice = if reply { &d.replies } else { &d.new_messages };
            return choice.as_deref().and_then(|n| self.signature(n));
        }
        self.signatures.iter().find(|s| if reply { s.default_reply } else { s.default_new })
    }

    /// Records the recipients of a sent message in the Auto-Complete list (most recent first, capped at
    /// [`MAX_RECENT_RECIPIENTS`]). `recipients` are (display name, address) pairs.
    pub fn record_recipients(&mut self, recipients: &[(String, String)], now: DateTime) {
        // Process in reverse so the first recipient ends up most recent.
        for (name, addr) in recipients.iter().rev() {
            let addr: String =
                addr.trim().chars().filter(|c| !c.is_control()).take(MAX_RECIPIENT_CHARS).collect();
            if addr.is_empty() || !addr.contains('@') {
                continue;
            }
            let name: String =
                name.trim().chars().filter(|c| !c.is_control()).take(MAX_RECIPIENT_CHARS).collect();
            let mut entry =
                match self.recent_recipients.iter().position(|r| r.address.eq_ignore_ascii_case(&addr)) {
                    Some(i) => self.recent_recipients.remove(i),
                    None => RecentRecipient { address: addr.clone(), ..Default::default() },
                };
            if !name.is_empty() && !name.eq_ignore_ascii_case(&addr) {
                entry.name = name;
            }
            entry.address = addr;
            entry.last_used = Some(now);
            entry.uses = entry.uses.saturating_add(1);
            self.recent_recipients.insert(0, entry);
        }
        self.recent_recipients.truncate(MAX_RECENT_RECIPIENTS);
    }

    /// Removes one address from the Auto-Complete list; false when it was not there.
    pub fn forget_recipient(&mut self, address: &str) -> bool {
        let before = self.recent_recipients.len();
        self.recent_recipients.retain(|r| !r.address.eq_ignore_ascii_case(address.trim()));
        before != self.recent_recipients.len()
    }

    /// Whether `word` is in the custom dictionary (case-insensitive).
    pub fn in_dictionary(&self, word: &str) -> bool {
        let w = word.trim();
        self.custom_dictionary.iter().any(|d| d.eq_ignore_ascii_case(w))
    }

    /// Adds a word to the custom dictionary; false when it was already there or is not a usable word.
    pub fn add_to_dictionary(&mut self, word: &str) -> bool {
        let w = word.trim();
        let ok = !w.is_empty()
            && w.chars().count() <= MAX_DICTIONARY_WORD_CHARS
            && !w.chars().any(|c| c.is_whitespace() || c.is_control());
        if !ok || self.in_dictionary(w) || self.custom_dictionary.len() >= MAX_DICTIONARY_WORDS {
            return false;
        }
        self.custom_dictionary.push(w.to_owned());
        true
    }

    /// Removes a word from the custom dictionary.
    pub fn remove_from_dictionary(&mut self, word: &str) -> bool {
        let before = self.custom_dictionary.len();
        self.custom_dictionary.retain(|d| !d.eq_ignore_ascii_case(word.trim()));
        before != self.custom_dictionary.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Account, AccountKind};

    fn mb() -> (Mailbox, AccountId) {
        let mut m = Mailbox::new_local("Local");
        let a = m.add_account(Account {
            kind: AccountKind::Imap,
            address: "me@example.com".into(),
            ..Default::default()
        });
        (m, a)
    }

    #[test]
    fn signature_crud_and_defaults() {
        let (mut m, a) = mb();
        assert!(m.add_signature("  ", Doc::default()).is_err());
        assert_eq!(m.add_signature(" Work ", Doc::from_plain_text("W")).unwrap(), "Work");
        assert!(m.add_signature("work", Doc::default()).is_err(), "names are case-insensitive");
        m.add_signature("Home", Doc::from_plain_text("H")).unwrap();
        assert!(m.default_signature(None, false).is_none());
        m.set_signature_defaults(None, Some(Some("work")), Some(Some("Home"))).unwrap();
        assert_eq!(m.default_signature(None, false).unwrap().name, "Work");
        assert_eq!(m.default_signature(Some(a), true).unwrap().name, "Home");
        // Per-account choice overrides the global one; unset parts keep the effective value.
        m.set_signature_defaults(Some(a), None, Some(None)).unwrap();
        assert_eq!(m.default_signature(Some(a), false).unwrap().name, "Work");
        assert!(m.default_signature(Some(a), true).is_none());
        assert_eq!(m.default_signature(None, true).unwrap().name, "Home");
        assert!(m.set_signature_defaults(Some(a), Some(Some("nope")), None).is_err());
        assert!(m.set_signature_defaults(Some(AccountId(999)), None, None).is_err());
        // Renames follow into account choices; deletes clear them.
        m.edit_signature("WORK", Some("Office"), Some(Doc::from_plain_text("O"))).unwrap();
        assert_eq!(m.default_signature(Some(a), false).unwrap().name, "Office");
        assert!(m.edit_signature("Office", Some("home"), None).is_err(), "rename onto an existing name");
        m.delete_signature("office").unwrap();
        assert!(m.default_signature(Some(a), false).is_none());
        assert!(m.delete_signature("office").is_err());
        assert_eq!(m.signatures.len(), 1);
    }

    #[test]
    fn recent_recipients_are_mru_and_capped() {
        let (mut m, _) = mb();
        let now = DateTime::from_civil(2026, 10, 9, 12, 0, 0, 0).unwrap();
        m.record_recipients(
            &[("Ann".into(), "ann@example.com".into()), ("".into(), "bo@example.net".into())],
            now,
        );
        assert_eq!(m.recent_recipients[0].address, "ann@example.com");
        m.record_recipients(
            &[("".into(), "BO@example.net".into()), ("".into(), "not-an-address".into())],
            now,
        );
        assert_eq!(m.recent_recipients.len(), 2);
        assert_eq!(m.recent_recipients[0].address, "BO@example.net");
        assert_eq!(m.recent_recipients[0].uses, 2);
        assert_eq!(m.recent_recipients[1].name, "Ann");
        let many: Vec<(String, String)> =
            (0..MAX_RECENT_RECIPIENTS + 20).map(|i| (String::new(), format!("u{i}@example.org"))).collect();
        m.record_recipients(&many, now);
        assert_eq!(m.recent_recipients.len(), MAX_RECENT_RECIPIENTS);
        assert_eq!(m.recent_recipients[0].address, "u0@example.org");
        assert!(m.forget_recipient("u0@EXAMPLE.org"));
        assert!(!m.forget_recipient("u0@example.org"));
    }

    #[test]
    fn dictionary() {
        let (mut m, _) = mb();
        assert!(m.add_to_dictionary("MailCraft"));
        assert!(!m.add_to_dictionary("mailcraft"));
        assert!(!m.add_to_dictionary("two words"));
        assert!(!m.add_to_dictionary(""));
        assert!(m.in_dictionary("MAILCRAFT"));
        assert!(m.remove_from_dictionary("mailcraft"));
        assert!(!m.in_dictionary("MailCraft"));
    }

    #[test]
    fn old_snapshots_without_the_new_fields_load() {
        let (m, _) = mb();
        let mut v = serde_json::to_value(&m).unwrap();
        let o = v.as_object_mut().unwrap();
        for k in ["signatures", "signature_defaults", "recent_recipients", "custom_dictionary"] {
            o.remove(k);
        }
        let back: Mailbox = serde_json::from_value(v).unwrap();
        assert!(back.signatures.is_empty() && back.recent_recipients.is_empty());
    }
}
