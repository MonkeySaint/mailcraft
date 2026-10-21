//! @mentions in the body editor (File › Options › Mail › Send messages › "Suggest names to mention when I use
//! the @ symbol in a message"). Typing `@` and the start of a name (at the start of a word) opens a list of
//! people under the caret, from the Auto-Complete list, contacts and the user's accounts
//! (`compose.suggestRecipients`). Up / Down move in it, Enter or Tab (or a click) picks: the typed `@name`
//! becomes an "@Name" link and the person is added to To (`compose.insertMention`). Esc closes the list for
//! that `@`. With the option off nothing is offered.
//!
//! State (the open list and the `@` it belongs to) lives in egui temp memory per compose. The body editor
//! calls [`take_keys`] before it reads its own keys and [`popup`] after painting.

use egui::{Event, Id, Key, Order, Rect, Sense, pos2, vec2};
use serde_json::json;

use mailcraft_engine::compose::{ComposeState, DocPos};

use super::editor::{EditorHost, leaf_chars};

/// Accessible name of the list.
pub const LIST_LABEL: &str = "People to mention";
/// Most people listed.
pub const MAX_PEOPLE: usize = 6;
/// Longest name typed after `@` that is still looked up.
const MAX_QUERY_CHARS: usize = 40;
/// Row height of the list.
const ROW_H: f32 = 24.0;
/// List width.
const LIST_W: f32 = 300.0;

/// The `@` being typed: where it is and what follows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Typed {
    /// Position of the `@`.
    pub at: DocPos,
    /// The chars typed after it, up to the caret.
    pub query: String,
}

#[derive(Clone, Debug, Default)]
struct State {
    /// The list shown last frame: the `@` it is for and the people (name, address).
    open: Option<(Typed, Vec<(String, String)>)>,
    pick: usize,
    /// Esc closed the list for the `@` at this position.
    dismissed: Option<DocPos>,
}

fn id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-mention", cid))
}

fn load(ctx: &egui::Context, cid: u64) -> State {
    ctx.data(|d| d.get_temp::<State>(id(cid))).unwrap_or_default()
}

fn store(ctx: &egui::Context, cid: u64, st: State) {
    ctx.data_mut(|d| d.insert_temp(id(cid), st));
}

/// Whether the mention list of compose `cid` is open.
pub fn is_open(ctx: &egui::Context, cid: u64) -> bool {
    load(ctx, cid).open.is_some()
}

/// The `@name` being typed at the caret: an `@` at the start of the paragraph or after a space or opening
/// bracket, followed by at least one letter or digit (letters, digits, `.`, `-`, `_`, `'` up to the caret).
pub fn typed_at_caret(c: &ComposeState) -> Option<Typed> {
    if !c.selection.is_collapsed() {
        return None;
    }
    let caret = &c.selection.focus;
    let chars = leaf_chars(&c.body, &caret.path);
    let end = caret.offset.min(chars.len());
    let start = end.saturating_sub(MAX_QUERY_CHARS.saturating_add(1));
    let before = chars.get(start..end)?;
    let at = before.iter().rposition(|ch| *ch == '@')?;
    let query: String = before.get(at.saturating_add(1)..)?.iter().collect();
    let word_char = |ch: char| ch.is_alphanumeric() || matches!(ch, '.' | '-' | '_' | '\'');
    if query.is_empty()
        || !query.chars().all(word_char)
        || !query.chars().next().is_some_and(char::is_alphanumeric)
    {
        return None;
    }
    let abs = start.saturating_add(at);
    let prev = abs.checked_sub(1).and_then(|i| chars.get(i)).copied();
    if prev.is_some_and(|p| !(p.is_whitespace() || matches!(p, '(' | '[' | '"' | '\u{201C}'))) {
        return None;
    }
    Some(Typed { at: DocPos::new(caret.path.clone(), abs), query })
}

