//! Rules, alerts and Quick Steps dialog tests (egui_kittest: accessibility tree + simulated input).

use egui::Key;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::{Value, json};

use super::{Draft, Sec};
use crate::App;
use mailcraft_engine::sample::sample_session;
use mailcraft_model::{
    FolderId, Importance, ItemId, QuickStepAction, RuleAction, RuleCondition, Sensitivity,
};

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

fn sample_app() -> App {
    App::new(sample_session())
}

fn names(h: &Harness<'_, App>) -> Vec<String> {
    h.state().ui.dialogs.iter().map(|(n, _)| n.clone()).collect()
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

fn inbox_items(app: &App) -> Vec<u64> {
    let folder = app.s.selection.folder;
    let mut ids: Vec<u64> = app.s.mailbox.items_in(folder).iter().map(|m| m.id.0).collect();
    ids.sort_unstable();
    ids
}

/// A scratch directory under the cargo target dir (never the system temp dir).
fn scratch_dir(name: &str) -> std::path::PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|e| e.ancestors().nth(3).map(|t| t.join("tmp")))
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp"));
    let dir = base.join(format!("mc-rules-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------------------------------------
// The checklist catalog covers every condition and action

fn all_conditions() -> Vec<RuleCondition> {
    vec![
        RuleCondition::SubjectContains(vec!["a".into()]),
        RuleCondition::From(vec!["b@example.org".into()]),
        RuleCondition::ThroughAccount("me@example.com".into()),
        RuleCondition::SentOnlyToMe,
        RuleCondition::MyNameInTo,
        RuleCondition::MarkedImportance(Importance::Low),
        RuleCondition::MarkedSensitivity(Sensitivity::Confidential),
        RuleCondition::FlaggedForAction,
        RuleCondition::MyNameInCc,
        RuleCondition::MyNameInToOrCc,
        RuleCondition::MyNameNotInTo,
        RuleCondition::SentTo(vec!["c@example.org".into()]),
        RuleCondition::BodyContains(vec!["d".into()]),
        RuleCondition::SubjectOrBodyContains(vec!["e".into()]),
        RuleCondition::HeaderContains(vec!["f".into()]),
        RuleCondition::RecipientAddressContains(vec!["g".into()]),
        RuleCondition::SenderAddressContains(vec!["h".into()]),
        RuleCondition::AssignedToCategory("Red category".into()),
        RuleCondition::HasAttachment,
        RuleCondition::SizeBetween(10, 20),
        RuleCondition::AutomaticReply,
        RuleCondition::MeetingInvitationOrUpdate,
        RuleCondition::UsesForm("Meeting Request".into()),
        RuleCondition::FormType("IPM.Schedule".into()),
        RuleCondition::FromRssFeedWithText(vec!["news".into()]),
        RuleCondition::FromAnyRssFeed,
        RuleCondition::SuspectedJunk,
        RuleCondition::SenderInContacts,
        RuleCondition::OnThisComputerOnly,
    ]
}

fn receive_actions() -> Vec<RuleAction> {
    vec![
        RuleAction::NewItemAlert("hi".into()),
        RuleAction::MoveTo(FolderId(1)),
        RuleAction::AssignCategory("Red category".into()),
        RuleAction::Delete,
        RuleAction::PermanentlyDelete,
        RuleAction::CopyTo(FolderId(2)),
        RuleAction::ForwardTo(vec!["a@example.org".into()]),
        RuleAction::ForwardAsAttachment(vec!["b@example.org".into()]),
        RuleAction::RedirectTo(vec!["c@example.org".into()]),
        RuleAction::ReplyWithTemplate("/t/x.eml".into()),
        RuleAction::FlagForFollowUp("Reply".into(), 3),
        RuleAction::ClearFlag,
        RuleAction::ClearCategories,
        RuleAction::MarkImportance(Importance::Low),
        RuleAction::Print,
        RuleAction::PlaySound("/s/ding.wav".into()),
        RuleAction::MarkAsRead,
        RuleAction::DesktopAlert,
        RuleAction::MarkSensitivity(Sensitivity::Personal),
        RuleAction::ServerReply("Thanks".into()),
        RuleAction::StopProcessingMoreRules,
    ]
}

#[test]
fn every_condition_and_action_round_trips_through_the_checklists() {
    let app = sample_app();
    let conds = all_conditions();
    let acts = receive_actions();
    let d = Draft::from_rule(&app, &conds, &acts, &conds);
    let back = d.conditions(&app, Sec::Cond).unwrap();
    let want: Vec<RuleCondition> = conds.clone();
    assert_eq!(back.len(), want.len());
    for c in &want {
        assert!(back.contains(c), "{c:?} lost");
    }
    assert_eq!(d.conditions(&app, Sec::Exc).unwrap().len(), conds.len());
    let back = d.actions().unwrap();
    for a in &acts {
        assert!(back.contains(a), "{a:?} lost");
    }
    assert_eq!(back.last(), Some(&RuleAction::StopProcessingMoreRules), "stop reads last");
    // Date spans keep their days.
    let span = RuleCondition::ReceivedBetween(1_790_000_000, 1_790_500_000);
    let d = Draft::from_rule(&app, std::slice::from_ref(&span), &[RuleAction::MarkAsRead], &[]);
    assert!(matches!(d.conditions(&app, Sec::Cond).unwrap()[..], [RuleCondition::ReceivedBetween(..)]));
}

#[test]
fn send_rules_keep_their_marker_and_send_actions() {
    let app = sample_app();
    let conds = vec![RuleCondition::AfterSending, RuleCondition::SubjectContains(vec!["x".into()])];
    let acts = vec![
        RuleAction::DeferDelivery(15),
        RuleAction::CcTo(vec!["boss@example.org".into()]),
        RuleAction::NotifyWhenRead,
        RuleAction::NotifyWhenDelivered,
        RuleAction::AssignCategory("Red category".into()),
    ];
    let d = Draft::from_rule(&app, &conds, &acts, &[]);
    assert!(d.outgoing);
    assert_eq!(d.conditions(&app, Sec::Cond).unwrap(), conds);
    let back = d.actions().unwrap();
    assert_eq!(back.len(), acts.len());
    assert!(acts.iter().all(|a| back.contains(a)), "{back:?}");
    // Receive-only conditions ticked in a send rule are dropped, not saved.
    let d = Draft::from_rule(&app, &[RuleCondition::AfterSending, RuleCondition::SentOnlyToMe], &acts, &[]);
    assert_eq!(d.conditions(&app, Sec::Cond).unwrap(), vec![RuleCondition::AfterSending]);
}

// ---------------------------------------------------------------------------------------------------------
// Rules Wizard

#[test]
fn wizard_templates_page_lists_the_groups() {
    let mut app = sample_app();
    app.run("rules.new", json!({})).unwrap();
    let h = harness(app);
    for label in [
        "Stay Organized",
        "Stay Up to Date",
        "Start from a blank rule",
        "Move RSS items from a specific RSS Feed to a folder",
        "Apply rule on messages I receive",
        "Apply rule on messages I send",
    ] {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    let mobile = h.get_by_label("Send an alert to my mobile device when I get messages from someone");
    assert!(mobile.accesskit_node().is_disabled());
}

#[test]
fn wizard_builds_a_rule_for_sent_messages() {
    let mut app = sample_app();
    app.run("rules.new", json!({})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Apply rule on messages I send").click();
    step(&mut h);
    assert!(h.query_by_label("Apply this rule after I send the message").is_some());
    h.get_by_label("Next >").click();
    step(&mut h);
    assert!(h.query_by_label("Which condition(s) do you want to check?").is_some());
    assert!(h.query_by_label("sent only to me").is_none(), "receive-only conditions are hidden");
    h.get_by_label("Next >").click();
    step(&mut h);
    assert!(h.query_by_label("What do you want to do with the message?").is_some());
    assert!(h.query_by_label("print it").is_none(), "receive-only actions are hidden");
    h.query_all_by_label("a number of").next().unwrap().click();
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
    // Deferral is done by this app: the wizard says so.
    assert_eq!(names(&h), ["message"]);
    let rule = h.state().s.mailbox.rules.last().unwrap().clone();
    assert!(rule.is_outgoing());
    assert_eq!(rule.actions, vec![RuleAction::DeferDelivery(5)]);
}

#[test]
fn wizard_new_condition_values_are_editable() {
    let mut app = sample_app();
    app.run("rules.new", json!({})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Apply rule on messages I receive").click();
    step(&mut h);
    h.get_by_label("Next >").click();
    step(&mut h);
    h.query_all_by_label("form name").next().unwrap().scroll_to_me();
    step(&mut h);
    h.query_all_by_label("form name").next().unwrap().click();
    step(&mut h);
    h.get_by_label("Done").click();
    step(&mut h);
    h.get_by_label("Next >").click();
    step(&mut h);
    h.query_all_by_label("a specific message").nth(1).unwrap().scroll_to_me();
    step(&mut h);
    h.query_all_by_label("a specific message").nth(1).unwrap().click();
    step(&mut h);
    h.get_by_role(Role::MultilineTextInput).focus();
    step(&mut h);
    h.get_by_role(Role::MultilineTextInput).type_text("Got it, thanks.");
    step(&mut h);
    h.get_by_label("Done").click();
    step(&mut h);
    h.get_by_label("Next >").click();
    step(&mut h);
    h.get_by_label("Next >").click();
    step(&mut h);
    h.get_by_label("Finish").click();
    step(&mut h);
    let rule = h.state().s.mailbox.rules.last().unwrap().clone();
    assert_eq!(rule.conditions, vec![RuleCondition::UsesForm("Message".into())]);
    assert_eq!(rule.actions, vec![RuleAction::ServerReply("Got it, thanks.".into())]);
    assert!(!rule.is_outgoing());
}

// ---------------------------------------------------------------------------------------------------------
// Rules and Alerts

fn app_with_rule(actions: Value) -> (App, u64) {
    let mut app = sample_app();
    let r = app
        .run(
            "rules.create",
            json!({"name": "Budget", "conditions": [{"SubjectContains": ["budget"]}], "actions": actions}),
        )
        .unwrap();
    (app, r["id"].as_u64().unwrap())
}

#[test]
fn change_rule_menu_adds_quick_actions() {
    let (mut app, id) = app_with_rule(json!(["MarkAsRead"]));
    app.run("rules.manage", json!({})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Change Rule").click();
    step(&mut h);
    h.get_by_label("Mark as High Priority").click();
    step(&mut h);
    let rule = h.state().s.mailbox.rules.iter().find(|r| r.id == id).unwrap().clone();
    assert_eq!(rule.actions, vec![RuleAction::MarkAsRead, RuleAction::MarkImportance(Importance::High)]);
    h.get_by_label("Change Rule").click();
    step(&mut h);
    h.get_by_label("Mark as Low Priority").click();
    step(&mut h);
    let rule = h.state().s.mailbox.rules.iter().find(|r| r.id == id).unwrap().clone();
    assert_eq!(rule.actions, vec![RuleAction::MarkAsRead, RuleAction::MarkImportance(Importance::Low)]);
}

#[test]
fn copy_rule_to_another_account_or_here() {
    let (mut app, id) = app_with_rule(json!(["MarkAsRead"]));
    let other = app.s.mailbox.add_account(mailcraft_model::Account {
        kind: mailcraft_model::AccountKind::Imap,
        display_name: "Work".into(),
        address: "work@example.com".into(),
        ..Default::default()
    });
    app.s.mailbox.create_default_folders(other);
    app.run("rules.manage", json!({})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Copy...").click();
    step(&mut h);
    assert_eq!(names(&h), ["rulesAndAlerts", "copyRule"]);
    h.get_by_label_contains("work@example.com").click();
    step(&mut h);
    h.query_all_by_label("OK").last().unwrap().click();
    step(&mut h);
    assert_eq!(names(&h), ["rulesAndAlerts"]);
    let copy = h.state().s.mailbox.rules.last().unwrap().clone();
    assert_ne!(copy.id, id);
    assert_eq!(copy.account(), Some("work@example.com"));
    let n = h.state().s.mailbox.rules.len();
    h.get_by_label("Copy...").click();
    step(&mut h);
    h.get_by_label("Duplicate Here").click();
    step(&mut h);
    assert_eq!(h.state().s.mailbox.rules.len(), n + 1);
}

#[test]
fn manage_alerts_tab_lists_alert_rules() {
    let (mut app, _) = app_with_rule(json!([{"NewItemAlert": ""}, {"PlaySound": "/s/ding.wav"}]));
    app.run("rules.create", json!({"name": "Quiet", "actions": ["MarkAsRead"]})).unwrap();
    app.run("rules.manage", json!({})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Manage Alerts").click();
    step(&mut h);
    assert!(h.query_by_label_contains("Budget  —  New Item Alert window, Sound").is_some());
    assert!(h.query_by_label_contains("Quiet").is_none());
    h.get_by_label("New Alert...").click();
    step(&mut h);
    assert_eq!(names(&h), ["rulesAndAlerts", "rulesWizard"]);
    assert!(
        h.query_by_label("display a specific message in the New Item Alert window").is_some()
            || h.query_by_label("a specific message").is_some()
    );
}

#[test]
fn options_export_and_import_mcrules() {
    let dir = scratch_dir("options");
    let (mut app, _) = app_with_rule(json!(["MarkAsRead"]));
    app.run("rules.manage", json!({})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Options").click();
    step(&mut h);
    let path = dir.join("backup");
    h.get_by_role(Role::TextInput).focus();
    step(&mut h);
    h.get_by_role(Role::TextInput).type_text(path.to_str().unwrap());
    step(&mut h);
    h.get_by_label("Export Rules...").click();
    step(&mut h);
    assert!(h.query_by_label("Exported 1 rule(s).").is_some());
    assert!(dir.join("backup.mcrules").is_file());
    h.get_by_label("Import Rules...").click();
    step(&mut h);
    assert!(h.query_by_label("Imported 1 rule(s).").is_some());
    assert_eq!(h.state().s.mailbox.rules.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_rules_now_on_unread_messages() {
    let (mut app, _) = app_with_rule(json!([{"AssignCategory": "Red category"}]));
    app.run("rules.create", json!({"name": "Everything", "actions": [{"AssignCategory": "Rules test"}]}))
        .unwrap();
    let ids = inbox_items(&app);
    let first = *ids.first().unwrap();
    app.s.mailbox.set_read(&[ItemId(first)], true);
    let unread = ids.iter().filter(|i| !app.s.mailbox.mail.get(ItemId(**i)).unwrap().read).count();
    let everything = app.s.mailbox.rules.len() - 1;
    app.run("folder.runRulesNow", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["runRulesNow"]);
    // Tick "Everything" (the last rule) only.
    h.query_all_by_role(Role::CheckBox).nth(everything).unwrap().click();
    step(&mut h);
    h.get_by_role(Role::ComboBox).click();
    step(&mut h);
    h.get_by_label("Unread Messages").click();
    step(&mut h);
    h.get_by_label("Run Now").click();
    step(&mut h);
    assert!(h.query_by_label(&format!("Done. The rules acted on {unread} message(s).")).is_some());
    let mb = &h.state().s.mailbox;
    assert!(
        mb.mail.get(ItemId(first)).unwrap().categories.iter().all(|c| c != "Rules test"),
        "read message skipped"
    );
    let blue = mb.mail.items().filter(|m| m.categories.iter().any(|c| c == "Rules test")).count();
    assert_eq!(blue, unread);
}

// ---------------------------------------------------------------------------------------------------------
// New Item Alert window and sounds

#[test]
fn new_item_alert_window_collects_runs_and_opens_items() {
    let mut app = sample_app();
    let ids = inbox_items(&app);
    app.run(
        "rules.create",
        json!({"name": "Watch", "actions": [{"NewItemAlert": "Look"}, {"PlaySound": "/s/ding.wav"}]}),
    )
    .unwrap();
    let first = ids[0];
    let second = ids[1];
    app.run("rules.applyToNewMail", json!({"ids": [first]})).unwrap();
    app.run("rules.applyToNewMail", json!({"ids": [second]})).unwrap();
    let mut h = harness(app);
    step(&mut h);
    // Two runs, one window; the sound "dialog" played (once for the frame) and closed at once.
    assert_eq!(names(&h), ["newItemAlerts"]);
    assert_eq!(crate::dialogs::alerts::played_sounds(&h.ctx), vec!["/s/ding.wav".to_owned()]);
    let subject =
        |id: u64, h: &Harness<'_, App>| h.state().s.mailbox.mail.get(ItemId(id)).unwrap().subject.clone();
    let (s1, s2) = (subject(first, &h), subject(second, &h));
    assert!(h.query_by_label("From").is_some() && h.query_by_label("Received").is_some());
    assert!(h.query_all_by_label(&s1).next().is_some());
    assert!(h.query_all_by_label(&s2).next().is_some());
    assert!(h.get_by_label("Open Item").accesskit_node().is_disabled());
    h.query_all_by_label(&s1).next().unwrap().click();
    step(&mut h);
    assert!(h.query_by_label("Look").is_some(), "the rule's message shows for the selected line");
    h.get_by_label("Open Item").click();
    step(&mut h);
    assert!(h.state().ui.read_windows.contains(&first));
    assert!(names(&h).is_empty());
    // Lines stay for the next alert; Close empties them.
    h.state_mut().ui.dialogs.push(("newItemAlerts".into(), json!({"alerts": []})));
    step(&mut h);
    assert!(h.query_all_by_label(&s2).next().is_some());
    h.get_by_label("Close").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    h.state_mut().ui.dialogs.push(("newItemAlerts".into(), json!({"alerts": []})));
    step(&mut h);
    assert!(h.query_all_by_label(&s2).next().is_none());
    h.key_press(Key::Escape);
    step(&mut h);
    assert!(names(&h).is_empty());
}

// ---------------------------------------------------------------------------------------------------------
// Quick Steps

#[test]
fn first_use_setup_names_the_step_after_the_folder() {
    let mut app = sample_app();
    let target = projects(&app);
    let item = inbox_items(&app)[0];
    app.run("nav.selectItem", json!({"id": item})).unwrap();
    app.run("quicksteps.reset", json!({})).unwrap();
    app.run("quicksteps.run", json!({"id": "Move to: ?"})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["quickStepFirstUse"]);
    assert!(h.query_by_label("First Time Setup").is_some());
    assert!(h.get_by_label("Save").accesskit_node().is_disabled(), "needs a folder first");
    h.get_by_label("Choose Folder...").click();
    step(&mut h);
    h.get_by_label("Projects").click();
    step(&mut h);
    h.key_press(Key::Enter);
    step(&mut h);
    assert_eq!(names(&h), ["quickStepFirstUse"]);
    h.get_by_label("Save").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    let st = h.state();
    let q = &st.s.mailbox.quick_steps[0];
    assert_eq!(q.name, "Projects");
    assert_eq!(q.actions[0], QuickStepAction::MoveTo(Some(FolderId(target))));
    assert_eq!(st.s.mailbox.mail.get(ItemId(item)).unwrap().folder, FolderId(target));
}

#[test]
fn edit_quick_step_from_a_template_with_reordered_actions() {
    let mut app = sample_app();
    let target = projects(&app);
    app.run("quicksteps.manage", json!({})).unwrap();
    let mut h = harness(app);
    h.get_by_label("New").click();
    step(&mut h);
    h.get_by_label("Flag & Move...").click();
    step(&mut h);
    assert_eq!(names(&h), ["manageQuickSteps", "editQuickStep"]);
    // The template's name and actions are filled in.
    assert_eq!(h.query_all_by_role(Role::TextInput).next().unwrap().value().as_deref(), Some("Flag & Move"));
    h.get_by_label("Folder...").click();
    step(&mut h);
    h.get_by_label("Projects").click();
    step(&mut h);
    h.key_press(Key::Enter);
    step(&mut h);
    // Move the folder action above the flag.
    h.query_all_by_label("▲").last().unwrap().click();
    step(&mut h);
    h.query_all_by_role(Role::ComboBox).last().unwrap().click();
    step(&mut h);
    h.get_by_label("Ctrl+Shift+4").click();
    step(&mut h);
    h.get_by_label("Finish").click();
    step(&mut h);
    assert_eq!(names(&h), ["manageQuickSteps"]);
    let q = h.state().s.mailbox.quick_steps.last().unwrap().clone();
    assert_eq!(q.name, "Flag & Move");
    assert_eq!(
        q.actions,
        vec![QuickStepAction::MoveTo(Some(FolderId(target))), QuickStepAction::Flag("today".into())]
    );
    assert_eq!(q.shortcut, "Ctrl+Shift+4");
    // The Description pane shows it.
    h.get_by_label("Flag & Move").click();
    step(&mut h);
    assert!(h.query_by_label("  Flag message: Today").is_some());
    assert!(h.query_by_label("  Ctrl+Shift+4").is_some());
}

#[test]
fn manage_quick_steps_duplicate_delete_reset() {
    let mut app = sample_app();
    app.run("quicksteps.reset", json!({})).unwrap();
    app.run("quicksteps.manage", json!({})).unwrap();
    let mut h = harness(app);
    let n = h.state().s.mailbox.quick_steps.len();
    h.get_by_label("Duplicate").click();
    step(&mut h);
    assert_eq!(h.state().s.mailbox.quick_steps.len(), n + 1);
    assert!(h.query_all_by_label("Copy of Move to: ?").next().is_some());
    h.get_by_label("Delete").click();
    step(&mut h);
    assert_eq!(names(&h), ["manageQuickSteps", "confirm"]);
    h.get_by_label("Yes").click();
    step(&mut h);
    assert_eq!(h.state().s.mailbox.quick_steps.len(), n);
    h.get_by_label("▼").click();
    step(&mut h);
    assert_eq!(h.state().s.mailbox.quick_steps[1].name, "Move to: ?");
    h.get_by_label("Reset to Defaults").click();
    step(&mut h);
    h.get_by_label("Yes").click();
    step(&mut h);
    assert_eq!(h.state().s.mailbox.quick_steps, mailcraft_model::QuickStep::defaults());
    h.get_by_label("Edit").click();
    step(&mut h);
    assert_eq!(names(&h), ["manageQuickSteps", "editQuickStep"]);
    h.key_press(Key::Escape);
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
}
