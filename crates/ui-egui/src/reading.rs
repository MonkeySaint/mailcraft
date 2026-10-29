//! Reading pane and read windows (reading area).
//!
//! Layout per plan/reference/reading-pane.md and read-window.md: subject (large); persona circle with initials
//! (stable colour per sender) next to "Name <address>", To and Cc lines; on the right an optional
//! background toggle (dark mode only), the Reply | Reply All | Forward segmented control (icon-only when
//! narrow), a "…" menu and the sent date (short form when narrow); info bars; attachment chips; the
//! "pictures not downloaded" bar; then the body (HTML through mailcraft-html, plain text as paragraphs).
//! When an inline response exists for the selected item the compose area's inline editor replaces the view.
//!
//! Local UI state (not in the Session): which items show the light background in dark mode, which items may
//! load remote pictures, and per-surface parsed-body caches (egui temp data).
//!
//! Find and Select in the received message: Message › Find… (read windows) and the body's right-click
//! menu open a find bar above the message (matches highlighted, Next / Previous, match case, "n of m");
//! Select › Select All and Select Text With Similar Formatting select in the body for copying. The engine
//! asks for these with `messageFind` / `messageSelect` notices, which land in [`request`].
//!
//! Options that act here (engine settings): Reading Pane options (mark as read after N seconds, Space-bar
//! reading through [`space_read`], "Always preview messages": off, the pane shows the selected message only
//! once it is opened, with a link to turn previews back on), General › "Never change the message background
//! color", Search › highlight, Trust Center › pictures / plain text / link warning, Mail › "Close original
//! message window when replying or forwarding", "After moving or deleting an open item" and "Show Next and
//! Previous links in the message headers when reading messages" (read windows), People › "Show user
//! photographs when available" (the sender's contact picture instead of the initials), Advanced › "Allow
//! UTF-8 support for the mailto: protocol" (`mailto:` links go to `mail.newFromMailto`).

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::{
    Align, Color32, FontId, Id, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2,
};
use mailcraft_engine::queries::{self, ReadingHeader};
use mailcraft_model::{BodyFormat, Doc, ItemId};
use serde_json::json;

use crate::body::{self, BodyOptions, BodyView};
use crate::responsive::Frame;
use crate::{App, Surface};

/// Window width at or below which the header goes narrow (icon-only respond buttons, short date).
const NARROW_W: f32 = 700.0;
/// Reading-pane width below which the header goes narrow regardless of the window.
const NARROW_PANE_W: f32 = 520.0;
const PAD: f32 = 16.0;
/// Gap between the sender line and the header's button cluster.
const HEADER_GAP: f32 = 12.0;
/// Default size of a read window.
const READ_WINDOW_SIZE: [f32; 2] = [960.0, 760.0];

pub fn reading_pane(ui: &mut egui::Ui, app: &mut App, frame: &Frame) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, app.theme.reading);
    let focus = focused_mail(app);

    // Single-pane mode (responsive.md): a ← back button at the top-left, with the message beside it.
    if frame.single_pane {
        let back =
            Rect::from_min_size(Pos2::new(rect.left() + 4.0, rect.top() + 10.0), Vec2::new(32.0, 28.0));
        let mut b =
            ui.new_child(UiBuilder::new().max_rect(back).layout(Layout::left_to_right(Align::Center)));
        if plain_icon_button(&mut b, app, "back", None, "Back to the message list").clicked() {
            app.ui.single_pane_reading = false;
        }
        let content = Rect::from_min_max(Pos2::new(back.right(), rect.top()), rect.max);
        if content.width() < 1.0 {
            return;
        }
        let mut c = ui.new_child(UiBuilder::new().max_rect(content).layout(Layout::top_down(Align::Min)));
        c.set_clip_rect(content.intersect(ui.clip_rect()));
        pane_contents(&mut c, app, focus, frame);
        ui.advance_cursor_after_rect(rect);
    } else {
        pane_contents(ui, app, focus, frame);
    }
}

fn pane_contents(ui: &mut Ui, app: &mut App, focus: Option<u64>, frame: &Frame) {
    link_prompt(ui.ctx(), app);
    let Some(id) = focus else {
        empty_state(ui, app);
        return;
    };
    if let Some(cid) = inline_compose_for(app, id) {
        crate::compose::inline_compose(ui, app, cid);
        return;
    }
    if !app.s.settings.bool("mail.readingPane.alwaysPreview") {
        preview_off(ui, app);
        return;
    }
    let narrow = frame.width <= NARROW_W && frame.width > 0.0 || ui.available_width() < NARROW_PANE_W;
    mark_read_timer(ui.ctx(), app, id);
    message_view(ui, app, id, Surface::Main, narrow);
}

/// Shown instead of the selected message while Reading Pane options › "Always preview messages" is off.
pub const PREVIEW_OFF: &str = "Open the message to read it (double-click it or press Enter).";
/// The link that turns previews back on.
pub const ALWAYS_PREVIEW_LINK: &str = "Click here to always preview messages";

/// The reading pane while previews are off: nothing of the message is shown (and nothing marks it read);
/// the link turns "Always preview messages" back on.
fn preview_off(ui: &mut Ui, app: &mut App) {
    let avail = ui.available_rect_before_wrap();
    let center = Pos2::new(avail.center().x, avail.top() + (avail.height() * 0.4).max(60.0));
    let icon = Rect::from_center_size(center, Vec2::splat(56.0));
    app.icons.paint(ui, icon, "mail", app.theme.text_muted);
    let label_rect =
        Rect::from_center_size(Pos2::new(center.x, center.y + 64.0), Vec2::new(avail.width() - 24.0, 56.0));
    let mut turn_on = false;
    ui.scope_builder(UiBuilder::new().max_rect(label_rect).layout(Layout::top_down(Align::Center)), |ui| {
        ui.add(egui::Label::new(RichText::new(PREVIEW_OFF).size(13.5).color(app.theme.text_muted)).wrap());
        if ui.link(RichText::new(ALWAYS_PREVIEW_LINK).size(12.5).color(app.theme.link)).clicked() {
            turn_on = true;
        }
    });
    if turn_on {
        app.run("options.set", json!({"key": "mail.readingPane.alwaysPreview", "value": true}));
    }
}

/// Read window Previous / Next Item (Ctrl+< / Ctrl+>, and the header's Previous / Next links): shows the
/// message before / after `item` in the main window's list in the same window. Returns the new item.
pub(crate) fn read_step(app: &mut App, item: u64, next: bool) -> Option<u64> {
    let order = queries::visible_item_ids(&app.s);
    let pos = order.iter().position(|i| i.0 == item)?;
    let target =
        if next { order.get(pos.saturating_add(1)) } else { pos.checked_sub(1).and_then(|q| order.get(q)) };
    let t = target?.0;
    if app.ui.read_windows.contains(&t) {
        return None;
    }
    let slot = app.ui.read_windows.iter_mut().find(|w| **w == item)?;
    *slot = t;
    Some(t)
}

/// Accessible names of the read window header's Previous / Next links.
pub const PREVIOUS_LINK: &str = "Previous message";
pub const NEXT_LINK: &str = "Next message";

/// Reading Pane options › "Mark items as read when viewed in the Reading Pane" after the wait in seconds.
fn mark_read_timer(ctx: &egui::Context, app: &mut App, id: u64) {
    let key = per_viewport(ctx, Id::new("mailcraft.reading.read-timer"));
    let st = &app.s.settings;
    let unread = app.s.mailbox.mail.get(ItemId(id)).is_some_and(|m| !m.read);
    if !st.bool("mail.readingPane.markReadWhenViewed") || !unread {
        ctx.data_mut(|d| d.remove::<(u64, f64)>(key));
        return;
    }
    let wait = st.int("mail.readingPane.markReadAfterSeconds").clamp(0, 999) as f64;
    let now = ctx.input(|i| i.time);
    let since = match ctx.data_mut(|d| d.get_temp::<(u64, f64)>(key)) {
        Some((item, since)) if item == id => since,
        _ => {
            ctx.data_mut(|d| d.insert_temp(key, (id, now)));
            now
        }
    };
    let left = since + wait - now;
    if left <= 0.0 {
        ctx.data_mut(|d| d.remove::<(u64, f64)>(key));
        app.run("mail.markRead", json!({"ids": [id]}));
    } else {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(left.min(1000.0)));
    }
}

/// Reading-pane scroll metrics of the last frame (for Space-bar reading).
#[derive(Clone, Copy, Debug, Default)]
struct ReadScroll {
    item: u64,
    offset: f32,
    content: f32,
    view: f32,
}

fn scroll_id(ctx: &egui::Context) -> Id {
    per_viewport(ctx, Id::new("mailcraft.reading.scroll-metrics"))
}

fn pending_scroll_id(ctx: &egui::Context) -> Id {
    per_viewport(ctx, Id::new("mailcraft.reading.scroll-to"))
}

/// `base` for the window being drawn: the main window keeps `base`, each explorer window (a second main
/// window, its own viewport) gets its own id, so their reading panes never share caches or find bars.
fn per_viewport(ctx: &egui::Context, base: Id) -> Id {
    salted(base, ctx.viewport_id())
}

/// `base` for viewport `vp` (see [`per_viewport`]).
fn salted(base: Id, vp: egui::ViewportId) -> Id {
    if vp == egui::ViewportId::ROOT { base } else { base.with(vp) }
}

/// [`per_viewport`] for the main surface (read windows already have ids of their own).
fn surface_id(ctx: &egui::Context, base: Id, surface: Surface) -> Id {
    if surface == Surface::Main { per_viewport(ctx, base) } else { base }
}

/// Drops the reading-pane state an explorer window kept (it closed).
pub(crate) fn forget_viewport(ctx: &egui::Context, vp: egui::ViewportId) {
    if vp == egui::ViewportId::ROOT {
        return;
    }
    let salt = |base: Id| salted(base, vp);
    ctx.data_mut(|d| {
        d.remove::<MsgCache>(salt(Id::new(("mailcraft.reading.cache", Surface::Main))));
        d.remove::<FindBar>(salt(Id::new(("mailcraft.reading.find", Surface::Main))));
        d.remove::<(u64, Request)>(salt(Id::new(("mailcraft.reading.request", Surface::Main))));
        d.remove::<ReadScroll>(salt(Id::new("mailcraft.reading.scroll-metrics")));
        d.remove::<f32>(salt(Id::new("mailcraft.reading.scroll-to")));
        d.remove::<(u64, f64)>(salt(Id::new("mailcraft.reading.read-timer")));
        d.remove::<[egui::Rect; 3]>(salt(egui::Id::new(("mailcraft.reading.stops", Surface::Main.key()))));
    });
}

/// Space-bar reading (Reading Pane options › "Single key reading using space bar"): pages the reading pane
/// down (`back`: up); at the end of the message (start, going back) moves to the next (previous) item.
/// Returns whether anything happened.
pub fn space_read(ctx: &egui::Context, app: &mut App, back: bool) -> bool {
    let focus = focused_mail(app);
    // Ids are made before `data` locks the context (making one reads the context).
    let key = scroll_id(ctx);
    let m = ctx.data_mut(|d| d.get_temp::<ReadScroll>(key));
    if let Some(m) = m.filter(|m| Some(m.item) == focus && m.view > 0.0) {
        let max = (m.content - m.view).max(0.0);
        let page = (m.view * 0.9).max(20.0);
        let at_end = if back { m.offset <= 0.5 } else { m.offset >= max - 0.5 };
        if !at_end {
            let target = if back { m.offset - page } else { m.offset + page };
            let key = pending_scroll_id(ctx);
            ctx.data_mut(|d| d.insert_temp(key, target.clamp(0.0, max)));
            ctx.request_repaint();
            return true;
        }
    }
    app.run(if back { "nav.prevItem" } else { "nav.nextItem" }, json!({})).is_some()
}

/// Opened-message windows (secondary viewports) listed in `app.ui.read_windows`.
pub fn read_windows(ctx: &egui::Context, app: &mut App) {
    let ids = app.ui.read_windows.clone();
    // The engine keeps the bodies of open messages on this computer (the offline window passes them by).
    if app.s.open_messages.len() != ids.len()
        || ids.iter().any(|i| !app.s.open_messages.contains(&ItemId(*i)))
    {
        app.s.open_messages = ids.iter().copied().map(ItemId).collect();
    }
    let mut closed: Vec<u64> = Vec::new();
    let after = app.s.settings.text("mail.other.afterMoveOrDelete");
    for (index, id) in ids.into_iter().enumerate() {
        // The folder the item was in when the window first showed it: deleting, archiving or moving the item
        // takes it out of that folder, which counts as the item leaving the window.
        let folder_key = folder_id(id);
        let current = app.s.mailbox.mail.get(ItemId(id)).map(|m| m.folder.0);
        let opened_in = ctx.data_mut(|d| d.get_temp::<u64>(folder_key));
        if opened_in.is_none()
            && let Some(f) = current
        {
            ctx.data_mut(|d| d.insert_temp(folder_key, f));
        }
        let mut left = match (current, opened_in) {
            (None, _) => true,
            (Some(now), Some(then)) => now != then,
            (Some(_), None) => false,
        };
        // A message with unsaved Edit Message changes that was moved away (Move, Archive, Delete to Deleted
        // Items, from this window, the main window or a script) keeps its window, now in the new folder, and
        // the save question shows: its edits are never dropped without asking.
        if left
            && let Some(now) = current
            && crate::edit_message::has_unsaved_edits(ctx, id)
            && crate::edit_message::is_editing(app, id)
        {
            ctx.data_mut(|d| d.insert_temp(folder_key, now));
            crate::edit_message::may_close(ctx, app, id);
            left = false;
        }
        let m = app.s.mailbox.mail.get(ItemId(id)).filter(|_| !left);
        let Some(m) = m else {
            ctx.data_mut(|d| d.remove::<u64>(folder_key));
            // The window shows another message now (or closes): an editor of this one goes with it. Edits can
            // only be lost here when the message itself is gone (deleted for good elsewhere).
            if crate::edit_message::has_unsaved_edits(ctx, id) {
                app.ui.toast = Some((crate::edit_message::GONE_NOTE.to_owned(), 0.0));
            }
            crate::edit_message::stop(ctx, app, id);
            // Mail › "After moving or deleting an open item": open the previous / next item in this window, or
            // return to the folder (close).
            let neighbour = ctx.data_mut(|d| d.get_temp::<(Option<u64>, Option<u64>)>(neighbour_id(id)));
            let next = match (after.as_str(), neighbour) {
                ("openNext", Some((_, n))) => n,
                ("openPrevious", Some((p, _))) => p,
                _ => None,
            }
            .filter(|n| app.s.mailbox.mail.contains(ItemId(*n)) && !app.ui.read_windows.contains(n));
            let next = next.filter(|n| *n != id);
            match next {
                Some(n) => {
                    if let Some(slot) = app.ui.read_windows.iter_mut().find(|w| **w == id) {
                        *slot = n;
                    }
                    ctx.request_repaint();
                }
                None => closed.push(id),
            }
            ctx.data_mut(|d| d.remove::<(Option<u64>, Option<u64>)>(neighbour_id(id)));
            continue;
        };
        if after != "returnToFolder" {
            let order = queries::visible_item_ids(&app.s);
            if let Some(p) = order.iter().position(|i| i.0 == id) {
                let prev = p.checked_sub(1).and_then(|q| order.get(q)).map(|i| i.0);
                let next = order.get(p.saturating_add(1)).map(|i| i.0);
                ctx.data_mut(|d| d.insert_temp(neighbour_id(id), (prev, next)));
            }
        }
        let title = window_title(&m.subject, mailcraft_engine::smime::display_body(&app.s, m).format);
        let builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(READ_WINDOW_SIZE)
            .with_min_inner_size([300.0, 200.0]);
        let vid = read_viewport_id(id);
        let theme = app.theme.clone();
        // Read windows stack after the compose windows.
        let slot = crate::widgets::ItemSlot {
            index: crate::widgets::compose_windows_open(app).saturating_add(index),
            open: crate::widgets::item_windows_open(app),
        };
        let close = crate::widgets::item_window(
            ctx,
            vid,
            builder,
            READ_WINDOW_SIZE.into(),
            &theme,
            slot,
            |ui, class| read_window_ui(ui, app, id, class),
        );
        if close {
            closed.push(id);
        }
    }
    if !closed.is_empty() {
        app.ui.read_windows.retain(|i| !closed.contains(i));
        for id in closed {
            if !app.ui.read_windows.contains(&id) {
                ctx.data_mut(|d| d.remove::<u64>(folder_id(id)));
            }
            let key = cache_id(ctx, Surface::Read(id));
            ctx.data_mut(|d| d.remove::<MsgCache>(key));
            if !app.ui.read_windows.contains(&id) {
                crate::edit_message::stop(ctx, app, id);
            }
        }
    }
}

