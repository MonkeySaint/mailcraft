//! S/MIME dialogs (File › Options › Trust Center › Email Security; the reading pane's security info bar):
//!
//! - `digitalIds`: the user's digital IDs (import a .pfx / .p12 file or a PEM key and certificate, export,
//!   set the default, remove), other people's certificates (import, remove) and the certificates the user
//!   trusts (trust a file, stop trusting). Every button runs an `options.*` command; the lists are read from
//!   the engine each frame ([`mailcraft_engine::smime::summary`]).
//! - `messageSecurity`: what a message's signature and encryption say (`mail.signatureDetails`): the signer,
//!   the status and why, the signing time, the certificate chain; Trust lets a self-signed or private
//!   authority's signature count as valid.
//! - `encryptionProblem`: Send found recipients without a certificate: send without encryption, or cancel.

use egui::{Context, RichText};
use serde_json::{Value, json};

use crate::App;
use crate::dialogs::common::{
    self, Dlg, Out, btn, btn_en, footer, heading, muted, primary, window, with_state,
};

pub(super) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "digitalIds" => digital_ids(ctx, app, d),
        "messageSecurity" => message_security(ctx, app, d),
        "encryptionProblem" => encryption_problem(ctx, app, d),
        _ => return None,
    })
}

/// Which inline form the Digital IDs window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Form {
    #[default]
    None,
    ImportId,
    ExportId,
    ImportCert,
    TrustCert,
}

#[derive(Clone, Debug, Default)]
struct IdsState {
    form: Form,
    /// Selected rows (fingerprints) of the three lists.
    id: Option<String>,
    person: Option<(String, String)>,
    trusted: Option<String>,
    /// Import as key + certificate files instead of one .pfx file.
    pem: bool,
    path: String,
    key_path: String,
    password: String,
    password2: String,
    trust_issuer: bool,
    address: String,
    /// Result of the last action (error or confirmation).
    note: String,
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn run_note(app: &mut App, st: &mut IdsState, id: &str, params: Value, ok: &str) -> bool {
    match app.s.run(id, &params) {
        Ok(_) => {
            st.note = ok.to_owned();
            true
        }
        Err(e) => {
            st.note = e.to_string();
            false
        }
    }
}

fn file_row(
    ui: &mut egui::Ui,
    app: &App,
    label: &str,
    path: &mut String,
    save: bool,
    filters: &[(&str, &[&str])],
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::TextEdit::singleline(path).desired_width(300.0));
        if common::browse_button(ui, app) {
            let f: Vec<crate::FileFilter> =
                filters.iter().map(|(n, e)| crate::FileFilter::new(n, e)).collect();
            let picked = if save {
                common::browse_save(app, label, &f, path)
            } else {
                common::browse_open(app, label, &f, path)
            };
            if let Some(p) = picked {
                *path = p;
            }
        }
    });
}

fn password_row(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::TextEdit::singleline(value).password(true).desired_width(220.0));
    });
}

/// One selectable row of a certificate list; true when clicked.
fn cert_row(ui: &mut egui::Ui, selected: bool, cols: &[String]) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        for (i, c) in cols.iter().enumerate() {
            let w = if i == 0 { 200.0 } else { 150.0 };
            if ui.add_sized([w, 18.0], egui::Button::selectable(selected, c.as_str()).truncate()).clicked() {
                clicked = true;
            }
        }
    });
    clicked
}

fn list_frame(ui: &mut egui::Ui, app: &App, salt: &str, f: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(salt).max_height(110.0).auto_shrink([false, true]).show(ui, f);
    });
}

