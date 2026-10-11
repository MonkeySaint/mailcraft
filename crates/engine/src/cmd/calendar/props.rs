//! Calendar folder properties ([`CalendarProps`]: colour, source and subscription, saved view, time zones).
//!
//! **Storage.** The mailbox has no per-calendar table yet, so the properties of a calendar folder are kept as
//! JSON in its `Folder::remote_name` behind [`PREFIX`] (calendar folders are never synchronised by IMAP, which
//! only maps mail folders, so the field is free there). Folder changes are journaled like any other, so the
//! properties persist and undo with the mailbox. A plain `internet:<url>` (written by earlier builds) reads as
//! an internet subscription. Moving the table to a `Mailbox` field later only needs [`read`] and [`write`]
//! changed.

use mailcraft_model::calendar::{CalendarProps, CalendarSource, Subscription};
use mailcraft_model::{FolderId, FolderKind, Mailbox};

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

/// Stores `props` on calendar folder `folder`. Returns true when the folder changed.
pub fn write(mb: &mut Mailbox, folder: FolderId, props: &CalendarProps) -> bool {
    let Some(f) = mb.folders.get_mut(&folder) else { return false };
    if f.kind != FolderKind::Calendar {
        return false;
    }
    let new = encode(props);
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
    fn urls() {
        assert_eq!(normalize_url("webcals://h.example/x.ics").as_deref(), Some("https://h.example/x.ics"));
        assert_eq!(normalize_url(" HTTPS://h.example ").as_deref(), Some("https://h.example"));
        assert_eq!(normalize_url("ftp://h.example/x"), None);
        assert_eq!(normalize_url("http://"), None);
        assert_eq!(normalize_url("http://a b/"), None);
        assert_eq!(url_host("https://user@cal.example.net:8443/a?b"), "cal.example.net");
    }
}
