//! Account dialogs (File › Info › Account Settings, `plan/outlook/backstage.md`): Account Settings (Email and
//! Data Files tabs), the Add Account wizard (also Change Account), Test Account Settings, the Remove Account
//! confirmation and Send/Receive Progress.
//!
//! Everything acts through the `accounts.*`, `file.*` and `sendReceive.*` commands. Passwords typed here go
//! straight into the command params (the engine keeps them in its secret store) and are dropped with the
//! dialog state; nothing here prints or stores them.
//!
//! The wizard guesses `imap.<domain>` (or `pop.<domain>`) / `smtp.<domain>` with the standard ports (no provider
//! tables, no network lookups) and always shows the settings page so they can be checked. Its More Settings
//! section holds the per-account options (POP3 server copies, download mode, Send/Receive group membership and
//! interval, push), and Sign in with OAuth 2.0 runs `accounts.oauthSignIn` with a client id the user registered
//! (MailCraft ships none; see `crates/net/data/README.md`). **Test Account Settings**
//! needs an account to test: in the Add wizard the first test adds the account, later tests update it, and
//! Cancel removes it again.

use egui::{Context, RichText, Ui};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn, btn_en, footer, heading, muted, primary, window, with_state};
use crate::App;
use mailcraft_engine::accounts::{AccountOptions, DownloadMode};
use mailcraft_engine::sync::{PushMode, TestReport};
use mailcraft_model::{Account, AccountId, AccountKind, Security};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "accountSettings" => account_settings(ctx, app, d),
        "addAccount" => wizard(ctx, app, d),
        "testAccount" => test_account(ctx, app, d),
        "confirmRemoveAccount" => confirm_remove(ctx, app, d),
        "sendReceiveProgress" => progress(ctx, app, d),
        _ => return None,
    })
}

/// Runs a command, returning its error text instead of raising a toast (dialogs show errors inline).
fn try_run(app: &mut App, id: &str, params: Value) -> Result<Value, String> {
    app.s.run(id, &params).map_err(|e| e.to_string())
}

/// The account type shown in Account Settings (POP3 accounts are IMAP-kind accounts with the POP3 option).
fn account_type(app: &App, a: &Account) -> &'static str {
    if mailcraft_engine::accounts::kind_label(&app.s, a) == "pop3" { "POP/SMTP" } else { kind_label(a.kind) }
}

pub(crate) fn kind_label(k: AccountKind) -> &'static str {
    match k {
        AccountKind::Local => "Data file (local)",
        AccountKind::Imap => "IMAP/SMTP",
        AccountKind::Jmap => "JMAP",
        AccountKind::Exchange => "Exchange",
    }
}

fn is_email(a: &Account) -> bool {
    a.kind != AccountKind::Local
}

fn account_of(app: &App, id: u32) -> Option<Account> {
    app.s.mailbox.accounts.iter().find(|a| a.id.0 == id).cloned()
}

/// A label + single-line text field row inside a two-column grid; the field is labelled by the label (for
/// screen readers and tests).
fn grid_edit(ui: &mut Ui, label: &str, value: &mut String, hint: &str, password: bool) -> egui::Response {
    let l = ui.label(label);
    let r = ui
        .add(egui::TextEdit::singleline(value).hint_text(hint).password(password).desired_width(280.0))
        .labelled_by(l.id);
    ui.end_row();
    r
}

/// A label + port field row; returns true when the person changed the port.
fn grid_port(ui: &mut Ui, label: &str, port: &mut u16) -> bool {
    let l = ui.label(label);
    let changed = ui.add(egui::DragValue::new(port).range(1..=65_535)).labelled_by(l.id).changed();
    ui.end_row();
    changed
}

// ---------------------------------------------------------------------------------------------------------
// Account Settings

#[derive(Clone, Default)]
struct SettingsState {
    tab: usize,
    /// Selected row on the Email tab (an email account).
    email: Option<u32>,
    /// Selected row on the Data Files tab (any account).
    data: Option<u32>,
    data_name: String,
    error: String,
}

const SETTINGS_TABS: [&str; 5] = ["Email", "Data Files", "RSS Feeds", "Internet Calendars", "Address Books"];

fn account_settings(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let first = d
        .get("account")
        .and_then(|a| a.get("id").or(Some(a)))
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok());
    let tab = d.u64("tab").and_then(|t| usize::try_from(t).ok()).unwrap_or(0).min(SETTINGS_TABS.len() - 1);
    let init = |app: &App| {
        let accounts = &app.s.mailbox.accounts;
        let email = first
            .filter(|id| accounts.iter().any(|a| a.id.0 == *id && is_email(a)))
            .or_else(|| accounts.iter().find(|a| is_email(a)).map(|a| a.id.0));
        SettingsState { tab, email, data: first, ..Default::default() }
    };
    with_state(ctx, app, d.uid, init, |st, app| {
        window(ctx, app, d, "Account Settings", 660.0, |ui, app| {
            ui.horizontal(|ui| {
                for (i, t) in SETTINGS_TABS.iter().enumerate() {
                    if ui.selectable_label(st.tab == i, *t).clicked() && st.tab != i {
                        st.tab = i;
                        st.error.clear();
                    }
                }
            });
            ui.separator();
            let out = match st.tab {
                0 => email_tab(ui, app, st),
                1 => data_files_tab(ui, app, st),
                _ => {
                    muted(ui, app, "Nothing is set up here yet.");
                    ui.add_space(200.0);
                    Out::Keep
                }
            };
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            if !matches!(out, Out::Keep) {
                return out;
            }
            footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
        })
    })
}

