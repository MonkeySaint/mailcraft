//! Recoverable items: mail, appointments, contacts, tasks and notes that were deleted for good (removed
//! from the mailbox, not moved to Deleted Items). They stay here for a number of days so Folder › Recover
//! Deleted Items can put them back, independent of the undo history. The store keeps the bin in its own file
//! (`mailcraft-store`); the engine fills it after each command and purges it on a schedule.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{AccountId, Appointment, Contact, FolderId, ItemId, MailItem, Mailbox, Note, Task};

/// Most items the bin holds (the oldest go first).
pub const MAX_RECOVERABLE: usize = 5_000;

/// Longest folder path kept with an entry.
const MAX_PATH_CHARS: usize = 1_024;

/// A deleted item of any kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "item", rename_all = "camelCase")]
pub enum DeletedItem {
    Mail(Box<MailItem>),
    Appointment(Box<Appointment>),
    Contact(Box<Contact>),
    Task(Box<Task>),
    Note(Box<Note>),
}

impl DeletedItem {
    pub fn id(&self) -> ItemId {
        match self {
            DeletedItem::Mail(m) => m.id,
            DeletedItem::Appointment(a) => a.id,
            DeletedItem::Contact(c) => c.id,
            DeletedItem::Task(t) => t.id,
            DeletedItem::Note(n) => n.id,
        }
    }

    pub fn folder(&self) -> FolderId {
        match self {
            DeletedItem::Mail(m) => m.folder,
            DeletedItem::Appointment(a) => a.folder,
            DeletedItem::Contact(c) => c.folder,
            DeletedItem::Task(t) => t.folder,
            DeletedItem::Note(n) => n.folder,
        }
    }

    /// "mail", "appointment", "contact", "task" or "note".
    pub fn kind(&self) -> &'static str {
        match self {
            DeletedItem::Mail(_) => "mail",
            DeletedItem::Appointment(_) => "appointment",
            DeletedItem::Contact(_) => "contact",
            DeletedItem::Task(_) => "task",
            DeletedItem::Note(_) => "note",
        }
    }

    /// (subject, from, date) for the Recover Deleted Items list.
    pub fn summary(&self) -> (String, String, i64) {
        match self {
            DeletedItem::Mail(m) => (
                m.subject.clone(),
                m.from.as_ref().map(|f| f.addr.clone()).unwrap_or_default(),
                m.received.unix,
            ),
            DeletedItem::Appointment(a) => (a.subject.clone(), String::new(), a.start.unix),
            DeletedItem::Contact(c) => (c.full_name(), String::new(), 0),
            DeletedItem::Task(t) => (t.subject.clone(), String::new(), t.due.map_or(0, |d| d.unix)),
            DeletedItem::Note(n) => {
                (n.text.lines().next().unwrap_or("").to_owned(), String::new(), n.modified.unix)
            }
        }
    }

    /// Whether `mb` holds an item of this kind with this id.
    pub fn present_in(&self, mb: &Mailbox) -> bool {
        present(mb, self.kind(), self.id())
    }

    /// Puts the item back into `mb` with `id` in `folder`. A mail item comes back as a new local message
    /// (the server copy is gone; a synchronised folder uploads it).
    pub fn restore(self, mb: &mut Mailbox, id: ItemId, folder: FolderId) {
        match self {
            DeletedItem::Mail(mut m) => {
                m.id = id;
                m.folder = folder;
                m.remote_id = None;
                mb.mail.insert(id, *m);
            }
            DeletedItem::Appointment(mut a) => {
                a.id = id;
                a.folder = folder;
                mb.appointments.insert(id, *a);
            }
            DeletedItem::Contact(mut c) => {
                c.id = id;
                c.folder = folder;
                mb.contacts.insert(id, *c);
            }
            DeletedItem::Task(mut t) => {
                t.id = id;
                t.folder = folder;
                mb.tasks.insert(id, *t);
            }
            DeletedItem::Note(mut n) => {
                n.id = id;
                n.folder = folder;
                mb.notes.insert(id, *n);
            }
        }
    }
}

