//! Message-body rendering: lays out a rich-text Doc with mailcraft-layout and paints the display list with
//! egui (reading area owns; compose area uses). Remote images blocked unless allowed.
//!
//! - Layout is cached in [`BodyView`] by (content hash, width, zoom); text runs are painted with the exact
//!   bundled face the layout measured with (each face is registered with egui on first use as the named family
//!   `mailcraft-body:<family>:<weight>:<n|i>`, plus egui's own fallback fonts for emoji and symbols).
//! - Only items inside the clip rect are painted, so long bodies in a scroll area stay cheap.
//! - Links: pointing-hand cursor and the target as a tooltip; a click returns the target.
//! - Selection: drag to select (by [`mailcraft_layout::Layout::hit_pos`]), double-click selects a word, Ctrl+A
//!   selects all while the body has focus, Ctrl+C copies the selected text. Extra ranges
//!   ([`BodyView::extra`], Select Text With Similar Formatting) are painted and copied with the selection;
//!   find hits ([`BodyView::marks`], from [`find_all`]) are painted on a highlight colour.
//! - Where [`BodyView::context_menu`] is on (reading pane, read windows), right-click offers Copy, Select All,
//!   Select Text With Similar Formatting and Find… (reported in [`BodyView::find_requested`]).
//! - Images: `data:image/…;base64` pictures are decoded (PNG, JPEG, GIF; [`decode_image`], at most
//!   [`MAX_IMAGE_SIDE`] pixels a side) and drawn in their box; `cid:` pictures are drawn once the owner hands
//!   over the attachment bytes with [`provide_image`]. Anything else (undecodable data, unknown `cid:`,
//!   `http(s):`, unless consent permits desktop downloading) shows a framed placeholder with its alt text; remote pictures are
//!   labelled blocked unless [`BodyOptions::allow_remote`]. Decoded pictures are kept as textures in a small
//!   per-context cache ([`MAX_IMAGE_CACHE`] pictures).
//! - Dark mode: colours are re-mapped by lightness (dark text becomes light, white backgrounds become dark) so
//!   authored hues survive; pass `dark = false` for the "light background" toggle.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::{Color32, CursorIcon, FontFamily, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};
use mailcraft_fonts::{FontDb, FontId as McFontId};
use mailcraft_geom::Color;
use mailcraft_layout::{DisplayItem, Layout, LayoutOptions, LineBox, TextPos, layout_doc};
use mailcraft_model::{Block, Doc, Inline};

/// Cached layout of one body at one width.
#[derive(Clone, Debug, Default)]
pub struct BodyView {
    pub width: f32,
    pub layout: Option<mailcraft_layout::Layout>,
    /// Cache key of `layout` (content hash, width, zoom).
    pub key: u64,
    /// Content hash of the doc the selection belongs to.
    pub doc_key: u64,
    /// Selection as (anchor, focus); either order.
    pub selection: Option<(TextPos, TextPos)>,
    /// More selected ranges (Select Text With Similar Formatting), each in document order.
    pub extra: Vec<(TextPos, TextPos)>,
    /// Find hits, painted on the highlight colour (document order).
    pub marks: Vec<(TextPos, TextPos)>,
    /// Top-left corner of the body as last painted (screen coordinates).
    pub origin: Option<Pos2>,
    /// The body widget's id as last painted (to give it keyboard focus for Ctrl+C).
    pub widget: Option<Id>,
    /// Offer the right-click menu (Copy, Select All, Select Text With Similar Formatting, Find…).
    pub context_menu: bool,
    /// Find… was picked from the right-click menu (the owner opens its find bar and clears this).
    pub find_requested: bool,
    /// Where the right-click menu was opened.
    menu_at: Option<TextPos>,
}

impl BodyView {
    /// Selection in document order, when non-empty.
    pub fn ordered_selection(&self) -> Option<(TextPos, TextPos)> {
        let (a, b) = self.selection.as_ref()?;
        if a == b {
            return None;
        }
        Some(if a <= b { (a.clone(), b.clone()) } else { (b.clone(), a.clone()) })
    }
}

/// Rendering options beyond the frozen [`show_doc`] signature.
#[derive(Clone, Debug)]
pub struct BodyOptions {
    /// Dark rendering (colours re-mapped, dark background).
    pub dark: bool,
    /// Zoom factor (1.0 = 100 %).
    pub zoom: f32,
    /// Remote (http/https) images allowed ("Download pictures"). Desktop downloads run in the background.
    pub allow_remote: bool,
    /// Paint the page background behind the body.
    pub paint_background: bool,
    /// Precomputed content key for `doc` (skips hashing the doc every frame); `None` hashes it.
    pub doc_key: Option<u64>,
}

impl Default for BodyOptions {
    fn default() -> Self {
        BodyOptions { dark: false, zoom: 1.0, allow_remote: false, paint_background: true, doc_key: None }
    }
}

/// Paints `doc` into `ui` at the available width; returns the clicked link, if any.
pub fn show_doc(ui: &mut egui::Ui, view: &mut BodyView, doc: &Doc, dark: bool) -> Option<String> {
    show_doc_with(ui, view, doc, &BodyOptions { dark, ..BodyOptions::default() })
}

/// [`show_doc`] with explicit options.
pub fn show_doc_with(ui: &mut Ui, view: &mut BodyView, doc: &Doc, opts: &BodyOptions) -> Option<String> {
    let width = ui.available_width();
    let width = if width.is_finite() && width > 1.0 { width } else { 1.0 };
    let zoom = if opts.zoom.is_finite() && opts.zoom > 0.05 { opts.zoom.min(5.0) } else { 1.0 };
    let doc_key = opts.doc_key.unwrap_or_else(|| doc_hash(doc));
    let intrinsic_images = intrinsic_image_sizes(ui.ctx(), doc, opts.allow_remote);
    let key = {
        let mut h = DefaultHasher::new();
        doc_key.hash(&mut h);
        intrinsic_images.hash(&mut h);
        ((width * 2.0).round() as i64).hash(&mut h);
        zoom.to_bits().hash(&mut h);
        h.finish()
    };
    if view.doc_key != doc_key {
        view.selection = None;
        view.extra.clear();
        view.marks.clear();
        view.doc_key = doc_key;
    }
    let db = font_db(ui.ctx());
    if view.key != key || view.layout.is_none() {
        let lo = LayoutOptions { width, zoom, intrinsic_images, ..LayoutOptions::default() };
        view.layout = Some(layout_doc(doc, &db, &lo));
        view.width = width;
        view.key = key;
    }
    let layout = view.layout.take()?;
    let out = paint_and_interact(ui, view, &layout, doc, &db, opts);
    view.layout = Some(layout);
    out
}

/// True when the doc references remote (http/https) images.
pub fn has_remote_images(doc: &Doc) -> bool {
    let mut found = false;
    walk_inlines(&doc.blocks, 0, &mut |i| {
        if let Inline::Image { src, .. } = i
            && is_remote(src)
        {
            found = true;
        }
    });
    found
}

/// Content hash of a doc (stable within one process run).
pub fn doc_hash(doc: &Doc) -> u64 {
    let mut h = DefaultHasher::new();
    let _ = std::fmt::write(&mut HashWriter(&mut h), format_args!("{doc:?}"));
    h.finish()
}

/// Plain text of the current selection (blocks joined by line breaks), if any.
pub fn selected_text(view: &BodyView, doc: &Doc) -> Option<String> {
    let (a, b) = view.ordered_selection()?;
    range_text(view, doc, &a, &b)
}

/// Text of the selection and the extra ranges, in document order, one range per line.
pub fn selected_text_all(view: &BodyView, doc: &Doc) -> Option<String> {
    if view.extra.is_empty() {
        return selected_text(view, doc);
    }
    let mut ranges: Vec<(TextPos, TextPos)> = view.ordered_selection().into_iter().collect();
    ranges.extend(view.extra.iter().cloned());
    ranges.sort();
    ranges.dedup();
    let parts: Vec<String> = ranges.iter().filter_map(|(a, b)| range_text(view, doc, a, b)).collect();
    if parts.is_empty() { None } else { Some(parts.join("\n")) }
}

