//! Exchange accounts over Exchange Web Services (EWS; see `mailcraft_net::ews`): folders from FindFolder
//! (listed again when SyncFolderHierarchy reports a change), mail from SyncFolderItems per folder with its
//! sync state (new messages downloaded whole as MimeContent; on the first run of a folder only mail received
//! inside the account's "download mail from the past N months" window), local changes written back with
//! UpdateItem (read, flag, categories, replied/forwarded), MoveItem, DeleteItem, CreateItem (uploads) and
//! CreateFolder / UpdateFolder / MoveFolder / DeleteFolder, and the Outbox sent with CreateItem
//! `SendAndSaveCopy` (the server files the copy in Sent Items). Contacts, calendar and tasks are in
//! `ews_pim.rs`; automatic replies, availability and meeting answers in `ews_extra.rs`.
//!
//! Folder `remote_name` = EWS folder id; item `remote_id` = EWS item id. Like JMAP, EWS ids are strings and
//! the account keeps a *shadow* of what the server last had for each item (folder, read, flag, categories,
//! last verb) and for each folder without a role (name and parent), in the account options; each run sends
//! what differs. EWS item ids change when an item moves: a move made here updates the item's id from the
//! server's answer; a move made elsewhere arrives as a deletion in one folder and a new item in the other.
//!
//! **New folders.** As for JMAP: a folder made here is created first in a run and referred to as
//! `#f<folder id>` until the server names it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use mailcraft_net::ews::{EwsAuth, EwsClient, EwsFolder, FlagChange, ItemChange, is_code};
use mailcraft_net::{ConnectOptions, Credentials, NetError};

use super::native::{Event, Events, Outgoing};
use super::*;
use crate::accounts::{EwsShadow, JmapFolderShadow, prefs};
use mailcraft_model::{Account, FolderRole};

/// Changes asked for per SyncFolderItems call.
const SYNC_PAGE: usize = 256;
/// Most SyncFolderItems rounds per folder in one run.
const MAX_ROUNDS: usize = 400;
/// Bound on the messages one first run of a folder downloads (the date window is the real limit).
const MAX_INITIAL: usize = 200_000;

/// The distinguished folders mapped onto default folders.
const ROLES: [(&str, FolderRole); 6] = [
    ("inbox", FolderRole::Inbox),
    ("sentitems", FolderRole::SentItems),
    ("drafts", FolderRole::Drafts),
    ("deleteditems", FolderRole::DeletedItems),
    ("junkemail", FolderRole::JunkEmail),
    ("archive", FolderRole::Archive),
];

/// How to reach an Exchange account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EwsConn {
    url: String,
    auth: EwsAuth,
    options: ConnectOptions,
    address: String,
}

impl EwsConn {
    pub(crate) fn new(
        url: &str,
        mechanism: &str,
        creds: &Credentials,
        options: ConnectOptions,
        address: &str,
    ) -> EwsConn {
        let auth = match creds {
            Credentials::Password { user, password } => EwsAuth::from_mechanism(mechanism, user, password),
            Credentials::OAuth2 { access_token, .. } => EwsAuth::Bearer(access_token.clone()),
        };
        EwsConn { url: url.trim().to_owned(), auth, options, address: address.trim().to_owned() }
    }
    pub(crate) fn set_bearer(&mut self, token: &str) {
        if let EwsAuth::Bearer(t) = &mut self.auth {
            *t = token.to_owned();
        }
    }
    pub(crate) fn url(&self) -> &str {
        &self.url
    }
    /// Caps the connect and read timeouts (for a request a command waits for).
    pub(crate) fn limit_timeouts(&mut self, connect: std::time::Duration, io: std::time::Duration) {
        self.options.connect_timeout = self.options.connect_timeout.min(connect);
        self.options.io_timeout = self.options.io_timeout.min(io);
    }
    pub(crate) fn address(&self) -> &str {
        &self.address
    }
    /// A client (nothing is sent yet).
    pub(crate) fn client(&self) -> Result<EwsClient, NetError> {
        let http = mailcraft_net::http::HttpClient::new(self.options.clone());
        let anchor = Some(self.address.as_str()).filter(|a| !a.is_empty());
        EwsClient::new(&self.url, self.auth.clone(), http, anchor)
    }
    /// A client that has signed in.
    pub(crate) fn connect(&self) -> Result<EwsClient, NetError> {
        let c = self.client()?;
        c.distinguished(&["msgfolderroot"])?;
        Ok(c)
    }
}

/// The URL an EWS server name gives: a full URL as it is, a bare host name at the usual path.
pub(crate) fn ews_url(host: &str) -> String {
    let h = host.trim();
    if h.contains("://") { h.to_owned() } else { format!("https://{h}/EWS/Exchange.asmx") }
}

/// One message to send: the Outbox item, its source and the blind copies.
#[derive(Clone, Debug)]
pub(crate) struct EwsSend {
    item: ItemId,
    raw: Vec<u8>,
    bcc: Vec<String>,
}

/// Local changes and receive state for one run.
#[derive(Clone, Debug, Default)]
pub(crate) struct EwsJob {
    receive: bool,
    hierarchy_state: String,
    /// Server folders to update (`None`: every mail folder), with their sync states.
    folders: Option<Vec<String>>,
    states: BTreeMap<String, String>,
    /// First runs of a folder: only mail received at or after this (Unix time).
    received_after: Option<i64>,
    /// (item id, change, shadow after it).
    updates: Vec<(String, FlagChange, EwsShadow)>,
    /// (item id, target folder or placeholder, shadow after the move).
    moves: Vec<(String, String, EwsShadow)>,
    destroy: Vec<String>,
    /// (item, folder or placeholder, source, read, draft, shadow).
    uploads: Vec<(ItemId, String, Vec<u8>, bool, bool, EwsShadow)>,
    /// Folders to create, parents first: (folder, name, parent folder id or placeholder; `None` = the root).
    folder_creates: Vec<(FolderId, String, Option<String>)>,
    /// (folder id, new name if renamed, new parent if moved (`None` inside = the root), shadow after).
    folder_updates: Vec<(String, Option<String>, Option<Option<String>>, JmapFolderShadow)>,
    folder_destroys: Vec<String>,
    send: Vec<EwsSend>,
}

