//! IMAP account synchronisation: folder list with roles, incremental per-folder sync (new messages, flag
//! changes, vanished UIDs) using CONDSTORE/QRESYNC (RFC 7162) when available and full flag scans
//! otherwise, plus the write-back operations (flags, move, copy, append, expunge, folder management) and
//! change notification through IDLE.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mailcraft_model::{FolderRole, Security, ServerSettings};
use serde::{Deserialize, Serialize};

use crate::error::{NetError, Result};
use crate::imap::client::{CopyUid, IdleEvent, ImapClient, MailboxInfo, StoreMode};
use crate::imap::parse::Untagged;
use crate::imap::seqset::SeqSet;
use crate::imap::{role_from_attributes, role_from_name};
use crate::sasl::Credentials;
use crate::transport::ConnectOptions;

/// Everything needed to reach and log in to an IMAP account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImapSettings {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub credentials: Credentials,
    /// SASL mechanism or "LOGIN"; empty chooses automatically.
    pub auth_mechanism: String,
    pub options: ConnectOptions,
    /// Messages per UID FETCH of bodies.
    pub fetch_batch: usize,
    /// Most new messages one [`ImapAccount::sync_folder`] call downloads (oldest first); the rest are
    /// reported through [`SyncDelta::more`].
    pub max_new_per_sync: usize,
    /// The first download of a folder only takes messages that arrived on or after this day (UTC year,
    /// month, day; IMAP `SEARCH SINCE` on the internal date). `None` downloads everything.
    pub first_sync_since: Option<(i64, u8, u8)>,
}

impl ImapSettings {
    pub fn new(host: impl Into<String>, port: u16, security: Security, credentials: Credentials) -> Self {
        ImapSettings {
            host: host.into(),
            port,
            security,
            credentials,
            auth_mechanism: String::new(),
            options: ConnectOptions::default(),
            fetch_batch: 50,
            max_new_per_sync: 500,
            first_sync_since: None,
        }
    }

    /// From the model's account server settings plus a secret.
    pub fn from_server(s: &ServerSettings, credentials: Credentials) -> Self {
        ImapSettings {
            auth_mechanism: s.auth.clone(),
            ..ImapSettings::new(s.host.clone(), s.port, s.security, credentials)
        }
    }
}

/// A folder on the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFolder {
    /// Full decoded path ("Archive/2024").
    pub name: String,
    /// Last path segment.
    pub display_name: String,
    pub parent: Option<String>,
    pub delimiter: Option<char>,
    pub role: Option<FolderRole>,
    /// False for `\Noselect` / `\NonExistent` containers.
    pub selectable: bool,
    pub subscribed: bool,
    pub attributes: Vec<String>,
}

/// Which folders a folder list covers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ListScope {
    /// The user's own mailboxes (the personal namespace).
    #[default]
    Personal,
    /// One other user's or shared mailbox: `root` and everything below it. The root itself (or its
    /// `INBOX` child) is the store's Inbox; its children are top-level folders of the store.
    Store { root: String, delimiter: Option<char> },
}

/// What kind of namespace a shared mailbox is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SharedKind {
    /// Another user's mailbox the user may open (RFC 2342 "Other Users" namespace).
    OtherUser,
    /// A mailbox shared by several users (RFC 2342 "Shared" namespace).
    Shared,
}

/// A mailbox in another user's or the shared namespace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedRoot {
    /// Full server name of the mailbox ("Shared/support").
    pub root: String,
    /// The name below the namespace prefix ("support").
    pub name: String,
    pub delimiter: Option<char>,
    pub kind: SharedKind,
}

/// Most folders listed for one shared mailbox.
const MAX_STORE_FOLDERS: usize = 5_000;
/// Most shared mailboxes reported.
const MAX_SHARED_ROOTS: usize = 500;

