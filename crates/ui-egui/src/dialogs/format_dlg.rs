//! Format Text dialogs of the compose window: Font (Font and Advanced tabs), Paragraph (Indents and Spacing,
//! Line and Page Breaks), the colour picker behind every "More Colors..." (Standard and Custom tabs), and the
//! small style forms (Create New Style from Formatting, Save as a New Style Set, Custom Paragraph Spacing).
//! Each dialog opens with the formatting at the caret and applies its result with one command, so OK is a
//! single undo step. Layout and wording are our own.

use egui::{Color32, Context, RichText, Sense, Ui, vec2};
use serde_json::{Map, Value, json};

use super::common::{self, Dlg, Out, btn, footer, muted, ok_cancel, primary, window, with_state};
use super::compose_dlg::{Form, Kind, cmd, f, run_form};
use crate::App;
use crate::body::{BodyOptions, BodyView};
use mailcraft_model::rich::{Align, Block, Doc, Inline, LineRule, ParaStyle, TextStyle, UnderlineKind};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "font" => font(ctx, app, d),
        "paragraph" => paragraph(ctx, app, d),
        "colors" => colors(ctx, app, d, d.str("target")),
        "pageColor" => colors(ctx, app, d, "page".to_owned()),
        "createStyle" => create_style(ctx, app, d),
        "saveStyleSet" => save_style_set(ctx, app, d),
        "customSpacing" => custom_spacing(ctx, app, d),
        _ => return None,
    })
}

/// A tab strip; returns true when the tab changed.
fn tabs(ui: &mut Ui, app: &App, current: &mut usize, names: &[&str]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        for (i, name) in names.iter().enumerate() {
            let on = *current == i;
            let text =
                if on { RichText::new(*name).strong().color(app.theme.accent) } else { RichText::new(*name) };
            if ui.selectable_label(on, text).clicked() && !on {
                *current = i;
                changed = true;
            }
        }
    });
    ui.separator();
    changed
}

/// A body-style preview of `doc` in a white box `height` points tall.
fn preview(ui: &mut Ui, view: &mut BodyView, doc: &Doc, height: f32) {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .stroke(egui::Stroke::new(1.0, Color32::from_gray(0xb0)))
        .inner_margin(6.0)
        .show(ui, |ui| {
            ui.set_min_height(height);
            ui.set_max_height(height);
            egui::ScrollArea::vertical().id_salt("fmt-preview").max_height(height).show(ui, |ui| {
                let opts = BodyOptions { paint_background: false, ..BodyOptions::default() };
                let _ = crate::body::show_doc_with(ui, view, doc, &opts);
            });
        });
}

