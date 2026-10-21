//! Borders and Shading: a Borders tab (setting, line style / colour / width, and a preview whose edges
//! switch on and off one by one) and a Shading tab (fill, pattern and pattern colour). OK sends the whole
//! border set with `format.borders {"apply"}` and the shading with `format.shading`.

use egui::{Color32, Context, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn, footer, muted, primary, window, with_state};
use super::compose_dlg::cmd;
use crate::App;

/// Edge slots: top, bottom, left, right, inside horizontal (between paragraphs), inside vertical.
const EDGES: [&str; 6] = ["top", "bottom", "left", "right", "insideH", "insideV"];

/// Line styles in the order the engine lists them, with the label shown.
const STYLE_LABELS: [(&str, &str); 8] = [
    ("single", "Single"),
    ("dotted", "Dotted"),
    ("dashed", "Dashed"),
    ("double", "Double"),
    ("groove", "3-D carved"),
    ("ridge", "3-D raised"),
    ("inset", "Inset"),
    ("outset", "Outset"),
];

/// A border edge as the dialog edits it.
#[derive(Clone, Debug, PartialEq)]
struct Edge {
    style: String,
    /// `auto` or `#rrggbb`.
    color: String,
    width: f64,
}

impl Edge {
    fn from_json(v: &Value) -> Option<Edge> {
        let o = v.as_object()?;
        Some(Edge {
            style: o.get("style").and_then(Value::as_str).unwrap_or("single").to_owned(),
            color: o.get("color").and_then(Value::as_str).unwrap_or("auto").to_owned(),
            width: o.get("width").and_then(Value::as_f64).unwrap_or(0.5),
        })
    }
    fn json(&self) -> Value {
        json!({"style": self.style, "color": self.color, "width": self.width})
    }
}

#[derive(Clone)]
struct State {
    shading_tab: bool,
    setting: String,
    edges: [Option<Edge>; 6],
    shadow: bool,
    pen: Edge,
    color_text: String,
    fill: String,
    pattern: String,
    pattern_color: String,
    init_shading: (String, String, String),
    error: String,
}

/// `#rrggbb` or `auto` → an egui colour (auto = black).
fn color32(c: &str) -> Color32 {
    let c = c.trim();
    mailcraft_geom::Color::parse_hex(c.trim_start_matches('#'))
        .or_else(|| mailcraft_html::parse_css_color(c))
        .map_or(Color32::BLACK, |c| Color32::from_rgb(c.r, c.g, c.b))
}

/// "½ pt" style label of a width in points.
fn width_label(w: f64) -> String {
    let frac = match ((w.fract() * 100.0).round() as i64, w.trunc() as i64) {
        (25, 0) => return "¼ pt".into(),
        (50, 0) => return "½ pt".into(),
        (75, 0) => return "¾ pt".into(),
        (25, n) => format!("{n}¼"),
        (50, n) => format!("{n}½"),
        (75, n) => format!("{n}¾"),
        (_, n) => format!("{n}"),
    };
    format!("{frac} pt")
}

