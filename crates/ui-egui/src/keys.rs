//! Keyboard: shortcuts → commands with context (main window vs compose vs read window, and by module), Alt
//! keytips (keys area).
//!
//! Bindings follow `plan/reference/keyboard.md` (the accelerator tables and the tested-keys sections) plus each
//! `CommandSpec.shortcut`. Keys that mean different things in different places are resolved by surface and
//! module:
//!
//! | Keys | Main · Mail | Main · other modules | Compose | Read window |
//! |---|---|---|---|---|
//! | Mod+U | Mark as Unread | – | Underline | Mark as Unread |
//! | Mod+A | Select All (list) | – | Select All (body) | – |
//! | Mod+C | Copy items | – | Copy (body) | – |
//! | Mod+F | Forward | People: Forward Contact | – | Forward |
//! | Mod+G | – | Calendar: Go to Date | Go To | – |
//! | Mod+R | Reply | – | Align Right | Reply |
//! | Mod+E | Search | Search | Center | – |
//! | Mod+Y | Go to Folder | Go to Folder | Redo | – |
//! | Delete | Delete message | Delete contact / appointment / task / note | – (body editing) | Delete message |
//! | F11 | Search People (ribbon) | People: Search People | – | – |
//! | Space / Shift+Space | Page through the message, then next / previous item (option) | – | – | – |
//! | F6 / Shift+F6 | Move keyboard focus to the next / previous pane | same | Header fields → body → ribbon | Header → body → info bar → ribbon |
//! | Mod+F1 | Collapse / expand the ribbon | same | – | – |
//! | Mod+< / Mod+> (Mod+, / Mod+.) | – | – | Previous / Next Item | Previous / Next Item |
//!
//! Options (engine settings) change some keys: Mail › "CTRL + ENTER sends a message", Reading Pane ›
//! "Single key reading using space bar", Advanced › "Prompt for confirmation before permanently deleting
//! items" (Shift+Delete asks first).
//!
//! Nothing is taken from a focused text field except Escape, function keys, Mod+Shift+letter / Mod+Alt chords
//! and Mod+digit — and, in compose windows, the formatting shortcuts (Bold, Italic, alignment, link, font
//! size, sub/superscript, Send, Save) and Previous / Next Item.
//!
//! When keyboard focus is on a widget that a click activates (a ribbon button, tab, check box, folder row —
//! reached with Tab or F6), Space and Enter press that widget and the plain arrow keys move focus between
//! widgets: they are not taken for Space-bar reading, Open or list navigation. The message list and the
//! reading pane's body keep their keys while they have focus.
//!
//! F6 moves egui keyboard focus itself (not only a highlight): the selected folder's row, the message list,
//! the message body, the status bar's first control and the selected ribbon tab (Tab then walks the ribbon's
//! controls). Panes with nothing focusable get the focus ring only. Compose and read windows follow the
//! same rule over their own stops ([`window_stops`]): the header fields (To first), the body, the info bars
//! (read windows) and the ribbon's selected tab. Embedded windows share the main window's input; keys
//! pressed while focus (or the F6 ring) is in one go to that window.
//!
//! Call [`handle`] once per frame per surface **before** drawing that surface's widgets, so consumed key
//! events do not also reach the widgets.

use std::collections::BTreeMap;

use egui::{Event, Key};
use serde_json::{Value, json};

use mailcraft_engine::{Module, Registry, Session};

use crate::{App, Surface};

// ---------------------------------------------------------------------------------------------------------
// Chords

/// A key with modifiers in portable form (`Mod` = Ctrl, ⌘ on macOS).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: Key,
    /// Ctrl (⌘ on macOS).
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    /// Plain key without modifiers.
    pub const fn plain(key: Key) -> Chord {
        Chord { key, cmd: false, shift: false, alt: false }
    }

    /// `Mod+key`.
    pub const fn cmd(key: Key) -> Chord {
        Chord { key, cmd: true, shift: false, alt: false }
    }

    /// `Mod+Shift+key`.
    pub const fn cmd_shift(key: Key) -> Chord {
        Chord { key, cmd: true, shift: true, alt: false }
    }

    /// Normalised chord from an egui key event. `+` is folded into `Shift+=` (so `Ctrl+Shift++` and the
    /// numpad plus both match `Mod+Shift+=`).
    pub fn from_event(key: Key, m: egui::Modifiers) -> Chord {
        let cmd = m.command || m.ctrl || m.mac_cmd;
        match key {
            Key::Plus => Chord { key: Key::Equals, cmd, shift: true, alt: m.alt },
            _ => Chord { key, cmd, shift: m.shift, alt: m.alt },
        }
    }

    /// Parses `Mod+Shift+R`, `Ctrl+Alt+1`, `F9`, `Delete`, `Mod+Shift+>`, `Ctrl+*`, `Mod+Shift++`. Modifier
    /// names: Mod / Ctrl / Control / Cmd / Command, Shift, Alt / Option. Shifted symbols (`> < * +`) become the
    /// unshifted key with Shift. `None` when the key is unknown.
    pub fn parse(text: &str) -> Option<Chord> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let (mods, key) = if let Some(rest) = text.strip_suffix("++") {
            (rest, "+")
        } else if text == "+" {
            ("", "+")
        } else {
            match text.rsplit_once('+') {
                Some((m, k)) => (m, k),
                None => ("", text),
            }
        };
        let mut c = Chord::plain(Key::Escape);
        for m in mods.split('+').map(str::trim).filter(|m| !m.is_empty()) {
            match m.to_ascii_lowercase().as_str() {
                "mod" | "ctrl" | "control" | "cmd" | "command" | "⌘" => c.cmd = true,
                "shift" => c.shift = true,
                "alt" | "option" => c.alt = true,
                _ => return None,
            }
        }
        let key = key.trim();
        let (k, shifted) = match key {
            ">" => (Key::Period, true),
            "<" => (Key::Comma, true),
            "*" => (Key::Num8, true),
            "+" | "Plus" => (Key::Equals, true),
            "Del" | "del" => (Key::Delete, false),
            "Ins" | "ins" => (Key::Insert, false),
            "Esc" | "esc" => (Key::Escape, false),
            "PgUp" => (Key::PageUp, false),
            "PgDn" => (Key::PageDown, false),
            other => {
                let k = Key::from_name(other)
                    .or_else(|| Key::from_name(&other.to_ascii_uppercase()))
                    .or_else(|| Key::from_name(&capitalise(other)))?;
                (k, false)
            }
        };
        c.key = k;
        if shifted {
            c.shift = true;
        }
        if c.key == Key::Plus {
            c.key = Key::Equals;
            c.shift = true;
        }
        Some(c)
    }

    /// Portable text: `Mod+Alt+Shift+R`, `F9`, `Mod+Shift+=`.
    pub fn portable(&self) -> String {
        let mut s = String::new();
        if self.cmd {
            s.push_str("Mod+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        s.push_str(key_name(self.key));
        s
    }
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

/// Short name of a key as used in portable shortcuts.
pub fn key_name(key: Key) -> &'static str {
    match key {
        Key::ArrowDown => "Down",
        Key::ArrowUp => "Up",
        Key::ArrowLeft => "Left",
        Key::ArrowRight => "Right",
        Key::Equals => "=",
        Key::Comma => ",",
        Key::Period => ".",
        Key::Minus => "-",
        k => k.name(),
    }
}

/// Every chord a portable shortcut field lists (`"Delete / Mod+D"` → two chords).
pub fn parse_shortcut_list(text: &str) -> Vec<Chord> {
    text.split(" / ").filter_map(Chord::parse).collect()
}

// ---------------------------------------------------------------------------------------------------------
// Context and actions

/// Where a key was pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyContext {
    pub surface: Surface,
    pub module: Module,
    /// A text field (egui `TextEdit`) has keyboard focus.
    pub text_focus: bool,
    /// The search box is active (Search tab shown).
    pub search_active: bool,
    /// The File backstage is open on this surface.
    pub backstage: bool,
    /// A dialog is open.
    pub dialog: bool,
    /// An egui popup / context menu is open (it gets Escape).
    pub popup: bool,
    /// Options › Mail › "CTRL + ENTER sends a message".
    pub ctrl_enter_sends: bool,
    /// Reading Pane options › "Single key reading using space bar".
    pub space_reading: bool,
    /// A non-text widget that a click activates (button, tab, check box, folder row) has keyboard focus:
    /// Space / Enter press it and the arrow keys move focus, so they are not bound here.
    pub widget_focus: bool,
}

impl KeyContext {
    /// Main window, Mail module, nothing open.
    pub fn main(module: Module) -> KeyContext {
        KeyContext {
            surface: Surface::Main,
            module,
            text_focus: false,
            search_active: false,
            backstage: false,
            dialog: false,
            popup: false,
            ctrl_enter_sends: true,
            space_reading: true,
            widget_focus: false,
        }
    }

    /// Compose window `id`, body focused.
    pub fn compose(id: u64) -> KeyContext {
        KeyContext { surface: Surface::Compose(id), ..KeyContext::main(Module::Mail) }
    }

    /// Read window for `item`.
    pub fn read(item: u64) -> KeyContext {
        KeyContext { surface: Surface::Read(item), ..KeyContext::main(Module::Mail) }
    }

    /// Context of `surface` from the app state (focus/popup flags from `ctx`).
    pub fn of(ctx: &egui::Context, app: &App, surface: Surface) -> KeyContext {
        KeyContext {
            surface,
            module: app.s.view.module,
            text_focus: ctx.text_edit_focused(),
            search_active: app.ui.search_active,
            backstage: app.ui.backstage.as_ref().is_some_and(|(s, _)| *s == surface),
            dialog: !app.ui.dialogs.is_empty(),
            popup: ctx.any_popup_open(),
            ctrl_enter_sends: app.s.settings.bool("mail.send.ctrlEnterSends"),
            space_reading: app.s.settings.bool("mail.readingPane.spaceBarReading"),
            widget_focus: widget_focused(ctx),
        }
    }
}

/// Whether keyboard focus is on a widget that Space / Enter activate: enabled, senses clicks but not drags
/// (text fields, the message body and list rows sense drags too), and not the message list body (its own
/// keys are list navigation).
pub fn widget_focused(ctx: &egui::Context) -> bool {
    let Some(id) = ctx.memory(|m| m.focused()) else { return false };
    if ctx.text_edit_focused() || id == crate::list::focus_id() {
        return false;
    }
    ctx.viewport(|v| {
        v.this_pass
            .widgets
            .get(id)
            .or_else(|| v.prev_pass.widgets.get(id))
            .is_some_and(|w| w.enabled && w.sense.senses_click() && !w.sense.senses_drag())
    })
}

/// Keys that belong to a focused button-like widget (see [`KeyContext::widget_focus`]): Space and Enter
/// (egui presses the focused widget with either, Shift or not) and the plain arrows (egui moves focus).
fn goes_to_focused_widget(c: Chord) -> bool {
    if c.cmd || c.alt {
        return false;
    }
    match c.key {
        Key::Space | Key::Enter => true,
        Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight => !c.shift,
        _ => false,
    }
}

