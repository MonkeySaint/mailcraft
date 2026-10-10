//! Calendar module: date navigator + My Calendars (folder pane), folder bar (Today, previous/next, title,
//! view switcher) and the Day / Work Week / Week / Month / Schedule views.
//!
//! The view state is the engine's `CalendarViewState`, kept in [`ModState`] and changed through the
//! `calendar.*` view commands (they take the current state and return the new one). `app.ui.calendar_view`
//! mirrors the mode so the ribbon can show and switch it.

use std::collections::BTreeSet;

use egui::{
    Align, Align2, Color32, ComboBox, CornerRadius, FontId, Id, Key, Layout, Modifiers, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetType, pos2, vec2,
};
use serde_json::{Value, json};

use mailcraft_engine::cmd::calendar::pim::Kind;
use mailcraft_engine::cmd::calendar::{CalendarMode, CalendarViewState, occurrences};
use mailcraft_model::{Appointment, BusyStatus, DateTime, FolderId, FolderKind, ItemId, dates};

use super::common::{
    DAY, ModState, a11y, add_months, blend, category_color, child, clock, current_folder, day_of, day_string,
    empty_message, folder_rows, group_header, hour_label, load, local_secs, long_date, month_name,
    now_minute, paint_text_clipped, pane_bg, parse_day, select_folder, shift_month, split_folder_pane, store,
    today, tz, week_start, weekday, weekday_name, ymd,
};
use super::editors;
use crate::App;

/// Folder bar height (Today, arrows, title, view switcher).
const BAR_H: f32 = 48.0;
/// Height of one time slot in day/week views.
const SLOT_H: f32 = 20.0;
/// Width of the time ruler.
const RULER_W: f32 = 56.0;
const WORK_START: i64 = 8 * 60;
const WORK_END: i64 = 17 * 60;
/// Height of one all-day banner lane.
const LANE_H: f32 = 22.0;

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

/// Runs `id` (adding the current view state for view commands) and keeps a returned view state.
pub(super) fn run_view_cmd(app: &mut App, st: &mut ModState, id: &str, params: Value) -> Option<Value> {
    let mut params = if params.is_object() { params } else { json!({}) };
    if VIEW_COMMANDS.contains(&id)
        && let Some(o) = params.as_object_mut()
    {
        o.insert("view".into(), serde_json::to_value(&st.cal).unwrap_or(Value::Null));
    }
    let out = app.run(id, params)?;
    if let Some(v) = out.get("view").and_then(|v| serde_json::from_value::<CalendarViewState>(v.clone()).ok())
    {
        st.cal = v;
        app.ui.calendar_view = st.cal.mode.name().to_owned();
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

/// Days shown by the current view (Month: the whole 5–6 week grid).
fn visible_days(st: &ModState, sel: i64) -> Vec<i64> {
    match st.cal.mode {
        CalendarMode::Day => (0..i64::from(st.cal.days.clamp(1, 14))).map(|i| sel + i).collect(),
        CalendarMode::WorkWeek => (1..=5).map(|i| week_start(sel) + i).collect(),
        CalendarMode::Week => (0..7).map(|i| week_start(sel) + i).collect(),
        CalendarMode::Month => {
            let (start, weeks) = month_grid(sel);
            (0..weeks * 7).map(|i| start + i).collect()
        }
        CalendarMode::Schedule => vec![sel],
    }
}

/// First cell and number of weeks of the month grid containing `day`.
fn month_grid(day: i64) -> (i64, i64) {
    let (y, m, _) = ymd(day);
    let first = day_of(y, m, 1).unwrap_or(day);
    let start = week_start(first);
    let dim = i64::from(dates::days_in_month(y, m));
    let weeks = ((first - start) + dim + 6) / 7;
    (start, weeks.max(4))
}

/// Folder-bar title for the view.
fn title(st: &ModState, sel: i64) -> String {
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
            let days = visible_days(st, sel);
            match (days.first(), days.last()) {
                (Some(a), Some(b)) => span(*a, *b),
                _ => long_date(sel),
            }
        }
    }
}

