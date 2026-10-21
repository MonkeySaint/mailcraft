//! Folder pickers (Move/Copy Items, Move/Copy Folder, Go to Folder, Archive, Always Move…), Create New
//! Folder, Rename, Properties (its AutoArchive tab is `super::archive`'s form), New Search Folder, Recover
//! Deleted Items, and the generic Yes/No confirmation (plan/reference/dialogs.md: new-folder,
//! new-search-folder).

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{
    self, Dlg, Out, Pick, btn, btn_en, child, folder_tree, footer, muted, ok_cancel, primary, send_reply,
    take_replies, window, with_state,
};
use crate::App;
use mailcraft_engine::search_index::SEARCH_FOLDER_TEMPLATES;
use mailcraft_model::FolderId;

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "moveToFolder"
        | "copyToFolder"
        | "archiveFolder"
        | "alwaysMoveConversation"
        | "selectFolder"
        | "moveFolder"
        | "copyFolder"
        | "goToFolder"
        | "pickFolder" => picker(ctx, app, d),
        "createFolder" => create_folder(ctx, app, d),
        "renameFolder" => rename_folder(ctx, app, d),
        "folderProperties" => properties(ctx, app, d),
        "newSearchFolder" => new_search_folder(ctx, app, d),
        "recoverDeletedItems" => recover(ctx, app, d),
        "confirm" => confirm(ctx, app, d),
        "dataFileProperties" => data_file_properties(ctx, app, d),
        "openDataFile" => open_data_file(ctx, app, d),
        _ => return None,
    })
}

/// Last error text after a failed `app.run` (the toast it set).
pub(crate) fn last_error(app: &App) -> String {
    app.ui.toast.as_ref().map(|(t, _)| t.clone()).unwrap_or_else(|| "The action failed.".into())
}

#[derive(Clone, Default)]
struct PickState {
    sel: Option<u64>,
    error: String,
}

fn picker(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let (title, prompt, pick) = match d.name {
        "moveToFolder" => ("Move Items", "Move the selected items to the folder:", Pick::Mail),
        "copyToFolder" => ("Copy Items", "Copy the selected items to the folder:", Pick::Mail),
        "archiveFolder" => ("Archive", "Choose the folder that archived items go to:", Pick::Mail),
        "alwaysMoveConversation" => {
            ("Always Move Conversation", "Move this conversation and its future messages to:", Pick::Mail)
        }
        "selectFolder" => ("Select Folder", "Choose a folder:", Pick::Mail),
        "moveFolder" => ("Move Folder", "Move the selected folder to the folder:", Pick::Parent),
        "copyFolder" => ("Copy Folder", "Copy the selected folder to the folder:", Pick::Parent),
        "goToFolder" => ("Go to Folder", "Folders:", Pick::Any),
        _ => (
            "Select Folder",
            "Choose a folder:",
            match d.str("pick").as_str() {
                "parent" => Pick::Parent,
                "any" => Pick::Any,
                _ => Pick::Mail,
            },
        ),
    };
    let title = if d.str("title").is_empty() { title.to_owned() } else { d.str("title") };
    let uid = d.uid;
    let initial = d.u64("current").or_else(|| d.u64("selected"));
    with_state(
        ctx,
        app,
        uid,
        |_: &App| PickState { sel: initial, error: String::new() },
        |st, app| {
            for (key, v) in take_replies(ctx, uid) {
                if key == "newFolder" {
                    st.sel = v.as_u64();
                }
            }
            window(ctx, app, d, &title, 380.0, |ui, app| {
                ui.label(prompt);
                if d.name == "moveFolder" || d.name == "copyFolder" {
                    muted(ui, app, &format!("Folder: {}", d.str("path")));
                }
                let ev = folder_tree(ui, app, &mut st.sel, pick, ("pick", uid), 260.0);
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                let ok = footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    let new_parent = st.sel.unwrap_or(app.s.selection.folder.0);
                    if d.name != "goToFolder" && btn(ui, "New...") {
                        out =
                            Out::open("createFolder", child(uid, "newFolder", json!({"parent": new_parent})));
                    }
                    primary(ui, app, st.sel.is_some(), "OK")
                });
                let enter = st.sel.is_some() && common::enter_pressed(ui);
                if (ok || ev.activated || enter)
                    && matches!(out, Out::Keep)
                    && let Some(f) = st.sel
                {
                    out = finish_pick(ctx, app, d, f, &mut st.error);
                }
                out
            })
        },
    )
}