/// What a key does.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Run a command.
    Run { id: String, params: Value },
    /// Run a command only when it is enabled (otherwise the key is left alone).
    RunIfEnabled { id: String, params: Value },
    /// Focus the search box (F3 / Mod+E).
    FocusSearch,
    /// Close the topmost thing: dialog, backstage, compose/read window, search.
    Escape,
    /// Show keytips (Alt / F10).
    Keytips,
    /// Calendar view (Mod+Alt+1…5): "day", "workWeek", "week", "month", "schedule".
    CalendarView(&'static str),
    /// Run the quick step bound to Ctrl+Shift+`n`.
    QuickStep(u8),
    /// Apply the category bound to this shortcut ("Ctrl+F2" … "Ctrl+F12").
    Category(String),
    /// Read window: show the next (`true`) or previous item.
    ReadStep(bool),
    /// Focus the ribbon's Search People box (F11; Home tab).
    FocusSearchPeople,
    /// Space-bar reading: page the reading pane, then the next item (`true` = Shift: back).
    SpaceRead(bool),
    /// F6 / Shift+F6 (`true` = back): move focus to the next pane.
    CyclePane(bool),
    /// Compose window: Previous (`false`) / Next (`true`) Item — close it and open the neighbouring message.
    ComposeStep(bool),
    /// Mod+F1: collapse or expand the ribbon.
    ToggleRibbon,
    /// F4: Find in the received message (reading pane or read window).
    FindInMessage,
}

fn run(id: &str) -> Option<Action> {
    Some(Action::Run { id: id.to_owned(), params: json!({}) })
}

fn run_p(id: &str, params: Value) -> Option<Action> {
    Some(Action::Run { id: id.to_owned(), params })
}

/// Treat Folders / Shortcuts like Mail (they show mail folders and the message list).
fn mail_like(m: Module) -> bool {
    matches!(m, Module::Mail | Module::Folders | Module::Shortcuts)
}

/// Keys allowed while a text field has focus.
fn allowed_with_text_focus(c: Chord, surface: Surface) -> bool {
    let fkey = function_key_number(c.key).is_some();
    if fkey || c.key == Key::Escape {
        return true;
    }
    if c.cmd && c.alt {
        return true;
    }
    if c.cmd && c.shift && is_letter(c.key) && c.key != Key::Z {
        return true;
    }
    if c.cmd && !c.shift && !c.alt && digit_of(c.key).is_some() {
        return true;
    }
    if let Surface::Compose(_) = surface {
        return is_compose_formatting(c);
    }
    false
}

/// Compose formatting shortcuts that apply even while a header field has focus.
fn is_compose_formatting(c: Chord) -> bool {
    if !c.cmd {
        return false;
    }
    matches!(
        (c.shift, c.alt, c.key),
        (
            false,
            false,
            Key::B
                | Key::I
                | Key::U
                | Key::E
                | Key::L
                | Key::R
                | Key::J
                | Key::K
                | Key::S
                | Key::Enter
                | Key::Equals
                | Key::Period
                | Key::Comma
        ) | (true, false, Key::Period | Key::Comma | Key::Equals | Key::Num8)
    )
}

fn is_letter(k: Key) -> bool {
    let n = k.name();
    n.len() == 1 && n.chars().all(|c| c.is_ascii_uppercase())
}

fn digit_of(k: Key) -> Option<u8> {
    Some(match k {
        Key::Num0 => 0,
        Key::Num1 => 1,
        Key::Num2 => 2,
        Key::Num3 => 3,
        Key::Num4 => 4,
        Key::Num5 => 5,
        Key::Num6 => 6,
        Key::Num7 => 7,
        Key::Num8 => 8,
        Key::Num9 => 9,
        _ => return None,
    })
}

fn function_key_number(k: Key) -> Option<u8> {
    Some(match k {
        Key::F1 => 1,
        Key::F2 => 2,
        Key::F3 => 3,
        Key::F4 => 4,
        Key::F5 => 5,
        Key::F6 => 6,
        Key::F7 => 7,
        Key::F8 => 8,
        Key::F9 => 9,
        Key::F10 => 10,
        Key::F11 => 11,
        Key::F12 => 12,
        _ => return None,
    })
}

/// Resolves a chord with the built-in bindings (no registry fallback). Honours the text-focus rule.
pub fn resolve(c: Chord, cx: &KeyContext) -> Option<Action> {
    if cx.text_focus && !allowed_with_text_focus(c, cx.surface) {
        return None;
    }
    if cx.widget_focus && goes_to_focused_widget(c) {
        return None;
    }
    if c.key == Key::Escape && !c.cmd && !c.alt && !c.shift {
        // An open popup / context menu closes itself on Escape.
        let nothing_open = cx.surface == Surface::Main && !cx.dialog && !cx.backstage && !cx.search_active;
        return if cx.popup || nothing_open { None } else { Some(Action::Escape) };
    }
    if c == Chord::plain(Key::F10) {
        return Some(Action::Keytips);
    }
    // Modal things take only Escape (and keytips).
    if cx.dialog || cx.backstage {
        return None;
    }
    match cx.surface {
        Surface::Main => resolve_main(c, cx),
        Surface::Compose(id) => resolve_compose(c, id, cx),
        Surface::Read(item) => resolve_read(c, item, cx),
    }
}

/// Resolves with the built-in bindings first, then any registry command whose `shortcut` lists the chord and
/// whose area fits the surface / module.
pub fn resolve_with(c: Chord, cx: &KeyContext, registry: &Registry) -> Option<Action> {
    if let Some(a) = resolve(c, cx) {
        return Some(a);
    }
    // Options › Mail › "CTRL + ENTER sends a message" off: Send's registry shortcut does not apply either.
    if matches!(cx.surface, Surface::Compose(_)) && !cx.ctrl_enter_sends && c == Chord::cmd(Key::Enter) {
        return None;
    }
    if cx.text_focus && !allowed_with_text_focus(c, cx.surface) {
        return None;
    }
    if cx.widget_focus && goes_to_focused_widget(c) {
        return None;
    }
    if cx.dialog || cx.backstage {
        return None;
    }
    registry
        .specs
        .iter()
        .filter(|s| !s.shortcut.is_empty() && parse_shortcut_list(s.shortcut).contains(&c))
        .find(|s| area_fits(s.id, cx))
        .map(|s| {
            let params = match cx.surface {
                Surface::Compose(id) => json!({"compose": id}),
                Surface::Read(item) => json!({"ids": [item]}),
                Surface::Main => json!({}),
            };
            Action::Run { id: s.id.to_owned(), params }
        })
}

/// Whether a command id's area belongs to the surface / module.
fn area_fits(id: &str, cx: &KeyContext) -> bool {
    let area = id.split('.').next().unwrap_or("");
    match cx.surface {
        Surface::Compose(_) => matches!(area, "compose" | "format" | "insert" | "review"),
        Surface::Read(_) => matches!(area, "mail"),
        Surface::Main => match area {
            "mail" | "folder" | "view" | "rules" | "quicksteps" | "categories" => mail_like(cx.module),
            "calendar" => cx.module == Module::Calendar,
            "people" => cx.module == Module::People,
            "tasks" => cx.module == Module::Tasks,
            "notes" => cx.module == Module::Notes,
            "app" | "nav" | "search" | "sendReceive" | "help" | "file" | "options" => true,
            _ => false,
        },
    }
}

fn resolve_main(c: Chord, cx: &KeyContext) -> Option<Action> {
    let m = cx.module;
    let (cmd, shift, alt) = (c.cmd, c.shift, c.alt);
    // Quick steps: Ctrl+Shift+1…9; categories: Ctrl+F2…F12.
    if cmd
        && shift
        && !alt
        && mail_like(m)
        && let Some(n @ 1..=9) = digit_of(c.key)
    {
        return Some(Action::QuickStep(n));
    }
    if cmd
        && !shift
        && !alt
        && mail_like(m)
        && let Some(n @ 2..=12) = function_key_number(c.key)
    {
        return Some(Action::Category(format!("Ctrl+F{n}")));
    }
    // Calendar views: Ctrl+Alt+1…5.
    if cmd && alt && !shift && m == Module::Calendar {
        let view = match digit_of(c.key) {
            Some(1) => Some("day"),
            Some(2) => Some("workWeek"),
            Some(3) => Some("week"),
            Some(4) => Some("month"),
            Some(5) => Some("schedule"),
            _ => None,
        };
        if let Some(v) = view {
            return Some(Action::CalendarView(v));
        }
    }
    match (cmd, alt, shift, c.key) {
        // Function keys.
        (false, false, false, Key::F9) => run("sendReceive.all"),
        (false, false, false, Key::F3) => Some(Action::FocusSearch),
        (false, false, false, Key::F1) => run("help.help"),
        (true, false, false, Key::F1) => Some(Action::ToggleRibbon),
        (false, false, back, Key::F6) => Some(Action::CyclePane(back)),
        (false, false, back, Key::Space) if mail_like(m) && cx.space_reading => Some(Action::SpaceRead(back)),
        // Mail: the ribbon's Search People box (keyboard.md); People keeps its own `people.search`.
        (false, false, false, Key::F11) if mail_like(m) => Some(Action::FocusSearchPeople),
        (true, false, false, Key::E) => Some(Action::FocusSearch),
        // Modules (Ctrl+1…4 observed; Ctrl+5…8 showed no change).
        (true, false, false, Key::Num1) => run("nav.mail"),
        (true, false, false, Key::Num2) => run("nav.calendar"),
        (true, false, false, Key::Num3) => run("nav.people"),
        (true, false, false, Key::Num4) => run("nav.tasks"),
        // Go to.
        (true, false, true, Key::I) => run("nav.inbox"),
        (true, false, true, Key::O) => run("nav.outbox"),
        (true, false, false, Key::Y) => run("nav.goToFolder"),
        // New items (any module).
        (true, false, true, Key::M) => run("mail.newEmail"),
        (true, false, true, Key::A) => run("calendar.newAppointment"),
        (true, false, true, Key::Q) => run("calendar.newMeeting"),
        (true, false, true, Key::C) => run("people.new"),
        (true, false, true, Key::L) => run("people.newGroup"),
        (true, false, true, Key::K) => run("tasks.new"),
        (true, false, true, Key::S) => run_p("mail.newItem", json!({"kind": "post"})),
        (true, true, true, Key::U) => run_p("mail.newItem", json!({"kind": "taskRequest"})),
        (true, false, false, Key::N) => match m {
            Module::Calendar => run("calendar.newAppointment"),
            Module::People => run("people.new"),
            Module::Tasks => run("tasks.new"),
            Module::Notes => run("notes.new"),
            _ => run("mail.newEmail"),
        },
        // Tools.
        (true, false, true, Key::B) => run("compose.addressBook"),
        (true, false, true, Key::F) => run("search.advancedFind"),
        (true, false, true, Key::E) => run("folder.new"),
        (true, false, true, Key::P) => run("folder.newSearchFolder"),
        (true, true, false, Key::M) => run("sendReceive.markToDownload"),
        (true, true, false, Key::U) => run("sendReceive.unmarkToDownload"),
        // Search scope (Search tab).
        (true, true, false, Key::A) if cx.search_active => {
            run_p("search.scope", json!({"scope": "allMailboxes"}))
        }
        (true, true, false, Key::K) if cx.search_active => {
            run_p("search.scope", json!({"scope": "currentFolder"}))
        }
        (true, true, false, Key::Z) if cx.search_active => {
            run_p("search.scope", json!({"scope": "subfolders"}))
        }
        (true, false, false, Key::Z) => run("app.undo"),
        _ => match m {
            Module::Calendar => resolve_calendar(c),
            Module::People => resolve_people(c),
            Module::Tasks => resolve_delete(c, "tasks.delete"),
            Module::Notes => resolve_delete(c, "notes.delete"),
            Module::Mail | Module::Folders | Module::Shortcuts => resolve_mail(c),
        },
    }
}

