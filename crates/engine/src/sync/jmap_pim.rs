//! Contacts and calendars of a JMAP account over JMAP itself: address books and cards (JMAP for Contacts,
//! RFC 9610: AddressBook/get, ContactCard/query, /get, /set) and calendars and events (JMAP for Calendars:
//! Calendar/get, CalendarEvent/query, /get, /set), when the server offers `urn:ietf:params:jmap:contacts` /
//! `urn:ietf:params:jmap:calendars`. Tasks have no JMAP counterpart MailCraft uses: they stay on the
//! account's CalDAV server, if it has one.
//!
//! **One synchronisation for both protocols.** [`Store`] gives the DAV synchroniser (`dav.rs`) the view of
//! a collection it expects from a WebDAV server: resources with an address and an entity tag, read and written
//! as vCard / iCalendar text. A collection's address is `https://jmap.invalid/<addressbook|calendar>/<id>/`
//! (the reserved `.invalid` name never reaches the network); an object's address is made from its UID like
//! a DAV resource name made here ([`pim::resource_url`]), and its tag is a hash of the object as the server
//! returned it. Reading converts JSContact / JSCalendar to the model ([`super::jscontact`],
//! [`super::jscalendar`]) and writes MailCraft's own vCard / iCalendar text of it; writing reads that text
//! back into the mapped properties. An update sends only the mapped properties whose value differs from the
//! server's object read the same way, so a property the edit did not touch is never rewritten (with what
//! MailCraft cannot show inside it: further organizations, a photo stored as a blob, further alerts, …), and
//! a property MailCraft does not map stays as it is. A property the edit did change is replaced whole by
//! MailCraft's version of it. Conditional writes are checked against the tag of the object as it is just
//! before the write. Events are written with `sendSchedulingMessages: false`: the server never mails
//! attendees on MailCraft's behalf.

use std::cell::RefCell;
use std::collections::BTreeMap;

use mailcraft_model::DateTime;
use mailcraft_net::dav::{Fetched, Listing, Precondition, Resource};
use mailcraft_net::jmap::{CALENDARS, CONTACTS, JmapClient};
use mailcraft_net::{NetError, http::percent_encode};
use serde_json::{Map, Value, json};

use super::pim::{self, PimKind};
use super::{jscalendar, jscontact};

use super::pim::JMAP_ORIGIN as ORIGIN;
/// Ids per Foo/get call.
const GET_BATCH: usize = 200;
/// Ids per Foo/query page.
const QUERY_PAGE: u64 = 1_000;

fn segment(kind: PimKind) -> &'static str {
    match kind {
        PimKind::Contacts => "addressbook",
        PimKind::Tasks | PimKind::Calendar => "calendar",
    }
}

/// The address a JMAP address book or calendar gets.
pub(super) fn collection_url(kind: PimKind, id: &str) -> String {
    format!("{ORIGIN}/{}/{}/", segment(kind), percent_encode(id))
}

/// The JMAP id of the collection at `url` (made by [`collection_url`] for `kind`).
fn collection_id(url: &str, kind: PimKind) -> Option<String> {
    let rest = url.trim().get(ORIGIN.len()..)?.strip_prefix('/')?;
    let rest = rest.strip_prefix(segment(kind))?.strip_prefix('/')?;
    let id = rest.trim_end_matches('/');
    if id.is_empty() || id.contains('/') {
        return None;
    }
    Some(mailcraft_net::http::percent_decode(id, false))
}

/// The capability, object type, collection type, query filter and membership property of `kind`.
fn names(kind: PimKind) -> (&'static str, &'static str, &'static str, &'static str, &'static str) {
    match kind {
        PimKind::Contacts => (CONTACTS, "ContactCard", "AddressBook", "inAddressBook", "addressBookIds"),
        PimKind::Tasks | PimKind::Calendar => {
            (CALENDARS, "CalendarEvent", "Calendar", "inCalendar", "calendarIds")
        }
    }
}

/// True when the server offers JMAP for `kind` (contacts or calendars).
pub(super) fn offers(c: &JmapClient, kind: PimKind) -> bool {
    kind != PimKind::Tasks && c.session().has_capability(names(kind).0)
}

