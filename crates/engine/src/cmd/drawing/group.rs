//! Several drawings at once: Group / Ungroup / Regroup, Align (to the margins, or to each other with
//! several selected), Distribute Horizontally / Vertically, and turning or flipping a group as one.

use serde_json::{Value, json};

use super::{DrawingKind, bounds, descendants, explicit_ids, find, opt_str, rejoin, shift, targets, tidy};
use crate::cmd::insert::drawing as draw;
use crate::compose::ComposeState;
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::rich::{Drawing, WrapStyle};

/// The width and height drawings are aligned in (the text area, at least as tall as the text and the
/// drawings).
fn page(c: &ComposeState, ds: &[u32]) -> (f32, f32) {
    let width = c.view_width.filter(|w| w.is_finite() && *w > 40.0).unwrap_or(draw::DEFAULT_VIEW_WIDTH);
    let lo = mailcraft_layout::LayoutOptions { width, ..Default::default() };
    let fonts = mailcraft_fonts::FontDb::bundled();
    let text_h = mailcraft_layout::layout_doc(&c.body, &fonts, &lo).height;
    let low = bounds(&c.body.drawings, ds).map_or(0.0, |b| b.3);
    (width, text_h.max(low))
}

/// Moves `id` and what goes with it by (`dx`, `dy`).
fn shift_tree(c: &mut ComposeState, id: u32, dx: f32, dy: f32) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    let ids: Vec<u32> = std::iter::once(id).chain(descendants(&c.body.drawings, id)).collect();
    shift(&mut c.body.drawings, &ids, dx, dy);
    for m in &ids {
        if find(&c.body.drawings, *m).is_some_and(|d| d.kind != DrawingKind::Canvas) {
            rejoin(c, *m);
        }
    }
}

/// The floating drawings among `ids` (in-line drawings sit in the text and do not align).
fn floating(c: &ComposeState, ids: &[u32]) -> Vec<u32> {
    ids.iter()
        .copied()
        .filter(|id| find(&c.body.drawings, *id).is_some_and(|d| d.wrap != WrapStyle::Inline))
        .collect()
}

/// Align: `kind` left / center / right / top / middle / bottom. With several drawings selected they align
/// to each other (to the box around them), else (or with Align to Margin) to the margins. `relative`
/// alone switches between the two.
pub(super) fn align(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(rel) = opt_str(p, "relative") {
        let to_margin = match rel {
            "margin" => true,
            "selected" => false,
            other => return Err(CmdError::Params(format!("unknown `relative` {other} (margin, selected)"))),
        };
        let c = crate::cmd::compose::compose_mut(s, p)?;
        c.align_to_margin = to_margin;
        if p.get("kind").is_none() {
            return Ok(json!({"alignToMargin": to_margin}));
        }
    }
    let kind = opt_str(p, "kind").unwrap_or("").to_owned();
    if !matches!(kind.as_str(), "left" | "center" | "right" | "top" | "middle" | "bottom") {
        return Err(CmdError::Params(format!(
            "unknown alignment `{kind}` (left, center, right, top, middle, bottom)"
        )));
    }
    let (cid, ids) = targets(s, p)?;
    let explicit = explicit_ids(p)?.is_some();
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let ids = floating(c, &ids);
    if ids.is_empty() {
        return Err(CmdError::Failed("a drawing in line with the text moves with the text".into()));
    }
    let to_each_other = ids.len() >= 2 && !c.align_to_margin;
    let (x0, y0, x1, y1) = if to_each_other {
        bounds(&c.body.drawings, &ids).unwrap_or_default()
    } else {
        let (w, h) = page(c, &ids);
        (0.0, 0.0, w, h)
    };
    let moves: Vec<(u32, f32, f32)> = ids
        .iter()
        .filter_map(|id| find(&c.body.drawings, *id))
        .map(|d| {
            let (nx, ny) = match kind.as_str() {
                "left" => (x0, d.y),
                "center" => ((x0 + x1 - d.w) / 2.0, d.y),
                "right" => (x1 - d.w, d.y),
                "top" => (d.x, y0),
                "middle" => (d.x, (y0 + y1 - d.h) / 2.0),
                _ => (d.x, y1 - d.h),
            };
            // To the margins, nothing goes past the top-left corner.
            let (nx, ny) = if to_each_other { (nx, ny) } else { (nx.max(0.0), ny.max(0.0)) };
            (d.id, nx - d.x, ny - d.y)
        })
        .collect();
    let changed = moves.iter().any(|(_, dx, dy)| *dx != 0.0 || *dy != 0.0);
    if changed {
        c.checkpoint();
        for (id, dx, dy) in &moves {
            shift_tree(c, *id, *dx, *dy);
        }
        tidy(&mut c.body.drawings);
        c.dirty = true;
    }
    if explicit && ids.len() == 1 {
        c.select_drawing(ids.first().copied());
    }
    let d = ids.first().and_then(|id| find(&c.body.drawings, *id)).cloned().unwrap_or_default();
    Ok(json!({"drawing": d.id, "changed": changed, "x": d.x, "y": d.y, "toEachOther": to_each_other}))
}

