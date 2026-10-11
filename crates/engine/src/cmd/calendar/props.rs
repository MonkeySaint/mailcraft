//! Calendar folder properties ([`CalendarProps`]: colour, source and subscription, saved view, time zones).
//!
//! **Storage.** The mailbox has no per-calendar table yet, so the properties of a calendar folder are kept as
//! JSON in its `Folder::remote_name` behind [`PREFIX`] (calendar folders are never synchronised by IMAP, which
//! only maps mail folders, so the field is free there). Folder changes are journaled like any other, so the
//! properties persist and undo with the mailbox. A plain `internet:<url>` (written by earlier builds) reads as
//! an internet subscription. Moving the table to a `Mailbox` field later only needs [`read`] and [`write`]
//! changed.
//!
//! The same JSON object also carries the calendar views the user saved by name (View › Change View › Save
//! Current View As a New View, [`named_views`]) under [`NAMED_VIEWS`]; [`write`] keeps them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use mailcraft_model::calendar::{CalendarProps, CalendarSource, CalendarViewState, Subscription};
use mailcraft_model::{FolderId, FolderKind, Mailbox};

/// Key of the saved named views inside the stored JSON.
pub const NAMED_VIEWS: &str = "namedViews";
/// Most named views kept per calendar.
pub const MAX_NAMED_VIEWS: usize = 50;
/// Longest view name (characters).
pub const MAX_VIEW_NAME: usize = 64;

/// A calendar view saved by name (its date left out).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedView {
    pub name: String,
    pub view: CalendarViewState,
}

/// Marks a `remote_name` holding calendar properties.
pub const PREFIX: &str = "mailcraft-calendar:";
/// Largest stored properties text accepted when reading.
const MAX_BYTES: usize = 64 * 1024;
/// Refresh interval of a new subscription when the publisher names none (minutes).
pub const DEFAULT_REFRESH_MIN: u32 = 60;
/// Shortest and longest refresh intervals honoured (minutes).
pub const MIN_REFRESH_MIN: u32 = 15;
pub const MAX_REFRESH_MIN: u32 = 7 * 24 * 60;

/// Decodes stored properties (`None`, foreign or damaged text = defaults).
pub fn decode(remote: Option<&str>) -> CalendarProps {
    let Some(r) = remote else { return CalendarProps::default() };
    if let Some(json) = r.strip_prefix(PREFIX) {
        if json.len() > MAX_BYTES {
            return CalendarProps::default();
        }
        return serde_json::from_str(json).unwrap_or_default();
    }
    if let Some(url) = r.strip_prefix("internet:") {
        return CalendarProps {
            source: CalendarSource::Internet,
            read_only: true,
            subscription: Some(Subscription {
                url: normalize_url(url).unwrap_or_else(|| url.trim().to_owned()),
                refresh_min: DEFAULT_REFRESH_MIN,
                ..Default::default()
            }),
            ..Default::default()
        };
    }
    CalendarProps::default()
}

/// Encodes properties for storage (`None` when there is nothing to keep).
pub fn encode(p: &CalendarProps) -> Option<String> {
    if p.is_default() {
        return None;
    }
    serde_json::to_string(p).ok().map(|j| format!("{PREFIX}{j}"))
}

/// Properties of `folder` (defaults for a missing or non-calendar folder).
pub fn read(mb: &Mailbox, folder: FolderId) -> CalendarProps {
    match mb.folder(folder) {
        Some(f) if f.kind == FolderKind::Calendar => decode(f.remote_name.as_deref()),
        _ => CalendarProps::default(),
    }
}

/// The stored JSON object of `remote` (`None` when it holds no calendar properties).
fn stored_object(remote: Option<&str>) -> Option<serde_json::Map<String, Value>> {
    let json = remote?.strip_prefix(PREFIX)?;
    if json.len() > MAX_BYTES {
        return None;
    }
    match serde_json::from_str::<Value>(json).ok()? {
        Value::Object(o) => Some(o),
        _ => None,
    }
}

/// Encodes `props` with the named views `extra` (`None` when there is nothing to keep).
fn encode_with(props: &CalendarProps, named: Option<Value>) -> Option<String> {
    let named = named.filter(|v| v.as_array().is_some_and(|a| !a.is_empty()));
    let Some(named) = named else { return encode(props) };
    let mut obj = match serde_json::to_value(props).ok()? {
        Value::Object(o) => o,
        _ => serde_json::Map::new(),
    };
    obj.insert(NAMED_VIEWS.into(), named);
    serde_json::to_string(&obj).ok().map(|j| format!("{PREFIX}{j}"))
}

/// Stores `props` on calendar folder `folder` (keeping its named views). Returns true when the folder
/// changed.
pub fn write(mb: &mut Mailbox, folder: FolderId, props: &CalendarProps) -> bool {
    let Some(f) = mb.folders.get_mut(&folder) else { return false };
    if f.kind != FolderKind::Calendar {
        return false;
    }
    let named = stored_object(f.remote_name.as_deref()).and_then(|mut o| o.remove(NAMED_VIEWS));
    let new = encode_with(props, named);
    if f.remote_name == new {
        return false;
    }
    f.remote_name = new;
    true
}

/// The views saved by name on calendar `folder` (damaged entries skipped).
pub fn named_views(mb: &Mailbox, folder: FolderId) -> Vec<NamedView> {
    let Some(f) = mb.folder(folder).filter(|f| f.kind == FolderKind::Calendar) else { return Vec::new() };
    let Some(Value::Array(list)) =
        stored_object(f.remote_name.as_deref()).and_then(|mut o| o.remove(NAMED_VIEWS))
    else {
        return Vec::new();
    };
    list.into_iter()
        .filter_map(|v| serde_json::from_value::<NamedView>(v).ok())
        .filter(|v| !v.name.trim().is_empty())
        .take(MAX_NAMED_VIEWS)
        .collect()
}

