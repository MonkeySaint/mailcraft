//! Compose state: new messages, replies, forwards (window or inline in the reading pane), plus the body editor
//! model that the `compose.*`, `format.*`, `insert.*` and `review.*` commands act on.
//!
//! Body positions are [`DocPos`], the serialisable twin of [`mailcraft_layout::TextPos`]: a block path through
//! nested blocks (lists add `[item, block_in_item]`, quotes and containers `[child]`, tables
//! `[row, cell, block_in_cell]`) and a char offset into the flattened inline text of a *leaf* block, where a
//! line break and an image each count as one char. Leaf blocks (where a caret can sit) are paragraphs,
//! headings, preformatted blocks and rules (length 0). Leaf paths in document order are lexicographically
//! increasing, so `DocPos` ordering is document order.
//!
//! Every editing operation is total: stale or hostile positions are snapped to the nearest leaf, offsets are
//! clamped, and nothing indexes without `get`.

use serde::{Deserialize, Serialize};

use mailcraft_geom::Color;
use mailcraft_layout::TextPos;
use mailcraft_model::rich::{Align, ListKind, TableCell};
use mailcraft_model::{
    Block, BodyFormat, DateTime, Doc, FlagState, FolderId, Importance, Inline, ItemId, ParaStyle,
    Sensitivity, TextStyle,
};

/// Deepest block path accepted from callers.
pub const MAX_PATH_DEPTH: usize = 64;
/// Deepest block nesting walked by the editor.
const MAX_WALK_DEPTH: usize = 64;
/// Deepest link nesting flattened.
const MAX_INLINE_DEPTH: usize = 64;
/// Editor undo depth per compose.
pub const MAX_EDIT_UNDO: usize = 100;
/// In-app clipboard history length.
pub const MAX_CLIPBOARD: usize = 24;
/// Paragraph indent step (half an inch at 96 dpi).
pub const INDENT_STEP_PX: f32 = 48.0;
/// Most chars inserted by one call.
pub const MAX_INSERT_CHARS: usize = 4 * 1024 * 1024;
/// Most replacements done by one Replace All.
const MAX_REPLACEMENTS: usize = 100_000;
/// Most nested list levels.
pub const MAX_LIST_LEVEL: usize = 9;
/// The object replacement char that stands for an image in flattened text.
const OBJECT_CHAR: char = '\u{FFFC}';

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComposeKind {
    #[default]
    New,
    Reply,
    ReplyAll,
    Forward,
    /// Editing an existing draft.
    Draft,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ComposeAttachment {
    pub filename: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// A position in the compose body (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DocPos {
    pub path: Vec<usize>,
    pub offset: usize,
}

impl DocPos {
    pub fn new(path: Vec<usize>, offset: usize) -> Self {
        Self { path, offset }
    }
}

impl From<TextPos> for DocPos {
    fn from(t: TextPos) -> Self {
        Self { path: t.block_path, offset: t.offset }
    }
}

impl From<DocPos> for TextPos {
    fn from(p: DocPos) -> Self {
        TextPos { block_path: p.path, offset: p.offset }
    }
}

/// Body selection: `anchor` is where it started, `focus` is the caret end.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodySelection {
    pub anchor: DocPos,
    pub focus: DocPos,
}

impl BodySelection {
    pub fn caret(p: DocPos) -> Self {
        Self { anchor: p.clone(), focus: p }
    }
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.focus
    }
    /// The earlier end.
    pub fn start(&self) -> &DocPos {
        if self.anchor <= self.focus { &self.anchor } else { &self.focus }
    }
    /// The later end.
    pub fn end(&self) -> &DocPos {
        if self.anchor <= self.focus { &self.focus } else { &self.anchor }
    }
}

/// Where the sent copy goes (Options › Save Sent Item To).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SaveSentTo {
    #[default]
    Default,
    Folder(FolderId),
    DoNotSave,
}

/// Document theme choices applied to the draft (Options › Themes).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeState {
    pub name: Option<String>,
    pub colors: Option<String>,
    pub fonts: Option<String>,
    pub effects: Option<String>,
}

/// The signature block currently in the body (so a new choice replaces it).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SignatureBlock {
    pub name: String,
    /// Blocks as inserted (wrapped in one `Container` in the body).
    pub blocks: Vec<Block>,
}

/// An image embedded in the body, referenced as `cid:<content_id>`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineImage {
    pub content_id: String,
    pub filename: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub pos: DocPos,
}

/// One in-app clipboard entry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipEntry {
    pub text: String,
    pub doc: Doc,
}

/// A saved reusable block (Quick Parts, AutoText, text boxes, quick tables, equations).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BuildingBlock {
    pub gallery: String,
    pub name: String,
    pub blocks: Vec<Block>,
}

/// Captured formatting while Format Painter is armed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PainterState {
    pub style: TextStyle,
    pub para: ParaStyle,
    /// Stays armed after applying (double-click in the UI).
    pub sticky: bool,
}

/// A style created from a selection (Styles › Create a Style).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NamedStyle {
    pub name: String,
    pub style: TextStyle,
    pub para: ParaStyle,
}

/// Editor-only view toggles.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorFlags {
    pub show_marks: bool,
    pub show_gridlines: bool,
    pub select_objects: bool,
    pub selection_pane: bool,
    pub styles_pane: bool,
    pub clipboard_pane: bool,
    /// Highlight palette shows only high-contrast colours.
    pub high_contrast_only: bool,
}

/// One editor undo step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditSnapshot {
    pub body: Doc,
    pub selection: BodySelection,
    /// The signature block the body held (so Undo of a signature change restores which one it is).
    pub signature: Option<SignatureBlock>,
}

/// Letter-case rewrite kinds (Change Case).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseKind {
    Sentence,
    Lower,
    Upper,
    Title,
    Toggle,
}

/// How pasted content is merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteMode {
    KeepSource,
    Merge,
    TextOnly,
}

/// Word Count figures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordCount {
    pub words: usize,
    pub characters_no_spaces: usize,
    pub characters_with_spaces: usize,
    pub paragraphs: usize,
    pub lines: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComposeState {
    pub id: u64,
    pub kind: ComposeKind,
    /// The message replied to / forwarded.
    pub source: Option<ItemId>,
    /// Draft item backing this compose once saved.
    pub draft: Option<ItemId>,
    /// Inline in the reading pane (true) or its own window.
    pub inline: bool,
    /// Recipient fields as typed ("Name <a@b>; c@d").
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub from: String,
    pub subject: String,
    pub body: Doc,
    pub format: BodyFormat,
    pub importance: Importance,
    pub attachments: Vec<ComposeAttachment>,
    pub show_bcc: bool,
    pub show_from: bool,
    pub request_read_receipt: bool,
    pub request_delivery_receipt: bool,
    /// Changed since last save (drives the save prompt on close).
    pub dirty: bool,

    // --- added in commit 3 (compose/format/insert/review commands) ---
    /// Body selection / caret.
    pub selection: BodySelection,
    /// Extra ranges selected by Select Similar Formatting (character formatting applies to them too).
    pub extra_ranges: Vec<BodySelection>,
    /// Typing style for a collapsed selection (Bold with nothing selected, …).
    pub pending_style: Option<TextStyle>,
    pub sensitivity: Sensitivity,
    /// Direct Replies To (Reply-To) as typed.
    pub reply_to: String,
    /// Delay Delivery: hold in the Outbox until this time.
    pub delay_until: Option<DateTime>,
    /// Follow-up flag set on the outgoing message.
    pub flag: FlagState,
    /// Voting button choices ("Yes;No"), empty = none.
    pub voting: String,
    pub save_sent: SaveSentTo,
    pub encrypt: bool,
    pub theme: ThemeState,
    pub style_set: Option<String>,
    pub paragraph_spacing_preset: Option<String>,
    pub custom_styles: Vec<NamedStyle>,
    /// Switch Background: show the body on a light canvas while dark mode is on.
    pub light_canvas: bool,
    /// Body zoom percent; `None` = 100.
    pub zoom: Option<u16>,
    pub signature: Option<SignatureBlock>,
    pub inline_images: Vec<InlineImage>,
    pub bookmarks: Vec<Bookmark>,
    /// Proofing language (BCP 47 tag).
    pub language: Option<String>,
    /// In-app clipboard history (newest last).
    pub clipboard: Vec<ClipEntry>,
    pub building_blocks: Vec<BuildingBlock>,
    pub painter: Option<PainterState>,
    pub editor: EditorFlags,
    pub last_font_color: Option<Color>,
    pub last_highlight: Option<Color>,
    /// Body before Translate Message replaced it (show original toggle).
    pub original_body: Option<Doc>,
    pub translated_to: Option<String>,
    /// Last Find text (Find Next continues with it).
    pub find_text: String,
    #[serde(skip)]
    pub edit_undo: Vec<EditSnapshot>,
    #[serde(skip)]
    pub edit_redo: Vec<EditSnapshot>,

    // --- added in commit 6 (compose depth: spelling) ---
    /// Body edit counter (bumped by every checkpointed edit, typing and undo/redo); the spelling cache is
    /// refreshed when it moves.
    #[serde(skip)]
    pub edit_rev: u64,
    /// Cached spelling issues, valid while `spell_rev == Some(edit_rev)`.
    #[serde(skip)]
    pub spell_issues: Vec<SpellIssue>,
    #[serde(skip)]
    pub spell_rev: Option<u64>,
    /// Words ignored in this message (Ignore All), compared case-insensitively.
    pub ignored_words: Vec<String>,
    /// Single occurrences ignored (Ignore Once): where the word starts and the word.
    pub ignored_once: Vec<(DocPos, String)>,

    // --- added in the commit 6 review ---
    /// The last coalescing edit (see [`ComposeState::run_edit`]): its kind, the caret it left and the edit
    /// counter after it.
    #[serde(skip)]
    pub edit_run: Option<(EditRun, DocPos, u64)>,
}

/// Edits that share one undo step while they follow each other at the caret (typing a word, holding
/// Backspace), the way the classic editor undoes "Typing" as one action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditRun {
    Typing,
    DeleteBackward,
    DeleteForward,
}

/// One spelling issue in the compose body (char range of a leaf, see [`DocPos`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpellIssue {
    pub start: DocPos,
    pub end: DocPos,
    pub word: String,
    /// "spelling" or "repeated" (a doubled word).
    pub kind: String,
    /// Replacement suggestions (best first; may be empty, filled lazily by the checker).
    pub suggestions: Vec<String>,
}

/// One issue a spelling checker reports for a text: char range `[start, end)` and kind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextIssue {
    pub start: usize,
    pub end: usize,
    pub kind: String,
    pub suggestions: Vec<String>,
}

/// Table cell navigation (Tab / Shift+Tab / arrow keys between rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellMove {
    Next,
    Previous,
    Up,
    Down,
}

/// Table structure edits (Table Layout › Rows & Columns, Delete).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableEdit {
    InsertRowAbove,
    InsertRowBelow,
    InsertColumnLeft,
    InsertColumnRight,
    DeleteRow,
    DeleteColumn,
    DeleteTable,
}

/// Most attachments on one compose.
pub const MAX_COMPOSE_ATTACHMENTS: usize = 500;
/// Longest attachment file name (chars).
pub const MAX_ATTACHMENT_NAME_CHARS: usize = 255;
/// Most spelling issues kept per check.
pub const MAX_SPELL_ISSUES: usize = 5_000;

impl ComposeState {
    /// Window title: "Untitled - Message (HTML)" / "<subject> - Message (Plain Text)".
    pub fn title(&self) -> String {
        let subj = if self.subject.trim().is_empty() { "Untitled" } else { self.subject.trim() };
        format!("{subj} - Message ({})", self.format.title_label())
    }

    /// Body zoom percent.
    pub fn zoom_pct(&self) -> u16 {
        self.zoom.unwrap_or(100)
    }

    // ---------------------------------------------------------------------------------------------------
    // Selection
    // ---------------------------------------------------------------------------------------------------

    /// Makes sure the body has a leaf block and the selection points at valid leaves.
    pub fn ensure_body(&mut self) {
        ensure_leaf(&mut self.body);
        self.selection.anchor = normalize(&self.body, &self.selection.anchor);
        self.selection.focus = normalize(&self.body, &self.selection.focus);
        let body = &self.body;
        self.extra_ranges.retain(|r| block_at(&body.blocks, &r.anchor.path).is_some());
    }

    /// Caret position for the layout engine.
    pub fn caret(&self) -> TextPos {
        self.selection.focus.clone().into()
    }

    /// Whether some body text is selected.
    pub fn has_selection(&self) -> bool {
        !self.selection.is_collapsed()
    }

    /// Moves the selection (normalised). Clears the typing style and Select Similar ranges; applies an armed
    /// Format Painter to a non-empty new selection.
    pub fn set_selection(&mut self, anchor: DocPos, focus: DocPos) {
        ensure_leaf(&mut self.body);
        self.selection =
            BodySelection { anchor: normalize(&self.body, &anchor), focus: normalize(&self.body, &focus) };
        self.pending_style = None;
        self.extra_ranges.clear();
        if self.has_selection()
            && let Some(p) = self.painter.clone()
        {
            self.checkpoint();
            let style = p.style.clone();
            self.restyle_ranges(&mut |s| *s = style.clone());
            let para = p.para.clone();
            self.for_each_para(&mut |ps| *ps = para.clone());
            if !p.sticky {
                self.painter = None;
            }
            self.dirty = true;
        }
    }

    /// Collapses the selection to `pos`.
    pub fn set_caret(&mut self, pos: DocPos) {
        self.set_selection(pos.clone(), pos);
    }

    /// Selects the whole body.
    pub fn select_all(&mut self) {
        ensure_leaf(&mut self.body);
        let leaves = leaves(&self.body.blocks);
        let first = leaves.first().cloned().unwrap_or_default();
        let last = leaves.last().cloned().unwrap_or_default();
        let len = leaf_len(&self.body.blocks, &last);
        self.set_selection(DocPos::new(first, 0), DocPos::new(last, len));
    }

    /// Start of the body.
    pub fn body_start(&self) -> DocPos {
        DocPos::new(leaves(&self.body.blocks).first().cloned().unwrap_or_default(), 0)
    }

    /// End of the body.
    pub fn body_end(&self) -> DocPos {
        let last = leaves(&self.body.blocks).last().cloned().unwrap_or_default();
        let len = leaf_len(&self.body.blocks, &last);
        DocPos::new(last, len)
    }

    /// Selected text (paragraphs joined with "\n"); empty when collapsed.
    pub fn selected_text(&self) -> String {
        if self.selection.is_collapsed() {
            return String::new();
        }
        range_text(&self.body, self.selection.start(), self.selection.end())
    }

    /// Ranges character formatting applies to: the selection plus Select Similar ranges.
    fn style_ranges(&self) -> Vec<(DocPos, DocPos)> {
        let mut v = vec![(self.selection.start().clone(), self.selection.end().clone())];
        for r in &self.extra_ranges {
            v.push((r.start().clone(), r.end().clone()));
        }
        v
    }

    // ---------------------------------------------------------------------------------------------------
    // Undo
    // ---------------------------------------------------------------------------------------------------

