//! `file.*`: the File backstage — Info (account summary), Open & Export (import/export through
//! `mailcraft-store`), Save As (.eml), Save Attachments, data files (new / close / reveal), Options, Exit.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::cmd::rules::{dialog, focused_item, need_selection, write_file};
use crate::cmd::util::{opt_str, req_str};
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session, always};
use mailcraft_model::mailbox::LOCAL_ACCOUNT_FALLBACK_NAME;
use mailcraft_model::{
    Account, AccountId, AccountKind, FolderId, FolderKind, FolderRole, ItemId, MailItem, QuickStepAction,
    RuleAction,
};

/// Most attachments written by one Save Attachments run.
const MAX_SAVED_ATTACHMENTS: usize = 10_000;
/// Most data files (accounts) in one profile.
const MAX_ACCOUNTS: usize = 256;

fn spec(
    id: &'static str,
    label: &'static str,
    location: &'static str,
    params: &'static str,
    mutates: bool,
    enabled: fn(&Session) -> Option<&'static str>,
    run: fn(&mut Session, &Value) -> CmdResult,
) -> CommandSpec {
    CommandSpec { id, label, location, shortcut: "", params, mutates, enabled, run }
}

pub fn specs() -> Vec<CommandSpec> {
    const ROOT: &str = "Account root context menu";
    vec![
        spec("file.info", "Info", "File tab", "", false, always, info),
        spec(
            "file.openExport",
            "Import/Export",
            "File › Open & Export",
            r#"{"action"?: "importEml"|"importMbox"|"importMaildir"|"exportMbox", "path"?: string, "folder"?: number} (no action = open the wizard)"#,
            true,
            always,
            open_export,
        ),
        spec(
            "file.saveAs",
            "Save As",
            "File › Save As",
            r#"{"path"?: string (file or directory), "ids"?: [number]}"#,
            false,
            need_selection,
            save_as,
        ),
        spec(
            "file.saveAttachments",
            "Save Attachments",
            "File › Save Attachments",
            r#"{"dir"?: string, "ids"?: [number]}"#,
            false,
            has_attachments,
            save_attachments,
        ),
        spec("file.print", "Print", "File › Print", r#"{"ids"?: [number]}"#, false, need_selection, print),
        spec("file.options", "Options", "File › Options", r#"{"page"?: string}"#, false, always, options),
        spec(
            "file.accountSettings",
            "Account Properties...",
            "Account root context menu; File › Info",
            r#"{"account"?: string|number}"#,
            false,
            has_accounts,
            account_settings,
        ),
        spec("file.manageRules", "Manage Rules & Alerts", "File › Info", "", false, always, manage_rules),
        spec("file.cleanupTools", "Tools", "File › Info", "", false, always, cleanup_tools),
        spec(
            "file.newDataFile",
            "New Data File",
            "Home › New Items › More Items",
            r#"{"path"?: string, "name"?: string}"#,
            true,
            always,
            new_data_file,
        ),
        spec(
            "file.closeDataFile",
            "Remove",
            ROOT,
            r#"{"store": string|number (account id, name or address)}"#,
            true,
            has_secondary,
            close_data_file,
        ),
        spec(
            "file.openFileLocation",
            "Open File Location",
            ROOT,
            r#"{"store"?: string|number}"#,
            false,
            has_store,
            open_file_location,
        ),
        spec("file.exit", "Close", "Window caption", "", false, always, exit),
    ]
}

fn has_accounts(s: &Session) -> Option<&'static str> {
    if s.mailbox.accounts.is_empty() { Some("no accounts") } else { None }
}

fn has_secondary(s: &Session) -> Option<&'static str> {
    if s.mailbox.accounts.len() > 1 { None } else { Some("only the primary data file is open") }
}

fn has_store(s: &Session) -> Option<&'static str> {
    if s.store.is_some() { None } else { Some("this session has no data file on disk") }
}