fn mode_label(m: CalendarMode) -> &'static str {
    match m {
        CalendarMode::Day => "Day",
        CalendarMode::WorkWeek => "Work Week",
        CalendarMode::Week => "Week",
        CalendarMode::Month => "Month",
        CalendarMode::Schedule => "Schedule View",
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
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub subject: String,
    pub location: String,
    pub base: Color32,
    pub show_as: BusyStatus,
    pub private: bool,
    pub recurring: bool,
    pub meeting: bool,
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

/// Occurrence spans (local seconds) of `a` overlapping days `[from, to)`, expanded by the engine
/// (`calendar::occurrences`, i.e. the formats crate's RFC 5545 expansion with EXDATEs).
pub(super) fn expand(a: &Appointment, tz: i32, from: i64, to: i64) -> Vec<(i64, i64)> {
    let shift = i64::from(tz).saturating_mul(60);
    let at = |day: i64| DateTime::from_unix(day.saturating_mul(DAY).saturating_sub(shift), tz);
    if to <= from {
        return Vec::new();
    }
    occurrences(a, at(from), at(to), 1000)
        .into_iter()
        .map(|(s, e)| {
            let ls = local_secs(s, tz);
            (ls, local_secs(e, tz).max(ls))
        })
        .collect()
}

/// Occurrences of appointments in `calendars` over days `[from, to)`, sorted by start.
pub(super) fn gather(app: &App, calendars: &BTreeSet<FolderId>, from: i64, to: i64) -> Vec<Occ> {
    let tz = tz(app);
    let mut out = Vec::new();
    for a in app.s.mailbox.appointments.values() {
        if !calendars.contains(&a.folder) {
            continue;
        }
        let base = category_color(app, &a.categories).unwrap_or(app.theme.accent);
        for (start, end) in expand(a, tz, from, to) {
            out.push(Occ {
                id: a.id,
                folder: a.folder,
                start,
                end,
                all_day: a.all_day,
                subject: a.subject.clone(),
                location: a.location.clone(),
                base,
                show_as: a.show_as,
                private: a.private,
                recurring: a.recurrence.is_some(),
                meeting: a.is_meeting,
            });
        }
    }
    out.sort_by_key(|o| (o.start, std::cmp::Reverse(o.end), o.id));
    out
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
    let occ_unix = o.start.saturating_sub(i64::from(tz(app)).saturating_mul(60));
    if resp.double_clicked() {
        editors::open_occurrence(app, st, id, occ_unix);
    }
    let recurring = o.recurring;
    resp.context_menu(|ui| {
        if ui.button("Open").clicked() {
            editors::open_occurrence(app, st, id, occ_unix);
            ui.close();
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

/// Paints an occurrence block (time grid / schedule).
fn paint_block(ui: &Ui, app: &mut App, rect: Rect, o: &Occ, selected: bool) {
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
    if inner.width() < 6.0 || inner.height() < 6.0 {
        return;
    }
    let mut x = inner.min.x;
    for (on, icon) in [(o.recurring, "recurrence"), (o.private, "private"), (o.meeting, "people")] {
        if on && inner.width() > 60.0 {
            let r = Rect::from_min_size(pos2(x, inner.min.y + 1.0), Vec2::splat(12.0));
            app.icons.paint(ui, r, icon, app.theme.text_muted);
            x += 14.0;
        }
    }
    let w = inner.max.x - x;
    let sub = paint_text_clipped(
        ui,
        pos2(x, inner.min.y),
        &o.label(),
        FontId::proportional(12.5),
        app.theme.text,
        w,
    );
    if !o.location.trim().is_empty() && inner.height() > 30.0 {
        paint_text_clipped(
            ui,
            pos2(inner.min.x, sub.max.y + 1.0),
            o.location.trim(),
            FontId::proportional(11.5),
            app.theme.text_muted,
            inner.width(),
        );
    }
}

// ---------------------------------------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------------------------------------

pub(super) fn show(ui: &mut Ui, app: &mut App) {
    let ctx = ui.ctx().clone();
    let mut st = load(&ctx);
    sync_mode(app, &mut st);
    let full = ui.max_rect();
    let (pane, main) = split_folder_pane(ui, app);
    if let Some(p) = pane {
        pane_bg(ui, p, app.theme.pane);
        child(ui, p.shrink(6.0), "mc-cal-pane", |ui| folder_pane(ui, app, &mut st));
    }
    pane_bg(ui, main, app.theme.reading);
    child(ui, main, "mc-cal-main", |ui| content(ui, app, &mut st));
    keyboard(&ctx, app, &mut st);
    ui.expand_to_include_rect(full);
    store(&ctx, st);
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
        app.ui.module_selection = None;
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        editors::open_appointment(app, st, sel);
    }
}

fn folder_pane(ui: &mut Ui, app: &mut App, st: &mut ModState) {
    ScrollArea::vertical().id_salt("mc-cal-pane-scroll").auto_shrink([false, false]).show(ui, |ui| {
        let sel = selected_day(app, st);
        let (y, m) = st.nav_month.unwrap_or_else(|| {
            let (y, m, _) = ymd(sel);
            (y, m)
        });
        let months = if ui.available_height() > 560.0 { 2 } else { 1 };
        for i in 0..months {
            let (yy, mm) = add_months(y, m, i);
            mini_month(ui, app, st, yy, mm, i == 0);
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
                st.cal_shown = set;
            }
            if let Some(f) = clicked {
                let mut set = shown;
                set.insert(f.0);
                st.cal_shown = set;
                select_folder(app, f);
            }
        }
    });
}

/// Date navigator month.
fn mini_month(ui: &mut Ui, app: &mut App, st: &mut ModState, y: i64, m: u8, arrows: bool) {
    let w = ui.available_width().min(240.0);
    let cell = (w / 7.0).floor();
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
        let x = row.min.x + cell * (c as f32) + cell / 2.0;
        let name = WEEK_INITIALS.get(c).copied().unwrap_or("");
        ui.painter().text(
            pos2(x, row.center().y),
            Align2::CENTER_CENTER,
            name,
            FontId::proportional(11.0),
            app.theme.text_muted,
        );
    }
    let first = day_of(y, m, 1).unwrap_or_else(|| today(app));
    let start = week_start(first);
    let dim = i64::from(dates::days_in_month(y, m));
    let weeks = ((first - start) + dim + 6) / 7;
    let sel = selected_day(app, st);
    let visible: BTreeSet<i64> = if st.cal.mode == CalendarMode::Month {
        [sel].into_iter().collect()
    } else {
        visible_days(st, sel).into_iter().collect()
    };
    let busy: BTreeSet<i64> = gather(app, &shown_calendars(app, st), start, start + weeks * 7)
        .iter()
        .flat_map(|o| o.first_day()..=o.last_day().min(o.first_day() + 62))
        .collect();
    let now = today(app);
    let (grid, _) = ui.allocate_exact_size(vec2(w, cell.min(24.0) * weeks as f32), Sense::hover());
    let ch = cell.min(24.0);
    for i in 0..weeks * 7 {
        let d = start + i;
        let (r, c) = (i / 7, i % 7);
        let rect = Rect::from_min_size(
            pos2(grid.min.x + cell * c as f32, grid.min.y + ch * r as f32),
            vec2(cell, ch),
        );
        let resp = ui.interact(rect, Id::new(("mc-nav-day", y, m, d)), Sense::click());
        a11y(&resp, WidgetType::Button, &long_date(d));
        let in_month = ymd(d).1 == m;
        let mut color = if in_month { app.theme.text } else { app.theme.text_muted };
        if d == now {
            ui.painter().rect_filled(rect.shrink(1.0), CornerRadius::same(3), app.theme.accent);
            color = Color32::WHITE;
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
        if busy.contains(&d) && in_month {
            let dot = pos2(rect.center().x, rect.max.y - 3.0);
            ui.painter().circle_filled(dot, 1.6, if d == now { Color32::WHITE } else { app.theme.accent });
        }
        if resp.clicked() {
            let keep = st.nav_month;
            go_to(app, st, d);
            st.nav_month = keep;
        }
    }
}

const WEEK_INITIALS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

fn content(ui: &mut Ui, app: &mut App, st: &mut ModState) {
    let rect = ui.max_rect();
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), BAR_H));
    child(ui, bar.shrink2(vec2(10.0, 8.0)), "mc-cal-bar", |ui| folder_bar(ui, app, st));
    ui.painter().hline(rect.x_range(), bar.max.y, Stroke::new(1.0, app.theme.separator));
    let body = Rect::from_min_max(pos2(rect.min.x, bar.max.y + 1.0), rect.max);
    if body.height() < 40.0 || body.width() < 80.0 {
        return;
    }
    match st.cal.mode {
        CalendarMode::Month => month_view(ui, app, st, body),
        CalendarMode::Schedule => schedule_view(ui, app, st, body),
        _ => time_grid_view(ui, app, st, body),
    }
}

