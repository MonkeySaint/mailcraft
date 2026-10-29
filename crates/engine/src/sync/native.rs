//! Native send/receive: one worker thread per server account, the jobs it runs (IMAP, POP3, JMAP, SMTP),
//! IMAP IDLE push between jobs (with reconnect backoff), OAuth token refresh, and applying the results on the
//! session's thread.
//!
//! **Push.** After a run that worked, an IMAP account whose options ask for push gets a `Push` message: its
//! worker keeps the connection open and IDLEs (RFC 2177) on the Inbox between jobs, re-issuing IDLE well
//! within the 29-minute limit. A new job raises the worker's wake flag, which ends IDLE (`DONE`) at once.
//! When the server reports a change the worker tells the session, which starts an Inbox update. A lost
//! connection is retried with exponential backoff; a server without IDLE is reported as `Polling`, and the
//! session then checks the Inbox every few minutes instead.
//!
//! JMAP accounts push the same way through the server's event source (RFC 8620 section 7.3): the worker keeps
//! the `text/event-stream` open between jobs and reports a change when a `StateChange` names a new Email or
//! Mailbox state. A server without an event source is reported as `Polling`.
//!
//! **Slow connections.** Runs measure the transfer rate of the mail they download; when it is below the
//! session's threshold and the account asks for headers only on slow connections, new mail arrives as
//! headers until a later run measures a faster link.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use mailcraft_net::oauth::{OAuthProvider, TokenSet};
use mailcraft_net::pop3::{Pop3Client, Pop3Settings};
use mailcraft_net::smtp::{SmtpClient, SmtpSettings};
use mailcraft_net::sync::{
    FetchProgress, FolderState, ImapAccount, ImapSettings, ListScope, NewMessage, RemoteFolder, SharedRoot,
    SyncDelta,
};
use mailcraft_net::{ConnectOptions, Credentials, NetError};

use super::*;
use crate::accounts::{
    DownloadMode, account_options, account_secret, client_secret_key, pem_certificates, refresh_key,
    secret_key,
};
use mailcraft_model::account::CredentialKind;
use mailcraft_model::{Account, FolderRole, ServerSettings};

/// Most batches of new messages fetched for one folder in one run.
const MAX_ROUNDS_PER_FOLDER: usize = 50;
/// Most events applied by one poll (the rest wait for the next one).
const MAX_EVENTS_PER_POLL: usize = 5_000;
/// Longest single IDLE before it is re-issued (RFC 2177: servers may drop clients idle for 30 minutes).
const IDLE_SLICE: Duration = Duration::from_secs(25 * 60);
/// First reconnect delay of a push connection; doubles per failure up to [`PUSH_BACKOFF_MAX`].
const PUSH_BACKOFF_BASE: Duration = Duration::from_secs(5);
const PUSH_BACKOFF_MAX: Duration = Duration::from_secs(300);
/// Most new POP3 messages downloaded in one run (the rest come next time).
const MAX_POP3_PER_RUN: usize = 500;
/// Refresh an OAuth access token this many seconds before it expires.
const TOKEN_REFRESH_SKEW: u64 = 120;
/// Prefix of the `remote_id` of POP3 items: `pop3:<unique id>`.
pub(super) const POP3_PREFIX: &str = "pop3:";
/// Ping interval asked of a JMAP event source; the stream counts as lost after twice this plus a margin.
const JMAP_PING: Duration = Duration::from_secs(60);
/// How long one wait for JMAP push events lasts before the worker looks at its messages again.
const JMAP_PUSH_SLICE: Duration = Duration::from_millis(250);
/// Least time between two progress reports of a download (the first and the last always go out).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// Spaces out progress reports so a fast download does not flood the session with them.
#[derive(Default)]
pub(super) struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    /// True when a report should go out now (always when `force`).
    pub(super) fn due(&mut self, force: bool) -> bool {
        let now = Instant::now();
        if force || self.last.is_none_or(|t| now.saturating_duration_since(t) >= PROGRESS_INTERVAL) {
            self.last = Some(now);
            return true;
        }
        false
    }
}

/// Bytes a run must download before its transfer rate counts (smaller downloads mostly measure latency).
const MIN_RATE_SAMPLE: u64 = 32 * 1024;

/// How an account's incoming server is reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Incoming {
    Imap(ImapSettings),
    Pop3(Pop3Settings),
    Jmap(super::jmap::JmapConn),
}

/// What a worker needs to refresh an OAuth access token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OAuthRefresh {
    pub(super) provider: OAuthProvider,
    pub(super) refresh_token: String,
    pub(super) expires_at: Option<u64>,
    pub(super) options: ConnectOptions,
}

/// Connection settings for one account (built on the session thread, used by the worker).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Conn {
    pub(super) incoming: Incoming,
    pub(super) smtp: Option<SmtpSettings>,
    pub(super) oauth: Option<OAuthRefresh>,
    /// The OAuth access token renewed during this run (the DAV part signs in with it too).
    pub(super) renewed: Option<mailcraft_net::dav::DavAuth>,
}

impl Conn {
    /// Puts a fresh OAuth access token into every credential of the connection.
    fn set_token(&mut self, token: &str) {
        let swap = |c: &mut Credentials| {
            if let Credentials::OAuth2 { access_token, .. } = c {
                *access_token = token.to_owned();
            }
        };
        match &mut self.incoming {
            Incoming::Imap(i) => swap(&mut i.credentials),
            Incoming::Pop3(p) => swap(&mut p.credentials),
            Incoming::Jmap(j) => j.set_bearer(token),
        }
        if let Some(c) = self.smtp.as_mut().and_then(|s| s.credentials.as_mut()) {
            swap(c);
        }
        self.renewed = Some(mailcraft_net::dav::DavAuth::Bearer(token.to_owned()));
    }
}

pub(super) struct Outgoing {
    pub(super) item: ItemId,
    pub(super) from: String,
    pub(super) rcpts: Vec<String>,
    pub(super) raw: Vec<u8>,
}

struct Receive {
    only: Option<Vec<String>>,
    states: BTreeMap<String, FolderState>,
    /// The folders listed: the personal namespace, or a shared store's root.
    scope: ListScope,
    /// Also list the other users' and shared mailboxes the login can open (full runs of a personal account).
    shared_roots: bool,
}

/// One POP3 run: what is already here, what to delete and what to download in full.
#[derive(Clone, Debug, Default)]
pub(super) struct Pop3Job {
    receive: bool,
    known: BTreeSet<String>,
    delete: BTreeSet<String>,
    full: BTreeSet<String>,
    headers_only: bool,
}

struct Job {
    conn: Conn,
    ops: Vec<(u64, RemoteOp, Option<Vec<u8>>)>,
    outgoing: Vec<Outgoing>,
    receive: Option<Receive>,
    /// IMAP: new messages arrive as headers only.
    headers_only: bool,
    /// IMAP: after the headers, download the new messages in full too (Download Headers and Then Full Items).
    then_full: bool,
    /// IMAP: whole messages to download, by server folder and UID (Process Marked Headers).
    fetch: Vec<(String, Vec<u32>)>,
    pop3: Option<Pop3Job>,
    jmap: Option<super::jmap::JmapJob>,
    /// Contacts and tasks (CardDAV / CalDAV) and calendar publishing.
    dav: Option<super::dav::DavJob>,
    /// IMAP: a server search (Search › Include Older Results).
    search: Option<ImapSearch>,
    /// JMAP: a server search.
    jmap_search: Option<super::jmap::JmapSearch>,
}

/// An IMAP server search: the criteria and, per server folder, its known UIDVALIDITY, the highest UID its
/// sync has covered (matches above it are new mail for the next sync) and the UIDs held here (those are not
/// downloaded again).
struct ImapSearch {
    generation: u64,
    criterion: mailcraft_net::search::Criterion,
    folders: Vec<SearchFolder>,
}

/// One server folder of an [`ImapSearch`].
struct SearchFolder {
    name: String,
    uidvalidity: u32,
    up_to: u32,
    known: Vec<u32>,
}

/// What a worker listens to between jobs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PushTarget {
    /// IMAP IDLE on this server folder.
    Imap(String),
    /// The JMAP event source, with the Email and Mailbox states the session last saw.
    Jmap { conn: super::jmap::JmapConn, email_state: String, mailbox_state: String },
}

/// What the session sends a worker.
enum Msg {
    Run(Box<Job>),
    /// Listen for new mail between jobs (`None` stops push).
    Push(Option<PushTarget>),
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
    Progress {
        account: AccountId,
        text: String,
        detail: Option<SyncProgress>,
    },
    OpDone {
        account: AccountId,
        op: u64,
        result: Result<OpOutcome, OpError>,
    },
    Sent {
        account: AccountId,
        item: ItemId,
        result: Result<(), String>,
    },
    Folders {
        account: AccountId,
        folders: Vec<RemoteFolder>,
    },
    Delta {
        account: AccountId,
        name: String,
        delta: SyncDelta,
    },
    /// Headers of messages a server search found in IMAP folder `name` (`total`: matches not held here).
    Found {
        account: AccountId,
        name: String,
        generation: u64,
        msgs: Vec<NewMessage>,
        total: usize,
    },
    /// A server search could not run.
    SearchFailed {
        account: AccountId,
        error: String,
    },
    /// Whole messages for items first downloaded as headers (IMAP folder name and UIDs).
    Bodies {
        account: AccountId,
        name: String,
        msgs: Vec<NewMessage>,
    },
    FolderFailed {
        account: AccountId,
        name: String,
        error: String,
    },
    Done {
        account: AccountId,
        error: Option<String>,
    },
    Test {
        account: AccountId,
        report: TestReport,
    },
    Push {
        account: AccountId,
        mode: PushMode,
        error: Option<String>,
    },
    /// The server reported a change in the folder the worker IDLEs on (JMAP: a new Email or Mailbox state).
    PushChanged {
        account: AccountId,
    },
    /// The mailbox quota the server reported (`None` = no limits).
    Quota {
        account: AccountId,
        quota: Option<mailcraft_net::sync::Quota>,
    },
    /// The transfer rate a run measured (bytes per second).
    LinkSpeed {
        account: AccountId,
        bytes_per_sec: u64,
    },
    Pop3New {
        account: AccountId,
        uidl: String,
        raw: Vec<u8>,
        size: Option<u64>,
        headers_only: bool,
    },
    Pop3Full {
        account: AccountId,
        uidl: String,
        raw: Vec<u8>,
    },
    /// After a POP3 session ended cleanly: the unique ids on the server and those deleted.
    Pop3Server {
        account: AccountId,
        present: Vec<String>,
        deleted: Vec<String>,
    },
    Jmap {
        account: AccountId,
        ev: super::jmap::JmapEvent,
    },
    /// Contacts, tasks and published calendars (see `pim.rs`).
    Dav {
        account: AccountId,
        ev: Box<super::pim::PimEvent>,
    },
    /// Other users' and shared mailboxes the account's login can open (RFC 2342).
    SharedRoots {
        account: AccountId,
        roots: Vec<SharedRoot>,
    },
    /// A refreshed OAuth token (store it).
    Tokens {
        account: AccountId,
        tokens: TokenSet,
    },
    OAuthPrompt {
        account: AccountId,
        prompt: Value,
    },
    OAuthDone {
        account: AccountId,
        result: Result<TokenSet, String>,
    },
}

impl Event {
    /// A step without counts ("Connecting…").
    pub(super) fn step(account: AccountId, text: impl Into<String>) -> Event {
        Event::Progress { account, text: text.into(), detail: None }
    }
    /// A step with folder and message counts; the text says them.
    pub(super) fn counted(account: AccountId, detail: &SyncProgress) -> Event {
        Event::Progress { account, text: detail.text(), detail: Some(detail.clone()) }
    }
    fn account(&self) -> AccountId {
        match self {
            Event::Progress { account, .. }
            | Event::OpDone { account, .. }
            | Event::Sent { account, .. }
            | Event::Folders { account, .. }
            | Event::Delta { account, .. }
            | Event::Found { account, .. }
            | Event::SearchFailed { account, .. }
            | Event::Bodies { account, .. }
            | Event::FolderFailed { account, .. }
            | Event::Done { account, .. }
            | Event::Test { account, .. }
            | Event::Push { account, .. }
            | Event::PushChanged { account }
            | Event::LinkSpeed { account, .. }
            | Event::Quota { account, .. }
            | Event::Pop3New { account, .. }
            | Event::Pop3Full { account, .. }
            | Event::Pop3Server { account, .. }
            | Event::Jmap { account, .. }
            | Event::Dav { account, .. }
            | Event::SharedRoots { account, .. }
            | Event::Tokens { account, .. }
            | Event::OAuthPrompt { account, .. }
            | Event::OAuthDone { account, .. } => *account,
        }
    }
}

struct Worker {
    jobs: mpsc::Sender<Msg>,
    cancel: Arc<AtomicBool>,
    /// Raised when a message is sent to the worker: ends an IDLE so the message is read at once.
    wake: Arc<AtomicBool>,
    /// Tags this worker's events: results of a worker that was replaced or forgotten (account removed,
    /// its id reused, its server changed) are dropped instead of being applied to the wrong account.
    serial: u64,
}

/// Serial of events not tied to a worker (Test Account Settings, OAuth sign-in).
const NO_WORKER: u64 = 0;

/// Where a worker sends its events.
#[derive(Clone)]
pub(super) struct Events {
    tx: mpsc::Sender<(u64, Event)>,
    serial: u64,
}

impl Events {
    /// False when the session is gone.
    pub(super) fn send(&self, e: Event) -> bool {
        self.tx.send((self.serial, e)).is_ok()
    }

    /// Events sent to `tx` (unit tests of a worker part).
    #[cfg(test)]
    pub(super) fn to(tx: mpsc::Sender<(u64, Event)>) -> Events {
        Events { tx, serial: NO_WORKER }
    }
}

pub(super) struct Runtime {
    tx: mpsc::Sender<(u64, Event)>,
    rx: mpsc::Receiver<(u64, Event)>,
    workers: BTreeMap<AccountId, Worker>,
    last_serial: u64,
    /// OAuth sign-ins in progress: their cancel flags.
    oauth: BTreeMap<AccountId, Arc<AtomicBool>>,
}

impl Default for Runtime {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Runtime { tx, rx, workers: BTreeMap::new(), last_serial: NO_WORKER, oauth: BTreeMap::new() }
    }
}

impl Drop for Runtime {
    /// The session is closing: workers stop after their current step (dropping the job senders then ends
    /// their threads) instead of finishing a long download nobody will apply.
    fn drop(&mut self) {
        for w in self.workers.values() {
            w.cancel.store(true, Ordering::SeqCst);
            w.wake.store(true, Ordering::SeqCst);
        }
        for c in self.oauth.values() {
            c.store(true, Ordering::SeqCst);
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
            w.wake.store(true, Ordering::SeqCst);
        }
        if let Some(c) = self.oauth.remove(&id) {
            c.store(true, Ordering::SeqCst);
        }
    }

