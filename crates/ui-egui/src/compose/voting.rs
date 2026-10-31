//! Reading-pane tracking bars (our layout; behaviour from plan/reference/options.md › Tracking and
//! ribbon-compose.md › Use Voting Buttons):
//!
//! - a received voting message: "This message has voting buttons." with one button per choice, each
//!   offering "Send the response now" or "Edit the response before sending" (`compose.vote`); once answered,
//!   "You responded Approve on …";
//! - a sent voting message: the reply totals ("Reply totals: Approve 2; Reject 1") with the responses;
//! - a received message that asks for a read receipt (policy "Ask each time"): Send Receipt / Don't Send
//!   (`compose.sendReadReceipt`); with "Always send" the receipt goes out once, without asking, unless the
//!   request must be confirmed (no or another Return-Path, several addresses: RFC 8098 section 2.1);
//! - a sent message with delivery or read receipts back: who it was delivered to and who read it.
//!
//! The reading pane calls [`tracking_bars`] under its other info bars.

use egui::{RichText, Sense, Ui, Vec2};
use serde_json::json;

use mailcraft_engine::cmd::compose::{Tracking, tracking};
use mailcraft_model::ItemId;

use crate::App;

/// What to do with read-receipt requests (Options › Mail › Tracking).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReceiptPolicy {
    Always,
    Never,
    #[default]
    Ask,
}

impl ReceiptPolicy {
    /// From the setting's choice id (`always`, `never`, `ask`).
    pub fn from_setting(id: &str) -> ReceiptPolicy {
        match id {
            "always" => ReceiptPolicy::Always,
            "never" => ReceiptPolicy::Never,
            _ => ReceiptPolicy::Ask,
        }
    }
}

/// Text of the read-receipt request bar.
pub const READ_RECEIPT_REQUEST: &str = "The sender of this message has requested a read receipt.";

#[derive(Clone, Default)]
struct Cache {
    key: (u64, usize, u64),
    tracking: Option<Tracking>,
}

/// Item ids whose read-receipt request was answered (or auto-sent) in this session.
#[derive(Clone, Default)]
struct Answered(Vec<u64>);

fn cache_id() -> egui::Id {
    egui::Id::unique("mailcraft-tracking-cache")
}

fn answered_id() -> egui::Id {
    egui::Id::unique("mailcraft-receipt-answered")
}

/// Tracking state of `id`, recomputed only when the item or the mailbox changed.
fn cached_tracking(ctx: &egui::Context, app: &App, id: ItemId) -> Option<Tracking> {
    let key = (id.0, app.s.mailbox.mail.len(), app.s.mailbox.next_id);
    let hit = ctx.data(|d| d.get_temp::<Cache>(cache_id())).filter(|c| c.key == key);
    if let Some(c) = hit {
        return c.tracking;
    }
    let t = tracking(&app.s, id);
    ctx.data_mut(|d| d.insert_temp(cache_id(), Cache { key, tracking: t.clone() }));
    t
}

fn bar(ui: &mut Ui, app: &mut App, icon: &str, body: impl FnOnce(&mut Ui, &mut App)) {
    let theme = app.theme.clone();
    ui.add_space(6.0);
    egui::Frame::new()
        .fill(theme.info_bar)
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                app.icons.paint(ui, r, icon, theme.text_muted);
                body(ui, app);
            });
        });
}

fn label(ui: &mut Ui, app: &App, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(app.theme.text));
}

