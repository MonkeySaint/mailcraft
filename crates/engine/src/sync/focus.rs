//! Focused Inbox: the automatic Focused / Other classification of new mail.
//!
//! A small, explainable heuristic of our own, applied once when a message arrives (step 2 of
//! [`super::arrival`]); the user's choices always win and nothing already in the mailbox is reclassified.
//! In order, the first that applies decides:
//!
//! 1. **Sender overrides.** Always Move to Focused (`mail.focused.focusedSenders`) and Always Move to Other
//!    (`mail.focused.otherSenders`) name senders by address; the newer choice replaced the older one when it
//!    was made, so an address is never on both lists.
//! 2. **Conversations with me.** A reply to a message I sent (its In-Reply-To or References names one of my
//!    messages), or mail sent from one of my own addresses, is Focused.
//! 3. **People I know.** A sender who is one of my contacts, or someone I have written to (a To or Cc
//!    recipient of mail I sent), is Focused.
//! 4. **Invitations.** Meeting requests and other calendar mail are Focused.
//! 5. **Bulk mail.** Mailing-list and bulk traffic goes to Other: a `List-Id` or `List-Unsubscribe` field
//!    (RFC 2919 / RFC 2369), `Precedence: bulk | list | junk`, `Auto-Submitted: auto-generated` (RFC 3834),
//!    or a sender address that says it takes no replies ("noreply", "no-reply", "donotreply"…).
//! 6. Everything else is Focused.

use std::collections::BTreeSet;

use mailcraft_model::{FocusClass, ItemId, MailItem};

use crate::Session;

/// Setting holding the senders whose mail always goes to Focused (Always Move to Focused; lower-case
/// addresses).
pub const FOCUSED_SENDERS_SETTING: &str = "mail.focused.focusedSenders";

/// Why a message got its class (for tests and the command results).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Always Move to Focused / Always Move to Other.
    SenderOverride,
    /// A reply to my mail, or mail from me.
    WithMe,
    /// A contact, or someone I have written to.
    KnownSender,
    /// A meeting request or other calendar mail.
    Invitation,
    /// Mailing-list or bulk mail (the header or address that said so).
    Bulk(&'static str),
    /// Nothing special: the default class.
    Default,
}

/// What the classifier knows about the user, gathered once per batch of new mail.
#[derive(Clone, Debug, Default)]
pub struct Known {
    /// My own addresses (lower case).
    pub me: BTreeSet<String>,
    /// Contacts' addresses and people I have written to (lower case).
    pub people: BTreeSet<String>,
    /// Message-IDs of mail I sent (bare, lower case).
    pub mine: BTreeSet<String>,
    /// Always Move to Focused senders.
    pub focused_senders: BTreeSet<String>,
    /// Always Move to Other senders.
    pub other_senders: BTreeSet<String>,
}

/// Most of my sent messages scanned for correspondents (the newest first would need a sort; the cap only
/// bounds the work on very large mailboxes).
const MAX_SENT_SCANNED: usize = 50_000;

