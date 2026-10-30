//! A small Scheduling Assistant for the meeting editor: one working day as a grid of half-hour slots with a
//! row for the user (busy blocks from their own calendars, `calendar::free_busy`) and one per attendee. With
//! an Exchange account the attendees' busy times come from its server (what `calendar.freeBusy` with
//! `attendees` asks, GetUserAvailability), looked up off the UI thread once per day shown; without one, or
//! for an address the server knows nothing about, the row is hatched. The meeting time is a band over the
//! grid; clicking a slot moves the meeting there (keeping its length) and "Next free time" finds the next
//! slot of that length that is free on the user's calendar.
//!
//! Options › Calendar › Scheduling assistant › "Show calendar details in the scheduling grid": on (default),
//! each busy block shows its subject (and reads "subject, time, show-as" to assistive technology); off, the
//! blocks show only the busy time, and conflicts are counted instead of named.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use serde_json::Value;

use mailcraft_engine::cmd::calendar::{BusySlot, free_busy};
use mailcraft_model::{DateTime, ItemId};

use super::common::{DAY, a11y, blend, day_string, hour_label, long_date, parse_day};
use super::editors::ApptEditor;
use crate::App;

/// First and last hour shown.
const FIRST_HOUR: i64 = 7;
const LAST_HOUR: i64 = 19;
const SLOT_W: f32 = 22.0;
const ROW_H: f32 = 24.0;
const NAME_W: f32 = 170.0;
/// Days searched by "Next free time".
const SEARCH_DAYS: i64 = 21;

fn day_bounds(app: &App, day: i64) -> (DateTime, DateTime) {
    let tz = app.s.tz_offset_min;
    let shift = i64::from(tz).saturating_mul(60);
    let from = DateTime::from_unix(day.saturating_mul(DAY).saturating_sub(shift), tz);
    let to = DateTime::from_unix(day.saturating_add(1).saturating_mul(DAY).saturating_sub(shift), tz);
    (from, to)
}

/// Busy blocks of the user's calendars on `day` (local minutes), the edited appointment left out.
pub(crate) fn busy_minutes(app: &App, day: i64, exclude: Option<u64>) -> Vec<(i64, i64, BusySlot)> {
    let tz = i64::from(app.s.tz_offset_min).saturating_mul(60);
    let (from, to) = day_bounds(app, day);
    let base = day.saturating_mul(DAY);
    free_busy(&app.s, from, to, exclude.map(ItemId), None)
        .into_iter()
        .map(|b| {
            let s = (b.start.saturating_add(tz).saturating_sub(base) / 60).clamp(0, 24 * 60);
            let e = (b.end.saturating_add(tz).saturating_sub(base) / 60).clamp(0, 24 * 60);
            (s, e.max(s), b)
        })
        .collect()
}

/// The attendees' busy times for one day, asked of the Exchange server on a thread of its own.
#[derive(Clone, Default)]
pub(crate) struct AttendeeFreeBusy {
    /// The day and the addresses asked about.
    key: (i64, Vec<String>),
    /// No Exchange account (or no thread): nothing to ask.
    unavailable: bool,
    /// The answer (`calendar.freeBusy`'s `attendees` list), once it came.
    slot: Arc<Mutex<Option<Value>>>,
}

/// What the grid knows about one attendee.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AttendeeRow {
    /// No server to ask, still waiting, or the server knows nothing.
    Unknown,
    /// Busy blocks (local minutes of the day, show-as).
    Busy(Vec<(i64, i64, String)>),
}

/// The address in an attendee entry ("Name <a@example.com>" or "a@example.com"), lower-case.
fn address_of(entry: &str) -> String {
    let e = entry.trim();
    let inner = match (e.rfind('<'), e.rfind('>')) {
        (Some(a), Some(b)) if a < b => e.get(a + 1..b).unwrap_or(e),
        _ => e,
    };
    inner.trim().to_ascii_lowercase()
}

