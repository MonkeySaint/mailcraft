//! File-side dialogs: Mailbox Cleanup, the Import and Export Wizard (EML, MSG, MBOX, Maildir, iCalendar,
//! vCard, CSV), Save As (EML, MSG, text, HTML) and Save Attachments (Print: `print.rs`). Path fields get a Browse... button when the app installed file dialogs
//! ([`crate::FileDialogs`]). Account dialogs live in `accounts`.

use egui::{Context, RichText, Ui};
use serde_json::{Value, json};

use super::common::{
    self, Dlg, Out, Pick, btn, btn_en, folder_path, folder_tree, footer, muted, ok_cancel, primary, window,
    with_state,
};
use super::folders::last_error;
use crate::App;
use mailcraft_model::{FolderId, FolderKind, FolderRole};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "mailboxCleanup" => cleanup(ctx, app, d),
        "importExport" => import_export(ctx, app, d),
        "saveAs" => save_as(ctx, app, d),
        "saveAttachments" => save_attachments(ctx, app, d),
        _ => return None,
    })
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".into())
}

// ---------------------------------------------------------------------------------------------------------
// Mailbox Cleanup

#[derive(Clone, Default)]
struct CleanupState {
    older: u32,
    larger: u32,
    show_sizes: bool,
}

fn cleanup(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    with_state(
        ctx,
        app,
        uid,
        |_: &App| CleanupState { older: 90, larger: 250, show_sizes: false },
        |st, app| {
            window(ctx, app, d, "Mailbox Cleanup", 520.0, |ui, app| {
                let mut out = Out::Keep;
                muted(
                    ui,
                    app,
                    "Keep your mailbox small: find old or large items, archive, and empty Deleted Items.",
                );
                ui.horizontal(|ui| {
                    let total: u64 = app.s.mailbox.mail.values().map(|m| m.size).sum();
                    ui.label(format!("Mailbox size: {}", common::size_text(total)));
                    if btn(ui, "View Mailbox Size...") {
                        st.show_sizes = !st.show_sizes;
                    }
                });
                if st.show_sizes {
                    folder_sizes(ui, app, uid);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Find items older than");
                    ui.add(egui::DragValue::new(&mut st.older).range(1..=3650));
                    ui.label("days");
                    if btn(ui, "Find...") {
                        out = Out::open("advancedFind", json!({"olderDays": st.older}));
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Find items larger than");
                    ui.add(egui::DragValue::new(&mut st.larger).range(1..=1_000_000));
                    ui.label("kilobytes");
                    if btn(ui, "Find...") {
                        out = Out::open("advancedFind", json!({"largerKb": st.larger}));
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Move old items to the archive folder (AutoArchive).");
                    if btn(ui, "AutoArchive") {
                        app.run("file.archive", json!({"all": true}));
                    }
                });
                ui.separator();
                let deleted = deleted_size(app);
                ui.horizontal(|ui| {
                ui.label(format!("Deleted Items: {}", common::size_text(deleted)));
                if btn_en(ui, deleted > 0 || has_deleted_subfolders(app), "Empty") {
                    out = Out::open("confirm", json!({"title": "Mailbox Cleanup", "text": "Permanently delete everything in Deleted Items?", "command": "folder.emptyDeleted", "params": {}}));
                }
            });
                if !matches!(out, Out::Keep) {
                    return out;
                }
                footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
            })
        },
    )
}

fn deleted_folder(app: &App) -> Option<FolderId> {
    let acct = app.s.current_account()?;
    app.s.mailbox.role_folder(acct, mailcraft_model::FolderRole::DeletedItems)
}

fn deleted_size(app: &App) -> u64 {
    let Some(d) = deleted_folder(app) else { return 0 };
    let tree = app.s.mailbox.subtree(d);
    app.s.mailbox.mail.values().filter(|m| tree.contains(&m.folder) || m.folder == d).map(|m| m.size).sum()
}

fn has_deleted_subfolders(app: &App) -> bool {
    deleted_folder(app).is_some_and(|d| app.s.mailbox.folders.values().any(|f| f.parent == Some(d)))
}

fn folder_sizes(ui: &mut Ui, app: &App, uid: u64) {
    egui::ScrollArea::vertical().id_salt(("sizes", uid)).max_height(160.0).show(ui, |ui| {
        egui::Grid::new(("sizesg", uid)).num_columns(3).striped(true).show(ui, |ui| {
            ui.label(RichText::new("Folder").strong());
            ui.label(RichText::new("Items").strong());
            ui.label(RichText::new("Size").strong());
            ui.end_row();
            for f in app.s.mailbox.folders.values() {
                let items = app.s.mailbox.items_in(f.id);
                if items.is_empty() {
                    continue;
                }
                let size: u64 = items.iter().map(|m| m.size).sum();
                ui.label(app.s.mailbox.folder_path(f.id));
                ui.label(items.len().to_string());
                ui.label(common::size_text(size));
                ui.end_row();
            }
        });
    });
}

// ---------------------------------------------------------------------------------------------------------
// Import and Export Wizard

/// One Import and Export Wizard action (`file.openExport` `action`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct IeAction {
    pub key: &'static str,
    pub label: &'static str,
    pub desc: &'static str,
    /// Folders the action reads from or files into.
    pub pick: Pick,
    /// File type for the file dialogs: name and extensions (empty = a folder, Maildir).
    pub filter: (&'static str, &'static [&'static str]),
    /// File name suggested for exports.
    pub suggested: &'static str,
}

impl IeAction {
    pub(crate) fn export(&self) -> bool {
        self.key.starts_with("export")
    }
    pub(crate) fn folder_input(&self) -> bool {
        self.filter.1.is_empty()
    }
    pub(crate) fn filters(&self) -> Vec<crate::FileFilter> {
        if self.folder_input() {
            return Vec::new();
        }
        vec![
            crate::FileFilter::new(self.filter.0, self.filter.1),
            crate::FileFilter::new("All files", &["*"]),
        ]
    }
    /// What the folder list is called on the second page.
    pub(crate) fn folder_prompt(&self) -> &'static str {
        match (self.export(), self.pick) {
            (true, _) => "Select the folder to export from:",
            (false, Pick::Kinds(k)) if k.contains(&FolderKind::Calendar) => "Import into this calendar:",
            (false, Pick::Kinds(_)) => "Import into this contacts folder:",
            (false, _) => "Import into this folder:",
        }
    }
}

const MAIL: Pick = Pick::Mail;
const CALENDAR: Pick = Pick::Kinds(&[FolderKind::Calendar]);
const CALENDAR_OR_TASKS: Pick = Pick::Kinds(&[FolderKind::Calendar, FolderKind::Tasks]);
const CONTACTS: Pick = Pick::Kinds(&[FolderKind::Contacts]);

/// Wizard actions, imports first (labels name the file types; formats from public specs).
pub(crate) const IE_ACTIONS: [IeAction; 11] = [
    IeAction {
        key: "importEml",
        label: "Import an Internet message file (.eml)",
        desc: "Adds one message saved as an .eml file (RFC 5322) to a folder.",
        pick: MAIL,
        filter: ("Internet message", &["eml"]),
        suggested: "",
    },
    IeAction {
        key: "importMsg",
        label: "Import a .msg message file",
        desc: "Adds one message saved as a .msg file to a folder.",
        pick: MAIL,
        filter: ("Message file", &["msg"]),
        suggested: "",
    },
    IeAction {
        key: "importMbox",
        label: "Import an MBOX mailbox file",
        desc: "Adds every message in an MBOX file (Thunderbird, Apple Mail, mutt…) to a folder.",
        pick: MAIL,
        filter: ("MBOX mailbox", &["mbox", "mbx"]),
        suggested: "",
    },
    IeAction {
        key: "importMaildir",
        label: "Import a Maildir folder",
        desc: "Adds the messages in a Maildir directory (cur/ and new/) to a folder.",
        pick: MAIL,
        filter: ("", &[]),
        suggested: "",
    },
    IeAction {
        key: "importIcs",
        label: "Import an iCalendar (.ics) file",
        desc: "Adds the events (and tasks) of an iCalendar file (RFC 5545) to a calendar.",
        pick: CALENDAR,
        filter: ("iCalendar", &["ics", "ical", "ifb", "vcs"]),
        suggested: "",
    },
    IeAction {
        key: "importVcf",
        label: "Import a vCard file (.vcf)",
        desc: "Adds the contacts of a vCard file (RFC 6350) to a contacts folder.",
        pick: CONTACTS,
        filter: ("vCard", &["vcf", "vcard"]),
        suggested: "",
    },
    IeAction {
        key: "importCsv",
        label: "Import contacts from a CSV file",
        desc: "Adds contacts from a comma-separated values file; columns are matched by their names.",
        pick: CONTACTS,
        filter: ("Comma-separated values", &["csv"]),
        suggested: "",
    },
    IeAction {
        key: "exportMbox",
        label: "Export a folder to an MBOX file",
        desc: "Writes every message in a folder to one MBOX file other mail programs can open.",
        pick: MAIL,
        filter: ("MBOX mailbox", &["mbox"]),
        suggested: "export.mbox",
    },
    IeAction {
        key: "exportIcs",
        label: "Export a calendar to an iCalendar (.ics) file",
        desc: "Writes the events of a calendar (or the tasks of a task list) to an iCalendar file.",
        pick: CALENDAR_OR_TASKS,
        filter: ("iCalendar", &["ics"]),
        suggested: "calendar.ics",
    },
    IeAction {
        key: "exportVcf",
        label: "Export contacts to a vCard file (.vcf)",
        desc: "Writes the contacts of a folder to one vCard 4.0 file.",
        pick: CONTACTS,
        filter: ("vCard", &["vcf"]),
        suggested: "contacts.vcf",
    },
    IeAction {
        key: "exportCsv",
        label: "Export contacts to a CSV file",
        desc: "Writes the contacts of a folder to a comma-separated values file spreadsheets can open.",
        pick: CONTACTS,
        filter: ("Comma-separated values", &["csv"]),
        suggested: "contacts.csv",
    },
];

/// A folder the action can use: `current` when it fits, else the current account's folder of the right
/// kind (Calendar, Contacts), else the first one that fits.
pub(crate) fn ie_folder(app: &App, action: &IeAction, current: Option<u64>) -> Option<u64> {
    let fits = |f: &mailcraft_model::Folder| match action.pick {
        Pick::Mail => f.kind == FolderKind::Mail,
        Pick::Kinds(k) => k.contains(&f.kind),
        Pick::Parent | Pick::Any => true,
    };
    let mb = &app.s.mailbox;
    if let Some(f) = current.and_then(|c| mb.folder(FolderId(c))).filter(|f| fits(f)) {
        return Some(f.id.0);
    }
    let role = match action.pick {
        Pick::Kinds(k) if k.contains(&FolderKind::Calendar) => Some(FolderRole::Calendar),
        Pick::Kinds(k) if k.contains(&FolderKind::Contacts) => Some(FolderRole::Contacts),
        _ => Some(FolderRole::Inbox),
    };
    app.s
        .current_account()
        .zip(role)
        .and_then(|(a, r)| mb.role_folder(a, r))
        .filter(|f| mb.folder(*f).is_some_and(fits))
        .or_else(|| mb.folders.values().find(|f| fits(f)).map(|f| f.id))
        .map(|f| f.0)
}

/// Path of a folder for labels ("Mailbox › Inbox"; empty when none).
pub(crate) fn folder_label(app: &App, id: Option<u64>) -> String {
    folder_path(app, id)
}

/// What an import or export did, for the closing message.
pub(crate) fn ie_result_text(
    app: &App,
    action: &IeAction,
    v: &Value,
    path: &str,
    folder: Option<u64>,
) -> String {
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let what = match action.pick {
        Pick::Kinds(k) if k.contains(&FolderKind::Contacts) => "contact(s)",
        Pick::Kinds(_) => "item(s)",
        _ => "message(s)",
    };
    if action.export() {
        return format!("Exported {} {what} to {path}.", n("exported"));
    }
    let into = folder_path(app, v.get("folder").and_then(Value::as_u64).or(folder));
    let mut text = if action.key == "importIcs" {
        format!("Imported {} appointment(s) and {} task(s) into {into}.", n("appointments"), n("tasks"))
    } else {
        format!("Imported {} {what} into {into}.", n("imported"))
    };
    let unmapped: Vec<&str> = v
        .get("unmappedColumns")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).take(20).collect())
        .unwrap_or_default();
    if !unmapped.is_empty() {
        text.push_str(&format!(" Columns not imported: {}.", unmapped.join(", ")));
    }
    text
}

