//! Message list: header (Focused/Other, arrange menu, sort direction), grouped rows, compact/single/preview
//! layouts, hover actions, selection, keyboard navigation (list area).
//!
//! Behaviour from `plan/outlook/message-list.md` and `plan/outlook/reading-pane.md`: a 97 px header block,
//! date-group headers (24 px) and compact three-line rows (72 px with one preview line), or one-line column
//! rows (Single / Preview views, and Compact when the reading pane sits at the bottom or is off on a wide
//! list). Rows are virtualised: only the rows inside the viewport are laid out and painted, and the row
//! model from `queries::list_rows` is cached until the session changes, so thousands of items scroll
//! smoothly. All changes go through [`App::run`].

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Color32, EventFilter, FontFamily, FontId, Galley, Id, Key, Modifiers, Painter, Pos2, Rect, Sense, Stroke,
    Ui, UiBuilder, WidgetInfo, WidgetType, pos2, vec2,
};
use mailcraft_engine::Session;
use mailcraft_engine::queries::{self, ItemRow, ListRow};
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
    Id::new("mailcraft.list.focus")
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
        let mut job = LayoutJob::single_section(
            clean,
            TextFormat { font_id: FontId::new(size, family), color, ..Default::default() },
        );
        job.wrap = TextWrapping {
            max_width: max_w.max(1.0),
            max_rows: rows.max(1),
            break_anywhere: rows <= 1,
            overflow_character: Some('…'),
        };
        Text { galley: ui.painter().layout_job(job), faux_bold }
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
    searching: bool,
    /// Effective reading pane position for the folder.
    pane: ReadingPanePos,
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
    Id::new("mailcraft.list.state")
}

/// Cheap fingerprint of everything `list_rows` depends on. A mutating command that changes the mailbox
/// always pushes a new undo entry (a fresh boxed snapshot), so the top entry's address changes with it.
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
    s.undo.undo.last().map(|e| std::ptr::from_ref(e.before.as_ref()) as usize).hash(&mut h);
    s.undo.redo.last().map(|e| std::ptr::from_ref(e.before.as_ref()) as usize).hash(&mut h);
    for (k, c) in &s.composes {
        k.hash(&mut h);
        c.source.hash(&mut h);
    }
    (s.now().unix / 60).hash(&mut h);
    s.tz_offset_min.hash(&mut h);
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
    Cached { key, rows: Arc::new(queries::list_rows(s)), vs, role, focused_tabs, other_new, searching, pane }
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

/// Message list columns (column layout).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Col {
    Importance,
    Reminder,
    Icon,
    Attachment,
    From,
    Subject,
    Received,
    Size,
    Categories,
    Mention,
    Flag,
}

impl Col {
    /// `view.arrangeBy` key for clicking the header (`None` = not sortable).
    fn key(self, outgoing: bool) -> Option<&'static str> {
        match self {
            Col::Importance => Some("importance"),
            Col::Reminder | Col::Mention => None,
            Col::Icon => Some("type"),
            Col::Attachment => Some("attachments"),
            Col::From => Some(if outgoing { "to" } else { "from" }),
            Col::Subject => Some("subject"),
            Col::Received => Some("date"),
            Col::Size => Some("size"),
            Col::Categories => Some("categories"),
            Col::Flag => Some("flagStatus"),
        }
    }

    fn arrange(self, outgoing: bool) -> Option<ArrangeBy> {
        Some(match self {
            Col::Importance => ArrangeBy::Importance,
            Col::Icon => ArrangeBy::Type,
            Col::Attachment => ArrangeBy::Attachments,
            Col::From if outgoing => ArrangeBy::To,
            Col::From => ArrangeBy::From,
            Col::Subject => ArrangeBy::Subject,
            Col::Received => ArrangeBy::Date,
            Col::Size => ArrangeBy::Size,
            Col::Categories => ArrangeBy::Categories,
            Col::Flag => ArrangeBy::FlagStatus,
            Col::Reminder | Col::Mention => return None,
        })
    }

    fn name(self, outgoing: bool) -> &'static str {
        match self {
            Col::Importance => "Importance",
            Col::Reminder => "Reminder",
            Col::Icon => "Icon",
            Col::Attachment => "Attachment",
            Col::From if outgoing => "To",
            Col::From => "From",
            Col::Subject => "Subject",
            Col::Received => "Received",
            Col::Size => "Size",
            Col::Categories => "Categories",
            Col::Mention => "Mention",
            Col::Flag => "Flag Status",
        }
    }

    /// Header icon (icon-only columns).
    fn icon(self) -> Option<&'static str> {
        match self {
            Col::Importance => Some("importance-high"),
            Col::Reminder => Some("bell"),
            Col::Icon => Some("envelope-open"),
            Col::Attachment => Some("paperclip"),
            Col::Flag => Some("flag"),
            _ => None,
        }
    }
}

