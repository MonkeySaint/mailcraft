//! The compose body editor: a custom egui widget over the compose [`Doc`].
//!
//! Display goes through [`crate::body::show_doc_with`] (layout + paint, reading area): the editor keeps a
//! [`crate::body::BodyView`] per compose, feeds it the compose selection (so the body painter draws the
//! highlight) and uses the layout it painted with for hit testing, caret placement
//! ([`mailcraft_layout::Layout::caret_rect`]) and caret movement over the line boxes. The editor's own
//! interaction sits on top of the painted body, so the body's read-only selection handling never runs here.
//!
//! Every change goes through the host's command runner (`App::run` for compose windows): typing →
//! `compose.insertText`, Backspace / Delete → `compose.deleteText`, caret moves and selections →
//! `compose.select`, clipboard → `format.copy` / `format.cut` / `format.paste`, Ctrl+B/I/U → `format.bold` /
//! `format.italic` / `format.underline`, Ctrl+L/E/R/J → `format.align`, Ctrl+Z / Ctrl+Y → `compose.editUndo` /
//! `compose.editRedo`, Tab / Shift+Tab → next / previous table cell (`compose.tableCell`) or, in a list,
//! `format.indent` / `format.outdent`; F7 opens the spelling dialog. Keys the editor handles are removed from
//! the frame's input so the compose-surface shortcut handler (`crate::keys::handle`) does not run them a
//! second time.
//!
//! On top of the painted body the editor draws:
//! - red wavy underlines under misspelled words and blue double underlines under grammar issues, from the
//!   compose's cached spelling issues (re-checked when the body changed and the user paused typing);
//! - inline pictures (`cid:` parts of the compose and `data:` URIs) through egui's image loaders, over the
//!   body painter's placeholder (the placeholder stays when no loader decodes the format);
//! - the caret.
//!
//! Right-click opens the context menu: spelling suggestions, Ignore All and Add to Dictionary on a flagged
//! word; Open / Copy / Remove Link on a link; table rows and columns in a table; Cut / Copy / Paste. Ctrl+click
//! follows a link (http, https, mailto; `#bookmark` jumps inside the message).
//!
//! The editor works through [`EditorHost`], so the same widget edits a signature in Signatures and Stationery
//! (a private session) as well as a compose window body.

use egui::{
    Color32, CursorIcon, Event, EventFilter, Id, Key, Modifiers, Pos2, Rect, Sense, Shape, Stroke, Ui,
    UiBuilder, WidgetInfo, WidgetType, pos2, vec2,
};
use serde_json::{Value, json};

use mailcraft_engine::compose::{ComposeState, DocPos, SpellIssue};
use mailcraft_layout::{DisplayItem, Layout, LayoutOptions, LineBox, TextPos, layout_doc};
use mailcraft_model::{Block, Doc, Inline};

use crate::App;
use crate::body::{BodyOptions, BodyView};

/// Inner padding around the body text.
pub const PAD: f32 = 16.0;
/// Narrowest text width laid out.
const MIN_TEXT_W: f32 = 24.0;
/// Caret height when the body has no laid-out line yet (px at 100 %).
const EMPTY_LINE_H: f32 = 20.0;
/// Deepest block path walked when reading leaf text.
const MAX_DEPTH: usize = 64;
/// Most chars of a leaf read for word movement.
const MAX_LEAF_CHARS: usize = 1 << 20;
/// Most spelling issues underlined in one frame.
const MAX_SQUIGGLES: usize = 2_000;
/// Most suggestions offered in the context menu.
const MENU_SUGGESTIONS: usize = 5;
/// Accessible name of the body editor.
pub const BODY_LABEL: &str = "Message body";
/// Context-menu label when the speller has nothing to offer.
pub const NO_SUGGESTIONS: &str = "(No spelling suggestions)";

/// The egui id of the body editor of compose `cid` (focus target; also used by `keys` to tell whether the body
/// has keyboard focus).
pub fn editor_id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-body", cid))
}

/// Whether the body editor of compose `cid` has keyboard focus.
pub fn body_focused(ctx: &egui::Context, cid: u64) -> bool {
    ctx.memory(|m| m.has_focus(editor_id(cid)))
}

// =======================================================================================================
// Host
// =======================================================================================================

/// What the body editor edits through: the app session (compose windows, inline responses) or a private
/// session (the signature editor in Signatures and Stationery).
pub(crate) trait EditorHost {
    /// The compose being edited.
    fn compose(&self, cid: u64) -> Option<&ComposeState>;
    /// Runs a command (errors are reported by the host; `None` = failed).
    fn run(&mut self, id: &str, params: Value) -> Option<Value>;
    /// Colours.
    fn theme(&self) -> &crate::theme::Theme;
    /// Spell checking as you type (squiggles, the spelling context menu, F7).
    fn proofing(&self) -> bool {
        false
    }
    /// Re-checks the spelling of compose `cid` when its body changed since the last check.
    fn refresh_spelling(&mut self, _cid: u64) {}
    /// Spelling suggestions for `word`.
    fn suggest(&self, _word: &str) -> Vec<String> {
        Vec::new()
    }
    /// F7 in the body.
    fn open_spelling(&mut self, _ctx: &egui::Context, _cid: u64) {}
    /// How a paste from the system clipboard is merged (`keepSource`, `merge`, `textOnly`): Options › Mail ›
    /// Editor Options › "Pasting from other programs".
    fn paste_mode(&self) -> &'static str {
        "keepSource"
    }
    /// Options › Mail › "Show Paste Options button when content is pasted into a message".
    fn paste_options(&self) -> bool {
        true
    }
}

impl EditorHost for App {
    fn compose(&self, cid: u64) -> Option<&ComposeState> {
        self.s.composes.get(&cid)
    }
    fn run(&mut self, id: &str, params: Value) -> Option<Value> {
        App::run(self, id, params)
    }
    fn theme(&self) -> &crate::theme::Theme {
        &self.theme
    }
    fn proofing(&self) -> bool {
        true
    }
    fn refresh_spelling(&mut self, cid: u64) {
        if !self.s.composes.get(&cid).is_some_and(ComposeState::spelling_stale) {
            return;
        }
        let sp = mailcraft_engine::cmd::review::speller(&self.s);
        if let Some(c) = self.s.composes.get_mut(&cid) {
            mailcraft_engine::cmd::review::spell_check(&sp, c, false, 1);
        }
    }
    fn suggest(&self, word: &str) -> Vec<String> {
        mailcraft_engine::cmd::review::speller(&self.s).suggest(word, MENU_SUGGESTIONS)
    }
    fn open_spelling(&mut self, ctx: &egui::Context, cid: u64) {
        super::spelling::open(ctx, self, cid);
    }
    fn paste_mode(&self) -> &'static str {
        let chosen = self.s.settings.text("mail.editor.pasteFromOtherPrograms");
        PASTE_MODES.iter().map(|(id, _)| *id).find(|id| *id == chosen).unwrap_or("keepSource")
    }
    fn paste_options(&self) -> bool {
        self.s.settings.bool("mail.other.pasteOptions")
    }
}

/// Per-editor state kept in egui temp memory (taken out for the frame, put back at the end).
#[derive(Clone, Default)]
struct Mem {
    /// Body painter cache (layout of the painted doc).
    view: BodyView,
    /// The doc `doc_key` was computed for.
    doc: Option<Doc>,
    doc_key: u64,
    /// Column kept across Up / Down moves.
    preferred_x: Option<f32>,
    /// Selection anchor while a mouse drag is in progress.
    drag_anchor: Option<DocPos>,
    /// Scroll the caret into view on the next paint.
    reveal: bool,
    /// Where the context menu was opened, and the menu's spelling suggestions.
    menu: Option<MenuAt>,
    /// Image URIs already handed to egui's loaders.
    images: Vec<String>,
    /// List markers patched into the cached layout (see [`patch_markers`]).
    markers: Vec<(Vec<usize>, usize, String, String)>,
}