/// One message as the server reported it.
#[derive(Clone, Debug, Default)]
pub(crate) struct EItem {
    id: String,
    folder: String,
    shadow: EwsShadow,
    size: Option<u64>,
    raw: Option<Vec<u8>>,
}

pub(crate) enum EwsEvent {
    /// The folder list: mail folders, the server ids of the distinguished folders, the hierarchy state.
    Folders {
        list: Vec<EwsFolder>,
        roles: Vec<(String, String)>,
        root: String,
        state: String,
    },
    Items {
        list: Vec<EItem>,
    },
    Gone {
        ids: Vec<String>,
    },
    ReadFlags {
        list: Vec<(String, bool)>,
    },
    ItemState {
        folder: String,
        state: String,
    },
    /// The folder's sync state is no longer valid: start it over.
    Reset {
        folder: String,
    },
    Updated {
        id: String,
        shadow: EwsShadow,
    },
    Moved {
        old: String,
        new: String,
        shadow: EwsShadow,
    },
    Destroyed {
        ids: Vec<String>,
    },
    Uploaded {
        item: ItemId,
        id: String,
        shadow: EwsShadow,
    },
    FolderCreated {
        folder: FolderId,
        id: String,
        shadow: JmapFolderShadow,
    },
    FolderUpdated {
        id: String,
        shadow: JmapFolderShadow,
    },
    FoldersDestroyed {
        ids: Vec<String>,
    },
}

// ---------------------------------------------------------------------------------------------------
// Session side: building a job
// ---------------------------------------------------------------------------------------------------

/// PidTagFlagStatus of an item's flag.
fn flag_status(m: &MailItem) -> i64 {
    match m.flag {
        FlagState::None => 0,
        FlagState::Complete { .. } => 1,
        FlagState::Flagged { .. } => 2,
    }
}

/// PidTagLastVerbExecuted of an item's response.
fn verb(m: &MailItem) -> Option<i64> {
    match m.response {
        ResponseState::Replied(_) => Some(102),
        ResponseState::RepliedAll(_) => Some(103),
        ResponseState::Forwarded(_) => Some(104),
        ResponseState::None => None,
    }
}

/// What the server should have for `m` in `folder`.
fn shadow_of(m: &MailItem, folder: &str) -> EwsShadow {
    let mut categories = m.categories.clone();
    categories.sort();
    EwsShadow { folder: folder.to_owned(), read: m.read, flag: flag_status(m), categories, verb: verb(m) }
}

fn placeholder(f: FolderId) -> String {
    format!("#f{}", f.0)
}

fn resolve(id: &str, created: &BTreeMap<String, String>) -> Option<String> {
    if id.starts_with("#f") { created.get(id).cloned() } else { Some(id.to_owned()) }
}

/// The local changes and receive state of one run. `window` is the first day (UTC) of a first download.
pub(crate) fn build_job(
    s: &Session,
    a: &Account,
    plan: &Plan,
    window: Option<(i64, u8, u8)>,
    outgoing: &[Outgoing],
) -> Option<EwsJob> {
    let st = s.sync.prefs.data.ews.get(&a.id.0).cloned().unwrap_or_default();
    let outbox = s.mailbox.role_folder(a.id, FolderRole::Outbox);
    let received_after = window
        .and_then(|(y, m, d)| mailcraft_model::dates::days_from_civil(y, m, d))
        .map(|days| days.saturating_mul(86_400));
    let mut job = EwsJob {
        receive: plan.receive,
        hierarchy_state: st.hierarchy_state.clone(),
        states: st.item_states.clone(),
        received_after,
        ..Default::default()
    };
    job.folders = plan.folders.as_ref().map(|fs| {
        fs.iter()
            .filter_map(|f| s.mailbox.folder(*f))
            .filter(|f| f.account == a.id)
            .filter_map(|f| f.remote_name.clone())
            .collect()
    });
    let target = folder_job(s, a, &st, outbox, &mut job);
    let mut here: BTreeSet<&str> = BTreeSet::new();
    for m in s.mailbox.mail.items() {
        let Some(f) = s.mailbox.folder(m.folder).filter(|f| f.account == a.id && f.kind == FolderKind::Mail)
        else {
            continue;
        };
        if Some(f.id) == outbox {
            continue;
        }
        // Mail in a folder that stays local is left where it is on the server, never deleted there.
        if let Some(id) = m.remote_id.as_deref() {
            here.insert(id);
        }
        let Some(folder) = target.get(&f.id).map(String::as_str) else { continue };
        let want = shadow_of(m, folder);
        match m.remote_id.as_deref() {
            Some(id) => {
                let Some(sh) = st.shadow.get(id) else { continue };
                if sh.folder != want.folder {
                    job.moves.push((id.to_owned(), want.folder.clone(), want.clone()));
                }
                let change = FlagChange {
                    read: (sh.read != want.read).then_some(want.read),
                    flag_status: (sh.flag != want.flag).then_some(want.flag),
                    categories: (sh.categories != want.categories).then(|| want.categories.clone()),
                    last_verb: (sh.verb != want.verb).then_some(want.verb.unwrap_or(0)),
                };
                if change != FlagChange::default() {
                    let mut after = want.clone();
                    // The flags are changed before the move: the shadow keeps the old folder until it moves.
                    after.folder = sh.folder.clone();
                    job.updates.push((id.to_owned(), change, after));
                }
            }
            None => job.uploads.push((m.id, folder.to_owned(), item_source(s, m), m.read, m.is_draft, want)),
        }
    }
    job.destroy = st.shadow.keys().filter(|id| !here.contains(id.as_str())).cloned().collect();
    for o in outgoing {
        let bcc = s
            .mailbox
            .mail
            .get(o.item)
            .map(|m| m.bcc.iter().map(|b| b.addr.clone()).collect())
            .unwrap_or_default();
        job.send.push(EwsSend { item: o.item, raw: o.raw.clone(), bcc });
    }
    if !job.receive
        && job.updates.is_empty()
        && job.moves.is_empty()
        && job.destroy.is_empty()
        && job.uploads.is_empty()
        && job.folder_creates.is_empty()
        && job.folder_updates.is_empty()
        && job.folder_destroys.is_empty()
        && job.send.is_empty()
    {
        return None;
    }
    Some(job)
}