#[derive(Clone, Default)]
struct IeState {
    page: usize,
    action: usize,
    path: String,
    folder: Option<u64>,
    error: String,
}

fn import_export(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |a: &App| IeState { folder: Some(a.s.selection.folder.0), ..Default::default() };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Import and Export Wizard", 520.0, |ui, app| {
            let Some(action) = IE_ACTIONS.get(st.action).copied() else { return Out::Close };
            let export = action.export();
            if st.page == 0 {
                ui.label("Choose an action to perform:");
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, app.theme.separator))
                    .inner_margin(4.0)
                    .show(ui, |ui| {
                        ui.set_min_height(120.0);
                        ui.set_min_width(ui.available_width());
                        egui::ScrollArea::vertical().id_salt(("ie-actions", uid)).max_height(240.0).show(
                            ui,
                            |ui| {
                                for (i, a) in IE_ACTIONS.iter().enumerate() {
                                    if ui.selectable_label(st.action == i, a.label).clicked()
                                        && st.action != i
                                    {
                                        st.action = i;
                                        st.path.clear();
                                    }
                                }
                            },
                        );
                    });
                ui.label(RichText::new("Description").strong());
                muted(ui, app, action.desc);
            } else {
                ui.label(RichText::new(action.label).strong());
                ui.label(if export {
                    "Save exported file as:"
                } else if action.folder_input() {
                    "Maildir folder to import:"
                } else {
                    "File to import:"
                });
                if st.path.is_empty() {
                    st.path = if export {
                        format!("{}/{}", home(), action.suggested)
                    } else {
                        format!("{}/", home())
                    };
                }
                ui.horizontal(|ui| {
                    let browse = common::browse_button(ui, app);
                    ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(ui.available_width()));
                    if browse {
                        let title = action.label;
                        let picked = if export {
                            common::browse_save(app, title, &action.filters(), &st.path)
                        } else if action.folder_input() {
                            common::browse_folder(app, title, &st.path)
                        } else {
                            common::browse_open(app, title, &action.filters(), &st.path)
                        };
                        if let Some(p) = picked {
                            st.path = p;
                        }
                    }
                });
                ui.label(action.folder_prompt());
                folder_tree(ui, app, &mut st.folder, action.pick, ("ie", uid), 180.0);
            }
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            let mut out = Out::Keep;
            footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    out = Out::Close;
                }
                if st.page == 0 {
                    if primary(ui, app, true, "Next >") {
                        st.folder = ie_folder(app, &action, st.folder);
                        st.page = 1;
                    }
                } else {
                    let ready = !st.path.trim().is_empty() && st.folder.is_some();
                    if primary(ui, app, ready, "Finish") {
                        let res = app.run(
                            "file.openExport",
                            json!({"action": action.key, "path": st.path.trim(), "folder": st.folder}),
                        );
                        match res {
                            Some(v) => {
                                let text = ie_result_text(app, &action, &v, st.path.trim(), st.folder);
                                out = Out::close_open(
                                    "message",
                                    json!({"title": "Import and Export", "text": text}),
                                );
                            }
                            None => st.error = last_error(app),
                        }
                    }
                }
                if btn_en(ui, st.page > 0, "< Back") {
                    st.page = 0;
                    st.error.clear();
                }
            });
            out
        })
    })
}

