//! The compose body editor: a custom egui widget over the compose [`Doc`].
//!
//! Display goes through [`crate::body::show_doc_with`] (layout + paint, reading area): the editor keeps a
//! [`crate::body::BodyView`] per compose, feeds it the compose selection (so the body painter draws the
//! highlight) and uses the layout it painted with for hit testing, caret placement
//! ([`mailcraft_layout::Layout::caret_rect`]) and caret movement over the line boxes. The editor's own
//! interaction sits on top of the painted body, so the body's read-only selection handling never runs here.
//!
//! Every change goes through `App::run`: typing → `compose.insertText`, Backspace / Delete →
//! `compose.deleteText`, caret moves and selections → `compose.select`, clipboard → `format.copy` /
//! `format.cut` / `format.paste`, Ctrl+B/I/U → `format.bold` / `format.italic` / `format.underline`, Ctrl+L/E/R/J →
//! `format.align`, Ctrl+Z / Ctrl+Y → `compose.editUndo` / `compose.editRedo`, Tab / Shift+Tab in a list →
//! `format.indent` / `format.outdent`. Keys the editor handles are removed from the frame's input so the
//! compose-surface shortcut handler (`crate::keys::handle`) does not run them a second time.

use egui::{
    CursorIcon, Event, EventFilter, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, Ui, UiBuilder, WidgetInfo,
    WidgetType, pos2, vec2,
};
use serde_json::{Value, json};

use mailcraft_engine::compose::{ComposeState, DocPos};
use mailcraft_layout::{Layout, LayoutOptions, LineBox, TextPos, layout_doc};
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
/// Accessible name of the body editor.
pub const BODY_LABEL: &str = "Message body";

/// The egui id of the body editor of compose `cid` (focus target; also used by `keys` to tell whether the body
/// has keyboard focus).
pub fn editor_id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-body", cid))
}