/// Folder list entries with their roles. `root` (store root and delimiter) maps a shared mailbox: its root or
/// `INBOX` child is the Inbox and the root's children are top-level folders.
fn remote_folders(
    entries: Vec<crate::imap::ListEntry>,
    subscribed: &HashSet<String>,
    special_use: bool,
    root: Option<&(String, char)>,
) -> Vec<RemoteFolder> {
    let mut out: Vec<RemoteFolder> = Vec::with_capacity(entries.len());
    let mut taken: BTreeSet<FolderRole> = BTreeSet::new();
    let store_inbox = root.map(|(r, d)| {
        let child = format!("{r}{d}INBOX");
        if entries.iter().any(|e| e.name.eq_ignore_ascii_case(&child)) { child } else { r.clone() }
    });
    // Within a store the name `INBOX` means nothing special: only the store's own Inbox is one.
    let attr_role =
        |name: &str, attrs: &[String]| role_from_attributes(if root.is_some() { "" } else { name }, attrs);
    for e in &entries {
        if let Some(r) = attr_role(&e.name, &e.attributes) {
            taken.insert(r);
        }
    }
    for e in entries {
        let is_inbox = match &store_inbox {
            Some(i) => e.name.eq_ignore_ascii_case(i),
            None => e.name.eq_ignore_ascii_case("INBOX"),
        };
        let (mut parent, leaf) = match e.delimiter.and_then(|d| e.name.rsplit_once(d)) {
            Some((p, l)) if !p.is_empty() => (Some(p.to_owned()), l.to_owned()),
            _ => (None, e.name.clone()),
        };
        if let Some((r, _)) = root {
            if e.name == *r && !is_inbox {
                // The root is only a container: its children are the store's top-level folders.
                continue;
            }
            if parent.as_deref() == Some(r.as_str()) || is_inbox {
                parent = None;
            }
        }
        let mut role = if is_inbox { Some(FolderRole::Inbox) } else { attr_role(&e.name, &e.attributes) };
        let top_or_inbox_child = parent.as_deref().is_none_or(|p| {
            p.eq_ignore_ascii_case("INBOX")
                || store_inbox.as_deref().is_some_and(|i| i.eq_ignore_ascii_case(p))
        });
        if role.is_none() && !special_use && top_or_inbox_child {
            role = role_from_name(&leaf)
                .filter(|r| !taken.contains(r) && (root.is_none() || *r != FolderRole::Inbox));
            if let Some(r) = role {
                taken.insert(r);
            }
        }
        let selectable = !e.has_attribute("\\Noselect") && !e.has_attribute("\\NonExistent");
        out.push(RemoteFolder {
            subscribed: is_inbox || root.is_some() || subscribed.contains(&e.name),
            display_name: if is_inbox { "Inbox".into() } else { leaf },
            name: e.name,
            parent,
            delimiter: e.delimiter,
            role,
            selectable,
            attributes: e.attributes,
        });
    }
    out
}

/// What the client remembers about a folder between syncs. Persist it with the local copy.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderState {
    pub uidvalidity: u32,
    pub uidnext: u32,
    /// 0 when the server has no CONDSTORE.
    pub highest_modseq: u64,
    /// UIDs held locally, ascending.
    pub uids: Vec<u32>,
}

/// A message downloaded by a sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMessage {
    pub uid: u32,
    pub raw: Vec<u8>,
    pub flags: Vec<String>,
    pub internal_date: Option<String>,
    pub size: Option<u64>,
    /// Only the header was downloaded (`raw` is the header section); see
    /// [`ImapAccount::sync_folder_headers`] and [`ImapAccount::fetch_messages`].
    pub headers_only: bool,
}

/// The result of syncing one folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncDelta {
    /// UIDVALIDITY changed: discard every local message of this folder before applying `new`.
    pub reset: bool,
    pub new: Vec<NewMessage>,
    /// Current flags of known messages. With CONDSTORE these are only the changed ones; without it every
    /// known message is listed and the caller compares.
    pub changed: Vec<(u32, Vec<String>)>,
    /// Known UIDs that no longer exist on the server.
    pub vanished: Vec<u32>,
    /// More new messages are waiting (see [`ImapSettings::max_new_per_sync`]).
    pub more: bool,
    /// New messages the server had for this folder when the sync started, including those left for later
    /// calls by [`ImapSettings::max_new_per_sync`] (so `waiting - new.len()` are still to come).
    pub waiting: usize,
    /// State to pass to the next sync.
    pub state: FolderState,
}

/// A mailbox's storage quota as the server reports it (IMAP QUOTA, RFC 9208; JMAP Quota, RFC 9425).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    /// Bytes in use.
    pub used: u64,
    /// Bytes allowed (0 = no storage limit reported).
    pub limit: u64,
    /// Messages in use and allowed, when the server limits those too.
    pub messages: Option<(u64, u64)>,
}

impl Quota {
    /// From IMAP quota roots: the tightest STORAGE limit (in units of 1024 octets) and MESSAGE limit.
    /// `None` when no root limits storage or messages.
    pub fn from_imap(roots: &[(String, Vec<crate::imap::QuotaResource>)]) -> Option<Quota> {
        let mut storage: Option<(u64, u64)> = None;
        let mut messages: Option<(u64, u64)> = None;
        for r in roots.iter().flat_map(|(_, r)| r) {
            let pair = (r.usage, r.limit);
            // The tightest limit is the one the user runs into first (largest share used).
            let tighter = |cur: Option<(u64, u64)>| match cur {
                Some((u, l)) if l > 0 && pair.1 > 0 => {
                    u128::from(pair.0) * u128::from(l) > u128::from(u) * u128::from(pair.1)
                }
                _ => true,
            };
            match r.name.as_str() {
                "STORAGE" if tighter(storage) => storage = Some(pair),
                "MESSAGE" if tighter(messages) => messages = Some(pair),
                _ => {}
            }
        }
        if storage.is_none() && messages.is_none() {
            return None;
        }
        let (used, limit) = storage.unwrap_or((0, 0));
        Some(Quota { used: used.saturating_mul(1024), limit: limit.saturating_mul(1024), messages })
    }

