//! Borders and Shading: the Borders menu (each edge toggles), the Shading button and gallery, and the
//! Borders and Shading dialog (setting, line style / colour / width, the edges one by one, shading fill and
//! pattern). Paragraph borders are real borders on the paragraphs; in a table they are the table's edges.

use serde_json::{Map, Value, json};

use super::{compose_id, compose_mut, parse_color, requested_theme_color};
use crate::cmd::util::{dialog, opt_bool, opt_str};
use crate::compose::ComposeState;
use crate::{CmdError, CmdResult, Session};
use mailcraft_geom::Color;
use mailcraft_model::{BorderEdge, BorderLine, ParaBorders, TableBorders};

/// The Borders menu items, as `format.borders` takes them.
pub const BORDER_KINDS: [&str; 10] =
    ["bottom", "top", "left", "right", "none", "all", "outside", "inside", "insideH", "insideV"];

/// Shading patterns: name and how much of the pattern colour covers the fill (percent). Patterns are drawn
/// as the blend of the two colours (mail programs have no pattern fills).
pub const SHADING_PATTERNS: [(&str, u8); 15] = [
    ("clear", 0),
    ("5%", 5),
    ("10%", 10),
    ("12.5%", 13),
    ("15%", 15),
    ("20%", 20),
    ("25%", 25),
    ("30%", 30),
    ("40%", 40),
    ("50%", 50),
    ("60%", 60),
    ("70%", 70),
    ("75%", 75),
    ("80%", 80),
    ("solid", 100),
];

/// An edge as JSON: `{"style", "color" ("auto" = the text colour), "width" (points)}`; `null` for none.
pub fn edge_json(e: Option<&BorderEdge>) -> Value {
    match e {
        None => Value::Null,
        Some(e) => json!({
            "style": e.line.name(),
            "color": e.color.map_or_else(|| "auto".to_owned(), |c| c.to_hex()),
            "width": e.width_pt,
        }),
    }
}

/// An edge from JSON (`null`, `false` or `"none"` = no line; missing parts come from `pen`).
fn edge_from(v: &Value, pen: &BorderEdge) -> Result<Option<BorderEdge>, CmdError> {
    match v {
        Value::Null | Value::Bool(false) => return Ok(None),
        Value::Bool(true) => return Ok(Some(pen.clone())),
        Value::String(s) if s.eq_ignore_ascii_case("none") => return Ok(None),
        Value::Object(_) => {}
        _ => return Err(CmdError::Params("a border edge is an object, true or null".into())),
    }
    let line = match v.get("style").and_then(Value::as_str) {
        Some(n) => {
            BorderLine::from_name(n).ok_or_else(|| CmdError::Params(format!("unknown border style `{n}`")))?
        }
        None => pen.line,
    };
    let color = match v.get("color").and_then(Value::as_str) {
        Some(c) if c.eq_ignore_ascii_case("auto") || c.eq_ignore_ascii_case("automatic") => None,
        Some(c) => Some(parse_color(c)?),
        None => pen.color,
    };
    let width = match v.get("width") {
        Some(w) => {
            let w = w
                .as_f64()
                .filter(|w| w.is_finite())
                .ok_or_else(|| CmdError::Params("`width` (points) must be a number".into()))?;
            #[allow(clippy::cast_possible_truncation)]
            let w = w as f32;
            w
        }
        None => pen.width_pt,
    };
    Ok(Some(BorderEdge::new(line, color, width)))
}

/// The line a Borders call draws: `style` / `color` / `width` params over the last line chosen (a thin
/// single line in the text colour at first).
fn pen(c: &ComposeState, p: &Value) -> Result<BorderEdge, CmdError> {
    let last = c.last_border.clone().unwrap_or_default();
    let mut spec = Map::new();
    for k in ["style", "color", "width"] {
        if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
            spec.insert(k.into(), v.clone());
        }
    }
    Ok(edge_from(&Value::Object(spec), &last)?.unwrap_or(last))
}