/// Distribute Horizontally / Vertically: equal gaps between the drawings. With three or more selected
/// (and Align Selected Objects) they spread between the outer two; otherwise across the margins.
pub(super) fn distribute(s: &mut Session, p: &Value) -> CmdResult {
    let horizontal = match opt_str(p, "axis") {
        Some("horizontal") => true,
        Some("vertical") => false,
        other => {
            return Err(CmdError::Params(format!("`axis` is horizontal or vertical, not {other:?}")));
        }
    };
    let (cid, ids) = targets(s, p)?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let ids = floating(c, &ids);
    if ids.is_empty() {
        return Err(CmdError::Failed("a drawing in line with the text moves with the text".into()));
    }
    let mut items: Vec<Drawing> = ids.iter().filter_map(|id| find(&c.body.drawings, *id).cloned()).collect();
    let key = |d: &Drawing| if horizontal { d.x + d.w / 2.0 } else { d.y + d.h / 2.0 };
    items.sort_by(|a, b| key(a).total_cmp(&key(b)));
    let size = |d: &Drawing| if horizontal { d.w } else { d.h };
    let start = |d: &Drawing| if horizontal { d.x } else { d.y };
    let between = items.len() >= 3 && !c.align_to_margin;
    let (lo, hi) = if between {
        let lo = items.iter().map(start).fold(f32::MAX, f32::min);
        let hi = items.iter().map(|d| start(d) + size(d)).fold(f32::MIN, f32::max);
        (lo, hi)
    } else {
        let (w, h) = page(c, &ids);
        (0.0, if horizontal { w } else { h })
    };
    let total: f32 = items.iter().map(size).sum();
    let n = items.len();
    let moves: Vec<(u32, f32)> = if n == 1 {
        // One drawing goes in the middle.
        items.iter().map(|d| (d.id, (lo + hi - size(d)) / 2.0 - start(d))).collect()
    } else {
        let gap = (hi - lo - total) / (n - 1) as f32;
        let mut at = lo;
        items
            .iter()
            .map(|d| {
                let m = (d.id, at - start(d));
                at += size(d) + gap;
                m
            })
            .collect()
    };
    let changed = moves.iter().any(|(_, v)| v.abs() > 0.01);
    if changed {
        c.checkpoint();
        for (id, v) in &moves {
            let (dx, dy) = if horizontal { (*v, 0.0) } else { (0.0, *v) };
            shift_tree(c, *id, dx, dy);
        }
        tidy(&mut c.body.drawings);
        c.dirty = true;
    }
    Ok(json!({"changed": changed, "betweenSelected": between, "count": n}))
}

