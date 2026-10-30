//! A WebDAV server with the CardDAV (RFC 6352) and CalDAV (RFC 4791) subset a contacts, tasks and calendar
//! publishing client uses, on the web server's ports (HTTP and HTTPS, 127.0.0.1 only):
//!
//! - `/.well-known/carddav` and `/.well-known/caldav` redirect (301) to `/dav/` (RFC 6764);
//! - `PROPFIND /dav/` names the signed-in user's principal, `/dav/<user>/`, which is also the address book and
//!   calendar home (`addressbook-home-set`, `calendar-home-set`);
//! - every user has the collections `contacts` (address book), `calendar` (events), `tasks` (calendar of
//!   VTODOs only) and `public` (a plain WebDAV collection for published files, readable without signing in);
//! - `PROPFIND` depth 0/1 (resource type, display name, supported components, ETags, content types),
//!   `addressbook-multiget` / `calendar-multiget` `REPORT`s, and `GET`, `PUT`, `DELETE` of resources with
//!   `If-Match` / `If-None-Match: *` preconditions (412 when they fail).
//!
//! Requests sign in with HTTP Basic (the user's password) or a Bearer token like the JMAP server. Users may
//! only touch their own collections. Everything is in memory; tests read and change it through
//! [`crate::TestServers`].

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use crate::conn::Conn;
use crate::jmap::{Request, authenticate};
use crate::server::Ctx;

/// Largest resource accepted by PUT.
pub const MAX_RESOURCE: usize = 4 * 1024 * 1024;
/// Most resources per collection.
const MAX_ITEMS: usize = 50_000;

/// What a collection holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    AddressBook,
    /// A calendar taking these component types.
    Calendar(&'static [&'static str]),
    /// A plain WebDAV collection (published files).
    Plain,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub data: Vec<u8>,
    pub etag: String,
    pub content_type: String,
}

#[derive(Clone, Debug)]
pub struct Collection {
    pub kind: Kind,
    pub display: String,
    pub items: BTreeMap<String, Item>,
}

/// Every user's collections, created on first use.
#[derive(Debug, Default)]
pub struct DavState {
    /// (user key, collection name) → collection.
    pub collections: BTreeMap<(String, String), Collection>,
    next_etag: u64,
    /// "METHOD path" of every DAV request, oldest first (capped).
    pub requests: Vec<String>,
    /// Answer PUT without an ETag (allowed by RFC 4791 / 6352: the client lists again to learn it).
    pub put_without_etag: bool,
    /// Stored right after the next successful PUT, as if another client wrote it meanwhile: (user,
    /// collection, name, data).
    pub after_next_put: Option<(String, String, String, String)>,
    /// Collections moved elsewhere: (user key, old name) → new name. Requests for the old one are redirected
    /// (301) to the new one.
    pub moved: BTreeMap<(String, String), String>,
    /// The next signed-in request whose path starts with this is refused (401), as if the server had revoked
    /// the sign-in on its side only.
    pub refuse_once: Option<String>,
    /// An href added to every collection listing (a response naming something that is not a member).
    pub stray_href: Option<String>,
    /// Users get one calendar for events and tasks (`calendar`, VEVENT and VTODO) and no `tasks` collection,
    /// as servers that give each user one default calendar.
    pub single_calendar: bool,
    /// A new resource is stored under a name the server picks (`srv-<n>.<ext>`), not the one PUT named, as
    /// some servers do (the client learns the name from the next listing).
    pub rename_new: bool,
    /// Every resource written gets this ETag (a version counter that starts the same for each new
    /// resource, as some servers send): ETags are then unique per resource only, not across a collection.
    pub constant_etag: Option<String>,
}

pub type SharedDav = Arc<Mutex<DavState>>;

/// The default collections of a user: (name, kind, display name).
pub const DEFAULT_COLLECTIONS: [(&str, Kind, &str); 4] = [
    ("contacts", Kind::AddressBook, "Contacts"),
    ("calendar", Kind::Calendar(&["VEVENT"]), "Calendar"),
    ("tasks", Kind::Calendar(&["VTODO"]), "Tasks"),
    ("public", Kind::Plain, "Published"),
];

