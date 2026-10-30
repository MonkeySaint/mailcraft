//! Send / Receive › Server and Preferences (`plan/reference/ribbon-mail.md`): Download Headers, Mark to
//! Download (and Message Copy), Unmark to Download (and Unmark All), Process Marked Headers (and in All
//! Folders), and Download Preferences with the Send/Receive group basics (per-account membership, interval and
//! push).
//!
//! These are the command bodies for the `sendReceive.downloadHeaders`, `sendReceive.markToDownload`,
//! `sendReceive.unmarkToDownload`, `sendReceive.processMarked` and `sendReceive.preferences` specs (same ids,
//! same parameters plus the new optional ones in [`PREFERENCES_PARAMS`]).
//!
//! Header-only items are those downloaded with "Download Headers" or a headers-first download preference:
//! their id is in the account options' `headersOnly` set and they carry no body or source yet. Marks live in
//! the same file, so they survive a restart; Process Marked Headers hands the marked items to the accounts'
//! workers, which download them whole (IMAP `UID FETCH BODY.PEEK[]`, POP3 `RETR`).

use serde_json::{Value, json};

use crate::accounts::{DownloadMode, account_options, apply_option_params, find_account, prefs, save_prefs};
use crate::cmd::util::opt_bool;
use crate::sync::Plan;
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::{AccountId, AccountKind, FolderId, ItemId, MailItem};

/// Parameters of `sendReceive.preferences`.
pub const PREFERENCES_PARAMS: &str = r#"{"account"?: number|string (default: every account, the All Accounts group), "mode"?: "full"|"headersThenFull"|"headers", "slowHeadersOnly"?: bool|"toggle" (on slow connections download only headers), "downloadMonths"?: number|null (first download: mail from the past N months; null = all), "include"?: bool (member of All Accounts: F9 and scheduled runs), "intervalMinutes"?: number|null (scheduled send/receive; null = Options › Advanced), "push"?: bool (new mail pushed with IMAP IDLE), "pollMinutes"?: number}"#;

/// No stored body and no stored source: only the header is on this computer (downloaded as headers only, or
/// the body was taken off by the account's offline window).
fn bodyless(m: &MailItem) -> bool {
    m.body_key.is_none() && m.blob.is_none()
}

/// True when the item was downloaded as headers only and not yet in full, or its body was taken off this
/// computer by the account's offline window.
pub fn is_header_only(s: &Session, id: ItemId) -> bool {
    s.mailbox
        .mail
        .get(id)
        .is_some_and(|m| (s.sync.prefs.data.headers_only.contains(&id.0) && m.blob.is_none()) || bodyless(m))
}

/// True when the rest of the item is on its server only: header-only ([`is_header_only`]) and downloaded as
/// headers only, taken off by the offline window, or a server copy (a server id in a synchronised folder). A
/// local item that simply has no content is not.
pub fn rest_on_server(s: &Session, id: ItemId) -> bool {
    is_header_only(s, id)
        && s.mailbox.mail.get(id).is_some_and(|m| {
            m.body_evicted
                || s.sync.prefs.data.headers_only.contains(&id.0)
                || crate::sync::queue_account(&s.mailbox, m).is_some()
        })
}

/// The error for work that needs the whole of messages only whose header is on this computer (`missing`,
/// see [`rest_on_server`]): `doing` says what was refused ("moved out of their account", "copied"…). Their
/// download starts now unless MailCraft works offline, so the same command works once they have arrived.
pub(crate) fn not_downloaded(s: &mut Session, missing: &[ItemId], doing: &str) -> CmdError {
    let started = !s.view.work_offline && download_now(s, missing).is_ok_and(|n| n > 0);
    let (what, it) = match missing.len() {
        1 => ("1 message has only its header".to_owned(), "it"),
        n => (format!("{n} messages have only their headers"), "they"),
    };
    let next = if started {
        "Downloading now: try again when the download has finished."
    } else {
        "Download the rest first (Download the rest of this message, or Mark to Download and Process Marked \
         Headers) while the account is connected."
    };
    CmdError::Failed(format!(
        "{what} on this computer (the rest is on the server), so {it} cannot be {doing} yet. {next}"
    ))
}

