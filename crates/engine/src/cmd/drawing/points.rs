//! Edit Points: the points of a freeform (or a scribble or curve) can be dragged, added and taken away;
//! a point can be a smooth, straight or corner point, with curve handles that drag; a segment can be
//! straight or curved; the path can be opened at a segment or closed. A shape from the gallery becomes a
//! freeform of its own outline when its points are edited, its curves kept as curved segments.

use serde_json::{Value, json};

use super::{DrawingKind, change, find, num, opt_bool, target};
use crate::cmd::insert::drawing as draw;
use crate::cmd::insert::freeform::{self, Node};
use crate::cmd::insert::shapes;
use crate::cmd::util::opt_str;
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::rich::{Drawing, PointKind};

/// Most points a shape's outline turns into.
const MAX_EDIT_POINTS: usize = 128;

/// Whether a shape keeps editable points as it is.
fn editable(shape: &str) -> bool {
    matches!(shape, "freeform" | "scribble" | "curve")
}

/// The fewest points a path keeps (a closed one needs three).
fn min_points(closed: bool) -> usize {
    if closed { 3 } else { 2 }
}

fn is_closed(d: &Drawing) -> bool {
    d.shape == "freeform"
}

/// Turns `d` into a freeform (closed shapes) or scribble (lines) of its own outline: its corners become
/// corner points, its straight sides straight segments and its curves curved segments.
fn to_points(d: &mut Drawing) -> Result<(), CmdError> {
    match d.kind {
        DrawingKind::WordArt | DrawingKind::Canvas | DrawingKind::Group => {
            return Err(CmdError::Failed("only shapes and text boxes have points to edit".into()));
        }
        DrawingKind::TextBox => d.kind = DrawingKind::Shape,
        DrawingKind::Shape => {}
    }
    if editable(&d.shape) && d.points.len() >= min_points(is_closed(d)) {
        return Ok(());
    }
    let shape = if d.kind == DrawingKind::Shape && d.shape != "textBox" {
        d.shape.clone()
    } else {
        "rectangle".into()
    };
    let g = shapes::geometry(&shape, d.w, d.h, &d.points)
        .ok_or_else(|| CmdError::Failed("this shape has no points to edit".into()))?;
    let (path, closed) = match g.fill.iter().max_by_key(|p| p.len()) {
        Some(p) if p.len() >= 3 => (p.clone(), true),
        _ => (g.lines.iter().max_by_key(|p| p.len()).cloned().unwrap_or_default(), false),
    };
    if path.len() < 2 {
        return Err(CmdError::Failed("this shape has no points to edit".into()));
    }
    let mut nodes =
        match freeform::fit(&path, closed, MAX_EDIT_POINTS).filter(|n| n.len() >= min_points(closed)) {
            Some(nodes) => nodes,
            // Too many corners to keep them all: an even sample of the outline.
            None => {
                let step = path.len().div_ceil(MAX_EDIT_POINTS).max(1);
                path.iter().step_by(step).map(|p| Node::corner(*p)).collect()
            }
        };
    // Where the outline pokes out of the box (an arrowhead), the box grows to hold it rather than the
    // points being pulled in.
    for n in &mut nodes {
        n.translate(d.x, d.y);
    }
    if !store(d, &nodes, closed) {
        return Err(CmdError::Failed("this shape has no points to edit".into()));
    }
    d.shape = if closed { "freeform".into() } else { "scribble".into() };
    Ok(())
}

/// The points of `d` in body px, with their handles.
fn nodes_of(d: &Drawing) -> Vec<Node> {
    freeform::nodes_from(&d.points, &d.handles, d.x, d.y, d.w, d.h)
}

/// Puts `nodes` (body px) back into `d`, its box fitted round the drawn path.
fn store(d: &mut Drawing, nodes: &[Node], closed: bool) -> bool {
    let Some((x, y, w, h)) = freeform::bounds(nodes, closed) else { return false };
    let (w, h) = (w.max(draw::MIN_DRAWING_SIDE), h.max(draw::MIN_DRAWING_SIDE));
    if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
        return false;
    }
    // Rounding in following the curves must not creep the box a hair on every edit.
    let keep = |new: f32, old: f32| if (new - old).abs() < 0.01 { old } else { new };
    let (x, y, w, h) = (keep(x, d.x), keep(y, d.y), keep(w, d.w), keep(h, d.h));
    let (points, handles) = freeform::to_fractions(nodes, x, y, w, h);
    d.x = x;
    d.y = y;
    d.w = w;
    d.h = h;
    d.points = points;
    d.handles = handles;
    true
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