fn has_attachments(s: &Session) -> Option<&'static str> {
    let any = s
        .target_ids(&Value::Null)
        .iter()
        .any(|id| s.mailbox.mail.get(id).is_some_and(MailItem::has_attachments));
    if any { None } else { Some("no attachments in the selection") }
}

/// Account named by `p[key]`: id number (or numeric string), address or display name; the current account
/// when absent.
fn find_account(s: &Session, p: &Value, key: &str) -> Result<AccountId, CmdError> {
    let accounts = &s.mailbox.accounts;
    let found = match p.get(key) {
        None | Some(Value::Null) => s.current_account(),
        Some(Value::Number(n)) => {
            n.as_u64().and_then(|n| accounts.iter().find(|a| u64::from(a.id.0) == n)).map(|a| a.id)
        }
        Some(Value::String(t)) => {
            let t = t.trim();
            accounts
                .iter()
                .find(|a| t.parse::<u64>().is_ok_and(|n| u64::from(a.id.0) == n))
                .or_else(|| accounts.iter().find(|a| a.address.eq_ignore_ascii_case(t)))
                .or_else(|| accounts.iter().find(|a| a.display_name.eq_ignore_ascii_case(t)))
                .map(|a| a.id)
        }
        Some(_) => None,
    };
    found.ok_or_else(|| {
        CmdError::Params(format!("no such account/data file {}", p.get(key).unwrap_or(&Value::Null)))
    })
}

fn kind_name(k: AccountKind) -> &'static str {
    match k {
        AccountKind::Local => "local",
        AccountKind::Imap => "imap",
        AccountKind::Jmap => "jmap",
        AccountKind::Exchange => "exchange",
    }
}

fn account_json(s: &Session, a: &Account) -> Value {
    let folders: Vec<FolderId> =
        s.mailbox.folders.values().filter(|f| f.account == a.id).map(|f| f.id).collect();
    let items = s.mailbox.mail.values().filter(|m| folders.contains(&m.folder));
    let (total, unread, bytes) = items.fold((0usize, 0usize, 0u64), |(t, u, b), m| {
        (t.saturating_add(1), u.saturating_add(usize::from(!m.read)), b.saturating_add(m.size))
    });
    json!({
        "id": a.id.0,
        "name": a.display_name,
        "address": a.address,
        "kind": kind_name(a.kind),
        "default": a.is_default,
        "canSend": a.can_send(),
        "folders": folders.len(),
        "items": total,
        "unread": unread,
        "bytes": bytes,
    })
}

fn info(s: &mut Session, _: &Value) -> CmdResult {
    let accounts: Vec<Value> = s.mailbox.accounts.iter().map(|a| account_json(s, a)).collect();
    let current = s.current_account().map(|a| a.0);
    let store = s
        .store
        .as_ref()
        .map(|st| json!({"path": st.dir.display().to_string(), "journalBytes": st.journal_bytes()}));
    let summary = json!({"page": "info", "current": current, "accounts": accounts, "store": store, "rules": s.mailbox.rules.len()});
    s.notify(Notice::OpenDialog("backstage".into(), json!({"page": "info"})));
    Ok(summary)
}

/// Target folder for imports: `p.folder`, else the selected folder; must be a mail folder.
fn mail_folder(s: &Session, p: &Value) -> Result<FolderId, CmdError> {
    let f = s.target_folder(p);
    match s.mailbox.folder(f) {
        Some(folder) if folder.kind == FolderKind::Mail => Ok(f),
        Some(folder) => Err(CmdError::Params(format!("\"{}\" is not a mail folder", folder.name))),
        None => Err(CmdError::Params(format!("no such folder {}", f.0))),
    }
}

fn store_err(e: mailcraft_store::StoreError) -> CmdError {
    CmdError::Failed(e.to_string())
}

