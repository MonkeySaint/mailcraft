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
    MailMerge(Box<MergeForm>),
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
    /// The dates the dialog opened with: OK on Tasks sends only the fields changed from these, so rows of a
    /// multiple selection (which opens blank) keep their own dates.
    pub initial: FlagDates,
}

/// The date fields of the Custom follow-up dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FlagDates {
    pub start: String,
    pub due: String,
    pub reminder_on: bool,
    pub reminder_date: String,
    pub reminder_min: i64,
}

impl FlagForm {
    fn dates(&self) -> FlagDates {
        FlagDates {
            start: self.start.trim().to_owned(),
            due: self.due.trim().to_owned(),
            reminder_on: self.reminder_on,
            reminder_date: self.reminder_date.trim().to_owned(),
            reminder_min: self.reminder_min,
        }
    }
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
    /// Notes tables: Sort › Then by keys (column key, descending), Group By fields, Expand/collapse
    /// defaults and fonts (0 = the view's own).
    pub then_sort: Vec<(String, bool)>,
    pub group_by: Vec<(String, bool)>,
    pub expand: String,
    pub row_font: u8,
    pub column_font: u8,
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
    pub group: String,
    pub other: String,
    /// The view is a table (Columns, Other Settings and Format Columns apply).
    pub table: bool,
    /// The view shows columns Show Columns picks (a table, or the Card view's fields).
    pub has_columns: bool,
    /// The view groups (Group By applies).
    pub grouped: bool,
}

impl TableSummary {
    /// From the engine's table summary (`None` when the view keeps no table settings).
    fn of(t: &Value) -> Option<TableSummary> {
        if t.get("settings").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let flag = |k: &str| t.get(k).and_then(Value::as_bool).unwrap_or(false);
        Some(TableSummary {
            columns: str_of(t, "columns"),
            sort: str_of(t, "sort"),
            filter: str_of(t, "filter"),
            rules: str_of(t, "rules"),
            formats: str_of(t, "formats"),
            group: str_of(t, "group"),
            other: str_of(t, "other"),
            table: flag("table"),
            has_columns: flag("hasColumns"),
            grouped: flag("grouped"),
        })
    }
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
            || self.group_by != o.group_by
            || self.expand != o.expand
            || self.row_font != o.row_font
            || self.column_font != o.column_font
    }
}

