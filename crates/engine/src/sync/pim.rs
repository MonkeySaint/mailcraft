//! Contacts, tasks and events kept on a CardDAV (RFC 6352) / CalDAV (RFC 4791) server, and calendars
//! published to a WebDAV server: everything here except the network (the native worker side is
//! `sync/dav.rs`).
//!
//! **Mapping.** An account's DAV settings ([`mailcraft_model::DavSettings`]) name the server; send/receive
//! discovers its address books, task lists and calendars. The account's Contacts folder takes the first
//! address book, its Tasks folder the first task list and its Calendar the first calendar of events; further
//! collections get folders of their own below them. The mapping (collection URL and the ETags seen at the
//! last synchronisation) lives in `AccountSync::dav`.
//!
//! **Items.** A synchronised contact or task carries a [`RemoteRef`]: resource URL, ETag, UID and the
//! fingerprint of the item as last synchronised. The fingerprint is a hash of the vCard / VTODO MailCraft
//! writes for the item, so an edit of anything the server keeps marks it changed; local-only details (a
//! contact's follow-up flag, a task's link to the mail it came from) do not.
//!
//! **Events.** The server keeps a recurring series and its exceptions (RECURRENCE-ID) as one resource; here
//! they are separate appointments sharing a UID ([`event_groups`]). The group's lead (the series, else its
//! first instance) holds the [`RemoteRef`]; the fingerprint covers the whole group, so editing one occurrence
//! uploads the resource again. A collection holding both events and tasks is shared by the Calendar and the
//! Tasks folder: each takes its own kind of object and leaves the other's alone.
//!
//! **Rules.** Changed here and unchanged there: uploaded with `If-Match`. Changed there: the server copy
//! replaces the local one unless the item was also changed here, in which case the local copy is uploaded
//! over it on the next run (the newer edit wins; nothing is lost silently mid-run). New here: uploaded with
//! `If-None-Match: *`. Gone here but known with the same ETag: deleted on the server. Gone there: removed
//! here unless changed here (then uploaded again). Every change applied here is journaled without an undo
//! step, and undo carries the synchronised state over ([`carry_over`]).

use std::collections::{BTreeMap, BTreeSet};

use mailcraft_formats::ics::{self, ImportOptions};
use mailcraft_formats::{keep, vcf};
use mailcraft_model::account::{DavFolderState, normalize_url_path};
use mailcraft_model::task::TaskRecurrence;
use mailcraft_model::{
    AccountId, Appointment, Contact, DateTime, Folder, FolderId, FolderKind, FolderRole, ItemId, Mailbox,
    RemoteRef, Task,
};

use super::Plan;
use crate::{Notice, Session};

/// Most items of one folder synchronised per run.
pub const MAX_ITEMS: usize = 20_000;

/// What a synchronised folder holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PimKind {
    Contacts,
    Tasks,
    /// Events (VEVENT) of a calendar.
    Calendar,
}

impl PimKind {
    pub fn of(kind: FolderKind) -> Option<PimKind> {
        match kind {
            FolderKind::Contacts => Some(PimKind::Contacts),
            FolderKind::Tasks => Some(PimKind::Tasks),
            FolderKind::Calendar => Some(PimKind::Calendar),
            _ => None,
        }
    }
    pub fn folder_kind(self) -> FolderKind {
        match self {
            PimKind::Contacts => FolderKind::Contacts,
            PimKind::Tasks => FolderKind::Tasks,
            PimKind::Calendar => FolderKind::Calendar,
        }
    }
    pub fn role(self) -> FolderRole {
        match self {
            PimKind::Contacts => FolderRole::Contacts,
            PimKind::Tasks => FolderRole::Tasks,
            PimKind::Calendar => FolderRole::Calendar,
        }
    }
    /// File name extension of a resource.
    pub fn extension(self) -> &'static str {
        match self {
            PimKind::Contacts => "vcf",
            PimKind::Tasks | PimKind::Calendar => "ics",
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            PimKind::Contacts => "text/vcard; charset=utf-8",
            PimKind::Tasks | PimKind::Calendar => "text/calendar; charset=utf-8",
        }
    }
    /// Progress wording ("Updating contacts…").
    pub fn label(self) -> &'static str {
        match self {
            PimKind::Contacts => "contacts",
            PimKind::Tasks => "tasks",
            PimKind::Calendar => "calendars",
        }
    }
    /// Whether the account's DAV settings turn this kind on.
    pub fn enabled(self, d: &mailcraft_model::DavSettings) -> bool {
        match self {
            PimKind::Contacts => d.contacts,
            PimKind::Tasks => d.tasks,
            PimKind::Calendar => d.calendar,
        }
    }
}

/// Most appointments read from one server resource (a series and its exceptions).
pub const MAX_GROUP: usize = 2_000;

/// FNV-1a 64: a stable fingerprint (the same on every build and platform).
pub fn fnv(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A UID safe to write into a vCard or iCalendar line and a URL path: printable, no separators.
fn clean_uid(uid: &str) -> String {
    uid.chars().filter(|c| c.is_ascii_graphic() && !matches!(c, ';' | ':' | ',' | '\\')).take(200).collect()
}

/// The contact as the vCard 4.0 MailCraft uploads (with `uid`, when given).
pub fn contact_card(c: &Contact, uid: &str) -> String {
    contact_card_as(c, uid, vcf::Version::V4)
}

/// The task as the iCalendar object (one VTODO) MailCraft uploads.
pub fn task_calendar(t: &Task, uid: &str, now: DateTime) -> String {
    let mut plain = t.clone();
    plain.remote = None;
    plain.uid = clean_uid(uid);
    crate::cmd::tasks::task_ics(&plain, None, None, &[], now)
}

/// The server copy of a resource as items keep it ([`RemoteRef::raw`]).
pub fn kept(kind: PimKind, data: &str) -> String {
    match kind {
        PimKind::Contacts => keep::kept_card(data),
        // A tasks folder holds no event, so no event's files: they stay in the kept copy.
        PimKind::Tasks => keep::kept_calendar(data, 0),
        // The files of the events `read_events` takes are the item's.
        PimKind::Calendar => keep::kept_calendar(data, MAX_GROUP),
    }
}

/// The contact's vCard written in `version` (with `uid`, when given).
fn contact_card_as(c: &Contact, uid: &str, version: vcf::Version) -> String {
    let mut plain = c.clone();
    plain.remote = None;
    let mut out = String::new();
    vcf::write_one(&plain, version, &mut out);
    let uid = clean_uid(uid);
    if !uid.is_empty()
        && let Some(end) = out.rfind("END:VCARD")
    {
        out.insert_str(end, &format!("UID:{uid}\r\n"));
    }
    out
}

/// The vCard uploaded for a changed contact: [`contact_card`], rebuilt on the server's copy (`server`, as
/// kept) so what MailCraft does not model goes back unchanged ([`mailcraft_formats::keep`]). The rebuilt
/// card is used only when it reads back exactly as MailCraft's own card does.
pub fn upload_contact(c: &Contact, uid: &str, server: Option<&str>) -> String {
    let mine = contact_card(c, uid);
    let Some(raw) = server else { return mine };
    let Some((card, _)) = read_contact(raw) else { return mine };
    let version = keep::card_version(raw);
    let base = contact_card_as(&merged_contact(card, Some(c)), uid, version);
    let Some(patched) = keep::patch_card(raw, &base, &contact_card_as(c, uid, version)) else { return mine };
    let reads =
        |text: &str| read_contact(text).map(|(x, _)| contact_fingerprint(&merged_contact(x, Some(c))));
    match (reads(&patched), reads(&mine)) {
        (Some(a), Some(b)) if a == b => patched,
        _ => mine,
    }
}

/// The iCalendar object uploaded for a changed task: [`task_calendar`] rebuilt on the server's copy (see
/// [`upload_contact`]).
pub fn upload_task(t: &Task, uid: &str, now: DateTime, server: Option<&str>, tz_offset_min: i32) -> String {
    let mine = task_calendar(t, uid, now);
    let Some(raw) = server else { return mine };
    let Some((task, _)) = read_task(raw, tz_offset_min) else { return mine };
    let base = task_calendar(&merged_task(task, Some(t)), uid, now);
    let Some(patched) = keep::patch_calendar(raw, &base, &mine, tz_offset_min) else { return mine };
    let reads =
        |text: &str| read_task(text, tz_offset_min).map(|(x, _)| task_fingerprint(&merged_task(x, Some(t))));
    match (reads(&patched), reads(&mine)) {
        (Some(a), Some(b)) if a == b => patched,
        _ => mine,
    }
}

/// The iCalendar object uploaded for a changed group of appointments: [`event_calendar`] rebuilt on the
/// server's copy (see [`upload_contact`]).
pub fn upload_events(
    items: &[&Appointment],
    uid: &str,
    now: DateTime,
    server: Option<&str>,
    tz_offset_min: i32,
) -> String {
    let mine = event_calendar(items, uid, now);
    let Some(raw) = server else { return mine };
    let Some((events, _)) = read_events(raw, tz_offset_min) else { return mine };
    let base = event_calendar(&events.iter().collect::<Vec<_>>(), uid, now);
    let Some(patched) = keep::patch_calendar(raw, &base, &mine, tz_offset_min) else { return mine };
    let reads = |text: &str| {
        read_events(text, tz_offset_min).map(|(x, _)| event_fingerprint(&x.iter().collect::<Vec<_>>()))
    };
    match (reads(&patched), reads(&mine)) {
        (Some(a), Some(b)) if a == b => patched,
        _ => mine,
    }
}

pub fn contact_fingerprint(c: &Contact) -> u64 {
    fnv(contact_card(c, "").as_bytes())
}

pub fn task_fingerprint(t: &Task) -> u64 {
    let mut plain = t.clone();
    plain.uid.clear();
    fnv(task_calendar(&plain, "", DateTime::from_unix(0, 0)).as_bytes())
}

/// Appointments of one folder that the server keeps as one resource: a series (or a lone event) with the
/// exceptions sharing its UID, or the instances of a series that is not here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventGroup {
    /// The series, else the first instance: it holds the server link.
    pub lead: ItemId,
    /// Every appointment of the group, the lead first.
    pub members: Vec<ItemId>,
    /// The UID the group shares (empty when the lead has none, or another series of the folder already uses
    /// it: the group then goes up under a new one).
    pub uid: String,
}