/// The address books or calendars of the account: (address, name), the default one first.
pub(super) fn collections(c: &JmapClient, kind: PimKind) -> Result<Vec<(String, String)>, NetError> {
    let (cap, _, coll, _, _) = names(kind);
    let account = c.primary_account(cap);
    let (list, _) = c.get_objects(&format!("{coll}/get"), &account, None, None)?;
    let mut out: Vec<(bool, u64, String, String)> = list
        .iter()
        .take(pim::MAX_COLLECTIONS)
        .filter_map(|v| {
            let id = v.get("id")?.as_str()?;
            let name = v.get("name").and_then(Value::as_str).unwrap_or(id).chars().take(255).collect();
            let default = v.get("isDefault").and_then(Value::as_bool) == Some(true);
            let order = v.get("sortOrder").and_then(Value::as_u64).unwrap_or(u64::MAX);
            Some((!default, order, collection_url(kind, id), name))
        })
        .collect();
    out.sort();
    Ok(out.into_iter().map(|(_, _, u, n)| (u, n)).collect())
}

fn http(status: u16, what: &str) -> NetError {
    NetError::Http { status, body: what.to_owned() }
}

/// The object tag: a hash of the object as the server returned it.
fn tag_of(v: &Value) -> String {
    format!("\"{:016x}\"", pim::fnv(v.to_string().as_bytes()))
}

/// One collection's objects as DAV resources (see the module notes).
pub(super) struct Store<'a> {
    client: &'a JmapClient,
    account: String,
    kind: PimKind,
    tz_offset_min: i32,
    now: DateTime,
    /// Resource address → JMAP id (from listings and creations).
    ids: RefCell<BTreeMap<String, String>>,
    /// Resource address → (tag, object) as last read.
    cache: RefCell<BTreeMap<String, (String, Value)>>,
}