fn open_export(s: &mut Session, p: &Value) -> CmdResult {
    let Some(action) = opt_str(p, "action") else {
        return dialog(s, "importExport", json!({}));
    };
    let path = PathBuf::from(req_str(p, "path")?);
    let folder = mail_folder(s, p)?;
    match action {
        "importEml" => {
            let id = match s.store.as_mut() {
                Some(st) => st.import_eml(&mut s.mailbox, &path, folder).map_err(store_err)?,
                None => {
                    let item = mailcraft_store::import::read_eml(&path, folder).map_err(store_err)?;
                    mailcraft_store::import::insert_item(&mut s.mailbox, item).map_err(store_err)?
                }
            };
            Ok(json!({"imported": 1, "ids": [id.0], "folder": folder.0}))
        }
        "importMbox" | "importMaildir" => {
            let ids = if action == "importMbox" {
                mailcraft_store::import_mbox(&mut s.mailbox, &path, folder)
            } else {
                mailcraft_store::import_maildir(&mut s.mailbox, &path, folder)
            }
            .map_err(store_err)?;
            Ok(json!({"imported": ids.len(), "folder": folder.0}))
        }
        "exportMbox" => {
            let mut items: Vec<&MailItem> = s.mailbox.items_in(folder);
            items.sort_by_key(|m| (m.received.unix, m.id));
            let n = items.len();
            match s.store.as_ref() {
                Some(st) => st.export_mbox(&items, &path).map_err(store_err)?,
                None => mailcraft_store::export_mbox(&items, &path).map_err(store_err)?,
            }
            Ok(json!({"exported": n, "path": path.display().to_string()}))
        }
        other => Err(CmdError::Params(format!("unknown action `{other}`"))),
    }
}

/// Raw RFC 5322 source of an item: kept bytes, else the blob store, else rebuilt from its fields.
fn raw_source(s: &Session, m: &MailItem) -> Vec<u8> {
    if let Some(raw) = &m.raw {
        return raw.clone();
    }
    if let Some(st) = &s.store
        && let Ok(Some(raw)) = st.raw_of(m)
    {
        return raw;
    }
    mailcraft_store::rebuild_message(m)
}

/// Safe file name from a subject ("Untitled" when empty).
fn file_stem(subject: &str) -> String {
    let cleaned: String =
        subject.chars().map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let t: String = cleaned.trim().trim_matches('.').chars().take(120).collect();
    if t.trim().is_empty() { "Untitled".to_owned() } else { t.trim().to_owned() }
}

/// `dir/name`, adding " (2)", " (3)" … before the extension while the name is taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (name.get(..i).unwrap_or(name), name.get(i..).unwrap_or("")),
        _ => (name, ""),
    };
    for n in 2u32..10_000 {
        let p = dir.join(format!("{stem} ({n}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem} (copy){ext}"))
}

fn save_as(s: &mut Session, p: &Value) -> CmdResult {
    let id = focused_item(s, p).ok_or_else(|| CmdError::Disabled("no message selected".into()))?;
    let Some(m) = s.mailbox.mail.get(&id) else {
        return Err(CmdError::Disabled("no message selected".into()));
    };
    let Some(path) = opt_str(p, "path") else {
        let name = format!("{}.eml", file_stem(&m.subject));
        return dialog(s, "saveAs", json!({"item": id.0, "suggested": name}));
    };
    let mut path = PathBuf::from(path);
    if path.is_dir() {
        path = unique_path(&path, &format!("{}.eml", file_stem(&m.subject)));
    }
    let data = raw_source(s, m);
    write_file(&path, &data)?;
    Ok(json!({"path": path.display().to_string(), "bytes": data.len()}))
}