/// A paste the Paste Options button can redo in another mode.
#[derive(Clone, Debug, Default)]
struct LastPaste {
    html: Option<String>,
    text: String,
    mode: String,
    /// The compose's edit counter right after the paste.
    rev: u64,
}

/// Paste modes offered by the Paste Options button: (mode, label).
pub const PASTE_MODES: [(&str, &str); 3] =
    [("keepSource", "Keep Source Formatting"), ("merge", "Merge Formatting"), ("textOnly", "Keep Text Only")];

fn paste_mode_id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-paste-mode", cid))
}

/// Pastes the system clipboard into compose `cid` with `mode` (ribbon Paste › Keep Source Formatting /
/// Merge Formatting / Keep Text Only): asks the platform for a paste event, which the body editor takes with
/// this mode instead of the default one.
pub fn request_paste(ctx: &egui::Context, cid: u64, mode: &'static str) {
    ctx.data_mut(|d| d.insert_temp(paste_mode_id(cid), mode));
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
}

/// Replaces the default list markers the layout drew (`markers`: list path, item, marker, default marker)
/// with the formatted ones, keeping each marker's right edge where it was. Returns whether anything
/// changed.
fn patch_markers(
    layout: &mut Layout,
    markers: &[(Vec<usize>, usize, String, String)],
    db: &mailcraft_fonts::FontDb,
) -> bool {
    let mut changed = false;
    for (list, item, text, default) in markers {
        let mut item_path = list.clone();
        item_path.push(*item);
        let Some(line) = layout.index.lines.iter().find(|l| l.block_path.starts_with(&item_path)) else {
            continue;
        };
        let (baseline, left) = (line.baseline, line.rect.x);
        for it in &mut layout.display.items {
            if let DisplayItem::Text { origin, text: t, font, size_px, .. } = it
                && (origin.y - baseline).abs() < 1.0
                && origin.x < left
                && t == default
            {
                let old_w = db.measure(*font, t, *size_px);
                let new_w = db.measure(*font, text, *size_px);
                let right = origin.x + old_w;
                origin.x = (right - new_w).max(0.0);
                *t = text.clone();
                changed = true;
                break;
            }
        }
    }
    changed
}

/// Context-menu target.
#[derive(Clone, Debug, Default)]
struct MenuAt {
    issue: Option<SpellIssue>,
    suggestions: Vec<String>,
    link: Option<String>,
    in_table: bool,
}

/// A layout of `doc` with the options the body painter uses (for moves right after an edit in the same frame).
fn fresh_layout(ctx: &egui::Context, doc: &Doc, width: f32, zoom: f32) -> Layout {
    let db = crate::body::font_db(ctx);
    layout_doc(doc, &db, &LayoutOptions { width, zoom, ..LayoutOptions::default() })
}

// =======================================================================================================
// Positions
// =======================================================================================================

fn to_text(p: &DocPos) -> TextPos {
    TextPos { block_path: p.path.clone(), offset: p.offset }
}

fn to_doc(p: TextPos) -> DocPos {
    DocPos::new(p.block_path, p.offset)
}

/// `{"path": [..], "offset": n}`.
fn pos_json(p: &DocPos) -> Value {
    json!({"path": p.path, "offset": p.offset})
}

/// Distinct leaf paths that have lines, in document order.
fn block_paths(layout: &Layout) -> Vec<Vec<usize>> {
    let mut v: Vec<Vec<usize>> = layout.index.lines.iter().map(|l| l.block_path.clone()).collect();
    v.sort();
    v.dedup();
    v
}

/// Caret stop offsets of a block, ascending.
fn block_stops(layout: &Layout, path: &[usize]) -> Vec<usize> {
    let mut v: Vec<usize> = layout
        .index
        .lines
        .iter()
        .filter(|l| l.block_path == path)
        .flat_map(|l| l.stops.iter().map(|s| s.offset))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn block_end(layout: &Layout, path: &[usize]) -> usize {
    block_stops(layout, path).last().copied().unwrap_or(0)
}

/// The line a position sits on (same rule as `Layout::caret_rect`).
fn line_of<'a>(layout: &'a Layout, pos: &DocPos) -> Option<&'a LineBox> {
    let mut last = None;
    for line in layout.index.lines.iter().filter(|l| l.block_path == pos.path) {
        if pos.offset >= line.start && pos.offset < line.end {
            return Some(line);
        }
        last = Some(line);
    }
    last
}

/// Whether `line` is the last line of its block.
fn is_last_line(layout: &Layout, line: &LineBox) -> bool {
    !layout.index.lines.iter().any(|l| l.block_path == line.block_path && l.start > line.start)
}

/// Nearest caret stop on `line` to `x`.
fn pos_on_line(line: &LineBox, x: f32) -> DocPos {
    let mut best: Option<(f32, usize)> = None;
    for s in &line.stops {
        let d = (s.x - x).abs();
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, s.offset));
        }
    }
    DocPos::new(line.block_path.clone(), best.map(|(_, o)| o).unwrap_or(line.start))
}

/// x of char offset `off` on `line`.
fn x_at(line: &LineBox, off: usize) -> f32 {
    line.stops
        .iter()
        .rfind(|s| s.offset <= off)
        .or_else(|| line.stops.first())
        .map(|s| s.x)
        .unwrap_or(line.rect.x)
}

// =======================================================================================================
// Leaf text (for word movement and double-click)
// =======================================================================================================

fn flatten(inlines: &[Inline], out: &mut String, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for i in inlines {
        if out.len() > MAX_LEAF_CHARS {
            return;
        }
        match i {
            Inline::Text { text, .. } => out.push_str(text),
            Inline::Link { children, .. } => flatten(children, out, depth + 1),
            Inline::LineBreak => out.push('\n'),
            Inline::Image { .. } => out.push('\u{FFFC}'),
        }
    }
}

/// The block at `path` (lists `[item, block]`, quotes / containers `[child]`, tables `[row, cell, block]`).
fn block_at<'a>(blocks: &'a [Block], path: &[usize]) -> Option<&'a Block> {
    let (&first, rest) = path.split_first()?;
    let b = blocks.get(first)?;
    if rest.is_empty() {
        return Some(b);
    }
    match b {
        Block::List { items, .. } => {
            let (&item, rest) = rest.split_first()?;
            block_at(items.get(item)?, rest)
        }
        Block::Quote { blocks } | Block::Container { blocks, .. } => block_at(blocks, rest),
        Block::Table { rows, .. } => {
            let (&row, rest) = rest.split_first()?;
            let (&cell, rest) = rest.split_first()?;
            block_at(&rows.get(row)?.get(cell)?.blocks, rest)
        }
        _ => None,
    }
}

/// Flattened chars of the leaf at `path`.
pub(crate) fn leaf_chars(doc: &Doc, path: &[usize]) -> Vec<char> {
    if path.len() > MAX_DEPTH {
        return Vec::new();
    }
    let mut s = String::new();
    match block_at(&doc.blocks, path) {
        Some(Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. }) => {
            flatten(inlines, &mut s, 0)
        }
        Some(Block::Pre { text }) => s.push_str(text),
        _ => {}
    }
    s.chars().take(MAX_LEAF_CHARS).collect()
}

/// Whether the leaf at `path` sits inside a list.
fn in_list(doc: &Doc, path: &[usize]) -> bool {
    (1..path.len())
        .any(|n| matches!(path.get(..n).and_then(|p| block_at(&doc.blocks, p)), Some(Block::List { .. })))
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\''
}

/// Start of the word before `off` (Ctrl+Left).
fn word_left(chars: &[char], off: usize) -> usize {
    let mut i = off.min(chars.len());
    while i > 0 && chars.get(i - 1).is_some_and(|c| c.is_whitespace()) {
        i -= 1;
    }
    if i > 0 && chars.get(i - 1).is_some_and(|c| is_word(*c)) {
        while i > 0 && chars.get(i - 1).is_some_and(|c| is_word(*c)) {
            i -= 1;
        }
    } else {
        i = i.saturating_sub(1);
    }
    i
}

