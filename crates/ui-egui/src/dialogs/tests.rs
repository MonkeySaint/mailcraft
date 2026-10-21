//! Dialog and backstage tests driven through egui_kittest (accessibility tree + simulated input).

use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::{Value, json};

use crate::{App, Surface};
use mailcraft_engine::sample::sample_session;
use mailcraft_model::{FlagState, FolderId, ItemId, RuleAction, RuleCondition};

fn harness(app: App) -> Harness<'static, App> {
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).build_ui_state(
        |ui, app: &mut App| {
            let ctx = ui.ctx().clone();
            crate::backstage::backstage(&ctx, app);
            // Same order as the app: item windows and module dialogs take their requests first.
            crate::modules::pim_windows(&ctx, app);
            crate::dialogs::dialogs(&ctx, app);
        },
        app,
    );
    h.run_steps(2);
    h
}

/// A fresh scratch directory under the cargo target dir (never the system temp dir, which may be RAM).
/// Unit tests get no `CARGO_TARGET_TMPDIR`, so it is `<target>/tmp`, found from the test binary's path
/// (`<target>/<profile>/deps/<binary>`).
fn scratch_dir(name: &str) -> std::path::PathBuf {
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok()?.ancestors().nth(3).map(|t| t.join("tmp")))
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp"));
    let dir = base.join(format!("mc-dlg-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn scratch_dir_is_inside_the_target_dir() {
    let dir = scratch_dir("probe");
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap();
    assert!(dir.starts_with(target) || option_env!("CARGO_TARGET_TMPDIR").is_some(), "{}", dir.display());
}

fn sample_app() -> App {
    App::new(sample_session())
}

/// First message in the selected folder, selected and focused.
fn select_first(app: &mut App) -> u64 {
    let folder = app.s.selection.folder;
    let id = app.s.mailbox.items_in(folder).first().map(|m| m.id.0).expect("sample Inbox has mail");
    app.run("nav.selectItem", json!({"id": id})).expect("select");
    id
}

fn projects(app: &App) -> u64 {
    app.s
        .mailbox
        .folders
        .values()
        .find(|f| f.name == "Projects")
        .map(|f| f.id.0)
        .expect("sample has Projects")
}

fn names(h: &Harness<'_, App>) -> Vec<String> {
    h.state().ui.dialogs.iter().map(|(n, _)| n.clone()).collect()
}

fn step(h: &mut Harness<'_, App>) {
    h.run_steps(3);
}

#[test]
fn create_rule_end_to_end() {
    let mut app = sample_app();
    let item = select_first(&mut app);
    let from_name = app
        .s
        .mailbox
        .mail
        .get(&ItemId(item))
        .and_then(|m| m.from.clone())
        .map(|f| f.display().to_owned())
        .unwrap();
    let target = projects(&app);
    app.run("rules.createFromMessage", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["createRule"]);
    h.get_by_label(&format!("From {from_name}")).click();
    h.get_by_label("Move the item to folder:").click();
    step(&mut h);
    h.get_by_label("Select Folder...").click();
    step(&mut h);
    assert_eq!(names(&h), ["createRule", "pickFolder"]);
    h.get_by_label("Projects").click();
    step(&mut h);
    h.key_press(Key::Enter);
    step(&mut h);
    assert_eq!(names(&h), ["createRule"], "picker closes and replies");
    h.get_by_label("OK").click();
    step(&mut h);
    assert_eq!(names(&h), ["ruleCreated"]);
    assert!(h.query_by_label_contains("has been created").is_some());
    h.get_by_label_contains("Run this rule now").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    let app = h.state();
    assert_eq!(app.s.mailbox.rules.len(), 1);
    let rule = &app.s.mailbox.rules[0];
    assert!(matches!(&rule.conditions[..], [RuleCondition::From(_)]));
    assert_eq!(rule.actions, vec![RuleAction::MoveTo(FolderId(target))]);
    // Run now moved the selected message.
    assert_eq!(app.s.mailbox.mail.get(&ItemId(item)).map(|m| m.folder.0), Some(target));
}

#[test]
fn rules_and_alerts_toggle_rename_delete() {
    let mut app = sample_app();
    app.run(
        "rules.create",
        json!({"name": "Budget", "conditions": [{"SubjectContains": ["budget"]}], "actions": ["MarkAsRead"]}),
    )
    .unwrap();
    app.run("rules.manage", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["rulesAndAlerts"]);
    assert!(h.query_by_label("Budget").is_some());
    // Description pane shows the underlined value.
    assert!(h.query_by_label("budget").is_some());
    h.query_all_by_role(egui::accesskit::Role::CheckBox).next().unwrap().click();
    step(&mut h);
    assert!(!h.state().s.mailbox.rules[0].enabled);
    h.get_by_label("Delete").click();
    step(&mut h);
    assert_eq!(names(&h), ["rulesAndAlerts", "confirm"]);
    assert!(h.query_by_label("Delete rule \"Budget\"?").is_some());
    h.get_by_label("Yes").click();
    step(&mut h);
    assert!(h.state().s.mailbox.rules.is_empty());
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
}

#[test]
fn rules_wizard_builds_a_rule_from_a_template() {
    let mut app = sample_app();
    let target = projects(&app);
    app.run("rules.new", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["rulesWizard"]);
    h.get_by_label("Move messages with specific words in the subject to a folder").click();
    step(&mut h);
    h.get_by_label("Next >").click();
    step(&mut h);
    assert!(h.query_by_label("Which condition(s) do you want to check?").is_some());
    h.query_all_by_label("specific words").next().unwrap().click();
    step(&mut h);
    h.get_by_role(egui::accesskit::Role::TextInput).focus();
    step(&mut h);
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("quarterly report");
    step(&mut h);
    h.get_by_label("Done").click();
    step(&mut h);
    h.get_by_label("Next >").click();
    step(&mut h);
    assert!(h.query_by_label("What do you want to do with the message?").is_some());
    h.query_all_by_label("specified").next().unwrap().click();
    step(&mut h);
    h.get_by_label("Choose Folder...").click();
    step(&mut h);
    assert_eq!(names(&h), ["rulesWizard", "pickFolder"]);
    h.get_by_label("Projects").click();
    step(&mut h);
    h.key_press(Key::Enter);
    step(&mut h);
    h.get_by_label("Done").click();
    step(&mut h);
    h.get_by_label("Next >").click();
    step(&mut h);
    h.get_by_label("Next >").click();
    step(&mut h);
    assert!(h.query_by_label("Finish rule setup.").is_some());
    h.get_by_label("Finish").click();
    step(&mut h);
    assert!(names(&h).is_empty(), "{:?}", names(&h));
    let rule = &h.state().s.mailbox.rules[0];
    assert_eq!(rule.name, "quarterly report");
    assert_eq!(rule.conditions, vec![RuleCondition::SubjectContains(vec!["quarterly report".into()])]);
    assert_eq!(rule.actions, vec![RuleAction::MoveTo(FolderId(target))]);
    assert!(rule.enabled);
}

#[test]
fn wizard_requires_values_before_next() {
    let mut app = sample_app();
    app.run("rules.new", json!({})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Next >").click();
    step(&mut h);
    h.get_by_label("Next >").click();
    step(&mut h);
    assert!(h.query_by_label_contains("fill it in before continuing").is_some());
    h.key_press(Key::Escape);
    step(&mut h);
    assert!(names(&h).is_empty());
    assert!(h.state().s.mailbox.rules.is_empty());
}

#[test]
fn move_to_folder_picker_moves_the_selection() {
    let mut app = sample_app();
    let item = select_first(&mut app);
    let target = projects(&app);
    app.run("mail.move", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["moveToFolder"]);
    h.get_by_label("Projects").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    assert_eq!(h.state().s.mailbox.mail.get(&ItemId(item)).map(|m| m.folder.0), Some(target));
}

#[test]
fn always_move_conversation_can_be_stopped_from_its_dialog() {
    let mut app = sample_app();
    let item = select_first(&mut app);
    let target = projects(&app);
    app.run("mail.alwaysMoveConversation", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["alwaysMoveConversation"]);
    assert!(h.query_by_label("This conversation is not being moved yet.").is_some());
    h.get_by_label("Projects").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    let marked =
        |h: &Harness<'_, App>| h.state().s.mailbox.mail.get(&ItemId(item)).and_then(|m| m.always_move_to);
    assert_eq!(marked(&h), Some(FolderId(target)));
    // Opened again: it says where the conversation goes and unticking the box stops it.
    h.state_mut().run("mail.alwaysMoveConversation", json!({"ids": [item]})).unwrap();
    step(&mut h);
    assert_eq!(names(&h), ["alwaysMoveConversation"]);
    let path = h.state().s.mailbox.folder_path(FolderId(target));
    assert!(h.query_by_label(&format!("New messages in this conversation go to: {path}")).is_some());
    h.get_by_label("Keep moving new messages in this conversation").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    assert_eq!(marked(&h), None);
    // The message already moved stays where it went.
    assert_eq!(h.state().s.mailbox.mail.get(&ItemId(item)).map(|m| m.folder.0), Some(target));
}

#[test]
fn new_folder_dialog_creates_a_folder() {
    let mut app = sample_app();
    app.run("folder.new", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["createFolder"]);
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("Receipts 2026");
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    assert!(h.state().s.mailbox.folders.values().any(|f| f.name == "Receipts 2026"));
}

#[test]
fn color_categories_new_category_is_assigned() {
    let mut app = sample_app();
    let item = select_first(&mut app);
    app.run("mail.allCategories", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["colorCategories"]);
    h.get_by_label("New...").click();
    step(&mut h);
    assert_eq!(names(&h), ["colorCategories", "newCategory"]);
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("Urgent");
    step(&mut h);
    h.key_press(Key::Enter);
    step(&mut h);
    assert_eq!(names(&h), ["colorCategories"]);
    h.get_by_label("OK").click();
    step(&mut h);
    let app = h.state();
    assert!(app.s.mailbox.categories.iter().any(|c| c.name == "Urgent"));
    assert!(app.s.mailbox.mail.get(&ItemId(item)).unwrap().categories.iter().any(|c| c == "Urgent"));
}

#[test]
fn custom_flag_sets_label_and_due() {
    let mut app = sample_app();
    let item = select_first(&mut app);
    app.run("mail.flagCustom", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["customFlag"]);
    h.query_all_by_role(egui::accesskit::Role::TextInput).nth(2).unwrap().focus();
    step(&mut h);
    h.query_all_by_role(egui::accesskit::Role::TextInput).nth(2).unwrap().type_text("2026-10-20");
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty(), "{:?}", names(&h));
    match &h.state().s.mailbox.mail.get(&ItemId(item)).unwrap().flag {
        FlagState::Flagged { label, due, .. } => {
            assert_eq!(label, "Follow up");
            assert!(due.is_some());
        }
        other => panic!("not flagged: {other:?}"),
    }
}

#[test]
fn options_apply_engine_settings_on_ok() {
    let mut app = sample_app();
    assert!(app.s.view.status_bar);
    app.run("options.open", json!({"page": "advanced"})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["options"]);
    for page in [
        "General",
        "Mail",
        "Calendar",
        "Groups",
        "People",
        "Tasks",
        "Search",
        "Language",
        "Accessibility",
        "Advanced",
        "Customize Ribbon",
        "Quick Access Toolbar",
        "Add-ins",
        "Trust Center",
    ] {
        assert!(h.query_by_label(page).is_some(), "missing page {page}");
    }
    h.get_by_label("Show the status bar").click();
    step(&mut h);
    assert!(h.state().s.view.status_bar, "applied only on OK");
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(!h.state().s.view.status_bar);
    // Mail settings are saved in the profile settings store.
    let mut app = h.into_state();
    app.run("options.open", json!({"page": "mail"})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Play a sound").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(!h.state().s.settings.bool("mail.arrival.playSound"));
}

#[test]
fn every_known_dialog_renders_and_closes() {
    let mut app = sample_app();
    let item = select_first(&mut app);
    let compose =
        app.run("mail.newEmail", json!({})).and_then(|v| v.get("compose").and_then(Value::as_u64)).unwrap();
    let folder = app.s.selection.folder.0;
    let mut h = harness(app);
    for name in crate::dialogs::KNOWN {
        if *name == "exit" {
            continue;
        }
        let params = json!({"ids": [item], "id": item, "item": item, "compose": compose, "folder": folder, "name": "Test", "path": "Inbox", "text": "hello", "rules": [], "items": [], "files": [], "steps": [], "style": {}, "unresolved": [{"name": "zed", "candidates": ["Zed <zed@example.com>"]}]});
        h.state_mut().ui.dialogs.push(((*name).to_owned(), params));
        step(&mut h);
        let open = names(&h);
        if open.is_empty() {
            // Routed elsewhere (search box, main window).
            continue;
        }
        h.key_press(Key::Escape);
        step(&mut h);
        while !h.state().ui.dialogs.is_empty() {
            h.key_press(Key::Escape);
            step(&mut h);
        }
    }
}

#[test]
fn module_dialogs_raised_mid_frame_do_not_flash_a_generic_window() {
    let app = sample_app();
    let mut h = harness(app);
    // Raised by a command during the frame (after the modules drew): the dialogs area leaves it queued.
    h.state_mut()
        .s
        .notices
        .push(mailcraft_engine::Notice::OpenDialog("mailMerge".into(), json!({"ids": []})));
    h.run_steps(1);
    assert!(h.query_by_label_contains("is not available yet").is_none());
    assert!(h.query_by_label_contains("needs a word processor").is_none());
    h.run_steps(2);
    assert!(names(&h).is_empty(), "the modules area took the request: {:?}", names(&h));
}

#[test]
fn unknown_dialog_shows_not_available() {
    let mut app = sample_app();
    app.ui.dialogs.push(("frobnicate".into(), json!({})));
    let mut h = harness(app);
    assert!(h.query_by_label_contains("is not available yet").is_some());
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
}

#[test]
fn import_export_wizard_exports_mbox() {
    let dir = scratch_dir("export");
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("inbox.mbox");
    let mut app = sample_app();
    app.run("file.openExport", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["importExport"]);
    h.get_by_label("Export a folder to an MBOX file").click();
    step(&mut h);
    h.get_by_label("Next >").click();
    step(&mut h);
    h.get_by_role(egui::accesskit::Role::TextInput).focus();
    step(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
    h.get_by_role(egui::accesskit::Role::TextInput).type_text(out.to_str().unwrap());
    step(&mut h);
    h.get_by_label("Finish").click();
    step(&mut h);
    assert_eq!(names(&h), ["message"], "{:?}", h.state().ui.toast);
    assert!(out.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn backstage_pages_navigate_and_switch_theme() {
    let mut app = sample_app();
    select_first(&mut app);
    app.ui.backstage = Some((Surface::Main, "info".into()));
    let mut h = harness(app);
    assert!(h.query_by_label("Account Information").is_some());
    assert!(h.query_by_label("Manage Rules & Alerts...").is_some());
    h.get_by_label("Manage Rules & Alerts...").click();
    step(&mut h);
    assert_eq!(names(&h), ["rulesAndAlerts"]);
    h.key_press(Key::Escape);
    step(&mut h);
    h.get_by_label("Print").click();
    step(&mut h);
    assert_eq!(h.state().ui.backstage, Some((Surface::Main, "print".into())));
    assert!(h.query_by_label("Memo Style").is_some());
    h.get_by_label("Account").click();
    step(&mut h);
    h.get_by_label("Dark").click();
    step(&mut h);
    assert!(h.state().s.view.dark);
    h.get_by_label("Open & Export").click();
    step(&mut h);
    assert!(h.query_by_label("Quick import and export").is_some());
    h.get_by_label("Back").click();
    step(&mut h);
    assert_eq!(h.state().ui.backstage, None);
}

#[test]
fn backstage_options_opens_the_options_dialog() {
    let mut app = sample_app();
    app.ui.backstage = Some((Surface::Main, "info".into()));
    let mut h = harness(app);
    h.get_by_label("Options").click();
    step(&mut h);
    assert_eq!(names(&h), ["options"]);
}

#[test]
fn reading_pane_options_has_every_observed_setting() {
    let mut app = sample_app();
    app.run("view.readingPaneOptions", json!({})).unwrap();
    let mut h = harness(app);
    for label in [
        "Mark items as read when viewed in the Reading Pane",
        "Mark item as read when selection changes",
        "Single key reading using space bar",
        "Turn on automatic full-screen reading in portrait orientation",
        "Always preview messages",
    ] {
        assert!(h.query_by_label(label).is_some(), "missing {label}");
    }
    h.get_by_label("Always preview messages").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    assert!(!h.state().s.settings.bool("mail.readingPane.alwaysPreview"));
}

#[test]
fn color_categories_lists_names_alphabetically() {
    let mut app = sample_app();
    select_first(&mut app);
    app.run("mail.allCategories", json!({})).unwrap();
    let h = harness(app);
    let mut names: Vec<String> = h.state().s.mailbox.categories.iter().map(|c| c.name.clone()).collect();
    names.sort_by_key(|n| n.to_lowercase());
    let tops: Vec<f32> = names
        .iter()
        .map(|n| h.get_by_role_and_label(egui::accesskit::Role::Button, n).rect().min.y)
        .collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]), "rows not in name order: {names:?} {tops:?}");
}

#[test]
fn read_window_backstage_acts_on_its_own_message() {
    let mut app = sample_app();
    let (with_att, file) = app
        .s
        .mailbox
        .mail
        .values()
        .find_map(|m| m.attachments.first().map(|a| (m.id, a.filename.clone())))
        .expect("sample has an attachment");
    let other = app.s.mailbox.mail.values().find(|m| !m.has_attachments()).map(|m| m.id).unwrap();
    app.s.selection.items = vec![other];
    app.s.selection.focus = Some(other);
    // Alt, F in a read window opens its backstage; the window does not host it, so the main window shows it.
    app.ui.backstage = Some((Surface::Read(with_att.0), "saveAttachments".into()));
    let h = harness(app);
    assert!(h.query_by_label(&format!("•  {file}")).is_some(), "lists the read window's attachment");
    assert!(h.query_by_label("The selected message has no attachments.").is_none());
}

/// Conversation Settings shows the folder's settings as the engine keeps them; OK without changes changes
/// nothing, and a change is applied (also while turning the conversation view off).
#[test]
fn conversation_settings_dialog_follows_the_engine() {
    use mailcraft_engine::queries::{conversation_settings, view_settings};
    let mut app = sample_app();
    app.run("view.conversations", json!({"on": true}));
    let folder = app.s.selection.folder;
    let before = conversation_settings(&app.s, folder);
    assert!(before.senders_above_subject, "engine default");
    crate::dialogs::open(&mut app, "conversationSettings", json!({}));
    let mut h = harness(app);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    assert_eq!(conversation_settings(&h.state().s, folder), before, "OK without changes changes nothing");
    assert!(h.state().ui.toast.is_none(), "{:?}", h.state().ui.toast);
    // Tick Classic Indented View and turn conversations off in one go.
    crate::dialogs::open(h.state_mut(), "conversationSettings", json!({}));
    step(&mut h);
    h.get_by_label("Use Classic Indented View").click();
    step(&mut h);
    h.get_by_label("Show as Conversations").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    let s = &h.state().s;
    assert!(conversation_settings(s, folder).classic_indent);
    assert!(!view_settings(s, folder).conversations);
    assert!(h.state().ui.toast.is_none(), "{:?}", h.state().ui.toast);
}

#[test]
fn ignore_prompt_offers_dont_ask_again() {
    let mut app = sample_app();
    let item = select_first(&mut app);
    app.run("mail.ignore", json!({"ids": [item]}));
    crate::dialogs::take_notices(&mut app);
    let mut h = harness(app);
    assert_eq!(names(&h), ["confirm"]);
    h.get_by_label("Don't ask me again").click();
    step(&mut h);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Ignore Conversation").click();
    step(&mut h);
    let app = h.state();
    assert!(app.s.mailbox.mail[&ItemId(item)].ignored);
    assert!(app.s.settings.bool(mailcraft_engine::cmd::mail::IGNORE_DONT_ASK));
}

#[test]
fn remove_data_file_asks_then_keeps_it_for_open_data_file() {
    let mut app = sample_app();
    app.run("file.newDataFile", json!({"name": "Old Mail"}));
    let acct = app.s.mailbox.accounts.iter().find(|a| a.display_name == "Old Mail").map(|a| a.id.0).unwrap();
    app.run("file.closeDataFile", json!({"store": acct}));
    crate::dialogs::take_notices(&mut app);
    let mut h = harness(app);
    assert_eq!(names(&h), ["confirm"]);
    assert!(h.state().s.mailbox.accounts.iter().any(|a| a.id.0 == acct), "nothing removed before Yes");
    h.get_by_label("Yes").click();
    step(&mut h);
    assert!(!h.state().s.mailbox.accounts.iter().any(|a| a.display_name == "Old Mail"));
    // File › Open & Export › Open Data File lists it; opening brings it back.
    h.state_mut().run("file.openDataFile", json!({}));
    crate::dialogs::take_notices(h.state_mut());
    step(&mut h);
    assert_eq!(names(&h), ["openDataFile"]);
    h.get_by_label_contains("Old Mail").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty(), "{:?}", h.state().ui.toast);
    assert!(h.state().s.mailbox.accounts.iter().any(|a| a.display_name == "Old Mail"));
}

#[test]
fn data_file_properties_renames_the_data_file() {
    let mut app = sample_app();
    let acct = app.s.mailbox.accounts[0].id.0;
    app.run("file.dataFileProperties", json!({"store": acct}));
    crate::dialogs::take_notices(&mut app);
    let mut h = harness(app);
    assert_eq!(names(&h), ["dataFileProperties"]);
    assert!(h.query_all_by_label_contains("items in").next().is_some());
    let field = h.get_by_role(egui::accesskit::Role::TextInput);
    field.focus();
    step(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("Renamed");
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    assert_eq!(h.state().s.mailbox.accounts[0].display_name, "Renamed");
}

#[test]
fn new_folder_from_the_account_root_goes_to_the_top_level() {
    let mut app = sample_app();
    let acct = app.s.mailbox.accounts[0].id;
    app.run("folder.new", json!({"account": acct.0}));
    crate::dialogs::take_notices(&mut app);
    let mut h = harness(app);
    assert_eq!(names(&h), ["createFolder"]);
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("Top");
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    let s = &h.state().s;
    let f = s.mailbox.folders.values().find(|f| f.name == "Top").expect("created");
    assert_eq!(f.parent, None);
    assert_eq!(f.account, acct);
}
