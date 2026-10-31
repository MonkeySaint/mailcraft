//! View › Change View › List and Active: the calendar's items as a table (one row per appointment or
//! series), sorted by a column (click a header; again to reverse). Active shows items that have not ended,
//! and series with occurrences still to come. View Settings picks the columns and their header labels,
//! groups the rows by a column, filters them, draws or hides grid lines and applies Conditional
//! Formatting.

use std::collections::BTreeSet;

use egui::{CornerRadius, FontId, Id, Rect, Role, ScrollArea, Sense, Stroke, Ui, pos2, vec2};
use serde_json::json;

use mailcraft_engine::cmd::calendar::{
    CalendarColumn, CalendarCurrentView, describe_recurrence, occurrences, props,
};
use mailcraft_model::calendar::CalendarViewState;
use mailcraft_model::view::{FilterText, ItemStyle, rule_style, rules_active, rules_need_body};
use mailcraft_model::{Appointment, DateTime, FolderId, ItemId};

use super::super::common::{DAY, ModState, a11y, clock, empty_message, paint_text_clipped, short_date};
use super::super::editors;
use super::{calendar_color32, run_view_cmd};
use crate::App;

const HEAD_H: f32 = 26.0;
const ROW_H: f32 = 24.0;
const ROW_H_TIGHT: f32 = 20.0;
/// Most rows listed.
const MAX_ROWS: usize = 20_000;

/// One table row.
#[derive(Clone, Debug)]
pub(super) struct Row {
    pub id: ItemId,
    pub folder: FolderId,
    pub subject: String,
    pub location: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub recurrence: String,
    pub categories: String,
    pub calendar: String,
    pub read_only: bool,
    /// Conditional Formatting: bold, italic, strikethrough, colour ("#rrggbb").
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub color: Option<String>,
    /// Group By header text (empty when the table is not grouped).
    pub group: String,
}

/// True when `a` has not ended by `now` (unix), or is a series with an occurrence still to come.
fn active(a: &Appointment, now: i64) -> bool {
    if a.end.unix >= now {
        return true;
    }
    if a.recurrence.is_none() {
        return false;
    }
    let from = DateTime::from_unix(now, 0);
    let to = DateTime::from_unix(now.saturating_add(10 * 366 * DAY), 0);
    !occurrences(a, from, to, 1).is_empty()
}

/// Rows for the calendars `cals` as view `v` lists them: Active only current and future items; the View
/// Settings filter applied; sorted by `v.sort`; grouped by `v.group_by` (groups in A to Z order, the sort
/// kept inside each); Conditional Formatting worked out.
pub(super) fn rows(app: &App, cals: &BTreeSet<FolderId>, v: &CalendarViewState) -> Vec<Row> {
    let (current, sort, ascending) = (v.current, v.sort, v.ascending);
    let filter = (!v.filter.is_empty()).then_some(&v.filter);
    // Each item's text is lowercased once for the filter and every rule, and only when one needs it.
    let want_text = filter.is_some() || rules_active(&v.rules);
    let want_body = filter.is_some_and(|f| f.needs_body()) || rules_need_body(&v.rules);
    let clock = super::cal_clock(app);
    let now = app.s.now().unix;
    // Per-calendar details, looked up once.
    let info: std::collections::BTreeMap<FolderId, (String, bool)> = cals
        .iter()
        .map(|f| {
            let name = app.s.mailbox.folder(*f).map(|x| x.name.clone()).unwrap_or_default();
            (*f, (name, props::is_read_only(&app.s.mailbox, *f)))
        })
        .collect();
    let mut out: Vec<Row> = app
        .s
        .mailbox
        .appointments
        .values()
        .filter(|a| cals.contains(&a.folder))
        .filter(|a| current != CalendarCurrentView::Active || active(a, now))
        .filter_map(|a| {
            let text = want_text.then(|| FilterText::new(&a.subject, &a.body, want_body));
            if let (Some(f), Some(t)) = (filter, &text)
                && !f.matches_text(t, &a.categories, a.start.unix, now)
            {
                return None;
            }
            Some((a, text))
        })
        .take(MAX_ROWS)
        .map(|(a, text)| {
            let style = match &text {
                Some(t) => rule_style(&v.rules, |f| f.matches_text(t, &a.categories, a.start.unix, now)),
                None => ItemStyle::default(),
            };
            Row {
                id: a.id,
                folder: a.folder,
                subject: if a.subject.trim().is_empty() {
                    "(No subject)".into()
                } else {
                    a.subject.trim().to_owned()
                },
                location: a.location.trim().to_owned(),
                start: clock.local(a.start.unix),
                end: clock.local(a.end.unix),
                all_day: a.all_day,
                recurrence: if a.recurrence.is_some() {
                    describe_recurrence(a, clock.offset_at(a.start.unix)).unwrap_or_default()
                } else {
                    String::new()
                },
                categories: a.categories.join(", "),
                calendar: info.get(&a.folder).map(|x| x.0.clone()).unwrap_or_default(),
                read_only: info.get(&a.folder).is_some_and(|x| x.1),
                bold: style.bold,
                italic: style.italic,
                strike: style.strike,
                color: style.color,
                group: String::new(),
            }
        })
        .collect();
    let text = |r: &Row| -> String {
        match sort {
            CalendarColumn::Subject => r.subject.to_lowercase(),
            CalendarColumn::Location => r.location.to_lowercase(),
            CalendarColumn::Recurrence => r.recurrence.to_lowercase(),
            CalendarColumn::Categories => r.categories.to_lowercase(),
            CalendarColumn::Calendar => r.calendar.to_lowercase(),
            CalendarColumn::Start | CalendarColumn::End => String::new(),
        }
    };
    out.sort_by(|a, b| {
        let ord = match sort {
            CalendarColumn::Start => a.start.cmp(&b.start),
            CalendarColumn::End => a.end.cmp(&b.end),
            _ => text(a).cmp(&text(b)),
        };
        let ord = ord.then(a.start.cmp(&b.start)).then(a.id.cmp(&b.id));
        if ascending { ord } else { ord.reverse() }
    });
    if let Some(g) = v.group_by {
        for r in &mut out {
            let text = match g {
                CalendarColumn::Start | CalendarColumn::End => {
                    let local = if g == CalendarColumn::Start { r.start } else { r.end };
                    when(local, true)
                }
                _ => cell(r, g),
            };
            r.group =
                format!("{}: {}", v.column_label(g), if text.trim().is_empty() { "(none)" } else { &text });
        }
        // Stable: the sort order stays inside each group.
        out.sort_by_key(|r| r.group.to_lowercase());
    }
    out
}