fn save_attachments(s: &mut Session, p: &Value) -> CmdResult {
    let ids: Vec<ItemId> = s
        .target_ids(p)
        .into_iter()
        .filter(|id| s.mailbox.mail.get(id).is_some_and(MailItem::has_attachments))
        .collect();
    if ids.is_empty() {
        return Err(CmdError::Disabled("no attachments in the selection".into()));
    }
    let Some(dir) = opt_str(p, "dir") else {
        let files: Vec<Value> = ids
            .iter()
            .filter_map(|id| s.mailbox.mail.get(id))
            .flat_map(|m| {
                m.attachments
                    .iter()
                    .filter(|a| !a.inline)
                    .map(|a| json!({"item": m.id.0, "filename": a.filename, "size": a.size}))
            })
            .collect();
        return dialog(s, "saveAttachments", json!({"files": files}));
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return Err(CmdError::Params(format!("{} is not a directory", dir.display())));
    }
    let mut saved = Vec::new();
    let mut missing = 0usize;
    for id in &ids {
        let Some(m) = s.mailbox.mail.get(id) else { continue };
        if m.raw.is_none() && m.blob.is_none() {
            missing = missing.saturating_add(1);
            continue;
        }
        let raw = raw_source(s, m);
        let Ok(msg) = mailcraft_mime::parse(&raw) else {
            missing = missing.saturating_add(1);
            continue;
        };
        for a in msg.attachments().into_iter().filter(|a| !a.inline) {
            if saved.len() >= MAX_SAVED_ATTACHMENTS {
                break;
            }
            let mut name = mailcraft_mime::message::sanitize_filename(&a.filename);
            if name.is_empty() {
                name = "attachment.dat".into();
            }
            let path = unique_path(&dir, &name);
            write_file(&path, &a.data)?;
            saved.push(path.display().to_string());
        }
    }
    Ok(json!({"saved": saved, "unavailable": missing}))
}

fn print(s: &mut Session, p: &Value) -> CmdResult {
    let ids: Vec<u64> = s.target_ids(p).iter().map(|i| i.0).collect();
    dialog(s, "print", json!({"ids": ids}))
}

fn options(s: &mut Session, p: &Value) -> CmdResult {
    let page = opt_str(p, "page").unwrap_or("general");
    s.run("options.open", &json!({"page": page}))
}

fn account_settings(s: &mut Session, p: &Value) -> CmdResult {
    let id = find_account(s, p, "account")?;
    let acct = s.mailbox.account(id).map(|a| account_json(s, a)).unwrap_or(Value::Null);
    dialog(s, "accountSettings", json!({"account": acct}))
}

fn manage_rules(s: &mut Session, p: &Value) -> CmdResult {
    s.run("rules.manage", p)
}

fn cleanup_tools(s: &mut Session, _: &Value) -> CmdResult {
    dialog(s, "mailboxCleanup", json!({}))
}

fn new_data_file(s: &mut Session, p: &Value) -> CmdResult {
    if s.mailbox.accounts.len() >= MAX_ACCOUNTS {
        return Err(CmdError::Failed(format!("at most {MAX_ACCOUNTS} data files")));
    }
    let from_path = opt_str(p, "path").and_then(|path| Path::new(path).file_stem()).and_then(|n| n.to_str());
    let clean = |n: &str| -> String { n.chars().filter(|c| !c.is_control()).take(255).collect() };
    let base = opt_str(p, "name")
        .or(from_path)
        .map(|n| clean(n).trim().to_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| LOCAL_ACCOUNT_FALLBACK_NAME.to_owned());
    let taken = |n: &str| s.mailbox.accounts.iter().any(|a| a.display_name.eq_ignore_ascii_case(n));
    let mut name = base.clone();
    let mut k = 2u32;
    while taken(&name) && k < 10_000 {
        name = format!("{base} ({k})");
        k = k.saturating_add(1);
    }
    let id = s.mailbox.add_account(Account {
        kind: AccountKind::Local,
        display_name: name.clone(),
        sender_name: name.clone(),
        is_default: false,
        ..Default::default()
    });
    s.mailbox.create_default_folders(id);
    Ok(json!({"account": id.0, "name": name}))
}