fn rgb_of(v: Option<&Value>) -> Option<[u8; 3]> {
    let c = match v? {
        Value::String(s) => mailcraft_geom::Color::parse_hex(s)?,
        other => serde_json::from_value::<mailcraft_geom::Color>(other.clone()).ok()?,
    };
    Some([c.r, c.g, c.b])
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn color32(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// A colour button with an Automatic choice: `None` = automatic.
fn auto_color(ui: &mut Ui, id: &str, value: &mut Option<[u8; 3]>) {
    ui.horizontal(|ui| {
        let mut auto = value.is_none();
        if ui.checkbox(&mut auto, "Automatic").changed() {
            *value = if auto { None } else { Some([0, 0, 0]) };
        }
        if let Some(c) = value.as_mut() {
            ui.push_id(id, |ui| egui::color_picker::color_edit_button_srgb(ui, c));
        }
    });
}

// ---------------------------------------------------------------------------------------------------------
// Font

const SIZES: [&str; 17] =
    ["8", "9", "10", "10.5", "11", "12", "14", "16", "18", "20", "22", "24", "26", "28", "36", "48", "72"];
const UNDERLINES: [&str; 6] = ["(none)", "Single", "Double", "Dotted", "Dashed", "Wavy"];
const UNDERLINE_KINDS: [&str; 6] = ["none", "single", "double", "dotted", "dashed", "wavy"];

/// What the Font dialog edits (compared with the opening values: only changes are applied).
#[derive(Clone, Debug, PartialEq)]
struct FontVals {
    family: String,
    size: String,
    bold: bool,
    italic: bool,
    color: Option<[u8; 3]>,
    underline: usize,
    underline_color: Option<[u8; 3]>,
    strike: bool,
    double_strike: bool,
    superscript: bool,
    subscript: bool,
    small_caps: bool,
    all_caps: bool,
    spacing: f64,
    position: f64,
}

impl FontVals {
    fn from_style(s: &Value) -> FontVals {
        let b = |k: &str| s.get(k).and_then(Value::as_bool).unwrap_or(false);
        let n = |k: &str| s.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let underline = if b("underline") {
            let kind = s.get("underline_kind").and_then(Value::as_str).unwrap_or("single");
            UNDERLINE_KINDS.iter().position(|k| *k == kind).unwrap_or(1)
        } else {
            0
        };
        FontVals {
            family: s
                .get("font_family")
                .and_then(Value::as_str)
                .unwrap_or(mailcraft_fonts::BODY_FAMILY)
                .to_owned(),
            size: s
                .get("size_pt")
                .and_then(Value::as_f64)
                .map_or_else(|| format!("{}", mailcraft_fonts::BODY_SIZE_PT), |n| format!("{n}")),
            bold: b("bold"),
            italic: b("italic"),
            color: rgb_of(s.get("color")),
            underline,
            underline_color: rgb_of(s.get("underline_color")),
            strike: b("strike"),
            double_strike: b("double_strike"),
            superscript: b("superscript"),
            subscript: b("subscript"),
            small_caps: b("small_caps"),
            all_caps: b("all_caps"),
            spacing: n("spacing_pt"),
            position: n("position_pt"),
        }
    }

    /// The model style these values describe (for the preview).
    fn text_style(&self) -> TextStyle {
        let some = |b: bool| b.then_some(true);
        let kind = UNDERLINE_KINDS.get(self.underline).and_then(|k| UnderlineKind::from_name(k));
        #[allow(clippy::cast_possible_truncation)]
        let pt = |v: f64| (v != 0.0 && v.is_finite()).then_some(v.clamp(-200.0, 200.0) as f32);
        TextStyle {
            font_family: Some(self.family.clone()).filter(|f| !f.trim().is_empty()),
            size_pt: self
                .size
                .trim()
                .parse::<f32>()
                .ok()
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(1.0, 72.0)),
            bold: some(self.bold),
            italic: some(self.italic),
            color: self.color.map(|c| mailcraft_geom::Color::rgb(c[0], c[1], c[2])),
            underline: some(self.underline > 0),
            underline_kind: kind.filter(|k| *k != UnderlineKind::Single && self.underline > 0),
            underline_color: self.underline_color.map(|c| mailcraft_geom::Color::rgb(c[0], c[1], c[2])),
            strike: some(self.strike),
            double_strike: some(self.double_strike),
            superscript: some(self.superscript),
            subscript: some(self.subscript),
            small_caps: some(self.small_caps),
            all_caps: some(self.all_caps),
            spacing_pt: pt(self.spacing),
            position_pt: pt(self.position),
            ..TextStyle::default()
        }
    }

    /// `format.fontDialog {"apply"}` parameters for what differs from `was`.
    fn changes(&self, was: &FontVals) -> Map<String, Value> {
        let mut m = Map::new();
        let auto = |c: Option<[u8; 3]>| c.map_or_else(|| "automatic".to_owned(), hex);
        if self.family.trim() != was.family.trim() && !self.family.trim().is_empty() {
            m.insert("family".into(), json!(self.family.trim()));
        }
        if self.size.trim() != was.size.trim()
            && let Ok(pt) = self.size.trim().parse::<f64>()
        {
            m.insert("pt".into(), json!(pt));
        }
        for (key, now, then) in [
            ("bold", self.bold, was.bold),
            ("italic", self.italic, was.italic),
            ("strike", self.strike, was.strike),
            ("doubleStrike", self.double_strike, was.double_strike),
            ("superscript", self.superscript, was.superscript),
            ("subscript", self.subscript, was.subscript),
            ("smallCaps", self.small_caps, was.small_caps),
            ("allCaps", self.all_caps, was.all_caps),
        ] {
            if now != then {
                m.insert(key.into(), json!(now));
            }
        }
        if self.color != was.color {
            m.insert("color".into(), json!(auto(self.color)));
        }
        if self.underline != was.underline {
            m.insert(
                "underline".into(),
                json!(UNDERLINE_KINDS.get(self.underline).copied().unwrap_or("none")),
            );
        }
        if self.underline_color != was.underline_color {
            m.insert("underlineColor".into(), json!(auto(self.underline_color)));
        }
        if (self.spacing - was.spacing).abs() > f64::EPSILON {
            m.insert("spacing".into(), json!(self.spacing));
        }
        if (self.position - was.position).abs() > f64::EPSILON {
            m.insert("position".into(), json!(self.position));
        }
        m
    }
}

#[derive(Clone)]
struct FontDlg {
    tab: usize,
    vals: FontVals,
    initial: FontVals,
    heading_font: String,
    body_font: String,
    fonts: Vec<String>,
    view: BodyView,
    error: String,
}

/// Picks a font name from a filtered list under the name field.
fn font_list(ui: &mut Ui, st: &mut FontDlg) {
    ui.add(egui::TextEdit::singleline(&mut st.vals.family).desired_width(210.0));
    let typed = st.vals.family.trim().to_lowercase();
    let themed = [
        (format!("{} (Headings)", st.heading_font), st.heading_font.clone()),
        (format!("{} (Body)", st.body_font), st.body_font.clone()),
    ];
    egui::ScrollArea::vertical().id_salt("font-list").max_height(110.0).show(ui, |ui| {
        ui.set_min_width(210.0);
        for (label, family) in &themed {
            if ui.selectable_label(st.vals.family == *family, label.as_str()).clicked() {
                st.vals.family.clone_from(family);
            }
        }
        ui.separator();
        // Typing narrows the list to the names that start with the text (the whole list once it matches).
        let exact = st.fonts.iter().any(|n| n.to_lowercase() == typed);
        for name in
            st.fonts.iter().filter(|n| exact || typed.is_empty() || n.to_lowercase().starts_with(&typed))
        {
            if ui.selectable_label(st.vals.family == *name, name.as_str()).clicked() {
                st.vals.family.clone_from(name);
            }
        }
    });
}

fn font(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let init = |_: &App| {
        let vals = FontVals::from_style(d.get("style").unwrap_or(&Value::Null));
        FontDlg {
            tab: usize::from(d.str("tab") == "advanced"),
            initial: vals.clone(),
            vals,
            heading_font: Some(d.str("headingFont"))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Aptos Display".into()),
            body_font: Some(d.str("bodyFont")).filter(|s| !s.is_empty()).unwrap_or_else(|| "Aptos".into()),
            fonts: crate::compose::font_names(),
            view: BodyView::default(),
            error: String::new(),
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Font", 520.0, |ui, app| {
            tabs(ui, app, &mut st.tab, &["Font", "Advanced"]);
            if st.tab == 0 {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label("Font:");
                        font_list(ui, st);
                    });
                    ui.vertical(|ui| {
                        ui.label("Font style:");
                        ui.checkbox(&mut st.vals.bold, "Bold");
                        ui.checkbox(&mut st.vals.italic, "Italic");
                    });
                    ui.vertical(|ui| {
                        ui.label("Size:");
                        ui.add(egui::TextEdit::singleline(&mut st.vals.size).desired_width(56.0));
                        egui::ScrollArea::vertical().id_salt("size-list").max_height(110.0).show(ui, |ui| {
                            for s in SIZES {
                                if ui.selectable_label(st.vals.size == s, s).clicked() {
                                    st.vals.size = s.to_owned();
                                }
                            }
                        });
                    });
                });
                egui::Grid::new(("font-colors", uid)).num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                    ui.label("Font color:");
                    auto_color(ui, "fc", &mut st.vals.color);
                    ui.end_row();
                    ui.label("Underline style:");
                    common::combo_idx(ui, ("ul", uid), &mut st.vals.underline, &UNDERLINES, 140.0);
                    ui.end_row();
                    ui.label("Underline color:");
                    ui.add_enabled_ui(st.vals.underline > 0, |ui| {
                        auto_color(ui, "uc", &mut st.vals.underline_color)
                    });
                    ui.end_row();
                });
                common::heading(ui, app, "Effects");
                egui::Grid::new(("font-effects", uid)).num_columns(2).spacing([24.0, 2.0]).show(ui, |ui| {
                    let v = &mut st.vals;
                    if ui.checkbox(&mut v.strike, "Strikethrough").changed() && v.strike {
                        v.double_strike = false;
                    }
                    if ui.checkbox(&mut v.small_caps, "Small caps").changed() && v.small_caps {
                        v.all_caps = false;
                    }
                    ui.end_row();
                    if ui.checkbox(&mut v.double_strike, "Double strikethrough").changed() && v.double_strike
                    {
                        v.strike = false;
                    }
                    if ui.checkbox(&mut v.all_caps, "All caps").changed() && v.all_caps {
                        v.small_caps = false;
                    }
                    ui.end_row();
                    if ui.checkbox(&mut v.superscript, "Superscript").changed() && v.superscript {
                        v.subscript = false;
                    }
                    ui.end_row();
                    if ui.checkbox(&mut v.subscript, "Subscript").changed() && v.subscript {
                        v.superscript = false;
                    }
                    ui.end_row();
                });
            } else {
                common::heading(ui, app, "Character Spacing");
                egui::Grid::new(("font-adv", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Spacing (pt, negative = condensed):");
                    ui.add(
                        egui::DragValue::new(&mut st.vals.spacing)
                            .range(-50.0..=200.0)
                            .speed(0.1)
                            .suffix(" pt"),
                    );
                    ui.end_row();
                    ui.label("Position (pt, negative = lowered):");
                    ui.add(
                        egui::DragValue::new(&mut st.vals.position)
                            .range(-200.0..=200.0)
                            .speed(0.5)
                            .suffix(" pt"),
                    );
                    ui.end_row();
                });
                muted(
                    ui,
                    app,
                    "Spacing widens or narrows the gaps between characters; position raises or lowers the text.",
                );
            }
            common::heading(ui, app, "Preview");
            let doc = Doc {
                blocks: vec![Block::Paragraph {
                    style: ParaStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "The quick brown fox".into(),
                        style: st.vals.text_style(),
                    }],
                }],
                ..Doc::default()
            };
            preview(ui, &mut st.view, &doc, 46.0);
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let changes = st.vals.changes(&st.initial);
                    if changes.is_empty() {
                        return Out::Close;
                    }
                    match cmd(app, "format.fontDialog", json!({"compose": compose, "apply": changes})) {
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
// Paragraph

const ALIGNS: [&str; 4] = ["Left", "Centered", "Right", "Justified"];
const ALIGN_KEYS: [&str; 4] = ["left", "center", "right", "justify"];
const SPECIALS: [&str; 3] = ["(none)", "First line", "Hanging"];
const SPECIAL_KEYS: [&str; 3] = ["none", "firstLine", "hanging"];
const LINES: [&str; 6] = ["Single", "1.5 lines", "Double", "At least", "Exactly", "Multiple"];
const LINE_KEYS: [&str; 6] = ["single", "1.5", "double", "atLeast", "exactly", "multiple"];

#[derive(Clone, Debug, PartialEq)]
struct ParaVals {
    align: usize,
    left: f64,
    right: f64,
    special: usize,
    by: f64,
    before: f64,
    after: f64,
    line: usize,
    at: f64,
    widow: bool,
    keep_next: bool,
    keep_together: bool,
    break_before: bool,
}

impl ParaVals {
    fn from_json(p: &Value) -> ParaVals {
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let n = |k: &str| p.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let b = |k: &str, d: bool| p.get(k).and_then(Value::as_bool).unwrap_or(d);
        let pos = |list: &[&str], v: &str| list.iter().position(|x| *x == v).unwrap_or(0);
        ParaVals {
            align: pos(&ALIGN_KEYS, &s("align")),
            left: n("left"),
            right: n("right"),
            special: pos(&SPECIAL_KEYS, &s("special")),
            by: n("by"),
            before: n("before"),
            after: n("after"),
            line: pos(&LINE_KEYS, &s("line")),
            at: p.get("at").and_then(Value::as_f64).unwrap_or(1.0),
            widow: b("widowControl", true),
            keep_next: b("keepWithNext", false),
            keep_together: b("keepTogether", false),
            break_before: b("pageBreakBefore", false),
        }
    }

    /// The fixed line height in points the rule needs (At least / Exactly) or the multiple.
    fn at_for_rule(&self) -> f64 {
        match self.line {
            3 | 4 => self.at.max(1.0),
            5 => self.at.clamp(0.25, 10.0),
            _ => self.at,
        }
    }

    /// `format.paragraphDialog {"apply"}` parameters for what differs from `was` (the values the dialog
    /// opened with, which are the caret paragraph's). Sending only the changes keeps the other selected
    /// paragraphs' own indents, spacing and line rules. The special indent goes with its `by`, the line
    /// rule with its `at`.
    fn changes(&self, was: &ParaVals) -> Map<String, Value> {
        let mut m = Map::new();
        let differs = |a: f64, b: f64| (a - b).abs() > 1e-9;
        if self.align != was.align {
            m.insert("align".into(), json!(ALIGN_KEYS.get(self.align).copied().unwrap_or("left")));
        }
        for (key, now, then) in [
            ("left", self.left, was.left),
            ("right", self.right, was.right),
            ("before", self.before, was.before),
            ("after", self.after, was.after),
        ] {
            if differs(now, then) {
                m.insert(key.into(), json!(now));
            }
        }
        if self.special != was.special || (self.special > 0 && differs(self.by, was.by)) {
            m.insert("special".into(), json!(SPECIAL_KEYS.get(self.special).copied().unwrap_or("none")));
            m.insert("by".into(), json!(self.by));
        }
        if self.line != was.line || (self.line >= 3 && differs(self.at_for_rule(), was.at_for_rule())) {
            m.insert("line".into(), json!(LINE_KEYS.get(self.line).copied().unwrap_or("single")));
            m.insert("at".into(), json!(self.at_for_rule()));
        }
        for (key, now, then) in [
            ("widowControl", self.widow, was.widow),
            ("keepWithNext", self.keep_next, was.keep_next),
            ("keepTogether", self.keep_together, was.keep_together),
            ("pageBreakBefore", self.break_before, was.break_before),
        ] {
            if now != then {
                m.insert(key.into(), json!(now));
            }
        }
        m
    }

    /// The model paragraph style these values describe (for the preview).
    #[allow(clippy::cast_possible_truncation)]
    fn para_style(&self) -> ParaStyle {
        let px = |pt: f64| (pt.clamp(0.0, 1584.0) * 4.0 / 3.0) as f32;
        let first = match self.special {
            1 => px(self.by),
            2 => -px(self.by),
            _ => 0.0,
        };
        let (line_height, line_px) = match self.line {
            1 => (Some(1.5), None),
            2 => (Some(2.0), None),
            3 => (None, Some((px(self.at.max(1.0)), LineRule::AtLeast))),
            4 => (None, Some((px(self.at.max(1.0)), LineRule::Exactly))),
            5 => (Some(self.at.clamp(0.25, 10.0) as f32), None),
            _ => (Some(1.0), None),
        };
        ParaStyle {
            align: [Align::Start, Align::Center, Align::End, Align::Justify]
                .get(self.align)
                .copied()
                .unwrap_or_default(),
            // A hanging indent moves the following lines in at least as far as it hangs (as the engine
            // stores it), so the first line never starts left of the paragraph.
            indent_px: px(self.left).max(-first),
            right_indent_px: px(self.right),
            first_line_px: first,
            space_before_px: px(self.before),
            space_after_px: px(self.after),
            line_height,
            line_px,
            ..ParaStyle::default()
        }
    }
}

#[derive(Clone)]
struct ParaDlg {
    tab: usize,
    vals: ParaVals,
    initial: ParaVals,
    view: BodyView,
    error: String,
}

const SAMPLE: &str = "Sample text shows how the paragraph will look with these settings applied to it, \
                      line after line of ordinary words.";

fn paragraph(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let init = |_: &App| {
        let vals = ParaVals::from_json(d.get("paragraph").unwrap_or(&Value::Null));
        ParaDlg {
            tab: usize::from(d.str("tab") == "breaks"),
            initial: vals.clone(),
            vals,
            view: BodyView::default(),
            error: String::new(),
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Paragraph", 520.0, |ui, app| {
            tabs(ui, app, &mut st.tab, &["Indents and Spacing", "Line and Page Breaks"]);
            let v = &mut st.vals;
            if st.tab == 0 {
                common::heading(ui, app, "General");
                ui.horizontal(|ui| {
                    ui.label("Alignment:");
                    common::combo_idx(ui, ("pa", uid), &mut v.align, &ALIGNS, 120.0);
                });
                common::heading(ui, app, "Indentation");
                egui::Grid::new(("pi", uid)).num_columns(4).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Left:");
                    ui.add(egui::DragValue::new(&mut v.left).range(0.0..=1584.0).speed(1.0).suffix(" pt"));
                    ui.label("Special:");
                    common::combo_idx(ui, ("ps", uid), &mut v.special, &SPECIALS, 100.0);
                    ui.end_row();
                    ui.label("Right:");
                    ui.add(egui::DragValue::new(&mut v.right).range(0.0..=1584.0).speed(1.0).suffix(" pt"));
                    ui.label("By:");
                    ui.add_enabled(
                        v.special > 0,
                        egui::DragValue::new(&mut v.by).range(0.0..=1584.0).speed(1.0).suffix(" pt"),
                    );
                    ui.end_row();
                });
                common::heading(ui, app, "Spacing");
                egui::Grid::new(("psp", uid)).num_columns(4).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Before:");
                    ui.add(egui::DragValue::new(&mut v.before).range(0.0..=1584.0).speed(1.0).suffix(" pt"));
                    ui.label("Line spacing:");
                    if common::combo_idx(ui, ("pl", uid), &mut v.line, &LINES, 100.0) {
                        // Each rule starts from a sensible "At" value.
                        v.at = match v.line {
                            3 | 4 => 12.0,
                            5 => 3.0,
                            _ => v.at,
                        };
                    }
                    ui.end_row();
                    ui.label("After:");
                    ui.add(egui::DragValue::new(&mut v.after).range(0.0..=1584.0).speed(1.0).suffix(" pt"));
                    ui.label("At:");
                    match v.line {
                        3 | 4 => ui.add(
                            egui::DragValue::new(&mut v.at).range(1.0..=1584.0).speed(0.5).suffix(" pt"),
                        ),
                        5 => ui.add(egui::DragValue::new(&mut v.at).range(0.25..=10.0).speed(0.05)),
                        _ => ui.add_enabled(false, egui::Label::new("")),
                    };
                    ui.end_row();
                });
            } else {
                common::heading(ui, app, "Pagination");
                ui.checkbox(&mut v.widow, "Widow/Orphan control");
                ui.checkbox(&mut v.keep_next, "Keep with next");
                ui.checkbox(&mut v.keep_together, "Keep lines together");
                ui.checkbox(&mut v.break_before, "Page break before");
                muted(ui, app, "These take effect when the message is printed.");
            }
            common::heading(ui, app, "Preview");
            let grey = TextStyle {
                color: Some(mailcraft_geom::Color::rgb(0xa0, 0xa0, 0xa0)),
                size_pt: Some(8.0),
                ..TextStyle::default()
            };
            let sample = TextStyle { size_pt: Some(8.0), ..TextStyle::default() };
            let para = |style: ParaStyle, ts: &TextStyle| Block::Paragraph {
                style,
                inlines: vec![Inline::Text { text: SAMPLE.into(), style: ts.clone() }],
            };
            let plain = ParaStyle { space_after_px: 4.0, ..ParaStyle::default() };
            let doc = Doc {
                blocks: vec![para(plain.clone(), &grey), para(v.para_style(), &sample), para(plain, &grey)],
                ..Doc::default()
            };
            preview(ui, &mut st.view, &doc, 110.0);
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let changes = st.vals.changes(&st.initial);
                    if changes.is_empty() {
                        return Out::Close;
                    }
                    match cmd(app, "format.paragraphDialog", json!({"compose": compose, "apply": changes})) {
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
// Colours

/// The Standard tab: twelve hues in six shades, then a grey ramp (our own palette).
pub(crate) fn standard_palette() -> Vec<[u8; 3]> {
    let mut out = Vec::with_capacity(84);
    for l in [0.85, 0.72, 0.6, 0.48, 0.36, 0.24] {
        for h in 0..12 {
            out.push(hsl_to_rgb(f64::from(h) * 30.0, 0.75, l));
        }
    }
    for g in 0..12u8 {
        let v = 255u8.saturating_sub(g.saturating_mul(23));
        out.push([v, v, v]);
    }
    out
}

/// HSL (hue in degrees, saturation and lightness 0..1) → RGB.
pub(crate) fn hsl_to_rgb(h: f64, s: f64, l: f64) -> [u8; 3] {
    let (h, s, l) = (h.rem_euclid(360.0) / 360.0, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |t: f64| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let b = (v * 255.0).round().clamp(0.0, 255.0) as u8;
        b
    };
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

/// RGB → HSL (hue in degrees, saturation and lightness 0..1).
pub(crate) fn rgb_to_hsl(c: [u8; 3]) -> (f64, f64, f64) {
    let [r, g, b] = c.map(|v| f64::from(v) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d.abs() < f64::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if (max - r).abs() < f64::EPSILON {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if (max - g).abs() < f64::EPSILON {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

#[derive(Clone)]
struct ColorsDlg {
    tab: usize,
    rgb: [u8; 3],
    current: Option<[u8; 3]>,
    hex: String,
    /// Custom tab colour model: 0 = RGB, 1 = HSL.
    model: usize,
    error: String,
}

/// The command a colour target runs, and the dialog title.
fn color_target(target: &str) -> (&'static str, &'static str) {
    match target {
        "highlight" => ("format.highlight", "Highlight Colors"),
        "shading" => ("format.shading", "Shading Colors"),
        "page" => ("compose.pageColor", "Page Color"),
        "shapeFill" => ("drawing.fill", "Shape Fill Colors"),
        "shapeOutline" => ("drawing.outline", "Outline Colors"),
        "textFill" => ("drawing.textColor", "Text Fill Colors"),
        "textOutline" => ("drawing.textOutline", "Text Outline Colors"),
        _ => ("format.color", "Colors"),
    }
}

fn colors(ctx: &Context, app: &mut App, d: &Dlg<'_>, target: String) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let current = rgb_of(d.get("current")).or_else(|| {
        let cid = compose?;
        let c = app.s.composes.get(&cid)?;
        let col = match target.as_str() {
            "page" => c.body.background,
            "highlight" => c.style_at_caret().highlight,
            "shading" => c.para_at_caret().background,
            _ => c.style_at_caret().color,
        }?;
        Some([col.r, col.g, col.b])
    });
    let init = |_: &App| {
        let rgb = current.unwrap_or([0x1f, 0x5f, 0xbf]);
        ColorsDlg { tab: 0, rgb, current, hex: hex(rgb), model: 0, error: String::new() }
    };
    let (command, title) = color_target(&target);
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, title, 440.0, |ui, app| {
            tabs(ui, app, &mut st.tab, &["Standard", "Custom"]);
            let before = st.rgb;
            if st.tab == 0 {
                let palette = standard_palette();
                egui::Grid::new(("std-colors", uid)).spacing([2.0, 2.0]).show(ui, |ui| {
                    for (i, c) in palette.iter().enumerate() {
                        let (r, resp) = ui.allocate_exact_size(vec2(22.0, 18.0), Sense::click());
                        ui.painter().rect_filled(r, 2.0, color32(*c));
                        if *c == st.rgb {
                            ui.painter().rect_stroke(
                                r,
                                2.0,
                                egui::Stroke::new(2.0, app.theme.text),
                                egui::StrokeKind::Inside,
                            );
                        }
                        resp.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, hex(*c))
                        });
                        let resp = resp.on_hover_text(hex(*c));
                        if resp.clicked() {
                            st.rgb = *c;
                        }
                        if i % 12 == 11 {
                            ui.end_row();
                        }
                    }
                });
            } else {
                let mut c32 = color32(st.rgb);
                if egui::color_picker::color_picker_color32(ui, &mut c32, egui::color_picker::Alpha::Opaque) {
                    st.rgb = [c32.r(), c32.g(), c32.b()];
                }
                ui.horizontal(|ui| {
                    ui.label("Color model:");
                    common::combo_idx(ui, ("cm", uid), &mut st.model, &["RGB", "HSL"], 80.0);
                });
                if st.model == 0 {
                    egui::Grid::new(("rgb", uid)).num_columns(2).show(ui, |ui| {
                        for (i, label) in ["Red:", "Green:", "Blue:"].iter().enumerate() {
                            ui.label(*label);
                            if let Some(v) = st.rgb.get_mut(i) {
                                ui.add(egui::DragValue::new(v).range(0..=255));
                            }
                            ui.end_row();
                        }
                    });
                } else {
                    let (mut h, mut s, mut l) = rgb_to_hsl(st.rgb);
                    let (mut sp, mut lp) = (s * 100.0, l * 100.0);
                    let mut changed = false;
                    egui::Grid::new(("hsl", uid)).num_columns(2).show(ui, |ui| {
                        ui.label("Hue:");
                        changed |=
                            ui.add(egui::DragValue::new(&mut h).range(0.0..=359.0).suffix("°")).changed();
                        ui.end_row();
                        ui.label("Saturation:");
                        changed |=
                            ui.add(egui::DragValue::new(&mut sp).range(0.0..=100.0).suffix(" %")).changed();
                        ui.end_row();
                        ui.label("Lightness:");
                        changed |=
                            ui.add(egui::DragValue::new(&mut lp).range(0.0..=100.0).suffix(" %")).changed();
                        ui.end_row();
                    });
                    if changed {
                        (s, l) = (sp / 100.0, lp / 100.0);
                        st.rgb = hsl_to_rgb(h, s, l);
                    }
                }
                ui.horizontal(|ui| {
                    ui.label("Hex:");
                    let r = ui.add(egui::TextEdit::singleline(&mut st.hex).desired_width(80.0));
                    if r.changed()
                        && let Some(c) = mailcraft_geom::Color::parse_hex(st.hex.trim())
                    {
                        st.rgb = [c.r, c.g, c.b];
                    }
                });
            }
            if st.rgb != before {
                st.hex = hex(st.rgb);
            }
            ui.horizontal(|ui| {
                ui.label("New");
                let (r, _) = ui.allocate_exact_size(vec2(44.0, 22.0), Sense::hover());
                ui.painter().rect_filled(r, 0.0, color32(st.rgb));
                ui.label("Current");
                let (r, _) = ui.allocate_exact_size(vec2(44.0, 22.0), Sense::hover());
                match st.current {
                    Some(c) => {
                        ui.painter().rect_filled(r, 0.0, color32(c));
                    }
                    None => {
                        ui.painter().rect_stroke(
                            r,
                            0.0,
                            egui::Stroke::new(1.0, app.theme.separator),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            });
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            let run =
                |app: &mut App, color: Value| cmd(app, command, json!({"compose": compose, "color": color}));
            footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    return Out::Close;
                }
                let ok = primary(ui, app, true, "OK") || common::enter_pressed(ui);
                if target == "page" && btn(ui, "No Color") {
                    return match run(app, Value::Null) {
                        Ok(_) => Out::Close,
                        Err(e) => {
                            st.error = e;
                            Out::Keep
                        }
                    };
                }
                if ok {
                    return match run(app, json!(hex(st.rgb))) {
                        Ok(_) => Out::Close,
                        Err(e) => {
                            st.error = e;
                            Out::Keep
                        }
                    };
                }
                Out::Keep
            })
        })
    })
}

