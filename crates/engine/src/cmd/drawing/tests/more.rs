//! Fills, outlines, effects, text directions, Format Shape, the Selection pane and equations (round 7).

use super::*;

/// A 2×2 PNG (red, green / blue, white) for picture fills.
fn tiny_png() -> String {
    use base64::Engine as _;
    let px = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
    let png = mailcraft_render::Canvas { width: 2, height: 2, pixels: px }.to_png();
    base64::engine::general_purpose::STANDARD.encode(png)
}

fn new_rect(s: &mut Session, cid: u64) -> u32 {
    let r = s
        .run("insert.shape", &json!({"compose": cid, "kind": "rectangle", "x": 0, "y": 0, "w": 120, "h": 80}))
        .unwrap();
    s.run("drawing.effect", &json!({"compose": cid, "effect": "none"})).unwrap();
    r["drawing"].as_u64().unwrap() as u32
}

#[test]
fn fills_dark_gradients_stops_pictures_and_transparency() {
    let mut s = session();
    let cid = open(&mut s);
    let id = new_rect(&mut s, cid);
    let solid = raster_of(&s, cid, id);
    s.run("drawing.fill", &json!({"compose": cid, "gradient": "dark:down"})).unwrap();
    let g = raster_of(&s, cid, id);
    assert!(shade(&g, 0.5, 0.2) > shade(&g, 0.5, 0.8) + 40, "dark at the bottom");
    s.run(
        "drawing.fill",
        &json!({"compose": cid, "stops": [{"pos": 0, "color": "#000000"}, {"pos": 1, "color": "#ffffff"}], "angle": 0}),
    )
    .unwrap();
    let g = raster_of(&s, cid, id);
    assert!(shade(&g, 0.8, 0.5) > shade(&g, 0.2, 0.5) + 200, "black to white, left to right");
    assert!(s.run("drawing.fill", &json!({"compose": cid, "stops": [{"pos": 0, "color": "#000"}]})).is_err());
    s.run("drawing.fill", &json!({"compose": cid, "picture": tiny_png()})).unwrap();
    let d = drawings(&s, cid)[0].clone();
    assert!(!d.fill_picture.is_empty() && d.gradient_fill.is_none());
    let p = raster_of(&s, cid, id);
    assert_ne!(p.rgba, solid.rgba);
    assert!(s.run("drawing.fill", &json!({"compose": cid, "picture": "bm90IGEgcGljdHVyZQ=="})).is_err());
    s.run("drawing.fill", &json!({"compose": cid, "color": "#ff0000"})).unwrap();
    assert!(drawings(&s, cid)[0].fill_picture.is_empty(), "a colour replaces the picture");
    s.run("drawing.fill", &json!({"compose": cid, "transparency": "50%"})).unwrap();
    assert_eq!(drawings(&s, cid)[0].transparency, 0.5);
    let r = raster_of(&s, cid, id);
    let mid = ((r.height as usize / 2) * r.width as usize + r.width as usize / 2) * 4 + 3;
    assert!(r.rgba[mid] < 160, "see-through: {}", r.rgba[mid]);
}

#[test]
fn outlines_get_dashes_and_text_outlines_weight() {
    let mut s = session();
    let cid = open(&mut s);
    let id = new_rect(&mut s, cid);
    s.run("drawing.fill", &json!({"compose": cid, "color": null})).unwrap();
    s.run("drawing.outline", &json!({"compose": cid, "weight": 3})).unwrap();
    let solid = raster_of(&s, cid, id);
    s.run("drawing.outline", &json!({"compose": cid, "dash": "dash"})).unwrap();
    let dashed = raster_of(&s, cid, id);
    let ink = |r: &draw::Raster| r.rgba.chunks(4).filter(|p| p[3] > 128).count();
    assert!(ink(&dashed) < ink(&solid), "gaps in the line");
    assert!(s.run("drawing.outline", &json!({"compose": cid, "dash": "zigzag"})).is_err());
    let r = s.run("insert.textBox", &json!({"compose": cid, "text": "Words", "x": 0, "y": 200})).unwrap();
    let tb = r["drawing"].as_u64().unwrap() as u32;
    s.run("drawing.textOutline", &json!({"compose": cid, "id": tb, "color": "#ff0000"})).unwrap();
    let thin = raster_of(&s, cid, tb);
    s.run("drawing.textOutline", &json!({"compose": cid, "id": tb, "weight": 4})).unwrap();
    assert_ne!(raster_of(&s, cid, tb).rgba, thin.rgba);
    s.run("drawing.textOutline", &json!({"compose": cid, "id": tb, "dash": "squareDot"})).unwrap();
    assert_eq!(drawings(&s, cid).iter().find(|d| d.id == tb).unwrap().text_outline_dash, "squareDot");
}

