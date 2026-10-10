//! kittest tests for compose windows and inline responses (embedded viewports in the test harness).

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use mailcraft_engine::compose::{ComposeKind, ComposeState};
use mailcraft_engine::model::Mailbox;
use mailcraft_engine::{Notice, Session};

use super::*;

fn app_with_compose(inline: bool) -> (App, u64) {
    let mut s = Session::new(Mailbox::new_local("Local"));
    let cid = s.open_compose(ComposeState { inline, kind: ComposeKind::New, ..ComposeState::default() });
    s.notices.clear();
    (App::new(s), cid)
}

fn window_harness(app: App) -> Harness<'static, App> {
    Harness::builder()
        .with_size([1600.0, 1000.0])
        .build_ui_state(|ui, app: &mut App| compose_windows(ui.ctx(), app), app)
}

fn inline_harness(app: App, cid: u64) -> Harness<'static, App> {
    Harness::builder().with_size([900.0, 700.0]).build_ui_state(
        move |ui, app: &mut App| {
            inline_compose(ui, app, cid);
        },
        app,
    )
}

fn compose(h: &Harness<'_, App>, cid: u64) -> ComposeState {
    h.state().s.composes.get(&cid).cloned().unwrap_or_default()
}

#[test]
fn typing_into_to_and_body_updates_the_compose() {
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();

    // Focus starts in To for a new message.
    h.get_by_role_and_label(Role::TextInput, "To").type_text("bob@example.com");
    h.run();
    assert_eq!(compose(&h, cid).to, "bob@example.com");

    h.get_by_role_and_label(Role::TextInput, "Subject").click();
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Subject").type_text("Lunch");
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.subject, "Lunch");
    assert_eq!(c.title(), "Lunch - Message (HTML)");

    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.get_by_label(BODY_LABEL).type_text("Hello");
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    h.get_by_label(BODY_LABEL).type_text("World");
    h.run();
    assert_eq!(compose(&h, cid).body.to_plain_text().trim_end(), "Hello\nWorld");

    // Backspace and arrow keys edit through the engine.
    h.key_press(egui::Key::Backspace);
    h.run();
    h.key_press(egui::Key::ArrowLeft);
    h.key_press(egui::Key::ArrowLeft);
    h.run();
    h.get_by_label(BODY_LABEL).type_text("x");
    h.run();
    assert_eq!(compose(&h, cid).body.to_plain_text().trim_end(), "Hello\nWoxrl");

    // Shift+Home selects to the line start; typing replaces the selection.
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Home);
    h.run();
    assert!(!compose(&h, cid).selection.is_collapsed());
    h.get_by_label(BODY_LABEL).type_text("Y");
    h.run();
    assert_eq!(compose(&h, cid).body.to_plain_text().trim_end(), "Hello\nYrl");
    assert!(compose(&h, cid).dirty);
}

#[test]
fn send_without_account_shows_the_notice() {
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();
    h.get_by_role_and_label(Role::TextInput, "To").type_text("a@example.com");
    h.run();
    h.get_by_label("Send").click();
    h.run();
    let notices = &h.state().s.notices;
    assert!(
        notices.contains(&Notice::Error(mailcraft_engine::cmd::compose::NO_ACCOUNT.into())),
        "notices: {notices:?}"
    );
    assert!(h.state().s.composes.contains_key(&cid), "the compose stays open");
}

#[test]
fn closing_with_changes_asks_to_save() {
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();
    h.get_by_role_and_label(Role::TextInput, "To").type_text("x@example.com");
    h.run();
    h.get_by_label("Close").click();
    h.run();
    h.get_by_label(SAVE_PROMPT);
    h.get_by_label("Cancel").click();
    h.run();
    assert!(h.query_by_label(SAVE_PROMPT).is_none());
    assert!(h.state().s.composes.contains_key(&cid));

    h.get_by_label("Close").click();
    h.run();
    h.get_by_label("No").click();
    h.run();
    assert!(!h.state().s.composes.contains_key(&cid));
}

