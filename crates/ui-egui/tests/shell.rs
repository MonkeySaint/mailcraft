//! Shell tests: the main window renders the sample mailbox at the observed window sizes (plan/reference/
//! layout.md, responsive.md), checked through the accessibility tree and the shell's layout snapshot.

#![allow(clippy::unwrap_used)]

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mailcraft_engine::queries::{self, ListRow};
use mailcraft_engine::sample::sample_session;
use mailcraft_model::{Address, Body, BodyFormat, DateTime, MailItem};
use mailcraft_ui_egui::App;
use serde_json::json;

fn harness(w: f32, h: f32) -> Harness<'static, App> {
    let app = App::new(sample_session());
    let mut fonts_done = false;
    let mut h = Harness::builder()
        .with_accessibility_check(false)
        .allow_missing_glyphs()
        .with_size(egui::vec2(w, h))
        .build_ui_state(
            move |ui, app: &mut App| {
                if !fonts_done {
                    App::install_fonts(ui.ctx());
                    fonts_done = true;
                }
                app.ui_root(ui);
            },
            app,
        );
    h.run_steps(4);
    h
}

/// Subject of the first message row in the current view.
fn first_subject(app: &App) -> String {
    queries::list_rows(&app.s)
        .into_iter()
        .find_map(|r| match r {
            ListRow::Item(i) => Some(i.subject),
            ListRow::Group { .. } => None,
        })
        .unwrap()
}

fn first_item(app: &App) -> u64 {
    queries::list_rows(&app.s)
        .into_iter()
        .find_map(|r| match r {
            ListRow::Item(i) => Some(i.id.0),
            ListRow::Group { .. } => None,
        })
        .unwrap()
}

fn assert_status_counts(h: &Harness<'_, App>) {
    let (items, unread) = queries::status_counts(&h.state().s);
    assert!(items > 0, "sample Inbox is empty");
    h.get_by_label(&format!("Items: {items}"));
    if unread > 0 {
        h.get_by_label(&format!("Unread: {unread}"));
    }
}

fn has_label(h: &Harness<'_, App>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some() || h.query_all_by_label_contains(label).next().is_some()
}

#[test]
fn wide_window_shows_all_three_panes() {
    let h = harness(1920.0, 1040.0);
    let layout = App::shell_layout(&h.ctx);
    assert_eq!(layout.panes, ["folders", "list", "reading"]);
    assert_eq!(layout.folder_pane_width, 230.0);
    assert!(layout.tabs_shown && layout.ribbon_shown);
    let list = layout.rect("list").unwrap();
    assert_eq!(list.width(), 300.0);
    let reading = layout.rect("reading").unwrap();
    assert!(reading.width() > 1000.0, "reading pane takes the rest: {reading:?}");

    assert_status_counts(&h);
    for folder in ["Inbox", "Sent Items", "Deleted Items"] {
        assert!(has_label(&h, folder), "folder {folder} not visible");
    }
    let subject = first_subject(h.state());
    assert!(has_label(&h, &subject), "subject {subject:?} not visible");
    // Chrome: File, the tabs, the module rail and the Quick Access Toolbar.
    for label in ["File", "Home", "View", "Mail", "Calendar", "People", "To Do", "More Apps", "Undo"] {
        assert!(h.query_all_by_label(label).next().is_some(), "{label} missing");
    }
}

#[test]
fn eight_hundred_wide_minimises_the_folder_pane() {
    let h = harness(800.0, 700.0);
    let layout = App::shell_layout(&h.ctx);
    assert_eq!(layout.panes, ["folders", "list", "reading"]);
    assert_eq!(layout.folder_pane_width, 26.0);
    assert_status_counts(&h);
    assert!(has_label(&h, "Inbox"), "minimised strip shows Inbox");
    let subject = first_subject(h.state());
    assert!(has_label(&h, &subject), "subject {subject:?} not visible");
}

#[test]
fn six_hundred_wide_is_single_pane() {
    let mut h = harness(600.0, 600.0);
    let layout = App::shell_layout(&h.ctx);
    assert_eq!(layout.panes, ["folders", "list"], "only the list shows with nothing selected");
    assert_status_counts(&h);
    let subject = first_subject(h.state());
    assert!(has_label(&h, &subject), "subject {subject:?} not visible");

    // Selecting a message swaps the list for the reading pane.
    let id = first_item(h.state());
    h.state_mut().run("nav.selectItem", json!({ "id": id })).unwrap();
    h.run_steps(3);
    assert_eq!(App::shell_layout(&h.ctx).panes, ["folders", "reading"]);

    // Back (the reading pane's ← button clears this flag) shows the list again.
    h.state_mut().ui.single_pane_reading = false;
    h.run_steps(3);
    assert_eq!(App::shell_layout(&h.ctx).panes, ["folders", "list"]);
}

