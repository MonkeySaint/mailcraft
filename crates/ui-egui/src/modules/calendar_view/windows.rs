//! Windows the calendar draws itself: Change Time Zone (named zones, a second zone), Add Calendar › From
//! Internet, From File and Create New Blank Calendar, and the calendar's View Settings; plus the internet
//! calendar refresh workers and the strip that shows a subscription's state.
//!
//! Requests arrive as entries in `app.ui.dialogs` named `calendar:*` (and the generic `timeZone` /
//! `addCalendar` requests while the calendar is showing); [`take_requests`] takes them out before the
//! dialogs area draws. Internet calendars are fetched on worker threads ([`Fetches`]); the results go back
//! to the engine through `calendar.refreshCalendar`.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use egui::{ComboBox, Id, RichText, TextEdit, Ui};
use serde_json::{Value, json};

use mailcraft_engine::cmd::calendar::subscribe::{self, Due, Fetched};
use mailcraft_engine::cmd::calendar::{
    CalendarColumn, DailyTaskList, Subscription, TIME_SCALES, props, tz, views,
};
use mailcraft_model::{FolderId, ReadingPanePos};

use super::super::common::{DAY, ModState, clock, local_secs, short_date};
use super::{CalUi, run_view_cmd};
use crate::App;

pub(super) const TZ_DIALOG: &str = "calendar:timeZone";
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
}

#[derive(Clone, Default)]
struct TzForm {
    label: String,
    zone: String,
    show_second: bool,
    second_label: String,
    second_zone: String,
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
}

/// True for a window request the calendar draws itself (while the Calendar module shows).
pub(crate) fn is_request(name: &str, p: &Value) -> bool {
    name.starts_with("calendar:")
        || name == "timeZone"
        || (name == "addCalendar"
            && matches!(p.get("source").and_then(Value::as_str), Some("internet" | "blank" | "file")))
}

/// Takes the calendar's window requests out of `app.ui.dialogs` and opens the windows.
pub(super) fn take_requests(app: &mut App, cu: &mut CalUi) {
    let mut taken: Vec<(String, Value)> = Vec::new();
    app.ui.dialogs.retain(|(name, p)| {
        let ours = is_request(name, p);
        if ours {
            taken.push((name.clone(), p.clone()));
        }
        !ours
    });
    for (name, p) in taken {
        open(app, cu, &name, &p);
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
                error_line(ui, app, &f.error);
                let (ok, cancel) = ok_cancel(ui, "OK");
                if cancel {
                    keep = false;
                }
                if ok {
                    if f.name.trim().is_empty() {
                        f.error = "Enter a name for the calendar.".into();
                    } else {
                        match app
                            .run("calendar.addCalendar", json!({ "source": "blank", "name": f.name.trim() }))
                        {
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
                        let p = json!({
                            "sort": f.sort.name(),
                            "ascending": f.ascending,
                            "timeScale": f.time_scale,
                            "readingPane": rp,
                            "dailyTaskList": daily,
                            "tighterSpacing": f.tighter,
                        });
                        if run_view_cmd(app, st, "view.viewSettings", p).is_some() {
                            keep = false;
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
