//! kittest tests for drawing objects in the compose body (drawing them with a tool, selecting, moving,
//! typing into them, deleting them, the Shape Format tab), the table pencil, and the picture dialogs (Insert
//! Icons, the chart data sheet).

use egui::{Event, Modifiers, PointerButton, Pos2, Rect, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use mailcraft_engine::Session;
use mailcraft_engine::compose::{ComposeKind, ComposeState};
use mailcraft_engine::model::{Block, DrawingKind, Mailbox};

use super::editor::{BODY_LABEL, PAD};
use super::*;

fn app_with_body(text: &str) -> (App, u64) {
    let mut s = Session::new(Mailbox::new_local("Local"));
    let cid = s.open_compose(ComposeState { kind: ComposeKind::New, ..ComposeState::default() });
    s.run("compose.insertText", &json!({"compose": cid, "text": text})).unwrap();
    s.notices.clear();
    (App::new(s), cid)
}

fn harness(app: App) -> Harness<'static, App> {
    Harness::builder().with_size([1600.0, 1000.0]).build_ui_state(
        |ui, app: &mut App| {
            let ctx = ui.ctx().clone();
            compose_windows(&ctx, app);
            // The test window is drawn inside the main one: the main handler routes the keys the window
            // left to the compose (after it, as a window of its own does).
            crate::keys::handle(&ctx, app, crate::Surface::Main);
            crate::dialogs::dialogs(&ctx, app);
        },
        app,
    )
}

fn compose(h: &Harness<'_, App>, cid: u64) -> ComposeState {
    h.state().s.composes.get(&cid).cloned().unwrap_or_default()
}

fn body(h: &Harness<'_, App>) -> Rect {
    h.get_by_label(BODY_LABEL).rect()
}

fn button(h: &mut Harness<'_, App>, pos: Pos2, pressed: bool) {
    h.input_mut().events.push(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    });
    h.run();
}

/// Presses at `a`, moves to `b` in steps, releases.
fn drag(h: &mut Harness<'_, App>, a: Pos2, b: Pos2) {
    h.input_mut().events.push(Event::PointerMoved(a));
    h.run();
    button(h, a, true);
    for t in 1..=4 {
        h.input_mut().events.push(Event::PointerMoved(a + (b - a) * (t as f32 / 4.0)));
        h.run();
    }
    button(h, b, false);
}

/// A click: press and release in one frame (a held press in the text keeps repainting to drag-select).
fn click(h: &mut Harness<'_, App>, p: Pos2) {
    h.input_mut().events.push(Event::PointerMoved(p));
    h.run();
    for pressed in [true, false] {
        h.input_mut().events.push(Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run();
}

#[test]
fn a_shape_is_drawn_by_dragging_then_moved_sized_and_deleted() {
    let (app, cid) = app_with_body("Hello");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("insert.shape", json!({"compose": cid, "kind": "oval", "draw": true}));
    h.run();
    let origin = body(&h).min + vec2(PAD, PAD);
    drag(&mut h, origin + vec2(100.0, 40.0), origin + vec2(220.0, 120.0));
    let c = compose(&h, cid);
    assert_eq!(c.body.drawings.len(), 1, "the drag drew an oval");
    let d = c.body.drawings[0].clone();
    assert_eq!((d.shape.as_str(), d.kind), ("oval", DrawingKind::Shape));
    assert!((d.x - 100.0).abs() < 2.0 && (d.y - 40.0).abs() < 2.0, "{d:?}");
    assert!((d.w - 120.0).abs() < 2.0 && (d.h - 80.0).abs() < 2.0, "{d:?}");
    assert_eq!(c.draw_tool, None, "the tool is put away after one shape");
    assert_eq!(c.selected_drawing, Some(d.id));
    h.run();
    assert!(h.query_by_label("Shape Format").is_some(), "the Shape Format tab shows for a selected drawing");
    // Drag the oval by its middle.
    let mid = origin + vec2(160.0, 80.0);
    drag(&mut h, mid, mid + vec2(50.0, 20.0));
    let d = compose(&h, cid).body.drawings[0].clone();
    assert!((d.x - 150.0).abs() < 2.0 && (d.y - 60.0).abs() < 2.0, "moved: {d:?}");
    // The bottom-right handle sizes it.
    let corner = origin + vec2(d.x + d.w, d.y + d.h);
    drag(&mut h, corner, corner + vec2(30.0, 10.0));
    let d = compose(&h, cid).body.drawings[0].clone();
    assert!((d.w - 150.0).abs() < 2.0 && (d.h - 90.0).abs() < 2.0, "sized: {d:?}");
    // Arrow keys nudge it; Delete removes it.
    h.key_press(egui::Key::ArrowRight);
    h.run();
    assert!((compose(&h, cid).body.drawings[0].x - d.x - 8.0).abs() < 0.5);
    h.key_press(egui::Key::Delete);
    h.run();
    assert!(compose(&h, cid).body.drawings.is_empty());
    assert_eq!(compose(&h, cid).body.to_plain_text(), "Hello", "the text was not touched");
    h.run();
    assert!(h.query_by_label("Shape Format").is_none());
}

#[test]
fn a_new_text_box_takes_typing_in_place() {
    let (app, cid) = app_with_body("Body text");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("insert.textBox", json!({"compose": cid, "preset": "draw"}));
    h.run();
    let origin = body(&h).min + vec2(PAD, PAD);
    drag(&mut h, origin + vec2(300.0, 60.0), origin + vec2(500.0, 140.0));
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.body.drawings.len(), 1);
    assert_eq!(c.body.drawings[0].kind, DrawingKind::TextBox);
    // The caret is in the box: what is typed goes there, not into the message text.
    h.input_mut().events.push(Event::Text("Inside the box".into()));
    h.run();
    h.run();
    // A click outside the box ends the typing.
    click(&mut h, origin + vec2(20.0, 300.0));
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.body.drawings[0].text, "Inside the box");
    assert_eq!(c.body.to_plain_text(), "Body text");
    assert_eq!(c.selected_drawing, None, "clicking the text deselects the box");
}

