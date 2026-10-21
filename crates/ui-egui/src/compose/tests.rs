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
    use crate::responsive::inspector_tiny;
    assert!(inspector_tiny(300.0, 200.0));
    assert!(!inspector_tiny(500.0, 400.0));
    assert!(!inspector_tiny(DEFAULT_SIZE[0], DEFAULT_SIZE[1]));
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
    // The Encrypt tile toggles S/MIME encryption of the message.
    assert!(compose(&h, cid).encrypt);
    h.get_by_role_and_label(Role::Button, "Encrypt").click();
    h.run();
    assert!(!compose(&h, cid).encrypt);
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

fn clipboard_html_fixture() -> Option<String> {
    Some("<p>Rich <b>bold</b> paste</p>".into())
}

#[test]
fn ctrl_v_pastes_the_html_flavour_and_offers_paste_options() {
    let (mut h, cid) = typed_body("x");
    set_clipboard_hooks(ClipboardHooks {
        read_html: Some(clipboard_html_fixture),
        ..ClipboardHooks::default()
    });
    push(&mut h, Event::Paste("Rich bold paste".into()));
    h.run();
    h.run();
    let c = compose(&h, cid);
    assert!(c.body.to_plain_text().contains("Rich bold paste"), "{}", c.body.to_plain_text());
    let bold = c.body.blocks.iter().any(|b| match b {
        mailcraft_model::Block::Paragraph { inlines, .. } => inlines.iter().any(|i| {
            matches!(i, mailcraft_model::Inline::Text { text, style } if text.contains("bold") && style.bold == Some(true))
        }),
        _ => false,
    });
    assert!(bold, "the HTML formatting came along: {:?}", c.body);
    // Paste Options › Keep Text Only redoes the paste without formatting.
    h.get_by_label("(Ctrl) Paste Options").click();
    h.run();
    h.get_by_label("Keep Text Only").click();
    h.run();
    h.run();
    let c = compose(&h, cid);
    assert!(c.body.to_plain_text().contains("Rich bold paste"), "{}", c.body.to_plain_text());
    let still_bold = c.body.blocks.iter().any(|b| match b {
        mailcraft_model::Block::Paragraph { inlines, .. } => inlines
            .iter()
            .any(|i| matches!(i, mailcraft_model::Inline::Text { style, .. } if style.bold == Some(true))),
        _ => false,
    });
    assert!(!still_bold);
    set_clipboard_hooks(ClipboardHooks::default());
    // An HTML flavour that does not match the pasted text is ignored.
    h.get_by_label(BODY_LABEL).click();
    h.run();
    set_clipboard_hooks(ClipboardHooks {
        read_html: Some(clipboard_html_fixture),
        ..ClipboardHooks::default()
    });
    push(&mut h, Event::Paste("other words".into()));
    h.run();
    h.run();
    set_clipboard_hooks(ClipboardHooks::default());
    assert!(compose(&h, cid).body.to_plain_text().contains("other words"));
}

fn voting_app() -> (App, ItemId) {
    let mut mb = Mailbox::new_local("Local");
    let inbox = mb
        .folders
        .values()
        .find(|f| f.role == Some(mailcraft_engine::model::FolderRole::Inbox))
        .map(|f| f.id)
        .unwrap();
    let raw = b"From: Ann <ann@example.org>\r\nTo: me@example.com\r\nSubject: Lunch\r\nMessage-ID: <v1@example.org>\r\nX-MailCraft-Voting: Approve;Reject\r\nDisposition-Notification-To: ann@example.org\r\n\r\nWhere?\r\n".to_vec();
    let msg = mailcraft_mime::parse(&raw).unwrap();
    let mut item = mailcraft_engine::model::MailItem::from_message(
        &msg,
        &raw,
        DateTime::from_civil(2026, 10, 9, 12, 0, 0, 0).unwrap(),
    );
    item.folder = inbox;
    let id = mb.add_mail(item);
    (App::new(Session::new(mb)), id)
}

#[test]
fn reading_pane_tracking_bars_offer_votes_and_the_read_receipt() {
    let (app, id) = voting_app();
    let mut h = Harness::builder()
        .with_size([900.0, 300.0])
        .build_ui_state(move |ui, app: &mut App| tracking_bars(ui, app, id, ReceiptPolicy::Ask), app);
    h.run();
    h.get_by_label("This message has voting buttons. Choose a response.");
    h.get_by_label(READ_RECEIPT_REQUEST);
    h.get_by_label("Approve");
    h.get_by_label("Reject");
    // Don't Send hides the request without sending anything.
    h.get_by_label("Don't Send").click();
    h.run();
    assert!(h.query_by_label(READ_RECEIPT_REQUEST).is_none());
    // Voting: Edit the response before sending opens a compose with the vote.
    h.get_by_label("Reject").click();
    h.run();
    h.get_by_label("Edit the response before sending").click();
    h.run();
    let c = h.state().s.composes.values().next().cloned().unwrap();
    assert_eq!(c.vote_response.as_deref(), Some("Reject"));
    assert_eq!(c.subject, "Reject: Lunch");
}

#[test]
fn paste_defaults_follow_the_mail_options() {
    use super::editor::EditorHost;
    let (mut app, _) = app_with_compose(false);
    assert_eq!(EditorHost::paste_mode(&app), "keepSource");
    assert!(EditorHost::paste_options(&app));
    app.run("options.set", json!({"key": "mail.editor.pasteFromOtherPrograms", "value": "textOnly"}));
    app.run("options.set", json!({"key": "mail.other.pasteOptions", "value": false}));
    assert_eq!(EditorHost::paste_mode(&app), "textOnly");
    assert!(!EditorHost::paste_options(&app));
}

