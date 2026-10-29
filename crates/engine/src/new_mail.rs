//! New-mail indicator: Options › Mail › Message arrival › "Show an envelope icon in the taskbar".
//!
//! Send/receive hands newly arrived mail to [`note_arrivals`] once the junk filter and the rules have run
//! (`sync::arrival`). Those messages stay "new" until they are read, deleted or junked, or until their folder
//! is shown in the message list ([`seen_folder`], called when a folder is opened). [`envelope`] counts them
//! for the shell, which shows the envelope (window icon badge where the platform shows one, and an envelope in
//! the status bar) while the count is not zero and the option is on.

use std::collections::BTreeSet;

use mailcraft_model::{FolderId, FolderKind, FolderRole, ItemId, Mailbox};

use crate::Session;

/// Most new messages remembered (a first download can bring thousands; the indicator only needs some).
pub const MAX_NEW: usize = 10_000;

/// Remembers `ids` (newly arrived mail, after the junk filter and the rules) as new. When the set would
/// overflow it first drops what no longer counts (read, deleted, junked since), so stale entries for mail in
/// folders nobody opens cannot stop later arrivals from being tracked.
pub(crate) fn note_arrivals(s: &mut Session, ids: &[ItemId]) {
    let skip = excluded_folders(&s.mailbox);
    let mb = &s.mailbox;
    let new = &mut s.sync.new_mail;
    if new.len().saturating_add(ids.len()) > MAX_NEW {
        new.retain(|id| counts(mb, &skip, *id));
    }
    for id in ids {
        if new.len() >= MAX_NEW {
            break;
        }
        if counts(mb, &skip, *id) {
            new.insert(*id);
        }
    }
}

/// The user looked at `folder`: its new messages are no longer new.
pub(crate) fn seen_folder(s: &mut Session, folder: FolderId) {
    if s.sync.new_mail.is_empty() {
        return;
    }
    let mb = &s.mailbox;
    s.sync.new_mail.retain(|id| mb.mail.get(*id).is_some_and(|m| m.folder != folder));
}

/// Folders whose mail never counts as new: every Deleted Items and Junk Email folder with its subfolders,
/// worked out once per count rather than once per message (the shell counts every frame).
fn excluded_folders(mb: &Mailbox) -> BTreeSet<FolderId> {
    mb.folders
        .values()
        .filter(|f| matches!(f.role, Some(FolderRole::DeletedItems | FolderRole::JunkEmail)))
        .flat_map(|f| mb.subtree(f.id))
        .collect()
}

/// True while `id` is unread mail outside the folders in `skip` ([`excluded_folders`]).
fn counts(mb: &Mailbox, skip: &BTreeSet<FolderId>, id: ItemId) -> bool {
    let Some(m) = mb.mail.get(id) else { return false };
    if m.read || m.is_draft || skip.contains(&m.folder) {
        return false;
    }
    mb.folder(m.folder).is_some_and(|f| f.kind == FolderKind::Mail)
}

/// New messages waiting (unread, not looked at yet), or 0 when the envelope option is off.
pub fn envelope(s: &Session) -> usize {
    if !s.settings.bool("mail.arrival.envelope") || s.sync.new_mail.is_empty() {
        return 0;
    }
    let skip = excluded_folders(&s.mailbox);
    s.sync.new_mail.iter().filter(|id| counts(&s.mailbox, &skip, **id)).count()
}