#[test]
fn effects_with_options_and_text_effects_transform() {
    let mut s = session();
    let cid = open(&mut s);
    let id = new_rect(&mut s, cid);
    let plain = raster_of(&s, cid, id);
    for p in [
        json!({"shadow": "outerBottomRight"}),
        json!({"shadow": {"color": "#00ff00", "distance": 10}}),
        json!({"glow": {"theme": "accent2", "size": 11}}),
        json!({"reflection": "full8"}),
        json!({"softEdges": 10}),
        json!({"bevel": "circle"}),
        json!({"rotation3d": "perspectiveLeft"}),
        json!({"preset": 3}),
    ] {
        let mut q = p.clone();
        q["compose"] = json!(cid);
        s.run("drawing.effect", &json!({"compose": cid, "effect": "none"})).unwrap();
        s.run("drawing.effect", &q).unwrap();
        assert_ne!(raster_of(&s, cid, id).rgba, plain.rgba, "{p}");
    }
    s.run("drawing.effect", &json!({"compose": cid, "preset": 0, "reflection": "fullTouching"})).unwrap();
    let refl = raster_of(&s, cid, id);
    assert!(refl.height > plain.height, "the reflection adds height");
    s.run("drawing.effect", &json!({"compose": cid, "reflection": null})).unwrap();
    assert_eq!(drawings(&s, cid)[0].fx, None, "no effects left");
    // Text: Transform warps the letters; options work on letters as on shapes.
    let r = s
        .run("insert.wordArt", &json!({"compose": cid, "style": "Fill: Black, Text color 1", "text": "Wave"}))
        .unwrap();
    let wa = r["drawing"].as_u64().unwrap() as u32;
    let flat = raster_of(&s, cid, wa);
    s.run("drawing.textEffect", &json!({"compose": cid, "id": wa, "transform": "archUp"})).unwrap();
    assert_ne!(raster_of(&s, cid, wa).rgba, flat.rgba);
    s.run(
        "drawing.textEffect",
        &json!({"compose": cid, "id": wa, "transform": "", "glow": {"color": "#ff0000", "size": 5}}),
    )
    .unwrap();
    let d = drawings(&s, cid).into_iter().find(|d| d.id == wa).unwrap();
    assert!(d.text_fx.as_ref().unwrap().glow.is_some() && d.warp.is_empty());
    assert!(s.run("drawing.textEffect", &json!({"compose": cid, "id": wa, "transform": "twist"})).is_err());
    let r = s.run("insert.textBox", &json!({"compose": cid, "text": "Box", "x": 0, "y": 300})).unwrap();
    let tb = r["drawing"].as_u64().unwrap();
    assert!(
        s.run("drawing.textEffect", &json!({"compose": cid, "id": tb, "reflection": "halfTouching"}))
            .is_err(),
        "reflections are for WordArt"
    );
    s.run("drawing.textEffect", &json!({"compose": cid, "id": tb, "shadow": "outerBottom"})).unwrap();
}

