//! Microsoft Graph accounts (Microsoft 365 and Outlook.com mailboxes through the Microsoft Graph REST API
//! v1.0, signed in with OAuth): the mail folder tree with child folders (the well-known folders become the
//! default folders), mail from a delta query per folder (the first one limited to the account's download
//! window, later ones continuing from the saved `deltaLink`), each new message's MIME source from `$value`
//! (or only its header, built from the delta's properties, under a headers-first download preference; the
//! rest is downloaded when opened or marked), local changes written back (read state, follow-up flag,
//! categories, importance, moves, permanent deletes, uploads, folders made, renamed, moved and removed), and
//! the Outbox sent with `sendMail` (MIME content), or as a draft with its large attachments uploaded through
//! upload sessions and then sent. The service files sent mail in Sent Items itself.
//!
//! Folder `remote_name` = folder id; item `remote_id` = message id (immutable ids, so a message keeps its id
//! when it moves). Like JMAP, Graph has no change queue here: the account's bookkeeping keeps a *shadow* of
//! what the service last had for every message (folder, read, flag with its start and due day, categories,
//! importance) and for every folder without a role (name, parent), and each run sends what differs from it.
//! The service files every upload as a draft; whether a message is a draft is taken from it only for mail
//! first downloaded from it.
//!
//! **New folders.** A folder made here is created first in a run; until the service names it, the job refers
//! to it as `#f<folder id>` (in parents, moves and uploads). A folder below a default folder the service does
//! not have (Conversation History, RSS Feeds) stays here: mail moved into it keeps its copy on the service
//! where it was.
//!
//! **Moves on the service.** A delta query is per folder: a message moved there shows as removed from one
//! folder and listed in another. Removals are applied at the end of a run, without the messages some folder
//! of the run listed. A run that did not read every folder (the Inbox polled, one folder's Send/Receive, a
//! folder list cut short by the caps) asks the service where each removed message is now, so a moved
//! message keeps its item; only one the service no longer has goes. A folder list is applied (folders
//! missing from it removed here) only when it is the whole tree, never after a cancel.
//!
//! **Uploads.** Mail moved or copied in from elsewhere is uploaded; a large one goes without its large files,
//! which follow through upload sessions, and when one of them fails the message is deleted again, so no
//! half-made copy stays on the service (the same for a large message sent from the Outbox).
//!
//! **Push.** Graph change notifications need a public web endpoint, which a desktop client does not have:
//! the account's Inbox is polled instead (Options › push off: scheduled send/receive only).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mailcraft_net::graph::GraphClient;
use mailcraft_net::graph::mail::{
    FileAttachment, FlagStatus, GraphMessage, MAX_INLINE_REQUEST, MailFolder, MessagePatch,
};
use mailcraft_net::{ConnectOptions, Credentials, NetError};

use super::native::{Event, Events, Outgoing};
use super::*;
use crate::accounts::{GraphFolderShadow, GraphShadow, prefs};
use mailcraft_model::{Account, FocusClass, FolderRole, Importance};

/// Most delta pages read for one folder in one run (the rest continues next time).
const MAX_PAGES: usize = 2_000;
/// Page size asked of delta queries.
const PAGE_SIZE: u32 = 100;
/// Largest message source uploaded or sent in one request (base64 grows it by a third).
const MAX_ONE_REQUEST: usize = MAX_INLINE_REQUEST / 4 * 3 - 64 * 1024;
/// The destination of a move to the top of the folder tree.
const ROOT: &str = "msgfolderroot";

/// How to reach a Graph account.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GraphConn {
    base: String,
    token: String,
    options: ConnectOptions,
}

impl std::fmt::Debug for GraphConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphConn").field("base", &self.base).field("token", &"..").finish()
    }
}

impl GraphConn {
    pub(crate) fn new(base: &str, creds: &Credentials, options: ConnectOptions) -> GraphConn {
        let token = match creds {
            Credentials::Password { password, .. } => password.clone(),
            Credentials::OAuth2 { access_token, .. } => access_token.clone(),
        };
        GraphConn { base: base.trim().to_owned(), token, options }
    }
    pub(crate) fn set_token(&mut self, token: &str) {
        token.clone_into(&mut self.token);
    }
    pub(crate) fn base(&self) -> &str {
        &self.base
    }
    pub(crate) fn connect(&self, cancel: Option<&Arc<AtomicBool>>) -> Result<GraphClient, NetError> {
        let http = mailcraft_net::http::HttpClient::new(self.options.clone());
        let mut c = GraphClient::new(&self.base, &self.token, http)?;
        c.page_size = PAGE_SIZE;
        Ok(match cancel {
            Some(cancel) => c.with_cancel(Arc::clone(cancel)),
            None => c,
        })
    }
}

/// One run's work for a Graph account.
#[derive(Clone, Debug, Default)]
pub(crate) struct GraphJob {
    receive: bool,
    /// Folders to update (`None`: all of them), with their names here.
    only: Option<BTreeMap<String, String>>,
    /// The folder list was read before: a run for some folders only (polling the Inbox) skips reading it.
    listed: bool,
    /// Folder id → the delta link to continue from.
    delta: BTreeMap<String, String>,
    /// Folders without a delta link yet: only mail received on or after this UTC time (RFC 3339).
    received_after: Option<String>,
    /// New mail arrives as headers only.
    headers_only: bool,
    /// After the headers, download the new messages whole in the same run.
    then_full: bool,
    /// Message ids held here.
    known: BTreeSet<String>,
    /// (message id, patch, shadow after it).
    updates: Vec<(String, MessagePatch, GraphShadow)>,
    /// (message id, destination folder id or placeholder, shadow after it).
    moves: Vec<(String, String, GraphShadow)>,
    destroy: Vec<String>,
    /// (item, folder id or placeholder, source, shadow after it).
    imports: Vec<(ItemId, String, Vec<u8>, GraphShadow)>,
    /// Folders to create, parents first: (folder, name, parent folder id or placeholder).
    folder_creates: Vec<(FolderId, String, Option<String>)>,
    /// (folder id, new name, new parent (`Some(None)`: the top level), shadow after it).
    folder_updates: Vec<(String, Option<String>, Option<Option<String>>, GraphFolderShadow)>,
    /// Folders removed here, children first.
    folder_destroys: Vec<String>,
    /// Header-only messages to download whole: (item, message id).
    fetch: Vec<(ItemId, String)>,
}

/// A message as downloaded: its properties and, when new, its source (or only a header).
#[derive(Clone, Debug, Default)]
pub(crate) struct GMsg {
    meta: GraphMessage,
    raw: Option<Vec<u8>>,
    headers_only: bool,
}

pub(crate) enum GraphEvent {
    Folders {
        list: Vec<MailFolder>,
        /// The whole tree (else a folder missing from `list` may still be on the service).
        complete: bool,
    },
    Messages {
        folder: String,
        list: Vec<GMsg>,
    },
    /// Messages gone from the service (not listed in any folder this run).
    Gone {
        ids: Vec<String>,
    },
    DeltaLink {
        folder: String,
        link: String,
    },
    /// The service lost the folder's delta state: its next update starts over.
    DeltaReset {
        folder: String,
    },
    Updated {
        id: String,
        shadow: GraphShadow,
    },
    Moved {
        old: String,
        new: String,
        shadow: GraphShadow,
    },
    Destroyed {
        ids: Vec<String>,
    },
    Imported {
        item: ItemId,
        id: String,
        shadow: GraphShadow,
    },
    FolderCreated {
        folder: FolderId,
        id: String,
        shadow: GraphFolderShadow,
    },
    /// A folder renamed or moved (`id`: its id afterwards).
    FolderUpdated {
        old: String,
        id: String,
        shadow: GraphFolderShadow,
    },
    FoldersDestroyed {
        ids: Vec<String>,
    },
    /// Whole messages for header-only items: (message id, source).
    Bodies {
        list: Vec<(String, Vec<u8>)>,
    },
}

// ---------------------------------------------------------------------------------------------------
// Session side: building a job
// ---------------------------------------------------------------------------------------------------

fn placeholder(f: FolderId) -> String {
    format!("#f{}", f.0)
}

fn resolve(m: &str, created: &BTreeMap<String, String>) -> Option<String> {
    if m.starts_with("#f") { created.get(m).cloned() } else { Some(m.to_owned()) }
}

