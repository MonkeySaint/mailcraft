//! Module dialogs: Move / Copy to Folder and Custom follow-up for contacts, tasks and notes;
//! the module View Settings, Apply View and Save View dialogs; Mail Merge Contacts; the Task Request
//! window (People › Assign Task, New Items › Task Request); and the question asked before a task accepted
//! from a task request is deleted.
//!
//! The engine asks for them with `Notice::OpenDialog` (ids in [`HANDLED`]); [`super::show`] takes them out
//! of `app.ui.dialogs` before the dialogs area draws, so these replace any generic renderer. Every button
//! acts through `app.run` with the module's commands.

use std::collections::BTreeSet;

use egui::{ComboBox, Id, RichText, ScrollArea, TextEdit, Ui, vec2};
use serde_json::{Value, json};

use mailcraft_model::FolderKind;
use mailcraft_model::view::{ItemFilter, ItemRule};

use super::super::common::{ModState, day_string, day_time_string, parse_day, run_checked, today};
use super::{STATUSES, date_field, error_line, recurrence_choices, time_combo};
use crate::App;

/// Dialog ids drawn here.
pub(crate) const HANDLED: [&str; 9] = [
    "pimMove",
    "deleteTaskRequest",
    "pimFlag",
    "moduleViewSettings",
    "applyModuleView",
    "saveModuleView",
    "mailMerge",
    "taskRequest",
    "newTaskRequest",
];

/// Open module dialogs.
#[derive(Clone, Default)]
pub(crate) struct ModuleDialogs {
    pub open: Vec<Dialog>,
    pub nonce: u64,
}

/// One open dialog.
#[derive(Clone)]
pub(crate) struct Dialog {
    pub nonce: u64,
    pub kind: Kind,
    pub error: Option<String>,
}

#[derive(Clone)]
pub(crate) enum Kind {
    Move {
        ids: Vec<u64>,
        area: String,
        copy: bool,
        folder: Option<u64>,
    },
    Flag(FlagForm),
    ViewSettings(Box<ViewForm>),
    ApplyView {
        noun: String,
        folders: Vec<(u64, String)>,
        picked: BTreeSet<u64>,
        subfolders: bool,
    },
    SaveView {
        name: String,
        noun: String,
        all_text: String,
        all: bool,
    },
    MailMerge(MergeForm),
    TaskRequest(RequestForm),
    /// Deleting a task accepted from a task request: decline, mark complete and notify, or just delete.
    DeleteRequest {
        ids: Vec<u64>,
        subject: String,
        choice: String,
        /// Set once the tasks were really deleted (the task window showing one of them then closes).
        done: bool,
    },
}

#[derive(Clone, Default)]
pub(crate) struct FlagForm {
    pub ids: Vec<u64>,
    pub area: String,
    /// Add Reminder (only the reminder is set).
    pub reminder_only: bool,
    pub label: String,
    pub start: String,
    pub due: String,
    pub reminder_on: bool,
    pub reminder_date: String,
    pub reminder_min: i64,
}

#[derive(Clone, Default)]
pub(crate) struct ViewForm {
    pub module: String,
    pub views: Vec<String>,
    pub arrangements: Vec<(String, String)>,
    pub view: String,
    pub arrange: Option<String>,
    pub reverse: bool,
    pub groups: bool,
    pub preview: u8,
    pub pane: String,
    pub spacing: bool,
    /// The table parts of View Settings (Columns, Sort, Filter, Conditional Formatting, Format Columns): their
    /// descriptions,
    /// `None` when the view is not a table.
    pub table: Option<TableSummary>,
    /// Opened from Manage All Views › Modify on this saved view: OK stores the view into it.
    pub saved_view: String,
    /// The view Change View checks (a saved view's name, else the built-in view).
    pub current: String,
    /// Notes tables: sort column ("" = the view's own order), columns shown and every column (key, label).
    pub sort: String,
    pub columns: Vec<String>,
    pub all_columns: Vec<(String, String)>,
    /// Filter, Other Settings and Conditional Formatting.
    pub filter: ItemFilter,
    pub auto_preview: bool,
    pub grid_lines: bool,
    pub rules: Vec<ItemRule>,
    /// Values when the dialog opened (only changes are applied).
    pub initial: Box<Option<ViewForm>>,
}

/// What the table parts of a module view are set to.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct TableSummary {
    pub columns: String,
    pub sort: String,
    pub filter: String,
    pub rules: String,
    pub formats: String,
}

impl ViewForm {
    /// True when the advanced settings (columns, sort, filter, other settings, rules) differ from `o`.
    fn advanced_changed(&self, o: &ViewForm) -> bool {
        self.sort != o.sort
            || self.columns != o.columns
            || self.filter != o.filter
            || self.auto_preview != o.auto_preview
            || self.grid_lines != o.grid_lines
            || self.rules != o.rules
    }
}

