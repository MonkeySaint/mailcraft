//! Send a Calendar via E-mail: the shared events rendered into the message body as HTML, next to the
//! iCalendar attachment, so recipients without a calendar program can read them.
//!
//! Two layouts: **Daily schedule** (a table per day: all-day events first, then the timed events in order with
//! the free time between them inside the working hours) and **List of events** (every event under its day,
//! with its time, location and notes). "Only times inside my work hours" leaves out timed events
//! that do not touch the working hours (File › Options › Calendar › Work hours). The events come already cut
//! down to the chosen detail level (`share_items`).

use mailcraft_model::{Appointment, DateTime, ListTime};

use super::pim::DAY_SECS;
use super::{occurrences, tz};

/// Most event occurrences rendered into one message.
pub const MAX_EVENTS: usize = 1_000;
/// Longest span rendered (days); a whole calendar with endless series is cut there.
pub const MAX_DAYS: i64 = 366;
/// Ranges up to this many days list every day in the daily schedule (longer ones only days with events).
const EVERY_DAY_UP_TO: i64 = 31;

/// How the events are laid out in the message body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    DailySchedule,
    ListOfEvents,
}

impl Layout {
    pub fn parse(s: &str) -> Option<Layout> {
        match s {
            "dailySchedule" | "daily" => Some(Layout::DailySchedule),
            "listOfEvents" | "list" => Some(Layout::ListOfEvents),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Layout::DailySchedule => "dailySchedule",
            Layout::ListOfEvents => "listOfEvents",
        }
    }
}

/// Rendering choices (the Share dialog's Advanced options).
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub layout: Layout,
    /// Leave out timed events outside the working hours.
    pub working_hours_only: bool,
    /// Working hours (minutes after midnight, start before end).
    pub work: (i64, i64),
}

/// One occurrence to render.
struct Occ<'a> {
    start: DateTime,
    end: DateTime,
    a: &'a Appointment,
}

pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

/// Plain notes as HTML lines.
fn notes_html(text: &str) -> String {
    let t: String = text.trim().chars().take(4_000).collect();
    escape(&t).replace('\n', "<br>")
}

fn time(clock: &tz::Clock, t: DateTime) -> String {
    ListTime::clock(t, clock.offset_at(t.unix))
}

fn day_title(clock: &tz::Clock, day: i64) -> String {
    let t = clock.from_local(day.saturating_mul(DAY_SECS));
    ListTime::long_date(t, clock.offset_at(t.unix))
}

/// The occurrences of `appts` in `window` (for the whole calendar: from the first event on, up to
/// [`MAX_DAYS`]), in time order.
fn collect<'a>(appts: &'a [Appointment], window: Option<(DateTime, DateTime)>) -> (Vec<Occ<'a>>, bool) {
    let (from, to) = match window {
        Some(w) => w,
        None => {
            let first = appts.iter().map(|a| a.start.unix).min().unwrap_or(0);
            let last = appts.iter().map(|a| a.end.unix).max().unwrap_or(first).max(first);
            let cap = first.saturating_add(MAX_DAYS.saturating_mul(DAY_SECS));
            (DateTime::from_unix(first, 0), DateTime::from_unix(last.min(cap).saturating_add(1), 0))
        }
    };
    let to = DateTime::from_unix(to.unix.min(from.unix.saturating_add(MAX_DAYS.saturating_mul(DAY_SECS))), 0);
    let mut out: Vec<Occ<'a>> = Vec::new();
    let mut cut = false;
    let order = |o: &Occ<'_>| (o.start.unix, !o.a.all_day, o.a.id);
    for a in appts {
        for (start, end) in occurrences(a, from, to, MAX_EVENTS.saturating_add(1)) {
            out.push(Occ { start, end, a });
        }
        // Bounded: every appointment is looked at, keeping the earliest occurrences seen so far (cutting in
        // item order would drop earlier events of items further on).
        if out.len() > MAX_EVENTS.saturating_mul(4) {
            out.sort_by_key(order);
            out.truncate(MAX_EVENTS);
            cut = true;
        }
    }
    out.sort_by_key(order);
    if out.len() > MAX_EVENTS {
        out.truncate(MAX_EVENTS);
        cut = true;
    }
    (out, cut)
}