/// Column x offsets and widths for a list `width` wide.
fn columns(width: f32) -> Vec<(Col, f32, f32)> {
    let wide = width >= 620.0;
    let mut fixed: Vec<(Col, f32)> = vec![(Col::Importance, 22.0)];
    if wide {
        fixed.push((Col::Reminder, 22.0));
    }
    fixed.extend([(Col::Icon, 24.0), (Col::Attachment, 22.0)]);
    let tail: Vec<(Col, f32)> = if wide {
        vec![
            (Col::Received, 150.0),
            (Col::Size, 72.0),
            (Col::Categories, 110.0),
            (Col::Mention, 22.0),
            (Col::Flag, 26.0),
        ]
    } else {
        vec![(Col::Received, 112.0), (Col::Flag, 26.0)]
    };
    let used: f32 = fixed.iter().chain(tail.iter()).map(|(_, w)| *w).sum::<f32>() + 8.0;
    let flex = (width - used).max(80.0);
    // From stays narrow on wide lists; Subject takes the rest (size-data-bottom geometry).
    let from_w = (flex * 0.34).clamp(40.0, 200.0);
    let subject_w = (flex - from_w).max(40.0);
    let mut out = Vec::new();
    let mut x = 4.0;
    for (c, w) in fixed.into_iter().chain([(Col::From, from_w), (Col::Subject, subject_w)]).chain(tail) {
        out.push((c, x, w));
        x += w;
    }
    out
}

// -------------------------------------------------------------------------------------------------------------
// Entry point.