impl<'a> Store<'a> {
    pub(super) fn new(client: &'a JmapClient, kind: PimKind, tz_offset_min: i32, now: DateTime) -> Store<'a> {
        let account = client.primary_account(names(kind).0);
        Store {
            client,
            account,
            kind,
            tz_offset_min,
            now,
            ids: RefCell::new(BTreeMap::new()),
            cache: RefCell::new(BTreeMap::new()),
        }
    }

    fn method(&self, op: &str) -> String {
        format!("{}/{op}", names(self.kind).1)
    }

    /// The objects with `ids` (all properties).
    fn get(&self, ids: &[String]) -> Result<Vec<Value>, NetError> {
        let mut out = Vec::new();
        for chunk in ids.chunks(GET_BATCH) {
            let (list, _) = self.client.get_objects(&self.method("get"), &self.account, Some(chunk), None)?;
            out.extend(list);
        }
        Ok(out)
    }

    fn get_one(&self, id: &str) -> Result<Option<Value>, NetError> {
        Ok(self.get(&[id.to_owned()])?.into_iter().find(|v| v.get("id").and_then(Value::as_str) == Some(id)))
    }

    /// Records `v` (an object of collection `collection`) under the address its UID gives.
    fn remember(&self, collection: &str, v: Value, taken: &mut BTreeMap<String, String>) -> Option<Resource> {
        let id = v.get("id")?.as_str()?.to_owned();
        let uid = v.get("uid").and_then(Value::as_str).unwrap_or_default().to_owned();
        let mut url = if uid.trim().is_empty() {
            pim::resource_url(collection, &format!("jmap-{id}"), self.kind)
        } else {
            pim::resource_url(collection, &uid, self.kind)
        };
        if taken.get(&url).is_some_and(|other| *other != id) {
            url = pim::alternate_resource_url(collection, &uid, self.kind);
            if taken.get(&url).is_some_and(|other| *other != id) {
                url = pim::resource_url(collection, &format!("jmap-{id}"), self.kind);
            }
        }
        let url = super::dav::canon(&url);
        taken.insert(url.clone(), id.clone());
        let tag = tag_of(&v);
        self.ids.borrow_mut().insert(url.clone(), id);
        self.cache.borrow_mut().insert(url.clone(), (tag.clone(), v));
        Some(Resource { url, etag: tag, content_type: self.kind.content_type().to_owned() })
    }

    /// The listing of `collection`: every object in it, read whole (JMAP has no cheaper way to learn what
    /// changed without keeping its state per collection).
    pub(super) fn listing(&self, collection: &str) -> Result<Listing, NetError> {
        let (_, _, _, filter, _) = names(self.kind);
        let Some(cid) = collection_id(collection, self.kind) else {
            return Err(NetError::Protocol(format!("{collection} is not a JMAP collection")));
        };
        let mut ids: Vec<String> = Vec::new();
        let mut position = 0u64;
        loop {
            let r = self.client.call_one(
                &self.method("query"),
                json!({"accountId": self.account, "filter": {filter: cid}, "position": position,
                       "limit": QUERY_PAGE, "calculateTotal": true}),
            )?;
            let page: Vec<String> = r
                .get("ids")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default();
            if page.is_empty() {
                break;
            }
            position = position.saturating_add(u64::try_from(page.len()).unwrap_or(u64::MAX));
            ids.extend(page);
            if ids.len() > pim::MAX_ITEMS {
                // A partial listing would make what it leaves out look deleted there.
                return Err(NetError::Protocol(pim::too_many_error()));
            }
            let total = r.get("total").and_then(Value::as_u64);
            if total.is_some_and(|t| position >= t) {
                break;
            }
        }
        let mut resources = Vec::new();
        let mut taken: BTreeMap<String, String> = BTreeMap::new();
        let mut dropped = 0usize;
        let listed = self.get(&ids)?;
        if listed.len() < ids.len() {
            // Some went away between the query and the get: the listing is not complete.
            dropped = ids.len() - listed.len();
        }
        for v in listed {
            match self.remember(collection, v, &mut taken) {
                Some(r) => resources.push(r),
                None => dropped += 1,
            }
        }
        Ok(Listing { collection: collection.to_owned(), resources, dropped })
    }

    /// The text of the objects at `urls` (MailCraft's own vCard / iCalendar of each).
    pub(super) fn multiget(&self, urls: &[String]) -> Result<Vec<Fetched>, NetError> {
        let missing: Vec<String> = {
            let cache = self.cache.borrow();
            let ids = self.ids.borrow();
            urls.iter().filter(|u| !cache.contains_key(*u)).filter_map(|u| ids.get(u).cloned()).collect()
        };
        if !missing.is_empty() {
            let got = self.get(&missing)?;
            let ids = self.ids.borrow().clone();
            for v in got {
                let id = v.get("id").and_then(Value::as_str).unwrap_or_default();
                if let Some((url, _)) = ids.iter().find(|(_, i)| *i == id) {
                    self.cache.borrow_mut().insert(url.clone(), (tag_of(&v), v));
                }
            }
        }
        let cache = self.cache.borrow();
        Ok(urls
            .iter()
            .filter_map(|u| {
                let (tag, v) = cache.get(u)?;
                let data = self.text_of(v)?;
                Some(Fetched { url: u.clone(), etag: tag.clone(), data })
            })
            .collect())
    }

    fn text_of(&self, v: &Value) -> Option<String> {
        match self.kind {
            PimKind::Contacts => {
                let (c, uid) = jscontact::to_contact(v);
                Some(pim::contact_card(&c, &uid))
            }
            PimKind::Tasks | PimKind::Calendar => {
                let (items, uid) = jscalendar::to_appointments(v, self.tz_offset_min)?;
                let refs: Vec<&mailcraft_model::Appointment> = items.iter().collect();
                Some(pim::event_calendar(&refs, &uid, self.now))
            }
        }
    }

    /// The object properties for `data` (vCard / iCalendar text) and the mapped property names.
    fn props_of(&self, data: &[u8]) -> Option<(Map<String, Value>, &'static [&'static str])> {
        let text = String::from_utf8_lossy(data);
        match self.kind {
            PimKind::Contacts => {
                let (c, uid) = pim::read_contact(&text)?;
                Some((jscontact::from_contact(&c, &uid), &jscontact::MAPPED))
            }
            PimKind::Tasks | PimKind::Calendar => {
                let (items, uid) = pim::read_events(&text, self.tz_offset_min)?;
                Some((jscalendar::from_appointments(&items, &uid), &jscalendar::MAPPED))
            }
        }
    }

    fn set(
        &self,
        create: Map<String, Value>,
        update: Map<String, Value>,
        destroy: &[String],
    ) -> Result<mailcraft_net::jmap::SetResult, NetError> {
        let mut extra = Map::new();
        if self.kind != PimKind::Contacts {
            extra.insert("sendSchedulingMessages".into(), json!(false));
        }
        self.client.set_objects(&self.method("set"), &self.account, create, update, destroy, extra)
    }

    /// The object's tag now (`None`: it is gone).
    fn current_tag(&self, id: &str) -> Result<Option<String>, NetError> {
        Ok(self.get_one(id)?.map(|v| tag_of(&v)))
    }

    /// Reads the object back after a write: its new tag (also cached).
    fn reread(&self, url: &str, id: &str) -> Result<Option<String>, NetError> {
        let Some(v) = self.get_one(id)? else { return Ok(None) };
        let tag = tag_of(&v);
        self.ids.borrow_mut().insert(url.to_owned(), id.to_owned());
        self.cache.borrow_mut().insert(url.to_owned(), (tag.clone(), v));
        Ok(Some(tag))
    }

