//! Undo/redo. A step keeps what a command changed, as it was before: everything but the mail (folders,
//! accounts, calendar, contacts, tasks, notes, configuration: small next to the mail) and the before-image
//! of each mail item the command wrote (storage stage S3; `changes.rs` records them). Undo puts those back
//! and keeps what it replaced as the redo step, and the other way round.
//!
//! The history is capped twice: by steps ([`DEFAULT_LIMIT`]) and by the estimated memory of the steps it
//! holds ([`DEFAULT_MAX_BYTES`], issue #25), whichever comes first. The newest step is always kept, so Undo
//! works even for a step larger than the byte cap (moving 100,000 messages); spilling such steps to disk is
//! storage stage S10.

use mailcraft_model::{MailChanges, Mailbox};

/// Default undo depth.
pub const DEFAULT_LIMIT: usize = 100;

/// Default cap on the estimated memory of the undo (or redo) history (`plan/storage-adr.md` §3.3). A step
/// holds the mail items its command changed, and (until storage stage S12) a whole copy of everything but
/// the mail: with a large address book or calendar (contact pictures, raw vCard and iCalendar data) that copy
/// alone can be tens of megabytes. The cap stays where it was before change records so such profiles keep
/// as many steps as they did; it bounds the history after a few bulk steps.
pub const DEFAULT_MAX_BYTES: usize = 512 * 1024 * 1024;

/// One undoable step: a label, everything but the mail as it was, and the mail items the step changed as they
/// were.
#[derive(Clone, Debug)]
pub struct UndoEntry {
    pub label: String,
    /// Everything but the mail, as it was ([`Mailbox::without_mail`]: its mail table is empty).
    pub small: Box<Mailbox>,
    /// The before-image of each mail item the step changed (`None`: the item did not exist).
    pub mail: MailChanges,
    /// Estimated memory of the step, worked out once when it is made.
    pub bytes: usize,
}

impl UndoEntry {
    /// A step restoring `small` (everything but the mail) and the mail `before`-images, with its size
    /// estimated. `small`'s own mail table is ignored.
    pub fn new(label: impl Into<String>, small: Box<Mailbox>, mail: MailChanges) -> UndoEntry {
        let bytes = small.approx_bytes().saturating_add(mail.approx_bytes());
        UndoEntry { label: label.into(), small, mail, bytes }
    }

    /// A step back from `after` to `before`, found by comparing the two in full (tests, and code that holds
    /// two whole mailboxes).
    pub fn between(label: impl Into<String>, before: &Mailbox, after: &Mailbox) -> UndoEntry {
        UndoEntry::new(
            label,
            Box::new(before.without_mail()),
            MailChanges::between(&before.mail, &after.mail),
        )
    }

    /// The step reverting a change set; `None` when its mail record was lost (nothing to put back).
    pub(crate) fn from_changes(label: &str, changes: crate::changes::ChangeSet) -> Option<UndoEntry> {
        let crate::changes::ChangeSet { small, mail, .. } = changes;
        Some(UndoEntry::new(label, small, mail?))
    }
}

#[derive(Clone, Debug)]
pub struct UndoStack {
    pub undo: Vec<UndoEntry>,
    pub redo: Vec<UndoEntry>,
    /// Maximum depth (oldest dropped).
    pub limit: usize,
    /// Most estimated bytes each history holds (oldest dropped, the newest step always kept). 0 = default.
    pub max_bytes: usize,
}

impl UndoStack {
    /// Every content key the item images of the undo and redo steps refer to (content collection keeps it, so
    /// undoing a step finds the body and source it puts back).
    pub fn content_keys(&self) -> std::collections::BTreeSet<String> {
        mailcraft_store::bodies::keys_of(
            self.undo.iter().chain(&self.redo).flat_map(|e| e.mail.iter().filter_map(|(_, m)| m)),
        )
    }
}

impl Default for UndoStack {
    fn default() -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit: DEFAULT_LIMIT, max_bytes: DEFAULT_MAX_BYTES }
    }
}

