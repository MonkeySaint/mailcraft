//! Borders and Shading, Find and Replace › More, Insert Hyperlink, Sort Text and Define New Multilevel List,
//! driven through egui_kittest.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::{Value, json};

use crate::App;
use mailcraft_engine::sample::sample_session;
use mailcraft_model::{Block, Inline};

fn harness(app: App) -> Harness<'static, App> {
    let mut h = crate::test_harness().with_size(egui::vec2(1280.0, 900.0)).build_ui_state(
        |ui, app: &mut App| {
            let ctx = ui.ctx().clone();
            crate::dialogs::dialogs(&ctx, app);
        },
        app,
    );
    h.run_steps(2);
    h
}

fn step(h: &mut Harness<'_, App>) {
    h.run_steps(3);
}

/// An app with a compose holding `text`; the compose id.
fn app_with(text: &str) -> (App, u64) {
    let mut app = App::new(sample_session());
    let cid =
        app.run("mail.newEmail", json!({})).and_then(|v| v.get("compose").and_then(Value::as_u64)).unwrap();
    app.run("compose.insertText", json!({"compose": cid, "text": text})).unwrap();
    (app, cid)
}

fn open(h: &mut Harness<'_, App>, cmd: &str, p: Value) {
    h.state_mut().run(cmd, p);
    step(h);
}

fn dialogs(h: &Harness<'_, App>) -> Vec<String> {
    h.state().ui.dialogs.iter().map(|(n, _)| n.clone()).collect()
}

#[test]
fn borders_and_shading_dialog_applies_a_box_and_shading() {
    let (app, cid) = app_with("boxed");
    let mut h = harness(app);
    open(&mut h, "format.borders", json!({"compose": cid, "dialog": true}));
    assert_eq!(dialogs(&h), ["bordersAndShading"]);
    h.get_by_label("Shadow").click();
    step(&mut h);
    h.get_by_label("Shading").click();
    step(&mut h);
    let fill = h.query_all_by_role(egui::accesskit::Role::TextInput).next().unwrap();
    fill.focus();
    fill.type_text("#eeeeee");
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(dialogs(&h).is_empty());
    let c = h.state().s.composes.get(&cid).unwrap().clone();
    let b = c.para_borders_at_caret();
    assert!(b.shadow && b.top.is_some() && b.bottom.is_some() && b.left.is_some() && b.right.is_some());
    assert_eq!(c.para_at_caret().background.map(|x| x.to_hex()), Some("#eeeeee".into()));
}

#[test]
fn borders_dialog_preview_buttons_switch_single_edges() {
    let (app, cid) = app_with("ruled");
    let mut h = harness(app);
    open(&mut h, "format.borders", json!({"compose": cid}));
    h.get_by_label("Bottom").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    let b = h.state().s.composes.get(&cid).unwrap().para_borders_at_caret();
    assert!(b.bottom.is_some() && b.top.is_none() && b.left.is_none());
}

#[test]
fn find_dialog_more_options_find_whole_words() {
    let (app, cid) = app_with("concat cat");
    let mut h = harness(app);
    h.state_mut().run(
        "compose.select",
        json!({"compose": cid, "anchor": {"path": [0], "offset": 0}, "focus": {"path": [0], "offset": 0}}),
    );
    open(&mut h, "format.find", json!({"compose": cid}));
    assert_eq!(dialogs(&h), ["find"]);
    h.get_by_label("More >>").click();
    step(&mut h);
    h.get_by_label("Find whole words only").click();
    step(&mut h);
    let find = h.query_all_by_role(egui::accesskit::Role::TextInput).next().unwrap();
    find.focus();
    find.type_text("cat");
    step(&mut h);
    h.get_by_label("Find Next").click();
    step(&mut h);
    let c = h.state().s.composes.get(&cid).unwrap().clone();
    assert_eq!((c.selection.start().offset, c.selection.end().offset), (7, 10));
}

#[test]
fn insert_hyperlink_dialog_links_to_an_email_address_with_a_screen_tip() {
    let (app, cid) = app_with("");
    let mut h = harness(app);
    open(&mut h, "insert.link", json!({"compose": cid}));
    assert_eq!(dialogs(&h), ["insertLink"]);
    h.get_by_label("E-mail address").click();
    step(&mut h);
    h.get_by_label("ScreenTip...").click();
    step(&mut h);
    let inputs: Vec<_> = h.query_all_by_role(egui::accesskit::Role::TextInput).collect();
    assert_eq!(inputs.len(), 4, "text, ScreenTip, address, subject");
    drop(inputs);
    for (i, text) in [(0, "write to us"), (1, "Opens a new message"), (2, "team@example.com"), (3, "Hello")] {
        let input = h.query_all_by_role(egui::accesskit::Role::TextInput).nth(i).unwrap();
        input.focus();
        input.type_text(text);
        step(&mut h);
    }
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(dialogs(&h).is_empty(), "{:?}", dialogs(&h));
    let c = h.state().s.composes.get(&cid).unwrap().clone();
    let link = c.body.blocks.iter().find_map(|b| match b {
        Block::Paragraph { inlines, .. } => inlines.iter().find_map(|i| match i {
            Inline::Link { href, title, .. } => Some((href.clone(), title.clone())),
            _ => None,
        }),
        _ => None,
    });
    assert_eq!(
        link,
        Some(("mailto:team@example.com?subject=Hello".into(), Some("Opens a new message".into())))
    );
}

