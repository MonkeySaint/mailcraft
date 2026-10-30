//! `file.*`: the File backstage — Info (account summary), Account Settings (`accounts.*`, see
//! `crate::accounts`), Open & Export (import/export through `mailcraft-store` and `mailcraft-formats`: EML,
//! MSG, MBOX, Maildir, iCalendar, vCard, CSV), Save As (.eml, .msg, .txt, .html), Save Attachments, data files
//! (new / close / reveal), Options, Exit.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::cmd::rules::{dialog, focused_item, need_selection, write_file};
use crate::cmd::util::{opt_bool, opt_str, req_str};
use crate::{CmdError, CmdResult, CommandSpec, Notice, Session, always};
use mailcraft_model::mailbox::LOCAL_ACCOUNT_FALLBACK_NAME;
use mailcraft_model::{Account, AccountId, AccountKind, FolderId, FolderKind, FolderRole, ItemId, MailItem};

/// Most attachments written by one Save Attachments run.
const MAX_SAVED_ATTACHMENTS: usize = 10_000;
/// Most data files (accounts) in one profile.
pub(crate) const MAX_ACCOUNTS: usize = 256;
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
            r#"{"action"?: "import" (by file type)|"importEml"|"importMsg"|"importMbox"|"importMaildir"|"importIcs"|"importVcf"|"importCsv"|"importPst"|"exportMbox"|"exportIcs"|"exportVcf"|"exportCsv", "path"?: string, "folder"?: number, "into"?: "account" (importPst, default: the PST file's folders merge into the folder's account)|"beneath" (beneath the folder), "duplicates"?: "skip" (default)|"allow", "wait"?: bool (importPst: return when it is done, with the report; else it runs in the background, see file.importProgress), "timeoutSecs"?: number} (no action = open the wizard)"#,
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
        spec(
            "file.automaticReplies",
            "Automatic Replies",
            "File › Info",
            r#"{"account"?: number|string, "state"?: "off"|"on"|"scheduled", "start"?: "YYYY-MM-DDTHH:MM", "end"?: "YYYY-MM-DDTHH:MM", "insideReply"?: string, "outsideReply"?: string, "outside"?: "none"|"contacts"|"everyone", "dialog"?: bool} (Exchange accounts; no change = read them and open the dialog unless dialog is false)"#,
            false,
            has_exchange,
            automatic_replies,
        ),
        spec("file.cleanupTools", "Tools", "File › Info", "", false, always, cleanup_tools),
        spec(
            "file.newDataFile",
            "New Data File",
            "Home › New Items › More Items",
            r#"{"path"?: string (a file of its own; .mcdata added without an extension; an existing data file there opens), "password"?: string (with path: encrypt the file), "name"?: string, "confirm"?: bool} (without a name or path the Create Data File prompt opens; confirm: true creates one kept with the profile)"#,
            true,
            always,
            new_data_file,
        ),
        spec(
            "file.closeDataFile",
            "Remove",
            ROOT,
            r#"{"store": string|number (account id, name or address), "confirm"?: bool (without it the command only asks), "password"?: string (a protected data file whose password is not kept; checked against its file), "verify"?: string (the password again, needed when its file is gone)}"#,
            true,
            has_secondary,
            close_data_file,
        ),
        spec(
            "file.openDataFile",
            "Open Data File",
            "File › Open & Export",
            r#"{"path"?: string (a .mcdata file; one outside this profile's closed data files stays linked to it; a PST file (.pst) is imported into a new data file named after it), "password"?: string (a protected file), "name"?: string (a closed data file of this profile), "wait"?: bool (a PST file: return when it is imported)} (neither = list the closed data files)"#,
            true,
            always,
            open_data_file,
        ),
        spec(
            "file.dataFileProperties",
            "Data File Properties...",
            ROOT,
            r#"{"store"?: string|number, "name"?: string (rename), "compact"?: bool (compact the profile's data now), "save"?: bool (write a data file of its own to its file now)} (none of them = open the dialog)"#,
            true,
            has_accounts,
            data_file_properties,
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
        spec(
            "file.importProgress",
            "Import Progress",
            "File › Open & Export",
            r#"{"cancel"?: bool (stop the PST import that runs; what came in so far stays), "show"?: bool (open the progress window, also after an import ended: its report)}"#,
            false,
            always,
            import_progress,
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
        .any(|id| s.mailbox.mail.get(*id).is_some_and(MailItem::has_attachments));
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
        AccountKind::Graph => "graph",
    }
}

