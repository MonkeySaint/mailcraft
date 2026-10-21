//! `drawing.*`: the floating drawing objects of the compose body (text boxes, shapes, WordArt, drawing
//! canvases; see [`mailcraft_model::Drawing`]) and the Shape Format tab that edits them: select, move and
//! size (the editor's drag handles end in `drawing.move` / `drawing.size`), text, Shape Fill, Shape Outline,
//! Shape Effects, Shape Quick Styles, gradient fills, Text Fill, Text Outline, Text Effects, WordArt Quick
//! Styles, Text Direction, Align Text, Change Shape, Bring Forward / Send Backward,
//! Align, Rotate, Alt Text, Size and Position, Delete, and the drawing tool (the next drag in the body draws
//! the armed shape, text box, canvas or scribble).
//!
//! Every command acts on `{"id": n}` or, without it, on the compose's selected drawing; each change is one
//! editor undo step. Moving a drawing canvas moves the drawings drawn in it (`Drawing::canvas`).

use serde_json::{Value, json};

use super::compose::{compose_id, compose_mut, spec};
use super::insert::drawing::{self as draw, EFFECTS};
use super::insert::shapes;
use crate::cmd::util::{dialog, opt_bool, opt_str};
use crate::compose::ComposeState;
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_geom::Color;
use mailcraft_model::rich::{Align, Drawing, DrawingKind, VAlign};

/// Enabled: an open compose has drawings.
fn has_drawings(s: &Session) -> Option<&'static str> {
    if s.composes.is_empty() {
        Some("no message is being composed")
    } else if s.composes.values().all(|c| c.body.drawings.is_empty()) {
        Some("insert a shape, text box or WordArt first")
    } else {
        None
    }
}

/// Enabled: a compose is open (selecting nothing and disarming always work).
fn any_compose(s: &Session) -> Option<&'static str> {
    if s.composes.is_empty() { Some("no message is being composed") } else { None }
}

/// The drawing a command acts on: `id`, else the selected one.
fn target(s: &Session, p: &Value) -> Result<(u64, u32), CmdError> {
    let cid = compose_id(s, p)?;
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let id = match p.get("id") {
        Some(v) if !v.is_null() => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| CmdError::Params("`id` must be a drawing id".into()))?,
        _ => c.selected_drawing.ok_or_else(|| CmdError::Disabled("select a drawing first".into()))?,
    };
    if !c.body.drawings.iter().any(|d| d.id == id) {
        return Err(CmdError::Params(format!("no drawing with id {id}")));
    }
    Ok((cid, id))
}

/// Applies `f` to the target drawing as one undo step (no step when nothing changed).
fn change(
    s: &mut Session,
    p: &Value,
    f: impl FnOnce(&mut Drawing, &ComposeState) -> Result<(), CmdError>,
) -> CmdResult {
    let (cid, id) = target(s, p)?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let Some(before) = c.body.drawings.iter().find(|d| d.id == id).cloned() else {
        return Err(CmdError::Params(format!("no drawing with id {id}")));
    };
    let mut d = before.clone();
    f(&mut d, c)?;
    draw::sanitize(&mut d);
    if d.kind == DrawingKind::WordArt {
        draw::fit_word_art(&mut d, &c.theme_resolved());
    }
    let changed = d != before;
    if changed {
        c.checkpoint();
        if let Some(slot) = c.body.drawings.iter_mut().find(|x| x.id == id) {
            *slot = d.clone();
        }
        rejoin(c, id);
        c.dirty = true;
    }
    c.selected_drawing = Some(id);
    Ok(json!({"drawing": id, "changed": changed, "x": d.x, "y": d.y, "w": d.w, "h": d.h}))
}

/// After drawing `id` (not a canvas) moved or changed size: it belongs to the canvas that now holds it, if
/// any (moved out of its canvas, it leaves it; moved into one, it joins it).
fn rejoin(c: &mut ComposeState, id: u32) {
    let Some(d) = c.body.drawings.iter().find(|d| d.id == id) else { return };
    let canvas = draw::canvas_around(&c.body.drawings, d);
    if let Some(d) = c.body.drawings.iter_mut().find(|d| d.id == id) {
        d.canvas = canvas;
    }
}

fn num(p: &Value, key: &str) -> Result<Option<f32>, CmdError> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v
                .as_f64()
                .or_else(|| v.as_str().and_then(|t| t.trim().trim_end_matches("px").trim().parse().ok()))
                .filter(|n| n.is_finite())
                .ok_or_else(|| CmdError::Params(format!("`{key}` must be a number")))?;
            Ok(Some(n as f32))
        }
    }
}

/// The colour a fill / outline / text colour call names: `{"color": "#rrggbb" | name | null}` (null = none)
/// or `{"theme": key, "tint": n}`. `Ok(None)` when neither is given.
fn color_choice(s: &Session, p: &Value) -> Result<Option<Option<Color>>, CmdError> {
    if let Some(c) = crate::cmd::format::requested_theme_color(s, p)? {
        return Ok(Some(Some(c)));
    }
    match p.get("color") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(t)) if t.eq_ignore_ascii_case("none") => Ok(Some(None)),
        Some(Value::String(t)) => mailcraft_html::parse_css_color(t)
            .map(|c| Some(Some(c)))
            .ok_or_else(|| CmdError::Params(format!("`{t}` is not a colour"))),
        Some(_) => Err(CmdError::Params("`color` must be a colour or null".into())),
    }
}

/// The colour picker for `target` (`shapeFill`, `shapeOutline`, `textFill`) with the current colour.
fn color_dialog(s: &mut Session, p: &Value, target_name: &str, current: Option<Color>) -> CmdResult {
    let cid = compose_id(s, p)?;
    dialog(
        s,
        "colors",
        json!({"compose": cid, "target": target_name, "current": current.map(|c| c.to_hex())}),
    )
}

fn select(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let id = match p.get("id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| CmdError::Params("`id` must be a drawing id or null".into()))?,
        ),
    };
    let c = compose_mut(s, p)?;
    if let Some(id) = id
        && !c.body.drawings.iter().any(|d| d.id == id)
    {
        return Err(CmdError::Params(format!("no drawing with id {id}")));
    }
    c.selected_drawing = id;
    Ok(json!({"compose": cid, "selected": id}))
}

/// Names a drawing tool can have.
fn valid_tool(t: &str) -> bool {
    match t.strip_prefix("shape:") {
        Some(shape) => shapes::canonical(shape).is_some_and(|c| c == shape),
        None => matches!(
            t,
            "textBox"
                | "verticalTextBox"
                | "canvas"
                | "scribble"
                | "freeform"
                | "tablePencil"
                | "tableEraser"
        ),
    }
}

fn tool(s: &mut Session, p: &Value) -> CmdResult {
    let tool = match p.get("tool") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) if valid_tool(t) => Some(t.clone()),
        Some(other) => return Err(CmdError::Params(format!("unknown drawing tool {other}"))),
    };
    let c = compose_mut(s, p)?;
    if tool.is_some() {
        c.selected_drawing = None;
    }
    c.draw_tool.clone_from(&tool);
    Ok(json!({"armed": tool}))
}

fn list(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let drawings = serde_json::to_value(&c.body.drawings).unwrap_or(Value::Null);
    Ok(json!({"compose": cid, "drawings": drawings, "selected": c.selected_drawing, "tool": c.draw_tool}))
}

