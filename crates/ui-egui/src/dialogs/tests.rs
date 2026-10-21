//! Dialog and backstage tests driven through egui_kittest (accessibility tree + simulated input).

use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
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

fn new_compose(app: &mut App) -> u64 {
    app.run("mail.newEmail", json!({})).and_then(|v| v.get("compose").and_then(Value::as_u64)).unwrap()
}

#[test]
fn custom_flag_for_a_new_message_flags_the_recipients_and_clears() {
    let mut app = sample_app();
    let c = new_compose(&mut app);
    app.run("compose.followUp", json!({"compose": c, "kind": "custom"})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["customFlag"]);
    // Nothing is flagged yet: Clear Flag is greyed out.
    assert!(h.get_by_label("Clear Flag").accesskit_node().is_disabled());
    h.get_by_label("Flag for Recipients").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty(), "{:?}", names(&h));
    let cs = h.state().s.composes[&c].clone();
    assert!(matches!(cs.flag, FlagState::Flagged { .. }));
    assert_eq!(cs.recipient_flag.as_ref().map(|r| r.label.as_str()), Some("Follow up"));
    // Reopened, the dialog shows both and Clear Flag clears both.
    h.state_mut().run("compose.followUp", json!({"compose": c, "kind": "custom"}));
    step(&mut h);
    assert!(!h.get_by_label("Clear Flag").accesskit_node().is_disabled());
    h.get_by_label("Clear Flag").click();
    step(&mut h);
    let cs = h.state().s.composes[&c].clone();
    assert_eq!(cs.flag, FlagState::None);
    assert!(cs.recipient_flag.is_none());
}

#[test]
fn properties_dialog_sets_security_expiry_and_archive() {
    let mut app = sample_app();
    let c = new_compose(&mut app);
    app.run("compose.messageOptions", json!({"compose": c})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["messageOptions"]);
    for label in [
        super::mailopts::PROPERTIES_ENCRYPT,
        super::mailopts::PROPERTIES_SIGN,
        "Expires after",
        "Do not AutoArchive this item",
    ] {
        h.get_by_label(label).click();
        step(&mut h);
    }
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty(), "{:?}", names(&h));
    let cs = h.state().s.composes[&c].clone();
    assert!(cs.encrypt && cs.sign && cs.no_auto_archive);
    assert!(cs.expires.is_some());
}