fn finish_pick(ctx: &Context, app: &mut App, d: &Dlg<'_>, f: u64, error: &mut String) -> Out {
    if let Some((to, key)) = d.reply_to() {
        send_reply(ctx, to, &key, json!(f));
        return Out::Close;
    }
    let ids = d.ids();
    let res = match d.name {
        "moveToFolder" => app.run("mail.move", json!({"folder": f, "ids": ids})),
        "copyToFolder" => app.run("mail.copy", json!({"folder": f, "ids": ids})),
        "archiveFolder" => app.run("mail.archive", json!({"folder": f, "ids": ids, "remember": true})),
        "alwaysMoveConversation" => app.run("mail.alwaysMoveConversation", json!({"folder": f, "ids": ids})),
        "selectFolder" if d.str("purpose") == "saveSentItemTo" => app.run(
            "compose.saveSentItemTo",
            json!({"compose": d.u64("compose"), "mode": "other", "folder": f}),
        ),
        "selectFolder" => {
            let item = d.u64("item");
            app.run(
                "rules.alwaysMoveFrom",
                json!({"folder": f, "ids": item.map(|i| vec![i]).unwrap_or_default()}),
            )
        }
        "moveFolder" => app.run("folder.move", json!({"folder": d.u64("folder"), "parent": f})),
        "copyFolder" => app.run("folder.copy", json!({"folder": d.u64("folder"), "parent": f})),
        "goToFolder" => app.run("nav.goToFolder", json!({"folder": f})),
        _ => Some(Value::Null),
    };
    if res.is_some() {
        if matches!(d.name, "moveToFolder" | "copyToFolder") {
            // Shows up under Move ▸ in the context menus next time.
            crate::menus::note_recent_folder(ctx, FolderId(f));
        }
        Out::Close
    } else {
        *error = last_error(app);
        Out::Keep
    }
}

// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct NewFolderState {
    name: String,
    contains: usize,
    sel: Option<u64>,
    error: String,
    focused: bool,
}

const CONTAINS: [&str; 6] =
    ["Mail and Post Items", "Calendar Items", "Contact Items", "Journal Items", "Note Items", "Task Items"];

/// Engine names of [`CONTAINS`] ("Journal Items" is kept for completeness).
const CONTAINS_KEYS: [&str; 6] = ["mail", "calendar", "contacts", "journal", "notes", "tasks"];

/// The "Folder contains" choice that matches a folder's kind.
fn contains_index(app: &App, folder: Option<u64>) -> usize {
    use mailcraft_model::FolderKind;
    match folder.and_then(|f| app.s.mailbox.folder(FolderId(f))).map(|f| f.kind) {
        Some(FolderKind::Calendar) => 1,
        Some(FolderKind::Contacts) => 2,
        Some(FolderKind::Journal) => 3,
        Some(FolderKind::Notes) => 4,
        Some(FolderKind::Tasks) => 5,
        _ => 0,
    }
}

