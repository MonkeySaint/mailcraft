//! Contacts and tasks kept on a CardDAV (RFC 6352) / CalDAV (RFC 4791) server, and calendars published to a
//! WebDAV server: everything here except the network (the native worker side is `sync/dav.rs`).
//!
//! **Mapping.** An account's DAV settings ([`mailcraft_model::DavSettings`]) name the server; send/receive
//! discovers its address books and task lists. The account's Contacts folder takes the first address book and
//! its Tasks folder the first task list; further collections get folders of their own below them. The mapping
//! (collection URL and the ETags seen at the last synchronisation) lives in `AccountSync::dav`.
//!
//! **Items.** A synchronised contact or task carries a [`RemoteRef`]: resource URL, ETag, UID and the
//! fingerprint of the item as last synchronised. The fingerprint is a hash of the vCard / VTODO MailCraft
//! writes for the item, so an edit of anything the server keeps marks it changed; local-only details (a
//! contact's follow-up flag, a task's link to the mail it came from) do not.
//!
//! **Rules.** Changed here and unchanged there: uploaded with `If-Match`. Changed there: the server copy
//! replaces the local one unless the item was also changed here, in which case the local copy is uploaded
//! over it on the next run (the newer edit wins; nothing is lost silently mid-run). New here: uploaded with
//! `If-None-Match: *`. Gone here but known with the same ETag: deleted on the server. Gone there: removed
//! here unless changed here (then uploaded again). Every change applied here is journaled without an undo
//! step, and undo carries the synchronised state over ([`carry_over`]).

use std::collections::{BTreeMap, BTreeSet};

use mailcraft_formats::ics::{self, ImportOptions};
use mailcraft_formats::vcf;
use mailcraft_model::account::DavFolderState;
use mailcraft_model::task::TaskRecurrence;
use mailcraft_model::{
    AccountId, Contact, DateTime, Folder, FolderId, FolderKind, FolderRole, ItemId, Mailbox, RemoteRef, Task,
};

use super::Plan;
use crate::{Notice, Session};

/// Most items of one folder synchronised per run.
pub const MAX_ITEMS: usize = 20_000;

/// What a synchronised folder holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PimKind {
    Contacts,
    Tasks,
}

impl PimKind {
    pub fn of(kind: FolderKind) -> Option<PimKind> {
        match kind {
            FolderKind::Contacts => Some(PimKind::Contacts),
            FolderKind::Tasks => Some(PimKind::Tasks),
            _ => None,
        }
    }
    pub fn folder_kind(self) -> FolderKind {
        match self {
            PimKind::Contacts => FolderKind::Contacts,
            PimKind::Tasks => FolderKind::Tasks,
        }
    }
    pub fn role(self) -> FolderRole {
        match self {
            PimKind::Contacts => FolderRole::Contacts,
            PimKind::Tasks => FolderRole::Tasks,
        }
    }
    /// File name extension of a resource.
    pub fn extension(self) -> &'static str {
        match self {
            PimKind::Contacts => "vcf",
            PimKind::Tasks => "ics",
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            PimKind::Contacts => "text/vcard; charset=utf-8",
            PimKind::Tasks => "text/calendar; charset=utf-8",
        }
    }
    /// Progress wording ("Updating contacts…").
    pub fn label(self) -> &'static str {
        match self {
            PimKind::Contacts => "contacts",
            PimKind::Tasks => "tasks",
        }
    }
}

/// FNV-1a 64: a stable fingerprint (the same on every build and platform).
pub fn fnv(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A UID safe to write into a vCard or iCalendar line and a URL path: printable, no separators.
fn clean_uid(uid: &str) -> String {
    uid.chars().filter(|c| c.is_ascii_graphic() && !matches!(c, ';' | ':' | ',' | '\\')).take(200).collect()
}

/// The contact as the vCard 4.0 MailCraft uploads (with `uid`, when given).
pub fn contact_card(c: &Contact, uid: &str) -> String {
    let mut plain = c.clone();
    plain.remote = None;
    let mut out = String::new();
    vcf::write_one(&plain, vcf::Version::V4, &mut out);
    let uid = clean_uid(uid);
    if !uid.is_empty()
        && let Some(end) = out.rfind("END:VCARD")
    {
        out.insert_str(end, &format!("UID:{uid}\r\n"));
    }
    out
}

/// The task as the iCalendar object (one VTODO) MailCraft uploads.
pub fn task_calendar(t: &Task, uid: &str, now: DateTime) -> String {
    let mut plain = t.clone();
    plain.remote = None;
    plain.uid = clean_uid(uid);
    crate::cmd::tasks::task_ics(&plain, None, None, &[], now)
}

pub fn contact_fingerprint(c: &Contact) -> u64 {
    fnv(contact_card(c, "").as_bytes())
}

pub fn task_fingerprint(t: &Task) -> u64 {
    let mut plain = t.clone();
    plain.uid.clear();
    fnv(task_calendar(&plain, "", DateTime::from_unix(0, 0)).as_bytes())
}

/// Unfolded content lines (RFC 6350 / 5545 line folding).
fn unfold(text: &str) -> String {
    text.replace("\r\n ", "").replace("\r\n\t", "").replace("\n ", "").replace("\n\t", "")
}

/// The first card of `data` and its UID.
pub fn read_contact(data: &str) -> Option<(Contact, String)> {
    let c = vcf::read_str(data).into_iter().next()?;
    let unfolded = unfold(data);
    let uid = unfolded
        .lines()
        .map(str::trim_start)
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            let name = name.split(';').next()?.rsplit('.').next()?;
            name.eq_ignore_ascii_case("UID").then(|| value.trim().to_owned())
        })
        .unwrap_or_default();
    Some((c, uid))
}