/// The attendees' rows for `day`: asks the server (once per day and attendee list) and reads what it said.
pub(crate) fn attendee_rows(
    app: &App,
    ed: &mut ApptEditor,
    day: i64,
    attendees: &[String],
    ctx: &egui::Context,
) -> (BTreeMap<String, AttendeeRow>, bool) {
    let addresses: Vec<String> =
        attendees.iter().map(|a| address_of(a)).filter(|a| a.contains('@')).collect::<Vec<_>>();
    let mut rows: BTreeMap<String, AttendeeRow> = BTreeMap::new();
    if addresses.is_empty() {
        return (rows, false);
    }
    let key = (day, addresses.clone());
    if ed.attendee_fb.as_ref().is_none_or(|c| c.key != key) {
        let (from, to) = day_bounds(app, day);
        let mut c = AttendeeFreeBusy { key, ..Default::default() };
        match mailcraft_engine::sync::availability_lookup(&app.s, &addresses, from.unix, to.unix) {
            None => c.unavailable = true,
            Some(lookup) => {
                let slot = Arc::clone(&c.slot);
                let ctx = ctx.clone();
                let spawned = std::thread::Builder::new().name("mc-free-busy".into()).spawn(move || {
                    let v = lookup.run();
                    if let Ok(mut g) = slot.lock() {
                        *g = Some(v);
                    }
                    ctx.request_repaint();
                });
                c.unavailable = spawned.is_err();
            }
        }
        ed.attendee_fb = Some(c);
    }
    let Some(c) = ed.attendee_fb.as_ref() else { return (rows, false) };
    if c.unavailable {
        return (rows, false);
    }
    let answer = c.slot.lock().ok().and_then(|g| g.clone());
    let Some(list) = answer.as_ref().and_then(Value::as_array) else { return (rows, true) };
    let tz = i64::from(app.s.tz_offset_min).saturating_mul(60);
    let base = day.saturating_mul(DAY);
    let minute = |t: i64| (t.saturating_add(tz).saturating_sub(base) / 60).clamp(0, 24 * 60);
    for entry in list {
        let Some(address) = entry.get("address").and_then(Value::as_str) else { continue };
        let row = match entry.get("busy").and_then(Value::as_array) {
            Some(blocks) => AttendeeRow::Busy(
                blocks
                    .iter()
                    .filter_map(|b| {
                        let s = minute(b.get("start")?.as_i64()?);
                        let e = minute(b.get("end")?.as_i64()?);
                        let show = b.get("showAs").and_then(Value::as_str).unwrap_or("busy").to_owned();
                        (e > s && !matches!(show.as_str(), "free" | "unknown")).then_some((s, e, show))
                    })
                    .collect(),
            ),
            None => AttendeeRow::Unknown,
        };
        rows.insert(address.to_ascii_lowercase(), row);
    }
    (rows, true)
}

fn show_as_fill(app: &App, show_as: &str) -> Color32 {
    let theme = &app.theme;
    match show_as {
        "tentative" => blend(theme.accent, theme.reading, 0.6),
        "outOfOffice" => Color32::from_rgb(0x8a, 0x3f, 0x9e),
        "workingElsewhere" => blend(theme.text_muted, theme.reading, 0.5),
        _ => theme.accent,
    }
}

/// The meeting's (start, end) in local minutes of its start day, when timed.
fn meeting_span(ed: &ApptEditor) -> Option<(i64, i64, i64)> {
    let sd = parse_day(&ed.start_date)?;
    let ed_day = parse_day(&ed.end_date)?;
    if ed.all_day {
        return None;
    }
    let start = ed.start_min;
    let end = (ed_day - sd).saturating_mul(24 * 60).saturating_add(ed.end_min);
    Some((sd, start, end.max(start)))
}

/// Moves the meeting to `day` at `minute`, keeping its length.
fn move_to(ed: &mut ApptEditor, day: i64, minute: i64) {
    let Some((_, s, e)) = meeting_span(ed) else { return };
    let len = e - s;
    let end = day.saturating_mul(24 * 60).saturating_add(minute).saturating_add(len);
    ed.start_date = day_string(day);
    ed.start_min = minute;
    ed.end_date = day_string(end.div_euclid(24 * 60));
    ed.end_min = end.rem_euclid(24 * 60);
}