#[derive(Clone, Default)]
pub(crate) struct MergeForm {
    /// "all" (the contacts of the current view) or "selected".
    pub contacts: String,
    pub output: String,
    /// The contacts selected when the window opened.
    pub selected: Vec<u64>,
    pub fields: Vec<String>,
    pub subject: String,
    pub body: String,
    pub index: usize,
    pub preview: Option<Value>,
    pub result: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct RequestForm {
    pub to: String,
    pub subject: String,
    pub start: String,
    pub due: String,
    pub status: String,
    pub priority: String,
    pub recurrence: String,
    pub body: String,
    pub keep_copy: bool,
    pub status_report: bool,
    pub folder: Option<u64>,
}

fn ids_of(p: &Value) -> Vec<u64> {
    p.get("ids")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default()
}

fn str_of(p: &Value, key: &str) -> String {
    p.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn strings_of(p: &Value, key: &str) -> Vec<String> {
    p.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Opens the dialog a command asked for.
pub(crate) fn open(app: &App, st: &mut ModState, name: &str, p: &Value) {
    let kind = match name {
        "pimMove" => Kind::Move {
            ids: ids_of(p),
            area: str_of(p, "area"),
            copy: p.get("copy").and_then(Value::as_bool).unwrap_or(false),
            folder: None,
        },
        "pimFlag" => Kind::Flag(FlagForm {
            ids: ids_of(p),
            area: str_of(p, "area"),
            reminder_only: p.get("reminder").and_then(Value::as_bool).unwrap_or(false),
            label: "Follow up".into(),
            reminder_on: p.get("reminder").and_then(Value::as_bool).unwrap_or(false),
            reminder_date: day_string(today(app)),
            reminder_min: 16 * 60,
            ..Default::default()
        }),
        "moduleViewSettings" => {
            let mut f = ViewForm {
                module: str_of(p, "module"),
                views: strings_of(p, "views"),
                arrangements: p
                    .get("arrangements")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| {
                                Some((
                                    x.get("key")?.as_str()?.to_owned(),
                                    x.get("label")?.as_str()?.to_owned(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                view: str_of(p, "view"),
                arrange: p.get("arrange").and_then(Value::as_str).map(str::to_owned),
                reverse: p.get("reverse").and_then(Value::as_bool).unwrap_or(false),
                groups: p.get("groups").and_then(Value::as_bool).unwrap_or(true),
                preview: p
                    .get("previewLines")
                    .and_then(Value::as_u64)
                    .and_then(|n| u8::try_from(n).ok())
                    .unwrap_or(0),
                pane: str_of(p, "readingPane"),
                spacing: p.get("tighterSpacing").and_then(Value::as_bool).unwrap_or(false),
                table: p.get("table").filter(|t| t.get("table").and_then(Value::as_bool) == Some(true)).map(
                    |t| TableSummary {
                        columns: str_of(t, "columns"),
                        sort: str_of(t, "sort"),
                        filter: str_of(t, "filter"),
                        rules: str_of(t, "rules"),
                        formats: str_of(t, "formats"),
                    },
                ),
                saved_view: str_of(p, "savedView"),
                current: str_of(p, "current"),
                sort: str_of(p, "sort"),
                columns: strings_of(p, "columns"),
                all_columns: p
                    .get("allColumns")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| {
                                Some((
                                    x.get("key")?.as_str()?.to_owned(),
                                    x.get("label")?.as_str()?.to_owned(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                filter: p
                    .get("filter")
                    .and_then(|v| serde_json::from_value::<ItemFilter>(v.clone()).ok())
                    .unwrap_or_default(),
                auto_preview: p.get("autoPreview").and_then(Value::as_bool).unwrap_or(true),
                grid_lines: p.get("gridLines").and_then(Value::as_bool).unwrap_or(true),
                rules: p
                    .get("rules")
                    .and_then(|v| serde_json::from_value::<Vec<ItemRule>>(v.clone()).ok())
                    .unwrap_or_default(),
                initial: Box::new(None),
            };
            f.initial = Box::new(Some(f.clone()));
            Kind::ViewSettings(Box::new(f))
        }
        "applyModuleView" => Kind::ApplyView {
            noun: str_of(p, "noun"),
            folders: p
                .get("folders")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| Some((x.get("id")?.as_u64()?, x.get("path")?.as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default(),
            picked: BTreeSet::new(),
            subfolders: false,
        },
        "saveModuleView" => Kind::SaveView {
            name: str_of(p, "name"),
            noun: str_of(p, "noun"),
            all_text: str_of(p, "allFolders"),
            all: false,
        },
        "mailMerge" => Kind::MailMerge(MergeForm {
            contacts: if str_of(p, "contacts") == "selected" { "selected".into() } else { "all".into() },
            // E-mail is the output MailCraft can produce.
            output: "email".into(),
            selected: app
                .s
                .selection
                .items
                .iter()
                .filter(|i| app.s.mailbox.contacts.contains_key(i))
                .map(|i| i.0)
                .collect(),
            fields: strings_of(p, "fields"),
            subject: String::new(),
            body: "Dear {{First Name}},\n\n".into(),
            ..Default::default()
        }),
        "deleteTaskRequest" => Kind::DeleteRequest {
            ids: ids_of(p),
            subject: str_of(p, "subject"),
            choice: "decline".into(),
            done: false,
        },
        "taskRequest" | "newTaskRequest" => Kind::TaskRequest(RequestForm {
            to: str_of(p, "to"),
            status: "notStarted".into(),
            priority: "normal".into(),
            // Options › Tasks: "Keep my task list updated with copies…" and "Send status report…".
            keep_copy: app.s.settings.bool("tasks.keepCopies"),
            status_report: app.s.settings.bool("tasks.statusReport"),
            folder: p.get("folder").and_then(Value::as_u64),
            ..Default::default()
        }),
        _ => return,
    };
    st.dialogs.nonce = st.dialogs.nonce.wrapping_add(1);
    let nonce = st.dialogs.nonce;
    st.dialogs.open.push(Dialog { nonce, kind, error: None });
}

/// Draws the open module dialogs.
pub(crate) fn show(ctx: &egui::Context, app: &mut App, st: &mut ModState) {
    let open = std::mem::take(&mut st.dialogs.open);
    let mut keep = Vec::new();
    for mut d in open {
        if draw(ctx, app, &mut d) {
            keep.push(d);
        } else if let Kind::DeleteRequest { ids, done: true, .. } = &d.kind {
            close_deleted_task(st, ids);
        }
    }
    // Dialogs opened while drawing (none today) come after the ones kept.
    keep.append(&mut st.dialogs.open);
    st.dialogs.open = keep;
}

/// The task window stays open while the delete question is asked; it closes once its task is deleted.
fn close_deleted_task(st: &mut ModState, ids: &[u64]) {
    if st.editors.task.as_ref().and_then(|e| e.id).is_some_and(|id| ids.contains(&id)) {
        st.editors.task = None;
    }
}

fn window<'a>(title: &str, nonce: u64) -> egui::Window<'a> {
    egui::Window::new(title.to_owned())
        .id(Id::new(("mc-module-dialog", nonce)))
        .collapsible(false)
        .resizable(true)
        .default_width(440.0)
        .order(egui::Order::Foreground)
}

/// What the buttons row asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    None,
    Ok,
    Cancel,
    Other,
}

fn ok_cancel(ui: &mut Ui, ok: &str, enabled: bool) -> Btn {
    let mut out = Btn::None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.button("Cancel").clicked() {
            out = Btn::Cancel;
        }
        if ui.add_enabled(enabled, egui::Button::new(RichText::new(ok).strong())).clicked() {
            out = Btn::Ok;
        }
    });
    out
}

fn folder_kind(area: &str) -> FolderKind {
    match area {
        "people" => FolderKind::Contacts,
        "tasks" => FolderKind::Tasks,
        "notes" => FolderKind::Notes,
        "calendar" => FolderKind::Calendar,
        _ => FolderKind::Mail,
    }
}

/// Draws one dialog; false when it closed.
fn draw(ctx: &egui::Context, app: &mut App, d: &mut Dialog) -> bool {
    let mut open = true;
    let mut btn = Btn::None;
    let nonce = d.nonce;
    let title = match &d.kind {
        Kind::Move { copy: true, .. } => "Copy Items".to_owned(),
        Kind::Move { .. } => "Move Items".to_owned(),
        Kind::Flag(f) if f.reminder_only => "Add Reminder".to_owned(),
        Kind::Flag(_) => "Custom".to_owned(),
        Kind::ViewSettings(f) => format!("Advanced View Settings: {}", f.view),
        Kind::ApplyView { .. } => "Apply View".to_owned(),
        Kind::SaveView { .. } => "Save Current View As a New View".to_owned(),
        Kind::MailMerge(_) => "Mail Merge Contacts".to_owned(),
        Kind::DeleteRequest { .. } => "Delete Task".to_owned(),
        Kind::TaskRequest(f) => {
            format!(
                "{} - Task Request",
                if f.subject.trim().is_empty() { "Untitled" } else { f.subject.trim() }
            )
        }
    };
    let error = d.error.clone();
    window(&title, nonce).open(&mut open).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &app.theme);
        btn = match &mut d.kind {
            Kind::Move { ids, area, copy, folder } => move_ui(ui, app, ids.len(), area, *copy, folder),
            Kind::Flag(f) => flag_ui(ui, app, f, nonce),
            Kind::ViewSettings(f) => view_ui(ui, app, f, nonce),
            Kind::ApplyView { noun, folders, picked, subfolders, .. } => {
                apply_ui(ui, noun, folders, picked, subfolders)
            }
            Kind::SaveView { name, noun, all_text, all } => save_ui(ui, name, noun, all_text, all),
            Kind::MailMerge(f) => merge_ui(ui, app, f, nonce),
            Kind::TaskRequest(f) => request_ui(ui, app, f, nonce),
            Kind::DeleteRequest { subject, choice, .. } => {
                let s = if subject.trim().is_empty() { "this task" } else { subject.trim() };
                ui.label(format!("\"{s}\" was assigned to you. Before it is deleted:"));
                for (k, l) in [
                    ("decline", "Decline it and delete"),
                    ("complete", "Mark it complete, tell the person who assigned it, and delete"),
                    ("delete", "Delete it without telling anyone"),
                ] {
                    ui.radio_value(choice, k.to_owned(), l);
                }
                ok_cancel(ui, "OK", true)
            }
        };
        error_line(ui, app, &error);
    });
    if !open || btn == Btn::Cancel {
        return false;
    }
    let result = match btn {
        Btn::Ok => apply(app, &mut d.kind),
        Btn::Other => other(app, &mut d.kind),
        _ => return true,
    };
    match result {
        Ok(close) => {
            d.error = None;
            !close
        }
        Err(e) => {
            d.error = Some(e);
            true
        }
    }
}

fn move_ui(
    ui: &mut Ui,
    app: &mut App,
    count: usize,
    area: &str,
    copy: bool,
    folder: &mut Option<u64>,
) -> Btn {
    let verb = if copy { "Copy" } else { "Move" };
    ui.label(format!("{verb} the {count} selected item{} to the folder:", if count == 1 { "" } else { "s" }));
    let kind = folder_kind(area);
    let mut folders: Vec<(String, u64)> = app
        .s
        .mailbox
        .folders
        .values()
        .filter(|f| f.kind == kind)
        .map(|f| (app.s.mailbox.folder_path(f.id), f.id.0))
        .collect();
    folders.sort();
    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(ui, |ui| {
        ScrollArea::vertical().id_salt("mc-move-folders").max_height(220.0).auto_shrink([false, true]).show(
            ui,
            |ui| {
                for (path, id) in &folders {
                    if ui.selectable_label(*folder == Some(*id), path).clicked() {
                        *folder = Some(*id);
                    }
                }
            },
        );
    });
    let mut out = Btn::None;
    ui.horizontal(|ui| {
        // New Folder: a folder of this kind under the one picked (or the current one).
        if ui.button("New...").clicked() {
            let parent = folder.or(Some(app.s.selection.folder.0));
            let contains = mailcraft_engine::cmd::folder::contents_key(kind);
            crate::dialogs::open(app, "createFolder", json!({ "parent": parent, "contains": contains }));
        }
        let b = ok_cancel(ui, "OK", folder.is_some());
        if b != Btn::None {
            out = b;
        }
    });
    out
}

fn flag_ui(ui: &mut Ui, app: &App, f: &mut FlagForm, nonce: u64) -> Btn {
    let now = today(app);
    egui::Grid::new(("mc-flag-grid", nonce)).num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        if !f.reminder_only {
            if f.area == "people" {
                ui.label("Flag to");
                ui.add(TextEdit::singleline(&mut f.label).desired_width(200.0));
                ui.end_row();
            }
            for (label, value, id) in
                [("Start date", &mut f.start, "mc-flag-start"), ("Due date", &mut f.due, "mc-flag-due")]
            {
                ui.label(label);
                ui.horizontal(|ui| {
                    date_field(ui, value, id);
                    if ui.small_button("Today").clicked() {
                        *value = day_string(now);
                    }
                    if ui.small_button("None").clicked() {
                        value.clear();
                    }
                });
                ui.end_row();
            }
        }
        ui.label("Reminder");
        ui.horizontal(|ui| {
            ui.checkbox(&mut f.reminder_on, "");
            ui.add_enabled_ui(f.reminder_on, |ui| {
                date_field(ui, &mut f.reminder_date, "mc-flag-rdate");
                time_combo(ui, "mc-flag-rtime", &mut f.reminder_min);
            });
        });
        ui.end_row();
    });
    let mut out = Btn::None;
    ui.horizontal(|ui| {
        if f.area == "people" && !f.reminder_only && ui.button("Clear Flag").clicked() {
            out = Btn::Other;
        }
        let b = ok_cancel(ui, "OK", true);
        if b != Btn::None {
            out = b;
        }
    });
    out
}

const PANES: [(&str, &str); 3] = [("right", "Right"), ("bottom", "Bottom"), ("off", "Off")];

/// The table parts of View Settings: a button per part (it opens that dialog) with what it is set to.
fn table_parts(ui: &mut Ui, app: &mut App, t: &TableSummary, nonce: u64) {
    let parts: [(&str, &str, Value, &str); 5] = [
        ("Columns...", "view.addColumns", json!({}), &t.columns),
        ("Sort...", "", json!({}), &t.sort),
        ("Filter...", "view.customFilter", json!({}), &t.filter),
        ("Conditional Formatting...", "view.conditionalFormatting", json!({}), &t.rules),
        ("Format Columns...", "view.formatColumns", json!({}), &t.formats),
    ];
    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(6.0).show(ui, |ui| {
        ui.label(RichText::new("Description").strong());
        egui::Grid::new(("mc-viewset-parts", nonce)).num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
            for (label, cmd, params, desc) in parts {
                if ui.add(egui::Button::new(label).min_size(vec2(180.0, 22.0))).clicked() {
                    if cmd.is_empty() {
                        let folder = app.s.selection.folder.0;
                        crate::dialogs::open(app, "viewSort", json!({ "folder": folder }));
                    } else {
                        app.run(cmd, params);
                    }
                }
                ui.add(egui::Label::new(RichText::new(desc).color(app.theme.text_muted)).truncate());
                ui.end_row();
            }
        });
    });
    ui.add_space(6.0);
}

fn view_ui(ui: &mut Ui, app: &mut App, f: &mut ViewForm, nonce: u64) -> Btn {
    let mut out = Btn::None;
    // The table parts describe the view as it is now (they apply at once in their own dialogs).
    if let Some(t) = &f.table {
        let folder = app.s.selection.folder;
        let live = mailcraft_engine::cmd::view::modules::current(&app.s, folder)
            .map(|(kind, st)| mailcraft_engine::cmd::view::table::summary(folder, kind, &st));
        let t = match live {
            Some(v) if v.get("table").and_then(Value::as_bool) == Some(true) => TableSummary {
                columns: str_of(&v, "columns"),
                sort: str_of(&v, "sort"),
                filter: str_of(&v, "filter"),
                rules: str_of(&v, "rules"),
                formats: str_of(&v, "formats"),
            },
            _ => t.clone(),
        };
        table_parts(ui, app, &t, nonce);
    }
    egui::Grid::new(("mc-viewset-grid", nonce)).num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Current view");
        ComboBox::from_id_salt(("mc-viewset-view", nonce))
            .selected_text(f.view.clone())
            .width(200.0)
            .show_ui(ui, |ui| {
                for v in f.views.clone() {
                    if ui.selectable_label(f.view == v, &v).clicked() {
                        if f.view != v {
                            f.arrange = None;
                        }
                        f.view = v;
                    }
                }
            });
        ui.end_row();
        let can_arrange = match f.module.as_str() {
            "people" => matches!(f.view.as_str(), "Phone" | "List"),
            "tasks" => true,
            _ => false,
        };
        if !f.arrangements.is_empty() {
            ui.label("Group By");
            let current = f
                .arrange
                .as_ref()
                .and_then(|k| f.arrangements.iter().find(|(key, _)| key == k))
                .map_or("(view default)".to_owned(), |(_, l)| l.clone());
            ui.add_enabled_ui(can_arrange, |ui| {
                ComboBox::from_id_salt(("mc-viewset-arrange", nonce))
                    .selected_text(current)
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for (k, l) in f.arrangements.clone() {
                            if ui.selectable_label(f.arrange.as_deref() == Some(k.as_str()), &l).clicked() {
                                f.arrange = Some(k);
                            }
                        }
                    });
            });
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut f.groups, "Show in groups");
            ui.end_row();
        }
        ui.label("Sort");
        ui.checkbox(&mut f.reverse, "Reverse sort");
        ui.end_row();
        if f.module == "tasks" {
            ui.label("Message preview");
            let text = if f.preview == 0 { "Off".to_owned() } else { format!("{} line(s)", f.preview) };
            ComboBox::from_id_salt(("mc-viewset-preview", nonce)).selected_text(text).width(120.0).show_ui(
                ui,
                |ui| {
                    for n in 0..=3u8 {
                        let label = if n == 0 { "Off".to_owned() } else { format!("{n} line(s)") };
                        ui.selectable_value(&mut f.preview, n, label);
                    }
                },
            );
            ui.end_row();
        }
        ui.label("Reading pane");
        let cur = PANES.iter().find(|(k, _)| *k == f.pane).map_or("Right", |(_, l)| *l);
        ComboBox::from_id_salt(("mc-viewset-pane", nonce)).selected_text(cur).width(120.0).show_ui(
            ui,
            |ui| {
                for (k, l) in PANES {
                    ui.selectable_value(&mut f.pane, k.to_owned(), l);
                }
            },
        );
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut f.spacing, "Use tighter spacing");
        ui.end_row();
    });
    if f.module == "notes" {
        advanced_ui(ui, f, nonce);
    }
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Reset Current View").clicked() {
            out = Btn::Other;
        }
        let b = ok_cancel(ui, "OK", true);
        if b != Btn::None {
            out = b;
        }
    });
    out
}