impl UndoStack {
    /// Effective limit (0 is treated as the default, never as "keep nothing").
    fn cap(&self) -> usize {
        if self.limit == 0 { DEFAULT_LIMIT } else { self.limit }
    }

    /// Effective byte cap (0 is treated as the default).
    fn byte_cap(&self) -> usize {
        if self.max_bytes == 0 { DEFAULT_MAX_BYTES } else { self.max_bytes }
    }

    fn trim(list: &mut Vec<UndoEntry>, cap: usize, byte_cap: usize) {
        if list.len() > cap {
            let excess = list.len().saturating_sub(cap);
            list.drain(..excess);
        }
        // Newest first: keep steps while they fit, always at least the newest one.
        let mut total = 0usize;
        let mut keep = 0usize;
        for e in list.iter().rev() {
            total = total.saturating_add(e.bytes);
            if keep > 0 && total > byte_cap {
                break;
            }
            keep = keep.saturating_add(1);
        }
        let excess = list.len().saturating_sub(keep);
        if excess > 0 {
            list.drain(..excess);
        }
    }

    /// Estimated bytes held by the undo and redo histories.
    pub fn bytes(&self) -> usize {
        self.undo.iter().chain(&self.redo).map(|e| e.bytes).fold(0usize, usize::saturating_add)
    }

    /// Records a new step (a fresh command): clears redo, drops the oldest beyond the limits.
    pub fn push(&mut self, entry: UndoEntry) {
        self.redo.clear();
        self.push_keep_redo(entry);
    }

    /// Records a step without touching redo (used by redo).
    pub fn push_keep_redo(&mut self, entry: UndoEntry) {
        self.undo.push(entry);
        let (cap, bytes) = (self.cap(), self.byte_cap());
        Self::trim(&mut self.undo, cap, bytes);
    }

    /// Records a redo step (used by undo).
    pub fn push_redo(&mut self, entry: UndoEntry) {
        self.redo.push(entry);
        let (cap, bytes) = (self.cap(), self.byte_cap());
        Self::trim(&mut self.redo, cap, bytes);
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Label of the step Undo would revert ("Delete", "Move to Folder", …).
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }

    /// Label of the step Redo would reapply.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::{ItemId, MailItem};
    use serde_json::json;

    fn entry(n: usize) -> UndoEntry {
        UndoEntry::new(format!("step {n}"), Box::default(), MailChanges::new())
    }

    fn sized(n: usize, bytes: usize) -> UndoEntry {
        UndoEntry { bytes, ..entry(n) }
    }

    #[test]
    fn limit_drops_oldest() {
        let mut u = UndoStack::default();
        for i in 0..150 {
            u.push(entry(i));
        }
        assert_eq!(u.undo.len(), DEFAULT_LIMIT);
        assert_eq!(u.undo.first().map(|e| e.label.as_str()), Some("step 50"));
        assert_eq!(u.undo_label(), Some("step 149"));
    }

    #[test]
    fn push_clears_redo_but_redo_push_keeps_it() {
        let mut u = UndoStack { limit: 0, ..Default::default() };
        u.push_redo(entry(1));
        u.push_keep_redo(entry(2));
        assert!(u.can_redo());
        u.push(entry(3));
        assert!(!u.can_redo());
        assert!(u.can_undo());
    }

    /// Issue #25: the history is capped by bytes too; the oldest steps go first and the newest stays even
    /// when it alone is over the cap.
    #[test]
    fn byte_cap_drops_oldest_but_keeps_the_newest() {
        let mut u = UndoStack { max_bytes: 1_000, ..Default::default() };
        for i in 0..10 {
            u.push(sized(i, 300));
        }
        assert_eq!(u.undo.len(), 3, "3 × 300 fit in 1000 bytes");
        assert_eq!(u.undo.first().map(|e| e.label.as_str()), Some("step 7"));
        assert!(u.bytes() <= 1_000);
        u.push(sized(10, 5_000));
        assert_eq!(u.undo.len(), 1, "a step over the cap on its own is still kept");
        assert_eq!(u.undo_label(), Some("step 10"));
        u.push_redo(sized(11, 600));
        u.push_redo(sized(12, 600));
        assert_eq!(u.redo.len(), 1, "redo is capped the same way");
        assert_eq!(u.redo_label(), Some("step 12"));
    }