    /// Share of the storage limit in use (0.0–1.0; `None` without a storage limit).
    pub fn fraction(&self) -> Option<f32> {
        if self.limit == 0 {
            return None;
        }
        let f = self.used as f64 / self.limit as f64;
        Some(f.clamp(0.0, 1.0) as f32)
    }
}

/// How far a folder sync has got: reported once the new messages are counted and after every batch of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FetchProgress {
    /// New messages downloaded so far by this call.
    pub done: usize,
    /// New messages this call downloads (at most [`ImapSettings::max_new_per_sync`]).
    pub batch: usize,
    /// New messages on the server, including those left for later calls (see [`SyncDelta::waiting`]).
    pub waiting: usize,
}

/// One IMAP account, connecting lazily and reconnecting after connection loss.
#[derive(Debug)]
pub struct ImapAccount {
    settings: ImapSettings,
    client: Option<ImapClient>,
    /// UIDVALIDITY and UIDNEXT of each folder when it was last synced (as the sync's SELECT saw them), until
    /// [`Self::idle_once`] reports a change in it. Mail that arrives after the sync but before IDLE starts
    /// is taken in by the SELECT (or an untagged EXISTS) on the way, so IDLE alone would never report it.
    synced: HashMap<String, (u32, u32)>,
}

/// Most folders whose sync state [`ImapAccount`] remembers for IDLE (a guard against hostile folder lists).
const MAX_SYNCED_FOLDERS: usize = 4096;

const BODY_ITEMS: &str = "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[])";
const HEADER_ITEMS: &str = "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER])";

impl ImapAccount {
    pub fn new(settings: ImapSettings) -> Self {
        ImapAccount { settings, client: None, synced: HashMap::new() }
    }

    pub fn settings(&self) -> &ImapSettings {
        &self.settings
    }

    /// Connects and logs in if not connected; enables QRESYNC/CONDSTORE and UTF8=ACCEPT when offered.
    pub fn connect(&mut self) -> Result<&mut ImapClient> {
        if self.client.as_ref().is_some_and(|c| !c.is_usable()) {
            self.client = None;
        }
        if self.client.is_none() {
            let s = &self.settings;
            let mut c = ImapClient::connect(&s.host, s.port, s.security, &s.options)?;
            c.login_with(&s.credentials, &s.auth_mechanism)?;
            let mut want = Vec::new();
            if c.has_capability("QRESYNC") {
                want.push("QRESYNC");
            } else if c.has_capability("CONDSTORE") {
                want.push("CONDSTORE");
            }
            if c.has_capability("UTF8=ACCEPT") {
                want.push("UTF8=ACCEPT");
            }
            if !want.is_empty() {
                c.enable(&want)?;
            }
            self.client = Some(c);
        }
        self.client.as_mut().ok_or(NetError::Closed)
    }

    /// Logs out and drops the connection.
    pub fn disconnect(&mut self) {
        if let Some(mut c) = self.client.take() {
            let _ = c.logout();
        }
    }

    /// The live client, if connected.
    pub fn client(&mut self) -> Option<&mut ImapClient> {
        self.client.as_mut()
    }

    /// Runs `f` with a connected client. Connection loss drops the client; idempotent operations are
    /// retried once on a fresh connection.
    fn with_client<T>(
        &mut self,
        idempotent: bool,
        mut f: impl FnMut(&mut ImapClient) -> Result<T>,
    ) -> Result<T> {
        let mut attempt = 0;
        loop {
            let res = self.connect().and_then(&mut f);
            match res {
                Err(e) if e.is_connection_lost() || self.client.as_ref().is_some_and(|c| !c.is_usable()) => {
                    self.client = None;
                    attempt += 1;
                    if !idempotent || attempt > 1 {
                        return Err(e);
                    }
                    log::info!("imap: connection lost ({e}); reconnecting");
                }
                other => return other,
            }
        }
    }

    /// The quota of the Inbox's quota roots (`Ok(None)` when the server has no QUOTA extension or sets no
    /// limits).
    pub fn quota(&mut self) -> Result<Option<Quota>> {
        self.with_client(true, |c| c.get_quota_root("INBOX")).map(|roots| Quota::from_imap(&roots))
    }

