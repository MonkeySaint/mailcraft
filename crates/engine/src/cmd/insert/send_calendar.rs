//! Insert › Calendar… (Send a Calendar via Email): a calendar folder's items over a date range written into
//! the message, as a daily schedule (a table per day with the free time between items) or a list of events,
//! with three levels of detail, optionally limited to working hours, and optionally attached as an iCalendar
//! (.ics) file so the recipient can open the items in a calendar program.
//!
//! Parameters: `calendar` (folder id or name; default: the first calendar folder), `range` (`today`,
//! `tomorrow`, `next7`, `next14`, `next30`, `thisMonth`, `nextMonth`) or `from` / `to` dates, `detail`
//! (`availability`, `limited`, `full`), `workingHours`, `includePrivate` (details of private items),
//! `layout` (`schedule` or `list`) and `ics`. Without `range` or `from` the Send a Calendar dialog opens.

use serde_json::{Value, json};

use super::super::compose::{compose_id, compose_mut, parse_when};
use super::{busy_label, edit, table_cell};
use crate::cmd::calendar::occurrences;
use crate::cmd::util::{dialog, opt_bool, opt_str};
use crate::compose::ComposeAttachment;
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::dates::ListTime;
use mailcraft_model::rich::TableCell;
use mailcraft_model::{Appointment, Block, DateTime, FolderId, FolderKind, Inline, ParaStyle, TextStyle};

/// Date-range presets of the dialog: (id, label).
pub const RANGES: [(&str, &str); 7] = [
    ("today", "Today"),
    ("tomorrow", "Tomorrow"),
    ("next7", "Next 7 days"),
    ("next14", "Next 14 days"),
    ("next30", "Next 30 days"),
    ("thisMonth", "This month"),
    ("nextMonth", "Next month"),
];

/// Detail levels: (id, label).
pub const DETAILS: [(&str, &str); 3] =
    [("availability", "Availability only"), ("limited", "Limited details"), ("full", "Full details")];

/// Email layouts: (id, label).
pub const LAYOUTS: [(&str, &str); 2] =
    [("schedule", "Day by day, with free time"), ("list", "Just the events")];

/// Longest range sent (days).
const MAX_DAYS: i64 = 366;
/// Most occurrences of one appointment collected.
const MAX_OCCURRENCES: usize = 400;
/// Shortest free gap shown in the daily schedule (minutes).
const MIN_FREE_MIN: i64 = 15;

/// One occurrence in the range.
struct Occ<'a> {
    start: DateTime,
    end: DateTime,
    appt: &'a Appointment,
}

/// The calendar folders, default role first: (id, path).
pub fn calendar_folders(s: &Session) -> Vec<(FolderId, String)> {
    let mut out: Vec<(FolderId, String, bool)> = s
        .mailbox
        .folders
        .values()
        .filter(|f| f.kind == FolderKind::Calendar)
        .map(|f| (f.id, s.mailbox.folder_path(f.id), f.role.is_some()))
        .collect();
    out.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)).then(a.0.0.cmp(&b.0.0)));
    out.into_iter().map(|(id, path, _)| (id, path)).collect()
}

/// First and last day numbers (in the session's offset) of a range preset, counted from `today`.
fn preset_days(today: i64, range: &str) -> Option<(i64, i64)> {
    let month_bounds = |day: i64, ahead: u8| {
        let (y, m, _) = mailcraft_model::dates::civil_from_days(day);
        let (y, m) = if ahead == 0 {
            (y, m)
        } else if m == 12 {
            (y.saturating_add(1), 1)
        } else {
            (y, m + 1)
        };
        let first = mailcraft_model::dates::days_from_civil(y, m, 1)?;
        let len = i64::from(mailcraft_model::dates::days_in_month(y, m));
        Some((first, first.saturating_add(len.saturating_sub(1))))
    };
    match range {
        "today" => Some((today, today)),
        "tomorrow" => Some((today.saturating_add(1), today.saturating_add(1))),
        "next7" => Some((today, today.saturating_add(6))),
        "next14" => Some((today, today.saturating_add(13))),
        "next30" => Some((today, today.saturating_add(29))),
        "thisMonth" => month_bounds(today, 0),
        "nextMonth" => month_bounds(today, 1),
        _ => None,
    }
}