fn flag_status(m: &MailItem) -> FlagStatus {
    match m.flag {
        FlagState::None => FlagStatus::NotFlagged,
        FlagState::Flagged { .. } => FlagStatus::Flagged,
        FlagState::Complete { .. } => FlagStatus::Complete,
    }
}

fn importance_name(i: Importance) -> &'static str {
    match i {
        Importance::Low => "low",
        Importance::Normal => "normal",
        Importance::High => "high",
    }
}

fn sorted(v: &[String]) -> Vec<String> {
    let mut c: Vec<String> = v.iter().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
    c.sort();
    c.dedup();
    c
}

/// A flag's (start, due) as the service keeps them: either both or neither (a missing one is the other).
fn both(start: Option<String>, due: Option<String>) -> (Option<String>, Option<String>) {
    (start.clone().or_else(|| due.clone()), due.or(start))
}

/// A flagged item's start and due day, in UTC, as the service keeps them (`None` for an item not flagged).
fn flag_dates(m: &MailItem) -> (Option<String>, Option<String>) {
    match &m.flag {
        FlagState::Flagged { start, due, .. } => both(start.as_ref().map(utc), due.as_ref().map(utc)),
        _ => (None, None),
    }
}

/// What the service should have for `m` in `folder`.
fn shadow_of(m: &MailItem, folder: &str) -> GraphShadow {
    let (start, due) = flag_dates(m);
    GraphShadow {
        folder: folder.to_owned(),
        read: m.read,
        flag: flag_status(m).name().to_owned(),
        start,
        due,
        categories: sorted(&m.categories),
        importance: importance_name(m.importance).to_owned(),
    }
}

/// `YYYY-MM-DDTHH:MM:SS` in UTC, as Graph's `dateTimeTimeZone` takes it.
fn utc(d: &mailcraft_model::DateTime) -> String {
    let c = d.civil(0);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", c.year, c.month, c.day, c.hour, c.minute, c.second)
}

/// A `dateTimeTimeZone.dateTime` the service sent (UTC, maybe with a fraction of a second).
fn graph_time(s: &str) -> Option<mailcraft_model::DateTime> {
    let t = s.trim();
    let head = t.get(..19).filter(|h| h.contains('T')).unwrap_or(t);
    mailcraft_model::DateTime::parse_rfc5322(head).ok()
}

/// A flag date as the shadow keeps it (the service's text in the form [`utc`] gives).
fn shadow_time(s: Option<&str>) -> Option<String> {
    s.and_then(graph_time).map(|d| utc(&d))
}

/// The PATCH turning `was` into `now` (`m` gives the flag dates).
fn patch_for(was: &GraphShadow, now: &GraphShadow, m: &MailItem) -> MessagePatch {
    let mut p = MessagePatch::default();
    if was.read != now.read {
        p.is_read = Some(now.read);
    }
    // A new start or due day of a message already flagged is a change of the flag too.
    let dates_changed =
        now.flag == FlagStatus::Flagged.name() && (was.start != now.start || was.due != now.due);
    if was.flag != now.flag || dates_changed {
        let (start, due) = flag_dates(m);
        p.clear_flag_dates = due.is_none() && (was.start.is_some() || was.due.is_some());
        p.flag = Some((FlagStatus::parse(&now.flag), start, due));
    }
    if was.categories != now.categories {
        p.categories =
            Some(m.categories.iter().map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).collect());
    }
    if was.importance != now.importance {
        p.importance = Some(now.importance.clone());
    }
    p
}

/// The local changes and receive state of one run. `window` is the first day (UTC) of the first download.
pub(crate) fn build_job(
    s: &Session,
    a: &Account,
    plan: &Plan,
    window: Option<(i64, u8, u8)>,
    headers_only: bool,
    then_full: bool,
    fetch: &[ItemId],
) -> Option<GraphJob> {
    let st = s.sync.prefs.data.graph.get(&a.id.0).cloned().unwrap_or_default();
    let outbox = s.mailbox.role_folder(a.id, FolderRole::Outbox);
    let only = plan.folders.as_ref().map(|fs| {
        fs.iter()
            .filter_map(|f| s.mailbox.folder(*f))
            .filter(|f| f.account == a.id)
            .filter_map(|f| Some((f.remote_name.clone()?, f.name.clone())))
            .collect()
    });
    let mut job = GraphJob {
        receive: plan.receive,
        only,
        listed: st.listed,
        delta: st.delta.clone(),
        received_after: window
            .filter(|(y, _, _)| (1..=9999).contains(y))
            .map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}T00:00:00Z")),
        headers_only,
        then_full,
        ..Default::default()
    };
    let target = folder_job(s, a, &st, outbox, &mut job);
    let mut here: BTreeSet<String> = BTreeSet::new();
    let account_folders: BTreeSet<FolderId> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.account == a.id && f.kind == FolderKind::Mail)
        .map(|f| f.id)
        .collect();
    for m in s.mailbox.mail.items() {
        if !account_folders.contains(&m.folder) || Some(m.folder) == outbox {
            continue;
        }
        let Some(folder) = target.get(&m.folder) else {
            // In a folder of the account that stays here (below a default folder the service does not
            // have): the service keeps its copy where it was, never deleted for a move inside the account.
            if let Some(id) = m.remote_id.as_deref()
                && keeps_server_copy(&s.mailbox, a.id, m.folder)
            {
                here.insert(id.to_owned());
            }
            continue;
        };
        let now = shadow_of(m, folder);
        match m.remote_id.as_deref() {
            Some(id) => {
                here.insert(id.to_owned());
                let Some(was) = st.shadow.get(id) else { continue };
                let patch = patch_for(was, &now, m);
                if !patch.is_empty() {
                    let after = GraphShadow { folder: was.folder.clone(), ..now.clone() };
                    job.updates.push((id.to_owned(), patch, after));
                }
                if was.folder != *folder {
                    job.moves.push((id.to_owned(), folder.clone(), now));
                }
            }
            None => job.imports.push((m.id, folder.clone(), item_source(s, m), now)),
        }
    }
    job.destroy = st.shadow.keys().filter(|id| !here.contains(*id)).cloned().collect();
    job.known = here;
    job.known.extend(st.shadow.keys().cloned());
    job.fetch = fetch
        .iter()
        .filter_map(|i| s.mailbox.mail.get(*i))
        .filter(|m| account_folders.contains(&m.folder))
        .filter_map(|m| Some((m.id, m.remote_id.clone()?)))
        .collect();
    if !job.receive
        && job.updates.is_empty()
        && job.moves.is_empty()
        && job.destroy.is_empty()
        && job.imports.is_empty()
        && job.folder_creates.is_empty()
        && job.folder_updates.is_empty()
        && job.folder_destroys.is_empty()
        && job.fetch.is_empty()
    {
        return None;
    }
    Some(job)
}

/// Default folders made on the service when it has none for them (Graph's well-known folders, which every
/// mailbox normally has).
fn creatable_role(r: FolderRole) -> bool {
    matches!(
        r,
        FolderRole::Archive
            | FolderRole::DeletedItems
            | FolderRole::JunkEmail
            | FolderRole::SentItems
            | FolderRole::Drafts
    )
}

fn folder_depth(mb: &Mailbox, f: FolderId) -> usize {
    let mut n = 0usize;
    let mut cur = mb.folder(f).and_then(|x| x.parent);
    while let Some(p) = cur {
        n = n.saturating_add(1);
        if n > mb.folders.len() {
            break;
        }
        cur = mb.folder(p).and_then(|x| x.parent);
    }
    n
}

