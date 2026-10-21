//! IMAP: response parser, client, sequence sets, modified UTF-7, namespaces (RFC 2342), and folder-role
//! mapping.

pub mod client;
pub mod namespace;
pub mod parse;
pub mod seqset;
pub mod utf7;

pub use client::{CopyUid, Done, IdleEvent, ImapClient, Limits, MailboxInfo, StoreMode};
pub use parse::{
    Address, BodyStructure, Code, Disposition, Envelope, Fetch, ListEntry, QuotaResource, Response,
    SinglePart, Status, Untagged, Value, parse_response,
};
pub use seqset::SeqSet;

use mailcraft_model::FolderRole;

/// Maps RFC 6154 special-use attributes (and `INBOX`) to a folder role.
pub fn role_from_attributes(name: &str, attributes: &[String]) -> Option<FolderRole> {
    if name.eq_ignore_ascii_case("INBOX") {
        return Some(FolderRole::Inbox);
    }
    attributes.iter().find_map(|a| match a.to_ascii_lowercase().as_str() {
        "\\sent" => Some(FolderRole::SentItems),
        "\\drafts" => Some(FolderRole::Drafts),
        "\\trash" => Some(FolderRole::DeletedItems),
        "\\junk" => Some(FolderRole::JunkEmail),
        "\\archive" => Some(FolderRole::Archive),
        _ => None,
    })
}

/// Guesses a role from a common folder name, for servers without SPECIAL-USE. `leaf` is the last
/// hierarchy segment.
pub fn role_from_name(leaf: &str) -> Option<FolderRole> {
    match leaf.trim().to_ascii_lowercase().as_str() {
        "inbox" => Some(FolderRole::Inbox),
        "sent" | "sent items" | "sent messages" | "sent mail" => Some(FolderRole::SentItems),
        "drafts" | "draft" => Some(FolderRole::Drafts),
        "trash" | "deleted items" | "deleted messages" | "bin" => Some(FolderRole::DeletedItems),
        "junk" | "spam" | "junk email" | "junk e-mail" | "bulk mail" => Some(FolderRole::JunkEmail),
        "archive" | "archives" => Some(FolderRole::Archive),
        "outbox" => Some(FolderRole::Outbox),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(role_from_attributes("inbox", &[]), Some(FolderRole::Inbox));
        assert_eq!(
            role_from_attributes("x", &a(&["\\HasNoChildren", "\\Sent"])),
            Some(FolderRole::SentItems)
        );
        assert_eq!(role_from_attributes("x", &a(&["\\TRASH"])), Some(FolderRole::DeletedItems));
        assert_eq!(role_from_attributes("x", &a(&["\\Junk"])), Some(FolderRole::JunkEmail));
        assert_eq!(role_from_attributes("x", &a(&["\\Drafts"])), Some(FolderRole::Drafts));
        assert_eq!(role_from_attributes("x", &a(&["\\Archive"])), Some(FolderRole::Archive));
        assert_eq!(role_from_attributes("x", &a(&["\\All", "\\Flagged"])), None);
        assert_eq!(role_from_name("Sent Items"), Some(FolderRole::SentItems));
        assert_eq!(role_from_name("Spam"), Some(FolderRole::JunkEmail));
        assert_eq!(role_from_name("Projects"), None);
    }
}
