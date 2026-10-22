//! Save Selection to Text Box Gallery with a text box (or other drawing) selected, and inserting the saved
//! entry back as a drawing.

use serde_json::json;

use crate::Session;
use crate::cmd::compose::tests::{open, session};
use crate::cmd::insert::tests::last_dialog;
use crate::compose::{BuildingBlock, MAX_BLOCK_DRAWINGS};
use mailcraft_geom::Color;
use mailcraft_model::Doc;
use mailcraft_model::rich::{Drawing, DrawingKind, WrapStyle};

fn setup() -> (Session, u64) {
    let mut s = session();
    let id = open(&mut s);
    s.run("compose.insertText", &json!({"text": "Hello"})).unwrap();
    (s, id)
}

fn drawings(s: &Session, id: u64) -> Vec<Drawing> {
    s.composes.get(&id).unwrap().body.drawings.clone()
}

fn body_text(s: &Session, id: u64) -> String {
    s.composes.get(&id).unwrap().body.text_content()
}

/// Release check: Save Selection to Text Box Gallery needed selected text and kept only text, so a selected
/// text box could not be saved, and the saved entry came back as plain text in the flow.
#[test]
fn a_selected_text_box_is_saved_and_comes_back_as_a_text_box() {
    let (mut s, id) = setup();
    let r = s
        .run("insert.textBox", &json!({"text": "Call us today", "x": 40, "y": 30, "w": 180, "h": 60}))
        .unwrap();
    let box_id = r["drawing"].as_u64().unwrap();
    s.run("drawing.fill", &json!({"id": box_id, "color": "#336699"})).unwrap();
    let original = drawings(&s, id).into_iter().find(|d| u64::from(d.id) == box_id).unwrap();
    assert!(s.composes[&id].selection.is_collapsed(), "no text is selected");
    assert!(super::has_selection_or_drawing(&s).is_none(), "enabled with a text box selected");

    // Without a name: the Create New Building Block dialog, named after the box's text.
    let r = s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "textBoxes"})).unwrap();
    assert_eq!(r["dialog"], "createBuildingBlock");
    assert_eq!(last_dialog(&s)["name"], "Call us today");
    let r = s
        .run("insert.saveToGallery", &json!({"compose": id, "gallery": "textBoxes", "name": "Contact box"}))
        .unwrap();
    assert_eq!(r["drawings"], 1);
    let saved = super::find_block(&s, id, Some("textBoxes"), "Contact box").unwrap();
    assert!(saved.blocks.is_empty(), "the entry is the text box, not text");
    assert_eq!((saved.drawings[0].x, saved.drawings[0].y), (0.0, 0.0), "kept relative to its own corner");

    // Gone from the message, then picked from the Text Box menu: a text box again, not a paragraph.
    s.run("drawing.delete", &json!({"id": box_id})).unwrap();
    assert!(drawings(&s, id).is_empty());
    let before = body_text(&s, id);
    let r = s.run("insert.quickPart", &json!({"gallery": "textBoxes", "name": "Contact box"})).unwrap();
    assert_eq!(r["drawings"].as_array().unwrap().len(), 1);
    assert_eq!(body_text(&s, id), before, "no text went into the flow");
    let back = drawings(&s, id);
    assert_eq!(back.len(), 1);
    let d = &back[0];
    assert_eq!(d.kind, DrawingKind::TextBox);
    assert_eq!(d.text, "Call us today");
    assert_eq!((d.w, d.h), (original.w, original.h));
    assert_eq!(d.fill, original.fill);
    assert_eq!(s.composes[&id].selected_drawings(), vec![d.id], "the new box is selected");

    // The saved:<name> preset of Insert › Text Box places it where asked, as a second box.
    let r = s.run("insert.textBox", &json!({"preset": "saved:Contact box", "x": 100, "y": 200})).unwrap();
    let second = r["drawings"][0].as_u64().unwrap();
    let d2 = drawings(&s, id).into_iter().find(|d| u64::from(d.id) == second).unwrap();
    assert_eq!((d2.x, d2.y), (100.0, 200.0));
    assert_eq!(drawings(&s, id).len(), 2);
    // One undo step takes it out again.
    s.run("compose.editUndo", &json!({})).unwrap();
    assert_eq!(drawings(&s, id).len(), 1);

    // The Quick Parts list previews the box's text.
    s.run("insert.quickPart", &json!({"gallery": "textBoxes"})).unwrap();
    let entries = last_dialog(&s)["entries"].clone();
    let e = entries.as_array().unwrap().iter().find(|e| e["name"] == "Contact box").unwrap().clone();
    assert_eq!(e["preview"], "Call us today");
    assert_eq!(e["drawings"], 1);
}