/// Refuses work on `ids` that needs their whole content when the rest of any of them is on its server only
/// (see [`rest_on_server`] and [`not_downloaded`]).
pub(crate) fn need_content(s: &mut Session, ids: &[ItemId], doing: &str) -> Result<(), CmdError> {
    let missing: Vec<ItemId> = ids.iter().copied().filter(|i| rest_on_server(s, *i)).collect();
    if missing.is_empty() { Ok(()) } else { Err(not_downloaded(s, &missing, doing)) }
}

/// `Some(copy)` when the item is marked to download (`copy`: Mark to Download Message Copy).
pub fn marked(s: &Session, id: ItemId) -> Option<bool> {
    s.sync.prefs.data.marked.get(&id.0).copied()
}

/// The download preference of the current account (Download Preferences menu state).
pub fn download_mode(s: &Session) -> DownloadMode {
    match s.current_account() {
        Some(a) => account_options(s, a).download,
        None => s.sync.prefs.data.defaults.download,
    }
}

/// "On Slow Connections Download Only Headers" for the current account (the menu's check box state; the
/// value `sendReceive.preferences {"slowHeadersOnly": "toggle"}` flips).
pub fn slow_headers_only(s: &Session) -> bool {
    match s.current_account() {
        Some(a) => account_options(s, a).slow_headers_only,
        None => s.sync.prefs.data.defaults.slow_headers_only,
    }
}

fn server_account(s: &Session, id: AccountId) -> bool {
    s.mailbox.account(id).is_some_and(|a| {
        matches!(a.kind, AccountKind::Imap | AccountKind::Jmap | AccountKind::Graph | AccountKind::Exchange)
            && a.incoming.is_some()
    })
}

/// Download Headers: the folder's account downloads the headers of new messages (bodies later, on demand).
pub fn download_headers(s: &mut Session, p: &Value) -> CmdResult {
    crate::accounts::load_prefs(s);
    let f = s.target_folder(p);
    let Some(folder) = s.mailbox.folder(f) else {
        return Err(CmdError::Params(format!("no such folder {}", f.0)));
    };
    let account = folder.account;
    let mapped = folder.remote_name.is_some();
    if !server_account(s, account) {
        return Err(CmdError::Disabled("this folder's account has no mail server".into()));
    }
    s.sync.force_headers.insert(account);
    let folders = (mapped && !crate::accounts::is_pop3(s, account)).then(|| vec![f]);
    let plan = Plan { send: false, receive: true, folders, user: true };
    let started = match s.send_receive(&[account], plan) {
        Ok(n) => n,
        Err(e) => {
            s.sync.force_headers.remove(&account);
            return Err(e);
        }
    };
    Ok(json!({"folder": f.0, "account": account.0, "started": started, "downloaded": 0}))
}

/// Downloads header-only items in full now (their accounts' workers fetch them at once, as Process Marked
/// Headers does for marked ones). Returns how many runs started; items that are not header-only, or whose
/// account has no mail server, are left alone.
pub fn download_now(s: &mut Session, ids: &[ItemId]) -> Result<usize, CmdError> {
    crate::accounts::load_prefs(s);
    let mut by_account: std::collections::BTreeMap<AccountId, Vec<ItemId>> = Default::default();
    for id in ids {
        if !is_header_only(s, *id) {
            continue;
        }
        let Some(acct) = s.mailbox.mail.get(*id).and_then(|m| s.mailbox.folder(m.folder)).map(|f| f.account)
        else {
            continue;
        };
        if server_account(s, acct) {
            by_account.entry(acct).or_default().push(*id);
        }
    }
    let mut started = 0usize;
    for (acct, ids) in by_account {
        let list = s.sync.fetch.entry(acct).or_default();
        for i in ids {
            if !list.contains(&i) {
                list.push(i);
            }
        }
        let plan = Plan { send: false, receive: false, folders: Some(Vec::<FolderId>::new()), user: true };
        started = started.saturating_add(s.send_receive(&[acct], plan)?);
    }
    Ok(started)
}