fn email_tab(ui: &mut Ui, app: &mut App, st: &mut SettingsState) -> Out {
    muted(ui, app, "You can add or remove an account. You can select an account and change its settings.");
    let accounts: Vec<Account> = app.s.mailbox.accounts.iter().filter(|a| is_email(a)).cloned().collect();
    if st.email.is_some_and(|id| !accounts.iter().any(|a| a.id.0 == id)) {
        st.email = accounts.first().map(|a| a.id.0);
    }
    let sel = st.email.and_then(|id| accounts.iter().find(|a| a.id.0 == id)).cloned();
    let mut out = Out::Keep;
    ui.horizontal(|ui| {
        if ui.button("New...").clicked() {
            out = Out::open("addAccount", json!({}));
        }
        if ui.add_enabled(sel.is_some(), egui::Button::new("Repair...")).clicked()
            && let Some(a) = &sel
        {
            out = Out::open("testAccount", json!({"account": a.id.0, "repair": true}));
        }
        if ui.add_enabled(sel.is_some(), egui::Button::new("Change...")).clicked()
            && let Some(a) = &sel
        {
            out = Out::open("addAccount", json!({"change": a.id.0}));
        }
        let can_default = sel.as_ref().is_some_and(|a| !a.is_default);
        if ui.add_enabled(can_default, egui::Button::new("Set as Default")).clicked()
            && let Some(a) = &sel
        {
            st.error =
                try_run(app, "accounts.setDefault", json!({"account": a.id.0})).err().unwrap_or_default();
        }
        if ui.add_enabled(sel.is_some(), egui::Button::new("Remove")).clicked()
            && let Some(a) = &sel
        {
            // Without `confirm` the engine only asks (it opens confirmRemoveAccount).
            st.error = try_run(app, "accounts.remove", json!({"account": a.id.0})).err().unwrap_or_default();
        }
    });
    list_frame(ui, app, |ui| {
        egui::Grid::new("mc-acct-email").num_columns(2).striped(true).spacing([24.0, 4.0]).show(ui, |ui| {
            ui.label(RichText::new("Name").strong());
            ui.label(RichText::new("Type").strong());
            ui.end_row();
            for a in &accounts {
                if ui.selectable_label(st.email == Some(a.id.0), common::account_title(a)).clicked() {
                    st.email = Some(a.id.0);
                }
                let mut kind = account_type(app, a).to_owned();
                if a.is_default {
                    kind.push_str(" (send from this account by default)");
                }
                ui.label(kind);
                ui.end_row();
            }
        });
        if accounts.is_empty() {
            ui.add_space(8.0);
            muted(ui, app, "No email account is set up. Click New... to add one.");
        }
    });
    if let Some(a) = &sel {
        ui.label(format!("Selected account delivers new messages to: {} › Inbox", a.display_name));
    }
    let loc = app.s.secrets.location();
    if loc == "memory" {
        muted(ui, app, "Passwords are kept in memory and forgotten when MailCraft closes.");
    } else if app.s.secrets.encrypted() {
        muted(ui, app, &format!("Passwords and sign-in tokens are kept in the {loc}."));
    } else {
        muted(ui, app, &format!("Passwords are kept in {loc}: readable only by you, but NOT encrypted."));
        if ui
            .button("Use System Keyring")
            .on_hover_text(
                "Move passwords and sign-in tokens into the operating system's encrypted credential store",
            )
            .clicked()
        {
            st.error =
                try_run(app, "accounts.useKeyring", json!({"confirm": true})).err().unwrap_or_default();
        }
    }
    out
}

fn data_files_tab(ui: &mut Ui, app: &mut App, st: &mut SettingsState) -> Out {
    muted(ui, app, "Data files hold your folders and items on this computer.");
    let accounts = app.s.mailbox.accounts.clone();
    if st.data.is_some_and(|id| !accounts.iter().any(|a| a.id.0 == id)) {
        st.data = None;
    }
    let sel = st.data.and_then(|id| accounts.iter().find(|a| a.id.0 == id)).cloned();
    let primary_local = accounts.iter().find(|a| a.kind == AccountKind::Local).map(|a| a.id);
    let mut out = Out::Keep;
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut st.data_name).hint_text("New data file name").desired_width(180.0));
        if ui.button("Add...").clicked() {
            let name = st.data_name.trim().to_owned();
            let p = if name.is_empty() { json!({}) } else { json!({"name": name}) };
            st.error = try_run(app, "file.newDataFile", p).err().unwrap_or_default();
            st.data_name.clear();
        }
        let removable = sel.as_ref().is_some_and(|a| a.kind == AccountKind::Local && Some(a.id) != primary_local);
        if ui.add_enabled(removable, egui::Button::new("Remove")).clicked()
            && let Some(a) = &sel
        {
            out = Out::open(
                "confirm",
                json!({"title": "Account Settings", "text": format!("Close the data file \"{}\"? Its folders and items are removed from this profile.", a.display_name), "command": "file.closeDataFile", "params": {"store": a.id.0}}),
            );
        }
        if ui.add_enabled(app.s.store.is_some(), egui::Button::new("Open File Location...")).clicked() {
            st.error = try_run(app, "file.openFileLocation", json!({})).err().unwrap_or_default();
        }
    });
    let location = app.s.store.as_ref().map(|s| s.dir.display().to_string());
    list_frame(ui, app, |ui| {
        egui::Grid::new("mc-acct-data").num_columns(2).striped(true).spacing([24.0, 4.0]).show(ui, |ui| {
            ui.label(RichText::new("Name").strong());
            ui.label(RichText::new("Location").strong());
            ui.end_row();
            for a in &accounts {
                if ui.selectable_label(st.data == Some(a.id.0), a.display_name.as_str()).clicked() {
                    st.data = Some(a.id.0);
                }
                let what = if is_email(a) { "offline copy of the mailbox" } else { "data file" };
                ui.label(match &location {
                    Some(dir) => format!("{dir} ({what})"),
                    None => format!("In memory, not saved ({what})"),
                });
                ui.end_row();
            }
        });
    });
    out
}

fn list_frame(ui: &mut Ui, app: &App, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(ui, |ui| {
        ui.set_min_height(200.0);
        ui.set_min_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt("mc-acct-list").max_height(260.0).show(ui, add);
    });
}

// ---------------------------------------------------------------------------------------------------------
// Add Account / Change Account wizard

/// Encryption choices: label, engine name.
const SECURITY: [(&str, &str); 3] = [("SSL/TLS", "tls"), ("STARTTLS", "starttls"), ("None", "none")];

fn sec_index(s: Security) -> usize {
    match s {
        Security::Tls => 0,
        Security::StartTls => 1,
        Security::None => 2,
    }
}

fn security_name(i: usize) -> &'static str {
    SECURITY.get(i).map_or("tls", |s| s.1)
}

/// Standard port for an encryption choice (RFC 8314: 993/465 with TLS; 143 and 587 for STARTTLS or none).
fn default_port(outgoing: bool, sec: usize) -> u16 {
    match (outgoing, security_name(sec)) {
        (false, "tls") => 993,
        (false, _) => 143,
        (true, "tls") => 465,
        (true, _) => 587,
    }
}

