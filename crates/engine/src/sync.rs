//! Send/receive (`plan/outlook/ribbon-mail.md` Send / Receive, `reading-pane.md` sending, `folders.md`).
//!
//! **Threads.** Each server account gets a worker thread (native builds only) that owns its IMAP connection
//! (`mailcraft_net::sync::ImapAccount`) and opens SMTP sessions for sending. The session talks to workers
//! through channels: [`Session::send_receive`] hands a worker one job (queued server changes, Outbox
//! messages, folders to update), and [`Session::poll_sync`] — called by the UI every frame, or by
//! [`Session::wait_for_sync`] — applies what came back. Every change to the mailbox is made on the session's
//! thread and journaled like a command's changes; nothing on a worker touches the mailbox.
//!
//! **Server → local.** Folder lists map RFC 6154 SPECIAL-USE folders onto the account's default folders and
//! create the others; per-folder deltas (RFC 9051 UIDs, RFC 7162 CONDSTORE/QRESYNC when offered) add new
//! messages (`MailItem::from_message`, `remote_id` = UID), apply flag changes (the server wins) and remove
//! vanished messages. Rules run on new Inbox mail.
//!
//! **Local → server.** After every successful mutating command the session compares the mailbox before and
//! after (see [`queue_changes`]) and queues [`RemoteOp`]s on the account (persisted with the mailbox): flag
//! changes, moves, uploads, permanent deletes and folder create/rename/delete. Workers run them in order
//! before receiving; failures are retried with backoff and dropped after [`MAX_ATTEMPTS`] or when the
//! server refuses them outright.
//!
//! **Sending.** `compose.send` files the message in the account's Outbox (local only); a send job submits it
//! over SMTP and, on success, the session moves it to Sent Items, whose server copy is then uploaded by the
//! ordinary queue (unless the account says the server files sent mail itself). A failure leaves the message
//! in the Outbox with an error notice; the next send/receive tries again.
//!
//! **Undo.** Undo restores whole-mailbox snapshots; [`after_restore`] carries the synchronised state (queue,
//! folder states, mail that arrived or vanished meanwhile) into the restored mailbox and queues the server
//! changes that revert the undone command.

// The browser build has no worker threads: the code that applies their results is unused there.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{CmdError, Notice, Session};
use mailcraft_model::account::{PendingOp, RemoteOp};
use mailcraft_model::{
    AccountId, AccountKind, FlagState, Folder, FolderId, FolderKind, ItemId, MailItem, Mailbox, ResponseState,
};

/// Default automatic send/receive interval (Options › Advanced › Send/Receive: every 30 minutes).
pub const DEFAULT_AUTO_MINUTES: u32 = 30;
/// Failed attempts after which a queued server change is dropped.
pub const MAX_ATTEMPTS: u32 = 10;
/// First retry delay; doubles per failure up to [`BACKOFF_MAX_SECS`].
const BACKOFF_BASE_SECS: i64 = 30;
const BACKOFF_MAX_SECS: i64 = 3600;
/// Wait after a send/receive could not even start (no password, bad settings) before trying automatically.
const BLOCKED_RETRY_SECS: i64 = 300;
/// Most queued server changes kept per account (oldest dropped beyond it).
const MAX_PENDING: usize = 50_000;

// =======================================================================================================
// Status
// =======================================================================================================

/// Connection state shown at the right of the status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Connection {
    /// No server account, or nothing tried yet.
    #[default]
    Idle,
    Connecting,
    Connected,
    Offline,
    Disconnected,
}

impl Connection {
    /// Status bar text.
    pub fn label(self) -> &'static str {
        match self {
            Connection::Idle => "",
            Connection::Connecting => "Connecting…",
            Connection::Connected => "Connected",
            Connection::Offline => "Working offline",
            Connection::Disconnected => "Disconnected",
        }
    }
}

/// One account's send/receive state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub account: u32,
    pub name: String,
    pub busy: bool,
    /// What the worker is doing ("Updating Inbox…").
    pub progress: String,
    pub error: Option<String>,
    /// Unix time of the last send/receive that finished without error.
    pub last_sync: Option<i64>,
    pub pending_changes: usize,
    pub outbox: usize,
}

/// What the status bar and the Send/Receive Progress dialog show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// Left text: "Updating Inbox…", "All folders are up to date.", "Send/Receive error".
    pub text: String,
    pub connection: Connection,
    /// Right text: "Connected", "Working offline", "Disconnected".
    pub connection_text: String,
    pub busy: bool,
    pub accounts: Vec<AccountStatus>,
}

/// Result of Test Account Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestReport {
    pub account: u32,
    pub incoming_ok: bool,
    pub incoming: String,
    /// `None` when the account has no outgoing server.
    pub outgoing_ok: Option<bool>,
    pub outgoing: String,
}

impl TestReport {
    pub fn ok(&self) -> bool {
        self.incoming_ok && self.outgoing_ok != Some(false)
    }
    pub fn to_json(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            o.insert("ok".into(), json!(self.ok()));
        }
        v
    }
}

/// What one send/receive run does for an account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Send the Outbox.
    pub send: bool,
    /// Update folders (all, or only `folders`).
    pub receive: bool,
    pub folders: Option<Vec<FolderId>>,
    /// Started by the user (ignores retry backoff).
    pub user: bool,
}

impl Plan {
    pub fn all() -> Plan {
        Plan { send: true, receive: true, folders: None, user: true }
    }
    fn merge(&mut self, o: &Plan) {
        self.send |= o.send;
        self.user |= o.user;
        if o.receive {
            self.folders = match (self.receive, self.folders.take(), &o.folders) {
                (false, _, f) => f.clone(),
                (true, None, _) | (true, _, None) => None,
                (true, Some(mut a), Some(b)) => {
                    a.extend(b.iter().copied());
                    a.sort();
                    a.dedup();
                    Some(a)
                }
            };
            self.receive = true;
        }
    }
}

/// What synchronisation changed, so undo keeps it (ids only; cleared when the undo history is empty).
#[derive(Clone, Debug, Default)]
pub(crate) struct Track {
    created: BTreeSet<ItemId>,
    removed: BTreeSet<ItemId>,
    flags: BTreeSet<ItemId>,
    folders_created: BTreeSet<FolderId>,
    folders_removed: BTreeSet<FolderId>,
}

impl Track {
    fn is_empty(&self) -> bool {
        self.created.is_empty()
            && self.removed.is_empty()
            && self.flags.is_empty()
            && self.folders_created.is_empty()
            && self.folders_removed.is_empty()
    }
}

/// Per-account runtime state (not persisted).
#[derive(Clone, Debug, Default)]
struct AccountRun {
    /// The job in flight.
    busy: Option<InFlight>,
    /// Another run requested while busy.
    again: Option<Plan>,
    progress: String,
    error: Option<String>,
    last_sync: Option<i64>,
    /// Automatic runs wait until then after a run could not start.
    blocked_until: i64,
}

#[derive(Clone, Debug, Default)]
struct InFlight {
    ops: BTreeSet<u64>,
    sending: BTreeSet<ItemId>,
}

/// Queued server changes in flight, as (account, op id).
pub(crate) type Inflight = BTreeSet<(AccountId, u64)>;

/// Send/receive state of a session.
#[derive(Default)]
pub struct SyncState {
    pub status: SyncStatus,
    /// Last Test Account Settings result per account.
    pub tests: BTreeMap<AccountId, TestReport>,
    runs: BTreeMap<AccountId, AccountRun>,
    last_auto: Option<i64>,
    /// Something happened since the status was last computed.
    dirty: bool,
    pub(crate) track: Track,
    #[cfg(not(target_arch = "wasm32"))]
    rt: native::Runtime,
}

impl std::fmt::Debug for SyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncState").field("status", &self.status).finish()
    }
}

impl SyncState {
    /// Queued server changes handed to a worker and not reported back yet, as (account, op id): op ids are
    /// only unique within an account.
    fn inflight_ops(&self) -> Inflight {
        self.runs
            .iter()
            .filter_map(|(a, r)| Some((*a, r.busy.as_ref()?)))
            .flat_map(|(a, b)| b.ops.iter().map(move |op| (a, *op)))
            .collect()
    }
    fn busy(&self, id: AccountId) -> bool {
        self.runs.get(&id).is_some_and(|r| r.busy.is_some())
    }
}

// =======================================================================================================
// Session API
// =======================================================================================================

fn is_server_account(kind: AccountKind) -> bool {
    matches!(kind, AccountKind::Imap | AccountKind::Jmap)
}

impl Session {
    /// The send/receive status for the status bar and the progress dialog.
    pub fn sync_status(&self) -> &SyncStatus {
        &self.sync.status
    }

    /// True while any account's worker is busy.
    pub fn sync_busy(&self) -> bool {
        self.sync.runs.values().any(|r| r.busy.is_some())
    }

    /// Starts a send/receive run for `accounts` (server accounts only). Returns how many runs started (an
    /// account already busy runs again when it finishes). Errors that stop an account from starting (no
    /// password, JMAP not supported yet, offline) are reported in the status and, for user-started runs, as
    /// notices; the error of the last account is returned when nothing started.
    pub fn send_receive(&mut self, accounts: &[AccountId], plan: Plan) -> Result<usize, CmdError> {
        if self.view.work_offline {
            refresh_status(self);
            return Err(CmdError::Disabled("Working offline".into()));
        }
        let mut started = 0usize;
        let mut last_err = None;
        for id in accounts {
            match dispatch(self, *id, plan.clone()) {
                Ok(true) => started = started.saturating_add(1),
                Ok(false) => {}
                Err(e) => {
                    let name = self.mailbox.account(*id).map(|a| a.display_name.clone()).unwrap_or_default();
                    let until = self.now().unix.saturating_add(BLOCKED_RETRY_SECS);
                    let run = self.sync.runs.entry(*id).or_default();
                    run.error = Some(e.clone());
                    run.blocked_until = until;
                    if plan.user {
                        self.notify(Notice::Error(format!("{name}: {e}")));
                    }
                    last_err = Some(e);
                }
            }
        }
        refresh_status(self);
        match (started, last_err) {
            (0, Some(e)) if accounts.len() == 1 => Err(CmdError::Failed(e)),
            _ => Ok(started),
        }
    }

    /// Applies finished worker results (new folders and messages, flag changes, removed messages, sent
    /// messages, completed server changes), runs rules on new Inbox mail, queues the server changes rules
    /// made, starts automatic runs that are due, and journals everything. Returns true when the mailbox or
    /// the status changed. Never panics: a panic while applying results restores the mailbox.
    pub fn poll_sync(&mut self) -> bool {
        let status_before = self.sync.status.clone();
        let mailbox_changed = poll_events(self);
        tick(self);
        if self.undo.undo.is_empty() && self.undo.redo.is_empty() && !self.sync.track.is_empty() {
            self.sync.track = Track::default();
        }
        // The status scans the Outboxes: only recompute it when something may have changed.
        let accounts = self.mailbox.accounts.iter().filter(|a| is_server_account(a.kind)).count();
        let offline_shown = self.sync.status.connection == Connection::Offline;
        if self.sync.dirty
            || mailbox_changed
            || accounts != self.sync.status.accounts.len()
            || (accounts > 0 && offline_shown != self.view.work_offline)
        {
            refresh_status(self);
        }
        mailbox_changed || self.sync.status != status_before
    }

    /// Polls until no account is busy and no queued change is due (or `timeout` passes). For the CLI, the
    /// control channel and tests. Returns true when everything finished in time.
    pub fn wait_for_sync(&mut self, timeout: std::time::Duration) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let deadline = std::time::Instant::now().checked_add(timeout);
            loop {
                self.poll_sync();
                let now = self.now().unix;
                let due = self.mailbox.accounts.iter().any(|a| {
                    a.kind == AccountKind::Imap
                        && !self.view.work_offline
                        && has_due_ops(&self.mailbox, a, now)
                        && self.sync.runs.get(&a.id).is_none_or(|r| r.blocked_until <= now)
                });
                if !self.sync_busy() && !due {
                    return true;
                }
                if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = timeout;
            self.poll_sync();
            true
        }
    }

    /// Asks every busy worker to stop after its current step. Returns how many were asked.
    pub fn cancel_sync(&mut self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let busy: Vec<AccountId> =
                self.sync.runs.iter().filter(|(_, r)| r.busy.is_some()).map(|(id, _)| *id).collect();
            for id in &busy {
                self.sync.rt.cancel(*id);
                if let Some(r) = self.sync.runs.get_mut(id) {
                    r.again = None;
                }
            }
            busy.len()
        }
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }
}

