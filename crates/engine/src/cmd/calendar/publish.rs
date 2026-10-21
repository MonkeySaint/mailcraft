//! Share Calendar › publish online (`calendar.publish`): the calendar goes to a WebDAV server as an
//! iCalendar file (an HTTP PUT into the account's publishing collection, RFC 4918), and every send/receive
//! uploads it again when it changed, so people who subscribed to its address (Add Calendar › From Internet,
//! or any calendar app) see a live calendar. The file names its refresh interval (RFC 7986
//! `REFRESH-INTERVAL`, `X-PUBLISHED-TTL`). A sharing invitation (`calendar.share` with `publish: true`) is a
//! message carrying the address in its `webcal://` and `https://` forms.
//!
//! The publication (location, range, detail, last upload) is kept in the calendar's properties
//! ([`super::props`]); stopping it removes the file from the server on the next run.

use serde_json::{Value, json};

use mailcraft_model::calendar::{Publication, Withdrawal};
use mailcraft_model::{AccountId, Appointment, DateTime, FolderId};

use super::pim;
use super::{file_safe, ics_calendar, props, share_detail, share_items, share_window, target_calendar};
use crate::cmd::util::opt_str;
use crate::compose::ComposeState;
use crate::sync::Plan;
use crate::{CmdError, CmdResult, Notice, Session};

/// Refresh interval published calendars ask subscribers to keep (minutes).
pub const REFRESH_MIN: u32 = 60;

/// Most earlier publications waiting to be removed from their servers.
const MAX_WITHDRAWALS: usize = 20;

/// Ranges a publication can keep (a fixed custom range would stop being useful).
const RANGES: [&str; 5] = ["all", "today", "tomorrow", "next7", "next30"];

/// The published iCalendar text of calendar `folder` and its fingerprint (the same text without the time
/// it was made, so an unchanged calendar is not uploaded again).
pub fn publication_ics(s: &Session, folder: FolderId, p: &Publication) -> Result<(String, u64), CmdError> {
    let range = if RANGES.contains(&p.range.as_str()) { p.range.as_str() } else { "all" };
    let window = share_window(s, range, &Value::Null)?;
    let detail = if p.detail.is_empty() { "full" } else { p.detail.as_str() };
    share_detail(detail)?;
    let mut appts = share_items(s, folder, window, detail, p.include_private);
    // A published calendar carries no attached files (anyone with the address reads it).
    for a in &mut appts {
        a.attachments.clear();
    }
    let refs: Vec<&Appointment> = appts.iter().collect();
    let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
    let with_header = |text: String| -> String {
        let clean: String = name.chars().filter(|c| !c.is_control()).collect();
        let escaped = clean.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,");
        let head = format!(
            "NAME:{escaped}\r\nX-WR-CALNAME:{escaped}\r\nREFRESH-INTERVAL;VALUE=DURATION:PT{REFRESH_MIN}M\r\n\
             X-PUBLISHED-TTL:PT{REFRESH_MIN}M\r\n"
        );
        match text.find("VERSION:2.0\r\n") {
            Some(i) => {
                let at = i.saturating_add("VERSION:2.0\r\n".len());
                let mut t = text;
                if t.is_char_boundary(at) {
                    t.insert_str(at, &head);
                }
                t
            }
            None => text,
        }
    };
    let stable = with_header(ics_calendar(Some("PUBLISH"), &refs, DateTime::from_unix(0, 0)));
    let fingerprint = crate::sync::pim::fnv(stable.as_bytes()).max(1);
    Ok((with_header(ics_calendar(Some("PUBLISH"), &refs, s.now())), fingerprint))
}

/// The `webcal://` form of an http(s) address (what calendar apps subscribe to).
pub fn webcal(url: &str) -> String {
    let t = url.trim();
    if let Some(r) = t.strip_prefix("https://") {
        format!("webcals://{r}")
    } else if let Some(r) = t.strip_prefix("http://") {
        format!("webcal://{r}")
    } else {
        t.to_owned()
    }
}