// ---------------------------------------------------------------------------------------------------------
// Style forms

fn create_style(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let compose = d.u64("compose");
    run_form(
        ctx,
        app,
        d,
        Form::new(
            "Create New Style from Formatting",
            vec![
                f("name", "Name:", Kind::Text),
                f("n", "", Kind::Note("The style takes the formatting at the caret.".into())),
            ],
        )
        .init("name", json!(d.str("name"))),
        move |app, v| {
            if v.s("name").is_empty() {
                return Err("Type a name for the style.".into());
            }
            cmd(app, "format.style", json!({"compose": compose, "name": v.s("name"), "action": "create"}))
                .map(|_| None)
        },
    )
}

fn save_style_set(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let compose = d.u64("compose");
    run_form(
        ctx,
        app,
        d,
        Form::new(
            "Save as a New Style Set",
            vec![
                f("name", "Style set name:", Kind::Text),
                f(
                    "n",
                    "",
                    Kind::Note("Saves this message's style set and paragraph spacing; the name then appears under Change Styles › Style Set.".into()),
                ),
            ],
        )
        .ok("Save"),
        move |app, v| {
            if v.s("name").is_empty() {
                return Err("Type a name for the style set.".into());
            }
            cmd(app, "format.styleSet", json!({"compose": compose, "action": "save", "name": v.s("name")})).map(|_| None)
        },
    )
}