#[test]
fn ribbon_paste_modes_paste_the_system_clipboard() {
    let (mut app, cid) = app_with_compose(false);
    let ctx = egui::Context::default();
    // Paste ▾ entries with a mode ask the platform for a paste; the body editor takes it with that mode.
    for mode in ["keepSource", "merge", "textOnly"] {
        assert!(ribbon_command(&ctx, &mut app, cid, "format.paste", &json!({"mode": mode})));
    }
    // Explicit content or no mode runs the engine command as before.
    assert!(!ribbon_command(&ctx, &mut app, cid, "format.paste", &json!({"mode": "merge", "text": "x"})));
    assert!(!ribbon_command(&ctx, &mut app, cid, "format.paste", &json!({})));
}

/// A 1×1 PNG, base64.
const PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

fn select_range(app: &mut App, cid: u64, a: usize, b: usize) {
    app.run(
        "compose.select",
        json!({"compose": cid, "anchor": {"path": [0], "offset": a}, "focus": {"path": [0], "offset": b}}),
    )
    .unwrap();
}

#[test]
fn compose_panes_paste_apply_styles_and_hide_pictures() {
    let (mut app, cid) = app_with_compose(false);
    app.run("compose.insertText", json!({"compose": cid, "text": "alpha beta"})).unwrap();
    select_range(&mut app, cid, 0, 5);
    app.run("format.copy", json!({"compose": cid})).unwrap();
    select_range(&mut app, cid, 10, 10);
    app.run("format.paste", json!({"compose": cid, "image": PNG_B64})).unwrap();
    for c in ["format.clipboardPane", "format.stylesPane", "format.selectionPane"] {
        app.run(c, json!({"compose": cid})).unwrap();
    }
    select_range(&mut app, cid, 0, 0);
    let mut h = window_harness(app);
    h.run();
    for t in [CLIPBOARD_TITLE, STYLES_TITLE, SELECTION_TITLE] {
        h.get_by_label(&format!("Close {t} pane"));
    }
    // Clipboard: clicking an item pastes it at the caret.
    h.get_by_label("alpha").click();
    h.run();
    assert!(
        compose(&h, cid).body.to_plain_text().starts_with("alphaalpha beta"),
        "{}",
        compose(&h, cid).body.to_plain_text()
    );
    // Styles: clicking a style applies it.
    h.get_by_label("Heading 1").click();
    h.run();
    assert!(matches!(compose(&h, cid).body.blocks[0], mailcraft_model::Block::Heading { level: 1, .. }));
    // Selection: hide / show the picture (view only), select it by name.
    h.get_by_label("Hide Picture").click();
    h.run();
    let src = format!("cid:{}", compose(&h, cid).inline_images[0].content_id);
    assert_eq!(hidden_objects(&h.ctx, cid), vec![src]);
    h.get_by_label("Show All").click();
    h.run();
    assert!(hidden_objects(&h.ctx, cid).is_empty());
    h.get_by_label("Hide All").click();
    h.run();
    assert_eq!(hidden_objects(&h.ctx, cid).len(), 1);
    h.get_by_label("Picture").click();
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.selection.end().offset, c.selection.start().offset + 1, "the picture is selected");
    // Closing a pane turns it off.
    h.get_by_label("Close Styles pane").click();
    h.run();
    assert!(!compose(&h, cid).editor.styles_pane);
    h.get_by_label("Close Clipboard pane").click();
    h.run();
    assert!(!compose(&h, cid).editor.clipboard_pane);
    h.get_by_label("Close Selection pane").click();
    h.run();
    assert!(!compose(&h, cid).editor.selection_pane);
}

#[test]
fn styles_pane_creates_a_style_from_the_selection() {
    let (mut app, cid) = app_with_compose(false);
    app.run("compose.insertText", json!({"compose": cid, "text": "plain words"})).unwrap();
    select_range(&mut app, cid, 0, 5);
    app.run("format.bold", json!({"compose": cid})).unwrap();
    app.run("format.stylesPane", json!({"compose": cid})).unwrap();
    let mut h = window_harness(app);
    h.run();
    h.get_by_label("New Style...").click();
    h.run();
    h.get_by_label(panes::NEW_STYLE_NAME).click();
    h.run();
    h.get_by_label(panes::NEW_STYLE_NAME).type_text("Loud");
    h.run();
    h.get_by_label("Create").click();
    h.run();
    let c = compose(&h, cid);
    assert!(
        c.custom_styles.iter().any(|x| x.name == "Loud" && x.style.bold == Some(true)),
        "{:?}",
        c.custom_styles
    );
    h.get_by_label("Loud");
}

#[test]
fn panes_leave_a_usable_body_in_narrow_windows() {
    let c = ComposeState {
        editor: mailcraft_engine::compose::EditorFlags {
            clipboard_pane: true,
            styles_pane: true,
            ..Default::default()
        },
        ..ComposeState::default()
    };
    assert_eq!(panes::widths(&c, 1200.0), (PANE_W, PANE_W));
    assert_eq!(panes::widths(&c, 600.0), (0.0, PANE_W), "the right column stays when one fits");
    assert_eq!(panes::widths(&c, 400.0), (0.0, 0.0));
    assert_eq!(panes::widths(&c, f32::NAN), (0.0, 0.0));
    assert_eq!(panes::widths(&ComposeState::default(), 1200.0), (0.0, 0.0));
    assert_eq!(clip_label("a\n  b", 0), "a b");
    assert_eq!(clip_label("", 2), "[2 pictures]");
    assert_eq!(clip_label("x", 1), "x  [1 picture]");
    assert_eq!(clip_label("", 0), "(empty)");
    assert!(clip_label(&"w ".repeat(200), 0).ends_with('\u{2026}'));
}

fn png_reader() -> Option<Vec<u8>> {
    Some(mailcraft_mime::transfer::decode_base64(PNG_B64.as_bytes()))
}