/// Opt-in for sending the password over a connection without encryption.
const ALLOW_PLAIN: &str =
    "Allow sign-in without encryption (only for a server you trust, such as one on this computer)";

const PAGE_EMAIL: u8 = 0;
const PAGE_TYPE: u8 = 1;
const PAGE_SETTINGS: u8 = 2;
const PAGE_DONE: u8 = 3;

/// Download Preferences choices: label, engine name.
const DOWNLOAD: [(&str, &str); 3] = [
    ("Download full items", "full"),
    ("Download headers and then full items", "headersThenFull"),
    ("Download headers only", "headers"),
];

fn download_index(m: DownloadMode) -> usize {
    match m {
        DownloadMode::Full => 0,
        DownloadMode::HeadersThenFull => 1,
        DownloadMode::Headers => 2,
    }
}

/// The More Settings and OAuth part of the wizard.
#[derive(Clone)]
struct Extra {
    leave_on_server: bool,
    remove_days_on: bool,
    remove_days: u32,
    remove_when_deleted: bool,
    include: bool,
    interval_own: bool,
    interval: u32,
    push: bool,
    download: usize,
    oauth_open: bool,
    providers: Vec<(String, String, bool)>,
    provider: usize,
    client_id: String,
    client_secret: String,
    device: bool,
    oauth_error: String,
}

impl Default for Extra {
    fn default() -> Self {
        let o = AccountOptions::default();
        Extra::from_options(&o)
    }
}

impl Extra {
    fn from_options(o: &AccountOptions) -> Extra {
        Extra {
            leave_on_server: o.leave_on_server,
            remove_days_on: o.remove_after_days.is_some(),
            remove_days: o.remove_after_days.unwrap_or(14),
            remove_when_deleted: o.remove_when_deleted,
            include: o.include_in_send_receive,
            interval_own: o.interval_minutes.is_some(),
            interval: o.interval_minutes.unwrap_or(30),
            push: o.push,
            download: download_index(o.download),
            oauth_open: false,
            providers: Vec::new(),
            provider: 0,
            client_id: o.oauth.as_ref().map(|b| b.client_id.clone()).unwrap_or_default(),
            client_secret: String::new(),
            device: true,
            oauth_error: String::new(),
        }
    }
}

#[derive(Clone, Default)]
struct Wizard {
    page: u8,
    /// Change Account: the account being changed.
    change: Option<u32>,
    /// Add Account: the account a Test Account Settings run added (removed again on Cancel).
    created: Option<u32>,
    finished: bool,
    manual: bool,
    jmap: bool,
    /// Incoming mail over POP3.
    pop3: bool,
    extra: Extra,
    name: String,
    address: String,
    account_name: String,
    in_host: String,
    in_port: u16,
    in_sec: usize,
    in_user: String,
    out_host: String,
    out_port: u16,
    out_sec: usize,
    out_separate: bool,
    out_user: String,
    out_password: String,
    password: String,
    /// Sign in over a connection without encryption (encryption None): an explicit choice per account.
    allow_plain: bool,
    has_secret: bool,
    /// Ports typed by hand are kept when the encryption changes.
    in_port_set: bool,
    out_port_set: bool,
    guessed: bool,
    error: String,
}

impl Wizard {
    fn domain(&self) -> String {
        self.address.trim().rsplit_once('@').map(|(_, d)| d.trim().to_ascii_lowercase()).unwrap_or_default()
    }

    fn address_ok(&self) -> bool {
        let a = self.address.trim();
        a.rsplit_once('@').is_some_and(|(l, d)| !l.is_empty() && !d.is_empty() && !d.contains('@'))
            && !a.chars().any(|c| c.is_whitespace() || c.is_control())
    }

    /// Settings guessed from the address: `imap.<domain>` / `smtp.<domain>` with the standard ports, or the
    /// RFC 8620 well-known JMAP session URL; the address as user name.
    fn guess(&mut self) {
        let domain = self.domain();
        if self.jmap {
            self.in_host = format!("https://{domain}/.well-known/jmap");
        } else {
            self.in_host = if self.pop3 { format!("pop.{domain}") } else { format!("imap.{domain}") };
            self.out_host = format!("smtp.{domain}");
        }
        self.in_sec = 0;
        self.in_port = self.in_default_port(0);
        self.out_sec = 1;
        self.out_port = default_port(true, 1);
        self.in_port_set = false;
        self.out_port_set = false;
        self.in_user = self.address.trim().to_owned();
        self.guessed = true;
    }

    /// Standard incoming port: IMAP 993/143, POP3 995/110.
    fn in_default_port(&self, sec: usize) -> u16 {
        match (self.pop3, security_name(sec)) {
            (true, "tls") => 995,
            (true, _) => 110,
            (false, _) => default_port(false, sec),
        }
    }

