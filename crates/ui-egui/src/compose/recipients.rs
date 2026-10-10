//! Recipient fields (To, Cc, Bcc): Auto-Complete, resolved names, Check Names.
//!
//! - **Auto-Complete**: while a field has focus and its text changes, the entry being typed (after the last
//!   `;`) is matched with [`mailcraft_engine::queries::address_suggestions`] (recently used recipients, then
//!   contacts, then the user's own accounts) and a dropdown opens under the field. Up / Down move the
//!   highlight, Enter or Tab accepts (the entry becomes "Name <address>; "), Escape closes the list, a click
//!   picks a row, and the × on a row removes that address from the Auto-Complete list
//!   (`compose.forgetRecipient`). File › Options › Mail › "Use Auto-Complete List…" (UI preference
//!   `mail.autoComplete`) turns it off.
//! - **Resolved names**: when the field does not have focus, entries that are valid addresses are shown as
//!   underlined name pills (display name, address on hover), the way resolved names look; entries that are not
//!   addresses stay plain text. The field's text (and its accessible value) is unchanged.
//! - **Check Names**: Ctrl+K in a recipient field runs `compose.checkNames`; names it cannot resolve (none or
//!   several matches) open the Check Names dialog.

use egui::text::{CCursor, CCursorRange};
use egui::{
    Color32, CornerRadius, Event, EventFilter, Id, Key, Rect, Sense, Stroke, TextEdit, Ui, pos2, vec2,
};
use serde_json::json;

use mailcraft_engine::queries::{Suggestion, address_suggestions};

use super::{Field, field_id};
use crate::App;

/// Most Auto-Complete rows shown.
pub const MAX_SUGGESTIONS: usize = 8;

/// One entry of a recipient field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The entry as typed.
    pub text: String,
    /// What a resolved entry shows: the display name, else the address.
    pub label: String,
    /// The address of a resolved entry.
    pub address: Option<String>,
}

impl Entry {
    pub fn resolved(&self) -> bool {
        self.address.is_some()
    }
}

/// Splits a recipient field at `;` outside quoted names and angle brackets (trimmed, empty entries dropped).
pub fn split_entries(field: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut quoted, mut escaped, mut angle) = (false, false, false);
    let mut start = 0usize;
    for (i, ch) in field.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ';' if !quoted && !angle => {
                out.extend(field.get(start..i));
                start = i.saturating_add(1);
            }
            _ => {}
        }
    }
    out.extend(field.get(start..));
    out.into_iter().map(str::trim).filter(|x| !x.is_empty()).collect()
}

fn valid_address(addr: &str) -> bool {
    let Some((local, domain)) = addr.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !addr.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';'))
}

/// The entries of a recipient field, resolved where they are valid addresses ("Name <a@b>", "a@b",
/// "Last, First <a@b>").
pub fn entries(field: &str) -> Vec<Entry> {
    split_entries(field)
        .into_iter()
        .take(500)
        .map(|piece| {
            let parsed = match piece.strip_suffix('>').and_then(|p| p.rsplit_once('<')) {
                Some((name, addr)) => {
                    let name = name.trim().trim_matches('"').trim();
                    Some((name.to_owned(), addr.trim().to_owned()))
                }
                None => mailcraft_mime::addr::parse_mailbox(piece)
                    .map(|m| (m.name.unwrap_or_default().trim().to_owned(), m.addr)),
            };
            match parsed.filter(|(_, a)| valid_address(a)) {
                Some((name, addr)) => Entry {
                    text: piece.to_owned(),
                    label: if name.is_empty() { addr.clone() } else { name },
                    address: Some(addr),
                },
                None => Entry { text: piece.to_owned(), label: piece.to_owned(), address: None },
            }
        })
        .collect()
}

/// The field text after accepting `entry` for the entry being typed (everything after the last `;`).
pub fn complete_entry(field: &str, entry: &str) -> String {
    match field.rfind(';') {
        Some(i) => {
            let head = field.get(..=i).unwrap_or_default().trim_end();
            format!("{head} {entry}; ")
        }
        None => format!("{entry}; "),
    }
}

/// The entry being typed (after the last `;`), trimmed.
pub fn typed_entry(field: &str) -> &str {
    field.rsplit(';').next().unwrap_or(field).trim()
}

// ---------------------------------------------------------------------------------------------------------

/// Auto-Complete state of one field (egui temp memory).
#[derive(Clone, Debug, Default)]
struct Ac {
    items: Vec<Suggestion>,
    sel: usize,
    /// Closed with Escape / by accepting, until the text changes again.
    dismissed: bool,
}

fn ac_id(cid: u64, f: Field) -> Id {
    Id::new(("mailcraft-compose-autocomplete", cid, f))
}

fn auto_complete_on(ctx: &egui::Context) -> bool {
    crate::dialogs::ui_pref(ctx, "mail.autoComplete").and_then(|v| v.as_bool()).unwrap_or(true)
}

/// Text of an Auto-Complete row.
pub fn row_label(s: &Suggestion) -> String {
    if s.display.trim().is_empty() || s.display.eq_ignore_ascii_case(&s.address) {
        s.address.clone()
    } else {
        format!("{} <{}>", s.display, s.address)
    }
}

