//! Internet calendars (Add Calendar › From Internet) and calendars from .ics files (From File).
//!
//! A subscription is a read-only calendar folder whose properties hold the URL ([`super::props`]). Refreshing
//! fetches the URL with `mailcraft-net`'s HTTP client (conditional GET with the stored ETag /
//! Last-Modified), parses it with `mailcraft-formats` and replaces the folder's items, keeping item ids for
//! events whose UID (and RECURRENCE-ID) were there before so selections and open windows survive. The refresh
//! interval is the publisher's `REFRESH-INTERVAL` (RFC 7986) or `X-PUBLISHED-TTL`, else an hour.
//!
//! Fetching blocks, so the UI fetches on a worker thread ([`fetch`]) and hands the result to
//! `calendar.refreshCalendar` (`data` / `notModified` / `error` params); without those the command fetches
//! itself (CLI, MCP, Send/Receive). The browser build has no sockets: there a refresh needs `data`.

use std::collections::BTreeMap;

use mailcraft_formats::ics::{self, ImportOptions};
use mailcraft_model::calendar::Subscription;
use mailcraft_model::{self as mbx, FolderId, ItemId};

use super::{new_uid, props, tz};
use crate::{CmdError, Session};

/// Most events kept from one calendar.
pub const MAX_EVENTS: usize = 50_000;

