//! Add Holidays (File › Options › Calendar): pick countries; their public holidays for this year and the
//! next two go to the calendar as all-day events (`calendar.addHolidays`).

use egui::Context;
use serde_json::{Value, json};

use super::common::{Dlg, Out, muted, ok_cancel, window, with_state};
use crate::App;

/// Window title.
pub const TITLE: &str = "Add Holidays to Calendar";

#[derive(Clone, Default)]
struct State {
    /// (code, name, chosen).
    countries: Vec<(String, String, bool)>,
    error: Option<String>,
}

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    (d.name == "addHolidays").then(|| add_holidays(ctx, app, d))
}

fn add_holidays(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let listed: Vec<(String, String, bool)> = d
        .params
        .get("countries")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(100)
                .filter_map(|c| {
                    let code = c.get("code").and_then(Value::as_str)?;
                    let name = c.get("name").and_then(Value::as_str).unwrap_or(code);
                    Some((code.to_owned(), name.to_owned(), false))
                })
                .collect()
        })
        .unwrap_or_default();
    with_state(
        ctx,
        app,
        d.uid,
        |_| State { countries: listed, error: None },
        |st, app| {
            window(ctx, app, d, TITLE, 360.0, |ui, app| {
                muted(ui, app, "Choose the countries whose holidays you want in your calendar:");
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    for (_, name, on) in st.countries.iter_mut() {
                        ui.checkbox(on, name.as_str());
                    }
                });
                muted(ui, app, "This year's holidays and the next two years' are added as all-day events.");
                if let Some(e) = &st.error {
                    ui.colored_label(app.theme.danger, e);
                }
                let chosen: Vec<String> =
                    st.countries.iter().filter(|(_, _, on)| *on).map(|(c, _, _)| c.clone()).collect();
                match ok_cancel(ui, app, !chosen.is_empty()) {
                    Some(false) => Out::Close,
                    Some(true) => match app.s.run("calendar.addHolidays", &json!({ "countries": chosen })) {
                        Ok(r) => {
                            let added = r.get("added").and_then(Value::as_u64).unwrap_or(0);
                            let text = match added {
                                0 => "These holidays are already in your calendar.".to_owned(),
                                1 => "1 holiday was added to your calendar.".to_owned(),
                                n => format!("{n} holidays were added to your calendar."),
                            };
                            Out::CloseOpen(vec![("message".into(), json!({"title": TITLE, "text": text}))])
                        }
                        Err(e) => {
                            st.error = Some(e.to_string());
                            Out::Keep
                        }
                    },
                    None => Out::Keep,
                }
            })
        },
    )
}
