//! Insert-tab picture dialogs and the drawing dialogs of the compose window: Insert Icons (preview grid,
//! categories and search), Insert Chart (chart types and a data sheet, with a preview; also edits a chart
//! in the body), SmartArt (layout gallery and text pane, with a preview; also edits a graphic in the body),
//! 3D Model Views (turning a model picture), Object (as an icon, as text, or linked), and for a drawing
//! object Alt Text and Format Shape (`format_shape`). Every action goes through `app.run`; wording is our own.

use egui::{Color32, Context, Sense, Ui, Vec2, vec2};
use serde_json::{Value, json};

use super::common::{Dlg, Out, btn, footer, heading, muted, primary, window, with_state};
use super::compose_dlg::cmd;
use crate::App;
use mailcraft_engine::cmd::insert::{chart, icons, smartart};

mod format_shape;

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    let out = match d.name {
        "icons" => insert_icons(ctx, app, d),
        "insertChart" => insert_chart(ctx, app, d),
        "smartArt" => smart_art(ctx, app, d),
        "model3dView" => model_view(ctx, app, d),
        "insertObject" => object(ctx, app, d),
        "drawingAltText" => alt_text(ctx, app, d),
        "formatShape" => format_shape::format_shape(ctx, app, d),
        _ => return None,
    };
    if !out.keeps() {
        // The preview texture goes with the dialog.
        ctx.data_mut(|m| m.remove::<Preview>(preview_id(d.uid)));
    }
    Some(out)
}

/// Sets `key` on a JSON object (a no-op on anything else).
fn put(v: &mut Value, key: &str, value: Value) {
    if let Some(o) = v.as_object_mut() {
        o.insert(key.to_owned(), value);
    }
}

fn compose_of(d: &Dlg<'_>) -> u64 {
    d.u64("compose").unwrap_or(0)
}

/// OK / Cancel without the Enter shortcut (for dialogs with multi-line text).
fn ok_cancel_plain(ui: &mut Ui, app: &App, ok: &str, enabled: bool) -> Option<bool> {
    footer(ui, |ui| {
        if btn(ui, "Cancel") {
            return Some(false);
        }
        if primary(ui, app, enabled, ok) {
            return Some(true);
        }
        None
    })
}

/// A texture from PNG bytes, cached under `key`.
fn png_texture(
    ctx: &Context,
    key: &str,
    png: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<egui::TextureHandle> {
    let id = egui::Id::new(("mailcraft-dialog-png", key));
    if let Some(t) = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(id)) {
        return t;
    }
    let tex = png()
        .and_then(|data| crate::body::decode_image(&data))
        .map(|img| ctx.load_texture(format!("mailcraft-dialog-{key}"), img, egui::TextureOptions::LINEAR));
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    tex
}

/// The one preview picture of dialog `uid` and what it shows.
type Preview = (String, Option<egui::TextureHandle>);

fn preview_id(uid: u64) -> egui::Id {
    egui::Id::new(("mailcraft-dialog-preview", uid))
}