#[test]
fn reading_pane_bottom_and_off() {
    let mut h = harness(1280.0, 900.0);
    h.state_mut().run("view.readingPane", json!({ "position": "bottom" })).unwrap();
    h.run_steps(3);
    let l = App::shell_layout(&h.ctx);
    assert_eq!(l.panes, ["folders", "list", "reading"]);
    let (list, reading) = (l.rect("list").unwrap(), l.rect("reading").unwrap());
    assert!(reading.top() > list.bottom() && (list.width() - reading.width()).abs() < 1.0);

    h.state_mut().run("view.readingPane", json!({ "position": "off" })).unwrap();
    h.run_steps(3);
    assert_eq!(App::shell_layout(&h.ctx).panes, ["folders", "list"]);
}

#[test]
fn other_modules_and_ribbon_modes() {
    let mut h = harness(1440.0, 900.0);
    for (cmd, pane) in [
        ("nav.calendar", "calendar"),
        ("nav.people", "people"),
        ("nav.tasks", "tasks"),
        ("nav.notes", "notes"),
    ] {
        h.state_mut().run(cmd, json!({})).unwrap();
        h.run_steps(3);
        assert_eq!(App::shell_layout(&h.ctx).panes, [pane], "{cmd}");
    }
    h.state_mut().run("nav.mail", json!({})).unwrap();
    h.state_mut().run("app.toggleRibbonMode", json!({ "mode": "tabsOnly" })).unwrap();
    h.run_steps(3);
    let l = App::shell_layout(&h.ctx);
    assert!(l.tabs_shown && !l.ribbon_shown);
    h.state_mut().run("app.toggleRibbonMode", json!({ "mode": "autoHide" })).unwrap();
    h.run_steps(3);
    let l = App::shell_layout(&h.ctx);
    assert!(!l.tabs_shown && !l.ribbon_shown);
    assert!(has_label(&h, "Show Ribbon"));
}

#[test]
fn notices_become_dialogs_and_toasts() {
    let mut h = harness(1280.0, 800.0);
    h.state_mut().run("view.reminderWindow", json!({})).unwrap();
    h.state_mut().s.notices.push(mailcraft_engine::Notice::Info("Hello from a test".into()));
    h.run_steps(2);
    let app = h.state();
    assert!(app.s.notices.is_empty());
    // The Reminders window is the modules area's own window, not a queued dialog.
    assert!(!app.ui.dialogs.iter().any(|(id, _)| id == "reminders"), "{:?}", app.ui.dialogs);
    assert!(has_label(&h, "Dismiss All"));
    assert!(has_label(&h, "Hello from a test"));

    // Search focus opens the Search tab instead of a dialog (with no dialog holding the keyboard focus).
    h.state_mut().ui.dialogs.clear();
    h.run_steps(1);
    h.state_mut().run("search.focus", json!({})).unwrap();
    h.run_steps(2);
    assert!(h.state().ui.search_active);
    assert_eq!(h.state().ui.tab(mailcraft_ui_egui::Surface::Main), Some("Search"));

    // File › Info opens the backstage.
    h.state_mut().run("file.info", json!({})).unwrap();
    h.run_steps(2);
    assert!(h.state().ui.backstage.is_some());
}

#[test]
fn recent_searches_answer_the_arrow_keys() {
    let mut h = harness(1400.0, 900.0);
    h.state_mut().s.search.recent = vec!["alpha".into(), "beta".into(), "gamma".into()];
    h.state_mut().run("search.focus", json!({})).unwrap();
    h.run_steps(3);
    assert!(has_label(&h, "Recent Searches"), "focusing the empty box lists recent searches");
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(1);
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(1);
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(1);
    h.key_press(egui::Key::ArrowUp);
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().s.view.search, "beta");
}

#[test]
fn tiny_window_does_not_panic() {
    let mut h = harness(400.0, 400.0);
    h.state_mut().run("view.readingPane", json!({ "position": "bottom" })).unwrap();
    h.run_steps(2);
    h.set_size(egui::vec2(1.0, 1.0));
    h.run_steps(2);
}

#[test]
fn todo_bar_follows_the_view_command() {
    let mut h = harness(1600.0, 900.0);
    assert!(!App::shell_layout(&h.ctx).panes.contains(&"todoBar"));
    h.state_mut().run("view.todoBar", json!({"part": "calendar"})).unwrap();
    h.run_steps(3);
    let l = App::shell_layout(&h.ctx);
    assert!(l.panes.contains(&"todoBar"), "{:?}", l.panes);
    assert!(l.panes.contains(&"list"), "the mail panes stay beside the bar");
    assert_eq!(h.state().ui.todo_bar_parts, (true, false, false));
    h.state_mut().run("view.todoBar", json!({"part": "off"})).unwrap();
    h.run_steps(3);
    assert!(!App::shell_layout(&h.ctx).panes.contains(&"todoBar"));
    assert_eq!(h.state().ui.todo_bar_parts, (false, false, false));
}

