//! File-side dialogs: Account Settings (+ Add Account form), Mailbox Cleanup, the Import and Export Wizard,
//! Save As, Save Attachments and Print (with a preview shared by the backstage Print page).

use egui::{Context, Id, RichText, Ui};
use serde_json::{Value, json};

use super::common::{
    self, Dlg, Out, Pick, btn, btn_en, folder_path, folder_tree, footer, muted, ok_cancel, primary, window,
    with_state,
};
use super::folders::last_error;
use crate::App;
use mailcraft_model::{AccountKind, Doc, FolderId, ItemId, MailItem};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "accountSettings" => account_settings(ctx, app, d),
        "addAccount" => add_account(ctx, app, d),
        "mailboxCleanup" => cleanup(ctx, app, d),
        "importExport" => import_export(ctx, app, d),
        "saveAs" => save_as(ctx, app, d),
        "saveAttachments" => save_attachments(ctx, app, d),
        "print" => print(ctx, app, d),
        _ => return None,
    })
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".into())
}

fn kind_label(k: AccountKind) -> &'static str {
    match k {
        AccountKind::Local => "Data file (local)",
        AccountKind::Imap => "IMAP/SMTP",
        AccountKind::Jmap => "JMAP",
        AccountKind::Exchange => "Exchange",
    }
}

// ---------------------------------------------------------------------------------------------------------
// Account Settings

#[derive(Clone, Default)]
struct AcctState {
    tab: usize,
    selected: Option<u32>,
    data_name: String,
}

const ACCT_TABS: [&str; 4] = ["Email", "Data Files", "RSS Feeds", "Address Books"];

fn account_settings(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let first = d
        .get("account")
        .and_then(|a| a.get("id"))
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok());
    with_state(
        ctx,
        app,
        uid,
        |_: &App| AcctState { selected: first, ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Account Settings", 640.0, |ui, app| {
                ui.horizontal(|ui| {
                    for (i, t) in ACCT_TABS.iter().enumerate() {
                        if ui.selectable_label(st.tab == i, *t).clicked() {
                            st.tab = i;
                        }
                    }
                });
                ui.separator();
                let accounts = app.s.mailbox.accounts.clone();
                let pending: Vec<Value> = common::pref(ctx, "accounts.pending")
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                let sel = st.selected.and_then(|id| accounts.iter().find(|a| a.id.0 == id)).cloned();
                let mut out = Out::Keep;
                match st.tab {
                    0 => {
                        muted(
                            ui,
                            app,
                            "Add, change or remove the accounts MailCraft reads and sends mail with.",
                        );
                        ui.horizontal(|ui| {
                        if ui.button("New...").clicked() {
                            out = Out::open("addAccount", json!({}));
                        }
                        if ui.add_enabled(sel.is_some(), egui::Button::new("Change...")).clicked()
                            && let Some(a) = &sel
                        {
                            out = Out::open("addAccount", json!({"name": a.sender_name, "address": a.address, "change": a.id.0}));
                        }
                        ui.add_enabled(false, egui::Button::new("Set as Default"))
                            .on_disabled_hover_text("The default account is the first data file.");
                        let removable = sel.as_ref().is_some_and(|a| !a.is_default);
                        if ui.add_enabled(removable, egui::Button::new("Remove")).clicked()
                            && let Some(a) = &sel
                        {
                            out = Out::open("confirm", json!({"title": "Account Settings", "text": format!("Remove \"{}\" and everything stored in it from this profile?", a.display_name), "command": "file.closeDataFile", "params": {"store": a.id.0}}));
                        }
                    });
                        account_list(ui, app, st, &accounts, &pending, true);
                    }
                    1 => {
                        muted(ui, app, "Data files hold your folders and items on this computer.");
                        ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut st.data_name).hint_text("New data file name").desired_width(180.0));
                        if ui.button("Add...").clicked() {
                            let name = st.data_name.trim().to_owned();
                            app.run("file.newDataFile", if name.is_empty() { json!({}) } else { json!({"name": name}) });
                            st.data_name.clear();
                        }
                        let removable = sel.as_ref().is_some_and(|a| !a.is_default);
                        if ui.add_enabled(removable, egui::Button::new("Remove")).clicked()
                            && let Some(a) = &sel
                        {
                            out = Out::open("confirm", json!({"title": "Account Settings", "text": format!("Close the data file \"{}\"?", a.display_name), "command": "file.closeDataFile", "params": {"store": a.id.0}}));
                        }
                        if ui.add_enabled(app.s.store.is_some(), egui::Button::new("Open File Location...")).clicked() {
                            app.run("file.openFileLocation", json!({}));
                        }
                    });
                        account_list(ui, app, st, &accounts, &[], false);
                        if let Some(st) = &app.s.store {
                            muted(ui, app, &format!("Location: {}", st.dir.display()));
                        }
                    }
                    _ => {
                        muted(ui, app, "Nothing is set up here yet.");
                        ui.add_space(160.0);
                    }
                }
                if !matches!(out, Out::Keep) {
                    return out;
                }
                footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
            })
        },
    )
}