    /// Lists every folder of the personal namespace with its role and subscription state (other users' and
    /// shared mailboxes, RFC 2342, are left out: they are opened as stores of their own).
    pub fn sync_folder_list(&mut self) -> Result<Vec<RemoteFolder>> {
        self.sync_folder_list_in(&ListScope::Personal)
    }

    /// Lists the folders of `scope`: the personal namespace, or one shared mailbox (see [`SharedRoot`]).
    pub fn sync_folder_list_in(&mut self, scope: &ListScope) -> Result<Vec<RemoteFolder>> {
        self.with_client(true, |c| {
            let (entries, root) = match scope {
                ListScope::Personal => {
                    let mut entries = c.list("", "*")?;
                    let ns = c.namespace()?;
                    let foreign: Vec<String> =
                        ns.foreign_prefixes().iter().map(|n| n.prefix.clone()).collect();
                    entries.retain(|e| {
                        !foreign.iter().any(|p| {
                            e.name.starts_with(p.as_str())
                                || e.delimiter.is_some_and(|d| p.strip_suffix(d) == Some(e.name.as_str()))
                        })
                    });
                    (entries, None)
                }
                ListScope::Store { root, delimiter } => {
                    let mut entries = c.list("", root)?;
                    let d = delimiter.or_else(|| entries.first().and_then(|e| e.delimiter)).unwrap_or('/');
                    let mut below = c.list("", &format!("{root}{d}*"))?;
                    below.retain(|e| e.name.len() > root.len());
                    entries.extend(below);
                    entries.truncate(MAX_STORE_FOLDERS);
                    (entries, Some((root.clone(), d)))
                }
            };
            let subscribed: HashSet<String> = match (&root, c.lsub("", "*")) {
                (Some(_), _) => HashSet::new(),
                (None, Ok(l)) => l.into_iter().map(|e| e.name).collect(),
                (None, Err(e)) if e.is_connection_lost() => return Err(e),
                (None, Err(_)) => HashSet::new(),
            };
            let special_use = c.has_capability("SPECIAL-USE") || c.has_capability("XLIST");
            Ok(remote_folders(entries, &subscribed, special_use, root.as_ref()))
        })
    }

    /// Other users' mailboxes and shared mailboxes this login can open (RFC 2342 namespaces): the names one
    /// level below each other-users and shared namespace prefix. Empty when the server has no such
    /// namespaces.
    pub fn shared_roots(&mut self) -> Result<Vec<SharedRoot>> {
        self.with_client(true, |c| {
            let ns = c.namespace()?;
            let mut out: Vec<SharedRoot> = Vec::new();
            let groups = [(SharedKind::OtherUser, &ns.other_users), (SharedKind::Shared, &ns.shared)];
            for (kind, list) in groups {
                for n in list.iter().filter(|n| !n.prefix.is_empty()) {
                    let mut entries = c.list("", &format!("{}%", n.prefix))?;
                    entries.sort_by(|a, b| a.name.cmp(&b.name));
                    for e in entries {
                        let Some(rest) = e.name.strip_prefix(n.prefix.as_str()) else { continue };
                        let name = rest.trim_matches(|ch| Some(ch) == n.delimiter).to_owned();
                        if name.is_empty() || out.iter().any(|r| r.root == e.name) {
                            continue;
                        }
                        if out.len() >= MAX_SHARED_ROOTS {
                            return Ok(out);
                        }
                        out.push(SharedRoot {
                            root: e.name.clone(),
                            name,
                            delimiter: e.delimiter.or(n.delimiter),
                            kind,
                        });
                    }
                }
            }
            Ok(out)
        })
    }

    /// Selects `name` unless it is already selected (`fresh` forces a new SELECT for current counters).
    fn select(c: &mut ImapClient, name: &str, fresh: bool) -> Result<MailboxInfo> {
        if !fresh
            && let Some(s) = c.selected()
            && s.name == name
            && !s.read_only
        {
            return Ok(s.clone());
        }
        c.select(name)
    }

    /// Brings a folder up to date relative to `since` (use `FolderState::default()` the first time).
    pub fn sync_folder(&mut self, name: &str, since: &FolderState) -> Result<SyncDelta> {
        self.sync_folder_with_progress(name, since, false, &mut |_| {})
    }

    /// [`Self::sync_folder`], but new messages come with their header only (`BODY.PEEK[HEADER]`, marked
    /// [`NewMessage::headers_only`]); download the rest later with [`Self::fetch_messages`].
    pub fn sync_folder_headers(&mut self, name: &str, since: &FolderState) -> Result<SyncDelta> {
        self.sync_folder_with_progress(name, since, true, &mut |_| {})
    }