/// Server accounts that take part in send/receive.
pub(crate) fn server_accounts(s: &Session) -> Vec<AccountId> {
    s.mailbox.accounts.iter().filter(|a| is_server_account(a.kind)).map(|a| a.id).collect()
}

/// After `compose.send`: sends the Outbox of `account` now when "Send immediately when connected" is on.
/// Problems only show in the status (the message waits in the Outbox either way).
pub(crate) fn send_now(s: &mut Session, account: AccountId) {
    if s.view.work_offline || !s.view.send_immediately {
        return;
    }
    if s.mailbox.account(account).is_none_or(|a| a.kind != AccountKind::Imap) {
        return;
    }
    let plan = Plan { send: true, receive: false, folders: None, user: false };
    if let Err(e) = dispatch(s, account, plan) {
        let until = s.now().unix.saturating_add(BLOCKED_RETRY_SECS);
        let run = s.sync.runs.entry(account).or_default();
        run.error = Some(e);
        run.blocked_until = until;
    }
    refresh_status(s);
}

/// Drops the worker and runtime state of a removed account.
pub(crate) fn forget_account(s: &mut Session, id: AccountId) {
    s.sync.runs.remove(&id);
    s.sync.tests.remove(&id);
    #[cfg(not(target_arch = "wasm32"))]
    s.sync.rt.forget(id);
    refresh_status(s);
}

/// Tests an account's servers (connect + log in). `wait` blocks for the result; otherwise the test runs
/// on its own thread and the report arrives through [`Session::poll_sync`] (`None` returned).
pub(crate) fn test_account(
    s: &mut Session,
    id: AccountId,
    wait: bool,
) -> Result<Option<TestReport>, CmdError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        native::test_account(s, id, wait)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, id, wait);
        Err(CmdError::Disabled("Testing an account is not available in the browser.".into()))
    }
}

// =======================================================================================================
// Status computation
// =======================================================================================================

fn refresh_status(s: &mut Session) {
    s.sync.dirty = false;
    let mut st = SyncStatus::default();
    let mut any_error = None;
    let mut any_synced = false;
    let mut busy_text = None;
    for a in s.mailbox.accounts.iter().filter(|a| is_server_account(a.kind)) {
        let run = s.sync.runs.get(&a.id);
        let outbox = s
            .mailbox
            .role_folder(a.id, mailcraft_model::FolderRole::Outbox)
            .map_or(0, |f| s.mailbox.total_count(f));
        let busy = run.is_some_and(|r| r.busy.is_some());
        let row = AccountStatus {
            account: a.id.0,
            name: a.display_name.clone(),
            busy,
            progress: run.map(|r| r.progress.clone()).unwrap_or_default(),
            error: run.and_then(|r| r.error.clone()),
            last_sync: run.and_then(|r| r.last_sync),
            pending_changes: a.sync.pending.len(),
            outbox,
        };
        if busy && busy_text.is_none() {
            busy_text = Some(row.progress.clone());
        }
        if let Some(e) = &row.error {
            any_error.get_or_insert_with(|| e.clone());
        }
        any_synced |= row.last_sync.is_some();
        st.accounts.push(row);
    }
    st.busy = busy_text.is_some();
    if st.accounts.is_empty() {
        st.connection = Connection::Idle;
    } else if s.view.work_offline {
        st.connection = Connection::Offline;
        st.text = "Working offline".into();
    } else if let Some(t) = busy_text {
        st.connection = if any_synced { Connection::Connected } else { Connection::Connecting };
        st.text = if t.is_empty() { "Send/Receive in progress…".into() } else { t };
    } else if let Some(e) = any_error {
        st.connection = Connection::Disconnected;
        st.text = format!("Send/Receive error: {e}");
    } else if any_synced {
        st.connection = Connection::Connected;
        st.text = "All folders are up to date.".into();
    }
    st.connection_text = st.connection.label().to_owned();
    s.sync.status = st;
}

// =======================================================================================================
// Dispatch, polling and the automatic timer
// =======================================================================================================

#[cfg(not(target_arch = "wasm32"))]
fn dispatch(s: &mut Session, id: AccountId, plan: Plan) -> Result<bool, String> {
    native::dispatch(s, id, plan)
}

