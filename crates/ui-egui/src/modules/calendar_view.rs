//! Calendar module: date navigator + My Calendars (folder pane), folder bar (Today, previous/next, title,
//! view switcher) and the Day / Work Week / Week / Month / Schedule views, the List and Active tables
//! ([`table`]), the reading pane, the Daily Task List and the To-Do Bar.
//!
//! The view state is the engine's `CalendarViewState`, kept in [`ModState`] and changed through the
//! `calendar.*` view commands and the calendar's handling of the View tab (`calendar::module_view`); they
//! take the current state and return the new one. `app.ui.calendar_view` mirrors the mode so the ribbon can
//! show and switch it. The engine also saves each view on its calendar folder with a rising number; a newer
//! saved view (from a command that ran without the UI's state, or the last session) is adopted here.
//!
//! Several calendars show side by side (one pane each, with a coloured tab) or overlaid in one grid
//! (View › Overlay). Items take their first category's colour, else their calendar's colour (View › Color).
//! In day and week views appointments are dragged to move them and their top or bottom edge is dragged to
//! change their length (`calendar.moveItem` / `calendar.resize`; one occurrence of a series becomes an
//! exception). Day and week views show the second and third time zones (Change Time Zone) in rulers of
//! their own.
//!
//! Windows the calendar draws itself (Change Time Zone, Add Calendar From Internet / From File / Create New
//! Blank Calendar, View Settings, Share Calendar, Manage Views, Save View As, Apply View) and the internet
//! calendar refresh workers live in [`windows`]; new appointment and meeting windows asked for by commands
//! (prefilled, nothing saved until Save or Send) open through [`forms`].
//!
//! File › Options › Calendar applies here: the first day of the week (week, work week and month grids, the
//! Date Navigator), week numbers (month view and Date Navigator, by the "first week of year" rule), working
//! hours and days (shading, Work Week, Schedule View), the default duration and reminder of new items, the
//! bell icon on items with reminders, free items in Schedule View, the default calendar colour, the
//! time-zone label over the ruler and the automatic switch between the vertical layout and Schedule View.
//! Meeting requests are answered automatically when Auto Accept/Decline asks for it (`calendar.autoRespond`,
//! checked while the calendar shows).

pub(super) mod forms;
mod table;
#[cfg(test)]
mod ui_tests;
pub(super) mod windows;

use std::collections::{BTreeMap, BTreeSet};

use egui::{
    Align, Align2, Color32, ComboBox, CornerRadius, CursorIcon, FontId, Id, Key, Layout, Modifiers, Rect,
    RichText, ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetType, pos2, vec2,
};
use serde_json::{Value, json};

use mailcraft_engine::Module;
use mailcraft_engine::cmd::calendar::pim::Kind;
use mailcraft_engine::cmd::calendar::tz::Clock;
use mailcraft_engine::cmd::calendar::{
    self as engine_cal, CalendarCurrentView, CalendarMode, CalendarViewState, DailyTaskList, occurrences,
    props, views,
};
use mailcraft_model::{Appointment, BusyStatus, FolderId, FolderKind, ItemId, ReadingPanePos, dates};

use super::common::{
    DAY, ModState, a11y, add_months, blend, category_color, child, clock, current_folder, day_of, day_string,
    empty_message, folder_rows, group_header, hour_label, load, long_date, module_folder_pane, month_name,
    now_minute, paint_text_clipped, pane_bg, parse_day, select_folder, shift_month, store, today, tz,
    weekday, weekday_name, ymd,
};
use super::editors;
use crate::App;

/// Folder bar height (Today, arrows, title, view switcher).
const BAR_H: f32 = 48.0;
/// Height of one time slot in day/week views (Use Tighter Spacing: the smaller one).
const SLOT_H: f32 = 20.0;
const SLOT_H_TIGHT: f32 = 15.0;
/// Width of one time ruler.
const RULER_W: f32 = 56.0;
/// Height of one all-day banner lane.
const LANE_H: f32 = 22.0;
/// Height of a calendar's tab above its pane (several calendars shown).
const TAB_H: f32 = 24.0;
/// Daily Task List: header and row heights.
const TASK_HEAD_H: f32 = 22.0;
const TASK_ROW_H: f32 = 20.0;
/// Most task rows per day in the Daily Task List.
const TASK_ROWS: usize = 3;
/// Height of the internet calendar info strip.
const INFO_H: f32 = 28.0;
/// Most calendars shown side by side (more are overlaid).
const MAX_SIDE_BY_SIDE: usize = 6;
/// Grab zone (px) at a block's top and bottom edges for resizing.
const EDGE: f32 = 5.0;
/// Seconds between checks for internet calendars due a refresh (a new subscription is fetched soon after).
const POLL_SECS: f64 = 5.0;
/// Seconds between automatic accept / decline passes over new meeting requests.
const AUTO_RESPOND_SECS: f64 = 30.0;
/// Width of the week-number column (month view, Date Navigator).
const WEEK_NO_W: f32 = 24.0;

const VIEW_COMMANDS: [&str; 8] = [
    "calendar.today",
    "calendar.next7Days",
    "calendar.goToDate",
    "calendar.arrange",
    "calendar.timeScale",
    "calendar.workingHours",
    "calendar.overlay",
    "calendar.dailyTaskList",
];

// ---------------------------------------------------------------------------------------------------------
// Calendar UI state (egui memory, separate from the shared ModState)
// ---------------------------------------------------------------------------------------------------------

/// What the calendar keeps between frames besides `ModState`. The saved views seen, the drag, the scroll
/// keys and the calendar's own windows belong to the window (egui viewport) drawing the calendar
/// ([`WindowUi`]); the internet calendar fetches and the timers are shared by all windows.
#[derive(Clone, Default)]
struct CalUi {
    /// Saved-view number last adopted, per calendar folder.
    seen: BTreeMap<u64, u64>,
    /// Appointment being dragged in a day/week view.
    drag: Option<Drag>,
    /// Scroll position key per pane (the grid scrolls to the working day when the view changes).
    scroll_keys: BTreeMap<usize, String>,
    windows: windows::Windows,
    fetches: windows::Fetches,
    /// `ctx.input().time` of the last check for internet calendars due a refresh.
    last_poll: Option<f64>,
    /// `ctx.input().time` of the last automatic accept / decline pass, and the messages it looked at.
    last_auto: Option<f64>,
    auto_seen: BTreeSet<u64>,
}

/// The per-window part of [`CalUi`] (windows other than the main one).
#[derive(Clone, Default)]
struct WindowUi {
    seen: BTreeMap<u64, u64>,
    drag: Option<Drag>,
    scroll_keys: BTreeMap<usize, String>,
    windows: windows::Windows,
}

impl WindowUi {
    fn of(cu: &CalUi) -> WindowUi {
        WindowUi {
            seen: cu.seen.clone(),
            drag: cu.drag.clone(),
            scroll_keys: cu.scroll_keys.clone(),
            windows: cu.windows.clone(),
        }
    }

    fn apply(self, cu: &mut CalUi) {
        cu.seen = self.seen;
        cu.drag = self.drag;
        cu.scroll_keys = self.scroll_keys;
        cu.windows = self.windows;
    }
}

fn ui_id() -> Id {
    Id::new("mailcraft.modules.calendar.ui")
}

fn window_ui_id(viewport: egui::ViewportId) -> Id {
    Id::new(("mailcraft.modules.calendar.ui.window", viewport))
}

fn load_ui(ctx: &egui::Context) -> CalUi {
    let mut cu = ctx.data_mut(|d| d.get_temp::<CalUi>(ui_id())).unwrap_or_default();
    let vp = ctx.viewport_id();
    if vp != egui::ViewportId::ROOT {
        // A new window starts with no windows open and nothing being dragged.
        let own = ctx
            .data_mut(|d| d.get_temp::<WindowUi>(window_ui_id(vp)))
            .unwrap_or_else(|| WindowUi { seen: cu.seen.clone(), ..Default::default() });
        own.apply(&mut cu);
    }
    cu
}

fn store_ui(ctx: &egui::Context, mut cu: CalUi) {
    let vp = ctx.viewport_id();
    if vp != egui::ViewportId::ROOT {
        let main = ctx.data_mut(|d| d.get_temp::<CalUi>(ui_id())).unwrap_or_default();
        let own = WindowUi::of(&cu);
        WindowUi::of(&main).apply(&mut cu);
        ctx.data_mut(|d| d.insert_temp(window_ui_id(vp), own));
    }
    ctx.data_mut(|d| d.insert_temp(ui_id(), cu));
}

/// Drops what the calendar kept for a closed window.
pub(super) fn forget_window(ctx: &egui::Context, viewport: egui::ViewportId) {
    ctx.data_mut(|d| d.remove::<WindowUi>(window_ui_id(viewport)));
}

/// A drag in progress: which block, what part, and where it started.
#[derive(Clone, Debug, PartialEq)]
struct Drag {
    id: ItemId,
    /// Unix start of the occurrence (series only).
    occurrence: Option<i64>,
    kind: DragKind,
    /// The block's local start / end (seconds).
    start: i64,
    end: i64,
    /// Minutes between the block's start and the grab point (moves).
    grab_min: i64,
    pane: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragKind {
    Move,
    Top,
    Bottom,
}

/// Settings the views read each frame.
#[derive(Clone, Debug, Default)]
struct Look {
    /// Working days, 0 = Sunday.
    work_days: Vec<u8>,
    /// Working hours (minutes after midnight).
    work: (i64, i64),
    slot_h: f32,
    /// Second time zone: (label, offset at the first visible day).
    second: Option<(String, i32)>,
    /// Third time zone, shown left of the second one.
    third: Option<(String, i32)>,
    /// The calendar's own offset then (the second ruler shows the difference).
    primary_off: i32,
    primary_label: String,
    preview: bool,
    /// First day of the week, 0 = Sunday.
    first_day: u8,
    /// Week numbers in the month view and Date Navigator, and the "first week of year" rule.
    week_numbers: bool,
    week_rule: String,
    /// Schedule View shows free items.
    schedule_free: bool,
}

impl Look {
    fn of(app: &mut App, st: &ModState) -> Look {
        // "Today" and the time line follow a daylight-saving change while the window stays open.
        views::refresh_time_zone(&mut app.s);
        let sel = selected_day(app, st);
        let clock = cal_clock(app);
        let noon = clock.from_local(sel.saturating_mul(DAY).saturating_add(12 * 3600)).unix;
        let primary_off = clock.offset_at(noon);
        let mut work_days = engine_cal::work_week(&mut app.s);
        work_days.retain(|d| *d < 7);
        if work_days.is_empty() {
            work_days = vec![1, 2, 3, 4, 5];
        }
        Look {
            work_days,
            work: engine_cal::work_hours(&mut app.s),
            slot_h: if st.cal.tighter_spacing { SLOT_H_TIGHT } else { SLOT_H },
            second: engine_cal::second_zone(&mut app.s, noon),
            third: engine_cal::third_zone(&mut app.s, noon),
            primary_off,
            primary_label: views::primary_zone_label(&app.s),
            preview: st.cal.current == CalendarCurrentView::Preview,
            first_day: views::first_day_of_week(&app.s),
            week_numbers: app.s.settings.bool("calendar.weekNumbers"),
            week_rule: views::first_week_rule(&app.s),
            schedule_free: app.s.settings.bool("calendar.scheduleShowFree"),
        }
    }
}

impl Look {
    /// The time zones shown left of the calendar's own ruler, leftmost first: the third, then the second.
    fn extra_zones(&self) -> Vec<(String, i32)> {
        self.third.iter().chain(self.second.iter()).take(2).cloned().collect()
    }
}

/// One view pane: the calendars it shows and a key that keeps its widget ids apart.
#[derive(Clone, Debug)]
struct Pane {
    cals: BTreeSet<FolderId>,
    key: usize,
}

// ---------------------------------------------------------------------------------------------------------
// View state
// ---------------------------------------------------------------------------------------------------------

/// Follows `app.ui.calendar_view` (the ribbon may switch the mode) and fills in today's date.
pub(super) fn sync_mode(app: &mut App, st: &mut ModState) {
    match CalendarMode::parse(app.ui.calendar_view.trim()) {
        Some(m) if m != st.cal.mode => {
            st.cal.mode = m;
            st.cal.days = 1;
        }
        Some(_) => {}
        None => app.ui.calendar_view = st.cal.mode.name().to_owned(),
    }
    if parse_day(&st.cal.date).is_none() {
        st.cal.date = day_string(today(app));
    }
}

/// Adopts the current calendar's saved view when it is newer than the last one seen (keeps the date).
fn adopt_saved(app: &mut App, st: &mut ModState, cu: &mut CalUi) {
    let Some(folder) = current_folder(app, Kind::Appointment) else { return };
    let Some((saved, seq)) = engine_cal::saved_view(&app.s) else { return };
    let seen = cu.seen.get(&folder.0).copied().unwrap_or(0);
    if seq <= seen {
        return;
    }
    cu.seen.insert(folder.0, seq);
    // Saved by a view command of this or another window: each window keeps its own view.
    if st.cal_ui_seq.get(&folder.0) == Some(&seq) {
        return;
    }
    let (date, days) = (st.cal.date.clone(), st.cal.days);
    let same_mode = saved.mode == st.cal.mode;
    st.cal = saved;
    st.cal.date = date;
    st.cal.days = if same_mode { days } else { 1 };
    app.ui.calendar_view = st.cal.mode.name().to_owned();
}

/// The calendar's own window for a command run without the details it needs (`None` = run it).
fn own_window(id: &str, p: &Value) -> Option<&'static str> {
    let has = |k: &str| p.get(k).is_some_and(|v| !v.is_null() && v.as_str() != Some(""));
    match id {
        "calendar.changeTimeZone"
            if ![
                "offsetMin",
                "zone",
                "label",
                "secondZone",
                "secondLabel",
                "showSecond",
                "thirdZone",
                "thirdLabel",
                "showThird",
                "swap",
            ]
            .iter()
            .any(|k| has(k)) =>
        {
            Some(windows::TZ_DIALOG)
        }
        "calendar.share" if !["range", "detail"].iter().any(|k| has(k)) => Some(windows::SHARE_DIALOG),
        "calendar.addCalendar" => match p.get("source").and_then(Value::as_str) {
            Some("internet") if !has("url") => Some(windows::INTERNET_DIALOG),
            Some("blank") if !has("name") => Some(windows::BLANK_DIALOG),
            Some("file") if !has("path") && !has("data") => Some(windows::FILE_DIALOG),
            _ => None,
        },
        _ => None,
    }
}

/// Runs `id` (adding the current view state for view commands) and keeps a returned view state. The View
/// tab's commands run the calendar's own handling directly; commands that need a calendar window open it.
pub(super) fn run_view_cmd(app: &mut App, st: &mut ModState, id: &str, params: Value) -> Option<Value> {
    let mut params = if params.is_object() { params } else { json!({}) };
    if let Some(dialog) = own_window(id, &params) {
        crate::dialogs::open(app, dialog, params);
        return Some(json!({ "dialog": dialog }));
    }
    let module_view = views::MODULE_VIEW_IDS.contains(&id);
    if (VIEW_COMMANDS.contains(&id) || module_view)
        && let Some(o) = params.as_object_mut()
    {
        o.insert("view".into(), serde_json::to_value(&st.cal).unwrap_or(Value::Null));
    }
    let out = if module_view {
        app.s.view.module = Module::Calendar;
        match engine_cal::module_view(&mut app.s, id, &params) {
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                app.ui.toast = Some((e.to_string(), 0.0));
                return None;
            }
            None => app.run(id, params)?,
        }
    } else {
        app.run(id, params)?
    };
    // A new appointment or meeting window asked for (prefilled; nothing is saved before Save or Send).
    forms::take_notices(app, st);
    if let Some(v) = out.get("view").and_then(|v| serde_json::from_value::<CalendarViewState>(v.clone()).ok())
    {
        st.cal = v;
        app.ui.calendar_view = st.cal.mode.name().to_owned();
        // The view this window just saved is not an outside change for the other windows.
        if let (Some(folder), Some((_, seq))) =
            (current_folder(app, Kind::Appointment), engine_cal::saved_view(&app.s))
        {
            if st.cal_ui_seq.len() > 1_000 {
                st.cal_ui_seq.clear();
            }
            st.cal_ui_seq.insert(folder.0, seq);
        }
    }
    Some(out)
}

fn go_to(app: &mut App, st: &mut ModState, day: i64) {
    run_view_cmd(app, st, "calendar.goToDate", json!({ "date": day_string(day) }));
}