fn kind_name(k: PointKind) -> &'static str {
    match k {
        PointKind::Corner => "corner",
        PointKind::Smooth => "smooth",
        PointKind::Straight => "straight",
    }
}

fn node_json(n: &Node) -> Value {
    let pair = |p: Option<(f32, f32)>| p.map_or(Value::Null, |(x, y)| json!([x, y]));
    json!({"at": [n.at.0, n.at.1], "kind": kind_name(n.kind), "before": pair(n.before), "after": pair(n.after)})
}

/// What one `drawing.editPoints` call changes.
#[derive(Clone, Debug, Default)]
struct Ops {
    point: Option<usize>,
    point_kind: Option<PointKind>,
    handle: Option<(usize, bool)>,
    segment: Option<(usize, bool)>,
    insert: Option<usize>,
    remove: Option<usize>,
    path: Option<(bool, Option<usize>)>,
    all: Option<Vec<(f32, f32)>>,
    x: Option<f32>,
    y: Option<f32>,
}

impl Ops {
    fn parse(p: &Value) -> Result<Self, CmdError> {
        let point_kind = match opt_str(p, "pointType") {
            None => None,
            Some("smooth") => Some(PointKind::Smooth),
            Some("straight") => Some(PointKind::Straight),
            Some("corner") => Some(PointKind::Corner),
            Some(o) => {
                return Err(CmdError::Params(format!("unknown point type `{o}` (smooth, straight, corner)")));
            }
        };
        let handle = match index(p, "handle")? {
            None => None,
            Some(i) => match opt_str(p, "side") {
                Some("before") => Some((i, false)),
                Some("after") => Some((i, true)),
                _ => return Err(CmdError::Params("`side` is before or after".into())),
            },
        };
        let segment_index = index(p, "segment")?;
        let segment = match opt_str(p, "segmentType") {
            None => None,
            Some(t @ ("straight" | "curved")) => {
                let i = segment_index.ok_or_else(|| CmdError::Params("give the `segment` number".into()))?;
                Some((i, t == "curved"))
            }
            Some(o) => {
                return Err(CmdError::Params(format!("unknown segment type `{o}` (straight, curved)")));
            }
        };
        let path = match opt_str(p, "path") {
            None => None,
            Some("open") => Some((true, segment_index)),
            Some("close") => Some((false, None)),
            Some(o) => return Err(CmdError::Params(format!("unknown path change `{o}` (open, close)"))),
        };
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
        Ok(Ops {
            point: index(p, "point")?,
            point_kind,
            handle,
            segment,
            insert: index(p, "insert")?,
            remove: index(p, "delete")?,
            path,
            all,
            x: num(p, "x")?,
            y: num(p, "y")?,
        })
    }

    fn is_empty(&self) -> bool {
        self.point.is_none()
            && self.handle.is_none()
            && self.segment.is_none()
            && self.insert.is_none()
            && self.remove.is_none()
            && self.path.is_none()
            && self.all.is_none()
    }

    fn xy(&self) -> Option<(f32, f32)> {
        self.x.zip(self.y)
    }
}

fn no_point(i: usize) -> CmdError {
    CmdError::Params(format!("no point {i}"))
}

