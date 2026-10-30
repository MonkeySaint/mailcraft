//! Contacts, calendars and Microsoft To Do tasks of a Microsoft Graph account over Microsoft Graph: contact
//! folders and contacts, calendars (the user's own, calendars shared with them, and other people's calendars
//! opened here) and events, To Do lists and tasks.
//!
//! **One synchronisation for every protocol.** Like JMAP (`jmap_pim.rs`), [`Store`] shows the DAV
//! synchroniser (`dav.rs`) each Graph collection as a WebDAV collection: a collection's address is
//! `https://graph.invalid/<contacts|todo|calendar|shared|user>/<id>/` (the reserved `.invalid` name never
//! reaches the network; `shared` is a calendar another person shared with the user, `user` another person's
//! calendar opened by address, `contacts/~/` the default contacts folder), an object's address is the
//! collection's plus the object's id (an item made here is uploaded under a name made from its UID and moves
//! to its id's address once the service names it), and its tag is made from the object's `changeKey`. Objects
//! are read as MailCraft's own vCard / iCalendar text (`graph_items.rs`); an update sends only the mapped
//! properties whose value differs from the service's copy read the same way, so what the edit did not touch
//! (and what MailCraft does not model) stays as it is. Conditional writes are checked against the tag of the
//! object just before the write.
//!
//! **Delta.** Every collection keeps a delta link (`DavFolderState::delta`): a run reads only what changed
//! since (whole objects, removals as `@removed`) and lists the rest as it was known; a lost delta state (410)
//! lists the collection whole again. A calendar's delta is the calendar view's (`calendarView/delta`) over a
//! range of dates around today, so a calendar is also listed whole every week; a changed instance (an
//! occurrence or exception) re-reads its series, a series with its exceptions (`instances`) is one resource,
//! and occurrences missing from the instances are the series' exception dates (only for a series in a zone
//! MailCraft knows). A removal the run cannot place re-reads the known series whose master changed (one
//! small read each). The delta link advances only when the run applied everything it listed.
//!
//! **Scheduling.** Graph does the scheduling: an event with attendees made or changed here is an invitation
//! or update the service sends, and a meeting removed here is cancelled by it (with the user's note, through
//! the event's `cancel` action); answers to invitations go through the event's `accept` /
//! `tentativelyAccept` / `decline` actions ([`respond_all`]). No iTIP mail goes from MailCraft for a Graph
//! calendar, except an answer for a meeting the service does not have. A meeting someone else organized is
//! never made on the service from here ([`Store::keeps_local`]): the service would invite its attendees in
//! the user's name.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use mailcraft_model::account::{GraphCancel, GraphResponse, SharedCalendarRef};
use mailcraft_model::{Appointment, DateTime};
use mailcraft_net::NetError;
use mailcraft_net::dav::{Fetched, Listing, Precondition, Resource};
use mailcraft_net::graph::pim::{self as gpim, Response};
use mailcraft_net::graph::{GraphClient, seg};
use mailcraft_net::http::{percent_decode, percent_encode};
use serde_json::{Map, Value, json};

use super::graph_items::{self as items, format_local};
use super::pim::{self, FolderJob, FolderOutcome, GRAPH_ORIGIN as ORIGIN, PimKind};

const DAY: i64 = 86_400;
/// A calendar is listed whole again after this long (its delta covers a range of dates).
pub(super) const CALENDAR_REFRESH: i64 = 7 * DAY;
/// The calendar view range a delta covers: from a year back to two years ahead.
const VIEW_BACK: i64 = 365 * DAY;
const VIEW_AHEAD: i64 = 730 * DAY;
/// How far ahead of today (or its start) a series' instances are read.
const SERIES_AHEAD: i64 = 730 * DAY;
/// Most collections of one kind listed.
const MAX_COLLECTIONS: usize = pim::MAX_COLLECTIONS;

/// The Graph collection a store address names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Where {
    /// A contacts folder (`~`: the default one).
    Contacts(String),
    /// A To Do list.
    Todo(String),
    /// One of the user's own calendars.
    Calendar(String),
    /// A calendar another person shared with the user (listed in `/me/calendars`).
    Shared(String),
    /// Another person's default calendar opened by address.
    User(String),
}

impl Where {
    fn segment(&self) -> (&'static str, &str) {
        match self {
            Where::Contacts(id) => ("contacts", id),
            Where::Todo(id) => ("todo", id),
            Where::Calendar(id) => ("calendar", id),
            Where::Shared(id) => ("shared", id),
            Where::User(a) => ("user", a),
        }
    }

    pub(super) fn url(&self) -> String {
        let (s, id) = self.segment();
        super::dav::canon(&format!("{ORIGIN}/{s}/{}/", percent_encode(id)))
    }

    pub(super) fn parse(url: &str) -> Option<Where> {
        let rest = url.trim().get(ORIGIN.len()..)?.strip_prefix('/')?;
        if !url.trim().to_ascii_lowercase().starts_with(ORIGIN) {
            return None;
        }
        let (s, rest) = rest.split_once('/')?;
        let id = rest.strip_suffix('/').unwrap_or(rest);
        if id.is_empty() || id.contains('/') {
            return None;
        }
        let id = percent_decode(id, false);
        Some(match s {
            "contacts" => Where::Contacts(id),
            "todo" => Where::Todo(id),
            "calendar" => Where::Calendar(id),
            "shared" => Where::Shared(id),
            "user" => Where::User(id),
            _ => return None,
        })
    }

    /// The calendar's path (`/me/calendars/{id}`, `/users/{address}/calendar`).
    fn calendar(&self) -> Option<String> {
        match self {
            Where::Calendar(id) | Where::Shared(id) => Some(format!("/me/calendars/{}", seg(id))),
            Where::User(a) => Some(gpim::user_calendar_path(a)),
            Where::Contacts(_) | Where::Todo(_) => None,
        }
    }

    fn kind(&self) -> PimKind {
        match self {
            Where::Contacts(_) => PimKind::Contacts,
            Where::Todo(_) => PimKind::Tasks,
            Where::Calendar(_) | Where::Shared(_) | Where::User(_) => PimKind::Calendar,
        }
    }
}

/// True for the address of a calendar of another person (shared with the user, or opened by address):
/// it goes below Shared Calendars.
pub(super) fn is_shared_url(url: &str) -> bool {
    matches!(Where::parse(url), Some(Where::Shared(_) | Where::User(_)))
}

/// Asks the service for times in UTC and bodies as text.
pub(super) fn prepare(c: &mut GraphClient) {
    for p in ["outlook.timezone=\"UTC\"", "outlook.body-content-type=\"text\""] {
        if !c.prefer.iter().any(|x| x == p) {
            c.prefer.push(p.to_owned());
        }
    }
}

fn http(status: u16, what: &str) -> NetError {
    NetError::Http { status, body: what.to_owned() }
}

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).map(|s| s.chars().take(500).collect()).unwrap_or_default()
}

/// The user's own addresses on the service: the account's address as typed, and the primary address and
/// sign-in name the service knows them by (`/me`; they often differ from the typed one: an alias, a short
/// name, an Outlook.com account's generated address). Lowercase, without duplicates.
pub(super) fn my_addresses(c: &GraphClient, typed: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |a: &str| {
        let a = a.trim().to_ascii_lowercase();
        if !a.is_empty() && a.len() <= 320 && !out.contains(&a) && out.len() < 50 {
            out.push(a);
        }
    };
    for t in typed {
        add(t);
    }
    match c.me() {
        Ok(v) => {
            add(&text(v.get("mail")));
            add(&text(v.get("userPrincipalName")));
        }
        Err(e) => log::info!("graph: the signed-in user was not read: {e}"),
    }
    out
}

