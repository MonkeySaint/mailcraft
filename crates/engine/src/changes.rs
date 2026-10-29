//! Change sets: what a command, an undo step, a sync batch or an automatic action changed, without copying
//! and comparing the whole mailbox (storage stage S3, `plan/storage-adr.md` §3.5 and §4.2).
//!
//! [`Session::begin_changes`] copies everything but the mail (folders, accounts, calendar, contacts, tasks,
//! notes and configuration: small next to the mail) and opens a change record on the mail table, which keeps
//! the before-image of each item the first time it is written. [`Session::end_changes`] closes the record and
//! returns a [`ChangeSet`]: the small "before" plus the mail before-images. From it come the store ops
//! ([`ChangeSet::ops`]), the server changes (`sync::queue_local_changes`), the recoverable items
//! (`recover::capture`) and the undo step (`UndoEntry::from_changes`). The work is proportional to what
//! changed, not to the mailbox.
//!
//! **Safety net.** In debug builds and tests, `begin_changes` also keeps a full copy of the mailbox, and
//! [`ChangeSet::ops`] checks that the ops it derives equal the old whole-mailbox comparison
//! ([`crate::session::diff_ops`]) and that they replay onto the mailbox before into the mailbox now (the
//! comparison shares `small_ops` with the record, so only the replay checks the configuration and sync
//! ops). When either fails a debug build logs it and journals the whole mailbox
//! instead (it never panics: an app run with `cargo run` must not lose open drafts over it); the engine's
//! unit tests and the tests built with the `strict-change-check` feature (the engine's integration tests,
//! `tests/fuzz.rs` among them, which runs every command with hostile parameters) fail. Release builds keep no
//! copy.
//!
//! **Lost records.** A record only disappears when the mailbox is replaced by one that did not come from it
//! (a clone taken inside the record keeps it). The change set then has no mail part: it journals the whole
//! mailbox, records no undo step (the history is cleared), and queues no server changes. A failed command
//! whose record was lost cannot be rolled back: its changes stay and the whole mailbox is journaled, so the
//! store keeps matching memory.
//!
//! **Accounts and configuration** (storage stage S4). A command on synchronised mail appends to its account's
//! queue of server changes, so the account's sync state changes with it: that journals one `Op::PutSync` per
//! changed account next to the per-item ops. Accounts are compared without their sync state; settings
//! changed, accounts added, removed or reordered, and the other configuration (categories, rules, quick
//! steps, views, signatures, Auto-Complete, dictionary, junk lists) journal one `Op::PutConfig` with the parts
//! that changed. Only calendar, contact, task and note changes still journal a whole-mailbox `Replace`, until
//! storage stage S12.

use mailcraft_model::{ChangeToken, MailChanges, Mailbox};
use mailcraft_store::Op;

use crate::Session;

/// An open change record ([`Session::begin_changes`]).
#[derive(Debug)]
#[must_use = "a change record must be closed with end_changes or rollback_changes"]
pub(crate) struct Recording {
    token: ChangeToken,
    small: Box<Mailbox>,
    #[cfg(any(test, debug_assertions))]
    full: Box<Mailbox>,
}

impl Recording {
    /// Everything but the mail as it was when the record opened.
    pub(crate) fn before_small(&self) -> &Mailbox {
        &self.small
    }

    /// The mail before-images written so far (see [`mailcraft_model::MailTable::recorded`]).
    pub(crate) fn mail_so_far<'a>(
        &self,
        now: &'a Mailbox,
    ) -> Vec<(mailcraft_model::ItemId, Option<&'a mailcraft_model::MailItem>)> {
        now.mail.recorded(self.token)
    }
}

/// What changed between [`Session::begin_changes`] and [`Session::end_changes`].
#[derive(Debug)]
pub(crate) struct ChangeSet {
    /// Everything but the mail, as it was before ([`Mailbox::without_mail`]).
    pub(crate) small: Box<Mailbox>,
    /// The before-image of every mail item written; `None` when the record was lost.
    pub(crate) mail: Option<MailChanges>,
    /// The whole mailbox as it was before (debug builds and tests): [`ChangeSet::ops`] checks against it.
    #[cfg(any(test, debug_assertions))]
    full: Option<Box<Mailbox>>,
}

impl ChangeSet {
    /// A change set from a small "before" and mail before-images (no full copy to check against).
    pub(crate) fn new(small: Box<Mailbox>, mail: Option<MailChanges>) -> ChangeSet {
        ChangeSet {
            small,
            mail,
            #[cfg(any(test, debug_assertions))]
            full: None,
        }
    }

