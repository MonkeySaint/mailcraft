//! Mail option dialogs: Reading Pane options, Message Options / Properties, and Signatures and Stationery
//! (`signatures`). Reading Pane and Stationery settings are engine settings (`mail.readingPane.*`,
//! `mail.compose.font*`, saved with the profile). Junk Email Options are `super::junk`.

mod signatures;

use egui::Context;
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn, muted, ok_cancel, window, with_state};
use crate::App;
use mailcraft_engine::compose::SaveSentTo;
use mailcraft_model::{Importance, ItemId, Sensitivity};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "readingPaneOptions" => reading_pane(ctx, app, d),
        "messageOptions" => message_options(ctx, app, d),
        "stationery" | "signatures" => signatures::show(ctx, app, d),
        _ => return None,
    })
}

fn sbool(app: &App, key: &str) -> bool {
    app.s.settings.bool(key)
}

/// Applies option values; an error (none expected from these controls) is shown in the dialog.
fn save(app: &mut App, values: &[(&str, Value)]) -> Option<String> {
    common::set_options(app, values)
}

#[derive(Clone, Default)]
struct PaneState {
    mark_viewed: bool,
    wait: u32,
    mark_change: bool,
    space: bool,
    portrait: bool,
    always_preview: bool,
    error: Option<String>,
}

fn reading_pane(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |a: &App| PaneState {
        mark_viewed: sbool(a, "mail.readingPane.markReadWhenViewed"),
        wait: u32::try_from(a.s.settings.int("mail.readingPane.markReadAfterSeconds")).unwrap_or(5),
        mark_change: sbool(a, "mail.readingPane.markReadOnSelectionChange"),
        space: sbool(a, "mail.readingPane.spaceBarReading"),
        portrait: sbool(a, "mail.readingPane.portraitFullScreen"),
        always_preview: sbool(a, "mail.readingPane.alwaysPreview"),
        error: None,
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Reading Pane", 440.0, |ui, app| {
            common::heading(ui, app, "Reading Pane options");
            ui.checkbox(&mut st.mark_viewed, "Mark items as read when viewed in the Reading Pane");
            ui.add_enabled_ui(st.mark_viewed, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Wait");
                    ui.add(egui::DragValue::new(&mut st.wait).range(0..=999));
                    ui.label("seconds before marking item as read");
                });
            });
            ui.checkbox(&mut st.mark_change, "Mark item as read when selection changes");
            ui.checkbox(&mut st.space, "Single key reading using space bar");
            ui.checkbox(&mut st.portrait, "Turn on automatic full-screen reading in portrait orientation");
            ui.checkbox(&mut st.always_preview, "Always preview messages");
            if let Some(e) = &st.error {
                muted(ui, app, e);
            }
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    st.error = save(
                        app,
                        &[
                            ("mail.readingPane.markReadWhenViewed", json!(st.mark_viewed)),
                            ("mail.readingPane.markReadAfterSeconds", json!(st.wait)),
                            ("mail.readingPane.markReadOnSelectionChange", json!(st.mark_change)),
                            ("mail.readingPane.spaceBarReading", json!(st.space)),
                            ("mail.readingPane.portraitFullScreen", json!(st.portrait)),
                            ("mail.readingPane.alwaysPreview", json!(st.always_preview)),
                        ],
                    );
                    common::sync_mirror(ctx, app);
                    if st.error.is_some() { Out::Keep } else { Out::Close }
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

// ---------------------------------------------------------------------------------------------------------

/// Properties › Security: encrypt the message (S/MIME).
pub const PROPERTIES_ENCRYPT: &str = "Encrypt this message";
/// Properties › Security: sign the message (S/MIME).
pub const PROPERTIES_SIGN: &str = "Sign this message";

const IMPORTANCE: [&str; 3] = ["Low", "Normal", "High"];
const SENSITIVITY: [&str; 4] = ["Normal", "Personal", "Private", "Confidential"];