#[test]
fn closing_without_changes_closes_right_away() {
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();
    h.get_by_label("Close").click();
    h.run();
    assert!(!h.state().s.composes.contains_key(&cid));
}

#[test]
fn inline_compose_types_discards_and_pops_out() {
    let (app, cid) = app_with_compose(true);
    let mut h = inline_harness(app, cid);
    h.run();
    h.get_by_role_and_label(Role::TextInput, "To").type_text("ann@example.org");
    h.run();
    assert_eq!(compose(&h, cid).to, "ann@example.org");
    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.get_by_label(BODY_LABEL).type_text("Sounds good");
    h.run();
    assert_eq!(compose(&h, cid).body.to_plain_text().trim_end(), "Sounds good");

    h.get_by_label("Pop Out").click();
    h.run();
    assert!(!compose(&h, cid).inline);

    let (app, cid) = app_with_compose(true);
    let mut h = inline_harness(app, cid);
    h.run();
    h.get_by_label("Discard").click();
    h.run();
    assert!(!h.state().s.composes.contains_key(&cid));
}

#[test]
fn bold_shortcut_in_body_runs_format_bold() {
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.get_by_label(BODY_LABEL).type_text("abc");
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::B);
    h.run();
    let c = compose(&h, cid);
    assert!(c.selection_all(|s, _| s.bold == Some(true)), "selection is bold");
}

#[test]
fn tiny_windows_hide_the_ribbon() {
    assert!(is_tiny(300.0, 200.0));
    assert!(!is_tiny(500.0, 400.0));
    assert!(!is_tiny(DEFAULT_SIZE[0], DEFAULT_SIZE[1]));
    assert_eq!(size_text(10), "1 KB");
    assert_eq!(size_text(3 * 1024 * 1024), "3.0 MB");
}

#[test]
fn caret_moves_through_a_quoted_reply() {
    let ids: Vec<u64> =
        mailcraft_engine::sample::sample_session().mailbox.mail.keys().map(|i| i.0).take(4).collect();
    assert!(!ids.is_empty());
    for id in ids {
        let mut s = mailcraft_engine::sample::sample_session();
        s.selection.items = vec![mailcraft_engine::model::ItemId(id)];
        s.selection.focus = Some(mailcraft_engine::model::ItemId(id));
        let r = s.run("mail.reply", &serde_json::json!({"ids": [id], "window": true}));
        let cid = r.ok().and_then(|v| v["compose"].as_u64()).unwrap_or_default();
        let mut h = window_harness(App::new(s));
        h.run();
        // Replies start with the caret in the body.
        assert_eq!(compose(&h, cid).kind, ComposeKind::Reply);
        for key in [
            egui::Key::ArrowDown,
            egui::Key::ArrowDown,
            egui::Key::End,
            egui::Key::PageDown,
            egui::Key::ArrowUp,
            egui::Key::ArrowRight,
            egui::Key::ArrowLeft,
            egui::Key::Home,
        ] {
            h.key_press(key);
            h.run();
            h.key_press_modifiers(egui::Modifiers::SHIFT, key);
            h.run();
        }
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::End);
        h.run();
        let c = compose(&h, cid);
        assert_eq!(c.selection.focus, c.body_end(), "Ctrl+End goes to the end");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::ArrowLeft);
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Home);
        h.run();
        let c = compose(&h, cid);
        assert_eq!(c.selection.focus, c.body_start(), "Ctrl+Home goes to the start");
        assert!(!c.dirty, "moving the caret changes nothing");
    }
}