/// The event groups of `folder`, in item order. A second series with a UID already taken (a copy) is a group
/// of its own without a UID.
pub fn event_groups(mb: &Mailbox, folder: FolderId) -> Vec<EventGroup> {
    let mut items: Vec<&Appointment> = mb.appointments.values().filter(|a| a.folder == folder).collect();
    items.sort_by_key(|a| a.id);
    let mut groups: Vec<EventGroup> = Vec::new();
    let mut by_uid: BTreeMap<String, usize> = BTreeMap::new();
    for a in items.iter().filter(|a| a.recurrence_id.is_none()) {
        let uid = a.uid.trim();
        if !uid.is_empty() && !by_uid.contains_key(uid) {
            by_uid.insert(uid.to_owned(), groups.len());
            groups.push(EventGroup { lead: a.id, members: vec![a.id], uid: uid.to_owned() });
        } else {
            groups.push(EventGroup { lead: a.id, members: vec![a.id], uid: String::new() });
        }
    }
    for a in items.iter().filter(|a| a.recurrence_id.is_some()) {
        let uid = a.uid.trim();
        match by_uid.get(uid).and_then(|&i| groups.get_mut(i)) {
            Some(g) if !uid.is_empty() => g.members.push(a.id),
            _ => {
                if !uid.is_empty() {
                    by_uid.insert(uid.to_owned(), groups.len());
                }
                groups.push(EventGroup { lead: a.id, members: vec![a.id], uid: uid.to_owned() });
            }
        }
    }
    groups
}

/// The members of every event group of `folder`, by lead (one scan of the calendar).
fn groups_by_lead(mb: &Mailbox, folder: FolderId) -> BTreeMap<ItemId, Vec<ItemId>> {
    event_groups(mb, folder).into_iter().map(|g| (g.lead, g.members)).collect()
}

/// The iCalendar object MailCraft uploads for a group of appointments (all under `uid`): the series first,
/// then its exceptions. An instance the group overrides is not also an EXDATE of the series (RFC 5545:
/// RECURRENCE-ID names an instance of the set). Organizer and attendees say the client schedules
/// (SCHEDULE-AGENT=CLIENT): MailCraft mails the invitations, so the server must not send its own.
pub fn event_calendar(items: &[&Appointment], uid: &str, now: DateTime) -> String {
    stored_events(items, uid, now, false)
}

/// [`event_calendar`]; with `digest_files` each attached file is written as its length and hash instead of
/// its contents (for fingerprints: no base64 of large files on every run).
fn stored_events(items: &[&Appointment], uid: &str, now: DateTime, digest_files: bool) -> String {
    let uid = clean_uid(uid);
    let overridden: BTreeSet<i64> = items.iter().filter_map(|a| a.recurrence_id.map(|r| r.unix)).collect();
    let mut plain: Vec<Appointment> = items
        .iter()
        .take(MAX_GROUP)
        .map(|a| {
            let mut x = (*a).clone();
            x.remote = None;
            x.uid = uid.clone();
            if let Some(r) = x.recurrence.as_mut() {
                r.exdates.retain(|d| !overridden.contains(&d.unix));
            }
            if digest_files {
                for f in &mut x.attachments {
                    let digest = format!("{}:{:016x}", f.data.len(), fnv(&f.data));
                    f.data = std::sync::Arc::from(digest.as_bytes());
                }
            }
            x
        })
        .collect();
    plain.sort_by_key(|a| (a.recurrence_id.is_some(), a.recurrence_id.map(|r| r.unix), a.start.unix, a.id));
    let refs: Vec<&Appointment> = plain.iter().collect();
    ics::write_stored(&refs, now)
}

/// Fingerprint of a group of appointments (what the server keeps of them; not the UID).
pub fn event_fingerprint(items: &[&Appointment]) -> u64 {
    fnv(stored_events(items, "fingerprint", DateTime::from_unix(0, 0), true).as_bytes())
}

/// The appointments of a server object (the events sharing its first event's UID) and that UID; `None` when
/// it holds no event.
pub fn read_events(data: &str, tz_offset_min: i32) -> Option<(Vec<Appointment>, String)> {
    let cal = ics::read_str(data, ImportOptions { local_offset_min: tz_offset_min }).ok()?;
    let uid = cal.events.first().map(|e| e.appointment.uid.trim().to_owned())?;
    let list: Vec<Appointment> = cal
        .events
        .into_iter()
        .filter(|e| e.appointment.uid.trim() == uid)
        .take(MAX_GROUP)
        .map(|e| e.appointment)
        .collect();
    Some((list, uid))
}

/// True when `data` is an iCalendar object of the other calendar kind: a task (VTODO) without events for a
/// calendar folder, an event without tasks for a tasks folder. A collection holding both is shared.
fn other_kind(kind: PimKind, data: &str) -> bool {
    let comps = ics::parse::parse(data);
    let has = |name: &'static str| comps.iter().any(|c| c.children_named(name).next().is_some());
    match kind {
        PimKind::Calendar => has("VTODO") && !has("VEVENT"),
        PimKind::Tasks => has("VEVENT") && !has("VTODO"),
        PimKind::Contacts => false,
    }
}

/// Unfolded content lines (RFC 6350 / 5545 line folding).
fn unfold(text: &str) -> String {
    text.replace("\r\n ", "").replace("\r\n\t", "").replace("\n ", "").replace("\n\t", "")
}

/// The first card of `data` and its UID.
pub fn read_contact(data: &str) -> Option<(Contact, String)> {
    let c = vcf::read_str(data).into_iter().next()?;
    let unfolded = unfold(data);
    let uid = unfolded
        .lines()
        .map(str::trim_start)
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            let name = name.split(';').next()?.rsplit('.').next()?;
            name.eq_ignore_ascii_case("UID").then(|| value.trim().to_owned())
        })
        .unwrap_or_default();
    Some((c, uid))
}

/// The first task of `data` and its UID (with its RRULE as the task's recurrence).
pub fn read_task(data: &str, tz_offset_min: i32) -> Option<(Task, String)> {
    let cal = ics::read_str(data, ImportOptions { local_offset_min: tz_offset_min }).ok()?;
    let mut t = cal.tasks.into_iter().next()?;
    let comps = ics::parse::parse(data);
    let todo = comps.iter().flat_map(|c| c.children_named("VTODO")).next();
    let uid = todo.and_then(|c| c.text("UID")).map(|u| u.trim().to_owned()).unwrap_or_default();
    if let Some(rule) =
        todo.and_then(|c| c.prop("RRULE")).map(|p| p.value.trim().to_owned()).filter(|r| !r.is_empty())
    {
        t.recurrence = Some(TaskRecurrence { rrule: rule, start: t.due, ..Default::default() });
    }
    Some((t, uid))
}

/// A new UID for an item first uploaded from here.
fn new_uid(account: AccountId, id: ItemId, now: i64) -> String {
    let h = fnv(format!("{}:{}:{now}", account.0, id.0).as_bytes());
    format!("mailcraft-{h:016x}-{}", id.0)
}

/// Longest resource name made from a UID (before the extension).
const MAX_RESOURCE_NAME: usize = 200;
/// How much of a too-long UID's name is kept before its hash.
const RESOURCE_NAME_PREFIX: usize = 150;

/// The resource URL for a new item: `<collection><name>.<ext>`, the name being the UID with every byte other
/// than a letter, digit, `-`, `_` or `.` percent-encoded, so different UIDs never share a name. A name longer
/// than [`MAX_RESOURCE_NAME`] is cut and ends in `!` and a hash of the whole UID instead (`!` is never part of
/// an encoded name).
pub fn resource_url(collection: &str, uid: &str, kind: PimKind) -> String {
    let base = if collection.ends_with('/') { collection.to_owned() } else { format!("{collection}/") };
    let pieces: Vec<String> = uid
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let total = pieces.iter().map(String::len).fold(0usize, usize::saturating_add);
    let name = if total <= MAX_RESOURCE_NAME {
        pieces.concat()
    } else {
        let mut name = String::with_capacity(RESOURCE_NAME_PREFIX + 17);
        for p in &pieces {
            if name.len() + p.len() > RESOURCE_NAME_PREFIX {
                break;
            }
            name.push_str(p);
        }
        format!("{name}!{:016x}", fnv(uid.as_bytes()))
    };
    format!("{base}{name}.{}", kind.extension())
}

/// Another resource URL for a new item whose [`resource_url`] another item already holds on the server: the
/// name ends in `!` and a hash of the UID (a short name never holds a `!`, and a long one already ends in its
/// hash, so this one ends in a second, different hash).
pub fn alternate_resource_url(collection: &str, uid: &str, kind: PimKind) -> String {
    let first = resource_url(collection, uid, kind);
    let ext = format!(".{}", kind.extension());
    let stem = first.strip_suffix(&ext).unwrap_or(&first);
    let mut salted = Vec::with_capacity(uid.len().saturating_add(1));
    salted.extend_from_slice(uid.as_bytes());
    salted.push(0);
    format!("{stem}!{:016x}{ext}", fnv(&salted))
}

/// True when a server copy's UID (as read from its data) is the UID of local item `local` (as uploaded: the
/// characters a UID cannot carry are dropped, see `clean_uid`).
pub fn same_uid(local: &str, server: &str) -> bool {
    !local.is_empty() && !server.is_empty() && (local == server || clean_uid(local) == server)
}

/// The path of an http(s) address (from the first `/` after the host up to any `?` or `#`; `/` when none).
fn web_path(url: &str) -> &str {
    let rest = url.trim().split_once("://").map_or("", |(_, r)| r);
    let path = rest.find(['/', '?', '#']).and_then(|i| rest.get(i..)).unwrap_or("/");
    match path.split(['?', '#']).next() {
        Some(p) if p.starts_with('/') => p,
        _ => "/",
    }
}

/// Where a calendar published at `url` goes when the account's publishing location is `base`: the address
/// in its canonical form (origin and normalised path). The account's login goes with the upload, and the
/// publishing server is often the contacts and calendar server too, so the address must name a file strictly
/// inside the publishing location: the same origin, below its path, not a folder (no trailing `/`), and
/// without `.` or `..` segments, an escaped `/` or `\`, a query or a fragment.
pub fn publication_target(base: &str, url: &str) -> Result<String, String> {
    let url = url.trim();
    let base = base.trim();
    let Some(home) = web_origin(base) else {
        return Err("the account has no publishing location".into());
    };
    if web_origin(url).as_deref() != Some(home.as_str()) {
        return Err(format!(
            "`{url}` is not on the account's publishing server ({home}): a calendar is published there only"
        ));
    }
    if url.contains(['?', '#']) || url.contains(char::is_whitespace) {
        return Err(format!("`{url}` must be a plain file address (no spaces, `?` or `#`)"));
    }
    let mut inside = normalize_url_path(web_path(base));
    if !inside.ends_with('/') {
        inside.push('/');
    }
    let path = normalize_url_path(web_path(url));
    let Some(name) = path.strip_prefix(&inside) else {
        return Err(format!("`{url}` is not inside the publishing location {base}"));
    };
    if name.is_empty() || name.ends_with('/') {
        return Err(format!("`{url}` names a folder: a calendar is published as a file (…/calendar.ics)"));
    }
    for seg in name.split('/') {
        let lower = seg.to_ascii_lowercase();
        if seg.is_empty() || seg == "." || seg == ".." || lower.contains("%2f") || lower.contains("%5c") {
            return Err(format!("`{url}` must name a file inside the publishing location {base}"));
        }
    }
    Ok(format!("{home}{path}"))
}

