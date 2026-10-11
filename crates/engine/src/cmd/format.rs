//! `format.*`: character / paragraph formatting, clipboard, find and selection in the compose body. All act
//! on the selection of `{"compose": id}` (default: the most recent compose). Character formatting with a
//! collapsed selection sets the typing style. Body edits keep their own editor undo (`compose.editUndo`).

use serde_json::{Value, json};

use super::compose::{
    compose_id, compose_mut, has_body_selection, has_compose, pos_json, rich_compose, spec,
};
use crate::cmd::util::{dialog, opt_bool, opt_str, req_str};
use crate::compose::{
    BodySelection, CaseKind, ClipEntry, ComposeState, DocPos, ListFormat, NamedStyle, NumberStyle,
    PainterState, PasteMode, align_of, multilevel_formats,
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
/// Subtitle / subtle reference greys.
const SUBTITLE: Color = Color::rgb(0x59, 0x59, 0x59);
const SUBTLE_REF: Color = Color::rgb(0x5a, 0x5a, 0x5a);
/// Colours the built-in styles set (Normal removes them).
const STYLE_COLORS: [Color; 4] = [ACCENT, SUBTLE, SUBTITLE, SUBTLE_REF];
/// Code style: monospace family and a light grey paragraph background.
const CODE_FONT: &str = "Consolas";
const CODE_BACKGROUND: Color = Color::rgb(0xf2, 0xf2, 0xf2);

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

/// The list format a Bullets / Numbering call asks for: `glyph` (Define New Bullet), `pattern` +
/// `numberStyle` (Define New Number Format) or a gallery `style` key. `Ok(None)` for a plain toggle.
fn requested_format(p: &Value, numbered: bool) -> Result<Option<ListFormat>, CmdError> {
    if let Some(g) = opt_str(p, "glyph").filter(|g| !g.trim().is_empty()) {
        return Ok(Some(ListFormat::bullet(g)));
    }
    if numbered && (p.get("pattern").is_some() || p.get("numberStyle").is_some()) {
        let style = match opt_str(p, "numberStyle") {
            Some(st) => Some(
                NumberStyle::parse(st)
                    .ok_or_else(|| CmdError::Params(format!("unknown number style `{st}`")))?,
            ),
            None => None,
        };
        let pattern = opt_str(p, "pattern").unwrap_or("1.");
        return Ok(Some(ListFormat::from_pattern(pattern, style)));
    }
    match opt_str(p, "style") {
        None => Ok(None),
        Some(key) => match ListFormat::from_key(key) {
            Some(f) if f.is_number() == numbered => Ok(Some(f)),
            _ => Err(CmdError::Params(format!(
                "unknown {} style `{key}`",
                if numbered { "numbering" } else { "bullet" }
            ))),
        },
    }
}

fn format_json(f: Option<&ListFormat>) -> Value {
    match f {
        None => Value::Null,
        Some(ListFormat::Bullet { glyph }) => json!({"glyph": glyph}),
        Some(ListFormat::Number { style, prefix, suffix, chain }) => {
            json!({"numberStyle": style.css(), "prefix": prefix, "suffix": suffix, "chain": chain, "sample": f.map(|f| f.marker(1, &[1]))})
        }
    }
}

fn list_cmd(s: &mut Session, p: &Value, kind: ListKind, define_dialog: &str) -> CmdResult {
    let numbered = matches!(kind, ListKind::Numbered { .. });
    let style = opt_str(p, "style");
    match style {
        Some("none") => {
            edit(s, p, |c| c.toggle_list(None))?;
            return Ok(json!({"list": null}));
        }
        Some("define") => {
            let cid = compose_id(s, p)?;
            let cur = s.composes.get(&cid).and_then(ComposeState::list_format_at_caret);
            return dialog(s, define_dialog, json!({"compose": cid, "format": format_json(cur.as_ref())}));
        }
        _ => {}
    }
    let format = requested_format(p, numbered)?;
    let c = compose_mut(s, p)?;
    c.checkpoint();
    if format.is_some() {
        // Applying a format keeps an existing list of the same kind.
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
    if let Some(f) = format {
        c.set_list_format(vec![f], false);
    }
    let now = c.list_at_caret().map(|(_, k)| match k {
        ListKind::Bullet => "bullet",
        ListKind::Numbered { .. } => "numbered",
    });
    let fmt = c.list_format_at_caret();
    Ok(json!({"list": now, "format": format_json(fmt.as_ref())}))
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

/// Multilevel gallery key for a key or an observed gallery name ("Outline numbered: L1: Numbered, I, II,
/// III, …, Number Format: Article I., …").
fn multilevel_key(name: &str) -> Option<&'static str> {
    const KEYS: [&str; 8] =
        ["numbered", "numberedParen", "outline", "article", "chapter", "outlineRoman", "bullets", "diamonds"];
    if let Some(k) = KEYS.iter().find(|k| k.eq_ignore_ascii_case(name)) {
        return Some(k);
    }
    let n = name.to_lowercase();
    Some(if n.contains("article") {
        "article"
    } else if n.contains("chapter") {
        "chapter"
    } else if n.contains("diamond") {
        "diamonds"
    } else if n.contains("bullet") {
        "bullets"
    } else if n.contains("format: i.") {
        "outlineRoman"
    } else if n.contains("format: 1)") {
        "numberedParen"
    } else if n.contains("1.1") || n.contains("legal") {
        "outline"
    } else if n.contains("format: 1") || n.contains("numbered") {
        "numbered"
    } else {
        return None;
    })
}

/// One level of a Define New Multilevel List definition: `{"glyph"}` or `{"pattern","numberStyle","chain"}`.
fn level_param(v: &Value) -> Result<ListFormat, CmdError> {
    if let Some(g) = v.get("glyph").and_then(Value::as_str) {
        return Ok(ListFormat::bullet(g));
    }
    let style = match v.get("numberStyle").and_then(Value::as_str) {
        Some(st) => Some(
            NumberStyle::parse(st).ok_or_else(|| CmdError::Params(format!("unknown number style `{st}`")))?,
        ),
        None => None,
    };
    let pattern = v.get("pattern").and_then(Value::as_str).unwrap_or("1.");
    let mut f = ListFormat::from_pattern(pattern, style);
    if v.get("chain").and_then(Value::as_bool) == Some(true)
        && let ListFormat::Number { chain, .. } = &mut f
    {
        *chain = true;
    }
    Ok(f)
}

fn multilevel(s: &mut Session, p: &Value) -> CmdResult {
    let style = opt_str(p, "style");
    if let Some(f) = opt_str(p, "filter") {
        if !matches!(f, "all" | "current" | "library") {
            return Err(CmdError::Params(format!("unknown gallery filter `{f}`")));
        }
        if style.is_none() && p.get("levels").is_none() {
            // Gallery filter: which definitions the gallery shows.
            let cid = compose_id(s, p)?;
            let current = s.composes.get(&cid).and_then(ComposeState::list_format_at_caret);
            let library = [
                "numbered",
                "numberedParen",
                "outline",
                "article",
                "chapter",
                "outlineRoman",
                "bullets",
                "diamonds",
            ];
            let shown: Vec<&str> = match f {
                "current" if current.is_none() => Vec::new(),
                "current" => vec!["current"],
                _ => library.to_vec(),
            };
            return Ok(json!({"filter": f, "shown": shown, "current": format_json(current.as_ref())}));
        }
    }
    let levels: Option<Vec<ListFormat>> = match (p.get("levels"), style) {
        (Some(v), _) => {
            let arr = v.as_array().ok_or_else(|| CmdError::Params("`levels` must be a list".into()))?;
            if arr.is_empty() || arr.len() > crate::compose::MAX_LIST_LEVEL {
                return Err(CmdError::Params("`levels` must define 1 to 9 levels".into()));
            }
            Some(arr.iter().map(level_param).collect::<Result<Vec<_>, _>>()?)
        }
        (None, None) => Some(multilevel_formats("numbered").unwrap_or_default()),
        (None, Some("none" | "None")) => {
            return list_cmd(s, &json!({"style": "none", "compose": p.get("compose")}), ListKind::Bullet, "");
        }
        (None, Some("define")) => {
            return dialog(s, "defineNewMultilevelList", json!({"compose": compose_id(s, p)?}));
        }
        (None, Some("defineStyle")) => {
            return dialog(s, "defineNewListStyle", json!({"compose": compose_id(s, p)?}));
        }
        (None, Some(name)) => {
            let key = multilevel_key(name)
                .ok_or_else(|| CmdError::Params(format!("unknown multilevel list `{name}`")))?;
            multilevel_formats(key)
        }
    };
    let levels = levels.unwrap_or_default();
    let numbered = levels.first().is_none_or(ListFormat::is_number);
    let kind = if numbered { ListKind::Numbered { start: 1 } } else { ListKind::Bullet };
    let c = compose_mut(s, p)?;
    c.checkpoint();
    let same = matches!(
        (c.list_at_caret().map(|(_, k)| k), &kind),
        (Some(ListKind::Bullet), ListKind::Bullet)
            | (Some(ListKind::Numbered { .. }), ListKind::Numbered { .. })
    );
    if !same {
        c.toggle_list(Some(kind));
    }
    c.set_list_format(levels, true);
    let fmt = c.list_format_at_caret();
    Ok(json!({"list": if numbered { "numbered" } else { "bullet" }, "format": format_json(fmt.as_ref())}))
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
            let theme_accent = accent(c);
            let heading_font = (c.theme != Default::default()).then(|| c.theme_resolved().heading_font);
            c.set_heading(None);
            c.set_para_style(|ps| *ps = ParaStyle::default());
            // Drop what the paragraph styles set (Title size, Quote colours, Code font), keep the rest.
            c.with_whole_paragraphs(|c| {
                c.apply_text_style(|st| {
                    st.size_pt = None;
                    st.monospace = None;
                    if st.font_family.as_deref() == Some(CODE_FONT)
                        || (heading_font.is_some() && st.font_family == heading_font)
                    {
                        st.font_family = None;
                    }
                    if st.color.is_some_and(|col| STYLE_COLORS.contains(&col) || col == theme_accent) {
                        st.color = None;
                        st.italic = None;
                    }
                });
            });
        }
        "code" => {
            c.set_heading(None);
            c.set_para_style(|ps| {
                ps.space_before_px = 0.0;
                ps.space_after_px = 0.0;
                ps.line_height = Some(1.0);
                ps.background = Some(CODE_BACKGROUND);
            });
            c.with_whole_paragraphs(|c| {
                c.apply_text_style(|st| {
                    st.monospace = Some(true);
                    st.font_family = Some(CODE_FONT.to_owned());
                });
            });
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
            themed_heading(c, 1);
            c.with_whole_paragraphs(|c| c.apply_text_style(|st| st.size_pt = Some(28.0)));
        }
        "subtitle" => {
            c.set_heading(None);
            c.with_whole_paragraphs(|c| {
                c.apply_text_style(|st| {
                    st.size_pt = Some(14.0);
                    st.color = Some(SUBTITLE);
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
            let accent = accent(c);
            c.set_heading(None);
            c.set_para_style(|ps| {
                ps.align = mailcraft_model::rich::Align::Center;
                ps.indent_px = 48.0;
            });
            c.with_whole_paragraphs(|c| {
                c.apply_text_style(|st| {
                    st.italic = Some(true);
                    st.color = Some(accent);
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
        "intense emphasis" => {
            let accent = accent(c);
            char_style(c, &|st| {
                st.italic = Some(true);
                st.color = Some(accent);
            });
        }
        "strong" => char_style(c, &|st| st.bold = Some(true)),
        "subtle reference" => char_style(c, &|st| st.color = Some(SUBTLE_REF)),
        "intense reference" => {
            let accent = accent(c);
            char_style(c, &|st| {
                st.bold = Some(true);
                st.color = Some(accent);
            });
        }
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
            themed_heading(c, level.min(6));
        }
    }
    true
}

/// Heading formatting from the theme and style set, when either was chosen (otherwise headings keep the
/// plain built-in look).
fn themed_heading(c: &mut ComposeState, level: u8) {
    if c.theme == Default::default() && c.style_set.is_none() {
        return;
    }
    let hs = c.heading_text_style(level);
    let set = c.style_set_def().copied();
    let accent = c.theme_resolved().color(crate::compose::ACCENT1);
    if let Some(set) = set {
        c.set_para_style(|ps| {
            ps.align = if set.centered {
                mailcraft_model::rich::Align::Center
            } else {
                mailcraft_model::rich::Align::Start
            };
            ps.background = (set.shaded && level == 1).then_some(accent);
        });
    }
    c.with_whole_paragraphs(|c| {
        c.apply_text_style(|st| {
            st.font_family = hs.font_family.clone();
            if hs.size_pt.is_some() {
                st.size_pt = hs.size_pt;
                st.bold = hs.bold;
                st.italic = hs.italic;
                st.color = hs.color;
            }
        });
    });
}

/// Accent colour for the emphasis styles: the theme's Accent 1 when a theme was chosen.
fn accent(c: &ComposeState) -> Color {
    if c.theme == Default::default() { ACCENT } else { c.theme_resolved().color(crate::compose::ACCENT1) }
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
            c.style_set = Some("Default".to_owned());
            c.apply_style_set();
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
            if let Some(n) = &name {
                let Some(set) = crate::compose::style_set(n) else {
                    c.undo_edit();
                    c.edit_redo.clear();
                    return Err(CmdError::Params(format!("unknown style set `{n}`")));
                };
                c.style_set = Some(set.name.to_owned());
                c.apply_style_set();
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

/// Largest picture embedded from pasted HTML (bigger `data:` pictures are left out).
const MAX_PASTED_IMAGE_BYTES: usize = 25 * 1024 * 1024;
/// Most pictures embedded from one paste.
const MAX_PASTED_IMAGES: usize = 200;
/// Deepest block nesting walked for pasted pictures.
const MAX_PASTE_DEPTH: usize = 64;

/// `data:image/png;base64,…` → (MIME type, bytes); `None` for other URLs, other encodings and junk.
fn decode_data_image(src: &str) -> Option<(String, Vec<u8>)> {
    use base64::Engine as _;
    let rest = src.trim().get(..5).filter(|p| p.eq_ignore_ascii_case("data:")).and(src.trim().get(5..))?;
    let (meta, payload) = rest.split_once(',')?;
    let mut parts = meta.split(';').map(str::trim);
    let mime = parts.next()?.to_ascii_lowercase();
    let valid = mime.strip_prefix("image/").is_some_and(|sub| {
        !sub.is_empty() && sub.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    if !valid || !parts.any(|x| x.eq_ignore_ascii_case("base64")) {
        return None;
    }
    // Base64 is 4/3 the size of its bytes: refuse oversized pictures before decoding.
    if payload.len() / 4 * 3 > MAX_PASTED_IMAGE_BYTES {
        return None;
    }
    let clean: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
    let data = base64::engine::general_purpose::STANDARD
        .decode(clean.as_bytes())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(clean.as_bytes()))
        .ok()?;
    (!data.is_empty() && data.len() <= MAX_PASTED_IMAGE_BYTES).then_some((mime, data))
}

/// File name extension for an image MIME type.
fn image_ext(mime: &str) -> &str {
    match mime {
        "image/jpeg" | "image/jpg" | "image/pjpeg" => "jpg",
        "image/svg+xml" => "svg",
        m => {
            m.strip_prefix("image/").filter(|x| x.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or("img")
        }
    }
}

/// Pictures in pasted HTML that carry their data (`data:image/…;base64,…`, as browsers and office programs
/// copy screenshots and pictures) become inline attachments of the compose referenced as `cid:`, like Insert
/// Picture makes them: many mail programs refuse `data:` pictures. Ones that cannot be decoded, or beyond
/// the size and count limits, are left out. A `cid:` picture copied from another open message (`others`:
/// the other composes' inline pictures) is brought along, so it is not sent as a dangling reference.
fn embed_pasted_images(doc: &mut Doc, c: &mut ComposeState, others: &[crate::compose::InlineImage]) {
    fn inlines(
        list: &mut Vec<mailcraft_model::Inline>,
        c: &mut ComposeState,
        others: &[crate::compose::InlineImage],
        n: &mut usize,
        depth: usize,
    ) {
        if depth > MAX_PASTE_DEPTH {
            return;
        }
        list.retain_mut(|i| match i {
            mailcraft_model::Inline::Image { src, .. }
                if src.trim_start().get(..4).is_some_and(|p| p.eq_ignore_ascii_case("cid:")) =>
            {
                let id = src.trim_start().get(4..).unwrap_or_default().trim();
                let known = |x: &crate::compose::InlineImage| x.content_id.eq_ignore_ascii_case(id);
                if !c.inline_images.iter().any(known)
                    && let Some(img) = others.iter().find(|x| known(x))
                {
                    c.inline_images.push(img.clone());
                }
                true
            }
            mailcraft_model::Inline::Image { src, .. }
                if src.trim_start().get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) =>
            {
                let Some((mime, data)) = decode_data_image(src).filter(|_| *n < MAX_PASTED_IMAGES) else {
                    return false;
                };
                *n += 1;
                let cid = c.next_content_id();
                let filename =
                    format!("image{:03}.{}", c.inline_images.len().saturating_add(1), image_ext(&mime));
                c.inline_images.push(crate::compose::InlineImage {
                    content_id: cid.clone(),
                    filename,
                    mime_type: mime,
                    data,
                });
                *src = format!("cid:{cid}");
                true
            }
            mailcraft_model::Inline::Link { children, .. } => {
                inlines(children, c, others, n, depth.saturating_add(1));
                true
            }
            _ => true,
        });
    }
    fn blocks(
        list: &mut [mailcraft_model::Block],
        c: &mut ComposeState,
        others: &[crate::compose::InlineImage],
        n: &mut usize,
        depth: usize,
    ) {
        use mailcraft_model::Block;
        if depth > MAX_PASTE_DEPTH {
            return;
        }
        let d = depth.saturating_add(1);
        for b in list {
            match b {
                Block::Paragraph { inlines: l, .. } | Block::Heading { inlines: l, .. } => {
                    inlines(l, c, others, n, d)
                }
                Block::List { items, .. } => {
                    for item in items {
                        blocks(item, c, others, n, d);
                    }
                }
                Block::Quote { blocks: inner } | Block::Container { blocks: inner, .. } => {
                    blocks(inner, c, others, n, d)
                }
                Block::Table { rows, .. } => {
                    for cell in rows.iter_mut().flatten() {
                        blocks(&mut cell.blocks, c, others, n, d);
                    }
                }
                Block::Pre { .. } | Block::Rule => {}
            }
        }
    }
    let mut n = 0usize;
    blocks(&mut doc.blocks, c, others, &mut n, 0);
}

fn do_paste(s: &mut Session, p: &Value, mode: PasteMode) -> CmdResult {
    let cid = compose_id(s, p)?;
    let mut doc = paste_source(s, p, cid)?;
    let others: Vec<crate::compose::InlineImage> = s
        .composes
        .iter()
        .filter(|(id, _)| **id != cid)
        .flat_map(|(_, x)| x.inline_images.iter().cloned())
        .take(MAX_PASTED_IMAGES)
        .collect();
    let c = compose_mut(s, p)?;
    let mode = if c.format == mailcraft_model::BodyFormat::PlainText { PasteMode::TextOnly } else { mode };
    if mode != PasteMode::TextOnly {
        embed_pasted_images(&mut doc, c, &others);
    }
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
        s.run("format.styleSet", &json!({"paragraphSpacing": "double", "name": "Basic (Simple)"})).unwrap();
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

    fn send_html(s: &mut Session) -> String {
        s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
        assert_eq!(s.run("compose.send", &json!({})).unwrap()["sent"], true);
        let m = s.mailbox.mail.values().filter(|m| !m.is_draft).max_by_key(|m| m.id).unwrap();
        m.body.html.clone().unwrap()
    }

    #[test]
    fn number_formats_bullet_glyphs_and_markers() {
        let (mut s, id) = with_text("one\ntwo\nthree");
        s.run("compose.select", &json!({"all": true})).unwrap();
        let r = s.run("format.numbering", &json!({"style": "lowerRoman"})).unwrap();
        assert_eq!(r["list"], "numbered");
        assert_eq!(r["format"]["numberStyle"], "lower-roman");
        let markers: Vec<String> = s.composes[&id].list_markers().into_iter().map(|m| m.2).collect();
        assert_eq!(markers, vec!["i.", "ii.", "iii."]);
        assert_eq!(s.composes[&id].plain_text_with_list_formats(), "i. one\nii. two\niii. three");
        assert_eq!(s.composes[&id].list_markers()[2].3, "3.", "what the layout would draw");
        // The format survives typing a paragraph above the list (the list moves).
        s.run("compose.select", &json!({"anchor": {"path": [0, 0, 0], "offset": 0}})).unwrap();
        s.run("format.numbering", &json!({"style": "decimalParen"})).unwrap();
        s.composes.get_mut(&id).unwrap().body.blocks.insert(0, Block::text_paragraph("intro"));
        s.composes.get_mut(&id).unwrap().set_caret(crate::compose::DocPos::new(vec![0], 5));
        s.run("compose.insertText", &json!({"text": "!"})).unwrap();
        let markers: Vec<String> = s.composes[&id].list_markers().into_iter().map(|m| m.2).collect();
        assert_eq!(markers, vec!["1)", "2)", "3)"]);
        // Undo / redo carry the formats with the body.
        s.run("format.numbering", &json!({"style": "upperAlpha"})).ok();
        let html = send_html(&mut s);
        assert!(html.contains("1)\u{a0}one") || html.contains("list-style-type"), "{html}");
        let _ = id;
    }

    #[test]
    fn list_formats_in_sent_html() {
        let (mut s, _) = with_text("a\nb");
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("format.numbering", &json!({"style": "upperRoman"})).unwrap();
        let html = send_html(&mut s);
        assert!(html.contains("<ol style=\"list-style-type:upper-roman\">"), "{html}");

        let (mut s, _) = with_text("a\nb");
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("format.numbering", &json!({"style": "decimalParen"})).unwrap();
        let html = send_html(&mut s);
        assert!(html.contains("list-style-type:none"), "{html}");
        assert!(html.contains(">1)\u{a0}a") && html.contains(">2)\u{a0}b"), "{html}");

        let (mut s, _) = with_text("a\nb");
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("format.bullets", &json!({"style": "check"})).unwrap();
        let html = send_html(&mut s);
        assert!(html.contains(">\u{2714}\u{a0}a"), "{html}");
        // Unformatted lists are sent as before.
        let (mut s, _) = with_text("a\nb");
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("format.bullets", &json!({})).unwrap();
        let html = send_html(&mut s);
        assert!(html.contains("<ul>") && !html.contains("list-style-type"), "{html}");
    }

    #[test]
    fn define_new_formats_and_multilevel_lists() {
        let (mut s, id) = with_text("one\ntwo\nthree");
        s.run("compose.select", &json!({"all": true})).unwrap();
        assert_eq!(
            s.run("format.numbering", &json!({"style": "define"})).unwrap()["dialog"],
            "defineNewNumberFormat"
        );
        s.run("format.numbering", &json!({"pattern": "Step 1:"})).unwrap();
        let m: Vec<String> = s.composes[&id].list_markers().into_iter().map(|m| m.2).collect();
        assert_eq!(m, vec!["Step 1:", "Step 2:", "Step 3:"]);
        s.run("format.numbering", &json!({"pattern": "(a)", "numberStyle": "upperRoman"})).unwrap();
        assert_eq!(s.composes[&id].list_markers()[1].2, "(II)");
        assert!(s.run("format.numbering", &json!({"numberStyle": "klingon"})).is_err());
        s.run("format.bullets", &json!({"glyph": "\u{2192}"})).unwrap();
        assert_eq!(s.composes[&id].list_markers()[0].2, "\u{2192}");
        assert!(s.run("format.bullets", &json!({"style": "zigzag"})).is_err());

        // Multilevel: outline numbering 1. / 1.1.
        let (mut s, id) = with_text("one\ntwo\nthree");
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("format.multilevel", &json!({"style": "outline"})).unwrap();
        s.run(
            "compose.select",
            &json!({"anchor": {"path": [0, 1, 0], "offset": 0}, "focus": {"path": [0, 2, 0], "offset": 1}}),
        )
        .unwrap();
        s.run("format.indent", &json!({})).unwrap();
        let m: Vec<String> = s.composes[&id].list_markers().into_iter().map(|m| m.2).collect();
        assert_eq!(m, vec!["1.", "1.1.", "1.2."], "{m:?}");
        // Observed gallery names map to definitions.
        let (mut s, id) = with_text("x\ny");
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("format.multilevel", &json!({"style": "Outline numbered: L1: Numbered, I, II, III, …, ALIGNMENT Left, Number Format: Article I., Aligned at:  0"})).unwrap();
        assert_eq!(s.composes[&id].list_markers()[1].2, "Article II.");
        s.run("format.multilevel", &json!({"levels": [{"pattern": "Part A"}, {"glyph": "-"}]})).unwrap();
        assert_eq!(s.composes[&id].list_markers()[0].2, "Part A");
        assert!(s.run("format.multilevel", &json!({"levels": []})).is_err());
        assert!(s.run("format.multilevel", &json!({"style": "Mystery"})).is_err());
        let f = s.run("format.multilevel", &json!({"filter": "current"})).unwrap();
        assert_eq!(f["shown"], json!(["current"]));
    }

    #[test]
    fn number_styles_and_patterns() {
        use crate::compose::{ListFormat, NumberStyle};
        assert_eq!(NumberStyle::UpperRoman.format(1994), "MCMXCIV");
        assert_eq!(NumberStyle::LowerAlpha.format(28), "ab");
        assert_eq!(NumberStyle::UpperAlpha.format(0), "0");
        assert_eq!(NumberStyle::DecimalZero.format(7), "07");
        assert_eq!(NumberStyle::LowerRoman.format(4000), "4000");
        assert_eq!(ListFormat::from_pattern("Chapter 1", None).marker(3, &[]), "Chapter 3");
        assert_eq!(ListFormat::from_pattern("a)", None).marker(2, &[]), "b)");
        assert_eq!(ListFormat::from_pattern("Article I.", None).marker(4, &[]), "Article IV.");
        assert_eq!(ListFormat::from_pattern("01.", None).marker(5, &[]), "05.");
        assert_eq!(ListFormat::from_pattern("-", Some(NumberStyle::Decimal)).marker(2, &[]), "2-");
        assert_eq!(ListFormat::from_key("decimal").unwrap().css_keyword(), Some("decimal"));
        assert_eq!(ListFormat::from_key("decimalParen").unwrap().css_keyword(), None);
        assert_eq!(ListFormat::bullet("").marker(1, &[]), "\u{2022}");
        assert_eq!(ListFormat::bullet(&"x".repeat(100)).marker(1, &[]).len(), 16);
    }

    #[test]
    fn style_sets_restyle_headings_and_spacing() {
        use mailcraft_model::rich::Align;
        let (mut s, id) = with_text("Head\nbody");
        sel(&mut s, (&[0], 1), (&[0], 1));
        s.run("format.style", &json!({"name": "Heading 1"})).unwrap();
        s.run("format.styleSet", &json!({"name": "Shaded"})).unwrap();
        let theme = s.composes[&id].theme_resolved();
        match &s.composes[&id].body.blocks[0] {
            Block::Heading { style, inlines, .. } => {
                assert_eq!(style.background, Some(theme.color(crate::compose::ACCENT1)));
                match &inlines[0] {
                    Inline::Text { style, .. } => {
                        assert_eq!(style.size_pt, Some(16.0));
                        assert_eq!(style.bold, Some(true));
                        assert_eq!(style.color, Some(theme.color(1)));
                    }
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("{other:?}"),
        }
        s.run("format.styleSet", &json!({"name": "Centered"})).unwrap();
        assert!(
            matches!(&s.composes[&id].body.blocks[0], Block::Heading { style, .. } if style.align == Align::Center && style.background.is_none())
        );
        match &s.composes[&id].body.blocks[1] {
            Block::Paragraph { style, .. } => assert_eq!(style.line_height, Some(1.15)),
            other => panic!("{other:?}"),
        }
        // New headings follow the set too.
        sel(&mut s, (&[1], 1), (&[1], 1));
        s.run("format.style", &json!({"name": "Heading 2"})).unwrap();
        match &s.composes[&id].body.blocks[1] {
            Block::Heading { style, inlines, .. } => {
                assert_eq!(style.align, Align::Center);
                assert!(matches!(&inlines[0], Inline::Text { style, .. } if style.size_pt == Some(14.0)));
            }
            other => panic!("{other:?}"),
        }
        assert!(s.run("format.styleSet", &json!({"name": "Plaid"})).is_err());
        s.run("format.styleSet", &json!({"action": "reset"})).unwrap();
        assert!(s.composes[&id].style_set.is_none());
        assert!(
            matches!(&s.composes[&id].body.blocks[0], Block::Heading { style, .. } if style.align == Align::Start)
        );
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
    fn pasted_data_pictures_become_inline_attachments() {
        let (mut s, id) = with_text("x");
        // A 1x1 PNG as a browser copies it, plus a broken one, a non-image and a remote picture.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let html = format!(
            "<p>Look <img src=\"data:image/png;base64,{png}\" alt=\"dot\"> \
             <img src=\"data:image/png;base64,%%%\"> <a href=\"https://example.com\"><img src=\"DATA:image/gif;base64,R0lGODlhAQABAAAAACw=\"></a> \
             <img src=\"https://example.com/a.png\"></p>"
        );
        s.run("format.paste", &json!({"html": html})).unwrap();
        let c = &s.composes[&id];
        assert_eq!(c.inline_images.len(), 2, "{:?}", c.images());
        assert_eq!(c.inline_images[0].mime_type, "image/png");
        assert_eq!(&c.inline_images[0].data[1..4], b"PNG");
        assert_eq!(c.inline_images[1].mime_type, "image/gif");
        assert!(c.inline_images[1].filename.ends_with(".gif"));
        let srcs: Vec<String> = c.images().into_iter().map(|(src, _)| src).collect();
        assert_eq!(srcs.len(), 3, "the broken picture is left out: {srcs:?}");
        assert!(srcs.iter().all(|x| !x.starts_with("data:")), "{srcs:?}");
        assert_eq!(srcs[0], format!("cid:{}", c.inline_images[0].content_id));
        assert_eq!(srcs[2], "https://example.com/a.png");
        // Sent, the pictures travel as related parts.
        let cids: Vec<String> = c.inline_images.iter().map(|i| i.content_id.clone()).collect();
        s.run("compose.setTo", &json!({"value": "a@example.org"})).unwrap();
        s.run("compose.send", &json!({})).unwrap();
        let out = s
            .mailbox
            .mail
            .values()
            .filter_map(|m| m.raw.as_ref())
            .map(|r| String::from_utf8_lossy(r).into_owned())
            .find(|r| r.contains("Content-ID"))
            .unwrap();
        for cid in &cids {
            assert!(out.contains(&format!("<{cid}>")), "{out}");
        }
        assert!(!out.contains("data:image"), "{out}");
        // A picture copied from another message comes along with it.
        let (mut s, a) = with_text("");
        s.run("insert.picture", &json!({"compose": a, "name": "dot.png", "data_base64": png})).unwrap();
        s.run(
            "compose.select",
            &json!({"compose": a, "anchor": {"path": [0], "offset": 0}, "focus": {"path": [0], "offset": 1}}),
        )
        .unwrap();
        let copied =
            s.run("format.copy", &json!({"compose": a})).unwrap()["html"].as_str().unwrap().to_owned();
        assert!(copied.contains("cid:"), "{copied}");
        let b = open(&mut s);
        s.run("format.paste", &json!({"compose": b, "html": copied})).unwrap();
        assert_eq!(s.composes[&b].inline_images, s.composes[&a].inline_images);
        // Text-only paste embeds nothing.
        let (mut s, id) = with_text("y");
        s.run("format.paste", &json!({"html": html, "mode": "textOnly"})).unwrap();
        assert!(s.composes[&id].inline_images.is_empty());
        // Junk never panics.
        for src in [
            "data:",
            "data:image/png",
            "data:image/;base64,AA",
            "data:text/html;base64,PGI+",
            "data:image/png;base64,",
        ] {
            assert!(decode_data_image(src).is_none(), "{src}");
        }
        assert_eq!(image_ext("image/jpeg"), "jpg");
        assert_eq!(image_ext("image/x.y"), "img");
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

#[cfg(test)]
mod range_tests {
    //! Every formatting command on a selection that spans paragraphs, plus undo/redo of each.

    use super::super::compose::tests::{open, session};
    use super::*;
    use mailcraft_model::rich::Align;
    use mailcraft_model::{Block, Inline};

    const LINES: &str = "alpha beta\ngamma\ndelta epsilon";

    /// Session with three paragraphs and "beta … delta" selected (spanning all three).
    fn spanning() -> (Session, u64) {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": LINES})).unwrap();
        s.run(
            "compose.select",
            &json!({"anchor": {"path": [0], "offset": 6}, "focus": {"path": [2], "offset": 5}}),
        )
        .unwrap();
        (s, id)
    }

    /// (char, style) of every char of top-level block `i`.
    fn chars(s: &Session, id: u64, i: usize) -> Vec<(char, TextStyle)> {
        let mut out = Vec::new();
        let inlines = match &s.composes[&id].body.blocks[i] {
            Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => inlines.clone(),
            other => panic!("{other:?}"),
        };
        fn walk(inlines: &[Inline], out: &mut Vec<(char, TextStyle)>) {
            for x in inlines {
                match x {
                    Inline::Text { text, style } => out.extend(text.chars().map(|c| (c, style.clone()))),
                    Inline::Link { children, .. } => walk(children, out),
                    _ => {}
                }
            }
        }
        walk(&inlines, &mut out);
        out
    }

    /// Checks `pred` on the selected chars and its negation on "alpha " and " epsilon".
    fn check_range(s: &Session, id: u64, what: &str, pred: impl Fn(&TextStyle) -> bool) {
        let p0 = chars(s, id, 0);
        let p1 = chars(s, id, 1);
        let p2 = chars(s, id, 2);
        for (k, (c, st)) in p0.iter().enumerate() {
            assert_eq!(pred(st), k >= 6, "{what}: para 0 char {k} {c:?}");
        }
        assert!(p1.iter().all(|(_, st)| pred(st)), "{what}: para 1");
        for (k, (c, st)) in p2.iter().enumerate() {
            assert_eq!(pred(st), k < 5, "{what}: para 2 char {k} {c:?}");
        }
    }

    fn undo_redo_round_trip(s: &mut Session, id: u64) {
        let after = s.composes[&id].body.clone();
        assert_eq!(s.run("compose.editUndo", &json!({})).unwrap()["undone"], true);
        assert_ne!(s.composes[&id].body, after, "undo restores");
        assert_eq!(s.run("compose.editRedo", &json!({})).unwrap()["redone"], true);
        assert_eq!(s.composes[&id].body, after, "redo reapplies");
    }

    #[test]
    fn character_formatting_spans_paragraphs() {
        type Case = (&'static str, Value, fn(&TextStyle) -> bool);
        let cases: Vec<Case> = vec![
            ("format.bold", json!({}), |st| st.bold == Some(true)),
            ("format.italic", json!({}), |st| st.italic == Some(true)),
            ("format.underline", json!({}), |st| st.underline == Some(true)),
            ("format.strike", json!({}), |st| st.strike == Some(true)),
            ("format.sub", json!({}), |st| st.subscript == Some(true)),
            ("format.sup", json!({}), |st| st.superscript == Some(true)),
            ("format.font", json!({"family": "Georgia"}), |st| st.font_family.as_deref() == Some("Georgia")),
            ("format.fontSize", json!({"pt": 18}), |st| st.size_pt == Some(18.0)),
            ("format.growFont", json!({}), |st| st.size_pt == Some(14.0)),
            ("format.shrinkFont", json!({}), |st| st.size_pt == Some(11.0)),
            ("format.color", json!({"color": "#336699"}), |st| {
                st.color == Some(Color::rgb(0x33, 0x66, 0x99))
            }),
            ("format.highlight", json!({"color": "yellow"}), |st| st.highlight.is_some()),
        ];
        for (id_, params, pred) in cases {
            let (mut s, id) = spanning();
            s.run(id_, &params).unwrap();
            check_range(&s, id, id_, pred);
            undo_redo_round_trip(&mut s, id);
            // The selection is kept, so the toggles turn off again over the same range.
            if params == json!({}) && !id_.contains("Font") {
                s.run(id_, &params).unwrap();
                for i in 0..3 {
                    assert!(chars(&s, id, i).iter().all(|(_, st)| !pred(st)), "{id_} off again: {i}");
                }
            }
        }
        // Clear Formatting removes everything from the range only.
        let (mut s, id) = spanning();
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("format.bold", &json!({})).unwrap();
        s.run("format.color", &json!({"color": "red"})).unwrap();
        s.run(
            "compose.select",
            &json!({"anchor": {"path": [0], "offset": 6}, "focus": {"path": [2], "offset": 5}}),
        )
        .unwrap();
        s.run("format.clearFormatting", &json!({})).unwrap();
        check_range(&s, id, "clear", |st| *st == TextStyle::default());
        undo_redo_round_trip(&mut s, id);
    }

    fn para(s: &Session, id: u64, i: usize) -> ParaStyle {
        match &s.composes[&id].body.blocks[i] {
            Block::Paragraph { style, .. } | Block::Heading { style, .. } => style.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn paragraph_formatting_spans_paragraphs() {
        let (mut s, id) = spanning();
        s.run("format.align", &json!({"kind": "center"})).unwrap();
        assert!((0..3).all(|i| para(&s, id, i).align == Align::Center));
        undo_redo_round_trip(&mut s, id);
        s.run("format.indent", &json!({})).unwrap();
        assert!((0..3).all(|i| para(&s, id, i).indent_px == crate::compose::INDENT_STEP_PX));
        s.run("format.outdent", &json!({})).unwrap();
        assert!((0..3).all(|i| para(&s, id, i).indent_px == 0.0));
        s.run("format.lineSpacing", &json!({"multiple": 1.5})).unwrap();
        assert!((0..3).all(|i| para(&s, id, i).line_height == Some(1.5)));
        s.run("format.paragraphSpacing", &json!({"before": 6, "after": 12})).unwrap();
        assert!(
            (0..3).all(|i| para(&s, id, i).space_after_px == 16.0 && para(&s, id, i).space_before_px == 8.0)
        );
        undo_redo_round_trip(&mut s, id);
    }

    fn list_items(s: &Session, id: u64) -> Option<(bool, usize)> {
        s.composes[&id].body.blocks.iter().find_map(|b| match b {
            Block::List { kind, items } => Some((matches!(kind, ListKind::Bullet), items.len())),
            _ => None,
        })
    }

    #[test]
    fn lists_span_paragraphs() {
        let (mut s, id) = spanning();
        s.run("format.bullets", &json!({})).unwrap();
        assert_eq!(list_items(&s, id), Some((true, 3)));
        s.run("format.numbering", &json!({})).unwrap();
        assert_eq!(list_items(&s, id), Some((false, 3)), "switches the kind");
        undo_redo_round_trip(&mut s, id);
        // Multilevel: second and third item one level down.
        s.run(
            "compose.select",
            &json!({"anchor": {"path": [0, 1, 0], "offset": 0}, "focus": {"path": [0, 2, 0], "offset": 2}}),
        )
        .unwrap();
        s.run("format.indent", &json!({})).unwrap();
        match &s.composes[&id].body.blocks[0] {
            Block::List { items, .. } => {
                assert_eq!(items.len(), 1);
                assert!(matches!(items[0].last(), Some(Block::List { items, .. }) if items.len() == 2));
            }
            other => panic!("{other:?}"),
        }
        s.run("format.outdent", &json!({})).unwrap();
        assert_eq!(list_items(&s, id), Some((false, 3)));
        s.run("compose.select", &json!({"all": true})).unwrap();
        s.run("format.numbering", &json!({})).unwrap();
        assert_eq!(list_items(&s, id), None, "same kind again removes the list");
        s.run("format.multilevel", &json!({})).unwrap();
        assert_eq!(list_items(&s, id), Some((false, 3)));
    }

    #[test]
    fn paragraph_styles_span_paragraphs() {
        for (name, level) in [("Heading 1", 1u8), ("Heading 2", 2), ("Heading 3", 3)] {
            let (mut s, id) = spanning();
            s.run("format.style", &json!({"name": name})).unwrap();
            for i in 0..3 {
                assert!(
                    matches!(s.composes[&id].body.blocks[i], Block::Heading { level: l, .. } if l == level),
                    "{name} {i}"
                );
            }
            undo_redo_round_trip(&mut s, id);
            s.run("format.style", &json!({"name": "Normal"})).unwrap();
            assert!((0..3).all(|i| matches!(s.composes[&id].body.blocks[i], Block::Paragraph { .. })));
        }
        // Title / Quote / Code apply to whole paragraphs; Normal takes their formatting off again.
        let (mut s, id) = spanning();
        s.run("format.style", &json!({"name": "Title"})).unwrap();
        assert!(chars(&s, id, 0).iter().all(|(_, st)| st.size_pt == Some(28.0)), "whole first paragraph");
        s.run("format.style", &json!({"name": "Quote"})).unwrap();
        assert!(chars(&s, id, 2).iter().all(|(_, st)| st.italic == Some(true)));
        assert_eq!(para(&s, id, 1).align, Align::Center);
        s.run("format.style", &json!({"name": "Code"})).unwrap();
        assert!((0..3).all(|i| chars(&s, id, i).iter().all(|(_, st)| st.monospace == Some(true))));
        assert!(para(&s, id, 1).background.is_some());
        s.run("format.style", &json!({"name": "Normal"})).unwrap();
        for i in 0..3 {
            assert_eq!(para(&s, id, i), ParaStyle::default());
            assert!(
                chars(&s, id, i).iter().all(|(_, st)| st.size_pt.is_none()
                    && st.monospace.is_none()
                    && st.font_family.is_none()
                    && st.color.is_none()),
                "{i}: {:?}",
                chars(&s, id, i)
            );
        }
        assert!(s.run("format.style", &json!({"name": "No Such Style"})).is_err());
    }

    #[test]
    fn insert_operations() {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": "See "})).unwrap();
        s.run("insert.link", &json!({"url": "example.com", "text": "the site"})).unwrap();
        let c = &s.composes[&id];
        assert!(
            matches!(&c.body.blocks[0], Block::Paragraph { inlines, .. }
                if inlines.iter().any(|i| matches!(i, Inline::Link { href, children }
                    if href.ends_with("://example.com")
                        && matches!(&children[0], Inline::Text { text, .. } if text == "the site")))),
            "{:?}",
            c.body.blocks[0]
        );
        // Picture from bytes: embedded as a cid: inline image.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let r =
            s.run("insert.picture", &json!({"name": "dot.png", "data_base64": png, "alt": "dot"})).unwrap();
        let cid = r["cid"].as_str().unwrap().to_owned();
        let c = &s.composes[&id];
        assert_eq!(c.inline_images.len(), 1);
        assert_eq!(c.inline_images[0].mime_type, "image/png");
        assert!(c.images().iter().any(|(src, alt)| *src == format!("cid:{cid}") && alt == "dot"));
        assert!(
            s.run("insert.picture", &json!({"name": "x.bin", "data_base64": "AAAA"})).is_err(),
            "not a picture"
        );
        // Picture from a file path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.png");
        std::fs::write(&path, mailcraft_mime::transfer::decode_base64(png.as_bytes())).unwrap();
        s.run("insert.picture", &json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(s.composes[&id].inline_images.len(), 2);
        // Table 2x3, horizontal line, symbol, date & time.
        s.run("insert.table", &json!({"rows": 2, "cols": 3})).unwrap();
        assert!(s.composes[&id].cell_at_caret().is_some());
        s.run("compose.tableCell", &json!({"direction": "down"})).unwrap();
        let end = s.run("compose.select", &json!({"all": true})).unwrap()["focus"].clone();
        s.run("compose.select", &json!({"anchor": end})).unwrap();
        s.run("insert.horizontalLine", &json!({})).unwrap();
        assert!(s.composes[&id].body.blocks.iter().any(|b| matches!(b, Block::Rule)));
        s.run("insert.symbol", &json!({"char": "U+00A9"})).unwrap();
        s.run("insert.dateTime", &json!({"format": "yyyy-MM-dd"})).unwrap();
        let text = s.composes[&id].body.to_plain_text();
        assert!(text.contains("©2026-10-09"), "{text}");
        for _ in 0..6 {
            s.run("compose.editUndo", &json!({})).unwrap();
        }
        assert!(!s.composes[&id].body.blocks.iter().any(|b| matches!(b, Block::Table { .. })));
    }

    #[test]
    fn find_replace_word_count_and_paste() {
        let (mut s, id) = spanning();
        let r = s.run("format.find", &json!({"text": "TA"})).unwrap();
        assert_eq!(r["found"], true);
        assert_eq!(s.composes[&id].selected_text(), "ta", "beta (case-insensitive)");
        let r = s.run("format.find", &json!({"text": "TA", "matchCase": true})).unwrap();
        assert_eq!(r["found"], false);
        let r = s
            .run("format.replace", &json!({"find": "a", "replace": "A", "all": true, "matchCase": true}))
            .unwrap();
        assert_eq!(r["replaced"], 6);
        assert_eq!(s.composes[&id].body.to_plain_text(), "AlphA betA\ngAmmA\ndeltA epsilon");
        undo_redo_round_trip(&mut s, id);
        s.run("compose.select", &json!({"anchor": {"path": [0], "offset": 0}})).unwrap();
        let r = s.run("review.wordCount", &json!({})).unwrap();
        assert_eq!((r["words"].as_u64(), r["paragraphs"].as_u64()), (Some(5), Some(3)));
        // Paste HTML / plain text at the caret.
        s.run("compose.select", &json!({"anchor": {"path": [1], "offset": 5}})).unwrap();
        s.run("format.paste", &json!({"html": "<p> one</p><ul><li>two</li></ul><p>three </p>"})).unwrap();
        let t = s.composes[&id].body.to_plain_text();
        assert!(t.starts_with("AlphA betA\ngAmmAone\n"), "{t}");
        assert!(t.contains("two") && t.contains("three"), "{t}");
        s.run("compose.editUndo", &json!({})).unwrap();
        s.run("format.pasteText", &json!({"text": "X\nY"})).unwrap();
        assert_eq!(s.composes[&id].body.to_plain_text(), "AlphA betA\ngAmmAX\nY\ndeltA epsilon");
    }
}
