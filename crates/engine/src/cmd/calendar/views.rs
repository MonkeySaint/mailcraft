//! The View tab's generic commands in the Calendar module ([`module_view`]): Change View (Calendar,
//! Preview, List, Active), Arrange By, Reading Pane, To-Do Bar, View Settings, Reset View and Use Tighter
//! Spacing, plus the saved per-folder view and the settings the views read (work week, work hours, time
//! zones).
//!
//! Like the `calendar.*` view commands, these take the UI's current [`CalendarViewState`] as `params.view`
//! and return the new one as `{"view": {...}}`. Every view change is also saved on the current calendar
//! folder ([`CalendarProps::view`], date left out) with a rising sequence number, journaled without an undo
//! entry: a caller that could not pass `view` (a ribbon button run straight through `Session::run`) still
//! reaches the UI, which adopts a saved view whose number is newer than the last it saw, and the view
//! survives a restart.

use serde_json::{Value, json};

use super::pim::{self, Kind};
use super::{TIME_SCALES, props, tz};
use crate::cmd::util::opt_str;
use crate::{CmdError, CmdResult, Module, Session};
use mailcraft_model::calendar::{
    CalendarColumn, CalendarCurrentView, CalendarMode, CalendarTimeZones, CalendarViewState,
};
use mailcraft_model::{FolderId, ReadingPanePos};
use mailcraft_store::Op;

/// View commands [`module_view`] answers in the Calendar module.
pub const MODULE_VIEW_IDS: [&str; 7] = [
    "view.changeView",
    "view.arrangeBy",
    "view.readingPane",
    "view.todoBar",
    "view.viewSettings",
    "view.resetView",
    "view.tighterSpacing",
];

/// Dialog id of the calendar's View Settings window.
pub const VIEW_SETTINGS_DIALOG: &str = "calendar:viewSettings";

/// Runs a View-tab command for the Calendar module. `None` when `id` is not one of [`MODULE_VIEW_IDS`] or the
/// Calendar module is not showing (the generic mail behaviour applies).
pub fn module_view(s: &mut Session, id: &str, p: &Value) -> Option<CmdResult> {
    if s.view.module != Module::Calendar || !MODULE_VIEW_IDS.contains(&id) {
        return None;
    }
    Some(run_module_view(s, id, p))
}

fn run_module_view(s: &mut Session, id: &str, p: &Value) -> CmdResult {
    let mut v = view_or_saved(s, p)?;
    let mut extra = serde_json::Map::new();
    match id {
        "view.changeView" => {
            let name = opt_str(p, "name").or_else(|| opt_str(p, "view")).unwrap_or("");
            v.current = CalendarCurrentView::parse(name).ok_or_else(|| {
                CmdError::Params(format!("`name` must be Calendar, Preview, List or Active, not `{name}`"))
            })?;
        }
        "view.arrangeBy" => {
            let key = opt_str(p, "key").unwrap_or("").trim();
            if let Some(mode) = CalendarMode::parse(key)
                .or_else(|| CalendarMode::ALL.into_iter().find(|m| m.label().eq_ignore_ascii_case(key)))
            {
                v.mode = mode;
                v.days = 1;
                if v.current.is_table() {
                    v.current = CalendarCurrentView::Calendar;
                }
            } else {
                let col = CalendarColumn::parse(key).ok_or_else(|| {
                    CmdError::Params(format!(
                        "`key` must be an arrangement (day, workWeek, week, month, schedule) or a column ({})",
                        CalendarColumn::ALL.map(CalendarColumn::name).join(", ")
                    ))
                })?;
                v.ascending = match opt_str(p, "order") {
                    Some("ascending") => true,
                    Some("descending") => false,
                    Some(o) => {
                        return Err(CmdError::Params(format!(
                            "`order` must be ascending or descending, not `{o}`"
                        )));
                    }
                    // Choosing the current column again flips the order.
                    None if v.sort == col => !v.ascending,
                    None => true,
                };
                v.sort = col;
            }
        }
        "view.readingPane" => {
            v.reading_pane = match opt_str(p, "position").unwrap_or("") {
                "right" => ReadingPanePos::Right,
                "bottom" => ReadingPanePos::Bottom,
                "off" => ReadingPanePos::Off,
                o => {
                    return Err(CmdError::Params(format!(
                        "`position` must be right, bottom or off, not `{o}`"
                    )));
                }
            };
        }
        "view.todoBar" => {
            let part = opt_str(p, "part").unwrap_or("");
            if !["calendar", "people", "tasks", "off"].contains(&part) {
                return Err(CmdError::Params(format!(
                    "`part` must be calendar, people, tasks or off, not `{part}`"
                )));
            }
            extra.insert("todoBar".into(), json!(part));
        }
        "view.tighterSpacing" => {
            v.tighter_spacing = pim::flag(p, "on")?.unwrap_or(!v.tighter_spacing);
        }
        "view.resetView" => {
            let keep = (v.date.clone(), v.days);
            v = CalendarViewState::default();
            (v.date, v.days) = keep;
        }
        "view.viewSettings" => {
            let keys = ["sort", "ascending", "timeScale", "readingPane", "tighterSpacing", "dailyTaskList"];
            if !pim::has_any(p, &keys) {
                // The calendar's View Settings window (drawn by the calendar view).
                s.notify(crate::Notice::OpenDialog(VIEW_SETTINGS_DIALOG.into(), json!({})));
                extra.insert("dialog".into(), json!(VIEW_SETTINGS_DIALOG));
            } else {
                apply_settings(&mut v, p)?;
            }
        }
        _ => return Err(CmdError::Unknown(id.to_owned())),
    }
    let mut out = super::view_result(s, &v)?;
    if let Some(o) = out.as_object_mut() {
        o.extend(extra);
    }
    Ok(out)
}