fn close_data_file(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("store").is_none() {
        return Err(CmdError::Params("`store` (account id, name or address) required".into()));
    }
    let id = find_account(s, p, "store")?;
    let is_primary = s.mailbox.account(id).is_some_and(|a| a.is_default);
    if is_primary {
        return Err(CmdError::Disabled("the primary data file cannot be removed".into()));
    }
    let folders: Vec<FolderId> =
        s.mailbox.folders.values().filter(|f| f.account == id).map(|f| f.id).collect();
    let mb = &mut s.mailbox;
    mb.mail.retain(|_, m| !folders.contains(&m.folder));
    mb.appointments.retain(|_, x| !folders.contains(&x.folder));
    mb.contacts.retain(|_, x| !folders.contains(&x.folder));
    mb.tasks.retain(|_, x| !folders.contains(&x.folder));
    mb.notes.retain(|_, x| !folders.contains(&x.folder));
    for f in &folders {
        mb.folders.remove(f);
        mb.views.remove(f);
    }
    mb.accounts.retain(|a| a.id != id);
    // Rules that file into the removed data file are turned off; quick steps that do ask for a folder again.
    let gone = |f: &FolderId| folders.contains(f);
    for rule in &mut mb.rules {
        if rule.actions.iter().any(|a| matches!(a, RuleAction::MoveTo(f) | RuleAction::CopyTo(f) if gone(f)))
        {
            rule.enabled = false;
        }
    }
    for step in &mut mb.quick_steps {
        for action in &mut step.actions {
            if let QuickStepAction::MoveTo(target) | QuickStepAction::CopyTo(target) = action
                && target.as_ref().is_some_and(gone)
            {
                *target = None;
            }
        }
    }
    s.view.collapsed_groups.retain(|f, _| !folders.contains(f));
    if folders.contains(&s.selection.folder) {
        let fallback = s
            .mailbox
            .default_account()
            .map(|a| a.id)
            .and_then(|a| s.mailbox.role_folder(a, FolderRole::Inbox))
            .unwrap_or_default();
        s.selection = crate::Selection { folder: fallback, items: Vec::new(), focus: None };
    }
    Ok(json!({"removed": id.0, "folders": folders.len()}))
}

fn open_file_location(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("store").is_some() {
        find_account(s, p, "store")?;
    }
    let Some(st) = &s.store else {
        return Err(CmdError::Disabled("this session has no data file on disk".into()));
    };
    let path = st.dir.display().to_string();
    dialog(s, "revealPath", json!({"path": path}))
}