/// Colours offered by Conditional Formatting (label, "#rrggbb"; "" = the list's own).
const RULE_COLORS: [(&str, &str); 7] = [
    ("Automatic", ""),
    ("Red", "#c42b1c"),
    ("Orange", "#ca5010"),
    ("Green", "#107c10"),
    ("Blue", "#0f6cbd"),
    ("Purple", "#8764b8"),
    ("Gray", "#8a8886"),
];

/// Columns, Sort, Filter, Other Settings and Conditional Formatting of a Notes table view.
fn advanced_ui(ui: &mut Ui, f: &mut ViewForm, nonce: u64) {
    let table = f.view != "Icon";
    egui::CollapsingHeader::new("Columns").id_salt(("mc-viewset-cols", nonce)).show(ui, |ui| {
        if !table {
            ui.label(RichText::new("Columns apply to the Notes List and Last 7 Days views.").weak());
        }
        ui.add_enabled_ui(table, |ui| {
            for (key, label) in f.all_columns.clone() {
                let mut on = f.columns.contains(&key);
                let fixed = key == "subject";
                if ui.add_enabled(!fixed, egui::Checkbox::new(&mut on, &label)).changed() {
                    if on {
                        // Keep the dialog's column order.
                        let order: Vec<String> = f.all_columns.iter().map(|(k, _)| k.clone()).collect();
                        f.columns.push(key.clone());
                        f.columns.sort_by_key(|c| order.iter().position(|k| k == c).unwrap_or(usize::MAX));
                    } else {
                        f.columns.retain(|c| *c != key);
                    }
                }
            }
        });
    });
    egui::CollapsingHeader::new("Sort").id_salt(("mc-viewset-sort", nonce)).show(ui, |ui| {
        ui.add_enabled_ui(table, |ui| {
            let cur = f
                .all_columns
                .iter()
                .find(|(k, _)| *k == f.sort)
                .map_or("Created (newest first)".to_owned(), |(_, l)| l.clone());
            ui.horizontal(|ui| {
                ui.label("Sort items by");
                ComboBox::from_id_salt(("mc-viewset-sortby", nonce)).selected_text(cur).width(200.0).show_ui(
                    ui,
                    |ui| {
                        ui.selectable_value(&mut f.sort, String::new(), "Created (newest first)");
                        for (k, l) in f.all_columns.clone() {
                            ui.selectable_value(&mut f.sort, k, l);
                        }
                    },
                );
            });
        });
    });
    let filter_title = format!("Filter: {}", f.filter.describe());
    egui::CollapsingHeader::new(filter_title).id_salt(("mc-viewset-filter", nonce)).show(ui, |ui| {
        filter_ui(ui, &mut f.filter, ("mc-viewset-filter-grid", nonce));
        if ui.button("Clear All").clicked() {
            f.filter = ItemFilter::default();
        }
    });
    egui::CollapsingHeader::new("Other Settings").id_salt(("mc-viewset-other", nonce)).show(ui, |ui| {
        ui.add_enabled_ui(table, |ui| {
            ui.checkbox(&mut f.auto_preview, "AutoPreview (the note text under each row)");
            ui.checkbox(&mut f.grid_lines, "Grid lines between rows");
        });
    });
    egui::CollapsingHeader::new("Conditional Formatting").id_salt(("mc-viewset-rules", nonce)).show(
        ui,
        |ui| {
            let mut remove = None;
            for (i, r) in f.rules.iter_mut().enumerate() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut r.on, "");
                        ui.add(TextEdit::singleline(&mut r.name).desired_width(160.0).hint_text("Rule name"));
                        if ui.small_button("Delete").clicked() {
                            remove = Some(i);
                        }
                    });
                    filter_ui(ui, &mut r.filter, ("mc-viewset-rule", nonce, i));
                    ui.horizontal(|ui| {
                        ui.label("Font");
                        ui.checkbox(&mut r.bold, "Bold");
                        ui.checkbox(&mut r.italic, "Italic");
                        ui.checkbox(&mut r.strike, "Strikethrough");
                        let cur =
                            RULE_COLORS.iter().find(|(_, h)| *h == r.color).map_or("Automatic", |(l, _)| *l);
                        ComboBox::from_id_salt(("mc-viewset-rule-color", nonce, i))
                            .selected_text(cur)
                            .show_ui(ui, |ui| {
                                for (l, h) in RULE_COLORS {
                                    ui.selectable_value(&mut r.color, h.to_owned(), l);
                                }
                            });
                    });
                });
            }
            if let Some(i) = remove
                && i < f.rules.len()
            {
                f.rules.remove(i);
            }
            if ui.add_enabled(f.rules.len() < 32, egui::Button::new("Add")).clicked() {
                let n = f.rules.len().saturating_add(1);
                f.rules.push(ItemRule { name: format!("Untitled {n}"), on: true, ..Default::default() });
            }
        },
    );
}

