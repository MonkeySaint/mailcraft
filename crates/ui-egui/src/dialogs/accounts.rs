//! Account dialogs (File › Info › Account Settings, `plan/outlook/backstage.md`): Account Settings (Email and
//! Data Files tabs), the Add Account wizard (also Change Account), Test Account Settings, the Remove Account
//! confirmation and Send/Receive Progress.
//!
//! Everything acts through the `accounts.*`, `file.*` and `sendReceive.*` commands. Passwords typed here go
//! straight into the command params (the engine keeps them in its secret store) and are dropped with the
//! dialog state; nothing here prints or stores them.
//!
//! The wizard guesses `imap.<domain>` / `smtp.<domain>` with the standard ports (no provider tables, no
//! network lookups) and always shows the settings page so they can be checked. **Test Account Settings**
//! needs an account to test: in the Add wizard the first test adds the account, later tests update it, and
//! Cancel removes it again.

use egui::{Context, RichText, Ui};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn, btn_en, footer, heading, muted, primary, window, with_state};
use crate::App;
use mailcraft_engine::sync::TestReport;
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
                let mut kind = kind_label(a.kind).to_owned();
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
    } else {
        muted(ui, app, &format!("Passwords are kept in {loc} (readable only by you; not encrypted yet)."));
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
            self.in_host = format!("imap.{domain}");
            self.out_host = format!("smtp.{domain}");
        }
        self.in_sec = 0;
        self.in_port = default_port(false, 0);
        self.out_sec = 1;
        self.out_port = default_port(true, 1);
        self.in_port_set = false;
        self.out_port_set = false;
        self.in_user = self.address.trim().to_owned();
        self.guessed = true;
    }

    /// Fills the fields from an account (Change Account). Passwords are never read back, only whether one
    /// is saved.
    fn fill_from(&mut self, app: &App, a: &Account) {
        self.jmap = a.kind == AccountKind::Jmap;
        self.address.clone_from(&a.address);
        self.name.clone_from(&a.sender_name);
        self.account_name.clone_from(&a.display_name);
        self.has_secret = app.s.secrets.get(&mailcraft_engine::accounts::secret_key(a.id, false)).is_some();
        if let Some(i) = &a.incoming {
            self.in_host.clone_from(&i.host);
            self.in_sec = sec_index(i.security);
            self.in_port = if i.port == 0 { default_port(false, self.in_sec) } else { i.port };
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
            && (self.change.is_some() || self.created.is_some() || !self.password.is_empty())
    }

    /// Params for `accounts.add` (no `account`) or `accounts.edit` (with it). Empty passwords are left out
    /// (Change keeps the saved one).
    fn params(&self, account: Option<u32>) -> Value {
        let user = if self.in_user.trim().is_empty() { self.address.trim() } else { self.in_user.trim() };
        let mut p = json!({
            "kind": if self.jmap { "jmap" } else { "imap" },
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
    let mut pick: Option<bool> = None;
    ui.horizontal(|ui| {
        for (jmap, label, desc) in [
            (false, "IMAP", "Reads mail with IMAP and sends it with SMTP (most providers)."),
            (true, "JMAP", "One JSON-over-HTTPS server for reading and sending (RFC 8620/8621)."),
        ] {
            let r = ui
                .add_sized([120.0, 60.0], egui::Button::new(RichText::new(label).size(16.0)))
                .on_hover_text(desc);
            if r.clicked() {
                pick = Some(jmap);
            }
        }
    });
    if let Some(jmap) = pick {
        st.jmap = jmap;
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
    let kind = if st.jmap { "JMAP" } else { "IMAP" };
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
                grid_edit(ui, "Server:", &mut st.in_host, "imap.example.com", false);
                st.in_port_set |= grid_port(ui, "Port:", &mut st.in_port);
                if security_row(ui, "Encryption method:", ("mc-wiz-insec", uid), &mut st.in_sec)
                    && !st.in_port_set
                {
                    st.in_port = default_port(false, st.in_sec);
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

fn page_done(ui: &mut Ui, app: &mut App, st: &mut Wizard) -> Out {
    let text =
        if st.change.is_some() { "Account successfully updated" } else { "Account successfully added" };
    heading(ui, app, text);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(18.0), egui::Sense::hover());
        app.icons.paint(ui, r, "check", app.theme.accent);
        ui.label(format!("{}  ({})", st.address.trim(), if st.jmap { "JMAP" } else { "IMAP" }));
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