/// Whether the body editor of compose `cid` has keyboard focus.
pub fn body_focused(ctx: &egui::Context, cid: u64) -> bool {
    ctx.memory(|m| m.has_focus(editor_id(cid)))
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
fn leaf_chars(doc: &Doc, path: &[usize]) -> Vec<char> {
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

fn with_compose(cid: u64, mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.insert("compose".into(), json!(cid));
        v
    } else {
        json!({"compose": cid})
    }
}

/// Runs `compose.select` when the selection differs.
fn select(app: &mut App, cid: u64, anchor: &DocPos, focus: &DocPos) {
    let same = app
        .s
        .composes
        .get(&cid)
        .is_some_and(|c| c.selection.anchor == *anchor && c.selection.focus == *focus);
    if !same {
        app.run(
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
fn apply(ui: &Ui, app: &mut App, cid: u64, geo: &Geo<'_>, px: &mut Option<f32>, acts: Vec<Act>) -> bool {
    let ctx = ui.ctx().clone();
    let mut any = false;
    // Layout of the current doc once an action changed it (moves after typing in the same frame).
    let mut fresh: Option<Layout> = None;
    let mut edited = false;
    for act in acts {
        let Some(c) = app.s.composes.get(&cid) else {
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
                app.run("compose.insertText", json!({"compose": cid, "text": t}));
            }
            Act::Paste(t) => {
                app.run("format.paste", json!({"compose": cid, "text": t}));
            }
            Act::Copy | Act::Cut => {
                if c.selection.is_collapsed() {
                    continue;
                }
                let id = if act == Act::Copy { "format.copy" } else { "format.cut" };
                if let Some(r) = app.run(id, json!({"compose": cid}))
                    && let Some(t) = r.get("text").and_then(Value::as_str)
                {
                    ctx.copy_text(t.to_owned());
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
                    select(app, cid, &from, &to);
                    app.run("compose.deleteText", json!({"compose": cid, "direction": "selection"}));
                } else {
                    let dir = if back { "backward" } else { "forward" };
                    app.run("compose.deleteText", json!({"compose": cid, "direction": dir}));
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
                select(app, cid, &anchor, &target);
                continue;
            }
            Act::SelectAll => {
                app.run("compose.select", json!({"compose": cid, "all": true}));
                continue;
            }
            Act::Undo => {
                app.run("compose.editUndo", json!({"compose": cid}));
            }
            Act::Redo => {
                app.run("compose.editRedo", json!({"compose": cid}));
            }
            Act::Tab { back } => {
                let listed = in_list(&c.body, &c.selection.focus.path);
                if back {
                    if listed {
                        app.run("format.outdent", json!({"compose": cid}));
                    }
                } else if listed {
                    app.run("format.indent", json!({"compose": cid}));
                } else {
                    app.run("compose.insertText", json!({"compose": cid, "text": "\t"}));
                }
            }
            Act::Cmd(id, params) => {
                if !id.is_empty() {
                    app.run(id, with_compose(cid, params));
                }
            }
        }
        edited = true;
        fresh = None;
    }
    any
}

// =======================================================================================================
// The widget
// =======================================================================================================

/// The body editor of compose `cid`, filling the available width and at least `min_height` tall. Place it in
/// a vertical scroll area; it scrolls the caret into view itself.
pub fn body_editor(ui: &mut Ui, app: &mut App, cid: u64, min_height: f32) -> Option<egui::Response> {
    let ctx = ui.ctx().clone();
    let id = editor_id(cid);
    let c = app.s.composes.get(&cid)?;
    let dark = app.theme.dark && !c.light_canvas;
    let zoom = f32::from(c.zoom_pct().clamp(10, 500)) / 100.0;
    let avail = ui.available_width();
    let width = if avail.is_finite() { avail.max(MIN_TEXT_W + 2.0 * PAD) } else { 800.0 };
    let text_w = (width - 2.0 * PAD).max(MIN_TEXT_W);
    let mut mem: Mem = ctx.data_mut(|d| d.remove_temp::<Mem>(id)).unwrap_or_default();
    if mem.doc.as_ref() != Some(&c.body) {
        mem.doc_key = crate::body::doc_hash(&c.body);
        mem.doc = Some(c.body.clone());
    }
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

    // The editor's interaction, on top of the painted body.
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, BODY_LABEL));
    if resp.hovered() {
        ctx.set_cursor_icon(CursorIcon::Text);
    }
    super::claim_initial_focus(ui, cid, &resp);
    let body_start = c.body_start();
    let Mem { view, doc, preferred_x, drag_anchor, reveal, .. } = &mut mem;
    let layout = view.layout.as_ref();
    let hit = |p: Pos2| -> DocPos {
        let local = p - origin;
        layout
            .and_then(|l| l.hit_pos(mailcraft_geom::Point { x: local.x, y: local.y }))
            .map(to_doc)
            .unwrap_or_else(|| body_start.clone())
    };
    let mut changed = false;
    let (pressed, down, shift) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers.shift));
    if let Some(p) = resp.interact_pointer_pos() {
        let at = hit(p);
        if resp.double_clicked() || resp.triple_clicked() {
            let (a, b) = if resp.triple_clicked() {
                let end = layout.map(|l| block_end(l, &at.path)).unwrap_or(0);
                (0, end)
            } else {
                let chars = doc.as_ref().map(|d| leaf_chars(d, &at.path)).unwrap_or_default();
                word_at(&chars, at.offset)
            };
            *drag_anchor = None;
            select(app, cid, &DocPos::new(at.path.clone(), a), &DocPos::new(at.path.clone(), b));
            changed = true;
        } else if pressed {
            resp.request_focus();
            let anchor = if shift { sel.anchor.clone() } else { at.clone() };
            select(app, cid, &anchor, &at);
            *drag_anchor = Some(anchor);
            *preferred_x = None;
            changed = true;
        } else if down && let Some(anchor) = drag_anchor.clone() {
            select(app, cid, &anchor, &at);
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

    let focused = resp.has_focus();
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
        if apply(ui, app, cid, &geo, preferred_x, acts) {
            changed = true;
            *reveal = true;
        }
    }

    // Caret, from the state this frame was painted with.
    if focused_before && sel.is_collapsed() {
        let caret = layout
            .and_then(|l| l.caret_rect(&to_text(&sel.focus)))
            .map(|r| Rect::from_min_size(pos2(r.x, r.y), vec2(1.0, r.h)))
            .unwrap_or_else(|| Rect::from_min_size(Pos2::ZERO, vec2(1.0, line_h)))
            .translate(origin.to_vec2());
        let col = if dark { app.theme.text } else { egui::Color32::BLACK };
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
    if changed {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(id, mem));
    Some(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