fn move_to(s: &mut Session, p: &Value) -> CmdResult {
    let (x, y, dx, dy) = (num(p, "x")?, num(p, "y")?, num(p, "dx")?, num(p, "dy")?);
    if x.is_none() && y.is_none() && dx.is_none() && dy.is_none() {
        return Err(CmdError::Params("give `x` / `y` or `dx` / `dy`".into()));
    }
    let (cid, id) = target(s, p)?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let Some(d) = c.body.drawings.iter().find(|d| d.id == id).cloned() else {
        return Err(CmdError::Params(format!("no drawing with id {id}")));
    };
    let nx = x.unwrap_or(d.x) + dx.unwrap_or(0.0);
    let ny = y.unwrap_or(d.y) + dy.unwrap_or(0.0);
    let mut moved = d.clone();
    moved.x = nx;
    moved.y = ny;
    draw::sanitize(&mut moved);
    let (ox, oy) = (moved.x - d.x, moved.y - d.y);
    if ox == 0.0 && oy == 0.0 {
        c.selected_drawing = Some(id);
        return Ok(json!({"drawing": id, "changed": false, "x": d.x, "y": d.y}));
    }
    // A canvas carries the drawings drawn in it.
    let members: Vec<u32> = if d.kind == DrawingKind::Canvas {
        c.body.drawings.iter().filter(|o| o.id != id && o.canvas == Some(id)).map(|o| o.id).collect()
    } else {
        Vec::new()
    };
    c.checkpoint();
    for o in &mut c.body.drawings {
        if o.id == id || members.contains(&o.id) {
            o.x += ox;
            o.y += oy;
            draw::sanitize(o);
        }
    }
    rejoin(c, id);
    c.selected_drawing = Some(id);
    c.dirty = true;
    Ok(json!({"drawing": id, "changed": true, "x": moved.x, "y": moved.y, "moved": members.len() + 1}))
}

fn size(s: &mut Session, p: &Value) -> CmdResult {
    let (w, h, x, y) = (num(p, "w")?, num(p, "h")?, num(p, "x")?, num(p, "y")?);
    if w.is_none() && h.is_none() && x.is_none() && y.is_none() {
        return Err(CmdError::Params("give `w` and / or `h` (and optionally `x`, `y`)".into()));
    }
    let keep = opt_bool(p, "keepRatio").unwrap_or(false);
    change(s, p, |d, _| {
        let (ow, oh) = (d.w.max(1.0), d.h.max(1.0));
        let (mut nw, mut nh) = (w.unwrap_or(d.w), h.unwrap_or(d.h));
        if keep {
            match (w, h) {
                (Some(_), None) => nh = oh * nw / ow,
                (None, Some(_)) => nw = ow * nh / oh,
                _ => {}
            }
        }
        if d.kind == DrawingKind::WordArt && (h.is_some() || w.is_some()) {
            // WordArt is as big as its letters: a new size is a new font size.
            let k = if h.is_some() { nh / oh } else { nw / ow };
            let pt = d.text_style.size_pt.unwrap_or(36.0);
            d.text_style.size_pt = Some((pt * k).clamp(4.0, 400.0));
        }
        d.w = nw;
        d.h = nh;
        if let Some(x) = x {
            d.x = x;
        }
        if let Some(y) = y {
            d.y = y;
        }
        Ok(())
    })
}

fn text(s: &mut Session, p: &Value) -> CmdResult {
    let t = p
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| CmdError::Params("`text` (string) required".into()))?
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    change(s, p, |d, _| {
        if d.kind == DrawingKind::Canvas {
            return Err(CmdError::Failed("a drawing canvas holds drawings, not text".into()));
        }
        if d.kind == DrawingKind::WordArt && d.alt == d.text {
            d.alt = t.clone();
        }
        d.text = t;
        Ok(())
    })
}

fn current(s: &Session, p: &Value) -> Result<Drawing, CmdError> {
    let (cid, id) = target(s, p)?;
    s.composes
        .get(&cid)
        .and_then(|c| c.body.drawings.iter().find(|d| d.id == id))
        .cloned()
        .ok_or_else(|| CmdError::Params(format!("no drawing with id {id}")))
}

fn fill(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "custom") == Some(true) {
        let cur = current(s, p)?.fill;
        return color_dialog(s, p, "shapeFill", cur);
    }
    let gradient = match p.get("gradient") {
        None | Some(Value::Null) => None,
        Some(Value::String(g)) if draw::GRADIENTS.contains(&g.as_str()) => Some(g.clone()),
        Some(other) => {
            return Err(CmdError::Params(format!(
                "unknown gradient {other} (down, up, right, left, center, or \"\" for a solid fill)"
            )));
        }
    };
    let col = color_choice(s, p)?;
    if col.is_none() && gradient.is_none() {
        return Err(CmdError::Params(
            "give `color` (null for No Fill), `theme` and `tint`, or `gradient`".into(),
        ));
    }
    change(s, p, |d, c| {
        if d.kind == DrawingKind::WordArt && gradient.as_deref().is_some_and(|g| !g.is_empty()) {
            return Err(CmdError::Failed("WordArt takes its gradient from its style".into()));
        }
        if let Some(col) = col {
            d.fill = col;
            if col.is_none() {
                d.gradient.clear();
            }
        }
        if let Some(g) = gradient {
            if !g.is_empty() && d.fill.is_none() {
                // A gradient needs a colour to shade: the theme's first accent.
                d.fill = Some(c.theme_resolved().color(crate::compose::ACCENT1));
            }
            d.gradient = g;
        }
        Ok(())
    })
}

/// Text Outline: a colour (null = none) drawn around the letters of the text.
fn text_outline(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "custom") == Some(true) {
        let cur = current(s, p)?.text_outline;
        return color_dialog(s, p, "textOutline", cur);
    }
    let Some(col) = color_choice(s, p)? else {
        return Err(CmdError::Params("give `color` (null for No Outline) or `theme` and `tint`".into()));
    };
    change(s, p, |d, _| {
        if d.kind == DrawingKind::Canvas {
            return Err(CmdError::Failed("a drawing canvas has no text".into()));
        }
        d.text_outline = col;
        Ok(())
    })
}

/// Text Effects: one effect on the letters (`""` gives WordArt back its style's effects).
fn text_effect(s: &mut Session, p: &Value) -> CmdResult {
    let e = opt_str(p, "effect").unwrap_or("").to_owned();
    if !draw::TEXT_EFFECTS.contains(&e.as_str()) {
        return Err(CmdError::Params(format!(
            "unknown text effect `{e}` (none, shadow, glow, reflection, bevel, or \"\" for the WordArt style's)"
        )));
    }
    change(s, p, |d, _| {
        match d.kind {
            DrawingKind::Canvas => return Err(CmdError::Failed("a drawing canvas has no text".into())),
            DrawingKind::WordArt => {}
            _ if e == "reflection" => {
                return Err(CmdError::Failed("only WordArt text can have a reflection".into()));
            }
            _ => {}
        }
        d.text_effect = e;
        Ok(())
    })
}

/// Text Direction: the text of a text box or shape runs left to right, or top to bottom.
fn text_direction(s: &mut Session, p: &Value) -> CmdResult {
    let vertical = match p.get("vertical").and_then(Value::as_bool) {
        Some(v) => v,
        None => !current(s, p)?.vertical,
    };
    change(s, p, |d, _| {
        if matches!(d.kind, DrawingKind::Canvas | DrawingKind::WordArt) {
            return Err(CmdError::Failed("only text boxes and shapes can turn their text".into()));
        }
        d.vertical = vertical;
        Ok(())
    })
}

