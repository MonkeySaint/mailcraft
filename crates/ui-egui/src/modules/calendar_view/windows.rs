//! Windows the calendar draws itself: Change Time Zone (named zones, a second zone), Add Calendar › From
//! Internet, From File and Create New Blank Calendar, the calendar's View Settings, Share Calendar (calendar,
//! date range, detail), and Change View › Manage Views, Save Current View As a New View and Apply Current
//! View to Other Calendar Folders (the calendar's own views, not the mail ones); plus the internet calendar
//! refresh workers and the strip that shows a subscription's state.
//!
//! Requests arrive as entries in `app.ui.dialogs` named `calendar:*` (and the generic `timeZone` /
//! `addCalendar` requests, and new appointment / meeting windows, while the calendar is showing);
//! [`take_requests`] takes them out before the dialogs area draws. Internet calendars are fetched on worker threads ([`Fetches`]); the results go back
//! to the engine through `calendar.refreshCalendar`.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use egui::{ComboBox, Id, RichText, TextEdit, Ui};
use serde_json::{Value, json};

use mailcraft_engine::cmd::calendar::subscribe::{self, Due, Fetched};
use mailcraft_engine::cmd::calendar::{
    CalendarColumn, CalendarCurrentView, DailyTaskList, Subscription, TIME_SCALES, props, tz, views,
};
use mailcraft_model::view::{ItemFilter, ItemRule};
use mailcraft_model::{FolderId, ReadingPanePos};

use mailcraft_engine::cmd::calendar::pim::{self, Kind};
use mailcraft_model::FolderKind;

use super::super::common::{
    DAY, ModState, clock, current_folder, day_string, local_secs, parse_day, short_date, today,
};
use super::{CalUi, forms, run_view_cmd};
use crate::App;

pub(super) const TZ_DIALOG: &str = "calendar:timeZone";
pub(super) const SHARE_DIALOG: &str = "calendar:share";
pub(super) const INTERNET_DIALOG: &str = "calendar:internet";
pub(super) const BLANK_DIALOG: &str = "calendar:blank";
pub(super) const FILE_DIALOG: &str = "calendar:file";

/// Open calendar windows.
#[derive(Clone, Default)]
pub(super) struct Windows {
    tz: Option<TzForm>,
    internet: Option<InternetForm>,
    blank: Option<NameForm>,
    file: Option<FileForm>,
    settings: Option<SettingsForm>,
    /// View Settings was asked for (the form is filled from the view when drawn).
    settings_requested: bool,
    /// Manage Views › Modify asked for View Settings of this saved view.
    modify: Option<String>,
    share: Option<ShareForm>,
    manage: Option<ManageForm>,
    save_view: Option<NameForm>,
    apply_view: Option<ApplyForm>,
}

/// Share Calendar: which calendar, which days, how much detail.
#[derive(Clone, Default)]
struct ShareForm {
    folder: u64,
    /// `today`, `tomorrow`, `next7`, `next30`, `all` or `custom`.
    range: String,
    from: String,
    to: String,
    /// `availability`, `limited` or `full`.
    detail: String,
    include_private: bool,
    error: String,
}

/// Date ranges Share Calendar offers (engine id, label).
const SHARE_RANGES: [(&str, &str); 6] = [
    ("today", "Today"),
    ("tomorrow", "Tomorrow"),
    ("next7", "Next 7 days"),
    ("next30", "Next 30 days"),
    ("all", "Whole calendar"),
    ("custom", "Custom dates"),
];

/// Detail levels Share Calendar offers (engine id, label, what it shares).
const SHARE_DETAILS: [(&str, &str, &str); 3] = [
    ("availability", "Availability only", "Free and busy times only, without subjects."),
    ("limited", "Limited details", "Times and subjects."),
    ("full", "Full details", "Times, subjects, locations, notes and attendees."),
];

/// Manage Views: the selected view, the name typed for New / Copy / Rename, the kind of a new view, where
/// a new or copied view can be used and the "only this calendar's views" filter.
#[derive(Clone, Default)]
struct ManageForm {
    selected: String,
    new_name: String,
    error: String,
    base: usize,
    this_folder_scope: bool,
    only_this_folder: bool,
}

/// Apply Current View to Other Calendar Folders: the calendars ticked.
#[derive(Clone, Default)]
struct ApplyForm {
    folders: BTreeSet<u64>,
    error: String,
}

#[derive(Clone, Default)]
struct TzForm {
    label: String,
    zone: String,
    show_second: bool,
    second_label: String,
    second_zone: String,
    show_third: bool,
    third_label: String,
    third_zone: String,
    error: String,
}

#[derive(Clone, Default)]
struct InternetForm {
    url: String,
    name: String,
    error: String,
}

#[derive(Clone, Default)]
struct NameForm {
    name: String,
    error: String,
    /// Save View As: the view is for the current calendar only.
    this_folder: bool,
    /// Create New Blank Calendar: the folder the calendar goes in (`None` = the default calendar).
    parent: Option<u64>,
}

#[derive(Clone, Default)]
struct FileForm {
    path: String,
    name: String,
    error: String,
}

#[derive(Clone)]
struct SettingsForm {
    sort: CalendarColumn,
    ascending: bool,
    time_scale: u8,
    reading_pane: ReadingPanePos,
    daily: DailyTaskList,
    tighter: bool,
    /// List / Active tables: columns shown, Group By, Filter, Other Settings, Conditional Formatting and
    /// Format Columns (header labels).
    columns: Vec<CalendarColumn>,
    group_by: Option<CalendarColumn>,
    filter: ItemFilter,
    grid_lines: bool,
    rules: Vec<ItemRule>,
    labels: std::collections::BTreeMap<String, String>,
    /// Other Settings of the Day, Week and Month views: end times in Month view, Date Navigator marks.
    month_end_time: bool,
    nav_marks: bool,
    /// Manage Views › Modify: the saved view that takes the settings on OK.
    modify: Option<String>,
}

