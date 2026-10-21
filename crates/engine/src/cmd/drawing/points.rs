//! Edit Points: the corner points of a freeform (or a scribble or curve) can be dragged, added and taken
//! away. A shape from the gallery becomes a freeform of its own outline when its points are edited.

use serde_json::{Value, json};

use super::{DrawingKind, change, find, num, opt_bool, target};
use crate::cmd::insert::drawing as draw;
use crate::cmd::insert::shapes;
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::rich::Drawing;

/// Most points a shape's outline turns into.
const MAX_EDIT_POINTS: usize = 48;

/// Whether a shape keeps editable points as it is.
fn editable(shape: &str) -> bool {
    matches!(shape, "freeform" | "scribble" | "curve")
}

/// The fewest points a shape keeps (a closed shape needs three).
fn min_points(shape: &str) -> usize {
    if shape == "freeform" { 3 } else { 2 }
}

/// Turns `d` into a freeform (closed shapes) or scribble (lines) of its own outline.
fn to_points(d: &mut Drawing) -> Result<(), CmdError> {
    match d.kind {
        DrawingKind::WordArt | DrawingKind::Canvas | DrawingKind::Group => {
            return Err(CmdError::Failed("only shapes and text boxes have points to edit".into()));
        }
        DrawingKind::TextBox => d.kind = DrawingKind::Shape,
        DrawingKind::Shape => {}
    }
    if editable(&d.shape) && d.points.len() >= min_points(&d.shape) {
        return Ok(());
    }
    let shape = if d.kind == DrawingKind::Shape && d.shape != "textBox" {
        d.shape.clone()
    } else {
        "rectangle".into()
    };
    let g = shapes::geometry(&shape, d.w, d.h, &d.points)
        .ok_or_else(|| CmdError::Failed("this shape has no points to edit".into()))?;
    let (w, h) = (d.w.max(1.0), d.h.max(1.0));
    let (path, closed) = match g.fill.iter().max_by_key(|p| p.len()) {
        Some(p) if p.len() >= 3 => (p.clone(), true),
        _ => (g.lines.iter().max_by_key(|p| p.len()).cloned().unwrap_or_default(), false),
    };
    if path.len() < 2 {
        return Err(CmdError::Failed("this shape has no points to edit".into()));
    }
    let step = path.len().div_ceil(MAX_EDIT_POINTS).max(1);
    d.points =
        path.iter().step_by(step).map(|(x, y)| ((x / w).clamp(0.0, 1.0), (y / h).clamp(0.0, 1.0))).collect();
    d.shape = if closed { "freeform".into() } else { "scribble".into() };
    Ok(())
}

/// Puts the points of `d` (in body px) back as fractions of a box that fits them.
fn refit(d: &mut Drawing, abs: &[(f32, f32)]) {
    let (x0, y0, x1, y1) = abs.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |a, p| {
        (a.0.min(p.0), a.1.min(p.1), a.2.max(p.0), a.3.max(p.1))
    });
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
        return;
    }
    let (w, h) = ((x1 - x0).max(draw::MIN_DRAWING_SIDE), (y1 - y0).max(draw::MIN_DRAWING_SIDE));
    d.x = x0;
    d.y = y0;
    d.w = w;
    d.h = h;
    d.points =
        abs.iter().map(|(x, y)| (((x - x0) / w).clamp(0.0, 1.0), ((y - y0) / h).clamp(0.0, 1.0))).collect();
}

/// The points of `d` in body px.
fn absolute(d: &Drawing) -> Vec<(f32, f32)> {
    d.points.iter().map(|(fx, fy)| (d.x + fx * d.w, d.y + fy * d.h)).collect()
}

fn index(p: &Value, key: &str) -> Result<Option<usize>, CmdError> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| CmdError::Params(format!("`{key}` must be a point number"))),
    }
}

