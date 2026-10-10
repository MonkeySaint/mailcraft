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