/// Start of the next word after `off` (Ctrl+Right).
fn word_right(chars: &[char], off: usize) -> usize {
    let n = chars.len();
    let mut i = off.min(n);
    if chars.get(i).is_some_and(|c| is_word(*c)) {
        while i < n && chars.get(i).is_some_and(|c| is_word(*c)) {
            i += 1;
        }
    } else if i < n && !chars.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    while i < n && chars.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

/// Word (or run of spaces / one punctuation char) around `off`: `(start, end)`.
fn word_at(chars: &[char], off: usize) -> (usize, usize) {
    let n = chars.len();
    let off = off.min(n);
    let probe = if off < n { off } else { off.saturating_sub(1) };
    let Some(&c) = chars.get(probe) else {
        return (off, off);
    };
    let class = |x: char| {
        if is_word(x) {
            0
        } else if x.is_whitespace() {
            1
        } else {
            2
        }
    };
    let k = class(c);
    if k == 2 {
        return (probe, probe + 1);
    }
    let mut a = probe;
    while a > 0 && chars.get(a - 1).is_some_and(|x| class(*x) == k) {
        a -= 1;
    }
    let mut b = probe + 1;
    while b < n && chars.get(b).is_some_and(|x| class(*x) == k) {
        b += 1;
    }
    // A word takes its trailing spaces, like word processors do.
    if k == 0 {
        while b < n && chars.get(b).is_some_and(|x| *x == ' ') {
            b += 1;
        }
    }
    (a, b)
}

// =======================================================================================================
// Motions
// =======================================================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
    PageUp,
    PageDown,
}

/// Where `motion` takes the caret from `from`. `x` is the preferred column for vertical moves (updated).
fn motion_target(
    layout: &Layout,
    doc: &Doc,
    c: &ComposeState,
    from: &DocPos,
    motion: Motion,
    x: &mut Option<f32>,
    page: f32,
) -> DocPos {
    let paths = block_paths(layout);
    if paths.is_empty() {
        return match motion {
            Motion::Right
            | Motion::WordRight
            | Motion::DocEnd
            | Motion::LineEnd
            | Motion::Down
            | Motion::PageDown => c.body_end(),
            _ => c.body_start(),
        };
    }
    let k = paths.iter().position(|p| *p == from.path);
    let first = paths.first().cloned().unwrap_or_default();
    let last = paths.last().cloned().unwrap_or_default();
    let doc_end = || DocPos::new(last.clone(), block_end(layout, &last));
    let prev_block_end = |k: Option<usize>| {
        k.and_then(|k| k.checked_sub(1))
            .and_then(|j| paths.get(j))
            .map(|p| DocPos::new(p.clone(), block_end(layout, p)))
    };
    let next_block_start =
        |k: Option<usize>| k.and_then(|k| paths.get(k + 1)).map(|p| DocPos::new(p.clone(), 0));
    match motion {
        Motion::DocStart => DocPos::new(first.clone(), 0),
        Motion::DocEnd => doc_end(),
        Motion::Left => {
            let stops = block_stops(layout, &from.path);
            match stops.iter().rev().find(|&&s| s < from.offset) {
                Some(&s) => DocPos::new(from.path.clone(), s),
                None => prev_block_end(k).unwrap_or_else(|| DocPos::new(from.path.clone(), 0)),
            }
        }
        Motion::Right => {
            let stops = block_stops(layout, &from.path);
            match stops.iter().find(|&&s| s > from.offset) {
                Some(&s) => DocPos::new(from.path.clone(), s),
                None => next_block_start(k).unwrap_or_else(|| from.clone()),
            }
        }
        Motion::WordLeft => {
            if from.offset == 0 {
                return prev_block_end(k).unwrap_or_else(|| from.clone());
            }
            let chars = leaf_chars(doc, &from.path);
            DocPos::new(from.path.clone(), word_left(&chars, from.offset))
        }
        Motion::WordRight => {
            let chars = leaf_chars(doc, &from.path);
            if from.offset >= chars.len() {
                return next_block_start(k).unwrap_or_else(|| from.clone());
            }
            DocPos::new(from.path.clone(), word_right(&chars, from.offset))
        }
        Motion::LineStart => match line_of(layout, from) {
            Some(line) => DocPos::new(line.block_path.clone(), line.start),
            None => DocPos::new(from.path.clone(), 0),
        },
        Motion::LineEnd => match line_of(layout, from) {
            Some(line) if is_last_line(layout, line) => {
                DocPos::new(line.block_path.clone(), block_end(layout, &line.block_path))
            }
            Some(line) => {
                let off = line.stops.iter().map(|s| s.offset).max().unwrap_or(line.start);
                DocPos::new(line.block_path.clone(), off)
            }
            None => from.clone(),
        },
        Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
            let Some(cur) = line_of(layout, from) else {
                return from.clone();
            };
            let cx = *x.get_or_insert_with(|| x_at(cur, from.offset));
            let up = matches!(motion, Motion::Up | Motion::PageUp);
            if matches!(motion, Motion::PageUp | Motion::PageDown) {
                let ty = if up { cur.rect.y - page } else { cur.rect.y + cur.rect.h + page };
                let target = layout
                    .index
                    .lines
                    .iter()
                    .min_by(|a, b| {
                        let da = (a.rect.y + a.rect.h / 2.0 - ty).abs();
                        let db = (b.rect.y + b.rect.h / 2.0 - ty).abs();
                        da.total_cmp(&db)
                    })
                    .unwrap_or(cur);
                return pos_on_line(target, cx);
            }
            let cur_mid = cur.rect.y + cur.rect.h / 2.0;
            let candidates = layout.index.lines.iter().filter(|l| {
                let mid = l.rect.y + l.rect.h / 2.0;
                if up {
                    mid < cur.rect.y.min(cur_mid - 0.5)
                } else {
                    mid > (cur.rect.y + cur.rect.h).max(cur_mid + 0.5)
                }
            });
            // Closest line vertically; among lines on the same row (table cells), the closest horizontally.
            let target = candidates.min_by(|a, b| {
                let dy = |l: &LineBox| (l.rect.y + l.rect.h / 2.0 - cur_mid).abs();
                let dx = |l: &LineBox| {
                    if cx < l.rect.x {
                        l.rect.x - cx
                    } else if cx > l.rect.x + l.rect.w {
                        cx - l.rect.x - l.rect.w
                    } else {
                        0.0
                    }
                };
                (dy(a).round(), dx(a))
                    .partial_cmp(&(dy(b).round(), dx(b)))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            match target {
                Some(l) => pos_on_line(l, cx),
                None if up => DocPos::new(cur.block_path.clone(), cur.start),
                None => {
                    if is_last_line(layout, cur) {
                        DocPos::new(cur.block_path.clone(), block_end(layout, &cur.block_path))
                    } else {
                        from.clone()
                    }
                }
            }
        }
    }
}

// =======================================================================================================
// Input
// =======================================================================================================

#[derive(Clone, Debug, PartialEq)]
enum Act {
    Text(String),
    Paste(String),
    Copy,
    Cut,
    Delete { back: bool, word: bool },
    Move(Motion, bool),
    SelectAll,
    Undo,
    Redo,
    Tab { back: bool },
    Spelling,
    Cmd(&'static str, Value),
}

/// Maps a key press to an editor action (`None` = not ours; the event stays for other handlers).
fn key_act(key: Key, m: Modifiers) -> Option<Act> {
    let cmd = m.command;
    let word = m.command || m.alt;
    let mv = |mo| Some(Act::Move(mo, m.shift));
    match key {
        Key::ArrowLeft if m.mac_cmd => mv(Motion::LineStart),
        Key::ArrowRight if m.mac_cmd => mv(Motion::LineEnd),
        Key::ArrowUp if m.mac_cmd => mv(Motion::DocStart),
        Key::ArrowDown if m.mac_cmd => mv(Motion::DocEnd),
        Key::ArrowLeft if word => mv(Motion::WordLeft),
        Key::ArrowRight if word => mv(Motion::WordRight),
        Key::ArrowLeft => mv(Motion::Left),
        Key::ArrowRight => mv(Motion::Right),
        Key::ArrowUp => mv(Motion::Up),
        Key::ArrowDown => mv(Motion::Down),
        Key::Home if cmd => mv(Motion::DocStart),
        Key::End if cmd => mv(Motion::DocEnd),
        Key::Home => mv(Motion::LineStart),
        Key::End => mv(Motion::LineEnd),
        Key::PageUp => mv(Motion::PageUp),
        Key::PageDown => mv(Motion::PageDown),
        Key::Backspace => Some(Act::Delete { back: true, word }),
        Key::Delete if !m.shift => Some(Act::Delete { back: false, word }),
        Key::Enter if !cmd && !m.alt => Some(Act::Text("\n".into())),
        Key::Tab if !cmd && !m.alt => Some(Act::Tab { back: m.shift }),
        Key::F7 if !cmd && !m.alt && !m.shift => Some(Act::Spelling),
        Key::A if cmd && !m.shift && !m.alt => Some(Act::SelectAll),
        Key::Z if cmd && m.shift => Some(Act::Redo),
        Key::Z if cmd && !m.alt => Some(Act::Undo),
        Key::Y if cmd && !m.shift && !m.alt => Some(Act::Redo),
        Key::B if cmd && !m.shift && !m.alt => Some(Act::Cmd("format.bold", json!({}))),
        Key::I if cmd && !m.shift && !m.alt => Some(Act::Cmd("format.italic", json!({}))),
        Key::U if cmd && !m.shift && !m.alt => Some(Act::Cmd("format.underline", json!({}))),
        Key::L if cmd && !m.shift && !m.alt => Some(Act::Cmd("format.align", json!({"kind": "left"}))),
        Key::E if cmd && !m.shift && !m.alt => Some(Act::Cmd("format.align", json!({"kind": "center"}))),
        Key::R if cmd && !m.shift && !m.alt => Some(Act::Cmd("format.align", json!({"kind": "right"}))),
        Key::J if cmd && !m.shift && !m.alt => Some(Act::Cmd("format.align", json!({"kind": "justify"}))),
        // Clipboard keys arrive as Copy / Cut / Paste events; swallow the key so it is not run twice.
        Key::C | Key::X | Key::V if cmd && !m.shift && !m.alt => Some(Act::Cmd("", Value::Null)),
        _ => None,
    }
}

/// Takes the editor's events out of this frame's input (in order), leaving everything else.
fn take_actions(ui: &Ui) -> Vec<Act> {
    ui.input_mut(|i| {
        let mut acts = Vec::new();
        i.events.retain(|e| {
            let act = match e {
                Event::Text(t) if !t.is_empty() => Some(Act::Text(t.clone())),
                Event::Ime(egui::ImeEvent::Commit(t)) if !t.is_empty() => Some(Act::Text(t.clone())),
                Event::Paste(t) => Some(Act::Paste(t.clone())),
                Event::Copy => Some(Act::Copy),
                Event::Cut => Some(Act::Cut),
                Event::Key { key, pressed: true, modifiers, .. } => key_act(*key, *modifiers),
                // Releases of keys we handle are harmless to leave in place.
                _ => None,
            };
            match act {
                Some(a) => {
                    acts.push(a);
                    false
                }
                None => true,
            }
        });
        // Merge consecutive typing into one insert (one undo step per burst, one engine call).
        let mut out: Vec<Act> = Vec::with_capacity(acts.len());
        for a in acts {
            if let (Some(Act::Text(prev)), Act::Text(t)) = (out.last_mut(), &a) {
                prev.push_str(t);
                continue;
            }
            out.push(a);
        }
        out
    })
}

fn last_paste_id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-last-paste", cid))
}

fn with_compose(cid: u64, mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.insert("compose".into(), json!(cid));
        v
    } else {
        json!({"compose": cid})
    }
}

