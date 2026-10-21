//! Format Shape (`drawing.format`): the pane's state and applying what it sets — Fill & Line (no fill, a
//! colour, a preset or stop-by-stop gradient, a picture; transparency; line colour, weight, dashes and
//! transparency), Effects (shadow, reflection, glow, soft edges, bevel, 3-D rotation, with options), Layout
//! & Properties (text box margins, Resize Shape to Fit Text, vertical alignment, text wrapping, alt text,
//! name) and Size (position, size, rotation) — as one undo step. Also the parameter readers the Shape Fill
//! and Shape Outline commands share.

use serde_json::{Value, json};

use super::{find, num, targets};
use crate::cmd::insert::drawing as draw;
use crate::cmd::util::{dialog, opt_bool, opt_str};
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::rich::{Drawing, DrawingKind, Gradient, GradientStop, VAlign};

/// A 0..=1 parameter (a number above 1 or a string with `%` is a percentage).
pub(super) fn unit_param(p: &Value, key: &str) -> Result<Option<f32>, CmdError> {
    let pct = p.get(key).and_then(Value::as_str).is_some_and(|t| t.trim_end().ends_with('%'));
    let v = match p.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(v) => v
            .as_f64()
            .or_else(|| v.as_str().and_then(|t| t.trim().trim_end_matches('%').trim().parse().ok()))
            .filter(|n| n.is_finite())
            .ok_or_else(|| CmdError::Params(format!("`{key}` must be a number")))? as f32,
    };
    Ok(Some(if pct || v > 1.0 { v / 100.0 } else { v }.clamp(0.0, 1.0)))
}

/// `dash`: an outline dash style.
pub(super) fn dash_param(p: &Value) -> Result<Option<String>, CmdError> {
    match p.get("dash") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(String::new())),
        Some(Value::String(d)) if d == "solid" => Ok(Some(String::new())),
        Some(Value::String(d)) if draw::DASHES.contains(&d.as_str()) => Ok(Some(d.clone())),
        Some(other) => Err(CmdError::Params(format!(
            "unknown dash {other} (solid, roundDot, squareDot, dash, dashDot, longDash, longDashDot, longDashDotDot)"
        ))),
    }
}

/// A picture fill: `picture: null` (none), or `picture: true` with `path` / `data_base64` (a PNG picture),
/// or `picture` as base64 PNG. Returns the stored form (`""` = none).
pub(super) fn picture_param(p: &Value) -> Result<Option<String>, CmdError> {
    let data = match p.get("picture") {
        None => return Ok(None),
        Some(Value::Null) | Some(Value::Bool(false)) => return Ok(Some(String::new())),
        Some(Value::Bool(true)) => {
            match crate::cmd::compose::file_param(p, crate::cmd::insert::MAX_PICTURE_BYTES)? {
                Some((_, data)) => data,
                None => return Err(CmdError::Params("give the picture as `path` or `data_base64`".into())),
            }
        }
        Some(Value::String(b64)) => mailcraft_mime::transfer::decode_base64(b64.as_bytes()),
        Some(_) => {
            return Err(CmdError::Params("`picture` is true (with a file), base64 PNG or null".into()));
        }
    };
    draw::store_picture(&data, draw::PICTURE_FILL_SIDE).map(Some).ok_or_else(|| {
        CmdError::Failed("only PNG pictures can fill a shape (the app converts other pictures)".into())
    })
}

/// A stop-by-stop gradient: `stops` ([{pos, color}] or null to drop it), `angle`, `radial`.
pub(super) fn gradient_param(p: &Value) -> Result<Option<Option<Gradient>>, CmdError> {
    let stops = match p.get("stops") {
        None => return Ok(None),
        Some(Value::Null) => return Ok(Some(None)),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(CmdError::Params("`stops` must be a list of {pos, color}".into())),
    };
    let mut out = Vec::new();
    for st in stops.iter().take(draw::MAX_GRADIENT_STOPS) {
        let pos = st
            .get("pos")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .ok_or_else(|| CmdError::Params("a stop needs a `pos` from 0 to 1".into()))?
            as f32;
        let pos = if pos > 1.0 { pos / 100.0 } else { pos };
        let text = st.get("color").and_then(Value::as_str).unwrap_or("");
        let mut color = mailcraft_html::parse_css_color(text)
            .ok_or_else(|| CmdError::Params(format!("`{text}` is not a colour")))?;
        if let Some(t) = unit_param(st, "transparency")? {
            color = color.with_alpha(((1.0 - t) * 255.0).round() as u8);
        }
        out.push(GradientStop { pos: pos.clamp(0.0, 1.0), color });
    }
    if out.len() < 2 {
        return Err(CmdError::Params("a gradient needs at least two stops".into()));
    }
    let angle = num(p, "angle")?.unwrap_or(90.0);
    Ok(Some(Some(Gradient { radial: opt_bool(p, "radial") == Some(true), angle, stops: out })))
}