/// Checks that drawings `ids` can go into a new group (before anything changes).
fn check_group(ds: &[Drawing], ids: &[u32]) -> Result<(), CmdError> {
    if ids.len() < 2 {
        return Err(CmdError::Failed("select two or more drawings to group them".into()));
    }
    let mut canvas = None;
    for (k, id) in ids.iter().enumerate() {
        let d = find(ds, *id).ok_or_else(|| CmdError::Params(format!("no drawing with id {id}")))?;
        if d.kind == DrawingKind::Canvas {
            return Err(CmdError::Failed("a drawing canvas cannot be grouped".into()));
        }
        if d.wrap == WrapStyle::Inline {
            return Err(CmdError::Failed(
                "a drawing in line with the text cannot be grouped; change its wrapping first".into(),
            ));
        }
        if k == 0 {
            canvas = d.canvas;
        } else if d.canvas != canvas {
            return Err(CmdError::Failed("drawings in and out of a drawing canvas cannot be grouped".into()));
        }
    }
    if ds.len() >= draw::MAX_DRAWINGS {
        return Err(CmdError::Failed(format!("a message holds at most {} drawings", draw::MAX_DRAWINGS)));
    }
    Ok(())
}

/// Puts drawings `ids` into a new group; returns its id.
fn make_group(c: &mut ComposeState, ids: &[u32]) -> Result<u32, CmdError> {
    check_group(&c.body.drawings, ids)?;
    let ds = &c.body.drawings;
    let gid = draw::next_id(&c.body);
    let (x0, y0, x1, y1) = bounds(ds, ids).unwrap_or_default();
    let first = ids.iter().filter_map(|id| find(ds, *id)).next().cloned().unwrap_or_default();
    // Every member is in the same canvas (or none): the group is too.
    let canvas = first.canvas;
    // The group takes over its members' wrapping and place in front of / behind the text.
    let wrap = if first.wrap.wraps() { first.wrap } else { WrapStyle::Front };
    let behind = ids.iter().all(|id| find(ds, *id).is_some_and(|d| d.behind_text));
    let parent = ids.iter().filter_map(|id| find(ds, *id)).map(|d| d.group).next().flatten();
    let same_parent = ids.iter().all(|id| find(ds, *id).is_some_and(|d| d.group == parent));
    let group = Drawing {
        id: gid,
        kind: DrawingKind::Group,
        shape: "rectangle".into(),
        x: x0,
        y: y0,
        w: (x1 - x0).max(draw::MIN_DRAWING_SIDE),
        h: (y1 - y0).max(draw::MIN_DRAWING_SIDE),
        fill: None,
        outline: None,
        effect: "none".into(),
        wrap,
        behind_text: behind,
        canvas,
        group: if same_parent { parent } else { None },
        alt: "Group".into(),
        ..Drawing::default()
    };
    // The group sits just behind its back-most member.
    let at = c.body.drawings.iter().position(|d| ids.contains(&d.id)).unwrap_or(0);
    c.body.drawings.insert(at, group);
    for d in c.body.drawings.iter_mut().filter(|d| ids.contains(&d.id)) {
        d.group = Some(gid);
        d.wrap = WrapStyle::Front;
        d.behind_text = behind;
    }
    Ok(gid)
}