/// The conditions of an [`ItemFilter`]: words (subject, or subject and text), category, days.
fn filter_ui(ui: &mut Ui, f: &mut ItemFilter, id: impl std::hash::Hash + std::fmt::Debug) {
    egui::Grid::new(id).num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
        let l = ui.label("Search for the word(s)");
        ui.add(TextEdit::singleline(&mut f.words).desired_width(220.0)).labelled_by(l.id);
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut f.in_body, "Also in the note text");
        ui.end_row();
        let l = ui.label("Category");
        ui.add(TextEdit::singleline(&mut f.category).desired_width(220.0)).labelled_by(l.id);
        ui.end_row();
        ui.label("Changed within");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut f.days).range(0..=36_500));
            ui.label(if f.days == 0 { "days (any time)" } else { "days" });
        });
        ui.end_row();
    });
}

fn apply_ui(
    ui: &mut Ui,
    noun: &str,
    folders: &[(u64, String)],
    picked: &mut BTreeSet<u64>,
    subfolders: &mut bool,
) -> Btn {
    ui.label(format!("Apply the current view to these {} folders:", noun.to_lowercase()));
    ScrollArea::vertical().id_salt("mc-apply-view").max_height(220.0).auto_shrink([false, true]).show(
        ui,
        |ui| {
            if folders.is_empty() {
                ui.label(RichText::new("There are no other folders of this kind.").weak());
            }
            for (id, path) in folders {
                let mut on = picked.contains(id);
                if ui.checkbox(&mut on, path).changed() {
                    if on {
                        picked.insert(*id);
                    } else {
                        picked.remove(id);
                    }
                }
            }
        },
    );
    ui.checkbox(subfolders, "Apply view to subfolders");
    ok_cancel(ui, "OK", !picked.is_empty())
}