/// The viewport of `item`'s read window.
pub(crate) fn read_viewport_id(item: u64) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("mailcraft.read-window", item))
}

fn neighbour_id(item: u64) -> Id {
    Id::new(("mailcraft.read-window.neighbours", item))
}

/// Folder a read window's item was in when the window opened on it.
fn folder_id(item: u64) -> Id {
    Id::new(("mailcraft.read-window.folder", item))
}

/// "<subject> - Message (HTML)".
pub fn window_title(subject: &str, format: BodyFormat) -> String {
    let subject = if subject.trim().is_empty() { "(no subject)" } else { subject };
    format!("{subject} - Message ({})", format.title_label())
}

/// Short date form used when narrow: "Thu 10/8/2026 7:30 AM" → "Thu 7:30 AM".
pub fn short_date(date: &str) -> String {
    let parts: Vec<&str> = date.split_whitespace().collect();
    match parts.as_slice() {
        [day, d, rest @ ..] if d.contains('/') && !rest.is_empty() => {
            let mut s = (*day).to_owned();
            for r in rest {
                s.push(' ');
                s.push_str(r);
            }
            s
        }
        _ => date.to_owned(),
    }
}

/// Persona circle colour for a palette index (our own 16-colour palette).
pub fn persona_color(index: u8) -> Color32 {
    const PALETTE: [(u8, u8, u8); 16] = [
        (0x2e, 0x6d, 0xa4),
        (0x8a, 0x3f, 0x8f),
        (0x1f, 0x7a, 0x5c),
        (0xb5, 0x4a, 0x2a),
        (0x4b, 0x5d, 0xa8),
        (0x9c, 0x2f, 0x52),
        (0x2b, 0x7f, 0x8e),
        (0x7a, 0x6a, 0x1c),
        (0x5a, 0x4a, 0x9e),
        (0x3d, 0x7d, 0x2f),
        (0xa8, 0x3c, 0x3c),
        (0x26, 0x60, 0x7a),
        (0x8b, 0x55, 0x2b),
        (0x6b, 0x3f, 0x8f),
        (0x37, 0x6e, 0x55),
        (0x5f, 0x5f, 0x75),
    ];
    let (r, g, b) = PALETTE.get(usize::from(index) % PALETTE.len()).copied().unwrap_or((0x5f, 0x5f, 0x75));
    Color32::from_rgb(r, g, b)
}

// ---------------------------------------------------------------------------------------------------------
// State

/// Per-item toggles shared by the pane and the read windows.
#[derive(Clone, Debug, Default)]
struct Toggles {
    /// Items shown on a light background while the app is dark.
    light_bg: BTreeSet<u64>,
    /// Items allowed to load remote pictures.
    pictures: BTreeSet<u64>,
}

fn toggles_id() -> Id {
    Id::new("mailcraft.reading.toggles")
}

fn toggles(ctx: &egui::Context) -> Toggles {
    ctx.data(|d| d.get_temp::<Toggles>(toggles_id())).unwrap_or_default()
}

fn update_toggles(ctx: &egui::Context, f: impl FnOnce(&mut Toggles)) {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default::<Toggles>(toggles_id())));
}

/// Parsed body + layout cache for one surface.
#[derive(Clone, Debug, Default)]
struct MsgCache {
    item: u64,
    source_key: u64,
    doc: Arc<Doc>,
    doc_key: u64,
    remote_images: bool,
    view: BodyView,
}

fn cache_id(ctx: &egui::Context, surface: Surface) -> Id {
    surface_id(ctx, Id::new(("mailcraft.reading.cache", surface)), surface)
}

/// Cheap identity of a stored body (no full-content hash per frame).
fn source_key(id: u64, b: &mailcraft_model::Body) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    std::mem::discriminant(&b.format).hash(&mut h);
    for s in [&b.html, &b.text] {
        match s {
            Some(s) => {
                s.len().hash(&mut h);
                (s.as_ptr() as usize).hash(&mut h);
                s.get(..s.len().min(256)).hash(&mut h);
            }
            None => 0usize.hash(&mut h),
        }
    }
    h.finish()
}

/// The body to show. `unwrap`: Options › Mail › Message format › "Remove extra line breaks in plain text
/// messages" (plain-text messages only).
fn body_doc(b: &mailcraft_model::Body, plain: bool, unwrap: bool) -> Doc {
    let text_doc = |text: &str| {
        let joined = if unwrap && b.format == BodyFormat::PlainText {
            mailcraft_engine::cmd::compose::remove_extra_line_breaks(text)
        } else {
            None
        };
        Doc::from_plain_text(joined.as_deref().unwrap_or(text))
    };
    if plain {
        // Trust Center › "Read all standard mail in plain text".
        return match (&b.text, &b.html) {
            (Some(text), _) => text_doc(text),
            (None, Some(html)) => Doc::from_plain_text(&mailcraft_html::html_to_doc(html).to_plain_text()),
            (None, None) => Doc::default(),
        };
    }
    match (&b.html, &b.text, b.format) {
        (Some(html), _, BodyFormat::Html | BodyFormat::RichText) => mailcraft_html::html_to_doc(html),
        (_, Some(text), _) => text_doc(text),
        (Some(html), None, _) => mailcraft_html::html_to_doc(html),
        (None, None, _) => Doc::default(),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Find bar and selection requests

/// What the engine asked a message view to do (Find…, Select All, Select Text With Similar Formatting).
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Find {
        text: String,
        match_case: bool,
    },
    SelectAll,
    SelectSimilar,
    /// Selection Pane: object n (a picture) of the body.
    SelectObject(usize),
}

/// Accessible names of the find bar's controls.
pub const FIND_LABEL: &str = "Find in message";
pub const FIND_NEXT: &str = "Next";
pub const FIND_PREVIOUS: &str = "Previous";
pub const FIND_CLOSE: &str = "Close find";
/// Shown when nothing matches.
pub const NO_MATCHES: &str = "No results";

fn request_id(ctx: &egui::Context, surface: Surface) -> Id {
    surface_id(ctx, Id::new(("mailcraft.reading.request", surface)), surface)
}

fn find_id(ctx: &egui::Context, surface: Surface) -> Id {
    surface_id(ctx, Id::new(("mailcraft.reading.find", surface)), surface)
}

fn find_field_id(ctx: &egui::Context, surface: Surface) -> Id {
    surface_id(ctx, Id::new(("mailcraft.reading.find-field", surface)), surface)
}

/// Hands a Find / Select request for message `item` to the view showing it: its read window when one is
/// open, else the reading pane.
pub fn request(ctx: &egui::Context, app: &App, item: u64, r: Request) {
    let surface = if app.ui.read_windows.contains(&item) { Surface::Read(item) } else { Surface::Main };
    // Made from a dialog of the main window: explorer windows showing the item get it too (the view that
    // shows the item takes it; the others leave it).
    if surface == Surface::Main && ctx.viewport_id() == egui::ViewportId::ROOT {
        for w in &app.ui.explorers {
            if w.selection.focus == Some(ItemId(item)) || w.selection.items.contains(&ItemId(item)) {
                let id = salted(
                    Id::new(("mailcraft.reading.request", Surface::Main)),
                    crate::explorer::viewport_id(w.id),
                );
                ctx.data_mut(|d| d.insert_temp(id, (item, r.clone())));
            }
        }
    }
    let key = request_id(ctx, surface);
    ctx.data_mut(|d| d.insert_temp(key, (item, r)));
    ctx.request_repaint();
}

/// The find bar of one message view.
#[derive(Clone, Debug, Default)]
struct FindBar {
    open: bool,
    text: String,
    match_case: bool,
    /// Matches of `text` in the body (document order) and what they were found for.
    hits: Vec<(mailcraft_layout::TextPos, mailcraft_layout::TextPos)>,
    hits_for: Option<(u64, String, bool)>,
    /// The match shown as the selection.
    current: usize,
    /// Scroll the current match into view after the next paint.
    reveal: bool,
    /// Put keyboard focus in the text field.
    focus: bool,
}

fn find_bar_state(ctx: &egui::Context, surface: Surface) -> FindBar {
    let key = find_id(ctx, surface);
    ctx.data(|d| d.get_temp::<FindBar>(key)).unwrap_or_default()
}

/// Recomputes the matches when the text, case or message changed; keeps the current match in range.
fn refresh_hits(bar: &mut FindBar, cache: &MsgCache) {
    let key = (cache.doc_key, bar.text.clone(), bar.match_case);
    if bar.hits_for.as_ref() != Some(&key) {
        bar.hits = body::find_all(&cache.doc, &bar.text, bar.match_case);
        bar.hits_for = Some(key);
        bar.current = 0;
        bar.reveal = !bar.hits.is_empty();
    }
    if bar.current >= bar.hits.len() {
        bar.current = 0;
    }
}

/// Moves to the next (`back`: previous) match, wrapping around.
fn step(bar: &mut FindBar, back: bool) {
    let n = bar.hits.len();
    if n == 0 {
        return;
    }
    bar.current = if back { bar.current.checked_sub(1).unwrap_or(n - 1) } else { (bar.current + 1) % n };
    bar.reveal = true;
}

/// "3 of 12" / "No results".
pub fn match_label(current: usize, total: usize) -> String {
    if total == 0 { NO_MATCHES.to_owned() } else { format!("{} of {total}", current.min(total - 1) + 1) }
}

/// Applies a pending request to the message view, and the body menu's Find….
fn take_request(ctx: &egui::Context, surface: Surface, id: u64, bar: &mut FindBar, cache: &mut MsgCache) {
    if std::mem::take(&mut cache.view.find_requested) {
        bar.open = true;
        bar.focus = true;
        if let Some(t) = body::selected_text(&cache.view, &cache.doc).filter(|t| !t.contains('\n')) {
            bar.text = t.chars().take(200).collect();
        }
    }
    let key = request_id(ctx, surface);
    let Some((item, r)) = ctx.data(|d| d.get_temp::<(u64, Request)>(key)) else {
        return;
    };
    if item != id {
        return;
    }
    ctx.data_mut(|d| d.remove::<(u64, Request)>(key));
    match r {
        Request::Find { text, match_case } => {
            bar.open = true;
            bar.focus = true;
            if !text.is_empty() {
                bar.text = text;
                bar.match_case = match_case;
            }
        }
        Request::SelectAll => {
            body::select_all(&mut cache.view);
            focus_body(ctx, &cache.view);
        }
        Request::SelectSimilar => {
            let at = cache.view.ordered_selection().map(|(a, _)| a).or_else(|| {
                body::leaf_paths(&cache.doc)
                    .into_iter()
                    .next()
                    .map(|p| mailcraft_layout::TextPos { block_path: p, offset: 0 })
            });
            if let Some(at) = at {
                body::select_similar_at(&mut cache.view, &cache.doc, &at);
            }
            focus_body(ctx, &cache.view);
        }
        Request::SelectObject(n) => {
            let objects = mailcraft_engine::cmd::format::doc_objects(&cache.doc);
            if let Some(o) = objects.get(n) {
                let at = mailcraft_layout::TextPos { block_path: o.at.path.clone(), offset: o.at.offset };
                let end = mailcraft_layout::TextPos {
                    block_path: o.at.path.clone(),
                    offset: o.at.offset.saturating_add(1),
                };
                cache.view.selection = Some((at, end));
                cache.view.extra.clear();
                focus_body(ctx, &cache.view);
            }
        }
    }
}

/// Gives the body keyboard focus (so Ctrl+C copies what was selected).
fn focus_body(ctx: &egui::Context, view: &BodyView) {
    if let Some(w) = view.widget {
        ctx.memory_mut(|m| m.request_focus(w));
    }
}

/// The find bar row: text field, match count, Previous / Next, Match case, close.
fn find_bar(ui: &mut Ui, app: &App, surface: Surface, bar: &mut FindBar) {
    let theme = app.theme.clone();
    let mut close = false;
    egui::Frame::new().fill(theme.info_bar).inner_margin(egui::Margin::symmetric(PAD as i8, 4)).show(
        ui,
        |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let field_w = (ui.available_width() - 260.0).clamp(80.0, 320.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut bar.text)
                        .id(find_field_id(ui.ctx(), surface))
                        .hint_text("Find")
                        .desired_width(field_w),
                );
                let r = r.on_hover_text(FIND_LABEL);
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, FIND_LABEL));
                if std::mem::take(&mut bar.focus) {
                    r.request_focus();
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter {
                    let back = ui.input(|i| i.modifiers.shift);
                    step(bar, back);
                    r.request_focus();
                }
                let label = if bar.text.is_empty() {
                    String::new()
                } else {
                    match_label(bar.current, bar.hits.len())
                };
                ui.label(RichText::new(label).size(12.0).color(theme.text_muted));
                let any = !bar.hits.is_empty();
                if ui.add_enabled(any, egui::Button::new(FIND_PREVIOUS)).clicked() {
                    step(bar, true);
                }
                if ui.add_enabled(any, egui::Button::new(FIND_NEXT)).clicked() {
                    step(bar, false);
                }
                ui.checkbox(&mut bar.match_case, "Match case");
                let x = ui.button("\u{2715}").on_hover_text(FIND_CLOSE);
                x.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, FIND_CLOSE));
                if x.clicked() {
                    close = true;
                }
            });
        },
    );
    if close {
        bar.open = false;
    }
}

