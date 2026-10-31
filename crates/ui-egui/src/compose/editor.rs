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
//! `format.indent` / `format.outdent`; F7 opens the spelling dialog; Ctrl+Shift+. / Ctrl+Shift+, (Ctrl+> /
//! Ctrl+<) → `format.growFont` / `format.shrinkFont`. Keys the editor handles are removed from
//! the frame's input so the compose-surface shortcut handler (`crate::keys::handle`) does not run them a
//! second time.
//!
//! On top of the painted body the editor draws:
//! - red wavy underlines under misspelled words and blue double underlines under grammar issues, from the
//!   compose's cached spelling issues (re-checked when the body or the Editor Options proofing settings
//!   changed and the user paused typing; "Check spelling as you type" / "Mark grammar errors as you type"
//!   decide which are drawn, and with both off nothing is checked while typing);
//! - Live Preview: while a gallery choice is under the pointer (`request_live_preview`), the body as that
//!   choice would leave it (`compose.livePreview`), instead of the body;
//! - inline pictures (`cid:` parts of the compose and `data:` URIs) through egui's image loaders, over the
//!   body painter's placeholder (the placeholder stays when no loader decodes the format);
//! - the caret;
//! - with Show All (`format.showMarks`): formatting marks (¶ at each paragraph end, · for spaces, → for tabs,
//!   ↵ for line breaks);
//! - with View Gridlines (`format.showGridlines`): dashed cell boundaries of tables that have no borders;
//! - Select Text With Similar Formatting: the extra selected ranges;
//! - pictures hidden in the Selection pane are covered (view only).
//!
//! Select Objects mode (`format.selectObjects`): a click selects the picture under the pointer and text is not
//! selected. Ctrl+V with only a picture on the clipboard pastes the picture (when the platform installed a
//! picture reader, see `compose::set_clipboard_image_reader`).
//!
//! Right-click opens the context menu: spelling suggestions, Ignore All and Add to Dictionary on a flagged
//! word; Open / Copy / Remove Link on a link; table rows and columns in a table; Cut / Copy / Paste. Ctrl+click
//! follows a link (http, https, mailto; `#bookmark` jumps inside the message).
//!
//! Above the body: the Mini Toolbar over a mouse selection (`minibar`) and the @mention list under the caret
//! (`mention`).
//!
//! The editor works through [`EditorHost`], so the same widget edits a signature in Signatures and Stationery
//! (a private session) as well as a compose window body.

use egui::{
    Color32, CursorIcon, Event, EventFilter, Id, Key, Modifiers, Pos2, Rect, Role, Sense, Shape, Stroke, Ui,
    UiBuilder, WidgetInfo, pos2, vec2,
};
use serde_json::{Value, json};