fn custom_spacing(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let compose = d.u64("compose");
    let p = d.get("paragraph").cloned().unwrap_or(Value::Null);
    let n = |k: &str, dflt: f64| p.get(k).and_then(Value::as_f64).unwrap_or(dflt);
    let line = if p.get("line").and_then(Value::as_str) == Some("multiple") { n("at", 1.0) } else { 1.0 };
    run_form(
        ctx,
        app,
        d,
        Form::new(
            "Custom Paragraph Spacing",
            vec![
                f("before", "Before (pt):", Kind::Num { min: 0.0, max: 1584.0 }),
                f("after", "After (pt):", Kind::Num { min: 0.0, max: 1584.0 }),
                f("line", "Line spacing (multiple):", Kind::Num { min: 0.25, max: 10.0 }),
                f("n", "", Kind::Note("Applies to every paragraph of the message.".into())),
            ],
        )
        .init("before", json!(n("before", 0.0)))
        .init("after", json!(n("after", 8.0)))
        .init("line", json!(line)),
        move |app, v| {
            let p = json!({"compose": compose, "paragraphSpacing": "custom", "before": v.n("before"), "after": v.n("after"), "line": v.n("line").max(0.25)});
            cmd(app, "format.styleSet", p).map(|_| None)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsl_round_trips() {
        for c in [[0u8, 0, 0], [255, 255, 255], [31, 95, 191], [200, 30, 40], [12, 200, 90]] {
            let (h, s, l) = rgb_to_hsl(c);
            let back = hsl_to_rgb(h, s, l);
            for (a, b) in c.iter().zip(back) {
                assert!(a.abs_diff(b) <= 1, "{c:?} -> {back:?}");
            }
        }
        assert_eq!(standard_palette().len(), 84);
    }

    #[test]
    fn font_values_send_only_what_changed() {
        let was = FontVals::from_style(&json!({"bold": true, "font_family": "Calibri", "size_pt": 11.0}));
        let mut now = was.clone();
        assert!(now.changes(&was).is_empty());
        now.bold = false;
        now.underline = 2;
        now.small_caps = true;
        now.spacing = 1.5;
        let ch = now.changes(&was);
        assert_eq!(ch.get("bold"), Some(&json!(false)));
        assert_eq!(ch.get("underline"), Some(&json!("double")));
        assert_eq!(ch.get("smallCaps"), Some(&json!(true)));
        assert_eq!(ch.get("spacing"), Some(&json!(1.5)));
        assert!(ch.get("family").is_none() && ch.get("pt").is_none());
    }

    #[test]
    fn paragraph_values_read_the_engine_payload() {
        let p = json!({"align": "center", "left": 36.0, "special": "hanging", "by": 18.0, "line": "exactly", "at": 14.0, "widowControl": false});
        let v = ParaVals::from_json(&p);
        assert_eq!((v.align, v.special, v.line), (1, 2, 4));
        assert!(!v.widow);
        let st = v.para_style();
        assert_eq!(st.align, Align::Center);
        assert!(st.first_line_px < 0.0);
        assert_eq!(st.line_px.map(|(_, r)| r), Some(LineRule::Exactly));
        let mut now = v.clone();
        now.line = 5;
        now.at = 3.0;
        assert_eq!(now.changes(&v).get("line"), Some(&json!("multiple")));
    }

    #[test]
    fn paragraph_values_send_only_what_changed() {
        // Regression: OK sent every field, so changing only the alignment of several selected paragraphs
        // gave them all the caret paragraph's indents and spacing.
        let was = ParaVals::from_json(&json!({"align": "left", "left": 36.0, "before": 6.0, "after": 12.0}));
        assert!(was.changes(&was).is_empty(), "an unchanged dialog sends nothing");
        let mut now = was.clone();
        now.align = 1;
        now.keep_next = true;
        let ch = now.changes(&was);
        assert_eq!(ch.get("align"), Some(&json!("center")));
        assert_eq!(ch.get("keepWithNext"), Some(&json!(true)));
        assert_eq!(ch.len(), 2, "{ch:?}");
        let mut hang = was.clone();
        hang.special = 2;
        hang.by = 18.0;
        let ch = hang.changes(&was);
        assert_eq!((ch.get("special"), ch.get("by")), (Some(&json!("hanging")), Some(&json!(18.0))));
        assert!(ch.get("left").is_none());
    }
}