fn save_ui(ui: &mut Ui, name: &mut String, noun: &str, all_text: &str, all: &mut bool) -> Btn {
    let l = ui.label("Name of new view:");
    let r = ui.add(TextEdit::singleline(name).desired_width(260.0)).labelled_by(l.id);
    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !name.trim().is_empty() {
        return Btn::Ok;
    }
    ui.label("Can be used on:");
    ui.radio_value(all, false, format!("This {} folder", noun.to_lowercase()));
    let every = if all_text.is_empty() { format!("All {noun} folders") } else { all_text.to_owned() };
    ui.radio_value(all, true, every);
    ok_cancel(ui, "OK", !name.trim().is_empty())
}

fn merge_ui(ui: &mut Ui, app: &mut App, f: &mut MergeForm, nonce: u64) -> Btn {
    let mut out = Btn::None;
    ui.label(RichText::new("Contacts").strong());
    let mut changed = false;
    changed |= ui.radio_value(&mut f.contacts, "all".to_owned(), "All contacts in current view").changed();
    ui.add_enabled_ui(!f.selected.is_empty(), |ui| {
        changed |= ui.radio_value(&mut f.contacts, "selected".to_owned(), "Only selected contacts").changed();
    });
    ui.add_space(6.0);
    ui.label(RichText::new("Merge options").strong());
    ui.horizontal(|ui| {
        ui.label("Document type");
        for (k, l) in [("letters", "Form Letters"), ("labels", "Mailing Labels"), ("envelopes", "Envelopes")]
        {
            ui.add_enabled(false, egui::RadioButton::new(f.output == k, l))
                .on_disabled_hover_text("Needs a word processor; merge to E-mail instead.");
        }
    });
    ui.horizontal(|ui| {
        ui.label("Merge to");
        ui.radio_value(&mut f.output, "email".to_owned(), "E-mail");
    });
    egui::Grid::new(("mc-merge-grid", nonce)).num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        let l = ui.label("Message subject line");
        changed |=
            ui.add(TextEdit::singleline(&mut f.subject).desired_width(320.0)).labelled_by(l.id).changed();
        ui.end_row();
        ui.label("Insert merge field");
        ComboBox::from_id_salt(("mc-merge-field", nonce)).selected_text("Choose…").width(200.0).show_ui(
            ui,
            |ui| {
                for field in f.fields.clone() {
                    if ui.selectable_label(false, &field).clicked() {
                        f.body.push_str(&format!("{{{{{field}}}}}"));
                        changed = true;
                    }
                }
            },
        );
        ui.end_row();
    });
    changed |= ui
        .add(
            TextEdit::multiline(&mut f.body)
                .desired_rows(6)
                .desired_width(f32::INFINITY)
                .hint_text("Message text"),
        )
        .changed();
    if changed {
        f.preview = None;
    }
    if f.preview.is_none() {
        let p = merge_params(f, &[("action", json!("preview")), ("index", json!(f.index))]);
        f.preview =
            Some(app.s.run("people.mailMerge", &p).unwrap_or_else(|e| json!({ "error": e.to_string() })));
    }
    ui.separator();
    if let Some(pv) = f.preview.clone() {
        let count = pv.get("count").and_then(Value::as_u64).unwrap_or(0);
        let skipped = pv.get("skipped").and_then(Value::as_u64).unwrap_or(0);
        if let Some(e) = pv.get("error").and_then(Value::as_str) {
            ui.colored_label(app.theme.danger, e);
        } else {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Preview").strong());
                if ui.add_enabled(f.index > 0, egui::Button::new("◀")).clicked() {
                    f.index = f.index.saturating_sub(1);
                    f.preview = None;
                }
                ui.label(format!("{} of {count}", if count == 0 { 0 } else { f.index.saturating_add(1) }));
                if ui
                    .add_enabled((f.index as u64).saturating_add(1) < count, egui::Button::new("▶"))
                    .clicked()
                {
                    f.index = f.index.saturating_add(1);
                    f.preview = None;
                }
                if skipped > 0 {
                    ui.label(RichText::new(format!("{skipped} without an email address skipped")).weak());
                }
            });
            if let Some(m) = pv.get("preview").filter(|m| !m.is_null()) {
                ui.label(format!("To: {}", m.get("to").and_then(Value::as_str).unwrap_or("")));
                ui.label(format!("Subject: {}", m.get("subject").and_then(Value::as_str).unwrap_or("")));
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, app.theme.separator))
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ScrollArea::vertical().id_salt(("mc-merge-preview", nonce)).max_height(140.0).show(
                            ui,
                            |ui| {
                                ui.label(m.get("body").and_then(Value::as_str).unwrap_or(""));
                            },
                        );
                    });
            }
        }
    }
    if let Some(r) = &f.result {
        ui.label(RichText::new(r).color(app.theme.accent));
    }
    ui.horizontal(|ui| {
        let b = ok_cancel(ui, "Send", !f.subject.trim().is_empty());
        if b != Btn::None {
            out = b;
        }
    });
    out
}

