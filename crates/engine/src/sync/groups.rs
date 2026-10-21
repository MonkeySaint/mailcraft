//! User-defined Send/Receive Groups (Send / Receive › Send/Receive Groups › Define Send/Receive Groups,
//! also Options › Advanced › Send/Receive).
//!
//! A group is a set of accounts, and for each account what a run does: send the Outbox, receive (all
//! folders or chosen ones), and whether new mail comes as headers only. A group also says when it runs: with
//! Send/Receive All Folders (F9), on a schedule (one interval while online, one while working offline) and
//! when MailCraft exits. The built-in "All Accounts" group stays what it was: every account included in
//! send/receive (Account Settings), the Options › Advanced interval and exit option.
//!
//! Groups are saved with the account options (`account-options.json`).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Plan, dispatch, is_server_account, refresh_status};
use crate::{CmdError, Notice, Session};
use mailcraft_model::{AccountId, FolderId, FolderKind};

/// Name of the built-in group.
pub const ALL_ACCOUNTS: &str = "All Accounts";
/// Most user-defined groups.
pub const MAX_GROUPS: usize = 64;
/// Longest group name.
pub const MAX_NAME_CHARS: usize = 64;
/// Longest schedule interval (minutes): one day.
pub const MAX_MINUTES: u32 = 1440;
/// Most folders one member lists.
const MAX_FOLDERS: usize = 1_000;

/// One account in a group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GroupMember {
    pub account: u32,
    /// Send the account's Outbox.
    pub send: bool,
    /// Receive mail.
    pub receive: bool,
    /// New messages arrive as headers only.
    pub headers_only: bool,
    /// Folders to update (`None` = every folder).
    pub folders: Option<Vec<u64>>,
}

impl Default for GroupMember {
    fn default() -> Self {
        GroupMember { account: 0, send: true, receive: true, headers_only: false, folders: None }
    }
}

/// A user-defined Send/Receive group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SendReceiveGroup {
    pub name: String,
    pub members: Vec<GroupMember>,
    /// Online: part of Send/Receive All Folders (F9).
    pub include: bool,
    /// Online: scheduled run every N minutes (0 = no schedule).
    pub every_minutes: u32,
    /// Run when MailCraft exits.
    pub on_exit: bool,
    /// Working offline: F9 still runs this group.
    pub offline_include: bool,
    /// Working offline: scheduled run every N minutes (0 = none).
    pub offline_every_minutes: u32,
}

impl Default for SendReceiveGroup {
    fn default() -> Self {
        SendReceiveGroup {
            name: String::new(),
            members: Vec::new(),
            include: true,
            every_minutes: 0,
            on_exit: false,
            offline_include: false,
            offline_every_minutes: 0,
        }
    }
}

impl SendReceiveGroup {
    fn to_json(&self, s: &Session) -> Value {
        let members: Vec<Value> = self
            .members
            .iter()
            .map(|m| {
                let name = s
                    .mailbox
                    .account(AccountId(m.account))
                    .map(|a| a.display_name.clone())
                    .unwrap_or_default();
                json!({
                    "account": m.account,
                    "accountName": name,
                    "send": m.send,
                    "receive": m.receive,
                    "headersOnly": m.headers_only,
                    "folders": m.folders,
                })
            })
            .collect();
        json!({
            "name": self.name,
            "builtIn": false,
            "members": members,
            "include": self.include,
            "everyMinutes": self.every_minutes,
            "onExit": self.on_exit,
            "offlineInclude": self.offline_include,
            "offlineEveryMinutes": self.offline_every_minutes,
        })
    }
}

/// The user-defined groups.
pub fn groups(s: &Session) -> &[SendReceiveGroup] {
    &s.sync.prefs.data.groups
}

/// The user-defined group called `name` (case-insensitive).
pub fn group(s: &Session, name: &str) -> Option<SendReceiveGroup> {
    let n = name.trim();
    groups(s).iter().find(|g| g.name.eq_ignore_ascii_case(n)).cloned()
}