    /// Debug builds and tests: the whole mailbox as it was before, for [`ChangeSet::ops`] to check against.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn set_full(&mut self, full: Mailbox) {
        self.full = Some(Box::new(full));
    }

    /// Adds the mail changes of work done after these (the older before-images win).
    pub(crate) fn absorb(&mut self, later: Option<MailChanges>) {
        match (&mut self.mail, later) {
            (Some(mail), Some(later)) => mail.absorb(later),
            (Some(_), None) => {
                log::error!(
                    "the record of a follow-up change to the mail was lost; saving the whole mailbox"
                );
                self.mail = None;
            }
            (None, _) => {}
        }
    }

    /// The store ops turning the "before" into `now` (the same ops [`crate::session::diff_ops`] gives, from
    /// the change record instead of a full comparison). Drops the items that were touched but are unchanged.
    pub(crate) fn ops(&mut self, now: &Mailbox) -> Vec<Op> {
        let ops = match self.mail.as_mut() {
            Some(mail) => {
                mail.retain_changed(&now.mail);
                ops_for(&self.small, mail, now)
            }
            None => vec![Op::Replace(Box::new(now.clone()))],
        };
        // A `Replace` (a lost record) is right whatever happened: only ops from a record are checked.
        // The full copy is used up by the check (a change set's ops are taken once).
        #[cfg(any(test, debug_assertions))]
        if self.mail.is_some()
            && let Some(full) = self.full.take()
            && !check_ops(*full, now, &ops)
        {
            return vec![Op::Replace(Box::new(now.clone()))];
        }
        ops
    }
}

/// Store ops from a small "before", the mail before-images and the mailbox now: configuration and sync state
/// ops, then per-folder and per-item upserts and removals; or one `Replace` when the calendar, contacts, tasks
/// or notes changed. Same order as `diff_ops`.
pub(crate) fn ops_for(before: &Mailbox, mail: &MailChanges, after: &Mailbox) -> Vec<Op> {
    let Some(mut ops) = crate::session::small_ops(before, after) else {
        return vec![Op::Replace(Box::new(after.clone()))];
    };
    for (id, f) in &after.folders {
        if before.folders.get(id) != Some(f) {
            ops.push(Op::PutFolder(Box::new(f.clone())));
        }
    }
    let mut removed = Vec::new();
    for (id, b, a) in mail.changed(&after.mail) {
        match a {
            Some(a) => ops.push(Op::PutMail(Box::new(a.clone()))),
            None if b.is_some() => removed.push(Op::RemoveMail(id)),
            None => {}
        }
    }
    ops.extend(removed);
    for id in before.folders.keys() {
        if !after.folders.contains_key(id) {
            ops.push(Op::RemoveFolder(*id));
        }
    }
    ops
}

/// Debug builds and tests: whether the ops from a change record equal the full comparison, and whether they
/// replay onto the mailbox before into the mailbox now. The comparison alone cannot catch a mistake in the
/// configuration and sync ops (`small_ops` builds them for both sides from the same input); the replay
/// checks what the store will rebuild on the next start. A disagreement is logged (the caller then journals
/// the whole mailbox) and fails a test build ([`strict_failure`]).
#[cfg(any(test, debug_assertions))]
fn check_ops(full: Mailbox, now: &Mailbox, ops: &[Op]) -> bool {
    let names = |ops: &[Op]| -> Vec<String> {
        ops.iter()
            .map(|o| match o {
                Op::PutMail(m) => format!("PutMail({})", m.id.0),
                Op::RemoveMail(id) => format!("RemoveMail({})", id.0),
                Op::PutFolder(f) => format!("PutFolder({})", f.id.0),
                Op::RemoveFolder(id) => format!("RemoveFolder({})", id.0),
                Op::PutAccount(a) => format!("PutAccount({})", a.id.0),
                Op::PutSync(id, _) => format!("PutSync({})", id.0),
                Op::PutConfig(_) => "PutConfig".to_owned(),
                Op::Replace(_) => "Replace".to_owned(),
                Op::Batch(ops) => format!("Batch({})", ops.len()),
            })
            .collect()
    };
    let expected = crate::session::diff_ops(&full, now);
    if expected.as_slice() != ops {
        let msg = format!(
            "change record and full comparison disagree: recorded {:?}, compared {:?}",
            names(ops),
            names(&expected)
        );
        log::error!("{msg}; saving the whole mailbox");
        strict_failure(&msg);
        return false;
    }
    if let Some(what) = replay_mismatch(full, now, ops) {
        let msg = format!("change record ops {:?} do not replay to the mailbox: {what}", names(ops));
        log::error!("{msg}; saving the whole mailbox");
        strict_failure(&msg);
        return false;
    }
    true
}

