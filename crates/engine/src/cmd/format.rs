//! `format.*`: character / paragraph formatting, clipboard, find and selection in the compose body. All act
//! on the selection of `{"compose": id}` (default: the most recent compose). Character formatting with a
//! collapsed selection sets the typing style. Body edits keep their own editor undo (`compose.editUndo`).

use serde_json::{Value, json};

use super::compose::{
    compose_id, compose_mut, has_body_selection, has_compose, pos_json, rich_compose, spec,
};
use crate::cmd::util::{dialog, opt_bool, opt_str, req_str};
use crate::compose::{
    BodySelection, CaseKind, ClipEntry, ComposeState, DocPos, NamedStyle, PainterState, PasteMode, align_of,
};
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_geom::Color;
use mailcraft_model::rich::ListKind;
use mailcraft_model::{Doc, ParaStyle, TextStyle};

/// Font sizes stepped through by Grow / Shrink Font.
pub const FONT_SIZES: [f32; 16] =
    [8.0, 9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 26.0, 28.0, 36.0, 48.0, 72.0];
/// Largest font size accepted (points).
pub const MAX_FONT_PT: f32 = 1638.0;
/// Default Font Color button colour before one is picked.
const DEFAULT_FONT_COLOR: Color = Color::rgb(0xee, 0x00, 0x00);
/// Default highlight colour (the yellow on the button).
const DEFAULT_HIGHLIGHT: Color = Color::rgb(0xff, 0xff, 0x00);
/// Accent used by the built-in emphasis / reference styles.
const ACCENT: Color = Color::rgb(0x0f, 0x47, 0x61);
const SUBTLE: Color = Color::rgb(0x40, 0x40, 0x40);

fn pt_to_px(pt: f32) -> f32 {
    pt * 4.0 / 3.0
}

fn base_size(c: &ComposeState) -> f32 {
    c.body.base.size_pt.unwrap_or(mailcraft_fonts::BODY_SIZE_PT)
}

/// Runs a body edit on the target compose with an editor undo step.
fn edit(s: &mut Session, p: &Value, f: impl FnOnce(&mut ComposeState)) -> Result<(), CmdError> {
    let c = compose_mut(s, p)?;
    c.checkpoint();
    f(c);
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------
// Character formatting
// ---------------------------------------------------------------------------------------------------------

fn toggle(
    s: &mut Session,
    p: &Value,
    name: &str,
    is_on: fn(&TextStyle, bool) -> bool,
    set: fn(&mut TextStyle, bool),
) -> CmdResult {
    let c = compose_mut(s, p)?;
    let on = !c.selection_all(is_on);
    c.checkpoint();
    c.apply_text_style(|st| set(st, on));
    Ok(json!({name: on}))
}

fn bold(s: &mut Session, p: &Value) -> CmdResult {
    toggle(s, p, "bold", |st, heading| st.bold.unwrap_or(heading), |st, on| st.bold = Some(on))
}

fn italic(s: &mut Session, p: &Value) -> CmdResult {
    toggle(s, p, "italic", |st, _| st.italic == Some(true), |st, on| st.italic = Some(on))
}

fn underline(s: &mut Session, p: &Value) -> CmdResult {
    toggle(s, p, "underline", |st, _| st.underline == Some(true), |st, on| st.underline = Some(on))
}

fn strike(s: &mut Session, p: &Value) -> CmdResult {
    toggle(s, p, "strike", |st, _| st.strike == Some(true), |st, on| st.strike = Some(on))
}

fn sub(s: &mut Session, p: &Value) -> CmdResult {
    toggle(
        s,
        p,
        "subscript",
        |st, _| st.subscript == Some(true),
        |st, on| {
            st.subscript = Some(on);
            if on {
                st.superscript = Some(false);
            }
        },
    )
}

fn sup(s: &mut Session, p: &Value) -> CmdResult {
    toggle(
        s,
        p,
        "superscript",
        |st, _| st.superscript == Some(true),
        |st, on| {
            st.superscript = Some(on);
            if on {
                st.subscript = Some(false);
            }
        },
    )
}

fn font(s: &mut Session, p: &Value) -> CmdResult {
    let family = req_str(p, "family")?.trim().to_owned();
    if family.is_empty() || family.len() > 256 {
        return Err(CmdError::Params("`family` must be a font name".into()));
    }
    edit(s, p, |c| c.apply_text_style(|st| st.font_family = Some(family.clone())))?;
    Ok(json!({"family": family}))
}

fn font_size(s: &mut Session, p: &Value) -> CmdResult {
    let pt = p
        .get("pt")
        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())))
        .ok_or_else(|| CmdError::Params("`pt` (number) required".into()))?;
    #[allow(clippy::cast_possible_truncation)]
    let pt = pt as f32;
    if !pt.is_finite() || !(1.0..=MAX_FONT_PT).contains(&pt) {
        return Err(CmdError::Params(format!("font size must be between 1 and {MAX_FONT_PT}")));
    }
    let pt = (pt * 2.0).round() / 2.0;
    edit(s, p, |c| c.apply_text_style(|st| st.size_pt = Some(pt)))?;
    Ok(json!({"pt": pt}))
}

/// Next size up in the list (beyond 72 in steps of 10).
pub fn grow_size(cur: f32) -> f32 {
    FONT_SIZES
        .iter()
        .copied()
        .find(|s| *s > cur + 0.01)
        .unwrap_or_else(|| ((cur / 10.0).floor() * 10.0 + 10.0).min(MAX_FONT_PT))
}

/// Next size down in the list (beyond 72 in steps of 10; never below 1).
pub fn shrink_size(cur: f32) -> f32 {
    if cur > 72.0 + 0.01 {
        return ((cur / 10.0).ceil() * 10.0 - 10.0).max(72.0);
    }
    FONT_SIZES.iter().rev().copied().find(|s| *s < cur - 0.01).unwrap_or_else(|| (cur - 1.0).max(1.0))
}

fn grow_font(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    let base = base_size(c);
    c.checkpoint();
    c.apply_text_style(|st| st.size_pt = Some(grow_size(st.size_pt.unwrap_or(base))));
    Ok(json!({"pt": c.style_at_caret().size_pt}))
}

fn shrink_font(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    let base = base_size(c);
    c.checkpoint();
    c.apply_text_style(|st| st.size_pt = Some(shrink_size(st.size_pt.unwrap_or(base))));
    Ok(json!({"pt": c.style_at_caret().size_pt}))
}

fn parse_color(t: &str) -> Result<Color, CmdError> {
    mailcraft_html::parse_css_color(t).ok_or_else(|| CmdError::Params(format!("unknown colour `{t}`")))
}

fn color(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "custom") == Some(true) {
        return dialog(s, "colors", json!({"compose": compose_id(s, p)?, "target": "font"}));
    }
    let chosen: Option<Option<Color>> = match opt_str(p, "color") {
        Some(t) if t.eq_ignore_ascii_case("automatic") || t.eq_ignore_ascii_case("auto") => Some(None),
        Some(t) => Some(Some(parse_color(t)?)),
        None => None,
    };
    let c = compose_mut(s, p)?;
    let col = match chosen {
        Some(v) => v,
        None => Some(c.last_font_color.unwrap_or(DEFAULT_FONT_COLOR)),
    };
    if let Some(v) = col {
        c.last_font_color = Some(v);
    }
    c.checkpoint();
    c.apply_text_style(|st| st.color = col);
    Ok(json!({"color": col.map(|x| x.to_hex())}))
}