/// Colours offered by Conditional Formatting (label, "#rrggbb"; "" = the table's own).
const RULE_COLORS: [(&str, &str); 7] = [
    ("Automatic", ""),
    ("Red", "#c42b1c"),
    ("Orange", "#ca5010"),
    ("Green", "#107c10"),
    ("Blue", "#0f6cbd"),
    ("Purple", "#8764b8"),
    ("Gray", "#8a8886"),
];

/// The conditions of an [`ItemFilter`] for calendar items.
fn filter_fields(ui: &mut Ui, f: &mut ItemFilter, id: impl std::hash::Hash + std::fmt::Debug) {
    egui::Grid::new(id).num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
        let l = ui.label("Search for the word(s):");
        ui.add(TextEdit::singleline(&mut f.words).desired_width(220.0)).labelled_by(l.id);
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut f.in_body, "Also in the notes");
        ui.end_row();
        let l = ui.label("Category:");
        ui.add(TextEdit::singleline(&mut f.category).desired_width(220.0)).labelled_by(l.id);
        ui.end_row();
        ui.label("Starting within:");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut f.days).range(0..=36_500));
            ui.label(if f.days == 0 { "days (any time)" } else { "days of today" });
        });
        ui.end_row();
    });
}

/// Columns, Group By, Filter, Other Settings, Conditional Formatting and Format Columns of the List and
/// Active tables (the parts of the observed Advanced View Settings that apply to them).
fn table_settings(ui: &mut Ui, f: &mut SettingsForm) {
    egui::CollapsingHeader::new("Columns...").id_salt("mc-cal-settings-cols").show(ui, |ui| {
        for c in CalendarColumn::ALL {
            let mut on = f.columns.contains(&c);
            let fixed = c == CalendarColumn::Subject;
            if ui.add_enabled(!fixed, egui::Checkbox::new(&mut on, c.label())).changed() {
                if on {
                    f.columns.push(c);
                    f.columns.sort_by_key(|x| CalendarColumn::ALL.iter().position(|y| y == x));
                } else {
                    f.columns.retain(|x| *x != c);
                }
            }
        }
    });
    egui::CollapsingHeader::new("Group By...").id_salt("mc-cal-settings-group").show(ui, |ui| {
        let cur = f.group_by.map_or("(none)", CalendarColumn::label);
        ComboBox::from_id_salt("mc-cal-settings-groupby").selected_text(cur).show_ui(ui, |ui| {
            ui.selectable_value(&mut f.group_by, None, "(none)");
            for c in CalendarColumn::ALL {
                ui.selectable_value(&mut f.group_by, Some(c), c.label());
            }
        });
    });
    egui::CollapsingHeader::new(format!("Filter... ({})", f.filter.describe()))
        .id_salt("mc-cal-settings-filter")
        .show(ui, |ui| {
            filter_fields(ui, &mut f.filter, "mc-cal-settings-filter-grid");
            if ui.button("Clear All").clicked() {
                f.filter = ItemFilter::default();
            }
        });
    egui::CollapsingHeader::new("Other Settings...").id_salt("mc-cal-settings-other").show(ui, |ui| {
        ui.checkbox(&mut f.grid_lines, "Grid lines between rows (List, Active)");
        ui.checkbox(&mut f.month_end_time, "Show end times in Month view");
        ui.checkbox(&mut f.nav_marks, "Mark days with items in the Date Navigator");
    });
    egui::CollapsingHeader::new("Conditional Formatting...").id_salt("mc-cal-settings-rules").show(
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
                    filter_fields(ui, &mut r.filter, ("mc-cal-rule", i));
                    ui.horizontal(|ui| {
                        ui.label("Font:");
                        ui.checkbox(&mut r.bold, "Bold");
                        ui.checkbox(&mut r.italic, "Italic");
                        ui.checkbox(&mut r.strike, "Strikethrough");
                        let cur =
                            RULE_COLORS.iter().find(|(_, h)| *h == r.color).map_or("Automatic", |(l, _)| *l);
                        ComboBox::from_id_salt(("mc-cal-rule-color", i)).selected_text(cur).show_ui(
                            ui,
                            |ui| {
                                for (l, h) in RULE_COLORS {
                                    ui.selectable_value(&mut r.color, h.to_owned(), l);
                                }
                            },
                        );
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
    egui::CollapsingHeader::new("Format Columns...").id_salt("mc-cal-settings-format").show(ui, |ui| {
        egui::Grid::new("mc-cal-settings-labels").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
            for c in CalendarColumn::ALL {
                ui.label(c.label());
                let mut text = f.labels.get(c.name()).cloned().unwrap_or_default();
                if ui.add(TextEdit::singleline(&mut text).hint_text(c.label()).desired_width(180.0)).changed()
                {
                    f.labels.insert(c.name().to_owned(), text);
                }
                ui.end_row();
            }
        });
    });
}

/// True for a window request the calendar draws itself (while the Calendar module shows).
pub(crate) fn is_request(name: &str, p: &Value) -> bool {
    name.starts_with("calendar:")
        || name == "timeZone"
        || (name == "addCalendar"
            && matches!(p.get("source").and_then(Value::as_str), Some("internet" | "blank" | "file")))
        || forms::is_form(name, p)
}

/// Takes the calendar's window requests out of `app.ui.dialogs` and opens the windows.
pub(super) fn take_requests(app: &mut App, st: &mut ModState, cu: &mut CalUi) {
    let mut taken: Vec<(String, Value)> = Vec::new();
    app.ui.dialogs.retain(|(name, p)| {
        let ours = is_request(name, p);
        if ours {
            taken.push((name.clone(), p.clone()));
        }
        !ours
    });
    for (name, p) in taken {
        if forms::is_form(&name, &p) {
            forms::open(app, st, &name, &p);
        } else {
            open(app, cu, &name, &p);
        }
    }
}

fn open(app: &mut App, cu: &mut CalUi, name: &str, p: &Value) {
    let source = p.get("source").and_then(Value::as_str).unwrap_or("");
    match (name, source) {
        (TZ_DIALOG | "timeZone", _) => {
            let z = views::time_zones(&app.s);
            cu.windows.tz = Some(TzForm {
                label: z.label,
                zone: z.zone,
                show_second: z.show_second,
                second_label: z.second_label,
                second_zone: z.second_zone,
                show_third: z.show_third,
                third_label: z.third_label,
                third_zone: z.third_zone,
                error: String::new(),
            });
        }
        (INTERNET_DIALOG, _) | ("addCalendar", "internet") => {
            cu.windows.internet = Some(InternetForm::default())
        }
        (BLANK_DIALOG, _) | ("addCalendar", "blank") => cu.windows.blank = Some(NameForm::default()),
        (FILE_DIALOG, _) | ("addCalendar", "file") => {
            // The platform's file dialog when there is one; else a window with a path box.
            let filters = [crate::FileFilter::new("iCalendar", &["ics", "ical", "ifb"])];
            if app.files.is_some() {
                if let Some(path) = crate::dialogs::browse_open(app, "Open Calendar", &filters, "") {
                    add_file(app, cu, &path, "");
                }
            } else {
                cu.windows.file = Some(FileForm::default());
            }
        }
        (views::VIEW_SETTINGS_DIALOG, _) => cu.windows.settings_requested = true,
        (SHARE_DIALOG, _) => {
            let folder = p
                .get("folder")
                .and_then(Value::as_u64)
                .map(FolderId)
                .or_else(|| current_folder(app, Kind::Appointment))
                .or_else(|| pim::default_folder(&app.s, Kind::Appointment))
                .map_or(0, |f| f.0);
            let day = day_string(today(app));
            cu.windows.share = Some(ShareForm {
                folder,
                range: "next7".into(),
                from: day.clone(),
                to: day,
                detail: "availability".into(),
                ..Default::default()
            });
        }
        (views::MANAGE_VIEWS_DIALOG, _) => cu.windows.manage = Some(ManageForm::default()),
        (views::SAVE_VIEW_DIALOG, _) => cu.windows.save_view = Some(NameForm::default()),
        (views::APPLY_VIEW_DIALOG, _) => cu.windows.apply_view = Some(ApplyForm::default()),
        _ => {}
    }
}

/// Sets `key` on a JSON object.
fn put(v: &mut Value, key: &str, x: Value) {
    if let Some(o) = v.as_object_mut() {
        o.insert(key.to_owned(), x);
    }
}

/// Error text of the last failed command (its toast), taken so it shows in the window instead.
fn take_error(app: &mut App) -> String {
    app.ui.toast.take().map(|t| t.0).unwrap_or_else(|| "That did not work.".into())
}

/// Adds the calendar from an .ics file. True on success.
fn add_file(app: &mut App, cu: &mut CalUi, path: &str, name: &str) -> bool {
    let mut p = json!({ "source": "file", "path": path });
    if !name.trim().is_empty() {
        put(&mut p, "name", json!(name.trim()));
    }
    match app.run("calendar.addCalendar", p) {
        Some(out) => {
            show_new(app, cu, &out);
            true
        }
        None => false,
    }
}

/// Selects a calendar Add Calendar just made, so it shows.
fn show_new(app: &mut App, _cu: &mut CalUi, out: &Value) {
    if let Some(f) = out.get("folder").and_then(Value::as_u64) {
        let ctx_folder = FolderId(f);
        super::super::common::select_folder(app, ctx_folder);
    }
}

fn window<'a>(title: &'a str, id: &'a str) -> egui::Window<'a> {
    egui::Window::new(title)
        .id(Id::new(id))
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .pivot(egui::Align2::CENTER_CENTER)
}

fn error_line(ui: &mut Ui, app: &App, error: &str) {
    if !error.is_empty() {
        ui.label(RichText::new(error).color(app.theme.danger));
    }
}

/// OK / Cancel row: returns (ok, cancel).
fn ok_cancel(ui: &mut Ui, ok: &str) -> (bool, bool) {
    let mut out = (false, false);
    ui.horizontal(|ui| {
        out.0 = ui.button(ok).clicked();
        out.1 = ui.button("Cancel").clicked();
    });
    out
}

fn zone_combo(ui: &mut Ui, id: &str, zone: &mut String, now: i64, allow_none: bool) {
    let text = if zone.is_empty() { "(UTC offset only)".to_owned() } else { tz::label(zone, now) };
    ComboBox::from_id_salt(id).selected_text(text).width(300.0).height(320.0).show_ui(ui, |ui| {
        if allow_none {
            ui.selectable_value(zone, String::new(), "(UTC offset only)");
        }
        for z in tz::ZONES {
            ui.selectable_value(zone, (*z).to_owned(), tz::label(z, now));
        }
    });
}

/// Draws the open windows.
pub(super) fn show(ctx: &egui::Context, app: &mut App, st: &mut ModState, cu: &mut CalUi) {
    let center = ctx.content_rect().center();
    let now = app.s.now().unix;
    if let Some(mut f) = cu.windows.tz.take() {
        let mut open = true;
        let mut keep = true;
        window("Time Zone", "mc-cal-tz-window").default_pos(center).open(&mut open).show(ctx, |ui| {
            crate::dialogs::form_visuals(ui, &app.theme);
            ui.label(RichText::new("Time zone").strong());
            egui::Grid::new("mc-cal-tz-grid").num_columns(2).show(ui, |ui| {
                ui.label("Label:");
                ui.add(TextEdit::singleline(&mut f.label).desired_width(300.0));
                ui.end_row();
                ui.label("Time zone:");
                zone_combo(ui, "mc-cal-tz-zone", &mut f.zone, now, true);
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.checkbox(&mut f.show_second, "Show a second time zone");
            ui.add_enabled_ui(f.show_second, |ui| {
                egui::Grid::new("mc-cal-tz-grid2").num_columns(2).show(ui, |ui| {
                    ui.label("Label:");
                    ui.add(TextEdit::singleline(&mut f.second_label).desired_width(300.0));
                    ui.end_row();
                    ui.label("Time zone:");
                    zone_combo(ui, "mc-cal-tz-zone2", &mut f.second_zone, now, false);
                    ui.end_row();
                });
                let can_swap = tz::known(&f.zone) && tz::known(&f.second_zone);
                if ui.add_enabled(can_swap, egui::Button::new("Swap Time Zones")).clicked() {
                    std::mem::swap(&mut f.zone, &mut f.second_zone);
                    std::mem::swap(&mut f.label, &mut f.second_label);
                }
                ui.add_space(6.0);
                ui.checkbox(&mut f.show_third, "Show a third time zone");
                ui.add_enabled_ui(f.show_third, |ui| {
                    egui::Grid::new("mc-cal-tz-grid3").num_columns(2).show(ui, |ui| {
                        ui.label("Label:");
                        ui.add(TextEdit::singleline(&mut f.third_label).desired_width(300.0));
                        ui.end_row();
                        ui.label("Time zone:");
                        zone_combo(ui, "mc-cal-tz-zone3", &mut f.third_zone, now, false);
                        ui.end_row();
                    });
                });
            });
            error_line(ui, app, &f.error);
            ui.add_space(6.0);
            let (ok, cancel) = ok_cancel(ui, "OK");
            if cancel {
                keep = false;
            }
            if ok {
                let mut p = json!({
                    "label": f.label.trim(),
                    "showSecond": f.show_second,
                    "secondLabel": f.second_label.trim(),
                });
                if !f.zone.is_empty() {
                    put(&mut p, "zone", json!(f.zone));
                }
                if f.show_second || !f.second_zone.is_empty() {
                    put(&mut p, "secondZone", json!(f.second_zone));
                }
                let third = f.show_second && f.show_third;
                put(&mut p, "showThird", json!(third));
                put(&mut p, "thirdLabel", json!(f.third_label.trim()));
                if third || !f.third_zone.is_empty() {
                    put(&mut p, "thirdZone", json!(f.third_zone));
                }
                if app.run("calendar.changeTimeZone", p).is_some() {
                    keep = false;
                } else {
                    f.error = take_error(app);
                }
            }
        });
        if open && keep {
            cu.windows.tz = Some(f);
        }
    }
    if let Some(mut f) = cu.windows.internet.take() {
        let mut open = true;
        let mut keep = true;
        window("New Internet Calendar Subscription", "mc-cal-internet-window")
            .default_pos(center)
            .open(&mut open)
            .show(ctx, |ui| {
                crate::dialogs::form_visuals(ui, &app.theme);
                ui.label("Enter the location of the internet calendar you want to add:");
                ui.add(
                    TextEdit::singleline(&mut f.url)
                        .hint_text("webcal://example.com/calendar.ics")
                        .desired_width(f32::INFINITY),
                );
                ui.label("Calendar name (optional):");
                ui.add(TextEdit::singleline(&mut f.name).desired_width(f32::INFINITY));
                ui.label(
                    RichText::new("The calendar is read-only and updates itself from this address.")
                        .color(app.theme.text_muted),
                );
                error_line(ui, app, &f.error);
                let (ok, cancel) = ok_cancel(ui, "OK");
                if cancel {
                    keep = false;
                }
                if ok {
                    if props::normalize_url(&f.url).is_none() {
                        f.error = "Enter an http, https or webcal address.".into();
                    } else {
                        let mut p = json!({ "source": "internet", "url": f.url.trim(), "fetch": false });
                        if !f.name.trim().is_empty() {
                            put(&mut p, "name", json!(f.name.trim()));
                        }
                        match app.run("calendar.addCalendar", p) {
                            Some(out) => {
                                if let Some(folder) = out.get("folder").and_then(Value::as_u64)
                                    && let Some(d) = subscribe::subscription(&app.s, FolderId(folder))
                                {
                                    cu.fetches.start(ctx, d);
                                    st.cal_shown.insert(folder);
                                }
                                show_new(app, cu, &out);
                                keep = false;
                            }
                            None => f.error = take_error(app),
                        }
                    }
                }
            });
        if open && keep {
            cu.windows.internet = Some(f);
        }
    }
    if let Some(mut f) = cu.windows.blank.take() {
        let mut open = true;
        let mut keep = true;
        window("Create New Calendar", "mc-cal-blank-window").default_pos(center).open(&mut open).show(
            ctx,
            |ui| {
                crate::dialogs::form_visuals(ui, &app.theme);
                ui.label("Name:");
                let r = ui.add(TextEdit::singleline(&mut f.name).desired_width(f32::INFINITY));
                if !r.has_focus() && f.name.is_empty() {
                    r.request_focus();
                }
                ui.label("Folder contains: Calendar Items");
                ui.label("Select where to place the folder:");
                if f.parent.is_none() {
                    f.parent = pim::default_folder(&app.s, Kind::Appointment).map(|x| x.0);
                }
                crate::dialogs::folder_tree(
                    ui,
                    app,
                    &mut f.parent,
                    crate::dialogs::Pick::Parent,
                    "mc-cal-blank-parent",
                    180.0,
                );
                error_line(ui, app, &f.error);
                let (ok, cancel) = ok_cancel(ui, "OK");
                if cancel {
                    keep = false;
                }
                if ok {
                    if f.name.trim().is_empty() {
                        f.error = "Enter a name for the calendar.".into();
                    } else {
                        let mut p = json!({ "source": "blank", "name": f.name.trim() });
                        if let (Some(parent), Some(o)) = (f.parent, p.as_object_mut()) {
                            o.insert("parent".into(), json!(parent));
                        }
                        match app.run("calendar.addCalendar", p) {
                            Some(out) => {
                                if let Some(folder) = out.get("folder").and_then(Value::as_u64) {
                                    st.cal_shown.insert(folder);
                                }
                                show_new(app, cu, &out);
                                keep = false;
                            }
                            None => f.error = take_error(app),
                        }
                    }
                }
            },
        );
        if open && keep {
            cu.windows.blank = Some(f);
        }
    }
    if let Some(mut f) = cu.windows.file.take() {
        let mut open = true;
        let mut keep = true;
        window("Open Calendar From File", "mc-cal-file-window").default_pos(center).open(&mut open).show(
            ctx,
            |ui| {
                crate::dialogs::form_visuals(ui, &app.theme);
                ui.label("iCalendar file (.ics):");
                ui.add(
                    TextEdit::singleline(&mut f.path)
                        .hint_text("/path/to/calendar.ics")
                        .desired_width(f32::INFINITY),
                );
                ui.label("Calendar name (optional):");
                ui.add(TextEdit::singleline(&mut f.name).desired_width(f32::INFINITY));
                error_line(ui, app, &f.error);
                let (ok, cancel) = ok_cancel(ui, "Open");
                if cancel {
                    keep = false;
                }
                if ok {
                    let (path, name) = (f.path.trim().to_owned(), f.name.clone());
                    if path.is_empty() {
                        f.error = "Enter the file's path.".into();
                    } else if add_file(app, cu, &path, &name) {
                        keep = false;
                    } else {
                        f.error = take_error(app);
                    }
                }
            },
        );
        if open && keep {
            cu.windows.file = Some(f);
        }
    }
    if std::mem::take(&mut cu.windows.settings_requested) {
        cu.windows.settings = Some(SettingsForm {
            sort: st.cal.sort,
            ascending: st.cal.ascending,
            time_scale: st.cal.time_scale,
            reading_pane: st.cal.reading_pane,
            daily: st.cal.daily_task_list,
            tighter: st.cal.tighter_spacing,
            columns: st.cal.table_columns(),
            group_by: st.cal.group_by,
            filter: st.cal.filter.clone(),
            grid_lines: !st.cal.no_grid,
            rules: st.cal.rules.clone(),
            labels: st.cal.labels.clone(),
            month_end_time: st.cal.month_end_time,
            nav_marks: st.cal.nav_marks,
            modify: cu.windows.modify.take(),
        });
    }
    if let Some(mut f) = cu.windows.settings.take() {
        let mut open = true;
        let mut keep = true;
        window("View Settings: Calendar", "mc-cal-settings-window").default_pos(center).open(&mut open).show(
            ctx,
            |ui| {
                crate::dialogs::form_visuals(ui, &app.theme);
                egui::Grid::new("mc-cal-settings-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Sort items by (List, Active):");
                    ui.horizontal(|ui| {
                        ComboBox::from_id_salt("mc-cal-settings-sort").selected_text(f.sort.label()).show_ui(
                            ui,
                            |ui| {
                                for c in CalendarColumn::ALL {
                                    ui.selectable_value(&mut f.sort, c, c.label());
                                }
                            },
                        );
                        ui.radio_value(&mut f.ascending, true, "Ascending");
                        ui.radio_value(&mut f.ascending, false, "Descending");
                    });
                    ui.end_row();
                    ui.label("Time scale:");
                    ComboBox::from_id_salt("mc-cal-settings-scale")
                        .selected_text(format!("{} minutes", f.time_scale))
                        .show_ui(ui, |ui| {
                            for m in TIME_SCALES {
                                ui.selectable_value(&mut f.time_scale, m, format!("{m} minutes"));
                            }
                        });
                    ui.end_row();
                    ui.label("Reading pane:");
                    ComboBox::from_id_salt("mc-cal-settings-rp")
                        .selected_text(format!("{:?}", f.reading_pane))
                        .show_ui(ui, |ui| {
                            for (v, l) in [
                                (ReadingPanePos::Right, "Right"),
                                (ReadingPanePos::Bottom, "Bottom"),
                                (ReadingPanePos::Off, "Off"),
                            ] {
                                ui.selectable_value(&mut f.reading_pane, v, l);
                            }
                        });
                    ui.end_row();
                    ui.label("Daily Task List:");
                    ComboBox::from_id_salt("mc-cal-settings-tasks")
                        .selected_text(format!("{:?}", f.daily))
                        .show_ui(ui, |ui| {
                            for (v, l) in [
                                (DailyTaskList::Normal, "Normal"),
                                (DailyTaskList::Minimized, "Minimized"),
                                (DailyTaskList::Off, "Off"),
                            ] {
                                ui.selectable_value(&mut f.daily, v, l);
                            }
                        });
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut f.tighter, "Use tighter spacing");
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.label(RichText::new("List and Active views").strong());
                table_settings(ui, &mut f);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        let rp = match f.reading_pane {
                            ReadingPanePos::Right => "right",
                            ReadingPanePos::Bottom => "bottom",
                            ReadingPanePos::Off => "off",
                        };
                        let daily = match f.daily {
                            DailyTaskList::Normal => "normal",
                            DailyTaskList::Minimized => "minimized",
                            DailyTaskList::Off => "off",
                        };
                        let columns: Vec<&str> = f.columns.iter().map(|c| c.name()).collect();
                        let rules: Vec<ItemRule> =
                            f.rules.iter().filter(|r| !r.name.trim().is_empty()).cloned().collect();
                        let labels: serde_json::Map<String, Value> = CalendarColumn::ALL
                            .iter()
                            .map(|c| {
                                (
                                    c.name().to_owned(),
                                    json!(f.labels.get(c.name()).cloned().unwrap_or_default()),
                                )
                            })
                            .collect();
                        let p = json!({
                            "sort": f.sort.name(),
                            "ascending": f.ascending,
                            "timeScale": f.time_scale,
                            "readingPane": rp,
                            "dailyTaskList": daily,
                            "tighterSpacing": f.tighter,
                            "columns": columns,
                            "groupBy": f.group_by.map(CalendarColumn::name),
                            "filter": f.filter,
                            "gridLines": f.grid_lines,
                            "monthEndTime": f.month_end_time,
                            "navMarks": f.nav_marks,
                            "rules": rules,
                            "labels": labels,
                        });
                        if run_view_cmd(app, st, "view.viewSettings", p).is_some() {
                            keep = false;
                            if let Some(name) = f.modify.clone() {
                                run_view_cmd(
                                    app,
                                    st,
                                    "view.manageViews",
                                    json!({ "action": "update", "name": name }),
                                );
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                    if ui.button("Reset Current View").clicked() {
                        run_view_cmd(app, st, "view.resetView", json!({}));
                        keep = false;
                    }
                });
            },
        );
        if open && keep {
            cu.windows.settings = Some(f);
        }
    }
    share_window(ctx, app, cu);
    manage_window(ctx, app, st, cu);
    save_view_window(ctx, app, st, cu);
    apply_view_window(ctx, app, st, cu);
}

/// Calendar folders (id, name), the default calendar first.
fn calendars(app: &App) -> Vec<(u64, String)> {
    let mut v: Vec<(bool, String, u64)> = app
        .s
        .mailbox
        .folders
        .values()
        .filter(|f| f.kind == FolderKind::Calendar)
        .map(|f| (f.role.is_none(), f.name.clone(), f.id.0))
        .collect();
    v.sort();
    v.into_iter().map(|(_, n, id)| (id, n)).collect()
}

/// Share Calendar: the calendar, a date range and a detail level; OK writes the message with the calendar
/// as an iCalendar attachment (`calendar.share`).
fn share_window(ctx: &egui::Context, app: &mut App, cu: &mut CalUi) {
    let Some(mut f) = cu.windows.share.take() else { return };
    let center = ctx.content_rect().center();
    let mut open = true;
    let mut keep = true;
    window("Share Calendar", "mc-cal-share-window").default_pos(center).open(&mut open).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &app.theme);
        let cals = calendars(app);
        egui::Grid::new("mc-cal-share-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Calendar:");
            let name =
                cals.iter().find(|(id, _)| *id == f.folder).map(|(_, n)| n.clone()).unwrap_or_default();
            ComboBox::from_id_salt("mc-cal-share-cal").selected_text(name).width(240.0).show_ui(ui, |ui| {
                for (id, n) in &cals {
                    ui.selectable_value(&mut f.folder, *id, n);
                }
            });
            ui.end_row();
            ui.label("Date Range:");
            let label = SHARE_RANGES.iter().find(|(k, _)| *k == f.range).map_or("", |(_, l)| *l);
            ComboBox::from_id_salt("mc-cal-share-range").selected_text(label).width(240.0).show_ui(
                ui,
                |ui| {
                    for (k, l) in SHARE_RANGES {
                        ui.selectable_value(&mut f.range, k.to_owned(), l);
                    }
                },
            );
            ui.end_row();
            if f.range == "custom" {
                ui.label("Start:");
                ui.add(TextEdit::singleline(&mut f.from).hint_text("YYYY-MM-DD").desired_width(120.0));
                ui.end_row();
                ui.label("End:");
                ui.add(TextEdit::singleline(&mut f.to).hint_text("YYYY-MM-DD").desired_width(120.0));
                ui.end_row();
            }
            ui.label("Detail:");
            let label = SHARE_DETAILS.iter().find(|(k, _, _)| *k == f.detail).map_or("", |(_, l, _)| *l);
            ComboBox::from_id_salt("mc-cal-share-detail").selected_text(label).width(240.0).show_ui(
                ui,
                |ui| {
                    for (k, l, _) in SHARE_DETAILS {
                        ui.selectable_value(&mut f.detail, k.to_owned(), l);
                    }
                },
            );
            ui.end_row();
            ui.label("");
            let hint = SHARE_DETAILS.iter().find(|(k, _, _)| *k == f.detail).map_or("", |(_, _, h)| *h);
            ui.label(RichText::new(hint).color(app.theme.text_muted));
            ui.end_row();
            ui.label("");
            ui.add_enabled(
                f.detail != "availability",
                egui::Checkbox::new(&mut f.include_private, "Share private items in detail too"),
            );
            ui.end_row();
        });
        error_line(ui, app, &f.error);
        ui.add_space(6.0);
        let (ok, cancel) = ok_cancel(ui, "OK");
        if cancel {
            keep = false;
        }
        if ok {
            let mut p = json!({
                "folder": f.folder,
                "range": f.range,
                "detail": f.detail,
                "includePrivate": f.include_private && f.detail != "availability",
            });
            if f.range == "custom" {
                if parse_day(&f.from).is_none() || parse_day(&f.to).is_none() {
                    f.error = "Enter the start and end dates as YYYY-MM-DD.".into();
                    return;
                }
                put(&mut p, "from", json!(f.from.trim()));
                put(&mut p, "to", json!(f.to.trim()));
            }
            if app.run("calendar.share", p).is_some() {
                keep = false;
            } else {
                f.error = take_error(app);
            }
        }
    });
    if open && keep {
        cu.windows.share = Some(f);
    }
}