// ---------------------------------------------------------------------------------------------------------
// Pieces

fn focused_mail(app: &App) -> Option<u64> {
    let sel = &app.s.selection;
    sel.focus
        .into_iter()
        .chain(sel.items.iter().copied())
        .find(|id| app.s.mailbox.mail.contains(*id))
        .map(|id| id.0)
}

/// Inline response (compose with `inline`) belonging to item `id`, if any.
fn inline_compose_for(app: &App, id: u64) -> Option<u64> {
    app.s
        .composes
        .values()
        .find(|c| c.inline && (c.source == Some(ItemId(id)) || c.draft == Some(ItemId(id))))
        .map(|c| c.id)
}

fn empty_state(ui: &mut Ui, app: &mut App) {
    let avail = ui.available_rect_before_wrap();
    let center = Pos2::new(avail.center().x, avail.top() + (avail.height() * 0.4).max(60.0));
    let icon = Rect::from_center_size(center, Vec2::splat(56.0));
    app.icons.paint(ui, icon, "envelope-open", app.theme.text_muted);
    let label_rect =
        Rect::from_center_size(Pos2::new(center.x, center.y + 60.0), Vec2::new(avail.width(), 48.0));
    let mut open_options = false;
    ui.scope_builder(UiBuilder::new().max_rect(label_rect).layout(Layout::top_down(Align::Center)), |ui| {
        ui.label(RichText::new("Select an item to read").size(15.0).color(app.theme.text_muted));
        // layout.md: the empty pane also links to the preview settings.
        if app.mail_like_module()
            && app.s.enabled("view.readingPaneOptions").is_none()
            && ui.link(RichText::new("Reading pane options").size(12.5).color(app.theme.link)).clicked()
        {
            open_options = true;
        }
    });
    if open_options {
        app.run("view.readingPaneOptions", json!({}));
    }
}

/// Header + body of one message (reading pane or read window).
fn message_view(ui: &mut Ui, app: &mut App, id: u64, surface: Surface, narrow: bool) {
    // S/MIME: a signed or encrypted message is shown opened (verified, decrypted); a valid signer's
    // certificate is remembered for encrypted replies.
    mailcraft_engine::smime::on_view(&mut app.s, ItemId(id));
    // A message a server search found (Include Older Results) downloads in full when it is opened.
    mailcraft_engine::sync::older::opened(&mut app.s, ItemId(id));
    // A body the account's offline window took off this computer downloads again when it is opened online.
    mailcraft_engine::offline::opened(&mut app.s, ItemId(id));
    // Options › Language › Always translate: a message in another language is translated when first shown.
    mailcraft_engine::translate::auto_translate(&mut app.s, ItemId(id));
    let Some(h) = queries::reading_header(&app.s, ItemId(id)) else {
        empty_state(ui, app);
        return;
    };
    let ctx = ui.ctx().clone();
    let key = cache_id(&ctx, surface);
    let mut cache: MsgCache = ctx.data_mut(|d| d.remove_temp::<MsgCache>(key)).unwrap_or_default();
    // Junk Email Options › "Disable links and other functionality in phishing messages": a message in Junk
    // Email is shown as plain text, without pictures, and its links do not open.
    let links_off = mailcraft_engine::junk::links_disabled(&app.s, ItemId(id));
    let plain = app.s.settings.bool("trust.plainText") || links_off;
    let unwrap = app.s.settings.bool("mail.format.removeExtraLineBreaks");
    // Translate Message: a translation shows in place of the body (Show Original switches back).
    let translated = mailcraft_engine::translate::shown(&app.s, ItemId(id)).map(|t| t.text.clone());
    let translated_key = translated.as_ref().map_or(0, |t| {
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        t.hash(&mut hs);
        hs.finish() | 1
    });
    if let Some(m) = mailcraft_engine::smime::display_item(&app.s, ItemId(id)) {
        let shown_body = mailcraft_engine::smime::display_body(&app.s, &m);
        let sk = source_key(id, &shown_body)
            ^ u64::from(plain)
            ^ (u64::from(unwrap) << 1)
            ^ translated_key.rotate_left(3);
        if cache.item != id || cache.source_key != sk {
            // Inline attachments, so `cid:` pictures draw instead of the placeholder.
            for (src, data) in queries::inline_pictures(&app.s, ItemId(id)) {
                body::provide_image(&ctx, &src, &data);
            }
            let doc = match &translated {
                Some(t) => Doc::from_plain_text(t),
                None => body_doc(&shown_body, plain, unwrap),
            };
            cache = MsgCache {
                item: id,
                source_key: sk,
                doc_key: sk ^ 0x9e37_79b9_7f4a_7c15,
                remote_images: body::has_remote_images(&doc),
                doc: Arc::new(doc),
                view: BodyView::default(),
            };
        }
    }
    let tg = toggles(&ctx);
    // General › "Never change the message background color" keeps messages light in dark mode.
    let dark =
        app.theme.dark && !tg.light_bg.contains(&id) && !app.s.settings.bool("general.keepMessageBackground");
    // Trust Center › "Don't download pictures automatically" (on by default).
    let allow_remote = !links_off && (tg.pictures.contains(&id) || !app.s.settings.bool("trust.blockImages"));
    // A read window can be zoomed on its own (Message › Zoom); the reading pane uses the shared zoom.
    let pct = match surface {
        Surface::Read(_) => mailcraft_engine::cmd::view::read_zoom(&app.s, ItemId(id)),
        _ => app.s.view.zoom,
    };
    let zoom = match pct {
        0 => 1.0,
        z => f32::from(z) / 100.0,
    };
    let in_window = matches!(surface, Surface::Read(_));

    // Find bar and Select requests (Message › Find…, Select, the body's right-click menu).
    cache.view.context_menu = true;
    // The view belongs to this doc (a new doc gets a new cache), so marks set below survive its first paint.
    cache.view.doc_key = cache.doc_key;
    let mut bar = find_bar_state(&ctx, surface);
    take_request(&ctx, surface, id, &mut bar, &mut cache);
    if bar.open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        bar.open = false;
    }
    if bar.open {
        find_bar(ui, app, surface, &mut bar);
    }
    if bar.open {
        refresh_hits(&mut bar, &cache);
        cache.view.marks = bar.hits.clone();
        if bar.reveal
            && let Some(h) = bar.hits.get(bar.current)
        {
            cache.view.selection = Some(h.clone());
            cache.view.extra.clear();
        }
    } else if !cache.view.marks.is_empty() || bar.hits_for.is_some() {
        cache.view.marks.clear();
        bar.hits.clear();
        bar.hits_for = None;
    }
    let reveal = bar.open && std::mem::take(&mut bar.reveal);

    let mut clicked_link = None;
    let mut area = egui::ScrollArea::vertical()
        .id_salt(("mailcraft.reading.scroll", surface, id))
        .auto_shrink([false, false]);
    let pending_scroll = pending_scroll_id(&ctx);
    if surface == Surface::Main
        && let Some(y) = ctx.data_mut(|d| d.remove_temp::<f32>(pending_scroll))
    {
        area = area.vertical_scroll_offset(y);
    }
    let shown = area.show(ui, |ui| {
        ui.add_space(12.0);
        let head = egui::Frame::new().inner_margin(egui::Margin::symmetric(PAD as i8, 0)).show(ui, |ui| {
            let info_top = header(ui, app, &h, id, narrow, in_window);
            if cache.remote_images && !allow_remote {
                pictures_bar(ui, app, id);
            }
            header_only_bar(ui, app, id);
            translation_bar(ui, app, id);
            translate_offer_bar(ui, app, id);
            info_top
        });
        ui.add_space(10.0);
        // The page colour fills the pane from here down, edge to edge (not a box behind the text only); a
        // message without a background of its own shown dark takes the pane's colour.
        if let Some(fill) = body::page_fill(&cache.doc, dark) {
            let top = ui.cursor().top();
            let bottom = ui.clip_rect().bottom().max(top);
            let page = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=bottom);
            ui.painter().rect_filled(page, 0.0, fill);
        }
        let body_frame =
            egui::Frame::new().inner_margin(egui::Margin::symmetric(PAD as i8, 0)).show(ui, |ui| {
                let opts = BodyOptions {
                    dark,
                    zoom,
                    allow_remote,
                    paint_background: false,
                    doc_key: Some(cache.doc_key),
                };
                let doc = Arc::clone(&cache.doc);
                clicked_link = body::show_doc_with(ui, &mut cache.view, &doc, &opts);
                if reveal
                    && let Some(h) = bar.hits.get(bar.current)
                    && let Some(r) = body::range_rect(&cache.view, h)
                {
                    ui.scroll_to_rect(r.expand(24.0), Some(Align::Center));
                }
            });
        ui.add_space(PAD);
        // F6 stops (read windows): the header, its info bars, the body, as far as they show.
        let (r, top) = (head.response.rect, head.inner);
        let visible = ui.clip_rect();
        let header = egui::Rect::from_x_y_ranges(r.x_range(), r.top()..=top.clamp(r.top(), r.bottom()));
        let info = egui::Rect::from_x_y_ranges(r.x_range(), top.clamp(r.top(), r.bottom())..=r.bottom());
        let stops = [header, info, body_frame.response.rect].map(|s| s.intersect(visible));
        let key = stops_id(&ctx, surface);
        ctx.data_mut(|d| d.insert_temp(key, stops));
    });
    if surface == Surface::Main {
        let m = ReadScroll {
            item: id,
            offset: shown.state.offset.y,
            content: shown.content_size.y,
            view: shown.inner_rect.height(),
        };
        let key = scroll_id(&ctx);
        ctx.data_mut(|d| d.insert_temp(key, m));
    }
    // The body's Find… (right-click) opens the bar on the next frame.
    if cache.view.find_requested {
        ctx.request_repaint();
    }
    let find_key = find_id(&ctx, surface);
    ctx.data_mut(|d| d.insert_temp(find_key, bar));
    ctx.data_mut(|d| d.insert_temp(key, cache));
    if let Some(href) = clicked_link
        && !links_off
    {
        open_link(&ctx, app, &href);
    }
}

/// The message header (subject, people, categories, attachments, then the info bars). Returns where the
/// info bars start (screen y), for the read window's F6 stops.
fn header(ui: &mut Ui, app: &mut App, h: &ReadingHeader, id: u64, narrow: bool, in_window: bool) -> f32 {
    let theme = app.theme.clone();
    // 1. Subject.
    let subject = if h.subject.trim().is_empty() { "(no subject)" } else { h.subject.as_str() };
    let subject_size = if in_window { 18.0 } else { 20.0 };
    // Search words in the subject on the highlight background (File › Options › Search).
    let hits = if app.s.settings.bool("search.highlight") {
        mailcraft_engine::search_index::highlights(
            &app.s,
            mailcraft_engine::search_index::Field::Subject,
            subject,
        )
    } else {
        Vec::new()
    };
    if hits.is_empty() {
        ui.add(egui::Label::new(RichText::new(subject).size(subject_size).strong().color(theme.text)).wrap());
    } else {
        let format = egui::TextFormat {
            font_id: FontId::proportional(subject_size),
            color: theme.text,
            ..Default::default()
        };
        let job = crate::list::highlight_job(subject.to_owned(), &format, &hits, theme.search_hit);
        ui.add(egui::Label::new(job).wrap());
    }
    ui.add_space(10.0);

    // 2. Persona + addresses (left) and actions + date (right).
    let full = ui.available_rect_before_wrap();
    let right_w: f32 = if narrow { 150.0 } else { 300.0 };
    let right_w = right_w.min(full.width() * 0.6);
    let right_rect =
        Rect::from_min_size(Pos2::new(full.right() - right_w, full.top()), Vec2::new(right_w, 80.0));
    let mut right = ui.new_child(UiBuilder::new().max_rect(right_rect).layout(Layout::top_down(Align::Max)));
    actions(&mut right, app, id, narrow, in_window);
    let date = if narrow { short_date(&h.date) } else { h.date.clone() };
    if !date.is_empty() {
        right.add_space(4.0);
        right.label(RichText::new(date).size(12.0).color(theme.text_muted));
    }
    // Mail › Other › "Show Next and Previous links in the message headers when reading messages".
    if in_window && app.s.settings.bool("mail.other.nextPreviousLinks") {
        right.add_space(2.0);
        let mut step = None;
        right.allocate_ui_with_layout(Vec2::new(right_w, 20.0), Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let order = queries::visible_item_ids(&app.s);
            let pos = order.iter().position(|i| i.0 == id);
            let has_next = pos.is_some_and(|p| p.saturating_add(1) < order.len());
            let has_prev = pos.is_some_and(|p| p > 0);
            for (next, label, name, on) in
                [(true, "Next", NEXT_LINK, has_next), (false, "Previous", PREVIOUS_LINK, has_prev)]
            {
                let r =
                    ui.add_enabled(on, egui::Link::new(RichText::new(label).size(12.0).color(theme.link)));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, on, name));
                if r.clicked() {
                    step = Some(next);
                }
            }
        });
        if let Some(next) = step {
            read_step(app, id, next);
        }
    }
    let right_used = right.min_rect();

    // The sender line ends a clear gap before the button cluster, which can be wider than its share (the dark
    // theme's background toggle adds a button).
    let right_left = right_used.left().min(right_rect.left());
    let left_w = (right_left - full.left() - HEADER_GAP).max(60.0);
    ui.allocate_ui_with_layout(Vec2::new(left_w, 0.0), Layout::left_to_right(Align::Min), |ui| {
        let d = if in_window { 48.0 } else { 40.0 };
        let (r, _) = ui.allocate_exact_size(Vec2::splat(d), Sense::hover());
        let c = r.center();
        // People › "Show user photographs when available": the sender's contact picture.
        let photo = h
            .photo
            .and_then(|cid| app.s.mailbox.contacts.get(&cid))
            .is_some_and(|contact| crate::modules::paint_contact_picture(ui, r, contact));
        if !photo {
            ui.painter().circle_filled(c, d / 2.0, persona_color(h.persona_color));
            ui.painter().text(
                c,
                egui::Align2::CENTER_CENTER,
                &h.from_initials,
                FontId::proportional(d * 0.36),
                Color32::WHITE,
            );
        }
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.set_max_width((left_w - d - 12.0).max(40.0));
            ui.spacing_mut().item_spacing.y = 2.0;
            let from = if h.from.is_empty() { "(unknown sender)" } else { h.from.as_str() };
            ui.add(egui::Label::new(RichText::new(from).size(14.0).strong().color(theme.text)).truncate());
            if !h.to.is_empty() {
                recipients_line(ui, "To:", &h.to, &theme);
            }
            if !h.cc.is_empty() {
                recipients_line(ui, "Cc:", &h.cc, &theme);
            }
        });
    });
    ui.advance_cursor_after_rect(right_used);

    // 3. Categories.
    if !h.categories.is_empty() {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            for c in &h.categories {
                category_chip(ui, app, c);
            }
        });
    }
    // 4. Attachments.
    if !h.attachments.is_empty() {
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            for (i, (name, size)) in h.attachments.iter().enumerate() {
                attachment_chip(ui, app, id, i, name, size);
            }
        });
    }
    // 5. Info bars. S/MIME first: encryption and the digital signature, with Details.
    let info_top = ui.cursor().top();
    security_bars(ui, app, id);
    for line in &h.info {
        ui.add_space(6.0);
        info_bar(ui, app, "info", line, None);
    }
    // Junk Email: the folder notice and the suspicious-domain warning.
    let junk = mailcraft_engine::junk::reading_info(&app.s, ItemId(id));
    for (key, icon) in [("infobar", "junk"), ("warning", "warning")] {
        if let Some(line) = junk.get(key).and_then(serde_json::Value::as_str) {
            ui.add_space(6.0);
            info_bar(ui, app, icon, line, None);
        }
    }
    // Voting buttons, vote tallies, delivery / read receipts and read-receipt requests.
    let policy = crate::compose::ReceiptPolicy::from_setting(
        &app.s.settings.text("mail.tracking.readReceiptResponse"),
    );
    crate::compose::tracking_bars(ui, app, ItemId(id), policy);
    // 6. Meeting requests, responses, cancellations and proposals; task requests, responses and status
    // reports (nothing for ordinary mail).
    crate::modules::meeting_bar(ui, app, id);
    info_top
}