/// What a fetch brought back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fetched {
    /// 304: the calendar did not change since the stored validators.
    NotModified,
    Data {
        body: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// Calendar-level details of an iCalendar text: (name from `NAME` / `X-WR-CALNAME`, refresh minutes).
pub fn calendar_meta(data: &[u8]) -> (Option<String>, Option<u32>) {
    let text = String::from_utf8_lossy(data.get(..data.len().min(ics::MAX_ICS_BYTES)).unwrap_or_default());
    let top = ics::parse::parse(&text);
    let Some(cal) = top.iter().find(|c| c.name == "VCALENDAR") else { return (None, None) };
    let name = ["NAME", "X-WR-CALNAME"]
        .iter()
        .find_map(|k| cal.text(k))
        .map(|n| n.trim().chars().filter(|c| !c.is_control()).take(200).collect::<String>())
        .filter(|n| !n.is_empty());
    let refresh = ["REFRESH-INTERVAL", "X-PUBLISHED-TTL"]
        .iter()
        .find_map(|k| cal.prop(k))
        .and_then(|p| ics::value::parse_duration(&p.value))
        .filter(|secs| *secs > 0)
        .map(|secs| {
            let min = u32::try_from(secs / 60).unwrap_or(props::MAX_REFRESH_MIN);
            min.clamp(props::MIN_REFRESH_MIN, props::MAX_REFRESH_MIN)
        });
    (name, refresh)
}

/// Replaces the items of calendar `folder` with the events of `data`. Returns the number of events kept.
/// `reminders: false` drops the events' alarms (an internet calendar does not pop up reminders).
pub fn replace_items(
    s: &mut Session,
    folder: FolderId,
    data: &[u8],
    reminders: bool,
) -> Result<usize, CmdError> {
    let cal = ics::read(data, ImportOptions { local_offset_min: s.tz_offset_min })
        .map_err(|e| CmdError::Failed(format!("not an iCalendar calendar: {e}")))?;
    let key = |uid: &str, rid: Option<i64>| (uid.trim().to_ascii_lowercase(), rid);
    let old: BTreeMap<(String, Option<i64>), ItemId> = s
        .mailbox
        .appointments
        .values()
        .filter(|a| a.folder == folder && !a.uid.trim().is_empty())
        .map(|a| (key(&a.uid, a.recurrence_id.map(|r| r.unix)), a.id))
        .collect();
    s.mailbox.appointments.retain(|_, a| a.folder != folder);
    let stamp = s.now().unix;
    let mut kept = 0usize;
    let mut used = std::collections::BTreeSet::new();
    for ev in cal.events.into_iter().take(MAX_EVENTS) {
        let mut a = ev.appointment;
        a.folder = folder;
        if !reminders {
            a.reminder_min = None;
        }
        a.recurrence_id = a.recurrence_id.or(ev.recurrence_id);
        // The reader names the IANA zone (also for a zone the file defines under another name).
        if a.all_day || a.zone().is_some_and(|z| !tz::known(z)) {
            a.tz = None;
        }
        let id = match old.get(&key(&a.uid, a.recurrence_id.map(|r| r.unix))) {
            Some(id) if !a.uid.trim().is_empty() && used.insert(*id) => *id,
            _ => ItemId(s.mailbox.alloc_id()),
        };
        a.id = id;
        if a.uid.trim().is_empty() {
            a.uid = new_uid(s, id, stamp);
        }
        s.mailbox.appointments.insert(id, a);
        kept += 1;
    }
    Ok(kept)
}

/// Records a fetch outcome on subscription `folder`: items replaced (data), validators and times kept, or the
/// error noted. Returns `{"folder", "events"? , "notModified"?, "error"?}`.
pub fn apply(s: &mut Session, folder: FolderId, outcome: Result<Fetched, String>) -> serde_json::Value {
    let now = s.now().unix;
    let mut out = serde_json::Map::new();
    out.insert("folder".into(), folder.0.into());
    let mut sub = props::read(&s.mailbox, folder).subscription.unwrap_or_default();
    sub.last_fetch = Some(now);
    match outcome {
        Ok(Fetched::NotModified) => {
            sub.last_error = None;
            out.insert("notModified".into(), true.into());
        }
        Ok(Fetched::Data { body, etag, last_modified }) => match replace_items(s, folder, &body, false) {
            Ok(n) => {
                let (_, refresh) = calendar_meta(&body);
                if let Some(r) = refresh {
                    sub.refresh_min = r;
                }
                sub.etag = etag.filter(|e| e.len() <= 1024);
                sub.last_modified = last_modified.filter(|e| e.len() <= 1024);
                sub.last_update = Some(now);
                sub.last_error = None;
                out.insert("events".into(), n.into());
            }
            Err(e) => {
                sub.last_error = Some(e.to_string());
                out.insert("error".into(), e.to_string().into());
            }
        },
        Err(e) => {
            let e: String = e.chars().take(500).collect();
            sub.last_error = Some(e.clone());
            out.insert("error".into(), e.into());
        }
    }
    if sub.refresh_min == 0 {
        sub.refresh_min = props::DEFAULT_REFRESH_MIN;
    }
    props::update(&mut s.mailbox, folder, |p| p.subscription = Some(sub));
    serde_json::Value::Object(out)
}

/// Applies a fetch the UI made in the background (a refresh that came due, or the Refresh button): like
/// `calendar.refreshCalendar` with the fetched result, but as synchronisation rather than an editing step.
/// The changes are journaled without an undo entry, so Undo still undoes the user's last action instead of a
/// refresh (and cannot bring an older copy of the calendar back), and the redo history survives. Returns the
/// same object as one entry of `calendar.refreshCalendar`'s `refreshed` list. Never panics: a panic while
/// applying restores the mailbox.
pub fn apply_background(
    s: &mut Session,
    folder: FolderId,
    outcome: Result<Fetched, String>,
) -> serde_json::Value {
    if subscription(s, folder).is_none() {
        return serde_json::json!({ "folder": folder.0, "error": "not an internet calendar" });
    }
    let before = s.mailbox.clone();
    let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| apply(s, folder, outcome)));
    match applied {
        Ok(v) => {
            let ops = crate::session::diff_ops(&before, &s.mailbox);
            s.journal(ops);
            s.tidy_selection();
            v
        }
        Err(_) => {
            log::error!("applying an internet calendar refresh panicked; the mailbox was restored");
            s.mailbox = before;
            serde_json::json!({ "folder": folder.0, "error": "the calendar could not be updated" })
        }
    }
}

