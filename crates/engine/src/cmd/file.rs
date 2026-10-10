//! `file.*`: the File backstage — Info (account summary), Account Settings (`accounts.*`, see
//! `crate::accounts`), Open & Export (import/export through `mailcraft-store` and `mailcraft-formats`: EML,
//! MSG, MBOX, Maildir, iCalendar, vCard, CSV), Save As (.eml, .msg, .txt, .html), Save Attachments, data files
//! (new / close / reveal), Options, Exit.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::cmd::rules::{dialog, focused_item, need_selection, write_file};
use crate::cmd::util::{opt_str, req_str};
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session, always};
use mailcraft_model::mailbox::LOCAL_ACCOUNT_FALLBACK_NAME;
use mailcraft_model::{Account, AccountId, AccountKind, FolderId, FolderKind, FolderRole, ItemId, MailItem};

/// Most attachments written by one Save Attachments run.
const MAX_SAVED_ATTACHMENTS: usize = 10_000;
/// Most data files (accounts) in one profile.
const MAX_ACCOUNTS: usize = 256;
/// Largest file read by an import (each format applies its own, smaller cap).
const MAX_IMPORT_FILE: u64 = 100 * 1024 * 1024;

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
            r#"{"action"?: "import" (by file type)|"importEml"|"importMsg"|"importMbox"|"importMaildir"|"importIcs"|"importVcf"|"importCsv"|"exportMbox"|"exportIcs"|"exportVcf"|"exportCsv", "path"?: string, "folder"?: number} (no action = open the wizard)"#,
            true,
            always,
            open_export,
        ),
        spec(
            "file.saveAs",
            "Save As",
            "File › Save As",
            r#"{"path"?: string (file or directory), "type"?: "eml"|"msg"|"txt"|"html" (default: from the extension, else eml), "ids"?: [number]}"#,
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
        spec(
            "file.print",
            "Print",
            "File › Print",
            r#"{"ids"?: [number], (with "style", "printer" or "path": as print.print)}"#,
            false,
            need_selection,
            print,
        ),
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
    .into_iter()
    .chain(crate::accounts::specs())
    .collect()
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

/// A folder of `kind` for imports/exports: `p.folder` when it is one, else the current account's default
/// folder of `role`, else any folder of that kind.
fn kind_folder(s: &Session, p: &Value, kind: FolderKind, role: FolderRole) -> Result<FolderId, CmdError> {
    if let Some(f) = crate::cmd::util::opt_folder(p) {
        return match s.mailbox.folder(f) {
            Some(folder) if folder.kind == kind => Ok(f),
            Some(folder) => Err(CmdError::Params(format!("\"{}\" cannot hold these items", folder.name))),
            None => Err(CmdError::Params(format!("no such folder {}", f.0))),
        };
    }
    s.current_account()
        .and_then(|a| s.mailbox.role_folder(a, role))
        .or_else(|| s.mailbox.folders.values().find(|f| f.kind == kind).map(|f| f.id))
        .ok_or_else(|| CmdError::Failed(format!("there is no {} folder", role.default_name())))
}

fn format_err(e: mailcraft_formats::FormatError) -> CmdError {
    CmdError::Failed(e.to_string())
}

/// Action for `import` from the file's extension, else its first bytes.
fn detect_import(path: &Path) -> Result<&'static str, CmdError> {
    use mailcraft_formats::Format;
    let fmt = Format::from_path(path).or_else(|| {
        let head = crate::cmd::rules::read_file_capped(path, 64 * 1024).ok()?;
        Format::sniff(&head)
    });
    Ok(match fmt {
        Some(Format::Eml) => "importEml",
        Some(Format::Msg) => "importMsg",
        Some(Format::Mbox) => "importMbox",
        Some(Format::Ics) => "importIcs",
        Some(Format::Vcf) => "importVcf",
        Some(Format::Csv) => "importCsv",
        None if path.is_dir() => "importMaildir",
        None => return Err(CmdError::Params(format!("{}: unknown file type", path.display()))),
    })
}

