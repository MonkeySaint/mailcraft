//! Reading pane and read windows (reading area).
//!
//! Layout per plan/outlook/reading-pane.md and read-window.md: subject (large); persona circle with initials
//! (stable colour per sender) next to "Name <address>", To and Cc lines; on the right an optional
//! background toggle (dark mode only), the Reply | Reply All | Forward segmented control (icon-only when
//! narrow), a "…" menu and the sent date (short form when narrow); info bars; attachment chips; the
//! "pictures not downloaded" bar; then the body (HTML through mailcraft-html, plain text as paragraphs).
//! When an inline response exists for the selected item the compose area's inline editor replaces the view.
//!
//! Local UI state (not in the Session): which items show the light background in dark mode, which items may
//! load remote pictures, and per-surface parsed-body caches (egui temp data).

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
    let Some(id) = focus else {
        empty_state(ui, app);
        return;
    };
    if let Some(cid) = inline_compose_for(app, id) {
        crate::compose::inline_compose(ui, app, cid);
        return;
    }
    let narrow = frame.width <= NARROW_W && frame.width > 0.0 || ui.available_width() < NARROW_PANE_W;
    message_view(ui, app, id, Surface::Main, narrow);
}

/// Opened-message windows (secondary viewports) listed in `app.ui.read_windows`.
pub fn read_windows(ctx: &egui::Context, app: &mut App) {
    let ids = app.ui.read_windows.clone();
    let mut closed: Vec<u64> = Vec::new();
    for id in ids {
        let Some(m) = app.s.mailbox.mail.get(&ItemId(id)) else {
            closed.push(id);
            continue;
        };
        let title = window_title(&m.subject, m.body.format);
        let builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([960.0, 760.0])
            .with_min_inner_size([300.0, 200.0]);
        let vid = egui::ViewportId::from_hash_of(("mailcraft.read-window", id));
        let close = ctx.show_viewport_immediate(vid, builder, |ui, class| read_window_ui(ui, app, id, class));
        if close {
            closed.push(id);
        }
    }
    if !closed.is_empty() {
        app.ui.read_windows.retain(|i| !closed.contains(i));
        for id in closed {
            let key = cache_id(Surface::Read(id));
            ctx.data_mut(|d| d.remove::<MsgCache>(key));
        }
    }
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

fn cache_id(surface: Surface) -> Id {
    Id::new(("mailcraft.reading.cache", surface))
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

fn body_doc(b: &mailcraft_model::Body) -> Doc {
    match (&b.html, &b.text, b.format) {
        (Some(html), _, BodyFormat::Html | BodyFormat::RichText) => mailcraft_html::html_to_doc(html),
        (_, Some(text), _) => Doc::from_plain_text(text),
        (Some(html), None, _) => mailcraft_html::html_to_doc(html),
        (None, None, _) => Doc::default(),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Pieces

fn focused_mail(app: &App) -> Option<u64> {
    let sel = &app.s.selection;
    sel.focus
        .into_iter()
        .chain(sel.items.iter().copied())
        .find(|id| app.s.mailbox.mail.contains_key(id))
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
    let Some(h) = queries::reading_header(&app.s, ItemId(id)) else {
        empty_state(ui, app);
        return;
    };
    let ctx = ui.ctx().clone();
    let key = cache_id(surface);
    let mut cache: MsgCache = ctx.data_mut(|d| d.remove_temp::<MsgCache>(key)).unwrap_or_default();
    if let Some(m) = app.s.mailbox.mail.get(&ItemId(id)) {
        let sk = source_key(id, &m.body);
        if cache.item != id || cache.source_key != sk {
            let doc = body_doc(&m.body);
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
    let dark = app.theme.dark && !tg.light_bg.contains(&id);
    let allow_remote = tg.pictures.contains(&id);
    let zoom = match app.s.view.zoom {
        0 => 1.0,
        z => f32::from(z) / 100.0,
    };
    let in_window = matches!(surface, Surface::Read(_));

    let mut clicked_link = None;
    egui::ScrollArea::vertical()
        .id_salt(("mailcraft.reading.scroll", surface, id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(12.0);
            egui::Frame::new().inner_margin(egui::Margin::symmetric(PAD as i8, 0)).show(ui, |ui| {
                header(ui, app, &h, id, narrow, in_window);
                if cache.remote_images && !allow_remote {
                    pictures_bar(ui, app, id);
                }
            });
            ui.add_space(10.0);
            egui::Frame::new().inner_margin(egui::Margin::symmetric(PAD as i8, 0)).show(ui, |ui| {
                let opts = BodyOptions {
                    dark,
                    zoom,
                    allow_remote,
                    paint_background: true,
                    doc_key: Some(cache.doc_key),
                };
                let doc = Arc::clone(&cache.doc);
                clicked_link = body::show_doc_with(ui, &mut cache.view, &doc, &opts);
            });
            ui.add_space(PAD);
        });
    ctx.data_mut(|d| d.insert_temp(key, cache));
    if let Some(href) = clicked_link {
        open_link(&ctx, app, &href);
    }
}

fn header(ui: &mut Ui, app: &mut App, h: &ReadingHeader, id: u64, narrow: bool, in_window: bool) {
    let theme = app.theme.clone();
    // 1. Subject.
    let subject = if h.subject.trim().is_empty() { "(no subject)" } else { h.subject.as_str() };
    let subject_size = if in_window { 18.0 } else { 20.0 };
    // Search words in the subject on the highlight background (File › Options › Search).
    let hits = if crate::dialogs::search_highlight_on(ui.ctx()) {
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
    let right_used = right.min_rect();

    let left_w = (full.width() - right_w - 8.0).max(60.0);
    ui.allocate_ui_with_layout(Vec2::new(left_w, 0.0), Layout::left_to_right(Align::Min), |ui| {
        let d = if in_window { 48.0 } else { 40.0 };
        let (r, _) = ui.allocate_exact_size(Vec2::splat(d), Sense::hover());
        let c = r.center();
        ui.painter().circle_filled(c, d / 2.0, persona_color(h.persona_color));
        ui.painter().text(
            c,
            egui::Align2::CENTER_CENTER,
            &h.from_initials,
            FontId::proportional(d * 0.36),
            Color32::WHITE,
        );
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
    // 5. Info bars.
    for line in &h.info {
        ui.add_space(6.0);
        info_bar(ui, app, "info", line, None);
    }
    // 6. Meeting requests, responses, cancellations and proposals (nothing for ordinary mail).
    crate::modules::meeting_bar(ui, app, id);
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
    if let Some(rest) = lower.strip_prefix("mailto:").map(|_| h.get(7..).unwrap_or("")) {
        let addr = rest.split('?').next().unwrap_or("").to_owned();
        if let Some(v) = app.run("mail.newEmail", json!({}))
            && let Some(cid) = v.get("compose").and_then(serde_json::Value::as_u64)
            && !addr.is_empty()
        {
            app.run("compose.setTo", json!({"compose": cid, "value": addr}));
        }
    } else if lower.starts_with("http://") || lower.starts_with("https://") {
        ctx.open_url(egui::OpenUrl::new_tab(h));
    }
}

// ---------------------------------------------------------------------------------------------------------
// Read windows

/// One read window's contents; returns true when it should close.
fn read_window_ui(ui: &mut Ui, app: &mut App, id: u64, class: egui::ViewportClass) -> bool {
    let ctx = ui.ctx().clone();
    let embedded = class == egui::ViewportClass::EmbeddedWindow;
    let mut close = !embedded && ctx.input(|i| i.viewport().close_requested());
    if !embedded {
        // A real window has its own input; embedded ones share the main window's (handled there).
        crate::keys::handle(&ctx, app, Surface::Read(id));
        if !app.ui.read_windows.contains(&id) {
            return true;
        }
    }
    let size = if embedded { ui.available_size() } else { ctx.content_rect().size() };
    let frame = Frame::compute(size.x, size.y);
    let narrow = size.x <= NARROW_W;
    egui::Frame::new().fill(app.theme.reading).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        if embedded {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if plain_icon_button(ui, app, "close", None, "Close").clicked() {
                        close = true;
                    }
                });
            });
        }
        if !(size.x <= 300.0 && size.y <= 200.0) {
            crate::ribbon::ribbon(ui, app, Surface::Read(id), &frame);
            ui.separator();
        }
        message_view(ui, app, id, Surface::Read(id), narrow);
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(&ctx) {
        close = true;
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
        let id = s.mailbox.mail.values().find(|m| m.subject == subject).map(|m| m.id).unwrap();
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
        let m = s.mailbox.mail.values().find(|m| m.body.html.is_some()).unwrap();
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
        let m = s.mailbox.mail.values_mut().find(|m| m.subject == "Quarterly report draft").unwrap();
        m.body.html = None;
        m.body.format = BodyFormat::PlainText;
        m.body.text = Some("Hi team,\nplease see the attached draft.\n".into());
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

    fn app_with_session(mut s: mailcraft_engine::Session, subject: &str) -> App {
        let id = s.mailbox.mail.values().find(|m| m.subject == subject).map(|m| m.id).unwrap();
        s.selection.items = vec![id];
        s.selection.focus = Some(id);
        App::new(s)
    }

    #[test]
    fn read_window_closes_on_escape_when_embedded() {
        let s = sample_session();
        let id = s.mailbox.mail.keys().next().copied().unwrap();
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
}