// ---------------------------------------------------------------------------------------------------------
// Save As / Save Attachments

/// Save As file types: engine type, label, extensions.
pub(crate) const SAVE_TYPES: [(&str, &str, &[&str]); 4] = [
    ("eml", "Internet message (*.eml)", &["eml"]),
    ("msg", "Message file (*.msg)", &["msg"]),
    ("txt", "Text only (*.txt)", &["txt"]),
    ("html", "HTML (*.htm; *.html)", &["html", "htm"]),
];

/// Index of the Save As type a path's extension names (default: .eml).
pub(crate) fn save_type_of(path: &str) -> usize {
    let ext = std::path::Path::new(path.trim())
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    SAVE_TYPES.iter().position(|(_, _, exts)| exts.contains(&ext.as_str())).unwrap_or(0)
}

/// `path` with its extension replaced by the first one of Save As type `idx` (folders are left alone).
pub(crate) fn with_save_ext(path: &str, idx: usize) -> String {
    let p = std::path::Path::new(path.trim());
    let Some(ext) = SAVE_TYPES.get(idx).and_then(|t| t.2.first()) else { return path.to_owned() };
    if path.trim().is_empty() || p.is_dir() || path.ends_with('/') || path.ends_with('\\') {
        return path.to_owned();
    }
    p.with_extension(ext).display().to_string()
}