#[test]
fn file_opens_the_backstage_inside_the_compose_window() {
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();
    h.get_by_role_and_label(Role::TextInput, "To").type_text("pat@example.com");
    h.run();
    h.get_by_label("File").click();
    h.run();
    assert_eq!(h.state().ui.backstage, Some((Surface::Compose(cid), "info".to_owned())));
    // The page is drawn in the window (it used to stay blank when the window was embedded).
    h.get_by_role_and_label(Role::Button, "Encrypt").click();
    h.run();
    assert!(compose(&h, cid).encrypt);
    // Escape goes back to the message.
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(h.state().ui.backstage, None);
    assert!(h.query_by_role_and_label(Role::TextInput, "To").is_some());

    // File › Close with unsaved changes asks in the window, and No closes without saving.
    h.get_by_label("File").click();
    h.run();
    h.get_by_label("Close").click();
    h.run();
    assert_eq!(h.state().ui.backstage, None);
    h.get_by_label(SAVE_PROMPT);
    h.get_by_label("No").click();
    h.run();
    assert!(!h.state().s.composes.contains_key(&cid));
}

// =======================================================================================================
// Compose depth: Auto-Complete, resolved names, Check Names, spelling, tables, links, attachments, signatures
// =======================================================================================================

use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use serde_json::json;

use mailcraft_engine::model::{Contact, DateTime, Doc, ItemId};

/// A mailbox with two contacts (Alice: two addresses, Bob: one), an Auto-Complete entry and a signature, and
/// a new compose.
fn people_app() -> (App, u64) {
    let mut mb = Mailbox::new_local("Local");
    for (first, last, emails) in [
        ("Alice", "Rivera", vec!["alice@example.com", "a.rivera@example.org"]),
        ("Bob", "Stone", vec!["bob@example.net"]),
    ] {
        let id = mb.alloc_id();
        mb.contacts.insert(
            ItemId(id),
            Contact {
                id: ItemId(id),
                first_name: first.into(),
                last_name: last.into(),
                emails: emails.into_iter().map(str::to_owned).collect(),
                ..Default::default()
            },
        );
    }
    let now = DateTime::from_civil(2026, 10, 9, 12, 0, 0, 0).unwrap();
    mb.record_recipients(&[("Alan Ng".into(), "alan@example.net".into())], now);
    mb.add_signature("Formal", Doc::from_plain_text("Regards,\nAnn")).unwrap();
    let mut s = Session::new(mb);
    let cid = s.open_compose(ComposeState { kind: ComposeKind::New, ..ComposeState::default() });
    s.notices.clear();
    (App::new(s), cid)
}

/// Compose windows plus the dialogs area (Check Names, Signatures and Stationery).
fn full_harness(app: App) -> Harness<'static, App> {
    Harness::builder().with_size([1600.0, 1000.0]).build_ui_state(
        |ui, app: &mut App| {
            let ctx = ui.ctx().clone();
            compose_windows(&ctx, app);
            crate::dialogs::dialogs(&ctx, app);
        },
        app,
    )
}

fn push(h: &mut Harness<'_, App>, e: Event) {
    h.input_mut().events.push(e);
}

/// Clicks `button` at `pos` with `modifiers`.
fn click_at(h: &mut Harness<'_, App>, pos: Pos2, button: PointerButton, modifiers: Modifiers) {
    push(h, Event::PointerMoved(pos));
    push(h, Event::ModifiersChanged(modifiers));
    for pressed in [true, false] {
        push(h, Event::PointerButton { pos, button, pressed, modifiers });
    }
    h.run();
    push(h, Event::ModifiersChanged(Modifiers::NONE));
    h.run();
}

/// A point on the first line of the body, `dx` px from the text's left edge.
fn body_point(h: &Harness<'_, App>, dx: f32) -> Pos2 {
    let r = h.get_by_label(BODY_LABEL).rect();
    Pos2::new(r.left() + editor::PAD + dx, r.top() + editor::PAD + 9.0)
}

