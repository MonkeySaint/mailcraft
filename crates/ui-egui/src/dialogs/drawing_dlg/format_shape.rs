//! Format Shape: a drawing's properties on four pages. Fill & Line (no fill, solid, gradient stop by stop,
//! picture; the outline's colour, weight, dash and transparency), Effects (shadow, reflection, glow, soft
//! edges, bevel and 3-D rotation of the shape or of its text), Layout & Properties (text box margins,
//! vertical alignment, resize to fit the text, text wrapping, name and alt text) and Size (height, width,
//! rotation, position). OK applies what changed with one `drawing.format`, so it is one undo step.

use egui::{Context, Ui};
use mailcraft_geom::Color;
use mailcraft_model::rich::{Glow, Reflection, Shadow, ShadowKind, ShapeEffects};
use serde_json::{Value, json};

use super::super::common::{Dlg, Out, browse_open, heading, muted, window, with_state};
use super::super::compose_dlg::cmd;
use super::{compose_of, put};
use crate::App;

/// Pages: (id, title).
const PAGES: [(&str, &str); 4] =
    [("fill", "Fill & Line"), ("effects", "Effects"), ("layout", "Layout & Properties"), ("size", "Size")];

const FILLS: [&str; 4] = ["No fill", "Solid fill", "Gradient fill", "Picture fill"];
const NO_FILL: usize = 0;
const SOLID: usize = 1;
const GRADIENT: usize = 2;
const PICTURE: usize = 3;

/// Dash styles: (id, label); "" is solid.
const DASHES: [(&str, &str); 8] = [
    ("", "Solid"),
    ("roundDot", "Round Dot"),
    ("squareDot", "Square Dot"),
    ("dash", "Dash"),
    ("dashDot", "Dash Dot"),
    ("longDash", "Long Dash"),
    ("longDashDot", "Long Dash Dot"),
    ("longDashDotDot", "Long Dash Dot Dot"),
];

/// Wrap Text choices: (id, label).
const WRAPS: [(&str, &str); 7] = [
    ("inline", "In Line with Text"),
    ("square", "Square"),
    ("tight", "Tight"),
    ("through", "Through"),
    ("topBottom", "Top and Bottom"),
    ("behind", "Behind Text"),
    ("front", "In Front of Text"),
];

const VALIGNS: [(&str, &str); 3] = [("top", "Top"), ("middle", "Middle"), ("bottom", "Bottom")];

const SHADOW_KINDS: [(ShadowKind, &str, &str); 3] = [
    (ShadowKind::Outer, "outer", "Outer"),
    (ShadowKind::Inner, "inner", "Inner"),
    (ShadowKind::Perspective, "perspective", "Perspective"),
];

/// Largest picture file read for a picture fill.
const MAX_PICTURE_FILE: u64 = 30 * 1024 * 1024;
/// Longest side of a picture fill (the engine keeps no more).
const PICTURE_SIDE: u32 = 512;

/// A gradient stop as edited: position and transparency in percent.
#[derive(Clone, Debug, PartialEq)]
struct Stop {
    pos: f32,
    color: [u8; 3],
    transparency: f32,
}

/// Everything the pages edit. OK sends what differs from the drawing as it was.
#[derive(Clone, Debug, PartialEq)]
struct Props {
    fill_kind: usize,
    fill: [u8; 3],
    /// Percent.
    transparency: f32,
    stops: Vec<Stop>,
    angle: f32,
    radial: bool,
    /// A newly chosen picture (base64 PNG) and its file name.
    picture: Option<(String, String)>,
    line: bool,
    outline: [u8; 3],
    width: f32,
    dash: String,
    /// Percent.
    line_transparency: f32,
    fx: ShapeEffects,
    text_fx: ShapeEffects,
    inset: [f32; 4],
    autofit: bool,
    valign: String,
    wrap: String,
    name: String,
    alt: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    rotation: f32,
}

#[derive(Clone)]
struct FormatState {
    page: String,
    now: Props,
    was: Props,
    lock_ratio: bool,
    /// Effects page: the text's effects instead of the shape's.
    text_effects: bool,
    error: String,
}