/// Change View › Manage Views: the standard calendar views and those saved by name; Apply View, Copy,
/// Rename, Delete (saved views) and Reset (standard views).
fn manage_window(ctx: &egui::Context, app: &mut App, st: &mut ModState, cu: &mut CalUi) {
    let Some(mut f) = cu.windows.manage.take() else { return };
    let center = ctx.content_rect().center();
    let mut open = true;
    let mut keep = true;
    let rows = views::view_rows(&app.s);
    let names: Vec<String> =
        rows.iter().filter_map(|r| r.get("name").and_then(Value::as_str)).map(str::to_owned).collect();
    if !names.iter().any(|n| n == &f.selected) {
        f.selected = st.cal.current.label().to_owned();
    }
    let standard = CalendarCurrentView::parse(&f.selected).is_some();
    window("Manage All Views", "mc-cal-manage-window").default_pos(center).open(&mut open).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &app.theme);
        egui::ScrollArea::vertical().id_salt("mc-cal-manage-list").max_height(180.0).show(ui, |ui| {
            egui::Grid::new("mc-cal-manage-grid").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                ui.label(RichText::new("View Name").strong());
                ui.label(RichText::new("Can Be Used On").strong());
                ui.end_row();
                for r in &rows {
                    let name = r.get("name").and_then(Value::as_str).unwrap_or_default();
                    let used_on = r.get("usedOn").and_then(Value::as_str).unwrap_or_default();
                    let folder_only = r.get("scope").and_then(Value::as_str) == Some("folder");
                    if f.only_this_folder && !folder_only {
                        continue;
                    }
                    if ui.selectable_label(f.selected == name, name).clicked() {
                        f.selected = name.to_owned();
                        f.error.clear();
                    }
                    ui.label(used_on);
                    ui.end_row();
                }
            });
        });
        ui.checkbox(&mut f.only_this_folder, "Only show views created for this calendar");
        let description = rows
            .iter()
            .find(|r| r.get("name").and_then(Value::as_str) == Some(f.selected.as_str()))
            .and_then(|r| r.get("description").and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned();
        ui.label(RichText::new("Description").strong());
        ui.label(RichText::new(description).color(app.theme.text_muted));
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Name (New, Copy, Rename):");
            ui.add(TextEdit::singleline(&mut f.new_name).desired_width(180.0));
        });
        ui.horizontal(|ui| {
            ui.label("Type of view (New):");
            let kinds: Vec<&str> = CalendarCurrentView::ALL.iter().map(|c| c.label()).collect();
            let cur = kinds.get(f.base).copied().unwrap_or("Calendar");
            ComboBox::from_id_salt("mc-cal-manage-base").selected_text(cur).show_ui(ui, |ui| {
                for (i, k) in kinds.iter().enumerate() {
                    ui.selectable_value(&mut f.base, i, *k);
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("Can be used on (New, Copy):");
            ui.radio_value(&mut f.this_folder_scope, false, "All Calendar folders");
            ui.radio_value(&mut f.this_folder_scope, true, "This calendar");
        });
        error_line(ui, app, &f.error);
        ui.add_space(4.0);
        let mut action: Option<&str> = None;
        ui.horizontal(|ui| {
            if ui.button("New...").clicked() {
                action = Some("new");
            }
            if ui.button("Copy...").clicked() {
                action = Some("copy");
            }
            if ui.button("Modify...").clicked() {
                action = Some("modify");
            }
            if ui.add_enabled(!standard, egui::Button::new("Rename...")).clicked() {
                action = Some("rename");
            }
            if ui.add_enabled(!standard, egui::Button::new("Delete")).clicked() {
                action = Some("delete");
            }
            if ui.add_enabled(standard, egui::Button::new("Reset")).clicked() {
                action = Some("reset");
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Apply View").clicked() {
                action = Some("apply");
            }
            if ui.button("Close").clicked() {
                keep = false;
            }
        });
        let Some(a) = action else { return };
        if matches!(a, "new" | "copy" | "rename") && f.new_name.trim().is_empty() {
            f.error = "Type a name for the view.".into();
            return;
        }
        let scope = if f.this_folder_scope { "folder" } else { "all" };
        let p = match a {
            "modify" => json!({ "action": "apply", "name": f.selected }),
            "new" => json!({
                "action": "new",
                "name": f.new_name.trim(),
                "base": CalendarCurrentView::ALL.get(f.base).map_or("Calendar", |c| c.label()),
                "scope": scope,
            }),
            _ => json!({ "action": a, "name": f.selected, "newName": f.new_name.trim(), "scope": scope }),
        };
        if run_view_cmd(app, st, "view.manageViews", p).is_some() {
            f.error.clear();
            if matches!(a, "new" | "copy" | "rename") {
                f.selected = f.new_name.trim().to_owned();
                f.new_name.clear();
            }
            if a == "modify" {
                // View Settings of the view shown; OK stores them in the saved view.
                cu.windows.modify = (!standard).then(|| f.selected.clone());
                cu.windows.settings_requested = true;
            }
            if a == "apply" {
                keep = false;
            }
        } else {
            f.error = take_error(app);
        }
    });
    if open && keep {
        cu.windows.manage = Some(f);
    }
}