fn selected_day(app: &App, st: &ModState) -> i64 {
    parse_day(&st.cal.date).unwrap_or_else(|| today(app))
}

/// Calendar folders whose items are shown.
fn shown_calendars(app: &App, st: &ModState) -> BTreeSet<FolderId> {
    let mut set: BTreeSet<FolderId> = st
        .cal_shown
        .iter()
        .map(|x| FolderId(*x))
        .filter(|f| app.s.mailbox.folder(*f).is_some_and(|f| f.kind == FolderKind::Calendar))
        .collect();
    if set.is_empty()
        && let Some(c) = current_folder(app, Kind::Appointment)
    {
        set.insert(c);
    }
    set
}

/// Shown calendars in folder order (the default calendar first, then by name).
fn ordered(app: &App, cals: &BTreeSet<FolderId>) -> Vec<FolderId> {
    let mut v: Vec<(bool, String, FolderId)> = cals
        .iter()
        .filter_map(|f| app.s.mailbox.folder(*f).map(|x| (x.role.is_none(), x.name.to_lowercase(), *f)))
        .collect();
    v.sort();
    v.into_iter().map(|(_, _, f)| f).collect()
}

/// "#rrggbb" → colour.
fn hex_color(text: &str) -> Option<Color32> {
    let h = text.trim().strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?))
}

/// The colour calendar `folder` is drawn in: its View › Color, else File › Options › Calendar › Default
/// calendar color (on every calendar with "Use this color on all calendars"), else its automatic colour.
pub(super) fn calendar_color32(app: &App, folder: FolderId) -> Color32 {
    let default = hex_color(&app.s.settings.text("calendar.defaultColor"));
    if let Some(c) = default.filter(|_| app.s.settings.bool("calendar.useColorOnAll")) {
        return c;
    }
    if let Some(c) = default
        && props::read(&app.s.mailbox, folder).color.is_none()
    {
        return c;
    }
    let (_, [r, g, b]) = engine_cal::calendar_color(&app.s.mailbox, folder);
    Color32::from_rgb(r, g, b)
}

/// The first day of the week containing `day`, weeks starting on `first` (0 = Sunday).
fn week_start(day: i64, first: u8) -> i64 {
    views::week_start_on(day, first)
}

/// Days shown by the current view (Month: the whole 5–6 week grid). `work` = working days, 0 = Sunday;
/// weeks start on `first`.
fn visible_days(st: &ModState, sel: i64, work: &[u8], first: u8) -> Vec<i64> {
    let ws = week_start(sel, first);
    match st.cal.mode {
        CalendarMode::Day => (0..i64::from(st.cal.days.clamp(1, 14))).map(|i| sel + i).collect(),
        CalendarMode::WorkWeek => {
            // The working days in week order (from the first day of the week).
            let days: Vec<i64> = (0..7).map(|i| ws + i).filter(|d| work.contains(&weekday(*d))).collect();
            if days.is_empty() {
                (0..7).map(|i| ws + i).filter(|d| (1..=5).contains(&weekday(*d))).collect()
            } else {
                days
            }
        }
        CalendarMode::Week => (0..7).map(|i| ws + i).collect(),
        CalendarMode::Month => {
            let (start, weeks) = month_grid(sel, first);
            (0..weeks * 7).map(|i| start + i).collect()
        }
        CalendarMode::Schedule => vec![sel],
    }
}

/// First cell and number of weeks of the month grid containing `day` (weeks starting on `first`).
fn month_grid(day: i64, first_day: u8) -> (i64, i64) {
    let (y, m, _) = ymd(day);
    let first = day_of(y, m, 1).unwrap_or(day);
    let start = week_start(first, first_day);
    let dim = i64::from(dates::days_in_month(y, m));
    let weeks = ((first - start) + dim + 6) / 7;
    (start, weeks.max(4))
}

/// Folder-bar title for the view.
fn title(st: &ModState, sel: i64, work: &[u8], first: u8) -> String {
    let span = |a: i64, b: i64| {
        let (ya, ma, da) = ymd(a);
        let (yb, mb, db) = ymd(b);
        if ya != yb {
            format!("{} {da}, {ya} – {} {db}, {yb}", month_name(ma), month_name(mb))
        } else if ma != mb {
            format!("{} {da} – {} {db}, {yb}", month_name(ma), month_name(mb))
        } else {
            format!("{} {da} – {db}, {yb}", month_name(ma))
        }
    };
    match st.cal.mode {
        CalendarMode::Month => {
            let (y, m, _) = ymd(sel);
            format!("{} {y}", month_name(m))
        }
        CalendarMode::Day if st.cal.days <= 1 => long_date(sel),
        CalendarMode::Schedule => long_date(sel),
        _ => {
            let days = visible_days(st, sel, work, first);
            match (days.first(), days.last()) {
                (Some(a), Some(b)) => span(*a, *b),
                _ => long_date(sel),
            }
        }
    }
}

/// `label` with the alternate calendar's date after it ("Friday, October 9, 2026, 26 Tishri 5787").
fn with_alternate(label: String, alt: Option<&mailcraft_model::altcal::AltDate>) -> String {
    match alt {
        Some(a) => format!("{label}, {}", a.long()),
        None => label,
    }
}

/// The date the previous/next arrows go to.
fn step(st: &ModState, sel: i64, dir: i64) -> i64 {
    match st.cal.mode {
        CalendarMode::Day => sel + dir * i64::from(st.cal.days.clamp(1, 14)),
        CalendarMode::WorkWeek | CalendarMode::Week => sel + 7 * dir,
        CalendarMode::Month => shift_month(sel, dir),
        CalendarMode::Schedule => sel + dir,
    }
}

// ---------------------------------------------------------------------------------------------------------
// Occurrences (appointments expanded over a range, recurrences included)
// ---------------------------------------------------------------------------------------------------------

/// One appointment occurrence in local seconds.
#[derive(Clone, Debug)]
pub(super) struct Occ {
    pub id: ItemId,
    pub folder: FolderId,
    /// Local seconds on the calendar's clock.
    pub start: i64,
    pub end: i64,
    /// The occurrence's start instant (unix seconds): names it to the engine.
    pub start_unix: i64,
    pub all_day: bool,
    pub subject: String,
    pub location: String,
    /// The first lines of the body (Preview view).
    pub preview: String,
    pub base: Color32,
    pub show_as: BusyStatus,
    pub private: bool,
    pub recurring: bool,
    pub meeting: bool,
    /// Has a reminder (the bell icon, when File › Options › Calendar shows it).
    pub reminder: bool,
    /// In a read-only (internet) calendar.
    pub read_only: bool,
    /// View Settings › Conditional Formatting: the text's font effects (the colour is in `base`).
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
}

impl Occ {
    fn first_day(&self) -> i64 {
        self.start.div_euclid(DAY)
    }
    fn last_day(&self) -> i64 {
        (self.end - 1).max(self.start).div_euclid(DAY)
    }
    /// Shown in the all-day strip (all-day or 24 hours and longer).
    fn banner(&self) -> bool {
        self.all_day || self.end - self.start >= DAY
    }
    fn covers(&self, day: i64) -> bool {
        self.first_day() <= day && day <= self.last_day()
    }
    fn label(&self) -> String {
        if self.subject.trim().is_empty() { "(No subject)".to_owned() } else { self.subject.clone() }
    }
}

/// Occurrence spans (local seconds) of `a` overlapping days `[from, to)` at fixed offset `tz`.
#[cfg(test)]
pub(super) fn expand(a: &Appointment, tz: i32, from: i64, to: i64) -> Vec<(i64, i64)> {
    expand_on(a, &Clock::fixed(tz), from, to).into_iter().map(|(s, e, _)| (s, e)).collect()
}

/// Occurrences of `a` overlapping local days `[from, to)` on `clock` (the calendar's: its named zone across
/// daylight-saving changes): (local start, local end, start instant), expanded by the engine
/// (`calendar::occurrences`: RFC 5545 expansion with EXDATEs, in the appointment's time zone when it has one).
pub(super) fn expand_on(a: &Appointment, clock: &Clock, from: i64, to: i64) -> Vec<(i64, i64, i64)> {
    let at = |day: i64| clock.from_local(day.saturating_mul(DAY));
    if to <= from {
        return Vec::new();
    }
    occurrences(a, at(from), at(to), 1000)
        .into_iter()
        .map(|(s, e)| {
            let ls = clock.local(s.unix);
            (ls, clock.local(e.unix).max(ls), s.unix)
        })
        .collect()
}

/// The calendar's clock (see `engine_cal::tz::Clock`).
pub(super) fn cal_clock(app: &App) -> Clock {
    Clock::of(&app.s)
}