fn account_list(
    ui: &mut Ui,
    app: &App,
    st: &mut AcctState,
    accounts: &[mailcraft_model::Account],
    pending: &[Value],
    email: bool,
) {
    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(ui, |ui| {
        ui.set_min_height(200.0);
        egui::Grid::new(("accts", email)).num_columns(3).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
            ui.label(RichText::new("Name").strong());
            ui.label(RichText::new("Type").strong());
            ui.label(RichText::new("").strong());
            ui.end_row();
            for a in accounts {
                let title = if email { common::account_title(a) } else { a.display_name.clone() };
                if ui.selectable_label(st.selected == Some(a.id.0), title).clicked() {
                    st.selected = Some(a.id.0);
                }
                ui.label(kind_label(a.kind));
                ui.label(if a.is_default { "Default" } else { "" });
                ui.end_row();
            }
            for p in pending {
                ui.label(p.get("address").and_then(Value::as_str).unwrap_or_default());
                ui.label("IMAP/SMTP (not connected)");
                ui.label("");
                ui.end_row();
            }
        });
    });
}

#[derive(Clone, Default)]
struct NewAcct {
    name: String,
    address: String,
    kind: usize,
    in_host: String,
    in_port: u16,
    in_sec: usize,
    out_host: String,
    out_port: u16,
    out_sec: usize,
    user: String,
    password: String,
    smtp_auth: bool,
    error: String,
}

const SECURITY: [&str; 3] = ["SSL/TLS", "STARTTLS", "None"];