/// Takes the list's keys out of this frame's input while it is open (before the editor reads its keys); Enter
/// or Tab inserts the highlighted person.
pub(super) fn take_keys(ui: &egui::Ui, host: &mut dyn EditorHost, cid: u64) {
    let ctx = ui.ctx().clone();
    let mut st = load(&ctx, cid);
    let Some((typed, people)) = st.open.clone() else {
        return;
    };
    let mut picked = false;
    let mut dismiss = false;
    let n = people.len();
    ui.input_mut(|i| {
        i.events.retain(|e| {
            let Event::Key { key, pressed: true, modifiers, .. } = e else {
                return true;
            };
            if modifiers.command || modifiers.alt {
                return true;
            }
            match key {
                Key::ArrowDown => st.pick = (st.pick + 1) % n.max(1),
                Key::ArrowUp => st.pick = st.pick.checked_sub(1).unwrap_or(n.saturating_sub(1)),
                Key::Enter | Key::Tab => picked = true,
                Key::Escape => dismiss = true,
                _ => return true,
            }
            false
        });
    });
    if dismiss {
        st = State { dismissed: Some(typed.at), ..State::default() };
    } else if picked && let Some((name, addr)) = people.get(st.pick).cloned() {
        insert(host, cid, &typed, &name, &addr);
        st = State::default();
    }
    store(&ctx, cid, st);
}

fn insert(host: &mut dyn EditorHost, cid: u64, typed: &Typed, name: &str, addr: &str) {
    let replace = typed.query.chars().count().saturating_add(1);
    host.run(
        "compose.insertMention",
        json!({"compose": cid, "address": addr, "name": name, "replace": replace}),
    );
}

/// Opens, refreshes or closes the list for what is typed at the caret, and paints it under `caret` (screen
/// rect of the caret, when known).
pub(super) fn popup(
    ctx: &egui::Context,
    host: &mut dyn EditorHost,
    cid: u64,
    caret: Option<Rect>,
    focused: bool,
) {
    let mut st = load(ctx, cid);
    let typed = host.compose(cid).and_then(typed_at_caret).filter(|_| focused && host.mentions());
    let Some(typed) = typed.filter(|t| st.dismissed.as_ref() != Some(&t.at)) else {
        if st.open.is_some()
            || (st.dismissed.is_some() && host.compose(cid).and_then(typed_at_caret).is_none())
        {
            store(ctx, cid, State::default());
        }
        return;
    };
    let people = match &st.open {
        Some((t, people)) if *t == typed => people.clone(),
        _ => host.mention_suggestions(&typed.query),
    };
    if people.is_empty() {
        if st.open.is_some() {
            store(ctx, cid, State { dismissed: st.dismissed, ..State::default() });
        }
        return;
    }
    if st.open.as_ref().is_none_or(|(t, _)| *t != typed) {
        st.pick = 0;
    }
    st.pick = st.pick.min(people.len().saturating_sub(1));
    st.open = Some((typed.clone(), people.clone()));
    let Some(caret) = caret else {
        store(ctx, cid, st);
        return;
    };
    let (fill, text, selected, separator) = {
        let t = host.theme();
        (t.pane, t.text, t.selected, t.separator)
    };
    let mut clicked: Option<usize> = None;
    let pos = pos2(caret.left(), caret.bottom() + 2.0);
    egui::Area::new(id(cid).with("list")).order(Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).fill(fill).stroke(egui::Stroke::new(1.0, separator)).show(ui, |ui| {
            ui.set_width(LIST_W);
            let (list, _) = ui.allocate_exact_size(vec2(LIST_W, 0.0), Sense::hover());
            ui.interact(list, id(cid).with("label"), Sense::hover())
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, LIST_LABEL));
            for (i, (name, addr)) in people.iter().enumerate() {
                let label =
                    if name.eq_ignore_ascii_case(addr) { addr.clone() } else { format!("{name} <{addr}>") };
                let (r, resp) = ui.allocate_exact_size(vec2(LIST_W, ROW_H), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
                if i == st.pick || resp.hovered() {
                    ui.painter().rect_filled(r, 2.0, selected);
                }
                ui.painter().text(
                    r.left_center() + vec2(6.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    &label,
                    egui::FontId::proportional(13.0),
                    text,
                );
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
        });
    });
    if let Some(i) = clicked
        && let Some((name, addr)) = people.get(i)
    {
        insert(host, cid, &typed, name, addr);
        st = State::default();
        ctx.memory_mut(|m| m.request_focus(super::editor::editor_id(cid)));
    }
    store(ctx, cid, st);
}