/// Group Objects: `group` (the selected drawings, two or more), `ungroup` (a selected group: its members
/// are selected), `regroup` (the drawings last ungrouped).
pub(super) fn group(s: &mut Session, p: &Value) -> CmdResult {
    let action = opt_str(p, "action").unwrap_or("group").to_owned();
    match action.as_str() {
        "group" => {
            let (cid, ids) = targets(s, p)?;
            let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
            let tops: Vec<u32> = {
                let ds = &c.body.drawings;
                let mut out: Vec<u32> = Vec::new();
                for id in &ids {
                    let r = super::root_of(ds, *id);
                    // A drawing picked inside a group stands for the group.
                    let pick = if ids.contains(&r) || r == *id { *id } else { r };
                    if !out.contains(&pick) {
                        out.push(pick);
                    }
                }
                out
            };
            // A refused group changes nothing (no undo step, the redo history stays).
            check_group(&c.body.drawings, &tops)?;
            let snapshot = c.body.drawings.clone();
            c.checkpoint();
            match make_group(c, &tops) {
                Ok(gid) => {
                    tidy(&mut c.body.drawings);
                    c.select_drawing(Some(gid));
                    c.dirty = true;
                    Ok(json!({"group": gid, "members": tops}))
                }
                Err(e) => {
                    // Nothing changed: take the undo step back.
                    c.body.drawings = snapshot;
                    c.edit_undo.pop();
                    Err(e)
                }
            }
        }
        "ungroup" => {
            let (cid, ids) = targets(s, p)?;
            let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
            let groups: Vec<u32> = ids
                .iter()
                .copied()
                .filter(|id| find(&c.body.drawings, *id).is_some_and(|d| d.kind == DrawingKind::Group))
                .collect();
            if groups.is_empty() {
                return Err(CmdError::Failed("select a group to ungroup".into()));
            }
            c.checkpoint();
            let mut members: Vec<u32> = Vec::new();
            for gid in &groups {
                let Some(g) = find(&c.body.drawings, *gid).cloned() else { continue };
                for d in c.body.drawings.iter_mut().filter(|d| d.group == Some(*gid)) {
                    d.group = g.group;
                    d.wrap = g.wrap;
                    d.behind_text = g.behind_text;
                    members.push(d.id);
                }
                c.body.drawings.retain(|d| d.id != *gid);
            }
            tidy(&mut c.body.drawings);
            c.selected_drawing = members.first().copied();
            c.also_selected = members.get(1..).map(<[u32]>::to_vec).unwrap_or_default();
            c.last_ungrouped.clone_from(&members);
            c.dirty = true;
            Ok(json!({"ungrouped": groups, "members": members}))
        }
        "regroup" => {
            let cid = super::compose_id(s, p)?;
            let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
            let ids: Vec<u32> = c
                .last_ungrouped
                .iter()
                .copied()
                .filter(|id| find(&c.body.drawings, *id).is_some_and(|d| d.group.is_none()))
                .collect();
            if ids.len() < 2 {
                return Err(CmdError::Failed("there is no group to put back together".into()));
            }
            check_group(&c.body.drawings, &ids)?;
            let snapshot = c.body.drawings.clone();
            c.checkpoint();
            match make_group(c, &ids) {
                Ok(gid) => {
                    tidy(&mut c.body.drawings);
                    c.select_drawing(Some(gid));
                    c.last_ungrouped.clear();
                    c.dirty = true;
                    Ok(json!({"group": gid, "members": ids}))
                }
                Err(e) => {
                    c.body.drawings = snapshot;
                    c.edit_undo.pop();
                    Err(e)
                }
            }
        }
        other => Err(CmdError::Params(format!("unknown action `{other}` (group, ungroup, regroup)"))),
    }
}

/// Rotate with a group among the targets: a group turns about its middle, carrying its members.
pub(super) fn rotate(s: &mut Session, p: &Value, by: Option<f32>, to: Option<f32>) -> CmdResult {
    let (cid, ids) = targets(s, p)?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    c.checkpoint();
    for id in &ids {
        let Some(t) = find(&c.body.drawings, *id).cloned() else { continue };
        let delta = match to {
            Some(deg) => deg - t.rotation,
            None => by.unwrap_or(0.0),
        };
        let delta = if delta.is_finite() { delta } else { 0.0 };
        let (cx, cy) = (t.x + t.w / 2.0, t.y + t.h / 2.0);
        let (sn, cs) = delta.to_radians().sin_cos();
        let members: Vec<u32> = std::iter::once(*id).chain(descendants(&c.body.drawings, *id)).collect();
        for d in c.body.drawings.iter_mut().filter(|d| members.contains(&d.id)) {
            d.rotation += delta;
            if d.id != *id {
                // Members circle the group's middle.
                let (mx, my) = (d.x + d.w / 2.0 - cx, d.y + d.h / 2.0 - cy);
                let (nx, ny) = (mx * cs - my * sn, mx * sn + my * cs);
                d.x = cx + nx - d.w / 2.0;
                d.y = cy + ny - d.h / 2.0;
            }
            draw::sanitize(d);
        }
    }
    // The group keeps the angle it was turned to; its box follows its members.
    let angles: Vec<(u32, f32)> =
        ids.iter().filter_map(|id| find(&c.body.drawings, *id).map(|d| (d.id, d.rotation))).collect();
    tidy(&mut c.body.drawings);
    for (id, r) in angles {
        if let Some(d) = c.body.drawings.iter_mut().find(|d| d.id == id) {
            d.rotation = r;
        }
    }
    c.dirty = true;
    let d = ids.first().and_then(|id| find(&c.body.drawings, *id)).cloned().unwrap_or_default();
    Ok(json!({"drawing": d.id, "changed": true, "rotation": d.rotation}))
}