fn present(mb: &Mailbox, kind: &str, id: ItemId) -> bool {
    match kind {
        "mail" => mb.mail.contains(id),
        "appointment" => mb.appointments.contains_key(&id),
        "contact" => mb.contacts.contains_key(&id),
        "task" => mb.tasks.contains_key(&id),
        "note" => mb.notes.contains_key(&id),
        _ => false,
    }
}

/// One entry of the bin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recoverable {
    pub item: DeletedItem,
    /// When it was deleted for good (Unix seconds).
    pub deleted: i64,
    /// Folder path it was deleted from ("Inbox\\Projects"), kept for the list after the folder is gone.
    #[serde(default)]
    pub folder_path: String,
    /// Recovered as this item id (the entry is kept so undoing the recovery leaves it recoverable).
    #[serde(default)]
    pub restored_as: Option<ItemId>,
    /// The account (data file) the item was deleted from, kept after its folder is gone (`None` for
    /// entries written before it was recorded, or when it was not known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AccountId>,
}

impl Recoverable {
    /// Listed by Recover Deleted Items: not recovered (or the recovered copy is gone again), and not back in
    /// the mailbox through undo.
    pub fn listed(&self, mb: &Mailbox) -> bool {
        match self.restored_as {
            Some(id) => !present(mb, self.item.kind(), id),
            None => !self.item.present_in(mb),
        }
    }
}

/// Permanently deleted items kept for recovery, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RecoverBin {
    pub items: Vec<Recoverable>,
}

impl RecoverBin {
    /// Adds a deleted item. An earlier entry for the same item (deleted, recovered, deleted again) is
    /// replaced; past [`MAX_RECOVERABLE`] the oldest entries go.
    pub fn add(&mut self, item: DeletedItem, folder_path: &str, now: i64) {
        self.add_all(vec![(item, folder_path.to_owned())], now);
    }

    /// Adds deleted items (with the folder path each was deleted from) in one pass: earlier entries for the
    /// same items are replaced, and past [`MAX_RECOVERABLE`] the oldest entries go (of `items` only the
    /// last [`MAX_RECOVERABLE`] are kept).
    pub fn add_all(&mut self, items: Vec<(DeletedItem, String)>, now: i64) {
        self.add_all_from(items.into_iter().map(|(item, path)| (item, path, None)).collect(), now);
    }

    /// [`RecoverBin::add_all`] with the account each item was deleted from.
    pub fn add_all_from(&mut self, items: Vec<(DeletedItem, String, Option<AccountId>)>, now: i64) {
        if items.is_empty() {
            return;
        }
        let skip = items.len().saturating_sub(MAX_RECOVERABLE);
        let items: Vec<(DeletedItem, String, Option<AccountId>)> = items.into_iter().skip(skip).collect();
        let new: HashSet<(&'static str, ItemId)> = items.iter().map(|(i, _, _)| (i.kind(), i.id())).collect();
        self.items.retain(|e| {
            let kind = e.item.kind();
            !new.contains(&(kind, e.item.id())) && e.restored_as.is_none_or(|r| !new.contains(&(kind, r)))
        });
        for (item, folder_path, account) in items {
            let folder_path: String = folder_path.chars().take(MAX_PATH_CHARS).collect();
            self.items.push(Recoverable { item, deleted: now, folder_path, restored_as: None, account });
        }
        if self.items.len() > MAX_RECOVERABLE {
            let excess = self.items.len().saturating_sub(MAX_RECOVERABLE);
            self.items.drain(..excess);
        }
    }

    /// Drops entries deleted more than `days` days before `now` (0 days keeps nothing). Returns how many
    /// went.
    pub fn purge(&mut self, now: i64, days: u32) -> usize {
        let keep_secs = i64::from(days).saturating_mul(86_400);
        let before = self.items.len();
        self.items.retain(|e| days > 0 && now.saturating_sub(e.deleted) < keep_secs);
        before.saturating_sub(self.items.len())
    }

    /// Entries the dialog lists, newest first.
    pub fn listed<'a>(&'a self, mb: &'a Mailbox) -> impl Iterator<Item = &'a Recoverable> + 'a {
        self.items.iter().rev().filter(move |e| e.listed(mb))
    }