/// The first task of `data` and its UID (with its RRULE as the task's recurrence).
pub fn read_task(data: &str, tz_offset_min: i32) -> Option<(Task, String)> {
    let cal = ics::read_str(data, ImportOptions { local_offset_min: tz_offset_min }).ok()?;
    let mut t = cal.tasks.into_iter().next()?;
    let comps = ics::parse::parse(data);
    let todo = comps.iter().flat_map(|c| c.children_named("VTODO")).next();
    let uid = todo.and_then(|c| c.text("UID")).map(|u| u.trim().to_owned()).unwrap_or_default();
    if let Some(rule) =
        todo.and_then(|c| c.prop("RRULE")).map(|p| p.value.trim().to_owned()).filter(|r| !r.is_empty())
    {
        t.recurrence = Some(TaskRecurrence { rrule: rule, start: t.due, ..Default::default() });
    }
    Some((t, uid))
}

/// A new UID for an item first uploaded from here.
fn new_uid(account: AccountId, id: ItemId, now: i64) -> String {
    let h = fnv(format!("{}:{}:{now}", account.0, id.0).as_bytes());
    format!("mailcraft-{h:016x}-{}", id.0)
}

/// The resource URL for a new item: `<collection><uid>.<ext>`.
pub fn resource_url(collection: &str, uid: &str, kind: PimKind) -> String {
    let base = if collection.ends_with('/') { collection.to_owned() } else { format!("{collection}/") };
    let name: String = uid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' })
        .collect();
    format!("{base}{name}.{}", kind.extension())
}

/// `scheme://host[:port]` of an http(s) address, lower case and without the default port; `None` for
/// anything else, and for an address carrying a user name (`https://a@b/` names host `b`, which a reader
/// easily takes for `a`).
pub fn web_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "https" => "443",
        "http" => "80",
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains(['@', '\\']) {
        return None;
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let (h, after) = v6.split_once(']')?;
            let port = if after.is_empty() { "" } else { after.strip_prefix(':')? };
            (format!("[{h}]"), port)
        }
        None => match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), p),
            None => (authority.to_owned(), ""),
        },
    };
    if host.is_empty() || host == "[]" || !port.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let host = host.to_ascii_lowercase();
    let port = port.trim_start_matches('0');
    if port.is_empty() || port == default {
        Some(format!("{scheme}://{host}"))
    } else {
        Some(format!("{scheme}://{host}:{port}"))
    }
}

/// Which contacts and tasks server (and login) an account's DAV links belong to: the server address in a
/// comparable form (origin, path without its trailing `/`) and the user name it signs in with. `None` when
/// the account has no such server. Links survive any settings change that keeps this.
pub(crate) fn dav_identity(a: &mailcraft_model::Account) -> Option<(String, String)> {
    let d = a.dav.as_ref()?;
    let url = d.url.trim();
    let origin = web_origin(url)?;
    let rest = url.split_once("://").map_or("", |(_, r)| r);
    let path = rest.find('/').and_then(|i| rest.get(i..)).unwrap_or("/");
    let path = path.split(['?', '#']).next().unwrap_or("/").trim_end_matches('/');
    let user = [d.username.trim(), a.incoming.as_ref().map(|i| i.username.trim()).unwrap_or_default()]
        .into_iter()
        .find(|u| !u.is_empty())
        .unwrap_or_else(|| a.address.trim())
        .to_owned();
    Some((format!("{origin}{path}"), user))
}

/// The UID of a server copy, and for a contact the fingerprint of the card (used to recognise a local item
/// already on the server when its link was lost).
pub fn server_identity(kind: PimKind, data: &str) -> (String, Option<u64>) {
    match kind {
        PimKind::Contacts => match read_contact(data) {
            Some((c, uid)) => (uid, Some(contact_fingerprint(&c))),
            None => (String::new(), None),
        },
        PimKind::Tasks => {
            let comps = ics::parse::parse(data);
            let uid = comps
                .iter()
                .flat_map(|c| c.children_named("VTODO"))
                .next()
                .and_then(|c| c.text("UID"))
                .map(|u| u.trim().to_owned())
                .unwrap_or_default();
            (uid, None)
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// What a run takes and brings back
// ---------------------------------------------------------------------------------------------------------

/// A local item as a run sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalItem {
    pub id: ItemId,
    /// Its server copy (`None`: new here).
    pub remote: Option<RemoteRef>,
    pub uid: String,
    /// What to upload when it is new or changed (empty otherwise).
    pub body: String,
    pub fingerprint: u64,
}

impl LocalItem {
    pub fn changed(&self) -> bool {
        self.remote.as_ref().is_none_or(|r| r.synced != self.fingerprint)
    }
}

/// One folder to synchronise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderJob {
    pub folder: FolderId,
    pub kind: PimKind,
    /// Mapped collection (`None`: the first one discovery finds).
    pub collection: Option<String>,
    pub known: BTreeMap<String, String>,
    pub items: Vec<LocalItem>,
    /// The folder holds more than [`MAX_ITEMS`] items: it is not synchronised (a partial view would make
    /// the items left out look deleted here).
    pub too_many: bool,
}

/// A calendar to upload to (or withdraw from) its publishing location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishJob {
    pub folder: FolderId,
    pub url: String,
    pub data: Vec<u8>,
    pub fingerprint: u64,
    /// Remove the published file instead.
    pub withdraw: bool,
}

/// Why a folder is not synchronised when it holds too many items.
pub fn too_many_error() -> String {
    format!("the folder holds more than {MAX_ITEMS} items, too many to synchronise")
}

/// A server copy to apply here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    /// The local item it updates, with its fingerprint when the run started.
    pub local: Option<(ItemId, u64)>,
    pub url: String,
    pub etag: String,
    pub data: String,
}