/// Moves the text cursor of field `id` to the end of `text`.
fn cursor_to_end(ctx: &egui::Context, id: Id, text: &str) {
    if let Some(mut state) = TextEdit::load_state(ctx, id) {
        let n = text.chars().count();
        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(n))));
        state.store(ctx, id);
    }
}

/// A recipient field filling `rect` (text edit, resolved-name pills, Auto-Complete dropdown, Ctrl+K).
pub(super) fn address_field(
    ui: &mut Ui,
    app: &mut App,
    cid: u64,
    f: Field,
    rect: Rect,
    label_id: Option<Id>,
) {
    let Some(c) = app.s.composes.get(&cid) else {
        return;
    };
    let ctx = ui.ctx().clone();
    let fid = field_id(cid, f);
    let mut text = f.value(c).to_owned();
    let before = text.clone();
    let focused = ctx.memory(|m| m.has_focus(fid));
    let mut ac: Ac = ctx.data(|d| d.get_temp(ac_id(cid, f))).unwrap_or_default();
    if !focused {
        ac = Ac::default();
    }
    let popup = focused && !ac.dismissed && !ac.items.is_empty();

    // Keys for the dropdown and Ctrl+K, taken before the text edit sees them.
    let mut accept: Option<usize> = None;
    let mut check_names = false;
    if focused {
        let n = ac.items.len();
        ui.input_mut(|i| {
            i.events.retain(|e| match e {
                Event::Key { key: Key::K, pressed: true, modifiers, .. }
                    if modifiers.command && !modifiers.shift && !modifiers.alt =>
                {
                    check_names = true;
                    false
                }
                Event::Key { key, pressed: true, modifiers, .. } if popup && !modifiers.command => {
                    match key {
                        Key::ArrowDown => {
                            ac.sel = (ac.sel + 1).min(n.saturating_sub(1));
                            false
                        }
                        Key::ArrowUp => {
                            ac.sel = ac.sel.saturating_sub(1);
                            false
                        }
                        Key::Enter | Key::Tab => {
                            accept = Some(ac.sel);
                            false
                        }
                        Key::Escape => {
                            ac.dismissed = true;
                            false
                        }
                        _ => true,
                    }
                }
                Event::Key { key: Key::Enter | Key::Tab | Key::Escape, pressed: false, .. } if popup => false,
                _ => true,
            });
        });
    }

    // The text edit (pills painted over it when it does not have focus).
    let edit_rect =
        Rect::from_min_max(pos2(rect.left(), rect.top() + 3.0), pos2(rect.right(), rect.bottom() - 3.0));
    let shown = entries(&text);
    let pills = !focused && shown.iter().any(Entry::resolved);
    let color = if pills { Color32::TRANSPARENT } else { app.theme.text };
    let te = TextEdit::singleline(&mut text)
        .id(fid)
        .frame(egui::Frame::NONE)
        .text_color(color)
        .vertical_align(egui::Align::Center)
        .desired_width(edit_rect.width());
    let mut r = ui.put(edit_rect, te);
    if let Some(l) = label_id {
        r = r.labelled_by(l);
    }
    super::claim_initial_focus(ui, cid, &r);
    let line_col = if r.has_focus() { app.theme.accent } else { app.theme.separator };
    ui.painter().line_segment(
        [pos2(rect.left(), rect.bottom() - 1.0), pos2(rect.right(), rect.bottom() - 1.0)],
        Stroke::new(1.0, line_col),
    );
    if pills {
        paint_pills(ui, app, cid, f, edit_rect, &shown);
    }

    // Typing: store the text and refresh the suggestions.
    if text != before {
        app.run(f.command(), json!({"compose": cid, "value": text}));
        ac.dismissed = false;
        ac.sel = 0;
        ac.items = if auto_complete_on(&ctx) && !typed_entry(&text).is_empty() {
            let mut items = address_suggestions(&app.s, &text, MAX_SUGGESTIONS);
            // Nothing to suggest when the entry already is the only match.
            if items.len() == 1 && items.first().is_some_and(|s| s.entry() == typed_entry(&text)) {
                items.clear();
            }
            items
        } else {
            Vec::new()
        };
    }

    // The dropdown.
    let mut forget: Option<String> = None;
    if r.has_focus() || focused {
        if !ac.dismissed && !ac.items.is_empty() {
            let pos = pos2(rect.left(), rect.bottom() + 1.0);
            let width = rect.width().clamp(240.0, 460.0);
            let theme = app.theme.clone();
            egui::Area::new(Id::new(("mailcraft-compose-autocomplete-list", cid, f)))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).fill(theme.pane).show(ui, |ui| {
                        ui.set_width(width);
                        for (i, s) in ac.items.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let label = row_label(s);
                                let row = ui.add_sized(
                                    vec2(width - 34.0, 24.0),
                                    egui::Button::selectable(i == ac.sel, label).frame_when_inactive(false),
                                );
                                if row.clicked() {
                                    accept = Some(i);
                                }
                                if row.hovered() {
                                    ac.sel = i;
                                }
                                if s.source == mailcraft_engine::queries::SuggestionSource::Recent
                                    && ui
                                        .add(egui::Button::new("×").frame(false).small())
                                        .on_hover_text("Remove from the Auto-Complete list")
                                        .clicked()
                                {
                                    forget = Some(s.address.clone());
                                }
                            });
                        }
                    });
                });
        }
        // Keep Tab / arrows / Escape in this field while the list is open (applies from the next frame).
        if !ac.dismissed && !ac.items.is_empty() {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    fid,
                    EventFilter { tab: true, vertical_arrows: true, horizontal_arrows: false, escape: true },
                );
            });
        }
    }
    if let Some(addr) = forget {
        app.run("compose.forgetRecipient", json!({"address": addr}));
        ac.items.retain(|s| !s.address.eq_ignore_ascii_case(&addr));
        ac.sel = ac.sel.min(ac.items.len().saturating_sub(1));
        ctx.memory_mut(|m| m.request_focus(fid));
    }
    if let Some(i) = accept
        && let Some(s) = ac.items.get(i).cloned()
    {
        let current = app.s.composes.get(&cid).map(|c| f.value(c).to_owned()).unwrap_or_default();
        let new_text = complete_entry(&current, &s.entry());
        app.run(f.command(), json!({"compose": cid, "value": new_text}));
        cursor_to_end(&ctx, fid, &new_text);
        ac = Ac { dismissed: true, ..Ac::default() };
        ctx.memory_mut(|m| m.request_focus(fid));
    }
    if check_names {
        ac = Ac { dismissed: true, ..Ac::default() };
        app.run("compose.checkNames", json!({"compose": cid}));
    }
    ctx.data_mut(|d| d.insert_temp(ac_id(cid, f), ac));
}

