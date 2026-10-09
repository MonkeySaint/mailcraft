//! `sendReceive.*`. No network yet (commit 8): send/receive reports what is queued in each account's Outbox
//! and which accounts cannot send at all; nothing is transmitted. Offline mode and download preferences have no
//! session field yet, so those commands return their new state for the caller to keep.

use serde_json::{Value, json};

use crate::cmd::rules::{dialog, need_selection};
use crate::cmd::util::{opt_bool, opt_str, req_str};
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session, always};
use mailcraft_model::{AccountId, FolderRole, ItemId, MailItem};

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
        spec("sendReceive.sendAll", "Send All", SR, "", r#"{"offline"?: bool}"#, has_accounts, send_all),
        spec("sendReceive.cancelAll", "Cancel All", DL, "", "", always, cancel_all),
        spec("sendReceive.showProgress", "Show Progress", DL, "", "", always, show_progress),
        spec(
            "sendReceive.downloadHeaders",
            "Download Headers",
            SV,
            "",
            r#"{"folder"?: number}"#,
            server_account,
            download_headers,
        ),
        spec(
            "sendReceive.markToDownload",
            "Mark to Download",
            SV,
            "Mod+Alt+M",
            r#"{"copy"?: bool, "ids"?: [number]}"#,
            need_selection,
            mark,
        ),
        spec(
            "sendReceive.unmarkToDownload",
            "Unmark to Download",
            SV,
            "Mod+Alt+U",
            r#"{"all"?: bool, "ids"?: [number]}"#,
            always,
            unmark,
        ),
        spec(
            "sendReceive.processMarked",
            "Process Marked Headers",
            SV,
            "",
            r#"{"allFolders"?: bool}"#,
            server_account,
            process_marked,
        ),
        spec(
            "sendReceive.preferences",
            "Download Preferences",
            PR,
            "",
            r#"{"mode"?: "full"|"headersThenFull"|"headers", "slowHeadersOnly"?: bool}"#,
            always,
            preferences,
        ),
        spec(
            "sendReceive.workOffline",
            "Work Offline",
            PR,
            "",
            r#"{"offline"?: bool (current state; toggled)}"#,
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

/// Per-account Outbox report. Nothing is sent before the network layer exists: messages stay queued, and those
/// of accounts that cannot send at all are reported as such.
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

fn all(s: &mut Session, p: &Value) -> CmdResult {
    let ids = all_account_ids(s);
    Ok(outbox_report(s, &ids, opt_bool(p, "offline").unwrap_or(false)))
}

fn send_all(s: &mut Session, p: &Value) -> CmdResult {
    let ids = all_account_ids(s);
    Ok(outbox_report(s, &ids, opt_bool(p, "offline").unwrap_or(false)))
}

fn folder(s: &mut Session, p: &Value) -> CmdResult {
    let f = s.target_folder(p);
    let Some(folder) = s.mailbox.folder(f) else {
        return Err(CmdError::Params(format!("no such folder {}", f.0)));
    };
    let name = folder.name.clone();
    let account = folder.account;
    let remote = s.mailbox.account(account).is_some_and(|a| a.incoming.is_some());
    Ok(json!({
        "folder": f.0,
        "name": name,
        "received": 0,
        "offline": opt_bool(p, "offline").unwrap_or(false),
        "status": if remote { "up to date" } else { "local folder" },
    }))
}

fn group(s: &mut Session, p: &Value) -> CmdResult {
    let name = req_str(p, "name")?.trim().to_owned();
    let offline = opt_bool(p, "offline").unwrap_or(false);
    if name.eq_ignore_ascii_case("All Accounts") {
        return all(s, p);
    }
    if let Some(a) = s
        .mailbox
        .accounts
        .iter()
        .find(|a| a.address.eq_ignore_ascii_case(&name) || a.display_name.eq_ignore_ascii_case(&name))
    {
        let id = a.id;
        return Ok(outbox_report(s, &[id], offline));
    }
    if let Some(f) = s.mailbox.folders.values().find(|f| f.name.eq_ignore_ascii_case(&name)).map(|f| f.id) {
        return folder(s, &json!({"folder": f.0, "offline": offline}));
    }
    Err(CmdError::Params(format!("no account, group or folder named \"{name}\"")))
}

fn cancel_all(_: &mut Session, _: &Value) -> CmdResult {
    Ok(json!({"cancelled": 0}))
}

fn show_progress(s: &mut Session, _: &Value) -> CmdResult {
    dialog(s, "sendReceiveProgress", json!({"tasks": []}))
}

fn download_headers(s: &mut Session, p: &Value) -> CmdResult {
    let f = s.target_folder(p);
    Ok(json!({"folder": f.0, "downloaded": 0}))
}

/// Header-only items: no body, no raw source and no stored blob.
fn header_only(m: &MailItem) -> bool {
    m.body.text.is_none() && m.body.html.is_none() && m.raw.is_none() && m.blob.is_none()
}

fn mark(s: &mut Session, p: &Value) -> CmdResult {
    let ids: Vec<ItemId> = s.target_ids(p);
    let marked: Vec<u64> =
        ids.iter().filter(|id| s.mailbox.mail.get(id).is_some_and(header_only)).map(|id| id.0).collect();
    Ok(json!({"marked": marked, "copy": opt_bool(p, "copy").unwrap_or(false)}))
}

fn unmark(s: &mut Session, p: &Value) -> CmdResult {
    let all = opt_bool(p, "all").unwrap_or(false);
    let ids: Vec<u64> = if all {
        s.mailbox.mail.values().filter(|m| header_only(m)).map(|m| m.id.0).collect()
    } else {
        s.target_ids(p)
            .iter()
            .filter(|id| s.mailbox.mail.get(id).is_some_and(header_only))
            .map(|id| id.0)
            .collect()
    };
    Ok(json!({"unmarked": ids, "all": all}))
}

fn process_marked(s: &mut Session, p: &Value) -> CmdResult {
    let all_folders = opt_bool(p, "allFolders").unwrap_or(false);
    let f = s.target_folder(p);
    Ok(json!({"allFolders": all_folders, "folder": f.0, "downloaded": 0}))
}

fn preferences(_: &mut Session, p: &Value) -> CmdResult {
    let mode = opt_str(p, "mode").unwrap_or("full");
    if !["full", "headersThenFull", "headers"].contains(&mode) {
        return Err(CmdError::Params(format!("unknown mode `{mode}`")));
    }
    Ok(json!({"mode": mode, "slowHeadersOnly": opt_bool(p, "slowHeadersOnly").unwrap_or(false)}))
}

fn work_offline(s: &mut Session, p: &Value) -> CmdResult {
    let offline = !opt_bool(p, "offline").unwrap_or(false);
    s.notify(Notice::Info(if offline { "Working offline.".into() } else { "Working online.".into() }));
    Ok(json!({"offline": offline}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rules::tests::session;
    use mailcraft_model::{Account, AccountKind, ServerSettings};

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
        assert_eq!(s.run("sendReceive.workOffline", &json!({"offline": true})).unwrap()["offline"], false);
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
}