/// Create New Folder: name, what the folder holds, and where it goes (a folder, or the top of an account
/// when the dialog was opened from the account's root with `account`).
fn create_folder(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let account = d.u64("account").filter(|_| d.u64("parent").is_none());
    let parent = if account.is_some() { None } else { d.u64("parent").or(Some(app.s.selection.folder.0)) };
    with_state(
        ctx,
        app,
        uid,
        |a: &App| NewFolderState { sel: parent, contains: contains_index(a, parent), ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Create New Folder", 340.0, |ui, app| {
                ui.label("Name:");
                let r = ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(f32::INFINITY));
                if !st.focused {
                    r.request_focus();
                    st.focused = true;
                }
                ui.label("Folder contains:");
                common::combo_idx(ui, ("contains", uid), &mut st.contains, &CONTAINS, 300.0);
                ui.label("Select where to place the folder:");
                let account_name = account
                    .and_then(|a| app.s.mailbox.accounts.iter().find(|x| u64::from(x.id.0) == a))
                    .map(common::account_title);
                if let Some(name) = &account_name {
                    // From the account's root: at the top of its folder list, or under a folder picked below.
                    let top = st.sel.is_none();
                    if ui.selectable_label(top, format!("{name} (top level)")).clicked() {
                        st.sel = None;
                    }
                }
                let before = st.sel;
                folder_tree(ui, app, &mut st.sel, Pick::Parent, ("nf", uid), 200.0);
                if st.sel != before {
                    st.contains = contains_index(app, st.sel);
                }
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let can = !st.name.trim().is_empty() && (st.sel.is_some() || account_name.is_some());
                match ok_cancel(ui, app, can) {
                    Some(true) => {
                        let contains = CONTAINS_KEYS.get(st.contains).copied().unwrap_or("mail");
                        let p = match st.sel {
                            Some(f) => json!({"name": st.name.trim(), "contains": contains, "parent": f}),
                            None => json!({"name": st.name.trim(), "contains": contains, "account": account}),
                        };
                        match app.run("folder.new", p) {
                            Some(v) => {
                                if let (Some((to, key)), Some(id)) =
                                    (d.reply_to(), v.get("folder").and_then(Value::as_u64))
                                {
                                    send_reply(ctx, to, &key, json!(id));
                                }
                                Out::Close
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

#[derive(Clone, Default)]
struct TextState {
    text: String,
    error: String,
    focused: bool,
}

fn rename_folder(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let current = d.str("name");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| TextState { text: current.clone(), ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Rename Folder", 320.0, |ui, app| {
                ui.label("New name:");
                let r = ui.add(egui::TextEdit::singleline(&mut st.text).desired_width(f32::INFINITY));
                if !st.focused {
                    r.request_focus();
                    st.focused = true;
                }
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                match ok_cancel(ui, app, !st.text.trim().is_empty()) {
                    Some(true) => {
                        if st.text.trim() == current {
                            return Out::Close;
                        }
                        if app
                            .run("folder.rename", json!({"folder": d.u64("folder"), "name": st.text.trim()}))
                            .is_some()
                        {
                            Out::Close
                        } else {
                            st.error = last_error(app);
                            Out::Keep
                        }
                    }
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

/// Yes/No confirmation: Yes runs `command` with `params`. With `dontAsk` (a profile setting key) it offers
/// "Don't ask me again", which turns that setting on when the answer is Yes.
fn confirm(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let title = if d.str("title").is_empty() { "MailCraft".to_owned() } else { d.str("title") };
    let dont_ask_key = d.str("dontAsk");
    let state_id = egui::Id::new(("mc-confirm-dont-ask", d.uid));
    let mut dont_ask = ctx.data(|m| m.get_temp::<bool>(state_id)).unwrap_or(false);
    let out = window(ctx, app, d, &title, 380.0, |ui, app| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(28.0), egui::Sense::hover());
            app.icons.paint(ui, r, "warning", app.theme.flag);
            ui.label(d.str("text"));
        });
        if !dont_ask_key.is_empty() {
            ui.checkbox(&mut dont_ask, "Don't ask me again");
        }
        footer(ui, |ui| {
            if btn(ui, "No") {
                return Out::Close;
            }
            if primary(ui, app, true, "Yes") {
                if dont_ask && !dont_ask_key.is_empty() {
                    app.run("options.set", json!({"key": dont_ask_key, "value": true}));
                }
                let cmd = d.str("command");
                if !cmd.is_empty() {
                    app.run(&cmd, d.get("params").cloned().unwrap_or(json!({})));
                }
                if let Some((to, key)) = d.reply_to() {
                    send_reply(ctx, to, &key, json!(true));
                }
                return Out::Close;
            }
            Out::Keep
        })
    });
    if matches!(out, Out::Keep) {
        ctx.data_mut(|m| m.insert_temp(state_id, dont_ask));
    } else {
        ctx.data_mut(|m| m.remove::<bool>(state_id));
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Data files

#[derive(Clone, Default)]
struct DataFileState {
    name: String,
    note: String,
    error: String,
}

/// Data File Properties: the data file's name (editable), where it is kept, its size and counts, and
/// Compact Now (`file.dataFileProperties`).
fn data_file_properties(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let account = d.u64("account");
    let original = d.str("name");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| DataFileState { name: original.clone(), ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Data File Properties", 420.0, |ui, app| {
                egui::Grid::new(("dfp", uid)).num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(260.0));
                    ui.end_row();
                    if !d.str("address").is_empty() {
                        ui.label("Account:");
                        ui.label(d.str("address"));
                        ui.end_row();
                    }
                    ui.label("Location:");
                    let path = d.str("path");
                    ui.label(if path.is_empty() { "In memory, not saved".to_owned() } else { path });
                    ui.end_row();
                    ui.label("Size:");
                    ui.label(common::size_text(d.u64("bytes").unwrap_or(0)));
                    ui.end_row();
                    ui.label("Contents:");
                    ui.label(format!(
                        "{} items in {} folders",
                        d.u64("items").unwrap_or(0),
                        d.u64("folders").unwrap_or(0)
                    ));
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let can = !d.str("path").is_empty();
                    if btn_en(ui, can, "Compact Now") {
                        match app.run("file.dataFileProperties", json!({"store": account, "compact": true})) {
                            Some(_) => st.note = "The data file was compacted.".into(),
                            None => st.error = last_error(app),
                        }
                    }
                    if !st.note.is_empty() {
                        muted(ui, app, &st.note);
                    }
                });
                muted(ui, app, "Compacting writes the data file afresh, folding in the changes made since.");
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                match ok_cancel(ui, app, !st.name.trim().is_empty()) {
                    Some(true) => {
                        if st.name.trim() == original.trim() {
                            return Out::Close;
                        }
                        let p = json!({"store": account, "name": st.name.trim()});
                        if app.run("file.dataFileProperties", p).is_some() {
                            Out::Close
                        } else {
                            st.error = last_error(app);
                            Out::Keep
                        }
                    }
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

#[derive(Clone, Default)]
struct OpenDataState {
    sel: Option<usize>,
    path: String,
    error: String,
}

/// Open Data File: the data files closed in this profile (Remove keeps them), or a .mcdata file picked by
/// path (`file.openDataFile`).
fn open_data_file(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let files: Vec<(String, u64)> = d
        .arr("files")
        .iter()
        .map(|f| {
            (
                f.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                f.get("bytes").and_then(Value::as_u64).unwrap_or(0),
            )
        })
        .collect();
    with_state(
        ctx,
        app,
        uid,
        |_: &App| OpenDataState::default(),
        |st, app| {
            window(ctx, app, d, "Open Data File", 420.0, |ui, app| {
                if files.is_empty() {
                    muted(
                        ui,
                        app,
                        "No closed data files. Data files you remove are kept here to open again.",
                    );
                } else {
                    ui.label("Closed data files:");
                    egui::Frame::new()
                        .stroke(egui::Stroke::new(1.0, app.theme.separator))
                        .inner_margin(4.0)
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            for (i, (name, bytes)) in files.iter().enumerate() {
                                let label = format!("{name}  ({})", common::size_text(*bytes));
                                if ui.selectable_label(st.sel == Some(i), label).clicked() {
                                    st.sel = Some(i);
                                    st.path.clear();
                                }
                            }
                        });
                }
                ui.label("Or a data file on disk:");
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut st.path)
                            .hint_text("Path to a .mcdata file")
                            .desired_width(280.0),
                    );
                    if r.changed() && !st.path.is_empty() {
                        st.sel = None;
                    }
                    if common::browse_button(ui, app) {
                        let filters = [crate::FileFilter::new("MailCraft data file", &["mcdata"])];
                        if let Some(p) = common::browse_open(app, "Open Data File", &filters, &st.path) {
                            st.path = p;
                            st.sel = None;
                        }
                    }
                });
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let can = st.sel.is_some() || !st.path.trim().is_empty();
                match ok_cancel(ui, app, can) {
                    Some(true) => {
                        let p = match st.sel.and_then(|i| files.get(i)) {
                            Some((name, _)) => json!({"name": name}),
                            None => json!({"path": st.path.trim()}),
                        };
                        if app.run("file.openDataFile", p).is_some() {
                            Out::Close
                        } else {
                            st.error = last_error(app);
                            Out::Keep
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

#[derive(Clone, Default)]
struct PropState {
    tab: usize,
    name: String,
    description: String,
    show_total: bool,
    show_size: bool,
    /// The AutoArchive tab.
    archive: super::archive::FolderForm,
    error: String,
}

fn properties(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let folder = d.u64("folder");
    let key = format!("folder.{}.", folder.unwrap_or(0));
    let init = |a: &App| PropState {
        name: d.str("name"),
        description: common::pref(ctx, &format!("{key}description"))
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
        show_total: common::pref(ctx, &format!("{key}showTotal")).and_then(|v| v.as_bool()).unwrap_or(false),
        archive: folder.map(|f| super::archive::FolderForm::load(a, f)).unwrap_or_default(),
        ..Default::default()
    };
    let kind = d.str("kind");
    let is_mail = kind.is_empty() || kind == "mail";
    let kind_type = d.str("type");
    let query = d.str("query");
    let calendar = d.get("calendar").map(calendar_rows).unwrap_or_default();
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in take_replies(ctx, uid) {
            if k == "archiveTarget" {
                st.archive.target = v.as_u64();
            }
        }
        let title = format!("{} Properties", d.str("name"));
        window(ctx, app, d, &title, 420.0, |ui, app| {
            ui.horizontal(|ui| {
                for (i, t) in ["General", "AutoArchive"].iter().enumerate() {
                    if ui.selectable_label(st.tab == i, *t).clicked() {
                        st.tab = i;
                    }
                }
            });
            ui.separator();
            let is_default = folder
                .and_then(|f| app.s.mailbox.folder(FolderId(f)))
                .is_some_and(mailcraft_model::Folder::is_default);
            if st.tab == 0 {
                egui::Grid::new(("props", uid)).num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Name:");
                    ui.add_enabled(
                        !is_default,
                        egui::TextEdit::singleline(&mut st.name).desired_width(240.0),
                    );
                    ui.end_row();
                    ui.label("Type:");
                    ui.label(if kind_type.is_empty() { "Mail and post items" } else { kind_type.as_str() });
                    ui.end_row();
                    ui.label("Location:");
                    ui.label(d.str("path"));
                    ui.end_row();
                    if !query.is_empty() {
                        ui.label("Search:");
                        ui.label(&query);
                        ui.end_row();
                    }
                    ui.label("Description:");
                    ui.add(
                        egui::TextEdit::multiline(&mut st.description).desired_rows(3).desired_width(240.0),
                    );
                    ui.end_row();
                    if is_mail {
                        ui.label("Unread items:");
                        ui.label(d.u64("unread").unwrap_or(0).to_string());
                        ui.end_row();
                    }
                    ui.label("Total items:");
                    ui.label(d.u64("total").unwrap_or(0).to_string());
                    ui.end_row();
                    for (label, value) in &calendar {
                        ui.label(*label);
                        ui.label(value);
                        ui.end_row();
                    }
                });
                if is_mail {
                    ui.radio_value(&mut st.show_total, false, "Show number of unread items");
                    ui.radio_value(&mut st.show_total, true, "Show total number of items");
                }
                if btn(ui, "Folder Size...") {
                    st.show_size = !st.show_size;
                }
                if st.show_size {
                    muted(ui, app, &format!("Size: {} KB", d.u64("sizeKb").unwrap_or(0)));
                }
            } else if let Some(f) = folder {
                let out = super::archive::folder_form(ui, app, &mut st.archive, f, uid, "archiveTarget");
                if !matches!(out, Out::Keep) {
                    return out;
                }
            }
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    common::set_pref(ctx, &format!("{key}description"), json!(st.description));
                    common::set_pref(ctx, &format!("{key}showTotal"), json!(st.show_total));
                    if !is_default
                        && st.name.trim() != d.str("name")
                        && !st.name.trim().is_empty()
                        && app
                            .run("folder.rename", json!({"folder": folder, "name": st.name.trim()}))
                            .is_none()
                    {
                        st.error = last_error(app);
                        return Out::Keep;
                    }
                    if let Some(f) = folder
                        && st.archive != super::archive::FolderForm::load(app, f)
                        && let Err(e) = st.archive.save(app, f)
                    {
                        st.tab = 1;
                        st.error = e;
                        return Out::Keep;
                    }
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

/// Calendar Properties rows (label, value) from the `calendar` part of the folderProperties params.
fn calendar_rows(c: &Value) -> Vec<(&'static str, String)> {
    let text = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let mut rows = vec![("Color:", text("color"))];
    let source = match text("source").as_str() {
        "internet" => "Internet calendar subscription",
        "file" => "Imported from an iCalendar file",
        _ => "Items created in MailCraft",
    };
    rows.push(("Source:", source.to_owned()));
    if c.get("readOnly").and_then(Value::as_bool) == Some(true) {
        rows.push(("Permissions:", "Read-only".to_owned()));
    }
    let url = text("url");
    if !url.is_empty() {
        rows.push(("Address:", url));
        if let Some(m) = c.get("refreshMin").and_then(Value::as_u64).filter(|m| *m > 0) {
            rows.push(("Updates:", format!("Every {m} minutes")));
        }
        let err = text("lastError");
        if !err.is_empty() {
            rows.push(("Last update:", format!("Failed: {err}")));
        } else if c.get("lastUpdate").and_then(Value::as_i64).is_none() {
            rows.push(("Last update:", "Not downloaded yet".to_owned()));
        }
    }
    rows
}

// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct SearchFolderState {
    pick: usize,
    text: String,
    name: String,
    error: String,
}

fn new_search_folder(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    with_state(
        ctx,
        app,
        uid,
        |_: &App| SearchFolderState::default(),
        |st, app| {
            window(ctx, app, d, "New Search Folder", 400.0, |ui, app| {
                ui.label("Select a Search Folder:");
                let frame =
                    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0);
                frame.show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(("sf", uid))
                        .max_height(230.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            let mut group = "";
                            for (i, (_, g, label, _)) in SEARCH_FOLDER_TEMPLATES.iter().enumerate() {
                                if *g != group {
                                    group = g;
                                    ui.label(RichText::new(*g).strong());
                                }
                                let r = ui.add(
                                    egui::Button::selectable(st.pick == i, format!("    {label}"))
                                        .frame_when_inactive(false),
                                );
                                if r.clicked() && st.pick != i {
                                    st.pick = i;
                                    st.text.clear();
                                    st.error.clear();
                                }
                            }
                        });
                });
                let Some((template, _, _, needs)) = SEARCH_FOLDER_TEMPLATES.get(st.pick).copied() else {
                    return Out::Close;
                };
                ui.label("Customize Search Folder:");
                let custom = template == "custom";
                // The template's option: (label, hint); `?` marks an optional one.
                let optional = needs.ends_with('?');
                let option = match needs.trim_end_matches('?') {
                    "people" => Some(("People:", "Names or addresses, separated by ;")),
                    "group" => Some(("Group:", "Group addresses, separated by ;")),
                    "category" => Some(("Category:", "Leave empty for any category")),
                    "kb" => Some(("Larger than (KB):", "Default 100 KB")),
                    "days" => Some(("Older than (days):", "Default 7 days")),
                    "words" => Some(("Words:", "Words in the subject or body, separated by ;")),
                    _ => None,
                };
                if custom {
                    common::field(ui, "Name:", &mut st.name, 260.0);
                    common::field(ui, "Criteria:", &mut st.text, 260.0);
                    muted(ui, app, "Criteria use the search box syntax, e.g. from:ann hasattachments:yes");
                } else if let Some((label, hint)) = option {
                    common::field(ui, label, &mut st.text, 260.0);
                    muted(ui, app, hint);
                } else {
                    muted(ui, app, "No options to set.");
                }
                let acct = app
                    .s
                    .current_account()
                    .and_then(|a| app.s.mailbox.account(a))
                    .map(common::account_title)
                    .unwrap_or_default();
                muted(ui, app, &format!("Search mail in: {acct}"));
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let text = st.text.trim().to_owned();
                let ready = if custom {
                    !st.name.trim().is_empty() && !text.is_empty()
                } else {
                    option.is_none() || optional || !text.is_empty()
                };
                match ok_cancel(ui, app, ready) {
                    Some(true) => {
                        let params = if custom {
                            json!({"template": template, "value": text, "name": st.name.trim()})
                        } else {
                            json!({"template": template, "value": text})
                        };
                        if app.run("folder.newSearchFolder", params).is_some() {
                            Out::Close
                        } else {
                            st.error = last_error(app);
                            Out::Keep
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

#[derive(Clone, Default)]
struct RecoverState {
    picked: Vec<u64>,
    /// Items already recovered (no longer listed).
    recovered: Vec<u64>,
    done: String,
}

fn recover(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let items = d.arr("items");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| RecoverState::default(),
        |st, app| {
            window(ctx, app, d, "Recover Deleted Items", 560.0, |ui, app| {
                let id_of = |it: &Value| it.get("id").and_then(Value::as_u64);
                let left: Vec<&Value> =
                    items.iter().filter(|it| id_of(it).is_some_and(|i| !st.recovered.contains(&i))).collect();
                if left.is_empty() {
                    muted(ui, app, "There are no items to recover.");
                }
                egui::ScrollArea::vertical().id_salt(("rec", uid)).max_height(260.0).show(ui, |ui| {
                    egui::Grid::new(("recg", uid)).num_columns(5).striped(true).show(ui, |ui| {
                        ui.label("");
                        ui.label(RichText::new("Subject").strong());
                        ui.label(RichText::new("Type").strong());
                        ui.label(RichText::new("From").strong());
                        ui.label(RichText::new("Folder").strong());
                        ui.end_row();
                        for it in &left {
                            let Some(id) = it.get("id").and_then(Value::as_u64) else { continue };
                            let mut on = st.picked.contains(&id);
                            let subject = it.get("subject").and_then(Value::as_str).unwrap_or_default();
                            if ui.checkbox(&mut on, "").changed() {
                                if on {
                                    st.picked.push(id);
                                } else {
                                    st.picked.retain(|x| *x != id);
                                }
                            }
                            ui.label(if subject.is_empty() { "(no subject)" } else { subject });
                            ui.label(kind_label(it.get("kind").and_then(Value::as_str).unwrap_or("mail")));
                            ui.label(it.get("from").and_then(Value::as_str).unwrap_or_default());
                            ui.label(it.get("folder").and_then(Value::as_str).unwrap_or_default());
                            ui.end_row();
                        }
                    });
                });
                if !st.done.is_empty() {
                    muted(ui, app, &st.done);
                }
                footer(ui, |ui| {
                    if btn(ui, "Close") {
                        return Out::Close;
                    }
                    if btn_en(ui, !st.picked.is_empty(), "Recover Selected Items") {
                        let res = app.run("folder.recoverDeleted", json!({"ids": st.picked}));
                        if let Some(v) = res {
                            st.done = format!(
                                "Recovered {} item(s) to Deleted Items.",
                                v.get("restored").and_then(Value::as_u64).unwrap_or(0)
                            );
                            st.recovered.extend(
                                v.get("recovered")
                                    .and_then(Value::as_array)
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Value::as_u64),
                            );
                            st.picked.clear();
                        }
                    }
                    if btn_en(ui, !left.is_empty(), "Select All") {
                        st.picked = left.iter().filter_map(|it| id_of(it)).collect();
                    }
                    Out::Keep
                })
            })
        },
    )
}

/// Item type shown in the Recover Deleted Items list.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "appointment" => "Appointment",
        "contact" => "Contact",
        "task" => "Task",
        "note" => "Note",
        _ => "Message",
    }
}