fn merge_params(f: &MergeForm, extra: &[(&str, Value)]) -> Value {
    let mut p = serde_json::Map::new();
    p.insert("contacts".into(), json!(f.contacts));
    p.insert("output".into(), json!(f.output));
    p.insert("subject".into(), json!(f.subject));
    p.insert("body".into(), json!(f.body));
    if f.contacts == "selected" {
        p.insert("ids".into(), json!(f.selected));
    }
    for (k, v) in extra {
        p.insert((*k).to_owned(), v.clone());
    }
    Value::Object(p)
}

const REQUEST_PRIORITIES: [(&str, &str); 3] = [("low", "Low"), ("normal", "Normal"), ("high", "High")];

fn request_ui(ui: &mut Ui, app: &App, f: &mut RequestForm, nonce: u64) -> Btn {
    let now = today(app);
    egui::Grid::new(("mc-request-grid", nonce)).num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        let l = ui.label("To");
        ui.add(TextEdit::singleline(&mut f.to).hint_text("Name <address>; …").desired_width(f32::INFINITY))
            .labelled_by(l.id);
        ui.end_row();
        let l = ui.label("Subject");
        ui.add(TextEdit::singleline(&mut f.subject).desired_width(f32::INFINITY)).labelled_by(l.id);
        ui.end_row();
        for (label, value, id) in
            [("Start date", &mut f.start, "mc-req-start"), ("Due date", &mut f.due, "mc-req-due")]
        {
            ui.label(label);
            ui.horizontal(|ui| {
                date_field(ui, value, id);
                if ui.small_button("Today").clicked() {
                    *value = day_string(now);
                }
                if ui.small_button("None").clicked() {
                    value.clear();
                }
            });
            ui.end_row();
        }
        ui.label("Status");
        let cur = STATUSES.iter().find(|(k, _)| *k == f.status).map_or("Not Started", |(_, l)| *l);
        ComboBox::from_id_salt(("mc-req-status", nonce)).selected_text(cur).width(200.0).show_ui(ui, |ui| {
            for (k, l) in STATUSES {
                ui.selectable_value(&mut f.status, k.to_owned(), l);
            }
        });
        ui.end_row();
        ui.label("Priority");
        let cur = REQUEST_PRIORITIES.iter().find(|(k, _)| *k == f.priority).map_or("Normal", |(_, l)| *l);
        ComboBox::from_id_salt(("mc-req-prio", nonce)).selected_text(cur).width(200.0).show_ui(ui, |ui| {
            for (k, l) in REQUEST_PRIORITIES {
                ui.selectable_value(&mut f.priority, k.to_owned(), l);
            }
        });
        ui.end_row();
        ui.label("Recurrence");
        let choices = recurrence_choices(parse_day(&f.due));
        let cur =
            choices.iter().find(|(_, r)| *r == f.recurrence).map_or("Custom".to_owned(), |(l, _)| l.clone());
        ComboBox::from_id_salt(("mc-req-rec", nonce)).selected_text(cur).width(200.0).show_ui(ui, |ui| {
            for (label, rule) in &choices {
                ui.selectable_value(&mut f.recurrence, rule.clone(), label);
            }
        });
        ui.end_row();
    });
    ui.checkbox(&mut f.keep_copy, "Keep an updated copy of this task on my task list");
    ui.checkbox(&mut f.status_report, "Send me a status report when this task is complete");
    ui.add(TextEdit::multiline(&mut f.body).hint_text("Notes").desired_rows(4).desired_width(f32::INFINITY));
    ok_cancel(ui, "Send", !f.to.trim().is_empty() && !f.subject.trim().is_empty())
}