/// The next working-hours slot after the meeting's start, of the meeting's length, free on the user's
/// calendar: (day, minute).
pub(crate) fn next_free(app: &App, ed: &ApptEditor) -> Option<(i64, i64)> {
    let (sd, s, e) = meeting_span(ed)?;
    let len = (e - s).max(1);
    for day in sd..sd.saturating_add(SEARCH_DAYS) {
        let busy = busy_minutes(app, day, ed.id);
        let mut m = if day == sd { s + 30 - s.rem_euclid(30) } else { FIRST_HOUR * 60 };
        while m + len <= LAST_HOUR * 60 {
            if m >= FIRST_HOUR * 60 && !busy.iter().any(|(bs, be, _)| *bs < m + len && *be > m) {
                return Some((day, m));
            }
            m += 30;
        }
    }
    None
}

/// What a busy block says: "Subject, 9:00 AM - 9:15 AM, Busy" with calendar details on (private items say
/// "Private appointment"), else only "Busy, 9:00 AM - 9:15 AM".
pub(crate) fn block_label(b: &BusySlot, start_min: i64, end_min: i64, details: bool) -> String {
    let show_as = match b.show_as.as_str() {
        "tentative" => "Tentative",
        "outOfOffice" => "Out of office",
        "workingElsewhere" => "Working elsewhere",
        _ => "Busy",
    };
    let clock = |m: i64| {
        let (h, mm) = (m.div_euclid(60).rem_euclid(24), m.rem_euclid(60));
        let (h12, ap) = match h {
            0 => (12, "AM"),
            1..=11 => (h, "AM"),
            12 => (12, "PM"),
            _ => (h - 12, "PM"),
        };
        format!("{h12}:{mm:02} {ap}")
    };
    let when = format!("{} - {}", clock(start_min), clock(end_min));
    if !details {
        return format!("{show_as}, {when}");
    }
    let subject = if b.private {
        "Private appointment"
    } else if b.subject.trim().is_empty() {
        "(No subject)"
    } else {
        b.subject.trim()
    };
    format!("{subject}, {when}, {show_as}")
}