/// What a run did to one folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderOutcome {
    pub folder: FolderId,
    pub kind: Option<PimKind>,
    pub collection: String,
    /// Local items now on the server (their new server copy; `synced` = the fingerprint uploaded).
    pub uploaded: Vec<(ItemId, RemoteRef)>,
    pub incoming: Vec<Incoming>,
    /// Local items whose server copy is gone (with their fingerprint when the run started).
    pub removed: Vec<(ItemId, u64)>,
    /// Server copies this run deleted (items removed here).
    pub deleted: Vec<String>,
    /// Resource URL → ETag on the server after the run.
    pub known: BTreeMap<String, String>,
    /// Items that could not be synchronised (the rest went through).
    pub errors: Vec<String>,
}

/// What a DAV run reports to the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PimEvent {
    /// Collections found on the server: (URL, name) of address books and of task lists.
    Collections {
        contacts: Option<Vec<(String, String)>>,
        tasks: Option<Vec<(String, String)>>,
    },
    Folder(Box<FolderOutcome>),
    FolderFailed {
        folder: FolderId,
        error: String,
    },
    Published {
        folder: FolderId,
        /// The file written or removed.
        url: String,
        fingerprint: u64,
        withdrawn: bool,
        result: Result<(), String>,
    },
}

// ---------------------------------------------------------------------------------------------------------
// Session side: which folders, building jobs
// ---------------------------------------------------------------------------------------------------------

/// The folders of `account` that synchronise with its DAV server: the default Contacts / Tasks folders when
/// turned on in the settings, plus every folder mapped to a collection.
pub fn synced_folders(mb: &Mailbox, account: AccountId) -> Vec<(FolderId, PimKind)> {
    let Some(a) = mb.account(account) else { return Vec::new() };
    let Some(dav) = a.dav.as_ref().filter(|d| !d.url.trim().is_empty()) else { return Vec::new() };
    let mut out: Vec<(FolderId, PimKind)> = Vec::new();
    for (on, kind) in [(dav.contacts, PimKind::Contacts), (dav.tasks, PimKind::Tasks)] {
        if !on {
            continue;
        }
        if let Some(f) = mb.role_folder(account, kind.role()) {
            out.push((f, kind));
        }
        for fid in a.sync.dav.keys() {
            if let Some(f) = mb.folder(*fid).filter(|f| f.account == account && f.kind == kind.folder_kind())
                && !out.iter().any(|(x, _)| *x == f.id)
            {
                out.push((f.id, kind));
            }
        }
    }
    out
}

/// True when Update Folder on `folder` synchronises it with a contacts or tasks server.
pub fn is_synced_folder(mb: &Mailbox, folder: FolderId) -> bool {
    mb.folder(folder).is_some_and(|f| synced_folders(mb, f.account).iter().any(|(x, _)| *x == folder))
}