/// Applies `ops` to the path `nodes` (`closed`). Returns whether the path is closed afterwards.
fn apply(nodes: &mut Vec<Node>, mut closed: bool, ops: &Ops) -> Result<bool, CmdError> {
    let n = nodes.len();
    if let Some(i) = ops.point {
        if i >= n {
            return Err(no_point(i));
        }
        if ops.xy().is_none() && ops.point_kind.is_none() {
            return Err(CmdError::Params("give the point's new `x` and `y`, or a `pointType`".into()));
        }
        if let (Some((px, py)), Some(node)) = (ops.xy(), nodes.get_mut(i)) {
            let (dx, dy) = (px - node.at.0, py - node.at.1);
            node.translate(dx, dy);
        }
        if let Some(kind) = ops.point_kind {
            if let Some(node) = nodes.get_mut(i) {
                node.kind = kind;
            }
            if kind != PointKind::Corner {
                // A smooth or straight point gets handles in line with its neighbours.
                freeform::add_handles(nodes, i, closed);
                if let Some(node) = nodes.get_mut(i) {
                    freeform::constrain(node, true);
                }
            }
        }
    }
    if let Some((i, after)) = ops.handle {
        if i >= n {
            return Err(no_point(i));
        }
        let Some((px, py)) = ops.xy() else {
            return Err(CmdError::Params("give the handle's new `x` and `y`".into()));
        };
        if !closed && ((i == 0 && !after) || (i + 1 == n && after)) {
            return Err(CmdError::Params("there is no segment on that side of the point".into()));
        }
        if let Some(node) = nodes.get_mut(i) {
            if after {
                node.after = Some((px, py));
            } else {
                node.before = Some((px, py));
            }
            freeform::constrain(node, after);
        }
    }
    if let Some((i, curved)) = ops.segment {
        if i >= freeform::segment_count(n, closed) {
            return Err(CmdError::Params(format!("no segment {i}")));
        }
        let j = (i + 1) % n.max(1);
        let (a, b) = (nodes.get(i).map(|x| x.at), nodes.get(j).map(|x| x.at));
        let (Some(a), Some(b)) = (a, b) else { return Err(no_point(i)) };
        let was_curved = freeform::segment(nodes, i, closed).is_some_and(|s| s.4);
        if curved && !was_curved {
            // Handles on the line, a third of the way in from each end: drag them to bend it.
            if let Some(x) = nodes.get_mut(i) {
                x.after = Some((a.0 + (b.0 - a.0) / 3.0, a.1 + (b.1 - a.1) / 3.0));
            }
            if let Some(x) = nodes.get_mut(j) {
                x.before = Some((b.0 + (a.0 - b.0) / 3.0, b.1 + (a.1 - b.1) / 3.0));
            }
        } else if !curved {
            if let Some(x) = nodes.get_mut(i) {
                x.after = None;
            }
            if let Some(x) = nodes.get_mut(j) {
                x.before = None;
            }
        }
    }
    if let Some(i) = ops.insert {
        if i >= freeform::segment_count(n, closed) {
            return Err(CmdError::Params(format!("no segment after point {i}")));
        }
        if n >= draw::MAX_POINTS {
            return Err(CmdError::Failed("this shape has as many points as it can hold".into()));
        }
        let Some((p0, c1, c2, p3, curved)) = freeform::segment(nodes, i, closed) else {
            return Err(no_point(i));
        };
        let j = (i + 1) % n.max(1);
        if curved {
            // On a curved segment the new point splits the curve where it was asked for (or halfway),
            // so the outline keeps its shape.
            let t = ops.xy().map_or(0.5, |at| freeform::nearest_t(p0, c1, c2, p3, at));
            let (l1, l2, mid, r1, r2) = freeform::split_cubic(p0, c1, c2, p3, t);
            let keep = |h: (f32, f32), end: (f32, f32)| {
                ((h.0 - end.0).abs() + (h.1 - end.1).abs() > 1e-4).then_some(h)
            };
            if let Some(a) = nodes.get_mut(i) {
                a.after = keep(l1, p0);
            }
            if let Some(b) = nodes.get_mut(j) {
                b.before = keep(r2, p3);
            }
            let node = Node { at: mid, kind: PointKind::Smooth, before: Some(l2), after: Some(r1) };
            nodes.insert(i + 1, node);
        } else {
            let at = ops.xy().unwrap_or(((p0.0 + p3.0) / 2.0, (p0.1 + p3.1) / 2.0));
            nodes.insert(i + 1, Node::corner(at));
        }
    }
    if let Some(i) = ops.remove {
        let n = nodes.len();
        if i >= n {
            return Err(no_point(i));
        }
        if n <= min_points(closed) {
            return Err(CmdError::Failed("a shape needs more points than that".into()));
        }
        nodes.remove(i);
        if !closed {
            if let Some(first) = nodes.first_mut() {
                first.before = None;
            }
            if let Some(last) = nodes.last_mut() {
                last.after = None;
            }
        }
    }
    if let Some((open, at)) = ops.path {
        let n = nodes.len();
        if open {
            if !closed {
                return Err(CmdError::Failed("the path is open already".into()));
            }
            // Opening at segment `i` takes that segment away: the path then starts after it.
            let i = at.unwrap_or(n.saturating_sub(1));
            if i >= n {
                return Err(CmdError::Params(format!("no segment {i}")));
            }
            nodes.rotate_left((i + 1) % n.max(1));
            if let Some(first) = nodes.first_mut() {
                first.before = None;
            }
            if let Some(last) = nodes.last_mut() {
                last.after = None;
            }
            closed = false;
        } else {
            if closed {
                return Err(CmdError::Failed("the path is closed already".into()));
            }
            if n < min_points(true) {
                return Err(CmdError::Failed("a closed shape needs three points".into()));
            }
            closed = true;
        }
    }
    Ok(closed)
}