pub fn message_list(ui: &mut Ui, app: &mut App, frame: &Frame) {
    let ctx = ui.ctx().clone();
    let mut st: ListState = ctx.data_mut(|d| d.get_temp(state_id())).unwrap_or_default();
    let key = fingerprint(&app.s);
    let cached = match &st.cache {
        Some(c) if c.key == key => c.clone(),
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
    if c.other_new.is_some() {
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
        let resp = ui.interact(tab, Id::new(("mailcraft.list.tab", i)), Sense::click());
        if resp.hovered() && !on {
            painter.rect_filled(tab, 3.0, th.hover);
        }
        t.paint_left(&painter, tab, x);
        if on {
            let y = tab.bottom() - 2.0;
            painter.line_segment([pos2(x, y), pos2(x + t.width(), y)], Stroke::new(2.0, th.accent));
        }
        resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, on, *label));
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
    let sort_resp = ui.interact(sort_rect, Id::new("mailcraft.list.sortdir"), Sense::click());
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
    sort_resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, sort_tip));
    let sort_resp = sort_resp.on_hover_text(sort_tip);
    if sort_resp.clicked() {
        run(ui, app, "view.reverseSort", json!({}));
    }

    // "By Date ▾" arrange / filter / sort menu.
    let by = format!("By {}", c.vs.arrange_by.label());
    let t = Text::new(ui, &by, 13.0, false, th.text, 160.0, 1);
    let bw = t.width() + 30.0;
    let by_rect = Rect::from_min_size(pos2(sort_rect.left() - 4.0 - bw, row_a.top() + 8.0), vec2(bw, 24.0));
    let by_resp = ui.interact(by_rect, Id::new("mailcraft.list.arrange"), Sense::click());
    if by_resp.hovered() {
        painter.rect_filled(by_rect, 3.0, th.hover);
    }
    let end = t.paint_left(&painter, by_rect, by_rect.left() + 8.0);
    app.icons.paint(ui, icon_rect(pos2(end + 10.0, by_rect.center().y), 12.0), "chevron-down", th.text_muted);
    by_resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &by));
    egui::Popup::menu(&by_resp).show(|ui| crate::menus::arrange_menu(ui, app));

    // Row B: "Other: New messages (N)" banner (only while there is new Other mail).
    let row_b = Rect::from_min_size(
        pos2(rect.left(), row_a.bottom() + (BANNER_ROW - 30.0) / 2.0),
        vec2(rect.width(), 30.0),
    );
    if let Some((n, senders)) = &c.other_new {
        let resp = ui.interact(row_b, Id::new("mailcraft.list.otherbanner"), Sense::click());
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
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{label} {senders}")));
        if resp.clicked() {
            run(ui, app, "view.focusedTab", json!({ "tab": "other" }));
        }
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

fn column_header(ui: &mut Ui, app: &mut App, c: &Cached, row: Rect) {
    let th = app.theme.clone();
    let painter = ui.painter_at(row);
    painter.rect_filled(row, 0.0, th.group_header);
    let out = outgoing(c.role);
    for (col, x, w) in columns(row.width()) {
        let cell = Rect::from_min_size(pos2(row.left() + x, row.top()), vec2(w, row.height()));
        let resp = ui.interact(cell, Id::new(("mailcraft.list.colhdr", col as u8)), Sense::click());
        if resp.hovered() && col.key(out).is_some() {
            painter.rect_filled(cell, 0.0, th.hover);
        }
        let sorted = col.arrange(out) == Some(c.vs.arrange_by);
        let name = col.name(out);
        let mut right = cell.right() - 4.0;
        if sorted {
            let icon = if c.vs.order == SortOrder::Descending { "sort-down" } else { "sort-up" };
            if w > 40.0 {
                app.icons.paint(ui, icon_rect(pos2(right - 6.0, cell.center().y), 12.0), icon, th.text_muted);
                right -= 14.0;
            }
        }
        match col.icon() {
            Some(icon) => {
                let tint = if sorted { th.accent } else { th.text_muted };
                app.icons.paint(ui, icon_rect(cell.center(), 14.0), icon, tint);
            }
            None if col == Col::Mention => {
                let t = Text::new(ui, "@", 12.0, false, th.text_muted, w, 1);
                t.paint_left(&painter, cell, cell.center().x - t.width() / 2.0);
            }
            None => {
                let t =
                    Text::new(ui, name, 12.0, sorted, th.text_muted, (right - cell.left() - 6.0).max(1.0), 1);
                t.paint_left(&painter, cell, cell.left() + 6.0);
            }
        }
        painter.line_segment(
            [pos2(cell.right() - 0.5, cell.top() + 5.0), pos2(cell.right() - 0.5, cell.bottom() - 5.0)],
            Stroke::new(1.0, th.separator),
        );
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
        let resp = resp.on_hover_text(name);
        if resp.clicked()
            && let Some(key) = col.key(out)
        {
            if sorted {
                run(ui, app, "view.reverseSort", json!({}));
            } else {
                run(ui, app, "view.arrangeBy", json!({ "key": key }));
            }
        }
        resp.context_menu(|ui| crate::menus::arrange_menu(ui, app));
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
        bg.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, "Message list, empty"));
        st.last_focus = app.s.selection.focus.map(|f| f.0);
        return;
    }
    bg.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, "Message list"));

    let item_h = item_height(mode, c.vs.preview_lines, c.vs.tighter_spacing);
    let mut starts: Vec<f32> = Vec::with_capacity(rows.len());
    let mut y = 0.0_f32;
    for r in rows.iter() {
        starts.push(y);
        y += match r {
            ListRow::Group { .. } => size::GROUP_HEADER,
            ListRow::Item(_) => item_h,
        };
    }
    let total = y;
    let height_of = |i: usize| match rows.get(i) {
        Some(ListRow::Group { .. }) => size::GROUP_HEADER,
        _ => item_h,
    };

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
                    Some(ListRow::Group { label, count, collapsed }) => {
                        group_row(ui, app, r, label, *count, *collapsed);
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
        }
    }
}