pub fn para_borders_json(b: &ParaBorders) -> Value {
    json!({
        "top": edge_json(b.top.as_ref()),
        "bottom": edge_json(b.bottom.as_ref()),
        "left": edge_json(b.left.as_ref()),
        "right": edge_json(b.right.as_ref()),
        "insideH": edge_json(b.between.as_ref()),
        "shadow": b.shadow,
    })
}

pub fn table_borders_json(b: &TableBorders) -> Value {
    json!({
        "top": edge_json(b.top.as_ref()),
        "bottom": edge_json(b.bottom.as_ref()),
        "left": edge_json(b.left.as_ref()),
        "right": edge_json(b.right.as_ref()),
        "insideH": edge_json(b.inside_h.as_ref()),
        "insideV": edge_json(b.inside_v.as_ref()),
    })
}

/// The Setting a set of paragraph borders matches (None, Box, Shadow or Custom).
fn para_setting(b: &ParaBorders) -> &'static str {
    let sides = [&b.top, &b.bottom, &b.left, &b.right];
    if b.is_empty() {
        return "none";
    }
    let first = b.top.as_ref();
    let same = sides.iter().all(|s| s.is_some() && s.as_ref() == first) && b.between.is_none();
    match (same, b.shadow) {
        (true, false) => "box",
        (true, true) => "shadow",
        _ => "custom",
    }
}

/// The Setting a set of table borders matches (None, Box, All, Grid or Custom).
fn table_setting(b: &TableBorders) -> &'static str {
    if b.is_empty() {
        return "none";
    }
    let outer = [&b.top, &b.bottom, &b.left, &b.right];
    let first = b.top.as_ref();
    let box_same = outer.iter().all(|s| s.is_some() && s.as_ref() == first);
    match (box_same, b.inside_h.as_ref(), b.inside_v.as_ref()) {
        (true, None, None) => "box",
        (true, Some(h), Some(v)) if Some(h) == first && Some(v) == first => "all",
        (true, Some(h), Some(v)) if h == v => "grid",
        _ => "custom",
    }
}

/// What the dialog opens with: the paragraph's or table's borders, the line last chosen and the shading.
fn dialog_state(c: &ComposeState, tab: &str) -> Value {
    let pen = c.last_border.clone().unwrap_or_default();
    let styles: Vec<&str> = BorderLine::ALL.iter().map(|(_, n, _)| *n).collect();
    let patterns: Vec<&str> = SHADING_PATTERNS.iter().map(|(n, _)| *n).collect();
    let (target, borders, setting) = match c.table_borders_at_caret() {
        Some(t) => ("table", table_borders_json(&t), table_setting(&t)),
        None => {
            let b = c.para_borders_at_caret();
            ("paragraph", para_borders_json(&b), para_setting(&b))
        }
    };
    let fill = match c.cell_at_caret() {
        Some(_) => c.cell_background_at_caret(),
        None => c.para_at_caret().background,
    };
    json!({
        "compose": c.id,
        "tab": tab,
        "target": target,
        "paragraphs": c.selected_leaves().len().max(1),
        "borders": borders,
        "setting": setting,
        "pen": edge_json(Some(&pen)),
        "shading": {"fill": fill.map(|f| f.to_hex())},
        "styles": styles,
        "widths": BorderEdge::WIDTHS_PT,
        "patterns": patterns,
    })
}

/// Turns each named edge on with `pen`, or off when all of them are already on (the Borders menu items
/// toggle).
fn toggle_edges(slots: &mut [&mut Option<BorderEdge>], pen: &BorderEdge) {
    let all_on = slots.iter().all(|s| s.is_some());
    for s in slots.iter_mut() {
        **s = if all_on { None } else { Some(pen.clone()) };
    }
}