/// The preview of dialog `uid` for `key` (the chart or graphic it shows): one texture per dialog, replaced
/// when `key` changes, so typing in a data sheet does not pile up pictures.
fn preview_texture(
    ctx: &Context,
    uid: u64,
    key: &str,
    png: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<egui::TextureHandle> {
    let id = preview_id(uid);
    if let Some((k, t)) = ctx.data(|d| d.get_temp::<Preview>(id))
        && k == key
    {
        return t;
    }
    let tex = png().and_then(|data| crate::body::decode_image(&data)).map(|img| {
        ctx.load_texture(format!("mailcraft-dialog-preview-{uid}"), img, egui::TextureOptions::LINEAR)
    });
    ctx.data_mut(|d| d.insert_temp::<Preview>(id, (key.to_owned(), tex.clone())));
    tex
}

/// Draws `tex` scaled to fit `max` (keeping its proportions).
fn fit_image(ui: &mut Ui, tex: &egui::TextureHandle, max: Vec2) {
    let size = tex.size_vec2();
    let k = (max.x / size.x.max(1.0)).min(max.y / size.y.max(1.0)).min(1.0);
    ui.add(egui::Image::new((tex.id(), size * k)));
}

// ---------------------------------------------------------------------------------------------------------
// Insert Icons

#[derive(Clone, Default)]
struct IconsState {
    search: String,
    category: String,
    picked: Vec<String>,
    error: String,
}

/// Insert Icons: every bundled icon as a preview, filtered by category and search; the picked ones are
/// inserted together.
fn insert_icons(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| IconsState::default(),
        |st, app| {
            window(ctx, app, d, "Insert Icons", 560.0, |ui, app| {
                ui.horizontal(|ui| {
                    let l = ui.label("Search:");
                    ui.add(
                        egui::TextEdit::singleline(&mut st.search)
                            .desired_width(220.0)
                            .hint_text("icon name"),
                    )
                    .labelled_by(l.id);
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.selectable_label(st.category.is_empty(), "All").clicked() {
                        st.category.clear();
                    }
                    for cat in icons::CATEGORIES {
                        if ui.selectable_label(st.category == cat, cat).clicked() {
                            st.category = cat.to_owned();
                        }
                    }
                });
                ui.separator();
                let needle = st.search.trim().to_lowercase();
                let shown: Vec<&(&str, &str, &str, &str)> = icons::ICONS
                    .iter()
                    .filter(|(id, label, cat, _)| {
                        (st.category.is_empty() || *cat == st.category)
                            && (needle.is_empty()
                                || label.to_lowercase().contains(&needle)
                                || id.to_lowercase().contains(&needle))
                    })
                    .collect();
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    if shown.is_empty() {
                        muted(ui, app, "No icon matches that search.");
                    }
                    egui::Grid::new(("icons-grid", d.uid)).spacing([6.0, 6.0]).show(ui, |ui| {
                        for (i, (id, label, ..)) in shown.iter().enumerate() {
                            let picked = st.picked.iter().any(|p| p == id);
                            let (r, resp) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::click());
                            let bg =
                                if picked { app.theme.accent.gamma_multiply(0.25) } else { Color32::WHITE };
                            ui.painter().rect_filled(r, 4.0, bg);
                            let stroke = if picked { app.theme.accent } else { app.theme.separator };
                            ui.painter().rect_stroke(
                                r,
                                4.0,
                                egui::Stroke::new(1.0, stroke),
                                egui::StrokeKind::Inside,
                            );
                            if let Some(tex) =
                                png_texture(ui.ctx(), &format!("icon-{id}"), || icons::png(id, 80.0))
                            {
                                ui.painter().image(
                                    tex.id(),
                                    egui::Rect::from_center_size(r.center(), vec2(40.0, 40.0)),
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            }
                            resp.widget_info(|| {
                                egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, picked, *label)
                            });
                            let resp = resp.on_hover_text(*label);
                            if resp.clicked() {
                                if picked {
                                    st.picked.retain(|p| p != id);
                                } else {
                                    st.picked.push((*id).to_owned());
                                }
                            }
                            if resp.double_clicked() {
                                st.picked = vec![(*id).to_owned()];
                            }
                            if i % 8 == 7 {
                                ui.end_row();
                            }
                        }
                    });
                });
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let n = st.picked.len();
                let label = if n > 1 { format!("Insert ({n})") } else { "Insert".to_owned() };
                match ok_cancel_plain(ui, app, &label, n > 0) {
                    Some(true) => {
                        for id in st.picked.clone() {
                            if let Err(e) = cmd(app, "insert.icon", json!({"compose": cid, "name": id})) {
                                st.error = e;
                                return Out::Keep;
                            }
                        }
                        Out::Close
                    }
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Insert Chart

#[derive(Clone)]
struct ChartState {
    kind: String,
    title: String,
    legend: bool,
    sheet: Vec<Vec<String>>,
    error: String,
}

fn default_sheet() -> Vec<Vec<String>> {
    let rows = [
        ["", "Series 1", "Series 2"],
        ["Category 1", "4", "2"],
        ["Category 2", "6", "3"],
        ["Category 3", "5", "4"],
    ];
    rows.iter().map(|r| r.iter().map(|c| (*c).to_owned()).collect()).collect()
}

fn chart_params(st: &ChartState) -> Value {
    json!({"kind": st.kind, "title": st.title, "legend": st.legend, "sheet": st.sheet})
}

/// Insert Chart: the chart type, title and legend, a data sheet (series across, categories down) and a
/// preview. With `edit` it changes a chart already in the message.
fn insert_chart(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let edit = d.get("edit").and_then(Value::as_str).map(str::to_owned);
    let current = d
        .get("chart")
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<chart::ChartSpec>(v.clone()).ok());
    let kind = d.str("kind");
    let init = move |_: &App| match &current {
        Some(spec) => ChartState {
            kind: spec.kind.clone(),
            title: spec.title.clone(),
            legend: spec.legend,
            sheet: spec.sheet(),
            error: String::new(),
        },
        None => ChartState {
            kind: if kind.is_empty() { "column".into() } else { kind.clone() },
            title: String::new(),
            legend: true,
            sheet: default_sheet(),
            error: String::new(),
        },
    };
    let title = if edit.is_some() { "Edit Chart Data" } else { "Insert Chart" };
    with_state(ctx, app, d.uid, init, |st, app| {
        window(ctx, app, d, title, 760.0, |ui, app| {
            heading(ui, app, "Chart type");
            ui.horizontal_wrapped(|ui| {
                for (id, label) in chart::KINDS {
                    if ui.selectable_label(st.kind == id, label).clicked() {
                        st.kind = id.to_owned();
                    }
                }
            });
            ui.horizontal(|ui| {
                let l = ui.label("Chart title:");
                ui.add(egui::TextEdit::singleline(&mut st.title).desired_width(260.0)).labelled_by(l.id);
                ui.checkbox(&mut st.legend, "Legend");
            });
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(400.0);
                    heading(ui, app, "Data sheet");
                    muted(ui, app, "Series names across the top, category names down the side.");
                    sheet_editor(ui, app, d.uid, &mut st.sheet);
                });
                // Preview from what the sheet holds now.
                let params = chart_params(st);
                let theme = app.s.composes.get(&cid).map(|c| c.theme_resolved());
                let key = format!("chart-{params}");
                let preview = preview_texture(ui.ctx(), d.uid, &key, || {
                    let spec = chart::ChartSpec::from_params(&params).ok().flatten()?;
                    chart::png(&spec, theme.as_ref()?)
                });
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .stroke(egui::Stroke::new(1.0, app.theme.separator))
                    .show(ui, |ui| {
                        ui.set_min_size(vec2(320.0, 220.0));
                        match &preview {
                            Some(tex) => fit_image(ui, tex, vec2(320.0, 220.0)),
                            None => muted(ui, app, "Enter at least one number to see the chart."),
                        }
                    });
            });
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            match ok_cancel_plain(ui, app, if edit.is_some() { "OK" } else { "Insert" }, true) {
                Some(true) => {
                    let mut p = chart_params(st);
                    put(&mut p, "compose", json!(cid));
                    if let Some(e) = &edit {
                        put(&mut p, "edit", json!(e));
                    }
                    match cmd(app, "insert.chart", p) {
                        Ok(_) => Out::Close,
                        Err(e) => {
                            st.error = e;
                            Out::Keep
                        }
                    }
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

/// The data sheet grid: a text cell per value, with rows and series added and removed at the end.
fn sheet_editor(ui: &mut Ui, app: &App, uid: u64, sheet: &mut Vec<Vec<String>>) {
    let cols = sheet.iter().map(Vec::len).max().unwrap_or(1).max(2);
    for row in sheet.iter_mut() {
        row.resize(cols, String::new());
    }
    egui::ScrollArea::both().id_salt(("sheet", uid)).max_height(170.0).max_width(400.0).show(ui, |ui| {
        egui::Grid::new(("sheet-grid", uid)).spacing([2.0, 2.0]).show(ui, |ui| {
            for (r, row) in sheet.iter_mut().enumerate() {
                for (c, cell) in row.iter_mut().enumerate() {
                    if r == 0 && c == 0 {
                        ui.label("");
                        continue;
                    }
                    let header = r == 0 || c == 0;
                    let te =
                        egui::TextEdit::singleline(cell).desired_width(if c == 0 { 120.0 } else { 80.0 });
                    let te = if header {
                        te.font(egui::TextStyle::Body)
                    } else {
                        te.horizontal_align(egui::Align::RIGHT)
                    };
                    let resp = ui.add(te);
                    let name = match (r, c) {
                        (0, _) => format!("Series {c} name"),
                        (_, 0) => format!("Category {r} name"),
                        _ => format!("Row {r}, series {c}"),
                    };
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, &name));
                }
                ui.end_row();
            }
        });
    });
    ui.horizontal_wrapped(|ui| {
        if btn(ui, "Add Category") && sheet.len() <= chart::MAX_CATEGORIES {
            let n = sheet.len();
            let mut row = vec![String::new(); cols];
            if let Some(first) = row.first_mut() {
                *first = format!("Category {n}");
            }
            sheet.push(row);
        }
        if btn(ui, "Remove Category") && sheet.len() > 2 {
            sheet.pop();
        }
        if btn(ui, "Add Series") && cols <= chart::MAX_SERIES {
            for (r, row) in sheet.iter_mut().enumerate() {
                row.push(if r == 0 { format!("Series {cols}") } else { String::new() });
            }
        }
        if btn(ui, "Remove Series") && cols > 2 {
            for row in sheet.iter_mut() {
                row.pop();
            }
        }
        muted(ui, app, &format!("{} categories, {} series", sheet.len().saturating_sub(1), cols - 1));
    });
}

// ---------------------------------------------------------------------------------------------------------
// SmartArt

#[derive(Clone)]
struct SmartState {
    layout: String,
    text: String,
    error: String,
}

/// SmartArt: the layout gallery by category, the text pane (one item per line; Tab before an item makes it
/// a lower level) and a preview. With `edit` it changes a graphic already in the message.
fn smart_art(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let edit = d.get("edit").and_then(Value::as_str).map(str::to_owned);
    let current = d.get("current").cloned().unwrap_or(Value::Null);
    let init = move |_: &App| SmartState {
        layout: current.get("layout").and_then(Value::as_str).unwrap_or("list").to_owned(),
        text: current
            .get("items")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"))
            .unwrap_or_else(|| "First step\nSecond step\nThird step".to_owned()),
        error: String::new(),
    };
    let title = if edit.is_some() { "Edit SmartArt Graphic" } else { "Choose a SmartArt Graphic" };
    with_state(ctx, app, d.uid, init, |st, app| {
        window(ctx, app, d, title, 700.0, |ui, app| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(190.0);
                    let mut cat = "";
                    for (id, label, category) in smartart::LAYOUTS {
                        if category != cat {
                            heading(ui, app, category);
                            cat = category;
                        }
                        if ui.selectable_label(st.layout == id, label).clicked() {
                            st.layout = id.to_owned();
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    heading(ui, app, "Text");
                    muted(
                        ui,
                        app,
                        "One item per line. Press Tab at the start of a line to make it a lower level.",
                    );
                    ui.add(
                        egui::TextEdit::multiline(&mut st.text)
                            .desired_rows(8)
                            .desired_width(440.0)
                            .lock_focus(true)
                            .code_editor(),
                    );
                    let items: Vec<String> =
                        st.text.lines().filter(|l| !l.trim().is_empty()).map(str::to_owned).collect();
                    let key = format!("smartart-{}-{}", st.layout, items.join("\u{1f}"));
                    let theme = app.s.composes.get(&cid).map(|c| c.theme_resolved());
                    let layout = st.layout.clone();
                    let preview = preview_texture(ui.ctx(), d.uid, &key, || {
                        smartart::png(&layout, &items, theme.as_ref()?).map(|p| p.0)
                    });
                    egui::Frame::new()
                        .fill(Color32::WHITE)
                        .stroke(egui::Stroke::new(1.0, app.theme.separator))
                        .show(ui, |ui| {
                            ui.set_min_size(vec2(440.0, 180.0));
                            match &preview {
                                Some(tex) => fit_image(ui, tex, vec2(440.0, 180.0)),
                                None => muted(ui, app, "Type some items to see the graphic."),
                            }
                        });
                });
            });
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            match ok_cancel_plain(ui, app, if edit.is_some() { "OK" } else { "Insert" }, true) {
                Some(true) => {
                    let items: Vec<String> =
                        st.text.lines().filter(|l| !l.trim().is_empty()).map(str::to_owned).collect();
                    if items.is_empty() {
                        st.error = "Type at least one item.".into();
                        return Out::Keep;
                    }
                    let mut p = json!({"compose": cid, "layout": st.layout, "items": items});
                    if let Some(e) = &edit {
                        put(&mut p, "edit", json!(e));
                    }
                    match cmd(app, "insert.smartArt", p) {
                        Ok(_) => Out::Close,
                        Err(e) => {
                            st.error = e;
                            Out::Keep
                        }
                    }
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

// ---------------------------------------------------------------------------------------------------------
// 3D Model Views

#[derive(Clone)]
struct ViewState {
    yaw: f32,
    pitch: f32,
    error: String,
}

/// 3D Model Views: turn the model (around, and towards you) or pick a view.
fn model_view(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let edit = d.str("edit");
    let f = |k: &str, dflt: f32| d.get(k).and_then(Value::as_f64).map_or(dflt, |v| v as f32);
    let (yaw, pitch) = (f("yaw", 35.0), f("pitch", 25.0));
    with_state(
        ctx,
        app,
        d.uid,
        move |_: &App| ViewState { yaw, pitch, error: String::new() },
        |st, app| {
            window(ctx, app, d, "3D Model Views", 380.0, |ui, app| {
                ui.horizontal_wrapped(|ui| {
                    for (label, y, p) in [
                        ("Front", 0.0, 0.0),
                        ("Above", 0.0, 89.0),
                        ("Side", 90.0, 0.0),
                        ("Corner", 35.0, 25.0),
                        ("Below", 0.0, -89.0),
                    ] {
                        if ui
                            .selectable_label((st.yaw - y).abs() < 0.5 && (st.pitch - p).abs() < 0.5, label)
                            .clicked()
                        {
                            st.yaw = y;
                            st.pitch = p;
                        }
                    }
                });
                ui.add(egui::Slider::new(&mut st.yaw, 0.0..=359.0).text("Turn (°)"));
                ui.add(egui::Slider::new(&mut st.pitch, -90.0..=90.0).text("Tilt (°)"));
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                match super::common::ok_cancel(ui, app, true) {
                    Some(true) => match cmd(
                        app,
                        "insert.model3d",
                        json!({"compose": cid, "edit": edit, "yaw": st.yaw, "pitch": st.pitch}),
                    ) {
                        Ok(_) => Out::Close,
                        Err(e) => {
                            st.error = e;
                            Out::Keep
                        }
                    },
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Object

#[derive(Clone)]
struct ObjectState {
    path: String,
    as_text: bool,
    link: bool,
    error: String,
}

/// Whether `path` names a network share (`\\server\share`, `//server/share`, `smb://...`) that
/// recipients may be able to reach, rather than a folder on this computer.
fn network_path(path: &str) -> bool {
    let p = path.trim();
    p.starts_with(r"\\") || p.starts_with("//") || p.to_ascii_lowercase().starts_with("smb://")
}

/// Object: a file put into the message as an icon (attached, or linking to the file) or as its text.
fn object(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let path = d.str("path");
    let as_text = d.bool("asText");
    let init = move |_: &App| ObjectState {
        path: if path.is_empty() {
            format!("{}/", std::env::var("HOME").unwrap_or_default())
        } else {
            path.clone()
        },
        as_text,
        link: false,
        error: String::new(),
    };
    with_state(ctx, app, d.uid, init, |st, app| {
        window(ctx, app, d, "Object", 460.0, |ui, app| {
            super::common::field(ui, "File name:", &mut st.path, 320.0);
            ui.radio_value(&mut st.as_text, false, "Show the file as an icon");
            ui.add_enabled_ui(!st.as_text, |ui| {
                ui.indent("link", |ui| {
                    ui.checkbox(&mut st.link, "Link to the file instead of attaching it");
                });
            });
            ui.radio_value(&mut st.as_text, true, "Insert the file's text");
            muted(
                ui,
                app,
                if st.as_text {
                    "The text of the file goes into the message at the cursor."
                } else if st.link {
                    "The file is not sent: the icon opens it where it is now."
                } else {
                    "The file is attached and its icon goes into the message."
                },
            );
            if st.link && !st.as_text && !network_path(&st.path) {
                ui.colored_label(
                    app.theme.danger,
                    "This file is on this computer: recipients cannot open the link, and they see the \
                     file's full path (which can include your user name). Attach the file, or link to a \
                     shared network location instead.",
                );
            }
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            match super::common::ok_cancel(ui, app, !st.path.trim().is_empty()) {
                Some(true) => {
                    let p = json!({"compose": cid, "path": st.path.trim(), "asText": st.as_text, "link": st.link && !st.as_text});
                    match cmd(app, "insert.object", p) {
                        Ok(_) => Out::Close,
                        Err(e) => {
                            st.error = e;
                            Out::Keep
                        }
                    }
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

// ---------------------------------------------------------------------------------------------------------
// Drawing objects: Alt Text, Size and Position

#[derive(Clone)]
struct AltState {
    alt: String,
    error: String,
}

/// Alt Text of a drawing object.
fn alt_text(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let id = d.u64("id");
    let alt = d.str("alt");
    with_state(
        ctx,
        app,
        d.uid,
        move |_: &App| AltState { alt, error: String::new() },
        |st, app| {
            window(ctx, app, d, "Alt Text", 420.0, |ui, app| {
                muted(
                    ui,
                    app,
                    "Describe the drawing for people who use screen readers. The message's picture of it gets this description.",
                );
                ui.add(egui::TextEdit::multiline(&mut st.alt).desired_rows(4).desired_width(390.0));
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                match ok_cancel_plain(ui, app, "OK", true) {
                    Some(true) => {
                        match cmd(app, "drawing.altText", json!({"compose": cid, "id": id, "alt": st.alt})) {
                            Ok(_) => Out::Close,
                            Err(e) => {
                                st.error = e;
                                Out::Keep
                            }
                        }
                    }
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_data_sheet_makes_a_chart() {
        let st = ChartState {
            kind: "column".into(),
            title: "T".into(),
            legend: true,
            sheet: default_sheet(),
            error: String::new(),
        };
        let spec = chart::ChartSpec::from_params(&chart_params(&st)).unwrap().unwrap();
        assert_eq!((spec.series.len(), spec.categories.len()), (2, 3));
        // The sheet round-trips through the recipe (editing a chart shows its data).
        assert_eq!(spec.sheet(), default_sheet());
    }

    /// Regression: every keystroke in the data sheet or text pane kept a new preview texture for the rest of
    /// the session; a dialog now keeps one, replaced when what it shows changes.
    #[test]
    fn a_dialog_keeps_one_preview_texture() {
        let ctx = Context::default();
        let id = icons::ICONS.first().map(|i| i.0).unwrap();
        let mut made = 0;
        let mut first = None;
        for key in ["a", "a", "b", "c", "d", "d"] {
            let tex = preview_texture(&ctx, 7, key, || {
                made += 1;
                icons::png(id, 40.0)
            });
            assert!(tex.is_some());
            first.get_or_insert_with(|| ctx.tex_manager().read().num_allocated());
        }
        assert_eq!(made, 4, "the same key reuses the picture");
        assert_eq!(Some(ctx.tex_manager().read().num_allocated()), first, "older previews were freed");
        assert_eq!(ctx.data(|d| d.get_temp::<Preview>(preview_id(7))).map(|p| p.0), Some("d".to_owned()));
    }

    #[test]
    fn linking_to_a_local_file_is_told_apart_from_a_share() {
        assert!(network_path(r"\\server\share\plan.pdf"));
        assert!(network_path("//server/share/plan.pdf"));
        assert!(network_path("SMB://server/share/plan.pdf"));
        assert!(!network_path("/home/user/plan.pdf"));
        assert!(!network_path(r"C:\Users\user\plan.pdf"));
    }
}
