//! Calendar styles: Daily (one page per day: all-day strip, then a half-hour grid with appointment boxes in
//! side-by-side lanes), Weekly (one page per week, a band per day listing its appointments) and Monthly (one
//! page per month, a week-by-day grid with appointment lines in each cell).
//!
//! Days are civil day numbers (days since 1970-01-01, local time); minutes count from local midnight.

use mailcraft_fonts::FontDb;
use mailcraft_geom::{Color, Rect};
use mailcraft_layout::DisplayItem;
use mailcraft_model::dates::{civil_from_days, days_from_civil, days_in_month, weekday_of_day};

use super::text::{self, Style};
use super::{Frame, GRID, MAX_PAGES, SHADE, hrule, line};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CalendarStyle {
    #[default]
    Daily,
    Weekly,
    Monthly,
}

/// One appointment occurrence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalendarEvent {
    pub start_day: i64,
    /// Minutes after local midnight (0..1440).
    pub start_min: u32,
    pub end_day: i64,
    pub end_min: u32,
    pub all_day: bool,
    pub subject: String,
    pub location: String,
}

/// Calendar style content: the inclusive day range to print and the occurrences in it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalendarData {
    pub style: CalendarStyle,
    /// Calendar name printed under the title (may be empty).
    pub title: String,
    pub first_day: i64,
    pub last_day: i64,
    /// First day of the week, 0 = Sunday.
    pub week_start: u8,
    pub events: Vec<CalendarEvent>,
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
/// Most occurrences considered.
const MAX_EVENTS: usize = 10_000;
const DAY_MIN: u32 = 24 * 60;
/// Default visible hours of the daily grid (widened to fit the day's appointments).
const DAY_FROM_H: u32 = 7;
const DAY_TO_H: u32 = 19;
/// Most side-by-side lanes in the daily grid.
const MAX_LANES: usize = 8;

fn month_name(m: u8) -> &'static str {
    MONTHS.get(usize::from(m).saturating_sub(1)).copied().unwrap_or("")
}

/// "Monday, October 12, 2026".
pub(crate) fn long_date(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{}, {} {d}, {y}", weekday_of_day(day).name(), month_name(m))
}

/// "October 12".
fn month_day(day: i64) -> String {
    let (_, m, d) = civil_from_days(day);
    format!("{} {d}", month_name(m))
}