fn folder_depth(mb: &Mailbox, f: FolderId) -> usize {
    let mut n = 0usize;
    let mut cur = mb.folder(f).and_then(|x| x.parent);
    while let Some(p) = cur {
        n += 1;
        if n > mb.folders.len() {
            break;
        }
        cur = mb.folder(p).and_then(|x| x.parent);
    }
    n
}

/// The folder part of a job. Returns, for every mail folder of the account that is (or will be) on the
/// server, its folder id or placeholder.
fn folder_job(
    s: &Session,
    a: &Account,
    st: &crate::accounts::EwsState,
    outbox: Option<FolderId>,
    job: &mut EwsJob,
) -> BTreeMap<FolderId, String> {
    let mut folders: Vec<&Folder> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.account == a.id && f.kind == FolderKind::Mail && Some(f.id) != outbox)
        .collect();
    folders.sort_by_cached_key(|f| (folder_depth(&s.mailbox, f.id), f.id));
    let mut target: BTreeMap<FolderId, String> = BTreeMap::new();
    let listed = !st.hierarchy_state.is_empty();
    // A default folder the server lacks (Archive) is made there once mail from the server is moved into it or
    // into a folder below it (else that mail would stay behind in its old folder); subfolders follow.
    let mut holds_server_mail: BTreeSet<FolderId> = BTreeSet::new();
    for m in s.mailbox.mail.items().filter(|m| m.remote_id.is_some()) {
        let mut cur = Some(m.folder);
        let mut steps = 0usize;
        while let Some(f) = cur {
            if !holds_server_mail.insert(f) || steps > s.mailbox.folders.len() {
                break;
            }
            steps += 1;
            cur = s.mailbox.folder(f).and_then(|x| x.parent);
        }
    }
    for f in &folders {
        if let Some(r) = &f.remote_name {
            target.insert(f.id, r.clone());
        } else if f.role.is_none() {
            let parent = match f.parent {
                None => Some(None),
                Some(p) => target.get(&p).map(|t| Some(t.clone())),
            };
            if let Some(parent) = parent {
                job.folder_creates.push((f.id, f.name.clone(), parent));
                target.insert(f.id, placeholder(f.id));
            }
        } else if f.role == Some(FolderRole::Archive) && f.parent.is_none() && listed {
            // A server without an archive folder gets one when mail from it is filed there (or below it).
            if holds_server_mail.contains(&f.id) {
                job.folder_creates.push((f.id, f.name.clone(), None));
                target.insert(f.id, placeholder(f.id));
            }
        }
    }
    let mut has_plain = false;
    for f in &folders {
        let Some(r) = f.remote_name.as_ref().filter(|_| f.role.is_none()) else { continue };
        has_plain = true;
        let Some(sh) = st.folders.get(r) else { continue };
        let parent = match f.parent {
            None => Some(None),
            Some(p) => target.get(&p).map(|t| Some(t.clone())),
        };
        let Some(parent) = parent else { continue };
        let rename = (sh.name != f.name).then(|| f.name.clone());
        let reparent = (sh.parent != parent).then(|| parent.clone());
        if rename.is_some() || reparent.is_some() {
            job.folder_updates.push((
                r.clone(),
                rename,
                reparent,
                JmapFolderShadow { name: f.name.clone(), parent },
            ));
        }
    }
    let mapped: BTreeSet<&str> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.account == a.id)
        .filter_map(|f| f.remote_name.as_deref())
        .collect();
    let depth = |id: &str| {
        let mut n = 0usize;
        let mut cur = st.folders.get(id).and_then(|x| x.parent.clone());
        while let Some(p) = cur {
            n += 1;
            if n > st.folders.len() {
                break;
            }
            cur = st.folders.get(&p).and_then(|x| x.parent.clone());
        }
        n
    };
    let mut gone: Vec<String> =
        st.folders.keys().filter(|id| !mapped.contains(id.as_str())).cloned().collect();
    gone.sort_by_cached_key(|id| std::cmp::Reverse(depth(id)));
    job.folder_destroys = gone;
    if has_plain && st.folders.is_empty() {
        job.hierarchy_state.clear();
    }
    target
}

// ---------------------------------------------------------------------------------------------------
// Worker side
// ---------------------------------------------------------------------------------------------------

fn send(events: &Events, cancel: &AtomicBool, account: AccountId, ev: EwsEvent) {
    if !events.send(Event::Ews { account, ev }) {
        cancel.store(true, Ordering::SeqCst);
    }
}

/// Unix time of an xs:dateTime (fractions of a second and a trailing zone letter allowed).
pub(crate) fn parse_time(s: &str) -> Option<i64> {
    parse_datetime(s).map(|d| d.unix)
}

/// An xs:dateTime as a date-time (UTC when it names no zone).
pub(crate) fn parse_datetime(s: &str) -> Option<mailcraft_model::DateTime> {
    let s = s.trim();
    let (date_part, rest) = s.split_once('T').unwrap_or((s, ""));
    let (clock, zone) = match rest.find(['Z', 'z', '+']).or_else(|| rest.rfind('-').filter(|i| *i >= 5)) {
        Some(i) => (rest.get(..i)?, rest.get(i..)?),
        None => (rest, ""),
    };
    let clock = clock.split('.').next().unwrap_or_default();
    let zone = if zone.eq_ignore_ascii_case("z") || zone.is_empty() { "Z" } else { zone };
    let date_part = date_part.trim_end_matches(['Z', 'z']);
    let text = if clock.is_empty() {
        format!("{date_part}T00:00:00{zone}")
    } else {
        format!("{date_part}T{clock}{zone}")
    };
    mailcraft_model::DateTime::parse_rfc5322(&text).ok()
}