/// `(field, descending)` pairs of an engine `[{"field", "dir"}]` list.
fn fields_of(v: Option<&Value>) -> Vec<(String, bool)> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|f| {
                    Some((
                        f.get("field")?.as_str()?.to_owned(),
                        f.get("dir").and_then(Value::as_str) == Some("desc"),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn fields_json(list: &[(String, bool)]) -> Value {
    Value::Array(
        list.iter()
            .filter(|(k, _)| !k.is_empty())
            .map(|(k, d)| json!({"field": k, "dir": if *d { "desc" } else { "asc" }}))
            .collect(),
    )
}

#[derive(Clone, Default)]
pub(crate) struct MergeForm {
    /// "all" (the contacts of the current view) or "selected".
    pub contacts: String,
    /// The People search the view shows when the window opened ("All contacts in current view" honours it).
    pub query: String,
    /// Fields to merge: "all" (every contact field) or "view" (the fields of the current view).
    pub fields_choice: String,
    pub all_fields: Vec<String>,
    pub view_fields: Vec<String>,
    /// Document file: an existing document whose text is merged ("" = type the text here).
    pub template: String,
    pub use_template: bool,
    /// The document whose text is in `body` (loaded with Load or Browse).
    pub template_loaded: String,
    /// Document type: "letters", "labels", "envelopes" or "catalog".
    pub doc: String,
    /// Merge to: "document" (a new PDF), "printer" or "email" (form letters only).
    pub to: String,
    /// The new document's file, the printer.
    pub path: String,
    pub printer: String,
    pub printers: Vec<String>,
    /// Label sheet and envelope size ids, and the choices (id, label).
    pub labels: String,
    pub envelope: String,
    pub label_sheets: Vec<(String, String)>,
    pub envelope_sizes: Vec<(String, String)>,
    pub return_address: String,
    /// Save the contact data file, and where.
    pub data_file_on: bool,
    pub data_file: String,
    /// The contacts selected when the window opened.
    pub selected: Vec<u64>,
    pub fields: Vec<String>,
    pub subject: String,
    pub body: String,
    /// The standard address block labels and envelopes start from.
    pub address_block: String,
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

/// `[{"id", "label"}]` entries as (id, label) pairs.
fn pairs_of(p: &Value, key: &str) -> Vec<(String, String)> {
    p.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some((x.get("id")?.as_str()?.to_owned(), x.get("label")?.as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
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
        "pimFlag" => {
            // The current dates of the item (Tasks sends them), so None can clear one.
            let at = super::super::common::parse_day_time(&str_of(p, "reminderAt"));
            let (at_day, at_min) = (at.map(|(d, _)| day_string(d)), at.map(|(_, m)| m));
            let mut f = FlagForm {
                ids: ids_of(p),
                area: str_of(p, "area"),
                reminder_only: p.get("reminder").and_then(Value::as_bool).unwrap_or(false),
                label: "Follow up".into(),
                start: str_of(p, "start"),
                due: str_of(p, "due"),
                reminder_on: p.get("reminder").and_then(Value::as_bool).unwrap_or(false) || at_day.is_some(),
                reminder_date: at_day.unwrap_or_else(|| day_string(today(app))),
                reminder_min: at_min.unwrap_or(16 * 60),
                initial: FlagDates::default(),
            };
            f.initial = f.dates();
            Kind::Flag(f)
        }
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
                table: p.get("table").and_then(TableSummary::of),
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
                // The first sort key is `sort` (and Reverse Sort); the rest are the Then by keys.
                then_sort: fields_of(p.get("sortBy")).into_iter().skip(1).collect(),
                group_by: fields_of(p.get("groupBy")),
                expand: str_of(p, "expandGroups"),
                row_font: p
                    .get("rowFont")
                    .and_then(Value::as_u64)
                    .and_then(|n| u8::try_from(n).ok())
                    .unwrap_or(0),
                column_font: p
                    .get("columnFont")
                    .and_then(Value::as_u64)
                    .and_then(|n| u8::try_from(n).ok())
                    .unwrap_or(0),
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
        "mailMerge" => Kind::MailMerge(Box::new({
            let doc = match str_of(p, "output").as_str() {
                o @ ("labels" | "envelopes" | "catalog") => o.to_owned(),
                _ => "letters".to_owned(),
            };
            let address_block = Some(str_of(p, "addressBlock"))
                .filter(|b| !b.trim().is_empty())
                .unwrap_or_else(|| mailcraft_engine::cmd::people::ADDRESS_BLOCK.to_owned());
            let body = start_text(&doc, &address_block);
            MergeForm {
                contacts: if str_of(p, "contacts") == "selected" { "selected".into() } else { "all".into() },
                // Form letters to a new document unless E-mail was asked for.
                doc,
                to: if str_of(p, "output") == "email" { "email".into() } else { "document".into() },
                path: format!("{}/Mail Merge.pdf", crate::backstage::home()),
                printer: mailcraft_engine::cmd::print::DEFAULT_PRINTER.to_owned(),
                labels: "30".into(),
                envelope: "10".into(),
                label_sheets: pairs_of(p, "labels"),
                envelope_sizes: pairs_of(p, "envelopes"),
                data_file: format!("{}/Mail Merge Data.csv", crate::backstage::home()),
                selected: app
                    .s
                    .selection
                    .items
                    .iter()
                    .filter(|i| app.s.mailbox.contacts.contains_key(i))
                    .map(|i| i.0)
                    .collect(),
                fields: strings_of(p, "fields"),
                fields_choice: if str_of(p, "fieldsChoice") == "view" { "view".into() } else { "all".into() },
                all_fields: strings_of(p, "allFields"),
                view_fields: strings_of(p, "viewFields"),
                query: st.people_search.trim().to_owned(),
                subject: String::new(),
                body,
                address_block,
                ..Default::default()
            }
        })),
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
        .id(Id::unique(("mc-module-dialog", nonce)))
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

/// The table parts of View Settings: a button per part (it opens that dialog) with what it is set to. Parts
/// a view does not have are greyed out (a card view has no Format Columns; the Card view's columns are the
/// fields of its cards).
fn table_parts(ui: &mut Ui, app: &mut App, t: &TableSummary, nonce: u64, card: bool) {
    let not_here = "Not available in this view".to_owned();
    let or_not = |on: bool, s: &str| if on { s.to_owned() } else { not_here.clone() };
    // (button, command or UI dialog ("@name"), description, enabled)
    let parts: [(&str, &str, String, bool); 7] = [
        (
            if card { "Fields..." } else { "Columns..." },
            "view.addColumns",
            or_not(t.has_columns, &t.columns),
            t.has_columns,
        ),
        ("Group By...", "@viewGroupBy", or_not(t.grouped, &t.group), t.grouped),
        ("Sort...", "@viewSort", t.sort.clone(), true),
        ("Filter...", "view.customFilter", t.filter.clone(), true),
        ("Other Settings...", "@viewOtherSettings", or_not(t.table, &t.other), t.table),
        ("Conditional Formatting...", "view.conditionalFormatting", t.rules.clone(), true),
        ("Format Columns...", "view.formatColumns", or_not(t.table, &t.formats), t.table),
    ];
    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(6.0).show(ui, |ui| {
        ui.label(RichText::new("Description").strong());
        egui::Grid::new(("mc-viewset-parts", nonce)).num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
            for (label, cmd, desc, enabled) in parts {
                if ui.add_enabled(enabled, egui::Button::new(label).min_size(vec2(180.0, 22.0))).clicked() {
                    if let Some(name) = cmd.strip_prefix('@') {
                        let folder = app.s.selection.folder.0;
                        crate::dialogs::open(app, name, json!({ "folder": folder }));
                    } else {
                        app.run(cmd, json!({}));
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
        let t = live.as_ref().and_then(TableSummary::of).unwrap_or_else(|| t.clone());
        let card = mailcraft_engine::cmd::view::modules::current(&app.s, folder)
            .is_some_and(|(_, st)| st.view == "Card");
        table_parts(ui, app, &t, nonce, card);
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
        advanced_ui(ui, app, f, nonce);
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
/// Expand/collapse defaults (engine key, label).
const EXPAND: [(&str, &str); 3] =
    [("expanded", "All expanded"), ("collapsed", "All collapsed"), ("lastViewed", "As last viewed")];

/// Up to `max` rows of (field, direction) for Notes Sort › Then by or Group By; an empty row ends the list
/// (the rows after it are greyed out). `cols` are (key, label).
fn note_levels(
    ui: &mut Ui,
    list: &mut Vec<(String, bool)>,
    cols: &[(String, String)],
    id: (&str, u64),
    first: &str,
    max: usize,
) {
    let mut rows: Vec<(String, bool)> = list.clone();
    rows.resize(max, (String::new(), false));
    let mut open = true;
    egui::Grid::new(id).num_columns(3).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
        for (i, (key, desc)) in rows.iter_mut().enumerate() {
            ui.label(if i == 0 { first } else { "Then by" });
            let cur = cols.iter().find(|(k, _)| k == key).map_or("(none)".to_owned(), |(_, l)| l.clone());
            ui.add_enabled_ui(open, |ui| {
                ComboBox::from_id_salt((id.0, id.1, i)).selected_text(cur).width(170.0).show_ui(ui, |ui| {
                    ui.selectable_value(key, String::new(), "(none)");
                    for (k, l) in cols {
                        ui.selectable_value(key, k.clone(), l);
                    }
                });
            });
            ui.add_enabled_ui(open && !key.is_empty(), |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(desc, false, "Ascending");
                    ui.radio_value(desc, true, "Descending");
                });
            });
            ui.end_row();
            open &= !key.is_empty();
        }
    });
    *list = rows.into_iter().take_while(|(k, _)| !k.is_empty()).collect();
}

fn advanced_ui(ui: &mut Ui, app: &mut App, f: &mut ViewForm, nonce: u64) {
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
            let cols = f.all_columns.clone();
            note_levels(ui, &mut f.then_sort, &cols, ("mc-viewset-then", nonce), "Then by", 3);
        });
    });
    egui::CollapsingHeader::new("Group By").id_salt(("mc-viewset-group", nonce)).show(ui, |ui| {
        ui.add_enabled_ui(table, |ui| {
            // The icon column groups by the note colour.
            let cols: Vec<(String, String)> = f
                .all_columns
                .iter()
                .map(|(k, l)| (k.clone(), if k == "icon" { "Color".to_owned() } else { l.clone() }))
                .collect();
            note_levels(ui, &mut f.group_by, &cols, ("mc-viewset-groupby", nonce), "Group items by", 4);
            ui.horizontal(|ui| {
                ui.label("Expand/collapse defaults");
                let cur = EXPAND.iter().find(|(k, _)| *k == f.expand).map_or("All expanded", |(_, l)| *l);
                ComboBox::from_id_salt(("mc-viewset-expand", nonce)).selected_text(cur).show_ui(ui, |ui| {
                    for (k, l) in EXPAND {
                        ui.selectable_value(&mut f.expand, k.to_owned(), l);
                    }
                });
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
            for (label, slot, salt) in [
                ("Column font", &mut f.column_font, "mc-viewset-colfont"),
                ("Row font", &mut f.row_font, "mc-viewset-rowfont"),
            ] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    let text = |pt: u8| if pt == 0 { "Default".to_owned() } else { format!("{pt} pt") };
                    ComboBox::from_id_salt((salt, nonce)).selected_text(text(*slot)).show_ui(ui, |ui| {
                        for pt in [0u8, 8, 9, 10, 11, 12, 14, 16, 18, 20, 24] {
                            ui.selectable_value(slot, pt, text(pt));
                        }
                    });
                });
            }
        });
    });
    ui.add_enabled_ui(table, |ui| {
        if ui.button("Format Columns...").clicked() {
            app.run("view.formatColumns", json!({}));
        }
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
    if f.contacts == "all" && !f.query.is_empty() {
        ui.label(RichText::new(format!("The view is searched for \u{201c}{}\u{201d}.", f.query)).weak());
    }
    ui.add_space(4.0);
    ui.label(RichText::new("Fields to merge").strong());
    ui.horizontal(|ui| {
        let before = f.fields_choice.clone();
        ui.radio_value(&mut f.fields_choice, "all".to_owned(), "All contact fields");
        ui.add_enabled_ui(!f.view_fields.is_empty(), |ui| {
            ui.radio_value(&mut f.fields_choice, "view".to_owned(), "Contact fields in current view");
        });
        if f.fields_choice != before {
            f.fields = if f.fields_choice == "view" { f.view_fields.clone() } else { f.all_fields.clone() };
        }
    });
    ui.add_space(4.0);
    ui.label(RichText::new("Document file").strong());
    ui.horizontal(|ui| {
        ui.radio_value(&mut f.use_template, false, "New document");
        ui.radio_value(&mut f.use_template, true, "Existing document");
    });
    ui.horizontal(|ui| {
        ui.add_enabled_ui(f.use_template, |ui| {
            let l = ui.label("Document:");
            changed |= ui
                .add(TextEdit::singleline(&mut f.template).desired_width(260.0).hint_text("letter.txt"))
                .labelled_by(l.id)
                .changed();
            if crate::dialogs::browse_button(ui, app)
                && let Some(path) = crate::dialogs::browse_open(
                    app,
                    "Existing Document",
                    &[crate::FileFilter::new(
                        "Text, web page or message",
                        &["txt", "htm", "html", "eml", "md"],
                    )],
                    &f.template,
                )
            {
                f.template = path;
                load_template(app, f);
                changed = true;
            }
            if ui.add_enabled(!f.template.trim().is_empty(), egui::Button::new("Load")).clicked() {
                load_template(app, f);
                changed = true;
            }
        });
    });
    ui.add_space(6.0);
    ui.label(RichText::new("Merge options").strong());
    egui::Grid::new(("mc-merge-opts", nonce)).num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Document type");
        let doc_label = MERGE_DOCS.iter().find(|(k, _)| *k == f.doc).map_or("Form Letters", |(_, l)| *l);
        let before = f.doc.clone();
        ComboBox::from_id_salt(("mc-merge-doc", nonce)).selected_text(doc_label).width(200.0).show_ui(
            ui,
            |ui| {
                for (k, l) in MERGE_DOCS {
                    changed |= ui.selectable_value(&mut f.doc, k.to_owned(), l).changed();
                }
            },
        );
        if f.doc != before {
            switch_merge_text(f, &before);
        }
        ui.end_row();
        if f.doc != "letters" && f.to == "email" {
            // E-mail merges are messages: only form letters can be mailed.
            f.to = "document".into();
            changed = true;
        }
        ui.label("Merge to");
        ui.horizontal(|ui| {
            changed |= ui.radio_value(&mut f.to, "document".to_owned(), "New Document").changed();
            changed |= ui.radio_value(&mut f.to, "printer".to_owned(), "Printer").changed();
            ui.add_enabled_ui(f.doc == "letters", |ui| {
                changed |= ui.radio_value(&mut f.to, "email".to_owned(), "E-mail").changed();
            });
        });
        ui.end_row();
        if f.doc == "labels" && !f.label_sheets.is_empty() {
            ui.label("Labels");
            let cur = f.label_sheets.iter().find(|(k, _)| *k == f.labels).map_or("", |(_, l)| l.as_str());
            ComboBox::from_id_salt(("mc-merge-labels", nonce)).selected_text(cur).width(280.0).show_ui(
                ui,
                |ui| {
                    for (k, l) in &f.label_sheets {
                        changed |= ui.selectable_value(&mut f.labels, k.clone(), l).changed();
                    }
                },
            );
            ui.end_row();
        }
        if f.doc == "envelopes" && !f.envelope_sizes.is_empty() {
            ui.label("Envelope size");
            let cur = f.envelope_sizes.iter().find(|(k, _)| *k == f.envelope).map_or("", |(_, l)| l.as_str());
            ComboBox::from_id_salt(("mc-merge-env", nonce)).selected_text(cur).width(280.0).show_ui(
                ui,
                |ui| {
                    for (k, l) in &f.envelope_sizes {
                        changed |= ui.selectable_value(&mut f.envelope, k.clone(), l).changed();
                    }
                },
            );
            ui.end_row();
            let l = ui.label("Return address");
            ui.add(TextEdit::multiline(&mut f.return_address).desired_rows(3).desired_width(280.0))
                .labelled_by(l.id);
            ui.end_row();
        }
        match f.to.as_str() {
            "email" => {
                let l = ui.label("Message subject line");
                changed |= ui
                    .add(TextEdit::singleline(&mut f.subject).desired_width(320.0))
                    .labelled_by(l.id)
                    .changed();
                ui.end_row();
            }
            "printer" => {
                if f.printers.is_empty() {
                    f.printers = printer_names(app);
                }
                ui.label("Printer");
                ComboBox::from_id_salt(("mc-merge-printer", nonce))
                    .selected_text(f.printer.clone())
                    .width(280.0)
                    .show_ui(ui, |ui| {
                        for p in f.printers.clone() {
                            ui.selectable_value(&mut f.printer, p.clone(), p);
                        }
                    });
                ui.end_row();
            }
            _ => {
                let l = ui.label("New document (PDF)");
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut f.path).desired_width(240.0)).labelled_by(l.id);
                    if crate::dialogs::browse_button(ui, app)
                        && let Some(p) = crate::dialogs::browse_save(
                            app,
                            "New Document",
                            &[crate::FileFilter::new("PDF document", &["pdf"])],
                            &f.path,
                        )
                    {
                        f.path = p;
                    }
                });
                ui.end_row();
            }
        }
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
    let hint = match f.doc.as_str() {
        "letters" if f.to == "email" => "Message text",
        "letters" => "Letter text",
        "catalog" => "Text for each contact",
        _ => "Address (blank = the standard address block)",
    };
    changed |= ui
        .add(TextEdit::multiline(&mut f.body).desired_rows(6).desired_width(f32::INFINITY).hint_text(hint))
        .changed();
    ui.horizontal(|ui| {
        ui.checkbox(&mut f.data_file_on, "Save the contact data to a file");
        ui.add_enabled_ui(f.data_file_on, |ui| {
            let l = ui.label("File:");
            ui.add(TextEdit::singleline(&mut f.data_file).desired_width(220.0).hint_text("contacts.csv"))
                .labelled_by(l.id);
            if crate::dialogs::browse_button(ui, app)
                && let Some(p) = crate::dialogs::browse_save(
                    app,
                    "Contact Data File",
                    &[crate::FileFilter::new("CSV file", &["csv"])],
                    &f.data_file,
                )
            {
                f.data_file = p;
            }
        });
    });
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
                    let why = if f.to == "email" { "without an email address" } else { "(contact groups)" };
                    ui.label(RichText::new(format!("{skipped} {why} skipped")).weak());
                }
            });
            if let Some(m) = pv.get("preview").filter(|m| !m.is_null()) {
                if f.to == "email" {
                    ui.label(format!("To: {}", m.get("to").and_then(Value::as_str).unwrap_or("")));
                    ui.label(format!("Subject: {}", m.get("subject").and_then(Value::as_str).unwrap_or("")));
                }
                let text = m.get("body").or_else(|| m.get("text")).and_then(Value::as_str).unwrap_or("");
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, app.theme.separator))
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ScrollArea::vertical().id_salt(("mc-merge-preview", nonce)).max_height(140.0).show(
                            ui,
                            |ui| {
                                ui.label(text);
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
        let (label, ready) = match f.to.as_str() {
            "email" => ("Send", !f.subject.trim().is_empty()),
            "printer" => ("Print", true),
            _ => ("Merge", !f.path.trim().is_empty()),
        };
        let b = ok_cancel(ui, label, ready && (!f.data_file_on || !f.data_file.trim().is_empty()));
        if b != Btn::None {
            out = b;
        }
    });
    out
}