fn stops_id(ctx: &egui::Context, surface: Surface) -> egui::Id {
    surface_id(ctx, egui::Id::new(("mailcraft.reading.stops", surface.key())), surface)
}

/// Where the message view of `surface` drew its header, info bars and body in the last frame (visible
/// parts; empty rectangles when there were none): the read window's F6 stops.
fn view_stops(ctx: &egui::Context, surface: Surface) -> [egui::Rect; 3] {
    let key = stops_id(ctx, surface);
    ctx.data(|d| d.get_temp::<[egui::Rect; 3]>(key)).unwrap_or([egui::Rect::NOTHING; 3])
}

fn recipients_line(ui: &mut Ui, label: &str, names: &str, theme: &crate::theme::Theme) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new(label).size(12.0).color(theme.text_muted));
        ui.add(egui::Label::new(RichText::new(names).size(12.0).color(theme.text)).truncate());
    });
}

/// Background toggle (dark mode), Reply | Reply All | Forward, "…".
fn actions(ui: &mut Ui, app: &mut App, id: u64, narrow: bool, in_window: bool) {
    let mut action: Option<(&'static str, serde_json::Value)> = None;
    let respond_params = if in_window { json!({"ids": [id], "window": true}) } else { json!({"ids": [id]}) };
    // One 30-px row at the top of the block (a bare `with_layout` would centre the buttons in the whole
    // block's height and push the date below the persona).
    let row = Vec2::new(ui.available_width(), 30.0);
    ui.allocate_ui_with_layout(row, Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        // "…" menu.
        let more = plain_icon_button(ui, app, "more", None, "More actions");
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(200.0);
            let items: &[(&str, &'static str)] = &[
                ("Reply", "mail.reply"),
                ("Reply All", "mail.replyAll"),
                ("Forward", "mail.forward"),
                ("Reply with Meeting", "mail.replyWithMeeting"),
                ("Forward as Attachment", "mail.forwardAsAttachment"),
            ];
            for (label, cmd) in items {
                if ui.button(*label).clicked() {
                    let p = if *cmd == "mail.forwardAsAttachment" || *cmd == "mail.replyWithMeeting" {
                        json!({"ids": [id]})
                    } else {
                        respond_params.clone()
                    };
                    action = Some((cmd, p));
                }
            }
            ui.separator();
            let items2: &[(&str, &'static str)] = &[
                ("Delete", "mail.delete"),
                ("Mark as Unread", "mail.markUnread"),
                ("Flag", "mail.flag"),
                ("Mark Complete", "mail.markComplete"),
                ("Block Sender", "mail.block"),
            ];
            for (label, cmd) in items2 {
                if ui.button(*label).clicked() {
                    action = Some((cmd, json!({"ids": [id]})));
                }
            }
            ui.separator();
            if !in_window && ui.button("Open in New Window").clicked() {
                action = Some(("mail.open", json!({"ids": [id]})));
            }
            if ui.button("Print").clicked() {
                action = Some(("mail.print", json!({"ids": [id]})));
            }
            if ui.button("View Source").clicked() {
                action = Some(("mail.viewSource", json!({"ids": [id]})));
            }
        });

        // Segmented Reply | Reply All | Forward (drawn right to left).
        let seg_rect_start = ui.cursor();
        let labels: [(&str, &str, &'static str); 3] = [
            ("forward", "Forward", "mail.forward"),
            ("reply-all", "Reply All", "mail.replyAll"),
            ("reply", "Reply", "mail.reply"),
        ];
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut seg = Rect::NOTHING;
        for (n, (icon, label, cmd)) in labels.iter().enumerate() {
            let r = plain_icon_button(ui, app, icon, (!narrow).then_some(*label), label);
            if n < 2 {
                // Divider on the left side of this segment.
                let x = r.rect.left();
                ui.painter().line_segment(
                    [Pos2::new(x, r.rect.top() + 5.0), Pos2::new(x, r.rect.bottom() - 5.0)],
                    Stroke::new(1.0, app.theme.separator),
                );
            }
            seg = seg.union(r.rect);
            if r.clicked() {
                action = Some((cmd, respond_params.clone()));
            }
        }
        let _ = seg_rect_start;
        ui.painter().rect_stroke(seg, 4.0, Stroke::new(1.0, app.theme.separator), StrokeKind::Inside);
        ui.spacing_mut().item_spacing.x = 4.0;

        // Background toggle, dark mode only.
        if app.theme.dark {
            let light = toggles(ui.ctx()).light_bg.contains(&id);
            let tip = if light { "Switch background to dark" } else { "Switch background to light" };
            let r = sun_button(ui, app, light, tip);
            if r.clicked() {
                update_toggles(ui.ctx(), |t| {
                    if !t.light_bg.remove(&id) {
                        t.light_bg.insert(id);
                    }
                });
            }
        }
    });
    if let Some((cmd, p)) = action {
        app.run(cmd, p);
    }
}

fn attachment_chip(ui: &mut Ui, app: &mut App, id: u64, index: usize, name: &str, size: &str) {
    let theme = app.theme.clone();
    let mut action: Option<(&'static str, serde_json::Value)> = None;
    let inner = egui::Frame::new()
        .stroke(Stroke::new(1.0, theme.separator))
        .corner_radius(4.0)
        .fill(theme.pane)
        .inner_margin(egui::Margin { left: 8, right: 2, top: 4, bottom: 4 })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (r, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                app.icons.paint(ui, r, "attach", theme.accent);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.set_max_width(180.0);
                    ui.add(egui::Label::new(RichText::new(name).size(12.0).color(theme.text)).truncate());
                    ui.label(RichText::new(size).size(11.0).color(theme.text_muted));
                });
                let menu = plain_icon_button(ui, app, "chevron-down", None, "Attachment options");
                egui::Popup::menu(&menu).show(|ui| {
                    ui.set_min_width(170.0);
                    ui.add_enabled(false, egui::Button::new("Open"))
                        .on_disabled_hover_text("Opening attachments isn't available yet. Use Save As.");
                    if ui.button("Save As").clicked() {
                        action = Some((
                            "file.saveAttachments",
                            json!({"ids": [id], "index": index, "name": name}),
                        ));
                    }
                    if ui.button("Save All Attachments").clicked() {
                        action = Some(("file.saveAttachments", json!({"ids": [id]})));
                    }
                });
            });
        });
    inner
        .response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, format!("{name} {size}")));
    if let Some((cmd, p)) = action {
        app.run(cmd, p);
    }
}

fn category_chip(ui: &mut Ui, app: &mut App, name: &str) {
    let theme = app.theme.clone();
    egui::Frame::new()
        .fill(theme.group_header)
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                app.icons.paint(ui, r, "category-tag", theme.accent);
                ui.label(RichText::new(name).size(11.0).color(theme.text));
            });
        });
}

fn info_bar(ui: &mut Ui, app: &mut App, icon: &str, text: &str, link: Option<&str>) -> bool {
    let theme = app.theme.clone();
    let mut clicked = false;
    egui::Frame::new()
        .fill(theme.info_bar)
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                app.icons.paint(ui, r, icon, theme.text_muted);
                ui.label(RichText::new(text).size(12.0).color(theme.text));
                if let Some(l) = link
                    && ui.link(RichText::new(l).size(12.0).color(theme.link)).clicked()
                {
                    clicked = true;
                }
            });
        });
    clicked
}

/// The S/MIME info bars of message `id`: "Encrypted…" or why it cannot be opened, and who signed it with the
/// signature's status; Details opens the signature details (`mail.signatureDetails`).
fn security_bars(ui: &mut Ui, app: &mut App, id: u64) {
    let Some(sec) = mailcraft_engine::smime::security(&app.s, ItemId(id)) else { return };
    let lines = mailcraft_engine::smime::info_lines(&sec);
    let last = lines.len().saturating_sub(1);
    for (i, (kind, text)) in lines.iter().enumerate() {
        let icon = match *kind {
            "ok" => "certificate",
            "info" => "private",
            "warning" => "warning",
            _ => "error",
        };
        ui.add_space(6.0);
        let link = (i == last).then_some("Details...");
        if info_bar(ui, app, icon, text, link) {
            app.run("mail.signatureDetails", json!({"id": id}));
        }
    }
}

/// Translate Message on a received message: which translation shows, with a switch to the original.
fn translation_bar(ui: &mut Ui, app: &mut App, id: u64) {
    if mailcraft_engine::translate::pending(&app.s, ItemId(id)) {
        // The translation server is working on it (on a worker thread; the window stays usable).
        ui.add_space(6.0);
        info_bar(ui, app, "email", "Translating this message…", None);
        return;
    }
    let Some(t) = app.s.translations.get(&ItemId(id)).cloned() else { return };
    let lang = mailcraft_engine::translate::language_name(&t.to).unwrap_or(t.to.as_str()).to_owned();
    let (text, action) = if t.show_original {
        (format!("This is the original message. A translation into {lang} is available."), "Show translation")
    } else {
        (format!("Translated into {lang} by {}.", t.by), "Show original")
    };
    ui.add_space(6.0);
    if info_bar(ui, app, "email", &text, Some(action)) {
        // Show Original switches between the translation and the original.
        app.run("review.translate", json!({"id": id, "showOriginal": true}));
    }
}

/// A message of which only the header was downloaded: say so, with a link that downloads the rest now.
fn header_only_bar(ui: &mut Ui, app: &mut App, id: u64) {
    // A body the offline window took off: say why, and offer the download unless working offline.
    if let Some(text) = mailcraft_engine::offline::placeholder(&app.s, ItemId(id)) {
        ui.add_space(6.0);
        let link = (!app.s.view.work_offline).then_some("Download it now");
        if info_bar(ui, app, "sync", &text, link) {
            app.run("sendReceive.markToDownload", json!({"ids": [id], "now": true}));
        }
        return;
    }
    if !queries::is_server_header_only(&app.s, ItemId(id)) {
        return;
    }
    ui.add_space(6.0);
    let text = "Only the header of this message has been downloaded.";
    if info_bar(ui, app, "sync", text, Some("Download the rest of the message")) {
        app.run("sendReceive.markToDownload", json!({"ids": [id], "now": true}));
    }
}

/// Options › Language › Ask me before translating: a message that seems to be in another language offers
/// a translation (the language is told on this computer; nothing is sent until the link is clicked).
fn translate_offer_bar(ui: &mut Ui, app: &mut App, id: u64) {
    let Some((from, to)) = mailcraft_engine::translate::offer(&app.s, ItemId(id)) else { return };
    let name = |t: &str| mailcraft_engine::translate::language_name(t).unwrap_or(t).to_owned();
    let text = format!("This message seems to be in {}.", name(from));
    let link = format!("Translate it into {}", name(&to));
    ui.add_space(6.0);
    if info_bar(ui, app, "email", &text, Some(&link)) {
        app.run("review.translate", json!({"id": id}));
    }
}

fn pictures_bar(ui: &mut Ui, app: &mut App, id: u64) {
    ui.add_space(6.0);
    if info_bar(
        ui,
        app,
        "picture",
        "Some pictures in this message weren't downloaded, to protect your privacy.",
        Some("Download pictures"),
    ) {
        update_toggles(ui.ctx(), |t| {
            t.pictures.insert(id);
        });
    }
}

/// Flat icon button (optional label), hover/press feedback, accessible name = label or tooltip.
fn plain_icon_button(
    ui: &mut Ui,
    app: &mut App,
    icon: &str,
    label: Option<&str>,
    tooltip: &str,
) -> egui::Response {
    let theme = app.theme.clone();
    let font = FontId::proportional(13.0);
    let galley = label.map(|l| ui.painter().layout_no_wrap(l.to_owned(), font, theme.text));
    let text_w = galley.as_ref().map(|g| g.size().x + 6.0).unwrap_or(0.0);
    let w = 8.0 + 16.0 + text_w + 8.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 28.0), Sense::click());
    if resp.hovered() || resp.has_focus() {
        let fill = if resp.is_pointer_button_down_on() { theme.selected } else { theme.hover };
        ui.painter().rect_filled(rect.shrink(1.0), 3.0, fill);
    }
    let ir = Rect::from_min_size(Pos2::new(rect.left() + 8.0, rect.center().y - 8.0), Vec2::splat(16.0));
    app.icons.paint(ui, ir, icon, theme.text);
    if let Some(g) = galley {
        let pos = Pos2::new(ir.right() + 6.0, rect.center().y - g.size().y / 2.0);
        ui.painter().galley(pos, g, theme.text);
    }
    let name = label.unwrap_or(tooltip).to_owned();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name.clone()));
    if label.is_none() || label != Some(tooltip) { resp.on_hover_text(tooltip.to_owned()) } else { resp }
}