#[test]
fn ctrl_v_with_only_a_picture_pastes_the_picture() {
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).click();
    h.run();
    set_clipboard_image_reader(Some(png_reader));
    h.key_press_modifiers(Modifiers::COMMAND, Key::V);
    h.run();
    set_clipboard_image_reader(None);
    let c = compose(&h, cid);
    assert_eq!(c.inline_images.len(), 1, "{:?}", h.state().ui.toast);
    assert_eq!(c.inline_images[0].mime_type, "image/png");
    // Without a picture reader Ctrl+V does nothing by itself (text arrives as a paste event).
    h.key_press_modifiers(Modifiers::COMMAND, Key::V);
    h.run();
    assert_eq!(compose(&h, cid).inline_images.len(), 1);
}

#[test]
fn show_marks_gridlines_and_select_objects_draw_without_trouble() {
    let (mut app, cid) = app_with_compose(false);
    app.run("compose.insertText", json!({"compose": cid, "text": "a b\tc"})).unwrap();
    app.run("insert.table", json!({"compose": cid, "rows": 2, "cols": 2})).unwrap();
    app.run("format.borders", json!({"compose": cid, "kind": "none"})).unwrap();
    for c in ["format.showMarks", "format.showGridlines", "format.selectObjects"] {
        app.run(c, json!({"compose": cid})).unwrap();
    }
    let mut h = window_harness(app);
    h.run();
    h.run();
    let c = compose(&h, cid);
    assert!(c.editor.show_marks && c.editor.show_gridlines && c.editor.select_objects);
    // Select Objects: a click on text does not move the caret.
    let before = c.selection.clone();
    h.get_by_label(BODY_LABEL).click();
    h.run();
    assert_eq!(compose(&h, cid).selection, before);
}

#[test]
fn insert_3d_model_and_gallery_filter_open_their_windows() {
    let (mut app, cid) = app_with_compose(false);
    app.run("insert.model3d", json!({"compose": cid})).unwrap();
    crate::dialogs::take_notices(&mut app);
    let mut h = full_harness(app);
    h.run();
    h.get_by_label("Insert 3D Model");
    h.get_by_label("OBJ and STL models are inserted as a picture of the model.");
    h.get_by_label("Cancel").click();
    h.run();
    assert!(h.state().ui.dialogs.is_empty());
    // Multilevel List › Gallery Filter › List Library: the library as a picker; a pick applies it.
    let app = h.state_mut();
    app.run("compose.insertText", json!({"compose": cid, "text": "item"})).unwrap();
    app.run("format.multilevel", json!({"compose": cid, "filter": "library"})).unwrap();
    crate::dialogs::take_notices(app);
    h.run();
    h.get_by_label("Multilevel List: List Library");
    h.get_by_label("1. 1.1. 1.1.1.").click();
    h.run();
    assert!(compose(&h, cid).list_at_caret().is_some());
    assert!(h.state().ui.dialogs.is_empty());
}

#[test]
fn auto_save_timer_ticks_while_a_compose_has_changes() {
    let (mut app, cid) = app_with_compose(false);
    let now = app.s.now();
    app.s.fixed_now = Some(now);
    app.run("options.set", json!({"key": "mail.save.autoSave", "value": true}));
    app.run("options.set", json!({"key": "mail.save.autoSaveMinutes", "value": 1}));
    let ctx = egui::Context::default();
    // Nothing unsaved: no tick.
    assert!(!auto_save_timer(&ctx, &mut app, 0.0));
    app.run("compose.insertText", json!({"compose": cid, "text": "Draft text"}));
    assert!(app.s.composes[&cid].dirty);
    // The period starts when the changes are first seen; the tick runs once it has passed.
    assert!(!auto_save_timer(&ctx, &mut app, 1.0));
    assert!(!auto_save_timer(&ctx, &mut app, 10.0));
    assert!(auto_save_timer(&ctx, &mut app, 1.0 + AUTO_SAVE_CHECK_SECS));
    // The engine waits for the auto-save minutes: nothing saved yet.
    assert!(app.s.composes[&cid].dirty && app.s.composes[&cid].draft.is_none());
    app.s.fixed_now = Some(DateTime { unix: now.unix + 61, offset_min: now.offset_min });
    assert!(!auto_save_timer(&ctx, &mut app, 20.0), "one tick per period");
    assert!(auto_save_timer(&ctx, &mut app, 1.0 + 2.0 * AUTO_SAVE_CHECK_SECS));
    let c = &app.s.composes[&cid];
    assert!(!c.dirty && c.draft.is_some(), "saved without anyone typing");
    // Saved: the timer stops until the next change.
    assert!(!auto_save_timer(&ctx, &mut app, 100.0));
    // A clock that went backwards restarts the period instead of ticking.
    app.run("compose.insertText", json!({"compose": cid, "text": "!"}));
    assert!(!auto_save_timer(&ctx, &mut app, 200.0));
    assert!(!auto_save_timer(&ctx, &mut app, 5.0));
    // The window draws the timer every frame.
    let mut h = window_harness(app);
    h.run();
    assert!(h.ctx.data(|d| d.get_temp::<f64>(egui::Id::new("mailcraft-compose-autosave"))).is_some());
}