#[test]
fn auto_complete_suggests_while_typing_and_keys_accept() {
    let (app, cid) = people_app();
    let mut h = window_harness(app);
    h.run();
    h.get_by_role_and_label(Role::TextInput, "To").type_text("al");
    h.run();
    // Auto-Complete list first (recent), then contacts by name.
    h.get_by_label("Alan Ng <alan@example.net>");
    h.get_by_label("Alice Rivera <a.rivera@example.org>");
    h.key_press(Key::ArrowDown);
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(compose(&h, cid).to, "Alice Rivera <a.rivera@example.org>; ");
    assert!(h.query_by_label("Alan Ng <alan@example.net>").is_none(), "the list closed");

    // The next entry: a click picks a row; Tab accepts too.
    h.get_by_role_and_label(Role::TextInput, "To").type_text("bo");
    h.run();
    h.get_by_label("Bob Stone <bob@example.net>").click();
    h.run();
    assert_eq!(compose(&h, cid).to, "Alice Rivera <a.rivera@example.org>; Bob Stone <bob@example.net>; ");
    h.get_by_role_and_label(Role::TextInput, "Cc").click();
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Cc").type_text("ala");
    h.run();
    h.key_press(Key::Tab);
    h.run();
    assert_eq!(compose(&h, cid).cc, "Alan Ng <alan@example.net>; ");

    // Escape closes the list and keeps the text.
    h.get_by_role_and_label(Role::TextInput, "Cc").type_text("ali");
    h.run();
    assert!(h.query_by_label("Alice Rivera <alice@example.com>").is_some());
    h.key_press(Key::Escape);
    h.run();
    assert!(h.query_by_label("Alice Rivera <alice@example.com>").is_none());
    assert!(compose(&h, cid).cc.ends_with("ali"));
}

#[test]
fn resolved_names_show_as_name_pills_without_focus() {
    let (app, cid) = people_app();
    let mut h = window_harness(app);
    h.run();
    h.get_by_role_and_label(Role::TextInput, "To").type_text("Alice Rivera <alice@example.com>; carol");
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Subject").click();
    h.run();
    // The resolved entry shows its display name; the field text is unchanged.
    h.get_by_label("Alice Rivera");
    assert_eq!(compose(&h, cid).to, "Alice Rivera <alice@example.com>; carol");
    let e = recipient_entries(&compose(&h, cid).to);
    assert!(e[0].resolved() && !e[1].resolved());
}

#[test]
fn ctrl_k_checks_names_and_the_dialog_picks_an_address() {
    let (app, cid) = people_app();
    let mut h = full_harness(app);
    h.run();
    h.get_by_role_and_label(Role::TextInput, "To").type_text("Bob; Alice");
    h.run();
    // Close the Auto-Complete list, then Ctrl+K.
    h.key_press(Key::Escape);
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::K);
    h.run();
    // Bob has one address and resolves; Alice has two, so the Check Names dialog asks.
    assert_eq!(compose(&h, cid).to, "Bob Stone <bob@example.net>; Alice");
    assert!(h.state().ui.dialogs.iter().any(|(n, _)| n == "checkNames"));
    h.get_by_label("Alice Rivera <a.rivera@example.org>").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    assert_eq!(compose(&h, cid).to, "Bob Stone <bob@example.net>; Alice Rivera <a.rivera@example.org>");
}

#[test]
fn check_names_keeps_quoted_names_with_semicolons_whole() {
    let (mut app, cid) = people_app();
    app.run("compose.setCc", json!({"compose": cid, "value": "\"Doe; Jo\" <jo@example.com>; Alice"}));
    app.run("compose.checkNames", json!({"compose": cid}));
    let mut h = full_harness(app);
    h.run();
    h.get_by_label("Alice Rivera <alice@example.com>").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    assert_eq!(compose(&h, cid).cc, "\"Doe; Jo\" <jo@example.com>; Alice Rivera <alice@example.com>");
}

/// A new compose whose body is `text`, focused, with spelling checked.
fn typed_body(text: &str) -> (Harness<'static, App>, u64) {
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.get_by_label(BODY_LABEL).type_text(text);
    h.run();
    h.run();
    (h, cid)
}