fn highlight(s: &mut Session, p: &Value) -> CmdResult {
    let hc = opt_bool(p, "highContrastOnly");
    let chosen: Option<Option<Color>> = match p.get("color") {
        Some(Value::Null) => Some(None),
        Some(v) => {
            let t = v
                .as_str()
                .ok_or_else(|| CmdError::Params("`color` must be a colour string or null".into()))?;
            if t.eq_ignore_ascii_case("none") { Some(None) } else { Some(Some(parse_color(t)?)) }
        }
        None => None,
    };
    let c = compose_mut(s, p)?;
    if let Some(v) = hc {
        c.editor.high_contrast_only = v;
        if chosen.is_none() {
            return Ok(json!({"highContrastOnly": v}));
        }
    }
    let col = match chosen {
        Some(v) => v,
        None => Some(c.last_highlight.unwrap_or(DEFAULT_HIGHLIGHT)),
    };
    if let Some(v) = col {
        c.last_highlight = Some(v);
    }
    c.checkpoint();
    c.apply_text_style(|st| st.highlight = col);
    Ok(json!({"highlight": col.map(|x| x.to_hex())}))
}

fn clear_formatting(s: &mut Session, p: &Value) -> CmdResult {
    edit(s, p, |c| {
        c.apply_text_style(|st| *st = TextStyle::default());
        c.set_heading(None);
        c.set_para_style(|ps| *ps = ParaStyle::default());
    })?;
    Ok(json!({"cleared": true}))
}

fn change_case(s: &mut Session, p: &Value) -> CmdResult {
    let kind = match req_str(p, "kind")? {
        "sentence" => CaseKind::Sentence,
        "lower" => CaseKind::Lower,
        "upper" => CaseKind::Upper,
        "title" => CaseKind::Title,
        "toggle" => CaseKind::Toggle,
        other => return Err(CmdError::Params(format!("unknown case `{other}`"))),
    };
    let c = compose_mut(s, p)?;
    c.checkpoint();
    c.change_case(kind);
    Ok(json!({"text": c.selected_text()}))
}

fn font_dialog(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let style = s.composes.get(&cid).map(ComposeState::style_at_caret).unwrap_or_default();
    dialog(s, "font", json!({"compose": cid, "style": style}))
}

fn format_painter(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    if c.painter.is_some() {
        c.painter = None;
        return Ok(json!({"armed": false}));
    }
    c.painter = Some(PainterState {
        style: c.style_at_caret(),
        para: c.para_at_caret(),
        sticky: opt_bool(p, "sticky") == Some(true),
    });
    Ok(json!({"armed": true}))
}

// ---------------------------------------------------------------------------------------------------------
// Paragraph formatting
// ---------------------------------------------------------------------------------------------------------

fn align(s: &mut Session, p: &Value) -> CmdResult {
    let kind = opt_str(p, "kind").unwrap_or("left");
    let a = align_of(kind).ok_or_else(|| CmdError::Params(format!("unknown alignment `{kind}`")))?;
    edit(s, p, |c| c.set_para_style(|ps| ps.align = a))?;
    Ok(json!({"align": kind}))
}

fn line_spacing(s: &mut Session, p: &Value) -> CmdResult {
    let m = p
        .get("multiple")
        .and_then(Value::as_f64)
        .ok_or_else(|| CmdError::Params("`multiple` (number) required".into()))?;
    if !m.is_finite() || !(0.25..=10.0).contains(&m) {
        return Err(CmdError::Params("line spacing must be between 0.25 and 10".into()));
    }
    #[allow(clippy::cast_possible_truncation)]
    let m = m as f32;
    edit(s, p, |c| c.set_para_style(|ps| ps.line_height = Some(m)))?;
    Ok(json!({"multiple": m}))
}

fn paragraph_spacing(s: &mut Session, p: &Value) -> CmdResult {
    let read = |k: &str| -> Result<Option<f32>, CmdError> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => {
                let n = v.as_f64().filter(|n| n.is_finite() && (0.0..=1584.0).contains(n));
                #[allow(clippy::cast_possible_truncation)]
                n.map(|n| Some(pt_to_px(n as f32)))
                    .ok_or_else(|| CmdError::Params(format!("`{k}` must be 0..1584 points")))
            }
        }
    };
    let before = read("before")?;
    let after = read("after")?;
    if before.is_none() && after.is_none() {
        return Err(CmdError::Params("`before` and/or `after` (points) required".into()));
    }
    edit(s, p, |c| {
        c.set_para_style(|ps| {
            if let Some(b) = before {
                ps.space_before_px = b;
            }
            if let Some(a) = after {
                ps.space_after_px = a;
            }
        });
    })?;
    Ok(json!({"before": before, "after": after}))
}

fn shading(s: &mut Session, p: &Value) -> CmdResult {
    let col = match p.get("color") {
        None => {
            return dialog(s, "bordersAndShading", json!({"compose": compose_id(s, p)?, "tab": "shading"}));
        }
        Some(Value::Null) => None,
        Some(v) => {
            let t = v
                .as_str()
                .ok_or_else(|| CmdError::Params("`color` must be a colour string or null".into()))?;
            if t.eq_ignore_ascii_case("none") { None } else { Some(parse_color(t)?) }
        }
    };
    let c = compose_mut(s, p)?;
    c.checkpoint();
    let target = if let Some(cell) = c.cell_at_caret_mut() {
        cell.background = col;
        "cell"
    } else {
        c.set_para_style(|ps| ps.background = col);
        "paragraph"
    };
    c.dirty = true;
    Ok(json!({"shading": col.map(|x| x.to_hex()), "target": target}))
}

fn borders(s: &mut Session, p: &Value) -> CmdResult {
    let kind = opt_str(p, "kind");
    if opt_bool(p, "dialog") == Some(true) || kind.is_none() {
        return dialog(s, "bordersAndShading", json!({"compose": compose_id(s, p)?, "tab": "borders"}));
    }
    let kind = kind.unwrap_or("none");
    if !matches!(
        kind,
        "bottom" | "top" | "left" | "right" | "none" | "all" | "outside" | "inside" | "insideH" | "insideV"
    ) {
        return Err(CmdError::Params(format!("unknown border `{kind}`")));
    }
    let c = compose_mut(s, p)?;
    c.checkpoint();
    if let Some(table) = c.table_at_caret() {
        let _ = table;
        let on = kind != "none";
        let changed = c.set_table_border(on);
        return Ok(json!({"target": "table", "border": on, "changed": changed}));
    }
    let changed = match kind {
        "bottom" => c.set_para_rule(false, true),
        "top" => c.set_para_rule(true, true),
        "none" => {
            let a = c.set_para_rule(true, false);
            let b = c.set_para_rule(false, false);
            a || b
        }
        "outside" | "all" => {
            let a = c.set_para_rule(true, true);
            let b = c.set_para_rule(false, true);
            a || b
        }
        _ => return Err(CmdError::Failed(format!("`{kind}` borders apply to tables only"))),
    };
    Ok(json!({"target": "paragraph", "border": kind, "changed": changed}))
}