    pub(super) fn has_worker(&self, id: AccountId) -> bool {
        self.workers.contains_key(&id)
    }

    /// Starts or stops push on an existing worker. False when the account has no worker.
    pub(super) fn push(&mut self, id: AccountId, target: Option<PushTarget>) -> bool {
        let Some(w) = self.workers.get(&id) else { return false };
        w.wake.store(true, Ordering::SeqCst);
        if w.jobs.send(Msg::Push(target)).is_err() {
            self.workers.remove(&id);
            return false;
        }
        true
    }

    fn submit(&mut self, account: AccountId, job: Job) -> Result<(), String> {
        let mut job = Some(Box::new(job));
        for _ in 0..2 {
            if !self.workers.contains_key(&account) {
                let (jtx, jrx) = mpsc::channel::<Msg>();
                let cancel = Arc::new(AtomicBool::new(false));
                let wake = Arc::new(AtomicBool::new(false));
                self.last_serial = self.last_serial.saturating_add(1);
                let serial = self.last_serial;
                let events = Events { tx: self.tx.clone(), serial };
                let (c2, w2) = (Arc::clone(&cancel), Arc::clone(&wake));
                std::thread::Builder::new()
                    .name(format!("mailcraft-sync-{}", account.0))
                    .spawn(move || worker_main(account, jrx, events, c2, w2))
                    .map_err(|e| format!("cannot start the send/receive thread: {e}"))?;
                self.workers.insert(account, Worker { jobs: jtx, cancel, wake, serial });
            }
            let Some(w) = self.workers.get(&account) else { continue };
            w.cancel.store(false, Ordering::SeqCst);
            let Some(j) = job.take() else { break };
            w.wake.store(true, Ordering::SeqCst);
            match w.jobs.send(Msg::Run(j)) {
                Ok(()) => return Ok(()),
                Err(mpsc::SendError(Msg::Run(j))) => {
                    // The worker ended (it should not): start a new one.
                    job = Some(j);
                    self.workers.remove(&account);
                }
                Err(_) => {
                    self.workers.remove(&account);
                }
            }
        }
        Err("the send/receive thread is not running".into())
    }

    /// An event sender not tied to a worker (OAuth sign-in threads, tests).
    pub(super) fn loose_events(&self) -> Events {
        Events { tx: self.tx.clone(), serial: NO_WORKER }
    }
}

// ---------------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------------

pub(super) fn connect_options(srv: &ServerSettings) -> Result<ConnectOptions, String> {
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

/// The OAuth provider of an account's sign-in (registration from the options, secret from the store).
pub(super) fn oauth_provider(s: &Session, id: AccountId) -> Option<OAuthProvider> {
    let b = account_options(s, id).oauth?;
    Some(OAuthProvider {
        id: b.provider,
        name: b.name,
        authorization_endpoint: b.authorization_endpoint,
        token_endpoint: b.token_endpoint,
        device_authorization_endpoint: b.device_authorization_endpoint,
        client_id: b.client_id,
        client_secret: s.secrets.get(&client_secret_key(id)),
        scopes: b.scopes,
        domains: Vec::new(),
        extra_auth_params: b.extra_auth_params,
    })
}

/// HTTP options for talking to OAuth endpoints (https only; tests may allow a local plain server).
pub(super) fn oauth_http_options(s: &Session) -> ConnectOptions {
    s.sync.oauth_http.clone().unwrap_or_else(|| ConnectOptions {
        connect_timeout: Duration::from_secs(20),
        io_timeout: Duration::from_secs(60),
        ..ConnectOptions::default()
    })
}

pub(super) fn conn_settings(s: &Session, a: &Account) -> Result<Conn, String> {
    if let Some(sh) = &a.shared {
        // Another user's or a shared mailbox: the owner's login, IMAP only, nothing to send or refresh.
        let owner = s
            .mailbox
            .account(sh.owner)
            .filter(|o| o.shared.is_none() && o.kind == AccountKind::Imap)
            .ok_or("The account this shared mailbox belongs to is gone.")?;
        let mut c = conn_settings(s, owner)?;
        if !matches!(c.incoming, Incoming::Imap(_)) {
            return Err("Shared mailboxes need an IMAP account.".into());
        }
        c.smtp = None;
        c.oauth = None;
        return Ok(c);
    }
    let inc = a.incoming.as_ref().ok_or("The account has no incoming mail server.")?;
    if inc.host.trim().is_empty() {
        return Err("The account has no incoming mail server.".into());
    }
    let secret = account_secret(s, a.id, false)
        .ok_or_else(|| format!("Enter the password for {} (Account Settings).", a.address))?;
    let user = if inc.username.is_empty() { a.address.clone() } else { inc.username.clone() };
    let creds = credentials(a, &user, secret);
    let incoming = if a.kind == AccountKind::Jmap {
        Incoming::Jmap(super::jmap::JmapConn::new(&inc.host, &creds, connect_options(inc)?))
    } else if crate::accounts::is_pop3(s, a.id) {
        let mut p = Pop3Settings::from_server(inc, creds);
        p.options = connect_options(inc)?;
        Incoming::Pop3(p)
    } else {
        let mut imap = ImapSettings::from_server(inc, creds);
        imap.options = connect_options(inc)?;
        Incoming::Imap(imap)
    };
    let smtp = match &a.outgoing {
        Some(o) if !o.host.trim().is_empty() && a.kind != AccountKind::Jmap => {
            let mut st = SmtpSettings::new(o.host.clone(), o.port, o.security);
            st.auth_mechanism = o.auth.clone();
            st.options = connect_options(o)?;
            let user = if o.username.is_empty() { a.address.clone() } else { o.username.clone() };
            st.credentials = account_secret(s, a.id, true).map(|sec| credentials(a, &user, sec));
            Some(st)
        }
        _ => None,
    };
    let oauth = if a.credential == CredentialKind::OAuthToken {
        match (oauth_provider(s, a.id), s.secrets.get(&refresh_key(a.id))) {
            (Some(provider), Some(refresh_token)) => Some(OAuthRefresh {
                provider,
                refresh_token,
                expires_at: account_options(s, a.id).oauth.and_then(|b| b.expires_at),
                options: oauth_http_options(s),
            }),
            _ => None,
        }
    } else {
        None
    };
    Ok(Conn { incoming, smtp, oauth, renewed: None })
}

/// What a JMAP account's worker listens to for push (`None` when the account cannot connect: no password).
pub(super) fn jmap_push_target(s: &Session, id: AccountId) -> Option<PushTarget> {
    let a = s.mailbox.account(id)?;
    let conn = conn_settings(s, a).ok()?;
    let Incoming::Jmap(conn) = conn.incoming else { return None };
    let st = s.sync.prefs.data.jmap.get(&id.0).cloned().unwrap_or_default();
    Some(PushTarget::Jmap { conn, email_state: st.email_state, mailbox_state: st.mailbox_state })
}

// ---------------------------------------------------------------------------------------------------
// Dispatch (session thread)
// ---------------------------------------------------------------------------------------------------

/// Outbox messages due for sending, oldest first.
fn outgoing_items(s: &Session, a: &Account, plan: &Plan, now: i64) -> Vec<Outgoing> {
    let mut outgoing = Vec::new();
    let can = a.outgoing.is_some() || a.kind == AccountKind::Jmap;
    if !plan.send || !can {
        return outgoing;
    }
    let Some(outbox) = s.mailbox.role_folder(a.id, FolderRole::Outbox) else { return outgoing };
    let mut items: Vec<&MailItem> = s
        .mailbox
        .items_in(outbox)
        .into_iter()
        .filter(|m| !m.is_draft && m.sent.is_none_or(|t| t.unix <= now))
        .collect();
    items.sort_by_key(|m| (m.received.unix, m.id));
    // Options › Advanced › "Allow international domain names": the envelope carries the ASCII form too, like
    // the headers (`intl::apply_outgoing`), so a server without SMTPUTF8 accepts it.
    let idn = s.settings.bool("advanced.idn");
    let envelope = |addr: &str| {
        let addr = addr.trim();
        if idn {
            mailcraft_mime::idn::addr_to_ascii(addr).unwrap_or_else(|_| addr.to_owned())
        } else {
            addr.to_owned()
        }
    };
    for m in items {
        let mut rcpts: Vec<String> = Vec::new();
        for addr in m.to.iter().chain(&m.cc).chain(&m.bcc) {
            let x = envelope(&addr.addr);
            if !x.is_empty() && !rcpts.iter().any(|r| r.eq_ignore_ascii_case(&x)) {
                rcpts.push(x);
            }
        }
        let from = if a.address.trim().is_empty() {
            m.from.as_ref().map(|f| envelope(&f.addr)).unwrap_or_default()
        } else {
            envelope(&a.address)
        };
        outgoing.push(Outgoing { item: m.id, from, rcpts, raw: item_source(s, m) });
    }
    outgoing
}

/// The IMAP receive part of a run: known UIDs and states per server folder.
fn imap_receive(s: &Session, a: &Account, plan: &Plan, sending: bool) -> Option<Receive> {
    let id = a.id;
    // Sending before the first update: list the folders so the sent copy can be filed on the server.
    let sent_unmapped = s
        .mailbox
        .role_folder(id, FolderRole::SentItems)
        .and_then(|f| s.mailbox.folder(f))
        .is_some_and(|f| f.remote_name.is_none());
    let list_only = !plan.receive && sending && sent_unmapped && !a.server_saves_sent;
    (plan.receive || list_only).then(|| {
        let mut uids: BTreeMap<FolderId, Vec<u32>> = BTreeMap::new();
        for m in s.mailbox.mail.items() {
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
                fs.iter().filter_map(|f| s.mailbox.folder(*f).and_then(|f| f.remote_name.clone())).collect()
            })
        };
        let scope = match &a.shared {
            Some(sh) => ListScope::Store { root: sh.root.clone(), delimiter: sh.delimiter },
            None => ListScope::Personal,
        };
        let shared_roots = a.shared.is_none() && plan.receive && plan.folders.is_none() && !list_only;
        Receive { only, states, scope, shared_roots }
    })
}

/// IMAP: whole-message downloads for marked header-only items, grouped by server folder.
fn imap_fetch_list(s: &Session, id: AccountId, items: &[ItemId]) -> Vec<(String, Vec<u32>)> {
    let mut by_folder: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for i in items {
        let Some(m) = s.mailbox.mail.get(*i) else { continue };
        let Some(uid) = uid_of(m) else { continue };
        let Some(f) = s.mailbox.folder(m.folder).filter(|f| f.account == id) else { continue };
        let Some(name) = &f.remote_name else { continue };
        by_folder.entry(name.clone()).or_default().push(uid);
    }
    by_folder.into_iter().collect()
}

/// The POP3 unique id of an item (`remote_id` = `pop3:<uidl>`).
pub(super) fn pop3_uidl(m: &MailItem) -> Option<&str> {
    m.remote_id.as_deref()?.strip_prefix(POP3_PREFIX)
}

/// The POP3 part of a run: known unique ids, server copies to delete (Leave a copy on the server, Remove
/// from server after N days, Remove when deleted here) and marked messages to download in full.
fn pop3_job(
    s: &Session,
    a: &Account,
    receive: bool,
    force_headers: bool,
    fetch: &[ItemId],
    now: i64,
) -> Option<Pop3Job> {
    let opts = account_options(s, a.id);
    let folders: BTreeSet<FolderId> =
        s.mailbox.folders.values().filter(|f| f.account == a.id).map(|f| f.id).collect();
    let prefs = &s.sync.prefs.data;
    let header_only = |m: &MailItem| prefs.headers_only.contains(&m.id.0) && m.blob.is_none();
    // Unique id → (item, downloaded in full).
    let mut local: BTreeMap<String, (ItemId, bool)> = BTreeMap::new();
    for m in s.mailbox.mail.items().filter(|m| folders.contains(&m.folder)) {
        if let Some(u) = pop3_uidl(m) {
            local.insert(u.to_owned(), (m.id, !header_only(m)));
        }
    }
    // Unique ids still held anywhere (an item moved into another data file keeps its id): such mail was
    // archived, not deleted, so "Remove from server when deleted" leaves its server copy alone.
    let anywhere: BTreeSet<&str> = s.mailbox.mail.items().filter_map(pop3_uidl).collect();
    let st = prefs.pop3.get(&a.id.0).cloned().unwrap_or_default();
    let mut known: BTreeSet<String> = st.seen.keys().cloned().collect();
    known.extend(local.keys().cloned());
    let mut delete = st.delete.clone();
    let full_here = |u: &str| local.get(u).is_some_and(|(_, full)| *full);
    for (u, first) in &st.seen {
        let gone_here = !local.contains_key(u);
        let header_only_here = local.get(u).is_some_and(|(_, full)| !*full);
        if header_only_here {
            continue;
        }
        if !opts.leave_on_server && (full_here(u) || gone_here) {
            delete.insert(u.clone());
        } else if opts.leave_on_server {
            let expired = opts
                .remove_after_days
                .is_some_and(|d| first.saturating_add(i64::from(d).saturating_mul(86_400)) <= now);
            if expired || (opts.remove_when_deleted && gone_here && !anywhere.contains(u.as_str())) {
                delete.insert(u.clone());
            }
        }
    }
    let full: BTreeSet<String> = fetch
        .iter()
        .filter_map(|i| s.mailbox.mail.get(*i))
        .filter(|m| folders.contains(&m.folder))
        .filter_map(|m| pop3_uidl(m).map(str::to_owned))
        .collect();
    for u in &full {
        delete.remove(u);
    }
    if !receive && full.is_empty() && delete.is_empty() {
        return None;
    }
    Some(Pop3Job {
        receive,
        known,
        delete,
        full,
        headers_only: force_headers || opts.download.headers_first(),
    })
}

