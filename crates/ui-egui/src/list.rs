//! Message list: header (Focused/Other, arrange menu, sort direction), grouped rows, compact/single/preview
//! layouts, hover actions, selection, keyboard navigation (list area).
//!
//! Behaviour from `plan/reference/message-list.md` and `plan/reference/reading-pane.md`: a 97 px header block,
//! date-group headers (24 px) and compact three-line rows (72 px with one preview line), or one-line column
//! rows (Single / Preview views, and Compact when the reading pane sits at the bottom or is off on a wide
//! list). Rows are virtualised: only the rows inside the viewport are laid out and painted, and the row
//! model from `queries::list_rows` is cached until the session changes, so thousands of items scroll
//! smoothly. All changes go through [`App::run`].
//!
//! Conversation view (`plan/reference/reply-forward.md` › Conversation view): a conversation is a header row
//! (◢ expanded / ▷ collapsed glyph, senders, subject without prefixes, unread count, latest time) followed,
//! when expanded, by indented compact child rows (sender + time, preview below); messages from other
//! folders are dimmed with their folder name in italics. Left / Right collapse / expand the focused
//! conversation. Turning Show as Conversations on or off asks for the scope ([`ask_conversation_scope`]).
//!
//! What the view settings add (`mailcraft_engine::cmd::view::mail`): the column layout shows the folder's
//! columns in their order, labels, formats and widths (drag a header edge to resize; the header's context
//! menu adds, removes and formats columns); rows take the font of the view's Conditional Formatting rules;
//! the hover buttons are the saved Quick Actions; the category column applies the Quick Click category;
//! pinned messages carry a pin and lead the list; header-only and marked-to-download messages show it.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Color32, EventFilter, FontFamily, FontId, Galley, Id, Key, Modifiers, Painter, Pos2, Rect, Role, Sense,
    Stroke, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use mailcraft_engine::Session;
use mailcraft_engine::cmd::view::mail::{self as mailview, ColumnKind, ColumnSpec};
use mailcraft_engine::queries::{self, ConversationRow, ItemRow, ListRow, RowStyle};
use mailcraft_engine::search_index::{self, Field};
#[cfg(test)]
use mailcraft_model::FolderId;
use mailcraft_model::{
    ArrangeBy, FlagState, FocusClass, FolderRole, ListFilter, ReadingPanePos, SortOrder, ViewLayout,
    ViewSettings,
};
use serde_json::{Value, json};

use crate::App;
use crate::responsive::Frame;
use crate::theme::{Theme, size};

/// Drag-and-drop payload: message ids dragged from the list (dropped on folders → `mail.move`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraggedItems(pub Vec<u64>);

/// Id of the list's keyboard-focus widget (the list body).
pub fn focus_id() -> Id {
    Id::unique("mailcraft.list.focus")
}

/// True while the message list has keyboard focus (Up/Down/Home/End/PgUp/PgDn/Enter belong to the list then).
pub fn has_focus(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.has_focus(focus_id()))
}

// -------------------------------------------------------------------------------------------------------------
// Text helpers shared with the folder pane.

/// Laid-out text; `faux_bold` paints the run twice when no bold face is installed.
#[derive(Clone)]
pub(crate) struct Text {
    pub galley: Arc<Galley>,
    faux_bold: bool,
}

/// A bold font family installed by the shell (any `FontFamily::Name` containing "bold"), if there is one.
pub(crate) fn bold_family(ctx: &egui::Context) -> Option<FontFamily> {
    ctx.fonts(|f| {
        let mut best: Option<(i32, FontFamily)> = None;
        for fam in f.definitions().families.keys() {
            let FontFamily::Name(name) = fam else { continue };
            let l = name.to_lowercase();
            if !l.contains("bold") || l.contains("italic") || l.contains("mono") {
                continue;
            }
            let mut score = 0;
            if l.contains("inter") || l.contains("ui") {
                score += 2;
            }
            if l.contains("semi") {
                score += 1;
            }
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, fam.clone()));
            }
        }
        best.map(|(_, f)| f)
    })
}

/// A layout job for `text` with the byte `ranges` (sorted or not, possibly overlapping, possibly off char
/// boundaries) drawn on `bg`; invalid ranges are skipped.
pub(crate) fn highlight_job(
    text: String,
    format: &TextFormat,
    ranges: &[(usize, usize)],
    bg: Color32,
) -> LayoutJob {
    let mut spans: Vec<(usize, usize)> = ranges
        .iter()
        .copied()
        .filter(|&(a, b)| a < b && b <= text.len() && text.is_char_boundary(a) && text.is_char_boundary(b))
        .collect();
    spans.sort_unstable();
    let mut job = LayoutJob::default();
    let mut at = 0usize;
    let hit = TextFormat { background: bg, ..format.clone() };
    for (a, b) in spans {
        let a = a.max(at);
        if a >= b {
            continue;
        }
        if let Some(plain) = text.get(at..a) {
            job.append(plain, 0.0, format.clone());
        }
        if let Some(word) = text.get(a..b) {
            job.append(word, 0.0, hit.clone());
        }
        at = b;
    }
    if let Some(rest) = text.get(at..) {
        job.append(rest, 0.0, format.clone());
    }
    job
}

/// Font effects of list text (Conditional Formatting).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Look {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

impl Look {
    fn plain(bold: bool) -> Look {
        Look { bold, ..Look::default() }
    }

    /// A row's rule font; `bold` follows the rules (the Unread messages rule makes unread rows bold).
    fn of(st: &RowStyle) -> Look {
        Look { bold: st.bold, italic: st.italic, underline: st.underline, strike: st.strike }
    }
}

/// The rule colour of a row, else `fallback`.
fn rule_color(st: &RowStyle, fallback: Color32) -> Color32 {
    st.color.as_deref().and_then(|h| Color32::from_hex(h.trim()).ok()).unwrap_or(fallback)
}

impl Text {
    /// Lays out `text` within `max_w`, at most `rows` lines, elided with "…".
    pub(crate) fn new(
        ui: &Ui,
        text: &str,
        size: f32,
        bold: bool,
        color: Color32,
        max_w: f32,
        rows: usize,
    ) -> Text {
        Text::highlighted(ui, text, size, bold, color, max_w, rows, None)
    }

    /// [`Text::new`] with search words (the active search's hits in `field`) on a highlight background.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn highlighted(
        ui: &Ui,
        text: &str,
        size: f32,
        bold: bool,
        color: Color32,
        max_w: f32,
        rows: usize,
        hits: Option<(&Session, Field, Color32)>,
    ) -> Text {
        Text::styled(ui, text, size, Look::plain(bold), color, max_w, rows, hits)
    }

    /// [`Text::highlighted`] with a full [`Look`] (italic, underline, strikethrough).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn styled(
        ui: &Ui,
        text: &str,
        size: f32,
        look: Look,
        color: Color32,
        max_w: f32,
        rows: usize,
        hits: Option<(&Session, Field, Color32)>,
    ) -> Text {
        let bold = look.bold;
        let (family, faux_bold) = if bold {
            match bold_family(ui.ctx()) {
                Some(f) => (f, false),
                None => (FontFamily::Proportional, true),
            }
        } else {
            (FontFamily::Proportional, false)
        };
        // Line breaks in one-line fields (subjects, previews) would push text out of the row.
        let clean: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let line = |on: bool| if on { Stroke::new(1.0, color) } else { Stroke::NONE };
        let format = TextFormat {
            font_id: FontId::new(size, family),
            color,
            italics: look.italic,
            underline: line(look.underline),
            strikethrough: line(look.strike),
            ..Default::default()
        };
        let ranges = match hits {
            Some((s, field, _)) => search_index::highlights(s, field, &clean),
            None => Vec::new(),
        };
        let mut job = match hits {
            Some((_, _, bg)) if !ranges.is_empty() => highlight_job(clean, &format, &ranges, bg),
            _ => LayoutJob::single_section(clean, format),
        };
        job.wrap = TextWrapping {
            max_width: max_w.max(1.0),
            max_rows: rows.max(1),
            break_anywhere: rows <= 1,
            overflow_character: Some('…'),
        };
        Text { galley: ui.painter().layout_job(job), faux_bold }
    }

    /// One line of italic text within `max_w`, elided with "…".
    pub(crate) fn italic(ui: &Ui, text: &str, size: f32, color: Color32, max_w: f32) -> Text {
        let clean: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let mut job = LayoutJob::single_section(
            clean,
            TextFormat { font_id: FontId::proportional(size), color, italics: true, ..Default::default() },
        );
        job.wrap = TextWrapping {
            max_width: max_w.max(1.0),
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        Text { galley: ui.painter().layout_job(job), faux_bold: false }
    }

    pub(crate) fn width(&self) -> f32 {
        self.galley.size().x + if self.faux_bold { 0.6 } else { 0.0 }
    }

    pub(crate) fn height(&self) -> f32 {
        self.galley.size().y
    }

    pub(crate) fn paint(&self, painter: &Painter, pos: Pos2) {
        painter.galley(pos, self.galley.clone(), Color32::PLACEHOLDER);
        if self.faux_bold {
            painter.galley(pos + vec2(0.6, 0.0), self.galley.clone(), Color32::PLACEHOLDER);
        }
    }

    /// Paints left-aligned at `x`, vertically centred in `band`; returns the right edge.
    pub(crate) fn paint_left(&self, painter: &Painter, band: Rect, x: f32) -> f32 {
        self.paint(painter, pos2(x, band.center().y - self.height() / 2.0));
        x + self.width()
    }

    /// Paints right-aligned to `right`, vertically centred in `band`; returns the left edge.
    pub(crate) fn paint_right(&self, painter: &Painter, band: Rect, right: f32) -> f32 {
        let x = right - self.width();
        self.paint(painter, pos2(x, band.center().y - self.height() / 2.0));
        x
    }
}

/// Square icon rect of side `side` centred at `c`.
pub(crate) fn icon_rect(c: Pos2, side: f32) -> Rect {
    Rect::from_center_size(c, vec2(side, side))
}

fn hex_color(hex: &str) -> Color32 {
    Color32::from_hex(hex.trim()).unwrap_or(Color32::GRAY)
}

// -------------------------------------------------------------------------------------------------------------
// Cached row model.