/// `scheme://host[:port]` of an http(s) address, lower case and without the default port; `None` for
/// anything else, and for an address carrying a user name (`https://a@b/` names host `b`, which a reader
/// easily takes for `a`).
pub fn web_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "https" => "443",
        "http" => "80",
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains(['@', '\\']) {
        return None;
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let (h, after) = v6.split_once(']')?;
            let port = if after.is_empty() { "" } else { after.strip_prefix(':')? };
            (format!("[{h}]"), port)
        }
        None => match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), p),
            None => (authority.to_owned(), ""),
        },
    };
    if host.is_empty() || host == "[]" || !port.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let host = host.to_ascii_lowercase();
    let port = port.trim_start_matches('0');
    if port.is_empty() || port == default {
        Some(format!("{scheme}://{host}"))
    } else {
        Some(format!("{scheme}://{host}:{port}"))
    }
}

/// Origin and path of an address MailCraft stored (a link, a known resource, a collection). Stored addresses
/// have no query, so a `?` or `#` there is part of the path.
fn split_stored(url: &str) -> Option<(String, &str)> {
    let url = url.trim();
    let origin = web_origin(url)?;
    let rest = url.split_once("://").map_or("", |(_, r)| r);
    let path = rest.find(['/', '?', '#']).and_then(|i| rest.get(i..)).unwrap_or("/");
    Some((origin, path))
}

/// Canonical form of a stored address (origin and normalised path). What this version stores is canonical
/// already and stays as it is. An earlier version stored the path fully decoded (`/c/John Smith.vcf`, a
/// calendar named `a#b`), which a request cannot carry and which never equals the server's form: it is
/// escaped again here.
pub fn stored_canon(url: &str) -> String {
    match split_stored(url) {
        Some((origin, path)) if path.starts_with('/') => format!("{origin}{}", normalize_url_path(path)),
        Some((origin, path)) => format!("{origin}/{}", normalize_url_path(path)),
        None => url.trim().to_owned(),
    }
}

/// The form an earlier version stored addresses in (origin plus the fully decoded path), for a stored address
/// and a server's alike. Only for recognising such a link: two different resources (`a%2Bb`, `a+b`) may
/// share it.
pub fn legacy_key(url: &str) -> String {
    let Some((origin, path)) = split_stored(url) else { return url.trim().to_owned() };
    let b = path.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match hex.filter(|h| c == b'%' && h.bytes().all(|x| x.is_ascii_hexdigit())) {
            Some(h) => {
                out.push(u8::from_str_radix(h, 16).unwrap_or(b'%'));
                i += 3;
            }
            None => {
                out.push(c);
                i += 1;
            }
        }
    }
    format!("{origin}{}", String::from_utf8_lossy(&out))
}

/// The discovered address (of `found`: URL, name) of the collection stored as `stored`, in this version's
/// form or decoded as an earlier version stored it. `None` when discovery did not find it (or, decoded, more
/// than one collection would match).
pub fn discovered_form(found: &[(String, String)], stored: &str) -> Option<String> {
    let exact = stored_canon(stored);
    let urls = || found.iter().map(|(u, _)| u);
    if let Some(u) = urls().find(|u| **u == exact) {
        return Some(u.clone());
    }
    let key = legacy_key(stored);
    let mut same = urls().filter(|u| legacy_key(u) == key);
    match (same.next(), same.next()) {
        (Some(u), None) => Some(u.clone()),
        _ => None,
    }
}

/// True when the stored link `url` is inside the stored collection `collection` (either written in this
/// version's form, or decoded as an earlier version wrote it).
pub fn stored_inside(url: &str, collection: &str) -> bool {
    url.starts_with(collection)
        || stored_canon(url).starts_with(&stored_canon(collection))
        || legacy_key(url).starts_with(&legacy_key(collection))
}

/// Which contacts and tasks server (and login) an account's DAV links belong to: the server address in a
/// comparable form (origin, path without its trailing `/`) and the user name it signs in with. `None` when
/// the account has no such server. Links survive any settings change that keeps this.
pub(crate) fn dav_identity(a: &mailcraft_model::Account) -> Option<(String, String)> {
    let d = a.dav.as_ref()?;
    let url = d.url.trim();
    let origin = web_origin(url)?;
    let rest = url.split_once("://").map_or("", |(_, r)| r);
    let path = rest.find('/').and_then(|i| rest.get(i..)).unwrap_or("/");
    let path = path.split(['?', '#']).next().unwrap_or("/").trim_end_matches('/');
    let user = [d.username.trim(), a.incoming.as_ref().map(|i| i.username.trim()).unwrap_or_default()]
        .into_iter()
        .find(|u| !u.is_empty())
        .unwrap_or_else(|| a.address.trim())
        .to_owned();
    Some((format!("{origin}{path}"), user))
}

/// The UID of a server copy, and for a contact the fingerprint of the card (used to recognise a local item
/// already on the server when its link was lost).
pub fn server_identity(kind: PimKind, data: &str) -> (String, Option<u64>) {
    match kind {
        PimKind::Contacts => match read_contact(data) {
            Some((c, uid)) => (uid, Some(contact_fingerprint(&c))),
            None => (String::new(), None),
        },
        PimKind::Tasks | PimKind::Calendar => {
            let name = if kind == PimKind::Tasks { "VTODO" } else { "VEVENT" };
            let comps = ics::parse::parse(data);
            let uid = comps
                .iter()
                .flat_map(|c| c.children_named(name))
                .next()
                .and_then(|c| c.text("UID"))
                .map(|u| u.trim().to_owned())
                .unwrap_or_default();
            (uid, None)
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// What a run takes and brings back
// ---------------------------------------------------------------------------------------------------------

/// A local item as a run sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalItem {
    pub id: ItemId,
    /// Its server copy (`None`: new here).
    pub remote: Option<RemoteRef>,
    pub uid: String,
    /// What to upload when it is new or changed (`None` otherwise); the text is built by the run
    /// ([`LocalItem::body`]), off the session thread.
    pub upload: Option<Box<Upload>>,
    pub fingerprint: u64,
}

impl LocalItem {
    pub fn changed(&self) -> bool {
        self.remote.as_ref().is_none_or(|r| r.synced != self.fingerprint)
    }

    /// The text to upload (empty when there is nothing to upload). Rebuilding it on the server's kept copy
    /// reads and writes the whole object, so the run does it in the background, not the session.
    pub fn body(&self) -> String {
        self.upload.as_ref().map(|u| u.body(&self.uid)).unwrap_or_default()
    }
}

/// A new or changed item as it goes up: the item, and the server's copy as kept ([`RemoteRef::raw`]) when
/// its link counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Upload {
    Contact { contact: Box<Contact>, server: Option<String> },
    Task { task: Box<Task>, now: DateTime, server: Option<String>, tz_offset_min: i32 },
    Events { items: Vec<Appointment>, now: DateTime, server: Option<String>, tz_offset_min: i32 },
}

impl Upload {
    /// The vCard or iCalendar text uploaded under `uid` ([`upload_contact`], [`upload_task`],
    /// [`upload_events`]).
    pub fn body(&self, uid: &str) -> String {
        match self {
            Upload::Contact { contact, server } => upload_contact(contact, uid, server.as_deref()),
            Upload::Task { task, now, server, tz_offset_min } => {
                upload_task(task, uid, *now, server.as_deref(), *tz_offset_min)
            }
            Upload::Events { items, now, server, tz_offset_min } => {
                let refs: Vec<&Appointment> = items.iter().collect();
                upload_events(&refs, uid, *now, server.as_deref(), *tz_offset_min)
            }
        }
    }
}

/// One folder to synchronise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderJob {
    pub folder: FolderId,
    pub kind: PimKind,
    /// Mapped collection (`None`: the first one discovery finds).
    pub collection: Option<String>,
    pub known: BTreeMap<String, String>,
    pub items: Vec<LocalItem>,
    /// The folder holds more than [`MAX_ITEMS`] items: it is not synchronised (a partial view would make
    /// the items left out look deleted here).
    pub too_many: bool,
}

/// A calendar to upload to (or withdraw from) its publishing location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishJob {
    pub folder: FolderId,
    pub url: String,
    pub data: Vec<u8>,
    pub fingerprint: u64,
    /// Remove the published file instead.
    pub withdraw: bool,
    /// ETag of the file as MailCraft last wrote it (empty: never written). Writes and removals are
    /// conditional on it, so a file someone else put at the address is never replaced or removed.
    pub etag: String,
}

/// Why a folder is not synchronised when it holds too many items.
pub fn too_many_error() -> String {
    format!("the folder holds more than {MAX_ITEMS} items, too many to synchronise")
}

/// A server copy to apply here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    /// The local item it updates, with its fingerprint when the run started.
    pub local: Option<(ItemId, u64)>,
    pub url: String,
    pub etag: String,
    pub data: String,
}

/// What a run did to one folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderOutcome {
    pub folder: FolderId,
    pub kind: Option<PimKind>,
    pub collection: String,
    /// Local items now on the server (their new server copy; `synced` = the fingerprint uploaded).
    pub uploaded: Vec<(ItemId, RemoteRef)>,
    pub incoming: Vec<Incoming>,
    /// Local items whose server copy is gone (with their fingerprint when the run started).
    pub removed: Vec<(ItemId, u64)>,
    /// Server copies this run deleted (items removed here).
    pub deleted: Vec<String>,
    /// Unchanged items whose link was stored in another form (by an earlier version, or before the
    /// collection moved): the server's address of their copy.
    pub relinked: Vec<(ItemId, String)>,
    /// Resource URL → ETag on the server after the run.
    pub known: BTreeMap<String, String>,
    /// Items that could not be synchronised (the rest went through).
    pub errors: Vec<String>,
}

/// What a DAV run reports to the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PimEvent {
    /// Collections found on the server: (URL, name) of address books and of task lists.
    Collections {
        contacts: Option<Vec<(String, String)>>,
        tasks: Option<Vec<(String, String)>>,
        calendars: Option<Vec<(String, String)>>,
    },
    Folder(Box<FolderOutcome>),
    FolderFailed {
        folder: FolderId,
        error: String,
    },
    Published {
        folder: FolderId,
        /// The file written or removed.
        url: String,
        fingerprint: u64,
        withdrawn: bool,
        /// Done: the published file's new ETag (`*`: the server gave none; empty for a removal).
        result: Result<String, String>,
    },
}

// ---------------------------------------------------------------------------------------------------------
// Session side: which folders, building jobs
// ---------------------------------------------------------------------------------------------------------

/// The folders of `account` that synchronise with its DAV server: the default Contacts / Tasks folders when
/// turned on in the settings, plus every folder mapped to a collection.
pub fn synced_folders(mb: &Mailbox, account: AccountId) -> Vec<(FolderId, PimKind)> {
    let Some(a) = mb.account(account) else { return Vec::new() };
    let Some(dav) = a.dav.as_ref().filter(|d| !d.url.trim().is_empty()) else { return Vec::new() };
    let mut out: Vec<(FolderId, PimKind)> = Vec::new();
    for kind in [PimKind::Contacts, PimKind::Tasks, PimKind::Calendar] {
        if !kind.enabled(dav) {
            continue;
        }
        if let Some(f) = mb.role_folder(account, kind.role()) {
            out.push((f, kind));
        }
        for fid in a.sync.dav.keys() {
            if let Some(f) = mb.folder(*fid).filter(|f| f.account == account && f.kind == kind.folder_kind())
                && !out.iter().any(|(x, _)| *x == f.id)
            {
                out.push((f.id, kind));
            }
        }
    }
    out
}