/// A file name for the published calendar: letters, digits, `-`, `_` and `.`.
fn slug(name: &str) -> String {
    let base = file_safe(name);
    let s: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' })
        .collect();
    let s = s.trim_matches(|c| matches!(c, '-' | '_' | '.')).to_owned();
    if s.is_empty() { "calendar".into() } else { s }
}

/// The account that publishes calendar `folder`: `account` when given, else the calendar's own account,
/// else the first with a publishing location.
fn publishing_account(s: &Session, folder: FolderId, p: &Value) -> Result<AccountId, CmdError> {
    let has_location = |id: AccountId| {
        s.mailbox.account(id).and_then(|a| a.dav.as_ref()).is_some_and(|d| !d.publish_url.trim().is_empty())
    };
    if p.get("account").is_some_and(|v| !v.is_null()) {
        return crate::accounts::find_account(s, p, "account");
    }
    if let Some(own) = s.mailbox.folder_account(folder).filter(|a| has_location(*a)) {
        return Ok(own);
    }
    s.mailbox.accounts.iter().map(|a| a.id).find(|a| has_location(*a)).ok_or_else(|| {
        CmdError::Disabled(
            "No publishing location is set up: add the WebDAV address for published calendars in Account \
                 Settings (contacts, tasks and calendar server)."
                .into(),
        )
    })
}

/// Uploads now (or as soon as a send/receive can run): a run of the publishing account for this calendar.
fn start_upload(s: &mut Session, account: AccountId, folder: FolderId) -> usize {
    if s.view.work_offline || !crate::sync::server_accounts(s).contains(&account) {
        return 0;
    }
    let plan = Plan { send: false, receive: true, folders: Some(vec![folder]), user: true };
    s.send_receive(&[account], plan).unwrap_or(0)
}

/// `calendar.publish`: publishes the calendar (or, with `stop: true`, stops publishing it).
pub fn run_publish(s: &mut Session, p: &Value) -> CmdResult {
    let folder = target_calendar(s, p)?;
    let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
    let mut props = props::read(&s.mailbox, folder);
    if pim::flag(p, "stop")? == Some(true) {
        let Some(mut publication) = props.publication.take() else {
            return Ok(json!({ "folder": folder.0, "published": false }));
        };
        let account = publication.account;
        publication.withdraw = true;
        props.publication = Some(publication);
        props::write(&mut s.mailbox, folder, &props);
        let started = start_upload(s, account, folder);
        return Ok(json!({ "folder": folder.0, "published": false, "started": started }));
    }
    let account = publishing_account(s, folder, p)?;
    let range = opt_str(p, "range")
        .map(str::to_owned)
        .or_else(|| props.publication.as_ref().map(|x| x.range.clone()))
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "all".into());
    if !RANGES.contains(&range.as_str()) {
        return Err(CmdError::Params(format!(
            "`range` must be one of {} for a published calendar",
            RANGES.join(", ")
        )));
    }
    let detail = opt_str(p, "detail")
        .map(str::to_owned)
        .or_else(|| props.publication.as_ref().map(|x| x.detail.clone()))
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "full".into());
    share_detail(&detail)?;
    let include_private = pim::flag(p, "includePrivate")?
        .or_else(|| props.publication.as_ref().map(|x| x.include_private))
        .unwrap_or(false);
    let base = s
        .mailbox
        .account(account)
        .and_then(|a| a.dav.as_ref())
        .map(|d| d.publish_url.trim().to_owned())
        .unwrap_or_default();
    // The account's login goes with the upload, so the file can only go to its own publishing server.
    let Some(home) = crate::sync::pim::web_origin(&base) else {
        return Err(CmdError::Disabled(
            "That account has no publishing location: add the WebDAV address for published calendars in \
             Account Settings (contacts, tasks and calendar server)."
                .into(),
        ));
    };
    let url = match opt_str(p, "url").map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => u.to_owned(),
        None => match props.publication.as_ref().filter(|x| x.account == account && !x.withdraw) {
            Some(x) => x.url.clone(),
            None => {
                let base = if base.ends_with('/') { base } else { format!("{base}/") };
                format!("{base}{}-{}.ics", slug(&name), folder.0)
            }
        },
    };
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.contains(char::is_whitespace) {
        return Err(CmdError::Params(format!("`{url}` is not a web address to publish to")));
    }
    if crate::sync::pim::web_origin(&url).as_deref() != Some(home.as_str()) {
        return Err(CmdError::Params(format!(
            "`{url}` is not on the account's publishing server ({home}): a calendar is published there only"
        )));
    }
    // Published before at another address (or by another account): that file is removed by the next run.
    if let Some(old) = props.publication.take()
        && (old.url != url || old.account != account)
        && !props.withdrawals.iter().any(|w| w.url == old.url && w.account == old.account)
    {
        props.withdrawals.push(Withdrawal { url: old.url, account: old.account });
        // Bounded: the calendar's properties have a size limit.
        if props.withdrawals.len() > MAX_WITHDRAWALS {
            props.withdrawals.remove(0);
        }
    }
    props.withdrawals.retain(|w| !(w.url == url && w.account == account));
    props.publication = Some(Publication {
        url: url.clone(),
        account,
        range: range.clone(),
        detail: detail.clone(),
        include_private,
        ..Default::default()
    });
    props::write(&mut s.mailbox, folder, &props);
    let started = start_upload(s, account, folder);
    if started == 0 {
        s.notify(Notice::Info(format!(
            "The calendar \"{name}\" will be published at {url} with the next send/receive."
        )));
    }
    Ok(json!({
        "folder": folder.0,
        "published": true,
        "url": url,
        "webcal": webcal(&url),
        "account": account.0,
        "range": range,
        "detail": detail,
        "started": started,
    }))
}