/// Everything the list draws for one session state.
struct Cached {
    key: u64,
    rows: Arc<Vec<ListRow>>,
    vs: ViewSettings,
    role: Option<FolderRole>,
    /// Focused / Other tabs are shown (Inbox with Focused Inbox on, no search).
    focused_tabs: bool,
    /// "Other: New messages (N)" banner: count and sender names.
    other_new: Option<(usize, String)>,
    /// Search › "Notify me when results might be limited…": the notice above incomplete search results.
    search_notice: Option<String>,
    searching: bool,
    /// Effective reading pane position for the folder.
    pane: ReadingPanePos,
    /// Columns of the column layout (Show Columns, Format Columns, widths).
    columns: Vec<ColumnSpec>,
    /// Quick Actions on a hovered row, nearest the right edge first.
    quick: [&'static str; 2],
    /// [`download_signature`] of the rows.
    downloads: u64,
    /// File › Options › Mail › Other › "Highlight flagged items in the message list": flagged rows get the
    /// flagged tint.
    highlight_flagged: bool,
}

#[derive(Clone, Default)]
struct ListState {
    cache: Option<Arc<Cached>>,
    /// Focus at the end of the previous frame (scrolls a newly focused row into view).
    last_focus: Option<u64>,
    /// Item rows per viewport (PgUp/PgDn step).
    page_rows: usize,
    /// "Mentioned" marks per item for the Mention column (cleared with the cache).
    mentions: BTreeMap<u64, bool>,
}

fn state_id() -> Id {
    Id::unique("mailcraft.list.state")
}

/// The messages in the main window's list order, the order Previous / Next Item walk
/// ([`queries::visible_item_ids`]). Read windows ask for it every frame; it is built again only when the
/// list changes ([`fingerprint`], as the list's own rows), not once per window per frame: with thousands of
/// messages in the folder that rebuild held up every frame of the main window while a message window was
/// open.
pub fn visible_ids(ctx: &egui::Context, s: &Session) -> Arc<Vec<mailcraft_model::ItemId>> {
    let key = fingerprint(s);
    let id = Id::unique("mailcraft.list.visible-ids");
    if let Some((k, ids)) = ctx.data(|d| d.get_temp::<(u64, Arc<Vec<mailcraft_model::ItemId>>)>(id))
        && k == key
    {
        return ids;
    }
    let ids = Arc::new(queries::visible_item_ids(s));
    ctx.data_mut(|d| d.insert_temp(id, (key, ids.clone())));
    ids
}

/// Cheap fingerprint of everything `list_rows` depends on. A mutating command that changes the mailbox
/// always pushes a new undo entry (each holds a freshly boxed small-state snapshot), so the top entry's
/// address changes with it.
fn fingerprint(s: &Session) -> u64 {
    let mut h = DefaultHasher::new();
    let folder = s.selection.folder;
    folder.hash(&mut h);
    s.selection.items.hash(&mut h);
    s.selection.focus.hash(&mut h);
    s.view.focused_tab.hash(&mut h);
    s.view.search.hash(&mut h);
    std::mem::discriminant(&s.view.search_scope).hash(&mut h);
    std::mem::discriminant(&s.view.reading_pane).hash(&mut h);
    std::mem::discriminant(&s.view.module).hash(&mut h);
    s.view.collapsed_groups.get(&folder).hash(&mut h);
    serde_json::to_string(&s.mailbox.views.get(&folder)).unwrap_or_default().hash(&mut h);
    s.mailbox.mail.len().hash(&mut h);
    s.mailbox.folders.len().hash(&mut h);
    s.undo.undo.len().hash(&mut h);
    s.undo.redo.len().hash(&mut h);
    s.undo.undo.last().map(|e| std::ptr::from_ref(e.small.as_ref()) as usize).hash(&mut h);
    s.undo.redo.last().map(|e| std::ptr::from_ref(e.small.as_ref()) as usize).hash(&mut h);
    for (k, c) in &s.composes {
        k.hash(&mut h);
        c.source.hash(&mut h);
    }
    (s.now().unix / 60).hash(&mut h);
    s.tz_offset_min.hash(&mut h);
    // Quick Actions and saved views live in the settings.
    s.settings.rev().hash(&mut h);
    // Include Older Results: server matches and the search's progress.
    let o = &s.sync.older;
    (o.on, o.generation, o.pending.len(), o.hits.len(), o.found, o.error.is_some()).hash(&mut h);
    h.finish()
}

/// Download state (header only, marked to download) of the listed items: marking and downloading change no
/// undo entry, so the cache compares this too.
fn download_signature(s: &Session, rows: &[ListRow]) -> u64 {
    use mailcraft_engine::sync::cmds;
    let mut h = DefaultHasher::new();
    for r in rows {
        if let ListRow::Item(i) = r {
            cmds::marked(s, i.id).hash(&mut h);
            cmds::is_header_only(s, i.id).hash(&mut h);
        }
    }
    h.finish()
}

fn build(s: &Session, key: u64) -> Cached {
    let folder = s.selection.folder;
    let vs = queries::view_settings(s, folder);
    let f = s.mailbox.folder(folder);
    let role = f.and_then(|f| f.role);
    let searching = !s.view.search.trim().is_empty();
    let focused_tabs = role == Some(FolderRole::Inbox) && vs.focused_inbox && !searching;
    let other_new = if focused_tabs && s.view.focused_tab {
        let new: Vec<_> = s
            .mailbox
            .items_in(folder)
            .into_iter()
            .filter(|m| m.focus == FocusClass::Other && !m.read)
            .collect();
        if new.is_empty() {
            None
        } else {
            let mut names: Vec<String> = Vec::new();
            for m in &new {
                let n =
                    m.from.as_ref().or(m.sender.as_ref()).map(|a| a.display().to_owned()).unwrap_or_default();
                if !n.is_empty() && !names.contains(&n) {
                    names.push(n);
                }
                if names.len() >= 3 {
                    break;
                }
            }
            Some((new.len(), names.join(", ")))
        }
    } else {
        None
    };
    let pane = s.mailbox.views.get(&folder).map(|v| v.reading_pane).unwrap_or(s.view.reading_pane);
    let rows = queries::list_rows(s);
    let downloads = download_signature(s, &rows);
    Cached {
        key,
        rows: Arc::new(rows),
        downloads,
        vs,
        role,
        focused_tabs,
        other_new,
        search_notice: queries::search_notice(s),
        searching,
        pane,
        columns: mailview::columns(s, folder),
        quick: mailview::quick_actions(s),
        highlight_flagged: s.settings.bool("mail.other.highlightFlagged"),
    }
}

/// Runs a command and asks for a repaint so the change shows next frame.
fn run(ui: &Ui, app: &mut App, id: &str, params: Value) {
    app.run(id, params);
    ui.ctx().request_repaint();
}

/// Runs a message command on explicit `ids` (added to `params`). The engine enables message commands only
/// while something is selected, so with an empty selection the items are selected first.
pub(crate) fn run_on_items(ui: &Ui, app: &mut App, id: &str, ids: &[u64], mut params: Value) {
    if ids.is_empty() {
        return;
    }
    if app.s.selection.items.is_empty() && app.s.selection.focus.is_none() {
        app.run("nav.selectItem", json!({ "ids": ids }));
    }
    if let Some(map) = params.as_object_mut() {
        map.insert("ids".to_owned(), json!(ids));
    }
    run(ui, app, id, params);
}

// -------------------------------------------------------------------------------------------------------------
// Geometry.

/// How rows are drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// Three-line rows.
    Compact,
    /// One-line column rows; `preview` lines of preview text under each (Preview view).
    Columns { preview: u8 },
}

fn mode_for(c: &Cached, width: f32) -> Mode {
    match c.vs.layout {
        ViewLayout::Single => Mode::Columns { preview: 0 },
        ViewLayout::Preview => Mode::Columns { preview: c.vs.preview_lines.clamp(1, 3) },
        ViewLayout::Compact => match c.pane {
            // reading-pane.md: with the pane at the bottom the list switches to the column layout by itself,
            // each row keeping a preview line under it.
            ReadingPanePos::Bottom => Mode::Columns { preview: c.vs.preview_lines.min(3) },
            ReadingPanePos::Off if width >= 640.0 => Mode::Columns { preview: 0 },
            _ => Mode::Compact,
        },
    }
}

const LINE1: f32 = 20.0;
const LINE: f32 = 18.0;
const COL_ROW: f32 = 24.0;
const PREVIEW_LINE: f32 = 17.0;

fn item_height(mode: Mode, preview_lines: u8, tighter: bool) -> f32 {
    match mode {
        Mode::Compact => {
            let pad = if tighter { 8.0 } else { 16.0 };
            pad + LINE1 + LINE + LINE * f32::from(preview_lines.min(3))
        }
        Mode::Columns { preview } => {
            let base = if tighter { COL_ROW - 4.0 } else { COL_ROW };
            if preview == 0 { base } else { base + PREVIEW_LINE * f32::from(preview) + 4.0 }
        }
    }
}

/// Width of the conversation glyph column (◢ / ▷) in compact rows.
const GLYPH_W: f32 = 14.0;
/// Extra indent per reply level of a conversation child row (Classic Indented View), up to `MAX_INDENT`.
const INDENT: f32 = 14.0;
const MAX_INDENT: u8 = 6;

/// Height of a conversation child row: compact rows put sender and time on one line with the preview
/// below; the column layout keeps its usual rows.
fn child_height(mode: Mode, preview_lines: u8, tighter: bool) -> f32 {
    match mode {
        Mode::Compact => {
            let pad = if tighter { 6.0 } else { 12.0 };
            pad + LINE1 + LINE * f32::from(preview_lines.min(3))
        }
        Mode::Columns { .. } => item_height(mode, preview_lines, tighter),
    }
}

/// Below this list width the column layout keeps only the essential columns (`ColumnSpec::essential`).
const NARROW_LIST: f32 = 620.0;
/// Received on a narrow list (the observed narrow geometry), unless set by hand.
const NARROW_RECEIVED: f32 = 112.0;

/// Width of a column that does not share the free space (`None` = it shares it).
fn fixed_width(c: &ColumnSpec, wide: bool) -> Option<f32> {
    match c.width {
        Some(w) => Some(f32::from(w)),
        None if c.default_width == 0 => None,
        None if !wide && c.id == "received" => Some(NARROW_RECEIVED),
        None => Some(f32::from(c.default_width)),
    }
}

/// Column x offsets and widths for a list `width` wide: `(index into cols, x, w)`. Person columns (From, To)
/// that share the free space with Subject stay narrow (a third of it, at most 200 px); the rest goes to the
/// other sharing columns. A narrow list keeps only the essential columns.
fn layout_columns(cols: &[ColumnSpec], width: f32) -> Vec<(usize, f32, f32)> {
    let wide = width >= NARROW_LIST;
    let mut shown: Vec<usize> =
        (0..cols.len()).filter(|i| wide || cols.get(*i).is_some_and(|c| c.essential)).collect();
    if shown.is_empty() {
        shown = (0..cols.len()).collect();
    }
    let used: f32 =
        shown.iter().filter_map(|i| cols.get(*i)).filter_map(|c| fixed_width(c, wide)).sum::<f32>() + 8.0;
    let sharing: Vec<usize> = shown
        .iter()
        .copied()
        .filter(|i| cols.get(*i).is_some_and(|c| fixed_width(c, wide).is_none()))
        .collect();
    let n_person =
        sharing.iter().filter(|i| cols.get(**i).is_some_and(|c| c.kind == ColumnKind::Person)).count();
    let n_other = sharing.len().saturating_sub(n_person);
    let free = (width - used).max(80.0);
    #[allow(clippy::cast_precision_loss)]
    let (person_w, other_w) = match (n_person, n_other) {
        (0, 0) => (0.0, 0.0),
        (0, n) => (0.0, (free / n as f32).max(40.0)),
        (p, 0) => ((free / p as f32).max(40.0), 0.0),
        (p, n) => {
            let each = (free * 0.34 / p as f32).clamp(40.0, 200.0);
            (each, ((free - each * p as f32) / n as f32).max(40.0))
        }
    };
    let mut out = Vec::new();
    let mut x = 4.0;
    for i in shown {
        let Some(c) = cols.get(i) else { continue };
        let w = match fixed_width(c, wide) {
            Some(w) => w,
            None if c.kind == ColumnKind::Person => person_w,
            None => other_w,
        };
        out.push((i, x, w));
        x += w;
    }
    out
}

/// Header label of a column: From reads "To" in Drafts, Sent Items and the Outbox (unless renamed).
fn column_label(c: &ColumnSpec, outgoing: bool) -> String {
    if c.id == "from" && outgoing && c.label == "From" { "To".to_owned() } else { c.label.clone() }
}

/// Arrangement of a header click (From arranges by To in outgoing folders).
fn column_arrange(c: &ColumnSpec, outgoing: bool) -> Option<ArrangeBy> {
    match c.arrange {
        Some(ArrangeBy::From) if outgoing => Some(ArrangeBy::To),
        a => a,
    }
}

/// Header glyph of an icon column (icon format).
fn column_icon(c: &ColumnSpec) -> Option<&'static str> {
    if c.kind != ColumnKind::Icon || c.format != "icon" {
        return None;
    }
    match c.id {
        "importance" => Some("importance-high"),
        "reminder" => Some("bell"),
        "icon" => Some("envelope-open"),
        "attachment" => Some("paperclip"),
        "flagStatus" => Some("flag"),
        _ => None,
    }
}

// -------------------------------------------------------------------------------------------------------------
// Entry point.

pub fn message_list(ui: &mut Ui, app: &mut App, frame: &Frame) {
    let ctx = ui.ctx().clone();
    let mut st: ListState = ctx.data_mut(|d| d.get_temp(state_id())).unwrap_or_default();
    let key = fingerprint(&app.s);
    let cached = match &st.cache {
        Some(c) if c.key == key && c.downloads == download_signature(&app.s, &c.rows) => c.clone(),
        _ => {
            let c = Arc::new(build(&app.s, key));
            st.cache = Some(c.clone());
            st.mentions.clear();
            c
        }
    };
    let full = ui.available_rect_before_wrap();
    if full.width() < 1.0 || full.height() < 1.0 {
        ctx.data_mut(|d| d.insert_temp(state_id(), st));
        return;
    }
    let mode = mode_for(&cached, full.width());
    let header_h = header_height(&cached, mode).min(full.height());
    let header_rect = Rect::from_min_size(full.min, vec2(full.width(), header_h));
    let body_rect = Rect::from_min_max(pos2(full.left(), header_rect.bottom()), full.max);
    header(ui, app, &cached, header_rect, mode);
    if body_rect.height() > 1.0 {
        body(ui, app, &cached, body_rect, mode, &mut st, frame);
    }
    drag_preview(ui, app);
    ui.advance_cursor_after_rect(full);
    ctx.data_mut(|d| d.insert_temp(state_id(), st));
    conversation_scope_prompt(&ctx, app);
}

// -------------------------------------------------------------------------------------------------------------
// Show as Conversations: scope question.

/// Pending scope question: the Show as Conversations state the answer switches to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScopePrompt {
    on: bool,
}

fn scope_prompt_id() -> Id {
    Id::unique("mailcraft.list.convscope")
}

/// Asks whether Show as Conversations changes for this folder or for all mailboxes before toggling it
/// (`plan/reference/reply-forward.md` › Conversation view). The answer runs `view.conversations` with
/// `{"on", "scope": "folder" | "all"}`; Cancel changes nothing. Surfaces offering Show as Conversations
/// call this instead of running the command directly.
pub fn ask_conversation_scope(ctx: &egui::Context, app: &App) {
    let on = !queries::view_settings(&app.s, app.s.selection.folder).conversations;
    ctx.data_mut(|d| d.insert_temp(scope_prompt_id(), ScopePrompt { on }));
    ctx.request_repaint();
}

/// The scope question is showing.
pub fn conversation_scope_pending(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<ScopePrompt>(scope_prompt_id())).is_some()
}

/// Draws the scope question while it is pending ([`message_list`] calls this every frame).
pub fn conversation_scope_prompt(ctx: &egui::Context, app: &mut App) {
    let Some(p) = ctx.data(|d| d.get_temp::<ScopePrompt>(scope_prompt_id())) else { return };
    // Some(Some(scope)) = answered, Some(None) = cancelled.
    let mut choice: Option<Option<&str>> = None;
    let modal = egui::Modal::new(Id::unique("mailcraft.list.convscope.modal")).show(ctx, |ui| {
        ui.set_max_width(360.0);
        let text = if p.on {
            "Arrange messages by conversation in:"
        } else {
            "Stop arranging messages by conversation in:"
        };
        ui.label(text);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("This folder").clicked() {
                choice = Some(Some("folder"));
            }
            if ui.button("All mailboxes").clicked() {
                choice = Some(Some("all"));
            }
            if ui.button("Cancel").clicked() {
                choice = Some(None);
            }
        });
    });
    if choice.is_none() && modal.should_close() {
        choice = Some(None);
    }
    if let Some(answer) = choice {
        ctx.data_mut(|d| d.remove::<ScopePrompt>(scope_prompt_id()));
        if let Some(scope) = answer {
            app.run("view.conversations", json!({ "on": p.on, "scope": scope }));
        }
        ctx.request_repaint();
    }
}

// -------------------------------------------------------------------------------------------------------------
// Header block.

