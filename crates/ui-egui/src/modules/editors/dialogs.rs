//! Module dialogs: Move / Copy to Folder, All Categories and Custom follow-up for contacts, tasks and notes;
//! the module View Settings, Apply View and Save View dialogs; Mail Merge Contacts; and the Task Request
//! window (People › Assign Task, New Items › Task Request).
//!
//! The engine asks for them with `Notice::OpenDialog` (ids in [`HANDLED`]); [`super::show`] takes them out
//! of `app.ui.dialogs` before the dialogs area draws, so these replace any generic renderer. Every button
//! acts through `app.run` with the module's commands.

use std::collections::BTreeSet;

use egui::{ComboBox, Id, RichText, ScrollArea, TextEdit, Ui, vec2};
use serde_json::{Value, json};

use mailcraft_model::FolderKind;

use super::super::common::{ModState, day_string, day_time_string, parse_day, run_checked, today};
use super::{STATUSES, date_field, error_line, recurrence_choices, time_combo};
use crate::App;

/// Dialog ids drawn here.
pub(crate) const HANDLED: [&str; 9] = [
    "pimMove",
    "pimCategories",
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
    Move { ids: Vec<u64>, area: String, copy: bool, folder: Option<u64> },
    Categories { ids: Vec<u64>, area: String, initial: Vec<String>, on: Vec<String> },
    Flag(FlagForm),
    ViewSettings(ViewForm),
    ApplyView { noun: String, folders: Vec<(u64, String)>, picked: BTreeSet<u64>, subfolders: bool },
    SaveView { name: String, noun: String },
    MailMerge(MergeForm),
    TaskRequest(RequestForm),
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
    /// Values when the dialog opened (only changes are applied).
    pub initial: Box<Option<ViewForm>>,
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
        "pimCategories" => {
            let on = strings_of(p, "on");
            Kind::Categories { ids: ids_of(p), area: str_of(p, "area"), initial: on.clone(), on }
        }
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
                initial: Box::new(None),
            };
            f.initial = Box::new(Some(f.clone()));
            Kind::ViewSettings(f)
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
        "saveModuleView" => Kind::SaveView { name: str_of(p, "name"), noun: str_of(p, "noun") },
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
        }
    }
    // Dialogs opened while drawing (none today) come after the ones kept.
    keep.append(&mut st.dialogs.open);
    st.dialogs.open = keep;
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
        Kind::Categories { .. } => "Color Categories".to_owned(),
        Kind::Flag(f) if f.reminder_only => "Add Reminder".to_owned(),
        Kind::Flag(_) => "Custom".to_owned(),
        Kind::ViewSettings(f) => format!("Advanced View Settings: {}", f.view),
        Kind::ApplyView { .. } => "Apply View".to_owned(),
        Kind::SaveView { .. } => "Save Current View As a New View".to_owned(),
        Kind::MailMerge(_) => "Mail Merge Contacts".to_owned(),
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
            Kind::Categories { on, .. } => categories_ui(ui, app, on),
            Kind::Flag(f) => flag_ui(ui, app, f, nonce),
            Kind::ViewSettings(f) => view_ui(ui, f, nonce),
            Kind::ApplyView { noun, folders, picked, subfolders, .. } => {
                apply_ui(ui, noun, folders, picked, subfolders)
            }
            Kind::SaveView { name, noun } => save_ui(ui, name, noun),
            Kind::MailMerge(f) => merge_ui(ui, app, f, nonce),
            Kind::TaskRequest(f) => request_ui(ui, app, f, nonce),
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

fn move_ui(ui: &mut Ui, app: &App, count: usize, area: &str, copy: bool, folder: &mut Option<u64>) -> Btn {
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
    ok_cancel(ui, "OK", folder.is_some())
}