/// Runs `compose.select` when the selection differs.
fn select(host: &mut dyn EditorHost, cid: u64, anchor: &DocPos, focus: &DocPos) {
    let same =
        host.compose(cid).is_some_and(|c| c.selection.anchor == *anchor && c.selection.focus == *focus);
    if !same {
        host.run(
            "compose.select",
            json!({"compose": cid, "anchor": pos_json(anchor), "focus": pos_json(focus)}),
        );
    }
}

/// Layout and geometry the frame's actions work with.
struct Geo<'a> {
    /// Layout painted this frame (of the doc as it was before the actions).
    painted: Option<&'a Layout>,
    text_w: f32,
    zoom: f32,
    /// Page height for Page Up / Page Down.
    page: f32,
}

/// Applies the frame's actions. Returns whether anything ran.
fn apply(
    ui: &Ui,
    host: &mut dyn EditorHost,
    cid: u64,
    geo: &Geo<'_>,
    px: &mut Option<f32>,
    acts: Vec<Act>,
) -> bool {
    let ctx = ui.ctx().clone();
    let mut any = false;
    // Layout of the current doc once an action changed it (moves after typing in the same frame).
    let mut fresh: Option<Layout> = None;
    let mut edited = false;
    for act in acts {
        let Some(c) = host.compose(cid) else {
            break;
        };
        if !matches!(act, Act::Move(Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown, _)) {
            *px = None;
        }
        any = true;
        let needs_layout = matches!(act, Act::Move(..) | Act::Delete { word: true, .. });
        if needs_layout && edited && fresh.is_none() {
            fresh = Some(fresh_layout(&ctx, &c.body, geo.text_w, geo.zoom));
        }
        let layout = fresh.as_ref().or(geo.painted);
        match act {
            Act::Text(t) => {
                host.run("compose.insertText", json!({"compose": cid, "text": t}));
            }
            Act::Paste(t) => {
                let pending: Option<&'static str> = ctx.data_mut(|d| d.remove_temp(paste_mode_id(cid)));
                let mode = pending.unwrap_or_else(|| host.paste_mode());
                let html = super::clipboard::html_for_paste(&t);
                let mut p = json!({"compose": cid, "text": t, "mode": mode});
                if let (Some(h), Some(o)) = (&html, p.as_object_mut()) {
                    o.insert("html".into(), json!(h));
                }
                if host.run("format.paste", p).is_some() && html.is_some() && host.paste_options() {
                    let rev = host.compose(cid).map_or(0, |c| c.edit_rev);
                    ctx.data_mut(|d| {
                        d.insert_temp(
                            last_paste_id(cid),
                            LastPaste { html, text: t.clone(), mode: mode.to_owned(), rev },
                        );
                    });
                }
            }
            Act::Copy | Act::Cut => {
                if c.selection.is_collapsed() {
                    continue;
                }
                let id = if act == Act::Copy { "format.copy" } else { "format.cut" };
                if let Some(r) = host.run(id, json!({"compose": cid}))
                    && let Some(t) = r.get("text").and_then(Value::as_str)
                {
                    // HTML (with the text as its alternative) where the platform takes it, else text.
                    let html = r.get("html").and_then(Value::as_str).unwrap_or_default();
                    if !super::clipboard::copy_html(html, t) {
                        ctx.copy_text(t.to_owned());
                    }
                }
            }
            Act::Delete { back, word } => {
                if word && c.selection.is_collapsed() {
                    let Some(layout) = layout else {
                        continue;
                    };
                    let from = c.selection.focus.clone();
                    let motion = if back { Motion::WordLeft } else { Motion::WordRight };
                    let to = motion_target(layout, &c.body, c, &from, motion, &mut None, geo.page);
                    if to.path != from.path {
                        // At a block boundary Ctrl+Backspace / Ctrl+Delete act like Backspace / Delete
                        // (leave a list item, stay inside table cells, drop an empty paragraph by a table).
                        let dir = if back { "backward" } else { "forward" };
                        host.run("compose.deleteText", json!({"compose": cid, "direction": dir}));
                    } else {
                        select(host, cid, &from, &to);
                        host.run("compose.deleteText", json!({"compose": cid, "direction": "selection"}));
                    }
                } else {
                    let dir = if back { "backward" } else { "forward" };
                    host.run("compose.deleteText", json!({"compose": cid, "direction": dir}));
                }
            }
            Act::Move(motion, extend) => {
                let Some(layout) = layout else {
                    continue;
                };
                let sel = c.selection.clone();
                // Left / Right on a selection collapse it to its start / end.
                let target =
                    if !extend && !sel.is_collapsed() && matches!(motion, Motion::Left | Motion::Right) {
                        if motion == Motion::Left { sel.start().clone() } else { sel.end().clone() }
                    } else {
                        motion_target(layout, &c.body, c, &sel.focus, motion, px, geo.page)
                    };
                let anchor = if extend { sel.anchor.clone() } else { target.clone() };
                select(host, cid, &anchor, &target);
                continue;
            }
            Act::SelectAll => {
                host.run("compose.select", json!({"compose": cid, "all": true}));
                continue;
            }
            Act::Undo => {
                host.run("compose.editUndo", json!({"compose": cid}));
            }
            Act::Redo => {
                host.run("compose.editRedo", json!({"compose": cid}));
            }
            Act::Tab { back } => {
                // In a table Tab moves between cells (and adds a row after the last cell).
                if c.cell_at_caret().is_some() {
                    let dir = if back { "previous" } else { "next" };
                    host.run("compose.tableCell", json!({"compose": cid, "direction": dir}));
                } else {
                    let listed = in_list(&c.body, &c.selection.focus.path);
                    if back {
                        if listed {
                            host.run("format.outdent", json!({"compose": cid}));
                        }
                    } else if listed {
                        host.run("format.indent", json!({"compose": cid}));
                    } else {
                        host.run("compose.insertText", json!({"compose": cid, "text": "\t"}));
                    }
                }
            }
            Act::Spelling => {
                host.open_spelling(&ctx, cid);
                continue;
            }
            Act::Cmd(id, params) => {
                if !id.is_empty() {
                    host.run(id, with_compose(cid, params));
                }
            }
        }
        edited = true;
        fresh = None;
    }
    any
}

