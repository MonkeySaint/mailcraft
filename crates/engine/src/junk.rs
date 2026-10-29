//! The junk filter: applies [`mailcraft_model::junk`] verdicts to mail (Junk Email folder or permanent
//! delete), and answers what the reading pane needs (links disabled, suspicious-domain warning).
//!
//! Entry points:
//! - [`on_new_mail`]: newly arrived Inbox mail (the send/receive hook in `sync.rs` calls it before the rules,
//!   so rules see the result: "suspected to be junk" matches items it moved);
//! - [`run_filter`]: Run Junk Filter over a folder (`mail.junk {"action":"run"}`);
//! - [`classify`]: one item's verdict with its signals (transparency, `mail.junk {"action":"classify"}`);
//! - [`links_disabled`], [`sender_warning`], [`reading_info`]: reading pane / read window;
//! - [`on_sent`]: "Automatically add people I email to the Safe Senders List".
//!
//! Like `rules_exec`, nothing here journals: the enclosing command or sync step diffs the mailbox.

use serde_json::{Value, json};

use crate::Session;
use mailcraft_model::junk::{self, JunkContext, JunkList, ScoreInput, Verdict};
use mailcraft_model::{FolderId, FolderRole, ItemId, MailItem};

/// Bytes of a raw message scanned for declared charsets.
const MAX_CHARSET_SCAN: usize = 256 * 1024;
/// Most charsets collected from one message.
const MAX_CHARSETS: usize = 32;

/// What a filter run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterReport {
    /// Items looked at.
    pub checked: usize,
    /// Items moved to Junk Email.
    pub moved: Vec<ItemId>,
    /// Items permanently deleted ("Permanently delete suspected junk email").
    pub deleted: Vec<ItemId>,
}

impl FilterReport {
    pub fn to_json(&self) -> Value {
        json!({
            "checked": self.checked,
            "moved": self.moved.len(),
            "deleted": self.deleted.len(),
            "ids": self.moved.iter().chain(self.deleted.iter()).map(|i| i.0).collect::<Vec<_>>(),
        })
    }
}

// ---------------------------------------------------------------------------------------------------------
// Context

/// Lower-case addresses of every contact.
fn contact_addresses(s: &Session) -> Vec<String> {
    let mut out: Vec<String> = s
        .mailbox
        .contacts
        .values()
        .flat_map(|c| c.emails.iter())
        .map(|e| e.trim().to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Charsets a raw message declares: `charset=` parameters (message and parts) and RFC 2047 encoded words.
pub fn declared_charsets(raw: &[u8]) -> Vec<String> {
    let scan = raw.get(..raw.len().min(MAX_CHARSET_SCAN)).unwrap_or(&[]);
    let text = String::from_utf8_lossy(scan).to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut add = |c: &str| {
        let c = c.trim().trim_matches(|ch| ch == '"' || ch == '\'');
        if !c.is_empty() && c.len() <= 40 && out.len() < MAX_CHARSETS && !out.iter().any(|o| o == c) {
            out.push(c.to_owned());
        }
    };
    for (pos, m) in text.match_indices("charset=") {
        let rest = text.get(pos.saturating_add(m.len())..).unwrap_or("");
        let rest = rest.trim_start_matches('"');
        let token: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
            .take(41)
            .collect();
        add(&token);
    }
    // Encoded words: =?charset?b?...?= / =?charset*lang?q?...?=
    for (pos, m) in text.match_indices("=?") {
        let rest = text.get(pos.saturating_add(m.len())..).unwrap_or("");
        let token: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
            .take(41)
            .collect();
        let after = rest.get(token.len()..).unwrap_or("");
        if after.starts_with('?') || after.starts_with('*') {
            add(&token);
        }
    }
    out
}

/// The start of a message's source (its kept bytes or its blob in the store), enough for the header scans
/// below.
fn source_head<'a>(s: &Session, item: &'a MailItem) -> Option<std::borrow::Cow<'a, [u8]>> {
    crate::cmd::compose::stored_head(s, item, MAX_CHARSET_SCAN)
}

/// The mailing-list address of a message (List-Post `mailto:`), when it has one.
pub fn list_address(s: &Session, item: &MailItem) -> Option<String> {
    list_address_in(&source_head(s, item)?)
}

/// [`list_address`] from the message's source (or its start).
pub fn list_address_in(raw: &[u8]) -> Option<String> {
    let post = crate::rules_exec::header_value(raw, "List-Post")?;
    let lower = post.to_lowercase();
    let start = lower.find("mailto:")?.saturating_add("mailto:".len());
    let addr: String = lower
        .get(start..)?
        .chars()
        .take_while(|c| !matches!(c, '>' | '?' | ',' | ' ' | '\t'))
        .take(junk::MAX_ENTRY_CHARS)
        .collect();
    addr.contains('@').then_some(addr)
}