/// Deepest contact folder walked below the default one.
const MAX_FOLDER_DEPTH: usize = 5;

/// The contact folders below the default one: `/me/contactFolders` lists its children, each folder's
/// `childFolders` the next level (breadth first, at most [`MAX_FOLDER_DEPTH`] levels and
/// [`MAX_COLLECTIONS`] folders; one listed twice is kept once).
fn contact_folders(c: &GraphClient) -> Result<Vec<(String, String)>, NetError> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut level: Vec<String> = Vec::new();
    let mut take = |list: Vec<Value>, next: &mut Vec<String>, out: &mut Vec<(String, String)>| {
        for f in list {
            let id = text(f.get("id"));
            if id.is_empty() || id == "~" || out.len() >= MAX_COLLECTIONS || !seen.insert(id.clone()) {
                continue;
            }
            let name = text(f.get("displayName"));
            next.push(id.clone());
            out.push((id.clone(), if name.is_empty() { id } else { name }));
        }
    };
    take(c.get_all("/me/contactFolders?$top=100", MAX_COLLECTIONS)?, &mut level, &mut out);
    for _ in 1..MAX_FOLDER_DEPTH {
        if level.is_empty() || out.len() >= MAX_COLLECTIONS {
            break;
        }
        let mut next = Vec::new();
        for id in std::mem::take(&mut level) {
            if out.len() >= MAX_COLLECTIONS {
                break;
            }
            let list = match c
                .get_all(&format!("/me/contactFolders/{}/childFolders?$top=100", seg(&id)), MAX_COLLECTIONS)
            {
                Ok(l) => l,
                // A folder gone since it was listed has no children.
                Err(e) if gpim::not_found(&e) => continue,
                Err(e) => return Err(e),
            };
            take(list, &mut next, &mut out);
        }
        level = next;
    }
    Ok(out)
}

/// The collections of `kind` on the account: (address, name), the default one first, and the addresses of
/// those the user may not change. `me` are the user's own addresses ([`my_addresses`]); `opened` the other
/// people's calendars opened on the account.
pub(super) fn collections(
    c: &GraphClient,
    kind: PimKind,
    me: &[String],
    opened: &[SharedCalendarRef],
) -> Result<(Vec<(String, String)>, Vec<String>), NetError> {
    let mut read_only = Vec::new();
    let list = match kind {
        PimKind::Contacts => {
            let mut out = vec![(Where::Contacts("~".into()).url(), "Contacts".to_owned())];
            for (id, name) in contact_folders(c)? {
                out.push((Where::Contacts(id).url(), name));
            }
            out
        }
        PimKind::Tasks => {
            let mut lists: Vec<(bool, String, String)> = c
                .get_all("/me/todo/lists?$top=100", MAX_COLLECTIONS)?
                .iter()
                .filter_map(|l| {
                    let id = text(l.get("id"));
                    // Flagged e-mail is the service's own read-only list of flagged messages: not a task folder.
                    let flagged = l.get("wellknownListName").and_then(Value::as_str) == Some("flaggedEmails");
                    (!id.is_empty() && !flagged).then(|| {
                        let default =
                            l.get("wellknownListName").and_then(Value::as_str) == Some("defaultList");
                        let name = text(l.get("displayName"));
                        (!default, Where::Todo(id.clone()).url(), if name.is_empty() { id } else { name })
                    })
                })
                .collect();
            lists.sort_by_key(|a| a.0);
            lists.into_iter().map(|(_, u, n)| (u, n)).collect()
        }
        PimKind::Calendar => {
            let opened_by: BTreeSet<String> =
                opened.iter().map(|o| o.address.trim().to_ascii_lowercase()).collect();
            let mut own: Vec<(bool, String, String)> = Vec::new();
            let mut shared: Vec<(String, String)> = Vec::new();
            for cal in c.get_all("/me/calendars?$top=100", MAX_COLLECTIONS)? {
                let id = text(cal.get("id"));
                if id.is_empty() {
                    continue;
                }
                let owner = cal
                    .get("owner")
                    .and_then(|o| o.get("address"))
                    .and_then(Value::as_str)
                    .map(|a| a.trim().to_ascii_lowercase())
                    .unwrap_or_default();
                let owner_name = cal.get("owner").map(|o| text(o.get("name"))).unwrap_or_default();
                let name = text(cal.get("name"));
                let edit = cal.get("canEdit").and_then(Value::as_bool) != Some(false);
                let default = cal.get("isDefaultCalendar").and_then(Value::as_bool) == Some(true);
                // The user's own: the default calendar, one only its owner may share, or one owned by one of
                // the user's addresses (the typed address alone often differs from the service's).
                let mine = default
                    || cal.get("canShare").and_then(Value::as_bool) == Some(true)
                    || owner.is_empty()
                    || me.contains(&owner);
                if mine {
                    own.push((
                        !default,
                        Where::Calendar(id).url(),
                        if name.is_empty() { "Calendar".into() } else { name },
                    ));
                } else if !opened_by.contains(&owner) {
                    let url = Where::Shared(id).url();
                    let who = if owner_name.is_empty() { owner.clone() } else { owner_name };
                    let label = if name.is_empty() || name.eq_ignore_ascii_case("calendar") {
                        who
                    } else {
                        format!("{name} ({who})")
                    };
                    if !edit {
                        read_only.push(url.clone());
                    }
                    shared.push((url, label));
                }
            }
            own.sort_by_key(|a| a.0);
            let mut out: Vec<(String, String)> = own.into_iter().map(|(_, u, n)| (u, n)).collect();
            out.extend(shared);
            for o in opened.iter().take(MAX_COLLECTIONS) {
                let w = Where::User(o.address.trim().to_owned());
                let url = w.url();
                // Whether the owner lets the user change it; a calendar not reachable now still keeps its folder.
                if let Some(path) = w.calendar() {
                    match c.get_json(&path) {
                        Ok(v) if v.get("canEdit").and_then(Value::as_bool) == Some(false) => {
                            read_only.push(url.clone())
                        }
                        Ok(_) => {}
                        Err(e) if gpim::denied(&e) || gpim::not_found(&e) => read_only.push(url.clone()),
                        Err(e) => return Err(e),
                    }
                }
                let name = if o.name.trim().is_empty() {
                    o.address.trim().to_owned()
                } else {
                    o.name.trim().to_owned()
                };
                out.push((url, name));
            }
            out
        }
    };
    Ok((list.into_iter().take(MAX_COLLECTIONS.saturating_mul(2)).collect(), read_only))
}

/// The tag of a Graph object: from its `changeKey` (or the whole object when it has none).
fn tag_of(v: &Value) -> String {
    let key = v
        .get("changeKey")
        .and_then(Value::as_str)
        .or_else(|| v.get("@odata.etag").and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string());
    format!("\"{:016x}\"", pim::fnv(key.as_bytes()))
}

/// Most series a run checks one by one for an occurrence removed from the calendar view that it cannot place
/// (more: the calendar is listed whole on the next run).
const MAX_SERIES_CHECKS: usize = 200;

/// A short hash of a series master's `changeKey` (it changes when an occurrence is cancelled or changed).
fn master_key(master: &Value) -> String {
    let key = master
        .get("changeKey")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| master.to_string());
    format!("{:08x}", pim::fnv(key.as_bytes()) & 0xffff_ffff)
}