/// Flip Horizontal / Vertical: each target is mirrored in place; a group mirrors its members across its
/// box.
pub(super) fn flip(s: &mut Session, p: &Value, which: &str) -> CmdResult {
    let horizontal = match which {
        "horizontal" => true,
        "vertical" => false,
        other => return Err(CmdError::Params(format!("`flip` is horizontal or vertical, not `{other}`"))),
    };
    let (cid, ids) = targets(s, p)?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    c.checkpoint();
    for id in &ids {
        let Some(t) = find(&c.body.drawings, *id).cloned() else { continue };
        let members: Vec<u32> = std::iter::once(*id).chain(descendants(&c.body.drawings, *id)).collect();
        for d in c.body.drawings.iter_mut().filter(|d| members.contains(&d.id)) {
            if horizontal {
                d.flip_h = !d.flip_h;
                if d.id != *id {
                    d.x = t.x + t.w - (d.x - t.x) - d.w;
                }
            } else {
                d.flip_v = !d.flip_v;
                if d.id != *id {
                    d.y = t.y + t.h - (d.y - t.y) - d.h;
                }
            }
            // A turned drawing mirrors its angle too.
            if d.id != *id || t.kind != DrawingKind::Group {
                d.rotation = (360.0 - d.rotation).rem_euclid(360.0);
            }
        }
    }
    tidy(&mut c.body.drawings);
    c.dirty = true;
    let d = ids.first().and_then(|id| find(&c.body.drawings, *id)).cloned().unwrap_or_default();
    Ok(json!({"drawing": d.id, "flipH": d.flip_h, "flipV": d.flip_v}))
}

#[cfg(test)]
mod tests {
    use super::super::super::compose::tests::{open, session};
    use super::*;

    fn shape(s: &mut Session, cid: u64, x: f32, y: f32, w: f32, h: f32) -> u32 {
        let r = s
            .run(
                "insert.shape",
                &json!({"compose": cid, "kind": "rectangle", "x": x, "y": y, "w": w, "h": h}),
            )
            .unwrap();
        r["drawing"].as_u64().unwrap() as u32
    }

    fn get(s: &Session, cid: u64, id: u32) -> Drawing {
        find(&s.composes[&cid].body.drawings, id).cloned().unwrap()
    }