/// The run's view of `folder`.
pub fn folder_job(s: &Session, account: AccountId, folder: FolderId, kind: PimKind) -> FolderJob {
    let state = s.mailbox.account(account).and_then(|a| a.sync.dav.get(&folder)).cloned();
    let collection = state.as_ref().map(|st| st.collection.clone()).filter(|c| !c.is_empty());
    let known = state.map(|st| st.known).unwrap_or_default();
    let now = s.now();
    let mut job =
        FolderJob { folder, kind, collection: collection.clone(), known, items: Vec::new(), too_many: false };
    let count = match kind {
        PimKind::Contacts => s.mailbox.contacts.values().filter(|c| c.folder == folder).count(),
        PimKind::Tasks => s.mailbox.tasks.values().filter(|t| t.folder == folder).count(),
    };
    if count > MAX_ITEMS {
        job.too_many = true;
        return job;
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    // The remote copy only counts when it is in this folder's collection and no other item claimed it (a
    // copied or moved item is new here).
    let mut keep = |r: &Option<RemoteRef>| -> Option<RemoteRef> {
        let r = r.as_ref()?;
        let inside = collection.as_deref().is_some_and(|c| r.url.starts_with(c));
        (inside && seen.insert(r.url.clone())).then(|| r.clone())
    };
    match kind {
        PimKind::Contacts => {
            for c in s.mailbox.contacts.values().filter(|c| c.folder == folder) {
                let remote = keep(&c.remote);
                let uid = remote.as_ref().map(|r| r.uid.clone()).filter(|u| !u.is_empty());
                let uid = uid.unwrap_or_else(|| new_uid(account, c.id, now.unix));
                let fingerprint = contact_fingerprint(c);
                let mut item = LocalItem { id: c.id, remote, uid, body: String::new(), fingerprint };
                if item.changed() {
                    item.body = contact_card(c, &item.uid);
                }
                job.items.push(item);
            }
        }
        PimKind::Tasks => {
            let list: Vec<(&Task, Option<RemoteRef>)> = s
                .mailbox
                .tasks
                .values()
                .filter(|t| t.folder == folder)
                .map(|t| (t, keep(&t.remote)))
                .collect();
            // A UID names one item of a collection (RFC 4791 section 5.3.2.1): a task without a server copy of
            // its own whose UID another task already uses (a copy, a completed occurrence) gets a new one.
            let mut claimed: BTreeSet<String> = list
                .iter()
                .filter_map(|(_, r)| r.as_ref().map(|r| r.uid.clone()))
                .filter(|u| !u.is_empty())
                .collect();
            for (t, remote) in list {
                let uid = match remote.as_ref().map(|r| r.uid.clone()).filter(|u| !u.is_empty()) {
                    Some(u) => u,
                    None => {
                        let own = t.uid.trim();
                        if !own.is_empty() && claimed.insert(own.to_owned()) {
                            own.to_owned()
                        } else {
                            new_uid(account, t.id, now.unix)
                        }
                    }
                };
                let fingerprint = task_fingerprint(t);
                let mut item = LocalItem { id: t.id, remote, uid, body: String::new(), fingerprint };
                if item.changed() {
                    item.body = task_calendar(t, &item.uid, now);
                }
                job.items.push(item);
            }
        }
    }
    job
}

/// The folders a run of `plan` synchronises for `account`.
pub fn plan_folders(s: &Session, account: AccountId, plan: &Plan) -> Vec<FolderJob> {
    if !plan.receive {
        return Vec::new();
    }
    synced_folders(&s.mailbox, account)
        .into_iter()
        .filter(|(f, _)| plan.folders.as_ref().is_none_or(|fs| fs.contains(f)))
        .map(|(f, kind)| folder_job(s, account, f, kind))
        .collect()
}

/// True when `plan` updates mail folders of `account` (not only contacts, tasks or calendars).
pub fn plan_has_mail(s: &Session, account: AccountId, plan: &Plan) -> bool {
    plan.folders.as_ref().is_none_or(|fs| {
        fs.iter()
            .any(|f| s.mailbox.folder(*f).is_none_or(|f| f.account != account || f.kind == FolderKind::Mail))
    })
}

// ---------------------------------------------------------------------------------------------------------
// Session side: applying results
// ---------------------------------------------------------------------------------------------------------

/// Applies one event of `account`'s DAV run.
pub(super) fn apply(s: &mut Session, account: AccountId, ev: PimEvent) {
    match ev {
        PimEvent::Collections { contacts, tasks } => {
            let mut new_folders = Vec::new();
            if let Some(list) = contacts {
                new_folders.extend(map_collections(s, account, PimKind::Contacts, &list));
            }
            if let Some(list) = tasks {
                new_folders.extend(map_collections(s, account, PimKind::Tasks, &list));
            }
            if !new_folders.is_empty() {
                // Synchronise the new folders right after this run.
                let plan = Plan { send: false, receive: true, folders: Some(new_folders), user: false };
                let run = s.sync.runs.entry(account).or_default();
                match run.again.as_mut() {
                    Some(again) => again.merge(&plan),
                    None => run.again = Some(plan),
                }
            }
        }
        PimEvent::Folder(out) => apply_folder(s, account, *out),
        PimEvent::FolderFailed { folder, error } => {
            let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
            log::warn!("contacts/tasks sync of folder {name}: {error}");
            s.sync.runs.entry(account).or_default().error = Some(format!("{name}: {error}"));
        }
        PimEvent::Published { folder, url, fingerprint, withdrawn, result } => {
            published(s, account, folder, &url, fingerprint, withdrawn, result)
        }
    }
}

/// Maps discovered collections of `kind` to folders: the default folder takes the first, the others get
/// folders of their own. Folders whose collection is gone are unmapped (their items stay, as local items).
/// Returns the folders created.
fn map_collections(
    s: &mut Session,
    account: AccountId,
    kind: PimKind,
    list: &[(String, String)],
) -> Vec<FolderId> {
    let Some(default) = s.mailbox.role_folder(account, kind.role()) else { return Vec::new() };
    let on = s.mailbox.account(account).and_then(|a| a.dav.as_ref()).is_some_and(|d| match kind {
        PimKind::Contacts => d.contacts,
        PimKind::Tasks => d.tasks,
    });
    if !on || list.is_empty() {
        return Vec::new();
    }
    let mapped: BTreeMap<FolderId, String> = s
        .mailbox
        .account(account)
        .map(|a| {
            a.sync
                .dav
                .iter()
                .filter(|(f, _)| s.mailbox.folder(**f).is_some_and(|f| f.kind == kind.folder_kind()))
                .map(|(f, st)| (*f, st.collection.clone()))
                .collect()
        })
        .unwrap_or_default();
    // Gone from the server.
    let gone: Vec<FolderId> =
        mapped.iter().filter(|(_, c)| !list.iter().any(|(u, _)| u == *c)).map(|(f, _)| *f).collect();
    for f in &gone {
        unmap(s, account, *f);
    }
    let mut taken: BTreeSet<String> =
        mapped.iter().filter(|(f, _)| !gone.contains(f)).map(|(_, c)| c.clone()).collect();
    let mut created = Vec::new();
    for (i, (url, name)) in list.iter().enumerate().take(MAX_COLLECTIONS) {
        if taken.contains(url) {
            continue;
        }
        let default_free = !mapped.contains_key(&default) || gone.contains(&default);
        let folder = if i == 0 && default_free {
            default
        } else {
            let Some(f) = create_folder(s, account, default, kind, name) else { continue };
            created.push(f);
            f
        };
        if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == account) {
            a.sync.dav.insert(folder, DavFolderState { collection: url.clone(), known: BTreeMap::new() });
        }
        taken.insert(url.clone());
    }
    created
}

/// Most collections of one kind mapped to folders.
pub const MAX_COLLECTIONS: usize = 50;

fn create_folder(
    s: &mut Session,
    account: AccountId,
    parent: FolderId,
    kind: PimKind,
    name: &str,
) -> Option<FolderId> {
    let base: String = name.trim().chars().filter(|c| !c.is_control()).take(100).collect();
    let base = if base.is_empty() { kind.role().default_name().to_owned() } else { base };
    let taken = |mb: &Mailbox, n: &str| {
        mb.folders.values().any(|f| f.parent == Some(parent) && f.name.eq_ignore_ascii_case(n))
    };
    let mut name = base.clone();
    let mut k = 2u32;
    while taken(&s.mailbox, &name) {
        if k > 999 {
            return None;
        }
        name = format!("{base} ({k})");
        k = k.saturating_add(1);
    }
    let id = FolderId(s.mailbox.alloc_id());
    s.mailbox.folders.insert(
        id,
        Folder { id, account, parent: Some(parent), name, kind: kind.folder_kind(), ..Default::default() },
    );
    s.sync.track.folders_created.insert(id);
    Some(id)
}