/// Debug builds and tests: replays `ops` onto `before` (as the store does on the next start) and
/// names the first part of the mailbox that comes out different from `now`; `None` when it all matches.
/// `next_id` is not compared (replay only keeps it ahead of the ids it inserts), nor the view settings of a
/// folder that was removed (`RemoveFolder` drops them on replay; a stray entry in memory is harmless).
#[cfg(any(test, debug_assertions))]
fn replay_mismatch(before: Mailbox, now: &Mailbox, ops: &[Op]) -> Option<&'static str> {
    let mut replayed = before;
    for op in ops.iter().cloned() {
        let removed = match &op {
            Op::RemoveFolder(id) => now.views.get(id).cloned().map(|v| (*id, v)),
            _ => None,
        };
        op.apply(&mut replayed);
        if let Some((id, v)) = removed {
            replayed.views.insert(id, v);
        }
    }
    replayed.next_id = now.next_id;
    if replayed == *now {
        return None;
    }
    // Exhaustive on purpose: a new Mailbox field fails to compile here until it is compared.
    let Mailbox {
        accounts,
        categories,
        rules,
        quick_steps,
        views,
        signatures,
        signature_defaults,
        recent_recipients,
        custom_dictionary,
        junk,
        folders,
        mail,
        next_id: _,
        appointments,
        contacts,
        tasks,
        notes,
    } = now;
    let parts: [(&'static str, bool); 16] = [
        ("accounts", replayed.accounts == *accounts),
        ("categories", replayed.categories == *categories),
        ("rules", replayed.rules == *rules),
        ("quick steps", replayed.quick_steps == *quick_steps),
        ("views", replayed.views == *views),
        ("signatures", replayed.signatures == *signatures),
        ("signature defaults", replayed.signature_defaults == *signature_defaults),
        ("recent recipients", replayed.recent_recipients == *recent_recipients),
        ("custom dictionary", replayed.custom_dictionary == *custom_dictionary),
        ("junk lists", replayed.junk == *junk),
        ("folders", replayed.folders == *folders),
        ("mail", replayed.mail == *mail),
        ("appointments", replayed.appointments == *appointments),
        ("contacts", replayed.contacts == *contacts),
        ("tasks", replayed.tasks == *tasks),
        ("notes", replayed.notes == *notes),
    ];
    Some(parts.iter().find(|(_, same)| !same).map_or("the mailbox", |(name, _)| *name))
}

/// Test builds (the engine's unit tests, and builds with the `strict-change-check` feature, which the
/// engine's integration tests turn on): a change record that disagrees with the full comparison fails the
/// test. Never in an app build, debug or release.
#[cfg(any(test, all(debug_assertions, feature = "strict-change-check")))]
#[allow(clippy::panic, reason = "test builds only: a record that missed a change must fail the test")]
fn strict_failure(msg: &str) {
    panic!("{msg}");
}

#[cfg(all(debug_assertions, not(test), not(feature = "strict-change-check")))]
fn strict_failure(_: &str) {}

impl Session {
    /// Opens a change record (see the module docs). Close it with [`Session::end_changes`] or
    /// [`Session::rollback_changes`].
    pub(crate) fn begin_changes(&mut self) -> Recording {
        let small = Box::new(self.mailbox.without_mail());
        let token = self.mailbox.mail.begin();
        Recording {
            token,
            small,
            #[cfg(any(test, debug_assertions))]
            full: Box::new(self.mailbox.clone()),
        }
    }

    /// Closes a change record: what changed since [`Session::begin_changes`].
    pub(crate) fn end_changes(&mut self, r: Recording) -> ChangeSet {
        let mail = self.mailbox.mail.commit(r.token);
        if mail.is_none() {
            log::error!("the record of the mail a change touched was lost; saving the whole mailbox");
        }
        ChangeSet {
            small: r.small,
            mail,
            #[cfg(any(test, debug_assertions))]
            full: Some(r.full),
        }
    }

