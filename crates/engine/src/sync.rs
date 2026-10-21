//! Send/receive (`plan/reference/ribbon-mail.md` Send / Receive, `reading-pane.md` sending, `folders.md`).
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
/// Settings for `accounts.discover` and the server connections (for front ends and tests that configure them).
#[cfg(not(target_arch = "wasm32"))]
pub use mailcraft_net::ConnectOptions;
#[cfg(not(target_arch = "wasm32"))]
pub use mailcraft_net::discover::{DiscoverOptions, ResolverChoice};

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
pub(crate) const MAX_PENDING: usize = 50_000;
/// Below this measured transfer rate (bytes per second; 128 kbit/s) a connection counts as slow for
/// Download Preferences › On Slow Connections Download Only Headers.
pub const SLOW_LINK_BYTES_PER_SEC: u64 = 16_000;
/// How long a transfer-rate measurement counts. A link found slow downloads headers only, which rarely
/// measures it again; after this long the next run downloads normally and measures afresh.
const LINK_SPEED_TTL_SECS: i64 = 3600;
/// Address lookups (`accounts.discover`) remembered per session.
const MAX_DISCOVERIES: usize = 16;

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
    /// What the worker is doing ("Updating Inbox…", "Downloading Inbox: 1,240 of 5,000 messages").
    pub progress: String,
    /// The same as numbers (folder and message counts) while the worker downloads or updates folders.
    pub detail: Option<SyncProgress>,
    pub error: Option<String>,
    /// Unix time of the last send/receive that finished without error.
    pub last_sync: Option<i64>,
    pub pending_changes: usize,
    pub outbox: usize,
    /// New-mail push (IMAP IDLE, JMAP event source) state.
    pub push: PushMode,
    /// The last measured transfer rate was below the slow-connection threshold.
    pub slow_connection: bool,
}

/// How far a send/receive run has got: the folder it updates (its place among the folders of the run) and
/// the messages downloaded so far out of those the server has. Front ends draw bars and counts from it; its
/// [`SyncProgress::text`] is the status text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    /// Display name of the folder ("Inbox"); empty when the step covers the whole account (JMAP).
    pub folder: String,
    /// 1-based position of the folder among the `folder_count` folders this run updates (0 = not counted).
    pub folder_index: usize,
    pub folder_count: usize,
    /// New messages downloaded so far and how many there are (`total` 0 = none or not known yet).
    pub done: u64,
    pub total: u64,
    /// Only the headers of new messages are downloaded.
    pub headers: bool,
}

impl SyncProgress {
    /// "Downloading Inbox: 1,240 of 5,000 messages (folder 3 of 12)", "Updating Archive (folder 4 of 12)…",
    /// "Downloading 300 of 2,000 messages".
    pub fn text(&self) -> String {
        let of_folders = if self.folder_count > 1 && self.folder_index > 0 {
            format!(" (folder {} of {})", self.folder_index, self.folder_count)
        } else {
            String::new()
        };
        let what = if self.headers { "headers" } else { "messages" };
        let done = grouped(self.done.min(self.total));
        if self.total > 0 && self.folder.is_empty() {
            format!("Downloading {done} of {} {what}{of_folders}", grouped(self.total))
        } else if self.total > 0 {
            format!("Downloading {}: {done} of {} {what}{of_folders}", self.folder, grouped(self.total))
        } else if self.folder.is_empty() {
            "Updating messages…".into()
        } else {
            format!("Updating {}{of_folders}…", self.folder)
        }
    }

    /// Share of the messages downloaded (0..=1), when the total is known.
    pub fn fraction(&self) -> Option<f32> {
        if self.total == 0 {
            return None;
        }
        // Display only: the precision lost converting large counts does not matter.
        #[allow(clippy::cast_precision_loss)]
        let f = self.done.min(self.total) as f64 / self.total as f64;
        #[allow(clippy::cast_possible_truncation)]
        Some(f as f32)
    }
}

