//! IMAP account synchronisation: folder list with roles, incremental per-folder sync (new messages, flag
//! changes, vanished UIDs) using CONDSTORE/QRESYNC (RFC 7162) when available and full flag scans
//! otherwise, plus the write-back operations (flags, move, copy, append, expunge, folder management) and
//! change notification through IDLE.

use std::collections::{BTreeSet, HashSet};
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
    /// State to pass to the next sync.
    pub state: FolderState,
}

/// One IMAP account, connecting lazily and reconnecting after connection loss.
#[derive(Debug)]
pub struct ImapAccount {
    settings: ImapSettings,
    client: Option<ImapClient>,
}

const BODY_ITEMS: &str = "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[])";
const HEADER_ITEMS: &str = "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER])";

impl ImapAccount {
    pub fn new(settings: ImapSettings) -> Self {
        ImapAccount { settings, client: None }
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

    /// Lists every folder with its role and subscription state.
    pub fn sync_folder_list(&mut self) -> Result<Vec<RemoteFolder>> {
        self.with_client(true, |c| {
            let entries = c.list("", "*")?;
            let subscribed: HashSet<String> = match c.lsub("", "*") {
                Ok(l) => l.into_iter().map(|e| e.name).collect(),
                Err(e) if e.is_connection_lost() => return Err(e),
                Err(_) => HashSet::new(),
            };
            let special_use = c.has_capability("SPECIAL-USE") || c.has_capability("XLIST");
            let mut out: Vec<RemoteFolder> = Vec::with_capacity(entries.len());
            let mut taken: BTreeSet<FolderRole> = BTreeSet::new();
            for e in &entries {
                if let Some(r) = role_from_attributes(&e.name, &e.attributes) {
                    taken.insert(r);
                }
            }
            for e in entries {
                let (parent, leaf) = match e.delimiter.and_then(|d| e.name.rsplit_once(d)) {
                    Some((p, l)) if !p.is_empty() => (Some(p.to_owned()), l.to_owned()),
                    _ => (None, e.name.clone()),
                };
                let mut role = role_from_attributes(&e.name, &e.attributes);
                let top_or_inbox_child = parent.as_deref().is_none_or(|p| p.eq_ignore_ascii_case("INBOX"));
                if role.is_none() && !special_use && top_or_inbox_child {
                    role = role_from_name(&leaf).filter(|r| !taken.contains(r));
                    if let Some(r) = role {
                        taken.insert(r);
                    }
                }
                let selectable = !e.has_attribute("\\Noselect") && !e.has_attribute("\\NonExistent");
                let is_inbox = e.name.eq_ignore_ascii_case("INBOX");
                out.push(RemoteFolder {
                    subscribed: is_inbox || subscribed.contains(&e.name),
                    display_name: if is_inbox { "Inbox".into() } else { leaf },
                    name: e.name,
                    parent,
                    delimiter: e.delimiter,
                    role,
                    selectable,
                    attributes: e.attributes,
                });
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
        let batch = self.settings.fetch_batch.max(1);
        let cap = self.settings.max_new_per_sync.max(1);
        self.with_client(true, |c| sync_once(c, name, since, batch, cap, false))
    }

    /// [`Self::sync_folder`], but new messages come with their header only (`BODY.PEEK[HEADER]`, marked
    /// [`NewMessage::headers_only`]); download the rest later with [`Self::fetch_messages`].
    pub fn sync_folder_headers(&mut self, name: &str, since: &FolderState) -> Result<SyncDelta> {
        let batch = self.settings.fetch_batch.max(1);
        let cap = self.settings.max_new_per_sync.max(1);
        self.with_client(true, |c| sync_once(c, name, since, batch, cap, true))
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
    pub fn idle_once(&mut self, folder: &str, max_wait: Duration, stop: Option<&AtomicBool>) -> Result<bool> {
        self.with_client(true, |c| {
            if !c.has_capability("IDLE") && !c.has_capability("IMAP4rev2") {
                return Err(NetError::Unsupported("IDLE".into()));
            }
            Self::select(c, folder, false)?;
            Ok(matches!(c.idle(max_wait, stop)?, IdleEvent::Changed(_)))
        })
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

fn sync_once(
    c: &mut ImapClient,
    name: &str,
    since: &FolderState,
    batch: usize,
    cap: usize,
    headers_only: bool,
) -> Result<SyncDelta> {
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

    // New UIDs (UIDs only grow, so anything above the highest known one is new).
    let mut new_uids: Vec<u32> = if info.exists == 0 {
        Vec::new()
    } else {
        let from = max_known.saturating_add(1).max(1);
        c.uid_search(&format!("UID {}", SeqSet::from(from)))?.into_iter().filter(|&u| u > max_known).collect()
    };
    new_uids.sort_unstable();
    if new_uids.len() > cap {
        new_uids.truncate(cap);
        delta.more = true;
    }

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
    for chunk in SeqSet::chunks(&new_uids, batch) {
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
