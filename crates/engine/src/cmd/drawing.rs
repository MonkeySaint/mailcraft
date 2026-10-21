//! `drawing.*`: the floating drawing objects of the compose body (text boxes, shapes, WordArt, drawing
//! canvases, groups; see [`mailcraft_model::Drawing`]) and the Shape Format tab that edits them: select
//! (one or several), move and size (the editor's drag handles end in `drawing.move` / `drawing.size`), text,
//! Shape Fill (colours, gradients, pictures), Shape Outline (colour, weight, dashes), Shape Effects (with
//! galleries and options), Shape Quick Styles, Text Fill, Text Outline, Text Effects (with Transform), WordArt
//! Quick Styles, Text Direction, Align Text, Change Shape, Edit Points, Create Text Box Link, Bring Forward /
//! Send Backward, Wrap Text and Position, Align and Distribute, Group, Rotate and Flip, Alt Text, Format Shape,
//! Delete, and the drawing tool (the next drag in the body draws the armed shape, text box, canvas or
//! scribble).
//!
//! Every command acts on `{"id": n}` (or `{"ids": [..]}`) or, without it, on the selected drawings; each
//! change is one editor undo step. Formatting a group formats the drawings in it; moving, sizing, rotating
//! and deleting a group does so to all of it. Moving a drawing canvas moves the drawings drawn in it
//! (`Drawing::canvas`).

mod fx;
mod group;
mod link;
mod points;
mod props;
#[cfg(test)]
mod tests;
mod wrap;

pub use fx::{GLOW_SIZES, PRESETS, REFLECTIONS, ROTATIONS, SHADOWS, SOFT_EDGES, preset_of};
pub use link::{chain_head, flow_text};
pub use wrap::{inline_boxes, wrap_name};

use serde_json::{Value, json};

use super::compose::{compose_id, compose_mut, spec};
use super::insert::drawing::{self as draw, EFFECTS};
use super::insert::shapes;
use crate::cmd::util::{dialog, opt_bool, opt_str};
use crate::compose::ComposeState;
use crate::{CmdError, CmdResult, CommandSpec, Session};
use mailcraft_geom::Color;
use mailcraft_model::rich::{Align, Drawing, DrawingKind, VAlign, WrapStyle};

/// Deepest group nesting followed.
const MAX_NEST: usize = 32;

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

/// A drawing id parameter.
fn parse_id(v: &Value) -> Result<u32, CmdError> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|t| t.trim().parse().ok()))
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| CmdError::Params("`id` must be a drawing id".into()))
}

/// The drawings named by `id` or `ids`, if any.
fn explicit_ids(p: &Value) -> Result<Option<Vec<u32>>, CmdError> {
    if let Some(v) = p.get("id").filter(|v| !v.is_null()) {
        return Ok(Some(vec![parse_id(v)?]));
    }
    match p.get("ids") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for v in a.iter().take(draw::MAX_DRAWINGS) {
                let id = parse_id(v)?;
                if !out.contains(&id) {
                    out.push(id);
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err(CmdError::Params("`ids` must be a list of drawing ids".into())),
    }
}

pub fn find(ds: &[Drawing], id: u32) -> Option<&Drawing> {
    ds.iter().find(|d| d.id == id)
}

/// The drawings a command acts on: `id` / `ids`, else the selected drawings (the one picked last first).
pub(crate) fn targets(s: &Session, p: &Value) -> Result<(u64, Vec<u32>), CmdError> {
    let cid = compose_id(s, p)?;
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let ids = match explicit_ids(p)? {
        Some(ids) => ids,
        None => c.selected_drawings(),
    };
    if ids.is_empty() {
        return Err(CmdError::Disabled("select a drawing first".into()));
    }
    for id in &ids {
        if find(&c.body.drawings, *id).is_none() {
            return Err(CmdError::Params(format!("no drawing with id {id}")));
        }
    }
    Ok((cid, ids))
}

/// The drawing a command acts on alone: `id`, else the selected one.
fn target(s: &Session, p: &Value) -> Result<(u64, u32), CmdError> {
    let (cid, ids) = targets(s, p)?;
    let id = ids.first().copied().ok_or_else(|| CmdError::Disabled("select a drawing first".into()))?;
    Ok((cid, id))
}

/// The outermost group drawing `id` is in (itself when it is in none).
pub fn root_of(ds: &[Drawing], id: u32) -> u32 {
    let mut at = id;
    for _ in 0..MAX_NEST {
        match find(ds, at).and_then(|d| d.group).filter(|g| find(ds, *g).is_some()) {
            Some(g) if g != at => at = g,
            _ => break,
        }
    }
    at
}

/// Everything that goes with drawing `id` when it moves or is deleted: the members of a group (and of
/// groups in it) and the drawings drawn in a canvas. Not `id` itself.
pub fn descendants(ds: &[Drawing], id: u32) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let mut todo = vec![(id, 0usize)];
    while let Some((at, depth)) = todo.pop() {
        if depth > MAX_NEST || out.len() > draw::MAX_DRAWINGS {
            break;
        }
        let Some(d) = find(ds, at) else { continue };
        for o in ds {
            let member = (d.kind == DrawingKind::Group && o.group == Some(at))
                || (d.kind == DrawingKind::Canvas && o.canvas == Some(at));
            if member && o.id != id && !out.contains(&o.id) {
                out.push(o.id);
                todo.push((o.id, depth + 1));
            }
        }
    }
    out
}

/// `ids` with every group replaced by the drawings in it (what formatting applies to).
pub(crate) fn leaves(ds: &[Drawing], ids: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for id in ids {
        let Some(d) = find(ds, *id) else { continue };
        if d.kind == DrawingKind::Group {
            for m in descendants(ds, *id) {
                if find(ds, m).is_some_and(|x| x.kind != DrawingKind::Group) && !out.contains(&m) {
                    out.push(m);
                }
            }
        } else if !out.contains(id) {
            out.push(*id);
        }
    }
    out
}