#[test]
fn text_runs_up_or_stacked_flips_and_resize_to_fit_text() {
    let mut s = session();
    let cid = open(&mut s);
    let r = s
        .run("insert.textBox", &json!({"compose": cid, "text": "WWWWWW", "x": 0, "y": 0, "w": 300, "h": 300}))
        .unwrap();
    let id = r["drawing"].as_u64().unwrap() as u32;
    s.run("drawing.outline", &json!({"compose": cid, "color": null})).unwrap();
    s.run("drawing.fill", &json!({"compose": cid, "color": null})).unwrap();
    s.run("drawing.textDirection", &json!({"compose": cid, "direction": "down"})).unwrap();
    let down = raster_of(&s, cid, id);
    s.run("drawing.textDirection", &json!({"compose": cid, "direction": "up"})).unwrap();
    let up = raster_of(&s, cid, id);
    assert_ne!(down.rgba, up.rgba, "turned the other way");
    let d = drawings(&s, cid)[0].clone();
    assert!(!d.vertical && d.text_flow == "up");
    s.run("drawing.textDirection", &json!({"compose": cid, "direction": "stacked"})).unwrap();
    assert!(s.run("drawing.textDirection", &json!({"compose": cid, "direction": "diagonal"})).is_err());
    s.run("drawing.textDirection", &json!({"compose": cid, "direction": "horizontal"})).unwrap();
    // Flips mirror the shape.
    let r = s
        .run(
            "insert.shape",
            &json!({"compose": cid, "kind": "rightTriangle", "x": 400, "y": 0, "w": 100, "h": 100}),
        )
        .unwrap();
    let tri = r["drawing"].as_u64().unwrap() as u32;
    s.run("drawing.effect", &json!({"compose": cid, "effect": "none"})).unwrap();
    let before = raster_of(&s, cid, tri);
    s.run("drawing.rotate", &json!({"compose": cid, "flip": "horizontal"})).unwrap();
    let after = raster_of(&s, cid, tri);
    assert!(drawings(&s, cid).iter().find(|d| d.id == tri).unwrap().flip_h);
    assert_ne!(before.rgba, after.rgba);
    // Resize Shape to Fit Text: the box takes the text's height.
    let many = (1..=12).map(|n| format!("Line {n}")).collect::<Vec<_>>().join("\n");
    s.run("drawing.text", &json!({"compose": cid, "id": id, "text": many})).unwrap();
    s.run("drawing.format", &json!({"compose": cid, "id": id, "autofit": true})).unwrap();
    let h = drawings(&s, cid).iter().find(|d| d.id == id).unwrap().h;
    assert!(h > 150.0 && h < 300.0, "{h}");
}

#[test]
fn format_shape_applies_everything_as_one_step() {
    let mut s = session();
    let cid = open(&mut s);
    let id = new_rect(&mut s, cid);
    let r = s.run("drawing.format", &json!({"compose": cid, "page": "fill"})).unwrap();
    assert!(r.get("dialog").is_some());
    let before = drawings(&s, cid)[0].clone();
    s.run(
        "drawing.format",
        &json!({"compose": cid, "fill": "#123456", "transparency": 0.25, "outline": "#ff0000", "outlineWidth": 4,
                "dash": "dashDot", "effects": {"shadow": "outerTop", "bevel": "angle"}, "inset": [1, 2, 3, 4],
                "valign": "middle", "w": 200, "x": 30, "rotation": 10, "name": "Logo box", "wrap": "square"}),
    )
    .unwrap();
    let d = drawings(&s, cid)[0].clone();
    assert_eq!(d.fill, Some(Color::rgb(0x12, 0x34, 0x56)));
    assert_eq!((d.transparency, d.outline_px, d.line_dash.as_str()), (0.25, 4.0, "dashDot"));
    assert_eq!(d.fx.as_ref().unwrap().bevel, "angle");
    assert_eq!((d.inset, d.valign), (Some([1.0, 2.0, 3.0, 4.0]), VAlign::Middle));
    assert_eq!((d.w, d.x, d.rotation, d.name.as_str()), (200.0, 30.0, 10.0, "Logo box"));
    assert_eq!(d.wrap, WrapStyle::Square);
    s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
    assert_eq!(drawings(&s, cid)[0], before, "one undo takes it all back");
    assert!(s.run("drawing.format", &json!({"compose": cid, "id": id, "inset": [1, 2]})).is_err());
}