    /// The estimate grows with the before-images a step keeps, so bulk steps use up the cap sooner.
    #[test]
    fn steps_are_sized_from_what_they_keep() {
        let small = entry(0).bytes;
        let mut mail = MailChanges::new();
        for i in 0..20u64 {
            let item = MailItem {
                id: ItemId(i),
                subject: "x".repeat(10_000),
                preview: "y".repeat(5_000),
                ..Default::default()
            };
            mail.record(item.id, Some(item));
        }
        let big = UndoEntry::new("big", Box::default(), mail).bytes;
        assert!(big >= small + 20 * 15_000, "{small} → {big}");
        let mut u = UndoStack { max_bytes: big * 3, ..Default::default() };
        for _ in 0..10 {
            u.push(UndoEntry { bytes: big, ..entry(0) });
        }
        assert_eq!(u.undo.len(), 3);
    }

    /// A step keeps only the items its command changed, not a copy of the mailbox.
    #[test]
    fn a_step_keeps_only_what_its_command_changed() {
        let mut s = crate::sample::sample_session();
        let total = s.mailbox.mail.len();
        assert!(total > 5);
        let id = s.mailbox.mail.items().find(|m| !m.read).map(|m| m.id).unwrap();
        s.run("mail.markRead", &json!({"ids": [id.0]})).unwrap();
        let step = s.undo.undo.last().unwrap();
        assert_eq!(step.mail.ids().collect::<Vec<_>>(), vec![id]);
        assert!(step.mail.before(id).flatten().is_some_and(|m| !m.read));
        assert!(step.small.mail.is_empty());
        assert!(step.bytes < s.mailbox.approx_bytes() / 2, "{} vs {}", step.bytes, s.mailbox.approx_bytes());
        // Undo and redo swap the item back and forth; each keeps the other as its step.
        let before = s.mailbox.clone();
        s.run("app.undo", &json!({})).unwrap();
        assert!(!s.mailbox.mail.get(id).unwrap().read);
        assert!(s.undo.redo.last().unwrap().mail.before(id).flatten().is_some_and(|m| m.read));
        s.run("app.redo", &json!({})).unwrap();
        assert_eq!(s.mailbox, before);
        assert!(!s.mailbox.mail.is_recording(), "no change record left open");
    }

    /// Undo puts back only what its step changed: changes made afterwards without an undo step of their own
    /// (an automatic action, synchronisation) stay, and mail filed into a folder the step created keeps the
    /// folder instead of disappearing with it.
    #[test]
    fn undo_keeps_what_happened_after_the_step() {
        let mut s = crate::sample::sample_session();
        let inbox = s.selection.folder;
        let folders_before: Vec<_> = s.mailbox.folders.keys().copied().collect();
        s.run("folder.new", &json!({"name": "Later", "parent": inbox.0})).unwrap();
        let later = *s.mailbox.folders.keys().find(|f| !folders_before.contains(f)).unwrap();
        // An automatic action files existing mail there (no undo step of its own).
        let moved: Vec<ItemId> = s.mailbox.items_in(inbox).iter().take(2).map(|m| m.id).collect();
        assert_eq!(moved.len(), 2);
        s.run_automatic("mail.move", &json!({"ids": [moved[0].0, moved[1].0], "folder": later.0})).unwrap();
        let unread: Vec<ItemId> = s.mailbox.mail.items().filter(|m| !m.read).map(|m| m.id).take(1).collect();
        s.run_automatic("mail.markRead", &json!({"ids": [unread[0].0]})).unwrap();
        let total = s.mailbox.mail.len();
        s.run("app.undo", &json!({})).unwrap();
        assert_eq!(s.mailbox.mail.len(), total, "no mail dropped");
        assert!(s.mailbox.folders.contains_key(&later), "the folder holding it stays");
        assert!(moved.iter().all(|id| s.mailbox.mail.get(*id).is_some_and(|m| m.folder == later)));
        assert!(s.mailbox.mail.get(unread[0]).unwrap().read, "the later change stays");
        assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
        // Without mail in it, undoing New Folder takes the folder away as before.
        s.run("folder.new", &json!({"name": "Empty", "parent": inbox.0})).unwrap();
        let n = s.mailbox.folders.len();
        s.run("app.undo", &json!({})).unwrap();
        assert_eq!(s.mailbox.folders.len(), n - 1);
        s.run("app.redo", &json!({})).unwrap();
        assert_eq!(s.mailbox.folders.len(), n);
    }

