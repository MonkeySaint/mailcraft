mod more;

use super::super::compose::tests::{open, session};
use super::*;
use mailcraft_model::rich::WrapStyle;

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
    s.run("drawing.format", &json!({"compose": cid, "x": 5, "y": 6, "w": 70, "h": 80, "behindText": true}))
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
    // Forward past the canvas and what is drawn in it (they stay together).
    assert_eq!(drawings(&s, cid)[2].id, ids[2]);
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
    let r = s.run("insert.textBox", &json!({"compose": cid, "preset": "draw", "vertical": true})).unwrap();
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
    s.run("drawing.textOutline", &json!({"compose": cid, "id": wa, "theme": "accent2", "tint": 0})).unwrap();
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
    s.run("insert.wordArt", &json!({"compose": cid, "style": "Fill: Black, Text color 1; Shadow"})).unwrap();
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