/// The tracking bars for message `id` (nothing when it has no voting buttons, receipt request or receipts).
pub fn tracking_bars(ui: &mut Ui, app: &mut App, id: ItemId, policy: ReceiptPolicy) {
    let ctx = ui.ctx().clone();
    let Some(t) = cached_tracking(&ctx, app, id) else {
        return;
    };
    let tz = app.s.tz_offset_min;
    if let Some(text) = t.voting_text(tz) {
        let choices = t.choices.clone();
        let can_vote = !t.sent_by_me && t.my_vote.is_none();
        let responses = t.responses.clone();
        bar(ui, app, "check", |ui, app| {
            label(ui, app, &text);
            if can_vote {
                for choice in &choices {
                    ui.menu_button(RichText::new(choice).size(12.0), |ui| {
                        if ui.button("Send the response now").clicked() {
                            app.run("compose.vote", json!({"id": id.0, "choice": choice}));
                            ui.close();
                        }
                        if ui.button("Edit the response before sending").clicked() {
                            app.run("compose.vote", json!({"id": id.0, "choice": choice, "edit": true}));
                            ui.close();
                        }
                    });
                }
            }
            if !responses.is_empty() {
                ui.menu_button(RichText::new("Responses").size(12.0).color(app.theme.link), |ui| {
                    egui::Grid::new(("vote-responses", id.0)).num_columns(2).show(ui, |ui| {
                        for r in &responses {
                            ui.label(&r.from);
                            ui.label(&r.choice);
                            ui.end_row();
                        }
                    });
                });
            }
        });
    }

    // Read-receipt request.
    let answered = ctx.data(|d| d.get_temp::<Answered>(answered_id())).unwrap_or_default();
    if let Some(to) = t.read_receipt_to.clone()
        && !t.read_receipt_sent
        && !answered.0.contains(&id.0)
    {
        let remember = |ctx: &egui::Context| {
            ctx.data_mut(|d| d.get_temp_mut_or_default::<Answered>(answered_id()).0.push(id.0));
        };
        // "Always send" still asks when the request may not be answered automatically (RFC 8098 2.1).
        let policy =
            if policy == ReceiptPolicy::Always && t.read_receipt_ask { ReceiptPolicy::Ask } else { policy };
        match policy {
            ReceiptPolicy::Never => {}
            ReceiptPolicy::Always => {
                remember(&ctx);
                app.run("compose.sendReadReceipt", json!({"id": id.0, "automatic": true}));
            }
            ReceiptPolicy::Ask => {
                let mut choice: Option<bool> = None;
                bar(ui, app, "read", |ui, app| {
                    label(ui, app, READ_RECEIPT_REQUEST);
                    if ui
                        .button("Send Receipt")
                        .on_hover_text(format!("Send a read receipt to {to}"))
                        .clicked()
                    {
                        choice = Some(true);
                    }
                    if ui.button("Don't Send").clicked() {
                        choice = Some(false);
                    }
                });
                if let Some(send) = choice {
                    remember(&ctx);
                    app.run("compose.sendReadReceipt", json!({"id": id.0, "send": send}));
                }
            }
        }
    }

    // Receipts on a sent message.
    if t.sent_by_me && !t.receipts.is_empty() {
        let delivered = t.receipts.iter().filter(|r| r.delivery.as_deref() == Some("Delivered")).count();
        let failed = t.receipts.iter().filter(|r| r.delivery.as_deref() == Some("Failed")).count();
        let read = t.receipts.iter().filter(|r| r.read.as_deref() == Some("Read")).count();
        let mut parts = Vec::new();
        if delivered > 0 {
            parts.push(format!("delivered to {delivered}"));
        }
        if failed > 0 {
            parts.push(format!("not delivered to {failed}"));
        }
        if read > 0 {
            parts.push(format!("read by {read}"));
        }
        let text = format!("Tracking: {}.", parts.join("; "));
        let rows = t.receipts.clone();
        bar(ui, app, "info", |ui, app| {
            label(ui, app, &text);
            ui.menu_button(RichText::new("Details").size(12.0).color(app.theme.link), |ui| {
                egui::Grid::new(("receipts", id.0)).num_columns(3).show(ui, |ui| {
                    ui.strong("Recipient");
                    ui.strong("Delivery");
                    ui.strong("Read");
                    ui.end_row();
                    for r in &rows {
                        ui.label(&r.recipient);
                        ui.label(r.delivery.as_deref().unwrap_or(""));
                        ui.label(r.read.as_deref().unwrap_or(""));
                        ui.end_row();
                    }
                });
            });
        });
    }
}