    /// Undo of a bulk move keeps one before-image per moved item and nothing else, and puts them all back.
    #[test]
    fn bulk_move_and_undo_round_trip() {
        let mut s = crate::sample::sample_session();
        let inbox = s.selection.folder;
        let target = s.mailbox.create_folder(inbox, "Bulk").unwrap();
        let ids: Vec<ItemId> = s.mailbox.items_in(inbox).iter().map(|m| m.id).collect();
        assert!(ids.len() > 3);
        let before = s.mailbox.clone();
        let list: Vec<u64> = ids.iter().map(|i| i.0).collect();
        s.run("mail.move", &json!({"ids": list, "folder": target.0})).unwrap();
        let step = s.undo.undo.last().unwrap();
        assert_eq!(step.mail.len(), ids.len());
        let after = s.mailbox.clone();
        s.run("app.undo", &json!({})).unwrap();
        assert_eq!(s.mailbox, before);
        s.run("app.redo", &json!({})).unwrap();
        assert_eq!(s.mailbox, after);
    }

    fn unread(s: &crate::Session) -> ItemId {
        s.mailbox.mail.items().find(|m| !m.read).map(|m| m.id).unwrap()
    }

    /// Undo takes away the item its step made and frees the id. Something made before Redo without an undo
    /// step of its own (mail that arrived, an auto-saved draft, a new folder) must not
    /// get that id, or Redo would write over it.
    #[test]
    fn ids_freed_by_undo_are_not_handed_out_again() {
        let mut s = crate::sample::sample_session();
        let inbox = s.selection.folder;
        let id = s.mailbox.items_in(inbox).first().map(|m| m.id).unwrap();
        let target = s.mailbox.create_folder(inbox, "Copies").unwrap();
        let before: Vec<ItemId> = s.mailbox.mail.ids().collect();
        s.run("mail.copy", &json!({"ids": [id.0], "folder": target.0})).unwrap();
        let copy = s.mailbox.mail.ids().find(|i| !before.contains(i)).unwrap();
        s.run("app.undo", &json!({})).unwrap();
        assert!(!s.mailbox.mail.contains(copy));
        let arrived =
            s.mailbox.add_mail(MailItem { folder: inbox, subject: "arrived".into(), ..Default::default() });
        // A folder made meanwhile must not take the id either (a folder and an item with one id).
        let folder = s.mailbox.create_folder(inbox, "Meanwhile").unwrap();
        // An auto-saved draft (no undo step either).
        let cid = s.run("mail.newEmail", &json!({})).unwrap()["compose"].as_u64().unwrap();
        s.run("compose.insertText", &json!({"compose": cid, "text": "Unsent"})).unwrap();
        let now = s.now();
        s.fixed_now = Some(mailcraft_model::DateTime { unix: now.unix + 3600, offset_min: now.offset_min });
        let saved = s.run("compose.autoSaveTick", &json!({})).unwrap();
        let draft = ItemId(saved["saved"][0]["draft"].as_u64().unwrap());
        assert!(s.undo.can_redo(), "none of that cleared Redo");
        for made in [arrived.0, folder.0, draft.0] {
            assert_ne!(made, copy.0, "id {made} handed out again");
        }
        s.run("app.redo", &json!({})).unwrap();
        assert_eq!(s.mailbox.mail.get(copy).map(|m| m.folder), Some(target));
        assert_eq!(s.mailbox.mail.get(arrived).map(|m| m.subject.as_str()), Some("arrived"));
        assert!(s.mailbox.mail.get(draft).is_some_and(|m| m.is_draft));
        assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
        // The compose window still saves to its own draft.
        s.run("compose.insertText", &json!({"compose": cid, "text": " more"})).unwrap();
        s.run("compose.saveDraft", &json!({"compose": cid})).unwrap();
        assert_eq!(s.mailbox.mail.get(copy).map(|m| m.folder), Some(target), "the copy is left alone");
    }