use mailcraft_engine::cmd::review::LiveProofing;
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
    Id::unique(("mailcraft-compose-body", cid))
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
    /// What the editor underlines while you type (Options › Mail › Editor Options: "Check spelling as you
    /// type", "Mark grammar errors as you type").
    fn live_proofing(&self) -> LiveProofing {
        LiveProofing::default()
    }
    /// Re-checks the spelling of compose `cid` when its body changed since the last check.
    fn refresh_spelling(&mut self, _cid: u64) {}
    /// Spelling suggestions for `word` in compose `cid` (its proofing language).
    fn suggest(&self, _cid: u64, _word: &str) -> Vec<String> {
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
    /// Options › Mail › "Suggest names to mention when I use the @ symbol in a message".
    fn mentions(&self) -> bool {
        false
    }
    /// People to mention for what was typed after `@`: (name, address).
    fn mention_suggestions(&self, _query: &str) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Options › General › "Show Mini Toolbar on selection".
    fn mini_toolbar(&self) -> bool {
        false
    }
    /// Live Preview: shows `choice` (a gallery command and its parameters) in compose `cid`'s body, or stops
    /// previewing (`None`).
    fn live_preview(&mut self, _cid: u64, _choice: Option<(String, Value)>) {}
    /// Tells the compose how wide its text area is shown (px at 100 %), for placing and aligning drawings.
    fn set_view_width(&mut self, _cid: u64, _width: f32) {}
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
    fn live_proofing(&self) -> LiveProofing {
        mailcraft_engine::cmd::review::live_proofing(&self.s)
    }
    fn refresh_spelling(&mut self, cid: u64) {
        // Nothing is checked while typing when nothing is marked; F7 checks on demand.
        if !self.live_proofing().any()
            || !self
                .s
                .composes
                .get(&cid)
                .is_some_and(|c| mailcraft_engine::cmd::review::spelling_stale(&self.s, c))
        {
            return;
        }
        // Never waits for an installed dictionary: it is read in the background and the body re-checked.
        let sp = mailcraft_engine::cmd::review::compose_speller_live(&self.s, cid);
        if let Some(c) = self.s.composes.get_mut(&cid) {
            // Stale after an edit, or checked under proofing options that changed since.
            mailcraft_engine::cmd::review::spell_check(&sp, c, false, 1);
        }
    }
    fn suggest(&self, cid: u64, word: &str) -> Vec<String> {
        mailcraft_engine::cmd::review::compose_speller_live(&self.s, cid).suggest(word, MENU_SUGGESTIONS)
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
    fn mentions(&self) -> bool {
        mailcraft_engine::cmd::compose::mentions_enabled(&self.s).is_none()
    }
    fn mention_suggestions(&self, query: &str) -> Vec<(String, String)> {
        mailcraft_engine::queries::address_suggestions(&self.s, query, super::mention::MAX_PEOPLE)
            .into_iter()
            .map(|g| (g.display, g.address))
            .collect()
    }
    fn mini_toolbar(&self) -> bool {
        self.s.settings.bool("general.miniToolbar")
    }
    fn live_preview(&mut self, cid: u64, choice: Option<(String, Value)>) {
        let previewing = self.s.composes.get(&cid).is_some_and(|c| c.preview.is_some());
        // Errors (a choice that does not apply here) only mean there is nothing to preview: no toast.
        match choice {
            Some((command, params)) if mailcraft_engine::cmd::compose::live_preview_on(&self.s) => {
                let p = json!({"compose": cid, "command": command, "params": params});
                let _ = self.s.run("compose.livePreview", &p);
            }
            _ if previewing => {
                let _ = self.s.run("compose.livePreview", &json!({"compose": cid, "clear": true}));
            }
            _ => {}
        }
    }
    fn set_view_width(&mut self, cid: u64, width: f32) {
        if let Some(c) = self.s.composes.get_mut(&cid)
            && width.is_finite()
            && c.view_width != Some(width)
        {
            c.view_width = Some(width);
        }
    }
}

/// Live Preview requests from galleries (see [`request_live_preview`]).
#[derive(Clone, Debug)]
struct PreviewRequest {
    command: String,
    params: Value,
    /// The pass it was made in.
    pass: u64,
}

fn preview_request_id(cid: u64) -> Id {
    Id::unique(("mailcraft-compose-live-preview", cid))
}

/// Live Preview (File › Options › General › "Enable Live Preview"): a gallery calls this every frame the
/// pointer rests on one of its choices (`command` with `params`, e.g. `format.style {"name": "Heading 1"}`);
/// the body editor of compose `cid` then shows the body with that choice applied, without applying it, and
/// stops as soon as no request comes.
pub fn request_live_preview(ctx: &egui::Context, cid: u64, command: &str, params: &Value) {
    if !mailcraft_engine::cmd::compose::PREVIEW_COMMANDS.contains(&command) {
        return;
    }
    let req = PreviewRequest {
        command: command.to_owned(),
        params: params.clone(),
        pass: ctx.cumulative_pass_nr(),
    };
    ctx.data_mut(|d| d.insert_temp(preview_request_id(cid), req));
}

/// The gallery choice to preview this frame (the ribbon and its menus draw before the body, so a choice still
/// under the pointer was requested in this pass; an older request means the pointer moved on).
fn take_preview_request(ctx: &egui::Context, cid: u64) -> Option<(String, Value)> {
    let req: PreviewRequest = ctx.data(|d| d.get_temp(preview_request_id(cid)))?;
    if req.pass != ctx.cumulative_pass_nr() {
        ctx.data_mut(|d| d.remove::<PreviewRequest>(preview_request_id(cid)));
        return None;
    }
    Some((req.command, req.params))
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
    /// A click (or double-click) the armed Format Painter / pen paints once no further click of the same
    /// gesture can follow: (input time it is due, whether it was a plain click that paints the word).
    paint_due: Option<(f64, bool)>,

    /// List markers patched into the cached layout (see [`patch_markers`]).
    markers: Vec<(Vec<usize>, usize, String, String)>,
    /// View Gridlines: cell rectangles of borderless tables, for (doc key, width, zoom).
    grid: Option<(u64, u32, u32, Vec<mailcraft_geom::Rect>)>,
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
    Id::unique(("mailcraft-compose-paste-mode", cid))
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
        // A formatted marker that reads like the default one needs no patch (patching it would report a
        // change, and ask for a repaint, on every frame).
        if text == default {
            continue;
        }
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
    /// A point or segment of the drawing whose points are being edited.
    points: Option<super::drawing::PointMenu>,
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
            Inline::Anchor { .. } => {}
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
    Delete {
        back: bool,
        word: bool,
    },
    Move(Motion, bool),
    SelectAll,
    Undo,
    Redo,
    Tab {
        back: bool,
    },
    Spelling,
    /// Ctrl+V: a paste event carries text; with none, a picture on the clipboard is pasted.
    PasteKey,
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
        // Ctrl+> / Ctrl+< (Ctrl+Shift+. and Ctrl+Shift+, on a US layout): Grow Font / Shrink Font.
        Key::Period if cmd && m.shift && !m.alt => Some(Act::Cmd("format.growFont", json!({}))),
        Key::Comma if cmd && m.shift && !m.alt => Some(Act::Cmd("format.shrinkFont", json!({}))),
        // Clipboard keys arrive as Copy / Cut / Paste events; swallow the key so it is not run twice.
        Key::V if cmd && !m.shift && !m.alt => Some(Act::PasteKey),
        Key::C | Key::X if cmd && !m.shift && !m.alt => Some(Act::Cmd("", Value::Null)),
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
    Id::unique(("mailcraft-compose-last-paste", cid))
}

fn last_copy_id() -> Id {
    Id::unique("mailcraft-compose-last-copy")
}

/// The text the last Copy / Cut in a message body put on the clipboard, and the compose it came from.
#[derive(Clone, Default)]
struct LastCopy {
    cid: u64,
    text: String,
}

/// Where pasted text `t` was copied (for the paste default in Options › Mail › Editor Options ›
/// Advanced): `sameMessage` / `otherMessage` when it is what the last in-app copy put on the clipboard
/// (from compose `cid` / another compose), else `otherProgram`.
pub(crate) fn paste_origin(ctx: &egui::Context, cid: u64, t: &str) -> &'static str {
    let Some(last) = ctx.data(|d| d.get_temp::<LastCopy>(last_copy_id())) else { return "otherProgram" };
    // Platforms may hand line breaks back as CR LF.
    let same = last.text.chars().filter(|c| *c != '\r').eq(t.chars().filter(|c| *c != '\r'));
    match (same, last.cid == cid) {
        (true, true) => "sameMessage",
        (true, false) => "otherMessage",
        _ => "otherProgram",
    }
}

/// Puts what `format.copy` / `format.cut` returned (`text`, `html`) on the system clipboard: HTML with the
/// text as its alternative where the platform takes it, else the text. Remembers the copy for the paste
/// defaults ([`paste_origin`]). Returns whether anything was put there.
pub(crate) fn put_copy_on_clipboard(ctx: &egui::Context, cid: u64, r: &Value) -> bool {
    let Some(t) = r.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()) else {
        return false;
    };
    ctx.data_mut(|d| d.insert_temp(last_copy_id(), LastCopy { cid, text: t.to_owned() }));
    let html = r.get("html").and_then(Value::as_str).unwrap_or_default();
    if html.trim().is_empty() || !super::clipboard::copy_html(html, t) {
        ctx.copy_text(t.to_owned());
    }
    true
}