// =======================================================================================================
// Links, pictures, squiggles
// =======================================================================================================

/// Follows a link: web and mail links open outside (`mailto:` in the system's handler), `#name` jumps to a
/// bookmark of compose `cid`. Other schemes (`javascript:`, `file:`…) are ignored. Returns whether it acted.
pub(crate) fn follow_link(ctx: &egui::Context, host: &mut dyn EditorHost, cid: u64, href: &str) -> bool {
    let h = href.trim();
    if let Some(name) = h.strip_prefix('#') {
        let pos = host.compose(cid).and_then(|c| {
            c.bookmarks.iter().find(|b| b.name.eq_ignore_ascii_case(name)).map(|b| b.pos.clone())
        });
        return match pos {
            Some(p) => {
                select(host, cid, &p, &p);
                true
            }
            None => false,
        };
    }
    if is_followable(h) {
        ctx.open_url(egui::OpenUrl::new_tab(h));
        return true;
    }
    false
}

/// Whether a link target may be opened outside the app.
pub(crate) fn is_followable(href: &str) -> bool {
    let lower = href.trim().to_ascii_lowercase();
    ["http://", "https://", "mailto:"].iter().any(|s| lower.starts_with(s) && lower.len() > s.len())
}

/// egui image URI and bytes of a body picture: `cid:` parts of the compose, `data:…;base64,` URIs.
fn image_source(c: &ComposeState, cid: u64, src: &str) -> Option<(String, Vec<u8>)> {
    let s = src.trim();
    if let Some(content_id) = s.strip_prefix("cid:") {
        let img = c.inline_images.iter().find(|i| i.content_id.eq_ignore_ascii_case(content_id))?;
        if img.data.is_empty() {
            return None;
        }
        let ext = img.filename.rsplit('.').next().unwrap_or("bin").to_ascii_lowercase();
        return Some((format!("bytes://mailcraft/compose-{cid}/{content_id}.{ext}"), img.data.clone()));
    }
    let rest = s.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    if !meta.to_ascii_lowercase().ends_with(";base64") || payload.len() > 32 * 1024 * 1024 {
        return None;
    }
    let data = mailcraft_mime::transfer::decode_base64(payload.as_bytes());
    if data.is_empty() {
        return None;
    }
    let ext = meta.split(';').next().and_then(|m| m.rsplit('/').next()).unwrap_or("bin").to_ascii_lowercase();
    let key = crate::body::doc_hash(&Doc::from_plain_text(&payload.chars().take(4096).collect::<String>()));
    Some((format!("bytes://mailcraft/data-{key:016x}-{}.{ext}", payload.len()), data))
}