pub(super) fn borders(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(a) = p.get("apply") {
        return apply(s, p, a);
    }
    let kind = opt_str(p, "kind");
    if opt_bool(p, "dialog") == Some(true) || kind.is_none() {
        let cid = compose_id(s, p)?;
        let tab = opt_str(p, "tab").filter(|t| *t == "shading").unwrap_or("borders");
        let state = s.composes.get(&cid).map(|c| dialog_state(c, tab)).unwrap_or(Value::Null);
        return dialog(s, "bordersAndShading", state);
    }
    let kind = kind.unwrap_or("none");
    if !BORDER_KINDS.contains(&kind) {
        return Err(CmdError::Params(format!("unknown border `{kind}`")));
    }
    let c = compose_mut(s, p)?;
    let pen = pen(c, p)?;
    c.checkpoint();
    if c.table_at_caret().is_some() {
        let changed = c.edit_table_borders(|t| match kind {
            "none" => *t = TableBorders::default(),
            "all" => toggle_edges(
                &mut [&mut t.top, &mut t.bottom, &mut t.left, &mut t.right, &mut t.inside_h, &mut t.inside_v],
                &pen,
            ),
            "outside" => toggle_edges(&mut [&mut t.top, &mut t.bottom, &mut t.left, &mut t.right], &pen),
            "inside" => toggle_edges(&mut [&mut t.inside_h, &mut t.inside_v], &pen),
            one => {
                if let Some(slot) = t.edge_mut(one) {
                    toggle_edges(&mut [slot], &pen);
                }
            }
        });
        c.last_border = Some(pen);
        let now = c.table_borders_at_caret().unwrap_or_default();
        return Ok(
            json!({"target": "table", "kind": kind, "changed": changed, "borders": table_borders_json(&now)}),
        );
    }
    if kind == "insideV" {
        return Err(CmdError::Failed("Inside Vertical Border applies to tables".into()));
    }
    // Toggle state from the paragraph at the caret, applied to every selected paragraph.
    let cur = c.para_borders_at_caret();
    let names: &[&str] = match kind {
        "none" => &[],
        "all" => &["top", "bottom", "left", "right", "between"],
        "outside" => &["top", "bottom", "left", "right"],
        "inside" | "insideH" => &["between"],
        one => match one {
            "top" => &["top"],
            "bottom" => &["bottom"],
            "left" => &["left"],
            _ => &["right"],
        },
    };
    let side_on = |n: &str| match n {
        "top" => cur.top.is_some(),
        "bottom" => cur.bottom.is_some(),
        "left" => cur.left.is_some(),
        "right" => cur.right.is_some(),
        _ => cur.between.is_some(),
    };
    let all_on = !names.is_empty() && names.iter().all(|n| side_on(n));
    let changed = c.edit_para_borders(|b| {
        if names.is_empty() {
            *b = ParaBorders::default();
            return;
        }
        for n in names {
            if let Some(slot) = b.side_mut(n) {
                *slot = if all_on { None } else { Some(pen.clone()) };
            }
        }
    });
    c.last_border = Some(pen);
    let now = c.para_borders_at_caret();
    Ok(json!({"target": "paragraph", "kind": kind, "changed": changed, "borders": para_borders_json(&now)}))
}