#[test]
fn misspelled_words_are_flagged_and_the_context_menu_fixes_them() {
    let (mut h, cid) = typed_body("Recieve wrld today");
    let words: Vec<String> = compose(&h, cid).spell_issues.iter().map(|i| i.word.clone()).collect();
    assert_eq!(words, ["Recieve", "wrld"], "checked once typing paused");

    // Right-click on the first word: the speller's suggestions, Ignore All, Add to Dictionary.
    let expected = mailcraft_engine::cmd::review::speller(&h.state().s).suggest("Recieve", 5);
    let first = expected.first().cloned().unwrap();
    let p = body_point(&h, 8.0);
    click_at(&mut h, p, PointerButton::Secondary, Modifiers::NONE);
    h.get_by_label("Ignore All");
    h.get_by_label("Add to Dictionary");
    h.get_by_label(&first).click();
    h.run();
    h.run();
    let c = compose(&h, cid);
    assert!(c.body.to_plain_text().starts_with(&format!("{first} wrld")), "{}", c.body.to_plain_text());
    assert!(c.spell_issues.iter().all(|i| i.word != "Recieve"));
}

#[test]
fn ignore_all_and_add_to_dictionary_from_the_context_menu() {
    let (mut h, cid) = typed_body("Zorblat and zorblat Quxxo");
    assert!(compose(&h, cid).spell_issues.len() >= 3);
    let p = body_point(&h, 8.0);
    click_at(&mut h, p, PointerButton::Secondary, Modifiers::NONE);
    h.get_by_label("Ignore All").click();
    h.run();
    let c = compose(&h, cid);
    assert!(c.ignored_words.iter().any(|w| w.eq_ignore_ascii_case("zorblat")));
    assert!(c.spell_issues.iter().all(|i| !i.word.eq_ignore_ascii_case("zorblat")));

    // Add to Dictionary on the last word (right of the text: the caret goes to the end of the line).
    let p = body_point(&h, 400.0);
    click_at(&mut h, p, PointerButton::Secondary, Modifiers::NONE);
    h.get_by_label("Add to Dictionary").click();
    h.run();
    assert!(h.state().s.mailbox.custom_dictionary.iter().any(|w| w.eq_ignore_ascii_case("Quxxo")));
    let left = compose(&h, cid).spell_issues;
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn f7_steps_through_issues_in_the_spelling_dialog() {
    let (mut h, cid) = typed_body("Thiss is fine");
    let expected = mailcraft_engine::cmd::review::speller(&h.state().s).suggest("Thiss", 5);
    h.key_press(Key::F7);
    h.run();
    assert!(spelling_open(&h.ctx, cid));
    h.get_by_label("Not in dictionary:");
    assert!(!compose(&h, cid).selection.is_collapsed(), "the issue is selected in the body");
    h.get_by_label("Change").click();
    h.run();
    let first = expected.first().cloned().unwrap();
    assert!(compose(&h, cid).body.to_plain_text().starts_with(&first));
    h.get_by_label(SPELLING_COMPLETE);
    h.get_by_label("OK").click();
    h.run();
    assert!(!spelling_open(&h.ctx, cid));

    // Ignore Once from the ribbon's Spelling and Grammar (the window intercepts review.spelling).
    let (mut h, cid) = typed_body("Wrongg words");
    let ctx = h.ctx.clone();
    assert!(ribbon_command(&ctx, h.state_mut(), cid, "review.spelling", &json!({})));
    h.run();
    h.get_by_label("Ignore Once").click();
    h.run();
    h.get_by_label(SPELLING_COMPLETE);
    assert_eq!(compose(&h, cid).body.to_plain_text().trim_end(), "Wrongg words");
}

#[test]
fn tab_moves_between_table_cells() {
    let (mut app, cid) = app_with_compose(false);
    app.s.run("insert.table", &json!({"compose": cid, "rows": 2, "cols": 2})).unwrap();
    let ti = app.s.composes[&cid]
        .body
        .blocks
        .iter()
        .position(|b| matches!(b, mailcraft_engine::model::Block::Table { .. }))
        .unwrap();
    app.s
        .run("compose.select", &json!({"compose": cid, "anchor": {"path": [ti, 0, 0, 0], "offset": 0}}))
        .unwrap();
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).focus();
    h.run();
    h.key_press(Key::Tab);
    h.run();
    assert_eq!(compose(&h, cid).cell_at_caret().map(|(_, r, c)| (r, c)), Some((0, 1)));
    h.get_by_label(BODY_LABEL).type_text("x");
    h.run();
    h.key_press_modifiers(Modifiers::SHIFT, Key::Tab);
    h.run();
    assert_eq!(compose(&h, cid).cell_at_caret().map(|(_, r, c)| (r, c)), Some((0, 0)));
    // Tab in the last cell adds a row.
    for _ in 0..4 {
        h.key_press(Key::Tab);
        h.run();
    }
    assert_eq!(compose(&h, cid).cell_at_caret().map(|(_, r, c)| (r, c)), Some((2, 0)));
}