/// Forgets the server collection of `folder`: its items stay as local items.
fn unmap(s: &mut Session, account: AccountId, folder: FolderId) {
    if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == account) {
        a.sync.dav.remove(&folder);
    }
    for c in s.mailbox.contacts.values_mut().filter(|c| c.folder == folder) {
        c.remote = None;
    }
    for t in s.mailbox.tasks.values_mut().filter(|t| t.folder == folder) {
        t.remote = None;
    }
}

/// A contact from the server, keeping what only lives here (the follow-up flag).
fn merged_contact(new: Contact, old: Option<&Contact>) -> Contact {
    match old {
        Some(o) => Contact { id: o.id, folder: o.folder, flag: o.flag.clone(), ..new },
        None => new,
    }
}

/// A task from the server, keeping what only lives here (its mail link, task request details, privacy).
fn merged_task(new: Task, old: Option<&Task>) -> Task {
    match old {
        Some(o) => Task {
            id: o.id,
            folder: o.folder,
            from_mail: o.from_mail,
            private: o.private,
            assigned_to: o.assigned_to.clone(),
            requested_by: o.requested_by.clone(),
            request_state: o.request_state,
            last_report: o.last_report,
            // The server keeps the rule only: while it is the same, the series start and the occurrences
            // completed so far stay as they are here.
            recurrence: match (new.recurrence.clone(), o.recurrence.as_ref()) {
                (Some(n), Some(old)) if n.rrule == old.rrule => Some(old.clone()),
                (Some(n), _) => Some(n),
                (None, _) => o.recurrence.clone().filter(|r| r.regenerate_days > 0),
            },
            ..new
        },
        None => new,
    }
}

fn apply_folder(s: &mut Session, account: AccountId, out: FolderOutcome) {
    let Some(folder) = s.mailbox.folder(out.folder).filter(|f| f.account == account).map(|f| f.id) else {
        return;
    };
    let Some(kind) = out.kind else { return };
    let tz = s.tz_offset_min;
    let now = s.now();
    for (id, r) in out.uploaded {
        match kind {
            PimKind::Contacts => {
                if let Some(c) = s.mailbox.contacts.get_mut(&id) {
                    c.remote = Some(r);
                }
            }
            PimKind::Tasks => {
                if let Some(t) = s.mailbox.tasks.get_mut(&id) {
                    // The UID it went up with (a new one when its own was another task's).
                    if !r.uid.is_empty() {
                        t.uid = r.uid.clone();
                    }
                    t.remote = Some(r);
                }
            }
        }
    }
    let mut problems = out.errors;
    let mut known = out.known;
    let claimed: BTreeSet<String> = match kind {
        PimKind::Contacts => s
            .mailbox
            .contacts
            .values()
            .filter(|c| c.folder == folder)
            .filter_map(|c| c.remote.as_ref().map(|r| r.url.clone()))
            .collect(),
        PimKind::Tasks => s
            .mailbox
            .tasks
            .values()
            .filter(|t| t.folder == folder)
            .filter_map(|t| t.remote.as_ref().map(|r| r.url.clone()))
            .collect(),
    };
    for inc in out.incoming {
        let remote = |uid: String, synced: u64| RemoteRef {
            url: inc.url.clone(),
            etag: inc.etag.clone(),
            uid,
            synced,
        };
        match kind {
            PimKind::Contacts => {
                let Some((card, uid)) = read_contact(&inc.data) else {
                    problems.push(format!("{} is not a vCard", inc.url));
                    // Not taken: it must not look like an item deleted here on the next run.
                    known.remove(&inc.url);
                    continue;
                };
                let old = inc.local.and_then(|(id, fp)| s.mailbox.contacts.get(&id).map(|c| (c.clone(), fp)));
                match old {
                    Some((o, fp)) if contact_fingerprint(&o) == fp => {
                        let mut c = merged_contact(card, Some(&o));
                        c.remote = Some(remote(uid, contact_fingerprint(&c)));
                        s.mailbox.contacts.insert(c.id, c);
                    }
                    Some((o, _)) => {
                        // Changed here while the run went on: this copy wins on the next run.
                        let synced = contact_fingerprint(&merged_contact(card, Some(&o)));
                        if let Some(c) = s.mailbox.contacts.get_mut(&o.id) {
                            c.remote = Some(remote(uid, synced));
                        }
                    }
                    None if claimed.contains(&inc.url) => {}
                    None => {
                        let id = ItemId(s.mailbox.alloc_id());
                        let mut c = Contact { id, folder, ..card };
                        c.remote = Some(remote(uid, contact_fingerprint(&c)));
                        s.mailbox.contacts.insert(id, c);
                        s.sync.track.pim_created.insert(id);
                    }
                }
            }
            PimKind::Tasks => {
                let Some((task, uid)) = read_task(&inc.data, tz) else {
                    problems.push(format!("{} is not a task", inc.url));
                    known.remove(&inc.url);
                    continue;
                };
                let old = inc.local.and_then(|(id, fp)| s.mailbox.tasks.get(&id).map(|t| (t.clone(), fp)));
                match old {
                    Some((o, fp)) if task_fingerprint(&o) == fp => {
                        let mut t = merged_task(task, Some(&o));
                        t.uid = if uid.is_empty() { o.uid.clone() } else { uid.clone() };
                        t.modified = Some(now);
                        t.remote = Some(remote(uid, task_fingerprint(&t)));
                        s.mailbox.tasks.insert(t.id, t);
                    }
                    Some((o, _)) => {
                        let synced = task_fingerprint(&merged_task(task, Some(&o)));
                        if let Some(t) = s.mailbox.tasks.get_mut(&o.id) {
                            t.remote = Some(remote(uid, synced));
                        }
                    }
                    None if claimed.contains(&inc.url) => {}
                    None => {
                        let id = ItemId(s.mailbox.alloc_id());
                        let mut t = Task { id, folder, uid: uid.clone(), modified: Some(now), ..task };
                        t.remote = Some(remote(uid, task_fingerprint(&t)));
                        s.mailbox.tasks.insert(id, t);
                        s.sync.track.pim_created.insert(id);
                    }
                }
            }
        }
    }
    for (id, fp) in out.removed {
        match kind {
            PimKind::Contacts => {
                let unchanged = s.mailbox.contacts.get(&id).is_some_and(|c| contact_fingerprint(c) == fp);
                if unchanged {
                    s.mailbox.contacts.remove(&id);
                    s.sync.track.pim_removed.insert(id);
                } else if let Some(c) = s.mailbox.contacts.get_mut(&id) {
                    c.remote = None;
                }
            }
            PimKind::Tasks => {
                let unchanged = s.mailbox.tasks.get(&id).is_some_and(|t| task_fingerprint(t) == fp);
                if unchanged {
                    s.mailbox.tasks.remove(&id);
                    s.sync.track.pim_removed.insert(id);
                } else if let Some(t) = s.mailbox.tasks.get_mut(&id) {
                    t.remote = None;
                }
            }
        }
    }
    // Items moved out of the folder keep no link to the server copy this run deleted.
    let deleted: BTreeSet<&str> = out.deleted.iter().map(String::as_str).collect();
    if !deleted.is_empty() {
        let hit = |r: &Option<RemoteRef>| r.as_ref().is_some_and(|r| deleted.contains(r.url.as_str()));
        for c in s.mailbox.contacts.values_mut().filter(|c| c.folder != folder && hit(&c.remote)) {
            c.remote = None;
        }
        for t in s.mailbox.tasks.values_mut().filter(|t| t.folder != folder && hit(&t.remote)) {
            t.remote = None;
        }
    }
    if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == account) {
        a.sync.dav.insert(folder, DavFolderState { collection: out.collection, known });
    }
    if !problems.is_empty() {
        let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
        for p in problems.iter().take(5) {
            log::warn!("contacts/tasks sync of {name}: {p}");
        }
        let first = problems.first().cloned().unwrap_or_default();
        s.sync.runs.entry(account).or_default().error =
            Some(format!("{name}: {} item(s) could not be synchronised ({first})", problems.len()));
    }
}