/// True when a timed occurrence touches the working hours of its day.
fn in_work_hours(clock: &tz::Clock, o: &Occ<'_>, work: (i64, i64)) -> bool {
    if o.a.all_day {
        return true;
    }
    let day = clock.day(o.start);
    let mut d = day;
    // A long event may cover the working hours of a later day.
    while d <= clock.day(o.end).min(day.saturating_add(MAX_DAYS)) {
        let w0 = clock.from_local(d.saturating_mul(DAY_SECS).saturating_add(work.0.saturating_mul(60)));
        let w1 = clock.from_local(d.saturating_mul(DAY_SECS).saturating_add(work.1.saturating_mul(60)));
        if o.start.unix < w1.unix && o.end.unix > w0.unix {
            return true;
        }
        d = d.saturating_add(1);
    }
    false
}

fn when(clock: &tz::Clock, o: &Occ<'_>) -> String {
    if o.a.all_day {
        let days = o.end.unix.saturating_sub(o.start.unix).div_euclid(DAY_SECS);
        return if days > 1 { format!("All day ({days} days)") } else { "All day".to_owned() };
    }
    if clock.day(o.start) == clock.day(o.end) || o.end.unix == o.start.unix {
        format!("{} - {}", time(clock, o.start), time(clock, o.end))
    } else {
        format!("{} - {} {}", time(clock, o.start), day_title(clock, clock.day(o.end)), time(clock, o.end))
    }
}

fn event_cell(o: &Occ<'_>, detail_notes: bool) -> String {
    let a = o.a;
    let mut cell =
        format!("<b>{}</b>", escape(if a.subject.trim().is_empty() { "(No subject)" } else { &a.subject }));
    if !a.location.trim().is_empty() {
        cell.push_str(&format!(" ({})", escape(a.location.trim())));
    }
    if detail_notes && !a.body.trim().is_empty() {
        cell.push_str(&format!("<br>{}", notes_html(&a.body)));
    }
    cell
}

/// The message body for the shared events: a heading and the events in `opts.layout`.
pub fn agenda_html(
    title: &str,
    appts: &[Appointment],
    window: Option<(DateTime, DateTime)>,
    clock: &tz::Clock,
    opts: Options,
) -> String {
    let (all, cut) = collect(appts, window);
    let occs: Vec<Occ<'_>> =
        all.into_iter().filter(|o| !opts.working_hours_only || in_work_hours(clock, o, opts.work)).collect();
    let mut html = format!("<h2>{}</h2>", escape(title));
    let range = match window {
        Some((f, t)) => {
            let last = clock.day(DateTime::from_unix(t.unix.saturating_sub(1).max(f.unix), 0));
            let first = clock.day(f);
            if first == last {
                day_title(clock, first)
            } else {
                format!("{} - {}", day_title(clock, first), day_title(clock, last))
            }
        }
        None => "All events".to_owned(),
    };
    html.push_str(&format!("<p>{}</p>", escape(&range)));
    if opts.working_hours_only {
        let w0 = clock.from_local(opts.work.0.saturating_mul(60));
        let w1 = clock.from_local(opts.work.1.saturating_mul(60));
        html.push_str(&format!(
            "<p><i>Showing time within working hours only ({} - {}).</i></p>",
            ListTime::clock(w0, clock.offset_at(w0.unix)),
            ListTime::clock(w1, clock.offset_at(w1.unix))
        ));
    }
    // Days to show: every day of a short range (daily schedule), else the days with events.
    let mut days: Vec<i64> = occs.iter().map(|o| clock.day(o.start)).collect();
    if let (Layout::DailySchedule, Some((f, t))) = (opts.layout, window) {
        let first = clock.day(f);
        let last = clock.day(DateTime::from_unix(t.unix.saturating_sub(1).max(f.unix), 0));
        if last.saturating_sub(first) < EVERY_DAY_UP_TO {
            days.extend(first..=last);
        }
    }
    days.sort_unstable();
    days.dedup();
    if days.is_empty() {
        html.push_str("<p>No events.</p>");
    }
    for day in days {
        let today: Vec<&Occ<'_>> = occs.iter().filter(|o| clock.day(o.start) == day).collect();
        html.push_str(&format!("<h3>{}</h3>", escape(&day_title(clock, day))));
        match opts.layout {
            Layout::DailySchedule => html.push_str(&daily_table(clock, day, &today, opts)),
            Layout::ListOfEvents => {
                html.push_str("<ul>");
                for o in &today {
                    html.push_str(&format!("<li>{}: {}</li>", escape(&when(clock, o)), event_cell(o, true)));
                }
                html.push_str("</ul>");
            }
        }
    }
    if cut {
        html.push_str(&format!(
            "<p><i>Only the first {MAX_EVENTS} events are listed; the attached calendar holds them all.</i></p>"
        ));
    }
    html
}