/// The box around drawings `ids` (x0, y0, x1, y1).
pub(crate) fn bounds(ds: &[Drawing], ids: &[u32]) -> Option<(f32, f32, f32, f32)> {
    let mut b: Option<(f32, f32, f32, f32)> = None;
    for d in ids.iter().filter_map(|id| find(ds, *id)) {
        let r = (d.x, d.y, d.x + d.w, d.y + d.h);
        b = Some(match b {
            None => r,
            Some(o) => (o.0.min(r.0), o.1.min(r.1), o.2.max(r.2), o.3.max(r.3)),
        });
    }
    b
}

/// After drawings changed: groups take the box of their members (a group left with fewer than two members
/// dissolves), and links and group references to drawings that are gone are dropped.
pub(crate) fn tidy(ds: &mut Vec<Drawing>) {
    for _ in 0..MAX_NEST {
        let lonely = ds.iter().find(|g| {
            g.kind == DrawingKind::Group && ds.iter().filter(|m| m.group == Some(g.id)).count() < 2
        });
        let Some((gid, parent)) = lonely.map(|g| (g.id, g.group)) else { break };
        for m in ds.iter_mut().filter(|m| m.group == Some(gid)) {
            m.group = parent;
        }
        ds.retain(|d| d.id != gid);
    }
    let ids: Vec<u32> = ds.iter().map(|d| d.id).collect();
    let groups: Vec<u32> = ds.iter().filter(|d| d.kind == DrawingKind::Group).map(|d| d.id).collect();
    for d in ds.iter_mut() {
        if d.group.is_some_and(|g| !groups.contains(&g) || g == d.id) {
            d.group = None;
        }
        if d.link_next.is_some_and(|l| !ids.contains(&l) || l == d.id) {
            d.link_next = None;
        }
    }
    // Boxes from the innermost groups out.
    for _ in 0..MAX_NEST {
        let mut changed = false;
        for gid in &groups {
            let members: Vec<u32> = ds.iter().filter(|m| m.group == Some(*gid)).map(|m| m.id).collect();
            let Some((x0, y0, x1, y1)) = bounds(ds, &members) else { continue };
            let canvas = {
                let first = members.first().and_then(|m| find(ds, *m)).and_then(|m| m.canvas);
                members
                    .iter()
                    .all(|m| find(ds, *m).is_some_and(|d| d.canvas == first))
                    .then_some(first)
                    .flatten()
            };
            if let Some(g) = ds.iter_mut().find(|g| g.id == *gid) {
                let nb =
                    (x0, y0, (x1 - x0).max(draw::MIN_DRAWING_SIDE), (y1 - y0).max(draw::MIN_DRAWING_SIDE));
                if (g.x, g.y, g.w, g.h) != nb || g.canvas != canvas {
                    (g.x, g.y, g.w, g.h) = nb;
                    g.canvas = canvas;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// Moves drawings `ids` by (`dx`, `dy`).
pub(crate) fn shift(ds: &mut [Drawing], ids: &[u32], dx: f32, dy: f32) {
    for d in ds.iter_mut().filter(|d| ids.contains(&d.id)) {
        d.x += dx;
        d.y += dy;
        draw::sanitize(d);
    }
}

/// Applies `f` to every target drawing (groups: the drawings in them) as one undo step (no step when
/// nothing changed). A drawing `f` refuses is left alone; when it refuses all of them, its error is the
/// command's.
fn change(
    s: &mut Session,
    p: &Value,
    f: impl Fn(&mut Drawing, &ComposeState) -> Result<(), CmdError>,
) -> CmdResult {
    let (cid, ids) = targets(s, p)?;
    let explicit = explicit_ids(p)?.is_some();
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let theme = c.theme_resolved();
    let mut updated: Vec<Drawing> = Vec::new();
    let mut first_err: Option<CmdError> = None;
    let mut ok = 0usize;
    for id in leaves(&c.body.drawings, &ids) {
        let Some(before) = find(&c.body.drawings, id).cloned() else { continue };
        let mut d = before.clone();
        if let Err(e) = f(&mut d, c) {
            first_err.get_or_insert(e);
            continue;
        }
        ok += 1;
        draw::sanitize(&mut d);
        if d.kind == DrawingKind::WordArt {
            draw::fit_word_art(&mut d, &theme);
        }
        if d.autofit
            && let Some(h) = draw::fitted_height(&d, &theme)
        {
            d.h = h;
        }
        if d != before {
            updated.push(d);
        }
    }
    if ok == 0 {
        return Err(first_err.unwrap_or_else(|| CmdError::Disabled("select a drawing first".into())));
    }
    let changed = !updated.is_empty();
    if changed {
        c.checkpoint();
        for d in updated {
            let id = d.id;
            if let Some(slot) = c.body.drawings.iter_mut().find(|x| x.id == id) {
                *slot = d;
            }
            rejoin(c, id);
        }
        tidy(&mut c.body.drawings);
        // In-line drawings: their place in the text takes their new size.
        c.sync_inline_drawings();
        c.dirty = true;
    }
    let primary = ids.first().copied().unwrap_or(0);
    if explicit && !c.selected_drawings().contains(&primary) {
        c.select_drawing(Some(primary));
    }
    let d = find(&c.body.drawings, primary).cloned().unwrap_or_default();
    Ok(json!({"drawing": primary, "changed": changed, "x": d.x, "y": d.y, "w": d.w, "h": d.h}))
}

/// After drawing `id` (not a canvas) moved or changed size: it belongs to the canvas that now holds it, if
/// any (moved out of its canvas, it leaves it; moved into one, it joins it).
fn rejoin(c: &mut ComposeState, id: u32) {
    let Some(d) = find(&c.body.drawings, id) else { return };
    if d.wrap == WrapStyle::Inline {
        return;
    }
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
    let ids = match p.get("id") {
        Some(Value::Null) => Some(Vec::new()),
        _ => explicit_ids(p)?,
    };
    let (add, toggle, member) = (
        opt_bool(p, "add") == Some(true),
        opt_bool(p, "toggle") == Some(true),
        opt_bool(p, "member") == Some(true),
    );
    let c = compose_mut(s, p)?;
    let ids = ids.unwrap_or_default();
    for id in &ids {
        if find(&c.body.drawings, *id).is_none() {
            return Err(CmdError::Params(format!("no drawing with id {id}")));
        }
    }
    // A click on a drawing in a group picks the group (`member`: the drawing itself).
    let picked: Vec<u32> =
        ids.iter().map(|id| if member { *id } else { root_of(&c.body.drawings, *id) }).collect();
    let mut set = if add || toggle { c.selected_drawings() } else { Vec::new() };
    for id in picked {
        if toggle && set.contains(&id) {
            set.retain(|x| *x != id);
        } else {
            set.retain(|x| *x != id);
            set.insert(0, id);
        }
    }
    c.selected_drawing = set.first().copied();
    c.also_selected = set.get(1..).map(<[u32]>::to_vec).unwrap_or_default();
    if c.edit_points.is_some_and(|e| c.selected_drawing != Some(e)) {
        c.edit_points = None;
    }
    Ok(json!({"compose": cid, "selected": c.selected_drawing, "selection": set}))
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
        c.select_drawing(None);
        c.edit_points = None;
    }
    c.draw_tool.clone_from(&tool);
    Ok(json!({"armed": tool}))
}

fn list(s: &mut Session, p: &Value) -> CmdResult {
    let cid = compose_id(s, p)?;
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let drawings = serde_json::to_value(&c.body.drawings).unwrap_or(Value::Null);
    Ok(json!({"compose": cid, "drawings": drawings, "selected": c.selected_drawing,
              "selection": c.selected_drawings(), "tool": c.draw_tool, "editPoints": c.edit_points}))
}

fn move_to(s: &mut Session, p: &Value) -> CmdResult {
    let (x, y, dx, dy) = (num(p, "x")?, num(p, "y")?, num(p, "dx")?, num(p, "dy")?);
    if x.is_none() && y.is_none() && dx.is_none() && dy.is_none() {
        return Err(CmdError::Params("give `x` / `y` or `dx` / `dy`".into()));
    }
    let (cid, ids) = targets(s, p)?;
    let explicit = explicit_ids(p)?.is_some();
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let primary = ids.first().copied().unwrap_or(0);
    let Some(d) = find(&c.body.drawings, primary).cloned() else {
        return Err(CmdError::Params(format!("no drawing with id {primary}")));
    };
    if d.wrap == WrapStyle::Inline {
        // In line with the text: it moves to the place in the text it is dropped on.
        let (ix, iy) = wrap::inline_box(c, primary).map_or((d.x, d.y), |b| (b.0, b.1));
        let at = (x.unwrap_or(ix) + dx.unwrap_or(0.0), y.unwrap_or(iy) + dy.unwrap_or(0.0));
        let moved = wrap::move_inline(c, primary, at);
        return Ok(json!({"drawing": primary, "changed": moved, "inline": true}));
    }
    let mut probe = d.clone();
    probe.x = x.unwrap_or(d.x) + dx.unwrap_or(0.0);
    probe.y = y.unwrap_or(d.y) + dy.unwrap_or(0.0);
    draw::sanitize(&mut probe);
    let (ox, oy) = (probe.x - d.x, probe.y - d.y);
    if explicit {
        let keep = c.selected_drawings();
        if !keep.contains(&primary) {
            c.select_drawing(Some(primary));
        }
    }
    if ox == 0.0 && oy == 0.0 {
        return Ok(json!({"drawing": primary, "changed": false, "x": d.x, "y": d.y}));
    }
    // Each target with what goes with it (group members, a canvas's drawings); in-line drawings stay.
    let mut moving: Vec<u32> = Vec::new();
    for id in &ids {
        if find(&c.body.drawings, *id).is_some_and(|t| t.wrap == WrapStyle::Inline) {
            continue;
        }
        for m in std::iter::once(*id).chain(descendants(&c.body.drawings, *id)) {
            if !moving.contains(&m) {
                moving.push(m);
            }
        }
    }
    c.checkpoint();
    shift(&mut c.body.drawings, &moving, ox, oy);
    for id in &moving {
        if find(&c.body.drawings, *id).is_some_and(|d| d.kind != DrawingKind::Canvas) {
            rejoin(c, *id);
        }
    }
    tidy(&mut c.body.drawings);
    c.dirty = true;
    let d = find(&c.body.drawings, primary).cloned().unwrap_or_default();
    Ok(json!({"drawing": primary, "changed": true, "x": d.x, "y": d.y, "moved": moving.len()}))
}

/// Scales group `gid`'s members from its old box to `to` (x, y, w, h).
fn scale_group(c: &mut ComposeState, gid: u32, to: (f32, f32, f32, f32)) {
    let Some(g) = find(&c.body.drawings, gid).cloned() else { return };
    let (kx, ky) = (to.2 / g.w.max(1.0), to.3 / g.h.max(1.0));
    let members = descendants(&c.body.drawings, gid);
    let theme = c.theme_resolved();
    for d in c.body.drawings.iter_mut().filter(|d| members.contains(&d.id)) {
        d.x = to.0 + (d.x - g.x) * kx;
        d.y = to.1 + (d.y - g.y) * ky;
        d.w *= kx;
        d.h *= ky;
        if d.kind == DrawingKind::WordArt {
            let pt = d.text_style.size_pt.unwrap_or(36.0);
            d.text_style.size_pt = Some((pt * kx.min(ky)).clamp(4.0, 400.0));
            draw::fit_word_art(d, &theme);
        }
        draw::sanitize(d);
    }
}

fn size(s: &mut Session, p: &Value) -> CmdResult {
    let (w, h, x, y) = (num(p, "w")?, num(p, "h")?, num(p, "x")?, num(p, "y")?);
    if w.is_none() && h.is_none() && x.is_none() && y.is_none() {
        return Err(CmdError::Params("give `w` and / or `h` (and optionally `x`, `y`)".into()));
    }
    let keep = opt_bool(p, "keepRatio").unwrap_or(false);
    let (cid, ids) = targets(s, p)?;
    // Groups scale what is in them.
    let groups: Vec<u32> = {
        let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
        ids.iter()
            .copied()
            .filter(|id| find(&c.body.drawings, *id).is_some_and(|d| d.kind == DrawingKind::Group))
            .collect()
    };
    if !groups.is_empty() {
        let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
        c.checkpoint();
        for gid in &groups {
            let Some(g) = find(&c.body.drawings, *gid).cloned() else { continue };
            let (ow, oh) = (g.w.max(1.0), g.h.max(1.0));
            let (mut nw, mut nh) = (w.unwrap_or(g.w), h.unwrap_or(g.h));
            if keep {
                match (w, h) {
                    (Some(_), None) => nh = oh * nw / ow,
                    (None, Some(_)) => nw = ow * nh / oh,
                    _ => {}
                }
            }
            let to = (
                x.unwrap_or(g.x),
                y.unwrap_or(g.y),
                nw.clamp(draw::MIN_DRAWING_SIDE, draw::MAX_DRAWING_SIDE),
                nh.clamp(draw::MIN_DRAWING_SIDE, draw::MAX_DRAWING_SIDE),
            );
            scale_group(c, *gid, to);
        }
        tidy(&mut c.body.drawings);
        c.dirty = true;
        let primary = ids.first().copied().unwrap_or(0);
        let d = find(&c.body.drawings, primary).cloned().unwrap_or_default();
        if ids.iter().all(|id| groups.contains(id)) {
            return Ok(json!({"drawing": primary, "changed": true, "x": d.x, "y": d.y, "w": d.w, "h": d.h}));
        }
    }
    let rest: Vec<Value> = ids.iter().filter(|id| !groups.contains(id)).map(|id| json!(id)).collect();
    let mut q = p.clone();
    if let Some(o) = q.as_object_mut() {
        o.remove("id");
        o.insert("ids".into(), Value::Array(rest));
    }
    change(s, &q, |d, _| {
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
    // Linked text boxes share one text: it belongs to the first box of the chain.
    let (cid, id) = target(s, p)?;
    let head = s.composes.get(&cid).map_or(id, |c| link::chain_head(&c.body.drawings, id));
    let mut q = p.clone();
    if let Some(o) = q.as_object_mut() {
        o.insert("id".into(), json!(head));
        o.remove("ids");
    }
    change(s, &q, |d, _| {
        if matches!(d.kind, DrawingKind::Canvas | DrawingKind::Group) {
            return Err(CmdError::Failed("a drawing canvas or group holds drawings, not text".into()));
        }
        if d.kind == DrawingKind::WordArt && d.alt == d.text {
            d.alt.clone_from(&t);
        }
        d.text.clone_from(&t);
        Ok(())
    })
}

/// The drawing whose formatting the Shape Format galleries show for compose `c`: the selected drawing, or
/// for a group the first drawing in it (a group's format is its members').
pub fn format_source(c: &ComposeState) -> Option<&Drawing> {
    let id = c.selected_drawing?;
    let leaf = leaves(&c.body.drawings, &[id]).first().copied().unwrap_or(id);
    find(&c.body.drawings, leaf).or_else(|| find(&c.body.drawings, id))
}

fn current(s: &Session, p: &Value) -> Result<Drawing, CmdError> {
    let (cid, id) = target(s, p)?;
    let c = s.composes.get(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    // A group's format is its first drawing's.
    let id = leaves(&c.body.drawings, &[id]).first().copied().unwrap_or(id);
    find(&c.body.drawings, id).cloned().ok_or_else(|| CmdError::Params(format!("no drawing with id {id}")))
}

fn fill(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "custom") == Some(true) {
        let cur = current(s, p)?.fill;
        return color_dialog(s, p, "shapeFill", cur);
    }
    if opt_bool(p, "moreGradients") == Some(true) {
        return props::format_dialog(s, p, "fill");
    }
    let picture = props::picture_param(p)?;
    let gradient = match p.get("gradient") {
        None | Some(Value::Null) => None,
        Some(Value::String(g)) if draw::valid_gradient(g) => Some(g.clone()),
        Some(other) => {
            return Err(CmdError::Params(format!(
                "unknown gradient {other} (down, up, right, left, center, downRight, downLeft, upRight, upLeft, each also as dark:<direction>, or \"\" for a solid fill)"
            )));
        }
    };
    let stops = props::gradient_param(p)?;
    let transparency = props::unit_param(p, "transparency")?;
    let col = color_choice(s, p)?;
    if col.is_none() && gradient.is_none() && stops.is_none() && picture.is_none() && transparency.is_none() {
        return Err(CmdError::Params(
            "give `color` (null for No Fill), `theme` and `tint`, `gradient`, `stops`, `picture` or `transparency`"
                .into(),
        ));
    }
    change(s, p, |d, c| {
        if matches!(d.kind, DrawingKind::WordArt)
            && (gradient.as_deref().is_some_and(|g| !g.is_empty()) || stops.is_some())
        {
            return Err(CmdError::Failed("WordArt takes its gradient from its style".into()));
        }
        if let Some(col) = col {
            d.fill = col;
            d.gradient_fill = None;
            d.fill_picture.clear();
            if col.is_none() {
                d.gradient.clear();
            }
        }
        if let Some(g) = &gradient {
            if !g.is_empty() && d.fill.is_none() {
                // A gradient needs a colour to shade: the theme's first accent.
                d.fill = Some(c.theme_resolved().color(crate::compose::ACCENT1));
            }
            d.gradient.clone_from(g);
            d.gradient_fill = None;
            d.fill_picture.clear();
        }
        if let Some(gf) = &stops {
            d.gradient_fill = gf.clone();
            if d.fill.is_none() {
                d.fill = Some(c.theme_resolved().color(crate::compose::ACCENT1));
            }
            d.fill_picture.clear();
        }
        if let Some(pic) = &picture {
            d.fill_picture.clone_from(pic);
            if !pic.is_empty() {
                d.gradient_fill = None;
                d.gradient.clear();
                if d.fill.is_none() {
                    d.fill = Some(Color::WHITE);
                }
            }
        }
        if let Some(t) = transparency {
            d.transparency = t;
        }
        Ok(())
    })
}

/// Text Outline: a colour (null = none) drawn around the letters of the text, its weight and dashes.
fn text_outline(s: &mut Session, p: &Value) -> CmdResult {
    if opt_bool(p, "custom") == Some(true) {
        let cur = current(s, p)?.text_outline;
        return color_dialog(s, p, "textOutline", cur);
    }
    let col = color_choice(s, p)?;
    let weight = num(p, "weight")?;
    let dash = props::dash_param(p)?;
    if col.is_none() && weight.is_none() && dash.is_none() {
        return Err(CmdError::Params(
            "give `color` (null for No Outline), `theme` and `tint`, `weight` or `dash`".into(),
        ));
    }
    change(s, p, |d, c| {
        if matches!(d.kind, DrawingKind::Canvas | DrawingKind::Group) {
            return Err(CmdError::Failed("a drawing canvas has no text".into()));
        }
        if let Some(col) = col {
            d.text_outline = col;
        }
        let needs = weight.is_some_and(|w| w > 0.0) || dash.as_deref().is_some_and(|x| !x.is_empty());
        if needs && d.text_outline.is_none() {
            d.text_outline = Some(c.theme_resolved().color(0));
        }
        if let Some(w) = weight {
            d.text_outline_px = w;
        }
        if let Some(x) = &dash {
            d.text_outline_dash.clone_from(x);
        }
        Ok(())
    })
}

/// Text Effects: one effect on the letters (`""` gives WordArt back its style's effects), effects with
/// options (as Shape Effects), and Transform (`transform`: a warp of the text).
fn text_effect(s: &mut Session, p: &Value) -> CmdResult {
    let legacy = opt_str(p, "effect").map(str::to_owned);
    if let Some(e) = &legacy
        && !draw::TEXT_EFFECTS.contains(&e.as_str())
    {
        return Err(CmdError::Params(format!(
            "unknown text effect `{e}` (none, shadow, glow, reflection, bevel, or \"\" for the WordArt style's)"
        )));
    }
    let transform = opt_str(p, "transform").map(str::to_owned);
    if let Some(t) = &transform
        && !draw::WARPS.iter().any(|(id, _)| id == t)
    {
        return Err(CmdError::Params(format!("unknown transform `{t}`")));
    }
    let options = fx::has_options(p);
    if legacy.is_none() && transform.is_none() && !options {
        return Err(CmdError::Params("give `effect`, `transform` or effect options".into()));
    }
    change(s, p, |d, c| {
        match d.kind {
            DrawingKind::Canvas | DrawingKind::Group => {
                return Err(CmdError::Failed("a drawing canvas has no text".into()));
            }
            DrawingKind::WordArt => {}
            _ if legacy.as_deref() == Some("reflection")
                || p.get("reflection").is_some_and(|v| !v.is_null()) =>
            {
                return Err(CmdError::Failed("only WordArt text can have a reflection".into()));
            }
            _ => {}
        }
        if let Some(e) = &legacy {
            d.text_effect.clone_from(e);
            d.text_fx = None;
        }
        if options {
            let mut t = d.text_fx.as_deref().cloned().unwrap_or_else(|| fx::from_legacy(&d.text_effect));
            let accent = c.theme_resolved().color(crate::compose::ACCENT1);
            fx::apply(&mut t, p, c, accent)?;
            d.text_fx = Some(Box::new(t));
            if d.text_effect.is_empty() {
                // Options replace a WordArt style's own effects.
                d.text_effect = "none".into();
            }
        }
        if let Some(t) = &transform {
            d.warp.clone_from(t);
        }
        Ok(())
    })
}

/// Text Direction: the text of a text box or shape runs left to right, top to bottom, bottom to top, or
/// stacked one letter under another.
fn text_direction(s: &mut Session, p: &Value) -> CmdResult {
    let dir = match (opt_str(p, "direction"), p.get("vertical").and_then(Value::as_bool)) {
        (Some(d @ ("horizontal" | "down" | "up" | "stacked")), _) => d.to_owned(),
        (Some(other), _) => {
            return Err(CmdError::Params(format!(
                "unknown direction `{other}` (horizontal, down, up, stacked)"
            )));
        }
        (None, Some(true)) => "down".into(),
        (None, Some(false)) => "horizontal".into(),
        (None, None) => {
            let d = current(s, p)?;
            if d.vertical || !d.text_flow.is_empty() { "horizontal".into() } else { "down".into() }
        }
    };
    change(s, p, |d, _| {
        if matches!(d.kind, DrawingKind::Canvas | DrawingKind::WordArt | DrawingKind::Group) {
            return Err(CmdError::Failed("only text boxes and shapes can turn their text".into()));
        }
        d.vertical = dir == "down";
        d.text_flow = if matches!(dir.as_str(), "up" | "stacked") { dir.clone() } else { String::new() };
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
        if matches!(d.kind, DrawingKind::Canvas | DrawingKind::WordArt | DrawingKind::Group) {
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
        d.gradient_fill = None;
        d.fill_picture.clear();
        d.fx = None;
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
    let dash = props::dash_param(p)?;
    let transparency = props::unit_param(p, "transparency")?;
    if col.is_none() && weight.is_none() && dash.is_none() && transparency.is_none() {
        return Err(CmdError::Params(
            "give `color` (null for No Outline), `theme` and `tint`, `weight`, `dash` or `transparency`"
                .into(),
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
        if let Some(x) = &dash {
            d.line_dash.clone_from(x);
            if d.outline.is_none() && !x.is_empty() {
                d.outline = Some(Color::BLACK);
            }
        }
        if let Some(t) = transparency {
            d.line_transparency = t;
        }
        Ok(())
    })
}

/// Shape Effects: a preset effect (`effect`, `""` = the theme's), or effects with options (`shadow`,
/// `reflection`, `glow`, `softEdges`, `bevel`, `rotation3d`, `preset`; see [`fx::apply`]).
fn effect(s: &mut Session, p: &Value) -> CmdResult {
    let legacy = opt_str(p, "effect").map(str::to_owned);
    if let Some(e) = &legacy
        && !EFFECTS.contains(&e.as_str())
    {
        return Err(CmdError::Params(format!(
            "unknown effect `{e}` (none, shadow, glow, reflection, bevel, softEdges, or \"\" for the theme's)"
        )));
    }
    let options = fx::has_options(p);
    if legacy.is_none() && !options {
        return Err(CmdError::Params("give `effect` or effect options".into()));
    }
    change(s, p, |d, c| {
        if d.kind == DrawingKind::Group {
            return Err(CmdError::Failed("a group has no effects of its own".into()));
        }
        if let Some(e) = &legacy {
            d.effect.clone_from(e);
            d.fx = None;
        }
        if options {
            let mut f = d.fx.as_deref().cloned().unwrap_or_else(|| fx::from_legacy(&d.effect));
            let accent =
                d.fill.or(d.outline).unwrap_or_else(|| c.theme_resolved().color(crate::compose::ACCENT1));
            fx::apply(&mut f, p, c, accent)?;
            // Options take over from the theme's effect style.
            d.effect = "none".into();
            d.fx = (!f.is_empty()).then(|| Box::new(f));
        }
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
        if matches!(d.kind, DrawingKind::Canvas | DrawingKind::Group) {
            return Err(CmdError::Failed("a drawing canvas has no text".into()));
        }
        if let Some(f) = &family {
            d.text_style.font_family = Some(f.clone());
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
        if matches!(d.kind, DrawingKind::Canvas | DrawingKind::Group) {
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
        d.word_art.clone_from(&style);
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
            DrawingKind::Canvas | DrawingKind::WordArt | DrawingKind::Group => {
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
        if matches!(d.wrap, WrapStyle::Tight | WrapStyle::Through) {
            d.wrap_points = wrap::outline_points(d);
        }
        Ok(())
    })
}

/// Moves drawing `id` (with what goes with it) in the back-to-front order: `bringForward`,
/// `bringToFront`, `sendBackward`, `sendToBack`; `behindText` / `inFrontOfText` put it behind or in front
/// of the body text. One undo step; returns whether anything changed.
pub(crate) fn arrange_id(s: &mut Session, cid: u64, id: u32, action: &str) -> Result<bool, CmdError> {
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let changed = arrange_in(c, &[id], action)?;
    Ok(changed)
}

fn arrange_in(c: &mut ComposeState, ids: &[u32], action: &str) -> Result<bool, CmdError> {
    if !matches!(
        action,
        "bringForward" | "bringToFront" | "sendBackward" | "sendToBack" | "behindText" | "inFrontOfText"
    ) {
        return Err(CmdError::Params(format!(
            "unknown arrangement `{action}` (bringForward, bringToFront, sendBackward, sendToBack, behindText, inFrontOfText)"
        )));
    }
    let before = c.body.drawings.clone();
    let mut list = c.body.drawings.clone();
    let mut inline_out: Vec<u32> = Vec::new();
    // Front moves go front-most first, back moves back-most first, so the targets keep their order.
    let mut order: Vec<u32> = ids.to_vec();
    let pos = |l: &[Drawing], id: u32| l.iter().position(|d| d.id == id).unwrap_or(0);
    order.sort_by_key(|id| pos(&list, *id));
    if matches!(action, "bringForward" | "bringToFront") {
        order.reverse();
    }
    for id in order {
        let block: Vec<u32> = std::iter::once(id).chain(descendants(&list, id)).collect();
        match action {
            "behindText" | "inFrontOfText" => {
                for d in list.iter_mut().filter(|d| block.contains(&d.id)) {
                    d.behind_text = action == "behindText";
                    if d.id == id && d.wrap != WrapStyle::Front {
                        if d.wrap == WrapStyle::Inline {
                            inline_out.push(d.id);
                        }
                        d.wrap = WrapStyle::Front;
                    }
                }
            }
            _ => {
                let at = list.iter().position(|d| block.contains(&d.id)).unwrap_or(0);
                let (moving, mut rest): (Vec<Drawing>, Vec<Drawing>) =
                    list.into_iter().partition(|d| block.contains(&d.id));
                let to = match action {
                    "bringToFront" => rest.len(),
                    "sendToBack" => 0,
                    // Past the next drawing (and what goes with it) in that direction.
                    "bringForward" => {
                        let mut k = at;
                        if let Some(next) = rest.get(k) {
                            let skip = std::iter::once(next.id).chain(descendants(&rest, next.id)).count();
                            k = (k + skip).min(rest.len());
                        }
                        k
                    }
                    _ => at.saturating_sub(1),
                };
                let to = to.min(rest.len());
                rest.splice(to..to, moving);
                list = rest;
            }
        }
    }
    let changed = list != before;
    if changed {
        // In line with the text before: the drawing stays where it was shown.
        let boxes: Vec<(u32, (f32, f32, f32, f32))> =
            inline_out.iter().filter_map(|id| wrap::inline_box(c, *id).map(|b| (*id, b))).collect();
        c.checkpoint();
        c.body.drawings = list;
        for (id, b) in boxes {
            if let Some(d) = c.body.drawings.iter_mut().find(|d| d.id == id) {
                (d.x, d.y) = (b.0, b.1);
            }
            crate::compose::remove_placeholders(&mut c.body.blocks, id);
        }
        c.dirty = true;
    }
    Ok(changed)
}

fn arrange(s: &mut Session, p: &Value) -> CmdResult {
    let action = opt_str(p, "action").unwrap_or("").to_owned();
    let (cid, ids) = targets(s, p)?;
    let explicit = explicit_ids(p)?.is_some();
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let changed = arrange_in(c, &ids, &action)?;
    let primary = ids.first().copied().unwrap_or(0);
    if explicit {
        c.select_drawing(Some(primary));
    }
    let order: Vec<u32> = c.body.drawings.iter().map(|d| d.id).collect();
    Ok(json!({"drawing": primary, "changed": changed, "order": order}))
}

/// Turns drawings `ids` by `by` degrees (or to `degrees`); groups turn as one, about their middle.
fn rotate(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(flip) = opt_str(p, "flip") {
        return group::flip(s, p, flip);
    }
    let by = num(p, "by")?;
    let to = num(p, "degrees")?;
    if by.is_none() && to.is_none() {
        return Err(CmdError::Params("give `by` (degrees to turn) or `degrees`, or `flip`".into()));
    }
    let (cid, ids) = targets(s, p)?;
    let has_group = s.composes.get(&cid).is_some_and(|c| {
        ids.iter().any(|id| find(&c.body.drawings, *id).is_some_and(|d| d.kind == DrawingKind::Group))
    });
    if has_group {
        return group::rotate(s, p, by, to);
    }
    change(s, p, |d, _| {
        d.rotation = to.unwrap_or(d.rotation) + by.unwrap_or(0.0);
        Ok(())
    })
}

fn delete(s: &mut Session, p: &Value) -> CmdResult {
    let (cid, ids) = targets(s, p)?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    // A group goes with its members, a canvas with what was drawn in it.
    let mut gone: Vec<u32> = Vec::new();
    for id in &ids {
        for g in std::iter::once(*id).chain(descendants(&c.body.drawings, *id)) {
            if !gone.contains(&g) {
                gone.push(g);
            }
        }
    }
    let order: Vec<u32> = c.body.drawings.iter().map(|d| d.id).filter(|id| gone.contains(id)).collect();
    c.checkpoint();
    for id in &order {
        crate::compose::remove_placeholders(&mut c.body.blocks, *id);
    }
    c.body.drawings.retain(|o| !gone.contains(&o.id));
    tidy(&mut c.body.drawings);
    c.select_drawing(None);
    if c.edit_points.is_some_and(|e| gone.contains(&e)) {
        c.edit_points = None;
    }
    c.dirty = true;
    Ok(json!({"deleted": order}))
}

fn alt_text(s: &mut Session, p: &Value) -> CmdResult {
    let Some(alt) = opt_str(p, "alt") else {
        let (cid, id) = target(s, p)?;
        let alt = s
            .composes
            .get(&cid)
            .and_then(|c| find(&c.body.drawings, id))
            .map(|d| d.alt.clone())
            .unwrap_or_default();
        return dialog(s, "drawingAltText", json!({"compose": cid, "id": id, "alt": alt}));
    };
    let alt: String = alt.trim().chars().take(2000).collect();
    let (cid, ids) = targets(s, p)?;
    // Alt text describes the object picked: a group keeps its own.
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let changed = ids.iter().any(|id| find(&c.body.drawings, *id).is_some_and(|d| d.alt != alt));
    if changed {
        c.checkpoint();
        for d in c.body.drawings.iter_mut().filter(|d| ids.contains(&d.id)) {
            d.alt.clone_from(&alt);
        }
        c.dirty = true;
    }
    if explicit_ids(p)?.is_some() {
        c.select_drawing(ids.first().copied());
    }
    Ok(json!({"drawing": ids.first(), "changed": changed}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "drawing.select",
            "Select Drawing",
            "Compose body (click a drawing; Shift- or Ctrl-click adds one; drag around drawings)",
            "",
            r#"{"id"?:number|null,"ids"?:[number],"add"?:bool,"toggle"?:bool,"member"?:bool}  (null selects none; a drawing in a group selects the group unless member)"#,
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
            r#"{"id"?:number,"ids"?:[number],"x"?:number,"y"?:number,"dx"?:number,"dy"?:number}  (several drawings move together)"#,
            true,
            has_drawings,
            move_to,
        ),
        spec(
            "drawing.size",
            "Size",
            "Shape Format › Size (Height, Width; drag a handle)",
            "",
            r#"{"id"?:number,"w"?:number|string,"h"?:number|string,"x"?:number,"y"?:number,"keepRatio"?:bool}  ("120 px" typed in the box works)"#,
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
            r#"{"id"?:number,"color"?:string|null,"theme"?:string,"tint"?:number,"gradient"?:string,"stops"?:[{"pos":number,"color":string}],"angle"?:number,"radial"?:bool,"picture"?:bool|null,"path"?:string,"data_base64"?:string,"transparency"?:number,"custom"?:bool,"moreGradients"?:bool}  (null = No Fill)"#,
            true,
            has_drawings,
            fill,
        ),
        spec(
            "drawing.outline",
            "Shape Outline",
            "Shape Format › Shape Styles",
            "",
            r#"{"id"?:number,"color"?:string|null,"theme"?:string,"tint"?:number,"weight"?:number,"dash"?:string,"transparency"?:number,"custom"?:bool}  (null = No Outline)"#,
            true,
            has_drawings,
            outline,
        ),
        spec(
            "drawing.effect",
            "Shape Effects",
            "Shape Format › Shape Styles",
            "",
            r#"{"id"?:number,"effect"?:""|"none"|"shadow"|"glow"|"reflection"|"bevel"|"softEdges","shadow"?:string|object|null,"reflection"?:string|object|null,"glow"?:object|null,"softEdges"?:number,"bevel"?:string,"bevelSize"?:number,"rotation3d"?:string|object|null,"preset"?:number}"#,
            true,
            has_drawings,
            effect,
        ),
        spec(
            "drawing.textOutline",
            "Text Outline",
            "Shape Format › WordArt Styles",
            "",
            r#"{"id"?:number,"color"?:string|null,"theme"?:string,"tint"?:number,"weight"?:number,"dash"?:string,"custom"?:bool}  (null = No Outline)"#,
            true,
            has_drawings,
            text_outline,
        ),
        spec(
            "drawing.textEffect",
            "Text Effects",
            "Shape Format › WordArt Styles",
            "",
            r#"{"id"?:number,"effect"?:""|"none"|"shadow"|"glow"|"reflection"|"bevel","transform"?:string,"shadow"?:string|object|null,"reflection"?:string|object|null,"glow"?:object|null,"bevel"?:string,"rotation3d"?:string|object|null}  (reflection: WordArt)"#,
            true,
            has_drawings,
            text_effect,
        ),
        spec(
            "drawing.textDirection",
            "Text Direction",
            "Shape Format › Text",
            "",
            r#"{"id"?:number,"direction"?:"horizontal"|"down"|"up"|"stacked","vertical"?:bool}  (nothing: turns it the other way)"#,
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
            r#"{"id"?:number,"family"?:string,"size"?:number|string,"bold"?:bool,"italic"?:bool,"underline"?:bool}"#,
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
            "drawing.editPoints",
            "Edit Points",
            "Shape Format › Insert Shapes › Edit Shape",
            "",
            r#"{"id"?:number,"on"?:bool,"point"?:number,"x"?:number,"y"?:number,"insert"?:number,"delete"?:number,"points"?:[[number,number]]}  (on: drag the corner points; x, y in the body)"#,
            true,
            has_drawings,
            points::edit_points,
        ),
        spec(
            "drawing.linkTextBox",
            "Create Text Box Link",
            "Shape Format › Text",
            "",
            r#"{"id"?:number,"to"?:number,"break"?:bool}  (no to: the next click on an empty text box links it)"#,
            true,
            has_drawings,
            link::link_text_box,
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
            "drawing.wrapText",
            "Wrap Text",
            "Shape Format › Arrange",
            "",
            r#"{"id"?:number,"wrap":"inline"|"square"|"tight"|"through"|"topBottom"|"behind"|"front"}"#,
            true,
            has_drawings,
            wrap::wrap_text,
        ),
        spec(
            "drawing.position",
            "Position",
            "Command only (CLI, MCP, control channel): not on the Shape Format tab",
            "",
            r#"{"id"?:number,"at":"inline"|"topLeft"|"topCenter"|"topRight"|"middleLeft"|"middleCenter"|"middleRight"|"bottomLeft"|"bottomCenter"|"bottomRight"}  (with text wrapping)"#,
            true,
            has_drawings,
            wrap::position,
        ),
        spec(
            "drawing.align",
            "Align",
            "Shape Format › Arrange",
            "",
            r#"{"id"?:number,"ids"?:[number],"kind"?:"left"|"center"|"right"|"top"|"middle"|"bottom","relative"?:"margin"|"selected"}  (several selected: to each other unless Align to Margin)"#,
            true,
            has_drawings,
            group::align,
        ),
        spec(
            "drawing.distribute",
            "Distribute",
            "Shape Format › Arrange › Align",
            "",
            r#"{"ids"?:[number],"axis":"horizontal"|"vertical"}  (three or more selected: between the outer ones; else across the margins)"#,
            true,
            has_drawings,
            group::distribute,
        ),
        spec(
            "drawing.group",
            "Group Objects",
            "Shape Format › Arrange",
            "",
            r#"{"ids"?:[number],"id"?:number,"action"?:"group"|"ungroup"|"regroup"}  (group: the selected drawings)"#,
            true,
            has_drawings,
            group::group,
        ),
        spec(
            "drawing.rotate",
            "Rotate",
            "Shape Format › Arrange",
            "",
            r#"{"id"?:number,"by"?:number,"degrees"?:number,"flip"?:"horizontal"|"vertical"}"#,
            true,
            has_drawings,
            rotate,
        ),
        spec(
            "drawing.delete",
            "Delete Drawing",
            "Compose body (Delete with a drawing selected)",
            "",
            r#"{"id"?:number,"ids"?:[number]}"#,
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
            "Format Shape",
            "Shape Format › Shape Styles / Size (dialog launchers); Size and Position",
            "",
            r#"{"id"?:number,"page"?:"fill"|"effects"|"layout"|"size","x"?,"y"?,"w"?,"h"?,"rotation"?,"fill"?:string|null,"transparency"?,"gradient"?,"stops"?,"angle"?,"radial"?,"picture"?,"outline"?:string|null,"outlineWidth"?,"dash"?,"lineTransparency"?,"effects"?:{..drawing.effect},"textEffects"?:{..drawing.textEffect},"inset"?:[l,t,r,b]|null,"autofit"?:bool,"valign"?,"wrap"?,"behindText"?,"alt"?,"text"?,"name"?}  (nothing: the Format Shape pane)"#,
            true,
            has_drawings,
            props::format,
        ),
    ]
}