/// True when Update Folder on `folder` synchronises it with a contacts or tasks server.
pub fn is_synced_folder(mb: &Mailbox, folder: FolderId) -> bool {
    mb.folder(folder).is_some_and(|f| synced_folders(mb, f.account).iter().any(|(x, _)| *x == folder))
}

/// The run's view of `folder`.
pub fn folder_job(s: &Session, account: AccountId, folder: FolderId, kind: PimKind) -> FolderJob {
    let state = s.mailbox.account(account).and_then(|a| a.sync.dav.get(&folder)).cloned();
    let collection = state.as_ref().map(|st| st.collection.clone()).filter(|c| !c.is_empty());
    let known = state.map(|st| st.known).unwrap_or_default();
    let now = s.now();
    let mut job =
        FolderJob { folder, kind, collection: collection.clone(), known, items: Vec::new(), too_many: false };
    let count = match kind {
        PimKind::Contacts => s.mailbox.contacts.values().filter(|c| c.folder == folder).count(),
        PimKind::Tasks => s.mailbox.tasks.values().filter(|t| t.folder == folder).count(),
        PimKind::Calendar => s.mailbox.appointments.values().filter(|a| a.folder == folder).count(),
    };
    if count > MAX_ITEMS {
        job.too_many = true;
        return job;
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    // The remote copy only counts when it is in this folder's collection and no other item claimed it (a
    // copied or moved item is new here).
    // The run gets the kept server copy only inside what goes up ([`Upload`]), not on the link.
    let mut keep = |r: &Option<RemoteRef>| -> Option<RemoteRef> {
        let r = r.as_ref()?;
        let inside = collection.as_deref().is_some_and(|c| stored_inside(&r.url, c));
        (inside && seen.insert(r.url.clone())).then(|| RemoteRef {
            url: r.url.clone(),
            etag: r.etag.clone(),
            uid: r.uid.clone(),
            synced: r.synced,
            raw: String::new(),
        })
    };
    // The kept server copy of an item whose link counts.
    let server = |linked: bool, r: &Option<RemoteRef>| -> Option<String> {
        r.as_ref().map(|r| r.raw.clone()).filter(|raw| linked && !raw.is_empty())
    };
    match kind {
        PimKind::Contacts => {
            for c in s.mailbox.contacts.values().filter(|c| c.folder == folder) {
                let remote = keep(&c.remote);
                let uid = remote.as_ref().map(|r| r.uid.clone()).filter(|u| !u.is_empty());
                let uid = uid.unwrap_or_else(|| new_uid(account, c.id, now.unix));
                let fingerprint = contact_fingerprint(c);
                let mut item = LocalItem { id: c.id, remote, uid, upload: None, fingerprint };
                if item.changed() {
                    let server = server(item.remote.is_some(), &c.remote);
                    item.upload = Some(Box::new(Upload::Contact { contact: Box::new(c.clone()), server }));
                }
                job.items.push(item);
            }
        }
        PimKind::Tasks => {
            let list: Vec<(&Task, Option<RemoteRef>)> = s
                .mailbox
                .tasks
                .values()
                .filter(|t| t.folder == folder)
                .map(|t| (t, keep(&t.remote)))
                .collect();
            // A UID names one item of a collection (RFC 4791 section 5.3.2.1): a task without a server copy of
            // its own whose UID another task already uses (a copy, a completed occurrence) gets a new one.
            let mut claimed: BTreeSet<String> = list
                .iter()
                .filter_map(|(_, r)| r.as_ref().map(|r| r.uid.clone()))
                .filter(|u| !u.is_empty())
                .collect();
            for (t, remote) in list {
                let uid = match remote.as_ref().map(|r| r.uid.clone()).filter(|u| !u.is_empty()) {
                    Some(u) => u,
                    None => {
                        let own = t.uid.trim();
                        if !own.is_empty() && claimed.insert(own.to_owned()) {
                            own.to_owned()
                        } else {
                            new_uid(account, t.id, now.unix)
                        }
                    }
                };
                let fingerprint = task_fingerprint(t);
                let mut item = LocalItem { id: t.id, remote, uid, upload: None, fingerprint };
                if item.changed() {
                    let server = server(item.remote.is_some(), &t.remote);
                    let task = Box::new(t.clone());
                    item.upload =
                        Some(Box::new(Upload::Task { task, now, server, tz_offset_min: s.tz_offset_min }));
                }
                job.items.push(item);
            }
        }
        PimKind::Calendar => {
            for g in event_groups(&s.mailbox, folder) {
                let items: Vec<&Appointment> =
                    g.members.iter().filter_map(|id| s.mailbox.appointments.get(id)).collect();
                let Some(lead) = items.first() else { continue };
                let remote = keep(&lead.remote);
                let uid = match remote.as_ref().map(|r| r.uid.clone()).filter(|u| !u.is_empty()) {
                    Some(u) => u,
                    None if !g.uid.is_empty() => g.uid.clone(),
                    None => new_uid(account, lead.id, now.unix),
                };
                let fingerprint = event_fingerprint(&items);
                let mut item = LocalItem { id: lead.id, remote, uid, upload: None, fingerprint };
                if item.changed() {
                    let server = server(item.remote.is_some(), &lead.remote);
                    let items: Vec<Appointment> = items.iter().map(|a| (*a).clone()).collect();
                    let tz_offset_min = s.tz_offset_min;
                    item.upload = Some(Box::new(Upload::Events { items, now, server, tz_offset_min }));
                }
                job.items.push(item);
            }
        }
    }
    job
}

/// The folders a run of `plan` synchronises for `account`.
pub fn plan_folders(s: &Session, account: AccountId, plan: &Plan) -> Vec<FolderJob> {
    if !plan.receive {
        return Vec::new();
    }
    synced_folders(&s.mailbox, account)
        .into_iter()
        .filter(|(f, _)| plan.folders.as_ref().is_none_or(|fs| fs.contains(f)))
        .map(|(f, kind)| folder_job(s, account, f, kind))
        .collect()
}

/// True when `plan` updates mail folders of `account` (not only contacts, tasks or calendars).
pub fn plan_has_mail(s: &Session, account: AccountId, plan: &Plan) -> bool {
    plan.folders.as_ref().is_none_or(|fs| {
        fs.iter()
            .any(|f| s.mailbox.folder(*f).is_none_or(|f| f.account != account || f.kind == FolderKind::Mail))
    })
}

// ---------------------------------------------------------------------------------------------------------
// Session side: applying results
// ---------------------------------------------------------------------------------------------------------

/// Applies one event of `account`'s DAV run.
pub(super) fn apply(s: &mut Session, account: AccountId, ev: PimEvent) {
    match ev {
        PimEvent::Collections { contacts, tasks, calendars } => {
            let mut new_folders = Vec::new();
            if let Some(list) = contacts {
                new_folders.extend(map_collections(s, account, PimKind::Contacts, &list));
            }
            if let Some(list) = tasks {
                new_folders.extend(map_collections(s, account, PimKind::Tasks, &list));
            }
            if let Some(list) = calendars {
                new_folders.extend(map_collections(s, account, PimKind::Calendar, &list));
            }
            if !new_folders.is_empty() {
                // Synchronise the new folders right after this run.
                let plan = Plan { send: false, receive: true, folders: Some(new_folders), user: false };
                let run = s.sync.runs.entry(account).or_default();
                match run.again.as_mut() {
                    Some(again) => again.merge(&plan),
                    None => run.again = Some(plan),
                }
            }
        }
        PimEvent::Folder(out) => apply_folder(s, account, *out),
        PimEvent::FolderFailed { folder, error } => {
            let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
            log::warn!("DAV sync of folder {name}: {error}");
            s.sync.runs.entry(account).or_default().error = Some(format!("{name}: {error}"));
        }
        PimEvent::Published { folder, url, fingerprint, withdrawn, result } => {
            published(s, account, folder, &url, fingerprint, withdrawn, result)
        }
    }
}

/// Maps discovered collections of `kind` to folders: the default folder takes the first, the others get
/// folders of their own. Folders whose collection is gone are unmapped (their items stay, as local items).
/// Returns the folders created.
fn map_collections(
    s: &mut Session,
    account: AccountId,
    kind: PimKind,
    list: &[(String, String)],
) -> Vec<FolderId> {
    let Some(default) = s.mailbox.role_folder(account, kind.role()) else { return Vec::new() };
    let on = s.mailbox.account(account).and_then(|a| a.dav.as_ref()).is_some_and(|d| kind.enabled(d));
    if !on || list.is_empty() {
        return Vec::new();
    }
    let mut mapped: BTreeMap<FolderId, String> = s
        .mailbox
        .account(account)
        .map(|a| {
            a.sync
                .dav
                .iter()
                .filter(|(f, _)| s.mailbox.folder(**f).is_some_and(|f| f.kind == kind.folder_kind()))
                .map(|(f, st)| (*f, st.collection.clone()))
                .collect()
        })
        .unwrap_or_default();
    // A collection an earlier version stored (its path decoded) is the discovered one with the same address:
    // it takes the discovered form instead of looking gone.
    for (f, c) in mapped.iter_mut() {
        if list.iter().any(|(u, _)| u == c) {
            continue;
        }
        let Some(u) = discovered_form(list, c) else { continue };
        if let Some(st) =
            s.mailbox.accounts.iter_mut().find(|a| a.id == account).and_then(|a| a.sync.dav.get_mut(f))
        {
            st.collection.clone_from(&u);
        }
        *c = u;
    }
    // Gone from the server.
    let gone: Vec<FolderId> =
        mapped.iter().filter(|(_, c)| !list.iter().any(|(u, _)| u == *c)).map(|(f, _)| *f).collect();
    for f in &gone {
        unmap(s, account, *f);
    }
    let mut taken: BTreeSet<String> =
        mapped.iter().filter(|(f, _)| !gone.contains(f)).map(|(_, c)| c.clone()).collect();
    let mut created = Vec::new();
    for (i, (url, name)) in list.iter().enumerate().take(MAX_COLLECTIONS) {
        if taken.contains(url) {
            continue;
        }
        let default_free = !mapped.contains_key(&default) || gone.contains(&default);
        let folder = if i == 0 && default_free {
            default
        } else {
            let Some(f) = create_folder(s, account, default, kind, name) else { continue };
            created.push(f);
            f
        };
        if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == account) {
            a.sync.dav.insert(folder, DavFolderState { collection: url.clone(), known: BTreeMap::new() });
        }
        taken.insert(url.clone());
    }
    created
}