/// The dialog's OK: `{"apply": {"setting"?, "pen"?, "top"?, "bottom"?, "left"?, "right"?, "insideH"?,
/// "insideV"?, "shadow"?}}` replaces the borders of the selected paragraphs (or of the table at the caret).
/// A setting (`none`, `box`, `shadow`, `all`, `grid`) draws its edges with the pen; edges given one by one
/// win over it.
fn apply(s: &mut Session, p: &Value, a: &Value) -> CmdResult {
    if !a.is_object() {
        return Err(CmdError::Params("`apply` must be an object".into()));
    }
    let c = compose_mut(s, p)?;
    let last = c.last_border.clone().unwrap_or_default();
    let pen = match a.get("pen") {
        Some(v) => edge_from(v, &last)?.unwrap_or(last),
        None => last,
    };
    let setting = a.get("setting").and_then(Value::as_str).unwrap_or("custom");
    if !matches!(setting, "none" | "box" | "shadow" | "all" | "grid" | "custom") {
        return Err(CmdError::Params(format!("unknown border setting `{setting}`")));
    }
    let edge = |k: &str, default: Option<BorderEdge>| -> Result<Option<BorderEdge>, CmdError> {
        match a.get(k) {
            Some(v) => edge_from(v, &pen),
            None => Ok(default),
        }
    };
    let on = |yes: bool| yes.then(|| pen.clone());
    let boxed = matches!(setting, "box" | "shadow" | "all" | "grid");
    let inside = matches!(setting, "all" | "grid");
    let in_table =
        c.table_at_caret().is_some() && a.get("target").and_then(Value::as_str) != Some("paragraph");
    c.checkpoint();
    if in_table {
        let custom = setting == "custom";
        let cur = c.table_borders_at_caret().unwrap_or_default();
        let keep = |e: &Option<BorderEdge>, yes: bool| if custom { e.clone() } else { on(yes) };
        let mut grid_inside = on(inside);
        if setting == "grid"
            && let Some(e) = grid_inside.as_mut()
        {
            // Grid: a heavier outline round thin inside lines.
            e.width_pt = BorderEdge::DEFAULT_WIDTH_PT;
        }
        let t = TableBorders {
            top: edge("top", keep(&cur.top, boxed))?,
            bottom: edge("bottom", keep(&cur.bottom, boxed))?,
            left: edge("left", keep(&cur.left, boxed))?,
            right: edge("right", keep(&cur.right, boxed))?,
            inside_h: edge("insideH", if custom { cur.inside_h.clone() } else { grid_inside.clone() })?,
            inside_v: edge("insideV", if custom { cur.inside_v.clone() } else { grid_inside })?,
        };
        c.edit_table_borders(|b| *b = t);
        c.last_border = Some(pen);
        let now = c.table_borders_at_caret().unwrap_or_default();
        return Ok(json!({"target": "table", "borders": table_borders_json(&now)}));
    }
    let custom = setting == "custom";
    let cur = c.para_borders_at_caret();
    let keep = |e: &Option<BorderEdge>, yes: bool| if custom { e.clone() } else { on(yes) };
    let b = ParaBorders {
        top: edge("top", keep(&cur.top, boxed))?,
        bottom: edge("bottom", keep(&cur.bottom, boxed))?,
        left: edge("left", keep(&cur.left, boxed))?,
        right: edge("right", keep(&cur.right, boxed))?,
        between: edge("insideH", if custom { cur.between.clone() } else { None })?,
        shadow: a.get("shadow").and_then(Value::as_bool).unwrap_or(if custom {
            cur.shadow
        } else {
            setting == "shadow"
        }),
    };
    c.edit_para_borders(|x| *x = b.clone());
    c.last_border = Some(pen);
    Ok(json!({"target": "paragraph", "borders": para_borders_json(&c.para_borders_at_caret())}))
}

/// `color` blended with a shading `pattern` of `pattern_color` (the pattern colour covering that share of
/// the fill; no fill = white).
pub fn shade(fill: Option<Color>, pattern: &str, pattern_color: Color) -> Result<Option<Color>, CmdError> {
    let pct = SHADING_PATTERNS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(pattern.trim()))
        .map(|(_, p)| *p)
        .ok_or_else(|| CmdError::Params(format!("unknown shading pattern `{pattern}`")))?;
    if pct == 0 {
        return Ok(fill);
    }
    Ok(Some(fill.unwrap_or(Color::WHITE).lerp(pattern_color, f32::from(pct) / 100.0)))
}