/// The folder part of a job. Returns, for every mail folder of the account that is (or will be) on the
/// service, its folder id or placeholder.
fn folder_job(
    s: &Session,
    a: &Account,
    st: &crate::accounts::GraphState,
    outbox: Option<FolderId>,
    job: &mut GraphJob,
) -> BTreeMap<FolderId, String> {
    let below_outbox: BTreeSet<FolderId> =
        outbox.map(|o| s.mailbox.subtree(o)).unwrap_or_default().into_iter().collect();
    let mut folders: Vec<&Folder> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.account == a.id && f.kind == FolderKind::Mail && !below_outbox.contains(&f.id))
        .collect();
    folders.sort_by_cached_key(|f| (folder_depth(&s.mailbox, f.id), f.id));
    // Default folders the service has no folder for are made there once mail from the service is moved into
    // them or into a folder below them (else that mail would leave the service).
    let mut holds_server_mail: BTreeSet<FolderId> = BTreeSet::new();
    for m in s.mailbox.mail.items().filter(|m| m.remote_id.is_some()) {
        let mut cur = Some(m.folder);
        let mut steps = 0usize;
        while let Some(f) = cur {
            if !holds_server_mail.insert(f) || steps > s.mailbox.folders.len() {
                break;
            }
            steps = steps.saturating_add(1);
            cur = s.mailbox.folder(f).and_then(|x| x.parent);
        }
    }
    let mut target: BTreeMap<FolderId, String> = BTreeMap::new();
    for f in &folders {
        if let Some(r) = &f.remote_name {
            target.insert(f.id, r.clone());
        } else if f.role.is_some_and(creatable_role) {
            // A default folder the service has no folder for: made there (top level, under its name here) so
            // mail moved into it stays on the service.
            if f.parent.is_none() && st.listed && holds_server_mail.contains(&f.id) {
                job.folder_creates.push((f.id, f.name.clone(), None));
                target.insert(f.id, placeholder(f.id));
            }
        } else if f.role.is_none() && st.listed {
            let parent = match f.parent {
                None => Some(None),
                Some(p) => target.get(&p).map(|t| Some(t.clone())),
            };
            // Below a folder that stays local: local too.
            if let Some(parent) = parent {
                job.folder_creates.push((f.id, f.name.clone(), parent));
                target.insert(f.id, placeholder(f.id));
            }
        }
    }
    for f in &folders {
        let Some(r) = f.remote_name.as_ref().filter(|_| f.role.is_none()) else { continue };
        let Some(sh) = st.folders.get(r) else { continue };
        let parent = match f.parent {
            None => Some(None),
            Some(p) => target.get(&p).map(|t| Some(t.clone())),
        };
        // Moved below a folder that stays local: left where it is on the service.
        let Some(parent) = parent else { continue };
        if sh.name != f.name || sh.parent != parent {
            let rename = (sh.name != f.name).then(|| f.name.clone());
            let moved = (sh.parent != parent).then(|| parent.clone());
            job.folder_updates.push((
                r.clone(),
                rename,
                moved,
                GraphFolderShadow { name: f.name.clone(), parent },
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
            n = n.saturating_add(1);
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
    target
}

// ---------------------------------------------------------------------------------------------------
// Worker side
// ---------------------------------------------------------------------------------------------------

fn send(events: &Events, cancel: &AtomicBool, account: AccountId, ev: GraphEvent) {
    if !events.send(Event::Graph { account, ev: Box::new(ev) }) {
        cancel.store(true, Ordering::SeqCst);
    }
}

fn not_found(e: &NetError) -> bool {
    matches!(e, NetError::Graph { status: 404, .. } | NetError::Http { status: 404, .. })
}

/// The service no longer knows a saved delta state (410 Gone, with or without a Graph error body).
fn lost_state(e: &NetError) -> bool {
    matches!(e, NetError::Graph { status: 410, .. } | NetError::Http { status: 410, .. })
}

/// How a run ended: the error to show (`None`: fine) and whether the service refused the sign-in before
/// anything was changed (a renewed token may then try again).
pub(crate) struct GraphEnd {
    pub(crate) error: Option<String>,
    pub(crate) refused: bool,
}

/// The MIME content Graph should send: the submission-only fields taken out, and a `Bcc` field naming the
/// envelope recipients the header does not (Graph takes the recipients from the header and never passes
/// `Bcc` on).
fn outgoing_mime(m: &Outgoing) -> Vec<u8> {
    let raw = crate::cmd::compose::strip_submit_headers(&m.raw);
    let named: BTreeSet<String> = match mailcraft_mime::parse(&raw) {
        Ok(msg) => msg
            .to()
            .iter()
            .chain(msg.cc().iter())
            .chain(msg.bcc().iter())
            .flat_map(|a| a.mailboxes())
            .map(|mb| mb.addr.to_ascii_lowercase())
            .collect(),
        Err(_) => BTreeSet::new(),
    };
    let missing: Vec<&str> =
        m.rcpts.iter().map(String::as_str).filter(|r| !named.contains(&r.to_ascii_lowercase())).collect();
    if missing.is_empty() {
        return raw;
    }
    let mut out = format!("Bcc: {}\r\n", missing.join(", ")).into_bytes();
    out.extend_from_slice(&raw);
    out
}

/// Files of `raw` too large to go in one request with the message: the message without them, and them.
fn split_large(raw: &[u8]) -> Option<(Vec<u8>, Vec<FileAttachment>)> {
    let mut msg = mailcraft_mime::parse(raw).ok()?;
    let mut files = Vec::new();
    fn take(p: &mut mailcraft_mime::Part, files: &mut Vec<FileAttachment>, depth: usize) {
        if depth > 16 {
            return;
        }
        if let mailcraft_mime::PartBody::Multipart(parts) = &mut p.body {
            let mut keep = Vec::new();
            for mut c in std::mem::take(parts) {
                let big = matches!(&c.body, mailcraft_mime::PartBody::Leaf(d) if d.len() > 256 * 1024);
                if c.is_attachment() && big {
                    if let mailcraft_mime::PartBody::Leaf(d) =
                        std::mem::replace(&mut c.body, mailcraft_mime::PartBody::Leaf(Vec::new()))
                    {
                        files.push(FileAttachment {
                            name: c.filename.clone().unwrap_or_else(|| "attachment".into()),
                            content_type: c.content_type.mime_type.clone(),
                            content_id: c.content_id.clone(),
                            inline: c.disposition == mailcraft_mime::Disposition::Inline,
                            data: d,
                        });
                    }
                } else {
                    take(&mut c, files, depth.saturating_add(1));
                    keep.push(c);
                }
            }
            *parts = keep;
        }
    }
    take(&mut msg.root, &mut files, 0);
    if files.is_empty() {
        return None;
    }
    Some((msg.to_bytes(), files))
}

/// Creates a message from `raw` in `folder` (Drafts when `None`): in one request when small, else without
/// its large files, which then go through upload sessions.
fn create(c: &GraphClient, folder: Option<&str>, raw: &[u8]) -> Result<GraphMessage, NetError> {
    if raw.len() <= MAX_ONE_REQUEST {
        return c.create_from_mime(folder, raw);
    }
    let Some((small, files)) = split_large(raw) else {
        return Err(NetError::TooLarge(format!(
            "the message is {} KB and has no attachments that could be uploaded separately",
            raw.len() / 1024
        )));
    };
    if small.len() > MAX_ONE_REQUEST {
        return Err(NetError::TooLarge("the message text alone is too large to send".into()));
    }
    let m = c.create_from_mime(folder, &small)?;
    for f in &files {
        if let Err(e) = c.add_attachment(&m.id, f) {
            // No half-made message stays on the service (each later try would leave another one).
            if let Err(d) = c.delete_message(&m.id) {
                log::warn!("graph: a message left without its attachments could not be deleted: {d}");
            }
            return Err(e);
        }
    }
    Ok(m)
}

/// Sends one Outbox message.
fn send_one(c: &GraphClient, m: &Outgoing) -> Result<(), NetError> {
    let raw = outgoing_mime(m);
    if raw.len() <= MAX_ONE_REQUEST {
        return c.send_mime(&raw);
    }
    let draft = create(c, None, &raw)?;
    let sent = c.send_draft(&draft.id);
    if sent.is_err() {
        // No half-made draft is left behind: the next try makes a new one.
        let _ = c.delete_message(&draft.id);
    }
    sent
}

/// The folders of `list` below (and including) the service's Outbox: they stay out of MailCraft (it keeps
/// its own Outbox).
fn outbox_tree(list: &[MailFolder]) -> BTreeSet<String> {
    let mut out: BTreeSet<String> =
        list.iter().filter(|f| f.role == Some(FolderRole::Outbox)).map(|f| f.id.clone()).collect();
    loop {
        let more: Vec<String> = list
            .iter()
            .filter(|f| !out.contains(&f.id) && f.parent_folder_id.as_ref().is_some_and(|p| out.contains(p)))
            .map(|f| f.id.clone())
            .collect();
        if more.is_empty() {
            return out;
        }
        out.extend(more);
    }
}

pub(crate) fn run(
    account: AccountId,
    conn: &GraphConn,
    job: Option<&GraphJob>,
    outgoing: &[Outgoing],
    events: &Events,
    cancel: &Arc<AtomicBool>,
) -> GraphEnd {
    let progress = |t: &str| {
        events.send(Event::step(account, t));
    };
    let cancelled = || cancel.load(Ordering::SeqCst);
    progress("Connecting…");
    let c = match conn.connect(Some(cancel)) {
        Ok(c) => c,
        Err(e) => return GraphEnd { refused: matches!(e, NetError::Auth(_)), error: Some(e.to_string()) },
    };
    // The sign-in first, before anything changes: a refused token can be renewed and the run repeated.
    if let Err(e) = c.get_json("/me/mailFolders/inbox?$select=id") {
        return GraphEnd { refused: matches!(e, NetError::Auth(_)), error: Some(e.to_string()) };
    }
    let res: Result<(), NetError> = (|| {
        let mut fresh = BTreeSet::new();
        if let Some(j) = job {
            fresh = local_changes(&c, j, account, events, cancel)?;
        }
        if !outgoing.is_empty() {
            let n = outgoing.len();
            for (i, m) in outgoing.iter().enumerate() {
                if cancelled() {
                    break;
                }
                progress(&format!("Sending message {} of {n}…", i.saturating_add(1)));
                let result = send_one(&c, m).map_err(|e| e.to_string());
                events.send(Event::Sent { account, item: m.item, result });
            }
        }
        let Some(j) = job else { return Ok(()) };
        if j.receive && !cancelled() {
            receive(&c, j, &fresh, account, events, cancel)?;
        }
        if !j.fetch.is_empty() && !cancelled() {
            progress("Downloading messages…");
            let mut list = Vec::new();
            for (_, id) in &j.fetch {
                if cancelled() {
                    break;
                }
                match c.message_mime(id) {
                    Ok(raw) => list.push((id.clone(), raw)),
                    Err(e) if not_found(&e) => {}
                    Err(e) => return Err(e),
                }
            }
            send(events, cancel, account, GraphEvent::Bodies { list });
        }
        Ok(())
    })();
    let error = match res {
        _ if cancelled() => Some("Send/Receive was cancelled.".into()),
        Ok(()) => None,
        Err(e) => Some(e.to_string()),
    };
    GraphEnd { error, refused: false }
}

/// Sends the job's local changes. Returns the ids of the messages they made on the service (uploads, moved
/// messages under a new id), which this run's delta queries then list as known ones.
fn local_changes(
    c: &GraphClient,
    j: &GraphJob,
    account: AccountId,
    events: &Events,
    cancel: &Arc<AtomicBool>,
) -> Result<BTreeSet<String>, NetError> {
    let cancelled = || cancel.load(Ordering::SeqCst);
    let mut fresh: BTreeSet<String> = BTreeSet::new();
    let mut created: BTreeMap<String, String> = BTreeMap::new();
    for (folder, name, parent) in &j.folder_creates {
        if cancelled() {
            return Ok(fresh);
        }
        let parent = match parent {
            Some(p) => match resolve(p, &created) {
                Some(id) => Some(id),
                None => continue,
            },
            None => None,
        };
        let made = match c.create_folder(parent.as_deref(), name) {
            Ok(f) => Ok(f.id),
            // Already there (made elsewhere meanwhile): adopt it.
            Err(NetError::Graph { status: 409, .. }) => {
                let path = match &parent {
                    Some(p) => {
                        format!("/me/mailFolders/{}/childFolders?$top=100", mailcraft_net::graph::seg(p))
                    }
                    None => "/me/mailFolders?$top=100".to_owned(),
                };
                c.get_all(&path, mailcraft_net::graph::mail::MAX_FOLDERS)?
                    .iter()
                    .filter_map(MailFolder::from_json)
                    .find(|f| f.display_name.eq_ignore_ascii_case(name))
                    .map(|f| f.id)
                    .ok_or_else(|| "a folder of that name exists but could not be found".to_owned())
            }
            Err(e @ (NetError::Graph { .. } | NetError::Http { .. })) => Err(e.to_string()),
            Err(e) => return Err(e),
        };
        match made {
            Ok(id) => {
                created.insert(placeholder(*folder), id.clone());
                let shadow = GraphFolderShadow { name: name.clone(), parent: parent.clone() };
                send(events, cancel, account, GraphEvent::FolderCreated { folder: *folder, id, shadow });
            }
            Err(why) => {
                log::warn!("graph: folder {name:?} not created: {why}");
                events.send(Event::FolderFailed {
                    account,
                    name: name.clone(),
                    error: format!("the service did not create this folder ({why})"),
                });
            }
        }
    }
    for (id, rename, moved, shadow) in &j.folder_updates {
        if cancelled() {
            return Ok(fresh);
        }
        let mut shadow = shadow.clone();
        let mut now_id = id.clone();
        if let Some(n) = rename {
            match c.rename_folder(id, n) {
                Ok(()) => {}
                Err(e) if not_found(&e) => continue,
                Err(e @ (NetError::Graph { .. } | NetError::Http { .. })) => {
                    log::warn!("graph: folder {id} not renamed: {e}");
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        if let Some(parent) = moved {
            let dest = match parent {
                Some(p) => match resolve(p, &created) {
                    Some(d) => d,
                    None => continue,
                },
                None => ROOT.to_owned(),
            };
            shadow.parent = parent.as_ref().and_then(|p| resolve(p, &created));
            match c.move_folder(id, &dest) {
                Ok(f) => now_id = f.id,
                Err(e) if not_found(&e) => continue,
                Err(e @ (NetError::Graph { .. } | NetError::Http { .. })) => {
                    log::warn!("graph: folder {id} not moved: {e}");
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        send(events, cancel, account, GraphEvent::FolderUpdated { old: id.clone(), id: now_id, shadow });
    }
    if !j.updates.is_empty() {
        events.send(Event::step(account, "Updating the server…"));
    }
    for (id, patch, shadow) in &j.updates {
        if cancelled() {
            return Ok(fresh);
        }
        match c.update_message(id, patch) {
            Ok(()) => {
                send(events, cancel, account, GraphEvent::Updated { id: id.clone(), shadow: shadow.clone() })
            }
            Err(e) if not_found(&e) => {
                send(events, cancel, account, GraphEvent::Destroyed { ids: vec![id.clone()] })
            }
            Err(e @ (NetError::Graph { .. } | NetError::Http { .. })) => {
                log::warn!("graph: update of {id} refused: {e}")
            }
            Err(e) => return Err(e),
        }
    }
    for (id, dest, shadow) in &j.moves {
        if cancelled() {
            return Ok(fresh);
        }
        // A move into a folder the service could not make waits for the next run.
        let Some(dest) = resolve(dest, &created) else { continue };
        match c.move_message(id, &dest) {
            Ok(new) => {
                fresh.insert(new.clone());
                let shadow = GraphShadow { folder: dest, ..shadow.clone() };
                send(events, cancel, account, GraphEvent::Moved { old: id.clone(), new, shadow });
            }
            Err(e) if not_found(&e) => {
                send(events, cancel, account, GraphEvent::Destroyed { ids: vec![id.clone()] })
            }
            Err(e @ (NetError::Graph { .. } | NetError::Http { .. })) => {
                log::warn!("graph: move of {id} refused: {e}")
            }
            Err(e) => return Err(e),
        }
    }
    if !j.destroy.is_empty() {
        let mut gone = Vec::new();
        for id in &j.destroy {
            if cancelled() {
                break;
            }
            match c.delete_message(id) {
                Ok(()) => gone.push(id.clone()),
                Err(e @ (NetError::Graph { .. } | NetError::Http { .. })) => {
                    log::warn!("graph: delete of {id} refused: {e}")
                }
                Err(e) => {
                    send(events, cancel, account, GraphEvent::Destroyed { ids: gone });
                    return Err(e);
                }
            }
        }
        send(events, cancel, account, GraphEvent::Destroyed { ids: gone });
    }
    for (item, folder, raw, shadow) in &j.imports {
        if cancelled() {
            return Ok(fresh);
        }
        let Some(folder) = resolve(folder, &created) else { continue };
        events.send(Event::step(account, "Uploading…"));
        let made = match create(c, Some(&folder), raw) {
            Ok(m) => m,
            Err(e @ (NetError::Graph { .. } | NetError::Http { .. } | NetError::TooLarge(_))) => {
                log::warn!("graph: upload refused: {e}");
                continue;
            }
            Err(e) => return Err(e),
        };
        fresh.insert(made.id.clone());
        // The service files uploads as drafts and unread: give it the item's state.
        let was = shadow_from(&made, &folder);
        let mut patch = MessagePatch::default();
        if was.read != shadow.read {
            patch.is_read = Some(shadow.read);
        }
        if was.flag != shadow.flag || was.start != shadow.start || was.due != shadow.due {
            patch.flag = Some((FlagStatus::parse(&shadow.flag), shadow.start.clone(), shadow.due.clone()));
        }
        if was.categories != shadow.categories {
            patch.categories = Some(shadow.categories.clone());
        }
        if was.importance != shadow.importance {
            patch.importance = Some(shadow.importance.clone());
        }
        let shadow = match c.update_message(&made.id, &patch) {
            Ok(()) => GraphShadow { folder, ..shadow.clone() },
            Err(e) => {
                log::warn!("graph: the uploaded message's state was not set: {e}");
                was
            }
        };
        send(events, cancel, account, GraphEvent::Imported { item: *item, id: made.id, shadow });
    }
    if !j.folder_destroys.is_empty() && !cancelled() {
        let mut gone = Vec::new();
        for id in &j.folder_destroys {
            match c.delete_folder(id) {
                Ok(()) => gone.push(id.clone()),
                Err(e) if not_found(&e) => gone.push(id.clone()),
                Err(e @ (NetError::Graph { .. } | NetError::Http { .. })) => {
                    log::warn!("graph: folder {id} not deleted: {e}")
                }
                Err(e) => return Err(e),
            }
        }
        send(events, cancel, account, GraphEvent::FoldersDestroyed { ids: gone });
    }
    Ok(fresh)
}

/// Reads the folders' changes. `fresh`: messages this run's local changes made on the service.
fn receive(
    c: &GraphClient,
    j: &GraphJob,
    fresh: &BTreeSet<String>,
    account: AccountId,
    events: &Events,
    cancel: &Arc<AtomicBool>,
) -> Result<(), NetError> {
    let cancelled = || cancel.load(Ordering::SeqCst);
    // Every folder of the service is read this run (else a message removed from one folder may sit in
    // another one).
    let mut every_folder = j.only.is_none();
    let folders: Vec<MailFolder> = match &j.only {
        // Some folders only (the Inbox polled, a folder's Send/Receive): the folder list is not read again.
        Some(only) if j.listed => only
            .iter()
            .map(|(id, name)| MailFolder { id: id.clone(), display_name: name.clone(), ..Default::default() })
            .collect(),
        _ => {
            events.send(Event::step(account, "Updating the folder list…"));
            // A cancelled listing is an error: a part of the tree never passes for all of it.
            let (list, complete) = c.mail_folder_tree()?;
            if cancelled() {
                return Ok(());
            }
            every_folder &= complete;
            let skip = outbox_tree(&list);
            let folders = list
                .iter()
                .filter(|f| !skip.contains(&f.id))
                .filter(|f| j.only.as_ref().is_none_or(|o| o.contains_key(&f.id)))
                .cloned()
                .collect();
            send(events, cancel, account, GraphEvent::Folders { list, complete });
            folders
        }
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut removed: Vec<String> = Vec::new();
    let mut later: Vec<String> = Vec::new();
    let mut known = j.known.clone();
    known.extend(fresh.iter().cloned());
    for f in &folders {
        if cancelled() {
            return Ok(());
        }
        let name = if f.display_name.trim().is_empty() { "a folder" } else { f.display_name.as_str() };
        events.send(Event::step(account, format!("Updating {name}…")));
        let mut link = j.delta.get(&f.id).cloned();
        let mut restarted = false;
        let mut pages = 0usize;
        loop {
            if cancelled() {
                return Ok(());
            }
            let after = if link.is_none() { j.received_after.as_deref() } else { None };
            let page = match c.messages_delta(&f.id, link.as_deref(), after) {
                Ok(p) => p,
                // The service no longer knows the saved state: start the folder over (once).
                Err(e) if lost_state(&e) && !restarted => {
                    restarted = true;
                    link = None;
                    send(events, cancel, account, GraphEvent::DeltaReset { folder: f.id.clone() });
                    continue;
                }
                Err(e) => return Err(e),
            };
            pages = pages.saturating_add(1);
            let mut out = Vec::new();
            for m in page.messages {
                if m.removed {
                    removed.push(m.id);
                    continue;
                }
                seen.insert(m.id.clone());
                if known.contains(&m.id) {
                    out.push(GMsg { meta: m, raw: None, headers_only: false });
                    continue;
                }
                let g = if j.headers_only {
                    if j.then_full {
                        later.push(m.id.clone());
                    }
                    GMsg { raw: Some(m.header_block()), headers_only: true, meta: m }
                } else {
                    match c.message_mime(&m.id) {
                        Ok(raw) => GMsg { meta: m, raw: Some(raw), headers_only: false },
                        Err(e) if not_found(&e) => continue,
                        Err(e) => return Err(e),
                    }
                };
                known.insert(g.meta.id.clone());
                out.push(g);
                if cancelled() {
                    break;
                }
            }
            if !out.is_empty() {
                send(events, cancel, account, GraphEvent::Messages { folder: f.id.clone(), list: out });
            }
            if let Some(d) = page.delta {
                send(events, cancel, account, GraphEvent::DeltaLink { folder: f.id.clone(), link: d });
                break;
            }
            match page.next {
                // A very large folder continues next time from where this run stopped.
                Some(n) if pages >= MAX_PAGES => {
                    send(events, cancel, account, GraphEvent::DeltaLink { folder: f.id.clone(), link: n });
                    break;
                }
                Some(n) => link = Some(n),
                None => break,
            }
        }
    }
    // Removed from a folder and listed in none: gone, when every folder was read. Otherwise the service is
    // asked where each one is now (a message moved to a folder this run did not read keeps its item).
    let unmatched: BTreeSet<String> = removed.into_iter().filter(|id| !seen.contains(id)).collect();
    let mut gone: Vec<String> = Vec::new();
    if every_folder {
        gone.extend(unmatched);
    } else {
        let mut elsewhere: BTreeMap<String, Vec<GMsg>> = BTreeMap::new();
        for id in unmatched.into_iter().filter(|id| known.contains(id)) {
            if cancelled() {
                return Ok(());
            }
            match c.message(&id) {
                Ok(m) => {
                    if let Some(folder) = m.folder.clone() {
                        elsewhere.entry(folder).or_default().push(GMsg {
                            meta: m,
                            raw: None,
                            headers_only: false,
                        });
                    }
                }
                Err(e) if not_found(&e) => gone.push(id),
                // Not known for sure: the item stays.
                Err(e @ (NetError::Graph { .. } | NetError::Http { .. })) => {
                    log::warn!("graph: where message {id} went is unknown: {e}")
                }
                Err(e) => return Err(e),
            }
        }
        for (folder, list) in elsewhere {
            send(events, cancel, account, GraphEvent::Messages { folder, list });
        }
    }
    if !gone.is_empty() {
        send(events, cancel, account, GraphEvent::Gone { ids: gone });
    }
    if !later.is_empty() && !cancelled() {
        events.send(Event::step(account, "Downloading messages…"));
        let mut list = Vec::new();
        for id in later {
            if cancelled() {
                break;
            }
            match c.message_mime(&id) {
                Ok(raw) => list.push((id, raw)),
                Err(e) if not_found(&e) => {}
                Err(e) => return Err(e),
            }
        }
        send(events, cancel, account, GraphEvent::Bodies { list });
    }
    Ok(())
}

/// Test Account Settings: signs in and reads the Inbox.
pub(crate) fn test(conn: &GraphConn) -> Result<String, NetError> {
    let c = conn.connect(None)?;
    c.get_json("/me/mailFolders/inbox?$select=id")?;
    Ok(format!("Signed in to Microsoft Graph at {}.", conn.base()))
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

/// The service's flag (start, due) of `g`, as [`flag_dates`] gives them for an item.
fn service_dates(g: &GraphMessage) -> (Option<String>, Option<String>) {
    if g.flag != FlagStatus::Flagged {
        return (None, None);
    }
    both(shadow_time(g.flag_start.as_deref()), shadow_time(g.flag_due.as_deref()))
}

/// Puts the service's state of a message on its item (the service wins). Whether it is a draft is not
/// taken: the service files every message uploaded from here as a draft, and a draft stays one.
fn apply_state(m: &mut MailItem, g: &GraphMessage) {
    m.read = g.is_read;
    let start = || g.flag_start.as_deref().and_then(graph_time);
    let due = || g.flag_due.as_deref().and_then(graph_time);
    match g.flag {
        FlagStatus::Flagged if !m.is_flagged() => {
            m.flag =
                FlagState::Flagged { label: "Follow up".into(), start: start(), due: due(), reminder: None };
        }
        // Flagged on both sides: new days set on the service come here.
        FlagStatus::Flagged if flag_dates(m) != service_dates(g) => {
            if let FlagState::Flagged { start: s, due: d, .. } = &mut m.flag {
                *s = start();
                *d = due();
            }
        }
        FlagStatus::Complete if !matches!(m.flag, FlagState::Complete { .. }) => {
            m.flag = FlagState::Complete { completed: None };
        }
        FlagStatus::NotFlagged => m.flag = FlagState::None,
        _ => {}
    }
    if sorted(&m.categories) != sorted(&g.categories) {
        m.categories.clone_from(&g.categories);
    }
    m.importance = match g.importance.as_str() {
        "high" => Importance::High,
        "low" => Importance::Low,
        _ => Importance::Normal,
    };
}

fn shadow_from(g: &GraphMessage, folder: &str) -> GraphShadow {
    let (start, due) = service_dates(g);
    GraphShadow {
        folder: folder.to_owned(),
        read: g.is_read,
        flag: g.flag.name().to_owned(),
        start,
        due,
        categories: sorted(&g.categories),
        importance: g.importance.clone(),
    }
}

/// The service's folder list. Folders it no longer has go here only when `complete` (a list cut short by
/// the caps says nothing of the folders it left out).
fn apply_folders(s: &mut Session, account: AccountId, list: Vec<MailFolder>, complete: bool) {
    let skip = outbox_tree(&list);
    let list: Vec<MailFolder> = list.into_iter().filter(|f| !skip.contains(&f.id)).collect();
    let ids: BTreeSet<String> = list.iter().map(|f| f.id.clone()).collect();
    let known = prefs(s).graph.get(&account.0).map(|st| st.folders.clone()).unwrap_or_default();
    let parent_of = |f: &MailFolder| f.parent_folder_id.clone().filter(|p| ids.contains(p));
    for f in &list {
        let display = local_folder_name(&f.display_name);
        let parent_id = parent_of(f);
        let parent = parent_id.as_deref().and_then(|p| folder_of(s, account, p));
        if let Some(fid) = folder_of(s, account, &f.id) {
            // The service's name and place win when they changed there (a change made here and not sent yet
            // stays).
            let changed_there = known.get(&f.id).is_none_or(|k| k.name != display || k.parent != parent_id);
            if let Some(lf) = s.mailbox.folders.get_mut(&fid)
                && lf.role.is_none()
                && changed_there
                && (lf.name != display || lf.parent != parent)
            {
                lf.name = display;
                lf.parent = parent;
            }
            continue;
        }
        if let Some(role) = f.role
            && let Some(fid) = s.mailbox.role_folder(account, role)
            && s.mailbox.folder(fid).is_some_and(|x| x.remote_name.is_none())
        {
            if let Some(lf) = s.mailbox.folders.get_mut(&fid) {
                lf.remote_name = Some(f.id.clone());
            }
            continue;
        }
        // (A second folder for a role this account already maps is shown as a plain folder.)
        // A folder made here under the same name and parent (its creation crossed this listing): adopt it.
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
            if let Some(lf) = s.mailbox.folders.get_mut(&fid) {
                lf.remote_name = Some(f.id.clone());
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
    let gone: Vec<(FolderId, bool)> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.account == account && f.kind == FolderKind::Mail)
        .filter(|f| complete && f.remote_name.as_ref().is_some_and(|n| !ids.contains(n)))
        .map(|f| (f.id, f.role.is_some()))
        .collect();
    let mut remove = Vec::new();
    for (fid, is_role) in gone {
        if let Some(name) = s.mailbox.folder(fid).and_then(|f| f.remote_name.clone()) {
            s.sync.track.mailboxes_gone.insert((account, name.clone()));
            prefs(s).graph.entry(account.0).or_default().delta.remove(&name);
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
        // Their messages went with the folder on the service: their shadows go, so no run deletes them.
        let lost: Vec<String> = s
            .mailbox
            .mail
            .items()
            .filter(|m| sub.contains(&m.folder))
            .filter_map(|m| m.remote_id.clone())
            .collect();
        let items = remove_folders(&mut s.mailbox, &remove);
        let st = prefs(s).graph.entry(account.0).or_default();
        for e in &lost {
            st.shadow.remove(e);
        }
        s.sync.track.removed.extend(items);
        s.sync.track.folders_removed.extend(sub);
    }
    let mut folders: BTreeMap<String, GraphFolderShadow> = list
        .iter()
        .filter(|f| f.role.is_none())
        .map(|f| {
            (
                f.id.clone(),
                GraphFolderShadow { name: local_folder_name(&f.display_name), parent: parent_of(f) },
            )
        })
        .collect();
    if !complete {
        // The folders left out of the list keep what was known of them.
        for (id, sh) in known {
            folders.entry(id).or_insert(sh);
        }
    }
    let st = prefs(s).graph.entry(account.0).or_default();
    st.folders = folders;
    st.listed = true;
}

fn apply_messages(
    s: &mut Session,
    account: AccountId,
    folder: &str,
    list: Vec<GMsg>,
    new_inbox: &mut Vec<ItemId>,
) {
    let Some(local) = folder_of(s, account, folder) else { return };
    let now = s.now();
    let inbox = s.mailbox.role_folder(account, FolderRole::Inbox);
    for g in list {
        let shadow = shadow_from(&g.meta, folder);
        match item_of(s, account, &g.meta.id) {
            Some(id) => {
                // An item put in a folder of the account that stays here (below a default folder the service
                // does not have) stays there: the service keeps its copy where it was.
                let stays_here =
                    s.mailbox.mail.get(id).and_then(|m| s.mailbox.folder(m.folder)).is_some_and(|f| {
                        f.remote_name.is_none() && keeps_server_copy(&s.mailbox, account, f.id)
                    });
                if let Some(m) = s.mailbox.mail.get_mut(id) {
                    if m.folder != local && !stays_here {
                        m.folder = local;
                    }
                    apply_state(m, &g.meta);
                    s.sync.track.flags.insert(id);
                }
            }
            None => {
                let Some(raw) = &g.raw else { continue };
                let mut item = mailcraft_store::import::item_from_raw(raw, local, now);
                // A local copy waiting for this message (sent or uploaded here): adopt it.
                let adopt = item.message_id.as_deref().and_then(|mid| local_copy(&s.mailbox, local, mid));
                if let Some(l) = adopt {
                    if let Some(m) = s.mailbox.mail.get_mut(l) {
                        m.remote_id = Some(g.meta.id.clone());
                    }
                } else {
                    item.remote_id = Some(g.meta.id.clone());
                    apply_state(&mut item, &g.meta);
                    item.is_draft = g.meta.is_draft;
                    if let Some(at) = g.meta.received_at() {
                        item.received = at;
                    }
                    if g.meta.focused == Some(false) {
                        item.focus = FocusClass::Other;
                    }
                    let header_only = if g.headers_only {
                        item.blob = None;
                        item.body_key = None;
                        item.preview = g.meta.preview.chars().take(255).collect();
                        true
                    } else if let Err(err) = s.store_source(&mut item, raw) {
                        // Not taken in (nor remembered as seen), and the folder's delta link is not moved on:
                        // the next run lists it again and downloads it.
                        log::error!(
                            "could not store a downloaded message ({err}); it is downloaded again next time"
                        );
                        prefs(s).graph.entry(account.0).or_default().hold.insert(folder.to_owned());
                        continue;
                    } else {
                        false
                    };
                    let id = s.mailbox.add_mail(item);
                    if header_only {
                        prefs(s).headers_only.insert(id.0);
                    }
                    s.sync.track.created.insert(id);
                    if Some(local) == inbox {
                        new_inbox.push(id);
                    }
                }
            }
        }
        prefs(s).graph.entry(account.0).or_default().shadow.insert(g.meta.id, shadow);
    }
}

pub(crate) fn apply(s: &mut Session, account: AccountId, ev: GraphEvent, new_inbox: &mut Vec<ItemId>) {
    match ev {
        GraphEvent::Folders { list, complete } => apply_folders(s, account, list, complete),
        GraphEvent::Messages { folder, list } => apply_messages(s, account, &folder, list, new_inbox),
        GraphEvent::Gone { ids } => {
            for id in &ids {
                if let Some(item) = item_of(s, account, id) {
                    s.mailbox.mail.remove(item);
                    s.sync.track.removed.insert(item);
                }
            }
            let st = prefs(s).graph.entry(account.0).or_default();
            for id in ids {
                st.shadow.remove(&id);
            }
        }
        GraphEvent::DeltaLink { folder, link } => {
            let st = prefs(s).graph.entry(account.0).or_default();
            // A message of this run's pages could not be stored: the next run reads them again.
            if !st.hold.remove(&folder) {
                st.delta.insert(folder, link);
            }
        }
        GraphEvent::DeltaReset { folder } => {
            prefs(s).graph.entry(account.0).or_default().delta.remove(&folder);
        }
        GraphEvent::Updated { id, shadow } => {
            prefs(s).graph.entry(account.0).or_default().shadow.insert(id, shadow);
        }
        GraphEvent::Moved { old, new, shadow } => {
            if old != new
                && let Some(item) = item_of(s, account, &old)
                && let Some(m) = s.mailbox.mail.get_mut(item)
            {
                m.remote_id = Some(new.clone());
            }
            let st = prefs(s).graph.entry(account.0).or_default();
            st.shadow.remove(&old);
            st.shadow.insert(new, shadow);
        }
        GraphEvent::Destroyed { ids } => {
            let st = prefs(s).graph.entry(account.0).or_default();
            for id in ids {
                st.shadow.remove(&id);
            }
        }
        GraphEvent::Imported { item, id, shadow } => {
            if let Some(m) = s.mailbox.mail.get_mut(item) {
                m.remote_id = Some(id.clone());
                prefs(s).graph.entry(account.0).or_default().shadow.insert(id, shadow);
            }
        }
        GraphEvent::FolderCreated { folder, id, shadow } => {
            if let Some(f) = s.mailbox.folders.get_mut(&folder)
                && f.account == account
                && f.remote_name.is_none()
            {
                f.remote_name = Some(id.clone());
            }
            prefs(s).graph.entry(account.0).or_default().folders.insert(id, shadow);
        }
        GraphEvent::FolderUpdated { old, id, shadow } => {
            if old != id
                && let Some(fid) = folder_of(s, account, &old)
                && let Some(f) = s.mailbox.folders.get_mut(&fid)
            {
                f.remote_name = Some(id.clone());
            }
            let st = prefs(s).graph.entry(account.0).or_default();
            st.folders.remove(&old);
            if old != id {
                st.delta.remove(&old);
            }
            st.folders.insert(id, shadow);
        }
        GraphEvent::FoldersDestroyed { ids } => {
            let st = prefs(s).graph.entry(account.0).or_default();
            for id in &ids {
                st.folders.remove(id);
                st.delta.remove(id);
            }
            s.sync.track.mailboxes_gone.extend(ids.into_iter().map(|id| (account, id)));
        }
        GraphEvent::Bodies { list } => {
            for (id, raw) in list {
                if let Some(item) = item_of(s, account, &id)
                    && super::cmds::is_header_only(s, item)
                {
                    super::native::fill_item(s, item, &raw);
                }
            }
        }
    }
}

/// After a command: true when the change record changed mail of the Graph account `account` in a way the
/// service should learn (folder, read, flag, categories, importance, new or removed items; mail folders made,
/// renamed, moved or removed).
pub(crate) fn changed(before: &Mailbox, mail: &MailChanges, after: &Mailbox, account: AccountId) -> bool {
    let mail_folder = |f: &&Folder| f.account == account && f.kind == FolderKind::Mail;
    let folders = |mb: &Mailbox| -> BTreeMap<FolderId, (String, Option<FolderId>)> {
        mb.folders.values().filter(mail_folder).map(|f| (f.id, (f.name.clone(), f.parent))).collect()
    };
    if folders(before) != folders(after) {
        return true;
    }
    let in_acct = |mb: &Mailbox, m: &MailItem| mb.folder(m.folder).is_some_and(|f| f.account == account);
    let same = |a: &MailItem, b: &MailItem| {
        a.folder == b.folder
            && a.read == b.read
            && flag_status(a) == flag_status(b)
            && a.flag == b.flag
            && sorted(&a.categories) == sorted(&b.categories)
            && a.importance == b.importance
    };
    mail.iter().any(|(id, b)| match (b, after.mail.get(id)) {
        (Some(b), a) => in_acct(before, b) && a.is_none_or(|a| !same(a, b)),
        (None, Some(a)) => in_acct(after, a),
        (None, None) => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shadows_and_patches() {
        let mut m = MailItem { read: true, importance: Importance::High, ..Default::default() };
        m.categories = vec!["Red category".into(), " Blue category ".into(), "Red category".into()];
        let a = shadow_of(&m, "F1");
        assert_eq!(a.categories, vec!["Blue category".to_owned(), "Red category".to_owned()]);
        assert_eq!((a.flag.as_str(), a.importance.as_str()), ("notFlagged", "high"));
        let due = mailcraft_model::DateTime::parse_rfc5322("Fri, 9 Oct 2026 17:00:00 +0000").unwrap();
        m.flag =
            FlagState::Flagged { label: "Follow up".into(), start: None, due: Some(due), reminder: None };
        m.read = false;
        let b = shadow_of(&m, "F1");
        let p = patch_for(&a, &b, &m);
        let v = p.to_json();
        assert_eq!(v["isRead"], false);
        assert_eq!(v["flag"]["dueDateTime"]["dateTime"], "2026-10-09T17:00:00");
        assert!(v.get("categories").is_none() && v.get("importance").is_none(), "{v}");
        assert!(patch_for(&b, &b, &m).is_empty());
    }

    #[test]
    fn service_state_lands_on_the_item() {
        let g = GraphMessage::from_json(&json!({"id": "x", "isRead": true, "importance": "low",
            "flag": {"flagStatus": "complete"}, "categories": ["Green category"]}))
        .unwrap();
        let mut m = MailItem::default();
        apply_state(&mut m, &g);
        assert!(m.read && matches!(m.flag, FlagState::Complete { .. }) && m.importance == Importance::Low);
        assert_eq!(m.categories, vec!["Green category".to_owned()]);
        let s = shadow_from(&g, "F");
        assert_eq!(s, shadow_of(&m, "F"), "a message just received needs no write-back");
    }

    #[test]
    fn envelope_recipients_missing_from_the_header_become_bcc() {
        let raw = b"From: a@example.com\r\nTo: b@example.org\r\nX-MailCraft-Submit-Notify: x\r\nSubject: s\r\n\r\nhi\r\n";
        let m = Outgoing {
            item: ItemId(1),
            from: "a@example.com".into(),
            rcpts: vec!["b@example.org".into(), "hidden@example.net".into()],
            raw: raw.to_vec(),
        };
        let out = String::from_utf8(outgoing_mime(&m)).unwrap();
        assert!(out.starts_with("Bcc: hidden@example.net\r\n"), "{out}");
        assert!(!out.contains("X-MailCraft-Submit"), "{out}");
    }

    #[test]
    fn large_files_are_split_off() {
        let big = "QUFB".repeat(100_000);
        let raw = format!(
            "From: a@example.com\r\nTo: b@example.org\r\nSubject: s\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n--b1\r\nContent-Type: text/plain\r\n\r\nhello\r\n\
             --b1\r\nContent-Type: application/octet-stream; name=\"big.bin\"\r\n\
             Content-Disposition: attachment; filename=\"big.bin\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{big}\r\n--b1--\r\n"
        );
        let (small, files) = split_large(raw.as_bytes()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "big.bin");
        assert_eq!(files[0].data.len(), 300_000);
        assert!(small.len() < 10_000 && String::from_utf8_lossy(&small).contains("hello"));
        assert!(split_large(b"From: a@example.com\r\n\r\nno files\r\n").is_none());
    }

    /// A session with a Graph account (never connected) and its default folders; Inbox mapped to `F-in`.
    fn graph_session() -> (Session, AccountId) {
        let mut s = Session::new(Mailbox::new_local("Local Folders"));
        let a = s.mailbox.add_account(Account {
            kind: mailcraft_model::AccountKind::Graph,
            address: "alice@example.com".into(),
            display_name: "alice@example.com".into(),
            ..Default::default()
        });
        s.mailbox.create_default_folders(a);
        let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
        s.mailbox.folders.get_mut(&inbox).unwrap().remote_name = Some("F-in".into());
        (s, a)
    }

    fn add_folder(
        s: &mut Session,
        a: AccountId,
        parent: Option<FolderId>,
        name: &str,
        remote: Option<&str>,
    ) -> FolderId {
        let id = FolderId(s.mailbox.alloc_id());
        s.mailbox.folders.insert(
            id,
            Folder {
                id,
                account: a,
                parent,
                name: name.into(),
                kind: FolderKind::Mail,
                remote_name: remote.map(str::to_owned),
                ..Default::default()
            },
        );
        id
    }

    /// Regression: a folder list cut short (by the caps) removed every folder it left out, with its mail.
    #[test]
    fn an_incomplete_folder_list_removes_nothing() {
        let (mut s, a) = graph_session();
        let projects = add_folder(&mut s, a, None, "Projects", Some("F-proj"));
        s.mailbox.add_mail(MailItem { folder: projects, subject: "Plans".into(), ..Default::default() });
        let inbox = MailFolder { id: "F-in".into(), role: Some(FolderRole::Inbox), ..Default::default() };
        let mut ev = Vec::new();
        apply(&mut s, a, GraphEvent::Folders { list: vec![inbox.clone()], complete: false }, &mut ev);
        assert!(s.mailbox.folder(projects).is_some(), "kept: the list was not the whole tree");
        assert_eq!(s.mailbox.items_in(projects).len(), 1);
        apply(&mut s, a, GraphEvent::Folders { list: vec![inbox], complete: true }, &mut ev);
        assert!(s.mailbox.folder(projects).is_none(), "the whole tree without it: gone on the service");
    }

    /// Regression: a message that could not be stored was skipped, but the folder's delta link moved past it,
    /// so it was never listed again.
    #[test]
    fn a_message_not_stored_keeps_the_folder_delta_link() {
        let (mut s, a) = graph_session();
        prefs(&mut s).graph.entry(a.0).or_default().hold.insert("F-in".into());
        let mut ev = Vec::new();
        let link = |l: &str| GraphEvent::DeltaLink { folder: "F-in".into(), link: l.into() };
        apply(&mut s, a, link("L1"), &mut ev);
        let st = prefs(&mut s).graph.get(&a.0).cloned().unwrap();
        assert!(!st.delta.contains_key("F-in") && st.hold.is_empty(), "{st:?}");
        apply(&mut s, a, link("L2"), &mut ev);
        assert_eq!(prefs(&mut s).graph[&a.0].delta.get("F-in").map(String::as_str), Some("L2"));
    }

    /// Regression: a 410 without a Graph error body did not restart the folder, so every run failed.
    #[test]
    fn lost_sync_state_with_or_without_a_graph_body() {
        assert!(lost_state(&NetError::Http { status: 410, body: String::new() }));
        let g = NetError::Graph { status: 410, code: "SyncStateNotFound".into(), message: String::new() };
        assert!(lost_state(&g));
        assert!(!lost_state(&NetError::Http { status: 404, body: String::new() }));
    }

    /// Regression: new start or due days of a message already flagged were never sent, nor taken in.
    #[test]
    fn flag_days_of_a_flagged_message_go_both_ways() {
        let day = |d: &str| mailcraft_model::DateTime::parse_rfc5322(d).unwrap();
        let mut m = MailItem {
            flag: FlagState::Flagged {
                label: "Follow up".into(),
                start: None,
                due: Some(day("Fri, 9 Oct 2026 17:00:00 +0000")),
                reminder: None,
            },
            ..Default::default()
        };
        let was = shadow_of(&m, "F");
        assert_eq!(was.start.as_deref(), Some("2026-10-09T17:00:00"), "start defaults to the due day");
        if let FlagState::Flagged { due, .. } = &mut m.flag {
            *due = Some(day("Tue, 13 Oct 2026 17:00:00 +0000"));
        }
        let now = shadow_of(&m, "F");
        let v = patch_for(&was, &now, &m).to_json();
        assert_eq!(v["flag"]["dueDateTime"]["dateTime"], "2026-10-13T17:00:00", "{v}");
        // Days set on the service reach the item flagged here.
        let g = GraphMessage::from_json(&json!({"id": "x", "flag": {"flagStatus": "flagged",
            "startDateTime": {"dateTime": "2026-10-20T08:00:00.0000000", "timeZone": "UTC"},
            "dueDateTime": {"dateTime": "2026-10-21T17:00:00.0000000", "timeZone": "UTC"}}}))
        .unwrap();
        apply_state(&mut m, &g);
        let FlagState::Flagged { start, due, label, .. } = &m.flag else { panic!("{:?}", m.flag) };
        assert_eq!(label, "Follow up");
        assert_eq!(start.map(|d| utc(&d)).as_deref(), Some("2026-10-20T08:00:00"));
        assert_eq!(due.map(|d| utc(&d)).as_deref(), Some("2026-10-21T17:00:00"));
        assert_eq!(shadow_from(&g, "F"), shadow_of(&m, "F"), "nothing to write back");
        // Flagged without days now: the service's days are cleared.
        let was = shadow_of(&m, "F");
        m.flag = FlagState::Flagged { label: "Follow up".into(), start: None, due: None, reminder: None };
        let v = patch_for(&was, &shadow_of(&m, "F"), &m).to_json();
        assert!(v["flag"]["dueDateTime"].is_null() && v["flag"].get("dueDateTime").is_some(), "{v}");
    }

    /// Regression: the service files every upload as a draft, and received mail moved into the account
    /// turned into drafts here.
    #[test]
    fn the_service_draft_state_never_turns_mail_into_drafts() {
        let g = GraphMessage::from_json(&json!({"id": "x", "isDraft": true, "isRead": true})).unwrap();
        let mut m = MailItem::default();
        apply_state(&mut m, &g);
        assert!(m.read && !m.is_draft);
    }

    /// Regression: a move into a folder of the account that stays here (below Conversation History) deleted
    /// the service's copy.
    #[test]
    fn moves_inside_the_account_keep_the_service_copy() {
        let (mut s, a) = graph_session();
        let talk = s.mailbox.role_folder(a, FolderRole::ConversationHistory).unwrap();
        let chats = add_folder(&mut s, a, Some(talk), "Chats", None);
        let outbox = s.mailbox.role_folder(a, FolderRole::Outbox).unwrap();
        let queued = add_folder(&mut s, a, Some(outbox), "Queued", None);
        let local =
            s.mailbox.folders.values().find(|f| f.account != a && f.kind == FolderKind::Mail).unwrap().id;
        assert!(keeps_server_copy(&s.mailbox, a, chats));
        assert!(!keeps_server_copy(&s.mailbox, a, queued) && !keeps_server_copy(&s.mailbox, a, outbox));
        assert!(!keeps_server_copy(&s.mailbox, a, local));
        let inbox = s.mailbox.role_folder(a, FolderRole::Inbox).unwrap();
        let st = prefs(&mut s).graph.entry(a.0).or_default();
        st.listed = true;
        st.shadow.insert("m1".into(), GraphShadow { folder: "F-in".into(), ..Default::default() });
        let id = s.mailbox.add_mail(MailItem {
            folder: chats,
            remote_id: Some("m1".into()),
            subject: "Hi".into(),
            ..Default::default()
        });
        let acct = s.mailbox.account(a).cloned().unwrap();
        let job = build_job(&s, &acct, &Plan::default(), None, false, false, &[]);
        assert!(job.is_none_or(|j| j.destroy.is_empty() && j.moves.is_empty()), "the service copy stays");
        // The service lists it again (changed there): it stays where it was put here.
        let g = GMsg {
            meta: GraphMessage::from_json(&json!({"id": "m1", "isRead": true})).unwrap(),
            ..Default::default()
        };
        let mut ev = Vec::new();
        apply(&mut s, a, GraphEvent::Messages { folder: "F-in".into(), list: vec![g] }, &mut ev);
        let m = s.mailbox.mail.get(id).unwrap();
        assert!(m.folder == chats && m.read && m.folder != inbox);
    }

    #[test]
    fn the_service_outbox_stays_out() {
        let f = |id: &str, parent: Option<&str>, role: Option<FolderRole>| MailFolder {
            id: id.into(),
            parent_folder_id: parent.map(str::to_owned),
            role,
            ..Default::default()
        };
        let list = vec![
            f("o", None, Some(FolderRole::Outbox)),
            f("k", Some("o"), None),
            f("i", None, Some(FolderRole::Inbox)),
        ];
        assert_eq!(outbox_tree(&list), ["k".to_owned(), "o".to_owned()].into_iter().collect());
    }
}