#[test]
fn from_refiner_puts_the_caret_between_the_quotes() {
    let mut h = harness(1600.0, 900.0);
    h.state_mut().run("search.query", json!({"text": "budget"})).unwrap();
    h.run_steps(2);
    let r = h.state_mut().run("search.refine", json!({"kind": "from"})).unwrap();
    assert_eq!(r["edit"], json!([13, 13]));
    h.run_steps(4);
    assert!(h.state().ui.search_active);
    assert_eq!(h.state().ui.search_text, "budget from:\"\"");
    let box_id = egui::Id::unique("mailcraft_search_box");
    assert!(h.ctx.memory(|m| m.has_focus(box_id)), "the search box has the keyboard focus");
    let state = egui::TextEdit::load_state(&h.ctx, box_id).expect("search box state");
    let range = state.cursor.char_range().expect("caret");
    let caret = egui::text::CCursor::new(13);
    assert_eq!((range.primary, range.secondary), (caret, caret));
}

#[test]
fn bulk_folder_actions_and_ignore_ask_first() {
    let mut h = harness(1600.0, 900.0);
    let inbox = h.state().s.selection.folder;
    let count = h.state().s.mailbox.total_count(inbox);
    let first = queries::visible_item_ids(&h.state().s).first().copied().unwrap();
    h.state_mut().run("nav.selectItem", json!({"id": first.0})).unwrap();
    h.state_mut().run("mail.ignore", json!({})).unwrap();
    h.run_steps(3);
    assert!(has_label(&h, "Ignore Conversation"), "the confirmation is up");
    assert!(has_label(&h, "Don't ask me again"));
    assert!(!h.state().s.mailbox.mail.get(first).unwrap().ignored, "nothing ignored before the answer");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Ignore Conversation").click();
    h.run_steps(3);
    assert!(h.state().s.mailbox.mail.get(first).unwrap().ignored);
    assert!(h.state().s.mailbox.total_count(inbox) <= count);
    // A message whose whole text is quoted in a later reply: Clean Up would move it.
    let redundant = {
        let s = &mut h.state_mut().s;
        let now = s.now();
        let item = |subject: &str, unix: i64| MailItem {
            folder: inbox,
            subject: subject.into(),
            from: Some(Address::new(Some("Ann Lee"), "ann@example.com")),
            received: DateTime { unix, ..now },
            read: true,
            ..Default::default()
        };
        let text = |text: &str| {
            mailcraft_engine::Content::body(Body {
                format: BodyFormat::PlainText,
                text: Some(text.into()),
                html: None,
            })
        };
        let mut original = item("Picnic", now.unix - 120);
        original.message_id = Some("picnic-1@example.com".into());
        let mut reply = item("RE: Picnic", now.unix - 60);
        reply.in_reply_to = vec!["picnic-1@example.com".into()];
        reply.references = vec!["picnic-1@example.com".into()];
        let id = s.add_message(original, &text("Shall we meet at noon by the lake?")).unwrap();
        s.add_message(reply, &text("Yes, see you there.\n\n> Shall we meet at noon by the lake?")).unwrap();
        id
    };
    let ids_in = |h: &Harness<'_, App>| {
        let mut ids: Vec<u64> = h.state().s.mailbox.items_in(inbox).iter().map(|m| m.id.0).collect();
        ids.sort_unstable();
        ids
    };
    let before = ids_in(&h);
    let before_count = h.state().s.mailbox.total_count(inbox);
    h.state_mut().run("mail.cleanUpFolder", json!({})).unwrap();
    h.run_steps(3);
    assert!(has_label(&h, "Clean Up Folder"), "Clean Up asks first");
    assert_eq!(ids_in(&h), before, "nothing moves before the answer");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Cancel").click();
    h.run_steps(3);
    assert_eq!(ids_in(&h), before, "Cancel leaves the folder alone");
    assert_eq!(h.state().s.mailbox.total_count(inbox), before_count);
    // Answering yes does clean up, so the checks above are not vacuous.
    h.state_mut().run("mail.cleanUpFolder", json!({})).unwrap();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Clean Up Folder").click();
    h.run_steps(3);
    assert!(!ids_in(&h).contains(&redundant.0), "the quoted message was cleaned up");
}