#[test]
fn ctrl_click_follows_a_bookmark_link() {
    use mailcraft_engine::compose::{Bookmark, DocPos};
    use mailcraft_engine::model::{Block, Inline};
    let link = Block::Paragraph {
        style: Default::default(),
        inlines: vec![Inline::Link {
            href: "#end".into(),
            children: vec![Inline::Text { text: "Jump to the end".into(), style: Default::default() }],
        }],
    };
    let mut body = Doc::from_plain_text("\nmiddle\nlast line");
    body.blocks.insert(0, link);
    let mut s = Session::new(Mailbox::new_local("Local"));
    let target = DocPos::new(vec![3], 4);
    let cid = s.open_compose(ComposeState {
        kind: ComposeKind::Reply,
        body,
        bookmarks: vec![Bookmark { name: "end".into(), pos: target.clone() }],
        ..ComposeState::default()
    });
    s.notices.clear();
    let mut h = window_harness(App::new(s));
    h.run();
    // A plain click only places the caret.
    let p = body_point(&h, 20.0);
    click_at(&mut h, p, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(compose(&h, cid).selection.focus.path, vec![0]);
    click_at(&mut h, p, PointerButton::Primary, Modifiers::COMMAND);
    assert_eq!(compose(&h, cid).selection.focus, target);
}

#[test]
fn attachment_chips_open_a_menu_that_removes() {
    let (mut app, cid) = app_with_compose(false);
    let c = app.s.composes.get_mut(&cid).unwrap();
    c.attach_bytes("report.txt", None, b"hello".to_vec()).unwrap();
    c.attach_bytes("photo.png", None, vec![0; 3000]).unwrap();
    let mut h = window_harness(app);
    h.run();
    h.get_by_label("report.txt");
    h.get_by_label("3 KB");
    h.get_by_label("Options for report.txt").click();
    h.run();
    h.get_by_label("Open");
    h.get_by_label("Save As...");
    h.get_by_label("Remove Attachment").click();
    h.run();
    let names: Vec<String> = compose(&h, cid).attachments.iter().map(|a| a.filename.clone()).collect();
    assert_eq!(names, ["photo.png"]);
}

/// Scratch folder for file tests: under the workspace's `target/tmp`, never the (RAM-backed) system temp.
fn test_dir() -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/tmp")
        .join(format!("mailcraft-compose-ui-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn browse_and_save_as_use_the_platform_hooks() {
    let dir = test_dir();
    std::fs::write(dir.join("picked.txt"), b"picked bytes").unwrap();
    set_file_hooks(FileHooks {
        pick_files: Some(|| Some(vec![test_dir().join("picked.txt")])),
        pick_save: Some(|name| Some(test_dir().join(format!("saved-{name}")))),
        open_path: None,
    });
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();
    let ctx = h.ctx.clone();
    // Browse This PC… (Attach File ▾) runs the picker and attaches every file picked.
    assert!(ribbon_command(&ctx, h.state_mut(), cid, "compose.attachFile", &json!({"compose": cid})));
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.attachments.first().map(|a| a.data.clone()), Some(b"picked bytes".to_vec()));
    let picked = dir.join("picked.txt").to_string_lossy().into_owned();
    assert!(recent_files().contains(&picked));
    // A recent file attaches by path.
    assert!(ribbon_command(&ctx, h.state_mut(), cid, "compose.attachFile", &json!({"path": picked})));
    assert_eq!(compose(&h, cid).attachments.len(), 2);

    h.get_by_label("Options for picked.txt").click();
    h.run();
    h.get_by_label("Save As...").click();
    h.run();
    assert_eq!(std::fs::read(dir.join("saved-picked.txt")).unwrap(), b"picked bytes");
    set_file_hooks(FileHooks::default());
}

#[derive(Debug)]
struct TestDrop(std::path::PathBuf, Option<Vec<u8>>);

impl egui::DroppedFile for TestDrop {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        match &self.1 {
            Some(b) => Ok(b.clone()),
            None => std::fs::read(&self.0).map_err(|e| e.to_string()),
        }
    }
}