fn resolve_delete(c: Chord, id: &str) -> Option<Action> {
    match (c.cmd, c.alt, c.shift, c.key) {
        (false, false, false, Key::Delete) | (true, false, false, Key::D) => run(id),
        _ => None,
    }
}

fn resolve_calendar(c: Chord) -> Option<Action> {
    match (c.cmd, c.alt, c.shift, c.key) {
        (true, false, false, Key::G) => run("calendar.goToDate"),
        (true, false, false, Key::T) => run("calendar.today"),
        _ => resolve_delete(c, "calendar.delete"),
    }
}

fn resolve_people(c: Chord) -> Option<Action> {
    match (c.cmd, c.alt, c.shift, c.key) {
        (true, false, false, Key::F) => run_p("people.forward", json!({"as": "contact"})),
        _ => resolve_delete(c, "people.delete"),
    }
}

fn resolve_mail(c: Chord) -> Option<Action> {
    match (c.cmd, c.alt, c.shift, c.key) {
        (true, false, false, Key::R) => run("mail.reply"),
        (true, false, true, Key::R) => run("mail.replyAll"),
        (true, false, false, Key::F) => run("mail.forward"),
        (true, false, false, Key::Q) => run("mail.markRead"),
        (true, false, false, Key::U) => run("mail.markUnread"),
        (true, false, false, Key::A) => run("nav.selectAll"),
        (true, false, false, Key::C) => run("app.copy"),
        (true, false, false, Key::V) => run("app.paste"),
        (false, false, false, Key::Delete) | (true, false, false, Key::D) => run("mail.delete"),
        (false, false, true, Key::Delete) => run("mail.permanentDelete"),
        (true, false, false, Key::Delete) => run("mail.ignore"),
        (false, false, false, Key::Insert) => run("mail.flag"),
        (true, false, true, Key::G) => run("mail.flagCustom"),
        (true, false, true, Key::V) => run("mail.move"),
        (false, false, false, Key::Enter) | (true, false, false, Key::O) => run("mail.open"),
        (false, false, shift, Key::ArrowDown) => run_p("nav.nextItem", json!({"extend": shift})),
        (false, false, shift, Key::ArrowUp) => run_p("nav.prevItem", json!({"extend": shift})),
        (false, false, false, Key::Home) => run("nav.firstItem"),
        (false, false, false, Key::End) => run("nav.lastItem"),
        (false, false, false, Key::F4) => Some(Action::FindInMessage),
        _ => None,
    }
}

fn resolve_compose(c: Chord, id: u64, _cx: &KeyContext) -> Option<Action> {
    let p = || json!({"compose": id});
    let with = |extra: Value| {
        let mut v = json!({"compose": id});
        if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
            o.extend(e);
        }
        v
    };
    match (c.cmd, c.alt, c.shift, c.key) {
        (true, false, false, Key::B) => run_p("format.bold", p()),
        (true, false, false, Key::I) => run_p("format.italic", p()),
        (true, false, false, Key::U) => run_p("format.underline", p()),
        (true, false, false, Key::E) => run_p("format.align", with(json!({"kind": "center"}))),
        (true, false, false, Key::L) => run_p("format.align", with(json!({"kind": "left"}))),
        (true, false, false, Key::R) => run_p("format.align", with(json!({"kind": "right"}))),
        (true, false, false, Key::J) => run_p("format.align", with(json!({"kind": "justify"}))),
        (true, false, false, Key::K) => run_p("insert.link", p()),
        (true, false, true, Key::Period) => run_p("format.growFont", p()),
        // Ctrl+< / Ctrl+> (keyboard.md, compose window): the unshifted , and . keys; with Shift they size
        // the font.
        (true, false, false, Key::Period) => Some(Action::ComposeStep(true)),
        (true, false, false, Key::Comma) => Some(Action::ComposeStep(false)),
        (true, false, true, Key::Comma) => run_p("format.shrinkFont", p()),
        (true, false, false, Key::Equals) => run_p("format.sub", p()),
        (true, false, true, Key::Equals) => run_p("format.sup", p()),
        (true, false, true, Key::Num8) => run_p("format.showMarks", p()),
        (true, true, true, Key::S) => run_p("format.stylesPane", p()),
        (true, true, false, Key::V) => run_p("format.pasteSpecial", p()),
        (true, false, false, Key::H) => run_p("format.replace", p()),
        (true, false, false, Key::G) => run_p("format.goTo", p()),
        (true, false, false, Key::Enter) if _cx.ctrl_enter_sends => {
            Some(Action::RunIfEnabled { id: "compose.send".into(), params: p() })
        }
        (false, true, false, Key::S) => Some(Action::RunIfEnabled { id: "compose.send".into(), params: p() }),
        (true, false, false, Key::S) => run_p("compose.saveDraft", p()),
        (true, false, true, Key::B) => run_p("compose.addressBook", p()),
        (false, false, false, Key::F7) => run_p("review.spelling", p()),
        (true, false, false, Key::Z) => run_p("compose.editUndo", p()),
        (true, false, false, Key::Y) => run_p("compose.editRedo", p()),
        (true, false, false, Key::A) => run_p("format.selectAll", p()),
        (true, false, false, Key::C) => run_p("format.copy", p()),
        (true, false, false, Key::X) => run_p("format.cut", p()),
        (true, false, false, Key::V) => run_p("format.paste", p()),
        (true, false, true, Key::M) => run("mail.newEmail"),
        (false, false, false, Key::F9) => run("sendReceive.all"),
        (false, false, back, Key::F6) => Some(Action::CyclePane(back)),
        _ => None,
    }
}

