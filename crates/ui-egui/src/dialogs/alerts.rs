//! The New Item Alert window (rules' "display in the New Item Alert window", plan/reference/rules.md) and the
//! rules' sound player.
//!
//! The engine opens `newItemAlerts` once per rule run with `{"alerts": [{item, from, subject, receivedText,
//! text, rule}]}`. All runs share one window: a second `newItemAlerts` adds its lines to the open window's
//! list and closes. Close empties the list; Open Item opens the message and keeps the lines for next time.
//!
//! `ruleSound` (`{"paths": [..]}`) is not a window: the paths are recorded (see [`played_sounds`]) and handed
//! to [`play_sound`], which plays nothing until the app has an audio back end (no audio dependency yet).

use egui::{Context, Id, RichText};
use serde_json::{Value, json};

use crate::App;
use crate::dialogs::common::{Dlg, Out, btn, btn_en, footer, muted, window};

pub(super) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "newItemAlerts" => new_item_alerts(ctx, app, d),
        "ruleSound" => rule_sound(ctx, d),
        _ => return None,
    })
}

/// One line of the window.
#[derive(Clone, Debug, PartialEq)]
struct Alert {
    item: u64,
    from: String,
    subject: String,
    received: String,
    text: String,
    rule: String,
}

impl Alert {
    fn from_json(v: &Value) -> Option<Alert> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        Some(Alert {
            item: v.get("item").and_then(Value::as_u64)?,
            from: s("from"),
            subject: s("subject"),
            received: s("receivedText"),
            text: s("text"),
            rule: s("rule"),
        })
    }
}

/// Most lines kept.
const MAX_LINES: usize = 1_000;

#[derive(Clone, Debug, Default)]
struct Alerts {
    lines: Vec<Alert>,
    selected: Option<usize>,
}

fn list_id() -> Id {
    Id::unique("mc-new-item-alerts")
}

/// The `_dlg` uid of the first open New Item Alert window.
fn primary_uid(app: &App) -> Option<u64> {
    app.ui
        .dialogs
        .iter()
        .find(|(n, _)| n == "newItemAlerts")
        .and_then(|(_, p)| p.get("_dlg").and_then(Value::as_u64))
}

fn new_item_alerts(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    // Each opened window hands its lines to the shared list once.
    let merged = Id::unique(("mc-new-item-alerts-merged", uid));
    if !ctx.data_mut(|m| m.get_temp::<bool>(merged)).unwrap_or(false) {
        ctx.data_mut(|m| m.insert_temp(merged, true));
        let mut list = ctx.data_mut(|m| m.get_temp::<Alerts>(list_id())).unwrap_or_default();
        for a in d.arr("alerts").iter().filter_map(Alert::from_json) {
            if list.lines.len() < MAX_LINES && !list.lines.contains(&a) {
                list.lines.push(a);
            }
        }
        ctx.data_mut(|m| m.insert_temp(list_id(), list));
    }
    if primary_uid(app).is_some_and(|p| p != uid) {
        ctx.data_mut(|m| m.remove::<bool>(merged));
        return Out::Close;
    }
    let mut list = ctx.data_mut(|m| m.get_temp::<Alerts>(list_id())).unwrap_or_default();
    let mut keep_lines = false;
    let out = window(ctx, app, d, "New Item Alerts", 600.0, |ui, app| {
        muted(ui, app, "Messages your rules asked to show here:");
        let frame = egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0);
        let mut open: Option<u64> = None;
        frame.show(ui, |ui| {
            egui::Grid::new(("alerts-head", uid)).num_columns(3).min_col_width(150.0).show(ui, |ui| {
                ui.label(RichText::new("From").strong());
                ui.label(RichText::new("Subject").strong());
                ui.label(RichText::new("Received").strong());
                ui.end_row();
            });
            egui::ScrollArea::vertical()
                .id_salt(("alerts-list", uid))
                .max_height(180.0)
                .min_scrolled_height(180.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (i, a) in list.lines.iter().enumerate() {
                        let selected = list.selected == Some(i);
                        let row = ui.horizontal(|ui| {
                            let r1 = ui.add_sized([150.0, 18.0], egui::Button::selectable(selected, &a.from));
                            let r2 =
                                ui.add_sized([230.0, 18.0], egui::Button::selectable(selected, &a.subject));
                            let r3 =
                                ui.add_sized([150.0, 18.0], egui::Button::selectable(selected, &a.received));
                            r1.union(r2).union(r3)
                        });
                        let resp = row.inner;
                        if resp.clicked() {
                            list.selected = Some(i);
                        }
                        if resp.double_clicked() {
                            open = Some(a.item);
                        }
                    }
                });
        });
        if let Some(a) = list.selected.and_then(|i| list.lines.get(i)) {
            let detail = if a.text.is_empty() { format!("Rule: {}", a.rule) } else { a.text.clone() };
            muted(ui, app, &detail);
        }
        let sel_item = list.selected.and_then(|i| list.lines.get(i)).map(|a| a.item);
        let exists = sel_item.is_some_and(|id| app.s.mailbox.mail.contains(mailcraft_model::ItemId(id)));
        let mut close = false;
        footer(ui, |ui| {
            if btn(ui, "Close") {
                close = true;
            }
            if btn_en(ui, exists, "Open Item") {
                open = sel_item;
            }
        });
        if close {
            Out::Close
        } else if let Some(id) = open {
            // Dialogs are modal: the window steps aside for the read window (routed by `dialogs::open`) and
            // keeps its lines for the next time it opens.
            keep_lines = true;
            Out::close_open("readWindow", json!({"id": id}))
        } else {
            Out::Keep
        }
    });
    if out.keeps() || keep_lines {
        ctx.data_mut(|m| m.insert_temp(list_id(), list));
    } else {
        ctx.data_mut(|m| m.remove::<Alerts>(list_id()));
    }
    if !out.keeps() {
        ctx.data_mut(|m| m.remove::<bool>(merged));
    }
    out
}

/// Sound files the rules asked to play this session (newest last).
#[cfg(test)]
pub(crate) fn played_sounds(ctx: &Context) -> Vec<String> {
    ctx.data_mut(|m| m.get_temp::<Vec<String>>(Id::unique("mc-rule-sounds"))).unwrap_or_default()
}

/// Plays a sound file. MailCraft has no audio back end yet, so this does nothing (the request is recorded
/// by the caller).
#[cfg(not(target_arch = "wasm32"))]
fn play_sound(path: &str) {
    log::debug!("rule sound requested: {path} (no audio back end)");
}

#[cfg(target_arch = "wasm32")]
fn play_sound(_path: &str) {}

fn rule_sound(ctx: &Context, d: &Dlg<'_>) -> Out {
    let paths: Vec<String> = d.arr("paths").iter().filter_map(Value::as_str).map(str::to_owned).collect();
    let id = Id::unique("mc-rule-sounds");
    let mut played = ctx.data_mut(|m| m.get_temp::<Vec<String>>(id)).unwrap_or_default();
    for p in paths {
        play_sound(&p);
        played.push(p);
    }
    let excess = played.len().saturating_sub(50);
    played.drain(..excess);
    ctx.data_mut(|m| m.insert_temp(id, played));
    Out::Close
}