/// First lines of a body for the Preview view.
fn preview_text(body: &str) -> String {
    let mut out = String::new();
    for word in body.split_whitespace() {
        if out.len() > 160 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Occurrences of appointments in `calendars` over days `[from, to)`, sorted by start. Items take their
/// first category's colour, else their calendar's.
pub(super) fn gather(app: &App, calendars: &BTreeSet<FolderId>, from: i64, to: i64) -> Vec<Occ> {
    gather_in(app, calendars, from, to, None)
}

/// [`gather`] as view `view` shows the items: its View Settings filter leaves out the items it does not
/// match, and its Conditional Formatting rules colour the items they match (a later rule's colour wins)
/// and set their text's font effects.
pub(super) fn gather_in(
    app: &App,
    calendars: &BTreeSet<FolderId>,
    from: i64,
    to: i64,
    view: Option<&CalendarViewState>,
) -> Vec<Occ> {
    use mailcraft_model::view::{FilterText, ItemStyle, rule_style, rules_active, rules_need_body};
    let clock = cal_clock(app);
    let now = app.s.now().unix;
    let filter = view.map(|v| &v.filter).filter(|f| !f.is_empty());
    let rules: &[mailcraft_model::view::ItemRule] = view.map_or(&[], |v| v.rules.as_slice());
    // Each item's text is lowercased once, and only when the filter or a rule needs it.
    let want_text = filter.is_some() || rules_active(rules);
    let want_body = filter.is_some_and(|f| f.needs_body()) || rules_need_body(rules);
    let looks: BTreeMap<FolderId, (Color32, bool)> = calendars
        .iter()
        .map(|f| (*f, (calendar_color32(app, *f), props::is_read_only(&app.s.mailbox, *f))))
        .collect();
    let mut out = Vec::new();
    for a in app.s.mailbox.appointments.values() {
        let Some(&(cal_color, read_only)) = looks.get(&a.folder) else { continue };
        let text = want_text.then(|| FilterText::new(&a.subject, &a.body, want_body));
        if let (Some(f), Some(t)) = (filter, &text)
            && !f.matches_text(t, &a.categories, a.start.unix, now)
        {
            continue;
        }
        let style = match &text {
            Some(t) => rule_style(rules, |f| f.matches_text(t, &a.categories, a.start.unix, now)),
            None => ItemStyle::default(),
        };
        let base = style
            .color
            .as_deref()
            .and_then(hex_color)
            .or_else(|| category_color(app, &a.categories))
            .unwrap_or(cal_color);
        for (start, end, start_unix) in expand_on(a, &clock, from, to) {
            out.push(Occ {
                id: a.id,
                folder: a.folder,
                start,
                end,
                start_unix,
                all_day: a.all_day,
                subject: a.subject.clone(),
                location: a.location.clone(),
                preview: if a.private { String::new() } else { preview_text(&a.body) },
                base,
                show_as: a.show_as,
                private: a.private,
                recurring: a.recurrence.is_some(),
                meeting: a.is_meeting,
                reminder: a.reminder_min.is_some(),
                read_only,
                bold: style.bold,
                italic: style.italic,
                strike: style.strike,
            });
        }
    }
    out.sort_by_key(|o| (o.start, std::cmp::Reverse(o.end), o.id));
    out
}

/// An occurrence's text, truncated to `width`, with its Conditional Formatting font effects (bold is drawn
/// twice, a little apart).
fn paint_occ_text(
    ui: &Ui,
    pos: egui::Pos2,
    text: &str,
    o: &Occ,
    font: FontId,
    color: Color32,
    width: f32,
) -> Rect {
    if !(o.bold || o.italic || o.strike) {
        return paint_text_clipped(ui, pos, text, font, color, width);
    }
    let format = egui::TextFormat {
        font_id: font,
        color,
        italics: o.italic,
        strikethrough: if o.strike { Stroke::new(1.0, color) } else { Stroke::NONE },
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::single_section(text.to_owned(), format);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(4.0));
    let galley = ui.painter().layout_job(job);
    let rect = Rect::from_min_size(pos, galley.size());
    if o.bold {
        ui.painter().galley(pos2(pos.x + 0.6, pos.y), galley.clone(), color);
    }
    ui.painter().galley(pos, galley, color);
    rect
}

/// The Month view's dot before an appointment: the calendar's colour, lifted until it keeps 3:1 (the WCAG
/// figure for graphics) against `under`, the fill it sits on.
fn month_dot(bar: Color32, under: Color32) -> Color32 {
    crate::theme::legible(bar, under, 3.0)
}

/// Fill and bar colours of an occurrence block.
fn occ_colors(app: &App, o: &Occ) -> (Color32, Color32) {
    let bg = app.theme.reading;
    let share = if app.theme.dark { 0.42 } else { 0.26 };
    let fill = match o.show_as {
        BusyStatus::Free => blend(bg, o.base, share * 0.4),
        BusyStatus::Tentative => blend(bg, o.base, share * 0.7),
        _ => blend(bg, o.base, share),
    };
    let bar = match o.show_as {
        BusyStatus::OutOfOffice => Color32::from_rgb(0x7a, 0x3d, 0x9a),
        BusyStatus::Free => blend(bg, o.base, 0.35),
        BusyStatus::Tentative => blend(bg, o.base, 0.6),
        _ => o.base,
    };
    (fill, bar)
}

fn occ_a11y(o: &Occ) -> String {
    let (d, m) = (o.start.div_euclid(DAY), o.start.rem_euclid(DAY) / 60);
    if o.banner() {
        format!("{}, {}, all day", o.label(), long_date(d))
    } else {
        format!("{}, {} {}", o.label(), long_date(d), clock(m))
    }
}

/// Click / double-click handling shared by every occurrence block.
fn occ_interact(app: &mut App, st: &mut ModState, resp: &egui::Response, o: &Occ) {
    if resp.clicked() {
        app.ui.module_selection = Some(o.id.0);
        st.cal_slot = None;
    }
    let id = o.id;
    let occ_unix = o.start_unix;
    if resp.double_clicked() {
        editors::open_occurrence(app, st, id, occ_unix);
    }
    let (recurring, read_only) = (o.recurring, o.read_only);
    resp.context_menu(|ui| {
        if ui.button("Open").clicked() {
            editors::open_occurrence(app, st, id, occ_unix);
            ui.close();
        }
        if read_only {
            ui.label(RichText::new("Internet calendar (read-only)").color(app.theme.text_muted));
            return;
        }
        if recurring {
            if ui.button("Open Series").clicked() {
                editors::open_appointment(app, st, id);
                ui.close();
            }
            if ui.button("Delete Occurrence").clicked() {
                app.run("calendar.deleteOccurrence", json!({ "id": id.0, "occurrence": occ_unix }));
                ui.close();
            }
        }
        ui.menu_button("Categorize", |ui| {
            let cats: Vec<String> = app.s.mailbox.categories.iter().map(|c| c.name.clone()).collect();
            for c in cats {
                if ui.button(&c).clicked() {
                    app.run("calendar.categorize", json!({ "id": id.0, "name": c }));
                    ui.close();
                }
            }
            if ui.button("Clear All Categories").clicked() {
                app.run("calendar.categorize", json!({ "id": id.0, "clear": true }));
                ui.close();
            }
        });
        if ui.button(if recurring { "Delete Series" } else { "Delete" }).clicked() {
            app.run("calendar.delete", json!({ "id": id.0 }));
            if app.ui.module_selection == Some(id.0) {
                app.ui.module_selection = None;
            }
            ui.close();
        }
    });
}

/// Narrowest text area of an occurrence block that gets its subject; narrower blocks are plain markers.
const BLOCK_TEXT_MIN_W: f32 = 24.0;

/// Paints an occurrence block (time grid / schedule). `preview` adds the body's first lines.
fn paint_block(ui: &Ui, app: &mut App, rect: Rect, o: &Occ, selected: bool, preview: bool) {
    let (fill, bar) = occ_colors(app, o);
    let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    p.rect_filled(rect, CornerRadius::same(3), fill);
    let bar_rect = Rect::from_min_size(rect.min, vec2(4.0_f32.min(rect.width()), rect.height()));
    p.rect_filled(bar_rect, CornerRadius::same(2), bar);
    if selected {
        p.rect_stroke(rect, CornerRadius::same(3), Stroke::new(2.0, app.theme.text), StrokeKind::Inside);
    }
    let inner = Rect::from_min_max(
        pos2(rect.min.x + 7.0, rect.min.y + 2.0),
        pos2(rect.max.x - 3.0, rect.max.y - 1.0),
    );
    // Too narrow for a word (a 15-minute item in Schedule view): a plain marker, its details in the tooltip,
    // rather than a block of "…".
    if inner.width() < BLOCK_TEXT_MIN_W || inner.height() < 6.0 {
        return;
    }
    let mut x = inner.min.x;
    let bell = o.reminder && app.s.settings.bool("calendar.bellIcon");
    for (on, icon) in
        [(o.recurring, "recurrence"), (o.private, "private"), (o.meeting, "people"), (bell, "bell")]
    {
        if on && inner.width() > 60.0 {
            let r = Rect::from_min_size(pos2(x, inner.min.y + 1.0), Vec2::splat(12.0));
            app.icons.paint(ui, r, icon, app.theme.text_muted);
            x += 14.0;
        }
    }
    let w = inner.max.x - x;
    let sub = paint_occ_text(
        ui,
        pos2(x, inner.min.y),
        &o.label(),
        o,
        FontId::proportional(12.5),
        app.theme.text,
        w,
    );
    let mut y = sub.max.y + 1.0;
    if !o.location.trim().is_empty() && inner.height() > 30.0 {
        let r = paint_text_clipped(
            ui,
            pos2(inner.min.x, y),
            o.location.trim(),
            FontId::proportional(11.5),
            app.theme.text_muted,
            inner.width(),
        );
        y = r.max.y + 1.0;
    }
    if preview && !o.preview.is_empty() && inner.max.y - y > 12.0 {
        // Preview: as much of the body as fits, line by line.
        let mut job = egui::text::LayoutJob::single_section(
            o.preview.clone(),
            egui::TextFormat {
                font_id: FontId::proportional(11.5),
                color: app.theme.text_muted,
                ..Default::default()
            },
        );
        job.wrap = egui::text::TextWrapping {
            max_width: inner.width().max(8.0),
            max_rows: ((inner.max.y - y) / 14.0).floor().max(1.0) as usize,
            break_anywhere: false,
            overflow_character: Some('…'),
        };
        let galley = ui.painter().layout_job(job);
        p.galley(pos2(inner.min.x, y), galley, app.theme.text_muted);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------------------------------------

fn background_id() -> Id {
    Id::new("mailcraft.modules.calendar.background")
}

/// Every frame, in any module: internet calendar refreshes that Send/Receive or Update Folder queued
/// (`SyncState::calendar_requests`) go to the fetch workers, and finished fetches are applied. Idle frames
/// only read one flag.
pub(super) fn background(ctx: &egui::Context, app: &mut App) {
    let requests = mailcraft_engine::cmd::send_receive::take_calendar_requests(&mut app.s);
    let active = ctx.data(|d| d.get_temp::<bool>(background_id())).unwrap_or(false);
    if requests.is_empty() && !active {
        return;
    }
    let mut cu = load_ui(ctx);
    for d in requests {
        cu.fetches.start(ctx, d);
    }
    if app.s.view.module != mailcraft_engine::Module::Calendar {
        // The calendar view polls for itself while it shows.
        windows::poll_fetches(ctx, app, &mut cu);
    }
    let still = cu.fetches.active();
    store_ui(ctx, cu);
    ctx.data_mut(|d| d.insert_temp(background_id(), still));
    if still {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

pub(super) fn show(ui: &mut Ui, app: &mut App) {
    let ctx = ui.ctx().clone();
    let mut st = load(&ctx);
    let mut cu = load_ui(&ctx);
    sync_mode(app, &mut st);
    adopt_saved(app, &mut st, &mut cu);
    // Dialog requests from commands (the calendar's own windows among them) before the dialogs area draws.
    crate::dialogs::take_notices(app);
    windows::take_requests(app, &mut st, &mut cu);
    windows::poll_fetches(&ctx, app, &mut cu);
    let now = ctx.input(|i| i.time);
    if cu.last_poll.is_none_or(|t| now - t >= POLL_SECS || now < t) {
        cu.last_poll = Some(now);
        windows::refresh_due(&ctx, app, &mut cu);
        views::refresh_time_zone(&mut app.s);
    }
    if cu.last_auto.is_none_or(|t| now - t >= AUTO_RESPOND_SECS || now < t) {
        cu.last_auto = Some(now);
        auto_respond(app, &mut cu);
    }
    let look = Look::of(app, &st);
    let full = ui.max_rect();
    // View › Folder Pane: the pane, the minimised strip (chosen, narrow windows, Reading view) or nothing.
    let (pane, mut main) = module_folder_pane(ui, app);
    // To-Do Bar (View › Layout › To-Do Bar) on the right, when there is room.
    let bar_w = super::TODO_BAR_WIDTH;
    if super::todo_bar_shown(app) && main.width() >= bar_w + 420.0 {
        let bar = Rect::from_min_max(pos2(main.max.x - bar_w, main.min.y), main.max);
        main = Rect::from_min_max(main.min, pos2(bar.min.x - super::common::GAP, main.max.y));
        child(ui, bar, "mc-cal-todo-bar", |ui| super::todo_bar(ui, app));
    }
    if let Some(p) = pane {
        child(ui, p.shrink2(vec2(6.0, 0.0)), "mc-cal-pane", |ui| folder_pane(ui, app, &mut st, &look));
    }
    pane_bg(ui, main, app.theme.reading);
    child(ui, main, "mc-cal-main", |ui| content(ui, app, &mut st, &mut cu, &look));
    keyboard(&ctx, app, &mut st);
    windows::show(&ctx, app, &mut st, &mut cu);
    ui.expand_to_include_rect(full);
    store(&ctx, st);
    store_ui(&ctx, cu);
}

/// File › Options › Calendar › Automatic accept or decline: answers new meeting requests (each message is
/// looked at once per session; `calendar.autoRespond` skips those already answered).
fn auto_respond(app: &mut App, cu: &mut CalUi) {
    let ids: Vec<u64> = engine_cal::auto_respond_candidates(&app.s)
        .into_iter()
        .map(|i| i.0)
        .filter(|i| !cu.auto_seen.contains(i))
        .collect();
    if ids.is_empty() {
        return;
    }
    if cu.auto_seen.len() > 10_000 {
        cu.auto_seen.clear();
    }
    cu.auto_seen.extend(ids.iter().copied());
    if let Some(out) = app.run("calendar.autoRespond", json!({ "ids": ids }))
        && let Some(n) = out.get("handled").and_then(Value::as_array).map(Vec::len).filter(|n| *n > 0)
    {
        let s = if n == 1 { "" } else { "s" };
        app.ui.toast = Some((format!("Answered {n} meeting request{s} automatically."), 0.0));
    }
}

fn keyboard(ctx: &egui::Context, app: &mut App, st: &mut ModState) {
    if ctx.memory(|m| m.focused().is_some()) {
        return;
    }
    let Some(sel) =
        app.ui.module_selection.map(ItemId).filter(|i| app.s.mailbox.appointments.contains_key(i))
    else {
        return;
    };
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete)) {
        app.run("calendar.delete", json!({ "id": sel.0 }));
        if !app.s.mailbox.appointments.contains_key(&sel) {
            app.ui.module_selection = None;
        }
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        editors::open_appointment(app, st, sel);
    }
}

fn folder_pane(ui: &mut Ui, app: &mut App, st: &mut ModState, look: &Look) {
    ScrollArea::vertical().id_salt("mc-cal-pane-scroll").auto_shrink([false, false]).show(ui, |ui| {
        let sel = selected_day(app, st);
        let (y, m) = st.nav_month.unwrap_or_else(|| {
            let (y, m, _) = ymd(sel);
            (y, m)
        });
        let months = if ui.available_height() > 560.0 { 2 } else { 1 };
        for i in 0..months {
            let (yy, mm) = add_months(y, m, i);
            mini_month(ui, app, st, look, yy, mm, i == 0);
            ui.add_space(10.0);
        }
        ui.separator();
        if group_header(ui, app, "My Calendars", st.cal_group_collapsed, "mc-cal-group") {
            st.cal_group_collapsed = !st.cal_group_collapsed;
        }
        if !st.cal_group_collapsed {
            let shown: BTreeSet<u64> = shown_calendars(app, st).iter().map(|f| f.0).collect();
            let current = current_folder(app, Kind::Appointment);
            let (clicked, toggled) = folder_rows(ui, app, FolderKind::Calendar, current, Some(&shown));
            if let Some(f) = toggled {
                let mut set = shown.clone();
                if !set.remove(&f.0) {
                    set.insert(f.0);
                }
                let count = set.len();
                st.cal_shown = set;
                auto_layout(app, st, shown.len(), count);
            }
            if let Some(f) = clicked {
                let mut set = shown;
                set.insert(f.0);
                st.cal_shown = set;
                select_folder(app, f);
            }
            // Colour key of the shown calendars.
            let cals = ordered(app, &shown_calendars(app, st));
            if cals.len() > 1 {
                ui.add_space(4.0);
                for f in cals {
                    let name = app.s.mailbox.folder(f).map(|x| x.name.clone()).unwrap_or_default();
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                        ui.painter().rect_filled(r, CornerRadius::same(2), calendar_color32(app, f));
                        ui.label(RichText::new(name).size(12.0).color(app.theme.text_muted));
                    });
                }
            }
        }
    });
}

/// File › Options › Calendar › Display options: checking calendars until `calendar.autoScheduleViewAt` show
/// switches a vertical layout (day, work week, week) to Schedule View; unchecking them down to
/// `calendar.autoVerticalLayoutAt` switches Schedule View back to Day.
fn auto_layout(app: &mut App, st: &mut ModState, before: usize, after: usize) {
    let set = &app.s.settings;
    let at_most = |key: &str| usize::try_from(set.int(key)).unwrap_or(usize::MAX);
    let vertical = matches!(st.cal.mode, CalendarMode::Day | CalendarMode::WorkWeek | CalendarMode::Week);
    let to = if after > before
        && vertical
        && set.bool("calendar.autoScheduleView")
        && after >= at_most("calendar.autoScheduleViewAt")
    {
        Some(CalendarMode::Schedule)
    } else if after < before
        && st.cal.mode == CalendarMode::Schedule
        && set.bool("calendar.autoVerticalLayout")
        && after <= at_most("calendar.autoVerticalLayoutAt")
    {
        Some(CalendarMode::Day)
    } else {
        None
    };
    if let Some(mode) = to {
        run_view_cmd(app, st, "calendar.arrange", json!({ "mode": mode.name() }));
    }
}

/// Date navigator month (weeks from the first day of the week; week numbers on the left when shown).
fn mini_month(ui: &mut Ui, app: &mut App, st: &mut ModState, look: &Look, y: i64, m: u8, arrows: bool) {
    let w = ui.available_width().min(240.0);
    let num_w = if look.week_numbers { WEEK_NO_W.min(w / 8.0) } else { 0.0 };
    let cell = ((w - num_w) / 7.0).floor();
    let (head, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
    let text = format!("{} {y}", month_name(m));
    ui.painter().text(
        head.center(),
        Align2::CENTER_CENTER,
        &text,
        FontId::proportional(13.5),
        app.theme.text,
    );
    if arrows {
        for (dx, icon, label, delta) in
            [(10.0, "chevron-left", "Previous month", -1), (w - 10.0, "chevron-right", "Next month", 1)]
        {
            let r = Rect::from_center_size(pos2(head.min.x + dx, head.center().y), Vec2::splat(18.0));
            let resp = ui.interact(r, Id::new(("mc-nav-arrow", delta)), Sense::click());
            a11y(&resp, WidgetType::Button, label);
            if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(3), app.theme.hover);
            }
            app.icons.paint(ui, r.shrink(3.0), icon, app.theme.text_muted);
            if resp.clicked() {
                st.nav_month = Some(add_months(y, m, delta));
            }
        }
    }
    let (row, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
    for c in 0..7 {
        let x = row.min.x + num_w + cell * (c as f32) + cell / 2.0;
        let name = WEEK_INITIALS.get((c + usize::from(look.first_day)) % 7).copied().unwrap_or("");
        ui.painter().text(
            pos2(x, row.center().y),
            Align2::CENTER_CENTER,
            name,
            FontId::proportional(11.0),
            app.theme.text_muted,
        );
    }
    let first = day_of(y, m, 1).unwrap_or_else(|| today(app));
    let start = week_start(first, look.first_day);
    let dim = i64::from(dates::days_in_month(y, m));
    let weeks = ((first - start) + dim + 6) / 7;
    let sel = selected_day(app, st);
    let visible: BTreeSet<i64> = if st.cal.mode == CalendarMode::Month {
        [sel].into_iter().collect()
    } else {
        visible_days(st, sel, &look.work_days, look.first_day).into_iter().collect()
    };
    let busy: BTreeSet<i64> = gather(app, &shown_calendars(app, st), start, start + weeks * 7)
        .iter()
        .flat_map(|o| o.first_day()..=o.last_day().min(o.first_day() + 62))
        .collect();
    let now = today(app);
    let (grid, _) = ui.allocate_exact_size(vec2(w, cell.min(24.0) * weeks as f32), Sense::hover());
    let ch = cell.min(24.0);
    if look.week_numbers {
        for r in 0..weeks {
            let n = views::week_number(start + r * 7, look.first_day, &look.week_rule);
            ui.painter().text(
                pos2(grid.min.x + num_w / 2.0, grid.min.y + ch * r as f32 + ch / 2.0),
                Align2::CENTER_CENTER,
                n.to_string(),
                FontId::proportional(10.0),
                app.theme.text_muted,
            );
        }
    }
    for i in 0..weeks * 7 {
        let d = start + i;
        let (r, c) = (i / 7, i % 7);
        let rect = Rect::from_min_size(
            pos2(grid.min.x + num_w + cell * c as f32, grid.min.y + ch * r as f32),
            vec2(cell, ch),
        );
        let resp = ui.interact(rect, Id::new(("mc-nav-day", y, m, d)), Sense::click());
        a11y(&resp, WidgetType::Button, &long_date(d));
        let in_month = ymd(d).1 == m;
        let mut color = if in_month { app.theme.text } else { app.theme.text_muted };
        if d == now {
            ui.painter().rect_filled(rect.shrink(1.0), CornerRadius::same(3), app.theme.accent);
            color = app.theme.on_accent;
        } else if visible.contains(&d) {
            ui.painter().rect_filled(rect.shrink(1.0), CornerRadius::same(3), app.theme.selected);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect.shrink(1.0), CornerRadius::same(3), app.theme.hover);
        }
        let (_, _, dd) = ymd(d);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            dd.to_string(),
            FontId::proportional(11.5),
            color,
        );
        if st.cal.nav_marks && busy.contains(&d) && in_month {
            let dot = pos2(rect.center().x, rect.max.y - 3.0);
            ui.painter().circle_filled(
                dot,
                1.6,
                if d == now { app.theme.on_accent } else { app.theme.accent },
            );
        }
        if resp.clicked() {
            let keep = st.nav_month;
            go_to(app, st, d);
            st.nav_month = keep;
        }
    }
}

const WEEK_INITIALS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