/// The Scheduling Assistant tab.
pub(crate) fn show(ui: &mut Ui, app: &App, ed: &mut ApptEditor) {
    let details = app.s.settings.bool("calendar.gridDetails");
    let Some(start_day) = parse_day(&ed.start_date) else {
        ui.colored_label(app.theme.danger, "Enter the start date as YYYY-MM-DD.");
        return;
    };
    let day = ed.scheduling_day.unwrap_or(start_day);
    ui.horizontal(|ui| {
        if ui.small_button("◀").on_hover_text("Previous day").clicked() {
            ed.scheduling_day = Some(day - 1);
        }
        ui.label(egui::RichText::new(long_date(day)).strong());
        if ui.small_button("▶").on_hover_text("Next day").clicked() {
            ed.scheduling_day = Some(day + 1);
        }
        if ui.add_enabled(!ed.all_day, egui::Button::new("Next free time")).clicked()
            && let Some((d, m)) = next_free(app, ed)
        {
            move_to(ed, d, m);
            ed.scheduling_day = Some(d);
        }
    });
    let mut names: Vec<(String, bool)> = vec![("You".to_owned(), true)];
    for a in
        super::editors::split_list(&ed.attendees).into_iter().chain(super::editors::split_list(&ed.optional))
    {
        names.push((a, false));
    }
    let attendees: Vec<String> = names.iter().skip(1).map(|(n, _)| n.clone()).collect();
    let (fb, asked) = attendee_rows(app, ed, day, &attendees, ui.ctx());
    let slots = (LAST_HOUR - FIRST_HOUR) * 2;
    let width = NAME_W + SLOT_W * slots as f32;
    let height = ROW_H * (names.len() as f32 + 1.0);
    egui::ScrollArea::horizontal().id_salt("mc-sched-scroll").show(ui, |ui| {
        let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        a11y(&resp, egui::WidgetType::Other, "Scheduling grid");
        let p = ui.painter().with_clip_rect(rect);
        let theme = &app.theme;
        let x_of = |minute: i64| rect.min.x + NAME_W + ((minute - FIRST_HOUR * 60) as f32 / 30.0) * SLOT_W;
        // Hour header.
        for h in FIRST_HOUR..LAST_HOUR {
            let x = x_of(h * 60);
            p.text(
                pos2(x + 2.0, rect.min.y + ROW_H / 2.0),
                Align2::LEFT_CENTER,
                hour_label(h),
                FontId::proportional(11.0),
                theme.text_muted,
            );
        }
        let busy = busy_minutes(app, day, ed.id);
        for (i, (name, me)) in names.iter().enumerate() {
            let y = rect.min.y + ROW_H * (i as f32 + 1.0);
            let row = Rect::from_min_size(pos2(rect.min.x, y), vec2(width, ROW_H));
            p.hline(row.x_range(), row.min.y, Stroke::new(1.0, theme.separator));
            p.text(
                pos2(row.min.x + 4.0, row.center().y),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(12.0),
                theme.text,
            );
            let lane = Rect::from_min_max(
                pos2(x_of(FIRST_HOUR * 60), y + 3.0),
                pos2(x_of(LAST_HOUR * 60), y + ROW_H - 3.0),
            );
            if *me {
                for (s, e, b) in &busy {
                    let (s, e) = ((*s).max(FIRST_HOUR * 60), (*e).min(LAST_HOUR * 60));
                    if e <= s {
                        continue;
                    }
                    let r = Rect::from_min_max(pos2(x_of(s), lane.min.y), pos2(x_of(e), lane.max.y));
                    p.rect_filled(r, CornerRadius::same(2), show_as_fill(app, &b.show_as));
                    let label = block_label(b, s, e, details);
                    let block = ui.interact(r, ui.id().with(("mc-sched-busy", b.id, s)), Sense::hover());
                    a11y(&block, egui::WidgetType::Other, &label);
                    if details {
                        let text = if b.private { "Private appointment" } else { b.subject.trim() };
                        let clip = p.with_clip_rect(r.shrink(1.0));
                        clip.text(
                            pos2(r.min.x + 3.0, r.center().y),
                            Align2::LEFT_CENTER,
                            text,
                            FontId::proportional(10.5),
                            Color32::WHITE,
                        );
                        block.on_hover_text(label);
                    }
                }
            } else if let Some(AttendeeRow::Busy(blocks)) = fb.get(&address_of(name)) {
                // The attendee's busy times from the server (no subjects: only when they are busy).
                for (s, e, show_as) in blocks {
                    let (s, e) = ((*s).max(FIRST_HOUR * 60), (*e).min(LAST_HOUR * 60));
                    if e <= s {
                        continue;
                    }
                    let r = Rect::from_min_max(pos2(x_of(s), lane.min.y), pos2(x_of(e), lane.max.y));
                    p.rect_filled(r, CornerRadius::same(2), show_as_fill(app, show_as));
                    let slot = BusySlot {
                        id: 0,
                        start: 0,
                        end: 0,
                        show_as: show_as.clone(),
                        subject: String::new(),
                        private: false,
                    };
                    let label = format!("{name}: {}", block_label(&slot, s, e, false));
                    let block = ui.interact(r, ui.id().with(("mc-sched-att", i, s)), Sense::hover());
                    a11y(&block, egui::WidgetType::Other, &label);
                    block.on_hover_text(label);
                }
            } else {
                // No free/busy information for this person: hatched.
                let mut x = lane.min.x;
                while x < lane.max.x {
                    p.line_segment(
                        [pos2(x, lane.max.y), pos2((x + 6.0).min(lane.max.x), lane.min.y)],
                        Stroke::new(1.0, blend(theme.separator, theme.reading, 0.3)),
                    );
                    x += 6.0;
                }
            }
        }
        // Slot lines.
        for k in 0..=slots {
            let x = rect.min.x + NAME_W + SLOT_W * k as f32;
            let c = if k % 2 == 0 { theme.separator } else { blend(theme.separator, theme.reading, 0.6) };
            p.vline(x, rect.min.y + ROW_H..=rect.max.y, Stroke::new(1.0, c));
        }
        // The meeting band.
        if let Some((sd, s, e)) = meeting_span(ed) {
            let offset = (sd - day) * 24 * 60;
            let (s, e) = ((s + offset).max(FIRST_HOUR * 60), (e + offset).min(LAST_HOUR * 60));
            if e > s {
                let band = Rect::from_min_max(pos2(x_of(s), rect.min.y + ROW_H), pos2(x_of(e), rect.max.y));
                p.rect_filled(band, CornerRadius::ZERO, theme.accent.gamma_multiply(0.18));
                p.rect_stroke(band, CornerRadius::ZERO, Stroke::new(2.0, theme.accent), StrokeKind::Inside);
            }
        }
        if resp.clicked()
            && !ed.all_day
            && let Some(pos) = resp.interact_pointer_pos()
            && pos.x > rect.min.x + NAME_W
        {
            let slot = ((pos.x - rect.min.x - NAME_W) / SLOT_W).floor() as i64;
            move_to(ed, day, FIRST_HOUR * 60 + slot.clamp(0, slots - 1) * 30);
        }
    });
    // Conflicts with the meeting time.
    if let Some((sd, s, e)) = meeting_span(ed) {
        let clash: Vec<String> = busy_minutes(app, sd, ed.id)
            .into_iter()
            .filter(|(bs, be, _)| *bs < e && *be > s)
            .map(|(_, _, b)| if b.subject.trim().is_empty() { "(No subject)".to_owned() } else { b.subject })
            .collect();
        if clash.is_empty() {
            ui.label(egui::RichText::new("You're free at this time.").color(app.theme.text_muted));
        } else if details {
            ui.colored_label(app.theme.danger, format!("Conflicts with: {}", clash.join("; ")));
        } else {
            let n = clash.len();
            let what = if n == 1 { "1 item".to_owned() } else { format!("{n} items") };
            ui.colored_label(app.theme.danger, format!("Conflicts with {what} on your calendar."));
        }
    }
    let note = if !asked {
        "Attendees' free/busy times are not available; only your own calendar is shown."
    } else if fb.is_empty() {
        "Asking the Exchange server for the attendees' free/busy times…"
    } else {
        "Attendees' free/busy times come from the Exchange server; hatched rows have none."
    };
    ui.label(egui::RichText::new(note).color(app.theme.text_muted).size(11.0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_engine::sample::sample_session;

    fn editor(date: &str, start: i64, end: i64) -> ApptEditor {
        ApptEditor {
            start_date: date.into(),
            end_date: date.into(),
            start_min: start,
            end_min: end,
            ..Default::default()
        }
    }

    #[test]
    fn grid_details_follow_the_option() {
        let b = BusySlot {
            id: 1,
            start: 0,
            end: 0,
            show_as: "busy".into(),
            subject: "Team stand-up".into(),
            private: false,
        };
        assert_eq!(block_label(&b, 9 * 60, 9 * 60 + 15, true), "Team stand-up, 9:00 AM - 9:15 AM, Busy");
        assert_eq!(block_label(&b, 9 * 60, 9 * 60 + 15, false), "Busy, 9:00 AM - 9:15 AM");
        let private = BusySlot { private: true, show_as: "tentative".into(), ..b };
        assert_eq!(
            block_label(&private, 12 * 60, 13 * 60, true),
            "Private appointment, 12:00 PM - 1:00 PM, Tentative"
        );

        // In the grid: the stand-up on Friday 9 Oct is named only with details on.
        let grid = |details: bool| {
            let mut app = App::new(sample_session());
            app.run("options.set", serde_json::json!({"key": "calendar.gridDetails", "value": details}));
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 400.0)).build_ui_state(
                |ui, app: &mut App| {
                    let mut ed = editor("2026-10-09", 8 * 60 + 30, 9 * 60 + 30);
                    show(ui, app, &mut ed);
                },
                app,
            );
            h.run();
            use egui_kittest::kittest::Queryable;
            (
                h.query_by_label("Team stand-up, 9:00 AM - 9:15 AM, Busy").is_some(),
                h.query_by_label("Busy, 9:00 AM - 9:15 AM").is_some(),
                h.query_all_by_label_contains("Conflicts with 1 item").next().is_some(),
            )
        };
        assert_eq!(grid(true), (true, false, false));
        assert_eq!(grid(false), (false, true, true));
    }

    #[test]
    fn attendee_entries_give_their_address() {
        assert_eq!(address_of("Bob Example <Bob@Example.org>"), "bob@example.org");
        assert_eq!(address_of(" kim@example.com "), "kim@example.com");
        assert_eq!(address_of("odd > name <"), "odd > name <");
    }

    /// The attendees' rows show the busy times the Exchange server gives (the local test server's EWS
    /// endpoint, GetUserAvailability), looked up off the UI thread.
    #[test]
    fn attendee_rows_show_the_exchange_servers_busy_times() {
        use egui_kittest::kittest::Queryable;
        const PAT: &str = "pat@example.com";
        const BOB: &str = "bob@example.org";
        let srv = mailcraft_testmail::TestServers::start(mailcraft_testmail::Config {
            users: vec![(PAT.into(), "pat-pw".into()), (BOB.into(), "bob-pw".into())],
            ..mailcraft_testmail::Config::default()
        })
        .unwrap();
        srv.ews_put_item(BOB, "calendar", "<CalendarItem><Subject>Private</Subject><Start>2026-11-05T15:00:00Z</Start><End>2026-11-05T16:00:00Z</End><LegacyFreeBusyStatus>Tentative</LegacyFreeBusyStatus></CalendarItem>").unwrap();
        let mut s = mailcraft_engine::Session::new(mailcraft_model::Mailbox::new_local("Local Folders"));
        s.view.auto_send_receive_minutes = 0;
        s.tz_offset_min = 0;
        s.run(
            "accounts.add",
            &serde_json::json!({"kind": "exchange", "address": PAT, "password": "pat-pw",
                                "incoming": {"host": srv.ews_url(), "extraRootPem": srv.cert_pem}}),
        )
        .unwrap();
        let app = App::new(s);
        let mut ed = editor("2026-11-05", 14 * 60, 15 * 60 + 30);
        ed.attendees = format!("Bob Example <{BOB}>; nobody@example.net");
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1000.0, 400.0))
            .build_ui_state(move |ui, app: &mut App| show(ui, app, &mut ed), app);
        let label = format!("Bob Example <{BOB}>: Tentative, 3:00 PM - 4:00 PM");
        let mut found = false;
        for _ in 0..200 {
            h.run();
            if h.query_by_label(&label).is_some() {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(found, "the attendee's busy block is shown");
        assert!(h.query_all_by_label_contains("come from the Exchange server").next().is_some());
    }

    #[test]
    fn next_free_skips_busy_time() {
        let app = App::new(sample_session());
        // Friday 9 Oct: stand-up 9:00-9:15, lunch 12:00-13:00 (tentative).
        let ed = editor("2026-10-09", 8 * 60 + 30, 9 * 60 + 30);
        let busy = busy_minutes(&app, parse_day("2026-10-09").unwrap(), None);
        assert!(
            busy.iter().any(|(s, e, b)| *s == 9 * 60 && *e == 9 * 60 + 15 && b.subject == "Team stand-up")
        );
        assert_eq!(next_free(&app, &ed), Some((parse_day("2026-10-09").unwrap(), 9 * 60 + 30)));
        let ed = editor("2026-10-09", 11 * 60, 13 * 60);
        assert_eq!(next_free(&app, &ed), Some((parse_day("2026-10-09").unwrap(), 13 * 60)));
        let mut ed = editor("2026-10-09", 10 * 60, 11 * 60);
        move_to(&mut ed, parse_day("2026-10-12").unwrap(), 23 * 60 + 30);
        assert_eq!((ed.start_date.as_str(), ed.start_min), ("2026-10-12", 23 * 60 + 30));
        assert_eq!((ed.end_date.as_str(), ed.end_min), ("2026-10-13", 30));
    }
}