#[test]
fn files_dropped_on_the_window_become_attachments() {
    let dir = test_dir();
    let path = dir.join("dropped notes.txt");
    std::fs::write(&path, b"dropped").unwrap();
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).hover();
    h.run();
    h.input_mut().dropped_files.push(std::sync::Arc::new(TestDrop(path.clone(), None)));
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.attachments.len(), 1);
    assert_eq!(c.attachments[0].filename, "dropped notes.txt");
    assert!(recent_files().contains(&path.to_string_lossy().into_owned()));

    // A drop without a full path (a name only) attaches the bytes the drop carries.
    h.input_mut().dropped_files.push(std::sync::Arc::new(TestDrop(
        std::path::PathBuf::from("web-name.txt"),
        Some(b"web bytes".to_vec()),
    )));
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.attachments.len(), 2);
    assert_eq!(c.attachments[1].filename, "web-name.txt");
    assert_eq!(c.attachments[1].data, b"web bytes");
}

#[test]
fn signature_menu_lists_saved_signatures() {
    let (app, cid) = people_app();
    let mut h = full_harness(app);
    h.run();
    // Insert › Include › Signature ▾ (the Message tab overflows at the test window's width).
    h.get_by_label("Insert").click();
    h.run();
    h.get_by_label("Signature").click();
    h.run();
    h.get_by_label("Formal").click();
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.signature.as_ref().map(|s| s.name.as_str()), Some("Formal"));
    assert!(c.body.to_plain_text().contains("Regards,"));

    h.get_by_label("Signature").click();
    h.run();
    h.get_by_label("Signatures...").click();
    h.run();
    assert!(h.state().ui.dialogs.iter().any(|(n, _)| n == "signatures"));
    h.get_by_label("Choose default signature");
}

#[test]
fn signatures_dialog_creates_edits_renames_and_deletes() {
    let (mut app, cid) = people_app();
    app.s.composes.remove(&cid);
    crate::dialogs::open(&mut app, "signatures", json!({}));
    let mut h = full_harness(app);
    h.run();
    // The first signature is loaded into the editor.
    h.get_by_label("Formal");
    h.get_by_label("New").click();
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Type a name for this signature:").type_text("Casual");
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert!(h.state().s.mailbox.signature("Casual").is_some());

    // Type the body in the rich editor (the compose body widget) and save.
    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.get_by_label(BODY_LABEL).type_text("Cheers");
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run();
    h.get_by_label("Bold").click();
    h.run();
    h.get_by_label("Save").click();
    h.run();
    let sig = h.state().s.mailbox.signature("Casual").cloned().unwrap();
    assert_eq!(sig.body.to_plain_text().trim(), "Cheers");
    assert!(format!("{:?}", sig.body).contains("bold: Some(true)"), "{:?}", sig.body);

    // Rename.
    h.get_by_label("Rename").click();
    h.run();
    let field = h.get_by_role_and_label(Role::TextInput, "Type a name for this signature:");
    field.focus();
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.get_by_role_and_label(Role::TextInput, "Type a name for this signature:").type_text("Relaxed");
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert!(h.state().s.mailbox.signature("Casual").is_none());
    assert!(h.state().s.mailbox.signature("Relaxed").is_some());

    // Delete the selected one.
    h.get_by_label("Delete").click();
    h.run();
    assert!(h.state().s.mailbox.signature("Relaxed").is_none());
    assert!(h.state().s.mailbox.signature("Formal").is_some());
}

