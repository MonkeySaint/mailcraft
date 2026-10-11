//! Mail option dialogs: Reading Pane options, Message Options / Properties, and Signatures and Stationery
//! (`signatures`). Reading Pane and Stationery settings are engine settings (`mail.readingPane.*`,
//! `mail.compose.font*`, saved with the profile). Junk Email Options are `super::junk`.

mod signatures;

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, muted, ok_cancel, window, with_state};
use crate::App;
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
    voting_on: bool,
    voting: String,
    delivery: bool,
    read: bool,
    replies_on: bool,
    replies: String,
    delay_on: bool,
    delay: String,
    save_sent: usize,
}

fn message_options(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let item = d.u64("id");
    let init = |a: &App| {
        if let Some(c) = compose.and_then(|c| a.s.composes.get(&c)) {
            MsgOpts {
                importance: imp_idx(c.importance),
                sensitivity: sens_idx(c.sensitivity),
                voting_on: !c.voting.is_empty(),
                voting: if c.voting.is_empty() { "Approve;Reject".into() } else { c.voting.clone() },
                delivery: c.request_delivery_receipt,
                read: c.request_read_receipt,
                replies_on: !c.reply_to.is_empty(),
                replies: c.reply_to.clone(),
                delay_on: c.delay_until.is_some(),
                delay: c
                    .delay_until
                    .map(|t| {
                        let (dd, tt) = common::date_parts(a, t.unix);
                        format!("{dd} {tt}")
                    })
                    .unwrap_or_else(|| format!("{} 17:00", common::today(a))),
                save_sent: 0,
            }
        } else if let Some(m) = item.and_then(|i| a.s.mailbox.mail.get(&ItemId(i))) {
            MsgOpts {
                importance: imp_idx(m.importance),
                sensitivity: sens_idx(m.sensitivity),
                ..Default::default()
            }
        } else {
            MsgOpts { importance: 1, ..Default::default() }
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        let title = if compose.is_some() { "Properties" } else { "Message Options" };
        window(ctx, app, d, title, 520.0, |ui, app| {
            common::heading(ui, app, "Settings");
            ui.horizontal(|ui| {
                ui.label("Importance");
                common::combo_idx(ui, ("moimp", uid), &mut st.importance, &IMPORTANCE, 110.0);
                ui.label("Sensitivity");
                common::combo_idx(ui, ("mosens", uid), &mut st.sensitivity, &SENSITIVITY, 130.0);
            });
            if compose.is_some() {
                common::heading(ui, app, "Voting and Tracking options");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.voting_on, "Use voting buttons");
                    ui.add_enabled(
                        st.voting_on,
                        egui::TextEdit::singleline(&mut st.voting).desired_width(220.0),
                    );
                });
                ui.checkbox(&mut st.delivery, "Request a delivery receipt for this message");
                ui.checkbox(&mut st.read, "Request a read receipt for this message");
                common::heading(ui, app, "Delivery options");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.replies_on, "Have replies sent to");
                    ui.add_enabled(
                        st.replies_on,
                        egui::TextEdit::singleline(&mut st.replies).desired_width(240.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.delay_on, "Do not deliver before");
                    ui.add_enabled(
                        st.delay_on,
                        egui::TextEdit::singleline(&mut st.delay)
                            .hint_text("YYYY-MM-DD HH:MM")
                            .desired_width(160.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Save copy of sent message:");
                    common::combo_idx(
                        ui,
                        ("mosave", uid),
                        &mut st.save_sent,
                        &["Sent Items (default)", "Do not save"],
                        180.0,
                    );
                });
            } else if let Some(m) = item.and_then(|i| app.s.mailbox.mail.get(&ItemId(i))) {
                common::heading(ui, app, "Internet headers");
                let mut hdr = String::new();
                if let Some(f) = &m.from {
                    hdr.push_str(&format!("From: {}\n", f.to_display_string()));
                }
                hdr.push_str(&format!("Subject: {}\n", m.subject));
                if let Some(id) = &m.message_id {
                    hdr.push_str(&format!("Message-ID: {id}\n"));
                }
                hdr.push_str(&format!("Size: {}\n", common::size_text(m.size)));
                egui::ScrollArea::vertical().id_salt(("hdr", uid)).max_height(120.0).show(ui, |ui| {
                    ui.label(RichText::new(hdr).monospace());
                });
            }
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let imp = IMPORTANCE.get(st.importance).copied().unwrap_or("Normal").to_lowercase();
                    let sens = SENSITIVITY.get(st.sensitivity).copied().unwrap_or("Normal").to_lowercase();
                    match compose {
                        Some(c) => apply_compose(app, c, st, &imp, &sens),
                        None => {
                            app.run("mail.messageOptions", json!({"ids": item.into_iter().collect::<Vec<_>>(), "importance": imp, "sensitivity": sens}));
                        }
                    }
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

fn apply_compose(app: &mut App, c: u64, st: &MsgOpts, imp: &str, sens: &str) {
    let Some(cs) = app.s.composes.get(&c).cloned() else { return };
    if imp_idx(cs.importance) != st.importance {
        app.run("compose.importance", json!({"compose": c, "level": imp}));
    }
    if sens_idx(cs.sensitivity) != st.sensitivity {
        app.run("compose.sensitivity", json!({"compose": c, "level": sens}));
    }
    let voting = if st.voting_on { st.voting.trim().to_owned() } else { String::new() };
    if voting != cs.voting {
        let choices = if voting.is_empty() { cs.voting.clone() } else { voting };
        app.run("compose.votingButtons", json!({"compose": c, "choices": choices}));
    }
    if st.delivery != cs.request_delivery_receipt {
        app.run("compose.deliveryReceipt", json!({"compose": c, "on": st.delivery}));
    }
    if st.read != cs.request_read_receipt {
        app.run("compose.readReceipt", json!({"compose": c, "on": st.read}));
    }
    let replies = if st.replies_on { st.replies.trim().to_owned() } else { String::new() };
    if replies != cs.reply_to {
        app.run("compose.directRepliesTo", json!({"compose": c, "addresses": replies}));
    }
    if st.delay_on {
        app.run("compose.delayDelivery", json!({"compose": c, "at": st.delay.trim()}));
    } else if cs.delay_until.is_some() {
        app.run("compose.delayDelivery", json!({"compose": c, "at": null}));
    }
    if st.save_sent == 1 {
        app.run("compose.saveSentItemTo", json!({"compose": c, "mode": "none"}));
    }
}

// ---------------------------------------------------------------------------------------------------------