/// Regression: text typed into a text box stayed in the editor until the edit ended, so Ctrl+S saved and
/// Ctrl+Enter sent the message without it; Ctrl+B went to the hidden body selection.
#[test]
fn shortcuts_while_typing_in_a_text_box_see_the_typed_text() {
    let (app, cid) = app_with_body("Body text");
    let mut h = harness(app);
    h.run();
    // The body text is selected behind the box drawn next.
    h.state_mut().run("format.selectAll", json!({"compose": cid}));
    h.run();
    h.state_mut().run("insert.textBox", json!({"compose": cid, "preset": "draw"}));
    h.run();
    let origin = body(&h).min + vec2(PAD, PAD);
    drag(&mut h, origin + vec2(300.0, 60.0), origin + vec2(500.0, 140.0));
    h.run();
    assert!(compose(&h, cid).has_selection(), "the body selection stays");
    h.input_mut().events.push(Event::Text("Kept".into()));
    h.run();
    h.run();
    let c = compose(&h, cid);
    assert!(c.body.drawings[0].text.is_empty(), "still typing: not committed yet");
    assert_eq!(c.body.to_plain_text(), "Body text", "the typing went into the box");
    // Ctrl+B does not bold the body while the box has the caret.
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::B);
    h.run();
    let c = compose(&h, cid);
    let blocks = serde_json::to_string(&c.body.blocks).unwrap();
    assert!(!blocks.contains("\"bold\":true"), "the body was not formatted: {blocks}");
    // Ctrl+S saves the draft with the text that was typed.
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::S);
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.body.drawings[0].text, "Kept", "Save saw the typed text");
    assert!(!c.dirty, "saved");
    // Typing goes on in the box after the save, and any later command sees it.
    h.input_mut().events.push(Event::Text(" more".into()));
    h.run();
    h.state_mut().run("compose.saveDraft", json!({"compose": cid}));
    assert_eq!(compose(&h, cid).body.drawings[0].text, "Kept more");
}