/// Picks `item` in the `nth` combo box of the open dialog.
fn choose(h: &mut Harness<'_, App>, nth: usize, item: &str) {
    let combo = h.query_all_by_role(egui::accesskit::Role::ComboBox).nth(nth).unwrap();
    combo.click();
    step(h);
    h.get_by_label(item).click();
    step(h);
}

/// Runs Sort Text over all of `text` with the combo choices `picks` ((combo index, item)); the sorted text.
fn sort_through_dialog(text: &str, picks: &[(usize, &str)]) -> String {
    let (app, cid) = app_with(text);
    let mut h = harness(app);
    h.state_mut().run("compose.select", json!({"compose": cid, "all": true}));
    open(&mut h, "format.sort", json!({"compose": cid}));
    assert_eq!(dialogs(&h), ["sortText"]);
    // Sort by, Type, Order, then (for paragraphs) the field separator.
    for (nth, item) in picks {
        choose(&mut h, *nth, item);
    }
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(dialogs(&h).is_empty(), "{:?}", dialogs(&h));
    h.state().s.composes.get(&cid).unwrap().body.to_plain_text()
}

#[test]
fn sort_text_dialog_sorts_descending_by_number() {
    // As text, "item 9" sorts after "item 10"; the Number type reads the numbers.
    let sorted = sort_through_dialog("item 9\nitem 10\nitem 100", &[(1, "Number"), (2, "Descending")]);
    assert_eq!(sorted, "item 100\nitem 10\nitem 9");
    let as_text = sort_through_dialog("item 9\nitem 10\nitem 100", &[(2, "Descending")]);
    assert_eq!(as_text, "item 9\nitem 100\nitem 10");
}

#[test]
fn sort_text_dialog_sorts_by_date() {
    let sorted = sort_through_dialog("Mar 3, 2026\nJan 15, 2027\nDec 1, 2025", &[(1, "Date")]);
    assert_eq!(sorted, "Dec 1, 2025\nMar 3, 2026\nJan 15, 2027");
}

#[test]
fn sort_text_dialog_sorts_by_a_field_between_commas() {
    let sorted =
        sort_through_dialog("Lee,30\nAnn,4\nBo,200", &[(0, "Field 2"), (1, "Number"), (3, "Commas")]);
    assert_eq!(sorted, "Ann,4\nLee,30\nBo,200");
}

#[test]
fn insert_hyperlink_browse_attaches_the_file() {
    // Regression: Browse put a file: link to the local file (and so the sender's folders) into the message.
    struct PickOne(std::path::PathBuf);
    impl crate::FileDialogs for PickOne {
        fn open_files(
            &self,
            _: &str,
            _: &[crate::FileFilter],
            _: bool,
            _: Option<&std::path::Path>,
        ) -> Option<Vec<std::path::PathBuf>> {
            Some(vec![self.0.clone()])
        }
        fn save_file(
            &self,
            _: &str,
            _: &str,
            _: &[crate::FileFilter],
            _: Option<&std::path::Path>,
        ) -> Option<std::path::PathBuf> {
            None
        }
        fn pick_folder(&self, _: &str, _: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
            None
        }
    }
    let dir = std::env::temp_dir().join(format!("mailcraft-link-browse-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("plan.txt");
    std::fs::write(&file, b"synthetic").unwrap();
    let (mut app, cid) = app_with("See ");
    app.set_file_dialogs(std::sync::Arc::new(PickOne(file.clone())));
    let mut h = harness(app);
    open(&mut h, "insert.link", json!({"compose": cid}));
    assert_eq!(dialogs(&h), ["insertLink"]);
    h.get_by_label("Browse...").click();
    step(&mut h);
    assert!(h.query_by_label("To attach: plan.txt").is_some());
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(dialogs(&h).is_empty(), "{:?}", dialogs(&h));
    let c = h.state().s.composes.get(&cid).unwrap().clone();
    assert_eq!(c.attachments.iter().map(|a| a.filename.as_str()).collect::<Vec<_>>(), ["plan.txt"]);
    assert!(!format!("{:?}", c.body).contains("file:"), "{:?}", c.body);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn define_new_multilevel_list_dialog_applies_a_library_start() {
    let (app, cid) = app_with("item");
    let mut h = harness(app);
    open(&mut h, "format.multilevel", json!({"compose": cid, "style": "define"}));
    assert_eq!(dialogs(&h), ["defineNewMultilevelList"]);
    // All nine levels are listed.
    for n in 1..=9 {
        assert!(h.query_all_by_label(&n.to_string()).next().is_some(), "level {n}");
    }
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(dialogs(&h).is_empty(), "{:?}", dialogs(&h));
    assert!(h.state().s.composes.get(&cid).unwrap().list_at_caret().is_some());
}