fn digital_ids(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    with_state(
        ctx,
        app,
        uid,
        |_| IdsState::default(),
        |st, app| {
            window(ctx, app, d, "Digital IDs", 700.0, |ui, app| {
                let sum = mailcraft_engine::smime::summary(&app.s);
                let arr = |k: &str| sum.get(k).and_then(Value::as_array).cloned().unwrap_or_default();

                heading(ui, app, "My digital IDs");
                muted(ui, app, "Sign the messages you send and read the encrypted messages sent to you.");
                let ids = arr("ids");
                list_frame(ui, app, "smime-ids", |ui| {
                    if ids.is_empty() {
                        ui.label(
                            "No digital IDs. Import one from the file your certificate authority gave you.",
                        );
                    }
                    for v in &ids {
                        let fp = s(v, "fingerprint");
                        let mut status = if v.get("expired").and_then(Value::as_bool) == Some(true) {
                            "Expired".to_owned()
                        } else {
                            format!("Valid to {}", s(v, "validTo"))
                        };
                        if v.get("default").and_then(Value::as_bool) == Some(true) {
                            status.push_str(" (default)");
                        }
                        if v.get("usable").and_then(Value::as_bool) == Some(false) {
                            status = "Key unavailable".into();
                        }
                        let email = v
                            .get("emails")
                            .and_then(Value::as_array)
                            .and_then(|a| a.first())
                            .and_then(Value::as_str);
                        let cols = [s(v, "name"), email.unwrap_or_default().to_owned(), status];
                        if cert_row(ui, st.id.as_deref() == Some(fp.as_str()), &cols) {
                            st.id = Some(fp);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if btn(ui, "Import...") {
                        st.form = Form::ImportId;
                        st.note.clear();
                    }
                    let sel = st.id.clone();
                    if btn_en(ui, sel.is_some(), "Export...") {
                        st.form = Form::ExportId;
                        st.note.clear();
                    }
                    if btn_en(ui, sel.is_some(), "Set as Default")
                        && let Some(fp) = &sel
                    {
                        run_note(
                            app,
                            st,
                            "options.setDefaultDigitalId",
                            json!({"fingerprint": fp}),
                            "Default set.",
                        );
                    }
                    if btn_en(ui, sel.is_some(), "Remove")
                        && let Some(fp) = &sel
                        && run_note(
                            app,
                            st,
                            "options.removeDigitalId",
                            json!({"fingerprint": fp}),
                            "Digital ID removed.",
                        )
                    {
                        st.id = None;
                    }
                });

                ui.add_space(8.0);
                heading(ui, app, "Other people's certificates");
                muted(
                    ui,
                    app,
                    "Encrypted messages to these addresses use these certificates. Valid signed mail adds them.",
                );
                let people = arr("people");
                list_frame(ui, app, "smime-people", |ui| {
                    if people.is_empty() {
                        ui.label("None yet.");
                    }
                    for v in &people {
                        let key = (s(v, "fingerprint"), s(v, "address"));
                        let source = if s(v, "source") == "mail" { "From signed mail" } else { "Imported" };
                        let cols =
                            [s(v, "address"), s(v, "name"), format!("{source}, to {}", s(v, "validTo"))];
                        if cert_row(ui, st.person.as_ref() == Some(&key), &cols) {
                            st.person = Some(key);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if btn(ui, "Import Certificate...") {
                        st.form = Form::ImportCert;
                        st.note.clear();
                    }
                    let sel = st.person.clone();
                    if btn_en(ui, sel.is_some(), "Remove")
                        && let Some((fp, addr)) = &sel
                        && run_note(
                            app,
                            st,
                            "options.removeCertificate",
                            json!({"fingerprint": fp, "address": addr}),
                            "Certificate removed.",
                        )
                    {
                        st.person = None;
                    }
                });

                ui.add_space(8.0);
                heading(ui, app, "Trusted certificates");
                muted(
                    ui,
                    app,
                    "Signatures that lead to one of these (or to a root your system trusts) are valid.",
                );
                let trusted = arr("trusted");
                list_frame(ui, app, "smime-trusted", |ui| {
                    if trusted.is_empty() {
                        ui.label("Only the roots your system trusts.");
                    }
                    for v in &trusted {
                        let fp = s(v, "fingerprint");
                        let cols = [s(v, "name"), s(v, "issuer"), format!("Valid to {}", s(v, "validTo"))];
                        if cert_row(ui, st.trusted.as_deref() == Some(fp.as_str()), &cols) {
                            st.trusted = Some(fp);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if btn(ui, "Trust a Certificate...") {
                        st.form = Form::TrustCert;
                        st.note.clear();
                    }
                    let sel = st.trusted.clone();
                    if btn_en(ui, sel.is_some(), "Stop Trusting")
                        && let Some(fp) = &sel
                        && run_note(
                            app,
                            st,
                            "options.trustCertificate",
                            json!({"fingerprint": fp, "trust": false}),
                            "The certificate is no longer trusted.",
                        )
                    {
                        st.trusted = None;
                    }
                });

                form_ui(ui, app, st);
                if !st.note.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(&st.note).color(app.theme.text));
                }
                ui.add_space(4.0);
                muted(
                    ui,
                    app,
                    &format!(
                        "Private keys are stored sealed; the secrets that open them are kept with your account \
                     passwords ({}).",
                        s(&sum, "secrets")
                    ),
                );
                let mut out = Out::Keep;
                footer(ui, |ui| {
                    if primary(ui, app, true, "Close") {
                        out = Out::Close;
                    }
                });
                out
            })
        },
    )
}

const PFX: (&str, &[&str]) = ("Digital ID (.pfx, .p12)", &["pfx", "p12"]);
const PEM: (&str, &[&str]) =
    ("Key or certificate (.pem, .key, .crt, .cer)", &["pem", "key", "crt", "cer", "der"]);
const CERTS: (&str, &[&str]) =
    ("Certificate (.cer, .crt, .pem, .p7b)", &["cer", "crt", "pem", "der", "p7b", "p7c"]);

/// The inline form for the action in progress.
fn form_ui(ui: &mut egui::Ui, app: &mut App, st: &mut IdsState) {
    if st.form == Form::None {
        return;
    }
    ui.add_space(8.0);
    egui::Frame::new().fill(app.theme.info_bar).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
        match st.form {
            Form::ImportId => {
                ui.label(RichText::new("Import a digital ID").strong());
                ui.horizontal(|ui| {
                    ui.radio_value(&mut st.pem, false, "A .pfx or .p12 file");
                    ui.radio_value(&mut st.pem, true, "A key file and a certificate file");
                });
                if st.pem {
                    file_row(ui, app, "Key file:", &mut st.key_path, false, &[PEM]);
                    file_row(ui, app, "Certificate file:", &mut st.path, false, &[PEM, CERTS]);
                } else {
                    file_row(ui, app, "File:", &mut st.path, false, &[PFX]);
                }
                password_row(ui, "Password:", &mut st.password);
                ui.checkbox(&mut st.trust_issuer, "Also trust the authority that issued it");
                ui.horizontal(|ui| {
                    let ready = !st.path.trim().is_empty() && (!st.pem || !st.key_path.trim().is_empty());
                    if btn_en(ui, ready, "Import") {
                        let p = if st.pem {
                            json!({"keyPath": st.key_path.trim(), "certPath": st.path.trim(),
                                   "password": st.password, "trustIssuer": st.trust_issuer})
                        } else {
                            json!({"path": st.path.trim(), "password": st.password, "trustIssuer": st.trust_issuer})
                        };
                        if run_note(app, st, "options.importDigitalId", p, "The digital ID was imported.") {
                            st.form = Form::None;
                            st.password.clear();
                        }
                    }
                    if btn(ui, "Cancel") {
                        st.form = Form::None;
                    }
                });
            }
            Form::ExportId => {
                ui.label(RichText::new("Export the selected digital ID").strong());
                muted(ui, app, "The file holds your private key: protect it with a strong password.");
                file_row(ui, app, "Save as:", &mut st.path, true, &[PFX]);
                password_row(ui, "Password:", &mut st.password);
                password_row(ui, "Confirm:", &mut st.password2);
                ui.horizontal(|ui| {
                    let ready = !st.path.trim().is_empty() && !st.password.is_empty() && st.password == st.password2;
                    if btn_en(ui, ready, "Export")
                        && let Some(fp) = st.id.clone()
                    {
                        let p = json!({"fingerprint": fp, "path": st.path.trim(), "password": st.password});
                        if run_note(app, st, "options.exportDigitalId", p, "The digital ID was exported.") {
                            st.form = Form::None;
                            st.password.clear();
                            st.password2.clear();
                        }
                    }
                    if btn(ui, "Cancel") {
                        st.form = Form::None;
                    }
                });
            }
            Form::ImportCert => {
                ui.label(RichText::new("Import someone's certificate").strong());
                file_row(ui, app, "File:", &mut st.path, false, &[CERTS]);
                ui.horizontal(|ui| {
                    ui.label("For address (optional):");
                    ui.add(egui::TextEdit::singleline(&mut st.address).desired_width(220.0));
                });
                ui.horizontal(|ui| {
                    if btn_en(ui, !st.path.trim().is_empty(), "Import") {
                        let address = st.address.trim();
                        let p = if address.is_empty() {
                            json!({"path": st.path.trim()})
                        } else {
                            json!({"path": st.path.trim(), "address": address})
                        };
                        if run_note(app, st, "options.importCertificate", p, "The certificate was imported.") {
                            st.form = Form::None;
                        }
                    }
                    if btn(ui, "Cancel") {
                        st.form = Form::None;
                    }
                });
            }
            Form::TrustCert => {
                ui.label(RichText::new("Trust a certificate").strong());
                muted(ui, app, "Only trust an authority you know: every signature it vouches for will count as valid.");
                file_row(ui, app, "File:", &mut st.path, false, &[CERTS]);
                ui.horizontal(|ui| {
                    if btn_en(ui, !st.path.trim().is_empty(), "Trust") {
                        let p = json!({"path": st.path.trim(), "trust": true});
                        if run_note(app, st, "options.trustCertificate", p, "The certificate is trusted.") {
                            st.form = Form::None;
                        }
                    }
                    if btn(ui, "Cancel") {
                        st.form = Form::None;
                    }
                });
            }
            Form::None => {}
        }
    });
}

fn message_security(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let p = d.params.clone();
    let id = d.u64("id");
    window(ctx, app, d, "Message Security", 620.0, |ui, app| {
        // Fresh facts each frame (trusting a certificate changes them); the opening params as a fallback.
        let live = id
            .and_then(|i| mailcraft_engine::smime::security(&app.s, mailcraft_engine::model::ItemId(i)))
            .and_then(|sec| serde_json::to_value(&sec).ok());
        let v = live.unwrap_or(p);
        let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        if b("encrypted") {
            heading(ui, app, "Encryption");
            if b("locked") {
                ui.label(format!("Encrypted. It cannot be opened: {}.", s(&v, "decryptError")));
            } else {
                ui.label("Encrypted, and opened with your digital ID.");
            }
        }
        if b("signed") {
            heading(ui, app, "Digital signature");
            egui::Grid::new("smime-sig").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                let status = match s(&v, "status").as_str() {
                    "valid" => "Valid",
                    "untrusted" => "Not trusted",
                    "expired" => "Certificate not valid",
                    _ => "Not valid",
                };
                for (k, val) in [
                    ("Signed by:", s(&v, "signer")),
                    ("Status:", status.to_owned()),
                    (
                        "Sender's address:",
                        if b("addressMatches") {
                            "Matches the certificate".into()
                        } else {
                            format!("{} is not in the certificate", s(&v, "sender"))
                        },
                    ),
                    ("Signed at:", s(&v, "signingTime")),
                    ("Digest:", s(&v, "digest")),
                    ("Format:", if b("clearSigned") { "Clear-signed".into() } else { "Opaque".into() }),
                ] {
                    ui.label(k);
                    ui.label(val);
                    ui.end_row();
                }
            });
            let reason = s(&v, "reason");
            if !reason.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new(reason).color(app.theme.text));
            }
            let chain = v.get("chain").and_then(Value::as_array).cloned().unwrap_or_default();
            if !chain.is_empty() {
                ui.add_space(6.0);
                heading(ui, app, "Certificate chain");
                egui::ScrollArea::vertical().id_salt("smime-chain").max_height(220.0).show(ui, |ui| {
                    for (i, c) in chain.iter().enumerate() {
                        let who = if i == 0 { "Signer" } else { "Issued by" };
                        ui.label(RichText::new(format!("{who}: {}", s(c, "name"))).strong());
                        muted(ui, app, &format!("Subject: {}", s(c, "subject")));
                        muted(ui, app, &format!("Issuer: {}", s(c, "issuer")));
                        muted(ui, app, &format!("Valid {} to {}", s(c, "validFrom"), s(c, "validTo")));
                        muted(ui, app, &format!("Serial number: {}", s(c, "serial")));
                        muted(ui, app, &format!("SHA-1: {}", s(c, "sha1")));
                        muted(ui, app, &format!("SHA-256: {}", s(c, "fingerprint")));
                        ui.add_space(4.0);
                    }
                });
            }
        }
        let mut out = Out::Keep;
        footer(ui, |ui| {
            if primary(ui, app, true, "Close") {
                out = Out::Close;
            }
            if s(&v, "status") == "untrusted"
                && let Some(i) = id
                && btn(ui, "Trust the Issuer")
            {
                app.run("options.trustCertificate", json!({"id": i}));
            }
        });
        out
    })
}