/// The background toggle: our own sun glyph (filled when the light background is on).
fn sun_button(ui: &mut Ui, app: &mut App, on: bool, tooltip: &str) -> egui::Response {
    let theme = app.theme.clone();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(32.0, 28.0), Sense::click());
    if resp.hovered() || on {
        ui.painter().rect_filled(rect.shrink(1.0), 3.0, if on { theme.selected } else { theme.hover });
    }
    let c = rect.center();
    let ink = theme.text;
    if on {
        ui.painter().circle_filled(c, 4.0, ink);
    } else {
        ui.painter().circle_stroke(c, 4.0, Stroke::new(1.3, ink));
    }
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::FRAC_PI_4;
        let d = Vec2::new(a.cos(), a.sin());
        ui.painter().line_segment([c + d * 6.0, c + d * 8.5], Stroke::new(1.3, ink));
    }
    let name = tooltip.to_owned();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, name.clone()));
    resp.on_hover_text(tooltip.to_owned())
}

/// Opens a clicked body link: mailto → new message to that address; http(s) → system browser.
fn open_link(ctx: &egui::Context, app: &mut App, href: &str) {
    let h = href.trim();
    let lower = h.to_ascii_lowercase();
    if lower.starts_with("mailto:") {
        // RFC 6068: recipients, subject and body (Options › Advanced › mailto: UTF-8 support).
        app.run("mail.newFromMailto", json!({"url": h}));
    } else if lower.starts_with("http://") || lower.starts_with("https://") {
        if app.s.settings.bool("trust.warnLinks") {
            // Trust Center › "Ask before opening web links in messages": confirm first.
            ctx.data_mut(|d| d.insert_temp(link_prompt_id(), h.to_owned()));
        } else {
            ctx.open_url(egui::OpenUrl::new_tab(h));
        }
    }
}

fn link_prompt_id() -> Id {
    Id::new("mailcraft.reading.link-prompt")
}

/// The "open this link?" prompt of Trust Center › "Ask before opening web links in messages".
fn link_prompt(ctx: &egui::Context, app: &mut App) {
    let Some(url) = ctx.data_mut(|d| d.get_temp::<String>(link_prompt_id())) else { return };
    let mut done = false;
    egui::Modal::new(link_prompt_id().with("modal")).show(ctx, |ui| {
        ui.set_max_width(420.0);
        crate::dialogs::form_visuals(ui, &app.theme);
        ui.label(RichText::new("Open this link?").strong());
        let shown: String = url.chars().take(300).collect();
        ui.label(RichText::new(shown).color(app.theme.link));
        ui.horizontal(|ui| {
            if ui.button("Open").clicked() {
                ctx.open_url(egui::OpenUrl::new_tab(&url));
                done = true;
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                done = true;
            }
        });
    });
    if done {
        ctx.data_mut(|d| d.remove::<String>(link_prompt_id()));
    }
}

// ---------------------------------------------------------------------------------------------------------
// Read windows

/// The read window's Quick Access row (read-window.md): Save, Undo, Redo, Previous Item, Next Item, plus Close
/// at the right when the window is embedded (its egui frame has no close button). `tiny` keeps only Close;
/// `unsaved` (Edit Message changes waiting) enables Save, which saves them, and disables the item steps.
/// Returns whether Close was clicked.
fn read_quick_access(ui: &mut Ui, app: &mut App, id: u64, embedded: bool, tiny: bool, unsaved: bool) -> bool {
    let mut close = false;
    let can_undo = crate::keys::enabled(&app.s, "app.undo");
    let can_redo = crate::keys::enabled(&app.s, "app.redo");
    let order = queries::visible_item_ids(&app.s);
    let pos = order.iter().position(|i| i.0 == id);
    let neighbour = |step: Option<usize>| {
        step.and_then(|p| order.get(p)).is_some_and(|n| !app.ui.read_windows.contains(&n.0))
    };
    let has_prev = !unsaved && neighbour(pos.and_then(|p| p.checked_sub(1)));
    let has_next = !unsaved && neighbour(pos.map(|p| p.saturating_add(1)));
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), crate::compose::QAT_H),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_space(8.0);
            if !tiny {
                // Save keeps Edit Message changes (Save As, to a file, is in File); with nothing being
                // edited there is nothing to save.
                if crate::widgets::icon_only(ui, app, "save", "Save", unsaved).clicked() {
                    let ctx = ui.ctx().clone();
                    crate::edit_message::save(&ctx, app, id);
                }
                if crate::widgets::icon_only(ui, app, "undo", "Undo", can_undo).clicked() {
                    app.run("app.undo", json!({}));
                }
                if crate::widgets::icon_only(ui, app, "redo", "Redo", can_redo).clicked() {
                    app.run("app.redo", json!({}));
                }
                if crate::widgets::icon_only(ui, app, "chevron-up", "Previous Item", has_prev).clicked() {
                    read_step(app, id, false);
                }
                if crate::widgets::icon_only(ui, app, "chevron-down", "Next Item", has_next).clicked() {
                    read_step(app, id, true);
                }
            }
            if embedded {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(8.0);
                    if crate::widgets::icon_only(ui, app, "close", "Close", true).clicked() {
                        close = true;
                    }
                });
            }
        },
    );
    close
}

