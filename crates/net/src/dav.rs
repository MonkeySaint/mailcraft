//! A WebDAV client (RFC 4918) with the parts of CardDAV (RFC 6352) and CalDAV (RFC 4791) a contacts and
//! tasks synchroniser needs, plus plain WebDAV publishing (PUT of a file into a collection).
//!
//! - **Discovery** ([`DavClient::discover`]): from a server or collection URL, the collection itself when it
//!   already is an address book or calendar; else the principal (`current-user-principal`, RFC 5397), its
//!   `addressbook-home-set` / `calendar-home-set`, and the collections in that home. A bare server URL also
//!   tries the RFC 6764 well-known locations (`/.well-known/carddav`, `/.well-known/caldav`).
//! - **Listing** ([`DavClient::list`]): `PROPFIND` depth 1 for every resource's `getetag`.
//! - **Reading** ([`DavClient::multiget`]): `addressbook-multiget` / `calendar-multiget` `REPORT`s in
//!   batches, falling back to one `GET` per resource when the server refuses the report.
//! - **Writing** ([`DavClient::put`], [`DavClient::delete`]): conditional on the ETag (`If-Match`), or on the
//!   resource not existing yet (`If-None-Match: *`), so a change made on the server meanwhile is never
//!   overwritten blindly (the server answers 412 instead).
//!
//! Every response is size-capped and parsed with the crate's small XML reader (namespace prefixes dropped);
//! hostile answers give errors, never panics.
//!
//! **Credentials stay home.** The account's password or token goes only to the origins the client was told
//! to trust ([`DavClient::trust`]: the configured server and publishing location). A multistatus answer
//! naming resources on another origin has those responses dropped, and an address learned from the server
//! that points elsewhere is never followed with credentials.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine;

use crate::autoconfig::{Element, parse_xml_with};
use crate::error::{NetError, Result};
use crate::http::{HttpClient, HttpResponse, Url, normalize_path, percent_decode};
use crate::transport::{ConnectOptions, deadline_after};

/// Largest WebDAV response read.
pub const MAX_RESPONSE: usize = 32 * 1024 * 1024;
/// Most XML elements in one multistatus answer.
const MAX_XML_ELEMENTS: usize = 500_000;
/// Most resources taken from one collection listing.
pub const MAX_RESOURCES: usize = 20_000;
/// Most collections taken from one home.
pub const MAX_COLLECTIONS: usize = 200;
/// Resources asked for per multiget report.
const MULTIGET_BATCH: usize = 25;
/// Largest single contact or task taken from the server (bigger ones are left out of a download).
pub const MAX_ITEM_DATA: usize = 8 * 1024 * 1024;
/// Most data one [`DavClient::multiget`] call downloads in all.
pub const MAX_FETCH_TOTAL: usize = 256 * 1024 * 1024;
/// Longest one request may take, redirects included (each read also has the connection's own timeout).
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// How requests sign in.
#[derive(Clone, PartialEq, Eq)]
pub enum DavAuth {
    None,
    /// HTTP Basic (RFC 7617) with the account's user name and password.
    Basic {
        user: String,
        password: String,
    },
    /// An OAuth 2.0 access token (RFC 6750).
    Bearer(String),
}

impl std::fmt::Debug for DavAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DavAuth::None => f.write_str("None"),
            DavAuth::Basic { user, .. } => write!(f, "Basic({user}, ***)"),
            DavAuth::Bearer(_) => f.write_str("Bearer(***)"),
        }
    }
}

impl DavAuth {
    fn header(&self) -> Option<String> {
        match self {
            DavAuth::None => None,
            DavAuth::Basic { user, password } => Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
            )),
            DavAuth::Bearer(t) => Some(format!("Bearer {t}")),
        }
    }
}

/// What a discovery looks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    /// CardDAV address books.
    Contacts,
    /// CalDAV calendars (events and task lists).
    Calendars,
}

impl Service {
    fn well_known(self) -> &'static str {
        match self {
            Service::Contacts => "/.well-known/carddav",
            Service::Calendars => "/.well-known/caldav",
        }
    }
}

/// A collection found on the server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Collection {
    /// Absolute URL (ending in `/`).
    pub url: String,
    /// `displayname`, else the last path segment.
    pub name: String,
    pub addressbook: bool,
    pub calendar: bool,
    /// Calendar component types it accepts (`VEVENT`, `VTODO`); empty = not restricted.
    pub components: Vec<String>,
}

impl Collection {
    /// A calendar that takes tasks.
    pub fn holds_tasks(&self) -> bool {
        self.calendar && (self.components.is_empty() || self.components.iter().any(|c| c == "VTODO"))
    }
    /// A calendar that takes events.
    pub fn holds_events(&self) -> bool {
        self.calendar && (self.components.is_empty() || self.components.iter().any(|c| c == "VEVENT"))
    }
}

/// A resource of a collection listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// Absolute URL.
    pub url: String,
    /// Entity tag as the server wrote it (quotes included); empty when the server gave none.
    pub etag: String,
    pub content_type: String,
}