    /// Closes a change record and puts the mailbox back as it was at [`Session::begin_changes`] (a command
    /// that failed or panicked). Returns the changes undone: the mailbox as it was just before, as a change
    /// set leading to the restored state.
    ///
    /// When the record was lost (the mailbox was replaced as a whole) nothing can be put back: the mailbox
    /// stays as the failed change left it, and it is journaled whole so the store matches memory. Debug builds
    /// do the same as release builds here. The change set returned then has no mail part.
    pub(crate) fn rollback_changes(&mut self, r: Recording) -> ChangeSet {
        if !self.mailbox.mail.is_open(r.token) {
            log::error!("the record of what a failed change touched was lost; its changes stay, saved whole");
            self.journal(vec![Op::Replace(Box::new(self.mailbox.clone()))]);
            return ChangeSet::new(Box::new(self.mailbox.without_mail()), None);
        }
        let after_small = Box::new(self.mailbox.replace_all_but_mail(*r.small));
        let mail = self.mailbox.mail.rollback(r.token);
        ChangeSet::new(after_small, mail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::{ItemId, MailItem};

    fn session() -> Session {
        crate::sample::sample_session()
    }

    #[test]
    fn ops_match_the_full_comparison_for_mail_and_folder_changes() {
        let mut s = session();
        let ids: Vec<ItemId> = s.mailbox.mail.ids().take(3).collect();
        let r = s.begin_changes();
        let full = s.mailbox.clone();
        s.mailbox.set_read(&ids, true);
        if let Some(id) = ids.get(1) {
            s.mailbox.mail.remove(*id);
        }
        let inbox = s.selection.folder;
        let new = s.mailbox.add_mail(MailItem { folder: inbox, subject: "new".into(), ..Default::default() });
        // Touched, left as it was.
        if let Some(id) = ids.get(2) {
            let _ = s.mailbox.mail.get_mut(*id);
        }
        let fresh = s.mailbox.create_folder(inbox, "Fresh").unwrap();
        let mut ch = s.end_changes(r);
        let ops = ch.ops(&s.mailbox);
        assert_eq!(ops, crate::session::diff_ops(&full, &s.mailbox));
        assert!(ops.iter().any(|o| matches!(o, Op::PutMail(m) if m.id == new)));
        assert!(ops.iter().any(|o| matches!(o, Op::PutFolder(f) if f.id == fresh)));
        assert!(ops.iter().any(|o| matches!(o, Op::RemoveMail(_))));
        assert!(ch.mail.as_ref().is_some_and(|m| m.contains(new)));
    }

    #[test]
    fn rollback_restores_mail_and_small_state() {
        let mut s = session();
        let before = s.mailbox.clone();
        let r = s.begin_changes();
        let ids: Vec<ItemId> = s.mailbox.mail.ids().collect();
        s.mailbox.set_read(&ids, true);
        s.mailbox.mail.clear();
        s.mailbox.categories.clear();
        let back = s.rollback_changes(r);
        assert_eq!(s.mailbox, before);
        assert!(back.mail.is_some_and(|m| m.len() == ids.len()));
        assert!(back.small.categories.is_empty(), "the state that was undone");
        assert!(!s.mailbox.mail.is_recording());
    }

    /// The debug-build safety net: a change the record missed (here: written while no record was open, as
    /// if a write bypassed the table) makes `ops` disagree with the full comparison, which asserts.
    #[test]
    #[should_panic(expected = "change record and full comparison disagree")]
    fn a_change_the_record_missed_is_caught() {
        let mut s = session();
        let id = s.mailbox.mail.ids().next().unwrap();
        let r = s.begin_changes();
        let images = s.mailbox.mail.commit(r.token).unwrap();
        s.mailbox.mail.update(id, |m| m.subject = "changed behind the record's back".into());
        let mut ch = ChangeSet { small: r.small, mail: Some(images), full: Some(r.full) };
        let _ = ch.ops(&s.mailbox);
    }

    /// Nested records (a command running another) leave the outer record complete.
    #[test]
    fn nested_records_add_up() {
        let mut s = session();
        let ids: Vec<ItemId> = s.mailbox.mail.ids().take(2).collect();
        let outer = s.begin_changes();
        s.mailbox.set_read(&ids[..1], true);
        let inner = s.begin_changes();
        s.mailbox.mail.update(ids[1], |m| m.subject = "inner".into());
        s.mailbox.mail.update(ids[0], |m| m.subject = "inner too".into());
        let mut inner = s.end_changes(inner);
        assert_eq!(inner.mail.as_ref().map(MailChanges::len), Some(2));
        let _ = inner.ops(&s.mailbox);
        // A record left open by a panic is folded into the outer one.
        let _orphan = s.mailbox.mail.begin();
        let fresh = s.mailbox.add_mail(MailItem { folder: s.selection.folder, ..Default::default() });
        let mut outer = s.end_changes(outer);
        let ops = outer.ops(&s.mailbox);
        assert!(ops.iter().any(|o| matches!(o, Op::PutMail(m) if m.id == fresh)));
        assert_eq!(outer.mail.as_ref().map(MailChanges::len), Some(3));
        assert!(!s.mailbox.mail.is_recording());
    }

    /// A failed change whose record was lost (it replaced the mailbox as a whole) cannot be rolled back, in
    /// debug and release builds alike: what it did stays and is saved whole, so the store matches memory.
    #[test]
    fn a_failed_change_with_a_lost_record_is_saved_whole() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path()).unwrap();
        let inbox = s.selection.folder;
        let r = s.begin_changes();
        s.mailbox = serde_json::from_value(serde_json::to_value(&s.mailbox).unwrap()).unwrap();
        let kept =
            s.mailbox.add_mail(MailItem { folder: inbox, subject: "kept".into(), ..Default::default() });
        s.mailbox.categories.clear();
        let back = s.rollback_changes(r);
        assert!(back.mail.is_none());
        assert!(s.mailbox.mail.contains(kept) && s.mailbox.categories.is_empty(), "nothing was put back");
        assert!(!s.mailbox.mail.is_recording());
        let memory = s.mailbox.clone();
        drop(s);
        let s = Session::open(dir.path()).unwrap();
        assert_eq!(s.mailbox.mail, memory.mail);
        assert_eq!(s.mailbox.categories, memory.categories);
        assert_eq!(s.mailbox.folders, memory.folders);
    }