    /// The state an undo step brings back.
    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            body: self.body.clone(),
            selection: self.selection.clone(),
            signature: self.signature.clone(),
        }
    }

    /// Puts `snap` in place and returns the state it replaced.
    fn restore(&mut self, snap: EditSnapshot) -> EditSnapshot {
        EditSnapshot {
            body: std::mem::replace(&mut self.body, snap.body),
            selection: std::mem::replace(&mut self.selection, snap.selection),
            signature: std::mem::replace(&mut self.signature, snap.signature),
        }
    }

    /// Records an editor undo step (call before changing the body).
    pub fn checkpoint(&mut self) {
        self.mark_edited();
        self.edit_undo.push(self.snapshot());
        if self.edit_undo.len() > MAX_EDIT_UNDO {
            self.edit_undo.remove(0);
        }
        self.edit_redo.clear();
    }

    /// Runs one editor action `f` as an undo step. With `run`, the action joins the previous step when that
    /// was the same kind of edit and nothing happened since (no other edit, caret where it was left, nothing
    /// selected). An action that leaves the body unchanged records no step and keeps the redo history.
    pub fn run_edit(&mut self, run: Option<EditRun>, f: impl FnOnce(&mut Self)) {
        let joins = run.is_some()
            && self.selection.is_collapsed()
            && !self.edit_undo.is_empty()
            && self.edit_run.as_ref().is_some_and(|(k, at, rev)| {
                Some(*k) == run && *at == self.selection.focus && *rev == self.edit_rev
            });
        let before = self.snapshot();
        let dirty = self.dirty;
        f(self);
        if self.body == before.body {
            self.dirty = dirty;
            if self.selection != before.selection {
                self.edit_run = None;
            }
            return;
        }
        if !joins {
            self.edit_undo.push(before);
            if self.edit_undo.len() > MAX_EDIT_UNDO {
                self.edit_undo.remove(0);
            }
        }
        self.edit_redo.clear();
        self.dirty = true;
        self.mark_edited();
        self.edit_run = run.map(|k| (k, self.selection.focus.clone(), self.edit_rev));
    }

    /// Undoes the last body edit; false when there is nothing to undo.
    pub fn undo_edit(&mut self) -> bool {
        let Some(snap) = self.edit_undo.pop() else {
            return false;
        };
        let cur = self.restore(snap);
        self.edit_redo.push(cur);
        self.dirty = true;
        self.mark_edited();
        self.ensure_body();
        true
    }

    /// Redoes the last undone body edit.
    pub fn redo_edit(&mut self) -> bool {
        let Some(snap) = self.edit_redo.pop() else {
            return false;
        };
        let cur = self.restore(snap);
        self.edit_undo.push(cur);
        self.dirty = true;
        self.mark_edited();
        self.ensure_body();
        true
    }

    // ---------------------------------------------------------------------------------------------------
    // Text editing
    // ---------------------------------------------------------------------------------------------------

    /// Types `text` at the caret, replacing the selection. `\n` starts a new paragraph (a new list item
    /// inside a list; a line break inside preformatted text). Uses the pending typing style when set.
    pub fn insert_text(&mut self, text: &str) {
        self.ensure_body();
        self.delete_selection();
        let text = clean_text(text);
        let style = self.pending_style.take();
        let mut pos = self.selection.focus.clone();
        for (i, seg) in text.split('\n').enumerate() {
            if i > 0 {
                pos = split_at(&mut self.body, &pos, true);
            }
            if !seg.is_empty() {
                pos = insert_chars(&mut self.body, &pos, seg, style.as_ref());
            }
        }
        self.selection = BodySelection::caret(normalize(&self.body, &pos));
        self.extra_ranges.clear();
        self.dirty = true;
        self.mark_edited();
    }

    /// Deletes the selected text; false when the selection is collapsed.
    pub fn delete_selection(&mut self) -> bool {
        if self.selection.is_collapsed() {
            return false;
        }
        let start = self.selection.start().clone();
        let end = self.selection.end().clone();
        let pos = delete_range(&mut self.body, &start, &end);
        self.selection = BodySelection::caret(normalize(&self.body, &pos));
        self.extra_ranges.clear();
        self.dirty = true;
        self.mark_edited();
        true
    }

    /// Deletes `start..end` and puts the caret at `start`.
    pub fn delete_range(&mut self, start: &DocPos, end: &DocPos) {
        let (a, b) = if start <= end { (start, end) } else { (end, start) };
        let a = normalize(&self.body, a);
        let b = normalize(&self.body, b);
        let pos = delete_range(&mut self.body, &a, &b);
        self.selection = BodySelection::caret(normalize(&self.body, &pos));
        self.dirty = true;
        self.mark_edited();
    }

    /// Backspace.
    pub fn delete_backward(&mut self) {
        self.ensure_body();
        if self.delete_selection() {
            return;
        }
        let pos = self.selection.focus.clone();
        if pos.offset > 0 {
            let start = DocPos::new(pos.path.clone(), pos.offset.saturating_sub(1));
            self.delete_range(&start, &pos);
            return;
        }
        // At the start of a list item's first block: remove the list formatting first.
        if let Some((Owner::ListItem { list, item }, 0)) = parent_of(&self.body.blocks, &pos.path) {
            let ord = leaf_ordinal(&self.body.blocks, &pos.path);
            unwrap_items(&mut self.body.blocks, &list, item, item);
            self.restore_caret_ord(ord, 0);
            self.dirty = true;
            return;
        }
        let leaves = leaves(&self.body.blocks);
        let Some(k) = leaves.iter().position(|p| *p == pos.path) else {
            return;
        };
        let Some(prev) = k.checked_sub(1).and_then(|k| leaves.get(k)) else {
            return;
        };
        let prev_len = leaf_len(&self.body.blocks, prev);
        let start = DocPos::new(prev.clone(), prev_len);
        // Never merge across a table cell boundary: the caret moves into the previous cell / out of the table,
        // and an empty paragraph after a table goes away (not the last one, so text can follow the table).
        if cell_of(&self.body.blocks, prev) != cell_of(&self.body.blocks, &pos.path) {
            if leaves.get(k.saturating_add(1)).is_some() && self.removable_empty_leaf(&pos.path) {
                remove_block(&mut self.body.blocks, &pos.path);
                self.dirty = true;
                self.mark_edited();
            }
            self.selection = BodySelection::caret(normalize(&self.body, &start));
            self.extra_ranges.clear();
            return;
        }
        self.delete_range(&start, &pos);
    }

    /// Whether the leaf at `path` is an empty paragraph outside any table cell (Backspace / Delete next to a
    /// table may remove it).
    fn removable_empty_leaf(&self, path: &[usize]) -> bool {
        matches!(block_at(&self.body.blocks, path), Some(Block::Paragraph { .. }))
            && leaf_len(&self.body.blocks, path) == 0
            && cell_of(&self.body.blocks, path).is_none()
    }

    /// Delete key.
    pub fn delete_forward(&mut self) {
        self.ensure_body();
        if self.delete_selection() {
            return;
        }
        let pos = self.selection.focus.clone();
        let len = leaf_len(&self.body.blocks, &pos.path);
        if pos.offset < len {
            let end = DocPos::new(pos.path.clone(), pos.offset.saturating_add(1));
            self.delete_range(&pos, &end);
            return;
        }
        let leaves = leaves(&self.body.blocks);
        let Some(k) = leaves.iter().position(|p| *p == pos.path) else {
            return;
        };
        let Some(next) = leaves.get(k.saturating_add(1)) else {
            return;
        };
        // Never merge across a table cell boundary; an empty paragraph before a table goes away.
        if cell_of(&self.body.blocks, next) != cell_of(&self.body.blocks, &pos.path) {
            if self.removable_empty_leaf(&pos.path) {
                remove_block(&mut self.body.blocks, &pos.path);
                self.restore_caret_ord(k, 0);
                self.dirty = true;
                self.mark_edited();
            }
            return;
        }
        let end = DocPos::new(next.clone(), 0);
        self.delete_range(&pos, &end);
    }

    // ---------------------------------------------------------------------------------------------------
    // Character formatting
    // ---------------------------------------------------------------------------------------------------

    /// Explicit style at the caret: the typing style when set, else the style of the char before the caret
    /// (or after it at the start of a block).
    pub fn style_at_caret(&self) -> TextStyle {
        if let Some(p) = &self.pending_style {
            return p.clone();
        }
        let pos = &self.selection.focus;
        let Some((_, flat)) = leaf(&self.body.blocks, &pos.path) else {
            return TextStyle::default();
        };
        let i = pos.offset.min(flat.len());
        let atom = i.checked_sub(1).and_then(|j| flat.atoms.get(j)).or_else(|| flat.atoms.get(i));
        atom.and_then(|a| flat.attrs.get(a.attr)).map(|r| r.style.clone()).unwrap_or_default()
    }

    /// Paragraph style of the leaf at the caret.
    pub fn para_at_caret(&self) -> ParaStyle {
        match block_at(&self.body.blocks, &self.selection.focus.path) {
            Some(Block::Paragraph { style, .. } | Block::Heading { style, .. }) => style.clone(),
            _ => ParaStyle::default(),
        }
    }

    /// Whether every selected char satisfies `pred(style, in_heading)` (the caret style when collapsed).
    pub fn selection_all(&self, pred: impl Fn(&TextStyle, bool) -> bool) -> bool {
        let in_heading =
            |path: &[usize]| matches!(block_at(&self.body.blocks, path), Some(Block::Heading { .. }));
        if self.selection.is_collapsed() {
            return pred(&self.style_at_caret(), in_heading(&self.selection.focus.path));
        }
        let mut any = false;
        for (a, b) in self.style_ranges() {
            for (path, from, to) in leaf_ranges(&self.body.blocks, &a, &b) {
                let Some((kind, flat)) = leaf(&self.body.blocks, &path) else {
                    continue;
                };
                let heading = kind == LeafKind::Heading;
                for atom in flat.atoms.get(from..to).unwrap_or_default() {
                    if let AtomKind::Char(c) = atom.kind
                        && !c.is_whitespace()
                    {
                        any = true;
                        let style = flat.attrs.get(atom.attr).map(|r| &r.style);
                        if !pred(style.unwrap_or(&TextStyle::default()), heading) {
                            return false;
                        }
                    }
                }
            }
        }
        any
    }

    /// Applies `f` to the character style of the selection (and Select Similar ranges); with a collapsed
    /// selection it changes the typing style instead.
    pub fn apply_text_style(&mut self, mut f: impl FnMut(&mut TextStyle)) {
        self.ensure_body();
        if self.selection.is_collapsed() && self.extra_ranges.is_empty() {
            let mut s = self.style_at_caret();
            f(&mut s);
            self.pending_style = Some(s);
            return;
        }
        self.restyle_ranges(&mut f);
        self.dirty = true;
    }

    fn restyle_ranges(&mut self, f: &mut dyn FnMut(&mut TextStyle)) {
        for (a, b) in self.style_ranges() {
            for (path, from, to) in leaf_ranges(&self.body.blocks, &a, &b) {
                if from >= to {
                    continue;
                }
                let Some((kind, mut flat)) = leaf(&self.body.blocks, &path) else {
                    continue;
                };
                if matches!(kind, LeafKind::Pre | LeafKind::Rule) {
                    continue;
                }
                flat.restyle(from, to, &mut |r| f(&mut r.style));
                store_leaf(&mut self.body.blocks, &path, &flat);
            }
        }
    }

    /// Links the selection to `href` (`None` removes links). With a collapsed selection and `text`, inserts
    /// the text as a link.
    pub fn set_link(&mut self, href: Option<&str>, text: Option<&str>) {
        self.ensure_body();
        if self.selection.is_collapsed() {
            let Some(h) = href else {
                // Remove the link around the caret.
                let pos = self.selection.focus.clone();
                if let Some((_, mut flat)) = leaf(&self.body.blocks, &pos.path) {
                    let i = pos.offset.min(flat.len());
                    let link = i
                        .checked_sub(1)
                        .and_then(|j| flat.atoms.get(j))
                        .and_then(|a| flat.attrs.get(a.attr))
                        .and_then(|r| r.link.clone());
                    if let Some(link) = link {
                        let (s, e) = flat.link_extent(i.saturating_sub(1), &link);
                        flat.restyle(s, e, &mut |r| r.link = None);
                        store_leaf(&mut self.body.blocks, &pos.path, &flat);
                        self.dirty = true;
                    }
                }
                return;
            };
            let shown = text.filter(|t| !t.is_empty()).unwrap_or(h);
            let start = self.selection.focus.clone();
            self.insert_text(shown);
            let end = self.selection.focus.clone();
            self.selection = BodySelection { anchor: start, focus: end.clone() };
            self.set_link_on_selection(Some(h));
            self.selection = BodySelection::caret(end);
            return;
        }
        if let Some(t) = text.filter(|t| !t.is_empty())
            && t != self.selected_text()
        {
            let start = self.selection.start().clone();
            self.insert_text(t);
            let end = self.selection.focus.clone();
            self.selection = BodySelection { anchor: start, focus: end };
        }
        self.set_link_on_selection(href);
    }

    fn set_link_on_selection(&mut self, href: Option<&str>) {
        let (a, b) = (self.selection.start().clone(), self.selection.end().clone());
        for (path, from, to) in leaf_ranges(&self.body.blocks, &a, &b) {
            let Some((kind, mut flat)) = leaf(&self.body.blocks, &path) else {
                continue;
            };
            if matches!(kind, LeafKind::Pre | LeafKind::Rule) {
                continue;
            }
            flat.restyle(from, to, &mut |r| r.link = href.map(str::to_owned));
            store_leaf(&mut self.body.blocks, &path, &flat);
        }
        self.dirty = true;
    }

    /// Link target at the caret, if any.
    pub fn link_at_caret(&self) -> Option<String> {
        let pos = &self.selection.focus;
        let (_, flat) = leaf(&self.body.blocks, &pos.path)?;
        let i = pos.offset.min(flat.len());
        let atom = i.checked_sub(1).and_then(|j| flat.atoms.get(j)).or_else(|| flat.atoms.get(i))?;
        flat.attrs.get(atom.attr)?.link.clone()
    }

    /// Change Case on the selection (the word at the caret when collapsed).
    pub fn change_case(&mut self, kind: CaseKind) {
        self.ensure_body();
        if self.selection.is_collapsed() {
            self.select_word_at_caret();
        }
        let (a, b) = (self.selection.start().clone(), self.selection.end().clone());
        for (path, from, to) in leaf_ranges(&self.body.blocks, &a, &b) {
            let Some((kind_leaf, mut flat)) = leaf(&self.body.blocks, &path) else {
                continue;
            };
            if kind_leaf == LeafKind::Rule {
                continue;
            }
            flat.change_case(from, to, kind);
            store_leaf(&mut self.body.blocks, &path, &flat);
        }
        self.dirty = true;
    }

    /// Extends a collapsed selection to the word around the caret.
    pub fn select_word_at_caret(&mut self) {
        let pos = self.selection.focus.clone();
        let Some((_, flat)) = leaf(&self.body.blocks, &pos.path) else {
            return;
        };
        let chars: Vec<char> = flat.text().chars().collect();
        let is_word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric() || *c == '\'' || *c == '-');
        let mut s = pos.offset.min(chars.len());
        while s > 0 && is_word(chars.get(s.saturating_sub(1))) {
            s = s.saturating_sub(1);
        }
        let mut e = pos.offset.min(chars.len());
        while e < chars.len() && is_word(chars.get(e)) {
            e = e.saturating_add(1);
        }
        self.selection =
            BodySelection { anchor: DocPos::new(pos.path.clone(), s), focus: DocPos::new(pos.path, e) };
    }

    // ---------------------------------------------------------------------------------------------------
    // Paragraph formatting
    // ---------------------------------------------------------------------------------------------------

    /// Paths of the leaves the selection touches.
    pub fn selected_leaves(&self) -> Vec<Vec<usize>> {
        leaf_ranges(&self.body.blocks, self.selection.start(), self.selection.end())
            .into_iter()
            .map(|(p, _, _)| p)
            .collect()
    }

    /// Applies `f` to the paragraph style of every selected paragraph/heading.
    pub fn set_para_style(&mut self, mut f: impl FnMut(&mut ParaStyle)) {
        self.ensure_body();
        self.for_each_para(&mut f);
        self.dirty = true;
    }

    fn for_each_para(&mut self, f: &mut dyn FnMut(&mut ParaStyle)) {
        for path in self.selected_leaves() {
            if let Some(Block::Paragraph { style, .. } | Block::Heading { style, .. }) =
                block_at_mut(&mut self.body.blocks, &path)
            {
                f(style);
            }
        }
    }

    /// Turns the selected paragraphs into headings of `level` (1..=6) or, with `None`, plain paragraphs.
    pub fn set_heading(&mut self, level: Option<u8>) {
        self.ensure_body();
        for path in self.selected_leaves() {
            let Some(b) = block_at_mut(&mut self.body.blocks, &path) else {
                continue;
            };
            let (style, inlines) = match b {
                Block::Paragraph { style, inlines } | Block::Heading { style, inlines, .. } => {
                    (std::mem::take(style), std::mem::take(inlines))
                }
                _ => continue,
            };
            *b = match level {
                Some(l) => Block::Heading { level: l.clamp(1, 6), style, inlines },
                None => Block::Paragraph { style, inlines },
            };
        }
        self.dirty = true;
    }

    /// Kind of the list the caret is in (innermost), with its path.
    pub fn list_at_caret(&self) -> Option<(Vec<usize>, ListKind)> {
        list_of(&self.body.blocks, &self.selection.focus.path)
    }

    /// Bullets / Numbering toggle: wraps the selected paragraphs in a list of `kind`, changes the kind of
    /// the list they are in, or (same kind, or `None`) removes the list formatting.
    pub fn toggle_list(&mut self, kind: Option<ListKind>) {
        self.ensure_body();
        let ords = self.selection_ords();
        let start = self.selection.start().clone();
        let end = self.selection.end().clone();
        match parent_of(&self.body.blocks, &start.path) {
            Some((Owner::ListItem { list, item }, _)) if end.path.starts_with(&list) => {
                let cur = match block_at(&self.body.blocks, &list) {
                    Some(Block::List { kind, .. }) => kind.clone(),
                    _ => return,
                };
                // The end leaf's item index sits right after the list path.
                let last = end.path.get(list.len()).copied().unwrap_or(item).max(item);
                match kind {
                    Some(k) if !same_list_kind(&k, &cur) => {
                        if let Some(Block::List { kind, .. }) = block_at_mut(&mut self.body.blocks, &list) {
                            *kind = k;
                        }
                    }
                    _ => {
                        unwrap_items(&mut self.body.blocks, &list, item, last);
                        self.restore_ords(ords);
                        self.unwrap_selected_items();
                    }
                }
            }
            _ => {
                let Some(k) = kind else {
                    return;
                };
                let Some((&i, prefix)) = start.path.split_last() else {
                    return;
                };
                let j = if end.path.len() > prefix.len() && end.path.starts_with(prefix) {
                    end.path.get(prefix.len()).copied().unwrap_or(i).max(i)
                } else {
                    i
                };
                wrap_in_list(&mut self.body.blocks, prefix, i, j, k);
            }
        }
        self.restore_ords(ords);
        self.dirty = true;
    }

    /// Removes list formatting from every selected paragraph that starts a list item (nested levels too);
    /// continuation paragraphs stay where they are.
    fn unwrap_selected_items(&mut self) {
        for _ in 0..MAX_WALK_DEPTH {
            let sel = self.selected_leaves();
            let first = sel.iter().find_map(|p| match parent_of(&self.body.blocks, p) {
                Some((Owner::ListItem { list, item }, 0)) => Some((list, item)),
                _ => None,
            });
            let Some((list, a)) = first else {
                break;
            };
            let b = sel
                .iter()
                .filter(|p| p.len() > list.len() && p.starts_with(&list))
                .filter_map(|p| p.get(list.len()).copied())
                .max()
                .unwrap_or(a)
                .max(a);
            let ords = self.selection_ords();
            unwrap_items(&mut self.body.blocks, &list, a, b);
            self.restore_ords(ords);
        }
    }

    /// Sets the start number of the numbered list at the caret (creating one when needed).
    pub fn set_list_start(&mut self, start: u32) {
        match self.list_at_caret() {
            Some((path, ListKind::Numbered { .. })) => {
                if let Some(Block::List { kind, .. }) = block_at_mut(&mut self.body.blocks, &path) {
                    *kind = ListKind::Numbered { start };
                }
                self.dirty = true;
            }
            _ => self.toggle_list(Some(ListKind::Numbered { start })),
        }
    }

    /// Increase Indent: demotes list items one level, indents other paragraphs by one step.
    pub fn indent(&mut self) {
        self.shift_level(true, None);
    }

    /// Decrease Indent: promotes list items one level (out of the list at level 1), outdents paragraphs.
    pub fn outdent(&mut self) {
        self.shift_level(false, None);
    }

    /// Change List Level: moves the selected list items to `level` (1-based).
    pub fn set_list_level(&mut self, level: usize) {
        let level = level.clamp(1, MAX_LIST_LEVEL);
        self.shift_level(true, Some(level));
    }

    fn shift_level(&mut self, deeper: bool, target: Option<usize>) {
        self.ensure_body();
        let ords = self.selection_ords();
        let all = leaves(&self.body.blocks);
        let sk = all.iter().position(|p| *p == self.selection.start().path).unwrap_or(0);
        let ek = all.iter().position(|p| *p == self.selection.end().path).unwrap_or(sk).max(sk);
        let initial: Vec<usize> =
            (sk..=ek).map(|k| all.get(k).map_or(0, |p| list_depth(&self.body.blocks, p))).collect();
        let mut current = all;
        let mut stale = false;
        for (n, k) in (sk..=ek).enumerate() {
            let depth0 = initial.get(n).copied().unwrap_or(0);
            if stale {
                current = leaves(&self.body.blocks);
                stale = false;
            }
            let Some(path) = current.get(k).cloned() else {
                break;
            };
            if list_depth(&self.body.blocks, &path) != depth0 {
                continue;
            }
            match parent_of(&self.body.blocks, &path) {
                Some((Owner::ListItem { .. }, idx)) => {
                    if idx != 0 {
                        continue;
                    }
                    stale = true;
                    match target {
                        Some(level) => {
                            for _ in 0..MAX_LIST_LEVEL.saturating_mul(2) {
                                let Some(p) = leaves(&self.body.blocks).get(k).cloned() else {
                                    break;
                                };
                                let d = list_depth(&self.body.blocks, &p);
                                let changed = if d < level {
                                    demote(&mut self.body.blocks, &p)
                                } else if d > level {
                                    promote(&mut self.body.blocks, &p)
                                } else {
                                    break;
                                };
                                if !changed {
                                    break;
                                }
                            }
                        }
                        None if deeper => {
                            if !demote(&mut self.body.blocks, &path) {
                                indent_para(&mut self.body.blocks, &path, INDENT_STEP_PX);
                            }
                        }
                        None => {
                            promote(&mut self.body.blocks, &path);
                        }
                    }
                }
                _ if target.is_some() => {}
                _ => indent_para(
                    &mut self.body.blocks,
                    &path,
                    if deeper { INDENT_STEP_PX } else { -INDENT_STEP_PX },
                ),
            }
        }
        self.restore_ords(ords);
        self.dirty = true;
    }

    /// Sorts the selected paragraphs (or the rows of the table / items of the list holding the selection).
    pub fn sort_selection(&mut self, descending: bool) {
        self.ensure_body();
        let start = self.selection.start().clone();
        let end = self.selection.end().clone();
        let ords = self.selection_ords();
        let sp = parent_of(&self.body.blocks, &start.path);
        let ep = parent_of(&self.body.blocks, &end.path);
        match (sp, ep) {
            (Some((Owner::Cell { table: t1, .. }, _)), Some((Owner::Cell { table: t2, .. }, _)))
                if t1 == t2 =>
            {
                if let Some(Block::Table { rows, .. }) = block_at_mut(&mut self.body.blocks, &t1) {
                    rows.sort_by(|a, b| {
                        let ka = a.first().map(|c| blocks_text(&c.blocks)).unwrap_or_default();
                        let kb = b.first().map(|c| blocks_text(&c.blocks)).unwrap_or_default();
                        sort_cmp(&ka, &kb, descending)
                    });
                }
            }
            (Some((Owner::ListItem { list: l1, .. }, _)), Some((Owner::ListItem { list: l2, .. }, _)))
                if l1 == l2 =>
            {
                if let Some(Block::List { items, .. }) = block_at_mut(&mut self.body.blocks, &l1) {
                    items.sort_by(|a, b| sort_cmp(&blocks_text(a), &blocks_text(b), descending));
                }
            }
            _ => {
                let Some((&i, prefix)) = start.path.split_last() else {
                    return;
                };
                let j = if end.path.len() > prefix.len() && end.path.starts_with(prefix) {
                    end.path.get(prefix.len()).copied().unwrap_or(i).max(i)
                } else {
                    i
                };
                if let Some(seq) = seq_mut(&mut self.body.blocks, prefix)
                    && let Some(part) = seq.get_mut(i..=j)
                {
                    part.sort_by(|a, b| {
                        sort_cmp(
                            &blocks_text(std::slice::from_ref(a)),
                            &blocks_text(std::slice::from_ref(b)),
                            descending,
                        )
                    });
                }
            }
        }
        // Select the sorted range (whole leaves).
        let all = leaves(&self.body.blocks);
        let (sk, ek) = (ords.0.0.min(ords.1.0), ords.0.0.max(ords.1.0));
        let first = all.get(sk).cloned().unwrap_or_default();
        let last = all.get(ek).cloned().unwrap_or_default();
        let len = leaf_len(&self.body.blocks, &last);
        self.selection = BodySelection { anchor: DocPos::new(first, 0), focus: DocPos::new(last, len) };
        self.ensure_body();
        self.dirty = true;
    }

    /// Table cell at the caret.
    pub fn cell_at_caret_mut(&mut self) -> Option<&mut TableCell> {
        let (owner, _) = parent_of(&self.body.blocks, &self.selection.focus.path)?;
        let Owner::Cell { table, row, col } = owner else {
            return None;
        };
        match block_at_mut(&mut self.body.blocks, &table)? {
            Block::Table { rows, .. } => rows.get_mut(row)?.get_mut(col),
            _ => None,
        }
    }

    /// Turns the border of the table holding the caret on or off; false when the caret is not in a table or
    /// nothing changed.
    pub fn set_table_border(&mut self, on: bool) -> bool {
        let Some(path) = self.table_at_caret() else {
            return false;
        };
        match block_at_mut(&mut self.body.blocks, &path) {
            Some(Block::Table { border, .. }) if *border != on => {
                *border = on;
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// Path of the table holding the caret (innermost).
    pub fn table_at_caret(&self) -> Option<Vec<usize>> {
        match parent_of(&self.body.blocks, &self.selection.focus.path)? {
            (Owner::Cell { table, .. }, _) => Some(table),
            _ => None,
        }
    }

    // ---------------------------------------------------------------------------------------------------
    // Blocks, fragments, clipboard
    // ---------------------------------------------------------------------------------------------------

    /// Inserts blocks at the caret (splitting the paragraph when the caret is inside it). The caret moves into
    /// the first table cell for a table, else to the start of the block after the inserted ones.
    pub fn insert_blocks(&mut self, blocks: Vec<Block>) {
        self.ensure_body();
        self.delete_selection();
        if blocks.is_empty() {
            return;
        }
        let pos = self.selection.focus.clone();
        let Some((&idx, prefix)) = pos.path.split_last() else {
            return;
        };
        let prefix = prefix.to_vec();
        let len = leaf_len(&self.body.blocks, &pos.path);
        let kind = leaf(&self.body.blocks, &pos.path).map(|(k, _)| k);
        let at = if kind == Some(LeafKind::Rule) {
            idx.saturating_add(1)
        } else if pos.offset == 0 {
            idx
        } else if pos.offset >= len {
            idx.saturating_add(1)
        } else if kind == Some(LeafKind::Pre) {
            // Enter in preformatted text adds a line break; here the block really splits in two.
            split_pre(&mut self.body, &pos);
            idx.saturating_add(1)
        } else {
            split_at(&mut self.body, &pos, false);
            idx.saturating_add(1)
        };
        let n = blocks.len();
        let first_is_table = matches!(blocks.first(), Some(Block::Table { .. }));
        let Some(seq) = seq_mut(&mut self.body.blocks, &prefix) else {
            return;
        };
        let at = at.min(seq.len());
        seq.splice(at..at, blocks);
        let after = at.saturating_add(n);
        if seq.get(after).is_none() {
            seq.push(Block::text_paragraph(""));
        }
        let mut target = prefix.clone();
        if first_is_table {
            target.push(at);
            let inside = leaves(&self.body.blocks).into_iter().find(|p| p.starts_with(&target));
            if let Some(p) = inside {
                self.selection = BodySelection::caret(DocPos::new(p, 0));
                self.dirty = true;
                return;
            }
            target.pop();
        }
        target.push(after);
        self.selection = BodySelection::caret(normalize(&self.body, &DocPos::new(target, 0)));
        self.dirty = true;
    }

    /// The selection as a stand-alone document (copy).
    pub fn fragment(&self) -> Doc {
        let mut blocks = Vec::new();
        for (path, from, to) in leaf_ranges(&self.body.blocks, self.selection.start(), self.selection.end()) {
            let Some(b) = block_at(&self.body.blocks, &path) else {
                continue;
            };
            let Some((_, flat)) = leaf(&self.body.blocks, &path) else {
                continue;
            };
            let part = flat.slice(from, to);
            blocks.push(match b {
                Block::Paragraph { style, .. } => {
                    Block::Paragraph { style: style.clone(), inlines: part.to_inlines() }
                }
                Block::Heading { level, style, .. } => {
                    Block::Heading { level: *level, style: style.clone(), inlines: part.to_inlines() }
                }
                Block::Pre { .. } => Block::Pre { text: part.plain() },
                _ => Block::Rule,
            });
        }
        Doc { blocks, base: self.body.base.clone(), background: None }
    }

    /// Pastes a document fragment at the caret.
    pub fn insert_fragment(&mut self, frag: &Doc, mode: PasteMode) {
        self.ensure_body();
        if mode == PasteMode::TextOnly {
            self.insert_text(&frag.to_plain_text());
            return;
        }
        let single = match frag.blocks.as_slice() {
            [Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. }] => Some(inlines),
            _ => None,
        };
        let all_text =
            frag.blocks.iter().all(|b| matches!(b, Block::Paragraph { .. } | Block::Heading { .. }));
        if mode == PasteMode::Merge && all_text {
            self.insert_text(&frag.to_plain_text());
            return;
        }
        let Some(inlines) = single else {
            if !self.merge_paste(&frag.blocks) {
                self.insert_blocks(frag.blocks.clone());
            }
            return;
        };
        self.delete_selection();
        let pos = self.selection.focus.clone();
        let Some((kind, mut flat)) = leaf(&self.body.blocks, &pos.path) else {
            return;
        };
        if kind == LeafKind::Rule {
            self.insert_text(&Flat::from_inlines(inlines).plain());
            return;
        }
        let mut add = Flat::from_inlines(inlines);
        if kind == LeafKind::Pre {
            add = Flat::from_text(&add.plain());
        }
        let n = flat.insert_flat(pos.offset, &add);
        store_leaf(&mut self.body.blocks, &pos.path, &flat);
        self.selection = BodySelection::caret(DocPos::new(pos.path, pos.offset.saturating_add(n)));
        self.dirty = true;
    }

    /// A Content-ID not used by this compose's inline images.
    pub fn next_content_id(&self) -> String {
        let first = self.inline_images.len().saturating_add(1);
        // Among len + 1 candidates at least one is free.
        for n in first..=first.saturating_add(self.inline_images.len()) {
            let cid = format!("image{n:03}.c{}@mailcraft.invalid", self.id);
            if !self.inline_images.iter().any(|i| i.content_id == cid) {
                return cid;
            }
        }
        format!("image.c{}.{}@mailcraft.invalid", self.id, self.inline_images.len())
    }

    /// Embeds an image (referenced as `cid:`) and inserts it at the caret.
    pub fn insert_image(&mut self, image: InlineImage, alt: &str, width: Option<f32>, height: Option<f32>) {
        let src = format!("cid:{}", image.content_id);
        self.inline_images.push(image);
        let frag = Doc {
            blocks: vec![Block::Paragraph {
                style: ParaStyle::default(),
                inlines: vec![Inline::Image { src, alt: alt.to_owned(), width, height }],
            }],
            ..Default::default()
        };
        self.insert_fragment(&frag, PasteMode::KeepSource);
    }

    /// Adds an entry to the in-app clipboard history.
    pub fn push_clipboard(&mut self, entry: ClipEntry) {
        self.clipboard.push(entry);
        if self.clipboard.len() > MAX_CLIPBOARD {
            self.clipboard.remove(0);
        }
    }

    /// Converts the selected paragraphs to a table (cells split at tabs, else commas).
    pub fn text_to_table(&mut self) -> bool {
        self.ensure_body();
        let start = self.selection.start().clone();
        let end = self.selection.end().clone();
        let Some((&i, prefix)) = start.path.split_last() else {
            return false;
        };
        let j = if end.path.len() == start.path.len() && end.path.starts_with(prefix) {
            end.path.get(prefix.len()).copied().unwrap_or(i).max(i)
        } else {
            i
        };
        let Some(seq) = seq_mut(&mut self.body.blocks, prefix) else {
            return false;
        };
        let Some(part) = seq.get(i..=j) else {
            return false;
        };
        if !part.iter().all(|b| matches!(b, Block::Paragraph { .. })) {
            return false;
        }
        let lines: Vec<String> = part.iter().map(|b| blocks_text(std::slice::from_ref(b))).collect();
        let sep = if lines.iter().any(|l| l.contains('\t')) { '\t' } else { ',' };
        let cols = lines.iter().map(|l| l.split(sep).count()).max().unwrap_or(1).max(1);
        let rows: Vec<Vec<TableCell>> = lines
            .iter()
            .map(|l| {
                let mut cells: Vec<TableCell> = l.split(sep).map(|c| table_cell(c.trim())).collect();
                while cells.len() < cols {
                    cells.push(table_cell(""));
                }
                cells
            })
            .collect();
        seq.splice(i..=j, std::iter::once(Block::Table { rows, border: true, width_px: None }));
        let mut target = prefix.to_vec();
        target.push(i);
        if let Some(p) = leaves(&self.body.blocks).into_iter().find(|p| p.starts_with(&target)) {
            self.selection = BodySelection::caret(DocPos::new(p, 0));
        }
        self.ensure_body();
        self.dirty = true;
        true
    }

    // ---------------------------------------------------------------------------------------------------
    // Find / replace / analysis
    // ---------------------------------------------------------------------------------------------------

    /// Selects the next match of `needle` after the selection (wrapping around). False when none.
    pub fn find_next(&mut self, needle: &str, match_case: bool) -> bool {
        self.ensure_body();
        if needle.is_empty() {
            return false;
        }
        let needle_n: Vec<char> = needle.chars().collect();
        let needle_s = fold(needle, match_case);
        let all = leaves(&self.body.blocks);
        let from = self.selection.end().clone();
        let k0 = all.iter().position(|p| *p == from.path).unwrap_or(0);
        let n = all.len();
        for step in 0..=n {
            let k = (k0.saturating_add(step)) % n.max(1);
            let Some(path) = all.get(k) else {
                continue;
            };
            let Some((_, flat)) = leaf(&self.body.blocks, path) else {
                continue;
            };
            let text = fold(&flat.text(), match_case);
            let start_at = if step == 0 { from.offset } else { 0 };
            if let Some(i) = find_chars(&text, &needle_s, start_at) {
                if step == n && i >= from.offset {
                    break;
                }
                self.selection = BodySelection {
                    anchor: DocPos::new(path.clone(), i),
                    focus: DocPos::new(path.clone(), i.saturating_add(needle_n.len())),
                };
                return true;
            }
        }
        false
    }

    /// Replaces every match; returns the count.
    pub fn replace_all(&mut self, needle: &str, with: &str, match_case: bool) -> usize {
        self.ensure_body();
        if needle.is_empty() {
            return 0;
        }
        let needle_len = needle.chars().count();
        let needle_s = fold(needle, match_case);
        let with = clean_text(with).replace('\n', " ");
        let with_len = with.chars().count();
        let mut count = 0usize;
        for path in leaves(&self.body.blocks) {
            let Some((kind, mut flat)) = leaf(&self.body.blocks, &path) else {
                continue;
            };
            if kind == LeafKind::Rule {
                continue;
            }
            let mut from = 0usize;
            let mut changed = false;
            while count < MAX_REPLACEMENTS {
                let text = fold(&flat.text(), match_case);
                let Some(i) = find_chars(&text, &needle_s, from) else {
                    break;
                };
                flat.replace_range(i, i.saturating_add(needle_len), &with);
                from = i.saturating_add(with_len);
                count = count.saturating_add(1);
                changed = true;
            }
            if changed {
                store_leaf(&mut self.body.blocks, &path, &flat);
            }
        }
        if count > 0 {
            self.dirty = true;
            self.ensure_body();
        }
        count
    }

    /// Replaces the selection when it matches `needle`, then finds the next match. Returns (replaced, found).
    pub fn replace_next(&mut self, needle: &str, with: &str, match_case: bool) -> (bool, bool) {
        self.ensure_body();
        let sel = self.selected_text();
        let matches = if match_case { sel == needle } else { fold(&sel, false) == fold(needle, false) };
        let mut replaced = false;
        if !needle.is_empty() && matches && !sel.is_empty() {
            let start = self.selection.start().clone();
            if let Some((_, mut flat)) = leaf(&self.body.blocks, &start.path) {
                let end = self.selection.end().offset;
                flat.replace_range(start.offset, end, &clean_text(with).replace('\n', " "));
                store_leaf(&mut self.body.blocks, &start.path, &flat);
                let after = start.offset.saturating_add(with.chars().count());
                self.selection = BodySelection::caret(DocPos::new(start.path, after));
                self.dirty = true;
                replaced = true;
            }
        }
        let found = self.find_next(needle, match_case);
        (replaced, found)
    }

    /// Select Similar Formatting: selects every run whose style equals the style at the selection start.
    /// Returns the number of ranges.
    pub fn select_similar(&mut self) -> usize {
        self.ensure_body();
        let target = {
            let pos = self.selection.start();
            leaf(&self.body.blocks, &pos.path)
                .and_then(|(_, f)| {
                    let i = pos.offset.min(f.len());
                    let a = f.atoms.get(i).or_else(|| i.checked_sub(1).and_then(|j| f.atoms.get(j)))?;
                    f.attrs.get(a.attr).map(|r| r.style.clone())
                })
                .unwrap_or_default()
        };
        let mut ranges = Vec::new();
        for path in leaves(&self.body.blocks) {
            let Some((_, flat)) = leaf(&self.body.blocks, &path) else {
                continue;
            };
            let mut run: Option<usize> = None;
            for (i, a) in flat.atoms.iter().enumerate() {
                let same = matches!(a.kind, AtomKind::Char(_))
                    && flat.attrs.get(a.attr).is_some_and(|r| r.style == target);
                match (same, run) {
                    (true, None) => run = Some(i),
                    (false, Some(s)) => {
                        ranges.push(BodySelection {
                            anchor: DocPos::new(path.clone(), s),
                            focus: DocPos::new(path.clone(), i),
                        });
                        run = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = run {
                ranges.push(BodySelection {
                    anchor: DocPos::new(path.clone(), s),
                    focus: DocPos::new(path.clone(), flat.len()),
                });
            }
        }
        let n = ranges.len();
        let mut it = ranges.into_iter();
        if let Some(first) = it.next() {
            self.selection = first;
            self.extra_ranges = it.collect();
            self.pending_style = None;
        }
        n
    }

    /// Word / character / paragraph / line counts for the selection, or the whole body when collapsed.
    pub fn word_count(&self) -> WordCount {
        let (a, b) = if self.selection.is_collapsed() {
            (self.body_start(), self.body_end())
        } else {
            (self.selection.start().clone(), self.selection.end().clone())
        };
        let mut wc = WordCount::default();
        for (path, from, to) in leaf_ranges(&self.body.blocks, &a, &b) {
            let Some((_, flat)) = leaf(&self.body.blocks, &path) else {
                continue;
            };
            let text: String = flat.slice(from, to).plain();
            if text.trim().is_empty() {
                continue;
            }
            wc.paragraphs = wc.paragraphs.saturating_add(1);
            wc.lines = wc.lines.saturating_add(text.split('\n').filter(|l| !l.trim().is_empty()).count());
            wc.words = wc.words.saturating_add(text.split_whitespace().count());
            wc.characters_no_spaces =
                wc.characters_no_spaces.saturating_add(text.chars().filter(|c| !c.is_whitespace()).count());
            wc.characters_with_spaces =
                wc.characters_with_spaces.saturating_add(text.chars().filter(|c| *c != '\n').count());
        }
        wc
    }

    /// The text and style of the first char of every leaf (used by drop caps).
    pub fn first_char_style_mut(&mut self, path: &[usize], f: impl FnOnce(&mut TextStyle)) -> bool {
        let Some((kind, mut flat)) = leaf(&self.body.blocks, path) else {
            return false;
        };
        if !matches!(kind, LeafKind::Para | LeafKind::Heading) {
            return false;
        }
        let Some(i) =
            flat.atoms.iter().position(|a| matches!(a.kind, AtomKind::Char(c) if !c.is_whitespace()))
        else {
            return false;
        };
        let mut f = Some(f);
        flat.restyle(i, i.saturating_add(1), &mut |r| {
            if let Some(f) = f.take() {
                f(&mut r.style);
            }
        });
        store_leaf(&mut self.body.blocks, path, &flat);
        self.dirty = true;
        true
    }

    /// Image `src` values in the body (for inline-image bookkeeping and accessibility checks), with alt text.
    pub fn images(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for path in leaves(&self.body.blocks) {
            if let Some((_, flat)) = leaf(&self.body.blocks, &path) {
                for a in &flat.atoms {
                    if let AtomKind::Image { src, alt, .. } = &a.kind {
                        out.push((src.clone(), alt.clone()));
                    }
                }
            }
        }
        out
    }

    /// Every explicit text colour used in the body.
    pub fn text_colors(&self) -> Vec<Color> {
        let mut out: Vec<Color> = Vec::new();
        for path in leaves(&self.body.blocks) {
            if let Some((_, flat)) = leaf(&self.body.blocks, &path) {
                for r in &flat.attrs {
                    if let Some(c) = r.style.color
                        && !out.contains(&c)
                    {
                        out.push(c);
                    }
                }
            }
        }
        out
    }

    /// Leaf paths in document order.
    pub fn leaf_paths(&self) -> Vec<Vec<usize>> {
        leaves(&self.body.blocks)
    }

    /// Runs `f` with the selection widened to whole paragraphs (paragraph styles that set character
    /// formatting), then restores the selection.
    pub fn with_whole_paragraphs(&mut self, f: impl FnOnce(&mut ComposeState)) {
        self.ensure_body();
        let saved = self.selection.clone();
        let extra = std::mem::take(&mut self.extra_ranges);
        let start = DocPos::new(self.selection.start().path.clone(), 0);
        let end_path = self.selection.end().path.clone();
        let len = leaf_len(&self.body.blocks, &end_path);
        self.selection = BodySelection { anchor: start, focus: DocPos::new(end_path, len) };
        f(self);
        self.selection = saved;
        self.extra_ranges = extra;
        self.ensure_body();
    }

    /// Runs `f` over the whole body (every paragraph selected), then restores the selection.
    pub fn with_whole_body(&mut self, f: impl FnOnce(&mut ComposeState)) {
        self.ensure_body();
        let saved = self.selection.clone();
        let extra = std::mem::take(&mut self.extra_ranges);
        self.selection = BodySelection { anchor: self.body_start(), focus: self.body_end() };
        f(self);
        self.selection = saved;
        self.extra_ranges = extra;
        self.ensure_body();
    }

    /// Start of each table's first cell, in document order (Go To › Table).
    pub fn table_starts(&self) -> Vec<DocPos> {
        let mut seen: Vec<Vec<usize>> = Vec::new();
        let mut out = Vec::new();
        for path in leaves(&self.body.blocks) {
            if let Some((Owner::Cell { table, .. }, _)) = parent_of(&self.body.blocks, &path)
                && !seen.contains(&table)
            {
                seen.push(table);
                out.push(DocPos::new(path, 0));
            }
        }
        out
    }

    /// Adds (`on`) or removes a horizontal rule directly above the first / below the last selected
    /// paragraph (paragraph top / bottom borders). Returns whether the body changed.
    pub fn set_para_rule(&mut self, above: bool, on: bool) -> bool {
        self.ensure_body();
        let ords = self.selection_ords();
        let path =
            if above { self.selection.start().path.clone() } else { self.selection.end().path.clone() };
        let Some((&idx, prefix)) = path.split_last() else {
            return false;
        };
        let Some(seq) = seq_mut(&mut self.body.blocks, prefix) else {
            return false;
        };
        let neighbour = if above { idx.checked_sub(1) } else { Some(idx.saturating_add(1)) };
        let has_rule = neighbour.and_then(|n| seq.get(n)).is_some_and(|b| matches!(b, Block::Rule));
        let changed = match (on, has_rule, neighbour) {
            (true, false, _) => {
                let at = if above { idx } else { idx.saturating_add(1) }.min(seq.len());
                seq.insert(at, Block::Rule);
                true
            }
            (false, true, Some(n)) if n < seq.len() => {
                seq.remove(n);
                true
            }
            _ => false,
        };
        if changed {
            let shift = |(k, off): (usize, usize)| {
                if !above {
                    (k, off)
                } else if on {
                    (k.saturating_add(1), off)
                } else {
                    (k.saturating_sub(1), off)
                }
            };
            self.restore_ords((shift(ords.0), shift(ords.1)));
            self.dirty = true;
        }
        changed
    }

    fn selection_ords(&self) -> ((usize, usize), (usize, usize)) {
        let all = leaves(&self.body.blocks);
        let ord = |p: &DocPos| (all.iter().position(|x| *x == p.path).unwrap_or(0), p.offset);
        (ord(&self.selection.anchor), ord(&self.selection.focus))
    }

    fn restore_ords(&mut self, ords: ((usize, usize), (usize, usize))) {
        let all = leaves(&self.body.blocks);
        let pos = |(k, off): (usize, usize)| {
            let path = all.get(k).or_else(|| all.last()).cloned().unwrap_or_default();
            DocPos::new(path, off)
        };
        self.selection = BodySelection { anchor: pos(ords.0), focus: pos(ords.1) };
        self.ensure_body();
    }

    fn restore_caret_ord(&mut self, ord: usize, off: usize) {
        self.restore_ords(((ord, off), (ord, off)));
    }
}

// =======================================================================================================
// Compose depth (commit 6): paste merging, attachments, table navigation, spelling cache, signatures
// =======================================================================================================

impl ComposeState {
    /// Records that the body changed (the spelling cache goes stale).
    pub fn mark_edited(&mut self) {
        self.edit_rev = self.edit_rev.wrapping_add(1);
    }

    // ---------------------------------------------------------------------------------------------------
    // Paste
    // ---------------------------------------------------------------------------------------------------

    /// Pastes plain text at the caret, replacing the selection (one editor undo step).
    pub fn paste_plain(&mut self, text: &str) {
        self.ensure_body();
        self.checkpoint();
        self.insert_text(text);
    }

    /// Pastes HTML at the caret, replacing the selection: sanitised through `mailcraft_html::html_to_doc`;
    /// a plain-text body gets the text only (one editor undo step).
    pub fn paste_html(&mut self, html: &str) {
        let doc = mailcraft_html::html_to_doc(html);
        self.ensure_body();
        self.checkpoint();
        let mode =
            if self.format == BodyFormat::PlainText { PasteMode::TextOnly } else { PasteMode::KeepSource };
        self.insert_fragment(&doc, mode);
    }

    /// Multi-block paste the way word processors do it: the first pasted paragraph joins the text before the
    /// caret, the last one joins the text after it, the blocks in between go between them. False (nothing
    /// changed) when the shapes do not allow it (the caller then inserts the blocks whole).
    fn merge_paste(&mut self, blocks: &[Block]) -> bool {
        let (Some(first), Some(last)) = (blocks.first(), blocks.last()) else {
            return false;
        };
        let text_block = |b: &Block| matches!(b, Block::Paragraph { .. } | Block::Heading { .. });
        if blocks.len() < 2 || !text_block(first) || !text_block(last) {
            return false;
        }
        self.delete_selection();
        let pos = self.selection.focus.clone();
        if !matches!(leaf(&self.body.blocks, &pos.path), Some((LeafKind::Para | LeafKind::Heading, _))) {
            return false;
        }
        let Some((&idx, prefix)) = pos.path.split_last() else {
            return false;
        };
        let prefix = prefix.to_vec();
        if seq_mut(&mut self.body.blocks, &prefix).is_none() {
            return false;
        }
        let inlines_of = |b: &Block| match b {
            Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => inlines.clone(),
            _ => Vec::new(),
        };
        // Split at the caret: the left part stays at `pos.path`, the right part becomes its next sibling.
        split_at(&mut self.body, &pos, false);
        if let Some((_, mut flat)) = leaf(&self.body.blocks, &pos.path) {
            let add = Flat::from_inlines(&inlines_of(first));
            let n = flat.len();
            flat.insert_flat(n, &add);
            store_leaf(&mut self.body.blocks, &pos.path, &flat);
        }
        let middle: Vec<Block> =
            blocks.get(1..blocks.len().saturating_sub(1)).map(<[Block]>::to_vec).unwrap_or_default();
        let m = middle.len();
        if let Some(seq) = seq_mut(&mut self.body.blocks, &prefix) {
            let at = idx.saturating_add(1).min(seq.len());
            seq.splice(at..at, middle);
        }
        let mut right = prefix;
        right.push(idx.saturating_add(1).saturating_add(m));
        let add = Flat::from_inlines(&inlines_of(last));
        let n = add.len();
        if let Some((_, mut flat)) = leaf(&self.body.blocks, &right) {
            flat.insert_flat(0, &add);
            store_leaf(&mut self.body.blocks, &right, &flat);
        }
        self.selection = BodySelection::caret(normalize(&self.body, &DocPos::new(right, n)));
        self.extra_ranges.clear();
        self.dirty = true;
        self.mark_edited();
        true
    }

    // ---------------------------------------------------------------------------------------------------
    // Attachments
    // ---------------------------------------------------------------------------------------------------

    /// Adds an attachment (name cleaned, MIME type given or guessed from the extension); returns its index.
    pub fn attach_bytes(&mut self, name: &str, mime: Option<&str>, data: Vec<u8>) -> Result<usize, String> {
        if self.attachments.len() >= MAX_COMPOSE_ATTACHMENTS {
            return Err("too many attachments".into());
        }
        let filename = clean_attachment_name(name, "attachment");
        let mime_type = mime
            .map(str::trim)
            .filter(|m| valid_mime_type(m))
            .map(str::to_ascii_lowercase)
            .unwrap_or_else(|| mime_for_name(&filename).to_owned());
        self.attachments.push(ComposeAttachment { filename, mime_type, data });
        self.dirty = true;
        Ok(self.attachments.len().saturating_sub(1))
    }

    /// Attaches a stored message as an item attachment (`message/rfc822`, "<subject>.eml").
    pub fn attach_message(&mut self, m: &mailcraft_model::MailItem) -> Result<usize, String> {
        // Slashes in a subject are not paths here.
        let subject: String =
            m.subject.trim().chars().map(|c| if matches!(c, '/' | '\\') { '_' } else { c }).collect();
        let name = clean_attachment_name(&format!("{subject}.eml"), "message.eml");
        let name = if name.eq_ignore_ascii_case(".eml") { "message.eml".to_owned() } else { name };
        self.attach_bytes(&name, Some("message/rfc822"), crate::cmd::compose::message_source(m))
    }

    /// Renames attachment `index`; the MIME type follows a changed extension when it is a known one.
    pub fn rename_attachment(&mut self, index: usize, new_name: &str) -> Result<String, String> {
        if new_name.trim().is_empty() {
            return Err("the new name is empty".into());
        }
        let name = clean_attachment_name(new_name, "attachment");
        let Some(a) = self.attachments.get_mut(index) else {
            return Err("no such attachment".into());
        };
        let guessed = mime_for_name(&name);
        if guessed != "application/octet-stream" {
            a.mime_type = guessed.to_owned();
        }
        a.filename = name.clone();
        self.dirty = true;
        Ok(name)
    }

    /// Removes attachment `index`.
    pub fn remove_attachment(&mut self, index: usize) -> Option<ComposeAttachment> {
        if index >= self.attachments.len() {
            return None;
        }
        self.dirty = true;
        Some(self.attachments.remove(index))
    }

    /// Total attachment bytes.
    pub fn attachments_size(&self) -> u64 {
        self.attachments
            .iter()
            .fold(0u64, |n, a| n.saturating_add(u64::try_from(a.data.len()).unwrap_or(u64::MAX)))
    }

    // ---------------------------------------------------------------------------------------------------
    // Tables
    // ---------------------------------------------------------------------------------------------------

    /// The innermost table cell holding the caret: (table path, row, column).
    pub fn cell_at_caret(&self) -> Option<(Vec<usize>, usize, usize)> {
        cell_of(&self.body.blocks, &self.selection.focus.path)
    }

    /// Moves to another cell of the table holding the caret and selects its content (Tab / Shift+Tab /
    /// row up/down). Next in the last cell adds a row first. False when the caret is not in a table or there
    /// is no cell in that direction.
    pub fn move_to_cell(&mut self, dir: CellMove) -> bool {
        self.ensure_body();
        let Some((table, row, col)) = self.cell_at_caret() else {
            return false;
        };
        let widths: Vec<usize> = match block_at(&self.body.blocks, &table) {
            Some(Block::Table { rows, .. }) => rows.iter().map(Vec::len).collect(),
            _ => return false,
        };
        let width = |r: usize| widths.get(r).copied().unwrap_or(0);
        let last_col = |r: usize| width(r).saturating_sub(1);
        let n = widths.len();
        let target = match dir {
            CellMove::Next if col.saturating_add(1) < width(row) => Some((row, col.saturating_add(1))),
            CellMove::Next => (row.saturating_add(1)..n).find(|r| width(*r) > 0).map(|r| (r, 0)),
            CellMove::Previous if col > 0 => Some((row, col.saturating_sub(1).min(last_col(row)))),
            CellMove::Previous => match (0..row).rev().find(|r| width(*r) > 0) {
                Some(r) => Some((r, last_col(r))),
                None => return false,
            },
            CellMove::Up => match (0..row).rev().find(|r| width(*r) > 0) {
                Some(r) => Some((r, col.min(last_col(r)))),
                None => return false,
            },
            CellMove::Down => match (row.saturating_add(1)..n).find(|r| width(*r) > 0) {
                Some(r) => Some((r, col.min(last_col(r)))),
                None => return false,
            },
        };
        let (r, c) = match target {
            Some(t) => t,
            None => {
                // Tab in the last cell adds a row like the current one.
                if n >= MAX_TABLE_ROWS_EDIT {
                    return false;
                }
                self.checkpoint();
                if let Some(Block::Table { rows, .. }) = block_at_mut(&mut self.body.blocks, &table) {
                    rows.push((0..width(row).max(1)).map(|_| table_cell("")).collect());
                }
                self.dirty = true;
                (n, 0)
            }
        };
        self.select_cell(&table, r, c)
    }

    /// Selects the content of cell (`r`, `c`) of the table at `table` (caret in it when empty).
    fn select_cell(&mut self, table: &[usize], r: usize, c: usize) -> bool {
        if let Some(Block::Table { rows, .. }) = block_at_mut(&mut self.body.blocks, table)
            && let Some(cell) = rows.get_mut(r).and_then(|row| row.get_mut(c))
            && leaves(&cell.blocks).is_empty()
        {
            cell.blocks.push(Block::text_paragraph(""));
        }
        let mut prefix = table.to_vec();
        prefix.push(r);
        prefix.push(c);
        let inside: Vec<Vec<usize>> = leaves(&self.body.blocks)
            .into_iter()
            .filter(|p| p.len() > prefix.len() && p.starts_with(&prefix))
            .collect();
        let (Some(first), Some(last)) = (inside.first(), inside.last()) else {
            return false;
        };
        let len = leaf_len(&self.body.blocks, last);
        self.selection =
            BodySelection { anchor: DocPos::new(first.clone(), 0), focus: DocPos::new(last.clone(), len) };
        self.pending_style = None;
        self.extra_ranges.clear();
        true
    }

    /// Row / column / table edits on the table holding the caret (one editor undo step). False when the
    /// caret is not in a table or the table is at its size limit.
    pub fn table_edit(&mut self, op: TableEdit) -> bool {
        self.ensure_body();
        let Some((table, row, col)) = self.cell_at_caret() else {
            return false;
        };
        let (nrows, width, max_width) = match block_at(&self.body.blocks, &table) {
            Some(Block::Table { rows, .. }) => (
                rows.len(),
                rows.get(row).map_or(1, Vec::len).max(1),
                rows.iter().map(Vec::len).max().unwrap_or(0),
            ),
            _ => return false,
        };
        let grows_rows = matches!(op, TableEdit::InsertRowAbove | TableEdit::InsertRowBelow);
        let grows_cols = matches!(op, TableEdit::InsertColumnLeft | TableEdit::InsertColumnRight);
        if (grows_rows && nrows >= MAX_TABLE_ROWS_EDIT) || (grows_cols && max_width >= MAX_TABLE_COLS_EDIT) {
            return false;
        }
        self.checkpoint();
        let mut target: Option<(usize, usize)> = None;
        let mut remove_table = false;
        if let Some(Block::Table { rows, .. }) = block_at_mut(&mut self.body.blocks, &table) {
            let new_row = |w: usize| -> Vec<TableCell> { (0..w).map(|_| table_cell("")).collect() };
            match op {
                TableEdit::InsertRowAbove => {
                    rows.insert(row.min(rows.len()), new_row(width));
                    target = Some((row, col));
                }
                TableEdit::InsertRowBelow => {
                    let at = row.saturating_add(1).min(rows.len());
                    rows.insert(at, new_row(width));
                    target = Some((at, col));
                }
                TableEdit::InsertColumnLeft | TableEdit::InsertColumnRight => {
                    let at = if op == TableEdit::InsertColumnLeft { col } else { col.saturating_add(1) };
                    for r in rows.iter_mut() {
                        let i = at.min(r.len());
                        r.insert(i, table_cell(""));
                    }
                    target = Some((row, at));
                }
                TableEdit::DeleteRow => {
                    if row < rows.len() {
                        rows.remove(row);
                    }
                    if rows.is_empty() {
                        remove_table = true;
                    } else {
                        target = Some((row.min(rows.len().saturating_sub(1)), col));
                    }
                }
                TableEdit::DeleteColumn => {
                    for r in rows.iter_mut() {
                        if col < r.len() {
                            r.remove(col);
                        }
                    }
                    rows.retain(|r| !r.is_empty());
                    if rows.is_empty() {
                        remove_table = true;
                    } else {
                        target = Some((row.min(rows.len().saturating_sub(1)), col.saturating_sub(1)));
                    }
                }
                TableEdit::DeleteTable => remove_table = true,
            }
        }
        if remove_table {
            remove_block(&mut self.body.blocks, &table);
            ensure_leaf(&mut self.body);
            let pos = normalize(&self.body, &DocPos::new(table, 0));
            self.selection = BodySelection::caret(pos);
        } else if let Some((r, c)) = target {
            let width = match block_at(&self.body.blocks, &table) {
                Some(Block::Table { rows, .. }) => rows.get(r).map_or(0, Vec::len),
                _ => 0,
            };
            if self.select_cell(&table, r, c.min(width.saturating_sub(1))) {
                let start = self.selection.start().clone();
                self.selection = BodySelection::caret(start);
            }
        }
        self.extra_ranges.clear();
        self.dirty = true;
        self.ensure_body();
        true
    }

    // ---------------------------------------------------------------------------------------------------
    // Spelling
    // ---------------------------------------------------------------------------------------------------

    /// Whether the body changed since the last spelling check.
    pub fn spelling_stale(&self) -> bool {
        self.spell_rev != Some(self.edit_rev)
    }

    /// First top-level block of the quoted original in a reply/forward (proofing skips it, Outlook's
    /// "Ignore original message text in reply or forward").
    pub fn quoted_start(&self) -> Option<usize> {
        if !matches!(self.kind, ComposeKind::Reply | ComposeKind::ReplyAll | ComposeKind::Forward) {
            return None;
        }
        let blocks = &self.body.blocks;
        blocks.iter().enumerate().skip(1).find_map(|(i, b)| {
            let is_header = match b {
                // The HTML header: a rule, then "From: …" (a rule the user drew above it is their text).
                Block::Rule => blocks.get(i.saturating_add(1)).is_some_and(|n| {
                    matches!(n, Block::Paragraph { .. })
                        && blocks_text(std::slice::from_ref(n)).trim_start().starts_with("From:")
                }),
                Block::Paragraph { .. } => {
                    blocks_text(std::slice::from_ref(b)).trim() == "-----Original Message-----"
                }
                _ => false,
            };
            is_header.then_some(i)
        })
    }

    /// Re-checks the body when it changed since the last check (or with `force`): `check` gets the text of
    /// one paragraph (one char per position; a line break is '\n', a picture U+FFFC) and returns issues as
    /// char ranges. Linked and monospace (code) text, the quoted original of a reply/forward, ignored words
    /// and ignored occurrences are skipped. Returns whether it re-checked.
    pub fn refresh_spelling(&mut self, force: bool, check: &mut dyn FnMut(&str) -> Vec<TextIssue>) -> bool {
        if !force && !self.spelling_stale() {
            return false;
        }
        let stop = self.quoted_start().map(|i| vec![i]);
        let mut out: Vec<SpellIssue> = Vec::new();
        'leaves: for path in leaves(&self.body.blocks) {
            if stop.as_ref().is_some_and(|s| path >= *s) {
                break;
            }
            let Some((kind, flat)) = leaf(&self.body.blocks, &path) else {
                continue;
            };
            if matches!(kind, LeafKind::Rule | LeafKind::Pre) {
                continue;
            }
            let text = flat.text();
            for ti in check(&text) {
                if ti.end <= ti.start || ti.end > flat.len() {
                    continue;
                }
                let skip = (ti.start..ti.end).any(|i| {
                    flat.atoms.get(i).and_then(|a| flat.attrs.get(a.attr)).is_some_and(|r| {
                        r.link.is_some()
                            || r.style.monospace == Some(true)
                            || r.style
                                .font_family
                                .as_deref()
                                .is_some_and(|f| CODE_FONTS.iter().any(|c| f.eq_ignore_ascii_case(c)))
                    })
                });
                if skip {
                    continue;
                }
                let word: String =
                    text.chars().skip(ti.start).take(ti.end.saturating_sub(ti.start)).collect();
                let start = DocPos::new(path.clone(), ti.start);
                if self.ignored_words.iter().any(|w| w.eq_ignore_ascii_case(&word))
                    || self.ignored_once.iter().any(|(p, w)| *p == start && *w == word)
                {
                    continue;
                }
                out.push(SpellIssue {
                    start,
                    end: DocPos::new(path.clone(), ti.end),
                    word,
                    kind: if ti.kind.is_empty() { "spelling".into() } else { ti.kind },
                    suggestions: ti.suggestions,
                });
                if out.len() >= MAX_SPELL_ISSUES {
                    break 'leaves;
                }
            }
        }
        self.spell_issues = out;
        self.spell_rev = Some(self.edit_rev);
        true
    }

    /// The cached issue at `pos` (inside or at either end of its range).
    pub fn spell_issue_at(&self, pos: &DocPos) -> Option<&SpellIssue> {
        self.spell_issues
            .iter()
            .find(|i| i.start.path == pos.path && i.start.offset <= pos.offset && pos.offset <= i.end.offset)
    }

    /// The first cached issue after the selection, wrapping to the first one (F7 / Next).
    pub fn next_spell_issue(&self) -> Option<&SpellIssue> {
        let from = self.selection.end();
        self.spell_issues.iter().find(|i| i.start >= *from).or_else(|| self.spell_issues.first())
    }

    /// Ignore (`all == false`: the occurrence at `at` or under the selection; `all`: every occurrence of
    /// `word` in this message). Returns the number of cached issues dropped.
    pub fn ignore_spelling(&mut self, word: Option<&str>, all: bool, at: Option<&DocPos>) -> usize {
        let pos = at.cloned().unwrap_or_else(|| self.selection.start().clone());
        let issue = self.spell_issue_at(&pos).cloned();
        let word = word.map(str::to_owned).or_else(|| issue.as_ref().map(|i| i.word.clone()));
        // Grammar issues on spaces ("More than one space") have a whitespace-only word; only "" is refused.
        let Some(word) = word.filter(|w| !w.is_empty()) else {
            return 0;
        };
        let before = self.spell_issues.len();
        if all {
            if !self.ignored_words.iter().any(|w| w.eq_ignore_ascii_case(&word))
                && self.ignored_words.len() < 10_000
            {
                self.ignored_words.push(word.clone());
            }
            self.spell_issues.retain(|i| !i.word.eq_ignore_ascii_case(&word));
        } else {
            let Some(issue) = issue.filter(|i| i.word == word) else {
                return 0;
            };
            if self.ignored_once.len() < 10_000 {
                self.ignored_once.push((issue.start.clone(), issue.word.clone()));
            }
            self.spell_issues.retain(|i| i.start != issue.start);
        }
        before.saturating_sub(self.spell_issues.len())
    }

    /// Drops cached issues for a word the dictionary now knows.
    pub fn forget_spelling_word(&mut self, word: &str) -> usize {
        let before = self.spell_issues.len();
        self.spell_issues.retain(|i| !i.word.eq_ignore_ascii_case(word));
        before.saturating_sub(self.spell_issues.len())
    }

    /// Replaces the issue at `at` (or under the selection) with `replacement`; with `all`, every cached issue
    /// for the same word (Change All). Issues whose text no longer matches are skipped. Returns the count.
    pub fn correct_spelling(&mut self, replacement: &str, at: Option<&DocPos>, all: bool) -> usize {
        self.ensure_body();
        let pos = at.cloned().unwrap_or_else(|| self.selection.start().clone());
        let Some(issue) = self.spell_issue_at(&pos).cloned() else {
            return 0;
        };
        let mut targets: Vec<SpellIssue> = if all {
            self.spell_issues.iter().filter(|i| i.word.eq_ignore_ascii_case(&issue.word)).cloned().collect()
        } else {
            vec![issue]
        };
        // Issues whose text no longer matches (edited since the check) are left alone.
        targets.retain(|t| range_text(&self.body, &t.start, &t.end) == t.word);
        if targets.is_empty() {
            return 0;
        }
        // Back to front so earlier positions stay valid.
        targets.sort_by(|a, b| b.start.cmp(&a.start));
        let replacement = clean_text(replacement).replace('\n', " ");
        self.checkpoint();
        let mut n = 0usize;
        for t in &targets {
            self.selection = BodySelection { anchor: t.start.clone(), focus: t.end.clone() };
            self.pending_style = None;
            self.insert_text(&replacement);
            n = n.saturating_add(1);
        }
        let done: Vec<DocPos> = targets.iter().map(|t| t.start.clone()).collect();
        self.spell_issues.retain(|i| !done.contains(&i.start));
        n
    }

    // ---------------------------------------------------------------------------------------------------
    // Recipients
    // ---------------------------------------------------------------------------------------------------

    /// (display name, address) of every valid recipient in To, Cc and Bcc, in field order.
    pub fn recipients(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for field in [&self.to, &self.cc, &self.bcc] {
            for piece in crate::cmd::compose::split_recipients(field) {
                if let Ok(list) = crate::cmd::compose::parse_recipients(piece) {
                    for a in list {
                        for m in a.mailboxes() {
                            out.push((m.name.clone().unwrap_or_default(), m.addr.clone()));
                        }
                    }
                }
            }
        }
        out
    }
}

/// Font families treated as code (not spell-checked).
const CODE_FONTS: [&str; 6] =
    ["Consolas", "Courier New", "Courier", "monospace", "Liberation Mono", "Cascadia Code"];
/// Most rows / columns a table grows to through Tab and Insert Row/Column.
pub const MAX_TABLE_ROWS_EDIT: usize = 1000;
pub const MAX_TABLE_COLS_EDIT: usize = 63;

/// The innermost table cell holding the block at `path`: (table path, row, column).
fn cell_of(blocks: &[Block], path: &[usize]) -> Option<(Vec<usize>, usize, usize)> {
    let mut p = path.to_vec();
    for _ in 0..MAX_WALK_DEPTH {
        match parent_of(blocks, &p)? {
            (Owner::Cell { table, row, col }, _) => return Some((table, row, col)),
            (Owner::ListItem { list, .. }, _) => p = list,
            (Owner::Box(b), _) => p = b,
            (Owner::Root, _) => return None,
        }
    }
    None
}

/// An attachment file name made safe: path separators and control chars replaced, trimmed, capped.
pub fn clean_attachment_name(name: &str, fallback: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base
        .chars()
        .map(|c| if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    let clean = clean.trim().trim_end_matches('.').trim();
    if clean.is_empty() {
        return fallback.to_owned();
    }
    let n = clean.chars().count();
    if n <= MAX_ATTACHMENT_NAME_CHARS {
        return clean.to_owned();
    }
    // Keep the extension when shortening.
    let ext: String =
        clean.rsplit_once('.').map(|(_, e)| e).filter(|e| e.chars().count() <= 16).unwrap_or("").to_owned();
    let keep = MAX_ATTACHMENT_NAME_CHARS.saturating_sub(ext.chars().count().saturating_add(1));
    let stem: String = clean.chars().take(keep).collect();
    if ext.is_empty() { stem } else { format!("{stem}.{ext}") }
}

/// Whether `m` looks like `type/subtype` (RFC 2045 tokens).
fn valid_mime_type(m: &str) -> bool {
    let token = |s: &str| {
        !s.is_empty()
            && s.len() <= 127
            && s.chars().all(|c| c.is_ascii_alphanumeric() || "!#$&-^_.+".contains(c))
    };
    m.split_once('/').is_some_and(|(t, s)| token(t) && token(s))
}

/// MIME type for a file name, by extension (case-insensitive); `application/octet-stream` when unknown.
pub fn mime_for_name(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    MIME_TABLE.iter().find(|(e, _)| *e == ext).map_or("application/octet-stream", |(_, m)| m)
}

/// Extension → MIME type (IANA media types registry names).
const MIME_TABLE: &[(&str, &str)] = &[
    // text
    ("txt", "text/plain"),
    ("text", "text/plain"),
    ("log", "text/plain"),
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("htm", "text/html"),
    ("html", "text/html"),
    ("css", "text/css"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("ics", "text/calendar"),
    ("ical", "text/calendar"),
    ("vcf", "text/vcard"),
    ("vcard", "text/vcard"),
    ("rtf", "application/rtf"),
    ("xml", "application/xml"),
    ("json", "application/json"),
    ("yaml", "application/yaml"),
    ("yml", "application/yaml"),
    ("toml", "application/toml"),
    ("js", "text/javascript"),
    ("mjs", "text/javascript"),
    ("ts", "text/plain"),
    ("py", "text/x-python"),
    ("rs", "text/plain"),
    ("c", "text/plain"),
    ("h", "text/plain"),
    ("cpp", "text/plain"),
    ("java", "text/plain"),
    ("sh", "application/x-sh"),
    ("sql", "application/sql"),
    // images
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("jpe", "image/jpeg"),
    ("gif", "image/gif"),
    ("bmp", "image/bmp"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("ico", "image/x-icon"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("heic", "image/heic"),
    ("heif", "image/heif"),
    ("avif", "image/avif"),
    ("emf", "image/emf"),
    ("wmf", "image/wmf"),
    // documents
    ("pdf", "application/pdf"),
    ("doc", "application/msword"),
    ("dot", "application/msword"),
    ("docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
    ("dotx", "application/vnd.openxmlformats-officedocument.wordprocessingml.template"),
    ("xls", "application/vnd.ms-excel"),
    ("xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
    ("xlsm", "application/vnd.ms-excel.sheet.macroenabled.12"),
    ("ppt", "application/vnd.ms-powerpoint"),
    ("pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
    ("epub", "application/epub+zip"),
    ("msg", "application/vnd.ms-outlook"),
    ("eml", "message/rfc822"),
    ("mht", "message/rfc822"),
    ("mhtml", "message/rfc822"),
    // archives
    ("zip", "application/zip"),
    ("gz", "application/gzip"),
    ("tgz", "application/gzip"),
    ("tar", "application/x-tar"),
    ("bz2", "application/x-bzip2"),
    ("xz", "application/x-xz"),
    ("zst", "application/zstd"),
    ("7z", "application/x-7z-compressed"),
    ("rar", "application/vnd.rar"),
    // audio / video
    ("mp3", "audio/mpeg"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("oga", "audio/ogg"),
    ("opus", "audio/opus"),
    ("flac", "audio/flac"),
    ("mid", "audio/midi"),
    ("midi", "audio/midi"),
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("mov", "video/quicktime"),
    ("avi", "video/x-msvideo"),
    ("mkv", "video/x-matroska"),
    ("webm", "video/webm"),
    ("ogv", "video/ogg"),
    ("mpeg", "video/mpeg"),
    ("mpg", "video/mpeg"),
    // fonts, 3D, keys
    ("ttf", "font/ttf"),
    ("otf", "font/otf"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("obj", "model/obj"),
    ("stl", "model/stl"),
    ("glb", "model/gltf-binary"),
    ("gltf", "model/gltf+json"),
    ("3mf", "model/3mf"),
    ("asc", "application/pgp-keys"),
    ("pgp", "application/pgp-encrypted"),
    ("p7s", "application/pkcs7-signature"),
    ("p7m", "application/pkcs7-mime"),
    ("cer", "application/pkix-cert"),
    ("crt", "application/x-x509-ca-cert"),
];

// ---------------------------------------------------------------------------------------------------------
// Default signatures
// ---------------------------------------------------------------------------------------------------------

/// Signature blocks for a body format: a plain-text body gets the text only.
pub fn signature_blocks(body: &Doc, format: BodyFormat) -> Vec<Block> {
    if format == BodyFormat::PlainText {
        Doc::from_plain_text(&body.to_plain_text()).blocks
    } else {
        body.blocks.clone()
    }
}

/// The account a compose sends from: the one whose address is in its From field, else the default account.
pub fn compose_account(mb: &mailcraft_model::Mailbox, from: &str) -> Option<mailcraft_model::AccountId> {
    let addr = crate::cmd::compose::parse_field_mailbox(from).map(|m| m.addr);
    addr.and_then(|a| {
        mb.accounts.iter().find(|x| !x.address.trim().is_empty() && x.address.trim().eq_ignore_ascii_case(&a))
    })
    .or_else(|| mb.default_account())
    .map(|a| a.id)
}

/// Inserts the sending account's default signature into a freshly opened compose (Options › Mail ›
/// Signatures: one for new messages, one for replies/forwards). New message: after an empty first
/// paragraph. Reply / forward: between the caret paragraph and the quoted header. The caret goes to the
/// start of the body. Does nothing (false) for drafts, when the compose already has a signature or no
/// default applies.
pub fn insert_default_signature(mb: &mailcraft_model::Mailbox, c: &mut ComposeState) -> bool {
    if c.signature.is_some() {
        return false;
    }
    let reply = match c.kind {
        ComposeKind::New => false,
        ComposeKind::Reply | ComposeKind::ReplyAll | ComposeKind::Forward => true,
        ComposeKind::Draft => return false,
    };
    let account = compose_account(mb, &c.from);
    let Some(sig) = mb.default_signature(account, reply) else {
        return false;
    };
    let name = sig.name.clone();
    let blocks = signature_blocks(&sig.body, c.format);
    if blocks.is_empty() {
        return false;
    }
    let container = Block::Container { style: ParaStyle::default(), blocks: blocks.clone() };
    let empty_para = |b: &Block| match b {
        Block::Paragraph { inlines, .. } => {
            inlines.iter().all(|i| matches!(i, Inline::Text { text, .. } if text.is_empty()))
        }
        _ => false,
    };
    let body = &mut c.body.blocks;
    if !reply {
        if body.iter().all(empty_para) {
            *body = vec![Block::text_paragraph(""), container];
        } else {
            body.push(Block::text_paragraph(""));
            body.push(container);
        }
    } else {
        let lead = body.iter().take_while(|b| empty_para(b)).count();
        match lead {
            0 => {
                body.splice(0..0, [Block::text_paragraph(""), container, Block::text_paragraph("")]);
            }
            1 => {
                body.splice(1..1, [container, Block::text_paragraph("")]);
            }
            _ => body.insert(1, container),
        }
    }
    c.signature = Some(SignatureBlock { name, blocks });
    ensure_leaf(&mut c.body);
    c.selection = BodySelection::caret(normalize(&c.body, &DocPos::new(vec![0], 0)));
    c.extra_ranges.clear();
    true
}

/// [`insert_default_signature`] by value (for builders that return a fresh [`ComposeState`]).
pub fn with_default_signature(mb: &mailcraft_model::Mailbox, mut c: ComposeState) -> ComposeState {
    insert_default_signature(mb, &mut c);
    c
}

// =======================================================================================================
// Flattened inline content
// =======================================================================================================

#[derive(Clone, Debug, Default, PartialEq)]
struct RunAttr {
    style: TextStyle,
    link: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum AtomKind {
    Char(char),
    Break,
    Image { src: String, alt: String, width: Option<f32>, height: Option<f32> },
}

#[derive(Clone, Debug, PartialEq)]
struct Atom {
    kind: AtomKind,
    attr: usize,
}

/// A leaf block's inline content as one atom per char (styles interned).
#[derive(Clone, Debug, Default, PartialEq)]
struct Flat {
    attrs: Vec<RunAttr>,
    atoms: Vec<Atom>,
}

impl Flat {
    fn from_inlines(inlines: &[Inline]) -> Flat {
        let mut f = Flat::default();
        f.push_inlines(inlines, None, 0);
        f
    }

    fn from_text(text: &str) -> Flat {
        let mut f = Flat::default();
        let a = f.intern(RunAttr::default());
        f.atoms.extend(
            text.chars()
                .map(|c| Atom { kind: if c == '\n' { AtomKind::Break } else { AtomKind::Char(c) }, attr: a }),
        );
        f
    }

    fn push_inlines(&mut self, inlines: &[Inline], link: Option<&str>, depth: usize) {
        if depth > MAX_INLINE_DEPTH {
            return;
        }
        for inline in inlines {
            match inline {
                Inline::Text { text, style } => {
                    let a = self.intern(RunAttr { style: style.clone(), link: link.map(str::to_owned) });
                    self.atoms.extend(text.chars().map(|c| Atom { kind: AtomKind::Char(c), attr: a }));
                }
                Inline::Link { href, children } => {
                    self.push_inlines(children, Some(href), depth.saturating_add(1))
                }
                Inline::LineBreak => {
                    let a =
                        self.intern(RunAttr { style: TextStyle::default(), link: link.map(str::to_owned) });
                    self.atoms.push(Atom { kind: AtomKind::Break, attr: a });
                }
                Inline::Image { src, alt, width, height } => {
                    let a =
                        self.intern(RunAttr { style: TextStyle::default(), link: link.map(str::to_owned) });
                    self.atoms.push(Atom {
                        kind: AtomKind::Image {
                            src: src.clone(),
                            alt: alt.clone(),
                            width: *width,
                            height: *height,
                        },
                        attr: a,
                    });
                }
            }
        }
    }

    fn intern(&mut self, a: RunAttr) -> usize {
        if let Some(i) = self.attrs.iter().position(|x| *x == a) {
            return i;
        }
        self.attrs.push(a);
        self.attrs.len().saturating_sub(1)
    }

    fn len(&self) -> usize {
        self.atoms.len()
    }

    fn link_at(&self, i: usize) -> Option<&str> {
        self.atoms.get(i).and_then(|a| self.attrs.get(a.attr)).and_then(|r| r.link.as_deref())
    }

    fn to_inlines(&self) -> Vec<Inline> {
        let mut out = Vec::new();
        let n = self.atoms.len();
        let mut i = 0usize;
        while i < n {
            let link = self.link_at(i);
            let mut group: Vec<Inline> = Vec::new();
            while i < n && self.link_at(i) == link {
                let Some(atom) = self.atoms.get(i) else {
                    break;
                };
                match &atom.kind {
                    AtomKind::Char(_) => {
                        let attr = atom.attr;
                        let mut s = String::new();
                        while let Some(Atom { kind: AtomKind::Char(c), attr: a2 }) = self.atoms.get(i) {
                            if *a2 != attr {
                                break;
                            }
                            s.push(*c);
                            i = i.saturating_add(1);
                        }
                        let style = self.attrs.get(attr).map(|r| r.style.clone()).unwrap_or_default();
                        group.push(Inline::Text { text: s, style });
                    }
                    AtomKind::Break => {
                        group.push(Inline::LineBreak);
                        i = i.saturating_add(1);
                    }
                    AtomKind::Image { src, alt, width, height } => {
                        group.push(Inline::Image {
                            src: src.clone(),
                            alt: alt.clone(),
                            width: *width,
                            height: *height,
                        });
                        i = i.saturating_add(1);
                    }
                }
            }
            match link {
                Some(h) => out.push(Inline::Link { href: h.to_owned(), children: group }),
                None => out.extend(group),
            }
        }
        out
    }

    /// Text with one char per atom (break = '\n', image = U+FFFC).
    fn text(&self) -> String {
        self.atoms
            .iter()
            .map(|a| match &a.kind {
                AtomKind::Char(c) => *c,
                AtomKind::Break => '\n',
                AtomKind::Image { .. } => OBJECT_CHAR,
            })
            .collect()
    }

    /// Plain text (images become their alt text).
    fn plain(&self) -> String {
        let mut s = String::new();
        for a in &self.atoms {
            match &a.kind {
                AtomKind::Char(c) => s.push(*c),
                AtomKind::Break => s.push('\n'),
                AtomKind::Image { alt, .. } => s.push_str(alt),
            }
        }
        s
    }

    fn slice(&self, a: usize, b: usize) -> Flat {
        let b = b.min(self.len());
        let a = a.min(b);
        Flat {
            attrs: self.attrs.clone(),
            atoms: self.atoms.get(a..b).map(<[Atom]>::to_vec).unwrap_or_default(),
        }
    }

    fn split_off(&mut self, at: usize) -> Flat {
        let at = at.min(self.len());
        Flat { attrs: self.attrs.clone(), atoms: self.atoms.split_off(at) }
    }

    /// Inserts `other` at `at`; returns the number of atoms inserted.
    fn insert_flat(&mut self, at: usize, other: &Flat) -> usize {
        let at = at.min(self.len());
        let mut map: Vec<Option<usize>> = vec![None; other.attrs.len()];
        let mut add = Vec::with_capacity(other.atoms.len());
        for atom in &other.atoms {
            let idx = match map.get(atom.attr).copied().flatten() {
                Some(i) => i,
                None => {
                    let i = self.intern(other.attrs.get(atom.attr).cloned().unwrap_or_default());
                    if let Some(slot) = map.get_mut(atom.attr) {
                        *slot = Some(i);
                    }
                    i
                }
            };
            add.push(Atom { kind: atom.kind.clone(), attr: idx });
        }
        let n = add.len();
        self.atoms.splice(at..at, add);
        n
    }

    fn remove(&mut self, a: usize, b: usize) {
        let b = b.min(self.len());
        let a = a.min(b);
        self.atoms.drain(a..b);
    }

    /// Replaces `a..b` with `text`, styled like the first replaced char.
    fn replace_range(&mut self, a: usize, b: usize, text: &str) {
        let b = b.min(self.len());
        let a = a.min(b);
        let existing =
            self.atoms.get(a).or_else(|| a.checked_sub(1).and_then(|j| self.atoms.get(j))).map(|x| x.attr);
        let attr = match existing {
            Some(x) => x,
            None => self.intern(RunAttr::default()),
        };
        let add: Vec<Atom> = text.chars().map(|c| Atom { kind: AtomKind::Char(c), attr }).collect();
        self.atoms.splice(a..b, add);
    }

    fn restyle(&mut self, a: usize, b: usize, f: &mut dyn FnMut(&mut RunAttr)) {
        let b = b.min(self.len());
        let a = a.min(b);
        let mut atoms = std::mem::take(&mut self.atoms);
        let mut cache: Vec<Option<usize>> = vec![None; self.attrs.len()];
        for atom in atoms.get_mut(a..b).unwrap_or_default() {
            if let Some(Some(n)) = cache.get(atom.attr) {
                atom.attr = *n;
                continue;
            }
            let mut ra = self.attrs.get(atom.attr).cloned().unwrap_or_default();
            f(&mut ra);
            let n = self.intern(ra);
            if let Some(slot) = cache.get_mut(atom.attr) {
                *slot = Some(n);
            }
            atom.attr = n;
        }
        self.atoms = atoms;
    }

    /// Extent of the run of atoms around `i` that share link `href`.
    fn link_extent(&self, i: usize, href: &str) -> (usize, usize) {
        let mut s = i.min(self.len());
        while s > 0 && self.link_at(s.saturating_sub(1)) == Some(href) {
            s = s.saturating_sub(1);
        }
        let mut e = i.min(self.len());
        while e < self.len() && self.link_at(e) == Some(href) {
            e = e.saturating_add(1);
        }
        (s, e)
    }

    fn change_case(&mut self, a: usize, b: usize, kind: CaseKind) {
        let b = b.min(self.len());
        let a = a.min(b);
        let prev_char = |atoms: &[Atom], i: usize| -> Option<char> {
            i.checked_sub(1).and_then(|j| atoms.get(j)).and_then(|x| match x.kind {
                AtomKind::Char(c) => Some(c),
                AtomKind::Break => Some('\n'),
                AtomKind::Image { .. } => None,
            })
        };
        // Sentence start: nothing but whitespace back to the block start or a terminator.
        let mut cap_next = {
            let mut j = a;
            loop {
                match prev_char(&self.atoms, j) {
                    None => break true,
                    Some(c) if c.is_whitespace() => j = j.saturating_sub(1),
                    Some(c) => break matches!(c, '.' | '!' | '?'),
                }
            }
        };
        let mut prev_word = prev_char(&self.atoms, a).is_some_and(|c| c.is_alphanumeric() || c == '\'');
        for atom in self.atoms.get_mut(a..b).unwrap_or_default() {
            let AtomKind::Char(c) = atom.kind else {
                prev_word = false;
                continue;
            };
            let n = match kind {
                CaseKind::Upper => upper(c),
                CaseKind::Lower => lower(c),
                CaseKind::Toggle => {
                    if c.is_uppercase() {
                        lower(c)
                    } else {
                        upper(c)
                    }
                }
                CaseKind::Title => {
                    if prev_word {
                        lower(c)
                    } else {
                        upper(c)
                    }
                }
                CaseKind::Sentence => {
                    if c.is_alphabetic() {
                        let r = if cap_next { upper(c) } else { lower(c) };
                        cap_next = false;
                        r
                    } else {
                        if matches!(c, '.' | '!' | '?') {
                            cap_next = true;
                        }
                        c
                    }
                }
            };
            prev_word = c.is_alphanumeric() || c == '\'';
            atom.kind = AtomKind::Char(n);
        }
    }
}

fn upper(c: char) -> char {
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

fn lower(c: char) -> char {
    let mut u = c.to_lowercase();
    match (u.next(), u.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// Case folding that keeps one char per char.
fn fold(s: &str, match_case: bool) -> String {
    if match_case { s.to_owned() } else { s.chars().map(lower).collect() }
}

/// Char index of the first match of `needle` in `hay` at or after char `from`.
fn find_chars(hay: &str, needle: &str, from: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let byte_from = hay.char_indices().nth(from).map(|(b, _)| b)?;
    let tail = hay.get(byte_from..)?;
    let rel = tail.find(needle)?;
    let head = tail.get(..rel)?;
    Some(from.saturating_add(head.chars().count()))
}

/// Normalises typed/pasted text: CRLF/CR → LF, other control chars (except tab) dropped, length capped.
fn clean_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .take(MAX_INSERT_CHARS)
        .collect()
}

// =======================================================================================================
// Block tree navigation
// =======================================================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeafKind {
    Para,
    Heading,
    Pre,
    Rule,
}

/// What owns a block sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Owner {
    Root,
    ListItem {
        list: Vec<usize>,
        item: usize,
    },
    /// Quote or container at this path.
    Box(Vec<usize>),
    Cell {
        table: Vec<usize>,
        row: usize,
        col: usize,
    },
}

fn block_at<'a>(blocks: &'a [Block], path: &[usize]) -> Option<&'a Block> {
    let (&first, rest) = path.split_first()?;
    let b = blocks.get(first)?;
    if rest.is_empty() {
        return Some(b);
    }
    match b {
        Block::List { items, .. } => {
            let (&it, rest) = rest.split_first()?;
            block_at(items.get(it)?, rest)
        }
        Block::Quote { blocks } | Block::Container { blocks, .. } => block_at(blocks, rest),
        Block::Table { rows, .. } => {
            let (&r, rest) = rest.split_first()?;
            let (&c, rest) = rest.split_first()?;
            block_at(&rows.get(r)?.get(c)?.blocks, rest)
        }
        _ => None,
    }
}

fn block_at_mut<'a>(blocks: &'a mut [Block], path: &[usize]) -> Option<&'a mut Block> {
    let (&first, rest) = path.split_first()?;
    let b = blocks.get_mut(first)?;
    if rest.is_empty() {
        return Some(b);
    }
    match b {
        Block::List { items, .. } => {
            let (&it, rest) = rest.split_first()?;
            block_at_mut(items.get_mut(it)?, rest)
        }
        Block::Quote { blocks } | Block::Container { blocks, .. } => block_at_mut(blocks, rest),
        Block::Table { rows, .. } => {
            let (&r, rest) = rest.split_first()?;
            let (&c, rest) = rest.split_first()?;
            block_at_mut(&mut rows.get_mut(r)?.get_mut(c)?.blocks, rest)
        }
        _ => None,
    }
}

/// The block sequence identified by `prefix` (the path of a child minus its last index).
fn seq_mut<'a>(blocks: &'a mut Vec<Block>, prefix: &[usize]) -> Option<&'a mut Vec<Block>> {
    let Some((&first, rest)) = prefix.split_first() else {
        return Some(blocks);
    };
    match blocks.get_mut(first)? {
        Block::List { items, .. } => {
            let (&it, rest) = rest.split_first()?;
            seq_mut(items.get_mut(it)?, rest)
        }
        Block::Quote { blocks } | Block::Container { blocks, .. } => seq_mut(blocks, rest),
        Block::Table { rows, .. } => {
            let (&r, rest) = rest.split_first()?;
            let (&c, rest) = rest.split_first()?;
            seq_mut(&mut rows.get_mut(r)?.get_mut(c)?.blocks, rest)
        }
        _ => None,
    }
}

/// Owner of the sequence at `prefix`.
fn seq_owner(blocks: &[Block], prefix: &[usize]) -> Option<Owner> {
    let mut cur: &[Block] = blocks;
    let mut owner = Owner::Root;
    let mut i = 0usize;
    while i < prefix.len() {
        let idx = *prefix.get(i)?;
        let here = prefix.get(..=i)?.to_vec();
        match cur.get(idx)? {
            Block::List { items, .. } => {
                let it = *prefix.get(i.saturating_add(1))?;
                cur = items.get(it)?;
                owner = Owner::ListItem { list: here, item: it };
                i = i.saturating_add(2);
            }
            Block::Quote { blocks } | Block::Container { blocks, .. } => {
                cur = blocks;
                owner = Owner::Box(here);
                i = i.saturating_add(1);
            }
            Block::Table { rows, .. } => {
                let r = *prefix.get(i.saturating_add(1))?;
                let c = *prefix.get(i.saturating_add(2))?;
                cur = &rows.get(r)?.get(c)?.blocks;
                owner = Owner::Cell { table: here, row: r, col: c };
                i = i.saturating_add(3);
            }
            _ => return None,
        }
    }
    Some(owner)
}

/// Owner of the sequence holding the block at `path`, and the block's index in it.
fn parent_of(blocks: &[Block], path: &[usize]) -> Option<(Owner, usize)> {
    let (&last, prefix) = path.split_last()?;
    Some((seq_owner(blocks, prefix)?, last))
}

/// Number of lists enclosing the block at `path`.
fn list_depth(blocks: &[Block], path: &[usize]) -> usize {
    let mut depth = 0usize;
    let mut cur: &[Block] = blocks;
    let mut i = 0usize;
    while i < path.len() {
        let Some(b) = path.get(i).and_then(|&idx| cur.get(idx)) else {
            break;
        };
        match b {
            Block::List { items, .. } => {
                let Some(item) = path.get(i.saturating_add(1)).and_then(|&it| items.get(it)) else {
                    break;
                };
                depth = depth.saturating_add(1);
                cur = item;
                i = i.saturating_add(2);
            }
            Block::Quote { blocks } | Block::Container { blocks, .. } => {
                cur = blocks;
                i = i.saturating_add(1);
            }
            Block::Table { rows, .. } => {
                let cell = path
                    .get(i.saturating_add(1))
                    .and_then(|&r| rows.get(r))
                    .and_then(|row| path.get(i.saturating_add(2)).and_then(|&c| row.get(c)));
                let Some(cell) = cell else {
                    break;
                };
                cur = &cell.blocks;
                i = i.saturating_add(3);
            }
            _ => break,
        }
    }
    depth
}

/// Innermost list holding the leaf at `path`.
fn list_of(blocks: &[Block], path: &[usize]) -> Option<(Vec<usize>, ListKind)> {
    let mut p = path.to_vec();
    while !p.is_empty() {
        p.pop();
        if let Some(Block::List { kind, .. }) = block_at(blocks, &p) {
            return Some((p, kind.clone()));
        }
    }
    None
}

fn is_leaf(b: &Block) -> bool {
    matches!(b, Block::Paragraph { .. } | Block::Heading { .. } | Block::Pre { .. } | Block::Rule)
}

/// Leaf paths in document order.
fn leaves(blocks: &[Block]) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut path = Vec::new();
    walk_leaves(blocks, &mut path, &mut out, 0);
    out
}

fn walk_leaves(blocks: &[Block], path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>, depth: usize) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    for (i, b) in blocks.iter().enumerate() {
        path.push(i);
        match b {
            _ if is_leaf(b) => out.push(path.clone()),
            Block::List { items, .. } => {
                for (k, item) in items.iter().enumerate() {
                    path.push(k);
                    walk_leaves(item, path, out, depth.saturating_add(1));
                    path.pop();
                }
            }
            Block::Quote { blocks } | Block::Container { blocks, .. } => {
                walk_leaves(blocks, path, out, depth.saturating_add(1));
            }
            Block::Table { rows, .. } => {
                for (r, row) in rows.iter().enumerate() {
                    for (c, cell) in row.iter().enumerate() {
                        path.push(r);
                        path.push(c);
                        walk_leaves(&cell.blocks, path, out, depth.saturating_add(1));
                        path.pop();
                        path.pop();
                    }
                }
            }
            _ => {}
        }
        path.pop();
    }
}

fn leaf_ordinal(blocks: &[Block], path: &[usize]) -> usize {
    leaves(blocks).iter().position(|p| p == path).unwrap_or(0)
}

fn leaf(blocks: &[Block], path: &[usize]) -> Option<(LeafKind, Flat)> {
    match block_at(blocks, path)? {
        Block::Paragraph { inlines, .. } => Some((LeafKind::Para, Flat::from_inlines(inlines))),
        Block::Heading { inlines, .. } => Some((LeafKind::Heading, Flat::from_inlines(inlines))),
        Block::Pre { text } => Some((LeafKind::Pre, Flat::from_text(text))),
        Block::Rule => Some((LeafKind::Rule, Flat::default())),
        _ => None,
    }
}

fn store_leaf(blocks: &mut [Block], path: &[usize], flat: &Flat) {
    match block_at_mut(blocks, path) {
        Some(Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. }) => {
            *inlines = flat.to_inlines()
        }
        Some(Block::Pre { text }) => *text = flat.plain(),
        _ => {}
    }
}

fn leaf_len(blocks: &[Block], path: &[usize]) -> usize {
    leaf(blocks, path).map_or(0, |(_, f)| f.len())
}

/// Adds an empty paragraph when the body has no leaf.
fn ensure_leaf(doc: &mut Doc) {
    if leaves(&doc.blocks).is_empty() {
        doc.blocks.push(Block::text_paragraph(""));
    }
}

/// Snaps `pos` to a leaf (the first leaf at or after it, else the last) and clamps the offset.
fn normalize(doc: &Doc, pos: &DocPos) -> DocPos {
    let all = leaves(&doc.blocks);
    if all.contains(&pos.path) {
        let len = leaf_len(&doc.blocks, &pos.path);
        return DocPos::new(pos.path.clone(), pos.offset.min(len));
    }
    if let Some(p) = all.iter().find(|p| **p > pos.path) {
        return DocPos::new(p.clone(), 0);
    }
    match all.last() {
        Some(p) => DocPos::new(p.clone(), leaf_len(&doc.blocks, p)),
        None => DocPos::new(vec![0], 0),
    }
}

/// `(leaf, from, to)` for every leaf between `a` and `b` (document order).
fn leaf_ranges(blocks: &[Block], a: &DocPos, b: &DocPos) -> Vec<(Vec<usize>, usize, usize)> {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    let all = leaves(blocks);
    let Some(si) = all.iter().position(|p| *p == a.path) else {
        return Vec::new();
    };
    let ei = all.iter().position(|p| *p == b.path).unwrap_or(si).max(si);
    let mut out = Vec::new();
    for (k, path) in all.iter().enumerate().take(ei.saturating_add(1)).skip(si) {
        let len = leaf_len(blocks, path);
        let from = if k == si { a.offset.min(len) } else { 0 };
        let to = if k == ei { b.offset.min(len) } else { len };
        out.push((path.clone(), from, to.max(from)));
    }
    out
}

fn range_text(doc: &Doc, a: &DocPos, b: &DocPos) -> String {
    let mut parts = Vec::new();
    for (path, from, to) in leaf_ranges(&doc.blocks, a, b) {
        if let Some((_, flat)) = leaf(&doc.blocks, &path) {
            parts.push(flat.slice(from, to).plain());
        }
    }
    parts.join("\n")
}

fn blocks_text(blocks: &[Block]) -> String {
    Doc { blocks: blocks.to_vec(), ..Default::default() }.to_plain_text()
}

fn sort_cmp(a: &str, b: &str, descending: bool) -> std::cmp::Ordering {
    let ord = match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
        (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => a.to_lowercase().cmp(&b.to_lowercase()),
    };
    if descending { ord.reverse() } else { ord }
}

fn same_list_kind(a: &ListKind, b: &ListKind) -> bool {
    matches!(
        (a, b),
        (ListKind::Bullet, ListKind::Bullet) | (ListKind::Numbered { .. }, ListKind::Numbered { .. })
    )
}

/// One table cell holding a paragraph with `text`.
pub fn table_cell(text: &str) -> TableCell {
    TableCell {
        blocks: vec![Block::text_paragraph(text)],
        colspan: 1,
        rowspan: 1,
        background: None,
        width_px: None,
    }
}

/// An empty bordered table.
pub fn empty_table(rows: usize, cols: usize) -> Block {
    let rows = (0..rows.max(1)).map(|_| (0..cols.max(1)).map(|_| table_cell("")).collect()).collect();
    Block::Table { rows, border: true, width_px: None }
}

// =======================================================================================================
// Structural edits
// =======================================================================================================

/// Removes the block at `path`, then prunes containers that became empty (an emptied list item and list,
/// quote or container disappear; an emptied cell or body gets an empty paragraph).
fn remove_block(blocks: &mut Vec<Block>, path: &[usize]) {
    let Some((&last, prefix)) = path.split_last() else {
        return;
    };
    let owner = seq_owner(blocks, prefix);
    let Some(seq) = seq_mut(blocks, prefix) else {
        return;
    };
    if last >= seq.len() {
        return;
    }
    seq.remove(last);
    if !seq.is_empty() {
        return;
    }
    match owner {
        Some(Owner::Root) | Some(Owner::Cell { .. }) => seq.push(Block::text_paragraph("")),
        Some(Owner::Box(p)) => remove_block(blocks, &p),
        Some(Owner::ListItem { list, item }) => {
            let empty = match block_at_mut(blocks, &list) {
                Some(Block::List { items, .. }) => {
                    if item < items.len() {
                        items.remove(item);
                    }
                    items.is_empty()
                }
                _ => false,
            };
            if empty {
                remove_block(blocks, &list);
            }
        }
        None => {}
    }
}

/// Inserts `text` (no newlines) at `pos`; returns the caret after it.
fn insert_chars(doc: &mut Doc, pos: &DocPos, text: &str, style: Option<&TextStyle>) -> DocPos {
    let Some((kind, mut flat)) = leaf(&doc.blocks, &pos.path) else {
        return pos.clone();
    };
    if kind == LeafKind::Rule {
        // Type into a new paragraph after the rule.
        let Some((&idx, prefix)) = pos.path.split_last() else {
            return pos.clone();
        };
        let Some(seq) = seq_mut(&mut doc.blocks, prefix) else {
            return pos.clone();
        };
        let at = idx.saturating_add(1).min(seq.len());
        seq.insert(at, Block::text_paragraph(""));
        let mut p = prefix.to_vec();
        p.push(at);
        return insert_chars(doc, &DocPos::new(p, 0), text, style);
    }
    let off = pos.offset.min(flat.len());
    let attr = if kind == LeafKind::Pre {
        RunAttr::default()
    } else {
        let prev = off.checked_sub(1).and_then(|j| flat.atoms.get(j)).and_then(|a| flat.attrs.get(a.attr));
        let next = flat.atoms.get(off).and_then(|a| flat.attrs.get(a.attr));
        let base = prev.or(next).cloned().unwrap_or_default();
        let inside_link = match (prev.and_then(|r| r.link.as_ref()), next.and_then(|r| r.link.as_ref())) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        RunAttr {
            style: style.cloned().unwrap_or(base.style),
            link: if inside_link { base.link } else { None },
        }
    };
    let a = flat.intern(attr);
    let add: Vec<Atom> = text.chars().map(|c| Atom { kind: AtomKind::Char(c), attr: a }).collect();
    let n = add.len();
    flat.atoms.splice(off..off, add);
    store_leaf(&mut doc.blocks, &pos.path, &flat);
    DocPos::new(pos.path.clone(), off.saturating_add(n))
}

/// Splits the leaf at `pos` (Enter). Returns the start of the new block. `list_aware`: inside a list item
/// the split starts a new item, and Enter in an empty single-paragraph item leaves the list.
fn split_at(doc: &mut Doc, pos: &DocPos, list_aware: bool) -> DocPos {
    let Some((kind, mut flat)) = leaf(&doc.blocks, &pos.path) else {
        return pos.clone();
    };
    let Some((&idx, prefix)) = pos.path.split_last() else {
        return pos.clone();
    };
    let prefix = prefix.to_vec();
    let off = pos.offset.min(flat.len());
    if kind == LeafKind::Pre {
        let a = flat.intern(RunAttr::default());
        flat.atoms.insert(off, Atom { kind: AtomKind::Break, attr: a });
        store_leaf(&mut doc.blocks, &pos.path, &flat);
        return DocPos::new(pos.path.clone(), off.saturating_add(1));
    }
    let owner = parent_of(&doc.blocks, &pos.path).map(|(o, _)| o);
    if list_aware
        && flat.len() == 0
        && let Some(Owner::ListItem { list, item }) = &owner
        && let Some(Block::List { items, .. }) = block_at(&doc.blocks, list)
        && items.get(*item).is_some_and(|it| it.len() == 1)
    {
        // A nested item moves up a level (a new item of the outer list); a top-level one leaves the list.
        let ord = leaf_ordinal(&doc.blocks, &pos.path);
        if !promote(&mut doc.blocks, &pos.path) {
            unwrap_items(&mut doc.blocks, list, *item, *item);
        }
        let p = leaves(&doc.blocks).get(ord).cloned().unwrap_or_default();
        return DocPos::new(p, 0);
    }
    let right = flat.split_off(off);
    let new_block = match block_at(&doc.blocks, &pos.path) {
        Some(Block::Heading { level, style, .. }) if right.len() > 0 => {
            Block::Heading { level: *level, style: style.clone(), inlines: right.to_inlines() }
        }
        Some(Block::Paragraph { style, .. }) => {
            Block::Paragraph { style: style.clone(), inlines: right.to_inlines() }
        }
        Some(Block::Rule) => Block::text_paragraph(""),
        _ => Block::Paragraph { style: ParaStyle::default(), inlines: right.to_inlines() },
    };
    store_leaf(&mut doc.blocks, &pos.path, &flat);
    if list_aware && let Some(Owner::ListItem { list, item }) = owner {
        if let Some(Block::List { items, .. }) = block_at_mut(&mut doc.blocks, &list)
            && let Some(cur) = items.get_mut(item)
        {
            let tail_from = idx.saturating_add(1).min(cur.len());
            let mut new_item = vec![new_block];
            new_item.extend(cur.drain(tail_from..));
            let at = item.saturating_add(1).min(items.len());
            items.insert(at, new_item);
            let mut p = list;
            p.push(at);
            p.push(0);
            return DocPos::new(p, 0);
        }
        return pos.clone();
    }
    let Some(seq) = seq_mut(&mut doc.blocks, &prefix) else {
        return pos.clone();
    };
    let at = idx.saturating_add(1).min(seq.len());
    seq.insert(at, new_block);
    let mut p = prefix;
    p.push(at);
    DocPos::new(p, 0)
}

/// Splits the preformatted block at `pos` into two preformatted blocks.
fn split_pre(doc: &mut Doc, pos: &DocPos) {
    let Some((LeafKind::Pre, mut flat)) = leaf(&doc.blocks, &pos.path) else {
        return;
    };
    let Some((&idx, prefix)) = pos.path.split_last() else {
        return;
    };
    let mut right = flat.split_off(pos.offset.min(flat.len()));
    // The line break at the split point goes with the split.
    if right.atoms.first().is_some_and(|a| a.kind == AtomKind::Break) {
        right.atoms.remove(0);
    }
    store_leaf(&mut doc.blocks, &pos.path, &flat);
    if let Some(seq) = seq_mut(&mut doc.blocks, prefix) {
        let at = idx.saturating_add(1).min(seq.len());
        seq.insert(at, Block::Pre { text: right.plain() });
    }
}

/// Deletes `start..end` (ordered, normalised). Returns the caret (`start`).
fn delete_range(doc: &mut Doc, start: &DocPos, end: &DocPos) -> DocPos {
    if start >= end {
        return start.clone();
    }
    if start.path == end.path {
        if let Some((_, mut flat)) = leaf(&doc.blocks, &start.path) {
            flat.remove(start.offset, end.offset);
            store_leaf(&mut doc.blocks, &start.path, &flat);
        }
        return start.clone();
    }
    if cell_of(&doc.blocks, &start.path) != cell_of(&doc.blocks, &end.path) {
        return delete_across_cells(doc, start, end);
    }
    let tail = leaf(&doc.blocks, &end.path).map(|(_, mut f)| f.split_off(end.offset)).unwrap_or_default();
    // Remove every block whose leaves all lie in (start, end], largest first.
    let mut covered = Vec::new();
    collect_covered(&doc.blocks, &mut Vec::new(), &start.path, &end.path, &mut covered, 0);
    for p in covered.iter().rev() {
        remove_block(&mut doc.blocks, p);
    }
    match leaf(&doc.blocks, &start.path) {
        Some((LeafKind::Rule, _)) => {
            if let Some(b) = block_at_mut(&mut doc.blocks, &start.path) {
                *b = Block::Paragraph { style: ParaStyle::default(), inlines: tail.to_inlines() };
            }
            DocPos::new(start.path.clone(), 0)
        }
        Some((_, mut flat)) => {
            flat.atoms.truncate(start.offset.min(flat.len()));
            flat.insert_flat(flat.len(), &tail);
            store_leaf(&mut doc.blocks, &start.path, &flat);
            start.clone()
        }
        None => start.clone(),
    }
}

/// [`delete_range`] when `start` and `end` lie in different table cells (or one in a cell, one outside):
/// nothing merges across the cell boundary. The start leaf keeps its text before `start`, the end leaf its
/// text after `end`, and everything between goes (cells in between are emptied, their table stays).
fn delete_across_cells(doc: &mut Doc, start: &DocPos, end: &DocPos) -> DocPos {
    let all = leaves(&doc.blocks);
    let Some(k) = all.iter().position(|p| *p == end.path) else {
        return start.clone();
    };
    let before_end = k.checked_sub(1).and_then(|j| all.get(j)).cloned().unwrap_or_else(|| start.path.clone());
    // The end leaf first: removing blocks before it may shift its path.
    if let Some((_, mut flat)) = leaf(&doc.blocks, &end.path) {
        flat.remove(0, end.offset.min(flat.len()));
        store_leaf(&mut doc.blocks, &end.path, &flat);
    }
    let mut covered = Vec::new();
    if before_end > start.path {
        collect_covered(&doc.blocks, &mut Vec::new(), &start.path, &before_end, &mut covered, 0);
    }
    for p in covered.iter().rev() {
        remove_block(&mut doc.blocks, p);
    }
    match leaf(&doc.blocks, &start.path) {
        Some((LeafKind::Rule, _)) => {
            if let Some(b) = block_at_mut(&mut doc.blocks, &start.path) {
                *b = Block::text_paragraph("");
            }
            DocPos::new(start.path.clone(), 0)
        }
        Some((_, mut flat)) => {
            flat.atoms.truncate(start.offset.min(flat.len()));
            store_leaf(&mut doc.blocks, &start.path, &flat);
            start.clone()
        }
        None => start.clone(),
    }
}

/// Collects (document order) the maximal blocks whose leaves all lie in `(lo, hi]`.
fn collect_covered(
    blocks: &[Block],
    prefix: &mut Vec<usize>,
    lo: &[usize],
    hi: &[usize],
    out: &mut Vec<Vec<usize>>,
    depth: usize,
) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    for (i, b) in blocks.iter().enumerate() {
        prefix.push(i);
        let mut own = Vec::new();
        if is_leaf(b) {
            own.push(prefix.clone());
        } else {
            walk_leaves(std::slice::from_ref(b), &mut Vec::new(), &mut own, depth);
            for p in &mut own {
                // walk_leaves on a one-block slice yields paths starting with 0; rebase onto `prefix`.
                let mut full = prefix.clone();
                full.extend(p.iter().skip(1));
                *p = full;
            }
        }
        if let (Some(first), Some(last)) = (own.first(), own.last()) {
            if first.as_slice() > lo && last.as_slice() <= hi {
                out.push(prefix.clone());
            } else if last.as_slice() > lo && first.as_slice() <= hi && !is_leaf(b) {
                match b {
                    Block::List { items, .. } => {
                        for (k, item) in items.iter().enumerate() {
                            prefix.push(k);
                            collect_covered(item, prefix, lo, hi, out, depth.saturating_add(1));
                            prefix.pop();
                        }
                    }
                    Block::Quote { blocks } | Block::Container { blocks, .. } => {
                        collect_covered(blocks, prefix, lo, hi, out, depth.saturating_add(1));
                    }
                    Block::Table { rows, .. } => {
                        for (r, row) in rows.iter().enumerate() {
                            for (c, cell) in row.iter().enumerate() {
                                prefix.push(r);
                                prefix.push(c);
                                collect_covered(&cell.blocks, prefix, lo, hi, out, depth.saturating_add(1));
                                prefix.pop();
                                prefix.pop();
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        prefix.pop();
    }
}

/// Removes list formatting from items `a..=b` of the list at `list`: the list splits around them and their
/// blocks join the enclosing sequence.
fn unwrap_items(blocks: &mut Vec<Block>, list: &[usize], a: usize, b: usize) {
    let Some((&li, prefix)) = list.split_last() else {
        return;
    };
    let Some(seq) = seq_mut(blocks, prefix) else {
        return;
    };
    let Some(Block::List { kind, items }) = seq.get_mut(li) else {
        return;
    };
    if items.is_empty() {
        return;
    }
    let kind = kind.clone();
    let b = b.min(items.len().saturating_sub(1));
    let a = a.min(b);
    let mut all = std::mem::take(items);
    let after = all.split_off(b.saturating_add(1));
    let mid = all.split_off(a);
    let before = all;
    let mut repl = Vec::new();
    if !before.is_empty() {
        repl.push(Block::List { kind: kind.clone(), items: before });
    }
    let moved = mid.len();
    for item in mid {
        repl.extend(item);
    }
    if !after.is_empty() {
        let kind = match kind {
            ListKind::Numbered { start } => ListKind::Numbered {
                start: start.saturating_add(u32::try_from(a.saturating_add(moved)).unwrap_or(u32::MAX)),
            },
            k => k,
        };
        repl.push(Block::List { kind, items: after });
    }
    if repl.is_empty() {
        repl.push(Block::text_paragraph(""));
    }
    seq.splice(li..=li, repl);
}

/// Wraps blocks `i..=j` of the sequence at `prefix` into one list (existing lists in the range merge in).
fn wrap_in_list(blocks: &mut Vec<Block>, prefix: &[usize], i: usize, j: usize, kind: ListKind) {
    let Some(seq) = seq_mut(blocks, prefix) else {
        return;
    };
    if i >= seq.len() {
        return;
    }
    let j = j.min(seq.len().saturating_sub(1)).max(i);
    let taken: Vec<Block> = seq.drain(i..=j).collect();
    let mut items = Vec::new();
    for b in taken {
        match b {
            Block::List { items: inner, .. } => items.extend(inner),
            other => items.push(vec![other]),
        }
    }
    seq.insert(i, Block::List { kind, items });
}

/// Moves the list item holding the leaf at `path` one level deeper (into the previous item). False when it
/// is not in a list or is the first item.
fn demote(blocks: &mut [Block], path: &[usize]) -> bool {
    let Some((Owner::ListItem { list, item }, _)) = parent_of(blocks, path) else {
        return false;
    };
    if item == 0 || list_depth(blocks, path) >= MAX_LIST_LEVEL {
        return false;
    }
    let Some(Block::List { kind, items }) = block_at_mut(blocks, &list) else {
        return false;
    };
    if item >= items.len() {
        return false;
    }
    let kind = kind.clone();
    let moving = items.remove(item);
    let Some(prev) = items.get_mut(item.saturating_sub(1)) else {
        return false;
    };
    if let Some(Block::List { kind: k2, items: sub }) = prev.last_mut()
        && same_list_kind(k2, &kind)
    {
        sub.push(moving);
    } else {
        prev.push(Block::List { kind, items: vec![moving] });
    }
    true
}

/// Moves the list item holding the leaf at `path` one level up (items after it nest under it); at the top
/// level the item leaves the list.
fn promote(blocks: &mut Vec<Block>, path: &[usize]) -> bool {
    let Some((Owner::ListItem { list, item }, _)) = parent_of(blocks, path) else {
        return false;
    };
    let Some((outer, li)) = parent_of(blocks, &list) else {
        return false;
    };
    let Owner::ListItem { list: outer_list, item: outer_item } = outer else {
        unwrap_items(blocks, &list, item, item);
        return true;
    };
    let Some(Block::List { kind, items }) = block_at_mut(blocks, &list) else {
        return false;
    };
    if item >= items.len() {
        return false;
    }
    let kind = kind.clone();
    let mut tail = items.split_off(item);
    let mut moving = tail.remove(0);
    if !tail.is_empty() {
        moving.push(Block::List { kind, items: tail });
    }
    let inner_empty = items.is_empty();
    if let Some(Block::List { items: outer_items, .. }) = block_at_mut(blocks, &outer_list) {
        if inner_empty
            && let Some(oi) = outer_items.get_mut(outer_item)
            && li < oi.len()
        {
            oi.remove(li);
        }
        let at = outer_item.saturating_add(1).min(outer_items.len());
        outer_items.insert(at, moving);
        // An outer item emptied by the move gets dropped.
        if outer_items.get(outer_item).is_some_and(Vec::is_empty) {
            outer_items.remove(outer_item);
        }
    }
    true
}

fn indent_para(blocks: &mut [Block], path: &[usize], delta: f32) {
    if let Some(Block::Paragraph { style, .. } | Block::Heading { style, .. }) = block_at_mut(blocks, path) {
        style.indent_px = (style.indent_px + delta).clamp(0.0, INDENT_STEP_PX * 20.0);
    }
}

/// Paragraph alignment from its name.
pub fn align_of(kind: &str) -> Option<Align> {
    match kind {
        "left" | "start" => Some(Align::Start),
        "center" | "centre" => Some(Align::Center),
        "right" | "end" => Some(Align::End),
        "justify" => Some(Align::Justify),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(lines: &[&str]) -> ComposeState {
        let mut c = ComposeState {
            body: Doc {
                blocks: lines.iter().map(|l| Block::text_paragraph(l)).collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        c.ensure_body();
        c
    }

    fn p(path: &[usize], off: usize) -> DocPos {
        DocPos::new(path.to_vec(), off)
    }

    #[test]
    fn insert_and_split() {
        let mut c = doc(&[]);
        c.insert_text("Hello\nWorld");
        assert_eq!(c.body.to_plain_text(), "Hello\nWorld");
        assert_eq!(c.selection.focus, p(&[1], 5));
        c.set_caret(p(&[0], 2));
        c.insert_text("XX");
        assert_eq!(c.body.to_plain_text(), "HeXXllo\nWorld");
    }

    #[test]
    fn delete_across_blocks() {
        let mut c = doc(&["abc", "def", "ghi"]);
        c.set_selection(p(&[0], 1), p(&[2], 2));
        c.delete_selection();
        assert_eq!(c.body.to_plain_text(), "ai");
        assert_eq!(c.selection.focus, p(&[0], 1));
    }

    #[test]
    fn backspace_merges() {
        let mut c = doc(&["ab", "cd"]);
        c.set_caret(p(&[1], 0));
        c.delete_backward();
        assert_eq!(c.body.to_plain_text(), "abcd");
        assert_eq!(c.selection.focus, p(&[0], 2));
        c.delete_forward();
        assert_eq!(c.body.to_plain_text(), "abd");
        c.set_caret(p(&[0], 0));
        c.delete_forward();
        assert_eq!(c.body.to_plain_text(), "bd");
    }

    #[test]
    fn bold_range_and_pending() {
        let mut c = doc(&["hello world"]);
        c.set_selection(p(&[0], 0), p(&[0], 5));
        c.apply_text_style(|s| s.bold = Some(true));
        assert!(c.selection_all(|s, _| s.bold == Some(true)));
        match &c.body.blocks[0] {
            Block::Paragraph { inlines, .. } => assert_eq!(inlines.len(), 2),
            _ => panic!(),
        }
        c.set_caret(p(&[0], 11));
        c.apply_text_style(|s| s.italic = Some(true));
        c.insert_text("!");
        c.set_selection(p(&[0], 11), p(&[0], 12));
        assert!(c.selection_all(|s, _| s.italic == Some(true)));
    }

    #[test]
    fn lists_toggle_indent() {
        let mut c = doc(&["a", "b", "c"]);
        c.select_all();
        c.toggle_list(Some(ListKind::Bullet));
        assert!(matches!(&c.body.blocks[0], Block::List { items, .. } if items.len() == 3));
        assert_eq!(c.body.to_plain_text(), "\u{2022} a\n\u{2022} b\n\u{2022} c");
        // Demote b.
        let leaves = c.leaf_paths();
        c.set_caret(DocPos::new(leaves[1].clone(), 0));
        c.indent();
        assert_eq!(list_depth(&c.body.blocks, &c.selection.focus.path), 2);
        c.outdent();
        assert_eq!(list_depth(&c.body.blocks, &c.selection.focus.path), 1);
        c.set_list_level(3);
        assert_eq!(list_depth(&c.body.blocks, &c.selection.focus.path), 2, "cannot exceed previous item + 1");
        c.select_all();
        c.toggle_list(Some(ListKind::Numbered { start: 1 }));
        assert!(matches!(&c.body.blocks[0], Block::List { kind: ListKind::Numbered { .. }, .. }));
        c.select_all();
        c.toggle_list(None);
        assert_eq!(c.body.to_plain_text(), "a\nb\nc");
        assert!(c.body.blocks.iter().all(|b| matches!(b, Block::Paragraph { .. })));
        // Unwrapping the middle of a list splits it.
        c.select_all();
        c.toggle_list(Some(ListKind::Numbered { start: 1 }));
        let mid = c.leaf_paths()[1].clone();
        c.set_caret(DocPos::new(mid, 0));
        c.toggle_list(Some(ListKind::Numbered { start: 1 }));
        assert_eq!(c.body.to_plain_text(), "1. a\nb\n3. c");
    }

    #[test]
    fn enter_in_list_makes_item_and_exits() {
        let mut c = doc(&["one"]);
        c.toggle_list(Some(ListKind::Bullet));
        c.set_caret(c.body_end());
        c.insert_text("\ntwo\n\n");
        assert_eq!(c.body.to_plain_text(), "\u{2022} one\n\u{2022} two\n");
        assert_eq!(list_depth(&c.body.blocks, &c.selection.focus.path), 0);
    }

    #[test]
    fn insert_table_and_rule() {
        let mut c = doc(&["abcd"]);
        c.set_caret(p(&[0], 2));
        c.insert_blocks(vec![empty_table(2, 2)]);
        assert_eq!(c.body.blocks.len(), 3);
        assert_eq!(c.selection.focus.path, vec![1, 0, 0, 0]);
        c.set_caret(c.body_end());
        c.insert_blocks(vec![Block::Rule]);
        assert!(matches!(c.body.blocks.last(), Some(Block::Paragraph { .. })));
    }

    #[test]
    fn find_replace_case() {
        let mut c = doc(&["The cat", "a Cat sat"]);
        assert!(c.find_next("cat", false));
        assert_eq!(c.selection.start(), &p(&[0], 4));
        assert!(c.find_next("cat", false));
        assert_eq!(c.selection.start(), &p(&[1], 2));
        assert!(c.find_next("cat", false));
        assert_eq!(c.selection.start(), &p(&[0], 4), "wraps");
        assert!(!c.find_next("dog", false));
        assert_eq!(c.replace_all("cat", "dog", false), 2);
        assert_eq!(c.body.to_plain_text(), "The dog\na dog sat");
        c.select_all();
        c.change_case(CaseKind::Title);
        assert_eq!(c.body.to_plain_text(), "The Dog\nA Dog Sat");
        c.change_case(CaseKind::Sentence);
        assert_eq!(c.body.to_plain_text(), "The dog\nA dog sat");
    }

    #[test]
    fn sort_and_count() {
        let mut c = doc(&["pear", "10", "apple", "9"]);
        c.select_all();
        c.sort_selection(false);
        assert_eq!(c.body.to_plain_text(), "9\n10\napple\npear");
        let wc = c.word_count();
        assert_eq!(wc.words, 4);
        assert_eq!(wc.paragraphs, 4);
    }

    #[test]
    fn undo_redo() {
        let mut c = doc(&["x"]);
        c.checkpoint();
        c.insert_text("y");
        assert!(c.undo_edit());
        assert_eq!(c.body.to_plain_text(), "x");
        assert!(c.redo_edit());
        assert_eq!(c.body.to_plain_text(), "yx");
    }

    #[test]
    fn hostile_positions_are_clamped() {
        let mut c = doc(&["ab"]);
        c.set_selection(p(&[9, 9, 9], 99), p(&[], 0));
        c.insert_text("z");
        c.delete_range(&p(&[42], 1), &p(&[0], 77));
        c.delete_backward();
        c.indent();
        c.sort_selection(true);
        c.toggle_list(None);
        assert!(!c.leaf_paths().is_empty());
    }

    fn seeded_doc() -> ComposeState {
        let mut c = doc(&["alpha beta", "gamma", "delta epsilon", "zeta"]);
        c.set_selection(DocPos::new(vec![1], 0), DocPos::new(vec![2], 3));
        c.toggle_list(Some(ListKind::Bullet));
        c.set_caret(c.body_end());
        c.insert_blocks(vec![empty_table(2, 2)]);
        c.insert_text("cell\ntext");
        c.set_caret(c.body_end());
        c.insert_blocks(vec![Block::Rule, Block::Quote { blocks: vec![Block::text_paragraph("quoted")] }]);
        c
    }

    #[derive(Clone, Debug)]
    enum Op {
        Select(Vec<usize>, usize, Vec<usize>, usize),
        Type(String),
        Back,
        Fwd,
        List(u8),
        Indent(bool),
        Level(usize),
        Table,
        Rule,
        Bold,
        Case(u8),
        Sort(bool),
        Paste,
        Replace,
        ToTable,
        Undo,
        Similar,
        ParaRule(bool, bool),
    }

    fn op() -> impl proptest::strategy::Strategy<Value = Op> {
        use proptest::prelude::*;
        let path = proptest::collection::vec(0usize..4, 0..5);
        prop_oneof![
            (path.clone(), 0usize..20, path, 0usize..20).prop_map(|(a, x, b, y)| Op::Select(a, x, b, y)),
            "[a-c \\n]{0,6}".prop_map(Op::Type),
            Just(Op::Back),
            Just(Op::Fwd),
            (0u8..3).prop_map(Op::List),
            any::<bool>().prop_map(Op::Indent),
            (0usize..12).prop_map(Op::Level),
            Just(Op::Table),
            Just(Op::Rule),
            Just(Op::Bold),
            (0u8..5).prop_map(Op::Case),
            any::<bool>().prop_map(Op::Sort),
            Just(Op::Paste),
            Just(Op::Replace),
            Just(Op::ToTable),
            Just(Op::Undo),
            Just(Op::Similar),
            (any::<bool>(), any::<bool>()).prop_map(|(a, b)| Op::ParaRule(a, b)),
        ]
    }

    proptest::proptest! {
        #[test]
        fn random_edits_never_break_the_body(ops in proptest::collection::vec(op(), 1..40)) {
            let mut c = seeded_doc();
            for o in ops {
                c.checkpoint();
                match o {
                    Op::Select(a, x, b, y) => c.set_selection(DocPos::new(a, x), DocPos::new(b, y)),
                    Op::Type(t) => c.insert_text(&t),
                    Op::Back => c.delete_backward(),
                    Op::Fwd => c.delete_forward(),
                    Op::List(0) => c.toggle_list(None),
                    Op::List(1) => c.toggle_list(Some(ListKind::Bullet)),
                    Op::List(_) => c.toggle_list(Some(ListKind::Numbered { start: 1 })),
                    Op::Indent(true) => c.indent(),
                    Op::Indent(false) => c.outdent(),
                    Op::Level(l) => c.set_list_level(l),
                    Op::Table => c.insert_blocks(vec![empty_table(2, 3)]),
                    Op::Rule => c.insert_blocks(vec![Block::Rule]),
                    Op::Bold => c.apply_text_style(|s| s.bold = Some(true)),
                    Op::Case(k) => c.change_case([CaseKind::Sentence, CaseKind::Lower, CaseKind::Upper, CaseKind::Title, CaseKind::Toggle][usize::from(k)]),
                    Op::Sort(d) => c.sort_selection(d),
                    Op::Paste => {
                        let f = c.fragment();
                        c.insert_fragment(&f, PasteMode::KeepSource);
                    }
                    Op::Replace => {
                        c.replace_all("a", "xy", false);
                    }
                    Op::ToTable => {
                        c.text_to_table();
                    }
                    Op::Undo => {
                        c.undo_edit();
                    }
                    Op::Similar => {
                        c.select_similar();
                        c.apply_text_style(|s| s.italic = Some(true));
                    }
                    Op::ParaRule(a, on) => {
                        c.set_para_rule(a, on);
                    }
                }
                c.ensure_body();
                let leaves = c.leaf_paths();
                proptest::prop_assert!(!leaves.is_empty());
                proptest::prop_assert!(leaves.contains(&c.selection.focus.path));
                proptest::prop_assert!(leaves.contains(&c.selection.anchor.path));
                proptest::prop_assert!(c.selection.focus.offset <= leaf_len(&c.body.blocks, &c.selection.focus.path));
                let _ = c.body.to_plain_text();
                let _ = c.word_count();
            }
        }
    }

    #[test]
    fn links_and_fragment_paste() {
        let mut c = doc(&["see docs"]);
        c.set_selection(p(&[0], 4), p(&[0], 8));
        c.set_link(Some("https://example.com/"), None);
        assert!(
            matches!(&c.body.blocks[0], Block::Paragraph { inlines, .. } if matches!(inlines.last(), Some(Inline::Link { .. })))
        );
        c.select_all();
        let frag = c.fragment();
        c.set_caret(c.body_end());
        c.insert_fragment(&frag, PasteMode::KeepSource);
        assert_eq!(c.body.to_plain_text(), "see docssee docs");
    }
}

#[cfg(test)]
mod depth_tests {
    use super::*;
    use mailcraft_model::{Account, AccountKind, Mailbox};

    fn doc(lines: &[&str]) -> ComposeState {
        let mut c = ComposeState {
            body: Doc {
                blocks: lines.iter().map(|l| Block::text_paragraph(l)).collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        c.ensure_body();
        c
    }

    fn p(path: &[usize], off: usize) -> DocPos {
        DocPos::new(path.to_vec(), off)
    }

    fn texts(c: &ComposeState) -> Vec<String> {
        c.leaf_paths()
            .iter()
            .map(|l| leaf(&c.body.blocks, l).map(|(_, f)| f.plain()).unwrap_or_default())
            .collect()
    }

    #[test]
    fn paste_html_merges_first_and_last_paragraphs() {
        let mut c = doc(&["Hello world"]);
        c.set_caret(p(&[0], 6));
        c.paste_html("<p>big <b>bold</b></p><h2>Middle</h2><p>end</p>");
        assert_eq!(texts(&c), ["Hello big bold", "Middle", "endworld"]);
        assert!(matches!(c.body.blocks[1], Block::Heading { level: 2, .. }));
        assert_eq!(c.selection.focus, p(&[2], 3));
        // Bold survived the merge.
        let bold = c.body.blocks[0]
            == Block::Paragraph {
                style: ParaStyle::default(),
                inlines: vec![
                    Inline::Text { text: "Hello big ".into(), style: TextStyle::default() },
                    Inline::Text {
                        text: "bold".into(),
                        style: TextStyle { bold: Some(true), ..Default::default() },
                    },
                ],
            };
        assert!(bold, "{:?}", c.body.blocks[0]);
        assert!(c.undo_edit());
        assert_eq!(texts(&c), ["Hello world"]);
        // Plain text paste splits paragraphs at line ends.
        c.set_caret(p(&[0], 5));
        c.paste_plain(" one\ntwo\nthree");
        assert_eq!(texts(&c), ["Hello one", "two", "three world"]);
        // A plain-text body takes only the text of pasted HTML.
        let mut t = doc(&["x"]);
        t.format = BodyFormat::PlainText;
        t.set_caret(p(&[0], 1));
        t.paste_html("<p><b>A</b></p><p>B</p>");
        assert_eq!(texts(&t), ["xA", "B"]);
        // Hostile HTML never breaks the body.
        let mut h = doc(&["a", "b"]);
        h.select_all();
        h.paste_html(&"<table><tr><td><ul><li>".repeat(200));
        h.ensure_body();
        assert!(!h.leaf_paths().is_empty());
    }

    #[test]
    fn table_navigation_and_layout_edits() {
        let mut c = doc(&["before"]);
        c.set_caret(p(&[0], 6));
        c.insert_blocks(vec![empty_table(2, 2)]);
        assert_eq!(c.cell_at_caret(), Some((vec![1], 0, 0)));
        c.insert_text("a");
        assert!(c.move_to_cell(CellMove::Next));
        assert_eq!(c.cell_at_caret(), Some((vec![1], 0, 1)));
        c.insert_text("b");
        assert!(c.move_to_cell(CellMove::Previous));
        assert_eq!(c.selected_text(), "a", "moving selects the cell content");
        assert!(!c.move_to_cell(CellMove::Up));
        assert!(c.move_to_cell(CellMove::Down));
        assert_eq!(c.cell_at_caret(), Some((vec![1], 1, 0)));
        assert!(c.move_to_cell(CellMove::Next));
        // Tab in the last cell adds a row.
        assert!(c.move_to_cell(CellMove::Next));
        assert_eq!(c.cell_at_caret(), Some((vec![1], 2, 0)));
        let rows = |c: &ComposeState| match &c.body.blocks[1] {
            Block::Table { rows, .. } => (rows.len(), rows[0].len()),
            _ => (0, 0),
        };
        assert_eq!(rows(&c), (3, 2));
        assert!(c.table_edit(TableEdit::InsertColumnRight));
        assert_eq!(rows(&c), (3, 3));
        assert_eq!(c.cell_at_caret(), Some((vec![1], 2, 1)));
        assert!(c.table_edit(TableEdit::InsertRowAbove));
        assert_eq!(rows(&c), (4, 3));
        assert!(c.table_edit(TableEdit::DeleteColumn));
        assert!(c.table_edit(TableEdit::DeleteRow));
        assert_eq!(rows(&c), (3, 2));
        assert!(c.table_edit(TableEdit::DeleteTable));
        assert!(!matches!(c.body.blocks.get(1), Some(Block::Table { .. })));
        assert!(c.cell_at_caret().is_none());
        assert!(!c.move_to_cell(CellMove::Next), "not in a table");
        assert!(!c.table_edit(TableEdit::DeleteRow));
        // Undo brings the table back.
        assert!(c.undo_edit());
        assert!(matches!(c.body.blocks.get(1), Some(Block::Table { .. })));
        // A list inside a cell still finds its cell; a 1x1 table deletes away by rows.
        let mut n = doc(&["x"]);
        n.insert_blocks(vec![empty_table(1, 1)]);
        n.toggle_list(Some(ListKind::Bullet));
        assert_eq!(n.cell_at_caret().map(|(_, r, c)| (r, c)), Some((0, 0)));
        assert!(n.table_edit(TableEdit::DeleteRow));
        assert!(n.cell_at_caret().is_none());
    }

    #[test]
    fn attachments_names_types_and_items() {
        let mut c = ComposeState::default();
        assert_eq!(c.attach_bytes("../../etc/report.PDF", None, vec![1, 2]).unwrap(), 0);
        assert_eq!(c.attachments[0].filename, "report.PDF");
        assert_eq!(c.attachments[0].mime_type, "application/pdf");
        c.attach_bytes("notes", Some("not a type"), vec![]).unwrap();
        assert_eq!(c.attachments[1].mime_type, "application/octet-stream");
        c.attach_bytes("x.bin", Some("Application/X-Custom"), vec![]).unwrap();
        assert_eq!(c.attachments[2].mime_type, "application/x-custom");
        assert_eq!(c.rename_attachment(1, "notes.md").unwrap(), "notes.md");
        assert_eq!(c.attachments[1].mime_type, "text/markdown");
        assert!(c.rename_attachment(9, "a").is_err());
        assert!(c.rename_attachment(0, "  ").is_err());
        assert_eq!(c.remove_attachment(2).map(|a| a.filename), Some("x.bin".into()));
        assert!(c.remove_attachment(2).is_none());
        assert_eq!(c.attachments_size(), 2);
        let m = mailcraft_model::MailItem { subject: "Q3: plan / budget?".into(), ..Default::default() };
        let i = c.attach_message(&m).unwrap();
        assert_eq!(c.attachments[i].filename, "Q3_ plan _ budget_.eml");
        assert_eq!(c.attachments[i].mime_type, "message/rfc822");
        assert!(!c.attachments[i].data.is_empty());
        assert_eq!(clean_attachment_name("", "x"), "x");
        assert_eq!(clean_attachment_name("a\u{0}b.txt", "x"), "a_b.txt");
        let long = format!("{}.docx", "n".repeat(400));
        let short = clean_attachment_name(&long, "x");
        assert!(short.ends_with(".docx") && short.chars().count() <= MAX_ATTACHMENT_NAME_CHARS);
        for (name, mime) in [
            ("a.JPG", "image/jpeg"),
            ("b.tar.gz", "application/gzip"),
            ("c.eml", "message/rfc822"),
            ("d.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            ("noext", "application/octet-stream"),
            ("e.", "application/octet-stream"),
        ] {
            assert_eq!(mime_for_name(name), mime, "{name}");
        }
    }

    fn sig_mailbox() -> (Mailbox, mailcraft_model::AccountId, mailcraft_model::AccountId) {
        let mut mb = Mailbox::new_local("Local");
        let work = mb.add_account(Account {
            kind: AccountKind::Imap,
            address: "me@work.example".into(),
            is_default: true,
            ..Default::default()
        });
        let home = mb.add_account(Account {
            kind: AccountKind::Imap,
            address: "me@home.example".into(),
            ..Default::default()
        });
        mb.add_signature("Work", mailcraft_html::html_to_doc("<p><b>Me</b><br>Work Inc.</p>")).unwrap();
        mb.add_signature("Short", Doc::from_plain_text("-- me")).unwrap();
        mb.set_signature_defaults(None, Some(Some("Work")), Some(Some("Short"))).unwrap();
        mb.set_signature_defaults(Some(home), Some(None), Some(None)).unwrap();
        (mb, work, home)
    }

    #[test]
    fn default_signature_for_new_messages() {
        let (mb, _, _) = sig_mailbox();
        let mut c = ComposeState {
            kind: ComposeKind::New,
            from: "Me <me@work.example>".into(),
            ..Default::default()
        };
        assert!(insert_default_signature(&mb, &mut c));
        assert_eq!(c.body.blocks.len(), 2);
        assert_eq!(c.body.blocks[0], Block::text_paragraph(""));
        assert!(matches!(&c.body.blocks[1], Block::Container { blocks, .. } if blocks.len() == 1));
        assert_eq!(c.selection.focus, p(&[0], 0));
        assert_eq!(c.signature.as_ref().map(|s| s.name.as_str()), Some("Work"));
        assert!(!c.dirty, "an automatic signature is not an unsaved change");
        assert!(!insert_default_signature(&mb, &mut c), "only once");
        assert!(c.body.to_plain_text().contains("Work Inc."));
        // The home account chose "(none)".
        let mut h =
            ComposeState { kind: ComposeKind::New, from: "me@home.example".into(), ..Default::default() };
        assert!(!insert_default_signature(&mb, &mut h));
        // Unknown From → the default account; drafts never get one.
        let mut u = ComposeState { kind: ComposeKind::New, ..Default::default() };
        assert!(insert_default_signature(&mb, &mut u));
        let mut d = ComposeState { kind: ComposeKind::Draft, ..Default::default() };
        assert!(!insert_default_signature(&mb, &mut d));
        // Plain text bodies get the text of the signature.
        let mut t =
            ComposeState { kind: ComposeKind::New, format: BodyFormat::PlainText, ..Default::default() };
        assert!(insert_default_signature(&mb, &mut t));
        assert!(matches!(&t.body.blocks[1], Block::Container { blocks, .. } if blocks.len() == 2));
        // A prefilled new message gets it at the end.
        let mut f = doc(&["Prefilled"]);
        assert!(insert_default_signature(&mb, &mut f));
        assert_eq!(f.body.blocks.len(), 3);
        assert!(matches!(f.body.blocks[2], Block::Container { .. }));
    }

    #[test]
    fn default_signature_for_replies_sits_above_the_quote() {
        let (mb, _, _) = sig_mailbox();
        let quoted = vec![
            Block::text_paragraph(""),
            Block::text_paragraph(""),
            Block::text_paragraph("-----Original Message-----"),
            Block::text_paragraph("From: A"),
        ];
        let mut c = ComposeState {
            kind: ComposeKind::Reply,
            body: Doc { blocks: quoted.clone(), ..Default::default() },
            ..Default::default()
        };
        assert!(insert_default_signature(&mb, &mut c));
        assert_eq!(c.signature.as_ref().map(|s| s.name.as_str()), Some("Short"));
        assert_eq!(c.body.blocks[0], Block::text_paragraph(""));
        assert!(matches!(c.body.blocks[1], Block::Container { .. }));
        assert_eq!(c.body.blocks[2], Block::text_paragraph(""));
        assert_eq!(c.body.blocks[3], Block::text_paragraph("-----Original Message-----"));
        assert_eq!(c.selection.focus, p(&[0], 0));
        // No leading empty paragraph: one is added for the caret.
        let mut f = ComposeState {
            kind: ComposeKind::Forward,
            body: Doc { blocks: quoted[2..].to_vec(), ..Default::default() },
            ..Default::default()
        };
        assert!(insert_default_signature(&mb, &mut f));
        assert_eq!(f.body.blocks.len(), 5);
        assert!(matches!(f.body.blocks[1], Block::Container { .. }));
        // The existing Signature command replaces the auto-inserted block (same container).
        let sig = f.signature.clone().unwrap();
        assert!(
            f.body
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Container { blocks, .. } if *blocks == sig.blocks))
        );
    }

    fn fake_check(text: &str) -> Vec<TextIssue> {
        // Flags every "teh" (char ranges).
        let chars: Vec<char> = text.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i + 3 <= chars.len() {
            if chars[i..i + 3] == ['t', 'e', 'h'] {
                out.push(TextIssue {
                    start: i,
                    end: i + 3,
                    kind: String::new(),
                    suggestions: vec!["the".into()],
                });
                i += 3;
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn spelling_cache_ignore_and_correct() {
        let mut c = doc(&["teh cat and teh dog", "more teh"]);
        assert!(c.spelling_stale());
        assert!(c.refresh_spelling(false, &mut fake_check));
        assert_eq!(c.spell_issues.len(), 3);
        assert_eq!(c.spell_issues[0].kind, "spelling");
        assert_eq!(
            (c.spell_issues[1].start.clone(), c.spell_issues[1].end.clone()),
            (p(&[0], 12), p(&[0], 15))
        );
        assert!(!c.refresh_spelling(false, &mut fake_check), "cached until the body changes");
        c.set_caret(p(&[0], 1));
        assert_eq!(c.next_spell_issue().map(|i| i.start.clone()), Some(p(&[0], 12)));
        // Ignore once.
        assert_eq!(c.ignore_spelling(None, false, Some(&p(&[0], 13))), 1);
        assert!(c.refresh_spelling(true, &mut fake_check));
        assert_eq!(c.spell_issues.len(), 2);
        // Correct one; positions after it in the same paragraph stay right.
        assert_eq!(c.correct_spelling("the", Some(&p(&[0], 0)), false), 1);
        assert_eq!(texts(&c)[0], "the cat and teh dog");
        assert!(c.spelling_stale());
        c.refresh_spelling(false, &mut fake_check);
        assert_eq!(c.spell_issues.len(), 1, "ignored-once occurrence stays ignored");
        // Change All, then Ignore All.
        let mut d = doc(&["teh teh", "teh"]);
        d.refresh_spelling(false, &mut fake_check);
        assert_eq!(d.correct_spelling("the", Some(&p(&[1], 0)), true), 3);
        assert_eq!(texts(&d), ["the the", "the"]);
        let mut e = doc(&["teh", "x teh"]);
        e.refresh_spelling(false, &mut fake_check);
        assert_eq!(e.ignore_spelling(Some("TEH"), true, None), 2);
        e.mark_edited();
        e.refresh_spelling(false, &mut fake_check);
        assert!(e.spell_issues.is_empty());
        assert_eq!(e.ignore_spelling(None, false, Some(&p(&[0], 0))), 0);
        // Stale cache entries never corrupt text.
        let mut f = doc(&["teh"]);
        f.refresh_spelling(false, &mut fake_check);
        f.body.blocks[0] = Block::text_paragraph("abc");
        assert_eq!(f.correct_spelling("the", Some(&p(&[0], 0)), false), 0);
        assert_eq!(texts(&f), ["abc"]);
        // Hostile checker output is ignored.
        let mut g = doc(&["abc"]);
        g.refresh_spelling(true, &mut |_| {
            vec![
                TextIssue { start: 2, end: 99, ..Default::default() },
                TextIssue { start: 2, end: 1, ..Default::default() },
            ]
        });
        assert!(g.spell_issues.is_empty());
    }

    #[test]
    fn spelling_skips_links_code_and_quoted_text() {
        let mut c = doc(&["teh", "", "-----Original Message-----", "teh"]);
        c.kind = ComposeKind::Reply;
        c.set_selection(p(&[1], 0), p(&[1], 0));
        c.insert_text("teh link");
        c.set_selection(p(&[1], 0), p(&[1], 8));
        c.set_link(Some("https://example.com/teh"), None);
        c.refresh_spelling(true, &mut fake_check);
        assert_eq!(c.spell_issues.len(), 1);
        assert_eq!(c.spell_issues[0].start, p(&[0], 0));
        let mut code = doc(&["teh"]);
        code.select_all();
        code.apply_text_style(|s| s.font_family = Some("Consolas".into()));
        code.refresh_spelling(true, &mut fake_check);
        assert!(code.spell_issues.is_empty());
    }

    #[test]
    fn recipients_from_fields() {
        let c = ComposeState {
            to: "Ann Lee <ann@example.org>; bad entry".into(),
            cc: "bo@example.net".into(),
            bcc: "\"Rivera, Al\" <al@example.com>".into(),
            ..Default::default()
        };
        assert_eq!(
            c.recipients(),
            [
                ("Ann Lee".to_owned(), "ann@example.org".to_owned()),
                (String::new(), "bo@example.net".to_owned()),
                ("Rivera, Al".to_owned(), "al@example.com".to_owned())
            ]
        );
    }

    #[test]
    fn edit_counter_moves_with_every_edit() {
        let mut c = doc(&["a"]);
        let r0 = c.edit_rev;
        c.insert_text("b");
        let r1 = c.edit_rev;
        assert_ne!(r0, r1);
        c.checkpoint();
        c.select_all();
        c.delete_selection();
        assert_ne!(c.edit_rev, r1);
        let r2 = c.edit_rev;
        c.undo_edit();
        assert_ne!(c.edit_rev, r2);
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    fn para(t: &str) -> Block {
        Block::text_paragraph(t)
    }

    fn state(blocks: Vec<Block>) -> ComposeState {
        let mut c = ComposeState { body: Doc { blocks, ..Default::default() }, ..Default::default() };
        c.ensure_body();
        c
    }

    fn p(path: &[usize], off: usize) -> DocPos {
        DocPos::new(path.to_vec(), off)
    }

    /// A body mixing paragraphs, a heading, nested lists, a quote and a rule.
    fn mixed() -> ComposeState {
        state(vec![
            para("alpha beta"),
            Block::Heading {
                level: 2,
                style: ParaStyle::default(),
                inlines: vec![Inline::Text { text: "Head line".into(), style: TextStyle::default() }],
            },
            Block::List {
                kind: ListKind::Bullet,
                items: vec![
                    vec![
                        para("one"),
                        Block::List { kind: ListKind::Bullet, items: vec![vec![para("one.a")]] },
                    ],
                    vec![para("two")],
                    vec![para("three"), para("three more")],
                ],
            },
            Block::Quote { blocks: vec![para("quoted one"), para("quoted two")] },
            para("gamma"),
            Block::Rule,
            para("delta epsilon"),
        ])
    }

    fn all_text(c: &ComposeState) -> String {
        range_text(&c.body, &c.body_start(), &c.body_end())
    }

    proptest::proptest! {
        /// Deleting any selection removes exactly the selected text: the text before the selection and the
        /// text after it join at the caret, and one undo brings the body back.
        #[test]
        fn delete_selection_removes_exactly_the_selection(a in 0usize..14, ao in 0usize..14, b in 0usize..14, bo in 0usize..14) {
            let mut c = mixed();
            let leaves = c.leaf_paths();
            let pa = leaves[a % leaves.len()].clone();
            let pb = leaves[b % leaves.len()].clone();
            let la = leaf_len(&c.body.blocks, &pa);
            let lb = leaf_len(&c.body.blocks, &pb);
            let x = DocPos::new(pa, ao.min(la));
            let y = DocPos::new(pb, bo.min(lb));
            c.set_selection(x, y);
            let (s, e) = (c.selection.start().clone(), c.selection.end().clone());
            let before = c.body.clone();
            let pre = range_text(&c.body, &c.body_start(), &s);
            let post = range_text(&c.body, &e, &c.body_end());
            c.checkpoint();
            c.delete_selection();
            c.ensure_body();
            proptest::prop_assert_eq!(all_text(&c), format!("{pre}{post}"));
            proptest::prop_assert_eq!(range_text(&c.body, &c.body_start(), &c.selection.focus), pre);
            proptest::prop_assert!(c.undo_edit());
            proptest::prop_assert_eq!(&c.body, &before);
        }
    }

    #[test]
    fn inserting_a_table_inside_preformatted_text_splits_it() {
        let mut c = state(vec![Block::Pre { text: "line one\nline two".into() }]);
        c.set_caret(p(&[0], 8));
        c.insert_blocks(vec![empty_table(1, 1)]);
        assert!(
            matches!(&c.body.blocks[0], Block::Pre { text } if text == "line one"),
            "{:?}",
            c.body.blocks
        );
        assert!(matches!(c.body.blocks[1], Block::Table { .. }));
        assert!(matches!(&c.body.blocks[2], Block::Pre { text } if text == "line two"));
    }

    #[test]
    fn backspace_after_a_table_does_not_pull_text_into_a_cell() {
        let mut c = state(vec![para("before"), empty_table(1, 2), para("after")]);
        c.set_caret(p(&[2], 0));
        c.delete_backward();
        assert!(matches!(c.body.blocks.get(2), Some(Block::Paragraph { .. })), "{:?}", c.body.blocks);
        assert_eq!(blocks_text(std::slice::from_ref(&c.body.blocks[2])), "after");
        assert_eq!(
            c.cell_at_caret().map(|(_, r, col)| (r, col)),
            Some((0, 1)),
            "caret moves into the last cell"
        );
        // An empty paragraph after a table goes away instead.
        let mut e = state(vec![empty_table(1, 1), para(""), para("x")]);
        e.set_caret(p(&[1], 0));
        e.delete_backward();
        assert_eq!(e.body.blocks.len(), 2);
        assert!(e.cell_at_caret().is_some());
    }

    #[test]
    fn delete_before_a_table_does_not_pull_text_out_of_a_cell() {
        let mut c = state(vec![para("before"), empty_table(1, 1), para("after")]);
        c.set_caret(p(&[1, 0, 0, 0], 0));
        c.insert_text("cell");
        c.set_caret(p(&[0], 6));
        c.delete_forward();
        assert_eq!(c.body.to_plain_text().lines().next(), Some("before"));
        assert_eq!(leaf_len(&c.body.blocks, &[1, 0, 0, 0]), 4, "{:?}", c.body.blocks);
        // Delete at the end of the last cell does not swallow the paragraph after the table.
        c.set_caret(p(&[1, 0, 0, 0], 4));
        c.delete_forward();
        assert_eq!(leaf_len(&c.body.blocks, &[2]), 5, "{:?}", c.body.blocks);
        // Backspace at the start of a cell stays in the table too.
        let mut t = state(vec![empty_table(1, 2)]);
        t.set_caret(p(&[0, 0, 0, 0], 0));
        t.insert_text("a");
        t.set_caret(p(&[0, 0, 1, 0], 0));
        t.insert_text("b");
        t.set_caret(p(&[0, 0, 1, 0], 0));
        t.delete_backward();
        assert_eq!(leaf_len(&t.body.blocks, &[0, 0, 0, 0]), 1);
        assert_eq!(leaf_len(&t.body.blocks, &[0, 0, 1, 0]), 1);
    }

    fn cell_text(c: &ComposeState, path: &[usize]) -> String {
        leaf(&c.body.blocks, path).map(|(_, f)| f.plain()).unwrap_or_default()
    }

    #[test]
    fn deleting_a_selection_across_table_cells_keeps_the_cells() {
        let mut t = state(vec![para("before"), empty_table(1, 2), para("after")]);
        t.set_caret(p(&[1, 0, 0, 0], 0));
        t.insert_text("abc");
        t.set_caret(p(&[1, 0, 1, 0], 0));
        t.insert_text("def");
        // Inside the table: the cells stay, each loses its selected part.
        let mut c = t.clone();
        c.set_selection(p(&[1, 0, 0, 0], 1), p(&[1, 0, 1, 0], 2));
        c.delete_selection();
        assert_eq!(cell_text(&c, &[1, 0, 0, 0]), "a", "{:?}", c.body.blocks);
        assert_eq!(cell_text(&c, &[1, 0, 1, 0]), "f", "{:?}", c.body.blocks);
        assert_eq!(c.selection.focus, p(&[1, 0, 0, 0], 1));
        // From the paragraph before the table into the second cell: the first cell is emptied, the text
        // after the selection stays in its cell.
        let mut c = t.clone();
        c.set_selection(p(&[0], 3), p(&[1, 0, 1, 0], 1));
        c.delete_selection();
        assert_eq!(cell_text(&c, &[0]), "bef");
        assert_eq!(cell_text(&c, &[1, 0, 0, 0]), "");
        assert_eq!(cell_text(&c, &[1, 0, 1, 0]), "ef");
        // From a cell to the paragraph after the table: the rest of the cells are emptied, the paragraph
        // keeps its unselected text and stays outside the table.
        let mut c = t.clone();
        c.set_selection(p(&[1, 0, 0, 0], 2), p(&[2], 2));
        c.delete_selection();
        assert_eq!(cell_text(&c, &[1, 0, 0, 0]), "ab");
        assert_eq!(cell_text(&c, &[1, 0, 1, 0]), "");
        assert_eq!(cell_text(&c, &[2]), "ter");
        // Typing over a selection that spans cells replaces it in the first cell.
        let mut c = t.clone();
        c.set_selection(p(&[1, 0, 0, 0], 0), p(&[1, 0, 1, 0], 3));
        c.insert_text("x");
        assert_eq!(cell_text(&c, &[1, 0, 0, 0]), "x");
        assert_eq!(cell_text(&c, &[1, 0, 1, 0]), "");
        // A whole table inside the selection still goes away.
        let mut c = t.clone();
        c.set_selection(p(&[0], 3), p(&[2], 2));
        c.delete_selection();
        assert_eq!(c.body.blocks.len(), 1);
        assert_eq!(cell_text(&c, &[0]), "befter");
    }

    #[test]
    fn a_spelling_change_that_finds_nothing_records_no_undo_step() {
        let mut c = state(vec![para("teh cat")]);
        c.refresh_spelling(true, &mut |t: &str| {
            if t.starts_with("teh") {
                vec![TextIssue { start: 0, end: 3, kind: "spelling".into(), suggestions: vec![] }]
            } else {
                Vec::new()
            }
        });
        assert_eq!(c.spell_issues.len(), 1);
        // The text changed under the cached issue (no re-check yet).
        c.set_caret(p(&[0], 0));
        c.insert_text("x");
        c.dirty = false;
        let undo = c.edit_undo.len();
        assert_eq!(c.correct_spelling("the", Some(&p(&[0], 1)), false), 0);
        assert_eq!(c.edit_undo.len(), undo, "no empty undo step");
        assert!(!c.dirty);
        assert_eq!(cell_text(&c, &[0]), "xteh cat");
    }

    #[test]
    fn a_rule_typed_above_the_quote_does_not_hide_the_reply_from_spelling() {
        let mut c = state(vec![
            para("teh reply"),
            Block::Rule,
            para("teh more"),
            Block::Rule,
            para("From: A <a@example.com>"),
            para("teh quoted"),
        ]);
        c.kind = ComposeKind::Reply;
        assert_eq!(c.quoted_start(), Some(3));
        c.refresh_spelling(true, &mut |t: &str| {
            if t.starts_with("teh") {
                vec![TextIssue { start: 0, end: 3, kind: "spelling".into(), suggestions: vec![] }]
            } else {
                Vec::new()
            }
        });
        let paths: Vec<Vec<usize>> = c.spell_issues.iter().map(|i| i.start.path.clone()).collect();
        assert_eq!(paths, [vec![0], vec![2]]);
    }

    #[test]
    fn enter_on_an_empty_nested_item_moves_it_up_a_level() {
        let nested = Block::List { kind: ListKind::Bullet, items: vec![vec![para("a")]] };
        let mut c =
            state(vec![Block::List { kind: ListKind::Bullet, items: vec![vec![para("one"), nested]] }]);
        c.set_caret(p(&[0, 0, 1, 0, 0], 1));
        c.insert_text("\n");
        assert_eq!(list_depth(&c.body.blocks, &c.selection.focus.path), 2, "a new nested item");
        c.insert_text("\n");
        assert_eq!(list_depth(&c.body.blocks, &c.selection.focus.path), 1, "{:?}", c.body.blocks);
        assert!(
            matches!(
                parent_of(&c.body.blocks, &c.selection.focus.path),
                Some((Owner::ListItem { item: 1, .. }, 0))
            ),
            "a new item of the outer list: {:?}",
            c.selection.focus
        );
        c.insert_text("two");
        assert!(
            matches!(&c.body.blocks[0], Block::List { items, .. } if items.len() == 2 && items[0].len() == 2),
            "{:?}",
            c.body.blocks
        );
        // Enter on the now empty outer item leaves the list.
        c.insert_text("\n\n");
        assert_eq!(list_depth(&c.body.blocks, &c.selection.focus.path), 0);
    }

    fn with_table() -> ComposeState {
        let mut t = empty_table(2, 2);
        if let Block::Table { rows, .. } = &mut t {
            for (r, row) in rows.iter_mut().enumerate() {
                for (k, cell) in row.iter_mut().enumerate() {
                    *cell = table_cell(&format!("cell{r}{k}"));
                }
            }
        }
        state(vec![
            para("alpha"),
            t,
            Block::List { kind: ListKind::Bullet, items: vec![vec![para("item")]] },
            para("omega"),
        ])
    }

    proptest::proptest! {
        /// Deleting any selection in a body with a table removes exactly the selected text (paragraph breaks
        /// aside: cells are emptied, not merged), keeps the table unless all of it was selected, and one undo
        /// brings the body back.
        #[test]
        fn delete_selection_with_tables_removes_exactly_the_selection(a in 0usize..10, ao in 0usize..8, b in 0usize..10, bo in 0usize..8) {
            let mut c = with_table();
            let leaves = c.leaf_paths();
            let pa = leaves[a % leaves.len()].clone();
            let pb = leaves[b % leaves.len()].clone();
            let x = DocPos::new(pa.clone(), ao.min(leaf_len(&c.body.blocks, &pa)));
            let y = DocPos::new(pb.clone(), bo.min(leaf_len(&c.body.blocks, &pb)));
            c.set_selection(x, y);
            let (s, e) = (c.selection.start().clone(), c.selection.end().clone());
            let whole_table = s.path < vec![1] && e.path >= vec![2];
            let before = c.body.clone();
            let pre = range_text(&c.body, &c.body_start(), &s);
            let post = range_text(&c.body, &e, &c.body_end());
            c.checkpoint();
            c.delete_selection();
            c.ensure_body();
            let flat = |t: String| t.replace('\n', "");
            proptest::prop_assert_eq!(flat(all_text(&c)), flat(format!("{pre}{post}")));
            let tables = c.body.blocks.iter().filter(|b| matches!(b, Block::Table { .. })).count();
            proptest::prop_assert_eq!(tables, usize::from(!whole_table));
            proptest::prop_assert!(c.leaf_paths().contains(&c.selection.focus.path));
            proptest::prop_assert!(c.undo_edit());
            proptest::prop_assert_eq!(&c.body, &before);
        }
    }

    #[test]
    fn extra_spaces_can_be_ignored_once_or_everywhere() {
        // A grammar issue on whitespace only ("More than one space"): its "word" is the spaces.
        let spaces = |t: &str| -> Vec<TextIssue> {
            let chars: Vec<char> = t.chars().collect();
            let mut out = Vec::new();
            let mut i = 0;
            while i + 1 < chars.len() {
                if chars[i] == ' ' && chars[i + 1] == ' ' {
                    out.push(TextIssue {
                        start: i,
                        end: i + 2,
                        kind: "doubleSpace".into(),
                        suggestions: vec![" ".into()],
                    });
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out
        };
        let mut c = state(vec![para("a  b  c")]);
        c.refresh_spelling(true, &mut |t: &str| spaces(t));
        assert_eq!(c.spell_issues.len(), 2);
        assert_eq!(c.ignore_spelling(None, false, Some(&p(&[0], 1))), 1, "Ignore Once");
        c.refresh_spelling(true, &mut |t: &str| spaces(t));
        assert_eq!(c.spell_issues.len(), 1, "the ignored occurrence stays ignored");
        assert_eq!(c.ignore_spelling(None, true, Some(&p(&[0], 4))), 1, "Ignore All");
        c.refresh_spelling(true, &mut |t: &str| spaces(t));
        assert!(c.spell_issues.is_empty());
        assert_eq!(c.ignore_spelling(Some(""), true, None), 0, "an empty word is never ignored");
    }
}