fn resolve_read(c: Chord, item: u64, _cx: &KeyContext) -> Option<Action> {
    let ids = || json!({"ids": [item]});
    match (c.cmd, c.alt, c.shift, c.key) {
        (true, false, false, Key::R) => run_p("mail.reply", ids()),
        (true, false, true, Key::R) => run_p("mail.replyAll", ids()),
        (true, false, false, Key::F) => run_p("mail.forward", ids()),
        (true, false, false, Key::Q) => run_p("mail.markRead", ids()),
        (true, false, false, Key::U) => run_p("mail.markUnread", ids()),
        (false, false, false, Key::Delete) | (true, false, false, Key::D) => run_p("mail.delete", ids()),
        (false, false, false, Key::Insert) => run_p("mail.flag", ids()),
        (true, false, true, Key::G) => run_p("mail.flagCustom", ids()),
        (true, false, true, Key::V) => run_p("mail.move", ids()),
        (true, false, _, Key::Period) => Some(Action::ReadStep(true)),
        (true, false, _, Key::Comma) => Some(Action::ReadStep(false)),
        (false, false, false, Key::F4) => Some(Action::FindInMessage),
        (true, false, true, Key::M) => run("mail.newEmail"),
        (true, false, true, Key::B) => run("compose.addressBook"),
        (false, false, false, Key::F9) => run("sendReceive.all"),
        (false, false, back, Key::F6) => Some(Action::CyclePane(back)),
        (true, false, false, Key::Z) => run("app.undo"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------------------
// Applying actions

/// Per-context keyboard state kept in egui memory.
#[derive(Clone, Debug, Default)]
struct KeyMemory {
    /// Alt went down with nothing else pressed since.
    alt_clean: bool,
    /// Keytip targets registered by the ribbon, per surface key.
    keytips: BTreeMap<String, Vec<KeytipTarget>>,
    /// A keytip asked for a menu / dropdown to open: (surface key, menu id).
    menu_request: Option<(String, String)>,
    /// Pane focused with F6 ("folders", "list", "reading", …, "statusBar", "ribbon"), until the next click.
    pane: Option<String>,
    /// Compose and read windows: the stop F6 moved to, by surface key ([`Surface::key`]), until the next
    /// click in that window.
    window_pane: BTreeMap<String, String>,
    /// Compose and read windows: their F6 stops as drawn in the last frame, by surface key.
    window_stops: BTreeMap<String, WindowStops>,
}

/// One F6 stop of a compose or read window: its name ("header", "body", "infoBar", "ribbon"), where it was
/// drawn, and the widget F6 focuses there (else the first button inside, or the selected ribbon tab).
#[derive(Clone, Debug)]
pub struct WindowStop {
    pub name: &'static str,
    pub rect: egui::Rect,
    pub target: Option<egui::Id>,
}

/// The F6 stops of a compose or read window, with the layer its widgets are drawn on (the window's own
/// viewport, or an embedded window's layer).
#[derive(Clone, Debug)]
struct WindowStops {
    layer: egui::LayerId,
    stops: Vec<WindowStop>,
}

/// The pane F6 moved focus to, if any (the shell draws a focus ring around it).
pub fn focused_pane(ctx: &egui::Context) -> Option<String> {
    with_mem(ctx, |m| m.pane.clone())
}

/// A focusable widget of the main window's previous frame.
#[derive(Clone, Copy, Debug)]
struct Focusable {
    id: egui::Id,
    /// Visible (clipped) interaction rectangle.
    rect: egui::Rect,
    click: bool,
    drag: bool,
}

/// Enabled, focusable, visible widgets the main window drew in the previous frame, in drawing order (which
/// is also egui's Tab order).
fn focusables(ctx: &egui::Context) -> Vec<Focusable> {
    ctx.viewport(|v| {
        v.prev_pass
            .widgets
            .get_layer(egui::LayerId::background())
            .filter(|w| w.enabled && w.sense.is_focusable() && w.interact_rect.is_positive())
            .map(|w| Focusable {
                id: w.id,
                rect: w.interact_rect,
                click: w.sense.senses_click(),
                drag: w.sense.senses_drag(),
            })
            .collect()
    })
}

/// The main window's F6 stops with their rectangles, in order: the content panes left to right (folder pane,
/// list, reading pane, To-Do Bar, module views), then the status bar and the ribbon (tab row and body) —
/// keyboard.md: "reading pane → status bar → ribbon tabs → …".
fn pane_stops(ctx: &egui::Context) -> Vec<(String, egui::Rect)> {
    let layout = App::shell_layout(ctx);
    let mut out: Vec<(String, egui::Rect)> =
        layout.panes.iter().zip(layout.rects.iter()).map(|(p, r)| ((*p).to_owned(), *r)).collect();
    // Where the shell drew the status bar and the ribbon (tab row and body) in the last frame.
    let bands = crate::chrome::bands(ctx);
    if let Some(r) = bands.status_bar {
        out.push(("statusBar".into(), r));
    }
    if let Some(r) = bands.ribbon {
        out.push(("ribbon".into(), r));
    }
    out
}

/// The stop holding the widget with keyboard focus, if any.
fn pane_of_focus(
    ctx: &egui::Context,
    stops: &[(String, egui::Rect)],
    widgets: &[Focusable],
) -> Option<String> {
    let id = ctx.memory(|m| m.focused())?;
    if id == crate::list::focus_id() && stops.iter().any(|(n, _)| n == "list") {
        return Some("list".into());
    }
    let w = widgets.iter().find(|w| w.id == id)?;
    stops.iter().find(|(_, r)| r.contains(w.rect.center())).map(|(n, _)| n.clone())
}

/// The widget F6 focuses in stop `name`: the selected folder's row, the message list, the message body, the
/// selected ribbon tab; otherwise the first clickable widget inside the stop.
fn focus_target(app: &App, name: &str, rect: egui::Rect, widgets: &[Focusable]) -> Option<egui::Id> {
    let inside = |w: &&Focusable| rect.contains(w.rect.center());
    let present = |id: egui::Id| widgets.iter().any(|w| w.id == id && rect.contains(w.rect.center()));
    match name {
        "list" if present(crate::list::focus_id()) => return Some(crate::list::focus_id()),
        "folders" => {
            let sel = app.s.selection.folder;
            for favorite in [false, true] {
                let id = crate::folders::row_focus_id(sel, favorite);
                if present(id) {
                    return Some(id);
                }
            }
        }
        "reading" => {
            if let Some(w) = widgets.iter().filter(inside).find(|w| w.click && w.drag) {
                return Some(w.id);
            }
        }
        "ribbon" => {
            // Tab row: File, then the tabs in order.
            let band = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.right(), (rect.top() + crate::theme::size::TAB_ROW).min(rect.bottom())),
            );
            let mut tabs: Vec<&Focusable> =
                widgets.iter().filter(|w| w.click && band.contains(w.rect.center())).collect();
            tabs.sort_by(|a, b| a.rect.left().total_cmp(&b.rect.left()));
            let selected = crate::ribbon::selected_tab(app, Surface::Main);
            let pos = crate::ribbon::tabs(app, Surface::Main).iter().position(|t| *t == selected);
            if let Some(w) = pos.and_then(|i| tabs.get(i.saturating_add(1))) {
                return Some(w.id);
            }
        }
        _ => {}
    }
    // Buttons before drag areas (a scroll area's background senses drags only and is never picked).
    widgets
        .iter()
        .filter(inside)
        .find(|w| w.click && !w.drag)
        .or_else(|| widgets.iter().filter(inside).find(|w| w.click))
        .map(|w| w.id)
}

/// F6 / Shift+F6: moves keyboard focus to the next (previous) pane of the main window, in drawing order, then
/// the status bar and the ribbon. The pane is the one after the F6 pane, else after the pane holding the
/// focused widget (a clicked list → reading pane). Returns the pane now focused.
fn cycle_pane(ctx: &egui::Context, app: &App, back: bool) -> Option<String> {
    let stops = pane_stops(ctx);
    if stops.is_empty() {
        return None;
    }
    let widgets = focusables(ctx);
    let cur_name = with_mem(ctx, |m| m.pane.clone()).or_else(|| pane_of_focus(ctx, &stops, &widgets));
    let cur = cur_name.and_then(|p| stops.iter().position(|(n, _)| *n == p));
    let n = stops.len();
    let next = match (cur, back) {
        (None, false) => 0,
        (None, true) => n.saturating_sub(1),
        (Some(i), false) => i.saturating_add(1) % n,
        (Some(i), true) => i.checked_sub(1).unwrap_or(n.saturating_sub(1)),
    };
    let (name, rect) = stops.get(next).cloned()?;
    let target = focus_target(app, &name, rect, &widgets);
    ctx.memory_mut(|m| match target {
        Some(id) => m.request_focus(id),
        // Nothing focusable there: focus leaves the old pane (its keys must not keep working).
        None => {
            if let Some(f) = m.focused() {
                m.surrender_focus(f);
            }
        }
    });
    with_mem(ctx, |m| m.pane = Some(name.clone()));
    ctx.request_repaint();
    Some(name)
}

/// Records the F6 stops a compose or read window drew this frame, in F6 order — the main window's rule
/// (content panes in drawing order, then the status / info bar, then the ribbon): header fields, body,
/// info bar, ribbon — and draws the focus ring around the stop F6 moved to. Stops with an empty rectangle
/// are left out.
pub fn window_stops(ui: &egui::Ui, app: &App, surface: Surface, stops: Vec<WindowStop>) {
    if surface == Surface::Main {
        return;
    }
    let ctx = ui.ctx();
    let stops: Vec<WindowStop> = stops
        .into_iter()
        .filter(|s| s.rect.is_finite() && s.rect.width() >= 1.0 && s.rect.height() >= 1.0)
        .collect();
    let key = surface.key();
    let ring = with_mem(ctx, |m| {
        let ring =
            m.window_pane.get(&key).and_then(|p| stops.iter().find(|s| s.name == p.as_str())).map(|s| s.rect);
        m.window_stops.insert(key.clone(), WindowStops { layer: ui.layer_id(), stops });
        ring
    });
    if let Some(r) = ring {
        ui.painter().with_clip_rect(ui.clip_rect()).rect_stroke(
            r.shrink(1.0),
            egui::CornerRadius::same(crate::theme::size::PANE_RADIUS),
            egui::Stroke::new(2.0, app.theme.accent),
            egui::StrokeKind::Inside,
        );
    }
}

/// The stop of a compose or read window F6 moved to, if any.
pub fn window_pane(ctx: &egui::Context, surface: Surface) -> Option<String> {
    let key = surface.key();
    with_mem(ctx, |m| m.window_pane.get(&key).cloned())
}

/// The compose or read window an embedded window's keyboard focus (or F6 ring) is in: embedded windows
/// share the main window's input, so the main handler hands such keys to that window.
fn embedded_surface(ctx: &egui::Context, app: &App) -> Option<Surface> {
    let focus = ctx.memory(|m| m.focused());
    let layer = focus.and_then(|id| ctx.viewport(|v| v.prev_pass.widgets.get(id).map(|w| w.layer_id)));
    let (stops, panes) = with_mem(ctx, |m| (m.window_stops.clone(), m.window_pane.clone()));
    // Windows still open only (a closed window's last stops may linger).
    let surface_of = |key: &str| {
        let s = match key.split_once(':') {
            Some(("compose", id)) => id.parse().ok().map(Surface::Compose),
            Some(("read", id)) => id.parse().ok().map(Surface::Read),
            _ => None,
        }?;
        let open = match s {
            Surface::Compose(id) => app.s.composes.get(&id).is_some_and(|c| !c.inline),
            Surface::Read(id) => app.ui.read_windows.contains(&id),
            Surface::Main => false,
        };
        open.then_some(s)
    };
    if let Some(layer) = layer.filter(|l| *l != egui::LayerId::background()) {
        return stops.iter().find(|(_, s)| s.layer == layer).and_then(|(k, _)| surface_of(k));
    }
    // A ring-only stop (nothing focusable there) still belongs to its window until the next click.
    if focus.is_none() {
        return panes
            .keys()
            .find(|k| stops.get(*k).is_some_and(|s| s.layer != egui::LayerId::background()))
            .and_then(|k| surface_of(k));
    }
    None
}

/// Enabled, focusable, visible widgets drawn on `layer` in the previous frame, in drawing order.
fn focusables_on(ctx: &egui::Context, layer: egui::LayerId) -> Vec<Focusable> {
    ctx.viewport(|v| {
        v.prev_pass
            .widgets
            .get_layer(layer)
            .filter(|w| w.enabled && w.sense.is_focusable() && w.interact_rect.is_positive())
            .map(|w| Focusable {
                id: w.id,
                rect: w.interact_rect,
                click: w.sense.senses_click(),
                drag: w.sense.senses_drag(),
            })
            .collect()
    })
}

/// The widget F6 focuses in a window stop: its own target when drawn, the selected ribbon tab, the body's
/// text area, else the first button inside.
fn window_target(app: &App, surface: Surface, stop: &WindowStop, widgets: &[Focusable]) -> Option<egui::Id> {
    let inside = |w: &&Focusable| stop.rect.contains(w.rect.center());
    if let Some(t) = stop.target.filter(|t| widgets.iter().any(|w| w.id == *t)) {
        return Some(t);
    }
    match stop.name {
        "ribbon" => {
            let band = egui::Rect::from_min_max(
                stop.rect.min,
                egui::pos2(
                    stop.rect.right(),
                    (stop.rect.top() + crate::theme::size::TAB_ROW).min(stop.rect.bottom()),
                ),
            );
            let mut tabs: Vec<&Focusable> =
                widgets.iter().filter(|w| w.click && band.contains(w.rect.center())).collect();
            tabs.sort_by(|a, b| a.rect.left().total_cmp(&b.rect.left()));
            let names = crate::ribbon::tabs(app, surface);
            let selected = crate::ribbon::selected_tab(app, surface);
            // The tab row may lead with File: the selected tab is counted from the right end.
            let from_end = names.iter().rev().position(|t| *t == selected);
            if let Some(w) =
                from_end.and_then(|i| tabs.len().checked_sub(i.saturating_add(1))).and_then(|i| tabs.get(i))
            {
                return Some(w.id);
            }
        }
        "body" => {
            if let Some(w) = widgets.iter().filter(inside).find(|w| w.click && w.drag) {
                return Some(w.id);
            }
        }
        _ => {}
    }
    widgets
        .iter()
        .filter(inside)
        .find(|w| w.click && !w.drag)
        .or_else(|| widgets.iter().filter(inside).find(|w| w.click))
        .map(|w| w.id)
}

/// F6 / Shift+F6 in a compose or read window: the next (previous) of its stops — header fields, body, info
/// bar, ribbon — after the one F6 moved to, else after the one holding keyboard focus. Returns the stop now
/// focused.
fn cycle_window_pane(ctx: &egui::Context, app: &App, surface: Surface, back: bool) -> Option<String> {
    let key = surface.key();
    let (recorded, current) =
        with_mem(ctx, |m| (m.window_stops.get(&key).cloned(), m.window_pane.get(&key).cloned()));
    let WindowStops { layer, stops } = recorded?;
    if stops.is_empty() {
        return None;
    }
    let widgets = focusables_on(ctx, layer);
    let holding = || {
        let id = ctx.memory(|m| m.focused())?;
        let w = widgets.iter().find(|w| w.id == id)?;
        stops
            .iter()
            .find(|s| s.target == Some(id) || s.rect.contains(w.rect.center()))
            .map(|s| s.name.to_owned())
    };
    let cur = current.or_else(holding).and_then(|p| stops.iter().position(|s| s.name == p.as_str()));
    let n = stops.len();
    let next = match (cur, back) {
        (None, false) => 0,
        (None, true) => n.saturating_sub(1),
        (Some(i), false) => i.saturating_add(1) % n,
        (Some(i), true) => i.checked_sub(1).unwrap_or(n.saturating_sub(1)),
    };
    let stop = stops.get(next)?;
    let target = window_target(app, surface, stop, &widgets);
    ctx.memory_mut(|m| match target {
        Some(id) => m.request_focus(id),
        None => {
            if let Some(f) = m.focused() {
                m.surrender_focus(f);
            }
        }
    });
    let name = stop.name.to_owned();
    with_mem(ctx, |m| m.window_pane.insert(key, name.clone()));
    ctx.request_repaint();
    Some(name)
}

/// Keeps the F6 ring on the pane that holds keyboard focus once Tab has moved it into another pane.
fn follow_focus(ctx: &egui::Context) {
    if with_mem(ctx, |m| m.pane.is_none()) || ctx.memory(|m| m.focused().is_none()) {
        return;
    }
    let stops = pane_stops(ctx);
    let widgets = focusables(ctx);
    if let Some(p) = pane_of_focus(ctx, &stops, &widgets) {
        with_mem(ctx, |m| m.pane = Some(p));
    }
}

fn mem_id() -> egui::Id {
    egui::Id::new("mailcraft.keys")
}

fn with_mem<R>(ctx: &egui::Context, f: impl FnOnce(&mut KeyMemory) -> R) -> R {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default::<KeyMemory>(mem_id())))
}

