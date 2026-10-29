//! Native side of contacts, tasks and calendar synchronisation (CardDAV / CalDAV) and calendar publishing: the DAV
//! part of a send/receive job is built on the session thread ([`build_job`]) and run on the account's worker
//! thread after its mail ([`run`]); results come back as [`PimEvent`]s (see `pim.rs` for the rules).
//!
//! Resource URLs are compared and requested in one canonical form (scheme, host, port and the normalised
//! path, see [`mailcraft_net::http::normalize_path`]), so a server that writes `%40` where the client wrote `@`
//! does not make an item look deleted, while an escaped `/` or `.` keeps its escape and names exactly the
//! resource the server listed.
//!
//! **JMAP.** A JMAP account keeps its contacts and calendar on its JMAP server when that server offers them
//! ([`pim::via_jmap`]); those folders run through the same [`sync_folder`] against a [`jmap_pim::Store`]
//! (see `jmap_pim.rs`) instead of the WebDAV client ([`PimStore`]).
//!
//! **Connections.** The DAV servers have their own connection settings (trusted certificate, plain-HTTP
//! consent) in [`mailcraft_model::DavSettings`]; nothing is taken over from the incoming mail server. An OAuth
//! account's access token goes only to its provider's own servers; any other DAV server needs a DAV
//! password of its own.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mailcraft_model::account::CredentialKind;
use mailcraft_model::{Account, AccountId, FolderId, FolderKind, ItemId, RemoteRef};
use mailcraft_net::dav::{DavAuth, DavClient, Fetched, Listing, Precondition, Resource, Service};
use mailcraft_net::http::{Url, normalize_path};
use mailcraft_net::jmap::JmapClient;
use mailcraft_net::oauth::OAuthProvider;
use mailcraft_net::{ConnectOptions, NetError};

use super::Plan;
use super::jmap::JmapConn;
use super::jmap_pim;
use super::native::{Event, Events, oauth_provider};
use super::pim::{self, FolderJob, FolderOutcome, PimEvent, PimKind, PublishJob, legacy_key, stored_canon};
use crate::Session;
use crate::accounts::{account_secret, dav_secret_key, pem_certificates};

/// The DAV part of a send/receive job.
#[derive(Clone, Debug)]
pub(super) struct DavJob {
    /// Where discovery starts (the account's DAV URL).
    start: String,
    /// Addresses whose origins get the credentials (the DAV server and the publishing location).
    trust: Vec<String>,
    /// Collections (canonical) mapped to folders, per kind of folder: a folder still looking for one never
    /// takes those of its kind (a collection of events and tasks serves one Calendar and one Tasks folder).
    held: BTreeMap<PimKind, BTreeSet<String>>,
    auth: DavAuth,
    /// The account's mail sign-in token was kept from a DAV origin that is not its provider's.
    token_withheld: bool,
    /// The publishing location (publications are confined to it).
    publish_base: String,
    options: ConnectOptions,
    /// Look for address books / task lists / calendars (a full run, or a folder not mapped yet).
    discover_contacts: bool,
    discover_tasks: bool,
    discover_calendars: bool,
    folders: Vec<FolderJob>,
    publish: Vec<PublishJob>,
    /// Contacts and calendars kept on the account's JMAP server.
    jmap: Option<JmapPimJob>,
}

/// The JMAP part of a [`DavJob`]: which kinds go to the JMAP server, and whether to list its address books /
/// calendars first.
#[derive(Clone, Debug)]
pub(super) struct JmapPimJob {
    conn: JmapConn,
    contacts: bool,
    calendars: bool,
    discover_contacts: bool,
    discover_calendars: bool,
    tz_offset_min: i32,
    now: mailcraft_model::DateTime,
}

impl JmapPimJob {
    fn handles(&self, kind: PimKind) -> bool {
        match kind {
            PimKind::Contacts => self.contacts,
            PimKind::Calendar => self.calendars,
            PimKind::Tasks => false,
        }
    }
}

/// What the folder synchronisation needs of a server: a WebDAV collection ([`DavClient`]) or a JMAP address
/// book or calendar seen as one ([`jmap_pim::Store`]).
pub(super) trait PimStore {
    fn listing(&self, collection: &str) -> Result<Listing, NetError>;
    fn put(
        &self,
        collection: &str,
        url: &str,
        data: &[u8],
        content_type: &str,
        pre: &Precondition,
    ) -> Result<Option<String>, NetError>;
    fn delete(&self, url: &str, etag: Option<&str>) -> Result<(), NetError>;
    fn multiget(&self, collection: &str, urls: &[String], service: Service)
    -> Result<Vec<Fetched>, NetError>;
    fn list(&self, collection: &str) -> Result<Vec<Resource>, NetError> {
        Ok(self.listing(collection)?.resources)
    }
}

impl PimStore for DavClient {
    fn listing(&self, collection: &str) -> Result<Listing, NetError> {
        DavClient::listing(self, collection)
    }
    fn put(
        &self,
        _collection: &str,
        url: &str,
        data: &[u8],
        content_type: &str,
        pre: &Precondition,
    ) -> Result<Option<String>, NetError> {
        DavClient::put(self, url, data, content_type, pre)
    }
    fn delete(&self, url: &str, etag: Option<&str>) -> Result<(), NetError> {
        DavClient::delete(self, url, etag)
    }
    fn multiget(
        &self,
        collection: &str,
        urls: &[String],
        service: Service,
    ) -> Result<Vec<Fetched>, NetError> {
        DavClient::multiget(self, collection, urls, service)
    }
    fn list(&self, collection: &str) -> Result<Vec<Resource>, NetError> {
        DavClient::list(self, collection)
    }
}

impl PimStore for jmap_pim::Store<'_> {
    fn listing(&self, collection: &str) -> Result<Listing, NetError> {
        jmap_pim::Store::listing(self, collection)
    }
    fn put(
        &self,
        collection: &str,
        url: &str,
        data: &[u8],
        _content_type: &str,
        pre: &Precondition,
    ) -> Result<Option<String>, NetError> {
        jmap_pim::Store::put(self, collection, url, data, pre)
    }
    fn delete(&self, url: &str, etag: Option<&str>) -> Result<(), NetError> {
        jmap_pim::Store::delete(self, url, etag)
    }
    fn multiget(
        &self,
        _collection: &str,
        urls: &[String],
        _service: Service,
    ) -> Result<Vec<Fetched>, NetError> {
        jmap_pim::Store::multiget(self, urls)
    }
}

impl DavJob {
    /// True when the job signs in with the account's OAuth access token on every server it talks to (a
    /// refused sign-in is then worth a renewed token).
    pub(super) fn uses_token(&self) -> bool {
        matches!(self.auth, DavAuth::Bearer(_)) && !self.token_withheld
    }

