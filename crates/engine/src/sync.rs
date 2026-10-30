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
//! **Undo.** Undo puts back what its step changed (everything but the mail, and the mail items the step
//! wrote); [`after_restore`] carries the synchronised state (queue, folder states, mail that arrived or vanished
//! meanwhile) into the items put back and queues the server changes that revert the undone command.

// The browser build has no worker threads: the code that applies their results is unused there.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use serde::Serialize;
use serde_json::{Value, json};

use crate::{CmdError, Notice, Session};
use mailcraft_model::account::{PendingOp, RemoteOp};
use mailcraft_model::{
    AccountId, AccountKind, FlagState, Folder, FolderId, FolderKind, ItemId, MailChanges, MailItem, Mailbox,
    ResponseState, ScanFilter,
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

/// A server mailbox's storage quota (IMAP QUOTA, RFC 9208; JMAP Quota, RFC 9425), as last reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxQuota {
    /// Bytes in use.
    pub used: u64,
    /// Bytes allowed (0 = only a message-count limit).
    pub limit: u64,
    /// Messages in use and allowed, when the server limits those.
    pub messages: Option<(u64, u64)>,
    /// When the server reported it (Unix seconds).
    pub checked: i64,
}

impl MailboxQuota {
    /// Share of the limit in use, 0–100 (storage, else messages); `None` without a limit.
    pub fn percent(&self) -> Option<u8> {
        let (used, limit) = if self.limit > 0 { (self.used, self.limit) } else { self.messages? };
        if limit == 0 {
            return None;
        }
        let pct = u128::from(used).saturating_mul(100) / u128::from(limit);
        Some(u8::try_from(pct.min(100)).unwrap_or(100))
    }