/// One day of the daily schedule: all-day events, then the timed ones with the free time between them
/// inside the working hours.
fn daily_table(clock: &tz::Clock, day: i64, today: &[&Occ<'_>], opts: Options) -> String {
    let at = |min: i64| clock.from_local(day.saturating_mul(DAY_SECS).saturating_add(min.saturating_mul(60)));
    let (w0, w1) = (at(opts.work.0), at(opts.work.1));
    let mut rows: Vec<(String, String)> = Vec::new();
    for o in today.iter().filter(|o| o.a.all_day) {
        rows.push((when(clock, o), event_cell(o, false)));
    }
    let free = |rows: &mut Vec<(String, String)>, from: i64, to: i64| {
        let (from, to) = (from.max(w0.unix), to.min(w1.unix));
        if to > from {
            let (f, t) = (DateTime::from_unix(from, 0), DateTime::from_unix(to, 0));
            rows.push((format!("{} - {}", time(clock, f), time(clock, t)), "<i>Free</i>".to_owned()));
        }
    };
    // Free time runs from the start of the working day to the first event, between events, and to the end.
    let mut busy_until = w0.unix;
    for o in today.iter().filter(|o| !o.a.all_day) {
        free(&mut rows, busy_until, o.start.unix);
        rows.push((when(clock, o), event_cell(o, false)));
        busy_until = busy_until.max(o.end.unix);
    }
    free(&mut rows, busy_until, w1.unix);
    if rows.is_empty() {
        return "<p>No events.</p>".to_owned();
    }
    let mut t = String::from("<table border=\"1\" cellpadding=\"4\" cellspacing=\"0\">");
    for (when, what) in rows {
        t.push_str(&format!("<tr><td>{}</td><td>{what}</td></tr>", escape(&when)));
    }
    t.push_str("</table>");
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::ItemId;

    #[test]
    fn the_cut_keeps_the_earliest_events_whatever_the_item_order() {
        // Regression: past 4000 occurrences the list stopped in item order, so earlier events of items further
        // on were dropped while "the first 1000 events" were said to be listed.
        let base = 1_790_000_000i64;
        let n = MAX_EVENTS * 4 + 200;
        let appts: Vec<Appointment> = (1..=n)
            .map(|i| {
                let start = base + i64::try_from(n - i).unwrap() * 60;
                Appointment {
                    id: ItemId(u64::try_from(i).unwrap()),
                    start: DateTime::from_unix(start, 0),
                    end: DateTime::from_unix(start + 60, 0),
                    ..Default::default()
                }
            })
            .collect();
        let (list, cut) = collect(&appts, None);
        assert!(cut);
        assert_eq!(list.len(), MAX_EVENTS);
        assert_eq!(list[0].a.id, ItemId(u64::try_from(n).unwrap()), "the earliest event comes first");
        assert!(list.windows(2).all(|w| w[0].start.unix <= w[1].start.unix));
        assert_eq!(list[MAX_EVENTS - 1].start.unix, base + i64::try_from(MAX_EVENTS - 1).unwrap() * 60);
    }
}