fn content(ui: &mut Ui, app: &mut App, st: &mut ModState, cu: &mut CalUi, look: &Look) {
    let rect = ui.max_rect();
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), BAR_H));
    child(ui, bar.shrink2(vec2(10.0, 8.0)), "mc-cal-bar", |ui| folder_bar(ui, app, st, look));
    ui.painter().hline(rect.x_range(), bar.max.y, Stroke::new(1.0, app.theme.separator));
    let mut top = bar.max.y + 1.0;
    // Internet calendar: when it was updated, a Refresh button.
    if let Some(folder) = current_folder(app, Kind::Appointment)
        && let Some(sub) = props::read(&app.s.mailbox, folder).subscription
    {
        let strip = Rect::from_min_size(pos2(rect.min.x, top), vec2(rect.width(), INFO_H));
        child(ui, strip.shrink2(vec2(10.0, 3.0)), "mc-cal-info", |ui| {
            windows::subscription_strip(ui, app, cu, folder, &sub);
        });
        ui.painter().hline(rect.x_range(), strip.max.y, Stroke::new(1.0, app.theme.separator));
        top = strip.max.y + 1.0;
    }
    let body = Rect::from_min_max(pos2(rect.min.x, top), rect.max);
    if body.height() < 40.0 || body.width() < 80.0 {
        return;
    }
    // Reading pane (View › Reading Pane): Right or Bottom, when there is room.
    let (view, reading) = match st.cal.reading_pane {
        ReadingPanePos::Right if body.width() > 640.0 => {
            let split = body.max.x - (body.width() * 0.34).clamp(260.0, 420.0);
            (
                Rect::from_min_max(body.min, pos2(split - 1.0, body.max.y)),
                Some(Rect::from_min_max(pos2(split, body.min.y), body.max)),
            )
        }
        ReadingPanePos::Bottom if body.height() > 320.0 => {
            let split = body.max.y - (body.height() * 0.38).clamp(140.0, 360.0);
            (
                Rect::from_min_max(body.min, pos2(body.max.x, split - 1.0)),
                Some(Rect::from_min_max(pos2(body.min.x, split), body.max)),
            )
        }
        _ => (body, None),
    };
    let cals = shown_calendars(app, st);
    if st.cal.current.is_table() {
        child(ui, view, "mc-cal-table", |ui| table::table_view(ui, app, st, &cals, view));
    } else {
        match st.cal.mode {
            CalendarMode::Schedule => schedule_view(ui, app, st, look, view),
            mode => panes(ui, app, st, cu, look, view, &cals, mode == CalendarMode::Month),
        }
    }
    if let Some(r) = reading {
        let line = Stroke::new(1.0, app.theme.separator);
        if st.cal.reading_pane == ReadingPanePos::Right {
            ui.painter().vline(r.min.x, r.y_range(), line);
        } else {
            ui.painter().hline(r.x_range(), r.min.y, line);
        }
        child(ui, r.shrink2(vec2(14.0, 10.0)), "mc-cal-reading", |ui| reading_pane(ui, app));
    }
}

/// Lays out one pane per calendar (side by side) or one overlaid pane, each with its calendar tabs, and
/// draws the month or day/week view in each.
#[allow(clippy::too_many_arguments)]
fn panes(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    cu: &mut CalUi,
    look: &Look,
    rect: Rect,
    cals: &BTreeSet<FolderId>,
    month: bool,
) {
    let list = ordered(app, cals);
    let side_by_side = list.len() > 1 && !st.cal.overlay && list.len() <= MAX_SIDE_BY_SIDE;
    let mut panes: Vec<(Rect, Vec<FolderId>, Pane)> = Vec::new();
    if side_by_side {
        let w = rect.width() / list.len() as f32;
        for (k, f) in list.iter().enumerate() {
            let r = Rect::from_min_size(pos2(rect.min.x + w * k as f32, rect.min.y), vec2(w, rect.height()));
            panes.push((r, vec![*f], Pane { cals: [*f].into_iter().collect(), key: k + 1 }));
        }
    } else {
        panes.push((
            rect,
            if list.len() > 1 { list.clone() } else { Vec::new() },
            Pane { cals: cals.clone(), key: 0 },
        ));
    }
    for (r, tabs, pane) in panes {
        let mut body = r;
        if !tabs.is_empty() {
            let strip = Rect::from_min_size(r.min, vec2(r.width(), TAB_H));
            calendar_tabs(ui, app, strip, &tabs, pane.key);
            body = Rect::from_min_max(pos2(r.min.x, strip.max.y + 1.0), r.max);
        }
        if body.height() < 40.0 || body.width() < 60.0 {
            continue;
        }
        if month {
            month_view(ui, app, st, look, &pane, body);
        } else {
            time_grid_view(ui, app, st, cu, look, &pane, body);
        }
        if pane.key > 0 {
            ui.painter().vline(r.max.x, r.y_range(), Stroke::new(1.0, app.theme.separator));
        }
    }
}

/// Coloured calendar tabs over a pane; clicking one makes it the current calendar.
fn calendar_tabs(ui: &mut Ui, app: &mut App, strip: Rect, tabs: &[FolderId], key: usize) {
    let n = tabs.len().max(1) as f32;
    let w = (strip.width() / n).min(220.0);
    for (i, f) in tabs.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(strip.min.x + w * i as f32 + 2.0, strip.min.y + 2.0),
            vec2(w - 4.0, TAB_H - 3.0),
        );
        let color = calendar_color32(app, *f);
        let name = app.s.mailbox.folder(*f).map(|x| x.name.clone()).unwrap_or_default();
        let resp = ui.interact(r, Id::new(("mc-cal-tab", key, f.0)), Sense::click());
        a11y(&resp, WidgetType::Button, &name);
        ui.painter().rect_filled(
            r,
            CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 },
            blend(app.theme.reading, color, 0.75),
        );
        paint_text_clipped(
            ui,
            pos2(r.min.x + 8.0, r.min.y + 4.0),
            &name,
            FontId::proportional(12.5),
            Color32::WHITE,
            r.width() - 12.0,
        );
        if resp.clicked() && current_folder(app, Kind::Appointment) != Some(*f) {
            select_folder(app, *f);
        }
    }
}

fn folder_bar(ui: &mut Ui, app: &mut App, st: &mut ModState, look: &Look) {
    let sel = selected_day(app, st);
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        if crate::widgets::icon_button(ui, app, "today", "Today", true).on_hover_text("Go to today").clicked()
        {
            run_view_cmd(app, st, "calendar.today", json!({}));
        }
        let prev = crate::widgets::icon_only(ui, app, "chevron-left", "Previous", true);
        a11y(&prev, WidgetType::Button, "Previous");
        if prev.clicked() {
            go_to(app, st, step(st, sel, -1));
        }
        let next = crate::widgets::icon_only(ui, app, "chevron-right", "Next", true);
        a11y(&next, WidgetType::Button, "Next");
        if next.clicked() {
            go_to(app, st, step(st, sel, 1));
        }
        ui.add_space(8.0);
        let text = if st.cal.current.is_table() {
            st.cal.current.label().to_owned()
        } else {
            title(st, sel, &look.work_days, look.first_day)
        };
        ui.label(RichText::new(text).size(19.0).color(app.theme.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut mode = st.cal.mode;
            ComboBox::from_id_salt("mc-cal-mode").selected_text(mode.label()).width(130.0).show_ui(
                ui,
                |ui| {
                    for m in CalendarMode::ALL {
                        ui.selectable_value(&mut mode, m, m.label());
                    }
                },
            );
            if mode != st.cal.mode {
                run_view_cmd(app, st, "calendar.arrange", json!({ "mode": mode.name() }));
            }
            let mut current = st.cal.current;
            ComboBox::from_id_salt("mc-cal-current").selected_text(current.label()).width(96.0).show_ui(
                ui,
                |ui| {
                    for v in CalendarCurrentView::ALL {
                        ui.selectable_value(&mut current, v, v.label());
                    }
                },
            );
            if current != st.cal.current {
                run_view_cmd(app, st, "view.changeView", json!({ "name": current.label() }));
            }
            let go = crate::widgets::icon_only(ui, app, "calendar", "Go to Date", true);
            a11y(&go, WidgetType::Button, "Go to Date");
            if go.clicked() {
                let (date, mode) = (st.cal.date.clone(), st.cal.mode);
                editors::open_go_to(st, &date, mode);
            }
        });
    });
}

/// Reading pane: the selected appointment.
fn reading_pane(ui: &mut Ui, app: &mut App) {
    let Some(a) = app.ui.module_selection.and_then(|i| app.s.mailbox.appointments.get(&ItemId(i))).cloned()
    else {
        empty_message(
            ui,
            ui.max_rect(),
            app,
            "Select an item to read",
            "Click an appointment to see it here.",
        );
        return;
    };
    let cal = cal_clock(app);
    let tz = cal.offset_at(a.start.unix);
    ScrollArea::vertical().id_salt("mc-cal-reading-scroll").auto_shrink([false, false]).show(ui, |ui| {
        let subject = if a.subject.trim().is_empty() { "(No subject)" } else { a.subject.trim() };
        ui.label(RichText::new(subject).size(18.0).strong().color(app.theme.text));
        ui.add_space(4.0);
        let when = engine_cal::describe_recurrence(&a, tz).unwrap_or_else(|| {
            let (s, e) = (cal.local(a.start.unix), cal.local(a.end.unix));
            if a.all_day {
                long_date(s.div_euclid(DAY))
            } else {
                format!(
                    "{} {} – {}",
                    long_date(s.div_euclid(DAY)),
                    clock(s.rem_euclid(DAY) / 60),
                    clock(e.rem_euclid(DAY) / 60)
                )
            }
        });
        ui.label(RichText::new(when).color(app.theme.text));
        if let Some(z) = a.zone() {
            ui.label(
                RichText::new(format!("Time zone: {}", z.replace('_', " "))).color(app.theme.text_muted),
            );
        }
        if !a.location.trim().is_empty() {
            ui.label(RichText::new(format!("Location: {}", a.location.trim())).color(app.theme.text_muted));
        }
        let cal = app.s.mailbox.folder(a.folder).map(|f| f.name.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, CornerRadius::same(2), calendar_color32(app, a.folder));
            ui.label(RichText::new(cal).color(app.theme.text_muted));
        });
        if let Some(o) = &a.organizer {
            ui.label(
                RichText::new(format!("Organizer: {}", o.to_display_string())).color(app.theme.text_muted),
            );
        }
        if !a.attendees.is_empty() {
            let list: Vec<String> = a.attendees.iter().map(|x| x.mailbox.to_display_string()).collect();
            ui.label(RichText::new(format!("Attendees: {}", list.join("; "))).color(app.theme.text_muted));
        }
        if !a.categories.is_empty() {
            ui.label(
                RichText::new(format!("Categories: {}", a.categories.join(", "))).color(app.theme.text_muted),
            );
        }
        if !a.body.trim().is_empty() && !a.private {
            ui.separator();
            ui.label(RichText::new(a.body.trim()).color(app.theme.text));
        }
    });
}

// ---------------------------------------------------------------------------------------------------------
// Month view
// ---------------------------------------------------------------------------------------------------------

/// A calendar to create items in: `f` when it can take them, else the default calendar.
fn writable(app: &App, f: Option<FolderId>) -> Option<FolderId> {
    match f {
        Some(f) if !props::is_read_only(&app.s.mailbox, f) => Some(f),
        _ => engine_cal::pim::default_folder(&app.s, Kind::Appointment),
    }
}

/// The calendar new items in `pane` go to.
fn pane_folder(app: &App, pane: &Pane) -> Option<FolderId> {
    let current = current_folder(app, Kind::Appointment);
    let f = if pane.cals.len() == 1 { pane.cals.iter().next().copied() } else { current };
    writable(app, f)
}