    fn kind_name(&self) -> &'static str {
        if self.jmap {
            "JMAP"
        } else if self.pop3 {
            "POP"
        } else {
            "IMAP"
        }
    }

    /// Fills the fields from an account (Change Account). Passwords are never read back, only whether one
    /// is saved.
    fn fill_from(&mut self, app: &App, a: &Account) {
        self.jmap = a.kind == AccountKind::Jmap;
        self.pop3 = mailcraft_engine::accounts::kind_label(&app.s, a) == "pop3";
        self.extra = Extra::from_options(&mailcraft_engine::accounts::account_options(&app.s, a.id));
        self.address.clone_from(&a.address);
        self.name.clone_from(&a.sender_name);
        self.account_name.clone_from(&a.display_name);
        self.has_secret = app.s.secrets.get(&mailcraft_engine::accounts::secret_key(a.id, false)).is_some();
        if let Some(i) = &a.incoming {
            self.in_host.clone_from(&i.host);
            self.in_sec = sec_index(i.security);
            self.in_port = if i.port == 0 { self.in_default_port(self.in_sec) } else { i.port };
            self.in_user.clone_from(&i.username);
            self.in_port_set = true;
            self.allow_plain |= i.allow_plaintext_auth;
        }
        match &a.outgoing {
            Some(o) => {
                self.out_host.clone_from(&o.host);
                self.out_sec = sec_index(o.security);
                self.out_port = if o.port == 0 { default_port(true, self.out_sec) } else { o.port };
                self.out_user.clone_from(&o.username);
                self.out_separate = !self.out_user.is_empty() && self.out_user != self.in_user;
                self.out_port_set = true;
                self.allow_plain |= o.allow_plaintext_auth;
            }
            None => {
                self.out_sec = 1;
                self.out_port = default_port(true, 1);
            }
        }
    }

    /// A server is set to encryption None (JMAP: an `http:` session URL).
    fn unencrypted(&self) -> bool {
        if self.jmap {
            return self.in_host.trim().to_ascii_lowercase().starts_with("http:");
        }
        security_name(self.in_sec) == "none" || security_name(self.out_sec) == "none"
    }

    fn settings_ok(&self) -> bool {
        self.address_ok()
            && !self.in_host.trim().is_empty()
            && (self.jmap || !self.out_host.trim().is_empty())
            && (self.change.is_some()
                || self.created.is_some()
                || !self.password.is_empty()
                || self.extra.oauth_open)
    }

    /// The More Settings options as command params.
    fn option_params(&self, o: &mut serde_json::Map<String, Value>) {
        let e = &self.extra;
        o.insert("mode".into(), json!(DOWNLOAD.get(e.download).map_or("full", |d| d.1)));
        o.insert("include".into(), json!(e.include));
        o.insert("intervalMinutes".into(), if e.interval_own { json!(e.interval) } else { Value::Null });
        if self.pop3 {
            o.insert("leaveOnServer".into(), json!(e.leave_on_server));
            o.insert(
                "removeAfterDays".into(),
                if e.remove_days_on { json!(e.remove_days.max(1)) } else { Value::Null },
            );
            o.insert("removeWhenDeleted".into(), json!(e.remove_when_deleted));
        } else if !self.jmap {
            o.insert("push".into(), json!(e.push));
        }
    }

    /// Params for `accounts.add` (no `account`) or `accounts.edit` (with it). Empty passwords are left out
    /// (Change keeps the saved one).
    fn params(&self, account: Option<u32>) -> Value {
        let user = if self.in_user.trim().is_empty() { self.address.trim() } else { self.in_user.trim() };
        let mut p = json!({
            "kind": if self.jmap { "jmap" } else if self.pop3 { "pop3" } else { "imap" },
            "address": self.address.trim(),
            "senderName": self.name.trim(),
            "incoming": {
                "host": self.in_host.trim(),
                "port": self.in_port,
                "security": security_name(self.in_sec),
                "username": user,
                "allowPlaintextAuth": self.allow_plain
                    && if self.jmap { self.unencrypted() } else { security_name(self.in_sec) == "none" },
            },
        });
        let Some(o) = p.as_object_mut() else { return p };
        if !self.account_name.trim().is_empty() {
            o.insert("displayName".into(), json!(self.account_name.trim()));
        }
        if !self.jmap {
            let out_user = if self.out_separate && !self.out_user.trim().is_empty() {
                self.out_user.trim()
            } else {
                user
            };
            o.insert(
                "outgoing".into(),
                json!({
                    "host": self.out_host.trim(),
                    "port": self.out_port,
                    "security": security_name(self.out_sec),
                    "username": out_user,
                    "allowPlaintextAuth": self.allow_plain && security_name(self.out_sec) == "none",
                }),
            );
        }
        if !self.password.is_empty() {
            o.insert("password".into(), json!(self.password));
        }
        if self.out_separate && !self.out_password.is_empty() {
            o.insert("outgoingPassword".into(), json!(self.out_password));
        }
        self.option_params(o);
        if let Some(id) = account {
            o.remove("kind");
            o.insert("account".into(), json!(id));
        }
        p
    }

    /// Adds the account (first time) or updates the one this wizard made or changes. Returns its id.
    fn save(&mut self, app: &mut App) -> Result<u32, String> {
        match self.change.or(self.created) {
            Some(id) => {
                try_run(app, "accounts.edit", self.params(Some(id)))?;
                Ok(id)
            }
            None => {
                let v = try_run(app, "accounts.add", self.params(None))?;
                let id = v
                    .get("account")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| "the account was not added".to_owned())?;
                self.created = Some(id);
                Ok(id)
            }
        }
    }
}

fn wizard(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let change = d.u64("change").and_then(|v| u32::try_from(v).ok());
    let init = |app: &App| {
        let mut w = Wizard { name: d.str("name"), address: d.str("address"), ..Default::default() };
        if let Some(a) = change.and_then(|id| account_of(app, id)).filter(is_email) {
            w.change = Some(a.id.0);
            w.page = PAGE_SETTINGS;
            w.fill_from(app, &a);
        }
        if w.name.is_empty() {
            w.name = app
                .s
                .mailbox
                .accounts
                .iter()
                .find(|a| !a.sender_name.is_empty())
                .map(|a| a.sender_name.clone())
                .unwrap_or_default();
        }
        w
    };
    let mut cleanup: Option<u32> = None;
    let out = with_state(ctx, app, d.uid, init, |st, app| {
        let title = if st.change.is_some() { "Change Account" } else { "Add Account" };
        let out = window(ctx, app, d, title, 560.0, |ui, app| match st.page {
            PAGE_EMAIL => page_email(ui, app, st),
            PAGE_TYPE => page_type(ui, app, st),
            PAGE_SETTINGS => page_settings(ui, app, st, d.uid),
            _ => page_done(ui, app, st),
        });
        if !out.keeps() && !st.finished {
            cleanup = st.created;
        }
        out
    });
    if let Some(id) = cleanup
        && let Err(e) = try_run(app, "accounts.remove", json!({"account": id, "confirm": true}))
    {
        log::warn!("could not remove the account added for testing: {e}");
    }
    out
}

fn page_email(ui: &mut Ui, app: &mut App, st: &mut Wizard) -> Out {
    heading(ui, app, "Add an email account");
    let mut enter = false;
    egui::Grid::new("mc-wiz-email").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
        let r = grid_edit(ui, "Email address:", &mut st.address, "name@example.com", false);
        enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        grid_edit(ui, "Your name:", &mut st.name, "Shown to people you send mail to", false);
    });
    ui.checkbox(&mut st.manual, "Let me set up my account manually");
    muted(
        ui,
        app,
        "MailCraft suggests server settings from your address; you can check and change them on the next page.",
    );
    if !st.error.is_empty() {
        ui.colored_label(app.theme.danger, &st.error);
    }
    let ok = st.address_ok();
    footer(ui, |ui| {
        if btn(ui, "Cancel") {
            return Out::Close;
        }
        if primary(ui, app, ok, "Connect") || (ok && enter) {
            st.error.clear();
            if st.manual {
                st.page = PAGE_TYPE;
            } else {
                st.jmap = false;
                st.pop3 = false;
                st.guess();
                st.page = PAGE_SETTINGS;
            }
        }
        Out::Keep
    })
}