/// File type filters for a Save As dialog: the chosen type first, then the others.
pub(crate) fn save_filters(idx: usize) -> Vec<crate::FileFilter> {
    let mut out = Vec::new();
    if let Some((_, label, exts)) = SAVE_TYPES.get(idx) {
        out.push(crate::FileFilter::new(label, exts));
    }
    for (i, (_, label, exts)) in SAVE_TYPES.iter().enumerate() {
        if i != idx {
            out.push(crate::FileFilter::new(label, exts));
        }
    }
    out
}

#[derive(Clone, Default)]
struct PathState {
    path: String,
    kind: usize,
    error: String,
}

fn save_as(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let suggested = if d.str("suggested").is_empty() { "Message.eml".to_owned() } else { d.str("suggested") };
    with_state(
        ctx,
        app,
        uid,
        |_: &App| PathState {
            path: format!("{}/{suggested}", home()),
            kind: save_type_of(&suggested),
            ..Default::default()
        },
        |st, app| {
            window(ctx, app, d, "Save As", 520.0, |ui, app| {
                let l = ui.label("File name:");
                ui.horizontal(|ui| {
                    let browse = common::browse_button(ui, app);
                    ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(ui.available_width()))
                        .labelled_by(l.id);
                    if browse
                        && let Some(p) = common::browse_save(app, "Save As", &save_filters(st.kind), &st.path)
                    {
                        st.kind = save_type_of(&p);
                        st.path = p;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Save as type:");
                    let labels: Vec<&str> = SAVE_TYPES.iter().map(|t| t.1).collect();
                    if common::combo_idx(ui, ("saveas-type", uid), &mut st.kind, &labels, 240.0) {
                        st.path = with_save_ext(&st.path, st.kind);
                    }
                });
                muted(ui, app, "A folder path saves the message there under its subject.");
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        return Out::Close;
                    }
                    if primary(ui, app, !st.path.trim().is_empty(), "Save") {
                        let ids: Vec<u64> = d.u64("item").into_iter().collect();
                        let kind = SAVE_TYPES.get(st.kind).map_or("eml", |t| t.0);
                        let p = json!({"path": st.path.trim(), "ids": ids, "type": kind});
                        if app.run("file.saveAs", p).is_some() {
                            return Out::Close;
                        }
                        st.error = last_error(app);
                    }
                    Out::Keep
                })
            })
        },
    )
}