/// Shape Quick Styles kinds.
pub const SHAPE_STYLES: [&str; 4] = ["outline", "fill", "light", "strong"];

/// Shape Quick Styles: `style` in `outline` (light with an accent outline), `fill` (accent with a darker
/// outline), `light` (a light tint of the accent), `strong` (accent with a shadow), in theme colour `accent`
/// (1-6; 0 = the text colour).
fn shape_style(s: &mut Session, p: &Value) -> CmdResult {
    let style = opt_str(p, "style").unwrap_or("").to_owned();
    if !SHAPE_STYLES.contains(&style.as_str()) {
        return Err(CmdError::Params(format!("unknown style `{style}` (outline, fill, light, strong)")));
    }
    let accent = p.get("accent").and_then(Value::as_u64).unwrap_or(1);
    if accent > 6 {
        return Err(CmdError::Params("`accent` is 0 (text colour) to 6".into()));
    }
    change(s, p, |d, c| {
        if matches!(d.kind, DrawingKind::Canvas | DrawingKind::WordArt) {
            return Err(CmdError::Failed("only shapes and text boxes take a shape style".into()));
        }
        let theme = c.theme_resolved();
        let a = match usize::try_from(accent).unwrap_or(1) {
            0 => theme.color(0),
            n => theme.color(crate::compose::ACCENT1 + n - 1),
        };
        let (dark, light) = (theme.color(0), theme.color(1));
        let line = shapes::is_line_shape(&d.shape);
        d.gradient.clear();
        d.effect = "none".into();
        d.outline_px = d.outline_px.max(1.0);
        match style.as_str() {
            "outline" => {
                d.fill = (!line).then_some(light);
                d.outline = Some(a);
                d.text_style.color = Some(dark);
            }
            "fill" => {
                d.fill = (!line).then_some(a);
                d.outline = Some(draw::darken_color(a, 0.35));
                d.text_style.color = Some(light);
            }
            "light" => {
                d.fill = (!line).then(|| draw::lighten_color(a, 0.8));
                d.outline = Some(a);
                d.text_style.color = Some(dark);
            }
            _ => {
                d.fill = (!line).then_some(a);
                d.outline = if line { Some(a) } else { None };
                d.effect = "shadow".into();
                d.text_style.color = Some(light);
            }
        }
        Ok(())
    })
}

fn outline(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "custom") == Some(true) {
        let cur = current(s, p)?.outline;
        return color_dialog(s, p, "shapeOutline", cur);
    }
    let col = color_choice(s, p)?;
    let weight = num(p, "weight")?;
    if col.is_none() && weight.is_none() {
        return Err(CmdError::Params(
            "give `color` (null for No Outline), `theme` and `tint`, or `weight`".into(),
        ));
    }
    change(s, p, |d, _| {
        if let Some(c) = col {
            d.outline = c;
        }
        if let Some(w) = weight {
            d.outline_px = w;
            if d.outline.is_none() && w > 0.0 {
                d.outline = Some(Color::BLACK);
            }
        }
        Ok(())
    })
}

fn effect(s: &mut Session, p: &Value) -> CmdResult {
    let e = opt_str(p, "effect").unwrap_or("").to_owned();
    if !EFFECTS.contains(&e.as_str()) {
        return Err(CmdError::Params(format!(
            "unknown effect `{e}` (none, shadow, glow, reflection, bevel, softEdges, or \"\" for the theme's)"
        )));
    }
    change(s, p, |d, _| {
        d.effect = e;
        Ok(())
    })
}

fn text_color(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "custom") == Some(true) {
        let cur = current(s, p)?.text_style.color;
        return color_dialog(s, p, "textFill", cur);
    }
    let Some(col) = color_choice(s, p)? else {
        return Err(CmdError::Params("give `color` (null for automatic) or `theme` and `tint`".into()));
    };
    change(s, p, |d, _| {
        d.text_style.color = col;
        Ok(())
    })
}

fn font(s: &mut Session, p: &Value) -> CmdResult {
    let family = opt_str(p, "family")
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(|f| f.chars().take(200).collect::<String>());
    let size = num(p, "size")?;
    let (bold, italic, underline) = (opt_bool(p, "bold"), opt_bool(p, "italic"), opt_bool(p, "underline"));
    if family.is_none() && size.is_none() && bold.is_none() && italic.is_none() && underline.is_none() {
        return Err(CmdError::Params("give `family`, `size`, `bold`, `italic` or `underline`".into()));
    }
    change(s, p, |d, _| {
        if let Some(f) = family {
            d.text_style.font_family = Some(f);
        }
        if let Some(sz) = size {
            d.text_style.size_pt = Some(sz.clamp(1.0, 400.0));
        }
        if bold.is_some() {
            d.text_style.bold = bold;
        }
        if italic.is_some() {
            d.text_style.italic = italic;
        }
        if underline.is_some() {
            d.text_style.underline = underline;
        }
        Ok(())
    })
}

fn word_art_style(s: &mut Session, p: &Value) -> CmdResult {
    let style = opt_str(p, "style").map(str::trim).unwrap_or("").chars().take(300).collect::<String>();
    change(s, p, |d, _| {
        if style.is_empty() {
            // Clear WordArt: the text stays, as plain text in a box without fill or outline.
            if d.kind == DrawingKind::WordArt {
                d.kind = DrawingKind::TextBox;
                d.word_art.clear();
                d.fill = None;
                d.outline = None;
                d.text_style.size_pt = d.text_style.size_pt.or(Some(36.0));
                d.w += 16.0;
                d.h += 8.0;
            }
            return Ok(());
        }
        if d.kind == DrawingKind::Canvas {
            return Err(CmdError::Failed("a drawing canvas has no text to style".into()));
        }
        if d.kind != DrawingKind::WordArt {
            // The text becomes WordArt in that style (the box's fill and outline go).
            d.kind = DrawingKind::WordArt;
            d.fill = None;
            d.outline = None;
            if d.text.trim().is_empty() {
                d.text = "Your text here".into();
            }
        }
        d.word_art = style;
        Ok(())
    })
}

fn align_text(s: &mut Session, p: &Value) -> CmdResult {
    let h = match opt_str(p, "h") {
        None => None,
        Some("left") => Some(Align::Start),
        Some("center") => Some(Align::Center),
        Some("right") => Some(Align::End),
        Some("justify") => Some(Align::Justify),
        Some(o) => {
            return Err(CmdError::Params(format!("unknown alignment `{o}` (left, center, right, justify)")));
        }
    };
    let v = match opt_str(p, "v") {
        None => None,
        Some("top") => Some(VAlign::Top),
        Some("middle") => Some(VAlign::Middle),
        Some("bottom") => Some(VAlign::Bottom),
        Some(o) => {
            return Err(CmdError::Params(format!("unknown vertical alignment `{o}` (top, middle, bottom)")));
        }
    };
    if h.is_none() && v.is_none() {
        return Err(CmdError::Params("give `h` and / or `v`".into()));
    }
    change(s, p, |d, _| {
        if let Some(h) = h {
            d.align = h;
        }
        if let Some(v) = v {
            d.valign = v;
        }
        // Explicit alignment: the shape no longer centres its text by itself.
        if d.kind == DrawingKind::Shape && d.text_style == mailcraft_model::TextStyle::default() {
            d.text_style.size_pt = Some(11.0);
        }
        Ok(())
    })
}