fn page_type(ui: &mut Ui, app: &mut App, st: &mut Wizard) -> Out {
    heading(ui, app, "Advanced setup");
    ui.label(format!("Choose the account type for {}:", st.address.trim()));
    ui.add_space(6.0);
    let mut pick: Option<(bool, bool)> = None;
    ui.horizontal(|ui| {
        for (jmap, pop3, label, desc) in [
            (false, false, "IMAP", "Reads mail with IMAP and sends it with SMTP (most providers)."),
            (
                false,
                true,
                "POP",
                "Downloads mail with POP3 and sends it with SMTP; folders stay on this computer.",
            ),
            (true, false, "JMAP", "One JSON-over-HTTPS server for reading and sending (RFC 8620/8621)."),
        ] {
            let r = ui
                .add_sized([120.0, 60.0], egui::Button::new(RichText::new(label).size(16.0)))
                .on_hover_text(desc);
            if r.clicked() {
                pick = Some((jmap, pop3));
            }
        }
    });
    if let Some((jmap, pop3)) = pick {
        st.jmap = jmap;
        st.pop3 = pop3;
        st.guess();
        st.page = PAGE_SETTINGS;
    }
    footer(ui, |ui| {
        if btn(ui, "Cancel") {
            return Out::Close;
        }
        if btn(ui, "< Back") {
            st.page = PAGE_EMAIL;
        }
        Out::Keep
    })
}

fn security_row(ui: &mut Ui, label: &str, salt: (&str, u64), sec: &mut usize) -> bool {
    let l = ui.label(label);
    let names: Vec<&str> = SECURITY.iter().map(|s| s.0).collect();
    let r = ui.scope(|ui| common::combo_idx(ui, salt, sec, &names, 120.0));
    let _ = r.response.labelled_by(l.id);
    ui.end_row();
    r.inner
}

fn page_settings(ui: &mut Ui, app: &mut App, st: &mut Wizard, uid: u64) -> Out {
    let kind = st.kind_name();
    heading(ui, app, &format!("{kind} Account Settings"));
    ui.horizontal(|ui| {
        ui.label(RichText::new(st.address.trim()).strong());
        if st.change.is_none() && ui.link("(Not you?)").clicked() {
            st.page = PAGE_EMAIL;
        }
    });
    if st.guessed && st.change.is_none() {
        muted(ui, app, "These settings were guessed from your address. Check them with your email provider.");
    }
    egui::ScrollArea::vertical().id_salt(("mc-wiz-settings", uid)).max_height(420.0).show(ui, |ui| {
        if st.change.is_some() {
            common::heading(ui, app, "Account");
            egui::Grid::new(("mc-wiz-acct", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                grid_edit(ui, "Account name:", &mut st.account_name, "", false);
                grid_edit(ui, "Your name:", &mut st.name, "", false);
            });
        }
        common::heading(ui, app, if st.jmap { "Server" } else { "Incoming mail" });
        egui::Grid::new(("mc-wiz-in", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            if st.jmap {
                grid_edit(ui, "Session URL:", &mut st.in_host, "https://example.com/.well-known/jmap", false);
            } else {
                let hint = if st.pop3 { "pop.example.com" } else { "imap.example.com" };
                grid_edit(ui, "Server:", &mut st.in_host, hint, false);
                st.in_port_set |= grid_port(ui, "Port:", &mut st.in_port);
                if security_row(ui, "Encryption method:", ("mc-wiz-insec", uid), &mut st.in_sec)
                    && !st.in_port_set
                {
                    st.in_port = st.in_default_port(st.in_sec);
                }
            }
            grid_edit(ui, "User name:", &mut st.in_user, "Usually your email address", false);
            let hint = if st.has_secret || st.created.is_some() {
                "Leave empty to keep the saved password"
            } else {
                ""
            };
            grid_edit(ui, "Password:", &mut st.password, hint, true);
        });
        if !st.jmap {
            common::heading(ui, app, "Outgoing mail (SMTP)");
            egui::Grid::new(("mc-wiz-out", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                grid_edit(ui, "Outgoing server:", &mut st.out_host, "smtp.example.com", false);
                st.out_port_set |= grid_port(ui, "Outgoing port:", &mut st.out_port);
                if security_row(ui, "Outgoing encryption:", ("mc-wiz-outsec", uid), &mut st.out_sec)
                    && !st.out_port_set
                {
                    st.out_port = default_port(true, st.out_sec);
                }
            });
            ui.checkbox(&mut st.out_separate, "The outgoing server uses a different user name and password");
            if st.out_separate {
                egui::Grid::new(("mc-wiz-outuser", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    grid_edit(ui, "Outgoing user name:", &mut st.out_user, "", false);
                    grid_edit(ui, "Outgoing password:", &mut st.out_password, "", true);
                });
            }
        }
        more_settings(ui, app, st, uid);
        if let Some(o) = oauth_section(ui, app, st, uid) {
            ui.ctx().open_url(egui::OpenUrl::new_tab(o));
        }
    });
    if st.unencrypted() {
        // Without this the password is never sent in clear text, so the sign-in fails.
        ui.checkbox(&mut st.allow_plain, ALLOW_PLAIN);
    }
    if st.change.is_some() {
        muted(ui, app, "Test Account Settings saves these settings first.");
    }
    if !st.error.is_empty() {
        ui.colored_label(app.theme.danger, &st.error);
    }
    let ok = st.settings_ok();
    let mut out = Out::Keep;
    footer(ui, |ui| {
        if btn(ui, "Cancel") {
            out = Out::Close;
        }
        let label = if st.change.is_some() { "Save" } else { "Connect" };
        if primary(ui, app, ok, label) {
            match st.save(app) {
                Ok(_) => {
                    st.error.clear();
                    st.finished = true;
                    st.page = PAGE_DONE;
                }
                Err(e) => st.error = e,
            }
        }
        if btn_en(ui, ok, "Test Account Settings...") {
            match st.save(app) {
                Ok(id) => {
                    st.error.clear();
                    out = Out::open("testAccount", json!({"account": id}));
                }
                Err(e) => st.error = e,
            }
        }
        if st.change.is_none() && btn(ui, "< Back") {
            st.page = if st.manual { PAGE_TYPE } else { PAGE_EMAIL };
            st.error.clear();
        }
    });
    out
}