/// Printers a merge can go to: the system's default printer, then the others (not Save as PDF: that is New
/// Document).
fn printer_names(app: &mut App) -> Vec<String> {
    use mailcraft_engine::cmd::print::{DEFAULT_PRINTER, SAVE_AS_PDF};
    let mut out = vec![DEFAULT_PRINTER.to_owned()];
    let listed = app
        .s
        .run("print.printers", &json!({}))
        .ok()
        .and_then(|v| v.get("printers").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    out.extend(
        listed
            .iter()
            .filter_map(Value::as_str)
            .filter(|p| !p.eq_ignore_ascii_case(SAVE_AS_PDF) && !p.eq_ignore_ascii_case(DEFAULT_PRINTER))
            .map(str::to_owned),
    );
    out
}

/// The text a form letter starts with.
const LETTER_START: &str = "Dear {{First Name}},\n\n";
/// What a catalog starts with: one line per contact.
const CATALOG_START: &str = "{{Full Name}}, {{Company}}, {{Business Phone}}";

/// The text a document type starts with.
fn start_text(doc: &str, address_block: &str) -> String {
    match doc {
        "letters" => LETTER_START.to_owned(),
        "catalog" => CATALOG_START.to_owned(),
        _ => address_block.to_owned(),
    }
}

/// After the document type changed from `before`: text the user has not changed follows the new type
/// (labels and envelopes get the address block, letters the greeting, catalogs a line per contact); edited
/// text stays.
fn switch_merge_text(f: &mut MergeForm, before: &str) {
    if start_text(before, &f.address_block) == start_text(&f.doc, &f.address_block) {
        return;
    }
    let untouched = f.body.trim().is_empty()
        || f.body == LETTER_START
        || f.body == CATALOG_START
        || f.body.trim() == f.address_block.trim();
    if untouched {
        f.body = start_text(&f.doc, &f.address_block);
    }
}

/// Reads the existing document into the merge text (and the subject, for a saved message).
fn load_template(app: &mut App, f: &mut MergeForm) {
    let p = json!({"action": "template", "template": f.template.trim()});
    match app.s.run("people.mailMerge", &p) {
        Ok(r) => {
            f.body = r.get("body").and_then(Value::as_str).unwrap_or_default().to_owned();
            f.template_loaded = f.template.trim().to_owned();
            if let Some(subj) = r.get("subject").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                f.subject = subj.to_owned();
            }
            f.result = None;
        }
        Err(e) => f.result = Some(format!("The document could not be read: {e}")),
    }
}