fn month_view(ui: &mut Ui, app: &mut App, st: &mut ModState, look: &Look, pane: &Pane, rect: Rect) {
    let sel = selected_day(app, st);
    let (start, weeks) = month_grid(sel, look.first_day);
    let (_, month, _) = ymd(sel);
    let now = today(app);
    let head_h = 26.0;
    let p = ui.painter().clone();
    // Week numbers in a column on the left.
    let rect = if look.week_numbers && rect.width() > 7.0 * 40.0 {
        let cell_h = ((rect.height() - head_h) / weeks as f32).max(30.0);
        for r in 0..weeks {
            let n = views::week_number(start + r * 7, look.first_day, &look.week_rule);
            p.text(
                pos2(rect.min.x + WEEK_NO_W / 2.0, rect.min.y + head_h + cell_h * r as f32 + 8.0),
                Align2::CENTER_TOP,
                n.to_string(),
                FontId::proportional(11.0),
                app.theme.text_muted,
            );
        }
        Rect::from_min_max(pos2(rect.min.x + WEEK_NO_W, rect.min.y), rect.max)
    } else {
        rect
    };
    let cell_w = rect.width() / 7.0;
    let cell_h = ((rect.height() - head_h) / weeks as f32).max(30.0);
    let k = pane.key;
    for c in 0..7 {
        let r = Rect::from_min_size(pos2(rect.min.x + cell_w * c as f32, rect.min.y), vec2(cell_w, head_h));
        let wd = (c + usize::from(look.first_day)) % 7;
        let strong = usize::from(weekday(now)) == wd;
        let name = super::common::WEEKDAYS.get(wd).copied().unwrap_or("");
        let name = if cell_w < 70.0 { name.get(..3).unwrap_or(name) } else { name };
        p.text(
            pos2(r.min.x + 8.0, r.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(if strong { 13.5 } else { 13.0 }),
            if strong { app.theme.accent } else { app.theme.text_muted },
        );
    }
    let occs = gather_in(app, &pane.cals, start, start + weeks * 7, Some(&st.cal));
    let shaded = blend(app.theme.reading, app.theme.backdrop, 0.45);
    let line = Stroke::new(1.0, app.theme.separator);
    for i in 0..weeks * 7 {
        let d = start + i;
        let (r, c) = (i / 7, i % 7);
        let cell = Rect::from_min_size(
            pos2(rect.min.x + cell_w * c as f32, rect.min.y + head_h + cell_h * r as f32),
            vec2(cell_w, cell_h),
        );
        let resp = ui.interact(cell, Id::new(("mc-cal-cell", k, d)), Sense::click());
        let alt = mailcraft_engine::queries::alternate_date(&app.s, d);
        a11y(&resp, WidgetType::Button, &with_alternate(long_date(d), alt.as_ref()));
        let (_, m, dd) = ymd(d);
        let fill = if d == sel {
            app.theme.selected
        } else if m != month {
            shaded
        } else {
            app.theme.reading
        };
        p.rect_filled(cell, CornerRadius::ZERO, fill);
        p.rect_stroke(cell, CornerRadius::ZERO, line, StrokeKind::Inside);
        let label = if dd == 1 || i == 0 {
            format!("{} {dd}", month_name(m).get(..3).unwrap_or(""))
        } else {
            dd.to_string()
        };
        if d == now {
            p.rect_filled(
                Rect::from_min_size(cell.min, vec2(cell.width(), 3.0)),
                CornerRadius::ZERO,
                app.theme.accent,
            );
        }
        let first = p.text(
            pos2(cell.min.x + 8.0, cell.min.y + 6.0),
            Align2::LEFT_TOP,
            label,
            FontId::proportional(13.0),
            if d == now {
                app.theme.accent
            } else if m == month {
                app.theme.text
            } else {
                app.theme.text_muted
            },
        );
        // Calendar › "Enable an alternate calendar": that calendar's date at the top right.
        if let Some(a) = &alt {
            let text = if a.day == 1 || i == 0 { a.short() } else { a.day.to_string() };
            paint_text_clipped(
                ui,
                pos2((cell.max.x - 6.0 - 90.0).max(first.max.x + 6.0), cell.min.y + 7.0),
                &text,
                FontId::proportional(11.0),
                app.theme.text_muted,
                (cell.max.x - 6.0 - first.max.x - 6.0).clamp(0.0, 90.0),
            );
        }
        if resp.clicked() {
            app.ui.module_selection = None;
            if d != sel {
                go_to(app, st, d);
            }
        }
        if resp.double_clicked() {
            let folder = pane_folder(app, pane);
            forms::new_at(app, st, folder, d, 0, true);
        }
        // Items
        let cell_fill = fill;
        let line_h = 19.0;
        let top = cell.min.y + 24.0;
        let avail = ((cell.max.y - top - 2.0) / line_h).floor().max(0.0) as usize;
        let mut items: Vec<&Occ> = occs.iter().filter(|o| o.covers(d)).collect();
        items.sort_by_key(|o| (!o.banner(), o.start, o.id));
        let n = items.len();
        let show = if n > avail { avail.saturating_sub(1) } else { n };
        for (j, o) in items.iter().take(show).enumerate() {
            let chip = Rect::from_min_size(
                pos2(cell.min.x + 3.0, top + line_h * j as f32),
                vec2(cell_w - 6.0, line_h - 2.0),
            );
            let resp = ui.interact(chip, Id::new(("mc-cal-occ", k, o.id.0, d)), Sense::click());
            a11y(&resp, WidgetType::Button, &occ_a11y(o));
            let selected = app.ui.module_selection == Some(o.id.0);
            let (fill, bar) = occ_colors(app, o);
            if o.banner() {
                p.rect_filled(chip, CornerRadius::same(3), fill);
                p.rect_filled(
                    Rect::from_min_size(chip.min, vec2(3.0, chip.height())),
                    CornerRadius::same(2),
                    bar,
                );
                paint_occ_text(
                    ui,
                    pos2(chip.min.x + 7.0, chip.min.y + 2.0),
                    &o.label(),
                    o,
                    FontId::proportional(12.0),
                    app.theme.text,
                    chip.width() - 10.0,
                );
            } else {
                if resp.hovered() || selected {
                    p.rect_filled(chip, CornerRadius::same(3), fill);
                }
                // The dot keeps 3:1 against what it sits on (a dark calendar colour on the dark theme's
                // selected day was nearly invisible).
                let under = if resp.hovered() || selected { fill } else { cell_fill };
                p.circle_filled(pos2(chip.min.x + 6.0, chip.center().y), 3.5, month_dot(bar, under));
                let text = month_chip_text(o, st.cal.month_end_time);
                paint_occ_text(
                    ui,
                    pos2(chip.min.x + 13.0, chip.min.y + 2.0),
                    &text,
                    o,
                    FontId::proportional(12.0),
                    app.theme.text,
                    chip.width() - 15.0,
                );
            }
            if selected {
                p.rect_stroke(
                    chip,
                    CornerRadius::same(3),
                    Stroke::new(1.5, app.theme.text),
                    StrokeKind::Inside,
                );
            }
            occ_interact(app, st, &resp, o);
        }
        if n > show {
            let more = Rect::from_min_size(
                pos2(cell.min.x + 3.0, top + line_h * show as f32),
                vec2(cell_w - 6.0, line_h - 2.0),
            );
            let resp = ui.interact(more, Id::new(("mc-cal-more", k, d)), Sense::click());
            let text = format!("+{} more", n - show);
            a11y(&resp, WidgetType::Button, &text);
            p.text(
                pos2(more.min.x + 6.0, more.center().y),
                Align2::LEFT_CENTER,
                &text,
                FontId::proportional(12.0),
                app.theme.link,
            );
            if resp.clicked() {
                run_view_cmd(app, st, "calendar.goToDate", json!({ "date": day_string(d), "mode": "day" }));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Day / Work Week / Week
// ---------------------------------------------------------------------------------------------------------

/// Assigns side-by-side columns to overlapping spans: returns (column, columns in its cluster) per input.
pub(super) fn lay_out_columns(spans: &[(i64, i64)]) -> Vec<(usize, usize)> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| {
        spans.get(i).map(|s| (s.0, std::cmp::Reverse(s.1))).unwrap_or((0, std::cmp::Reverse(0)))
    });
    let mut out = vec![(0usize, 1usize); spans.len()];
    let mut cluster: Vec<usize> = Vec::new();
    let mut col_ends: Vec<i64> = Vec::new();
    let mut cluster_end = i64::MIN;
    let finish = |cluster: &mut Vec<usize>, cols: usize, out: &mut Vec<(usize, usize)>| {
        for &i in cluster.iter() {
            if let Some(slot) = out.get_mut(i) {
                slot.1 = cols.max(1);
            }
        }
        cluster.clear();
    };
    for i in order {
        let Some(&(s, e)) = spans.get(i) else { continue };
        let e = e.max(s + 1);
        if s >= cluster_end && !cluster.is_empty() {
            finish(&mut cluster, col_ends.len(), &mut out);
            col_ends.clear();
        }
        let col = match col_ends.iter().position(|&end| end <= s) {
            Some(c) => {
                if let Some(x) = col_ends.get_mut(c) {
                    *x = e;
                }
                c
            }
            None => {
                col_ends.push(e);
                col_ends.len() - 1
            }
        };
        if let Some(slot) = out.get_mut(i) {
            slot.0 = col;
        }
        cluster.push(i);
        cluster_end = if cluster.len() == 1 { e } else { cluster_end.max(e) };
    }
    finish(&mut cluster, col_ends.len(), &mut out);
    out
}

/// Lanes for all-day banners over `days`: (occurrence index, lane).
fn banner_lanes(occs: &[Occ], first: i64, last: i64) -> Vec<(usize, usize)> {
    let mut lanes: Vec<i64> = Vec::new();
    let mut out = Vec::new();
    for (i, o) in occs.iter().enumerate() {
        if !o.banner() {
            continue;
        }
        let (a, b) = (o.first_day().max(first), o.last_day().min(last));
        if a > b {
            continue;
        }
        let lane = match lanes.iter().position(|&end| end < a) {
            Some(l) => l,
            None => {
                lanes.push(i64::MIN);
                lanes.len() - 1
            }
        };
        if let Some(x) = lanes.get_mut(lane) {
            *x = b;
        }
        out.push((i, lane));
    }
    out
}

/// "9 AM" for whole hours, else "9:30 AM" (second time zone ruler).
fn ruler_label(minute: i64) -> String {
    let m = minute.rem_euclid(24 * 60);
    if m % 60 == 0 { hour_label(m / 60) } else { clock(m) }
}

/// Where a drag lands: the new local (start, end) of the block, snapped to the time scale. `pointer` =
/// (day, minute of day) under the pointer.
fn drag_target(d: &Drag, pointer: (i64, i64), slot: i64, bounds: (i64, i64)) -> (i64, i64) {
    let slot = slot.max(1);
    let snap = |m: i64| ((m as f64 / slot as f64).round() as i64).saturating_mul(slot);
    let (day, minute) = pointer;
    let minute = minute.clamp(bounds.0, bounds.1);
    let len = (d.end - d.start).max(0);
    match d.kind {
        DragKind::Move => {
            let m = snap(minute - d.grab_min);
            let start = day.saturating_mul(DAY).saturating_add(m.saturating_mul(60));
            (start, start.saturating_add(len))
        }
        DragKind::Top => {
            let day0 = d.start.div_euclid(DAY);
            let start = day0.saturating_mul(DAY).saturating_add(snap(minute).saturating_mul(60));
            (start.min(d.end - slot * 60), d.end)
        }
        DragKind::Bottom => {
            let day0 = (d.end - 1).max(d.start).div_euclid(DAY);
            let end = day0.saturating_mul(DAY).saturating_add(snap(minute).saturating_mul(60));
            (d.start, end.max(d.start + slot * 60))
        }
    }
}

/// Sets `key` on a JSON object.
fn put(v: &mut Value, key: &str, x: Value) {
    if let Some(o) = v.as_object_mut() {
        o.insert(key.to_owned(), x);
    }
}

/// Runs the engine command for a finished drag. Returns the item now selected.
fn commit_drag(app: &mut App, d: &Drag, target: (i64, i64)) -> Option<ItemId> {
    if target == (d.start, d.end) {
        return None;
    }
    let clock = cal_clock(app);
    let unix = |local: i64| clock.from_local(local).unix;
    let mut params = json!({ "id": d.id.0 });
    let id = match d.kind {
        DragKind::Move => {
            put(&mut params, "start", json!(unix(target.0)));
            "calendar.moveItem"
        }
        DragKind::Top => {
            put(&mut params, "start", json!(unix(target.0)));
            "calendar.resize"
        }
        DragKind::Bottom => {
            put(&mut params, "end", json!(unix(target.1)));
            "calendar.resize"
        }
    };
    if let Some(o) = d.occurrence {
        put(&mut params, "occurrence", json!(o));
    }
    let out = app.run(id, params)?;
    let new_id = out.get("id").and_then(Value::as_u64).map(ItemId);
    if out.get("sendUpdate").and_then(Value::as_bool) == Some(true)
        && let Some(n) = new_id
    {
        crate::dialogs::confirm(
            app,
            "Send Update",
            "This meeting changed. Send an update to the attendees?",
            "calendar.sendUpdate",
            json!({ "id": n.0 }),
        );
    }
    new_id
}

#[allow(clippy::too_many_arguments)]
fn time_grid_view(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    cu: &mut CalUi,
    look: &Look,
    pane: &Pane,
    rect: Rect,
) {
    let sel = selected_day(app, st);
    let days = visible_days(st, sel, &look.work_days, look.first_day);
    let (Some(&first), Some(&last)) = (days.first(), days.last()) else { return };
    let n = days.len() as f32;
    let now = today(app);
    let scroll_w = 12.0;
    let k = pane.key;
    let extra = look.extra_zones();
    let ruler_w = RULER_W * (1 + extra.len()) as f32;
    let col_w = ((rect.width() - ruler_w - scroll_w) / n).max(20.0);
    let p = ui.painter().clone();
    let line = Stroke::new(1.0, app.theme.separator);
    let tz_off = tz(app);

    // Day headers (and the zone labels over the rulers).
    let head_h = 44.0;
    if extra.is_empty() && !look.primary_label.is_empty() {
        // File › Options › Calendar › Time zones › Label, over the ruler.
        paint_text_clipped(
            ui,
            pos2(rect.min.x + 4.0, rect.min.y + head_h - 16.0),
            &look.primary_label,
            FontId::proportional(10.5),
            app.theme.text_muted,
            RULER_W - 8.0,
        );
    }
    if !extra.is_empty() {
        let primary =
            if look.primary_label.is_empty() { "Local".to_owned() } else { look.primary_label.clone() };
        let labels: Vec<&str> =
            extra.iter().map(|(l, _)| l.as_str()).chain(std::iter::once(primary.as_str())).collect();
        for (i, text) in labels.iter().enumerate() {
            paint_text_clipped(
                ui,
                pos2(rect.min.x + 4.0 + RULER_W * i as f32, rect.min.y + head_h - 16.0),
                text,
                FontId::proportional(10.5),
                app.theme.text_muted,
                RULER_W - 8.0,
            );
        }
    }
    for (i, &d) in days.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(rect.min.x + ruler_w + col_w * i as f32, rect.min.y),
            vec2(col_w, head_h),
        );
        let resp = ui.interact(r, Id::new(("mc-cal-dayhead", k, d)), Sense::click());
        let alt = mailcraft_engine::queries::alternate_date(&app.s, d);
        a11y(&resp, WidgetType::Button, &with_alternate(long_date(d), alt.as_ref()));
        let color = if d == now { app.theme.accent } else { app.theme.text };
        let (_, _, dd) = ymd(d);
        let num = p.text(
            pos2(r.min.x + 8.0, r.center().y),
            Align2::LEFT_CENTER,
            dd.to_string(),
            FontId::proportional(20.0),
            color,
        );
        let name = if col_w > 110.0 { weekday_name(d) } else { super::common::weekday_short(d) };
        let day_name = p.text(
            pos2(num.max.x + 6.0, r.center().y + 1.0),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(12.5),
            color,
        );
        // Calendar › "Enable an alternate calendar": that calendar's date after the weekday.
        if let Some(a) = &alt {
            paint_text_clipped(
                ui,
                pos2(day_name.max.x + 8.0, r.center().y - 6.0),
                &a.short(),
                FontId::proportional(10.5),
                app.theme.text_muted,
                (r.max.x - day_name.max.x - 12.0).max(0.0),
            );
        }
        if d == now {
            p.rect_filled(
                Rect::from_min_size(pos2(r.min.x, r.max.y - 3.0), vec2(col_w, 3.0)),
                CornerRadius::ZERO,
                app.theme.accent,
            );
        }
        if resp.clicked() && days.len() > 1 {
            run_view_cmd(app, st, "calendar.goToDate", json!({ "date": day_string(d), "mode": "day" }));
        }
    }

    // All-day strip.
    let occs = gather_in(app, &pane.cals, first, last + 1, Some(&st.cal));
    let lanes = banner_lanes(&occs, first, last);
    let nlanes = lanes.iter().map(|(_, l)| l + 1).max().unwrap_or(0).min(4);
    let strip_h = LANE_H * nlanes.max(1) as f32 + 6.0;
    let strip = Rect::from_min_size(pos2(rect.min.x, rect.min.y + head_h), vec2(rect.width(), strip_h));
    p.rect_filled(strip, CornerRadius::ZERO, blend(app.theme.reading, app.theme.backdrop, 0.35));
    p.hline(strip.x_range(), strip.max.y, line);
    p.text(
        pos2(strip.min.x + 6.0, strip.center().y),
        Align2::LEFT_CENTER,
        "All day",
        FontId::proportional(11.0),
        app.theme.text_muted,
    );
    for (i, &d) in days.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(strip.min.x + ruler_w + col_w * i as f32, strip.min.y),
            vec2(col_w, strip_h),
        );
        let resp = ui.interact(r, Id::new(("mc-cal-allday", k, d)), Sense::click());
        a11y(&resp, WidgetType::Button, &format!("All day, {}", long_date(d)));
        p.vline(r.min.x, r.y_range(), line);
        if resp.double_clicked() {
            let folder = pane_folder(app, pane);
            forms::new_at(app, st, folder, d, 0, true);
        }
    }
    for &(i, lane) in &lanes {
        if lane >= 4 {
            continue;
        }
        let Some(o) = occs.get(i) else { continue };
        let a = (o.first_day().max(first) - first) as f32;
        let b = (o.last_day().min(last) - first + 1) as f32;
        let r = Rect::from_min_max(
            pos2(strip.min.x + ruler_w + col_w * a + 2.0, strip.min.y + 3.0 + LANE_H * lane as f32),
            pos2(strip.min.x + ruler_w + col_w * b - 2.0, strip.min.y + 1.0 + LANE_H * (lane + 1) as f32),
        );
        let resp = ui.interact(r, Id::new(("mc-cal-banner", k, o.id.0, o.start)), Sense::click());
        a11y(&resp, WidgetType::Button, &occ_a11y(o));
        let selected = app.ui.module_selection == Some(o.id.0);
        paint_block(ui, app, r, o, selected, false);
        occ_interact(app, st, &resp, o);
    }

    // Daily Task List under the grid.
    let mut grid_bottom = rect.max.y;
    if st.cal.daily_task_list != DailyTaskList::Off && rect.height() > 260.0 {
        let tasks = engine_cal::tasks_for_days(&app.s.mailbox, tz_off, now, first, last + 1, 500);
        let minimized = st.cal.daily_task_list == DailyTaskList::Minimized;
        let rows = if minimized {
            0
        } else {
            days.iter()
                .map(|d| tasks.iter().filter(|(x, _)| x == d).count())
                .max()
                .unwrap_or(0)
                .clamp(1, TASK_ROWS)
        };
        let h = TASK_HEAD_H + TASK_ROW_H * rows as f32 + 4.0;
        let r = Rect::from_min_max(pos2(rect.min.x, rect.max.y - h), rect.max);
        grid_bottom = r.min.y - 1.0;
        task_strip(ui, app, st, r, ruler_w, col_w, &days, &tasks, minimized, k);
    }

    // Time grid.
    let grid = Rect::from_min_max(pos2(rect.min.x, strip.max.y + 1.0), pos2(rect.max.x, grid_bottom));
    if grid.height() < 20.0 {
        return;
    }
    let (work_start, work_end) = look.work;
    let (start_min, end_min) = if st.cal.working_hours { (work_start, work_end) } else { (0, 24 * 60) };
    let slot = i64::from(st.cal.time_scale.max(5));
    let px_per_min = look.slot_h / slot as f32;
    let total_h = (end_min - start_min) as f32 * px_per_min;
    let key =
        format!("{}|{}|{}|{}|{}", st.cal.mode.name(), st.cal.date, slot, st.cal.working_hours, look.slot_h);
    let mut area = ScrollArea::vertical().id_salt(("mc-cal-grid", k)).auto_shrink([false, false]);
    if cu.scroll_keys.get(&k) != Some(&key) {
        area = area.vertical_scroll_offset(((work_start - start_min).max(0)) as f32 * px_per_min);
        cu.scroll_keys.insert(k, key);
    }
    let now_min = now_minute(app);
    // Minutes each extra ruler is ahead of the calendar's own clock (leftmost ruler first).
    let shifts: Vec<i64> =
        extra.iter().map(|(_, off)| i64::from(*off) - i64::from(look.primary_off)).collect();
    let salt = format!("mc-cal-grid-area-{k}");
    child(ui, grid, &salt, |ui| {
        area.show(ui, |ui| {
            let (area_rect, resp) =
                ui.allocate_exact_size(vec2(ruler_w + col_w * n, total_h), Sense::click());
            let p = ui.painter().clone();
            let shade = blend(app.theme.reading, app.theme.backdrop, 0.3);
            let x0 = area_rect.min.x + ruler_w;
            for (i, &d) in days.iter().enumerate() {
                let col =
                    Rect::from_min_size(pos2(x0 + col_w * i as f32, area_rect.min.y), vec2(col_w, total_h));
                p.rect_filled(col, CornerRadius::ZERO, shade);
                if look.work_days.contains(&weekday(d)) {
                    let a = (work_start.max(start_min) - start_min) as f32 * px_per_min;
                    let b = (work_end.min(end_min) - start_min) as f32 * px_per_min;
                    if b > a {
                        p.rect_filled(
                            Rect::from_min_max(
                                pos2(col.min.x, col.min.y + a),
                                pos2(col.max.x, col.min.y + b),
                            ),
                            CornerRadius::ZERO,
                            app.theme.reading,
                        );
                    }
                }
                p.vline(col.min.x, col.y_range(), line);
            }
            for i in 0..shifts.len() {
                p.vline(area_rect.min.x + RULER_W * (i + 1) as f32, area_rect.y_range(), line);
            }
            if let Some((sd, sm)) = st.cal_slot
                && let Some(i) = days.iter().position(|&d| d == sd)
                && (start_min..end_min).contains(&sm)
            {
                let y = area_rect.min.y + (sm - start_min) as f32 * px_per_min;
                let r = Rect::from_min_size(pos2(x0 + col_w * i as f32, y), vec2(col_w, look.slot_h));
                p.rect_filled(r, CornerRadius::ZERO, app.theme.selected);
            }
            let mut m = start_min;
            while m < end_min {
                let y = area_rect.min.y + (m - start_min) as f32 * px_per_min;
                let on_hour = m % 60 == 0;
                let stroke = if on_hour {
                    line
                } else {
                    Stroke::new(1.0, blend(app.theme.separator, app.theme.reading, 0.55))
                };
                p.hline(x0..=area_rect.max.x, y, stroke);
                if on_hour {
                    p.text(
                        pos2(x0 - 6.0, y + 2.0),
                        Align2::RIGHT_TOP,
                        hour_label(m / 60),
                        FontId::proportional(11.0),
                        app.theme.text_muted,
                    );
                    for (i, shift) in shifts.iter().enumerate() {
                        p.text(
                            pos2(area_rect.min.x + RULER_W * (i + 1) as f32 - 6.0, y + 2.0),
                            Align2::RIGHT_TOP,
                            ruler_label(m + shift),
                            FontId::proportional(11.0),
                            app.theme.text_muted,
                        );
                    }
                }
                m += slot;
            }
            let pos_to_point = |pos: egui::Pos2| -> Option<(i64, i64)> {
                let i = ((pos.x - x0) / col_w).floor();
                let i = if i < 0.0 { 0.0 } else { i };
                let d = days.get(i as usize).or(days.last()).copied()?;
                let minute = start_min + ((pos.y - area_rect.min.y) / px_per_min) as i64;
                Some((d, minute))
            };
            if (resp.clicked() || resp.double_clicked())
                && let Some(pos) = resp.interact_pointer_pos()
                && pos.x >= x0
                && let Some((d, minute)) = pos_to_point(pos)
            {
                let minute = ((minute - start_min) / slot) * slot + start_min;
                let minute = minute.clamp(start_min, end_min - slot);
                st.cal_slot = Some((d, minute));
                app.ui.module_selection = None;
                if resp.double_clicked() {
                    let folder = pane_folder(app, pane);
                    forms::new_at(app, st, folder, d, minute, false);
                }
            }
            // Appointments.
            let dragging = cu.drag.clone().filter(|d| d.pane == k);
            let mut finished: Option<(Drag, (i64, i64))> = None;
            for (i, &d) in days.iter().enumerate() {
                let (ds, de) = (d * DAY + start_min * 60, d * DAY + end_min * 60);
                let todays: Vec<&Occ> = occs
                    .iter()
                    .filter(|o| !o.banner() && o.end.max(o.start + 1) > ds && o.start < de)
                    .collect();
                let spans: Vec<(i64, i64)> =
                    todays.iter().map(|o| (o.start.max(ds), o.end.min(de))).collect();
                let cols = lay_out_columns(&spans);
                for ((o, &(s, e)), &(c, nc)) in todays.iter().zip(spans.iter()).zip(cols.iter()) {
                    let y0 = area_rect.min.y + ((s - ds) as f32 / 60.0) * px_per_min;
                    let y1 = (area_rect.min.y + ((e - ds) as f32 / 60.0) * px_per_min).max(y0 + 18.0);
                    let w = (col_w - 8.0) / nc as f32;
                    let x = x0 + col_w * i as f32 + 2.0 + w * c as f32;
                    let r = Rect::from_min_max(pos2(x, y0 + 1.0), pos2(x + w - 1.0, y1 - 1.0));
                    let sense = if o.read_only { Sense::click() } else { Sense::click_and_drag() };
                    let resp = ui.interact(r, Id::new(("mc-cal-block", k, o.id.0, o.start, d)), sense);
                    a11y(&resp, WidgetType::Button, &occ_a11y(o));
                    // Edges resize (the block's own start/end edges only), the body moves.
                    let edge_of = |pos: egui::Pos2| -> DragKind {
                        if r.height() < 3.0 * EDGE {
                            DragKind::Move
                        } else if pos.y <= r.min.y + EDGE && s == o.start {
                            DragKind::Top
                        } else if pos.y >= r.max.y - EDGE && e == o.end {
                            DragKind::Bottom
                        } else {
                            DragKind::Move
                        }
                    };
                    if !o.read_only
                        && let Some(pos) = resp.hover_pos()
                        && dragging.is_none()
                        && edge_of(pos) != DragKind::Move
                    {
                        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
                    }
                    // Where the button went down (a drag is reported only after the pointer moved).
                    let origin =
                        ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos());
                    if resp.drag_started()
                        && let Some(pos) = origin
                        && let Some((_, minute)) = pos_to_point(pos)
                    {
                        let kind = edge_of(pos);
                        let occ_unix = o.start_unix;
                        cu.drag = Some(Drag {
                            id: o.id,
                            occurrence: o.recurring.then_some(occ_unix),
                            kind,
                            start: o.start,
                            end: o.end,
                            grab_min: (d * 1440 + minute) - o.start.div_euclid(60),
                            pane: k,
                        });
                        app.ui.module_selection = Some(o.id.0);
                    }
                    let is_dragged = dragging.as_ref().is_some_and(|g| g.id == o.id && g.start == o.start);
                    let selected = app.ui.module_selection == Some(o.id.0);
                    paint_block(ui, app, r, o, selected, look.preview);
                    if is_dragged {
                        p.rect_filled(r, CornerRadius::same(3), app.theme.reading.gamma_multiply(0.6));
                    }
                    if resp.drag_stopped()
                        && let Some(g) = cu.drag.clone().filter(|g| g.id == o.id && g.start == o.start)
                        && let Some(pos) =
                            resp.interact_pointer_pos().or_else(|| ui.ctx().pointer_latest_pos())
                        && let Some(pt) = pos_to_point(pos)
                    {
                        finished = Some((g.clone(), drag_target(&g, pt, slot, (start_min, end_min))));
                    } else {
                        occ_interact(app, st, &resp, o);
                    }
                }
                if d == now && (start_min..end_min).contains(&now_min) {
                    let y = area_rect.min.y + (now_min - start_min) as f32 * px_per_min;
                    let xa = x0 + col_w * i as f32;
                    p.hline(xa..=xa + col_w, y, Stroke::new(2.0, app.theme.danger));
                    p.circle_filled(pos2(xa, y), 4.0, app.theme.danger);
                }
            }
            // The dragged block's new place.
            if let Some(g) = &dragging
                && finished.is_none()
            {
                if let Some(pos) = ui.ctx().pointer_latest_pos()
                    && let Some(pt) = pos_to_point(pos)
                {
                    ui.ctx().set_cursor_icon(if g.kind == DragKind::Move {
                        CursorIcon::Grabbing
                    } else {
                        CursorIcon::ResizeVertical
                    });
                    let (ns, ne) = drag_target(g, pt, slot, (start_min, end_min));
                    for (i, &d) in days.iter().enumerate() {
                        let (ds, de) = (d * DAY + start_min * 60, d * DAY + end_min * 60);
                        if ne <= ds || ns >= de {
                            continue;
                        }
                        let y0 = area_rect.min.y + ((ns.max(ds) - ds) as f32 / 60.0) * px_per_min;
                        let y1 = area_rect.min.y + ((ne.min(de) - ds) as f32 / 60.0) * px_per_min;
                        let r = Rect::from_min_max(
                            pos2(x0 + col_w * i as f32 + 2.0, y0 + 1.0),
                            pos2(x0 + col_w * (i + 1) as f32 - 6.0, y1.max(y0 + 6.0) - 1.0),
                        );
                        p.rect_filled(r, CornerRadius::same(3), app.theme.accent.gamma_multiply(0.25));
                        p.rect_stroke(
                            r,
                            CornerRadius::same(3),
                            Stroke::new(2.0, app.theme.accent),
                            StrokeKind::Inside,
                        );
                        let label = format!(
                            "{} – {}",
                            clock(ns.rem_euclid(DAY) / 60),
                            clock(ne.rem_euclid(DAY) / 60)
                        );
                        paint_text_clipped(
                            ui,
                            pos2(r.min.x + 6.0, r.min.y + 2.0),
                            &label,
                            FontId::proportional(11.5),
                            app.theme.text,
                            r.width() - 8.0,
                        );
                    }
                }
                if !ui.ctx().input(|i| i.pointer.any_down()) {
                    // Released outside any block response (e.g. after scrolling): drop the drag.
                    cu.drag = None;
                }
            }
            if let Some((g, target)) = finished {
                cu.drag = None;
                if let Some(id) = commit_drag(app, &g, target) {
                    app.ui.module_selection = Some(id.0);
                }
            }
        });
    });
}