    /// A command on synchronised mail changes its account's sync state (the queue of server changes): that
    /// journals the sync state, not the account or the whole mailbox.
    #[test]
    fn a_sync_state_change_journals_only_the_sync_state() {
        let mut s = session();
        let id = s.mailbox.mail.ids().next().unwrap();
        let r = s.begin_changes();
        let full = s.mailbox.clone();
        s.mailbox.set_read(&[id], true);
        let account = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        if let Some(a) = s.mailbox.accounts.first_mut() {
            a.sync.next_op = a.sync.next_op.wrapping_add(1);
        }
        let ops = s.end_changes(r).ops(&s.mailbox);
        assert_eq!(ops, crate::session::diff_ops(&full, &s.mailbox));
        assert!(matches!(ops.first(), Some(Op::PutSync(a, _)) if *a == account), "{}", ops.len());
        assert!(!ops.iter().any(|o| matches!(o, Op::Replace(_) | Op::PutConfig(_) | Op::PutAccount(_))));
        let mut replayed = full.clone();
        for op in ops {
            op.apply(&mut replayed);
        }
        assert_eq!(replayed, s.mailbox);
    }

    /// Ops from a change record, checked against the full comparison and replayed onto the mailbox before.
    fn recorded(s: &mut Session, change: impl FnOnce(&mut Mailbox)) -> Vec<Op> {
        let r = s.begin_changes();
        let full = s.mailbox.clone();
        change(&mut s.mailbox);
        let ops = s.end_changes(r).ops(&s.mailbox);
        assert_eq!(ops, crate::session::diff_ops(&full, &s.mailbox));
        let mut replayed = full;
        for op in ops.clone() {
            op.apply(&mut replayed);
        }
        assert_eq!(replayed, s.mailbox);
        ops
    }