/// Moves the focus `delta` visible items (clamped); `extend` selects the range from the anchor.
fn page(ui: &Ui, app: &mut App, c: &Cached, delta: isize, extend: bool) {
    let ids: Vec<u64> = c
        .rows
        .iter()
        .filter_map(|r| match r {
            ListRow::Item(i) => Some(i.id.0),
            ListRow::Group { .. } => None,
        })
        .collect();
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

/// Collapses or expands a group. The engine's group commands act on the focused item's group, so an item
/// of the group is focused first when the focus lies elsewhere.
fn toggle_group(ui: &Ui, app: &mut App, label: &str, collapsed: bool) {
    let ids = queries::group_items(&app.s, label);
    let Some(first) = ids.first() else { return };
    let focus_inside = app.s.selection.focus.is_some_and(|f| ids.contains(&f));
    if !focus_inside {
        run(ui, app, "nav.selectItem", json!({ "id": first.0 }));
    }
    let id = if collapsed { "view.expandGroup" } else { "view.collapseGroup" };
    run(ui, app, id, json!({}));
}

fn group_row(ui: &mut Ui, app: &mut App, r: Rect, label: &str, count: usize, collapsed: bool) {
    let th = app.theme.clone();
    let resp = ui.interact(r, Id::new(("mailcraft.list.group", label)), Sense::click());
    let painter = ui.painter().clone();
    let fill = if resp.hovered() { th.hover } else { th.group_header };
    painter.rect_filled(r.shrink2(vec2(0.0, 1.0)), 0.0, fill);
    let chevron = if collapsed { "chevron-right" } else { "chevron-down" };
    app.icons.paint(ui, icon_rect(pos2(r.left() + 14.0, r.center().y), 12.0), chevron, th.text_muted);
    let t = Text::new(ui, label, 12.5, true, th.text, (r.width() - 40.0).max(1.0), 1);
    t.paint_left(&painter, r, r.left() + 26.0);
    let state = if collapsed { "collapsed" } else { "expanded" };
    resp.widget_info(|| {
        WidgetInfo::labeled(WidgetType::CollapsingHeader, true, format!("{label}: {count} items, {state}"))
    });
    if resp.clicked() {
        toggle_group(ui, app, label, collapsed);
    }
    resp.context_menu(|ui| crate::menus::group_header_menu(ui, app, label));
}

/// Where the clickable parts of a row ended up.
struct Slots {
    flag: Rect,
    delete: Option<Rect>,
}

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
    let resp = ui.interact(r, Id::new(("mailcraft.list.row", id)), Sense::click_and_drag());
    let hovered = resp.contains_pointer() && !egui::DragAndDrop::has_any_payload(ui.ctx());
    if hovered {
        app.ui.hover_row = Some(id);
    } else if app.ui.hover_row == Some(id) {
        app.ui.hover_row = None;
    }
    let painter = ui.painter().clone();
    let fill = if row.selected {
        Some(th.selected)
    } else if hovered {
        Some(th.hover)
    } else if row.flagged {
        Some(th.flagged_row)
    } else {
        None
    };
    if let Some(fill) = fill {
        painter.rect_filled(r, 0.0, fill);
    }
    if row.unread {
        let bar = Rect::from_min_size(r.min + vec2(0.0, 1.0), vec2(size::UNREAD_BAR, r.height() - 2.0));
        painter.rect_filled(bar, 0.0, th.unread);
    }
    let slots = match mode {
        Mode::Compact => paint_compact(ui, app, r, row, hovered, c.vs.preview_lines, c.vs.tighter_spacing),
        Mode::Columns { preview } => paint_columns(ui, app, c, r, row, hovered, preview, st),
    };
    painter.line_segment(
        [pos2(r.left() + 8.0, r.bottom() - 0.5), pos2(r.right(), r.bottom() - 0.5)],
        Stroke::new(0.5, th.separator),
    );

    // Quick actions sit on top of the row (added after it, so they take the click).
    let flag_resp = ui.interact(slots.flag, Id::new(("mailcraft.list.flag", id)), Sense::click());
    let flag_tip = if row.flag_complete {
        "Clear flag"
    } else if row.flagged {
        "Mark complete"
    } else {
        "Flag this message"
    };
    if flag_resp.hovered() {
        painter.rect_stroke(
            slots.flag.shrink(1.0),
            3.0,
            Stroke::new(1.0, th.separator),
            egui::StrokeKind::Inside,
        );
    }
    flag_resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, flag_tip));
    if flag_resp.on_hover_text(flag_tip).clicked() {
        let params = if row.flag_complete { json!({ "kind": "clear" }) } else { json!({}) };
        run_on_items(ui, app, "mail.flag", &[id], params);
    }
    if let Some(del) = slots.delete {
        let del_resp = ui.interact(del, Id::new(("mailcraft.list.delete", id)), Sense::click());
        if del_resp.hovered() {
            painter.rect_stroke(
                del.shrink(1.0),
                3.0,
                Stroke::new(1.0, th.separator),
                egui::StrokeKind::Inside,
            );
        }
        del_resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Delete"));
        if del_resp.on_hover_text("Delete").clicked() {
            run_on_items(ui, app, "mail.delete", &[id], json!({}));
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
        if frame.single_pane && !m.command && !m.shift {
            app.ui.single_pane_reading = true;
        }
    }
    if resp.secondary_clicked() && !row.selected {
        run(ui, app, "nav.selectItem", json!({ "id": id }));
    }
    let accessible = row.accessible.trim_end_matches([',', ' ']).to_owned();
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, row.selected, &accessible));
    resp.context_menu(|ui| crate::menus::message_row_menu(ui, app, id));
}