/// More Settings: POP3 server copies, download preference, Send/Receive group membership and interval, push.
fn more_settings(ui: &mut Ui, app: &mut App, st: &mut Wizard, uid: u64) {
    egui::CollapsingHeader::new("More Settings").id_salt(("mc-wiz-more", uid)).show(ui, |ui| {
        let e = &mut st.extra;
        if st.pop3 {
            ui.checkbox(&mut e.leave_on_server, "Leave a copy of messages on the server");
            ui.add_enabled_ui(e.leave_on_server, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut e.remove_days_on, "Remove from server after");
                    ui.add_enabled(
                        e.remove_days_on,
                        egui::DragValue::new(&mut e.remove_days).range(1..=3650),
                    );
                    ui.label("days");
                });
                ui.checkbox(&mut e.remove_when_deleted, "Remove from server when deleted from Deleted Items");
            });
        }
        ui.horizontal(|ui| {
            let l = ui.label("Download:");
            let names: Vec<&str> = DOWNLOAD.iter().map(|d| d.0).collect();
            let r = ui.scope(|ui| common::combo_idx(ui, ("mc-wiz-dl", uid), &mut e.download, &names, 260.0));
            let _ = r.response.labelled_by(l.id);
        });
        ui.checkbox(
            &mut e.include,
            "Include this account in Send/Receive All Folders (F9) and scheduled send/receive",
        );
        ui.horizontal(|ui| {
            ui.checkbox(&mut e.interval_own, "Send/receive this account every");
            ui.add_enabled(e.interval_own, egui::DragValue::new(&mut e.interval).range(1..=1440));
            ui.label("minutes");
        });
        if !st.pop3 && !st.jmap {
            ui.checkbox(&mut e.push, "Show new mail as soon as it arrives (push with IMAP IDLE)");
        }
        let _ = app;
    });
}

/// Sign in with OAuth 2.0: provider, the user's client id, device code or browser. Returns a page to open.
fn oauth_section(ui: &mut Ui, app: &mut App, st: &mut Wizard, uid: u64) -> Option<String> {
    let mut open_url = None;
    let r = egui::CollapsingHeader::new("Sign in with OAuth 2.0 instead of a password")
        .id_salt(("mc-wiz-oauth", uid))
        .open(if st.extra.oauth_open { Some(true) } else { None })
        .show(ui, |ui| {
            if st.extra.providers.is_empty() {
                let list = try_run(app, "accounts.oauthProviders", json!({})).unwrap_or_default();
                let mut v: Vec<(String, String, bool)> = list
                    .get("providers")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| {
                                let device = p.get("flows").and_then(Value::as_array).is_some_and(|f| f.iter().any(|x| x == "device"));
                                Some((p.get("id")?.as_str()?.to_owned(), p.get("name")?.as_str()?.to_owned(), device))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if v.is_empty() {
                    v.push((String::new(), "(none available)".into(), false));
                }
                // The provider serving the address first.
                let domain = st.domain();
                if let Some(i) = v.iter().position(|(id, _, _)| {
                    (id == "google" && (domain == "gmail.com" || domain == "googlemail.com"))
                        || (id == "microsoft" && ["outlook.com", "hotmail.com", "live.com", "msn.com"].contains(&domain.as_str()))
                }) {
                    st.extra.provider = i;
                }
                st.extra.providers = v;
            }
            muted(
                ui,
                app,
                "MailCraft has no built-in registration with any provider. Register an application with your provider and enter its client id (and client secret, if the provider issued one).",
            );
            let e = &mut st.extra;
            egui::Grid::new(("mc-wiz-oauth-grid", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                let l = ui.label("Provider:");
                let names: Vec<&str> = e.providers.iter().map(|p| p.1.as_str()).collect();
                let r = ui.scope(|ui| common::combo_idx(ui, ("mc-wiz-prov", uid), &mut e.provider, &names, 280.0));
                let _ = r.response.labelled_by(l.id);
                ui.end_row();
                grid_edit(ui, "Client id:", &mut e.client_id, "From your app registration", false);
                grid_edit(ui, "Client secret:", &mut e.client_secret, "Only if the provider issued one", true);
            });
            let device_ok = e.providers.get(e.provider).is_some_and(|p| p.2);
            if !device_ok {
                e.device = false;
            }
            ui.horizontal(|ui| {
                ui.add_enabled_ui(device_ok, |ui| ui.radio_value(&mut e.device, true, "Device code"));
                ui.radio_value(&mut e.device, false, "Browser");
            });
            st.extra.oauth_open = true;
            let id = st.change.or(st.created);
            let flow = id.and_then(|i| app.s.sync.oauth.get(&AccountId(i)).cloned());
            let running = flow.as_ref().is_some_and(|f| f.status == "waiting" || f.status == "starting");
            if running {
                ctx_repaint(ui);
            }
            ui.horizontal(|ui| {
                let ready = st.address_ok() && !st.extra.client_id.trim().is_empty() && !st.in_host.trim().is_empty();
                if ui.add_enabled(ready && !running, egui::Button::new("Sign In")).clicked() {
                    match st.save(app) {
                        Ok(id) => {
                            let e = &st.extra;
                            let mut p = json!({
                                "account": id,
                                "provider": e.providers.get(e.provider).map(|p| p.0.clone()).unwrap_or_default(),
                                "clientId": e.client_id.trim(),
                                "flow": if e.device { "device" } else { "browser" },
                            });
                            if !e.client_secret.is_empty()
                                && let Some(o) = p.as_object_mut()
                            {
                                o.insert("clientSecret".into(), json!(e.client_secret));
                            }
                            match try_run(app, "accounts.oauthSignIn", p) {
                                Ok(v) => {
                                    st.extra.oauth_error.clear();
                                    open_url = v.get("authUrl").and_then(Value::as_str).map(str::to_owned);
                                }
                                Err(e) => st.extra.oauth_error = e,
                            }
                        }
                        Err(e) => st.extra.oauth_error = e,
                    }
                }
                if running && ui.button("Cancel Sign-In").clicked()
                    && let Some(i) = id
                {
                    let _ = try_run(app, "accounts.oauthCancel", json!({"account": i}));
                }
            });
            if let Some(f) = &flow {
                match f.status.as_str() {
                    "waiting" | "starting" => {
                        if let (Some(code), Some(uri)) = (&f.user_code, &f.verification_uri) {
                            ui.label(format!("Go to {uri} on any device and enter this code:"));
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(code).strong().size(18.0));
                                if ui.button("Copy").clicked() {
                                    ui.ctx().copy_text(code.clone());
                                }
                                if ui.link("Open the page").clicked() {
                                    open_url = Some(f.verification_uri_complete.clone().unwrap_or_else(|| uri.clone()));
                                }
                            });
                        } else if let Some(u) = &f.auth_url {
                            ui.label("Finish signing in in your browser; MailCraft waits for it here.");
                            if ui.link("Open the sign-in page again").clicked() {
                                open_url = Some(u.clone());
                            }
                        } else {
                            ui.label("Contacting the provider…");
                        }
                    }
                    "done" => {
                        ui.colored_label(app.theme.accent, format!("Signed in with {}.", f.provider));
                    }
                    "cancelled" => muted(ui, app, "Sign-in was cancelled."),
                    _ => {
                        ui.colored_label(app.theme.danger, f.error.clone().unwrap_or_else(|| "Sign-in failed.".into()));
                    }
                }
            }
            if !st.extra.oauth_error.is_empty() {
                ui.colored_label(app.theme.danger, &st.extra.oauth_error);
            }
        });
    if r.body_returned.is_none() {
        st.extra.oauth_open = false;
    }
    open_url
}

