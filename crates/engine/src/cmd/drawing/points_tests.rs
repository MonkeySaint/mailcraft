use super::super::super::compose::tests::{open, session};
use super::*;
use crate::cmd::insert::shapes::drawing_geometry;
use serde_json::json;

fn shape(s: &mut Session, cid: u64, kind: &str, w: f32, h: f32) -> u32 {
    let r = s
        .run("insert.shape", &json!({"compose": cid, "kind": kind, "x": 100, "y": 100, "w": w, "h": h}))
        .unwrap();
    r["drawing"].as_u64().unwrap() as u32
}

fn drawing(s: &Session, cid: u64, id: u32) -> Drawing {
    find(&s.composes[&cid].body.drawings, id).cloned().unwrap()
}

fn edit(s: &mut Session, cid: u64, id: u32, p: Value) -> Value {
    let mut q = p;
    q["compose"] = json!(cid);
    q["id"] = json!(id);
    s.run("drawing.editPoints", &q).unwrap()
}

/// The drawn outline of `d` in body px.
fn outline(d: &Drawing) -> Vec<(f32, f32)> {
    let g = drawing_geometry(&d.shape, d).unwrap();
    let path = g.fill.first().or_else(|| g.lines.first()).cloned().unwrap();
    path.into_iter().map(|(x, y)| (d.x + x, d.y + y)).collect()
}