    /// [`Self::sync_folder`] (or [`Self::sync_folder_headers`] when `headers_only`), calling `progress` once
    /// the new messages are counted (the count comes with the search that finds them: no extra round trip)
    /// and after every batch of [`ImapSettings::fetch_batch`] messages. After a lost connection the sync
    /// starts over, and so does the count.
    pub fn sync_folder_with_progress(
        &mut self,
        name: &str,
        since: &FolderState,
        headers_only: bool,
        progress: &mut dyn FnMut(FetchProgress),
    ) -> Result<SyncDelta> {
        let batch = self.settings.fetch_batch.max(1);
        let cap = self.settings.max_new_per_sync.max(1);
        let window = self.settings.first_sync_since;
        let delta = self.with_client(true, |c| {
            sync_once(c, name, since, Limits { batch, cap, headers_only, window }, &mut *progress)
        })?;
        if self.synced.len() >= MAX_SYNCED_FOLDERS && !self.synced.contains_key(name) {
            self.synced.clear();
        }
        self.synced.insert(name.to_owned(), (delta.state.uidvalidity, delta.state.uidnext));
        Ok(delta)
    }

    /// Downloads whole messages by UID (for messages first downloaded as headers). UIDs no longer on the
    /// server are simply missing from the result.
    pub fn fetch_messages(&mut self, folder: &str, uids: &[u32]) -> Result<Vec<NewMessage>> {
        let batch = self.settings.fetch_batch.max(1);
        let mut want: Vec<u32> = uids.iter().copied().filter(|u| *u > 0).collect();
        want.sort_unstable();
        want.dedup();
        self.with_client(true, |c| {
            Self::select(c, folder, false)?;
            let mut out = Vec::new();
            for chunk in SeqSet::chunks(&want, batch) {
                for f in c.uid_fetch(&chunk, BODY_ITEMS)? {
                    let (Some(uid), Some(raw)) = (f.uid, f.body) else { continue };
                    if !chunk.contains(uid) {
                        continue;
                    }
                    out.push(NewMessage {
                        uid,
                        raw,
                        flags: f.flags.unwrap_or_default(),
                        internal_date: f.internal_date,
                        size: f.size,
                        headers_only: false,
                    });
                }
            }
            out.sort_by_key(|m| m.uid);
            out.dedup_by_key(|m| m.uid);
            Ok(out)
        })
    }

    /// Searches folder `folder` on the server (`UID SEARCH`, RFC 3501/9051 section 6.4.4) and downloads the
    /// headers of the newest matches that are not in `skip` (messages already held locally), at most `max`
    /// of them. Returns those headers (marked [`NewMessage::headers_only`]) and how many matches the server
    /// had beyond `skip`. `uidvalidity`: the folder's UIDVALIDITY the caller knows; when the server reports
    /// another one the UIDs mean other messages and nothing is returned. `up_to`: the highest UID the
    /// folder's own sync has covered. Matches above it are left out: they are new mail the next sync
    /// downloads, and filing one here would make that sync skip the unmatched messages below it. A folder not
    /// synced yet (`uidvalidity` or `up_to` 0) is not searched.
    pub fn search_folder(
        &mut self,
        folder: &str,
        criteria: &crate::search::Criterion,
        skip: &[u32],
        uidvalidity: u32,
        up_to: u32,
        max: usize,
    ) -> Result<(Vec<NewMessage>, usize)> {
        criteria.check()?;
        if uidvalidity == 0 || up_to == 0 {
            return Ok((Vec::new(), 0));
        }
        let batch = self.settings.fetch_batch.max(1);
        let skip: HashSet<u32> = skip.iter().copied().collect();
        self.with_client(true, |c| {
            let info = Self::select(c, folder, false)?;
            if info.uidvalidity != uidvalidity {
                return Ok((Vec::new(), 0));
            }
            if info.exists == 0 {
                return Ok((Vec::new(), 0));
            }
            let cmd = criteria.imap_command(c.utf8())?;
            let mut found: Vec<u32> = c
                .uid_search_command(cmd)?
                .into_iter()
                .filter(|u| *u > 0 && *u <= up_to && !skip.contains(u))
                .collect();
            found.sort_unstable();
            found.dedup();
            let total = found.len();
            // The newest matches (UIDs grow with arrival).
            let keep: Vec<u32> = found.iter().rev().take(max).rev().copied().collect();
            let mut out = Vec::new();
            for chunk in SeqSet::chunks(&keep, batch) {
                for f in c.uid_fetch(&chunk, HEADER_ITEMS)? {
                    let (Some(uid), Some(raw)) = (f.uid, f.header) else { continue };
                    if !chunk.contains(uid) {
                        continue;
                    }
                    out.push(NewMessage {
                        uid,
                        raw,
                        flags: f.flags.unwrap_or_default(),
                        internal_date: f.internal_date,
                        size: f.size,
                        headers_only: true,
                    });
                }
            }
            out.sort_by_key(|m| m.uid);
            out.dedup_by_key(|m| m.uid);
            Ok((out, total))
        })
    }

