//! Mail option dialogs: Reading Pane options, Junk Email Options, Message Options / Properties, and
//! Signatures and Stationery (`signatures`).

mod signatures;

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn_en, muted, ok_cancel, window, with_state};
use crate::App;
use mailcraft_model::{Importance, ItemId, Sensitivity};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "readingPaneOptions" => reading_pane(ctx, app, d),
        "junkOptions" => junk(ctx, app, d),
        "messageOptions" => message_options(ctx, app, d),
        "stationery" | "signatures" => signatures::show(ctx, app, d),
        _ => return None,
    })
}

fn pbool(ctx: &Context, key: &str, default: bool) -> bool {
    common::pref(ctx, key).and_then(|v| v.as_bool()).unwrap_or(default)
}

#[derive(Clone, Default)]
struct PaneState {
    mark_viewed: bool,
    wait: u32,
    mark_change: bool,
    space: bool,
    portrait: bool,
    always_preview: bool,
}

fn reading_pane(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |_: &App| PaneState {
        mark_viewed: pbool(ctx, "readingPane.markWhenViewed", false),
        wait: common::pref(ctx, "readingPane.waitSeconds")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(5),
        mark_change: pbool(ctx, "readingPane.markOnSelectionChange", true),
        space: pbool(ctx, "readingPane.singleKey", true),
        portrait: pbool(ctx, "readingPane.portraitFullScreen", false),
        always_preview: pbool(ctx, "readingPane.alwaysPreview", true),
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
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    common::set_pref(ctx, "readingPane.markWhenViewed", json!(st.mark_viewed));
                    common::set_pref(ctx, "readingPane.waitSeconds", json!(st.wait));
                    common::set_pref(ctx, "readingPane.markOnSelectionChange", json!(st.mark_change));
                    common::set_pref(ctx, "readingPane.singleKey", json!(st.space));
                    common::set_pref(ctx, "readingPane.portraitFullScreen", json!(st.portrait));
                    common::set_pref(ctx, "readingPane.alwaysPreview", json!(st.always_preview));
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

// ---------------------------------------------------------------------------------------------------------

const JUNK_TABS: [&str; 5] =
    ["Options", "Safe Senders", "Safe Recipients", "Blocked Senders", "International"];
const JUNK_LISTS: [&str; 3] = ["junk.safeSenders", "junk.safeRecipients", "junk.blockedSenders"];

#[derive(Clone, Default)]
struct JunkState {
    tab: usize,
    level: usize,
    delete: bool,
    links: bool,
    warn: bool,
    lists: [Vec<String>; 3],
    entry: String,
    selected: Option<usize>,
    trust_contacts: bool,
}

fn list_pref(ctx: &Context, key: &str) -> Vec<String> {
    common::pref(ctx, key)
        .and_then(|v| v.as_array().cloned())
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn junk(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |_: &App| JunkState {
        level: common::pref(ctx, "junk.level")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(1),
        delete: pbool(ctx, "junk.deleteInstead", false),
        links: pbool(ctx, "junk.disableLinks", true),
        warn: pbool(ctx, "junk.warnDomains", true),
        trust_contacts: pbool(ctx, "junk.trustContacts", true),
        lists: [list_pref(ctx, JUNK_LISTS[0]), list_pref(ctx, JUNK_LISTS[1]), list_pref(ctx, JUNK_LISTS[2])],
        ..Default::default()
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Junk Email Options", 520.0, |ui, app| {
            ui.horizontal(|ui| {
                for (i, t) in JUNK_TABS.iter().enumerate() {
                    if ui.selectable_label(st.tab == i, *t).clicked() {
                        st.tab = i;
                        st.selected = None;
                    }
                }
            });
            ui.separator();
            match st.tab {
                0 => {
                    muted(ui, app, "Messages that look like junk can be moved to the Junk Email folder.");
                    ui.label("Choose the level of junk email protection you want:");
                    for (i, t) in [
                        "No Automatic Filtering. Mail from blocked senders is still moved to the Junk Email folder.",
                        "Low: Move the most obvious junk email to the Junk Email folder.",
                        "High: Most junk email is caught, but some regular mail may be caught as well.",
                        "Safe Lists Only: Only mail from people or domains on your Safe Senders List or Safe Recipients List is delivered to your Inbox.",
                    ]
                    .iter()
                    .enumerate()
                    {
                        ui.radio_value(&mut st.level, i, *t);
                    }
                    ui.checkbox(&mut st.delete, "Permanently delete suspected junk email instead of moving it to the Junk Email folder");
                    ui.checkbox(&mut st.links, "Disable links and other functionality in phishing messages");
                    ui.checkbox(&mut st.warn, "Warn me about suspicious domain names in email addresses");
                }
                1..=3 => {
                    let idx = st.tab - 1;
                    if idx == 0 {
                        ui.checkbox(&mut st.trust_contacts, "Also trust email from my Contacts");
                    }
                    ui.horizontal_top(|ui| {
                        egui::Frame::new()
                            .stroke(egui::Stroke::new(1.0, app.theme.separator))
                            .inner_margin(4.0)
                            .show(ui, |ui| {
                                ui.set_width(300.0);
                                ui.set_min_height(200.0);
                                if let Some(list) = st.lists.get(idx) {
                                    for (i, e) in list.iter().enumerate() {
                                        if ui.selectable_label(st.selected == Some(i), e).clicked() {
                                            st.selected = Some(i);
                                        }
                                    }
                                }
                            });
                        ui.vertical(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut st.entry)
                                    .hint_text("name@example.com or @example.com")
                                    .desired_width(170.0),
                            );
                            if btn_en(ui, !st.entry.trim().is_empty(), "Add...") {
                                let e = st.entry.trim().to_lowercase();
                                if let Some(list) = st.lists.get_mut(idx)
                                    && !list.contains(&e)
                                {
                                    list.push(e);
                                    list.sort();
                                }
                                st.entry.clear();
                            }
                            if btn_en(ui, st.selected.is_some(), "Remove")
                                && let (Some(i), Some(list)) = (st.selected.take(), st.lists.get_mut(idx))
                                && i < list.len()
                            {
                                list.remove(i);
                            }
                        });
                    });
                }
                _ => {
                    muted(ui, app, "Block messages by top-level domain or encoding: nothing is blocked.");
                    ui.add_space(120.0);
                }
            }
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    common::set_pref(ctx, "junk.level", json!(st.level));
                    common::set_pref(ctx, "junk.deleteInstead", json!(st.delete));
                    common::set_pref(ctx, "junk.disableLinks", json!(st.links));
                    common::set_pref(ctx, "junk.warnDomains", json!(st.warn));
                    common::set_pref(ctx, "junk.trustContacts", json!(st.trust_contacts));
                    for (k, l) in JUNK_LISTS.iter().zip(st.lists.iter()) {
                        common::set_pref(ctx, k, json!(l));
                    }
                    Out::Close
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