/// The Daily Task List under a day/week grid: unfinished tasks on their due days (overdue ones today).
#[allow(clippy::too_many_arguments)]
fn task_strip(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    rect: Rect,
    ruler_w: f32,
    col_w: f32,
    days: &[i64],
    tasks: &[(i64, ItemId)],
    minimized: bool,
    key: usize,
) {
    let p = ui.painter().clone();
    let line = Stroke::new(1.0, app.theme.separator);
    p.rect_filled(rect, CornerRadius::ZERO, blend(app.theme.reading, app.theme.backdrop, 0.35));
    p.hline(rect.x_range(), rect.min.y, line);
    p.text(
        pos2(rect.min.x + 6.0, rect.min.y + TASK_HEAD_H / 2.0),
        Align2::LEFT_CENTER,
        "Tasks",
        FontId::proportional(11.5),
        app.theme.text_muted,
    );
    for (i, &d) in days.iter().enumerate() {
        let col = Rect::from_min_max(
            pos2(rect.min.x + ruler_w + col_w * i as f32, rect.min.y),
            pos2(rect.min.x + ruler_w + col_w * (i + 1) as f32, rect.max.y),
        );
        p.vline(col.min.x, col.y_range(), line);
        let mine: Vec<ItemId> = tasks.iter().filter(|(x, _)| *x == d).map(|(_, id)| *id).collect();
        if minimized {
            if !mine.is_empty() {
                let text = format!("{} task{}", mine.len(), if mine.len() == 1 { "" } else { "s" });
                p.text(
                    pos2(col.min.x + 6.0, col.min.y + TASK_HEAD_H / 2.0),
                    Align2::LEFT_CENTER,
                    text,
                    FontId::proportional(11.5),
                    app.theme.text,
                );
            }
            continue;
        }
        let shown = if mine.len() > TASK_ROWS { TASK_ROWS - 1 } else { mine.len() };
        for (j, id) in mine.iter().take(shown).enumerate() {
            let Some(t) = app.s.mailbox.tasks.get(id).cloned() else { continue };
            let row = Rect::from_min_size(
                pos2(col.min.x + 2.0, col.min.y + TASK_HEAD_H + TASK_ROW_H * j as f32),
                vec2(col_w - 4.0, TASK_ROW_H - 2.0),
            );
            let subject =
                if t.subject.trim().is_empty() { "(No subject)".to_owned() } else { t.subject.clone() };
            let resp = ui.interact(row, Id::new(("mc-cal-task", key, d, id.0)), Sense::click());
            a11y(&resp, WidgetType::Button, &format!("Task {subject}"));
            if resp.hovered() {
                p.rect_filled(row, CornerRadius::same(3), app.theme.hover);
            }
            let check = Rect::from_min_size(pos2(row.min.x + 3.0, row.center().y - 6.0), Vec2::splat(12.0));
            let cresp = ui.interact(check, Id::new(("mc-cal-task-done", key, d, id.0)), Sense::click());
            a11y(&cresp, WidgetType::Checkbox, &format!("Mark {subject} complete"));
            p.rect_stroke(
                check,
                CornerRadius::same(2),
                Stroke::new(1.0, app.theme.text_muted),
                StrokeKind::Inside,
            );
            paint_text_clipped(
                ui,
                pos2(check.max.x + 5.0, row.min.y + 2.0),
                &subject,
                FontId::proportional(12.0),
                app.theme.text,
                row.width() - 22.0,
            );
            if cresp.clicked() {
                app.run("tasks.markComplete", json!({ "ids": [id.0] }));
            } else if resp.double_clicked() {
                editors::open_task(app, st, *id);
            }
        }
        if mine.len() > shown {
            p.text(
                pos2(col.min.x + 8.0, col.min.y + TASK_HEAD_H + TASK_ROW_H * shown as f32 + TASK_ROW_H / 2.0),
                Align2::LEFT_CENTER,
                format!("+{} more", mine.len() - shown),
                FontId::proportional(11.5),
                app.theme.link,
            );
        }
    }
}

/// A timed item's line in Month view: its start time (and end time, with View Settings › Show end times in
/// Month view) and its subject.
fn month_chip_text(o: &Occ, end_time: bool) -> String {
    let start = clock(o.start.rem_euclid(DAY) / 60);
    if end_time {
        format!("{start} - {} {}", clock(o.end.rem_euclid(DAY) / 60), o.label())
    } else {
        format!("{start} {}", o.label())
    }
}

// ---------------------------------------------------------------------------------------------------------
// Schedule view
// ---------------------------------------------------------------------------------------------------------

/// Width of one time slot in Schedule View (the Time Scale sets how long a slot is).
const SCHEDULE_SLOT_W: f32 = 40.0;

/// Schedule View's time axis: the minutes shown (all day, or the working hours with Working Hours on) and
/// the width of a minute (one [`SCHEDULE_SLOT_W`] slot per Time Scale step).
#[derive(Clone, Copy, Debug, PartialEq)]
struct ScheduleAxis {
    from: i64,
    to: i64,
    slot: i64,
    px_per_min: f32,
}

impl ScheduleAxis {
    fn of(v: &CalendarViewState, work: (i64, i64)) -> ScheduleAxis {
        let (from, to) = if v.working_hours && work.0 < work.1 {
            (work.0.clamp(0, 24 * 60), work.1.clamp(0, 24 * 60))
        } else {
            (0, 24 * 60)
        };
        let (from, to) = if to > from { (from, to) } else { (0, 24 * 60) };
        let slot = i64::from(v.time_scale.clamp(5, 60));
        ScheduleAxis { from, to, slot, px_per_min: SCHEDULE_SLOT_W / slot as f32 }
    }

    fn width(&self) -> f32 {
        (self.to - self.from) as f32 * self.px_per_min
    }

    /// X offset of minute `m` of the day (clamped to the axis).
    fn x(&self, m: f32) -> f32 {
        (m.clamp(self.from as f32, self.to as f32) - self.from as f32) * self.px_per_min
    }

    /// The slot start (minute of the day) at x offset `dx`.
    fn minute_at(&self, dx: f32) -> i64 {
        let m = self.from + (dx.max(0.0) / self.px_per_min) as i64;
        let m = (m.div_euclid(self.slot)) * self.slot;
        m.clamp(self.from, (self.to - self.slot).max(self.from))
    }

    /// Minutes of the day where a column line falls (each slot), and whether it starts an hour.
    fn lines(&self) -> Vec<(i64, bool)> {
        let first = (self.from + self.slot - 1).div_euclid(self.slot) * self.slot;
        (0..)
            .map(|i| first + i * self.slot)
            .take_while(|m| *m <= self.to)
            .take(24 * 60 / 5 + 1)
            .map(|m| (m, m % 60 == 0))
            .collect()
    }
}

/// Most lanes one Schedule View row splits into; items that would need more are counted in "+N more".
const MAX_SCHEDULE_LANES: usize = 6;
/// Most items one Schedule View row looks at (a hostile calendar can hold thousands on one day).
const MAX_SCHEDULE_ROW_ITEMS: usize = 500;

