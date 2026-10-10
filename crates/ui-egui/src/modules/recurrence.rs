//! The Appointment Recurrence dialog: appointment time, recurrence pattern (Daily / Weekly / Monthly /
//! Yearly) and range (no end date / end after N occurrences / end by a date). The form is the engine's
//! `calendar::recurrence::RecurrenceForm`; OK writes its RRULE into the appointment editor. The next
//! occurrences are previewed with the engine's expansion (`calendar::occurrences`).

use egui::{ComboBox, Id, RichText, TextEdit, Ui, vec2};

use mailcraft_engine::cmd::calendar::recurrence::{ORDINALS, Pattern, RangeEnd, RecurrenceForm, WEEKDAYS};
use mailcraft_engine::cmd::calendar::{describe_recurrence, occurrences, sync_series_start};
use mailcraft_model::{Appointment, DateTime, Recurrence};

use super::common::{DAY, MONTHS, clock, day_string, local_secs, long_date, parse_day};
use crate::App;

/// Occurrences previewed under the form.
const PREVIEW: usize = 5;

/// Open dialog state (kept inside the appointment editor).
#[derive(Clone, Debug, Default)]
pub(crate) struct RecurrenceDialog {
    pub form: RecurrenceForm,
    /// Range start ("Start:" date of the series).
    pub start_date: String,
    pub start_min: i64,
    pub end_min: i64,
    pub all_day: bool,
    pub error: Option<String>,
    pub nonce: u64,
}

/// What the dialog returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Keep,
    Cancel,
    /// OK: (RRULE, series start date, start minute, end minute).
    Set(String, String, i64, i64),
    /// Remove Recurrence.
    Remove,
}

impl RecurrenceDialog {
    /// Dialog for an appointment starting `start_date` at `start_min` (local), with its current RRULE.
    pub(crate) fn new(
        app: &App,
        start_date: &str,
        start_min: i64,
        end_min: i64,
        all_day: bool,
        rrule: &str,
        nonce: u64,
    ) -> RecurrenceDialog {
        let tz = app.s.tz_offset_min;
        let start = start_instant(start_date, start_min, tz).unwrap_or_else(|| app.s.now());
        let form = if rrule.trim().is_empty() {
            RecurrenceForm::for_start(start, tz)
        } else {
            RecurrenceForm::from_rrule(rrule, start, tz)
        };
        RecurrenceDialog {
            form,
            start_date: start_date.to_owned(),
            start_min,
            end_min,
            all_day,
            error: None,
            nonce,
        }
    }

    /// The series as an appointment (for the preview and the description).
    fn sample(&self, app: &App) -> Result<Appointment, String> {
        let tz = app.s.tz_offset_min;
        let start = start_instant(&self.start_date, if self.all_day { 0 } else { self.start_min }, tz)
            .ok_or("Enter the start date as YYYY-MM-DD.")?;
        let len = if self.all_day { DAY } else { (self.end_min - self.start_min).rem_euclid(DAY / 60) * 60 };
        let rrule = self.form.to_rrule(self.all_day, tz)?;
        let mut a = Appointment {
            start,
            end: DateTime::from_unix(start.unix.saturating_add(len), tz),
            all_day: self.all_day,
            recurrence: Some(Recurrence { rrule, exdates: Vec::new() }),
            ..Default::default()
        };
        // Saving starts the series on its first occurrence; the preview shows the same.
        sync_series_start(&mut a);
        Ok(a)
    }
}

fn start_instant(date: &str, minute: i64, tz: i32) -> Option<DateTime> {
    let day = parse_day(date)?;
    let local = day.saturating_mul(DAY).saturating_add(minute.clamp(0, 24 * 60 - 1).saturating_mul(60));
    Some(DateTime::from_unix(local.saturating_sub(i64::from(tz).saturating_mul(60)), tz))
}

fn time_combo(ui: &mut Ui, id: &str, minute: &mut i64) {
    ComboBox::from_id_salt(id).selected_text(clock(*minute)).width(100.0).show_ui(ui, |ui| {
        for k in 0..96 {
            let m = k * 15;
            ui.selectable_value(minute, m, clock(m));
        }
    });
}

fn number(ui: &mut Ui, value: &mut u32, max: u32) {
    let mut v = i64::from(*value);
    ui.add(egui::DragValue::new(&mut v).range(1..=i64::from(max)).speed(0.2));
    *value = u32::try_from(v.clamp(1, i64::from(max))).unwrap_or(1);
}

fn weekday_combo(ui: &mut Ui, id: &str, wd: &mut u8) {
    let text = WEEKDAYS.get(usize::from(*wd)).copied().unwrap_or("Monday");
    ComboBox::from_id_salt(id).selected_text(text).width(110.0).show_ui(ui, |ui| {
        for (i, name) in WEEKDAYS.iter().enumerate() {
            ui.selectable_value(wd, u8::try_from(i).unwrap_or(0), *name);
        }
    });
}