fn folder_bar(ui: &mut Ui, app: &mut App, st: &mut ModState) {
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
        ui.label(RichText::new(title(st, sel)).size(19.0).color(app.theme.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut mode = st.cal.mode;
            ComboBox::from_id_salt("mc-cal-mode").selected_text(mode_label(mode)).width(130.0).show_ui(
                ui,
                |ui| {
                    for m in [
                        CalendarMode::Day,
                        CalendarMode::WorkWeek,
                        CalendarMode::Week,
                        CalendarMode::Month,
                        CalendarMode::Schedule,
                    ] {
                        ui.selectable_value(&mut mode, m, mode_label(m));
                    }
                },
            );
            if mode != st.cal.mode {
                run_view_cmd(app, st, "calendar.arrange", json!({ "mode": mode.name() }));
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

// ---------------------------------------------------------------------------------------------------------
// Month view
// ---------------------------------------------------------------------------------------------------------

fn month_view(ui: &mut Ui, app: &mut App, st: &mut ModState, rect: Rect) {
    let sel = selected_day(app, st);
    let (start, weeks) = month_grid(sel);
    let (_, month, _) = ymd(sel);
    let now = today(app);
    let head_h = 26.0;
    let cell_w = rect.width() / 7.0;
    let cell_h = ((rect.height() - head_h) / weeks as f32).max(30.0);
    let p = ui.painter().clone();
    for c in 0..7 {
        let r = Rect::from_min_size(pos2(rect.min.x + cell_w * c as f32, rect.min.y), vec2(cell_w, head_h));
        let strong = weekday(now) == c as u8;
        let name = super::common::WEEKDAYS.get(c).copied().unwrap_or("");
        p.text(
            pos2(r.min.x + 8.0, r.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(if strong { 13.5 } else { 13.0 }),
            if strong { app.theme.accent } else { app.theme.text_muted },
        );
    }
    let occs = gather(app, &shown_calendars(app, st), start, start + weeks * 7);
    let shaded = blend(app.theme.reading, app.theme.backdrop, 0.45);
    let line = Stroke::new(1.0, app.theme.separator);
    for i in 0..weeks * 7 {
        let d = start + i;
        let (r, c) = (i / 7, i % 7);
        let cell = Rect::from_min_size(
            pos2(rect.min.x + cell_w * c as f32, rect.min.y + head_h + cell_h * r as f32),
            vec2(cell_w, cell_h),
        );
        let resp = ui.interact(cell, Id::new(("mc-cal-cell", d)), Sense::click());
        a11y(&resp, WidgetType::Button, &long_date(d));
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
        p.text(
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
        if resp.clicked() {
            app.ui.module_selection = None;
            if d != sel {
                go_to(app, st, d);
            }
        }
        if resp.double_clicked() {
            let folder = current_folder(app, Kind::Appointment);
            editors::new_appointment(app, st, folder, d, 0, 0, true);
        }
        // Items
        let line_h = 19.0;
        let top = cell.min.y + 24.0;
        let avail = ((cell.max.y - top - 2.0) / line_h).floor().max(0.0) as usize;
        let mut items: Vec<&Occ> = occs.iter().filter(|o| o.covers(d)).collect();
        items.sort_by_key(|o| (!o.banner(), o.start, o.id));
        let n = items.len();
        let show = if n > avail { avail.saturating_sub(1) } else { n };
        for (k, o) in items.iter().take(show).enumerate() {
            let chip = Rect::from_min_size(
                pos2(cell.min.x + 3.0, top + line_h * k as f32),
                vec2(cell_w - 6.0, line_h - 2.0),
            );
            let resp = ui.interact(chip, Id::new(("mc-cal-occ", o.id.0, d)), Sense::click());
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
                paint_text_clipped(
                    ui,
                    pos2(chip.min.x + 7.0, chip.min.y + 2.0),
                    &o.label(),
                    FontId::proportional(12.0),
                    app.theme.text,
                    chip.width() - 10.0,
                );
            } else {
                if resp.hovered() || selected {
                    p.rect_filled(chip, CornerRadius::same(3), fill);
                }
                p.circle_filled(pos2(chip.min.x + 6.0, chip.center().y), 3.5, bar);
                let text = format!("{} {}", clock(o.start.rem_euclid(DAY) / 60), o.label());
                paint_text_clipped(
                    ui,
                    pos2(chip.min.x + 13.0, chip.min.y + 2.0),
                    &text,
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
            let resp = ui.interact(more, Id::new(("mc-cal-more", d)), Sense::click());
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

fn time_grid_view(ui: &mut Ui, app: &mut App, st: &mut ModState, rect: Rect) {
    let sel = selected_day(app, st);
    let days = visible_days(st, sel);
    let (Some(&first), Some(&last)) = (days.first(), days.last()) else { return };
    let n = days.len() as f32;
    let now = today(app);
    let scroll_w = 12.0;
    let col_w = ((rect.width() - RULER_W - scroll_w) / n).max(20.0);
    let p = ui.painter().clone();
    let line = Stroke::new(1.0, app.theme.separator);

    // Day headers.
    let head_h = 44.0;
    for (i, &d) in days.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(rect.min.x + RULER_W + col_w * i as f32, rect.min.y),
            vec2(col_w, head_h),
        );
        let resp = ui.interact(r, Id::new(("mc-cal-dayhead", d)), Sense::click());
        a11y(&resp, WidgetType::Button, &long_date(d));
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
        p.text(
            pos2(num.max.x + 6.0, r.center().y + 1.0),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(12.5),
            color,
        );
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
    let occs = gather(app, &shown_calendars(app, st), first, last + 1);
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
            pos2(strip.min.x + RULER_W + col_w * i as f32, strip.min.y),
            vec2(col_w, strip_h),
        );
        let resp = ui.interact(r, Id::new(("mc-cal-allday", d)), Sense::click());
        a11y(&resp, WidgetType::Button, &format!("All day, {}", long_date(d)));
        p.vline(r.min.x, r.y_range(), line);
        if resp.double_clicked() {
            let folder = current_folder(app, Kind::Appointment);
            editors::new_appointment(app, st, folder, d, 0, 0, true);
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
            pos2(strip.min.x + RULER_W + col_w * a + 2.0, strip.min.y + 3.0 + LANE_H * lane as f32),
            pos2(strip.min.x + RULER_W + col_w * b - 2.0, strip.min.y + 1.0 + LANE_H * (lane + 1) as f32),
        );
        let resp = ui.interact(r, Id::new(("mc-cal-banner", o.id.0, o.start)), Sense::click());
        a11y(&resp, WidgetType::Button, &occ_a11y(o));
        let selected = app.ui.module_selection == Some(o.id.0);
        paint_block(ui, app, r, o, selected);
        occ_interact(app, st, &resp, o);
    }

    // Time grid.
    let grid = Rect::from_min_max(pos2(rect.min.x, strip.max.y + 1.0), rect.max);
    let (start_min, end_min) = if st.cal.working_hours { (WORK_START, WORK_END) } else { (0, 24 * 60) };
    let slot = i64::from(st.cal.time_scale.max(5));
    let px_per_min = SLOT_H / slot as f32;
    let total_h = (end_min - start_min) as f32 * px_per_min;
    let key = format!("{}|{}|{}|{}", st.cal.mode.name(), st.cal.date, slot, st.cal.working_hours);
    let mut area = ScrollArea::vertical().id_salt("mc-cal-grid").auto_shrink([false, false]);
    if st.cal_scroll_key != key {
        area = area.vertical_scroll_offset(((WORK_START - start_min).max(0)) as f32 * px_per_min);
        st.cal_scroll_key = key;
    }
    let now_min = now_minute(app);
    child(ui, grid, "mc-cal-grid-area", |ui| {
        area.show(ui, |ui| {
            let (area_rect, resp) =
                ui.allocate_exact_size(vec2(RULER_W + col_w * n, total_h), Sense::click());
            let p = ui.painter().clone();
            let shade = blend(app.theme.reading, app.theme.backdrop, 0.3);
            let x0 = area_rect.min.x + RULER_W;
            for (i, &d) in days.iter().enumerate() {
                let col =
                    Rect::from_min_size(pos2(x0 + col_w * i as f32, area_rect.min.y), vec2(col_w, total_h));
                p.rect_filled(col, CornerRadius::ZERO, shade);
                if (1..=5).contains(&weekday(d)) {
                    let a = (WORK_START.max(start_min) - start_min) as f32 * px_per_min;
                    let b = (WORK_END.min(end_min) - start_min) as f32 * px_per_min;
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
            if let Some((sd, sm)) = st.cal_slot
                && let Some(i) = days.iter().position(|&d| d == sd)
                && (start_min..end_min).contains(&sm)
            {
                let y = area_rect.min.y + (sm - start_min) as f32 * px_per_min;
                let r = Rect::from_min_size(pos2(x0 + col_w * i as f32, y), vec2(col_w, SLOT_H));
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
                        pos2(area_rect.min.x + RULER_W - 6.0, y + 2.0),
                        Align2::RIGHT_TOP,
                        hour_label(m / 60),
                        FontId::proportional(11.0),
                        app.theme.text_muted,
                    );
                }
                m += slot;
            }
            let pos_to_slot = |pos: egui::Pos2| -> Option<(i64, i64)> {
                let i = ((pos.x - x0) / col_w).floor();
                if i < 0.0 {
                    return None;
                }
                let d = *days.get(i as usize)?;
                let minute = start_min + (((pos.y - area_rect.min.y) / px_per_min) as i64 / slot) * slot;
                Some((d, minute.clamp(start_min, end_min - slot)))
            };
            if (resp.clicked() || resp.double_clicked())
                && let Some(pos) = resp.interact_pointer_pos()
                && let Some((d, minute)) = pos_to_slot(pos)
            {
                st.cal_slot = Some((d, minute));
                app.ui.module_selection = None;
                if resp.double_clicked() {
                    let folder = current_folder(app, Kind::Appointment);
                    editors::new_appointment(app, st, folder, d, minute, slot.max(30), false);
                }
            }
            // Appointments.
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
                    let resp = ui.interact(r, Id::new(("mc-cal-block", o.id.0, o.start, d)), Sense::click());
                    a11y(&resp, WidgetType::Button, &occ_a11y(o));
                    let selected = app.ui.module_selection == Some(o.id.0);
                    paint_block(ui, app, r, o, selected);
                    occ_interact(app, st, &resp, o);
                }
                if d == now && (start_min..end_min).contains(&now_min) {
                    let y = area_rect.min.y + (now_min - start_min) as f32 * px_per_min;
                    let xa = x0 + col_w * i as f32;
                    p.hline(xa..=xa + col_w, y, Stroke::new(2.0, app.theme.danger));
                    p.circle_filled(pos2(xa, y), 4.0, app.theme.danger);
                }
            }
        });
    });
}

// ---------------------------------------------------------------------------------------------------------
// Schedule view
// ---------------------------------------------------------------------------------------------------------

fn schedule_view(ui: &mut Ui, app: &mut App, st: &mut ModState, rect: Rect) {
    let sel = selected_day(app, st);
    let label_w = 190.0_f32.min(rect.width() / 3.0);
    let hour_w = 80.0;
    let head_h = 30.0;
    let row_h = 46.0;
    let shown = shown_calendars(app, st);
    let mut rows: Vec<(String, FolderId)> =
        shown.iter().filter_map(|f| app.s.mailbox.folder(*f).map(|x| (x.name.clone(), *f))).collect();
    rows.sort();
    if rows.is_empty() {
        empty_message(ui, rect, app, "No calendar selected", "Tick a calendar under My Calendars.");
        return;
    }
    let p = ui.painter().clone();
    let line = Stroke::new(1.0, app.theme.separator);
    let occs = gather(app, &shown, sel, sel + 1);
    // Calendar names.
    let labels = Rect::from_min_size(rect.min, vec2(label_w, rect.height()));
    p.rect_filled(labels, CornerRadius::ZERO, blend(app.theme.reading, app.theme.backdrop, 0.3));
    for (k, (name, _)) in rows.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(labels.min.x, labels.min.y + head_h + row_h * k as f32),
            vec2(label_w, row_h),
        );
        paint_text_clipped(
            ui,
            pos2(r.min.x + 10.0, r.center().y - 8.0),
            name,
            FontId::proportional(13.0),
            app.theme.text,
            label_w - 16.0,
        );
        p.hline(labels.x_range(), r.max.y, line);
    }
    let timeline = Rect::from_min_max(pos2(rect.min.x + label_w + 1.0, rect.min.y), rect.max);
    let key = format!("schedule|{}", st.cal.date);
    let mut area = ScrollArea::horizontal().id_salt("mc-cal-schedule").auto_shrink([false, false]);
    if st.cal_scroll_key != key {
        area = area.horizontal_scroll_offset(8.0 * hour_w);
        st.cal_scroll_key = key;
    }
    let now = today(app);
    let now_min = now_minute(app);
    child(ui, timeline, "mc-cal-schedule-area", |ui| {
        area.show(ui, |ui| {
            let total_w = hour_w * 24.0;
            let height = head_h + row_h * rows.len() as f32;
            let (area_rect, resp) =
                ui.allocate_exact_size(vec2(total_w, height.max(timeline.height() - 14.0)), Sense::click());
            let p = ui.painter().clone();
            for h in 0..24 {
                let x = area_rect.min.x + hour_w * h as f32;
                let work = (8..17).contains(&h) && (1..=5).contains(&weekday(sel));
                let col = Rect::from_min_size(
                    pos2(x, area_rect.min.y + head_h),
                    vec2(hour_w, row_h * rows.len() as f32),
                );
                p.rect_filled(
                    col,
                    CornerRadius::ZERO,
                    if work { app.theme.reading } else { blend(app.theme.reading, app.theme.backdrop, 0.3) },
                );
                p.vline(x, area_rect.y_range(), line);
                p.text(
                    pos2(x + 6.0, area_rect.min.y + head_h / 2.0),
                    Align2::LEFT_CENTER,
                    hour_label(h),
                    FontId::proportional(11.5),
                    app.theme.text_muted,
                );
            }
            for k in 0..=rows.len() {
                p.hline(area_rect.x_range(), area_rect.min.y + head_h + row_h * k as f32, line);
            }
            if (resp.clicked() || resp.double_clicked())
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let k = ((pos.y - area_rect.min.y - head_h) / row_h).floor();
                let minute = (((pos.x - area_rect.min.x) / hour_w * 60.0) as i64 / 30) * 30;
                if k >= 0.0
                    && let Some((_, folder)) = rows.get(k as usize)
                {
                    st.cal_slot = Some((sel, minute.clamp(0, 23 * 60 + 30)));
                    app.ui.module_selection = None;
                    if resp.double_clicked() {
                        editors::new_appointment(app, st, Some(*folder), sel, minute, 30, false);
                    }
                }
            }
            for (k, (_, folder)) in rows.iter().enumerate() {
                let y = area_rect.min.y + head_h + row_h * k as f32;
                for o in occs.iter().filter(|o| o.folder == *folder) {
                    let (ds, de) = (sel * DAY, (sel + 1) * DAY);
                    let (s, e) = (o.start.max(ds), o.end.min(de).max(o.start.max(ds) + 900));
                    let x0 = area_rect.min.x + ((s - ds) as f32 / 3600.0) * hour_w;
                    let x1 = area_rect.min.x + ((e - ds) as f32 / 3600.0) * hour_w;
                    let r = Rect::from_min_max(pos2(x0 + 1.0, y + 4.0), pos2(x1 - 1.0, y + row_h - 4.0));
                    let resp = ui.interact(r, Id::new(("mc-cal-sched", o.id.0, o.start)), Sense::click());
                    a11y(&resp, WidgetType::Button, &occ_a11y(o));
                    let selected = app.ui.module_selection == Some(o.id.0);
                    paint_block(ui, app, r, o, selected);
                    occ_interact(app, st, &resp, o);
                }
            }
            if sel == now {
                let x = area_rect.min.x + now_min as f32 / 60.0 * hour_w;
                p.vline(x, area_rect.y_range(), Stroke::new(2.0, app.theme.danger));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::Recurrence;

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
        assert_eq!(title(&st, sel), "October 2026");
        assert_eq!(day_string(step(&st, sel, 1)), "2026-11-09");
        st.cal.mode = CalendarMode::Week;
        assert_eq!(title(&st, sel), "October 4 – 10, 2026");
        st.cal.mode = CalendarMode::WorkWeek;
        assert_eq!(visible_days(&st, sel).len(), 5);
        st.cal.mode = CalendarMode::Day;
        assert_eq!(title(&st, sel), "Friday, October 9, 2026");
        assert_eq!(month_grid(sel), (day("2026-09-27"), 5));
    }
}
