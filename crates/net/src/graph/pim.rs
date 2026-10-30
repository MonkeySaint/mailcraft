//! Contacts, calendars and Microsoft To Do tasks over Microsoft Graph (v1.0): the delegated permissions they
//! need, delta rounds (a delta query read to its `@odata.deltaLink`), the event response actions (`accept`,
//! `tentativelyAccept`, `decline`) and the mailbox's time zone. The resources themselves (`/me/contactFolders`,
//! `/me/calendars`, `/users/{id}/calendar`, `/me/todo/lists`) are plain JSON read and written through
//! [`GraphClient`]; the engine maps them to its contacts, appointments and tasks.

use serde_json::{Value, json};

use super::{GraphClient, seg};
use crate::error::{NetError, Result};

/// Delegated permissions for contacts, calendars (the user's own and those shared with them), To Do tasks
/// and the mailbox settings (time zone), added to [`super::MAIL_SCOPES`] at sign-in ([`super::scopes`]).
pub const PIM_SCOPES: [&str; 5] = [
    "Contacts.ReadWrite",
    "Calendars.ReadWrite",
    "Calendars.ReadWrite.Shared",
    "Tasks.ReadWrite",
    "MailboxSettings.Read",
];

/// Most pages read in one delta round.
const MAX_ROUND_PAGES: usize = 10_000;

/// A delta query read to its end: what changed (whole objects), the ids removed, and the link for the
/// changes after this round.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeltaRound {
    pub changed: Vec<Value>,
    /// Ids reported with `@removed` (deleted, or no longer in the queried range).
    pub removed: Vec<String>,
    pub delta_link: String,
}

/// Reads a delta query from `start` (a path to begin, or a saved delta link) through every `@odata.nextLink`
/// to its `@odata.deltaLink`. More than `max` entries is an error, never a part of the changes passed off as
/// all of them (the caller starts over another way). A cancelled client gets an error.
pub fn delta_round(c: &GraphClient, start: &str, max: usize) -> Result<DeltaRound> {
    delta_pages(c, start, max, true)
}

/// Reads a delta query from `start` to its end like [`delta_round`] but keeps nothing of what it lists, only
/// the delta link (a first round over a whole collection the caller lists another way: its pages are dropped
/// as they come, so memory stays small however much the round lists).
pub fn delta_link(c: &GraphClient, start: &str, max: usize) -> Result<String> {
    delta_pages(c, start, max, false).map(|r| r.delta_link)
}

fn delta_pages(c: &GraphClient, start: &str, max: usize, keep: bool) -> Result<DeltaRound> {
    let mut out = DeltaRound::default();
    let mut seen = 0usize;
    let mut next = start.to_owned();
    for _ in 0..MAX_ROUND_PAGES {
        if c.cancelled() {
            return Err(super::cancelled_error());
        }
        let page = c.get_page(&next)?;
        for v in page.value {
            if seen >= max {
                return Err(NetError::Protocol(format!("Microsoft Graph listed more than {max} changes")));
            }
            seen = seen.saturating_add(1);
            if !keep {
                continue;
            }
            let id = v.get("id").and_then(Value::as_str).map(str::to_owned);
            match (v.get("@removed").is_some(), id) {
                (true, Some(id)) => out.removed.push(id),
                (false, Some(_)) => out.changed.push(v),
                (_, None) => {}
            }
        }
        if let Some(link) = page.delta_link {
            // The link must stay on the API's origin like every other one.
            c.url(&link)?;
            out.delta_link = link;
            return Ok(out);
        }
        match page.next_link {
            Some(n) => next = n,
            None => {
                return Err(NetError::Protocol(
                    "Microsoft Graph ended a delta query without a delta link".into(),
                ));
            }
        }
    }
    Err(NetError::Protocol("Microsoft Graph kept paging a delta query".into()))
}

/// The service no longer knows a saved delta state (410 Gone, or a sync-state error code): the query starts
/// over.
pub fn lost_delta(e: &NetError) -> bool {
    match e {
        NetError::Graph { status, code, .. } => {
            *status == 410
                || ["SyncStateNotFound", "SyncStateInvalid", "resyncRequired", "syncStateNotFound"]
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(code))
        }
        NetError::Http { status, .. } => *status == 410,
        _ => false,
    }
}

/// True for an answer saying the signed-in user may not do this (403, `ErrorAccessDenied`): the account
/// lacks the permission (signed in before it was asked for) or the owner did not share the calendar.
pub fn denied(e: &NetError) -> bool {
    matches!(e, NetError::Graph { status: 403, .. } | NetError::Http { status: 403, .. })
}

/// True when the object is gone (404).
pub fn not_found(e: &NetError) -> bool {
    matches!(e, NetError::Graph { status: 404, .. } | NetError::Http { status: 404, .. })
}

/// An answer to a meeting invitation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Response {
    Accept,
    Tentative,
    Decline,
}