#[test]
fn paste_defaults_follow_where_the_text_was_copied() {
    let (mut h, cid) = typed_body("Rich bold paste");
    // Nothing copied in a message yet: from another program.
    assert_eq!(super::editor::paste_origin(&h.ctx, cid, "Rich bold paste"), "otherProgram");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run();
    push(&mut h, Event::Copy);
    h.run();
    assert_eq!(super::editor::paste_origin(&h.ctx, cid, "Rich bold paste"), "sameMessage");
    assert_eq!(super::editor::paste_origin(&h.ctx, cid, "Rich bold paste\r\n"), "otherProgram");
    assert_eq!(super::editor::paste_origin(&h.ctx, cid + 1, "Rich bold paste"), "otherMessage");
    // "Pasting within the same message: Keep Text Only" drops the clipboard's formatting.
    h.state_mut().run("options.set", json!({"key": "mail.editor.pasteWithinMessage", "value": "textOnly"}));
    h.key_press(egui::Key::End);
    h.run();
    set_clipboard_hooks(ClipboardHooks {
        read_html: Some(clipboard_html_fixture),
        ..ClipboardHooks::default()
    });
    push(&mut h, Event::Paste("Rich bold paste".into()));
    h.run();
    h.run();
    set_clipboard_hooks(ClipboardHooks::default());
    let c = compose(&h, cid);
    assert_eq!(c.body.to_plain_text().matches("Rich bold paste").count(), 2, "{}", c.body.to_plain_text());
    let bold = c.body.blocks.iter().any(|b| match b {
        mailcraft_model::Block::Paragraph { inlines, .. } => inlines
            .iter()
            .any(|i| matches!(i, mailcraft_model::Inline::Text { style, .. } if style.bold == Some(true))),
        _ => false,
    });
    assert!(!bold, "kept text only: {:?}", c.body);
}

#[test]
fn f6_cycles_header_fields_body_and_ribbon_in_a_compose_window() {
    let (app, cid) = app_with_compose(false);
    let mut h = Harness::builder().with_size([1600.0, 1000.0]).build_ui_state(
        |ui, app: &mut App| {
            crate::keys::handle(ui.ctx(), app, crate::Surface::Main);
            compose_windows(ui.ctx(), app);
        },
        app,
    );
    h.run_steps(3);
    let focused = |h: &Harness<'_, App>| h.ctx.memory(|m| m.focused());
    // A new message starts in To.
    assert_eq!(focused(&h), Some(field_id(cid, Field::To)));
    let surface = crate::Surface::Compose(cid);
    let pane = |h: &Harness<'_, App>| crate::keys::window_pane(&h.ctx, surface);
    h.key_press(egui::Key::F6);
    h.run_steps(2);
    assert_eq!((pane(&h).as_deref(), focused(&h)), (Some("body"), Some(editor_id(cid))));
    h.key_press(egui::Key::F6);
    h.run_steps(2);
    assert_eq!(pane(&h).as_deref(), Some("ribbon"));
    assert!(crate::keys::widget_focused(&h.ctx), "the selected ribbon tab has focus");
    h.key_press(egui::Key::F6);
    h.run_steps(2);
    assert_eq!((pane(&h).as_deref(), focused(&h)), (Some("header"), Some(field_id(cid, Field::To))));
    // Shift+F6 goes back; typing in To still works afterwards.
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::F6);
    h.run_steps(2);
    assert_eq!(pane(&h).as_deref(), Some("ribbon"));
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::F6);
    h.run_steps(2);
    assert_eq!((pane(&h).as_deref(), focused(&h)), (Some("body"), Some(editor_id(cid))));
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::F6);
    h.run_steps(2);
    assert_eq!(focused(&h), Some(field_id(cid, Field::To)));
    h.get_by_role_and_label(Role::TextInput, "To").type_text("ann@example.com");
    h.run();
    assert_eq!(compose(&h, cid).to, "ann@example.com");
    // A click in the window ends the F6 ring.
    h.get_by_label(BODY_LABEL).click();
    h.run_steps(2);
    assert_eq!(pane(&h), None);
}

// ---------------------------------------------------------------------------------------------------------
// Compose and proofing options: proofing as you type, Mini Toolbar, @mentions, accessibility warnings,
// Live Preview.

fn set_option(h: &mut Harness<'_, App>, key: &str, value: serde_json::Value) {
    assert!(h.state_mut().run("options.set", json!({"key": key, "value": value})).is_some(), "{key}");
}

#[test]
fn proofing_as_you_type_follows_the_editor_options() {
    let (mut h, cid) = typed_body("Recieve QZXV the the end");
    let flagged = |h: &Harness<'_, App>| -> Vec<String> {
        compose(h, cid).spell_issues.iter().map(|i| i.word.trim().to_owned()).collect()
    };
    assert_eq!(flagged(&h), ["Recieve", "the"], "a spelling and a repeated word");
    // An option change re-checks the body without an edit.
    set_option(&mut h, "mail.editor.ignoreUppercase", json!(false));
    h.run();
    assert_eq!(flagged(&h), ["Recieve", "QZXV", "the"]);
    set_option(&mut h, "mail.editor.flagRepeated", json!(false));
    h.run();
    assert_eq!(flagged(&h), ["Recieve", "QZXV"]);
    // Check spelling as you type off: no spelling marks and no spelling suggestions on right-click.
    set_option(&mut h, "mail.editor.spellAsYouType", json!(false));
    h.run();
    let p = body_point(&h, 8.0);
    click_at(&mut h, p, PointerButton::Secondary, Modifiers::NONE);
    assert!(h.query_by_label("Ignore All").is_none(), "no spelling menu for unmarked words");
    h.key_press(Key::Escape);
    h.run();
    // Neither marked: typing is not checked at all, F7 still is.
    set_option(&mut h, "mail.editor.grammarAsYouType", json!(false));
    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::End);
    h.run();
    h.get_by_label(BODY_LABEL).type_text(" wrold");
    h.run();
    h.run();
    assert!(compose(&h, cid).body.to_plain_text().contains("wrold"));
    assert!(!flagged(&h).contains(&"wrold".to_owned()));
    let r = h.state_mut().run("review.spelling", json!({"compose": cid})).unwrap();
    assert!(r["issues"].as_array().unwrap().iter().any(|i| i["word"] == "wrold"), "{r}");
}

