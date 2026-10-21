//! The session's search index (`mailcraft-search`) kept in step with the mailbox, plus the search state
//! that is not view state: recent searches and the Include Deleted Items toggle.
//!
//! Keeping in step: [`SearchState::note_ops`] is called with every batch of store ops the session journals
//! (commands, undo/redo, send/receive batches), marking changed items dirty; a whole-mailbox `Replace`
//! marks everything for a cheap reconcile (a per-item fingerprint decides what to re-index). The work is
//! done lazily by the next query ([`SearchState::ensure`]), so a burst of changes costs one update. Changes
//! made to the mailbox directly (tests, importers that bypass the journal) are caught when the item counts
//! change, or by [`SearchState::invalidate`].
//!
//! Queries go through [`hits`] (ranked), [`count`] (search folder counts) and [`item_matches`] (one item,
//! no index). The scope of the active search comes from [`scope_filter`]; search folders use
//! [`search_folder_filter`].

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, MutexGuard};

use mailcraft_model::{
    Appointment, Contact, FlagState, FolderId, FolderKind, FolderRole, ItemId, MailItem, Mailbox, Note, Task,
};
/// Fields of a mail item that search highlights apply to (for [`highlights`]).
pub use mailcraft_search::Field;
use mailcraft_search::{Context, Counts, Doc, DocKind, Filter, Hit, Index, Query, Stats};
use mailcraft_store::Op;

use crate::{SearchScope, Session};

/// Recent searches kept (Search › Recent Searches).
pub const MAX_RECENT: usize = 10;
/// Longest query remembered in Recent Searches (chars).
const MAX_RECENT_CHARS: usize = 512;
/// Query results cached per index generation.
const CACHE_SLOTS: usize = 8;

/// Item counts per kind (plus the id allocator): when they change behind the journal's back the index
/// reconciles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Shape([usize; 5], u64);

fn shape(mb: &Mailbox) -> Shape {
    Shape(
        [mb.mail.len(), mb.appointments.len(), mb.contacts.len(), mb.tasks.len(), mb.notes.len()],
        mb.next_id,
    )
}

#[derive(Default)]
struct Inner {
    index: Index,
    built: bool,
    /// Reconcile every item at the next query.
    full: bool,
    dirty: BTreeSet<u64>,
    stamps: HashMap<u64, u64>,
    shape: Shape,
    /// Bumped whenever the index (or anything a query reads) changes; keys the result cache.
    generation: u64,
    cache: Vec<(u64, Vec<Hit>)>,
    /// Count results (search folder counts are drawn every frame).
    counts: Vec<(u64, Counts)>,
}

/// Search state owned by the session.
#[derive(Default)]
pub struct SearchState {
    inner: Mutex<Inner>,
    /// Recent searches, most recent first (at most [`MAX_RECENT`]).
    pub recent: Vec<String>,
    /// Search › Include Deleted Items: mailbox-wide scopes also search Deleted Items.
    pub include_deleted: bool,
}

impl std::fmt::Debug for SearchState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchState").field("recent", &self.recent).finish()
    }
}

// ---------------------------------------------------------------------------------------------------------
// Fingerprints

fn h<T: Hash + ?Sized>(st: &mut DefaultHasher, v: &T) {
    v.hash(st);
}

fn stamp_mail(m: &MailItem) -> u64 {
    let mut st = DefaultHasher::new();
    h(&mut st, &(0u8, m.folder.0, m.read, m.size, m.received.unix, m.sent.map(|d| d.unix)));
    h(&mut st, &(m.importance as u8, m.is_flagged(), m.has_attachments(), m.subject.as_str(), &m.categories));
    for a in m.from.iter().chain(&m.sender).chain(&m.to).chain(&m.cc) {
        h(&mut st, &(a.name.as_deref(), a.addr.as_str()));
    }
    for a in &m.attachments {
        h(&mut st, a.filename.as_str());
    }
    // The whole body: an edit past the preview that keeps the length must still re-index the item.
    h(&mut st, &(body_hash(m.body.text.as_deref()), body_hash(m.body.html.as_deref())));
    // Form-field properties (`Doc::props`): sensitivity and the flag's state and dates.
    let flag = match &m.flag {
        FlagState::None => (0u8, None, None),
        FlagState::Flagged { due, start, .. } => (1, due.map(|d| d.unix), start.map(|d| d.unix)),
        FlagState::Complete { .. } => (2, None, None),
    };
    h(&mut st, &(m.sensitivity as u8, flag));
    st.finish()
}

/// Mixes more properties into a stamp.
fn mix(stamp: u64, v: &impl Hash) -> u64 {
    let mut st = DefaultHasher::new();
    h(&mut st, &stamp);
    h(&mut st, v);
    st.finish()
}