fn imp_idx(i: Importance) -> usize {
    match i {
        Importance::Low => 0,
        Importance::Normal => 1,
        Importance::High => 2,
    }
}

fn sens_idx(s: Sensitivity) -> usize {
    match s {
        Sensitivity::Normal => 0,
        Sensitivity::Personal => 1,
        Sensitivity::Private => 2,
        Sensitivity::Confidential => 3,
    }
}

#[derive(Clone, Default)]
struct MsgOpts {
    importance: usize,
    sensitivity: usize,
    /// Security Settings: encrypt / add a digital signature.
    encrypt: bool,
    sign: bool,
    voting_on: bool,
    voting: String,
    delivery: bool,
    read: bool,
    replies_on: bool,
    replies: String,
    delay_on: bool,
    delay_date: String,
    delay_time: String,
    expires_on: bool,
    expires_date: String,
    expires_time: String,
    /// "Save copy of sent message": the chosen place (a value, not a position in the folder list, which can
    /// change while the dialog is open) and the one the message had when the dialog opened.
    save_sent: SaveSentTo,
    save_sent_was: SaveSentTo,
    categories: Vec<String>,
    contacts: String,
    /// "Do not AutoArchive this item".
    no_archive: bool,
    error: Option<String>,
}

/// Voting button presets of the Properties dialog (the Use Voting Buttons choices).
pub(crate) const VOTING_PRESETS: [&str; 3] = ["Approve;Reject", "Yes;No", "Yes;No;Maybe"];

/// Most folders offered for "Save copy of sent message".
const MAX_SAVE_FOLDERS: usize = 300;

/// "Save copy of sent message" choices: the default Sent Items, not saving, then the mail folders (sorted,
/// at most [`MAX_SAVE_FOLDERS`]). `current` is always among them, even past the limit or when its folder is
/// gone.
fn save_sent_choices(app: &App, current: SaveSentTo) -> Vec<(String, SaveSentTo)> {
    let mut out = vec![
        ("Sent Items (default)".to_owned(), SaveSentTo::Default),
        ("Do not save".to_owned(), SaveSentTo::DoNotSave),
    ];
    let mut folders: Vec<(String, SaveSentTo)> = app
        .s
        .mailbox
        .folders
        .values()
        .filter(|f| f.kind == mailcraft_model::FolderKind::Mail)
        .map(|f| (format!("Folder: {}", app.s.mailbox.folder_path(f.id)), SaveSentTo::Folder(f.id)))
        .collect();
    folders.sort_by(|a, b| a.0.cmp(&b.0));
    folders.truncate(MAX_SAVE_FOLDERS);
    out.extend(folders);
    if let SaveSentTo::Folder(id) = current
        && !out.iter().any(|(_, t)| *t == current)
    {
        let label = match app.s.mailbox.folders.get(&id) {
            Some(_) => format!("Folder: {}", app.s.mailbox.folder_path(id)),
            None => "Folder: (no longer exists)".to_owned(),
        };
        out.push((label, current));
    }
    out
}

/// The categories menu of the Properties dialog: every category with a checkbox.
fn categories_menu(ui: &mut egui::Ui, app: &App, cats: &mut Vec<String>) {
    let text = if cats.is_empty() { "None".to_owned() } else { cats.join(", ") };
    ui.menu_button(text, |ui| {
        if app.s.mailbox.categories.is_empty() {
            ui.label("No categories yet.");
        }
        for c in &app.s.mailbox.categories {
            let mut on = cats.iter().any(|x| x.eq_ignore_ascii_case(&c.name));
            if ui.checkbox(&mut on, &c.name).changed() {
                if on {
                    cats.push(c.name.clone());
                } else {
                    cats.retain(|x| !x.eq_ignore_ascii_case(&c.name));
                }
            }
        }
    });
}