/// An inline response whose body is `text`, focused.
fn inline_body(text: &str) -> (Harness<'static, App>, u64) {
    let (app, cid) = app_with_compose(true);
    let mut h = inline_harness(app, cid);
    h.run();
    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.get_by_label(BODY_LABEL).type_text(text);
    h.run();
    (h, cid)
}

/// Drags the mouse across the first line of the body from `dx0` to `dx1`.
fn drag_select(h: &mut Harness<'_, App>, dx0: f32, dx1: f32) {
    let (a, b) = (body_point(h, dx0), body_point(h, dx1));
    push(h, Event::PointerMoved(a));
    push(
        h,
        Event::PointerButton {
            pos: a,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        },
    );
    h.step();
    // Steps, not runs: the editor repaints while a drag selects.
    for i in 1..=4 {
        push(h, Event::PointerMoved(a + (b - a) * (i as f32 / 4.0)));
        h.step();
    }
    push(
        h,
        Event::PointerButton {
            pos: b,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        },
    );
    h.run();
    h.run();
}

#[test]
fn mini_toolbar_appears_over_a_mouse_selection() {
    let (mut h, cid) = inline_body("Make this bold please");
    drag_select(&mut h, 0.0, 60.0);
    assert!(!compose(&h, cid).selection.is_collapsed());
    assert!(mini_toolbar_showing(&h.ctx, cid));
    h.get_by_label(MINI_TOOLBAR_LABEL);
    h.get_by_label("Bold").click();
    h.run();
    let c = compose(&h, cid);
    assert!(c.selection_all(|st, _| st.bold == Some(true)), "the selection is bold");
    // Typing hides it.
    h.get_by_label(BODY_LABEL).type_text("X");
    h.run();
    assert!(!mini_toolbar_showing(&h.ctx, cid));
    // Turned off: a mouse selection shows nothing.
    set_option(&mut h, "general.miniToolbar", json!(false));
    drag_select(&mut h, 0.0, 60.0);
    assert!(!compose(&h, cid).selection.is_collapsed());
    assert!(!mini_toolbar_showing(&h.ctx, cid));
    assert!(h.query_by_label(MINI_TOOLBAR_LABEL).is_none());
}

#[test]
fn at_mentions_suggest_people_and_add_them_to_to() {
    let (app, cid) = people_app();
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.get_by_label(BODY_LABEL).type_text("Thanks @bo");
    h.run();
    assert!(mention_list_open(&h.ctx, cid));
    h.get_by_label(MENTION_LIST_LABEL);
    h.get_by_label("Bob Stone <bob@example.net>");
    h.key_press(Key::Enter);
    h.run();
    let c = compose(&h, cid);
    assert_eq!(c.body.to_plain_text().trim_end(), "Thanks @Bob Stone", "Enter picked, no new paragraph");
    assert_eq!(c.to, "Bob Stone <bob@example.net>");
    assert!(!mention_list_open(&h.ctx, cid));
    // An e-mail address is not a mention; Esc closes the list for that @.
    h.get_by_label(BODY_LABEL).type_text(" mail ann@al");
    h.run();
    assert!(!mention_list_open(&h.ctx, cid));
    h.get_by_label(BODY_LABEL).type_text(" @al");
    h.run();
    assert!(mention_list_open(&h.ctx, cid));
    h.key_press(Key::Escape);
    h.run();
    assert!(!mention_list_open(&h.ctx, cid));
    assert!(h.state().s.composes.contains_key(&cid), "Esc closed the list, not the window");
    // Turned off: nothing is offered.
    set_option(&mut h, "mail.send.mentions", json!(false));
    h.get_by_label(BODY_LABEL).type_text(" @bo");
    h.run();
    assert!(!mention_list_open(&h.ctx, cid));
    assert_eq!(compose(&h, cid).to, "Bob Stone <bob@example.net>");
}

#[test]
fn accessibility_warning_bar_follows_the_checker_options() {
    let (mut app, cid) = app_with_compose(true);
    app.run(
        "insert.picture",
        json!({"compose": cid, "name": "p.gif", "data_base64": "R0lGODlhAQABAAAAACw="}),
    );
    let mut h = inline_harness(app, cid);
    h.run();
    assert!(h.query_by_label(ACCESSIBILITY_BAR_LABEL).is_none(), "no condition holds");
    h.state_mut().s.composes.get_mut(&cid).unwrap().importance = mailcraft_model::Importance::High;
    h.run();
    h.get_by_label(ACCESSIBILITY_BAR_LABEL);
    h.get_by_label_contains("1 issue to check (high importance)");
    h.get_by_label("Check Accessibility").click();
    h.run();
    assert!(
        h.state().s.notices.iter().any(|n| matches!(n, Notice::OpenDialog(d, _) if d == "accessibility"))
    );
    set_option(&mut h, "accessibility.warnings", json!("checkerOnly"));
    h.run();
    assert!(h.query_by_label(ACCESSIBILITY_BAR_LABEL).is_none());
    set_option(&mut h, "accessibility.warnings", json!("whileWorking"));
    set_option(&mut h, "accessibility.warnHighImportance", json!(false));
    h.run();
    h.get_by_label(ACCESSIBILITY_BAR_LABEL);
}