fn item_shadow(i: &mailcraft_net::ews::EwsItem, folder: &str) -> EwsShadow {
    let mut categories = i.categories.clone();
    categories.sort();
    EwsShadow {
        folder: folder.to_owned(),
        read: i.read.unwrap_or(false),
        flag: i.flag_status.unwrap_or(0),
        categories,
        verb: i.last_verb.filter(|v| matches!(v, 102..=104)),
    }
}

/// Creates and changes the job's folders on the server (parents first). Returns placeholder → folder id.
fn folder_changes(
    c: &EwsClient,
    j: &EwsJob,
    account: AccountId,
    events: &Events,
    cancel: &AtomicBool,
) -> Result<BTreeMap<String, String>, NetError> {
    let mut created: BTreeMap<String, String> = BTreeMap::new();
    for (folder, name, parent) in &j.folder_creates {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let parent_id = match parent {
            Some(p) => match resolve(p, &created) {
                Some(id) => id,
                None => continue,
            },
            None => "msgfolderroot".to_owned(),
        };
        match c.create_folder(&parent_id, name) {
            Ok(id) => {
                created.insert(placeholder(*folder), id.clone());
                let shadow = JmapFolderShadow {
                    name: name.clone(),
                    parent: parent.as_ref().map(|_| parent_id.clone()),
                };
                send(events, cancel, account, EwsEvent::FolderCreated { folder: *folder, id, shadow });
            }
            Err(e @ NetError::Ews { .. }) => {
                log::warn!("ews: folder {name:?} not created: {e}");
                events.send(Event::FolderFailed {
                    account,
                    name: name.clone(),
                    error: format!("the server did not create this folder ({e})"),
                });
            }
            Err(e) => return Err(e),
        }
    }
    for (id, rename, reparent, shadow) in &j.folder_updates {
        let mut shadow = shadow.clone();
        if let Some(p) = &shadow.parent {
            let Some(real) = resolve(p, &created) else { continue };
            shadow.parent = Some(real);
        }
        let mut ok = true;
        if let Some(n) = rename
            && let Err(e) = c.rename_folder(id, n)
        {
            log::warn!("ews: folder {id} not renamed: {e}");
            ok = false;
        }
        if ok && reparent.is_some() {
            let to = shadow.parent.clone().unwrap_or_else(|| "msgfolderroot".to_owned());
            if let Err(e) = c.move_folder(id, &to) {
                log::warn!("ews: folder {id} not moved: {e}");
                ok = false;
            }
        }
        if ok {
            send(events, cancel, account, EwsEvent::FolderUpdated { id: id.clone(), shadow });
        }
    }
    Ok(created)
}