fn change_shape(s: &mut Session, p: &Value) -> CmdResult {
    let raw = opt_str(p, "shape").ok_or_else(|| CmdError::Params("`shape` (string) required".into()))?;
    let shape = shapes::canonical(raw)
        .filter(|c| *c != "canvas")
        .ok_or_else(|| CmdError::Params(format!("unknown shape `{raw}`")))?;
    change(s, p, |d, c| {
        match d.kind {
            DrawingKind::Canvas | DrawingKind::WordArt => {
                return Err(CmdError::Failed("only shapes and text boxes can change shape".into()));
            }
            DrawingKind::TextBox => d.kind = DrawingKind::Shape,
            DrawingKind::Shape => {}
        }
        let was_line = shapes::is_line_shape(&d.shape);
        d.shape = shape.to_owned();
        let accent = c.theme_resolved().color(crate::compose::ACCENT1);
        if shapes::is_line_shape(shape) {
            d.outline = d.outline.or(Some(accent));
        } else if was_line && d.fill.is_none() {
            d.fill = Some(accent);
        }
        if !matches!(shape, "freeform" | "scribble") {
            d.points.clear();
        }
        Ok(())
    })
}

fn arrange(s: &mut Session, p: &Value) -> CmdResult {
    let action = opt_str(p, "action").unwrap_or("").to_owned();
    let (cid, id) = target(s, p)?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let Some(at) = c.body.drawings.iter().position(|d| d.id == id) else {
        return Err(CmdError::Params(format!("no drawing with id {id}")));
    };
    let last = c.body.drawings.len().saturating_sub(1);
    let before = c.body.drawings.clone();
    let mut list = c.body.drawings.clone();
    match action.as_str() {
        "bringForward" => list.swap(at, (at + 1).min(last)),
        "sendBackward" => list.swap(at, at.saturating_sub(1)),
        "bringToFront" => {
            let d = list.remove(at);
            list.push(d);
        }
        "sendToBack" => {
            let d = list.remove(at);
            list.insert(0, d);
        }
        "behindText" | "inFrontOfText" => {
            if let Some(d) = list.get_mut(at) {
                d.behind_text = action == "behindText";
            }
        }
        other => {
            return Err(CmdError::Params(format!(
                "unknown arrangement `{other}` (bringForward, bringToFront, sendBackward, sendToBack, behindText, inFrontOfText)"
            )));
        }
    }
    let changed = list != before;
    if changed {
        c.checkpoint();
        c.body.drawings = list;
        c.dirty = true;
    }
    c.selected_drawing = Some(id);
    let order: Vec<u32> = c.body.drawings.iter().map(|d| d.id).collect();
    Ok(json!({"drawing": id, "changed": changed, "order": order}))
}

fn align(s: &mut Session, p: &Value) -> CmdResult {
    let kind = opt_str(p, "kind").unwrap_or("").to_owned();
    if !matches!(kind.as_str(), "left" | "center" | "right" | "top" | "middle" | "bottom") {
        return Err(CmdError::Params(format!(
            "unknown alignment `{kind}` (left, center, right, top, middle, bottom)"
        )));
    }
    change(s, p, |d, c| {
        let width = c.view_width.filter(|w| w.is_finite() && *w > 40.0).unwrap_or(draw::DEFAULT_VIEW_WIDTH);
        let lo = mailcraft_layout::LayoutOptions { width, ..Default::default() };
        let fonts = mailcraft_fonts::FontDb::bundled();
        let height = mailcraft_layout::layout_doc(&c.body, &fonts, &lo).height.max(d.y + d.h);
        match kind.as_str() {
            "left" => d.x = 0.0,
            "center" => d.x = ((width - d.w) / 2.0).max(0.0),
            "right" => d.x = (width - d.w).max(0.0),
            "top" => d.y = 0.0,
            "middle" => d.y = ((height - d.h) / 2.0).max(0.0),
            _ => d.y = (height - d.h).max(0.0),
        }
        Ok(())
    })
}

fn rotate(s: &mut Session, p: &Value) -> CmdResult {
    let by = num(p, "by")?;
    let to = num(p, "degrees")?;
    if by.is_none() && to.is_none() {
        return Err(CmdError::Params("give `by` (degrees to turn) or `degrees`".into()));
    }
    change(s, p, |d, _| {
        d.rotation = to.unwrap_or(d.rotation) + by.unwrap_or(0.0);
        Ok(())
    })
}

fn delete(s: &mut Session, p: &Value) -> CmdResult {
    let (cid, id) = target(s, p)?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let Some(d) = c.body.drawings.iter().find(|d| d.id == id).cloned() else {
        return Err(CmdError::Params(format!("no drawing with id {id}")));
    };
    // Deleting a canvas deletes what was drawn in it.
    let gone: Vec<u32> = c
        .body
        .drawings
        .iter()
        .filter(|o| o.id == id || (d.kind == DrawingKind::Canvas && o.canvas == Some(id)))
        .map(|o| o.id)
        .collect();
    c.checkpoint();
    c.body.drawings.retain(|o| !gone.contains(&o.id));
    c.selected_drawing = None;
    c.dirty = true;
    Ok(json!({"deleted": gone}))
}

fn alt_text(s: &mut Session, p: &Value) -> CmdResult {
    let Some(alt) = opt_str(p, "alt") else {
        let d = current(s, p)?;
        let cid = compose_id(s, p)?;
        return dialog(s, "drawingAltText", json!({"compose": cid, "id": d.id, "alt": d.alt}));
    };
    let alt: String = alt.trim().chars().take(2000).collect();
    change(s, p, |d, _| {
        d.alt = alt;
        Ok(())
    })
}