fn list_cmd(s: &mut Session, p: &Value, kind: ListKind, define_dialog: &str) -> CmdResult {
    let style = opt_str(p, "style");
    match style {
        Some("none") => {
            edit(s, p, |c| c.toggle_list(None))?;
            return Ok(json!({"list": null}));
        }
        Some("define") => return dialog(s, define_dialog, json!({"compose": compose_id(s, p)?})),
        _ => {}
    }
    let c = compose_mut(s, p)?;
    c.checkpoint();
    if style.is_some() {
        // Applying a gallery style keeps an existing list of the same kind.
        let same = matches!(
            (c.list_at_caret().map(|(_, k)| k), &kind),
            (Some(ListKind::Bullet), ListKind::Bullet)
                | (Some(ListKind::Numbered { .. }), ListKind::Numbered { .. })
        );
        if !same {
            c.toggle_list(Some(kind));
        }
    } else {
        c.toggle_list(Some(kind));
    }
    let now = c.list_at_caret().map(|(_, k)| match k {
        ListKind::Bullet => "bullet",
        ListKind::Numbered { .. } => "numbered",
    });
    Ok(json!({"list": now}))
}

fn bullets(s: &mut Session, p: &Value) -> CmdResult {
    list_cmd(s, p, ListKind::Bullet, "defineNewBullet")
}

fn numbering(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(v) = p.get("start") {
        let n = v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| CmdError::Params("`start` must be a number".into()))?;
        edit(s, p, |c| c.set_list_start(n))?;
        return Ok(json!({"list": "numbered", "start": n}));
    }
    if opt_str(p, "style") == Some("setValue") {
        return dialog(s, "setNumberingValue", json!({"compose": compose_id(s, p)?}));
    }
    list_cmd(s, p, ListKind::Numbered { start: 1 }, "defineNewNumberFormat")
}

fn multilevel(s: &mut Session, p: &Value) -> CmdResult {
    let style = opt_str(p, "style");
    if let Some(f) = opt_str(p, "filter") {
        if !matches!(f, "all" | "current" | "library") {
            return Err(CmdError::Params(format!("unknown gallery filter `{f}`")));
        }
        if style.is_none() {
            return Ok(json!({"filter": f}));
        }
    }
    match style {
        None => edit(s, p, |c| c.toggle_list(Some(ListKind::Numbered { start: 1 })))
            .map(|()| json!({"list": "numbered"})),
        Some("none" | "None") => {
            list_cmd(s, &json!({"style": "none", "compose": p.get("compose")}), ListKind::Bullet, "")
        }
        Some("define") => dialog(s, "defineNewMultilevelList", json!({"compose": compose_id(s, p)?})),
        Some("defineStyle") => dialog(s, "defineNewListStyle", json!({"compose": compose_id(s, p)?})),
        Some(name) => {
            let kind = if name.to_lowercase().contains("bullet") {
                ListKind::Bullet
            } else {
                ListKind::Numbered { start: 1 }
            };
            list_cmd(s, p, kind, "defineNewMultilevelList")
        }
    }
}

fn indent(s: &mut Session, p: &Value) -> CmdResult {
    let level = p.get("level").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok());
    let c = compose_mut(s, p)?;
    c.checkpoint();
    match level {
        Some(l) => c.set_list_level(l),
        None => c.indent(),
    }
    Ok(json!({"indent": c.para_at_caret().indent_px}))
}

fn outdent(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.checkpoint();
    c.outdent();
    Ok(json!({"indent": c.para_at_caret().indent_px}))
}

fn sort(s: &mut Session, p: &Value) -> CmdResult {
    let desc = opt_bool(p, "descending").unwrap_or(false);
    edit(s, p, |c| c.sort_selection(desc))?;
    Ok(json!({"sorted": true, "descending": desc}))
}

fn paragraph_dialog(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let para = s.composes.get(&cid).map(ComposeState::para_at_caret).unwrap_or_default();
    dialog(s, "paragraph", json!({"compose": cid, "paragraph": para}))
}

// ---------------------------------------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------------------------------------

/// Applies a built-in style by name; false when the name is unknown.
fn apply_builtin_style(c: &mut ComposeState, name: &str) -> bool {
    let char_style = |c: &mut ComposeState, f: &dyn Fn(&mut TextStyle)| c.apply_text_style(|st| f(st));
    match name {
        "normal" => {
            c.set_heading(None);
            c.set_para_style(|ps| *ps = ParaStyle::default());
        }
        "no spacing" => {
            c.set_heading(None);
            c.set_para_style(|ps| {
                ps.space_before_px = 0.0;
                ps.space_after_px = 0.0;
                ps.line_height = Some(1.0);
            });
        }
        "title" => {
            c.set_heading(Some(1));
            c.with_whole_paragraphs(|c| c.apply_text_style(|st| st.size_pt = Some(28.0)));
        }
        "subtitle" => {
            c.set_heading(None);
            c.with_whole_paragraphs(|c| {
                c.apply_text_style(|st| {
                    st.size_pt = Some(14.0);
                    st.color = Some(Color::rgb(0x59, 0x59, 0x59));
                });
            });
        }
        "quote" => {
            c.set_heading(None);
            c.set_para_style(|ps| ps.align = mailcraft_model::rich::Align::Center);
            c.with_whole_paragraphs(|c| {
                c.apply_text_style(|st| {
                    st.italic = Some(true);
                    st.color = Some(SUBTLE);
                });
            });
        }
        "intense quote" => {
            c.set_heading(None);
            c.set_para_style(|ps| {
                ps.align = mailcraft_model::rich::Align::Center;
                ps.indent_px = 48.0;
            });
            c.with_whole_paragraphs(|c| {
                c.apply_text_style(|st| {
                    st.italic = Some(true);
                    st.color = Some(ACCENT);
                });
            });
        }
        "list paragraph" => {
            c.set_heading(None);
            c.set_para_style(|ps| ps.indent_px = 48.0);
        }
        "subtle emphasis" => char_style(c, &|st| {
            st.italic = Some(true);
            st.color = Some(SUBTLE);
        }),
        "emphasis" => char_style(c, &|st| st.italic = Some(true)),
        "intense emphasis" => char_style(c, &|st| {
            st.italic = Some(true);
            st.color = Some(ACCENT);
        }),
        "strong" => char_style(c, &|st| st.bold = Some(true)),
        "subtle reference" => char_style(c, &|st| st.color = Some(Color::rgb(0x5a, 0x5a, 0x5a))),
        "intense reference" => char_style(c, &|st| {
            st.bold = Some(true);
            st.color = Some(ACCENT);
        }),
        "book title" => char_style(c, &|st| {
            st.bold = Some(true);
            st.italic = Some(true);
        }),
        n => {
            let Some(level) = n
                .strip_prefix("heading ")
                .and_then(|l| l.trim().parse::<u8>().ok())
                .filter(|l| (1..=9).contains(l))
            else {
                return false;
            };
            c.set_heading(Some(level.min(6)));
        }
    }
    true
}

fn style(s: &mut Session, p: &Value) -> CmdResult {
    let raw = req_str(p, "name")?;
    let name = raw.trim_start_matches('¶').trim().to_owned();
    if name.is_empty() {
        return Err(CmdError::Params("`name` must name a style".into()));
    }
    let action = opt_str(p, "action").unwrap_or("apply");
    let c = compose_mut(s, p)?;
    match action {
        "create" => {
            let st = NamedStyle { name: name.clone(), style: c.style_at_caret(), para: c.para_at_caret() };
            c.custom_styles.retain(|x| !x.name.eq_ignore_ascii_case(&name));
            c.custom_styles.push(st);
            Ok(json!({"created": name}))
        }
        "apply" => {
            c.checkpoint();
            if let Some(custom) = c.custom_styles.iter().find(|x| x.name.eq_ignore_ascii_case(&name)).cloned()
            {
                c.set_para_style(|ps| *ps = custom.para.clone());
                c.with_whole_paragraphs(|c| c.apply_text_style(|st| *st = custom.style.clone()));
            } else if !apply_builtin_style(c, &name.to_lowercase()) {
                c.undo_edit();
                c.edit_redo.clear();
                return Err(CmdError::Params(format!("unknown style `{name}`")));
            }
            c.dirty = true;
            Ok(json!({"style": name}))
        }
        other => Err(CmdError::Params(format!("unknown style action `{other}`"))),
    }
}