pub(crate) fn run(
    account: AccountId,
    conn: &EwsConn,
    job: Option<&EwsJob>,
    events: &Events,
    cancel: &AtomicBool,
) -> Option<String> {
    let progress = |t: &str| {
        events.send(Event::step(account, t));
    };
    let cancelled = || cancel.load(Ordering::SeqCst);
    let j = job?;
    progress("Connecting…");
    let c = match conn.connect() {
        Ok(c) => c,
        Err(e) => return Some(e.to_string()),
    };
    let mut folders_changed_here = false;
    let res: Result<(), NetError> = (|| {
        // 1. Local changes.
        let created = folder_changes(&c, j, account, events, cancel)?;
        folders_changed_here = !j.folder_creates.is_empty() || !j.folder_updates.is_empty();
        if !j.updates.is_empty() {
            progress("Updating the server…");
            let list: Vec<(String, FlagChange)> =
                j.updates.iter().map(|(id, ch, _)| (id.clone(), ch.clone())).collect();
            let results = c.set_flags(&list)?;
            for ((id, _, shadow), r) in j.updates.iter().zip(results) {
                match r {
                    Ok(_) => send(
                        events,
                        cancel,
                        account,
                        EwsEvent::Updated { id: id.clone(), shadow: shadow.clone() },
                    ),
                    Err(e) if is_code(&e, "ErrorItemNotFound") => {
                        send(events, cancel, account, EwsEvent::Destroyed { ids: vec![id.clone()] });
                    }
                    Err(e) => log::warn!("ews: update of an item refused: {e}"),
                }
            }
        }
        // Moves, grouped by target folder.
        let mut by_target: BTreeMap<String, Vec<(String, EwsShadow)>> = BTreeMap::new();
        for (id, to, shadow) in &j.moves {
            let Some(real) = resolve(to, &created) else { continue };
            let mut sh = shadow.clone();
            sh.folder = real.clone();
            by_target.entry(real).or_default().push((id.clone(), sh));
        }
        for (to, items) in by_target {
            if cancelled() {
                break;
            }
            let ids: Vec<String> = items.iter().map(|(i, _)| i.clone()).collect();
            let results = c.move_items(&ids, &to)?;
            for ((old, shadow), r) in items.into_iter().zip(results) {
                match r {
                    Ok(new) => {
                        let new = new.map(|(n, _)| n).unwrap_or_else(|| old.clone());
                        send(events, cancel, account, EwsEvent::Moved { old, new, shadow });
                    }
                    Err(e) if is_code(&e, "ErrorItemNotFound") => {
                        send(events, cancel, account, EwsEvent::Destroyed { ids: vec![old] });
                    }
                    Err(e) => log::warn!("ews: move refused: {e}"),
                }
            }
        }
        if !j.destroy.is_empty() {
            let results = c.delete_items(&j.destroy, true)?;
            let gone: Vec<String> =
                j.destroy.iter().zip(results).filter(|(_, r)| r.is_ok()).map(|(id, _)| id.clone()).collect();
            send(events, cancel, account, EwsEvent::Destroyed { ids: gone });
        }
        for (item, folder, raw, read, draft, shadow) in &j.uploads {
            if cancelled() {
                break;
            }
            let Some(folder) = resolve(folder, &created) else { continue };
            progress("Uploading…");
            let (id, _) = match c.upload(&folder, raw, *read, *draft) {
                Ok(x) => x,
                // The server refused this one message (too large, not convertible, over quota): reported, and
                // the rest of the run (other uploads, sending, receiving) goes on. Only transport errors stop it.
                Err(e @ (NetError::Ews { .. } | NetError::Http { status: 413, .. })) => {
                    log::warn!("ews: upload of item {} refused: {e}", item.0);
                    events.send(Event::FolderFailed {
                        account,
                        name: "A message".into(),
                        error: format!("the server did not take it, so it stays only here ({e})"),
                    });
                    continue;
                }
                Err(e) => return Err(e),
            };
            let mut sh = shadow.clone();
            sh.folder = folder;
            send(events, cancel, account, EwsEvent::Uploaded { item: *item, id, shadow: sh });
        }
        if !j.folder_destroys.is_empty() && !cancelled() {
            let mut gone = Vec::new();
            for id in &j.folder_destroys {
                // A folder still holding mail (arrived meanwhile, or never downloaded here) or any subfolder
                // (kept, never listed here, or not a mail folder) is kept: deleting it would take them along.
                // The folder list is read again and its folder comes back.
                match c.folder(id) {
                    Ok(Some(f)) if f.total.unwrap_or(0) == 0 && f.children == Some(0) => {
                        match c.delete_folder(id) {
                            Ok(()) => gone.push(id.clone()),
                            Err(e) if is_code(&e, "ErrorFolderNotFound") => gone.push(id.clone()),
                            Err(e) => {
                                log::warn!("ews: folder not deleted: {e}");
                                folders_changed_here = true;
                            }
                        }
                    }
                    Ok(_) => folders_changed_here = true,
                    Err(e) if is_code(&e, "ErrorFolderNotFound") => gone.push(id.clone()),
                    Err(e) => return Err(e),
                }
            }
            send(events, cancel, account, EwsEvent::FoldersDestroyed { ids: gone });
        }
        // 2. The Outbox.
        if !j.send.is_empty() {
            progress("Sending…");
            for m in &j.send {
                if cancelled() {
                    break;
                }
                let result = c.send(&m.raw, &m.bcc, true).map_err(|e| e.to_string());
                events.send(Event::Sent { account, item: m.item, result });
            }
        }
        // 3. Receive.
        if !j.receive {
            return Ok(());
        }
        progress("Updating the folder list…");
        let (changed, hstate) = match c.hierarchy_changed(&j.hierarchy_state) {
            Ok(x) => x,
            Err(NetError::Ews { .. }) => (true, String::new()),
            Err(e) => return Err(e),
        };
        let mut mail_folders: Vec<String> = j.states.keys().cloned().collect();
        if changed || folders_changed_here || j.hierarchy_state.is_empty() {
            let names: Vec<&str> =
                ["msgfolderroot", "outbox"].into_iter().chain(ROLES.iter().map(|(n, _)| *n)).collect();
            let roles: Vec<(String, String)> =
                c.distinguished(&names)?.into_iter().filter_map(|(n, f)| Some((n, f?.id))).collect();
            let root = roles
                .iter()
                .find(|(n, _)| n == "msgfolderroot")
                .map(|(_, id)| id.clone())
                .unwrap_or_default();
            let outbox = roles.iter().find(|(n, _)| n == "outbox").map(|(_, id)| id.clone());
            let list: Vec<EwsFolder> =
                c.folders()?.into_iter().filter(|f| f.is_mail() && Some(&f.id) != outbox.as_ref()).collect();
            // Only folders below the root (and below other mail folders) count.
            let ids: BTreeSet<String> = list.iter().map(|f| f.id.clone()).collect();
            let list: Vec<EwsFolder> = list
                .into_iter()
                .filter(|f| f.parent.as_ref().is_none_or(|p| *p == root || ids.contains(p)))
                .collect();
            mail_folders = list.iter().map(|f| f.id.clone()).collect();
            send(events, cancel, account, EwsEvent::Folders { list, roles, root, state: hstate });
        }
        if let Some(only) = &j.folders {
            mail_folders.retain(|f| only.contains(f));
            for f in only {
                if !mail_folders.contains(f) {
                    mail_folders.push(f.clone());
                }
            }
        }
        progress("Updating messages…");
        let mut detail = SyncProgress { folder_count: mail_folders.len(), ..Default::default() };
        let mut throttle = super::native::Throttle::default();
        for folder in &mail_folders {
            if cancelled() {
                return Ok(());
            }
            detail.folder_index = detail.folder_index.saturating_add(1);
            if throttle.due(detail.folder_index == 1) {
                events.send(Event::counted(account, &detail));
            }
            sync_folder(&c, j, folder, account, events, cancel)?;
        }
        Ok(())
    })();
    match res {
        Ok(()) if cancelled() => Some("Send/Receive was cancelled.".into()),
        Ok(()) => None,
        Err(e) => Some(e.to_string()),
    }
}