/// "9:30 AM".
fn clock(min: u32) -> String {
    let min = min.min(DAY_MIN);
    let (h, m) = ((min / 60) % 24, min % 60);
    let (h12, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{h12}:{m:02} {ampm}")
}

/// "8 AM".
fn hour_label(h: u32) -> String {
    let h = h % 24;
    match h {
        0 => "12 AM".into(),
        1..=11 => format!("{h} AM"),
        12 => "12 PM".into(),
        _ => format!("{} PM", h - 12),
    }
}

impl CalendarEvent {
    fn touches(&self, day: i64) -> bool {
        let end_day = if !self.all_day && self.end_min == 0 && self.end_day > self.start_day {
            self.end_day - 1
        } else if self.all_day && self.end_day > self.start_day {
            // All-day events end at midnight of the following day.
            self.end_day - 1
        } else {
            self.end_day
        };
        self.start_day <= day && day <= end_day.max(self.start_day)
    }

    /// Spans whole days on `day` (all-day, or covering 0:00–24:00 of a multi-day range).
    fn whole_day(&self, day: i64) -> bool {
        if self.all_day {
            return true;
        }
        let starts_before = self.start_day < day || (self.start_day == day && self.start_min == 0);
        let ends_after = self.end_day > day + 1 || (self.end_day == day + 1 && self.end_min == 0);
        starts_before && ends_after && self.end_day > self.start_day
    }

    /// Start/end minutes on `day`, clipped to the day.
    fn span_on(&self, day: i64) -> (u32, u32) {
        let a = if self.start_day < day { 0 } else { self.start_min.min(DAY_MIN) };
        let b = if self.end_day > day { DAY_MIN } else { self.end_min.min(DAY_MIN) };
        (a, b.max(a))
    }

    fn label(&self) -> String {
        let s = text::clean(&self.subject);
        let s = if s.trim().is_empty() { "(No subject)".to_owned() } else { s };
        if self.location.trim().is_empty() { s } else { format!("{s} ({})", text::clean(&self.location)) }
    }

    /// One-line agenda entry for `day`: "9:00 AM–10:00 AM Subject (Location)" or "All day Subject".
    fn agenda(&self, day: i64) -> String {
        if self.whole_day(day) {
            return format!("All day  {}", self.label());
        }
        let (a, b) = self.span_on(day);
        format!("{}–{}  {}", clock(a), clock(b), self.label())
    }

    fn sort_key(&self, day: i64) -> (bool, u32) {
        (!self.whole_day(day), self.span_on(day).0)
    }
}

fn day_events(c: &CalendarData, day: i64) -> Vec<&CalendarEvent> {
    let mut v: Vec<&CalendarEvent> = c.events.iter().take(MAX_EVENTS).filter(|e| e.touches(day)).collect();
    v.sort_by_key(|e| e.sort_key(day));
    v
}

fn range(c: &CalendarData) -> (i64, i64) {
    let a = c.first_day.clamp(-1_000_000, 1_000_000);
    let b = c.last_day.clamp(a, a.saturating_add(366 * 20));
    (a, b)
}

pub(crate) fn pages(c: &CalendarData, frame: &Frame, fonts: &FontDb) -> Vec<Vec<DisplayItem>> {
    let (a, b) = range(c);
    let mut out = Vec::new();
    match c.style {
        CalendarStyle::Daily => {
            let mut day = a;
            while day <= b && out.len() < MAX_PAGES {
                out.push(daily(c, day, frame, fonts));
                day += 1;
            }
        }
        CalendarStyle::Weekly => {
            let ws = i64::from(c.week_start % 7);
            let back = (i64::from(weekday_of_day(a).0) - ws).rem_euclid(7);
            let mut start = a - back;
            while start <= b && out.len() < MAX_PAGES {
                out.push(weekly(c, start, frame, fonts));
                start += 7;
            }
        }
        CalendarStyle::Monthly => {
            let (mut y, mut m, _) = civil_from_days(a);
            while let Some(first) = days_from_civil(y, m, 1) {
                if first > b || out.len() >= MAX_PAGES {
                    break;
                }
                out.push(monthly(c, y, m, frame, fonts));
                if m == 12 {
                    y += 1;
                    m = 1;
                } else {
                    m += 1;
                }
            }
        }
    }
    out
}

/// Title (and calendar name) at the top of the body; returns the y below it.
fn heading(items: &mut Vec<DisplayItem>, title: &str, sub: &str, frame: &Frame, fonts: &FontDb) -> f32 {
    let body = frame.body;
    let big = Style::new(fonts, 16.0, true);
    let small = Style::new(fonts, 9.0, false).color(Color::rgb(0x40, 0x40, 0x40));
    let mut y = body.y;
    items.push(big.text_at(fonts, body.x, y, &text::ellipsize(fonts, &big, title, body.w)));
    y += big.line_height(fonts);
    if !sub.trim().is_empty() {
        items.push(small.text_at(fonts, body.x, y, &text::ellipsize(fonts, &small, sub, body.w)));
        y += small.line_height(fonts);
    }
    items.push(hrule(body.x, body.x + body.w, y + 3.0, 2.0, Color::BLACK));
    y + 10.0
}

fn daily(c: &CalendarData, day: i64, frame: &Frame, fonts: &FontDb) -> Vec<DisplayItem> {
    let body = frame.body;
    let mut items = Vec::new();
    let mut y = heading(&mut items, &long_date(day), &c.title, frame, fonts);
    let small = Style::new(fonts, 8.0, false);
    let event = Style::new(fonts, 8.0, true);
    let lh = small.line_height(fonts);
    let gutter = 56.0f32.min(body.w / 4.0);
    let events = day_events(c, day);

    // All-day strip.
    let all_day: Vec<&&CalendarEvent> = events.iter().filter(|e| e.whole_day(day)).collect();
    if !all_day.is_empty() {
        let shown = all_day.len().min(6);
        let h = shown as f32 * (lh + 2.0) + 6.0;
        items.push(DisplayItem::FillRect { rect: Rect::new(body.x, y, body.w, h), color: SHADE });
        for (i, e) in all_day.iter().take(shown).enumerate() {
            let s = text::ellipsize(fonts, &event, &e.label(), body.w - gutter - 6.0);
            items.push(event.text_at(fonts, body.x + gutter, y + 3.0 + i as f32 * (lh + 2.0), &s));
        }
        y += h + 6.0;
    }

    // Time grid.
    let timed: Vec<&&CalendarEvent> = events.iter().filter(|e| !e.whole_day(day)).collect();
    let mut from = DAY_FROM_H * 60;
    let mut to = DAY_TO_H * 60;
    for e in &timed {
        let (a, b) = e.span_on(day);
        from = from.min(a / 60 * 60);
        to = to.max(b.div_ceil(60) * 60);
    }
    let to = to.clamp(from + 60, DAY_MIN);
    let slots = ((to - from) / 30).max(1);
    let grid_h = (body.y + body.h - y).max(lh * 2.0);
    let slot_h = grid_h / slots as f32;
    let per_min = slot_h / 30.0;
    let gx = body.x + gutter;
    let gw = (body.w - gutter).max(1.0);
    for k in 0..=slots {
        let yy = y + k as f32 * slot_h;
        let full = k % 2 == 0;
        items.push(line(
            (if full { body.x } else { gx }, yy),
            (body.x + body.w, yy),
            0.5,
            if full { GRID } else { SHADE },
        ));
        if full && k < slots {
            items.push(small.text_at(fonts, body.x, yy + 2.0, &hour_label((from + k * 30) / 60)));
        }
    }
    items.push(line((gx, y), (gx, y + slots as f32 * slot_h), 0.5, GRID));

    // Lanes: greedy, each event takes the first lane free at its start.
    let mut lanes: Vec<u32> = Vec::new();
    let mut placed: Vec<(usize, u32, u32)> = Vec::new();
    for e in &timed {
        let (a, b) = e.span_on(day);
        let b = b.max(a + 15);
        let lane = match lanes.iter().position(|end| *end <= a) {
            Some(l) => l,
            None if lanes.len() < MAX_LANES => {
                lanes.push(0);
                lanes.len() - 1
            }
            None => lanes.len() - 1,
        };
        if let Some(end) = lanes.get_mut(lane) {
            *end = b;
        }
        placed.push((lane, a, b));
    }
    let n_lanes = lanes.len().max(1) as f32;
    let lane_w = gw / n_lanes;
    for (e, (lane, a, b)) in timed.iter().zip(placed) {
        let top = y + (a.saturating_sub(from)) as f32 * per_min;
        let h = ((b - a) as f32 * per_min).max(lh + 2.0);
        let r = Rect::new(gx + lane as f32 * lane_w + 2.0, top + 1.0, (lane_w - 4.0).max(1.0), h - 2.0);
        items.push(DisplayItem::FillRect { rect: r, color: Color::rgb(0xf2, 0xf2, 0xf2) });
        items.push(DisplayItem::StrokeRect { rect: r, color: Color::rgb(0x50, 0x50, 0x50), width: 0.75 });
        let lines = ((r.h - 2.0) / lh).floor().max(1.0) as usize;
        let label = format!("{} {}", clock(a), e.label());
        for (i, l) in text::wrap(fonts, &event, &label, r.w - 6.0, lines).iter().enumerate() {
            items.push(event.text_at(fonts, r.x + 3.0, r.y + 1.0 + i as f32 * lh, l));
        }
    }
    items
}

fn weekly(c: &CalendarData, start: i64, frame: &Frame, fonts: &FontDb) -> Vec<DisplayItem> {
    let body = frame.body;
    let mut items = Vec::new();
    let (ys, _, _) = civil_from_days(start + 6);
    let title = format!("{} – {}, {ys}", month_day(start), month_day(start + 6));
    let y = heading(&mut items, &title, &c.title, frame, fonts);
    let head = Style::new(fonts, 10.0, true);
    let entry = Style::new(fonts, 9.0, false);
    let lh = entry.line_height(fonts);
    let row_h = (body.y + body.h - y) / 7.0;
    for i in 0..7 {
        let day = start + i;
        let top = y + i as f32 * row_h;
        let hh = head.line_height(fonts) + 4.0;
        items.push(DisplayItem::FillRect { rect: Rect::new(body.x, top, body.w, hh), color: SHADE });
        items.push(head.text_at(fonts, body.x + 4.0, top + 2.0, &long_date(day)));
        items.push(DisplayItem::StrokeRect {
            rect: Rect::new(body.x, top, body.w, row_h),
            color: GRID,
            width: 0.75,
        });
        let cap = ((row_h - hh - 4.0) / lh).floor().max(0.0) as usize;
        let events = day_events(c, day);
        let more = events.len() > cap;
        let show = if more { cap.saturating_sub(1) } else { events.len() };
        let mut ly = top + hh + 2.0;
        for e in events.iter().take(show) {
            items.push(entry.text_at(
                fonts,
                body.x + 8.0,
                ly,
                &text::ellipsize(fonts, &entry, &e.agenda(day), body.w - 16.0),
            ));
            ly += lh;
        }
        if more && cap > 0 {
            items.push(entry.text_at(fonts, body.x + 8.0, ly, &format!("+{} more", events.len() - show)));
        }
    }
    items
}

fn monthly(c: &CalendarData, y: i64, m: u8, frame: &Frame, fonts: &FontDb) -> Vec<DisplayItem> {
    let body = frame.body;
    let mut items = Vec::new();
    let first = days_from_civil(y, m, 1).unwrap_or(0);
    let last = first + i64::from(days_in_month(y, m)) - 1;
    let top = heading(&mut items, &format!("{} {y}", month_name(m)), &c.title, frame, fonts);
    let head = Style::new(fonts, 8.0, true);
    let num = Style::new(fonts, 8.0, true);
    let dim = Style::new(fonts, 8.0, false).color(Color::rgb(0x90, 0x90, 0x90));
    let entry = Style::new(fonts, 7.0, false);
    let lh = entry.line_height(fonts);
    let ws = i64::from(c.week_start % 7);
    let grid_start = first - (i64::from(weekday_of_day(first).0) - ws).rem_euclid(7);
    let weeks = ((last - grid_start) / 7 + 1).clamp(4, 6);
    let col_w = body.w / 7.0;
    let hh = head.line_height(fonts) + 4.0;
    items.push(DisplayItem::FillRect { rect: Rect::new(body.x, top, body.w, hh), color: SHADE });
    for i in 0..7 {
        let name = weekday_of_day(grid_start + i).name();
        let x = body.x + i as f32 * col_w;
        items.push(head.text_at(
            fonts,
            x + 4.0,
            top + 2.0,
            &text::ellipsize(fonts, &head, name, col_w - 8.0),
        ));
    }
    let gy = top + hh;
    let row_h = (body.y + body.h - gy) / weeks as f32;
    for w in 0..weeks {
        for i in 0..7 {
            let day = grid_start + w * 7 + i;
            let r = Rect::new(body.x + i as f32 * col_w, gy + w as f32 * row_h, col_w, row_h);
            items.push(DisplayItem::StrokeRect { rect: r, color: GRID, width: 0.5 });
            let (_, dm, dd) = civil_from_days(day);
            let st = if dm == m { &num } else { &dim };
            let label = if dd == 1 {
                format!("{} {dd}", month_name(dm).get(..3).unwrap_or(""))
            } else {
                dd.to_string()
            };
            items.push(st.text_at(fonts, r.x + 3.0, r.y + 2.0, &label));
            if day < c.first_day || day > c.last_day {
                continue;
            }
            let cap = ((r.h - st.line_height(fonts) - 4.0) / lh).floor().max(0.0) as usize;
            let events = day_events(c, day);
            let more = events.len() > cap;
            let show = if more { cap.saturating_sub(1) } else { events.len() };
            let mut ly = r.y + st.line_height(fonts) + 2.0;
            for e in events.iter().take(show) {
                let s = if e.whole_day(day) {
                    e.label()
                } else {
                    format!("{} {}", clock(e.span_on(day).0), e.label())
                };
                items.push(entry.text_at(
                    fonts,
                    r.x + 3.0,
                    ly,
                    &text::ellipsize(fonts, &entry, &s, r.w - 6.0),
                ));
                ly += lh;
            }
            if more && cap > 0 {
                items.push(entry.text_at(fonts, r.x + 3.0, ly, &format!("+{} more", events.len() - show)));
            }
        }
    }
    items
}