/// Most collections of one kind mapped to folders.
pub const MAX_COLLECTIONS: usize = 50;

fn create_folder(
    s: &mut Session,
    account: AccountId,
    parent: FolderId,
    kind: PimKind,
    name: &str,
) -> Option<FolderId> {
    let base: String = name.trim().chars().filter(|c| !c.is_control()).take(100).collect();
    let base = if base.is_empty() { kind.role().default_name().to_owned() } else { base };
    let taken = |mb: &Mailbox, n: &str| {
        mb.folders.values().any(|f| f.parent == Some(parent) && f.name.eq_ignore_ascii_case(n))
    };
    let mut name = base.clone();
    let mut k = 2u32;
    while taken(&s.mailbox, &name) {
        if k > 999 {
            return None;
        }
        name = format!("{base} ({k})");
        k = k.saturating_add(1);
    }
    let id = FolderId(s.mailbox.alloc_id());
    s.mailbox.folders.insert(
        id,
        Folder { id, account, parent: Some(parent), name, kind: kind.folder_kind(), ..Default::default() },
    );
    s.sync.track.folders_created.insert(id);
    Some(id)
}

/// Forgets the server collection of `folder`: its items stay as local items.
fn unmap(s: &mut Session, account: AccountId, folder: FolderId) {
    if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == account) {
        a.sync.dav.remove(&folder);
    }
    for c in s.mailbox.contacts.values_mut().filter(|c| c.folder == folder) {
        c.remote = None;
    }
    for t in s.mailbox.tasks.values_mut().filter(|t| t.folder == folder) {
        t.remote = None;
    }
    for a in s.mailbox.appointments.values_mut().filter(|a| a.folder == folder) {
        a.remote = None;
    }
}

/// A contact from the server, keeping what only lives here (the follow-up flag).
fn merged_contact(new: Contact, old: Option<&Contact>) -> Contact {
    match old {
        Some(o) => Contact { id: o.id, folder: o.folder, flag: o.flag.clone(), ..new },
        None => new,
    }
}

/// A task from the server, keeping what only lives here (its mail link, task request details, privacy).
fn merged_task(new: Task, old: Option<&Task>) -> Task {
    match old {
        Some(o) => Task {
            id: o.id,
            folder: o.folder,
            from_mail: o.from_mail,
            private: o.private,
            assigned_to: o.assigned_to.clone(),
            requested_by: o.requested_by.clone(),
            request_state: o.request_state,
            last_report: o.last_report,
            // The server keeps the rule only: while it is the same, the series start and the occurrences
            // completed so far stay as they are here.
            recurrence: match (new.recurrence.clone(), o.recurrence.as_ref()) {
                (Some(n), Some(old)) if n.rrule == old.rrule => Some(old.clone()),
                (Some(n), _) => Some(n),
                (None, _) => o.recurrence.clone().filter(|r| r.regenerate_days > 0),
            },
            ..new
        },
        None => new,
    }
}

fn apply_folder(s: &mut Session, account: AccountId, out: FolderOutcome) {
    let Some(folder) = s.mailbox.folder(out.folder).filter(|f| f.account == account).map(|f| f.id) else {
        return;
    };
    let Some(kind) = out.kind else { return };
    let tz = s.tz_offset_min;
    let now = s.now();
    for (id, r) in out.uploaded {
        match kind {
            PimKind::Contacts => {
                if let Some(c) = s.mailbox.contacts.get_mut(&id) {
                    c.remote = Some(r);
                }
            }
            PimKind::Tasks => {
                if let Some(t) = s.mailbox.tasks.get_mut(&id) {
                    // The UID it went up with (a new one when its own was another task's).
                    if !r.uid.is_empty() {
                        t.uid = r.uid.clone();
                    }
                    t.remote = Some(r);
                }
            }
            PimKind::Calendar => {
                if let Some(a) = s.mailbox.appointments.get_mut(&id) {
                    // A lead that went up under a new UID (it had none, or another series' UID) takes it; it
                    // had no instances of its own.
                    if !r.uid.is_empty() {
                        a.uid = r.uid.clone();
                    }
                    a.remote = Some(r);
                }
            }
        }
    }
    for (id, url) in out.relinked {
        let remote = match kind {
            PimKind::Contacts => s.mailbox.contacts.get_mut(&id).and_then(|c| c.remote.as_mut()),
            PimKind::Tasks => s.mailbox.tasks.get_mut(&id).and_then(|t| t.remote.as_mut()),
            PimKind::Calendar => s.mailbox.appointments.get_mut(&id).and_then(|a| a.remote.as_mut()),
        };
        if let Some(r) = remote {
            r.url = url;
        }
    }
    let mut problems = out.errors;
    let mut known = out.known;
    let claimed: BTreeSet<String> = match kind {
        PimKind::Contacts => s
            .mailbox
            .contacts
            .values()
            .filter(|c| c.folder == folder)
            .filter_map(|c| c.remote.as_ref().map(|r| r.url.clone()))
            .collect(),
        PimKind::Tasks => s
            .mailbox
            .tasks
            .values()
            .filter(|t| t.folder == folder)
            .filter_map(|t| t.remote.as_ref().map(|r| r.url.clone()))
            .collect(),
        PimKind::Calendar => s
            .mailbox
            .appointments
            .values()
            .filter(|a| a.folder == folder)
            .filter_map(|a| a.remote.as_ref().map(|r| r.url.clone()))
            .collect(),
    };
    // The calendar's event groups, found once (not per item: a calendar may hold thousands of events) and
    // kept up to date as server objects replace them.
    let mut groups = match kind {
        PimKind::Calendar => groups_by_lead(&s.mailbox, folder),
        PimKind::Contacts | PimKind::Tasks => BTreeMap::new(),
    };
    for inc in out.incoming {
        // What MailCraft does not model of the server copy goes back with the next upload of the item.
        let raw = kept(kind, &inc.data);
        let remote = |uid: String, synced: u64| RemoteRef {
            url: inc.url.clone(),
            etag: inc.etag.clone(),
            uid,
            synced,
            raw: raw.clone(),
        };
        match kind {
            PimKind::Contacts => {
                let Some((card, uid)) = read_contact(&inc.data) else {
                    problems.push(format!("{} is not a vCard", inc.url));
                    // Not taken: it must not look like an item deleted here on the next run.
                    known.remove(&inc.url);
                    continue;
                };
                let old = inc.local.and_then(|(id, fp)| s.mailbox.contacts.get(&id).map(|c| (c.clone(), fp)));
                match old {
                    Some((o, fp)) if contact_fingerprint(&o) == fp => {
                        let mut c = merged_contact(card, Some(&o));
                        c.remote = Some(remote(uid, contact_fingerprint(&c)));
                        s.mailbox.contacts.insert(c.id, c);
                    }
                    Some((o, _)) => {
                        // Changed here while the run went on: this copy wins on the next run.
                        let synced = contact_fingerprint(&merged_contact(card, Some(&o)));
                        if let Some(c) = s.mailbox.contacts.get_mut(&o.id) {
                            c.remote = Some(remote(uid, synced));
                        }
                    }
                    None if claimed.contains(&inc.url) => {}
                    None => {
                        let id = ItemId(s.mailbox.alloc_id());
                        let mut c = Contact { id, folder, ..card };
                        c.remote = Some(remote(uid, contact_fingerprint(&c)));
                        s.mailbox.contacts.insert(id, c);
                        s.sync.track.pim_created.insert(id);
                    }
                }
            }
            PimKind::Tasks => {
                let Some((task, uid)) = read_task(&inc.data, tz) else {
                    // An event in a collection shared with the calendar is the calendar's.
                    if !other_kind(kind, &inc.data) {
                        problems.push(format!("{} is not a task", inc.url));
                    }
                    known.remove(&inc.url);
                    continue;
                };
                let old = inc.local.and_then(|(id, fp)| s.mailbox.tasks.get(&id).map(|t| (t.clone(), fp)));
                match old {
                    Some((o, fp)) if task_fingerprint(&o) == fp => {
                        let mut t = merged_task(task, Some(&o));
                        t.uid = if uid.is_empty() { o.uid.clone() } else { uid.clone() };
                        t.modified = Some(now);
                        t.remote = Some(remote(uid, task_fingerprint(&t)));
                        s.mailbox.tasks.insert(t.id, t);
                    }
                    Some((o, _)) => {
                        let synced = task_fingerprint(&merged_task(task, Some(&o)));
                        if let Some(t) = s.mailbox.tasks.get_mut(&o.id) {
                            t.remote = Some(remote(uid, synced));
                        }
                    }
                    None if claimed.contains(&inc.url) => {}
                    None => {
                        let id = ItemId(s.mailbox.alloc_id());
                        let mut t = Task { id, folder, uid: uid.clone(), modified: Some(now), ..task };
                        t.remote = Some(remote(uid, task_fingerprint(&t)));
                        s.mailbox.tasks.insert(id, t);
                        s.sync.track.pim_created.insert(id);
                    }
                }
            }
            PimKind::Calendar => {
                let Some((events, uid)) = read_events(&inc.data, tz) else {
                    // A task in a collection shared with the tasks folder is the tasks folder's.
                    if !other_kind(kind, &inc.data) {
                        problems.push(format!("{} is not an event", inc.url));
                    }
                    known.remove(&inc.url);
                    continue;
                };
                let old = inc.local.and_then(|(id, fp)| {
                    let members = groups.get(&id).cloned().unwrap_or_default();
                    let items: Vec<&Appointment> =
                        members.iter().filter_map(|m| s.mailbox.appointments.get(m)).collect();
                    let unchanged = event_fingerprint(&items) == fp;
                    (!items.is_empty()).then_some((id, members, unchanged))
                });
                match old {
                    Some((lead, members, true)) => {
                        let placed = place_events(s, folder, events, Some(lead), &members);
                        let fp = group_synced(s, &placed);
                        if let Some(a) = placed.first().and_then(|l| s.mailbox.appointments.get_mut(l)) {
                            a.remote = Some(remote(uid, fp));
                        }
                        groups.remove(&lead);
                        if let Some(&l) = placed.first() {
                            groups.insert(l, placed);
                        }
                    }
                    Some((lead, _, false)) => {
                        // Changed here while the run went on: this copy wins on the next run.
                        let synced = event_fingerprint(&events.iter().collect::<Vec<_>>());
                        if let Some(a) = s.mailbox.appointments.get_mut(&lead) {
                            a.remote = Some(remote(uid, synced));
                        }
                    }
                    None if claimed.contains(&inc.url) => {}
                    None => {
                        let placed = place_events(s, folder, events, None, &[]);
                        let fp = group_synced(s, &placed);
                        if let Some(a) = placed.first().and_then(|l| s.mailbox.appointments.get_mut(l)) {
                            a.remote = Some(remote(uid, fp));
                        }
                        if let Some(&l) = placed.first() {
                            groups.insert(l, placed);
                        }
                    }
                }
            }
        }
    }
    for (id, fp) in out.removed {
        match kind {
            PimKind::Contacts => {
                let unchanged = s.mailbox.contacts.get(&id).is_some_and(|c| contact_fingerprint(c) == fp);
                if unchanged {
                    s.mailbox.contacts.remove(&id);
                    s.sync.track.pim_removed.insert(id);
                } else if let Some(c) = s.mailbox.contacts.get_mut(&id) {
                    c.remote = None;
                }
            }
            PimKind::Tasks => {
                let unchanged = s.mailbox.tasks.get(&id).is_some_and(|t| task_fingerprint(t) == fp);
                if unchanged {
                    s.mailbox.tasks.remove(&id);
                    s.sync.track.pim_removed.insert(id);
                } else if let Some(t) = s.mailbox.tasks.get_mut(&id) {
                    t.remote = None;
                }
            }
            PimKind::Calendar => {
                let members = groups.remove(&id).unwrap_or_default();
                let items: Vec<&Appointment> =
                    members.iter().filter_map(|m| s.mailbox.appointments.get(m)).collect();
                let unchanged = !items.is_empty() && event_fingerprint(&items) == fp;
                if unchanged {
                    for m in members {
                        s.mailbox.appointments.remove(&m);
                        s.sync.track.pim_removed.insert(m);
                    }
                } else if let Some(a) = s.mailbox.appointments.get_mut(&id) {
                    a.remote = None;
                }
            }
        }
    }
    // Items moved out of the folder keep no link to the server copy this run deleted.
    let deleted: BTreeSet<&str> = out.deleted.iter().map(String::as_str).collect();
    if !deleted.is_empty() {
        let hit = |r: &Option<RemoteRef>| r.as_ref().is_some_and(|r| deleted.contains(r.url.as_str()));
        for c in s.mailbox.contacts.values_mut().filter(|c| c.folder != folder && hit(&c.remote)) {
            c.remote = None;
        }
        for t in s.mailbox.tasks.values_mut().filter(|t| t.folder != folder && hit(&t.remote)) {
            t.remote = None;
        }
        for a in s.mailbox.appointments.values_mut().filter(|a| a.folder != folder && hit(&a.remote)) {
            a.remote = None;
        }
    }
    if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == account) {
        a.sync.dav.insert(folder, DavFolderState { collection: out.collection, known });
    }
    if !problems.is_empty() {
        let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
        for p in problems.iter().take(5) {
            log::warn!("DAV sync of {name}: {p}");
        }
        let first = problems.first().cloned().unwrap_or_default();
        s.sync.runs.entry(account).or_default().error =
            Some(format!("{name}: {} item(s) could not be synchronised ({first})", problems.len()));
    }
}