/// Plain text between `a` and `b` (document order) over the laid-out lines.
fn range_text(view: &BodyView, doc: &Doc, a: &TextPos, b: &TextPos) -> Option<String> {
    let (a, b) = (a.clone(), b.clone());
    let layout = view.layout.as_ref()?;
    let mut out = String::new();
    let mut prev_path: Option<&Vec<usize>> = None;
    let mut cache: (Vec<usize>, Vec<char>) = (Vec::new(), Vec::new());
    let mut cached = false;
    for line in &layout.index.lines {
        let Some((s, e)) = line_selection(line, &a, &b) else { continue };
        if let Some(p) = prev_path
            && p != &line.block_path
        {
            out.push('\n');
        }
        prev_path = Some(&line.block_path);
        if !cached || cache.0 != line.block_path {
            let t = block_text(doc, &line.block_path).unwrap_or_default();
            cache = (line.block_path.clone(), t.chars().collect());
            cached = true;
        }
        for c in cache.1.iter().skip(s).take(e.saturating_sub(s)) {
            if *c != '\u{FFFC}' {
                out.push(*c);
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

// ---------------------------------------------------------------------------------------------------------
// Painting and interaction

fn paint_and_interact(
    ui: &mut Ui,
    view: &mut BodyView,
    layout: &Layout,
    doc: &Doc,
    db: &FontDb,
    opts: &BodyOptions,
) -> Option<String> {
    let height = if layout.height.is_finite() { layout.height.max(1.0) } else { 1.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(view.width, height), Sense::click_and_drag());
    let origin = rect.min;
    view.origin = Some(origin);
    view.widget = Some(resp.id);
    let to_doc = |p: Pos2| mailcraft_geom::Point::new(p.x - origin.x, p.y - origin.y);

    // --- interaction -------------------------------------------------------------------------------
    let mut clicked_link = None;
    let hover_link = resp.hover_pos().and_then(|p| layout.hit(to_doc(p)).map(str::to_owned));
    if let Some(href) = &hover_link {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        let tip = href.clone();
        resp.clone().on_hover_text_at_pointer(tip);
    } else if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }
    if resp.drag_started()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let start = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
        view.selection = layout.hit_pos(to_doc(start)).map(|t| (t.clone(), t));
        view.extra.clear();
        resp.request_focus();
    }
    if resp.dragged()
        && let (Some(p), Some((anchor, _))) = (resp.interact_pointer_pos(), view.selection.clone())
        && let Some(t) = layout.hit_pos(to_doc(p))
    {
        view.selection = Some((anchor, t));
        // Auto-scroll when dragging near the visible edge.
        let clip = ui.clip_rect();
        if p.y > clip.bottom() - 8.0 || p.y < clip.top() + 8.0 {
            ui.scroll_to_rect(Rect::from_center_size(p, Vec2::splat(16.0)), None);
        }
    }
    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos()
            && let Some(t) = layout.hit_pos(to_doc(p))
        {
            view.selection = word_at(doc, &t);
            view.extra.clear();
        }
    } else if resp.clicked() {
        resp.request_focus();
        if let Some(href) = &hover_link {
            clicked_link = Some(href.clone());
        } else {
            view.selection = None;
            view.extra.clear();
        }
    }
    if view.context_menu {
        context_menu(ui, view, layout, doc, &resp, origin);
    }
    if resp.has_focus() {
        let (copy, select_all) = ui.input(|i| {
            let copy = i.events.iter().any(|e| {
                matches!(e, egui::Event::Copy)
                    || matches!(e, egui::Event::Key { key: egui::Key::C, pressed: true, modifiers, .. }
                        if modifiers.command)
            });
            let all = i.events.iter().any(|e| {
                matches!(e, egui::Event::Key { key: egui::Key::A, pressed: true, modifiers, .. } if modifiers.command)
            });
            (copy, all)
        });
        if select_all {
            view.selection = select_all_range(layout);
            view.extra.clear();
        }
        if copy && let Some(text) = selected_text_all(view, doc) {
            ui.ctx().copy_text(text);
        }
    }
    resp.widget_info(|| {
        let mut text = doc.to_plain_text();
        if text.len() > 4000 {
            let mut cut = 4000;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
        }
        egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text)
    });

    // --- painting ----------------------------------------------------------------------------------
    let clip = ui.clip_rect().intersect(rect);
    if clip.width() <= 0.0 || clip.height() <= 0.0 {
        return clicked_link;
    }
    let painter = ui.painter().with_clip_rect(clip);
    let dark = opts.dark;
    if opts.paint_background {
        let bg = map_color(doc.background.unwrap_or(Color::WHITE), dark);
        painter.rect_filled(rect, 0.0, bg);
    }
    paint_ranges(&painter, layout, &view.marks, origin, clip, mark_fill(dark));
    paint_selection(&painter, layout, view, origin, clip, dark);
    paint_ranges(&painter, layout, &view.extra, origin, clip, selection_fill(dark));
    let ctx = ui.ctx().clone();
    let mut families = FamilyCache::default();
    let rect_of = |r: &mailcraft_geom::Rect| {
        Rect::from_min_size(Pos2::new(origin.x + r.x, origin.y + r.y), Vec2::new(r.w.max(0.0), r.h.max(0.0)))
    };
    for item in &layout.display.items {
        match item {
            DisplayItem::FillRect { rect: r, color } => {
                let rr = rect_of(r);
                if rr.intersects(clip) {
                    painter.rect_filled(rr, 0.0, map_color(*color, dark));
                }
            }
            DisplayItem::StrokeRect { rect: r, color, width } => {
                let rr = rect_of(r);
                if rr.expand(*width).intersects(clip) {
                    let w = if width.is_finite() { width.clamp(0.0, 50.0) } else { 1.0 };
                    painter.rect_stroke(rr, 0.0, Stroke::new(w, map_color(*color, dark)), StrokeKind::Inside);
                }
            }
            DisplayItem::Line { from, to, color, width } => {
                let a = Pos2::new(origin.x + from.x, origin.y + from.y);
                let b = Pos2::new(origin.x + to.x, origin.y + to.y);
                if Rect::from_two_pos(a, b).expand(width.abs() + 1.0).intersects(clip) {
                    let w = if width.is_finite() { width.clamp(0.0, 50.0) } else { 1.0 };
                    painter.line_segment([a, b], Stroke::new(w, map_color(*color, dark)));
                }
            }
            DisplayItem::Text { origin: o, text, font, size_px, color, underline, strike, .. } => {
                if !size_px.is_finite() || *size_px <= 0.0 || text.is_empty() {
                    continue;
                }
                let size = size_px.min(400.0);
                let base = Pos2::new(origin.x + o.x, origin.y + o.y);
                // Cheap cull before shaping: one em above and half an em below the baseline.
                let approx = Rect::from_min_max(
                    Pos2::new(base.x - size, base.y - size * 1.3),
                    Pos2::new(clip.right().max(base.x) + 1.0, base.y + size * 0.6),
                );
                if !approx.intersects(clip) || base.x > clip.right() {
                    continue;
                }
                let col = map_color(*color, dark);
                let family = families.get(&ctx, db, *font);
                let galley = ctx.fonts_mut(|f| {
                    f.layout_no_wrap(text.clone(), egui::FontId::new(size, family.clone()), col)
                });
                let baseline = galley
                    .rows
                    .first()
                    .and_then(|r| r.row.glyphs.first().map(|g| r.pos.y + g.pos.y))
                    .unwrap_or(size * 0.8);
                let w = galley.size().x;
                painter.galley(Pos2::new(base.x, base.y - baseline), galley, col);
                let thick = (size / 14.0).max(1.0);
                if *underline {
                    let y = base.y + (size * 0.12).max(1.0);
                    painter.line_segment(
                        [Pos2::new(base.x, y), Pos2::new(base.x + w, y)],
                        Stroke::new(thick, col),
                    );
                }
                if *strike {
                    let y = base.y - size * 0.3;
                    painter.line_segment(
                        [Pos2::new(base.x, y), Pos2::new(base.x + w, y)],
                        Stroke::new(thick, col),
                    );
                }
            }
            DisplayItem::Image { rect: r, src, alt } => {
                let rr = rect_of(r);
                if rr.intersects(clip) {
                    match permitted_image_texture(&ctx, src, opts.allow_remote) {
                        Some(tex) => paint_texture(&painter, rr, &tex),
                        None => paint_image_placeholder(&painter, rr, src, alt, opts, &ctx),
                    }
                }
            }
        }
    }
    clicked_link
}