/// The Format Shape pane for the target drawings (`id` / `ids`, else the selection), open on `page`. The
/// size, position, rotation, wrapping, name and alt text shown are the first target's own (a group's are
/// the group's); fill, line, effects and text are its first drawing's (a group has none of its own). OK
/// sends the changes back for every target (`ids`).
pub(super) fn format_dialog(s: &mut Session, p: &Value, page: &str) -> CmdResult {
    let (cid, ids) = targets(s, p)?;
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let primary = ids.first().copied().ok_or_else(|| CmdError::Disabled("select a drawing first".into()))?;
    let t = find(&c.body.drawings, primary)
        .cloned()
        .ok_or_else(|| CmdError::Params(format!("no drawing with id {primary}")))?;
    let look = super::leaves(&c.body.drawings, &[primary]).first().copied().unwrap_or(primary);
    let d = &find(&c.body.drawings, look).cloned().unwrap_or_else(|| t.clone());
    let hex = |c: Option<mailcraft_geom::Color>| c.map(|c| c.to_hex());
    let stops: Vec<Value> = d
        .gradient_fill
        .iter()
        .flat_map(|g| g.stops.iter())
        .map(|st| json!({"pos": st.pos, "color": st.color.with_alpha(255).to_hex(), "transparency": 1.0 - f32::from(st.color.a) / 255.0}))
        .collect();
    // A drawing with only a legacy effect name shows that effect's options.
    let fx = d
        .fx
        .as_deref()
        .cloned()
        .or_else(|| (!d.effect.is_empty() && d.effect != "none").then(|| super::fx::from_legacy(&d.effect)))
        .map(|f| serde_json::to_value(f).unwrap_or(Value::Null));
    let text_fx = d.text_fx.as_deref().map(|f| serde_json::to_value(f).unwrap_or(Value::Null));
    dialog(
        s,
        "formatShape",
        json!({
            "compose": cid, "id": t.id, "ids": ids, "kind": t.kind, "page": page,
            "x": t.x, "y": t.y, "w": t.w, "h": t.h, "rotation": t.rotation,
            "fill": hex(d.fill), "transparency": d.transparency, "gradient": d.gradient,
            "stops": stops, "angle": d.gradient_fill.as_ref().map_or(90.0, |g| g.angle),
            "radial": d.gradient_fill.as_ref().is_some_and(|g| g.radial),
            "picture": !d.fill_picture.is_empty(),
            "outline": hex(d.outline), "outlineWidth": d.outline_px, "dash": d.line_dash,
            "lineTransparency": d.line_transparency,
            "effect": d.effect, "fx": fx, "textFx": text_fx,
            "inset": d.inset.unwrap_or(draw::TEXT_BOX_INSET), "autofit": d.autofit, "valign": d.valign,
            "wrap": super::wrap_name(&t), "behindText": t.behind_text,
            "alt": t.alt, "text": d.text, "name": t.name,
        }),
    )
}

/// Keys Format Shape applies.
const KEYS: [&str; 28] = [
    "x",
    "y",
    "w",
    "h",
    "rotation",
    "outlineWidth",
    "alt",
    "text",
    "behindText",
    "fill",
    "transparency",
    "gradient",
    "stops",
    "picture",
    "outline",
    "lineTransparency",
    "dash",
    "effects",
    "textEffects",
    "inset",
    "autofit",
    "valign",
    "wrap",
    "name",
    "color",
    "data_base64",
    "path",
    "noFill",
];