#[test]
fn a_selected_group_is_saved_whole_and_its_links_follow_the_new_ids() {
    let (mut s, id) = setup();
    let a = s.run("insert.textBox", &json!({"text": "Left", "x": 10, "y": 10, "w": 80, "h": 40})).unwrap()
        ["drawing"]
        .as_u64()
        .unwrap();
    let b = s.run("insert.textBox", &json!({"x": 110, "y": 50, "w": 80, "h": 40})).unwrap()["drawing"]
        .as_u64()
        .unwrap();
    s.run("drawing.linkTextBox", &json!({"id": a, "to": b})).unwrap();
    s.run("drawing.group", &json!({"ids": [a, b]})).unwrap();
    let group = s.composes[&id].selected_drawings();
    assert_eq!(group.len(), 1);
    s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "textBoxes", "name": "Pair"})).unwrap();
    let saved = super::find_block(&s, id, Some("textBoxes"), "Pair").unwrap();
    assert_eq!(saved.drawings.len(), 3, "the group and both members");

    let r = s
        .run("insert.quickPart", &json!({"gallery": "textBoxes", "name": "Pair", "x": 300, "y": 300}))
        .unwrap();
    let ids: Vec<u32> =
        r["drawings"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect();
    assert_eq!(ids.len(), 3);
    let all = drawings(&s, id);
    let new: Vec<&Drawing> = all.iter().filter(|d| ids.contains(&d.id)).collect();
    let g = new.iter().find(|d| d.kind == DrawingKind::Group).unwrap();
    let members: Vec<&&Drawing> = new.iter().filter(|d| d.kind == DrawingKind::TextBox).collect();
    assert_eq!(members.len(), 2);
    assert!(members.iter().all(|m| m.group == Some(g.id)), "members belong to the new group");
    let left = members.iter().find(|m| m.text == "Left").unwrap();
    let right = members.iter().find(|m| m.id != left.id).unwrap();
    assert_eq!((left.x, left.y), (300.0, 300.0));
    assert_eq!((right.x - left.x, right.y - left.y), (100.0, 40.0), "layout kept");
    assert_eq!(left.link_next, Some(right.id), "the text box link follows the copy");
    assert_eq!(s.composes[&id].selected_drawings(), vec![g.id], "the new group is selected");
}

#[test]
fn nothing_selected_is_refused_and_text_galleries_still_save_text() {
    let (mut s, id) = setup();
    s.run("drawing.select", &json!({"id": null})).ok();
    assert!(super::has_selection_or_drawing(&s).is_some());
    assert!(
        s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "textBoxes", "name": "X"})).is_err()
    );
    // Quick Tables and equations keep text only: a selected drawing alone does not count there.
    let box_id = s.run("insert.textBox", &json!({"text": "Box"})).unwrap()["drawing"].as_u64().unwrap();
    assert!(
        s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "quickTables", "name": "T"}))
            .is_err()
    );
    // A selected drawing wins over text left selected from before it was clicked.
    s.run("compose.select", &json!({"compose": id, "all": true})).unwrap();
    s.run("drawing.select", &json!({"id": box_id})).unwrap();
    s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "textBoxes", "name": "Boxed"})).unwrap();
    assert_eq!(super::find_block(&s, id, Some("textBoxes"), "Boxed").unwrap().drawings.len(), 1);
    // With no drawing selected, the selected text is saved.
    s.run("drawing.select", &json!({"id": null})).unwrap();
    s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "textBoxes", "name": "Words"})).unwrap();
    let saved = super::find_block(&s, id, Some("textBoxes"), "Words").unwrap();
    assert!(saved.drawings.is_empty());
    assert!(!saved.blocks.is_empty());
}

/// Saved entries come back from the profile file: hostile geometry and a flood of drawings must not crash
/// or overflow the message.
#[test]
fn hostile_saved_drawings_are_capped_and_cleaned() {
    let (mut s, id) = setup();
    let bad = Drawing {
        id: 7,
        kind: DrawingKind::TextBox,
        x: f32::NAN,
        y: f32::INFINITY,
        w: -5.0,
        h: f32::NAN,
        group: Some(999),
        link_next: Some(7),
        fill: Some(Color::rgb(1, 2, 3)),
        ..Drawing::default()
    };
    let flood: Vec<Drawing> = (0..200u32).map(|i| Drawing { id: i + 100, ..bad.clone() }).collect();
    let block = |name: &str, drawings: Vec<Drawing>| BuildingBlock {
        gallery: "textBoxes".into(),
        name: name.into(),
        drawings,
        ..BuildingBlock::default()
    };
    let c = s.composes.get_mut(&id).unwrap();
    c.building_blocks.push(block("Bad", vec![bad]));
    c.building_blocks.push(block("Flood", flood));
    s.run("insert.quickPart", &json!({"gallery": "textBoxes", "name": "Bad"})).unwrap();
    let d = drawings(&s, id);
    assert_eq!(d.len(), 1);
    assert!(d[0].x.is_finite() && d[0].y.is_finite() && d[0].w > 0.0 && d[0].h > 0.0);
    assert_eq!(d[0].group, None, "a group that was not saved is dropped");
    let r = s.run("insert.quickPart", &json!({"gallery": "textBoxes", "name": "Flood"})).unwrap();
    assert_eq!(r["drawings"].as_array().unwrap().len(), MAX_BLOCK_DRAWINGS);
}