    #[test]
    fn several_drawings_select_group_move_size_and_ungroup_as_one() {
        let mut s = session();
        let cid = open(&mut s);
        let a = shape(&mut s, cid, 0.0, 0.0, 50.0, 50.0);
        let b = shape(&mut s, cid, 100.0, 100.0, 50.0, 50.0);
        let c3 = shape(&mut s, cid, 300.0, 0.0, 20.0, 20.0);
        // Shift-click adds, Ctrl-click a selected one takes it out.
        s.run("drawing.select", &json!({"compose": cid, "id": a})).unwrap();
        s.run("drawing.select", &json!({"compose": cid, "id": b, "add": true})).unwrap();
        s.run("drawing.select", &json!({"compose": cid, "id": c3, "add": true})).unwrap();
        s.run("drawing.select", &json!({"compose": cid, "id": c3, "toggle": true})).unwrap();
        assert_eq!(s.composes[&cid].selected_drawings(), vec![b, a]);
        let r = s.run("drawing.group", &json!({"compose": cid})).unwrap();
        let g = r["group"].as_u64().unwrap() as u32;
        let gd = get(&s, cid, g);
        assert_eq!((gd.kind, gd.x, gd.y, gd.w, gd.h), (DrawingKind::Group, 0.0, 0.0, 150.0, 150.0));
        assert_eq!(s.composes[&cid].selected_drawings(), vec![g]);
        // A click on a member picks the group.
        s.run("drawing.select", &json!({"compose": cid, "id": a})).unwrap();
        assert_eq!(s.composes[&cid].selected_drawing, Some(g));
        // The group moves and sizes as one.
        s.run("drawing.move", &json!({"compose": cid, "dx": 10, "dy": 20})).unwrap();
        assert_eq!((get(&s, cid, b).x, get(&s, cid, b).y), (110.0, 120.0));
        s.run("drawing.size", &json!({"compose": cid, "w": 300, "h": 300})).unwrap();
        let bb = get(&s, cid, b);
        assert_eq!((bb.x, bb.y, bb.w, bb.h), (210.0, 220.0, 100.0, 100.0));
        // Formatting a group formats its members.
        s.run("drawing.fill", &json!({"compose": cid, "color": "#00ff00"})).unwrap();
        assert_eq!(get(&s, cid, a).fill, Some(mailcraft_geom::Color::rgb(0, 255, 0)));
        // Undo takes back the fill, then the size.
        s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
        s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
        assert_eq!(get(&s, cid, b).w, 50.0);
        // Ungroup selects the members; Regroup puts them back.
        let r = s.run("drawing.group", &json!({"compose": cid, "action": "ungroup"})).unwrap();
        assert_eq!(r["members"].as_array().unwrap().len(), 2);
        assert!(find(&s.composes[&cid].body.drawings, g).is_none());
        assert_eq!(s.composes[&cid].selected_drawings().len(), 2);
        let r = s.run("drawing.group", &json!({"compose": cid, "action": "regroup"})).unwrap();
        let g2 = r["group"].as_u64().unwrap() as u32;
        // Deleting the group deletes what is in it.
        let r = s.run("drawing.delete", &json!({"compose": cid, "id": g2})).unwrap();
        assert_eq!(r["deleted"].as_array().unwrap().len(), 3);
        assert_eq!(s.composes[&cid].body.drawings.len(), 1);
        // One drawing cannot be grouped.
        assert!(s.run("drawing.group", &json!({"compose": cid, "ids": [c3]})).is_err());
        assert!(s.run("drawing.group", &json!({"compose": cid, "action": "ungroup", "id": c3})).is_err());
    }

    #[test]
    fn groups_survive_saving_and_send_as_one_picture() {
        let mut s = session();
        let cid = open(&mut s);
        s.run("compose.insertText", &json!({"compose": cid, "text": "Body"})).unwrap();
        let a = shape(&mut s, cid, 0.0, 40.0, 50.0, 50.0);
        let b = shape(&mut s, cid, 200.0, 40.0, 50.0, 50.0);
        s.run("drawing.group", &json!({"compose": cid, "ids": [a, b]})).unwrap();
        let f = draw::flattened(&s.composes[&cid]).unwrap();
        assert_eq!(f.inline_images.len(), 1, "apart, yet one picture: they are a group");
        s.run("compose.saveDraft", &json!({"compose": cid})).unwrap();
        assert_eq!(s.composes[&cid].body.drawings.len(), 3, "still a group in the open message");
    }