pub(super) fn dispatch(s: &mut Session, id: AccountId, plan: Plan) -> Result<bool, String> {
    let Some(a) = s.mailbox.account(id).cloned() else { return Ok(false) };
    match a.kind {
        AccountKind::Imap | AccountKind::Jmap => {}
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
    crate::accounts::load_prefs(s);
    let conn = conn_settings(s, &a)?;
    let opts = account_options(s, id);
    let now = s.now().unix;
    let fetch_items: Vec<ItemId> = s.sync.fetch.get(&id).cloned().unwrap_or_default();
    let force_headers = s.sync.force_headers.contains(&id);
    // On Slow Connections Download Only Headers: like Download Headers, without the bodies following.
    let slow = super::slow_headers_only(s, id);
    let window = opts.download_months.map(|m| mailcraft_net::sync::months_before(now, m));
    // Contacts, tasks and calendars go to the DAV server; the mail part only sees mail folders.
    let jmap_conn = match &conn.incoming {
        Incoming::Jmap(c) => Some(c),
        _ => None,
    };
    let dav = super::dav::build_job(s, &a, &plan, jmap_conn).filter(|j| !j.is_empty());
    let full_plan = plan;
    let plan = super::dav::mail_only(s, id, &full_plan);
    let outgoing = outgoing_items(s, &a, &plan, now);
    let mut job = Job {
        conn,
        ops: Vec::new(),
        outgoing,
        receive: None,
        headers_only: false,
        then_full: false,
        fetch: Vec::new(),
        pop3: None,
        jmap: None,
        dav,
        search: None,
        jmap_search: None,
    };
    // Search › Include Older Results: a server search waiting for this account.
    let older = s.sync.older_requests.get(&id).cloned();
    match &mut job.conn.incoming {
        Incoming::Imap(settings) => {
            settings.first_sync_since = window;
            job.ops = job_ops(s, &a, &plan, now);
            job.receive = imap_receive(s, &a, &plan, !job.outgoing.is_empty());
            job.headers_only =
                job.receive.is_some() && (force_headers || slow || opts.download.headers_first());
            job.then_full =
                job.headers_only && !force_headers && !slow && opts.download == DownloadMode::HeadersThenFull;
            job.fetch = imap_fetch_list(s, id, &fetch_items);
            job.search = older.as_ref().and_then(|r| imap_search(s, &a, r));
        }
        Incoming::Pop3(_) => {
            job.pop3 = pop3_job(s, &a, plan.receive, force_headers || slow, &fetch_items, now);
        }
        Incoming::Jmap(_) => {
            job.jmap = super::jmap::build_job(s, &a, &plan, window);
            job.jmap_search = older.as_ref().and_then(|r| super::jmap::build_search(s, &a, r));
        }
    }
    if older.is_some() && job.search.is_none() && job.jmap_search.is_none() {
        // Nothing of this account to search after all (no folder of it in the scope).
        s.sync.older_requests.remove(&id);
        super::older::run_ended(s, id, None);
    }
    if job.ops.is_empty()
        && job.outgoing.is_empty()
        && job.receive.is_none()
        && job.fetch.is_empty()
        && job.pop3.is_none()
        && job.jmap.is_none()
        && job.dav.is_none()
        && job.search.is_none()
        && job.jmap_search.is_none()
    {
        s.sync.fetch.remove(&id);
        s.sync.force_headers.remove(&id);
        return Ok(false);
    }
    let inflight = InFlight {
        ops: job.ops.iter().map(|(i, _, _)| *i).collect(),
        sending: job.outgoing.iter().map(|o| o.item).collect(),
    };
    let receiving = job.receive.is_some()
        || !job.ops.is_empty()
        || job.pop3.is_some()
        || job.jmap.is_some()
        || job.dav.is_some();
    let progress = if receiving || !job.fetch.is_empty() { "Connecting…" } else { "Sending…" };
    s.sync.rt.submit(id, job)?;
    s.sync.older_requests.remove(&id);
    s.sync.fetch.remove(&id);
    s.sync.force_headers.remove(&id);
    let run = s.sync.runs.entry(id).or_default();
    run.busy = Some(inflight);
    run.progress = progress.into();
    run.detail = None;
    run.error = None;
    Ok(true)
}

// ---------------------------------------------------------------------------------------------------
// Worker thread
// ---------------------------------------------------------------------------------------------------

/// Push state kept by a worker between jobs.
struct PushLoop {
    target: Option<PushTarget>,
    /// The open JMAP event stream.
    jmap: Option<JmapPush>,
    failures: u32,
    retry_at: Option<Instant>,
    reported: Option<PushMode>,
}

/// An open JMAP event source and what it last reported.
struct JmapPush {
    stream: mailcraft_net::eventsource::EventStream,
    account_id: String,
    email_state: String,
    mailbox_state: String,
    last_data: Instant,
}

impl PushLoop {
    fn report(&mut self, account: AccountId, events: &Events, mode: PushMode, error: Option<String>) {
        if self.reported != Some(mode) || error.is_some() {
            self.reported = Some(mode);
            events.send(Event::Push { account, mode, error });
        }
    }

    /// The push connection failed: try again after a growing delay.
    fn lost(&mut self, account: AccountId, events: &Events, e: &NetError) {
        if let Some(mut j) = self.jmap.take() {
            j.stream.close();
        }
        self.failures = self.failures.saturating_add(1);
        self.retry_at = Some(mailcraft_net::transport::deadline_after(push_backoff(self.failures)));
        if matches!(e, NetError::Auth(_)) {
            // A password or token problem will not fix itself: the next run reports it.
            self.target = None;
            self.report(account, events, PushMode::Off, Some(e.to_string()));
        } else {
            self.report(account, events, PushMode::Reconnecting, Some(e.to_string()));
        }
    }
}

fn push_backoff(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(16);
    PUSH_BACKOFF_BASE.saturating_mul(1u32 << shift).min(PUSH_BACKOFF_MAX)
}

fn worker_main(
    account: AccountId,
    jobs: mpsc::Receiver<Msg>,
    events: Events,
    cancel: Arc<AtomicBool>,
    wake: Arc<AtomicBool>,
) {
    let mut imap: Option<ImapAccount> = None;
    let mut push = PushLoop { target: None, jmap: None, failures: 0, retry_at: None, reported: None };
    loop {
        let idling = match &push.target {
            Some(PushTarget::Imap(_)) => imap.is_some(),
            Some(PushTarget::Jmap { .. }) => true,
            None => false,
        };
        let msg = if idling {
            match jobs.try_recv() {
                Ok(m) => Some(m),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        } else {
            match jobs.recv() {
                Ok(m) => Some(m),
                Err(_) => break,
            }
        };
        match msg {
            Some(Msg::Push(target)) => {
                wake.store(false, Ordering::SeqCst);
                if target.is_none() {
                    push.report(account, &events, PushMode::Off, None);
                }
                if push.target != target
                    && let Some(mut j) = push.jmap.take()
                {
                    j.stream.close();
                }
                push.target = target;
                push.failures = 0;
                push.retry_at = None;
                continue;
            }
            Some(Msg::Run(mut job)) => {
                wake.store(false, Ordering::SeqCst);
                let res = catch_unwind(AssertUnwindSafe(|| {
                    run_job(account, &mut imap, &mut job, &events, &cancel)
                }));
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
                // A job reconnects anyway: retry push now.
                push.retry_at = None;
                continue;
            }
            None => {}
        }
        // Idle step: wait out a reconnect delay, or listen until a change, a message or the slice ends.
        if let Some(t) = push.retry_at {
            if Instant::now() < t {
                let until = t.min(mailcraft_net::transport::deadline_after(Duration::from_millis(200)));
                while Instant::now() < until && !wake.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                continue;
            }
            push.retry_at = None;
        }
        match push.target.clone() {
            Some(PushTarget::Imap(folder)) => {
                let Some(acc) = imap.as_mut() else { continue };
                if !imap_push_step(account, acc, &folder, &mut push, &events, &wake) {
                    imap = None;
                }
            }
            Some(PushTarget::Jmap { conn, email_state, mailbox_state }) => {
                let res = catch_unwind(AssertUnwindSafe(|| {
                    jmap_push_step(account, &conn, (&email_state, &mailbox_state), &mut push, &events, &wake)
                }));
                if res.is_err() {
                    push.jmap = None;
                    push.target = None;
                    push.report(
                        account,
                        &events,
                        PushMode::Off,
                        Some("internal error while waiting for mail".into()),
                    );
                }
            }
            None => {}
        }
    }
    if let Some(mut j) = push.jmap.take() {
        j.stream.close();
    }
    if let Some(mut a) = imap {
        a.disconnect();
    }
}

/// One IMAP IDLE (until a change, a message or the slice ends). False when the connection must be dropped.
fn imap_push_step(
    account: AccountId,
    acc: &mut ImapAccount,
    folder: &str,
    push: &mut PushLoop,
    events: &Events,
    wake: &AtomicBool,
) -> bool {
    // Connected already: say what happens next before the (long) IDLE starts.
    match acc.client().map(|c| c.has_capability("IDLE") || c.has_capability("IMAP4rev2")) {
        Some(true) => push.report(account, events, PushMode::Idle, None),
        Some(false) => {
            push.report(account, events, PushMode::Polling, None);
            push.target = None;
            return true;
        }
        None => {}
    }
    let started = Instant::now();
    let res = catch_unwind(AssertUnwindSafe(|| acc.idle_once(folder, IDLE_SLICE, Some(wake))));
    match res {
        Ok(Ok(changed)) => {
            push.failures = 0;
            push.report(account, events, PushMode::Idle, None);
            if changed {
                events.send(Event::PushChanged { account });
            }
            // A server that ends IDLE at once must not turn this loop into a busy one.
            if !changed && !wake.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(1) {
                push.retry_at = Some(mailcraft_net::transport::deadline_after(Duration::from_secs(1)));
            }
            true
        }
        Ok(Err(NetError::Unsupported(_))) => {
            push.report(account, events, PushMode::Polling, None);
            push.target = None;
            true
        }
        Ok(Err(e)) => {
            push.lost(account, events, &e);
            true
        }
        Err(_) => {
            push.target = None;
            push.report(account, events, PushMode::Off, Some("internal error while waiting for mail".into()));
            false
        }
    }
}

/// One JMAP push step: opens the event source when needed, then waits up to [`JMAP_PUSH_SLICE`] for an
/// event. A `StateChange` naming a new Email or Mailbox state of the account is reported as new mail.
fn jmap_push_step(
    account: AccountId,
    conn: &super::jmap::JmapConn,
    known: (&str, &str),
    push: &mut PushLoop,
    events: &Events,
    wake: &AtomicBool,
) {
    if push.jmap.is_none() {
        let opened = conn.connect().and_then(|c| {
            let stream =
                c.open_events(&["Email", "Mailbox"], u32::try_from(JMAP_PING.as_secs()).unwrap_or(60))?;
            Ok((stream, c.account_id().to_owned()))
        });
        match opened {
            Ok((stream, account_id)) => {
                push.failures = 0;
                push.jmap = Some(JmapPush {
                    stream,
                    account_id,
                    email_state: known.0.to_owned(),
                    mailbox_state: known.1.to_owned(),
                    last_data: Instant::now(),
                });
                push.report(account, events, PushMode::Idle, None);
            }
            Err(NetError::Unsupported(_)) => {
                push.report(account, events, PushMode::Polling, None);
                push.target = None;
            }
            Err(e) => push.lost(account, events, &e),
        }
        return;
    }
    let Some(j) = push.jmap.as_mut() else { return };
    if wake.load(Ordering::SeqCst) {
        return;
    }
    match j.stream.next_event(JMAP_PUSH_SLICE) {
        Ok(Some(ev)) => {
            j.last_data = Instant::now();
            if ev.event != "state" {
                return;
            }
            let Some(sc) = mailcraft_net::jmap::StateChange::parse(&ev.data) else { return };
            let mut changed = false;
            if let Some(st) = sc.state_of(&j.account_id, "Email")
                && st != j.email_state
            {
                st.clone_into(&mut j.email_state);
                changed = true;
            }
            if let Some(st) = sc.state_of(&j.account_id, "Mailbox")
                && st != j.mailbox_state
            {
                st.clone_into(&mut j.mailbox_state);
                changed = true;
            }
            if changed {
                events.send(Event::PushChanged { account });
            }
        }
        Ok(None) => {
            // The server pings at least every JMAP_PING: silence for much longer means a dead connection.
            if j.last_data.elapsed() > JMAP_PING.saturating_mul(2).saturating_add(Duration::from_secs(30)) {
                push.lost(account, events, &NetError::Timeout("no events from the JMAP server".into()));
            }
        }
        Err(e) => push.lost(account, events, &e),
    }
}

/// Bytes downloaded and time spent downloading them, for the slow-connection check.
#[derive(Default)]
struct RateMeter {
    bytes: u64,
    time: Duration,
}

impl RateMeter {
    fn add(&mut self, bytes: usize, time: Duration) {
        if bytes > 0 {
            self.bytes = self.bytes.saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
            self.time = self.time.saturating_add(time);
        }
    }
    /// Reports the rate when enough was downloaded to tell.
    fn report(&self, account: AccountId, events: &Events) {
        if self.bytes < MIN_RATE_SAMPLE {
            return;
        }
        let ms = u64::try_from(self.time.as_millis()).unwrap_or(u64::MAX).max(1);
        let bytes_per_sec = self.bytes.saturating_mul(1000) / ms;
        events.send(Event::LinkSpeed { account, bytes_per_sec });
    }
}

fn raw_len(msgs: &[NewMessage]) -> usize {
    msgs.iter().map(|m| m.raw.len()).fold(0usize, usize::saturating_add)
}

fn transient(e: &NetError) -> bool {
    e.is_connection_lost()
        || matches!(e, NetError::Connect(_) | NetError::Auth(_) | NetError::Insecure(_) | NetError::Tls(_))
}

/// Refreshes the OAuth access token when it expires soon (or `force`), updating the job's credentials and
/// telling the session to store the new tokens. Errors are reported as the run's error by the caller.
fn refresh_token(account: AccountId, conn: &mut Conn, events: &Events, force: bool) -> Result<bool, String> {
    let Some(o) = conn.oauth.as_mut() else { return Ok(false) };
    let now = mailcraft_net::oauth::now_unix();
    let due = o.expires_at.is_some_and(|t| now.saturating_add(TOKEN_REFRESH_SKEW) >= t);
    if !force && !due {
        return Ok(false);
    }
    let http = mailcraft_net::http::HttpClient::new(o.options.clone());
    let tokens = mailcraft_net::oauth::refresh(&http, &o.provider, &o.refresh_token)
        .map_err(|e| format!("The sign-in could not be renewed ({e}). Sign in again in Account Settings."))?;
    o.expires_at = tokens.expires_at;
    if let Some(r) = &tokens.refresh_token {
        o.refresh_token = r.clone();
    }
    let access = tokens.access_token.clone();
    conn.set_token(&access);
    events.send(Event::Tokens { account, tokens });
    Ok(true)
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
        RemoteOp::CreateFolder { name, special_use } => {
            imap.create_folder(name, special_use.as_deref()).map(|()| OpOutcome::Done)
        }
        RemoteOp::RenameFolder { from, to } => imap.rename_folder(from, to).map(|()| OpOutcome::Done),
        RemoteOp::DeleteFolder { name } => imap.delete_folder(name).map(|()| OpOutcome::Done),
    };
    res.map_err(|e| OpError { transient: transient(&e), message: e.to_string() })
}

fn run_job(
    account: AccountId,
    slot: &mut Option<ImapAccount>,
    job: &mut Job,
    events: &Events,
    cancel: &Arc<AtomicBool>,
) -> Option<String> {
    if let Err(e) = refresh_token(account, &mut job.conn, events, false) {
        return Some(e);
    }
    let mut meter = RateMeter::default();
    let mail_part = job.receive.is_some()
        || !job.ops.is_empty()
        || !job.outgoing.is_empty()
        || !job.fetch.is_empty()
        || job.pop3.is_some()
        || job.jmap.is_some()
        || job.search.is_some()
        || job.jmap_search.is_some();
    let res = if !mail_part {
        None
    } else {
        match job.conn.incoming.clone() {
            Incoming::Imap(settings) => run_imap(account, slot, settings, job, events, cancel, &mut meter),
            Incoming::Pop3(settings) => {
                if let Some(mut old) = slot.take() {
                    old.disconnect();
                }
                run_pop3(account, &settings, job, events, cancel, &mut meter)
            }
            Incoming::Jmap(conn) => {
                if let Some(mut old) = slot.take() {
                    old.disconnect();
                }
                super::jmap::run(
                    account,
                    &conn,
                    job.jmap.as_ref(),
                    job.jmap_search.as_ref(),
                    &job.outgoing,
                    events,
                    cancel,
                )
            }
        }
    };
    meter.report(account, events);
    // Contacts, tasks and published calendars: after the mail, also when the mail part failed (another
    // server may well be reachable). A token renewed above (or by the mail part) replaces the one the job was
    // built with.
    let dav = match job.dav.as_mut() {
        Some(d) if !cancel.load(Ordering::SeqCst) => {
            if let Some(mailcraft_net::dav::DavAuth::Bearer(t)) = &job.conn.renewed {
                d.renew_token(t);
            }
            let mut end = super::dav::run(account, d, events, cancel);
            // The token was refused (revoked, or expired early while the mail connection stayed signed in):
            // renew it once and try again.
            if end.refused && d.uses_token() && job.conn.oauth.is_some() {
                end = match refresh_token(account, &mut job.conn, events, true) {
                    Ok(_) => {
                        if let Some(mailcraft_net::dav::DavAuth::Bearer(t)) = &job.conn.renewed {
                            d.renew_token(t);
                        }
                        super::dav::run(account, d, events, cancel)
                    }
                    Err(e) => super::dav::DavEnd { error: Some(e), refused: true },
                };
            }
            end.error
        }
        _ => None,
    };
    res.or(dav)
}

fn run_imap(
    account: AccountId,
    slot: &mut Option<ImapAccount>,
    mut settings: ImapSettings,
    job: &mut Job,
    events: &Events,
    cancel: &AtomicBool,
    meter: &mut RateMeter,
) -> Option<String> {
    let send = |e: Event| {
        if !events.send(e) {
            // Nobody is listening any more (the session closed): stop at the next check.
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let progress = |t: &str| send(Event::step(account, t));
    let cancelled = || cancel.load(Ordering::SeqCst);
    if slot.as_ref().is_some_and(|a| a.settings() != &settings)
        && let Some(mut old) = slot.take()
    {
        old.disconnect();
    }
    let mut imap = slot.take().unwrap_or_else(|| ImapAccount::new(settings.clone()));
    // An expired OAuth token shows as a login failure: renew it once and log in again.
    if job.conn.oauth.is_some()
        && let Err(NetError::Auth(_)) = imap.connect().map(|_| ())
    {
        match refresh_token(account, &mut job.conn, events, true) {
            Ok(_) => {
                if let Incoming::Imap(s2) = &job.conn.incoming {
                    settings = s2.clone();
                }
                imap = ImapAccount::new(settings.clone());
            }
            Err(e) => return Some(e),
        }
    }
    let imap = slot.insert(imap);
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
        if rcv.shared_roots {
            match imap.shared_roots() {
                Ok(roots) => send(Event::SharedRoots { account, roots }),
                Err(e) if e.is_connection_lost() => error = Some(e.to_string()),
                Err(e) => log::info!("imap: no shared mailboxes listed: {e}"),
            }
        }
        match imap.sync_folder_list_in(&rcv.scope) {
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
        // The mailbox quota (RFC 9208), when the server has the extension.
        if error.is_none() {
            match imap.quota() {
                Ok(quota) => send(Event::Quota { account, quota }),
                Err(e) if e.is_connection_lost() => error = Some(e.to_string()),
                Err(e) => log::info!("imap: quota not available: {e}"),
            }
        }
    }
    // 3. The Outbox (SMTP works even when the IMAP server does not).
    if !job.outgoing.is_empty() {
        send_outgoing(account, job, events, cancel, &mut error);
    }
    if error.is_some() {
        return error;
    }
    // 4. Marked headers: whole messages.
    for (name, uids) in &job.fetch {
        if cancelled() {
            return Some("Send/Receive was cancelled.".into());
        }
        progress(&format!("Downloading {} marked message(s)…", uids.len()));
        let t0 = Instant::now();
        match imap.fetch_messages(name, uids) {
            Ok(msgs) => {
                meter.add(raw_len(&msgs), t0.elapsed());
                send(Event::Bodies { account, name: name.clone(), msgs });
            }
            Err(e) => {
                let lost = e.is_connection_lost();
                send(Event::FolderFailed { account, name: name.clone(), error: e.to_string() });
                if lost {
                    return Some(e.to_string());
                }
            }
        }
    }
    // Server search (Search › Include Older Results): headers of the matches not held here.
    if let Some(sj) = &job.search {
        progress("Searching the server…");
        let mut left = super::older::MAX_PER_ACCOUNT;
        for SearchFolder { name, uidvalidity, up_to, known } in &sj.folders {
            if cancelled() {
                return Some("Send/Receive was cancelled.".into());
            }
            let max = super::older::MAX_PER_FOLDER.min(left);
            let t0 = Instant::now();
            match imap.search_folder(name, &sj.criterion, known, *uidvalidity, *up_to, max) {
                Ok((msgs, total)) => {
                    meter.add(raw_len(&msgs), t0.elapsed());
                    left = left.saturating_sub(msgs.len());
                    let generation = sj.generation;
                    send(Event::Found { account, name: name.clone(), generation, msgs, total });
                }
                Err(e) => {
                    let lost = e.is_connection_lost();
                    send(Event::SearchFailed { account, error: format!("{name}: {e}") });
                    if lost {
                        return Some(e.to_string());
                    }
                }
            }
        }
    }
    // 5. Folders.
    let Some(rcv) = &job.receive else { return error };
    let folder_count = todo.len();
    for (i, (name, display)) in todo.into_iter().enumerate() {
        if cancelled() {
            return Some("Send/Receive was cancelled.".into());
        }
        // "Updating Inbox (folder 1 of 12)…", then "Downloading Inbox: 50 of 5,000 messages (…)" as batches
        // arrive. The total is what the server has beyond the messages downloaded so far (the search that
        // finds new mail counts it), so it holds across the rounds a large folder takes.
        let mut detail = SyncProgress {
            folder: display,
            folder_index: i.saturating_add(1),
            folder_count,
            headers: job.headers_only,
            ..SyncProgress::default()
        };
        send(Event::counted(account, &detail));
        let mut throttle = Throttle::default();
        let mut base = 0u64;
        let mut since = rcv.states.get(&name).cloned().unwrap_or_default();
        for _ in 0..MAX_ROUNDS_PER_FOLDER {
            let t0 = Instant::now();
            let mut report = |p: FetchProgress| {
                let total = base.saturating_add(u64::try_from(p.waiting).unwrap_or(u64::MAX));
                if total == 0 {
                    return;
                }
                detail.done = base.saturating_add(u64::try_from(p.done).unwrap_or(u64::MAX)).min(total);
                detail.total = total;
                if throttle.due(p.done == 0 || p.done >= p.batch) {
                    send(Event::counted(account, &detail));
                }
            };
            let res = imap.sync_folder_with_progress(&name, &since, job.headers_only, &mut report);
            match res {
                Ok(delta) => {
                    meter.add(raw_len(&delta.new), t0.elapsed());
                    base = base.saturating_add(u64::try_from(delta.new.len()).unwrap_or(u64::MAX));
                    let more = delta.more;
                    since = delta.state.clone();
                    let fresh: Vec<u32> = delta.new.iter().map(|m| m.uid).collect();
                    send(Event::Delta { account, name: name.clone(), delta });
                    // Download Headers and Then Full Items: the bodies follow the headers.
                    if job.then_full && !fresh.is_empty() && !cancelled() {
                        let t1 = Instant::now();
                        match imap.fetch_messages(&name, &fresh) {
                            Ok(msgs) => {
                                meter.add(raw_len(&msgs), t1.elapsed());
                                send(Event::Bodies { account, name: name.clone(), msgs });
                            }
                            Err(e) => {
                                send(Event::FolderFailed {
                                    account,
                                    name: name.clone(),
                                    error: e.to_string(),
                                });
                                if e.is_connection_lost() {
                                    return Some(e.to_string());
                                }
                            }
                        }
                    }
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

fn run_pop3(
    account: AccountId,
    settings: &Pop3Settings,
    job: &mut Job,
    events: &Events,
    cancel: &AtomicBool,
    meter: &mut RateMeter,
) -> Option<String> {
    let send = |e: Event| {
        if !events.send(e) {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let progress = |t: &str| send(Event::step(account, t));
    let cancelled = || cancel.load(Ordering::SeqCst);
    let mut error: Option<String> = None;
    // The Outbox first: SMTP works even when the POP3 server does not.
    if !job.outgoing.is_empty() {
        send_outgoing(account, job, events, cancel, &mut error);
    }
    let Some(p) = job.pop3.clone() else { return error };
    if cancelled() {
        return Some("Send/Receive was cancelled.".into());
    }
    progress("Checking for new messages…");
    // Sending may have renewed the OAuth token: use the job's current settings.
    let mut settings = match &job.conn.incoming {
        Incoming::Pop3(current) => current.clone(),
        _ => settings.clone(),
    };
    let mut client = match Pop3Client::connect(&settings) {
        Ok(c) => c,
        Err(NetError::Auth(_)) if job.conn.oauth.is_some() => {
            if let Err(e) = refresh_token(account, &mut job.conn, events, true) {
                return Some(e);
            }
            if let Incoming::Pop3(s2) = &job.conn.incoming {
                settings = s2.clone();
            }
            match Pop3Client::connect(&settings) {
                Ok(c) => c,
                Err(e) => return Some(e.to_string()),
            }
        }
        Err(e) => return Some(e.to_string()),
    };
    let res: Result<(Vec<String>, Vec<String>), NetError> = (|| {
        let list = client.uidl()?;
        let sizes: BTreeMap<u32, u64> = client.list().map(|l| l.into_iter().collect()).unwrap_or_default();
        let present: Vec<String> = list.iter().map(|(_, u)| u.clone()).collect();
        let mut deleted = Vec::new();
        for (n, u) in &list {
            if p.delete.contains(u) {
                client.dele(*n)?;
                deleted.push(u.clone());
            }
        }
        let wanted: Vec<&(u32, String)> =
            list.iter().filter(|(_, u)| p.full.contains(u) && !deleted.contains(u)).collect();
        for (i, (n, u)) in wanted.iter().enumerate() {
            if cancelled() {
                return Err(NetError::Invalid("Send/Receive was cancelled.".into()));
            }
            progress(&format!("Downloading marked message {} of {}…", i.saturating_add(1), wanted.len()));
            let t0 = Instant::now();
            let raw = client.retr(*n)?;
            meter.add(raw.len(), t0.elapsed());
            send(Event::Pop3Full { account, uidl: u.clone(), raw });
        }
        if p.receive {
            let fresh: Vec<&(u32, String)> = list
                .iter()
                .filter(|(_, u)| !p.known.contains(u) && !deleted.contains(u))
                .take(MAX_POP3_PER_RUN)
                .collect();
            // "Downloading Inbox: 120 of 400 messages" (the count comes with UIDL, no extra command).
            let mut detail = SyncProgress {
                folder: "Inbox".into(),
                total: u64::try_from(fresh.len()).unwrap_or(u64::MAX),
                headers: p.headers_only,
                ..SyncProgress::default()
            };
            let mut throttle = Throttle::default();
            for (i, (n, u)) in fresh.iter().enumerate() {
                if cancelled() {
                    return Err(NetError::Invalid("Send/Receive was cancelled.".into()));
                }
                detail.done = u64::try_from(i).unwrap_or(u64::MAX);
                if throttle.due(i == 0) {
                    send(Event::counted(account, &detail));
                }
                let t0 = Instant::now();
                let raw = if p.headers_only { client.top(*n, 0)? } else { client.retr(*n)? };
                meter.add(raw.len(), t0.elapsed());
                send(Event::Pop3New {
                    account,
                    uidl: u.clone(),
                    raw,
                    size: sizes.get(n).copied(),
                    headers_only: p.headers_only,
                });
            }
            if !fresh.is_empty() {
                detail.done = detail.total;
                send(Event::counted(account, &detail));
            }
        }
        Ok((present, deleted))
    })();
    match res {
        Ok((present, deleted)) => match client.quit() {
            Ok(()) => {
                send(Event::Pop3Server { account, present, deleted });
                error
            }
            // Deletions only happen at a clean QUIT; nothing was removed.
            Err(e) => Some(e.to_string()),
        },
        Err(e) => Some(e.to_string()),
    }
}

fn send_outgoing(
    account: AccountId,
    job: &mut Job,
    events: &Events,
    cancel: &AtomicBool,
    error: &mut Option<String>,
) {
    let send = |e: Event| {
        if !events.send(e) {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let Some(set) = job.conn.smtp.clone() else {
        for m in &job.outgoing {
            send(Event::Sent {
                account,
                item: m.item,
                result: Err("The account has no outgoing mail server.".into()),
            });
        }
        return;
    };
    send(Event::step(account, "Sending…"));
    let mut connected = SmtpClient::connect(&set);
    // An expired OAuth token shows as a sign-in failure: renew it once and sign in again.
    if job.conn.oauth.is_some() && matches!(connected, Err(NetError::Auth(_))) {
        match refresh_token(account, &mut job.conn, events, true) {
            Ok(_) => {
                if let Some(set) = &job.conn.smtp {
                    connected = SmtpClient::connect(set);
                }
            }
            Err(e) => connected = Err(NetError::Auth(e)),
        }
    }
    match connected {
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
                send(Event::step(account, format!("Sending message {} of {n}…", i.saturating_add(1))));
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

fn run_test(account: AccountId, conn: Result<Conn, String>) -> TestReport {
    let mut r = TestReport { account: account.0, ..Default::default() };
    let mut conn = match conn {
        Ok(c) => c,
        Err(e) => {
            r.incoming = e;
            return r;
        }
    };
    // Tokens refreshed here are not stored (the test has no session); the next run refreshes again.
    let (tx, _rx) = mpsc::channel();
    let quiet = Events { tx, serial: NO_WORKER };
    if let Err(e) = refresh_token(account, &mut conn, &quiet, false) {
        r.incoming = e;
        return r;
    }
    match &conn.incoming {
        Incoming::Jmap(j) => {
            match j.connect() {
                Ok(_) => {
                    r.incoming_ok = true;
                    r.incoming = format!("Signed in to {} (JMAP).", j.url());
                    r.outgoing_ok = Some(true);
                    r.outgoing = "Sending uses the same JMAP server.".into();
                }
                Err(e) => r.incoming = e.to_string(),
            }
            return r;
        }
        Incoming::Imap(set) => {
            let mut imap = ImapAccount::new(set.clone());
            match imap.connect() {
                Ok(_) => {
                    r.incoming_ok = true;
                    r.incoming = format!("Logged in to the incoming mail server {} (IMAP).", set.host);
                }
                Err(e) => r.incoming = e.to_string(),
            }
            imap.disconnect();
        }
        Incoming::Pop3(set) => match Pop3Client::connect(set) {
            Ok(c) => {
                let _ = c.quit();
                r.incoming_ok = true;
                r.incoming = format!("Logged in to the incoming mail server {} (POP3).", set.host);
            }
            Err(e) => r.incoming = e.to_string(),
        },
    }
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
    crate::accounts::load_prefs(s);
    let conn = conn_settings(s, &a);
    if wait {
        let report = run_test(id, conn);
        s.sync.tests.insert(id, report.clone());
        return Ok(Some(report));
    }
    let events = s.sync.rt.loose_events();
    std::thread::Builder::new()
        .name(format!("mailcraft-test-{}", id.0))
        .spawn(move || {
            let report = catch_unwind(AssertUnwindSafe(|| run_test(id, conn))).unwrap_or_else(|_| {
                TestReport { account: id.0, incoming: "internal error".into(), ..Default::default() }
            });
            events.send(Event::Test { account: id, report });
        })
        .map_err(|e| CmdError::Failed(format!("cannot start the test: {e}")))?;
    Ok(None)
}

// ---------------------------------------------------------------------------------------------------
// OAuth sign-in (background thread)
// ---------------------------------------------------------------------------------------------------

/// How long the browser sign-in waits for the redirect.
const BROWSER_SIGN_IN_WAIT: Duration = Duration::from_secs(10 * 60);

/// Starts an OAuth sign-in for `account` with the registration in its options. The device flow's code and
/// the browser flow's page arrive through [`Session::poll_sync`] (see `oauth_status`); the browser flow's
/// address is known at once and returned.
pub(super) fn start_oauth(
    s: &mut Session,
    id: AccountId,
    address: &str,
    device: bool,
) -> Result<Value, String> {
    let provider = oauth_provider(s, id).ok_or("no OAuth registration for this account")?;
    let http = mailcraft_net::http::HttpClient::new(oauth_http_options(s));
    if let Some(old) = s.sync.rt.oauth.remove(&id) {
        old.store(true, Ordering::SeqCst);
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let events = s.sync.rt.loose_events();
    let mut flow = OAuthFlow {
        provider: provider.name.clone(),
        flow: if device { "device".into() } else { "browser".into() },
        status: "starting".into(),
        ..Default::default()
    };
    let c2 = Arc::clone(&cancel);
    let spawn = if device {
        std::thread::Builder::new().name(format!("mailcraft-oauth-{}", id.0)).spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(|| device_flow(id, &http, &provider, &events, &c2)))
                .unwrap_or_else(|_| Err("internal error during sign-in".into()));
            events.send(Event::OAuthDone { account: id, result: r });
        })
    } else {
        let redirect = mailcraft_net::oauth::LoopbackRedirect::bind().map_err(|e| e.to_string())?;
        let pkce = mailcraft_net::oauth::Pkce::generate().map_err(|e| e.to_string())?;
        let state = mailcraft_net::oauth::random_state().map_err(|e| e.to_string())?;
        let url = mailcraft_net::oauth::authorization_url(
            &provider,
            &redirect.redirect_uri(),
            &pkce,
            &state,
            Some(address),
        )
        .map_err(|e| e.to_string())?;
        flow.auth_url = Some(url);
        flow.status = "waiting".into();
        std::thread::Builder::new().name(format!("mailcraft-oauth-{}", id.0)).spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(|| {
                let deadline = mailcraft_net::transport::deadline_after(BROWSER_SIGN_IN_WAIT);
                let target = loop {
                    if c2.load(Ordering::SeqCst) {
                        return Err("Sign-in was cancelled.".to_owned());
                    }
                    if Instant::now() >= deadline {
                        return Err("The browser sign-in timed out.".to_owned());
                    }
                    match redirect.wait_for(Duration::from_millis(500), &state) {
                        Ok(t) => break t,
                        Err(NetError::Timeout(_)) => {}
                        Err(e) => return Err(e.to_string()),
                    }
                };
                let code =
                    mailcraft_net::oauth::parse_redirect(&target, &state).map_err(|e| e.to_string())?;
                mailcraft_net::oauth::exchange_code(&http, &provider, &code, &redirect.redirect_uri(), &pkce)
                    .map_err(|e| e.to_string())
            }))
            .unwrap_or_else(|_| Err("internal error during sign-in".into()));
            events.send(Event::OAuthDone { account: id, result: r });
        })
    };
    spawn.map_err(|e| format!("cannot start the sign-in: {e}"))?;
    s.sync.rt.oauth.insert(id, cancel);
    let v = flow.to_json(id);
    s.sync.oauth.insert(id, flow);
    Ok(v)
}

fn device_flow(
    id: AccountId,
    http: &mailcraft_net::http::HttpClient,
    p: &OAuthProvider,
    events: &Events,
    cancel: &AtomicBool,
) -> Result<TokenSet, String> {
    let da = mailcraft_net::oauth::start_device_flow(http, p).map_err(|e| e.to_string())?;
    events.send(Event::OAuthPrompt {
        account: id,
        prompt: json!({
            "userCode": da.user_code,
            "verificationUri": da.verification_uri,
            "verificationUriComplete": da.verification_uri_complete,
            "expiresIn": da.expires_in,
        }),
    });
    mailcraft_net::oauth::wait_for_device_token(http, p, &da, |d| {
        let end = mailcraft_net::transport::deadline_after(d);
        while Instant::now() < end {
            if cancel.load(Ordering::SeqCst) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        !cancel.load(Ordering::SeqCst)
    })
    .map_err(|e| e.to_string())
}

pub(super) fn cancel_oauth(s: &mut Session, id: AccountId) -> bool {
    match s.sync.rt.oauth.remove(&id) {
        Some(c) => {
            c.store(true, Ordering::SeqCst);
            if let Some(f) = s.sync.oauth.get_mut(&id)
                && (f.status == "waiting" || f.status == "starting")
            {
                f.status = "cancelled".into();
            }
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------------------------------
// Applying results (session thread)
// ---------------------------------------------------------------------------------------------------

/// Queues synthetic results for IMAP account `account` (see [`super::SyntheticImap`]) on the channel its
/// worker would use; `poll_events` applies them. False when the account is not an IMAP account.
pub(super) fn queue_synthetic(s: &Session, account: AccountId, results: super::SyntheticImap) -> bool {
    if !s.mailbox.account(account).is_some_and(|a| a.kind == AccountKind::Imap) {
        return false;
    }
    let event = match results {
        super::SyntheticImap::Delta { folder, delta } => Event::Delta { account, name: folder, delta },
        super::SyntheticImap::Bodies { folder, msgs } => Event::Bodies { account, name: folder, msgs },
    };
    s.sync.rt.loose_events().send(event)
}

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
    crate::accounts::load_prefs(s);
    // Progress, tests and the end of a run leave the mailbox alone: no snapshot needed.
    let touches = events.iter().any(|e| {
        matches!(
            e,
            Event::OpDone { .. }
                | Event::Sent { .. }
                | Event::Folders { .. }
                | Event::Delta { .. }
                | Event::Bodies { .. }
                | Event::Found { .. }
                | Event::Pop3New { .. }
                | Event::Pop3Full { .. }
                | Event::Jmap { .. }
                | Event::Dav { .. }
                | Event::SharedRoots { .. }
                | Event::OAuthDone { .. }
        )
    });
    let prefs_before = s.sync.prefs.data.clone();
    let recording = touches.then(|| s.begin_changes());
    let res = catch_unwind(AssertUnwindSafe(|| apply_all(s, events)));
    if s.sync.prefs.data != prefs_before {
        crate::accounts::save_prefs(s);
    }
    let Some(recording) = recording else {
        if res.is_err() {
            log::error!("applying send/receive status panicked");
        }
        return false;
    };
    if res.is_err() {
        log::error!("applying send/receive results panicked; the mailbox was restored");
        s.rollback_changes(recording);
        s.mailbox_changed();
        return false;
    }
    let ops = s.end_changes(recording).ops(&s.mailbox);
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
            Event::Progress { account, text, detail } => {
                let run = s.sync.runs.entry(account).or_default();
                run.progress = text;
                run.detail = detail;
            }
            Event::OpDone { account, op, result } => op_done(s, account, op, result),
            Event::Folders { account, folders } => apply_folders(s, account, folders),
            Event::Delta { account, name, delta } => apply_delta(s, account, &name, delta, &mut new_inbox),
            Event::Bodies { account, name, msgs } => apply_bodies(s, account, &name, msgs),
            Event::Found { account, name, generation, msgs, total } => {
                apply_found(s, account, &name, generation, msgs, total);
            }
            Event::SearchFailed { account, error } => {
                log::warn!("server search of account {}: {error}", account.0);
                s.sync.older.error = Some(error);
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
            Event::Push { account, mode, error } => {
                let run = s.sync.runs.entry(account).or_default();
                run.push = mode;
                if mode == PushMode::Off {
                    run.push_on = false;
                    // A sign-in problem stops push until a send/receive works again (no retry storm).
                    if let Some(e) = &error {
                        run.error = Some(e.clone());
                    }
                }
                run.push_error = error;
            }
            Event::Quota { account, quota } => {
                let checked = s.now().unix;
                let q = quota.map(|q| super::MailboxQuota {
                    used: q.used,
                    limit: q.limit,
                    messages: q.messages,
                    checked,
                });
                super::set_quota(s, account, q);
            }
            Event::LinkSpeed { account, bytes_per_sec } => {
                let now = s.now().unix;
                s.sync.runs.entry(account).or_default().link_speed = Some((bytes_per_sec, now));
            }
            Event::PushChanged { account } => {
                if !s.view.work_offline {
                    let inbox = s.mailbox.role_folder(account, FolderRole::Inbox);
                    let plan =
                        Plan { send: false, receive: true, folders: inbox.map(|f| vec![f]), user: false };
                    if let Err(e) = dispatch(s, account, plan) {
                        s.sync.runs.entry(account).or_default().error = Some(e);
                    }
                }
            }
            Event::Pop3New { account, uidl, raw, size, headers_only } => {
                apply_pop3_new(s, account, &uidl, &raw, size, headers_only, &mut new_inbox)
            }
            Event::Pop3Full { account, uidl, raw } => apply_pop3_full(s, account, &uidl, &raw),
            Event::Pop3Server { account, present, deleted } => {
                apply_pop3_server(s, account, &present, &deleted)
            }
            Event::Jmap { account, ev } => super::jmap::apply(s, account, ev, &mut new_inbox),
            Event::Dav { account, ev } => super::pim::apply(s, account, *ev),
            Event::SharedRoots { account, roots } => super::shared::apply_roots(s, account, roots),
            Event::Tokens { account, tokens } => store_tokens(s, account, &tokens),
            Event::OAuthPrompt { account, prompt } => {
                if let Some(f) = s.sync.oauth.get_mut(&account) {
                    f.status = "waiting".into();
                    let st = |k: &str| prompt.get(k).and_then(Value::as_str).map(str::to_owned);
                    f.user_code = st("userCode");
                    f.verification_uri = st("verificationUri");
                    f.verification_uri_complete = st("verificationUriComplete");
                }
            }
            Event::OAuthDone { account, result } => oauth_done(s, account, result),
            e @ Event::Sent { .. } => sent.push(e),
            e @ Event::Done { .. } => done.push(e),
        }
    }
    // Changes below are local actions (sending files the message, rules move it): they reach the server
    // through the queue like a command's changes.
    if !sent.is_empty() || !new_inbox.is_empty() {
        let mid = s.begin_changes();
        for ev in sent {
            if let Event::Sent { account, item, result } = ev {
                message_sent(s, account, item, result);
            }
        }
        if !new_inbox.is_empty() {
            let ids: Vec<ItemId> = new_inbox.into_iter().filter(|i| s.mailbox.mail.contains(*i)).collect();
            // Junk Email filter first, then Focused Inbox overrides, the rules for received messages
            // (client-side, like every rule here), automatic meeting responses, tracking and receipts.
            super::arrival::on_new_mail(s, &ids);
        }
        let mut local = s.end_changes(mid);
        queue_local_changes(s, &mut local);
    }
    let inflight = s.sync.inflight_ops();
    prune_stale_ops(&mut s.mailbox, &inflight);
    for ev in done {
        if let Event::Done { account, error } = ev {
            run_done(s, account, error);
        }
    }
}

/// Stores refreshed OAuth tokens (access token as the account's secret, refresh token and expiry).
fn store_tokens(s: &mut Session, account: AccountId, t: &TokenSet) {
    if let Err(e) = s.secrets.set(&secret_key(account, false), &t.access_token) {
        log::error!("cannot store the renewed sign-in of account {}: {e}", account.0);
        s.notify(Notice::Error(format!("The renewed sign-in could not be saved: {e}")));
        return;
    }
    if let Some(r) = &t.refresh_token
        && s.secrets.get(&refresh_key(account)).as_deref() != Some(r.as_str())
        && let Err(e) = s.secrets.set(&refresh_key(account), r)
    {
        log::error!("cannot store the refresh token of account {}: {e}", account.0);
    }
    let exp = t.expires_at;
    if let Some(o) = crate::accounts::prefs(s).accounts.get_mut(&account.0)
        && let Some(b) = o.oauth.as_mut()
    {
        b.expires_at = exp;
    }
}

fn oauth_done(s: &mut Session, account: AccountId, result: Result<TokenSet, String>) {
    s.sync.rt.oauth.remove(&account);
    let name = s.mailbox.account(account).map(|a| a.display_name.clone()).unwrap_or_default();
    let cancelled = s.sync.oauth.get(&account).is_some_and(|f| f.status == "cancelled");
    match result {
        Ok(t) => {
            store_tokens(s, account, &t);
            // The token signs in to both servers: an old outgoing password must not shadow it.
            if s.secrets.get(&secret_key(account, true)).is_some() {
                let _ = s.secrets.remove(&secret_key(account, true));
            }
            if let Some(a) = account_mut(s, account) {
                a.credential = CredentialKind::OAuthToken;
            }
            if let Some(f) = s.sync.oauth.get_mut(&account) {
                f.status = "done".into();
                f.error = None;
            }
            if let Some(r) = s.sync.runs.get_mut(&account) {
                r.blocked_until = 0;
                r.error = None;
            }
            s.notify(Notice::Info(format!("{name}: signed in.")));
        }
        Err(e) => {
            if let Some(f) = s.sync.oauth.get_mut(&account) {
                f.status = if cancelled { "cancelled".into() } else { "failed".into() };
                f.error = Some(e.clone());
            }
            if !cancelled {
                s.notify(Notice::Error(format!("{name}: sign-in failed. {e}")));
            }
        }
    }
}

/// Replaces a header-only item's content with the whole message, keeping its id, folder and state. The
/// source goes to the body store first; when that fails the item stays as it was (false).
fn fill_item(s: &mut Session, id: ItemId, raw: &[u8]) -> bool {
    let now = s.now();
    let Some(old) = s.mailbox.mail.get(id).cloned() else { return false };
    let mut full = mailcraft_store::import::item_from_raw(raw, old.folder, now);
    if let Err(e) = s.store_source(&mut full, raw) {
        log::error!("could not store the downloaded message {}: {e}", id.0);
        return false;
    }
    full.keep_local_state(&old);
    s.mailbox.mail.insert(id, full);
    crate::offline::arrived(s, id);
    let p = crate::accounts::prefs(s);
    p.headers_only.remove(&id.0);
    p.marked.remove(&id.0);
    true
}

/// A header-only download: the item shows its header; body, source and attachments come later.
fn strip_to_header(s: &mut Session, item: &mut MailItem, size: Option<u64>) {
    item.blob = None;
    item.body_key = None;
    item.preview.clear();
    if let Some(n) = size {
        item.size = n;
    }
    let _ = s;
}

/// Stores the downloaded `raw` as `item`'s source; when the body store refuses it (a full disk), the item
/// is kept as a header-only download instead (the server still has the message), and false comes back.
fn store_downloaded(s: &mut Session, item: &mut MailItem, raw: &[u8], size: Option<u64>) -> bool {
    match s.store_source(item, raw) {
        Ok(()) => true,
        Err(e) => {
            log::error!("could not store a downloaded message ({e}); keeping its header only");
            strip_to_header(s, item, size);
            false
        }
    }
}

/// The IMAP part of a server search for account `a`: the folders of the request that are on the server and
/// have been synced, with what is known of each.
///
/// The next sync of a folder downloads only UIDs above the highest one known (held here or referred to by a
/// queued change), so a match above that must not be filed here: the sync would then skip every unmatched
/// message below it. Such matches are new mail the next sync brings anyway. A folder not synced yet has no
/// such bound and is left out.
fn imap_search(s: &Session, a: &Account, req: &super::older::Request) -> Option<ImapSearch> {
    let mut uids: BTreeMap<FolderId, Vec<u32>> = BTreeMap::new();
    for m in s.mailbox.mail.items() {
        if req.folders.contains(&m.folder)
            && let Some(u) = uid_of(m)
        {
            uids.entry(m.folder).or_default().push(u);
        }
    }
    let mut queued: BTreeMap<&str, u32> = BTreeMap::new();
    for p in &a.sync.pending {
        if let RemoteOp::Flags { folder, uid, .. }
        | RemoteOp::Move { folder, uid, .. }
        | RemoteOp::Expunge { folder, uid } = &p.op
        {
            let top = queued.entry(folder.as_str()).or_default();
            *top = (*top).max(*uid);
        }
    }
    let mut folders: Vec<SearchFolder> = Vec::new();
    for f in s.mailbox.folders.values() {
        if f.account != a.id || f.kind != FolderKind::Mail || !req.folders.contains(&f.id) {
            continue;
        }
        let Some(name) = &f.remote_name else { continue };
        let Some(st) = a.sync.folders.get(&f.id).filter(|st| st.uidvalidity != 0) else { continue };
        let known = uids.remove(&f.id).unwrap_or_default();
        let mut up_to =
            known.iter().copied().max().unwrap_or(0).max(queued.get(name.as_str()).copied().unwrap_or(0));
        if st.uidnext > 0 {
            up_to = up_to.min(st.uidnext.saturating_sub(1));
        }
        if up_to == 0 {
            continue;
        }
        folders.push(SearchFolder { name: name.clone(), uidvalidity: st.uidvalidity, up_to, known });
    }
    // Inbox first, then in name order.
    folders.sort_by_key(|f| (!f.name.eq_ignore_ascii_case("INBOX"), f.name.clone()));
    (!folders.is_empty()).then(|| ImapSearch {
        generation: req.generation,
        criterion: req.criterion.clone(),
        folders,
    })
}

/// Headers a server search found in folder `name`: filed as header-only messages (their bodies download
/// when opened) and added to the search's results. Old mail: no rules or new-mail alerts.
fn apply_found(
    s: &mut Session,
    account: AccountId,
    name: &str,
    generation: u64,
    msgs: Vec<NewMessage>,
    total: usize,
) {
    let Some(fid) = find_by_remote(&s.mailbox, account, name) else { return };
    let now = s.now();
    let mut by_uid: BTreeMap<u32, ItemId> =
        s.mailbox.mail.in_folder(fid).filter_map(|m| Some((uid_of(m)?, m.id))).collect();
    let mut ids = Vec::new();
    for nm in msgs {
        if let Some(id) = by_uid.get(&nm.uid) {
            ids.push(*id);
            continue;
        }
        let mut item = mailcraft_store::import::item_from_raw(&nm.raw, fid, now);
        item.remote_id = Some(nm.uid.to_string());
        apply_server_flags(&mut item, &nm.flags, now);
        strip_to_header(s, &mut item, nm.size);
        let id = s.mailbox.add_mail(item);
        crate::accounts::prefs(s).headers_only.insert(id.0);
        s.sync.track.created.insert(id);
        by_uid.insert(nm.uid, id);
        ids.push(id);
    }
    super::older::found(s, generation, &ids, total);
}

fn apply_bodies(s: &mut Session, account: AccountId, name: &str, msgs: Vec<NewMessage>) {
    let Some(fid) = find_by_remote(&s.mailbox, account, name) else { return };
    let by_uid: BTreeMap<u32, ItemId> =
        s.mailbox.mail.in_folder(fid).filter_map(|m| Some((uid_of(m)?, m.id))).collect();
    for m in msgs {
        if let Some(id) = by_uid.get(&m.uid) {
            fill_item(s, *id, &m.raw);
        }
    }
}

fn find_pop3(s: &Session, account: AccountId, uidl: &str) -> Option<ItemId> {
    first_item(&s.mailbox, &account_scope(&s.mailbox, account), |m| pop3_uidl(m) == Some(uidl))
}

fn apply_pop3_new(
    s: &mut Session,
    account: AccountId,
    uidl: &str,
    raw: &[u8],
    size: Option<u64>,
    headers_only: bool,
    new_inbox: &mut Vec<ItemId>,
) {
    let now = s.now();
    let p = crate::accounts::prefs(s);
    p.pop3.entry(account.0).or_default().seen.entry(uidl.to_owned()).or_insert(now.unix);
    if find_pop3(s, account, uidl).is_some() {
        return;
    }
    let Some(inbox) = s.mailbox.role_folder(account, FolderRole::Inbox) else { return };
    let mut item = mailcraft_store::import::item_from_raw(raw, inbox, now);
    item.remote_id = Some(format!("{POP3_PREFIX}{uidl}"));
    if headers_only {
        strip_to_header(s, &mut item, size);
    } else if let Err(e) = s.store_source(&mut item, raw) {
        // A POP3 server may delete what was downloaded: the message is not taken in, so the next run
        // downloads it again.
        log::error!("could not store a downloaded message ({e}); it is downloaded again next time");
        if let Some(p) = crate::accounts::prefs(s).pop3.get_mut(&account.0) {
            p.seen.remove(uidl);
        }
        return;
    }
    let id = s.mailbox.add_mail(item);
    if headers_only {
        crate::accounts::prefs(s).headers_only.insert(id.0);
    }
    s.sync.track.created.insert(id);
    new_inbox.push(id);
}

fn apply_pop3_full(s: &mut Session, account: AccountId, uidl: &str, raw: &[u8]) {
    let Some(id) = find_pop3(s, account, uidl) else { return };
    let copy = crate::accounts::prefs(s).marked.get(&id.0).copied().unwrap_or(true);
    if fill_item(s, id, raw) && !copy {
        // Mark to Download (not a copy): the server copy goes on the next run.
        crate::accounts::prefs(s).pop3.entry(account.0).or_default().delete.insert(uidl.to_owned());
    }
}

fn apply_pop3_server(s: &mut Session, account: AccountId, present: &[String], deleted: &[String]) {
    let p = crate::accounts::prefs(s);
    let st = p.pop3.entry(account.0).or_default();
    for d in deleted {
        st.seen.remove(d);
        st.delete.remove(d);
    }
    // Unique ids gone from the server (another client removed them) need no bookkeeping any more.
    let on_server: BTreeSet<&String> = present.iter().collect();
    st.seen.retain(|u, _| on_server.contains(u));
    st.delete.retain(|u| on_server.contains(u));
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
                || !matches!(&p.op, RemoteOp::Append { item, .. } if !mail.contains(*item))
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
    let here = s.mailbox.mail.get(item).and_then(|m| synced_folder(&s.mailbox, m.folder));
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
    let Some(m) = s.mailbox.mail.get(item) else { return };
    if Some(m.folder) != outbox {
        return;
    }
    let subject = if m.subject.trim().is_empty() { "(no subject)".to_owned() } else { m.subject.clone() };
    match result {
        Ok(()) => {
            // "Automatically add people I email to the Safe Senders List" (Junk Email Options).
            crate::junk::on_sent(s, &[item]);
            // Save Sent Item To: Do Not Save, or another folder.
            if crate::cmd::compose::file_sent_copy(s, item) {
                return;
            }
            // JMAP servers file the sent copy themselves (EmailSubmission onSuccessUpdateEmail).
            let saves = s
                .mailbox
                .account(account)
                .is_some_and(|a| a.server_saves_sent || a.kind == AccountKind::Jmap);
            let sent = s.mailbox.role_folder(account, FolderRole::SentItems);
            match sent {
                Some(sent) if !saves || s.mailbox.folder(sent).is_some_and(|f| f.remote_name.is_none()) => {
                    // The copy uploaded to the server's Sent folder must not carry the submission-only
                    // `X-MailCraft-Submit-*` fields.
                    let stripped = s
                        .mailbox
                        .mail
                        .get(item)
                        .and_then(|m| crate::cmd::compose::stored_raw(s, m))
                        .and_then(|r| {
                            let out = crate::cmd::compose::strip_submit_headers(&r);
                            (out.len() != r.len()).then_some(out)
                        });
                    if let Some(mut m) = s.mailbox.mail.get(item).cloned() {
                        m.folder = sent;
                        m.read = true;
                        m.remote_id = None;
                        if let Some(r) = stripped {
                            match s.store_source(&mut m, &r) {
                                Ok(()) => m.size = u64::try_from(r.len()).unwrap_or(m.size),
                                Err(e) => log::warn!("the sent copy keeps its submission headers ({e})"),
                            }
                        }
                        s.mailbox.mail.insert(item, m);
                    }
                }
                // The server files its own copy (downloaded by the next update of Sent Items).
                _ => {
                    s.mailbox.mail.remove(item);
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
    super::older::run_ended(s, account, error.as_deref());
    let now = s.now().unix;
    let run = s.sync.runs.entry(account).or_default();
    run.busy = None;
    run.progress.clear();
    run.detail = None;
    match error {
        Some(e) => {
            run.error = Some(e);
            run.blocked_until = now.saturating_add(BLOCKED_RETRY_SECS.min(60));
        }
        None => {
            let ok = run.error.is_none();
            if ok {
                run.last_sync = Some(now);
            }
            run.blocked_until = 0;
            if ok {
                // The account is reachable: its offline window may take old bodies off (`offline.rs`).
                crate::offline::note_run_done(s, account);
            }
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
            RemoteOp::Move { folder, dest, item, .. } if folder == name => match mail.get(*item) {
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
                    RemoteOp::CreateFolder { name, .. } => Some(name.clone()),
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
            s.mailbox.mail.update_where(|m| m.folder == fid, |m| m.remote_id = None);
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
    for m in s.mailbox.mail.in_folder(fid) {
        if let Some(u) = uid_of(m) {
            by_uid.insert(u, m.id);
        }
    }
    if delta.reset {
        for (_, id) in std::mem::take(&mut by_uid) {
            s.mailbox.mail.remove(id);
            s.sync.track.removed.insert(id);
        }
        forget_old_uids(s, account, name);
    }
    for uid in &delta.vanished {
        if let Some(id) = by_uid.remove(uid) {
            s.mailbox.mail.remove(id);
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
            && let Some(m) = s.mailbox.mail.get_mut(*id)
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
        let adopt = item.message_id.as_deref().and_then(|mid| local_copy(&s.mailbox, fid, mid));
        if let Some(local) = adopt {
            set_remote_id(&mut s.mailbox, local, Some(nm.uid));
            if let Some(a) = account_mut(s, account) {
                a.sync.pending.retain(|p| !matches!(&p.op, RemoteOp::Append { item, .. } if *item == local));
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
        let headers_only = if nm.headers_only {
            strip_to_header(s, &mut item, nm.size);
            true
        } else {
            !store_downloaded(s, &mut item, &nm.raw, nm.size)
        };
        let id = s.mailbox.add_mail(item);
        if headers_only {
            crate::accounts::prefs(s).headers_only.insert(id.0);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rules::tests::session;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// A local plain-HTTP OAuth server: `respond(path, form body)` gives (status, JSON). Serves until dropped
    /// by the test process (thread detached; at most `n` requests).
    fn oauth_server(n: usize, respond: impl Fn(&str, &str) -> (u16, String) + Send + 'static) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((sock, _)) = l.accept() else { return };
                let mut r = BufReader::new(sock.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                let (status, json) = respond(&path, &String::from_utf8_lossy(&body));
                let mut w = sock;
                let _ = w.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        json.len()
                    )
                    .as_bytes(),
                );
            }
        });
        base
    }

    fn imap_account(s: &mut Session) -> AccountId {
        let r = s
            .run(
                "accounts.add",
                &json!({"kind": "imap", "address": "me@example.net", "incoming": {"host": "imap.example.net"},
                    "outgoing": {"host": "smtp.example.net"}, "password": "pw", "outgoingPassword": "old"}),
            )
            .unwrap();
        AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
    }

    fn bind(s: &mut Session, id: AccountId, base: &str) {
        let b = crate::accounts::OAuthBinding {
            provider: "custom".into(),
            name: "Example".into(),
            authorization_endpoint: format!("{base}/authorize"),
            token_endpoint: format!("{base}/token"),
            device_authorization_endpoint: Some(format!("{base}/device")),
            client_id: "client-1".into(),
            scopes: vec!["mail".into()],
            ..Default::default()
        };
        crate::accounts::update_options(s, id, |o| o.oauth = Some(b));
        s.sync.oauth_http = Some(ConnectOptions { allow_plaintext_auth: true, ..ConnectOptions::default() });
    }

    fn wait_status(s: &mut Session, id: AccountId, want: &str) -> Value {
        let end = mailcraft_net::transport::deadline_after(Duration::from_secs(20));
        loop {
            s.poll_sync();
            let v = crate::sync::oauth_status(s, id);
            if v["status"] == want {
                return v;
            }
            assert!(Instant::now() < end, "OAuth status {v}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn device_sign_in_stores_tokens_and_refresh_renews_them() {
        let (mut s, _, _) = session();
        let id = imap_account(&mut s);
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p2 = Arc::clone(&polls);
        let base = oauth_server(8, move |path, body| match path {
            "/device" => {
                assert!(body.contains("client_id=client-1") && body.contains("scope=mail"), "{body}");
                (200, r#"{"device_code":"dev-1","user_code":"ABCD-EFGH","verification_uri":"https://example.com/device","expires_in":600,"interval":1}"#.into())
            }
            "/token" if body.contains("device_code=dev-1") => {
                if p2.fetch_add(1, Ordering::SeqCst) == 0 {
                    (400, r#"{"error":"authorization_pending"}"#.into())
                } else {
                    (200, r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":3600,"token_type":"Bearer"}"#.into())
                }
            }
            "/token" if body.contains("grant_type=refresh_token") && body.contains("refresh_token=rt-1") => {
                (200, r#"{"access_token":"at-2","expires_in":3600}"#.into())
            }
            _ => (400, r#"{"error":"invalid_request"}"#.into()),
        });
        bind(&mut s, id, &base);
        let v = start_oauth(&mut s, id, "me@example.net", true).unwrap();
        assert_eq!(v["flow"], "device");
        let v = wait_status(&mut s, id, "waiting");
        if v["userCode"].is_null() {
            // The prompt may arrive with the result in one poll; either way the code is shown before "done".
        } else {
            assert_eq!(v["userCode"], "ABCD-EFGH");
        }
        wait_status(&mut s, id, "done");
        assert_eq!(s.secrets.get(&secret_key(id, false)).as_deref(), Some("at-1"));
        assert_eq!(s.secrets.get(&refresh_key(id)).as_deref(), Some("rt-1"));
        assert!(s.secrets.get(&secret_key(id, true)).is_none(), "the old outgoing password is gone");
        assert_eq!(s.mailbox.account(id).unwrap().credential, CredentialKind::OAuthToken);
        let exp = account_options(&s, id).oauth.unwrap().expires_at.unwrap();
        assert!(exp > mailcraft_net::oauth::now_unix());

        // The worker refreshes a token about to expire, and the session stores the new one.
        crate::accounts::update_options(&mut s, id, |o| {
            if let Some(b) = o.oauth.as_mut() {
                b.expires_at = Some(1);
            }
        });
        let a = s.mailbox.account(id).unwrap().clone();
        let mut conn = conn_settings(&s, &a).unwrap();
        assert!(conn.oauth.is_some());
        let events = s.sync.rt.loose_events();
        assert!(refresh_token(id, &mut conn, &events, false).unwrap());
        match &conn.incoming {
            Incoming::Imap(i) => assert!(
                matches!(&i.credentials, Credentials::OAuth2 { access_token, .. } if access_token == "at-2")
            ),
            _ => panic!("imap"),
        }
        assert!(
            matches!(conn.smtp.as_ref().and_then(|x| x.credentials.as_ref()), Some(Credentials::OAuth2 { access_token, .. }) if access_token == "at-2")
        );
        // Not due: nothing to do.
        assert!(!refresh_token(id, &mut conn, &events, false).unwrap());
        s.poll_sync();
        assert_eq!(s.secrets.get(&secret_key(id, false)).as_deref(), Some("at-2"));
        assert_eq!(s.secrets.get(&refresh_key(id)).as_deref(), Some("rt-1"), "kept when not rotated");
    }

    #[test]
    fn browser_sign_in_with_pkce_and_cancel() {
        let (mut s, _, _) = session();
        let id = imap_account(&mut s);
        let base = oauth_server(2, |path, body| match path {
            "/token" if body.contains("code=the-code") && body.contains("code_verifier=") => {
                (200, r#"{"access_token":"at-b","refresh_token":"rt-b","expires_in":60}"#.into())
            }
            _ => (400, r#"{"error":"invalid_grant"}"#.into()),
        });
        bind(&mut s, id, &base);
        let v = start_oauth(&mut s, id, "me@example.net", false).unwrap();
        let url = v["authUrl"].as_str().unwrap().to_owned();
        assert!(url.starts_with(&format!("{base}/authorize?")), "{url}");
        let q = mailcraft_net::oauth::query_params(&url);
        let get = |k: &str| q.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone()).unwrap();
        assert_eq!(get("code_challenge_method"), "S256");
        assert_eq!(get("login_hint"), "me@example.net");
        // The "browser" comes back to the loopback redirect.
        let redirect = get("redirect_uri");
        let state = get("state");
        let target = redirect.trim_start_matches("http://").trim_end_matches('/').to_owned();
        // Another local process reaching the port first with a forged error (no state) is turned away and
        // does not end the sign-in.
        let mut forged = std::net::TcpStream::connect(&target).unwrap();
        forged.write_all(b"GET /?error=access_denied HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut page = String::new();
        let _ = forged.read_to_string(&mut page);
        assert!(page.contains("400"), "{page}");
        s.poll_sync();
        assert_eq!(crate::sync::oauth_status(&s, id)["status"], "waiting");
        let mut sock = std::net::TcpStream::connect(&target).unwrap();
        sock.write_all(format!("GET /?code=the-code&state={state} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .unwrap();
        let mut page = String::new();
        let _ = sock.read_to_string(&mut page);
        assert!(page.contains("200 OK"));
        wait_status(&mut s, id, "done");
        assert_eq!(s.secrets.get(&secret_key(id, false)).as_deref(), Some("at-b"));

        // A second sign-in can be cancelled.
        start_oauth(&mut s, id, "me@example.net", false).unwrap();
        assert!(cancel_oauth(&mut s, id));
        assert_eq!(crate::sync::oauth_status(&s, id)["status"], "cancelled");
        assert!(!cancel_oauth(&mut s, id));
    }

    fn pop3_new(account: AccountId, uidl: &str, from: &str, subject: &str) -> Event {
        let raw = format!(
            "From: {from}\r\nTo: me@example.net\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0000\r\n\
             Message-ID: <{uidl}@example.org>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHi.\r\n"
        );
        Event::Pop3New { account, uidl: uidl.into(), raw: raw.into_bytes(), size: None, headers_only: false }
    }

    #[test]
    fn a_downloaded_body_keeps_the_pin_votes_and_other_local_state() {
        let (mut s, _, _) = session();
        let r = s
            .run(
                "accounts.add",
                &json!({"kind": "pop3", "address": "me@example.net", "incoming": {"host": "pop.example.net"},
                    "outgoing": {"host": "smtp.example.net"}, "password": "pw"}),
            )
            .unwrap();
        let acct = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let Event::Pop3New { raw, .. } = pop3_new(acct, "h1", "Ann <ann@example.org>", "later") else {
            unreachable!()
        };
        apply_all(
            &mut s,
            vec![Event::Pop3New {
                account: acct,
                uidl: "h1".into(),
                raw: raw.clone(),
                size: None,
                headers_only: true,
            }],
        );
        let id = find_pop3(&s, acct, "h1").unwrap();
        assert!(s.body_now(id).text.is_none(), "header only");
        s.run("mail.pin", &json!({"ids": [id.0], "on": true})).unwrap();
        let vote = mailcraft_model::VoteRecord {
            from: "bo@example.org".into(),
            choice: "Yes".into(),
            ..Default::default()
        };
        if let Some(m) = s.mailbox.mail.get_mut(id) {
            m.votes.push(vote.clone());
            m.categories = vec!["Blue".into()];
        }
        // Mark to Download: the whole message arrives for the same item.
        apply_all(&mut s, vec![Event::Pop3Full { account: acct, uidl: "h1".into(), raw }]);
        let m = s.mailbox.mail.get(id).unwrap();
        assert!(s.body_now(id).text.as_deref().is_some_and(|t| t.contains("Hi.")), "the body is filled in");
        assert!(m.pinned, "still pinned");
        assert_eq!(m.votes, [vote]);
        assert_eq!(m.categories, ["Blue"]);
        assert!(!crate::accounts::prefs(&mut s).headers_only.contains(&id.0));
    }

    #[test]
    fn pop3_remove_when_deleted_spares_messages_moved_to_another_data_file() {
        // "Remove from server when deleted from Deleted Items" removes the server copy of mail deleted here,
        // not of mail the user moved into another data file (archived).
        let (mut s, _, _) = session();
        let r = s
            .run(
                "accounts.add",
                &json!({"kind": "pop3", "address": "me@example.net", "incoming": {"host": "pop.example.net"},
                    "outgoing": {"host": "smtp.example.net"}, "password": "pw"}),
            )
            .unwrap();
        let acct = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        s.run(
            "accounts.options",
            &json!({"account": acct.0, "leaveOnServer": true, "removeWhenDeleted": true}),
        )
        .unwrap();
        apply_all(
            &mut s,
            vec![
                pop3_new(acct, "u1", "Ann <ann@example.org>", "keep"),
                pop3_new(acct, "u2", "Bo <bo@example.org>", "gone"),
            ],
        );
        let other = s
            .mailbox
            .accounts
            .iter()
            .map(|a| a.id)
            .find(|a| *a != acct && s.mailbox.role_folder(*a, FolderRole::Inbox).is_some())
            .unwrap();
        let archive = s.mailbox.role_folder(other, FolderRole::Inbox).unwrap();
        let moved = find_pop3(&s, acct, "u1").unwrap();
        let deleted = find_pop3(&s, acct, "u2").unwrap();
        s.mailbox.move_items(&[moved], archive).unwrap();
        s.mailbox.mail.remove(deleted);
        let a = s.mailbox.account(acct).cloned().unwrap();
        let job = pop3_job(&s, &a, true, false, &[], s.now().unix).unwrap();
        assert!(job.delete.contains("u2"), "deleted here: the server copy goes");
        assert!(!job.delete.contains("u1"), "archived to another data file: the server copy stays");
        // Without "Leave a copy on the server" every downloaded message leaves the server, moved or not.
        s.run("accounts.options", &json!({"account": acct.0, "leaveOnServer": false})).unwrap();
        let job = pop3_job(&s, &a, true, false, &[], s.now().unix).unwrap();
        assert!(job.delete.contains("u1") && job.delete.contains("u2"));
    }

    #[test]
    fn new_mail_goes_through_the_junk_filter_before_the_rules() {
        let (mut s, _, _) = session();
        let acct = imap_account(&mut s);
        s.run("mail.junk", &json!({"action": "add", "list": "blockedSenders", "entry": "spam@bad.example"}))
            .unwrap();
        s.run(
            "rules.create",
            &json!({"name": "Hello", "conditions": [{"SubjectContains": ["hello"]}],
                    "actions": [{"AssignCategory": "Red category"}]}),
        )
        .unwrap();
        apply_all(
            &mut s,
            vec![
                pop3_new(acct, "u1", "Spammer <spam@bad.example>", "hello there"),
                pop3_new(acct, "u2", "Ann <ann@example.org>", "hello again"),
            ],
        );
        let junk = s.mailbox.role_folder(acct, FolderRole::JunkEmail).unwrap();
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        let by_subject = |subj: &str| s.mailbox.mail.items().find(|m| m.subject == subj).unwrap();
        let spam = by_subject("hello there");
        assert_eq!(spam.folder, junk, "blocked sender filed in Junk Email");
        assert!(spam.categories.is_empty(), "rules do not run on junk");
        let ham = by_subject("hello again");
        assert_eq!(ham.folder, inbox);
        assert_eq!(ham.categories, vec!["Red category".to_string()]);
        assert_eq!(ham.focus, mailcraft_model::FocusClass::Focused);
        // Always Move to Other reaches mail that arrives later.
        s.run("options.set", &json!({"key": "mail.focused.otherSenders", "value": ["ann@example.org"]}))
            .unwrap();
        apply_all(&mut s, vec![pop3_new(acct, "u3", "Ann <ann@example.org>", "later")]);
        let later = s.mailbox.mail.items().find(|m| m.subject == "later").unwrap();
        assert_eq!(later.focus, mailcraft_model::FocusClass::Other);
    }

    #[test]
    fn sent_messages_honour_save_sent_and_lose_submission_headers() {
        let (mut s, _, _) = session();
        let acct = imap_account(&mut s);
        s.run("mail.junk", &json!({"action": "set", "settings": {"addRecipientsToSafe": true}})).unwrap();
        let outbox = s.mailbox.role_folder(acct, FolderRole::Outbox).unwrap();
        let sent_folder = s.mailbox.role_folder(acct, FolderRole::SentItems).unwrap();
        let queued = |s: &mut Session, extra: &str, to: &str| {
            let raw = format!(
                "From: me@example.net\r\nTo: {to}\r\nSubject: Report\r\n{extra}\
                 Content-Type: text/plain\r\n\r\nBody\r\n"
            );
            let item = mailcraft_store::import::item_from_raw(raw.as_bytes(), outbox, s.now());
            s.add_message(item, &mailcraft_store::Content::source(raw.into_bytes())).unwrap()
        };
        let dropped = queued(&mut s, "X-MailCraft-Submit-Save-Sent: none\r\n", "bo@example.org");
        let filed =
            queued(&mut s, "X-MailCraft-Submit-DSN: NOTIFY=SUCCESS,FAILURE; RET=HDRS\r\n", "cy@example.org");
        apply_all(
            &mut s,
            vec![
                Event::Sent { account: acct, item: dropped, result: Ok(()) },
                Event::Sent { account: acct, item: filed, result: Ok(()) },
            ],
        );
        assert!(!s.mailbox.mail.contains(dropped), "Do Not Save");
        let m = s.mailbox.mail.get(filed).unwrap();
        assert_eq!(m.folder, sent_folder);
        let raw = String::from_utf8(s.source(m.id).unwrap().unwrap()).unwrap();
        assert!(!raw.contains("X-MailCraft-Submit"), "{raw}");
        assert!(raw.contains("Subject: Report"));
        let safe = s.run("mail.junk", &json!({"action": "get"})).unwrap();
        let safe = safe.to_string();
        assert!(safe.contains("bo@example.org") && safe.contains("cy@example.org"), "{safe}");
    }

    #[test]
    fn envelope_addresses_follow_the_international_domain_names_option() {
        let (mut s, _, _) = session();
        let acct = imap_account(&mut s);
        if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == acct) {
            a.address = "me@bücher.example".into();
        }
        let outbox = s.mailbox.role_folder(acct, FolderRole::Outbox).unwrap();
        let raw = "From: me@bücher.example\r\nTo: ann@例え.example, Ann@例え.example\r\n\
                   Cc: bo@example.org\r\nSubject: Hello\r\n\r\nBody\r\n";
        let item = mailcraft_store::import::item_from_raw(raw.as_bytes(), outbox, s.now());
        s.add_message(item, &mailcraft_store::Content::source(raw.as_bytes().to_vec())).unwrap();
        let envelope = |s: &Session| {
            let a = s.mailbox.account(acct).unwrap();
            let out = outgoing_items(s, a, &Plan::all(), s.now().unix);
            assert_eq!(out.len(), 1);
            (out[0].from.clone(), out[0].rcpts.clone())
        };
        // On (default): ASCII domains, so a server without SMTPUTF8 takes the message.
        let (from, rcpts) = envelope(&s);
        assert_eq!(from, "me@xn--bcher-kva.example");
        assert_eq!(rcpts, vec!["ann@xn--r8jz45g.example".to_owned(), "bo@example.org".to_owned()]);
        // Off: left as written (sending refuses such addresses before they reach the Outbox).
        s.run("options.set", &json!({"key": "advanced.idn", "value": false})).unwrap();
        let (from, rcpts) = envelope(&s);
        assert_eq!(from, "me@bücher.example");
        assert_eq!(rcpts.first().map(String::as_str), Some("ann@例え.example"));
    }

    // -----------------------------------------------------------------------------------------------
    // Download progress against the local test servers (127.0.0.1, synthetic mail)

    const PROGRESS_USER: &str = "alice@example.com";
    const PROGRESS_PW: &str = "alice-progress-pw";

    fn progress_servers() -> mailcraft_testmail::TestServers {
        mailcraft_testmail::TestServers::start(mailcraft_testmail::Config {
            users: vec![(PROGRESS_USER.into(), PROGRESS_PW.into())],
            add_trace_headers: false,
            ..mailcraft_testmail::Config::default()
        })
        .unwrap()
    }

    fn seed(srv: &mailcraft_testmail::TestServers, mailbox: &str, n: usize) {
        for i in 0..n {
            let raw = format!(
                "From: bob@example.org\r\nTo: {PROGRESS_USER}\r\nSubject: Note {mailbox} {i}\r\n\
                 Date: Mon, 5 Oct 2026 10:00:00 +0000\r\nMessage-ID: <{mailbox}-{i}@example.org>\r\n\r\nHi\r\n"
            );
            srv.seed_message(PROGRESS_USER, mailbox, raw.as_bytes(), &[]).unwrap();
        }
    }

    fn progress_session(
        srv: &mailcraft_testmail::TestServers,
        incoming: Value,
        kind: &str,
    ) -> (Session, AccountId) {
        let mut s = Session::new(Mailbox::new_local("Local Folders"));
        s.view.auto_send_receive_minutes = 0;
        let r = s
            .run(
                "accounts.add",
                &json!({"kind": kind, "address": PROGRESS_USER, "incoming": incoming, "password": PROGRESS_PW,
                    "outgoing": {"host": "127.0.0.1", "port": srv.smtp_port, "security": "starttls",
                        "extraRootPem": srv.cert_pem}}),
            )
            .unwrap();
        (s, AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap()))
    }

    /// Runs a full send/receive, keeping every event the worker sends until its run ends, then hands them to
    /// the session as usual. Returns the counted progress reports in order.
    fn sync_watching(s: &mut Session, id: AccountId) -> Vec<(String, SyncProgress)> {
        assert_eq!(s.send_receive(&[id], Plan::all()).unwrap(), 1);
        let mut events = Vec::new();
        loop {
            let (serial, e) = s.sync.rt.rx.recv_timeout(Duration::from_secs(30)).expect("worker event");
            let done = matches!(e, Event::Done { .. });
            events.push((serial, e));
            if done {
                break;
            }
        }
        let counted = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::Progress { text, detail: Some(d), .. } => Some((text.clone(), d.clone())),
                _ => None,
            })
            .collect();
        for e in events {
            s.sync.rt.tx.send(e).unwrap();
        }
        assert!(s.wait_for_sync(Duration::from_secs(30)), "{:?}", s.sync_status());
        assert!(s.sync_status().detail.is_none(), "no counts once the run is over");
        assert!(s.sync_status().accounts.iter().all(|a| a.detail.is_none()));
        counted
    }

    fn assert_counts_climb(reports: &[&SyncProgress], total: u64) {
        assert!(!reports.is_empty());
        assert!(reports.iter().all(|d| d.total == total && d.done <= d.total), "{reports:?}");
        assert!(reports.windows(2).all(|w| w[0].done <= w[1].done), "{reports:?}");
        assert_eq!(reports.first().map(|d| d.done), Some(0), "the count shows before the first batch");
        assert_eq!(reports.last().map(|d| d.done), Some(total), "the count reaches the total");
    }

    #[test]
    fn imap_first_sync_reports_folder_and_message_counts() {
        let srv = progress_servers();
        seed(&srv, "INBOX", 130);
        srv.create_mailbox(PROGRESS_USER, "Projects", None).unwrap();
        seed(&srv, "Projects", 3);
        let incoming = json!({"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls",
            "extraRootPem": srv.cert_pem});
        let (mut s, id) = progress_session(&srv, incoming, "imap");
        let counted = sync_watching(&mut s, id);

        let inbox: Vec<&SyncProgress> =
            counted.iter().map(|(_, d)| d).filter(|d| d.folder == "Inbox" && d.total > 0).collect();
        assert_counts_climb(&inbox, 130);
        let folders = inbox[0].folder_count;
        assert!(folders >= 2, "{counted:?}");
        assert!(inbox.iter().all(|d| d.folder_index == 1 && d.folder_count == folders));
        assert!(
            counted.iter().any(|(t, _)| t == &format!("Downloading Inbox: 130 of 130 messages (folder 1 of {folders})")),
            "{counted:?}"
        );
        assert!(counted.iter().any(|(t, _)| t == &format!("Updating Inbox (folder 1 of {folders})…")));
        let projects: Vec<&SyncProgress> =
            counted.iter().map(|(_, d)| d).filter(|d| d.folder == "Projects" && d.total > 0).collect();
        assert_counts_climb(&projects, 3);
        assert!(projects.iter().all(|d| d.folder_index > 1 && d.folder_index <= folders));

        let inbox_id = s.mailbox.role_folder(id, FolderRole::Inbox).unwrap();
        assert_eq!(s.mailbox.mail.in_folder(inbox_id).count(), 130);
        assert_eq!(s.sync_status().text, "All folders are up to date.");

        // The next run finds nothing new: no message counts, only the folder steps.
        let again = sync_watching(&mut s, id);
        assert!(again.iter().all(|(_, d)| d.total == 0), "{again:?}");
    }

    #[test]
    fn pop3_and_jmap_first_sync_report_message_counts() {
        let srv = progress_servers();
        seed(&srv, "INBOX", 12);
        let incoming = json!({"host": "127.0.0.1", "port": srv.pop3s_port, "security": "tls",
            "extraRootPem": srv.cert_pem});
        let (mut s, id) = progress_session(&srv, incoming, "pop3");
        let counted = sync_watching(&mut s, id);
        let reports: Vec<&SyncProgress> = counted.iter().map(|(_, d)| d).collect();
        assert_counts_climb(&reports, 12);
        assert!(reports.iter().all(|d| d.folder == "Inbox" && d.folder_count == 0));
        assert!(counted.iter().any(|(t, _)| t == "Downloading Inbox: 12 of 12 messages"), "{counted:?}");

        let srv = progress_servers();
        seed(&srv, "INBOX", 150);
        let (mut s, id) =
            progress_session(&srv, json!({"host": srv.jmap_url(), "extraRootPem": srv.cert_pem}), "jmap");
        let counted = sync_watching(&mut s, id);
        let reports: Vec<&SyncProgress> = counted.iter().map(|(_, d)| d).collect();
        assert_counts_climb(&reports, 150);
        assert!(counted.iter().any(|(t, _)| t == "Downloading 150 of 150 messages"), "{counted:?}");
        assert_eq!(s.mailbox.mail.items().filter(|m| m.subject.starts_with("Note INBOX")).count(), 150);
    }

    /// Queues an Include Older Results search for "budget" on `id`, the way a search started while the
    /// account was busy waits for its next run, then runs a full Send/Receive.
    fn search_with_receive(s: &mut Session, id: AccountId) {
        let folders = s.mailbox.folders.values().filter(|f| f.account == id).map(|f| f.id).collect();
        let criterion = super::super::older::criterion(s, "budget").unwrap();
        s.sync.older.on = true;
        s.sync.older.query = "budget".into();
        s.sync.older.generation = s.sync.older.generation.wrapping_add(1);
        s.sync.older.pending.insert(id);
        let generation = s.sync.older.generation;
        s.sync.older_requests.insert(id, super::super::older::Request { generation, criterion, folders });
        assert_eq!(s.send_receive(&[id], Plan::all()).unwrap(), 1);
        assert!(s.wait_for_sync(Duration::from_secs(30)), "{:?}", s.sync_status());
    }

    /// Regression: a JMAP server that refused the Include Older Results search (`unsupportedFilter`) failed
    /// the whole run it rode along with, so the account's new mail was not received.
    #[test]
    fn a_refused_jmap_search_does_not_stop_receiving() {
        let srv = mailcraft_testmail::TestServers::start(mailcraft_testmail::Config {
            users: vec![(PROGRESS_USER.into(), PROGRESS_PW.into())],
            add_trace_headers: false,
            jmap_unsupported_filters: vec!["text".into()],
            ..mailcraft_testmail::Config::default()
        })
        .unwrap();
        seed(&srv, "INBOX", 1);
        let (mut s, id) =
            progress_session(&srv, json!({"host": srv.jmap_url(), "extraRootPem": srv.cert_pem}), "jmap");
        assert_eq!(s.send_receive(&[id], Plan::all()).unwrap(), 1);
        assert!(s.wait_for_sync(Duration::from_secs(30)), "{:?}", s.sync_status());
        let raw = format!(
            "From: bob@example.org\r\nTo: {PROGRESS_USER}\r\nSubject: Arrived meanwhile\r\n\
             Date: Mon, 5 Oct 2026 11:00:00 +0000\r\nMessage-ID: <later@example.org>\r\n\r\nHi\r\n"
        );
        srv.seed_message(PROGRESS_USER, "INBOX", raw.as_bytes(), &[]).unwrap();
        search_with_receive(&mut s, id);
        assert!(
            s.mailbox.mail.items().any(|m| m.subject == "Arrived meanwhile"),
            "the receive ran although the search was refused"
        );
        let error = s.sync.older.error.clone().unwrap_or_default();
        assert!(error.contains("unsupportedFilter"), "the search reports why: {error:?}");
        assert!(s.sync.older.pending.is_empty());
        let st = s.sync_status();
        assert!(st.accounts.iter().all(|a| a.error.is_none()), "no account error: {st:?}");
    }

    /// The same for IMAP: a server that refuses a text SEARCH fails the search, not the receive.
    #[test]
    fn a_refused_imap_search_does_not_stop_receiving() {
        let srv = mailcraft_testmail::TestServers::start(mailcraft_testmail::Config {
            users: vec![(PROGRESS_USER.into(), PROGRESS_PW.into())],
            add_trace_headers: false,
            imap_refuse_text_search: true,
            ..mailcraft_testmail::Config::default()
        })
        .unwrap();
        seed(&srv, "INBOX", 1);
        let incoming = json!({"host": "127.0.0.1", "port": srv.imaps_port, "security": "tls",
            "extraRootPem": srv.cert_pem});
        let (mut s, id) = progress_session(&srv, incoming, "imap");
        assert_eq!(s.send_receive(&[id], Plan::all()).unwrap(), 1);
        assert!(s.wait_for_sync(Duration::from_secs(30)), "{:?}", s.sync_status());
        let raw = format!(
            "From: bob@example.org\r\nTo: {PROGRESS_USER}\r\nSubject: Arrived meanwhile\r\n\
             Date: Mon, 5 Oct 2026 11:00:00 +0000\r\nMessage-ID: <later@example.org>\r\n\r\nHi\r\n"
        );
        srv.seed_message(PROGRESS_USER, "INBOX", raw.as_bytes(), &[]).unwrap();
        search_with_receive(&mut s, id);
        assert!(s.mailbox.mail.items().any(|m| m.subject == "Arrived meanwhile"), "{:?}", s.sync_status());
        assert!(s.sync.older.error.is_some(), "the search reports that it failed");
        let st = s.sync_status();
        assert!(st.accounts.iter().all(|a| a.error.is_none()), "no account error: {st:?}");
    }

    #[test]
    fn progress_text_and_throttle() {
        let mut d = SyncProgress {
            folder: "Inbox".into(),
            folder_index: 3,
            folder_count: 12,
            done: 1240,
            total: 5000,
            headers: false,
        };
        assert_eq!(d.text(), "Downloading Inbox: 1,240 of 5,000 messages (folder 3 of 12)");
        assert_eq!(d.fraction(), Some(0.248));
        d.headers = true;
        d.folder_count = 1;
        d.done = 9_000;
        assert_eq!(d.text(), "Downloading Inbox: 5,000 of 5,000 headers", "done never passes the total");
        d.total = 0;
        d.folder_count = 12;
        assert_eq!(d.text(), "Updating Inbox (folder 3 of 12)…");
        assert_eq!(d.fraction(), None);
        let j = SyncProgress { done: 7, total: 1_234_567, ..SyncProgress::default() };
        assert_eq!(j.text(), "Downloading 7 of 1,234,567 messages");
        assert_eq!(SyncProgress::default().text(), "Updating messages…");
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(u64::MAX), "18,446,744,073,709,551,615");

        let mut t = Throttle::default();
        assert!(t.due(false), "the first report goes out");
        assert!(!t.due(false), "the next waits");
        assert!(t.due(true), "forced reports always go out");
    }

    #[test]
    fn filling_a_header_only_item_keeps_its_state() {
        // Regression: "Do not AutoArchive this item" was lost when a header-only message was downloaded in
        // full, so the next AutoArchive run could move or delete it.
        let (mut s, folder, _) = session();
        let id = s.mailbox.add_mail(MailItem {
            folder,
            subject: "Headers only".into(),
            remote_id: Some("5".into()),
            ignored: true,
            no_auto_archive: true,
            read: true,
            ..Default::default()
        });
        crate::accounts::prefs(&mut s).headers_only.insert(id.0);
        let raw = b"From: a@example.com\r\nSubject: Headers only\r\n\r\nThe whole body\r\n";
        assert!(fill_item(&mut s, id, raw));
        assert_eq!(s.body_now(id).text.as_deref().map(str::trim), Some("The whole body"));
        let m = s.mailbox.mail.get(id).unwrap();
        assert!(m.no_auto_archive && m.ignored && m.read);
        assert_eq!(m.remote_id.as_deref(), Some("5"));
        assert!(!crate::sync::cmds::is_header_only(&s, id));
    }
}