    /// Puts a renewed OAuth access token in place of the one the job was built with (the run refreshed it).
    pub(super) fn renew_token(&mut self, token: &str) {
        if let DavAuth::Bearer(t) = &mut self.auth {
            token.clone_into(t);
        }
        if let Some(j) = self.jmap.as_mut() {
            j.conn.set_bearer(token);
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.folders.is_empty()
            && self.publish.is_empty()
            && !self.discover_contacts
            && !self.discover_tasks
            && !self.discover_calendars
            && !self.jmap.as_ref().is_some_and(|j| j.discover_contacts || j.discover_calendars)
    }
}

/// Canonical form of a resource or collection URL, used both to compare addresses and to request them:
/// origin plus the normalised path (query left out). Normalising is idempotent, so a canonical URL stays as
/// it is.
pub(super) fn canon(url: &str) -> String {
    match Url::parse(url) {
        Ok(u) => format!(
            "{}{}",
            u.origin().to_ascii_lowercase(),
            normalize_path(u.path.split('?').next().unwrap_or("/"))
        ),
        Err(_) => url.trim().to_owned(),
    }
}

/// Request form of the account's DAV address (as the user typed it, escapes included): canonical, with its
/// query kept.
fn start_url(raw: &str) -> String {
    let query = Url::parse(raw).ok().and_then(|u| u.path.split_once('?').map(|(_, q)| q.to_owned()));
    match query {
        Some(q) => format!("{}?{q}", canon(raw)),
        None => canon(raw),
    }
}

/// The host of an http(s) address or of a bare host name, lower case.
fn host_of(s: &str) -> Option<String> {
    let s = s.trim();
    if s.contains("://") {
        return Url::parse(s).ok().map(|u| u.host.to_ascii_lowercase());
    }
    let host = s.split(['/', ':']).next().unwrap_or_default().to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// True when `url` is on one of the OAuth provider's own servers: the incoming mail server's host, a host of
/// the provider's sign-in endpoints, or (a subdomain of) one of the mail domains the provider serves. Only
/// there may the account's access token go.
fn provider_serves(url: &str, provider: Option<&OAuthProvider>, incoming_host: Option<&str>) -> bool {
    let Some(host) = host_of(url) else { return false };
    if incoming_host.and_then(host_of).is_some_and(|h| h == host) {
        return true;
    }
    let Some(p) = provider else { return false };
    let endpoints =
        [Some(&p.authorization_endpoint), Some(&p.token_endpoint), p.device_authorization_endpoint.as_ref()];
    endpoints.into_iter().flatten().filter_map(|e| host_of(e)).any(|h| h == host)
        || p.domains
            .iter()
            .map(|d| d.trim().to_ascii_lowercase())
            .filter(|d| !d.is_empty())
            .any(|d| host == d || host.strip_suffix(&d).is_some_and(|head| head.ends_with('.')))
}

/// Connection options of the DAV and publishing servers: their own trusted certificate and plain-HTTP consent
/// (never the incoming mail server's).
fn dav_options(dav: &mailcraft_model::DavSettings) -> ConnectOptions {
    let mut o = ConnectOptions {
        connect_timeout: Duration::from_secs(20),
        io_timeout: Duration::from_secs(60),
        ..ConnectOptions::default()
    };
    if let Some(pem) = dav.extra_root_pem.as_deref().filter(|p| !p.trim().is_empty()) {
        match pem_certificates(pem) {
            Ok(der) => o.tls.extra_roots_der = der,
            Err(e) => log::warn!("DAV server certificate: {e}"),
        }
    }
    o.allow_plaintext_auth = dav.allow_plaintext_auth;
    o
}

/// Calendars of `account` whose published copy is due: changed since the last upload, failed last time, or
/// to be withdrawn.
fn publish_jobs(s: &Session, account: AccountId, plan: &Plan) -> Vec<PublishJob> {
    use crate::cmd::calendar::{props, publish::publication_ics};
    let mut out = Vec::new();
    for f in s.mailbox.folders.values().filter(|f| f.kind == FolderKind::Calendar) {
        if plan.folders.as_ref().is_some_and(|fs| !fs.contains(&f.id)) {
            continue;
        }
        let all = props::read(&s.mailbox, f.id);
        for w in all.withdrawals.iter().filter(|w| w.account == account) {
            out.push(PublishJob {
                folder: f.id,
                url: w.url.clone(),
                data: Vec::new(),
                fingerprint: 0,
                withdraw: true,
                etag: w.etag.clone(),
            });
        }
        let Some(p) = all.publication else { continue };
        if p.account != account || p.url.trim().is_empty() {
            continue;
        }
        // Written by an earlier version, which kept no ETag: the file is there (`If-Match: *`).
        let etag =
            if p.etag.is_empty() && p.last_published.is_some() { "*".to_owned() } else { p.etag.clone() };
        if p.withdraw {
            out.push(PublishJob {
                folder: f.id,
                url: p.url.clone(),
                data: Vec::new(),
                fingerprint: 0,
                withdraw: true,
                etag,
            });
            continue;
        }
        let Ok((data, fingerprint)) = publication_ics(s, f.id, &p) else { continue };
        if fingerprint != p.fingerprint || p.last_error.is_some() || p.last_published.is_none() {
            out.push(PublishJob {
                folder: f.id,
                url: p.url.clone(),
                data: data.into_bytes(),
                fingerprint,
                withdraw: false,
                etag,
            });
        }
    }
    out
}

/// The DAV part of `a`'s run for `plan` (`None` when there is nothing to do). `jmap` is the account's JMAP
/// connection (a JMAP account), for the contacts and calendars it keeps there.
pub(super) fn build_job(s: &Session, a: &Account, plan: &Plan, jmap: Option<&JmapConn>) -> Option<DavJob> {
    let folders = pim::plan_folders(s, a.id, plan);
    let publish = publish_jobs(s, a.id, plan);
    let dav = a.dav.clone().unwrap_or_default();
    let full = plan.receive && plan.folders.is_none();
    let unmapped = |k: PimKind| folders.iter().any(|f| f.kind == k && f.collection.is_none());
    let start = dav.url.trim().to_owned();
    let jmap_contacts = jmap.is_some() && pim::via_jmap(a, PimKind::Contacts);
    let jmap_calendars = jmap.is_some() && pim::via_jmap(a, PimKind::Calendar);
    let discover_contacts =
        !start.is_empty() && dav.contacts && !jmap_contacts && (full || unmapped(PimKind::Contacts));
    let discover_tasks = !start.is_empty() && dav.tasks && (full || unmapped(PimKind::Tasks));
    let discover_calendars =
        !start.is_empty() && dav.calendar && !jmap_calendars && (full || unmapped(PimKind::Calendar));
    let jmap = jmap.filter(|_| jmap_contacts || jmap_calendars).map(|conn| JmapPimJob {
        conn: conn.clone(),
        contacts: jmap_contacts,
        calendars: jmap_calendars,
        discover_contacts: jmap_contacts && (full || unmapped(PimKind::Contacts)),
        discover_calendars: jmap_calendars && (full || unmapped(PimKind::Calendar)),
        tz_offset_min: s.tz_offset_min,
        now: s.now(),
    });
    if folders.is_empty()
        && publish.is_empty()
        && !discover_contacts
        && !discover_tasks
        && !discover_calendars
        && !jmap.as_ref().is_some_and(|j| j.discover_contacts || j.discover_calendars)
    {
        return None;
    }
    let inc = a.incoming.as_ref();
    let user = [dav.username.trim(), inc.map(|i| i.username.trim()).unwrap_or_default(), a.address.trim()]
        .into_iter()
        .find(|u| !u.is_empty())
        .unwrap_or_default()
        .to_owned();
    let mut trust: Vec<String> = [dav.url.trim(), dav.publish_url.trim()]
        .into_iter()
        .filter(|u| !u.is_empty())
        .map(str::to_owned)
        .collect();
    let mut token_withheld = false;
    let auth = match (s.secrets.get(&dav_secret_key(a.id)), account_secret(s, a.id, false)) {
        (Some(password), _) => DavAuth::Basic { user, password },
        (None, Some(token)) if a.credential == CredentialKind::OAuthToken => {
            // The mail sign-in token is for the provider: other servers never see it.
            let provider = oauth_provider(s, a.id);
            let before = trust.len();
            trust.retain(|u| provider_serves(u, provider.as_ref(), inc.map(|i| i.host.as_str())));
            token_withheld = trust.len() < before;
            DavAuth::Bearer(token)
        }
        (None, Some(password)) => DavAuth::Basic { user, password },
        (None, None) => DavAuth::None,
    };
    let options = dav_options(&dav);
    let mut held: BTreeMap<PimKind, BTreeSet<String>> = BTreeMap::new();
    for (f, st) in &a.sync.dav {
        if let Some(kind) = s.mailbox.folder(*f).and_then(|f| PimKind::of(f.kind)) {
            held.entry(kind).or_default().insert(stored_canon(&st.collection));
        }
    }
    Some(DavJob {
        start,
        trust,
        held,
        auth,
        token_withheld,
        publish_base: dav.publish_url.trim().to_owned(),
        options,
        discover_contacts,
        discover_tasks,
        discover_calendars,
        folders,
        publish,
        jmap,
    })
}

/// Errors that end the DAV part of a run (the others only affect one item).
fn fatal(e: &NetError) -> bool {
    e.is_connection_lost() || matches!(e, NetError::Auth(_) | NetError::Connect(_) | NetError::Insecure(_))
}

fn user_error(job: &DavJob, e: &NetError) -> String {
    match e {
        NetError::Auth(_) if job.token_withheld => {
            "the contacts and tasks server needs a password of its own \
            (Account Settings): the mail sign-in only goes to the mail provider's own servers"
                .into()
        }
        NetError::Auth(_) => "the contacts and tasks server did not accept the user name or password".into(),
        other => other.to_string(),
    }
}

const CANCELLED: &str = "Send/Receive was cancelled.";

/// True when a published file may be removed: MailCraft wrote it (it has the file's ETag), and when that
/// ETag is only `*` (the server gave none, or an earlier version kept none, so `If-Match` matches any file)
/// the address is also a file strictly inside the current publishing location.
fn may_withdraw(publish_base: &str, url: &str, etag: &str) -> bool {
    match etag {
        "" => false,
        "*" => pim::publication_target(publish_base, url).is_ok(),
        _ => true,
    }
}

/// Uploads (or removes) one published calendar; `Ok` carries the file's new ETag (`*` when the server gave
/// none, empty after a removal). Writes are conditional, so a file MailCraft did not write is never replaced
/// or removed: the first upload only creates (`If-None-Match: *`), later ones and the removal need the ETag
/// of the last upload (`If-Match`).
fn publish_one(client: &DavClient, job: &DavJob, p: &PublishJob) -> Result<String, String> {
    if p.withdraw {
        return withdraw_one(client, job, p);
    }
    let url = pim::publication_target(&job.publish_base, &p.url)?;
    let pre = if p.etag.is_empty() { Precondition::Create } else { Precondition::Match(p.etag.clone()) };
    match client.put(&url, &p.data, "text/calendar; charset=utf-8", &pre) {
        Ok(Some(etag)) => Ok(etag),
        // No ETag in the answer: ask for it, else remember only that the file is ours.
        Ok(None) => Ok(client.etag(&url).ok().flatten().unwrap_or_else(|| "*".into())),
        Err(NetError::Http { status: 412, .. }) if p.etag.is_empty() => Err(format!(
            "a file MailCraft did not publish is already at {url}; it was left as it is (publish at another address)"
        )),
        Err(NetError::Http { status: 412, .. }) => Err(format!(
            "the published file at {url} was changed or replaced on the server; it was left as it is (publish at \
             another address, or remove that file)"
        )),
        Err(e) => Err(user_error(job, &e)),
    }
}

/// Removes a published file. A file MailCraft never wrote (no ETag) is not its to remove. A file it cannot
/// remove safely (it changed on the server since, or an earlier version published it outside the current
/// publishing location) is left in place and reported, naming the address that is still public; the request
/// stays, so a later run sees the file gone once it was removed by hand.
fn withdraw_one(client: &DavClient, job: &DavJob, p: &PublishJob) -> Result<String, String> {
    if p.etag.is_empty() {
        return Ok(String::new());
    }
    // An address stored by an earlier version may be decoded: escaped again for the request.
    let url = stored_canon(&p.url);
    // Gone (removed by hand, or never there): nothing left to remove.
    let gone = || matches!(client.etag(&url), Err(NetError::Http { status: 404 | 410, .. }));
    if !may_withdraw(&job.publish_base, &p.url, &p.etag) {
        if gone() {
            return Ok(String::new());
        }
        return Err(format!(
            "the calendar is still published at {} (outside the current publishing location, so MailCraft does \
             not remove it): remove that file on the server",
            p.url
        ));
    }
    match client.delete(&url, Some(&p.etag)) {
        Ok(()) => Ok(String::new()),
        Err(NetError::Http { status: 412, .. }) if gone() => Ok(String::new()),
        Err(NetError::Http { status: 412, .. }) => Err(format!(
            "the calendar is still published at {}: the file changed on the server since MailCraft published \
             it, so it was left in place (remove it on the server)",
            p.url
        )),
        Err(e) => Err(user_error(job, &e)),
    }
}

/// How the DAV part of a run ended.
pub(super) struct DavEnd {
    /// The error that stopped it (or the first one met), if any.
    pub(super) error: Option<String>,
    /// The server refused the sign-in, which stopped the run (an OAuth account renews its token and tries
    /// again).
    pub(super) refused: bool,
}

impl DavEnd {
    fn stopped(error: String) -> DavEnd {
        DavEnd { error: Some(error), refused: false }
    }
}

/// Runs the DAV part of a job.
pub(super) fn run(account: AccountId, job: &DavJob, events: &Events, cancel: &Arc<AtomicBool>) -> DavEnd {
    // Set once a result that changes the mailbox went out: from then on a refused sign-in is not retried
    // from the job's start (that would work from what this run already changed).
    let changed = std::cell::Cell::new(false);
    let send = |ev: PimEvent| {
        if matches!(ev, PimEvent::Folder(_) | PimEvent::Published { .. }) {
            changed.set(true);
        }
        if !events.send(Event::Dav { account, ev: Box::new(ev) }) {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let progress = |t: &str| {
        events.send(Event::step(account, t));
    };
    let cancelled = || cancel.load(Ordering::SeqCst);
    let client = job
        .trust
        .iter()
        .fold(DavClient::new(job.options.clone(), job.auth.clone()), |c, u| c.trust(u))
        .with_cancel(Arc::clone(cancel));
    // A request that failed because Send/Receive was cancelled ends the run as cancelled.
    let failed = |e: &NetError| if cancelled() { CANCELLED.to_owned() } else { user_error(job, e) };
    let end = |e: &NetError| DavEnd {
        error: Some(failed(e)),
        refused: matches!(e, NetError::Auth(_)) && !changed.get(),
    };
    let mut error: Option<String> = None;
    let mut found: BTreeMap<&'static str, Vec<(String, String)>> = BTreeMap::new();
    // The JMAP server, for the contacts and calendars kept there.
    let jmap: Option<Result<JmapClient, NetError>> = job.jmap.as_ref().map(|j| j.conn.connect());
    let jmap_discover = job.jmap.as_ref().is_some_and(|j| j.discover_contacts || j.discover_calendars);
    if job.discover_contacts || job.discover_tasks || job.discover_calendars || jmap_discover {
        progress("Looking for address books, task lists and calendars…");
        let mut contacts = None;
        let mut tasks = None;
        let mut calendars = None;
        if let (Some(j), Some(c)) = (job.jmap.as_ref(), jmap.as_ref()) {
            for (kind, wanted, key) in [
                (PimKind::Contacts, j.discover_contacts, "contacts"),
                (PimKind::Calendar, j.discover_calendars, "calendars"),
            ] {
                if !wanted || cancelled() {
                    continue;
                }
                let list = match c {
                    Ok(c) if jmap_pim::offers(c, kind) => jmap_pim::collections(c, kind),
                    Ok(_) => Ok(Vec::new()),
                    Err(e) => Err(e.clone()),
                };
                match list {
                    Ok(l) => {
                        found.insert(key, l.clone());
                        if kind == PimKind::Contacts {
                            contacts = Some(l);
                        } else {
                            calendars = Some(l);
                        }
                    }
                    Err(e) if fatal(&e) => return end(&e),
                    Err(e) => error = Some(format!("{}: {}", kind.label(), failed(&e))),
                }
            }
        }
        if job.discover_contacts {
            match client.discover(&start_url(&job.start), Service::Contacts) {
                Ok(list) => {
                    let l: Vec<(String, String)> =
                        list.into_iter().filter(|c| c.addressbook).map(|c| (canon(&c.url), c.name)).collect();
                    found.insert("contacts", l.clone());
                    contacts = Some(l);
                }
                Err(e) if fatal(&e) => return end(&e),
                Err(e) => error = Some(format!("address books: {}", failed(&e))),
            }
        }
        if (job.discover_tasks || job.discover_calendars) && !cancelled() {
            match client.discover(&start_url(&job.start), Service::Calendars) {
                Ok(list) => {
                    let pick = |events: bool| -> Vec<(String, String)> {
                        list.iter()
                            .filter(|c| if events { c.holds_events() } else { c.holds_tasks() })
                            .map(|c| (canon(&c.url), c.name.clone()))
                            .collect()
                    };
                    if job.discover_tasks {
                        let l = pick(false);
                        found.insert("tasks", l.clone());
                        tasks = Some(l);
                    }
                    if job.discover_calendars {
                        let l = pick(true);
                        found.insert("calendars", l.clone());
                        calendars = Some(l);
                    }
                }
                Err(e) if fatal(&e) => return end(&e),
                Err(e) => error = Some(format!("calendars and task lists: {}", failed(&e))),
            }
        }
        send(PimEvent::Collections { contacts, tasks, calendars });
    }
    // Collections taken by a folder of this run that had none yet.
    let mut held = job.held.clone();
    for f in &job.folders {
        if cancelled() {
            return DavEnd::stopped(CANCELLED.into());
        }
        if f.too_many {
            let what = pim::too_many_error();
            send(PimEvent::FolderFailed { folder: f.folder, error: what.clone() });
            error.get_or_insert(what);
            continue;
        }
        let kind_key = match f.kind {
            PimKind::Contacts => "contacts",
            PimKind::Tasks => "tasks",
            PimKind::Calendar => "calendars",
        };
        let via_jmap = job.jmap.as_ref().is_some_and(|j| j.handles(f.kind));
        let collection = match &f.collection {
            Some(c) if via_jmap => Some(c.clone()),
            // Stored by an earlier version (decoded): the address discovery just found for it, when it ran.
            Some(c) => Some(
                found
                    .get(kind_key)
                    .and_then(|l| pim::discovered_form(l, c))
                    .unwrap_or_else(|| stored_canon(c)),
            ),
            None => {
                let mine = held.entry(f.kind).or_default();
                let free = found.get(kind_key).and_then(|l| free_collection(l, mine));
                if let Some(c) = &free {
                    mine.insert(c.clone());
                }
                free
            }
        };
        let Some(collection) = collection else {
            let what = match f.kind {
                PimKind::Contacts => "No address book was found on the contacts server.",
                PimKind::Tasks => "No task list was found on the server.",
                PimKind::Calendar => "No calendar was found on the server.",
            };
            send(PimEvent::FolderFailed { folder: f.folder, error: what.into() });
            error.get_or_insert_with(|| what.to_owned());
            continue;
        };
        progress(&format!("Updating {}…", f.kind.label()));
        let result = if via_jmap {
            match jmap.as_ref() {
                Some(Ok(c)) => {
                    let (tz, now) = job
                        .jmap
                        .as_ref()
                        .map_or((0, mailcraft_model::DateTime::default()), |j| (j.tz_offset_min, j.now));
                    let store = jmap_pim::Store::new(c, f.kind, tz, now);
                    sync_folder(&store, f, &collection)
                }
                Some(Err(e)) => Err(e.clone()),
                None => Err(NetError::Protocol("no JMAP connection".into())),
            }
        } else {
            sync_folder(&client, f, &collection)
        };
        match result {
            Ok(out) => send(PimEvent::Folder(Box::new(out))),
            Err(e) => {
                let msg = failed(&e);
                send(PimEvent::FolderFailed { folder: f.folder, error: msg.clone() });
                if fatal(&e) {
                    return end(&e);
                }
                error.get_or_insert(msg);
            }
        }
    }
    for p in &job.publish {
        if cancelled() {
            return DavEnd::stopped(CANCELLED.into());
        }
        progress("Publishing calendars…");
        let result = publish_one(&client, job, p);
        // A write that went through is recorded even when Send/Receive was cancelled meanwhile: its new ETag
        // is what the next upload and the removal are conditional on. A failure the cancel caused is not.
        if cancelled() && result.is_err() {
            return DavEnd::stopped(CANCELLED.into());
        }
        if let Err(e) = &result {
            error.get_or_insert_with(|| e.clone());
        }
        send(PimEvent::Published {
            folder: p.folder,
            url: p.url.clone(),
            fingerprint: p.fingerprint,
            withdrawn: p.withdraw,
            result,
        });
        if cancelled() {
            return DavEnd::stopped(CANCELLED.into());
        }
    }
    DavEnd { error, refused: false }
}

/// The first discovered collection no folder holds (a folder without one takes it).
fn free_collection(found: &[(String, String)], held: &BTreeSet<String>) -> Option<String> {
    // A collection an earlier version stored (decoded) is held too.
    let legacy: BTreeSet<String> = held.iter().map(|h| legacy_key(h)).collect();
    found.iter().map(|(u, _)| u).find(|u| !held.contains(*u) && !legacy.contains(&legacy_key(u))).cloned()
}

/// Resolves stored resource addresses against a listing: the canonical form when the server lists it;
/// else the one listed resource an earlier version would have stored the same way (its decoded form); else,
/// when the listing came from another address (a redirect), the same name there.
struct Resolver<'a> {
    server: &'a BTreeMap<String, String>,
    /// Decoded form → the listed address (`None`: more than one share it).
    legacy: BTreeMap<String, Option<String>>,
    /// The collection as requested and as it answered (canonical), when they differ.
    moved: Option<(String, String)>,
}

impl<'a> Resolver<'a> {
    fn new(server: &'a BTreeMap<String, String>, requested: &str, answered: &str) -> Resolver<'a> {
        let mut legacy: BTreeMap<String, Option<String>> = BTreeMap::new();
        for u in server.keys() {
            legacy.entry(legacy_key(u)).and_modify(|v| *v = None).or_insert_with(|| Some(u.clone()));
        }
        let slash = |s: &str| if s.ends_with('/') { s.to_owned() } else { format!("{s}/") };
        let moved = (requested != answered).then(|| (slash(requested), slash(answered)));
        Resolver { server, legacy, moved }
    }

    fn resolve(&self, stored: &str) -> String {
        let c = stored_canon(stored);
        if self.server.contains_key(&c) {
            return c;
        }
        if let Some(Some(u)) = self.legacy.get(&legacy_key(stored)) {
            return u.clone();
        }
        if let Some((from, to)) = &self.moved
            && let Some(name) = c.strip_prefix(from.as_str())
        {
            let there = format!("{to}{name}");
            if self.server.contains_key(&there) {
                return there;
            }
            if let Some(Some(u)) = self.legacy.get(&legacy_key(&there)) {
                return u.clone();
            }
            return there;
        }
        c
    }
}

/// A server copy this run does not know yet, waiting to be matched with an item new here.
struct Unknown {
    url: String,
    etag: String,
    data: String,
    uid: String,
    fingerprint: Option<u64>,
    taken: bool,
}

/// Synchronises one folder with `collection` (canonical URL).
fn sync_folder(client: &dyn PimStore, job: &FolderJob, collection: &str) -> Result<FolderOutcome, NetError> {
    let kind = job.kind;
    let service = match kind {
        PimKind::Contacts => Service::Contacts,
        PimKind::Tasks | PimKind::Calendar => Service::Calendars,
    };
    let requested = stored_canon(collection);
    let listing = client.listing(&requested)?;
    // A collection that answered from another address (its server moved it) is used, and kept, there.
    let answered = canon(&listing.collection);
    // Moved to another server: its members are not this collection's, and nothing here is compared with
    // them (an item missing from such a listing would read as deleted on the server).
    if pim::web_origin(&answered) != pim::web_origin(&requested) {
        return Err(NetError::Protocol(format!("{requested} answered from another server ({answered})")));
    }
    let coll_wire = answered;
    // Some responses were not members of the collection: the listing may be incomplete, so an item missing
    // from it is not taken as deleted on the server this time.
    let complete = listing.dropped == 0;
    let server: BTreeMap<String, String> =
        listing.resources.into_iter().map(|r| (canon(&r.url), r.etag)).collect();
    let resolver = Resolver::new(&server, &requested, &coll_wire);
    let known_before: BTreeMap<String, String> =
        job.known.iter().map(|(u, e)| (resolver.resolve(u), e.clone())).collect();
    let mut out = FolderOutcome {
        folder: job.folder,
        kind: Some(kind),
        collection: coll_wire.clone(),
        ..Default::default()
    };
    let mut known = server.clone();
    let mut fetch: Vec<(Option<(ItemId, u64)>, String)> = Vec::new();
    let mut here: BTreeSet<String> = BTreeSet::new();
    let mut relist = false;
    enum Put {
        /// Written; the new ETag (empty when the server gave none).
        Done(String),
        /// The precondition failed (412).
        Conflict,
        /// Refused for this item only (noted in `errors`).
        Failed,
    }
    let put = |url: &str, body: &str, pre: Precondition, out: &mut FolderOutcome| -> Result<Put, NetError> {
        match client.put(&coll_wire, url, body.as_bytes(), kind.content_type(), &pre) {
            Ok(etag) => Ok(Put::Done(etag.unwrap_or_default())),
            Err(e) if fatal(&e) => Err(e),
            Err(NetError::Http { status: 412, .. }) => Ok(Put::Conflict),
            Err(e) => {
                out.errors.push(format!("{url}: {e}"));
                Ok(Put::Failed)
            }
        }
    };
    let uploaded = |item: &pim::LocalItem, url: &str, etag: String, body: &str| RemoteRef {
        url: url.to_owned(),
        etag,
        uid: item.uid.clone(),
        synced: item.fingerprint,
        // The server now holds what went up.
        raw: pim::kept(kind, body),
    };
    // Items new here go up after the server's unknown copies are read: one of those may be the same item
    // (its link was lost with a settings change, or another device uploaded it), and is then taken instead
    // of duplicated.
    let mut new_here: Vec<(&pim::LocalItem, String)> = Vec::new();
    for item in &job.items {
        let url = match &item.remote {
            Some(r) => resolver.resolve(&r.url),
            None => canon(&pim::resource_url(&coll_wire, &item.uid, kind)),
        };
        match (&item.remote, item.changed()) {
            (None, _) => new_here.push((item, url)),
            (Some(_), true) => {
                here.insert(url.clone());
                // Changed here: this copy wins over the server's (If-Match on the server's current ETag, so a
                // change made there during this run is not overwritten unseen).
                let pre = match server.get(&url) {
                    Some(etag) if !etag.is_empty() => Precondition::Match(etag.clone()),
                    Some(_) => Precondition::Any,
                    None => Precondition::Create,
                };
                // Built here, off the session thread: the merge with the server's copy reads the whole object.
                let body = item.body();
                match put(&url, &body, pre, &mut out)? {
                    Put::Done(etag) => {
                        relist |= etag.is_empty();
                        known.insert(url.clone(), etag.clone());
                        out.uploaded.push((item.id, uploaded(item, &url, etag, &body)));
                    }
                    Put::Conflict => out.errors.push(format!("{url}: changed on the server meanwhile")),
                    Put::Failed => {}
                }
            }
            (Some(r), false) => {
                here.insert(url.clone());
                match server.get(&url) {
                    Some(etag) if *etag != r.etag => {
                        fetch.push((Some((item.id, item.fingerprint)), url.clone()))
                    }
                    // Unchanged, but stored in another form (an earlier version's, or before the collection
                    // moved): the link takes the server's address.
                    Some(_) if url != r.url => out.relinked.push((item.id, url.clone())),
                    Some(_) => {}
                    None if complete => out.removed.push((item.id, item.fingerprint)),
                    None => {}
                }
            }
        }
    }
    let pending: BTreeSet<&str> = new_here.iter().map(|(_, u)| u.as_str()).collect();
    for (url, etag) in &server {
        if here.contains(url) {
            continue;
        }
        if !pending.contains(url.as_str()) && known_before.get(url) == Some(etag) {
            // Removed here since the last run: remove the server copy too (unless it changed meanwhile).
            match client.delete(url, Some(etag)) {
                Ok(()) => {
                    known.remove(url);
                    out.deleted.push(url.clone());
                }
                Err(e) if fatal(&e) => return Err(e),
                Err(NetError::Http { status: 412, .. }) => fetch.push((None, url.clone())),
                Err(e) => out.errors.push(format!("{url}: {e}")),
            }
        } else {
            fetch.push((None, url.clone()));
        }
    }
    let mut unknown: Vec<Unknown> = Vec::new();
    download(client, &coll_wire, service, &fetch, &mut known, |local, url, etag, data| match local {
        Some(local) => out.incoming.push(pim::Incoming { local: Some(local), url, etag, data }),
        None => {
            let (uid, fingerprint) = pim::server_identity(kind, &data);
            unknown.push(Unknown { url, etag, data, uid, fingerprint, taken: false });
        }
    })?;
    let mut conflicts: Vec<(&pim::LocalItem, String)> = Vec::new();
    for (item, url) in new_here {
        // The same resource (holding the same item, or one without a UID), the same UID, or (a contact) the
        // same card. A server copy of another item is never taken by its address alone.
        let same = unknown
            .iter()
            .position(|u| !u.taken && u.url == url && (u.uid.is_empty() || pim::same_uid(&item.uid, &u.uid)))
            .or_else(|| {
                unknown.iter().position(|u| {
                    !u.taken && (pim::same_uid(&item.uid, &u.uid) || u.fingerprint == Some(item.fingerprint))
                })
            });
        if let Some(u) = same.and_then(|i| unknown.get_mut(i)) {
            u.taken = true;
            here.insert(u.url.clone());
            out.incoming.push(pim::Incoming {
                local: Some((item.id, item.fingerprint)),
                url: u.url.clone(),
                etag: u.etag.clone(),
                data: std::mem::take(&mut u.data),
            });
            continue;
        }
        here.insert(url.clone());
        let body = item.body();
        match put(&url, &body, Precondition::Create, &mut out)? {
            Put::Done(etag) => {
                relist |= etag.is_empty();
                known.insert(url.clone(), etag.clone());
                out.uploaded.push((item.id, uploaded(item, &url, etag, &body)));
            }
            // Something already lives at that name (the same item from another device): take it.
            Put::Conflict => conflicts.push((item, url.clone())),
            Put::Failed => {}
        }
    }
    for u in unknown.into_iter().filter(|u| !u.taken) {
        out.incoming.push(pim::Incoming { local: None, url: u.url, etag: u.etag, data: u.data });
    }
    let conflict_fetch: Vec<(Option<(ItemId, u64)>, String)> =
        conflicts.iter().map(|(item, url)| (Some((item.id, item.fingerprint)), url.clone())).collect();
    let mut taken_by_others: Vec<(&pim::LocalItem, String)> = Vec::new();
    download(client, &coll_wire, service, &conflict_fetch, &mut known, |local, url, etag, data| {
        let mine = conflicts.iter().find(|(_, u)| *u == url).map(|(item, _)| *item);
        let (server_uid, _) = pim::server_identity(kind, &data);
        match mine {
            Some(item)
                if !server_uid.is_empty()
                    && !item.uid.is_empty()
                    && !pim::same_uid(&item.uid, &server_uid) =>
            {
                // Another item lives at that address: it is a server item like any other (imported here, so
                // its address being known never reads as a deletion here), and the item here goes up under
                // another name.
                if !out.incoming.iter().any(|i| i.url == url) {
                    out.incoming.push(pim::Incoming { local: None, url: url.clone(), etag, data });
                }
                taken_by_others.push((item, url));
            }
            _ => out.incoming.push(pim::Incoming { local, url, etag, data }),
        }
    })?;
    for (item, taken) in taken_by_others {
        let url = canon(&pim::alternate_resource_url(&coll_wire, &item.uid, kind));
        if url == taken || known.contains_key(&url) {
            out.errors.push(format!("{taken}: another item already has this address on the server"));
            continue;
        }
        let body = item.body();
        match put(&url, &body, Precondition::Create, &mut out)? {
            Put::Done(etag) => {
                relist |= etag.is_empty();
                known.insert(url.clone(), etag.clone());
                out.uploaded.push((item.id, uploaded(item, &url, etag, &body)));
            }
            Put::Conflict => {
                out.errors.push(format!("{taken}: another item already has this address on the server"))
            }
            Put::Failed => {}
        }
    }
    if relist {
        // Only the ETags of what this run uploaded without one are filled in. Anything else in the new listing
        // (created elsewhere meanwhile, too large, unreadable) is not taken as known: the next run would
        // otherwise read its absence here as a deletion and remove it from the server.
        for r in client.list(&coll_wire)? {
            let url = canon(&r.url);
            if let Some((_, rr)) = out.uploaded.iter_mut().find(|(_, x)| x.url == url && x.etag.is_empty()) {
                rr.etag = r.etag.clone();
                known.insert(url, r.etag);
            }
        }
    }
    out.known = known;
    Ok(out)
}

/// Downloads `fetch` (local item, canonical URL) and hands each copy to `take` with its ETag. A copy that
/// did not come back (gone meanwhile, or too large) is dropped from `known`, so the next run does not take
/// it for an item removed here.
fn download(
    client: &dyn PimStore,
    coll_wire: &str,
    service: Service,
    fetch: &[(Option<(ItemId, u64)>, String)],
    known: &mut BTreeMap<String, String>,
    mut take: impl FnMut(Option<(ItemId, u64)>, String, String, String),
) -> Result<(), NetError> {
    if fetch.is_empty() {
        return Ok(());
    }
    let urls: Vec<String> = fetch.iter().map(|(_, u)| u.clone()).collect();
    let got = client.multiget(coll_wire, &urls, service)?;
    let mut by_url: BTreeMap<String, (String, String)> =
        got.into_iter().map(|f| (canon(&f.url), (f.etag, f.data))).collect();
    for (local, url) in fetch {
        let Some((etag, data)) = by_url.remove(url) else {
            known.remove(url);
            continue;
        };
        let etag = if etag.is_empty() { known.get(url).cloned().unwrap_or_default() } else { etag };
        known.insert(url.clone(), etag.clone());
        take(*local, url.clone(), etag, data);
    }
    Ok(())
}

/// Folders of a plan that hold mail of `account` (contacts, tasks and calendars are not IMAP's).
pub(super) fn mail_only(s: &Session, account: AccountId, plan: &Plan) -> Plan {
    let mut p = plan.clone();
    if !pim::plan_has_mail(s, account, plan) {
        p.receive = false;
        p.folders = None;
    } else if let Some(fs) = &plan.folders {
        let mail: Vec<FolderId> = fs
            .iter()
            .copied()
            .filter(|f| s.mailbox.folder(*f).is_none_or(|x| x.kind == FolderKind::Mail))
            .collect();
        p.folders = Some(mail);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_compare_in_canonical_form() {
        assert_eq!(
            canon("https://h.example.com:443/dav/a%40b.com/c/x.vcf"),
            "https://h.example.com/dav/a@b.com/c/x.vcf"
        );
        assert_eq!(
            canon("https://h.example.com/dav/a@b.com/c/x.vcf"),
            canon("https://H.example.com/dav/a%40b.com/c/x.vcf")
        );
        assert_eq!(canon("https://h.example.com/c/"), "https://h.example.com/c/");
        assert_eq!(canon("not a url"), "not a url");
        // Regression: `%2F` and `%2E` were decoded, so a different resource was requested.
        assert_eq!(canon("https://h.example.com/c/a%2Fb.vcf"), "https://h.example.com/c/a%2Fb.vcf");
        assert_eq!(
            canon("https://h.example.com/c/%2E%2E%2F%2E%2E%2Fother%2Fx.vcf"),
            "https://h.example.com/c/..%2F..%2Fother%2Fx.vcf"
        );
        // A link stored by an earlier version (fully decoded) still matches the server's escaped form.
        assert_eq!(canon("https://h.example.com/c/\u{fc}.vcf"), canon("https://h.example.com/c/%C3%BC.vcf"));
        let c = canon("https://h.example.com/c/a%2fb%20c.vcf");
        assert_eq!(canon(&c), c, "canonical URLs stay as they are");
    }

    #[test]
    fn the_start_address_keeps_its_escapes() {
        // Regression: `me%40example.com` was encoded again to `me%2540example.com`, and a query to `%3F…`.
        assert_eq!(
            start_url("https://h.example.com/remote.php/dav/principals/users/me%40example.com/"),
            "https://h.example.com/remote.php/dav/principals/users/me@example.com/"
        );
        assert_eq!(start_url("https://h.example.com/dav/a%20b/?x=1"), "https://h.example.com/dav/a%20b/?x=1");
    }

    #[test]
    fn the_mail_token_goes_only_to_the_providers_servers() {
        let p = OAuthProvider {
            id: "example".into(),
            name: "Example".into(),
            authorization_endpoint: "https://login.example.com/auth".into(),
            token_endpoint: "https://login.example.com/token".into(),
            device_authorization_endpoint: None,
            client_id: "c".into(),
            client_secret: None,
            scopes: vec!["mail".into()],
            domains: vec!["example.com".into()],
            extra_auth_params: Vec::new(),
        };
        let serves = |u: &str| provider_serves(u, Some(&p), Some("imap.example.net"));
        assert!(serves("https://dav.example.com/"));
        assert!(serves("https://example.com/dav/"));
        assert!(serves("https://login.example.com/x"));
        assert!(serves("https://imap.example.net/dav/"), "the incoming server's host");
        assert!(!serves("https://dav.example.org/"));
        assert!(!serves("https://notexample.com/"));
        assert!(!serves("https://example.com.example.org/"));
        assert!(!provider_serves("https://dav.example.com/", None, None));
    }

    #[test]
    fn only_files_known_to_be_published_here_are_withdrawn() {
        // Regression: a stopped (or moved) publication was removed with an unconditional DELETE, whatever the
        // address named. An earlier version kept no ETag, so such a file is only removed (`If-Match: *`) when
        // it is a file inside the publishing location.
        let base = "https://cloud.example.com/dav/me/public/";
        assert!(!may_withdraw(base, "https://cloud.example.com/dav/me/public/a.ics", ""), "never written");
        assert!(may_withdraw(base, "https://cloud.example.com/dav/me/public/a.ics", "*"));
        assert!(!may_withdraw(base, "https://cloud.example.com/dav/me/contacts/", "*"));
        assert!(!may_withdraw(base, "https://cloud.example.com/dav/me/contacts/x.vcf", "*"));
        assert!(!may_withdraw("", "https://cloud.example.com/dav/me/public/a.ics", "*"));
        // Its own ETag: removed only while the file is still the one written.
        assert!(may_withdraw(base, "https://cloud.example.com/old/a.ics", "\"e1\""));
    }

    #[test]
    fn stored_links_resolve_to_the_listed_resources() {
        // Regression: links an earlier version stored decoded (a space, `+` from `%2B`, `#`) never matched the
        // listing, so unchanged items were taken as deleted on the server and removed here.
        let h = "https://dav.example.com/dav/me/c/";
        let server: BTreeMap<String, String> = [
            format!("{h}John%20Smith.vcf"),
            format!("{h}a%2Bb.vcf"),
            format!("{h}x%23y.vcf"),
            format!("{h}plain.vcf"),
        ]
        .into_iter()
        .map(|u| (u, "\"1\"".to_owned()))
        .collect();
        let r = Resolver::new(&server, h, h);
        assert_eq!(r.resolve(&format!("{h}John Smith.vcf")), format!("{h}John%20Smith.vcf"));
        assert_eq!(r.resolve(&format!("{h}a+b.vcf")), format!("{h}a%2Bb.vcf"));
        assert_eq!(r.resolve(&format!("{h}x#y.vcf")), format!("{h}x%23y.vcf"));
        assert_eq!(r.resolve(&format!("{h}plain.vcf")), format!("{h}plain.vcf"));
        assert_eq!(r.resolve(&format!("{h}a%2Bb.vcf")), format!("{h}a%2Bb.vcf"));
        assert_eq!(r.resolve(&format!("{h}gone.vcf")), format!("{h}gone.vcf"), "not listed: as stored");
        // Two listed resources share a decoded form: neither is guessed.
        let both: BTreeMap<String, String> = [format!("{h}a%3Db%2C.vcf"), format!("{h}a=b%2C.vcf")]
            .into_iter()
            .map(|u| (u, String::new()))
            .collect();
        let r = Resolver::new(&both, h, h);
        assert_eq!(r.resolve(&format!("{h}a=b,.vcf")), format!("{h}a=b,.vcf"));
        // The collection answered from a new address: links follow it.
        let moved: BTreeMap<String, String> =
            [("https://dav.example.com/new/c/x.vcf".to_owned(), String::new())].into_iter().collect();
        let r = Resolver::new(&moved, h, "https://dav.example.com/new/c/");
        assert_eq!(r.resolve(&format!("{h}x.vcf")), "https://dav.example.com/new/c/x.vcf");
    }

    /// A one-request HTTP server: answers `answer` after setting `flag` (as if the user cancelled while the
    /// request was on its way).
    fn serve_once_and_cancel(
        answer: &'static str,
        flag: Arc<AtomicBool>,
    ) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(':')
                    && k.eq_ignore_ascii_case("content-length")
                {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            flag.store(true, Ordering::SeqCst);
            let mut s = s;
            s.write_all(answer.as_bytes()).unwrap();
        });
        (base, t)
    }

    #[test]
    fn an_upload_that_went_through_is_recorded_even_when_cancelled() {
        // Regression: cancelling while a publish PUT was on its way dropped the result of the PUT that went
        // through; the publication kept the old ETag, and every later upload (and the removal) failed with 412.
        let cancel = Arc::new(AtomicBool::new(false));
        let (base, t) = serve_once_and_cancel(
            "HTTP/1.1 204 No Content\r\nETag: \"e2\"\r\nContent-Length: 0\r\n\r\n",
            Arc::clone(&cancel),
        );
        let job = DavJob {
            start: String::new(),
            trust: vec![base.clone()],
            held: BTreeMap::new(),
            auth: DavAuth::Basic { user: "me@example.com".into(), password: "pw".into() },
            token_withheld: false,
            publish_base: format!("{base}/p/"),
            options: ConnectOptions { allow_plaintext_auth: true, ..ConnectOptions::default() },
            discover_contacts: false,
            discover_tasks: false,
            discover_calendars: false,
            folders: Vec::new(),
            jmap: None,
            publish: vec![PublishJob {
                folder: FolderId(7),
                url: format!("{base}/p/cal.ics"),
                data: b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".to_vec(),
                fingerprint: 42,
                withdraw: false,
                etag: "\"e1\"".into(),
            }],
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let end = run(AccountId(1), &job, &Events::to(tx), &cancel);
        t.join().unwrap();
        assert_eq!(end.error.as_deref(), Some(CANCELLED));
        let published: Vec<PimEvent> = rx
            .try_iter()
            .filter_map(|(_, e)| match e {
                Event::Dav { ev, .. } => Some(*ev),
                _ => None,
            })
            .collect();
        assert!(
            matches!(published.as_slice(), [PimEvent::Published { result: Ok(etag), fingerprint: 42, .. }] if etag == "\"e2\""),
            "{published:?}"
        );
    }

    #[test]
    fn a_collection_redirected_to_another_server_removes_nothing() {
        // A listing that answered from another origin names that server's resources, never this collection's:
        // an item missing from it must not read as deleted on the server.
        let flag = Arc::new(AtomicBool::new(false));
        let answer = "HTTP/1.1 301 Moved\r\nLocation: http://127.0.0.2:9/c/\r\nContent-Length: 0\r\n\r\n";
        let (base, t) = serve_once_and_cancel(answer, Arc::clone(&flag));
        let client = DavClient::new(ConnectOptions::default(), DavAuth::None).trust(&base);
        let job = FolderJob {
            folder: FolderId(3),
            kind: PimKind::Contacts,
            collection: Some(format!("{base}/c/")),
            known: [(format!("{base}/c/pat.vcf"), "\"1\"".to_owned())].into_iter().collect(),
            items: vec![pim::LocalItem {
                id: ItemId(9),
                remote: Some(mailcraft_model::RemoteRef {
                    url: format!("{base}/c/pat.vcf"),
                    etag: "\"1\"".into(),
                    uid: "pat".into(),
                    synced: 5,
                    ..Default::default()
                }),
                uid: "pat".into(),
                upload: None,
                fingerprint: 5,
            }],
            too_many: false,
        };
        let r = sync_folder(&client, &job, &format!("{base}/c/"));
        t.join().unwrap();
        assert!(r.is_err(), "not synchronised against another server: {r:?}");
    }

    #[test]
    fn a_failure_the_cancel_caused_is_not_recorded() {
        let cancel = Arc::new(AtomicBool::new(false));
        let (base, t) =
            serve_once_and_cancel("HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n", Arc::clone(&cancel));
        let job = DavJob {
            start: String::new(),
            trust: vec![base.clone()],
            held: BTreeMap::new(),
            auth: DavAuth::None,
            token_withheld: false,
            publish_base: format!("{base}/p/"),
            options: ConnectOptions::default(),
            discover_contacts: false,
            discover_tasks: false,
            discover_calendars: false,
            folders: Vec::new(),
            jmap: None,
            publish: vec![PublishJob {
                folder: FolderId(7),
                url: format!("{base}/p/cal.ics"),
                data: b"x".to_vec(),
                fingerprint: 42,
                withdraw: false,
                etag: String::new(),
            }],
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let end = run(AccountId(1), &job, &Events::to(tx), &cancel);
        t.join().unwrap();
        assert_eq!(end.error.as_deref(), Some(CANCELLED));
        assert!(rx.try_iter().all(|(_, e)| !matches!(e, Event::Dav { .. })));
    }

    #[test]
    fn dav_connections_do_not_inherit_the_mail_servers_settings() {
        // Regression: the incoming server's plain-text consent and trusted certificate applied to DAV too.
        let d = mailcraft_model::DavSettings { url: "http://dav.example.com/".into(), ..Default::default() };
        let o = dav_options(&d);
        assert!(!o.allow_plaintext_auth);
        assert!(o.tls.extra_roots_der.is_empty());
        let d = mailcraft_model::DavSettings { allow_plaintext_auth: true, ..d };
        assert!(dav_options(&d).allow_plaintext_auth);
    }

    #[test]
    fn a_folder_without_a_collection_never_takes_one_another_folder_holds() {
        let found = vec![
            ("https://h.example.com/c/a/".to_owned(), "A".to_owned()),
            ("https://h.example.com/c/b/".to_owned(), "B".to_owned()),
        ];
        let mut held = BTreeSet::new();
        assert_eq!(free_collection(&found, &held).as_deref(), Some("https://h.example.com/c/a/"));
        held.insert("https://h.example.com/c/a/".to_owned());
        assert_eq!(free_collection(&found, &held).as_deref(), Some("https://h.example.com/c/b/"));
        held.insert("https://h.example.com/c/b/".to_owned());
        assert_eq!(free_collection(&found, &held), None);
    }
}