/// Puts a server object's appointments into `folder` in place of `members` (the group they update; empty for
/// a new one): the series takes the `lead` id, an exception the id of the local instance with the same
/// RECURRENCE-ID; the rest get new ids, and members left over go. Returns the placed ids, the lead first.
fn place_events(
    s: &mut Session,
    folder: FolderId,
    events: Vec<Appointment>,
    lead: Option<ItemId>,
    members: &[ItemId],
) -> Vec<ItemId> {
    let mut free: Vec<ItemId> = members.iter().copied().filter(|m| Some(*m) != lead).collect();
    let mut lead = lead;
    let mut placed: Vec<ItemId> = Vec::new();
    let mut events = events;
    // The series first (it takes the lead's id), then the instances in time order.
    events.sort_by_key(|a| (a.recurrence_id.is_some(), a.recurrence_id.map(|r| r.unix), a.start.unix));
    for mut a in events {
        let reuse = match a.recurrence_id {
            None => lead.take(),
            Some(rid) => {
                let same = free.iter().position(|m| {
                    s.mailbox
                        .appointments
                        .get(m)
                        .and_then(|x| x.recurrence_id)
                        .is_some_and(|r| r.unix == rid.unix)
                });
                match same {
                    Some(i) => Some(free.remove(i)),
                    None => lead.take(),
                }
            }
        };
        let id = match reuse {
            Some(id) => id,
            None => {
                let id = ItemId(s.mailbox.alloc_id());
                s.sync.track.pim_created.insert(id);
                id
            }
        };
        a.id = id;
        a.folder = folder;
        a.remote = None;
        s.mailbox.appointments.insert(id, a);
        placed.push(id);
    }
    // Instances the server copy no longer has (and a lead it left without a place).
    for m in free.into_iter().chain(lead) {
        if s.mailbox.appointments.remove(&m).is_some() {
            s.sync.track.pim_removed.insert(m);
        }
    }
    placed
}

/// The fingerprint of placed appointments.
fn group_synced(s: &Session, ids: &[ItemId]) -> u64 {
    let items: Vec<&Appointment> = ids.iter().filter_map(|id| s.mailbox.appointments.get(id)).collect();
    event_fingerprint(&items)
}

/// Records the outcome of publishing (or removing) calendar `folder`'s file at `url`. A result for a
/// publication that changed meanwhile (published again elsewhere, or stopped) does not touch the new one.
fn published(
    s: &mut Session,
    account: AccountId,
    folder: FolderId,
    url: &str,
    fingerprint: u64,
    withdrawn: bool,
    result: Result<String, String>,
) {
    use crate::cmd::calendar::props;
    let mut p = props::read(&s.mailbox, folder);
    let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
    let this = |x: &mailcraft_model::calendar::Publication| x.url == url && x.account == account;
    if withdrawn {
        let stopped = p.publication.as_ref().is_some_and(|x| x.withdraw && this(x));
        match result {
            Ok(_) => {
                p.withdrawals.retain(|w| !(w.url == url && w.account == account));
                if stopped {
                    p.publication = None;
                    s.notify(Notice::Info(format!("The calendar \"{name}\" is no longer published.")));
                }
            }
            Err(e) => {
                let e: String = e.chars().take(500).collect();
                s.notify(Notice::Error(format!(
                    "Couldn't remove the published copy of the calendar \"{name}\" at {url}: {e}"
                )));
                if let Some(x) = p.publication.as_mut().filter(|_| stopped) {
                    x.last_error = Some(e);
                }
            }
        }
        props::write(&mut s.mailbox, folder, &p);
        return;
    }
    // Uploaded, but published elsewhere (or stopped) meanwhile: the file waiting to be removed is the one just
    // written, with this ETag.
    if let Ok(etag) = &result
        && let Some(w) = p.withdrawals.iter_mut().find(|w| w.url == url && w.account == account)
    {
        w.etag.clone_from(etag);
        props::write(&mut s.mailbox, folder, &p);
        return;
    }
    if let (Ok(etag), Some(x)) = (&result, p.publication.as_mut().filter(|x| x.withdraw && this(x))) {
        x.etag.clone_from(etag);
        props::write(&mut s.mailbox, folder, &p);
        return;
    }
    let Some(publication) = p.publication.as_mut().filter(|x| !x.withdraw && this(x)) else { return };
    match result {
        Ok(etag) => {
            let first = publication.last_published.is_none();
            publication.etag = etag;
            publication.fingerprint = fingerprint;
            publication.last_published = Some(s.now().unix);
            publication.last_error = None;
            if first {
                let at = publication.url.clone();
                s.notify(Notice::Info(format!("The calendar \"{name}\" is published at {at}.")));
            }
        }
        Err(e) => {
            let e: String = e.chars().take(500).collect();
            publication.last_error = Some(e.clone());
            s.notify(Notice::Error(format!("Couldn't publish the calendar \"{name}\": {e}")));
        }
    }
    props::write(&mut s.mailbox, folder, &p);
}

