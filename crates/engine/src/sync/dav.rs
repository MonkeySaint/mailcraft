//! Native side of contacts, tasks and calendar synchronisation (CardDAV / CalDAV) and calendar publishing: the DAV
//! part of a send/receive job is built on the session thread ([`build_job`]) and run on the account's worker
//! thread after its mail ([`run`]); results come back as [`PimEvent`]s (see `pim.rs` for the rules).
//!
//! Resource URLs are compared in one canonical form (scheme, host, port and the percent-decoded path), so a
//! server that writes `%40` where the client wrote `@` does not make an item look deleted.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mailcraft_model::account::CredentialKind;
use mailcraft_model::{Account, AccountId, FolderId, FolderKind, ItemId, RemoteRef};
use mailcraft_net::dav::{DavAuth, DavClient, Precondition, Service};
use mailcraft_net::http::{Url, percent_decode, percent_encode};
use mailcraft_net::{ConnectOptions, NetError};

use super::Plan;
use super::native::{Event, Events, connect_options};
use super::pim::{self, FolderJob, FolderOutcome, PimEvent, PimKind, PublishJob};
use crate::Session;
use crate::accounts::account_secret;

/// The DAV part of a send/receive job.
#[derive(Clone, Debug)]
pub(super) struct DavJob {
    /// Where discovery starts (the account's DAV URL).
    start: String,
    /// Addresses whose origins get the credentials (the DAV server and the publishing location).
    trust: Vec<String>,
    /// Collections (canonical) mapped to folders: a folder still looking for one never takes these.
    held: BTreeSet<String>,
    auth: DavAuth,
    options: ConnectOptions,
    /// Look for address books / task lists / calendars (a full run, or a folder not mapped yet).
    discover_contacts: bool,
    discover_tasks: bool,
    discover_calendars: bool,
    folders: Vec<FolderJob>,
    publish: Vec<PublishJob>,
}

impl DavJob {
    pub(super) fn is_empty(&self) -> bool {
        self.folders.is_empty()
            && self.publish.is_empty()
            && !self.discover_contacts
            && !self.discover_tasks
            && !self.discover_calendars
    }
}

/// Identity form of a resource or collection URL: origin plus the percent-decoded path.
pub(super) fn canon(url: &str) -> String {
    match Url::parse(url) {
        Ok(u) => format!(
            "{}{}",
            u.origin().to_ascii_lowercase(),
            percent_decode(u.path.split('?').next().unwrap_or("/"), false)
        ),
        Err(_) => url.trim().to_owned(),
    }
}

/// Request form of a canonical URL: every path segment percent-encoded.
fn wire(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_owned() };
    let (host, path) = match rest.find('/') {
        Some(i) => (rest.get(..i).unwrap_or_default(), rest.get(i..).unwrap_or("/")),
        None => (rest, "/"),
    };
    let path: Vec<String> = path.split('/').map(percent_encode).collect();
    format!("{scheme}://{host}{}", path.join("/"))
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
            });
        }
        let Some(p) = all.publication else { continue };
        if p.account != account || p.url.trim().is_empty() {
            continue;
        }
        if p.withdraw {
            out.push(PublishJob {
                folder: f.id,
                url: p.url.clone(),
                data: Vec::new(),
                fingerprint: 0,
                withdraw: true,
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
            });
        }
    }
    out
}

/// The DAV part of `a`'s run for `plan` (`None` when there is nothing to do).
pub(super) fn build_job(s: &Session, a: &Account, plan: &Plan) -> Option<DavJob> {
    let folders = pim::plan_folders(s, a.id, plan);
    let publish = publish_jobs(s, a.id, plan);
    let dav = a.dav.clone().unwrap_or_default();
    let full = plan.receive && plan.folders.is_none();
    let unmapped = |k: PimKind| folders.iter().any(|f| f.kind == k && f.collection.is_none());
    let start = dav.url.trim().to_owned();
    let discover_contacts = !start.is_empty() && dav.contacts && (full || unmapped(PimKind::Contacts));
    let discover_tasks = !start.is_empty() && dav.tasks && (full || unmapped(PimKind::Tasks));
    let discover_calendars = !start.is_empty() && dav.calendar && (full || unmapped(PimKind::Calendar));
    if folders.is_empty()
        && publish.is_empty()
        && !discover_contacts
        && !discover_tasks
        && !discover_calendars
    {
        return None;
    }
    let inc = a.incoming.as_ref();
    let user = [dav.username.trim(), inc.map(|i| i.username.trim()).unwrap_or_default(), a.address.trim()]
        .into_iter()
        .find(|u| !u.is_empty())
        .unwrap_or_default()
        .to_owned();
    let auth = match account_secret(s, a.id, false) {
        Some(secret) if a.credential == CredentialKind::OAuthToken => DavAuth::Bearer(secret),
        Some(password) => DavAuth::Basic { user, password },
        None => DavAuth::None,
    };
    let options = inc.and_then(|i| connect_options(i).ok()).unwrap_or_else(|| ConnectOptions {
        connect_timeout: Duration::from_secs(20),
        io_timeout: Duration::from_secs(60),
        ..ConnectOptions::default()
    });
    let trust: Vec<String> = [dav.url.trim(), dav.publish_url.trim()]
        .into_iter()
        .filter(|u| !u.is_empty())
        .map(str::to_owned)
        .collect();
    let held: BTreeSet<String> = a.sync.dav.values().map(|st| canon(&st.collection)).collect();
    Some(DavJob {
        start,
        trust,
        held,
        auth,
        options,
        discover_contacts,
        discover_tasks,
        discover_calendars,
        folders,
        publish,
    })
}