/// The edges a setting draws with `pen` (table or paragraph).
fn setting_edges(setting: &str, pen: &Edge, table: bool) -> [Option<Edge>; 6] {
    let on = |b: bool| b.then(|| pen.clone());
    let boxed = matches!(setting, "box" | "shadow" | "all" | "grid");
    let inside = table && matches!(setting, "all" | "grid");
    let mut thin = pen.clone();
    if setting == "grid" {
        thin.width = 0.5;
    }
    [on(boxed), on(boxed), on(boxed), on(boxed), inside.then(|| thin.clone()), inside.then_some(thin)]
}

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let table = d.str("target") == "table";
    let paragraphs = d.u64("paragraphs").unwrap_or(1);
    let styles: Vec<String> = {
        let listed: Vec<String> =
            d.arr("styles").iter().filter_map(Value::as_str).map(str::to_owned).collect();
        if listed.is_empty() { STYLE_LABELS.iter().map(|(k, _)| (*k).to_owned()).collect() } else { listed }
    };
    let widths: Vec<f64> = {
        let listed: Vec<f64> = d.arr("widths").iter().filter_map(Value::as_f64).collect();
        if listed.is_empty() { vec![0.25, 0.5, 0.75, 1.0, 1.5, 2.25, 3.0, 4.5, 6.0] } else { listed }
    };
    let patterns: Vec<String> = {
        let listed: Vec<String> =
            d.arr("patterns").iter().filter_map(Value::as_str).map(str::to_owned).collect();
        if listed.is_empty() { vec!["clear".into(), "solid".into()] } else { listed }
    };
    let init = |_: &App| {
        let borders = d.get("borders").cloned().unwrap_or(Value::Null);
        let mut edges: [Option<Edge>; 6] = Default::default();
        for (slot, key) in edges.iter_mut().zip(EDGES) {
            *slot = borders.get(key).and_then(Edge::from_json);
        }
        let pen = d.get("pen").and_then(Edge::from_json).unwrap_or(Edge {
            style: "single".into(),
            color: "auto".into(),
            width: 0.5,
        });
        let fill =
            d.get("shading").and_then(|s| s.get("fill")).and_then(Value::as_str).unwrap_or("").to_owned();
        let setting = d.str("setting");
        State {
            shading_tab: d.str("tab") == "shading",
            setting: if setting.is_empty() { "none".into() } else { setting },
            shadow: borders.get("shadow").and_then(Value::as_bool).unwrap_or(false),
            color_text: pen.color.clone(),
            pen,
            edges,
            init_shading: (fill.clone(), "clear".into(), "auto".into()),
            fill,
            pattern: "clear".into(),
            pattern_color: "auto".into(),
            error: String::new(),
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Borders and Shading", 600.0, |ui, app| {
            ui.horizontal(|ui| {
                if ui.selectable_label(!st.shading_tab, "Borders").clicked() {
                    st.shading_tab = false;
                }
                if ui.selectable_label(st.shading_tab, "Shading").clicked() {
                    st.shading_tab = true;
                }
            });
            ui.separator();
            if st.shading_tab {
                shading_tab(ui, app, st, uid, &patterns);
            } else {
                borders_tab(ui, app, st, uid, table, paragraphs, &styles, &widths);
            }
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            let mut res = Out::Keep;
            let mut go = false;
            footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    res = Out::Close;
                }
                go = primary(ui, app, true, "OK");
            });
            if go && matches!(res, Out::Keep) {
                res = match submit(app, st, compose, table) {
                    Ok(()) => Out::Close,
                    Err(e) => {
                        st.error = e;
                        Out::Keep
                    }
                };
            }
            res
        })
    })
}

