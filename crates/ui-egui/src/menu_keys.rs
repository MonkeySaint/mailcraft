//! Access keys of context menus and list menus (keys area): each entry's letter is underlined, and typing it
//! while the menu is open runs the entry (or opens its submenu). When several entries share a letter, each
//! press moves the keyboard focus to the next of them and Enter runs it, as menus do on the desktop.
//!
//! Labels mark the key with `&` ("&Copy", "Mark as Un&read"); `&&` is a literal `&`, and text that is not
//! MailCraft's own (folder, category and quick step names) goes through [`literal`] first. The By Date menu's
//! Arrange by entries take their letter with Alt ([`Access::alt`]).
//!
//! The letters typed in a menu reach only the innermost open menu: [`crate::keys`] leaves plain letters,
//! Enter and Space alone while a popup is open, and the menu takes them out of the input here.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, Event, Id, Key, Stroke, TextStyle, Ui, WidgetText};

/// An entry's access key: an ASCII letter or digit (kept upper-case), typed alone or with Alt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Access {
    pub ch: char,
    pub alt: bool,
}

impl Access {
    pub fn plain(ch: char) -> Option<Access> {
        ch.is_ascii_alphanumeric().then(|| Access { ch: ch.to_ascii_uppercase(), alt: false })
    }

    pub fn alt(ch: char) -> Option<Access> {
        Access::plain(ch).map(|a| Access { alt: true, ..a })
    }
}

/// `text` with its `&` markers doubled, so it shows as typed and has no access key.
pub fn literal(text: &str) -> String {
    text.replace('&', "&&")
}

/// Splits a marked label: ("Mark as Unread", Some(byte index of "r")) for "Mark as Un&read". A `&` before a
/// character that cannot be a key, or at the end, is shown as is.
pub fn parse(label: &str) -> (String, Option<usize>) {
    let mut text = String::with_capacity(label.len());
    let mut at = None;
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            text.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('&') => {
                chars.next();
                text.push('&');
            }
            Some(n) if at.is_none() && n.is_ascii_alphanumeric() => at = Some(text.len()),
            _ => text.push('&'),
        }
    }
    (text, at)
}

/// The access key at byte `at` of `text`.
pub fn key_at(text: &str, at: Option<usize>, alt: bool) -> Option<Access> {
    let ch = text.get(at?..)?.chars().next()?;
    if alt { Access::alt(ch) } else { Access::plain(ch) }
}

/// Byte index of `ch` in `text` for the underline: the first exact match, else the first either-case one.
pub fn find_key(text: &str, ch: char) -> Option<usize> {
    text.find(ch).or_else(|| text.char_indices().find(|(_, c)| c.eq_ignore_ascii_case(&ch)).map(|(i, _)| i))
}

/// The label with the access key underlined (colours follow the widget's state).
pub fn label(ui: &Ui, text: &str, at: Option<usize>) -> WidgetText {
    let Some(at) = at else { return WidgetText::from(text) };
    let Some(key_len) = text.get(at..).and_then(|s| s.chars().next()).map(char::len_utf8) else {
        return WidgetText::from(text);
    };
    let end = at.saturating_add(key_len);
    let (Some(before), Some(key), Some(after)) = (text.get(..at), text.get(at..end), text.get(end..)) else {
        return WidgetText::from(text);
    };
    let font_id = TextStyle::Button.resolve(ui.style());
    let plain = TextFormat { font_id, color: Color32::PLACEHOLDER, ..Default::default() };
    let underlined = TextFormat { underline: Stroke::new(1.0, Color32::PLACEHOLDER), ..plain.clone() };
    let mut job = LayoutJob::default();
    job.append(before, 0.0, plain.clone());
    job.append(key, 0.0, underlined);
    job.append(after, 0.0, plain);
    WidgetText::LayoutJob(job.into())
}

/// What a typed access key does to one entry this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    None,
    /// The only enabled entry with the key: run it (or open its submenu).
    Activate,
    /// One of several entries with the key: it takes the keyboard focus (Enter runs it).
    Focus,
}

/// Entries of one open menu with access keys, as drawn in the previous and the current frame.
#[derive(Clone, Debug, Default)]
struct Table {
    pass: u64,
    rows: Vec<(Access, Id, bool)>,
    prev: Vec<(Access, Id, bool)>,
    /// (entry, the widget drawn for it): the keyboard focus is on widgets.
    widgets: Vec<(Id, Id)>,
    prev_widgets: Vec<(Id, Id)>,
    hit_pass: u64,
    hit: Option<(Id, Hit)>,
}

fn table_id(ui: &Ui) -> Id {
    egui::containers::menu::find_menu_root(ui).id.with("mc-menu-access-keys")
}

/// Records that entry `row` was drawn as widget `widget` (so a shared key moves on from the focused one).
pub fn drawn(ui: &Ui, row: Id, widget: Id) {
    let id = table_id(ui);
    ui.ctx().data_mut(|d| d.get_temp_mut_or_default::<Table>(id).widgets.push((row, widget)));
}

