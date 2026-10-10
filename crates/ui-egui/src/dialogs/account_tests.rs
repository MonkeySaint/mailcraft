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

fn wait_for_report(h: &mut Harness<'_, App>, id: u32) {
    for _ in 0..200 {
        step(h);
        if h.state().s.sync.tests.contains_key(&AccountId(id)) {
            step(h);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("no test report");
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
    let item = *app.s.mailbox.mail.keys().next().unwrap();
    app.s.mailbox.mail.get_mut(&item).unwrap().folder = outbox;
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
    assert_eq!(h.state().s.mailbox.mail.get(&ItemId(item.0)).map(|m| m.folder), Some(outbox));
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
    let mut failed = false;
    for _ in 0..400 {
        h.run_steps(1);
        let st = h.state().s.sync_status();
        if !st.busy && st.accounts.iter().any(|a| a.account == id && a.error.is_some()) {
            failed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(failed, "the run against a closed port finishes with an error");
    h.run_steps(2);
    assert!(h.query_by_label("Retry").is_some());
}