fn ordinal_combo(ui: &mut Ui, id: &str, ord: &mut i8) {
    let text = ORDINALS.iter().find(|(n, _)| n == ord).map(|(_, s)| *s).unwrap_or("first");
    ComboBox::from_id_salt(id).selected_text(text).width(80.0).show_ui(ui, |ui| {
        for (n, name) in ORDINALS {
            ui.selectable_value(ord, n, name);
        }
    });
}

fn month_combo(ui: &mut Ui, id: &str, month: &mut u8) {
    let text = MONTHS.get(usize::from(*month).saturating_sub(1)).copied().unwrap_or("January");
    ComboBox::from_id_salt(id).selected_text(text).width(110.0).show_ui(ui, |ui| {
        for (i, name) in MONTHS.iter().enumerate() {
            ui.selectable_value(month, u8::try_from(i + 1).unwrap_or(1), *name);
        }
    });
}

fn day_number(ui: &mut Ui, day: &mut u8) {
    let mut v = i64::from(*day);
    ui.add(egui::DragValue::new(&mut v).range(1..=31).speed(0.2));
    *day = u8::try_from(v.clamp(1, 31)).unwrap_or(1);
}

/// Draws the dialog; returns what the user chose.
pub(crate) fn show(ctx: &egui::Context, app: &App, d: &mut RecurrenceDialog, has_rule: bool) -> Outcome {
    let mut open = true;
    let mut out = Outcome::Keep;
    egui::Window::new("Appointment Recurrence")
        .id(Id::new(("mc-recurrence", d.nonce)))
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .order(egui::Order::Foreground)
        .default_pos(ctx.content_rect().min + egui::vec2(60.0, 40.0))
        .open(&mut open)
        .show(ctx, |ui| {
            crate::dialogs::form_visuals(ui, &app.theme);
            // Appointment time.
            ui.label(RichText::new("Appointment time").strong());
            ui.horizontal(|ui| {
                ui.checkbox(&mut d.all_day, "All day");
                if !d.all_day {
                    ui.label("Start");
                    time_combo(ui, "mc-rec-st", &mut d.start_min);
                    ui.label("End");
                    time_combo(ui, "mc-rec-et", &mut d.end_min);
                    let len = (d.end_min - d.start_min).rem_euclid(24 * 60);
                    ui.label(
                        RichText::new(format!("Duration {}h {:02}m", len / 60, len % 60))
                            .color(app.theme.text_muted),
                    );
                }
            });
            ui.separator();
            // Pattern.
            ui.label(RichText::new("Recurrence pattern").strong());
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(90.0);
                    for (p, label) in [
                        (Pattern::Daily, "Daily"),
                        (Pattern::Weekly, "Weekly"),
                        (Pattern::Monthly, "Monthly"),
                        (Pattern::Yearly, "Yearly"),
                    ] {
                        ui.radio_value(&mut d.form.pattern, p, label);
                    }
                });
                ui.add_space(12.0);
                ui.vertical(|ui| pattern_detail(ui, d));
            });
            ui.separator();
            // Range.
            ui.label(RichText::new("Range of recurrence").strong());
            egui::Grid::new("mc-rec-range").num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
                ui.label("Start");
                ui.add(TextEdit::singleline(&mut d.start_date).desired_width(110.0).hint_text("YYYY-MM-DD"));
                ui.end_row();
                ui.label("");
                ui.vertical(|ui| {
                    ui.radio_value(&mut d.form.end, RangeEnd::NoEnd, "No end date");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.form.end, RangeEnd::After, "End after");
                        number(ui, &mut d.form.count, mailcraft_engine::cmd::calendar::recurrence::MAX_COUNT);
                        ui.label("occurrences");
                    });
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.form.end, RangeEnd::By, "End by");
                        ui.add(
                            TextEdit::singleline(&mut d.form.until)
                                .desired_width(110.0)
                                .hint_text("YYYY-MM-DD"),
                        );
                    });
                });
                ui.end_row();
            });
            ui.separator();
            // Preview.
            match d.sample(app) {
                Ok(a) => {
                    let tz = app.s.tz_offset_min;
                    if let Some(text) = describe_recurrence(&a, tz) {
                        ui.label(RichText::new(text).color(app.theme.text_muted));
                    }
                    let far = DateTime::from_unix(a.start.unix.saturating_add(400 * 7 * DAY), tz);
                    let next = occurrences(&a, a.start, far, PREVIEW);
                    ui.label(RichText::new("Next occurrences").strong());
                    for (s, _) in &next {
                        let l = local_secs(*s, tz);
                        let when = if a.all_day {
                            long_date(l.div_euclid(DAY))
                        } else {
                            format!("{} {}", long_date(l.div_euclid(DAY)), clock(l.rem_euclid(DAY) / 60))
                        };
                        ui.label(when);
                    }
                    if next.is_empty() {
                        ui.label(RichText::new("The pattern has no occurrences.").color(app.theme.danger));
                    }
                }
                Err(e) => {
                    ui.colored_label(app.theme.danger, e);
                }
            }
            if let Some(e) = &d.error {
                ui.colored_label(app.theme.danger, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(RichText::new("OK").strong()).clicked() {
                    out = match (parse_day(&d.start_date), d.form.to_rrule(d.all_day, app.s.tz_offset_min)) {
                        (None, _) => {
                            d.error = Some("Enter the start date as YYYY-MM-DD.".into());
                            Outcome::Keep
                        }
                        (_, Err(e)) => {
                            d.error = Some(e);
                            Outcome::Keep
                        }
                        (Some(day), Ok(rule)) => {
                            // The series starts on its first occurrence (as saving does).
                            let tz = app.s.tz_offset_min;
                            let day = d.sample(app).map_or(day, |a| local_secs(a.start, tz).div_euclid(DAY));
                            Outcome::Set(rule, day_string(day), d.start_min, d.end_min)
                        }
                    };
                }
                if ui.button("Cancel").clicked() {
                    out = Outcome::Cancel;
                }
                if ui.add_enabled(has_rule, egui::Button::new("Remove Recurrence")).clicked() {
                    out = Outcome::Remove;
                }
            });
        });
    if !open && out == Outcome::Keep {
        return Outcome::Cancel;
    }
    out
}