/// Errors that end the DAV part of a run (the others only affect one item).
fn fatal(e: &NetError) -> bool {
    e.is_connection_lost() || matches!(e, NetError::Auth(_) | NetError::Connect(_) | NetError::Insecure(_))
}

fn user_error(e: &NetError) -> String {
    match e {
        NetError::Auth(_) => "the contacts and tasks server did not accept the user name or password".into(),
        other => other.to_string(),
    }
}

/// Runs the DAV part of a job. Returns the error that stopped it, if any.
pub(super) fn run(account: AccountId, job: &DavJob, events: &Events, cancel: &AtomicBool) -> Option<String> {
    let send = |ev: PimEvent| {
        if !events.send(Event::Dav { account, ev: Box::new(ev) }) {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let progress = |t: &str| {
        events.send(Event::step(account, t));
    };
    let cancelled = || cancel.load(Ordering::SeqCst);
    let client =
        job.trust.iter().fold(DavClient::new(job.options.clone(), job.auth.clone()), |c, u| c.trust(u));
    let mut error: Option<String> = None;
    let mut found: BTreeMap<&'static str, Vec<(String, String)>> = BTreeMap::new();
    if job.discover_contacts || job.discover_tasks || job.discover_calendars {
        progress("Looking for address books, task lists and calendars…");
        let mut contacts = None;
        let mut tasks = None;
        let mut calendars = None;
        if job.discover_contacts {
            match client.discover(&wire(&job.start), Service::Contacts) {
                Ok(list) => {
                    let l: Vec<(String, String)> =
                        list.into_iter().filter(|c| c.addressbook).map(|c| (canon(&c.url), c.name)).collect();
                    found.insert("contacts", l.clone());
                    contacts = Some(l);
                }
                Err(e) if fatal(&e) => return Some(user_error(&e)),
                Err(e) => error = Some(format!("address books: {}", user_error(&e))),
            }
        }
        if (job.discover_tasks || job.discover_calendars) && !cancelled() {
            match client.discover(&wire(&job.start), Service::Calendars) {
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
                Err(e) if fatal(&e) => return Some(user_error(&e)),
                Err(e) => error = Some(format!("calendars and task lists: {}", user_error(&e))),
            }
        }
        send(PimEvent::Collections { contacts, tasks, calendars });
    }
    // Collections taken by a folder of this run that had none yet.
    let mut held = job.held.clone();
    for f in &job.folders {
        if cancelled() {
            return Some("Send/Receive was cancelled.".into());
        }
        if f.too_many {
            let what = pim::too_many_error();
            send(PimEvent::FolderFailed { folder: f.folder, error: what.clone() });
            error.get_or_insert(what);
            continue;
        }
        let collection = match &f.collection {
            Some(c) => Some(c.clone()),
            None => {
                let k = match f.kind {
                    PimKind::Contacts => "contacts",
                    PimKind::Tasks => "tasks",
                    PimKind::Calendar => "calendars",
                };
                let free = found.get(k).and_then(|l| free_collection(l, &held));
                if let Some(c) = &free {
                    held.insert(c.clone());
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
        match sync_folder(&client, f, &collection) {
            Ok(out) => send(PimEvent::Folder(Box::new(out))),
            Err(e) => {
                let msg = user_error(&e);
                send(PimEvent::FolderFailed { folder: f.folder, error: msg.clone() });
                if fatal(&e) {
                    return Some(msg);
                }
                error.get_or_insert(msg);
            }
        }
    }
    for p in &job.publish {
        if cancelled() {
            return Some("Send/Receive was cancelled.".into());
        }
        progress("Publishing calendars…");
        let url = wire(&canon(&p.url));
        let result = if p.withdraw {
            client.delete(&url, None)
        } else {
            client.put(&url, &p.data, "text/calendar; charset=utf-8", &Precondition::Any).map(|_| ())
        };
        let result = result.map_err(|e| user_error(&e));
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
    }
    error
}

/// The first discovered collection no folder holds (a folder without one takes it).
fn free_collection(found: &[(String, String)], held: &BTreeSet<String>) -> Option<String> {
    found.iter().map(|(u, _)| u).find(|u| !held.contains(*u)).cloned()
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
fn sync_folder(client: &DavClient, job: &FolderJob, collection: &str) -> Result<FolderOutcome, NetError> {
    let kind = job.kind;
    let service = match kind {
        PimKind::Contacts => Service::Contacts,
        PimKind::Tasks | PimKind::Calendar => Service::Calendars,
    };
    let coll_wire = wire(collection);
    let listing = client.list(&coll_wire)?;
    let server: BTreeMap<String, String> = listing.into_iter().map(|r| (canon(&r.url), r.etag)).collect();
    let known_before: BTreeMap<String, String> =
        job.known.iter().map(|(u, e)| (canon(u), e.clone())).collect();
    let mut out = FolderOutcome {
        folder: job.folder,
        kind: Some(kind),
        collection: collection.to_owned(),
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
        match client.put(&wire(url), body.as_bytes(), kind.content_type(), &pre) {
            Ok(etag) => Ok(Put::Done(etag.unwrap_or_default())),
            Err(e) if fatal(&e) => Err(e),
            Err(NetError::Http { status: 412, .. }) => Ok(Put::Conflict),
            Err(e) => {
                out.errors.push(format!("{url}: {e}"));
                Ok(Put::Failed)
            }
        }
    };
    let uploaded = |item: &pim::LocalItem, url: &str, etag: String| RemoteRef {
        url: url.to_owned(),
        etag,
        uid: item.uid.clone(),
        synced: item.fingerprint,
    };
    // Items new here go up after the server's unknown copies are read: one of those may be the same item
    // (its link was lost with a settings change, or another device uploaded it), and is then taken instead
    // of duplicated.
    let mut new_here: Vec<(&pim::LocalItem, String)> = Vec::new();
    for item in &job.items {
        let url = match &item.remote {
            Some(r) => canon(&r.url),
            None => canon(&pim::resource_url(collection, &item.uid, kind)),
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
                match put(&url, &item.body, pre, &mut out)? {
                    Put::Done(etag) => {
                        relist |= etag.is_empty();
                        known.insert(url.clone(), etag.clone());
                        out.uploaded.push((item.id, uploaded(item, &url, etag)));
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
                    Some(_) => {}
                    None => out.removed.push((item.id, item.fingerprint)),
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
            match client.delete(&wire(url), Some(etag)) {
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
    let mut conflicts: Vec<(Option<(ItemId, u64)>, String)> = Vec::new();
    for (item, url) in new_here {
        // The same resource, the same UID, or (a contact) the same card.
        let same = unknown.iter().position(|u| !u.taken && u.url == url).or_else(|| {
            unknown.iter().position(|u| {
                !u.taken
                    && ((!item.uid.is_empty() && u.uid == item.uid)
                        || u.fingerprint == Some(item.fingerprint))
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
        match put(&url, &item.body, Precondition::Create, &mut out)? {
            Put::Done(etag) => {
                relist |= etag.is_empty();
                known.insert(url.clone(), etag.clone());
                out.uploaded.push((item.id, uploaded(item, &url, etag)));
            }
            // Something already lives at that name (the same item from another device): take it.
            Put::Conflict => conflicts.push((Some((item.id, item.fingerprint)), url.clone())),
            Put::Failed => {}
        }
    }
    for u in unknown.into_iter().filter(|u| !u.taken) {
        out.incoming.push(pim::Incoming { local: None, url: u.url, etag: u.etag, data: u.data });
    }
    download(client, &coll_wire, service, &conflicts, &mut known, |local, url, etag, data| {
        out.incoming.push(pim::Incoming { local, url, etag, data })
    })?;
    if relist {
        for r in client.list(&coll_wire)? {
            let url = canon(&r.url);
            if let Some((_, rr)) = out.uploaded.iter_mut().find(|(_, x)| x.url == url && x.etag.is_empty()) {
                rr.etag = r.etag.clone();
            }
            known.insert(url, r.etag);
        }
    }
    out.known = known;
    Ok(out)
}

/// Downloads `fetch` (local item, canonical URL) and hands each copy to `take` with its ETag. A copy that
/// did not come back (gone meanwhile, or too large) is dropped from `known`, so the next run does not take
/// it for an item removed here.
fn download(
    client: &DavClient,
    coll_wire: &str,
    service: Service,
    fetch: &[(Option<(ItemId, u64)>, String)],
    known: &mut BTreeMap<String, String>,
    mut take: impl FnMut(Option<(ItemId, u64)>, String, String, String),
) -> Result<(), NetError> {
    if fetch.is_empty() {
        return Ok(());
    }
    let urls: Vec<String> = fetch.iter().map(|(_, u)| wire(u)).collect();
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
        assert_eq!(wire("https://h.example.com/dav/a b/x.vcf"), "https://h.example.com/dav/a%20b/x.vcf");
        assert_eq!(wire(&canon("https://h.example.com/c/")), "https://h.example.com/c/");
        assert_eq!(canon("not a url"), "not a url");
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
