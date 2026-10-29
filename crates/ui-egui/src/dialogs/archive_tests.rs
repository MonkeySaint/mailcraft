//! AutoArchive dialog tests (egui_kittest): each dialog saves through its engine command, the Options page
//! opens the AutoArchive dialog, and the scheduler asks before running when told to.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use super::*;
use mailcraft_engine::sample::sample_session;
use mailcraft_model::folder::ArchiveMode;
use mailcraft_model::{FolderRole, ItemId, MailItem};

fn harness(app: App) -> Harness<'static, App> {
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).build_ui_state(
        |ui, app: &mut App| {
            let ctx = ui.ctx().clone();
            crate::dialogs::auto_archive_tick(&ctx, app);
            crate::backstage::backstage(&ctx, app);
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

fn names(h: &Harness<'_, App>) -> Vec<String> {
    h.state().ui.dialogs.iter().map(|(n, _)| n.clone()).collect()
}

fn inbox(app: &App) -> FolderId {
    let acct = app.s.mailbox.accounts.first().map(|a| a.id).unwrap();
    app.s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap()
}

/// A message received `days` days before the sample's "now".
fn old_mail(app: &mut App, days: i64) -> ItemId {
    let folder = inbox(app);
    let received = mailcraft_model::DateTime::from_unix(app.s.now().unix - days * 86_400, 0);
    app.s.mailbox.add_mail(MailItem { folder, subject: "Old news".into(), received, ..Default::default() })
}

fn path_of(app: &App, id: ItemId) -> String {
    app.s.mailbox.mail.get(id).map(|m| app.s.mailbox.folder_path(m.folder)).unwrap_or_default()
}

#[test]
fn folder_dialog_saves_the_folders_settings() {
    let mut app = App::new(sample_session());
    let folder = inbox(&app);
    app.run("folder.autoArchiveSettings", json!({"folder": folder.0})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["autoArchiveSettings"]);
    h.get_by_label("Archive this folder using these settings:").click();
    step(&mut h);
    h.get_by_label("Permanently delete old items").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    let p = archive::folder_props(&h.state().s.mailbox, folder);
    assert_eq!(p.mode, ArchiveMode::Custom);
    assert_eq!(p.action, ArchiveAction::Delete);
    assert_eq!((p.age, p.unit), (6, AgeUnit::Months));
}

#[test]
fn folder_properties_autoarchive_tab_saves_on_ok() {
    let mut app = App::new(sample_session());
    let folder = inbox(&app);
    app.run("folder.properties", json!({"folder": folder.0})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["folderProperties"]);
    h.get_by_label("AutoArchive").click();
    step(&mut h);
    h.get_by_label("Do not archive items in this folder").click();
    step(&mut h);
    assert_eq!(archive::folder_props(&h.state().s.mailbox, folder).mode, ArchiveMode::Default, "only on OK");
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    assert_eq!(archive::folder_props(&h.state().s.mailbox, folder).mode, ArchiveMode::Never);
}

#[test]
fn options_dialog_saves_the_global_settings() {
    let mut app = App::new(sample_session());
    app.run("options.autoArchive", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["autoArchiveOptions"]);
    h.get_by_label("Run AutoArchive every").click();
    step(&mut h);
    h.get_by_label("Prompt before AutoArchive runs").click();
    h.get_by_label("Delete expired items (email folders only)").click();
    h.get_by_label("Permanently delete old items").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    let d = archive::defaults(&h.state().s);
    assert!(d.run && d.prompt && d.delete_expired);
    assert_eq!(d.action, ArchiveAction::Delete);
    assert!(archive::schedule(&h.state().s).last_run.is_some(), "turning it on starts the clock");
}

#[test]
fn options_window_opens_the_autoarchive_dialog() {
    let mut app = App::new(sample_session());
    app.run("options.open", json!({"page": "advanced"})).unwrap();
    let mut h = harness(app);
    assert!(
        h.query_by_label("Run AutoArchive every number of days").is_none(),
        "in the dialog, not the page"
    );
    assert!(h.query_by_label_contains("Keep folders small").is_some());
    h.get_by_label("AutoArchive Settings...").click();
    step(&mut h);
    assert_eq!(names(&h), ["options", "autoArchiveOptions"]);
}

#[test]
fn archive_dialog_archives_the_folder_tree() {
    let mut app = App::new(sample_session());
    let old = old_mail(&mut app, 400);
    app.run("file.archive", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["archive"]);
    assert!(h.query_by_label("Archives").is_some(), "the default archive folder is named");
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(names(&h).is_empty(), "{:?}", h.state().ui.toast);
    assert_eq!(path_of(h.state(), old), "Archives\\Inbox");
    // One undo step.
    let mut app = h.into_state();
    app.run("app.undo", json!({})).unwrap();
    assert_eq!(path_of(&app, old), "Inbox");
}

#[test]
fn scheduler_prompts_before_running_and_runs_on_yes() {
    let mut app = App::new(sample_session());
    let old = old_mail(&mut app, 400);
    app.run("options.autoArchive", json!({"run": true, "prompt": true, "everyDays": 14})).unwrap();
    // Last run 20 days ago: due.
    let last = app.s.now().unix - 20 * 86_400;
    app.s.settings.set(archive::LAST_RUN_KEY, &json!(last)).unwrap();
    let mut h = harness(app);
    assert!(names(&h).is_empty(), "nothing during start-up");
    store_tick(&h.ctx, Tick { next: Some(0.0), approved: false });
    step(&mut h);
    assert_eq!(names(&h), ["autoArchivePrompt"]);
    assert_eq!(path_of(h.state(), old), "Inbox", "nothing moves before Yes");
    h.get_by_label("Yes").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    assert_eq!(path_of(h.state(), old), "Archives\\Inbox");
    assert!(h.state().s.undo.undo.is_empty(), "a scheduled run records no undo step");
    assert!(!archive::schedule(&h.state().s).due);
}

#[test]
fn scheduler_no_skips_the_run_and_unprompted_runs_go_ahead() {
    let mut app = App::new(sample_session());
    let old = old_mail(&mut app, 400);
    app.run("options.autoArchive", json!({"run": true, "prompt": true})).unwrap();
    let last = app.s.now().unix - 20 * 86_400;
    app.s.settings.set(archive::LAST_RUN_KEY, &json!(last)).unwrap();
    let mut h = harness(app);
    store_tick(&h.ctx, Tick { next: Some(0.0), approved: false });
    step(&mut h);
    h.get_by_label("No").click();
    step(&mut h);
    assert_eq!(path_of(h.state(), old), "Inbox");
    assert!(!archive::schedule(&h.state().s).due, "No waits for the next run");
    // Without the prompt a due run just happens.
    let mut app = h.into_state();
    app.run("options.autoArchive", json!({"prompt": false})).unwrap();
    app.s.settings.set(archive::LAST_RUN_KEY, &json!(last)).unwrap();
    let mut h = harness(app);
    store_tick(&h.ctx, Tick { next: Some(0.0), approved: false });
    step(&mut h);
    assert!(names(&h).is_empty());
    assert_eq!(path_of(h.state(), old), "Archives\\Inbox");
}

#[test]
fn scheduler_escape_postpones_like_no() {
    let mut app = App::new(sample_session());
    let old = old_mail(&mut app, 400);
    app.run("options.autoArchive", json!({"run": true, "prompt": true})).unwrap();
    let last = app.s.now().unix - 20 * 86_400;
    app.s.settings.set(archive::LAST_RUN_KEY, &json!(last)).unwrap();
    let mut h = harness(app);
    store_tick(&h.ctx, Tick { next: Some(0.0), approved: false });
    step(&mut h);
    assert_eq!(names(&h), ["autoArchivePrompt"]);
    h.key_press(egui::Key::Escape);
    step(&mut h);
    assert!(names(&h).is_empty());
    assert_eq!(path_of(h.state(), old), "Inbox");
    assert!(!archive::schedule(&h.state().s).due, "closing the prompt waits for the next run");
    // The next check does not ask again.
    store_tick(&h.ctx, Tick { next: Some(0.0), approved: false });
    step(&mut h);
    assert!(names(&h).is_empty());
}

#[test]
fn scheduler_waits_while_a_dialog_is_open() {
    let mut app = App::new(sample_session());
    let old = old_mail(&mut app, 400);
    app.run("options.autoArchive", json!({"run": true})).unwrap();
    let last = app.s.now().unix - 20 * 86_400;
    app.s.settings.set(archive::LAST_RUN_KEY, &json!(last)).unwrap();
    app.run("options.autoArchive", json!({})).unwrap();
    let mut h = harness(app);
    store_tick(&h.ctx, Tick { next: Some(0.0), approved: false });
    step(&mut h);
    assert_eq!(path_of(h.state(), old), "Inbox", "busy: not now");
}

#[test]
fn scheduler_waits_for_send_receive_when_the_server_queue_is_full() {
    use mailcraft_model::account::{PendingOp, RemoteOp};
    use mailcraft_model::{Account, AccountKind};
    let mut app = App::new(sample_session());
    let acct = app.s.mailbox.add_account(Account {
        kind: AccountKind::Imap,
        display_name: "user@example.com".into(),
        address: "user@example.com".into(),
        ..Default::default()
    });
    app.s.mailbox.create_default_folders(acct);
    let inbox = app.s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
    app.s.mailbox.folders.get_mut(&inbox).unwrap().remote_name = Some("INBOX".into());
    let received = mailcraft_model::DateTime::from_unix(app.s.now().unix - 400 * 86_400, 0);
    let body = mailcraft_model::item::Body { text: Some("Downloaded".into()), ..Default::default() };
    let item = MailItem { folder: inbox, received, remote_id: Some("3".into()), ..Default::default() };
    let old = app.s.add_message(item, &mailcraft_engine::Content::body(body)).unwrap();
    let full: Vec<PendingOp> = (1..=archive::QUEUE_ROOM)
        .map(|n| PendingOp {
            id: n as u64,
            op: RemoteOp::Expunge { folder: "Sent".into(), uid: n as u32 },
            attempts: 0,
            not_before: 0,
            last_error: None,
        })
        .collect();
    app.s.mailbox.accounts.iter_mut().find(|a| a.id == acct).unwrap().sync.pending = full;
    app.run("options.autoArchive", json!({"run": true})).unwrap();
    let last = app.s.now().unix - 20 * 86_400;
    app.s.settings.set(archive::LAST_RUN_KEY, &json!(last)).unwrap();
    let mut h = harness(app);
    store_tick(&h.ctx, Tick { next: Some(0.0), approved: false });
    step(&mut h);
    assert_eq!(path_of(h.state(), old), "Inbox", "the queue is full: the server message waits");
    assert!(archive::schedule(&h.state().s).due, "still due");
    let next = load_tick(&h.ctx).next.unwrap();
    assert!(next >= BLOCKED_SECS, "waits for send/receive instead of trying every frame: {next}");
}

#[test]
fn message_options_sets_do_not_autoarchive() {
    let mut app = App::new(sample_session());
    let old = old_mail(&mut app, 400);
    app.run("nav.selectItem", json!({"id": old.0})).unwrap();
    app.run("mail.messageOptions", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["messageOptions"]);
    h.get_by_label("Do not AutoArchive this item").click();
    step(&mut h);
    h.get_by_label("OK").click();
    step(&mut h);
    assert!(h.state().s.mailbox.mail.get(old).unwrap().no_auto_archive);
    let mut app = h.into_state();
    app.run("file.archive", json!({"all": true})).unwrap();
    assert_eq!(path_of(&app, old), "Inbox");
}