    /// Accounts added, removed, reordered or with changed settings, and the rest of the configuration,
    /// journal one `PutConfig` with just the parts that changed (accounts without their sync state).
    #[test]
    fn configuration_changes_journal_the_parts_that_changed() {
        let mut s = session();
        let ops = recorded(&mut s, |mb| {
            mb.categories.push(mailcraft_model::Category { name: "Fresh".into(), ..Default::default() })
        });
        let [Op::PutConfig(state)] = ops.as_slice() else { panic!("{ops:?}") };
        assert!(state.categories.is_some() && state.accounts.is_none() && state.rules.is_none());
        // Settings of an account that keeps its place.
        let ops = recorded(&mut s, |mb| {
            if let Some(a) = mb.accounts.first_mut() {
                a.sender_name = "Renamed".into();
            }
        });
        let [Op::PutConfig(state)] = ops.as_slice() else { panic!("{ops:?}") };
        let accounts = state.accounts.as_ref().unwrap();
        assert!(accounts.iter().all(|a| a.sync == Default::default()), "carried without sync state");
        // Removing and reordering keeps the others' sync state; a new account with sync state gets PutSync.
        let ops = recorded(&mut s, |mb| {
            if let Some(a) = mb.accounts.first_mut() {
                a.sync.next_op = 41;
            }
            mb.accounts.reverse();
            let gone = mb.accounts.pop();
            let mut fresh = gone.unwrap_or_default();
            fresh.id = mailcraft_model::AccountId(9_000);
            fresh.sync.next_op = 5;
            mb.accounts.push(fresh);
        });
        assert!(matches!(ops.first(), Some(Op::PutConfig(_))), "{ops:?}");
        assert!(ops.iter().any(|o| matches!(o, Op::PutSync(id, sy) if id.0 == 9_000 && sy.next_op == 5)));
        assert!(!ops.iter().any(|o| matches!(o, Op::Replace(_))));
        // Calendar, contacts, tasks and notes still journal the whole mailbox (until stage S12).
        let ops = recorded(&mut s, |mb| {
            mb.notes.insert(ItemId(8_000_000), mailcraft_model::Note::default());
        });
        assert!(matches!(ops.as_slice(), [Op::Replace(_)]), "{ops:?}");
        // Repeated account ids cannot be said per account.
        let ops = recorded(&mut s, |mb| {
            if let Some(a) = mb.accounts.first().cloned() {
                mb.accounts.push(a);
            }
        });
        assert!(matches!(ops.as_slice(), [Op::Replace(_)]), "{ops:?}");
    }

    /// Storage S4 review: the debug check also replays the ops, so a configuration or sync op that the
    /// comparison agrees with (both sides come from `small_ops`) but that rebuilds a different mailbox is
    /// caught: a part left out, an account's sync state lost, a `PutSync` for an account that is not there.
    #[test]
    fn the_check_replays_the_ops_onto_the_mailbox_before() {
        let s = session();
        let before = s.mailbox.clone();
        let mut now = before.clone();
        now.categories.push(mailcraft_model::Category { name: "Fresh".into(), ..Default::default() });
        let account = now.accounts.first().map(|a| a.id).unwrap();
        if let Some(a) = now.accounts.first_mut() {
            a.sync.next_op = a.sync.next_op.wrapping_add(3);
        }
        let ops = crate::session::diff_ops(&before, &now);
        assert_eq!(replay_mismatch(before.clone(), &now, &ops), None);
        assert!(check_ops(before.clone(), &now, &ops));
        // The new category left out of the configuration op.
        let lost: Vec<Op> = ops.iter().filter(|o| !matches!(o, Op::PutConfig(_))).cloned().collect();
        assert_eq!(replay_mismatch(before.clone(), &now, &lost), Some("categories"));
        // The sync state change left out, or sent to an account that is not there.
        let lost: Vec<Op> = ops.iter().filter(|o| !matches!(o, Op::PutSync(..))).cloned().collect();
        assert_eq!(replay_mismatch(before.clone(), &now, &lost), Some("accounts"));
        let elsewhere: Vec<Op> = ops
            .iter()
            .cloned()
            .map(|o| match o {
                Op::PutSync(id, sy) if id == account => Op::PutSync(mailcraft_model::AccountId(9_999), sy),
                o => o,
            })
            .collect();
        assert_eq!(replay_mismatch(before.clone(), &now, &elsewhere), Some("accounts"));
        // A removed folder's view settings left in memory are no mismatch (replay drops them).
        let mut now = before.clone();
        let folder = now.folders.keys().next_back().copied().unwrap();
        now.folders.remove(&folder);
        now.views.insert(folder, Default::default());
        let ops = crate::session::diff_ops(&before, &now);
        assert_eq!(replay_mismatch(before, &now, &ops), None);
    }

    #[test]
    fn a_replaced_mailbox_loses_the_record_and_journals_everything() {
        let mut s = session();
        let r = s.begin_changes();
        s.mailbox = serde_json::from_value(serde_json::to_value(&s.mailbox).unwrap()).unwrap();
        let mut ch = s.end_changes(r);
        assert!(ch.mail.is_none());
        ch.full = None;
        assert!(matches!(ch.ops(&s.mailbox).as_slice(), [Op::Replace(_)]));
    }
}