/// Edit Points: `on` (true / false) starts or ends editing the points of the drawing (a gallery shape
/// becomes a freeform); `point` with `x`, `y` (body px) moves a point and its handles, with `pointType`
/// (smooth, straight, corner) changes how the point joins its segments; `handle` with `side` (before,
/// after) and `x`, `y` drags a curve handle; `segment` with `segmentType` (straight, curved) changes a
/// segment; `insert` adds a point on the segment after the one given (at `x`, `y`, or halfway; a curved
/// segment is split where it runs); `delete` takes one away; `path` opens the path (at `segment`, else at
/// its closing segment) or closes it; `points` sets them all (as fractions of the box, all corners).
pub(super) fn edit_points(s: &mut Session, p: &Value) -> CmdResult {
    let (cid, id) = target(s, p)?;
    let on = opt_bool(p, "on");
    let ops = Ops::parse(p)?;
    let editing = on.unwrap_or(true) && (on.is_some() || !ops.is_empty());
    if on.is_none() && ops.is_empty() {
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
        if let Some(v) = &ops.all {
            if v.len() < min_points(is_closed(d)) {
                return Err(CmdError::Failed("a shape needs more points than that".into()));
            }
            d.points.clone_from(v);
            d.handles.clear();
        }
        let mut nodes = nodes_of(d);
        let closed = apply(&mut nodes, is_closed(d), &ops)?;
        if closed != is_closed(d) {
            d.shape = if closed { "freeform".into() } else { "scribble".into() };
        }
        if !store(d, &nodes, closed) {
            return Err(CmdError::Failed("the points cannot be placed there".into()));
        }
        Ok(())
    })?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    if editing {
        c.edit_points = Some(id);
    }
    let d = find(&c.body.drawings, id);
    let nodes = d.map(nodes_of).unwrap_or_default();
    let closed = d.is_some_and(is_closed);
    let pts: Vec<Value> = nodes.iter().map(|n| json!([n.at.0, n.at.1])).collect();
    Ok(json!({
        "drawing": id,
        "editing": c.edit_points == Some(id),
        "points": pts,
        "nodes": nodes.iter().map(node_json).collect::<Vec<_>>(),
        "closed": closed,
        "changed": r.get("changed").cloned(),
    }))
}

/// The drawn outline of a point-edited drawing, as fractions of its box (curved segments followed), and
/// its points with their handles (fractions too): what the editor draws over it.
pub fn edit_outline(d: &Drawing) -> (Vec<(f32, f32)>, Vec<Node>, bool) {
    let closed = is_closed(d);
    let nodes = freeform::nodes_from(&d.points, &d.handles, 0.0, 0.0, 1.0, 1.0);
    (freeform::flatten(&nodes, closed), nodes, closed)
}

#[cfg(test)]
#[path = "points_tests.rs"]
mod tests;