fn paint_image_placeholder(
    painter: &egui::Painter,
    r: Rect,
    src: &str,
    alt: &str,
    opts: &BodyOptions,
    ctx: &egui::Context,
) {
    let dark = opts.dark;
    let fill = if dark { Color32::from_rgb(0x2c, 0x30, 0x35) } else { Color32::from_rgb(0xf0, 0xf2, 0xf4) };
    let line = if dark { Color32::from_rgb(0x55, 0x5b, 0x63) } else { Color32::from_rgb(0xc4, 0xc9, 0xd0) };
    let ink = if dark { Color32::from_rgb(0xa8, 0xaf, 0xb8) } else { Color32::from_rgb(0x6a, 0x71, 0x7a) };
    painter.rect_filled(r, 2.0, fill);
    painter.rect_stroke(r, 2.0, Stroke::new(1.0, line), StrokeKind::Inside);
    // A small picture glyph (frame, sun, hill) when there is room.
    if r.width() >= 14.0 && r.height() >= 12.0 {
        let s = r.width().min(r.height()).min(28.0) * 0.6;
        let c = if r.width() >= 60.0 && r.height() >= 40.0 {
            Pos2::new(r.left() + 6.0 + s / 2.0, r.top() + 6.0 + s / 2.0)
        } else {
            r.center()
        };
        let g = Rect::from_center_size(c, Vec2::new(s, s * 0.8));
        painter.rect_stroke(g, 1.0, Stroke::new(1.0, ink), StrokeKind::Inside);
        painter.circle_filled(
            Pos2::new(g.left() + g.width() * 0.7, g.top() + g.height() * 0.3),
            s * 0.09,
            ink,
        );
        painter.line_segment(
            [
                Pos2::new(g.left() + 1.0, g.bottom() - 1.0),
                Pos2::new(g.left() + g.width() * 0.4, g.top() + g.height() * 0.45),
            ],
            Stroke::new(1.0, ink),
        );
        painter.line_segment(
            [
                Pos2::new(g.left() + g.width() * 0.4, g.top() + g.height() * 0.45),
                Pos2::new(g.right() - 1.0, g.bottom() - 1.0),
            ],
            Stroke::new(1.0, ink),
        );
        if r.width() >= 60.0 && r.height() >= 40.0 {
            let label = if !alt.trim().is_empty() {
                alt.trim().to_owned()
            } else if is_remote(src) && !opts.allow_remote {
                "Picture blocked".to_owned()
            } else {
                "Picture".to_owned()
            };
            let galley = ctx.fonts_mut(|f| {
                f.layout(label, egui::FontId::proportional(11.0), ink, (r.width() - 12.0).max(10.0))
            });
            let pos = Pos2::new(r.left() + 6.0, (c.y + s / 2.0 + 4.0).min(r.bottom() - 14.0));
            painter.with_clip_rect(r.shrink(2.0).intersect(painter.clip_rect())).galley(pos, galley, ink);
        }
    }
}

/// Selection colour.
fn selection_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgba_unmultiplied(0x4a, 0x8f, 0xd8, 110)
    } else {
        Color32::from_rgba_unmultiplied(0x3a, 0x86, 0xd6, 80)
    }
}

/// Find-hit colour (a highlighter yellow).
fn mark_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgba_unmultiplied(0xc8, 0xa0, 0x00, 120)
    } else {
        Color32::from_rgba_unmultiplied(0xff, 0xd5, 0x00, 140)
    }
}

/// Most ranges painted per frame.
const MAX_PAINTED_RANGES: usize = 5_000;

/// Paints document-order ranges (find hits, extra selections) under the text.
fn paint_ranges(
    painter: &egui::Painter,
    layout: &Layout,
    ranges: &[(TextPos, TextPos)],
    origin: Pos2,
    clip: Rect,
    fill: Color32,
) {
    for (a, b) in ranges.iter().take(MAX_PAINTED_RANGES) {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if a == b {
            continue;
        }
        paint_range(painter, layout, a, b, origin, clip, fill);
    }
}

fn paint_selection(
    painter: &egui::Painter,
    layout: &Layout,
    view: &BodyView,
    origin: Pos2,
    clip: Rect,
    dark: bool,
) {
    let Some((a, b)) = view.ordered_selection() else { return };
    paint_range(painter, layout, &a, &b, origin, clip, selection_fill(dark));
}

fn paint_range(
    painter: &egui::Painter,
    layout: &Layout,
    a: &TextPos,
    b: &TextPos,
    origin: Pos2,
    clip: Rect,
    fill: Color32,
) {
    for line in &layout.index.lines {
        let Some((s, e)) = line_selection(line, a, b) else { continue };
        let x0 = x_of(line, s);
        let x1 =
            if e >= line.end && e > s { line_right(layout, line).max(x_of(line, e)) } else { x_of(line, e) };
        let r = Rect::from_min_max(
            Pos2::new(origin.x + x0, origin.y + line.rect.y),
            Pos2::new(origin.x + x1.max(x0 + 3.0), origin.y + line.rect.bottom()),
        );
        if r.intersects(clip) {
            painter.rect_filled(r, 0.0, fill);
        }
    }
}

/// Selected char range `[s, e)` of a line for the ordered selection `a..b`.
fn line_selection(line: &LineBox, a: &TextPos, b: &TextPos) -> Option<(usize, usize)> {
    let start = TextPos { block_path: line.block_path.clone(), offset: line.start };
    let end = TextPos { block_path: line.block_path.clone(), offset: line.end };
    if *b <= start || *a >= end {
        // Zero-length lines (empty paragraphs) count when strictly inside the selection.
        if line.start == line.end && *a < start && start < *b {
            return Some((line.start, line.end));
        }
        return None;
    }
    let s = if a.block_path == line.block_path { a.offset.max(line.start) } else { line.start };
    let e = if b.block_path == line.block_path { b.offset.min(line.end) } else { line.end };
    if e < s { None } else { Some((s, e)) }
}

/// x of char offset `off` on a line (nearest stop at or before it).
fn x_of(line: &LineBox, off: usize) -> f32 {
    line.stops
        .iter()
        .rfind(|s| s.offset <= off)
        .or_else(|| line.stops.first())
        .map(|s| s.x)
        .unwrap_or(line.rect.x)
}

/// Right edge of the ink on a line (the rightmost text run vertically inside it).
fn line_right(layout: &Layout, line: &LineBox) -> f32 {
    let top = line.rect.y;
    let bottom = line.rect.bottom();
    let mut right: Option<f32> = None;
    for run in &layout.index.runs {
        let cy = run.rect.y + run.rect.h / 2.0;
        if cy >= top
            && cy <= bottom
            && run.rect.right() <= line.rect.right() + 1.0
            && run.rect.x >= line.rect.x - 1.0
        {
            right = Some(right.map_or(run.rect.right(), |r: f32| r.max(run.rect.right())));
        }
    }
    right.unwrap_or_else(|| line.stops.last().map(|s| s.x).unwrap_or(line.rect.x))
}