fn add_account(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |_: &App| NewAcct {
        name: d.str("name"),
        address: d.str("address"),
        in_port: 993,
        out_port: 587,
        out_sec: 1,
        smtp_auth: true,
        ..Default::default()
    };
    with_state(ctx, app, uid, init, |st, app| {
        let title = if d.get("change").is_some() { "Change Account" } else { "Add Account" };
        window(ctx, app, d, title, 520.0, |ui, app| {
            common::heading(ui, app, "User Information");
            egui::Grid::new(("na1", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Your Name:");
                ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(300.0));
                ui.end_row();
                ui.label("Email Address:");
                if ui.add(egui::TextEdit::singleline(&mut st.address).desired_width(300.0)).changed()
                    && st.user.is_empty()
                {
                    st.user.clone_from(&st.address);
                }
                ui.end_row();
            });
            common::heading(ui, app, "Server Information");
            egui::Grid::new(("na2", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Account Type:");
                common::combo_idx(ui, ("natype", uid), &mut st.kind, &["IMAP", "POP3"], 120.0);
                ui.end_row();
                ui.label("Incoming mail server:");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut st.in_host)
                            .hint_text("imap.example.com")
                            .desired_width(200.0),
                    );
                    ui.add(egui::DragValue::new(&mut st.in_port).range(1..=65535));
                    common::combo_idx(ui, ("nainsec", uid), &mut st.in_sec, &SECURITY, 90.0);
                });
                ui.end_row();
                ui.label("Outgoing mail server (SMTP):");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut st.out_host)
                            .hint_text("smtp.example.com")
                            .desired_width(200.0),
                    );
                    ui.add(egui::DragValue::new(&mut st.out_port).range(1..=65535));
                    common::combo_idx(ui, ("naoutsec", uid), &mut st.out_sec, &SECURITY, 90.0);
                });
                ui.end_row();
            });
            common::heading(ui, app, "Logon Information");
            egui::Grid::new(("na3", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("User Name:");
                ui.add(egui::TextEdit::singleline(&mut st.user).desired_width(300.0));
                ui.end_row();
                ui.label("Password:");
                ui.add(egui::TextEdit::singleline(&mut st.password).password(true).desired_width(300.0));
                ui.end_row();
            });
            ui.checkbox(&mut st.smtp_auth, "My outgoing server (SMTP) requires authentication");
            muted(
                ui,
                app,
                "Network accounts connect once MailCraft's sync engine is available; until then the settings are kept for this session.",
            );
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            let valid =
                st.address.contains('@') && !st.in_host.trim().is_empty() && !st.out_host.trim().is_empty();
            match ok_cancel(ui, app, valid) {
                Some(true) => {
                    // No engine command adds network accounts yet: keep the settings (never the password) in
                    // UI state so Account Settings lists them.
                    let mut prefs = common::prefs(ctx);
                    let list = prefs.values.entry("accounts.pending").or_insert_with(|| json!([]));
                    let kind = ["imap", "pop3"].get(st.kind).copied().unwrap_or("imap");
                    let in_sec = SECURITY.get(st.in_sec).copied().unwrap_or("");
                    let out_sec = SECURITY.get(st.out_sec).copied().unwrap_or("");
                    if let Some(a) = list.as_array_mut() {
                        a.retain(|x| x.get("address").and_then(Value::as_str) != Some(st.address.trim()));
                        a.push(json!({
                            "name": st.name.trim(), "address": st.address.trim(), "type": kind,
                            "incoming": {"host": st.in_host.trim(), "port": st.in_port, "security": in_sec},
                            "outgoing": {"host": st.out_host.trim(), "port": st.out_port, "security": out_sec, "auth": st.smtp_auth},
                            "user": st.user.trim(),
                        }));
                    }
                    common::set_prefs(ctx, prefs);
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
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
                    ui.label("Move old items to the archive file.");
                    if btn(ui, "AutoArchive") {
                        app.run("folder.autoArchiveSettings", json!({}));
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

const IE_ACTIONS: [(&str, &str, &str); 4] = [
    (
        "importEml",
        "Import an Internet message file (.eml)",
        "Adds one message saved as an .eml file to a folder.",
    ),
    (
        "importMbox",
        "Import an MBOX mailbox file",
        "Adds every message in an MBOX file (Thunderbird, Apple Mail, mutt…) to a folder.",
    ),
    (
        "importMaildir",
        "Import a Maildir folder",
        "Adds the messages in a Maildir directory (cur/ and new/) to a folder.",
    ),
    (
        "exportMbox",
        "Export a folder to an MBOX file",
        "Writes every message in a folder to one MBOX file other mail programs can open.",
    ),
];

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
        window(ctx, app, d, "Import and Export Wizard", 480.0, |ui, app| {
            let Some((key, label, desc)) = IE_ACTIONS.get(st.action).copied() else { return Out::Close };
            let export = key.starts_with("export");
            if st.page == 0 {
                ui.label("Choose an action to perform:");
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, app.theme.separator))
                    .inner_margin(4.0)
                    .show(ui, |ui| {
                        ui.set_min_height(120.0);
                        for (i, (_, l, _)) in IE_ACTIONS.iter().enumerate() {
                            if ui.selectable_label(st.action == i, *l).clicked() {
                                st.action = i;
                            }
                        }
                    });
                ui.label(RichText::new("Description").strong());
                muted(ui, app, desc);
            } else {
                ui.label(RichText::new(label).strong());
                ui.label(if export {
                    "Save exported file as:"
                } else if key == "importMaildir" {
                    "Maildir folder to import:"
                } else {
                    "File to import:"
                });
                if st.path.is_empty() {
                    st.path = if export { format!("{}/export.mbox", home()) } else { format!("{}/", home()) };
                }
                ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(f32::INFINITY));
                ui.label(if export {
                    "Select the folder to export from:"
                } else {
                    "Import into this folder:"
                });
                folder_tree(ui, app, &mut st.folder, Pick::Mail, ("ie", uid), 180.0);
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
                        st.page = 1;
                    }
                } else {
                    let ready = !st.path.trim().is_empty() && st.folder.is_some();
                    if primary(ui, app, ready, "Finish") {
                        let res = app.run(
                            "file.openExport",
                            json!({"action": key, "path": st.path.trim(), "folder": st.folder}),
                        );
                        match res {
                            Some(v) => {
                                let text = if export {
                                    format!(
                                        "Exported {} message(s) to {}.",
                                        v.get("exported").and_then(Value::as_u64).unwrap_or(0),
                                        st.path.trim()
                                    )
                                } else {
                                    format!(
                                        "Imported {} message(s) into {}.",
                                        v.get("imported").and_then(Value::as_u64).unwrap_or(0),
                                        folder_path(app, st.folder)
                                    )
                                };
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

#[derive(Clone, Default)]
struct PathState {
    path: String,
    error: String,
}

fn save_as(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let suggested = if d.str("suggested").is_empty() { "Message.eml".to_owned() } else { d.str("suggested") };
    with_state(
        ctx,
        app,
        uid,
        |_: &App| PathState { path: format!("{}/{suggested}", home()), ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Save As", 480.0, |ui, app| {
                ui.label("File name:");
                ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(f32::INFINITY));
                muted(
                    ui,
                    app,
                    "Save as type: Internet message (*.eml). A folder path saves under the message subject.",
                );
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        return Out::Close;
                    }
                    if primary(ui, app, !st.path.trim().is_empty(), "Save") {
                        let ids: Vec<u64> = d.u64("item").into_iter().collect();
                        if app.run("file.saveAs", json!({"path": st.path.trim(), "ids": ids})).is_some() {
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
                ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(f32::INFINITY));
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

// ---------------------------------------------------------------------------------------------------------
// Print

#[derive(Clone, Default)]
struct PrintState {
    style: usize,
    copies: u32,
    path: String,
    page: usize,
    error: String,
}

pub(crate) const STYLES: [&str; 2] = ["Memo Style", "Table Style"];

fn print(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let ids = d.ids();
    let table = d.str("style") == "table";
    let init = |_: &App| PrintState {
        style: usize::from(table),
        copies: 1,
        path: format!("{}/print.html", home()),
        ..Default::default()
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Print", 720.0, |ui, app| {
            let mut out = Out::Keep;
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(240.0);
                    common::heading(ui, app, "Printer");
                    ui.label("Save as HTML file (open it in a browser to print)");
                    ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(230.0));
                    common::heading(ui, app, "Print style");
                    for (i, s) in STYLES.iter().enumerate() {
                        ui.radio_value(&mut st.style, i, *s);
                    }
                    common::heading(ui, app, "Copies");
                    ui.horizontal(|ui| {
                        ui.label("Number of copies:");
                        ui.add(egui::DragValue::new(&mut st.copies).range(1..=99));
                    });
                    muted(ui, app, &format!("{} item(s) selected.", ids.len()));
                    if !st.error.is_empty() {
                        ui.colored_label(app.theme.danger, &st.error);
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    print_preview(ui, app, &ids, st.style == 1, &mut st.page, ("pp", uid), 380.0);
                });
            });
            footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    out = Out::Close;
                }
                if primary(ui, app, !ids.is_empty(), "Print") {
                    match print_to_file(app, &ids, st.style == 1, st.copies, st.path.trim()) {
                        Ok(path) => {
                            out = Out::close_open(
                                "message",
                                json!({"title": "Print", "text": format!("Saved the printout to {path}.")}),
                            )
                        }
                        Err(e) => st.error = e,
                    }
                }
            });
            out
        })
    })
}