/// Pastes text `t` read from the system clipboard (with its HTML flavour `html`, when it matches) into
/// compose `cid`: with `mode` (the ribbon's Paste menu), else with the Options default for where the text
/// was copied. Offers the Paste Options button after a rich paste. Returns whether it pasted.
pub(crate) fn paste_clipboard(
    ctx: &egui::Context,
    host: &mut dyn EditorHost,
    cid: u64,
    t: &str,
    html: Option<String>,
    mode: Option<&str>,
) -> bool {
    let mut p = json!({"compose": cid, "text": t});
    if let Some(o) = p.as_object_mut() {
        match mode {
            Some(m) => o.insert("mode".into(), json!(m)),
            None => o.insert("origin".into(), json!(paste_origin(ctx, cid, t))),
        };
        if let Some(h) = &html {
            o.insert("html".into(), json!(h));
        }
    }
    let Some(r) = host.run("format.paste", p) else {
        return false;
    };
    if html.is_some() && host.paste_options() {
        let mode = r
            .get("mode")
            .and_then(Value::as_str)
            .map_or_else(|| mode.map_or_else(|| host.paste_mode().to_owned(), str::to_owned), str::to_owned);
        let rev = host.compose(cid).map_or(0, |c| c.edit_rev);
        ctx.data_mut(|d| {
            d.insert_temp(last_paste_id(cid), LastPaste { html, text: t.to_owned(), mode, rev });
        });
    }
    true
}

fn refocus_id(cid: u64) -> Id {
    Id::unique(("mailcraft-compose-refocus", cid))
}

/// Gives the body editor of compose `cid` keyboard focus again once the click that ran a ribbon command is
/// over (that click took the focus away from the body; the caret and selection stay where they were).
pub fn refocus_body(ctx: &egui::Context, cid: u64) {
    ctx.data_mut(|d| d.insert_temp(refocus_id(cid), true));
}

/// Takes a pending [`refocus_body`] request in a frame without pointer presses or clicks (a click in the
/// same frame would take the focus away again).
fn take_refocus(ctx: &egui::Context, cid: u64) -> bool {
    let busy = ctx.input(|i| i.pointer.any_pressed() || i.pointer.any_click() || i.pointer.any_down());
    !busy && ctx.data_mut(|d| d.remove_temp::<bool>(refocus_id(cid))).unwrap_or(false)
}

fn with_compose(cid: u64, mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.insert("compose".into(), json!(cid));
        v
    } else {
        json!({"compose": cid})
    }
}

/// Runs `compose.select` when the selection differs. An armed Format Painter or highlighter pen is not
/// applied here: the editor applies it once a mouse selection is finished ([`apply_armed_tools`]).
fn select(host: &mut dyn EditorHost, cid: u64, anchor: &DocPos, focus: &DocPos) {
    let same =
        host.compose(cid).is_some_and(|c| c.selection.anchor == *anchor && c.selection.focus == *focus);
    if !same {
        host.run(
            "compose.select",
            json!({"compose": cid, "anchor": pos_json(anchor), "focus": pos_json(focus), "paint": false}),
        );
    }
}

/// Whether compose `cid` has Format Painter or the highlighter pen armed.
fn tools_armed(host: &dyn EditorHost, cid: u64) -> bool {
    host.compose(cid).is_some_and(|c| c.painter.is_some() || c.highlight_pen.is_some())
}