fn select_all_range(layout: &Layout) -> Option<(TextPos, TextPos)> {
    let first =
        layout.index.lines.iter().min_by(|x, y| (&x.block_path, x.start).cmp(&(&y.block_path, y.start)))?;
    let last =
        layout.index.lines.iter().max_by(|x, y| (&x.block_path, x.end).cmp(&(&y.block_path, y.end)))?;
    Some((
        TextPos { block_path: first.block_path.clone(), offset: first.start },
        TextPos { block_path: last.block_path.clone(), offset: last.end },
    ))
}

/// Word around `pos` (alphanumeric run), as a selection.
fn word_at(doc: &Doc, pos: &TextPos) -> Option<(TextPos, TextPos)> {
    let text: Vec<char> = block_text(doc, &pos.block_path)?.chars().collect();
    let is_word = |c: &char| c.is_alphanumeric() || *c == '_' || *c == '\'';
    let mut s = pos.offset.min(text.len());
    let mut e = s;
    while s > 0 && text.get(s - 1).is_some_and(is_word) {
        s -= 1;
    }
    while text.get(e).is_some_and(is_word) {
        e += 1;
    }
    if s == e {
        return None;
    }
    Some((
        TextPos { block_path: pos.block_path.clone(), offset: s },
        TextPos { block_path: pos.block_path.clone(), offset: e },
    ))
}

// ---------------------------------------------------------------------------------------------------------
// Select, find and the right-click menu

/// Context-menu labels of the read-only body.
pub const MENU_COPY: &str = "Copy";
pub const MENU_SELECT_ALL: &str = "Select All";
pub const MENU_SIMILAR: &str = "Select Text With Similar Formatting";
pub const MENU_FIND: &str = "Find...";

fn context_menu(
    ui: &Ui,
    view: &mut BodyView,
    layout: &Layout,
    doc: &Doc,
    resp: &egui::Response,
    origin: Pos2,
) {
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let at = layout.hit_pos(mailcraft_geom::Point::new(p.x - origin.x, p.y - origin.y));
        // A right-click outside the selection drops it (the menu then works on the clicked spot).
        let inside = match (view.ordered_selection(), &at) {
            (Some((a, b)), Some(t)) => a <= *t && *t <= b,
            _ => false,
        };
        if !inside {
            view.selection = None;
            view.extra.clear();
        }
        view.menu_at = at;
        resp.request_focus();
    }
    let has_selection = view.ordered_selection().is_some();
    let mut pick: Option<&'static str> = None;
    resp.context_menu(|ui| {
        ui.set_min_width(220.0);
        if ui.add_enabled(has_selection, egui::Button::new(MENU_COPY)).clicked() {
            pick = Some(MENU_COPY);
        }
        if ui.button(MENU_SELECT_ALL).clicked() {
            pick = Some(MENU_SELECT_ALL);
        }
        if ui.button(MENU_SIMILAR).clicked() {
            pick = Some(MENU_SIMILAR);
        }
        ui.separator();
        if ui.button(MENU_FIND).clicked() {
            pick = Some(MENU_FIND);
        }
        if pick.is_some() {
            ui.close();
        }
    });
    match pick {
        Some(MENU_COPY) => {
            if let Some(text) = selected_text_all(view, doc) {
                ui.ctx().copy_text(text);
            }
        }
        Some(MENU_SELECT_ALL) => {
            view.selection = select_all_range(layout);
            view.extra.clear();
        }
        Some(MENU_SIMILAR) => {
            let at = view.ordered_selection().map(|(a, _)| a).or_else(|| view.menu_at.clone());
            if let Some(at) = at {
                select_similar_at(view, doc, &at);
            }
        }
        Some(MENU_FIND) => view.find_requested = true,
        _ => {}
    }
}

/// Selects the whole body (Select All).
pub fn select_all(view: &mut BodyView) {
    view.selection = view.layout.as_ref().and_then(select_all_range);
    view.extra.clear();
}

/// Select Text With Similar Formatting from `at`: every run formatted like the text there becomes selected
/// (the first as the selection, the rest as extra ranges). Returns how many ranges were selected.
pub fn select_similar_at(view: &mut BodyView, doc: &Doc, at: &TextPos) -> usize {
    let mut ranges = similar_ranges(doc, at);
    let n = ranges.len();
    if ranges.is_empty() {
        return 0;
    }
    let first = ranges.remove(0);
    view.selection = Some(first);
    view.extra = ranges;
    n
}

/// Most find hits / similar ranges collected.
pub const MAX_RANGES: usize = 10_000;
/// Deepest block nesting walked for leaves.
const MAX_LEAF_DEPTH: usize = 64;

/// Leaf block paths of a doc in document order (paragraphs, headings and preformatted blocks), using the
/// layout's path scheme.
pub fn leaf_paths(doc: &Doc) -> Vec<Vec<usize>> {
    fn walk(blocks: &[Block], prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if prefix.len() > MAX_LEAF_DEPTH || out.len() >= 1_000_000 {
            return;
        }
        for (i, b) in blocks.iter().enumerate() {
            prefix.push(i);
            match b {
                Block::Paragraph { .. } | Block::Heading { .. } | Block::Pre { .. } => {
                    out.push(prefix.clone())
                }
                Block::List { items, .. } => {
                    for (k, item) in items.iter().enumerate() {
                        prefix.push(k);
                        walk(item, prefix, out);
                        prefix.pop();
                    }
                }
                Block::Quote { blocks } | Block::Container { blocks, .. } => walk(blocks, prefix, out),
                Block::Table { rows, .. } => {
                    for (r, row) in rows.iter().enumerate() {
                        for (c, cell) in row.iter().enumerate() {
                            prefix.push(r);
                            prefix.push(c);
                            walk(&cell.blocks, prefix, out);
                            prefix.pop();
                            prefix.pop();
                        }
                    }
                }
                Block::Rule => {}
            }
            prefix.pop();
        }
    }
    let mut out = Vec::new();
    walk(&doc.blocks, &mut Vec::new(), &mut out);
    out
}

/// Simple per-char case folding that keeps char offsets (one char in, one char out).
fn fold(c: char, match_case: bool) -> char {
    if match_case { c } else { c.to_lowercase().next().unwrap_or(c) }
}

