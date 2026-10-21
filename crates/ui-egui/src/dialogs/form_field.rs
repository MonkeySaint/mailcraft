//! Search › Refine › More › Add Form Field: a chooser over the item properties grouped by form
//! (Frequently-used fields, All Mail fields, All Contact fields, …), a condition and a value; and the
//! form-field conditions already in the search as editable rows (field, condition, value) with Remove.
//! Conditions are `prop:` terms of the search query (`mailcraft_engine::cmd::search::FORM_FIELDS`).

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{Dlg, Out, btn, btn_en, combo_idx, footer, heading, muted, window, with_state};
use crate::App;
use mailcraft_engine::cmd::search::{FORM_FIELDS, FieldType, conditions_of, field_conditions};

/// One condition of the current search as the rows show it.
#[derive(Clone, Default)]
struct Row {
    term: String,
    field: String,
    cond: usize,
    value: String,
}

#[derive(Clone, Default)]
struct FormFieldState {
    form: usize,
    field: usize,
    cond: usize,
    value: String,
    rows: Vec<Row>,
    error: String,
}

fn forms() -> Vec<&'static str> {
    let mut out: Vec<&str> = Vec::new();
    for (g, _, _) in FORM_FIELDS {
        if !out.contains(g) {
            out.push(g);
        }
    }
    out
}

fn fields_of(form: &str) -> Vec<(&'static str, FieldType)> {
    FORM_FIELDS.iter().filter(|(g, _, _)| *g == form).map(|(_, l, t)| (*l, *t)).collect()
}

fn field_type(label: &str) -> FieldType {
    FORM_FIELDS.iter().find(|(_, l, _)| *l == label).map_or(FieldType::Text, |(_, _, t)| *t)
}

fn needs_value(cond: &str) -> bool {
    !matches!(cond, "isEmpty" | "isNotEmpty")
}

fn rows_of(app: &App) -> Vec<Row> {
    field_conditions(&app.s.view.search)
        .into_iter()
        .map(|v| {
            let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
            let field = s("field");
            let cond = conditions_of(field_type(&field))
                .iter()
                .position(|(id, _, _)| *id == s("condition"))
                .unwrap_or(0);
            Row { term: s("term"), field, cond, value: s("value") }
        })
        .collect()
}

/// A value box: a choice list for choice fields, else a text field (dates as YYYY-MM-DD).
fn value_input(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    t: FieldType,
    value: &mut String,
) {
    match t {
        FieldType::Choice(list) => {
            let mut idx = list.iter().position(|c| c.eq_ignore_ascii_case(value)).unwrap_or(0);
            combo_idx(ui, id, &mut idx, list, 170.0);
            *value = list.get(idx).copied().unwrap_or_default().to_owned();
        }
        FieldType::Date => {
            ui.add(egui::TextEdit::singleline(value).hint_text("YYYY-MM-DD").desired_width(170.0));
        }
        _ => {
            ui.add(egui::TextEdit::singleline(value).desired_width(170.0));
        }
    }
}

fn run_refine(app: &mut App, st: &mut FormFieldState, p: Value) {
    match app.s.run("search.refine", &p) {
        Ok(_) => st.error.clear(),
        Err(e) => st.error = e.to_string(),
    }
    st.rows = rows_of(app);
}

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    with_state(
        ctx,
        app,
        uid,
        |app: &App| FormFieldState { rows: rows_of(app), ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Add Form Field", 560.0, |ui, app| {
                let forms = forms();
                ui.horizontal(|ui| {
                    ui.label("Fields from:");
                    if combo_idx(ui, ("ff-form", uid), &mut st.form, &forms, 220.0) {
                        st.field = 0;
                        st.cond = 0;
                        st.value.clear();
                    }
                });
                let fields = fields_of(forms.get(st.form).copied().unwrap_or_default());
                st.field = st.field.min(fields.len().saturating_sub(1));
                ui.horizontal_top(|ui| {
                    // One field per line (the row around it is horizontal).
                    ui.vertical(|ui| {
                        ui.set_width(200.0);
                        egui::ScrollArea::vertical().id_salt(("ff-list", uid)).max_height(180.0).show(ui, |ui| {
                            ui.set_width(200.0);
                            for (i, (label, _)) in fields.iter().enumerate() {
                                if ui.selectable_label(st.field == i, *label).clicked() && st.field != i {
                                    st.field = i;
                                    st.cond = 0;
                                    st.value.clear();
                                }
                            }
                        });
                    });
                    ui.vertical(|ui| {
                        let Some((label, t)) = fields.get(st.field).copied() else { return };
                        ui.label(RichText::new(label).strong());
                        let conds = conditions_of(t);
                        let names: Vec<&str> = conds.iter().map(|(_, l, _)| *l).collect();
                        st.cond = st.cond.min(names.len().saturating_sub(1));
                        ui.horizontal(|ui| {
                            ui.label("Condition:");
                            combo_idx(ui, ("ff-cond", uid), &mut st.cond, &names, 170.0);
                        });
                        let cond = conds.get(st.cond).map_or("is", |(id, _, _)| *id);
                        if needs_value(cond) {
                            ui.horizontal(|ui| {
                                ui.label("Value:");
                                value_input(ui, ("ff-val", uid), t, &mut st.value);
                            });
                        }
                        let ok = !needs_value(cond) || !st.value.trim().is_empty();
                        if btn_en(ui, ok, "Add to Search") {
                            let p = json!({"kind": "field", "field": label, "condition": cond, "value": st.value});
                            run_refine(app, st, p);
                            if st.error.is_empty() {
                                st.value.clear();
                            }
                        }
                    });
                });
                heading(ui, app, "Form-field conditions in this search");
                if st.rows.is_empty() {
                    muted(ui, app, "None yet. Pick a field, a condition and a value, then Add to Search.");
                }
                let mut action: Option<Value> = None;
                egui::Grid::new(("ff-rows", uid)).num_columns(4).spacing([8.0, 4.0]).show(ui, |ui| {
                    for (i, row) in st.rows.iter_mut().enumerate() {
                        let t = field_type(&row.field);
                        let conds = conditions_of(t);
                        let names: Vec<&str> = conds.iter().map(|(_, l, _)| *l).collect();
                        ui.label(&row.field);
                        let mut changed = combo_idx(ui, ("ff-rc", uid, i), &mut row.cond, &names, 140.0);
                        let cond = conds.get(row.cond).map_or("is", |(id, _, _)| *id);
                        if needs_value(cond) {
                            let before = row.value.clone();
                            value_input(ui, ("ff-rv", uid, i), t, &mut row.value);
                            changed |= before != row.value && matches!(t, FieldType::Choice(_));
                            if btn(ui, "Apply") {
                                changed = true;
                            }
                        } else {
                            ui.label("");
                        }
                        if btn(ui, "Remove") {
                            action = Some(json!({"kind": "removeField", "term": row.term}));
                        } else if changed && action.is_none() {
                            action = Some(json!({
                                "kind": "field",
                                "field": row.field,
                                "condition": cond,
                                "value": row.value,
                                "replace": row.term,
                            }));
                        }
                        ui.end_row();
                    }
                });
                if let Some(p) = action {
                    run_refine(app, st, p);
                }
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
            })
        },
    )
}