/// Gathers [`Known`] from the session: my addresses, contacts, the recipients and Message-IDs of mail I
/// sent, and the sender overrides.
pub fn known(s: &Session) -> Known {
    let me: BTreeSet<String> = crate::cmd::mail::my_addresses(s).into_iter().collect();
    let mut people: BTreeSet<String> = s
        .mailbox
        .contacts
        .values()
        .flat_map(|c| c.emails.iter())
        .map(|e| e.trim().to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    let mut mine = BTreeSet::new();
    for m in s
        .mailbox
        .mail
        .items()
        .filter(|m| m.from.as_ref().is_some_and(|f| me.contains(&f.addr.trim().to_lowercase())))
        .take(MAX_SENT_SCANNED)
    {
        if let Some(id) = m.message_id.as_deref().map(bare_id).filter(|i| !i.is_empty()) {
            mine.insert(id);
        }
        // Drafts never went anywhere.
        if !m.is_draft {
            people.extend(
                m.to.iter().chain(&m.cc).map(|a| a.addr.trim().to_lowercase()).filter(|a| !a.is_empty()),
            );
        }
    }
    let list = |key: &str| -> BTreeSet<String> {
        s.settings.list(key).into_iter().map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect()
    };
    Known {
        people: people.difference(&me).cloned().collect(),
        me,
        mine,
        focused_senders: list(FOCUSED_SENDERS_SETTING),
        other_senders: list(crate::cmd::mail::OTHER_SENDERS_SETTING),
    }
}

/// The Message-ID key the mail table matches by (no brackets, lower-case).
fn bare_id(v: &str) -> String {
    mailcraft_model::message_id_key(v)
}

/// True for an address whose local part says it takes no replies ("noreply@", "no-reply@",
/// "do_not_reply@", "donotreply@", "no.reply@"…).
pub fn is_no_reply(addr: &str) -> bool {
    let local = addr.trim().rsplit_once('@').map_or(addr.trim(), |(l, _)| l);
    let squashed: String =
        local.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect();
    squashed.contains("noreply") || squashed.contains("donotreply") || squashed.contains("noreplies")
}

/// The bulk-mail header among `head`, if any (step 5 of the module docs).
pub fn bulk_header(head: &mailcraft_mime::Headers) -> Option<&'static str> {
    if head.get("List-Id").is_some_and(|v| !v.trim().is_empty()) {
        return Some("List-Id");
    }
    if head.get("List-Unsubscribe").is_some_and(|v| !v.trim().is_empty()) {
        return Some("List-Unsubscribe");
    }
    if let Some(p) = head.text("Precedence") {
        let p = p.trim().to_ascii_lowercase();
        if matches!(p.as_str(), "bulk" | "list" | "junk") {
            return Some("Precedence");
        }
    }
    if let Some(a) = head.text("Auto-Submitted") {
        // RFC 3834: "auto-generated" is machine mail; "auto-replied" (an out-of-office answer) comes from a
        // person's mailbox and stays where the person would be.
        if a.trim().to_ascii_lowercase().starts_with("auto-generated") {
            return Some("Auto-Submitted");
        }
    }
    None
}

/// The class of new message `m` (with its header section `head` when the source is at hand).
pub fn classify(m: &MailItem, head: Option<&mailcraft_mime::Headers>, k: &Known) -> (FocusClass, Reason) {
    let from = m.from.as_ref().map(|a| a.addr.trim().to_lowercase()).unwrap_or_default();
    if !from.is_empty() {
        if k.focused_senders.contains(&from) {
            return (FocusClass::Focused, Reason::SenderOverride);
        }
        if k.other_senders.contains(&from) {
            return (FocusClass::Other, Reason::SenderOverride);
        }
    }
    let about_mine = m.in_reply_to.iter().chain(&m.references).any(|r| k.mine.contains(&bare_id(r)));
    if about_mine || (!from.is_empty() && k.me.contains(&from)) {
        return (FocusClass::Focused, Reason::WithMe);
    }
    if !from.is_empty() && k.people.contains(&from) {
        return (FocusClass::Focused, Reason::KnownSender);
    }
    let calendar = m.attachments.iter().any(|a| {
        a.mime_type.to_ascii_lowercase().starts_with("text/calendar")
            || a.filename.trim().to_ascii_lowercase().ends_with(".ics")
    });
    if calendar {
        return (FocusClass::Focused, Reason::Invitation);
    }
    if let Some(h) = head.and_then(bulk_header) {
        return (FocusClass::Other, Reason::Bulk(h));
    }
    let sender = m.sender.as_ref().map(|a| a.addr.as_str()).unwrap_or_default();
    if (!from.is_empty() && is_no_reply(&from)) || (!sender.is_empty() && is_no_reply(sender)) {
        return (FocusClass::Other, Reason::Bulk("no-reply sender"));
    }
    (FocusClass::Focused, Reason::Default)
}