/// Paints the compose's pictures over the body painter's placeholders where an egui image loader decodes
/// them (SVG always; PNG / JPEG / GIF when the app installs those loaders).
fn paint_images(ui: &Ui, layout: &Layout, c: &ComposeState, cid: u64, origin: Pos2, seen: &mut Vec<String>) {
    let ctx = ui.ctx();
    let clip = ui.clip_rect();
    let painter = ui.painter().with_clip_rect(clip);
    for item in &layout.display.items {
        let DisplayItem::Image { rect, src, .. } = item else {
            continue;
        };
        let r = Rect::from_min_size(pos2(origin.x + rect.x, origin.y + rect.y), vec2(rect.w, rect.h));
        if !r.intersects(clip) || !r.is_finite() || r.width() < 1.0 || r.height() < 1.0 {
            continue;
        }
        let Some((uri, data)) = image_source(c, cid, src) else {
            continue;
        };
        if !seen.contains(&uri) {
            ctx.include_bytes(uri.clone(), data);
            if seen.len() < 512 {
                seen.push(uri.clone());
            }
        }
        match ctx.try_load_texture(&uri, egui::TextureOptions::LINEAR, egui::SizeHint::default()) {
            Ok(egui::load::TexturePoll::Ready { texture }) => {
                painter.image(
                    texture.id,
                    r,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            Ok(egui::load::TexturePoll::Pending { .. }) => ctx.request_repaint(),
            Err(_) => {}
        }
    }
}

/// Spelling squiggle colour.
const SPELLING_INK: Color32 = Color32::from_rgb(0xd8, 0x2c, 0x2c);
/// Grammar underline colour.
const GRAMMAR_INK: Color32 = Color32::from_rgb(0x2b, 0x5c, 0xd6);

/// x of char offset `off` on `line`, or the line's right end when `off` is past its last stop.
fn x_end(line: &LineBox, off: usize) -> f32 {
    match line.stops.iter().find(|s| s.offset == off) {
        Some(s) => s.x,
        None => line.stops.iter().map(|s| s.x).fold(line.rect.x, f32::max),
    }
}

/// Screen spans `(x0, x1, baseline_y)` of the issues, per line, in layout coordinates.
fn issue_spans(layout: &Layout, issues: &[SpellIssue]) -> Vec<(f32, f32, f32, bool)> {
    let mut out = Vec::new();
    for issue in issues.iter().take(MAX_SQUIGGLES) {
        if issue.start.path != issue.end.path {
            continue;
        }
        let spelling = issue.kind == "spelling";
        for line in layout.index.lines.iter().filter(|l| l.block_path == issue.start.path) {
            // The last line of a block also holds the block-end position.
            let line_end = if is_last_line(layout, line) { usize::MAX } else { line.end };
            let s = issue.start.offset.max(line.start);
            let e = issue.end.offset.min(line_end);
            if e <= s {
                continue;
            }
            let x0 = x_at(line, s);
            let x1 = x_end(line, e);
            if x1 > x0 + 0.5 {
                out.push((x0, x1, line.baseline, spelling));
            }
        }
    }
    out
}

/// Red wavy underlines (spelling) and blue double underlines (grammar).
fn paint_squiggles(ui: &Ui, layout: &Layout, issues: &[SpellIssue], origin: Pos2, zoom: f32) {
    let clip = ui.clip_rect();
    let painter = ui.painter().with_clip_rect(clip);
    let amp = (1.2 * zoom).clamp(0.8, 3.0);
    let step = (2.0 * zoom).clamp(1.5, 6.0);
    for (x0, x1, base, spelling) in issue_spans(layout, issues) {
        let y = origin.y + base + (2.5 * zoom).max(2.0);
        let (x0, x1) = (origin.x + x0, origin.x + x1);
        if y < clip.top() - 4.0 || y > clip.bottom() + 4.0 {
            continue;
        }
        if spelling {
            let mut pts = Vec::new();
            let mut x = x0;
            let mut up = true;
            while x <= x1 && pts.len() < 4096 {
                pts.push(pos2(x, if up { y - amp } else { y + amp }));
                up = !up;
                x += step;
            }
            if pts.len() >= 2 {
                painter.add(Shape::line(pts, Stroke::new(1.0, SPELLING_INK)));
            }
        } else {
            for dy in [0.0, 2.0] {
                painter.line_segment([pos2(x0, y + dy), pos2(x1, y + dy)], Stroke::new(1.0, GRAMMAR_INK));
            }
        }
    }
}

// =======================================================================================================
// Context menu
// =======================================================================================================

/// What the context menu picked.
#[derive(Clone, Debug, PartialEq)]
enum Pick {
    Correct(String, DocPos),
    IgnoreAll(DocPos),
    AddToDictionary(DocPos),
    Cut,
    Copy,
    Paste,
    OpenLink(String),
    CopyLink(String),
    RemoveLink,
    Table(&'static str),
}

/// Context-menu target at `pos`: the spelling issue (with suggestions), the link and whether it is in a table.
fn menu_target(host: &dyn EditorHost, cid: u64, pos: &DocPos, link: Option<String>) -> MenuAt {
    let Some(c) = host.compose(cid) else {
        return MenuAt::default();
    };
    let issue = if host.proofing() { c.spell_issue_at(pos).cloned() } else { None };
    let mut suggestions = issue.as_ref().map(|i| i.suggestions.clone()).unwrap_or_default();
    if let Some(i) = &issue
        && suggestions.is_empty()
        && i.kind == "spelling"
    {
        suggestions = host.suggest(&i.word);
    }
    suggestions.truncate(MENU_SUGGESTIONS);
    MenuAt { issue, suggestions, link, in_table: c.cell_at_caret().is_some() }
}

fn menu_ui(ui: &mut Ui, at: &MenuAt, has_selection: bool) -> Option<Pick> {
    let mut pick = None;
    ui.set_min_width(200.0);
    if let Some(issue) = &at.issue {
        if at.suggestions.is_empty() {
            ui.add_enabled(false, egui::Button::new(NO_SUGGESTIONS));
        }
        for s in &at.suggestions {
            let label = if s.is_empty() { "(delete)".to_owned() } else { s.clone() };
            if ui.button(egui::RichText::new(label).strong()).clicked() {
                pick = Some(Pick::Correct(s.clone(), issue.start.clone()));
            }
        }
        ui.separator();
        if ui.button("Ignore All").clicked() {
            pick = Some(Pick::IgnoreAll(issue.start.clone()));
        }
        if issue.kind == "spelling" && ui.button("Add to Dictionary").clicked() {
            pick = Some(Pick::AddToDictionary(issue.start.clone()));
        }
        ui.separator();
    }
    if ui.add_enabled(has_selection, egui::Button::new("Cut")).clicked() {
        pick = Some(Pick::Cut);
    }
    if ui.add_enabled(has_selection, egui::Button::new("Copy")).clicked() {
        pick = Some(Pick::Copy);
    }
    if ui.button("Paste").clicked() {
        pick = Some(Pick::Paste);
    }
    if let Some(href) = &at.link {
        ui.separator();
        if ui.button("Open Link").clicked() {
            pick = Some(Pick::OpenLink(href.clone()));
        }
        if ui.button("Copy Link").clicked() {
            pick = Some(Pick::CopyLink(href.clone()));
        }
        if ui.button("Remove Link").clicked() {
            pick = Some(Pick::RemoveLink);
        }
    }
    if at.in_table {
        ui.separator();
        ui.menu_button("Insert", |ui| {
            for (label, action) in [
                ("Insert Rows Above", "insertRowAbove"),
                ("Insert Rows Below", "insertRowBelow"),
                ("Insert Columns to the Left", "insertColumnLeft"),
                ("Insert Columns to the Right", "insertColumnRight"),
            ] {
                if ui.button(label).clicked() {
                    pick = Some(Pick::Table(action));
                }
            }
        });
        ui.menu_button("Delete", |ui| {
            for (label, action) in [
                ("Delete Rows", "deleteRow"),
                ("Delete Columns", "deleteColumn"),
                ("Delete Table", "deleteTable"),
            ] {
                if ui.button(label).clicked() {
                    pick = Some(Pick::Table(action));
                }
            }
        });
    }
    if pick.is_some() {
        ui.close();
    }
    pick
}

fn apply_pick(ctx: &egui::Context, host: &mut dyn EditorHost, cid: u64, pick: Pick) {
    match pick {
        Pick::Correct(r, at) => {
            host.run("review.correct", json!({"compose": cid, "replacement": r, "at": pos_json(&at)}));
        }
        Pick::IgnoreAll(at) => {
            host.run("review.ignore", json!({"compose": cid, "all": true, "at": pos_json(&at)}));
        }
        Pick::AddToDictionary(at) => {
            host.run("review.addToDictionary", json!({"compose": cid, "at": pos_json(&at)}));
        }
        Pick::Cut | Pick::Copy => {
            let id = if pick == Pick::Copy { "format.copy" } else { "format.cut" };
            if let Some(r) = host.run(id, json!({"compose": cid}))
                && let Some(t) = r.get("text").and_then(Value::as_str)
            {
                let html = r.get("html").and_then(Value::as_str).unwrap_or_default();
                if !super::clipboard::copy_html(html, t) {
                    ctx.copy_text(t.to_owned());
                }
            }
        }
        Pick::Paste => {
            // The platform answers with a Paste event, which the focused editor takes next frame.
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        }
        Pick::OpenLink(h) => {
            follow_link(ctx, host, cid, &h);
        }
        Pick::CopyLink(h) => ctx.copy_text(h),
        Pick::RemoveLink => {
            host.run("insert.link", json!({"compose": cid, "url": ""}));
        }
        Pick::Table(action) => {
            host.run("compose.tableEdit", json!({"compose": cid, "action": action}));
        }
    }
    ctx.memory_mut(|m| m.request_focus(editor_id(cid)));
}

// =======================================================================================================
// The widget
// =======================================================================================================

/// The body editor of compose `cid`, filling the available width and at least `min_height` tall. Place it in
/// a vertical scroll area; it scrolls the caret into view itself.
pub fn body_editor(ui: &mut Ui, app: &mut App, cid: u64, min_height: f32) -> Option<egui::Response> {
    body_editor_in(ui, app, cid, min_height)
}

/// [`body_editor`] over any [`EditorHost`].
pub(crate) fn body_editor_in(
    ui: &mut Ui,
    host: &mut dyn EditorHost,
    cid: u64,
    min_height: f32,
) -> Option<egui::Response> {
    let ctx = ui.ctx().clone();
    let id = editor_id(cid);
    let c = host.compose(cid)?;
    let theme_dark = host.theme().dark;
    let theme_text = host.theme().text;
    let dark = theme_dark && !c.light_canvas;
    let zoom = f32::from(c.zoom_pct().clamp(10, 500)) / 100.0;
    let avail = ui.available_width();
    let width = if avail.is_finite() { avail.max(MIN_TEXT_W + 2.0 * PAD) } else { 800.0 };
    let text_w = (width - 2.0 * PAD).max(MIN_TEXT_W);
    let mut mem: Mem = ctx.data_mut(|d| d.remove_temp::<Mem>(id)).unwrap_or_default();
    if mem.doc.as_ref() != Some(&c.body) {
        mem.doc_key = crate::body::doc_hash(&c.body);
        mem.doc = Some(c.body.clone());
    }
    let painted_rev = c.edit_rev;
    let line_h = EMPTY_LINE_H * zoom;
    let min_height = if min_height.is_finite() { min_height } else { 0.0 };
    let est_h = mem.view.layout.as_ref().map(|l| l.height).filter(|h| h.is_finite()).unwrap_or(line_h);
    let height = (est_h + 2.0 * PAD).max(min_height).max(line_h + 2.0 * PAD);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let origin = rect.min + vec2(PAD, PAD);
    let focused_before = ctx.memory(|m| m.has_focus(id));

    // Background and body (the body painter draws the selection handed to it).
    let bg = crate::body::map_color(c.body.background.unwrap_or(mailcraft_geom::Color::WHITE), dark);
    ui.painter().rect_filled(rect, 0.0, bg);
    let sel = c.selection.clone();
    mem.view.doc_key = mem.doc_key;
    mem.view.selection = Some((to_text(&sel.anchor), to_text(&sel.focus)));
    // Formatted list markers: a new set of markers needs a fresh layout to patch.
    let markers = if c.list_formats.is_empty() { Vec::new() } else { c.list_markers() };
    if markers != mem.markers {
        mem.view.key = 0;
        mem.markers = markers;
    }
    {
        let text_rect = Rect::from_min_size(origin, vec2(text_w, est_h.max(line_h)));
        let mut child =
            ui.new_child(UiBuilder::new().max_rect(text_rect).id_salt(("mailcraft-compose-doc", cid)));
        let opts = BodyOptions {
            dark,
            zoom,
            allow_remote: false,
            paint_background: false,
            doc_key: Some(mem.doc_key),
        };
        let _link = crate::body::show_doc_with(&mut child, &mut mem.view, &c.body, &opts);
    }
    if !mem.markers.is_empty()
        && let Some(layout) = mem.view.layout.as_mut()
        && patch_markers(layout, &mem.markers, &crate::body::font_db(&ctx))
    {
        // Painted with the default markers this frame; the patched layout shows from the next one.
        ctx.request_repaint();
    }
    if let Some(layout) = mem.view.layout.as_ref() {
        paint_images(ui, layout, c, cid, origin, &mut mem.images);
    }

    // The editor's interaction, on top of the painted body.
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, BODY_LABEL));
    let body_start = c.body_start();
    let Mem { view, doc, preferred_x, drag_anchor, reveal, menu, .. } = &mut mem;
    let layout = view.layout.as_ref();
    let local = |p: Pos2| mailcraft_geom::Point { x: p.x - origin.x, y: p.y - origin.y };
    let hit = |p: Pos2| -> DocPos {
        layout.and_then(|l| l.hit_pos(local(p))).map(to_doc).unwrap_or_else(|| body_start.clone())
    };
    let link_under = |p: Pos2| -> Option<String> { layout.and_then(|l| l.hit(local(p))).map(str::to_owned) };
    let (pressed, down, mods) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers));
    let hover_link = resp.hover_pos().and_then(link_under);
    if resp.hovered() {
        let icon =
            if hover_link.is_some() && mods.command { CursorIcon::PointingHand } else { CursorIcon::Text };
        ctx.set_cursor_icon(icon);
    }
    super::claim_initial_focus(ui, cid, &resp);
    let mut changed = false;
    if let Some(p) = resp.interact_pointer_pos() {
        let at = hit(p);
        let link = if mods.command { link_under(p) } else { None };
        if let (true, Some(href)) = (pressed, link) {
            // Ctrl+click follows the link instead of placing the caret.
            resp.request_focus();
            *drag_anchor = None;
            follow_link(&ctx, host, cid, &href);
            changed = true;
        } else if resp.double_clicked() || resp.triple_clicked() {
            let (a, b) = if resp.triple_clicked() {
                let end = layout.map(|l| block_end(l, &at.path)).unwrap_or(0);
                (0, end)
            } else {
                let chars = doc.as_ref().map(|d| leaf_chars(d, &at.path)).unwrap_or_default();
                word_at(&chars, at.offset)
            };
            *drag_anchor = None;
            select(host, cid, &DocPos::new(at.path.clone(), a), &DocPos::new(at.path.clone(), b));
            changed = true;
        } else if pressed {
            resp.request_focus();
            let anchor = if mods.shift { sel.anchor.clone() } else { at.clone() };
            select(host, cid, &anchor, &at);
            *drag_anchor = Some(anchor);
            *preferred_x = None;
            changed = true;
        } else if down && let Some(anchor) = drag_anchor.clone() {
            select(host, cid, &anchor, &at);
            changed = true;
            let clip = ui.clip_rect();
            if p.y > clip.bottom() - 8.0 || p.y < clip.top() + 8.0 {
                ui.scroll_to_rect(Rect::from_center_size(p, vec2(16.0, 16.0)), None);
            }
        }
    }
    if !down {
        *drag_anchor = None;
    }
    if resp.clicked() && !resp.has_focus() {
        resp.request_focus();
    }
    if let Some(href) = &hover_link
        && !mods.command
        && resp.hovered()
        && !resp.context_menu_opened()
    {
        resp.clone().on_hover_text_at_pointer(format!("{href}\nCtrl+click to follow the link"));
    }

    // Right-click: the caret moves to the click unless it falls inside the selection; the menu works on that
    // spot (spelling issue, link, table).
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let at = hit(p);
        let inside = !sel.is_collapsed() && *sel.start() <= at && at <= *sel.end();
        if !inside {
            select(host, cid, &at, &at);
        }
        resp.request_focus();
        *menu = Some(menu_target(&*host, cid, &at, link_under(p)));
    }
    let mut pick = None;
    if let Some(at) = menu.clone() {
        let has_selection = host.compose(cid).is_some_and(ComposeState::has_selection);
        resp.context_menu(|ui| {
            pick = menu_ui(ui, &at, has_selection);
        });
        if !resp.secondary_clicked() && !resp.context_menu_opened() {
            *menu = None;
        }
    }
    if let Some(p) = pick {
        *menu = None;
        apply_pick(&ctx, host, cid, p);
        changed = true;
        *reveal = true;
    }

    let focused = resp.has_focus();
    let mut acted = false;
    if focused {
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false },
            );
        });
        let acts = take_actions(ui);
        let page = (ui.clip_rect().height() - 2.0 * line_h).max(line_h);
        let geo = Geo { painted: layout, text_w, zoom, page };
        if apply(ui, host, cid, &geo, preferred_x, acts) {
            changed = true;
            acted = true;
            *reveal = true;
        }
    }

    // Spelling: re-check once typing pauses (a frame without edits), then underline what the check found
    // when it matches the painted text.
    if host.proofing() {
        if !acted {
            host.refresh_spelling(cid);
        }
        if let (Some(layout), Some(c)) = (layout, host.compose(cid))
            && c.edit_rev == painted_rev
            && c.spell_rev == Some(c.edit_rev)
            && !c.spell_issues.is_empty()
        {
            paint_squiggles(ui, layout, &c.spell_issues, origin, zoom);
        }
    }

    // Caret, from the state this frame was painted with.
    if focused_before && sel.is_collapsed() {
        let caret = layout
            .and_then(|l| l.caret_rect(&to_text(&sel.focus)))
            .map(|r| Rect::from_min_size(pos2(r.x, r.y), vec2(1.0, r.h)))
            .unwrap_or_else(|| Rect::from_min_size(Pos2::ZERO, vec2(1.0, line_h)))
            .translate(origin.to_vec2());
        let col = if dark { theme_text } else { Color32::BLACK };
        ui.painter().line_segment([caret.center_top(), caret.center_bottom()], Stroke::new(1.5, col));
        if *reveal {
            ui.scroll_to_rect(caret.expand2(vec2(4.0, 8.0)), None);
            *reveal = false;
        }
        ctx.output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                rect,
                cursor_rect: caret,
                should_interrupt_composition: false,
            });
        });
    }
    paste_options_button(ui, host, cid, rect);
    if changed {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(id, mem));
    Some(resp)
}