/// Local midnight of day `day` as a time.
fn day_start(day: i64, tz: i32) -> DateTime {
    DateTime::from_unix(day.saturating_mul(86_400).saturating_sub(i64::from(tz).saturating_mul(60)), tz)
}

fn bold(text: String) -> Block {
    Block::Paragraph {
        style: ParaStyle::default(),
        inlines: vec![Inline::Text { text, style: TextStyle { bold: Some(true), ..Default::default() } }],
    }
}

/// What one item says at a detail level.
fn describe(a: &Appointment, detail: &str, include_private: bool) -> String {
    let private = a.private && !include_private;
    match detail {
        "availability" => busy_label(a.show_as).to_owned(),
        _ if private => format!("{}  Private", busy_label(a.show_as)),
        "limited" => a.subject.clone(),
        _ => {
            let mut l = a.subject.clone();
            if !a.location.is_empty() {
                l.push_str(&format!(" ({})", a.location));
            }
            let body = a.body.trim();
            if !body.is_empty() {
                let one_line: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
                let short: String = one_line.chars().take(300).collect();
                l.push_str(&format!(" \u{2014} {short}"));
            }
            l
        }
    }
}

pub(super) fn calendar(s: &mut Session, p: &Value) -> CmdResult {
    let range = opt_str(p, "range");
    let from = opt_str(p, "from");
    if range.is_none() && from.is_none() {
        let folders: Vec<Value> =
            calendar_folders(s).into_iter().map(|(id, path)| json!({"id": id.0, "name": path})).collect();
        return dialog(s, "insertCalendar", json!({"compose": compose_id(s, p)?, "calendars": folders}));
    }
    let tz = s.tz_offset_min;
    let today = s.now().day_number(tz);
    let (d0, d1) = match (range, from) {
        (Some(r), _) => {
            preset_days(today, r).ok_or_else(|| CmdError::Params(format!("unknown range `{r}`")))?
        }
        (None, Some(f)) => {
            let to = opt_str(p, "to").unwrap_or(f);
            let a =
                parse_when(s, f).ok_or_else(|| CmdError::Params(format!("cannot read the date `{f}`")))?;
            let b =
                parse_when(s, to).ok_or_else(|| CmdError::Params(format!("cannot read the date `{to}`")))?;
            (a.day_number(tz), b.day_number(tz))
        }
        (None, None) => return Err(CmdError::Params("`range` or `from` required".into())),
    };
    if d1 < d0 || d1.saturating_sub(d0) >= MAX_DAYS {
        return Err(CmdError::Params("the date range must run forwards and span at most a year".into()));
    }
    let detail = opt_str(p, "detail").unwrap_or("limited");
    if !DETAILS.iter().any(|(id, _)| *id == detail) {
        return Err(CmdError::Params(format!("unknown detail `{detail}`")));
    }
    let layout = opt_str(p, "layout").unwrap_or("list");
    if !LAYOUTS.iter().any(|(id, _)| *id == layout) {
        return Err(CmdError::Params(format!("unknown layout `{layout}`")));
    }
    let working = opt_bool(p, "workingHours") == Some(true);
    let include_private = opt_bool(p, "includePrivate") == Some(true);
    let attach = opt_bool(p, "ics") == Some(true);
    let all = calendar_folders(s);
    let wanted = match p.get("calendar") {
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::String(t)) => Some(t.clone()),
        _ => None,
    };
    let folder = match wanted.as_deref() {
        None => all.first().map(|(id, _)| *id),
        Some(c) => all
            .iter()
            .find(|(id, _)| id.0.to_string() == c.trim())
            .or_else(|| {
                all.iter().find(|(id, path)| {
                    path.eq_ignore_ascii_case(c.trim())
                        || s.mailbox.folders.get(id).is_some_and(|f| f.name.eq_ignore_ascii_case(c.trim()))
                })
            })
            .map(|(id, _)| *id),
    }
    .ok_or_else(|| CmdError::Params("no such calendar".into()))?;
    let cal_name =
        s.mailbox.folders.get(&folder).map(|f| f.name.clone()).unwrap_or_else(|| "Calendar".into());
    let (work_start, work_end) = s.settings.work_hours();
    let work_week = s.settings.work_week();
    let range_from = day_start(d0, tz);
    let range_to = day_start(d1.saturating_add(1), tz);
    let mut occ: Vec<Occ<'_>> = Vec::new();
    for a in s.mailbox.appointments.values().filter(|a| a.folder == folder) {
        for (start, end) in occurrences(a, range_from, range_to, MAX_OCCURRENCES) {
            occ.push(Occ { start, end, appt: a });
        }
    }
    occ.sort_by(|a, b| a.start.unix.cmp(&b.start.unix).then(a.appt.subject.cmp(&b.appt.subject)));

    let mut blocks = vec![bold(cal_name.clone())];
    let first = day_start(d0, tz);
    let last = day_start(d1, tz);
    blocks.push(Block::text_paragraph(&if d0 == d1 {
        ListTime::long_date(first, tz)
    } else {
        format!("{} \u{2013} {}", ListTime::long_date(first, tz), ListTime::long_date(last, tz))
    }));
    if working {
        let clock = |m: u32| format!("{:02}:{:02}", m / 60, m % 60);
        blocks.push(Block::text_paragraph(&format!(
            "Working hours only ({} \u{2013} {})",
            clock(work_start),
            clock(work_end)
        )));
    }
    let mut shown: Vec<&Appointment> = Vec::new();
    // Each occurrence shown once (an all-day item spans several days), for the .ics.
    let mut shown_occ: Vec<(DateTime, DateTime, &Appointment)> = Vec::new();
    let mut listed = 0usize;
    for day in d0..=d1 {
        let midnight = day_start(day, tz);
        let weekday = usize::from(mailcraft_model::dates::weekday_of_day(day).0);
        if working && !work_week.get(weekday).copied().unwrap_or(true) {
            continue;
        }
        let (win_start, win_end) = if working {
            (
                midnight.unix.saturating_add(i64::from(work_start).saturating_mul(60)),
                midnight.unix.saturating_add(i64::from(work_end).saturating_mul(60)),
            )
        } else {
            (midnight.unix, midnight.unix.saturating_add(86_400))
        };
        let today_items: Vec<&Occ<'_>> = occ
            .iter()
            .filter(|o| {
                let (st, en) = (o.start.unix, o.end.unix.max(o.start.unix.saturating_add(1)));
                // All-day items belong to every day they cover; timed ones to the part of the day shown.
                if o.appt.all_day {
                    st < midnight.unix.saturating_add(86_400) && en > midnight.unix
                } else {
                    st < win_end && en > win_start
                }
            })
            .collect();
        if today_items.is_empty() && layout == "list" {
            continue;
        }
        blocks.push(bold(ListTime::long_date(midnight, tz)));
        let when = |o: &Occ<'_>| {
            if o.appt.all_day {
                "All day".to_owned()
            } else {
                format!("{} \u{2013} {}", ListTime::clock(o.start, tz), ListTime::clock(o.end, tz))
            }
        };
        if layout == "schedule" {
            let mut rows: Vec<Vec<TableCell>> = Vec::new();
            let mut cursor = win_start;
            let free_row = |a: i64, b: i64| {
                vec![
                    table_cell(&format!(
                        "{} \u{2013} {}",
                        ListTime::clock(DateTime::from_unix(a, tz), tz),
                        ListTime::clock(DateTime::from_unix(b, tz), tz)
                    )),
                    table_cell("Free"),
                ]
            };
            for o in today_items.iter().filter(|o| o.appt.all_day) {
                rows.push(vec![table_cell(&when(o)), table_cell(&describe(o.appt, detail, include_private))]);
            }
            for o in today_items.iter().filter(|o| !o.appt.all_day) {
                let st = o.start.unix.max(win_start);
                if st.saturating_sub(cursor) >= MIN_FREE_MIN * 60 {
                    rows.push(free_row(cursor, st));
                }
                rows.push(vec![table_cell(&when(o)), table_cell(&describe(o.appt, detail, include_private))]);
                cursor = cursor.max(o.end.unix.min(win_end));
            }
            if win_end.saturating_sub(cursor) >= MIN_FREE_MIN * 60 && (working || !today_items.is_empty()) {
                rows.push(free_row(cursor, win_end));
            }
            if rows.is_empty() {
                rows.push(vec![
                    table_cell(if working { "Working hours" } else { "All day" }),
                    table_cell("Free"),
                ]);
            }
            blocks.push(Block::Table { borders: None, rows, border: true, width_px: Some(480.0) });
        } else {
            let items: Vec<Vec<Block>> = today_items
                .iter()
                .map(|o| {
                    vec![Block::text_paragraph(&format!(
                        "{}  {}",
                        when(o),
                        describe(o.appt, detail, include_private)
                    ))]
                })
                .collect();
            blocks.push(Block::List { kind: mailcraft_model::rich::ListKind::Bullet, items });
        }
        listed = listed.saturating_add(today_items.len());
        for o in &today_items {
            if !shown.iter().any(|a| a.id == o.appt.id) {
                shown.push(o.appt);
            }
            if !shown_occ.iter().any(|(st, _, a)| a.id == o.appt.id && st.unix == o.start.unix) {
                shown_occ.push((o.start, o.end, o.appt));
            }
        }
    }
    if listed == 0 && layout == "list" {
        blocks.push(Block::text_paragraph("Nothing is scheduled in this period."));
    }
    // The .ics carries what the message shows and nothing more: each occurrence in the range as a single
    // event (a recurring item's other occurrences stay private), with availability-only and private items
    // stripped to their busy status, and no reminders of the sender's.
    let ics = if attach {
        let redacted: Vec<Appointment> = shown_occ
            .iter()
            .map(|(start, end, a)| ics_event(a, *start, *end, detail, include_private))
            .collect();
        let refs: Vec<&Appointment> = redacted.iter().collect();
        Some(mailcraft_formats::ics::write(&refs, &[], None, s.now()))
    } else {
        None
    };
    let n = shown.len();
    edit(s, p, |c| c.insert_blocks(blocks))?;
    if let Some(text) = ics {
        let safe: String =
            cal_name.chars().map(|c| if c.is_alphanumeric() || c == ' ' { c } else { '_' }).collect();
        let c = compose_mut(s, p)?;
        c.attachments.push(ComposeAttachment {
            filename: format!("{} Calendar.ics", safe.trim()),
            mime_type: "text/calendar".into(),
            data: text.into_bytes(),
        });
        c.dirty = true;
    }
    Ok(json!({
        "appointments": n,
        "occurrences": listed,
        "detail": detail,
        "layout": layout,
        "calendar": folder.0,
        "from": d0,
        "to": d1,
        "ics": attach,
    }))
}