/// Tabs row (Focused / Other or All / Unread, "By Date", sort direction): 48 px in the observed list
/// (pane top to first group header).
const HEADER_ROW: f32 = 48.0;
/// "Other: New messages (N)" banner row; with it the header block measures 97 px (message-list.md).
const BANNER_ROW: f32 = size::LIST_HEADER - HEADER_ROW;
/// Column header row of the column layout.
const COLUMN_HEADER: f32 = 26.0;

/// Height of the header block: the tabs row, plus the banner while there is new Other mail, plus the column
/// headers in the column layout. Nothing else sits above the rows (no folder title).
fn header_height(c: &Cached, mode: Mode) -> f32 {
    let mut h = HEADER_ROW;
    if c.other_new.is_some() || c.search_notice.is_some() {
        h += BANNER_ROW;
    }
    if let Mode::Columns { .. } = mode {
        h += COLUMN_HEADER;
    }
    h
}

fn header(ui: &mut Ui, app: &mut App, c: &Cached, rect: Rect, mode: Mode) {
    let th = app.theme.clone();
    let painter = ui.painter_at(rect);
    // Row A: tabs + arrange button + sort direction (controls sit in its lower 40 px).
    let row_a =
        Rect::from_min_size(pos2(rect.left(), rect.top() + HEADER_ROW - 40.0), vec2(rect.width(), 40.0));
    let (labels, active): ([&str; 2], Option<usize>) = if c.focused_tabs {
        (["Focused", "Other"], Some(if app.s.view.focused_tab { 0 } else { 1 }))
    } else {
        let active = match c.vs.filter {
            ListFilter::All => Some(0),
            ListFilter::Unread => Some(1),
            _ => None,
        };
        (["All", "Unread"], active)
    };
    let mut x = row_a.left() + 12.0;
    for (i, label) in labels.iter().enumerate() {
        let on = active == Some(i);
        let color = if on { th.text } else { th.text_muted };
        let t = Text::new(ui, label, 14.0, on, color, 120.0, 1);
        let tab = Rect::from_min_size(pos2(x - 4.0, row_a.top() + 6.0), vec2(t.width() + 8.0, 30.0));
        let resp = ui.interact(tab, Id::unique(("mailcraft.list.tab", i)), Sense::click());
        if resp.hovered() && !on {
            painter.rect_filled(tab, 3.0, th.hover);
        }
        t.paint_left(&painter, tab, x);
        if on {
            let y = tab.bottom() - 2.0;
            painter.line_segment([pos2(x, y), pos2(x + t.width(), y)], Stroke::new(2.0, th.accent));
        }
        resp.widget_info(|| WidgetInfo::selected(Role::Button, true, on, *label));
        if resp.clicked() && !on {
            if c.focused_tabs {
                let tab = if i == 0 { "focused" } else { "other" };
                run(ui, app, "view.focusedTab", json!({ "tab": tab }));
            } else {
                let kind = if i == 0 { "all" } else { "unread" };
                run(ui, app, "view.filter", json!({ "kind": kind }));
            }
        }
        x += t.width() + 20.0;
    }

    // Sort direction (right edge).
    let desc = c.vs.order == SortOrder::Descending;
    let sort_rect = Rect::from_min_size(pos2(row_a.right() - 32.0, row_a.top() + 8.0), vec2(24.0, 24.0));
    let sort_resp = ui.interact(sort_rect, Id::unique("mailcraft.list.sortdir"), Sense::click());
    if sort_resp.hovered() {
        painter.rect_filled(sort_rect, 3.0, th.hover);
    }
    // The observed default (Newest on top) shows ↑ here; the column header's arrow keeps ▼ for descending.
    let sort_icon = if desc { "sort-up" } else { "sort-down" };
    app.icons.paint(ui, icon_rect(sort_rect.center(), 16.0), sort_icon, th.text_muted);
    let sort_tip = match (c.vs.arrange_by, desc) {
        (ArrangeBy::Date, true) => "Newest on top",
        (ArrangeBy::Date, false) => "Oldest on top",
        (_, true) => "Sorted descending",
        (_, false) => "Sorted ascending",
    };
    sort_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, sort_tip));
    let sort_resp = sort_resp.on_hover_text(sort_tip);
    if sort_resp.clicked() {
        run(ui, app, "view.reverseSort", json!({}));
    }

    // "By Date ▾" arrange / filter / sort menu.
    let by = format!("By {}", c.vs.arrange_by.label());
    let t = Text::new(ui, &by, 13.0, false, th.text, 160.0, 1);
    let bw = t.width() + 30.0;
    let by_rect = Rect::from_min_size(pos2(sort_rect.left() - 4.0 - bw, row_a.top() + 8.0), vec2(bw, 24.0));
    let by_resp = ui.interact(by_rect, Id::unique("mailcraft.list.arrange"), Sense::click());
    if by_resp.hovered() {
        painter.rect_filled(by_rect, 3.0, th.hover);
    }
    let end = t.paint_left(&painter, by_rect, by_rect.left() + 8.0);
    app.icons.paint(ui, icon_rect(pos2(end + 10.0, by_rect.center().y), 12.0), "chevron-down", th.text_muted);
    by_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &by));
    egui::Popup::menu(&by_resp).show(|ui| crate::menus::arrange_menu(ui, app));

    // Row B: "Other: New messages (N)" banner (only while there is new Other mail).
    let row_b = Rect::from_min_size(
        pos2(rect.left(), row_a.bottom() + (BANNER_ROW - 30.0) / 2.0),
        vec2(rect.width(), 30.0),
    );
    if let Some((n, senders)) = &c.other_new {
        let resp = ui.interact(row_b, Id::unique("mailcraft.list.otherbanner"), Sense::click());
        if resp.hovered() {
            painter.rect_filled(row_b.shrink2(vec2(6.0, 1.0)), 3.0, th.hover);
        }
        let label = format!("Other: New messages ({n})");
        let t = Text::new(ui, &label, 12.5, true, th.unread, row_b.width() - 24.0, 1);
        let end = t.paint_left(&painter, row_b, row_b.left() + 12.0);
        if !senders.is_empty() {
            let rest = (row_b.right() - end - 20.0).max(1.0);
            let s = Text::new(ui, senders, 12.0, false, th.text_muted, rest, 1);
            s.paint_left(&painter, row_b, end + 8.0);
        }
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("{label} {senders}")));
        if resp.clicked() {
            run(ui, app, "view.focusedTab", json!({ "tab": "other" }));
        }
    }

    if let Some(notice) = &c.search_notice {
        let t = Text::new(ui, notice, 12.0, false, th.text_muted, row_b.width() - 24.0, 2);
        t.paint_left(&painter, row_b, row_b.left() + 12.0);
        let resp = ui.interact(row_b, Id::unique("mailcraft.list.searchnotice"), Sense::hover());
        resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, notice));
        resp.on_hover_text(notice);
    }

    // Row C: column headers in the column layout.
    if let Mode::Columns { .. } = mode {
        let h = COLUMN_HEADER.min(rect.height());
        let row_c = Rect::from_min_max(pos2(rect.left(), rect.bottom() - h), rect.max);
        column_header(ui, app, c, row_c);
    }
    painter.line_segment(
        [pos2(rect.left(), rect.bottom() - 0.5), pos2(rect.right(), rect.bottom() - 0.5)],
        Stroke::new(1.0, th.separator),
    );
}

fn outgoing(role: Option<FolderRole>) -> bool {
    matches!(role, Some(FolderRole::Drafts | FolderRole::SentItems | FolderRole::Outbox))
}

/// Id of a column's resize drag (memory holds the px dragged so far).
fn resize_id(col: &str) -> Id {
    Id::unique(("mailcraft.list.colsize", col))
}

fn column_header(ui: &mut Ui, app: &mut App, c: &Cached, row: Rect) {
    let th = app.theme.clone();
    let painter = ui.painter_at(row);
    painter.rect_filled(row, 0.0, th.group_header);
    let out = outgoing(c.role);
    let mut resized: Option<(&'static str, f32)> = None;
    for (i, x, w) in layout_columns(&c.columns, row.width()) {
        let Some(col) = c.columns.get(i) else { continue };
        let cell = Rect::from_min_size(pos2(row.left() + x, row.top()), vec2(w, row.height()));
        let resp = ui.interact(cell, Id::unique(("mailcraft.list.colhdr", col.id)), Sense::click());
        let arrange = column_arrange(col, out);
        if resp.hovered() && arrange.is_some() {
            painter.rect_filled(cell, 0.0, th.hover);
        }
        let sorted = arrange == Some(c.vs.arrange_by);
        let name = column_label(col, out);
        let mut right = cell.right() - 4.0;
        if sorted {
            let icon = if c.vs.order == SortOrder::Descending { "sort-down" } else { "sort-up" };
            if w > 40.0 {
                app.icons.paint(ui, icon_rect(pos2(right - 6.0, cell.center().y), 12.0), icon, th.text_muted);
                right -= 14.0;
            }
        }
        match column_icon(col) {
            Some(icon) => {
                let tint = if sorted { th.accent } else { th.text_muted };
                app.icons.paint(ui, icon_rect(cell.center(), 14.0), icon, tint);
            }
            None if col.id == "mention" && col.format == "icon" => {
                let t = Text::new(ui, "@", 12.0, false, th.text_muted, w, 1);
                t.paint_left(&painter, cell, cell.center().x - t.width() / 2.0);
            }
            None => {
                let room = (right - cell.left() - 6.0).max(1.0);
                let t = Text::new(ui, &name, 12.0, sorted, th.text_muted, room, 1);
                match col.align {
                    "right" => t.paint_right(&painter, cell, right),
                    "center" => t.paint_left(&painter, cell, cell.center().x - t.width() / 2.0),
                    _ => t.paint_left(&painter, cell, cell.left() + 6.0),
                };
            }
        }
        painter.line_segment(
            [pos2(cell.right() - 0.5, cell.top() + 5.0), pos2(cell.right() - 0.5, cell.bottom() - 5.0)],
            Stroke::new(1.0, th.separator),
        );
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
        let resp = resp.on_hover_text(&name);
        if resp.clicked()
            && let Some(a) = arrange
        {
            if sorted {
                run(ui, app, "view.reverseSort", json!({}));
            } else {
                run(
                    ui,
                    app,
                    "view.arrangeBy",
                    json!({ "key": mailcraft_engine::cmd::view::arrange_name(a) }),
                );
            }
        }
        resp.context_menu(|ui| crate::menus::column_header_menu(ui, app, col.id));

        // Right edge: drag to resize (committed as the column's width when released).
        let edge = Rect::from_center_size(pos2(cell.right(), cell.center().y), vec2(6.0, row.height()));
        let grip = ui.interact(edge, Id::unique(("mailcraft.list.colgrip", col.id)), Sense::drag());
        let grip = grip.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        let rid = resize_id(col.id);
        if grip.dragged() {
            let dx = grip.drag_delta().x;
            let total = ui.ctx().data_mut(|d| {
                let v = d.get_temp_mut_or_default::<f32>(rid);
                *v += dx;
                *v
            });
            let guide = (cell.right() + total).max(cell.left() + f32::from(mailview::MIN_WIDTH));
            ui.painter().vline(guide, row.y_range(), Stroke::new(2.0, th.accent));
        }
        if grip.drag_stopped() {
            let total = ui.ctx().data_mut(|d| d.remove_temp::<f32>(rid)).unwrap_or(0.0);
            if total.abs() >= 1.0 {
                resized = Some((col.id, (w + total).max(f32::from(mailview::MIN_WIDTH))));
            }
        }
    }
    if let Some((id, w)) = resized {
        run(ui, app, "view.columns", json!({ "widths": { id: w.round() } }));
    }
}

// -------------------------------------------------------------------------------------------------------------
// Body.

fn body(ui: &mut Ui, app: &mut App, c: &Cached, rect: Rect, mode: Mode, st: &mut ListState, frame: &Frame) {
    let th = app.theme.clone();
    let bg = ui.interact(rect, focus_id(), Sense::click());
    if bg.clicked() {
        bg.request_focus();
    }
    let focused = bg.has_focus();
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                focus_id(),
                EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: false },
            );
        });
    }
    let rows = c.rows.clone();
    if rows.is_empty() {
        let msg = if c.searching { "No results found." } else { "Nothing to show in this view." };
        let t = Text::new(ui, msg, 13.0, false, th.text_muted, rect.width() - 24.0, 2);
        t.paint(ui.painter(), pos2(rect.center().x - t.width() / 2.0, rect.top() + 32.0));
        bg.widget_info(|| WidgetInfo::labeled(Role::Unknown, true, "Message list, empty"));
        st.last_focus = app.s.selection.focus.map(|f| f.0);
        return;
    }
    bg.widget_info(|| WidgetInfo::labeled(Role::Unknown, true, "Message list"));

    let item_h = item_height(mode, c.vs.preview_lines, c.vs.tighter_spacing);
    let child_h = child_height(mode, c.vs.preview_lines, c.vs.tighter_spacing);
    let row_h = |r: &ListRow| match r {
        ListRow::Group { .. } => size::GROUP_HEADER,
        ListRow::Item(it) if it.conversation.as_ref().is_some_and(|c| !c.header) => child_h,
        ListRow::Item(_) => item_h,
    };
    let mut starts: Vec<f32> = Vec::with_capacity(rows.len());
    let mut y = 0.0_f32;
    for r in rows.iter() {
        starts.push(y);
        y += row_h(r);
    }
    let total = y;
    let height_of = |i: usize| rows.get(i).map_or(item_h, row_h);

    // Keyboard (before drawing so the scroll-into-view below sees the new focus next frame).
    if focused {
        keyboard(ui, app, c, st);
    }

    let focus_now = app.s.selection.focus.map(|f| f.0);
    let scroll_to = if focus_now != st.last_focus { focus_now } else { None };
    st.last_focus = focus_now;

    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    egui::ScrollArea::vertical().id_salt("mailcraft.list.scroll").auto_shrink([false, false]).show_viewport(
        &mut child,
        |ui, viewport| {
            ui.set_min_height(total);
            let origin = ui.max_rect().min;
            let width = ui.max_rect().width();
            st.page_rows = ((viewport.height() / item_h).floor() as usize).max(1);
            if let Some(target) = scroll_to
                && let Some(i) = rows.iter().position(|r| matches!(r, ListRow::Item(it) if it.id.0 == target))
                && let Some(top) = starts.get(i)
            {
                let r = Rect::from_min_size(origin + vec2(0.0, *top), vec2(width, height_of(i)));
                ui.scroll_to_rect(r, None);
            }
            let first = starts.partition_point(|s| *s + item_h.max(size::GROUP_HEADER) < viewport.min.y);
            for i in first..rows.len() {
                let Some(top) = starts.get(i).copied() else { break };
                if top > viewport.max.y {
                    break;
                }
                let r = Rect::from_min_size(origin + vec2(0.0, top), vec2(width, height_of(i)));
                match rows.get(i) {
                    Some(ListRow::Group { label, count, collapsed, depth }) => {
                        group_row(ui, app, r, label, *count, *collapsed, *depth);
                    }
                    Some(ListRow::Item(row)) => item_row(ui, app, c, r, row, mode, st, frame),
                    None => {}
                }
            }
        },
    );
}