fn open_export(s: &mut Session, p: &Value) -> CmdResult {
    let Some(action) = opt_str(p, "action") else {
        return dialog(s, "importExport", json!({}));
    };
    let path = PathBuf::from(req_str(p, "path")?);
    let action = if action == "import" { detect_import(&path)? } else { action };
    let now = s.now();
    match action {
        "importEml" => {
            let folder = mail_folder(s, p)?;
            let id = match s.store.as_mut() {
                Some(st) => st.import_eml(&mut s.mailbox, &path, folder).map_err(store_err)?,
                None => {
                    let item = mailcraft_store::import::read_eml(&path, folder).map_err(store_err)?;
                    mailcraft_store::import::insert_item(&mut s.mailbox, item).map_err(store_err)?
                }
            };
            Ok(json!({"imported": 1, "ids": [id.0], "folder": folder.0}))
        }
        "importMsg" => {
            let folder = mail_folder(s, p)?;
            let data = crate::cmd::rules::read_file_capped(&path, MAX_IMPORT_FILE)?;
            let item = mailcraft_formats::msg::read_item(&data, folder, now).map_err(format_err)?;
            let id = mailcraft_store::import::insert_item(&mut s.mailbox, item).map_err(store_err)?;
            Ok(json!({"imported": 1, "ids": [id.0], "folder": folder.0}))
        }
        "importMbox" | "importMaildir" => {
            let folder = mail_folder(s, p)?;
            let ids = if action == "importMbox" {
                mailcraft_store::import_mbox(&mut s.mailbox, &path, folder)
            } else {
                mailcraft_store::import_maildir(&mut s.mailbox, &path, folder)
            }
            .map_err(store_err)?;
            Ok(json!({"imported": ids.len(), "folder": folder.0}))
        }
        "importIcs" => {
            let calendar = kind_folder(s, p, FolderKind::Calendar, FolderRole::Calendar)?;
            let data = crate::cmd::rules::read_file_capped(&path, MAX_IMPORT_FILE)?;
            let opts = mailcraft_formats::ics::ImportOptions { local_offset_min: s.tz_offset_min };
            let cal = mailcraft_formats::ics::read(&data, opts).map_err(format_err)?;
            let account = s.mailbox.folder_account(calendar);
            let tasks_folder = account.and_then(|a| s.mailbox.role_folder(a, FolderRole::Tasks));
            let mb = &mut s.mailbox;
            let mut events = 0usize;
            for mut a in cal.appointments() {
                let id = mailcraft_model::ItemId(mb.alloc_id());
                a.id = id;
                a.folder = calendar;
                mb.appointments.insert(id, a);
                events = events.saturating_add(1);
            }
            let mut tasks = 0usize;
            if let Some(tf) = tasks_folder {
                for mut t in cal.tasks {
                    let id = mailcraft_model::ItemId(mb.alloc_id());
                    t.id = id;
                    t.folder = tf;
                    mb.tasks.insert(id, t);
                    tasks = tasks.saturating_add(1);
                }
            }
            Ok(
                json!({"imported": events.saturating_add(tasks), "appointments": events, "tasks": tasks, "folder": calendar.0}),
            )
        }
        "importVcf" | "importCsv" => {
            let contacts_folder = kind_folder(s, p, FolderKind::Contacts, FolderRole::Contacts)?;
            let data = crate::cmd::rules::read_file_capped(&path, MAX_IMPORT_FILE)?;
            let (list, unmapped) = if action == "importVcf" {
                (mailcraft_formats::vcf::read(&data).map_err(format_err)?, Vec::new())
            } else {
                let r = mailcraft_formats::csv::import_contacts(&data).map_err(format_err)?;
                (r.contacts, r.unmapped_columns)
            };
            let mb = &mut s.mailbox;
            let n = list.len();
            for mut c in list {
                let id = mailcraft_model::ItemId(mb.alloc_id());
                c.id = id;
                c.folder = contacts_folder;
                mb.contacts.insert(id, c);
            }
            Ok(json!({"imported": n, "folder": contacts_folder.0, "unmappedColumns": unmapped}))
        }
        "exportMbox" => {
            let folder = mail_folder(s, p)?;
            let mut items: Vec<&MailItem> = s.mailbox.items_in(folder);
            items.sort_by_key(|m| (m.received.unix, m.id));
            let n = items.len();
            match s.store.as_ref() {
                Some(st) => st.export_mbox(&items, &path).map_err(store_err)?,
                None => mailcraft_store::export_mbox(&items, &path).map_err(store_err)?,
            }
            Ok(json!({"exported": n, "path": path.display().to_string()}))
        }
        "exportIcs" => {
            let folder = match crate::cmd::util::opt_folder(p).and_then(|f| s.mailbox.folder(f)) {
                Some(f) if f.kind == FolderKind::Tasks => f.id,
                _ => kind_folder(s, p, FolderKind::Calendar, FolderRole::Calendar)?,
            };
            let mut appts: Vec<&mailcraft_model::Appointment> =
                s.mailbox.appointments.values().filter(|a| a.folder == folder).collect();
            appts.sort_by_key(|a| (a.start.unix, a.id));
            let tasks: Vec<&mailcraft_model::Task> =
                s.mailbox.tasks.values().filter(|t| t.folder == folder).collect();
            let text = mailcraft_formats::ics::write(&appts, &tasks, None, now);
            write_file(&path, text.as_bytes())?;
            Ok(
                json!({"exported": appts.len().saturating_add(tasks.len()), "path": path.display().to_string()}),
            )
        }
        "exportVcf" | "exportCsv" => {
            let folder = kind_folder(s, p, FolderKind::Contacts, FolderRole::Contacts)?;
            let mut contacts: Vec<mailcraft_model::Contact> =
                s.mailbox.contacts.values().filter(|c| c.folder == folder).cloned().collect();
            contacts.sort_by(|a, b| a.file_as.cmp(&b.file_as).then(a.id.cmp(&b.id)));
            let text = if action == "exportVcf" {
                mailcraft_formats::vcf::write(&contacts, mailcraft_formats::vcf::Version::V4)
            } else {
                mailcraft_formats::csv::export_contacts(&contacts)
            };
            write_file(&path, text.as_bytes())?;
            Ok(json!({"exported": contacts.len(), "path": path.display().to_string()}))
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

/// Save As file types: (id, extension).
const SAVE_TYPES: [&str; 4] = ["eml", "msg", "txt", "html"];

fn escape_html(t: &str) -> String {
    t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Header lines shown above the body in text and HTML saves: (label, value).
fn save_headers(m: &MailItem) -> Vec<(&'static str, String)> {
    let list = |v: &[mailcraft_model::Address]| {
        v.iter().map(|a| a.to_display_string()).collect::<Vec<_>>().join("; ")
    };
    let mut h = vec![
        ("From", m.from.as_ref().map(|a| a.to_display_string()).unwrap_or_default()),
        ("Sent", m.sent.unwrap_or(m.received).to_rfc5322()),
        ("To", list(&m.to)),
    ];
    if !m.cc.is_empty() {
        h.push(("Cc", list(&m.cc)));
    }
    h.push(("Subject", m.subject.clone()));
    h
}

fn save_bytes(s: &Session, m: &MailItem, kind: &str) -> Result<Vec<u8>, CmdError> {
    Ok(match kind {
        "msg" => {
            let mut item = m.clone();
            if item.raw.is_none() {
                item.raw = Some(raw_source(s, m));
            }
            mailcraft_formats::msg::write_item(&item).map_err(format_err)?
        }
        "txt" => {
            let mut out = String::new();
            for (k, v) in save_headers(m) {
                out.push_str(&format!("{k}:\t{v}\r\n"));
            }
            out.push_str("\r\n");
            out.push_str(&m.body_text().replace("\r\n", "\n").replace('\n', "\r\n"));
            out.into_bytes()
        }
        "html" => {
            let body = match &m.body.html {
                Some(h) => mailcraft_html::doc_to_html(&mailcraft_html::html_to_doc(h)),
                None => mailcraft_html::text_to_html(&m.body_text()),
            };
            let mut out = format!(
                "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>{}</title></head><body>\n<table>\n",
                escape_html(&m.subject)
            );
            for (k, v) in save_headers(m) {
                out.push_str(&format!("<tr><th align=\"left\">{k}:</th><td>{}</td></tr>\n", escape_html(&v)));
            }
            out.push_str("</table>\n<hr>\n");
            out.push_str(&body);
            out.push_str("\n</body></html>\n");
            out.into_bytes()
        }
        _ => raw_source(s, m),
    })
}

fn save_as(s: &mut Session, p: &Value) -> CmdResult {
    let id = focused_item(s, p).ok_or_else(|| CmdError::Disabled("no message selected".into()))?;
    let Some(m) = s.mailbox.mail.get(&id) else {
        return Err(CmdError::Disabled("no message selected".into()));
    };
    let wanted = opt_str(p, "type").map(|t| t.trim().trim_start_matches('.').to_ascii_lowercase());
    if let Some(t) = &wanted
        && !SAVE_TYPES.contains(&(if t == "htm" { "html" } else { t.as_str() }))
    {
        return Err(CmdError::Params(format!("unknown file type `{t}` (eml, msg, txt or html)")));
    }
    let Some(path) = opt_str(p, "path") else {
        let ext = wanted.as_deref().unwrap_or("eml");
        let name = format!("{}.{ext}", file_stem(&m.subject));
        return dialog(s, "saveAs", json!({"item": id.0, "suggested": name, "types": SAVE_TYPES}));
    };
    let mut path = PathBuf::from(path);
    let from_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|e| SAVE_TYPES.contains(&e.as_str()) || e == "htm");
    let kind = match (wanted, from_ext, path.is_dir()) {
        (Some(t), _, _) => t,
        (None, Some(e), false) => e,
        _ => "eml".to_owned(),
    };
    let kind = if kind == "htm" { "html".to_owned() } else { kind };
    if path.is_dir() {
        path = unique_path(&path, &format!("{}.{kind}", file_stem(&m.subject)));
    }
    let data = save_bytes(s, m, &kind)?;
    write_file(&path, &data)?;
    Ok(json!({"path": path.display().to_string(), "bytes": data.len(), "type": kind}))
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
    if let Some(done) = crate::cmd::print::forward(s, p, &ids) {
        return done;
    }
    dialog(s, "print", json!({"ids": ids}))
}

fn options(s: &mut Session, p: &Value) -> CmdResult {
    let page = opt_str(p, "page").unwrap_or("general");
    s.run("options.open", &json!({"page": page}))
}

fn account_settings(s: &mut Session, p: &Value) -> CmdResult {
    let id = find_account(s, p, "account")?;
    let acct = s.mailbox.account(id).map(|a| account_json(s, a)).unwrap_or(Value::Null);
    // Server settings for the Change Account page (never the password, only whether one is stored).
    let settings =
        s.mailbox.account(id).map(|a| crate::accounts::account_settings_json(s, a)).unwrap_or(Value::Null);
    dialog(s, "accountSettings", json!({"account": acct, "settings": settings}))
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
    crate::sync::forget_account(s, id);
    let folders = crate::accounts::remove_account_data(s, id);
    Ok(json!({"removed": id.0, "folders": folders}))
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
    fn contacts_and_calendar_round_trip_through_files() {
        let (mut s, _, _) = session();
        let acct = s.mailbox.accounts[0].id;
        let contacts = s.mailbox.role_folder(acct, FolderRole::Contacts).unwrap();
        let calendar = s.mailbox.role_folder(acct, FolderRole::Calendar).unwrap();
        let cid = mailcraft_model::ItemId(s.mailbox.alloc_id());
        s.mailbox.contacts.insert(
            cid,
            mailcraft_model::Contact {
                id: cid,
                folder: contacts,
                first_name: "Ann".into(),
                last_name: "Lee".into(),
                file_as: "Lee, Ann".into(),
                emails: vec!["ann@example.com".into()],
                ..Default::default()
            },
        );
        let aid = mailcraft_model::ItemId(s.mailbox.alloc_id());
        let start = mailcraft_model::DateTime::from_unix(1_791_100_800, 0);
        s.mailbox.appointments.insert(
            aid,
            mailcraft_model::Appointment {
                id: aid,
                folder: calendar,
                uid: "a1@example.com".into(),
                subject: "Planning".into(),
                start,
                end: mailcraft_model::DateTime::from_unix(start.unix + 3600, 0),
                ..Default::default()
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n).to_str().unwrap().to_owned();
        for (export, file) in [("exportVcf", "c.vcf"), ("exportCsv", "c.csv"), ("exportIcs", "cal.ics")] {
            let r = s.run("file.openExport", &json!({"action": export, "path": p(file)})).unwrap();
            assert_eq!(r["exported"], 1, "{export}");
        }
        for file in ["c.vcf", "c.csv"] {
            let r = s.run("file.openExport", &json!({"action": "import", "path": p(file)})).unwrap();
            assert_eq!(r["imported"], 1, "{file}");
        }
        assert_eq!(s.mailbox.contacts.values().filter(|c| c.emails == ["ann@example.com"]).count(), 3);
        let r = s.run("file.openExport", &json!({"action": "import", "path": p("cal.ics")})).unwrap();
        assert_eq!(r["appointments"], 1);
        let copies: Vec<_> = s.mailbox.appointments.values().filter(|a| a.subject == "Planning").collect();
        assert_eq!(copies.len(), 2);
        assert!(copies.iter().all(|a| a.start.unix == start.unix && a.folder == calendar));
        assert!(matches!(
            s.run(
                "file.openExport",
                &json!({"action": "importVcf", "path": p("c.vcf"), "folder": calendar.0})
            ),
            Err(CmdError::Params(_))
        ));
        assert!(s.mailbox.validate().is_empty());
    }

    #[test]
    fn save_as_text_html_and_msg() {
        let (mut s, inbox, _) = session();
        let dir = tempfile::tempdir().unwrap();
        let eml = dir.path().join("in.eml");
        std::fs::write(&eml, EML).unwrap();
        let r = s
            .run(
                "file.openExport",
                &json!({"action": "import", "path": eml.to_str().unwrap(), "folder": inbox.0}),
            )
            .unwrap();
        let id = ItemId(r["ids"][0].as_u64().unwrap());
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        let txt = dir.path().join("m.txt");
        let r = s.run("file.saveAs", &json!({"path": txt.to_str().unwrap()})).unwrap();
        assert_eq!(r["type"], "txt");
        let text = std::fs::read_to_string(&txt).unwrap();
        assert!(
            text.starts_with("From:\tAnn <ann@example.com>\r\n") && text.contains("Subject:\tReport: Q3/Q4")
        );
        assert!(text.contains("See attached."));
        let html = dir.path().join("m.htm");
        s.run("file.saveAs", &json!({"path": html.to_str().unwrap()})).unwrap();
        let page = std::fs::read_to_string(&html).unwrap();
        assert!(page.contains("<title>Report: Q3/Q4</title>") && page.contains("ann@example.com"));
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let r = s.run("file.saveAs", &json!({"path": out.to_str().unwrap(), "type": "msg"})).unwrap();
        let msg = PathBuf::from(r["path"].as_str().unwrap());
        assert_eq!(msg.extension().unwrap(), "msg");
        let r = s
            .run(
                "file.openExport",
                &json!({"action": "import", "path": msg.to_str().unwrap(), "folder": inbox.0}),
            )
            .unwrap();
        let back = ItemId(r["ids"][0].as_u64().unwrap());
        assert_eq!(s.mailbox.mail[&back].subject, "Report: Q3/Q4");
        assert!(matches!(
            s.run("file.saveAs", &json!({"path": out.to_str().unwrap(), "type": "pdf"})),
            Err(CmdError::Params(_))
        ));
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