#[test]
fn live_preview_paints_the_gallery_choice_until_the_pointer_moves_on() {
    let (mut h, cid) = inline_body("Preview me");
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run();
    let body = compose(&h, cid).body;
    let ctx = h.ctx.clone();
    request_live_preview(&ctx, cid, "format.highlight", &json!({"color": "#ffff00"}));
    h.step();
    let c = compose(&h, cid);
    assert!(c.preview.is_some(), "previewed");
    assert_eq!(c.body, body, "not applied");
    // No request this frame: back to the body.
    h.step();
    assert!(compose(&h, cid).preview.is_none());
    // Commands without a Live Preview are ignored.
    request_live_preview(&ctx, cid, "compose.send", &json!({}));
    h.step();
    assert!(compose(&h, cid).preview.is_none());
    // Turned off: nothing is previewed.
    set_option(&mut h, "general.livePreview", json!(false));
    request_live_preview(&ctx, cid, "format.highlight", &json!({"color": "#ffff00"}));
    h.step();
    assert!(compose(&h, cid).preview.is_none());
    assert_eq!(compose(&h, cid).body, body);
}

#[test]
fn ctrl_shift_angle_brackets_grow_and_shrink_the_font_in_the_body() {
    let (app, cid) = app_with_compose(false);
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.get_by_label(BODY_LABEL).type_text("abc");
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run();
    // The size of the first run (0 = inherited).
    let size = |h: &Harness<'_, App>| match compose(h, cid).body.blocks.first() {
        Some(mailcraft_model::Block::Paragraph { inlines, .. }) => inlines
            .iter()
            .find_map(|i| match i {
                mailcraft_model::Inline::Text { style, .. } => Some(style.size_pt.unwrap_or(0.0)),
                _ => None,
            })
            .unwrap_or(0.0),
        _ => 0.0,
    };
    let before = size(&h);
    // Ctrl+> (Ctrl+Shift+.) grows, Ctrl+< (Ctrl+Shift+,) shrinks: the body leaves them to the window keys.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Period);
    h.run();
    let grown = size(&h);
    assert!(grown > before, "{before:?} → {grown:?}");
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Comma);
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Comma);
    h.run();
    assert!(size(&h) < grown);
    assert_eq!(compose(&h, cid).body.to_plain_text().trim_end(), "abc", "no text typed");
}

#[test]
fn hovering_a_colour_in_the_font_colour_menu_previews_it() {
    let (mut app, cid) = app_with_compose(false);
    app.s.view.simplified_ribbon = false;
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.get_by_label(BODY_LABEL).type_text("Colour me");
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run();
    let body = compose(&h, cid).body;
    h.get_by_label("Font Color options").click();
    h.run();
    h.get_by_label("Orange, Accent 2").hover();
    h.run();
    let c = compose(&h, cid);
    let preview = c.preview.clone().expect("previewed while the pointer rests on the colour");
    assert_ne!(preview, body);
    assert_eq!(c.body, body, "nothing applied");
    // The pointer moves off the menu.
    let p = body_point(&h, 300.0) + egui::vec2(0.0, 200.0);
    push(&mut h, Event::PointerMoved(p));
    h.run();
    assert!(compose(&h, cid).preview.is_none());
    assert_eq!(compose(&h, cid).body, body);
}

// ---------------------------------------------------------------------------------------------------------
// Ribbon clipboard, Format Painter and the Format Text dialogs

fn copied(h: &Harness<'_, App>) -> Vec<String> {
    h.output()
        .platform_output
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

fn classic_body(text: &str) -> (Harness<'static, App>, u64) {
    let (mut app, cid) = app_with_compose(false);
    app.s.view.simplified_ribbon = false;
    let mut h = window_harness(app);
    h.run();
    h.get_by_label(BODY_LABEL).click();
    h.run();
    h.get_by_label(BODY_LABEL).type_text(text);
    h.run();
    (h, cid)
}

#[test]
fn ribbon_copy_and_cut_reach_the_system_clipboard() {
    let (mut h, cid) = classic_body("alpha beta");
    select_range(h.state_mut(), cid, 0, 5);
    h.run();
    h.get_by_label("Copy").click();
    h.step();
    assert_eq!(copied(&h), vec!["alpha".to_owned()], "the ribbon's Copy fills the system clipboard");
    h.run();
    select_range(h.state_mut(), cid, 6, 10);
    h.run();
    h.get_by_label("Cut").click();
    h.step();
    assert_eq!(copied(&h), vec!["beta".to_owned()]);
    h.run();
    assert_eq!(compose(&h, cid).body.to_plain_text().trim_end(), "alpha");
    // The body has the keyboard again: typing goes on at the caret.
    push(&mut h, Event::Text("!".into()));
    h.run();
    assert_eq!(compose(&h, cid).body.to_plain_text().trim_end(), "alpha !");
}

#[test]
fn ribbon_paste_takes_the_system_clipboard() {
    let (mut app, cid) = app_with_compose(false);
    let ctx = egui::Context::default();
    // Text copied in another program.
    assert!(paste_system_clipboard(&ctx, &mut app, cid, "from elsewhere", None, None, None));
    assert!(app.s.composes[&cid].body.to_plain_text().contains("from elsewhere"));
    // Its HTML flavour, with a mode.
    let html = Some("<p><b>loud</b></p>".to_owned());
    assert!(paste_system_clipboard(&ctx, &mut app, cid, "loud", html, None, Some("keepSource")));
    let bold = app.s.composes[&cid].body.blocks.iter().any(|b| match b {
        mailcraft_model::Block::Paragraph { inlines, .. } => inlines.iter().any(|i| {
            matches!(i, mailcraft_model::Inline::Text { text, style } if text.contains("loud") && style.bold == Some(true))
        }),
        _ => false,
    });
    assert!(bold);
    // Only a picture on the clipboard.
    let png = mailcraft_mime::transfer::decode_base64(PNG_B64.as_bytes());
    let pictures = app.s.composes[&cid].inline_images.len();
    assert!(paste_system_clipboard(&ctx, &mut app, cid, "", None, Some(png), None));
    assert_eq!(app.s.composes[&cid].inline_images.len(), pictures + 1);
    // Nothing at all, or far too much text: nothing happens.
    let before = app.s.composes[&cid].body.clone();
    assert!(!paste_system_clipboard(&ctx, &mut app, cid, "", None, None, None));
    assert!(!paste_system_clipboard(&ctx, &mut app, cid, &"x".repeat(MAX_PASTE_TEXT + 1), None, None, None));
    assert_eq!(app.s.composes[&cid].body, before);
}

fn elsewhere_text() -> Option<String> {
    Some("from elsewhere".into())
}

fn unreadable_text() -> Option<String> {
    None
}

#[test]
fn ribbon_paste_button_reads_the_system_clipboard() {
    let (mut h, cid) = classic_body("x ");
    set_clipboard_hooks(ClipboardHooks { read_text: Some(elsewhere_text), ..ClipboardHooks::default() });
    h.get_by_label("Paste").click();
    h.run();
    h.run();
    set_clipboard_hooks(ClipboardHooks::default());
    assert!(
        compose(&h, cid).body.to_plain_text().contains("from elsewhere"),
        "{}",
        compose(&h, cid).body.to_plain_text()
    );
}

#[test]
fn ribbon_paste_falls_back_to_the_in_app_clipboard_when_the_system_one_cannot_be_read() {
    // Regression: with a reader installed that could not read the clipboard, Paste pasted nothing and
    // never used what the ribbon's Copy had just put on the in-app clipboard.
    let (mut h, cid) = classic_body("alpha beta");
    select_range(h.state_mut(), cid, 0, 5);
    h.run();
    h.get_by_label("Copy").click();
    h.run();
    select_range(h.state_mut(), cid, 10, 10);
    h.run();
    set_clipboard_hooks(ClipboardHooks { read_text: Some(unreadable_text), ..ClipboardHooks::default() });
    h.get_by_label("Paste").click();
    h.run();
    h.run();
    set_clipboard_hooks(ClipboardHooks::default());
    let text = compose(&h, cid).body.to_plain_text();
    assert_eq!(text.matches("alpha").count(), 2, "{text}");
}

/// Clicks `n` times at `pos`, one click per frame (frames are a quarter second apart, inside egui's
/// double-click interval), then runs the harness.
fn multi_click(h: &mut Harness<'_, App>, pos: Pos2, n: usize) {
    push(h, Event::PointerMoved(pos));
    for _ in 0..n {
        for pressed in [true, false] {
            push(
                h,
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                },
            );
        }
        h.step();
    }
    h.run();
}

