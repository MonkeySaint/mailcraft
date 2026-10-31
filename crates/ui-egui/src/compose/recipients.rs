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
//! - **Commas**: semicolons always separate recipients. With File › Options › Mail › Send messages › "Commas
//!   can be used to separate multiple message recipients" (`mail.send.commasSeparate`) on, commas outside
//!   quoted names and angle brackets separate them too: a typed separating comma becomes a semicolon (the
//!   engine's separator), and the pills and Auto-Complete split at commas in text that came from elsewhere.
//!   One entry written as "Last, First <address>" stays one recipient either way.

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
    split_entries_with(field, false)
}

/// Options › Mail › Send messages › "Commas can be used to separate multiple message recipients".
pub fn commas_separate(s: &mailcraft_engine::Session) -> bool {
    s.settings.bool("mail.send.commasSeparate")
}

/// One entry written as an unquoted display name holding commas and an angle address ("Rivera, Alice
/// <alice@example.com>"): a single recipient even where commas separate recipients.
fn comma_named(entry: &str) -> bool {
    entry.trim().strip_suffix('>').and_then(|p| p.rsplit_once('<')).is_some_and(|(name, _)| {
        let name = name.trim();
        name.contains(',') && !name.contains(['"', '@', '<', '>', ';'])
    })
}

/// Byte offsets of the separators of a recipient field: `;` outside quoted names and angle brackets, and,
/// when `commas`, `,` likewise (except inside a "Last, First <address>" entry). In order.
fn separators(field: &str, commas: bool) -> Vec<usize> {
    let mut semis = Vec::new();
    let mut comma_at = Vec::new();
    let (mut quoted, mut escaped, mut angle) = (false, false, false);
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
            ';' if !quoted && !angle => semis.push(i),
            ',' if commas && !quoted && !angle => comma_at.push(i),
            _ => {}
        }
    }
    if comma_at.is_empty() {
        return semis;
    }
    // Commas inside an entry that is one comma-named mailbox stay part of its name.
    let mut out = semis.clone();
    let mut start = 0usize;
    let mut commas = comma_at.iter().copied().peekable();
    for end in semis.iter().copied().chain(std::iter::once(field.len())) {
        let whole = field.get(start..end).is_some_and(comma_named);
        while let Some(c) = commas.next_if(|&c| c < end) {
            if !whole {
                out.push(c);
            }
        }
        start = end.saturating_add(1);
    }
    out.sort_unstable();
    out
}

/// [`split_entries`], also splitting at commas when `commas` (`mail.send.commasSeparate`).
pub fn split_entries_with(field: &str, commas: bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for i in separators(field, commas) {
        out.extend(field.get(start..i));
        start = i.saturating_add(1);
    }
    out.extend(field.get(start..));
    out.into_iter().map(str::trim).filter(|x| !x.is_empty()).collect()
}

/// The field with every separating comma turned into `;` (same length, so the text cursor stays put);
/// `None` when it has none. Commas inside quoted names, angle brackets and "Last, First <address>" entries
/// are kept.
pub fn commas_to_semicolons(field: &str) -> Option<String> {
    let bytes = field.as_bytes();
    let commas: Vec<usize> =
        separators(field, true).into_iter().filter(|&i| bytes.get(i) == Some(&b',')).collect();
    if commas.is_empty() {
        return None;
    }
    // Both lists are in order: walk them together.
    let mut next = commas.iter().copied().peekable();
    let mut out = String::with_capacity(field.len());
    for (i, ch) in field.char_indices() {
        if next.peek() == Some(&i) {
            next.next();
            out.push(';');
        } else {
            out.push(ch);
        }
    }
    Some(out)
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
    entries_with(field, false)
}

/// [`entries`], also splitting at commas when `commas` (`mail.send.commasSeparate`).
pub fn entries_with(field: &str, commas: bool) -> Vec<Entry> {
    split_entries_with(field, commas)
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
    Id::unique(("mailcraft-compose-autocomplete", cid, f))
}