/// Fast fingerprint of a body (word-at-a-time multiply-rotate; several GB/s, so reconciling a large mailbox
/// stays cheap). Not collision resistant, which a change detector does not need.
fn body_hash(body: Option<&str>) -> u64 {
    const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;
    let Some(b) = body else { return 0 };
    let bytes = b.as_bytes();
    let mut x = (bytes.len() as u64).wrapping_mul(K) ^ 1;
    let (chunks, rest) = bytes.as_chunks::<8>();
    for c in chunks {
        x = (x.rotate_left(5) ^ u64::from_le_bytes(*c)).wrapping_mul(K);
    }
    for b in rest {
        x = (x.rotate_left(5) ^ u64::from(*b)).wrapping_mul(K);
    }
    x
}

fn stamp_text(kind: u8, folder: FolderId, parts: &[&str], cats: &[String], when: i64) -> u64 {
    let mut st = DefaultHasher::new();
    h(&mut st, &(kind, folder.0, when, cats));
    for p in parts {
        h(&mut st, p);
    }
    st.finish()
}

fn stamp_appointment(a: &Appointment) -> u64 {
    // Names and addresses: both are indexed.
    let people: Vec<&str> = a
        .organizer
        .iter()
        .chain(a.attendees.iter().map(|x| &x.mailbox))
        .flat_map(|m| [m.name.as_deref().unwrap_or(""), m.addr.as_str()])
        .collect();
    let mut parts = vec![a.subject.as_str(), a.location.as_str(), a.body.as_str()];
    parts.extend(people);
    let s = stamp_text(1, a.folder, &parts, &a.categories, a.start.unix);
    mix(s, &(a.end.unix, a.all_day, a.recurrence.is_some(), a.private))
}

fn stamp_contact(c: &Contact) -> u64 {
    let mut parts: Vec<&str> = [&c.title, &c.first_name, &c.middle_name, &c.last_name, &c.suffix, &c.company]
        .into_iter()
        .chain([&c.job_title, &c.department, &c.file_as, &c.web_page, &c.im_address, &c.notes])
        .map(String::as_str)
        .collect();
    parts.extend(c.emails.iter().map(String::as_str));
    parts.extend(c.phones.iter().flat_map(|(k, n)| [k.as_str(), n.as_str()]));
    let s = stamp_text(2, c.folder, &parts, &c.categories, 0);
    mix(s, &(c.office.as_str(), c.private, c.birthday.as_deref()))
}

fn stamp_task(t: &Task) -> u64 {
    let s = stamp_text(
        3,
        t.folder,
        &[&t.subject, &t.body],
        &t.categories,
        t.due.or(t.start).map_or(0, |d| d.unix),
    );
    let dates = (t.start.map(|d| d.unix), t.completed.map(|d| d.unix));
    mix(s, &(t.status as u8, t.priority as u8, t.percent_complete, t.private, dates, &t.assigned_to))
}

fn stamp_note(n: &Note) -> u64 {
    stamp_text(4, n.folder, &[&n.text], &n.categories, n.modified.unix)
}

/// Every searchable item of the mailbox as (id, fingerprint, doc builder).
fn for_each_item(mb: &Mailbox, mut f: impl FnMut(u64, u64, &dyn Fn() -> Doc)) {
    for m in mb.mail.values() {
        f(m.id.0, stamp_mail(m), &|| Doc::from_mail(m));
    }
    for a in mb.appointments.values() {
        f(a.id.0, stamp_appointment(a), &|| Doc::from_appointment(a));
    }
    for c in mb.contacts.values() {
        f(c.id.0, stamp_contact(c), &|| Doc::from_contact(c));
    }
    for t in mb.tasks.values() {
        f(t.id.0, stamp_task(t), &|| Doc::from_task(t));
    }
    for n in mb.notes.values() {
        f(n.id.0, stamp_note(n), &|| Doc::from_note(n));
    }
}

/// Fingerprint and doc of one item, wherever it lives.
fn doc_of(mb: &Mailbox, id: u64) -> Option<(u64, Doc)> {
    let id = ItemId(id);
    if let Some(m) = mb.mail.get(&id) {
        Some((stamp_mail(m), Doc::from_mail(m)))
    } else if let Some(a) = mb.appointments.get(&id) {
        Some((stamp_appointment(a), Doc::from_appointment(a)))
    } else if let Some(c) = mb.contacts.get(&id) {
        Some((stamp_contact(c), Doc::from_contact(c)))
    } else if let Some(t) = mb.tasks.get(&id) {
        Some((stamp_task(t), Doc::from_task(t)))
    } else {
        mb.notes.get(&id).map(|n| (stamp_note(n), Doc::from_note(n)))
    }
}

// ---------------------------------------------------------------------------------------------------------