    #[test]
    fn align_and_distribute_to_each_other_and_to_the_margins() {
        let mut s = session();
        let cid = open(&mut s);
        s.composes.get_mut(&cid).unwrap().view_width = Some(600.0);
        let a = shape(&mut s, cid, 10.0, 0.0, 20.0, 20.0);
        let b = shape(&mut s, cid, 100.0, 50.0, 40.0, 20.0);
        let c3 = shape(&mut s, cid, 400.0, 100.0, 20.0, 20.0);
        s.run("drawing.align", &json!({"compose": cid, "ids": [a, b, c3], "kind": "left"})).unwrap();
        assert!([a, b, c3].iter().all(|id| get(&s, cid, *id).x == 10.0), "to each other");
        s.run("drawing.align", &json!({"compose": cid, "ids": [a, b, c3], "kind": "top"})).unwrap();
        assert!([a, b, c3].iter().all(|id| get(&s, cid, *id).y == 0.0));
        // Distribute: put them apart again, then even out the gaps between the outer two.
        s.run("drawing.move", &json!({"compose": cid, "id": b, "x": 50})).unwrap();
        s.run("drawing.move", &json!({"compose": cid, "id": c3, "x": 300})).unwrap();
        s.run("drawing.distribute", &json!({"compose": cid, "ids": [a, b, c3], "axis": "horizontal"}))
            .unwrap();
        let (xa, xb, xc) = (get(&s, cid, a), get(&s, cid, b), get(&s, cid, c3));
        let gap1 = xb.x - (xa.x + xa.w);
        let gap2 = xc.x - (xb.x + xb.w);
        assert!((gap1 - gap2).abs() < 0.01, "{gap1} {gap2}");
        assert_eq!((xa.x, xc.x), (10.0, 300.0), "the outer ones stay");
        // Align to Margin: with several selected they go to the margin instead.
        s.run("drawing.align", &json!({"compose": cid, "relative": "margin"})).unwrap();
        s.run("drawing.align", &json!({"compose": cid, "ids": [a, b], "kind": "right"})).unwrap();
        assert_eq!(get(&s, cid, a).x, 580.0);
        assert_eq!(get(&s, cid, b).x, 560.0);
        // Two drawings distribute across the margins.
        s.run("drawing.distribute", &json!({"compose": cid, "ids": [a, b], "axis": "horizontal"})).unwrap();
        let (l, r) = (get(&s, cid, a).x.min(get(&s, cid, b).x), get(&s, cid, a).x.max(get(&s, cid, b).x));
        assert_eq!(l, 0.0);
        assert!(r > 500.0);
        assert!(s.run("drawing.distribute", &json!({"compose": cid, "axis": "diagonal"})).is_err());
    }

    #[test]
    fn groups_turn_and_flip_as_one() {
        let mut s = session();
        let cid = open(&mut s);
        let a = shape(&mut s, cid, 0.0, 0.0, 20.0, 20.0);
        let b = shape(&mut s, cid, 80.0, 0.0, 20.0, 20.0);
        let r = s.run("drawing.group", &json!({"compose": cid, "ids": [a, b]})).unwrap();
        let g = r["group"].as_u64().unwrap() as u32;
        s.run("drawing.rotate", &json!({"compose": cid, "by": 90})).unwrap();
        let (da, db) = (get(&s, cid, a), get(&s, cid, b));
        assert!((da.x - db.x).abs() < 0.5 && (da.y - db.y).abs() > 50.0, "{da:?} {db:?}");
        assert_eq!(get(&s, cid, g).rotation, 90.0);
        s.run("drawing.rotate", &json!({"compose": cid, "degrees": 0})).unwrap();
        s.run("drawing.rotate", &json!({"compose": cid, "flip": "horizontal"})).unwrap();
        let (da, db) = (get(&s, cid, a), get(&s, cid, b));
        assert!(da.x > db.x, "mirrored across the group");
        assert!(da.flip_h && db.flip_h);
        assert!(s.run("drawing.rotate", &json!({"compose": cid, "flip": "sideways"})).is_err());
    }
}