    /// Removes listed entries of these item ids for good (Purge Selected Items). Returns how many went.
    pub fn remove(&mut self, ids: &[ItemId], mb: &Mailbox) -> usize {
        let before = self.items.len();
        self.items.retain(|e| !(e.listed(mb) && ids.contains(&e.item.id())));
        before.saturating_sub(self.items.len())
    }

    /// Items that were in `before` and are gone from `after` (deleted for good by whatever ran in between).
    pub fn vanished(before: &Mailbox, after: &Mailbox) -> Vec<DeletedItem> {
        RecoverBin::vanished_where(before, after, |_| true, usize::MAX)
    }

    /// Like [`RecoverBin::vanished`], limited to items whose folder passes `keep` and to at most `limit`
    /// items (only those are copied).
    pub fn vanished_where(
        before: &Mailbox,
        after: &Mailbox,
        keep: impl Fn(FolderId) -> bool,
        limit: usize,
    ) -> Vec<DeletedItem> {
        let mut out: Vec<DeletedItem> = before
            .mail
            .items()
            .filter(|m| !after.mail.contains(m.id) && keep(m.folder))
            .take(limit)
            .map(|m| DeletedItem::Mail(Box::new(m.clone())))
            .collect();
        let room = |out: &Vec<DeletedItem>| limit.saturating_sub(out.len());
        let n = room(&out);
        out.extend(
            before
                .appointments
                .values()
                .filter(|a| !after.appointments.contains_key(&a.id) && keep(a.folder))
                .take(n)
                .map(|a| DeletedItem::Appointment(Box::new(a.clone()))),
        );
        let n = room(&out);
        out.extend(
            before
                .contacts
                .values()
                .filter(|c| !after.contacts.contains_key(&c.id) && keep(c.folder))
                .take(n)
                .map(|c| DeletedItem::Contact(Box::new(c.clone()))),
        );
        let n = room(&out);
        out.extend(
            before
                .tasks
                .values()
                .filter(|t| !after.tasks.contains_key(&t.id) && keep(t.folder))
                .take(n)
                .map(|t| DeletedItem::Task(Box::new(t.clone()))),
        );
        let n = room(&out);
        out.extend(
            before
                .notes
                .values()
                .filter(|x| !after.notes.contains_key(&x.id) && keep(x.folder))
                .take(n)
                .map(|x| DeletedItem::Note(Box::new(x.clone()))),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u64, text: &str) -> DeletedItem {
        DeletedItem::Note(Box::new(Note { id: ItemId(id), text: text.into(), ..Default::default() }))
    }

    #[test]
    fn add_replaces_and_caps() {
        let mut bin = RecoverBin::default();
        bin.add(note(1, "a"), "Notes", 10);
        bin.add(note(1, "a again"), "Notes", 20);
        assert_eq!(bin.items.len(), 1);
        assert_eq!(bin.items[0].deleted, 20);
        for i in 0..(MAX_RECOVERABLE as u64 + 10) {
            bin.add(note(100 + i, "x"), "", 30);
        }
        assert_eq!(bin.items.len(), MAX_RECOVERABLE);
    }

    #[test]
    fn add_all_replaces_in_one_pass_and_keeps_the_newest() {
        let mut bin = RecoverBin::default();
        bin.add(note(1, "a"), "Notes", 10);
        bin.items[0].restored_as = Some(ItemId(2));
        bin.add(note(3, "c"), "Notes", 10);
        // 1 again and its recovered copy 2: both replace the first entry.
        bin.add_all(vec![(note(1, "a2"), "N".into()), (note(2, "b"), "N".into())], 20);
        let ids: Vec<u64> = bin.items.iter().map(|e| e.item.id().0).collect();
        assert_eq!(ids, vec![3, 1, 2]);
        // More than the cap at once: the last ones stay.
        let many: Vec<(DeletedItem, String)> =
            (0..(MAX_RECOVERABLE as u64 + 5)).map(|i| (note(1_000 + i, "x"), String::new())).collect();
        bin.add_all(many, 30);
        assert_eq!(bin.items.len(), MAX_RECOVERABLE);
        assert_eq!(bin.items.first().map(|e| e.item.id().0), Some(1_005));
        bin.add_all(Vec::new(), 40);
        assert_eq!(bin.items.len(), MAX_RECOVERABLE);
    }

    #[test]
    fn entries_keep_their_account_and_older_entries_still_read() {
        let mut bin = RecoverBin::default();
        bin.add_all_from(vec![(note(1, "a"), "Notes".into(), Some(AccountId(7)))], 10);
        bin.add(note(2, "b"), "Notes", 10);
        assert_eq!(bin.items[0].account, Some(AccountId(7)));
        assert_eq!(bin.items[1].account, None);
        let text = serde_json::to_string(&bin).unwrap();
        let back: RecoverBin = serde_json::from_str(&text).unwrap();
        assert_eq!(back, bin);
        // An entry written before the account was recorded.
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["items"][1].get("account").is_none(), "not written when unknown");
        v["items"][0].as_object_mut().unwrap().remove("account");
        let old: RecoverBin = serde_json::from_value(v).unwrap();
        assert_eq!(old.items[0].account, None);
    }