#[test]
fn one_shot_format_painter_paints_a_triple_clicked_paragraph() {
    // Regression: the first click of the triple-click painted the word under it and used the painter up,
    // so the paragraph the triple-click selected was never painted.
    let (mut h, cid) = inline_body("Make this bold please and the rest too");
    select_range(h.state_mut(), cid, 0, 4);
    h.state_mut().run("format.bold", json!({"compose": cid})).unwrap();
    select_range(h.state_mut(), cid, 1, 1);
    h.state_mut().run("format.formatPainter", json!({"compose": cid})).unwrap();
    h.run();
    let at = body_point(&h, 120.0);
    multi_click(&mut h, at, 3);
    let c = compose(&h, cid);
    assert!(c.painter.is_none(), "a one-shot painter is used up");
    let plain = c.body.to_plain_text();
    assert!(
        c.selection.focus.offset.abs_diff(c.selection.anchor.offset) + 1 >= plain.trim_end().chars().count()
    );
    assert!(c.selection_all(|st, _| st.bold == Some(true)), "the whole paragraph is painted");
    // A single click still paints the word under it, once the double-click interval has passed.
    let (mut h, cid) = inline_body("Make this bold please");
    select_range(h.state_mut(), cid, 0, 4);
    h.state_mut().run("format.bold", json!({"compose": cid})).unwrap();
    select_range(h.state_mut(), cid, 1, 1);
    h.state_mut().run("format.formatPainter", json!({"compose": cid})).unwrap();
    h.run();
    let at = body_point(&h, 120.0);
    multi_click(&mut h, at, 1);
    for _ in 0..4 {
        h.step();
    }
    let c = compose(&h, cid);
    assert!(c.painter.is_none());
    let bold: String = c
        .body
        .blocks
        .iter()
        .flat_map(|b| match b {
            mailcraft_model::Block::Paragraph { inlines, .. } => inlines.clone(),
            _ => Vec::new(),
        })
        .filter_map(|i| match i {
            mailcraft_model::Inline::Text { text, style } if style.bold == Some(true) => Some(text),
            _ => None,
        })
        .collect();
    assert!(
        bold.contains("Make") && bold.len() > "Make".len() && bold.len() < c.body.to_plain_text().len(),
        "{bold}"
    );
}

#[test]
fn double_clicking_the_format_painter_button_keeps_it_armed() {
    let (mut h, cid) = classic_body("alpha beta");
    select_range(h.state_mut(), cid, 1, 1);
    h.run();
    let at = h.get_by_label("Format Painter").rect().center();
    multi_click(&mut h, at, 2);
    let c = compose(&h, cid);
    assert!(c.painter.as_ref().is_some_and(|p| p.sticky), "{:?}", c.painter);
}