    /// Adds and removes flags on messages.
    pub fn push_flags(&mut self, folder: &str, uids: &[u32], add: &[&str], remove: &[&str]) -> Result<()> {
        let set = SeqSet::from_ids(uids);
        if set.is_empty() {
            return Ok(());
        }
        self.with_client(true, |c| {
            Self::select(c, folder, false)?;
            if !add.is_empty() {
                c.uid_store(&set, StoreMode::Add, add, None)?;
            }
            if !remove.is_empty() {
                c.uid_store(&set, StoreMode::Remove, remove, None)?;
            }
            Ok(())
        })
    }

    /// Moves messages to `dest`; returns the new UIDs when the server reports them (UIDPLUS).
    pub fn move_messages(&mut self, folder: &str, uids: &[u32], dest: &str) -> Result<Option<CopyUid>> {
        let set = SeqSet::from_ids(uids);
        self.with_client(false, |c| {
            Self::select(c, folder, false)?;
            c.uid_move(&set, dest)
        })
    }

    /// Copies messages to `dest`.
    pub fn copy_messages(&mut self, folder: &str, uids: &[u32], dest: &str) -> Result<Option<CopyUid>> {
        let set = SeqSet::from_ids(uids);
        self.with_client(false, |c| {
            Self::select(c, folder, false)?;
            c.uid_copy(&set, dest)
        })
    }

    /// Appends a message (a sent copy, a draft); returns its UID when the server reports it (UIDPLUS).
    pub fn append(&mut self, folder: &str, raw: &[u8], flags: &[&str]) -> Result<Option<u32>> {
        self.with_client(false, |c| Ok(c.append(folder, flags, None, raw)?.map(|(_, uid)| uid)))
    }

    /// Permanently removes messages: marks them `\Deleted` and expunges just those (UID EXPUNGE), or the
    /// whole folder's deleted messages when the server lacks UIDPLUS.
    pub fn expunge(&mut self, folder: &str, uids: &[u32]) -> Result<()> {
        let set = SeqSet::from_ids(uids);
        if set.is_empty() {
            return Ok(());
        }
        self.with_client(true, |c| {
            Self::select(c, folder, false)?;
            c.uid_store(&set, StoreMode::Add, &["\\Deleted"], None)?;
            if c.has_capability("UIDPLUS") || c.has_capability("IMAP4rev2") {
                c.uid_expunge(&set)?;
            } else {
                c.expunge()?;
            }
            Ok(())
        })
    }

    pub fn create_folder(&mut self, name: &str) -> Result<()> {
        self.with_client(false, |c| {
            c.create(name)?;
            let _ = c.subscribe(name);
            Ok(())
        })
    }

    pub fn rename_folder(&mut self, from: &str, to: &str) -> Result<()> {
        self.with_client(false, |c| {
            if c.selected().is_some_and(|s| s.name == from) {
                c.unselect()?;
            }
            c.rename(from, to)?;
            let _ = c.unsubscribe(from);
            let _ = c.subscribe(to);
            Ok(())
        })
    }

    pub fn delete_folder(&mut self, name: &str) -> Result<()> {
        self.with_client(false, |c| {
            if c.selected().is_some_and(|s| s.name == name) {
                c.unselect()?;
            }
            let _ = c.unsubscribe(name);
            c.delete(name)
        })
    }

    /// One IDLE (RFC 2177) on `folder` for up to `max_wait`, ended early when `stop` is raised. True when the
    /// server reported a change. Unlike [`Self::wait_for_changes`] there is no NOOP fallback: a server
    /// without IDLE gives [`NetError::Unsupported`], so the caller can poll on its own schedule. Connects
    /// (and reconnects once after a lost connection) as needed.
    ///
    /// When `folder` was synced through this account, mail that arrived since that sync counts as a change
    /// too, reported at once without waiting: the folder is selected afresh and its UIDNEXT compared with
    /// the one the sync saw. A change is reported once; after that only IDLE news counts until the folder's
    /// next sync.
    pub fn idle_once(&mut self, folder: &str, max_wait: Duration, stop: Option<&AtomicBool>) -> Result<bool> {
        let known = self.synced.get(folder).copied();
        let changed = self.with_client(true, |c| {
            if !c.has_capability("IDLE") && !c.has_capability("IMAP4rev2") {
                return Err(NetError::Unsupported("IDLE".into()));
            }
            let info = Self::select(c, folder, known.is_some())?;
            if let Some((uidvalidity, uidnext)) = known
                && (info.uidvalidity != uidvalidity || info.uidnext > uidnext)
            {
                return Ok(true);
            }
            Ok(matches!(c.idle(max_wait, stop)?, IdleEvent::Changed(_)))
        })?;
        if changed {
            self.synced.remove(folder);
        }
        Ok(changed)
    }