    fn add_imap(s: &mut crate::Session) -> mailcraft_model::AccountId {
        s.view.work_offline = true;
        let r = s
            .run(
                "accounts.add",
                &json!({
                    "kind": "imap",
                    "address": "undo@example.net",
                    "incoming": {"host": "imap.example.net", "security": "tls"},
                    "outgoing": {"host": "smtp.example.net", "security": "starttls"},
                    "password": "pw",
                }),
            )
            .unwrap();
        mailcraft_model::AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
    }

    /// Every folder belongs to an account that exists, and the mailbox is consistent.
    fn no_orphan_folders(s: &crate::Session) {
        let mb = &s.mailbox;
        assert!(
            mb.folders.values().all(|f| mb.account(f.account).is_some()),
            "a folder outlived its account"
        );
        assert!(mb.mail.items().all(|m| mb.folders.contains_key(&m.folder)), "mail outlived its folder");
        assert!(mb.validate().is_empty(), "{:?}", mb.validate());
    }

    /// Undo of Add Account after mail arrived in the new account: the account's folders and that mail go with
    /// it (no folders left behind without an account).
    #[test]
    fn undo_of_add_account_takes_mail_that_arrived_with_it() {
        let mut s = crate::sample::sample_session();
        let acct = add_imap(&mut s);
        let inbox = s.mailbox.role_folder(acct, mailcraft_model::FolderRole::Inbox).unwrap();
        let arrived =
            s.mailbox.add_mail(MailItem { folder: inbox, subject: "arrived".into(), ..Default::default() });
        let local_mail = s.mailbox.mail.items().filter(|m| m.folder != inbox).count();
        s.run("app.undo", &json!({})).unwrap();
        assert!(s.mailbox.account(acct).is_none());
        assert!(!s.mailbox.mail.contains(arrived));
        assert_eq!(s.mailbox.mail.len(), local_mail, "the other mail stays");
        no_orphan_folders(&s);
        // Redo brings the account back with its folders.
        s.run("app.redo", &json!({})).unwrap();
        assert!(s.mailbox.account(acct).is_some() && s.mailbox.folders.contains_key(&inbox));
        no_orphan_folders(&s);
    }

    /// Redo of Remove Account after mail arrived while the removal was undone: that mail goes too.
    #[test]
    fn redo_of_remove_account_takes_mail_that_arrived_with_it() {
        let mut s = crate::sample::sample_session();
        let acct = add_imap(&mut s);
        s.run("accounts.remove", &json!({"account": acct.0, "confirm": true})).unwrap();
        s.run("app.undo", &json!({})).unwrap();
        let inbox = s.mailbox.role_folder(acct, mailcraft_model::FolderRole::Inbox).unwrap();
        let arrived =
            s.mailbox.add_mail(MailItem { folder: inbox, subject: "arrived".into(), ..Default::default() });
        s.run("app.redo", &json!({})).unwrap();
        assert!(s.mailbox.account(acct).is_none());
        assert!(!s.mailbox.mail.contains(arrived));
        no_orphan_folders(&s);
    }

