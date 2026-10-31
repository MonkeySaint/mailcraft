//! kittest tests for the compose window's Insert, Options and Review features: the Table size grid, the
//! building-block galleries and Create New Building Block, Screenshot through a capture backend, Draw Table,
//! recently used symbols and Read Aloud.

use std::sync::Arc;

use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use mailcraft_engine::Session;
use mailcraft_engine::compose::{ComposeKind, ComposeState};
use mailcraft_engine::model::{Block, Mailbox};

use super::*;
use crate::ribbon::state::dyn_items;

fn app_with_body(text: &str) -> (App, u64) {
    let mut s = Session::new(Mailbox::new_local("Local"));
    let cid = s.open_compose(ComposeState { kind: ComposeKind::New, ..ComposeState::default() });
    s.run("compose.insertText", &json!({"compose": cid, "text": text})).unwrap();
    s.notices.clear();
    (App::new(s), cid)
}

/// Compose windows plus the dialogs area.
fn harness(app: App) -> Harness<'static, App> {
    crate::test_harness().with_size([1600.0, 1000.0]).build_ui_state(
        |ui, app: &mut App| {
            let ctx = ui.ctx().clone();
            compose_windows(&ctx, app);
            crate::dialogs::dialogs(&ctx, app);
        },
        app,
    )
}

fn compose(h: &Harness<'_, App>, cid: u64) -> ComposeState {
    h.state().s.composes.get(&cid).cloned().unwrap_or_default()
}

fn dialog_names(h: &Harness<'_, App>) -> Vec<String> {
    h.state().ui.dialogs.iter().map(|(n, _)| n.clone()).collect()
}

fn first_table(c: &ComposeState) -> Option<(usize, usize)> {
    c.body.blocks.iter().find_map(|b| match b {
        Block::Table { rows, .. } => Some((rows.len(), rows.first().map_or(0, Vec::len))),
        _ => None,
    })
}

#[test]
fn table_menu_grid_inserts_the_picked_size() {
    let (app, cid) = app_with_body("x");
    let mut h = harness(app);
    h.run();
    h.get_by_label("Insert").click();
    h.run();
    h.get_by_label("Table").click();
    h.run();
    // 7 × 6 cells, each named by its size.
    assert!(h.query_by_label("7x6 Table").is_some());
    h.get_by_label("3x2 Table").hover();
    h.run();
    h.get_by_label("3x2 Table").click();
    h.run();
    assert_eq!(first_table(&compose(&h, cid)), Some((2, 3)), "two rows of three columns");
}

#[test]
fn building_blocks_save_with_a_name_and_appear_in_their_gallery() {
    let (app, cid) = app_with_body("Kind regards");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("compose.select", json!({"compose": cid, "all": true}));
    h.state_mut().run("insert.saveToGallery", json!({"compose": cid, "gallery": "autoText"}));
    h.run();
    h.run();
    assert!(dialog_names(&h).contains(&"createBuildingBlock".to_owned()));
    h.get_by_label("OK").click();
    h.run();
    h.run();
    assert!(!dialog_names(&h).contains(&"createBuildingBlock".to_owned()));
    let items = dyn_items(h.state(), crate::Surface::Compose(cid), "blocks:autoText");
    assert_eq!(items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["Kind regards"]);
    assert!(dyn_items(h.state(), crate::Surface::Compose(cid), "blocks:quickParts").is_empty());
    // The organizer lists it and inserts it.
    h.state_mut().run("compose.select", json!({"compose": cid, "anchor": {"path": [0], "offset": 0}}));
    h.state_mut().run("insert.quickPart", json!({"compose": cid, "gallery": "autoText"}));
    h.run();
    h.run();
    assert!(dialog_names(&h).contains(&"quickParts".to_owned()));
    h.query_all_by_label("Kind regards").last().unwrap().click();
    h.run();
    h.query_all_by_label("Insert").last().unwrap().click();
    h.run();
    assert_eq!(compose(&h, cid).body.to_plain_text().matches("Kind regards").count(), 2);
}

