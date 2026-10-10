//! A small Scheduling Assistant for the meeting editor: one working day as a grid of half-hour slots with a
//! row for the user (busy blocks from their own calendars, `calendar::free_busy`) and one per attendee (no
//! free/busy information: MailCraft has no server to ask). The meeting time is a band over the grid;
//! clicking a slot moves the meeting there (keeping its length) and "Next free time" finds the next slot of
//! that length that is free on the user's calendar.

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

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

/// The Scheduling Assistant tab.
pub(crate) fn show(ui: &mut Ui, app: &App, ed: &mut ApptEditor) {
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
                    let fill = match b.show_as.as_str() {
                        "tentative" => blend(theme.accent, theme.reading, 0.6),
                        "outOfOffice" => Color32::from_rgb(0x8a, 0x3f, 0x9e),
                        "workingElsewhere" => blend(theme.text_muted, theme.reading, 0.5),
                        _ => theme.accent,
                    };
                    p.rect_filled(r, CornerRadius::same(2), fill);
                }
            } else {
                // No free/busy information for other people: hatched.
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
        } else {
            ui.colored_label(app.theme.danger, format!("Conflicts with: {}", clash.join("; ")));
        }
    }
    ui.label(
        egui::RichText::new("Attendees' free/busy times are not available; only your own calendar is shown.")
            .color(app.theme.text_muted)
            .size(11.0),
    );
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