pub(super) fn shading(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "custom") == Some(true) {
        return dialog(s, "colors", json!({"compose": compose_id(s, p)?, "target": "shading"}));
    }
    // High-contrast only: the Shading palette offers only colours text stays readable on.
    if let Some(v) = opt_bool(p, "highContrastOnly") {
        let c = compose_mut(s, p)?;
        c.editor.shading_high_contrast_only = v;
        if p.get("color").is_none() && p.get("theme").is_none() {
            return Ok(json!({"highContrastOnly": v}));
        }
    }
    if opt_bool(p, "dialog") == Some(true) {
        let cid = compose_id(s, p)?;
        let state = s.composes.get(&cid).map(|c| dialog_state(c, "shading")).unwrap_or(Value::Null);
        return dialog(s, "bordersAndShading", state);
    }
    let themed = requested_theme_color(s, p)?;
    let last = s.composes.get(&compose_id(s, p)?).map(|c| c.last_shading.unwrap_or(super::DEFAULT_SHADING));
    let pattern = opt_str(p, "pattern");
    let mut col = match p.get("color") {
        _ if themed.is_some() => themed,
        // The button face shades with the colour chosen last (light grey before one is chosen).
        None => last.filter(|_| pattern.is_none()),
        Some(Value::Null) => None,
        Some(v) => {
            let t = v
                .as_str()
                .ok_or_else(|| CmdError::Params("`color` must be a colour string or null".into()))?;
            if t.eq_ignore_ascii_case("none") { None } else { Some(parse_color(t)?) }
        }
    };
    if let Some(pat) = pattern {
        let pc = match opt_str(p, "patternColor") {
            Some(t) if t.eq_ignore_ascii_case("auto") || t.eq_ignore_ascii_case("automatic") => Color::BLACK,
            Some(t) => parse_color(t)?,
            None => Color::BLACK,
        };
        col = shade(col, pat, pc)?;
    }
    let c = compose_mut(s, p)?;
    if col.is_some() && pattern.is_none() {
        c.last_shading = col;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::compose::tests::{open, session};
    use crate::cmd::insert::tests::last_dialog;
    use mailcraft_model::Block;

    /// Runs `id` with `p` and returns the params of the dialog it opened.
    fn opened(s: &mut Session, id: &str, p: Value) -> Value {
        assert!(s.run(id, &p).unwrap().get("dialog").is_some());
        last_dialog(s)
    }

    fn with_text(text: &str) -> (Session, u64) {
        let mut s = session();
        let id = open(&mut s);
        s.run("compose.insertText", &json!({"text": text})).unwrap();
        (s, id)
    }

    fn select_all(s: &mut Session) {
        s.run("compose.select", &json!({"all": true})).unwrap();
    }

    fn para_borders(s: &Session, id: u64, i: usize) -> Option<ParaBorders> {
        match &s.composes[&id].body.blocks[i] {
            Block::Paragraph { style, .. } | Block::Heading { style, .. } => {
                style.borders.as_deref().cloned()
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn menu_items_toggle_paragraph_edges_with_the_pen() {
        let (mut s, id) = with_text("one\ntwo\nthree");
        select_all(&mut s);
        let r = s
            .run(
                "format.borders",
                &json!({"kind": "outside", "style": "dashed", "color": "#c00000", "width": 1.5}),
            )
            .unwrap();
        assert_eq!(r["target"], "paragraph");
        assert_eq!(r["borders"]["top"]["style"], "dashed");
        assert_eq!(r["borders"]["left"]["width"], 1.5);
        // Every selected paragraph got the same box (so they share one).
        for i in 0..3 {
            let b = para_borders(&s, id, i).unwrap();
            assert_eq!(b.top.as_ref().map(|e| e.line), Some(BorderLine::Dashed));
            assert_eq!(b.right.as_ref().and_then(|e| e.color), Some(Color::rgb(0xc0, 0, 0)));
        }
        // The pen is remembered: Inside Horizontal draws with it, between the paragraphs.
        s.run("format.borders", &json!({"kind": "insideH"})).unwrap();
        assert_eq!(para_borders(&s, id, 1).unwrap().between.map(|e| e.width_pt), Some(1.5));
        // Clicking Outside Borders again takes the box off, keeping the between line.
        s.run("format.borders", &json!({"kind": "outside"})).unwrap();
        let b = para_borders(&s, id, 0).unwrap();
        assert!(b.top.is_none() && b.left.is_none() && b.between.is_some());
        s.run("format.borders", &json!({"kind": "none"})).unwrap();
        assert!(para_borders(&s, id, 0).is_none());
        // Each single edge toggles.
        s.run("format.borders", &json!({"kind": "right"})).unwrap();
        assert!(para_borders(&s, id, 2).unwrap().right.is_some());
        s.run("format.borders", &json!({"kind": "right"})).unwrap();
        assert!(para_borders(&s, id, 2).is_none());
        // Undo puts the borders back one step at a time.
        s.run("compose.editUndo", &json!({})).unwrap();
        assert!(para_borders(&s, id, 2).unwrap().right.is_some());
        assert!(s.run("format.borders", &json!({"kind": "left", "style": "wavy"})).is_err());
        assert!(s.run("format.borders", &json!({"kind": "diagonal"})).is_err());
    }

    #[test]
    fn table_edges_toggle_on_the_table() {
        let (mut s, id) = with_text("x");
        s.run("insert.table", &json!({"rows": 2, "cols": 2})).unwrap();
        let table = |s: &Session| {
            s.composes[&id]
                .body
                .blocks
                .iter()
                .find_map(|b| match b {
                    Block::Table { border, borders, .. } => Some((*border, borders.as_deref().cloned())),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(table(&s), (true, None));
        // Taking the inside lines off the plain grid leaves its outline as per-edge borders.
        let r = s.run("format.borders", &json!({"kind": "inside"})).unwrap();
        assert_eq!(r["target"], "table");
        let (border, b) = table(&s);
        let b = b.unwrap();
        assert!(!border && b.inside_h.is_none() && b.inside_v.is_none() && b.top.is_some());
        // Putting them back makes it the plain grid again.
        s.run(
            "format.borders",
            &json!({"kind": "inside", "style": "single", "color": "#808080", "width": 0.75}),
        )
        .unwrap();
        assert_eq!(table(&s), (true, None));
        s.run("format.borders", &json!({"kind": "none"})).unwrap();
        assert_eq!(table(&s), (false, None));
        s.run("format.borders", &json!({"kind": "insideV", "style": "double"})).unwrap();
        assert_eq!(table(&s).1.unwrap().inside_v.map(|e| e.line), Some(BorderLine::Double));
    }

    #[test]
    fn dialog_opens_with_the_state_and_applies_a_setting() {
        let (mut s, id) = with_text("boxed");
        let r = opened(&mut s, "format.borders", json!({}));
        assert_eq!(r["tab"], "borders");
        assert_eq!(r["target"], "paragraph");
        assert_eq!(r["setting"], "none");
        assert_eq!(r["styles"].as_array().unwrap().len(), BorderLine::ALL.len());
        let pen = json!({"style": "double", "color": "auto", "width": 0.75});
        s.run("format.borders", &json!({"apply": {"setting": "shadow", "pen": pen}})).unwrap();
        let b = para_borders(&s, id, 0).unwrap();
        assert!(
            b.shadow && b.left.as_ref().is_some_and(|e| e.line == BorderLine::Double && e.color.is_none())
        );
        assert_eq!(opened(&mut s, "format.borders", json!({"dialog": true}))["setting"], "shadow");
        // Custom: the preview's edge clicks (here: no top, a dotted bottom).
        s.run(
            "format.borders",
            &json!({"apply": {"setting": "custom", "top": null, "bottom": {"style": "dotted"}, "shadow": false}}),
        )
        .unwrap();
        let b = para_borders(&s, id, 0).unwrap();
        assert!(b.top.is_none() && !b.shadow);
        assert_eq!(b.bottom.as_ref().map(|e| e.line), Some(BorderLine::Dotted));
        assert_eq!(b.left.as_ref().map(|e| e.line), Some(BorderLine::Double), "untouched edges stay");
        assert_eq!(opened(&mut s, "format.borders", json!({"dialog": true}))["setting"], "custom");
        s.run("format.borders", &json!({"apply": {"setting": "none"}})).unwrap();
        assert!(para_borders(&s, id, 0).is_none());
        // In a table the dialog edits the table: All draws every edge.
        s.run("insert.table", &json!({"rows": 2, "cols": 2})).unwrap();
        assert_eq!(opened(&mut s, "format.borders", json!({}))["target"], "table");
        s.run("format.borders", &json!({"apply": {"setting": "all", "pen": {"style": "single", "color": "#000000", "width": 2.25}}}))
            .unwrap();
        let t = s.composes[&id].table_borders_at_caret().unwrap();
        assert_eq!(t, TableBorders::all(&BorderEdge::new(BorderLine::Single, Some(Color::BLACK), 2.25)));
        assert!(s.run("format.borders", &json!({"apply": {"setting": "zigzag"}})).is_err());
        assert!(s.run("format.borders", &json!({"apply": {"top": 5}})).is_err());
        assert!(s.run("format.borders", &json!({"apply": "box"})).is_err());
    }

    #[test]
    fn shading_patterns_blend_and_the_button_applies_its_face_colour() {
        let (mut s, id) = with_text("shaded");
        // The face applies black before any colour was picked, as observed (it used to open the dialog).
        let r = s.run("format.shading", &json!({})).unwrap();
        assert_eq!(r["shading"], "#000000");
        let r = opened(&mut s, "format.shading", json!({"dialog": true}));
        assert_eq!(r["tab"], "shading");
        s.run("format.shading", &json!({"color": null})).unwrap();
        s.composes.get_mut(&id).unwrap().last_shading = None;
        // High-contrast only is a switch of the Shading menu.
        assert_eq!(
            s.run("format.shading", &json!({"highContrastOnly": true})).unwrap()["highContrastOnly"],
            true
        );
        assert!(s.composes[&id].editor.shading_high_contrast_only);
        let r = s
            .run("format.shading", &json!({"color": "#ffffff", "pattern": "25%", "patternColor": "#000000"}))
            .unwrap();
        assert_eq!(r["shading"], "#bfbfbf");
        assert_eq!(s.composes[&id].para_at_caret().background, Some(Color::rgb(0xbf, 0xbf, 0xbf)));
        // A pattern does not become the button colour.
        assert_eq!(s.composes[&id].last_shading, None);
        assert_eq!(shade(None, "clear", Color::BLACK).unwrap(), None);
        assert_eq!(
            shade(Some(Color::WHITE), "solid", Color::rgb(1, 2, 3)).unwrap(),
            Some(Color::rgb(1, 2, 3))
        );
        assert!(s.run("format.shading", &json!({"pattern": "plaid"})).is_err());
    }

    #[test]
    fn junk_border_params_never_panic() {
        let (mut s, _) = with_text("x");
        for p in [
            json!({"kind": "top", "width": "wide"}),
            json!({"kind": "top", "width": 1e30}),
            json!({"kind": "top", "color": 7}),
            json!({"apply": {"pen": {"width": -3}, "setting": "box"}}),
            json!({"apply": {"left": {"style": null, "color": "auto", "width": f64::MAX}}}),
            json!({"apply": {"insideV": true}}),
        ] {
            let _ = s.run("format.borders", &p);
        }
        let b = s.composes.values().next().unwrap().para_borders_at_caret();
        for e in [&b.top, &b.bottom, &b.left, &b.right].into_iter().flatten() {
            assert!(e.width_pt.is_finite() && e.width_pt <= BorderEdge::MAX_WIDTH_PT && e.width_pt > 0.0);
        }
    }
}