fn keyboard(ui: &mut Ui, app: &mut App, c: &Cached, st: &ListState) {
    #[derive(Clone, Copy)]
    enum K {
        Next,
        Prev,
        First,
        Last,
        PageDown,
        PageUp,
        Open,
        Collapse,
        Expand,
    }
    let mut keys: Vec<(K, bool)> = Vec::new();
    ui.input_mut(|i| {
        for (key, k) in [
            (Key::ArrowDown, K::Next),
            (Key::ArrowUp, K::Prev),
            (Key::Home, K::First),
            (Key::End, K::Last),
            (Key::PageDown, K::PageDown),
            (Key::PageUp, K::PageUp),
        ] {
            while i.consume_key(Modifiers::SHIFT, key) {
                keys.push((k, true));
            }
            while i.consume_key(Modifiers::NONE, key) {
                keys.push((k, false));
            }
        }
        if i.consume_key(Modifiers::NONE, Key::Enter) {
            keys.push((K::Open, false));
        }
        if c.vs.conversations {
            if i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                keys.push((K::Collapse, false));
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowRight) {
                keys.push((K::Expand, false));
            }
        }
    });
    for (k, shift) in keys {
        match k {
            K::Next => run(ui, app, "nav.nextItem", json!({ "extend": shift })),
            K::Prev => run(ui, app, "nav.prevItem", json!({ "extend": shift })),
            K::First if shift => page(ui, app, c, isize::MIN / 2, true),
            K::Last if shift => page(ui, app, c, isize::MAX / 2, true),
            K::First => run(ui, app, "nav.firstItem", json!({})),
            K::Last => run(ui, app, "nav.lastItem", json!({})),
            K::PageDown => page(ui, app, c, isize::try_from(st.page_rows).unwrap_or(1), shift),
            K::PageUp => page(ui, app, c, -isize::try_from(st.page_rows).unwrap_or(1), shift),
            K::Open => {
                if app.s.selection.focus.is_some() {
                    run(ui, app, "mail.open", json!({}));
                }
            }
            K::Collapse | K::Expand => {
                let collapse = matches!(k, K::Collapse);
                let target = app.s.selection.focus.and_then(|f| queries::conversation_of(&app.s, f));
                if let Some(conv) = target
                    && conv.is_thread()
                    && conv.expanded == collapse
                {
                    let id = if collapse { "view.collapseGroup" } else { "view.expandGroup" };
                    run(ui, app, id, json!({ "conversation": true }));
                }
            }
        }
    }
}

/// Moves the focus `delta` visible items (clamped); `extend` selects the range from the anchor.
fn page(ui: &Ui, app: &mut App, c: &Cached, delta: isize, extend: bool) {
    let ids: Vec<u64> = c
        .rows
        .iter()
        .filter_map(|r| match r {
            // Non-mail results of an All Items search open their own windows; the keys skip them, as
            // `queries::visible_item_ids` does.
            ListRow::Item(i) if i.kind.is_none() => Some(i.id.0),
            ListRow::Item(_) | ListRow::Group { .. } => None,
        })
        .collect();
    // A conversation header and its first child are the same message.
    let mut seen = std::collections::BTreeSet::new();
    let ids: Vec<u64> = ids.into_iter().filter(|i| seen.insert(*i)).collect();
    if ids.is_empty() {
        return;
    }
    let cur = app.s.selection.focus.and_then(|f| ids.iter().position(|x| *x == f.0));
    let last = ids.len().saturating_sub(1);
    let target = match cur {
        None => 0,
        Some(i) => {
            let i = isize::try_from(i).unwrap_or(0);
            usize::try_from(i.saturating_add(delta).max(0)).unwrap_or(0).min(last)
        }
    };
    if let Some(id) = ids.get(target) {
        run(ui, app, "nav.selectItem", json!({ "id": id, "extend": extend }));
    }
}

/// Collapses or expands the group under header `label`. The group is named explicitly: under Arrange by
/// Categories a message sits in every group of its categories, so its focus alone does not say which header
/// was clicked. The commands need a selected item, so an item of the group is focused first when the focus
/// lies elsewhere.
fn toggle_group(ui: &Ui, app: &mut App, label: &str, collapsed: bool) {
    let ids = queries::group_items(&app.s, label);
    let Some(first) = ids.first() else { return };
    let focus_inside = app.s.selection.focus.is_some_and(|f| ids.contains(&f));
    if !focus_inside {
        run(ui, app, "nav.selectItem", json!({ "id": first.0 }));
    }
    let id = if collapsed { "view.expandGroup" } else { "view.collapseGroup" };
    run(ui, app, id, json!({ "label": label }));
}

#[allow(clippy::too_many_arguments)]
fn group_row(ui: &mut Ui, app: &mut App, r: Rect, label: &str, count: usize, collapsed: bool, depth: u8) {
    let th = app.theme.clone();
    let resp = ui.interact(r, Id::unique(("mailcraft.list.group", label)), Sense::click());
    let painter = ui.painter().clone();
    let fill = if resp.hovered() { th.hover } else { th.group_header };
    painter.rect_filled(r.shrink2(vec2(0.0, 1.0)), 0.0, fill);
    // A group nested by Group By is indented under the group around it and shows its own label.
    let indent = 16.0 * f32::from(depth.min(6));
    let title = queries::group_title(label);
    let chevron = if collapsed { "chevron-right" } else { "chevron-down" };
    app.icons.paint(
        ui,
        icon_rect(pos2(r.left() + 14.0 + indent, r.center().y), 12.0),
        chevron,
        th.text_muted,
    );
    let t = Text::new(ui, title, 12.5, true, th.text, (r.width() - 40.0 - indent).max(1.0), 1);
    t.paint_left(&painter, r, r.left() + 26.0 + indent);
    let state = if collapsed { "collapsed" } else { "expanded" };
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("{title}: {count} items, {state}")));
    if resp.clicked() {
        toggle_group(ui, app, label, collapsed);
    }
    resp.context_menu(|ui| crate::menus::group_header_menu(ui, app, label));
}

/// Where the clickable parts of a row ended up.
struct Slots {
    /// Flag column (none in a column layout without the Flag Status column).
    flag: Option<Rect>,
    /// Category column (applies the Quick Click category).
    category: Option<Rect>,
    /// Quick Action buttons shown while the row is hovered.
    actions: Vec<(&'static str, Rect)>,
}

/// Glyph and accessible name of a Quick Action button for `row`.
fn action_look(action: &str, row: &ItemRow) -> (&'static str, &'static str) {
    match action {
        "delete" => ("delete", "Delete"),
        "archive" => ("archive", "Archive"),
        "markRead" if row.unread => ("read", "Mark as Read"),
        "markRead" => ("unread", "Mark as Unread"),
        "move" => ("move", "Move"),
        "categorize" => ("categorize", "Categorize"),
        "pin" if row.pinned => ("pin", "Unpin"),
        "pin" => ("pin", "Pin"),
        _ => ("ellipsis", ""),
    }
}

/// Runs a Quick Action on message `id`.
fn run_action(ui: &Ui, app: &mut App, action: &str, id: u64) {
    let cmd = match action {
        "delete" => "mail.delete",
        "archive" => "mail.archive",
        "markRead" => "mail.toggleRead",
        "move" => "mail.move",
        // Without a name the Quick Click category is applied (or chosen first).
        "categorize" => "mail.categorize",
        "pin" => "mail.pin",
        _ => return,
    };
    run_on_items(ui, app, cmd, &[id], json!({}));
}

/// The flag column takes clicks while Flag / Unflag is one of the Quick Actions.
fn flag_active(c: &Cached) -> bool {
    c.quick.contains(&"flag")
}

/// Quick Action buttons (hovered rows) placed right to left from `right` on `band`; returns them and the left
/// edge they reached.
fn paint_actions(
    ui: &Ui,
    icons: &mut crate::icons::Icons,
    th: &Theme,
    c: &Cached,
    row: &ItemRow,
    band: Rect,
    mut right: f32,
) -> (Vec<(&'static str, Rect)>, f32) {
    let mut out = Vec::new();
    for a in c.quick {
        if a == "none" || a == "flag" {
            continue;
        }
        let r = Rect::from_min_size(pos2(right - 19.0, band.center().y - 10.0), vec2(19.0, 20.0));
        let (icon, _) = action_look(a, row);
        icons.paint(ui, icon_rect(r.center(), 14.0), icon, th.text_muted);
        out.push((a, r));
        right = r.left() - 2.0;
    }
    (out, right)
}

/// Category column blocks (up to three) inside `slot`; an outline tag while hovered without categories.
fn paint_categories(ui: &Ui, app: &mut App, row: &ItemRow, slot: Rect, hovered: bool) {
    let n = row.categories.len().min(3);
    if n == 0 {
        if hovered {
            app.icons.paint(ui, icon_rect(slot.center(), 13.0), "category-tag", app.theme.text_muted);
        }
        return;
    }
    let mut x = slot.right() - 6.0;
    for hex in row.categories.iter().take(n).rev() {
        let block = Rect::from_min_size(pos2(x - 5.0, slot.center().y - 6.0), vec2(5.0, 12.0));
        ui.painter().rect_filled(block, 1.5, hex_color(hex));
        x -= 7.0;
    }
}

/// Width of the category column of a compact row.
fn category_width(row: &ItemRow) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let n = row.categories.len().min(3) as f32;
    (n * 7.0 + 8.0).max(19.0)
}

/// Line-1 right end of compact rows: flag column, category column, then (hovered) the Quick Actions. Returns
/// the slots and where the status icons may end.
fn paint_row_end(
    ui: &Ui,
    app: &mut App,
    c: &Cached,
    row: &ItemRow,
    line1: Rect,
    right: f32,
    hovered: bool,
) -> (Slots, f32) {
    let th = app.theme.clone();
    let flag = Rect::from_min_size(pos2(right - 19.0, line1.center().y - 10.0), vec2(19.0, 20.0));
    paint_flag(ui, &mut app.icons, &th, row, flag, hovered && flag_active(c));
    let cw = category_width(row);
    let category = Rect::from_min_size(pos2(flag.left() - cw - 1.0, flag.top()), vec2(cw, 20.0));
    paint_categories(ui, app, row, category, hovered);
    let (actions, left) = if hovered {
        paint_actions(ui, &mut app.icons, &th, c, row, line1, category.left() - 2.0)
    } else {
        (Vec::new(), category.left())
    };
    (Slots { flag: Some(flag), category: Some(category), actions }, left)
}

/// Background of an item row: selected, hovered, the header of an expanded selected conversation (a light
/// tint; the child row carries the selection), then flagged rows when "Highlight flagged items" is on.
fn row_fill(
    th: &Theme,
    row: &ItemRow,
    hovered: bool,
    header_of_selection: bool,
    highlight_flagged: bool,
) -> Option<Color32> {
    if row.selected {
        Some(th.selected)
    } else if hovered {
        Some(th.hover)
    } else if header_of_selection {
        Some(th.selected.gamma_multiply(0.45))
    } else if row.flagged && highlight_flagged {
        Some(th.flagged_row)
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn item_row(
    ui: &mut Ui,
    app: &mut App,
    c: &Cached,
    r: Rect,
    row: &ItemRow,
    mode: Mode,
    st: &mut ListState,
    frame: &Frame,
) {
    let th = app.theme.clone();
    let id = row.id.0;
    // A conversation header shows the same message as its first child row: give it its own widget ids.
    // A message listed again (under each of its categories) gets its own ids too.
    let part: u32 =
        if row.conversation.as_ref().is_some_and(|c| c.header) { 1 } else { 0 } + 2 * u32::from(row.repeat);
    let resp = ui.interact(r, Id::unique(("mailcraft.list.row", id, part)), Sense::click_and_drag());
    let hovered = resp.contains_pointer() && !egui::DragAndDrop::has_any_payload(ui.ctx());
    // The hovered message for hover shortcuts; header rows leave it to the message's own (child) row.
    if part.is_multiple_of(2) && row.kind.is_none() {
        if hovered {
            app.ui.hover_row = Some(id);
        } else if app.ui.hover_row == Some(id) {
            app.ui.hover_row = None;
        }
    }
    let painter = ui.painter().clone();
    let conv = row.conversation.as_ref();
    let header_of_selection = conv.is_some_and(|c| c.header && c.contains_selection);
    let fill = row_fill(&th, row, hovered, header_of_selection, c.highlight_flagged);
    if let Some(fill) = fill {
        painter.rect_filled(r, 0.0, fill);
    }
    if row.unread {
        let bar = Rect::from_min_size(r.min + vec2(0.0, 1.0), vec2(size::UNREAD_BAR, r.height() - 2.0));
        painter.rect_filled(bar, 0.0, th.unread);
    }
    let (preview_lines, tighter) = (c.vs.preview_lines, c.vs.tighter_spacing);
    let (slots, glyph) = match (mode, conv) {
        (Mode::Compact, Some(cv)) if cv.header => {
            paint_conversation_header(ui, app, c, r, row, cv, hovered, preview_lines, tighter)
        }
        (Mode::Compact, Some(cv)) => {
            (paint_child(ui, app, c, r, row, cv, hovered, preview_lines, tighter), None)
        }
        (Mode::Compact, None) => {
            (paint_compact(ui, app, c, r, row, hovered, preview_lines, tighter, 0.0), None)
        }
        (Mode::Columns { preview }, _) => paint_columns(ui, app, c, r, row, hovered, preview, st),
    };
    painter.line_segment(
        [pos2(r.left() + 8.0, r.bottom() - 0.5), pos2(r.right(), r.bottom() - 0.5)],
        Stroke::new(0.5, th.separator),
    );

    // A non-mail result of an All Items search opens its own item window; message actions do not apply.
    if let Some(window) = row.kind.as_deref() {
        let accessible = row.accessible.trim_end_matches([',', ' ']).to_owned();
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &accessible));
        if resp.double_clicked() || (resp.clicked() && frame.single_pane) {
            crate::dialogs::open(app, window, json!({ "id": id }));
        }
        return;
    }

    // Quick actions sit on top of the row (added after it, so they take the click).
    if let (Some(g), Some(cv)) = (glyph, conv) {
        conversation_glyph(ui, app, g, row, cv);
    }
    let outline = |rect: Rect| {
        painter.rect_stroke(rect.shrink(1.0), 3.0, Stroke::new(1.0, th.separator), egui::StrokeKind::Inside);
    };
    if flag_active(c)
        && let Some(flag) = slots.flag
    {
        let flag_resp = ui.interact(flag, Id::unique(("mailcraft.list.flag", id, part)), Sense::click());
        let flag_tip = if row.flag_complete {
            "Clear flag"
        } else if row.flagged {
            "Mark complete"
        } else {
            "Flag this message"
        };
        if flag_resp.hovered() {
            outline(flag);
        }
        flag_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, flag_tip));
        if flag_resp.on_hover_text(flag_tip).clicked() {
            let params = if row.flag_complete { json!({ "kind": "clear" }) } else { json!({}) };
            run_on_items(ui, app, "mail.flag", &[id], params);
        }
    }
    if let Some(cat) = slots.category {
        let cat_resp = ui.interact(cat, Id::unique(("mailcraft.list.category", id, part)), Sense::click());
        if cat_resp.hovered() {
            outline(cat);
        }
        let label = if row.categories.is_empty() { "No Categories" } else { "Categories" };
        cat_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
        if cat_resp.on_hover_text("Quick Click category").clicked() {
            run_on_items(ui, app, "mail.categorize", &[id], json!({}));
        }
    }
    for (action, rect) in &slots.actions {
        let a_resp =
            ui.interact(*rect, Id::unique(("mailcraft.list.action", id, part, *action)), Sense::click());
        if a_resp.hovered() {
            outline(*rect);
        }
        let (_, label) = action_look(action, row);
        a_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
        if a_resp.on_hover_text(label).clicked() {
            run_action(ui, app, action, id);
        }
    }

    // Drag rows onto folders (pressing on an unselected row selects it, as a click would).
    if resp.drag_started() && !row.selected {
        run(ui, app, "nav.selectItem", json!({ "id": id }));
    }
    let dragged: Vec<u64> =
        if row.selected { app.s.selection.items.iter().map(|i| i.0).collect() } else { vec![id] };
    resp.dnd_set_drag_payload(DraggedItems(dragged));

    if resp.double_clicked() {
        run(ui, app, "mail.open", json!({ "ids": [id] }));
    } else if resp.clicked() {
        let m = ui.input(|i| i.modifiers);
        ui.memory_mut(|mem| mem.request_focus(focus_id()));
        run(
            ui,
            app,
            "nav.selectItem",
            json!({ "id": id, "toggle": m.command, "extend": m.shift && !m.command }),
        );
        // Clicking a conversation header selects the conversation, which shows it expanded (also after it
        // was collapsed by hand while selected).
        if conv.is_some_and(|cv| cv.header)
            && !m.command
            && !m.shift
            && queries::conversation_of(&app.s, row.id).is_some_and(|cv| cv.is_thread() && !cv.expanded)
        {
            run(ui, app, "view.expandGroup", json!({ "conversation": true, "id": id }));
        }
        if frame.single_pane && !m.command && !m.shift {
            app.ui.single_pane_reading = true;
        }
    }
    if resp.secondary_clicked() && !row.selected {
        run(ui, app, "nav.selectItem", json!({ "id": id }));
    }
    let accessible = row.accessible.trim_end_matches([',', ' ']).to_owned();
    resp.widget_info(|| WidgetInfo::selected(Role::Button, true, row.selected, &accessible));
    if conv.is_some_and(|cv| cv.header) {
        resp.context_menu(|ui| crate::menus::conversation_menu(ui, app, id));
    } else {
        resp.context_menu(|ui| crate::menus::message_row_menu(ui, app, id));
    }
}