/// Records the outcome of publishing (or removing) calendar `folder`'s file at `url`. A result for a
/// publication that changed meanwhile (published again elsewhere, or stopped) does not touch the new one.
fn published(
    s: &mut Session,
    account: AccountId,
    folder: FolderId,
    url: &str,
    fingerprint: u64,
    withdrawn: bool,
    result: Result<(), String>,
) {
    use crate::cmd::calendar::props;
    let mut p = props::read(&s.mailbox, folder);
    let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
    let this = |x: &mailcraft_model::calendar::Publication| x.url == url && x.account == account;
    if withdrawn {
        let stopped = p.publication.as_ref().is_some_and(|x| x.withdraw && this(x));
        match result {
            Ok(()) => {
                p.withdrawals.retain(|w| !(w.url == url && w.account == account));
                if stopped {
                    p.publication = None;
                    s.notify(Notice::Info(format!("The calendar \"{name}\" is no longer published.")));
                }
            }
            Err(e) => {
                let e: String = e.chars().take(500).collect();
                s.notify(Notice::Error(format!(
                    "Couldn't remove the published copy of the calendar \"{name}\" at {url}: {e}"
                )));
                if let Some(x) = p.publication.as_mut().filter(|_| stopped) {
                    x.last_error = Some(e);
                }
            }
        }
        props::write(&mut s.mailbox, folder, &p);
        return;
    }
    let Some(publication) = p.publication.as_mut().filter(|x| !x.withdraw && this(x)) else { return };
    match result {
        Ok(()) => {
            let first = publication.last_published.is_none();
            publication.fingerprint = fingerprint;
            publication.last_published = Some(s.now().unix);
            publication.last_error = None;
            if first {
                let at = publication.url.clone();
                s.notify(Notice::Info(format!("The calendar \"{name}\" is published at {at}.")));
            }
        }
        Err(e) => {
            let e: String = e.chars().take(500).collect();
            publication.last_error = Some(e.clone());
            s.notify(Notice::Error(format!("Couldn't publish the calendar \"{name}\": {e}")));
        }
    }
    props::write(&mut s.mailbox, folder, &p);
}