/// Every occurrence of `needle` in the doc's text (within one paragraph each), in document order.
pub fn find_all(doc: &Doc, needle: &str, match_case: bool) -> Vec<(TextPos, TextPos)> {
    let pat: Vec<char> = needle.chars().map(|c| fold(c, match_case)).collect();
    let mut out = Vec::new();
    if pat.is_empty() || pat.len() > 1000 {
        return out;
    }
    for path in leaf_paths(doc) {
        let Some(text) = block_text(doc, &path) else { continue };
        let hay: Vec<char> = text.chars().map(|c| fold(c, match_case)).collect();
        let mut i = 0usize;
        while i + pat.len() <= hay.len() {
            if hay.get(i..i + pat.len()) == Some(pat.as_slice()) {
                out.push((
                    TextPos { block_path: path.clone(), offset: i },
                    TextPos { block_path: path.clone(), offset: i + pat.len() },
                ));
                if out.len() >= MAX_RANGES {
                    return out;
                }
                i += pat.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Character runs of a leaf: `(start, end, style, in a link)`; line breaks and pictures are left out.
fn leaf_runs(doc: &Doc, path: &[usize]) -> Vec<(usize, usize, mailcraft_model::TextStyle, bool)> {
    fn walk(
        inlines: &[Inline],
        link: bool,
        at: &mut usize,
        out: &mut Vec<(usize, usize, mailcraft_model::TextStyle, bool)>,
        depth: usize,
    ) {
        if depth > MAX_LEAF_DEPTH {
            return;
        }
        for i in inlines {
            match i {
                Inline::Text { text, style } => {
                    let n = text.chars().count();
                    if n > 0 {
                        out.push((*at, at.saturating_add(n), style.clone(), link));
                    }
                    *at = at.saturating_add(n);
                }
                Inline::LineBreak | Inline::Image { .. } => *at = at.saturating_add(1),
                Inline::Link { children, .. } => walk(children, true, at, out, depth + 1),
            }
        }
    }
    let mut out = Vec::new();
    match block_at(&doc.blocks, path) {
        Some(Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. }) => {
            walk(inlines, false, &mut 0, &mut out, 0);
        }
        Some(Block::Pre { text }) => {
            let n = text.chars().count();
            if n > 0 {
                let mono = mailcraft_model::TextStyle { monospace: Some(true), ..Default::default() };
                out.push((0, n, mono, false));
            }
        }
        _ => {}
    }
    out
}

/// The formatting at `at` (of the char after it, or before it at the end of a paragraph): its style, link
/// state and heading level.
fn format_at(doc: &Doc, at: &TextPos) -> Option<(mailcraft_model::TextStyle, bool, u8)> {
    let runs = leaf_runs(doc, &at.block_path);
    let run = runs
        .iter()
        .find(|(a, b, ..)| *a <= at.offset && at.offset < *b)
        .or_else(|| runs.iter().rev().find(|(_, b, ..)| *b <= at.offset))
        .or_else(|| runs.first())?;
    Some((run.2.clone(), run.3, heading_level(doc, &at.block_path)))
}

fn heading_level(doc: &Doc, path: &[usize]) -> u8 {
    match block_at(&doc.blocks, path) {
        Some(Block::Heading { level, .. }) => *level,
        _ => 0,
    }
}

/// Ranges formatted like the text at `at` (same character style, link state and heading level), merged
/// within each paragraph, in document order.
pub fn similar_ranges(doc: &Doc, at: &TextPos) -> Vec<(TextPos, TextPos)> {
    let Some(want) = format_at(doc, at) else {
        return Vec::new();
    };
    let mut out: Vec<(TextPos, TextPos)> = Vec::new();
    for path in leaf_paths(doc) {
        let level = heading_level(doc, &path);
        if level != want.2 {
            continue;
        }
        let mut open: Option<(usize, usize)> = None;
        for (a, b, style, link) in leaf_runs(doc, &path) {
            if style == want.0 && link == want.1 {
                open = match open {
                    // Runs split only by a line break or picture still read as one stretch.
                    Some((s, e)) if a <= e.saturating_add(1) => Some((s, b)),
                    Some((s, e)) => {
                        out.push((
                            TextPos { block_path: path.clone(), offset: s },
                            TextPos { block_path: path.clone(), offset: e },
                        ));
                        Some((a, b))
                    }
                    None => Some((a, b)),
                };
            } else if let Some((s, e)) = open.take() {
                out.push((
                    TextPos { block_path: path.clone(), offset: s },
                    TextPos { block_path: path.clone(), offset: e },
                ));
            }
            if out.len() >= MAX_RANGES {
                return out;
            }
        }
        if let Some((s, e)) = open {
            out.push((
                TextPos { block_path: path.clone(), offset: s },
                TextPos { block_path: path.clone(), offset: e },
            ));
        }
    }
    out
}

/// Screen rectangle of a range's first line as last painted (to scroll a find hit into view).
pub fn range_rect(view: &BodyView, range: &(TextPos, TextPos)) -> Option<Rect> {
    let origin = view.origin?;
    let layout = view.layout.as_ref()?;
    let a = layout.caret_rect(&range.0)?;
    let b = layout.caret_rect(&range.1).filter(|b| (b.y - a.y).abs() < 0.5);
    let right = b.map_or(a.x + 8.0, |b| b.x.max(a.x + 1.0));
    let r = Rect::from_min_max(
        Pos2::new(origin.x + a.x, origin.y + a.y),
        Pos2::new(origin.x + right, origin.y + a.y + a.h),
    );
    r.is_finite().then_some(r)
}

// ---------------------------------------------------------------------------------------------------------
// Doc helpers

/// Block at a layout block path (see `mailcraft_layout` docs for the path scheme).
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

/// Flattened inline text of a block (line break = '\n', image = U+FFFC; Pre = its text).
fn block_text(doc: &Doc, path: &[usize]) -> Option<String> {
    match block_at(&doc.blocks, path)? {
        Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => {
            let mut s = String::new();
            flatten_inlines(inlines, &mut s, 0);
            Some(s)
        }
        Block::Pre { text } => Some(text.clone()),
        _ => None,
    }
}

fn flatten_inlines(inlines: &[Inline], out: &mut String, depth: usize) {
    for i in inlines {
        match i {
            Inline::Text { text, .. } => out.push_str(text),
            Inline::LineBreak => out.push('\n'),
            Inline::Image { .. } => out.push('\u{FFFC}'),
            Inline::Link { children, .. } if depth < 256 => flatten_inlines(children, out, depth + 1),
            Inline::Link { .. } => {}
        }
    }
}

fn walk_inlines(blocks: &[Block], depth: usize, f: &mut dyn FnMut(&Inline)) {
    if depth > 64 {
        return;
    }
    for b in blocks {
        match b {
            Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => {
                walk_inline_list(inlines, 0, f)
            }
            Block::List { items, .. } => {
                for item in items {
                    walk_inlines(item, depth + 1, f);
                }
            }
            Block::Quote { blocks } | Block::Container { blocks, .. } => walk_inlines(blocks, depth + 1, f),
            Block::Table { rows, .. } => {
                for cell in rows.iter().flatten() {
                    walk_inlines(&cell.blocks, depth + 1, f);
                }
            }
            Block::Pre { .. } | Block::Rule => {}
        }
    }
}

fn walk_inline_list(inlines: &[Inline], depth: usize, f: &mut dyn FnMut(&Inline)) {
    for i in inlines {
        f(i);
        if let Inline::Link { children, .. } = i
            && depth < 64
        {
            walk_inline_list(children, depth + 1, f);
        }
    }
}

fn is_remote(src: &str) -> bool {
    let s = src.trim_start();
    let lower: String = s.chars().take(8).collect::<String>().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("//")
}

struct HashWriter<'a>(&'a mut DefaultHasher);

impl std::fmt::Write for HashWriter<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.write(s.as_bytes());
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------
// Pictures

/// Longest side of a picture that is decoded (pixels); bigger pictures show the placeholder.
pub const MAX_IMAGE_SIDE: u32 = 4096;
/// Largest encoded picture that is decoded (bytes).
pub const MAX_IMAGE_BYTES: usize = 25 * 1024 * 1024;
/// Most memory a decoder may allocate for one picture (a 4096 × 4096 RGBA picture plus working space).
const MAX_DECODE_ALLOC: u64 = 160 * 1024 * 1024;
/// Most decoded pictures kept as textures per egui context (oldest dropped first).
pub const MAX_IMAGE_CACHE: usize = 64;

/// Decodes a PNG, JPEG or GIF picture (first frame) into an image; `None` for other formats, damaged data,
/// an empty picture, or one beyond [`MAX_IMAGE_BYTES`] / [`MAX_IMAGE_SIDE`]. Never panics: the decoders run
/// with size limits, and a panic inside one is caught and treated as undecodable.
pub fn decode_image(data: &[u8]) -> Option<egui::ColorImage> {
    if data.is_empty() || data.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let format = image::guess_format(data).ok()?;
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::Gif) {
        return None;
    }
    std::panic::catch_unwind(|| decode_limited(data, format)).ok().flatten()
}

fn decode_limited(data: &[u8], format: image::ImageFormat) -> Option<egui::ColorImage> {
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(data), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let rgba = reader.decode().ok()?.into_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    if w == 0 || h == 0 || w > MAX_IMAGE_SIDE || h > MAX_IMAGE_SIDE {
        return None;
    }
    let size = [usize::try_from(w).ok()?, usize::try_from(h).ok()?];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

/// Decodes a contact's picture (PNG, JPEG or GIF) for the cards and the contact window.
pub fn decode_contact_picture(pic: &mailcraft_model::contact::ContactPicture) -> Option<egui::ColorImage> {
    decode_image(&pic.data)
}

/// The bytes of a `data:image/…;base64,…` URL; `None` for other URLs, other encodings and oversized data.
pub fn data_image_bytes(src: &str) -> Option<Vec<u8>> {
    let src = src.trim();
    let rest = src.get(..5).filter(|p| p.eq_ignore_ascii_case("data:")).and(src.get(5..))?;
    let (meta, payload) = rest.split_once(',')?;
    let mut parts = meta.split(';').map(str::trim);
    let mime = parts.next()?;
    if !mime.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("image/")) {
        return None;
    }
    if !parts.any(|x| x.eq_ignore_ascii_case("base64")) || payload.len() / 4 * 3 > MAX_IMAGE_BYTES {
        return None;
    }
    let data = mailcraft_mime::transfer::decode_base64(payload.as_bytes());
    (!data.is_empty() && data.len() <= MAX_IMAGE_BYTES).then_some(data)
}

/// Cheap fingerprint of a byte string: its length, both ends and samples across it (long `data:` URLs
/// are not hashed in full every frame).
fn fingerprint(b: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    b.len().hash(&mut h);
    if b.len() <= 4096 {
        b.hash(&mut h);
    } else {
        b.get(..1024).hash(&mut h);
        b.get(b.len().saturating_sub(1024)..).hash(&mut h);
        let step = b.len() / 64;
        for k in 1..64usize {
            let at = k.saturating_mul(step);
            b.get(at..at.saturating_add(16).min(b.len())).hash(&mut h);
        }
    }
    h.finish()
}

/// Decoded pictures by source: (source fingerprint, data fingerprint, texture or `None` when undecodable),
/// oldest first.
#[derive(Clone, Default)]
struct ImageCache {
    entries: Vec<(u64, u64, Option<egui::TextureHandle>)>,
}

impl ImageCache {
    fn get(&self, key: u64) -> Option<&(u64, u64, Option<egui::TextureHandle>)> {
        self.entries.iter().find(|e| e.0 == key)
    }

    fn put(&mut self, key: u64, data_key: u64, tex: Option<egui::TextureHandle>) {
        self.entries.retain(|e| e.0 != key);
        while self.entries.len() >= MAX_IMAGE_CACHE {
            self.entries.remove(0);
        }
        self.entries.push((key, data_key, tex));
    }
}

fn image_cache_id() -> Id {
    Id::new("mailcraft-body-images")
}

fn load_picture(ctx: &egui::Context, key: u64, data: &[u8]) -> Option<egui::TextureHandle> {
    decode_image(data).map(|img| {
        ctx.load_texture(format!("mailcraft-body-image-{key:016x}"), img, egui::TextureOptions::LINEAR)
    })
}

/// Hands over the bytes of the picture a body refers to as `src` (an inline attachment's `cid:…` URL), so
/// bodies draw it instead of the placeholder. Decodes once per source and data; returns whether the picture
/// could be decoded. Call it again when showing the message (pictures can drop out of the cache).
pub fn provide_image(ctx: &egui::Context, src: &str, data: &[u8]) -> bool {
    let key = fingerprint(src.trim().as_bytes());
    let data_key = fingerprint(data);
    let cached = ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<ImageCache>(image_cache_id())
            .get(key)
            .filter(|e| e.1 == data_key)
            .map(|e| e.2.is_some())
    });
    if let Some(ok) = cached {
        return ok;
    }
    let tex = load_picture(ctx, key, data);
    let ok = tex.is_some();
    ctx.data_mut(|d| d.get_temp_mut_or_default::<ImageCache>(image_cache_id()).put(key, data_key, tex));
    ok
}