/// Edit Points: `on` (true / false) starts or ends editing the points of the drawing (a gallery shape
/// becomes a freeform); `point` with `x`, `y` (body px) moves a point; `insert` adds a point after the
/// one given (at `x`, `y`, or halfway to the next); `delete` takes one away; `points` sets them all (as
/// fractions of the box).
pub(super) fn edit_points(s: &mut Session, p: &Value) -> CmdResult {
    let (cid, id) = target(s, p)?;
    let on = opt_bool(p, "on");
    let (point, insert, remove) = (index(p, "point")?, index(p, "insert")?, index(p, "delete")?);
    let (x, y) = (num(p, "x")?, num(p, "y")?);
    let all: Option<Vec<(f32, f32)>> = match p.get("points") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => {
            let mut v = Vec::new();
            for pt in a.iter().take(draw::MAX_POINTS) {
                let pair = pt
                    .as_array()
                    .and_then(|xy| Some((xy.first()?.as_f64()? as f32, xy.get(1)?.as_f64()? as f32)))
                    .filter(|(a, b)| a.is_finite() && b.is_finite())
                    .ok_or_else(|| CmdError::Params("`points` is a list of [x, y] fractions".into()))?;
                v.push((pair.0.clamp(0.0, 1.0), pair.1.clamp(0.0, 1.0)));
            }
            Some(v)
        }
        Some(_) => return Err(CmdError::Params("`points` is a list of [x, y] fractions".into())),
    };
    let editing = on.unwrap_or(true)
        && (on.is_some() || point.is_some() || insert.is_some() || remove.is_some() || all.is_some());
    if on.is_none() && point.is_none() && insert.is_none() && remove.is_none() && all.is_none() {
        // Without anything to do: toggles editing.
        let now = s.composes.get(&cid).and_then(|c| c.edit_points) == Some(id);
        let mut q = p.clone();
        if let Some(o) = q.as_object_mut() {
            o.insert("on".into(), json!(!now));
        }
        return edit_points(s, &q);
    }
    if on == Some(false) {
        let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
        c.edit_points = None;
        return Ok(json!({"drawing": id, "editing": false}));
    }
    let q = json!({"compose": cid, "id": id});
    let r = change(s, &q, |d, _| {
        to_points(d)?;
        let mut abs = absolute(d);
        let n = abs.len();
        if let Some(i) = point {
            let (Some(px), Some(py)) = (x, y) else {
                return Err(CmdError::Params("give the point's new `x` and `y`".into()));
            };
            let slot = abs.get_mut(i).ok_or_else(|| CmdError::Params(format!("no point {i}")))?;
            *slot = (px, py);
        }
        if let Some(i) = insert {
            if i >= n {
                return Err(CmdError::Params(format!("no point {i}")));
            }
            if n >= draw::MAX_POINTS {
                return Err(CmdError::Failed("this shape has as many points as it can hold".into()));
            }
            let a = abs.get(i).copied().unwrap_or_default();
            let b = abs.get((i + 1) % n).copied().unwrap_or(a);
            let at = match (x, y) {
                (Some(px), Some(py)) => (px, py),
                _ => ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0),
            };
            abs.insert(i + 1, at);
        }
        if let Some(i) = remove {
            if i >= n {
                return Err(CmdError::Params(format!("no point {i}")));
            }
            if n <= min_points(&d.shape) {
                return Err(CmdError::Failed("a shape needs more points than that".into()));
            }
            abs.remove(i);
        }
        if let Some(v) = &all {
            if v.len() < min_points(&d.shape) {
                return Err(CmdError::Failed("a shape needs more points than that".into()));
            }
            d.points.clone_from(v);
            abs = absolute(d);
        }
        refit(d, &abs);
        Ok(())
    })?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    if editing {
        c.edit_points = Some(id);
    }
    let pts = find(&c.body.drawings, id).map(absolute).unwrap_or_default();
    Ok(
        json!({"drawing": id, "editing": c.edit_points == Some(id), "points": pts, "changed": r.get("changed").cloned()}),
    )
}

#[cfg(test)]
mod tests {
    use super::super::super::compose::tests::{open, session};
    use super::*;

    #[test]
    fn edit_points_turns_a_shape_into_a_freeform_whose_points_move() {
        let mut s = session();
        let cid = open(&mut s);
        let r = s
            .run(
                "insert.shape",
                &json!({"compose": cid, "kind": "triangle", "x": 10, "y": 10, "w": 100, "h": 100}),
            )
            .unwrap();
        let id = r["drawing"].as_u64().unwrap() as u32;
        let r = s.run("drawing.editPoints", &json!({"compose": cid, "on": true})).unwrap();
        assert_eq!(r["editing"], true);
        let d = find(&s.composes[&cid].body.drawings, id).cloned().unwrap();
        assert_eq!(d.shape, "freeform");
        assert_eq!(d.points.len(), 3, "a triangle's three corners");
        // Drag the top point up and out: the box grows to hold it.
        let r = s.run("drawing.editPoints", &json!({"compose": cid, "point": 0, "x": 60, "y": -40})).unwrap();
        assert_eq!(r["points"][0], json!([60.0, -40.0]));
        let d = find(&s.composes[&cid].body.drawings, id).cloned().unwrap();
        assert_eq!((d.y, d.h), (-40.0, 150.0));
        // Add one between the first two, take it away again.
        s.run("drawing.editPoints", &json!({"compose": cid, "insert": 0})).unwrap();
        assert_eq!(find(&s.composes[&cid].body.drawings, id).unwrap().points.len(), 4);
        s.run("drawing.editPoints", &json!({"compose": cid, "delete": 1})).unwrap();
        assert!(
            s.run("drawing.editPoints", &json!({"compose": cid, "delete": 0})).is_err(),
            "three is the least"
        );
        assert!(s.run("drawing.editPoints", &json!({"compose": cid, "point": 9, "x": 1, "y": 1})).is_err());
        // Undo (the deleted point, the added one, then the drag) puts the point back where it was.
        for _ in 0..3 {
            s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
        }
        let d = find(&s.composes[&cid].body.drawings, id).cloned().unwrap();
        assert_eq!(d.y, 10.0);
        s.run("drawing.editPoints", &json!({"compose": cid, "on": false})).unwrap();
        assert_eq!(s.composes[&cid].edit_points, None);
        // WordArt has no points.
        let r = s
            .run(
                "insert.wordArt",
                &json!({"compose": cid, "style": "Fill: Black, Text color 1", "text": "Hi"}),
            )
            .unwrap();
        let wa = r["drawing"].as_u64().unwrap();
        assert!(s.run("drawing.editPoints", &json!({"compose": cid, "id": wa, "on": true})).is_err());
    }
}