/// "Fri 10/9/2026 9:00 AM" (all-day: the date only).
fn when(local: i64, all_day: bool) -> String {
    let day = local.div_euclid(DAY);
    let name = super::super::common::weekday_short(day);
    if all_day {
        format!("{name} {}", short_date(day))
    } else {
        format!("{name} {} {}", short_date(day), clock(local.rem_euclid(DAY) / 60))
    }
}

fn cell(r: &Row, c: CalendarColumn) -> String {
    match c {
        CalendarColumn::Subject => r.subject.clone(),
        CalendarColumn::Location => r.location.clone(),
        CalendarColumn::Start => when(r.start, r.all_day),
        CalendarColumn::End => when(if r.all_day { r.end - 1 } else { r.end }, r.all_day),
        CalendarColumn::Recurrence => r.recurrence.clone(),
        CalendarColumn::Categories => r.categories.clone(),
        CalendarColumn::Calendar => r.calendar.clone(),
    }
}

/// Share of the table width a column takes before scaling.
fn share(c: CalendarColumn) -> f32 {
    match c {
        CalendarColumn::Subject => 0.26,
        CalendarColumn::Location | CalendarColumn::Categories => 0.14,
        CalendarColumn::Start | CalendarColumn::End => 0.15,
        CalendarColumn::Recurrence => 0.16,
        CalendarColumn::Calendar => 0.12,
    }
}

/// The view's columns with their widths as shares of the table width; the Calendar column joins the
/// default columns when several calendars are listed.
fn columns(v: &CalendarViewState, several: bool) -> Vec<(CalendarColumn, f32)> {
    let mut cols = v.table_columns();
    if several && v.columns.is_empty() && !cols.contains(&CalendarColumn::Calendar) {
        cols.push(CalendarColumn::Calendar);
    }
    let total: f32 = cols.iter().map(|c| share(*c)).sum::<f32>().max(0.01);
    cols.into_iter().map(|c| (c, share(c) / total)).collect()
}

/// One line of the table: a group header or an item row (index into the rows).
#[derive(Clone, Debug, PartialEq)]
enum Line {
    Group(String, usize),
    Item(usize),
}

/// Lines of the table: with groups, a header before the first row of each group, with the number of rows
/// under it. One pass: [`rows`] keeps each group's rows together.
fn lines(rows: &[Row]) -> Vec<Line> {
    let mut out = Vec::with_capacity(rows.len());
    let mut last: Option<&str> = None;
    // Index in `out` of the current group's header.
    let mut header: Option<usize> = None;
    for (i, r) in rows.iter().enumerate() {
        if r.group.is_empty() {
            header = None;
            last = None;
        } else if last != Some(r.group.as_str()) {
            header = Some(out.len());
            out.push(Line::Group(r.group.clone(), 0));
            last = Some(r.group.as_str());
        }
        if let Some(Line::Group(_, n)) = header.and_then(|h| out.get_mut(h)) {
            *n = n.saturating_add(1);
        }
        out.push(Line::Item(i));
    }
    out
}