/// Cached doc + layout for the preview of one item.
#[derive(Clone, Default)]
struct PreviewCache {
    id: u64,
    doc: Doc,
    view: crate::body::BodyView,
}

pub(crate) fn item_doc(m: &MailItem) -> Doc {
    match (&m.body.html, &m.body.text) {
        (Some(h), _) => mailcraft_html::html_to_doc(h),
        (None, Some(t)) => Doc::from_plain_text(t),
        (None, None) => Doc::from_plain_text(&m.preview),
    }
}

fn header_rows(app: &App, m: &MailItem) -> Vec<(&'static str, String)> {
    let people = |l: &[mailcraft_model::Address]| {
        l.iter().map(mailcraft_model::Address::to_display_string).collect::<Vec<_>>().join("; ")
    };
    let when = m.sent.unwrap_or(m.received);
    let mut rows = vec![
        ("From", m.from.as_ref().map(mailcraft_model::Address::to_display_string).unwrap_or_default()),
        ("Sent", mailcraft_model::ListTime::long(when, app.s.tz_offset_min)),
        ("To", people(&m.to)),
    ];
    if !m.cc.is_empty() {
        rows.push(("Cc", people(&m.cc)));
    }
    rows.push(("Subject", m.subject.clone()));
    if !m.attachments.is_empty() {
        rows.push((
            "Attachments",
            m.attachments.iter().map(|a| a.filename.clone()).collect::<Vec<_>>().join("; "),
        ));
    }
    rows
}