/// Mark to Download (`copy`: Mark to Download Message Copy) on the selected header-only items. `now`: download
/// them at once instead (the reading pane's "Download the rest of this message").
pub fn mark(s: &mut Session, p: &Value) -> CmdResult {
    crate::accounts::load_prefs(s);
    let copy = opt_bool(p, "copy").unwrap_or(false);
    let ids: Vec<ItemId> = s.target_ids(p).into_iter().filter(|i| is_header_only(s, *i)).collect();
    if opt_bool(p, "now") == Some(true) {
        let started = download_now(s, &ids)?;
        return Ok(json!({"downloading": ids.iter().map(|i| i.0).collect::<Vec<_>>(), "started": started}));
    }
    let pr = prefs(s);
    for i in &ids {
        pr.marked.insert(i.0, copy);
    }
    if !ids.is_empty() {
        save_prefs(s);
    }
    Ok(json!({"marked": ids.iter().map(|i| i.0).collect::<Vec<_>>(), "copy": copy}))
}

/// Unmark to Download (`all`: Unmark All to Download).
pub fn unmark(s: &mut Session, p: &Value) -> CmdResult {
    crate::accounts::load_prefs(s);
    let all = opt_bool(p, "all").unwrap_or(false);
    let ids: Vec<u64> = if all {
        let mut v: Vec<u64> = s.sync.prefs.data.marked.keys().copied().collect();
        // Header-only items that were never marked are reported too (nothing to undo for them).
        v.extend(s.mailbox.mail.items().filter(|m| bodyless(m)).map(|m| m.id.0));
        v.sort_unstable();
        v.dedup();
        v
    } else {
        s.target_ids(p)
            .iter()
            .filter(|i| is_header_only(s, **i) || marked(s, **i).is_some())
            .map(|i| i.0)
            .collect()
    };
    let pr = prefs(s);
    let mut changed = false;
    for i in &ids {
        changed |= pr.marked.remove(i).is_some();
    }
    if changed {
        save_prefs(s);
    }
    Ok(json!({"unmarked": ids, "all": all}))
}

/// Process Marked Headers (`allFolders`: in All Folders): marked items are downloaded in full by their
/// accounts' next run, started now.
pub fn process_marked(s: &mut Session, p: &Value) -> CmdResult {
    crate::accounts::load_prefs(s);
    let all_folders = opt_bool(p, "allFolders").unwrap_or(false);
    let f = s.target_folder(p);
    let marked_ids: Vec<ItemId> = s.sync.prefs.data.marked.keys().map(|i| ItemId(*i)).collect();
    let mut by_account: std::collections::BTreeMap<AccountId, Vec<ItemId>> = Default::default();
    let mut stale = Vec::new();
    for id in marked_ids {
        let Some(m) = s.mailbox.mail.get(id) else {
            stale.push(id.0);
            continue;
        };
        if !all_folders && m.folder != f {
            continue;
        }
        if !is_header_only(s, id) {
            stale.push(id.0);
            continue;
        }
        let Some(acct) = s.mailbox.folder(m.folder).map(|x| x.account) else { continue };
        by_account.entry(acct).or_default().push(id);
    }
    if !stale.is_empty() {
        let pr = prefs(s);
        for i in stale {
            pr.marked.remove(&i);
        }
        save_prefs(s);
    }
    let requested: usize = by_account.values().map(Vec::len).sum();
    let mut started = 0usize;
    for (acct, ids) in by_account {
        if !server_account(s, acct) {
            continue;
        }
        s.sync.fetch.entry(acct).or_default().extend(ids);
        let plan = Plan { send: false, receive: false, folders: Some(Vec::<FolderId>::new()), user: true };
        started = started.saturating_add(s.send_receive(&[acct], plan)?);
    }
    Ok(
        json!({"allFolders": all_folders, "folder": f.0, "requested": requested, "started": started, "downloaded": 0}),
    )
}