    /// Waits up to `max_wait` for changes in `folder` (IDLE, or NOOP polling every few seconds when the
    /// server lacks IDLE). Returns true when something changed; false on timeout or stop.
    pub fn wait_for_changes(
        &mut self,
        folder: &str,
        max_wait: Duration,
        stop: Option<&AtomicBool>,
    ) -> Result<bool> {
        self.with_client(true, |c| {
            Self::select(c, folder, false)?;
            if c.has_capability("IDLE") || c.has_capability("IMAP4rev2") {
                return Ok(matches!(c.idle(max_wait, stop)?, IdleEvent::Changed(_)));
            }
            let deadline = crate::transport::deadline_after(max_wait);
            loop {
                let changes = c.noop()?;
                if changes.iter().any(|u| {
                    matches!(
                        u,
                        Untagged::Exists(_)
                            | Untagged::Expunge(_)
                            | Untagged::Fetch(_)
                            | Untagged::Vanished { .. }
                    )
                }) {
                    return Ok(true);
                }
                let step_end = crate::transport::deadline_after(Duration::from_secs(5)).min(deadline);
                while Instant::now() < step_end {
                    if stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
                        return Ok(false);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                if Instant::now() >= deadline {
                    return Ok(false);
                }
            }
        })
    }
}

/// An IMAP `date` (RFC 3501 `date-text`, e.g. `5-Oct-2026`); `None` for an impossible date.
pub fn imap_date(year: i64, month: u8, day: u8) -> Option<String> {
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let name = MONTHS.get(usize::from(month).checked_sub(1)?)?;
    if !(1..=9999).contains(&year) || day == 0 || day > mailcraft_model::dates::days_in_month(year, month) {
        return None;
    }
    Some(format!("{day}-{name}-{year}"))
}

/// The UTC day `months` calendar months before the Unix time `now` (the day of month is clamped to the
/// target month's length): the start of a "download mail from the past N months" window.
pub fn months_before(now: i64, months: u32) -> (i64, u8, u8) {
    let (y, m, d) = mailcraft_model::dates::civil_from_days(now.div_euclid(86_400));
    let total = y.saturating_mul(12).saturating_add(i64::from(m) - 1).saturating_sub(i64::from(months));
    let year = total.div_euclid(12);
    let month = u8::try_from(total.rem_euclid(12) + 1).unwrap_or(1);
    let day = d.min(mailcraft_model::dates::days_in_month(year, month));
    (year, month, day)
}

/// How one [`sync_once`] downloads.
#[derive(Clone, Copy)]
struct Limits {
    batch: usize,
    cap: usize,
    headers_only: bool,
    window: Option<(i64, u8, u8)>,
}

fn sync_once(
    c: &mut ImapClient,
    name: &str,
    since: &FolderState,
    limits: Limits,
    progress: &mut dyn FnMut(FetchProgress),
) -> Result<SyncDelta> {
    let Limits { batch, cap, headers_only, window } = limits;
    let info = c.select(name)?;
    let mut delta = SyncDelta::default();
    let mut since = since.clone();
    if since.uidvalidity != 0 && since.uidvalidity != info.uidvalidity {
        delta.reset = true;
        since = FolderState::default();
    }
    let mut known: Vec<u32> = since.uids.clone();
    known.sort_unstable();
    known.dedup();
    let known_set: HashSet<u32> = known.iter().copied().collect();
    let max_known = known.last().copied().unwrap_or(0);
    let server_modseq = info.highest_modseq.unwrap_or(0);
    // The very first download of the folder may be limited to recent mail.
    let first = known.is_empty() && since.uidnext == 0;
    let since_key =
        window.filter(|_| first).and_then(|(y, m, d)| imap_date(y, m, d)).map(|d| format!(" SINCE {d}"));

    // New UIDs (UIDs only grow, so anything above the highest known one is new).
    let mut new_uids: Vec<u32> = if info.exists == 0 {
        Vec::new()
    } else {
        let from = max_known.saturating_add(1).max(1);
        let key = since_key.unwrap_or_default();
        c.uid_search(&format!("UID {}{key}", SeqSet::from(from)))?
            .into_iter()
            .filter(|&u| u > max_known)
            .collect()
    };
    new_uids.sort_unstable();
    new_uids.dedup();
    delta.waiting = new_uids.len();
    if new_uids.len() > cap {
        new_uids.truncate(cap);
        delta.more = true;
    }
    let mut counts = FetchProgress { done: 0, batch: new_uids.len(), waiting: delta.waiting };
    progress(counts);

    // Changes to known messages.
    let mut vanished: BTreeSet<u32> = BTreeSet::new();
    if !known.is_empty() {
        let range = SeqSet::range(1, max_known);
        let use_condstore = server_modseq > 0 && since.highest_modseq > 0;
        if use_condstore {
            if since.highest_modseq < server_modseq {
                let (fetches, gone) =
                    c.uid_fetch_changed_since(&range, "(UID FLAGS)", since.highest_modseq)?;
                for f in fetches {
                    if let (Some(uid), Some(flags)) = (f.uid, f.flags)
                        && known_set.contains(&uid)
                    {
                        delta.changed.push((uid, flags));
                    }
                }
                for set in gone {
                    vanished.extend(known.iter().copied().filter(|u| set.contains(*u)));
                }
            }
            if !c.is_enabled("QRESYNC") {
                // CONDSTORE alone does not report expunges: compare counts, search only on mismatch.
                let total_new =
                    if delta.more { u32::MAX } else { u32::try_from(new_uids.len()).unwrap_or(u32::MAX) };
                let expected = u32::try_from(known.len()).unwrap_or(u32::MAX).saturating_add(total_new);
                if info.exists != expected {
                    let present: HashSet<u32> = c.uid_search(&format!("UID {range}"))?.into_iter().collect();
                    vanished.extend(known.iter().copied().filter(|u| !present.contains(u)));
                }
            }
        } else {
            let fetches = c.uid_fetch(&range, "(UID FLAGS)")?;
            let mut present = HashSet::with_capacity(fetches.len());
            for f in fetches {
                if let Some(uid) = f.uid {
                    present.insert(uid);
                    if let Some(flags) = f.flags
                        && known_set.contains(&uid)
                    {
                        delta.changed.push((uid, flags));
                    }
                }
            }
            vanished.extend(known.iter().copied().filter(|u| !present.contains(u)));
        }
    }
    delta.changed.retain(|(u, _)| !vanished.contains(u));
    delta.changed.sort_by_key(|(u, _)| *u);
    delta.changed.dedup_by_key(|(u, _)| *u);

    // Download new messages.
    for ids in new_uids.chunks(batch) {
        let chunk = SeqSet::from_ids(ids);
        let items = if headers_only { HEADER_ITEMS } else { BODY_ITEMS };
        for f in c.uid_fetch(&chunk, items)? {
            let raw = if headers_only { f.header } else { f.body };
            let (Some(uid), Some(raw)) = (f.uid, raw) else { continue };
            if !chunk.contains(uid) || uid <= max_known {
                continue;
            }
            delta.new.push(NewMessage {
                uid,
                raw,
                flags: f.flags.unwrap_or_default(),
                internal_date: f.internal_date,
                size: f.size,
                headers_only,
            });
        }
        counts.done = counts.done.saturating_add(ids.len()).min(counts.batch);
        progress(counts);
    }
    delta.new.sort_by_key(|m| m.uid);
    delta.new.dedup_by_key(|m| m.uid);

    let mut uids: Vec<u32> = known.into_iter().filter(|u| !vanished.contains(u)).collect();
    uids.extend(delta.new.iter().map(|m| m.uid));
    uids.sort_unstable();
    uids.dedup();
    delta.vanished = vanished.into_iter().collect();
    delta.state = FolderState {
        uidvalidity: info.uidvalidity,
        uidnext: info.uidnext,
        highest_modseq: server_modseq,
        uids,
    };
    Ok(delta)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn date_window() {
        assert_eq!(imap_date(2026, 10, 5).as_deref(), Some("5-Oct-2026"));
        assert_eq!(imap_date(2026, 2, 29), None);
        assert_eq!(imap_date(2026, 13, 1), None);
        assert_eq!(imap_date(2026, 0, 1), None);
        // 2026-10-10 12:00 UTC.
        let now = 1_791_633_600;
        assert_eq!(months_before(now, 0), (2026, 10, 10));
        assert_eq!(months_before(now, 12), (2025, 10, 10));
        assert_eq!(months_before(now, 10), (2025, 12, 10));
        assert_eq!(months_before(now, 11), (2025, 11, 10));
        // 31 March minus one month is the end of February.
        let mar31 = 1_774_958_400; // 2026-03-31 12:00 UTC
        assert_eq!(months_before(mar31, 1), (2026, 2, 28));
        let (y, m, _) = months_before(0, u32::MAX);
        assert!(y < 0 && (1..=12).contains(&m) && imap_date(y, m, 1).is_none());
    }
}