#[test]
fn selection_pane_selects_several_renames_and_reorders() {
    let mut s = session();
    let cid = open(&mut s);
    let a = new_rect(&mut s, cid);
    let b = new_rect(&mut s, cid);
    // The pane lists drawings front-most first: 0 = b, 1 = a.
    s.run("format.selectionPane", &json!({"compose": cid, "select": 0})).unwrap();
    s.run("format.selectionPane", &json!({"compose": cid, "select": 1, "add": true})).unwrap();
    assert_eq!(s.composes[&cid].selected_drawings(), vec![a, b]);
    s.run("format.selectionPane", &json!({"compose": cid, "select": 1, "add": true})).unwrap();
    assert_eq!(s.composes[&cid].selected_drawings(), vec![b], "Ctrl-click again takes it out");
    s.run("format.selectionPane", &json!({"compose": cid, "rename": 0, "name": "Front box"})).unwrap();
    let r = s.run("format.selectionPane", &json!({"compose": cid, "list": true})).unwrap();
    assert_eq!(r["objects"][0]["name"], "Front box");
    let r = s.run("format.selectionPane", &json!({"compose": cid, "reorder": 0, "by": -1})).unwrap();
    assert_eq!(r["objects"][1], "Front box", "sent backward");
    assert!(s.run("format.selectionPane", &json!({"compose": cid, "rename": 9, "name": "x"})).is_err());
}

#[test]
fn equations_reopen_in_the_equation_dialog() {
    let mut s = session();
    let cid = open(&mut s);
    let r = s.run("insert.equation", &json!({"compose": cid, "text": "a^2+b^2=c^2"})).unwrap();
    let pic = r["cid"].as_str().unwrap().to_owned();
    let r = s.run("insert.editPicture", &json!({"compose": cid, "cid": pic})).unwrap();
    assert_eq!(r["dialog"], "equation");
    let dlg = crate::cmd::insert::tests::last_dialog(&s);
    assert_eq!(dlg["text"], "a^2+b^2=c^2");
    assert!(dlg["structures"].as_array().unwrap().len() >= 20);
    let r = s.run("insert.equation", &json!({"compose": cid, "edit": pic, "text": "x/y"})).unwrap();
    let new = r["cid"].as_str().unwrap().to_owned();
    assert_ne!(new, pic);
    let c = &s.composes[&cid];
    assert!(c.images().iter().any(|(src, alt)| src == &format!("cid:{new}") && alt == "x/y"));
    assert!(!c.images().iter().any(|(src, _)| src == &format!("cid:{pic}")), "replaced in place");
    s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
    assert!(s.composes[&cid].images().iter().any(|(src, _)| src == &format!("cid:{pic}")));
    assert!(s.run("insert.equation", &json!({"compose": cid, "edit": "nope", "text": "x"})).is_err());
}

/// Two rectangles side by side, grouped; returns (group, members).
fn new_group(s: &mut Session, cid: u64) -> (u32, Vec<u32>) {
    let mut ids = Vec::new();
    for x in [0, 200] {
        let r = s
            .run(
                "insert.shape",
                &json!({"compose": cid, "kind": "rectangle", "x": x, "y": 0, "w": 100, "h": 50}),
            )
            .unwrap();
        ids.push(r["drawing"].as_u64().unwrap() as u32);
    }
    s.run("drawing.select", &json!({"compose": cid, "ids": ids})).unwrap();
    let g = s.run("drawing.group", &json!({"compose": cid})).unwrap()["group"].as_u64().unwrap() as u32;
    (g, ids)
}

fn by_id(s: &Session, cid: u64, id: u32) -> Drawing {
    find(&s.composes[&cid].body.drawings, id).cloned().unwrap()
}