/// Everything [`junk::classify`] needs about `item`.
fn context_for(s: &Session, item: &MailItem, contacts: &[String]) -> JunkContext {
    let head = source_head(s, item);
    JunkContext {
        contacts: contacts.to_vec(),
        charsets: head.as_deref().map(declared_charsets).unwrap_or_default(),
        list_address: head.as_deref().and_then(list_address_in),
        score: ScoreInput {
            has_message_id: item.message_id.as_deref().is_some_and(|m| !m.trim().is_empty()),
        },
    }
}

/// The verdict for one item under the current settings.
pub fn classify(s: &Session, id: ItemId) -> Option<Verdict> {
    let item = s.mailbox.mail.get(&id)?;
    let contacts = contact_addresses(s);
    Some(junk::classify(&s.mailbox.junk, item, &context_for(s, item, &contacts)))
}

/// A verdict as JSON (reason, entry, score, signals).
pub fn verdict_json(id: ItemId, v: &Verdict) -> Value {
    json!({
        "id": id.0,
        "junk": v.junk,
        "reason": v.reason.key(),
        "entry": v.reason.entry(),
        "score": v.score,
        "signals": v.signals.iter().map(|s| json!({"key": s.key, "points": s.points, "detail": s.detail})).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------------------------------------
// Filtering

fn role_of(s: &Session, id: ItemId) -> Option<FolderRole> {
    s.mailbox.mail.get(&id).and_then(|m| s.mailbox.folder(m.folder)).and_then(|f| f.role)
}

/// Folders the filter never touches (the user's own mail and the junk/trash bins).
fn exempt(role: Option<FolderRole>) -> bool {
    matches!(
        role,
        Some(
            FolderRole::JunkEmail
                | FolderRole::Drafts
                | FolderRole::Outbox
                | FolderRole::SentItems
                | FolderRole::DeletedItems
        )
    )
}

/// Classifies `ids` and files the junk: to the account's Junk Email folder, or permanently deleted when
/// "Permanently delete suspected junk email" is on. Items without a Junk Email folder in their account are
/// left alone (and not counted as moved).
pub fn filter_items(s: &mut Session, ids: &[ItemId]) -> FilterReport {
    let mut report = FilterReport::default();
    let contacts = contact_addresses(s);
    for &id in ids {
        if exempt(role_of(s, id)) {
            continue;
        }
        let Some(item) = s.mailbox.mail.get(&id) else { continue };
        report.checked = report.checked.saturating_add(1);
        let verdict = junk::classify(&s.mailbox.junk, item, &context_for(s, item, &contacts));
        if !verdict.junk {
            continue;
        }
        if s.mailbox.junk.delete_suspected {
            if s.mailbox.mail.remove(&id).is_some() {
                report.deleted.push(id);
            }
            continue;
        }
        let dest = s
            .mailbox
            .mail
            .get(&id)
            .and_then(|m| s.mailbox.folder_account(m.folder))
            .and_then(|a| s.mailbox.role_folder(a, FolderRole::JunkEmail));
        if let Some(dest) = dest
            && let Ok(moved) = s.mailbox.move_items(&[id], dest)
        {
            report.moved.extend(moved);
        }
    }
    if !report.moved.is_empty() || !report.deleted.is_empty() {
        let gone: Vec<ItemId> = report.moved.iter().chain(report.deleted.iter()).copied().collect();
        s.selection.items.retain(|i| !gone.contains(i));
        if s.selection.focus.is_some_and(|f| gone.contains(&f)) {
            s.selection.focus = s.selection.items.first().copied();
        }
    }
    report
}

/// Newly arrived mail: the junk filter over the items that landed in an Inbox. Call before the receive
/// rules.
pub fn on_new_mail(s: &mut Session, ids: &[ItemId]) -> FilterReport {
    let inbox: Vec<ItemId> =
        ids.iter().copied().filter(|i| role_of(s, *i) == Some(FolderRole::Inbox)).collect();
    filter_items(s, &inbox)
}

/// Run Junk Filter over every message in `folder`.
pub fn run_filter(s: &mut Session, folder: FolderId) -> FilterReport {
    let ids: Vec<ItemId> = s.mailbox.mail.values().filter(|m| m.folder == folder).map(|m| m.id).collect();
    filter_items(s, &ids)
}

/// Messages just sent: with "Automatically add people I email to the Safe Senders List" on, their To/Cc/Bcc
/// addresses join the Safe Senders List. Returns how many were added.
pub fn on_sent(s: &mut Session, ids: &[ItemId]) -> usize {
    if !s.mailbox.junk.add_recipients_to_safe {
        return 0;
    }
    let addrs: Vec<String> = ids
        .iter()
        .filter_map(|id| s.mailbox.mail.get(id))
        .flat_map(|m| m.to.iter().chain(m.cc.iter()).chain(m.bcc.iter()))
        .map(|a| a.addr.clone())
        .collect();
    let mut added = 0usize;
    for a in addrs {
        if matches!(s.mailbox.junk.add(JunkList::SafeSenders, &a), Ok(true)) {
            added = added.saturating_add(1);
        }
    }
    added
}

// ---------------------------------------------------------------------------------------------------------
// Reading pane

/// Whether links (and other active content) are disabled for `id`: it sits in Junk Email and "Disable links
/// and other functionality" is on.
pub fn links_disabled(s: &Session, id: ItemId) -> bool {
    s.mailbox.junk.disable_links && role_of(s, id) == Some(FolderRole::JunkEmail)
}

/// The "suspicious domain name" warning for `id`'s sender, when that option is on.
pub fn sender_warning(s: &Session, id: ItemId) -> Option<&'static str> {
    if !s.mailbox.junk.warn_suspicious_domains {
        return None;
    }
    let item = s.mailbox.mail.get(&id)?;
    junk::suspicious_domain(&item.from.as_ref()?.addr)
}

/// Info-bar text for a message in Junk Email (our wording).
pub const JUNK_INFOBAR: &str = "This message is in the Junk Email folder. Links and other functionality have \
                                been disabled; move it to the Inbox to turn them on.";
/// Info-bar text when links stay on in Junk Email.
pub const JUNK_INFOBAR_LINKS_ON: &str = "This message is in the Junk Email folder.";

/// What the reading pane shows for `id`: `{"junk", "linksDisabled", "infobar", "warning"}`.
pub fn reading_info(s: &Session, id: ItemId) -> Value {
    let in_junk = role_of(s, id) == Some(FolderRole::JunkEmail);
    let disabled = links_disabled(s, id);
    let infobar = in_junk.then_some(if disabled { JUNK_INFOBAR } else { JUNK_INFOBAR_LINKS_ON });
    json!({
        "junk": in_junk,
        "linksDisabled": disabled,
        "infobar": infobar,
        "warning": sender_warning(s, id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::sample_session;
    use mailcraft_model::junk::{JunkLevel, JunkReason};
    use mailcraft_model::{Address, MailItem};

    fn inbox(s: &Session) -> FolderId {
        let a = s.current_account().unwrap();
        s.mailbox.role_folder(a, FolderRole::Inbox).unwrap()
    }
    fn junkf(s: &Session) -> FolderId {
        let a = s.current_account().unwrap();
        s.mailbox.role_folder(a, FolderRole::JunkEmail).unwrap()
    }
    fn deliver(s: &mut Session, from: &str, subject: &str, body: &str, raw: Option<&str>) -> ItemId {
        let id = ItemId(s.mailbox.alloc_id());
        let folder = inbox(s);
        s.mailbox.mail.insert(
            id,
            MailItem {
                id,
                folder,
                message_id: Some(format!("<{}@example.com>", id.0)),
                from: Some(Address::new(None, from)),
                to: vec![Address::new(None, "me@example.com")],
                subject: subject.into(),
                sent: Some(s.now()),
                body: mailcraft_model::Body { text: Some(body.into()), ..Default::default() },
                raw: raw.map(|r| r.as_bytes().to_vec()),
                ..Default::default()
            },
        );
        id
    }

    #[test]
    fn sample_inbox_is_clean_at_low_and_high() {
        let mut s = sample_session();
        let f = inbox(&s);
        for level in [JunkLevel::Low, JunkLevel::High] {
            s.mailbox.junk.level = level;
            let r = run_filter(&mut s, f);
            assert!(r.checked > 0);
            assert!(r.moved.is_empty(), "{level:?} moved {:?}", r.moved);
        }
    }

    #[test]
    fn sample_junk_scores_as_junk_at_high() {
        let s = sample_session();
        let j = junkf(&s);
        let ids: Vec<ItemId> = s.mailbox.items_in(j).iter().map(|m| m.id).collect();
        assert!(!ids.is_empty());
        let prize = ids
            .iter()
            .find(|i| s.mailbox.mail.get(i).is_some_and(|m| m.subject.contains("WON")))
            .copied()
            .unwrap();
        let v = classify(&s, prize).unwrap();
        assert!(v.score >= junk::HIGH_THRESHOLD, "{v:?}");
    }

    #[test]
    fn new_mail_filter_moves_blocked_and_spam_and_spares_safe() {
        let mut s = sample_session();
        s.mailbox.junk.add(JunkList::BlockedSenders, "@spam.example.net").unwrap();
        s.mailbox.junk.add(JunkList::SafeSenders, "friend@spam.example.net").unwrap();
        let blocked = deliver(&mut s, "x@spam.example.net", "Hello", "hi", None);
        let safe =
            deliver(&mut s, "friend@spam.example.net", "YOU HAVE WON!!!", "act now, claim your prize", None);
        let spam = deliver(
            &mut s,
            "promo@example.org",
            "YOU HAVE WON A PRIZE!!!",
            "Claim your prize, act now, 100% free http://192.0.2.9/x",
            None,
        );
        let normal =
            deliver(&mut s, "colleague@example.com", "Notes from today", "Attached are the notes.", None);
        let r = on_new_mail(&mut s, &[blocked, safe, spam, normal]);
        assert_eq!(r.checked, 4);
        let j = junkf(&s);
        let i = inbox(&s);
        assert_eq!(s.mailbox.mail[&blocked].folder, j);
        assert_eq!(s.mailbox.mail[&spam].folder, j);
        assert_eq!(s.mailbox.mail[&safe].folder, i);
        assert_eq!(s.mailbox.mail[&normal].folder, i);
        assert_eq!(r.moved.len(), 2);
        assert!(links_disabled(&s, spam));
        assert!(!links_disabled(&s, normal));
        s.mailbox.junk.disable_links = false;
        assert!(!links_disabled(&s, spam));
        assert_eq!(reading_info(&s, spam)["infobar"], JUNK_INFOBAR_LINKS_ON);
    }

    #[test]
    fn new_mail_ignores_items_outside_inbox() {
        let mut s = sample_session();
        s.mailbox.junk.level = JunkLevel::SafeListsOnly;
        let id = deliver(&mut s, "a@example.org", "Hi", "x", None);
        let a = s.current_account().unwrap();
        let sent = s.mailbox.role_folder(a, FolderRole::SentItems).unwrap();
        s.mailbox.mail.get_mut(&id).unwrap().folder = sent;
        assert_eq!(on_new_mail(&mut s, &[id]).checked, 0);
        assert_eq!(filter_items(&mut s, &[id]).checked, 0);
        assert_eq!(s.mailbox.mail[&id].folder, sent);
    }

    #[test]
    fn permanently_delete_suspected_junk() {
        let mut s = sample_session();
        s.mailbox.junk.delete_suspected = true;
        s.mailbox.junk.add(JunkList::BlockedSenders, "bad@example.net").unwrap();
        let id = deliver(&mut s, "bad@example.net", "x", "y", None);
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        let r = on_new_mail(&mut s, &[id]);
        assert_eq!(r.deleted, vec![id]);
        assert!(!s.mailbox.mail.contains_key(&id));
        assert!(s.selection.focus.is_none());
    }

    #[test]
    fn blocked_encoding_and_list_address_from_raw() {
        let mut s = sample_session();
        s.mailbox.junk.add(JunkList::BlockedEncodings, "koi8-r").unwrap();
        let raw = "From: a@example.org\r\nSubject: =?KOI8-R?B?8NLJ18XU?=\r\nContent-Type: text/plain; charset=\"utf-8\"\r\n\r\nhi\r\n";
        let id = deliver(&mut s, "a@example.org", "x", "hi", Some(raw));
        assert_eq!(declared_charsets(raw.as_bytes()), vec!["utf-8", "koi8-r"]);
        let v = classify(&s, id).unwrap();
        assert_eq!(v.reason, JunkReason::BlockedEncoding("koi8-r".into()));

        let raw = "From: b@example.org\r\nList-Post: <mailto:Team@Lists.Example.org?subject=x>\r\n\r\nhi\r\n";
        s.mailbox.junk.level = JunkLevel::SafeListsOnly;
        let id = deliver(&mut s, "b@example.org", "x", "hi", Some(raw));
        assert_eq!(list_address(&s, &s.mailbox.mail[&id]).as_deref(), Some("team@lists.example.org"));
        assert!(classify(&s, id).unwrap().junk);
        s.mailbox.junk.add(JunkList::SafeRecipients, "team@lists.example.org").unwrap();
        assert!(!classify(&s, id).unwrap().junk);
    }

    #[test]
    fn contacts_are_trusted() {
        let mut s = sample_session();
        s.mailbox.junk.level = JunkLevel::SafeListsOnly;
        let addr = s.mailbox.contacts.values().find_map(|c| c.emails.first().cloned()).unwrap();
        let id = deliver(&mut s, &addr, "x", "y", None);
        assert_eq!(classify(&s, id).unwrap().reason, JunkReason::Contact);
    }

    #[test]
    fn on_sent_adds_recipients_when_enabled() {
        let mut s = sample_session();
        let id = deliver(&mut s, "me@example.com", "x", "y", None);
        assert_eq!(on_sent(&mut s, &[id]), 0);
        s.mailbox.junk.add_recipients_to_safe = true;
        assert_eq!(on_sent(&mut s, &[id]), 1);
        assert_eq!(on_sent(&mut s, &[id]), 0);
        assert!(s.mailbox.junk.safe_senders.contains(&"me@example.com".to_owned()));
    }

    #[test]
    fn sender_warning_follows_option() {
        let mut s = sample_session();
        let id = deliver(&mut s, "a@xn--exmple-cua.com", "x", "y", None);
        assert!(sender_warning(&s, id).is_some());
        assert!(reading_info(&s, id)["warning"].is_string());
        s.mailbox.junk.warn_suspicious_domains = false;
        assert!(sender_warning(&s, id).is_none());
    }

    #[test]
    fn nothing_is_deleted_without_the_option() {
        // Every path that files junk (new mail, Run Junk Filter, Safe Lists Only, blocked lists) moves at
        // most; only "Permanently delete suspected junk email" removes items.
        let mut s = sample_session();
        assert!(!s.mailbox.junk.delete_suspected, "off by default");
        s.mailbox.junk.add(JunkList::BlockedSenders, "@spam.example.net").unwrap();
        let before = s.mailbox.mail.len();
        let a = deliver(&mut s, "x@spam.example.net", "x", "y", None);
        let b = deliver(&mut s, "promo@example.org", "YOU HAVE WON!!!", "act now, claim your prize", None);
        let all: Vec<ItemId> = s.mailbox.mail.keys().copied().collect();
        for level in JunkLevel::ALL {
            s.mailbox.junk.level = level;
            assert!(on_new_mail(&mut s, &[a, b]).deleted.is_empty());
            assert!(filter_items(&mut s, &all).deleted.is_empty());
            let i = inbox(&s);
            assert!(run_filter(&mut s, i).deleted.is_empty());
        }
        assert_eq!(s.mailbox.mail.len(), before + 2, "every item is still there");
        // An account without a Junk Email folder keeps its junk where it is.
        let j = junkf(&s);
        s.mailbox.folders.remove(&j);
        let c = deliver(&mut s, "y@spam.example.net", "x", "y", None);
        let r = on_new_mail(&mut s, &[c]);
        assert!(r.moved.is_empty() && r.deleted.is_empty());
        assert_eq!(s.mailbox.mail[&c].folder, inbox(&s));
    }

    #[test]
    fn junk_lists_are_sanitized_when_a_data_file_opens() {
        // Lists written by an older or hand-edited data file may be unsorted, duplicated or invalid; `add`
        // relies on sorted lists, so they are cleaned on open.
        let dir = tempfile::tempdir().unwrap();
        {
            let mut s = Session::open(dir.path()).unwrap();
            let mut mb = s.mailbox.clone();
            mb.junk.blocked_senders = vec![
                "Z@Example.org".into(),
                "a@example.org".into(),
                "a@example.org".into(),
                "bad entry".into(),
            ];
            mb.junk.blocked_tlds = vec![".RU".into(), "xyz".into()];
            s.mailbox = mb.clone();
            s.journal(vec![mailcraft_store::Op::Replace(Box::new(mb))]);
        }
        let mut s = Session::open(dir.path()).unwrap();
        assert_eq!(s.mailbox.junk.blocked_senders, vec!["a@example.org", "z@example.org"]);
        assert_eq!(s.mailbox.junk.blocked_tlds, vec!["ru", "xyz"]);
        assert!(!s.mailbox.junk.add(JunkList::BlockedSenders, "z@example.org").unwrap());
        assert_eq!(s.mailbox.junk.blocked_senders.len(), 2);
    }

    #[test]
    fn hostile_raw_never_panics() {
        let raw: Vec<u8> =
            b"charset=".iter().chain(b"=?\xff\xfe?".iter()).copied().cycle().take(600_000).collect();
        assert!(declared_charsets(&raw).len() <= MAX_CHARSETS);
        assert!(declared_charsets(b"").is_empty());
        assert!(declared_charsets(b"charset=").is_empty());
    }
}