/// Paragraph spacing presets for Change Styles: (space after in pt, line height multiple).
fn spacing_preset(name: &str) -> Option<(f32, f32)> {
    match name {
        "default" => Some((8.0, 1.08)),
        "noParagraphSpace" => Some((0.0, 1.0)),
        "compact" => Some((4.0, 1.0)),
        "tight" => Some((6.0, 1.0)),
        "open" => Some((10.0, 1.15)),
        "relaxed" => Some((6.0, 1.15)),
        "double" => Some((8.0, 2.0)),
        _ => None,
    }
}

fn style_set(s: &mut Session, p: &Value) -> CmdResult {
    let action = opt_str(p, "action").unwrap_or("apply");
    match action {
        "save" => return dialog(s, "saveStyleSet", json!({"compose": compose_id(s, p)?})),
        "apply" | "reset" | "setDefault" => {}
        other => return Err(CmdError::Params(format!("unknown action `{other}`"))),
    }
    let name = opt_str(p, "name").map(str::to_owned);
    let spacing = opt_str(p, "paragraphSpacing").map(str::to_owned);
    if let Some(sp) = &spacing
        && spacing_preset(sp).is_none()
    {
        return Err(CmdError::Params(format!("unknown paragraph spacing `{sp}`")));
    }
    let c = compose_mut(s, p)?;
    c.checkpoint();
    match action {
        "reset" => {
            c.style_set = None;
            c.paragraph_spacing_preset = None;
        }
        "setDefault" => {
            return Ok(json!({"default": c.style_set, "paragraphSpacing": c.paragraph_spacing_preset}));
        }
        _ => {
            if name.is_none() && spacing.is_none() {
                return Err(CmdError::Params("`name` or `paragraphSpacing` required".into()));
            }
            if name.is_some() {
                c.style_set = name;
            }
            if let Some(sp) = spacing {
                if let Some((after, line)) = spacing_preset(&sp) {
                    c.with_whole_body(|c| {
                        c.set_para_style(|ps| {
                            ps.space_after_px = pt_to_px(after);
                            ps.line_height = Some(line);
                        });
                    });
                }
                c.paragraph_spacing_preset = Some(sp);
            }
        }
    }
    c.dirty = true;
    Ok(json!({"styleSet": c.style_set, "paragraphSpacing": c.paragraph_spacing_preset}))
}

fn styles_pane(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "apply") == Some(true) {
        return dialog(s, "applyStyles", json!({"compose": compose_id(s, p)?}));
    }
    let c = compose_mut(s, p)?;
    c.editor.styles_pane = !c.editor.styles_pane;
    Ok(json!({"stylesPane": c.editor.styles_pane}))
}

// ---------------------------------------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------------------------------------

fn copy_selection(s: &mut Session, p: &Value) -> Result<ClipEntry, CmdError> {
    let c = compose_mut(s, p)?;
    if c.selection.is_collapsed() {
        return Err(CmdError::Disabled("select some text first".into()));
    }
    let entry = ClipEntry { text: c.selected_text(), doc: c.fragment() };
    for other in s.composes.values_mut() {
        other.push_clipboard(entry.clone());
    }
    s.clipboard_text = Some(entry.text.clone());
    Ok(entry)
}

fn copy(s: &mut Session, p: &Value) -> CmdResult {
    let e = copy_selection(s, p)?;
    Ok(json!({"text": e.text, "html": mailcraft_html::doc_to_html(&e.doc)}))
}

fn cut(s: &mut Session, p: &Value) -> CmdResult {
    let e = copy_selection(s, p)?;
    let c = compose_mut(s, p)?;
    c.checkpoint();
    c.delete_selection();
    Ok(json!({"text": e.text, "html": mailcraft_html::doc_to_html(&e.doc)}))
}

/// What Paste inserts: `params.html` / `params.text`, else the newest in-app entry (when it is still what
/// the session clipboard holds), else the session's clipboard text.
fn paste_source(s: &Session, p: &Value, cid: u64) -> Result<Doc, CmdError> {
    if let Some(h) = opt_str(p, "html") {
        return Ok(mailcraft_html::html_to_doc(h));
    }
    if let Some(t) = opt_str(p, "text") {
        return Ok(Doc::from_plain_text(t));
    }
    let entry = s.composes.get(&cid).and_then(|c| c.clipboard.last());
    match (entry, &s.clipboard_text) {
        (Some(e), Some(t)) if e.text == *t => Ok(e.doc.clone()),
        (Some(e), None) => Ok(e.doc.clone()),
        (_, Some(t)) => Ok(Doc::from_plain_text(t)),
        (None, None) => Err(CmdError::Failed("the clipboard is empty".into())),
    }
}

fn paste_mode(m: Option<&str>) -> Result<PasteMode, CmdError> {
    match m {
        None | Some("keepSource") => Ok(PasteMode::KeepSource),
        Some("merge") => Ok(PasteMode::Merge),
        Some("textOnly") => Ok(PasteMode::TextOnly),
        Some(other) => Err(CmdError::Params(format!("unknown paste mode `{other}`"))),
    }
}

fn do_paste(s: &mut Session, p: &Value, mode: PasteMode) -> CmdResult {
    let cid = compose_id(s, p)?;
    let doc = paste_source(s, p, cid)?;
    let c = compose_mut(s, p)?;
    let mode = if c.format == mailcraft_model::BodyFormat::PlainText { PasteMode::TextOnly } else { mode };
    c.checkpoint();
    c.insert_fragment(&doc, mode);
    Ok(json!({"pasted": true, "caret": pos_json(&c.selection.focus)}))
}

fn paste(s: &mut Session, p: &Value) -> CmdResult {
    let mode = paste_mode(opt_str(p, "mode"))?;
    do_paste(s, p, mode)
}

fn paste_text(s: &mut Session, p: &Value) -> CmdResult {
    do_paste(s, p, PasteMode::TextOnly)
}

fn paste_special(s: &mut Session, p: &Value) -> CmdResult {
    match opt_str(p, "as") {
        None => dialog(s, "pasteSpecial", json!({"compose": compose_id(s, p)?})),
        Some("html" | "rtf") => do_paste(s, p, PasteMode::KeepSource),
        Some("text") => do_paste(s, p, PasteMode::TextOnly),
        Some("image") => Err(CmdError::Failed("the clipboard holds no picture".into())),
        Some(other) => Err(CmdError::Params(format!("unknown paste format `{other}`"))),
    }
}

fn clipboard_pane(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.editor.clipboard_pane = !c.editor.clipboard_pane;
    let entries: Vec<&str> = c.clipboard.iter().rev().map(|e| e.text.as_str()).collect();
    Ok(json!({"clipboardPane": c.editor.clipboard_pane, "entries": entries}))
}

// ---------------------------------------------------------------------------------------------------------
// Editing: find, replace, go to, select
// ---------------------------------------------------------------------------------------------------------

fn find(s: &mut Session, p: &Value) -> CmdResult {
    let Some(text) = opt_str(p, "text").map(str::to_owned) else {
        return dialog(s, "find", json!({"compose": compose_id(s, p)?}));
    };
    let case = opt_bool(p, "matchCase").unwrap_or(false);
    let c = compose_mut(s, p)?;
    c.find_text = text.clone();
    let found = c.find_next(&text, case);
    Ok(
        json!({"found": found, "anchor": pos_json(&c.selection.anchor), "focus": pos_json(&c.selection.focus)}),
    )
}