/// Regression: Format Shape on a group opened on its first drawing and changed only that drawing (its
/// size, name; wrapping always failed).
#[test]
fn format_shape_on_a_group_formats_the_group() {
    let mut s = session();
    let cid = open(&mut s);
    let (g, members) = new_group(&mut s, cid);
    s.run("drawing.fill", &json!({"compose": cid, "color": "#336699"})).unwrap();
    s.run("drawing.format", &json!({"compose": cid})).unwrap();
    let dlg = crate::cmd::insert::tests::last_dialog(&s);
    assert_eq!(
        (dlg["id"].clone(), dlg["ids"].clone(), dlg["kind"].clone()),
        (json!(g), json!([g]), json!("group"))
    );
    assert_eq!(
        (dlg["x"].clone(), dlg["w"].clone(), dlg["h"].clone()),
        (json!(0.0), json!(300.0), json!(50.0))
    );
    assert_eq!(dlg["fill"], "#336699", "the fill shown is the group's drawings'");
    let undo = s.composes[&cid].edit_undo.len();
    s.run(
        "drawing.format",
        &json!({"compose": cid, "ids": [g], "w": 600, "wrap": "square", "name": "Logo", "alt": "Two boxes",
                "fill": "#ff0000"}),
    )
    .unwrap();
    let gd = by_id(&s, cid, g);
    assert_eq!((gd.w, gd.h, gd.wrap), (600.0, 50.0, WrapStyle::Square));
    assert_eq!((gd.name.as_str(), gd.alt.as_str()), ("Logo", "Two boxes"));
    for m in &members {
        let d = by_id(&s, cid, *m);
        assert_eq!((d.w, d.h), (200.0, 50.0), "members scale with the group");
        assert_eq!(d.fill, Some(Color::rgb(255, 0, 0)));
        assert_ne!(d.name, "Logo");
        assert_ne!(d.alt, "Two boxes");
    }
    assert_eq!(s.composes[&cid].edit_undo.len(), undo + 1, "one undo step");
    // Several drawings (no group): every one is formatted, the name goes to the first.
    let a = new_rect(&mut s, cid);
    let b = new_rect(&mut s, cid);
    s.run("drawing.select", &json!({"compose": cid, "ids": [a, b]})).unwrap();
    s.run("drawing.format", &json!({"compose": cid})).unwrap();
    let dlg = crate::cmd::insert::tests::last_dialog(&s);
    let ids = dlg["ids"].clone();
    assert_eq!(ids.as_array().unwrap().len(), 2);
    s.run(
        "drawing.format",
        &json!({"compose": cid, "ids": ids, "h": 33, "outline": "#00ff00", "name": "First"}),
    )
    .unwrap();
    let (da, db) = (by_id(&s, cid, a), by_id(&s, cid, b));
    assert_eq!((da.h, db.h, da.w, db.w), (33.0, 33.0, 120.0, 120.0), "only the side given changes");
    assert_eq!((da.outline, db.outline), (Some(Color::rgb(0, 255, 0)), Some(Color::rgb(0, 255, 0))));
    let first = ids[0].as_u64().unwrap() as u32;
    let (named, other) = if first == a { (da, db) } else { (db, da) };
    assert_eq!(named.name, "First");
    assert_ne!(other.name, "First");
}