fn submit(app: &mut App, st: &State, compose: Option<u64>, table: bool) -> Result<(), String> {
    let mut apply = serde_json::Map::new();
    apply.insert("pen".into(), st.pen.json());
    for (key, e) in EDGES.iter().zip(&st.edges) {
        if *key == "insideV" && !table {
            continue;
        }
        apply.insert((*key).into(), e.as_ref().map_or(Value::Null, Edge::json));
    }
    apply.insert("shadow".into(), json!(st.shadow && !table));
    apply.insert("target".into(), json!(if table { "table" } else { "paragraph" }));
    // The edges are sent as shown (the preview may have changed after a setting was picked).
    apply.insert("setting".into(), json!("custom"));
    cmd(app, "format.borders", json!({"compose": compose, "apply": Value::Object(apply)}))?;
    let now = (st.fill.trim().to_owned(), st.pattern.clone(), st.pattern_color.clone());
    if now != st.init_shading {
        let color = if now.0.is_empty() { Value::Null } else { json!(now.0) };
        let mut p = json!({"compose": compose, "color": color});
        if let Some(o) = p.as_object_mut()
            && now.1 != "clear"
        {
            o.insert("pattern".into(), json!(now.1));
            o.insert("patternColor".into(), json!(now.2));
        }
        cmd(app, "format.shading", p)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn borders_tab(
    ui: &mut Ui,
    app: &App,
    st: &mut State,
    uid: u64,
    table: bool,
    paragraphs: u64,
    styles: &[String],
    widths: &[f64],
) {
    let settings: &[(&str, &str)] = if table {
        &[("none", "None"), ("box", "Box"), ("all", "All"), ("grid", "Grid"), ("custom", "Custom")]
    } else {
        &[("none", "None"), ("box", "Box"), ("shadow", "Shadow"), ("custom", "Custom")]
    };
    let mut pen_changed = false;
    ui.horizontal_top(|ui| {
        // Setting.
        ui.vertical(|ui| {
            ui.set_width(110.0);
            ui.label("Setting:");
            for (key, label) in settings {
                if ui.selectable_label(st.setting == *key, *label).clicked() {
                    st.setting = (*key).to_owned();
                    if *key != "custom" {
                        st.edges = setting_edges(key, &st.pen, table);
                        st.shadow = *key == "shadow";
                    }
                }
            }
        });
        ui.separator();
        // Style, colour, width.
        ui.vertical(|ui| {
            ui.set_width(200.0);
            ui.label("Style:");
            egui::ScrollArea::vertical().id_salt(("bs-styles", uid)).max_height(150.0).show(ui, |ui| {
                for key in styles {
                    let label = STYLE_LABELS.iter().find(|(k, _)| k == key).map_or(key.as_str(), |(_, l)| l);
                    let selected = st.pen.style == *key;
                    let r = ui.add(egui::Button::selectable(selected, "").min_size(Vec2::new(180.0, 18.0)));
                    let rect = r.rect.shrink2(Vec2::new(8.0, 0.0));
                    ui.painter().text(
                        rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::FontId::proportional(11.0),
                        app.theme.text_muted,
                    );
                    let sample =
                        Rect::from_min_max(Pos2::new(rect.left() + 80.0, rect.top()), rect.right_bottom());
                    paint_line(
                        ui,
                        sample.left(),
                        sample.right(),
                        sample.center().y,
                        true,
                        key,
                        2.0,
                        app.theme.text,
                    );
                    if r.clicked() && !selected {
                        st.pen.style = key.clone();
                        pen_changed = true;
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Color:");
                let mut names: Vec<String> = vec!["Automatic".into()];
                names.extend(
                    mailcraft_engine::cmd::format::STANDARD_COLORS.iter().map(|(n, _)| (*n).to_owned()),
                );
                let current = if st.pen.color == "auto" {
                    "Automatic".to_owned()
                } else {
                    mailcraft_engine::cmd::format::STANDARD_COLORS
                        .iter()
                        .find(|(_, h)| h.eq_ignore_ascii_case(&st.pen.color))
                        .map_or_else(|| st.pen.color.clone(), |(n, _)| (*n).to_owned())
                };
                let mut pick = current.clone();
                let list: Vec<&str> = names.iter().map(String::as_str).collect();
                if common::combo(ui, ("bs-color", uid), &mut pick, &list, 110.0) {
                    st.pen.color = if pick == "Automatic" {
                        "auto".into()
                    } else {
                        mailcraft_engine::cmd::format::STANDARD_COLORS
                            .iter()
                            .find(|(n, _)| *n == pick)
                            .map_or_else(|| "auto".into(), |(_, h)| (*h).to_owned())
                    };
                    st.color_text = st.pen.color.clone();
                    pen_changed = true;
                }
                if ui.add(egui::TextEdit::singleline(&mut st.color_text).desired_width(70.0)).lost_focus() {
                    let t = st.color_text.trim().to_owned();
                    if t.eq_ignore_ascii_case("auto") || mailcraft_html::parse_css_color(&t).is_some() {
                        st.pen.color = if t.eq_ignore_ascii_case("auto") { "auto".into() } else { t };
                        pen_changed = true;
                    } else {
                        st.color_text = st.pen.color.clone();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Width:");
                let labels: Vec<String> = widths.iter().map(|w| width_label(*w)).collect();
                let list: Vec<&str> = labels.iter().map(String::as_str).collect();
                let mut idx = widths.iter().position(|w| (*w - st.pen.width).abs() < 1e-6).unwrap_or(1);
                if common::combo_idx(ui, ("bs-width", uid), &mut idx, &list, 90.0) {
                    st.pen.width = widths.get(idx).copied().unwrap_or(0.5);
                    pen_changed = true;
                }
            });
        });
        ui.separator();
        // Preview: click an edge (or a button) to switch it.
        ui.vertical(|ui| {
            ui.label("Preview:");
            muted(ui, app, "Click the diagram or use the buttons to apply borders.");
            let clicked = preview(ui, app, st, table, paragraphs);
            ui.horizontal_wrapped(|ui| {
                let mut keys =
                    vec![("top", "Top"), ("bottom", "Bottom"), ("left", "Left"), ("right", "Right")];
                if table || paragraphs > 1 {
                    keys.push(("insideH", if table { "Inside H" } else { "Between" }));
                }
                if table {
                    keys.push(("insideV", "Inside V"));
                }
                for (key, label) in keys {
                    let i = EDGES.iter().position(|k| *k == key).unwrap_or(0);
                    let on = st.edges.get(i).is_some_and(Option::is_some);
                    if ui.selectable_label(on, label).clicked() {
                        toggle(st, i);
                    }
                }
            });
            if let Some(i) = clicked {
                toggle(st, i);
            }
            ui.label(if table { "Apply to: Table" } else { "Apply to: Paragraph" });
        });
    });
    if pen_changed && !matches!(st.setting.as_str(), "none" | "custom") {
        st.edges = setting_edges(&st.setting, &st.pen, table);
    }
}

/// Switches edge `i`: on with the pen (or to the pen when it is drawn differently), off when it already is
/// the pen. The setting becomes Custom.
fn toggle(st: &mut State, i: usize) {
    if let Some(slot) = st.edges.get_mut(i) {
        *slot = if slot.as_ref() == Some(&st.pen) { None } else { Some(st.pen.clone()) };
    }
    st.setting = if st.edges.iter().all(Option::is_none) { "none".into() } else { "custom".into() };
    if st.setting == "none" {
        st.shadow = false;
    }
}

/// Paints the preview page and returns the edge clicked, if any.
fn preview(ui: &mut Ui, app: &App, st: &State, table: bool, paragraphs: u64) -> Option<usize> {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(200.0, 150.0), Sense::click());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, Color32::WHITE);
    p.rect_stroke(rect, 2.0, Stroke::new(1.0, common::control_border(&app.theme)), egui::StrokeKind::Inside);
    let inner = rect.shrink(22.0);
    let grey = Color32::from_gray(190);
    // Sample text: two paragraphs (or a 2×2 table) of grey bars.
    let rows = if table || paragraphs > 1 { 2 } else { 1 };
    let row_h = inner.height() / rows as f32;
    for r in 0..rows {
        let top = inner.top() + row_h * r as f32 + 8.0;
        let mut y = top;
        while y < inner.top() + row_h * (r as f32 + 1.0) - 6.0 {
            p.line_segment(
                [Pos2::new(inner.left() + 8.0, y), Pos2::new(inner.right() - 8.0, y)],
                Stroke::new(3.0, grey),
            );
            y += 8.0;
        }
    }
    let edge_color = |e: &Edge| if e.color == "auto" { Color32::BLACK } else { color32(&e.color) };
    let w = |e: &Edge| ((e.width * 4.0 / 3.0) as f32).clamp(1.0, 8.0);
    let mid_y = inner.center().y;
    let mid_x = inner.center().x;
    let lines: [(usize, Pos2, Pos2, bool); 6] = [
        (0, inner.left_top(), inner.right_top(), true),
        (1, inner.left_bottom(), inner.right_bottom(), true),
        (2, inner.left_top(), inner.left_bottom(), false),
        (3, inner.right_top(), inner.right_bottom(), false),
        (4, Pos2::new(inner.left(), mid_y), Pos2::new(inner.right(), mid_y), true),
        (5, Pos2::new(mid_x, inner.top()), Pos2::new(mid_x, inner.bottom()), false),
    ];
    for (i, a, b, horizontal) in lines {
        let Some(Some(e)) = st.edges.get(i) else {
            // Guides where an edge can go.
            if (i < 4) || (i == 4 && (table || paragraphs > 1)) || (i == 5 && table) {
                p.line_segment([a, b], Stroke::new(1.0, Color32::from_gray(225)));
            }
            continue;
        };
        if horizontal {
            paint_line(ui, a.x, b.x, a.y, true, &e.style, w(e), edge_color(e));
        } else {
            paint_line(ui, a.y, b.y, a.x, false, &e.style, w(e), edge_color(e));
        }
    }
    if st.shadow && !table {
        let s = 4.0;
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(inner.right(), inner.top() + s),
                Pos2::new(inner.right() + s, inner.bottom() + s),
            ),
            0.0,
            Color32::from_gray(128),
        );
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(inner.left() + s, inner.bottom()),
                Pos2::new(inner.right() + s, inner.bottom() + s),
            ),
            0.0,
            Color32::from_gray(128),
        );
    }
    if !resp.clicked() {
        return None;
    }
    let pos = resp.interact_pointer_pos()?;
    let near = 10.0;
    let candidates = [
        (0, (pos.y - inner.top()).abs()),
        (1, (pos.y - inner.bottom()).abs()),
        (2, (pos.x - inner.left()).abs()),
        (3, (pos.x - inner.right()).abs()),
        (4, if table || paragraphs > 1 { (pos.y - mid_y).abs() } else { f32::MAX }),
        (5, if table { (pos.x - mid_x).abs() } else { f32::MAX }),
    ];
    candidates.into_iter().filter(|(_, d)| *d <= near).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i)
}