/// Resolved names as underlined pills (address on hover), unresolved entries as plain text, in `rect`.
fn paint_pills(ui: &mut Ui, app: &App, cid: u64, f: Field, rect: Rect, shown: &[Entry]) {
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let font = egui::FontId::proportional(14.0);
    let mut x = rect.left() + 4.0;
    let cy = rect.center().y;
    for (i, e) in shown.iter().enumerate() {
        if x > rect.right() {
            break;
        }
        let ink = app.theme.text;
        let galley = painter.layout_no_wrap(e.label.clone(), font.clone(), ink);
        let size = galley.size();
        let pill = Rect::from_min_size(pos2(x, cy - size.y / 2.0 - 2.0), vec2(size.x + 12.0, size.y + 4.0));
        if e.resolved() {
            painter.rect_filled(pill, CornerRadius::same(4), app.theme.hover);
            let tx = pill.left() + 6.0;
            painter.galley(pos2(tx, cy - size.y / 2.0), galley, ink);
            let uy = cy + size.y / 2.0;
            painter.line_segment([pos2(tx, uy), pos2(tx + size.x, uy)], Stroke::new(1.0, ink));
            let full = match &e.address {
                Some(a) if *a != e.label => format!("{} <{a}>", e.label),
                Some(a) => a.clone(),
                None => e.text.clone(),
            };
            let hover = ui.interact(
                pill.intersect(rect),
                Id::new(("mailcraft-recipient", cid, f, i)),
                Sense::hover(),
            );
            let label = e.label.clone();
            hover.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &label));
            hover.on_hover_text(full);
            x = pill.right() + 6.0;
        } else {
            let text = format!("{};", e.label);
            let galley = painter.layout_no_wrap(text, font.clone(), app.theme.text);
            let w = galley.size().x;
            painter.galley(pos2(x + 2.0, cy - size.y / 2.0), galley, app.theme.text);
            x += w + 10.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_resolve_valid_addresses() {
        let e =
            entries("Alice Rivera <alice@example.com>; bob@example.org; Rivera, Al <al@example.net>; carol");
        assert_eq!(e.len(), 4);
        assert_eq!(e[0].label, "Alice Rivera");
        assert_eq!(e[0].address.as_deref(), Some("alice@example.com"));
        assert_eq!(e[1].label, "bob@example.org");
        assert!(e[1].resolved());
        assert_eq!(e[2].label, "Rivera, Al");
        assert!(!e[3].resolved());
        assert_eq!(e[3].label, "carol");
        assert!(entries("x@y; ;;").iter().all(|e| !e.resolved()));
        assert!(entries("").is_empty());
        assert_eq!(split_entries("\"Doe; Jo\" <jo@example.com>; a@example.com").len(), 2);
        // Hostile input stays total.
        let long = "a@b.c;".repeat(10_000);
        assert_eq!(entries(&long).len(), 500);
    }

    #[test]
    fn accepting_replaces_the_typed_entry() {
        assert_eq!(complete_entry("al", "Alan Ng <alan@example.net>"), "Alan Ng <alan@example.net>; ");
        assert_eq!(
            complete_entry("bob@example.org; al", "Alan Ng <alan@example.net>"),
            "bob@example.org; Alan Ng <alan@example.net>; "
        );
        assert_eq!(typed_entry("bob@example.org; al "), "al");
        assert_eq!(typed_entry("bob@example.org;"), "");
    }
}