/// Registers entry `row` (with `key`) of the menu `ui` is in, and says what the key typed this frame does
/// to it.
pub fn access(ui: &Ui, row: Id, key: Option<Access>, enabled: bool) -> Hit {
    let ctx = ui.ctx().clone();
    let root = egui::containers::menu::find_menu_root(ui).id;
    let deepest = egui::containers::menu::MenuState::from_id(&ctx, root, |s| s.open_item.is_none());
    let pass = ctx.cumulative_pass_nr();
    let table_id = table_id(ui);
    let mut t: Table = ctx.data_mut(|d| d.get_temp(table_id)).unwrap_or_default();
    if t.pass != pass {
        t.prev = std::mem::take(&mut t.rows);
        t.prev_widgets = std::mem::take(&mut t.widgets);
        t.pass = pass;
    }
    if let Some(k) = key {
        t.rows.push((k, row, enabled));
    }
    if t.hit_pass != pass {
        t.hit_pass = pass;
        t.hit = None;
        if deepest && let Some(typed) = take_key(&ctx) {
            let focused = ctx.memory(|m| m.focused());
            let entry = focused.and_then(|f| t.prev_widgets.iter().find(|(_, w)| *w == f).map(|(r, _)| *r));
            t.hit = resolve(&t.prev, typed, entry);
        }
    }
    let hit = match t.hit {
        Some((id, h)) if id == row => h,
        _ => Hit::None,
    };
    ctx.data_mut(|d| d.insert_temp(table_id, t));
    hit
}

/// Which entry a typed key reaches: the only enabled entry with it, else the next one after the focused
/// entry among those sharing it.
fn resolve(rows: &[(Access, Id, bool)], typed: Access, focused: Option<Id>) -> Option<(Id, Hit)> {
    let matching: Vec<Id> =
        rows.iter().filter(|(k, _, on)| *k == typed && *on).map(|(_, id, _)| *id).collect();
    match matching.as_slice() {
        [] => None,
        [one] => Some((*one, Hit::Activate)),
        many => {
            let next = focused
                .and_then(|f| many.iter().position(|id| *id == f))
                .map_or(0, |i| i.saturating_add(1) % many.len());
            many.get(next).map(|id| (*id, Hit::Focus))
        }
    }
}

/// Takes the first letter or digit typed this frame (alone or with Alt; not with Ctrl / Cmd) out of the
/// input, with the text event it also produced.
fn take_key(ctx: &egui::Context) -> Option<Access> {
    ctx.input_mut(|i| {
        let mut typed = None;
        i.events.retain(|e| match e {
            Event::Key { key, pressed: true, modifiers, .. }
                if typed.is_none() && !modifiers.ctrl && !modifiers.command =>
            {
                match key_char(*key)
                    .and_then(|c| if modifiers.alt { Access::alt(c) } else { Access::plain(c) })
                {
                    Some(a) => {
                        typed = Some(a);
                        false
                    }
                    None => true,
                }
            }
            _ => true,
        });
        // The text the key typed (before or after its key event).
        if let Some(a) = typed {
            let same = |t: &str| t.chars().count() == 1 && t.chars().all(|c| c.eq_ignore_ascii_case(&a.ch));
            i.events.retain(|e| !matches!(e, Event::Text(t) if same(t)));
        }
        typed
    })
}

/// The letter or digit of a key.
pub fn key_char(key: Key) -> Option<char> {
    let name = key.name();
    let mut chars = name.chars();
    let c = chars.next()?;
    (chars.next().is_none() && c.is_ascii_alphanumeric()).then_some(c.to_ascii_uppercase())
}

/// Opens the submenu whose button gets widget id `button` (the menu's `next_auto_id` before it is drawn).
pub fn open_submenu(ui: &Ui, button: Id) {
    use egui::containers::menu::{MenuState, SubMenu};
    let ctx = ui.ctx();
    let root = egui::containers::menu::find_menu_root(ui).id;
    let sub = SubMenu::id_from_widget_id(button);
    // A submenu counts as open only once shown; mark it so the parent keeps it open this frame.
    MenuState::mark_shown(ctx, sub);
    MenuState::from_id(ctx, root, |s| s.open_item = Some(sub));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_parse_their_access_keys() {
        assert_eq!(parse("&Copy"), ("Copy".to_owned(), Some(0)));
        assert_eq!(parse("Mark as Un&read"), ("Mark as Unread".to_owned(), Some(10)));
        assert_eq!(parse("Reply && Delete"), ("Reply & Delete".to_owned(), None));
        assert_eq!(parse(&literal("R&D")), ("R&D".to_owned(), None));
        assert_eq!(parse("Tom & Jerry"), ("Tom & Jerry".to_owned(), None));
        assert_eq!(parse("End&"), ("End&".to_owned(), None));
        let (t, at) = parse("Mark as Un&read");
        assert_eq!(key_at(&t, at, false), Access::plain('r'));
        assert_eq!(find_key("Categories", 'e'), Some(3));
        assert_eq!(find_key("Importance", 'I'), Some(0));
        assert_eq!(find_key("Size", 's'), Some(0));
        assert_eq!(key_char(Key::Q), Some('Q'));
        assert_eq!(key_char(Key::Num7), Some('7'));
        assert_eq!(key_char(Key::Enter), None);
    }

    #[test]
    fn shared_keys_cycle_the_focus() {
        let (a, b, c) = (Id::new("a"), Id::new("b"), Id::new("c"));
        let s = Access::plain('S').unwrap();
        let rows = vec![(s, a, true), (Access::plain('M').unwrap(), b, true), (s, c, true)];
        assert_eq!(resolve(&rows, Access::plain('M').unwrap(), None), Some((b, Hit::Activate)));
        assert_eq!(resolve(&rows, s, None), Some((a, Hit::Focus)));
        assert_eq!(resolve(&rows, s, Some(a)), Some((c, Hit::Focus)));
        assert_eq!(resolve(&rows, s, Some(c)), Some((a, Hit::Focus)));
        assert_eq!(resolve(&rows, Access::alt('S').unwrap(), None), None);
        // A disabled entry is skipped: the other one with the key runs.
        let rows = vec![(s, a, false), (s, c, true)];
        assert_eq!(resolve(&rows, s, None), Some((c, Hit::Activate)));
    }
}