fn rgb(c: Color) -> [u8; 3] {
    [c.r, c.g, c.b]
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn color_of(v: Option<&Value>) -> Option<Color> {
    v.and_then(Value::as_str).and_then(Color::parse_hex)
}

fn num(v: Option<&Value>, default: f32) -> f32 {
    v.and_then(Value::as_f64).map_or(default, |n| n as f32)
}

fn effects_of(v: Option<&Value>) -> ShapeEffects {
    v.filter(|v| v.is_object()).and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
}

/// The drawing as the dialog payload describes it.
fn props_of(d: &Dlg<'_>) -> Props {
    let f = |k: &str| num(d.get(k), 0.0);
    let fill = color_of(d.get("fill"));
    let stops: Vec<Stop> = d
        .arr("stops")
        .iter()
        .map(|s| Stop {
            pos: num(s.get("pos"), 0.0) * 100.0,
            color: color_of(s.get("color")).map_or([255, 255, 255], rgb),
            transparency: num(s.get("transparency"), 0.0) * 100.0,
        })
        .collect();
    let base = fill.map_or([68, 114, 196], rgb);
    let fill_kind = if d.bool("picture") {
        PICTURE
    } else if !stops.is_empty() || !d.str("gradient").is_empty() {
        GRADIENT
    } else if fill.is_some() {
        SOLID
    } else {
        NO_FILL
    };
    let stops = if stops.len() >= 2 { stops } else { default_stops(base) };
    let outline = color_of(d.get("outline"));
    let inset = d
        .get("inset")
        .and_then(Value::as_array)
        .map(|a| {
            let mut v = [0.0f32; 4];
            for (slot, n) in v.iter_mut().zip(a) {
                *slot = num(Some(n), 0.0);
            }
            v
        })
        .unwrap_or([7.0, 3.0, 7.0, 3.0]);
    let valign = d.str("valign");
    Props {
        fill_kind,
        fill: base,
        transparency: f("transparency") * 100.0,
        stops,
        angle: num(d.get("angle"), 90.0),
        radial: d.bool("radial"),
        picture: None,
        line: outline.is_some(),
        outline: outline.map_or([47, 82, 143], rgb),
        width: num(d.get("outlineWidth"), 1.0),
        dash: d.str("dash"),
        line_transparency: f("lineTransparency") * 100.0,
        fx: effects_of(d.get("fx")),
        text_fx: effects_of(d.get("textFx")),
        inset,
        autofit: d.bool("autofit"),
        valign: if valign.is_empty() { "top".into() } else { valign },
        wrap: {
            let w = d.str("wrap");
            if w.is_empty() { "front".into() } else { w }
        },
        name: d.str("name"),
        alt: d.str("alt"),
        x: f("x"),
        y: f("y"),
        w: f("w"),
        h: f("h"),
        rotation: f("rotation"),
    }
}

/// A two-stop gradient from `base` to a lighter tint of it.
fn default_stops(base: [u8; 3]) -> Vec<Stop> {
    let light = base.map(|c| c.saturating_add((255 - c) / 2));
    vec![
        Stop { pos: 0.0, color: light, transparency: 0.0 },
        Stop { pos: 100.0, color: base, transparency: 0.0 },
    ]
}

/// The `drawing.effect` options that make `fx` (every option given, so the result is exactly `fx`).
fn fx_params(fx: &ShapeEffects) -> Value {
    let color = |c: Option<Color>| c.map_or(Value::Null, |c| json!(hex(rgb(c))));
    let shadow = fx.shadow.map_or(Value::Null, |s| {
        let kind = SHADOW_KINDS.iter().find(|k| k.0 == s.kind).map_or("outer", |k| k.1);
        json!({"kind": kind, "color": color(s.color), "transparency": s.transparency, "blur": s.blur,
               "angle": s.angle, "distance": s.distance})
    });
    let reflection = fx.reflection.map_or(
        Value::Null,
        |r| json!({"transparency": r.transparency, "size": r.size, "distance": r.distance, "blur": r.blur}),
    );
    let glow = fx.glow.map_or(
        Value::Null,
        |g| json!({"color": color(g.color), "size": g.size, "transparency": g.transparency}),
    );
    json!({
        "shadow": shadow, "reflection": reflection, "glow": glow, "softEdges": fx.soft_edges,
        "bevel": if fx.bevel.is_empty() { "none" } else { fx.bevel.as_str() }, "bevelSize": fx.bevel_size,
        "rotation3d": {"x": fx.rot_x, "y": fx.rot_y, "perspective": fx.perspective},
    })
}

/// What OK sends: the parts of `now` that differ from `was` (empty: nothing changed).
fn changes(now: &Props, was: &Props) -> Value {
    let mut p = json!({});
    // Only the side that changed (several drawings keep their other side).
    if now.w != was.w {
        put(&mut p, "w", json!(now.w));
    }
    if now.h != was.h {
        put(&mut p, "h", json!(now.h));
    }
    if (now.x, now.y) != (was.x, was.y) {
        put(&mut p, "x", json!(now.x));
        put(&mut p, "y", json!(now.y));
    }
    if now.rotation != was.rotation {
        put(&mut p, "rotation", json!(now.rotation));
    }
    let fill_changed = now.fill_kind != was.fill_kind
        || now.picture.is_some()
        || match now.fill_kind {
            SOLID => (now.fill, now.transparency) != (was.fill, was.transparency),
            GRADIENT => {
                (&now.stops, now.angle, now.radial, now.transparency)
                    != (&was.stops, was.angle, was.radial, was.transparency)
            }
            _ => false,
        };
    if fill_changed {
        let had_picture = was.fill_kind == PICTURE;
        match now.fill_kind {
            NO_FILL => {
                put(&mut p, "noFill", json!(true));
                put(&mut p, "stops", Value::Null);
                if had_picture {
                    put(&mut p, "picture", Value::Null);
                }
            }
            SOLID | GRADIENT => {
                put(&mut p, "fill", json!(hex(now.fill)));
                put(&mut p, "transparency", json!(now.transparency / 100.0));
                if now.fill_kind == GRADIENT {
                    let stops: Vec<Value> = now
                        .stops
                        .iter()
                        .map(|s| {
                            json!({"pos": (s.pos / 100.0).clamp(0.0, 1.0), "color": hex(s.color),
                                   "transparency": (s.transparency / 100.0).clamp(0.0, 1.0)})
                        })
                        .collect();
                    put(&mut p, "stops", json!(stops));
                    put(&mut p, "angle", json!(now.angle));
                    put(&mut p, "radial", json!(now.radial));
                } else {
                    put(&mut p, "stops", Value::Null);
                    put(&mut p, "gradient", json!(""));
                }
                if had_picture {
                    put(&mut p, "picture", Value::Null);
                }
            }
            _ => {
                if let Some((data, _)) = &now.picture {
                    put(&mut p, "picture", json!(data));
                }
            }
        }
    }
    if (now.line, now.outline, now.width, &now.dash, now.line_transparency)
        != (was.line, was.outline, was.width, &was.dash, was.line_transparency)
    {
        if now.line {
            put(&mut p, "outline", json!(hex(now.outline)));
            put(&mut p, "outlineWidth", json!(now.width));
            put(&mut p, "dash", json!(if now.dash.is_empty() { "solid" } else { now.dash.as_str() }));
            put(&mut p, "lineTransparency", json!(now.line_transparency / 100.0));
        } else {
            put(&mut p, "outline", Value::Null);
        }
    }
    if now.fx != was.fx {
        put(&mut p, "effects", fx_params(&now.fx));
    }
    if now.text_fx != was.text_fx {
        put(&mut p, "textEffects", fx_params(&now.text_fx));
    }
    if now.inset != was.inset {
        put(&mut p, "inset", json!(now.inset));
    }
    if now.autofit != was.autofit {
        put(&mut p, "autofit", json!(now.autofit));
    }
    if now.valign != was.valign {
        put(&mut p, "valign", json!(now.valign));
    }
    if now.wrap != was.wrap {
        put(&mut p, "wrap", json!(now.wrap));
    }
    if now.alt != was.alt {
        put(&mut p, "alt", json!(now.alt));
    }
    // A picture fill takes `name` as its file name; the drawing's own name then goes separately.
    if now.name != was.name && now.picture.is_none() {
        put(&mut p, "name", json!(now.name));
    }
    p
}

/// A picture file as a base64 PNG no larger than [`PICTURE_SIDE`].
fn load_picture(path: &str) -> Result<String, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_PICTURE_FILE {
        return Err("That picture is too large to fill a shape.".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&bytes)
        .map_err(|_| "That file is not a picture MailCraft can read.".to_owned())?;
    let img = if img.width() > PICTURE_SIDE || img.height() > PICTURE_SIDE {
        img.thumbnail(PICTURE_SIDE, PICTURE_SIDE)
    } else {
        img
    };
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(mailcraft_mime::transfer::encode_base64_flat(&out.into_inner()))
}

fn pick(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    value: &mut String,
    choices: &[(&str, &str)],
) {
    let shown = choices.iter().find(|c| c.0 == value.as_str()).map_or(value.as_str(), |c| c.1).to_owned();
    egui::ComboBox::from_id_salt(id).selected_text(shown).width(170.0).show_ui(ui, |ui| {
        for (key, label) in choices {
            if ui.selectable_label(value == key, *label).clicked() {
                *value = (*key).to_owned();
            }
        }
    });
}

fn percent(ui: &mut Ui, label: &str, v: &mut f32) {
    ui.label(label);
    ui.add(egui::Slider::new(v, 0.0..=100.0).suffix(" %"));
    ui.end_row();
}

fn pixels(ui: &mut Ui, label: &str, v: &mut f32, max: f32) {
    ui.label(label);
    ui.add(egui::DragValue::new(v).range(0.0..=max).speed(0.25).suffix(" px"));
    ui.end_row();
}

fn degrees(ui: &mut Ui, label: &str, v: &mut f32, max: f32) {
    ui.label(label);
    ui.add(egui::DragValue::new(v).range(0.0..=max).speed(1.0).suffix("°"));
    ui.end_row();
}

/// An optional colour: "Automatic" or a picked one.
fn optional_color(ui: &mut Ui, label: &str, c: &mut Option<Color>) {
    ui.label(label);
    ui.horizontal(|ui| {
        let mut auto = c.is_none();
        if ui.checkbox(&mut auto, "Automatic").changed() {
            *c = if auto { None } else { Some(Color::rgb(0, 0, 0)) };
        }
        if let Some(col) = c.as_mut() {
            let mut v = rgb(*col);
            if egui::color_picker::color_edit_button_srgb(ui, &mut v).changed() {
                *col = Color::rgb(v[0], v[1], v[2]);
            }
        }
    });
    ui.end_row();
}

fn fill_page(ui: &mut Ui, app: &App, uid: u64, st: &mut FormatState) {
    let n = &mut st.now;
    heading(ui, app, "Fill");
    ui.horizontal_wrapped(|ui| {
        for (i, label) in FILLS.iter().enumerate() {
            ui.radio_value(&mut n.fill_kind, i, *label);
        }
    });
    egui::Grid::new(("fs-fill", uid)).num_columns(2).show(ui, |ui| match n.fill_kind {
        SOLID => {
            ui.label("Colour:");
            egui::color_picker::color_edit_button_srgb(ui, &mut n.fill);
            ui.end_row();
            percent(ui, "Transparency:", &mut n.transparency);
        }
        GRADIENT => {
            ui.label("Type:");
            ui.horizontal(|ui| {
                ui.radio_value(&mut n.radial, false, "Linear");
                ui.radio_value(&mut n.radial, true, "Radial");
            });
            ui.end_row();
            if !n.radial {
                degrees(ui, "Angle:", &mut n.angle, 360.0);
            }
            percent(ui, "Transparency:", &mut n.transparency);
        }
        PICTURE => {
            ui.label("Picture:");
            ui.horizontal(|ui| {
                let label = n.picture.as_ref().map_or_else(
                    || {
                        if st.was.fill_kind == PICTURE {
                            "(the current picture)".to_owned()
                        } else {
                            "(none)".to_owned()
                        }
                    },
                    |(_, name)| name.clone(),
                );
                ui.label(label);
                if app.files.is_some() && ui.button("Insert...").clicked() {
                    let filters =
                        [crate::FileFilter::new("Pictures", &["png", "jpg", "jpeg", "gif", "bmp", "webp"])];
                    if let Some(path) = browse_open(app, "Fill with a Picture", &filters, "") {
                        match load_picture(&path) {
                            Ok(data) => {
                                let name = std::path::Path::new(&path)
                                    .file_name()
                                    .map(|f| f.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                n.picture = Some((data, name));
                                st.error.clear();
                            }
                            Err(e) => st.error = e,
                        }
                    }
                }
            });
            ui.end_row();
        }
        _ => {}
    });
    if n.fill_kind == GRADIENT {
        ui.label("Gradient stops:");
        let mut remove = None;
        let count = n.stops.len();
        for (i, s) in n.stops.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("{}.", i + 1));
                ui.add(egui::DragValue::new(&mut s.pos).range(0.0..=100.0).suffix(" %"))
                    .on_hover_text("Position");
                egui::color_picker::color_edit_button_srgb(ui, &mut s.color);
                ui.add(egui::DragValue::new(&mut s.transparency).range(0.0..=100.0).suffix(" % clear"))
                    .on_hover_text("Transparency");
                if count > 2 && ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove
            && i < n.stops.len()
        {
            n.stops.remove(i);
        }
        if n.stops.len() < mailcraft_engine::cmd::insert::drawing::MAX_GRADIENT_STOPS
            && ui.button("Add Stop").clicked()
        {
            let last =
                n.stops.last().cloned().unwrap_or(Stop { pos: 100.0, color: n.fill, transparency: 0.0 });
            n.stops.push(Stop { pos: (last.pos + 10.0).min(100.0), ..last });
        }
    }
    ui.add_space(6.0);
    heading(ui, app, "Line");
    ui.horizontal(|ui| {
        ui.radio_value(&mut n.line, false, "No line");
        ui.radio_value(&mut n.line, true, "Solid line");
    });
    if n.line {
        egui::Grid::new(("fs-line", uid)).num_columns(2).show(ui, |ui| {
            ui.label("Colour:");
            egui::color_picker::color_edit_button_srgb(ui, &mut n.outline);
            ui.end_row();
            percent(ui, "Transparency:", &mut n.line_transparency);
            pixels(ui, "Width:", &mut n.width, 24.0);
            ui.label("Dash type:");
            pick(ui, ("fs-dash", uid), &mut n.dash, &DASHES);
            ui.end_row();
        });
    }
}

fn effects_page(ui: &mut Ui, app: &App, uid: u64, st: &mut FormatState, has_text: bool) {
    if has_text {
        ui.horizontal(|ui| {
            ui.radio_value(&mut st.text_effects, false, "Shape");
            ui.radio_value(&mut st.text_effects, true, "Text");
        });
    }
    let fx = if st.text_effects { &mut st.now.text_fx } else { &mut st.now.fx };
    let section = |ui: &mut Ui, title: &str, on: bool| -> bool {
        let mut on = on;
        ui.checkbox(&mut on, egui::RichText::new(title).strong());
        on
    };
    // Shadow.
    let on = section(ui, "Shadow", fx.shadow.is_some());
    if on != fx.shadow.is_some() {
        fx.shadow = on.then(Shadow::default);
    }
    if let Some(s) = fx.shadow.as_mut() {
        egui::Grid::new(("fs-shadow", uid)).num_columns(2).show(ui, |ui| {
            ui.label("Kind:");
            ui.horizontal(|ui| {
                for (k, _, label) in SHADOW_KINDS {
                    ui.radio_value(&mut s.kind, k, label);
                }
            });
            ui.end_row();
            optional_color(ui, "Colour:", &mut s.color);
            let mut t = s.transparency * 100.0;
            percent(ui, "Transparency:", &mut t);
            s.transparency = t / 100.0;
            pixels(ui, "Blur:", &mut s.blur, 100.0);
            degrees(ui, "Angle:", &mut s.angle, 359.0);
            pixels(ui, "Distance:", &mut s.distance, 200.0);
        });
    }
    // Reflection.
    let on = section(ui, "Reflection", fx.reflection.is_some());
    if on != fx.reflection.is_some() {
        fx.reflection = on.then(Reflection::default);
    }
    if let Some(r) = fx.reflection.as_mut() {
        egui::Grid::new(("fs-reflection", uid)).num_columns(2).show(ui, |ui| {
            let mut t = r.transparency * 100.0;
            percent(ui, "Transparency:", &mut t);
            r.transparency = t / 100.0;
            let mut size = r.size * 100.0;
            percent(ui, "Size:", &mut size);
            r.size = size / 100.0;
            pixels(ui, "Distance:", &mut r.distance, 100.0);
            pixels(ui, "Blur:", &mut r.blur, 100.0);
        });
    }
    // Glow.
    let on = section(ui, "Glow", fx.glow.is_some());
    if on != fx.glow.is_some() {
        fx.glow = on.then(Glow::default);
    }
    if let Some(g) = fx.glow.as_mut() {
        egui::Grid::new(("fs-glow", uid)).num_columns(2).show(ui, |ui| {
            optional_color(ui, "Colour:", &mut g.color);
            pixels(ui, "Size:", &mut g.size, 150.0);
            let mut t = g.transparency * 100.0;
            percent(ui, "Transparency:", &mut t);
            g.transparency = t / 100.0;
        });
    }
    heading(ui, app, "Soft Edges, 3-D Format and 3-D Rotation");
    egui::Grid::new(("fs-3d", uid)).num_columns(2).show(ui, |ui| {
        pixels(ui, "Soft edges:", &mut fx.soft_edges, 100.0);
        ui.label("Bevel:");
        let bevels: Vec<(&str, String)> = mailcraft_engine::cmd::insert::drawing::BEVELS
            .iter()
            .map(|b| {
                (*b, if b.is_empty() { "None".to_owned() } else { crate::ribbon::state::bevel_label(b) })
            })
            .collect();
        let shown = bevels.iter().find(|b| b.0 == fx.bevel).map_or_else(|| fx.bevel.clone(), |b| b.1.clone());
        egui::ComboBox::from_id_salt(("fs-bevel", uid)).selected_text(shown).width(170.0).show_ui(ui, |ui| {
            for (key, label) in &bevels {
                if ui.selectable_label(fx.bevel == *key, label).clicked() {
                    fx.bevel = (*key).to_owned();
                }
            }
        });
        ui.end_row();
        pixels(ui, "Bevel size:", &mut fx.bevel_size, 100.0);
        degrees(ui, "X rotation:", &mut fx.rot_x, 359.9);
        degrees(ui, "Y rotation:", &mut fx.rot_y, 359.9);
        degrees(ui, "Perspective:", &mut fx.perspective, 120.0);
    });
}

fn layout_page(ui: &mut Ui, app: &App, uid: u64, n: &mut Props, has_text: bool) {
    if has_text {
        heading(ui, app, "Text Box");
        egui::Grid::new(("fs-text", uid)).num_columns(2).show(ui, |ui| {
            ui.label("Vertical alignment:");
            pick(ui, ("fs-valign", uid), &mut n.valign, &VALIGNS);
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut n.autofit, "Resize shape to fit text");
            ui.end_row();
            let [l, t, r, b] = &mut n.inset;
            pixels(ui, "Left margin:", l, 200.0);
            pixels(ui, "Right margin:", r, 200.0);
            pixels(ui, "Top margin:", t, 200.0);
            pixels(ui, "Bottom margin:", b, 200.0);
        });
    }
    heading(ui, app, "Text Wrapping");
    pick(ui, ("fs-wrap", uid), &mut n.wrap, &WRAPS);
    heading(ui, app, "Properties");
    super::super::common::field(ui, "Name:", &mut n.name, 260.0);
    ui.label("Alt text:");
    ui.add(egui::TextEdit::multiline(&mut n.alt).desired_rows(3).desired_width(380.0));
}