/// `calendar.share` with `publish: true`: publishes the calendar and opens a sharing invitation carrying its
/// address.
pub(crate) fn share_invitation(s: &mut Session, p: &Value) -> CmdResult {
    let mut out = run_publish(s, p)?;
    let folder = FolderId(out.get("folder").and_then(Value::as_u64).unwrap_or_default());
    let name = s.mailbox.folder(folder).map(|f| f.name.clone()).unwrap_or_default();
    let url = out.get("url").and_then(Value::as_str).unwrap_or_default().to_owned();
    let me = s.mailbox.default_account().map(|a| a.sender_name.clone()).unwrap_or_default();
    let who = if me.trim().is_empty() { "I".to_owned() } else { me.trim().to_owned() };
    let text = format!(
        "{who} shared the calendar \"{name}\" with you.\n\n\
         Subscribe to it in your calendar app to see it and every later change:\n{}\n\n\
         In MailCraft: Calendar › Add Calendar › From Internet, then paste this address:\n{url}\n",
        webcal(&url)
    );
    let mut compose = ComposeState {
        to: opt_str(p, "recipients").unwrap_or_default().to_owned(),
        subject: format!("Shared calendar: {name}"),
        body: mailcraft_model::Doc::from_plain_text(&text),
        dirty: true,
        ..Default::default()
    };
    crate::compose::insert_default_signature(&s.mailbox, &mut compose);
    let id = s.open_compose(compose);
    if let Some(o) = out.as_object_mut() {
        o.insert("compose".into(), json!(id));
        o.insert("calendar".into(), json!(folder.0));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webcal_and_slug_forms() {
        assert_eq!(webcal("https://dav.example.com/p/team.ics"), "webcals://dav.example.com/p/team.ics");
        assert_eq!(webcal("http://dav.example.com/p/team.ics"), "webcal://dav.example.com/p/team.ics");
        assert_eq!(slug("Team: Q4 / plans"), "Team_-Q4-_-plans");
        assert_eq!(slug("////"), "calendar");
    }
}