/// Replaces the named views of calendar `folder` (at most [`MAX_NAMED_VIEWS`]). Returns true when the
/// folder changed.
pub fn set_named_views(mb: &mut Mailbox, folder: FolderId, views: &[NamedView]) -> bool {
    let props = read(mb, folder);
    let Some(f) = mb.folders.get_mut(&folder) else { return false };
    if f.kind != FolderKind::Calendar {
        return false;
    }
    let list: Vec<Value> =
        views.iter().take(MAX_NAMED_VIEWS).filter_map(|v| serde_json::to_value(v).ok()).collect();
    let new = encode_with(&props, Some(Value::Array(list)));
    if f.remote_name == new {
        return false;
    }
    f.remote_name = new;
    true
}

/// Reads, changes and stores the properties of `folder`. Returns true when the folder changed.
pub fn update(mb: &mut Mailbox, folder: FolderId, change: impl FnOnce(&mut CalendarProps)) -> bool {
    let mut p = read(mb, folder);
    change(&mut p);
    write(mb, folder, &p)
}

/// True when items in `folder` may not be changed (internet subscriptions).
pub fn is_read_only(mb: &Mailbox, folder: FolderId) -> bool {
    read(mb, folder).read_only
}

/// An internet calendar address as an http(s) URL: `webcal://` → `http://`, `webcals://` → `https://`.
/// `None` for other schemes or a missing host.
pub fn normalize_url(url: &str) -> Option<String> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    let (scheme, rest) = [
        ("https://", "https://"),
        ("http://", "http://"),
        ("webcals://", "https://"),
        ("webcal://", "http://"),
    ]
    .iter()
    .find_map(|(from, to)| lower.starts_with(from).then(|| (*to, url.get(from.len()..).unwrap_or(""))))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.contains(char::is_whitespace) || url.chars().any(char::is_control) {
        return None;
    }
    Some(format!("{scheme}{rest}"))
}

/// Host part of an http(s) URL (for a default calendar name).
pub fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Drop user info and port.
    let host = host.rsplit('@').next().unwrap_or(host);
    host.split(':').next().unwrap_or(host).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_legacy() {
        assert_eq!(decode(None), CalendarProps::default());
        assert_eq!(encode(&CalendarProps::default()), None);
        let p = CalendarProps { color: Some("Teal".into()), read_only: true, ..Default::default() };
        let enc = encode(&p).unwrap();
        assert!(enc.starts_with(PREFIX));
        assert_eq!(decode(Some(&enc)), p);
        let legacy = decode(Some("internet:webcal://example.org/cal.ics"));
        assert_eq!(legacy.source, CalendarSource::Internet);
        assert!(legacy.read_only);
        assert_eq!(legacy.subscription.unwrap().url, "http://example.org/cal.ics");
        // Damaged or foreign text reads as defaults.
        assert_eq!(decode(Some("mailcraft-calendar:{nope")), CalendarProps::default());
        assert_eq!(decode(Some("INBOX")), CalendarProps::default());
        let huge = format!("{PREFIX}{}", "x".repeat(MAX_BYTES + 1));
        assert_eq!(decode(Some(&huge)), CalendarProps::default());
    }

    #[test]
    fn named_views_survive_property_writes() {
        let mut mb = Mailbox::new_local("T");
        let acc = mb.default_account().unwrap().id;
        let cal = mb.role_folder(acc, mailcraft_model::FolderRole::Calendar).unwrap();
        assert!(named_views(&mb, cal).is_empty());
        let v = NamedView { name: "Weekly".into(), view: CalendarViewState::default() };
        assert!(set_named_views(&mut mb, cal, std::slice::from_ref(&v)));
        assert!(update(&mut mb, cal, |p| p.color = Some("Teal".into())));
        assert_eq!(named_views(&mb, cal), vec![v]);
        assert_eq!(read(&mb, cal).color.as_deref(), Some("Teal"));
        // Clearing the views and the colour leaves nothing stored.
        assert!(set_named_views(&mut mb, cal, &[]));
        assert!(update(&mut mb, cal, |p| p.color = None));
        assert_eq!(mb.folder(cal).unwrap().remote_name, None);
        // Damaged entries are skipped; a mail folder never takes views.
        let inbox = mb.role_folder(acc, mailcraft_model::FolderRole::Inbox).unwrap();
        assert!(!set_named_views(&mut mb, inbox, &[]));
        mb.folders.get_mut(&cal).unwrap().remote_name =
            Some(format!("{PREFIX}{{\"namedViews\":[1,{{\"name\":\"\"}},null]}}"));
        assert!(named_views(&mb, cal).is_empty());
    }

    #[test]
    fn urls() {
        assert_eq!(normalize_url("webcals://h.example/x.ics").as_deref(), Some("https://h.example/x.ics"));
        assert_eq!(normalize_url(" HTTPS://h.example ").as_deref(), Some("https://h.example"));
        assert_eq!(normalize_url("ftp://h.example/x"), None);
        assert_eq!(normalize_url("http://"), None);
        assert_eq!(normalize_url("http://a b/"), None);
        assert_eq!(url_host("https://user@cal.example.net:8443/a?b"), "cal.example.net");
    }
}