/// One occurrence of `a` (`start`..`end`) as the .ics sends it: a single event, its own UID per occurrence
/// of a recurring item, trimmed to `detail` ("availability" and private items without `include_private`
/// keep only the busy status; "limited" drops location, notes, people and categories).
fn ics_event(
    a: &Appointment,
    start: DateTime,
    end: DateTime,
    detail: &str,
    include_private: bool,
) -> Appointment {
    let mut e = a.clone();
    if a.recurrence.is_some() || a.recurrence_id.is_some() {
        let base = if a.uid.trim().is_empty() { format!("item-{}", a.id.0) } else { a.uid.clone() };
        e.uid = format!("{base}-{}", start.unix);
    }
    e.start = start;
    e.end = end;
    e.recurrence = None;
    e.recurrence_id = None;
    e.reminder_min = None;
    if detail == "availability" || (a.private && !include_private) {
        e.subject = busy_label(a.show_as).to_owned();
        e.location.clear();
        e.body.clear();
        e.attendees.clear();
        e.organizer = None;
        e.categories.clear();
    } else if detail == "limited" {
        e.location.clear();
        e.body.clear();
        e.attendees.clear();
        e.organizer = None;
        e.categories.clear();
    }
    e
}

#[cfg(test)]
mod tests {
    use super::super::super::compose::tests::{open, session};
    use super::*;
    use mailcraft_model::{BusyStatus, ItemId, Recurrence};