fn save_attachments(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let files = d.arr("files");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| PathState { path: home(), ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Save All Attachments", 440.0, |ui, app| {
                ui.label("Attachments:");
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, app.theme.separator))
                    .inner_margin(4.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().id_salt(("att", uid)).max_height(150.0).show(ui, |ui| {
                            for f in &files {
                                let name = f.get("filename").and_then(Value::as_str).unwrap_or("attachment");
                                let size = f.get("size").and_then(Value::as_u64).unwrap_or(0);
                                ui.horizontal(|ui| {
                                    let (r, _) =
                                        ui.allocate_exact_size(egui::Vec2::splat(16.0), egui::Sense::hover());
                                    app.icons.paint(ui, r, "paperclip", app.theme.text_muted);
                                    ui.label(name);
                                    ui.label(
                                        RichText::new(common::size_text(size)).color(app.theme.text_muted),
                                    );
                                });
                            }
                        });
                    });
                ui.label("Save in folder:");
                ui.horizontal(|ui| {
                    let browse = common::browse_button(ui, app);
                    ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(ui.available_width()));
                    if browse && let Some(p) = common::browse_folder(app, "Save All Attachments", &st.path) {
                        st.path = p;
                    }
                });
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                match ok_cancel(ui, app, !st.path.trim().is_empty()) {
                    Some(true) => {
                        let mut ids: Vec<u64> =
                            files.iter().filter_map(|f| f.get("item").and_then(Value::as_u64)).collect();
                        ids.dedup();
                        let mut p = json!({"dir": st.path.trim()});
                        if !ids.is_empty()
                            && let Some(o) = p.as_object_mut()
                        {
                            o.insert("ids".into(), json!(ids));
                        }
                        match app.run("file.saveAttachments", p) {
                            Some(v) => {
                                let n = v.get("saved").and_then(Value::as_array).map_or(0, Vec::len);
                                Out::close_open(
                                    "message",
                                    json!({"title": "Save Attachments", "text": format!("Saved {n} attachment(s) to {}.", st.path.trim())}),
                                )
                            }
                            None => {
                                st.error = last_error(app);
                                Out::Keep
                            }
                        }
                    }
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}