/// `12345` → `"12,345"` (the separators the status bar uses for its counts).
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && digits.len().saturating_sub(i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Whether new mail is pushed to an account (IMAP IDLE, JMAP event source) or found by polling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PushMode {
    /// No push: mail arrives with scheduled or manual send/receive.
    #[default]
    Off,
    /// Connected and waiting for the server to report new mail.
    Idle,
    /// The server cannot push; the Inbox is checked every few minutes instead.
    Polling,
    /// The push connection was lost; reconnecting with increasing delays (the Inbox is polled meanwhile).
    Reconnecting,
}

impl PushMode {
    pub fn label(self) -> &'static str {
        match self {
            PushMode::Off => "",
            PushMode::Idle => "Waiting for new mail",
            PushMode::Polling => "Checking for new mail every few minutes",
            PushMode::Reconnecting => "Reconnecting",
        }
    }
}

/// An OAuth sign-in started by `accounts.oauthSignIn`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OAuthFlow {
    pub provider: String,
    /// "device" or "browser".
    pub flow: String,
    /// "starting", "waiting", "done", "failed" or "cancelled".
    pub status: String,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub verification_uri_complete: Option<String>,
    /// Browser flow: the page to open.
    pub auth_url: Option<String>,
    pub error: Option<String>,
}