fn message_options(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let item = d.u64("id");
    let init = |a: &App| {
        if let Some(c) = compose.and_then(|c| a.s.composes.get(&c)) {
            let parts = |t: Option<mailcraft_model::DateTime>, default_time: &str| match t {
                Some(t) => common::date_parts(a, t.unix),
                None => (common::today(a), default_time.to_owned()),
            };
            let (delay_date, delay_time) = parts(c.delay_until, "17:00");
            let (expires_date, expires_time) = parts(c.expires, "17:00");
            MsgOpts {
                importance: imp_idx(c.importance),
                sensitivity: sens_idx(c.sensitivity),
                encrypt: c.encrypt,
                sign: c.sign,
                voting_on: !c.voting.is_empty(),
                voting: if c.voting.is_empty() { "Approve;Reject".into() } else { c.voting.clone() },
                delivery: c.request_delivery_receipt,
                read: c.request_read_receipt,
                replies_on: !c.reply_to.is_empty(),
                replies: c.reply_to.clone(),
                delay_on: c.delay_until.is_some(),
                delay_date,
                delay_time,
                expires_on: c.expires.is_some(),
                expires_date,
                expires_time,
                save_sent: c.save_sent,
                save_sent_was: c.save_sent,
                categories: c.categories.clone(),
                contacts: c.contacts.clone(),
                no_archive: c.no_auto_archive,
                error: None,
            }
        } else if let Some(m) = item.and_then(|i| a.s.mailbox.mail.get(&ItemId(i))) {
            MsgOpts {
                importance: imp_idx(m.importance),
                sensitivity: sens_idx(m.sensitivity),
                no_archive: m.no_auto_archive,
                ..Default::default()
            }
        } else {
            MsgOpts { importance: 1, ..Default::default() }
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        for (k, v) in common::take_replies(ctx, uid) {
            if let (true, Some(t)) = (k == "replies", v.as_str()) {
                st.replies = t.to_owned();
                st.replies_on = !t.trim().is_empty();
            }
        }
        let title = if compose.is_some() { "Properties" } else { "Message Options" };
        window(ctx, app, d, title, 560.0, |ui, app| {
            let mut out = Out::Keep;
            common::heading(ui, app, "Settings");
            ui.horizontal(|ui| {
                ui.label("Importance");
                common::combo_idx(ui, ("moimp", uid), &mut st.importance, &IMPORTANCE, 110.0);
                ui.label("Sensitivity");
                common::combo_idx(ui, ("mosens", uid), &mut st.sensitivity, &SENSITIVITY, 130.0);
            });
            if compose.is_some() {
                ui.checkbox(&mut st.no_archive, "Do not AutoArchive this item");
                common::heading(ui, app, "Security");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.encrypt, PROPERTIES_ENCRYPT);
                });
                ui.checkbox(&mut st.sign, PROPERTIES_SIGN);
                common::heading(ui, app, "Voting and Tracking options");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.voting_on, "Use voting buttons");
                    ui.add_enabled_ui(st.voting_on, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut st.voting).desired_width(200.0));
                        egui::ComboBox::from_id_salt(("movote", uid)).selected_text("").width(24.0).show_ui(
                            ui,
                            |ui| {
                                for preset in VOTING_PRESETS {
                                    if ui.selectable_label(st.voting == preset, preset).clicked() {
                                        st.voting = preset.to_owned();
                                    }
                                }
                            },
                        );
                    });
                });
                ui.checkbox(&mut st.delivery, "Request a delivery receipt for this message");
                ui.checkbox(&mut st.read, "Request a read receipt for this message");
                common::heading(ui, app, "Delivery options");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.replies_on, "Have replies sent to");
                    ui.add_enabled_ui(st.replies_on, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut st.replies).desired_width(220.0));
                        if btn(ui, "Select Names...") {
                            out = Out::open(
                                "addressBook",
                                common::child(
                                    uid,
                                    "replies",
                                    json!({"value": st.replies, "pickLabel": "Reply To ->"}),
                                ),
                            );
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.delay_on, "Do not deliver before");
                    ui.add_enabled_ui(st.delay_on, |ui| {
                        common::date_box(ui, app, ("modelay", uid), &mut st.delay_date, "", "Delivery date");
                        common::time_box(ui, ("modelayt", uid), &mut st.delay_time, "Delivery time");
                    });
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.expires_on, "Expires after");
                    ui.add_enabled_ui(st.expires_on, |ui| {
                        common::date_box(ui, app, ("moexp", uid), &mut st.expires_date, "", "Expiry date");
                        common::time_box(ui, ("moexpt", uid), &mut st.expires_time, "Expiry time");
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("Save copy of sent message:");
                    let choices = save_sent_choices(app, st.save_sent);
                    let labels: Vec<&str> = choices.iter().map(|(l, _)| l.as_str()).collect();
                    let mut idx = choices.iter().position(|(_, t)| *t == st.save_sent).unwrap_or(0);
                    if common::combo_idx(ui, ("mosave", uid), &mut idx, &labels, 260.0)
                        && let Some((_, t)) = choices.get(idx)
                    {
                        st.save_sent = *t;
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Contacts");
                    ui.add(
                        egui::TextEdit::singleline(&mut st.contacts)
                            .hint_text("Names, separated by semicolons")
                            .desired_width(300.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Categories");
                    categories_menu(ui, app, &mut st.categories);
                });
            } else if let Some(m) = item.and_then(|i| app.s.mailbox.mail.get(&ItemId(i))) {
                ui.checkbox(&mut st.no_archive, "Do not AutoArchive this item");
                common::heading(ui, app, "Internet headers");
                // The message's own header block as received (the engine reads it from the source).
                let mut hdr = d.str("headers");
                if hdr.is_empty() {
                    if let Some(f) = &m.from {
                        hdr.push_str(&format!("From: {}\n", f.to_display_string()));
                    }
                    hdr.push_str(&format!("Subject: {}\n", m.subject));
                    if let Some(id) = &m.message_id {
                        hdr.push_str(&format!("Message-ID: {id}\n"));
                    }
                }
                muted(ui, app, &format!("Size: {}", common::size_text(m.size)));
                egui::ScrollArea::both().id_salt(("hdr", uid)).max_height(160.0).show(ui, |ui| {
                    // Read-only but selectable, so the headers can be copied for diagnostics.
                    ui.add(
                        egui::TextEdit::multiline(&mut hdr.as_str())
                            .code_editor()
                            .desired_rows(8)
                            .desired_width(f32::INFINITY),
                    );
                });
            }
            if let Some(e) = &st.error {
                ui.colored_label(app.theme.danger, e);
            }
            if !matches!(out, Out::Keep) {
                return out;
            }
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let imp = IMPORTANCE.get(st.importance).copied().unwrap_or("Normal").to_lowercase();
                    let sens = SENSITIVITY.get(st.sensitivity).copied().unwrap_or("Normal").to_lowercase();
                    match compose {
                        Some(c) => {
                            st.error = apply_compose(app, c, st, &imp, &sens);
                            if st.error.is_some() { Out::Keep } else { Out::Close }
                        }
                        None => {
                            let ids: Vec<u64> = item.into_iter().collect();
                            app.run(
                                "mail.messageOptions",
                                json!({"ids": ids, "importance": imp, "sensitivity": sens}),
                            );
                            let was = item
                                .and_then(|i| app.s.mailbox.mail.get(&ItemId(i)))
                                .is_some_and(|m| m.no_auto_archive);
                            if !ids.is_empty() && was != st.no_archive {
                                app.run("mail.doNotAutoArchive", json!({"ids": ids, "on": st.no_archive}));
                            }
                            Out::Close
                        }
                    }
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

/// Applies the Properties dialog to compose `c`; returns the first error to show (the dialog stays open).
fn apply_compose(app: &mut App, c: u64, st: &MsgOpts, imp: &str, sens: &str) -> Option<String> {
    let cs = app.s.composes.get(&c).cloned()?;
    // Dates first, so a typo keeps the dialog open before anything changes.
    let when = |app: &App, on: bool, date: &str, time: &str, what: &str| -> Result<Option<String>, String> {
        if !on {
            return Ok(None);
        }
        common::parse_date(app, date, time)
            .map(|_| Some(format!("{} {}", date.trim(), time.trim())))
            .ok_or_else(|| format!("The {what} date or time is not valid."))
    };
    let delay = match when(app, st.delay_on, &st.delay_date, &st.delay_time, "delivery") {
        Ok(v) => v,
        Err(e) => return Some(e),
    };
    let expires = match when(app, st.expires_on, &st.expires_date, &st.expires_time, "expiry") {
        Ok(v) => v,
        Err(e) => return Some(e),
    };
    let mut failed: Option<String> = None;
    let mut run = |app: &mut App, cmd: &str, p: Value| {
        if app.run(cmd, p).is_none() && failed.is_none() {
            failed = Some(super::folders::last_error(app));
        }
    };
    if imp_idx(cs.importance) != st.importance {
        run(app, "compose.importance", json!({"compose": c, "level": imp}));
    }
    if sens_idx(cs.sensitivity) != st.sensitivity {
        run(app, "compose.sensitivity", json!({"compose": c, "level": sens}));
    }
    if st.encrypt != cs.encrypt {
        run(app, "compose.encrypt", json!({"compose": c, "on": st.encrypt}));
    }
    if st.sign != cs.sign {
        run(app, "compose.sign", json!({"compose": c, "on": st.sign}));
    }
    let voting = if st.voting_on { st.voting.trim().to_owned() } else { String::new() };
    if voting != cs.voting {
        let choices = if voting.is_empty() { cs.voting.clone() } else { voting };
        run(app, "compose.votingButtons", json!({"compose": c, "choices": choices}));
    }
    if st.delivery != cs.request_delivery_receipt {
        run(app, "compose.deliveryReceipt", json!({"compose": c, "on": st.delivery}));
    }
    if st.read != cs.request_read_receipt {
        run(app, "compose.readReceipt", json!({"compose": c, "on": st.read}));
    }
    let replies = if st.replies_on { st.replies.trim().to_owned() } else { String::new() };
    if replies != cs.reply_to {
        run(app, "compose.directRepliesTo", json!({"compose": c, "addresses": replies}));
    }
    match delay {
        Some(at) => run(app, "compose.delayDelivery", json!({"compose": c, "at": at})),
        None if cs.delay_until.is_some() => {
            run(app, "compose.delayDelivery", json!({"compose": c, "at": null}))
        }
        None => {}
    }
    // Only a choice the person changed is applied (anything else keeps what the message has).
    if st.save_sent != st.save_sent_was && st.save_sent != cs.save_sent {
        let p = match st.save_sent {
            SaveSentTo::Default => json!({"compose": c, "mode": "default"}),
            SaveSentTo::DoNotSave => json!({"compose": c, "mode": "none"}),
            SaveSentTo::Folder(f) => json!({"compose": c, "mode": "other", "folder": f.0}),
        };
        run(app, "compose.saveSentItemTo", p);
    }
    let props_changed = expires.is_some() != cs.expires.is_some()
        || expires.is_some()
        || st.categories != cs.categories
        || st.contacts.trim() != cs.contacts
        || st.no_archive != cs.no_auto_archive;
    if props_changed {
        run(
            app,
            "compose.messageOptions",
            json!({"compose": c, "expires": expires, "categories": st.categories, "contacts": st.contacts,
                   "noAutoArchive": st.no_archive}),
        );
    }
    failed
}

// ---------------------------------------------------------------------------------------------------------