fn pattern_detail(ui: &mut Ui, d: &mut RecurrenceDialog) {
    let f = &mut d.form;
    let max = mailcraft_engine::cmd::calendar::recurrence::MAX_INTERVAL;
    match f.pattern {
        Pattern::Daily => {
            ui.horizontal(|ui| {
                ui.radio_value(&mut f.every_weekday, false, "Every");
                number(ui, &mut f.interval, max);
                ui.label("day(s)");
            });
            ui.radio_value(&mut f.every_weekday, true, "Every weekday");
        }
        Pattern::Weekly => {
            ui.horizontal(|ui| {
                ui.label("Recur every");
                number(ui, &mut f.interval, max);
                ui.label("week(s) on:");
            });
            ui.horizontal_wrapped(|ui| {
                for (i, name) in WEEKDAYS.iter().enumerate() {
                    let wd = u8::try_from(i).unwrap_or(0);
                    let mut on = f.weekdays.contains(&wd);
                    if ui.checkbox(&mut on, *name).changed() {
                        if on {
                            f.weekdays.push(wd);
                            f.weekdays.sort_unstable();
                        } else {
                            f.weekdays.retain(|x| *x != wd);
                        }
                    }
                }
            });
        }
        Pattern::Monthly => {
            ui.horizontal(|ui| {
                ui.radio_value(&mut f.by_weekday, false, "Day");
                day_number(ui, &mut f.day);
                ui.label("of every");
                number(ui, &mut f.interval, max);
                ui.label("month(s)");
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut f.by_weekday, true, "The");
                ordinal_combo(ui, "mc-rec-mord", &mut f.ordinal);
                weekday_combo(ui, "mc-rec-mwd", &mut f.weekday);
                ui.label("of every");
                number(ui, &mut f.interval, max);
                ui.label("month(s)");
            });
        }
        Pattern::Yearly => {
            ui.horizontal(|ui| {
                ui.label("Recur every");
                number(ui, &mut f.interval, max);
                ui.label("year(s)");
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut f.by_weekday, false, "On");
                month_combo(ui, "mc-rec-ym", &mut f.month);
                day_number(ui, &mut f.day);
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut f.by_weekday, true, "On the");
                ordinal_combo(ui, "mc-rec-yord", &mut f.ordinal);
                weekday_combo(ui, "mc-rec-ywd", &mut f.weekday);
                ui.label("of");
                month_combo(ui, "mc-rec-ym2", &mut f.month);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_engine::sample::sample_session;

    #[test]
    fn dialog_reads_and_writes_rules() {
        let app = App::new(sample_session());
        let d = RecurrenceDialog::new(
            &app,
            "2026-10-12",
            9 * 60,
            9 * 60 + 30,
            false,
            "FREQ=WEEKLY;BYDAY=MO,WE",
            1,
        );
        assert_eq!(d.form.pattern, Pattern::Weekly);
        assert_eq!(d.form.weekdays, vec![0, 2]);
        let a = d.sample(&app).unwrap();
        assert_eq!(a.end.unix - a.start.unix, 1800);
        assert_eq!(a.recurrence.unwrap().rrule, "FREQ=WEEKLY;BYDAY=MO,WE");
        let blank = RecurrenceDialog::new(&app, "2026-10-12", 9 * 60, 10 * 60, false, "", 2);
        assert_eq!(blank.form.weekdays, vec![0], "defaults to the start's weekday");
        let bad = RecurrenceDialog { start_date: "x".into(), ..blank };
        assert!(bad.sample(&app).is_err());
    }

    #[test]
    fn preview_starts_on_the_first_matching_day() {
        let app = App::new(sample_session());
        // 2026-10-14 is a Wednesday; every Monday starts on the 19th, as saving does.
        let d =
            RecurrenceDialog::new(&app, "2026-10-14", 9 * 60, 9 * 60 + 30, false, "FREQ=WEEKLY;BYDAY=MO", 1);
        let a = d.sample(&app).unwrap();
        let tz = app.s.tz_offset_min;
        assert_eq!(day_string(local_secs(a.start, tz).div_euclid(DAY)), "2026-10-19");
        assert_eq!(a.end.unix - a.start.unix, 1800);
    }
}
