//! `sendReceive.*` (`plan/reference/ribbon-mail.md`, Send / Receive tab): start send/receive runs on the
//! accounts' worker threads (`crate::sync`), report each account's Outbox, cancel runs, show progress, and
//! toggle Work Offline (kept in the session's view state). Results arrive through `Session::poll_sync`.
//! The Server and Preferences command bodies (header-only download, marks, Download Preferences and the
//! per-account Send/Receive group settings) live in [`crate::sync::cmds`].

use serde_json::{Value, json};

use crate::cmd::calendar::subscribe;
use crate::cmd::rules::{dialog, need_selection};
use crate::cmd::util::{opt_bool, req_str};
use crate::sync::{Plan, cmds};
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session, always};
use mailcraft_model::{AccountId, FolderRole};

fn spec(
    id: &'static str,
    label: &'static str,
    location: &'static str,
    shortcut: &'static str,
    params: &'static str,
    enabled: fn(&Session) -> Option<&'static str>,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec { id, label, location, shortcut, params, mutates: false, enabled, run }
}

pub fn specs() -> Vec<CommandSpec> {
    const SR: &str = "Send / Receive › Send & Receive";
    const DL: &str = "Send / Receive › Download";
    const SV: &str = "Send / Receive › Server";
    const PR: &str = "Send / Receive › Preferences";
    vec![
        spec(
            "sendReceive.all",
            "Send/Receive All Folders",
            SR,
            "F9",
            r#"{"offline"?: bool}"#,
            has_accounts,
            all,
        ),
        spec(
            "sendReceive.folder",
            "Update Folder",
            SR,
            "",
            r#"{"folder"?: number, "offline"?: bool}"#,
            has_accounts,
            folder,
        ),
        spec(
            "sendReceive.group",
            "Send/Receive Groups",
            SR,
            "",
            r#"{"name": string (account address/name or folder name), "offline"?: bool}"#,
            has_accounts,
            group,
        ),
        spec(
            "sendReceive.defineGroups",
            "Define Send/Receive Groups",
            "Send / Receive › Send/Receive Groups",
            "",
            r#"{"action"?: "list"|"save"|"copy"|"rename"|"remove" (none: the dialog), "group"?: {"name": string, "members": [{"account": number, "send"?: bool, "receive"?: bool, "headersOnly"?: bool, "folders"?: [number]|null}], "include"?: bool, "everyMinutes"?: number, "onExit"?: bool, "offlineInclude"?: bool, "offlineEveryMinutes"?: number}, "oldName"?: string, "name"?: string, "newName"?: string}"#,
            always,
            crate::sync::groups::define,
        ),
        spec("sendReceive.sendAll", "Send All", SR, "", r#"{"offline"?: bool}"#, has_accounts, send_all),
        spec("sendReceive.cancelAll", "Cancel All", DL, "", "", always, cancel_all),
        spec("sendReceive.status", "Send/Receive Status", "Status bar", "", "", always, status),
        spec("sendReceive.showProgress", "Show Progress", DL, "", "", always, show_progress),
        spec(
            "sendReceive.downloadHeaders",
            "Download Headers",
            SV,
            "",
            r#"{"folder"?: number}"#,
            server_account,
            cmds::download_headers,
        ),
        spec(
            "sendReceive.markToDownload",
            "Mark to Download",
            SV,
            "Mod+Alt+M",
            r#"{"copy"?: bool, "ids"?: [number], "now"?: bool (download them now)}"#,
            need_selection,
            cmds::mark,
        ),
        spec(
            "sendReceive.unmarkToDownload",
            "Unmark to Download",
            SV,
            "Mod+Alt+U",
            r#"{"all"?: bool, "ids"?: [number]}"#,
            always,
            cmds::unmark,
        ),
        spec(
            "sendReceive.processMarked",
            "Process Marked Headers",
            SV,
            "",
            r#"{"allFolders"?: bool}"#,
            server_account,
            cmds::process_marked,
        ),
        spec(
            "sendReceive.preferences",
            "Download Preferences",
            PR,
            "",
            cmds::PREFERENCES_PARAMS,
            always,
            cmds::preferences,
        ),
        spec(
            "sendReceive.workOffline",
            "Work Offline",
            PR,
            "",
            r#"{"offline"?: bool (current state; toggled; default: the session's state)}"#,
            always,
            work_offline,
        ),
    ]
}

fn has_accounts(s: &Session) -> Option<&'static str> {
    if s.mailbox.accounts.is_empty() { Some("no accounts") } else { None }
}