/// OK / Send: runs the dialog's command. `Ok(true)` closes the dialog.
fn apply(app: &mut App, kind: &mut Kind) -> Result<bool, String> {
    match kind {
        Kind::Move { ids, area, copy, folder } => {
            let Some(folder) = folder else { return Ok(false) };
            let id = format!("{area}.{}", if *copy { "copy" } else { "move" });
            run_checked(app, &id, json!({ "folder": folder, "ids": ids }))?;
            Ok(true)
        }
        Kind::Flag(f) => {
            let day = |s: &str, what: &str| -> Result<Value, String> {
                if s.trim().is_empty() {
                    Ok(Value::Null)
                } else {
                    parse_day(s)
                        .map(|d| json!(day_string(d)))
                        .ok_or_else(|| format!("Enter the {what} as YYYY-MM-DD."))
                }
            };
            let reminder = if f.reminder_on {
                let d = parse_day(&f.reminder_date).ok_or("Enter the reminder date as YYYY-MM-DD.")?;
                json!(day_time_string(d, f.reminder_min))
            } else {
                Value::Null
            };
            let (start, due) = (day(&f.start, "start date")?, day(&f.due, "due date")?);
            if f.reminder_only && reminder.is_null() {
                return Err("Choose when to be reminded.".into());
            }
            if f.area == "tasks" && start.is_null() && due.is_null() && reminder.is_null() {
                return Err("Enter a start date, a due date or a reminder.".into());
            }
            let mut p = json!({ "ids": f.ids, "kind": if f.reminder_only { "reminder" } else { "custom" } });
            if let Some(o) = p.as_object_mut() {
                if !f.reminder_only {
                    o.insert("start".into(), start);
                    o.insert("due".into(), due);
                    if f.area == "people" {
                        let label = if f.label.trim().is_empty() { "Follow up" } else { f.label.trim() };
                        o.insert("label".into(), json!(label));
                    }
                }
                if !reminder.is_null() {
                    o.insert("reminder".into(), reminder);
                }
            }
            run_checked(app, &format!("{}.flag", f.area), p)?;
            Ok(true)
        }
        Kind::ViewSettings(f) => {
            let init = f.initial.as_ref().clone().unwrap_or_default();
            if f.view != init.view {
                run_checked(app, "view.changeView", json!({ "name": f.view }))?;
            }
            if f.arrange != init.arrange
                && let Some(a) = &f.arrange
            {
                run_checked(app, "view.arrangeBy", json!({ "key": a }))?;
            }
            if f.groups != init.groups {
                run_checked(app, "view.showInGroups", json!({}))?;
            }
            if f.reverse != init.reverse {
                run_checked(app, "view.reverseSort", json!({}))?;
            }
            if f.preview != init.preview {
                run_checked(app, "view.messagePreview", json!({ "lines": f.preview }))?;
            }
            if f.pane != init.pane && !f.pane.is_empty() {
                run_checked(app, "view.readingPane", json!({ "position": f.pane }))?;
            }
            if f.spacing != init.spacing {
                run_checked(app, "view.tighterSpacing", json!({}))?;
            }
            if f.module == "notes" && f.advanced_changed(&init) {
                let rules: Vec<ItemRule> =
                    f.rules.iter().filter(|r| !r.name.trim().is_empty()).cloned().collect();
                run_checked(
                    app,
                    "view.viewSettings",
                    json!({
                        "sort": f.sort,
                        "columns": f.columns,
                        "filter": f.filter,
                        "rules": rules,
                        "autoPreview": f.auto_preview,
                        "gridLines": f.grid_lines,
                    }),
                )?;
            }
            // Opened from Manage All Views › Modify: OK stores the view into that saved view. Otherwise a saved
            // view that shows keeps what was changed while it shows.
            let saved_shown =
                !f.current.is_empty() && !f.views.iter().any(|v| v.eq_ignore_ascii_case(&f.current));
            if !f.saved_view.is_empty() {
                run_checked(app, "view.manageViews", json!({ "op": "update", "name": f.saved_view }))?;
            } else if saved_shown && f.view == init.view {
                run_checked(app, "view.manageViews", json!({ "op": "update", "name": f.current }))?;
            }
            Ok(true)
        }
        Kind::ApplyView { picked, subfolders, .. } => {
            let folders: Vec<u64> = picked.iter().copied().collect();
            run_checked(app, "view.applyViewTo", json!({ "folders": folders, "subfolders": *subfolders }))?;
            Ok(true)
        }
        Kind::SaveView { name, all, .. } => {
            let scope = if *all { "all" } else { "folder" };
            run_checked(
                app,
                "view.saveViewAs",
                json!({ "name": name.trim(), "scope": scope, "confirm": true }),
            )?;
            Ok(true)
        }
        Kind::MailMerge(f) => {
            let p = merge_params(f, &[("action", json!("send"))]);
            let r = run_checked(app, "people.mailMerge", p)?;
            let sent = r.get("sent").and_then(Value::as_u64).unwrap_or(0);
            app.ui.toast = Some((format!("Mail Merge: {sent} message(s) sent to the Outbox."), 0.0));
            Ok(true)
        }
        Kind::DeleteRequest { ids, choice, done, .. } => {
            run_checked(app, "tasks.delete", json!({ "ids": ids, "request": choice }))?;
            *done = true;
            super::super::common::select_item(app, None);
            Ok(true)
        }
        Kind::TaskRequest(f) => {
            let date = |s: &str, what: &str| -> Result<Value, String> {
                if s.trim().is_empty() {
                    Ok(Value::Null)
                } else {
                    parse_day(s)
                        .map(|d| json!(day_string(d)))
                        .ok_or_else(|| format!("Enter the {what} as YYYY-MM-DD."))
                }
            };
            let mut p = json!({
                "to": f.to,
                "subject": f.subject.trim(),
                "start": date(&f.start, "start date")?,
                "due": date(&f.due, "due date")?,
                "status": f.status,
                "priority": f.priority,
                "body": f.body,
                "keepCopy": f.keep_copy,
                "statusReport": f.status_report,
            });
            if let Some(o) = p.as_object_mut() {
                if !f.recurrence.is_empty() {
                    o.insert("recurrence".into(), json!(f.recurrence));
                }
                if let Some(folder) = f.folder {
                    o.insert("folder".into(), json!(folder));
                }
            }
            let r = run_checked(app, "tasks.assign", p)?;
            if let Some(id) = r.get("task").and_then(Value::as_u64) {
                super::super::common::select_item(app, Some(id));
            }
            Ok(true)
        }
    }
}