impl OAuthFlow {
    pub fn to_json(&self, account: AccountId) -> Value {
        json!({
            "account": account.0,
            "provider": self.provider,
            "flow": self.flow,
            "status": self.status,
            "userCode": self.user_code,
            "verificationUri": self.verification_uri,
            "verificationUriComplete": self.verification_uri_complete,
            "authUrl": self.auth_url,
            "error": self.error,
        })
    }
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
    /// Folder and message counts behind `text` while an account downloads (see [`SyncProgress`]).
    pub detail: Option<SyncProgress>,
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
    detail: Option<SyncProgress>,
    error: Option<String>,
    last_sync: Option<i64>,
    /// Automatic runs wait until then after a run could not start.
    blocked_until: i64,
    /// Push state reported by the worker.
    push: PushMode,
    push_error: Option<String>,
    /// A `Push` message asking the worker to IDLE was sent (and not withdrawn).
    push_on: bool,
    /// Last scheduled send/receive (per account: each has its own interval).
    last_auto: Option<i64>,
    /// Last fallback Inbox check (push not available).
    last_poll: Option<i64>,
    /// Transfer rate measured by the last run that downloaded enough to tell (bytes per second), and when
    /// (Unix time).
    link_speed: Option<(u64, i64)>,
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
    /// Something happened since the status was last computed.
    dirty: bool,
    pub(crate) track: Track,
    /// Account options and bookkeeping (`account-options.json`, see `accounts.rs`).
    pub(crate) prefs: crate::accounts::PrefsSlot,
    /// Marked items to download in full with the account's next run.
    pub(crate) fetch: BTreeMap<AccountId, Vec<ItemId>>,
    /// Accounts whose next run downloads new messages as headers only (Download Headers).
    pub(crate) force_headers: BTreeSet<AccountId>,
    /// JMAP accounts with local changes not sent yet (they have no change queue; see `sync/jmap.rs`).
    pub(crate) jmap_dirty: BTreeSet<AccountId>,
    /// OAuth sign-ins started in this session.
    pub oauth: BTreeMap<AccountId, OAuthFlow>,
    /// HTTP options for OAuth endpoints (`None` = defaults). Tests use it to trust a local server.
    #[cfg(not(target_arch = "wasm32"))]
    pub oauth_http: Option<mailcraft_net::ConnectOptions>,
    /// How `accounts.discover` looks addresses up (system resolver and the autoconfig locations by
    /// default; tests point it at local servers).
    #[cfg(not(target_arch = "wasm32"))]
    pub discovery: mailcraft_net::discover::DiscoverOptions,
    /// Address lookups started by `accounts.discover`.
    #[cfg(not(target_arch = "wasm32"))]
    discoveries: BTreeMap<String, Discovering>,
    /// The slow-connection threshold in bytes per second (`None` = [`SLOW_LINK_BYTES_PER_SEC`]).
    pub slow_link_threshold: Option<u64>,
    /// Newly arrived mail not looked at yet (the new-mail envelope, see `new_mail.rs`).
    pub(crate) new_mail: BTreeSet<ItemId>,
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
        crate::accounts::load_prefs(self);
        let accounts = group_members(self, accounts, &plan);
        let mut started = 0usize;
        let mut last_err = None;
        for id in &accounts {
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

    /// The push (IDLE) state of an account and the last push error.
    pub fn push_state(&self, id: AccountId) -> (PushMode, Option<String>) {
        self.sync.runs.get(&id).map(|r| (r.push, r.push_error.clone())).unwrap_or_default()
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

/// The transfer rate last measured for an account (bytes per second), if a run in the last hour downloaded
/// enough to tell.
pub fn link_speed(s: &Session, id: AccountId) -> Option<u64> {
    let (rate, at) = s.sync.runs.get(&id).and_then(|r| r.link_speed)?;
    (s.now().unix.saturating_sub(at) < LINK_SPEED_TTL_SECS).then_some(rate)
}

/// True when the account's last measured transfer rate is below the slow-connection threshold.
pub fn slow_connection(s: &Session, id: AccountId) -> bool {
    let threshold = s.sync.slow_link_threshold.unwrap_or(SLOW_LINK_BYTES_PER_SEC);
    link_speed(s, id).is_some_and(|b| b < threshold)
}

/// Download Preferences › On Slow Connections Download Only Headers applies to the account's next run.
pub(crate) fn slow_headers_only(s: &Session, id: AccountId) -> bool {
    crate::accounts::account_options(s, id).slow_headers_only && slow_connection(s, id)
}

/// Server accounts that take part in send/receive.
pub(crate) fn server_accounts(s: &Session) -> Vec<AccountId> {
    s.mailbox.accounts.iter().filter(|a| is_server_account(a.kind)).map(|a| a.id).collect()
}

/// Send/Receive Groups: a run of the whole "All Accounts" group (every server account, all folders) skips
/// the accounts taken out of the group (`accounts.options` `include: false`); a run naming accounts or folders
/// runs them as asked.
fn group_members(s: &Session, accounts: &[AccountId], plan: &Plan) -> Vec<AccountId> {
    let all: BTreeSet<AccountId> = server_accounts(s).into_iter().collect();
    let asked: BTreeSet<AccountId> = accounts.iter().copied().collect();
    if plan.folders.is_some() || asked != all {
        return accounts.to_vec();
    }
    accounts
        .iter()
        .copied()
        .filter(|a| crate::accounts::account_options(s, *a).include_in_send_receive)
        .collect()
}

/// The accounts in the "All Accounts" Send/Receive group.
pub fn group_accounts(s: &Session) -> Vec<AccountId> {
    server_accounts(s)
        .into_iter()
        .filter(|a| crate::accounts::account_options(s, *a).include_in_send_receive)
        .collect()
}

/// Account options changed: the status and push follow on the next poll.
pub(crate) fn options_changed(s: &mut Session, id: AccountId) {
    if let Some(r) = s.sync.runs.get_mut(&id) {
        r.last_auto = None;
    }
    s.sync.dirty = true;
}

/// After `compose.send`: sends the Outbox of `account` now when "Send immediately when connected" is on.
/// Problems only show in the status (the message waits in the Outbox either way).
pub(crate) fn send_now(s: &mut Session, account: AccountId) {
    if s.view.work_offline || !s.view.send_immediately {
        return;
    }
    if s.mailbox.account(account).is_none_or(|a| !is_server_account(a.kind)) {
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
    s.sync.fetch.remove(&id);
    s.sync.force_headers.remove(&id);
    s.sync.oauth.remove(&id);
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
            detail: run.filter(|_| busy).and_then(|r| r.detail.clone()),
            error: run.and_then(|r| r.error.clone()),
            last_sync: run.and_then(|r| r.last_sync),
            pending_changes: a.sync.pending.len(),
            outbox,
            push: run.map(|r| r.push).unwrap_or_default(),
            slow_connection: slow_connection(s, a.id),
        };
        if busy && busy_text.is_none() {
            busy_text = Some(row.progress.clone());
            st.detail = row.detail.clone();
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
        st.detail = None;
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

/// Starts automatic runs: every N minutes (each account's Send/Receive group interval, else Options ›
/// Advanced), for queued server changes that are due, and Inbox checks of accounts whose server cannot
/// push; starts and stops push (IMAP IDLE) as the options and Work Offline ask.
fn tick(s: &mut Session) {
    crate::accounts::load_prefs(s);
    manage_push(s);
    if s.view.work_offline {
        return;
    }
    let accounts: Vec<AccountId> = s
        .mailbox
        .accounts
        .iter()
        .filter(|a| is_server_account(a.kind) && a.incoming.is_some())
        .map(|a| a.id)
        .collect();
    if accounts.is_empty() {
        return;
    }
    let now = s.now().unix;
    let global = s.view.auto_send_receive_minutes;
    for id in accounts {
        let opts = crate::accounts::account_options(s, id);
        let every = i64::from(opts.interval_minutes.unwrap_or(global)).saturating_mul(60);
        let poll_every = i64::from(opts.poll_minutes.max(1)).saturating_mul(60);
        let run = s.sync.runs.entry(id).or_default();
        let mut auto = false;
        if every > 0 && opts.include_in_send_receive {
            match run.last_auto {
                None => run.last_auto = Some(now),
                Some(t) if now.saturating_sub(t) >= every => {
                    run.last_auto = Some(now);
                    auto = true;
                }
                Some(_) => {}
            }
        }
        // Fallback polling: push wanted but the server cannot (or the connection is being re-made).
        let mut poll = false;
        if opts.push && matches!(run.push, PushMode::Polling | PushMode::Reconnecting) {
            match run.last_poll {
                None => run.last_poll = Some(now),
                Some(t) if now.saturating_sub(t) >= poll_every => {
                    run.last_poll = Some(now);
                    poll = true;
                }
                Some(_) => {}
            }
        }
        if s.sync.busy(id) || s.sync.runs.get(&id).is_some_and(|r| r.blocked_until > now) {
            continue;
        }
        let due = s.mailbox.account(id).is_some_and(|a| has_due_ops(&s.mailbox, a, now))
            || s.sync.jmap_dirty.remove(&id);
        let plan = if auto {
            Plan { send: true, receive: true, folders: None, user: false }
        } else if poll {
            let inbox = s.mailbox.role_folder(id, mailcraft_model::FolderRole::Inbox);
            Plan { send: false, receive: true, folders: inbox.map(|f| vec![f]), user: false }
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

/// Starts push (IDLE on the Inbox for IMAP, the event source for JMAP) on accounts that want it and have
/// connected once, and stops it when offline or no longer wanted.
fn manage_push(s: &mut Session) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // JMAP: the worker listens on the server's event source (RFC 8620 section 7.3) between jobs.
        let jmap: Vec<AccountId> = s
            .mailbox
            .accounts
            .iter()
            .filter(|a| a.kind == AccountKind::Jmap && a.incoming.is_some())
            .map(|a| a.id)
            .collect();
        for id in jmap {
            let opts = crate::accounts::account_options(s, id);
            let has_worker = s.sync.rt.has_worker(id);
            let Some(run) = s.sync.runs.get(&id) else { continue };
            let want = !s.view.work_offline
                && opts.push
                && has_worker
                && run.last_sync.is_some()
                && run.error.is_none();
            if want == run.push_on {
                continue;
            }
            let target = if want { native::jmap_push_target(s, id) } else { None };
            if want && target.is_none() {
                continue;
            }
            let sent = s.sync.rt.push(id, target);
            if let Some(r) = s.sync.runs.get_mut(&id) {
                r.push_on = want && sent;
                if !want {
                    r.push = PushMode::Off;
                }
            }
            s.sync.dirty = true;
        }
        let ids: Vec<AccountId> = s
            .mailbox
            .accounts
            .iter()
            .filter(|a| a.kind == AccountKind::Imap && a.incoming.is_some())
            .map(|a| a.id)
            .collect();
        for id in ids {
            let opts = crate::accounts::account_options(s, id);
            let has_worker = s.sync.rt.has_worker(id);
            let Some(run) = s.sync.runs.get(&id) else { continue };
            let want = !s.view.work_offline
                && opts.push
                && !opts.pop3
                && has_worker
                && run.last_sync.is_some()
                && run.error.is_none();
            if want != run.push_on {
                let target = want.then(|| native::PushTarget::Imap("INBOX".to_owned()));
                let sent = s.sync.rt.push(id, target);
                if let Some(r) = s.sync.runs.get_mut(&id) {
                    r.push_on = want && sent;
                    if !want {
                        r.push = PushMode::Off;
                    }
                }
                s.sync.dirty = true;
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = s;
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

/// The account whose server change queue moving or deleting `m` feeds (`None` for mail no server holds).
pub(crate) fn queue_account(mb: &Mailbox, m: &MailItem) -> Option<AccountId> {
    uid_of(m)?;
    synced_folder(mb, m.folder).map(|(a, _)| a)
}

/// The account holding `folder` on its server (`None` for local folders).
pub(crate) fn synced_account(mb: &Mailbox, folder: FolderId) -> Option<AccountId> {
    synced_folder(mb, folder).map(|(a, _)| a)
}

/// Server changes queued for `account`.
pub(crate) fn pending_len(mb: &Mailbox, account: AccountId) -> usize {
    mb.account(account).map_or(0, |a| a.sync.pending.len())
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
    crate::accounts::load_prefs(s);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let jmap: Vec<AccountId> =
            s.mailbox.accounts.iter().filter(|a| a.kind == AccountKind::Jmap).map(|a| a.id).collect();
        for a in jmap {
            if jmap::changed(before, &s.mailbox, a) {
                s.sync.jmap_dirty.insert(a);
            }
        }
    }
    let inflight = s.sync.inflight_ops();
    let pop3: BTreeSet<AccountId> =
        s.mailbox.accounts.iter().filter(|a| crate::accounts::is_pop3(s, a.id)).map(|a| a.id).collect();
    queue_changes_except(before, &mut s.mailbox, &inflight, &pop3)
}

/// Compares `before` and `after` for IMAP accounts and appends [`RemoteOp`]s to the accounts' queues.
/// Adjusts `after` as the server will see it: server names of new/renamed folders, cleared UIDs of moved or
/// copied items, and queued (not yet dispatched) operations that a later change made moot. Returns how many
/// operations were queued.
#[cfg(test)]
pub(crate) fn queue_changes(before: &Mailbox, after: &mut Mailbox, inflight: &Inflight) -> usize {
    queue_changes_except(before, after, inflight, &BTreeSet::new())
}

/// [`queue_changes`] leaving out `skip` (POP3 accounts: their folders are local).
pub(crate) fn queue_changes_except(
    before: &Mailbox,
    after: &mut Mailbox,
    inflight: &Inflight,
    skip: &BTreeSet<AccountId>,
) -> usize {
    if !after.accounts.iter().any(|a| a.kind == AccountKind::Imap && !skip.contains(&a.id)) {
        return 0;
    }
    let mut q: Vec<(AccountId, RemoteOp)> = Vec::new();
    folder_changes(before, after, skip, &mut q);
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

fn folder_changes(
    before: &Mailbox,
    after: &mut Mailbox,
    skip: &BTreeSet<AccountId>,
    q: &mut Vec<(AccountId, RemoteOp)>,
) {
    let mut ids: Vec<FolderId> = after
        .folders
        .values()
        .filter(|f| {
            f.kind == FolderKind::Mail
                && f.role.is_none()
                && is_imap(after, f.account)
                && !skip.contains(&f.account)
        })
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
                && !skip.contains(&b.account)
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
    // Internet calendars keep their last refresh (refreshes are not undo steps).
    crate::cmd::calendar::subscribe::carry_over(live, &mut s.mailbox);
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
    // Server names of folders present in both (calendar folders keep their properties there instead, which
    // undo restores: see `cmd::calendar::props`).
    let names: Vec<(FolderId, Option<String>)> = restored
        .folders
        .keys()
        .filter_map(|id| live.folders.get(id).map(|l| (*id, l)))
        .filter(|(_, l)| l.kind != FolderKind::Calendar)
        .map(|(id, l)| (id, l.remote_name.clone()))
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

pub mod arrival;
pub mod cmds;
pub mod focus;
#[cfg(not(target_arch = "wasm32"))]
mod jmap;
#[cfg(not(target_arch = "wasm32"))]
mod native;

// =======================================================================================================
// OAuth sign-in
// =======================================================================================================

/// Starts the OAuth sign-in of `id` with the registration stored in its options (`device`: RFC 8628 device
/// flow, else the browser flow with PKCE and a loopback redirect). Returns the sign-in state as JSON.
pub(crate) fn start_oauth(
    s: &mut Session,
    id: AccountId,
    address: &str,
    device: bool,
) -> Result<Value, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        native::start_oauth(s, id, address, device)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, id, address, device);
        Err("OAuth sign-in is not available in the browser.".into())
    }
}

/// The state of an account's OAuth sign-in (`status: "none"` when none was started).
pub(crate) fn oauth_status(s: &Session, id: AccountId) -> Value {
    match s.sync.oauth.get(&id) {
        Some(f) => f.to_json(id),
        None => json!({"account": id.0, "status": "none"}),
    }
}

/// Cancels a running OAuth sign-in; true when one was running.
pub(crate) fn cancel_oauth(s: &mut Session, id: AccountId) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        native::cancel_oauth(s, id)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, id);
        false
    }
}

// =======================================================================================================
// Server settings lookup (accounts.discover)
// =======================================================================================================

/// One address lookup.
#[cfg(not(target_arch = "wasm32"))]
enum Discovering {
    Running(std::sync::mpsc::Receiver<Value>),
    Done(Value),
}

#[cfg(not(target_arch = "wasm32"))]
fn discovery_json(d: &mailcraft_net::discover::Discovery) -> Value {
    use mailcraft_net::autoconfig::ServerConfig;
    let server = |c: &Option<ServerConfig>| {
        c.as_ref().map(|c| {
            json!({
                "host": c.host,
                "port": c.port,
                "security": match c.security {
                    mailcraft_model::Security::None => "none",
                    mailcraft_model::Security::StartTls => "starttls",
                    mailcraft_model::Security::Tls => "tls",
                },
                "username": c.username,
                "verified": c.verified,
                "auth": c.auth,
                "oauth": c.offers_oauth(),
                "oauthOnly": c.oauth_only(),
            })
        })
    };
    json!({
        "status": "done",
        "address": d.address,
        "domain": d.domain,
        "found": d.found_anything(),
        "sources": d.sources,
        "displayName": d.display_name,
        "imap": server(&d.imap),
        "pop3": server(&d.pop3),
        "smtp": server(&d.smtp),
        "jmap": d.jmap.as_ref().map(|j| json!({"url": j.host, "verified": j.verified})),
        "notes": d.notes,
    })
}

/// Finds the server settings of `address` (autoconfig on the mail domain, then DNS SRV records; see
/// `mailcraft_net::discover`). The lookup runs on its own thread: the first call starts it and reports
/// `"status": "running"`, later calls with the same address report the result (`wait` blocks for it, at most
/// a minute). Results are remembered for the session; `refresh` looks up again. Lookups that need no network
/// (reserved example domains) finish at once.
pub(crate) fn discover(s: &mut Session, address: &str, wait: bool, refresh: bool) -> Result<Value, CmdError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use mailcraft_net::discover::{self as nd, ResolverChoice};
        let key = address.trim().to_ascii_lowercase();
        let domain = nd::address_domain(&key).map_err(|e| CmdError::Params(e.to_string()))?;
        if refresh {
            s.sync.discoveries.remove(&key);
        }
        if !s.sync.discoveries.contains_key(&key) {
            let opts = s.sync.discovery.clone();
            let offline = (nd::is_reserved_domain(&domain) && !opts.allow_reserved)
                || (opts.resolver == ResolverChoice::Off && opts.autoconfig_urls.is_empty());
            let slot = if offline {
                let d = nd::discover(address, &opts).map_err(|e| CmdError::Params(e.to_string()))?;
                Discovering::Done(discovery_json(&d))
            } else {
                let (tx, rx) = std::sync::mpsc::channel();
                let addr = address.trim().to_owned();
                std::thread::Builder::new()
                    .name("mailcraft-discover".into())
                    .spawn(move || {
                        let r = std::panic::catch_unwind(|| nd::discover(&addr, &opts));
                        let v = match r {
                            Ok(Ok(d)) => discovery_json(&d),
                            Ok(Err(e)) => json!({"status": "done", "found": false, "error": e.to_string()}),
                            Err(_) => json!({"status": "done", "found": false, "error": "internal error"}),
                        };
                        let _ = tx.send(v);
                    })
                    .map_err(|e| CmdError::Failed(format!("cannot start the lookup: {e}")))?;
                Discovering::Running(rx)
            };
            while s.sync.discoveries.len() >= MAX_DISCOVERIES {
                let Some(k) = s.sync.discoveries.keys().next().cloned() else { break };
                s.sync.discoveries.remove(&k);
            }
            s.sync.discoveries.insert(key.clone(), slot);
        }
        let Some(slot) = s.sync.discoveries.get_mut(&key) else {
            return Err(CmdError::Failed("the lookup was lost".into()));
        };
        if let Discovering::Running(rx) = slot {
            let got = if wait {
                rx.recv_timeout(std::time::Duration::from_secs(60)).ok()
            } else {
                rx.try_recv().ok()
            };
            match got {
                Some(v) => *slot = Discovering::Done(v),
                None if wait => {
                    s.sync.discoveries.remove(&key);
                    return Err(CmdError::Failed("the lookup did not finish in time".into()));
                }
                None => {
                    // A thread that died without answering leaves a disconnected channel: report nothing found.
                    if matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected)) {
                        *slot = Discovering::Done(json!({"status": "done", "found": false}));
                    }
                }
            }
        }
        Ok(match slot {
            Discovering::Done(v) => v.clone(),
            Discovering::Running(_) => json!({"status": "running", "address": address.trim()}),
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, wait, refresh);
        Ok(json!({"status": "done", "address": address.trim(), "found": false,
            "notes": ["Server settings cannot be looked up in the browser."]}))
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
                headers_only: false,
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

    #[test]
    fn discover_without_network_finishes_at_once_and_is_bounded() {
        let mut s = Session::new(Mailbox::new_local("Data"));
        // Reserved example domains never reach the network, whatever the options.
        let v = discover(&mut s, "kim@example.com", false, false).unwrap();
        assert_eq!((v["status"].as_str(), v["found"].as_bool()), (Some("done"), Some(false)));
        assert!(v["notes"][0].as_str().unwrap().contains("reserved"));
        s.sync.discovery = mailcraft_net::discover::DiscoverOptions {
            allow_reserved: true,
            ..mailcraft_net::discover::DiscoverOptions::offline()
        };
        for i in 0..(MAX_DISCOVERIES + 5) {
            let v = discover(&mut s, &format!("u{i}@x{i}.example"), false, true).unwrap();
            assert_eq!(v["status"], "done");
        }
        assert_eq!(s.sync.discoveries.len(), MAX_DISCOVERIES);
        assert!(matches!(discover(&mut s, "no-domain", false, false), Err(CmdError::Params(_))));
    }

    #[test]
    fn slow_connection_follows_the_measurement_and_the_option() {
        let (mb, a, _, _) = imap_mailbox();
        let mut s = Session::new(mb);
        assert!(!slow_connection(&s, a), "nothing measured yet");
        let now = s.now().unix;
        s.sync.runs.entry(a).or_default().link_speed = Some((SLOW_LINK_BYTES_PER_SEC - 1, now));
        assert!(slow_connection(&s, a));
        assert!(slow_headers_only(&s, a), "on by default");
        crate::accounts::update_options(&mut s, a, |o| o.slow_headers_only = false);
        assert!(!slow_headers_only(&s, a));
        s.sync.runs.entry(a).or_default().link_speed = Some((SLOW_LINK_BYTES_PER_SEC, now));
        assert!(!slow_connection(&s, a));
        s.sync.slow_link_threshold = Some(u64::MAX);
        assert!(slow_connection(&s, a));
        refresh_status(&mut s);
        assert!(s.sync.status.accounts.iter().any(|r| r.slow_connection));
        // An old measurement no longer counts: the next run downloads normally and measures again.
        s.sync.runs.entry(a).or_default().link_speed = Some((1, now - LINK_SPEED_TTL_SECS));
        assert!(!slow_connection(&s, a) && link_speed(&s, a).is_none());
    }
}