/// Header-only download needs an account with an incoming server.
fn server_account(s: &Session) -> Option<&'static str> {
    if s.mailbox.accounts.iter().any(|a| a.incoming.is_some()) {
        None
    } else {
        Some("no account with a mail server")
    }
}

/// Per-account Outbox report: messages waiting to be sent (by the run this command starts), and those of
/// accounts that cannot send at all.
fn outbox_report(s: &mut Session, accounts: &[AccountId], offline: bool) -> Value {
    let mut rows = Vec::new();
    let mut queued = 0usize;
    let mut stuck = 0usize;
    for id in accounts {
        let Some(acct) = s.mailbox.account(*id) else { continue };
        let outbox = s.mailbox.role_folder(*id, FolderRole::Outbox);
        let n = outbox.map_or(0, |f| s.mailbox.total_count(f));
        let can_send = acct.can_send();
        let status = if n == 0 {
            "nothing to send"
        } else if !can_send {
            "cannot send"
        } else if offline {
            "offline"
        } else {
            "queued"
        };
        if can_send {
            queued = queued.saturating_add(n);
        } else {
            stuck = stuck.saturating_add(n);
        }
        rows.push(json!({
            "account": id.0,
            "name": acct.display_name,
            "address": acct.address,
            "canSend": can_send,
            "outbox": n,
            "status": status,
        }));
    }
    if stuck > 0 {
        s.notify(Notice::Error(format!(
            "{stuck} message(s) in the Outbox belong to an account without an outgoing mail server and cannot be sent."
        )));
    }
    json!({"sent": 0, "received": 0, "queued": queued, "cannotSend": stuck, "offline": offline, "accounts": rows})
}

fn all_account_ids(s: &Session) -> Vec<AccountId> {
    s.mailbox.accounts.iter().map(|a| a.id).collect()
}

fn offline(s: &Session, p: &Value) -> bool {
    opt_bool(p, "offline").unwrap_or(false) || s.view.work_offline
}

/// Starts runs (errors were already reported as notices and in the status) and adds the outcome to `report`.
fn start(s: &mut Session, accounts: &[AccountId], plan: Plan, offline: bool, mut report: Value) -> CmdResult {
    let servers: Vec<AccountId> =
        crate::sync::server_accounts(s).into_iter().filter(|a| accounts.contains(a)).collect();
    let started = if offline || servers.is_empty() { 0 } else { s.send_receive(&servers, plan).unwrap_or(0) };
    if let Some(o) = report.as_object_mut() {
        o.insert("started".into(), json!(started));
        o.insert("status".into(), serde_json::to_value(s.sync_status()).unwrap_or(Value::Null));
    }
    Ok(report)
}

fn all(s: &mut Session, p: &Value) -> CmdResult {
    let ids = all_account_ids(s);
    let off = offline(s, p);
    let report = outbox_report(s, &ids, off);
    let mut out = start(s, &ids, Plan::all(), off, report)?;
    // User-defined groups included in F9 (online, or set to run while working offline). The `offline`
    // parameter (no network at all) skips them too.
    let mut groups_started = 0usize;
    let mut group_names = Vec::new();
    let groups =
        if opt_bool(p, "offline") == Some(true) { Vec::new() } else { crate::sync::groups::f9_groups(s) };
    for g in groups {
        match crate::sync::groups::run(s, &g, true, s.view.work_offline) {
            Ok(n) => groups_started = groups_started.saturating_add(n),
            Err(e) => log::info!("send/receive group {}: {e}", g.name),
        }
        group_names.push(g.name);
    }
    if let Some(o) = out.as_object_mut() {
        o.insert("groups".into(), json!(group_names));
        if groups_started > 0 {
            let before = o.get("started").and_then(Value::as_u64).unwrap_or(0);
            o.insert("started".into(), json!(before.saturating_add(groups_started as u64)));
        }
    }
    // Send/Receive All Folders also updates the internet calendars.
    let calendars = subscribe::subscriptions(s, true);
    let refreshed = refresh_calendars(s, calendars, off);
    if let Some(o) = out.as_object_mut() {
        o.insert("calendars".into(), refreshed);
    }
    Ok(out)
}