/// Undo/redo replaced `live` with a snapshot (now `restored`): items and server links that synchronisation
/// brought meanwhile stay (they are the server's state, not an editing step).
pub(super) fn carry_over(
    live: &Mailbox,
    restored: &mut Mailbox,
    created: &BTreeSet<ItemId>,
    removed: &BTreeSet<ItemId>,
) {
    // Accounts whose contacts and tasks server the undone step changed: the restored settings come back with
    // the links they had (the live ones point at the other server).
    let own_links: BTreeSet<AccountId> = restored
        .accounts
        .iter()
        .filter(|a| live.account(a.id).is_some_and(|l| dav_identity(l) != dav_identity(a)))
        .map(|a| a.id)
        .collect();
    for a in restored.accounts.iter_mut() {
        if let Some(l) = live.account(a.id) {
            if !own_links.contains(&a.id) {
                a.sync.dav = l.sync.dav.clone();
            }
            a.sync.shared_roots = l.sync.shared_roots.clone();
        }
    }
    let keeps_links: BTreeSet<FolderId> =
        restored.folders.values().filter(|f| own_links.contains(&f.account)).map(|f| f.id).collect();
    // Folders made for server collections meanwhile.
    let mapped: Vec<FolderId> = live.accounts.iter().flat_map(|a| a.sync.dav.keys().copied()).collect();
    for f in mapped {
        if let Some(lf) = live.folders.get(&f)
            && !restored.folders.contains_key(&f)
            && lf.parent.is_none_or(|p| restored.folders.contains_key(&p))
            && restored.account(lf.account).is_some()
        {
            restored.folders.insert(f, lf.clone());
        }
    }
    if !created.is_empty() {
        restored.next_id = restored.next_id.max(live.next_id);
    }
    for id in created {
        if let Some(c) = live.contacts.get(id)
            && !restored.contacts.contains_key(id)
            && restored.folders.contains_key(&c.folder)
        {
            restored.contacts.insert(*id, c.clone());
        }
        if let Some(t) = live.tasks.get(id)
            && !restored.tasks.contains_key(id)
            && restored.folders.contains_key(&t.folder)
        {
            restored.tasks.insert(*id, t.clone());
        }
    }
    for id in removed {
        if !live.contacts.contains_key(id) {
            restored.contacts.remove(id);
        }
        if !live.tasks.contains_key(id) {
            restored.tasks.remove(id);
        }
    }
    // An item the undone command removed comes back: its server copy may be gone meanwhile, so it counts as
    // changed here and is uploaded again (instead of being taken for one deleted on the server).
    for (id, c) in restored.contacts.iter_mut() {
        match live.contacts.get(id) {
            Some(_) if keeps_links.contains(&c.folder) => {}
            Some(l) => c.remote = l.remote.clone(),
            None => {
                if let Some(r) = c.remote.as_mut() {
                    r.synced = 0;
                }
            }
        }
    }
    for (id, t) in restored.tasks.iter_mut() {
        match live.tasks.get(id) {
            Some(_) if keeps_links.contains(&t.folder) => {}
            Some(l) => t.remote = l.remote.clone(),
            None => {
                if let Some(r) = t.remote.as_mut() {
                    r.synced = 0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_follow_server_fields_only() {
        let c = Contact {
            first_name: "Pat".into(),
            emails: vec!["pat@example.net".into()],
            ..Default::default()
        };
        let fp = contact_fingerprint(&c);
        let flag = mailcraft_model::FlagState::Flagged {
            label: "Follow up".into(),
            start: None,
            due: None,
            reminder: None,
        };
        let flagged = Contact { flag, ..c.clone() };
        assert_eq!(contact_fingerprint(&flagged), fp, "the follow-up flag is local-only");
        let renamed = Contact { last_name: "Example".into(), ..c.clone() };
        assert_ne!(contact_fingerprint(&renamed), fp);
        let linked = Contact {
            remote: Some(RemoteRef { url: "https://dav.example.com/c/1.vcf".into(), ..Default::default() }),
            ..c.clone()
        };
        assert_eq!(contact_fingerprint(&linked), fp);
        let t = Task { id: ItemId(4), subject: "Report".into(), ..Default::default() };
        let tf = task_fingerprint(&t);
        assert_eq!(task_fingerprint(&Task { from_mail: Some(ItemId(9)), ..t.clone() }), tf);
        assert_ne!(task_fingerprint(&Task { percent_complete: 50, ..t.clone() }), tf);
        assert_eq!(task_fingerprint(&Task { uid: "x".into(), ..t.clone() }), tf, "the UID is the server's");
    }

    #[test]
    fn cards_and_tasks_round_trip_with_uids() {
        let c = Contact { first_name: "Pat".into(), last_name: "Example".into(), ..Default::default() };
        let card = contact_card(&c, "u-1;x");
        assert!(card.contains("UID:u-1x\r\nEND:VCARD"), "{card}");
        let (back, uid) = read_contact(&card).unwrap();
        assert_eq!(uid, "u-1x");
        assert_eq!(contact_fingerprint(&back), contact_fingerprint(&c));
        let folded =
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:A\r\nitem1.UID;VALUE=text:abc\r\n def\r\nEND:VCARD\r\n";
        assert_eq!(read_contact(folded).unwrap().1, "abcdef");
        assert!(read_contact("junk").is_none());
        let t = Task {
            subject: "Plan".into(),
            percent_complete: 25,
            recurrence: Some(TaskRecurrence { rrule: "FREQ=WEEKLY".into(), ..Default::default() }),
            ..Default::default()
        };
        let text = task_calendar(&t, "t-1", DateTime::from_unix(1_700_000_000, 0));
        let (back, uid) = read_task(&text, 0).unwrap();
        assert_eq!(uid, "t-1");
        assert_eq!(back.subject, "Plan");
        assert_eq!(back.recurrence.map(|r| r.rrule), Some("FREQ=WEEKLY".into()));
        assert!(read_task("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", 0).is_none());
        assert_eq!(resource_url("https://h/c", "a b/c", PimKind::Contacts), "https://h/c/a-b-c.vcf");
    }

    fn local_session() -> (Session, AccountId) {
        let s = Session::new(Mailbox::new_local("Local Folders"));
        let account = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        (s, account)
    }

    #[test]
    fn web_origins_and_dav_identity() {
        assert_eq!(web_origin("HTTPS://Dav.Example.com:443/a/b").as_deref(), Some("https://dav.example.com"));
        assert_eq!(web_origin("http://dav.example.com:8080").as_deref(), Some("http://dav.example.com:8080"));
        assert_eq!(web_origin("https://[::1]:8443/x").as_deref(), Some("https://[::1]:8443"));
        assert_eq!(web_origin("https://dav.example.com@elsewhere.example.net/"), None, "no user names");
        assert_eq!(web_origin("ftp://dav.example.com/"), None);
        assert_eq!(web_origin("https://:443/"), None);
        assert_eq!(web_origin("https://h.example.com:x/"), None);
        let mut a = mailcraft_model::Account {
            address: "pat@example.com".into(),
            dav: Some(mailcraft_model::account::DavSettings {
                url: "https://DAV.example.com/dav/".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let id = dav_identity(&a);
        assert_eq!(id, Some(("https://dav.example.com/dav".into(), "pat@example.com".into())));
        if let Some(d) = a.dav.as_mut() {
            d.url = "https://dav.example.com:443/dav".into();
        }
        assert_eq!(dav_identity(&a), id, "the same server spelled differently");
        if let Some(d) = a.dav.as_mut() {
            d.username = "pat".into();
        }
        assert_ne!(dav_identity(&a), id, "another login");
        a.dav = None;
        assert_eq!(dav_identity(&a), None);
    }

    #[test]
    fn server_edits_keep_the_series_progress() {
        let start = Some(DateTime::from_unix(1_790_000_000, 0));
        let old = Task {
            id: ItemId(3),
            recurrence: Some(TaskRecurrence {
                rrule: "FREQ=WEEKLY;COUNT=5".into(),
                start,
                completed_count: 3,
                ..Default::default()
            }),
            ..Default::default()
        };
        // What read_task makes of the server copy: the rule only.
        let rule = |r: &str| Some(TaskRecurrence { rrule: r.into(), start: None, ..Default::default() });
        let edited =
            Task { subject: "Renamed".into(), recurrence: rule("FREQ=WEEKLY;COUNT=5"), ..Default::default() };
        let m = merged_task(edited, Some(&old));
        assert_eq!(m.subject, "Renamed");
        let r = m.recurrence.unwrap();
        assert_eq!((r.completed_count, r.start), (3, start), "the progress through the series stays");
        let other = Task { recurrence: rule("FREQ=DAILY"), ..Default::default() };
        let r = merged_task(other, Some(&old)).recurrence.unwrap();
        assert_eq!((r.rrule.as_str(), r.completed_count), ("FREQ=DAILY", 0), "a new rule starts afresh");
    }

    #[test]
    fn tasks_sharing_a_uid_go_up_under_their_own() {
        let (mut s, account) = local_session();
        let folder = s.mailbox.role_folder(account, FolderRole::Tasks).unwrap();
        let collection = "https://dav.example.com/t/";
        if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == account) {
            a.sync
                .dav
                .insert(folder, DavFolderState { collection: collection.into(), known: BTreeMap::new() });
        }
        let remote = RemoteRef {
            url: format!("{collection}u1.ics"),
            etag: "\"1\"".into(),
            uid: "u1".into(),
            synced: 1,
        };
        for (n, remote) in [(1, Some(remote.clone())), (2, Some(remote)), (3, None), (4, None)] {
            let id = ItemId(1000 + n);
            s.mailbox.tasks.insert(id, Task { id, folder, uid: "u1".into(), remote, ..Default::default() });
        }
        let job = folder_job(&s, account, folder, PimKind::Tasks);
        let uids: Vec<&str> = job.items.iter().map(|i| i.uid.as_str()).collect();
        assert_eq!(uids[0], "u1", "the task holding the server copy keeps its UID");
        let distinct: BTreeSet<&str> = uids.iter().copied().collect();
        assert_eq!(distinct.len(), 4, "{uids:?}");
        assert!(job.items.iter().skip(1).all(|i| i.remote.is_none()));
        assert!(!job.too_many);
    }

    #[test]
    fn a_folder_over_the_item_limit_is_not_synchronised() {
        let (mut s, account) = local_session();
        let folder = s.mailbox.role_folder(account, FolderRole::Contacts).unwrap();
        for n in 0..=MAX_ITEMS {
            let id = ItemId(10_000 + u64::try_from(n).unwrap());
            s.mailbox.contacts.insert(id, Contact { id, folder, ..Default::default() });
        }
        let job = folder_job(&s, account, folder, PimKind::Contacts);
        assert!(job.too_many);
        assert!(job.items.is_empty());
    }

    #[test]
    fn publish_results_apply_to_the_publication_they_were_for() {
        use mailcraft_model::calendar::{Publication, Withdrawal};
        let (mut s, account) = local_session();
        let cal = s.mailbox.role_folder(account, FolderRole::Calendar).unwrap();
        let old = "https://dav.example.com/p/old.ics";
        let new = "https://dav.example.com/p/new.ics";
        let mut p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        p.publication = Some(Publication { url: new.into(), account, ..Default::default() });
        p.withdrawals = vec![Withdrawal { url: old.into(), account }];
        crate::cmd::calendar::props::write(&mut s.mailbox, cal, &p);
        let ev = |url: &str, withdrawn: bool| PimEvent::Published {
            folder: cal,
            url: url.into(),
            fingerprint: 7,
            withdrawn,
            result: Ok(()),
        };
        // An upload to the old address that finished late does not mark the new one published.
        apply(&mut s, account, ev(old, false));
        let p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        assert_eq!(p.publication.as_ref().unwrap().last_published, None);
        // The old file's removal clears the pending withdrawal only.
        apply(&mut s, account, ev(old, true));
        let p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        assert!(p.withdrawals.is_empty());
        assert_eq!(p.publication.as_ref().unwrap().url, new);
        apply(&mut s, account, ev(new, false));
        let p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        assert_eq!(p.publication.as_ref().unwrap().fingerprint, 7);
        // Stopped, then published again before the removal came back: the new publication stays.
        let mut p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        p.publication = Some(Publication { url: old.into(), account, ..Default::default() });
        crate::cmd::calendar::props::write(&mut s.mailbox, cal, &p);
        apply(&mut s, account, ev(new, true));
        let p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        assert_eq!(p.publication.map(|x| x.url).as_deref(), Some(old));
    }
}