/// The Paste Options button after a rich paste (while the paste is the latest edit): redo the paste as
/// Keep Source Formatting, Merge Formatting or Keep Text Only.
fn paste_options_button(ui: &mut Ui, host: &mut dyn EditorHost, cid: u64, rect: Rect) {
    let ctx = ui.ctx().clone();
    let Some(last) = ctx.data(|d| d.get_temp::<LastPaste>(last_paste_id(cid))) else {
        return;
    };
    if host.compose(cid).is_none_or(|c| c.edit_rev != last.rev) {
        ctx.data_mut(|d| d.remove::<LastPaste>(last_paste_id(cid)));
        return;
    }
    let at = Rect::from_min_size(rect.right_bottom() - vec2(132.0, 30.0), vec2(124.0, 24.0));
    let mut pick: Option<&'static str> = None;
    let mut child = ui.new_child(UiBuilder::new().max_rect(at).id_salt(("mailcraft-paste-options", cid)));
    let r = child.menu_button("(Ctrl) Paste Options", |ui| {
        for (mode, label) in PASTE_MODES {
            if ui.radio(last.mode == mode, label).clicked() {
                pick = Some(mode);
                ui.close();
            }
        }
    });
    r.response.on_hover_text("Paste Options");
    if let Some(mode) = pick
        && mode != last.mode
    {
        host.run("compose.editUndo", json!({"compose": cid}));
        let mut p = json!({"compose": cid, "text": last.text, "mode": mode});
        if let (Some(h), Some(o)) = (&last.html, p.as_object_mut()) {
            o.insert("html".into(), json!(h));
        }
        host.run("format.paste", p);
        let rev = host.compose(cid).map_or(0, |c| c.edit_rev);
        ctx.data_mut(|d| d.insert_temp(last_paste_id(cid), LastPaste { mode: mode.to_owned(), rev, ..last }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatted_list_markers_replace_the_default_ones() {
        let mut c = ComposeState::default();
        c.insert_text("one\ntwo\nthree");
        c.select_all();
        c.toggle_list(Some(mailcraft_model::rich::ListKind::Numbered { start: 1 }));
        let fmt = mailcraft_engine::compose::ListFormat::from_key("upperRoman").unwrap();
        assert!(c.set_list_format(vec![fmt], false));
        let db = mailcraft_fonts::FontDb::bundled();
        let mut layout =
            layout_doc(&c.body, &db, &LayoutOptions { width: 400.0, ..LayoutOptions::default() });
        let texts = |l: &Layout| -> Vec<String> {
            l.display
                .items
                .iter()
                .filter_map(|i| match i {
                    DisplayItem::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect()
        };
        assert!(texts(&layout).contains(&"2.".to_owned()));
        let markers = c.list_markers();
        assert!(patch_markers(&mut layout, &markers, &db));
        let t = texts(&layout);
        assert!(
            t.contains(&"II.".to_owned()) && t.contains(&"III.".to_owned()) && !t.contains(&"2.".to_owned()),
            "{t:?}"
        );
        assert!(!patch_markers(&mut layout, &markers, &db), "patching twice changes nothing");
    }

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn word_moves() {
        let c = chars("Hello there, world");
        assert_eq!(word_right(&c, 0), 6);
        assert_eq!(word_right(&c, 6), 11);
        assert_eq!(word_right(&c, 11), 13);
        assert_eq!(word_left(&c, 18), 13);
        assert_eq!(word_left(&c, 13), 11);
        assert_eq!(word_left(&c, 6), 0);
        assert_eq!(word_at(&c, 2), (0, 6));
        assert_eq!(word_at(&c, 11), (11, 12));
        assert_eq!(word_at(&[], 3), (0, 0));
    }

    #[test]
    fn leaf_text_walks_nested_blocks() {
        let doc = Doc {
            blocks: vec![
                Block::Paragraph {
                    style: Default::default(),
                    inlines: vec![
                        Inline::Text { text: "ab".into(), style: Default::default() },
                        Inline::LineBreak,
                    ],
                },
                Block::List {
                    kind: mailcraft_model::rich::ListKind::Bullet,
                    items: vec![vec![Block::Paragraph {
                        style: Default::default(),
                        inlines: vec![Inline::Text { text: "item".into(), style: Default::default() }],
                    }]],
                },
            ],
            ..Doc::default()
        };
        assert_eq!(leaf_chars(&doc, &[0]), chars("ab\n"));
        assert_eq!(leaf_chars(&doc, &[1, 0, 0]), chars("item"));
        assert!(leaf_chars(&doc, &[5]).is_empty());
        assert!(in_list(&doc, &[1, 0, 0]));
        assert!(!in_list(&doc, &[0]));
    }

    #[test]
    fn followable_links_and_hostile_ones() {
        assert!(is_followable("https://example.com/a"));
        assert!(is_followable(" HTTP://example.org "));
        assert!(is_followable("mailto:ann@example.com"));
        assert!(!is_followable("javascript:alert(1)"));
        assert!(!is_followable("file:///etc/passwd"));
        assert!(!is_followable("https://"));
        assert!(!is_followable(""));
    }

    #[test]
    fn squiggles_span_the_flagged_word() {
        let doc = Doc::from_plain_text("Helo world\nfine wrld");
        let db = mailcraft_fonts::FontDb::bundled();
        let layout = layout_doc(&doc, &db, &LayoutOptions { width: 400.0, ..LayoutOptions::default() });
        let issue = |path: usize, a: usize, b: usize| SpellIssue {
            start: DocPos::new(vec![path], a),
            end: DocPos::new(vec![path], b),
            word: String::new(),
            kind: "spelling".into(),
            suggestions: Vec::new(),
        };
        let spans = issue_spans(&layout, &[issue(0, 0, 4), issue(1, 5, 9)]);
        assert_eq!(spans.len(), 2);
        let (x0, x1, base0, spelling) = spans[0];
        assert!(spelling && x0 < 1.0 && x1 > 15.0 && x1 < 60.0, "{spans:?}");
        let (x2, x3, base1, _) = spans[1];
        assert!(base1 > base0 && x2 > 20.0 && x3 > x2, "{spans:?}");
        // Stale / hostile positions draw nothing.
        assert!(issue_spans(&layout, &[issue(7, 0, 4), issue(0, 4, 2)]).is_empty());
        let mut grammar = issue(0, 5, 10);
        grammar.kind = "repeated".into();
        assert!(!issue_spans(&layout, &[grammar])[0].3);
    }

    #[test]
    fn picture_sources_resolve_cid_and_data_uris() {
        let mut c = ComposeState::default();
        c.inline_images.push(mailcraft_engine::compose::InlineImage {
            content_id: "image001@x".into(),
            filename: "dot.svg".into(),
            mime_type: "image/svg+xml".into(),
            data: b"<svg/>".to_vec(),
        });
        let (uri, data) = image_source(&c, 3, "cid:image001@x").unwrap();
        assert!(uri.starts_with("bytes://mailcraft/compose-3/") && uri.ends_with(".svg"), "{uri}");
        assert_eq!(data, b"<svg/>");
        assert!(image_source(&c, 3, "cid:missing").is_none());
        let (uri, data) = image_source(&c, 3, "data:image/png;base64,aGVsbG8=").unwrap();
        assert!(uri.ends_with(".png"));
        assert_eq!(data, b"hello");
        assert!(image_source(&c, 3, "data:text/plain,hello").is_none());
        assert!(image_source(&c, 3, "https://example.com/a.png").is_none());
    }
}