fn ctx_repaint(ui: &Ui) {
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
}

fn page_done(ui: &mut Ui, app: &mut App, st: &mut Wizard) -> Out {
    let text =
        if st.change.is_some() { "Account successfully updated" } else { "Account successfully added" };
    heading(ui, app, text);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(18.0), egui::Sense::hover());
        app.icons.paint(ui, r, "check", app.theme.accent);
        ui.label(format!("{}  ({})", st.address.trim(), st.kind_name()));
    });
    if st.change.is_none() {
        muted(ui, app, "MailCraft downloads your folders and mail at the next send/receive.");
    }
    let mut out = Out::Keep;
    footer(ui, |ui| {
        if primary(ui, app, true, "Done") || common::enter_pressed(ui) {
            out = Out::Close;
        }
    });
    if !out.keeps() && st.change.is_none() && !app.s.view.work_offline {
        // Start the first send/receive of the new account (errors arrive in the status bar).
        let _ = try_run(app, "sendReceive.group", json!({"name": st.address.trim()}));
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Test Account Settings

#[derive(Clone, Default)]
struct TestState {
    started: bool,
    account: Option<u32>,
    report: Option<TestReport>,
    error: String,
    tab: usize,
}

fn test_account(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let wanted = d.u64("account").and_then(|v| u32::try_from(v).ok());
    let repair = d.bool("repair");
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| TestState::default(),
        |st, app| {
            if !st.started {
                st.started = true;
                st.account = wanted.and_then(|id| account_of(app, id)).filter(is_email).map(|a| a.id.0);
                if let Some(id) = st.account {
                    // Forget the previous result so the one shown is this run's.
                    app.s.sync.tests.remove(&AccountId(id));
                    if let Err(e) = try_run(app, "accounts.test", json!({"account": id})) {
                        st.error = e;
                    }
                } else {
                    st.error = "Choose an email account to test.".into();
                }
            }
            if st.report.is_none()
                && st.error.is_empty()
                && let Some(id) = st.account
            {
                // The app polls every frame; dialogs drawn without the shell (tests) poll here.
                app.s.poll_sync();
                st.report = app.s.sync.tests.get(&AccountId(id)).cloned();
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            let acct = st.account.and_then(|id| account_of(app, id));
            window(ctx, app, d, "Test Account Settings", 520.0, |ui, app| {
                let summary = match (&st.report, st.error.is_empty()) {
                    (_, false) => "The test could not run.".to_owned(),
                    (None, true) => "Testing the account settings…".to_owned(),
                    (Some(r), true) if r.ok() => {
                        "Congratulations! All tests completed successfully. Click Close to continue."
                            .to_owned()
                    }
                    (Some(_), true) => "Some tests did not complete. Check the errors below.".to_owned(),
                };
                ui.horizontal(|ui| {
                    if st.report.is_none() && st.error.is_empty() {
                        ui.add(egui::Spinner::new().size(14.0));
                    }
                    ui.label(summary);
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    for (i, t) in ["Tasks", "Errors"].iter().enumerate() {
                        if ui.selectable_label(st.tab == i, *t).clicked() {
                            st.tab = i;
                        }
                    }
                });
                let rows = test_rows(acct.as_ref(), st.report.as_ref(), &st.error);
                list_frame(ui, app, |ui| {
                    if st.tab == 0 {
                        egui::Grid::new("mc-test-tasks")
                            .num_columns(2)
                            .striped(true)
                            .spacing([24.0, 4.0])
                            .show(ui, |ui| {
                                ui.label(RichText::new("Tasks").strong());
                                ui.label(RichText::new("Status").strong());
                                ui.end_row();
                                for (task, status, _) in &rows {
                                    ui.label(task.as_str());
                                    ui.label(*status);
                                    ui.end_row();
                                }
                            });
                    } else {
                        let mut any = false;
                        for (task, status, detail) in &rows {
                            if *status == "Failed" {
                                any = true;
                                ui.label(RichText::new(task.as_str()).strong());
                                ui.colored_label(app.theme.danger, detail.as_str());
                                ui.add_space(4.0);
                            }
                        }
                        if !any {
                            muted(ui, app, "No errors.");
                        }
                    }
                });
                // What succeeded (failures are on the Errors tab).
                for (_, status, detail) in &rows {
                    if *status == "Completed" && !detail.is_empty() {
                        muted(ui, app, detail);
                    }
                }
                if st.tab == 0 && rows.iter().any(|r| r.1 == "Failed") {
                    muted(ui, app, "See the Errors tab for what went wrong.");
                }
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let failed = st.report.as_ref().is_some_and(|r| !r.ok()) || !st.error.is_empty();
                let mut out = Out::Keep;
                footer(ui, |ui| {
                    if btn(ui, "Close") {
                        out = Out::Close;
                    }
                    if repair
                        && failed
                        && let Some(id) = st.account
                        && btn(ui, "Change...")
                    {
                        out = Out::close_open("addAccount", json!({"change": id}));
                    }
                });
                out
            })
        },
    )
}

/// Task rows of a test: (task, status, detail).
fn test_rows(
    acct: Option<&Account>,
    r: Option<&TestReport>,
    error: &str,
) -> Vec<(String, &'static str, String)> {
    let jmap = acct.is_some_and(|a| a.kind == AccountKind::Jmap);
    let incoming = if jmap {
        "Sign in to the JMAP server".to_owned()
    } else {
        "Log onto incoming mail server (IMAP)".to_owned()
    };
    let mut rows = Vec::new();
    match r {
        None => {
            let status = if error.is_empty() { "In progress" } else { "Failed" };
            rows.push((incoming, status, error.to_owned()));
            if !jmap && acct.is_some_and(|a| a.outgoing.is_some()) {
                rows.push(("Log onto outgoing mail server (SMTP)".to_owned(), status, String::new()));
            }
        }
        Some(r) => {
            rows.push((incoming, if r.incoming_ok { "Completed" } else { "Failed" }, r.incoming.clone()));
            if let Some(ok) = r.outgoing_ok {
                let task = if jmap { "Check sending" } else { "Log onto outgoing mail server (SMTP)" };
                rows.push((task.to_owned(), if ok { "Completed" } else { "Failed" }, r.outgoing.clone()));
            }
        }
    }
    rows
}

// ---------------------------------------------------------------------------------------------------------
// Remove Account

fn confirm_remove(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let account = d.u64("account");
    let message = d.str("message");
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| String::new(),
        |error, app| {
            window(ctx, app, d, "Remove Account", 460.0, |ui, app| {
                let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(20.0), egui::Sense::hover());
                app.icons.paint(ui, r, "warning", app.theme.text_muted);
                ui.label(if message.is_empty() { "Remove this account?" } else { message.as_str() });
                ui.label("Do you want to continue?");
                if !error.is_empty() {
                    ui.colored_label(app.theme.danger, error.as_str());
                }
                footer(ui, |ui| {
                    if btn(ui, "No") {
                        return Out::Close;
                    }
                    if btn_en(ui, account.is_some(), "Yes")
                        && let Some(id) = account
                    {
                        match try_run(app, "accounts.remove", json!({"account": id, "confirm": true})) {
                            Ok(_) => return Out::Close,
                            Err(e) => *error = e,
                        }
                    }
                    Out::Keep
                })
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Send/Receive Progress

#[derive(Clone, Default)]
struct ProgressState {
    tab: usize,
}

fn progress(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let snapshot = d.arr("tasks");
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| ProgressState::default(),
        |st, app| {
            let status = app.s.sync_status().clone();
            if status.busy {
                ctx.request_repaint_after(std::time::Duration::from_millis(150));
            }
            window(ctx, app, d, "Send/Receive Progress", 520.0, |ui, app| {
                let total = status.accounts.len();
                let busy = status.accounts.iter().filter(|a| a.busy).count();
                let failed = status.accounts.iter().filter(|a| !a.busy && a.error.is_some()).count();
                let done = total.saturating_sub(busy).saturating_sub(failed);
                ui.horizontal(|ui| {
                    if status.busy {
                        ui.add(egui::Spinner::new().size(14.0));
                    }
                    ui.label(if total == 0 && snapshot.is_empty() {
                        "No email account is set up.".to_owned()
                    } else if status.busy {
                        format!("{done} of {total} tasks have completed successfully")
                    } else if failed > 0 {
                        format!("{failed} of {total} tasks reported errors")
                    } else {
                        "Send/Receive is complete.".to_owned()
                    });
                });
                ui.horizontal(|ui| {
                    for (i, t) in ["Tasks", "Errors"].iter().enumerate() {
                        if ui.selectable_label(st.tab == i, *t).clicked() {
                            st.tab = i;
                        }
                    }
                });
                list_frame(ui, app, |ui| {
                    if st.tab == 0 {
                        egui::Grid::new("mc-sr-tasks")
                            .num_columns(3)
                            .striped(true)
                            .spacing([20.0, 4.0])
                            .show(ui, |ui| {
                                ui.label(RichText::new("Name").strong());
                                ui.label(RichText::new("Progress").strong());
                                ui.label(RichText::new("Outbox").strong());
                                ui.end_row();
                                for a in &status.accounts {
                                    ui.label(a.name.as_str());
                                    let text = if a.busy {
                                        if a.progress.is_empty() {
                                            "In progress".to_owned()
                                        } else {
                                            a.progress.clone()
                                        }
                                    } else if a.error.is_some() {
                                        "Error".to_owned()
                                    } else if a.last_sync.is_some() && a.push != PushMode::Off {
                                        format!("Completed. {}", a.push.label())
                                    } else if a.last_sync.is_some() {
                                        "Completed".to_owned()
                                    } else {
                                        "Not started".to_owned()
                                    };
                                    ui.horizontal(|ui| {
                                        if a.busy {
                                            ui.add(egui::Spinner::new().size(12.0));
                                        }
                                        ui.label(text);
                                    });
                                    ui.label(if a.outbox > 0 { a.outbox.to_string() } else { String::new() });
                                    ui.end_row();
                                }
                                if status.accounts.is_empty() {
                                    for t in &snapshot {
                                        ui.label(t.get("name").and_then(Value::as_str).unwrap_or("Task"));
                                        ui.label(
                                            t.get("status").and_then(Value::as_str).unwrap_or("Completed"),
                                        );
                                        ui.label("");
                                        ui.end_row();
                                    }
                                }
                            });
                    } else {
                        let mut any = false;
                        for a in &status.accounts {
                            if let Some(e) = &a.error {
                                any = true;
                                ui.label(RichText::new(a.name.as_str()).strong());
                                ui.colored_label(app.theme.danger, e.as_str());
                                ui.add_space(4.0);
                            }
                        }
                        if !any {
                            muted(ui, app, "No errors.");
                        }
                    }
                });
                let mut hide = common::pref(ui.ctx(), "sendReceive.hideProgress")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if ui.checkbox(&mut hide, "Don't show this dialog box during Send/Receive").changed() {
                    common::set_pref(ui.ctx(), "sendReceive.hideProgress", json!(hide));
                }
                footer(ui, |ui| {
                    if btn(ui, "Close") {
                        return Out::Close;
                    }
                    if btn_en(ui, status.busy, "Cancel All") {
                        let _ = try_run(app, "sendReceive.cancelAll", json!({}));
                    }
                    Out::Keep
                })
            })
        },
    )
}