/// "#rrggbb" as a colour.
fn hex_color(c: &str) -> Option<egui::Color32> {
    let h = c.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    let [_, r, g, b] = v.to_be_bytes();
    Some(egui::Color32::from_rgb(r, g, b))
}

pub(super) fn table_view(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    cals: &BTreeSet<FolderId>,
    rect: Rect,
) {
    let list = rows(app, cals, &st.cal);
    let shown = lines(&list);
    let cols = columns(&st.cal, cals.len() > 1);
    let p = ui.painter().clone();
    let line = Stroke::new(1.0, app.theme.separator);
    let width = rect.width() - 14.0;
    // Header.
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), HEAD_H));
    p.rect_filled(head, CornerRadius::ZERO, super::blend(app.theme.reading, app.theme.backdrop, 0.3));
    let mut x = rect.min.x + 10.0;
    for (c, share) in &cols {
        let w = width * share;
        let r = Rect::from_min_size(pos2(x, head.min.y), vec2(w, HEAD_H));
        let resp = ui.interact(r, Id::unique(("mc-cal-table-head", c.name())), Sense::click());
        a11y(&resp, Role::Button, &format!("Sort by {}", c.label()));
        if resp.hovered() {
            p.rect_filled(r, CornerRadius::ZERO, app.theme.hover);
        }
        let mut label = st.cal.column_label(*c);
        if st.cal.sort == *c {
            label.push_str(if st.cal.ascending { " ▲" } else { " ▼" });
        }
        paint_text_clipped(
            ui,
            pos2(r.min.x + 4.0, r.min.y + 5.0),
            &label,
            FontId::proportional(12.5),
            app.theme.text,
            w - 8.0,
        );
        p.vline(r.max.x, r.y_range(), line);
        if resp.clicked() {
            run_view_cmd(app, st, "view.arrangeBy", json!({ "key": c.name() }));
        }
        x += w;
    }
    p.hline(rect.x_range(), head.max.y, line);
    let body = Rect::from_min_max(pos2(rect.min.x, head.max.y + 1.0), rect.max);
    if list.is_empty() {
        let hint = if !st.cal.filter.is_empty() {
            "No item meets the View Settings filter."
        } else if st.cal.current == CalendarCurrentView::Active {
            "Nothing is coming up in the calendars shown."
        } else {
            "The calendars shown have no items."
        };
        empty_message(ui, body, app, "No items", hint);
        return;
    }
    let row_h = if st.cal.tighter_spacing { ROW_H_TIGHT } else { ROW_H };
    let mut builder = egui::UiBuilder::new().max_rect(body).id_salt("mc-cal-table-body");
    builder = builder.layout(egui::Layout::top_down(egui::Align::Min));
    ui.scope_builder(builder, |ui| {
        ui.set_clip_rect(ui.clip_rect().intersect(body));
        ScrollArea::vertical().id_salt("mc-cal-table-scroll").auto_shrink([false, false]).show_rows(
            ui,
            row_h,
            shown.len(),
            |ui, range| {
                for k in range {
                    let i = match shown.get(k) {
                        Some(Line::Item(i)) => *i,
                        Some(Line::Group(label, count)) => {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                            paint_text_clipped(
                                ui,
                                pos2(rect.min.x + 8.0, rect.min.y + (row_h - 15.0) / 2.0),
                                &format!("{label} ({count})"),
                                FontId::proportional(12.5),
                                app.theme.accent,
                                rect.width() - 16.0,
                            );
                            ui.painter().hline(
                                rect.x_range(),
                                rect.max.y,
                                Stroke::new(1.0, app.theme.accent),
                            );
                            continue;
                        }
                        None => continue,
                    };
                    let Some(r) = list.get(i) else { continue };
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    a11y(&resp, Role::Button, &format!("{}, {}", r.subject, cell(r, CalendarColumn::Start)));
                    let selected = app.ui.module_selection == Some(r.id.0);
                    let p = ui.painter();
                    if selected {
                        p.rect_filled(rect, CornerRadius::ZERO, app.theme.selected);
                    } else if resp.hovered() {
                        p.rect_filled(rect, CornerRadius::ZERO, app.theme.hover);
                    }
                    p.rect_filled(
                        Rect::from_min_size(pos2(rect.min.x + 2.0, rect.min.y + 4.0), vec2(4.0, row_h - 8.0)),
                        CornerRadius::same(2),
                        calendar_color32(app, r.folder),
                    );
                    let mut x = rect.min.x + 10.0;
                    let ruled = r.color.as_deref().and_then(hex_color);
                    for (c, share) in &cols {
                        let w = width * share;
                        let color = ruled.unwrap_or(if *c == CalendarColumn::Subject {
                            app.theme.text
                        } else {
                            app.theme.text_muted
                        });
                        let mut job = egui::text::LayoutJob::single_section(
                            cell(r, *c),
                            egui::TextFormat {
                                font_id: FontId::proportional(12.5),
                                color,
                                italics: r.italic,
                                strikethrough: if r.strike { Stroke::new(1.0, color) } else { Stroke::NONE },
                                ..Default::default()
                            },
                        );
                        job.wrap = egui::text::TextWrapping {
                            max_width: (w - 8.0).max(1.0),
                            max_rows: 1,
                            break_anywhere: true,
                            overflow_character: Some('…'),
                        };
                        let galley = ui.painter().layout_job(job);
                        let at = pos2(x + 4.0, rect.min.y + (row_h - 15.0) / 2.0);
                        ui.painter().galley(at, galley.clone(), color);
                        if r.bold {
                            // Faux bold: the text again, a little to the right.
                            ui.painter().galley(at + vec2(0.6, 0.0), galley, color);
                        }
                        x += w;
                    }
                    if !st.cal.no_grid {
                        ui.painter().hline(
                            rect.x_range(),
                            rect.max.y,
                            Stroke::new(1.0, super::blend(app.theme.separator, app.theme.reading, 0.5)),
                        );
                    }
                    if resp.clicked() {
                        app.ui.module_selection = Some(r.id.0);
                    }
                    if resp.double_clicked() {
                        editors::open_appointment(app, st, r.id);
                    }
                    let (id, read_only) = (r.id, r.read_only);
                    resp.context_menu(|ui| {
                        if ui.button("Open").clicked() {
                            editors::open_appointment(app, st, id);
                            ui.close();
                        }
                        if !read_only && ui.button("Delete").clicked() {
                            app.run("calendar.delete", json!({ "id": id.0 }));
                            ui.close();
                        }
                    });
                }
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u64, subject: &str, start: i64) -> Row {
        Row {
            id: ItemId(id),
            folder: FolderId(1),
            subject: subject.into(),
            location: String::new(),
            start,
            end: start + 3600,
            all_day: false,
            recurrence: String::new(),
            categories: String::new(),
            calendar: String::new(),
            read_only: false,
            bold: false,
            italic: false,
            strike: false,
            color: None,
            group: String::new(),
        }
    }

    #[test]
    fn cells_and_columns() {
        let d = super::super::super::common::parse_day("2026-10-09").unwrap();
        let r = row(1, "Lunch", d * DAY + 12 * 3600);
        assert_eq!(cell(&r, CalendarColumn::Start), "Fri 10/9/2026 12:00 PM");
        let all = Row { all_day: true, end: (d + 1) * DAY, start: d * DAY, ..r.clone() };
        assert_eq!(cell(&all, CalendarColumn::End), "Fri 10/9/2026");
        let v = CalendarViewState::default();
        let total: f32 = columns(&v, true).iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1e-4);
        assert_eq!(columns(&v, false).len(), 6);
        assert_eq!(columns(&v, true).len(), 7);
        // Chosen columns are kept as they are (no Calendar column added).
        let picked = CalendarViewState {
            columns: vec![CalendarColumn::Subject, CalendarColumn::Start],
            ..CalendarViewState::default()
        };
        assert_eq!(columns(&picked, true).iter().map(|(c, _)| *c).collect::<Vec<_>>(), picked.columns);
    }

    #[test]
    fn group_headers_come_before_each_group() {
        let mut a = row(1, "A", 0);
        a.group = "Location: Room 1".into();
        let mut b = row(2, "B", 10);
        b.group = "Location: Room 1".into();
        let mut c = row(3, "C", 20);
        c.group = "Location: Room 2".into();
        let l = lines(&[a, b, c]);
        assert_eq!(
            l,
            vec![
                Line::Group("Location: Room 1".into(), 2),
                Line::Item(0),
                Line::Item(1),
                Line::Group("Location: Room 2".into(), 1),
                Line::Item(2),
            ]
        );
        assert_eq!(lines(&[row(4, "D", 0)]), vec![Line::Item(0)]);
        assert_eq!(hex_color("#ff0000"), Some(egui::Color32::from_rgb(255, 0, 0)));
        assert_eq!(hex_color("red"), None);
    }

    /// Regression: each header used to count its rows by scanning every row (groups x rows per frame).
    #[test]
    fn many_unique_groups_are_counted_in_one_pass() {
        let rows: Vec<Row> =
            (0..20_000u64).map(|i| Row { group: format!("Subject: {i:05}"), ..row(i, "x", 0) }).collect();
        let started = std::time::Instant::now();
        let l = lines(&rows);
        assert_eq!(l.len(), 40_000);
        assert!(l.iter().all(|x| !matches!(x, Line::Group(_, n) if *n != 1)));
        assert!(started.elapsed() < std::time::Duration::from_secs(8), "{:?}", started.elapsed());
    }
}