/// Most internet calendars one command refreshes.
const MAX_CALENDARS: usize = 200;

/// Refreshes internet calendars: with a UI (`SyncState::background_calendars`) they are queued for its
/// worker threads, else fetched now (CLI, MCP). Offline nothing is fetched. Returns `{"queued", "refreshed",
/// "errors"}` counts.
fn refresh_calendars(s: &mut Session, targets: Vec<subscribe::Due>, offline: bool) -> Value {
    if offline || targets.is_empty() {
        return json!({ "queued": 0, "refreshed": 0, "errors": 0 });
    }
    let targets: Vec<subscribe::Due> = targets.into_iter().take(MAX_CALENDARS).collect();
    if s.sync.background_calendars {
        let mut queued = 0usize;
        for d in targets {
            if !s.sync.calendar_requests.iter().any(|q| q.folder == d.folder)
                && s.sync.calendar_requests.len() < MAX_CALENDARS
            {
                s.sync.calendar_requests.push(d);
                queued = queued.saturating_add(1);
            }
        }
        return json!({ "queued": queued, "refreshed": 0, "errors": 0 });
    }
    let (mut refreshed, mut errors) = (0usize, 0usize);
    for d in targets {
        let outcome = subscribe::fetch(&d);
        let r = subscribe::apply_background(s, d.folder, outcome);
        if let Some(e) = r.get("error").and_then(Value::as_str) {
            errors = errors.saturating_add(1);
            let name = s.mailbox.folder(d.folder).map(|f| f.name.clone()).unwrap_or_default();
            s.notify(Notice::Error(format!("Couldn't update the calendar \"{name}\": {e}")));
        } else {
            refreshed = refreshed.saturating_add(1);
        }
    }
    json!({ "queued": 0, "refreshed": refreshed, "errors": errors })
}

/// Takes the internet calendar refreshes waiting for the UI's fetcher.
pub fn take_calendar_requests(s: &mut Session) -> Vec<subscribe::Due> {
    std::mem::take(&mut s.sync.calendar_requests)
}

fn send_all(s: &mut Session, p: &Value) -> CmdResult {
    let ids = all_account_ids(s);
    let off = offline(s, p);
    let report = outbox_report(s, &ids, off);
    start(s, &ids, Plan { send: true, receive: false, folders: None, user: true }, off, report)
}