impl SearchState {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock only means an earlier query panicked mid-way (the session's panic guard caught
        // it); the index itself is still consistent enough to rebuild from.
        match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                let mut g = poisoned.into_inner();
                g.full = true;
                g
            }
        }
    }

    /// Records journaled changes (called by `Session::journal`).
    pub fn note_ops(&mut self, ops: &[Op]) {
        let inner = match self.inner.get_mut() {
            Ok(i) => i,
            Err(p) => p.into_inner(),
        };
        for op in ops {
            match op {
                Op::PutMail(m) => {
                    inner.dirty.insert(m.id.0);
                }
                Op::RemoveMail(id) => {
                    inner.dirty.insert(id.0);
                }
                // Folder names feed `folder:` and scopes: results may change without index changes.
                Op::PutFolder(_) | Op::RemoveFolder(_) => {}
                Op::Replace(_) => inner.full = true,
            }
        }
        inner.generation = inner.generation.wrapping_add(1);
        inner.cache.clear();
        inner.counts.clear();
    }

    /// Re-checks every item at the next query (after direct mailbox edits that bypass the journal).
    pub fn invalidate(&mut self) {
        let inner = match self.inner.get_mut() {
            Ok(i) => i,
            Err(p) => p.into_inner(),
        };
        inner.full = true;
        inner.generation = inner.generation.wrapping_add(1);
        inner.cache.clear();
        inner.counts.clear();
    }

    /// Brings the index up to date with `mb`.
    fn ensure_locked(inner: &mut Inner, mb: &Mailbox) {
        let now_shape = shape(mb);
        if !inner.built || inner.full || inner.shape != now_shape {
            let mut seen: HashSet<u64> = HashSet::with_capacity(inner.stamps.len());
            let mut changed = false;
            let Inner { index, stamps, .. } = inner;
            for_each_item(mb, |id, stamp, make| {
                seen.insert(id);
                if stamps.get(&id) != Some(&stamp) {
                    index.upsert(&make());
                    stamps.insert(id, stamp);
                    changed = true;
                }
            });
            let gone: Vec<u64> = stamps.keys().filter(|id| !seen.contains(id)).copied().collect();
            for id in gone {
                stamps.remove(&id);
                index.remove(id);
                changed = true;
            }
            inner.built = true;
            inner.full = false;
            inner.dirty.clear();
            inner.shape = now_shape;
            if changed {
                inner.generation = inner.generation.wrapping_add(1);
                inner.cache.clear();
                inner.counts.clear();
            }
            return;
        }
        if inner.dirty.is_empty() {
            return;
        }
        // Items the journal names changed: re-indexed unconditionally (the fingerprint is only a shortcut for
        // the full reconcile).
        for id in std::mem::take(&mut inner.dirty) {
            match doc_of(mb, id) {
                Some((stamp, doc)) => {
                    inner.index.upsert(&doc);
                    inner.stamps.insert(id, stamp);
                }
                None => {
                    inner.stamps.remove(&id);
                    inner.index.remove(id);
                }
            }
        }
        inner.generation = inner.generation.wrapping_add(1);
        inner.cache.clear();
        inner.counts.clear();
    }

    /// Brings the index up to date with `mb` now (queries do this themselves).
    pub fn ensure(&self, mb: &Mailbox) {
        let mut inner = self.lock();
        Self::ensure_locked(&mut inner, mb);
    }

    /// Items waiting to be indexed (Indexing Status).
    pub fn pending(&self, mb: &Mailbox) -> usize {
        let inner = self.lock();
        let total: usize = shape(mb).0.iter().sum();
        if !inner.built {
            total
        } else if inner.full || inner.shape != shape(mb) {
            total.abs_diff(inner.index.len()).max(1)
        } else {
            inner.dirty.len()
        }
    }

    /// Index size.
    pub fn stats(&self, mb: &Mailbox) -> Stats {
        let mut inner = self.lock();
        Self::ensure_locked(&mut inner, mb);
        inner.index.stats()
    }

    /// Adds `query` to Recent Searches (most recent first, no duplicates, at most [`MAX_RECENT`]).
    pub fn remember(&mut self, query: &str) {
        let q: String = query.trim().chars().take(MAX_RECENT_CHARS).collect();
        if q.is_empty() {
            return;
        }
        self.recent.retain(|r| r != &q);
        self.recent.insert(0, q);
        self.recent.truncate(MAX_RECENT);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Queries

/// The evaluation context of the session: clock, time zone, the user's addresses, folder names.
pub fn context(s: &Session) -> Context {
    Context {
        now: s.now().unix,
        tz_offset_min: s.tz_offset_min,
        me: s.me.clone(),
        folders: s.mailbox.folders.values().map(|f| (f.id.0, f.name.clone())).collect(),
    }
}

fn deleted_folders(mb: &Mailbox) -> BTreeSet<FolderId> {
    mb.accounts
        .iter()
        .filter_map(|a| mb.role_folder(a.id, FolderRole::DeletedItems))
        .flat_map(|d| mb.subtree(d))
        .collect()
}

/// Setting listing the accounts and data files Search › Search Tools › Locations to Search leaves out of
/// All Mailboxes and All Items (store keys, see [`store_key`]).
pub const EXCLUDED_STORES_SETTING: &str = "search.excludedStores";

/// The key Locations to Search knows an account or data file by: its address, else its name (lower case).
pub fn store_key(a: &mailcraft_model::Account) -> String {
    let name = if a.address.trim().is_empty() { a.display_name.trim() } else { a.address.trim() };
    name.to_lowercase()
}

/// Accounts and data files that All Mailboxes and All Items search (Locations to Search: every store,
/// less the ones the user unchecked).
pub fn searched_stores(s: &Session) -> BTreeSet<mailcraft_model::AccountId> {
    let excluded: BTreeSet<String> =
        s.settings.list(EXCLUDED_STORES_SETTING).iter().map(|k| k.trim().to_lowercase()).collect();
    s.mailbox.accounts.iter().filter(|a| !excluded.contains(&store_key(a))).map(|a| a.id).collect()
}

/// Search Tools › Indexing Status: what is searchable and what is still to come. `remaining`: items not
/// indexed yet (the next search indexes them), `kinds`: the items of each kind (mail, calendar, contacts,
/// tasks, notes), `downloading`: while a send/receive is still bringing mail in, its progress (`done`,
/// `total`, `text`); new mail is indexed as it arrives.
pub fn indexing_status(s: &Session) -> serde_json::Value {
    let mb = &s.mailbox;
    let kinds = [
        ("mail", "Mail", mb.mail.len()),
        ("calendar", "Calendar", mb.appointments.len()),
        ("contacts", "Contacts", mb.contacts.len()),
        ("tasks", "Tasks", mb.tasks.len()),
        ("notes", "Notes", mb.notes.len()),
    ];
    let st = s.sync_status();
    let downloading = st.busy.then(|| {
        let d = st.detail.as_ref();
        serde_json::json!({
            "text": st.text,
            "done": d.map_or(0, |d| d.done),
            "total": d.map_or(0, |d| d.total),
        })
    });
    serde_json::json!({
        "remaining": s.search.pending(mb),
        "items": kinds.iter().map(|k| k.2).sum::<usize>(),
        "kinds": kinds.iter().map(|(id, label, n)| serde_json::json!({"kind": id, "label": label, "count": n})).collect::<Vec<_>>(),
        "downloading": downloading,
    })
}

/// Folders the active search covers. Mailbox-wide scopes leave out non-mail folders and (unless Include
/// Deleted Items is on) Deleted Items; All Items covers every kind of folder, again without Deleted Items
/// unless Include Deleted Items is on. All Mailboxes and All Items cover the
/// stores checked in Locations to Search ([`searched_stores`]).
pub fn scope_folders(s: &Session) -> Vec<FolderId> {
    let mb = &s.mailbox;
    let sel = s.selection.folder;
    if !mb.folders.contains_key(&sel) {
        return Vec::new();
    }
    let deleted = if s.search.include_deleted { BTreeSet::new() } else { deleted_folders(mb) };
    let mail_like = |f: &mailcraft_model::Folder| f.kind == FolderKind::Mail && !deleted.contains(&f.id);
    match s.view.search_scope {
        SearchScope::CurrentFolder => vec![sel],
        SearchScope::Subfolders => mb.subtree(sel),
        SearchScope::CurrentMailbox => {
            let account = mb.folder_account(sel).or_else(|| s.current_account());
            mb.folders.values().filter(|f| Some(f.account) == account && mail_like(f)).map(|f| f.id).collect()
        }
        SearchScope::AllMailboxes => {
            let stores = searched_stores(s);
            mb.folders
                .values()
                .filter(|f| stores.contains(&f.account) && mail_like(f))
                .map(|f| f.id)
                .collect()
        }
        SearchScope::AllItems => {
            // Every kind of folder, but Deleted Items (and its subfolders) only with Include Deleted Items.
            let stores = searched_stores(s);
            mb.folders
                .values()
                .filter(|f| stores.contains(&f.account) && !deleted.contains(&f.id))
                .map(|f| f.id)
                .collect()
        }
    }
}

/// Filter for the active search scope (All Items: every kind; otherwise mail only).
pub fn scope_filter(s: &Session) -> Filter {
    let folders = scope_folders(s).into_iter().map(|f| f.0);
    if s.view.search_scope == SearchScope::AllItems {
        Filter { folders: Some(folders.collect()), kinds: Vec::new() }
    } else {
        Filter::mail_in(folders)
    }
}

/// Folders a search folder covers: its account's mail folders except Deleted Items and Junk Email.
pub fn search_folder_filter(s: &Session, folder: FolderId) -> Filter {
    let mb = &s.mailbox;
    let Some(account) = mb.folder_account(folder) else {
        return Filter::mail_in([]);
    };
    let excluded: Vec<FolderId> = [FolderRole::DeletedItems, FolderRole::JunkEmail]
        .into_iter()
        .filter_map(|r| mb.role_folder(account, r))
        .collect();
    Filter::mail_in(
        mb.folders
            .values()
            .filter(|f| f.account == account && f.kind == FolderKind::Mail)
            .filter(|f| !excluded.iter().any(|x| mb.is_in_subtree(f.id, *x)))
            .map(|f| f.id.0),
    )
}

fn cache_key(query: &Query, ctx: &Context, filter: &Filter, generation: u64) -> u64 {
    let mut st = DefaultHasher::new();
    h(&mut st, &(generation, query.to_string(), ctx.day_of(ctx.now), ctx.tz_offset_min, &ctx.me));
    let mut folders: Vec<u64> = filter.folders.iter().flatten().copied().collect();
    folders.sort_unstable();
    h(
        &mut st,
        &(filter.folders.is_some(), folders, filter.kinds.iter().map(|k| *k as u8).collect::<Vec<_>>()),
    );
    // `folder:` reads folder names.
    if query.terms().iter().any(|(t, _)| matches!(t, mailcraft_search::Term::Folder(_))) {
        h(&mut st, &ctx.folders);
    }
    st.finish()
}

/// Items matching `query` within `filter`, best first.
pub fn hits(s: &Session, query: &str, filter: &Filter) -> Vec<Hit> {
    let q = mailcraft_search::parse(query);
    let ctx = context(s);
    let mut inner = s.search.lock();
    SearchState::ensure_locked(&mut inner, &s.mailbox);
    let key = cache_key(&q, &ctx, filter, inner.generation);
    if let Some((_, v)) = inner.cache.iter().find(|(k, _)| *k == key) {
        return v.clone();
    }
    let v = inner.index.search(&q, &ctx, filter);
    if inner.cache.len() >= CACHE_SLOTS {
        inner.cache.remove(0);
    }
    inner.cache.push((key, v.clone()));
    v
}

/// Total and unread counts of `query` within `filter`.
pub fn count(s: &Session, query: &str, filter: &Filter) -> Counts {
    let q = mailcraft_search::parse(query);
    let ctx = context(s);
    let mut inner = s.search.lock();
    SearchState::ensure_locked(&mut inner, &s.mailbox);
    let key = cache_key(&q, &ctx, filter, inner.generation);
    if let Some((_, c)) = inner.counts.iter().find(|(k, _)| *k == key) {
        return *c;
    }
    let c = inner.index.count(&q, &ctx, filter);
    if inner.counts.len() >= CACHE_SLOTS * 4 {
        inner.counts.remove(0);
    }
    inner.counts.push((key, c));
    c
}

/// Mail matching the active search (`s.view.search`) in its scope, best first; empty when no search.
pub fn results(s: &Session) -> Vec<ItemId> {
    if s.view.search.trim().is_empty() {
        return Vec::new();
    }
    let mut out: Vec<ItemId> =
        hits(s, &s.view.search, &scope_filter(s)).into_iter().map(|h| ItemId(h.id)).collect();
    // Search › Include Older Results: what the servers found (their text is not all here, so the local
    // index alone may not match them).
    let extra = crate::sync::older::extra_results(s);
    if !extra.is_empty() {
        let scope: BTreeSet<FolderId> = scope_folders(s).into_iter().collect();
        let have: BTreeSet<ItemId> = out.iter().copied().collect();
        out.extend(
            extra
                .into_iter()
                .filter(|i| !have.contains(i))
                .filter(|i| s.mailbox.mail.get(i).is_some_and(|m| scope.contains(&m.folder))),
        );
    }
    out
}

/// Items of a search folder (its saved query live over its account's mail), best first.
pub fn search_folder_items(s: &Session, folder: FolderId) -> Vec<ItemId> {
    let Some(f) = s.mailbox.folder(folder).filter(|f| f.kind == FolderKind::Search) else {
        return Vec::new();
    };
    let query = f.search_query.clone().unwrap_or_default();
    hits(s, &query, &search_folder_filter(s, folder)).into_iter().map(|h| ItemId(h.id)).collect()
}

/// Total and unread counts of a search folder (folder pane).
pub fn search_folder_counts(s: &Session, folder: FolderId) -> Counts {
    let Some(f) = s.mailbox.folder(folder).filter(|f| f.kind == FolderKind::Search) else {
        return Counts::default();
    };
    let query = f.search_query.clone().unwrap_or_default();
    count(s, &query, &search_folder_filter(s, folder))
}

/// True when mail item `m` matches `query` (direct evaluation, no index).
pub fn item_matches(s: &Session, m: &MailItem, query: &str) -> bool {
    mailcraft_search::matches(&mailcraft_search::parse(query), &Doc::from_mail(m), &context(s))
}

/// Highlight ranges (byte offsets) of the active search's words in `text` shown as `field`.
pub fn highlights(s: &Session, field: Field, text: &str) -> Vec<(usize, usize)> {
    if s.view.search.trim().is_empty() {
        return Vec::new();
    }
    mailcraft_search::highlights(&mailcraft_search::parse(&s.view.search), field, text)
}

/// Kinds the active scope searches.
pub fn scope_kinds(s: &Session) -> Vec<DocKind> {
    if s.view.search_scope == SearchScope::AllItems { DocKind::ALL.to_vec() } else { vec![DocKind::Mail] }
}

// ---------------------------------------------------------------------------------------------------------
// New Search Folder templates

/// New Search Folder templates: (id, dialog group, label, what the value means or "" when none).
pub const SEARCH_FOLDER_TEMPLATES: [(&str, &str, &str, &str); 14] = [
    ("unread", "Reading Mail", "Unread mail", ""),
    ("flagged", "Reading Mail", "Mail flagged for follow up", ""),
    ("unreadOrFlagged", "Reading Mail", "Mail either unread or flagged for follow up", ""),
    ("important", "Reading Mail", "Important mail", ""),
    ("fromAndTo", "Mail from People and Lists", "Mail from and to specific people", "people"),
    ("from", "Mail from People and Lists", "Mail from specific people", "people"),
    ("toMe", "Mail from People and Lists", "Mail sent directly to me", ""),
    ("toGroup", "Mail from People and Lists", "Mail sent to public groups", "group"),
    ("categorized", "Organizing Mail", "Categorized mail", "category?"),
    ("large", "Organizing Mail", "Large mail", "kb?"),
    ("old", "Organizing Mail", "Old mail", "days?"),
    ("attachments", "Organizing Mail", "Mail with attachments", ""),
    ("words", "Organizing Mail", "Mail with specific words", "words"),
    ("custom", "Custom", "Create a custom Search Folder", "query"),
];

/// Default size for Large mail (KB) and age for Old mail (days).
const LARGE_DEFAULT_KB: u64 = 100;
const OLD_DEFAULT_DAYS: u64 = 7;

fn quoted(v: &str) -> String {
    format!("\"{}\"", v.replace('"', "").trim())
}

/// `field:"a" OR field:"b"` for a `;`/`,`-separated list.
fn any_of(fields: &[&str], list: &str) -> Option<String> {
    let items: Vec<&str> = list.split([';', ',']).map(str::trim).filter(|v| !v.is_empty()).collect();
    if items.is_empty() {
        return None;
    }
    let parts: Vec<String> =
        items.iter().flat_map(|v| fields.iter().map(move |f| format!("{f}:{}", quoted(v)))).collect();
    Some(if parts.len() == 1 { parts.concat() } else { format!("({})", parts.join(" OR ")) })
}

/// (folder name, query) for a New Search Folder template; `value` is the template's option (people,
/// words, a category, a size in KB, an age in days, or the custom query). `Err` names what is missing.
pub fn search_folder_template(template: &str, value: &str) -> Result<(String, String), String> {
    let (_, _, label, needs) = SEARCH_FOLDER_TEMPLATES
        .iter()
        .find(|(id, ..)| *id == template)
        .ok_or_else(|| format!("unknown search folder template `{template}`"))?;
    let value = value.trim();
    let with = |v: &str| format!("{label} ({v})");
    let missing = || format!("`{template}` needs {needs}");
    let number = |default: u64| -> Result<u64, String> {
        if value.is_empty() {
            return Ok(default);
        }
        value
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0 && *n <= 10_000_000)
            .ok_or_else(|| format!("`{value}` is not a positive number"))
    };
    Ok(match template {
        "unread" => (label.to_string(), "isread:no".into()),
        "flagged" => (label.to_string(), "isflagged:yes".into()),
        "unreadOrFlagged" => (label.to_string(), "isread:no OR isflagged:yes".into()),
        "important" => (label.to_string(), "importance:high".into()),
        "fromAndTo" => (with(value), any_of(&["from", "tocc"], value).ok_or_else(missing)?),
        "from" => (with(value), any_of(&["from"], value).ok_or_else(missing)?),
        "toMe" => (label.to_string(), "to:me".into()),
        "toGroup" => (with(value), any_of(&["tocc"], value).ok_or_else(missing)?),
        "categorized" if value.is_empty() => (label.to_string(), "category:any".into()),
        "categorized" => (with(value), any_of(&["category"], value).ok_or_else(missing)?),
        "large" => {
            let kb = number(LARGE_DEFAULT_KB)?;
            (with(&format!("{kb} KB")), format!("size:>={kb}KB"))
        }
        "old" => {
            let days = number(OLD_DEFAULT_DAYS)?;
            (with(&format!("{days} days")), format!("received:<\"{days} days ago\""))
        }
        "attachments" => (label.to_string(), "hasattachments:yes".into()),
        "words" => (with(value), any_of(&["subject", "body"], value).ok_or_else(missing)?),
        "custom" if value.is_empty() => return Err(missing()),
        _ => (label.to_string(), value.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::{Address, DateTime};
    use serde_json::json;

    const NOW: i64 = 1_791_547_200;

    fn session() -> Session {
        let mut s = Session::new(Mailbox::new_local("Test"));
        s.fixed_now = Some(DateTime { unix: NOW, offset_min: 0 });
        s.me = vec!["me@example.com".into()];
        s
    }

    fn inbox(s: &Session) -> FolderId {
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap()
    }

    fn add(s: &mut Session, subject: &str) -> ItemId {
        let folder = inbox(s);
        s.mailbox.add_mail(MailItem {
            folder,
            subject: subject.into(),
            from: Some(Address { name: Some("Ann".into()), addr: "ann@example.com".into() }),
            received: DateTime { unix: NOW, offset_min: 0 },
            ..Default::default()
        })
    }

    #[test]
    fn follows_commands_and_direct_edits() {
        let mut s = session();
        let a = add(&mut s, "Budget review");
        add(&mut s, "Lunch");
        s.view.search = "budget".into();
        s.view.search_scope = SearchScope::CurrentFolder;
        assert_eq!(results(&s), [a]);
        // A command that changes the subject (journaled PutMail).
        s.selection.items = vec![a];
        s.selection.focus = Some(a);
        s.run("mail.markRead", &json!({"ids": [a.0]})).unwrap();
        assert!(s.mailbox.mail.get(&a).unwrap().read);
        assert_eq!(count(&s, "isread:yes", &scope_filter(&s)).total, 1);
        // Direct edit + invalidate.
        s.mailbox.mail.get_mut(&a).unwrap().subject = "Dinner".into();
        s.search.invalidate();
        assert!(results(&s).is_empty());
        // Direct add: picked up by the shape check.
        let c = add(&mut s, "Budget again");
        assert_eq!(results(&s), [c]);
        // Removal through a command.
        s.run("mail.delete", &json!({"ids": [c.0]})).unwrap();
        assert!(results(&s).is_empty());
        // All Items leaves Deleted Items out too until Include Deleted Items is on (regression: it used to
        // search Deleted Items whatever the toggle said).
        s.view.search_scope = SearchScope::AllItems;
        assert!(results(&s).is_empty());
        s.search.include_deleted = true;
        assert_eq!(results(&s), [c]);
        s.view.search_scope = SearchScope::AllMailboxes;
        assert_eq!(results(&s), [c]);
        s.search.include_deleted = false;
        assert!(results(&s).is_empty());
    }

    #[test]
    fn body_edits_past_the_preview_are_indexed() {
        let mut s = session();
        let a = add(&mut s, "Notes");
        let filler = "word ".repeat(80);
        let body = |tail: &str| format!("{filler}{tail}");
        s.mailbox.mail.get_mut(&a).unwrap().body.text = Some(body("alpha"));
        s.mailbox.mail.get_mut(&a).unwrap().refresh_preview();
        s.search.invalidate();
        s.view.search_scope = SearchScope::CurrentFolder;
        s.view.search = "alpha".into();
        assert_eq!(results(&s), [a]);
        // Same length, same preview: a journaled edit must still re-index the item.
        s.selection.items = vec![a];
        s.selection.focus = Some(a);
        s.run("mail.editMessage", &json!({"id": a.0, "text": body("gamma")})).unwrap();
        assert!(results(&s).is_empty());
        s.view.search = "gamma".into();
        assert_eq!(results(&s), [a]);
        // And a direct edit followed by `invalidate` (the full reconcile).
        s.mailbox.mail.get_mut(&a).unwrap().body.text = Some(body("delta"));
        s.search.invalidate();
        assert!(results(&s).is_empty());
        s.view.search = "delta".into();
        assert_eq!(results(&s), [a]);
    }

    #[test]
    fn attendee_name_changes_are_indexed() {
        let mut s = session();
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        let cal = s.mailbox.role_folder(acct, FolderRole::Calendar).unwrap();
        let id = ItemId(s.mailbox.alloc_id());
        let attendee = |name: &str| mailcraft_model::Attendee {
            mailbox: Address { name: Some(name.into()), addr: "x@example.com".into() },
            ..Default::default()
        };
        s.mailbox.appointments.insert(
            id,
            Appointment {
                id,
                folder: cal,
                subject: "Sync".into(),
                attendees: vec![attendee("Zelda")],
                ..Default::default()
            },
        );
        let all = Filter::default();
        assert_eq!(hits(&s, "to:zelda", &all).len(), 1);
        if let Some(a) = s.mailbox.appointments.get_mut(&id) {
            a.attendees = vec![attendee("Yorick")];
        }
        s.search.invalidate();
        assert!(hits(&s, "to:zelda", &all).is_empty());
        assert_eq!(hits(&s, "to:yorick", &all).len(), 1);
    }

    #[test]
    fn templates_build_queries() {
        let t = |id: &str, v: &str| search_folder_template(id, v).unwrap();
        assert_eq!(t("unread", ""), ("Unread mail".into(), "isread:no".into()));
        assert_eq!(t("unreadOrFlagged", "").1, "isread:no OR isflagged:yes");
        assert_eq!(t("from", "ann; bob@example.org").1, "(from:\"ann\" OR from:\"bob@example.org\")");
        assert_eq!(t("fromAndTo", "ann").1, "(from:\"ann\" OR tocc:\"ann\")");
        assert_eq!(t("large", "").1, "size:>=100KB");
        assert_eq!(t("large", "250").0, "Large mail (250 KB)");
        assert_eq!(t("old", "30").1, "received:<\"30 days ago\"");
        assert_eq!(t("categorized", "").1, "category:any");
        assert_eq!(t("words", "budget").1, "(subject:\"budget\" OR body:\"budget\")");
        assert_eq!(t("custom", "from:ann").1, "from:ann");
        assert!(search_folder_template("words", " ").is_err());
        assert!(search_folder_template("custom", "").is_err());
        assert!(search_folder_template("large", "-3").is_err());
        assert!(search_folder_template("nope", "").is_err());
        for (id, ..) in SEARCH_FOLDER_TEMPLATES {
            let v = if matches!(id, "large" | "old") { "5" } else { "x" };
            let (name, query) = search_folder_template(id, v).unwrap();
            assert!(!name.is_empty() && !query.is_empty(), "{id}");
            let parsed = mailcraft_search::parse(&query);
            assert!(
                parsed.terms().iter().all(|(t, _)| !matches!(t, mailcraft_search::Term::Invalid(..))),
                "{id}"
            );
        }
    }

    #[test]
    fn search_folders_evaluate_live() {
        let mut s = session();
        let a = add(&mut s, "Budget");
        add(&mut s, "Lunch");
        let r = s.run("folder.newSearchFolder", &json!({"template": "unread"})).unwrap();
        let sf = FolderId(r["folder"].as_u64().unwrap());
        assert_eq!(s.mailbox.folder(sf).unwrap().name, "Unread mail");
        assert_eq!(search_folder_counts(&s, sf), Counts { total: 2, unread: 2 });
        s.selection.items = vec![a];
        s.selection.focus = Some(a);
        s.run("mail.markRead", &json!({"ids": [a.0]})).unwrap();
        assert_eq!(search_folder_counts(&s, sf), Counts { total: 1, unread: 1 });
        assert_eq!(search_folder_items(&s, sf).len(), 1);
        let r = s.run("folder.newSearchFolder", &json!({"template": "words", "value": "budget"})).unwrap();
        let words = FolderId(r["folder"].as_u64().unwrap());
        assert_eq!(search_folder_items(&s, words), [a]);
        // Deleted Items is not searched.
        s.run("mail.delete", &json!({"ids": [a.0]})).unwrap();
        assert!(search_folder_items(&s, words).is_empty());
        assert!(s.run("folder.newSearchFolder", &json!({"template": "words"})).is_err());
        assert!(s.run("folder.newSearchFolder", &json!({"template": "bogus"})).is_err());
        // Not a search folder.
        assert_eq!(search_folder_counts(&s, inbox(&s)), Counts::default());
    }

    #[test]
    fn recent_searches_are_mru() {
        let mut st = SearchState::default();
        for i in 0..15 {
            st.remember(&format!("q{i}"));
        }
        st.remember("q3");
        st.remember("   ");
        assert_eq!(st.recent.len(), MAX_RECENT);
        assert_eq!(st.recent.first().map(String::as_str), Some("q3"));
        assert_eq!(st.recent.get(1).map(String::as_str), Some("q14"));
        assert_eq!(st.recent.iter().filter(|r| *r == "q3").count(), 1);
    }

    #[test]
    fn all_items_finds_other_kinds() {
        let mut s = session();
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        let tasks = s.mailbox.role_folder(acct, FolderRole::Tasks).unwrap();
        let id = ItemId(s.mailbox.alloc_id());
        s.mailbox.tasks.insert(
            id,
            mailcraft_model::Task {
                id,
                folder: tasks,
                subject: "File budget report".into(),
                ..Default::default()
            },
        );
        s.view.search = "budget".into();
        s.view.search_scope = SearchScope::AllMailboxes;
        assert!(results(&s).is_empty());
        s.view.search_scope = SearchScope::AllItems;
        assert_eq!(results(&s), [id]);
        assert_eq!(scope_kinds(&s).len(), 5);
    }

    #[test]
    fn highlights_follow_the_active_search() {
        let mut s = session();
        s.view.search = "bud -lunch".into();
        assert_eq!(highlights(&s, Field::Subject, "Budget lunch"), [(0, 6)]);
        s.view.search.clear();
        assert!(highlights(&s, Field::Subject, "Budget").is_empty());
    }
}