/// The texture for a body picture: one handed over with [`provide_image`], or a `data:` picture (decoded on
/// first use). `None` draws the placeholder.
pub fn image_texture(ctx: &egui::Context, src: &str) -> Option<egui::TextureHandle> {
    let key = fingerprint(src.trim().as_bytes());
    if let Some(hit) = ctx
        .data_mut(|d| d.get_temp_mut_or_default::<ImageCache>(image_cache_id()).get(key).map(|e| e.2.clone()))
    {
        return hit;
    }
    let data = data_image_bytes(src)?;
    let tex = load_picture(ctx, key, &data);
    ctx.data_mut(|d| d.get_temp_mut_or_default::<ImageCache>(image_cache_id()).put(key, 0, tex.clone()));
    tex
}

/// Draws a decoded picture in its box, keeping its aspect ratio (top-left aligned).
fn paint_texture(painter: &egui::Painter, r: Rect, tex: &egui::TextureHandle) {
    let [w, h] = tex.size();
    let (w, h) = (w as f32, h as f32);
    let scale = if w > 0.0 && h > 0.0 { (r.width() / w).min(r.height() / h) } else { 0.0 };
    let size = if scale.is_finite() && scale > 0.0 { Vec2::new(w * scale, h * scale) } else { r.size() };
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    painter.image(tex.id(), Rect::from_min_size(r.min, size), uv, Color32::WHITE);
}

// ---------------------------------------------------------------------------------------------------------
// Fonts and colours

/// The bundled font database, shared through egui's temp data (built once per context).
pub fn font_db(ctx: &egui::Context) -> Arc<FontDb> {
    let id = Id::new("mailcraft.body.fontdb");
    if let Some(db) = ctx.data(|d| d.get_temp::<Arc<FontDb>>(id)) {
        return db;
    }
    let db = Arc::new(FontDb::bundled());
    ctx.data_mut(|d| d.insert_temp(id, Arc::clone(&db)));
    db
}

/// egui family name used for one bundled face.
pub fn face_family_name(family: &str, weight: u16, italic: bool) -> String {
    format!("mailcraft-body:{family}:{weight}:{}", if italic { "i" } else { "n" })
}

/// Per-paint cache of FontId → egui family.
#[derive(Default)]
struct FamilyCache {
    map: Vec<(McFontId, FontFamily)>,
}

impl FamilyCache {
    fn get(&mut self, ctx: &egui::Context, db: &FontDb, id: McFontId) -> FontFamily {
        if let Some((_, f)) = self.map.iter().find(|(i, _)| *i == id) {
            return f.clone();
        }
        let f = egui_family(ctx, db, id);
        self.map.push((id, f.clone()));
        f
    }
}