fn format(s: &mut Session, p: &Value) -> CmdResult {
    let keys = ["x", "y", "w", "h", "rotation", "outlineWidth", "alt", "text", "behindText"];
    if !keys.iter().any(|k| p.get(*k).is_some()) {
        let d = current(s, p)?;
        let cid = compose_id(s, p)?;
        return dialog(
            s,
            "formatShape",
            json!({"compose": cid, "id": d.id, "kind": d.kind, "x": d.x, "y": d.y, "w": d.w, "h": d.h,
                   "rotation": d.rotation, "outlineWidth": d.outline_px, "alt": d.alt, "text": d.text,
                   "behindText": d.behind_text}),
        );
    }
    let (x, y, w, h) = (num(p, "x")?, num(p, "y")?, num(p, "w")?, num(p, "h")?);
    let (rot, ow) = (num(p, "rotation")?, num(p, "outlineWidth")?);
    let alt = opt_str(p, "alt").map(|a| a.trim().chars().take(2000).collect::<String>());
    let txt = opt_str(p, "text").map(|t| t.replace("\r\n", "\n"));
    let behind = opt_bool(p, "behindText");
    change(s, p, |d, _| {
        if let Some(v) = x {
            d.x = v;
        }
        if let Some(v) = y {
            d.y = v;
        }
        if d.kind == DrawingKind::WordArt {
            if let Some(nh) = h {
                let k = nh / d.h.max(1.0);
                d.text_style.size_pt = Some((d.text_style.size_pt.unwrap_or(36.0) * k).clamp(4.0, 400.0));
            }
        } else {
            if let Some(v) = w {
                d.w = v;
            }
            if let Some(v) = h {
                d.h = v;
            }
        }
        if let Some(v) = rot {
            d.rotation = v;
        }
        if let Some(v) = ow {
            d.outline_px = v;
        }
        if let Some(a) = alt {
            d.alt = a;
        }
        if let Some(t) = txt.filter(|_| d.kind != DrawingKind::Canvas) {
            d.text = t;
        }
        if let Some(b) = behind {
            d.behind_text = b;
        }
        Ok(())
    })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "drawing.select",
            "Select Drawing",
            "Compose body (click a drawing)",
            "",
            r#"{"id"?:number|null}  (null selects none)"#,
            false,
            any_compose,
            select,
        ),
        spec(
            "drawing.tool",
            "Drawing Tool",
            "Insert › Shapes; Insert › Text Box › Draw Text Box",
            "",
            r#"{"tool":"shape:<name>"|"textBox"|"canvas"|"scribble"|"freeform"|"tablePencil"|"tableEraser"|null}  (the next drag in the body draws it)"#,
            false,
            any_compose,
            tool,
        ),
        spec("drawing.list", "Drawings", "Compose body", "", "", false, any_compose, list),
        spec(
            "drawing.move",
            "Move Drawing",
            "Compose body (drag a drawing; arrow keys)",
            "",
            r#"{"id"?:number,"x"?:number,"y"?:number,"dx"?:number,"dy"?:number}"#,
            true,
            has_drawings,
            move_to,
        ),
        spec(
            "drawing.size",
            "Size",
            "Shape Format › Size (Height, Width; drag a handle)",
            "",
            r#"{"id"?:number,"w"?:number,"h"?:number,"x"?:number,"y"?:number,"keepRatio"?:bool}"#,
            true,
            has_drawings,
            size,
        ),
        spec(
            "drawing.text",
            "Edit Text",
            "Compose body (double-click a drawing)",
            "",
            r#"{"id"?:number,"text":string}"#,
            true,
            has_drawings,
            text,
        ),
        spec(
            "drawing.fill",
            "Shape Fill",
            "Shape Format › Shape Styles",
            "",
            r#"{"id"?:number,"color"?:string|null,"theme"?:string,"tint"?:number,"gradient"?:""|"down"|"up"|"right"|"left"|"center","custom"?:bool}  (null = No Fill)"#,
            true,
            has_drawings,
            fill,
        ),
        spec(
            "drawing.outline",
            "Shape Outline",
            "Shape Format › Shape Styles",
            "",
            r#"{"id"?:number,"color"?:string|null,"theme"?:string,"tint"?:number,"weight"?:number,"custom"?:bool}  (null = No Outline)"#,
            true,
            has_drawings,
            outline,
        ),
        spec(
            "drawing.effect",
            "Shape Effects",
            "Shape Format › Shape Styles",
            "",
            r#"{"id"?:number,"effect":""|"none"|"shadow"|"glow"|"reflection"|"bevel"|"softEdges"}"#,
            true,
            has_drawings,
            effect,
        ),
        spec(
            "drawing.textOutline",
            "Text Outline",
            "Shape Format › WordArt Styles",
            "",
            r#"{"id"?:number,"color"?:string|null,"theme"?:string,"tint"?:number,"custom"?:bool}  (null = No Outline)"#,
            true,
            has_drawings,
            text_outline,
        ),
        spec(
            "drawing.textEffect",
            "Text Effects",
            "Shape Format › WordArt Styles",
            "",
            r#"{"id"?:number,"effect":""|"none"|"shadow"|"glow"|"reflection"|"bevel"}  (reflection: WordArt)"#,
            true,
            has_drawings,
            text_effect,
        ),
        spec(
            "drawing.textDirection",
            "Text Direction",
            "Shape Format › Text",
            "",
            r#"{"id"?:number,"vertical"?:bool}  (no vertical: turns it the other way)"#,
            true,
            has_drawings,
            text_direction,
        ),
        spec(
            "drawing.shapeStyle",
            "Shape Quick Styles",
            "Shape Format › Shape Styles",
            "",
            r#"{"id"?:number,"style":"outline"|"fill"|"light"|"strong","accent"?:number}  (accent 1-6, 0 = text colour)"#,
            true,
            has_drawings,
            shape_style,
        ),
        spec(
            "drawing.textColor",
            "Text Fill",
            "Shape Format › WordArt Styles",
            "",
            r#"{"id"?:number,"color"?:string|null,"theme"?:string,"tint"?:number,"custom"?:bool}"#,
            true,
            has_drawings,
            text_color,
        ),
        spec(
            "drawing.font",
            "Drawing Font",
            "Shape Format › Text",
            "",
            r#"{"id"?:number,"family"?:string,"size"?:number,"bold"?:bool,"italic"?:bool,"underline"?:bool}"#,
            true,
            has_drawings,
            font,
        ),
        spec(
            "drawing.wordArt",
            "WordArt Quick Styles",
            "Shape Format › WordArt Styles",
            "",
            r#"{"id"?:number,"style":string}  ("" = Clear WordArt)"#,
            true,
            has_drawings,
            word_art_style,
        ),
        spec(
            "drawing.alignText",
            "Align Text",
            "Shape Format › Text",
            "",
            r#"{"id"?:number,"h"?:"left"|"center"|"right"|"justify","v"?:"top"|"middle"|"bottom"}"#,
            true,
            has_drawings,
            align_text,
        ),
        spec(
            "drawing.changeShape",
            "Change Shape",
            "Shape Format › Insert Shapes › Edit Shape",
            "",
            r#"{"id"?:number,"shape":string}"#,
            true,
            has_drawings,
            change_shape,
        ),
        spec(
            "drawing.arrange",
            "Arrange",
            "Shape Format › Arrange (Bring Forward, Send Backward)",
            "",
            r#"{"id"?:number,"action":"bringForward"|"bringToFront"|"sendBackward"|"sendToBack"|"behindText"|"inFrontOfText"}"#,
            true,
            has_drawings,
            arrange,
        ),
        spec(
            "drawing.align",
            "Align",
            "Shape Format › Arrange",
            "",
            r#"{"id"?:number,"kind":"left"|"center"|"right"|"top"|"middle"|"bottom"}  (to the margins)"#,
            true,
            has_drawings,
            align,
        ),
        spec(
            "drawing.rotate",
            "Rotate",
            "Shape Format › Arrange",
            "",
            r#"{"id"?:number,"by"?:number,"degrees"?:number}"#,
            true,
            has_drawings,
            rotate,
        ),
        spec(
            "drawing.delete",
            "Delete Drawing",
            "Compose body (Delete with a drawing selected)",
            "",
            r#"{"id"?:number}"#,
            true,
            has_drawings,
            delete,
        ),
        spec(
            "drawing.altText",
            "Alt Text",
            "Shape Format › Accessibility",
            "",
            r#"{"id"?:number,"alt"?:string}  (no alt: the Alt Text dialog)"#,
            true,
            has_drawings,
            alt_text,
        ),
        spec(
            "drawing.format",
            "Size and Position",
            "Shape Format › Size (dialog launcher); Format Shape",
            "",
            r#"{"id"?:number,"x"?:number,"y"?:number,"w"?:number,"h"?:number,"rotation"?:number,"outlineWidth"?:number,"alt"?:string,"text"?:string,"behindText"?:bool}  (nothing: the dialog)"#,
            true,
            has_drawings,
            format,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::compose::tests::{open, session};
    use super::*;

    fn drawings(s: &Session, cid: u64) -> Vec<Drawing> {
        s.composes[&cid].body.drawings.clone()
    }

    #[test]
    fn shapes_are_floating_objects_that_move_size_and_undo() {
        let mut s = session();
        let cid = open(&mut s);
        let r = s.run("insert.shape", &json!({"compose": cid, "kind": "oval", "x": 40, "y": 30})).unwrap();
        let id = r["drawing"].as_u64().unwrap() as u32;
        let d = &drawings(&s, cid)[0];
        assert_eq!((d.x, d.y, d.w, d.h), (40.0, 30.0, 100.0, 100.0));
        assert_eq!(s.composes[&cid].selected_drawing, Some(id));
        assert!(d.fill.is_some() && d.outline.is_some());
        s.run("drawing.move", &json!({"compose": cid, "dx": 10, "dy": -5})).unwrap();
        s.run("drawing.size", &json!({"compose": cid, "w": 200, "keepRatio": true})).unwrap();
        let d = &drawings(&s, cid)[0];
        assert_eq!((d.x, d.y, d.w, d.h), (50.0, 25.0, 200.0, 200.0));
        s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
        assert_eq!(drawings(&s, cid)[0].w, 100.0, "one undo step per change");
        s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
        assert_eq!(drawings(&s, cid)[0].x, 40.0);
        // Junk is refused, not crashed on.
        assert!(s.run("drawing.move", &json!({"compose": cid, "x": "far"})).is_err());
        assert!(s.run("drawing.size", &json!({"compose": cid, "id": 999, "w": 5})).is_err());
        s.run("drawing.size", &json!({"compose": cid, "w": 1e30, "h": -4})).unwrap();
        let d = &drawings(&s, cid)[0];
        assert_eq!((d.w, d.h), (draw::MAX_DRAWING_SIDE, draw::MIN_DRAWING_SIDE));
    }

    #[test]
    fn shape_format_commands_change_the_selected_drawing() {
        let mut s = session();
        let cid = open(&mut s);
        s.run("insert.shape", &json!({"compose": cid, "kind": "rectangle", "x": 0, "y": 0})).unwrap();
        s.run("drawing.fill", &json!({"compose": cid, "color": "#ff0000"})).unwrap();
        s.run("drawing.outline", &json!({"compose": cid, "color": null})).unwrap();
        s.run("drawing.effect", &json!({"compose": cid, "effect": "shadow"})).unwrap();
        s.run("drawing.text", &json!({"compose": cid, "text": "Inside\r\nthe box"})).unwrap();
        s.run("drawing.textColor", &json!({"compose": cid, "theme": "accent2", "tint": 0})).unwrap();
        s.run("drawing.alignText", &json!({"compose": cid, "h": "right", "v": "bottom"})).unwrap();
        s.run("drawing.font", &json!({"compose": cid, "size": 20, "bold": true})).unwrap();
        s.run("drawing.rotate", &json!({"compose": cid, "by": -90})).unwrap();
        s.run("drawing.altText", &json!({"compose": cid, "alt": "A red box"})).unwrap();
        let d = &drawings(&s, cid)[0];
        assert_eq!(d.fill, Some(Color::rgb(255, 0, 0)));
        assert_eq!(d.outline, None);
        assert_eq!(d.effect, "shadow");
        assert_eq!(d.text, "Inside\nthe box");
        assert_eq!(d.text_style.color, crate::cmd::format::theme_color(&s.composes[&cid], "accent2", 0));
        assert_eq!((d.align, d.valign), (Align::End, VAlign::Bottom));
        assert_eq!((d.text_style.size_pt, d.text_style.bold), (Some(20.0), Some(true)));
        assert_eq!(d.rotation, 270.0);
        assert_eq!(d.alt, "A red box");
        assert!(s.run("drawing.effect", &json!({"compose": cid, "effect": "sparkles"})).is_err());
        // Custom colours and the alt text / size dialogs.
        for (cmd, p) in [
            ("drawing.fill", json!({"compose": cid, "custom": true})),
            ("drawing.altText", json!({"compose": cid})),
            ("drawing.format", json!({"compose": cid})),
        ] {
            assert!(s.run(cmd, &p).unwrap().get("dialog").is_some(), "{cmd}");
        }
        s.run("drawing.changeShape", &json!({"compose": cid, "shape": "Arrow: Right"})).unwrap();
        assert_eq!(drawings(&s, cid)[0].shape, "rightArrow");
        s.run(
            "drawing.format",
            &json!({"compose": cid, "x": 5, "y": 6, "w": 70, "h": 80, "behindText": true}),
        )
        .unwrap();
        let d = &drawings(&s, cid)[0];
        assert_eq!((d.x, d.y, d.w, d.h, d.behind_text), (5.0, 6.0, 70.0, 80.0, true));
    }

    #[test]
    fn arrange_align_delete_and_canvas_contents() {
        let mut s = session();
        let cid = open(&mut s);
        s.run("insert.shape", &json!({"compose": cid, "kind": "canvas", "x": 0, "y": 0, "w": 300, "h": 200}))
            .unwrap();
        s.run("insert.shape", &json!({"compose": cid, "kind": "oval", "x": 20, "y": 20, "w": 50, "h": 50}))
            .unwrap();
        s.run("insert.shape", &json!({"compose": cid, "kind": "star5", "x": 400, "y": 0})).unwrap();
        let ids: Vec<u32> = drawings(&s, cid).iter().map(|d| d.id).collect();
        assert_eq!(drawings(&s, cid)[0].kind, DrawingKind::Canvas);
        s.run("drawing.arrange", &json!({"compose": cid, "id": ids[2], "action": "sendToBack"})).unwrap();
        assert_eq!(drawings(&s, cid)[0].id, ids[2]);
        s.run("drawing.arrange", &json!({"compose": cid, "id": ids[2], "action": "bringForward"})).unwrap();
        assert_eq!(drawings(&s, cid)[1].id, ids[2]);
        // Moving the canvas carries the oval drawn inside it, not the star outside it.
        s.run("drawing.move", &json!({"compose": cid, "id": ids[0], "dx": 10, "dy": 100})).unwrap();
        let by_id = |s: &Session, id: u32| drawings(s, cid).into_iter().find(|d| d.id == id).unwrap();
        assert_eq!((by_id(&s, ids[1]).x, by_id(&s, ids[1]).y), (30.0, 120.0));
        assert_eq!((by_id(&s, ids[2]).x, by_id(&s, ids[2]).y), (400.0, 0.0));
        s.composes.get_mut(&cid).unwrap().view_width = Some(500.0);
        s.run("drawing.align", &json!({"compose": cid, "id": ids[2], "kind": "right"})).unwrap();
        assert_eq!(by_id(&s, ids[2]).x, 400.0);
        s.run("drawing.align", &json!({"compose": cid, "id": ids[2], "kind": "center"})).unwrap();
        assert_eq!(by_id(&s, ids[2]).x, 200.0);
        // Deleting the canvas deletes what is in it.
        let r = s.run("drawing.delete", &json!({"compose": cid, "id": ids[0]})).unwrap();
        assert_eq!(r["deleted"].as_array().unwrap().len(), 2);
        assert_eq!(drawings(&s, cid).len(), 1);
        assert_eq!(s.composes[&cid].selected_drawing, None);
        assert!(s.run("drawing.fill", &json!({"compose": cid, "color": "red"})).is_err(), "nothing selected");
        s.run("drawing.select", &json!({"compose": cid, "id": ids[2]})).unwrap();
        s.run("drawing.fill", &json!({"compose": cid, "color": "red"})).unwrap();
        assert!(s.run("drawing.select", &json!({"compose": cid, "id": 77})).is_err());
    }

    /// Regression: a canvas took in, moved and deleted every drawing that happened to lie inside its box,
    /// including ones drawn before it.
    #[test]
    fn a_canvas_drawn_over_existing_drawings_does_not_take_them() {
        let mut s = session();
        let cid = open(&mut s);
        s.run("insert.shape", &json!({"compose": cid, "kind": "oval", "x": 20, "y": 20, "w": 50, "h": 50}))
            .unwrap();
        s.run("insert.shape", &json!({"compose": cid, "kind": "canvas", "x": 0, "y": 0, "w": 300, "h": 200}))
            .unwrap();
        s.run("insert.shape", &json!({"compose": cid, "kind": "star5", "x": 100, "y": 20, "w": 50, "h": 50}))
            .unwrap();
        let ids: Vec<u32> = drawings(&s, cid).iter().map(|d| d.id).collect();
        let by_id = |s: &Session, id: u32| drawings(s, cid).into_iter().find(|d| d.id == id).unwrap();
        assert_eq!(by_id(&s, ids[0]).canvas, None, "drawn before the canvas");
        assert_eq!(by_id(&s, ids[2]).canvas, Some(ids[1]), "drawn in the canvas");
        s.run("drawing.move", &json!({"compose": cid, "id": ids[1], "dx": 10, "dy": 0})).unwrap();
        assert_eq!(by_id(&s, ids[0]).x, 20.0, "the older oval stays put");
        assert_eq!(by_id(&s, ids[2]).x, 110.0, "the star moves with its canvas");
        // A member moved out of the canvas leaves it; the oval moved into it joins.
        s.run("drawing.move", &json!({"compose": cid, "id": ids[2], "x": 500, "y": 0})).unwrap();
        assert_eq!(by_id(&s, ids[2]).canvas, None);
        s.run("drawing.move", &json!({"compose": cid, "id": ids[0], "x": 40, "y": 40})).unwrap();
        assert_eq!(by_id(&s, ids[0]).canvas, Some(ids[1]));
        let r = s.run("drawing.delete", &json!({"compose": cid, "id": ids[1]})).unwrap();
        assert_eq!(r["deleted"], json!([ids[0], ids[1]]));
        assert_eq!(drawings(&s, cid).iter().map(|d| d.id).collect::<Vec<_>>(), vec![ids[2]]);
    }

    /// Alpha-weighted brightness of the raster's pixel at (`fx`, `fy`) as fractions of its size.
    fn shade(r: &draw::Raster, fx: f32, fy: f32) -> u32 {
        let x = ((r.width as f32 - 1.0) * fx) as usize;
        let y = ((r.height as f32 - 1.0) * fy) as usize;
        let i = (y * r.width as usize + x) * 4;
        r.rgba[i..i + 3].iter().map(|v| u32::from(*v)).sum()
    }

    fn raster_of(s: &Session, cid: u64, id: u32) -> draw::Raster {
        let c = &s.composes[&cid];
        let d = c.body.drawings.iter().find(|d| d.id == id).unwrap();
        draw::raster(d, &c.theme_resolved(), true).unwrap()
    }

    #[test]
    fn gradient_fill_shades_from_light_to_the_fill() {
        let mut s = session();
        let cid = open(&mut s);
        let r = s
            .run(
                "insert.shape",
                &json!({"compose": cid, "kind": "rectangle", "x": 0, "y": 0, "w": 200, "h": 200}),
            )
            .unwrap();
        let id = r["drawing"].as_u64().unwrap() as u32;
        s.run("drawing.effect", &json!({"compose": cid, "effect": "none"})).unwrap();
        let solid = raster_of(&s, cid, id);
        assert!(shade(&solid, 0.5, 0.2).abs_diff(shade(&solid, 0.5, 0.8)) < 6, "a solid fill is even");
        s.run("drawing.fill", &json!({"compose": cid, "gradient": "down"})).unwrap();
        let g = raster_of(&s, cid, id);
        assert!(shade(&g, 0.5, 0.2) > shade(&g, 0.5, 0.8) + 60, "lighter at the top");
        s.run("drawing.fill", &json!({"compose": cid, "gradient": "right"})).unwrap();
        let g = raster_of(&s, cid, id);
        assert!(shade(&g, 0.2, 0.5) > shade(&g, 0.8, 0.5) + 60, "lighter at the left");
        assert!(s.run("drawing.fill", &json!({"compose": cid, "gradient": "spiral"})).is_err());
        // No Fill drops the gradient; a gradient on an unfilled shape gives it the accent colour.
        s.run("drawing.fill", &json!({"compose": cid, "color": null})).unwrap();
        assert_eq!(drawings(&s, cid)[0].gradient, "");
        s.run("drawing.fill", &json!({"compose": cid, "gradient": "center"})).unwrap();
        let d = &drawings(&s, cid)[0];
        assert!(d.fill.is_some() && d.gradient == "center");
    }

    #[test]
    fn vertical_text_runs_top_to_bottom() {
        let mut s = session();
        let cid = open(&mut s);
        let r =
            s.run("insert.textBox", &json!({"compose": cid, "preset": "draw", "vertical": true})).unwrap();
        assert_eq!(r["armed"], "verticalTextBox");
        let r = s
            .run(
                "insert.textBox",
                &json!({"compose": cid, "text": "WWWWWWWWWW", "x": 0, "y": 0, "w": 300, "h": 300, "vertical": true}),
            )
            .unwrap();
        let id = r["drawing"].as_u64().unwrap() as u32;
        assert!(drawings(&s, cid)[0].vertical);
        s.run("drawing.outline", &json!({"compose": cid, "color": null})).unwrap();
        s.run("drawing.fill", &json!({"compose": cid, "color": null})).unwrap();
        // The ink of a long word is tall and narrow when vertical, wide and short when not.
        let ink = |r: &draw::Raster| {
            let (w, h) = (r.width as usize, r.height as usize);
            let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
            for y in 0..h {
                for x in 0..w {
                    if r.rgba[(y * w + x) * 4 + 3] > 128 {
                        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                    }
                }
            }
            (x1.saturating_sub(x0), y1.saturating_sub(y0))
        };
        let (w, h) = ink(&raster_of(&s, cid, id));
        assert!(h > w * 3, "vertical: {w}x{h}");
        s.run("drawing.textDirection", &json!({"compose": cid, "vertical": false})).unwrap();
        let (w, h) = ink(&raster_of(&s, cid, id));
        assert!(w > h * 3, "horizontal: {w}x{h}");
        // Without `vertical` it turns the other way.
        s.run("drawing.textDirection", &json!({"compose": cid})).unwrap();
        assert!(drawings(&s, cid)[0].vertical);
        s.run("insert.wordArt", &json!({"compose": cid, "style": "Fill: Black, Text color 1", "text": "Hi"}))
            .unwrap();
        assert!(s.run("drawing.textDirection", &json!({"compose": cid, "vertical": true})).is_err());
    }

    #[test]
    fn text_outline_effects_and_shape_styles() {
        let mut s = session();
        let cid = open(&mut s);
        let r = s
            .run(
                "insert.textBox",
                &json!({"compose": cid, "text": "Outlined", "x": 0, "y": 0, "w": 200, "h": 60}),
            )
            .unwrap();
        let id = r["drawing"].as_u64().unwrap() as u32;
        let plain = raster_of(&s, cid, id);
        s.run("drawing.textOutline", &json!({"compose": cid, "color": "#ff0000"})).unwrap();
        assert_eq!(drawings(&s, cid)[0].text_outline, Some(Color::rgb(0xff, 0, 0)));
        assert_ne!(raster_of(&s, cid, id).rgba, plain.rgba);
        s.run("drawing.textOutline", &json!({"compose": cid, "color": null})).unwrap();
        assert_eq!(raster_of(&s, cid, id).rgba, plain.rgba, "No Outline");
        s.run("drawing.textEffect", &json!({"compose": cid, "effect": "shadow"})).unwrap();
        assert_ne!(raster_of(&s, cid, id).rgba, plain.rgba);
        assert!(
            s.run("drawing.textEffect", &json!({"compose": cid, "effect": "reflection"})).is_err(),
            "reflections are for WordArt"
        );
        assert!(s.run("drawing.textEffect", &json!({"compose": cid, "effect": "sparkle"})).is_err());
        // WordArt: Text Effects replaces the style's effects; reflection makes it taller.
        let r = s
            .run(
                "insert.wordArt",
                &json!({"compose": cid, "style": "Fill: Black, Text color 1; Shadow", "text": "Big"}),
            )
            .unwrap();
        let wa = r["drawing"].as_u64().unwrap() as u32;
        let before = raster_of(&s, cid, wa);
        s.run("drawing.textEffect", &json!({"compose": cid, "id": wa, "effect": "reflection"})).unwrap();
        let after = raster_of(&s, cid, wa);
        assert!(after.height > before.height, "the reflection adds height");
        s.run("drawing.textOutline", &json!({"compose": cid, "id": wa, "theme": "accent2", "tint": 0}))
            .unwrap();
        assert_ne!(raster_of(&s, cid, wa).rgba, after.rgba);
        // Shape Quick Styles.
        let r = s.run("insert.shape", &json!({"compose": cid, "kind": "oval", "x": 300, "y": 0})).unwrap();
        let oval = r["drawing"].as_u64().unwrap() as u32;
        let theme = s.composes[&cid].theme_resolved();
        let accent3 = theme.color(crate::compose::ACCENT1 + 2);
        s.run("drawing.shapeStyle", &json!({"compose": cid, "style": "outline", "accent": 3})).unwrap();
        let d = drawings(&s, cid).into_iter().find(|d| d.id == oval).unwrap();
        assert_eq!((d.fill, d.outline), (Some(theme.color(1)), Some(accent3)));
        s.run("drawing.shapeStyle", &json!({"compose": cid, "style": "strong", "accent": 3})).unwrap();
        let d = drawings(&s, cid).into_iter().find(|d| d.id == oval).unwrap();
        assert_eq!((d.fill, d.outline, d.effect.as_str()), (Some(accent3), None, "shadow"));
        assert!(s.run("drawing.shapeStyle", &json!({"compose": cid, "style": "loud"})).is_err());
        assert!(s.run("drawing.shapeStyle", &json!({"compose": cid, "style": "fill", "accent": 9})).is_err());
        assert!(s.run("drawing.shapeStyle", &json!({"compose": cid, "id": wa, "style": "fill"})).is_err());
    }

    /// Saving a draft stores the drawings as pictures (what other mail programs can show) and leaves the
    /// open message's drawings editable.
    #[test]
    fn saving_a_draft_keeps_the_drawings_editable_in_the_open_message() {
        let mut s = session();
        let cid = open(&mut s);
        s.run("compose.insertText", &json!({"compose": cid, "text": "Body"})).unwrap();
        s.run("insert.textBox", &json!({"compose": cid, "text": "Inside", "x": 10, "y": 40})).unwrap();
        let before = drawings(&s, cid);
        let r = s.run("compose.saveDraft", &json!({"compose": cid})).unwrap();
        let draft = crate::model::ItemId(r["draft"].as_u64().unwrap());
        assert_eq!(drawings(&s, cid), before, "still floating, with their text");
        let html = s.mailbox.mail[&draft].body.html.clone().unwrap_or_default();
        assert!(html.contains("<img"), "the draft holds the drawing as a picture: {html}");
        s.run("drawing.text", &json!({"compose": cid, "text": "Changed"})).unwrap();
        assert_eq!(drawings(&s, cid)[0].text, "Changed");
    }

    #[test]
    fn tools_arm_and_drawn_shapes_disarm() {
        let mut s = session();
        let cid = open(&mut s);
        s.run("insert.shape", &json!({"compose": cid, "kind": "heart", "draw": true})).unwrap();
        assert_eq!(s.composes[&cid].draw_tool.as_deref(), Some("shape:heart"));
        assert!(drawings(&s, cid).is_empty());
        s.run("insert.shape", &json!({"compose": cid, "kind": "heart", "x": 1, "y": 2, "w": 30, "h": 40}))
            .unwrap();
        assert_eq!(s.composes[&cid].draw_tool, None);
        s.run("insert.textBox", &json!({"compose": cid, "preset": "draw"})).unwrap();
        assert_eq!(s.composes[&cid].draw_tool.as_deref(), Some("textBox"));
        s.run("drawing.tool", &json!({"compose": cid, "tool": null})).unwrap();
        assert_eq!(s.composes[&cid].draw_tool, None);
        assert!(s.run("drawing.tool", &json!({"compose": cid, "tool": "shape:nonsense"})).is_err());
        s.run("drawing.tool", &json!({"compose": cid, "tool": "scribble"})).unwrap();
        let pts = json!([[0.0, 0.0], [0.5, 1.0], [1.0, 0.2]]);
        s.run(
            "insert.shape",
            &json!({"compose": cid, "kind": "scribble", "x": 0, "y": 0, "w": 80, "h": 40, "points": pts}),
        )
        .unwrap();
        assert_eq!(drawings(&s, cid)[1].points.len(), 3);
        let listed = s.run("drawing.list", &json!({"compose": cid})).unwrap();
        assert_eq!(listed["drawings"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn word_art_is_editable_text_that_fits_its_box() {
        let mut s = session();
        let cid = open(&mut s);
        s.run("insert.wordArt", &json!({"compose": cid, "style": "Fill: Black, Text color 1; Shadow"}))
            .unwrap();
        let d = drawings(&s, cid)[0].clone();
        assert_eq!(d.kind, DrawingKind::WordArt);
        assert_eq!(d.text, "Your text here");
        s.run("drawing.text", &json!({"compose": cid, "text": "Hi"})).unwrap();
        let short = drawings(&s, cid)[0].clone();
        assert!(short.w < d.w, "the box fits the shorter text");
        s.run("drawing.size", &json!({"compose": cid, "h": short.h * 2.0})).unwrap();
        let big = drawings(&s, cid)[0].clone();
        assert!(big.text_style.size_pt.unwrap() > 60.0 && big.w > short.w * 1.5, "{big:?}");
        s.run(
            "drawing.wordArt",
            &json!({"compose": cid, "style": "Gradient Fill: Plum, Accent color 5; Reflection"}),
        )
        .unwrap();
        assert!(drawings(&s, cid)[0].word_art.starts_with("Gradient"));
        s.run("drawing.wordArt", &json!({"compose": cid, "style": ""})).unwrap();
        let cleared = drawings(&s, cid)[0].clone();
        assert_eq!(cleared.kind, DrawingKind::TextBox);
        assert_eq!(cleared.text, "Hi");
    }
}