/// Line-1 status icons (right to left after the flag slot): categories, importance, attachment.
fn paint_status_icons(ui: &Ui, app: &mut App, row: &ItemRow, band: Rect, mut right: f32) -> f32 {
    let th = app.theme.clone();
    let painter = ui.painter();
    for hex in row.categories.iter().take(4).rev() {
        let block = Rect::from_min_size(pos2(right - 8.0, band.center().y - 6.0), vec2(8.0, 12.0));
        painter.rect_filled(block, 2.0, hex_color(hex));
        right -= 11.0;
    }
    if row.importance_high {
        app.icons.paint(
            ui,
            icon_rect(pos2(right - 8.0, band.center().y), 14.0),
            "importance-high",
            th.importance_high,
        );
        right -= 17.0;
    } else if row.importance_low {
        app.icons.paint(
            ui,
            icon_rect(pos2(right - 8.0, band.center().y), 14.0),
            "importance-low",
            th.importance_low,
        );
        right -= 17.0;
    }
    if row.has_attachments {
        app.icons.paint(ui, icon_rect(pos2(right - 8.0, band.center().y), 14.0), "paperclip", th.text_muted);
        right -= 17.0;
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

fn paint_compact(
    ui: &Ui,
    app: &mut App,
    r: Rect,
    row: &ItemRow,
    hovered: bool,
    preview_lines: u8,
    tighter: bool,
) -> Slots {
    let th = app.theme.clone();
    let painter = ui.painter().clone();
    let pad = if tighter { 4.0 } else { 8.0 };
    let left = r.left() + 12.0 + 16.0 * f32::from(row.conversation_depth.min(4));
    let right = r.right() - 8.0;
    let line1 = Rect::from_min_max(pos2(left, r.top() + pad), pos2(right, r.top() + pad + LINE1));
    let line2 = Rect::from_min_max(pos2(left, line1.bottom()), pos2(right, line1.bottom() + LINE));

    // Line 1: [Draft] sender … status icons, delete (hover), flag.
    let flag = Rect::from_min_size(pos2(right - 19.0, line1.center().y - 10.0), vec2(19.0, 20.0));
    paint_flag(ui, &mut app.icons, &th, row, flag, hovered);
    let mut icons_left = flag.left();
    let delete = if hovered {
        let d = Rect::from_min_size(pos2(icons_left - 21.0, flag.top()), vec2(19.0, 20.0));
        app.icons.paint(ui, icon_rect(d.center(), 14.0), "delete", th.text_muted);
        icons_left = d.left();
        Some(d)
    } else {
        None
    };
    icons_left = paint_status_icons(ui, app, row, line1, icons_left - 2.0);
    let mut x = left;
    if row.draft_marker {
        let t = Text::new(ui, "[Draft]", 14.0, false, th.draft, 60.0, 1);
        x = t.paint_left(&painter, line1, x) + 5.0;
    }
    let who = Text::new(ui, &row.who, 14.5, false, th.text, (icons_left - x - 6.0).max(1.0), 1);
    who.paint_left(&painter, line1, x);

    // Line 2: subject … reply/forward state, time.
    let time_color = if row.unread { th.unread } else { th.text_muted };
    let time = Text::new(ui, &row.time, 12.0, row.unread, time_color, 120.0, 1);
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
    let subject_color = if row.unread { th.unread } else { th.text };
    let subject =
        Text::new(ui, &row.subject, 13.0, row.unread, subject_color, (sub_right - left).max(1.0), 1);
    subject.paint_left(&painter, line2, left);

    // Lines 3..: preview.
    let lines = usize::from(preview_lines.min(3));
    if lines > 0 && !row.preview.trim().is_empty() {
        let t = Text::new(ui, row.preview.trim(), 12.5, false, th.text_muted, (right - left).max(1.0), lines);
        t.paint(&painter, pos2(left, line2.bottom() + 1.0));
    }
    Slots { flag, delete }
}

fn paint_columns(
    ui: &Ui,
    app: &mut App,
    c: &Cached,
    r: Rect,
    row: &ItemRow,
    hovered: bool,
    preview: u8,
    st: &mut ListState,
) -> Slots {
    let th = app.theme.clone();
    let painter = ui.painter().clone();
    let line_h = if c.vs.tighter_spacing { COL_ROW - 4.0 } else { COL_ROW };
    let line = Rect::from_min_size(r.min, vec2(r.width(), line_h));
    let mail = app.s.mailbox.mail.get(&row.id);
    let text_color = if row.unread { th.unread } else { th.text };
    let mut flag = Rect::from_min_size(pos2(r.right() - 22.0, line.top()), vec2(19.0, line_h));
    let mut delete = None;
    for (col, x, w) in columns(r.width()) {
        let cell = Rect::from_min_size(pos2(r.left() + x, line.top()), vec2(w, line_h));
        let inner_w = (w - 10.0).max(1.0);
        match col {
            Col::Importance => {
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
            Col::Reminder => {
                let reminder =
                    mail.is_some_and(|m| matches!(m.flag, FlagState::Flagged { reminder: Some(_), .. }));
                if reminder {
                    app.icons.paint(ui, icon_rect(cell.center(), 13.0), "bell", th.text_muted);
                }
            }
            Col::Icon => {
                let icon = if row.draft_marker || mail.is_some_and(|m| m.is_draft) {
                    "draft"
                } else if row.replied {
                    "replied"
                } else if row.forwarded {
                    "forwarded"
                } else if row.unread {
                    "mail"
                } else {
                    "envelope-open"
                };
                app.icons.paint(ui, icon_rect(cell.center(), 14.0), icon, th.text_muted);
            }
            Col::Attachment => {
                if row.has_attachments {
                    app.icons.paint(ui, icon_rect(cell.center(), 14.0), "paperclip", th.text_muted);
                }
            }
            Col::From => {
                let mut x = cell.left() + 6.0;
                if row.draft_marker {
                    let t = Text::new(ui, "[Draft]", 13.0, false, th.draft, inner_w, 1);
                    x = t.paint_left(&painter, cell, x) + 4.0;
                }
                let t =
                    Text::new(ui, &row.who, 13.0, row.unread, th.text, (cell.right() - x - 4.0).max(1.0), 1);
                t.paint_left(&painter, cell, x);
            }
            Col::Subject => {
                let mut right = cell.right() - 4.0;
                if hovered {
                    let d = Rect::from_min_size(pos2(right - 19.0, cell.top()), vec2(19.0, line_h));
                    app.icons.paint(ui, icon_rect(d.center(), 14.0), "delete", th.text_muted);
                    right = d.left() - 2.0;
                    delete = Some(d);
                }
                let t = Text::new(
                    ui,
                    &row.subject,
                    13.0,
                    row.unread,
                    text_color,
                    (right - cell.left() - 6.0).max(1.0),
                    1,
                );
                t.paint_left(&painter, cell, cell.left() + 6.0);
            }
            Col::Received => {
                let color = if row.unread { th.unread } else { th.text_muted };
                let t = Text::new(ui, &row.time, 12.5, row.unread, color, inner_w, 1);
                t.paint_left(&painter, cell, cell.left() + 6.0);
            }
            Col::Size => {
                let label = mail.map(|m| m.size_label()).unwrap_or_default();
                let t = Text::new(ui, &label, 12.5, false, th.text_muted, inner_w, 1);
                t.paint_right(&painter, cell, cell.right() - 6.0);
            }
            Col::Categories => {
                let mut x = cell.left() + 6.0;
                let names: Vec<String> = mail.map(|m| m.categories.clone()).unwrap_or_default();
                for hex in row.categories.iter().take(3) {
                    let block = Rect::from_min_size(pos2(x, cell.center().y - 6.0), vec2(8.0, 12.0));
                    painter.rect_filled(block, 2.0, hex_color(hex));
                    x += 11.0;
                }
                if !names.is_empty() {
                    let t = Text::new(
                        ui,
                        &names.join(", "),
                        12.0,
                        false,
                        th.text_muted,
                        (cell.right() - x - 4.0).max(1.0),
                        1,
                    );
                    t.paint_left(&painter, cell, x + 2.0);
                }
            }
            Col::Mention => {
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
            Col::Flag => {
                flag = cell;
                paint_flag(ui, &mut app.icons, &th, row, cell, hovered);
            }
        }
    }
    if preview > 0 && !row.preview.trim().is_empty() {
        // The preview line starts under the From column.
        let from_x =
            columns(r.width()).iter().find(|(col, _, _)| *col == Col::From).map_or(76.0, |(_, x, _)| *x);
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
    Slots { flag, delete }
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
        ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("mailcraft.list.dragchip")));
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
    use mailcraft_engine::sample::sample_session;

    fn harness(app: App, size: egui::Vec2) -> Harness<'static, App> {
        Harness::builder().with_size(size).build_ui_state(
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
        let base = s.mailbox.mail.keys().map(|k| k.0).max().unwrap_or(0) + 1;
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
        let cols = columns(900.0);
        assert_eq!(cols.len(), 11);
        assert!(cols.windows(2).all(|w| w[0].1 < w[1].1));
        assert_eq!(columns(400.0).len(), 7);
        assert_eq!(hex_color("#ff0000"), Color32::from_rgb(255, 0, 0));
        assert_eq!(hex_color("junk"), Color32::GRAY);
    }
}