/// ◢ (expanded) or ▷ (collapsed) conversation glyph centred at `c`.
fn paint_conversation_glyph(painter: &Painter, c: Pos2, expanded: bool, color: Color32) {
    let h = 4.0;
    if expanded {
        let pts = vec![pos2(c.x + h, c.y - h), pos2(c.x + h, c.y + h), pos2(c.x - h, c.y + h)];
        painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    } else {
        let pts = vec![
            pos2(c.x - h * 0.7, c.y - h * 1.1),
            pos2(c.x + h * 0.9, c.y),
            pos2(c.x - h * 0.7, c.y + h * 1.1),
        ];
        painter.add(egui::Shape::convex_polygon(pts, Color32::TRANSPARENT, Stroke::new(1.0, color)));
    }
}

/// The glyph of a header row toggles the conversation without changing the selection.
fn conversation_glyph(ui: &Ui, app: &mut App, g: Rect, row: &ItemRow, cv: &ConversationRow) {
    let resp = ui.interact(g, Id::unique(("mailcraft.list.convglyph", row.id.0)), Sense::click());
    let label = if cv.expanded { "Collapse conversation" } else { "Expand conversation" };
    if resp.hovered() {
        ui.painter().rect_stroke(
            g.shrink(1.0),
            3.0,
            Stroke::new(1.0, app.theme.separator),
            egui::StrokeKind::Inside,
        );
    }
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if resp.on_hover_text(label).clicked() {
        let id = if cv.expanded { "view.collapseGroup" } else { "view.expandGroup" };
        run(ui, app, id, json!({ "conversation": true, "id": row.id.0 }));
    }
}

/// Compact conversation header: glyph column, then the usual three lines with the senders on line 1 and
/// the subject (with the unread count) on line 2, or the other way round when Show Senders Above the
/// Subject is off. Returns the slots and the glyph's rect.
#[allow(clippy::too_many_arguments)]
fn paint_conversation_header(
    ui: &Ui,
    app: &mut App,
    c: &Cached,
    r: Rect,
    row: &ItemRow,
    cv: &ConversationRow,
    hovered: bool,
    preview_lines: u8,
    tighter: bool,
) -> (Slots, Option<Rect>) {
    let mut shown = row.clone();
    if cv.unread > 0 {
        shown.subject = format!("{} ({})", row.subject, cv.unread);
    }
    if !cv.senders_above_subject {
        std::mem::swap(&mut shown.who, &mut shown.subject);
    }
    let slots = paint_compact(ui, app, c, r, &shown, hovered, preview_lines, tighter, GLYPH_W);
    let pad = if tighter { 4.0 } else { 8.0 };
    let center = pos2(r.left() + 12.0 + GLYPH_W / 2.0 - 2.0, r.top() + pad + LINE1 / 2.0);
    paint_conversation_glyph(ui.painter(), center, cv.expanded, app.theme.text_muted);
    (slots, Some(Rect::from_center_size(center, vec2(18.0, 20.0))))
}

/// Compact child row of a conversation: sender (dimmed, with the folder name in italics, for messages
/// from other folders) and time on one line, the preview below; indented under the header.
#[allow(clippy::too_many_arguments)]
fn paint_child(
    ui: &Ui,
    app: &mut App,
    c: &Cached,
    r: Rect,
    row: &ItemRow,
    cv: &ConversationRow,
    hovered: bool,
    preview_lines: u8,
    tighter: bool,
) -> Slots {
    let th = app.theme.clone();
    let painter = ui.painter().clone();
    let pad = if tighter { 3.0 } else { 6.0 };
    let depth = row.conversation_depth.clamp(1, MAX_INDENT);
    let left = r.left() + 12.0 + GLYPH_W + INDENT * f32::from(depth);
    let right = r.right() - 8.0;
    let line1 = Rect::from_min_max(pos2(left, r.top() + pad), pos2(right, r.top() + pad + LINE1));
    let other = cv.other_folder.as_deref();
    let (slots, icons_left) = paint_row_end(ui, app, c, row, line1, right, hovered);
    let icons_left = paint_status_icons(ui, app, row, line1, icons_left - 2.0);
    let look = Look::of(&row.style);
    let time_color = rule_color(&row.style, if row.unread { th.unread } else { th.text_muted });
    let time = Text::styled(ui, &row.time, 12.0, look, time_color, 120.0, 1, None);
    let time_left = time.paint_right(&painter, line1, icons_left - 4.0);
    let who_color = if other.is_some() {
        th.text_muted
    } else {
        rule_color(&row.style, if row.unread { th.unread } else { th.text })
    };
    let mut x = left;
    if row.draft_marker {
        let t = Text::new(ui, "[Draft]", 13.5, false, th.draft, 60.0, 1);
        x = t.paint_left(&painter, line1, x) + 5.0;
    }
    let room = (time_left - x - 8.0).max(1.0);
    let who = Text::styled(
        ui,
        &row.who,
        13.5,
        look,
        who_color,
        room * if other.is_some() { 0.6 } else { 1.0 },
        1,
        None,
    );
    let end = who.paint_left(&painter, line1, x);
    if let Some(folder) = other {
        let t = Text::italic(ui, folder, 12.0, th.text_muted, (time_left - end - 14.0).max(1.0));
        t.paint_left(&painter, line1, end + 8.0);
    }
    let lines = usize::from(preview_lines.min(3));
    if lines > 0 && !row.preview.trim().is_empty() {
        let color = if other.is_some() { th.text_muted.gamma_multiply(0.8) } else { th.text_muted };
        let t = Text::new(ui, row.preview.trim(), 12.5, false, color, (right - left).max(1.0), lines);
        t.paint(&painter, pos2(left, line1.bottom() + 1.0));
    }
    slots
}

/// Download state glyph and its name: marked to download, or only the header downloaded.
fn download_icon(row: &ItemRow) -> Option<(&'static str, &'static str)> {
    match row.marked {
        Some(true) => Some(("sync", "Marked to download a copy")),
        Some(false) => Some(("sync", "Marked to download")),
        None if row.header_only => Some(("ellipsis", "Only the header is downloaded")),
        None => None,
    }
}

/// Line-1 status icons (right to left from `right`): importance, attachment, pin, download state.
fn paint_status_icons(ui: &Ui, app: &mut App, row: &ItemRow, band: Rect, mut right: f32) -> f32 {
    let th = app.theme.clone();
    let mut icon = |name: &str, color: Color32| {
        app.icons.paint(ui, icon_rect(pos2(right - 8.0, band.center().y), 14.0), name, color);
        right -= 17.0;
    };
    if row.importance_high {
        icon("importance-high", th.importance_high);
    } else if row.importance_low {
        icon("importance-low", th.importance_low);
    }
    if row.has_attachments {
        icon("paperclip", th.text_muted);
    }
    if row.pinned {
        icon("pin", th.accent);
    }
    if let Some((name, _)) = download_icon(row) {
        icon(name, if row.marked.is_some() { th.accent } else { th.text_muted });
    }
    right
}