    /// "1.4 MB of 100.0 MB used (2%)" / "120 of 1,000 messages (12%)".
    pub fn text(&self) -> String {
        let pct = self.percent().map(|p| format!(" ({p}%)")).unwrap_or_default();
        if self.limit > 0 {
            format!(
                "{} of {} used{pct}",
                crate::queries::size_text(self.used),
                crate::queries::size_text(self.limit)
            )
        } else if let Some((u, l)) = self.messages {
            format!("{} of {} messages{pct}", grouped(u), grouped(l))
        } else {
            String::new()
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
    /// The mailbox quota the server reported on the last send/receive (`None` = no limit or not known).
    pub quota: Option<MailboxQuota>,
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
    /// Contacts and tasks the contacts/tasks server added or removed.
    pim_created: BTreeSet<ItemId>,
    pim_removed: BTreeSet<ItemId>,
    /// JMAP mailboxes (account, mailbox id) removed from the server meanwhile: a restored folder still
    /// naming one is made there again.
    mailboxes_gone: BTreeSet<(AccountId, String)>,
}

impl Track {
    fn is_empty(&self) -> bool {
        self.pim_created.is_empty()
            && self.pim_removed.is_empty()
            && self.created.is_empty()
            && self.removed.is_empty()
            && self.flags.is_empty()
            && self.folders_created.is_empty()
            && self.folders_removed.is_empty()
            && self.mailboxes_gone.is_empty()
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
    /// Mailbox quota reported by the server.
    quota: Option<MailboxQuota>,
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
    /// Offline-window passes and the downloads opening a message started (see `offline.rs`).
    pub(crate) offline: crate::offline::OfflineState,
    /// Accounts whose next run downloads new messages as headers only (Download Headers).
    pub(crate) force_headers: BTreeSet<AccountId>,
    /// Last run of each user-defined Send/Receive group (lower-case name → Unix time; see `groups.rs`).
    pub(crate) group_runs: BTreeMap<String, i64>,
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
    /// A UI fetches internet calendars on its own worker threads: Send/Receive and Update Folder queue
    /// their calendar refreshes in [`SyncState::calendar_requests`] instead of fetching while the command runs.
    pub background_calendars: bool,
    /// Internet calendars Send/Receive asked to refresh, waiting for the UI's fetcher.
    pub calendar_requests: Vec<crate::cmd::calendar::subscribe::Due>,
    /// Newly arrived mail not looked at yet (the new-mail envelope, see `new_mail.rs`).
    pub(crate) new_mail: BTreeSet<ItemId>,
    /// Search › Include Older Results and the server matches of the current search (`older.rs`).
    pub older: older::OlderResults,
    /// Server searches waiting for each account's next run.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) older_requests: BTreeMap<AccountId, older::Request>,
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
    matches!(kind, AccountKind::Imap | AccountKind::Jmap | AccountKind::Graph | AccountKind::Exchange)
}

/// Accounts whose local changes are found by comparing with a shadow of the server's state (JMAP,
/// Microsoft Graph, Exchange) rather than queued as they happen (IMAP).
pub(crate) fn is_shadow_account(kind: AccountKind) -> bool {
    matches!(kind, AccountKind::Jmap | AccountKind::Graph | AccountKind::Exchange)
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

    /// The error of account `id`'s last send/receive while none is in flight (`None` while one runs, or
    /// when the last one worked).
    pub(crate) fn last_run_error(&self, id: AccountId) -> Option<String> {
        self.sync.runs.get(&id).filter(|r| r.busy.is_none()).and_then(|r| r.error.clone())
    }

    /// The push (IDLE) state of an account and the last push error.
    pub fn push_state(&self, id: AccountId) -> (PushMode, Option<String>) {
        self.sync.runs.get(&id).map(|r| (r.push, r.push_error.clone())).unwrap_or_default()
    }

    /// Applies finished worker results (new folders and messages, flag changes, removed messages, sent
    /// messages, completed server changes), runs rules on new Inbox mail, queues the server changes rules
    /// made, starts automatic runs that are due, and journals everything. Returns true when the mailbox or
    /// the status changed. Never panics: a panic while applying results restores the mailbox. Translations
    /// that finished in the background are applied here too (`translate::poll`).
    pub fn poll_sync(&mut self) -> bool {
        self.drop_stray_records();
        let status_before = self.sync.status.clone();
        let translated = crate::translate::poll(self);
        let imported = crate::pst::poll(self);
        let mailbox_changed = poll_events(self) || translated || imported;
        tick(self);
        groups::tick(self);
        crate::recover::tick(self);
        crate::offline::tick(self);
        crate::search_index::tick(self);
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

/// The quota a server reported for an account's mailbox (stored with the run state; shown in the status bar).
pub(crate) fn set_quota(s: &mut Session, account: AccountId, quota: Option<MailboxQuota>) {
    let run = s.sync.runs.entry(account).or_default();
    if run.quota != quota {
        run.quota = quota;
        s.sync.dirty = true;
    }
}

/// The last reported quota of an account's mailbox.
pub fn account_quota(s: &Session, account: AccountId) -> Option<MailboxQuota> {
    s.sync.runs.get(&account).and_then(|r| r.quota)
}

/// The quota for the status bar: the current folder's account, else the first server account that has one.
pub fn status_quota(s: &Session) -> Option<(String, MailboxQuota)> {
    let name = |id: AccountId| s.mailbox.account(id).map(|a| a.display_name.clone()).unwrap_or_default();
    if let Some(a) = s.current_account()
        && let Some(q) = account_quota(s, a)
    {
        return Some((name(a), q));
    }
    s.mailbox.accounts.iter().find_map(|a| account_quota(s, a.id).map(|q| (name(a.id), q)))
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
            quota: run.and_then(|r| r.quota),
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
        // Exchange: no push connection yet; the Inbox is checked every few minutes instead (Polling).
        let exchange: Vec<AccountId> = s
            .mailbox
            .accounts
            .iter()
            .filter(|a| a.kind == AccountKind::Exchange && a.incoming.is_some())
            .map(|a| a.id)
            .collect();
        for id in exchange {
            let opts = crate::accounts::account_options(s, id);
            let Some(run) = s.sync.runs.get_mut(&id) else { continue };
            let want = !s.view.work_offline && opts.push && run.last_sync.is_some() && run.error.is_none();
            let mode = if want { PushMode::Polling } else { PushMode::Off };
            if run.push != mode {
                run.push = mode;
                s.sync.dirty = true;
            }
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
                // A shared mailbox's Inbox has its own server name.
                let inbox = s
                    .mailbox
                    .role_folder(id, mailcraft_model::FolderRole::Inbox)
                    .and_then(|f| s.mailbox.folder(f))
                    .and_then(|f| f.remote_name.clone())
                    .unwrap_or_else(|| "INBOX".to_owned());
                let target = want.then_some(native::PushTarget::Imap(inbox));
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
        // Microsoft Graph pushes only to public web endpoints: an account that wants push has its Inbox
        // polled once it has connected (`tick`).
        let graph: Vec<AccountId> = s
            .mailbox
            .accounts
            .iter()
            .filter(|a| a.kind == AccountKind::Graph && a.incoming.is_some())
            .map(|a| a.id)
            .collect();
        for id in graph {
            let opts = crate::accounts::account_options(s, id);
            let offline = s.view.work_offline;
            let Some(run) = s.sync.runs.get_mut(&id) else { continue };
            let mode = if !offline && opts.push && run.last_sync.is_some() && run.error.is_none() {
                PushMode::Polling
            } else {
                PushMode::Off
            };
            if run.push != mode {
                run.push = mode;
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
    matches!(op, RemoteOp::Append { item, .. } if !mb.mail.contains(*item))
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
            RemoteOp::Append { item, .. } => s.mailbox.mail.get(*item).map(|m| item_source(s, m)),
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

/// Messages the change recorded by `r` takes off their server while only their header is on this computer
/// (downloaded as headers only, or the body taken off by the offline window): moved out of their server
/// account (to a local folder, a data file or another account), or in a folder moved off the server. Their
/// server copy would be expunged and the item kept without a body, so `Session::run` refuses such a change.
pub(crate) fn header_only_leaving(s: &Session, r: &crate::changes::Recording) -> Vec<ItemId> {
    let before = r.before_small();
    let after = &s.mailbox;
    let mut out: BTreeSet<ItemId> = BTreeSet::new();
    // A Graph folder on the service: its account.
    let graph_folder = |mb: &Mailbox, f: FolderId| {
        mb.folder(f)
            .filter(|f| f.kind == FolderKind::Mail && f.remote_name.is_some())
            .map(|f| f.account)
            .filter(|a| mb.account(*a).is_some_and(|a| a.kind == AccountKind::Graph))
    };
    for (id, b) in r.mail_so_far(after) {
        let (Some(b), Some(a)) = (b, after.mail.get(id)) else { continue };
        if a.folder != b.folder
            && b.remote_id.is_some()
            && let Some(src) = graph_folder(after, b.folder).or_else(|| graph_folder(before, b.folder))
        {
            // Graph: the service keeps its copy for a move inside the account (outside the Outbox), even
            // into a folder that stays here.
            let kept = keeps_server_copy(after, src, a.folder);
            if !kept && crate::sync::cmds::is_header_only(s, id) {
                out.insert(id);
            }
            continue;
        }
        if a.folder == b.folder || uid_of(b).is_none() {
            continue;
        }
        let Some((src, _)) = synced_folder(after, b.folder).or_else(|| synced_folder(before, b.folder))
        else {
            continue;
        };
        if synced_account(after, a.folder) != Some(src) && crate::sync::cmds::is_header_only(s, id) {
            out.insert(id);
        }
    }
    // Folders moved off the server (under a local folder, or to another account) take their items along.
    let mut gone: BTreeSet<FolderId> = BTreeSet::new();
    for f in after.folders.values() {
        let Some(b) = before.folders.get(&f.id) else { continue };
        if b.remote_name.is_none()
            || !is_imap(before, b.account)
            || (b.parent == f.parent && b.account == f.account)
        {
            continue;
        }
        if f.account != b.account || remote_path(after, f).is_none() {
            gone.extend(after.subtree(f.id));
        }
    }
    if !gone.is_empty() {
        out.extend(
            after
                .mail
                .items()
                .filter(|m| gone.contains(&m.folder) && uid_of(m).is_some())
                .map(|m| m.id)
                .filter(|id| crate::sync::cmds::is_header_only(s, *id)),
        );
    }
    out.into_iter().collect()
}

/// True when mail of the Graph account `account` moved into `folder` keeps its copy on the service: any mail
/// folder of the account outside the Outbox (a folder that stays here, below a default folder the service
/// does not have, leaves the service's copy where it was). Mail moved anywhere else (another account, a
/// local folder, the Outbox) leaves the service.
pub(crate) fn keeps_server_copy(mb: &Mailbox, account: AccountId, folder: FolderId) -> bool {
    let Some(f) = mb.folder(folder) else { return false };
    if f.account != account || f.kind != FolderKind::Mail {
        return false;
    }
    let Some(outbox) = mb.role_folder(account, mailcraft_model::FolderRole::Outbox) else { return true };
    let mut cur = Some(folder);
    let mut steps = 0usize;
    while let Some(id) = cur {
        if id == outbox {
            return false;
        }
        steps = steps.saturating_add(1);
        if steps > mb.folders.len() {
            break;
        }
        cur = mb.folder(id).and_then(|x| x.parent);
    }
    true
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
    let root = mb.account(f.account).and_then(|a| a.shared.as_ref()).map(|sh| sh.root.clone());
    match f.parent {
        // A shared mailbox's top-level folders live below its root.
        None => Some(match root {
            Some(r) => format!("{r}{d}{leaf}"),
            None => leaf,
        }),
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
    if let Some(m) = mb.mail.get_mut(id) {
        m.remote_id = uid.map(|u| u.to_string());
    }
}

/// Queues the server changes a change set describes (after a command, an undo or a sync batch). The
/// adjustments this makes to the mailbox (server names, cleared UIDs, the accounts' queues) join `changes`.
pub(crate) fn queue_local_changes(s: &mut Session, changes: &mut crate::changes::ChangeSet) -> usize {
    crate::accounts::load_prefs(s);
    let Some(mail) = changes.mail.as_ref() else {
        log::error!("server changes not queued: the record of what changed was lost");
        return 0;
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let jmap: Vec<AccountId> =
            s.mailbox.accounts.iter().filter(|a| a.kind == AccountKind::Jmap).map(|a| a.id).collect();
        for a in jmap {
            if jmap::changed(&changes.small, mail, &s.mailbox, a) {
                s.sync.jmap_dirty.insert(a);
            }
        }
        // Microsoft Graph accounts have no change queue either: a run sends what differs from the shadow.
        let graph: Vec<AccountId> =
            s.mailbox.accounts.iter().filter(|a| a.kind == AccountKind::Graph).map(|a| a.id).collect();
        for a in graph {
            if graph::changed(&changes.small, mail, &s.mailbox, a) {
                s.sync.jmap_dirty.insert(a);
            }
        }
        let exchange: Vec<AccountId> =
            s.mailbox.accounts.iter().filter(|a| a.kind == AccountKind::Exchange).map(|a| a.id).collect();
        for a in exchange {
            if ews::changed(&changes.small, mail, &s.mailbox, a) {
                s.sync.jmap_dirty.insert(a);
            }
        }
    }
    let inflight = s.sync.inflight_ops();
    let pop3: BTreeSet<AccountId> =
        s.mailbox.accounts.iter().filter(|a| crate::accounts::is_pop3(s, a.id)).map(|a| a.id).collect();
    let token = s.mailbox.mail.begin();
    let n = queue_changes_except(&changes.small, mail, &mut s.mailbox, &inflight, &pop3);
    let more = s.mailbox.mail.commit(token);
    changes.absorb(more);
    n
}

/// Compares `before` and `after` for IMAP accounts and appends [`RemoteOp`]s to the accounts' queues.
/// Adjusts `after` as the server will see it: server names of new/renamed folders, cleared UIDs of moved or
/// copied items, and queued (not yet dispatched) operations that a later change made moot. Returns how many
/// operations were queued.
#[cfg(test)]
pub(crate) fn queue_changes(before: &Mailbox, after: &mut Mailbox, inflight: &Inflight) -> usize {
    let mail = MailChanges::between(&before.mail, &after.mail);
    queue_changes_except(before, &mail, after, inflight, &BTreeSet::new())
}

/// [`queue_changes`] from a change record: `before` holds everything but the mail as it was (its mail is not
/// read), `mail` the before-images of the items written since, and `after` the mailbox now. Leaves out
/// `skip` (POP3 accounts: their folders are local).
pub(crate) fn queue_changes_except(
    before: &Mailbox,
    mail: &MailChanges,
    after: &mut Mailbox,
    inflight: &Inflight,
    skip: &BTreeSet<AccountId>,
) -> usize {
    if !after.accounts.iter().any(|a| a.kind == AccountKind::Imap && !skip.contains(&a.id)) {
        return 0;
    }
    let mut q: Vec<(AccountId, RemoteOp)> = Vec::new();
    folder_changes(before, after, skip, &mut q);
    item_changes(mail, before, after, inflight, &mut q);
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
                    q.push((f.account, RemoteOp::CreateFolder { name: path.clone(), special_use: None }));
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
                        after.mail.update_where(|m| sub.contains(&m.folder), |m| m.remote_id = None);
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
    mail: &MailChanges,
    before: &Mailbox,
    after: &mut Mailbox,
    inflight: &Inflight,
    q: &mut Vec<(AccountId, RemoteOp)>,
) {
    // Only items whose server-relevant state changed (most commands touch a handful of items), among those
    // the change record holds (the others are as they were).
    let same = |a: &MailItem, b: &MailItem| {
        a.folder == b.folder
            && a.read == b.read
            && a.flag == b.flag
            && a.response == b.response
            && a.remote_id == b.remote_id
            && a.is_draft == b.is_draft
            && (!a.is_draft || (a.blob == b.blob && a.body_key == b.body_key))
    };
    let ids: Vec<ItemId> = mail
        .iter()
        .filter(|(id, b)| match (b, after.mail.get(*id)) {
            (Some(b), Some(a)) => !same(a, b),
            (Some(_), None) | (None, Some(_)) => true,
            (None, None) => false,
        })
        .map(|(id, _)| id)
        .collect();
    for id in ids {
        let b = mail.before(id).flatten();
        // What is needed of the item after the change (taken before `after` is adjusted).
        let Some((a_folder, a_uid, upload, words, draft_changed)) = after.mail.get(id).map(|a| {
            let draft_changed = b.is_some_and(|b| {
                a.is_draft && b.is_draft && a.blob.is_some() && (a.blob != b.blob || a.body_key != b.body_key)
            });
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
        // A server message moved into a default folder the server has no mailbox for (Archive on a server
        // without one), or into a folder below one: the mailboxes are made there first, so the message stays
        // on the server.
        let dst = match (dst, &src) {
            (None, Some((sa, _))) if a_folder != b.folder && b_uid.is_some() => {
                server_folder_below_default(after, a_folder, *sa, q)
            }
            (dst, _) => dst,
        };
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

/// Makes `folder`, a default folder (Archive, Deleted Items, Junk Email, Sent Items, Drafts) of IMAP account
/// `account` that the server has no mailbox for, on the server: queues its creation and gives it its server
/// name. Only once the account's folder list was read (else the server may well have it).
fn server_default_folder(
    after: &mut Mailbox,
    folder: FolderId,
    account: AccountId,
    q: &mut Vec<(AccountId, RemoteOp)>,
) -> Option<(AccountId, String)> {
    use mailcraft_model::FolderRole as R;
    let f = after.folders.get(&folder)?;
    let special_use = match f.role {
        Some(R::Archive) => "\\Archive",
        Some(R::DeletedItems) => "\\Trash",
        Some(R::JunkEmail) => "\\Junk",
        Some(R::SentItems) => "\\Sent",
        Some(R::Drafts) => "\\Drafts",
        _ => return None,
    };
    if f.account != account
        || f.kind != FolderKind::Mail
        || f.remote_name.is_some()
        || f.parent.is_some()
        || !is_imap(after, account)
        || after.account(account).is_none_or(|a| a.sync.delimiter.is_none() || a.shared.is_some())
    {
        return None;
    }
    let path = remote_path(after, f)?;
    q.push((
        account,
        RemoteOp::CreateFolder { name: path.clone(), special_use: Some(special_use.to_owned()) },
    ));
    if let Some(g) = after.folders.get_mut(&folder) {
        g.remote_name = Some(path.clone());
    }
    Some((account, path))
}

/// Makes `folder` of IMAP account `account` on the server when it is a default folder the server has no
/// mailbox for ([`server_default_folder`]) or a folder below one (`Archive/2024`): the default folder first
/// (when the server lacks it), then each folder down to `folder` that is not there yet. `None` (nothing
/// queued) for any other folder that is not on the server.
fn server_folder_below_default(
    after: &mut Mailbox,
    folder: FolderId,
    account: AccountId,
    q: &mut Vec<(AccountId, RemoteOp)>,
) -> Option<(AccountId, String)> {
    // `folder` and its parents, top level last.
    let mut chain = vec![folder];
    let mut cur = after.folders.get(&folder)?.parent;
    while let Some(p) = cur {
        if chain.len() > after.folders.len() || chain.contains(&p) {
            return None;
        }
        chain.push(p);
        cur = after.folders.get(&p)?.parent;
    }
    let top_id = *chain.last()?;
    let top = after.folders.get(&top_id)?;
    if top.role.is_none_or(|r| {
        !matches!(
            r,
            mailcraft_model::FolderRole::Archive
                | mailcraft_model::FolderRole::DeletedItems
                | mailcraft_model::FolderRole::JunkEmail
                | mailcraft_model::FolderRole::SentItems
                | mailcraft_model::FolderRole::Drafts
        )
    }) {
        return None;
    }
    // Every folder below the default folder must be a plain mail folder of the account.
    if chain.iter().rev().skip(1).any(|f| {
        after
            .folders
            .get(f)
            .is_none_or(|f| f.account != account || f.kind != FolderKind::Mail || f.role.is_some())
    }) {
        return None;
    }
    if top.remote_name.is_none() {
        server_default_folder(after, top_id, account, q)?;
    } else if !is_imap(after, account) || top.account != account {
        return None;
    }
    for fid in chain.iter().rev().skip(1) {
        let f = after.folders.get(fid)?;
        if f.remote_name.is_some() {
            continue;
        }
        let path = remote_path(after, f)?;
        q.push((account, RemoteOp::CreateFolder { name: path.clone(), special_use: None }));
        if let Some(g) = after.folders.get_mut(fid) {
            g.remote_name = Some(path);
        }
    }
    synced_folder(after, folder)
}

/// Undo/redo put a step back (`s.mailbox` now): `changes` holds what was there instead (`changes.small`,
/// everything but the mail, and the live versions of the mail items the step put back). Keep what
/// synchronisation did meanwhile and queue the server changes that revert the undone command; the
/// adjustments join `changes`.
pub(crate) fn after_restore(s: &mut Session, changes: &mut crate::changes::ChangeSet) {
    let live = &changes.small;
    // Internet calendars keep their last refresh (refreshes are not undo steps).
    crate::cmd::calendar::subscribe::carry_over(live, &mut s.mailbox);
    // Contacts and tasks keep what their server sent and their links to it.
    pim::carry_over(live, &mut s.mailbox, &s.sync.track.pim_created, &s.sync.track.pim_removed);
    // IMAP and JMAP accounts alike: without this, a JMAP folder or message the server added meanwhile would
    // be missing from the restored mailbox and the next run would remove it from the server.
    if !live.accounts.iter().any(|a| {
        matches!(a.kind, AccountKind::Imap | AccountKind::Jmap | AccountKind::Graph | AccountKind::Exchange)
    }) {
        return;
    }
    let Some(live_mail) = changes.mail.as_ref() else { return };
    let token = s.mailbox.mail.begin();
    carry_over(live, live_mail, &mut s.mailbox, &s.sync.track);
    #[cfg(not(target_arch = "wasm32"))]
    jmap::after_restore(s);
    let more = s.mailbox.mail.commit(token);
    changes.absorb(more);
    queue_local_changes(s, changes);
}

/// The live version of mail item `id`: its image in `live_mail` when the restore replaced it, else the item
/// in `restored` (left as it was).
fn live_item<'a>(live_mail: &'a MailChanges, restored: &'a Mailbox, id: ItemId) -> Option<&'a MailItem> {
    match live_mail.before(id) {
        Some(image) => image,
        None => restored.mail.get(id),
    }
}

/// `live` is everything but the mail as it was before the restore, `live_mail` the live versions of the items
/// the restore put back; the other items of `restored` are live already.
fn carry_over(live: &Mailbox, live_mail: &MailChanges, restored: &mut Mailbox, track: &Track) {
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
    // Mail that arrived or vanished meanwhile, and server flag changes. Only the items the restore put back
    // can differ from their live versions.
    for id in &track.created {
        if live_mail.contains(*id)
            && !restored.mail.contains(*id)
            && let Some(m) = live_item(live_mail, restored, *id)
            && restored.folders.contains_key(&m.folder)
        {
            let m = m.clone();
            restored.mail.insert(*id, m);
        }
    }
    for id in &track.removed {
        if live_mail.contains(*id) && live_item(live_mail, restored, *id).is_none() {
            restored.mail.remove(*id);
        }
    }
    // JMAP email ids and Graph message ids (immutable ids) stay the same when a message moves between folders
    // of its account (IMAP UIDs do not).
    let jmap_account = |mb: &Mailbox, f: FolderId| {
        mb.folder(f).map(|f| f.account).filter(|a| mb.account(*a).is_some_and(|a| is_shadow_account(a.kind)))
    };
    let put_back: Vec<(ItemId, FolderId, Option<String>, bool, FlagState)> = live_mail
        .iter()
        .filter_map(|(id, l)| {
            let l = l?;
            Some((id, l.folder, l.remote_id.clone(), l.read, l.flag.clone()))
        })
        .collect();
    for (id, folder, remote_id, read, flag) in put_back {
        let Some(m) = restored.mail.get(id) else { continue };
        let same_id = m.folder == folder
            || jmap_account(restored, m.folder).is_some_and(|a| jmap_account(live, folder) == Some(a));
        let flags = track.flags.contains(&id);
        if (!same_id || m.remote_id == remote_id) && (!flags || (m.read == read && m.flag == flag)) {
            continue;
        }
        restored.mail.update(id, |m| {
            if same_id {
                m.remote_id = remote_id;
            }
            if flags {
                m.read = read;
                m.flag = flag;
            }
        });
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
    let removed: Vec<ItemId> = mb.mail.items().filter(|m| all.contains(&m.folder)).map(|m| m.id).collect();
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

/// Every folder of `account` (server ids such as JMAP email ids and POP3 unique ids are per account).
fn account_scope(mb: &Mailbox, account: AccountId) -> ScanFilter {
    ScanFilter::Folders(mb.folders.values().filter(|f| f.account == account).map(|f| f.id).collect())
}

/// The first item (lowest id) that `scope` admits and `hit` accepts.
fn first_item(mb: &Mailbox, scope: &ScanFilter, mut hit: impl FnMut(&MailItem) -> bool) -> Option<ItemId> {
    let mut found = None;
    mb.mail.scan(scope, &mut |m| {
        if hit(m) {
            found = Some(m.id);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    found
}

/// A local copy in `folder` waiting for the server message whose Message-ID is `mid` (sent or uploaded here,
/// no server id yet): the lowest such item. Its stored Message-ID must equal `mid` exactly; the server hands
/// back the bytes this client wrote.
fn local_copy(mb: &Mailbox, folder: FolderId, mid: &str) -> Option<ItemId> {
    mb.mail.by_message_id(mid).into_iter().find(|id| {
        mb.mail.get(*id).is_some_and(|m| {
            m.folder == folder && m.remote_id.is_none() && m.message_id.as_deref() == Some(mid)
        })
    })
}

/// Raw source of an item: its stored source, else rebuilt from its fields and body.
fn item_source(s: &Session, m: &MailItem) -> Vec<u8> {
    crate::cmd::file::raw_source(s, m)
}

/// IMAP folder results made up without a server, for benchmarks (`apps/mailcraft-bench`) and tests: queued
/// with [`Session::queue_imap_results`], they are applied by the next [`Session::poll_sync`] exactly like
/// results a send/receive worker downloaded (rules, the Junk Email filter, journaling, undo tracking). Only
/// in tests and with the `synthetic-sync` feature, which only the benchmark tool turns on.
#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "synthetic-sync")))]
#[derive(Clone, Debug)]
pub enum SyntheticImap {
    /// One sync round of the folder whose server name is `folder` (new messages, flag changes, vanished
    /// UIDs, folder state).
    Delta { folder: String, delta: mailcraft_net::sync::SyncDelta },
    /// Whole messages for items of `folder` first downloaded as headers only.
    Bodies { folder: String, msgs: Vec<mailcraft_net::sync::NewMessage> },
}

#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "synthetic-sync")))]
impl Session {
    /// Queues `results` for IMAP account `account` as if its send/receive worker had sent them (see
    /// [`SyntheticImap`]); nothing connects to a server. False when `account` is not an IMAP account.
    pub fn queue_imap_results(&self, account: AccountId, results: SyntheticImap) -> bool {
        native::queue_synthetic(self, account, results)
    }
}

// =======================================================================================================
// Native: workers, jobs and results
// =======================================================================================================

pub mod arrival;
pub mod cmds;
#[cfg(not(target_arch = "wasm32"))]
mod dav;
#[cfg(not(target_arch = "wasm32"))]
mod ews;
#[cfg(not(target_arch = "wasm32"))]
mod ews_extra;
#[cfg(not(target_arch = "wasm32"))]
mod ews_pim;
pub mod focus;
#[cfg(not(target_arch = "wasm32"))]
mod graph;
pub mod groups;
#[cfg(not(target_arch = "wasm32"))]
mod jmap;
#[cfg(not(target_arch = "wasm32"))]
mod jmap_pim;
#[cfg(not(target_arch = "wasm32"))]
mod jscalendar;
#[cfg(not(target_arch = "wasm32"))]
mod jscontact;
#[cfg(not(target_arch = "wasm32"))]
mod native;
pub mod older;
pub mod pim;
pub mod shared;

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
// Exchange services: automatic replies, other people's free/busy time, meeting answers
// =======================================================================================================

/// Longest automatic reply accepted.
const MAX_REPLY_CHARS: usize = 64 * 1024;

/// `file.automaticReplies`: reads the Exchange account's automatic replies (and asks the UI to show them),
/// or changes them when the params name a change.
pub(crate) fn automatic_replies(s: &mut Session, p: &Value) -> Result<Value, CmdError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let wanted = match p.get("account") {
            Some(v) if !v.is_null() => Some(crate::accounts::find_account(s, p, "account")?),
            _ => None,
        };
        let Some(account) = ews_extra::exchange_account(s, wanted) else {
            return Err(CmdError::Disabled("Automatic replies need an Exchange account".into()));
        };
        let text = |k: &str| -> Result<Option<String>, CmdError> {
            match p.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(t)) if t.chars().count() <= MAX_REPLY_CHARS && !t.contains('\0') => {
                    Ok(Some(t.clone()))
                }
                Some(_) => Err(CmdError::Params(format!(
                    "`{k}` must be a text of at most {MAX_REPLY_CHARS} characters"
                ))),
            }
        };
        let state = match p.get("state").and_then(Value::as_str) {
            None => None,
            Some("off") => Some("Disabled"),
            Some("on") => Some("Enabled"),
            Some("scheduled") => Some("Scheduled"),
            Some(other) => {
                return Err(CmdError::Params(format!("`state` must be off, on or scheduled, not `{other}`")));
            }
        };
        let outside = match p.get("outside").and_then(Value::as_str) {
            None => None,
            Some("none") => Some("None"),
            Some("contacts") => Some("Known"),
            Some("everyone") => Some("All"),
            Some(other) => {
                return Err(CmdError::Params(format!(
                    "`outside` must be none, contacts or everyone, not `{other}`"
                )));
            }
        };
        let offset = i64::from(s.tz_offset_min);
        let time = |k: &str| -> Result<Option<String>, CmdError> {
            let Some(v) = p.get(k).and_then(Value::as_str) else { return Ok(None) };
            let has_zone =
                v.trim().ends_with('Z') || v.trim().get(16..).is_some_and(|t| t.contains(['+', '-']));
            let d = mailcraft_model::DateTime::parse_rfc5322(v.trim())
                .map_err(|_| CmdError::Params(format!("`{k}` must be a date and time (YYYY-MM-DDTHH:MM)")))?;
            let unix = if has_zone { d.unix } else { d.unix.saturating_sub(offset.saturating_mul(60)) };
            Ok(Some(mailcraft_model::DateTime::from_unix(unix, 0).to_rfc3339()))
        };
        let (inside, outside_text, start, end) =
            (text("insideReply")?, text("outsideReply")?, time("start")?, time("end")?);
        let changing = state.is_some()
            || outside.is_some()
            || inside.is_some()
            || outside_text.is_some()
            || start.is_some()
            || end.is_some();
        let mut o = ews_extra::oof(s, account).map_err(CmdError::Failed)?;
        if changing {
            if let Some(st) = state {
                o.state = st.into();
            }
            if let Some(x) = outside {
                o.external_audience = x.into();
            }
            if let Some(t) = inside {
                o.internal_reply = t;
            }
            if let Some(t) = outside_text {
                o.external_reply = t;
            }
            if start.is_some() {
                o.start = start;
            }
            if end.is_some() {
                o.end = end;
            }
            if o.state == "Scheduled" {
                let (Some(a), Some(b)) = (
                    o.start.as_deref().and_then(ews::parse_time),
                    o.end.as_deref().and_then(ews::parse_time),
                ) else {
                    return Err(CmdError::Params(
                        "scheduled automatic replies need `start` and `end`".into(),
                    ));
                };
                if b <= a {
                    return Err(CmdError::Params("`end` must follow `start`".into()));
                }
            }
            ews_extra::set_oof(s, account, &o).map_err(CmdError::Failed)?;
        }
        let state_name = match o.state.as_str() {
            "Enabled" => "on",
            "Scheduled" => "scheduled",
            _ => "off",
        };
        let outside_name = match o.external_audience.as_str() {
            "Known" => "contacts",
            "All" => "everyone",
            _ => "none",
        };
        // Times as local "YYYY-MM-DDTHH:MM", like the params.
        let local = |t: &Option<String>| {
            let unix = ews::parse_time(t.as_deref()?)?.saturating_add(offset.saturating_mul(60));
            mailcraft_model::DateTime::from_unix(unix, 0).to_rfc3339().get(..16).map(str::to_owned)
        };
        let mut v = json!({
            "account": account.0,
            "state": state_name,
            "start": local(&o.start),
            "end": local(&o.end),
            "insideReply": o.internal_reply,
            "outsideReply": o.external_reply,
            "outside": outside_name,
            "saved": changing,
        });
        if !changing && p.get("dialog").and_then(Value::as_bool) != Some(false) {
            s.notify(crate::Notice::OpenDialog("automaticReplies".into(), v.clone()));
            if let Some(o) = v.as_object_mut() {
                o.insert("dialog".into(), json!("automaticReplies"));
            }
        }
        Ok(v)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err(CmdError::Disabled("Automatic replies need the desktop app".into()))
    }
}

/// Other people's busy times from the Exchange server of the current account, for the Scheduling Assistant:
/// one entry per address, `{address, busy: [{start, end, showAs}]}` or `{address, error}`. `None` when the
/// account is not an Exchange account.
pub(crate) fn attendee_availability(s: &Session, addresses: &[String], from: i64, to: i64) -> Option<Value> {
    availability_lookup(s, addresses, from, to).map(AvailabilityLookup::run)
}

/// A lookup of other people's busy times (see `calendar.freeBusy` with `attendees`), prepared on the
/// session's thread and run on any other: the Scheduling Assistant runs it off the UI thread, as it waits on
/// the server.
#[derive(Clone, Debug)]
pub struct AvailabilityLookup {
    #[cfg(not(target_arch = "wasm32"))]
    conn: Result<ews::EwsConn, String>,
    addresses: Vec<String>,
    from: i64,
    to: i64,
}

/// Prepares the lookup of `addresses`' busy times between `from` and `to` (Unix seconds) on the Exchange
/// server of the current account (else the first Exchange account). `None` without an Exchange account.
pub fn availability_lookup(
    s: &Session,
    addresses: &[String],
    from: i64,
    to: i64,
) -> Option<AvailabilityLookup> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let account = ews_extra::exchange_account(s, None)?;
        Some(AvailabilityLookup {
            conn: ews_extra::conn(s, account),
            addresses: addresses.iter().take(100).cloned().collect(),
            from,
            to,
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, addresses, from, to);
        None
    }
}

impl AvailabilityLookup {
    /// The addresses asked about.
    pub fn addresses(&self) -> &[String] {
        &self.addresses
    }

    /// Asks the server (blocking): one entry per address, `{address, busy: [{start, end, showAs}]}` or
    /// `{address, error}`.
    pub fn run(self) -> Value {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.run_ews()
        }
        #[cfg(target_arch = "wasm32")]
        {
            json!(
                self.addresses
                    .iter()
                    .map(|a| json!({"address": a, "error": "not available"}))
                    .collect::<Vec<_>>()
            )
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn run_ews(self) -> Value {
        let (from, to) = (self.from, self.to);
        let list = self.addresses;
        let fmt = |t: i64| {
            let d = mailcraft_model::DateTime::from_unix(t, 0).to_rfc3339();
            d.trim_end_matches('Z').to_owned()
        };
        let r = match self.conn.and_then(|c| ews_extra::availability(&c, &list, &fmt(from), &fmt(to))) {
            Ok(r) => r,
            Err(e) => {
                return json!(list.iter().map(|a| json!({"address": a, "error": e})).collect::<Vec<_>>());
            }
        };
        let show = |b: &str| match b {
            "Tentative" => "tentative",
            "OOF" => "outOfOffice",
            "WorkingElsewhere" => "workingElsewhere",
            "Free" => "free",
            "NoData" => "unknown",
            _ => "busy",
        };
        let out: Vec<Value> = list
            .iter()
            .zip(r)
            .map(|(a, res)| match res {
                Ok(blocks) => json!({"address": a, "busy": blocks.iter().filter_map(|b| {
                    Some(json!({"start": ews::parse_time(&b.start)?, "end": ews::parse_time(&b.end)?, "showAs": show(&b.busy_type)}))
                }).collect::<Vec<_>>()}),
                Err(e) => json!({"address": a, "error": e}),
            })
            .collect();
        json!(out)
    }
}

/// Answers a meeting request kept on an Exchange server through the server itself (which updates its
/// calendar and, with `send`, tells the organizer). `Ok(false)` when the message is not one of those.
pub(crate) fn exchange_meeting_reply(
    s: &Session,
    mail: ItemId,
    response: &str,
    send: bool,
    comment: &str,
) -> Result<bool, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(m) = s.mailbox.mail.get(mail) else { return Ok(false) };
        let Some(remote) = m.remote_id.clone() else { return Ok(false) };
        let Some(account) = s.mailbox.folder(m.folder).map(|f| f.account) else { return Ok(false) };
        if ews_extra::exchange_account(s, Some(account)).is_none() {
            return Ok(false);
        }
        let reply = match response {
            "accepted" => mailcraft_net::ews::MeetingReply::Accept,
            "tentative" => mailcraft_net::ews::MeetingReply::Tentative,
            "declined" => mailcraft_net::ews::MeetingReply::Decline,
            _ => return Ok(false),
        };
        ews_extra::reply_to_meeting(s, account, &remote, reply, send, comment).map(|()| true)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, mail, response, send, comment);
        Ok(false)
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
        // An Exchange server found by Autodiscover: its EWS URL, or that it wants the password first.
        "ews": d.ews.as_ref().map(|e| json!({"url": e.url, "autodiscover": e.autodiscover,
            "needsSignIn": e.needs_sign_in, "verified": e.verified})),
        // A well-known provider (Microsoft 365, Outlook.com, Google): its OAuth provider, hints for the
        // settings page, and whether a Microsoft Graph account kind could serve it (graph-mail, issue #4).
        "provider": d.provider.map(|m| json!({
            "id": m.provider.key,
            "name": m.provider.name,
            "detectedBy": m.by,
            "oauthProvider": m.provider.oauth_provider,
            "oauthOnly": m.provider.oauth_only,
            "graph": m.provider.graph,
            "notes": m.provider.notes,
        })),
        "mx": d.mx,
        "tenant": d.tenant,
        "notes": d.notes,
    })
}

/// Finds the server settings of `address` (a consumer domain's well-known provider; otherwise autoconfig on
/// the mail domain, then DNS SRV and MX records and the Microsoft 365 tenant; see `mailcraft_net::discover`).
/// The lookup runs on its own thread: the first call starts it and reports `"status": "running"`, later calls
/// with the same address report the result (`wait` blocks for it, at most a minute). Results are remembered
/// for the session; `refresh` looks up again. Lookups that need no network (reserved example domains, the big
/// consumer domains) finish at once.
pub(crate) fn discover(s: &mut Session, address: &str, wait: bool, refresh: bool) -> Result<Value, CmdError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use mailcraft_net::discover as nd;
        let key = address.trim().to_ascii_lowercase();
        let domain = nd::address_domain(&key).map_err(|e| CmdError::Params(e.to_string()))?;
        if refresh {
            s.sync.discoveries.remove(&key);
        }
        if !s.sync.discoveries.contains_key(&key) {
            let opts = s.sync.discovery.clone();
            let offline = (nd::is_reserved_domain(&domain) && !opts.allow_reserved)
                || mailcraft_net::providers::by_domain(&domain).is_some()
                || opts.is_offline();
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

    /// The sync lookups that moved onto the table's targeted reads (`scan`, `by_message_id`) find exactly
    /// what the full walks they replaced found.
    #[test]
    fn server_id_and_local_copy_lookups() {
        let (mut mb, a, inbox, trash) = imap_mailbox();
        let b = mb.add_account(Account {
            kind: AccountKind::Imap,
            address: "other@example.org".into(),
            incoming: Some(Default::default()),
            ..Default::default()
        });
        mb.create_default_folders(b);
        let b_inbox = mb.role_folder(b, FolderRole::Inbox).unwrap();
        let other =
            mb.add_mail(MailItem { folder: b_inbox, remote_id: Some("9".into()), ..Default::default() });
        let first =
            mb.add_mail(MailItem { folder: trash, remote_id: Some("9".into()), ..Default::default() });
        let second =
            mb.add_mail(MailItem { folder: inbox, remote_id: Some("9".into()), ..Default::default() });
        let is_9 = |m: &MailItem| m.remote_id.as_deref() == Some("9");
        // Server ids are per account: any folder of the account, lowest id first, never another account's.
        assert_eq!(first_item(&mb, &account_scope(&mb, a), is_9), Some(first));
        assert_eq!(first_item(&mb, &account_scope(&mb, b), is_9), Some(other));
        assert_eq!(first_item(&mb, &account_scope(&mb, AccountId(999)), is_9), None);
        assert!(first < second, "Trash's copy has the lower id, the Inbox one is found later in id order");

        let mid = "sent.1@example.com";
        let msg = |folder, remote: Option<&str>, id: &str| MailItem {
            folder,
            remote_id: remote.map(str::to_owned),
            message_id: Some(id.to_owned()),
            ..Default::default()
        };
        mb.add_mail(msg(inbox, Some("3"), mid)); // already on the server
        mb.add_mail(msg(trash, None, mid)); // another folder
        mb.add_mail(msg(inbox, None, "Sent.1@Example.com")); // same key, different stored form
        assert_eq!(local_copy(&mb, inbox, mid), None, "the adoption match stays exact");
        let waiting = mb.add_mail(msg(inbox, None, mid));
        mb.add_mail(msg(inbox, None, mid));
        assert_eq!(local_copy(&mb, inbox, mid), Some(waiting), "the lowest waiting copy");
        assert_eq!(local_copy(&mb, inbox, ""), None);
    }

    #[test]
    fn flags_moves_deletes_and_folders_queue_server_changes() {
        let (mut mb, a, inbox, trash) = imap_mailbox();
        let id = mb.add_mail(MailItem { folder: inbox, remote_id: Some("5".into()), ..Default::default() });
        let none = BTreeSet::new();
        // Read + flag.
        let before = mb.clone();
        mb.mail.get_mut(id).unwrap().read = true;
        mb.mail.get_mut(id).unwrap().flag =
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
        mb.mail.get_mut(id).unwrap().folder = trash;
        queue_changes(&before, &mut mb, &none);
        assert_eq!(
            pending(&mb, a)[1],
            RemoteOp::Move { folder: "INBOX".into(), uid: 5, dest: "Trash".into(), item: id }
        );
        assert!(
            mb.mail.get(id).unwrap().remote_id.is_none(),
            "UID cleared until the server reports the new one"
        );
        // Moving back before the move ran cancels it and restores the UID.
        let before = mb.clone();
        mb.mail.get_mut(id).unwrap().folder = inbox;
        queue_changes(&before, &mut mb, &none);
        assert_eq!(pending(&mb, a).len(), 1);
        assert_eq!(mb.mail.get(id).unwrap().remote_id.as_deref(), Some("5"));
        // Permanent delete.
        let before = mb.clone();
        mb.mail.remove(id);
        queue_changes(&before, &mut mb, &none);
        assert_eq!(pending(&mb, a).last(), Some(&RemoteOp::Expunge { folder: "INBOX".into(), uid: 5 }));
        // New folder, rename, delete.
        let before = mb.clone();
        let f = mb.create_folder(inbox, "Projects").unwrap();
        queue_changes(&before, &mut mb, &none);
        assert_eq!(
            pending(&mb, a).last(),
            Some(&RemoteOp::CreateFolder { name: "INBOX/Projects".into(), special_use: None })
        );
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
        mb.mail.get_mut(n).unwrap().folder = inbox;
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
        assert_eq!(s.mailbox.mail.get(x).unwrap().folder, trash);
        assert_eq!(s.mailbox.account(a).unwrap().sync.pending.len(), 1);
        // Mail arrives from the server after the command.
        let y =
            s.mailbox.add_mail(MailItem { folder: inbox, remote_id: Some("2".into()), ..Default::default() });
        s.sync.track.created.insert(y);
        s.run("app.undo", &json!({})).unwrap();
        assert_eq!(s.mailbox.mail.get(x).unwrap().folder, inbox);
        assert!(s.mailbox.mail.contains(y), "mail that arrived meanwhile survives the undo");
        // The undone move was not sent yet: it is cancelled and the UID restored.
        assert!(s.mailbox.account(a).unwrap().sync.pending.is_empty());
        assert_eq!(s.mailbox.mail.get(x).unwrap().remote_id.as_deref(), Some("1"));
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
        let recording = s.begin_changes();
        f(&mut s.mailbox);
        let mut changes = s.end_changes(recording);
        queue_local_changes(s, &mut changes);
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
                mb.mail.get_mut(id).unwrap().folder = archive;
            }
        });
        let moves = op_ids(&s, a);
        assert_eq!(moves.len(), 3);
        mark_inflight(&mut s, a);
        // Meanwhile: x goes on to Deleted Items, y is deleted permanently, z goes to a local folder.
        change(&mut s, |mb| {
            mb.mail.get_mut(x).unwrap().folder = trash;
            mb.mail.remove(y);
            mb.mail.get_mut(z).unwrap().folder = local;
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
        assert!(
            s.mailbox.mail.get(x).unwrap().remote_id.is_none()
                && s.mailbox.mail.get(z).unwrap().remote_id.is_none()
        );
    }

    #[test]
    fn synthetic_imap_results_apply_like_worker_results() {
        let (mb, a, inbox, _) = imap_mailbox();
        let local = mb.default_account().map(|x| x.id).unwrap();
        let mut s = Session::new(mb);
        s.view.work_offline = true;
        let msg = |uid: u32, headers_only: bool| {
            mailcraft_net::sync::NewMessage {
            uid,
            raw: format!(
                "From: a@example.com\r\nSubject: Synthetic {uid}\r\nMessage-ID: <syn{uid}@example.com>\r\n\r\n\
                 Body {uid}\r\n"
            )
            .into_bytes(),
            flags: vec![],
            internal_date: None,
            size: None,
            headers_only,
        }
        };
        let delta = mailcraft_net::sync::SyncDelta {
            new: vec![msg(1, false), msg(2, true)],
            state: mailcraft_net::sync::FolderState { uidvalidity: 7, uidnext: 3, ..Default::default() },
            ..Default::default()
        };
        assert!(!s.queue_imap_results(
            local,
            SyntheticImap::Delta { folder: "INBOX".into(), delta: delta.clone() }
        ));
        assert!(s.queue_imap_results(a, SyntheticImap::Delta { folder: "INBOX".into(), delta }));
        assert!(s.poll_sync());
        let items: Vec<&MailItem> = s.mailbox.items_in(inbox);
        assert_eq!(items.len(), 2);
        let header = items.iter().find(|m| m.remote_id.as_deref() == Some("2")).map(|m| m.id).unwrap();
        assert!(s.body_now(header).text.is_none(), "header only");
        let bodies = SyntheticImap::Bodies { folder: "INBOX".into(), msgs: vec![msg(2, false)] };
        assert!(s.queue_imap_results(a, bodies));
        assert!(s.poll_sync());
        assert!(s.body_now(header).text.as_deref().is_some_and(|t| t.contains("Body 2")));
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
            mb.mail.get_mut(x).unwrap().read = true;
            mb.mail.get_mut(t).unwrap().read = true;
            mb.mail.get_mut(z).unwrap().folder = trash;
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
        assert!(!s.mailbox.mail.contains(x), "old copies are discarded");
        assert_eq!(new_inbox.len(), 1);
        assert_eq!(s.mailbox.mail.get(new_inbox[0]).unwrap().remote_id.as_deref(), Some("1"));
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
        change(&mut s, |mb| mb.mail.get_mut(x).unwrap().folder = b_trash);
        push_op(&mut s.mailbox, a, RemoteOp::CreateFolder { name: "Z".into(), special_use: None });
        mark_inflight(&mut s, a);
        assert!(op_ids(&s, a).contains(&op_ids(&s, b)[1]), "the test needs colliding ids");
        // Moving it back before the move ran cancels the move and restores the UID.
        change(&mut s, |mb| mb.mail.get_mut(x).unwrap().folder = b_inbox);
        assert_eq!(pending(&s.mailbox, b).len(), 1, "{:?}", pending(&s.mailbox, b));
        assert_eq!(s.mailbox.mail.get(x).unwrap().remote_id.as_deref(), Some("8"));
    }

    #[test]
    fn retry_delays_keep_the_queue_in_order() {
        let (mb, a, inbox, _) = imap_mailbox();
        let mut s = Session::new(mb);
        let x =
            s.mailbox.add_mail(MailItem { folder: inbox, remote_id: Some("5".into()), ..Default::default() });
        change(&mut s, |mb| mb.mail.get_mut(x).unwrap().read = true);
        change(&mut s, |mb| mb.mail.get_mut(x).unwrap().read = false);
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
        // The big consumer domains are known without any lookup, even with the default (network) options.
        s.sync.discovery = mailcraft_net::discover::DiscoverOptions::default();
        let v = discover(&mut s, "kim@outlook.com", false, true).unwrap();
        assert_eq!((v["status"].as_str(), v["found"].as_bool()), (Some("done"), Some(true)), "{v}");
        assert_eq!(v["provider"]["id"], "outlook.com");
        assert_eq!(v["provider"]["oauthProvider"], "microsoft");
        assert_eq!(v["provider"]["graph"], true);
        assert_eq!(v["imap"]["oauthOnly"], true);
        assert_eq!(v["smtp"]["host"], "smtp-mail.outlook.com");
        let v = discover(&mut s, "kim@gmail.com", false, true).unwrap();
        assert_eq!(
            (v["provider"]["id"].as_str(), v["provider"]["detectedBy"].as_str()),
            (Some("google"), Some("domain"))
        );
        assert_eq!(v["imap"]["oauth"], true);
        assert_eq!(v["imap"]["oauthOnly"], false);
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