/// Lanes for overlapping blocks in one Schedule View row: the lane of each (start, end) interval (given in
/// start order; `None` when all `max` lanes are taken at its start) and the number of lanes used.
fn lanes(spans: &[(i64, i64)], max: usize) -> (Vec<Option<usize>>, usize) {
    let max = max.max(1);
    let mut ends: Vec<i64> = Vec::new();
    let mut out = Vec::with_capacity(spans.len());
    for (s, e) in spans {
        let lane = match ends.iter().position(|end| *end <= *s) {
            Some(i) => Some(i),
            None if ends.len() < max => {
                ends.push(i64::MIN);
                Some(ends.len() - 1)
            }
            None => None,
        };
        if let Some(slot) = lane.and_then(|l| ends.get_mut(l)) {
            *slot = (*e).max(*s);
        }
        out.push(lane);
    }
    (out, ends.len().max(1))
}

/// Schedule View rows: one per shown calendar, or (Overlay) one row with every shown calendar.
fn schedule_rows(app: &App, shown: &BTreeSet<FolderId>, overlay: bool) -> Vec<(String, Vec<FolderId>)> {
    let rows: Vec<(String, FolderId)> = ordered(app, shown)
        .into_iter()
        .filter_map(|f| app.s.mailbox.folder(f).map(|x| (x.name.clone(), f)))
        .collect();
    if overlay && rows.len() > 1 {
        let names: Vec<String> = rows.iter().map(|(n, _)| n.clone()).collect();
        return vec![(names.join(", "), rows.into_iter().map(|(_, f)| f).collect())];
    }
    rows.into_iter().map(|(n, f)| (n, vec![f])).collect()
}

/// Whether a ruler label at `x`, `w` wide, shows whole inside `visible`.
fn ruler_label_fits(x: f32, w: f32, visible: egui::Rangef) -> bool {
    x >= visible.min && x + w <= visible.max
}

/// The minute Schedule view first scrolls to: the working day's start, unless the current time (`now`, when
/// the day shown is today) would then sit in the last quarter of the `view_w`-point pane or beyond it; the
/// view then opens with the current time a third of the way in.
fn schedule_start(axis: &ScheduleAxis, work: i64, now: Option<i64>, view_w: f32) -> i64 {
    let shown = if axis.px_per_min > 0.0 { (view_w / axis.px_per_min).max(0.0) as i64 } else { 0 };
    match now {
        Some(n) if shown > 0 && (n >= work + shown * 3 / 4 || n < work) => (n - shown / 3).max(axis.from),
        _ => work,
    }
}