fn replace(s: &mut Session, p: &Value) -> CmdResult {
    let Some(needle) = opt_str(p, "find").map(str::to_owned) else {
        return dialog(s, "replace", json!({"compose": compose_id(s, p)?}));
    };
    let with = opt_str(p, "replace").unwrap_or("").to_owned();
    let case = opt_bool(p, "matchCase").unwrap_or(false);
    let c = compose_mut(s, p)?;
    c.find_text = needle.clone();
    c.checkpoint();
    if opt_bool(p, "all") == Some(true) {
        let n = c.replace_all(&needle, &with, case);
        return Ok(json!({"replaced": n}));
    }
    let (replaced, found) = c.replace_next(&needle, &with, case);
    Ok(json!({"replaced": usize::from(replaced), "found": found}))
}

fn go_to(s: &mut Session, p: &Value) -> CmdResult {
    let Some(target) = opt_str(p, "target").map(str::to_owned) else {
        return dialog(s, "goTo", json!({"compose": compose_id(s, p)?}));
    };
    let c = compose_mut(s, p)?;
    let (kind, arg) = target.split_once(':').map_or((target.as_str(), ""), |(k, a)| (k, a.trim()));
    let nth = |a: &str| a.parse::<usize>().ok().and_then(|n| n.checked_sub(1));
    let pos: Option<DocPos> = match kind {
        "start" | "page" if kind == "start" || nth(arg) == Some(0) => Some(c.body_start()),
        "end" => Some(c.body_end()),
        "paragraph" | "line" => {
            nth(arg).and_then(|n| c.leaf_paths().get(n).cloned()).map(|path| DocPos::new(path, 0))
        }
        "table" => nth(arg).and_then(|n| c.table_starts().get(n).cloned()),
        "bookmark" => c.bookmarks.iter().find(|b| b.name == arg).map(|b| b.pos.clone()),
        name => c.bookmarks.iter().find(|b| b.name == name).map(|b| b.pos.clone()),
    };
    let Some(pos) = pos else {
        return Err(CmdError::Params(format!("cannot go to `{target}`")));
    };
    c.set_caret(pos);
    Ok(json!({"caret": pos_json(&c.selection.focus)}))
}

fn select_all(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.select_all();
    Ok(json!({"anchor": pos_json(&c.selection.anchor), "focus": pos_json(&c.selection.focus)}))
}

fn select_similar(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    let n = c.select_similar();
    let ranges: Vec<Value> = std::iter::once(&c.selection)
        .chain(c.extra_ranges.iter())
        .take(n)
        .map(|r: &BodySelection| json!({"anchor": pos_json(&r.anchor), "focus": pos_json(&r.focus)}))
        .collect();
    Ok(json!({"ranges": ranges}))
}

fn select_objects(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.editor.select_objects = !c.editor.select_objects;
    Ok(json!({"selectObjects": c.editor.select_objects}))
}

fn selection_pane(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.editor.selection_pane = !c.editor.selection_pane;
    Ok(json!({"selectionPane": c.editor.selection_pane}))
}

fn show_gridlines(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.editor.show_gridlines = !c.editor.show_gridlines;
    Ok(json!({"showGridlines": c.editor.show_gridlines}))
}

fn show_marks(s: &mut Session, p: &Value) -> CmdResult {
    let c = compose_mut(s, p)?;
    c.editor.show_marks = !c.editor.show_marks;
    Ok(json!({"showMarks": c.editor.show_marks}))
}