/// Flushes pending ops and checkpoints the store, then asks the UI to close the window.
fn exit(s: &mut Session, _: &Value) -> CmdResult {
    if let Some(st) = s.store.as_mut() {
        // Queued ops leave the queue only once written, so a failed save loses nothing.
        let res = st.append(&s.pending_ops).and_then(|()| {
            s.pending_ops.clear();
            st.checkpoint(&s.mailbox)
        });
        if let Err(e) = res {
            let msg = format!("Could not save the data file: {e}");
            s.notify(Notice::Error(msg.clone()));
            return Err(CmdError::Failed(msg));
        }
    } else {
        // Nothing to write to (in-memory session): the queue only mirrors the mailbox.
        s.pending_ops.clear();
    }
    s.notify(Notice::OpenDialog("exit".into(), json!({})));
    Ok(json!({"exit": true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rules::tests::session;

    const EML: &[u8] = b"From: Ann <ann@example.com>\r\nTo: me@example.net\r\nSubject: Report: Q3/Q4\r\nDate: Thu, 08 Oct 2026 07:30:00 +0000\r\nMessage-ID: <a1@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n--b\r\nContent-Type: text/plain; name=\"notes.txt\"\r\nContent-Disposition: attachment; filename=\"notes.txt\"\r\n\r\nhello notes\r\n--b--\r\n";

    #[test]
    fn info_summarises_accounts() {
        let (mut s, _, _) = session();
        let r = s.run("file.info", &json!({})).unwrap();
        assert_eq!(r["accounts"][0]["items"], 3);
        assert_eq!(r["accounts"][0]["unread"], 3);
        assert!(matches!(s.notices.last(), Some(Notice::OpenDialog(n, _)) if n == "backstage"));
    }

    #[test]
    fn import_eml_save_as_and_attachments() {
        let (mut s, inbox, _) = session();
        let dir = tempfile::tempdir().unwrap();
        let eml = dir.path().join("in.eml");
        std::fs::write(&eml, EML).unwrap();
        assert_eq!(s.run("file.openExport", &json!({})).unwrap()["dialog"], "importExport");
        let r = s
            .run(
                "file.openExport",
                &json!({"action": "importEml", "path": eml.to_str().unwrap(), "folder": inbox.0}),
            )
            .unwrap();
        let id = ItemId(r["ids"][0].as_u64().unwrap());
        assert_eq!(s.mailbox.mail[&id].subject, "Report: Q3/Q4");
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        // Save As into a directory picks a safe name from the subject.
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let r = s.run("file.saveAs", &json!({"path": out.to_str().unwrap()})).unwrap();
        let saved = PathBuf::from(r["path"].as_str().unwrap());
        assert_eq!(saved.file_name().unwrap(), "Report_ Q3_Q4.eml");
        assert_eq!(std::fs::read(&saved).unwrap(), EML);
        assert!(s.enabled("file.saveAttachments").is_none());
        let r = s.run("file.saveAttachments", &json!({"dir": out.to_str().unwrap()})).unwrap();
        assert_eq!(r["saved"].as_array().unwrap().len(), 1);
        assert_eq!(std::fs::read(out.join("notes.txt")).unwrap(), b"hello notes");
        // A second save does not overwrite.
        let r = s.run("file.saveAttachments", &json!({"dir": out.to_str().unwrap()})).unwrap();
        assert!(r["saved"][0].as_str().unwrap().ends_with("notes (2).txt"));
    }

    #[test]
    fn mbox_export_import_roundtrip() {
        let (mut s, inbox, proj) = session();
        let dir = tempfile::tempdir().unwrap();
        let mbox = dir.path().join("inbox.mbox");
        let r = s
            .run(
                "file.openExport",
                &json!({"action": "exportMbox", "path": mbox.to_str().unwrap(), "folder": inbox.0}),
            )
            .unwrap();
        assert_eq!(r["exported"], 3);
        let r = s
            .run(
                "file.openExport",
                &json!({"action": "importMbox", "path": mbox.to_str().unwrap(), "folder": proj.0}),
            )
            .unwrap();
        assert_eq!(r["imported"], 3);
        assert_eq!(s.mailbox.total_count(proj), 3);
        assert!(matches!(
            s.run("file.openExport", &json!({"action": "nope", "path": "x"})),
            Err(CmdError::Params(_))
        ));
    }

    #[test]
    fn data_files() {
        let (mut s, _, _) = session();
        assert!(s.enabled("file.closeDataFile").is_some());
        let r = s.run("file.newDataFile", &json!({"path": "/tmp/Archive 2026.pst"})).unwrap();
        assert_eq!(r["name"], "Archive 2026");
        let r2 = s.run("file.newDataFile", &json!({"name": "archive 2026"})).unwrap();
        assert_eq!(r2["name"], "archive 2026 (2)");
        let acct = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s.mailbox.add_mail(MailItem { folder: inbox, ..Default::default() });
        s.selection.folder = inbox;
        let before = s.mailbox.mail.len();
        let r = s.run("file.closeDataFile", &json!({"store": "Archive 2026"})).unwrap();
        assert_eq!(r["removed"], acct.0);
        assert_eq!(s.mailbox.mail.len(), before - 1);
        assert!(s.mailbox.folders.values().all(|f| f.account != acct));
        assert_ne!(s.selection.folder, inbox);
        assert!(s.mailbox.validate().is_empty());
        assert!(matches!(s.run("file.closeDataFile", &json!({"store": "Test"})), Err(CmdError::Disabled(_))));
    }

    #[test]
    fn dialogs_and_exit() {
        let (mut s, _, _) = session();
        assert!(s.enabled("file.openFileLocation").is_some());
        assert_eq!(
            s.run("file.accountSettings", &json!({"account": "Test"})).unwrap()["dialog"],
            "accountSettings"
        );
        assert!(matches!(
            s.run("file.accountSettings", &json!({"account": "zzz"})),
            Err(CmdError::Params(_))
        ));
        assert_eq!(s.run("file.cleanupTools", &json!({})).unwrap()["dialog"], "mailboxCleanup");
        assert_eq!(s.run("file.saveAs", &json!({})).unwrap()["dialog"], "saveAs");
        assert_eq!(s.run("file.exit", &json!({})).unwrap()["exit"], true);
    }

    #[test]
    fn close_data_file_disables_rules_and_resets_quick_steps_into_it() {
        let (mut s, _, _) = session();
        let r = s.run("file.newDataFile", &json!({"name": "Old"})).unwrap();
        let acct = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let target = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s.run("rules.create", &json!({"name": "File it", "actions": [{"MoveTo": target.0}]})).unwrap();
        s.run("quicksteps.edit", &json!({"id": 1, "actions": [{"MoveTo": target.0}]})).unwrap();
        s.mailbox.add_favorite(target).unwrap();
        s.run("file.closeDataFile", &json!({"store": "Old"})).unwrap();
        assert!(!s.mailbox.rules[0].enabled, "a rule moving into a removed data file must be turned off");
        assert!(s.mailbox.quick_steps[0].needs_setup(), "the quick step must ask for a folder again");
        assert!(!s.mailbox.favorites().contains(&target));
    }

    #[test]
    fn new_data_file_never_gets_an_empty_name() {
        let (mut s, _, _) = session();
        let r = s.run("file.newDataFile", &json!({"name": "\u{7}\u{7}"})).unwrap();
        assert_eq!(r["name"], LOCAL_ACCOUNT_FALLBACK_NAME);
    }

    #[cfg(unix)]
    #[test]
    fn exit_keeps_pending_ops_when_the_write_fails() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (mut s, inbox, _) = session();
        s.store = Some(mailcraft_store::Store::open(dir.path()).unwrap());
        // A raw message must go to the blob store, which is made read-only so the journal write fails.
        let item =
            MailItem { id: ItemId(9_999), folder: inbox, raw: Some(EML.to_vec()), ..Default::default() };
        s.pending_ops.push(mailcraft_store::Op::PutMail(Box::new(item)));
        let blobs = dir.path().join("blobs");
        std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = s.run("file.exit", &json!({}));
        std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o755)).unwrap();
        if r.is_ok() {
            // Permissions are not enforced (running as root): nothing to check.
            return;
        }
        assert_eq!(s.pending_ops.len(), 1, "unsaved changes must stay queued for the next attempt");
    }

    #[test]
    fn exit_flushes_store() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _, _) = session();
        s.store = Some(mailcraft_store::Store::open(dir.path()).unwrap());
        s.run("file.newDataFile", &json!({"name": "Extra"})).unwrap();
        s.run("file.exit", &json!({})).unwrap();
        assert!(s.pending_ops.is_empty());
        let open = s.run("file.openFileLocation", &json!({})).unwrap();
        assert_eq!(open["dialog"], "revealPath");
        s.store = None;
        let st = mailcraft_store::Store::open(dir.path()).unwrap();
        let mb = st.load().unwrap();
        assert!(mb.accounts.iter().any(|a| a.display_name == "Extra"));
    }
}