/// A collection's listing ([`DavClient::listing`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// The collection's address the answer came from (after redirects), path without its query.
    pub collection: String,
    pub resources: Vec<Resource>,
    /// Responses left out because they did not name a direct member of the collection. When some were, the
    /// listing may be incomplete: a resource missing from it is not known to be gone.
    pub dropped: usize,
}

/// A downloaded resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    pub url: String,
    pub etag: String,
    pub data: String,
}

/// The condition a write is made under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Precondition {
    /// Only when nothing is at the URL yet (`If-None-Match: *`).
    Create,
    /// Only when the server copy still has this ETag (`If-Match`).
    Match(String),
    /// Unconditionally (publishing a file the client owns).
    Any,
}

/// One `response` of a multistatus answer.
#[derive(Clone, Debug, Default)]
struct MsResponse {
    /// Absolute URL.
    url: String,
    /// The response's own status (a resource that is gone), when given.
    status: Option<u16>,
    /// `prop` elements of the propstats with a 2xx status.
    props: Vec<Element>,
}

impl MsResponse {
    fn prop(&self, name: &str) -> Option<&Element> {
        self.props.iter().find_map(|p| p.child(name))
    }
    fn text(&self, name: &str) -> Option<String> {
        self.prop(name).map(|e| e.text.trim().to_owned())
    }
    fn hrefs(&self, name: &str) -> Vec<String> {
        self.prop(name)
            .map(|e| e.all("href").map(|h| h.text.trim().to_owned()).filter(|h| !h.is_empty()).collect())
            .unwrap_or_default()
    }
    fn resource_types(&self) -> Vec<String> {
        self.prop("resourcetype")
            .map(|e| e.children.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default()
    }
}

/// The status code of an HTTP status line (`HTTP/1.1 200 OK`).
fn status_code(line: &str) -> Option<u16> {
    line.split_whitespace().nth(1)?.parse().ok()
}

/// A path for comparisons: percent-decoded, without a trailing `/`.
fn same_path(a: &str, b: &str) -> bool {
    let norm = |u: &str| -> String {
        let path = Url::parse(u).map(|x| x.path).unwrap_or_else(|_| u.to_owned());
        percent_decode(path.trim_end_matches('/'), false)
    };
    norm(a) == norm(b)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

const PROPFIND_HEAD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav" xmlns:cal="urn:ietf:params:xml:ns:caldav">"#;

fn propfind_body(props: &str) -> String {
    format!("{PROPFIND_HEAD}<d:prop>{props}</d:prop></d:propfind>")
}

/// The origin (`scheme://host[:port]`, lower case, default port left out) of `url`.
pub fn origin_of(url: &str) -> Option<String> {
    Url::parse(url.trim()).ok().map(|u| u.origin().to_ascii_lowercase())
}

/// Blocking WebDAV client.
#[derive(Clone, Debug)]
pub struct DavClient {
    pub http: HttpClient,
    auth: DavAuth,
    /// Origins that receive the credentials.
    trusted: Vec<String>,
    /// Largest single resource [`DavClient::multiget`] keeps.
    pub max_item: usize,
    /// Most data one [`DavClient::multiget`] downloads.
    pub max_total: usize,
    /// Longest one request may take ([`REQUEST_DEADLINE`]).
    pub request_deadline: Duration,
    /// Set from elsewhere to stop: the next request fails at once (a cancelled send/receive).
    cancel: Option<Arc<AtomicBool>>,
}

impl DavClient {
    /// A client that signs in with `auth` only on the origins added with [`DavClient::trust`].
    pub fn new(options: ConnectOptions, auth: DavAuth) -> Self {
        let http = HttpClient { max_body: MAX_RESPONSE, ..HttpClient::new(options) };
        DavClient {
            http,
            auth,
            trusted: Vec::new(),
            max_item: MAX_ITEM_DATA,
            max_total: MAX_FETCH_TOTAL,
            request_deadline: REQUEST_DEADLINE,
            cancel: None,
        }
    }

    /// Stops sending requests once `flag` is set: each request (every multiget batch and single GET
    /// included) checks it first and fails with [`NetError::Timeout`].
    #[must_use]
    pub fn with_cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// True once the cancel flag given to [`DavClient::with_cancel`] is set.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst))
    }

    /// Sends the credentials to `url`'s origin too (ignored when `url` is not an http(s) address).
    #[must_use]
    pub fn trust(mut self, url: &str) -> Self {
        if let Some(o) = origin_of(url)
            && !self.trusted.contains(&o)
        {
            self.trusted.push(o);
        }
        self
    }

    /// True when requests to `url` carry the credentials.
    pub fn is_trusted(&self, url: &str) -> bool {
        origin_of(url).is_some_and(|o| self.trusted.contains(&o))
    }

    /// Sends one request (with the account's credentials when `url` is on a trusted origin), finished within
    /// [`DavClient::request_deadline`]. 401 is [`NetError::Auth`].
    pub fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<HttpResponse> {
        if self.is_cancelled() {
            return Err(NetError::Timeout("Send/Receive was cancelled".into()));
        }
        let auth = if self.is_trusted(url) { self.auth.header() } else { None };
        let mut h: Vec<(&str, &str)> = headers.to_vec();
        if let Some(a) = auth.as_deref() {
            h.push(("Authorization", a));
        }
        let deadline = deadline_after(self.request_deadline);
        let resp = self.http.request_with_deadline(method, url, &h, body, Some(deadline))?;
        // 403 is also how CalDAV and CardDAV refuse data a collection does not take (RFC 4791 section 5.3.2.1):
        // only 401 means the sign-in failed.
        if resp.status == 401 {
            return Err(NetError::Auth(format!("{url} answered HTTP {}", resp.status)));
        }
        Ok(resp)
    }

    /// `PROPFIND` (or `REPORT`) with an XML body; the multistatus answer's responses.
    fn multistatus(&self, method: &str, url: &str, depth: &str, body: &str) -> Result<Vec<MsResponse>> {
        self.multistatus_at(method, url, depth, body).map(|(rs, _)| rs)
    }

    /// [`DavClient::multistatus`], also returning the address the answer came from (after redirects).
    fn multistatus_at(
        &self,
        method: &str,
        url: &str,
        depth: &str,
        body: &str,
    ) -> Result<(Vec<MsResponse>, Url)> {
        let resp = self.request(
            method,
            url,
            &[("Depth", depth), ("Content-Type", "application/xml; charset=utf-8")],
            Some(body.as_bytes()),
        )?;
        if resp.status != 207 {
            return Err(NetError::Http { status: resp.status, body: resp.text_snippet() });
        }
        let base = Url::parse(&resp.url).or_else(|_| Url::parse(url))?;
        let rs = parse_multistatus(&String::from_utf8_lossy(&resp.body), &base)?;
        Ok((rs, base))
    }

    /// Address books (or calendars) reachable from `start`: the collection itself when it is one, else the
    /// collections of the signed-in user's home.
    pub fn discover(&self, start: &str, service: Service) -> Result<Vec<Collection>> {
        let start_url = Url::parse(start)?;
        let mut tried_well_known = false;
        let mut target = start_url.to_string();
        loop {
            match self.discover_at(&target, service) {
                Ok(Some(found)) => return Ok(found),
                Ok(None) => {}
                Err(e @ NetError::Auth(_)) => return Err(e),
                Err(e) if tried_well_known => return Err(e),
                Err(_) => {}
            }
            if tried_well_known {
                return Ok(Vec::new());
            }
            tried_well_known = true;
            target = start_url.join(service.well_known())?.to_string();
        }
    }

    /// One discovery attempt at `url`: `None` when it leads nowhere.
    fn discover_at(&self, url: &str, service: Service) -> Result<Option<Vec<Collection>>> {
        let body = propfind_body(
            "<d:resourcetype/><d:displayname/><d:current-user-principal/><card:addressbook-home-set/>\
             <cal:calendar-home-set/><cal:supported-calendar-component-set/>",
        );
        let rs = self.multistatus("PROPFIND", url, "0", &body)?;
        let Some(me) = rs.iter().find(|r| same_path(&r.url, url)).or(rs.first()) else { return Ok(None) };
        let own = collection_of(me);
        let wanted = |c: &Collection| match service {
            Service::Contacts => c.addressbook,
            Service::Calendars => c.calendar,
        };
        if wanted(&own) {
            return Ok(Some(vec![own]));
        }
        let home_prop = match service {
            Service::Contacts => "addressbook-home-set",
            Service::Calendars => "calendar-home-set",
        };
        let base = Url::parse(&me.url).or_else(|_| Url::parse(url))?;
        let mut homes: Vec<String> = me
            .hrefs(home_prop)
            .iter()
            .filter_map(|h| base.join(h).ok())
            .map(|u| u.to_string())
            .filter(|u| self.is_trusted(u))
            .collect();
        if homes.is_empty() {
            let principal = me.hrefs("current-user-principal").into_iter().next();
            if let Some(p) = principal.and_then(|p| base.join(&p).ok()).map(|u| u.to_string())
                && self.is_trusted(&p)
                && !same_path(&p, url)
            {
                let body = propfind_body(&format!(
                    "<{}/>",
                    match service {
                        Service::Contacts => "card:addressbook-home-set",
                        Service::Calendars => "cal:calendar-home-set",
                    }
                ));
                let pr = self.multistatus("PROPFIND", &p, "0", &body)?;
                if let Some(r) = pr.first() {
                    let pbase = Url::parse(&r.url).unwrap_or_else(|_| base.clone());
                    homes = r
                        .hrefs(home_prop)
                        .iter()
                        .filter_map(|h| pbase.join(h).ok())
                        .map(|u| u.to_string())
                        .filter(|u| self.is_trusted(u))
                        .collect();
                }
            }
        }
        if homes.is_empty() {
            // A plain collection holding the wanted collections (a home given directly).
            if own.url.is_empty() {
                return Ok(None);
            }
            homes.push(own.url.clone());
        }
        let mut found = Vec::new();
        for home in homes.iter().take(4) {
            for c in self.collections(home)? {
                if wanted(&c) && !found.iter().any(|x: &Collection| same_path(&x.url, &c.url)) {
                    found.push(c);
                }
            }
        }
        Ok((!found.is_empty()).then_some(found))
    }

    /// The collections directly inside `home`.
    pub fn collections(&self, home: &str) -> Result<Vec<Collection>> {
        let body = propfind_body("<d:resourcetype/><d:displayname/><cal:supported-calendar-component-set/>");
        let rs = self.multistatus("PROPFIND", home, "1", &body)?;
        Ok(rs
            .iter()
            .filter(|r| !same_path(&r.url, home))
            .map(collection_of)
            .filter(|c| !c.url.is_empty())
            .take(MAX_COLLECTIONS)
            .collect())
    }

    /// Every resource in `collection` with its ETag (sub-collections left out). Only direct members are
    /// taken: an href outside the collection, deeper inside it, or with a `.` or `..` segment is dropped (a
    /// request for it would land on another resource than the one listed).
    pub fn list(&self, collection: &str) -> Result<Vec<Resource>> {
        self.listing(collection).map(|l| l.resources)
    }

    /// [`DavClient::list`], with the address the listing came from and how many responses were left out.
    /// Membership is checked against the address that answered: a collection whose `PROPFIND` was redirected
    /// (a server that moved its DAV root) lists its members under the new path.
    pub fn listing(&self, collection: &str) -> Result<Listing> {
        let body = propfind_body("<d:resourcetype/><d:getetag/><d:getcontenttype/>");
        let (rs, base) = self.multistatus_at("PROPFIND", collection, "1", &body)?;
        let answered = format!("{}{}", base.origin(), base.path.split('?').next().unwrap_or("/"));
        let mut out = Vec::new();
        let mut dropped = 0usize;
        for r in rs {
            if same_path(&r.url, collection) || same_path(&r.url, &answered) {
                continue;
            }
            if r.status.is_some_and(|s| !(200..300).contains(&s)) {
                continue;
            }
            if !is_member(&answered, &r.url) {
                log::warn!("dav: {} is not a resource of {answered}; left out", r.url);
                dropped = dropped.saturating_add(1);
                continue;
            }
            if r.resource_types().iter().any(|t| t == "collection") {
                continue;
            }
            if out.len() >= MAX_RESOURCES {
                return Err(NetError::TooLarge(format!("more than {MAX_RESOURCES} items in {collection}")));
            }
            out.push(Resource {
                etag: r.text("getetag").unwrap_or_default(),
                content_type: r.text("getcontenttype").unwrap_or_default(),
                url: r.url,
            });
        }
        Ok(Listing { collection: answered, resources: out, dropped })
    }

    /// Downloads resources of `collection` (multiget reports, or single GETs when the server refuses
    /// them). Resources gone meanwhile, and any larger than [`DavClient::max_item`], are left out; more than
    /// [`DavClient::max_total`] in all is [`NetError::TooLarge`].
    pub fn multiget(&self, collection: &str, urls: &[String], service: Service) -> Result<Vec<Fetched>> {
        let (root, data) = match service {
            Service::Contacts => ("card:addressbook-multiget", "card:address-data"),
            Service::Calendars => ("cal:calendar-multiget", "cal:calendar-data"),
        };
        let data_name = data.rsplit_once(':').map_or(data, |(_, l)| l);
        let mut out = Vec::new();
        let mut total: usize = 0;
        let mut keep = |f: Fetched, out: &mut Vec<Fetched>| -> Result<()> {
            if f.data.len() > self.max_item {
                log::warn!("dav: {} is larger than {} bytes; left out", f.url, self.max_item);
                return Ok(());
            }
            total = total.saturating_add(f.data.len());
            if total > self.max_total {
                return Err(NetError::TooLarge(format!(
                    "{collection} holds more than {} MiB of items",
                    self.max_total / (1024 * 1024)
                )));
            }
            out.push(f);
            Ok(())
        };
        let mut use_get = false;
        for batch in urls.chunks(MULTIGET_BATCH) {
            if !use_get {
                let hrefs: String = batch
                    .iter()
                    .map(|u| {
                        let path = Url::parse(u).map(|x| x.path).unwrap_or_else(|_| u.clone());
                        format!("<d:href>{}</d:href>", xml_escape(&path))
                    })
                    .collect();
                let body = format!(
                    r#"<?xml version="1.0" encoding="utf-8"?>
<{root} xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav" xmlns:cal="urn:ietf:params:xml:ns:caldav"><d:prop><d:getetag/><{data}/></d:prop>{hrefs}</{root}>"#
                );
                match self.multistatus("REPORT", collection, "1", &body) {
                    Ok(rs) => {
                        for r in rs {
                            if r.status.is_some_and(|s| !(200..300).contains(&s)) {
                                continue;
                            }
                            let Some(d) = r.prop(data_name) else { continue };
                            let f = Fetched {
                                etag: r.text("getetag").unwrap_or_default(),
                                data: d.text.clone(),
                                url: r.url.clone(),
                            };
                            keep(f, &mut out)?;
                        }
                        continue;
                    }
                    Err(NetError::Http { status: 400 | 403 | 405 | 415 | 501, .. }) => {
                        use_get = true;
                    }
                    Err(e) => return Err(e),
                }
            }
            for u in batch {
                match self.get(u) {
                    Ok(f) => keep(f, &mut out)?,
                    Err(NetError::Http { status: 404 | 410, .. }) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(out)
    }

    /// Downloads one resource.
    pub fn get(&self, url: &str) -> Result<Fetched> {
        let resp = self.request("GET", url, &[], None)?;
        if !resp.is_success() {
            return Err(NetError::Http { status: resp.status, body: resp.text_snippet() });
        }
        Ok(Fetched {
            etag: resp.header("etag").unwrap_or_default().to_owned(),
            data: String::from_utf8_lossy(&resp.body).into_owned(),
            url: url.to_owned(),
        })
    }

    /// Writes a resource. Returns its new ETag when the server says it (else `None`: list again to learn
    /// it). A failed precondition is `NetError::Http { status: 412 }`.
    pub fn put(
        &self,
        url: &str,
        data: &[u8],
        content_type: &str,
        pre: &Precondition,
    ) -> Result<Option<String>> {
        let mut h: Vec<(&str, &str)> = vec![("Content-Type", content_type)];
        match pre {
            Precondition::Create => h.push(("If-None-Match", "*")),
            Precondition::Match(e) if !e.is_empty() && !e.contains(['\r', '\n']) => h.push(("If-Match", e)),
            Precondition::Match(_) | Precondition::Any => {}
        }
        let resp = self.request("PUT", url, &h, Some(data))?;
        if !resp.is_success() {
            return Err(NetError::Http { status: resp.status, body: resp.text_snippet() });
        }
        Ok(resp.header("etag").map(str::to_owned).filter(|e| !e.is_empty() && e.len() <= 1024))
    }

    /// The ETag of one resource (`PROPFIND` depth 0); `None` when the server gives none.
    pub fn etag(&self, url: &str) -> Result<Option<String>> {
        let rs = self.multistatus("PROPFIND", url, "0", &propfind_body("<d:getetag/>"))?;
        Ok(rs
            .iter()
            .find(|r| same_path(&r.url, url))
            .and_then(|r| r.text("getetag"))
            .filter(|e| !e.is_empty() && e.len() <= 1024 && !e.contains(['\r', '\n'])))
    }

    /// Removes a resource (only while it still has `etag`, when given). A resource already gone is fine.
    pub fn delete(&self, url: &str, etag: Option<&str>) -> Result<()> {
        let mut h: Vec<(&str, &str)> = Vec::new();
        if let Some(e) = etag.filter(|e| !e.is_empty() && !e.contains(['\r', '\n'])) {
            h.push(("If-Match", e));
        }
        let resp = self.request("DELETE", url, &h, None)?;
        match resp.status {
            200..=299 | 404 | 410 => Ok(()),
            st => Err(NetError::Http { status: st, body: resp.text_snippet() }),
        }
    }
}

/// True when `resource` is a direct member of `collection`: its path (normalised) is the collection's plus one
/// non-empty segment that is not `.` or `..`.
pub fn is_member(collection: &str, resource: &str) -> bool {
    let path = |u: &str| -> Option<String> {
        let p = Url::parse(u).ok()?.path;
        Some(normalize_path(p.split('?').next().unwrap_or("/")))
    };
    let (Some(c), Some(r)) = (path(collection), path(resource)) else { return false };
    let c = if c.ends_with('/') { c } else { format!("{c}/") };
    let Some(name) = r.strip_prefix(&c) else { return false };
    !name.is_empty() && !name.contains('/') && name != "." && name != ".."
}

/// A collection from a PROPFIND response (`url` empty when the response is not a collection).
fn collection_of(r: &MsResponse) -> Collection {
    let types = r.resource_types();
    if !types.iter().any(|t| t == "collection") {
        return Collection::default();
    }
    let mut url = r.url.clone();
    if !url.ends_with('/') {
        url.push('/');
    }
    let components: Vec<String> = r
        .prop("supported-calendar-component-set")
        .map(|e| {
            e.all("comp")
                .filter_map(|c| c.attr("name"))
                .map(|n| n.trim().to_ascii_uppercase())
                .filter(|n| !n.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let name = r
        .text("displayname")
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            let path = Url::parse(&url).map(|u| u.path).unwrap_or_default();
            percent_decode(path.trim_end_matches('/').rsplit('/').next().unwrap_or_default(), false)
        })
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    Collection {
        url,
        name,
        addressbook: types.iter().any(|t| t == "addressbook"),
        calendar: types.iter().any(|t| t == "calendar"),
        components,
    }
}

/// The responses of a `multistatus` document, hrefs resolved against `base`. Responses naming a resource
/// on another origin are dropped (a server cannot point the client's credentials elsewhere).
fn parse_multistatus(text: &str, base: &Url) -> Result<Vec<MsResponse>> {
    let home = base.origin().to_ascii_lowercase();
    let root = parse_xml_with(text, MAX_RESPONSE, MAX_XML_ELEMENTS)?;
    if root.name != "multistatus" {
        return Err(NetError::Protocol(format!("expected a WebDAV multistatus, got <{}>", root.name)));
    }
    let mut out = Vec::new();
    for r in root.all("response") {
        let Some(href) = r.child_text("href").filter(|h| !h.is_empty()) else { continue };
        let Ok(url) = base.join(href) else { continue };
        if url.origin().to_ascii_lowercase() != home {
            continue;
        }
        let props = r
            .all("propstat")
            .filter(|ps| {
                ps.child_text("status").and_then(status_code).is_none_or(|s| (200..300).contains(&s))
            })
            .filter_map(|ps| ps.child("prop").cloned())
            .collect();
        out.push(MsResponse {
            url: url.to_string(),
            status: r.child_text("status").and_then(status_code),
            props,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::testserver::serve;

    fn ms(status: u16, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/xml\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn client(base: &str) -> DavClient {
        DavClient::new(
            ConnectOptions { allow_plaintext_auth: true, ..ConnectOptions::default() },
            DavAuth::Basic { user: "me@example.com".into(), password: "pw".into() },
        )
        .trust(base)
    }

    #[test]
    fn multistatus_parsing_keeps_ok_props_only() {
        let base = Url::parse("https://dav.example.com/dav/me/").unwrap();
        let doc = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">
          <d:response><d:href>/dav/me/contacts/</d:href>
            <d:propstat><d:prop><d:resourcetype><d:collection/><c:addressbook xmlns:c="urn:ietf:params:xml:ns:carddav"/></d:resourcetype><d:displayname>Friends &amp; Co</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
            <d:propstat><d:prop><d:getetag/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
          </d:response>
          <d:response><d:href>gone.vcf</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
        </d:multistatus>"#;
        let rs = parse_multistatus(doc, &base).unwrap();
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].url, "https://dav.example.com/dav/me/contacts/");
        assert!(rs[0].prop("getetag").is_none(), "a 404 propstat is ignored");
        let c = collection_of(&rs[0]);
        assert!(c.addressbook && !c.calendar);
        assert_eq!(c.name, "Friends & Co");
        assert_eq!(rs[1].url, "https://dav.example.com/dav/me/gone.vcf");
        assert_eq!(rs[1].status, Some(404));
        assert!(parse_multistatus("<html/>", &base).is_err());
        assert!(parse_multistatus("<d:multistatus", &base).is_err());
    }

    #[test]
    fn task_and_event_calendars() {
        let mut c = Collection { calendar: true, ..Default::default() };
        assert!(c.holds_tasks() && c.holds_events());
        c.components = vec!["VTODO".into()];
        assert!(c.holds_tasks() && !c.holds_events());
        c.calendar = false;
        assert!(!c.holds_tasks());
    }

    #[test]
    fn discovery_follows_principal_and_home() {
        let (base, t, rx) = serve(3, |req| match (req.method.as_str(), req.path.as_str()) {
            ("PROPFIND", "/") => ms(
                207,
                r#"<multistatus xmlns="DAV:"><response><href>/</href><propstat><prop><resourcetype><collection/></resourcetype><current-user-principal><href>/p/me/</href></current-user-principal></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#,
            ),
            ("PROPFIND", "/p/me/") => ms(
                207,
                r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><response><href>/p/me/</href><propstat><prop><C:calendar-home-set><href>/cal/me/</href></C:calendar-home-set></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#,
            ),
            ("PROPFIND", "/cal/me/") => ms(
                207,
                r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
                    <response><href>/cal/me/</href><propstat><prop><resourcetype><collection/></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat></response>
                    <response><href>/cal/me/work/</href><propstat><prop><resourcetype><collection/><C:calendar/></resourcetype><displayname>Work</displayname><C:supported-calendar-component-set><C:comp name="VEVENT"/></C:supported-calendar-component-set></prop><status>HTTP/1.1 200 OK</status></propstat></response>
                    <response><href>/cal/me/todo/</href><propstat><prop><resourcetype><collection/><C:calendar/></resourcetype><C:supported-calendar-component-set><C:comp name="VTODO"/></C:supported-calendar-component-set></prop><status>HTTP/1.1 200 OK</status></propstat></response>
                    </multistatus>"#,
            ),
            _ => ms(404, ""),
        });
        let found = client(&base).discover(&format!("{base}/"), Service::Calendars).unwrap();
        t.join().unwrap();
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Work", "todo"]);
        assert!(found[1].holds_tasks() && !found[0].holds_tasks());
        let seen: Vec<_> = rx.try_iter().collect();
        assert_eq!(seen[0].header("depth"), Some("0"));
        assert!(seen[0].header("authorization").unwrap().starts_with("Basic "));
        assert_eq!(seen[2].header("depth"), Some("1"));
    }

    #[test]
    fn conditional_writes_and_refused_reports() {
        let (base, t, rx) = serve(5, |req| match req.method.as_str() {
            "PUT" if req.header("if-none-match") == Some("*") => {
                b"HTTP/1.1 201 Created\r\nETag: \"e1\"\r\nContent-Length: 0\r\n\r\n".to_vec()
            }
            "PUT" => b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\n\r\n".to_vec(),
            "REPORT" => b"HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\n\r\n".to_vec(),
            "GET" => b"HTTP/1.1 200 OK\r\nETag: \"e9\"\r\nContent-Length: 4\r\n\r\ndata".to_vec(),
            _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
        });
        let c = client(&base);
        let url = format!("{base}/c/a.vcf");
        assert_eq!(
            c.put(&url, b"x", "text/vcard", &Precondition::Create).unwrap().as_deref(),
            Some("\"e1\"")
        );
        let e = c.put(&url, b"x", "text/vcard", &Precondition::Match("\"e0\"".into())).unwrap_err();
        assert!(matches!(e, NetError::Http { status: 412, .. }));
        let got = c.multiget(&format!("{base}/c/"), std::slice::from_ref(&url), Service::Contacts).unwrap();
        assert_eq!(got, vec![Fetched { url: url.clone(), etag: "\"e9\"".into(), data: "data".into() }]);
        c.delete(&url, Some("\"e9\"")).unwrap();
        t.join().unwrap();
        let seen: Vec<_> = rx.try_iter().collect();
        assert_eq!(seen[1].header("if-match"), Some("\"e0\""));
        assert_eq!(seen[2].method, "REPORT");
        assert!(String::from_utf8_lossy(&seen[2].body).contains("<d:href>/c/a.vcf</d:href>"));
        assert_eq!(seen[4].header("if-match"), Some("\"e9\""));
    }

    #[test]
    fn auth_failure_is_an_auth_error() {
        let (base, t, _rx) = serve(1, |_| b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_vec());
        let e = client(&base).list(&format!("{base}/c/")).unwrap_err();
        t.join().unwrap();
        assert!(matches!(e, NetError::Auth(_)), "{e:?}");
    }

    #[test]
    fn credentials_go_only_to_trusted_origins() {
        let (base, t, rx) = serve(2, |_| b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec());
        // Trusted for another origin only (a different port is a different origin).
        let other = client("http://127.0.0.1:1/");
        assert!(!other.is_trusted(&base));
        let _ = other.get(&format!("{base}/x.ics"));
        let _ = client(&base).get(&format!("{base}/x.ics"));
        t.join().unwrap();
        let seen: Vec<_> = rx.try_iter().collect();
        assert_eq!(seen[0].header("authorization"), None, "an untrusted origin gets no password");
        assert!(seen[1].header("authorization").is_some());
        assert_eq!(origin_of("HTTPS://Dav.Example.com:443/a/"), Some("https://dav.example.com".into()));
        assert_eq!(origin_of("not a url"), None);
    }

    #[test]
    fn multistatus_drops_responses_on_other_origins() {
        let base = Url::parse("https://dav.example.com/c/").unwrap();
        let doc = r#"<d:multistatus xmlns:d="DAV:">
          <d:response><d:href>/c/a.vcf</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop></d:propstat></d:response>
          <d:response><d:href>https://elsewhere.example.net/c/b.vcf</d:href><d:propstat><d:prop><d:getetag>"2"</d:getetag></d:prop></d:propstat></d:response>
          <d:response><d:href>https://dav.example.com/c/c.vcf</d:href><d:propstat><d:prop><d:getetag>"3"</d:getetag></d:prop></d:propstat></d:response>
        </d:multistatus>"#;
        let urls: Vec<String> = parse_multistatus(doc, &base).unwrap().into_iter().map(|r| r.url).collect();
        assert_eq!(urls, ["https://dav.example.com/c/a.vcf", "https://dav.example.com/c/c.vcf"]);
    }

    #[test]
    fn multiget_caps_item_and_total_size() {
        let card = |n: usize| {
            let body = "x".repeat(n);
            format!("HTTP/1.1 200 OK\r\nETag: \"e\"\r\nContent-Length: {}\r\n\r\n{body}", body.len())
                .into_bytes()
        };
        let (base, t, _rx) = serve(4, move |req| match (req.method.as_str(), req.path.as_str()) {
            ("REPORT", _) => b"HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\n\r\n".to_vec(),
            (_, "/c/big.vcf") => card(200),
            _ => card(60),
        });
        let mut c = client(&base);
        c.max_item = 100;
        c.max_total = 100;
        let urls: Vec<String> = ["big", "a", "b"].iter().map(|n| format!("{base}/c/{n}.vcf")).collect();
        // The oversized item is left out; the second normal one goes over the total.
        let e = c.multiget(&format!("{base}/c/"), &urls, Service::Contacts).unwrap_err();
        t.join().unwrap();
        assert!(matches!(e, NetError::TooLarge(_)), "{e:?}");
    }

    #[test]
    fn listings_keep_direct_members_only() {
        // Regression: an href such as `/c/%2E%2E%2Fother%2Fx.vcf` was decoded and requested as
        // `/c/../other/x.vcf`, a resource outside the collection.
        let body = r#"<d:multistatus xmlns:d="DAV:">
          <d:response><d:href>/c/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
          <d:response><d:href>/c/a.vcf</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop></d:propstat></d:response>
          <d:response><d:href>/c/a%2Fb.vcf</d:href><d:propstat><d:prop><d:getetag>"2"</d:getetag></d:prop></d:propstat></d:response>
          <d:response><d:href>/c/../other/x.vcf</d:href><d:propstat><d:prop><d:getetag>"3"</d:getetag></d:prop></d:propstat></d:response>
          <d:response><d:href>/c/%2E%2E</d:href><d:propstat><d:prop><d:getetag>"4"</d:getetag></d:prop></d:propstat></d:response>
          <d:response><d:href>/c/sub/x.vcf</d:href><d:propstat><d:prop><d:getetag>"5"</d:getetag></d:prop></d:propstat></d:response>
          <d:response><d:href>/other/y.vcf</d:href><d:propstat><d:prop><d:getetag>"6"</d:getetag></d:prop></d:propstat></d:response>
        </d:multistatus>"#
            .to_owned();
        let (base, t, _rx) = serve(1, move |_| ms(207, &body));
        let got = client(&base).list(&format!("{base}/c/")).unwrap();
        t.join().unwrap();
        let etags: Vec<&str> = got.iter().map(|r| r.etag.as_str()).collect();
        assert_eq!(etags, ["\"1\"", "\"2\""], "{got:?}");
        assert!(got[1].url.ends_with("/c/a%2Fb.vcf"), "the escape is kept: {}", got[1].url);
        assert!(is_member("https://h.example.com/c", "https://h.example.com/c/x%40y.vcf"));
        assert!(!is_member("https://h.example.com/c/", "https://h.example.com/c/."));
        assert!(!is_member("https://h.example.com/c/", "https://h.example.com/c/"));
        assert!(!is_member("https://h.example.com/c/", "https://h.example.com/cx/a.vcf"));
    }

    #[test]
    fn a_redirected_listing_keeps_the_members_at_the_new_address() {
        // Regression: membership was checked against the requested path, so a collection whose PROPFIND was
        // redirected (a server that moved its DAV root) listed nothing, and every item looked deleted.
        let body = r#"<d:multistatus xmlns:d="DAV:">
          <d:response><d:href>/new/c/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
          <d:response><d:href>/new/c/a.vcf</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop></d:propstat></d:response>
          <d:response><d:href>/other/x.vcf</d:href><d:propstat><d:prop><d:getetag>"2"</d:getetag></d:prop></d:propstat></d:response>
        </d:multistatus>"#
            .to_owned();
        let (base, t, rx) = serve(2, move |seen| {
            if seen.path.starts_with("/old/") {
                b"HTTP/1.1 301 Moved\r\nLocation: /new/c/\r\nContent-Length: 0\r\n\r\n".to_vec()
            } else {
                ms(207, &body)
            }
        });
        let l = client(&base).listing(&format!("{base}/old/c/")).unwrap();
        t.join().unwrap();
        assert_eq!(l.collection, format!("{base}/new/c/"));
        let urls: Vec<&str> = l.resources.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, [format!("{base}/new/c/a.vcf")], "{l:?}");
        assert_eq!(l.dropped, 1, "the href outside the collection is counted");
        let seen: Vec<String> = rx.try_iter().map(|s| s.path).collect();
        assert_eq!(seen, ["/old/c/", "/new/c/"]);
    }

    #[test]
    fn a_cancelled_client_sends_nothing() {
        let flag = Arc::new(AtomicBool::new(true));
        // Nothing listens on port 1: a request that got through would fail differently.
        let c = client("http://127.0.0.1:1/").with_cancel(Arc::clone(&flag));
        assert!(c.is_cancelled());
        let e = c.list("http://127.0.0.1:1/c/").unwrap_err();
        assert!(matches!(e, NetError::Timeout(ref m) if m.contains("cancelled")), "{e:?}");
        let e =
            c.multiget("http://127.0.0.1:1/c/", &["http://127.0.0.1:1/c/a.vcf".into()], Service::Contacts);
        assert!(matches!(e, Err(NetError::Timeout(_))), "{e:?}");
        assert_eq!(
            DavClient::new(ConnectOptions::default(), DavAuth::None).request_deadline,
            REQUEST_DEADLINE
        );
    }

    #[test]
    fn a_resource_etag_comes_from_a_depth_0_propfind() {
        let (base, t, rx) = serve(1, |_| {
            ms(
                207,
                r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/p/cal.ics</d:href><d:propstat><d:prop><d:getetag>"e5"</d:getetag></d:prop></d:propstat></d:response></d:multistatus>"#,
            )
        });
        assert_eq!(client(&base).etag(&format!("{base}/p/cal.ics")).unwrap().as_deref(), Some("\"e5\""));
        t.join().unwrap();
        let seen: Vec<_> = rx.try_iter().collect();
        assert_eq!((seen[0].method.as_str(), seen[0].header("depth")), ("PROPFIND", Some("0")));
    }
}