/// Flag column glyph: flag, complete check, or (hovered) an outline to click.
fn paint_flag(
    ui: &Ui,
    icons: &mut crate::icons::Icons,
    th: &Theme,
    row: &ItemRow,
    slot: Rect,
    hovered: bool,
) {
    let c = slot.center();
    if row.flag_complete {
        icons.paint(ui, icon_rect(c, 14.0), "flag-complete", th.complete);
    } else if row.flagged {
        icons.paint(ui, icon_rect(c, 14.0), "flag", th.flag);
    } else if hovered {
        icons.paint(ui, icon_rect(c, 14.0), "flag", th.text_muted);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_compact(
    ui: &Ui,
    app: &mut App,
    c: &Cached,
    r: Rect,
    row: &ItemRow,
    hovered: bool,
    preview_lines: u8,
    tighter: bool,
    extra_left: f32,
) -> Slots {
    let th = app.theme.clone();
    let painter = ui.painter().clone();
    let pad = if tighter { 4.0 } else { 8.0 };
    let left = r.left() + 12.0 + extra_left;
    let right = r.right() - 8.0;
    let line1 = Rect::from_min_max(pos2(left, r.top() + pad), pos2(right, r.top() + pad + LINE1));
    let line2 = Rect::from_min_max(pos2(left, line1.bottom()), pos2(right, line1.bottom() + LINE));
    let look = Look::of(&row.style);

    // Line 1: [Draft] sender … status icons, Quick Actions (hover), categories, flag.
    let (slots, icons_left) = paint_row_end(ui, app, c, row, line1, right, hovered);
    let icons_left = paint_status_icons(ui, app, row, line1, icons_left - 2.0);
    let mut x = left;
    if row.draft_marker {
        let t = Text::new(ui, "[Draft]", 14.0, false, th.draft, 60.0, 1);
        x = t.paint_left(&painter, line1, x) + 5.0;
    }
    let searching = !app.s.view.search.trim().is_empty() && crate::dialogs::search_highlight_on(&app.s);
    let hl = |field: Field| searching.then_some((&app.s, field, th.search_hit));
    let who_look = Look { bold: false, ..look };
    let who = Text::styled(
        ui,
        &row.who,
        14.5,
        who_look,
        rule_color(&row.style, th.text),
        (icons_left - x - 6.0).max(1.0),
        1,
        hl(Field::From),
    );
    who.paint_left(&painter, line1, x);

    // Line 2: subject … reply/forward state, time.
    let time_color = rule_color(&row.style, if row.unread { th.unread } else { th.text_muted });
    let time = Text::styled(ui, &row.time, 12.0, Look::plain(look.bold), time_color, 120.0, 1, None);
    let mut sub_right = time.paint_right(&painter, line2, right) - 6.0;
    let state_icon = if row.replied {
        Some("replied")
    } else if row.forwarded {
        Some("forwarded")
    } else {
        None
    };
    if let Some(icon) = state_icon {
        app.icons.paint(ui, icon_rect(pos2(sub_right - 7.0, line2.center().y), 13.0), icon, th.text_muted);
        sub_right -= 18.0;
    }
    let subject_color = rule_color(&row.style, if row.unread { th.unread } else { th.text });
    let subject = Text::styled(
        ui,
        &row.subject,
        13.0,
        look,
        subject_color,
        (sub_right - left).max(1.0),
        1,
        hl(Field::Subject),
    );
    subject.paint_left(&painter, line2, left);

    // Lines 3..: preview.
    let lines = usize::from(preview_lines.min(3));
    if lines > 0 && !row.preview.trim().is_empty() {
        let t = Text::highlighted(
            ui,
            row.preview.trim(),
            12.5,
            false,
            th.text_muted,
            (right - left).max(1.0),
            lines,
            hl(Field::Body),
        );
        t.paint(&painter, pos2(left, line2.bottom() + 1.0));
    }
    slots
}

/// Paints one cell's text with the column's alignment.
fn paint_cell_text(painter: &Painter, t: &Text, cell: Rect, align: &str) {
    match align {
        "right" => {
            t.paint_right(painter, cell, cell.right() - 6.0);
        }
        "center" => {
            t.paint_left(painter, cell, cell.center().x - t.width() / 2.0);
        }
        _ => {
            t.paint_left(painter, cell, cell.left() + 6.0);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_columns(
    ui: &Ui,
    app: &mut App,
    c: &Cached,
    r: Rect,
    row: &ItemRow,
    hovered: bool,
    preview: u8,
    st: &mut ListState,
) -> (Slots, Option<Rect>) {
    let th = app.theme.clone();
    let painter = ui.painter().clone();
    let line_h = if c.vs.tighter_spacing { COL_ROW - 4.0 } else { COL_ROW };
    let line = Rect::from_min_size(r.min, vec2(r.width(), line_h));
    let mail = app.s.mailbox.mail.get(row.id);
    let conv = row.conversation.as_ref();
    let header = conv.is_some_and(|cv| cv.header);
    let other = conv.is_some_and(|cv| cv.other_folder.is_some());
    let indent = conv
        .filter(|cv| !cv.header)
        .map_or(0.0, |_| INDENT * f32::from(row.conversation_depth.clamp(1, MAX_INDENT)));
    let look = Look::of(&row.style);
    let text_color = if other {
        th.text_muted
    } else {
        rule_color(&row.style, if row.unread { th.unread } else { th.text })
    };
    let muted = rule_color(&row.style, th.text_muted);
    let mut glyph = None;
    let mut slots = Slots { flag: None, category: None, actions: Vec::new() };
    let layout = layout_columns(&c.columns, r.width());
    // Quick Actions go at the right end of Subject, else of the first column sharing the free space.
    let action_col = c
        .columns
        .iter()
        .position(|x| x.id == "subject")
        .filter(|i| layout.iter().any(|(j, _, _)| j == i))
        .or_else(|| {
            layout
                .iter()
                .map(|(i, _, _)| *i)
                .find(|i| c.columns.get(*i).is_some_and(|x| x.default_width == 0))
        });
    let mut glyph_done = false;
    for (i, x, w) in layout.iter().copied() {
        let Some(col) = c.columns.get(i) else { continue };
        let cell = Rect::from_min_size(pos2(r.left() + x, line.top()), vec2(w, line_h));
        let inner_w = (w - 10.0).max(1.0);
        let mut right = cell.right() - 4.0;
        if hovered && action_col == Some(i) {
            let (actions, left) = paint_actions(ui, &mut app.icons, &th, c, row, cell, right);
            slots.actions = actions;
            right = left;
        }
        let text_cell = Rect::from_min_max(cell.min, pos2(right.max(cell.left() + 1.0), cell.max.y));
        // The conversation glyph takes the first narrow icon column (Importance by default).
        if header && !glyph_done && col.kind == ColumnKind::Icon {
            glyph_done = true;
            let center = cell.center();
            paint_conversation_glyph(&painter, center, conv.is_some_and(|cv| cv.expanded), th.text_muted);
            glyph = Some(Rect::from_center_size(center, vec2(18.0, line_h)));
            continue;
        }
        let text =
            |id: &str| mail.map(|m| mailview::cell_text(&app.s, m, id, col.format)).unwrap_or_default();
        if col.kind == ColumnKind::Icon && col.format == "text" {
            let t = Text::styled(ui, &text(col.id), 12.5, look, muted, inner_w, 1, None);
            paint_cell_text(&painter, &t, text_cell, col.align);
            if col.id == "flagStatus" {
                slots.flag = Some(cell);
            }
            continue;
        }
        match col.id {
            "importance" => {
                if row.importance_high {
                    app.icons.paint(
                        ui,
                        icon_rect(cell.center(), 14.0),
                        "importance-high",
                        th.importance_high,
                    );
                } else if row.importance_low {
                    app.icons.paint(ui, icon_rect(cell.center(), 14.0), "importance-low", th.importance_low);
                }
            }
            "reminder" => {
                let reminder =
                    mail.is_some_and(|m| matches!(m.flag, FlagState::Flagged { reminder: Some(_), .. }));
                if reminder {
                    app.icons.paint(ui, icon_rect(cell.center(), 13.0), "bell", th.text_muted);
                }
            }
            "icon" => {
                let (icon, tint) = if let Some((name, _)) = download_icon(row) {
                    (name, if row.marked.is_some() { th.accent } else { th.text_muted })
                } else if row.draft_marker || mail.is_some_and(|m| m.is_draft) {
                    ("draft", th.text_muted)
                } else if row.replied {
                    ("replied", th.text_muted)
                } else if row.forwarded {
                    ("forwarded", th.text_muted)
                } else if row.unread {
                    ("mail", th.text_muted)
                } else {
                    ("envelope-open", th.text_muted)
                };
                app.icons.paint(ui, icon_rect(cell.center(), 14.0), icon, tint);
            }
            "attachment" => {
                if row.has_attachments {
                    app.icons.paint(ui, icon_rect(cell.center(), 14.0), "paperclip", th.text_muted);
                }
            }
            "mention" => {
                let mentioned = match st.mentions.get(&row.id.0) {
                    Some(m) => *m,
                    None => {
                        let m = mail.is_some_and(|m| queries::mentions_me(&app.s, m));
                        st.mentions.insert(row.id.0, m);
                        m
                    }
                };
                if mentioned {
                    let t = Text::new(ui, "@", 13.0, true, th.accent, w, 1);
                    t.paint_left(&painter, cell, cell.center().x - t.width() / 2.0);
                }
            }
            "flagStatus" => {
                slots.flag = Some(cell);
                paint_flag(ui, &mut app.icons, &th, row, cell, hovered && flag_active(c));
            }
            "from" => {
                let mut x = cell.left() + 6.0 + indent;
                if row.draft_marker {
                    let t = Text::new(ui, "[Draft]", 13.0, false, th.draft, inner_w, 1);
                    x = t.paint_left(&painter, cell, x) + 4.0;
                }
                if row.pinned {
                    app.icons.paint(ui, icon_rect(pos2(x + 6.0, cell.center().y), 12.0), "pin", th.accent);
                    x += 15.0;
                }
                // Name format keeps the row's own text (conversation senders, "(unknown sender)").
                let who = if col.format == "name" { row.who.clone() } else { text("from") };
                let color = if other { th.text_muted } else { rule_color(&row.style, th.text) };
                let t = Text::styled(
                    ui,
                    &who,
                    13.0,
                    look,
                    color,
                    (text_cell.right() - x - 4.0).max(1.0),
                    1,
                    None,
                );
                t.paint_left(&painter, cell, x);
            }
            "subject" => {
                let subject = match conv {
                    Some(cv) if cv.header && cv.unread > 0 => format!("{} ({})", row.subject, cv.unread),
                    _ => row.subject.clone(),
                };
                let room = (text_cell.right() - cell.left() - 6.0).max(1.0);
                let t = Text::styled(ui, &subject, 13.0, look, text_color, room, 1, None);
                let end = match col.align {
                    "right" | "center" => {
                        paint_cell_text(&painter, &t, text_cell, col.align);
                        text_cell.right()
                    }
                    _ => t.paint_left(&painter, cell, cell.left() + 6.0),
                };
                if let Some(folder) = conv.and_then(|cv| cv.other_folder.as_deref()) {
                    let t = Text::italic(ui, folder, 12.0, th.text_muted, (right - end - 10.0).max(1.0));
                    t.paint_left(&painter, cell, end + 8.0);
                }
            }
            "received" => {
                let shown = if col.format == "bestFit" { row.time.clone() } else { text("received") };
                let t = Text::styled(ui, &shown, 12.5, look, muted, inner_w, 1, None);
                paint_cell_text(&painter, &t, text_cell, col.align);
            }
            "categories" => {
                slots.category = Some(cell);
                let mut x = cell.left() + 6.0;
                for hex in row.categories.iter().take(3) {
                    let block = Rect::from_min_size(pos2(x, cell.center().y - 6.0), vec2(8.0, 12.0));
                    painter.rect_filled(block, 2.0, hex_color(hex));
                    x += 11.0;
                }
                let names = text("categories");
                if !names.is_empty() {
                    let t = Text::new(
                        ui,
                        &names,
                        12.0,
                        false,
                        th.text_muted,
                        (cell.right() - x - 4.0).max(1.0),
                        1,
                    );
                    t.paint_left(&painter, cell, x + 2.0);
                } else if hovered {
                    app.icons.paint(
                        ui,
                        icon_rect(pos2(x + 6.0, cell.center().y), 13.0),
                        "category-tag",
                        th.text_muted,
                    );
                }
            }
            id => {
                let t = Text::styled(ui, &text(id), 12.5, look, muted, inner_w, 1, None);
                paint_cell_text(&painter, &t, text_cell, col.align);
            }
        }
    }
    if preview > 0 && !row.preview.trim().is_empty() {
        // The preview line starts under the first column sharing the free space (From by default).
        let from_x = layout
            .iter()
            .find(|(i, _, _)| c.columns.get(*i).is_some_and(|x| x.default_width == 0))
            .map_or(76.0, |(_, x, _)| *x);
        let left = r.left() + from_x + 6.0;
        let color = if row.unread { th.unread } else { th.text_muted };
        let t = Text::new(
            ui,
            row.preview.trim(),
            12.5,
            false,
            color,
            (r.right() - left - 12.0).max(1.0),
            usize::from(preview),
        );
        t.paint(&painter, pos2(left, line.bottom() + 1.0));
    }
    (slots, glyph)
}

/// "Move N items" chip under the pointer while rows are dragged.
fn drag_preview(ui: &Ui, app: &App) {
    let ctx = ui.ctx();
    let Some(payload) = egui::DragAndDrop::payload::<DraggedItems>(ctx) else { return };
    let Some(pos) = ctx.pointer_latest_pos() else { return };
    let copy = ctx.input(|i| i.modifiers.command);
    let n = payload.0.len();
    let verb = if copy { "Copy" } else { "Move" };
    let label = if n == 1 { format!("{verb} 1 item") } else { format!("{verb} {n} items") };
    let painter =
        ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::unique("mailcraft.list.dragchip")));
    let galley = painter.layout_no_wrap(label, FontId::proportional(12.5), app.theme.text);
    let rect = Rect::from_min_size(pos + vec2(14.0, 10.0), galley.size() + vec2(16.0, 8.0));
    painter.rect_filled(rect, 4.0, app.theme.pane);
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, app.theme.separator), egui::StrokeKind::Inside);
    painter.galley(rect.min + vec2(8.0, 4.0), galley, app.theme.text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn highlight_job_marks_hits_and_skips_bad_ranges() {
        let format = TextFormat::default();
        let bg = Color32::YELLOW;
        let text = "Café budget review".to_owned();
        // "budget" (6..12), a range inside "é" (off a char boundary), an overlap and one past the end.
        let job = highlight_job(text.clone(), &format, &[(6, 12), (4, 5), (8, 10), (15, 99)], bg);
        assert_eq!(job.text, text);
        let marked: Vec<&str> = job
            .sections
            .iter()
            .filter(|s| s.format.background == bg)
            .filter_map(|s| job.text.get(s.byte_range.start.0..s.byte_range.end.0))
            .collect();
        assert_eq!(marked, vec!["budget"]);
    }
    use mailcraft_engine::sample::sample_session;

    fn harness(app: App, size: egui::Vec2) -> Harness<'static, App> {
        crate::test_harness().with_size(size).build_ui_state(
            |ui, app: &mut App| {
                let frame = Frame::compute(1600.0, 900.0);
                message_list(ui, app, &frame);
            },
            app,
        )
    }

    fn item_ids(app: &App) -> Vec<(u64, String)> {
        queries::list_rows(&app.s)
            .into_iter()
            .filter_map(|r| match r {
                ListRow::Item(i) => Some((i.id.0, i.subject)),
                ListRow::Group { .. } => None,
            })
            .collect()
    }

    #[test]
    fn renders_sample_subjects_and_groups() {
        let app = App::new(sample_session());
        let subjects = item_ids(&app);
        assert!(!subjects.is_empty());
        let mut h = harness(app, vec2(420.0, 1400.0));
        h.run();
        for (_, subject) in subjects.iter().take(5) {
            assert!(
                h.query_all_by_label_contains(&format!("Subject {subject}")).next().is_some(),
                "row for {subject:?} is in the accessibility tree"
            );
        }
        assert!(h.query_all_by_label_contains("Today: ").next().is_some(), "Today group header");
        assert!(h.query_by_label("Focused").is_some());
        assert!(h.query_by_label("Other").is_some());
        assert!(h.query_by_label("By Date").is_some());
    }

    #[test]
    fn click_selects_and_focus_tab_switches() {
        let app = App::new(sample_session());
        let (id, subject) = item_ids(&app).into_iter().nth(1).unwrap();
        let mut h = harness(app, vec2(420.0, 1400.0));
        h.run();
        h.query_all_by_label_contains(&format!("Subject {subject}")).next().unwrap().click();
        h.run();
        assert_eq!(h.state().s.selection.focus.map(|f| f.0), Some(id));
        h.get_by_label("Other").click();
        h.run();
        assert!(!h.state().s.view.focused_tab);
    }

    #[test]
    fn group_header_click_collapses() {
        let app = App::new(sample_session());
        let mut h = harness(app, vec2(420.0, 1400.0));
        h.run();
        h.query_all_by_label_contains("Today: ").next().unwrap().click();
        h.run();
        let rows = queries::list_rows(&h.state().s);
        assert!(
            matches!(rows.first(), Some(ListRow::Group { label, collapsed: true, .. }) if label == "Today")
        );
        h.query_all_by_label_contains("Today: ").next().unwrap().click();
        h.run();
        let rows = queries::list_rows(&h.state().s);
        assert!(matches!(rows.first(), Some(ListRow::Group { collapsed: false, .. })));
    }

    #[test]
    fn category_header_click_collapses_that_category() {
        let mut s = sample_session();
        let _ = s.run("view.focusedInbox", &json!({"on": false}));
        let inbox = s.selection.folder;
        let ids: Vec<mailcraft_model::ItemId> = s.mailbox.items_in(inbox).iter().map(|m| m.id).collect();
        let newest = s.mailbox.items_in(inbox).iter().map(|m| m.received.unix).max().unwrap();
        for id in &ids {
            s.mailbox.mail.get_mut(*id).unwrap().categories.clear();
        }
        // The newest message is in Red and Blue, so it is the first item under the Red header too.
        let both = ids[0];
        let m = s.mailbox.mail.get_mut(both).unwrap();
        m.categories = vec!["Red category".into(), "Blue category".into()];
        m.received.unix = newest + 60;
        s.mailbox.mail.get_mut(ids[1]).unwrap().categories = vec!["Red category".into()];
        s.run("view.arrangeBy", &json!({"key": "categories"})).unwrap();
        let _ = s.run("view.sort", &json!({"order": "ascending"}));
        assert_eq!(queries::group_items(&s, "Red category").first(), Some(&both));
        let collapsed = |s: &Session, name: &str| {
            queries::list_rows(s)
                .into_iter()
                .any(|r| matches!(r, ListRow::Group { label, collapsed: true, .. } if label == name))
        };
        let mut h = harness(App::new(s), vec2(420.0, 1400.0));
        h.run();
        h.query_all_by_label_contains("Red category: ").next().unwrap().click();
        h.run();
        assert!(collapsed(&h.state().s, "Red category"), "the clicked header collapses");
        assert!(!collapsed(&h.state().s, "Blue category"), "the other category stays open");
        h.query_all_by_label_contains("Red category: ").next().unwrap().click();
        h.run();
        assert!(!collapsed(&h.state().s, "Red category"));
        // The engine checks a named group exists.
        assert!(h.state_mut().s.run("view.collapseGroup", &json!({"label": "No such group"})).is_err());
    }

    #[test]
    fn visible_ids_are_built_once_per_list_change() {
        // Read windows ask for the list order every frame; it is only built again when the list changes.
        let mut s = sample_session();
        s.fixed_now = Some(s.now());
        let ctx = egui::Context::default();
        let first = visible_ids(&ctx, &s);
        assert_eq!(*first, queries::visible_item_ids(&s));
        assert!(Arc::ptr_eq(&first, &visible_ids(&ctx, &s)), "the same list is not built again");
        s.run("search.query", &json!({"text": "zzz-no-such-word"})).unwrap();
        let searched = visible_ids(&ctx, &s);
        assert!(!Arc::ptr_eq(&first, &searched));
        assert_eq!(*searched, queries::visible_item_ids(&s));
    }

    #[test]
    fn page_keys_stay_on_mail_in_an_all_items_search() {
        let mut s = sample_session();
        let subject = s.mailbox.appointments.values().next().map(|a| a.subject.clone()).unwrap();
        let word = subject.split_whitespace().next().unwrap().to_owned();
        s.run("search.query", &json!({"text": word})).unwrap();
        s.run("search.scope", &json!({"scope": "allItems"})).unwrap();
        let rows = queries::list_rows(&s);
        let last_row_is_other = rows.iter().rev().find_map(|r| match r {
            ListRow::Item(i) => Some(i.kind.is_some()),
            ListRow::Group { .. } => None,
        });
        assert_eq!(last_row_is_other, Some(true), "non-mail hits are listed last");
        let mail = queries::visible_item_ids(&s);
        let first = *mail.first().expect("the search finds mail too");
        let folder = s.selection.folder;
        let mut h = harness(App::new(s), vec2(420.0, 1400.0));
        h.run();
        h.state_mut().run("nav.selectItem", json!({"id": first.0}));
        let subject = h.state().s.mailbox.mail.get(first).unwrap().subject.clone();
        h.query_all_by_label_contains(&format!("Subject {subject}")).next().unwrap().click();
        h.run();
        for (mods, key) in [
            (Modifiers::SHIFT, Key::End),
            (Modifiers::NONE, Key::PageDown),
            (Modifiers::SHIFT, Key::PageDown),
        ] {
            h.key_press_modifiers(mods, key);
            h.run();
            let st = &h.state().s;
            assert_eq!(st.selection.folder, folder, "{key:?} keeps the search results");
            let focus = st.selection.focus.unwrap();
            assert!(st.mailbox.mail.contains(focus), "{key:?} stays on mail");
            assert_eq!(Some(&focus), mail.last());
        }
    }

    #[test]
    fn column_layout_and_other_folders() {
        let mut s = sample_session();
        s.run("view.readingPane", &json!({"position": "bottom"})).unwrap();
        let app = App::new(s);
        let subjects = item_ids(&app);
        let mut h = harness(app, vec2(900.0, 900.0));
        h.run();
        assert!(h.query_by_label("Subject").is_some(), "column header");
        assert!(h.query_by_label("Received").is_some());
        let (_, first) = subjects.first().unwrap();
        assert!(h.query_all_by_label_contains(&format!("Subject {first}")).next().is_some());
        h.get_by_label("Subject").click();
        h.run();
        let folder = h.state().s.selection.folder;
        assert_eq!(queries::view_settings(&h.state().s, folder).arrange_by, ArrangeBy::Subject);
        // Drafts: All / Unread tabs, recipients.
        let drafts = {
            let s = &h.state().s;
            s.mailbox.role_folder(s.current_account().unwrap(), FolderRole::Drafts).unwrap()
        };
        h.state_mut().run("nav.selectFolder", json!({"folder": drafts.0}));
        h.run();
        assert!(h.query_by_label("All").is_some());
        assert!(h.query_by_label("Unread").is_some());
    }

    #[test]
    fn keyboard_moves_focus_when_list_focused() {
        let app = App::new(sample_session());
        let ids = item_ids(&app);
        let mut h = harness(app, vec2(420.0, 900.0));
        h.run();
        let (_, first) = ids.first().unwrap();
        h.query_all_by_label_contains(&format!("Subject {first}")).next().unwrap().click();
        h.run();
        assert!(has_focus(&h.ctx));
        h.key_press(Key::ArrowDown);
        h.run();
        assert_eq!(h.state().s.selection.focus.map(|f| f.0), ids.get(1).map(|x| x.0));
        h.key_press(Key::End);
        h.run();
        assert_eq!(h.state().s.selection.focus.map(|f| f.0), ids.last().map(|x| x.0));
        h.key_press(Key::Home);
        h.run();
        assert_eq!(h.state().s.selection.focus.map(|f| f.0), ids.first().map(|x| x.0));
    }

    #[test]
    fn many_items_render_virtualised() {
        let mut s = sample_session();
        let inbox = s.selection.folder;
        let template = s
            .mailbox
            .items_in(inbox)
            .into_iter()
            .find(|m| m.focus == FocusClass::Focused)
            .map(|m| (*m).clone())
            .unwrap();
        let now = s.now().unix;
        let base = s.mailbox.mail.ids().map(|k| k.0).max().unwrap_or(0) + 1;
        for i in 0..3000u64 {
            let mut m = template.clone();
            m.id = mailcraft_model::ItemId(base + i);
            m.subject = format!("Bulk message {i}");
            m.received.unix = now - 60 - i64::try_from(i).unwrap() * 3600;
            s.mailbox.mail.insert(m.id, m);
        }
        let app = App::new(s);
        let mut h = harness(app, vec2(420.0, 800.0));
        h.run();
        let rows = h.query_all_by_label_contains("Subject Bulk message").count();
        assert!(rows > 0 && rows < 40, "only visible rows are built ({rows})");
        let t = std::time::Instant::now();
        for _ in 0..20 {
            h.step();
        }
        let per_frame = t.elapsed() / 20;
        assert!(per_frame < std::time::Duration::from_millis(250), "frame took {per_frame:?}");
    }

    #[test]
    fn header_block_heights() {
        let mut s = sample_session();
        let inbox = build(&s, 0);
        let expected = if inbox.other_new.is_some() { size::LIST_HEADER } else { HEADER_ROW };
        assert_eq!(header_height(&inbox, Mode::Compact), expected, "97 px only with the Other banner");
        let drafts = s.mailbox.role_folder(s.current_account().unwrap(), FolderRole::Drafts).unwrap();
        s.run("nav.selectFolder", &json!({"folder": drafts.0})).unwrap();
        let c = build(&s, 0);
        assert!(c.other_new.is_none());
        assert_eq!(header_height(&c, Mode::Compact), HEADER_ROW, "no folder title row");
        assert_eq!(header_height(&c, Mode::Columns { preview: 0 }), HEADER_ROW + COLUMN_HEADER);
    }

    #[test]
    fn bottom_reading_pane_uses_columns_with_preview_line() {
        let mut s = sample_session();
        s.run("view.readingPane", &json!({"position": "bottom"})).unwrap();
        let c = build(&s, 0);
        let lines = c.vs.preview_lines.min(3);
        assert!(lines > 0, "sample view previews one line");
        assert_eq!(mode_for(&c, 1200.0), Mode::Columns { preview: lines });
        s.run("view.readingPane", &json!({"position": "right"})).unwrap();
        assert_eq!(mode_for(&build(&s, 0), 300.0), Mode::Compact);
    }

    #[test]
    fn helpers() {
        assert_eq!(item_height(Mode::Compact, 1, false), 72.0);
        assert_eq!(item_height(Mode::Compact, 0, false), 54.0);
        assert_eq!(item_height(Mode::Columns { preview: 0 }, 1, false), 24.0);
        let s = sample_session();
        let specs = mailview::columns(&s, s.selection.folder);
        let cols = layout_columns(&specs, 900.0);
        assert_eq!(cols.len(), 11);
        assert!(cols.windows(2).all(|w| w[0].1 < w[1].1));
        assert_eq!(layout_columns(&specs, 400.0).len(), 7, "a narrow list keeps the essential columns");
        // From stays narrow next to Subject; Subject takes the rest.
        let width_of = |id: &str| {
            cols.iter().find(|(i, _, _)| specs.get(*i).is_some_and(|c| c.id == id)).map(|x| x.2).unwrap()
        };
        assert!(width_of("from") < width_of("subject"));
        assert!(layout_columns(&[], 500.0).is_empty());
        assert_eq!(hex_color("#ff0000"), Color32::from_rgb(255, 0, 0));
        assert_eq!(hex_color("junk"), Color32::GRAY);
    }

    // ---- View settings: columns, Quick Actions, Quick Click, pins ------------------------------------------

    #[test]
    fn chosen_columns_drive_the_column_layout() {
        let mut s = sample_session();
        s.run("view.readingPane", &json!({"position": "bottom"})).unwrap();
        s.run("view.columns", &json!({"columns": ["subject", "cc", "size"]})).unwrap();
        s.run("view.formatColumns", &json!({"column": "size", "label": "Bytes", "format": "bytes"})).unwrap();
        let app = App::new(s);
        let mut h = harness(app, vec2(900.0, 700.0));
        h.run();
        assert!(h.query_by_label("Subject").is_some());
        assert!(h.query_by_label("Cc").is_some());
        assert!(h.query_by_label("Bytes").is_some(), "Format Columns label");
        assert!(h.query_by_label("Received").is_none(), "a column left out is not drawn");
        // Clicking a header still arranges by it.
        h.get_by_label("Bytes").click();
        h.run();
        let folder = h.state().s.selection.folder;
        assert_eq!(queries::view_settings(&h.state().s, folder).arrange_by, ArrangeBy::Size);
    }

    #[test]
    fn hover_buttons_are_the_saved_quick_actions() {
        let mut s = sample_session();
        s.run("view.quickActions", &json!({"actions": ["pin", "archive"]})).unwrap();
        let app = App::new(s);
        let (id, subject) = item_ids(&app).into_iter().next().unwrap();
        let mut h = harness(app, vec2(420.0, 1400.0));
        h.run();
        h.query_all_by_label_contains(&format!("Subject {subject}")).next().unwrap().hover();
        h.run();
        assert!(h.query_by_label("Archive").is_some());
        assert!(h.query_by_label("Delete").is_none(), "Delete is not a Quick Action now");
        assert!(h.query_by_label("Flag this message").is_none(), "flag clicks need the Flag Quick Action");
        h.get_by_label("Pin").click();
        h.run();
        assert!(mailview::pinned(&h.state().s).contains(&mailcraft_model::ItemId(id)));
        assert!(h.query_all_by_label_contains("Pinned").next().is_some(), "pinned group and row");
    }

    #[test]
    fn category_column_applies_the_quick_click_category() {
        let app = App::new(sample_session());
        let target = item_ids(&app)
            .into_iter()
            .map(|(id, _)| id)
            .find(|id| {
                app.s.mailbox.mail.get(mailcraft_model::ItemId(*id)).is_some_and(|m| m.categories.is_empty())
            })
            .unwrap();
        let category = mailcraft_engine::cmd::categories::quick_click_category(&app.s).unwrap();
        let mut h = harness(app, vec2(420.0, 1400.0));
        h.run();
        let before = h.query_all_by_label("No Categories").count();
        assert!(before > 0, "rows without categories offer the category column");
        // The first row without categories in list order.
        let rows: Vec<u64> = item_ids(h.state()).into_iter().map(|(id, _)| id).collect();
        let nth = rows
            .iter()
            .filter(|id| {
                h.state()
                    .s
                    .mailbox
                    .mail
                    .get(mailcraft_model::ItemId(**id))
                    .is_some_and(|m| m.categories.is_empty())
            })
            .position(|id| *id == target)
            .unwrap();
        h.query_all_by_label("No Categories").nth(nth).unwrap().click();
        h.run();
        let m = h.state().s.mailbox.mail.get(mailcraft_model::ItemId(target)).unwrap();
        assert!(m.categories.iter().any(|c| c.eq_ignore_ascii_case(&category)));
    }

    /// True when a rectangle filled with `color` was painted in the last frame.
    fn painted_fill(h: &Harness<'_, App>, color: Color32) -> bool {
        fn walk(shape: &egui::Shape, color: Color32) -> bool {
            match shape {
                egui::Shape::Vec(v) => v.iter().any(|s| walk(s, color)),
                egui::Shape::Rect(r) => r.fill == color,
                _ => false,
            }
        }
        h.output().shapes.iter().any(|c| walk(&c.shape, color))
    }

    #[test]
    fn flagged_rows_are_tinted_only_while_highlighting_is_on() {
        let mut s = sample_session();
        let ids = item_ids(&App::new(sample_session()));
        let (id, _) = ids.get(1).cloned().unwrap();
        s.selection.items = vec![mailcraft_model::ItemId(id)];
        s.selection.focus = Some(mailcraft_model::ItemId(id));
        s.run("mail.flag", &json!({"ids": [id]})).unwrap();
        s.selection.items.clear();
        s.selection.focus = None;
        let app = App::new(s);
        let tint = app.theme.flagged_row;
        let mut h = harness(app, vec2(420.0, 1400.0));
        h.run();
        assert!(painted_fill(&h, tint), "File › Options › Mail › Highlight flagged items is on by default");
        h.state_mut().run("options.set", json!({"key": "mail.other.highlightFlagged", "value": false}));
        h.run();
        h.run();
        assert!(!painted_fill(&h, tint), "no flagged tint once it is off");
        // The fill order: selection and hover beat the flagged tint.
        let th = h.state().theme.clone();
        let row = queries::list_rows(&h.state().s)
            .into_iter()
            .find_map(|r| match r {
                ListRow::Item(i) if i.flagged => Some(i),
                _ => None,
            })
            .unwrap();
        assert_eq!(row_fill(&th, &row, false, false, true), Some(th.flagged_row));
        assert_eq!(row_fill(&th, &row, false, false, false), None);
        assert_eq!(row_fill(&th, &row, true, false, true), Some(th.hover));
        let selected = ItemRow { selected: true, ..row };
        assert_eq!(row_fill(&th, &selected, true, false, true), Some(th.selected));
    }

    #[test]
    fn rule_fonts_and_download_state_reach_the_rows() {
        let mut s = sample_session();
        let folder: FolderId = s.selection.folder;
        let rules = json!([{"name": "Everything", "on": true, "kind": "custom",
            "condition": {"words": ""}, "font": {"italic": true}}]);
        // An empty condition matches nothing; one on the sender matches.
        s.run("view.conditionalFormatting", &json!({"rules": rules})).unwrap();
        let rows = queries::list_rows(&s);
        assert!(rows.iter().all(|r| !matches!(r, ListRow::Item(i) if i.style.italic)));
        let rules = json!([{"name": "Unread", "on": true, "kind": "unread", "font": {"underline": true}}]);
        s.run("view.conditionalFormatting", &json!({"rules": rules})).unwrap();
        let rows = queries::list_rows(&s);
        assert!(
            rows.iter()
                .any(|r| matches!(r, ListRow::Item(i) if i.unread && i.style.underline && !i.style.bold))
        );
        assert!(mailview::mail_view(&s, folder).rules.is_some());
        let app = App::new(s);
        let mut h = harness(app, vec2(420.0, 900.0));
        h.run();
    }

    // ---- Conversation view ---------------------------------------------------------------------------------

    /// Inbox with a four-message conversation (one reply in Sent Items, one forward) and two single messages,
    /// conversations on. Returns the session and the ids (lone, reply b1, forward f1, sent s1, original a1).
    fn conv_session() -> (Session, [u64; 5]) {
        use mailcraft_model::{Address, DateTime, MailItem, Mailbox};
        let now = 1_791_000_000;
        let mut s = Session::new(Mailbox::new_local("Test"));
        let acct = s.mailbox.accounts.first().map(|a| a.id).unwrap();
        let inbox = s.mailbox.role_folder(acct, FolderRole::Inbox).unwrap();
        let sent = s.mailbox.role_folder(acct, FolderRole::SentItems).unwrap();
        s.selection.folder = inbox;
        s.fixed_now = Some(DateTime { unix: now, offset_min: 0 });
        let mut add = |folder, subject: &str, from: &str, mid: &str, refs: &[&str], ago: i64, read: bool| {
            s.mailbox
                .add_mail(MailItem {
                    folder,
                    subject: subject.into(),
                    from: Some(Address {
                        name: Some(from.into()),
                        addr: format!("{}@example.com", from.to_lowercase()),
                    }),
                    message_id: Some(mid.into()),
                    in_reply_to: refs.last().map(|r| vec![(*r).to_owned()]).unwrap_or_default(),
                    references: refs.iter().map(|r| (*r).to_owned()).collect(),
                    received: DateTime { unix: now - ago, offset_min: 0 },
                    read,
                    preview: format!("{subject} preview"),
                    ..Default::default()
                })
                .0
        };
        let a1 = add(inbox, "Budget plan", "Alice", "a1@x", &[], 3 * 86_400, true);
        let s1 = add(sent, "RE: Budget plan", "Me", "s1@x", &["a1@x"], 2 * 86_400, true);
        let f1 = add(inbox, "FW: budget plan", "Carol", "f1@x", &[], 86_400, true);
        let b1 = add(inbox, "RE: Budget plan", "Bob", "b1@x", &["a1@x", "s1@x"], 3_600, false);
        let lone = add(inbox, "Lunch", "Dave", "l1@x", &[], 1_800, true);
        add(inbox, "Old thing", "Eve", "o1@x", &[], 10 * 86_400, true);
        s.run("view.conversations", &json!({})).unwrap();
        (s, [lone, b1, f1, s1, a1])
    }

    #[test]
    fn conversation_header_glyph_and_children() {
        let (s, [_, b1, f1, s1, _]) = conv_session();
        let mut h = harness(App::new(s), vec2(420.0, 900.0));
        h.run();
        let header = "Conversation, From Bob; Carol; Me; Alice, Subject Budget plan";
        assert!(h.query_all_by_label_contains(header).next().is_some(), "ThreadHeader accessible name");
        assert!(h.query_all_by_label_contains("Collapsed").next().is_some());
        assert!(h.query_all_by_label_contains("Subject FW: budget plan").next().is_none(), "children hidden");
        // The glyph expands without selecting.
        h.get_by_label("Expand conversation").click();
        h.run();
        assert!(h.state().s.selection.focus.is_none());
        assert!(h.query_all_by_label_contains("Subject FW: budget plan").next().is_some());
        assert!(
            h.query_all_by_label_contains("Folder Sent Items").next().is_some(),
            "Sent Items reply shown"
        );
        h.get_by_label("Collapse conversation").click();
        h.run();
        assert!(h.query_all_by_label_contains("Subject FW: budget plan").next().is_none());
        // Clicking the header selects the conversation's newest message and shows its children.
        h.query_all_by_label_contains(header).next().unwrap().click();
        h.run();
        assert_eq!(h.state().s.selection.focus.map(|f| f.0), Some(b1));
        assert!(h.query_all_by_label_contains("Expanded").next().is_some());
        // Keyboard: Down walks the children, Left collapses (focus back on the header), Right expands.
        h.key_press(Key::ArrowDown);
        h.run();
        assert_eq!(h.state().s.selection.focus.map(|f| f.0), Some(f1));
        h.key_press(Key::ArrowDown);
        h.run();
        assert_eq!(h.state().s.selection.focus.map(|f| f.0), Some(s1), "the Sent Items child");
        assert_eq!(h.state().s.mailbox.mail.get(mailcraft_model::ItemId(s1)).map(|m| m.folder), {
            let st = &h.state().s;
            st.mailbox.role_folder(st.current_account().unwrap(), FolderRole::SentItems)
        });
        h.key_press(Key::ArrowLeft);
        h.run();
        assert_eq!(h.state().s.selection.focus.map(|f| f.0), Some(b1));
        assert!(h.query_all_by_label_contains("Subject FW: budget plan").next().is_none(), "collapsed");
        h.key_press(Key::ArrowRight);
        h.run();
        assert!(h.query_all_by_label_contains("Subject FW: budget plan").next().is_some(), "expanded");
    }

    #[test]
    fn conversation_rows_in_column_layout() {
        let (mut s, [_, b1, ..]) = conv_session();
        s.run("view.readingPane", &json!({"position": "bottom"})).unwrap();
        s.run("nav.selectItem", &json!({"id": b1})).unwrap();
        let mut h = harness(App::new(s), vec2(900.0, 900.0));
        h.run();
        assert!(h.query_all_by_label_contains("Conversation, From Bob").next().is_some());
        assert!(h.query_by_label("Collapse conversation").is_some());
        assert!(h.query_all_by_label_contains("Folder Sent Items").next().is_some());
        h.get_by_label("Collapse conversation").click();
        h.run();
        assert!(h.query_by_label("Expand conversation").is_some());
    }

    #[test]
    fn child_rows_are_compact() {
        let c = build(&conv_session().0, 0);
        assert_eq!(c.vs.preview_lines, 1);
        assert!(child_height(Mode::Compact, 1, false) < item_height(Mode::Compact, 1, false));
        assert_eq!(child_height(Mode::Compact, 1, false), 12.0 + LINE1 + LINE);
        assert_eq!(child_height(Mode::Columns { preview: 0 }, 1, false), COL_ROW);
    }

    #[test]
    fn scope_question_on_toggle() {
        let (s, _) = conv_session();
        let sent = s.mailbox.role_folder(s.current_account().unwrap(), FolderRole::SentItems).unwrap();
        let mut h = harness(App::new(s), vec2(600.0, 700.0));
        h.run();
        // Turning off: Cancel keeps it on.
        ask_conversation_scope(&h.ctx.clone(), h.state());
        h.run();
        assert!(conversation_scope_pending(&h.ctx));
        assert!(h.query_by_label("Stop arranging messages by conversation in:").is_some());
        h.get_by_label("Cancel").click();
        h.run();
        assert!(!conversation_scope_pending(&h.ctx));
        let folder = h.state().s.selection.folder;
        assert!(queries::view_settings(&h.state().s, folder).conversations);
        // This folder: off here only.
        ask_conversation_scope(&h.ctx.clone(), h.state());
        h.run();
        h.get_by_label("This folder").click();
        h.run();
        assert!(!queries::view_settings(&h.state().s, folder).conversations);
        assert!(!queries::view_settings(&h.state().s, sent).conversations);
        // All mailboxes: on everywhere.
        ask_conversation_scope(&h.ctx.clone(), h.state());
        h.run();
        assert!(h.query_by_label("Arrange messages by conversation in:").is_some());
        h.get_by_label("All mailboxes").click();
        h.run();
        assert!(queries::view_settings(&h.state().s, folder).conversations);
        assert!(queries::view_settings(&h.state().s, sent).conversations);
    }

    #[test]
    fn conversation_settings_menu_reflects_engine_state() {
        let (s, _) = conv_session();
        let mut h = crate::test_harness().with_size(vec2(400.0, 300.0)).build_ui_state(
            |ui, app: &mut App| crate::menus::conversation_settings_menu(ui, app),
            App::new(s),
        );
        h.run();
        h.get_by_label("Use Classic Indented View").click();
        h.run();
        let st = &h.state().s;
        assert!(queries::conversation_settings(st, st.selection.folder).classic_indent);
        h.get_by_label("Show Messages from Other Folders").click();
        h.run();
        let st = &h.state().s;
        assert!(!queries::conversation_settings(st, st.selection.folder).from_other_folders);
    }
}