fn account_json(s: &Session, a: &Account) -> Value {
    let folders: Vec<FolderId> =
        s.mailbox.folders.values().filter(|f| f.account == a.id).map(|f| f.id).collect();
    let items = s.mailbox.mail.items().filter(|m| folders.contains(&m.folder));
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
    if path.is_file() && crate::pst::is_pst(path) {
        return Ok("importPst");
    }
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
            let id = mailcraft_store::import::import_eml(&mut s.mailbox, &mut s.bodies, &path, folder)
                .map_err(store_err)?;
            Ok(json!({"imported": 1, "ids": [id.0], "folder": folder.0}))
        }
        "importMsg" => {
            let folder = mail_folder(s, p)?;
            let data = crate::cmd::rules::read_file_capped(&path, MAX_IMPORT_FILE)?;
            let (mut item, content) =
                mailcraft_formats::msg::read_item(&data, folder, now).map_err(format_err)?;
            s.store_content(&mut item, &content)?;
            let id = mailcraft_store::import::insert_item(&mut s.mailbox, item).map_err(store_err)?;
            Ok(json!({"imported": 1, "ids": [id.0], "folder": folder.0}))
        }
        "importMbox" | "importMaildir" => {
            let folder = mail_folder(s, p)?;
            let ids = if action == "importMbox" {
                mailcraft_store::import_mbox(&mut s.mailbox, &mut s.bodies, &path, folder)
            } else {
                mailcraft_store::import_maildir(&mut s.mailbox, &mut s.bodies, &path, folder)
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
        "importPst" => {
            let folder = crate::cmd::util::opt_folder(p).unwrap_or(s.selection.folder);
            let Some(f) = s.mailbox.folder(folder) else {
                return Err(CmdError::Params(format!("no such folder {}", folder.0)));
            };
            let target = match opt_str(p, "into").unwrap_or("account") {
                "account" => crate::pst::Target::Account(f.account),
                "beneath" => {
                    if matches!(f.kind, FolderKind::Search | FolderKind::SearchRoot) {
                        return Err(CmdError::Params(
                            "a PST file cannot be imported beneath search folders".into(),
                        ));
                    }
                    crate::pst::Target::Beneath(folder)
                }
                other => return Err(CmdError::Params(format!("unknown `into` value `{other}`"))),
            };
            let skip = match opt_str(p, "duplicates").unwrap_or("skip") {
                "skip" => true,
                "allow" => false,
                other => return Err(CmdError::Params(format!("unknown `duplicates` value `{other}`"))),
            };
            crate::pst::start(s, &path, target, false, skip, pst_wait(p))
        }
        "exportMbox" => {
            let folder = mail_folder(s, p)?;
            let ids: Vec<ItemId> = s.mailbox.items_in(folder).iter().map(|m| m.id).collect();
            crate::sync::cmds::need_content(s, &ids, "exported")?;
            let mut items: Vec<&MailItem> = s.mailbox.items_in(folder);
            items.sort_by_key(|m| (m.received.unix, m.id));
            let n = items.len();
            mailcraft_store::export_mbox(&items, &s.bodies, &path).map_err(store_err)?;
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

/// Raw RFC 5322 source of an item: its stored source, else rebuilt from its fields and body.
pub(crate) fn raw_source(s: &Session, m: &MailItem) -> Vec<u8> {
    if let Some(raw) = crate::cmd::compose::stored_raw(s, m) {
        return raw;
    }
    mailcraft_store::rebuild_message(m, Some(&s.body_or_empty(m)))
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
            let raw = raw_source(s, m);
            let body = s.body_or_empty(m);
            mailcraft_formats::msg::write_item(m, Some(&raw), Some(&body)).map_err(format_err)?
        }
        "txt" => {
            let mut out = String::new();
            for (k, v) in save_headers(m) {
                out.push_str(&format!("{k}:\t{v}\r\n"));
            }
            out.push_str("\r\n");
            out.push_str(&s.body_text(m).replace("\r\n", "\n").replace('\n', "\r\n"));
            out.into_bytes()
        }
        "html" => {
            let content = s.body_or_empty(m);
            let body = match &content.html {
                Some(h) => mailcraft_html::doc_to_html(&mailcraft_html::html_to_doc(h)),
                None => mailcraft_html::text_to_html(&content.plain_text()),
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
    let Some(m) = s.mailbox.mail.get(id) else {
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
    crate::sync::cmds::need_content(s, &[id], "saved")?;
    let Some(m) = s.mailbox.mail.get(id) else {
        return Err(CmdError::Disabled("no message selected".into()));
    };
    let data = save_bytes(s, m, &kind)?;
    write_file(&path, &data)?;
    Ok(json!({"path": path.display().to_string(), "bytes": data.len(), "type": kind}))
}

fn save_attachments(s: &mut Session, p: &Value) -> CmdResult {
    let ids: Vec<ItemId> = s
        .target_ids(p)
        .into_iter()
        .filter(|id| s.mailbox.mail.get(*id).is_some_and(MailItem::has_attachments))
        .collect();
    if ids.is_empty() {
        return Err(CmdError::Disabled("no attachments in the selection".into()));
    }
    let Some(dir) = opt_str(p, "dir") else {
        // S/MIME messages offer the attachments of the opened message.
        let files: Vec<Value> = ids
            .iter()
            .filter_map(|id| crate::smime::display_item(s, *id))
            .flat_map(|m| {
                m.attachments
                    .iter()
                    .filter(|a| !a.inline)
                    .map(|a| json!({"item": m.id.0, "filename": a.filename, "size": a.size}))
                    .collect::<Vec<_>>()
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
        let Some(m) = s.mailbox.mail.get(*id) else { continue };
        if m.blob.is_none() {
            missing = missing.saturating_add(1);
            continue;
        }
        let raw = crate::smime::open_source(s, m);
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

/// Automatic replies live on an Exchange server.
fn has_exchange(s: &Session) -> Option<&'static str> {
    if s.mailbox.accounts.iter().any(|a| a.kind == AccountKind::Exchange && a.incoming.is_some()) {
        None
    } else {
        Some("Automatic replies need an Exchange account")
    }
}

fn automatic_replies(s: &mut Session, p: &Value) -> CmdResult {
    crate::sync::automatic_replies(s, p)
}

fn manage_rules(s: &mut Session, p: &Value) -> CmdResult {
    s.run("rules.manage", p)
}

fn cleanup_tools(s: &mut Session, _: &Value) -> CmdResult {
    dialog(s, "mailboxCleanup", json!({}))
}

/// `base`, numbered when another data file already has that name ("Archive (2)").
fn free_name(s: &Session, base: &str) -> String {
    let taken = |n: &str| s.mailbox.accounts.iter().any(|a| a.display_name.eq_ignore_ascii_case(n));
    let mut name = base.to_owned();
    let mut k = 2u32;
    while taken(&name) && k < 10_000 {
        name = format!("{base} ({k})");
        k = k.saturating_add(1);
    }
    name
}

/// Opens the data file read from `path` as a standalone data file (see `datafile::open_linked`).
fn open_linked_cmd(s: &mut Session, path: &Path, bytes: Vec<u8>, password: Option<&str>) -> CmdResult {
    let opened = crate::datafile::open_linked(s, path, bytes, password)?;
    let id = opened.account;
    // The data file is now open from its file: an undo step would drop it while the file waits elsewhere.
    s.undo_barrier = true;
    let name = s.mailbox.account(id).map(|a| a.display_name.clone()).unwrap_or_default();
    let (_, items, folders) = crate::datafile::usage(s, id);
    let protected = s.mailbox.account(id).and_then(|a| a.data_file.as_ref()).is_some_and(|l| l.protected);
    let backup = opened.backup.as_ref().map(|b| b.display().to_string());
    if opened.repairs.any() {
        let kept =
            backup.as_deref().map(|b| format!(" The file as it was is kept in {b}.")).unwrap_or_default();
        s.notify(Notice::Info(format!(
            "The data file \"{name}\" needed repairs when it opened: {}.{kept}",
            opened.repairs.describe()
        )));
    }
    Ok(json!({
        "account": id.0,
        "name": name,
        "items": items,
        "folders": folders,
        "path": path.display().to_string(),
        "protected": protected,
        "repaired": opened.repairs.any().then_some(opened.repairs),
        "backup": backup,
    }))
}

/// The data file password of a command (`password`; empty means none).
fn password_param(p: &Value) -> Result<Option<String>, CmdError> {
    match p.get("password") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(pw)) if pw.is_empty() => Ok(None),
        Some(Value::String(pw)) => {
            crate::datafile::check_password(pw)?;
            Ok(Some(pw.clone()))
        }
        Some(_) => Err(CmdError::Params("`password` must be text".into())),
    }
}

/// New Data File. With `path` the data file is a file of its own at that path (`.mcdata` is added when the
/// name has no extension), written at once and encrypted under `password` when one is given; a data file
/// that is already there is opened instead (Create or Open). Without a path it is kept with the profile's
/// data. Without a name or path the Create Data File prompt opens.
fn new_data_file(s: &mut Session, p: &Value) -> CmdResult {
    if s.mailbox.accounts.len() >= MAX_ACCOUNTS {
        return Err(CmdError::Failed(format!("at most {MAX_ACCOUNTS} data files")));
    }
    let password = password_param(p)?;
    if let Some(raw) = opt_str(p, "path").map(str::trim).filter(|x| !x.is_empty()) {
        if raw.chars().count() > 4096 || raw.chars().any(char::is_control) {
            return Err(CmdError::Params("`path` is not a usable file name".into()));
        }
        // Kept absolute: the file is written again on close and exit, whatever the current folder is then.
        let path = crate::datafile::absolute(&crate::datafile::with_extension(Path::new(raw)));
        if Path::new(raw).is_dir() || path.is_dir() {
            return Err(CmdError::Params(format!("{} is a folder; give a file name", path.display())));
        }
        if path.exists() {
            // Create or Open: a data file already at that path opens.
            let bytes = crate::datafile::read_file(&path)?;
            if crate::datafile::is_sealed(&bytes) && password.is_none() {
                return dialog(
                    s,
                    "dataFilePassword",
                    json!({"path": path.display().to_string(), "command": "file.openDataFile"}),
                );
            }
            return open_linked_cmd(s, &path, bytes, password.as_deref());
        }
        let clean = |n: &str| -> String { n.chars().filter(|c| !c.is_control()).take(255).collect() };
        let stem = path.file_stem().and_then(|n| n.to_str()).map(clean).unwrap_or_default();
        let base = opt_str(p, "name")
            .map(clean)
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .or_else(|| Some(stem.trim().to_owned()).filter(|n| !n.is_empty()))
            .unwrap_or_else(|| LOCAL_ACCOUNT_FALLBACK_NAME.to_owned());
        let name = free_name(s, &base);
        let id = crate::datafile::create_linked(s, &name, &path, password.as_deref())?;
        // The file now exists outside the mailbox: an undo step would leave it behind unopened.
        s.undo_barrier = true;
        return Ok(json!({
            "account": id.0,
            "name": name,
            "path": path.display().to_string(),
            "protected": password.is_some(),
        }));
    }
    if password.is_some() {
        return Err(CmdError::Params(
            "a password protects a data file kept in a file of its own: give a `path`".into(),
        ));
    }
    let clean = |n: &str| -> String { n.chars().filter(|c| !c.is_control()).take(255).collect() };
    let base = opt_str(p, "name")
        .map(|n| clean(n).trim().to_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| LOCAL_ACCOUNT_FALLBACK_NAME.to_owned());
    let name = free_name(s, &base);
    // Without a name or a file, ask for one first (Create or Open Data File); `confirm` skips the prompt.
    let named = opt_str(p, "name").is_some();
    if !named && p.get("confirm").and_then(Value::as_bool) != Some(true) {
        let location = s.store.as_ref().map(|st| st.dir.display().to_string()).unwrap_or_default();
        return dialog(
            s,
            "newDataFile",
            json!({ "name": name, "location": location, "extension": crate::datafile::EXTENSION }),
        );
    }
    let id = s.mailbox.add_account(Account {
        kind: AccountKind::Local,
        display_name: name.clone(),
        sender_name: String::new(),
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
    if crate::pst::importing_into(s, id) {
        return Err(CmdError::Disabled(
            "a PST file is being imported into this data file; stop the import first".into(),
        ));
    }
    let is_primary = s.mailbox.account(id).is_some_and(|a| a.is_default);
    if is_primary {
        return Err(CmdError::Disabled("the primary data file cannot be removed".into()));
    }
    let name = s.mailbox.account(id).map(|a| a.display_name.clone()).unwrap_or_default();
    let link = s.mailbox.account(id).and_then(|a| a.data_file.clone());
    if let Some(link) = link {
        // A data file of its own goes back to its file.
        if opt_bool(p, "confirm") != Some(true) {
            return dialog(
                s,
                "confirm",
                json!({
                    "title": "Remove Data File",
                    "text": format!("Close the data file \"{name}\"? Its folders leave the folder pane, and it is saved to {} so it can be opened again (or moved) later.", link.path),
                    "command": "file.closeDataFile",
                    "params": {"store": id.0, "confirm": true},
                }),
            );
        }
        if link.protected && s.secrets.get(&crate::datafile::secret_key(id)).is_none() {
            // The password is checked against the file before anything is written with it: a typo must not
            // leave the only copy encrypted under a password nobody knows.
            let existing = Path::new(&link.path);
            let sealed = existing
                .is_file()
                .then(|| crate::datafile::read_file(existing))
                .transpose()?
                .filter(|b| crate::datafile::is_sealed(b));
            match password_param(p)? {
                Some(pw) => {
                    match &sealed {
                        // Checked without decrypting the whole file.
                        Some(bytes) => crate::datafile::check_sealed_password(bytes, &pw)?,
                        // Nothing to check it against (the file is gone or not protected): it is typed twice.
                        None if opt_str(p, "verify") != Some(pw.as_str()) => {
                            return Err(CmdError::Params(
                                "the file is not there to check the password against: type it twice (`verify`)"
                                    .into(),
                            ));
                        }
                        None => {}
                    }
                    s.secrets.set(&crate::datafile::secret_key(id), &pw).map_err(CmdError::Failed)?;
                }
                None => {
                    return dialog(
                        s,
                        "dataFilePassword",
                        json!({
                            "path": link.path,
                            "store": id.0,
                            "command": "file.closeDataFile",
                            "verify": sealed.is_none(),
                        }),
                    );
                }
            }
        }
        let own_folders: std::collections::BTreeSet<FolderId> =
            s.mailbox.folders.values().filter(|f| f.account == id).map(|f| f.id).collect();
        let (path, folders) = crate::datafile::close_linked(s, id)?;
        s.undo_barrier = true;
        if link.protected {
            // A protected data file's contents leave the profile with it: its deleted items leave the
            // recoverable-items bin, its items leave the journal and snapshot (folded into a fresh one), and
            // then the message sources only it used leave the profile's blobs.
            crate::recover::forget_account(s, id, &own_folders);
            s.checkpoint_after = true;
            s.purge_blobs_after = true;
        }
        return Ok(
            json!({"removed": id.0, "folders": folders, "name": name, "path": path.display().to_string()}),
        );
    }
    if opt_bool(p, "confirm") != Some(true) {
        let kept = match crate::datafile::closed_dir(s) {
            Some(dir) => format!("It is kept in {} and can be opened again", dir.display()),
            None => "It is kept until MailCraft closes and can be opened again".to_owned(),
        };
        return dialog(
            s,
            "confirm",
            json!({
                "title": "Remove Data File",
                "text": format!("Close the data file \"{name}\"? Its folders leave the folder pane. {kept} from File › Open & Export › Open Data File."),
                "command": "file.closeDataFile",
                "params": {"store": id.0, "confirm": true},
            }),
        );
    }
    let (path, folders) = crate::datafile::detach(s, id)?;
    // The closed data file now holds the data: an undo step would bring the data file back while the
    // closed copy stays, so opening that later would add it twice.
    s.undo_barrier = true;
    let path = path.map(|p| p.display().to_string());
    Ok(json!({"removed": id.0, "folders": folders, "name": name, "path": path}))
}

/// How long a PST import may take when the caller waits for it (`wait`, `timeoutSecs`); `None` = in the
/// background.
fn pst_wait(p: &Value) -> Option<std::time::Duration> {
    (opt_bool(p, "wait") == Some(true)).then(|| {
        let secs = p.get("timeoutSecs").and_then(Value::as_u64).unwrap_or(24 * 3600).clamp(1, 7 * 24 * 3600);
        std::time::Duration::from_secs(secs)
    })
}

/// Open Data File with a PST file: a new data file named after the file, filled by an import.
fn open_pst(s: &mut Session, path: &Path, p: &Value) -> CmdResult {
    if s.mailbox.accounts.len() >= MAX_ACCOUNTS {
        return Err(CmdError::Failed(format!("at most {MAX_ACCOUNTS} data files")));
    }
    if crate::pst::running(s) {
        return Err(CmdError::Disabled(
            "a PST file is being imported already; wait for it or stop it".into(),
        ));
    }
    let stem: String = path
        .file_stem()
        .and_then(|n| n.to_str())
        .map(|n| n.chars().filter(|c| !c.is_control()).take(255).collect::<String>())
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "PST file".to_owned());
    let name = free_name(s, &stem);
    let id = s.mailbox.add_account(Account {
        kind: AccountKind::Local,
        display_name: name.clone(),
        sender_name: String::new(),
        is_default: false,
        ..Default::default()
    });
    s.mailbox.create_default_folders(id);
    let mut r = crate::pst::start(s, path, crate::pst::Target::Account(id), true, true, pst_wait(p))?;
    // The data file fills in the background: an undo step would take it away under the import.
    s.undo_barrier = true;
    crate::pst::put(&mut r, "name", json!(name));
    crate::pst::put(&mut r, "account", json!(id.0));
    Ok(r)
}

/// The PST import's progress (or the last one's report); `cancel` stops it, `show` opens the progress window
/// (the status bar and Open & Export reopen it after Hide).
fn import_progress(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "cancel") == Some(true) {
        let stopping = crate::pst::cancel(s);
        let mut v = crate::pst::status(s).unwrap_or_else(|| json!({"running": false}));
        crate::pst::put(&mut v, "stopping", json!(stopping));
        return Ok(v);
    }
    let v = crate::pst::status(s).unwrap_or_else(|| json!({"running": false}));
    if opt_bool(p, "show") == Some(true) {
        let job = v.get("job").cloned().unwrap_or(Value::Null);
        s.notify(Notice::OpenDialog("pstProgress".into(), json!({"job": job})));
    }
    Ok(v)
}

/// Opens a closed data file again: from a path, or by name among this profile's closed data files; with
/// neither, lists them (`openDataFile` dialog). A file from the profile's closed-data-files folder is renamed
/// `<name>.mcdata.opened` once its contents are back, so it is not offered twice.
fn open_data_file(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(path) = opt_str(p, "path").map(Path::new)
        && path.is_file()
        && crate::pst::is_pst(path)
    {
        let path = path.to_path_buf();
        return open_pst(s, &path, p);
    }
    let entries = crate::datafile::closed_entries(s);
    let password = password_param(p)?;
    let (bytes, from_dir, memory) = if let Some(path) = opt_str(p, "path") {
        let path = crate::datafile::absolute(Path::new(path));
        let in_dir = entries.iter().any(|e| e.path.as_deref() == Some(path.as_path()));
        let bytes = crate::datafile::read_file(&path)?;
        if !in_dir {
            // A data file of its own (anywhere but this profile's closed data files) opens from its file and
            // goes back there when it is closed; a protected one asks for its password first.
            if crate::datafile::is_sealed(&bytes) && password.is_none() {
                return dialog(
                    s,
                    "dataFilePassword",
                    json!({"path": path.display().to_string(), "command": "file.openDataFile"}),
                );
            }
            return open_linked_cmd(s, &path, bytes, password.as_deref());
        }
        (bytes, Some(path), None)
    } else if let Some(name) = opt_str(p, "name") {
        let Some(e) = entries.iter().find(|e| e.name.eq_ignore_ascii_case(name.trim())) else {
            return Err(CmdError::Params(format!("no closed data file named \"{name}\"")));
        };
        match &e.path {
            Some(path) => (crate::datafile::read_file(path)?, Some(path.clone()), None),
            None => {
                let i = s.closed_data_files.iter().position(|c| c.name.eq_ignore_ascii_case(&e.name));
                let Some(c) = i.and_then(|i| s.closed_data_files.get(i)) else {
                    return Err(CmdError::Params(format!("no closed data file named \"{name}\"")));
                };
                (c.bytes.clone(), None, i)
            }
        }
    } else {
        let files: Vec<Value> = entries
            .iter()
            .map(|e| json!({"name": e.name, "path": e.path.as_ref().map(|p| p.display().to_string()), "bytes": e.bytes}))
            .collect();
        let dir = crate::datafile::closed_dir(s).map(|d| d.display().to_string());
        return dialog(s, "openDataFile", json!({"files": files, "dir": dir}));
    };
    let image = crate::datafile::parse(&bytes)?;
    drop(bytes);
    let (id, repairs) = crate::datafile::attach_report(s, image)?;
    // The closed data file is consumed below (set aside or dropped): an undo step would take the data file
    // out of the mailbox with nothing left to open it from.
    s.undo_barrier = true;
    if let Some(i) = memory
        && i < s.closed_data_files.len()
    {
        s.closed_data_files.remove(i);
    }
    if let Some(path) = &from_dir {
        let done = path.with_extension(format!("{}.opened", crate::datafile::EXTENSION));
        if let Err(e) = std::fs::rename(path, &done) {
            s.notify(Notice::Info(format!(
                "The data file is open again, but {} could not be renamed: {e}",
                path.display()
            )));
        }
    }
    let name = s.mailbox.account(id).map(|a| a.display_name.clone()).unwrap_or_default();
    let (_, items, folders) = crate::datafile::usage(s, id);
    if repairs.any() {
        let kept = if from_dir.is_some() {
            " The file as it was stays in the closed data files folder (renamed to end in .opened)."
        } else {
            ""
        };
        s.notify(Notice::Info(format!(
            "The data file \"{name}\" needed repairs when it opened: {}.{kept}",
            repairs.describe()
        )));
    }
    Ok(json!({
        "account": id.0,
        "name": name,
        "items": items,
        "folders": folders,
        "repaired": repairs.any().then_some(repairs),
    }))
}

/// Data File Properties: the data file's name (renamed with `name`), where it is kept, its size and counts;
/// `compact` folds the profile's change journal into a fresh snapshot.
fn data_file_properties(s: &mut Session, p: &Value) -> CmdResult {
    let id = find_account(s, p, "store")?;
    if let Some(name) = opt_str(p, "name") {
        let clean: String = name.chars().filter(|c| !c.is_control()).take(255).collect();
        let clean = clean.trim().to_owned();
        if clean.is_empty() {
            return Err(CmdError::Params("a data file needs a name".into()));
        }
        if s.mailbox.accounts.iter().any(|a| a.id != id && a.display_name.eq_ignore_ascii_case(&clean)) {
            return Err(CmdError::Params(format!("another data file is already called \"{clean}\"")));
        }
        if let Some(a) = s.mailbox.accounts.iter_mut().find(|a| a.id == id) {
            a.display_name = clean.clone();
        }
        return Ok(json!({"account": id.0, "name": clean}));
    }
    if opt_bool(p, "compact") == Some(true) {
        let Some(st) = s.store.as_mut() else {
            return Err(CmdError::Disabled("this session has no data file on disk".into()));
        };
        let before = st.journal_bytes();
        let res = st.append(&s.pending_ops).and_then(|()| {
            s.pending_ops.clear();
            st.checkpoint(&s.mailbox)
        });
        res.map_err(|e| CmdError::Failed(format!("could not compact the data file: {e}")))?;
        let after = st.journal_bytes();
        return Ok(
            json!({"account": id.0, "compacted": true, "journalBytes": before, "journalAfter": after}),
        );
    }
    if opt_bool(p, "save") == Some(true) {
        // A data file of its own: write its file now (it is written on close and on exit too).
        let path = crate::datafile::save_linked(s, id)?;
        return Ok(json!({"account": id.0, "saved": path.display().to_string()}));
    }
    let Some(a) = s.mailbox.account(id).cloned() else {
        return Err(CmdError::Params("no such data file".into()));
    };
    let (bytes, items, folders) = crate::datafile::usage(s, id);
    let path = s.store.as_ref().map(|st| st.dir.display().to_string());
    dialog(
        s,
        "dataFileProperties",
        json!({
            "account": id.0,
            "name": a.display_name,
            "address": a.address,
            "local": a.kind == AccountKind::Local,
            "default": a.is_default,
            "path": path,
            "file": a.data_file.as_ref().map(|l| l.path.clone()),
            "protected": a.data_file.as_ref().is_some_and(|l| l.protected),
            "bytes": bytes,
            "items": items,
            "folders": folders,
        }),
    )
}

/// Writes every data file kept in a file of its own back to its file (on exit) when it changed since it was
/// last read or written. Returns the files that could not be written, with why.
fn save_linked_files(s: &mut Session) -> Vec<String> {
    let linked: Vec<AccountId> =
        s.mailbox.accounts.iter().filter(|a| a.data_file.is_some()).map(|a| a.id).collect();
    linked
        .into_iter()
        .filter_map(|id| crate::datafile::save_linked_as(s, id, true).err().map(|e| e.to_string()))
        .collect()
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

/// Writes data files of their own back to their files, flushes pending ops and checkpoints the store, then
/// asks the UI to close the window.
fn exit(s: &mut Session, _: &Value) -> CmdResult {
    // Data files of their own are written back to their files first: one written to a copy (its file was
    // changed elsewhere) is linked to that copy from now on, and the checkpoint below must save that link,
    // or the next start-up would follow the old file again and overwrite it.
    let failed = save_linked_files(s);
    for why in &failed {
        s.notify(Notice::Error(format!("Could not save a data file: {why}")));
    }
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
        assert_eq!(s.mailbox.mail.get(id).unwrap().subject, "Report: Q3/Q4");
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
        let r = s.run("file.newDataFile", &json!({"name": "Archive 2026"})).unwrap();
        assert_eq!(r["name"], "Archive 2026");
        let r2 = s.run("file.newDataFile", &json!({"name": "archive 2026"})).unwrap();
        assert_eq!(r2["name"], "archive 2026 (2)");
        let acct = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s.mailbox.add_mail(MailItem { folder: inbox, ..Default::default() });
        s.selection.folder = inbox;
        let before = s.mailbox.mail.len();
        // Remove asks first and changes nothing until confirmed.
        let r = s.run("file.closeDataFile", &json!({"store": "Archive 2026"})).unwrap();
        assert_eq!(r["dialog"], "confirm");
        assert_eq!(s.mailbox.mail.len(), before);
        let r = s.run("file.closeDataFile", &json!({"store": "Archive 2026", "confirm": true})).unwrap();
        assert_eq!(r["removed"], acct.0);
        assert_eq!(s.mailbox.mail.len(), before - 1);
        assert!(s.mailbox.folders.values().all(|f| f.account != acct));
        assert_ne!(s.selection.folder, inbox);
        assert!(s.mailbox.validate().is_empty());
        assert!(matches!(
            s.run("file.closeDataFile", &json!({"store": "Test", "confirm": true})),
            Err(CmdError::Disabled(_))
        ));
        // The closed data file is kept (in memory here) and opens again with its item.
        assert_eq!(s.closed_data_files.len(), 1);
        let r = s.run("file.openDataFile", &json!({})).unwrap();
        assert_eq!(r["dialog"], "openDataFile");
        let r = s.run("file.openDataFile", &json!({"name": "archive 2026"})).unwrap();
        assert_eq!(r["name"], "Archive 2026");
        assert_eq!(r["items"], 1);
        assert!(s.closed_data_files.is_empty());
        assert_eq!(s.mailbox.mail.len(), before);
        assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
    }

    #[test]
    fn new_data_file_at_a_path_is_a_file_that_closes_reopens_and_moves() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _, _) = session();
        let path = dir.path().join("Project Mail");
        let r = s.run("file.newDataFile", &json!({"path": path.to_str().unwrap()})).unwrap();
        let file = dir.path().join("Project Mail.mcdata");
        assert_eq!(r["name"], "Project Mail");
        assert_eq!(r["path"], file.to_str().unwrap());
        assert!(file.is_file(), "the file is written when the data file is made");
        let acct = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        assert!(s.mailbox.role_folder(acct, FolderRole::Inbox).is_some(), "with its default folders");
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s.mailbox.add_mail(MailItem { folder: inbox, subject: "Kept".into(), ..Default::default() });
        // Properties name the file; Save writes it now.
        s.run("file.dataFileProperties", &json!({"store": acct.0})).unwrap();
        let Some(Notice::OpenDialog(_, d)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(d["file"], file.to_str().unwrap());
        s.run("file.dataFileProperties", &json!({"store": acct.0, "save": true})).unwrap();
        let image = crate::datafile::parse(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(image.mail.len(), 1);
        assert!(image.account.data_file.is_none(), "the file does not say where it was");
        // Close writes it back to its file (not to the profile's closed data files).
        let r = s.run("file.closeDataFile", &json!({"store": acct.0})).unwrap();
        assert_eq!(r["dialog"], "confirm");
        let r = s.run("file.closeDataFile", &json!({"store": acct.0, "confirm": true})).unwrap();
        assert_eq!(r["path"], file.to_str().unwrap());
        assert!(s.closed_data_files.is_empty());
        assert!(s.mailbox.accounts.iter().all(|a| a.id != acct));
        // Moved elsewhere, it opens from there and stays linked to its new place.
        let moved = dir.path().join("moved.mcdata");
        std::fs::rename(&file, &moved).unwrap();
        let r = s.run("file.openDataFile", &json!({"path": moved.to_str().unwrap()})).unwrap();
        assert_eq!((r["name"].as_str(), r["items"].as_u64()), (Some("Project Mail"), Some(1)));
        let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let link = s.mailbox.account(id).and_then(|a| a.data_file.clone()).unwrap();
        assert_eq!(link.path, moved.to_str().unwrap());
        assert!(moved.is_file(), "a linked file is not renamed away");
        // It cannot be open twice; New Data File on an existing data file opens it (here: refused, it is open).
        assert!(s.run("file.openDataFile", &json!({"path": moved.to_str().unwrap()})).is_err());
        assert!(s.run("file.newDataFile", &json!({"path": moved.to_str().unwrap()})).is_err());
        // Exit writes it back too.
        let inbox = s.mailbox.role_folder(id, FolderRole::Inbox).unwrap();
        s.mailbox.add_mail(MailItem { folder: inbox, subject: "Later".into(), ..Default::default() });
        s.run("file.exit", &json!({})).unwrap();
        let image = crate::datafile::parse(&std::fs::read(&moved).unwrap()).unwrap();
        assert_eq!(image.mail.len(), 2);
        // A folder is not a file name; junk is refused.
        assert!(s.run("file.newDataFile", &json!({"path": dir.path().to_str().unwrap()})).is_err());
        assert!(s.run("file.newDataFile", &json!({"path": "a\u{0}b"})).is_err());
        let missing = dir.path().join("no-such-folder").join("x.mcdata");
        let before = s.mailbox.accounts.len();
        assert!(s.run("file.newDataFile", &json!({"path": missing.to_str().unwrap()})).is_err());
        assert_eq!(s.mailbox.accounts.len(), before, "nothing is added when the file cannot be written");
        assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
    }

    #[test]
    fn data_file_with_a_password_is_encrypted_and_asks_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _, _) = session();
        let file = dir.path().join("Private.mcdata");
        let p = file.to_str().unwrap();
        assert!(s.run("file.newDataFile", &json!({"name": "x", "password": "pw"})).is_err(), "needs a path");
        assert!(s.run("file.newDataFile", &json!({"path": p, "password": "a\u{7}b"})).is_err());
        let r = s.run("file.newDataFile", &json!({"path": p, "password": "s3cret"})).unwrap();
        assert_eq!(r["protected"], true);
        let acct = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s.mailbox.add_mail(MailItem {
            folder: inbox,
            subject: "Confidential plans".into(),
            ..Default::default()
        });
        s.run("file.closeDataFile", &json!({"store": acct.0, "confirm": true})).unwrap();
        let bytes = std::fs::read(&file).unwrap();
        assert!(crate::datafile::is_sealed(&bytes));
        assert!(!String::from_utf8_lossy(&bytes).contains("Confidential"), "the file is encrypted");
        assert!(crate::datafile::parse(&bytes).is_err());
        // Opening asks for the password; a wrong one is refused; the right one opens it.
        let r = s.run("file.openDataFile", &json!({"path": p})).unwrap();
        assert_eq!(r["dialog"], "dataFilePassword");
        let before = s.mailbox.accounts.len();
        assert!(matches!(
            s.run("file.openDataFile", &json!({"path": p, "password": "nope"})),
            Err(CmdError::Params(_))
        ));
        assert_eq!(s.mailbox.accounts.len(), before);
        let r = s.run("file.openDataFile", &json!({"path": p, "password": "s3cret"})).unwrap();
        assert_eq!(r["items"], 1);
        assert_eq!(r["protected"], true);
        let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        // Without the kept password, Close asks for it before writing.
        s.secrets.remove(&crate::datafile::secret_key(id)).unwrap();
        let r = s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true})).unwrap();
        assert_eq!(r["dialog"], "dataFilePassword");
        assert!(s.mailbox.account(id).is_some(), "nothing closed yet");
        assert_eq!(r["params"]["verify"], false, "the file is there to check the password against");
        // A mistyped password is checked against the file and refused: nothing is written or kept.
        assert!(matches!(
            s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true, "password": "s3cre"})),
            Err(CmdError::Params(_))
        ));
        assert!(s.secrets.get(&crate::datafile::secret_key(id)).is_none(), "a wrong password is not kept");
        assert!(s.mailbox.account(id).is_some());
        assert!(crate::datafile::unseal(&std::fs::read(&file).unwrap(), "s3cret").is_ok(), "file untouched");
        s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true, "password": "s3cret"})).unwrap();
        assert!(s.mailbox.account(id).is_none());
        let bytes = std::fs::read(&file).unwrap();
        assert!(crate::datafile::unseal(&bytes, "s3cret").is_ok());
        // Damaged protected files are an error, never a panic.
        let junk = br#"{"format":"mailcraft-data-file-sealed","version":1,"kdf":"pbkdf2-sha256","rounds":5,"salt":"!!","cipher":"aes-256-cbc","iv":"","data":""}"#;
        assert!(crate::datafile::unseal(junk, "x").is_err());
        assert!(crate::datafile::unseal(b"{", "x").is_err());
    }

    /// A relative path is kept absolute, so the file is written back to the same place from any folder.
    #[cfg(unix)]
    #[test]
    fn a_relative_data_file_path_is_kept_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        // The temporary folder spelled relative to the current folder ("../../…").
        let mut rel = PathBuf::new();
        for _ in cwd.components().skip(1) {
            rel.push("..");
        }
        rel.push(dir.path().strip_prefix("/").unwrap());
        rel.push("Relative");
        assert!(rel.is_relative());
        let (mut s, _, _) = session();
        let r = s.run("file.newDataFile", &json!({"path": rel.to_str().unwrap()})).unwrap();
        let want = dir.path().join("Relative.mcdata");
        assert!(want.is_file());
        assert!(Path::new(r["path"].as_str().unwrap()).is_absolute(), "{}", r["path"]);
        let link = s.mailbox.accounts.iter().find_map(|a| a.data_file.clone()).unwrap();
        assert!(Path::new(&link.path).is_absolute(), "{}", link.path);
        assert!(crate::datafile::linked_to(&s, &want).is_some());
        // Opening it by a relative path finds it open already.
        let again = rel.with_extension("mcdata");
        assert!(s.run("file.openDataFile", &json!({"path": again.to_str().unwrap()})).is_err());
    }

    #[test]
    fn closing_a_protected_data_file_takes_its_items_out_of_the_profile_journal() {
        closing_a_protected_data_file_takes_its_items_out_of_the_profile_journal_in(
            mailcraft_store::StoreKind::Legacy,
        );
        closing_a_protected_data_file_takes_its_items_out_of_the_profile_journal_in(
            mailcraft_store::StoreKind::Sqlite,
        );
    }

    fn closing_a_protected_data_file_takes_its_items_out_of_the_profile_journal_in(
        kind: mailcraft_store::StoreKind,
    ) {
        let profile = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open_with(profile.path(), kind).unwrap();
        let file = dir.path().join("Private.mcdata");
        let r =
            s.run("file.newDataFile", &json!({"path": file.to_str().unwrap(), "password": "pw-1"})).unwrap();
        let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let inbox = s.mailbox.role_folder(id, FolderRole::Inbox).unwrap();
        s.mailbox.add_mail(MailItem {
            folder: inbox,
            subject: "Confidential plans".into(),
            ..Default::default()
        });
        s.journal(vec![mailcraft_store::Op::Replace(Box::new(s.mailbox.clone()))]);
        // Everything in the profile: the snapshot and journal, or the database and its log.
        let on_disk = |p: &Path| -> String { all_text_under(p) };
        assert!(on_disk(profile.path()).contains("Confidential"), "open, its items live in the profile");
        s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true})).unwrap();
        assert!(!on_disk(profile.path()).contains("Confidential"), "closed, they leave the journal");
        drop(s);
        let s = Session::open_with(profile.path(), kind).unwrap();
        assert!(s.mailbox.account(id).is_none(), "and stay closed after a restart");
    }

    #[test]
    fn closing_a_protected_data_file_whose_file_is_gone_asks_for_the_password_twice() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _, _) = session();
        let file = dir.path().join("Gone.mcdata");
        let p = file.to_str().unwrap();
        let r = s.run("file.newDataFile", &json!({"path": p, "password": "right-1"})).unwrap();
        let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        s.secrets.remove(&crate::datafile::secret_key(id)).unwrap();
        std::fs::remove_file(&file).unwrap();
        let r = s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true})).unwrap();
        assert_eq!(r["dialog"], "dataFilePassword");
        assert_eq!(r["params"]["verify"], true, "nothing to check the password against: it is typed twice");
        for bad in [json!({}), json!({"verify": "right-2"})] {
            let mut q = json!({"store": id.0, "confirm": true, "password": "right-1"});
            if let (Some(o), Some(extra)) = (q.as_object_mut(), bad.as_object()) {
                o.extend(extra.clone());
            }
            assert!(matches!(s.run("file.closeDataFile", &q), Err(CmdError::Params(_))), "{q}");
            assert!(s.secrets.get(&crate::datafile::secret_key(id)).is_none());
            assert!(s.mailbox.account(id).is_some());
            assert!(!file.exists());
        }
        let ok = json!({"store": id.0, "confirm": true, "password": "right-1", "verify": "right-1"});
        s.run("file.closeDataFile", &ok).unwrap();
        assert!(s.mailbox.account(id).is_none());
        assert!(crate::datafile::unseal(&std::fs::read(&file).unwrap(), "right-1").is_ok());
    }

    /// Every file under `dir` read as text, for checking that something is nowhere in a profile.
    fn all_text_under(dir: &Path) -> String {
        let mut out = String::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    todo.push(p);
                } else {
                    out.push_str(&String::from_utf8_lossy(&std::fs::read(&p).unwrap_or_default()));
                }
            }
        }
        out
    }

    /// Regression: closing a protected data file left the raw source of every message in it unencrypted in
    /// the profile's `blobs` folder for good (and its deleted items in the recoverable-items bin).
    #[test]
    fn closing_a_protected_data_file_removes_its_message_sources_from_the_profile() {
        closing_a_protected_data_file_removes_its_message_sources_from_the_profile_in(
            mailcraft_store::StoreKind::Legacy,
        );
        closing_a_protected_data_file_removes_its_message_sources_from_the_profile_in(
            mailcraft_store::StoreKind::Sqlite,
        );
    }

    fn closing_a_protected_data_file_removes_its_message_sources_from_the_profile_in(
        kind: mailcraft_store::StoreKind,
    ) {
        let profile = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open_with(profile.path(), kind).unwrap();
        let file = dir.path().join("Private.mcdata");
        let r =
            s.run("file.newDataFile", &json!({"path": file.to_str().unwrap(), "password": "pw-1"})).unwrap();
        let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let inbox = s.mailbox.role_folder(id, FolderRole::Inbox).unwrap();
        let other = s.mailbox.accounts.iter().find(|a| a.id != id).map(|a| a.id).unwrap();
        let other_inbox = s.mailbox.role_folder(other, FolderRole::Inbox).unwrap();
        let secret = b"From: a@example.com\r\nSubject: Q\r\n\r\nTopsecret-body-7f3a\r\n".to_vec();
        let shared = b"From: b@example.org\r\nSubject: S\r\n\r\nShared-body-91c2\r\n".to_vec();
        s.add_message(
            MailItem { folder: inbox, ..Default::default() },
            &mailcraft_store::Content::source(secret),
        )
        .unwrap();
        s.add_message(
            MailItem { folder: inbox, ..Default::default() },
            &mailcraft_store::Content::source(shared.clone()),
        )
        .unwrap();
        s.add_message(
            MailItem { folder: other_inbox, ..Default::default() },
            &mailcraft_store::Content::source(shared),
        )
        .unwrap();
        // A message deleted for good from the protected data file while it was open.
        let mut deleted = MailItem {
            id: ItemId(900_001),
            folder: inbox,
            subject: "Deleted-plan-55e1".into(),
            ..Default::default()
        };
        s.store_source(&mut deleted, b"Subject: D\r\n\r\nDeleted-src-55e1\r\n").unwrap();
        let now = s.now().unix;
        s.recover.bin.add(mailcraft_model::DeletedItem::Mail(Box::new(deleted)), "Inbox", now);
        s.store.as_mut().unwrap().save_recoverable(&mut s.recover.bin).unwrap();
        s.journal(vec![mailcraft_store::Op::Replace(Box::new(s.mailbox.clone()))]);
        let open = all_text_under(profile.path());
        for needle in ["Topsecret-body-7f3a", "Deleted-src-55e1", "Deleted-plan-55e1", "Shared-body-91c2"] {
            assert!(open.contains(needle), "open, {needle} lives in the profile");
        }
        s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true})).unwrap();
        let closed = all_text_under(profile.path());
        for needle in ["Topsecret-body-7f3a", "Deleted-src-55e1", "Deleted-plan-55e1"] {
            assert!(!closed.contains(needle), "closed, {needle} is gone from the profile");
        }
        assert!(closed.contains("Shared-body-91c2"), "a source another data file shares stays");
        assert!(s.recover.bin.items.is_empty());
        // The file keeps everything; the other data file's message still has its source.
        let image =
            crate::datafile::parse(&crate::datafile::unseal(&std::fs::read(&file).unwrap(), "pw-1").unwrap())
                .unwrap();
        assert_eq!(image.mail.len(), 2);
        let kept = s.mailbox.mail.items().find(|m| m.folder == other_inbox && m.subject.is_empty()).unwrap();
        let raw = s.store.as_ref().unwrap().raw_of(kept).unwrap().unwrap();
        assert!(String::from_utf8_lossy(&raw).contains("Shared-body-91c2"));
        drop(s);
        let s = Session::open_with(profile.path(), kind).unwrap();
        assert!(s.mailbox.account(id).is_none());
        let kept = s.mailbox.mail.items().find(|m| m.folder == other_inbox && m.subject.is_empty()).unwrap();
        assert!(s.store.as_ref().unwrap().raw_of(kept).unwrap().is_some(), "and after a restart");
    }

    /// Regression: closing a protected data file dropped only the recoverable items whose folder it still
    /// had. Items deleted for good together with a folder of it stayed in the bin, and their text and
    /// sources stayed readable in the profile.
    #[test]
    fn closing_a_protected_data_file_removes_items_deleted_with_its_folders() {
        closing_a_protected_data_file_removes_items_deleted_with_its_folders_in(
            mailcraft_store::StoreKind::Legacy,
        );
        closing_a_protected_data_file_removes_items_deleted_with_its_folders_in(
            mailcraft_store::StoreKind::Sqlite,
        );
    }

    fn closing_a_protected_data_file_removes_items_deleted_with_its_folders_in(
        kind: mailcraft_store::StoreKind,
    ) {
        let profile = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open_with(profile.path(), kind).unwrap();
        let file = dir.path().join("Private.mcdata");
        let r =
            s.run("file.newDataFile", &json!({"path": file.to_str().unwrap(), "password": "pw-1"})).unwrap();
        let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let inbox = s.mailbox.role_folder(id, FolderRole::Inbox).unwrap();
        let sub = s.mailbox.create_folder(inbox, "Plans").unwrap();
        let item = MailItem { folder: sub, subject: "Gone-plan-6b2d".into(), ..Default::default() };
        s.add_message(
            item,
            &mailcraft_store::Content::source(b"Subject: G\r\n\r\nGone-src-6b2d\r\n".to_vec()),
        )
        .unwrap();
        s.journal(vec![mailcraft_store::Op::Replace(Box::new(s.mailbox.clone()))]);
        // The folder goes to Deleted Items, then is deleted for good from there, message and all.
        s.selection.folder = sub;
        let r = s.run("folder.delete", &json!({"folder": sub.0, "confirm": true})).unwrap();
        assert_eq!(r["permanent"], false);
        s.selection.folder = sub;
        let r = s.run("folder.delete", &json!({"folder": sub.0, "confirm": true})).unwrap();
        assert_eq!(r["permanent"], true);
        assert!(s.mailbox.folder(sub).is_none());
        assert_eq!(s.recover.bin.items.len(), 1, "recoverable");
        assert_eq!(s.recover.bin.items[0].account, Some(id), "with the data file it came from");
        let open = all_text_under(profile.path());
        for needle in ["Gone-src-6b2d", "Gone-plan-6b2d"] {
            assert!(open.contains(needle), "open, {needle} lives in the profile");
        }
        s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true})).unwrap();
        assert!(s.recover.bin.items.is_empty());
        let closed = all_text_under(profile.path());
        for needle in ["Gone-src-6b2d", "Gone-plan-6b2d"] {
            assert!(!closed.contains(needle), "closed, {needle} is gone from the profile");
        }
    }

    /// Regression: New Data File with a password on a file that exists unprotected opened it unprotected
    /// and dropped the password silently, so it kept being written in clear.
    #[test]
    fn a_password_for_an_existing_unprotected_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _, _) = session();
        let file = dir.path().join("Plain.mcdata");
        let p = file.to_str().unwrap();
        let r = s.run("file.newDataFile", &json!({"path": p})).unwrap();
        s.run("file.closeDataFile", &json!({"store": r["account"], "confirm": true})).unwrap();
        let before = std::fs::read(&file).unwrap();
        let accounts = s.mailbox.accounts.len();
        for cmd in ["file.newDataFile", "file.openDataFile"] {
            let e = s.run(cmd, &json!({"path": p, "password": "pw-1"})).unwrap_err();
            assert!(matches!(&e, CmdError::Params(m) if m.contains("not protected")), "{cmd}: {e}");
            assert_eq!(s.mailbox.accounts.len(), accounts, "{cmd}: nothing opens");
        }
        assert_eq!(std::fs::read(&file).unwrap(), before, "the file is untouched");
        assert!(!crate::datafile::is_sealed(&before));
        // Without a password it opens as it is.
        let r = s.run("file.newDataFile", &json!({"path": p})).unwrap();
        assert_eq!(r["protected"], false);
    }

    /// Regression: a data file whose name contains the protected format's name was taken for a protected
    /// file and could never be opened again.
    #[test]
    fn a_data_file_named_like_the_protected_format_opens_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _, _) = session();
        let file = dir.path().join("odd.mcdata");
        let p = file.to_str().unwrap();
        let name = "mailcraft-data-file-sealed notes";
        let r = s.run("file.newDataFile", &json!({"path": p, "name": name})).unwrap();
        s.run("file.closeDataFile", &json!({"store": r["account"], "confirm": true})).unwrap();
        let r = s.run("file.openDataFile", &json!({"path": p})).unwrap();
        assert_eq!(r["name"], name);
        assert_eq!(r["protected"], false);
    }

    /// A data file with items in missing folders, a message listed twice and a broken folder parent.
    fn damaged_image() -> Vec<u8> {
        use mailcraft_model::{Folder, Task};
        let a = AccountId(1);
        let image = crate::datafile::DataFileImage {
            format: crate::datafile::FORMAT.to_owned(),
            version: crate::datafile::VERSION,
            account: Account { id: a, display_name: "Rescued".into(), ..Default::default() },
            folders: vec![
                Folder {
                    id: FolderId(1),
                    account: a,
                    name: "Inbox".into(),
                    role: Some(FolderRole::Inbox),
                    ..Default::default()
                },
                Folder {
                    id: FolderId(2),
                    account: a,
                    parent: Some(FolderId(77)),
                    name: "Lost".into(),
                    ..Default::default()
                },
            ],
            mail: vec![
                MailItem {
                    id: ItemId(10),
                    folder: FolderId(1),
                    subject: "Kept".into(),
                    ..Default::default()
                },
                MailItem {
                    id: ItemId(10),
                    folder: FolderId(1),
                    subject: "Twin".into(),
                    ..Default::default()
                },
                MailItem {
                    id: ItemId(11),
                    folder: FolderId(99),
                    subject: "Orphan".into(),
                    ..Default::default()
                },
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
            tasks: vec![Task { folder: FolderId(98), subject: "Orphan task".into(), ..Default::default() }],
            ..Default::default()
        };
        serde_json::to_vec(&image).unwrap()
    }

    /// Regression: opening a data file repaired it silently, and the first write-back on Close or Exit lost
    /// what was left out for good. Now the repairs are reported and the file as it was is kept first.
    #[test]
    fn opening_a_damaged_data_file_reports_its_repairs_and_keeps_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _, _) = session();
        let file = dir.path().join("Rescued.mcdata");
        let original = damaged_image();
        std::fs::write(&file, &original).unwrap();
        let r = s.run("file.openDataFile", &json!({"path": file.to_str().unwrap()})).unwrap();
        assert_eq!(r["items"], 1);
        assert_eq!(r["repaired"]["itemsWithoutFolder"], 2);
        assert_eq!(r["repaired"]["duplicateItems"], 1);
        assert_eq!(r["repaired"]["foldersMovedToTop"], 1);
        let bak = dir.path().join("Rescued.mcdata.bak");
        assert_eq!(r["backup"], bak.to_str().unwrap());
        assert_eq!(std::fs::read(&bak).unwrap(), original, "the file as it was");
        assert!(s.notices.iter().any(|n| matches!(n,
            Notice::Info(t) if t.contains("needed repairs") && t.contains("2 items whose folder is not in the file")
                && t.contains("Rescued.mcdata.bak"))));
        // Close writes the repaired data file back; the backup stays as it was.
        s.run("file.closeDataFile", &json!({"store": r["account"], "confirm": true})).unwrap();
        assert_eq!(crate::datafile::parse(&std::fs::read(&file).unwrap()).unwrap().mail.len(), 1);
        assert_eq!(std::fs::read(&bak).unwrap(), original);
        // Nothing is left to repair the next time, so no further backup.
        let r = s.run("file.openDataFile", &json!({"path": file.to_str().unwrap()})).unwrap();
        assert!(r["repaired"].is_null() && r["backup"].is_null(), "{r}");
        assert!(!dir.path().join("Rescued.mcdata.2.bak").exists());
        // A closed data file of the profile reports its repairs too.
        s.closed_data_files
            .push(crate::datafile::ClosedDataFile { name: "Old".into(), bytes: damaged_image() });
        let r = s.run("file.openDataFile", &json!({"name": "Old"})).unwrap();
        assert_eq!(r["repaired"]["itemsWithoutFolder"], 2);
        assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
    }

    /// Regression: every Exit rewrote every linked data file, even unchanged, and two profiles with the same
    /// file open overwrote each other (last writer won). Now an unchanged data file is left alone, and a file
    /// something else wrote since it was read is never overwritten: this side goes to a copy next to it.
    #[test]
    fn exit_leaves_unchanged_files_alone_and_never_overwrites_a_file_changed_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Shared.mcdata");
        let p = file.to_str().unwrap();
        let (mut a, _, _) = session();
        let r = a.run("file.newDataFile", &json!({"path": p, "password": "pw-1"})).unwrap();
        let a_id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        // Unchanged: Exit does not write it again (a protected file would come out different: new salt).
        let written = std::fs::read(&file).unwrap();
        a.run("file.exit", &json!({})).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), written, "unchanged, not rewritten");
        // A second profile opens the same file.
        let (mut b, _, _) = session();
        let r = b.run("file.openDataFile", &json!({"path": p, "password": "pw-1"})).unwrap();
        let b_id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        // The first one changes it and exits: written.
        let inbox = a.mailbox.role_folder(a_id, FolderRole::Inbox).unwrap();
        a.mailbox.add_mail(MailItem { folder: inbox, subject: "From A".into(), ..Default::default() });
        a.run("file.exit", &json!({})).unwrap();
        let from_a = std::fs::read(&file).unwrap();
        assert_ne!(from_a, written);
        // The second one changes it too: its exit does not overwrite what the first wrote.
        let inbox = b.mailbox.role_folder(b_id, FolderRole::Inbox).unwrap();
        b.mailbox.add_mail(MailItem { folder: inbox, subject: "From B".into(), ..Default::default() });
        b.run("file.exit", &json!({})).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), from_a, "the other profile's save is kept");
        let copy = dir.path().join("Shared (copy).mcdata");
        let subjects = |path: &Path| -> Vec<String> {
            let plain = crate::datafile::unseal(&std::fs::read(path).unwrap(), "pw-1").unwrap();
            crate::datafile::parse(&plain).unwrap().mail.into_iter().map(|m| m.item.subject).collect()
        };
        assert_eq!(subjects(&file), ["From A"]);
        assert_eq!(subjects(&copy), ["From B"], "this side went to a copy, protected all the same");
        assert!(b.notices.iter().any(|n| matches!(n,
            Notice::Info(t) if t.contains("changed by another program") && t.contains("Shared (copy).mcdata"))));
        // It stays linked to the copy: closing it writes there, and the shared file is still left alone.
        let link = b.mailbox.account(b_id).and_then(|x| x.data_file.clone()).unwrap();
        assert_eq!(link.path, copy.to_str().unwrap());
        let r = b.run("file.closeDataFile", &json!({"store": b_id.0, "confirm": true})).unwrap();
        assert_eq!(r["path"], copy.to_str().unwrap());
        assert_eq!(std::fs::read(&file).unwrap(), from_a);
        assert!(b.linked_seen.is_empty(), "nothing is remembered for a closed data file");
    }

    /// Regression: Exit checkpointed the profile before it wrote the data files, so a data file it sent to
    /// a copy was linked to that copy in memory only. The next start-up followed the original file again,
    /// and the next Exit wrote over what the other program had written there.
    #[test]
    fn a_copy_made_on_exit_stays_linked_after_a_restart() {
        let profile = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Shared.mcdata");
        let copy = dir.path().join("Shared (copy).mcdata");
        let subjects = |path: &Path| -> Vec<String> {
            let image = crate::datafile::parse(&std::fs::read(path).unwrap()).unwrap();
            let mut v: Vec<String> = image.mail.into_iter().map(|m| m.item.subject).collect();
            v.sort();
            v
        };
        let mut s = Session::open(profile.path()).unwrap();
        let r = s.run("file.newDataFile", &json!({"path": file.to_str().unwrap()})).unwrap();
        let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        // Something else writes the file while this side changes the data file too.
        let other = crate::datafile::tests::plain_file("Written elsewhere");
        std::fs::write(&file, &other).unwrap();
        let inbox = s.mailbox.role_folder(id, FolderRole::Inbox).unwrap();
        s.mailbox.add_mail(MailItem { folder: inbox, subject: "Mine".into(), ..Default::default() });
        s.run("file.exit", &json!({})).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), other);
        assert_eq!(subjects(&copy), ["Mine"]);
        drop(s);
        // After a restart the data file still follows the copy.
        let mut s = Session::open(profile.path()).unwrap();
        let link = s.mailbox.account(id).and_then(|a| a.data_file.clone()).unwrap();
        assert_eq!(link.path, copy.to_str().unwrap());
        // So the next Exit writes there and leaves the other program's file as it wrote it.
        let inbox = s.mailbox.role_folder(id, FolderRole::Inbox).unwrap();
        s.mailbox.add_mail(MailItem { folder: inbox, subject: "Mine too".into(), ..Default::default() });
        s.run("file.exit", &json!({})).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), other);
        assert_eq!(subjects(&copy), ["Mine", "Mine too"]);
        assert!(!dir.path().join("Shared (copy 2).mcdata").exists());
        drop(s);
        let s = Session::open(profile.path()).unwrap();
        let link = s.mailbox.account(id).and_then(|a| a.data_file.clone()).unwrap();
        assert_eq!(link.path, copy.to_str().unwrap());
    }

    /// Regression: a profile that only had a shared file open wrote a stale copy of it on Exit once another
    /// profile had saved the file, and followed that stale copy from then on. With no changes of its own,
    /// nothing is written and the link stays.
    #[test]
    fn exit_without_changes_leaves_a_file_changed_elsewhere_and_makes_no_copy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Shared.mcdata");
        let p = file.to_str().unwrap();
        let (mut a, _, _) = session();
        let r = a.run("file.newDataFile", &json!({"path": p})).unwrap();
        let a_id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let (mut b, _, _) = session();
        let r = b.run("file.openDataFile", &json!({"path": p})).unwrap();
        let b_id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let inbox = a.mailbox.role_folder(a_id, FolderRole::Inbox).unwrap();
        a.mailbox.add_mail(MailItem { folder: inbox, subject: "From A".into(), ..Default::default() });
        a.run("file.exit", &json!({})).unwrap();
        let from_a = std::fs::read(&file).unwrap();
        b.run("file.exit", &json!({})).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), from_a);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no copy");
        let link = b.mailbox.account(b_id).and_then(|x| x.data_file.clone()).unwrap();
        assert_eq!(link.path, p);
        assert!(!b.notices.iter().any(|n| matches!(n, Notice::Info(t) if t.contains("changed by another"))));
        // Closing it without changes leaves the newer file too, and says why nothing was written.
        let r = b.run("file.closeDataFile", &json!({"store": b_id.0, "confirm": true})).unwrap();
        assert_eq!(r["path"], p);
        assert_eq!(std::fs::read(&file).unwrap(), from_a);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no copy");
        assert!(b.notices.iter().any(|n| matches!(n,
            Notice::Info(t) if t.contains("changed by another program") && t.contains("no changes of its own"))));
    }

    /// Changes made on disk while a profile was not running are noticed from the next start-up on.
    #[test]
    fn a_file_changed_after_start_up_is_not_overwritten() {
        let profile = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Notes.mcdata");
        let mut s = Session::open(profile.path()).unwrap();
        let r = s.run("file.newDataFile", &json!({"path": file.to_str().unwrap()})).unwrap();
        let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        drop(s);
        let mut s = Session::open(profile.path()).unwrap();
        assert!(s.linked_seen.contains_key(&id), "noted at start-up");
        // Something else rewrites the file (a different size).
        let other = crate::datafile::tests::plain_file("Written elsewhere");
        std::fs::write(&file, &other).unwrap();
        let r = s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true})).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), other);
        let copy = dir.path().join("Notes (copy).mcdata");
        assert_eq!(r["path"], copy.to_str().unwrap());
        assert!(crate::datafile::parse(&std::fs::read(&copy).unwrap()).is_ok());
    }

    /// A data file protected in the earlier format (no MAC) opens, and is written in the current one.
    #[test]
    fn a_version_1_protected_file_opens_and_is_saved_in_the_current_format() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _, _) = session();
        let file = dir.path().join("Old.mcdata");
        let p = file.to_str().unwrap();
        let plain = crate::datafile::tests::plain_file("Old archive");
        std::fs::write(&file, crate::datafile::tests::v1_file(&plain, "old-pw")).unwrap();
        assert_eq!(s.run("file.openDataFile", &json!({"path": p})).unwrap()["dialog"], "dataFilePassword");
        let r = s.run("file.openDataFile", &json!({"path": p, "password": "old-pw"})).unwrap();
        assert_eq!((r["name"].as_str(), r["protected"].as_bool()), (Some("Old archive"), Some(true)));
        let id = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        // Closing without the kept password checks the typed one against the old file first.
        s.secrets.remove(&crate::datafile::secret_key(id)).unwrap();
        assert!(
            s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true, "password": "nope"}))
                .is_err()
        );
        s.run("file.closeDataFile", &json!({"store": id.0, "confirm": true, "password": "old-pw"})).unwrap();
        let bytes = std::fs::read(&file).unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["version"], crate::datafile::SEALED_VERSION);
        assert_eq!(v["auth"], "hmac-sha256");
        assert!(crate::datafile::unseal(&bytes, "old-pw").is_ok());
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
        s.run("file.closeDataFile", &json!({"store": "Old", "confirm": true})).unwrap();
        assert!(!s.mailbox.rules[0].enabled, "a rule moving into a removed data file must be turned off");
        assert!(s.mailbox.quick_steps[0].needs_setup(), "the quick step must ask for a folder again");
        assert!(!s.mailbox.favorites().contains(&target));
    }

    #[test]
    fn closed_data_file_stays_on_disk_and_reopens_with_its_folders_and_sources() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path()).unwrap();
        let r = s.run("file.newDataFile", &json!({"name": "Old/Mail"})).unwrap();
        let acct = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        let sub = s.mailbox.create_folder(inbox, "Receipts").unwrap();
        let item = MailItem { folder: sub, subject: "Report: Q3/Q4".into(), ..Default::default() };
        let item = s.add_message(item, &mailcraft_store::Content::source(EML.to_vec())).unwrap();
        // The source is in the body store, as it is for downloaded mail.
        assert!(s.mailbox.mail.get(item).unwrap().blob.is_some());
        let r = s.run("file.closeDataFile", &json!({"store": acct.0, "confirm": true})).unwrap();
        let path = std::path::PathBuf::from(r["path"].as_str().unwrap());
        assert!(path.starts_with(dir.path().join(crate::datafile::CLOSED_DIR)));
        assert_eq!(path.file_name().unwrap(), "Old_Mail.mcdata");
        assert!(path.exists());
        assert!(s.mailbox.account(acct).is_none());
        assert_eq!(crate::datafile::closed_entries(&s).len(), 1);
        let r = s.run("file.openDataFile", &json!({"name": "Old_Mail"})).unwrap();
        let back = AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap());
        assert_eq!(r["name"], "Old/Mail");
        let inbox2 = s.mailbox.role_folder(back, FolderRole::Inbox).unwrap();
        let kids = s.mailbox.children(back, Some(inbox2));
        assert_eq!(kids.len(), 1);
        let m = s.mailbox.items_in(kids[0])[0];
        assert_eq!(m.subject, "Report: Q3/Q4");
        // The source travels with the closed data file (back in the store, it leaves memory again).
        assert_eq!(crate::cmd::compose::stored_raw(&s, m).as_deref(), Some(EML));
        assert!(m.blob.is_some());
        assert!(!path.exists(), "an opened file is not offered again");
        assert!(crate::datafile::closed_entries(&s).is_empty());
        assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
    }

    #[test]
    fn hostile_closed_data_files_are_refused_or_repaired() {
        use crate::datafile::{DataFileImage, FORMAT, attach, parse};
        use mailcraft_model::Folder;
        let (mut s, _, _) = session();
        assert!(parse(b"not json").is_err());
        assert!(parse(br#"{"format":"other","version":1}"#).is_err());
        assert!(parse(br#"{"format":"mailcraft-data-file","version":99}"#).is_err());
        let folder = |id: u64, parent: Option<u64>, name: &str, role: Option<FolderRole>| Folder {
            id: FolderId(id),
            account: AccountId(9),
            parent: parent.map(FolderId),
            name: name.into(),
            role,
            kind: FolderKind::Mail,
            ..Default::default()
        };
        // A parent cycle, a parent outside the file, a duplicate role and an item outside the file's folders.
        let image = DataFileImage {
            format: FORMAT.into(),
            version: 1,
            account: Account { display_name: "Test".into(), is_default: true, ..Default::default() },
            folders: vec![
                folder(1, Some(2), "A", None),
                folder(2, Some(1), "B", None),
                folder(3, Some(77), "C", Some(FolderRole::Inbox)),
                folder(4, None, "D", Some(FolderRole::Inbox)),
            ],
            mail: vec![
                MailItem { id: ItemId(5), folder: FolderId(3), subject: "kept".into(), ..Default::default() },
                MailItem {
                    id: ItemId(6),
                    folder: FolderId(99),
                    subject: "orphan".into(),
                    ..Default::default()
                },
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&image).unwrap();
        let id = attach(&mut s, parse(&bytes).unwrap()).unwrap();
        let a = s.mailbox.account(id).unwrap();
        assert_eq!(a.display_name, "Test (2)");
        assert!(!a.is_default, "an opened data file never takes over as the default");
        assert!(s.mailbox.mail.items().any(|m| m.subject == "kept"));
        assert!(!s.mailbox.mail.items().any(|m| m.subject == "orphan"));
        let inboxes =
            s.mailbox.folders.values().filter(|f| f.account == id && f.role == Some(FolderRole::Inbox));
        assert_eq!(inboxes.count(), 1);
        assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
    }

    fn account_of(r: &Value) -> AccountId {
        AccountId(u32::try_from(r["account"].as_u64().unwrap()).unwrap())
    }

    /// Regression: a server account's data file reopened as a server account with its old sync state, so
    /// the next command queued an upload of every folder and message (and replayed stale queued ops).
    #[test]
    fn reopened_server_account_is_a_local_data_file_and_queues_nothing() {
        use mailcraft_model::account::{AccountSync, PendingOp, RemoteFolderState, RemoteOp};
        use mailcraft_model::{Security, ServerSettings};
        let (mut s, _, _) = session();
        let server = |host: &str| ServerSettings {
            host: host.into(),
            port: 993,
            security: Security::Tls,
            username: "me@example.net".into(),
            ..Default::default()
        };
        let acct = s.mailbox.add_account(Account {
            kind: AccountKind::Imap,
            display_name: "me@example.net".into(),
            address: "me@example.net".into(),
            incoming: Some(server("imap.example.net")),
            outgoing: Some(server("smtp.example.net")),
            ..Default::default()
        });
        s.mailbox.create_default_folders(acct);
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s.mailbox.folders.get_mut(&inbox).unwrap().remote_name = Some("INBOX".into());
        let item = s.mailbox.add_mail(MailItem {
            folder: inbox,
            subject: "Synced".into(),
            remote_id: Some("7".into()),
            ..Default::default()
        });
        let sync = AccountSync {
            delimiter: Some('/'),
            folders: [(inbox, RemoteFolderState { uidvalidity: 1, uidnext: 8, highest_modseq: 0 })].into(),
            pending: vec![PendingOp {
                id: 1,
                op: RemoteOp::Flags {
                    folder: "INBOX".into(),
                    uid: 7,
                    add: vec!["\\Seen".into()],
                    remove: vec![],
                },
                attempts: 0,
                not_before: 0,
                last_error: None,
            }],
            next_op: 1,
            ..Default::default()
        };
        s.mailbox.accounts.iter_mut().find(|a| a.id == acct).unwrap().sync = sync;
        let _ = item;
        s.run("file.closeDataFile", &json!({"store": acct.0, "confirm": true})).unwrap();
        let r = s.run("file.openDataFile", &json!({"name": "me@example.net"})).unwrap();
        let back = account_of(&r);
        let a = s.mailbox.account(back).unwrap();
        assert_eq!(a.kind, AccountKind::Local);
        assert!(a.incoming.is_none() && a.outgoing.is_none());
        assert_eq!(a.sync, AccountSync::default());
        // Later changes stay local too.
        let inbox2 = s.mailbox.role_folder(back, FolderRole::Inbox).unwrap();
        let moved = s.mailbox.items_in(inbox2)[0].id;
        s.run("mail.markRead", &json!({"ids": [moved.0]})).ok();
        s.run("folder.new", &json!({"parent": inbox2.0, "name": "Later"})).ok();
        assert!(
            s.mailbox.accounts.iter().all(|a| a.sync.pending.is_empty()),
            "nothing is queued for a server"
        );
        assert!(s.mailbox.folders.values().filter(|f| f.account == back).all(|f| f.remote_name.is_none()));
        assert!(s.mailbox.validate().is_empty(), "{:?}", s.mailbox.validate());
    }

    /// Regression: Undo after Open Data File dropped the data file while its closed copy was already set
    /// aside (or gone, in memory), and Undo after Remove brought it back while the closed copy stayed.
    #[test]
    fn removing_and_opening_a_data_file_clear_the_undo_history() {
        let (mut s, _, _) = session();
        let acct = account_of(&s.run("file.newDataFile", &json!({"name": "Old"})).unwrap());
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        s.mailbox.add_mail(MailItem { folder: inbox, subject: "keep me".into(), ..Default::default() });
        assert!(s.undo.can_undo());
        s.run("file.closeDataFile", &json!({"store": "Old", "confirm": true})).unwrap();
        assert!(!s.undo.can_undo(), "Remove is not undoable");
        assert!(s.run("app.undo", &json!({})).is_err() || s.mailbox.account(acct).is_none());
        assert!(s.mailbox.accounts.iter().all(|a| a.display_name != "Old"));
        s.run("file.openDataFile", &json!({"name": "Old"})).unwrap();
        assert!(!s.undo.can_undo(), "Open Data File is not undoable");
        let _ = s.run("app.undo", &json!({}));
        let copies = s.mailbox.accounts.iter().filter(|a| a.display_name == "Old").count();
        assert_eq!(copies, 1);
        assert_eq!(s.mailbox.mail.items().filter(|m| m.subject == "keep me").count(), 1);
        // Later commands record undo steps again.
        let id = s.mailbox.mail.items().find(|m| m.subject == "keep me").unwrap().id;
        s.run("mail.markUnread", &json!({"ids": [id.0]})).ok();
        s.run("mail.markRead", &json!({"ids": [id.0]})).unwrap();
        assert!(s.undo.can_undo());
    }

    /// Regression: the 65th Remove in a session without a store silently threw away the oldest closed data
    /// file and its mail.
    #[test]
    fn in_memory_closed_data_files_are_never_evicted() {
        let (mut s, _, _) = session();
        for i in 0..64 {
            s.run("file.newDataFile", &json!({"name": format!("File {i}")})).unwrap();
            s.run("file.closeDataFile", &json!({"store": format!("File {i}"), "confirm": true})).unwrap();
        }
        assert_eq!(s.closed_data_files.len(), 64);
        s.run("file.newDataFile", &json!({"name": "One too many"})).unwrap();
        let r = s.run("file.closeDataFile", &json!({"store": "One too many", "confirm": true}));
        assert!(matches!(r, Err(CmdError::Failed(_))), "{r:?}");
        assert!(s.mailbox.accounts.iter().any(|a| a.display_name == "One too many"), "nothing was removed");
        assert_eq!(s.closed_data_files.len(), 64);
        assert_eq!(s.closed_data_files[0].name, "File 0", "the oldest closed data file is kept");
    }

    /// Regression: a long folder chain cost a full parent walk per folder (quadratic), the tree had no
    /// depth cap, and invalid or duplicate sibling names got through.
    #[test]
    fn opened_data_file_folders_are_capped_in_depth_and_named_validly() {
        use crate::datafile::{DataFileImage, FORMAT, attach};
        use mailcraft_model::Folder;
        let (mut s, _, _) = session();
        let folder = |id: u64, parent: Option<u64>, name: &str| Folder {
            id: FolderId(id),
            account: AccountId(9),
            parent: parent.map(FolderId),
            name: name.into(),
            kind: FolderKind::Mail,
            ..Default::default()
        };
        const N: u64 = 20_000;
        let mut folders: Vec<Folder> =
            (1..=N).map(|i| folder(i, if i == 1 { None } else { Some(i - 1) }, "Level")).collect();
        folders.push(folder(N + 1, None, "Dup"));
        folders.push(folder(N + 2, None, "dup"));
        folders.push(folder(N + 3, None, "Inbox"));
        folders.push(folder(N + 4, None, "a\\b"));
        let image = DataFileImage {
            format: FORMAT.into(),
            version: 1,
            account: Account { display_name: "Deep".into(), ..Default::default() },
            folders,
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let id = attach(&mut s, image).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "opening is not quadratic");
        let mine: Vec<&Folder> = s.mailbox.folders.values().filter(|f| f.account == id).collect();
        for f in &mine {
            let mut depth = 0usize;
            let mut cur = f.parent;
            while let Some(p) = cur {
                depth += 1;
                assert!(depth <= 64, "folder nested deeper than the cap");
                cur = s.mailbox.folders[&p].parent;
            }
            assert!(!f.name.contains('\\'), "{}", f.name);
        }
        let mut seen = std::collections::BTreeSet::new();
        for f in &mine {
            assert!(seen.insert((f.parent, f.name.to_lowercase())), "duplicate sibling name {}", f.name);
        }
        let inbox = s.mailbox.role_folder(id, FolderRole::Inbox).unwrap();
        assert_eq!(s.mailbox.folders[&inbox].name, "Inbox", "the default folder keeps its name");
        assert!(mine.iter().any(|f| f.name == "Inbox (2)"));
        assert!(mine.iter().any(|f| f.name == "a_b"));
    }

    /// Regression: ids stored inside an opened data file (AutoArchive folder, the message a task came from,
    /// voting responses) kept their old values, which point at unrelated folders and mail here.
    #[test]
    fn opened_data_file_links_follow_the_new_ids() {
        use crate::datafile::{DataFileImage, FORMAT, attach};
        use mailcraft_model::folder::FolderArchive;
        use mailcraft_model::{Folder, Task, VoteRecord};
        let (mut s, _, _) = session();
        let folder = |id: u64, target: Option<u64>| Folder {
            id: FolderId(id),
            account: AccountId(9),
            name: format!("F{id}"),
            kind: FolderKind::Mail,
            auto_archive: Some(FolderArchive { target: target.map(FolderId), ..Default::default() }),
            ..Default::default()
        };
        let vote = |r: u64| VoteRecord {
            from: "a@example.com".into(),
            response: Some(ItemId(r)),
            ..Default::default()
        };
        let image = DataFileImage {
            format: FORMAT.into(),
            version: 1,
            account: Account { display_name: "Links".into(), ..Default::default() },
            folders: vec![folder(1, Some(2)), folder(2, Some(4242)), folder(3, None)],
            mail: vec![
                MailItem {
                    id: ItemId(10),
                    folder: FolderId(1),
                    subject: "Vote".into(),
                    votes: vec![vote(11), vote(9999)],
                    ..Default::default()
                },
                MailItem {
                    id: ItemId(11),
                    folder: FolderId(1),
                    subject: "Response".into(),
                    ..Default::default()
                },
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
            tasks: vec![
                Task {
                    id: ItemId(20),
                    folder: FolderId(3),
                    from_mail: Some(ItemId(10)),
                    ..Default::default()
                },
                Task {
                    id: ItemId(21),
                    folder: FolderId(3),
                    from_mail: Some(ItemId(5555)),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let id = attach(&mut s, image).unwrap();
        let by_name = |n: &str| s.mailbox.folders.values().find(|f| f.account == id && f.name == n).unwrap();
        let (f1, f2) = (by_name("F1"), by_name("F2"));
        assert_eq!(f1.auto_archive.unwrap().target, Some(f2.id));
        assert_eq!(f2.auto_archive.unwrap().target, None, "a folder outside the file is not a target");
        let mail = |n: &str| s.mailbox.mail.items().find(|m| m.subject == n).unwrap();
        let (voted, response) = (mail("Vote"), mail("Response"));
        assert_eq!(voted.votes[0].response, Some(response.id));
        assert_eq!(voted.votes[1].response, None);
        let mut links: Vec<Option<ItemId>> = s
            .mailbox
            .tasks
            .values()
            .filter(|t| s.mailbox.folder_account(t.folder) == Some(id))
            .map(|t| t.from_mail)
            .collect();
        links.sort();
        assert_eq!(links, vec![None, Some(voted.id)]);
    }

    #[test]
    fn data_file_properties_rename_and_compact() {
        let (mut s, _, _) = session();
        let acct = s.mailbox.accounts[0].id;
        let r = s.run("file.dataFileProperties", &json!({"store": acct.0})).unwrap();
        assert_eq!(r["dialog"], "dataFileProperties");
        let Some(Notice::OpenDialog(_, d)) = s.notices.last() else { panic!("no dialog") };
        assert_eq!(d["items"], 3);
        assert_eq!(d["name"], "Test");
        s.run("file.dataFileProperties", &json!({"store": acct.0, "name": " Mine "})).unwrap();
        assert_eq!(s.mailbox.accounts[0].display_name, "Mine");
        assert!(s.run("file.dataFileProperties", &json!({"store": acct.0, "name": "  "})).is_err());
        assert!(matches!(
            s.run("file.dataFileProperties", &json!({"store": acct.0, "compact": true})),
            Err(CmdError::Disabled(_))
        ));
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path()).unwrap();
        s.run("file.newDataFile", &json!({"name": "Extra"})).unwrap();
        let r = s.run("file.dataFileProperties", &json!({"store": "Extra", "compact": true})).unwrap();
        assert_eq!(r["compacted"], true);
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
        assert_eq!(s.mailbox.mail.get(back).unwrap().subject, "Report: Q3/Q4");
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

    #[test]
    fn new_data_file_without_a_name_asks_for_one() {
        let (mut s, _, _) = session();
        let before = s.mailbox.accounts.len();
        let r = s.run("file.newDataFile", &json!({})).unwrap();
        assert_eq!(r["dialog"], "newDataFile");
        assert_eq!(s.mailbox.accounts.len(), before, "nothing is created until the dialog's OK");
        let Some(crate::Notice::OpenDialog(_, p)) = s.notices.last() else { panic!("no dialog") };
        assert!(p["name"].as_str().is_some_and(|n| !n.is_empty()));
        s.run("file.newDataFile", &json!({"name": "Archive"})).unwrap();
        assert_eq!(s.mailbox.accounts.len(), before + 1);
    }

    #[test]
    fn new_data_file_asks_for_a_name_first() {
        let (mut s, _, _) = session();
        let before = s.mailbox.accounts.len();
        // From the ribbon (no name): the Create Data File prompt, with a free name suggested.
        let r = s.run("file.newDataFile", &json!({})).unwrap();
        assert_eq!(r["dialog"], "newDataFile");
        let Some(crate::Notice::OpenDialog(_, p)) = s.notices.last() else { panic!("no dialog") };
        let suggested = p["name"].as_str().unwrap().to_owned();
        assert!(!suggested.is_empty());
        assert_eq!(s.mailbox.accounts.len(), before, "nothing made yet");
        let r = s.run("file.newDataFile", &json!({"name": suggested})).unwrap();
        assert_eq!(r["name"], suggested.as_str());
        assert_eq!(s.mailbox.accounts.len(), before + 1);
        // Scripts can skip the prompt.
        assert!(s.run("file.newDataFile", &json!({"confirm": true})).unwrap()["account"].is_u64());
    }

    #[test]
    fn choose_form_lists_and_opens_templates() {
        let dir = tempfile::tempdir().unwrap();
        let eml = "From: Ann <ann@example.com>\r\nTo: Team <team@example.org>\r\nSubject: Weekly report\r\n\r\nWins:\r\nRisks:\r\n";
        std::fs::write(dir.path().join("Weekly report.eml"), eml).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a template").unwrap();
        std::fs::write(dir.path().join("Broken.oft"), "not a compound file").unwrap();
        let (mut s, _, _) = session();
        let d = dir.path().display().to_string();
        let r = s.run("mail.newItem", &json!({"kind": "fromForm", "dir": d, "list": true})).unwrap();
        let names: Vec<&str> =
            r["templates"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, vec!["Broken", "Weekly report"]);
        // Without `list` the Choose Form dialog opens with them.
        let r = s.run("mail.newItem", &json!({"kind": "fromForm", "dir": d})).unwrap();
        assert_eq!(r["dialog"], "chooseForm");
        // Opening a template starts a new message from it.
        let path = dir.path().join("Weekly report.eml").display().to_string();
        let r = s.run("mail.newItem", &json!({"kind": "fromForm", "path": path})).unwrap();
        let c = &s.composes[&r["compose"].as_u64().unwrap()];
        assert_eq!(c.subject, "Weekly report");
        assert!(c.to.contains("team@example.org"));
        assert!(c.body.to_plain_text().contains("Risks:"));
        // Not a template, unreadable or missing files are refused.
        let txt = dir.path().join("notes.txt").display().to_string();
        assert!(matches!(
            s.run("mail.newItem", &json!({"kind": "fromForm", "path": txt})),
            Err(CmdError::Params(_))
        ));
        let broken = dir.path().join("Broken.oft").display().to_string();
        assert!(s.run("mail.newItem", &json!({"kind": "fromForm", "path": broken})).is_err());
        let gone = dir.path().join("Gone.msg").display().to_string();
        assert!(s.run("mail.newItem", &json!({"kind": "fromForm", "path": gone})).is_err());
        // A folder that cannot be read lists nothing.
        let r = s
            .run("mail.newItem", &json!({"kind": "fromForm", "dir": "/nonexistent-mc", "list": true}))
            .unwrap();
        assert!(r["templates"].as_array().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn exit_keeps_pending_ops_when_the_write_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, inbox, _) = session();
        s.store = Some(mailcraft_store::Store::open(dir.path()).unwrap());
        // A journal capped at one byte refuses the write.
        let item = MailItem { id: ItemId(9_999), folder: inbox, ..Default::default() };
        s.pending_ops.push(mailcraft_store::Op::PutMail(Box::new(item)));
        s.store.as_mut().unwrap().set_journal_cap(1);
        assert!(s.run("file.exit", &json!({})).is_err());
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
