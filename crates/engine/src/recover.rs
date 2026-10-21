//! Recoverable items (Folder › Recover Deleted Items).
//!
//! Every item a command removes from the mailbox for good (emptying Deleted Items, Shift+Delete, deleting in
//! Deleted Items, a rule or Quick Step that deletes permanently, AutoArchive deleting old items) goes into
//! a [`RecoverBin`] kept beside the mailbox: in the store's `recoverable.json` for a profile on disk, in
//! memory otherwise. It survives restarts and does not depend on the undo history. Entries older than
//! Options › Advanced › "Keep permanently deleted items recoverable for (days)" are purged when the
//! profile opens and then hourly. `Session::run` captures what a command deletes; work that runs outside a
//! command calls [`capture`] itself: new mail being processed (rules, the Junk Email filter) and scheduled
//! AutoArchive.
//!
//! Removals that are not deletions are not captured: sending a draft (the Outbox copy replaces it), closing
//! a data file and removing an account (their items leave with them).

use std::collections::{BTreeMap, BTreeSet};

use mailcraft_model::{AccountId, FolderId, ItemId, MAX_RECOVERABLE, Mailbox, RecoverBin, Recoverable};

use crate::{Notice, Session};

/// The option holding how many days deleted items stay recoverable.
pub const DAYS_KEY: &str = "advanced.recoverDays";

/// Seconds between scheduled purges.
const PURGE_EVERY_SECS: i64 = 3_600;

/// The bin of a session and its bookkeeping.
#[derive(Debug, Default)]
pub struct RecoverState {
    pub bin: RecoverBin,
    /// When the last purge ran (Unix seconds; 0 = not yet).
    last_purge: i64,
    /// A write failed and the user has been told (shown once until a write works again).
    save_error_shown: bool,
}

/// Days deleted items stay recoverable.
pub fn keep_days(s: &Session) -> u32 {
    u32::try_from(s.settings.int(DAYS_KEY)).unwrap_or(0)
}

/// Reads the bin from the store (at open). An unreadable file is set aside as `recoverable.json.bad` and an
/// empty bin is used.
pub(crate) fn load(s: &mut Session) {
    let Some(store) = s.store.as_ref() else { return };
    match store.load_recoverable() {
        Ok(bin) => s.recover.bin = bin,
        Err(e) => {
            log::error!("recoverable items unreadable: {e}");
            let dir = store.dir.clone();
            let file = dir.join(mailcraft_store::RECOVERABLE_FILE);
            let _ = std::fs::rename(&file, dir.join(format!("{}.bad", mailcraft_store::RECOVERABLE_FILE)));
            s.notify(Notice::Error(format!(
                "The list of recoverable deleted items could not be read ({e}); it starts empty."
            )));
        }
    }
    purge_expired(s);
}

/// Writes the bin to the store (no-op for in-memory sessions). Failures are logged and shown once.
fn save(s: &mut Session) {
    let Some(store) = s.store.as_mut() else { return };
    match store.save_recoverable(&mut s.recover.bin) {
        Ok(()) => s.recover.save_error_shown = false,
        Err(e) => {
            log::error!("cannot write the recoverable items: {e}");
            if !s.recover.save_error_shown {
                s.recover.save_error_shown = true;
                s.notify(Notice::Error(format!("Deleted items could not be kept for recovery ({e}).")));
            }
        }
    }
}

/// Puts the items that are in `before` and gone from the session's mailbox into the bin. Items of an
/// account that is gone too (a removed account, a closed data file) left with it and are not captured; at
/// most [`MAX_RECOVERABLE`] items are taken from one change.
pub(crate) fn capture(s: &mut Session, before: &Mailbox) {
    if keep_days(s) == 0 {
        return;
    }
    let live: BTreeSet<AccountId> = s.mailbox.accounts.iter().map(|a| a.id).collect();
    let kept = |f: FolderId| before.folder(f).is_none_or(|folder| live.contains(&folder.account));
    let gone = RecoverBin::vanished_where(before, &s.mailbox, kept, MAX_RECOVERABLE);
    if gone.is_empty() {
        return;
    }
    let now = s.now().unix;
    let mut paths: BTreeMap<FolderId, String> = BTreeMap::new();
    let items: Vec<_> = gone
        .into_iter()
        .map(|item| {
            let path =
                paths.entry(item.folder()).or_insert_with(|| before.folder_path(item.folder())).clone();
            (item, path)
        })
        .collect();
    s.recover.bin.add_all(items, now);
    save(s);
}

/// Drops entries past the retention period; runs at most hourly from the sync tick (and at open).
pub(crate) fn tick(s: &mut Session) {
    let now = s.now().unix;
    if s.recover.last_purge != 0 && now.saturating_sub(s.recover.last_purge) < PURGE_EVERY_SECS {
        return;
    }
    purge_expired(s);
}

fn purge_expired(s: &mut Session) {
    let now = s.now().unix;
    s.recover.last_purge = now;
    let days = keep_days(s);
    if s.recover.bin.purge(now, days) > 0 {
        save(s);
    }
}

/// Entries Recover Deleted Items lists, newest first.
pub fn listed(s: &Session) -> Vec<&Recoverable> {
    s.recover.bin.listed(&s.mailbox).collect()
}

/// Marks bin entries as recovered: `pairs` are (original id, id it came back as).
pub(crate) fn mark_recovered(s: &mut Session, pairs: &[(ItemId, ItemId)]) {
    if pairs.is_empty() {
        return;
    }
    for e in &mut s.recover.bin.items {
        if e.restored_as.is_some() {
            continue;
        }
        if let Some((_, new)) = pairs.iter().find(|(old, _)| *old == e.item.id()) {
            e.restored_as = Some(*new);
        }
    }
    save(s);
}

/// Purge Selected Items: removes listed entries for good. Returns how many went.
pub(crate) fn purge(s: &mut Session, ids: &[ItemId]) -> usize {
    let n = s.recover.bin.remove(ids, &s.mailbox);
    if n > 0 {
        save(s);
    }
    n
}

/// The retention period changed: apply it now.
pub(crate) fn days_changed(s: &mut Session) {
    purge_expired(s);
}