/// Regression: a Format Shape step that failed left the steps before it applied (and their undo steps).
#[test]
fn a_failing_format_shape_changes_nothing() {
    let mut s = session();
    let cid = open(&mut s);
    s.run("insert.shape", &json!({"compose": cid, "kind": "canvas", "x": 0, "y": 0, "w": 300, "h": 200}))
        .unwrap();
    let id = s
        .run("insert.shape", &json!({"compose": cid, "kind": "oval", "x": 20, "y": 20, "w": 50, "h": 50}))
        .unwrap()["drawing"]
        .as_u64()
        .unwrap() as u32;
    // Something to redo.
    s.run("drawing.move", &json!({"compose": cid, "id": id, "dx": 5})).unwrap();
    s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
    let before = s.composes[&cid].clone();
    let r = s.run(
        "drawing.format",
        &json!({"compose": cid, "id": id, "fill": "#ff0000", "outlineWidth": 6, "wrap": "square"}),
    );
    assert!(r.is_err(), "a drawing in a canvas wraps with the canvas");
    let c = &s.composes[&cid];
    assert_eq!(c.body, before.body, "the fill and the line are not applied");
    assert_eq!(c.edit_undo.len(), before.edit_undo.len(), "no undo step");
    assert_eq!(c.edit_redo.len(), before.edit_redo.len(), "the redo history stays");
    assert_eq!((c.dirty, c.edit_rev), (before.dirty, before.edit_rev));
    s.run("compose.editRedo", &json!({"compose": cid})).unwrap();
    assert_eq!(by_id(&s, cid, id).x, 25.0);
}

/// Regression: with the undo history full, one Format Shape became several undo steps.
#[test]
fn format_shape_is_one_undo_step_with_a_full_history() {
    let mut s = session();
    let cid = open(&mut s);
    let id = new_rect(&mut s, cid);
    for _ in 0..crate::compose::MAX_EDIT_UNDO + 5 {
        s.run("drawing.move", &json!({"compose": cid, "id": id, "dx": 1})).unwrap();
    }
    assert_eq!(s.composes[&cid].edit_undo.len(), crate::compose::MAX_EDIT_UNDO);
    let before = by_id(&s, cid, id);
    s.run(
        "drawing.format",
        &json!({"compose": cid, "id": id, "fill": "#ff0000", "outline": "#0000ff", "rotation": 30, "name": "Box"}),
    )
    .unwrap();
    assert_eq!(s.composes[&cid].edit_undo.len(), crate::compose::MAX_EDIT_UNDO);
    s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
    assert_eq!(by_id(&s, cid, id), before, "one undo takes the whole pane back");
}

/// Regression: a refused Group or Regroup cleared the redo history and marked the draft edited.
#[test]
fn a_refused_group_keeps_the_redo_history() {
    let mut s = session();
    let cid = open(&mut s);
    let id = new_rect(&mut s, cid);
    s.run("drawing.move", &json!({"compose": cid, "id": id, "dx": 5})).unwrap();
    s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
    let before = s.composes[&cid].clone();
    assert!(s.run("drawing.group", &json!({"compose": cid, "ids": [id]})).is_err());
    assert!(s.run("drawing.group", &json!({"compose": cid, "action": "regroup"})).is_err());
    let c = &s.composes[&cid];
    assert_eq!((c.edit_undo.len(), c.edit_redo.len()), (before.edit_undo.len(), before.edit_redo.len()));
    assert_eq!(c.edit_rev, before.edit_rev);
    assert_eq!(c.body, before.body);
}

#[test]
fn effect_presets_are_recognised() {
    let mut s = session();
    let cid = open(&mut s);
    let id = new_rect(&mut s, cid);
    assert_eq!(preset_of(&by_id(&s, cid, id), &s.composes[&cid], false), None);
    for n in [1, 4, 10] {
        s.run("drawing.effect", &json!({"compose": cid, "preset": n})).unwrap();
        assert_eq!(preset_of(&by_id(&s, cid, id), &s.composes[&cid], false), Some(n), "preset {n}");
    }
    s.run("drawing.textEffect", &json!({"compose": cid, "preset": 4})).unwrap();
    assert_eq!(preset_of(&by_id(&s, cid, id), &s.composes[&cid], true), Some(4));
    // A group shows its first drawing's.
    let (g, members) = new_group(&mut s, cid);
    s.run("drawing.effect", &json!({"compose": cid, "preset": 2})).unwrap();
    let c = &s.composes[&cid];
    assert_eq!(c.selected_drawing, Some(g));
    let src = format_source(c).unwrap();
    assert!(members.contains(&src.id));
    assert_eq!(preset_of(src, c, false), Some(2));
}