fn size_page(ui: &mut Ui, app: &App, uid: u64, st: &mut FormatState) {
    let n = &mut st.now;
    heading(ui, app, "Size");
    let (ow, oh) = (n.w.max(1.0), n.h.max(1.0));
    egui::Grid::new(("fs-size", uid)).num_columns(2).show(ui, |ui| {
        ui.label("Height (px):");
        let hr = ui.add(egui::DragValue::new(&mut n.h).range(1.0..=4000.0));
        ui.end_row();
        ui.label("Width (px):");
        let wr = ui.add(egui::DragValue::new(&mut n.w).range(1.0..=4000.0));
        ui.end_row();
        if st.lock_ratio {
            if hr.changed() {
                n.w = (ow * n.h / oh).clamp(1.0, 4000.0);
            } else if wr.changed() {
                n.h = (oh * n.w / ow).clamp(1.0, 4000.0);
            }
        }
        ui.label("Rotation (°):");
        ui.add(egui::DragValue::new(&mut n.rotation).range(-360.0..=360.0));
        ui.end_row();
    });
    ui.checkbox(&mut st.lock_ratio, "Keep the proportions");
    heading(ui, app, "Position (from the top-left of the message text)");
    egui::Grid::new(("fs-pos", uid)).num_columns(2).show(ui, |ui| {
        ui.label("Across (px):");
        ui.add(egui::DragValue::new(&mut n.x).range(-1000.0..=100_000.0));
        ui.end_row();
        ui.label("Down (px):");
        ui.add(egui::DragValue::new(&mut n.y).range(-1000.0..=100_000.0));
        ui.end_row();
    });
}