/// View Settings fields: sort column and order, time scale, reading pane, spacing, daily task list.
fn apply_settings(v: &mut CalendarViewState, p: &Value) -> Result<(), CmdError> {
    if let Some(c) = opt_str(p, "sort") {
        v.sort = CalendarColumn::parse(c).ok_or_else(|| CmdError::Params(format!("unknown column `{c}`")))?;
    }
    if let Some(a) = pim::flag(p, "ascending")? {
        v.ascending = a;
    }
    if let Some(m) = p.get("timeScale").filter(|x| !x.is_null()) {
        v.time_scale = m
            .as_u64()
            .and_then(|m| u8::try_from(m).ok())
            .filter(|m| TIME_SCALES.contains(m))
            .ok_or_else(|| CmdError::Params("`timeScale` must be 60, 30, 15, 10, 6 or 5".into()))?;
    }
    if let Some(r) = opt_str(p, "readingPane") {
        v.reading_pane = match r {
            "right" => ReadingPanePos::Right,
            "bottom" => ReadingPanePos::Bottom,
            "off" => ReadingPanePos::Off,
            o => {
                return Err(CmdError::Params(format!(
                    "`readingPane` must be right, bottom or off, not `{o}`"
                )));
            }
        };
    }
    if let Some(t) = pim::flag(p, "tighterSpacing")? {
        v.tighter_spacing = t;
    }
    if let Some(d) = opt_str(p, "dailyTaskList") {
        v.daily_task_list = match d {
            "normal" => mailcraft_model::calendar::DailyTaskList::Normal,
            "minimized" => mailcraft_model::calendar::DailyTaskList::Minimized,
            "off" => mailcraft_model::calendar::DailyTaskList::Off,
            o => {
                return Err(CmdError::Params(format!(
                    "`dailyTaskList` must be normal, minimized or off, not `{o}`"
                )));
            }
        };
    }
    Ok(())
}

/// `params.view`, else the current calendar's saved view, else the default (normalised either way).
pub(super) fn view_or_saved(s: &mut Session, p: &Value) -> Result<CalendarViewState, CmdError> {
    if p.get("view").is_some_and(|v| !v.is_null()) {
        return super::view_param(s, p);
    }
    let saved = pim::current_folder(s, Kind::Appointment)
        .and_then(|f| props::read(&s.mailbox, f).view)
        .unwrap_or_default();
    super::view_param(s, &json!({ "view": saved }))
}

/// The current calendar's saved view and its sequence number (0 = never saved).
pub fn saved_view(s: &Session) -> Option<(CalendarViewState, u64)> {
    let f = pim::current_folder(s, Kind::Appointment)?;
    let p = props::read(&s.mailbox, f);
    p.view.map(|v| (v, p.view_seq))
}

/// Saves `v` (without its date) on the current calendar folder with a new sequence number; journaled without
/// an undo entry.
pub(super) fn save_view(s: &mut Session, v: &CalendarViewState) {
    let Some(folder) = pim::current_folder(s, Kind::Appointment) else { return };
    let mut saved = v.clone();
    saved.date.clear();
    saved.days = 1;
    let now_seq = u64::try_from(s.now().unix).unwrap_or(0).saturating_mul(1000);
    let changed = props::update(&mut s.mailbox, folder, |p| {
        p.view_seq = p.view_seq.saturating_add(1).max(now_seq);
        p.view = Some(saved);
    });
    if changed {
        journal_folder(s, folder);
    }
}