#[test]
fn ctrl_enter_and_ctrl_delete_are_not_taken_by_a_selected_drawing() {
    let (app, cid) = app_with_body("Hello");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("insert.shape", json!({"compose": cid, "kind": "rectangle", "draw": true}));
    h.run();
    let origin = body(&h).min + vec2(PAD, PAD);
    drag(&mut h, origin + vec2(100.0, 40.0), origin + vec2(220.0, 120.0));
    h.run();
    assert!(compose(&h, cid).selected_drawing.is_some());
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Delete);
    h.run();
    assert_eq!(compose(&h, cid).body.drawings.len(), 1, "Ctrl+Delete did not delete the shape");
    h.key_press(egui::Key::Delete);
    h.run();
    assert!(compose(&h, cid).body.drawings.is_empty(), "Delete still does");
    // A canvas holds no text: typing with it selected is not swallowed.
    h.state_mut().run("insert.shape", json!({"compose": cid, "kind": "canvas", "draw": true}));
    h.run();
    drag(&mut h, origin + vec2(100.0, 40.0), origin + vec2(300.0, 160.0));
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.body.drawings.first().map(|d| d.kind), Some(DrawingKind::Canvas));
    assert!(c.selected_drawing.is_some());
    h.input_mut().events.push(Event::Text("x".into()));
    h.run();
    h.run();
    assert!(compose(&h, cid).body.to_plain_text().contains('x'), "typing reached the message text");
}

#[test]
fn the_table_pencil_draws_a_table_in_the_body() {
    let (app, cid) = app_with_body("Above");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("insert.table", json!({"compose": cid, "mode": "draw"}));
    h.run();
    let origin = body(&h).min + vec2(PAD, PAD);
    drag(&mut h, origin + vec2(10.0, 8.0), origin + vec2(250.0, 60.0));
    let tables = |c: &ComposeState| c.body.blocks.iter().filter(|b| matches!(b, Block::Table { .. })).count();
    assert_eq!(tables(&compose(&h, cid)), 1, "{:?}", compose(&h, cid).body.blocks);
    assert_eq!(compose(&h, cid).draw_tool.as_deref(), Some("tablePencil"), "the pencil stays out");
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(compose(&h, cid).draw_tool, None);
}

#[test]
fn insert_icons_inserts_the_picked_icons_as_pictures() {
    let (app, cid) = app_with_body("x");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("insert.icon", json!({"compose": cid}));
    h.run();
    h.run();
    // Picked in the grid (scrolled to), then found with the search box.
    h.get_by_label("Light bulb").scroll_to_me();
    h.run();
    h.get_by_label("Light bulb").click();
    h.run();
    h.get_by_label("Search:").focus();
    h.run();
    h.get_by_label("Search:").type_text("trop");
    h.run();
    assert!(h.query_by_label("Light bulb").is_none(), "the search filters the grid");
    h.get_by_label("Trophy").click();
    h.run();
    h.get_by_label("Insert (2)").click();
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.inline_images.len(), 2);
    assert!(c.inline_images.iter().all(|i| i.mime_type == "image/png"));
}

#[test]
fn the_chart_data_sheet_inserts_and_edits_a_chart() {
    let (app, cid) = app_with_body("x");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("insert.chart", json!({"compose": cid}));
    h.run();
    h.run();
    h.get_by_label("Add Series").click();
    h.run();
    h.get_by_label("Pie").click();
    h.run();
    h.query_all_by_label("Insert").last().unwrap().click();
    h.run();
    let c = compose(&h, cid);
    let g = c.generated.iter().find(|g| g.kind == "chart").expect("a chart recipe").clone();
    assert_eq!(g.spec["kind"], "pie");
    assert_eq!(g.spec["series"].as_array().map(Vec::len), Some(3), "the added series");
    // Editing it opens the same sheet with its data.
    h.state_mut().run("insert.editPicture", json!({"compose": cid, "cid": g.content_id}));
    h.run();
    h.run();
    assert!(h.query_by_label("Edit Chart Data").is_some());
    h.get_by_label("Line with Markers").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    let c = compose(&h, cid);
    assert!(c.generated.iter().any(|g| g.kind == "chart" && g.spec["kind"] == "line"));
    assert_eq!(c.images().len(), 1, "edited in place");
}

#[test]
fn smart_art_is_picked_from_the_gallery_and_inserted_from_the_text_pane() {
    let (app, cid) = app_with_body("x");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("insert.smartArt", json!({"compose": cid}));
    h.run();
    h.run();
    h.get_by_label("Basic Cycle").click();
    h.run();
    h.query_all_by_label("Insert").last().unwrap().click();
    h.run();
    let c = compose(&h, cid);
    let g = c.generated.iter().find(|g| g.kind == "smartArt").expect("a SmartArt recipe");
    assert_eq!(g.spec["layout"], "cycle");
    assert_eq!(g.spec["items"].as_array().map(Vec::len), Some(3), "the text pane's three lines");
    assert!(h.state().ui.dialogs.is_empty());
}