/// A border line from `a` to `b` along `at` (horizontal or vertical) in a line style.
#[allow(clippy::too_many_arguments)]
fn paint_line(ui: &Ui, a: f32, b: f32, at: f32, horizontal: bool, style: &str, w: f32, color: Color32) {
    let p = ui.painter();
    let seg = |p0: f32, p1: f32, off: f32, width: f32, col: Color32| {
        let (s, e) = if horizontal {
            (Pos2::new(p0, at + off), Pos2::new(p1, at + off))
        } else {
            (Pos2::new(at + off, p0), Pos2::new(at + off, p1))
        };
        p.line_segment([s, e], Stroke::new(width, col));
    };
    let dark = color.gamma_multiply(0.6);
    let light = Color32::from_gray(200);
    match style {
        "dotted" | "dashed" => {
            let (on, off) =
                if style == "dotted" { (w.max(1.5), w.max(1.5)) } else { (w * 3.0 + 3.0, w * 2.0 + 2.0) };
            let mut x = a;
            let mut n = 0;
            while x < b && n < 400 {
                seg(x, (x + on).min(b), 0.0, w, color);
                x += on + off;
                n += 1;
            }
        }
        "double" => {
            let t = w.max(1.0);
            seg(a, b, -t, t, color);
            seg(a, b, t, t, color);
        }
        "groove" | "ridge" => {
            let (x, y) = if style == "groove" { (dark, light) } else { (light, dark) };
            seg(a, b, -w / 4.0, w / 2.0, x);
            seg(a, b, w / 4.0, w / 2.0, y);
        }
        "inset" => seg(a, b, 0.0, w, dark),
        "outset" => seg(a, b, 0.0, w, light),
        _ => seg(a, b, 0.0, w, color),
    }
}