fn zoom(s: &mut Session, p: &Value) -> CmdResult {
    let Some(v) = p.get("pct") else {
        return dialog(s, "zoom", json!({"compose": compose_id(s, p)?}));
    };
    let pct = v
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .filter(|n| (10..=500).contains(n))
        .ok_or_else(|| CmdError::Params("`pct` must be 10..500".into()))?;
    let c = compose_mut(s, p)?;
    c.zoom = (pct != 100).then_some(pct);
    Ok(json!({"zoom": pct}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "format.align",
            "Align",
            "Message › Basic Text; Format Text › Paragraph",
            "Mod+L",
            r#"{"kind":"left"|"center"|"right"|"justify"}  (Mod+L left, Mod+E center, Mod+R right, Mod+J justify in the body)"#,
            false,
            rich_compose,
            align,
        ),
        spec(
            "format.bold",
            "Bold",
            "Message › Basic Text; Format Text › Font",
            "Mod+B",
            "",
            false,
            rich_compose,
            bold,
        ),
        spec(
            "format.borders",
            "Borders",
            "Format Text › Paragraph",
            "",
            r#"{"kind"?:"bottom"|"top"|"left"|"right"|"none"|"all"|"outside"|"inside"|"insideH"|"insideV","dialog"?:bool}"#,
            false,
            rich_compose,
            borders,
        ),
        spec(
            "format.bullets",
            "Bullets",
            "Message › Basic Text; Format Text › Paragraph",
            "",
            r#"{"style"?:string}  ("none" removes, "define" opens the dialog)"#,
            false,
            rich_compose,
            bullets,
        ),
        spec(
            "format.changeCase",
            "Change Case",
            "Format Text › Font",
            "",
            r#"{"kind":"sentence"|"lower"|"upper"|"title"|"toggle"}"#,
            false,
            has_compose,
            change_case,
        ),
        spec(
            "format.clearFormatting",
            "Clear Formatting",
            "Message › Basic Text; Format Text › Font; Format Text › Styles",
            "",
            "",
            false,
            rich_compose,
            clear_formatting,
        ),
        spec(
            "format.clipboardPane",
            "Clipboard History",
            "Message › Clipboard; Format Text › Clipboard",
            "",
            "",
            false,
            has_compose,
            clipboard_pane,
        ),
        spec(
            "format.color",
            "Font Color",
            "Message › Basic Text; Format Text › Font",
            "",
            r#"{"color"?:string,"custom"?:bool}  ("automatic" clears)"#,
            false,
            rich_compose,
            color,
        ),
        spec(
            "format.copy",
            "Copy",
            "Message › Clipboard; Format Text › Clipboard",
            "Mod+C",
            "",
            false,
            has_body_selection,
            copy,
        ),
        spec(
            "format.cut",
            "Cut",
            "Message › Clipboard; Format Text › Clipboard",
            "Mod+X",
            "",
            false,
            has_body_selection,
            cut,
        ),
        spec(
            "format.find",
            "Find",
            "Format Text › Editing",
            "",
            r#"{"text"?:string,"matchCase"?:bool}"#,
            false,
            has_compose,
            find,
        ),
        spec(
            "format.font",
            "Font",
            "Message › Basic Text; Format Text › Font",
            "",
            r#"{"family":string}"#,
            false,
            rich_compose,
            font,
        ),
        spec(
            "format.fontDialog",
            "Font Dialog",
            "Message › Basic Text; Format Text › Font",
            "",
            "",
            false,
            rich_compose,
            font_dialog,
        ),
        spec(
            "format.fontSize",
            "Font Size",
            "Message › Basic Text; Format Text › Font",
            "",
            r#"{"pt":number}"#,
            false,
            rich_compose,
            font_size,
        ),
        spec(
            "format.formatPainter",
            "Format Painter",
            "Message › Clipboard; Format Text › Clipboard",
            "",
            r#"{"sticky"?:bool}"#,
            false,
            rich_compose,
            format_painter,
        ),
        spec(
            "format.goTo",
            "Go To",
            "Format Text › Editing › Find",
            "Mod+G",
            r#"{"target"?:string}  ("start", "end", "page:1", "paragraph:n", "table:n", "bookmark:name")"#,
            false,
            has_compose,
            go_to,
        ),
        spec(
            "format.growFont",
            "Grow Font",
            "Message › Basic Text; Format Text › Font",
            "Mod+Shift+>",
            "",
            false,
            rich_compose,
            grow_font,
        ),
        spec(
            "format.highlight",
            "Text Highlight Color",
            "Message › Basic Text; Format Text › Font",
            "",
            r#"{"color"?:string|null,"highContrastOnly"?:bool}"#,
            false,
            rich_compose,
            highlight,
        ),
        spec(
            "format.indent",
            "Increase Indent",
            "Message › Basic Text; Format Text › Paragraph",
            "",
            r#"{"level"?:number}"#,
            false,
            rich_compose,
            indent,
        ),
        spec(
            "format.italic",
            "Italic",
            "Message › Basic Text; Format Text › Font",
            "Mod+I",
            "",
            false,
            rich_compose,
            italic,
        ),
        spec(
            "format.lineSpacing",
            "Line Spacing",
            "Format Text › Paragraph",
            "",
            r#"{"multiple":number}"#,
            false,
            rich_compose,
            line_spacing,
        ),
        spec(
            "format.multilevel",
            "Multilevel List",
            "Format Text › Paragraph",
            "",
            r#"{"style"?:string,"filter"?:"all"|"current"|"library"}"#,
            false,
            rich_compose,
            multilevel,
        ),
        spec(
            "format.numbering",
            "Numbering",
            "Message › Basic Text; Format Text › Paragraph",
            "",
            r#"{"style"?:string,"start"?:number}"#,
            false,
            rich_compose,
            numbering,
        ),
        spec(
            "format.outdent",
            "Decrease Indent",
            "Message › Basic Text; Format Text › Paragraph",
            "",
            "",
            false,
            rich_compose,
            outdent,
        ),
        spec(
            "format.paragraphDialog",
            "Paragraph Dialog",
            "Format Text › Paragraph",
            "",
            "",
            false,
            rich_compose,
            paragraph_dialog,
        ),
        spec(
            "format.paragraphSpacing",
            "Paragraph Spacing",
            "Format Text › Paragraph › Line and Paragraph Spacing",
            "",
            r#"{"before"?:number,"after"?:number}  (points)"#,
            false,
            rich_compose,
            paragraph_spacing,
        ),
        spec(
            "format.paste",
            "Paste",
            "Message › Clipboard; Format Text › Clipboard",
            "Mod+V",
            r#"{"mode"?:"keepSource"|"merge"|"textOnly","text"?:string,"html"?:string}"#,
            false,
            has_compose,
            paste,
        ),
        spec(
            "format.pasteText",
            "Paste Text",
            "Message › Clipboard › Paste",
            "",
            r#"{"text"?:string}"#,
            false,
            has_compose,
            paste_text,
        ),
        spec(
            "format.pasteSpecial",
            "Paste Special",
            "Message › Clipboard › Paste; Format Text › Clipboard › Paste",
            "Mod+Alt+V",
            r#"{"as"?:"html"|"rtf"|"text"|"image"}"#,
            false,
            has_compose,
            paste_special,
        ),
        spec(
            "format.replace",
            "Replace",
            "Format Text › Editing",
            "Mod+H",
            r#"{"find"?:string,"replace"?:string,"all"?:bool,"matchCase"?:bool}"#,
            false,
            has_compose,
            replace,
        ),
        spec(
            "format.selectAll",
            "Select All",
            "Format Text › Editing › Select",
            "Mod+A",
            "",
            false,
            has_compose,
            select_all,
        ),
        spec(
            "format.selectObjects",
            "Select Objects",
            "Format Text › Editing › Select",
            "",
            "",
            false,
            has_compose,
            select_objects,
        ),
        spec(
            "format.selectSimilar",
            "Select Similar Formatting",
            "Format Text › Editing › Select",
            "",
            "",
            false,
            has_compose,
            select_similar,
        ),
        spec(
            "format.selectionPane",
            "Selection Pane",
            "Format Text › Editing › Select",
            "",
            "",
            false,
            has_compose,
            selection_pane,
        ),
        spec(
            "format.shading",
            "Shading",
            "Format Text › Paragraph",
            "",
            r#"{"color"?:string|null}"#,
            false,
            rich_compose,
            shading,
        ),
        spec(
            "format.showGridlines",
            "View Gridlines",
            "Format Text › Paragraph › Borders",
            "",
            "",
            false,
            has_compose,
            show_gridlines,
        ),
        spec(
            "format.showMarks",
            "Show All",
            "Format Text › Paragraph",
            "Mod+Shift+8",
            "",
            false,
            has_compose,
            show_marks,
        ),
        spec(
            "format.shrinkFont",
            "Shrink Font",
            "Message › Basic Text; Format Text › Font",
            "Mod+Shift+<",
            "",
            false,
            rich_compose,
            shrink_font,
        ),
        spec(
            "format.sort",
            "Sort",
            "Format Text › Paragraph",
            "",
            r#"{"descending"?:bool}"#,
            false,
            has_compose,
            sort,
        ),
        spec("format.strike", "Strikethrough", "Format Text › Font", "", "", false, rich_compose, strike),
        spec(
            "format.style",
            "Styles",
            "Format Text › Styles",
            "",
            r#"{"name":string,"action"?:"apply"|"create"}"#,
            false,
            rich_compose,
            style,
        ),
        spec(
            "format.styleSet",
            "Change Styles",
            "Format Text › Styles",
            "",
            r#"{"name"?:string,"paragraphSpacing"?:"default"|"noParagraphSpace"|"compact"|"tight"|"open"|"relaxed"|"double","action"?:"apply"|"reset"|"save"|"setDefault"}"#,
            false,
            rich_compose,
            style_set,
        ),
        spec(
            "format.stylesPane",
            "Styles Pane",
            "Format Text › Styles",
            "Mod+Alt+Shift+S",
            r#"{"apply"?:bool}"#,
            false,
            rich_compose,
            styles_pane,
        ),
        spec("format.sub", "Subscript", "Format Text › Font", "Mod+=", "", false, rich_compose, sub),
        spec("format.sup", "Superscript", "Format Text › Font", "Mod+Shift+=", "", false, rich_compose, sup),
        spec(
            "format.underline",
            "Underline",
            "Message › Basic Text; Format Text › Font",
            "Mod+U",
            "",
            false,
            rich_compose,
            underline,
        ),
        spec("format.zoom", "Zoom", "Format Text › Zoom", "", r#"{"pct"?:number}"#, false, has_compose, zoom),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::compose::tests::{open, session};
    use super::*;
    use mailcraft_model::{Block, Inline};

    fn with_text(text: &str) -> (Session, u64) {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": text})).unwrap();
        (s, id)
    }

    fn sel(s: &mut Session, a: (&[usize], usize), b: (&[usize], usize)) {
        s.run(
            "compose.select",
            &json!({"anchor": {"path": a.0, "offset": a.1}, "focus": {"path": b.0, "offset": b.1}}),
        )
        .unwrap();
    }

    fn first_style(s: &Session, id: u64) -> TextStyle {
        match &s.composes[&id].body.blocks[0] {
            Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => match &inlines[0] {
                Inline::Text { style, .. } => style.clone(),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn char_toggles() {
        let (mut s, id) = with_text("hello world");
        sel(&mut s, (&[0], 0), (&[0], 5));
        assert_eq!(s.run("format.bold", &json!({})).unwrap()["bold"], true);
        assert_eq!(first_style(&s, id).bold, Some(true));
        assert_eq!(s.run("format.bold", &json!({})).unwrap()["bold"], false);
        s.run("format.italic", &json!({})).unwrap();
        s.run("format.underline", &json!({})).unwrap();
        s.run("format.strike", &json!({})).unwrap();
        s.run("format.sup", &json!({})).unwrap();
        s.run("format.sub", &json!({})).unwrap();
        let st = first_style(&s, id);
        assert_eq!(
            (st.italic, st.underline, st.strike, st.subscript, st.superscript),
            (Some(true), Some(true), Some(true), Some(true), Some(false))
        );
    }

    #[test]
    fn collapsed_toggle_sets_typing_style() {
        let (mut s, id) = with_text("a");
        s.run("format.bold", &json!({})).unwrap();
        s.run("compose.insertText", &json!({"text": "b"})).unwrap();
        let c = &s.composes[&id];
        match &c.body.blocks[0] {
            Block::Paragraph { inlines, .. } => assert!(
                matches!(&inlines[1], Inline::Text { style, text } if text == "b" && style.bold == Some(true))
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn font_size_color_highlight() {
        let (mut s, id) = with_text("abc");
        s.run("format.selectAll", &json!({})).unwrap();
        s.run("format.font", &json!({"family": "Carlito"})).unwrap();
        s.run("format.fontSize", &json!({"pt": 14})).unwrap();
        s.run("format.growFont", &json!({})).unwrap();
        assert_eq!(first_style(&s, id).size_pt, Some(16.0));
        s.run("format.shrinkFont", &json!({})).unwrap();
        s.run("format.shrinkFont", &json!({})).unwrap();
        assert_eq!(first_style(&s, id).size_pt, Some(12.0));
        assert!(s.run("format.fontSize", &json!({"pt": 0})).is_err());
        s.run("format.color", &json!({"color": "blue"})).unwrap();
        assert_eq!(first_style(&s, id).color, Some(Color::rgb(0, 0, 255)));
        s.run("format.color", &json!({"color": "automatic"})).unwrap();
        assert_eq!(first_style(&s, id).color, None);
        s.run("format.color", &json!({})).unwrap();
        assert_eq!(first_style(&s, id).color, Some(Color::rgb(0, 0, 255)), "last colour reused");
        s.run("format.highlight", &json!({})).unwrap();
        assert_eq!(first_style(&s, id).highlight, Some(DEFAULT_HIGHLIGHT));
        s.run("format.highlight", &json!({"color": null})).unwrap();
        assert_eq!(first_style(&s, id).highlight, None);
        assert_eq!(first_style(&s, id).font_family.as_deref(), Some("Carlito"));
        s.run("format.clearFormatting", &json!({})).unwrap();
        assert_eq!(first_style(&s, id), TextStyle::default());
        assert_eq!(s.run("format.color", &json!({"custom": true})).unwrap()["dialog"], "colors");
        assert_eq!(s.run("format.fontDialog", &json!({})).unwrap()["dialog"], "font");
    }

    #[test]
    fn size_steps() {
        assert_eq!(grow_size(11.0), 12.0);
        assert_eq!(grow_size(13.0), 14.0);
        assert_eq!(grow_size(72.0), 80.0);
        assert_eq!(shrink_size(8.0), 7.0);
        assert_eq!(shrink_size(80.0), 72.0);
        assert_eq!(shrink_size(1.0), 1.0);
        assert_eq!(grow_size(MAX_FONT_PT), MAX_FONT_PT);
    }

    #[test]
    fn paragraph_commands() {
        let (mut s, id) = with_text("one\ntwo\nthree");
        s.run("format.selectAll", &json!({})).unwrap();
        s.run("format.align", &json!({"kind": "center"})).unwrap();
        s.run("format.lineSpacing", &json!({"multiple": 1.5})).unwrap();
        s.run("format.paragraphSpacing", &json!({"before": 12, "after": 6})).unwrap();
        s.run("format.shading", &json!({"color": "#eeeeee"})).unwrap();
        match &s.composes[&id].body.blocks[1] {
            Block::Paragraph { style, .. } => {
                assert_eq!(style.align, mailcraft_model::rich::Align::Center);
                assert_eq!(style.line_height, Some(1.5));
                assert_eq!(style.space_before_px, 16.0);
                assert_eq!(style.space_after_px, 8.0);
                assert!(style.background.is_some());
            }
            other => panic!("{other:?}"),
        }
        assert!(s.run("format.align", &json!({"kind": "diagonal"})).is_err());
        s.run("format.numbering", &json!({})).unwrap();
        assert!(
            matches!(&s.composes[&id].body.blocks[0], Block::List { kind: ListKind::Numbered { .. }, items } if items.len() == 3)
        );
        s.run("format.numbering", &json!({"start": 5})).unwrap();
        assert!(matches!(
            &s.composes[&id].body.blocks[0],
            Block::List { kind: ListKind::Numbered { start: 5 }, .. }
        ));
        s.run("format.bullets", &json!({})).unwrap();
        assert!(matches!(&s.composes[&id].body.blocks[0], Block::List { kind: ListKind::Bullet, .. }));
        sel(&mut s, (&[0, 1, 0], 0), (&[0, 1, 0], 0));
        s.run("format.indent", &json!({})).unwrap();
        assert!(
            s.composes[&id].body.to_plain_text().contains("  \u{2022} two"),
            "{}",
            s.composes[&id].body.to_plain_text()
        );
        s.run("format.outdent", &json!({})).unwrap();
        s.run("format.bullets", &json!({"style": "none"})).unwrap();
        assert!(
            matches!(&s.composes[&id].body.blocks[0], Block::List { .. }),
            "only the caret item left the list"
        );
        s.run("format.multilevel", &json!({"style": "Outline numbered: L1: Bulleted"})).unwrap();
        assert_eq!(s.run("format.multilevel", &json!({"filter": "library"})).unwrap()["filter"], "library");
        assert_eq!(s.run("format.paragraphDialog", &json!({})).unwrap()["dialog"], "paragraph");
    }

    #[test]
    fn plain_indent_and_borders() {
        let (mut s, id) = with_text("para");
        s.run("format.indent", &json!({})).unwrap();
        assert_eq!(s.composes[&id].para_at_caret().indent_px, 48.0);
        s.run("format.outdent", &json!({})).unwrap();
        s.run("format.outdent", &json!({})).unwrap();
        assert_eq!(s.composes[&id].para_at_caret().indent_px, 0.0);
        s.run("format.borders", &json!({"kind": "bottom"})).unwrap();
        assert!(matches!(s.composes[&id].body.blocks[1], Block::Rule));
        s.run("format.borders", &json!({"kind": "none"})).unwrap();
        assert!(!s.composes[&id].body.blocks.iter().any(|b| matches!(b, Block::Rule)));
        assert!(s.run("format.borders", &json!({"kind": "left"})).is_err());
        assert_eq!(s.run("format.borders", &json!({"dialog": true})).unwrap()["dialog"], "bordersAndShading");
        s.run("insert.table", &json!({"rows": 2, "cols": 2})).unwrap();
        s.run("format.borders", &json!({"kind": "none"})).unwrap();
        assert!(s.composes[&id].body.blocks.iter().any(|b| matches!(b, Block::Table { border: false, .. })));
        s.run("format.shading", &json!({"color": "yellow"})).unwrap();
    }

    #[test]
    fn styles_and_style_sets() {
        let (mut s, id) = with_text("Title text\nbody");
        sel(&mut s, (&[0], 2), (&[0], 2));
        s.run("format.style", &json!({"name": "Heading 2"})).unwrap();
        assert!(matches!(s.composes[&id].body.blocks[0], Block::Heading { level: 2, .. }));
        s.run("format.style", &json!({"name": "¶ Normal"})).unwrap();
        assert!(matches!(s.composes[&id].body.blocks[0], Block::Paragraph { .. }));
        s.run("format.style", &json!({"name": "Title"})).unwrap();
        assert_eq!(first_style(&s, id).size_pt, Some(28.0));
        sel(&mut s, (&[1], 0), (&[1], 4));
        s.run("format.style", &json!({"name": "Strong"})).unwrap();
        s.run("format.style", &json!({"name": "Mine", "action": "create"})).unwrap();
        assert!(s.run("format.style", &json!({"name": "Nope"})).is_err());
        sel(&mut s, (&[0], 0), (&[0], 3));
        s.run("format.style", &json!({"name": "mine"})).unwrap();
        assert_eq!(first_style(&s, id).bold, Some(true));
        s.run("format.styleSet", &json!({"paragraphSpacing": "double", "name": "Basic"})).unwrap();
        match &s.composes[&id].body.blocks[1] {
            Block::Paragraph { style, .. } => assert_eq!(style.line_height, Some(2.0)),
            other => panic!("{other:?}"),
        }
        assert!(s.run("format.styleSet", &json!({"paragraphSpacing": "weird"})).is_err());
        s.run("format.styleSet", &json!({"action": "reset"})).unwrap();
        assert!(s.composes[&id].style_set.is_none());
        assert_eq!(s.run("format.stylesPane", &json!({})).unwrap()["stylesPane"], true);
        assert_eq!(s.run("format.stylesPane", &json!({"apply": true})).unwrap()["dialog"], "applyStyles");
    }

    #[test]
    fn clipboard_cut_copy_paste() {
        let (mut s, id) = with_text("alpha beta");
        assert!(s.run("format.copy", &json!({})).is_err(), "nothing selected");
        sel(&mut s, (&[0], 0), (&[0], 5));
        s.run("format.bold", &json!({})).unwrap();
        let r = s.run("format.cut", &json!({})).unwrap();
        assert_eq!(r["text"], "alpha");
        assert_eq!(s.composes[&id].body.to_plain_text(), " beta");
        assert_eq!(s.clipboard_text.as_deref(), Some("alpha"));
        sel(&mut s, (&[0], 5), (&[0], 5));
        s.run("format.paste", &json!({})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text(), " betaalpha");
        assert!(s.composes[&id].selection_all(|_, _| true));
        s.run("format.paste", &json!({"mode": "textOnly", "text": "!\nnext"})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text(), " betaalpha!\nnext");
        s.run("format.pasteText", &json!({"text": "x"})).unwrap();
        assert_eq!(s.run("format.pasteSpecial", &json!({})).unwrap()["dialog"], "pasteSpecial");
        s.run("format.pasteSpecial", &json!({"as": "text"})).unwrap();
        assert!(s.run("format.pasteSpecial", &json!({"as": "image"})).is_err());
        let r = s.run("format.clipboardPane", &json!({})).unwrap();
        assert_eq!(r["entries"][0], "alpha");
        assert!(s.run("format.paste", &json!({"mode": "weird"})).is_err());
    }

    #[test]
    fn find_replace_goto_select() {
        let (mut s, id) = with_text("cat dog cat\nbird");
        let r = s.run("format.find", &json!({"text": "cat"})).unwrap();
        assert_eq!(r["found"], true);
        assert_eq!(s.run("format.find", &json!({})).unwrap()["dialog"], "find");
        assert_eq!(
            s.run("format.replace", &json!({"find": "cat", "replace": "cow", "all": true})).unwrap()["replaced"],
            2
        );
        assert_eq!(s.composes[&id].body.to_plain_text(), "cow dog cow\nbird");
        let r = s.run("format.replace", &json!({"find": "dog", "replace": "pig"})).unwrap();
        assert_eq!(r["found"], true);
        let r = s.run("format.replace", &json!({"find": "dog", "replace": "pig"})).unwrap();
        assert_eq!(r["replaced"], 1);
        assert_eq!(s.composes[&id].body.to_plain_text(), "cow pig cow\nbird");
        s.run("format.goTo", &json!({"target": "paragraph:2"})).unwrap();
        assert_eq!(s.composes[&id].selection.focus, DocPos::new(vec![1], 0));
        s.run("insert.bookmark", &json!({"name": "here"})).unwrap();
        s.run("format.goTo", &json!({"target": "start"})).unwrap();
        s.run("format.goTo", &json!({"target": "bookmark:here"})).unwrap();
        assert_eq!(s.composes[&id].selection.focus, DocPos::new(vec![1], 0));
        assert!(s.run("format.goTo", &json!({"target": "table:3"})).is_err());
        assert_eq!(s.run("format.goTo", &json!({})).unwrap()["dialog"], "goTo");
        s.run("format.selectAll", &json!({})).unwrap();
        assert_eq!(s.composes[&id].selected_text(), "cow pig cow\nbird");
        sel(&mut s, (&[0], 0), (&[0], 3));
        s.run("format.bold", &json!({})).unwrap();
        let r = s.run("format.selectSimilar", &json!({})).unwrap();
        assert_eq!(r["ranges"].as_array().unwrap().len(), 1);
        sel(&mut s, (&[1], 0), (&[1], 0));
        let r = s.run("format.selectSimilar", &json!({})).unwrap();
        assert_eq!(r["ranges"].as_array().unwrap().len(), 2);
        s.run("format.italic", &json!({})).unwrap();
        s.run("format.changeCase", &json!({"kind": "upper"})).unwrap();
    }

    #[test]
    fn painter_toggles_and_view_flags() {
        let (mut s, id) = with_text("abc def");
        sel(&mut s, (&[0], 0), (&[0], 3));
        s.run("format.bold", &json!({})).unwrap();
        sel(&mut s, (&[0], 1), (&[0], 1));
        assert_eq!(s.run("format.formatPainter", &json!({})).unwrap()["armed"], true);
        sel(&mut s, (&[0], 4), (&[0], 7));
        assert!(s.composes[&id].painter.is_none());
        assert!(s.composes[&id].selection_all(|st, _| st.bold == Some(true)));
        for (cmd, key) in [
            ("format.showMarks", "showMarks"),
            ("format.showGridlines", "showGridlines"),
            ("format.selectObjects", "selectObjects"),
            ("format.selectionPane", "selectionPane"),
        ] {
            assert_eq!(s.run(cmd, &json!({})).unwrap()[key], true);
        }
        s.run("format.zoom", &json!({"pct": 150})).unwrap();
        assert_eq!(s.composes[&id].zoom_pct(), 150);
        assert!(s.run("format.zoom", &json!({"pct": 5})).is_err());
        assert_eq!(s.run("format.zoom", &json!({})).unwrap()["dialog"], "zoom");
        s.run("format.sort", &json!({"descending": true})).unwrap();
    }

    #[test]
    fn plain_text_disables_formatting() {
        let (mut s, _) = with_text("abc");
        s.run("compose.format", &json!({"kind": "plain"})).unwrap();
        assert!(matches!(s.run("format.bold", &json!({})), Err(CmdError::Disabled(_))));
        s.run("format.selectAll", &json!({})).unwrap();
        s.run("format.copy", &json!({})).unwrap();
    }
}