fn folder(s: &mut Session, p: &Value) -> CmdResult {
    let f = s.target_folder(p);
    let Some(folder) = s.mailbox.folder(f) else {
        return Err(CmdError::Params(format!("no such folder {}", f.0)));
    };
    let name = folder.name.clone();
    let account = folder.account;
    let mapped = folder.remote_name.is_some();
    let off = offline(s, p);
    // Update Folder on an internet calendar downloads it again.
    if let Some(d) = subscribe::subscription(s, f) {
        let r = refresh_calendars(s, vec![d], off);
        let count = |k: &str| r.get(k).and_then(Value::as_u64).unwrap_or(0);
        let status = if off {
            "offline"
        } else if count("queued") > 0 {
            "updating"
        } else if count("errors") > 0 {
            "failed"
        } else {
            "updated"
        };
        return Ok(json!({
            "folder": f.0,
            "name": name,
            "started": 0,
            "offline": off,
            "calendar": r,
            "status": status,
        }));
    }
    // Contacts, Tasks and Calendar folders synchronise with the account's contacts, tasks and calendar
    // server (CardDAV / CalDAV), not with the mail server. A published calendar also goes up to its
    // publishing location with its publishing account's run.
    let pim_kind = folder_kind(s, f).and_then(crate::sync::pim::PimKind::of);
    if let Some(kind) = pim_kind {
        let servers = crate::sync::server_accounts(s);
        let synced = crate::sync::pim::is_synced_folder(&s.mailbox, f) && servers.contains(&account);
        let mut publishers: Vec<mailcraft_model::AccountId> = Vec::new();
        if kind == crate::sync::pim::PimKind::Calendar {
            let p = crate::cmd::calendar::props::read(&s.mailbox, f);
            let accounts =
                p.publication.iter().map(|x| x.account).chain(p.withdrawals.iter().map(|w| w.account));
            for a in accounts {
                if servers.contains(&a) && !publishers.contains(&a) && !(synced && a == account) {
                    publishers.push(a);
                }
            }
        }
        let on_server = synced || !publishers.is_empty();
        let mut started = 0usize;
        if !off {
            let plan = Plan { send: false, receive: true, folders: Some(vec![f]), user: true };
            if synced {
                started = started.saturating_add(s.send_receive(&[account], plan.clone()).unwrap_or(0));
            }
            if !publishers.is_empty() {
                started = started.saturating_add(s.send_receive(&publishers, plan).unwrap_or(0));
            }
        }
        let status = if !on_server {
            "local folder"
        } else if off {
            "offline"
        } else {
            "updating"
        };
        if !on_server {
            let msg = match kind {
                crate::sync::pim::PimKind::Calendar => format!(
                    "\"{name}\" is kept on this computer only. To keep it on a server, turn on calendar \
                     synchronisation with a CalDAV server in Account Settings, or publish the calendar online."
                ),
                other => format!(
                    "\"{name}\" is kept on this computer only. To keep {} on a server, set up a contacts and \
                     tasks server (CardDAV / CalDAV) in Account Settings.",
                    other.label()
                ),
            };
            s.notify(Notice::Info(msg));
        }
        let server = if synced {
            "dav"
        } else if on_server {
            "publish"
        } else {
            "none"
        };
        return Ok(json!({
            "folder": f.0,
            "name": name,
            "started": started,
            "offline": off,
            "status": status,
            "server": server,
        }));
    }
    let remote = crate::sync::server_accounts(s).contains(&account);
    let mut started = 0;
    if remote && !off {
        // A folder not on the server yet is found by a full update of its account.
        let folders = mapped.then(|| vec![f]);
        let plan = Plan { send: false, receive: true, folders, user: true };
        started = s.send_receive(&[account], plan).unwrap_or(0);
    }
    Ok(json!({
        "folder": f.0,
        "name": name,
        "started": started,
        "offline": off,
        "status": if !remote { "local folder" } else if off { "offline" } else { "updating" },
    }))
}

fn folder_kind(s: &Session, f: mailcraft_model::FolderId) -> Option<mailcraft_model::FolderKind> {
    s.mailbox.folder(f).map(|x| x.kind)
}

fn group(s: &mut Session, p: &Value) -> CmdResult {
    let name = req_str(p, "name")?.trim().to_owned();
    let off = offline(s, p);
    if name.eq_ignore_ascii_case(crate::sync::groups::ALL_ACCOUNTS) {
        return all(s, p);
    }
    crate::accounts::load_prefs(s);
    if let Some(g) = crate::sync::groups::group(s, &name) {
        let ids: Vec<AccountId> = g.members.iter().map(|m| AccountId(m.account)).collect();
        let report = outbox_report(s, &ids, off);
        let started = if off { 0 } else { crate::sync::groups::run(s, &g, true, false).unwrap_or(0) };
        let mut out = report;
        if let Some(o) = out.as_object_mut() {
            o.insert("group".into(), json!(g.name));
            o.insert("started".into(), json!(started));
            o.insert("status".into(), serde_json::to_value(s.sync_status()).unwrap_or(Value::Null));
        }
        return Ok(out);
    }
    if let Some(a) = s
        .mailbox
        .accounts
        .iter()
        .find(|a| a.address.eq_ignore_ascii_case(&name) || a.display_name.eq_ignore_ascii_case(&name))
    {
        let id = a.id;
        let report = outbox_report(s, &[id], off);
        return start(s, &[id], Plan::all(), off, report);
    }
    // A folder name ("Inbox"): the current account's folder first.
    let current = s.current_account();
    let named = |f: &&mailcraft_model::Folder| f.name.eq_ignore_ascii_case(&name);
    let found = s
        .mailbox
        .folders
        .values()
        .filter(named)
        .find(|f| Some(f.account) == current)
        .or_else(|| s.mailbox.folders.values().find(named))
        .map(|f| f.id);
    if let Some(f) = found {
        return folder(s, &json!({"folder": f.0, "offline": off}));
    }
    Err(CmdError::Params(format!("no account, group or folder named \"{name}\"")))
}