    /// Writes `data` at `url` in `collection` (see [`mailcraft_net::dav::DavClient::put`]: a failed
    /// precondition is HTTP 412).
    pub(super) fn put(
        &self,
        collection: &str,
        url: &str,
        data: &[u8],
        pre: &Precondition,
    ) -> Result<Option<String>, NetError> {
        let Some((props, mapped)) = self.props_of(data) else {
            return Err(http(400, "the item could not be read"));
        };
        let existing = self.ids.borrow().get(url).cloned();
        match (pre, existing) {
            (Precondition::Create, Some(_)) => Err(http(412, "exists")),
            (Precondition::Match(_), None) => Err(http(412, "gone")),
            (Precondition::Create | Precondition::Any, None) => {
                let (_, _, _, _, member) = names(self.kind);
                let Some(cid) = collection_id(collection, self.kind) else {
                    return Err(NetError::Protocol(format!("{collection} is not a JMAP collection")));
                };
                let mut obj: Map<String, Value> = props.into_iter().filter(|(_, v)| !v.is_null()).collect();
                obj.insert(member.into(), json!({ cid: true }));
                let mut create = Map::new();
                create.insert("k".into(), Value::Object(obj));
                let r = self.set(create, Map::new(), &[])?;
                let Some(id) = r.created.get("k").and_then(|v| v.get("id")).and_then(Value::as_str) else {
                    let why = r.not_created.get("k").map(|e| format!("{} {}", e.kind, e.description));
                    return Err(http(400, &why.unwrap_or_else(|| "not created".into())));
                };
                self.reread(url, id)
            }
            (Precondition::Match(_) | Precondition::Any, Some(id)) => {
                let Some(current) = self.get_one(&id)? else { return Err(http(412, "gone")) };
                if let Precondition::Match(expected) = pre
                    && tag_of(&current) != *expected
                {
                    return Err(http(412, "changed"));
                }
                // Only the properties this edit changed are written: the server's object is read the way the
                // edited copy was (through the model), and a property whose value comes out the same is left
                // alone, with everything MailCraft does not show in it (a second organization, a photo
                // stored as a blob, further alerts, …).
                let base = self.text_of(&current).and_then(|t| self.props_of(t.as_bytes())).map(|(b, _)| b);
                let mut patch = Map::new();
                for k in mapped {
                    let now = props.get(*k).cloned().unwrap_or(Value::Null);
                    let was = base.as_ref().map(|b| b.get(*k).cloned().unwrap_or(Value::Null));
                    if was.as_ref() != Some(&now) {
                        patch.insert((*k).to_owned(), now);
                    }
                }
                if patch.is_empty() {
                    return self.reread(url, &id);
                }
                let mut update = Map::new();
                update.insert(id.clone(), Value::Object(patch));
                let r = self.set(Map::new(), update, &[])?;
                if let Some(e) = r.not_updated.get(&id) {
                    return Err(if e.kind == "notFound" {
                        http(412, "gone")
                    } else {
                        http(400, &format!("{} {}", e.kind, e.description))
                    });
                }
                self.reread(url, &id)
            }
        }
    }

    /// Removes the object at `url` (only while it still has `etag`, when given). One already gone is fine.
    pub(super) fn delete(&self, url: &str, etag: Option<&str>) -> Result<(), NetError> {
        let Some(id) = self.ids.borrow().get(url).cloned() else { return Ok(()) };
        if let Some(expected) = etag {
            match self.current_tag(&id)? {
                None => return Ok(()),
                Some(now) if now != expected => return Err(http(412, "changed")),
                Some(_) => {}
            }
        }
        let r = self.set(Map::new(), Map::new(), std::slice::from_ref(&id))?;
        match r.not_destroyed.get(&id) {
            Some(e) if e.kind != "notFound" => Err(http(400, &format!("{} {}", e.kind, e.description))),
            _ => {
                self.ids.borrow_mut().remove(url);
                self.cache.borrow_mut().remove(url);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_addresses() {
        let u = collection_url(PimKind::Contacts, "b 1");
        assert_eq!(u, "https://jmap.invalid/addressbook/b%201/");
        assert!(pim::is_jmap_url(&u));
        assert_eq!(collection_id(&u, PimKind::Contacts).as_deref(), Some("b 1"));
        assert_eq!(collection_id(&u, PimKind::Calendar), None);
        assert_eq!(collection_id("https://dav.example.com/addressbook/b/", PimKind::Contacts), None);
        assert!(!pim::is_jmap_url("https://dav.example.com/"));
        let c = collection_url(PimKind::Calendar, "c");
        assert_eq!(collection_id(&c, PimKind::Calendar).as_deref(), Some("c"));
        assert_eq!(collection_id(&format!("{c}x/"), PimKind::Calendar), None);
    }
}