/// The egui family for a bundled face; registers the face with egui on first use (it takes effect next
/// frame, until then the default proportional/monospace family is used).
fn egui_family(ctx: &egui::Context, db: &FontDb, id: McFontId) -> FontFamily {
    let Some(face) = db.face(id) else { return FontFamily::Proportional };
    let mono = face.family.to_ascii_lowercase().contains("mono");
    let fallback = if mono { FontFamily::Monospace } else { FontFamily::Proportional };
    let name = face_family_name(face.family, face.weight, face.italic);
    let family = FontFamily::Name(name.as_str().into());
    let (bound, fallbacks) = ctx.fonts(|f| {
        let defs = f.definitions();
        let bound = defs.families.get(&family).is_some_and(|v| !v.is_empty());
        let fallbacks: Vec<(String, Arc<egui::FontData>)> = if bound {
            Vec::new()
        } else {
            defs.families
                .get(&fallback)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|n| {
                            let d = defs.font_data.get(n)?;
                            matches!(d.font, std::borrow::Cow::Borrowed(_))
                                .then(|| (n.clone(), Arc::clone(d)))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        (bound, fallbacks)
    });
    if bound {
        return family;
    }
    // Register once per context (the insert is applied at the start of the next pass).
    let flag = Id::new(("mailcraft.body.font-requested", name.as_str()));
    let requested = ctx.data(|d| d.get_temp::<bool>(flag)).unwrap_or(false);
    if !requested {
        ctx.data_mut(|d| d.insert_temp(flag, true));
        ctx.add_font(egui::epaint::text::FontInsert::new(
            &name,
            egui::FontData::from_static(face.data),
            vec![egui::epaint::text::InsertFontFamily {
                family: family.clone(),
                priority: egui::epaint::text::FontPriority::Highest,
            }],
        ));
        for (fb_name, data) in fallbacks {
            ctx.add_font(egui::epaint::text::FontInsert::new(
                &format!("{name}+{fb_name}"),
                egui::FontData { font: data.font.clone(), index: data.index, tweak: data.tweak.clone() },
                vec![egui::epaint::text::InsertFontFamily {
                    family: family.clone(),
                    priority: egui::epaint::text::FontPriority::Lowest,
                }],
            ));
        }
        ctx.request_repaint();
    }
    fallback
}

/// Geometry colour → egui colour, re-mapped for dark rendering.
pub fn map_color(c: Color, dark: bool) -> Color32 {
    if !dark {
        return Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a);
    }
    let (h, s, l) = rgb_to_hsl(c.r, c.g, c.b);
    // Invert lightness into a range that keeps text readable on our dark page (white → ≈ #222529).
    let l2 = 0.13 + 0.82 * (1.0 - l);
    let (r, g, b) = hsl_to_rgb(h, s * 0.85, l2);
    Color32::from_rgba_unmultiplied(r, g, b, c.a)
}

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = f32::from(r) / 255.0;
    let g = f32::from(g) / 255.0;
    let b = f32::from(b) / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d.abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if (max - r).abs() < f32::EPSILON {
        ((g - b) / d).rem_euclid(6.0)
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    (to(r1), to(g1), to(b1))
}

fn intrinsic_image_sizes(
    ctx: &egui::Context,
    doc: &Doc,
    allow_remote: bool,
) -> std::collections::BTreeMap<String, [usize; 2]> {
    #[cfg(not(target_arch = "wasm32"))]
    let remote = if allow_remote { crate::remote_images::cached_sizes(ctx) } else { Default::default() };
    #[cfg(target_arch = "wasm32")]
    let _ = allow_remote;
    let mut sizes = std::collections::BTreeMap::new();
    walk_inlines(&doc.blocks, 0, &mut |inline| {
        if let Inline::Image { src, .. } = inline {
            let size = if is_remote(src) {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    remote.get(src.trim()).copied()
                }
                #[cfg(target_arch = "wasm32")]
                {
                    None
                }
            } else {
                image_texture(ctx, src).map(|texture| texture.size())
            };
            if let Some(size) = size {
                sizes.insert(src.clone(), size);
            }
        }
    });
    sizes
}
fn permitted_image_texture(
    ctx: &egui::Context,
    src: &str,
    allow_remote: bool,
) -> Option<egui::TextureHandle> {
    if is_remote(src) {
        #[cfg(not(target_arch = "wasm32"))]
        return crate::remote_images::image_texture(ctx, src, allow_remote);
        #[cfg(target_arch = "wasm32")]
        {
            let _ = allow_remote;
            return None;
        }
    }
    image_texture(ctx, src)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_mapping_inverts_lightness() {
        let white = map_color(Color::WHITE, true);
        assert!(white.r() < 0x40, "{white:?}");
        let black = map_color(Color::BLACK, true);
        assert!(black.r() > 0xd0, "{black:?}");
        assert_eq!(map_color(Color::rgb(1, 2, 3), false), Color32::from_rgb(1, 2, 3));
    }

    #[test]
    fn remote_detection_and_text_paths() {
        let doc = mailcraft_html::html_to_doc(
            "<p>Hello <b>big</b> world</p><ul><li>one</li></ul><p><img src=\"https://x.example/a.png\" alt=\"A\"></p>",
        );
        assert!(has_remote_images(&doc));
        assert!(!has_remote_images(&Doc::from_plain_text("hi\nthere")));
        assert_eq!(block_text(&doc, &[0]).as_deref(), Some("Hello big world"));
        let w = word_at(&doc, &TextPos { block_path: vec![0], offset: 7 }).unwrap();
        assert_eq!((w.0.offset, w.1.offset), (6, 9));
        assert_ne!(doc_hash(&doc), doc_hash(&Doc::default()));
    }

    fn tp(path: &[usize], offset: usize) -> TextPos {
        TextPos { block_path: path.to_vec(), offset }
    }

    #[test]
    fn find_all_matches_across_nested_blocks() {
        let doc = mailcraft_html::html_to_doc(
            "<p>Apple pie, apple tart</p><ul><li>green APPLE</li></ul><table><tr><td>apple</td></tr></table>",
        );
        let paths = leaf_paths(&doc);
        assert_eq!(paths.len(), 3, "{paths:?}");
        let hits = find_all(&doc, "apple", false);
        assert_eq!(hits.len(), 4, "{hits:?}");
        assert_eq!(hits[0], (tp(&[0], 0), tp(&[0], 5)));
        assert_eq!(hits[1], (tp(&[0], 11), tp(&[0], 16)));
        assert_eq!(hits[2].0.block_path, paths[1]);
        assert_eq!(find_all(&doc, "apple", true).len(), 2);
        assert!(find_all(&doc, "", false).is_empty());
        assert!(find_all(&doc, "zebra", false).is_empty());
        // Case folding keeps char offsets for wide characters.
        let doc = Doc::from_plain_text("ÄÖ straße STRASSE");
        let hits = find_all(&doc, "ö", false);
        assert_eq!(hits, vec![(tp(&[0], 1), tp(&[0], 2))]);
    }

    #[test]
    fn similar_formatting_selects_matching_runs() {
        let doc = mailcraft_html::html_to_doc(
            "<p>plain <b>bold one</b> plain <b>bold two</b></p><p><b>bold three</b> end</p><h1>head <b>bold</b></h1>",
        );
        let ranges = similar_ranges(&doc, &tp(&[0], 7));
        assert_eq!(ranges.len(), 3, "{ranges:?}");
        assert_eq!(ranges[0], (tp(&[0], 6), tp(&[0], 14)));
        assert_eq!(ranges[2].0.block_path, vec![1]);
        let mut view = BodyView::default();
        assert_eq!(
            select_similar_at(&mut view, &doc, &tp(&[0], 0)),
            3,
            "plain runs: two in the first paragraph, one in the second"
        );
        assert_eq!(view.extra.len(), 2);
        assert!(similar_ranges(&doc, &tp(&[9], 0)).is_empty());
    }

    #[test]
    fn select_all_and_copy_of_extra_ranges() {
        let doc = Doc::from_plain_text("one two\nthree");
        let db = FontDb::bundled();
        let layout = layout_doc(&doc, &db, &LayoutOptions::default());
        let mut view = BodyView { layout: Some(layout), ..Default::default() };
        select_all(&mut view);
        assert_eq!(selected_text_all(&view, &doc).as_deref(), Some("one two\nthree"));
        view.selection = Some((tp(&[1], 0), tp(&[1], 5)));
        view.extra = vec![(tp(&[0], 0), tp(&[0], 3))];
        assert_eq!(selected_text_all(&view, &doc).as_deref(), Some("one\nthree"));
        assert!(range_rect(&view, &(tp(&[0], 0), tp(&[0], 3))).is_none(), "not painted yet");
    }

    #[test]
    fn selection_text_spans_blocks() {
        let doc = Doc::from_plain_text("first line\nsecond line");
        let db = FontDb::bundled();
        let layout = layout_doc(&doc, &db, &LayoutOptions::default());
        let mut view = BodyView { layout: Some(layout), ..Default::default() };
        view.selection =
            Some((TextPos { block_path: vec![0], offset: 6 }, TextPos { block_path: vec![1], offset: 6 }));
        assert_eq!(selected_text(&view, &doc).as_deref(), Some("line\nsecond"));
        view.selection = select_all_range(view.layout.as_ref().unwrap());
        assert_eq!(selected_text(&view, &doc).as_deref(), Some("first line\nsecond line"));
    }

    fn encoded(format: image::ImageFormat, w: u32, h: u32) -> Vec<u8> {
        let img =
            image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 40) as u8, (y * 40) as u8, 200, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).to_rgb8().write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    fn data_url(mime: &str, data: &[u8]) -> String {
        format!("data:{mime};base64,{}", mailcraft_mime::transfer::encode_base64(data))
    }

    #[test]
    fn png_jpeg_and_gif_pictures_decode() {
        for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg, image::ImageFormat::Gif] {
            let img = decode_image(&encoded(format, 5, 3)).unwrap_or_else(|| panic!("{format:?}"));
            assert_eq!(img.size, [5, 3], "{format:?}");
        }
        let pic = mailcraft_model::contact::ContactPicture::sniff(&encoded(image::ImageFormat::Jpeg, 4, 4))
            .unwrap();
        assert_eq!(pic.mime, "image/jpeg");
        assert_eq!(decode_contact_picture(&pic).map(|i| i.size), Some([4, 4]));
        let gif =
            mailcraft_model::contact::ContactPicture::sniff(&encoded(image::ImageFormat::Gif, 2, 6)).unwrap();
        assert_eq!(decode_contact_picture(&gif).map(|i| i.size), Some([2, 6]));
    }

    #[test]
    fn hostile_pictures_are_refused_without_panicking() {
        assert!(decode_image(&[]).is_none());
        assert!(decode_image(b"not a picture at all").is_none());
        // Too wide: refused by the decoder limits.
        assert!(decode_image(&encoded(image::ImageFormat::Png, MAX_IMAGE_SIDE + 1, 1)).is_none());
        // Damaged and truncated data of every format.
        for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg, image::ImageFormat::Gif] {
            let good = encoded(format, 16, 16);
            for cut in [4, 12, 30, good.len() / 2, good.len().saturating_sub(3)] {
                let _ = decode_image(good.get(..cut).unwrap());
            }
            let mut bad = good.clone();
            for b in bad.iter_mut().skip(20).step_by(7) {
                *b ^= 0x5a;
            }
            let _ = decode_image(&bad);
        }
        // A header claiming a huge picture.
        let mut huge = encoded(image::ImageFormat::Png, 1, 1);
        huge.splice(16..24, [0, 1, 0, 0, 0, 1, 0, 0]);
        assert!(decode_image(&huge).is_none());
        // Other formats are not decoded.
        assert!(decode_image(b"BM\x3a\0\0\0\0\0\0\0\x36\0\0\0").is_none());
    }

    #[test]
    fn data_urls_decode_to_bytes() {
        let png = encoded(image::ImageFormat::Png, 2, 2);
        assert_eq!(data_image_bytes(&data_url("image/png", &png)), Some(png.clone()));
        assert_eq!(
            data_image_bytes(&format!(
                "  DATA:Image/PNG;BASE64,{}",
                mailcraft_mime::transfer::encode_base64_flat(&png)
            )),
            Some(png)
        );
        assert!(data_image_bytes("data:text/plain;base64,QUJD").is_none());
        assert!(data_image_bytes("data:image/png,rawtext").is_none());
        assert!(data_image_bytes("data:image/png;base64,").is_none());
        assert!(data_image_bytes("cid:logo@x").is_none());
        assert!(data_image_bytes("https://x.example/a.png").is_none());
        assert!(data_image_bytes("data:").is_none());
    }

    #[test]
    fn body_pictures_become_textures() {
        let ctx = egui::Context::default();
        let jpeg = encoded(image::ImageFormat::Jpeg, 6, 4);
        let tex = image_texture(&ctx, &data_url("image/jpeg", &jpeg)).unwrap();
        assert_eq!(tex.size(), [6, 4]);
        // Junk data and unknown sources draw the placeholder.
        assert!(image_texture(&ctx, "data:image/png;base64,AAAA").is_none());
        assert!(image_texture(&ctx, "cid:logo@x").is_none());
        assert!(image_texture(&ctx, "https://x.example/a.png").is_none());
        // An inline attachment handed over by the owner is drawn; new bytes for the same source replace it.
        assert!(provide_image(&ctx, "cid:logo@x", &encoded(image::ImageFormat::Gif, 3, 3)));
        assert_eq!(image_texture(&ctx, "cid:logo@x").map(|t| t.size()), Some([3, 3]));
        assert!(provide_image(&ctx, "cid:logo@x", &encoded(image::ImageFormat::Png, 8, 2)));
        assert_eq!(image_texture(&ctx, "cid:logo@x").map(|t| t.size()), Some([8, 2]));
        assert!(!provide_image(&ctx, "cid:bad@x", b"junk"));
        assert!(image_texture(&ctx, "cid:bad@x").is_none());
        // The cache stays bounded.
        let png = encoded(image::ImageFormat::Png, 1, 1);
        for i in 0..(MAX_IMAGE_CACHE + 10) {
            provide_image(&ctx, &format!("cid:{i}@x"), &png);
        }
        let n = ctx.data_mut(|d| d.get_temp_mut_or_default::<ImageCache>(image_cache_id()).entries.len());
        assert_eq!(n, MAX_IMAGE_CACHE);
    }

    #[test]
    fn body_with_a_data_picture_paints_it() {
        let png = encoded(image::ImageFormat::Png, 4, 4);
        let doc = mailcraft_html::html_to_doc(&format!(
            "<p>Logo <img src=\"{}\" width=40 height=40 alt=\"Logo\"></p>",
            data_url("image/png", &png)
        ));
        let ctx = egui::Context::default();
        let mut view = BodyView::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            show_doc(ui, &mut view, &doc, false);
        });
        out.textures_delta.clear();
        let textured = out.shapes.iter().any(|s| match &s.shape {
            egui::Shape::Mesh(m) => m.texture_id != egui::TextureId::default(),
            _ => false,
        });
        assert!(textured, "the picture is drawn from its texture");
    }
    #[test]
    fn newsletter_images_reflow_when_intrinsic_sizes_arrive() {
        let doc =
            mailcraft_html::html_to_doc("<p><img src='cid:photo' width='100%' style='height:auto'></p>");
        let ctx = egui::Context::default();
        let mut view = BodyView::default();
        let rect = |view: &BodyView| {
            view.layout
                .as_ref()
                .unwrap()
                .display
                .items
                .iter()
                .find_map(|item| match item {
                    DisplayItem::Image { rect, .. } => Some((rect.w, rect.h)),
                    _ => None,
                })
                .unwrap()
        };
        let mut output = ctx.run_ui(Default::default(), |ui| {
            show_doc(ui, &mut view, &doc, false);
        });
        output.textures_delta.clear();
        assert_eq!(rect(&view), (16.0, 16.0));
        let original_doc_key = view.doc_key;
        assert!(provide_image(&ctx, "cid:photo", &encoded(image::ImageFormat::Png, 558, 279)));
        let mut output = ctx.run_ui(Default::default(), |ui| {
            show_doc(ui, &mut view, &doc, false);
        });
        output.textures_delta.clear();
        assert_eq!(rect(&view), (558.0, 279.0));
        assert_eq!(view.doc_key, original_doc_key);
    }

    #[test]
    fn retina_newsletter_image_keeps_authored_width_and_auto_height() {
        let doc = mailcraft_html::html_to_doc(
            "<p><img src='cid:photo' width='558' style='width:100%;height:auto'></p>",
        );
        let ctx = egui::Context::default();
        assert!(provide_image(&ctx, "cid:photo", &encoded(image::ImageFormat::Png, 1116, 558)));
        let mut view = BodyView::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            show_doc(ui, &mut view, &doc, false);
        });
        output.textures_delta.clear();
        let dimensions = view
            .layout
            .as_ref()
            .unwrap()
            .display
            .items
            .iter()
            .find_map(|item| match item {
                DisplayItem::Image { rect, .. } => Some((rect.w, rect.h)),
                _ => None,
            })
            .unwrap();
        assert_eq!(dimensions, (558.0, 279.0));
    }
}