fn schedule_view(ui: &mut Ui, app: &mut App, st: &mut ModState, look: &Look, rect: Rect) {
    let sel = selected_day(app, st);
    let label_w = 190.0_f32.min(rect.width() / 3.0);
    let axis = ScheduleAxis::of(&st.cal, look.work);
    let head_h = 30.0;
    let row_h = 46.0;
    let shown = shown_calendars(app, st);
    let rows = schedule_rows(app, &shown, st.cal.overlay);
    if rows.is_empty() {
        empty_message(ui, rect, app, "No calendar selected", "Tick a calendar under My Calendars.");
        return;
    }
    let p = ui.painter().clone();
    let line = Stroke::new(1.0, app.theme.separator);
    let faint = Stroke::new(1.0, blend(app.theme.separator, app.theme.reading, 0.6));
    // Free items show only when File › Options › Calendar asks for them.
    let mut occs = gather_in(app, &shown, sel, sel + 1, Some(&st.cal));
    if !look.schedule_free {
        occs.retain(|o| o.show_as != BusyStatus::Free);
    }
    // Calendar names (an overlaid row shows a colour bar per calendar).
    let labels = Rect::from_min_size(rect.min, vec2(label_w, rect.height()));
    p.rect_filled(labels, CornerRadius::ZERO, blend(app.theme.reading, app.theme.backdrop, 0.3));
    for (k, (name, folders)) in rows.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(labels.min.x, labels.min.y + head_h + row_h * k as f32),
            vec2(label_w, row_h),
        );
        let bar_h = ((row_h - 12.0) / folders.len().max(1) as f32).max(2.0);
        for (i, folder) in folders.iter().enumerate() {
            p.rect_filled(
                Rect::from_min_size(pos2(r.min.x + 4.0, r.min.y + 6.0 + bar_h * i as f32), vec2(4.0, bar_h)),
                CornerRadius::same(2),
                calendar_color32(app, *folder),
            );
        }
        paint_text_clipped(
            ui,
            pos2(r.min.x + 14.0, r.center().y - 8.0),
            name,
            FontId::proportional(13.0),
            app.theme.text,
            label_w - 20.0,
        );
        p.hline(labels.x_range(), r.max.y, line);
    }
    let timeline = Rect::from_min_max(pos2(rect.min.x + label_w + 1.0, rect.min.y), rect.max);
    let key = format!("schedule|{}|{}|{}|{}", st.cal.date, axis.slot, axis.from, axis.to);
    // The scroll bar always shows: the timeline is wider than the pane, and a bar that appears only on hover
    // left no hint that the rest of the day is there.
    let mut area = ScrollArea::horizontal()
        .id_salt("mc-cal-schedule")
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);
    let now = today(app);
    let now_min = now_minute(app);
    if st.cal_scroll_key != key {
        // Opens on the working day (all day: from 8 AM), or on the current time today when that would sit at
        // or past the pane's edge.
        let work = if axis.from > 0 { axis.from } else { look.work.0.clamp(0, 23 * 60).min(8 * 60) };
        let today_now = (sel == now).then_some(now_min);
        let start = schedule_start(&axis, work, today_now, timeline.width());
        area = area.horizontal_scroll_offset(axis.x(start as f32));
        st.cal_scroll_key = key;
    }
    child(ui, timeline, "mc-cal-schedule-area", |ui| {
        area.show(ui, |ui| {
            let total_w = axis.width();
            let height = head_h + row_h * rows.len() as f32;
            let (area_rect, resp) =
                ui.allocate_exact_size(vec2(total_w, height.max(timeline.height() - 14.0)), Sense::click());
            let p = ui.painter().clone();
            let body_h = row_h * rows.len() as f32;
            // Working time shading, slot by slot.
            let working_day = look.work_days.contains(&weekday(sel));
            for (m, _) in axis.lines() {
                if m >= axis.to {
                    break;
                }
                let x = area_rect.min.x + axis.x(m as f32);
                let w = (axis.slot as f32 * axis.px_per_min).min(area_rect.max.x - x).max(0.0);
                let work = working_day && (look.work.0..look.work.1).contains(&(m + axis.slot / 2));
                p.rect_filled(
                    Rect::from_min_size(pos2(x, area_rect.min.y + head_h), vec2(w, body_h)),
                    CornerRadius::ZERO,
                    if work { app.theme.reading } else { blend(app.theme.reading, app.theme.backdrop, 0.3) },
                );
            }
            // Column lines: each slot (faint), each hour with its label; slots of a fine scale get their
            // minutes when there is room.
            for (m, hour) in axis.lines() {
                let x = area_rect.min.x + axis.x(m as f32);
                if hour {
                    p.vline(x, area_rect.y_range(), line);
                } else {
                    p.vline(x, (area_rect.min.y + head_h * 0.6)..=area_rect.max.y, faint);
                }
                if m >= axis.to {
                    continue;
                }
                let text = if hour {
                    hour_label(m / 60)
                } else if axis.slot as f32 * axis.px_per_min >= 36.0 {
                    format!(":{:02}", m % 60)
                } else {
                    continue;
                };
                // A label the pane's edge would cut ("M :30" for a scrolled-off "6 AM") is left out.
                let font = FontId::proportional(if hour { 11.5 } else { 10.0 });
                let galley = p.layout_no_wrap(text, font, app.theme.text_muted);
                let at = pos2(x + 6.0, area_rect.min.y + head_h / 2.0 - galley.size().y / 2.0);
                if ruler_label_fits(at.x, galley.size().x, ui.clip_rect().x_range()) {
                    p.galley(at, galley, app.theme.text_muted);
                }
            }
            for k in 0..=rows.len() {
                p.hline(area_rect.x_range(), area_rect.min.y + head_h + row_h * k as f32, line);
            }
            if (resp.clicked() || resp.double_clicked())
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let k = ((pos.y - area_rect.min.y - head_h) / row_h).floor();
                let minute = axis.minute_at(pos.x - area_rect.min.x);
                if k >= 0.0
                    && let Some((_, folders)) = rows.get(k as usize)
                {
                    st.cal_slot = Some((sel, minute));
                    app.ui.module_selection = None;
                    if resp.double_clicked() {
                        // An overlaid row adds to the calendar in use.
                        let target = if folders.len() == 1 { folders.first().copied() } else { None };
                        let f = writable(app, target);
                        forms::new_at(app, st, f, sel, minute, false);
                    }
                }
            }
            let (ds, de) = (sel * DAY, (sel + 1) * DAY);
            let (vs, ve) = (ds + axis.from * 60, ds + axis.to * 60);
            for (k, (_, folders)) in rows.iter().enumerate() {
                let y = area_rect.min.y + head_h + row_h * k as f32;
                // Items inside the time shown, in start order, in lanes where they overlap.
                let mut row: Vec<&Occ> = occs
                    .iter()
                    .filter(|o| folders.contains(&o.folder) && o.end.min(de) > vs && o.start.max(ds) < ve)
                    .collect();
                row.sort_by_key(|o| (o.start, o.end));
                let mut hidden = row.len().saturating_sub(MAX_SCHEDULE_ROW_ITEMS);
                row.truncate(MAX_SCHEDULE_ROW_ITEMS);
                let spans: Vec<(i64, i64)> = row
                    .iter()
                    .map(|o| {
                        let s = o.start.max(ds);
                        (s, o.end.min(de).max(s + 900))
                    })
                    .collect();
                let (lane_of, count) = lanes(&spans, MAX_SCHEDULE_LANES);
                let lane_h = (row_h - 8.0) / count as f32;
                // Where the first item without a lane starts ("+N more" goes there).
                let mut more_at: Option<i64> = None;
                for ((o, (s, e)), lane) in row.iter().zip(&spans).zip(&lane_of) {
                    let Some(lane) = lane else {
                        hidden = hidden.saturating_add(1);
                        more_at = Some(more_at.map_or(*s, |m| m.min(*s)));
                        continue;
                    };
                    let x0 = area_rect.min.x + axis.x((s - ds) as f32 / 60.0);
                    let x1 = area_rect.min.x + axis.x((e - ds) as f32 / 60.0);
                    let top = y + 4.0 + lane_h * *lane as f32;
                    let r = Rect::from_min_max(
                        pos2(x0 + 1.0, top),
                        pos2((x1 - 1.0).max(x0 + 3.0), top + lane_h - 1.0),
                    );
                    let mut resp =
                        ui.interact(r, Id::new(("mc-cal-sched", o.id.0, o.start, k)), Sense::click());
                    a11y(&resp, WidgetType::Button, &occ_a11y(o));
                    if r.width() < BLOCK_TEXT_MIN_W + 10.0 {
                        resp = resp.on_hover_text(occ_a11y(o));
                    }
                    let selected = app.ui.module_selection == Some(o.id.0);
                    paint_block(ui, app, r, o, selected, false);
                    occ_interact(app, st, &resp, o);
                }
                if hidden > 0 {
                    // Too many overlapping items for the row: the rest are counted, and the count opens the
                    // day in Day view.
                    let at = more_at.unwrap_or(vs).clamp(vs, ve);
                    let x = area_rect.min.x + axis.x((at - ds) as f32 / 60.0);
                    let text = format!("+{hidden} more");
                    let more = Rect::from_min_size(pos2(x + 2.0, y + row_h - 18.0), vec2(80.0, 16.0));
                    let resp = ui.interact(more, Id::new(("mc-cal-sched-more", k, sel)), Sense::click());
                    a11y(&resp, WidgetType::Button, &text);
                    p.rect_filled(more, CornerRadius::same(3), app.theme.reading);
                    p.text(
                        pos2(more.min.x + 4.0, more.center().y),
                        Align2::LEFT_CENTER,
                        &text,
                        FontId::proportional(11.5),
                        app.theme.link,
                    );
                    if resp.clicked() {
                        run_view_cmd(
                            app,
                            st,
                            "calendar.goToDate",
                            json!({ "date": day_string(sel), "mode": "day" }),
                        );
                    }
                }
            }
            if sel == now && (axis.from..axis.to).contains(&now_min) {
                let x = area_rect.min.x + axis.x(now_min as f32);
                p.vline(x, area_rect.y_range(), Stroke::new(2.0, app.theme.danger));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::Recurrence;

    /// Release check (Schedule view): at 1024×768 the view opened on 8 AM with the current time (2:30 PM)
    /// right on the pane's edge; and a scrolled-off hour label showed cut ("M :30").
    #[test]
    fn schedule_opens_with_now_in_view_and_skips_cut_labels() {
        let axis = ScheduleAxis::of(&CalendarViewState::default(), (8 * 60, 17 * 60));
        let px = axis.px_per_min;
        let narrow = 500.0; // about 6 hours at the default scale
        let shown = (narrow / px) as i64;
        // Not today, or now early in the view: the working day's start.
        assert_eq!(schedule_start(&axis, 8 * 60, None, narrow), 8 * 60);
        assert_eq!(schedule_start(&axis, 8 * 60, Some(9 * 60), narrow), 8 * 60);
        // 2:30 PM in a 6-hour-wide pane: now a third of the way in, well away from the edge.
        let start = schedule_start(&axis, 8 * 60, Some(14 * 60 + 30), narrow);
        let now_x = axis.x((14 * 60 + 30) as f32) - axis.x(start as f32);
        assert!(now_x > narrow * 0.2 && now_x < narrow * 0.75, "now at {now_x} of {narrow}");
        assert!(start >= axis.from && start + shown > 14 * 60 + 30);
        // Before the working day starts: still in view.
        let early = schedule_start(&axis, 8 * 60, Some(6 * 60), narrow);
        assert!(early <= 6 * 60);
        // A zero-width pane does not divide by anything odd.
        assert_eq!(schedule_start(&axis, 8 * 60, Some(14 * 60), 0.0), 8 * 60);
        // Ruler labels: whole or not at all.
        let visible = egui::Rangef::new(490.0, 1900.0);
        assert!(!ruler_label_fits(470.0, 30.0, visible), "cut at the left edge");
        assert!(!ruler_label_fits(1880.0, 30.0, visible), "cut at the right edge");
        assert!(ruler_label_fits(552.0, 30.0, visible));
    }

    /// Release check: in the dark theme the Month view dot of an appointment on the selected day (a dark
    /// calendar colour on the selected-cell fill) was nearly invisible.
    #[test]
    fn month_dots_stay_visible_on_every_cell_fill() {
        let app = App::new(mailcraft_engine::sample::sample_session());
        let cals: Vec<FolderId> =
            app.s.mailbox.folders.values().filter(|f| f.kind == FolderKind::Calendar).map(|f| f.id).collect();
        assert!(!cals.is_empty());
        let navy = Color32::from_rgb(0x1f, 0x3a, 0x68);
        for t in [crate::theme::Theme::light(), crate::theme::Theme::dark()] {
            for base in cals.iter().map(|f| calendar_color32(&app, *f)).chain([navy]) {
                for under in [t.selected, t.reading, t.hover] {
                    let c = crate::theme::contrast(month_dot(base, under), under);
                    assert!(c >= 3.0, "dark={} {base:?} on {under:?}: {c:.2}:1", t.dark);
                }
            }
        }
    }

    #[test]
    fn schedule_axis_follows_working_hours_and_time_scale() {
        let mut v = CalendarViewState::default();
        let work = (8 * 60 + 30, 17 * 60);
        let a = ScheduleAxis::of(&v, work);
        assert_eq!((a.from, a.to, a.slot), (0, 24 * 60, 30));
        assert!((a.width() - 48.0 * SCHEDULE_SLOT_W).abs() < 0.01, "a slot per half hour");
        v.working_hours = true;
        let a = ScheduleAxis::of(&v, work);
        assert_eq!((a.from, a.to), work, "Working Hours shows the working day only");
        assert_eq!(a.x(0.0), 0.0, "times before the working day are clamped");
        assert_eq!(a.minute_at(0.0), 8 * 60 + 30);
        assert_eq!(a.minute_at(SCHEDULE_SLOT_W * 1.5), 9 * 60, "clicks snap to the slot");
        assert_eq!(a.minute_at(1e9), 16 * 60 + 30, "the last slot");
        assert_eq!(a.lines().first(), Some(&(8 * 60 + 30, false)), "the first line is the working day start");
        v.time_scale = 15;
        v.working_hours = false;
        let a = ScheduleAxis::of(&v, work);
        assert!((a.width() - 96.0 * SCHEDULE_SLOT_W).abs() < 0.01, "a 15-minute scale is twice as wide");
        assert_eq!(a.lines().iter().filter(|(_, h)| *h).count(), 25);
        // Broken working hours fall back to the whole day.
        v.working_hours = true;
        assert_eq!(ScheduleAxis::of(&v, (17 * 60, 9 * 60)).from, 0);
        v.time_scale = 0;
        assert_eq!(ScheduleAxis::of(&v, work).slot, 5);
    }

    #[test]
    fn a_view_saved_by_one_window_is_not_adopted_by_another() {
        let mut app = App::new(mailcraft_engine::sample::sample_session());
        app.s.view.module = Module::Calendar;
        let mut a = ModState::default();
        sync_mode(&mut app, &mut a);
        let mut b = a.clone();
        let mut cu_b = CalUi::default();
        adopt_saved(&mut app, &mut b, &mut cu_b);
        // Window A changes its arrangement; window B (sharing the rest of the state) keeps Month.
        run_view_cmd(&mut app, &mut a, "calendar.arrange", json!({"mode": "week"}));
        assert_eq!(a.cal.mode, CalendarMode::Week);
        b.cal_ui_seq = a.cal_ui_seq.clone();
        adopt_saved(&mut app, &mut b, &mut cu_b);
        assert_eq!(b.cal.mode, CalendarMode::Month, "another window's view is not mirrored");
        // A change from outside the windows (a command run without a view) reaches the window.
        app.s.run("calendar.arrange", &json!({"mode": "day"})).unwrap();
        adopt_saved(&mut app, &mut b, &mut cu_b);
        assert_eq!(b.cal.mode, CalendarMode::Day);
    }

    /// View Settings › Filter and Conditional Formatting apply to the day, week, month and schedule grids,
    /// not only to the List views.
    #[test]
    fn grid_views_follow_the_view_filter_and_conditional_formatting() {
        use mailcraft_model::view::{ItemFilter, ItemRule};
        let mut app = App::new(mailcraft_engine::sample::sample_session());
        let cal = app.s.mailbox.appointments.values().next().map(|a| a.folder).unwrap();
        for subject in ["Budget review", "Lunch"] {
            app.s
                .run(
                    "calendar.newAppointment",
                    &json!({"folder": cal.0, "subject": subject, "start": "2026-10-14T12:00", "end": "2026-10-14T13:00"}),
                )
                .unwrap();
        }
        let cals: BTreeSet<FolderId> = [cal].into_iter().collect();
        let (from, to) = (day("2026-10-14"), day("2026-10-15"));
        let all = gather(&app, &cals, from, to);
        assert!(all.iter().any(|o| o.subject == "Lunch") && all.iter().any(|o| o.subject == "Budget review"));
        let mut v = CalendarViewState {
            filter: ItemFilter { words: "budget".into(), ..Default::default() },
            ..Default::default()
        };
        let shown = gather_in(&app, &cals, from, to, Some(&v));
        assert!(
            shown.iter().all(|o| o.subject.to_lowercase().contains("budget")),
            "the filter leaves Lunch out"
        );
        assert!(shown.iter().any(|o| o.subject == "Budget review"));
        v.filter = ItemFilter::default();
        v.rules = vec![ItemRule {
            name: "Lunch".into(),
            on: true,
            filter: ItemFilter { words: "lunch".into(), ..Default::default() },
            bold: true,
            strike: true,
            color: "#ff0000".into(),
            ..Default::default()
        }];
        let styled = gather_in(&app, &cals, from, to, Some(&v));
        let lunch = styled.iter().find(|o| o.subject == "Lunch").unwrap();
        assert!(lunch.bold && lunch.strike && !lunch.italic);
        assert_eq!(lunch.base, Color32::from_rgb(0xff, 0, 0));
        let budget = styled.iter().find(|o| o.subject == "Budget review").unwrap();
        assert!(!budget.bold);
        assert_ne!(budget.base, Color32::from_rgb(0xff, 0, 0));
        // The grids draw the styled items (every arrangement, in a small window too).
        let ctx = egui::Context::default();
        for mode in ["day", "week", "month", "schedule"] {
            let mut st = load(&ctx);
            st.cal = v.clone();
            store(&ctx, st);
            app.s.run("calendar.arrange", &json!({"mode": mode})).unwrap();
            app.s.view.module = Module::Calendar;
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| show(ui, &mut app));
            out.textures_delta.clear();
        }
    }

    #[test]
    fn overlapping_schedule_items_get_their_own_lanes() {
        let l = |spans: &[(i64, i64)]| {
            let (lanes, n) = lanes(spans, MAX_SCHEDULE_LANES);
            (lanes.into_iter().map(|x| x.unwrap()).collect::<Vec<_>>(), n)
        };
        assert_eq!(l(&[]), (vec![], 1));
        assert_eq!(l(&[(0, 10), (10, 20)]), (vec![0, 0], 1), "touching items share a lane");
        assert_eq!(l(&[(0, 10), (5, 15), (12, 20)]), (vec![0, 1, 0], 2));
        assert_eq!(l(&[(0, 100), (10, 20), (15, 30), (40, 50)]), (vec![0, 1, 2, 1], 3));
    }

    /// Regression: thousands of overlapping items (a hostile subscribed calendar) used to make as many lanes,
    /// each a fraction of a pixel high, at quadratic cost every frame. Lanes are capped; the rest are counted.
    #[test]
    fn schedule_lanes_are_capped() {
        let spans: Vec<(i64, i64)> = (0..5_000).map(|i| (i, 100_000)).collect();
        let (lane_of, n) = lanes(&spans, MAX_SCHEDULE_LANES);
        assert_eq!(n, MAX_SCHEDULE_LANES);
        assert_eq!(lane_of.iter().filter(|x| x.is_some()).count(), MAX_SCHEDULE_LANES);
        assert_eq!(lane_of.iter().filter(|x| x.is_none()).count(), 5_000 - MAX_SCHEDULE_LANES);
        // A freed lane is used again by a later item.
        let (lane_of, n) = lanes(&[(0, 10), (0, 10), (0, 10), (10, 20)], 2);
        assert_eq!((lane_of, n), (vec![Some(0), Some(1), None, Some(0)], 2));
        assert_eq!(lanes(&[(0, 10), (5, 10)], 0).0, vec![Some(0), None], "at least one lane");
    }

    fn appt(start: &str, end: &str, rrule: Option<&str>) -> Appointment {
        let s = mailcraft_engine::cmd::calendar::pim::parse_local(start, 0).unwrap();
        let e = mailcraft_engine::cmd::calendar::pim::parse_local(end, 0).unwrap();
        Appointment {
            start: s,
            end: e,
            recurrence: rrule.map(|r| Recurrence { rrule: r.into(), exdates: Vec::new() }),
            ..Default::default()
        }
    }

    fn day(s: &str) -> i64 {
        parse_day(s).unwrap()
    }

    const MON_FRI: [u8; 5] = [1, 2, 3, 4, 5];

    #[test]
    fn single_appointment_overlap() {
        let a = appt("2026-10-09T09:00", "2026-10-09T10:00", None);
        assert_eq!(expand(&a, 0, day("2026-10-09"), day("2026-10-10")).len(), 1);
        assert!(expand(&a, 0, day("2026-10-10"), day("2026-10-11")).is_empty());
    }

    #[test]
    fn weekly_weekdays() {
        let a = appt("2026-10-05T09:00", "2026-10-05T09:15", Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"));
        let occ = expand(&a, 0, day("2026-10-04"), day("2026-10-11"));
        assert_eq!(occ.len(), 5);
        // Nothing before the start.
        assert!(expand(&a, 0, day("2026-09-01"), day("2026-10-01")).is_empty());
    }

    #[test]
    fn count_until_interval_monthly() {
        let a = appt("2026-10-01T09:00", "2026-10-01T10:00", Some("FREQ=DAILY;COUNT=3"));
        assert_eq!(expand(&a, 0, day("2026-10-01"), day("2026-11-01")).len(), 3);
        // UNTIL is an instant: 00:00 on the 5th ends the series before that day's 09:00 (RFC 5545).
        let b = appt("2026-10-01T09:00", "2026-10-01T10:00", Some("FREQ=DAILY;UNTIL=20261005T000000Z"));
        assert_eq!(expand(&b, 0, day("2026-10-01"), day("2026-11-01")).len(), 4);
        let b = appt("2026-10-01T09:00", "2026-10-01T10:00", Some("FREQ=DAILY;UNTIL=20261005T235959Z"));
        assert_eq!(expand(&b, 0, day("2026-10-01"), day("2026-11-01")).len(), 5);
        let c = appt("2026-10-01T09:00", "2026-10-01T10:00", Some("FREQ=DAILY;INTERVAL=2"));
        assert_eq!(expand(&c, 0, day("2026-10-01"), day("2026-10-08")).len(), 4);
        let d = appt("2026-01-31T09:00", "2026-01-31T10:00", Some("FREQ=MONTHLY"));
        // Months without a 31st are skipped.
        assert_eq!(expand(&d, 0, day("2026-01-01"), day("2026-06-01")).len(), 3);
        let e = appt("2026-10-13T09:00", "2026-10-13T10:00", Some("FREQ=MONTHLY;BYDAY=2TU"));
        let occ = expand(&e, 0, day("2026-11-01"), day("2026-12-01"));
        assert_eq!(occ.len(), 1);
        assert_eq!(occ[0].0.div_euclid(DAY), day("2026-11-10"));
        let f = appt("2026-10-09T09:00", "2026-10-09T10:00", Some("FREQ=YEARLY"));
        assert_eq!(expand(&f, 0, day("2027-01-01"), day("2028-01-01")).len(), 1);
    }

    #[test]
    fn junk_rules_never_panic() {
        for r in [
            "",
            "FREQ=",
            "FREQ=HOURLY",
            "BYDAY=XX;FREQ=WEEKLY",
            "FREQ=WEEKLY;INTERVAL=0",
            "FREQ=MONTHLY;BYMONTHDAY=-40",
            "FREQ=DAILY;COUNT=-1;UNTIL=x",
        ] {
            let a = appt("2026-10-09T09:00", "2026-10-09T10:00", Some(r));
            let _ = expand(&a, 0, day("2026-10-01"), day("2026-12-01"));
        }
        let a = appt("2026-10-09T09:00", "2026-10-09T10:00", Some("FREQ=DAILY"));
        assert!(expand(&a, 0, i64::MAX - 10, i64::MAX).len() <= 1000);
        assert!(expand(&a, 0, i64::MIN, i64::MIN + 10).is_empty());
    }

    #[test]
    fn columns_for_overlaps() {
        let c = lay_out_columns(&[(0, 60), (30, 90), (100, 120)]);
        assert_eq!(c, vec![(0, 2), (1, 2), (0, 1)]);
        assert!(lay_out_columns(&[]).is_empty());
    }

    #[test]
    fn titles_and_steps() {
        let mut st = ModState::default();
        let sel = day("2026-10-09");
        st.cal.mode = CalendarMode::Month;
        assert_eq!(title(&st, sel, &MON_FRI, 0), "October 2026");
        assert_eq!(day_string(step(&st, sel, 1)), "2026-11-09");
        st.cal.mode = CalendarMode::Week;
        assert_eq!(title(&st, sel, &MON_FRI, 0), "October 4 – 10, 2026");
        // First day of week Monday (File › Options › Calendar).
        assert_eq!(title(&st, sel, &MON_FRI, 1), "October 5 – 11, 2026");
        st.cal.mode = CalendarMode::WorkWeek;
        assert_eq!(visible_days(&st, sel, &MON_FRI, 0).len(), 5);
        st.cal.mode = CalendarMode::Day;
        assert_eq!(title(&st, sel, &MON_FRI, 0), "Friday, October 9, 2026");
        assert_eq!(month_grid(sel, 0), (day("2026-09-27"), 5));
        assert_eq!(month_grid(sel, 1), (day("2026-09-28"), 5));
        // A month starting on the first day of the week starts the grid with it.
        assert_eq!(month_grid(day("2026-11-15"), 0), (day("2026-11-01"), 5));
        assert_eq!(month_grid(day("2026-11-15"), 1), (day("2026-10-26"), 6));
    }

    #[test]
    fn work_week_follows_the_working_days() {
        let mut st = ModState::default();
        st.cal.mode = CalendarMode::WorkWeek;
        let sel = day("2026-10-09");
        // Sunday to Thursday.
        let days = visible_days(&st, sel, &[0, 1, 2, 3, 4], 0);
        assert_eq!(days.first().copied(), Some(day("2026-10-04")));
        assert_eq!(days.len(), 5);
        assert_eq!(title(&st, sel, &[0, 1, 2, 3, 4], 0), "October 4 – 8, 2026");
        // Weeks from Monday: the working days in that week's order (Sunday comes last).
        let days = visible_days(&st, sel, &[0, 1, 2, 3, 4], 1);
        assert_eq!(days.first().copied(), Some(day("2026-10-05")));
        assert_eq!(days.last().copied(), Some(day("2026-10-11")));
        // Junk days fall back to Monday to Friday.
        assert_eq!(visible_days(&st, sel, &[9], 0).len(), 5);
        assert_eq!(visible_days(&st, sel, &[9], 3).len(), 5);
    }

    #[test]
    fn default_calendar_colors() {
        assert_eq!(hex_color("#3b78c2"), Some(Color32::from_rgb(0x3b, 0x78, 0xc2)));
        assert_eq!(hex_color(" #FFFFFF "), Some(Color32::WHITE));
        for bad in ["", "#", "#12345", "#1234567", "123456", "#ééé", "#gg0000"] {
            assert_eq!(hex_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn drag_targets_snap_and_keep_length() {
        let d0 = day("2026-10-09");
        let g = Drag {
            id: ItemId(1),
            occurrence: None,
            kind: DragKind::Move,
            start: d0 * DAY + 9 * 3600,
            end: d0 * DAY + 10 * 3600,
            grab_min: 10,
            pane: 0,
        };
        // Grabbed 10 minutes in, dropped at 13:22 next day → starts 13:15 with a 15-minute scale.
        assert_eq!(
            drag_target(&g, (d0 + 1, 13 * 60 + 22), 15, (0, 1440)),
            ((d0 + 1) * DAY + 13 * 3600 + 900, (d0 + 1) * DAY + 14 * 3600 + 900)
        );
        let top = Drag { kind: DragKind::Top, ..g.clone() };
        assert_eq!(drag_target(&top, (d0, 8 * 60 + 2), 30, (0, 1440)).0, d0 * DAY + 8 * 3600);
        // The top cannot pass the end.
        assert_eq!(drag_target(&top, (d0, 11 * 60), 30, (0, 1440)).0, d0 * DAY + 10 * 3600 - 1800);
        let bottom = Drag { kind: DragKind::Bottom, ..g };
        assert_eq!(drag_target(&bottom, (d0, 11 * 60 + 40), 30, (0, 1440)).1, d0 * DAY + 11 * 3600 + 1800);
        assert_eq!(drag_target(&bottom, (d0, 6 * 60), 30, (0, 1440)).1, d0 * DAY + 9 * 3600 + 1800);
    }

    #[test]
    fn own_windows_for_commands_without_details() {
        assert_eq!(own_window("calendar.changeTimeZone", &json!({})), Some(windows::TZ_DIALOG));
        assert_eq!(own_window("calendar.share", &json!({})), Some(windows::SHARE_DIALOG));
        assert_eq!(own_window("calendar.share", &json!({"range": "today"})), None);
        assert_eq!(own_window("calendar.changeTimeZone", &json!({"zone": "UTC"})), None);
        assert_eq!(
            own_window("calendar.addCalendar", &json!({"source": "internet"})),
            Some(windows::INTERNET_DIALOG)
        );
        assert_eq!(
            own_window("calendar.addCalendar", &json!({"source": "internet", "url": "https://x"})),
            None
        );
        assert_eq!(
            own_window("calendar.addCalendar", &json!({"source": "blank"})),
            Some(windows::BLANK_DIALOG)
        );
        assert_eq!(
            own_window("calendar.addCalendar", &json!({"source": "file"})),
            Some(windows::FILE_DIALOG)
        );
        assert_eq!(own_window("calendar.addCalendar", &json!({"source": "shared"})), None);
        assert_eq!(ruler_label(-60), "11 PM");
        assert_eq!(ruler_label(9 * 60 + 30), "9:30 AM");
        assert_eq!(preview_text("  a\n\nb   c "), "a b c");
    }
}