    #[test]
    fn vanished_where_filters_and_limits() {
        let mut before = Mailbox::default();
        for i in 1..=4u64 {
            let folder = FolderId(if i % 2 == 0 { 2 } else { 1 });
            before.notes.insert(ItemId(i), Note { id: ItemId(i), folder, ..Default::default() });
        }
        let after = Mailbox::default();
        assert_eq!(RecoverBin::vanished(&before, &after).len(), 4);
        let odd = RecoverBin::vanished_where(&before, &after, |f| f == FolderId(1), usize::MAX);
        assert!(odd.iter().all(|i| i.id().0 % 2 == 1));
        assert_eq!(odd.len(), 2);
        assert_eq!(RecoverBin::vanished_where(&before, &after, |_| true, 3).len(), 3);
    }

    #[test]
    fn purge_by_age() {
        let mut bin = RecoverBin::default();
        bin.add(note(1, "old"), "", 0);
        bin.add(note(2, "new"), "", 10 * 86_400);
        assert_eq!(bin.purge(14 * 86_400 + 1, 14), 1);
        assert_eq!(bin.items.len(), 1);
        assert_eq!(bin.purge(14 * 86_400 + 1, 0), 1);
        assert!(bin.items.is_empty());
    }

    #[test]
    fn listed_hides_items_back_in_the_mailbox() {
        let mut mb = Mailbox::default();
        let mut bin = RecoverBin::default();
        bin.add(note(5, "n"), "", 0);
        assert_eq!(bin.listed(&mb).count(), 1);
        mb.notes.insert(ItemId(5), Note { id: ItemId(5), ..Default::default() });
        assert_eq!(bin.listed(&mb).count(), 0);
        mb.notes.clear();
        bin.items[0].restored_as = Some(ItemId(9));
        assert_eq!(bin.listed(&mb).count(), 1, "the recovered copy is gone again");
        mb.notes.insert(ItemId(9), Note { id: ItemId(9), ..Default::default() });
        assert_eq!(bin.listed(&mb).count(), 0);
        assert_eq!(bin.remove(&[ItemId(5)], &mb), 0, "only listed entries are purged");
    }

    #[test]
    fn serde_round_trip() {
        let mut bin = RecoverBin::default();
        bin.add(note(3, "hello"), "Notes", 7);
        let json = serde_json::to_string(&bin).unwrap();
        let back: RecoverBin = serde_json::from_str(&json).unwrap();
        assert_eq!(back, bin);
    }
}