/// Runs frames until `done` (a capture thread finishes on its own time).
fn run_until(h: &mut Harness<'_, App>, done: impl Fn(&Harness<'_, App>) -> bool) {
    for _ in 0..300 {
        h.step();
        if done(h) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("timed out");
}

#[test]
fn screenshot_captures_windows_and_screen_clips_through_the_backend() {
    // The only test that installs a capture backend (it is process-wide).
    set_capture(Some(Arc::new(capture::tests::Fake)));
    let (app, cid) = app_with_body("x");
    let mut h = harness(app);
    h.run();
    let titles: Vec<String> = dyn_items(h.state(), crate::Surface::Compose(cid), "screenWindows")
        .into_iter()
        .map(|i| i.label)
        .collect();
    assert_eq!(compose(&h, cid).title(), "Untitled - Message (HTML)");
    assert_eq!(
        titles,
        ["Notes - Editor", "Calculator", MAIN_WINDOW_TITLE],
        "blank titles and the compose's own window are left out; the main window is listed"
    );
    let ctx = h.ctx.clone();
    // A window: captured off the UI thread, then inserted (its title is not put in the alt text).
    assert!(ribbon_command(
        &ctx,
        h.state_mut(),
        cid,
        "insert.screenshot",
        &json!({"compose": cid, "windowId": 7, "window": "Notes - Editor"})
    ));
    run_until(&mut h, |h| !compose(h, cid).inline_images.is_empty());
    assert!(compose(&h, cid).images().iter().any(|(_, alt)| alt == "Screenshot"));
    // A window that cannot be captured: the saved-picture dialog says why.
    ribbon_command(
        &ctx,
        h.state_mut(),
        cid,
        "insert.screenshot",
        &json!({"compose": cid, "windowId": 8, "window": "Calculator"}),
    );
    run_until(&mut h, |h| dialog_names(h).contains(&"screenshot".to_owned()));
    // Since egui 0.37 the new dialog's text reaches the accessibility tree a frame after it opens.
    h.run();
    h.query_by_label_contains("That window has closed.").unwrap();
    h.get_by_label("Cancel").click();
    h.run();
    // Screen Clipping: the screen comes back for choosing the region; Insert Whole Screen inserts all of it.
    // The compose window is minimised first (and restored when the screen is back).
    ribbon_command(&ctx, h.state_mut(), cid, "insert.screenshot", &json!({"compose": cid, "clip": true}));
    assert_eq!(capture::minimized_for(&ctx, cid), Some(true));
    run_until(&mut h, |h| dialog_names(h).contains(&"screenClip".to_owned()));
    h.run();
    h.get_by_label("Insert Whole Screen").click();
    h.run();
    h.run();
    assert_eq!(compose(&h, cid).inline_images.len(), 2);
    assert!(!dialog_names(&h).contains(&"screenClip".to_owned()));
    // Regression: through the desktop portal (Wayland) the window is not minimised, since the program
    // could not bring it back there and the portal's picker is above every window anyway.
    capture::tests::PORTAL.store(true, std::sync::atomic::Ordering::Relaxed);
    ribbon_command(&ctx, h.state_mut(), cid, "insert.screenshot", &json!({"compose": cid, "clip": true}));
    assert_eq!(capture::minimized_for(&ctx, cid), Some(false));
    run_until(&mut h, |h| dialog_names(h).contains(&"screenClip".to_owned()));
    capture::tests::PORTAL.store(false, std::sync::atomic::Ordering::Relaxed);
    h.state_mut().ui.dialogs.clear();
    h.run();
    // Regression: a capture that never comes back (a portal that does not answer) is given up after
    // CAPTURE_TIMEOUT, so Screenshot works again and the saved-picture dialog says why.
    let (release, wait) = std::sync::mpsc::channel();
    set_capture(Some(Arc::new(capture::tests::Stuck(std::sync::Mutex::new(Some(wait))))));
    ribbon_command(&ctx, h.state_mut(), cid, "insert.screenshot", &json!({"compose": cid, "clip": true}));
    assert!(capture::busy(&ctx, cid));
    h.step();
    assert!(capture::busy(&ctx, cid), "still waiting");
    let later = std::time::Instant::now() + capture::CAPTURE_TIMEOUT;
    capture::poll_at(&ctx, h.state_mut(), later);
    assert!(!capture::busy(&ctx, cid), "given up");
    h.run();
    h.query_by_label_contains(capture::CAPTURE_TIMED_OUT).unwrap();
    h.get_by_label("Cancel").click();
    h.run();
    let _ = release.send(());
    // Without a backend: the reason, and a saved picture instead.
    set_capture(None);
    assert!(ribbon_command(
        &ctx,
        h.state_mut(),
        cid,
        "insert.screenshot",
        &json!({"compose": cid, "clip": true})
    ));
    h.run();
    h.run();
    h.query_by_label_contains(NO_CAPTURE_BACKEND).unwrap();
    let items = dyn_items(h.state(), crate::Surface::Compose(cid), "screenWindows");
    assert_eq!(items.len(), 1, "one entry explaining why no windows are listed");
}

#[test]
fn draw_table_pad_makes_a_table_with_the_drawn_columns() {
    let (app, cid) = app_with_body("x");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("insert.table", json!({"compose": cid, "mode": "draw", "pad": true}));
    h.run();
    h.run();
    let pad = h.get_by_label("Table drawing area").rect();
    let drag = |h: &mut Harness<'_, App>, a: Pos2, b: Pos2| {
        let (a, b) = (pad.min + a.to_vec2(), pad.min + b.to_vec2());
        h.input_mut().events.push(Event::PointerMoved(a));
        h.run();
        h.input_mut().events.push(Event::PointerButton {
            pos: a,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        h.run();
        for t in 1..=4 {
            let p = a + (b - a) * (t as f32 / 4.0);
            h.input_mut().events.push(Event::PointerMoved(p));
            h.run();
        }
        h.input_mut().events.push(Event::PointerButton {
            pos: b,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        h.run();
    };
    drag(&mut h, Pos2::new(20.0, 20.0), Pos2::new(320.0, 220.0));
    drag(&mut h, Pos2::new(120.0, 10.0), Pos2::new(121.0, 230.0));
    drag(&mut h, Pos2::new(10.0, 120.0), Pos2::new(330.0, 121.0));
    h.query_all_by_label("Insert").last().unwrap().click();
    h.run();
    let c = compose(&h, cid);
    assert_eq!(first_table(&c), Some((2, 2)));
    let widths: Vec<Option<f32>> = c
        .body
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Table { rows, .. } => rows.first().map(|r| r.iter().map(|c| c.width_px).collect()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(
        matches!(widths.as_slice(), [Some(a), Some(b)] if (a - 100.0).abs() < 4.0 && (b - 200.0).abs() < 4.0),
        "{widths:?}"
    );
}

#[test]
fn equation_dialog_previews_and_inserts_a_drawn_equation() {
    let (app, cid) = app_with_body("x");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("insert.equation", json!({"compose": cid}));
    h.run();
    h.run();
    h.get_by_label("Pythagorean Theorem").click();
    h.run();
    h.run();
    assert!(h.query_by_label("Equation preview").is_some(), "the preview is drawn");
    h.query_all_by_label("Insert").last().unwrap().click();
    h.run();
    let c = compose(&h, cid);
    assert!(c.images().iter().any(|(_, alt)| alt == "a^2+b^2=c^2"));
    assert_eq!(c.inline_images.first().map(|i| i.mime_type.as_str()), Some("image/png"));
}

#[test]
fn thesaurus_looks_up_other_words_and_replaces_the_selection() {
    let (app, cid) = app_with_body("a big plan");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("compose.select", json!({"compose": cid, "anchor": {"path": [0], "offset": 3}}));
    h.state_mut().run("review.thesaurus", json!({"compose": cid}));
    h.run();
    h.run();
    // Look up another word, go back, then insert a synonym of the selected one.
    h.query_all_by_role(egui::accesskit::Role::TextInput).last().unwrap().type_text("s");
    h.run();
    h.get_by_label("Look Up").click();
    h.run();
    h.get_by_label("Back").click();
    h.run();
    h.get_by_label("large").click();
    h.run();
    h.query_all_by_label("Insert").last().unwrap().click();
    h.run();
    assert_eq!(compose(&h, cid).body.to_plain_text().trim_end(), "a large plan");
}

#[test]
fn symbols_from_the_menu_join_the_recently_used_list() {
    let (app, cid) = app_with_body("x");
    let mut h = harness(app);
    h.run();
    let ctx = h.ctx.clone();
    assert!(ribbon_command(
        &ctx,
        h.state_mut(),
        cid,
        "insert.symbol",
        &json!({"compose": cid, "char": "U+2603"})
    ));
    assert!(ribbon_command(&ctx, h.state_mut(), cid, "insert.symbol", &json!({"compose": cid, "char": "€"})));
    assert!(compose(&h, cid).body.to_plain_text().contains("\u{2603}€"));
    let recent = recent_symbols();
    assert_eq!(recent.first().map(String::as_str), Some("€"));
    assert!(recent.contains(&"\u{2603}".to_owned()));
    let items = dyn_items(h.state(), crate::Surface::Compose(cid), "recentSymbols");
    assert_eq!(items.first().map(|i| i.label.as_str()), Some("€"));
    // A bad code is refused and not remembered.
    ribbon_command(&ctx, h.state_mut(), cid, "insert.symbol", &json!({"compose": cid, "char": "U+ZZ"}));
    assert_eq!(recent_symbols().first().map(String::as_str), Some("€"));
}

#[test]
fn read_aloud_speaks_paragraph_by_paragraph() {
    let fake = Arc::new(crate::speech::tests::Fake::default());
    crate::speech::set_speech(Some(fake.clone()));
    let (app, cid) = app_with_body("First paragraph.\nSecond one.");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("review.readAloud", json!({"compose": cid}));
    h.run();
    h.run();
    h.get_by_label("Play").click();
    for _ in 0..8 {
        h.step();
    }
    let said: Vec<String> = fake.said.lock().unwrap().iter().map(|(t, _)| t.clone()).collect();
    assert_eq!(said, ["First paragraph.", "Second one."]);
    h.query_all_by_label("Close").last().unwrap().click();
    h.step();
    h.step();
    assert!(*fake.stops.lock().unwrap() >= 1, "closing stops speech");
    crate::speech::set_speech(None);
    h.state_mut().run("review.readAloud", json!({"compose": cid}));
    h.run();
    h.run();
    h.query_by_label_contains("Speech is not available").unwrap();
    assert!(h.query_by_label("Play").is_none());
}

#[test]
fn language_dialog_keeps_a_language_it_does_not_list() {
    // Regression: a language set elsewhere (CLI, MCP) and not in the dialog's list showed as English (United
    // States), and OK, even to tick only "Do not check spelling or grammar", changed it to en-US.
    let (app, cid) = app_with_body("Hej");
    let mut h = harness(app);
    h.run();
    h.state_mut().run("review.language", json!({"compose": cid, "lang": "sv-SE"}));
    h.state_mut().run("review.language", json!({"compose": cid}));
    h.run();
    h.run();
    assert!(h.query_by_label_contains("sv-SE").is_some(), "the message's own language is listed");
    h.get_by_label("Do not check spelling or grammar").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.language.as_deref(), Some("sv-SE"));
    assert!(c.no_proof);
    // A listed language is picked as before.
    h.state_mut().run("review.language", json!({"compose": cid}));
    h.run();
    h.run();
    h.get_by_label_contains("French (Canada)").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    h.run();
    assert_eq!(compose(&h, cid).language.as_deref(), Some("fr-CA"));
}

#[test]
fn message_options_keeps_a_save_sent_folder_past_the_list_limit() {
    // Regression: the choice was an index into a list cut to 300 folders; a folder past the cut showed as
    // "Sent Items (default)" and OK reset the message to it.
    use mailcraft_engine::compose::SaveSentTo;
    use mailcraft_engine::model::{FolderId, FolderRole};
    let (mut app, cid) = app_with_body("x");
    let account = app.s.mailbox.default_account().map(|a| a.id).unwrap();
    let inbox = app.s.mailbox.role_folder(account, FolderRole::Inbox).unwrap();
    let mut last = FolderId(0);
    for i in 0..310 {
        last = app.s.mailbox.create_folder(inbox, &format!("Z{i:03}")).unwrap();
    }
    if let Some(c) = app.s.composes.get_mut(&cid) {
        c.save_sent = SaveSentTo::Folder(last);
    }
    let mut h = harness(app);
    h.run();
    h.state_mut().run("compose.messageOptions", json!({"compose": cid}));
    h.run();
    h.run();
    h.get_by_label("Request a read receipt for this message").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    h.run();
    let c = compose(&h, cid);
    assert!(c.request_read_receipt, "the other change is applied");
    assert_eq!(c.save_sent, SaveSentTo::Folder(last), "the folder is kept");
}