fn encryption_problem(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let compose = d.u64("compose");
    let missing: Vec<String> =
        d.arr("missing").iter().filter_map(|v| v.as_str().map(str::to_owned)).collect();
    let sign = d.bool("sign");
    window(ctx, app, d, "Encryption Problem", 520.0, |ui, app| {
        ui.label("The message can't be encrypted: there is no certificate for these recipients:");
        for m in missing.iter().take(20) {
            ui.label(RichText::new(format!("  {m}")).strong());
        }
        if missing.len() > 20 {
            muted(ui, app, &format!("and {} more", missing.len() - 20));
        }
        ui.add_space(4.0);
        muted(
            ui,
            app,
            "Their certificates are added when they send you signed mail, or you can import them in Digital IDs.",
        );
        if sign {
            muted(ui, app, "Sent without encryption, the message is still signed.");
        }
        let mut out = Out::Keep;
        footer(ui, |ui| {
            if btn(ui, "Cancel") {
                out = Out::Close;
            }
            if primary(ui, app, compose.is_some(), "Send Unencrypted")
                && let Some(c) = compose
            {
                app.run("compose.send", json!({"compose": c, "encryption": "skip"}));
                out = Out::Close;
            }
        });
        out
    })
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use mailcraft_engine::sample::sample_session;
    use mailcraft_smime::testkit;
    use serde_json::json;

    use crate::App;

    fn harness(app: App) -> Harness<'static, App> {
        let mut h = Harness::builder().with_size(egui::vec2(1280.0, 1000.0)).build_ui_state(
            |ui, app: &mut App| {
                let ctx = ui.ctx().clone();
                crate::dialogs::dialogs(&ctx, app);
            },
            app,
        );
        h.run_steps(2);
        h
    }

    fn app_with_id() -> (App, mailcraft_smime::Identity) {
        let mut s = sample_session();
        let me = s.mailbox.default_account().unwrap().address.clone();
        let id = testkit::identity("Sample Me", &me);
        let pfx = mailcraft_smime::pkcs12::build(&id, "pw", "").unwrap();
        let data = mailcraft_mime::transfer::encode_base64_flat(&pfx);
        s.run("options.importDigitalId", &json!({"data": data, "password": "pw", "trustIssuer": true}))
            .unwrap();
        s.notices.clear();
        (App::new(s), id)
    }

    #[test]
    fn digital_ids_lists_and_removes() {
        let (mut app, id) = app_with_id();
        app.run("options.digitalIds", json!({}));
        let mut h = harness(app);
        h.run_steps(2);
        assert!(h.query_by_label("Sample Me").is_some(), "the ID is listed");
        assert!(h.query_by_label(testkit::CA_NAME).is_some(), "its issuer is trusted");
        h.get_by_label("Sample Me").click();
        h.run_steps(2);
        // The Remove next to the IDs list is the first one.
        h.get_all_by_label("Remove").next().unwrap().click();
        h.run_steps(2);
        assert!(h.query_by_label("Digital ID removed.").is_some());
        let app = h.into_state();
        assert!(mailcraft_engine::smime::identities(&app.s).iter().all(|i| i.cert != id.cert));
        // Import opens the form with a password field.
        let mut app = app;
        app.run("options.digitalIds", json!({}));
        let mut h = harness(app);
        h.get_all_by_label("Import...").next().unwrap().click();
        h.run_steps(2);
        assert!(h.query_all_by_label("Also trust the authority that issued it").next().is_some());
    }

    #[test]
    fn encryption_problem_sends_unencrypted() {
        use mailcraft_model::{Account, AccountKind, ServerSettings};
        let mut s = sample_session();
        let acc = s.mailbox.add_account(Account {
            kind: AccountKind::Imap,
            display_name: "me@example.com".into(),
            address: "me@example.com".into(),
            sender_name: "Me Example".into(),
            outgoing: Some(ServerSettings {
                host: "smtp.example.com".into(),
                port: 465,
                ..Default::default()
            }),
            ..Default::default()
        });
        s.mailbox.create_default_folders(acc);
        for a in &mut s.mailbox.accounts {
            a.is_default = a.id == acc;
        }
        s.view.work_offline = true;
        s.refresh_me();
        let me = testkit::identity("Me Example", "me@example.com");
        let pfx = mailcraft_smime::pkcs12::build(&me, "pw", "").unwrap();
        let data = mailcraft_mime::transfer::encode_base64_flat(&pfx);
        s.run("options.importDigitalId", &json!({"data": data, "password": "pw"})).unwrap();
        let mut app = App::new(s);
        let c = app.run("mail.newEmail", json!({})).unwrap()["compose"].as_u64().unwrap();
        app.run("compose.setTo", json!({"compose": c, "value": "stranger@example.net"}));
        app.run("compose.setSubject", json!({"compose": c, "value": "Hello"}));
        app.run("compose.encrypt", json!({"compose": c, "on": true}));
        let r = app.run("compose.send", json!({"compose": c})).unwrap();
        assert_eq!(r["reason"], "missingCertificates", "{r}");
        let mut h = harness(app);
        h.run_steps(2);
        assert!(h.query_all_by_label("  stranger@example.net").next().is_some());
        h.get_all_by_label("Send Unencrypted").next().unwrap().click();
        h.run_steps(3);
        let app = h.into_state();
        assert!(!app.s.composes.contains_key(&c), "sent");
    }

    #[test]
    fn message_security_shows_the_chain_and_trusts() {
        let mut s = sample_session();
        let ann = testkit::identity("Ann Lee", "ann@example.org");
        let msg = "From: Ann Lee <ann@example.org>\r\nTo: me@example.com\r\nSubject: Signed\r\nMIME-Version: 1.0\r\n\
                   Content-Type: text/plain\r\n\r\nHello.\r\n";
        let p = mailcraft_smime::mime::Protect {
            sign: Some(&ann),
            encrypt_to: &[],
            clear_signed: true,
            now: testkit::NOW,
        };
        let raw = mailcraft_smime::mime::protect(msg.as_bytes(), &p).unwrap();
        let parsed = mailcraft_mime::parse(&raw).unwrap();
        let mut item = mailcraft_model::MailItem::from_message(&parsed, &raw, s.now());
        item.folder = s.selection.folder;
        let id = s.add_message(item, &mailcraft_engine::Content::source(raw.clone())).unwrap();
        let mut app = App::new(s);
        app.run("mail.signatureDetails", json!({"id": id.0}));
        let mut h = harness(app);
        h.run_steps(2);
        assert!(h.query_by_label("Not trusted").is_some());
        assert!(h.query_by_label("Signer: Ann Lee").is_some());
        h.get_by_label("Trust the Issuer").click();
        h.run_steps(3);
        assert!(h.query_by_label("Valid").is_some(), "trusting the root makes the signature valid");
    }
}