#[test]
fn properties_select_names_fills_the_reply_address() {
    let mut app = sample_app();
    let c = new_compose(&mut app);
    app.run("compose.messageOptions", json!({"compose": c})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Have replies sent to").click();
    step(&mut h);
    h.get_by_label("Select Names...").click();
    step(&mut h);
    assert_eq!(names(&h), ["messageOptions", "addressBook"]);
    // The address book's one box (last text box) takes the address; OK hands it back.
    let boxes = h.query_all_by_role(egui::accesskit::Role::TextInput).count();
    h.query_all_by_role(egui::accesskit::Role::TextInput).nth(boxes - 1).unwrap().focus();
    step(&mut h);
    h.query_all_by_role(egui::accesskit::Role::TextInput)
        .nth(boxes - 1)
        .unwrap()
        .type_text("reply@example.com");
    step(&mut h);
    h.query_all_by_label("OK").last().unwrap().click();
    step(&mut h);
    assert_eq!(names(&h), ["messageOptions"]);
    h.get_by_label("OK").click();
    step(&mut h);
    assert_eq!(h.state().s.composes[&c].reply_to, "reply@example.com");
}

#[test]
fn send_a_calendar_inserts_the_chosen_calendar_with_an_ics() {
    let mut app = sample_app();
    let c = new_compose(&mut app);
    app.run("insert.calendar", json!({"compose": c})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["insertCalendar"]);
    h.get_by_label(super::compose_dlg::SEND_CALENDAR_WORKING_HOURS).click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty(), "{:?}", names(&h));
    let cs = h.state().s.composes[&c].clone();
    assert!(cs.body.to_plain_text().contains("Working hours only"), "{}", cs.body.to_plain_text());
    assert!(cs.attachments.iter().any(|a| a.filename.ends_with(".ics")));
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

fn module_folder(app: &mut App, kind: mailcraft_model::FolderKind) -> FolderId {
    let f = app.s.mailbox.folders.values().find(|f| f.kind == kind).map(|f| f.id).unwrap();
    app.run("nav.selectFolder", json!({"folder": f.0})).unwrap();
    f
}

#[test]
fn module_show_columns_adds_and_orders_columns() {
    let mut app = sample_app();
    let folder = module_folder(&mut app, mailcraft_model::FolderKind::Contacts);
    app.run("view.changeView", json!({"name": "Phone"})).unwrap();
    app.run("view.addColumns", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["showColumns"]);
    h.get_by_label("Department").click();
    step(&mut h);
    h.get_by_label("Add ->").click();
    step(&mut h);
    h.get_by_label("Move Up").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    let cols: Vec<&str> =
        mailcraft_engine::cmd::view::table::columns(&h.state().s, folder).iter().map(|c| c.id).collect();
    let n = cols.len();
    assert_eq!(cols.get(n - 2), Some(&"department"), "{cols:?}");
    assert_eq!(cols.last(), Some(&"flagStatus"));
}

#[test]
fn module_format_columns_aligns_a_task_column() {
    use mailcraft_engine::cmd::view::table;
    let mut app = sample_app();
    let folder = module_folder(&mut app, mailcraft_model::FolderKind::Tasks);
    app.run("view.changeView", json!({"name": "Detailed"})).unwrap();
    app.run("view.formatColumns", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["viewFormat"]);
    let first = table::shown(&h.state().s, folder).first().map(|c| c.col.id).unwrap();
    h.get_by_label("Right").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    let col = table::shown(&h.state().s, folder).into_iter().find(|c| c.col.id == first).unwrap();
    assert_eq!(col.align, "right");
}

#[test]
fn module_manage_views_creates_renames_and_deletes() {
    use mailcraft_engine::cmd::view::modules;
    let mut app = sample_app();
    let folder = module_folder(&mut app, mailcraft_model::FolderKind::Tasks);
    app.run("view.manageViews", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["manageViews"]);
    h.get_by_label("New...").click();
    step(&mut h);
    h.get_by_label("Name of new view:").focus();
    step(&mut h);
    h.get_by_label("Name of new view:").type_text("Errands");
    step(&mut h);
    h.query_all_by_label("All Task folders").last().unwrap().click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    let saved = modules::saved_views(&h.state().s, folder);
    assert_eq!(
        saved.iter().map(|v| (v.name.as_str(), v.everywhere)).collect::<Vec<_>>(),
        [("Errands", true)]
    );
    // "Only show views created for this folder" hides the built-in and module-wide views.
    assert!(h.query_by_label("Detailed").is_some());
    h.get_by_label("Only show views created for this folder").click();
    step(&mut h);
    assert!(h.query_by_label("Detailed").is_none());
    assert!(h.query_by_label("Errands").is_none());
    h.get_by_label("Only show views created for this folder").click();
    step(&mut h);
    h.get_by_label("Rename...").click();
    step(&mut h);
    let field = h.get_by_label("Name of new view:");
    field.focus();
    step(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
    h.get_by_label("Name of new view:").type_text("Chores");
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    let saved = modules::saved_views(&h.state().s, folder);
    assert_eq!(saved.first().map(|v| v.name.as_str()), Some("Chores"));
    h.get_by_label("Apply View").click();
    step(&mut h);
    assert_eq!(modules::current_view_name(&h.state().s, folder), "Chores");
    h.state_mut().run("view.manageViews", json!({}));
    step(&mut h);
    h.get_by_label("Delete").click();
    step(&mut h);
    assert!(modules::saved_views(&h.state().s, folder).is_empty());
}

#[test]
fn module_manage_views_describes_the_selected_view_and_has_ok() {
    let mut app = sample_app();
    module_folder(&mut app, mailcraft_model::FolderKind::Tasks);
    app.run("view.changeView", json!({"name": "Simple List"})).unwrap();
    app.run("view.columns", json!({"columns": ["subject", "priority"]})).unwrap();
    app.run("view.manageViews", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["manageViews"]);
    // The Description shows the selected view's settings, and follows the selection.
    assert!(h.query_by_label("Type of view: Simple List").is_some());
    assert!(h.query_all_by_label_contains("Columns: Subject, Priority").next().is_some());
    assert!(h.query_all_by_label_contains("Filter: Off").next().is_some());
    h.get_by_label("Detailed").click();
    step(&mut h);
    assert!(h.query_by_label("Type of view: Detailed").is_some());
    assert!(h.query_all_by_label_contains("Columns: Subject, Priority").next().is_none());
    // OK, Apply View and Close.
    assert!(h.query_by_label("Apply View").is_some() && h.query_by_label("Close").is_some());
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
}

#[test]
fn contact_color_categories_assign_through_people_categorize() {
    let mut app = sample_app();
    let folder = module_folder(&mut app, mailcraft_model::FolderKind::Contacts);
    let id = app.s.mailbox.contacts.values().find(|c| c.folder == folder).map(|c| c.id).unwrap();
    app.s.selection.items = vec![id];
    app.s.selection.focus = Some(id);
    app.run("people.allCategories", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["colorCategories"]);
    // The full dialog: New, Rename, Delete, Color and Shortcut Key.
    for label in ["New...", "Rename", "Delete", "Color:", "Shortcut Key:"] {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    h.get_by_label("New...").click();
    step(&mut h);
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("Neighbours");
    step(&mut h);
    h.key_press(Key::Enter);
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    assert!(h.state().s.mailbox.contacts[&id].categories.iter().any(|c| c == "Neighbours"));
}

#[test]
fn new_data_file_asks_for_a_name() {
    let mut app = sample_app();
    let before = app.s.mailbox.accounts.len();
    app.run("file.newDataFile", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["newDataFile"]);
    let field = h.get_by_label("File name:");
    field.focus();
    step(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
    h.get_by_label("File name:").type_text("Receipts archive");
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    assert_eq!(h.state().s.mailbox.accounts.len(), before + 1);
    assert!(h.state().s.mailbox.accounts.iter().any(|a| a.display_name == "Receipts archive"));
}

#[test]
fn stationery_picker_opens_a_message_on_the_chosen_stationery() {
    let mut app = sample_app();
    let before = app.s.composes.len();
    let list = mailcraft_engine::cmd::mail::stationery_list();
    crate::dialogs::open(&mut app, "pickStationery", json!({"stationery": list, "default": "none"}));
    let mut h = harness(app);
    h.get_by_label("Linen").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    let s = &h.state().s;
    assert_eq!(s.composes.len(), before + 1);
    let c = s.composes.values().max_by_key(|c| c.id).unwrap();
    assert_eq!(c.theme.name.as_deref(), Some("Organic"));
    assert!(c.body.background.is_some());
    assert!(!names(&h).iter().any(|n| n == "pickStationery"));
}

#[test]
fn send_receive_groups_dialog_lists_and_adds_groups() {
    let mut app = sample_app();
    let listing = app.s.run("sendReceive.defineGroups", &json!({"action": "list"})).unwrap();
    crate::dialogs::open(&mut app, "sendReceiveGroups", listing);
    let mut h = harness(app);
    assert!(h.query_by_label("All Accounts").is_some());
    assert!(h.query_by_label_contains("Settings for group").is_some());
    h.get_by_label("New...").click();
    step(&mut h);
    assert!(h.query_by_label("Group name:").is_some());
    // A group made through the command shows after the next listing; the editor opens on Edit.
    h.state_mut()
        .s
        .run("sendReceive.defineGroups", &json!({"action": "save", "group": {"name": "Evening"}}))
        .unwrap();
    assert_eq!(mailcraft_engine::sync::groups::groups(&h.state().s).len(), 1);
}

#[test]
fn add_form_field_adds_and_edits_conditions() {
    let mut app = sample_app();
    crate::dialogs::open(&mut app, "addFormField", json!({}));
    let mut h = harness(app);
    assert!(h.query_by_label("Fields from:").is_some());
    step(&mut h);
    // The fields are listed one per line.
    let top = |h: &Harness<'_, App>, l: &str| {
        h.get_all_by_label(l).map(|n| n.rect().min.y).fold(f32::MAX, f32::min)
    };
    assert!(top(&h, "Sensitivity") > top(&h, "Subject"));
    h.get_by_label("Sensitivity").click();
    step(&mut h);
    assert_eq!(h.get_all_by_label("Sensitivity").count(), 2, "the chosen field heads the condition");
    step(&mut h);
    h.get_by_label("Add to Search").click();
    step(&mut h);
    assert_eq!(h.state().s.view.search, "prop:\"Sensitivity=Normal\"");
    assert!(h.query_by_label("Remove").is_some(), "the condition shows as a row");
    h.get_by_label("Remove").click();
    step(&mut h);
    assert!(h.state().s.view.search.is_empty());
}

#[test]
fn training_lessons_page_with_next_and_back() {
    let mut app = sample_app();
    crate::dialogs::open(&mut app, "help", json!({"topic": "training"}));
    let mut h = harness(app);
    assert!(h.query_by_label("Lesson 1 of 8").is_some());
    h.get_by_label("Next").click();
    step(&mut h);
    assert!(h.query_by_label("Lesson 2 of 8").is_some());
    h.get_by_label("Back").click();
    step(&mut h);
    assert!(h.query_by_label("Lesson 1 of 8").is_some());
    for (_, _, _, cmd) in super::general::LESSONS {
        assert!(h.state().s.registry.get(cmd).is_some(), "{cmd}");
    }
}

#[test]
fn recover_deleted_items_purges_selected() {
    let mut app = sample_app();
    let account = app.s.current_account().unwrap();
    let deleted = app.s.mailbox.role_folder(account, mailcraft_model::FolderRole::DeletedItems).unwrap();
    app.s.mailbox.add_mail(mailcraft_model::MailItem {
        folder: deleted,
        subject: "Old memo".into(),
        ..Default::default()
    });
    app.s.run("folder.emptyDeleted", &json!({"folder": deleted.0})).unwrap();
    app.s.run("folder.recoverDeleted", &json!({})).unwrap();
    // The command asks for the dialog with a notice (the shell opens it); open it the same way here.
    let params = app
        .s
        .notices
        .iter()
        .rev()
        .find_map(|n| match n {
            mailcraft_engine::Notice::OpenDialog(d, p) if d == "recoverDeletedItems" => Some(p.clone()),
            _ => None,
        })
        .unwrap();
    crate::dialogs::open(&mut app, "recoverDeletedItems", params);
    let mut h = harness(app);
    assert!(h.query_by_label("Old memo").is_some());
    assert!(h.query_by_label("Purge Selected Items").is_some());
    h.get_by_label("Select All").click();
    step(&mut h);
    h.get_by_label("Purge Selected Items").click();
    step(&mut h);
    assert!(mailcraft_engine::recover::listed(&h.state().s).is_empty());
}

#[test]
fn custom_flag_due_date_from_the_month_picker() {
    let mut app = sample_app();
    let item = select_first(&mut app);
    app.run("mail.flagCustom", json!({})).unwrap();
    let mut h = harness(app);
    // The second ▾ is the due date's calendar.
    h.get_all_by_label("▾").nth(1).unwrap().click();
    step(&mut h);
    h.get_by_label("15").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    match &h.state().s.mailbox.mail.get(&ItemId(item)).unwrap().flag {
        FlagState::Flagged { due: Some(d), .. } => {
            let tz = h.state().s.tz_offset_min;
            assert_eq!(mailcraft_model::dates::LocalTime::of(*d, tz).day, 15);
        }
        other => panic!("no due date: {other:?}"),
    }
}