/// Brings one folder up to date (SyncFolderItems rounds; new messages downloaded whole).
fn sync_folder(
    c: &EwsClient,
    j: &EwsJob,
    folder: &str,
    account: AccountId,
    events: &Events,
    cancel: &AtomicBool,
) -> Result<(), NetError> {
    let mut state = j.states.get(folder).cloned().unwrap_or_default();
    let mut first = state.is_empty();
    let mut downloaded = 0usize;
    let mut reset_once = false;
    for _ in 0..MAX_ROUNDS {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let page = match c.sync_items(folder, &state, SYNC_PAGE) {
            Ok(p) => p,
            Err(e) if is_code(&e, "ErrorInvalidSyncStateData") && !reset_once => {
                reset_once = true;
                send(events, cancel, account, EwsEvent::Reset { folder: folder.to_owned() });
                state.clear();
                first = true;
                continue;
            }
            Err(e) if is_code(&e, "ErrorFolderNotFound") || is_code(&e, "ErrorInvalidIdMalformed") => {
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let mut fresh: Vec<(String, Option<u64>)> = Vec::new();
        let mut updated: Vec<EItem> = Vec::new();
        let mut gone: Vec<String> = Vec::new();
        let mut read: Vec<(String, bool)> = Vec::new();
        for ch in page.changes {
            match ch {
                ItemChange::Create(i) => {
                    let old = first
                        && j.received_after.is_some_and(|cut| {
                            i.received.as_deref().and_then(parse_time).is_some_and(|t| t < cut)
                        });
                    if old || downloaded >= MAX_INITIAL {
                        continue;
                    }
                    downloaded += 1;
                    fresh.push((i.id, i.size));
                }
                ItemChange::Update(i) => {
                    let shadow = item_shadow(&i, folder);
                    updated.push(EItem {
                        id: i.id,
                        folder: folder.to_owned(),
                        shadow,
                        size: i.size,
                        raw: None,
                    });
                }
                ItemChange::Delete(id) => gone.push(id),
                ItemChange::ReadFlag { id, read: r } => read.push((id, r)),
            }
        }
        if !gone.is_empty() {
            send(events, cancel, account, EwsEvent::Gone { ids: gone });
        }
        if !read.is_empty() {
            send(events, cancel, account, EwsEvent::ReadFlags { list: read });
        }
        if !updated.is_empty() {
            send(events, cancel, account, EwsEvent::Items { list: updated });
        }
        // Batched by size; a message too large to download stays on the server (reported), so the folder's
        // sync state still moves on.
        for chunk in fresh.chunks(mailcraft_net::ews::MIME_BATCH) {
            if cancel.load(Ordering::SeqCst) {
                return Ok(());
            }
            let (items, skipped) = c.get_mime_sized(chunk)?;
            for id in &skipped {
                let size = chunk.iter().find(|(i, _)| i == id).and_then(|(_, n)| *n).unwrap_or(0);
                log::warn!("ews: a message of {size} bytes is too large to download");
                events.send(Event::FolderFailed {
                    account,
                    name: format!("A message of {} MB", size.div_ceil(1024 * 1024)),
                    error: "too large to download; it stays on the server".into(),
                });
            }
            let list: Vec<EItem> = items
                .into_iter()
                .map(|i| {
                    let shadow = item_shadow(&i, folder);
                    EItem { id: i.id, folder: folder.to_owned(), shadow, size: i.size, raw: i.mime }
                })
                .collect();
            if !list.is_empty() {
                send(events, cancel, account, EwsEvent::Items { list });
            }
        }
        state = page.state;
        send(
            events,
            cancel,
            account,
            EwsEvent::ItemState { folder: folder.to_owned(), state: state.clone() },
        );
        first = false;
        if page.last {
            break;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------
// Session side: applying results
// ---------------------------------------------------------------------------------------------------

fn folder_of(s: &Session, account: AccountId, id: &str) -> Option<FolderId> {
    s.mailbox
        .folders
        .values()
        .find(|f| f.account == account && f.kind == FolderKind::Mail && f.remote_name.as_deref() == Some(id))
        .map(|f| f.id)
}

fn item_of(s: &Session, account: AccountId, id: &str) -> Option<ItemId> {
    first_item(&s.mailbox, &account_scope(&s.mailbox, account), |m| m.remote_id.as_deref() == Some(id))
}

/// Applies the server's state to an item (the server wins).
fn apply_shadow(m: &mut MailItem, sh: &EwsShadow, now: mailcraft_model::DateTime) {
    let mut flags = Vec::new();
    if sh.read {
        flags.push("\\Seen".to_owned());
    }
    if sh.flag == 2 {
        flags.push("\\Flagged".to_owned());
    }
    match sh.verb {
        Some(102 | 103) => flags.push("\\Answered".to_owned()),
        Some(104) => flags.push("$Forwarded".to_owned()),
        _ => {}
    }
    apply_server_flags(m, &flags, now);
    if sh.flag == 1 && !matches!(m.flag, FlagState::Complete { .. }) {
        m.flag = FlagState::Complete { completed: None };
    }
    if sh.verb.is_none() {
        m.response = ResponseState::None;
    }
    let mut mine = m.categories.clone();
    mine.sort();
    if mine != sh.categories {
        m.categories = sh.categories.clone();
    }
}

fn apply_folders(
    s: &mut Session,
    account: AccountId,
    mut list: Vec<EwsFolder>,
    roles: Vec<(String, String)>,
    root: String,
    state: String,
) {
    let parent_of = |f: &EwsFolder| f.parent.clone().filter(|p| *p != root && !p.is_empty());
    let by_id: BTreeMap<String, Option<String>> = list.iter().map(|f| (f.id.clone(), parent_of(f))).collect();
    let depth = |id: &str| {
        let mut n = 0usize;
        let mut cur = by_id.get(id).cloned().flatten();
        while let Some(p) = cur {
            n += 1;
            if n > by_id.len() {
                break;
            }
            cur = by_id.get(&p).cloned().flatten();
        }
        n
    };
    list.sort_by_cached_key(|f| (depth(&f.id), f.name.to_ascii_lowercase()));
    let role_of: BTreeMap<String, FolderRole> = roles
        .iter()
        .filter_map(|(n, id)| ROLES.iter().find(|(r, _)| r == n).map(|(_, role)| (id.clone(), *role)))
        .collect();
    let server_roles: BTreeSet<FolderRole> = role_of.values().copied().collect();
    let ids: BTreeSet<String> = list.iter().map(|f| f.id.clone()).collect();
    let known = prefs(s).ews.get(&account.0).map(|st| st.folders.clone()).unwrap_or_default();
    for f in &list {
        let display = local_folder_name(&f.name);
        let server_parent = parent_of(f);
        let parent = server_parent.as_deref().and_then(|p| folder_of(s, account, p));
        if let Some(fid) = folder_of(s, account, &f.id) {
            let changed_there =
                known.get(&f.id).is_none_or(|k| k.name != display || k.parent != server_parent);
            if let Some(local) = s.mailbox.folders.get_mut(&fid)
                && local.role.is_none()
                && changed_there
                && (local.name != display || local.parent != parent)
            {
                local.name = display;
                local.parent = parent;
            }
            continue;
        }
        let role = role_of.get(&f.id).copied().or_else(|| {
            server_parent
                .is_none()
                .then(|| mailcraft_net::imap::role_from_name(&f.name))
                .flatten()
                .filter(|r| *r != FolderRole::Inbox && *r != FolderRole::Outbox && !server_roles.contains(r))
        });
        if let Some(role) = role
            && let Some(fid) = s.mailbox.role_folder(account, role)
            && s.mailbox.folder(fid).is_some_and(|x| x.remote_name.is_none())
        {
            if let Some(x) = s.mailbox.folders.get_mut(&fid) {
                x.remote_name = Some(f.id.clone());
            }
            continue;
        }
        let adopt = s
            .mailbox
            .folders
            .values()
            .find(|x| {
                x.account == account
                    && x.parent == parent
                    && x.remote_name.is_none()
                    && x.role.is_none()
                    && x.kind == FolderKind::Mail
                    && x.name.eq_ignore_ascii_case(&display)
            })
            .map(|x| x.id);
        if let Some(fid) = adopt {
            if let Some(x) = s.mailbox.folders.get_mut(&fid) {
                x.remote_name = Some(f.id.clone());
            }
            continue;
        }
        let id = FolderId(s.mailbox.alloc_id());
        s.mailbox.folders.insert(
            id,
            Folder {
                id,
                account,
                parent,
                name: display,
                kind: FolderKind::Mail,
                remote_name: Some(f.id.clone()),
                ..Default::default()
            },
        );
        s.sync.track.folders_created.insert(id);
    }
    // Folders gone from the server.
    let gone: Vec<(FolderId, bool)> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.account == account && f.kind == FolderKind::Mail)
        .filter(|f| f.remote_name.as_ref().is_some_and(|n| !ids.contains(n)))
        .map(|f| (f.id, f.role.is_some()))
        .collect();
    let mut remove = Vec::new();
    for (fid, is_role) in gone {
        if let Some(name) = s.mailbox.folder(fid).and_then(|f| f.remote_name.clone()) {
            s.sync.track.mailboxes_gone.insert((account, name));
        }
        if is_role {
            if let Some(f) = s.mailbox.folders.get_mut(&fid) {
                f.remote_name = None;
            }
        } else {
            remove.push(fid);
        }
    }
    if !remove.is_empty() {
        let mut sub: BTreeSet<FolderId> = BTreeSet::new();
        for f in &remove {
            sub.extend(s.mailbox.subtree(*f));
        }
        let lost: Vec<String> = s
            .mailbox
            .mail
            .items()
            .filter(|m| sub.contains(&m.folder))
            .filter_map(|m| m.remote_id.clone())
            .collect();
        let items = remove_folders(&mut s.mailbox, &remove);
        let st = prefs(s).ews.entry(account.0).or_default();
        for e in &lost {
            st.shadow.remove(e);
        }
        s.sync.track.removed.extend(items);
        s.sync.track.folders_removed.extend(sub);
    }
    let folders: BTreeMap<String, JmapFolderShadow> = list
        .iter()
        .filter(|f| !role_of.contains_key(&f.id))
        .map(|f| (f.id.clone(), JmapFolderShadow { name: local_folder_name(&f.name), parent: parent_of(f) }))
        .collect();
    let st = prefs(s).ews.entry(account.0).or_default();
    st.hierarchy_state = state;
    st.folders = folders;
    st.item_states.retain(|f, _| ids.contains(f));
}

fn apply_items(s: &mut Session, account: AccountId, list: Vec<EItem>, new_inbox: &mut Vec<ItemId>) {
    let now = s.now();
    let inbox = s.mailbox.role_folder(account, FolderRole::Inbox);
    let mut by_remote: BTreeMap<String, ItemId> = BTreeMap::new();
    s.mailbox.mail.scan(&account_scope(&s.mailbox, account), &mut |m| {
        if let Some(r) = &m.remote_id {
            by_remote.entry(r.clone()).or_insert(m.id);
        }
        ControlFlow::Continue(())
    });
    for e in list {
        let Some(folder) = folder_of(s, account, &e.folder) else { continue };
        match by_remote.get(&e.id).copied() {
            Some(id) => {
                if let Some(m) = s.mailbox.mail.get_mut(id) {
                    if m.folder != folder {
                        m.folder = folder;
                    }
                    apply_shadow(m, &e.shadow, now);
                    s.sync.track.flags.insert(id);
                }
            }
            None => {
                let Some(raw) = &e.raw else { continue };
                let mut item = mailcraft_store::import::item_from_raw(raw, folder, now);
                let adopt = item.message_id.as_deref().and_then(|mid| local_copy(&s.mailbox, folder, mid));
                if let Some(local) = adopt {
                    if let Some(m) = s.mailbox.mail.get_mut(local) {
                        m.remote_id = Some(e.id.clone());
                    }
                } else {
                    item.remote_id = Some(e.id.clone());
                    apply_shadow(&mut item, &e.shadow, now);
                    if let Some(n) = e.size {
                        item.size = n;
                    }
                    if let Err(err) = s.store_source(&mut item, raw) {
                        log::error!(
                            "could not store a downloaded message ({err}); it is downloaded again next time"
                        );
                        continue;
                    }
                    let id = s.mailbox.add_mail(item);
                    by_remote.insert(e.id.clone(), id);
                    s.sync.track.created.insert(id);
                    if Some(folder) == inbox {
                        new_inbox.push(id);
                    }
                }
            }
        }
        prefs(s).ews.entry(account.0).or_default().shadow.insert(e.id, e.shadow);
    }
}

pub(crate) fn apply(s: &mut Session, account: AccountId, ev: EwsEvent, new_inbox: &mut Vec<ItemId>) {
    match ev {
        EwsEvent::Folders { list, roles, root, state } => apply_folders(s, account, list, roles, root, state),
        EwsEvent::Items { list } => apply_items(s, account, list, new_inbox),
        EwsEvent::Gone { ids } => {
            for id in &ids {
                if let Some(item) = item_of(s, account, id) {
                    s.mailbox.mail.remove(item);
                    s.sync.track.removed.insert(item);
                }
            }
            let st = prefs(s).ews.entry(account.0).or_default();
            for id in ids {
                st.shadow.remove(&id);
            }
        }
        EwsEvent::ReadFlags { list } => {
            for (id, read) in list {
                if let Some(item) = item_of(s, account, &id) {
                    s.mailbox.mail.update(item, |m| m.read = read);
                    s.sync.track.flags.insert(item);
                }
                if let Some(sh) = prefs(s).ews.entry(account.0).or_default().shadow.get_mut(&id) {
                    sh.read = read;
                }
            }
        }
        EwsEvent::ItemState { folder, state } => {
            prefs(s).ews.entry(account.0).or_default().item_states.insert(folder, state);
        }
        EwsEvent::Reset { folder } => {
            prefs(s).ews.entry(account.0).or_default().item_states.remove(&folder);
        }
        EwsEvent::Updated { id, shadow } => {
            prefs(s).ews.entry(account.0).or_default().shadow.insert(id, shadow);
        }
        EwsEvent::Moved { old, new, shadow } => {
            if let Some(item) = item_of(s, account, &old) {
                let n = new.clone();
                s.mailbox.mail.update(item, |m| m.remote_id = Some(n));
            }
            let st = prefs(s).ews.entry(account.0).or_default();
            st.shadow.remove(&old);
            st.shadow.insert(new, shadow);
        }
        EwsEvent::Destroyed { ids } => {
            let st = prefs(s).ews.entry(account.0).or_default();
            for id in ids {
                st.shadow.remove(&id);
            }
        }
        EwsEvent::Uploaded { item, id, shadow } => {
            if let Some(m) = s.mailbox.mail.get_mut(item) {
                m.remote_id = Some(id.clone());
                prefs(s).ews.entry(account.0).or_default().shadow.insert(id, shadow);
            }
        }
        EwsEvent::FolderCreated { folder, id, shadow } => {
            let is_role = s.mailbox.folder(folder).is_some_and(|f| f.role.is_some());
            if let Some(f) = s.mailbox.folders.get_mut(&folder)
                && f.account == account
                && f.remote_name.is_none()
            {
                f.remote_name = Some(id.clone());
            }
            if !is_role {
                prefs(s).ews.entry(account.0).or_default().folders.insert(id, shadow);
            }
        }
        EwsEvent::FolderUpdated { id, shadow } => {
            prefs(s).ews.entry(account.0).or_default().folders.insert(id, shadow);
        }
        EwsEvent::FoldersDestroyed { ids } => {
            let st = prefs(s).ews.entry(account.0).or_default();
            for id in &ids {
                st.folders.remove(id);
                st.item_states.remove(id);
            }
            s.sync.track.mailboxes_gone.extend(ids.into_iter().map(|id| (account, id)));
        }
    }
}

/// After a command: true when the change record changed mail of the Exchange account `account` in a way the
/// server should learn (what JMAP accounts look at, plus categories).
pub(crate) fn changed(before: &Mailbox, mail: &MailChanges, after: &Mailbox, account: AccountId) -> bool {
    if super::jmap::changed(before, mail, after, account) {
        return true;
    }
    let in_acct = |mb: &Mailbox, m: &MailItem| mb.folder(m.folder).is_some_and(|f| f.account == account);
    mail.iter().any(|(id, b)| match (b, after.mail.get(id)) {
        (Some(b), Some(a)) => in_acct(before, b) && a.categories != b.categories,
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_shadows_and_urls() {
        assert_eq!(parse_time("2026-10-29T19:06:13.476920900Z"), Some(1_793_300_773));
        assert_eq!(parse_time("2026-10-29T15:06:13-04:00"), Some(1_793_300_773));
        assert_eq!(parse_time("2026-10-29T19:06:13"), Some(1_793_300_773));
        assert_eq!(parse_time("2026-11-03Z"), parse_time("2026-11-03T00:00:00Z"));
        assert_eq!(parse_time("junk"), None);
        assert_eq!(ews_url("mail.example.com"), "https://mail.example.com/EWS/Exchange.asmx");
        assert_eq!(
            ews_url(" https://x.example.com/EWS/Exchange.asmx "),
            "https://x.example.com/EWS/Exchange.asmx"
        );
        let mut m = MailItem { read: true, categories: vec!["b".into(), "a".into()], ..Default::default() };
        m.flag = FlagState::Flagged { label: String::new(), start: None, due: None, reminder: None };
        let sh = shadow_of(&m, "F1");
        assert_eq!(
            (sh.read, sh.flag, sh.categories.clone(), sh.verb),
            (true, 2, vec!["a".to_owned(), "b".to_owned()], None)
        );
        let mut n = MailItem::default();
        apply_shadow(&mut n, &sh, mailcraft_model::DateTime::default());
        assert!(n.read && n.is_flagged());
        assert_eq!(n.categories, vec!["a".to_owned(), "b".to_owned()]);
        apply_shadow(
            &mut n,
            &EwsShadow { flag: 1, ..Default::default() },
            mailcraft_model::DateTime::default(),
        );
        assert!(!n.read && matches!(n.flag, FlagState::Complete { .. }) && n.categories.is_empty());
    }
}