/// Format Shape: without properties the pane (on `page`); with them, they are applied to the target
/// drawings, all or nothing, as one undo step. Size, position, rotation and wrapping apply to each target
/// as a whole (a group as one); fill, line, effects and the text box settings to the drawings in it; alt
/// text to each target; the name to the first target.
pub(super) fn format(s: &mut Session, p: &Value) -> CmdResult {
    if !KEYS.iter().any(|k| p.get(*k).is_some()) {
        let page = opt_str(p, "page").unwrap_or("size").to_owned();
        return format_dialog(s, p, &page);
    }
    let (cid, ids) = targets(s, p)?;
    let base = |extra: Value| -> Value {
        let mut q = json!({"compose": cid, "ids": ids});
        if let (Some(o), Some(e)) = (q.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
        q
    };
    let mut steps: Vec<(&str, Value)> = Vec::new();
    // Size and position.
    let (x, y, w, h) = (num(p, "x")?, num(p, "y")?, num(p, "w")?, num(p, "h")?);
    if w.is_some() || h.is_some() {
        let mut q = json!({});
        if let Some(o) = q.as_object_mut() {
            if let Some(w) = w {
                o.insert("w".into(), json!(w));
            }
            if let Some(h) = h {
                o.insert("h".into(), json!(h));
            }
        }
        steps.push(("size", base(q)));
    }
    if x.is_some() || y.is_some() {
        let mut q = json!({});
        if let Some(o) = q.as_object_mut() {
            if let Some(x) = x {
                o.insert("x".into(), json!(x));
            }
            if let Some(y) = y {
                o.insert("y".into(), json!(y));
            }
        }
        steps.push(("move", base(q)));
    }
    if let Some(r) = num(p, "rotation")? {
        steps.push(("rotate", base(json!({"degrees": r}))));
    }
    // Fill.
    let mut fill = json!({});
    if let Some(o) = fill.as_object_mut() {
        for k in
            ["transparency", "gradient", "stops", "angle", "radial", "picture", "path", "data_base64", "name"]
        {
            if let Some(v) = p.get(k).filter(|_| k != "name" || p.get("picture").is_some()) {
                o.insert(k.into(), v.clone());
            }
        }
        if let Some(v) = p.get("fill") {
            o.insert("color".into(), v.clone());
        }
        if opt_bool(p, "noFill") == Some(true) {
            o.insert("color".into(), Value::Null);
        }
    }
    if fill.as_object().is_some_and(|o| !o.is_empty()) {
        steps.push(("fill", base(fill)));
    }
    // Line.
    let mut line = json!({});
    if let Some(o) = line.as_object_mut() {
        if let Some(v) = p.get("outline") {
            o.insert("color".into(), v.clone());
        }
        if let Some(v) = p.get("outlineWidth") {
            o.insert("weight".into(), v.clone());
        }
        if let Some(v) = p.get("dash") {
            o.insert("dash".into(), v.clone());
        }
        if let Some(v) = p.get("lineTransparency") {
            o.insert("transparency".into(), v.clone());
        }
    }
    if line.as_object().is_some_and(|o| !o.is_empty()) {
        steps.push(("outline", base(line)));
    }
    if let Some(e) = p.get("effects").filter(|e| e.is_object()) {
        steps.push(("effect", base(e.clone())));
    }
    if let Some(e) = p.get("textEffects").filter(|e| e.is_object()) {
        steps.push(("textEffect", base(e.clone())));
    }
    if let Some(v) = p.get("wrap") {
        steps.push(("wrap", base(json!({"wrap": v}))));
    } else if let Some(b) = opt_bool(p, "behindText") {
        let action = if b { "behindText" } else { "inFrontOfText" };
        steps.push(("arrange", base(json!({"action": action}))));
    }
    // Text box and properties.
    let inset = match p.get("inset") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::Array(a)) if a.len() == 4 => {
            let mut v = [0.0f32; 4];
            for (slot, n) in v.iter_mut().zip(a) {
                *slot = n.as_f64().filter(|x| x.is_finite()).ok_or_else(|| {
                    CmdError::Params("`inset` is four numbers (left, top, right, bottom)".into())
                })? as f32;
            }
            Some(Some(v))
        }
        Some(_) => {
            return Err(CmdError::Params(
                "`inset` is four numbers (left, top, right, bottom) or null".into(),
            ));
        }
    };
    let autofit = opt_bool(p, "autofit");
    let valign = match opt_str(p, "valign") {
        None => None,
        Some(v @ ("top" | "middle" | "bottom")) => Some(v.to_owned()),
        Some(o) => return Err(CmdError::Params(format!("unknown vertical alignment `{o}`"))),
    };
    let alt = opt_str(p, "alt").map(|a| a.trim().chars().take(2000).collect::<String>());
    let txt = opt_str(p, "text").map(|t| t.replace("\r\n", "\n"));
    let name = if p.get("picture").is_some() {
        None
    } else {
        opt_str(p, "name").map(|n| n.trim().chars().take(80).collect::<String>())
    };
    let plan = Plan { steps, inset, autofit, valign, alt, txt, name };
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let saved = c.begin_steps();
    let result = apply(s, cid, &ids, &plan, &base);
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    c.end_steps(saved, result.is_ok());
    result?;
    let primary = ids.first().copied().unwrap_or(0);
    let d = find(&c.body.drawings, primary).cloned().unwrap_or_default();
    Ok(json!({"drawing": primary, "x": d.x, "y": d.y, "w": d.w, "h": d.h}))
}