/// Undo/redo replaced `live` with a snapshot (now `restored`): items and server links that synchronisation
/// brought meanwhile stay (they are the server's state, not an editing step).
pub(super) fn carry_over(
    live: &Mailbox,
    restored: &mut Mailbox,
    created: &BTreeSet<ItemId>,
    removed: &BTreeSet<ItemId>,
) {
    // Accounts whose contacts and tasks server the undone step changed: the restored settings come back with
    // the links they had (the live ones point at the other server).
    let own_links: BTreeSet<AccountId> = restored
        .accounts
        .iter()
        .filter(|a| live.account(a.id).is_some_and(|l| dav_identity(l) != dav_identity(a)))
        .map(|a| a.id)
        .collect();
    for a in restored.accounts.iter_mut() {
        if let Some(l) = live.account(a.id) {
            if !own_links.contains(&a.id) {
                a.sync.dav = l.sync.dav.clone();
            }
            a.sync.shared_roots = l.sync.shared_roots.clone();
        }
    }
    let keeps_links: BTreeSet<FolderId> =
        restored.folders.values().filter(|f| own_links.contains(&f.account)).map(|f| f.id).collect();
    // Folders made for server collections meanwhile.
    let mapped: Vec<FolderId> = live.accounts.iter().flat_map(|a| a.sync.dav.keys().copied()).collect();
    for f in mapped {
        if let Some(lf) = live.folders.get(&f)
            && !restored.folders.contains_key(&f)
            && lf.parent.is_none_or(|p| restored.folders.contains_key(&p))
            && restored.account(lf.account).is_some()
        {
            restored.folders.insert(f, lf.clone());
        }
    }
    if !created.is_empty() {
        restored.next_id = restored.next_id.max(live.next_id);
    }
    for id in created {
        if let Some(c) = live.contacts.get(id)
            && !restored.contacts.contains_key(id)
            && restored.folders.contains_key(&c.folder)
        {
            restored.contacts.insert(*id, c.clone());
        }
        if let Some(t) = live.tasks.get(id)
            && !restored.tasks.contains_key(id)
            && restored.folders.contains_key(&t.folder)
        {
            restored.tasks.insert(*id, t.clone());
        }
        if let Some(a) = live.appointments.get(id)
            && !restored.appointments.contains_key(id)
            && restored.folders.contains_key(&a.folder)
        {
            restored.appointments.insert(*id, a.clone());
        }
    }
    for id in removed {
        if !live.contacts.contains_key(id) {
            restored.contacts.remove(id);
        }
        if !live.tasks.contains_key(id) {
            restored.tasks.remove(id);
        }
        if !live.appointments.contains_key(id) {
            restored.appointments.remove(id);
        }
    }
    // An item the undone command removed comes back: its server copy may be gone meanwhile, so it counts as
    // changed here and is uploaded again (instead of being taken for one deleted on the server).
    for (id, c) in restored.contacts.iter_mut() {
        match live.contacts.get(id) {
            Some(_) if keeps_links.contains(&c.folder) => {}
            Some(l) => c.remote = l.remote.clone(),
            None => {
                if let Some(r) = c.remote.as_mut() {
                    r.synced = 0;
                }
            }
        }
    }
    for (id, t) in restored.tasks.iter_mut() {
        match live.tasks.get(id) {
            Some(_) if keeps_links.contains(&t.folder) => {}
            Some(l) => t.remote = l.remote.clone(),
            None => {
                if let Some(r) = t.remote.as_mut() {
                    r.synced = 0;
                }
            }
        }
    }
    for (id, a) in restored.appointments.iter_mut() {
        match live.appointments.get(id) {
            Some(_) if keeps_links.contains(&a.folder) => {}
            Some(l) => a.remote = l.remote.clone(),
            None => {
                if let Some(r) = a.remote.as_mut() {
                    r.synced = 0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_follow_server_fields_only() {
        let c = Contact {
            first_name: "Pat".into(),
            emails: vec!["pat@example.net".into()],
            ..Default::default()
        };
        let fp = contact_fingerprint(&c);
        let flag = mailcraft_model::FlagState::Flagged {
            label: "Follow up".into(),
            start: None,
            due: None,
            reminder: None,
        };
        let flagged = Contact { flag, ..c.clone() };
        assert_eq!(contact_fingerprint(&flagged), fp, "the follow-up flag is local-only");
        let renamed = Contact { last_name: "Example".into(), ..c.clone() };
        assert_ne!(contact_fingerprint(&renamed), fp);
        let linked = Contact {
            remote: Some(RemoteRef { url: "https://dav.example.com/c/1.vcf".into(), ..Default::default() }),
            ..c.clone()
        };
        assert_eq!(contact_fingerprint(&linked), fp);
        let t = Task { id: ItemId(4), subject: "Report".into(), ..Default::default() };
        let tf = task_fingerprint(&t);
        assert_eq!(task_fingerprint(&Task { from_mail: Some(ItemId(9)), ..t.clone() }), tf);
        assert_ne!(task_fingerprint(&Task { percent_complete: 50, ..t.clone() }), tf);
        assert_eq!(task_fingerprint(&Task { uid: "x".into(), ..t.clone() }), tf, "the UID is the server's");
    }

    #[test]
    fn cards_and_tasks_round_trip_with_uids() {
        let c = Contact { first_name: "Pat".into(), last_name: "Example".into(), ..Default::default() };
        let card = contact_card(&c, "u-1;x");
        assert!(card.contains("UID:u-1x\r\nEND:VCARD"), "{card}");
        let (back, uid) = read_contact(&card).unwrap();
        assert_eq!(uid, "u-1x");
        assert_eq!(contact_fingerprint(&back), contact_fingerprint(&c));
        let folded =
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:A\r\nitem1.UID;VALUE=text:abc\r\n def\r\nEND:VCARD\r\n";
        assert_eq!(read_contact(folded).unwrap().1, "abcdef");
        assert!(read_contact("junk").is_none());
        let t = Task {
            subject: "Plan".into(),
            percent_complete: 25,
            recurrence: Some(TaskRecurrence { rrule: "FREQ=WEEKLY".into(), ..Default::default() }),
            ..Default::default()
        };
        let text = task_calendar(&t, "t-1", DateTime::from_unix(1_700_000_000, 0));
        let (back, uid) = read_task(&text, 0).unwrap();
        assert_eq!(uid, "t-1");
        assert_eq!(back.subject, "Plan");
        assert_eq!(back.recurrence.map(|r| r.rrule), Some("FREQ=WEEKLY".into()));
        assert!(read_task("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", 0).is_none());
        assert_eq!(resource_url("https://h/c", "a b/c", PimKind::Contacts), "https://h/c/a%20b%2Fc.vcf");
    }

    fn local_session() -> (Session, AccountId) {
        let s = Session::new(Mailbox::new_local("Local Folders"));
        let account = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        (s, account)
    }

    #[test]
    fn web_origins_and_dav_identity() {
        assert_eq!(web_origin("HTTPS://Dav.Example.com:443/a/b").as_deref(), Some("https://dav.example.com"));
        assert_eq!(web_origin("http://dav.example.com:8080").as_deref(), Some("http://dav.example.com:8080"));
        assert_eq!(web_origin("https://[::1]:8443/x").as_deref(), Some("https://[::1]:8443"));
        assert_eq!(web_origin("https://dav.example.com@elsewhere.example.net/"), None, "no user names");
        assert_eq!(web_origin("ftp://dav.example.com/"), None);
        assert_eq!(web_origin("https://:443/"), None);
        assert_eq!(web_origin("https://h.example.com:x/"), None);
        let mut a = mailcraft_model::Account {
            address: "pat@example.com".into(),
            dav: Some(mailcraft_model::account::DavSettings {
                url: "https://DAV.example.com/dav/".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let id = dav_identity(&a);
        assert_eq!(id, Some(("https://dav.example.com/dav".into(), "pat@example.com".into())));
        if let Some(d) = a.dav.as_mut() {
            d.url = "https://dav.example.com:443/dav".into();
        }
        assert_eq!(dav_identity(&a), id, "the same server spelled differently");
        if let Some(d) = a.dav.as_mut() {
            d.username = "pat".into();
        }
        assert_ne!(dav_identity(&a), id, "another login");
        a.dav = None;
        assert_eq!(dav_identity(&a), None);
    }

    #[test]
    fn server_edits_keep_the_series_progress() {
        let start = Some(DateTime::from_unix(1_790_000_000, 0));
        let old = Task {
            id: ItemId(3),
            recurrence: Some(TaskRecurrence {
                rrule: "FREQ=WEEKLY;COUNT=5".into(),
                start,
                completed_count: 3,
                ..Default::default()
            }),
            ..Default::default()
        };
        // What read_task makes of the server copy: the rule only.
        let rule = |r: &str| Some(TaskRecurrence { rrule: r.into(), start: None, ..Default::default() });
        let edited =
            Task { subject: "Renamed".into(), recurrence: rule("FREQ=WEEKLY;COUNT=5"), ..Default::default() };
        let m = merged_task(edited, Some(&old));
        assert_eq!(m.subject, "Renamed");
        let r = m.recurrence.unwrap();
        assert_eq!((r.completed_count, r.start), (3, start), "the progress through the series stays");
        let other = Task { recurrence: rule("FREQ=DAILY"), ..Default::default() };
        let r = merged_task(other, Some(&old)).recurrence.unwrap();
        assert_eq!((r.rrule.as_str(), r.completed_count), ("FREQ=DAILY", 0), "a new rule starts afresh");
    }

    #[test]
    fn tasks_sharing_a_uid_go_up_under_their_own() {
        let (mut s, account) = local_session();
        let folder = s.mailbox.role_folder(account, FolderRole::Tasks).unwrap();
        let collection = "https://dav.example.com/t/";
        if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == account) {
            a.sync
                .dav
                .insert(folder, DavFolderState { collection: collection.into(), known: BTreeMap::new() });
        }
        let remote = RemoteRef {
            url: format!("{collection}u1.ics"),
            etag: "\"1\"".into(),
            uid: "u1".into(),
            synced: 1,
            ..Default::default()
        };
        for (n, remote) in [(1, Some(remote.clone())), (2, Some(remote)), (3, None), (4, None)] {
            let id = ItemId(1000 + n);
            s.mailbox.tasks.insert(id, Task { id, folder, uid: "u1".into(), remote, ..Default::default() });
        }
        let job = folder_job(&s, account, folder, PimKind::Tasks);
        let uids: Vec<&str> = job.items.iter().map(|i| i.uid.as_str()).collect();
        assert_eq!(uids[0], "u1", "the task holding the server copy keeps its UID");
        let distinct: BTreeSet<&str> = uids.iter().copied().collect();
        assert_eq!(distinct.len(), 4, "{uids:?}");
        assert!(job.items.iter().skip(1).all(|i| i.remote.is_none()));
        assert!(!job.too_many);
    }

    #[test]
    fn a_folder_over_the_item_limit_is_not_synchronised() {
        let (mut s, account) = local_session();
        let folder = s.mailbox.role_folder(account, FolderRole::Contacts).unwrap();
        for n in 0..=MAX_ITEMS {
            let id = ItemId(10_000 + u64::try_from(n).unwrap());
            s.mailbox.contacts.insert(id, Contact { id, folder, ..Default::default() });
        }
        let job = folder_job(&s, account, folder, PimKind::Contacts);
        assert!(job.too_many);
        assert!(job.items.is_empty());
    }

    #[test]
    fn publish_results_apply_to_the_publication_they_were_for() {
        use mailcraft_model::calendar::{Publication, Withdrawal};
        let (mut s, account) = local_session();
        let cal = s.mailbox.role_folder(account, FolderRole::Calendar).unwrap();
        let old = "https://dav.example.com/p/old.ics";
        let new = "https://dav.example.com/p/new.ics";
        let mut p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        p.publication = Some(Publication { url: new.into(), account, ..Default::default() });
        p.withdrawals = vec![Withdrawal { url: old.into(), account, etag: "\"w\"".into() }];
        crate::cmd::calendar::props::write(&mut s.mailbox, cal, &p);
        let ev = |url: &str, withdrawn: bool| PimEvent::Published {
            folder: cal,
            url: url.into(),
            fingerprint: 7,
            withdrawn,
            result: Ok(if withdrawn { String::new() } else { "\"e7\"".into() }),
        };
        // An upload to the old address that finished late does not mark the new one published.
        apply(&mut s, account, ev(old, false));
        let p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        assert_eq!(p.publication.as_ref().unwrap().last_published, None);
        // The old file's removal clears the pending withdrawal only.
        apply(&mut s, account, ev(old, true));
        let p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        assert!(p.withdrawals.is_empty());
        assert_eq!(p.publication.as_ref().unwrap().url, new);
        apply(&mut s, account, ev(new, false));
        let p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        assert_eq!(p.publication.as_ref().unwrap().fingerprint, 7);
        assert_eq!(p.publication.as_ref().unwrap().etag, "\"e7\"", "the next upload replaces only this file");
        // Stopped, then published again before the removal came back: the new publication stays.
        let mut p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        p.publication = Some(Publication { url: old.into(), account, ..Default::default() });
        crate::cmd::calendar::props::write(&mut s.mailbox, cal, &p);
        apply(&mut s, account, ev(new, true));
        let p = crate::cmd::calendar::props::read(&s.mailbox, cal);
        assert_eq!(p.publication.map(|x| x.url).as_deref(), Some(old));
    }

    #[test]
    fn resource_names_are_distinct_and_bounded() {
        // Regression: every character outside `[A-Za-z0-9._-]` became `-`, so `a@b.com`, `a#b.com` and
        // `a/b.com` shared one resource, and the name had no length limit.
        let c = "https://dav.example.com/c/";
        let urls: Vec<String> = ["a@b.com", "a#b.com", "a/b.com", "a-b.com", "a%40b.com"]
            .iter()
            .map(|u| resource_url(c, u, PimKind::Tasks))
            .collect();
        let distinct: BTreeSet<&String> = urls.iter().collect();
        assert_eq!(distinct.len(), urls.len(), "{urls:?}");
        assert_eq!(urls[0], "https://dav.example.com/c/a%40b.com.ics");
        assert_eq!(urls[2], "https://dav.example.com/c/a%2Fb.com.ics", "a `/` never makes a sub-path");
        assert_eq!(
            resource_url("https://dav.example.com/c", "x", PimKind::Contacts),
            "https://dav.example.com/c/x.vcf"
        );
        // Long UIDs: bounded, and still different from each other.
        let long_a = format!("{}a", "\u{e9}".repeat(400));
        let long_b = format!("{}b", "\u{e9}".repeat(400));
        let (a, b) =
            (resource_url(c, &long_a, PimKind::Calendar), resource_url(c, &long_b, PimKind::Calendar));
        assert_ne!(a, b);
        for u in [&a, &b] {
            let name = u.strip_prefix(c).unwrap();
            assert!(name.len() <= MAX_RESOURCE_NAME, "{name}");
            let kept = name.split('!').next().unwrap();
            assert!(name.contains('!') && kept.len() % 6 == 0, "cut between escapes: {name}");
        }
        assert!(same_uid("urn:uuid:1", "urnuuid1") && same_uid("x", "x"));
        assert!(!same_uid("x", "y") && !same_uid("", "") && !same_uid("x", ""));
    }

    #[test]
    fn publications_stay_strictly_inside_the_publishing_location() {
        // Regression: only the origin was checked, so a publication could overwrite (or, stopped, delete) any
        // resource of the same server, an address book included.
        let base = "https://cloud.example.com/dav/me/public/";
        assert_eq!(
            publication_target(base, "https://Cloud.example.com:443/dav/me/public/team%2Dcal.ics").as_deref(),
            Ok("https://cloud.example.com/dav/me/public/team-cal.ics")
        );
        assert!(
            publication_target(
                "https://cloud.example.com/dav/me/public",
                "https://cloud.example.com/dav/me/public/a.ics"
            )
            .is_ok()
        );
        for bad in [
            "https://cloud.example.com/dav/me/contacts/x.vcf",
            "https://cloud.example.com/dav/me/contacts/",
            "https://cloud.example.com/dav/me/public/",
            "https://cloud.example.com/dav/me/public/sub/",
            "https://cloud.example.com/dav/me/public/../contacts/x.vcf",
            "https://cloud.example.com/dav/me/public/%2E%2E/contacts/x.vcf",
            "https://cloud.example.com/dav/me/public/..%2Fcontacts%2Fx.vcf",
            "https://cloud.example.com/dav/me/public/a%5C..%5Cb.ics",
            "https://cloud.example.com/dav/me/publicx/a.ics",
            "https://cloud.example.com/dav/me/public/a.ics?x=1",
            "https://elsewhere.example.net/dav/me/public/a.ics",
            "http://cloud.example.com/dav/me/public/a.ics",
        ] {
            assert!(publication_target(base, bad).is_err(), "{bad}");
        }
        assert!(publication_target("", "https://cloud.example.com/a.ics").is_err());
    }

    #[test]
    fn links_stored_by_an_earlier_version_are_recognised() {
        // Regression: an earlier version stored every path fully decoded. Read back as it was, a link with a
        // space or `#` named no resource, and one from `%2B` named another, so unchanged items looked deleted
        // on the server and were removed here.
        let h = "https://dav.example.com/c";
        assert_eq!(stored_canon(&format!("{h}/John Smith.vcf")), format!("{h}/John%20Smith.vcf"));
        assert_eq!(
            stored_canon("https://dav.example.com/cal/en.usa#holiday@group.example.com/events/"),
            "https://dav.example.com/cal/en.usa%23holiday@group.example.com/events/"
        );
        assert_eq!(stored_canon(&format!("{h}/what?.vcf")), format!("{h}/what%3F.vcf"), "no query in a link");
        assert_eq!(stored_canon("HTTPS://Dav.Example.com:443/c/x.vcf"), format!("{h}/x.vcf"));
        // This version's form stays as it is.
        for c in [format!("{h}/John%20Smith.vcf"), format!("{h}/a%2Bb.vcf"), format!("{h}/a%2Fb.vcf")] {
            assert_eq!(stored_canon(&c), c);
        }
        // Reserved escapes: the decoded form is only a key to find the server's resource by.
        assert_eq!(legacy_key(&format!("{h}/a+b.vcf")), legacy_key(&format!("{h}/a%2Bb.vcf")));
        assert_eq!(legacy_key(&format!("{h}/a,b=c.vcf")), legacy_key(&format!("{h}/a%2Cb%3Dc.vcf")));
        assert_eq!(legacy_key(&format!("{h}/John Smith.vcf")), legacy_key(&format!("{h}/John%20Smith.vcf")));
        assert_ne!(legacy_key(&format!("{h}/a.vcf")), legacy_key(&format!("{h}/b.vcf")));
        assert_eq!(legacy_key("not a url"), "not a url");
        // A legacy link is still inside its (legacy or migrated) collection.
        let coll = "https://dav.example.com/dav/me/My Book/";
        assert!(stored_inside("https://dav.example.com/dav/me/My Book/a+b.vcf", coll));
        assert!(stored_inside("https://dav.example.com/dav/me/My Book/a+b.vcf", &stored_canon(coll)));
        assert!(stored_inside("https://dav.example.com/dav/me/My%20Book/a%2Bb.vcf", coll));
        assert!(!stored_inside("https://dav.example.com/dav/me/Other/a.vcf", coll));
        let found = vec![
            ("https://dav.example.com/dav/me/My%20Book/".to_owned(), "My Book".to_owned()),
            ("https://dav.example.com/dav/me/contacts/".to_owned(), "Contacts".to_owned()),
        ];
        assert_eq!(
            discovered_form(&found, coll).as_deref(),
            Some("https://dav.example.com/dav/me/My%20Book/")
        );
        assert_eq!(discovered_form(&found, "https://dav.example.com/dav/me/gone/"), None);
    }

    #[test]
    fn a_withdrawal_saved_by_an_earlier_version_is_still_removed() {
        // Regression: an earlier version kept no ETag for a pending removal; read back with none, it counted
        // as a file never written, so the published calendar stayed public.
        use mailcraft_model::calendar::Withdrawal;
        let old: Withdrawal =
            serde_json::from_str(r#"{"url": "https://dav.example.com/p/a.ics", "account": 1}"#).unwrap();
        assert_eq!(old.etag, "*");
        // Never written (this version's form): stays so through a save.
        let never = Withdrawal { url: "https://dav.example.com/p/b.ics".into(), ..Default::default() };
        let back: Withdrawal = serde_json::from_str(&serde_json::to_string(&never).unwrap()).unwrap();
        assert_eq!(back.etag, "");
    }

    #[test]
    fn an_alternate_resource_name_differs_from_every_first_choice() {
        let c = "https://dav.example.com/c/";
        for uid in ["pat-1", "a@b.com", &"x".repeat(400)] {
            let alt = alternate_resource_url(c, uid, PimKind::Contacts);
            assert_ne!(alt, resource_url(c, uid, PimKind::Contacts), "{uid}");
            assert!(alt.starts_with(c) && alt.ends_with(".vcf") && alt.contains('!'), "{alt}");
            assert_eq!(alt, alternate_resource_url(c, uid, PimKind::Contacts), "stable");
        }
    }

    #[test]
    fn task_uploads_keep_what_the_server_copy_holds() {
        // Regression: a task went up as MailCraft rebuilt it, deleting other clients' properties and alarms.
        let server = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VTODO\r\nUID:t-1\r\n\
            DTSTAMP:20261001T000000Z\r\nSUMMARY:Report\r\nX-OTHER-CLIENT:keep me\r\nCATEGORIES:Work\r\n\
            BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT1H\r\nEND:VALARM\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let now = DateTime::from_unix(1_790_000_000, 0);
        let (task, uid) = read_task(server, 0).unwrap();
        let kept = kept(PimKind::Tasks, server);
        let edited = Task { percent_complete: 50, ..task.clone() };
        let body = upload_task(&edited, &uid, now, Some(&kept), 0);
        assert!(body.contains("X-OTHER-CLIENT:keep me") && body.contains("TRIGGER:-PT1H"), "{body}");
        assert!(body.contains("PERCENT-COMPLETE:50"), "{body}");
        let (back, _) = read_task(&body, 0).unwrap();
        assert_eq!(task_fingerprint(&merged_task(back, Some(&edited))), task_fingerprint(&edited));
        // Without a kept copy, or when the kept copy is junk: MailCraft's own.
        assert_eq!(upload_task(&edited, &uid, now, None, 0), task_calendar(&edited, &uid, now));
        assert_eq!(upload_task(&edited, &uid, now, Some("junk"), 0), task_calendar(&edited, &uid, now));
        // Contacts: a property the mapping drops survives an edit.
        let card = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c-1\r\nFN:Pat Example\r\nN:Example;Pat;;;\r\n\
            X-OTHER:1\r\nEND:VCARD\r\n";
        let (c, uid) = read_contact(card).unwrap();
        let edited = Contact { job_title: "Engineer".into(), ..c };
        let body = upload_contact(&edited, &uid, Some(&kept_card_of(card)));
        assert!(body.contains("X-OTHER:1") && body.contains("TITLE:Engineer"), "{body}");
    }

    fn kept_card_of(card: &str) -> String {
        kept(PimKind::Contacts, card)
    }

    #[test]
    fn changed_items_are_merged_with_the_server_copy_by_the_run_not_the_session() {
        // Regression: the merge with the kept server copy (several parses of an object of up to 32 MiB) ran
        // on the session thread while planning a run; it now runs with the upload, in the background.
        let (mut s, account) = local_session();
        let folder = s.mailbox.role_folder(account, FolderRole::Contacts).unwrap();
        let collection = "https://dav.example.com/card/";
        if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == account) {
            a.sync
                .dav
                .insert(folder, DavFolderState { collection: collection.into(), known: BTreeMap::new() });
        }
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:c-2\r\nFN:Pat Example\r\nN:Example;Pat;;;\r\n\
            X-EVOLUTION-SPOUSE:Alex\r\nX-OTHER:1\r\nEND:VCARD\r\n";
        let (c, uid) = read_contact(card).unwrap();
        let id = ItemId(4000);
        let remote = RemoteRef {
            url: format!("{collection}c-2.vcf"),
            etag: "\"1\"".into(),
            uid: uid.clone(),
            synced: 1,
            raw: kept_card_of(card),
        };
        let edited = Contact { id, folder, spouse: "Sam".into(), remote: Some(remote), ..c };
        s.mailbox.contacts.insert(id, edited.clone());
        let job = folder_job(&s, account, folder, PimKind::Contacts);
        let item = job.items.iter().find(|i| i.id == id).unwrap();
        assert!(item.remote.as_ref().is_some_and(|r| r.raw.is_empty()), "the link carries no copy");
        assert!(matches!(item.upload.as_deref(), Some(Upload::Contact { server: Some(_), .. })));
        let body = item.body();
        assert_eq!(body, upload_contact(&edited, &uid, Some(&kept_card_of(card))));
        // The spouse read from another client's name is replaced there too, so it shows the new one.
        assert!(body.contains("Sam") && !body.contains("Alex") && body.contains("X-OTHER:1"), "{body}");
        // An unchanged item has nothing to upload.
        let unchanged = LocalItem { upload: None, ..item.clone() };
        assert!(unchanged.body().is_empty());
    }
}