    fn appt(s: &mut Session, folder: FolderId, subject: &str, day: u8, h: u8, mins: i64) -> ItemId {
        let id = ItemId(s.mailbox.alloc_id());
        let start = DateTime::from_civil(2026, 10, day, h, 0, 0, 0).unwrap();
        s.mailbox.appointments.insert(
            id,
            Appointment {
                id,
                folder,
                subject: subject.into(),
                location: "Room 2".into(),
                body: "Agenda: budget".into(),
                start,
                end: DateTime::from_unix(start.unix + mins * 60, 0),
                show_as: BusyStatus::Busy,
                ..Default::default()
            },
        );
        id
    }

    #[test]
    fn every_calendar_presets_layouts_working_hours_and_ics() {
        let mut s = session();
        let cid = open(&mut s);
        let main = calendar_folders(&s).first().map(|(id, _)| *id).unwrap();
        let team = s.mailbox.create_folder(main, "Team").unwrap();
        let names: Vec<String> = calendar_folders(&s).into_iter().map(|(_, n)| n).collect();
        assert!(names.len() >= 2 && names.iter().any(|n| n.ends_with("Team")), "{names:?}");
        // The dialog lists every calendar.
        s.run("insert.calendar", &json!({})).unwrap();
        let listed = s.notices.iter().rev().find_map(|n| match n {
            crate::Notice::OpenDialog(name, p) if name == "insertCalendar" => Some(p.clone()),
            _ => None,
        });
        assert_eq!(listed.unwrap()["calendars"].as_array().unwrap().len(), names.len());

        // Session "now" is 2026-10-09 (a Friday) at noon UTC.
        appt(&mut s, main, "Standup", 9, 9, 30);
        appt(&mut s, main, "Early run", 9, 6, 30);
        let private = appt(&mut s, main, "Doctor", 9, 14, 60);
        {
            let a = s.mailbox.appointments.get_mut(&private).unwrap();
            a.private = true;
            a.categories = vec!["Medical".into()];
            a.reminder_min = Some(15);
        }
        let weekly = appt(&mut s, team, "Team sync", 5, 10, 60);
        {
            let a = s.mailbox.appointments.get_mut(&weekly).unwrap();
            a.recurrence = Some(Recurrence { rrule: "FREQ=WEEKLY;COUNT=4".into(), ..Default::default() });
            a.categories = vec!["Interview".into()];
        }

        // Today, list of events, full detail: private details hidden unless asked for.
        let r = s.run("insert.calendar", &json!({"range": "today", "detail": "full"})).unwrap();
        assert_eq!(r["appointments"], 3);
        let t = s.composes[&cid].body.to_plain_text();
        assert!(t.contains("Standup (Room 2) \u{2014} Agenda: budget"), "{t}");
        assert!(t.contains("Busy  Private") && !t.contains("Doctor"), "{t}");

        // Working hours only: the 6:00 run is outside 8:00-17:00.
        s.composes.get_mut(&cid).unwrap().body = mailcraft_model::Doc::default();
        let r = s
            .run("insert.calendar", &json!({"range": "today", "workingHours": true, "includePrivate": true}))
            .unwrap();
        assert_eq!(r["appointments"], 2);
        let t = s.composes[&cid].body.to_plain_text();
        assert!(!t.contains("Early run") && t.contains("Doctor") && t.contains("Working hours only"), "{t}");

        // Another calendar, next 7 days: the weekly item recurs on the 12th.
        s.composes.get_mut(&cid).unwrap().body = mailcraft_model::Doc::default();
        let r = s
            .run(
                "insert.calendar",
                &json!({"calendar": team.0, "range": "next7", "layout": "schedule", "detail": "full", "ics": true}),
            )
            .unwrap();
        assert_eq!(r["occurrences"], 1, "{r}");
        let c = &s.composes[&cid];
        let t = c.body.to_plain_text();
        assert!(t.starts_with("Team") && t.contains("Team sync") && t.contains("Free"), "{t}");
        assert!(c.body.blocks.iter().any(|b| matches!(b, Block::Table { .. })));
        let ics = c.attachments.iter().find(|a| a.filename.ends_with(".ics")).expect("ics attached");
        assert_eq!(ics.mime_type, "text/calendar");
        let text = String::from_utf8_lossy(&ics.data);
        assert!(text.contains("BEGIN:VCALENDAR") && text.contains("Team sync"), "{text}");
        // Only the occurrence in the range goes out, as a single event: no rule for the rest of the series.
        assert!(!text.contains("RRULE") && !text.contains("EXDATE"), "{text}");
        assert_eq!(text.matches("BEGIN:VEVENT").count(), 1, "{text}");
        assert!(text.contains("20261012"), "{text}");
        // Full detail keeps the categories.
        assert!(text.contains("CATEGORIES:Interview"), "{text}");
        // Availability only: no category names, no rule either.
        s.run(
            "insert.calendar",
            &json!({"calendar": team.0, "range": "next7", "detail": "availability", "ics": true}),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&s.composes[&cid].attachments.last().unwrap().data).into_owned();
        assert!(
            !text.contains("CATEGORIES") && !text.contains("Interview") && !text.contains("RRULE"),
            "{text}"
        );
        // A private item without its details: no categories, no reminder of the sender's.
        s.run("insert.calendar", &json!({"range": "today", "detail": "full", "ics": true})).unwrap();
        let text = String::from_utf8_lossy(&s.composes[&cid].attachments.last().unwrap().data).into_owned();
        assert!(text.contains("Standup") && !text.contains("Doctor"), "{text}");
        assert!(!text.contains("Medical") && !text.contains("VALARM"), "{text}");

        // Availability only: the attached file carries no subjects either.
        let r = s
            .run("insert.calendar", &json!({"range": "today", "detail": "availability", "ics": true}))
            .unwrap();
        assert_eq!(r["detail"], "availability");
        let last = s.composes[&cid].attachments.last().unwrap();
        let text = String::from_utf8_lossy(&last.data);
        assert!(!text.contains("Standup") && text.contains("Busy"), "{text}");

        for bad in [
            json!({"range": "fortnight"}),
            json!({"range": "today", "layout": "poster"}),
            json!({"range": "today", "detail": "secret"}),
            json!({"range": "today", "calendar": "Nope"}),
            json!({"from": "2026-01-01", "to": "2027-06-01"}),
        ] {
            assert!(s.run("insert.calendar", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn presets_cover_the_expected_days() {
        let today = mailcraft_model::dates::days_from_civil(2026, 10, 21).unwrap();
        assert_eq!(preset_days(today, "today"), Some((today, today)));
        assert_eq!(preset_days(today, "next7"), Some((today, today + 6)));
        let (a, b) = preset_days(today, "thisMonth").unwrap();
        assert_eq!(mailcraft_model::dates::civil_from_days(a), (2026, 10, 1));
        assert_eq!(mailcraft_model::dates::civil_from_days(b), (2026, 10, 31));
        let dec = mailcraft_model::dates::days_from_civil(2026, 12, 5).unwrap();
        let (a, b) = preset_days(dec, "nextMonth").unwrap();
        assert_eq!(mailcraft_model::dates::civil_from_days(a), (2027, 1, 1));
        assert_eq!(mailcraft_model::dates::civil_from_days(b), (2027, 1, 31));
        assert_eq!(preset_days(today, "fortnight"), None);
    }
}