impl DavState {
    fn key(user: &str) -> String {
        user.trim().to_ascii_lowercase()
    }

    /// Creates the default collections of `user` when missing.
    pub fn ensure(&mut self, user: &str) {
        for (name, kind, display) in DEFAULT_COLLECTIONS {
            if self.single_calendar && name == "tasks" {
                continue;
            }
            let kind = if self.single_calendar && name == "calendar" {
                Kind::Calendar(&["VEVENT", "VTODO"])
            } else {
                kind
            };
            self.collections.entry((Self::key(user), name.to_owned())).or_insert_with(|| Collection {
                kind,
                display: display.to_owned(),
                items: BTreeMap::new(),
            });
        }
    }

    pub fn collection(&self, user: &str, name: &str) -> Option<&Collection> {
        self.collections.get(&(Self::key(user), name.to_owned()))
    }

    pub fn collection_mut(&mut self, user: &str, name: &str) -> Option<&mut Collection> {
        self.collections.get_mut(&(Self::key(user), name.to_owned()))
    }

    /// Adds (or replaces) a collection of `user`.
    pub fn add_collection(&mut self, user: &str, name: &str, kind: Kind, display: &str) {
        self.ensure(user);
        self.collections.insert(
            (Self::key(user), name.to_owned()),
            Collection { kind, display: display.to_owned(), items: BTreeMap::new() },
        );
    }

    /// Moves a collection of `user` to another name; the old name then redirects to the new one.
    pub fn move_collection(&mut self, user: &str, from: &str, to: &str) -> bool {
        self.ensure(user);
        let Some(c) = self.collections.remove(&(Self::key(user), from.to_owned())) else { return false };
        self.collections.insert((Self::key(user), to.to_owned()), c);
        self.moved.insert((Self::key(user), from.to_owned()), to.to_owned());
        true
    }

    pub fn new_etag(&mut self) -> String {
        self.next_etag = self.next_etag.saturating_add(1);
        match &self.constant_etag {
            Some(e) => e.clone(),
            None => format!("\"{}\"", self.next_etag),
        }
    }