/// Save Current View As a New View: a name for the current calendar view.
fn save_view_window(ctx: &egui::Context, app: &mut App, st: &mut ModState, cu: &mut CalUi) {
    let Some(mut f) = cu.windows.save_view.take() else { return };
    let center = ctx.content_rect().center();
    let mut open = true;
    let mut keep = true;
    window("Save View As", "mc-cal-save-view-window").default_pos(center).open(&mut open).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &app.theme);
        ui.label("View name:");
        let r = ui.add(TextEdit::singleline(&mut f.name).desired_width(f32::INFINITY));
        if !r.has_focus() && f.name.is_empty() {
            r.request_focus();
        }
        ui.label("Can be used on:");
        ui.radio_value(&mut f.this_folder, false, "All Calendar folders");
        ui.radio_value(&mut f.this_folder, true, "This calendar");
        error_line(ui, app, &f.error);
        let (ok, cancel) = ok_cancel(ui, "OK");
        if cancel {
            keep = false;
        }
        if ok {
            if f.name.trim().is_empty() {
                f.error = "Type a name for the view.".into();
            } else if run_view_cmd(
                app,
                st,
                "view.saveViewAs",
                json!({ "name": f.name.trim(), "scope": if f.this_folder { "folder" } else { "all" } }),
            )
            .is_some()
            {
                keep = false;
            } else {
                f.error = take_error(app);
            }
        }
    });
    if open && keep {
        cu.windows.save_view = Some(f);
    }
}