impl Response {
    /// The event action's name.
    pub fn action(self) -> &'static str {
        match self {
            Response::Accept => "accept",
            Response::Tentative => "tentativelyAccept",
            Response::Decline => "decline",
        }
    }

    pub fn parse(s: &str) -> Option<Response> {
        match s.trim() {
            "accept" | "accepted" => Some(Response::Accept),
            "tentative" | "tentativelyAccept" => Some(Response::Tentative),
            "decline" | "declined" => Some(Response::Decline),
            _ => None,
        }
    }
}

/// Longest comment sent with a response.
const MAX_COMMENT: usize = 32 * 1024;

/// Answers the invitation of event `event_path` (`/me/events/{id}`): the service records the answer and,
/// with `send`, tells the organizer (no iTIP mail goes from here).
pub fn respond(c: &GraphClient, event_path: &str, r: Response, comment: &str, send: bool) -> Result<()> {
    let comment: String = comment.chars().take(MAX_COMMENT).collect();
    c.post_json(
        &format!("{}/{}", event_path.trim_end_matches('/'), r.action()),
        &json!({"comment": comment, "sendResponse": send}),
    )
    .map(|_| ())
}

/// Cancels meeting `event_path` (`/me/events/{id}`, organized by the user) with `comment` for the attendees:
/// the service removes it and sends the cancellations.
pub fn cancel(c: &GraphClient, event_path: &str, comment: &str) -> Result<()> {
    let comment: String = comment.chars().take(MAX_COMMENT).collect();
    c.post_json(&format!("{}/cancel", event_path.trim_end_matches('/')), &json!({"Comment": comment}))
        .map(|_| ())
}

/// True for a failure that may pass (the network, a timeout, the service busy or throttling, 5xx): the
/// request is worth repeating on a later run.
pub fn transient(e: &NetError) -> bool {
    match e {
        NetError::Graph { status, .. } | NetError::Http { status, .. } => *status == 429 || *status >= 500,
        NetError::Connect(_)
        | NetError::Timeout(_)
        | NetError::Closed
        | NetError::Io(_)
        | NetError::Tls(_) => true,
        _ => false,
    }
}

/// The events of `calendar_path` (`/me/calendar`, `/me/calendars/{id}`, `/users/{id}/calendar`) whose
/// iCalendar UID is `uid` (an invitation's event on the user's calendar).
pub fn events_with_uid(c: &GraphClient, calendar_path: &str, uid: &str, max: usize) -> Result<Vec<Value>> {
    let quoted = uid.replace('\'', "''");
    let filter = crate::http::percent_encode(&format!("iCalUId eq '{quoted}'"));
    c.get_all(&format!("{}/events?$filter={filter}", calendar_path.trim_end_matches('/')), max)
}

/// The mailbox's time zone (`/me/mailboxSettings/timeZone`, a Windows or IANA name); `None` when not set.
pub fn mailbox_time_zone(c: &GraphClient) -> Result<Option<String>> {
    let v = c.get_json("/me/mailboxSettings/timeZone")?;
    Ok(v.get("value")
        .and_then(Value::as_str)
        .map(|s| s.chars().take(100).collect::<String>())
        .filter(|s| !s.trim().is_empty()))
}

/// Path of another user's default calendar (`/users/{address}/calendar`).
pub fn user_calendar_path(user: &str) -> String {
    format!("/users/{}/calendar", seg(user.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_and_errors() {
        assert_eq!(Response::parse("tentative").map(Response::action), Some("tentativelyAccept"));
        assert_eq!(Response::parse("declined"), Some(Response::Decline));
        assert_eq!(Response::parse("maybe"), None);
        assert!(lost_delta(&NetError::Http { status: 410, body: String::new() }));
        assert!(lost_delta(&NetError::Graph {
            status: 400,
            code: "SyncStateInvalid".into(),
            message: String::new()
        }));
        assert!(!lost_delta(&NetError::Graph {
            status: 400,
            code: "BadRequest".into(),
            message: String::new()
        }));
        assert!(denied(&NetError::Graph {
            status: 403,
            code: "ErrorAccessDenied".into(),
            message: String::new()
        }));
        assert!(not_found(&NetError::Http { status: 404, body: String::new() }));
        assert!(transient(&NetError::Graph {
            status: 503,
            code: "ServiceUnavailable".into(),
            message: String::new()
        }));
        assert!(transient(&NetError::Http { status: 429, body: String::new() }));
        assert!(transient(&NetError::Timeout("read".into())));
        assert!(!transient(&NetError::Graph {
            status: 400,
            code: "BadRequest".into(),
            message: String::new()
        }));
        assert!(!transient(&NetError::Http { status: 404, body: String::new() }));
        assert_eq!(user_calendar_path("pat@example.com"), "/users/pat%40example.com/calendar");
        let all = super::super::scopes(&PIM_SCOPES);
        assert!(all.iter().any(|s| s == "https://graph.microsoft.com/Calendars.ReadWrite.Shared"), "{all:?}");
        assert!(all.iter().any(|s| s == "offline_access"));
    }
}