    /// Stores a resource; returns its new ETag.
    pub fn put(&mut self, user: &str, coll: &str, name: &str, data: Vec<u8>, ctype: &str) -> Option<String> {
        self.ensure(user);
        let etag = self.new_etag();
        let c = self.collection_mut(user, coll)?;
        c.items.insert(name.to_owned(), Item { data, etag: etag.clone(), content_type: ctype.to_owned() });
        Some(etag)
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while i < b.len() {
        let c = b.get(i).copied().unwrap_or(0);
        if c == b'%'
            && let Some(h) = b.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(&String::from_utf8_lossy(h), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond(conn: &mut Conn, status: &str, headers: &[(&str, &str)], body: &[u8]) -> io::Result<()> {
    let mut out = format!("HTTP/1.1 {status}\r\n");
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()));
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    conn.write_all(&bytes)
}

fn plain(conn: &mut Conn, status: &str, text: &str) -> io::Result<()> {
    respond(conn, status, &[("Content-Type", "text/plain")], text.as_bytes())
}

const MS_OPEN: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav" xmlns:cal="urn:ietf:params:xml:ns:caldav">"#;

fn multistatus(conn: &mut Conn, responses: &[String]) -> io::Result<()> {
    let body = format!("{MS_OPEN}{}</d:multistatus>", responses.concat());
    respond(conn, "207 Multi-Status", &[("Content-Type", "application/xml; charset=utf-8")], body.as_bytes())
}

/// A resource name as it appears in an href: percent-encoded except for unreserved characters and `@`.
fn href_name(name: &str) -> String {
    name.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'@') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn response(href: &str, props: &str) -> String {
    format!(
        "<d:response><d:href>{}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status>\
         </d:propstat></d:response>",
        xml_escape(href)
    )
}

fn collection_props(c: &Collection) -> String {
    let (rtype, comps) = match c.kind {
        Kind::AddressBook => ("<d:collection/><card:addressbook/>".to_owned(), String::new()),
        Kind::Calendar(comps) => (
            "<d:collection/><cal:calendar/>".to_owned(),
            format!(
                "<cal:supported-calendar-component-set>{}</cal:supported-calendar-component-set>",
                comps.iter().map(|x| format!("<cal:comp name=\"{x}\"/>")).collect::<String>()
            ),
        ),
        Kind::Plain => ("<d:collection/>".to_owned(), String::new()),
    };
    format!(
        "<d:resourcetype>{rtype}</d:resourcetype><d:displayname>{}</d:displayname>{comps}",
        xml_escape(&c.display)
    )
}

fn item_props(i: &Item) -> String {
    format!(
        "<d:resourcetype/><d:getetag>{}</d:getetag><d:getcontenttype>{}</d:getcontenttype>",
        xml_escape(&i.etag),
        xml_escape(&i.content_type)
    )
}

/// Hrefs in a REPORT body (`<x:href>…</x:href>` with any prefix).
fn hrefs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("href>") {
        let before = rest.get(..i).unwrap_or_default();
        let after = rest.get(i + 5..).unwrap_or_default();
        let opening =
            before.rfind('<').is_some_and(|lt| !before.get(lt..).unwrap_or_default().starts_with("</"));
        if opening {
            let text = after.split('<').next().unwrap_or_default().trim();
            if !text.is_empty() && out.len() < 1000 {
                out.push(text.to_owned());
            }
        }
        rest = after;
    }
    out
}

/// True for requests the DAV server answers.
pub fn handles(path: &str) -> bool {
    path == "/dav"
        || path.starts_with("/dav/")
        || path.starts_with("/.well-known/carddav")
        || path.starts_with("/.well-known/caldav")
}

/// Answers one request.
pub fn handle(conn: &mut Conn, ctx: &Ctx, req: &Request) -> io::Result<()> {
    let path = req.path.split('?').next().unwrap_or("/").to_owned();
    {
        let mut d = ctx.dav.lock().unwrap_or_else(|e| e.into_inner());
        if d.requests.len() < 2000 {
            d.requests.push(format!("{} {path}", req.method));
        }
    }
    if path.starts_with("/.well-known/") {
        return respond(conn, "301 Moved Permanently", &[("Location", "/dav/")], b"");
    }
    let segments: Vec<String> =
        path.trim_start_matches("/dav").split('/').filter(|s| !s.is_empty()).map(percent_decode).collect();
    let user_seg = segments.first().cloned();
    let coll_seg = segments.get(1).cloned();
    let item_seg = segments.get(2).cloned();
    if segments.len() > 3 {
        return plain(conn, "404 Not Found", "not found");
    }
    // Published files are public.
    if req.method == "GET"
        && coll_seg.as_deref() == Some("public")
        && let (Some(u), Some(name)) = (&user_seg, &item_seg)
    {
        let d = ctx.dav.lock().unwrap_or_else(|e| e.into_inner());
        return match d.collection(u, "public").and_then(|c| c.items.get(name)) {
            Some(i) => {
                let (ct, et, data) = (i.content_type.clone(), i.etag.clone(), i.data.clone());
                drop(d);
                respond(conn, "200 OK", &[("Content-Type", &ct), ("ETag", &et)], &data)
            }
            None => {
                drop(d);
                plain(conn, "404 Not Found", "not found")
            }
        };
    }
    let signed_in = authenticate(ctx, req);
    let refused = signed_in.is_some() && {
        let mut d = ctx.dav.lock().unwrap_or_else(|e| e.into_inner());
        let hit = d.refuse_once.as_deref().is_some_and(|prefix| path.starts_with(prefix));
        if hit {
            d.refuse_once = None;
        }
        hit
    };
    let Some(me) = signed_in.filter(|_| !refused) else {
        return respond(
            conn,
            "401 Unauthorized",
            &[("WWW-Authenticate", "Basic realm=\"MailCraft test DAV\""), ("Content-Type", "text/plain")],
            b"sign in",
        );
    };
    let depth = req.header("depth").unwrap_or("0").trim().to_owned();
    let principal = format!("/dav/{me}/");
    let Some(user) = user_seg else {
        // The DAV root: names the principal.
        if req.method != "PROPFIND" {
            return plain(conn, "405 Method Not Allowed", "PROPFIND only");
        }
        let props = format!(
            "<d:resourcetype><d:collection/></d:resourcetype><d:current-user-principal><d:href>{}</d:href>\
             </d:current-user-principal>",
            xml_escape(&principal)
        );
        return multistatus(conn, &[response("/dav/", &props)]);
    };
    if !user.eq_ignore_ascii_case(&me) {
        return plain(conn, "403 Forbidden", "not your collection");
    }
    let mut d = ctx.dav.lock().unwrap_or_else(|e| e.into_inner());
    d.ensure(&me);
    let Some(coll) = coll_seg else {
        // Principal and home.
        if req.method != "PROPFIND" {
            drop(d);
            return plain(conn, "405 Method Not Allowed", "PROPFIND only");
        }
        let home = xml_escape(&principal);
        let mut out = vec![response(
            &principal,
            &format!(
                "<d:resourcetype><d:collection/><d:principal/></d:resourcetype><d:current-user-principal>\
                 <d:href>{home}</d:href></d:current-user-principal><card:addressbook-home-set><d:href>{home}</d:href>\
                 </card:addressbook-home-set><cal:calendar-home-set><d:href>{home}</d:href></cal:calendar-home-set>"
            ),
        )];
        if depth == "1" {
            for ((u, name), c) in &d.collections {
                if *u == DavState::key(&me) {
                    out.push(response(&format!("{principal}{}/", href_name(name)), &collection_props(c)));
                }
            }
        }
        drop(d);
        return multistatus(conn, &out);
    };
    if let Some(to) = d.moved.get(&(DavState::key(&me), coll.clone())).cloned() {
        drop(d);
        let rest = item_seg.as_deref().map(href_name).unwrap_or_default();
        let to = format!("{principal}{}/{rest}", href_name(&to));
        return respond(conn, "301 Moved Permanently", &[("Location", &to)], b"");
    }
    let base = format!("{principal}{}/", href_name(&coll));
    let Some(c) = d.collection(&me, &coll).cloned() else {
        drop(d);
        return plain(conn, "404 Not Found", "no such collection");
    };
    let Some(item) = item_seg else {
        return match req.method.as_str() {
            "PROPFIND" => {
                let stray = d.stray_href.clone();
                drop(d);
                let mut out = vec![response(&base, &collection_props(&c))];
                if depth == "1" {
                    for (name, i) in &c.items {
                        out.push(response(&format!("{base}{}", href_name(name)), &item_props(i)));
                    }
                    if let Some(h) = stray {
                        out.push(response(&h, "<d:resourcetype/><d:getetag>\"stray\"</d:getetag>"));
                    }
                }
                multistatus(conn, &out)
            }
            "REPORT" => {
                drop(d);
                let body = String::from_utf8_lossy(&req.body);
                let data_el = match c.kind {
                    Kind::AddressBook => "card:address-data",
                    _ => "cal:calendar-data",
                };
                let mut out = Vec::new();
                for h in hrefs(&body) {
                    let name = percent_decode(h.rsplit('/').next().unwrap_or_default());
                    match c.items.get(&name) {
                        Some(i) => out.push(response(
                            &h,
                            &format!(
                                "<d:getetag>{}</d:getetag><{data_el}>{}</{data_el}>",
                                xml_escape(&i.etag),
                                xml_escape(&String::from_utf8_lossy(&i.data))
                            ),
                        )),
                        None => out.push(format!(
                            "<d:response><d:href>{}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>",
                            xml_escape(&h)
                        )),
                    }
                }
                multistatus(conn, &out)
            }
            _ => {
                drop(d);
                plain(conn, "405 Method Not Allowed", "not on a collection")
            }
        };
    };
    let current = c.items.get(&item).cloned();
    match req.method.as_str() {
        "GET" => {
            drop(d);
            match current {
                Some(i) => {
                    respond(conn, "200 OK", &[("Content-Type", &i.content_type), ("ETag", &i.etag)], &i.data)
                }
                None => plain(conn, "404 Not Found", "not found"),
            }
        }
        "PUT" => {
            let if_match = req.header("if-match").map(str::trim);
            let if_none = req.header("if-none-match").map(str::trim);
            let failed = (if_none == Some("*") && current.is_some())
                || if_match.is_some_and(|m| current.as_ref().is_none_or(|i| m != "*" && i.etag != m));
            if failed {
                drop(d);
                return plain(conn, "412 Precondition Failed", "the resource changed");
            }
            if req.body.len() > MAX_RESOURCE {
                drop(d);
                return plain(conn, "413 Payload Too Large", "too large");
            }
            let text = String::from_utf8_lossy(&req.body);
            let valid = match c.kind {
                Kind::AddressBook => text.contains("BEGIN:VCARD"),
                Kind::Calendar(comps) => {
                    text.contains("BEGIN:VCALENDAR")
                        && comps.iter().any(|x| text.contains(&format!("BEGIN:{x}")))
                }
                Kind::Plain => true,
            };
            if !valid {
                drop(d);
                return plain(conn, "403 Forbidden", "not data this collection takes");
            }
            if current.is_none() && c.items.len() >= MAX_ITEMS {
                drop(d);
                return plain(conn, "507 Insufficient Storage", "full");
            }
            let ctype = req.header("content-type").unwrap_or("application/octet-stream").to_owned();
            let name = if current.is_none() && d.rename_new {
                let ext = item.rsplit_once('.').map(|(_, e)| e.to_owned()).unwrap_or_default();
                format!("srv-{}.{ext}", d.next_etag)
            } else {
                item.clone()
            };
            let etag = d.put(&me, &coll, &name, req.body.clone(), &ctype).unwrap_or_default();
            if let Some((u, c, n, data)) = d.after_next_put.take() {
                let ctype = if c == "contacts" { "text/vcard" } else { "text/calendar" };
                d.put(&u, &c, &n, data.into_bytes(), ctype);
            }
            let etag = if d.put_without_etag { String::new() } else { etag };
            drop(d);
            let status = if current.is_some() { "204 No Content" } else { "201 Created" };
            let headers: &[(&str, &str)] = if etag.is_empty() { &[] } else { &[("ETag", &etag)] };
            respond(conn, status, headers, b"")
        }
        "DELETE" => {
            let if_match = req.header("if-match").map(str::trim);
            let Some(cur) = current else {
                drop(d);
                return plain(conn, "404 Not Found", "not found");
            };
            if if_match.is_some_and(|m| m != "*" && m != cur.etag) {
                drop(d);
                return plain(conn, "412 Precondition Failed", "the resource changed");
            }
            if let Some(c) = d.collection_mut(&me, &coll) {
                c.items.remove(&item);
            }
            drop(d);
            respond(conn, "204 No Content", &[], b"")
        }
        "PROPFIND" => {
            drop(d);
            match current {
                Some(i) => {
                    multistatus(conn, &[response(&format!("{base}{}", href_name(&item)), &item_props(&i))])
                }
                None => plain(conn, "404 Not Found", "not found"),
            }
        }
        _ => {
            drop(d);
            plain(conn, "405 Method Not Allowed", "method not allowed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_hrefs_any_prefix() {
        let body = r#"<c:addressbook-multiget xmlns:d="DAV:"><d:prop><d:getetag/></d:prop>
            <d:href>/dav/a/contacts/1.vcf</d:href><D:href> /dav/a/contacts/2.vcf </D:href><href>x</href></c:addressbook-multiget>"#;
        assert_eq!(hrefs(body), ["/dav/a/contacts/1.vcf", "/dav/a/contacts/2.vcf", "x"]);
        assert!(hrefs("<d:href></d:href>").is_empty());
        assert_eq!(percent_decode("a%40b%2"), "a@b%2");
    }
}