/// Apply Current View to Other Calendar Folders: the calendars that take the current view.
fn apply_view_window(ctx: &egui::Context, app: &mut App, st: &mut ModState, cu: &mut CalUi) {
    let Some(mut f) = cu.windows.apply_view.take() else { return };
    let center = ctx.content_rect().center();
    let mut open = true;
    let mut keep = true;
    let current = current_folder(app, Kind::Appointment).map(|x| x.0);
    let others: Vec<(u64, String)> =
        calendars(app).into_iter().filter(|(id, _)| Some(*id) != current).collect();
    window("Apply View", "mc-cal-apply-view-window").default_pos(center).open(&mut open).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &app.theme);
        ui.label("Show these calendars with the current view:");
        if others.is_empty() {
            ui.label(RichText::new("There are no other calendars.").color(app.theme.text_muted));
        }
        for (id, name) in &others {
            let mut on = f.folders.contains(id);
            if ui.checkbox(&mut on, name).changed() {
                if on {
                    f.folders.insert(*id);
                } else {
                    f.folders.remove(id);
                }
            }
        }
        error_line(ui, app, &f.error);
        let (ok, cancel) = ok_cancel(ui, "OK");
        if cancel {
            keep = false;
        }
        if ok {
            let ids: Vec<u64> =
                f.folders.iter().copied().filter(|id| others.iter().any(|(o, _)| o == id)).collect();
            if ids.is_empty() {
                f.error = "Tick at least one calendar.".into();
            } else if run_view_cmd(app, st, "view.applyViewTo", json!({ "folders": ids })).is_some() {
                keep = false;
            } else {
                f.error = take_error(app);
            }
        }
    });
    if open && keep {
        cu.windows.apply_view = Some(f);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Internet calendar refresh
// ---------------------------------------------------------------------------------------------------------

/// Fetches running on worker threads and their finished results.
#[derive(Clone, Default)]
pub(super) struct Fetches(Arc<Mutex<FetchBox>>);

#[derive(Default)]
struct FetchBox {
    pending: BTreeSet<u64>,
    done: Vec<(u64, Result<Fetched, String>)>,
}

impl Fetches {
    /// Starts fetching `d` unless a fetch of its calendar is running.
    pub(super) fn start(&self, ctx: &egui::Context, d: Due) {
        let folder = d.folder.0;
        match self.0.lock() {
            Ok(mut b) => {
                if !b.pending.insert(folder) {
                    return;
                }
            }
            Err(_) => return,
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let me = self.clone();
            let ctx = ctx.clone();
            let spawned = std::thread::Builder::new().name("mailcraft-ics-fetch".into()).spawn(move || {
                let r = subscribe::fetch(&d);
                me.finish(folder, r);
                ctx.request_repaint();
            });
            if let Err(e) = spawned {
                self.finish(folder, Err(format!("could not start the download: {e}")));
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = ctx;
            self.finish(folder, subscribe::fetch(&d));
        }
    }

    fn finish(&self, folder: u64, r: Result<Fetched, String>) {
        if let Ok(mut b) = self.0.lock() {
            b.done.push((folder, r));
        }
    }

    fn take(&self) -> Vec<(u64, Result<Fetched, String>)> {
        match self.0.lock() {
            Ok(mut b) => {
                let done = std::mem::take(&mut b.done);
                for (f, _) in &done {
                    b.pending.remove(f);
                }
                done
            }
            Err(_) => Vec::new(),
        }
    }

    /// True while calendar `folder` is being fetched.
    pub(super) fn busy(&self, folder: u64) -> bool {
        self.0.lock().is_ok_and(|b| b.pending.contains(&folder))
    }

    /// True while any fetch runs or a finished one waits to be applied.
    pub(super) fn active(&self) -> bool {
        self.0.lock().is_ok_and(|b| !b.pending.is_empty() || !b.done.is_empty())
    }
}

/// Hands finished fetches to the engine as synchronisation (`subscribe::apply_background`: journaled, not an
/// undo step); failures become a toast.
pub(super) fn poll_fetches(_ctx: &egui::Context, app: &mut App, cu: &mut CalUi) {
    for (folder, r) in cu.fetches.take() {
        if subscribe::subscription(&app.s, FolderId(folder)).is_none() {
            continue; // Deleted meanwhile.
        }
        let out = subscribe::apply_background(&mut app.s, FolderId(folder), r);
        if let Some(err) = out.get("error").and_then(Value::as_str) {
            let name = app.s.mailbox.folder(FolderId(folder)).map(|f| f.name.clone()).unwrap_or_default();
            app.ui.toast = Some((format!("Couldn't update \"{name}\": {err}"), 0.0));
        }
    }
}

/// Starts fetches for internet calendars whose refresh interval has passed.
pub(super) fn refresh_due(ctx: &egui::Context, app: &mut App, cu: &mut CalUi) {
    for d in subscribe::subscriptions(&app.s, false) {
        cu.fetches.start(ctx, d);
    }
}

/// "9:41 AM" today, else "10/8/2026 9:41 AM".
fn stamp(app: &App, unix: i64) -> String {
    let local = local_secs(mailcraft_model::DateTime::from_unix(unix, 0), app.s.tz_offset_min);
    let day = local.div_euclid(DAY);
    let time = clock(local.rem_euclid(DAY) / 60);
    if day == super::super::common::today(app) { time } else { format!("{} {time}", short_date(day)) }
}

/// The strip over an internet calendar: its state and a Refresh button.
pub(super) fn subscription_strip(
    ui: &mut Ui,
    app: &mut App,
    cu: &mut CalUi,
    folder: FolderId,
    sub: &Subscription,
) {
    ui.horizontal(|ui| {
        let busy = cu.fetches.busy(folder.0);
        let (text, color) = if busy {
            ("Internet calendar · updating…".to_owned(), app.theme.text_muted)
        } else if let Some(e) = &sub.last_error {
            (format!("Internet calendar · couldn't update: {e}"), app.theme.danger)
        } else if let Some(t) = sub.last_update {
            (format!("Internet calendar · updated {}", stamp(app, t)), app.theme.text_muted)
        } else {
            ("Internet calendar · not downloaded yet".to_owned(), app.theme.text_muted)
        };
        ui.label(RichText::new(text).size(12.0).color(color)).on_hover_text(sub.url.clone());
        if ui.add_enabled(!busy, egui::Button::new("Refresh").small()).clicked()
            && let Some(d) = subscribe::subscription(&app.s, folder)
        {
            cu.fetches.start(ui.ctx(), d);
        }
    });
}