/// Options › Mail › "Use Auto-Complete List to suggest names" (engine setting `mail.autoComplete`).
fn auto_complete_on(app: &App) -> bool {
    app.s.settings.bool("mail.autoComplete")
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
    let commas = commas_separate(&app.s);
    let shown = entries_with(&text, commas);
    let pills = !focused && shown.iter().any(Entry::resolved);
    let color = if pills { Color32::TRANSPARENT } else { app.theme.text };
    let te = TextEdit::singleline(&mut text)
        .id(fid)
        .frame(egui::Frame::NONE)
        .text_color(color)
        .align(egui::Align2::LEFT_CENTER)
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

    // Typing: store the text and refresh the suggestions. Where commas separate recipients, a separating comma
    // is stored as the semicolon the rest of compose splits at.
    if text != before {
        if commas && let Some(t) = commas_to_semicolons(&text) {
            text = t;
        }
        app.run(f.command(), json!({"compose": cid, "value": text, "typed": true}));
        ac.dismissed = false;
        ac.sel = 0;
        ac.items = if auto_complete_on(app) && !typed_entry(&text).is_empty() {
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
            egui::Area::new(Id::unique(("mailcraft-compose-autocomplete-list", cid, f)))
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
        let mut current = app.s.composes.get(&cid).map(|c| f.value(c).to_owned()).unwrap_or_default();
        if commas && let Some(t) = commas_to_semicolons(&current) {
            current = t;
        }
        let new_text = complete_entry(&current, &s.entry());
        app.run(f.command(), json!({"compose": cid, "value": new_text, "typed": true}));
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
                Id::unique(("mailcraft-recipient", cid, f, i)),
                Sense::hover(),
            );
            let label = e.label.clone();
            hover.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
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

    #[test]
    fn commas_separate_only_when_the_option_is_on() {
        let field =
            "a@example.com, Bob <b@example.org>; \"Doe, Jo\" <jo@example.com>, Rivera, Al <al@example.net>";
        // Off: semicolons only.
        assert_eq!(split_entries_with(field, false).len(), 2);
        assert_eq!(split_entries(field), split_entries_with(field, false));
        // On: commas outside quotes and angle brackets too.
        assert_eq!(
            split_entries_with(field, true),
            vec![
                "a@example.com",
                "Bob <b@example.org>",
                "\"Doe, Jo\" <jo@example.com>",
                "Rivera",
                "Al <al@example.net>"
            ]
        );
        // One "Last, First <address>" entry stays whole; so do commas inside angle brackets.
        assert_eq!(split_entries_with("Rivera, Alice <alice@example.com>; bob@example.org", true).len(), 2);
        assert_eq!(split_entries_with("x <a,b@example.com>", true), vec!["x <a,b@example.com>"]);
        let e = entries_with("Rivera, Alice <alice@example.com>, bob@example.org", true);
        assert_eq!(e.len(), 3, "{e:?}");
        let e = entries_with("Rivera, Alice <alice@example.com>; bob@example.org, carol@example.net", true);
        assert_eq!(e.iter().filter(|e| e.resolved()).count(), 3);
        assert_eq!(e[0].label, "Rivera, Alice");
        assert_eq!(entries_with("bob@example.org, carol@example.net", false).len(), 1);
    }

    #[test]
    fn separating_commas_become_semicolons() {
        assert_eq!(
            commas_to_semicolons("a@example.com, b@example.org,").as_deref(),
            Some("a@example.com; b@example.org;")
        );
        assert_eq!(
            commas_to_semicolons("\"Doe, Jo\" <jo@example.com>, Rivera, Al <al@example.net>; x").as_deref(),
            Some("\"Doe, Jo\" <jo@example.com>; Rivera; Al <al@example.net>; x")
        );
        assert_eq!(commas_to_semicolons("Rivera, Alice <alice@example.com>; bo"), None);
        assert_eq!(commas_to_semicolons("plain; text"), None);
        assert_eq!(commas_to_semicolons(""), None);
        // Same length, multi-byte text intact.
        let t = "Zo\u{eb}, \u{4e2d}@example.com";
        assert_eq!(commas_to_semicolons(t).map(|x| x.len()), Some(t.len()));
        // Hostile input stays total and linear.
        let long = "a,\"b,".repeat(20_000);
        let _ = commas_to_semicolons(&long);
        assert!(split_entries_with(&",;".repeat(50_000), true).is_empty());
    }

    fn compose_app(commas: bool) -> (crate::App, u64) {
        use mailcraft_engine::compose::{ComposeKind, ComposeState};
        let mut s = mailcraft_engine::Session::new(mailcraft_engine::model::Mailbox::new_local("Local"));
        s.settings.set("mail.send.commasSeparate", &json!(commas)).unwrap();
        let cid =
            s.open_compose(ComposeState { inline: true, kind: ComposeKind::New, ..ComposeState::default() });
        s.notices.clear();
        (crate::App::new(s), cid)
    }

    fn typed_to(commas: bool, typed: &str) -> String {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let (app, cid) = compose_app(commas);
        let mut h = crate::test_harness().with_size([900.0, 700.0]).build_ui_state(
            move |ui, app: &mut crate::App| {
                super::super::inline_compose(ui, app, cid);
            },
            app,
        );
        h.run();
        h.get_by_role_and_label(Role::TextInput, "To").type_text(typed);
        h.run();
        h.state().s.composes.get(&cid).map(|c| c.to.clone()).unwrap_or_default()
    }

    #[test]
    fn typed_commas_follow_the_commas_separate_option() {
        let typed = "ann@example.com, \"Lee, Bo\" <bo@example.org>, cy@example.net";
        assert_eq!(typed_to(false, typed), typed);
        assert_eq!(typed_to(true, typed), "ann@example.com; \"Lee, Bo\" <bo@example.org>; cy@example.net");
    }
}