/// Paints a finished mouse selection with the armed Format Painter or highlighter pen. A plain click paints
/// the word under it with Format Painter. Returns whether the command ran.
fn apply_armed_tools(host: &mut dyn EditorHost, cid: u64, plain_click: bool) -> bool {
    let painter = host.compose(cid).is_some_and(|c| c.painter.is_some());
    host.run("format.formatPainter", json!({"compose": cid, "apply": true, "word": plain_click && painter}))
        .is_some()
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
    let text_paste = acts.iter().any(|a| matches!(a, Act::Paste(_)));
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
                // A mode chosen on the ribbon's Paste menu, else the Options default for where the text was
                // copied (the same message, another message, another program), which the engine applies.
                let pending: Option<&'static str> = ctx.data_mut(|d| d.remove_temp(paste_mode_id(cid)));
                let html = super::clipboard::html_for_paste(&t);
                paste_clipboard(&ctx, host, cid, &t, html, pending);
            }
            Act::Copy | Act::Cut => {
                if c.selection.is_collapsed() {
                    continue;
                }
                let id = if act == Act::Copy { "format.copy" } else { "format.cut" };
                if let Some(r) = host.run(id, json!({"compose": cid})) {
                    put_copy_on_clipboard(&ctx, cid, &r);
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
            Act::PasteKey => {
                // Only a picture on the clipboard (no text came with the key): paste the picture.
                let picture = if text_paste { None } else { super::clipboard::clipboard_image() };
                let Some(data) = picture else {
                    continue;
                };
                let b64 = mailcraft_mime::transfer::encode_base64_flat(&data);
                host.run("format.paste", json!({"compose": cid, "image": b64}));
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
        let pos = host.compose(cid).and_then(|c| c.bookmark_pos(name));
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

/// The picture data of a `cid:` source in the body: the compose's inline image with that content id.
fn inline_picture<'a>(c: &'a ComposeState, src: &str) -> Option<&'a [u8]> {
    let content_id = src.trim().strip_prefix("cid:")?;
    c.inline_images
        .iter()
        .find(|i| i.content_id.eq_ignore_ascii_case(content_id))
        .map(|i| i.data.as_slice())
        .filter(|d| !d.is_empty())
}

/// Most pictures handed over per frame (the decoded-picture cache holds [`crate::body::MAX_IMAGE_CACHE`]).
const MAX_PROVIDED: usize = crate::body::MAX_IMAGE_CACHE / 2;

/// Hands the compose's `cid:` pictures that the layout shows inside `clip` to the body painter
/// ([`crate::body::provide_image`], the reading pane's path; `data:` pictures decode in the painter itself).
/// Returns whether one was decoded just now: it draws from the next frame.
fn provide_pictures(
    ctx: &egui::Context,
    layout: &Layout,
    c: &ComposeState,
    origin: Pos2,
    clip: Rect,
) -> bool {
    let mut fresh = false;
    let mut handed = 0usize;
    for item in &layout.display.items {
        let DisplayItem::Image { rect, src, .. } = item else {
            continue;
        };
        let r = Rect::from_min_size(pos2(origin.x + rect.x, origin.y + rect.y), vec2(rect.w, rect.h));
        if !r.is_finite() || !r.intersects(clip) {
            continue;
        }
        let Some(data) = inline_picture(c, src) else {
            continue;
        };
        if handed >= MAX_PROVIDED {
            break;
        }
        handed = handed.saturating_add(1);
        fresh |= crate::body::provide_image_fresh(ctx, src, data);
    }
    fresh
}

/// Covers the pictures hidden in the Selection pane with the page colour (they stay in the message).
fn cover_hidden_pictures(ui: &Ui, layout: &Layout, origin: Pos2, hidden: &[String], page: Color32) {
    if hidden.is_empty() {
        return;
    }
    let clip = ui.clip_rect();
    let painter = ui.painter().with_clip_rect(clip);
    for item in &layout.display.items {
        let DisplayItem::Image { rect, src, .. } = item else {
            continue;
        };
        let r = Rect::from_min_size(pos2(origin.x + rect.x, origin.y + rect.y), vec2(rect.w, rect.h));
        if r.is_finite() && r.intersects(clip) && hidden.contains(src) {
            painter.rect_filled(r.expand(1.0), 0.0, page);
        }
    }
}

// =======================================================================================================
// Formatting marks, gridlines, objects
// =======================================================================================================

/// Formatting-mark colour (Show All).
const MARKS_INK: Color32 = Color32::from_rgb(0x4a, 0x7a, 0xb8);
/// Gridline colour (View Gridlines).
const GRID_INK: Color32 = Color32::from_rgb(0x8c, 0xb4, 0xe0);

/// Formatting marks of the visible lines: (glyph, x, baseline) in layout coordinates.
pub(crate) fn mark_glyphs(layout: &Layout, doc: &Doc, top: f32, bottom: f32) -> Vec<(char, f32, f32)> {
    let mut out = Vec::new();
    let mut cache: Option<(Vec<usize>, Vec<char>)> = None;
    for line in &layout.index.lines {
        if line.rect.y + line.rect.h < top || line.rect.y > bottom {
            continue;
        }
        if cache.as_ref().is_none_or(|(p, _)| *p != line.block_path) {
            cache = Some((line.block_path.clone(), leaf_chars(doc, &line.block_path)));
        }
        let Some((_, chars)) = cache.as_ref() else {
            continue;
        };
        for off in line.start..line.end.min(chars.len()) {
            let glyph = match chars.get(off) {
                Some(' ') => '\u{b7}',
                Some('\t') => '\u{2192}',
                Some('\n') => '\u{21b5}',
                Some('\u{a0}') => '\u{b0}',
                _ => continue,
            };
            let x0 = x_at(line, off);
            let x1 = line.stops.iter().find(|s| s.offset == off.saturating_add(1)).map_or(x0 + 4.0, |s| s.x);
            out.push((glyph, (x0 + x1) / 2.0, line.baseline));
            if out.len() > 20_000 {
                return out;
            }
        }
        if is_last_line(layout, line) {
            let end = line.stops.iter().map(|s| s.x).fold(line.rect.x, f32::max);
            out.push(('\u{b6}', end + 1.0, line.baseline));
        }
    }
    out
}

/// Show All: draws the formatting marks.
fn paint_marks(ui: &Ui, layout: &Layout, doc: &Doc, origin: Pos2, zoom: f32) {
    let clip = ui.clip_rect();
    let painter = ui.painter().with_clip_rect(clip);
    let size = (13.0 * zoom).clamp(6.0, 60.0);
    let font = egui::FontId::proportional(size);
    for (glyph, x, base) in
        mark_glyphs(layout, doc, clip.top() - origin.y - 40.0, clip.bottom() - origin.y + 40.0)
    {
        let (align, x) = match glyph {
            '\u{b6}' | '\u{21b5}' => (egui::Align2::LEFT_BOTTOM, x),
            _ => (egui::Align2::CENTER_BOTTOM, x),
        };
        painter.text(pos2(origin.x + x, origin.y + base + size * 0.2), align, glyph, font.clone(), MARKS_INK);
    }
}

/// Deepest nesting walked when turning table borders on for the gridline layout.
const MAX_GRID_DEPTH: usize = 32;

/// `doc` with every table bordered; whether any table had no border.
fn bordered(doc: &Doc) -> (Doc, bool) {
    fn walk(blocks: &mut [Block], any: &mut bool, depth: usize) {
        if depth > MAX_GRID_DEPTH {
            return;
        }
        for b in blocks {
            match b {
                Block::Table { rows, border, .. } => {
                    if !*border {
                        *border = true;
                        *any = true;
                    }
                    for cell in rows.iter_mut().flatten() {
                        walk(&mut cell.blocks, any, depth + 1);
                    }
                }
                Block::List { items, .. } => {
                    for item in items {
                        walk(item, any, depth + 1);
                    }
                }
                Block::Quote { blocks } | Block::Container { blocks, .. } => walk(blocks, any, depth + 1),
                _ => {}
            }
        }
    }
    let mut d = doc.clone();
    let mut any = false;
    walk(&mut d.blocks, &mut any, 0);
    (d, any)
}

/// View Gridlines: the cell rectangles of tables that have no borders (layout coordinates).
pub(crate) fn gridlines(
    db: &mailcraft_fonts::FontDb,
    doc: &Doc,
    width: f32,
    zoom: f32,
) -> Vec<mailcraft_geom::Rect> {
    let (with_borders, any) = bordered(doc);
    if !any {
        return Vec::new();
    }
    let opts = LayoutOptions { width, zoom, ..LayoutOptions::default() };
    let plain = layout_doc(doc, db, &opts);
    let shown: Vec<mailcraft_geom::Rect> = plain
        .display
        .items
        .iter()
        .filter_map(|i| match i {
            DisplayItem::StrokeRect { rect, .. } => Some(*rect),
            _ => None,
        })
        .collect();
    layout_doc(&with_borders, db, &opts)
        .display
        .items
        .iter()
        .filter_map(|i| match i {
            DisplayItem::StrokeRect { rect, .. } if !shown.contains(rect) => Some(*rect),
            _ => None,
        })
        .take(20_000)
        .collect()
}

/// Dashed gridlines.
fn paint_gridlines(ui: &Ui, rects: &[mailcraft_geom::Rect], origin: Pos2) {
    let clip = ui.clip_rect();
    let painter = ui.painter().with_clip_rect(clip);
    for r in rects {
        let rr = Rect::from_min_size(pos2(origin.x + r.x, origin.y + r.y), vec2(r.w.max(0.0), r.h.max(0.0)));
        if !rr.is_finite() || !rr.expand(2.0).intersects(clip) {
            continue;
        }
        let corners = [rr.left_top(), rr.right_top(), rr.right_bottom(), rr.left_bottom(), rr.left_top()];
        for w in corners.windows(2) {
            if let [a, b] = w {
                painter.extend(Shape::dashed_line(&[*a, *b], Stroke::new(1.0, GRID_INK), 3.0, 3.0));
            }
        }
    }
}

/// The picture under `p` (layout coordinates) as the char range it occupies, from the body's objects
/// (the k-th display of a source is its k-th occurrence in the body).
fn object_at(layout: &Layout, c: &ComposeState, p: mailcraft_geom::Point) -> Option<(DocPos, DocPos)> {
    let mut seen: Vec<&str> = Vec::new();
    for item in &layout.display.items {
        let DisplayItem::Image { rect, src, .. } = item else {
            continue;
        };
        let k = seen.iter().filter(|s| **s == src.as_str()).count();
        seen.push(src);
        if p.x >= rect.x && p.x <= rect.x + rect.w && p.y >= rect.y && p.y <= rect.y + rect.h {
            let objects = mailcraft_engine::cmd::format::body_objects(c);
            let obj = objects.iter().filter(|o| o.src == *src).nth(k)?;
            let end = DocPos::new(obj.at.path.clone(), obj.at.offset.saturating_add(1));
            return Some((obj.at.clone(), end));
        }
    }
    None
}

/// The content id of the chart, SmartArt graphic or 3D model picture under `p`, if any (pictures MailCraft
/// drew and kept the recipe of).
fn editable_picture(layout: &Layout, c: &ComposeState, p: mailcraft_geom::Point) -> Option<String> {
    layout.display.items.iter().rev().find_map(|item| {
        let DisplayItem::Image { rect, src, .. } = item else { return None };
        if !(p.x >= rect.x && p.x <= rect.x + rect.w && p.y >= rect.y && p.y <= rect.y + rect.h) {
            return None;
        }
        let cid = src.strip_prefix("cid:")?;
        c.generated
            .iter()
            .any(|g| {
                g.content_id.eq_ignore_ascii_case(cid)
                    && matches!(g.kind.as_str(), "chart" | "smartArt" | "model3d" | "equation")
            })
            .then(|| cid.to_owned())
    })
}

/// Spelling squiggle colour.
const SPELLING_INK: Color32 = Color32::from_rgb(0xd8, 0x2c, 0x2c);
/// Grammar underline colour.
const GRAMMAR_INK: Color32 = Color32::from_rgb(0x2b, 0x5c, 0xd6);
/// Dotted underline of writing refinements (Editor pane suggestions).
const REFINE_INK: Color32 = Color32::from_rgb(0x8a, 0x3f, 0xb8);

/// x of char offset `off` on `line`, or the line's right end when `off` is past its last stop.
fn x_end(line: &LineBox, off: usize) -> f32 {
    match line.stops.iter().find(|s| s.offset == off) {
        Some(s) => s.x,
        None => line.stops.iter().map(|s| s.x).fold(line.rect.x, f32::max),
    }
}

/// The cached issues the editor underlines under `live` (all of them only when every kind is marked).
fn marked_issues(live: LiveProofing, issues: &[SpellIssue]) -> std::borrow::Cow<'_, [SpellIssue]> {
    if live.all() {
        return std::borrow::Cow::Borrowed(issues);
    }
    std::borrow::Cow::Owned(
        issues.iter().filter(|i| live.marks(&i.kind)).take(MAX_SQUIGGLES).cloned().collect(),
    )
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

/// Red wavy underlines (spelling), blue double underlines (grammar) and purple dotted underlines (writing
/// refinements).
fn paint_squiggles(ui: &Ui, layout: &Layout, issues: &[SpellIssue], origin: Pos2, zoom: f32) {
    let clip = ui.clip_rect();
    let painter = ui.painter().with_clip_rect(clip);
    let amp = (1.2 * zoom).clamp(0.8, 3.0);
    let step = (2.0 * zoom).clamp(1.5, 6.0);
    let (refine, issues): (Vec<SpellIssue>, Vec<SpellIssue>) = issues
        .iter()
        .take(MAX_SQUIGGLES)
        .cloned()
        .partition(|i| mailcraft_engine::cmd::review::is_refinement(&i.kind));
    for (x0, x1, base, _) in issue_spans(layout, &refine) {
        let y = origin.y + base + (2.5 * zoom).max(2.0);
        if y < clip.top() - 4.0 || y > clip.bottom() + 4.0 {
            continue;
        }
        let dot = (1.0 * zoom).clamp(0.8, 2.0);
        let mut x = origin.x + x0;
        let mut n = 0;
        while x <= origin.x + x1 && n < 4096 {
            painter.circle_filled(pos2(x, y + 1.0), dot * 0.6, REFINE_INK);
            x += dot * 3.0;
            n += 1;
        }
    }
    for (x0, x1, base, spelling) in issue_spans(layout, &issues) {
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
    /// An Edit Points change (`drawing.editPoints` params).
    Points(Value),
}

/// Context-menu target at `pos`: the spelling issue (with suggestions), the link and whether it is in a table.
fn menu_target(host: &dyn EditorHost, cid: u64, pos: &DocPos, link: Option<String>) -> MenuAt {
    let Some(c) = host.compose(cid) else {
        return MenuAt::default();
    };
    // Only what is underlined (a check from F7 may have found issues that are not marked as you type).
    let live = host.live_proofing();
    let issue =
        if host.proofing() { c.spell_issue_at(pos).filter(|i| live.marks(&i.kind)).cloned() } else { None };
    let mut suggestions = issue.as_ref().map(|i| i.suggestions.clone()).unwrap_or_default();
    if let Some(i) = &issue
        && suggestions.is_empty()
        && i.kind == "spelling"
    {
        suggestions = host.suggest(cid, &i.word);
    }
    suggestions.truncate(MENU_SUGGESTIONS);
    MenuAt { issue, suggestions, link, in_table: c.cell_at_caret().is_some(), points: None }
}

fn menu_ui(ui: &mut Ui, at: &MenuAt, has_selection: bool) -> Option<Pick> {
    let mut pick = None;
    ui.set_min_width(200.0);
    if let Some(points) = &at.points {
        pick = super::drawing::point_menu_ui(ui, points).map(Pick::Points);
        if pick.is_some() {
            ui.close();
        }
        return pick;
    }
    if let Some(issue) = &at.issue {
        // What the checker says (grammar and refinements; a misspelling needs no words).
        if !issue.message.is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(&issue.message).weak()).wrap());
        }
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
            if let Some(r) = host.run(id, json!({"compose": cid})) {
                put_copy_on_clipboard(ctx, cid, &r);
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
        Pick::Points(mut p) => {
            // Adding, deleting or opening renumbers the points: the picked one is forgotten.
            let renumbers = ["insert", "delete", "path"].iter().any(|k| p.get(k).is_some());
            if let Some(o) = p.as_object_mut() {
                o.insert("compose".into(), json!(cid));
            }
            host.run("drawing.editPoints", p);
            if renumbers {
                super::drawing::forget_point(ctx, cid);
            }
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
    let choice = take_preview_request(&ctx, cid);
    host.live_preview(cid, choice);
    {
        // Drawings are placed and aligned in the width the text is shown in (px at 100 %).
        let zoom = f32::from(host.compose(cid)?.zoom_pct().clamp(10, 500)) / 100.0;
        let avail = ui.available_width();
        let width = if avail.is_finite() { avail.max(MIN_TEXT_W + 2.0 * PAD) } else { 800.0 };
        host.set_view_width(cid, ((width - 2.0 * PAD).max(MIN_TEXT_W) / zoom).round());
    }
    let c = host.compose(cid)?;
    // Live Preview: the body as the gallery choice under the pointer would leave it.
    let shown = c.preview.as_ref().unwrap_or(&c.body);
    let theme_dark = host.theme().dark;
    let theme_text = host.theme().text;
    let dark = theme_dark && !c.light_canvas;
    let zoom = f32::from(c.zoom_pct().clamp(10, 500)) / 100.0;
    let avail = ui.available_width();
    let width = if avail.is_finite() { avail.max(MIN_TEXT_W + 2.0 * PAD) } else { 800.0 };
    let text_w = (width - 2.0 * PAD).max(MIN_TEXT_W);
    let mut mem: Mem = ctx.data_mut(|d| d.remove_temp::<Mem>(id)).unwrap_or_default();
    if mem.doc.as_ref() != Some(shown) {
        mem.doc_key = crate::body::doc_hash(shown);
        mem.doc = Some(shown.clone());
    }
    let painted_rev = c.edit_rev;
    let line_h = EMPTY_LINE_H * zoom;
    let min_height = if min_height.is_finite() { min_height } else { 0.0 };
    let est_h = mem.view.layout.as_ref().map(|l| l.height).filter(|h| h.is_finite()).unwrap_or(line_h);
    let height = (est_h + 2.0 * PAD).max(min_height).max(line_h + 2.0 * PAD);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let origin = rect.min + vec2(PAD, PAD);
    if take_refocus(&ctx, cid) {
        ctx.memory_mut(|m| m.request_focus(id));
    }
    let focused_before = ctx.memory(|m| m.has_focus(id));

    // Background and body (the body painter draws the selection handed to it).
    let bg = crate::body::map_color(c.body.background.unwrap_or(mailcraft_geom::Color::WHITE), dark);
    ui.painter().rect_filled(rect, 0.0, bg);
    // Drawings behind the text.
    let behind_shown = super::drawing::displayed(c, mem.view.layout.as_ref(), zoom);
    super::drawing::paint(ui, &*host, cid, origin, zoom, true, &behind_shown);
    let sel = c.selection.clone();
    mem.view.doc_key = mem.doc_key;
    mem.view.selection = Some((to_text(&sel.anchor), to_text(&sel.focus)));
    // Select Text With Similar Formatting: the other selected ranges.
    mem.view.extra =
        c.extra_ranges.iter().take(10_000).map(|r| (to_text(r.start()), to_text(r.end()))).collect();
    // Find › Reading Highlight › Highlight All.
    mem.view.marks =
        c.find_marks.iter().take(10_000).map(|r| (to_text(r.start()), to_text(r.end()))).collect();
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
        let _link = crate::body::show_doc_with(&mut child, &mut mem.view, shown, &opts);
    }
    if !mem.markers.is_empty()
        && let Some(layout) = mem.view.layout.as_mut()
        && patch_markers(layout, &mem.markers, &crate::body::font_db(&ctx))
    {
        // Painted with the default markers this frame; the patched layout shows from the next one.
        ctx.request_repaint();
    }
    let hidden = super::panes::hidden_objects(&ctx, cid);
    if let Some(layout) = mem.view.layout.as_ref() {
        // The body painter drew the pictures it already has; the ones it lacks are handed over for the
        // next frame.
        if provide_pictures(&ctx, layout, c, origin, ui.clip_rect()) {
            ctx.request_repaint();
        }
        cover_hidden_pictures(ui, layout, origin, &hidden, bg);
    }
    if c.editor.show_gridlines {
        let key = (mem.doc_key, text_w.to_bits(), zoom.to_bits());
        if mem.grid.as_ref().is_none_or(|g| (g.0, g.1, g.2) != key) {
            let rects = gridlines(&crate::body::font_db(&ctx), &c.body, text_w, zoom);
            mem.grid = Some((key.0, key.1, key.2, rects));
        }
        if let Some((.., rects)) = mem.grid.as_ref() {
            paint_gridlines(ui, rects, origin);
        }
    }
    if c.editor.show_marks
        && let Some(layout) = mem.view.layout.as_ref()
    {
        paint_marks(ui, layout, &c.body, origin, zoom);
    }
    // Drawings in front of the text, the selected one's handles and the object being drawn.
    let shown_drawings = super::drawing::displayed(c, mem.view.layout.as_ref(), zoom);
    super::drawing::paint(ui, &*host, cid, origin, zoom, false, &shown_drawings);
    let select_objects = c.editor.select_objects;

    // The editor's interaction, on top of the painted body.
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    resp.widget_info(|| WidgetInfo::labeled(Role::TextInput, true, BODY_LABEL));
    let body_start = c.body_start();
    let Mem { view, doc, preferred_x, drag_anchor, reveal, menu, paint_due, .. } = &mut mem;
    let layout = view.layout.as_ref();
    let local = |p: Pos2| mailcraft_geom::Point { x: p.x - origin.x, y: p.y - origin.y };
    let hit = |p: Pos2| -> DocPos {
        layout.and_then(|l| l.hit_pos(local(p))).map(to_doc).unwrap_or_else(|| body_start.clone())
    };
    let link_under = |p: Pos2| -> Option<String> { layout.and_then(|l| l.hit(local(p))).map(str::to_owned) };
    // Drawings take the pointer (and their keys) first: drawing tools, selecting, moving, sizing, typing.
    let on_drawing =
        super::drawing::interact(ui, host, cid, &resp, origin, zoom, select_objects, &hit, &shown_drawings);
    let (pressed, down, mods) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers));
    let hover_link = resp.hover_pos().and_then(link_under);
    if resp.hovered() && !on_drawing {
        let icon = if select_objects {
            CursorIcon::Default
        } else if hover_link.is_some() && mods.command {
            CursorIcon::PointingHand
        } else {
            CursorIcon::Text
        };
        ctx.set_cursor_icon(icon);
    }
    super::claim_initial_focus(ui, cid, &resp);
    let mut changed = false;
    // A double-click on a chart, SmartArt graphic or 3D model opens its editor.
    let edit_pic = (resp.double_clicked() && !on_drawing)
        .then(|| resp.interact_pointer_pos())
        .flatten()
        .and_then(|p| layout.zip(host.compose(cid)).and_then(|(l, c)| editable_picture(l, c, local(p))));
    if on_drawing {
        *drag_anchor = None;
    } else if let Some(pic) = edit_pic {
        *drag_anchor = None;
        host.run("insert.editPicture", with_compose(cid, json!({"cid": pic})));
        changed = true;
    } else if select_objects {
        // Select Objects: a click picks the picture under the pointer; text is not selected.
        if pressed && let Some(p) = resp.interact_pointer_pos() {
            resp.request_focus();
            *drag_anchor = None;
            let picked = layout.zip(host.compose(cid)).and_then(|(l, c)| object_at(l, c, local(p)));
            if let Some((a, b)) = picked {
                select(host, cid, &a, &b);
                changed = true;
            }
        }
    } else if let Some(p) = resp.interact_pointer_pos() {
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
    // Format Painter / highlighter pen: the selection is painted when the mouse button is released.
    // A click or double-click waits until no further click of the same gesture can come (egui reports the
    // first click of a triple-click as a plain click), so a one-shot painter is not used up on the word
    // before the triple-click selects the paragraph.
    if !select_objects && tools_armed(&*host, cid) {
        let now = ui.input(|i| i.time);
        let window = ctx.options(|o| o.input_options.max_double_click_delay).clamp(0.0, 2.0);
        let finished = resp.drag_stopped() || resp.triple_clicked() || (resp.clicked() && mods.shift);
        if finished {
            *paint_due = None;
            if apply_armed_tools(host, cid, false) {
                changed = true;
            }
        } else if resp.double_clicked() {
            // A third click may follow within two click intervals of the first.
            *paint_due = Some((now + 2.0 * window, false));
        } else if resp.clicked() && !mods.command {
            *paint_due = Some((now + window, true));
        }
        // While the button is down a drag or the next click decides.
        if let Some((due, word)) = *paint_due
            && !down
        {
            if now >= due {
                *paint_due = None;
                if apply_armed_tools(host, cid, word) {
                    changed = true;
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64((due - now).clamp(0.0, 4.0)));
            }
        }
        if resp.hovered()
            && let Some(p) = resp.hover_pos()
        {
            // A brush (or pen) by the pointer shows what the next selection does.
            let icon = if host.compose(cid).is_some_and(|c| c.painter.is_some()) {
                "format-painter"
            } else {
                "highlight"
            };
            let r = Rect::from_min_size(p + vec2(8.0, 6.0), vec2(14.0, 14.0));
            crate::icons::Icons::image(icon, 14.0, host.theme().accent).paint_at(ui, r);
        }
    } else {
        *paint_due = None;
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
        && let Some(points) =
            super::drawing::point_menu_at(&ctx, &*host, cid, p, origin, zoom, &shown_drawings)
    {
        // Edit Points: the menu works on the point or segment under the pointer; the caret stays.
        resp.request_focus();
        *menu = Some(MenuAt { points: Some(points), ..MenuAt::default() });
    } else if resp.secondary_clicked()
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
        // Esc puts away an armed drawing tool or deselects a drawing, so the body keeps it then.
        let escape = host.compose(cid).is_some_and(|c| c.draw_tool.is_some() || c.selected_drawing.is_some());
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape },
            );
        });
        // The @mention list takes its keys first (Up / Down / Enter / Tab / Esc while it is open).
        super::mention::take_keys(ui, host, cid);
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
    let live = host.live_proofing();
    if host.proofing() && live.any() {
        if !acted {
            host.refresh_spelling(cid);
        }
        if let (Some(layout), Some(c)) = (layout, host.compose(cid))
            && c.edit_rev == painted_rev
            && c.spell_rev == Some(c.edit_rev)
            && !c.spell_issues.is_empty()
        {
            paint_squiggles(ui, layout, &marked_issues(live, &c.spell_issues), origin, zoom);
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

    // Mini Toolbar over a mouse selection; the @mention list under the caret.
    let mouse_selected = resp.drag_stopped()
        || resp.double_clicked()
        || resp.triple_clicked()
        || (resp.clicked() && mods.shift);
    let selected_at = mouse_selected.then(|| ctx.pointer_latest_pos()).flatten();
    super::minibar::track(&ctx, &*host, cid, selected_at, acted);
    super::minibar::show(&ctx, host, cid);
    let caret_now = host.compose(cid).map(|c| c.selection.focus.clone());
    let caret_rect = caret_now.and_then(|f| {
        mem.view
            .layout
            .as_ref()
            .and_then(|l| l.caret_rect(&to_text(&f)))
            .map(|r| Rect::from_min_size(pos2(r.x, r.y), vec2(1.0, r.h)).translate(origin.to_vec2()))
    });
    super::mention::popup(&ctx, host, cid, caret_rect, resp.has_focus());

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
        // Markers that equal the default ones are left alone.
        let same: Vec<_> = markers.iter().map(|(l, i, _, d)| (l.clone(), *i, d.clone(), d.clone())).collect();
        let mut fresh = layout_doc(&c.body, &db, &LayoutOptions { width: 400.0, ..LayoutOptions::default() });
        assert!(!patch_markers(&mut fresh, &same, &db));
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
    fn only_the_marked_kinds_are_underlined() {
        let issue = |kind: &str| SpellIssue {
            start: DocPos::new(vec![0], 0),
            end: DocPos::new(vec![0], 2),
            word: String::new(),
            kind: kind.into(),
            suggestions: Vec::new(),
            message: String::new(),
        };
        let all = [issue("spelling"), issue("agreement"), issue("wordiness"), issue("passive")];
        let kinds = |live: LiveProofing| -> Vec<String> {
            marked_issues(live, &all).iter().map(|i| i.kind.clone()).collect()
        };
        assert_eq!(kinds(LiveProofing { spelling: true, grammar: true, refinements: true }).len(), 4);
        // Spelling and grammar marked, refinements not: no refinement underlines.
        assert_eq!(
            kinds(LiveProofing { spelling: true, grammar: true, refinements: false }),
            ["spelling", "agreement"]
        );
        assert_eq!(kinds(LiveProofing { spelling: false, grammar: true, refinements: false }), ["agreement"]);
        assert!(kinds(LiveProofing::default()).is_empty());
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
            message: String::new(),
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
    fn formatting_marks_sit_at_spaces_tabs_breaks_and_paragraph_ends() {
        let text = |t: &str| Inline::Text { text: t.into(), style: Default::default() };
        let doc = Doc {
            blocks: vec![
                Block::Paragraph {
                    style: Default::default(),
                    inlines: vec![text("a b\tc"), Inline::LineBreak, text("d")],
                },
                Block::Paragraph { style: Default::default(), inlines: vec![text("next")] },
            ],
            ..Doc::default()
        };
        let db = mailcraft_fonts::FontDb::bundled();
        let layout = layout_doc(&doc, &db, &LayoutOptions { width: 400.0, ..LayoutOptions::default() });
        let marks = mark_glyphs(&layout, &doc, -1.0, 1.0e6);
        let glyphs: String = marks.iter().map(|m| m.0).collect();
        assert_eq!(glyphs, "\u{b7}\u{2192}\u{21b5}\u{b6}\u{b6}", "{glyphs:?}");
        let space_x = marks[0].1;
        assert!(space_x > 2.0 && space_x < 40.0, "{marks:?}");
        // Off-screen lines draw nothing.
        assert!(mark_glyphs(&layout, &doc, 5000.0, 6000.0).is_empty());
    }

    #[test]
    fn gridlines_only_for_tables_without_borders() {
        let db = mailcraft_fonts::FontDb::bundled();
        let open = mailcraft_html::html_to_doc(
            "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>",
        );
        let rects = gridlines(&db, &open, 400.0, 1.0);
        assert!(rects.len() >= 4, "{rects:?}");
        let boxed = mailcraft_html::html_to_doc("<table border=\"1\"><tr><td>a</td></tr></table>");
        assert!(matches!(boxed.blocks.first(), Some(Block::Table { border: true, .. })), "{boxed:?}");
        assert!(gridlines(&db, &boxed, 400.0, 1.0).is_empty());
        assert!(gridlines(&db, &Doc::from_plain_text("no table"), 400.0, 1.0).is_empty());
    }

    #[test]
    fn select_objects_hits_the_picture_under_the_pointer() {
        let mut c = ComposeState::default();
        c.insert_text("ab");
        c.insert_image(
            mailcraft_engine::compose::InlineImage {
                content_id: "p1@x".into(),
                filename: "p.png".into(),
                mime_type: "image/png".into(),
                data: vec![1, 2, 3],
            },
            "Pic",
            Some(40.0),
            Some(30.0),
        );
        let db = mailcraft_fonts::FontDb::bundled();
        let layout = layout_doc(&c.body, &db, &LayoutOptions { width: 400.0, ..LayoutOptions::default() });
        let r = layout
            .display
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Image { rect, .. } => Some(*rect),
                _ => None,
            })
            .unwrap();
        let hit =
            object_at(&layout, &c, mailcraft_geom::Point { x: r.x + r.w / 2.0, y: r.y + r.h / 2.0 }).unwrap();
        assert_eq!(hit.1.offset, hit.0.offset + 1);
        assert_eq!(leaf_chars(&c.body, &hit.0.path).get(hit.0.offset), Some(&'\u{fffc}'));
        assert!(object_at(&layout, &c, mailcraft_geom::Point { x: r.x - 30.0, y: r.y - 30.0 }).is_none());
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 120, 200, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn compose_pictures_go_through_the_body_painter() {
        let mut c = ComposeState::default();
        c.insert_text("ab");
        for (cid, name, mime, data) in [
            ("p1@x", "p.png", "image/png", png(6, 3)),
            ("s1@x", "s.svg", "image/svg+xml", br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="4"><rect width="8" height="4" fill="red"/></svg>"#.to_vec()),
            ("bad@x", "b.png", "image/png", b"junk".to_vec()),
        ] {
            c.insert_image(
                mailcraft_engine::compose::InlineImage {
                    content_id: cid.into(),
                    filename: name.into(),
                    mime_type: mime.into(),
                    data,
                },
                "Pic",
                Some(40.0),
                Some(20.0),
            );
        }
        assert_eq!(inline_picture(&c, "cid:P1@x").map(<[u8]>::len), Some(png(6, 3).len()));
        assert!(inline_picture(&c, "cid:missing").is_none());
        assert!(inline_picture(&c, "https://example.com/a.png").is_none());
        let db = mailcraft_fonts::FontDb::bundled();
        let layout = layout_doc(&c.body, &db, &LayoutOptions { width: 400.0, ..LayoutOptions::default() });
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        // Off screen: nothing is decoded.
        assert!(!provide_pictures(&ctx, &layout, &c, pos2(0.0, 5_000.0), screen));
        assert!(crate::body::image_texture(&ctx, "cid:p1@x").is_none());
        // On screen: the PNG and the SVG are decoded once, into the cache the body painter draws from.
        assert!(provide_pictures(&ctx, &layout, &c, Pos2::ZERO, screen));
        assert_eq!(crate::body::image_texture(&ctx, "cid:p1@x").map(|t| t.size()), Some([6, 3]));
        assert!(crate::body::image_texture(&ctx, "cid:s1@x").is_some_and(|t| t.size()[0] >= 8));
        assert!(crate::body::image_texture(&ctx, "cid:bad@x").is_none(), "junk keeps the placeholder");
        assert!(!provide_pictures(&ctx, &layout, &c, Pos2::ZERO, screen), "already decoded");
        // An edited picture (same content id, new bytes) is decoded again.
        if let Some(img) = c.inline_images.iter_mut().find(|i| i.content_id == "p1@x") {
            img.data = png(2, 2);
        }
        assert!(provide_pictures(&ctx, &layout, &c, Pos2::ZERO, screen));
        assert_eq!(crate::body::image_texture(&ctx, "cid:p1@x").map(|t| t.size()), Some([2, 2]));
    }
}