/// The folder holding the newest waiting new message (where clicking the envelope goes).
pub fn envelope_folder(s: &Session) -> Option<FolderId> {
    if s.sync.new_mail.is_empty() {
        return None;
    }
    let skip = excluded_folders(&s.mailbox);
    s.sync
        .new_mail
        .iter()
        .filter(|id| counts(&s.mailbox, &skip, **id))
        .filter_map(|id| s.mailbox.mail.get(*id))
        .max_by_key(|m| (m.received.unix, m.id))
        .map(|m| m.folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::sample_session;
    use serde_json::json;

    fn unread_inbox_mail(s: &mut Session) -> (ItemId, FolderId) {
        let inbox = s.selection.folder;
        let m = s.mailbox.items_in(inbox).first().map(|m| m.id).unwrap();
        s.mailbox.set_read(&[m], false);
        (m, inbox)
    }

    #[test]
    fn arrivals_light_the_envelope_until_read_or_seen() {
        let mut s = sample_session();
        let (m, inbox) = unread_inbox_mail(&mut s);
        assert_eq!(envelope(&s), 0);
        crate::sync::arrival::on_new_mail(&mut s, &[m]);
        assert_eq!(envelope(&s), 1);
        assert_eq!(envelope_folder(&s), Some(inbox));
        // Option off: no envelope (the arrival is still remembered).
        s.run("options.set", &json!({"key": "mail.arrival.envelope", "value": false})).unwrap();
        assert_eq!(envelope(&s), 0);
        s.run("options.set", &json!({"key": "mail.arrival.envelope", "value": true})).unwrap();
        assert_eq!(envelope(&s), 1);
        // Reading it puts the envelope out.
        s.mailbox.set_read(&[m], true);
        assert_eq!(envelope(&s), 0);
        // Showing its folder does too.
        s.mailbox.set_read(&[m], false);
        crate::sync::arrival::on_new_mail(&mut s, &[m]);
        assert_eq!(envelope(&s), 1);
        s.run("nav.goToFolder", &json!({"folder": inbox.0})).unwrap();
        assert_eq!(envelope(&s), 0);
        // Read mail never counts as new.
        let read = s.mailbox.items_in(inbox).iter().find(|x| x.read).map(|x| x.id).unwrap();
        crate::sync::arrival::on_new_mail(&mut s, &[read]);
        assert_eq!(envelope(&s), 0);
        assert_eq!(envelope_folder(&s), None);
    }

    #[test]
    fn mail_in_deleted_items_or_junk_is_not_new() {
        let mut s = sample_session();
        let (m, inbox) = unread_inbox_mail(&mut s);
        crate::sync::arrival::on_new_mail(&mut s, &[m]);
        assert_eq!(envelope(&s), 1);
        let account = s.mailbox.folder_account(inbox).unwrap();
        for role in [FolderRole::DeletedItems, FolderRole::JunkEmail] {
            let root = s.mailbox.role_folder(account, role).unwrap();
            // A subfolder of it counts as it does.
            let sub = s.mailbox.create_folder(root, "Kept").unwrap();
            for target in [root, sub] {
                s.mailbox.move_items(&[m], target).unwrap();
                assert_eq!(envelope(&s), 0, "{role:?}");
                assert_eq!(envelope_folder(&s), None, "{role:?}");
            }
            // Back in the Inbox (unread, never looked at): new again.
            s.mailbox.move_items(&[m], inbox).unwrap();
            assert_eq!(envelope(&s), 1, "{role:?}");
        }
    }

    #[test]
    fn a_full_set_drops_stale_entries_so_new_arrivals_still_count() {
        let mut s = sample_session();
        let (m, _) = unread_inbox_mail(&mut s);
        // Entries for mail that is gone (or read, or moved to folders nobody opens) fill the set.
        let first_free = s.mailbox.mail.ids().max().map_or(0, |i| i.0).saturating_add(1);
        let stale = u64::try_from(MAX_NEW).unwrap();
        s.sync.new_mail.extend((first_free..first_free.saturating_add(stale)).map(ItemId));
        assert_eq!(s.sync.new_mail.len(), MAX_NEW);
        assert_eq!(envelope(&s), 0);
        crate::sync::arrival::on_new_mail(&mut s, &[m]);
        assert_eq!(envelope(&s), 1, "the arrival is tracked");
        assert_eq!(s.sync.new_mail.len(), 1, "stale entries are gone");
    }
}