/// One read window's contents; returns true when it should close.
fn read_window_ui(ui: &mut Ui, app: &mut App, id: u64, class: egui::ViewportClass) -> bool {
    let ctx = ui.ctx().clone();
    let embedded = class == egui::ViewportClass::EmbeddedWindow;
    let mut close = !embedded && ctx.input(|i| i.viewport().close_requested());
    // Mail › "Close original message window when replying or forwarding".
    let responses = |app: &App| app.s.composes.values().filter(|c| c.source == Some(ItemId(id))).count();
    let responses_before = responses(app);
    // Edit Message: the message is edited in place; its keys belong to the editor, not the message commands.
    let editing = crate::edit_message::is_editing(app, id);
    if !embedded && !editing {
        // A real window has its own input; embedded ones share the main window's (handled there).
        crate::keys::handle(&ctx, app, Surface::Read(id));
        if !app.ui.read_windows.contains(&id) {
            return true;
        }
    }
    let size = if embedded { ui.available_size() } else { ctx.content_rect().size() };
    let frame = Frame::compute(size.x, size.y);
    let narrow = size.x <= NARROW_W;
    let backstage_here = matches!(&app.ui.backstage, Some((Surface::Read(s), _)) if *s == id);
    egui::Frame::new().fill(app.theme.reading).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        // File: the window's own backstage, over the whole window (Escape goes back to the message).
        if backstage_here {
            crate::backstage::window_backstage(ui, app, Surface::Read(id));
            return;
        }
        // Unsaved Edit Message changes: the window's commands wait until they are saved or discarded
        // (Reply, Move or Delete would act on the stored message, not on what is being edited).
        let unsaved =
            crate::edit_message::is_editing(app, id) && crate::edit_message::has_unsaved_edits(&ctx, id);
        // A tiny window (responsive.md: 300×200) keeps the message and drops the Quick Access buttons, the
        // tabs and the ribbon, as compose does (an embedded one keeps its Close button).
        if (!frame.inspector_tiny || embedded)
            && read_quick_access(ui, app, id, embedded, frame.inspector_tiny, unsaved)
        {
            close = true;
        }
        let ribbon_top = ui.cursor().top();
        if !frame.inspector_tiny {
            crate::compose::tab_row(ui, app, Surface::Read(id));
            ui.add_enabled_ui(!unsaved, |ui| crate::ribbon::ribbon(ui, app, Surface::Read(id), &frame))
                .response
                .on_disabled_hover_text(crate::edit_message::RIBBON_WAITS_NOTE);
            ui.separator();
        }
        let ribbon_bottom = ui.cursor().top().max(ribbon_top);
        let ribbon = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), ribbon_top..=ribbon_bottom);
        if crate::edit_message::is_editing(app, id) {
            if crate::edit_message::editor_ui(ui, app, id).close {
                close = true;
            }
            return;
        }
        message_view(ui, app, id, Surface::Read(id), narrow);
        // F6 (the main window's rule): header, body, info bars, ribbon (the selected tab).
        let [header, info, body] = view_stops(&ctx, Surface::Read(id));
        let stop = |name, rect| crate::keys::WindowStop { name, rect, target: None };
        crate::keys::window_stops(
            ui,
            app,
            Surface::Read(id),
            vec![stop("header", header), stop("body", body), stop("infoBar", info), stop("ribbon", ribbon)],
        );
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx) {
        close = true;
    }
    if responses(app) > responses_before && app.s.settings.bool("mail.reply.closeOriginal") {
        close = true;
    }
    // A message with unsaved edits asks before its window closes (the question shows in the window).
    if close && !crate::edit_message::may_close(&ctx, app, id) {
        if !embedded {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        close = false;
    }
    close
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use mailcraft_engine::sample::sample_session;

    fn app_with(subject: &str) -> App {
        let mut s = sample_session();
        let id = s.mailbox.mail.items().find(|m| m.subject == subject).map(|m| m.id).unwrap();
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        App::new(s)
    }

    fn harness(app: App) -> Harness<'static, App> {
        Harness::builder().with_size(Vec2::new(900.0, 700.0)).build_ui_state(
            |ui, app: &mut App| {
                let frame = Frame { width: 1400.0, height: 900.0, ..Frame::default() };
                reading_pane(ui, app, &frame);
            },
            app,
        )
    }

    #[test]
    fn mark_as_read_after_the_wait_and_space_bar_reading() {
        let s = sample_session();
        let subject =
            s.mailbox.mail.items().find(|m| s.body_or_empty(m).html.is_some()).unwrap().subject.clone();
        let mut app = app_with(&subject);
        let id = app.s.selection.focus.unwrap();
        app.s.mailbox.set_read(&[id], false);
        app.run("options.set", json!({"key": "mail.readingPane.markReadWhenViewed", "value": true}));
        app.run("options.set", json!({"key": "mail.readingPane.markReadAfterSeconds", "value": 0}));
        let mut h = harness(app);
        h.run_steps(2);
        assert!(h.state().s.mailbox.mail.get(id).unwrap().read, "marked read after 0 s");
        // Option off: viewing leaves it unread.
        let mut app = h.into_state();
        app.s.mailbox.set_read(&[id], false);
        app.run("options.set", json!({"key": "mail.readingPane.markReadWhenViewed", "value": false}));
        let mut h = harness(app);
        h.run_steps(2);
        assert!(!h.state().s.mailbox.mail.get(id).unwrap().read);
        // Space pages the message, then moves on.
        let ctx = h.ctx.clone();
        let mut moved = false;
        for _ in 0..50 {
            let app = h.state_mut();
            space_read(&ctx, app, false);
            h.run_steps(1);
            if h.state().s.selection.focus != Some(id) {
                moved = true;
                break;
            }
        }
        assert!(moved, "Space reached the next item");
    }

    #[test]
    fn inline_cid_pictures_reach_the_body() {
        let s = sample_session();
        let subject =
            s.mailbox.mail.items().find(|m| s.body_or_empty(m).html.is_some()).unwrap().subject.clone();
        let mut app = app_with(&subject);
        let id = app.s.selection.focus.unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(6, 4, image::Rgba([0, 120, 215, 255]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let b = mailcraft_mime::MessageBuilder {
            from: Some(mailcraft_mime::Mailbox::new(None, "ann@example.org")),
            subject: subject.clone(),
            text: Some("Logo".into()),
            html: Some("<p>Logo: <img src=\"cid:logo@example.org\" width=6 height=4></p>".into()),
            attachments: vec![mailcraft_mime::AttachmentSpec {
                filename: "logo.png".into(),
                mime_type: "image/png".into(),
                data: png.into_inner(),
                content_id: Some("logo@example.org".into()),
            }],
            ..Default::default()
        };
        let raw = b.build();
        let msg = mailcraft_mime::parse(&raw).unwrap();
        let mut built = mailcraft_model::MailItem::from_message(&msg, &raw, app.s.now());
        if let Some(m) = app.s.mailbox.mail.get_mut(id) {
            m.attachments = std::mem::take(&mut built.attachments);
        }
        app.s.set_source(id, Some(&raw)).unwrap();
        let mut h = harness(app);
        h.run();
        assert_eq!(body::image_texture(&h.ctx, "cid:logo@example.org").map(|t| t.size()), Some([6, 4]));
    }

    #[test]
    fn trust_center_and_background_options_render() {
        let s = sample_session();
        let subject =
            s.mailbox.mail.items().find(|m| s.body_or_empty(m).html.is_some()).unwrap().subject.clone();
        let mut app = app_with(&subject);
        for (k, v) in
            [("trust.plainText", true), ("trust.blockImages", false), ("general.keepMessageBackground", true)]
        {
            app.run("options.set", json!({"key": k, "value": v}));
        }
        app.run("options.set", json!({"key": "view.darkMode", "value": true}));
        let mut h = harness(app);
        h.run();
        h.get_by_label(&subject);
        let plain = body_doc(
            &mailcraft_model::Body {
                format: BodyFormat::Html,
                text: None,
                html: Some("<p>Hi <b>there</b></p>".into()),
            },
            true,
            true,
        );
        assert!(plain.to_plain_text().contains("Hi there"));
    }

    #[test]
    fn extra_line_breaks_in_plain_text_follow_the_option() {
        let text = "A plain text message that the sender's mail program wrapped at a\n\
                    fixed width, so its lines end in the middle of sentences here.\n\nBye";
        let body = |format| mailcraft_model::Body { format, text: Some(text.into()), html: None };
        let paragraphs = |doc: &Doc| doc.to_plain_text().lines().count();
        // On (the default): one paragraph, a blank line, the sign-off.
        let on = body_doc(&body(BodyFormat::PlainText), false, true);
        assert_eq!(paragraphs(&on), 3, "{}", on.to_plain_text());
        assert!(on.to_plain_text().contains("wrapped at a fixed width"));
        // Off, or an HTML message's text part: the breaks stay.
        assert_eq!(paragraphs(&body_doc(&body(BodyFormat::PlainText), false, false)), 4);
        assert_eq!(paragraphs(&body_doc(&body(BodyFormat::Html), true, true)), 4);
        // The reading pane follows the option.
        let (app, _) = app_with_text(text);
        let mut h = harness(app);
        h.run();
        h.get_by_label_contains("wrapped at a fixed width");
        h.state_mut().run("options.set", json!({"key": "mail.format.removeExtraLineBreaks", "value": false}));
        h.run();
        assert!(h.query_by_label_contains("wrapped at a fixed width").is_none());
    }

    #[test]
    fn smime_info_bars_and_details() {
        use mailcraft_smime::testkit;
        let mut s = sample_session();
        let me = s.mailbox.default_account().unwrap().address.clone();
        let my_id = testkit::identity("Sample Me", &me);
        let pfx = mailcraft_smime::pkcs12::build(&my_id, "pw", "").unwrap();
        let data = mailcraft_mime::transfer::encode_base64_flat(&pfx);
        s.run("options.importDigitalId", &json!({"data": data, "password": "pw", "trustIssuer": true}))
            .unwrap();
        // Ann signs and encrypts a message to me.
        let ann = testkit::identity("Ann Lee", "ann@example.org");
        let msg = format!(
            "From: Ann Lee <ann@example.org>\r\nTo: {me}\r\nSubject: Sealed\r\nMIME-Version: 1.0\r\n\
             Content-Type: text/plain\r\n\r\nThe code is 4711.\r\n"
        );
        let to = [my_id.cert.clone()];
        let p = mailcraft_smime::mime::Protect {
            sign: Some(&ann),
            encrypt_to: &to,
            clear_signed: true,
            now: testkit::NOW,
        };
        let raw = mailcraft_smime::mime::protect(msg.as_bytes(), &p).unwrap();
        let parsed = mailcraft_mime::parse(&raw).unwrap();
        let mut item = mailcraft_model::MailItem::from_message(&parsed, &raw, s.now());
        item.folder = s.selection.folder;
        let id = s.add_message(item, &mailcraft_engine::Content::source(raw.clone())).unwrap();
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        let mut h = harness(App::new(s));
        h.run_steps(2);
        assert!(h.query_by_label("Encrypted: only the recipients of this message can read it.").is_some());
        let signed = "Signed by Ann Lee <ann@example.org>. The digital signature is valid.";
        assert!(h.query_by_label(signed).is_some());
        assert!(
            h.query_by_label("smime.p7m").is_none(),
            "the encrypted part is not offered as an attachment"
        );
        h.get_by_label("Details...").click();
        h.run_steps(2);
        let app = h.into_state();
        assert!(app.s.notices.iter().any(|n| matches!(n,
            mailcraft_engine::Notice::OpenDialog(d, p) if d == "messageSecurity" && p["status"] == "valid")));
        // Ann's certificate was remembered from the valid signature.
        assert!(mailcraft_engine::smime::cert_for(&app.s, "ann@example.org").is_some());
    }

    #[test]
    fn junk_info_bar_and_read_receipt_request() {
        let s = sample_session();
        let subject =
            s.mailbox.mail.items().find(|m| s.body_or_empty(m).html.is_some()).unwrap().subject.clone();
        let mut app = app_with(&subject);
        let id = app.s.selection.focus.unwrap();
        // A read-receipt request: the bar asks (Options › Mail › Tracking default), "never" hides it.
        let raw = b"From: Ann <ann@example.org>\r\nTo: me@example.com\r\nSubject: Hi\r\n\
                    Disposition-Notification-To: ann@example.org\r\n\r\nBody\r\n";
        app.s.set_source(id, Some(raw)).unwrap();
        let mut h = harness(app);
        h.run_steps(2);
        assert!(h.query_by_label(crate::compose::READ_RECEIPT_REQUEST).is_some());
        let mut app = h.into_state();
        app.run("options.set", json!({"key": "mail.tracking.readReceiptResponse", "value": "never"}));
        let mut h = harness(app);
        h.run_steps(2);
        assert!(h.query_by_label(crate::compose::READ_RECEIPT_REQUEST).is_none());
        // In Junk Email: the info bar, and the body is shown as plain text without working links.
        let mut app = h.into_state();
        let acct = app.s.mailbox.folder_account(app.s.mailbox.mail.get(id).unwrap().folder).unwrap();
        let junk = app.s.mailbox.role_folder(acct, mailcraft_model::FolderRole::JunkEmail).unwrap();
        app.s.mailbox.move_items(&[id], junk).unwrap();
        assert!(mailcraft_engine::junk::links_disabled(&app.s, id));
        let mut h = harness(app);
        h.run_steps(2);
        assert!(h.query_by_label(mailcraft_engine::junk::JUNK_INFOBAR).is_some());
    }

    fn app_with_text(text: &str) -> (App, u64) {
        let mut s = sample_session();
        let id = s.mailbox.mail.items().find(|m| s.body_or_empty(m).html.is_some()).map(|m| m.id).unwrap();
        s.edit_body(id, |b| {
            b.html = None;
            b.format = BodyFormat::PlainText;
            b.text = Some(text.to_owned());
        })
        .unwrap();
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        (App::new(s), id.0)
    }

    /// Regression: an explorer window's reading pane shared the main window's cache slot, so with two
    /// different messages shown each frame threw the other's parsed body and selection away.
    #[test]
    fn main_and_explorer_reading_panes_keep_their_own_caches() {
        let s = sample_session();
        let mut bodies = s.mailbox.mail.items().filter(|m| {
            let b = s.body_or_empty(m);
            b.html.is_some() || b.text.is_some()
        });
        let (a, b) = (bodies.next().unwrap().id, bodies.next().unwrap().id);
        let mut app = App::new(s);
        let ctx = egui::Context::default();
        let explorer = crate::explorer::viewport_id(1);
        let frame = Frame { width: 1400.0, height: 900.0, ..Frame::default() };
        let base = Id::new(("mailcraft.reading.cache", Surface::Main));
        let draw = |app: &mut App, vp: egui::ViewportId, item: ItemId| {
            app.s.selection.items = vec![item];
            app.s.selection.focus = Some(item);
            let mut input = egui::RawInput { viewport_id: vp, ..Default::default() };
            input.viewports.insert(vp, egui::ViewportInfo::default());
            let mut out = ctx.run_ui(input, |ui| reading_pane(ui, app, &frame));
            out.textures_delta.clear();
            ctx.data(|d| d.get_temp::<MsgCache>(salted(base, vp))).unwrap()
        };
        let main1 = draw(&mut app, egui::ViewportId::ROOT, a);
        let other1 = draw(&mut app, explorer, b);
        let main2 = draw(&mut app, egui::ViewportId::ROOT, a);
        let other2 = draw(&mut app, explorer, b);
        assert_eq!((main2.item, other2.item), (a.0, b.0));
        assert!(Arc::ptr_eq(&main1.doc, &main2.doc), "the main window's body was not parsed again");
        assert!(Arc::ptr_eq(&other1.doc, &other2.doc), "the explorer window's body was not parsed again");
        // Closing the explorer window drops its state.
        forget_viewport(&ctx, explorer);
        assert!(ctx.data(|d| d.get_temp::<MsgCache>(salted(base, explorer))).is_none());
        assert!(ctx.data(|d| d.get_temp::<MsgCache>(base)).is_some());
    }

    #[test]
    fn find_bar_highlights_steps_and_closes() {
        let (app, id) = app_with_text("apple pie and apple tart\nno more Apple");
        let mut h = harness(app);
        h.run();
        let ctx = h.ctx.clone();
        request(&ctx, h.state(), id, Request::Find { text: "apple".into(), match_case: false });
        h.run();
        h.run();
        h.get_by_label(FIND_LABEL);
        h.get_by_label("1 of 3");
        h.get_by_label(FIND_NEXT).click();
        h.run();
        h.get_by_label("2 of 3");
        h.get_by_label(FIND_PREVIOUS).click();
        h.run();
        h.get_by_label(FIND_PREVIOUS).click();
        h.run();
        h.get_by_label("3 of 3");
        h.get_by_label("Match case").click();
        h.run();
        h.run();
        h.get_by_label("1 of 2");
        h.get_by_label(FIND_CLOSE).click();
        h.run();
        assert!(h.query_by_label(FIND_LABEL).is_none());
        let bar = find_bar_state(&ctx, Surface::Main);
        assert!(!bar.open && bar.hits.is_empty());
        assert_eq!(match_label(0, 0), NO_MATCHES);
        assert_eq!(match_label(7, 3), "3 of 3");
    }

    #[test]
    fn select_requests_select_the_body_for_copying() {
        let (app, id) = app_with_text("first line\nsecond line");
        let mut h = harness(app);
        h.run();
        let ctx = h.ctx.clone();
        request(&ctx, h.state(), id, Request::SelectAll);
        h.run();
        let (main_key, req_key) = (cache_id(&ctx, Surface::Main), request_id(&ctx, Surface::Main));
        let cache = ctx.data(|d| d.get_temp::<MsgCache>(main_key)).unwrap();
        assert_eq!(body::selected_text(&cache.view, &cache.doc).as_deref(), Some("first line\nsecond line"));
        request(&ctx, h.state(), id, Request::SelectSimilar);
        h.run();
        let cache = ctx.data(|d| d.get_temp::<MsgCache>(main_key)).unwrap();
        assert!(cache.view.ordered_selection().is_some());
        // A request for another message is left alone.
        request(&ctx, h.state(), id + 999, Request::SelectAll);
        h.run();
        assert!(ctx.data(|d| d.get_temp::<(u64, Request)>(req_key)).is_some());
    }

    #[test]
    fn read_window_find_runs_end_to_end() {
        let (mut app, id) = app_with_text("pear and pear");
        app.ui.read_windows.push(id);
        let mut h = Harness::builder().with_size(Vec2::new(1200.0, 900.0)).build_ui_state(
            |ui, app: &mut App| {
                ui.ctx().set_embed_viewports(true);
                app.ui_root(ui);
            },
            app,
        );
        h.run();
        // Message › Find… in the read window passes the window's item (the ribbon adds `ids` to commands
        // that take them).
        for cmd in ["format.find", "format.selectAll", "format.selectSimilar"] {
            assert!(h.state().s.registry.get(cmd).unwrap().params.contains("\"ids\""), "{cmd}");
        }
        h.state_mut().run("format.find", json!({"ids": [id], "text": "pear"}));
        for _ in 0..4 {
            h.run();
        }
        h.get_by_label("1 of 2");
        h.state_mut().run("format.selectAll", json!({"ids": [id]}));
        for _ in 0..3 {
            h.run();
        }
        let key = cache_id(&h.ctx, Surface::Read(id));
        let cache = h.ctx.data(|d| d.get_temp::<MsgCache>(key)).unwrap();
        assert_eq!(body::selected_text(&cache.view, &cache.doc).as_deref(), Some("pear and pear"));
    }

    #[test]
    fn helpers() {
        assert_eq!(short_date("Thu 10/8/2026 7:30 AM"), "Thu 7:30 AM");
        assert_eq!(short_date(""), "");
        assert_eq!(window_title("Hi", BodyFormat::Html), "Hi - Message (HTML)");
        assert_eq!(window_title(" ", BodyFormat::PlainText), "(no subject) - Message (Plain Text)");
        assert_ne!(persona_color(0), persona_color(1));
        assert_eq!(persona_color(16), persona_color(0));
    }

    #[test]
    fn empty_state_renders() {
        let mut s = sample_session();
        s.selection.items.clear();
        s.selection.focus = None;
        let mut h = harness(App::new(s));
        h.run();
        h.get_by_label("Select an item to read");
    }

    #[test]
    fn empty_state_links_to_reading_pane_options() {
        let mut s = sample_session();
        s.selection.items.clear();
        s.selection.focus = None;
        let mut h = harness(App::new(s));
        h.run();
        h.get_by_label("Reading pane options").click();
        h.run();
        let opened =
            h.state().s.notices.iter().any(
                |n| matches!(n, mailcraft_engine::Notice::OpenDialog(id, _) if id == "readingPaneOptions"),
            );
        assert!(opened, "{:?}", h.state().s.notices);
    }

    #[test]
    fn renders_html_message() {
        let s = sample_session();
        let m = s.mailbox.mail.items().find(|m| s.body_or_empty(m).html.is_some()).unwrap();
        let subject = m.subject.clone();
        let mut h = harness(app_with(&subject));
        h.run();
        h.get_by_label(&subject);
        h.get_by_label("Reply");
        h.get_by_label("Reply All");
        h.get_by_label("Forward");
        // The body widget exposes its text; the sample newsletter has an unsubscribe line.
        assert!(h.query_all_by_label_contains("Unsubscribe").next().is_some());
    }

    #[test]
    fn renders_plain_message_and_attachment_and_reply() {
        let mut s = sample_session();
        let id = s.mailbox.mail.items().find(|m| m.subject == "Quarterly report draft").unwrap().id;
        s.edit_body(id, |b| {
            b.html = None;
            b.format = BodyFormat::PlainText;
            b.text = Some("Hi team,\nplease see the attached draft.\n".into());
        })
        .unwrap();
        let mut h = harness(app_with_session(s, "Quarterly report draft"));
        h.run();
        assert!(h.query_all_by_label_contains("please see the attached draft.").next().is_some());
        assert!(h.query_all_by_label_contains("Quarterly report draft.pdf").next().is_some());
        assert!(h.query_all_by_label_contains("Follow up.").next().is_some());
        h.get_by_label("Reply").click();
        h.run();
        let app = h.state();
        assert!(app.s.composes.values().any(|c| c.inline));
    }

    /// A message whose body the offline window took off: the pane says so instead of showing an empty body,
    /// and offers the download when MailCraft is online.
    #[test]
    fn a_body_taken_off_by_the_offline_window_shows_why() {
        let mut s = sample_session();
        let acct = s.mailbox.add_account(mailcraft_model::Account {
            kind: mailcraft_model::AccountKind::Imap,
            display_name: "Work".into(),
            address: "me@example.com".into(),
            incoming: Some(mailcraft_model::ServerSettings {
                host: "127.0.0.1".into(),
                port: 9,
                ..Default::default()
            }),
            ..Default::default()
        });
        s.mailbox.create_default_folders(acct);
        let inbox = s.mailbox.role_folder(acct, mailcraft_model::FolderRole::Inbox).unwrap();
        s.mailbox.folders.get_mut(&inbox).unwrap().remote_name = Some("INBOX".into());
        let item = mailcraft_model::MailItem {
            folder: inbox,
            subject: "Old report".into(),
            remote_id: Some("5".into()),
            received: mailcraft_model::DateTime::from_unix(s.now().unix - 800 * 86_400, 0),
            ..Default::default()
        };
        let raw = b"From: ann@example.org\r\nSubject: Old report\r\n\r\nThe old figures\r\n".to_vec();
        let id = s.add_message(item, &mailcraft_engine::Content::source(raw)).unwrap();
        s.view.work_offline = true;
        assert_eq!(mailcraft_engine::offline::pass(&mut s, acct).evicted, 1);
        s.selection.folder = inbox;
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        let mut h = harness(App::new(s));
        h.run_steps(2);
        assert!(h.query_by_label_contains("only its header is here").is_some());
        assert!(h.query_by_label_contains("working offline").is_some());
        assert!(h.query_by_label("Download it now").is_none(), "nothing to offer while offline");
        assert!(h.query_by_label("Download the rest of the message").is_none(), "one bar, not two");
        // Online: opening it asks for the download, and the bar offers it again.
        h.state_mut().s.view.work_offline = false;
        h.run_steps(2);
        assert!(h.state().s.mailbox.mail.get(id).unwrap().body_evicted);
        assert!(h.query_by_label("Download it now").is_some());
    }

    fn app_with_session(mut s: mailcraft_engine::Session, subject: &str) -> App {
        let id = s.mailbox.mail.items().find(|m| m.subject == subject).map(|m| m.id).unwrap();
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        App::new(s)
    }

    #[test]
    fn tiny_read_windows_drop_the_ribbon_like_compose() {
        let s = sample_session();
        let id = s.mailbox.mail.ids().next().unwrap();
        let ribbon_shown = |w: f32, h: f32| {
            let mut app = App::new(sample_session());
            app.ui.read_windows.push(id.0);
            let mut hn = Harness::builder().with_size(Vec2::new(w, h)).build_ui_state(
                move |ui, app: &mut App| {
                    read_window_ui(ui, app, id.0, egui::ViewportClass::Immediate);
                },
                app,
            );
            hn.run();
            hn.query_all_by_label("Move & Delete").next().is_some() || hn.query_by_label("Delete").is_some()
        };
        assert!(ribbon_shown(900.0, 700.0), "a normal window has its ribbon");
        // responsive::inspector_tiny: narrower than 360 or lower than 260 (the observed 300×200 included).
        assert!(!ribbon_shown(340.0, 700.0));
        assert!(!ribbon_shown(900.0, 240.0));
        assert!(!ribbon_shown(300.0, 200.0));
    }

    #[test]
    fn read_window_closes_on_escape_when_embedded() {
        let s = sample_session();
        let id = s.mailbox.mail.ids().next().unwrap();
        let mut app = App::new(s);
        app.ui.read_windows.push(id.0);
        let mut h = Harness::builder().with_size(Vec2::new(1000.0, 800.0)).build_ui_state(
            |ui, app: &mut App| {
                ui.ctx().set_embed_viewports(true);
                let ctx = ui.ctx().clone();
                read_windows(&ctx, app);
            },
            app,
        );
        h.run();
        assert_eq!(h.state().ui.read_windows.len(), 1);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().ui.read_windows.is_empty());
    }

    /// Release check: the read window drew no tab row (no File, Message, Help) and no Quick Access Toolbar, so
    /// Help and its backstage could not be reached; at 1024×768 it also left slivers of the main window showing.
    #[test]
    fn read_window_has_quick_access_tabs_and_its_own_backstage() {
        let s = sample_session();
        let id = s.mailbox.mail.items().find(|m| m.subject == "Slides for Thursday").map(|m| m.id.0).unwrap();
        let mut app = App::new(s);
        app.ui.read_windows.push(id);
        let mut h = Harness::builder().with_size(Vec2::new(1024.0, 768.0)).build_ui_state(
            |ui, app: &mut App| {
                ui.ctx().set_embed_viewports(true);
                let ctx = ui.ctx().clone();
                read_windows(&ctx, app);
            },
            app,
        );
        h.run_steps(2);
        for label in
            ["Save", "Undo", "Redo", "Previous Item", "Next Item", "Close", "File", "Message", "Help"]
        {
            assert!(h.query_by_label(label).is_some(), "{label} missing");
        }
        // The window fills the 1024×768 screen: its title strip at the top, Close at the right edge.
        let title = h.get_by_label_contains("Slides for Thursday - Message").rect();
        assert!(title.top() < 8.0, "{title:?}");
        assert!(h.get_by_label("Close").rect().right() > 1024.0 - 24.0);
        assert!(h.get_by_label("Save").rect().left() < 40.0);
        // QAT, tab row and ribbon do not overlap.
        let qat = h.get_by_label("Save").rect();
        let tab = h.get_by_label("Message").rect();
        assert!(qat.bottom() <= tab.top() + 0.5, "{qat:?} {tab:?}");
        // Help is a tab of the window.
        h.get_by_label("Help").click();
        h.run_steps(2);
        assert_eq!(h.state().ui.tab(Surface::Read(id)), Some("Help"));
        // File opens the window's own backstage, drawn in the window; Escape goes back to the message.
        h.get_by_label("File").click();
        h.run_steps(2);
        assert!(matches!(&h.state().ui.backstage, Some((Surface::Read(r), _)) if *r == id));
        assert!(h.query_by_label("Save Attachments").is_some(), "the backstage shows in the window");
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.state().ui.backstage.is_none());
        assert_eq!(h.state().ui.read_windows, vec![id], "Escape left the backstage, not the window");
        // Next Item shows the following message in the same window.
        h.get_by_label("Next Item").click();
        h.run_steps(2);
        assert_ne!(h.state().ui.read_windows, vec![id]);
        assert_eq!(h.state().ui.read_windows.len(), 1);
    }

    /// A read window that fits with room to spare stays a floating window over the main one.
    #[test]
    fn read_window_floats_on_a_large_screen() {
        assert!(crate::widgets::embedded_fills(READ_WINDOW_SIZE.into(), Vec2::new(1024.0, 768.0)));
        assert!(crate::widgets::embedded_fills(
            crate::compose::DEFAULT_SIZE.into(),
            Vec2::new(1440.0, 900.0)
        ));
        assert!(!crate::widgets::embedded_fills(READ_WINDOW_SIZE.into(), Vec2::new(1920.0, 1080.0)));
        let s = sample_session();
        let id = s.mailbox.mail.ids().next().unwrap();
        let mut app = App::new(s);
        app.ui.read_windows.push(id.0);
        let mut h = Harness::builder().with_size(Vec2::new(1920.0, 1080.0)).build_ui_state(
            |ui, app: &mut App| {
                ui.ctx().set_embed_viewports(true);
                let ctx = ui.ctx().clone();
                read_windows(&ctx, app);
            },
            app,
        );
        h.run_steps(2);
        let close = h.get_by_label("Close").rect();
        assert!(close.right() < 1920.0 - 100.0, "floating, not filling: {close:?}");
    }

    fn click_at(h: &mut Harness<'_, App>, pos: egui::Pos2) {
        h.hover_at(pos);
        h.drag_at(pos);
        h.drop_at(pos);
        h.run_steps(2);
    }

    fn layer_shown(h: &Harness<'_, App>, layer: egui::LayerId) -> bool {
        h.ctx.memory(|m| m.areas().visible_last_frame(&layer))
    }

    fn top_window(h: &Harness<'_, App>) -> Option<egui::LayerId> {
        h.ctx.memory(|m| m.areas().top_layer_id(egui::Order::Middle))
    }

    /// Release check: at 1024×768 an embedded read window covered the screen and Reply's compose covered it
    /// in turn, with nothing to bring the message being answered forward again. With two item windows open
    /// both now float, staggered: the new one in front, the other one forward with a click on the part that
    /// shows, and either can be dragged aside. Alone again, the read window covers the screen and works.
    #[test]
    fn a_reply_over_a_full_screen_read_window_leaves_both_reachable() {
        use crate::widgets::{item_cover_layer, item_float_layer};
        let s = sample_session();
        let id = s.mailbox.items_in(s.selection.folder).first().map(|m| m.id.0).unwrap();
        let mut app = App::new(s);
        app.ui.read_windows.push(id);
        let mut h = Harness::builder().with_size(Vec2::new(1024.0, 768.0)).build_ui_state(
            |ui, app: &mut App| {
                ui.ctx().set_embed_viewports(true);
                let ctx = ui.ctx().clone();
                read_windows(&ctx, app);
                crate::compose::compose_windows(&ctx, app);
            },
            app,
        );
        h.run_steps(2);
        let read = read_viewport_id(id);
        assert!(layer_shown(&h, item_cover_layer(read)), "alone, the read window covers the screen");
        h.state_mut().run("mail.reply", json!({"ids": [id], "window": true})).unwrap();
        h.run_steps(3);
        let cid = h.state().s.composes.values().find(|c| !c.inline).map(|c| c.id).unwrap();
        let compose = crate::compose::viewport_id(cid);
        assert!(!layer_shown(&h, item_cover_layer(read)), "two windows: no cover");
        assert!(layer_shown(&h, item_float_layer(read)) && layer_shown(&h, item_float_layer(compose)));
        assert_eq!(top_window(&h), Some(item_float_layer(compose)), "the reply comes to the front");
        let rect = |h: &Harness<'_, App>, v: egui::ViewportId| {
            h.ctx.memory(|m| m.area_rect(item_float_layer(v).id)).unwrap()
        };
        let (r, c) = (rect(&h, read), rect(&h, compose));
        assert!(r.right() > c.right() + 8.0 && r.bottom() > c.bottom() + 8.0, "staggered: {r:?} {c:?}");
        assert!(r.max.x <= 1024.0 && r.max.y <= 768.0, "on the screen: {r:?}");
        // A click on the part of the read window that shows brings it forward.
        click_at(&mut h, egui::pos2(r.right() - 6.0, r.center().y));
        assert_eq!(top_window(&h), Some(item_float_layer(read)));
        h.state_mut().run("compose.close", json!({"compose": cid, "save": false}));
        h.run_steps(3);
        assert!(h.state().s.composes.is_empty());
        assert!(layer_shown(&h, item_cover_layer(read)), "alone again: the read window covers the screen");
        assert_eq!(h.ctx.layer_id_at(egui::pos2(512.0, 400.0)), Some(item_cover_layer(read)));
        h.get_by_label("Help").click();
        h.run_steps(2);
        assert_eq!(h.state().ui.tab(Surface::Read(id)), Some("Help"));
    }

    /// A read window opened over a compose that covers the screen comes to the front (the compose, drawn after
    /// it, would otherwise land on top as it starts floating).
    #[test]
    fn a_window_opened_over_a_full_screen_compose_comes_to_the_front() {
        use crate::widgets::item_float_layer;
        let s = sample_session();
        let id = s.mailbox.items_in(s.selection.folder).first().map(|m| m.id.0).unwrap();
        let mut app = App::new(s);
        app.run("mail.reply", json!({"ids": [id], "window": true})).unwrap();
        let cid = app.s.composes.values().find(|c| !c.inline).map(|c| c.id).unwrap();
        let mut h = Harness::builder().with_size(Vec2::new(1024.0, 768.0)).build_ui_state(
            |ui, app: &mut App| {
                ui.ctx().set_embed_viewports(true);
                let ctx = ui.ctx().clone();
                read_windows(&ctx, app);
                crate::compose::compose_windows(&ctx, app);
            },
            app,
        );
        h.run_steps(2);
        let compose = crate::compose::viewport_id(cid);
        assert!(layer_shown(&h, crate::widgets::item_cover_layer(compose)));
        h.state_mut().ui.read_windows.push(id);
        h.run_steps(2);
        assert!(layer_shown(&h, item_float_layer(compose)));
        assert_eq!(top_window(&h), Some(item_float_layer(read_viewport_id(id))));
        // It stays there: the next frames leave the order alone.
        h.run_steps(3);
        assert_eq!(top_window(&h), Some(item_float_layer(read_viewport_id(id))));
    }

    /// A floating item window moves when its title bar is dragged, past the screen's edge (part of it stays).
    #[test]
    fn a_floating_item_window_can_be_dragged_aside() {
        use crate::widgets::item_float_layer;
        let s = sample_session();
        let ids: Vec<u64> = s.mailbox.items_in(s.selection.folder).iter().take(2).map(|m| m.id.0).collect();
        let mut app = App::new(s);
        app.ui.read_windows.extend(ids.iter().copied());
        let mut h = Harness::builder().with_size(Vec2::new(1024.0, 768.0)).build_ui_state(
            |ui, app: &mut App| {
                ui.ctx().set_embed_viewports(true);
                let ctx = ui.ctx().clone();
                read_windows(&ctx, app);
            },
            app,
        );
        h.run_steps(3);
        let second = read_viewport_id(ids[1]);
        let before = h.ctx.memory(|m| m.area_rect(item_float_layer(second).id)).unwrap();
        let grip = egui::pos2(before.center().x, before.top() + 10.0);
        h.hover_at(grip);
        h.drag_at(grip);
        h.run_steps(2);
        for step in 1..=10 {
            h.hover_at(grip + egui::vec2(70.0 * step as f32, 0.0));
            h.run();
        }
        h.drop_at(grip + egui::vec2(700.0, 0.0));
        h.run_steps(2);
        let after = h.ctx.memory(|m| m.area_rect(item_float_layer(second).id)).unwrap();
        assert!(after.left() > before.left() + 300.0, "moved aside: {before:?} -> {after:?}");
        assert!(after.right() > 1024.0, "past the screen's edge: {after:?}");
        assert!(after.left() < 1024.0 - 100.0, "part of it stays on the screen: {after:?}");
    }

    fn embedded_read_windows(app: App) -> Harness<'static, App> {
        Harness::builder().with_size(Vec2::new(1000.0, 800.0)).build_ui_state(
            |ui, app: &mut App| {
                ui.ctx().set_embed_viewports(true);
                let ctx = ui.ctx().clone();
                read_windows(&ctx, app);
            },
            app,
        )
    }

    #[test]
    fn deleting_or_moving_the_open_item_closes_its_window() {
        for cmd in ["mail.delete", "mail.move"] {
            let s = sample_session();
            let inbox = s.selection.folder;
            let id = s.mailbox.items_in(inbox).first().map(|m| m.id).unwrap();
            let target = s
                .mailbox
                .folders
                .values()
                .find(|f| f.kind == mailcraft_model::FolderKind::Mail && f.id != inbox && f.role.is_none())
                .map(|f| f.id)
                .unwrap_or(inbox);
            let mut app = App::new(s);
            app.ui.read_windows.push(id.0);
            let mut h = embedded_read_windows(app);
            h.run();
            assert_eq!(h.state().ui.read_windows, [id.0]);
            let params = match cmd {
                "mail.move" => json!({"ids": [id.0], "folder": target.0}),
                _ => json!({"ids": [id.0]}),
            };
            h.state_mut().s.selection.items = vec![id];
            h.state_mut().s.selection.focus = Some(id);
            h.state_mut().s.run(cmd, &params).unwrap();
            assert!(h.state().s.mailbox.mail.contains(id), "{cmd}: the item still exists elsewhere");
            h.run();
            // Default "After moving or deleting an open item": return to the current folder.
            assert!(h.state().ui.read_windows.is_empty(), "{cmd} left the window open");
        }
    }

    /// Clicks Save in the save-changes question (the window's Quick Access row has a Save button too).
    fn prompt_save(h: &Harness<'_, App>) {
        // The question is centred; the Quick Access rows sit at the top.
        match h.query_all_by_label("Save").max_by(|a, b| a.rect().top().total_cmp(&b.rect().top())) {
            Some(n) => n.click(),
            None => panic!("no Save in the save question"),
        }
    }

    /// A read window editing the first Inbox message, with a subject edit not saved yet.
    fn editing_with_unsaved_edits(s: mailcraft_engine::Session) -> (Harness<'static, App>, ItemId) {
        let inbox = s.selection.folder;
        let id = s.mailbox.items_in(inbox).first().map(|m| m.id).unwrap();
        let mut app = App::new(s);
        crate::edit_message::start(&mut app, id.0);
        let mut h = embedded_read_windows(app);
        h.run_steps(2);
        crate::edit_message::append_to_subject(&h.ctx, id.0, " (edited)");
        h.run_steps(2);
        assert!(crate::edit_message::has_unsaved_edits(&h.ctx, id.0));
        (h, id)
    }

    fn other_mail_folder(s: &mailcraft_engine::Session) -> mailcraft_model::FolderId {
        let inbox = s.selection.folder;
        s.mailbox
            .folders
            .values()
            .find(|f| f.kind == mailcraft_model::FolderKind::Mail && f.id != inbox && f.role.is_none())
            .map(|f| f.id)
            .unwrap()
    }

    #[test]
    fn moving_a_message_with_unsaved_edits_keeps_its_window_and_asks() {
        // Regression: Move, Delete or Archive while editing closed the window (or swapped it to the next
        // message) and the edits went without a question.
        let mut s = sample_session();
        s.settings.set("mail.other.afterMoveOrDelete", &json!("openNext")).unwrap();
        let target = other_mail_folder(&s);
        let (mut h, id) = editing_with_unsaved_edits(s);
        h.state_mut().s.run("mail.move", &json!({"ids": [id.0], "folder": target.0})).unwrap();
        h.run_steps(2);
        assert_eq!(h.state().ui.read_windows, [id.0], "the window stays on the edited message");
        assert!(crate::edit_message::is_editing(h.state(), id.0));
        h.get_by_label(crate::edit_message::SAVE_PROMPT);
        prompt_save(&h);
        h.run_steps(3);
        let m = &h.state().s.mailbox.mail.get(id).unwrap();
        assert_eq!(m.folder, target);
        assert!(m.subject.ends_with(" (edited)"), "{}", m.subject);
        assert!(h.state().ui.read_windows.is_empty());
    }

    #[test]
    fn a_window_that_moves_on_drops_an_editor_with_nothing_unsaved() {
        // Regression: the window swapped to the next message and left the editor of the old one behind.
        let mut s = sample_session();
        s.settings.set("mail.other.afterMoveOrDelete", &json!("openNext")).unwrap();
        let first = queries::visible_item_ids(&s).first().copied().unwrap();
        let mut app = App::new(s);
        crate::edit_message::start(&mut app, first.0);
        let mut h = embedded_read_windows(app);
        h.run_steps(2);
        h.state_mut().s.run("mail.delete", &json!({"ids": [first.0]})).unwrap();
        h.run_steps(2);
        assert_ne!(h.state().ui.read_windows, [first.0]);
        assert!(h.state().ui.editing_messages.is_empty());
    }

    #[test]
    fn the_ribbon_waits_while_edits_are_unsaved() {
        use egui_kittest::kittest::NodeT;
        let (h, _) = editing_with_unsaved_edits(sample_session());
        let reply = h.get_all_by_label("Reply").next().expect("the Reply button");
        assert!(reply.accesskit_node().is_disabled(), "ribbon commands wait for Save or Discard");
    }

    #[test]
    fn exit_with_unsaved_edits_asks_first_and_stays_open() {
        // Regression: File › Exit and the main window's close button closed MailCraft and dropped the edits.
        let (mut h, id) = editing_with_unsaved_edits(sample_session());
        let ctx = h.ctx.clone();
        assert!(!crate::edit_message::may_exit(&ctx, h.state_mut()));
        h.run_steps(2);
        h.get_by_label(crate::edit_message::SAVE_PROMPT);
        let toast = h.state().ui.toast.as_ref().map(|t| t.0.clone());
        assert_eq!(toast.as_deref(), Some(crate::edit_message::EXIT_WAITS_NOTE));
        assert!(crate::edit_message::is_editing(h.state(), id.0));
        // Don't Save: nothing is left unsaved, so MailCraft may close.
        h.get_by_label("Don't Save").click();
        h.run_steps(2);
        assert!(crate::edit_message::may_exit(&ctx, h.state_mut()));
    }

    /// Release check: Exit brought a window waiting on the save question forward with a viewport Focus, which
    /// does nothing to a window embedded in the main one, so it could stay behind another item window.
    #[test]
    fn exit_brings_the_embedded_window_with_unsaved_edits_forward() {
        let (mut h, id) = editing_with_unsaved_edits(sample_session());
        // Another read window opens over it.
        let other = h.state().s.mailbox.mail.ids().find(|k| *k != id).unwrap();
        h.state_mut().ui.read_windows.push(other.0);
        h.run_steps(3);
        let layer = |v: u64| crate::widgets::item_float_layer(read_viewport_id(v));
        assert_eq!(top_window(&h), Some(layer(other.0)));
        let ctx = h.ctx.clone();
        assert!(!crate::edit_message::may_exit(&ctx, h.state_mut()));
        h.run_steps(2);
        assert_eq!(top_window(&h), Some(layer(id.0)), "the window with the question comes forward");
    }

    /// Release check: the Quick Access Save was disabled exactly while Edit Message had unsaved changes and
    /// otherwise opened the save-to-file dialog, so it could not save message edits. It now saves them.
    #[test]
    fn quick_access_save_saves_message_edits() {
        use egui_kittest::kittest::NodeT;
        let (mut h, id) = editing_with_unsaved_edits(sample_session());
        let before = h.state().s.mailbox.mail.get(id).unwrap().subject.clone();
        // The Quick Access row is the top one (the editor's own bar has a Save button too).
        let qat_save = h.get_all_by_label("Save").min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
        let qat_save = qat_save.expect("the Quick Access Save");
        assert!(!qat_save.accesskit_node().is_disabled(), "Save is enabled while edits wait");
        qat_save.click();
        h.run_steps(3);
        let m = &h.state().s.mailbox.mail.get(id).unwrap();
        assert_eq!(m.subject, format!("{before} (edited)"));
        assert!(!crate::edit_message::is_editing(h.state(), id.0), "saved: the editor stops");
        assert_eq!(h.state().ui.read_windows, [id.0], "the window stays open on the saved message");
        assert!(h.state().ui.dialogs.is_empty(), "no save-to-file dialog: {:?}", h.state().ui.dialogs);
        // Nothing being edited: nothing to save.
        let save = h.get_all_by_label("Save").min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
        assert!(save.expect("the Quick Access Save").accesskit_node().is_disabled());
    }

    #[test]
    fn closing_the_main_window_with_unsaved_edits_is_cancelled() {
        let s = sample_session();
        let id = s.mailbox.items_in(s.selection.folder).first().map(|m| m.id).unwrap();
        let mut app = App::new(s);
        crate::edit_message::start(&mut app, id.0);
        let mut h = Harness::builder()
            .with_size(Vec2::new(1400.0, 900.0))
            .build_ui_state(|ui, app: &mut App| app.ui_root(ui), app);
        h.run_steps(3);
        crate::edit_message::append_to_subject(&h.ctx, id.0, "!");
        h.run_steps(2);
        let close = |h: &mut Harness<'static, App>| {
            h.input_mut()
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .events
                .push(egui::ViewportEvent::Close);
            h.step();
            h.output()
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .is_some_and(|v| v.commands.contains(&egui::ViewportCommand::CancelClose))
        };
        assert!(close(&mut h), "the close is cancelled while edits are unsaved");
        h.run_steps(2);
        h.get_by_label(crate::edit_message::SAVE_PROMPT);
        prompt_save(&h);
        h.run_steps(3);
        assert!(h.state().s.mailbox.mail.get(id).unwrap().subject.ends_with('!'));
        assert!(!close(&mut h), "nothing unsaved: MailCraft closes");
    }

    #[test]
    fn moving_the_open_item_can_open_the_next_one() {
        let mut s = sample_session();
        s.settings.set("mail.other.afterMoveOrDelete", &json!("openNext")).unwrap();
        let inbox = s.selection.folder;
        let items: Vec<ItemId> = s.mailbox.items_in(inbox).iter().map(|m| m.id).collect();
        let order = queries::visible_item_ids(&s);
        let first = order.first().copied().unwrap();
        let second = order.get(1).copied().unwrap();
        assert!(items.contains(&first));
        let mut app = App::new(s);
        app.ui.read_windows.push(first.0);
        let mut h = embedded_read_windows(app);
        h.run();
        h.state_mut().s.selection.items = vec![first];
        h.state_mut().s.selection.focus = Some(first);
        h.state_mut().s.run("mail.delete", &json!({"ids": [first.0]})).unwrap();
        h.run();
        assert_eq!(h.state().ui.read_windows, [second.0]);
    }

    #[test]
    fn f6_cycles_header_body_info_bar_and_ribbon_in_a_read_window() {
        let mut s = sample_session();
        // A message with an info bar (a follow-up flag).
        let id = s.mailbox.mail.items().find(|m| s.body_or_empty(m).html.is_some()).map(|m| m.id).unwrap();
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        s.run("mail.flag", &json!({"ids": [id.0]})).unwrap();
        let mut app = App::new(s);
        app.ui.read_windows.push(id.0);
        let mut h = Harness::builder().with_size(Vec2::new(1100.0, 900.0)).build_ui_state(
            |ui, app: &mut App| {
                ui.ctx().set_embed_viewports(true);
                let ctx = ui.ctx().clone();
                crate::keys::handle(&ctx, app, Surface::Main);
                read_windows(&ctx, app);
            },
            app,
        );
        h.run_steps(3);
        let surface = Surface::Read(id.0);
        let ctx = h.ctx.clone();
        // From nowhere: the first stop, the header.
        crate::keys::apply(&ctx, h.state_mut(), surface, crate::keys::Action::CyclePane(false));
        h.run_steps(2);
        assert_eq!(crate::keys::window_pane(&h.ctx, surface).as_deref(), Some("header"));
        let mut seen = vec!["header".to_owned()];
        for _ in 0..3 {
            h.key_press(egui::Key::F6);
            h.run_steps(2);
            seen.push(crate::keys::window_pane(&h.ctx, surface).unwrap_or_default());
        }
        assert_eq!(seen, ["header", "body", "infoBar", "ribbon"]);
        assert!(crate::keys::widget_focused(&h.ctx), "a ribbon tab has focus");
        // Round the corner and back.
        h.key_press(egui::Key::F6);
        h.run_steps(2);
        assert_eq!(crate::keys::window_pane(&h.ctx, surface).as_deref(), Some("header"));
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::F6);
        h.run_steps(2);
        assert_eq!(crate::keys::window_pane(&h.ctx, surface).as_deref(), Some("ribbon"));
        // The main window's own F6 state is untouched.
        assert_eq!(crate::keys::focused_pane(&h.ctx), None);
    }

    /// Options › Language › Ask me before translating: a message in another language offers a translation,
    /// but only once a translation server is set up (nothing is offered or sent by default).
    #[test]
    fn a_message_in_another_language_offers_a_translation_once_a_server_is_set_up() {
        let french = "Bonjour, merci pour votre message. Nous avons reçu les documents et nous vous \
                      répondrons dans la semaine avec une proposition pour le projet.";
        let (app, _) = app_with_text(french);
        let mut h = harness(app);
        h.run_steps(2);
        assert!(h.query_by_label_contains("seems to be in French").is_none(), "no server: no offer");
        let mut app = h.into_state();
        // A local address; nothing is sent until the link is clicked.
        app.run("options.translator", json!({"url": "http://127.0.0.1:9"})).unwrap();
        let mut h = harness(app);
        h.run_steps(2);
        assert!(h.query_by_label("This message seems to be in French.").is_some());
        assert!(h.query_by_label("Translate it into English").is_some());
        let mut app = h.into_state();
        app.run("options.set", json!({"key": "language.translate", "value": "never"})).unwrap();
        let mut h = harness(app);
        h.run_steps(2);
        assert!(h.query_by_label_contains("seems to be in French").is_none());
    }

    fn sized(app: App, w: f32) -> Harness<'static, App> {
        Harness::builder().with_size(Vec2::new(w, 600.0)).build_ui_state(
            |ui, app: &mut App| {
                let frame = Frame { width: 1400.0, height: 900.0, ..Frame::default() };
                reading_pane(ui, app, &frame);
            },
            app,
        )
    }

    fn painted_rects(h: &Harness<'_, App>, color: Color32) -> Vec<egui::Rect> {
        fn walk(shape: &egui::Shape, color: Color32, out: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, color, out)),
                egui::Shape::Rect(r) if r.fill == color => out.push(r.rect),
                _ => {}
            }
        }
        let mut out = Vec::new();
        h.output().shapes.iter().for_each(|c| walk(&c.shape, color, &mut out));
        out
    }

    /// Release check: in the dark theme the body was painted on its own grey box behind the text only.
    /// A message without a background takes the pane's colour; a light page (the sun toggle) fills the pane
    /// from edge to edge.
    #[test]
    fn dark_message_body_has_no_box_of_its_own() {
        let mut app = app_with("Slides for Thursday");
        app.theme = crate::theme::Theme::dark();
        let mut h = sized(app, 900.0);
        h.run_steps(2);
        let mapped_white = body::map_color(mailcraft_geom::Color::WHITE, true);
        assert!(painted_rects(&h, mapped_white).is_empty(), "no grey page box behind the text");
        h.get_by_label("Switch background to light").click();
        h.run_steps(2);
        let page = painted_rects(&h, Color32::WHITE);
        assert!(
            page.iter().any(|r| r.width() >= 880.0 && r.bottom() >= 585.0),
            "the light page spans the pane to its bottom: {page:?}"
        );
    }

    /// Release check: in a narrow dark reading pane the background toggle butted against the truncated
    /// sender line.
    #[test]
    fn sender_line_keeps_a_gap_before_the_header_buttons() {
        for (w, dark) in [(420.0, true), (520.0, true), (420.0, false), (900.0, true)] {
            let mut app = app_with("Slides for Thursday");
            if dark {
                app.theme = crate::theme::Theme::dark();
            }
            let mut h = sized(app, w);
            h.run_steps(2);
            let from = h.get_by_label_contains("Grace Kim").rect();
            let mut buttons = vec![h.get_by_label("Reply").rect()];
            if dark {
                buttons.push(h.get_by_label("Switch background to light").rect());
            }
            for b in buttons {
                if b.top() < from.bottom() && b.bottom() > from.top() {
                    assert!(
                        from.right() + 8.0 <= b.left(),
                        "{w} dark={dark}: sender {from:?} runs into {b:?}"
                    );
                }
            }
        }
    }
}