fn cancel_all(s: &mut Session, _: &Value) -> CmdResult {
    let n = s.cancel_sync();
    Ok(json!({"cancelled": n}))
}

fn status(s: &mut Session, _: &Value) -> CmdResult {
    let mut v = serde_json::to_value(s.sync_status()).unwrap_or(Value::Null);
    let tests: Vec<Value> = s.sync.tests.values().map(crate::sync::TestReport::to_json).collect();
    if let Some(o) = v.as_object_mut() {
        o.insert("workOffline".into(), json!(s.view.work_offline));
        o.insert("tests".into(), json!(tests));
    }
    Ok(v)
}

fn show_progress(s: &mut Session, _: &Value) -> CmdResult {
    let tasks: Vec<Value> = s
        .sync_status()
        .accounts
        .iter()
        .map(|a| {
            let st = if a.busy {
                a.progress.clone()
            } else if let Some(e) = &a.error {
                format!("Error: {e}")
            } else if a.last_sync.is_some() {
                "Completed".to_owned()
            } else {
                "Not started".to_owned()
            };
            json!({"name": a.name, "status": st, "busy": a.busy})
        })
        .collect();
    dialog(s, "sendReceiveProgress", json!({"tasks": tasks}))
}

fn work_offline(s: &mut Session, p: &Value) -> CmdResult {
    let offline = !opt_bool(p, "offline").unwrap_or(s.view.work_offline);
    s.view.work_offline = offline;
    if offline {
        s.cancel_sync();
    }
    s.poll_sync();
    s.notify(Notice::Info(if offline { "Working offline.".into() } else { "Working online.".into() }));
    Ok(json!({"offline": offline}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rules::tests::session;
    use mailcraft_model::{Account, AccountKind, MailItem, ServerSettings};

    #[test]
    fn send_all_reports_stuck_outbox() {
        let (mut s, _, _) = session();
        let acct = s.mailbox.accounts[0].id;
        let outbox = s.mailbox.role_folder(acct, FolderRole::Outbox).unwrap();
        s.mailbox.add_mail(MailItem { folder: outbox, subject: "Queued".into(), ..Default::default() });
        let r = s.run("sendReceive.all", &json!({})).unwrap();
        assert_eq!(r["cannotSend"], 1);
        assert_eq!(r["sent"], 0);
        assert_eq!(r["accounts"][0]["status"], "cannot send");
        assert!(matches!(s.notices.last(), Some(Notice::Error(_))));
        // The message stays in the Outbox.
        assert_eq!(s.mailbox.total_count(outbox), 1);
        assert_eq!(s.registry.by_shortcut("F9").map(|c| c.id), Some("sendReceive.all"));
    }

    #[test]
    fn sending_account_queues() {
        let (mut s, _, _) = session();
        let id = s.mailbox.add_account(Account {
            kind: AccountKind::Imap,
            address: "me@example.net".into(),
            display_name: "me@example.net".into(),
            incoming: Some(ServerSettings::default()),
            outgoing: Some(ServerSettings::default()),
            ..Default::default()
        });
        s.mailbox.create_default_folders(id);
        let outbox = s.mailbox.role_folder(id, FolderRole::Outbox).unwrap();
        s.mailbox.add_mail(MailItem { folder: outbox, ..Default::default() });
        let r = s.run("sendReceive.group", &json!({"name": "ME@example.net"})).unwrap();
        assert_eq!(r["queued"], 1);
        assert_eq!(r["accounts"][0]["status"], "queued");
        let r = s.run("sendReceive.sendAll", &json!({"offline": true})).unwrap();
        assert_eq!(r["accounts"][1]["status"], "offline");
        assert!(s.enabled("sendReceive.downloadHeaders").is_none());
        assert!(matches!(s.run("sendReceive.group", &json!({"name": "zzz"})), Err(CmdError::Params(_))));
    }

    #[test]
    fn local_only_profile_disables_server_commands() {
        let (mut s, inbox, _) = session();
        assert!(s.enabled("sendReceive.downloadHeaders").is_some());
        assert!(s.enabled("sendReceive.processMarked").is_some());
        let r = s.run("sendReceive.folder", &json!({})).unwrap();
        assert_eq!(r["folder"], inbox.0);
        assert_eq!(r["status"], "local folder");
    }

    #[test]
    fn offline_toggle_and_preferences() {
        let (mut s, _, _) = session();
        assert_eq!(s.run("sendReceive.workOffline", &json!({})).unwrap()["offline"], true);
        assert!(s.view.work_offline);
        assert_eq!(s.run("sendReceive.workOffline", &json!({})).unwrap()["offline"], false);
        assert_eq!(s.run("sendReceive.workOffline", &json!({"offline": false})).unwrap()["offline"], true);
        assert_eq!(s.run("sendReceive.workOffline", &json!({"offline": true})).unwrap()["offline"], false);
        assert!(!s.view.work_offline);
        assert_eq!(s.run("sendReceive.status", &json!({})).unwrap()["workOffline"], false);
        let r =
            s.run("sendReceive.preferences", &json!({"mode": "headers", "slowHeadersOnly": true})).unwrap();
        assert_eq!(r["mode"], "headers");
        assert!(matches!(s.run("sendReceive.preferences", &json!({"mode": "x"})), Err(CmdError::Params(_))));
        assert_eq!(s.run("sendReceive.showProgress", &json!({})).unwrap()["dialog"], "sendReceiveProgress");
        assert_eq!(s.run("sendReceive.cancelAll", &json!({})).unwrap()["cancelled"], 0);
    }

    #[test]
    fn mark_finds_header_only_items() {
        let (mut s, inbox, _) = session();
        let id = s.mailbox.add_mail(MailItem { folder: inbox, ..Default::default() });
        s.mailbox.mail.values_mut().filter(|m| m.id != id).for_each(|m| m.body.text = Some("x".into()));
        s.selection.items = s.mailbox.items_in(inbox).iter().map(|m| m.id).collect();
        let r = s.run("sendReceive.markToDownload", &json!({"copy": true})).unwrap();
        assert_eq!(r["marked"], json!([id.0]));
        let r = s.run("sendReceive.unmarkToDownload", &json!({"all": true})).unwrap();
        assert_eq!(r["unmarked"], json!([id.0]));
        assert_eq!(s.registry.by_shortcut("Mod+Alt+M").map(|c| c.id), Some("sendReceive.markToDownload"));
    }

    #[test]
    fn download_preferences_are_kept_per_account() {
        let (mut s, _, _) = session();
        let id = s.mailbox.add_account(Account {
            kind: AccountKind::Imap,
            address: "me@example.net".into(),
            display_name: "me@example.net".into(),
            incoming: Some(ServerSettings::default()),
            ..Default::default()
        });
        s.mailbox.create_default_folders(id);
        let r = s
            .run("sendReceive.preferences", &json!({"account": id.0, "mode": "headers", "push": false}))
            .unwrap();
        assert_eq!(r["changed"], true);
        // Asked again without changes, the stored choice comes back (the old stub always said "full").
        let r = s.run("sendReceive.preferences", &json!({"account": id.0})).unwrap();
        assert_eq!(r["mode"], "headers");
        assert_eq!(r["push"], false);
    }
}