#[test]
fn format_painter_paints_the_whole_drag_and_esc_disarms_it() {
    let (mut h, cid) = inline_body("Make this bold please");
    select_range(h.state_mut(), cid, 0, 4);
    h.state_mut().run("format.bold", json!({"compose": cid})).unwrap();
    select_range(h.state_mut(), cid, 1, 1);
    h.state_mut().run("format.formatPainter", json!({"compose": cid})).unwrap();
    h.run();
    drag_select(&mut h, 60.0, 160.0);
    let c = compose(&h, cid);
    assert!(c.painter.is_none(), "a one-shot painter is used up");
    assert!(c.selection.focus.offset > c.selection.anchor.offset + 3, "{:?}", c.selection);
    assert!(
        c.selection_all(|st, _| st.bold == Some(true)),
        "every dragged character is painted, not just the first"
    );
    // Esc puts an armed painter (or pen) away and leaves the window open.
    h.state_mut().run("format.formatPainter", json!({"compose": cid, "sticky": true})).unwrap();
    let ctx = h.ctx.clone();
    let r =
        crate::keys::apply(&ctx, h.state_mut(), crate::Surface::Compose(cid), crate::keys::Action::Escape);
    assert_eq!(r["closed"], "painter");
    assert!(compose(&h, cid).painter.is_none());
}

#[test]
fn font_dialog_applies_effects_from_its_tabs() {
    let (mut app, cid) = app_with_compose(false);
    app.run("compose.insertText", json!({"compose": cid, "text": "fancy words"})).unwrap();
    select_range(&mut app, cid, 0, 5);
    let mut h = full_harness(app);
    h.run();
    h.state_mut().run("format.fontDialog", json!({"compose": cid})).unwrap();
    h.run();
    h.get_by_label("Small caps").click();
    h.run();
    h.get_by_label("Advanced").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    let c = compose(&h, cid);
    assert!(c.selection_all(|st, _| st.small_caps == Some(true)));
    assert!(h.query_by_label("Small caps").is_none(), "closed");
}

#[test]
fn paragraph_dialog_ok_keeps_what_it_opened_with() {
    let (mut app, cid) = app_with_compose(false);
    app.run("compose.insertText", json!({"compose": cid, "text": "spaced out"})).unwrap();
    app.run("format.align", json!({"compose": cid, "kind": "right"})).unwrap();
    app.run("format.paragraphSpacing", json!({"compose": cid, "before": 12, "after": 24})).unwrap();
    let before = app.s.composes[&cid].para_at_caret();
    let mut h = full_harness(app);
    h.run();
    h.state_mut().run("format.paragraphDialog", json!({"compose": cid})).unwrap();
    h.run();
    h.get_by_label("Line and Page Breaks").click();
    h.run();
    h.get_by_label("Keep with next").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    let after = compose(&h, cid).para_at_caret();
    assert_eq!(
        (after.align, after.space_before_px, after.space_after_px),
        (before.align, before.space_before_px, before.space_after_px),
        "OK no longer resets alignment and spacing"
    );
    assert!(after.keep_with_next);
}

#[test]
fn paragraph_dialog_over_several_paragraphs_changes_only_what_was_changed() {
    // Regression: OK sent every field with the caret paragraph's values, so ticking one box over two
    // paragraphs gave the second the first one's spacing and an explicit line height.
    let (mut app, cid) = app_with_compose(false);
    app.run("compose.insertText", json!({"compose": cid, "text": "first\nsecond"})).unwrap();
    app.run(
        "compose.select",
        json!({"compose": cid, "anchor": {"path": [1], "offset": 0}, "focus": {"path": [1], "offset": 6}}),
    )
    .unwrap();
    app.run("format.paragraphSpacing", json!({"compose": cid, "before": 12, "after": 24})).unwrap();
    app.run("format.selectAll", json!({"compose": cid})).unwrap();
    let paras = |c: &ComposeState| -> Vec<mailcraft_model::rich::ParaStyle> {
        c.body
            .blocks
            .iter()
            .filter_map(|b| match b {
                mailcraft_model::Block::Paragraph { style, .. } => Some(style.clone()),
                _ => None,
            })
            .collect()
    };
    let before = paras(&app.s.composes[&cid]);
    assert_ne!(before[0].space_before_px, before[1].space_before_px);
    let mut h = full_harness(app);
    h.run();
    h.state_mut().run("format.paragraphDialog", json!({"compose": cid, "tab": "breaks"})).unwrap();
    h.run();
    h.get_by_label("Keep with next").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    let after = paras(&compose(&h, cid));
    for (a, b) in after.iter().zip(&before) {
        assert!(a.keep_with_next);
        assert_eq!(
            (a.space_before_px, a.space_after_px, a.indent_px, a.line_height),
            (b.space_before_px, b.space_after_px, b.indent_px, b.line_height)
        );
    }
}

#[test]
fn more_colors_picks_from_the_standard_grid() {
    let (mut app, cid) = app_with_compose(false);
    app.run("compose.insertText", json!({"compose": cid, "text": "tinted"})).unwrap();
    select_range(&mut app, cid, 0, 6);
    let mut h = full_harness(app);
    h.run();
    h.state_mut().run("format.color", json!({"compose": cid, "custom": true})).unwrap();
    h.run();
    h.get_by_label("Custom").click();
    h.run();
    h.get_by_label("Standard").click();
    h.run();
    // A swatch other than the starting colour, so the grid itself is what sets it.
    let pick = crate::dialogs::format_dlg::standard_palette().get(39).copied().unwrap();
    let label = format!("#{:02x}{:02x}{:02x}", pick[0], pick[1], pick[2]);
    assert_ne!(label, "#1f5fbf");
    h.get_by_label(&label).click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    let c = compose(&h, cid);
    assert!(c.selection_all(|st, _| st.color == mailcraft_geom::Color::parse_hex(&label)), "{label}");
    // Page Color › More Colors... uses the same picker, with No Color.
    h.state_mut().run("compose.pageColor", json!({"compose": cid})).unwrap();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    assert!(compose(&h, cid).body.background.is_some());
    h.state_mut().run("compose.pageColor", json!({"compose": cid})).unwrap();
    h.run();
    h.get_by_label("No Color").click();
    h.run();
    assert!(compose(&h, cid).body.background.is_none());
}
