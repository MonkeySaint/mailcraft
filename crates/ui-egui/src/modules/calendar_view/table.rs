//! View › Change View › List and Active: the calendar's items as a table (one row per appointment or
//! series), sorted by a column (click a header; again to reverse). Active shows items that have not ended,
//! and series with occurrences still to come.

use std::collections::BTreeSet;

use egui::{CornerRadius, FontId, Id, Rect, ScrollArea, Sense, Stroke, Ui, WidgetType, pos2, vec2};
use serde_json::json;

use mailcraft_engine::cmd::calendar::{
    CalendarColumn, CalendarCurrentView, describe_recurrence, occurrences, props,
};
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

/// Rows for the calendars `cals` (Active: only current and future items), sorted by `sort`.
pub(super) fn rows(
    app: &App,
    cals: &BTreeSet<FolderId>,
    current: CalendarCurrentView,
    sort: CalendarColumn,
    ascending: bool,
) -> Vec<Row> {
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
        .take(MAX_ROWS)
        .map(|a| Row {
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

/// Column widths as shares of the table width.
fn columns(several: bool) -> Vec<(CalendarColumn, f32)> {
    let mut v = vec![
        (CalendarColumn::Subject, 0.26),
        (CalendarColumn::Location, 0.14),
        (CalendarColumn::Start, 0.15),
        (CalendarColumn::End, 0.15),
        (CalendarColumn::Recurrence, 0.16),
        (CalendarColumn::Categories, 0.14),
    ];
    if several {
        v.push((CalendarColumn::Calendar, 0.12));
    }
    let total: f32 = v.iter().map(|(_, w)| w).sum();
    v.into_iter().map(|(c, w)| (c, w / total)).collect()
}

pub(super) fn table_view(
    ui: &mut Ui,
    app: &mut App,
    st: &mut ModState,
    cals: &BTreeSet<FolderId>,
    rect: Rect,
) {
    let list = rows(app, cals, st.cal.current, st.cal.sort, st.cal.ascending);
    let cols = columns(cals.len() > 1);
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
        let resp = ui.interact(r, Id::new(("mc-cal-table-head", c.name())), Sense::click());
        a11y(&resp, WidgetType::Button, &format!("Sort by {}", c.label()));
        if resp.hovered() {
            p.rect_filled(r, CornerRadius::ZERO, app.theme.hover);
        }
        let mut label = c.label().to_owned();
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
        let hint = if st.cal.current == CalendarCurrentView::Active {
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
            list.len(),
            |ui, range| {
                for i in range {
                    let Some(r) = list.get(i) else { continue };
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    a11y(
                        &resp,
                        WidgetType::Button,
                        &format!("{}, {}", r.subject, cell(r, CalendarColumn::Start)),
                    );
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
                    for (c, share) in &cols {
                        let w = width * share;
                        paint_text_clipped(
                            ui,
                            pos2(x + 4.0, rect.min.y + (row_h - 15.0) / 2.0),
                            &cell(r, *c),
                            FontId::proportional(12.5),
                            if *c == CalendarColumn::Subject { app.theme.text } else { app.theme.text_muted },
                            w - 8.0,
                        );
                        x += w;
                    }
                    ui.painter().hline(
                        rect.x_range(),
                        rect.max.y,
                        Stroke::new(1.0, super::blend(app.theme.separator, app.theme.reading, 0.5)),
                    );
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
        }
    }

    #[test]
    fn cells_and_columns() {
        let d = super::super::super::common::parse_day("2026-10-09").unwrap();
        let r = row(1, "Lunch", d * DAY + 12 * 3600);
        assert_eq!(cell(&r, CalendarColumn::Start), "Fri 10/9/2026 12:00 PM");
        let all = Row { all_day: true, end: (d + 1) * DAY, start: d * DAY, ..r.clone() };
        assert_eq!(cell(&all, CalendarColumn::End), "Fri 10/9/2026");
        let total: f32 = columns(true).iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1e-4);
        assert_eq!(columns(false).len(), 6);
    }
}