/// Key used for a surface in `UiState::ribbon_tab` and keytip registrations ([`Surface::key`]).
pub fn surface_key(surface: Surface) -> String {
    surface.key()
}

/// Applies an action; returns a JSON summary (`{"action": …, "result": …}`) for the control channel.
pub fn apply(ctx: &egui::Context, app: &mut App, surface: Surface, action: Action) -> Value {
    match action {
        Action::Run { id, params } => {
            let Some(params) = module_params(app, surface, &id, params) else {
                return json!({"action": "run", "command": id, "ok": false, "disabled": "nothing selected"});
            };
            if id == "mail.permanentDelete" && app.s.settings.bool("advanced.confirmPermanentDelete") {
                // Advanced › "Prompt for confirmation before permanently deleting items".
                let ids: Vec<u64> = app.s.target_ids(&params).iter().map(|i| i.0).collect();
                if ids.is_empty() {
                    return json!({"action": "run", "command": id, "ok": false, "disabled": "nothing selected"});
                }
                let text = if ids.len() == 1 {
                    "The selected item will be permanently deleted.".to_owned()
                } else {
                    format!("The {} selected items will be permanently deleted.", ids.len())
                };
                crate::dialogs::confirm(app, "MailCraft", &text, &id, json!({"ids": ids}));
                return json!({"action": "confirm", "command": id, "ok": true});
            }
            let r = run_cmd(ctx, app, surface, &id, params);
            after_run(ctx, &id, r.as_ref());
            json!({"action": "run", "command": id, "ok": r.is_some(), "result": r})
        }
        Action::RunIfEnabled { id, params } => {
            if let Some(why) = app.s.enabled(&id) {
                return json!({"action": "run", "command": id, "ok": false, "disabled": why});
            }
            let Some(params) = module_params(app, surface, &id, params) else {
                return json!({"action": "run", "command": id, "ok": false, "disabled": "nothing selected"});
            };
            let r = run_cmd(ctx, app, surface, &id, params);
            after_run(ctx, &id, r.as_ref());
            json!({"action": "run", "command": id, "ok": r.is_some(), "result": r})
        }
        Action::FocusSearch => {
            // `search.focus` asks for the "searchBox" dialog; the shell turns that into focus on the box.
            let r = app.run("search.focus", json!({}));
            app.ui.search_active = true;
            json!({"action": "focusSearch", "ok": r.is_some()})
        }
        Action::Escape => json!({"action": "escape", "closed": escape(app, surface)}),
        Action::FocusSearchPeople => {
            // The same path as its keytip (Alt, H, F C): selects Home and focuses the box when drawn.
            let hit = crate::ribbon::keytip(ctx, app, surface, "HFC");
            json!({"action": "focusSearchPeople", "ok": hit == crate::ribbon::KeytipHit::Done})
        }
        Action::SpaceRead(back) => {
            let ok = crate::reading::space_read(ctx, app, back);
            json!({"action": "spaceRead", "ok": ok})
        }
        Action::CyclePane(back) => {
            let pane = if surface == Surface::Main {
                cycle_pane(ctx, app, back)
            } else {
                cycle_window_pane(ctx, app, surface, back)
            };
            json!({"action": "cyclePane", "pane": pane})
        }
        Action::ComposeStep(next) => compose_step(app, surface, next),
        Action::ToggleRibbon => {
            let mode = if app.s.view.ribbon_collapsed || app.s.view.ribbon_auto_hide {
                "always"
            } else {
                "tabsOnly"
            };
            let r = app.run("app.toggleRibbonMode", json!({"mode": mode}));
            json!({"action": "toggleRibbon", "ok": r.is_some(), "result": r})
        }
        Action::Keytips => {
            app.ui.keytips = if app.ui.keytips.is_some() { None } else { Some(String::new()) };
            json!({"action": "keytips", "on": app.ui.keytips.is_some()})
        }
        Action::CalendarView(view) => {
            let r = crate::modules::calendar_command(ctx, app, "calendar.arrange", json!({"mode": view}));
            if r.is_some() {
                app.ui.calendar_view = view.to_owned();
            }
            json!({"action": "calendarView", "view": view, "ok": r.is_some()})
        }
        Action::QuickStep(n) => {
            let want = format!("Ctrl+Shift+{n}");
            let step = app
                .s
                .mailbox
                .quick_steps
                .iter()
                .find(|q| Chord::parse(&q.shortcut).is_some_and(|c| Chord::parse(&want) == Some(c)))
                .map(|q| q.id);
            match step {
                Some(id) => {
                    // In a read window the step acts on the window's message.
                    let params = match surface {
                        Surface::Read(item) => json!({"id": id, "ids": [item]}),
                        _ => json!({"id": id}),
                    };
                    let r = app.run("quicksteps.run", params);
                    json!({"action": "quickStep", "id": id, "ok": r.is_some(), "result": r})
                }
                None => {
                    json!({"action": "quickStep", "ok": false, "error": "no quick step uses this shortcut"})
                }
            }
        }
        Action::Category(shortcut) => {
            let name = app
                .s
                .mailbox
                .categories
                .iter()
                .find(|c| c.shortcut.eq_ignore_ascii_case(&shortcut))
                .map(|c| c.name.clone());
            match name {
                Some(name) => {
                    let r = app.run("mail.categorize", json!({"name": name}));
                    json!({"action": "category", "name": name, "ok": r.is_some()})
                }
                None => json!({"action": "category", "ok": false, "error": "no category uses this shortcut"}),
            }
        }
        Action::ReadStep(next) => {
            let Surface::Read(item) = surface else {
                return json!({"action": "readStep", "ok": false});
            };
            match crate::reading::read_step(app, item, next) {
                Some(t) => json!({"action": "readStep", "ok": true, "item": t}),
                None => json!({"action": "readStep", "ok": false}),
            }
        }
        Action::FindInMessage => {
            // F4: the find bar of the message in this read window, else of the reading pane's message.
            let item = match surface {
                Surface::Read(item) => Some(item),
                _ => app.s.selection.focus.filter(|f| app.s.mailbox.mail.contains_key(f)).map(|f| f.0),
            };
            match item {
                Some(id) => {
                    let r = app.run("format.find", json!({"ids": [id]}));
                    json!({"action": "findInMessage", "ok": r.is_some(), "item": id})
                }
                None => json!({"action": "findInMessage", "ok": false}),
            }
        }
    }
}

/// Compose window Previous / Next Item (keyboard.md, Ctrl+< / Ctrl+>): the message before / after this one in
/// the main window's list — the saved draft, else the message replied to, else the selected message — is
/// selected and opened in a read window, and the compose window closes. Unsaved changes get the save prompt
/// first (nothing moves). Inline responses and new messages with nothing to step from do nothing.
fn compose_step(app: &mut App, surface: Surface, next: bool) -> Value {
    let fail = |why: &str| json!({"action": "composeStep", "ok": false, "error": why});
    let Surface::Compose(cid) = surface else { return fail("not a compose window") };
    let Some((inline, dirty, draft, source)) =
        app.s.composes.get(&cid).map(|c| (c.inline, c.dirty, c.draft, c.source))
    else {
        return fail("no such compose window");
    };
    if inline {
        return fail("inline response");
    }
    let order = mailcraft_engine::queries::visible_item_ids(&app.s);
    let pos = [draft, source, app.s.selection.focus]
        .into_iter()
        .flatten()
        .find_map(|anchor| order.iter().position(|i| *i == anchor));
    let target = pos.and_then(|p| {
        if next { order.get(p.saturating_add(1)) } else { p.checked_sub(1).and_then(|q| order.get(q)) }
    });
    let Some(target) = target.map(|t| t.0) else { return fail("no previous / next item") };
    if dirty {
        let r = app.run("compose.close", json!({"compose": cid}));
        return json!({"action": "composeStep", "ok": false, "prompt": true, "result": r});
    }
    if app.run("compose.close", json!({"compose": cid})).is_none() {
        return fail("cannot close");
    }
    app.run("nav.selectItem", json!({"id": target}));
    let r = app.run("mail.open", json!({"ids": [target]}));
    json!({"action": "composeStep", "ok": r.is_some(), "item": target})
}

/// Runs a command for a key: calendar commands in the main window go through the modules area so the
/// calendar's view state goes in and comes back (`modules::calendar_command`).
fn run_cmd(ctx: &egui::Context, app: &mut App, surface: Surface, id: &str, params: Value) -> Option<Value> {
    // The engine checks a mail command's enablement against the main selection even when `ids` names the
    // read window's item: with nothing (or something else) selected there, select the window's item first.
    if let Surface::Read(item) = surface
        && id.starts_with("mail.")
        && app.s.enabled(id).is_some()
        && app.s.mailbox.mail.contains_key(&mailcraft_model::ItemId(item))
        && !app.s.selection.items.iter().any(|i| i.0 == item)
    {
        let _ = app.run("nav.selectItem", json!({"id": item}));
    }
    if surface == Surface::Main && id.starts_with("calendar.") {
        crate::modules::calendar_command(ctx, app, id, params)
    } else {
        app.run(id, params)
    }
}

/// Item commands of the Calendar / People / Tasks / Notes modules act on the module's selected item
/// (`app.ui.module_selection`), not on the mail selection: adds it as `id` / `ids` when the command takes one
/// and none was given. `None` = a delete with nothing selected (the key does nothing).
fn module_params(app: &App, surface: Surface, id: &str, params: Value) -> Option<Value> {
    if surface != Surface::Main {
        return Some(params);
    }
    let area = match app.s.view.module {
        Module::Calendar => "calendar",
        Module::People => "people",
        Module::Tasks => "tasks",
        Module::Notes => "notes",
        _ => return Some(params),
    };
    if id.split('.').next() != Some(area) {
        return Some(params);
    }
    let spec = app.s.registry.get(id).map(|s| s.params).unwrap_or_default();
    let (takes_ids, takes_id) = (spec.contains("\"ids\""), spec.contains("\"id\""));
    if !takes_ids && !takes_id {
        return Some(params);
    }
    let mut obj = params.as_object().cloned().unwrap_or_default();
    if obj.contains_key("id") || obj.contains_key("ids") {
        return Some(Value::Object(obj));
    }
    let Some(sel) = app.ui.module_selection else {
        // Deleting with nothing selected must not fall through to the mail selection.
        return if id.ends_with(".delete") { None } else { Some(Value::Object(obj)) };
    };
    if takes_ids {
        obj.insert("ids".into(), json!([sel]));
    } else {
        obj.insert("id".into(), json!(sel));
    }
    Some(Value::Object(obj))
}