/// The master part of a series' tag ([`group_tag`]); `None` for another tag.
fn tag_master_key(tag: &str) -> Option<&str> {
    let t = tag.strip_prefix("\"s")?.strip_suffix('"')?;
    (t.len() == 24 && t.chars().all(|c| c.is_ascii_hexdigit())).then(|| t.get(..8)).flatten()
}

/// A series' tag (`"s…"`, so a run can find the series it knows, starting with [`master_key`]) or a single
/// event's (`"e…"`).
fn group_tag(g: &Value) -> String {
    let master = g.get("master").cloned().unwrap_or(Value::Null);
    let mut key = master
        .get("changeKey")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| master.to_string());
    let series = master.get("recurrence").is_some_and(Value::is_object);
    for x in g.get("exceptions").and_then(Value::as_array).into_iter().flatten() {
        key.push('|');
        key.push_str(&text(x.get("id")));
        key.push(':');
        key.push_str(
            &x.get("changeKey").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| x.to_string()),
        );
    }
    for t in g.get("cancelled").and_then(Value::as_array).into_iter().flatten() {
        key.push_str(&format!("|x{t}"));
    }
    if series {
        format!("\"s{}{:016x}\"", master_key(&master), pim::fnv(key.as_bytes()))
    } else {
        format!("\"e{:016x}\"", pim::fnv(key.as_bytes()))
    }
}

fn utc_param(unix: i64) -> String {
    percent_encode(&format!("{}Z", format_local(unix)))
}

fn cancelled_of(g: &Value) -> Vec<i64> {
    g.get("cancelled")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

/// One collection's objects as DAV resources (see the module notes).
pub(super) struct Store<'a> {
    c: &'a GraphClient,
    kind: PimKind,
    tz: i32,
    now: DateTime,
    /// The mailbox's time zone (an appointment without one of its own is written in it).
    zone: Option<String>,
    known: BTreeMap<String, String>,
    delta: String,
    delta_at: i64,
    /// Resource address → Graph id.
    ids: RefCell<BTreeMap<String, String>>,
    /// Resource address → (tag, object or event group) as last read.
    cache: RefCell<BTreeMap<String, (String, Value)>>,
    /// Addresses items were uploaded under → their id's address.
    renames: RefCell<BTreeMap<String, String>>,
    /// The delta link (and when its collection was listed whole) after this run.
    next: RefCell<Option<(String, i64)>>,
    /// The user's own addresses (lowercase): a meeting organized by anyone else is never made on the service
    /// from here (the service would send its invitations in the user's name).
    me: Vec<String>,
    /// Notes for the attendees of meetings cancelled here, by the meeting's address: (the address as queued,
    /// the note).
    cancels: BTreeMap<String, (String, String)>,
    /// Addresses (as queued) of the meetings whose cancellation went with its note this run.
    cancelled: RefCell<Vec<String>>,
}

