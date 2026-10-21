//! The accessibility warning under the compose header (File › Options › Accessibility › Accessibility checker
//! options): when the checker finds errors or warnings in the body and the "Accessibility warnings:" choice
//! says to show them now (while working, or when composing on a dark background, to a large audience,
//! outside the organization, or with high importance), a one-line bar says how many issues there are and
//! why, with Check Accessibility (`review.accessibility`) to open the checker.
//!
//! The engine decides ([`mailcraft_engine::cmd::review::accessibility_warning`]); the result is cached per
//! compose until the body, the recipients, the importance, dark mode or the options change.

use std::hash::{Hash, Hasher};

use egui::{Align, Id, Layout, RichText, Ui, vec2};
use serde_json::{Value, json};

use crate::App;

/// Accessible name of the bar.
pub const BAR_LABEL: &str = "Accessibility warning";
/// The bar's button.
pub const CHECK_BUTTON: &str = "Check Accessibility";

const BAR_H: f32 = 28.0;

/// What a cached warning was computed from.
fn inputs_key(app: &App, cid: u64) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(c) = app.s.composes.get(&cid) {
        (c.edit_rev, &c.to, &c.cc, &c.bcc, c.light_canvas, format!("{:?}", c.importance)).hash(&mut h);
        c.body.background.map(|b| b.to_hex()).hash(&mut h);
    }
    app.s.view.dark.hash(&mut h);
    for key in [
        "accessibility.warnings",
        "accessibility.warnDarkBackground",
        "accessibility.warnLargeAudience",
        "accessibility.warnOutsideOrganization",
        "accessibility.warnHighImportance",
    ] {
        app.s.settings.text(key).hash(&mut h);
        app.s.settings.bool(key).hash(&mut h);
    }
    app.s.mailbox.accounts.iter().for_each(|a| a.address.hash(&mut h));
    h.finish()
}

fn cache_id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-a11y", cid))
}

/// The engine's warning for compose `cid`, cached.
pub(super) fn warning(ui: &Ui, app: &App, cid: u64) -> Option<Value> {
    let key = inputs_key(app, cid);
    let ctx = ui.ctx();
    if let Some((k, v)) = ctx.data(|d| d.get_temp::<(u64, Value)>(cache_id(cid)))
        && k == key
    {
        return Some(v);
    }
    let v = mailcraft_engine::cmd::review::accessibility_warning(&app.s, cid)?;
    ctx.data_mut(|d| d.insert_temp(cache_id(cid), (key, v.clone())));
    Some(v)
}

/// Why the warning shows (the conditions that hold).
fn reason_text(reasons: &[Value]) -> String {
    let parts: Vec<&str> = reasons
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|r| match r {
            "darkBackground" => Some("dark background"),
            "largeAudience" => Some("large audience"),
            "outsideOrganization" => Some("recipients outside your organization"),
            "highImportance" => Some("high importance"),
            _ => None,
        })
        .collect();
    parts.join(", ")
}

/// The bar, when the warning shows.
pub(super) fn bar(ui: &mut Ui, app: &mut App, cid: u64) {
    let Some(w) = warning(ui, app, cid) else {
        return;
    };
    if w.get("show").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let n = |k: &str| w.get(k).and_then(Value::as_u64).unwrap_or(0);
    let count = n("errors").saturating_add(n("warnings"));
    let mut text = if count == 1 {
        "Accessibility: 1 issue to check".to_owned()
    } else {
        format!("Accessibility: {count} issues to check")
    };
    let reasons =
        reason_text(w.get("reasons").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default());
    if !reasons.is_empty() {
        text.push_str(&format!(" ({reasons})"));
    }
    let fill = app.theme.info_bar;
    let ink = app.theme.text;
    let width = ui.available_width();
    let mut check = false;
    ui.allocate_ui_with_layout(vec2(width, BAR_H), Layout::left_to_right(Align::Center), |ui| {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, fill);
        let group = ui.interact(rect, Id::new(("mailcraft-compose-a11y-bar", cid)), egui::Sense::hover());
        group.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, BAR_LABEL));
        ui.add_space(16.0);
        ui.label(RichText::new(text).color(ink));
        ui.add_space(8.0);
        if ui.button(CHECK_BUTTON).clicked() {
            check = true;
        }
    });
    if check {
        app.run("review.accessibility", json!({"compose": cid}));
    }
}