/// What one Format Shape OK does, checked and ready to apply.
struct Plan<'a> {
    steps: Vec<(&'a str, Value)>,
    inset: Option<Option<[f32; 4]>>,
    autofit: Option<bool>,
    valign: Option<String>,
    alt: Option<String>,
    txt: Option<String>,
    name: Option<String>,
}

/// Applies `plan` to drawings `ids` of compose `cid` step by step; the first failure stops it (the caller
/// puts everything back).
fn apply(
    s: &mut Session,
    cid: u64,
    ids: &[u32],
    plan: &Plan<'_>,
    base: &dyn Fn(Value) -> Value,
) -> Result<(), CmdError> {
    for (step, q) in &plan.steps {
        match *step {
            "size" => super::size(s, q),
            "move" => super::move_to(s, q),
            "rotate" => super::rotate(s, q),
            "fill" => super::fill(s, q),
            "outline" => super::outline(s, q),
            "effect" => super::effect(s, q),
            "textEffect" => super::text_effect(s, q),
            "wrap" => super::wrap::wrap_text(s, q),
            _ => super::arrange(s, q),
        }?;
    }
    // Text box settings: the drawings that hold text (a group's members).
    if plan.inset.is_some() || plan.autofit.is_some() || plan.valign.is_some() || plan.txt.is_some() {
        super::change(s, &base(json!({})), |d, _| {
            if let Some(i) = plan.inset {
                d.inset = i;
            }
            if let Some(a) = plan.autofit {
                d.autofit = a;
            }
            if let Some(v) = &plan.valign {
                d.valign = match v.as_str() {
                    "middle" => VAlign::Middle,
                    "bottom" => VAlign::Bottom,
                    _ => VAlign::Top,
                };
            }
            if let Some(t) =
                plan.txt.as_ref().filter(|_| !matches!(d.kind, DrawingKind::Canvas | DrawingKind::Group))
            {
                d.text.clone_from(t);
            }
            Ok(())
        })?;
    }
    // Alt text and the name belong to the targets themselves (a group's to the group).
    if plan.alt.is_some() || plan.name.is_some() {
        let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
        let primary = ids.first().copied();
        let wants = |d: &Drawing| {
            let alt = plan.alt.as_ref().is_some_and(|a| ids.contains(&d.id) && d.alt != *a);
            let name = plan.name.as_ref().is_some_and(|n| Some(d.id) == primary && d.name != *n);
            alt || name
        };
        if c.body.drawings.iter().any(wants) {
            c.checkpoint();
            for d in &mut c.body.drawings {
                if let Some(a) = plan.alt.as_ref().filter(|_| ids.contains(&d.id)) {
                    d.alt.clone_from(a);
                }
                if let Some(n) = plan.name.as_ref().filter(|_| Some(d.id) == primary) {
                    d.name.clone_from(n);
                }
            }
            c.dirty = true;
        }
    }
    Ok(())
}