/// The built-in group as the dialog shows it (its options are Options › Advanced ones).
fn all_accounts_json(s: &Session) -> Value {
    let members: Vec<Value> = super::server_accounts(s)
        .into_iter()
        .map(|a| {
            let opts = crate::accounts::account_options(s, a);
            let name = s.mailbox.account(a).map(|x| x.display_name.clone()).unwrap_or_default();
            json!({
                "account": a.0,
                "accountName": name,
                "send": opts.include_in_send_receive,
                "receive": opts.include_in_send_receive,
                "headersOnly": false,
                "folders": null,
            })
        })
        .collect();
    json!({
        "name": ALL_ACCOUNTS,
        "builtIn": true,
        "members": members,
        "include": true,
        "everyMinutes": s.view.auto_send_receive_minutes,
        "onExit": s.settings.bool("advanced.sendReceiveOnExit"),
        "offlineInclude": false,
        "offlineEveryMinutes": 0,
    })
}

/// Every group for the Define Send/Receive Groups dialog, the built-in one first, and the server accounts
/// with their mail folders (for the member editor).
pub fn listing(s: &Session) -> Value {
    let mut list = vec![all_accounts_json(s)];
    list.extend(groups(s).iter().map(|g| g.to_json(s)));
    let accounts: Vec<Value> = super::server_accounts(s)
        .into_iter()
        .filter_map(|a| s.mailbox.account(a))
        .map(|a| {
            let folders: Vec<Value> = s
                .mailbox
                .folders
                .values()
                .filter(|f| f.account == a.id && f.kind == FolderKind::Mail)
                .take(MAX_FOLDERS)
                .map(|f| json!({"id": f.id.0, "path": s.mailbox.folder_path(f.id)}))
                .collect();
            json!({"id": a.id.0, "name": a.display_name, "address": a.address, "folders": folders})
        })
        .collect();
    json!({"groups": list, "accounts": accounts})
}

/// A group from command params (`{"name", "members": [...], "include", "everyMinutes", …}`), checked.
fn parse_group(s: &Session, v: &Value) -> Result<SendReceiveGroup, CmdError> {
    let name = v.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    check_name(name)?;
    let minutes = |key: &str| -> Result<u32, CmdError> {
        match v.get(key) {
            None | Some(Value::Null) => Ok(0),
            Some(x) => x
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n <= MAX_MINUTES)
                .ok_or_else(|| CmdError::Params(format!("`{key}` takes minutes from 0 to {MAX_MINUTES}"))),
        }
    };
    let flag = |key: &str, default: bool| v.get(key).and_then(Value::as_bool).unwrap_or(default);
    let mut members = Vec::new();
    let mut seen = BTreeSet::new();
    for m in v.get("members").and_then(Value::as_array).into_iter().flatten().take(256) {
        let account = m
            .get("account")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| CmdError::Params("each member needs an `account` id".into()))?;
        let acct = s
            .mailbox
            .account(AccountId(account))
            .filter(|a| is_server_account(a.kind))
            .ok_or_else(|| CmdError::Params(format!("no mail server account {account}")))?;
        if !seen.insert(account) {
            continue;
        }
        let folders = match m.get("folders") {
            None | Some(Value::Null) => None,
            Some(Value::Array(list)) => Some(
                list.iter()
                    .filter_map(Value::as_u64)
                    .filter(|f| s.mailbox.folder(FolderId(*f)).is_some_and(|x| x.account == acct.id))
                    .take(MAX_FOLDERS)
                    .collect(),
            ),
            Some(_) => return Err(CmdError::Params("`folders` is a list of folder ids or null".into())),
        };
        members.push(GroupMember {
            account,
            send: m.get("send").and_then(Value::as_bool).unwrap_or(true),
            receive: m.get("receive").and_then(Value::as_bool).unwrap_or(true),
            headers_only: m.get("headersOnly").and_then(Value::as_bool).unwrap_or(false),
            folders,
        });
    }
    Ok(SendReceiveGroup {
        name: name.to_owned(),
        members,
        include: flag("include", true),
        every_minutes: minutes("everyMinutes")?,
        on_exit: flag("onExit", false),
        offline_include: flag("offlineInclude", false),
        offline_every_minutes: minutes("offlineEveryMinutes")?,
    })
}