/// Mail Merge document types (output id, label).
const MERGE_DOCS: [(&str, &str); 4] = [
    ("letters", "Form Letters"),
    ("labels", "Mailing Labels"),
    ("envelopes", "Envelopes"),
    ("catalog", "Catalog"),
];

fn merge_params(f: &MergeForm, extra: &[(&str, Value)]) -> Value {
    let mut p = serde_json::Map::new();
    let output = if f.to == "email" { "email" } else { f.doc.as_str() };
    p.insert("contacts".into(), json!(f.contacts));
    p.insert("output".into(), json!(output));
    p.insert("fields".into(), json!(f.fields_choice));
    if f.contacts == "all" && !f.query.is_empty() {
        p.insert("query".into(), json!(f.query));
    }
    p.insert("subject".into(), json!(f.subject));
    let template = f.template.trim();
    if f.use_template && !template.is_empty() && template != f.template_loaded {
        // An existing document typed but not loaded yet: the engine reads it.
        p.insert("template".into(), json!(template));
        p.insert("body".into(), json!(""));
    } else {
        p.insert("body".into(), json!(f.body));
    }
    if f.contacts == "selected" {
        p.insert("ids".into(), json!(f.selected));
    }
    if output != "email" {
        p.insert("to".into(), json!(f.to));
        p.insert("labels".into(), json!(f.labels));
        p.insert("envelope".into(), json!(f.envelope));
        p.insert("returnAddress".into(), json!(f.return_address));
        if f.to == "printer" {
            p.insert("printer".into(), json!(f.printer));
        } else {
            p.insert("path".into(), json!(f.path.trim()));
        }
    }
    if f.data_file_on && !f.data_file.trim().is_empty() {
        p.insert("dataFile".into(), json!(f.data_file.trim()));
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

/// Said after Move / Copy to Folder when `n` selected rows could not go into the folder chosen.
fn skipped_note(n: usize, copy: bool) -> String {
    let (what, pronoun) = if n == 1 { ("1 item".to_owned(), "it") } else { (format!("{n} items"), "they") };
    let verb = if copy { "copied" } else { "moved" };
    format!(
        "{what} could not be {verb} to that folder (a tasks folder holds only tasks) and stayed where {pronoun} {}.",
        if n == 1 { "was" } else { "were" }
    )
}

/// OK / Send: runs the dialog's command. `Ok(true)` closes the dialog.
fn apply(app: &mut App, kind: &mut Kind) -> Result<bool, String> {
    match kind {
        Kind::Move { ids, area, copy, folder } => {
            let Some(folder) = folder else { return Ok(false) };
            let id = format!("{area}.{}", if *copy { "copy" } else { "move" });
            let r = run_checked(app, &id, json!({ "folder": folder, "ids": ids }))?;
            // Rows the folder cannot hold (flagged messages and contacts of the To-Do List) stay where they
            // are: say so rather than leave them behind silently.
            let skipped = r.get("skipped").and_then(Value::as_array).map_or(0, Vec::len);
            if skipped > 0 {
                app.ui.toast = Some((skipped_note(skipped, *copy), 0.0));
            }
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
            let mut p = json!({ "ids": f.ids, "kind": if f.reminder_only { "reminder" } else { "custom" } });
            // Tasks: only what was changed is sent (a field left out stays as each row has it; a field cleared
            // is sent as null and clears it). Several rows open blank, so OK unchanged changes nothing.
            let (now, was) = (f.dates(), &f.initial);
            let tasks = f.area == "tasks";
            let reminder_changed = now.reminder_on != was.reminder_on
                || (now.reminder_on
                    && (now.reminder_date != was.reminder_date || now.reminder_min != was.reminder_min));
            if let Some(o) = p.as_object_mut() {
                if !f.reminder_only {
                    if !tasks || now.start != was.start {
                        o.insert("start".into(), start);
                    }
                    if !tasks || now.due != was.due {
                        o.insert("due".into(), due);
                    }
                    if f.area == "people" {
                        let label = if f.label.trim().is_empty() { "Follow up" } else { f.label.trim() };
                        o.insert("label".into(), json!(label));
                    }
                }
                // Unchecked: no reminder (a reminder the item had is removed).
                if (!tasks && !reminder.is_null()) || (tasks && reminder_changed) {
                    o.insert("reminder".into(), reminder);
                }
                if tasks
                    && !f.reminder_only
                    && !["start", "due", "reminder"].iter().any(|k| o.contains_key(*k))
                {
                    return Ok(true);
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
            if f.module == "notes" && (f.advanced_changed(&init) || f.then_sort != init.then_sort) {
                let rules: Vec<ItemRule> =
                    f.rules.iter().filter(|r| !r.name.trim().is_empty()).cloned().collect();
                let mut p = json!({
                    "sort": f.sort,
                    "columns": f.columns,
                    "filter": f.filter,
                    "rules": rules,
                    "autoPreview": f.auto_preview,
                    "gridLines": f.grid_lines,
                    "groupBy": fields_json(&f.group_by),
                    "expandGroups": if f.expand.is_empty() { "expanded" } else { f.expand.as_str() },
                    "rowFont": f.row_font,
                    "columnFont": f.column_font,
                });
                if f.then_sort != init.then_sort
                    && let Some(o) = p.as_object_mut()
                {
                    // Several keys: the sort column first (in the direction Reverse Sort now gives it).
                    let first = if f.sort.is_empty() { "created" } else { f.sort.as_str() };
                    let desc =
                        mailcraft_engine::cmd::view::modules::note_sort_descending(&f.sort) != f.reverse;
                    let mut keys = vec![(first.to_owned(), desc)];
                    keys.extend(f.then_sort.iter().cloned());
                    o.remove("sort");
                    o.insert("sortBy".into(), fields_json(&keys));
                }
                run_checked(app, "view.viewSettings", p)?;
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
            let action = if f.to == "email" { "send" } else { "merge" };
            let p = merge_params(f, &[("action", json!(action))]);
            let r = run_checked(app, "people.mailMerge", p)?;
            let n = |k: &str| r.get(k).and_then(Value::as_u64).unwrap_or(0);
            let text = match f.to.as_str() {
                "email" => format!("Mail Merge: {} message(s) sent to the Outbox.", n("sent")),
                "printer" => format!(
                    "Mail Merge: {} contact(s), {} page(s) sent to the printer.",
                    n("merged"),
                    n("pages")
                ),
                _ => format!(
                    "Mail Merge: {} contact(s), {} page(s) saved to {}.",
                    n("merged"),
                    n("pages"),
                    r.get("path").and_then(Value::as_str).unwrap_or_default()
                ),
            };
            app.ui.toast = Some((text, 0.0));
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
        assert!(app.ui.toast.is_none(), "nothing was left behind");
    }

    #[test]
    fn move_of_a_mixed_selection_says_which_rows_stayed() {
        let mut app = app();
        let tasks =
            app.s.mailbox.folders.values().find(|f| f.kind == FolderKind::Tasks).map(|f| f.id).unwrap();
        let sub = app.s.mailbox.create_folder(tasks, "Errands").unwrap();
        let t = app.s.mailbox.tasks.keys().next().copied().unwrap();
        let m = app.s.mailbox.mail.ids().next().unwrap();
        if let Some(mail) = app.s.mailbox.mail.get_mut(m) {
            mail.flag = mailcraft_model::FlagState::Flagged {
                label: "Follow up".into(),
                start: None,
                due: None,
                reminder: None,
            };
        }
        let folder_before = app.s.mailbox.mail.get(m).unwrap().folder;
        let mut k =
            Kind::Move { ids: vec![t.0, m.0], area: "tasks".into(), copy: false, folder: Some(sub.0) };
        assert_eq!(apply(&mut app, &mut k), Ok(true));
        assert_eq!(app.s.mailbox.tasks[&t].folder, sub);
        assert_eq!(app.s.mailbox.mail.get(m).unwrap().folder, folder_before);
        let toast = app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
        assert!(toast.starts_with("1 item could not be moved"), "{toast}");
    }

    /// Two tasks with their own dates (one with a reminder), as rows of the To-Do List.
    fn two_dated_tasks(app: &mut App) -> (u64, u64) {
        let mut new = |p: Value| app.s.run("tasks.new", &p).unwrap()["id"].as_u64().unwrap();
        let a = new(
            json!({"subject": "A", "start": "2026-10-05", "due": "2026-10-06", "reminder": "2026-10-06T09:00"}),
        );
        let b = new(json!({"subject": "B", "start": "2026-11-02", "due": "2026-11-09"}));
        (a, b)
    }

    fn custom_dialog(app: &mut App, ids: &[u64]) -> ModState {
        app.s.run("tasks.flag", &json!({"ids": ids, "kind": "custom"})).unwrap();
        let Some(mailcraft_engine::Notice::OpenDialog(name, p)) = app.s.notices.last().cloned() else {
            panic!("no dialog")
        };
        let mut st = ModState::default();
        open(app, &mut st, &name, &p);
        st
    }

    fn dates(app: &App, id: u64) -> (Option<i64>, Option<i64>, Option<i64>) {
        let t = &app.s.mailbox.tasks[&mailcraft_model::ItemId(id)];
        (t.start.map(|d| d.unix), t.due.map(|d| d.unix), t.reminder.map(|d| d.unix))
    }

    #[test]
    fn custom_follow_up_on_several_rows_keeps_each_rows_dates() {
        // Regression: the dialog opened with the first row's dates and OK sent them (and a cleared reminder)
        // to every selected row.
        let mut app = app();
        let (a, b) = two_dated_tasks(&mut app);
        let before = (dates(&app, a), dates(&app, b));
        let mut st = custom_dialog(&mut app, &[a, b]);
        let d = st.dialogs.open.first_mut().unwrap();
        let Kind::Flag(f) = &d.kind else { panic!("not the flag dialog") };
        assert!(f.start.is_empty() && f.due.is_empty() && !f.reminder_on, "several rows open blank");
        assert_eq!(apply(&mut app, &mut d.kind), Ok(true));
        assert_eq!((dates(&app, a), dates(&app, b)), before, "OK unchanged changes nothing");
        // A due date typed in goes to both; their start dates and reminders stay.
        let mut st = custom_dialog(&mut app, &[a, b]);
        let d = st.dialogs.open.first_mut().unwrap();
        if let Kind::Flag(f) = &mut d.kind {
            f.due = "2026-12-01".into();
        }
        assert_eq!(apply(&mut app, &mut d.kind), Ok(true));
        let (na, nb) = (dates(&app, a), dates(&app, b));
        assert_eq!(na.1, nb.1);
        assert_ne!(na.1, before.0.1);
        assert_eq!((na.0, na.2), (before.0.0, before.0.2));
        assert_eq!((nb.0, nb.2), (before.1.0, before.1.2));
    }

    #[test]
    fn custom_follow_up_on_one_row_starts_from_its_dates_and_can_clear_them() {
        let mut app = app();
        let (a, _) = two_dated_tasks(&mut app);
        let before = dates(&app, a);
        let mut st = custom_dialog(&mut app, &[a]);
        let d = st.dialogs.open.first_mut().unwrap();
        let Kind::Flag(f) = &mut d.kind else { panic!("not the flag dialog") };
        assert_eq!((f.start.as_str(), f.due.as_str(), f.reminder_on), ("2026-10-05", "2026-10-06", true));
        assert_eq!(apply(&mut app, &mut d.kind), Ok(true));
        assert_eq!(dates(&app, a), before);
        // None and an unchecked reminder clear them.
        let mut st = custom_dialog(&mut app, &[a]);
        let d = st.dialogs.open.first_mut().unwrap();
        if let Kind::Flag(f) = &mut d.kind {
            f.start.clear();
            f.reminder_on = false;
        }
        assert_eq!(apply(&mut app, &mut d.kind), Ok(true));
        assert_eq!(dates(&app, a), (None, before.1, None));
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