/// Journals folder `folder` as it is now (a change made outside a mutating command's mailbox diff).
pub(super) fn journal_folder(s: &mut Session, folder: FolderId) {
    if let Some(f) = s.mailbox.folder(folder).cloned() {
        s.journal(vec![Op::PutFolder(Box::new(f))]);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Settings the calendar reads (File › Options › Calendar), through `options.get`
// ---------------------------------------------------------------------------------------------------------

/// The value of option `key` (`None` when the session has no such option).
pub fn option(s: &mut Session, key: &str) -> Option<Value> {
    let run = s.registry.get("options.get")?.run;
    run(s, &json!({ "key": key })).ok()?.get("value").cloned()
}

/// Sets option `key`, ignoring a session without it.
pub(super) fn set_option(s: &mut Session, key: &str, value: Value) {
    if let Some(run) = s.registry.get("options.set").map(|x| x.run) {
        let _ = run(s, &json!({ "key": key, "value": value }));
    }
}

/// Weekday from a name or id ("mon", "Monday", "MO"), 0 = Sunday.
fn weekday_index(name: &str) -> Option<u8> {
    const NAMES: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];
    let n = name.trim().to_ascii_lowercase();
    if n.len() < 2 {
        return None;
    }
    NAMES.iter().position(|full| full.starts_with(&n)).and_then(|i| u8::try_from(i).ok())
}

/// Working days from a `calendar.workWeek` value (0 = Sunday), sorted; `None` when unusable.
pub fn parse_work_week(v: &Value) -> Option<Vec<u8>> {
    let mut days: Vec<u8> = match v {
        Value::Array(a) => a
            .iter()
            .take(14)
            .filter_map(|x| match x {
                Value::String(t) => weekday_index(t),
                Value::Number(n) => n.as_u64().filter(|d| *d < 7).and_then(|d| u8::try_from(d).ok()),
                _ => None,
            })
            .collect(),
        Value::String(t) => t.split([',', ';', ' ']).filter_map(weekday_index).collect(),
        _ => return None,
    };
    days.sort_unstable();
    days.dedup();
    (!days.is_empty()).then_some(days)
}

/// Working days (0 = Sunday): option `calendar.workWeek`, else Monday to Friday.
pub fn work_week(s: &mut Session) -> Vec<u8> {
    option(s, "calendar.workWeek").as_ref().and_then(parse_work_week).unwrap_or_else(|| vec![1, 2, 3, 4, 5])
}

/// "HH:MM" → minutes after midnight.
fn clock_min(t: &str) -> Option<i64> {
    let (h, m) = t.trim().split_once(':')?;
    let (h, m): (i64, i64) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 60 + m)
}

/// Working hours as minutes after midnight: option `calendar.workHours` ("08:00-17:00"), else 8:00–17:00.
pub fn work_hours(s: &mut Session) -> (i64, i64) {
    option(s, "calendar.workHours")
        .and_then(|v| {
            let t = v.as_str()?.to_owned();
            let (a, b) = t.split_once('-')?;
            let (a, b) = (clock_min(a)?, clock_min(b)?);
            (a < b).then_some((a, b))
        })
        .unwrap_or((8 * 60, 17 * 60))
}

/// The calendar's time zones (kept on the default calendar).
pub fn time_zones(s: &Session) -> CalendarTimeZones {
    pim::default_folder(s, Kind::Appointment)
        .and_then(|f| props::read(&s.mailbox, f).time_zones)
        .unwrap_or_default()
}

/// A second time zone to show in day/week views: (label, offset at `unix`), from the named second zone, else
/// the `calendar.secondTimeZone` options.
pub fn second_zone(s: &mut Session, unix: i64) -> Option<(String, i32)> {
    let z = time_zones(s);
    if z.show_second
        && let Some(off) = tz::offset_at(&z.second_zone, unix)
    {
        let label =
            if z.second_label.trim().is_empty() { short_zone_name(&z.second_zone) } else { z.second_label };
        return Some((label, off));
    }
    if option(s, "calendar.secondTimeZone").and_then(|v| v.as_bool()) == Some(true) {
        let off = option(s, "calendar.secondTimeZoneOffset").and_then(|v| v.as_i64()).unwrap_or(0);
        let off = i32::try_from(off.clamp(-720, 840)).unwrap_or(0);
        let label = option(s, "calendar.secondTimeZoneLabel")
            .and_then(|v| v.as_str().map(str::to_owned))
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_else(|| mailcraft_formats::ics::tz::fixed_tzid(off));
        return Some((label, off));
    }
    None
}

/// "Toronto" for "America/Toronto".
pub fn short_zone_name(id: &str) -> String {
    id.rsplit('/').next().unwrap_or(id).replace('_', " ")
}

/// Label of the primary zone for the ruler (the zone's label, else its city, else empty).
pub fn primary_zone_label(s: &Session) -> String {
    let z = time_zones(s);
    if !z.label.trim().is_empty() {
        z.label
    } else if tz::known(&z.zone) && z.show_second {
        short_zone_name(&z.zone)
    } else {
        String::new()
    }
}

/// Re-reads the display offset from the named zone (after DST changes or a restart). True when it changed.
pub fn refresh_time_zone(s: &mut Session) -> bool {
    let z = time_zones(s);
    let Some(off) = tz::offset_at(&z.zone, s.now().unix) else { return false };
    if s.tz_offset_min == off {
        return false;
    }
    s.tz_offset_min = off;
    true
}
