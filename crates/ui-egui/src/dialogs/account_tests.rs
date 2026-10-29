//! Account dialog, Send/Receive Progress, Import/Export, Save As and status bar tests (egui_kittest).
//!
//! Servers are only ever 127.0.0.1: a port that was just closed gives a fast, deterministic "connection
//! refused" for the failure paths. Addresses use example.com/.org/.net; work offline is on where a dialog
//! would otherwise start a send/receive.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::{Value, json};

use crate::{App, FileDialogs, FileFilter};
use mailcraft_engine::sample::sample_session;
use mailcraft_model::{AccountId, AccountKind, FolderRole, ItemId, Security};

fn harness(app: App) -> Harness<'static, App> {
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).build_ui_state(
        |ui, app: &mut App| {
            let ctx = ui.ctx().clone();
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

/// Scratch folder under the cargo target dir (never the system temp dir).
fn scratch_dir(name: &str) -> PathBuf {
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok()?.ancestors().nth(3).map(|t| t.join("tmp")))
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp"));
    let dir = base.join(format!("mc-acct-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A local port nothing listens on (bound, then closed).
fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn sample_app() -> App {
    App::new(sample_session())
}

/// Adds an IMAP account whose servers are a closed local port.
fn add_local_imap(app: &mut App, address: &str) -> u32 {
    let port = closed_port();
    let v = app
        .s
        .run(
            "accounts.add",
            &json!({
                "kind": "imap",
                "address": address,
                "senderName": "Pat Example",
                "incoming": {"host": "127.0.0.1", "port": port, "security": "none", "allowPlaintextAuth": true},
                "outgoing": {"host": "127.0.0.1", "port": port, "security": "none", "allowPlaintextAuth": true},
                "password": "pw-local-only",
            }),
        )
        .unwrap();
    u32::try_from(v["account"].as_u64().unwrap()).unwrap()
}

/// Replaces the text of the field labelled `label`.
fn set_text(h: &mut Harness<'_, App>, label: &str, text: &str) {
    h.get_by_label(label).focus();
    step(h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
    h.get_by_label(label).type_text(text);
    step(h);
}

fn combo_shows(h: &Harness<'_, App>, text: &str) -> bool {
    h.query_all_by_role(egui::accesskit::Role::ComboBox).any(|n| n.value().as_deref() == Some(text))
}

fn field_value(h: &Harness<'_, App>, label: &str) -> String {
    h.get_by_label(label).value().unwrap_or_default()
}

/// File dialogs that answer with fixed paths and record what they were asked.
#[derive(Default)]
struct FakeDialogs {
    open: Mutex<Option<PathBuf>>,
    save: Mutex<Option<PathBuf>>,
    folder: Mutex<Option<PathBuf>>,
    asked: Mutex<Vec<(String, Vec<FileFilter>)>>,
}

impl FileDialogs for FakeDialogs {
    fn open_files(
        &self,
        title: &str,
        filters: &[FileFilter],
        _: bool,
        _: Option<&Path>,
    ) -> Option<Vec<PathBuf>> {
        self.asked.lock().unwrap().push((title.to_owned(), filters.to_vec()));
        self.open.lock().unwrap().clone().map(|p| vec![p])
    }
    fn save_file(&self, title: &str, _: &str, filters: &[FileFilter], _: Option<&Path>) -> Option<PathBuf> {
        self.asked.lock().unwrap().push((title.to_owned(), filters.to_vec()));
        self.save.lock().unwrap().clone()
    }
    fn pick_folder(&self, title: &str, _: Option<&Path>) -> Option<PathBuf> {
        self.asked.lock().unwrap().push((title.to_owned(), Vec::new()));
        self.folder.lock().unwrap().clone()
    }
}

// ---------------------------------------------------------------------------------------------------------
// Account Settings

#[test]
fn account_settings_lists_email_accounts_and_sets_default() {
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    let other = add_local_imap(&mut app, "lee@example.org");
    app.run("file.accountSettings", json!({"account": id})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["accountSettings"]);
    assert!(h.query_by_label("pat@example.net").is_some());
    assert!(h.query_by_label("lee@example.org").is_some());
    assert!(h.query_by_label("IMAP/SMTP (send from this account by default)").is_some());
    for b in ["New...", "Repair...", "Change...", "Set as Default", "Remove"] {
        assert!(h.query_by_label(b).is_some(), "{b}");
    }
    h.get_by_label("lee@example.org").click();
    step(&mut h);
    h.get_by_label("Set as Default").click();
    step(&mut h);
    let mb = &h.state().s.mailbox;
    assert!(mb.account(AccountId(other)).unwrap().is_default);
    assert!(!mb.account(AccountId(id)).unwrap().is_default);
    // Data Files lists every account, local data files included.
    h.get_by_label("Data Files").click();
    step(&mut h);
    assert!(h.query_by_label("Open File Location...").is_some());
    assert!(h.query_all_by_label_contains("In memory").count() >= 2);
    h.get_by_label("Close").click();
    step(&mut h);
    assert!(names(&h).is_empty());
}

#[test]
fn remove_asks_then_removes_the_account() {
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    app.run("file.accountSettings", json!({"account": id})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Remove").click();
    step(&mut h);
    assert_eq!(names(&h), ["accountSettings", "confirmRemoveAccount"]);
    assert!(h.query_by_label_contains("Mail on the server is not affected").is_some());
    h.get_by_label("No").click();
    step(&mut h);
    assert!(h.state().s.mailbox.account(AccountId(id)).is_some());
    h.get_by_label("Remove").click();
    step(&mut h);
    h.get_by_label("Yes").click();
    step(&mut h);
    assert_eq!(names(&h), ["accountSettings"]);
    assert!(h.state().s.mailbox.account(AccountId(id)).is_none());
    assert!(h.query_by_label("No email account is set up. Click New... to add one.").is_some());
}

// ---------------------------------------------------------------------------------------------------------
// Add Account wizard

#[test]
fn add_account_guesses_servers_from_the_address() {
    let mut app = sample_app();
    // Done starts a send/receive unless working offline; these hosts are guesses, never contacted.
    app.s.view.work_offline = true;
    let before = app.s.mailbox.accounts.len();
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let mut h = harness(app);
    assert!(h.query_by_label("Add an email account").is_some());
    set_text(&mut h, "Email address:", "sam@example.org");
    h.get_by_label("Connect").click();
    step(&mut h);
    assert!(h.query_by_label("IMAP Account Settings").is_some());
    assert_eq!(field_value(&h, "Server:"), "imap.example.org");
    assert_eq!(field_value(&h, "Outgoing server:"), "smtp.example.org");
    assert_eq!(field_value(&h, "User name:"), "sam@example.org");
    set_text(&mut h, "Password:", "secret-pw");
    h.get_by_label("Connect").click();
    step(&mut h);
    assert!(h.query_by_label("Account successfully added").is_some(), "{:?}", h.state().ui.toast);
    h.get_by_label("Done").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    let app = h.state();
    assert_eq!(app.s.mailbox.accounts.len(), before + 1);
    let a = app.s.mailbox.accounts.iter().find(|a| a.address == "sam@example.org").unwrap();
    assert_eq!(a.kind, AccountKind::Imap);
    let inc = a.incoming.as_ref().unwrap();
    assert_eq!((inc.host.as_str(), inc.port, inc.security), ("imap.example.org", 993, Security::Tls));
    let out = a.outgoing.as_ref().unwrap();
    assert_eq!((out.host.as_str(), out.port, out.security), ("smtp.example.org", 587, Security::StartTls));
    let key = mailcraft_engine::accounts::secret_key(a.id, false);
    assert_eq!(app.s.secrets.get(&key).as_deref(), Some("secret-pw"));
}

#[test]
fn manual_setup_offers_jmap() {
    let mut app = sample_app();
    app.s.view.work_offline = true;
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let mut h = harness(app);
    set_text(&mut h, "Email address:", "kim@example.com");
    h.get_by_label("Let me set up my account manually").click();
    step(&mut h);
    h.get_by_label("Connect").click();
    step(&mut h);
    assert!(h.query_by_label("Advanced setup").is_some());
    h.get_by_label("JMAP").click();
    step(&mut h);
    assert!(h.query_by_label("JMAP Account Settings").is_some());
    assert_eq!(field_value(&h, "Session URL:"), "https://example.com/.well-known/jmap");
    assert!(h.query_by_label("Outgoing server:").is_none());
    h.get_by_label("< Back").click();
    step(&mut h);
    assert!(h.query_by_label("Advanced setup").is_some());
    h.get_by_label("Cancel").click();
    step(&mut h);
    assert!(names(&h).is_empty());
}

#[test]
fn wizard_requires_a_valid_address() {
    let mut app = sample_app();
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let mut h = harness(app);
    set_text(&mut h, "Email address:", "not an address");
    h.get_by_label("Connect").click();
    step(&mut h);
    assert!(h.query_by_label("Add an email account").is_some(), "Connect stays disabled");
}

#[test]
fn test_account_settings_in_the_wizard_and_cancel_removes_the_trial_account() {
    let mut app = sample_app();
    app.s.view.work_offline = true;
    let before = app.s.mailbox.accounts.len();
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let mut h = harness(app);
    set_text(&mut h, "Email address:", "ana@example.net");
    h.get_by_label("Connect").click();
    step(&mut h);
    set_text(&mut h, "Server:", "127.0.0.1");
    set_text(&mut h, "Outgoing server:", "127.0.0.1");
    set_text(&mut h, "Password:", "pw");
    h.get_by_label("Test Account Settings...").click();
    step(&mut h);
    assert_eq!(names(&h), ["addAccount", "testAccount"]);
    assert_eq!(h.state().s.mailbox.accounts.len(), before + 1, "testing needs the account");
    let trial = h.state().s.mailbox.accounts.iter().find(|a| a.address == "ana@example.net").unwrap().id.0;
    // The layout changes when the result arrives: wait for it before clicking.
    wait_for_report(&mut h, trial);
    h.get_by_label("Close").click();
    step(&mut h);
    assert_eq!(names(&h), ["addAccount"]);
    h.get_by_label("Cancel").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    let app = h.state();
    assert_eq!(app.s.mailbox.accounts.len(), before, "Cancel removes the account the test added");
    assert!(app.s.mailbox.accounts.iter().all(|a| a.address != "ana@example.net"));
}

#[test]
fn change_account_prefills_and_keeps_the_password() {
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    crate::dialogs::open(&mut app, "addAccount", json!({"change": id}));
    let mut h = harness(app);
    assert!(h.query_by_label("Change Account").is_some());
    assert_eq!(field_value(&h, "Server:"), "127.0.0.1");
    assert_eq!(field_value(&h, "User name:"), "pat@example.net");
    assert_eq!(field_value(&h, "Password:"), "", "passwords are never shown");
    set_text(&mut h, "Account name:", "Work mail");
    h.get_by_label("Save").click();
    step(&mut h);
    assert!(h.query_by_label("Account successfully updated").is_some(), "{:?}", h.state().ui.toast);
    h.get_by_label("Done").click();
    step(&mut h);
    let app = h.state();
    let a = app.s.mailbox.account(AccountId(id)).unwrap();
    assert_eq!(a.display_name, "Work mail");
    let key = mailcraft_engine::accounts::secret_key(AccountId(id), false);
    assert_eq!(app.s.secrets.get(&key).as_deref(), Some("pw-local-only"));
}

/// Picks `choice` in the `nth` encryption combo (0 = incoming, 1 = outgoing).
fn pick_encryption(h: &mut Harness<'_, App>, nth: usize, choice: &str) {
    h.query_all_by_role(egui::accesskit::Role::ComboBox).nth(nth).unwrap().click();
    step(h);
    h.get_by_label(choice).click();
    step(h);
}

/// Replaces the value of the port field labelled `label`.
fn set_port(h: &mut Harness<'_, App>, label: &str, port: u16) {
    h.get_by_label(label).focus();
    step(h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
    h.get_by_label(label).type_text(&port.to_string());
    step(h);
    h.key_press(Key::Tab);
    step(h);
}

/// The whole Add Account flow against the local test servers (127.0.0.1, no encryption): without
/// encryption the password only goes out when the person allows it for this server, and then Test Account
/// Settings completes both steps.
#[test]
fn wizard_connects_to_a_local_server_without_encryption() {
    let servers = mailcraft_testmail::TestServers::start(mailcraft_testmail::Config {
        users: vec![("ana@example.net".into(), "pw-ana-local".into())],
        ..mailcraft_testmail::Config::default()
    })
    .unwrap();
    let mut app = sample_app();
    app.s.view.work_offline = true;
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let mut h = harness(app);
    set_text(&mut h, "Email address:", "ana@example.net");
    h.get_by_label("Connect").click();
    step(&mut h);
    set_text(&mut h, "Server:", "127.0.0.1");
    set_text(&mut h, "Outgoing server:", "127.0.0.1");
    pick_encryption(&mut h, 0, "None");
    pick_encryption(&mut h, 1, "None");
    set_port(&mut h, "Port:", servers.imap_port);
    set_port(&mut h, "Outgoing port:", servers.smtp_port);
    set_text(&mut h, "Password:", "pw-ana-local");
    // Unencrypted sign-in is an explicit choice.
    let allow =
        "Allow sign-in without encryption (only for a server you trust, such as one on this computer)";
    h.get_by_label(allow).click();
    step(&mut h);
    h.get_by_label("Test Account Settings...").click();
    step(&mut h);
    assert_eq!(names(&h), ["addAccount", "testAccount"]);
    let id = h.state().s.mailbox.accounts.iter().find(|a| a.address == "ana@example.net").unwrap().id;
    let a = h.state().s.mailbox.account(id).unwrap();
    let (inc, out) = (a.incoming.clone().unwrap(), a.outgoing.clone().unwrap());
    assert_eq!((inc.port, inc.security, inc.allow_plaintext_auth), (servers.imap_port, Security::None, true));
    assert_eq!((out.port, out.security, out.allow_plaintext_auth), (servers.smtp_port, Security::None, true));
    wait_for_report(&mut h, id.0);
    let report = h.state().s.sync.tests.get(&id).cloned().unwrap();
    assert!(report.ok(), "{report:?}");
    assert!(
        h.query_by_label("Congratulations! All tests completed successfully. Click Close to continue.")
            .is_some()
    );
    assert_eq!(h.query_all_by_label("Completed").count(), 2);
    h.get_by_label("Close").click();
    step(&mut h);
    h.get_by_label("Connect").click();
    step(&mut h);
    assert!(h.query_by_label("Account successfully added").is_some(), "{:?}", h.state().ui.toast);
    h.get_by_label("Done").click();
    step(&mut h);
    assert!(names(&h).is_empty());
    assert!(h.state().s.mailbox.account(id).is_some(), "Done keeps the account");
    servers.shutdown();
}

/// Change Account shows the saved unencrypted sign-in choice and keeps it.
#[test]
fn change_account_keeps_unencrypted_sign_in() {
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    crate::dialogs::open(&mut app, "addAccount", json!({"change": id}));
    let mut h = harness(app);
    let allow =
        "Allow sign-in without encryption (only for a server you trust, such as one on this computer)";
    // Shown (encryption is None) and ticked: Save sends what the box shows.
    assert!(h.query_by_label(allow).is_some());
    h.get_by_label("Save").click();
    step(&mut h);
    let a = h.state().s.mailbox.account(AccountId(id)).unwrap();
    assert!(a.incoming.as_ref().unwrap().allow_plaintext_auth);
    assert!(a.outgoing.as_ref().unwrap().allow_plaintext_auth);
}

// ---------------------------------------------------------------------------------------------------------
// Test Account Settings

/// How long a test waits for background work (lookups, test connections, sync runs). Generous, so a busy
/// machine (the full CI run builds and tests in parallel) does not fail the test.
const WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Steps the UI until `done` holds, failing with `what` after [`WAIT`].
fn step_until(h: &mut Harness<'_, App>, what: &str, mut done: impl FnMut(&Harness<'_, App>) -> bool) {
    let end = std::time::Instant::now() + WAIT;
    loop {
        step(h);
        if done(h) {
            return;
        }
        assert!(std::time::Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn wait_for_report(h: &mut Harness<'_, App>, id: u32) {
    step_until(h, "the test report", |h| h.state().s.sync.tests.contains_key(&AccountId(id)));
    step(h);
}

#[test]
fn test_account_settings_shows_each_step() {
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    crate::dialogs::open(&mut app, "testAccount", json!({"account": id, "repair": true}));
    let mut h = harness(app);
    assert!(h.query_by_label("Log onto incoming mail server (IMAP)").is_some());
    assert!(h.query_by_label("Log onto outgoing mail server (SMTP)").is_some());
    wait_for_report(&mut h, id);
    assert!(h.query_by_label("Some tests did not complete. Check the errors below.").is_some());
    assert!(h.query_all_by_label("Failed").count() >= 1);
    h.get_by_label("Errors").click();
    step(&mut h);
    assert!(h.query_by_label("No errors.").is_none());
    // Repair offers Change... when the test fails.
    h.get_by_label("Change...").click();
    step(&mut h);
    assert_eq!(names(&h), ["addAccount"]);
}

#[test]
fn test_account_without_an_email_account_says_so() {
    let mut app = sample_app();
    crate::dialogs::open(&mut app, "testAccount", json!({"account": 1}));
    let mut h = harness(app);
    assert!(h.query_by_label("Choose an email account to test.").is_some());
    h.key_press(Key::Escape);
    step(&mut h);
    assert!(names(&h).is_empty());
}

// ---------------------------------------------------------------------------------------------------------
// Send/Receive Progress and the status bar

#[test]
fn send_receive_progress_lists_accounts_and_errors() {
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    app.s.poll_sync();
    app.run("sendReceive.showProgress", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["sendReceiveProgress"]);
    assert!(h.query_by_label("pat@example.net").is_some());
    assert!(h.query_by_label("Not started").is_some());
    // A run against the closed port fails; the dialog follows the live status.
    h.state_mut().run("sendReceive.group", json!({"name": "pat@example.net"}));
    assert!(h.state_mut().s.wait_for_sync(std::time::Duration::from_secs(10)));
    step(&mut h);
    assert!(h.state().s.sync_status().accounts.iter().any(|a| a.account == id && a.error.is_some()));
    assert!(h.query_by_label("Error").is_some());
    h.get_by_label("Errors").click();
    step(&mut h);
    assert!(h.query_by_label("No errors.").is_none());
    h.get_by_label("Close").click();
    step(&mut h);
    assert!(names(&h).is_empty());
}

fn status_harness(app: App) -> Harness<'static, App> {
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 40.0)).build_ui_state(
        |ui, app: &mut App| {
            let frame = app.frame_for(1280.0, 900.0);
            ui.horizontal(|ui| crate::chrome::status_bar(ui, app, &frame));
        },
        app,
    );
    h.run_steps(2);
    h
}

#[test]
fn status_bar_shows_outbox_offline_and_retry() {
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    // One message waiting in the new account's Outbox.
    let outbox = app.s.mailbox.role_folder(AccountId(id), FolderRole::Outbox).unwrap();
    let item = app.s.mailbox.mail.ids().next().unwrap();
    app.s.mailbox.mail.get_mut(item).unwrap().folder = outbox;
    app.s.view.work_offline = true;
    app.s.poll_sync();
    let mut h = status_harness(app);
    assert!(h.query_by_label("Outbox: 1").is_some());
    assert_eq!(h.query_all_by_label("Working offline").count(), 2, "status text and connection state");
    // Clicking the connection state turns Work Offline off.
    h.get_all_by_label("Working offline").last().unwrap().click();
    step(&mut h);
    assert!(!h.state().s.view.work_offline);
    // A failed send: the error replaces the badge and offers Retry.
    h.state_mut().run("sendReceive.sendAll", json!({}));
    assert!(h.state_mut().s.wait_for_sync(std::time::Duration::from_secs(10)));
    h.state_mut().s.poll_sync();
    step(&mut h);
    assert!(h.query_by_label("1 message(s) in the Outbox could not be sent").is_some());
    assert!(h.query_by_label("Retry").is_some());
    h.get_by_label("1 message(s) in the Outbox could not be sent").click();
    step(&mut h);
    let progress =
        h.state().s.notices.iter().any(
            |n| matches!(n, mailcraft_engine::Notice::OpenDialog(name, _) if name == "sendReceiveProgress"),
        );
    assert!(progress, "clicking the error opens Send/Receive Progress");
    assert_eq!(h.state().s.mailbox.mail.get(ItemId(item.0)).map(|m| m.folder), Some(outbox));
}

/// Puts the status of account `id` in the middle of a first download (as the worker reports it).
fn downloading(app: &mut App, id: u32) {
    let d = mailcraft_engine::sync::SyncProgress {
        folder: "Inbox".into(),
        folder_index: 3,
        folder_count: 12,
        done: 1240,
        total: 5000,
        headers: false,
    };
    let st = &mut app.s.sync.status;
    st.busy = true;
    st.text = d.text();
    st.detail = Some(d.clone());
    let row = st.accounts.iter_mut().find(|a| a.account == id).unwrap();
    row.busy = true;
    row.progress = d.text();
    row.detail = Some(d);
}

fn status_harness_at(app: App, width: f32) -> Harness<'static, App> {
    let mut h = Harness::builder().with_size(egui::vec2(width, 40.0)).build_ui_state(
        move |ui, app: &mut App| {
            let frame = app.frame_for(width, 900.0);
            ui.horizontal(|ui| crate::chrome::status_bar(ui, app, &frame));
        },
        app,
    );
    h.run_steps(2);
    h
}

#[test]
fn status_bar_and_progress_window_count_the_first_download() {
    let full = "Downloading Inbox: 1,240 of 5,000 messages (folder 3 of 12)";
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    app.s.poll_sync();
    downloading(&mut app, id);
    let h = status_harness(app);
    assert!(h.query_by_label(full).is_some(), "the status bar says how far the download has got");

    // A narrow window keeps the counts rather than cutting them off.
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    app.s.poll_sync();
    downloading(&mut app, id);
    let h = status_harness_at(app, 600.0);
    assert!(h.query_by_label(full).is_none());
    assert!(h.query_by_label("Inbox: 1,240 of 5,000").is_some());

    // Send/Receive Progress shows the same counts on the account's row.
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    app.s.poll_sync();
    app.run("sendReceive.showProgress", json!({})).unwrap();
    downloading(&mut app, id);
    let mut h = harness(app);
    assert_eq!(names(&h), ["sendReceiveProgress"]);
    assert!(h.query_by_label(full).is_some());
    assert!(h.query_by_label("0 of 1 tasks have completed successfully").is_some());
    // Once the run is over the counts go away.
    h.state_mut().s.sync.status.accounts.iter_mut().for_each(|a| {
        a.busy = false;
        a.detail = None;
        a.progress.clear();
    });
    h.state_mut().s.sync.status.busy = false;
    h.state_mut().s.sync.status.detail = None;
    step(&mut h);
    assert!(h.query_by_label(full).is_none());
}

#[test]
fn status_bar_is_quiet_without_email_accounts() {
    let app = sample_app();
    let h = status_harness(app);
    assert!(h.query_by_label_contains("Outbox").is_none());
    assert!(h.query_by_label("Working offline").is_none());
    assert!(h.query_by_label("Connected").is_none());
}

// ---------------------------------------------------------------------------------------------------------
// Import/Export and Save As

#[test]
fn import_export_imports_a_vcard_picked_with_browse() {
    let dir = scratch_dir("vcf");
    let file = dir.join("people.vcf");
    std::fs::write(
        &file,
        "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Robin Example\r\nEMAIL:robin@example.com\r\nEND:VCARD\r\n",
    )
    .unwrap();
    let fake = Arc::new(FakeDialogs::default());
    *fake.open.lock().unwrap() = Some(file.clone());
    let mut app = sample_app();
    app.set_file_dialogs(fake.clone());
    let contacts_before = app.s.mailbox.contacts.len();
    app.run("file.openExport", json!({})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Import a vCard file (.vcf)").click();
    step(&mut h);
    assert!(h.query_by_label_contains("RFC 6350").is_some());
    h.get_by_label("Next >").click();
    step(&mut h);
    assert!(h.query_by_label("Import into this contacts folder:").is_some());
    h.get_by_label("Browse...").click();
    step(&mut h);
    {
        let asked = fake.asked.lock().unwrap();
        let (title, filters) = asked.last().unwrap();
        assert_eq!(title, "Import a vCard file (.vcf)");
        assert!(filters.first().unwrap().extensions.contains(&"vcf".to_owned()));
    }
    h.get_by_label("Finish").click();
    step(&mut h);
    assert_eq!(names(&h), ["message"], "{:?}", h.state().ui.toast);
    assert!(h.query_by_label_contains("Imported 1 contact(s)").is_some());
    assert_eq!(h.state().s.mailbox.contacts.len(), contacts_before + 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_export_exports_a_calendar_to_ics() {
    let dir = scratch_dir("ics");
    let out = dir.join("cal.ics");
    let mut app = sample_app();
    app.run("file.openExport", json!({})).unwrap();
    let mut h = harness(app);
    h.get_by_label("Export a calendar to an iCalendar (.ics) file").click();
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
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.starts_with("BEGIN:VCALENDAR"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_as_picks_the_type_from_the_save_dialog() {
    let dir = scratch_dir("saveas");
    let target = dir.join("note.txt");
    let fake = Arc::new(FakeDialogs::default());
    *fake.save.lock().unwrap() = Some(target.clone());
    let mut app = sample_app();
    app.set_file_dialogs(fake.clone());
    let folder = app.s.selection.folder;
    let id = app.s.mailbox.items_in(folder).first().map(|m| m.id.0).unwrap();
    app.run("nav.selectItem", json!({"id": id})).unwrap();
    app.run("file.saveAs", json!({})).unwrap();
    let mut h = harness(app);
    assert_eq!(names(&h), ["saveAs"]);
    assert!(combo_shows(&h, "Internet message (*.eml)"));
    h.get_by_label("Browse...").click();
    step(&mut h);
    assert_eq!(field_value(&h, "File name:"), target.display().to_string());
    assert!(combo_shows(&h, "Text only (*.txt)"), "the type follows the chosen extension");
    h.get_by_label("Save").click();
    step(&mut h);
    assert!(names(&h).is_empty(), "{:?}", h.state().ui.toast);
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(text.contains("Subject:"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_as_type_changes_the_extension() {
    assert_eq!(super::files::with_save_ext("/x/Report.eml", 3), "/x/Report.html");
    assert_eq!(super::files::with_save_ext("/x/", 1), "/x/");
    assert_eq!(super::files::save_type_of("a.HTM"), 3);
    assert_eq!(super::files::save_type_of("a"), 0);
    let f = super::files::save_filters(2);
    assert_eq!(f.first().map(|f| f.extensions.clone()), Some(vec!["txt".to_owned()]));
    assert_eq!(f.len(), 4);
}

#[test]
fn import_export_actions_cover_every_engine_format() {
    let keys: Vec<&str> = super::files::IE_ACTIONS.iter().map(|a| a.key).collect();
    for k in [
        "importEml",
        "importMsg",
        "importMbox",
        "importMaildir",
        "importIcs",
        "importVcf",
        "importCsv",
        "exportMbox",
        "exportIcs",
        "exportVcf",
        "exportCsv",
    ] {
        assert!(keys.contains(&k), "{k}");
    }
    let params: Value = json!(keys);
    assert!(params.is_array());
}

// ---------------------------------------------------------------------------------------------------------
// Backstage and the shell

#[test]
fn backstage_account_settings_menu() {
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    app.run(
        "nav.selectFolder",
        json!({"folder": app.s.mailbox.role_folder(AccountId(id), FolderRole::Inbox).unwrap().0}),
    )
    .unwrap();
    app.ui.backstage = Some((crate::Surface::Main, "info".into()));
    let mut h = harness(app);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Account Settings").click();
    step(&mut h);
    assert!(h.query_by_label("Account Name and Sync Settings").is_some());
    assert!(h.query_by_label("Test Account Settings...").is_some());
    h.get_by_label("Account Settings...").click();
    step(&mut h);
    assert_eq!(names(&h), ["accountSettings"]);
    h.key_press(Key::Escape);
    step(&mut h);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Account Settings").click();
    step(&mut h);
    h.get_by_label("Server Settings").click();
    step(&mut h);
    assert_eq!(names(&h), ["addAccount"]);
    assert_eq!(field_value(&h, "Server:"), "127.0.0.1");
}

#[test]
fn the_shell_polls_send_receive_every_frame() {
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 900.0))
        .build_ui_state(|ui, app: &mut App| app.ui_root(ui), app);
    h.run_steps(2);
    h.state_mut().run("sendReceive.group", json!({"name": "pat@example.net"}));
    // No explicit poll: `App::ui_root` applies the worker's results.
    step_until(&mut h, "the run against a closed port to finish with an error", |h| {
        let st = h.state().s.sync_status();
        !st.busy && st.accounts.iter().any(|a| a.account == id && a.error.is_some())
    });
    h.run_steps(2);
    assert!(h.query_by_label("Retry").is_some());
}

#[test]
fn manual_setup_offers_pop3_with_server_copy_settings() {
    let mut app = sample_app();
    app.s.view.work_offline = true;
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let mut h = harness(app);
    set_text(&mut h, "Email address:", "lee@example.com");
    h.get_by_label("Let me set up my account manually").click();
    step(&mut h);
    h.get_by_label("Connect").click();
    step(&mut h);
    h.get_by_label("POP").click();
    step(&mut h);
    assert!(h.query_by_label("POP Account Settings").is_some());
    assert_eq!(field_value(&h, "Server:"), "pop.example.com");
    assert_eq!(field_value(&h, "Port:"), "995");
    h.get_by_label("More Settings").scroll_to_me();
    step(&mut h);
    h.get_by_label("More Settings").click();
    step(&mut h);
    h.get_by_label("Leave a copy of messages on the server").scroll_to_me();
    step(&mut h);
    h.get_by_label("Leave a copy of messages on the server").click();
    step(&mut h);
    set_text(&mut h, "Password:", "pop-pw");
    h.get_by_label("Connect").click();
    step(&mut h);
    assert!(h.query_by_label("Account successfully added").is_some(), "{:?}", h.state().ui.toast);
    h.get_by_label("Done").click();
    step(&mut h);
    let app = h.state();
    let a = app.s.mailbox.accounts.iter().find(|a| a.address == "lee@example.com").unwrap();
    assert_eq!(mailcraft_engine::accounts::kind_label(&app.s, a), "pop3");
    assert_eq!(a.incoming.as_ref().unwrap().port, 995);
    let o = mailcraft_engine::accounts::account_options(&app.s, a.id);
    assert!(o.pop3 && !o.leave_on_server);
}

#[test]
fn account_settings_shows_pop_accounts_and_the_secret_store() {
    let mut app = sample_app();
    app.s
        .run(
            "accounts.add",
            &json!({"kind": "pop3", "address": "pop@example.org", "incoming": {"host": "pop.example.org"}, "password": "x"}),
        )
        .unwrap();
    crate::dialogs::open(&mut app, "accountSettings", json!({}));
    let h = harness(app);
    assert!(h.query_by_label_contains("POP/SMTP").is_some());
    assert!(h.query_by_label_contains("kept in memory").is_some());
}

#[test]
fn change_account_starts_and_cancels_an_oauth_sign_in() {
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    crate::dialogs::open(&mut app, "addAccount", json!({"change": id}));
    let mut h = harness(app);
    h.get_by_label("Sign in with OAuth 2.0 instead of a password").scroll_to_me();
    step(&mut h);
    h.get_by_label("Sign in with OAuth 2.0 instead of a password").click();
    step(&mut h);
    assert!(h.query_by_label_contains("no built-in registration").is_some());
    set_text(&mut h, "Client id:", "my-registered-app");
    h.get_by_label("Browser").scroll_to_me();
    step(&mut h);
    h.get_by_label("Browser").click();
    step(&mut h);
    h.get_by_label("Sign In").scroll_to_me();
    step(&mut h);
    h.get_by_label("Sign In").click();
    step(&mut h);
    {
        let app = h.state();
        let flow = app.s.sync.oauth.get(&AccountId(id)).cloned().unwrap();
        assert_eq!(flow.status, "waiting");
        assert!(flow.auth_url.as_deref().is_some_and(|u| u.contains("client_id=my-registered-app")));
        let o = mailcraft_engine::accounts::account_options(&app.s, AccountId(id));
        assert_eq!(o.oauth.unwrap().client_id, "my-registered-app");
    }
    assert!(h.query_by_label_contains("Finish signing in in your browser").is_some());
    h.get_by_label("Cancel Sign-In").scroll_to_me();
    step(&mut h);
    h.get_by_label("Cancel Sign-In").click();
    step(&mut h);
    assert_eq!(h.state().s.sync.oauth.get(&AccountId(id)).unwrap().status, "cancelled");
    assert!(h.query_by_label_contains("Sign-in was cancelled").is_some());
}

// ---------------------------------------------------------------------------------------------------------
// Server lookup in the Add Account wizard

/// Points the session's server lookup at the local test servers (autoconfig over HTTPS, fake DNS).
fn local_lookup(app: &mut App, servers: &mailcraft_testmail::TestServers) {
    use mailcraft_engine::sync::{ConnectOptions, DiscoverOptions, ResolverChoice};
    app.s.sync.discovery = DiscoverOptions {
        resolver: ResolverChoice::Fixed(servers.dns_addr()),
        dns_timeout: std::time::Duration::from_secs(2),
        autoconfig_urls: servers.autoconfig_url_templates(),
        http: ConnectOptions::trusting(servers.cert_der.clone()),
        tenant_url: Some(servers.tenant_url_template()),
        allow_reserved: true,
    };
}

/// Steps until the settings page shows (the lookup runs in the background).
fn wait_for_settings(h: &mut Harness<'_, App>, title: &str) {
    step_until(h, "the settings page", |h| h.query_by_label(title).is_some());
}

#[test]
fn add_account_uses_the_providers_autoconfig_before_guessing() {
    let servers = mailcraft_testmail::TestServers::start(mailcraft_testmail::Config::default()).unwrap();
    servers.set_autoconfig(
        "example.net",
        Some(
            "<clientConfig version='1.1'><emailProvider id='example.net'>\
             <incomingServer type='imap'><hostname>mail.example.net</hostname><port>143</port>\
             <socketType>STARTTLS</socketType><username>%EMAILLOCALPART%</username></incomingServer>\
             <outgoingServer type='smtp'><hostname>mail.example.net</hostname><port>465</port>\
             <socketType>SSL</socketType><username>%EMAILLOCALPART%</username></outgoingServer>\
             </emailProvider></clientConfig>",
        ),
    );
    let mut app = sample_app();
    app.s.view.work_offline = true;
    local_lookup(&mut app, &servers);
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let mut h = harness(app);
    set_text(&mut h, "Email address:", "robin@example.net");
    h.get_by_label("Connect").click();
    wait_for_settings(&mut h, "IMAP Account Settings");
    assert!(h.query_by_label_contains("your email provider's configuration").is_some());
    assert_eq!(field_value(&h, "Server:"), "mail.example.net");
    assert_eq!(field_value(&h, "Port:"), "143");
    assert!(combo_shows(&h, "STARTTLS"));
    assert_eq!(field_value(&h, "User name:"), "robin");
    assert_eq!(field_value(&h, "Outgoing server:"), "mail.example.net");
    assert_eq!(field_value(&h, "Outgoing port:"), "465");
    // More Settings: how far back the first download goes, and headers only on slow connections.
    h.get_by_label("More Settings").scroll_to_me();
    step(&mut h);
    h.get_by_label("More Settings").click();
    step(&mut h);
    h.get_by_label("On slow connections download only headers").scroll_to_me();
    step(&mut h);
    h.get_by_label("On slow connections download only headers").click();
    step(&mut h);
    assert!(combo_shows(&h, "12 months"));
    h.get_by_label("Download mail from the past:").scroll_to_me();
    step(&mut h);
    let combo = "12 months";
    h.query_all_by_role(egui::accesskit::Role::ComboBox)
        .find(|n| n.value().as_deref() == Some(combo))
        .unwrap()
        .scroll_to_me();
    step(&mut h);
    h.query_all_by_role(egui::accesskit::Role::ComboBox)
        .find(|n| n.value().as_deref() == Some(combo))
        .unwrap()
        .click();
    step(&mut h);
    h.get_by_label("3 months").click();
    step(&mut h);
    set_text(&mut h, "Password:", "pw");
    h.get_by_label("Connect").click();
    step(&mut h);
    assert!(h.query_by_label("Account successfully added").is_some(), "{:?}", h.state().ui.toast);
    let app = h.state();
    let a = app.s.mailbox.accounts.iter().find(|a| a.address == "robin@example.net").unwrap();
    let inc = a.incoming.as_ref().unwrap();
    assert_eq!(
        (inc.host.as_str(), inc.port, inc.security, inc.username.as_str()),
        ("mail.example.net", 143, Security::StartTls, "robin")
    );
    let out = a.outgoing.as_ref().unwrap();
    assert_eq!((out.port, out.security), (465, Security::Tls));
    let o = mailcraft_engine::accounts::account_options(&app.s, a.id);
    assert_eq!(o.download_months, Some(3));
    assert!(!o.slow_headers_only, "the box was on by default and was cleared");
}

#[test]
fn add_account_points_out_servers_outside_the_domain_found_in_dns() {
    let servers = mailcraft_testmail::TestServers::start(mailcraft_testmail::Config::default()).unwrap();
    let srv = |port: u16, target: &str| {
        vec![mailcraft_testmail::Srv { priority: 0, weight: 0, port, target: target.into() }]
    };
    servers.set_srv("_imaps._tcp.example.org", srv(993, "imap.example.org"));
    servers.set_srv("_submission._tcp.example.org", srv(587, "smtp.hosting.example"));
    let mut app = sample_app();
    app.s.view.work_offline = true;
    local_lookup(&mut app, &servers);
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let mut h = harness(app);
    set_text(&mut h, "Email address:", "lou@example.org");
    h.get_by_label("Connect").click();
    wait_for_settings(&mut h, "IMAP Account Settings");
    assert!(h.query_by_label_contains("your domain's DNS records").is_some());
    assert_eq!(field_value(&h, "Server:"), "imap.example.org");
    assert_eq!(field_value(&h, "Outgoing server:"), "smtp.hosting.example");
    assert!(h.query_by_label_contains("smtp.hosting.example is not part of example.org").is_some());
    // Back, manual setup, POP: nothing found for POP, so the guess.
    h.get_by_label("< Back").click();
    step(&mut h);
    h.get_by_label("Let me set up my account manually").click();
    step(&mut h);
    h.get_by_label("Connect").click();
    step_until(&mut h, "Advanced setup", |h| h.query_by_label("Advanced setup").is_some());
    h.get_by_label("POP").click();
    step(&mut h);
    assert_eq!(field_value(&h, "Server:"), "pop.example.org");
    assert!(h.query_by_label_contains("guessed from your address").is_some());
}

#[test]
fn add_account_recognises_microsoft_365_by_mx_and_signs_in_with_oauth() {
    const TENANT: &str = "6f1a5c2e-0b7d-4c3a-9e8f-1d2c3b4a5e6f";
    let servers = mailcraft_testmail::TestServers::start(mailcraft_testmail::Config::default()).unwrap();
    servers.set_mx(
        "m365.example.net",
        vec![mailcraft_testmail::Mx {
            preference: 0,
            exchange: "m365-example-net.mail.protection.outlook.com".into(),
        }],
    );
    servers.set_tenant("m365.example.net", Some(TENANT));
    let mut app = sample_app();
    app.s.view.work_offline = true;
    local_lookup(&mut app, &servers);
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let mut h = harness(app);
    set_text(&mut h, "Email address:", "robin@m365.example.net");
    h.get_by_label("Connect").click();
    wait_for_settings(&mut h, "IMAP Account Settings");
    // Microsoft 365's servers, not imap.<domain> / smtp.<domain>.
    assert!(h.query_by_label_contains("(MX) records point at Microsoft 365").is_some());
    assert!(h.query_by_label_contains("guessed").is_none());
    assert_eq!(field_value(&h, "Server:"), "outlook.office365.com");
    assert_eq!(field_value(&h, "Port:"), "993");
    assert_eq!(field_value(&h, "User name:"), "robin@m365.example.net");
    assert_eq!(field_value(&h, "Outgoing server:"), "smtp.office365.com");
    assert_eq!(field_value(&h, "Outgoing port:"), "587");
    assert!(combo_shows(&h, "STARTTLS"));
    assert!(h.query_by_label_contains("Authenticated SMTP").is_some());
    assert!(h.query_by_label_contains("consent to the permissions").is_some());
    assert!(h.query_by_label_contains("is not part of").is_none(), "a known provider's own servers");
    // The OAuth section is open with the Microsoft provider; signing in uses the organization's tenant.
    assert!(h.query_by_label_contains("no built-in registration").is_some());
    assert!(combo_shows(&h, "Microsoft (personal and work or school accounts)"));
    set_text(&mut h, "Client id:", "org-registered-app");
    h.get_by_label("Browser").scroll_to_me();
    step(&mut h);
    h.get_by_label("Browser").click();
    step(&mut h);
    h.get_by_label("Sign In").scroll_to_me();
    step(&mut h);
    h.get_by_label("Sign In").click();
    step(&mut h);
    let id = {
        let app = h.state();
        let a = app.s.mailbox.accounts.iter().find(|a| a.address == "robin@m365.example.net").unwrap();
        let inc = a.incoming.as_ref().unwrap();
        assert_eq!(
            (inc.host.as_str(), inc.port, inc.security),
            ("outlook.office365.com", 993, Security::Tls)
        );
        let out = a.outgoing.as_ref().unwrap();
        assert_eq!(
            (out.host.as_str(), out.port, out.security),
            ("smtp.office365.com", 587, Security::StartTls)
        );
        let flow = app.s.sync.oauth.get(&a.id).cloned().unwrap();
        assert_eq!(flow.status, "waiting", "{:?}", flow.error);
        let url = flow.auth_url.unwrap_or_default();
        assert!(
            url.starts_with(&format!("https://login.microsoftonline.com/{TENANT}/oauth2/v2.0/authorize")),
            "{url}"
        );
        a.id
    };
    h.get_by_label("Cancel Sign-In").scroll_to_me();
    step(&mut h);
    h.get_by_label("Cancel Sign-In").click();
    step(&mut h);
    assert_eq!(h.state().s.sync.oauth.get(&id).unwrap().status, "cancelled");
}

#[test]
fn add_account_knows_gmail_without_a_lookup() {
    let mut app = sample_app();
    app.s.view.work_offline = true;
    // No test servers at all: a consumer domain needs none.
    app.s.sync.discovery = mailcraft_engine::sync::DiscoverOptions::offline();
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let mut h = harness(app);
    set_text(&mut h, "Email address:", "kim@gmail.com");
    h.get_by_label("Connect").click();
    wait_for_settings(&mut h, "IMAP Account Settings");
    assert!(h.query_by_label_contains("These are Google's settings for gmail.com addresses").is_some());
    assert_eq!(field_value(&h, "Server:"), "imap.gmail.com");
    assert_eq!(field_value(&h, "Outgoing server:"), "smtp.gmail.com");
    assert_eq!(field_value(&h, "Outgoing port:"), "465");
    assert!(h.query_by_label_contains("app password").is_some());
    assert!(combo_shows(&h, "Google"));
    // POP instead: Google's POP server.
    h.get_by_label("< Back").click();
    step(&mut h);
    h.get_by_label("Let me set up my account manually").click();
    step(&mut h);
    h.get_by_label("Connect").click();
    step_until(&mut h, "Advanced setup", |h| h.query_by_label("Advanced setup").is_some());
    h.get_by_label("POP").click();
    step(&mut h);
    assert_eq!(field_value(&h, "Server:"), "pop.gmail.com");
    assert_eq!(field_value(&h, "Port:"), "995");
}

#[test]
fn add_account_name_never_comes_from_a_data_file() {
    use mailcraft_engine::Session;
    use mailcraft_model::Mailbox;
    // A fresh profile has only its local data file: Your name starts empty.
    let mut app = App::new(Session::new(Mailbox::new_local("Local Folders")));
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let h = harness(app);
    assert_eq!(field_value(&h, "Your name:"), "");

    // With a user name in Options › General, that name is offered.
    let mut app = App::new(Session::new(Mailbox::new_local("Local Folders")));
    app.s.settings.set("general.userName", &json!("Sam Example")).expect("set user name");
    crate::dialogs::open(&mut app, "addAccount", json!({}));
    let h = harness(app);
    assert_eq!(field_value(&h, "Your name:"), "Sam Example");
}

/// The slider of the offline window: value text, stop number, disabled.
fn offline_slider(h: &Harness<'_, App>) -> (String, f64, bool) {
    use egui_kittest::kittest::NodeT;
    let n = h.get_by_label(crate::dialogs::accounts::OFFLINE_SLIDER);
    let node = n.accesskit_node();
    assert_eq!(node.role(), egui::accesskit::Role::Slider);
    (n.value().unwrap_or_default(), node.numeric_value().unwrap_or(-1.0), node.is_disabled())
}

/// Sends a screen reader's `action` (with `data`) to the offline window slider.
fn slider_action(
    h: &mut Harness<'_, App>,
    action: egui::accesskit::Action,
    data: Option<egui::accesskit::ActionData>,
) {
    use egui_kittest::kittest::NodeT;
    let (target_node, target_tree) =
        h.get_by_label(crate::dialogs::accounts::OFFLINE_SLIDER).accesskit_node().locate();
    h.event(egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action,
        target_tree,
        target_node,
        data,
    }));
    step(h);
}

#[test]
fn imap_settings_offer_the_offline_window_and_save_it() {
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    crate::dialogs::open(&mut app, "addAccount", json!({"change": id}));
    let mut h = harness(app);
    assert!(h.query_by_label("Offline mail").is_some());
    let keep = "Keep mail on this computer to read when offline";
    assert_eq!(offline_slider(&h), ("1 year".into(), 6.0, false), "one year by default");
    h.get_by_label(crate::dialogs::accounts::OFFLINE_SLIDER).scroll_to_me();
    step(&mut h);
    // The keyboard moves it one stop, three stops, or to either end; the chosen value shows beside it.
    h.get_by_label(crate::dialogs::accounts::OFFLINE_SLIDER).focus();
    step(&mut h);
    for (key, want, stop) in [
        (Key::ArrowLeft, "6 months", 5.0),
        (Key::End, "All", 10.0),
        (Key::ArrowRight, "All", 10.0),
        (Key::Home, "3 days", 0.0),
        (Key::ArrowDown, "3 days", 0.0),
        (Key::PageUp, "1 month", 3.0),
        (Key::ArrowUp, "3 months", 4.0),
    ] {
        h.key_press(key);
        step(&mut h);
        assert_eq!(offline_slider(&h), (want.into(), stop, false), "after {key:?}");
    }
    assert!(h.query_by_label("3 months").is_some(), "the value is written beside the slider");
    // Off: the slider is disabled.
    h.get_by_label(keep).click();
    step(&mut h);
    assert!(offline_slider(&h).2, "disabled while mail is not kept");
    h.get_by_label(keep).click();
    step(&mut h);
    assert!(!offline_slider(&h).2);
    h.get_by_label("Save").click();
    step(&mut h);
    assert!(h.query_by_label("Account successfully updated").is_some(), "{:?}", h.state().ui.toast);
    let o = mailcraft_engine::accounts::account_options(&h.state().s, AccountId(id));
    assert!(o.keep_offline);
    assert_eq!(o.offline_window, mailcraft_engine::offline::OfflineWindow::ThreeMonths);
}

#[test]
fn the_offline_window_slider_follows_the_screen_reader_and_the_pointer() {
    use egui::accesskit::{Action, ActionData};
    let mut app = sample_app();
    let id = add_local_imap(&mut app, "pat@example.net");
    crate::dialogs::open(&mut app, "addAccount", json!({"change": id}));
    let mut h = harness(app);
    h.get_by_label(crate::dialogs::accounts::OFFLINE_SLIDER).scroll_to_me();
    step(&mut h);
    // Assistive technology: Increment, Decrement and Set Value (to the nearest stop).
    slider_action(&mut h, Action::Increment, None);
    assert_eq!(offline_slider(&h).0, "2 years");
    slider_action(&mut h, Action::Decrement, None);
    slider_action(&mut h, Action::Decrement, None);
    assert_eq!(offline_slider(&h).0, "6 months");
    slider_action(&mut h, Action::SetValue, Some(ActionData::NumericValue(1.4)));
    assert_eq!(offline_slider(&h).0, "1 week");
    slider_action(&mut h, Action::SetValue, Some(ActionData::NumericValue(f64::NAN)));
    assert_eq!(offline_slider(&h).0, "1 week", "NaN is ignored");
    slider_action(&mut h, Action::SetValue, Some(ActionData::NumericValue(1e300)));
    assert_eq!(offline_slider(&h).0, "All", "out of range goes to the nearest end");
    // The pointer: a click at the left end picks the first stop.
    let rect = h.get_by_label(crate::dialogs::accounts::OFFLINE_SLIDER).rect();
    let at = egui::pos2(rect.left() + 2.0, rect.center().y);
    h.event(egui::Event::PointerMoved(at));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        step(&mut h);
    }
    assert_eq!(offline_slider(&h).0, "3 days");
}

#[test]
fn pop_accounts_have_no_offline_window() {
    let mut app = sample_app();
    let port = closed_port();
    let v = app
        .s
        .run(
            "accounts.add",
            &json!({
                "kind": "pop3",
                "address": "lee@example.com",
                "incoming": {"host": "127.0.0.1", "port": port, "security": "none", "allowPlaintextAuth": true},
                "outgoing": {"host": "127.0.0.1", "port": port, "security": "none", "allowPlaintextAuth": true},
                "password": "pw-local-only",
            }),
        )
        .unwrap();
    let id = v["account"].as_u64().unwrap();
    crate::dialogs::open(&mut app, "addAccount", json!({"change": id}));
    let h = harness(app);
    assert!(h.query_by_label("POP Account Settings").is_some());
    assert!(h.query_by_label("Offline mail").is_none());
    assert!(h.query_by_label(crate::dialogs::accounts::OFFLINE_SLIDER).is_none());
}