/// Undo/redo replaced `live` with a snapshot (now `restored`): an internet calendar present in both keeps
/// what its last refresh brought (its items and download state are the publisher's, not an editing step);
/// its other properties (colour, saved view) come back with the snapshot.
pub(crate) fn carry_over(live: &mbx::Mailbox, restored: &mut mbx::Mailbox) {
    let shared: Vec<(FolderId, Subscription)> = restored
        .folders
        .keys()
        .filter_map(|&f| {
            let mine = props::read(restored, f).subscription?;
            let theirs = props::read(live, f).subscription?;
            (mine.url == theirs.url && mine != theirs).then_some((f, theirs))
        })
        .collect();
    for (f, sub) in shared {
        restored.next_id = restored.next_id.max(live.next_id);
        // Items of other folders never give way (a read-only calendar's items cannot have moved out).
        let foreign = |id: &ItemId, r: &mbx::Mailbox| r.appointments.get(id).is_some_and(|a| a.folder != f);
        let incoming: Vec<mbx::Appointment> = live
            .appointments
            .values()
            .filter(|a| a.folder == f && !foreign(&a.id, restored))
            .cloned()
            .collect();
        restored.appointments.retain(|_, a| a.folder != f);
        for a in incoming {
            restored.appointments.insert(a.id, a);
        }
        props::update(restored, f, |p| p.subscription = Some(sub));
    }
}

/// A subscription to fetch: folder, URL and the stored validators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    pub folder: FolderId,
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

fn due_of(folder: FolderId, sub: &Subscription) -> Due {
    Due { folder, url: sub.url.clone(), etag: sub.etag.clone(), last_modified: sub.last_modified.clone() }
}

/// Every subscription (`all`), or those whose refresh interval has passed.
pub fn subscriptions(s: &Session, all: bool) -> Vec<Due> {
    let now = s.now().unix;
    s.mailbox
        .folders
        .values()
        .filter_map(|f| {
            let p = props::read(&s.mailbox, f.id);
            let sub = p.subscription.filter(|x| !x.url.is_empty())?;
            let every = i64::from(sub.refresh_min.max(props::MIN_REFRESH_MIN)).saturating_mul(60);
            let due = all || sub.last_fetch.is_none_or(|t| now.saturating_sub(t) >= every || t > now);
            due.then(|| due_of(f.id, &sub))
        })
        .collect()
}

/// The subscription of `folder`, if it is one.
pub fn subscription(s: &Session, folder: FolderId) -> Option<Due> {
    props::read(&s.mailbox, folder).subscription.filter(|x| !x.url.is_empty()).map(|x| due_of(folder, &x))
}

/// Fetches an internet calendar (blocking; call off the UI thread). Errors are user-facing text.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch(d: &Due) -> Result<Fetched, String> {
    use mailcraft_net::http::HttpClient;
    let url = props::normalize_url(&d.url)
        .ok_or_else(|| format!("`{}` is not an internet calendar address", d.url))?;
    let client = HttpClient::new(mailcraft_net::ConnectOptions {
        connect_timeout: std::time::Duration::from_secs(20),
        io_timeout: std::time::Duration::from_secs(60),
        ..Default::default()
    });
    let client = HttpClient { max_body: ics::MAX_ICS_BYTES, ..client };
    let mut headers: Vec<(&str, &str)> = vec![("Accept", "text/calendar, */*;q=0.5")];
    if let Some(e) = d.etag.as_deref().filter(|e| !e.contains(['\r', '\n'])) {
        headers.push(("If-None-Match", e));
    }
    if let Some(m) = d.last_modified.as_deref().filter(|e| !e.contains(['\r', '\n'])) {
        headers.push(("If-Modified-Since", m));
    }
    let resp = client.get(&url, &headers).map_err(|e| format!("could not fetch {url}: {e}"))?;
    match resp.status {
        304 => Ok(Fetched::NotModified),
        200..=299 => Ok(Fetched::Data {
            etag: resp.header("etag").map(str::to_owned),
            last_modified: resp.header("last-modified").map(str::to_owned),
            body: resp.body,
        }),
        st => Err(format!("{url} answered HTTP {st}")),
    }
}

/// The browser build has no sockets.
#[cfg(target_arch = "wasm32")]
pub fn fetch(_d: &Due) -> Result<Fetched, String> {
    Err("internet calendars cannot be fetched in the browser version".into())
}