#[test]
fn shift_selection_spans_paragraphs_and_the_ribbon_follows() {
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.get_by_label(BODY_LABEL).type_text("one");
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    h.get_by_label(BODY_LABEL).type_text("two");
    h.run();
    assert_eq!(compose(&h, cid).body.blocks.len(), 2, "Enter splits the paragraph");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Home);
    h.run();
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowDown);
    h.run();
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::End);
    h.run();
    let c = compose(&h, cid);
    assert_ne!(c.selection.anchor.path, c.selection.focus.path, "selection crosses the paragraph break");
    // The ribbon's Italic toggle reads `selection_all` (ribbon::state tests cover the toggle itself).
    assert!(!c.selection_all(|s, _| s.italic == Some(true)));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::I);
    h.run();
    let c = compose(&h, cid);
    assert!(c.selection_all(|s, _| s.italic == Some(true)), "both paragraphs are italic");
    h.key_press(egui::Key::ArrowRight);
    h.run();
    h.get_by_label(BODY_LABEL).type_text("!");
    h.run();
    assert!(compose(&h, cid).body.to_plain_text().contains("two!"));
}

#[test]
fn ctrl_backspace_at_a_block_start_acts_like_backspace() {
    // Start of the second list item: Ctrl+Backspace removes the bullet (like Backspace) instead of merging the
    // item into the one above.
    let (mut app, cid) = app_with_compose(false);
    app.s.run("compose.insertText", &json!({"compose": cid, "text": "one\ntwo"})).unwrap();
    app.s.run("compose.select", &json!({"compose": cid, "all": true})).unwrap();
    app.s.run("format.bullets", &json!({"compose": cid})).unwrap();
    let second = app.s.composes[&cid].leaf_paths()[1].clone();
    app.s.run("compose.select", &json!({"compose": cid, "anchor": {"path": second, "offset": 0}})).unwrap();
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).focus();
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::Backspace);
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.body.to_plain_text(), "\u{2022} one\ntwo", "{:?}", c.body.blocks);

    // Start of a table cell: Ctrl+Backspace stays in the table and moves nothing out of the cell.
    let (mut app, cid) = app_with_compose(false);
    app.s.run("insert.table", &json!({"compose": cid, "rows": 1, "cols": 2})).unwrap();
    let ti = app.s.composes[&cid]
        .body
        .blocks
        .iter()
        .position(|b| matches!(b, mailcraft_engine::model::Block::Table { .. }))
        .unwrap();
    for (col, text) in [(0, "abc"), (1, "def")] {
        app.s
            .run("compose.select", &json!({"compose": cid, "anchor": {"path": [ti, 0, col, 0], "offset": 0}}))
            .unwrap();
        app.s.run("compose.insertText", &json!({"compose": cid, "text": text})).unwrap();
    }
    app.s
        .run("compose.select", &json!({"compose": cid, "anchor": {"path": [ti, 0, 1, 0], "offset": 0}}))
        .unwrap();
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).focus();
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::Backspace);
    h.run();
    let c = compose(&h, cid);
    let cell =
        |col: usize| super::editor::leaf_chars(&c.body, &[ti, 0, col, 0]).into_iter().collect::<String>();
    assert_eq!((cell(0), cell(1)), ("abc".to_owned(), "def".to_owned()));
    assert_eq!(
        c.cell_at_caret().map(|(_, r, col)| (r, col)),
        Some((0, 0)),
        "the caret moves into the cell before"
    );
}