fn check_name(name: &str) -> Result<(), CmdError> {
    if name.is_empty() {
        return Err(CmdError::Params("the group needs a name".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(CmdError::Params(format!("group names have at most {MAX_NAME_CHARS} characters")));
    }
    if name.eq_ignore_ascii_case(ALL_ACCOUNTS) {
        return Err(CmdError::Params(format!("\"{ALL_ACCOUNTS}\" is the built-in group")));
    }
    Ok(())
}

fn position(s: &Session, name: &str) -> Option<usize> {
    groups(s).iter().position(|g| g.name.eq_ignore_ascii_case(name.trim()))
}

fn save(s: &mut Session) {
    crate::accounts::save_prefs(s);
    s.sync.dirty = true;
}

/// `sendReceive.defineGroups`: without `action`, the dialog; `save` (`group`, optional `oldName` to rename
/// or replace), `copy` (`name`, `newName`), `rename` (`name`, `newName`), `remove` (`name`), `list`.
pub fn define(s: &mut Session, p: &Value) -> crate::CmdResult {
    crate::accounts::load_prefs(s);
    let action = p.get("action").and_then(Value::as_str).unwrap_or("dialog");
    let name = p.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_owned();
    let new_name = p.get("newName").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_owned();
    match action {
        "dialog" => {
            let list = listing(s);
            s.notify(Notice::OpenDialog("sendReceiveGroups".into(), list.clone()));
            let mut v = json!({"dialog": "sendReceiveGroups"});
            if let (Some(o), Some(l)) = (v.as_object_mut(), list.as_object()) {
                o.extend(l.clone());
            }
            Ok(v)
        }
        "list" => Ok(listing(s)),
        "save" => {
            let g = parse_group(s, p.get("group").unwrap_or(&Value::Null))?;
            let old = p.get("oldName").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty());
            let slot = old.and_then(|o| position(s, o)).or_else(|| position(s, &g.name));
            if let Some(clash) = position(s, &g.name)
                && Some(clash) != slot
            {
                return Err(CmdError::Params(format!("there is already a group called \"{}\"", g.name)));
            }
            let list = &mut crate::accounts::prefs(s).groups;
            match slot {
                Some(i) => {
                    if let Some(x) = list.get_mut(i) {
                        *x = g.clone();
                    }
                }
                None if list.len() >= MAX_GROUPS => {
                    return Err(CmdError::Failed(format!("MailCraft keeps at most {MAX_GROUPS} groups")));
                }
                None => list.push(g.clone()),
            }
            save(s);
            Ok(json!({"saved": g.name, "groups": groups(s).len()}))
        }
        "copy" | "rename" => {
            check_name(&new_name)?;
            let i =
                position(s, &name).ok_or_else(|| CmdError::Params(format!("no group called \"{name}\"")))?;
            if position(s, &new_name).is_some_and(|j| action == "copy" || j != i) {
                return Err(CmdError::Params(format!("there is already a group called \"{new_name}\"")));
            }
            let list = &mut crate::accounts::prefs(s).groups;
            if action == "copy" {
                if list.len() >= MAX_GROUPS {
                    return Err(CmdError::Failed(format!("MailCraft keeps at most {MAX_GROUPS} groups")));
                }
                if let Some(mut g) = list.get(i).cloned() {
                    g.name.clone_from(&new_name);
                    list.push(g);
                }
            } else if let Some(g) = list.get_mut(i) {
                g.name.clone_from(&new_name);
            }
            save(s);
            let mut out = serde_json::Map::new();
            out.insert(if action == "copy" { "copied" } else { "renamed" }.into(), json!(new_name));
            Ok(Value::Object(out))
        }
        "remove" => {
            let i =
                position(s, &name).ok_or_else(|| CmdError::Params(format!("no group called \"{name}\"")))?;
            crate::accounts::prefs(s).groups.remove(i);
            s.sync.group_runs.remove(&name.to_lowercase());
            save(s);
            Ok(json!({"removed": name}))
        }
        other => {
            Err(CmdError::Params(format!("unknown action `{}`", other.chars().take(40).collect::<String>())))
        }
    }
}

/// Runs a user-defined group: each member account with its own plan. `offline_ok` lets a group set to run
/// while working offline connect anyway. Returns how many runs started.
pub fn run(s: &mut Session, g: &SendReceiveGroup, user: bool, offline_ok: bool) -> Result<usize, CmdError> {
    if s.view.work_offline && !offline_ok {
        refresh_status(s);
        return Err(CmdError::Disabled("Working offline".into()));
    }
    crate::accounts::load_prefs(s);
    let mut started = 0usize;
    let mut last_err = None;
    for m in &g.members {
        let id = AccountId(m.account);
        if s.mailbox.account(id).is_none_or(|a| !is_server_account(a.kind)) {
            continue;
        }
        let folders = m.folders.clone().map(|list| list.into_iter().map(FolderId).collect::<Vec<_>>());
        let receive = m.receive && folders.as_ref().is_none_or(|f| !f.is_empty());
        if !m.send && !receive {
            continue;
        }
        if receive && m.headers_only {
            s.sync.force_headers.insert(id);
        }
        let plan = Plan { send: m.send, receive, folders: folders.filter(|_| receive), user };
        let result = dispatch(s, id, plan);
        // Headers only applies to this group's run: when nothing was started or queued behind a busy run
        // (an error, nothing to do), it must not carry over to the account's next run.
        let queued = s.sync.runs.get(&id).is_some_and(|r| r.busy.is_some());
        if !matches!(result, Ok(true)) && !queued {
            s.sync.force_headers.remove(&id);
        }
        match result {
            Ok(true) => started = started.saturating_add(1),
            Ok(false) => {}
            Err(e) => {
                let name = s.mailbox.account(id).map(|a| a.display_name.clone()).unwrap_or_default();
                let until = s.now().unix.saturating_add(super::BLOCKED_RETRY_SECS);
                let run = s.sync.runs.entry(id).or_default();
                run.error = Some(e.clone());
                run.blocked_until = until;
                if user {
                    s.notify(Notice::Error(format!("{name}: {e}")));
                }
                last_err = Some(e);
            }
        }
    }
    s.sync.group_runs.insert(g.name.to_lowercase(), s.now().unix);
    refresh_status(s);
    match (started, last_err) {
        (0, Some(e)) => Err(CmdError::Failed(e)),
        _ => Ok(started),
    }
}

/// Groups that Send/Receive All Folders (F9) runs besides All Accounts, as things stand (online or offline).
pub fn f9_groups(s: &Session) -> Vec<SendReceiveGroup> {
    let offline = s.view.work_offline;
    groups(s).iter().filter(|g| if offline { g.offline_include } else { g.include }).cloned().collect()
}

/// Groups to run when MailCraft exits.
pub fn exit_groups(s: &Session) -> Vec<SendReceiveGroup> {
    groups(s).iter().filter(|g| g.on_exit).cloned().collect()
}

/// Scheduled runs: each group with an interval for the current state (online / offline) runs when it is
/// due. The first tick only starts the clock.
pub(crate) fn tick(s: &mut Session) {
    if groups(s).is_empty() {
        return;
    }
    let now = s.now().unix;
    let offline = s.view.work_offline;
    let due: Vec<SendReceiveGroup> = groups(s)
        .iter()
        .filter(|g| {
            let every = if offline { g.offline_every_minutes } else { g.every_minutes };
            if every == 0 {
                return false;
            }
            let key = g.name.to_lowercase();
            match s.sync.group_runs.get(&key) {
                None => false,
                Some(t) => now.saturating_sub(*t) >= i64::from(every).saturating_mul(60),
            }
        })
        .cloned()
        .collect();
    // Start the clock of groups seen for the first time.
    let names: Vec<String> = groups(s).iter().map(|g| g.name.to_lowercase()).collect();
    for n in names {
        s.sync.group_runs.entry(n).or_insert(now);
    }
    for g in due {
        if let Err(e) = run(s, &g, false, offline) {
            log::info!("scheduled send/receive of group {}: {e}", g.name);
            s.sync.group_runs.insert(g.name.to_lowercase(), now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::{Account, AccountKind, Mailbox, ServerSettings};

    #[test]
    fn a_failed_headers_only_run_does_not_leave_headers_only_behind() {
        let mut s = Session::new(Mailbox::new_local("Local Folders"));
        // An IMAP account without a password: its run cannot start.
        let id = s.mailbox.add_account(Account {
            kind: AccountKind::Imap,
            address: "me@example.net".into(),
            display_name: "me@example.net".into(),
            incoming: Some(ServerSettings { host: "127.0.0.1".into(), ..Default::default() }),
            ..Default::default()
        });
        s.mailbox.create_default_folders(id);
        let g = SendReceiveGroup {
            name: "Headers".into(),
            members: vec![GroupMember { account: id.0, headers_only: true, ..GroupMember::default() }],
            ..SendReceiveGroup::default()
        };
        assert!(run(&mut s, &g, true, false).is_err());
        assert!(!s.sync.force_headers.contains(&id), "the next run downloads full items again");
    }
}