fn shading_tab(ui: &mut Ui, app: &App, st: &mut State, uid: u64, patterns: &[String]) {
    egui::Grid::new(("bs-shade", uid)).num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
        ui.label("Fill:");
        ui.horizontal(|ui| {
            let mut names: Vec<String> = vec!["No Color".into()];
            names.extend(mailcraft_engine::cmd::format::STANDARD_COLORS.iter().map(|(n, _)| (*n).to_owned()));
            let current = if st.fill.is_empty() {
                "No Color".to_owned()
            } else {
                mailcraft_engine::cmd::format::STANDARD_COLORS
                    .iter()
                    .find(|(_, h)| h.eq_ignore_ascii_case(&st.fill))
                    .map_or_else(|| st.fill.clone(), |(n, _)| (*n).to_owned())
            };
            let mut pick = current;
            let list: Vec<&str> = names.iter().map(String::as_str).collect();
            if common::combo(ui, ("bs-fill", uid), &mut pick, &list, 120.0) {
                st.fill = mailcraft_engine::cmd::format::STANDARD_COLORS
                    .iter()
                    .find(|(n, _)| *n == pick)
                    .map_or_else(String::new, |(_, h)| (*h).to_owned());
            }
            ui.add(egui::TextEdit::singleline(&mut st.fill).hint_text("#rrggbb").desired_width(80.0));
        });
        ui.end_row();
        ui.label("Pattern style:");
        let list: Vec<&str> = patterns.iter().map(String::as_str).collect();
        common::combo(ui, ("bs-pattern", uid), &mut st.pattern, &list, 120.0);
        ui.end_row();
        ui.label("Pattern color:");
        ui.add_enabled(
            st.pattern != "clear",
            egui::TextEdit::singleline(&mut st.pattern_color)
                .hint_text("auto or #rrggbb")
                .desired_width(120.0),
        );
        ui.end_row();
        ui.label("Preview:");
        let fill = if st.fill.trim().is_empty() {
            mailcraft_geom::Color::WHITE
        } else {
            mailcraft_html::parse_css_color(st.fill.trim()).unwrap_or(mailcraft_geom::Color::WHITE)
        };
        let pct = st
            .pattern
            .trim_end_matches('%')
            .parse::<f32>()
            .ok()
            .map_or(if st.pattern == "solid" { 1.0 } else { 0.0 }, |v| v / 100.0);
        let pc = if st.pattern_color.trim().eq_ignore_ascii_case("auto") {
            mailcraft_geom::Color::BLACK
        } else {
            mailcraft_html::parse_css_color(st.pattern_color.trim()).unwrap_or(mailcraft_geom::Color::BLACK)
        };
        let c = fill.lerp(pc, pct);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(160.0, 40.0), Sense::hover());
        ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(c.r, c.g, c.b));
        ui.painter().rect_stroke(
            rect,
            2.0,
            Stroke::new(1.0, common::control_border(&app.theme)),
            egui::StrokeKind::Inside,
        );
        ui.end_row();
    });
    muted(
        ui,
        app,
        "Patterns are sent as the blend of the fill and pattern colours, which every mail program shows.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_labels() {
        assert_eq!(width_label(0.25), "¼ pt");
        assert_eq!(width_label(0.5), "½ pt");
        assert_eq!(width_label(1.0), "1 pt");
        assert_eq!(width_label(1.5), "1½ pt");
        assert_eq!(width_label(2.25), "2¼ pt");
        assert_eq!(width_label(4.5), "4½ pt");
    }

    #[test]
    fn settings_and_toggles() {
        let pen = Edge { style: "dashed".into(), color: "#c00000".into(), width: 1.5 };
        let e = setting_edges("box", &pen, false);
        assert!(e[..4].iter().all(|x| x.as_ref() == Some(&pen)) && e[4].is_none() && e[5].is_none());
        let g = setting_edges("grid", &pen, true);
        assert_eq!(g[4].as_ref().map(|x| x.width), Some(0.5));
        assert!(setting_edges("none", &pen, true).iter().all(Option::is_none));
        let mut st = State {
            shading_tab: false,
            setting: "box".into(),
            edges: e,
            shadow: false,
            pen: pen.clone(),
            color_text: String::new(),
            fill: String::new(),
            pattern: "clear".into(),
            pattern_color: "auto".into(),
            init_shading: Default::default(),
            error: String::new(),
        };
        toggle(&mut st, 0);
        assert!(st.edges[0].is_none());
        assert_eq!(st.setting, "custom");
        toggle(&mut st, 0);
        assert_eq!(st.edges[0].as_ref(), Some(&pen));
        for i in 0..4 {
            toggle(&mut st, i);
        }
        assert_eq!(st.setting, "none");
    }
}