fn text_box(s: &mut Session, text: &str, x: u32, y: u32) -> u32 {
    s.run("insert.textBox", &json!({"text": text, "x": x, "y": y, "w": 80, "h": 40})).unwrap()["drawing"]
        .as_u64()
        .unwrap() as u32
}

fn new_ids(r: &serde_json::Value) -> Vec<u32> {
    r["drawings"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect()
}

/// Release check: only the selected drawings and their direct members were saved, so a group of groups came
/// back with an empty inner group (an invisible selectable box) and a canvas came back without its shapes.
#[test]
fn a_group_of_groups_is_saved_with_everything_in_it() {
    let (mut s, id) = setup();
    let a = text_box(&mut s, "A", 10, 10);
    let b = text_box(&mut s, "B", 110, 10);
    let c = text_box(&mut s, "C", 10, 110);
    s.run("drawing.group", &json!({"ids": [a, b]})).unwrap();
    let inner = s.composes[&id].selected_drawings()[0];
    s.run("drawing.group", &json!({"ids": [inner, c]})).unwrap();
    s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "quickParts", "name": "Nested"}))
        .unwrap();
    let saved = super::find_block(&s, id, Some("quickParts"), "Nested").unwrap();
    assert_eq!(saved.drawings.len(), 5, "both groups and all three boxes");

    let r =
        s.run("insert.quickPart", &json!({"gallery": "quickParts", "name": "Nested", "x": 400, "y": 400}));
    let ids = new_ids(&r.unwrap());
    assert_eq!(ids.len(), 5);
    let all = drawings(&s, id);
    let new: Vec<&Drawing> = all.iter().filter(|d| ids.contains(&d.id)).collect();
    let groups: Vec<&&Drawing> = new.iter().filter(|d| d.kind == DrawingKind::Group).collect();
    assert_eq!(groups.len(), 2);
    let top = groups.iter().find(|g| g.group.is_none()).unwrap();
    let sub = groups.iter().find(|g| g.group == Some(top.id)).unwrap();
    let in_sub: Vec<&str> = new.iter().filter(|d| d.group == Some(sub.id)).map(|d| d.text.as_str()).collect();
    assert_eq!(in_sub, ["A", "B"], "the inner group keeps its boxes");
    assert!(new.iter().any(|d| d.text == "C" && d.group == Some(top.id)));
    assert_eq!((sub.x, sub.y, sub.w, sub.h), (400.0, 400.0, 180.0, 40.0), "the box of its members");
    assert_eq!(s.composes[&id].selected_drawings(), vec![top.id]);
}

#[test]
fn a_canvas_is_saved_with_the_shapes_in_it() {
    let (mut s, id) = setup();
    let canvas = s
        .run("insert.shape", &json!({"compose": id, "kind": "canvas", "x": 0, "y": 0, "w": 300, "h": 200}))
        .unwrap()["drawing"]
        .as_u64()
        .unwrap() as u32;
    let oval = s
        .run("insert.shape", &json!({"compose": id, "kind": "oval", "x": 20, "y": 30, "w": 50, "h": 50}))
        .unwrap()["drawing"]
        .as_u64()
        .unwrap() as u32;
    assert_eq!(drawings(&s, id).iter().find(|d| d.id == oval).unwrap().canvas, Some(canvas));
    s.run("drawing.select", &json!({"id": canvas})).unwrap();
    s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "textBoxes", "name": "Pad"})).unwrap();
    let saved = super::find_block(&s, id, Some("textBoxes"), "Pad").unwrap();
    assert_eq!(saved.drawings.len(), 2, "the canvas and the shape in it");
    assert!(saved.drawings.iter().any(|d| d.id == oval && d.canvas == Some(canvas)));
    let r = s.run("insert.quickPart", &json!({"gallery": "textBoxes", "name": "Pad", "x": 500, "y": 300}));
    let ids = new_ids(&r.unwrap());
    assert_eq!(ids.len(), 2);
    let all = drawings(&s, id);
    let new_canvas = all.iter().find(|d| ids.contains(&d.id) && d.kind == DrawingKind::Canvas).unwrap();
    let new_oval = all.iter().find(|d| ids.contains(&d.id) && d.kind != DrawingKind::Canvas).unwrap();
    assert_eq!(new_oval.canvas, Some(new_canvas.id), "the shape is in the new canvas");
    assert_eq!((new_oval.x, new_oval.y), (520.0, 330.0));
    assert_eq!(s.composes[&id].selected_drawings(), vec![new_canvas.id], "the canvas is selected");
}