/// Print preview of `ids` (one page per item in Memo style; a single table in Table style).
pub(crate) fn print_preview(
    ui: &mut Ui,
    app: &App,
    ids: &[u64],
    table: bool,
    page: &mut usize,
    salt: impl std::hash::Hash + std::fmt::Debug + Copy,
    height: f32,
) {
    let items: Vec<&MailItem> = ids.iter().filter_map(|i| app.s.mailbox.mail.get(&ItemId(*i))).collect();
    if items.is_empty() {
        muted(ui, app, "Select a message to see a preview.");
        return;
    }
    let pages = if table { 1 } else { items.len() };
    *page = (*page).min(pages.saturating_sub(1));
    ui.horizontal(|ui| {
        if ui.add_enabled(*page > 0, egui::Button::new("◀")).on_hover_text("Previous Page").clicked() {
            *page = page.saturating_sub(1);
        }
        ui.label(format!("Page {} of {pages}", *page + 1));
        if ui.add_enabled(*page + 1 < pages, egui::Button::new("▶")).on_hover_text("Next Page").clicked() {
            *page += 1;
        }
    });
    let paper = egui::Color32::WHITE;
    let ink = egui::Color32::from_rgb(0x20, 0x20, 0x20);
    egui::Frame::new()
        .fill(paper)
        .stroke(egui::Stroke::new(1.0, app.theme.separator))
        .inner_margin(18.0)
        .show(ui, |ui| {
            ui.visuals_mut().override_text_color = Some(ink);
            egui::ScrollArea::vertical()
                .id_salt(salt)
                .max_height(height)
                .min_scrolled_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let me =
                        app.s.mailbox.accounts.first().map(|a| a.display_name.clone()).unwrap_or_default();
                    ui.label(RichText::new(me).strong().size(15.0).color(ink));
                    ui.separator();
                    if table {
                        egui::Grid::new((salt, "t")).num_columns(4).striped(true).show(ui, |ui| {
                            for h in ["From", "Subject", "Received", "Size"] {
                                ui.label(RichText::new(h).strong().color(ink));
                            }
                            ui.end_row();
                            for m in &items {
                                ui.label(m.from.as_ref().map(|f| f.display().to_owned()).unwrap_or_default());
                                ui.label(&m.subject);
                                ui.label(mailcraft_model::ListTime::short_date_time(
                                    m.received,
                                    app.s.tz_offset_min,
                                ));
                                ui.label(common::size_text(m.size));
                                ui.end_row();
                            }
                        });
                        return;
                    }
                    let Some(m) = items.get(*page) else { return };
                    egui::Grid::new((salt, "h")).num_columns(2).show(ui, |ui| {
                        for (k, v) in header_rows(app, m) {
                            ui.label(RichText::new(format!("{k}:")).strong().color(ink));
                            ui.label(RichText::new(v).color(ink));
                            ui.end_row();
                        }
                    });
                    ui.add_space(8.0);
                    let id = Id::new(("mc-print-preview", salt));
                    let mut cache = ui.ctx().data_mut(|d| d.get_temp::<PreviewCache>(id)).unwrap_or_default();
                    if cache.id != m.id.0 {
                        cache = PreviewCache {
                            id: m.id.0,
                            doc: item_doc(m),
                            view: crate::body::BodyView::default(),
                        };
                    }
                    crate::body::show_doc(ui, &mut cache.view, &cache.doc, false);
                    ui.ctx().data_mut(|d| d.insert_temp(id, cache));
                });
        });
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Writes a printable HTML file for `ids`; returns the path written.
pub(crate) fn print_to_file(
    app: &App,
    ids: &[u64],
    table: bool,
    copies: u32,
    path: &str,
) -> Result<String, String> {
    let items: Vec<&MailItem> = ids.iter().filter_map(|i| app.s.mailbox.mail.get(&ItemId(*i))).collect();
    if items.is_empty() {
        return Err("Nothing to print: select a message first.".into());
    }
    if path.is_empty() {
        return Err("Enter a file name for the printout.".into());
    }
    let mut html = String::from(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MailCraft print</title><style>body{font-family:sans-serif;margin:2em}table.h td{padding:2px 8px 2px 0;vertical-align:top}th,td{text-align:left}.page{page-break-after:always}</style></head><body>",
    );
    for _ in 0..copies.clamp(1, 99) {
        if table {
            html.push_str(
                "<table class=\"page\"><tr><th>From</th><th>Subject</th><th>Received</th><th>Size</th></tr>",
            );
            for m in &items {
                html.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    esc(m.from.as_ref().map(|f| f.display().to_owned()).unwrap_or_default().as_str()),
                    esc(&m.subject),
                    esc(&mailcraft_model::ListTime::short_date_time(m.received, app.s.tz_offset_min)),
                    esc(&common::size_text(m.size)),
                ));
            }
            html.push_str("</table>");
        } else {
            for m in &items {
                html.push_str("<div class=\"page\"><table class=\"h\">");
                for (k, v) in header_rows(app, m) {
                    html.push_str(&format!("<tr><td><b>{k}:</b></td><td>{}</td></tr>", esc(&v)));
                }
                html.push_str("</table><hr>");
                html.push_str(&mailcraft_html::doc_to_html(&item_doc(m)));
                html.push_str("</div>");
            }
        }
    }
    html.push_str("</body></html>");
    std::fs::write(path, html).map_err(|e| format!("Could not write {path}: {e}"))?;
    Ok(path.to_owned())
}