/// What the Download Preferences menu shows: the selected account's download mode name ("full",
/// "headersThenFull", "headers") and its "On Slow Connections Download Only Headers" option (the
/// defaults when no account is selected), as `sendReceive.preferences` without an account reports them.
pub fn shown_preferences(s: &Session) -> (&'static str, bool) {
    let o = s
        .current_account()
        .map(|a| crate::accounts::account_options(s, a))
        .unwrap_or_else(|| s.sync.prefs.data.defaults.clone());
    (o.download.name(), o.slow_headers_only)
}

/// Download Preferences and the Send/Receive group basics, for one account or (no `account`) for all of them
/// and new ones. Without changes it reports the current state.
pub fn preferences(s: &mut Session, p: &Value) -> CmdResult {
    crate::accounts::load_prefs(s);
    let targets: Vec<AccountId> = if p.get("account").is_some_and(|v| !v.is_null()) {
        vec![find_account(s, p, "account")?]
    } else {
        s.mailbox.accounts.iter().filter(|a| a.kind != AccountKind::Local).map(|a| a.id).collect()
    };
    // The menu's On Slow Connections Download Only Headers flips what is shown (one value for every target).
    let mut owned;
    let p = if p.get("slowHeadersOnly").and_then(Value::as_str) == Some("toggle") {
        let current = match targets.first() {
            Some(id) if targets.len() == 1 => account_options(s, *id).slow_headers_only,
            _ => s.current_account().map_or(s.sync.prefs.data.defaults.slow_headers_only, |a| {
                account_options(s, a).slow_headers_only
            }),
        };
        owned = p.clone();
        if let Some(o) = owned.as_object_mut() {
            o.insert("slowHeadersOnly".into(), json!(!current));
        }
        &owned
    } else {
        p
    };
    // Validate once against the defaults, then apply everywhere (each account compared with its own options).
    let mut defaults = s.sync.prefs.data.defaults.clone();
    let defaults_changed = apply_option_params(&mut defaults, p)?;
    let mut changed = false;
    if defaults_changed && p.get("account").is_none_or(Value::is_null) {
        prefs(s).defaults = defaults.clone();
        changed = true;
    }
    for id in &targets {
        let mut o = account_options(s, *id);
        if apply_option_params(&mut o, p)? {
            crate::accounts::update_options(s, *id, |x| *x = o);
            crate::sync::options_changed(s, *id);
            changed = true;
        }
    }
    if changed {
        save_prefs(s);
    }
    let shown = match targets.first() {
        Some(id) if targets.len() == 1 => account_options(s, *id),
        _ => s.current_account().map(|a| account_options(s, a)).unwrap_or(defaults),
    };
    Ok(json!({
        "mode": shown.download.name(),
        "slowHeadersOnly": shown.slow_headers_only,
        "downloadMonths": shown.download_months,
        "include": shown.include_in_send_receive,
        "intervalMinutes": shown.interval_minutes,
        "push": shown.push,
        "pollMinutes": shown.poll_minutes,
        "accounts": targets.iter().map(|a| a.0).collect::<Vec<_>>(),
        "changed": changed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rules::tests::session;
    use mailcraft_model::{Account, ServerSettings};

    fn imap(s: &mut Session) -> AccountId {
        let id = s.mailbox.add_account(Account {
            kind: AccountKind::Imap,
            address: "me@example.net".into(),
            display_name: "me@example.net".into(),
            incoming: Some(ServerSettings::default()),
            ..Default::default()
        });
        s.mailbox.create_default_folders(id);
        id
    }

    #[test]
    fn mark_unmark_and_preferences() {
        let (mut s, inbox, _) = session();
        let a = imap(&mut s);
        let body = s
            .add_message(
                MailItem { folder: inbox, ..Default::default() },
                &mailcraft_store::Content::source(b"x".to_vec()),
            )
            .unwrap();
        let head = s.mailbox.add_mail(MailItem { folder: inbox, ..Default::default() });
        s.sync.prefs.data.headers_only.insert(head.0);
        let r = mark(&mut s, &json!({"ids": [body.0, head.0], "copy": true})).unwrap();
        assert_eq!(r["marked"], json!([head.0]));
        assert_eq!(marked(&s, head), Some(true));
        let r = unmark(&mut s, &json!({"ids": [head.0]})).unwrap();
        assert_eq!(r["unmarked"], json!([head.0]));
        assert_eq!(marked(&s, head), None);
        mark(&mut s, &json!({"ids": [head.0]})).unwrap();
        assert_eq!(marked(&s, head), Some(false));
        assert_eq!(unmark(&mut s, &json!({"all": true})).unwrap()["all"], true);
        assert!(s.sync.prefs.data.marked.is_empty());

        // Preferences: all accounts, then one.
        let r = preferences(&mut s, &json!({"mode": "headers", "slowHeadersOnly": false})).unwrap();
        assert_eq!(r["mode"], "headers");
        assert_eq!(account_options(&s, a).download, DownloadMode::Headers);
        assert_eq!(s.sync.prefs.data.defaults.download, DownloadMode::Headers);
        let r =
            preferences(&mut s, &json!({"account": a.0, "include": false, "intervalMinutes": 10})).unwrap();
        assert_eq!(r["include"], false);
        assert_eq!(account_options(&s, a).interval_minutes, Some(10));
        assert!(crate::sync::group_accounts(&s).is_empty());
        assert!(matches!(preferences(&mut s, &json!({"mode": "x"})), Err(CmdError::Params(_))));
        assert!(matches!(preferences(&mut s, &json!({"intervalMinutes": 99_999})), Err(CmdError::Params(_))));
        // Unchanged state is reported.
        assert_eq!(preferences(&mut s, &json!({"account": a.0})).unwrap()["changed"], false);
        // On Slow Connections Download Only Headers toggles; the first download window is kept per account.
        let r = preferences(&mut s, &json!({"account": a.0, "slowHeadersOnly": "toggle"})).unwrap();
        assert_eq!(r["slowHeadersOnly"], true);
        assert!(account_options(&s, a).slow_headers_only);
        let r = preferences(&mut s, &json!({"account": a.0, "slowHeadersOnly": "toggle"})).unwrap();
        assert_eq!(r["slowHeadersOnly"], false);
        assert!(matches!(
            preferences(&mut s, &json!({"slowHeadersOnly": "maybe"})),
            Err(CmdError::Params(_))
        ));
        let r = preferences(&mut s, &json!({"account": a.0, "downloadMonths": 3})).unwrap();
        assert_eq!(r["downloadMonths"], 3);
        assert_eq!(account_options(&s, a).download_months, Some(3));
        preferences(&mut s, &json!({"account": a.0, "downloadMonths": null})).unwrap();
        assert_eq!(account_options(&s, a).download_months, None, "null = all mail");
        assert!(matches!(preferences(&mut s, &json!({"downloadMonths": 5000})), Err(CmdError::Params(_))));
        assert_eq!(download_mode(&s), DownloadMode::Headers);
    }

    #[test]
    fn process_marked_needs_a_server_and_drops_stale_marks() {
        let (mut s, inbox, _) = session();
        let gone = ItemId(9_999);
        s.sync.prefs.data.marked.insert(gone.0, false);
        let full = s
            .add_message(
                MailItem { folder: inbox, ..Default::default() },
                &mailcraft_store::Content::source(b"x".to_vec()),
            )
            .unwrap();
        s.sync.prefs.data.marked.insert(full.0, false);
        let r = process_marked(&mut s, &json!({"allFolders": true})).unwrap();
        assert_eq!(r["requested"], 0);
        assert!(s.sync.prefs.data.marked.is_empty(), "marks of missing or complete items are dropped");
        assert!(matches!(download_headers(&mut s, &json!({})), Err(CmdError::Disabled(_))));
    }
}