    /// A record left open by a panic with nothing below it is dropped as the next command starts (and when
    /// send/receive results are applied), instead of copying every item written from then on.
    #[test]
    fn records_left_open_are_dropped() {
        let mut s = crate::sample::sample_session();
        let _stray = s.mailbox.mail.begin();
        let id = unread(&s);
        s.run("mail.markRead", &json!({"ids": [id.0]})).unwrap();
        assert!(!s.mailbox.mail.is_recording());
        assert_eq!(s.undo.undo.last().map(|e| e.mail.len()), Some(1));
        let _stray = s.mailbox.mail.begin();
        s.poll_sync();
        assert!(!s.mailbox.mail.is_recording());
    }

    /// A panic while undo adjusts for what happened after its step (leaving a record open and an item half
    /// changed) puts the mailbox back as it was, keeps the step, and leaves no record open.
    #[test]
    fn a_panic_during_undo_keeps_the_step_and_the_mailbox() {
        let mut s = crate::sample::sample_session();
        let id = unread(&s);
        s.run("mail.markRead", &json!({"ids": [id.0]})).unwrap();
        let before = s.mailbox.clone();
        let label = s.undo.undo_label().map(str::to_owned);
        crate::session::test_hooks::panic_in_next_restore();
        let r = s.run("app.undo", &json!({}));
        assert!(matches!(r, Err(crate::CmdError::Failed(_))), "{r:?}");
        assert_eq!(s.mailbox, before);
        assert!(!s.mailbox.mail.is_recording());
        assert_eq!(s.undo.undo_label().map(str::to_owned), label);
        assert!(!s.undo.can_redo());
        // The step still works.
        s.run("app.undo", &json!({})).unwrap();
        assert!(!s.mailbox.mail.get(id).unwrap().read);
        crate::session::test_hooks::panic_in_next_restore();
        assert!(s.run("app.redo", &json!({})).is_err());
        assert!(s.undo.can_redo() && !s.mailbox.mail.get(id).unwrap().read);
        s.run("app.redo", &json!({})).unwrap();
        assert_eq!(s.mailbox, before);
    }

    /// Until S12 every step copies the address book: a large one (pictures) still leaves a deep history under
    /// the default cap (with a 32 MiB cap this profile kept 5 steps).
    #[test]
    fn a_large_address_book_keeps_a_deep_history() {
        let mut s = crate::sample::sample_session();
        let folder =
            s.mailbox.folders.values().find(|f| f.kind == mailcraft_model::FolderKind::Contacts).unwrap().id;
        for i in 0..96u64 {
            let id = ItemId(s.mailbox.alloc_id());
            let contact = mailcraft_model::Contact {
                id,
                folder,
                last_name: format!("Contact {i}"),
                picture: Some(mailcraft_model::contact::ContactPicture {
                    mime: "image/png".into(),
                    data: vec![7; 64 * 1024],
                }),
                ..Default::default()
            };
            s.mailbox.contacts.insert(id, contact);
        }
        let small = s.mailbox.without_mail().approx_bytes();
        assert!(small > 6 << 20, "{small}");
        let id = s.mailbox.mail.items().find(|m| !m.read).map(|m| m.id).unwrap();
        for i in 0..12 {
            let cmd = if i % 2 == 0 { "mail.markRead" } else { "mail.markUnread" };
            s.run(cmd, &json!({"ids": [id.0]})).unwrap();
        }
        assert_eq!(s.undo.undo.len(), 12, "every step kept");
        assert!(s.undo.undo.iter().all(|e| e.bytes >= small));
    }

    /// The history of a long session stays within the byte cap, whatever the mailbox size.
    #[test]
    fn many_small_steps_stay_small() {
        let mut s = crate::sample::sample_session();
        let ids: Vec<ItemId> = s.mailbox.mail.ids().take(4).collect();
        for i in 0..40 {
            let id = ids[i % ids.len()];
            let cmd = if i % 2 == 0 { "mail.markRead" } else { "mail.markUnread" };
            let _ = s.run(cmd, &json!({"ids": [id.0]}));
        }
        assert!(s.undo.undo.iter().all(|e| e.mail.len() <= 1));
        assert!(s.undo.bytes() < 40 * (s.mailbox.without_mail().approx_bytes() + 64 * 1024));
    }
}