/// Side effects of some commands on the UI (clipboard text).
fn after_run(ctx: &egui::Context, id: &str, r: Option<&Value>) {
    let Some(r) = r else { return };
    let text = match id {
        "format.copy" | "format.cut" => r.get("text").and_then(Value::as_str),
        "app.copy" => r.get("copied_text").and_then(Value::as_str),
        _ => None,
    };
    if let Some(t) = text.filter(|t| !t.is_empty()) {
        ctx.copy_text(t.to_owned());
    }
}

/// Escape: closes keytips, the topmost dialog, the backstage, the compose / read window, or search (first
/// that applies). Returns what was closed.
fn escape(app: &mut App, surface: Surface) -> &'static str {
    if app.ui.keytips.is_some() {
        app.ui.keytips = None;
        return "keytips";
    }
    if app.ui.dialogs.pop().is_some() {
        return "dialog";
    }
    if app.ui.backstage.is_some() {
        app.ui.backstage = None;
        return "backstage";
    }
    match surface {
        // Escape closes a compose window (with the save prompt), not an inline response.
        Surface::Compose(id) if app.s.composes.get(&id).is_some_and(|c| !c.inline) => {
            app.run("compose.close", json!({"compose": id}));
            "compose"
        }
        Surface::Compose(_) => "nothing",
        Surface::Read(item) => {
            app.ui.read_windows.retain(|w| *w != item);
            "read"
        }
        Surface::Main => {
            if app.ui.search_active || !app.s.view.search.trim().is_empty() {
                app.ui.search_active = false;
                app.ui.search_text.clear();
                if !app.s.view.search.trim().is_empty() {
                    app.run("search.close", json!({}));
                }
                "search"
            } else {
                "nothing"
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Frame handler

/// Reads this frame's key events for `surface`, consumes the ones bound to an action and applies them.
pub fn handle(ctx: &egui::Context, app: &mut App, surface: Surface) {
    // Typing in an inline response (or an embedded compose window) shares the main window's input: those
    // keys belong to the compose surface, not to the message list.
    let surface = match surface {
        Surface::Main => crate::compose::focused_compose(ctx, app)
            .map(Surface::Compose)
            .or_else(|| embedded_surface(ctx, app))
            .unwrap_or(Surface::Main),
        other => other,
    };
    track_alt(ctx, app);
    if ctx.input(|i| i.pointer.any_pressed()) {
        let key = surface.key();
        with_mem(ctx, |m| {
            m.pane = None;
            m.window_pane.remove(&key);
        });
    } else if surface == Surface::Main {
        follow_focus(ctx);
    }
    if app.ui.keytips.is_some() {
        handle_keytips(ctx, app, surface);
        return;
    }
    let cx = KeyContext::of(ctx, app, surface);
    let actions = ctx.input_mut(|i| {
        let mut actions = Vec::new();
        i.events.retain(|e| {
            let (chord, paste_text) = match e {
                Event::Key { key, pressed: true, repeat, modifiers, .. } => {
                    let c = Chord::from_event(*key, *modifiers);
                    // Key repeat only for list navigation and deleting.
                    if *repeat && !matches!(c.key, Key::ArrowDown | Key::ArrowUp | Key::Delete) {
                        return true;
                    }
                    (c, None)
                }
                Event::Copy => (Chord::cmd(Key::C), None),
                Event::Cut => (Chord::cmd(Key::X), None),
                Event::Paste(t) => (Chord::cmd(Key::V), Some(t.clone())),
                _ => return true,
            };
            match resolve_with(chord, &cx, &app.s.registry) {
                Some(mut a) => {
                    if let (Some(text), Action::Run { id, params }) = (paste_text, &mut a)
                        && id == "format.paste"
                        && let Some(o) = params.as_object_mut()
                    {
                        o.insert("text".into(), Value::String(text));
                    }
                    actions.push(a);
                    false
                }
                None => true,
            }
        });
        actions
    });
    for a in actions {
        apply(ctx, app, surface, a);
    }
}

/// Applies a portable key sequence (`"Mod+R"`, `"Alt"`, `"Escape"`, `"Alt H"` for keytips) as if typed on
/// `surface`; used by the control channel. Returns what happened.
pub fn press(ctx: &egui::Context, app: &mut App, surface: Surface, keys: &str) -> Value {
    let mut out = Vec::new();
    for token in keys.split_whitespace() {
        if token.eq_ignore_ascii_case("alt") {
            out.push(apply(ctx, app, surface, Action::Keytips));
            continue;
        }
        if let Some(typed) = app.ui.keytips.clone() {
            out.push(keytip_input(ctx, app, surface, typed, token));
            continue;
        }
        let Some(chord) = Chord::parse(token) else {
            out.push(json!({"key": token, "error": "unknown key"}));
            continue;
        };
        let cx = KeyContext {
            text_focus: false,
            popup: false,
            widget_focus: false,
            ..KeyContext::of(ctx, app, surface)
        };
        match resolve_with(chord, &cx, &app.s.registry) {
            Some(a) => out.push(apply(ctx, app, surface, a)),
            None => out.push(json!({"key": chord.portable(), "action": null})),
        }
    }
    if out.len() == 1 { out.pop().unwrap_or(Value::Null) } else { Value::Array(out) }
}

/// Watches bare Alt presses: Alt down and up with nothing in between toggles keytips.
fn track_alt(ctx: &egui::Context, app: &mut App) {
    let (events, pointer) = ctx.input(|i| (i.events.clone(), i.pointer.any_pressed()));
    let mut toggle = false;
    with_mem(ctx, |m| {
        if pointer {
            m.alt_clean = false;
        }
        for e in &events {
            match e {
                Event::Key {
                    key: Key::AltLeft | Key::AltRight,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } => {
                    m.alt_clean = !(modifiers.ctrl || modifiers.shift || modifiers.command);
                }
                Event::Key { key: Key::AltLeft | Key::AltRight, pressed: false, .. } => {
                    if m.alt_clean {
                        toggle = true;
                    }
                    m.alt_clean = false;
                }
                // Holding Alt (key repeat) keeps it a bare Alt press.
                Event::Key { key: Key::AltLeft | Key::AltRight, .. } => {}
                Event::Key { pressed: true, .. } | Event::Text(_) => m.alt_clean = false,
                _ => {}
            }
        }
    });
    // Seen once per frame: a second handler in the same viewport (embedded compose) must not toggle again.
    ctx.input_mut(|i| {
        i.events.retain(|e| !matches!(e, Event::Key { key: Key::AltLeft | Key::AltRight, .. }));
    });
    if toggle {
        app.ui.keytips = if app.ui.keytips.is_some() { None } else { Some(String::new()) };
    }
    if pointer && app.ui.keytips.is_some() {
        app.ui.keytips = None;
    }
}

// ---------------------------------------------------------------------------------------------------------
// Keytips

/// What a keytip does.
#[derive(Clone, Debug, PartialEq)]
pub enum KeytipAction {
    /// Run a command.
    Run(String, Value),
    /// Select a ribbon tab; keytip mode continues with that tab's controls.
    Tab(String),
    /// Open the File backstage.
    Backstage,
    /// Open a dropdown / menu by id (the ribbon picks it up with [`take_menu_request`]).
    Menu(String),
}

/// One keytip: its key path (`["H", "RP"]` for `Alt, H, R P`), label and action.
#[derive(Clone, Debug, PartialEq)]
pub struct KeytipTarget {
    pub path: Vec<String>,
    pub label: String,
    pub action: KeytipAction,
}

impl KeytipTarget {
    /// From an observed keytip text: `"Alt, H, R P"` → path `["H", "RP"]`.
    pub fn from_text(keytip: &str, label: &str, action: KeytipAction) -> KeytipTarget {
        let path = keytip
            .split(',')
            .map(|p| p.split_whitespace().collect::<String>().to_uppercase())
            .filter(|p| !p.is_empty() && p != "ALT")
            .collect();
        KeytipTarget { path, label: label.to_owned(), action }
    }

    /// The whole sequence typed after Alt: `"HRP"`.
    pub fn sequence(&self) -> String {
        self.path.concat()
    }
}

/// Registers the keytips for `surface` (the ribbon calls this whenever it draws; the last call wins).
pub fn set_keytip_targets(ctx: &egui::Context, surface: Surface, targets: Vec<KeytipTarget>) {
    with_mem(ctx, |m| {
        m.keytips.insert(surface_key(surface), targets);
    });
}

/// Keytips for `surface`: the ribbon's registration plus the built-in top level (File, tabs, Quick Access
/// Toolbar) for anything the ribbon has not registered.
pub fn keytip_targets(ctx: &egui::Context, app: &App, surface: Surface) -> Vec<KeytipTarget> {
    let mut targets = with_mem(ctx, |m| m.keytips.get(&surface_key(surface)).cloned().unwrap_or_default());
    for t in default_keytips(app, surface) {
        if !targets.iter().any(|x| x.path == t.path) {
            targets.push(t);
        }
    }
    targets
}

/// Built-in top-level keytips (`plan/reference/*`: File `F`, Home `H`, Send / Receive `J S`, Folder `O`, View
/// `V`, Help `E`; compose: Message `H`, Insert `N`, Options `P`, Format Text `O`, Review `V`; Quick Access
/// Toolbar `1`, `2`).
pub fn default_keytips(app: &App, surface: Surface) -> Vec<KeytipTarget> {
    let mut tabs: Vec<&str> = crate::ribbon::tabs(app, surface);
    if tabs.is_empty() {
        tabs = match surface {
            Surface::Main => vec!["Home", "Send / Receive", "Folder", "View", "Help"],
            Surface::Compose(_) => vec!["Message", "Insert", "Options", "Format Text", "Review", "Help"],
            Surface::Read(_) => vec!["Message", "Help"],
        };
    }
    let mut out = vec![KeytipTarget::from_text("Alt, F", "File", KeytipAction::Backstage)];
    for tab in tabs {
        let from_ribbon = crate::ribbon::tab_keytip(app, surface, tab);
        let key = match tab {
            _ if !from_ribbon.is_empty() => from_ribbon,
            "Home" | "Message" => "H",
            "Send / Receive" => "J S",
            "Folder" | "Format Text" => "O",
            "View" | "Review" => "V",
            "Insert" => "N",
            "Options" => "P",
            "Help" => "E",
            "Draw" => "D",
            "Search" => "J S 2",
            _ => continue,
        };
        out.push(KeytipTarget::from_text(&format!("Alt, {key}"), tab, KeytipAction::Tab(tab.to_owned())));
    }
    if surface == Surface::Main {
        for (i, id) in app.s.view.quick_access.iter().enumerate().take(9) {
            let n = i.saturating_add(1);
            let label = app.s.registry.get(id).map(|s| s.label).unwrap_or(id.as_str()).to_owned();
            out.push(KeytipTarget::from_text(
                &n.to_string(),
                &label,
                KeytipAction::Run(id.clone(), json!({})),
            ));
        }
    }
    out
}

/// Result of typing a keytip sequence.
#[derive(Clone, Debug, PartialEq)]
pub enum KeytipStep {
    /// More keys needed.
    Pending,
    /// This target fires.
    Fire(KeytipTarget),
    /// Nothing starts with the sequence.
    NoMatch,
}

/// Matches `typed` (upper-case, no separators) against the targets. An exact match fires unless a longer
/// keytip continues it (then more keys are awaited); tab keytips fire at once so their controls show.
pub fn keytip_step(targets: &[KeytipTarget], typed: &str) -> KeytipStep {
    let typed = typed.to_uppercase();
    let exact = targets.iter().find(|t| t.sequence() == typed);
    let longer = targets.iter().any(|t| {
        let s = t.sequence();
        s.len() > typed.len() && s.starts_with(&typed)
    });
    match exact {
        Some(t) if matches!(t.action, KeytipAction::Tab(_)) => KeytipStep::Fire(t.clone()),
        Some(t) if !longer => KeytipStep::Fire(t.clone()),
        _ if targets.iter().any(|t| t.sequence().starts_with(&typed)) => KeytipStep::Pending,
        _ => KeytipStep::NoMatch,
    }
}

/// Takes a pending "open this menu" request made by a keytip for `surface`.
pub fn take_menu_request(ctx: &egui::Context, surface: Surface) -> Option<String> {
    let key = surface_key(surface);
    with_mem(ctx, |m| match &m.menu_request {
        Some((s, _)) if *s == key => m.menu_request.take().map(|(_, id)| id),
        _ => None,
    })
}

fn handle_keytips(ctx: &egui::Context, app: &mut App, surface: Surface) {
    let typed_keys: Vec<String> = ctx.input_mut(|i| {
        let mut out = Vec::new();
        i.events.retain(|e| match e {
            Event::Key { key, pressed: true, modifiers, .. } if !modifiers.command && !modifiers.ctrl => {
                if let Some(d) = digit_of(*key) {
                    out.push(d.to_string());
                } else if is_letter(*key) || matches!(key, Key::Escape | Key::Backspace | Key::F10) {
                    out.push(key.name().to_owned());
                } else {
                    return true;
                }
                false
            }
            // Typed letters are also sent as text; swallow them while keytips are shown.
            Event::Text(_) => false,
            _ => true,
        });
        out
    });
    for k in typed_keys {
        let Some(typed) = app.ui.keytips.clone() else { break };
        keytip_input(ctx, app, surface, typed, &k);
    }
}

/// One key in keytip mode.
fn keytip_input(ctx: &egui::Context, app: &mut App, surface: Surface, typed: String, key: &str) -> Value {
    match key {
        "Escape" | "Esc" | "Backspace" => {
            // Back one level (a selected tab), else leave keytip mode.
            let targets = keytip_targets(ctx, app, surface);
            let parent = targets
                .iter()
                .filter(|t| matches!(t.action, KeytipAction::Tab(_)))
                .map(KeytipTarget::sequence)
                .filter(|s| typed.starts_with(s.as_str()) && s.len() < typed.len())
                .max_by_key(String::len);
            app.ui.keytips = if typed.is_empty() { None } else { Some(parent.unwrap_or_default()) };
            // A dropdown a keytip opened closes with it.
            egui::Popup::close_all(ctx);
            json!({"keytips": app.ui.keytips})
        }
        "F10" => {
            app.ui.keytips = None;
            egui::Popup::close_all(ctx);
            json!({"keytips": null})
        }
        _ => {
            let next = format!("{typed}{}", key.to_uppercase());
            // The ribbon knows its tabs and controls (selects tabs, runs commands, opens dropdowns, focuses
            // edits); File, the Quick Access Toolbar and registered extras are handled here.
            match crate::ribbon::keytip(ctx, app, surface, &next) {
                crate::ribbon::KeytipHit::Tab(name) => {
                    app.ui.keytips = Some(next.clone());
                    return json!({"keytips": next, "tab": name});
                }
                crate::ribbon::KeytipHit::Partial => {
                    app.ui.keytips = Some(next.clone());
                    return json!({"keytips": next});
                }
                crate::ribbon::KeytipHit::Menu => {
                    // The dropdown opens on the next frame; its entries take the following keys.
                    app.ui.keytips = Some(next.clone());
                    return json!({"keytips": next, "menu": true});
                }
                crate::ribbon::KeytipHit::Done => {
                    app.ui.keytips = None;
                    return json!({"keytip": next, "done": true});
                }
                crate::ribbon::KeytipHit::NoMatch => {}
            }
            let targets = keytip_targets(ctx, app, surface);
            match keytip_step(&targets, &next) {
                KeytipStep::Pending => {
                    app.ui.keytips = Some(next.clone());
                    json!({"keytips": next})
                }
                KeytipStep::NoMatch => json!({"keytips": typed, "match": false}),
                KeytipStep::Fire(t) => fire_keytip(ctx, app, surface, t, next),
            }
        }
    }
}

fn fire_keytip(
    ctx: &egui::Context,
    app: &mut App,
    surface: Surface,
    t: KeytipTarget,
    typed: String,
) -> Value {
    match t.action {
        KeytipAction::Tab(tab) => {
            crate::ribbon::select_tab(app, surface, &tab);
            app.ui.keytips = Some(typed);
            json!({"keytip": t.label, "tab": tab})
        }
        KeytipAction::Backstage => {
            app.ui.keytips = None;
            app.ui.backstage = Some((surface, "info".to_owned()));
            json!({"keytip": t.label, "backstage": true})
        }
        KeytipAction::Menu(id) => {
            app.ui.keytips = None;
            with_mem(ctx, |m| m.menu_request = Some((surface_key(surface), id.clone())));
            json!({"keytip": t.label, "menu": id})
        }
        KeytipAction::Run(id, params) => {
            app.ui.keytips = None;
            let params = match (surface, params) {
                (Surface::Compose(cid), Value::Object(mut o)) if is_compose_area(&id) => {
                    o.entry("compose").or_insert(json!(cid));
                    Value::Object(o)
                }
                (_, p) => p,
            };
            let r = app.run(&id, params);
            after_run(ctx, &id, r.as_ref());
            json!({"keytip": t.label, "command": id, "ok": r.is_some()})
        }
    }
}

fn is_compose_area(id: &str) -> bool {
    matches!(id.split('.').next(), Some("compose" | "format" | "insert" | "review"))
}

/// Whether a session command is currently enabled (helper for callers showing shortcut hints).
pub fn enabled(s: &Session, id: &str) -> bool {
    s.enabled(id).is_none()
}

/// The portable shortcut text of a command for display ("Ctrl+Shift+R"; first of several), or "".
pub fn shortcut_hint(s: &Session, id: &str) -> String {
    let Some(spec) = s.registry.get(id) else { return String::new() };
    parse_shortcut_list(spec.shortcut).first().map(display_chord).unwrap_or_default()
}

/// Human text for a chord: `Ctrl+Shift+R` (`⌘` on macOS).
pub fn display_chord(c: &Chord) -> String {
    let m = if cfg!(target_os = "macos") { "⌘+" } else { "Ctrl+" };
    let mut s = String::new();
    if c.cmd {
        s.push_str(m);
    }
    if c.alt {
        s.push_str("Alt+");
    }
    if c.shift {
        s.push_str("Shift+");
    }
    s.push_str(match c.key {
        Key::Delete => "Del",
        k => key_name(k),
    });
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_engine::sample::sample_session;

    fn chord(text: &str) -> Chord {
        Chord::parse(text).unwrap()
    }

    #[test]
    fn option_dependent_bindings() {
        let main = KeyContext::main(Module::Mail);
        assert_eq!(resolve(chord("Space"), &main), Some(Action::SpaceRead(false)));
        assert_eq!(resolve(chord("Shift+Space"), &main), Some(Action::SpaceRead(true)));
        let off = KeyContext { space_reading: false, ..main };
        assert_eq!(resolve(chord("Space"), &off), None);
        assert_eq!(resolve(chord("Space"), &KeyContext::main(Module::Calendar)), None);
        let typing = KeyContext { text_focus: true, ..main };
        assert_eq!(resolve(chord("Space"), &typing), None, "typing a space is not a command");
        assert_eq!(resolve(chord("F6"), &main), Some(Action::CyclePane(false)));
        assert_eq!(resolve(chord("Shift+F6"), &main), Some(Action::CyclePane(true)));
        assert_eq!(resolve(chord("Mod+F1"), &main), Some(Action::ToggleRibbon));
        let s = sample_session();
        let compose = KeyContext::compose(1);
        assert!(matches!(
            resolve_with(chord("Mod+Enter"), &compose, &s.registry),
            Some(Action::RunIfEnabled { .. })
        ));
        let no_ctrl_enter = KeyContext { ctrl_enter_sends: false, ..compose };
        assert_eq!(resolve_with(chord("Mod+Enter"), &no_ctrl_enter, &s.registry), None);
        // Alt+S still sends.
        assert!(resolve_with(chord("Alt+S"), &no_ctrl_enter, &s.registry).is_some());
    }

    #[test]
    fn applying_new_actions() {
        let ctx = egui::Context::default();
        let mut app = App::new(sample_session());
        // Space with nothing focused selects the first message.
        apply(&ctx, &mut app, Surface::Main, Action::SpaceRead(false));
        assert!(app.s.selection.focus.is_some());
        // Ctrl+F1 collapses and expands the ribbon.
        apply(&ctx, &mut app, Surface::Main, Action::ToggleRibbon);
        assert!(app.s.view.ribbon_collapsed);
        apply(&ctx, &mut app, Surface::Main, Action::ToggleRibbon);
        assert!(!app.s.view.ribbon_collapsed);
        // F6 focuses a pane, Shift+F6 goes back (the stops are where the last frame drew the panes, the status
        // bar and the ribbon).
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.ui_root(ui));
        out.textures_delta.clear();
        let first = apply(&ctx, &mut app, Surface::Main, Action::CyclePane(false));
        assert!(first["pane"].is_string());
        assert_eq!(focused_pane(&ctx).as_deref(), first["pane"].as_str());
        // Shift+Delete asks first (Advanced › confirm before permanently deleting).
        let item = app.s.selection.focus.unwrap();
        let r = apply(
            &ctx,
            &mut app,
            Surface::Main,
            Action::Run { id: "mail.permanentDelete".into(), params: json!({}) },
        );
        assert_eq!(r["action"], "confirm");
        assert!(app.s.mailbox.mail.contains_key(&item));
        assert!(app.ui.dialogs.iter().any(|(n, p)| n == "confirm" && p["params"]["ids"][0] == item.0));
        app.ui.dialogs.clear();
        app.run("options.set", json!({"key": "advanced.confirmPermanentDelete", "value": false}));
        apply(
            &ctx,
            &mut app,
            Surface::Main,
            Action::Run { id: "mail.permanentDelete".into(), params: json!({"ids": [item.0]}) },
        );
        assert!(!app.s.mailbox.mail.contains_key(&item));
    }

    #[test]
    fn focused_button_gets_space_and_enter() {
        let main = KeyContext { widget_focus: true, ..KeyContext::main(Module::Mail) };
        for keys in ["Space", "Shift+Space", "Enter", "Up", "Down", "Left", "Right"] {
            assert_eq!(resolve(chord(keys), &main), None, "{keys} presses / moves from the focused widget");
        }
        let s = sample_session();
        assert_eq!(resolve_with(chord("Enter"), &main, &s.registry), None);
        // Everything else still works: shortcuts, list selection extension, F6.
        assert_eq!(resolve(chord("Mod+R"), &main), run("mail.reply"));
        assert!(matches!(resolve(chord("Shift+Down"), &main), Some(Action::Run { .. })));
        assert_eq!(resolve(chord("F6"), &main), Some(Action::CyclePane(false)));
        // Without a focused widget Space reads and Enter opens.
        let plain = KeyContext::main(Module::Mail);
        assert_eq!(resolve(chord("Space"), &plain), Some(Action::SpaceRead(false)));
        assert_eq!(resolve(chord("Enter"), &plain), run("mail.open"));
        // Compose: Ctrl+Enter still sends from a focused button.
        let compose = KeyContext { widget_focus: true, ..KeyContext::compose(3) };
        assert!(matches!(resolve(chord("Mod+Enter"), &compose), Some(Action::RunIfEnabled { .. })));
        // Nothing focused in a fresh context.
        assert!(!widget_focused(&egui::Context::default()));
    }

    #[test]
    fn previous_next_item_in_compose_and_read_windows() {
        let compose = KeyContext::compose(7);
        assert_eq!(resolve(chord("Mod+."), &compose), Some(Action::ComposeStep(true)));
        assert_eq!(resolve(chord("Mod+,"), &compose), Some(Action::ComposeStep(false)));
        // With Shift they are Grow / Shrink Font (Ctrl+Shift+> / Ctrl+Shift+<).
        assert_eq!(resolve(chord("Mod+>"), &compose), run_p("format.growFont", json!({"compose": 7})));
        assert_eq!(resolve(chord("Mod+<"), &compose), run_p("format.shrinkFont", json!({"compose": 7})));
        // Also while typing in the body or a header field.
        let typing = KeyContext { text_focus: true, ..compose };
        assert_eq!(resolve(chord("Mod+."), &typing), Some(Action::ComposeStep(true)));
        let read = KeyContext::read(5);
        assert_eq!(resolve(chord("Mod+."), &read), Some(Action::ReadStep(true)));
        assert_eq!(resolve(chord("Mod+,"), &read), Some(Action::ReadStep(false)));
    }

    #[test]
    fn compose_step_opens_the_neighbour_and_closes_the_window() {
        let ctx = egui::Context::default();
        let mut app = App::new(sample_session());
        let order = mailcraft_engine::queries::visible_item_ids(&app.s);
        let (first, second) = (order[0], order[1]);
        app.run("nav.selectItem", json!({"id": first.0}));
        let cid = app.run("mail.newEmail", json!({})).unwrap()["compose"].as_u64().unwrap();
        let surface = Surface::Compose(cid);
        // Nothing before the first message.
        let r = apply(&ctx, &mut app, surface, Action::ComposeStep(false));
        assert_eq!(r["ok"], false);
        assert!(app.s.composes.contains_key(&cid));
        // Unsaved changes: the save prompt comes first and nothing moves.
        app.run("compose.setSubject", json!({"compose": cid, "value": "Draft"}));
        let r = apply(&ctx, &mut app, surface, Action::ComposeStep(true));
        assert_eq!(r["prompt"], true);
        assert!(app.s.composes.contains_key(&cid));
        assert_eq!(app.s.selection.focus, Some(first));
        app.s.notices.clear();
        app.s.composes.get_mut(&cid).unwrap().dirty = false;
        // Next Item: the window closes, the next message is selected and opens.
        let r = apply(&ctx, &mut app, surface, Action::ComposeStep(true));
        assert_eq!(r["ok"], true, "{r}");
        assert!(!app.s.composes.contains_key(&cid));
        assert_eq!(app.s.selection.focus, Some(second));
        crate::dialogs::take_notices(&mut app);
        assert_eq!(app.ui.read_windows, vec![second.0]);
        // Not a compose window / gone: nothing happens.
        assert_eq!(apply(&ctx, &mut app, surface, Action::ComposeStep(true))["ok"], false);
        assert_eq!(apply(&ctx, &mut app, Surface::Main, Action::ComposeStep(true))["ok"], false);
    }

    fn shell(w: f32, h: f32) -> egui_kittest::Harness<'static, App> {
        let mut app = App::new(sample_session());
        let first = mailcraft_engine::queries::visible_item_ids(&app.s)[0];
        app.run("nav.selectItem", json!({"id": first.0}));
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(w, h))
            .build_ui_state(|ui, app: &mut App| app.ui_root(ui), app);
        h.run_steps(4);
        h
    }

    fn focus_of(h: &egui_kittest::Harness<'static, App>) -> Option<egui::Id> {
        h.ctx.memory(|m| m.focused())
    }

    fn press(h: &mut egui_kittest::Harness<'static, App>, m: egui::Modifiers, key: Key) {
        h.key_press_modifiers(m, key);
        h.run_steps(3);
    }

    #[test]
    fn f6_moves_keyboard_focus_through_the_panes() {
        let mut h = shell(1400.0, 900.0);
        let folder = h.state().s.selection.folder;
        let mut seen = Vec::new();
        for _ in 0..8 {
            press(&mut h, egui::Modifiers::NONE, Key::F6);
            let pane = focused_pane(&h.ctx).unwrap();
            let id = focus_of(&h);
            match pane.as_str() {
                "folders" => assert!(
                    [false, true]
                        .into_iter()
                        .any(|fav| id == Some(crate::folders::row_focus_id(folder, fav))),
                    "the selected folder's row has focus"
                ),
                "list" => assert!(crate::list::has_focus(&h.ctx)),
                "reading" => {
                    // The message body (senses clicks and drags), so Space keeps paging.
                    let w = h.ctx.viewport(|v| v.prev_pass.widgets.get(id.unwrap()).copied()).unwrap();
                    assert!(w.sense.senses_click() && w.sense.senses_drag());
                    assert!(!widget_focused(&h.ctx));
                }
                "ribbon" | "statusBar" => assert!(widget_focused(&h.ctx), "{pane}: a button has focus"),
                _ => {}
            }
            if seen.contains(&pane) {
                break;
            }
            seen.push(pane);
        }
        let at = |p: &str| seen.iter().position(|s| s == p).unwrap();
        assert!(at("folders") < at("list") && at("list") < at("reading"), "{seen:?}");
        assert!(at("reading") < at("statusBar") && at("statusBar") < at("ribbon"), "{seen:?}");
        // Back to the folder pane wrapped around; Shift+F6 goes the other way.
        assert_eq!(focused_pane(&h.ctx).as_deref(), Some("folders"));
        press(&mut h, egui::Modifiers::SHIFT, Key::F6);
        assert_eq!(focused_pane(&h.ctx).as_deref(), Some("ribbon"));
        press(&mut h, egui::Modifiers::SHIFT, Key::F6);
        assert_eq!(focused_pane(&h.ctx).as_deref(), Some("statusBar"));
    }

    #[test]
    fn ribbon_is_keyboard_operable_after_f6() {
        let mut h = shell(1400.0, 900.0);
        let focus = h.state().s.selection.focus;
        // Shift+F6 from nowhere: the last stop, the ribbon, with the selected tab (Home) focused.
        press(&mut h, egui::Modifiers::SHIFT, Key::F6);
        assert_eq!(focused_pane(&h.ctx).as_deref(), Some("ribbon"));
        assert!(widget_focused(&h.ctx));
        let home = crate::ribbon::selected_tab(h.state(), Surface::Main);
        // Tab walks to the next tab; Enter presses it (and does not open the selected message).
        press(&mut h, egui::Modifiers::NONE, Key::Tab);
        press(&mut h, egui::Modifiers::NONE, Key::Enter);
        let now = crate::ribbon::selected_tab(h.state(), Surface::Main);
        assert_ne!(now, home, "Enter selected the focused tab");
        assert!(h.state().ui.read_windows.is_empty(), "Enter was not Open");
        // Space presses too, and is not Space-bar reading.
        press(&mut h, egui::Modifiers::NONE, Key::Tab);
        press(&mut h, egui::Modifiers::NONE, Key::Space);
        assert_ne!(crate::ribbon::selected_tab(h.state(), Surface::Main), now, "Space selected the next tab");
        assert_eq!(h.state().s.selection.focus, focus, "Space did not move to another message");
    }

    /// What a key press or a click changed, coarsely: selection, module, dialogs, windows, backstage,
    /// ribbon tabs and the view state.
    fn outcome(app: &App) -> String {
        format!(
            "{:?}|{:?}|{:?}|{}|{}|{}|{:?}|{}",
            app.s.selection,
            app.s.view.module,
            app.ui.dialogs.iter().map(|d| d.0.clone()).collect::<Vec<_>>(),
            app.s.composes.len(),
            app.ui.read_windows.len(),
            app.ui.backstage.is_some(),
            app.ui.ribbon_tab,
            serde_json::to_string(&app.s.view).unwrap_or_default(),
        )
    }

    fn module_shell(nav: &str) -> egui_kittest::Harness<'static, App> {
        let mut h = shell(1400.0, 900.0);
        h.state_mut().run(nav, json!({}));
        h.run_steps(4);
        h
    }

    #[test]
    fn space_on_any_focused_button_does_what_a_click_does() {
        // Every enabled button-like widget of the main window (a sample of each module's), focused the way
        // Tab or F6 focuses it: Space presses it exactly as a click does. Text fields, the message list and the
        // message body keep their own keys (list navigation, Space-bar reading) and are left out.
        for (nav, most) in [("nav.mail", 40usize), ("nav.calendar", 20), ("nav.notes", 15)] {
            let h = module_shell(nav);
            let widgets: Vec<(egui::Id, egui::Rect)> = h.ctx.viewport(|v| {
                v.prev_pass
                    .widgets
                    .get_layer(egui::LayerId::background())
                    .filter(|w| {
                        w.enabled
                            && w.sense.is_focusable()
                            && w.sense.senses_click()
                            && !w.sense.senses_drag()
                    })
                    .filter(|w| w.interact_rect.is_positive() && w.id != crate::list::focus_id())
                    .map(|w| (w.id, w.interact_rect))
                    .collect()
            });
            drop(h);
            assert!(widgets.len() > 10, "{nav}: {} buttons", widgets.len());
            let step = widgets.len().div_ceil(most).max(1);
            for (id, rect) in widgets.into_iter().step_by(step) {
                let mut a = module_shell(nav);
                a.ctx.memory_mut(|m| m.request_focus(id));
                a.run_steps(2);
                if a.ctx.memory(|m| m.focused()) != Some(id) {
                    continue;
                }
                assert!(widget_focused(&a.ctx), "{nav}: {id:?} counts as a focused button");
                let before = outcome(a.state());
                press(&mut a, egui::Modifiers::NONE, Key::Space);
                let space = outcome(a.state());
                let mut b = module_shell(nav);
                let p = rect.center();
                b.event(egui::Event::PointerMoved(p));
                b.run_steps(1);
                for pressed in [true, false] {
                    b.event(egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                    b.run_steps(1);
                }
                b.run_steps(2);
                let click = outcome(b.state());
                assert_eq!(
                    space != before,
                    click != before,
                    "{nav}: {id:?} at {rect:?}\n{before}\n{space}\n{click}"
                );
            }
        }
    }
}