impl<'a> Store<'a> {
    pub(super) fn new(
        c: &'a GraphClient,
        job: &FolderJob,
        tz: i32,
        now: DateTime,
        zone: Option<String>,
        me: &[String],
        cancels: &[GraphCancel],
    ) -> Store<'a> {
        Store {
            c,
            kind: job.kind,
            tz,
            now,
            zone,
            known: job.known.iter().map(|(u, e)| (super::dav::canon(u), e.clone())).collect(),
            delta: job.delta.clone(),
            delta_at: job.delta_at,
            ids: RefCell::new(BTreeMap::new()),
            cache: RefCell::new(BTreeMap::new()),
            renames: RefCell::new(BTreeMap::new()),
            next: RefCell::new(None),
            me: me.iter().map(|m| m.trim().to_ascii_lowercase()).filter(|m| !m.is_empty()).collect(),
            cancels: cancels
                .iter()
                .take(500)
                .filter(|x| !x.url.trim().is_empty())
                .map(|x| (super::dav::canon(&x.url), (x.url.clone(), x.comment.clone())))
                .collect(),
            cancelled: RefCell::new(Vec::new()),
        }
    }

    /// True for a meeting organized by someone other than the user: an appointment with attendees whose
    /// organizer is none of the user's addresses.
    fn foreign(&self, a: &Appointment) -> bool {
        !a.attendees.is_empty()
            && a.organizer.as_ref().is_some_and(|o| {
                let addr = o.addr.trim().to_ascii_lowercase();
                !addr.is_empty() && !self.me.contains(&addr)
            })
    }

    /// True when an object made here is kept here instead of being made on the service: a meeting someone
    /// else organized (copied or imported into the calendar, or placed from an invitation the service has
    /// not put on its calendar). Made there, the service would make the user its organizer and send its
    /// invitations to every attendee in the user's name; the service's own copy, once it has one, takes
    /// the local one over (same UID).
    pub(super) fn keeps_local(&self, data: &[u8]) -> bool {
        if self.kind != PimKind::Calendar {
            return false;
        }
        let Some((list, _)) = pim::read_events(&String::from_utf8_lossy(data), self.tz) else { return false };
        list.iter().any(|a| self.foreign(a))
    }

    /// Addresses of the meetings whose cancellation went with its note this run.
    pub(super) fn cancelled_urls(&self) -> Vec<String> {
        self.cancelled.borrow().clone()
    }

    fn place(&self, collection: &str) -> Result<Where, NetError> {
        Where::parse(collection)
            .filter(|w| w.kind() == self.kind)
            .ok_or_else(|| NetError::Protocol(format!("{collection} is not a Microsoft Graph collection")))
    }

    fn object_url(collection: &str, id: &str) -> String {
        super::dav::canon(&format!(
            "{}{}",
            collection.trim_end_matches('/').to_owned() + "/",
            percent_encode(id)
        ))
    }

    /// The Graph id in an object address of `collection`.
    fn id_in(collection: &str, url: &str) -> Option<String> {
        let base = super::dav::canon(&(collection.trim_end_matches('/').to_owned() + "/"));
        let rest = super::dav::canon(url).strip_prefix(&base)?.to_owned();
        (!rest.is_empty() && !rest.contains('/')).then(|| percent_decode(&rest, false))
    }

    fn id_of(&self, url: &str) -> Option<String> {
        self.ids.borrow().get(&super::dav::canon(url)).cloned()
    }

    /// Path of object `id` (a contact, task or event).
    fn object_path(&self, w: &Where, id: &str) -> String {
        match w {
            Where::Contacts(_) => format!("/me/contacts/{}", seg(id)),
            Where::Todo(l) => format!("/me/todo/lists/{}/tasks/{}", seg(l), seg(id)),
            _ => format!("{}/events/{}", w.calendar().unwrap_or_default(), seg(id)),
        }
    }

    /// Path that lists (and creates) the collection's objects.
    fn list_path(w: &Where) -> String {
        match w {
            Where::Contacts(f) if f == "~" => "/me/contacts".into(),
            Where::Contacts(f) => format!("/me/contactFolders/{}/contacts", seg(f)),
            Where::Todo(l) => format!("/me/todo/lists/{}/tasks", seg(l)),
            _ => format!("{}/events", w.calendar().unwrap_or_default()),
        }
    }

    /// Where a delta of the collection starts (`None`: the default contacts folder whose id the service does
    /// not tell; it is listed whole).
    fn delta_start(&self, w: &Where) -> Result<Option<String>, NetError> {
        Ok(match w {
            Where::Contacts(f) if f == "~" => {
                // The default folder is the parent of its child folders, or of any contact in it.
                let from_folders = self.c.get_json("/me/contactFolders?$top=1")?;
                let first =
                    from_folders.get("value").and_then(Value::as_array).and_then(|a| a.first()).cloned();
                let mut id = first.map(|f| text(f.get("parentFolderId"))).unwrap_or_default();
                if id.is_empty() {
                    let v = self.c.get_json("/me/contacts?$top=1&$select=parentFolderId")?;
                    id = v
                        .get("value")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .map(|x| text(x.get("parentFolderId")))
                        .unwrap_or_default();
                }
                (!id.is_empty()).then(|| format!("/me/contactFolders/{}/contacts/delta", seg(&id)))
            }
            Where::Contacts(f) => Some(format!("/me/contactFolders/{}/contacts/delta", seg(f))),
            Where::Todo(l) => Some(format!("/me/todo/lists/{}/tasks/delta", seg(l))),
            _ => {
                let from = self.now.unix.saturating_sub(VIEW_BACK);
                let to = self.now.unix.saturating_add(VIEW_AHEAD);
                // Another person's default calendar: their calendar view.
                let base = match w {
                    Where::User(a) => format!("/users/{}", seg(a)),
                    _ => w.calendar().unwrap_or_default(),
                };
                Some(format!(
                    "{base}/calendarView/delta?startDateTime={}&endDateTime={}",
                    utc_param(from),
                    utc_param(to)
                ))
            }
        })
    }

    /// An object gone from the service: neither listed nor addressable any more (an item changed here then
    /// goes up as a new one).
    fn forget(&self, url: &str, listing: &mut BTreeMap<String, String>) {
        listing.remove(url);
        self.ids.borrow_mut().remove(url);
        self.cache.borrow_mut().remove(url);
    }

    fn remember(
        &self,
        collection: &str,
        id: &str,
        tag: String,
        v: Value,
        listing: &mut BTreeMap<String, String>,
    ) {
        let url = Self::object_url(collection, id);
        self.ids.borrow_mut().insert(url.clone(), id.to_owned());
        self.cache.borrow_mut().insert(url.clone(), (tag.clone(), v));
        listing.insert(url, tag);
    }

    /// The collection's listing: from its delta link when it has one, else whole (see the module notes).
    pub(super) fn listing(&self, collection: &str) -> Result<Listing, NetError> {
        let w = self.place(collection)?;
        let collection = super::dav::canon(collection);
        let listing = match self.kind {
            PimKind::Calendar => self.list_calendar(&collection, &w)?,
            PimKind::Contacts | PimKind::Tasks => self.list_items(&collection, &w)?,
        };
        let resources = listing
            .into_iter()
            .map(|(url, etag)| Resource { url, etag, content_type: self.kind.content_type().to_owned() })
            .collect();
        Ok(Listing { collection, resources, dropped: 0 })
    }

    fn known_listing(&self, collection: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (u, e) in &self.known {
            if let Some(id) = Self::id_in(collection, u) {
                self.ids.borrow_mut().insert(u.clone(), id);
                out.insert(u.clone(), e.clone());
            }
        }
        out
    }

    fn list_items(&self, collection: &str, w: &Where) -> Result<BTreeMap<String, String>, NetError> {
        let start = self.delta_start(w)?;
        if !self.delta.is_empty() && start.is_some() {
            match gpim::delta_round(self.c, &self.delta, pim::MAX_ITEMS) {
                Ok(r) => {
                    let mut listing = self.known_listing(collection);
                    for v in r.changed {
                        let id = text(v.get("id"));
                        self.remember(collection, &id, tag_of(&v), v, &mut listing);
                    }
                    for id in r.removed {
                        self.forget(&Self::object_url(collection, &id), &mut listing);
                    }
                    *self.next.borrow_mut() = Some((r.delta_link, self.delta_at));
                    return Ok(listing);
                }
                Err(e) if gpim::lost_delta(&e) => {
                    log::info!("graph: delta state lost; listing {collection} whole")
                }
                Err(e) => return Err(e),
            }
        }
        let mut listing = BTreeMap::new();
        match start {
            Some(p) => {
                let r = gpim::delta_round(self.c, &p, pim::MAX_ITEMS)?;
                for v in r.changed {
                    let id = text(v.get("id"));
                    self.remember(collection, &id, tag_of(&v), v, &mut listing);
                }
                *self.next.borrow_mut() = Some((r.delta_link, self.now.unix));
            }
            None => {
                let (all, whole) =
                    self.c.get_all_checked(&format!("{}?$top=100", Self::list_path(w)), pim::MAX_ITEMS)?;
                if !whole {
                    return Err(NetError::Protocol(pim::too_many_error()));
                }
                for v in all {
                    let id = text(v.get("id"));
                    self.remember(collection, &id, tag_of(&v), v, &mut listing);
                }
            }
        }
        Ok(listing)
    }

    fn list_calendar(&self, collection: &str, w: &Where) -> Result<BTreeMap<String, String>, NetError> {
        let cal = w.calendar().unwrap_or_default();
        let start = self.delta_start(w)?.unwrap_or_default();
        let fresh = self.now.unix.saturating_sub(self.delta_at) < CALENDAR_REFRESH;
        if !self.delta.is_empty() && fresh {
            match gpim::delta_round(self.c, &self.delta, pim::MAX_ITEMS.saturating_mul(4)) {
                Ok(r) => {
                    let mut listing = self.known_listing(collection);
                    let mut groups: BTreeSet<String> = BTreeSet::new();
                    for v in &r.changed {
                        let master = text(v.get("seriesMasterId"));
                        groups.insert(if master.is_empty() { text(v.get("id")) } else { master });
                    }
                    // A removal of an occurrence names no series: the known series whose master changed (a
                    // small read each) are read again; too many to check, and the next run lists the calendar
                    // whole.
                    let mut unplaced = false;
                    for id in &r.removed {
                        if listing.contains_key(&Self::object_url(collection, id)) {
                            groups.insert(id.clone());
                        } else {
                            unplaced = true;
                        }
                    }
                    let mut relist = false;
                    if unplaced {
                        let series: Vec<(String, String)> = listing
                            .iter()
                            .filter(|(_, tag)| tag.starts_with("\"s"))
                            .filter_map(|(u, tag)| Some((Self::id_in(collection, u)?, tag.clone())))
                            .filter(|(id, _)| !groups.contains(id))
                            .collect();
                        if series.len() > MAX_SERIES_CHECKS {
                            relist = true;
                        } else {
                            for (id, tag) in series {
                                let path = format!("{cal}/events/{}?$select=id,changeKey", seg(&id));
                                match self.c.get_json(&path) {
                                    Ok(v) if tag_master_key(&tag) == Some(master_key(&v).as_str()) => {}
                                    Ok(_) => {
                                        groups.insert(id);
                                    }
                                    Err(e) if gpim::not_found(&e) => {
                                        groups.insert(id);
                                    }
                                    Err(e) => return Err(e),
                                }
                            }
                        }
                    }
                    for gid in groups.into_iter().filter(|g| !g.is_empty()) {
                        match self.read_group(&cal, &gid)? {
                            Some((tag, g)) => self.remember(collection, &gid, tag, g, &mut listing),
                            None => {
                                self.forget(&Self::object_url(collection, &gid), &mut listing);
                            }
                        }
                    }
                    *self.next.borrow_mut() = Some((r.delta_link, if relist { 0 } else { self.delta_at }));
                    return Ok(listing);
                }
                Err(e) if gpim::lost_delta(&e) => {
                    log::info!("graph: delta state lost; listing {collection} whole")
                }
                Err(e) => return Err(e),
            }
        }
        // The delta first, so what changes while the calendar is listed comes with the next run. Only its link
        // is kept: the events come from the listing below, and the round's pages (every instance in the view)
        // are dropped as they arrive.
        let link = gpim::delta_link(self.c, &start, pim::MAX_ITEMS.saturating_mul(4))?;
        let (events, whole) = self.c.get_all_checked(&format!("{cal}/events?$top=100"), pim::MAX_ITEMS)?;
        if !whole {
            return Err(NetError::Protocol(pim::too_many_error()));
        }
        let mut listing = BTreeMap::new();
        for ev in events {
            if matches!(ev.get("type").and_then(Value::as_str), Some("occurrence" | "exception")) {
                continue;
            }
            let id = text(ev.get("id"));
            if id.is_empty() {
                continue;
            }
            let (tag, g) = self.group_from(&cal, ev)?;
            self.remember(collection, &id, tag, g, &mut listing);
        }
        *self.next.borrow_mut() = Some((link, self.now.unix));
        Ok(listing)
    }

    /// Event `gid` of calendar `cal` with its exceptions and cancelled occurrences (`None`: gone).
    fn read_group(&self, cal: &str, gid: &str) -> Result<Option<(String, Value)>, NetError> {
        match self.c.get_json(&format!("{cal}/events/{}", seg(gid))) {
            Ok(v) => {
                // An instance read by its own id stands for its series.
                let master = text(v.get("seriesMasterId"));
                if !master.is_empty() && master != gid {
                    return self.read_group(cal, &master);
                }
                self.group_from(cal, v).map(Some)
            }
            Err(e) if gpim::not_found(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The range a series' instances are read in: from its start to its end (at most two years past today
    /// or its start).
    fn series_window(&self, series: &Appointment) -> (i64, i64) {
        let from = series.start.unix.saturating_sub(DAY);
        let to = self.now.unix.max(series.start.unix).saturating_add(SERIES_AHEAD);
        (from, to)
    }

    /// An event group: `{"master", "exceptions", "cancelled"}` and its tag.
    fn group_from(&self, cal: &str, master: Value) -> Result<(String, Value), NetError> {
        let series = master.get("recurrence").is_some_and(Value::is_object);
        let mut exceptions = Vec::new();
        let mut cancelled: Vec<i64> = Vec::new();
        if series && let Some((items, _)) = items::to_appointments(&master, &[], &[], self.tz) {
            let id = text(master.get("id"));
            if let Some(s) = items.first() {
                let (from, to) = self.series_window(s);
                let (instances, whole) = self.c.get_all_checked(
                    &format!(
                        "{cal}/events/{}/instances?startDateTime={}&endDateTime={}&$top=100",
                        seg(&id),
                        utc_param(from),
                        utc_param(to)
                    ),
                    pim::MAX_GROUP.saturating_mul(2),
                )?;
                for i in &instances {
                    if i.get("type").and_then(Value::as_str) == Some("exception") {
                        exceptions.push(i.clone());
                    }
                }
                // Occurrences missing from the instances are cancelled; with only some of them (past the cap)
                // nothing is known to be.
                if whole {
                    let zone = text(master.get("originalStartTimeZone"));
                    cancelled = missing_occurrences(s, &zone, &instances, (from, to), self.tz);
                }
            }
        }
        let g = json!({"master": master, "exceptions": exceptions, "cancelled": cancelled});
        Ok((group_tag(&g), g))
    }

    /// MailCraft's own vCard / iCalendar text of a cached object or event group.
    fn text_of(&self, v: &Value) -> Option<String> {
        match self.kind {
            PimKind::Contacts => {
                let c = items::to_contact(v);
                Some(pim::contact_card(&c, &text(v.get("id"))))
            }
            PimKind::Tasks => {
                let t = items::to_task(v, self.tz);
                Some(pim::task_calendar(&t, &text(v.get("id")), self.now))
            }
            PimKind::Calendar => {
                let master = v.get("master")?;
                let exceptions: Vec<Value> =
                    v.get("exceptions").and_then(Value::as_array).cloned().unwrap_or_default();
                let (list, uid) = items::to_appointments(master, &exceptions, &cancelled_of(v), self.tz)?;
                let refs: Vec<&Appointment> = list.iter().collect();
                Some(pim::event_calendar(&refs, &uid, self.now))
            }
        }
    }

    /// The text of the objects at `urls`.
    pub(super) fn multiget(&self, collection: &str, urls: &[String]) -> Result<Vec<Fetched>, NetError> {
        let w = self.place(collection)?;
        let mut out = Vec::new();
        for u in urls {
            let url = super::dav::canon(u);
            let cached = self.cache.borrow().get(&url).cloned();
            let got = match cached {
                Some(x) => Some(x),
                None => {
                    let Some(id) = self.id_of(&url).or_else(|| Self::id_in(collection, &url)) else {
                        continue;
                    };
                    match self.kind {
                        PimKind::Calendar => self.read_group(&w.calendar().unwrap_or_default(), &id)?,
                        _ => match self.c.get_json(&self.object_path(&w, &id)) {
                            Ok(v) => Some((tag_of(&v), v)),
                            Err(e) if gpim::not_found(&e) => None,
                            Err(e) => return Err(e),
                        },
                    }
                }
            };
            let Some((tag, v)) = got else { continue };
            let Some(data) = self.text_of(&v) else { continue };
            self.cache.borrow_mut().insert(url.clone(), (tag.clone(), v));
            out.push(Fetched { url: u.clone(), etag: tag, data });
        }
        Ok(out)
    }

    /// Graph properties of `data` (vCard / iCalendar of one contact or task).
    fn item_props(&self, data: &str) -> Option<(Map<String, Value>, &'static [&'static str])> {
        match self.kind {
            PimKind::Contacts => {
                let (c, _) = pim::read_contact(data)?;
                Some((items::from_contact(&c), &items::CONTACT_MAPPED))
            }
            PimKind::Tasks => {
                let (t, _) = pim::read_task(data, self.tz)?;
                Some((items::from_task(&t), &items::TASK_MAPPED))
            }
            PimKind::Calendar => None,
        }
    }

    /// Writes `data` at `url` in `collection` (a failed precondition is HTTP 412, as for DAV).
    pub(super) fn put(
        &self,
        collection: &str,
        url: &str,
        data: &[u8],
        pre: &Precondition,
    ) -> Result<Option<String>, NetError> {
        let w = self.place(collection)?;
        let collection = super::dav::canon(collection);
        let url = super::dav::canon(url);
        let data = String::from_utf8_lossy(data);
        let existing = self.id_of(&url);
        match (pre, &existing) {
            (Precondition::Create, Some(_)) => return Err(http(412, "exists")),
            (Precondition::Match(_), None) => return Err(http(412, "gone")),
            _ => {}
        }
        if self.kind == PimKind::Calendar {
            return self.put_events(&w, &collection, &url, &data, pre, existing);
        }
        let Some((props, mapped)) = self.item_props(&data) else {
            return Err(http(400, "the item could not be read"));
        };
        let Some(id) = existing else {
            let body: Map<String, Value> = props.into_iter().filter(|(_, v)| !v.is_null()).collect();
            let v = self.c.post_json(&Self::list_path(&w), &Value::Object(body))?;
            let id = text(v.get("id"));
            if id.is_empty() {
                return Err(http(400, "the service gave the new item no id"));
            }
            return Ok(Some(self.created(&collection, &url, &id, tag_of(&v), v)));
        };
        let path = self.object_path(&w, &id);
        let current = match self.c.get_json(&path) {
            Ok(v) => v,
            Err(e) if gpim::not_found(&e) => return Err(http(412, "gone")),
            Err(e) => return Err(e),
        };
        if let Precondition::Match(expected) = pre
            && tag_of(&current) != *expected
        {
            return Err(http(412, "changed"));
        }
        let base = self.text_of(&current).and_then(|t| self.item_props(&t)).map(|(b, _)| b);
        let mut patch = Map::new();
        for k in mapped {
            let now = props.get(*k).cloned().unwrap_or(Value::Null);
            let was = base.as_ref().map(|b| b.get(*k).cloned().unwrap_or(Value::Null));
            if was.as_ref() != Some(&now) {
                patch.insert((*k).to_owned(), now);
            }
        }
        let after = if patch.is_empty() { current } else { self.patched(&path, &Value::Object(patch))? };
        let tag = tag_of(&after);
        self.cache.borrow_mut().insert(url, (tag.clone(), after));
        Ok(Some(tag))
    }

    /// PATCH, and the object after it (read again when the answer has none).
    fn patched(&self, path: &str, patch: &Value) -> Result<Value, NetError> {
        let v = self
            .c
            .patch_json(path, patch)
            .map_err(|e| if gpim::not_found(&e) { http(412, "gone") } else { e })?;
        if v.get("id").is_some() { Ok(v) } else { self.c.get_json(path) }
    }

    /// Records an object made here: its id's address replaces the one it was uploaded under.
    fn created(&self, collection: &str, url: &str, id: &str, tag: String, v: Value) -> String {
        let real = Self::object_url(collection, id);
        let mut ids = self.ids.borrow_mut();
        ids.insert(url.to_owned(), id.to_owned());
        ids.insert(real.clone(), id.to_owned());
        drop(ids);
        self.cache.borrow_mut().insert(url.to_owned(), (tag.clone(), v.clone()));
        self.cache.borrow_mut().insert(real.clone(), (tag.clone(), v));
        if real != url {
            self.renames.borrow_mut().insert(url.to_owned(), real);
        }
        tag
    }

    fn event_props(&self, a: &Appointment) -> Map<String, Value> {
        let mut a = a.clone();
        // Without a zone of its own: the mailbox's (an all-day event's midnights too).
        if a.zone().is_none() {
            a.tz.clone_from(&self.zone);
        }
        items::event_props(&a)
    }

    fn put_events(
        &self,
        w: &Where,
        collection: &str,
        url: &str,
        data: &str,
        pre: &Precondition,
        existing: Option<String>,
    ) -> Result<Option<String>, NetError> {
        let cal = w.calendar().unwrap_or_default();
        let Some((list, _)) = pim::read_events(data, self.tz) else {
            return Err(http(400, "the event could not be read"));
        };
        let Some(series) = list.iter().find(|a| a.recurrence_id.is_none()).or_else(|| list.first()).cloned()
        else {
            return Err(http(400, "the event could not be read"));
        };
        let mut series = series;
        series.recurrence_id = None;
        let exceptions: Vec<&Appointment> = if series.recurrence.is_some() {
            list.iter().filter(|a| a.recurrence_id.is_some()).collect()
        } else {
            Vec::new()
        };
        let Some(gid) = existing else {
            if list.iter().any(|a| self.foreign(a)) {
                // Never: the service would invite the attendees in the user's name (see `keeps_local`).
                return Err(http(409, "a meeting organized by someone else is not made on the service"));
            }
            let body: Map<String, Value> =
                self.event_props(&series).into_iter().filter(|(_, v)| !v.is_null()).collect();
            let v = self.c.post_json(&format!("{cal}/events"), &Value::Object(body))?;
            let gid = text(v.get("id"));
            if gid.is_empty() {
                return Err(http(400, "the service gave the new event no id"));
            }
            if let Err(e) = self.apply_instances(&cal, &gid, &series, &exceptions, None, false) {
                // No half-made series stays on the service: the next run makes it again whole.
                let _ = self.c.delete(&format!("{cal}/events/{}", seg(&gid)));
                return Err(e);
            }
            let (tag, g) =
                self.read_group(&cal, &gid)?.ok_or_else(|| http(400, "the new event went away"))?;
            return Ok(Some(self.created(collection, url, &gid, tag, g)));
        };
        let Some((tag, current)) = self.read_group(&cal, &gid)? else { return Err(http(412, "gone")) };
        if let Precondition::Match(expected) = pre
            && tag != *expected
        {
            return Err(http(412, "changed"));
        }
        let master = current.get("master").cloned().unwrap_or(Value::Null);
        let current_exceptions: Vec<Value> =
            current.get("exceptions").and_then(Value::as_array).cloned().unwrap_or_default();
        let (base, _) =
            items::to_appointments(&master, &current_exceptions, &cancelled_of(&current), self.tz)
                .ok_or_else(|| http(400, "the service's event could not be read"))?;
        let base_series = base.first().cloned().unwrap_or_default();
        let mut patch = diff(&self.event_props(&series), &self.event_props(&base_series));
        // An attendee's copy: the attendees are the organizer's to change (and to invite).
        let attendee_copy = master.get("isOrganizer").and_then(Value::as_bool) == Some(false);
        if attendee_copy {
            patch.remove("attendees");
        }
        if !patch.is_empty() {
            self.patched(&format!("{cal}/events/{}", seg(&gid)), &Value::Object(patch))?;
        }
        self.apply_instances(&cal, &gid, &series, &exceptions, Some((&base, &current)), attendee_copy)?;
        let (tag, g) = self.read_group(&cal, &gid)?.ok_or_else(|| http(412, "gone"))?;
        self.cache.borrow_mut().insert(url.to_owned(), (tag.clone(), g));
        Ok(Some(tag))
    }

    /// The instance of series `gid` that started at `rid` (its original start): its id and its appointment.
    fn instance_at(
        &self,
        cal: &str,
        gid: &str,
        rid: DateTime,
        all_day: bool,
    ) -> Result<Option<(String, Appointment)>, NetError> {
        let list = self.c.get_all(
            &format!(
                "{cal}/events/{}/instances?startDateTime={}&endDateTime={}",
                seg(gid),
                utc_param(rid.unix.saturating_sub(DAY)),
                utc_param(rid.unix.saturating_add(2 * DAY))
            ),
            50,
        )?;
        for i in list {
            let orig = i.get("originalStart").and_then(Value::as_str).and_then(items::parse_instant);
            let hit = match orig {
                Some(t) if all_day => {
                    let zone = items::zone_named(&text(i.get("originalStartTimeZone")));
                    items::all_day_local(t, &zone, self.tz).div_euclid(DAY)
                        == rid.unix.saturating_add(i64::from(rid.offset_min) * 60).div_euclid(DAY)
                }
                Some(t) => t == rid.unix,
                None => false,
            };
            if hit && let Some(a) = items::appointment_of(&i, self.tz) {
                return Ok(Some((text(i.get("id")), a)));
            }
        }
        Ok(None)
    }

    /// Writes a series' exceptions and exception dates: an occurrence changed here is changed there (an
    /// exception), an exception date cancels the occurrence. `base` is the service's group as read (the
    /// appointments and the raw group), `None` for a series just made.
    fn apply_instances(
        &self,
        cal: &str,
        gid: &str,
        series: &Appointment,
        exceptions: &[&Appointment],
        base: Option<(&[Appointment], &Value)>,
        attendee_copy: bool,
    ) -> Result<(), NetError> {
        let Some(rec) = series.recurrence.as_ref() else { return Ok(()) };
        let base_items: &[Appointment] = base.map(|(b, _)| b).unwrap_or(&[]);
        let base_cancelled: BTreeSet<i64> = base
            .and_then(|(b, _)| b.first())
            .and_then(|s| s.recurrence.as_ref())
            .map(|r| r.exdates.iter().map(|d| d.unix).collect())
            .unwrap_or_default();
        let raw_exceptions: Vec<Value> = base
            .and_then(|(_, g)| g.get("exceptions"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // An exception's original time is an exception date here too; that occurrence is the exception's.
        let overridden: BTreeSet<i64> =
            exceptions.iter().filter_map(|e| e.recurrence_id.map(|r| r.unix)).collect();
        for x in rec.exdates.iter().take(pim::MAX_GROUP) {
            if base_cancelled.contains(&x.unix) || overridden.contains(&x.unix) {
                continue;
            }
            if let Some((id, _)) = self.instance_at(cal, gid, *x, series.all_day)? {
                match self.c.delete(&format!("{cal}/events/{}", seg(&id))) {
                    Ok(()) => {}
                    Err(e) if gpim::not_found(&e) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        for ex in exceptions.iter().take(pim::MAX_GROUP) {
            let Some(rid) = ex.recurrence_id else { continue };
            let mut wanted = self.event_props(ex);
            wanted.remove("recurrence");
            let was = base_items.iter().skip(1).find(|b| b.recurrence_id.is_some_and(|r| r.unix == rid.unix));
            let target = match was {
                Some(b) => {
                    let id = raw_exceptions
                        .iter()
                        .find(|r| {
                            r.get("originalStart").and_then(Value::as_str).and_then(items::parse_instant)
                                == Some(rid.unix)
                        })
                        .map(|r| text(r.get("id")));
                    let mut old = self.event_props(b);
                    old.remove("recurrence");
                    id.map(|id| (id, old))
                }
                None => self.instance_at(cal, gid, rid, series.all_day)?.map(|(id, a)| {
                    let mut old = self.event_props(&a);
                    old.remove("recurrence");
                    (id, old)
                }),
            };
            let Some((id, old)) = target else { continue };
            let mut patch = diff(&wanted, &old);
            if attendee_copy {
                patch.remove("attendees");
            }
            if !patch.is_empty() {
                self.patched(&format!("{cal}/events/{}", seg(&id)), &Value::Object(patch))?;
            }
        }
        Ok(())
    }

    /// Removes the object at `url` (only while it still has `etag`, when given). One already gone is fine.
    pub(super) fn delete(&self, collection: &str, url: &str, etag: Option<&str>) -> Result<(), NetError> {
        let w = self.place(collection)?;
        let url = super::dav::canon(url);
        let Some(id) = self.id_of(&url) else { return Ok(()) };
        let path = self.object_path(&w, &id);
        let mut group: Option<Value> = self.cache.borrow().get(&url).map(|(_, g)| g.clone());
        if let Some(expected) = etag {
            let now = match self.kind {
                PimKind::Calendar => {
                    self.read_group(&w.calendar().unwrap_or_default(), &id)?.map(|(t, g)| {
                        group = Some(g);
                        t
                    })
                }
                _ => match self.c.get_json(&path) {
                    Ok(v) => Some(tag_of(&v)),
                    Err(e) if gpim::not_found(&e) => None,
                    Err(e) => return Err(e),
                },
            };
            match now {
                None => return Ok(()),
                Some(t) if t != expected => return Err(http(412, "changed")),
                Some(_) => {}
            }
        }
        // A meeting the user organized and cancelled with a note: the service's `cancel` action carries it.
        let note = group
            .filter(|_| self.kind == PimKind::Calendar)
            .and_then(|g| g.get("master").cloned())
            .filter(|m| m.get("isOrganizer").and_then(Value::as_bool) == Some(true))
            .and_then(|_| self.cancels.get(&url).cloned());
        let gone = match &note {
            Some((_, comment)) => gpim::cancel(self.c, &path, comment),
            None => self.c.delete(&path),
        };
        match gone {
            Ok(()) => {}
            Err(e) if gpim::not_found(&e) => {}
            Err(e) => return Err(e),
        }
        if let Some((queued, _)) = note {
            self.cancelled.borrow_mut().push(queued);
        }
        self.cache.borrow_mut().remove(&url);
        Ok(())
    }

    /// Puts the run's results in their final form: items made here under their id's address, and the delta
    /// link the next run continues from.
    pub(super) fn finish(&self, out: &mut FolderOutcome) {
        let renames = self.renames.borrow();
        for (_, r) in out.uploaded.iter_mut() {
            if let Some(real) = renames.get(&super::dav::canon(&r.url)) {
                real.clone_into(&mut r.url);
            }
        }
        for (from, to) in renames.iter() {
            if let Some(tag) = out.known.remove(from) {
                out.known.insert(to.clone(), tag);
            }
        }
        out.delta = self.next.borrow().clone();
    }
}

/// The original starts (Unix) of the occurrences of `series` (made in zone `zone`, the master's
/// `originalStartTimeZone`) missing from `instances`, all the service lists in `window`: they are cancelled.
/// Nothing is inferred without instances, or for a series in a zone MailCraft does not know (it would be
/// expanded in UTC, so after a daylight saving change every occurrence's start would differ from the
/// service's and read as cancelled).
fn missing_occurrences(
    series: &Appointment,
    zone: &str,
    instances: &[Value],
    window: (i64, i64),
    tz: i32,
) -> Vec<i64> {
    if instances.is_empty() || !items::zone_known(zone) {
        return Vec::new();
    }
    let z = items::zone_named(zone);
    // Timed: the original starts; all-day: the days they fall on.
    let mut seen: BTreeSet<i64> = BTreeSet::new();
    for i in instances {
        let orig = i.get("originalStart").and_then(Value::as_str).and_then(items::parse_instant);
        let t = match orig {
            Some(t) if series.all_day => Some(items::all_day_local(t, &z, tz).div_euclid(DAY)),
            Some(t) => Some(t),
            None => items::appointment_of(i, tz).map(|a| {
                if series.all_day {
                    a.start.unix.saturating_add(i64::from(a.start.offset_min) * 60).div_euclid(DAY)
                } else {
                    a.start.unix
                }
            }),
        };
        if let Some(t) = t {
            seen.insert(t);
        }
    }
    let expected = crate::cmd::calendar::occurrences(
        series,
        DateTime::from_unix(window.0, 0),
        DateTime::from_unix(window.1, 0),
        pim::MAX_GROUP,
    );
    let mut out = Vec::new();
    for (start, _) in expected {
        let key = if series.all_day {
            start.unix.saturating_add(i64::from(start.offset_min) * 60).div_euclid(DAY)
        } else {
            start.unix
        };
        if !seen.contains(&key) {
            out.push(start.unix);
        }
    }
    out
}

/// The mapped event properties of `now` whose value differs from `was`.
fn diff(now: &Map<String, Value>, was: &Map<String, Value>) -> Map<String, Value> {
    now.iter()
        .filter(|(k, _)| items::EVENT_MAPPED.contains(&k.as_str()))
        .filter(|(k, v)| was.get(*k).unwrap_or(&Value::Null) != *v)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Sends the meeting answers waiting for the account: each one's event (by its iCalendar UID) answered with
/// its action. Returns each UID with the outcome: `Ok(true)` sent, `Ok(false)` the service has no event for
/// the meeting (the answer goes another way), `Err` why it could not go (the caller tells a passing failure,
/// tried again on the next run, from a refusal).
pub(super) fn respond_all(c: &GraphClient, list: &[GraphResponse]) -> Vec<(String, Result<bool, NetError>)> {
    let mut out = Vec::new();
    for r in list.iter().take(200) {
        let result = (|| {
            let Some(resp) = Response::parse(&r.response) else {
                return Err(NetError::Invalid(format!("`{}` is not a meeting answer", r.response)));
            };
            let events = gpim::events_with_uid(c, "/me", &r.uid, 20)?;
            let Some(ev) =
                events.iter().find(|e| e.get("isOrganizer").and_then(Value::as_bool) != Some(true))
            else {
                return Ok(false);
            };
            let id = text(ev.get("id"));
            gpim::respond(c, &format!("/me/events/{}", seg(&id)), resp, &r.comment, r.send).map(|()| true)
        })();
        out.push((r.uid.clone(), result));
    }
    out
}

/// The mailbox's time zone as an IANA zone MailCraft knows (`None`: not set, unknown, or not allowed).
pub(super) fn mailbox_zone(c: &GraphClient) -> Option<String> {
    match gpim::mailbox_time_zone(c) {
        Ok(Some(z)) => match items::zone_named(&z) {
            items::Zone::Named(_, iana) => Some(iana.to_owned()),
            items::Zone::Utc => None,
        },
        Ok(None) => None,
        Err(e) => {
            log::info!("graph: mailbox time zone not read: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_addresses_round_trip() {
        for w in [
            Where::Contacts("~".into()),
            Where::Contacts("AAMk=/x+y".into()),
            Where::Todo("l1".into()),
            Where::Calendar("c 1".into()),
            Where::Shared("s".into()),
            Where::User("pat@example.com".into()),
        ] {
            let u = w.url();
            assert!(u.starts_with("https://graph.invalid/"), "{u}");
            assert_eq!(super::super::dav::canon(&u), u, "canonical already");
            assert_eq!(Where::parse(&u), Some(w.clone()));
        }
        assert!(is_shared_url(&Where::User("pat@example.com".into()).url()));
        assert!(!is_shared_url(&Where::Calendar("c".into()).url()));
        assert_eq!(Where::parse("https://jmap.invalid/calendar/c/"), None);
        assert_eq!(Where::parse("https://graph.invalid/other/c/"), None);
        assert_eq!(Where::parse("https://graph.invalid/todo/a/b/"), None);
        let coll = Where::Todo("l1".into()).url();
        let obj = Store::object_url(&coll, "AAk=+/");
        assert_eq!(Store::id_in(&coll, &obj).as_deref(), Some("AAk=+/"));
        assert_eq!(Store::id_in(&coll, &format!("{coll}a/b")), None);
        assert_eq!(Store::id_in(&Where::Todo("l2".into()).url(), &obj), None);
        assert_eq!(
            Where::User("pat@example.com".into()).calendar().as_deref(),
            Some("/users/pat%40example.com/calendar")
        );
    }

    #[test]
    fn tags_tell_series_from_single_events() {
        let single = json!({"master": {"id": "a", "changeKey": "k1"}, "exceptions": [], "cancelled": []});
        let series = json!({"master": {"id": "a", "changeKey": "k1", "recurrence": {"pattern": {}}},
                            "exceptions": [{"id": "x", "changeKey": "k2"}], "cancelled": [5]});
        assert!(group_tag(&single).starts_with("\"e"));
        assert!(group_tag(&series).starts_with("\"s"));
        let mut other = series.clone();
        other["cancelled"] = json!([6]);
        assert_ne!(group_tag(&series), group_tag(&other), "a cancelled occurrence changes the tag");
        assert_eq!(tag_of(&json!({"changeKey": "k"})), tag_of(&json!({"changeKey": "k", "x": 1})));
        let a = Map::from_iter([
            ("subject".to_owned(), json!(1)),
            ("showAs".to_owned(), json!(2)),
            ("x".to_owned(), json!(0)),
        ]);
        let b = Map::from_iter([("subject".to_owned(), json!(1)), ("showAs".to_owned(), json!(3))]);
        assert_eq!(
            diff(&a, &b).keys().collect::<Vec<_>>(),
            vec!["showAs"],
            "only mapped properties that differ"
        );
        // A series' tag starts with its master's key, so a run can tell a changed master cheaply.
        let master = series["master"].clone();
        assert_eq!(tag_master_key(&group_tag(&series)), Some(master_key(&master).as_str()));
        assert_eq!(tag_master_key(&group_tag(&single)), None);
        assert_eq!(tag_master_key("\"s0123\""), None, "an older tag: read again");
        assert_ne!(master_key(&json!({"changeKey": "k1"})), master_key(&json!({"changeKey": "k2"})));
    }

    /// A weekly series at 09:00 New York time from 1 March 2027 (four times: daylight saving time starts on
    /// 14 March), as read here; `zone`: the zone it was read in.
    fn weekly(zone: Option<&str>) -> Appointment {
        let start = items::parse_instant("2027-03-01T14:00:00Z").unwrap();
        Appointment {
            uid: "u".into(),
            start: DateTime::from_unix(start, if zone.is_some() { -300 } else { 0 }),
            end: DateTime::from_unix(start + 3600, if zone.is_some() { -300 } else { 0 }),
            tz: zone.map(str::to_owned),
            recurrence: Some(mailcraft_model::Recurrence {
                rrule: "FREQ=WEEKLY;BYDAY=MO;COUNT=4".into(),
                exdates: Vec::new(),
            }),
            ..Default::default()
        }
    }

    /// The service's instances: 09:00 New York time each Monday (14:00 UTC, 13:00 UTC from 14 March).
    fn instances(starts: &[&str]) -> Vec<Value> {
        starts
            .iter()
            .map(|t| json!({"type": "occurrence", "originalStart": format!("2027-03-{t}Z")}))
            .collect()
    }

    #[test]
    fn cancelled_occurrences_are_inferred_only_in_known_zones() {
        let window = (
            items::parse_instant("2027-02-28T00:00:00Z").unwrap(),
            items::parse_instant("2027-04-30T00:00:00Z").unwrap(),
        );
        let all = instances(&["01T14:00:00", "08T14:00:00", "15T13:00:00", "22T13:00:00"]);
        // An organizer's own zone the service names but MailCraft does not know: the series is expanded in
        // UTC here, so after the change every start would differ; nothing is taken as cancelled.
        for zone in ["tzone://Microsoft/Custom", "Customized Time Zone", "Pacific SA Standard Time", ""] {
            assert_eq!(
                missing_occurrences(&weekly(None), zone, &all, window, 0),
                Vec::<i64>::new(),
                "{zone}"
            );
        }
        // A zone it knows: nothing missing, nothing cancelled; one missing, that one.
        let ny = weekly(Some("America/New_York"));
        assert!(missing_occurrences(&ny, "Eastern Standard Time", &all, window, -300).is_empty());
        let some = instances(&["01T14:00:00", "08T14:00:00", "15T13:00:00"]);
        assert_eq!(
            missing_occurrences(&ny, "Eastern Standard Time", &some, window, -300),
            vec![items::parse_instant("2027-03-22T13:00:00Z").unwrap()]
        );
        // No instances at all: nothing is known.
        assert!(missing_occurrences(&ny, "Eastern Standard Time", &[], window, -300).is_empty());
        // UTC said outright is known.
        let utc = weekly(None);
        let in_utc = instances(&["01T14:00:00", "08T14:00:00", "15T14:00:00"]);
        assert_eq!(
            missing_occurrences(&utc, "UTC", &in_utc, window, 0),
            vec![items::parse_instant("2027-03-22T14:00:00Z").unwrap()]
        );
    }
}