/// Worst distance (as a fraction of the radius) of the outline from the ellipse in box (x, y, w, h).
fn off_ellipse(pts: &[(f32, f32)], b: (f32, f32, f32, f32)) -> f32 {
    let (rx, ry) = (b.2 / 2.0, b.3 / 2.0);
    let (cx, cy) = (b.0 + rx, b.1 + ry);
    pts.iter()
        .map(|(x, y)| ((((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2)).sqrt() - 1.0).abs())
        .fold(0.0, f32::max)
}

#[test]
fn edit_points_turns_a_shape_into_a_freeform_whose_points_move() {
    let mut s = session();
    let cid = open(&mut s);
    let id = shape(&mut s, cid, "triangle", 100.0, 100.0);
    let r = s.run("drawing.editPoints", &json!({"compose": cid, "on": true})).unwrap();
    assert_eq!(r["editing"], true);
    let d = drawing(&s, cid, id);
    assert_eq!(d.shape, "freeform");
    assert_eq!(d.points.len(), 3, "a triangle's three corners");
    assert!(d.handles.is_empty(), "straight sides only");
    assert_eq!(r["nodes"][0]["kind"], "corner");
    // Drag the top point up and out: the box grows to hold it.
    let r = edit(&mut s, cid, id, json!({"point": 0, "x": 160, "y": 60}));
    assert_eq!(r["points"][0], json!([160.0, 60.0]));
    let d = drawing(&s, cid, id);
    assert_eq!((d.y, d.h), (60.0, 140.0));
    // Add one between the first two, take it away again.
    edit(&mut s, cid, id, json!({"insert": 0}));
    assert_eq!(drawing(&s, cid, id).points.len(), 4);
    edit(&mut s, cid, id, json!({"delete": 1}));
    assert!(
        s.run("drawing.editPoints", &json!({"compose": cid, "id": id, "delete": 0})).is_err(),
        "three is the least"
    );
    assert!(s.run("drawing.editPoints", &json!({"compose": cid, "point": 9, "x": 1, "y": 1})).is_err());
    // Undo (the deleted point, the added one, then the drag) puts the point back where it was.
    for _ in 0..3 {
        s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
    }
    assert_eq!(drawing(&s, cid, id).y, 100.0);
    s.run("drawing.editPoints", &json!({"compose": cid, "on": false})).unwrap();
    assert_eq!(s.composes[&cid].edit_points, None);
    // WordArt has no points.
    let r = s
        .run("insert.wordArt", &json!({"compose": cid, "style": "Fill: Black, Text color 1", "text": "Hi"}))
        .unwrap();
    let wa = r["drawing"].as_u64().unwrap();
    assert!(s.run("drawing.editPoints", &json!({"compose": cid, "id": wa, "on": true})).is_err());
}

/// Release check: Edit Points sampled a preset's outline into at most 48 straight lines, so an oval became
/// a polygon. Its curves now stay curved segments.
#[test]
fn an_oval_keeps_its_curves_as_four_curved_segments() {
    let mut s = session();
    let cid = open(&mut s);
    let id = shape(&mut s, cid, "oval", 200.0, 80.0);
    let r = edit(&mut s, cid, id, json!({"on": true}));
    let d = drawing(&s, cid, id);
    assert_eq!(d.shape, "freeform");
    assert_eq!(d.points.len(), 4, "{:?}", d.points);
    assert_eq!(d.handles.len(), 4);
    assert!(d.handles.iter().all(|h| h.before.is_some() && h.after.is_some() && h.kind != PointKind::Corner));
    assert!(r["nodes"][0]["after"].is_array());
    // Still the same oval: the drawn outline stays on it, within half a percent.
    let worst = off_ellipse(&outline(&d), (100.0, 100.0, 200.0, 80.0));
    assert!(worst < 0.005, "off the oval by {worst}");
    assert!((d.x - 100.0).abs() < 0.5 && (d.w - 200.0).abs() < 1.0 && (d.h - 80.0).abs() < 1.0);
    // A point added on a curved segment splits it where it runs: the oval keeps its shape.
    edit(&mut s, cid, id, json!({"insert": 0}));
    let d = drawing(&s, cid, id);
    assert_eq!(d.points.len(), 5);
    assert!(off_ellipse(&outline(&d), (100.0, 100.0, 200.0, 80.0)) < 0.005);
}

#[test]
fn a_rounded_rectangle_has_straight_sides_and_curved_corners() {
    let mut s = session();
    let cid = open(&mut s);
    let id = shape(&mut s, cid, "roundedRectangle", 160.0, 100.0);
    edit(&mut s, cid, id, json!({"on": true}));
    let d = drawing(&s, cid, id);
    let (_, nodes, closed) = edit_outline(&d);
    assert!(closed);
    let segs: Vec<bool> = (0..freeform::segment_count(nodes.len(), true))
        .map(|i| freeform::segment(&nodes, i, true).unwrap().4)
        .collect();
    assert_eq!(segs.len(), 8, "{nodes:?}");
    assert_eq!(segs.iter().filter(|c| **c).count(), 4, "four curved corners");
    assert_eq!(segs.iter().filter(|c| !**c).count(), 4, "four straight sides");
}

#[test]
fn point_types_keep_or_free_the_handles() {
    let mut s = session();
    let cid = open(&mut s);
    let id = shape(&mut s, cid, "oval", 200.0, 100.0);
    let r = edit(&mut s, cid, id, json!({"on": true}));
    let node = |r: &Value, k: &str| -> (f32, f32) {
        let v = &r["nodes"][0][k];
        (v[0].as_f64().unwrap() as f32, v[1].as_f64().unwrap() as f32)
    };
    let at = node(&r, "at");
    let before = node(&r, "before");
    // A corner point: dragging one handle leaves the other where it was.
    edit(&mut s, cid, id, json!({"point": 0, "pointType": "corner"}));
    let r = edit(&mut s, cid, id, json!({"handle": 0, "side": "after", "x": at.0 + 30.0, "y": at.1 - 40.0}));
    assert_eq!(r["nodes"][0]["kind"], "corner");
    let b2 = node(&r, "before");
    assert!((b2.0 - before.0).abs() < 0.01 && (b2.1 - before.1).abs() < 0.01);
    // A smooth point: the other handle swings round to the opposite side, as long.
    let r = edit(&mut s, cid, id, json!({"point": 0, "pointType": "smooth"}));
    let (a, b, p) = (node(&r, "after"), node(&r, "before"), node(&r, "at"));
    let (va, vb) = ((a.0 - p.0, a.1 - p.1), (b.0 - p.0, b.1 - p.1));
    assert!((va.0 + vb.0).abs() < 0.05 && (va.1 + vb.1).abs() < 0.05, "mirrored: {va:?} {vb:?}");
    // A straight point: in line, but each keeps its own length.
    edit(&mut s, cid, id, json!({"point": 0, "pointType": "straight"}));
    let r = edit(&mut s, cid, id, json!({"handle": 0, "side": "before", "x": p.0 - 10.0, "y": p.1}));
    let (a, b) = (node(&r, "after"), node(&r, "before"));
    let la = ((a.0 - p.0).powi(2) + (a.1 - p.1).powi(2)).sqrt();
    let lva = ((va.0).powi(2) + (va.1).powi(2)).sqrt();
    assert!((la - lva).abs() < 0.05, "the after handle keeps its length");
    assert!((a.1 - p.1).abs() < 0.05 && a.0 > p.0, "and swings in line with the before handle");
    assert!((b.0 - (p.0 - 10.0)).abs() < 0.05);
    // Bad requests.
    let bad = |s: &mut Session, p: Value| {
        let mut q = p;
        q["compose"] = json!(cid);
        q["id"] = json!(id);
        s.run("drawing.editPoints", &q).is_err()
    };
    assert!(bad(&mut s, json!({"point": 0, "pointType": "wobbly"})));
    assert!(bad(&mut s, json!({"handle": 0, "side": "up", "x": 1, "y": 1})));
    assert!(bad(&mut s, json!({"handle": 99, "side": "after", "x": 1, "y": 1})));
    assert!(bad(&mut s, json!({"handle": 0, "side": "after"})));
    assert!(bad(&mut s, json!({"segment": 99, "segmentType": "curved"})));
    assert!(bad(&mut s, json!({"segmentType": "curved"})));
    assert!(bad(&mut s, json!({"point": 0})));
}

#[test]
fn segments_turn_curved_and_straight_and_the_box_follows_the_curve() {
    let mut s = session();
    let cid = open(&mut s);
    let id = shape(&mut s, cid, "rectangle", 100.0, 60.0);
    edit(&mut s, cid, id, json!({"on": true}));
    let d = drawing(&s, cid, id);
    assert!(d.handles.is_empty(), "a rectangle has straight sides");
    // The top side made curved, then bent upward by its handles.
    edit(&mut s, cid, id, json!({"segment": 0, "segmentType": "curved"}));
    let d = drawing(&s, cid, id);
    assert_eq!((d.y, d.h), (100.0, 60.0), "a new curved segment still runs straight");
    edit(&mut s, cid, id, json!({"handle": 0, "side": "after", "x": 130, "y": 40}));
    edit(&mut s, cid, id, json!({"handle": 1, "side": "before", "x": 170, "y": 40}));
    let d = drawing(&s, cid, id);
    assert!(d.y < 100.0 && d.y > 40.0, "the box holds the bulge, not the handles: {}", d.y);
    let top = outline(&d).iter().map(|p| p.1).fold(f32::MAX, f32::min);
    assert!((top - d.y).abs() < 0.5);
    // Straight again.
    edit(&mut s, cid, id, json!({"segment": 0, "segmentType": "straight"}));
    let d = drawing(&s, cid, id);
    assert!(d.handles.is_empty());
    assert_eq!((d.y, d.h), (100.0, 60.0));
}

#[test]
fn open_path_and_close_path() {
    let mut s = session();
    let cid = open(&mut s);
    let id = shape(&mut s, cid, "rectangle", 100.0, 60.0);
    edit(&mut s, cid, id, json!({"on": true}));
    let before = drawing(&s, cid, id).points;
    // Opening at the right side (segment 1): the path now runs from the bottom right corner round to the
    // top right one, and has no inside to fill.
    let r = edit(&mut s, cid, id, json!({"path": "open", "segment": 1}));
    assert_eq!(r["closed"], false);
    let d = drawing(&s, cid, id);
    assert_eq!(d.shape, "scribble");
    assert_eq!(d.points.len(), 4);
    assert_eq!(d.points.first(), before.get(2));
    assert_eq!(d.points.last(), before.get(1));
    let g = drawing_geometry(&d.shape, &d).unwrap();
    assert!(g.fill.is_empty() && g.lines.len() == 1);
    assert!(s.run("drawing.editPoints", &json!({"compose": cid, "id": id, "path": "open"})).is_err());
    // An open path's end points have no outer segment for a handle.
    assert!(
        s.run(
            "drawing.editPoints",
            &json!({"compose": cid, "id": id, "handle": 0, "side": "before", "x": 1, "y": 1})
        )
        .is_err()
    );
    // Closed again.
    let r = edit(&mut s, cid, id, json!({"path": "close"}));
    assert_eq!(r["closed"], true);
    assert_eq!(drawing(&s, cid, id).shape, "freeform");
    assert!(s.run("drawing.editPoints", &json!({"compose": cid, "id": id, "path": "close"})).is_err());
    assert!(s.run("drawing.editPoints", &json!({"compose": cid, "id": id, "path": "sideways"})).is_err());
}

/// Curve handles come back from drafts and files: hostile values must not crash drawing or editing.
#[test]
fn hostile_handles_are_cleaned_and_never_crash() {
    use mailcraft_model::rich::PointHandles;
    let mut s = session();
    let cid = open(&mut s);
    let id = shape(&mut s, cid, "oval", 100.0, 100.0);
    edit(&mut s, cid, id, json!({"on": true}));
    let c = s.composes.get_mut(&cid).unwrap();
    let d = c.body.drawings.iter_mut().find(|d| d.id == id).unwrap();
    d.handles =
        vec![
            PointHandles { before: Some((f32::NAN, 1.0)), after: Some((1e30, -1e30)), ..Default::default() };
            40
        ];
    let mut copy = d.clone();
    crate::cmd::insert::drawing::sanitize(&mut copy);
    assert!(copy.handles.len() <= copy.points.len());
    assert!(copy.handles.iter().all(|h| h.before.is_none()));
    assert!(copy.handles.iter().all(|h| h.after.is_none_or(|(x, y)| x.abs() < 20.0 && y.abs() < 20.0)));
    // Drawing and editing the unsanitized one do not panic either.
    let d = drawing(&s, cid, id);
    let _ = drawing_geometry(&d.shape, &d);
    let theme = s.composes[&cid].theme_resolved();
    let _ = crate::cmd::insert::drawing::raster(&d, &theme, true);
    let _ = s.run("drawing.editPoints", &json!({"compose": cid, "id": id, "point": 1, "x": 5, "y": 5}));
    let _ = s.run("drawing.editPoints", &json!({"compose": cid, "id": id, "insert": 0}));
}

/// Distance from `p` to the polyline `line` (closed: back to its start).
fn to_polyline(p: (f32, f32), line: &[(f32, f32)], closed: bool) -> f32 {
    let n = line.len();
    let segs = if closed { n } else { n.saturating_sub(1) };
    (0..segs)
        .map(|i| {
            let (a, b) = (line[i], line[(i + 1) % n]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l2 = (dx * dx + dy * dy).max(1e-9);
            let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0);
            ((a.0 + dx * t - p.0).powi(2) + (a.1 + dy * t - p.1).powi(2)).sqrt()
        })
        .fold(f32::MAX, f32::min)
}

/// Every gallery shape can be point-edited: no panic, quick, and the outline drawn from its points runs
/// where the shape's own did, whatever mix of corners, sides and curves the shape has.
#[test]
fn every_gallery_shape_turns_into_points_that_keep_its_outline() {
    let mut s = session();
    let cid = open(&mut s);
    let started = std::time::Instant::now();
    let mut curved = 0;
    for (kind, _, _) in shapes::SHAPES {
        let p = json!({"compose": cid, "kind": kind, "x": 50, "y": 50, "w": 180, "h": 120});
        let Ok(r) = s.run("insert.shape", &p) else { continue };
        let id = r["drawing"].as_u64().unwrap() as u32;
        let before = drawing(&s, cid, id);
        if s.run("drawing.editPoints", &json!({"compose": cid, "id": id, "on": true})).is_err() {
            continue;
        }
        let d = drawing(&s, cid, id);
        assert!(d.points.len() <= draw::MAX_POINTS, "{kind}");
        assert!(d.handles.is_empty() || d.handles.len() == d.points.len(), "{kind}");
        if !d.handles.is_empty() {
            curved += 1;
        }
        // The outline drawn from the points runs where the shape's own outline ran, both ways round.
        let g = shapes::geometry(kind, before.w, before.h, &before.points).unwrap();
        let main = g
            .fill
            .iter()
            .max_by_key(|p| p.len())
            .filter(|p| p.len() >= 3)
            .or_else(|| g.lines.iter().max_by_key(|p| p.len()));
        let old: Vec<(f32, f32)> = main.unwrap().iter().map(|(x, y)| (before.x + x, before.y + y)).collect();
        let new = outline(&d);
        let closed = d.shape == "freeform";
        let worst_new = new.iter().map(|p| to_polyline(*p, &old, closed)).fold(0.0, f32::max);
        let worst_old = old.iter().map(|p| to_polyline(*p, &new, closed)).fold(0.0, f32::max);
        // (A freeform, scribble or curve keeps the points it has.)
        if !matches!(*kind, "freeform" | "scribble" | "curve") {
            assert!(
                worst_new < 1.5 && worst_old < 1.5,
                "{kind}: off its outline by {worst_new} / {worst_old} ({} points, {} handles)",
                d.points.len(),
                d.handles.len()
            );
        }
        s.run("drawing.editPoints", &json!({"compose": cid, "id": id, "on": false})).unwrap();
        s.run("drawing.delete", &json!({"compose": cid, "ids": [id]})).unwrap();
    }
    assert!(curved >= 10, "only {curved} shapes kept curved segments");
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "took {:?}", started.elapsed());
}