/// Release check: an In Line with Text drawing was saved In Line, came back without a place in the text and
/// was dropped by the next edit.
#[test]
fn an_inline_drawing_comes_back_floating_and_survives_typing() {
    let (mut s, id) = setup();
    text_box(&mut s, "Inline", 10, 10);
    s.run("drawing.wrapText", &json!({"compose": id, "wrap": "inline"})).unwrap();
    s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "textBoxes", "name": "Inl"})).unwrap();
    let saved = super::find_block(&s, id, Some("textBoxes"), "Inl").unwrap();
    assert_eq!(saved.drawings[0].wrap, WrapStyle::Square, "saved floating");
    // An entry saved In Line (by an older build, or edited in the profile) floats too.
    let mut legacy = saved.clone();
    legacy.name = "Old".into();
    legacy.drawings[0].wrap = WrapStyle::Inline;
    s.composes.get_mut(&id).unwrap().building_blocks.push(legacy);
    for name in ["Inl", "Old"] {
        let before = drawings(&s, id).len();
        let r = s.run("insert.quickPart", &json!({"gallery": "textBoxes", "name": name})).unwrap();
        let ids = new_ids(&r);
        s.run("drawing.select", &json!({"id": null})).unwrap();
        s.run("compose.insertText", &json!({"compose": id, "text": "x"})).unwrap();
        let all = drawings(&s, id);
        assert_eq!(all.len(), before + 1, "{name}: still there after typing");
        assert_eq!(all.iter().find(|d| d.id == ids[0]).unwrap().wrap, WrapStyle::Square);
    }
}

/// Release check: AutoText while typing (F3) took the typed name away and inserted nothing for an entry
/// that holds a drawing.
#[test]
fn auto_text_with_a_drawing_replaces_the_typed_name_with_it() {
    let (mut s, id) = setup();
    let box_id = text_box(&mut s, "Stamp text", 10, 10);
    s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "autoText", "name": "Stampbox"}))
        .unwrap();
    s.run("drawing.delete", &json!({"id": box_id})).unwrap();
    s.run("drawing.select", &json!({"id": null})).ok();
    s.run("compose.insertText", &json!({"compose": id, "text": " Stampbox"})).unwrap();
    let typed = body_text(&s, id);
    let undo_steps = s.composes[&id].edit_undo.len();
    let r = s.run("insert.autoText", &json!({"compose": id})).unwrap();
    assert_eq!(new_ids(&r).len(), 1);
    assert!(!body_text(&s, id).contains("Stampbox"), "the typed name goes");
    let all = drawings(&s, id);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].text, "Stamp text");
    assert_eq!(s.composes[&id].edit_undo.len(), undo_steps + 1, "one undo step");
    s.run("compose.editUndo", &json!({"compose": id})).unwrap();
    assert_eq!(body_text(&s, id), typed);
    assert!(drawings(&s, id).is_empty());
}

/// Release check: a drawing selected before text was selected (Select All, compose.select) still won, so
/// Save Selection saved the old drawing instead of the text on screen.
#[test]
fn selecting_text_after_a_drawing_saves_the_text() {
    let (mut s, id) = setup();
    text_box(&mut s, "Box", 10, 10);
    assert_eq!(s.composes[&id].selected_drawings().len(), 1);
    s.run("compose.select", &json!({"compose": id, "all": true})).unwrap();
    assert!(s.composes[&id].selected_drawings().is_empty(), "selecting text unselects the drawing");
    s.run("insert.saveToGallery", &json!({"compose": id, "gallery": "quickParts", "name": "Hi"})).unwrap();
    let saved = super::find_block(&s, id, Some("quickParts"), "Hi").unwrap();
    assert!(saved.drawings.is_empty());
    assert!(Doc { blocks: saved.blocks, ..Doc::default() }.text_content().contains("Hello"), "the text");
    // A caret move (nothing selected) leaves a selected drawing alone.
    let box2 = text_box(&mut s, "Two", 50, 50);
    s.run("compose.select", &json!({"compose": id, "anchor": {"path": [0], "offset": 1}})).unwrap();
    assert_eq!(s.composes[&id].selected_drawings(), vec![box2]);
}