/// The dialog's second button (Clear Flag, Reset Current View).
fn other(app: &mut App, kind: &mut Kind) -> Result<bool, String> {
    match kind {
        Kind::Flag(f) => {
            run_checked(app, &format!("{}.flag", f.area), json!({ "ids": f.ids, "kind": "clear" }))?;
            Ok(true)
        }
        Kind::ViewSettings(_) => {
            run_checked(app, "view.resetView", json!({}))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new(mailcraft_engine::sample::sample_session())
    }

    #[test]
    fn open_parses_params_and_ignores_others() {
        let app = app();
        let mut st = ModState::default();
        open(&app, &mut st, "pimMove", &json!({"ids": [1, 2], "area": "people", "copy": true}));
        open(&app, &mut st, "nope", &json!({}));
        open(&app, &mut st, "pimFlag", &json!({"ids": "junk", "area": "people"}));
        assert_eq!(st.dialogs.open.len(), 2);
        assert!(matches!(&st.dialogs.open[0].kind, Kind::Move { ids, copy: true, .. } if ids == &vec![1, 2]));
        assert!(matches!(&st.dialogs.open[1].kind, Kind::Flag(f) if f.ids.is_empty() && f.area == "people"));
    }

    #[test]
    fn move_applies_through_commands() {
        let mut app = app();
        let tasks =
            app.s.mailbox.folders.values().find(|f| f.kind == FolderKind::Tasks).map(|f| f.id).unwrap();
        let sub = app.s.mailbox.create_folder(tasks, "Errands").unwrap();
        let id = app.s.mailbox.tasks.keys().next().copied().unwrap();
        super::super::super::common::select_item(&mut app, Some(id.0));
        let mut k = Kind::Move { ids: vec![id.0], area: "tasks".into(), copy: false, folder: Some(sub.0) };
        assert_eq!(apply(&mut app, &mut k), Ok(true));
        assert_eq!(app.s.mailbox.tasks[&id].folder, sub);
    }

    #[test]
    fn delete_request_dialog_defaults_to_decline_and_deletes() {
        let mut app = app();
        let mut st = ModState::default();
        let id = app.s.mailbox.tasks.keys().next().copied().unwrap();
        let folder = app.s.mailbox.tasks[&id].folder;
        open(&app, &mut st, "deleteTaskRequest", &json!({"ids": [id.0], "subject": "Budget"}));
        let d = st.dialogs.open.first_mut().unwrap();
        let Kind::DeleteRequest { choice, ids, .. } = &mut d.kind else { panic!("not the delete question") };
        assert_eq!((choice.as_str(), ids.as_slice()), ("decline", &[id.0][..]));
        *choice = "delete".into();
        assert_eq!(apply(&mut app, &mut d.kind), Ok(true));
        assert!(app.s.mailbox.tasks.get(&id).is_none_or(|t| t.folder != folder), "the task was deleted");
    }

    #[test]
    fn deleting_a_requested_task_from_its_window_keeps_the_edits_until_the_question_is_answered() {
        // Regression: Delete in the window of an accepted task request closed the window at once, so
        // Cancel in the question that followed lost every unsaved edit.
        let mut app = app();
        let mut st = ModState::default();
        let id = app.s.mailbox.tasks.values().find(|t| !t.is_complete()).map(|t| t.id).unwrap();
        if let Some(t) = app.s.mailbox.tasks.get_mut(&id) {
            t.requested_by = Some("Boss <boss@example.com>".into());
            t.request_state = mailcraft_model::task::TaskRequestState::Accepted;
        }
        let folder = app.s.mailbox.tasks[&id].folder;
        super::super::open_task(&app, &mut st, id);
        let mut ed = st.editors.task.take().unwrap();
        ed.body = "Unsaved notes".into();
        app.s.notices.clear();
        assert!(
            !super::super::delete_from_window(&mut app, &ed),
            "the window stays while the question is asked"
        );
        assert_eq!(app.s.mailbox.tasks[&id].folder, folder, "nothing deleted yet");
        let params = app
            .s
            .notices
            .iter()
            .find_map(|n| match n {
                mailcraft_engine::Notice::OpenDialog(name, p) if name == "deleteTaskRequest" => {
                    Some(p.clone())
                }
                _ => None,
            })
            .unwrap();
        st.editors.task = Some(ed);
        // Cancel: the question closes without deleting; the window and its edits are still there.
        open(&app, &mut st, "deleteTaskRequest", &params);
        let d = st.dialogs.open.pop().unwrap();
        let Kind::DeleteRequest { ids, done, .. } = &d.kind else { panic!("not the delete question") };
        assert!(!done);
        if *done {
            close_deleted_task(&mut st, ids);
        }
        assert_eq!(st.editors.task.as_ref().map(|e| e.body.as_str()), Some("Unsaved notes"));
        assert_eq!(app.s.mailbox.tasks[&id].folder, folder);
        // OK: the task is deleted, then its window closes.
        open(&app, &mut st, "deleteTaskRequest", &params);
        let mut d = st.dialogs.open.pop().unwrap();
        if let Kind::DeleteRequest { choice, .. } = &mut d.kind {
            *choice = "delete".into();
        }
        assert_eq!(apply(&mut app, &mut d.kind), Ok(true));
        let Kind::DeleteRequest { ids, done: true, .. } = &d.kind else { panic!("not marked deleted") };
        close_deleted_task(&mut st, ids);
        assert!(st.editors.task.is_none());
        assert!(app.s.mailbox.tasks.get(&id).is_none_or(|t| t.folder != folder));
        // A plain task deletes straight from its window.
        let plain = app.s.mailbox.tasks.values().find(|t| t.folder == folder).map(|t| t.id).unwrap();
        super::super::open_task(&app, &mut st, plain);
        let ed = st.editors.task.take().unwrap();
        assert!(super::super::delete_from_window(&mut app, &ed));
    }

    #[test]
    fn notes_view_settings_apply_advanced_parts_and_keep_them_in_a_saved_view() {
        use mailcraft_engine::cmd::view::modules::{self, ModuleKind};
        let mut app = app();
        let notes =
            app.s.mailbox.folders.values().find(|f| f.kind == FolderKind::Notes).map(|f| f.id).unwrap();
        app.run("nav.selectFolder", json!({"folder": notes.0}));
        app.run("view.changeView", json!({"name": "Notes List"}));
        app.run("view.saveViewAs", json!({"name": "Mine", "confirm": true}));
        app.run("view.viewSettings", json!({}));
        let mut st = ModState::default();
        let Some(mailcraft_engine::Notice::OpenDialog(name, p)) = app.s.notices.last().cloned() else {
            panic!("no dialog")
        };
        open(&app, &mut st, &name, &p);
        let Some(Dialog { kind: Kind::ViewSettings(mut f), .. }) = st.dialogs.open.pop() else {
            panic!("no view settings form")
        };
        assert_eq!(f.current, "Mine");
        assert_eq!(f.columns, vec!["icon", "subject", "created", "categories"]);
        f.columns.retain(|c| c != "categories");
        f.filter.words = "wifi".into();
        f.grid_lines = false;
        f.rules.push(ItemRule { name: "Guests".into(), on: true, bold: true, ..Default::default() });
        assert_eq!(apply(&mut app, &mut Kind::ViewSettings(f)), Ok(true));
        let st = modules::state(&app.s, notes, ModuleKind::Notes);
        assert_eq!(st.columns, vec!["icon", "subject", "created"]);
        assert_eq!(st.filter.words, "wifi");
        assert!(st.no_grid);
        let saved = modules::saved_views(&app.s, notes).into_iter().find(|v| v.name == "Mine").unwrap();
        assert_eq!(saved.state.filter.words, "wifi", "the saved view shown keeps the changes");
    }

    #[test]
    fn task_request_and_view_settings_apply() {
        let mut app = app();
        let mut k = Kind::TaskRequest(RequestForm {
            to: "Pat <pat@example.org>".into(),
            subject: "Collect receipts".into(),
            due: "2026-10-20".into(),
            status: "notStarted".into(),
            priority: "high".into(),
            keep_copy: true,
            ..Default::default()
        });
        assert_eq!(apply(&mut app, &mut k), Ok(true));
        let t = app.s.mailbox.tasks.values().find(|t| t.subject == "Collect receipts").unwrap();
        assert_eq!(t.assigned_to, vec!["Pat <pat@example.org>".to_string()]);
        let mut bad = Kind::TaskRequest(RequestForm {
            to: "x".into(),
            subject: "y".into(),
            due: "soon".into(),
            ..Default::default()
        });
        assert!(apply(&mut app, &mut bad).is_err());
        // View settings: switch the tasks folder to Detailed, grouped by importance, reversed.
        let tasks =
            app.s.mailbox.folders.values().find(|f| f.kind == FolderKind::Tasks).map(|f| f.id).unwrap();
        app.run("nav.selectFolder", json!({"folder": tasks.0}));
        let init = ViewForm {
            module: "tasks".into(),
            view: "To-Do List".into(),
            groups: true,
            pane: "right".into(),
            ..Default::default()
        };
        let mut f = init.clone();
        f.view = "Detailed".into();
        f.arrange = Some("importance".into());
        f.reverse = true;
        f.initial = Box::new(Some(init));
        assert_eq!(apply(&mut app, &mut Kind::ViewSettings(Box::new(f))), Ok(true));
        let st = mailcraft_engine::cmd::view::modules::state(
            &app.s,
            tasks,
            mailcraft_engine::cmd::view::modules::ModuleKind::Tasks,
        );
        assert_eq!((st.view.as_str(), st.arrange.as_deref()), ("Detailed", Some("importance")));
        assert!(mailcraft_engine::cmd::view::modules::reversed(&app.s, tasks));
    }
}