/// Classifies newly arrived mail `ids` (journaled); returns the items that went to Other.
pub fn on_new_mail(s: &mut Session, ids: &[ItemId]) -> Vec<ItemId> {
    if ids.is_empty() {
        return Vec::new();
    }
    let k = known(s);
    let mut decided: Vec<(ItemId, FocusClass)> = Vec::new();
    for id in ids.iter().take(super::arrival::MAX_PROCESSED) {
        let Some(m) = s.mailbox.mail.get(*id) else { continue };
        let head = crate::cmd::compose::stored_raw(s, m).map(|raw| crate::cmd::compose::raw_headers(&raw));
        let (class, _) = classify(m, head.as_ref(), &k);
        decided.push((*id, class));
    }
    let mut other = Vec::new();
    let mut changed = Vec::new();
    for (id, class) in decided {
        if let Some(m) = s.mailbox.mail.get_mut(id) {
            if m.focus != class {
                m.focus = class;
                changed.push(id);
            }
            if class == FocusClass::Other {
                other.push(id);
            }
        }
    }
    crate::cmd::mail::journal(s, &changed);
    other
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::calendar::tests::{role, session};
    use mailcraft_model::FolderRole;
    use serde_json::json;

    fn arrive(s: &mut Session, raw: &str) -> ItemId {
        let raw = raw.replace('\n', "\r\n");
        let msg = mailcraft_mime::parse(raw.as_bytes()).unwrap();
        let mut item = MailItem::from_message(&msg, raw.as_bytes(), s.now());
        item.folder = role(s, FolderRole::Inbox);
        s.mailbox.add_mail(item)
    }

    fn class(s: &mut Session, raw: &str) -> (FocusClass, Reason) {
        let id = arrive(s, raw);
        let k = known(s);
        let m = s.mailbox.mail.get(id).unwrap();
        let raw = m.raw.clone().unwrap_or_default();
        let head = crate::cmd::compose::raw_headers(&raw);
        classify(m, Some(&head), &k)
    }

    #[test]
    fn bulk_headers_and_no_reply_senders_go_to_other() {
        let mut s = session();
        let cases = [
            ("From: news@example.org\nList-Id: <weekly.example.org>\nSubject: a\n\nx\n", "List-Id"),
            (
                "From: shop@example.org\nList-Unsubscribe: <mailto:u@example.org>\nSubject: b\n\nx\n",
                "List-Unsubscribe",
            ),
            ("From: promo@example.net\nPrecedence: Bulk\nSubject: c\n\nx\n", "Precedence"),
            ("From: robot@example.net\nAuto-Submitted: auto-generated\nSubject: d\n\nx\n", "Auto-Submitted"),
            ("From: No Reply <no-reply@example.com>\nSubject: e\n\nx\n", "no-reply sender"),
            (
                "From: alerts@example.com\nSender: do_not_reply@example.com\nSubject: f\n\nx\n",
                "no-reply sender",
            ),
        ];
        for (raw, why) in cases {
            assert_eq!(class(&mut s, raw), (FocusClass::Other, Reason::Bulk(why)), "{raw}");
        }
        // Plain mail from a stranger, and an out-of-office answer, stay Focused.
        assert_eq!(class(&mut s, "From: ann@example.com\nSubject: g\n\nx\n").0, FocusClass::Focused);
        assert_eq!(
            class(&mut s, "From: bo@example.org\nAuto-Submitted: auto-replied\nSubject: h\n\nx\n").0,
            FocusClass::Focused
        );
        assert_eq!(
            class(&mut s, "From: cy@example.net\nPrecedence: first-class\nSubject: i\n\nx\n").0,
            FocusClass::Focused
        );
    }

    #[test]
    fn people_i_know_and_replies_to_me_stay_focused() {
        let mut s = session();
        // Someone I wrote to, a reply to my message (from a list), a contact, me.
        let sent = arrive(
            &mut s,
            "From: Me Myself <me@example.com>\nTo: Dana <dana@example.org>\nMessage-ID: <mine-1@example.com>\n\
             Subject: plan\n\nx\n",
        );
        let sent_folder = role(&s, FolderRole::SentItems);
        s.mailbox.move_items(&[sent], sent_folder).unwrap();
        let list = "List-Id: <team.example.net>\n";
        assert_eq!(
            class(&mut s, &format!("From: dana@example.org\n{list}Subject: a\n\nx\n")),
            (FocusClass::Focused, Reason::KnownSender)
        );
        assert_eq!(
            class(
                &mut s,
                &format!("From: eve@example.net\n{list}In-Reply-To: <MINE-1@example.com>\nSubject: b\n\nx\n")
            ),
            (FocusClass::Focused, Reason::WithMe)
        );
        // A contact (added here, so the case never depends on what the session starts with).
        let id = ItemId(s.mailbox.alloc_id());
        s.mailbox.contacts.insert(
            id,
            mailcraft_model::Contact {
                id,
                first_name: "Gus".into(),
                last_name: "Hale".into(),
                emails: vec!["Gus.Hale@Example.net".into()],
                ..Default::default()
            },
        );
        assert_eq!(
            class(&mut s, &format!("From: gus.hale@example.net\n{list}Subject: c\n\nx\n")),
            (FocusClass::Focused, Reason::KnownSender)
        );
        assert_eq!(
            class(&mut s, &format!("From: hal@example.net\n{list}Subject: c\n\nx\n")).0,
            FocusClass::Other
        );
        assert_eq!(
            class(&mut s, "From: me@example.com\nPrecedence: bulk\nSubject: d\n\nx\n").1,
            Reason::WithMe
        );
        // A draft's recipients are not people I wrote to.
        let draft = arrive(&mut s, "From: me@example.com\nTo: fay@example.com\nSubject: e\n\nx\n");
        s.mailbox.mail.get_mut(draft).unwrap().is_draft = true;
        assert!(!known(&s).people.contains("fay@example.com"));
        // An invitation is Focused even from a no-reply address.
        let invite = "From: noreply@example.net\nSubject: Invitation\nMIME-Version: 1.0\n\
                      Content-Type: multipart/mixed; boundary=\"b\"\n\n--b\nContent-Type: text/plain\n\nHi\n\
                      --b\nContent-Type: text/calendar; method=REQUEST\nContent-Disposition: attachment; \
                      filename=\"invite.ics\"\n\nBEGIN:VCALENDAR\nEND:VCALENDAR\n--b--\n";
        assert_eq!(class(&mut s, invite).1, Reason::Invitation);
    }

    #[test]
    fn sender_overrides_win() {
        let mut s = session();
        s.settings.set(FOCUSED_SENDERS_SETTING, &json!(["News@Example.org"])).unwrap();
        s.settings.set(crate::cmd::mail::OTHER_SENDERS_SETTING, &json!(["ann@example.com"])).unwrap();
        assert_eq!(
            class(&mut s, "From: news@example.org\nList-Id: <n.example.org>\nSubject: a\n\nx\n"),
            (FocusClass::Focused, Reason::SenderOverride)
        );
        assert_eq!(
            class(&mut s, "From: ann@example.com\nSubject: b\n\nx\n"),
            (FocusClass::Other, Reason::SenderOverride)
        );
    }

    #[test]
    fn new_mail_is_classified_and_journaled() {
        let mut s = session();
        let a = arrive(&mut s, "From: news@example.org\nList-Id: <n.example.org>\nSubject: a\n\nx\n");
        let b = arrive(&mut s, "From: ann@example.com\nSubject: b\n\nx\n");
        assert_eq!(on_new_mail(&mut s, &[a, b, ItemId(u64::MAX)]), vec![a]);
        assert_eq!(s.mailbox.mail.get(a).unwrap().focus, FocusClass::Other);
        assert_eq!(s.mailbox.mail.get(b).unwrap().focus, FocusClass::Focused);
        assert!(on_new_mail(&mut s, &[]).is_empty());
        // Junk and odd input never panics.
        let mut m = MailItem::default();
        assert_eq!(classify(&m, None, &Known::default()).1, Reason::Default);
        m.from = Some(mailcraft_model::Address::new(None, "@"));
        let _ = classify(&m, None, &Known::default());
        assert!(!is_no_reply(""));
        assert!(is_no_reply("No.Reply@example.com"));
        assert!(!is_no_reply("nora@example.com"));
        assert!(!is_no_reply("reply@noreply.example.com"), "only the local part counts");
    }
}