/// Format Shape of a drawing (`page`: the page it opens on).
pub(super) fn format_shape(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    // The drawings OK formats: every target the pane was opened for (a group as a whole).
    let ids = {
        let ids = d.arr("ids");
        if ids.is_empty() { d.get("id").cloned().map(|v| vec![v]).unwrap_or_default() } else { ids }
    };
    let kind = d.str("kind");
    let has_text = !matches!(kind.as_str(), "canvas" | "group" | "line" | "freeform");
    let props = props_of(d);
    let page = {
        let p = d.str("page");
        if PAGES.iter().any(|(k, _)| *k == p) { p } else { "size".to_owned() }
    };
    let init = FormatState {
        page,
        now: props.clone(),
        was: props,
        lock_ratio: kind == "wordArt",
        text_effects: false,
        error: String::new(),
    };
    with_state(
        ctx,
        app,
        d.uid,
        move |_: &App| init,
        |st, app| {
            window(ctx, app, d, "Format Shape", 440.0, |ui, app| {
                ui.horizontal(|ui| {
                    for (key, title) in PAGES {
                        if ui.selectable_label(st.page == key, title).clicked() {
                            st.page = key.to_owned();
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().id_salt(("fs-scroll", d.uid)).max_height(420.0).show(ui, |ui| {
                    match st.page.as_str() {
                        "fill" => fill_page(ui, app, d.uid, st),
                        "effects" => effects_page(ui, app, d.uid, st, has_text),
                        "layout" => layout_page(ui, app, d.uid, &mut st.now, has_text),
                        _ => size_page(ui, app, d.uid, st),
                    }
                });
                if st.now.fill_kind == PICTURE && st.now.picture.is_none() && st.was.fill_kind != PICTURE {
                    muted(ui, app, "Choose a picture to fill the shape with.");
                }
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                match super::super::common::ok_cancel(ui, app, true) {
                    Some(true) => apply(app, cid, &ids, st),
                    Some(false) => Out::Close,
                    None => Out::Keep,
                }
            })
        },
    )
}

/// The `drawing.format` parameters OK sends for drawings `ids` (`None`: nothing changed).
fn ok_params(cid: u64, ids: &[Value], st: &FormatState) -> Option<Value> {
    let mut p = changes(&st.now, &st.was);
    if p.as_object().is_some_and(serde_json::Map::is_empty) {
        return None;
    }
    put(&mut p, "compose", json!(cid));
    put(&mut p, "ids", json!(ids));
    Some(p)
}

/// OK: what changed, as one `drawing.format` for every drawing the pane is for (a renamed drawing with a
/// new picture fill takes two).
fn apply(app: &mut App, cid: u64, ids: &[Value], st: &mut FormatState) -> Out {
    let Some(p) = ok_params(cid, ids, st) else { return Out::Close };
    let mut result = cmd(app, "drawing.format", p);
    if result.is_ok() && st.now.picture.is_some() && st.now.name != st.was.name {
        result = cmd(app, "drawing.format", json!({"compose": cid, "ids": ids, "name": st.now.name}));
    }
    match result {
        Ok(_) => Out::Close,
        Err(e) => {
            st.error = e;
            Out::Keep
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props() -> Props {
        Props {
            fill_kind: SOLID,
            fill: [10, 20, 30],
            transparency: 0.0,
            stops: default_stops([10, 20, 30]),
            angle: 90.0,
            radial: false,
            picture: None,
            line: true,
            outline: [0, 0, 0],
            width: 1.0,
            dash: String::new(),
            line_transparency: 0.0,
            fx: ShapeEffects::default(),
            text_fx: ShapeEffects::default(),
            inset: [7.0, 3.0, 7.0, 3.0],
            autofit: false,
            valign: "top".into(),
            wrap: "front".into(),
            name: "Rectangle 1".into(),
            alt: String::new(),
            x: 1.0,
            y: 2.0,
            w: 30.0,
            h: 40.0,
            rotation: 0.0,
        }
    }

    #[test]
    fn ok_sends_only_what_changed() {
        let was = props();
        assert_eq!(changes(&was, &was), json!({}));
        let mut now = was.clone();
        now.line = false;
        now.wrap = "square".into();
        now.fx.shadow = Some(Shadow::default());
        let p = changes(&now, &was);
        assert_eq!(p["outline"], Value::Null);
        assert_eq!(p["wrap"], "square");
        assert_eq!(p["effects"]["shadow"]["kind"], "outer");
        assert_eq!(p["effects"]["glow"], Value::Null);
        assert!(p.get("fill").is_none() && p.get("w").is_none());
        // A gradient sends every stop with positions and transparencies as fractions.
        let mut now = was.clone();
        now.fill_kind = GRADIENT;
        now.stops[1].transparency = 50.0;
        let p = changes(&now, &was);
        assert_eq!(p["stops"][1]["pos"], 1.0);
        assert_eq!(p["stops"][1]["transparency"], 0.5);
        // No fill drops the gradient too.
        let mut now = was.clone();
        now.fill_kind = NO_FILL;
        let p = changes(&now, &was);
        assert_eq!((p["noFill"].clone(), p["stops"].clone()), (json!(true), Value::Null));
    }

    /// Regression: Format Shape on a group formatted only the group's first drawing (its box, wrapping and
    /// name), and a new width also sent the old height.
    #[test]
    fn format_shape_on_a_group_formats_the_group() {
        use mailcraft_engine::Notice;
        use mailcraft_model::rich::{DrawingKind, WrapStyle};
        let mut s = mailcraft_engine::sample::sample_session();
        let cid = s.open_compose(mailcraft_engine::compose::ComposeState::default());
        let mut ids = Vec::new();
        for x in [0, 200] {
            let r = s
                .run(
                    "insert.shape",
                    &json!({"compose": cid, "kind": "rectangle", "x": x, "y": 0, "w": 100, "h": 50}),
                )
                .unwrap();
            ids.push(r["drawing"].as_u64().unwrap());
        }
        s.run("drawing.select", &json!({"compose": cid, "ids": ids})).unwrap();
        let gid = s.run("drawing.group", &json!({"compose": cid})).unwrap()["group"].as_u64().unwrap();
        s.run("drawing.format", &json!({"compose": cid})).unwrap();
        let Some(Notice::OpenDialog(name, params)) = s.notices.last().cloned() else { panic!("no pane") };
        assert_eq!(name, "formatShape");
        let d = Dlg { name: &name, params: &params, uid: 1, top: true };
        let was = props_of(&d);
        assert_eq!((was.x, was.w, was.h), (0.0, 300.0, 50.0), "the group's own box");
        assert_eq!(d.arr("ids"), vec![json!(gid)]);
        let mut now = was.clone();
        now.w = 600.0;
        now.wrap = "square".into();
        now.name = "Logo".into();
        now.fill_kind = SOLID;
        now.fill = [200, 0, 0];
        let st = FormatState {
            page: "size".into(),
            now,
            was,
            lock_ratio: false,
            text_effects: false,
            error: String::new(),
        };
        let p = ok_params(cid, &d.arr("ids"), &st).unwrap();
        assert!(p.get("h").is_none(), "only the side that changed");
        s.run("drawing.format", &p).unwrap();
        let ds = &s.composes[&cid].body.drawings;
        let g = ds.iter().find(|x| u64::from(x.id) == gid).unwrap();
        assert_eq!(g.kind, DrawingKind::Group);
        assert_eq!((g.w, g.h, g.wrap, g.name.as_str()), (600.0, 50.0, WrapStyle::Square, "Logo"));
        for id in &ids {
            let m = ds.iter().find(|x| u64::from(x.id) == *id).unwrap();
            assert_eq!((m.w, m.h), (200.0, 50.0), "members scale with the group");
            assert_eq!(m.fill, Some(Color::rgb(200, 0, 0)), "fill goes to the members");
            assert_ne!(m.name, "Logo");
        }
    }

    #[test]
    fn effect_options_round_trip_through_the_engine() {
        let fx = ShapeEffects {
            glow: Some(Glow { color: Some(Color::rgb(255, 0, 0)), size: 9.0, transparency: 0.25 }),
            bevel: "circle".into(),
            rot_x: 30.0,
            ..ShapeEffects::default()
        };
        let mut s = mailcraft_engine::sample::sample_session();
        let cid = s.open_compose(mailcraft_engine::compose::ComposeState::default());
        let r = s.run("insert.shape", &json!({"compose": cid, "kind": "rectangle"})).unwrap();
        let id = r["drawing"].as_u64().unwrap();
        s.run("drawing.format", &json!({"compose": cid, "id": id, "effects": fx_params(&fx)})).unwrap();
        let got = s.composes[&cid].body.drawings.iter().find(|d| u64::from(d.id) == id).unwrap();
        assert_eq!(got.fx.as_deref(), Some(&fx));
    }
}