#[cfg(target_arch = "wasm32")]
fn dispatch(s: &mut Session, id: AccountId, _plan: Plan) -> Result<bool, String> {
    if s.mailbox.account(id).is_some_and(|a| is_server_account(a.kind)) {
        Err("Send/Receive is not available in the browser.".into())
    } else {
        Ok(false)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_events(s: &mut Session) -> bool {
    native::poll_events(s)
}

#[cfg(target_arch = "wasm32")]
fn poll_events(_: &mut Session) -> bool {
    false
}

/// Starts automatic runs: every N minutes (Options), and for queued server changes that are due.
fn tick(s: &mut Session) {
    if s.view.work_offline {
        return;
    }
    let accounts: Vec<AccountId> = s
        .mailbox
        .accounts
        .iter()
        .filter(|a| a.kind == AccountKind::Imap && a.incoming.is_some())
        .map(|a| a.id)
        .collect();
    if accounts.is_empty() {
        return;
    }
    let now = s.now().unix;
    let every = i64::from(s.view.auto_send_receive_minutes).saturating_mul(60);
    let mut auto = false;
    if every > 0 {
        match s.sync.last_auto {
            None => s.sync.last_auto = Some(now),
            Some(t) if now.saturating_sub(t) >= every => {
                s.sync.last_auto = Some(now);
                auto = true;
            }
            Some(_) => {}
        }
    }
    for id in accounts {
        if s.sync.busy(id) || s.sync.runs.get(&id).is_some_and(|r| r.blocked_until > now) {
            continue;
        }
        let due = s.mailbox.account(id).is_some_and(|a| has_due_ops(&s.mailbox, a, now));
        let plan = if auto {
            Plan { send: true, receive: true, folders: None, user: false }
        } else if due {
            Plan::default()
        } else {
            continue;
        };
        s.sync.dirty = true;
        if let Err(e) = dispatch(s, id, plan) {
            let run = s.sync.runs.entry(id).or_default();
            run.error = Some(e);
            run.blocked_until = now.saturating_add(BLOCKED_RETRY_SECS);
        }
    }
}

/// True when an upload's item no longer exists (the op is skipped and later pruned).
fn stale(mb: &Mailbox, op: &RemoteOp) -> bool {
    matches!(op, RemoteOp::Append { item, .. } if !mb.mail.contains_key(item))
}

/// True when the next queued server change of `a` is due at `now`. Changes run in queue order, so one waiting
/// out a retry delay holds back the ones after it (a later upload must not overtake the folder it goes into).
fn has_due_ops(mb: &Mailbox, a: &mailcraft_model::Account, now: i64) -> bool {
    a.sync.pending.iter().find(|p| !stale(mb, &p.op)).is_some_and(|p| p.not_before <= now)
}

/// The queued server changes one run of `plan` carries, in order, with the source of each upload. Automatic
/// runs stop at the first change still waiting out a retry delay; user-started ones take everything.
fn job_ops(
    s: &Session,
    a: &mailcraft_model::Account,
    plan: &Plan,
    now: i64,
) -> Vec<(u64, RemoteOp, Option<Vec<u8>>)> {
    let mut ops = Vec::new();
    for p in &a.sync.pending {
        if stale(&s.mailbox, &p.op) {
            continue;
        }
        if !plan.user && p.not_before > now {
            break;
        }
        let raw = match &p.op {
            RemoteOp::Append { item, .. } => s.mailbox.mail.get(item).map(|m| item_source(s, m)),
            _ => None,
        };
        ops.push((p.id, p.op.clone(), raw));
    }
    ops
}

/// Retry delay after `attempts` failures.
fn backoff_secs(attempts: u32) -> i64 {
    let shift = attempts.saturating_sub(1).min(16);
    BACKOFF_BASE_SECS.saturating_mul(1i64 << shift).min(BACKOFF_MAX_SECS)
}

// =======================================================================================================
// Local → server: queued changes
// =======================================================================================================

/// Server folder of a local folder: (account, server name) when the folder is synchronised.
fn synced_folder(mb: &Mailbox, fid: FolderId) -> Option<(AccountId, String)> {
    let f = mb.folders.get(&fid)?;
    if f.kind != FolderKind::Mail {
        return None;
    }
    let a = mb.account(f.account)?;
    if a.kind != AccountKind::Imap {
        return None;
    }
    Some((f.account, f.remote_name.clone()?))
}

fn uid_of(m: &MailItem) -> Option<u32> {
    m.remote_id.as_deref()?.parse().ok().filter(|u| *u > 0)
}

/// IMAP flags that mirror an item's state (`\Draft` only matters when uploading).
fn flag_words(m: &MailItem) -> BTreeSet<&'static str> {
    let mut f = BTreeSet::new();
    if m.read {
        f.insert("\\Seen");
    }
    if m.is_flagged() {
        f.insert("\\Flagged");
    }
    match m.response {
        ResponseState::Replied(_) | ResponseState::RepliedAll(_) => {
            f.insert("\\Answered");
        }
        ResponseState::Forwarded(_) => {
            f.insert("$Forwarded");
        }
        ResponseState::None => {}
    }
    f
}

fn upload_flags(m: &MailItem) -> Vec<String> {
    let mut f: Vec<String> = flag_words(m).into_iter().map(str::to_owned).collect();
    if m.is_draft {
        f.push("\\Draft".into());
    }
    f
}

fn delimiter(mb: &Mailbox, account: AccountId) -> char {
    mb.account(account).and_then(|a| a.sync.delimiter).unwrap_or('/')
}

/// Server name for a local folder: its parent's server name + delimiter + its name (the delimiter inside the
/// name becomes '-'); `None` when the parent is not on the server.
fn remote_path(mb: &Mailbox, f: &Folder) -> Option<String> {
    let d = delimiter(mb, f.account);
    let leaf: String = f.name.chars().map(|c| if c == d { '-' } else { c }).collect();
    match f.parent {
        None => Some(leaf),
        Some(p) => {
            let pf = mb.folders.get(&p)?;
            let pn = pf.remote_name.as_deref()?;
            Some(format!("{pn}{d}{leaf}"))
        }
    }
}

fn depth(mb: &Mailbox, fid: FolderId) -> usize {
    let mut n = 0usize;
    let mut cur = mb.folders.get(&fid).and_then(|f| f.parent);
    while let Some(p) = cur {
        n = n.saturating_add(1);
        if n > mb.folders.len() {
            break;
        }
        cur = mb.folders.get(&p).and_then(|f| f.parent);
    }
    n
}

fn push_op(mb: &mut Mailbox, account: AccountId, op: RemoteOp) {
    if let Some(a) = mb.accounts.iter_mut().find(|a| a.id == account) {
        a.sync.next_op = a.sync.next_op.saturating_add(1);
        let id = a.sync.next_op;
        a.sync.pending.push(PendingOp { id, op, attempts: 0, not_before: 0, last_error: None });
        if a.sync.pending.len() > MAX_PENDING {
            let excess = a.sync.pending.len().saturating_sub(MAX_PENDING);
            log::warn!("server change queue of account {} is full; dropping {excess} oldest", account.0);
            a.sync.pending.drain(..excess);
        }
    }
}

fn set_remote_id(mb: &mut Mailbox, id: ItemId, uid: Option<u32>) {
    if let Some(m) = mb.mail.get_mut(&id) {
        m.remote_id = uid.map(|u| u.to_string());
    }
}

/// Queues the server changes that turn `before` into the current mailbox (after a command or an undo).
pub(crate) fn queue_local_changes(s: &mut Session, before: &Mailbox) -> usize {
    let inflight = s.sync.inflight_ops();
    queue_changes(before, &mut s.mailbox, &inflight)
}

/// Compares `before` and `after` for IMAP accounts and appends [`RemoteOp`]s to the accounts' queues.
/// Adjusts `after` as the server will see it: server names of new/renamed folders, cleared UIDs of moved or
/// copied items, and queued (not yet dispatched) operations that a later change made moot. Returns how many
/// operations were queued.
pub(crate) fn queue_changes(before: &Mailbox, after: &mut Mailbox, inflight: &Inflight) -> usize {
    if !after.accounts.iter().any(|a| a.kind == AccountKind::Imap) {
        return 0;
    }
    let mut q: Vec<(AccountId, RemoteOp)> = Vec::new();
    folder_changes(before, after, &mut q);
    item_changes(before, after, inflight, &mut q);
    let n = q.len();
    for (account, op) in q {
        push_op(after, account, op);
    }
    n
}

fn is_imap(mb: &Mailbox, account: AccountId) -> bool {
    mb.account(account).is_some_and(|a| a.kind == AccountKind::Imap && a.incoming.is_some())
}

fn folder_changes(before: &Mailbox, after: &mut Mailbox, q: &mut Vec<(AccountId, RemoteOp)>) {
    let mut ids: Vec<FolderId> = after
        .folders
        .values()
        .filter(|f| f.kind == FolderKind::Mail && f.role.is_none() && is_imap(after, f.account))
        .map(|f| f.id)
        .collect();
    ids.sort_by_cached_key(|id| (depth(after, *id), *id));
    for fid in ids {
        let Some(f) = after.folders.get(&fid).cloned() else { continue };
        match before.folders.get(&fid) {
            None => {
                if f.remote_name.is_none()
                    && let Some(path) = remote_path(after, &f)
                {
                    q.push((f.account, RemoteOp::CreateFolder { name: path.clone() }));
                    if let Some(g) = after.folders.get_mut(&fid) {
                        g.remote_name = Some(path);
                    }
                }
            }
            Some(b) => {
                let Some(old) = b.remote_name.clone() else { continue };
                if f.remote_name.as_deref() != Some(old.as_str())
                    || (b.name == f.name && b.parent == f.parent)
                {
                    continue;
                }
                let d = delimiter(after, f.account);
                let prefix = format!("{old}{d}");
                match remote_path(after, &f) {
                    Some(new) if new != old => {
                        q.push((f.account, RemoteOp::RenameFolder { from: old.clone(), to: new.clone() }));
                        for g in after.folders.values_mut().filter(|g| g.account == f.account) {
                            if g.id == fid {
                                g.remote_name = Some(new.clone());
                            } else if let Some(rest) =
                                g.remote_name.as_deref().and_then(|n| n.strip_prefix(&prefix))
                            {
                                g.remote_name = Some(format!("{new}{d}{rest}"));
                            }
                        }
                    }
                    Some(_) => {}
                    None => {
                        // Moved under a folder that is not on the server: the server copy goes away and the
                        // folder (with its subfolders and items) becomes local.
                        let sub = after.subtree(fid);
                        let mut names: Vec<String> =
                            sub.iter().filter_map(|x| after.folders.get(x)?.remote_name.clone()).collect();
                        names.sort_by_key(|n| std::cmp::Reverse(n.len()));
                        for n in names {
                            q.push((f.account, RemoteOp::DeleteFolder { name: n }));
                        }
                        for x in &sub {
                            if let Some(g) = after.folders.get_mut(x) {
                                g.remote_name = None;
                            }
                        }
                        for m in after.mail.values_mut().filter(|m| sub.contains(&m.folder)) {
                            m.remote_id = None;
                        }
                    }
                }
            }
        }
    }
    let mut gone: Vec<(AccountId, String)> = before
        .folders
        .values()
        .filter(|b| {
            !after.folders.contains_key(&b.id)
                && b.role.is_none()
                && b.kind == FolderKind::Mail
                && is_imap(after, b.account)
        })
        .filter_map(|b| Some((b.account, b.remote_name.clone()?)))
        .collect();
    gone.sort_by_key(|(_, n)| std::cmp::Reverse(n.len()));
    for (a, name) in gone {
        q.push((a, RemoteOp::DeleteFolder { name }));
    }
}

/// Retargets a queued, not yet dispatched Move/Append of item `id` to `dst` (or cancels it). Returns
/// `(handled, uid to restore)`.
fn retarget(
    after: &mut Mailbox,
    inflight: &Inflight,
    id: ItemId,
    dst: Option<&(AccountId, String)>,
) -> (bool, Option<u32>) {
    for a in after.accounts.iter_mut() {
        let account = a.id;
        let Some(pos) = a.sync.pending.iter().position(|p| {
            !inflight.contains(&(account, p.id))
                && matches!(&p.op, RemoteOp::Move { item, .. } | RemoteOp::Append { item, .. } if *item == id)
        }) else {
            continue;
        };
        let same_account = dst.is_some_and(|(da, _)| *da == account);
        let Some(p) = a.sync.pending.get_mut(pos) else { continue };
        match &mut p.op {
            RemoteOp::Move { folder, uid, dest, .. } => {
                let (src, uid) = (folder.clone(), *uid);
                match dst {
                    Some((_, dn)) if same_account && *dn == src => {
                        a.sync.pending.remove(pos);
                        return (true, Some(uid));
                    }
                    Some((_, dn)) if same_account => {
                        *dest = dn.clone();
                        return (true, None);
                    }
                    _ => {
                        p.op = RemoteOp::Expunge { folder: src, uid };
                        return (dst.is_none(), None);
                    }
                }
            }
            RemoteOp::Append { folder, .. } => match dst {
                Some((_, dn)) if same_account => {
                    *folder = dn.clone();
                    return (true, None);
                }
                _ => {
                    a.sync.pending.remove(pos);
                    return (dst.is_none(), None);
                }
            },
            _ => {}
        }
    }
    (false, None)
}

/// Removes a queued, not yet dispatched op matching `pred`; true when one was removed.
fn cancel_op(after: &mut Mailbox, inflight: &Inflight, pred: impl Fn(&RemoteOp) -> bool) -> bool {
    for a in after.accounts.iter_mut() {
        let account = a.id;
        if let Some(pos) =
            a.sync.pending.iter().position(|p| !inflight.contains(&(account, p.id)) && pred(&p.op))
        {
            a.sync.pending.remove(pos);
            return true;
        }
    }
    false
}

fn item_changes(
    before: &Mailbox,
    after: &mut Mailbox,
    inflight: &Inflight,
    q: &mut Vec<(AccountId, RemoteOp)>,
) {
    // Only items whose server-relevant state changed (most commands touch a handful of items).
    let same = |a: &MailItem, b: &MailItem| {
        a.folder == b.folder
            && a.read == b.read
            && a.flag == b.flag
            && a.response == b.response
            && a.remote_id == b.remote_id
            && a.is_draft == b.is_draft
            && (!a.is_draft || a.raw == b.raw)
    };
    let mut ids: BTreeSet<ItemId> = before
        .mail
        .iter()
        .filter(|(id, b)| after.mail.get(id).is_none_or(|a| !same(a, b)))
        .map(|(id, _)| *id)
        .collect();
    ids.extend(after.mail.keys().filter(|id| !before.mail.contains_key(id)));
    for id in ids {
        let b = before.mail.get(&id);
        // What is needed of the item after the change (taken before `after` is adjusted).
        let Some((a_folder, a_uid, upload, words, draft_changed)) = after.mail.get(&id).map(|a| {
            let draft_changed =
                b.is_some_and(|b| a.is_draft && b.is_draft && a.raw.is_some() && a.raw != b.raw);
            (a.folder, uid_of(a), upload_flags(a), flag_words(a), draft_changed)
        }) else {
            // Removed (permanent delete). Items of a deleted folder go with the folder.
            let Some(b) = b else { continue };
            match (uid_of(b), synced_folder(after, b.folder)) {
                (Some(uid), Some((acct, name))) => q.push((acct, RemoteOp::Expunge { folder: name, uid })),
                _ => {
                    retarget(after, inflight, id, None);
                }
            }
            continue;
        };
        let dst = synced_folder(after, a_folder);
        let Some(b) = b else {
            // New item (copy, paste, import, rule copy, undo of a permanent delete).
            if let (Some(uid), Some((_, name))) = (a_uid, &dst)
                && cancel_op(
                    after,
                    inflight,
                    |op| matches!(op, RemoteOp::Expunge { folder, uid: u } if folder == name && *u == uid),
                )
            {
                continue;
            }
            set_remote_id(after, id, None);
            if let Some((acct, name)) = dst {
                q.push((acct, RemoteOp::Append { folder: name, item: id, flags: upload }));
            }
            continue;
        };
        let src = synced_folder(after, b.folder).or_else(|| synced_folder(before, b.folder));
        let b_uid = uid_of(b).filter(|_| src.is_some());
        if a_folder != b.folder {
            set_remote_id(after, id, None);
            match (b_uid, src, dst) {
                (Some(uid), Some((sa, sn)), Some((da, dn))) if sa == da => {
                    q.push((sa, RemoteOp::Move { folder: sn, uid, dest: dn, item: id }));
                }
                (Some(uid), Some((sa, sn)), dst) => {
                    q.push((sa, RemoteOp::Expunge { folder: sn, uid }));
                    if let Some((da, dn)) = dst {
                        q.push((da, RemoteOp::Append { folder: dn, item: id, flags: upload }));
                    }
                }
                (_, _, dst) => {
                    let (handled, restore) = retarget(after, inflight, id, dst.as_ref());
                    if let Some(uid) = restore {
                        set_remote_id(after, id, Some(uid));
                    } else if !handled && let Some((da, dn)) = dst {
                        q.push((da, RemoteOp::Append { folder: dn, item: id, flags: upload }));
                    }
                }
            }
            continue;
        }
        let (Some(uid), Some((sa, sn))) = (b_uid, src) else { continue };
        if draft_changed {
            // A draft saved again: upload the new version, remove the old one.
            set_remote_id(after, id, None);
            q.push((sa, RemoteOp::Append { folder: sn.clone(), item: id, flags: upload }));
            q.push((sa, RemoteOp::Expunge { folder: sn, uid }));
            continue;
        }
        let fb = flag_words(b);
        let add: Vec<String> = words.difference(&fb).map(|s| (*s).to_owned()).collect();
        let remove: Vec<String> = fb.difference(&words).map(|s| (*s).to_owned()).collect();
        if !add.is_empty() || !remove.is_empty() {
            q.push((sa, RemoteOp::Flags { folder: sn, uid, add, remove }));
        }
    }
}

/// Undo/redo replaced `live` with a snapshot (now `s.mailbox`): keep what synchronisation did meanwhile and
/// queue the server changes that revert the undone command.
pub(crate) fn after_restore(s: &mut Session, live: &Mailbox) {
    if !live.accounts.iter().any(|a| a.kind == AccountKind::Imap) {
        return;
    }
    carry_over(live, &mut s.mailbox, &s.sync.track);
    queue_local_changes(s, live);
}

fn carry_over(live: &Mailbox, restored: &mut Mailbox, track: &Track) {
    restored.next_id = restored.next_id.max(live.next_id);
    for a in restored.accounts.iter_mut() {
        if let Some(l) = live.account(a.id) {
            a.sync = l.sync.clone();
        }
    }
    // Folders synchronisation created or removed meanwhile.
    let mut created: Vec<&Folder> = track
        .folders_created
        .iter()
        .filter(|f| !restored.folders.contains_key(f))
        .filter_map(|f| live.folders.get(f))
        .collect();
    created.sort_by_key(|f| depth(live, f.id));
    for f in created {
        let parent_ok = f.parent.is_none_or(|p| restored.folders.contains_key(&p));
        if parent_ok && restored.account(f.account).is_some() {
            restored.folders.insert(f.id, f.clone());
        }
    }
    for fid in &track.folders_removed {
        if !live.folders.contains_key(fid) && restored.folders.contains_key(fid) {
            let sub = restored.subtree(*fid);
            restored.folders.retain(|id, _| !sub.contains(id));
            restored.mail.retain(|_, m| !sub.contains(&m.folder));
        }
    }
    // Server names of folders present in both.
    let names: Vec<(FolderId, Option<String>)> = restored
        .folders
        .keys()
        .filter_map(|id| live.folders.get(id).map(|l| (*id, l.remote_name.clone())))
        .collect();
    for (id, name) in names {
        if let Some(f) = restored.folders.get_mut(&id) {
            f.remote_name = name;
        }
    }
    // Mail that arrived or vanished meanwhile, and server flag changes.
    for id in &track.created {
        if let Some(m) = live.mail.get(id)
            && !restored.mail.contains_key(id)
            && restored.folders.contains_key(&m.folder)
        {
            restored.mail.insert(*id, m.clone());
        }
    }
    for id in &track.removed {
        if !live.mail.contains_key(id) {
            restored.mail.remove(id);
        }
    }
    for (id, m) in restored.mail.iter_mut() {
        let Some(l) = live.mail.get(id) else { continue };
        if l.folder == m.folder {
            m.remote_id = l.remote_id.clone();
        }
        if track.flags.contains(id) {
            m.read = l.read;
            m.flag = l.flag.clone();
        }
    }
}

// =======================================================================================================
// Server → local helpers (shared by the native worker results)
// =======================================================================================================

/// Applies server flags to an item (the server wins). Returns true when something changed.
fn apply_server_flags(m: &mut MailItem, flags: &[String], now: mailcraft_model::DateTime) -> bool {
    let has = |f: &str| flags.iter().any(|x| x.eq_ignore_ascii_case(f));
    let mut changed = false;
    let read = has("\\Seen");
    if m.read != read {
        m.read = read;
        changed = true;
    }
    let flagged = has("\\Flagged");
    if flagged && !m.is_flagged() {
        m.flag = FlagState::Flagged { label: "Follow up".into(), start: None, due: None, reminder: None };
        changed = true;
    } else if !flagged && m.is_flagged() {
        m.flag = FlagState::None;
        changed = true;
    }
    if m.response == ResponseState::None {
        if has("\\Answered") {
            m.response = ResponseState::Replied(now);
            changed = true;
        } else if has("$Forwarded") {
            m.response = ResponseState::Forwarded(now);
            changed = true;
        }
    }
    changed
}

/// A safe local folder name for a server folder name.
fn local_folder_name(name: &str) -> String {
    let cleaned: String =
        name.chars().filter(|c| !c.is_control()).map(|c| if c == '\\' { '_' } else { c }).collect();
    let t: String = cleaned.trim().chars().take(mailcraft_model::mailbox::MAX_FOLDER_NAME_CHARS).collect();
    if t.trim().is_empty() { "Folder".to_owned() } else { t.trim().to_owned() }
}

/// Removes folders (and their subfolders and items) permanently; returns the removed item ids.
fn remove_folders(mb: &mut Mailbox, ids: &[FolderId]) -> Vec<ItemId> {
    let mut all: BTreeSet<FolderId> = BTreeSet::new();
    for id in ids {
        all.extend(mb.subtree(*id));
    }
    let removed: Vec<ItemId> = mb.mail.values().filter(|m| all.contains(&m.folder)).map(|m| m.id).collect();
    mb.mail.retain(|_, m| !all.contains(&m.folder));
    mb.folders.retain(|id, _| !all.contains(id));
    mb.views.retain(|id, _| !all.contains(id));
    for a in mb.accounts.iter_mut() {
        a.sync.folders.retain(|id, _| !all.contains(id));
    }
    for rule in &mut mb.rules {
        if rule.actions.iter().any(|a| {
            matches!(a, mailcraft_model::RuleAction::MoveTo(f) | mailcraft_model::RuleAction::CopyTo(f) if all.contains(f))
        }) {
            rule.enabled = false;
        }
    }
    removed
}

/// Raw source of an item: kept bytes, else the blob store, else rebuilt from its fields.
fn item_source(s: &Session, m: &MailItem) -> Vec<u8> {
    if let Some(raw) = &m.raw {
        return raw.clone();
    }
    if let Some(st) = &s.store
        && let Ok(Some(raw)) = st.raw_of(m)
    {
        return raw;
    }
    mailcraft_store::rebuild_message(m)
}

// =======================================================================================================
// Native: workers, jobs and results
// =======================================================================================================

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::collections::{BTreeMap, BTreeSet};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use mailcraft_net::smtp::{SmtpClient, SmtpSettings};
    use mailcraft_net::sync::{FolderState, ImapAccount, ImapSettings, RemoteFolder, SyncDelta};
    use mailcraft_net::{ConnectOptions, Credentials, NetError};

    use super::*;
    use crate::accounts::{account_secret, pem_certificates};
    use mailcraft_model::account::CredentialKind;
    use mailcraft_model::{Account, FolderRole, ServerSettings};

    /// Most batches of new messages fetched for one folder in one run.
    const MAX_ROUNDS_PER_FOLDER: usize = 50;
    /// Most events applied by one poll (the rest wait for the next one).
    const MAX_EVENTS_PER_POLL: usize = 5_000;

    /// Connection settings for one account (built on the session thread, used by the worker).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct Conn {
        imap: ImapSettings,
        smtp: Option<SmtpSettings>,
    }

    struct Outgoing {
        item: ItemId,
        from: String,
        rcpts: Vec<String>,
        raw: Vec<u8>,
    }

    struct Receive {
        only: Option<Vec<String>>,
        states: BTreeMap<String, FolderState>,
    }

    struct Job {
        conn: Conn,
        ops: Vec<(u64, RemoteOp, Option<Vec<u8>>)>,
        outgoing: Vec<Outgoing>,
        receive: Option<Receive>,
    }

    pub(super) enum OpOutcome {
        Done,
        NewUid(Option<u32>),
    }

    pub(super) struct OpError {
        message: String,
        transient: bool,
    }

    pub(super) enum Event {
        Progress { account: AccountId, text: String },
        OpDone { account: AccountId, op: u64, result: Result<OpOutcome, OpError> },
        Sent { account: AccountId, item: ItemId, result: Result<(), String> },
        Folders { account: AccountId, folders: Vec<RemoteFolder> },
        Delta { account: AccountId, name: String, delta: SyncDelta },
        FolderFailed { account: AccountId, name: String, error: String },
        Done { account: AccountId, error: Option<String> },
        Test { account: AccountId, report: TestReport },
    }

    impl Event {
        fn account(&self) -> AccountId {
            match self {
                Event::Progress { account, .. }
                | Event::OpDone { account, .. }
                | Event::Sent { account, .. }
                | Event::Folders { account, .. }
                | Event::Delta { account, .. }
                | Event::FolderFailed { account, .. }
                | Event::Done { account, .. }
                | Event::Test { account, .. } => *account,
            }
        }
    }

    struct Worker {
        jobs: mpsc::Sender<Job>,
        cancel: Arc<AtomicBool>,
        /// Tags this worker's events: results of a worker that was replaced or forgotten (account removed,
        /// its id reused, its server changed) are dropped instead of being applied to the wrong account.
        serial: u64,
    }

    /// Serial of events not tied to a worker (Test Account Settings).
    const NO_WORKER: u64 = 0;

    /// Where a worker sends its events.
    #[derive(Clone)]
    struct Events {
        tx: mpsc::Sender<(u64, Event)>,
        serial: u64,
    }

    impl Events {
        /// False when the session is gone.
        fn send(&self, e: Event) -> bool {
            self.tx.send((self.serial, e)).is_ok()
        }
    }

    pub(super) struct Runtime {
        tx: mpsc::Sender<(u64, Event)>,
        rx: mpsc::Receiver<(u64, Event)>,
        workers: BTreeMap<AccountId, Worker>,
        last_serial: u64,
    }

    impl Default for Runtime {
        fn default() -> Self {
            let (tx, rx) = mpsc::channel();
            Runtime { tx, rx, workers: BTreeMap::new(), last_serial: NO_WORKER }
        }
    }

    impl Drop for Runtime {
        /// The session is closing: workers stop after their current step (dropping the job senders then ends
        /// their threads) instead of finishing a long download nobody will apply.
        fn drop(&mut self) {
            for w in self.workers.values() {
                w.cancel.store(true, Ordering::SeqCst);
            }
        }
    }

    impl Runtime {
        pub(super) fn cancel(&self, id: AccountId) {
            if let Some(w) = self.workers.get(&id) {
                w.cancel.store(true, Ordering::SeqCst);
            }
        }

        /// Dropping the job sender ends the worker after its current step; its events are ignored from now on.
        pub(super) fn forget(&mut self, id: AccountId) {
            if let Some(w) = self.workers.remove(&id) {
                w.cancel.store(true, Ordering::SeqCst);
            }
        }

        fn submit(&mut self, account: AccountId, job: Job) -> Result<(), String> {
            let mut job = Some(job);
            for _ in 0..2 {
                if !self.workers.contains_key(&account) {
                    let (jtx, jrx) = mpsc::channel::<Job>();
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.last_serial = self.last_serial.saturating_add(1);
                    let serial = self.last_serial;
                    let events = Events { tx: self.tx.clone(), serial };
                    let c2 = Arc::clone(&cancel);
                    std::thread::Builder::new()
                        .name(format!("mailcraft-sync-{}", account.0))
                        .spawn(move || worker_main(account, jrx, events, c2))
                        .map_err(|e| format!("cannot start the send/receive thread: {e}"))?;
                    self.workers.insert(account, Worker { jobs: jtx, cancel, serial });
                }
                let Some(w) = self.workers.get(&account) else { continue };
                w.cancel.store(false, Ordering::SeqCst);
                let Some(j) = job.take() else { break };
                match w.jobs.send(j) {
                    Ok(()) => return Ok(()),
                    Err(mpsc::SendError(j)) => {
                        // The worker ended (it should not): start a new one.
                        job = Some(j);
                        self.workers.remove(&account);
                    }
                }
            }
            Err("the send/receive thread is not running".into())
        }
    }

    // ---------------------------------------------------------------------------------------------------
    // Settings
    // ---------------------------------------------------------------------------------------------------

    fn connect_options(srv: &ServerSettings) -> Result<ConnectOptions, String> {
        let mut o = ConnectOptions {
            connect_timeout: Duration::from_secs(20),
            io_timeout: Duration::from_secs(60),
            ..ConnectOptions::default()
        };
        if let Some(pem) = &srv.extra_root_pem {
            o.tls.extra_roots_der = pem_certificates(pem)?;
        }
        o.allow_plaintext_auth = srv.allow_plaintext_auth;
        Ok(o)
    }

    fn credentials(a: &Account, user: &str, secret: String) -> Credentials {
        match a.credential {
            CredentialKind::Password => Credentials::password(user, secret),
            CredentialKind::OAuthToken => Credentials::oauth2(user, secret),
        }
    }

    pub(super) fn conn_settings(s: &Session, a: &Account) -> Result<Conn, String> {
        let inc = a.incoming.as_ref().ok_or("The account has no incoming mail server.")?;
        if inc.host.trim().is_empty() {
            return Err("The account has no incoming mail server.".into());
        }
        let secret = account_secret(s, a.id, false)
            .ok_or_else(|| format!("Enter the password for {} (Account Settings).", a.address))?;
        let user = if inc.username.is_empty() { a.address.clone() } else { inc.username.clone() };
        let mut imap = ImapSettings::from_server(inc, credentials(a, &user, secret));
        imap.options = connect_options(inc)?;
        let smtp = match &a.outgoing {
            Some(o) if !o.host.trim().is_empty() => {
                let mut st = SmtpSettings::new(o.host.clone(), o.port, o.security);
                st.auth_mechanism = o.auth.clone();
                st.options = connect_options(o)?;
                let user = if o.username.is_empty() { a.address.clone() } else { o.username.clone() };
                st.credentials = account_secret(s, a.id, true).map(|sec| credentials(a, &user, sec));
                Some(st)
            }
            _ => None,
        };
        Ok(Conn { imap, smtp })
    }

    // ---------------------------------------------------------------------------------------------------
    // Dispatch (session thread)
    // ---------------------------------------------------------------------------------------------------

    pub(super) fn dispatch(s: &mut Session, id: AccountId, plan: Plan) -> Result<bool, String> {
        let Some(a) = s.mailbox.account(id).cloned() else { return Ok(false) };
        match a.kind {
            AccountKind::Imap => {}
            AccountKind::Jmap => return Err("Send/Receive for JMAP accounts is not available yet.".into()),
            AccountKind::Local | AccountKind::Exchange => return Ok(false),
        }
        if let Some(run) = s.sync.runs.get_mut(&id)
            && run.busy.is_some()
        {
            match run.again.as_mut() {
                Some(again) => again.merge(&plan),
                None => run.again = Some(plan),
            }
            return Ok(false);
        }
        let conn = conn_settings(s, &a)?;
        let now = s.now().unix;
        let ops = job_ops(s, &a, &plan, now);
        let mut outgoing = Vec::new();
        if plan.send
            && a.outgoing.is_some()
            && let Some(outbox) = s.mailbox.role_folder(id, FolderRole::Outbox)
        {
            let mut items: Vec<&MailItem> = s
                .mailbox
                .items_in(outbox)
                .into_iter()
                .filter(|m| !m.is_draft && m.sent.is_none_or(|t| t.unix <= now))
                .collect();
            items.sort_by_key(|m| (m.received.unix, m.id));
            for m in items {
                let mut rcpts: Vec<String> = Vec::new();
                for addr in m.to.iter().chain(&m.cc).chain(&m.bcc) {
                    let x = addr.addr.trim().to_owned();
                    if !x.is_empty() && !rcpts.iter().any(|r| r.eq_ignore_ascii_case(&x)) {
                        rcpts.push(x);
                    }
                }
                let from = if a.address.trim().is_empty() {
                    m.from.as_ref().map(|f| f.addr.clone()).unwrap_or_default()
                } else {
                    a.address.trim().to_owned()
                };
                outgoing.push(Outgoing { item: m.id, from, rcpts, raw: item_source(s, m) });
            }
        }
        // Sending before the first update: list the folders so the sent copy can be filed on the server.
        let sent_unmapped = s
            .mailbox
            .role_folder(id, FolderRole::SentItems)
            .and_then(|f| s.mailbox.folder(f))
            .is_some_and(|f| f.remote_name.is_none());
        let list_only = !plan.receive && !outgoing.is_empty() && sent_unmapped && !a.server_saves_sent;
        let receive = (plan.receive || list_only).then(|| {
            let mut uids: BTreeMap<FolderId, Vec<u32>> = BTreeMap::new();
            for m in s.mailbox.mail.values() {
                if let Some(u) = uid_of(m) {
                    uids.entry(m.folder).or_default().push(u);
                }
            }
            // UIDs that queued changes still refer to count as known: a message moved or deleted here whose
            // server change has not run yet must not be downloaded again as new.
            let mut queued: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
            for p in &a.sync.pending {
                if let RemoteOp::Flags { folder, uid, .. }
                | RemoteOp::Move { folder, uid, .. }
                | RemoteOp::Expunge { folder, uid } = &p.op
                {
                    queued.entry(folder.as_str()).or_default().push(*uid);
                }
            }
            let mut states = BTreeMap::new();
            for f in s.mailbox.folders.values().filter(|f| f.account == id && f.kind == FolderKind::Mail) {
                let Some(name) = &f.remote_name else { continue };
                let st = a.sync.folders.get(&f.id).cloned().unwrap_or_default();
                let mut list = uids.remove(&f.id).unwrap_or_default();
                list.extend(queued.remove(name.as_str()).unwrap_or_default());
                list.sort_unstable();
                list.dedup();
                states.insert(
                    name.clone(),
                    FolderState {
                        uidvalidity: st.uidvalidity,
                        uidnext: st.uidnext,
                        highest_modseq: st.highest_modseq,
                        uids: list,
                    },
                );
            }
            let only = if list_only {
                Some(Vec::new())
            } else {
                plan.folders.as_ref().map(|fs| {
                    fs.iter()
                        .filter_map(|f| s.mailbox.folder(*f).and_then(|f| f.remote_name.clone()))
                        .collect()
                })
            };
            Receive { only, states }
        });
        if ops.is_empty() && outgoing.is_empty() && receive.is_none() {
            return Ok(false);
        }
        let inflight = InFlight {
            ops: ops.iter().map(|(i, _, _)| *i).collect(),
            sending: outgoing.iter().map(|o| o.item).collect(),
        };
        let progress = if receive.is_some() || !ops.is_empty() { "Connecting…" } else { "Sending…" };
        s.sync.rt.submit(id, Job { conn, ops, outgoing, receive })?;
        let run = s.sync.runs.entry(id).or_default();
        run.busy = Some(inflight);
        run.progress = progress.into();
        run.error = None;
        Ok(true)
    }

    // ---------------------------------------------------------------------------------------------------
    // Worker thread
    // ---------------------------------------------------------------------------------------------------

    fn worker_main(account: AccountId, jobs: mpsc::Receiver<Job>, events: Events, cancel: Arc<AtomicBool>) {
        let mut imap: Option<ImapAccount> = None;
        while let Ok(job) = jobs.recv() {
            let res = catch_unwind(AssertUnwindSafe(|| run_job(account, &mut imap, &job, &events, &cancel)));
            let error = match res {
                Ok(e) => e,
                Err(_) => {
                    imap = None;
                    Some("internal error in send/receive".to_owned())
                }
            };
            if !events.send(Event::Done { account, error }) {
                break;
            }
        }
        if let Some(mut a) = imap {
            a.disconnect();
        }
    }

    fn transient(e: &NetError) -> bool {
        e.is_connection_lost()
            || matches!(
                e,
                NetError::Connect(_) | NetError::Auth(_) | NetError::Insecure(_) | NetError::Tls(_)
            )
    }

    fn exec_op(imap: &mut ImapAccount, op: &RemoteOp, raw: Option<&[u8]>) -> Result<OpOutcome, OpError> {
        let res = match op {
            RemoteOp::Flags { folder, uid, add, remove } => {
                let add: Vec<&str> = add.iter().map(String::as_str).collect();
                let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
                imap.push_flags(folder, &[*uid], &add, &remove).map(|()| OpOutcome::Done)
            }
            RemoteOp::Move { folder, uid, dest, .. } => imap.move_messages(folder, &[*uid], dest).map(|c| {
                OpOutcome::NewUid(c.and_then(|c| c.pairs.iter().find(|(s, _)| s == uid).map(|(_, d)| *d)))
            }),
            RemoteOp::Append { folder, flags, .. } => match raw {
                Some(raw) => {
                    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
                    imap.append(folder, raw, &flags).map(OpOutcome::NewUid)
                }
                None => Err(NetError::Invalid("the message to upload no longer exists".into())),
            },
            RemoteOp::Expunge { folder, uid } => imap.expunge(folder, &[*uid]).map(|()| OpOutcome::Done),
            RemoteOp::CreateFolder { name } => imap.create_folder(name).map(|()| OpOutcome::Done),
            RemoteOp::RenameFolder { from, to } => imap.rename_folder(from, to).map(|()| OpOutcome::Done),
            RemoteOp::DeleteFolder { name } => imap.delete_folder(name).map(|()| OpOutcome::Done),
        };
        res.map_err(|e| OpError { transient: transient(&e), message: e.to_string() })
    }

    fn run_job(
        account: AccountId,
        slot: &mut Option<ImapAccount>,
        job: &Job,
        events: &Events,
        cancel: &AtomicBool,
    ) -> Option<String> {
        let send = |e: Event| {
            if !events.send(e) {
                // Nobody is listening any more (the session closed): stop at the next check.
                cancel.store(true, Ordering::SeqCst);
            }
        };
        let progress = |t: &str| send(Event::Progress { account, text: t.to_owned() });
        let cancelled = || cancel.load(Ordering::SeqCst);
        if slot.as_ref().is_some_and(|a| a.settings() != &job.conn.imap)
            && let Some(mut old) = slot.take()
        {
            old.disconnect();
        }
        let imap = slot.get_or_insert_with(|| ImapAccount::new(job.conn.imap.clone()));
        let mut error: Option<String> = None;
        // 1. Local changes first, so the folders read below already contain them.
        let total = job.ops.len();
        for (i, (id, op, raw)) in job.ops.iter().enumerate() {
            if cancelled() {
                return Some("Send/Receive was cancelled.".into());
            }
            progress(&format!("Updating the server ({} of {total})…", i.saturating_add(1)));
            let result = exec_op(imap, op, raw.as_deref());
            let lost = match &result {
                Err(e) if e.transient => Some(e.message.clone()),
                _ => None,
            };
            send(Event::OpDone { account, op: *id, result });
            if let Some(e) = lost {
                // Keep the order of the queue: stop at the first connection problem.
                error = Some(e);
                break;
            }
        }
        // 2. The folder list (before sending, so Sent Items is known when a sent message is filed).
        let mut todo: Vec<(String, String)> = Vec::new();
        if error.is_none()
            && let Some(rcv) = &job.receive
        {
            if cancelled() {
                return Some("Send/Receive was cancelled.".into());
            }
            progress("Updating the folder list…");
            match imap.sync_folder_list() {
                Ok(list) => {
                    todo = list
                        .iter()
                        .filter(|f| f.selectable)
                        .filter(|f| rcv.only.as_ref().is_none_or(|o| o.contains(&f.name)))
                        .map(|f| (f.name.clone(), f.display_name.clone()))
                        .collect();
                    // Inbox first, then in name order.
                    todo.sort_by_key(|(n, _)| (!n.eq_ignore_ascii_case("INBOX"), n.clone()));
                    send(Event::Folders { account, folders: list });
                }
                Err(e) => error = Some(e.to_string()),
            }
        }
        // 3. The Outbox (SMTP works even when the IMAP server does not).
        if !job.outgoing.is_empty() {
            send_outgoing(account, job, events, cancel, &mut error);
        }
        // 4. Folders.
        if error.is_some() {
            return error;
        }
        let Some(rcv) = &job.receive else { return error };
        for (name, display) in todo {
            if cancelled() {
                return Some("Send/Receive was cancelled.".into());
            }
            progress(&format!("Updating {display}…"));
            let mut since = rcv.states.get(&name).cloned().unwrap_or_default();
            for _ in 0..MAX_ROUNDS_PER_FOLDER {
                match imap.sync_folder(&name, &since) {
                    Ok(delta) => {
                        let more = delta.more;
                        since = delta.state.clone();
                        send(Event::Delta { account, name: name.clone(), delta });
                        if !more || cancelled() {
                            break;
                        }
                    }
                    Err(e) => {
                        let lost = e.is_connection_lost();
                        send(Event::FolderFailed { account, name: name.clone(), error: e.to_string() });
                        if lost {
                            return Some(e.to_string());
                        }
                        break;
                    }
                }
            }
        }
        error
    }

    fn send_outgoing(
        account: AccountId,
        job: &Job,
        events: &Events,
        cancel: &AtomicBool,
        error: &mut Option<String>,
    ) {
        let send = |e: Event| {
            if !events.send(e) {
                cancel.store(true, Ordering::SeqCst);
            }
        };
        let Some(set) = &job.conn.smtp else {
            for m in &job.outgoing {
                send(Event::Sent {
                    account,
                    item: m.item,
                    result: Err("The account has no outgoing mail server.".into()),
                });
            }
            return;
        };
        send(Event::Progress { account, text: "Sending…".into() });
        match SmtpClient::connect(set) {
            Err(e) => {
                let msg = e.to_string();
                for m in &job.outgoing {
                    send(Event::Sent { account, item: m.item, result: Err(msg.clone()) });
                }
                error.get_or_insert(msg);
            }
            Ok(mut c) => {
                let n = job.outgoing.len();
                for (i, m) in job.outgoing.iter().enumerate() {
                    if cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    send(Event::Progress {
                        account,
                        text: format!("Sending message {} of {n}…", i.saturating_add(1)),
                    });
                    let rcpts: Vec<&str> = m.rcpts.iter().map(String::as_str).collect();
                    let result = c.send(&m.from, &rcpts, &m.raw).map_err(|e| e.to_string());
                    send(Event::Sent { account, item: m.item, result });
                    if !c.is_usable() {
                        break;
                    }
                }
                let _ = c.quit();
            }
        }
    }

    // ---------------------------------------------------------------------------------------------------
    // Test Account Settings
    // ---------------------------------------------------------------------------------------------------

    fn run_test(
        account: AccountId,
        kind: AccountKind,
        conn: Result<Conn, String>,
        jmap_url: String,
    ) -> TestReport {
        let mut r = TestReport { account: account.0, ..Default::default() };
        let conn = match conn {
            Ok(c) => c,
            Err(e) => {
                r.incoming = e;
                return r;
            }
        };
        if kind == AccountKind::Jmap {
            let auth = match &conn.imap.credentials {
                Credentials::Password { user, password } => {
                    mailcraft_net::jmap::JmapAuth::Basic { user: user.clone(), password: password.clone() }
                }
                Credentials::OAuth2 { access_token, .. } => {
                    mailcraft_net::jmap::JmapAuth::Bearer(access_token.clone())
                }
            };
            let http = mailcraft_net::http::HttpClient::new(conn.imap.options.clone());
            match mailcraft_net::jmap::JmapClient::connect(&jmap_url, auth, http) {
                Ok(_) => {
                    r.incoming_ok = true;
                    r.incoming = format!("Signed in to {jmap_url} (JMAP).");
                    r.outgoing_ok = Some(true);
                    r.outgoing = "Sending uses the same JMAP server.".into();
                }
                Err(e) => r.incoming = e.to_string(),
            }
            return r;
        }
        let mut imap = ImapAccount::new(conn.imap.clone());
        match imap.connect() {
            Ok(_) => {
                r.incoming_ok = true;
                r.incoming = format!("Logged in to the incoming mail server {} (IMAP).", conn.imap.host);
            }
            Err(e) => r.incoming = e.to_string(),
        }
        imap.disconnect();
        if let Some(set) = &conn.smtp {
            match SmtpClient::connect(set) {
                Ok(mut c) => {
                    let _ = c.quit();
                    r.outgoing_ok = Some(true);
                    r.outgoing = if set.credentials.is_some() {
                        format!("Logged in to the outgoing mail server {} (SMTP).", set.host)
                    } else {
                        format!("Connected to the outgoing mail server {} (SMTP).", set.host)
                    };
                }
                Err(e) => {
                    r.outgoing_ok = Some(false);
                    r.outgoing = e.to_string();
                }
            }
        }
        r
    }

    pub(super) fn test_account(
        s: &mut Session,
        id: AccountId,
        wait: bool,
    ) -> Result<Option<TestReport>, CmdError> {
        let Some(a) = s.mailbox.account(id).cloned() else {
            return Err(CmdError::Params("no such account".into()));
        };
        let conn = conn_settings(s, &a);
        let url = a.incoming.as_ref().map(|i| i.host.clone()).unwrap_or_default();
        if wait {
            let report = run_test(id, a.kind, conn, url);
            s.sync.tests.insert(id, report.clone());
            return Ok(Some(report));
        }
        let events = Events { tx: s.sync.rt.tx.clone(), serial: NO_WORKER };
        std::thread::Builder::new()
            .name(format!("mailcraft-test-{}", id.0))
            .spawn(move || {
                let report =
                    catch_unwind(AssertUnwindSafe(|| run_test(id, a.kind, conn, url))).unwrap_or_else(|_| {
                        TestReport { account: id.0, incoming: "internal error".into(), ..Default::default() }
                    });
                events.send(Event::Test { account: id, report });
            })
            .map_err(|e| CmdError::Failed(format!("cannot start the test: {e}")))?;
        Ok(None)
    }

    // ---------------------------------------------------------------------------------------------------
    // Applying results (session thread)
    // ---------------------------------------------------------------------------------------------------

    pub(super) fn poll_events(s: &mut Session) -> bool {
        let mut events = Vec::new();
        let mut received = false;
        for _ in 0..MAX_EVENTS_PER_POLL {
            let Ok((serial, e)) = s.sync.rt.rx.try_recv() else { break };
            received = true;
            // Results of a worker that was forgotten or replaced are dropped (its account may be gone, or
            // a new account may have its id); test reports only need the account to exist.
            let current = if serial == NO_WORKER {
                s.mailbox.account(e.account()).is_some()
            } else {
                s.sync.rt.workers.get(&e.account()).is_some_and(|w| w.serial == serial)
            };
            if current {
                events.push(e);
            }
        }
        if !received {
            return false;
        }
        s.sync.dirty = true;
        if events.is_empty() {
            return false;
        }
        // Progress, tests and the end of a run leave the mailbox alone: no snapshot needed.
        let touches = events.iter().any(|e| {
            matches!(
                e,
                Event::OpDone { .. } | Event::Sent { .. } | Event::Folders { .. } | Event::Delta { .. }
            )
        });
        let before = touches.then(|| s.mailbox.clone());
        let res = catch_unwind(AssertUnwindSafe(|| apply_all(s, events)));
        let Some(before) = before else {
            if res.is_err() {
                log::error!("applying send/receive status panicked");
            }
            return false;
        };
        if res.is_err() {
            log::error!("applying send/receive results panicked; the mailbox was restored");
            s.mailbox = before;
            return false;
        }
        let ops = crate::session::diff_ops(&before, &s.mailbox);
        let changed = !ops.is_empty();
        s.journal(ops);
        s.tidy_selection();
        changed
    }

    fn apply_all(s: &mut Session, events: Vec<Event>) {
        let mut sent = Vec::new();
        let mut done = Vec::new();
        let mut new_inbox: Vec<ItemId> = Vec::new();
        for ev in events {
            match ev {
                Event::Progress { account, text } => s.sync.runs.entry(account).or_default().progress = text,
                Event::OpDone { account, op, result } => op_done(s, account, op, result),
                Event::Folders { account, folders } => apply_folders(s, account, folders),
                Event::Delta { account, name, delta } => {
                    apply_delta(s, account, &name, delta, &mut new_inbox)
                }
                Event::FolderFailed { account, name, error } => {
                    log::warn!("send/receive: folder {name} of account {}: {error}", account.0);
                    s.sync.runs.entry(account).or_default().error = Some(format!("{name}: {error}"));
                }
                Event::Test { account, report } => {
                    let name = s.mailbox.account(account).map(|a| a.display_name.clone()).unwrap_or_default();
                    s.notify(if report.ok() {
                        Notice::Info(format!("{name}: all tests completed successfully."))
                    } else {
                        let why = if report.incoming_ok { &report.outgoing } else { &report.incoming };
                        Notice::Error(format!("{name}: the account test failed. {why}"))
                    });
                    s.sync.tests.insert(account, report);
                }
                e @ Event::Sent { .. } => sent.push(e),
                e @ Event::Done { .. } => done.push(e),
            }
        }
        // Changes below are local actions (sending files the message, rules move it): they reach the server
        // through the queue like a command's changes.
        if !sent.is_empty() || !new_inbox.is_empty() {
            let mid = s.mailbox.clone();
            for ev in sent {
                if let Event::Sent { account, item, result } = ev {
                    message_sent(s, account, item, result);
                }
            }
            if !new_inbox.is_empty() {
                let ids: Vec<ItemId> =
                    new_inbox.into_iter().filter(|i| s.mailbox.mail.contains_key(i)).collect();
                // Rules for received messages (client-side, like every rule here).
                crate::cmd::rules::exec::on_new_mail(s, &ids);
            }
            queue_local_changes(s, &mid);
        }
        let inflight = s.sync.inflight_ops();
        prune_stale_ops(&mut s.mailbox, &inflight);
        for ev in done {
            if let Event::Done { account, error } = ev {
                run_done(s, account, error);
            }
        }
    }

    fn account_mut(s: &mut Session, id: AccountId) -> Option<&mut Account> {
        s.mailbox.accounts.iter_mut().find(|a| a.id == id)
    }

    /// Drops queued uploads whose item is gone (not those in flight: their result still has to be
    /// reconciled). Op ids are per account.
    pub(super) fn prune_stale_ops(mb: &mut Mailbox, inflight: &Inflight) {
        let mail = &mb.mail;
        for a in mb.accounts.iter_mut() {
            let account = a.id;
            a.sync.pending.retain(|p| {
                inflight.contains(&(account, p.id))
                    || !matches!(&p.op, RemoteOp::Append { item, .. } if !mail.contains_key(item))
            });
        }
    }

    pub(super) fn op_done(s: &mut Session, account: AccountId, op: u64, result: Result<OpOutcome, OpError>) {
        if let Some(b) = s.sync.runs.get_mut(&account).and_then(|r| r.busy.as_mut()) {
            b.ops.remove(&op);
        }
        let now = s.now().unix;
        let Some(a) = account_mut(s, account) else { return };
        let Some(pos) = a.sync.pending.iter().position(|p| p.id == op) else { return };
        match result {
            Ok(outcome) => {
                let p = a.sync.pending.remove(pos);
                if let OpOutcome::NewUid(Some(uid)) = outcome
                    && let RemoteOp::Move { dest: folder, item, .. } | RemoteOp::Append { folder, item, .. } =
                        p.op
                {
                    landed(s, account, item, folder, uid);
                }
            }
            Err(e) => {
                let Some(p) = a.sync.pending.get_mut(pos) else { return };
                p.attempts = p.attempts.saturating_add(1);
                p.last_error = Some(e.message.clone());
                p.not_before = now.saturating_add(backoff_secs(p.attempts));
                if !e.transient || p.attempts >= MAX_ATTEMPTS {
                    a.sync.pending.remove(pos);
                    log::warn!("dropping a server change for account {}: {}", account.0, e.message);
                    s.notify(Notice::Error(format!(
                        "A change could not be made on the mail server: {}",
                        e.message
                    )));
                }
            }
        }
    }

    /// A message reached server folder `folder` as `uid` (a finished move or upload of `item`). When the item
    /// was moved again or deleted while that ran (the change could not be queued against a UID nobody knew
    /// yet), the server copy follows it now: a queued upload of the item becomes a move, and a copy whose item
    /// left the account or was deleted is removed, so it is not downloaded again.
    fn landed(s: &mut Session, account: AccountId, item: ItemId, folder: String, uid: u32) {
        let here = s.mailbox.mail.get(&item).and_then(|m| synced_folder(&s.mailbox, m.folder));
        match here {
            Some((a, name)) if a == account && name == folder => {
                set_remote_id(&mut s.mailbox, item, Some(uid));
                return;
            }
            Some((a, name)) if a == account => {
                let inflight = s.sync.inflight_ops();
                if let Some(acct) = account_mut(s, account)
                    && let Some(p) = acct.sync.pending.iter_mut().find(|p| {
                        !inflight.contains(&(account, p.id))
                            && matches!(&p.op, RemoteOp::Append { item: i, folder: f, .. } if *i == item && *f == name)
                    })
                {
                    p.op = RemoteOp::Move { folder, uid, dest: name, item };
                }
                return;
            }
            _ => {}
        }
        // Deleted, or now in a local folder or another account: the server copy goes, unless its folder is
        // going away anyway.
        if find_by_remote(&s.mailbox, account, &folder).is_some() {
            push_op(&mut s.mailbox, account, RemoteOp::Expunge { folder, uid });
        }
    }

    fn message_sent(s: &mut Session, account: AccountId, item: ItemId, result: Result<(), String>) {
        if let Some(b) = s.sync.runs.get_mut(&account).and_then(|r| r.busy.as_mut()) {
            b.sending.remove(&item);
        }
        let outbox = s.mailbox.role_folder(account, FolderRole::Outbox);
        let Some(m) = s.mailbox.mail.get(&item) else { return };
        if Some(m.folder) != outbox {
            return;
        }
        let subject = if m.subject.trim().is_empty() { "(no subject)".to_owned() } else { m.subject.clone() };
        match result {
            Ok(()) => {
                let saves = s.mailbox.account(account).is_some_and(|a| a.server_saves_sent);
                let sent = s.mailbox.role_folder(account, FolderRole::SentItems);
                match sent {
                    Some(sent)
                        if !saves || s.mailbox.folder(sent).is_some_and(|f| f.remote_name.is_none()) =>
                    {
                        if let Some(m) = s.mailbox.mail.get_mut(&item) {
                            m.folder = sent;
                            m.read = true;
                            m.remote_id = None;
                        }
                    }
                    // The server files its own copy (downloaded by the next update of Sent Items).
                    _ => {
                        s.mailbox.mail.remove(&item);
                    }
                }
            }
            Err(e) => {
                s.sync.runs.entry(account).or_default().error = Some(e.clone());
                s.notify(Notice::Error(format!(
                    "The message \"{subject}\" could not be sent: {e} It stays in the Outbox, and the next \
                     send/receive tries again."
                )));
            }
        }
    }

    fn run_done(s: &mut Session, account: AccountId, error: Option<String>) {
        let now = s.now().unix;
        let run = s.sync.runs.entry(account).or_default();
        run.busy = None;
        run.progress.clear();
        match error {
            Some(e) => {
                run.error = Some(e);
                run.blocked_until = now.saturating_add(BLOCKED_RETRY_SECS.min(60));
            }
            None => {
                if run.error.is_none() {
                    run.last_sync = Some(now);
                }
                run.blocked_until = 0;
            }
        }
        if let Some(again) = s.sync.runs.get_mut(&account).and_then(|r| r.again.take())
            && let Err(e) = dispatch(s, account, again)
        {
            s.sync.runs.entry(account).or_default().error = Some(e);
        }
    }

    /// UIDVALIDITY of server folder `name` changed: queued changes aimed at its old UIDs would hit other
    /// messages now. Flag changes and removals are dropped (the folder is downloaded again); a move out of
    /// it becomes an upload of the local item to where it was moved.
    fn forget_old_uids(s: &mut Session, account: AccountId, name: &str) {
        let inflight = s.sync.inflight_ops();
        let mail = &s.mailbox.mail;
        let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == account) else { return };
        a.sync.pending.retain_mut(|p| {
            if inflight.contains(&(account, p.id)) {
                return true;
            }
            match &p.op {
                RemoteOp::Flags { folder, .. } | RemoteOp::Expunge { folder, .. } => folder != name,
                RemoteOp::Move { folder, dest, item, .. } if folder == name => match mail.get(item) {
                    Some(m) => {
                        let (dest, item) = (dest.clone(), *item);
                        p.op = RemoteOp::Append { folder: dest, item, flags: upload_flags(m) };
                        p.attempts = 0;
                        p.not_before = 0;
                        true
                    }
                    None => false,
                },
                _ => true,
            }
        });
    }

    fn find_by_remote(mb: &Mailbox, account: AccountId, name: &str) -> Option<FolderId> {
        mb.folders
            .values()
            .find(|f| {
                f.account == account && f.kind == FolderKind::Mail && f.remote_name.as_deref() == Some(name)
            })
            .map(|f| f.id)
    }

    fn apply_folders(s: &mut Session, account: AccountId, mut list: Vec<RemoteFolder>) {
        let delim = list.iter().find_map(|f| f.delimiter);
        if let Some(a) = account_mut(s, account)
            && delim.is_some()
            && a.sync.delimiter != delim
        {
            a.sync.delimiter = delim;
        }
        let d = delim.unwrap_or('/');
        list.sort_by_cached_key(|f| (f.name.matches(d).count(), f.name.clone()));
        let names: BTreeSet<String> = list.iter().map(|f| f.name.clone()).collect();
        for rf in &list {
            if find_by_remote(&s.mailbox, account, &rf.name).is_some() {
                continue;
            }
            let set_name = |s: &mut Session, fid: FolderId| {
                if let Some(f) = s.mailbox.folders.get_mut(&fid) {
                    f.remote_name = Some(rf.name.clone());
                }
            };
            if let Some(role) = rf.role
                && role != FolderRole::Outbox
                && let Some(fid) = s.mailbox.role_folder(account, role)
                && s.mailbox.folder(fid).is_some_and(|f| f.remote_name.is_none())
            {
                set_name(s, fid);
                continue;
            }
            let parent = rf.parent.as_deref().and_then(|p| find_by_remote(&s.mailbox, account, p));
            let display = local_folder_name(&rf.display_name);
            let adopt = s
                .mailbox
                .folders
                .values()
                .find(|f| {
                    f.account == account
                        && f.parent == parent
                        && f.remote_name.is_none()
                        && f.role.is_none()
                        && f.kind == FolderKind::Mail
                        && f.name.eq_ignore_ascii_case(&display)
                })
                .map(|f| f.id);
            if let Some(fid) = adopt {
                set_name(s, fid);
                continue;
            }
            let taken = |mb: &Mailbox, n: &str| {
                mb.folders
                    .values()
                    .any(|f| f.account == account && f.parent == parent && f.name.eq_ignore_ascii_case(n))
            };
            let mut name = display.clone();
            let mut k = 2u32;
            while taken(&s.mailbox, &name) && k < 1000 {
                name = format!("{display} ({k})");
                k = k.saturating_add(1);
            }
            let id = FolderId(s.mailbox.alloc_id());
            s.mailbox.folders.insert(
                id,
                Folder {
                    id,
                    account,
                    parent,
                    name,
                    kind: FolderKind::Mail,
                    remote_name: Some(rf.name.clone()),
                    ..Default::default()
                },
            );
            s.sync.track.folders_created.insert(id);
        }
        // Folders gone from the server (unless a queued change is about to create them).
        let queued: BTreeSet<String> = s
            .mailbox
            .account(account)
            .map(|a| {
                a.sync
                    .pending
                    .iter()
                    .filter_map(|p| match &p.op {
                        RemoteOp::CreateFolder { name } => Some(name.clone()),
                        RemoteOp::RenameFolder { to, .. } => Some(to.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let gone: Vec<(FolderId, bool)> = s
            .mailbox
            .folders
            .values()
            .filter(|f| f.account == account && f.kind == FolderKind::Mail)
            .filter(|f| f.remote_name.as_ref().is_some_and(|n| !names.contains(n) && !queued.contains(n)))
            .map(|f| (f.id, f.role.is_some()))
            .collect();
        let mut remove = Vec::new();
        for (fid, is_role) in gone {
            if is_role {
                if let Some(f) = s.mailbox.folders.get_mut(&fid) {
                    f.remote_name = None;
                }
                for m in s.mailbox.mail.values_mut().filter(|m| m.folder == fid) {
                    m.remote_id = None;
                }
                if let Some(a) = account_mut(s, account) {
                    a.sync.folders.remove(&fid);
                }
            } else {
                remove.push(fid);
            }
        }
        if !remove.is_empty() {
            let mut all = BTreeSet::new();
            for f in &remove {
                all.extend(s.mailbox.subtree(*f));
            }
            let items = remove_folders(&mut s.mailbox, &remove);
            s.sync.track.removed.extend(items);
            s.sync.track.folders_removed.extend(all);
        }
    }

    pub(super) fn apply_delta(
        s: &mut Session,
        account: AccountId,
        name: &str,
        delta: SyncDelta,
        new_inbox: &mut Vec<ItemId>,
    ) {
        let Some(fid) = find_by_remote(&s.mailbox, account, name) else { return };
        let now = s.now();
        let is_inbox = s.mailbox.folder(fid).is_some_and(|f| f.role == Some(FolderRole::Inbox));
        let mut by_uid: BTreeMap<u32, ItemId> = BTreeMap::new();
        for m in s.mailbox.mail.values().filter(|m| m.folder == fid) {
            if let Some(u) = uid_of(m) {
                by_uid.insert(u, m.id);
            }
        }
        if delta.reset {
            for (_, id) in std::mem::take(&mut by_uid) {
                s.mailbox.mail.remove(&id);
                s.sync.track.removed.insert(id);
            }
            forget_old_uids(s, account, name);
        }
        for uid in &delta.vanished {
            if let Some(id) = by_uid.remove(uid) {
                s.mailbox.mail.remove(&id);
                s.sync.track.removed.insert(id);
            }
        }
        // Flag changes: the server wins, except over a local change not sent yet.
        let mut newer: BTreeSet<u32> = BTreeSet::new();
        let mut failed: BTreeSet<u64> = BTreeSet::new();
        if let Some(a) = s.mailbox.account(account) {
            for p in &a.sync.pending {
                if let RemoteOp::Flags { folder, uid, .. } = &p.op
                    && folder == name
                {
                    if p.attempts == 0 {
                        newer.insert(*uid);
                    } else {
                        failed.insert(p.id);
                    }
                }
            }
        }
        if !failed.is_empty()
            && let Some(a) = account_mut(s, account)
        {
            a.sync.pending.retain(|p| !failed.contains(&p.id));
        }
        for (uid, flags) in &delta.changed {
            if newer.contains(uid) {
                continue;
            }
            if let Some(id) = by_uid.get(uid)
                && let Some(m) = s.mailbox.mail.get_mut(id)
                && apply_server_flags(m, flags, now)
            {
                s.sync.track.flags.insert(*id);
            }
        }
        for nm in delta.new {
            if by_uid.contains_key(&nm.uid) {
                continue;
            }
            let mut item = mailcraft_store::import::item_from_raw(&nm.raw, fid, now);
            // A local copy waiting for this message (uploaded without a reported UID, or the server filed a
            // sent message itself): adopt it instead of showing the message twice.
            let adopt = item.message_id.as_ref().and_then(|mid| {
                s.mailbox
                    .mail
                    .values()
                    .find(|m| m.folder == fid && m.remote_id.is_none() && m.message_id.as_ref() == Some(mid))
                    .map(|m| m.id)
            });
            if let Some(local) = adopt {
                set_remote_id(&mut s.mailbox, local, Some(nm.uid));
                if let Some(a) = account_mut(s, account) {
                    a.sync
                        .pending
                        .retain(|p| !matches!(&p.op, RemoteOp::Append { item, .. } if *item == local));
                }
                by_uid.insert(nm.uid, local);
                continue;
            }
            item.remote_id = Some(nm.uid.to_string());
            apply_server_flags(&mut item, &nm.flags, now);
            if nm.flags.iter().any(|f| f.eq_ignore_ascii_case("\\Draft")) {
                item.is_draft = true;
            }
            if let Some(size) = nm.size {
                item.size = size;
            }
            let id = s.mailbox.add_mail(item);
            s.sync.track.created.insert(id);
            by_uid.insert(nm.uid, id);
            if is_inbox {
                new_inbox.push(id);
            }
        }
        let st = mailcraft_model::account::RemoteFolderState {
            uidvalidity: delta.state.uidvalidity,
            uidnext: delta.state.uidnext,
            highest_modseq: delta.state.highest_modseq,
        };
        if let Some(a) = account_mut(s, account)
            && a.sync.folders.get(&fid) != Some(&st)
        {
            a.sync.folders.insert(fid, st);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::{Account, FolderRole};

    fn imap_mailbox() -> (Mailbox, AccountId, FolderId, FolderId) {
        let mut mb = Mailbox::new_local("Local");
        let a = mb.add_account(Account {
            kind: AccountKind::Imap,
            address: "me@example.com".into(),
            incoming: Some(Default::default()),
            ..Default::default()
        });
        mb.create_default_folders(a);
        let inbox = mb.role_folder(a, FolderRole::Inbox).unwrap();
        let trash = mb.role_folder(a, FolderRole::DeletedItems).unwrap();
        mb.folders.get_mut(&inbox).unwrap().remote_name = Some("INBOX".into());
        mb.folders.get_mut(&trash).unwrap().remote_name = Some("Trash".into());
        (mb, a, inbox, trash)
    }

    fn pending(mb: &Mailbox, a: AccountId) -> Vec<RemoteOp> {
        mb.account(a).unwrap().sync.pending.iter().map(|p| p.op.clone()).collect()
    }

    #[test]
    fn flags_moves_deletes_and_folders_queue_server_changes() {
        let (mut mb, a, inbox, trash) = imap_mailbox();
        let id = mb.add_mail(MailItem { folder: inbox, remote_id: Some("5".into()), ..Default::default() });
        let none = BTreeSet::new();
        // Read + flag.
        let before = mb.clone();
        mb.mail.get_mut(&id).unwrap().read = true;
        mb.mail.get_mut(&id).unwrap().flag =
            FlagState::Flagged { label: "x".into(), start: None, due: None, reminder: None };
        assert_eq!(queue_changes(&before, &mut mb, &none), 1);
        assert_eq!(
            pending(&mb, a)[0],
            RemoteOp::Flags {
                folder: "INBOX".into(),
                uid: 5,
                add: vec!["\\Flagged".into(), "\\Seen".into()],
                remove: vec![]
            }
        );
        // Move to Deleted Items.
        let before = mb.clone();
        mb.mail.get_mut(&id).unwrap().folder = trash;
        queue_changes(&before, &mut mb, &none);
        assert_eq!(
            pending(&mb, a)[1],
            RemoteOp::Move { folder: "INBOX".into(), uid: 5, dest: "Trash".into(), item: id }
        );
        assert!(mb.mail[&id].remote_id.is_none(), "UID cleared until the server reports the new one");
        // Moving back before the move ran cancels it and restores the UID.
        let before = mb.clone();
        mb.mail.get_mut(&id).unwrap().folder = inbox;
        queue_changes(&before, &mut mb, &none);
        assert_eq!(pending(&mb, a).len(), 1);
        assert_eq!(mb.mail[&id].remote_id.as_deref(), Some("5"));
        // Permanent delete.
        let before = mb.clone();
        mb.mail.remove(&id);
        queue_changes(&before, &mut mb, &none);
        assert_eq!(pending(&mb, a).last(), Some(&RemoteOp::Expunge { folder: "INBOX".into(), uid: 5 }));
        // New folder, rename, delete.
        let before = mb.clone();
        let f = mb.create_folder(inbox, "Projects").unwrap();
        queue_changes(&before, &mut mb, &none);
        assert_eq!(pending(&mb, a).last(), Some(&RemoteOp::CreateFolder { name: "INBOX/Projects".into() }));
        assert_eq!(mb.folders[&f].remote_name.as_deref(), Some("INBOX/Projects"));
        let before = mb.clone();
        mb.rename_folder(f, "Work").unwrap();
        queue_changes(&before, &mut mb, &none);
        assert_eq!(
            pending(&mb, a).last(),
            Some(&RemoteOp::RenameFolder { from: "INBOX/Projects".into(), to: "INBOX/Work".into() })
        );
        let before = mb.clone();
        mb.folders.remove(&f);
        queue_changes(&before, &mut mb, &none);
        assert_eq!(pending(&mb, a).last(), Some(&RemoteOp::DeleteFolder { name: "INBOX/Work".into() }));
        // A copy into a synced folder is uploaded; local-only folders queue nothing.
        let outbox = mb.role_folder(a, FolderRole::Outbox).unwrap();
        let before = mb.clone();
        let n = mb.add_mail(MailItem { folder: outbox, ..Default::default() });
        assert_eq!(queue_changes(&before, &mut mb, &none), 0);
        let before = mb.clone();
        mb.mail.get_mut(&n).unwrap().folder = inbox;
        queue_changes(&before, &mut mb, &none);
        assert!(
            matches!(pending(&mb, a).last(), Some(RemoteOp::Append { folder, item, .. }) if folder == "INBOX" && *item == n)
        );
    }

    #[test]
    fn local_accounts_queue_nothing() {
        let mut mb = Mailbox::new_local("Local");
        let before = mb.clone();
        let inbox = mb.folders.values().find(|f| f.role == Some(FolderRole::Inbox)).unwrap().id;
        mb.add_mail(MailItem { folder: inbox, ..Default::default() });
        assert_eq!(queue_changes(&before, &mut mb, &BTreeSet::new()), 0);
    }

    #[test]
    fn undo_keeps_synced_mail_and_reverts_on_server() {
        let (mb, a, inbox, trash) = imap_mailbox();
        let mut s = Session::new(mb);
        let x =
            s.mailbox.add_mail(MailItem { folder: inbox, remote_id: Some("1".into()), ..Default::default() });
        s.selection.items = vec![x];
        s.run("mail.delete", &json!({"ids": [x.0]})).unwrap();
        assert_eq!(s.mailbox.mail[&x].folder, trash);
        assert_eq!(s.mailbox.account(a).unwrap().sync.pending.len(), 1);
        // Mail arrives from the server after the command.
        let y =
            s.mailbox.add_mail(MailItem { folder: inbox, remote_id: Some("2".into()), ..Default::default() });
        s.sync.track.created.insert(y);
        s.run("app.undo", &json!({})).unwrap();
        assert_eq!(s.mailbox.mail[&x].folder, inbox);
        assert!(s.mailbox.mail.contains_key(&y), "mail that arrived meanwhile survives the undo");
        // The undone move was not sent yet: it is cancelled and the UID restored.
        assert!(s.mailbox.account(a).unwrap().sync.pending.is_empty());
        assert_eq!(s.mailbox.mail[&x].remote_id.as_deref(), Some("1"));
    }

    #[test]
    fn plan_merge_and_backoff() {
        let mut p = Plan { send: true, receive: true, folders: Some(vec![FolderId(2)]), user: false };
        p.merge(&Plan { receive: true, folders: Some(vec![FolderId(1)]), ..Default::default() });
        assert_eq!(p.folders, Some(vec![FolderId(1), FolderId(2)]));
        p.merge(&Plan::all());
        assert!(p.folders.is_none() && p.user);
        assert_eq!(backoff_secs(1), 30);
        assert_eq!(backoff_secs(2), 60);
        assert_eq!(backoff_secs(40), BACKOFF_MAX_SECS);
    }

    fn op_ids(s: &Session, a: AccountId) -> Vec<u64> {
        s.mailbox.account(a).unwrap().sync.pending.iter().map(|p| p.id).collect()
    }

    /// Marks every queued op of `a` as handed to a worker.
    fn mark_inflight(s: &mut Session, a: AccountId) {
        let ops = op_ids(s, a).into_iter().collect();
        s.sync.runs.entry(a).or_default().busy = Some(InFlight { ops, sending: BTreeSet::new() });
    }

    fn change(s: &mut Session, f: impl FnOnce(&mut Mailbox)) {
        let before = s.mailbox.clone();
        f(&mut s.mailbox);
        queue_local_changes(s, &before);
    }

    #[test]
    fn a_move_finishing_after_the_item_moved_again_follows_it() {
        let (mut mb, a, inbox, trash) = imap_mailbox();
        let archive = mb.role_folder(a, FolderRole::Archive).unwrap();
        mb.folders.get_mut(&archive).unwrap().remote_name = Some("Archive".into());
        let local = mb.folders.values().find(|f| f.account != a && f.kind == FolderKind::Mail).unwrap().id;
        let mut s = Session::new(mb);
        let mut ids = Vec::new();
        for uid in ["5", "6", "7"] {
            ids.push(s.mailbox.add_mail(MailItem {
                folder: inbox,
                remote_id: Some(uid.into()),
                ..Default::default()
            }));
        }
        let (x, y, z) = (ids[0], ids[1], ids[2]);
        // All three are moved to Archive and the moves are handed to the worker.
        change(&mut s, |mb| {
            for id in [x, y, z] {
                mb.mail.get_mut(&id).unwrap().folder = archive;
            }
        });
        let moves = op_ids(&s, a);
        assert_eq!(moves.len(), 3);
        mark_inflight(&mut s, a);
        // Meanwhile: x goes on to Deleted Items, y is deleted permanently, z goes to a local folder.
        change(&mut s, |mb| {
            mb.mail.get_mut(&x).unwrap().folder = trash;
            mb.mail.remove(&y);
            mb.mail.get_mut(&z).unwrap().folder = local;
        });
        // The moves finish with new UIDs in Archive.
        for (op, uid) in moves.iter().zip([15, 16, 17]) {
            native::op_done(&mut s, a, *op, Ok(native::OpOutcome::NewUid(Some(uid))));
        }
        let mut ops = pending(&s.mailbox, a);
        ops.sort_by_key(|o| format!("{o:?}"));
        assert_eq!(
            ops,
            vec![
                RemoteOp::Expunge { folder: "Archive".into(), uid: 16 },
                RemoteOp::Expunge { folder: "Archive".into(), uid: 17 },
                RemoteOp::Move { folder: "Archive".into(), uid: 15, dest: "Trash".into(), item: x },
            ]
        );
        assert!(s.mailbox.mail[&x].remote_id.is_none() && s.mailbox.mail[&z].remote_id.is_none());
    }

    #[test]
    fn uidvalidity_reset_drops_changes_aimed_at_old_uids() {
        let (mb, a, inbox, trash) = imap_mailbox();
        let mut s = Session::new(mb);
        let x =
            s.mailbox.add_mail(MailItem { folder: inbox, remote_id: Some("5".into()), ..Default::default() });
        let z =
            s.mailbox.add_mail(MailItem { folder: inbox, remote_id: Some("7".into()), ..Default::default() });
        let t =
            s.mailbox.add_mail(MailItem { folder: trash, remote_id: Some("3".into()), ..Default::default() });
        change(&mut s, |mb| {
            mb.mail.get_mut(&x).unwrap().read = true;
            mb.mail.get_mut(&t).unwrap().read = true;
            mb.mail.get_mut(&z).unwrap().folder = trash;
        });
        assert_eq!(pending(&s.mailbox, a).len(), 3);
        let raw = b"From: a@example.com\r\nSubject: Fresh\r\nMessage-ID: <fresh@example.com>\r\n\r\nHi\r\n"
            .to_vec();
        let delta = mailcraft_net::sync::SyncDelta {
            reset: true,
            new: vec![mailcraft_net::sync::NewMessage {
                uid: 1,
                raw,
                flags: vec![],
                internal_date: None,
                size: None,
            }],
            state: mailcraft_net::sync::FolderState { uidvalidity: 99, uidnext: 2, ..Default::default() },
            ..Default::default()
        };
        let mut new_inbox = Vec::new();
        native::apply_delta(&mut s, a, "INBOX", delta, &mut new_inbox);
        assert!(!s.mailbox.mail.contains_key(&x), "old copies are discarded");
        assert_eq!(new_inbox.len(), 1);
        assert_eq!(s.mailbox.mail[&new_inbox[0]].remote_id.as_deref(), Some("1"));
        // The flag change on an old INBOX UID is dropped; the moved message is uploaded to its folder
        // instead of moving whatever now has its old UID; Deleted Items is untouched.
        let ops = pending(&s.mailbox, a);
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(ops.contains(&RemoteOp::Flags {
            folder: "Trash".into(),
            uid: 3,
            add: vec!["\\Seen".into()],
            remove: vec![]
        }));
        assert!(
            ops.iter().any(
                |o| matches!(o, RemoteOp::Append { folder, item, .. } if folder == "Trash" && *item == z)
            )
        );
        assert_eq!(s.mailbox.account(a).unwrap().sync.folders[&inbox].uidvalidity, 99);
    }

    #[test]
    fn op_ids_of_different_accounts_never_mix() {
        let (mut mb, a, _, _) = imap_mailbox();
        let b = mb.add_account(Account {
            kind: AccountKind::Imap,
            address: "other@example.org".into(),
            incoming: Some(Default::default()),
            ..Default::default()
        });
        mb.create_default_folders(b);
        let b_inbox = mb.role_folder(b, FolderRole::Inbox).unwrap();
        mb.folders.get_mut(&b_inbox).unwrap().remote_name = Some("INBOX".into());
        let gone = ItemId(9_999);
        push_op(&mut mb, a, RemoteOp::Append { folder: "INBOX".into(), item: gone, flags: vec![] });
        push_op(&mut mb, b, RemoteOp::Flags { folder: "INBOX".into(), uid: 4, add: vec![], remove: vec![] });
        assert_eq!(mb.account(a).unwrap().sync.pending[0].id, mb.account(b).unwrap().sync.pending[0].id);
        native::prune_stale_ops(&mut mb, &Inflight::new());
        assert!(pending(&mb, a).is_empty(), "the upload of a deleted item is dropped");
        assert_eq!(pending(&mb, b).len(), 1, "the other account's change with the same id stays");
        // An op of account `a` in flight does not freeze account `b`'s queued op with the same id.
        let b_trash = mb.role_folder(b, FolderRole::DeletedItems).unwrap();
        mb.folders.get_mut(&b_trash).unwrap().remote_name = Some("Trash".into());
        let mut s = Session::new(mb);
        let x = s.mailbox.add_mail(MailItem {
            folder: b_inbox,
            remote_id: Some("8".into()),
            ..Default::default()
        });
        change(&mut s, |mb| mb.mail.get_mut(&x).unwrap().folder = b_trash);
        push_op(&mut s.mailbox, a, RemoteOp::CreateFolder { name: "Z".into() });
        mark_inflight(&mut s, a);
        assert!(op_ids(&s, a).contains(&op_ids(&s, b)[1]), "the test needs colliding ids");
        // Moving it back before the move ran cancels the move and restores the UID.
        change(&mut s, |mb| mb.mail.get_mut(&x).unwrap().folder = b_inbox);
        assert_eq!(pending(&s.mailbox, b).len(), 1, "{:?}", pending(&s.mailbox, b));
        assert_eq!(s.mailbox.mail[&x].remote_id.as_deref(), Some("8"));
    }

    #[test]
    fn retry_delays_keep_the_queue_in_order() {
        let (mb, a, inbox, _) = imap_mailbox();
        let mut s = Session::new(mb);
        let x =
            s.mailbox.add_mail(MailItem { folder: inbox, remote_id: Some("5".into()), ..Default::default() });
        change(&mut s, |mb| mb.mail.get_mut(&x).unwrap().read = true);
        change(&mut s, |mb| mb.mail.get_mut(&x).unwrap().read = false);
        let now = 1_000;
        if let Some(p) = s.mailbox.accounts.iter_mut().find(|x| x.id == a).unwrap().sync.pending.first_mut() {
            p.attempts = 3;
            p.not_before = now + 100;
        }
        let acct = s.mailbox.account(a).unwrap().clone();
        assert!(
            !has_due_ops(&s.mailbox, &acct, now),
            "a later change must not overtake one waiting to retry"
        );
        let auto = Plan::default();
        assert!(job_ops(&s, &acct, &auto, now).is_empty());
        let user = Plan { user: true, ..Plan::default() };
        assert_eq!(job_ops(&s, &acct, &user, now).len(), 2);
        assert!(has_due_ops(&s.mailbox, &acct, now + 100));
    }

    #[test]
    fn server_flags_win() {
        let mut m = MailItem { read: true, ..Default::default() };
        let now = mailcraft_model::DateTime::from_unix(0, 0);
        assert!(apply_server_flags(&mut m, &["\\Flagged".into(), "\\Answered".into()], now));
        assert!(!m.read && m.is_flagged() && matches!(m.response, ResponseState::Replied(_)));
        assert!(!apply_server_flags(&mut m, &["\\Flagged".into()], now));
    }
}