fn categories_ui(ui: &mut Ui, app: &App, on: &mut Vec<String>) -> Btn {
    ui.label("To assign Color Categories to the selected items, use the check boxes next to each category.");
    for c in &app.s.mailbox.categories {
        let mut checked = on.iter().any(|x| x.eq_ignore_ascii_case(&c.name));
        if ui.checkbox(&mut checked, &c.name).changed() {
            if checked {
                on.push(c.name.clone());
            } else {
                on.retain(|x| !x.eq_ignore_ascii_case(&c.name));
            }
        }
    }
    ok_cancel(ui, "OK", true)
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

fn view_ui(ui: &mut Ui, f: &mut ViewForm, nonce: u64) -> Btn {
    let mut out = Btn::None;
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

fn save_ui(ui: &mut Ui, name: &mut String, noun: &str) -> Btn {
    let l = ui.label("Name of new view:");
    let r = ui.add(TextEdit::singleline(name).desired_width(260.0)).labelled_by(l.id);
    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !name.trim().is_empty() {
        return Btn::Ok;
    }
    ui.label(RichText::new(format!("Can be used on: this {} folder", noun.to_lowercase())).weak());
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
        Kind::Categories { ids, area, initial, on } => {
            let id = format!("{area}.categorize");
            for c in &app.s.mailbox.categories.clone() {
                let was = initial.iter().any(|x| x.eq_ignore_ascii_case(&c.name));
                let now = on.iter().any(|x| x.eq_ignore_ascii_case(&c.name));
                if was != now {
                    run_checked(app, &id, json!({ "name": c.name, "on": now, "ids": ids }))?;
                }
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
            Ok(true)
        }
        Kind::ApplyView { picked, subfolders, .. } => {
            let folders: Vec<u64> = picked.iter().copied().collect();
            run_checked(app, "view.applyViewTo", json!({ "folders": folders, "subfolders": *subfolders }))?;
            Ok(true)
        }
        Kind::SaveView { name, .. } => {
            run_checked(app, "view.saveViewAs", json!({ "name": name.trim(), "confirm": true }))?;
            Ok(true)
        }
        Kind::MailMerge(f) => {
            let p = merge_params(f, &[("action", json!("send"))]);
            let r = run_checked(app, "people.mailMerge", p)?;
            let sent = r.get("sent").and_then(Value::as_u64).unwrap_or(0);
            app.ui.toast = Some((format!("Mail Merge: {sent} message(s) sent to the Outbox."), 0.0));
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
        open(&app, &mut st, "pimCategories", &json!({"ids": "junk", "on": [1, "Red category"]}));
        assert_eq!(st.dialogs.open.len(), 2);
        assert!(matches!(&st.dialogs.open[0].kind, Kind::Move { ids, copy: true, .. } if ids == &vec![1, 2]));
        assert!(
            matches!(&st.dialogs.open[1].kind, Kind::Categories { ids, on, .. } if ids.is_empty() && on.len() == 1)
        );
    }

    #[test]
    fn move_and_categories_apply_through_commands() {
        let mut app = app();
        let tasks =
            app.s.mailbox.folders.values().find(|f| f.kind == FolderKind::Tasks).map(|f| f.id).unwrap();
        let sub = app.s.mailbox.create_folder(tasks, "Errands").unwrap();
        let id = app.s.mailbox.tasks.keys().next().copied().unwrap();
        super::super::super::common::select_item(&mut app, Some(id.0));
        let mut k = Kind::Move { ids: vec![id.0], area: "tasks".into(), copy: false, folder: Some(sub.0) };
        assert_eq!(apply(&mut app, &mut k), Ok(true));
        assert_eq!(app.s.mailbox.tasks[&id].folder, sub);
        let mut k = Kind::Categories {
            ids: vec![id.0],
            area: "tasks".into(),
            initial: vec![],
            on: vec!["Blue category".into()],
        };
        assert_eq!(apply(&mut app, &mut k), Ok(true));
        assert!(app.s.mailbox.tasks[&id].categories.iter().any(|c| c == "Blue category"));
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
        assert_eq!(apply(&mut app, &mut Kind::ViewSettings(f)), Ok(true));
        let st = mailcraft_engine::cmd::view::modules::state(
            &app.s,
            tasks,
            mailcraft_engine::cmd::view::modules::ModuleKind::Tasks,
        );
        assert_eq!((st.view.as_str(), st.arrange.as_deref()), ("Detailed", Some("importance")));
        assert!(mailcraft_engine::cmd::view::modules::reversed(&app.s, tasks));
    }
}
