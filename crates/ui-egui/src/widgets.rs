//! Shared widgets (shell area owns; others call). Signatures frozen for commit 4.
//!
//! All buttons are flat (no frame until hovered), draw their icon with [`crate::icons::Icons::paint`] tinted
//! with the current text colour, and report an accessible label (the visible label, or the tooltip for
//! icon-only buttons) so tests and screen readers can find them.

use std::sync::Arc;

use egui::{Color32, Galley, Rect, Response, Sense, Ui, WidgetInfo, WidgetType, vec2};

use crate::App;

/// Small icon size (toolbar, simplified ribbon, menus).
pub const ICON: f32 = 16.0;
/// Large ribbon icon size.
pub const BIG_ICON: f32 = 32.0;
/// Horizontal padding inside flat buttons.
const PAD: f32 = 6.0;
/// Gap between icon and label.
const GAP: f32 = 5.0;
/// Widest label of a large button before it wraps.
const BIG_WRAP: f32 = 76.0;

/// Text colour for a widget in the current ui (disabled = faded).
fn fg(ui: &Ui, enabled: bool) -> Color32 {
    let c = ui.visuals().text_color();
    if enabled { c } else { c.gamma_multiply(0.45) }
}

fn label_galley(ui: &Ui, label: &str, color: Color32, wrap: Option<f32>) -> Option<Arc<Galley>> {
    if label.is_empty() {
        return None;
    }
    let font = egui::TextStyle::Button.resolve(ui.style());
    let painter = ui.painter();
    Some(match wrap {
        Some(w) => painter.layout(label.to_owned(), font, color, w),
        None => painter.layout_no_wrap(label.to_owned(), font, color),
    })
}

/// Hover / pressed / on background of a flat button.
fn paint_flat_bg(ui: &Ui, rect: Rect, resp: &Response, on: bool, enabled: bool) {
    let v = ui.visuals();
    let fill = if on {
        Some(v.selection.bg_fill)
    } else if enabled && resp.is_pointer_button_down_on() {
        Some(v.widgets.active.weak_bg_fill)
    } else if enabled && resp.hovered() {
        Some(v.widgets.hovered.weak_bg_fill)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(4), fill);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0, v.selection.stroke.color),
            egui::StrokeKind::Inside,
        );
    }
}

fn sense(enabled: bool) -> Sense {
    if enabled { Sense::click() } else { Sense::hover() }
}

/// Paints icon `name` centred in `rect` (square of `size`).
pub fn paint_icon(ui: &Ui, app: &mut App, center: egui::Pos2, size: f32, name: &str, tint: Color32) {
    let rect = Rect::from_center_size(center, vec2(size, size));
    app.icons.paint(ui, rect, name, tint);
}

fn flat(
    ui: &mut Ui,
    app: &mut App,
    icon: &str,
    label: &str,
    enabled: bool,
    on: bool,
    chevron: bool,
) -> Response {
    let color = fg(ui, enabled);
    let galley = label_galley(ui, label, color, None);
    let text_w = galley.as_ref().map(|g| g.size().x);
    let h = ui.spacing().interact_size.y.max(24.0);
    let w = flat_width(icon, text_w, chevron);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), sense(enabled));
    let info_label = label.to_owned();
    resp.widget_info(|| {
        if on {
            WidgetInfo::selected(WidgetType::Button, enabled, true, info_label.clone())
        } else {
            WidgetInfo::labeled(WidgetType::Button, enabled, info_label.clone())
        }
    });
    if ui.is_rect_visible(rect) {
        paint_flat_bg(ui, rect, &resp, on, enabled);
        let mut x = rect.left() + PAD;
        if !icon.is_empty() {
            paint_icon(ui, app, egui::pos2(x + ICON / 2.0, rect.center().y), ICON, icon, color);
            x += ICON + GAP;
        }
        if let Some(g) = galley {
            let pos = egui::pos2(x, rect.center().y - g.size().y / 2.0);
            ui.painter().galley(pos, g, color);
        }
        if chevron {
            paint_icon(
                ui,
                app,
                egui::pos2(rect.right() - PAD - 4.0, rect.center().y),
                10.0,
                "chevron-down",
                color,
            );
        }
    }
    resp
}

/// Width of a flat button whose label is `text_w` wide (`None` = no label).
fn flat_width(icon: &str, text_w: Option<f32>, chevron: bool) -> f32 {
    let mut w = PAD + if icon.is_empty() { 0.0 } else { ICON } + PAD;
    if let Some(t) = text_w {
        w += if icon.is_empty() { t } else { GAP + t };
    }
    if chevron {
        w += 12.0;
    }
    w
}

/// The width [`icon_button`] / [`toggle_button`] (or [`dropdown_button`] with `chevron`) takes for `label`,
/// so a row can be fitted before it is drawn.
pub fn button_width(ui: &Ui, icon: &str, label: &str, chevron: bool) -> f32 {
    let text_w = label_galley(ui, label, Color32::WHITE, None).map(|g| g.size().x);
    flat_width(icon, text_w, chevron)
}

/// Side of a square icon-only toggle in a toolbar row ([`icon_toggle`]).
pub fn icon_side(ui: &Ui) -> f32 {
    ui.spacing().interact_size.y.max(24.0)
}

/// Flat icon+label button as used in the ribbon and toolbars. `icon` is an `assets/icons` name.
pub fn icon_button(ui: &mut Ui, app: &mut App, icon: &str, label: &str, enabled: bool) -> Response {
    flat(ui, app, icon, label, enabled, false, false)
}

/// Large ribbon button (icon over label).
pub fn big_button(ui: &mut Ui, app: &mut App, icon: &str, label: &str, enabled: bool) -> Response {
    let color = fg(ui, enabled);
    let galley = label_galley(ui, label, color, Some(BIG_WRAP));
    let (tw, th) = galley.as_ref().map_or((0.0, 0.0), |g| (g.size().x, g.size().y));
    let w = (tw + 2.0 * PAD).max(BIG_ICON + 2.0 * PAD);
    let h = 4.0 + BIG_ICON + 4.0 + th.max(14.0) + 4.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), sense(enabled));
    let info_label = label.to_owned();
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, info_label.clone()));
    if ui.is_rect_visible(rect) {
        paint_flat_bg(ui, rect, &resp, false, enabled);
        paint_icon(
            ui,
            app,
            egui::pos2(rect.center().x, rect.top() + 4.0 + BIG_ICON / 2.0),
            BIG_ICON,
            icon,
            color,
        );
        if let Some(g) = galley {
            let pos = egui::pos2(rect.center().x - g.size().x / 2.0, rect.top() + 8.0 + BIG_ICON);
            ui.painter().galley(pos, g, color);
        }
    }
    resp
}

/// Icon-only button with a tooltip.
pub fn icon_only(ui: &mut Ui, app: &mut App, icon: &str, tooltip: &str, enabled: bool) -> Response {
    let color = fg(ui, enabled);
    let side = ui.spacing().interact_size.y.max(24.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(side + 4.0, side), sense(enabled));
    let info_label = tooltip.to_owned();
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, info_label.clone()));
    if ui.is_rect_visible(rect) {
        paint_flat_bg(ui, rect, &resp, false, enabled);
        paint_icon(ui, app, rect.center(), ICON, icon, color);
    }
    if tooltip.is_empty() { resp } else { resp.on_hover_text(tooltip) }
}

/// Toggle (check) button; returns the response, `on` shows the pressed state.
pub fn toggle_button(ui: &mut Ui, app: &mut App, icon: &str, label: &str, on: bool) -> Response {
    flat(ui, app, icon, label, true, on, false)
}

/// Flat icon+label button with a ▾ chevron (opens a menu; pair with [`egui::Popup::menu`]).
pub fn dropdown_button(ui: &mut Ui, app: &mut App, icon: &str, label: &str, enabled: bool) -> Response {
    flat(ui, app, icon, label, enabled, false, true)
}

/// Icon-only toggle with a tooltip (rail items, status bar view buttons).
pub fn icon_toggle(ui: &mut Ui, app: &mut App, icon: &str, tooltip: &str, on: bool, side: f32) -> Response {
    let color = fg(ui, true);
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    let info_label = tooltip.to_owned();
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, on, info_label.clone()));
    if ui.is_rect_visible(rect) {
        paint_flat_bg(ui, rect, &resp, on, true);
        let icon_size = (side * 0.6).clamp(ICON - 4.0, 20.0);
        paint_icon(ui, app, rect.center(), icon_size, icon, color);
    }
    resp.on_hover_text(tooltip)
}

/// Room an embedded item window keeps around itself; a window whose default size does not leave it fills the
/// screen instead.
pub const EMBED_FILL_MARGIN: f32 = 64.0;
/// Height of the title strip of an embedded item window that fills the screen.
pub const EMBED_TITLE_H: f32 = 32.0;

/// Whether an embedded item window of `default` size fills a `screen` of this size ([`item_window`]).
pub fn embedded_fills(default: egui::Vec2, screen: egui::Vec2) -> bool {
    default.x + EMBED_FILL_MARGIN > screen.x || default.y + EMBED_FILL_MARGIN > screen.y
}

/// Number of open compose windows (inline replies are not windows).
pub fn compose_windows_open(app: &App) -> usize {
    app.s.composes.values().filter(|c| !c.inline).count()
}

/// Number of open compose and read windows (the item windows [`item_window`] draws).
pub fn item_windows_open(app: &App) -> usize {
    compose_windows_open(app).saturating_add(app.ui.read_windows.len())
}

/// The layer of an embedded item window that covers the screen ([`item_window`]).
pub fn item_cover_layer(id: egui::ViewportId) -> egui::LayerId {
    egui::LayerId::new(egui::Order::Background, egui::Id::new(("mc-item-cover", id)))
}

/// The layer of an embedded item window drawn as a floating window ([`item_window`]).
pub fn item_float_layer(id: egui::ViewportId) -> egui::LayerId {
    egui::LayerId::new(egui::Order::Middle, egui::Id::new(id))
}

/// Brings item window `id` to the front: its OS window where it has one, else its embedded layer (an
/// embedded window has no OS window for [`egui::ViewportCommand::Focus`] to act on).
pub fn focus_item_window(ctx: &egui::Context, id: egui::ViewportId) {
    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
    if ctx.embed_viewports() {
        for layer in [item_cover_layer(id), item_float_layer(id)] {
            if ctx.memory(|m| m.areas().visible_last_frame(&layer)) {
                ctx.move_to_top(layer);
            }
        }
        ctx.request_repaint();
    }
}

/// Where an item window sits among the open compose and read windows ([`item_window`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItemSlot {
    /// Its place among them (staggers the floating windows so each one's title bar shows).
    pub index: usize,
    /// How many are open ([`item_windows_open`]).
    pub open: usize,
}

/// Step between the top-left corners of successive floating item windows.
const ITEM_CASCADE: f32 = 28.0;
/// Part of a floating item window that stays on screen however far it is dragged aside.
const ITEM_KEEP: egui::Vec2 = egui::vec2(120.0, 40.0);

/// Shows an item window (compose, read message): a real OS window where the platform has them, else embedded
/// in the main window. An embedded window that would nearly cover the screen covers it exactly, under a title
/// strip, while it is the only item window open: a slightly smaller floating window left slivers of the main
/// window's text showing at its edges. With other item windows open each one floats, staggered, sized to
/// leave room, and can be dragged mostly off the screen or brought forward with a click, so none hides
/// another for good. The cover sits under every floating window (the dialogs and windows it opens, such as
/// Reminders, stay in front of it); it covers the main window only.
pub fn item_window<T: Default>(
    ctx: &egui::Context,
    id: egui::ViewportId,
    builder: egui::ViewportBuilder,
    default: egui::Vec2,
    theme: &crate::theme::Theme,
    slot: ItemSlot,
    mut ui_cb: impl FnMut(&mut Ui, egui::ViewportClass) -> T,
) -> T {
    let screen = ctx.content_rect();
    if !ctx.embed_viewports() {
        // A new OS window stays hidden until it has painted its first frame, then shows: shown as it was
        // created, it stood blank (white on Windows) until then. eframe does the same for the main window.
        let created = ctx.input(|i| i.raw.viewports.contains_key(&id));
        return ctx.show_viewport_immediate(id, builder.with_visible(created), |ui, class| {
            let out = ui_cb(ui, class);
            if !created {
                // Applied right after this frame is on the window.
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Visible(true));
            }
            out
        });
    }
    let fills = embedded_fills(default, screen.size());
    let float = item_float_layer(id);
    let shown = |layer: egui::LayerId| ctx.memory(|m| m.areas().visible_last_frame(&layer));
    let was_float = shown(float);
    let was_cover = shown(item_cover_layer(id));
    let newest_key = egui::Id::new("mc-item-window-newest");
    let raise_key = egui::Id::new("mc-item-window-raise-newest");
    if !was_float && !was_cover {
        // Just opened: it comes to the front, above a window that starts floating in the same frame.
        ctx.data_mut(|d| d.insert_temp(newest_key, id));
    }
    if slot.open > 1 || !fills {
        // A floating window (what `show_viewport_immediate` embeds, without its panics should the window
        // not show). One that would fill the screen leaves room for the others.
        let size = if fills {
            (screen.size() - vec2(2.0, 2.0) * EMBED_FILL_MARGIN).max(vec2(200.0, 150.0)).min(default)
        } else {
            default
        };
        let step = (slot.index % 6) as f32 * ITEM_CASCADE;
        let outer = ctx.memory(|m| m.area_rect(item_float_layer(id).id)).map_or(size, |r| r.size());
        let spare = (outer - ITEM_KEEP).max(egui::Vec2::ZERO);
        let bounds = Rect::from_min_max(
            egui::pos2(screen.min.x - spare.x, screen.min.y),
            egui::pos2(screen.max.x + spare.x, screen.max.y + spare.y),
        );
        let out = egui::Window::from_viewport(id, builder.with_inner_size(size))
            .collapsible(false)
            .default_pos(screen.min + vec2(16.0 + step, 16.0 + step))
            .constrain_to(bounds)
            .show(ctx, |ui| ui_cb(ui, egui::ViewportClass::EmbeddedWindow))
            .and_then(|r| r.inner)
            .unwrap_or_default();
        // A window that appears in egui is raised over the others, all in the order they were drawn, so a
        // window that had covered the screen and now floats (another one just opened) could land on top of
        // the new one. Then the new one is raised once more in a second pass, before anything shows.
        let pass = ctx.cumulative_pass_nr();
        if was_cover {
            ctx.data_mut(|d| d.insert_temp(raise_key, pass));
            ctx.request_discard("a new item window comes to the front");
        }
        let flagged = ctx.data(|d| d.get_temp::<u64>(raise_key));
        if was_float
            && flagged.is_some_and(|p| p.saturating_add(1) == pass)
            && ctx.data(|d| d.get_temp::<egui::ViewportId>(newest_key)) == Some(id)
        {
            ctx.move_to_top(float);
        }
        return out;
    }
    let title = builder.title.unwrap_or_default();
    let layer = item_cover_layer(id);
    egui::Area::new(layer.id)
        .order(layer.order)
        .fixed_pos(screen.min)
        .constrain(false)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            ui.set_max_size(screen.size());
            // Swallow clicks on the strip and any gaps so nothing below reacts.
            let _ = ui.interact(screen, egui::Id::new(("mc-item-window-blocker", id)), Sense::click());
            ui.painter().rect_filled(screen, 0.0, theme.pane);
            let strip = Rect::from_min_size(screen.min, vec2(screen.width(), EMBED_TITLE_H));
            ui.painter().rect_filled(strip, 0.0, theme.ribbon);
            ui.painter().hline(strip.x_range(), strip.bottom(), egui::Stroke::new(1.0, theme.separator));
            ui.put(
                strip.shrink2(vec2(12.0, 0.0)),
                egui::Label::new(egui::RichText::new(title).size(14.0).color(theme.text)).truncate(),
            );
            let body = Rect::from_min_max(egui::pos2(screen.min.x, strip.bottom() + 1.0), screen.max);
            let mut child =
                ui.new_child(egui::UiBuilder::new().id_salt(("mc-item-window", id)).max_rect(body));
            ui_cb(&mut child, egui::ViewportClass::EmbeddedWindow)
        })
        .inner
}

/// A rounded content pane frame (folder pane, message list, reading pane, module views).
pub fn pane_frame(theme: &crate::theme::Theme, fill: Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(crate::theme::size::PANE_RADIUS))
        .stroke(egui::Stroke::new(if theme.dark { 0.0 } else { 1.0 }, theme.separator))
}

/// Width of one end of a [`stop_slider`] track (room for the thumb at the first and last stop).
const STOP_PAD: f32 = 12.0;

/// Index of `i` as a coordinate factor (stop lists are short; a longer one saturates).
fn stop_f32(i: usize) -> f32 {
    f32::from(u16::try_from(i).unwrap_or(u16::MAX))
}

/// A framed horizontal slider over a fixed list of evenly spaced stops; `labels` name the stops and `name` is
/// its accessible name. The pointer picks the nearest stop (click or drag). Keyboard: Left/Down and Right/Up
/// move one stop, Page Down/Page Up three, Home and End go to the ends. Screen readers get a slider whose
/// number is the stop's index (0 to `labels.len() - 1`) and whose value text is the stop's label, and which
/// takes their Increment, Decrement and Set Value actions. Returns the response, changed when the stop moved.
pub fn stop_slider(ui: &mut Ui, name: &str, index: &mut usize, labels: &[&str], width: f32) -> Response {
    use egui::{Key, accesskit::Action};
    let last = labels.len().saturating_sub(1);
    let old = (*index).min(last);
    let mut idx = old;
    let (rect, mut response) =
        ui.allocate_exact_size(vec2(width.max(4.0 * STOP_PAD), 28.0), Sense::click_and_drag());
    let enabled = ui.is_enabled();
    let left = rect.left() + STOP_PAD;
    let span = (rect.width() - 2.0 * STOP_PAD).max(1.0);
    let x_of = |i: usize| if last == 0 { left } else { left + span * stop_f32(i) / stop_f32(last) };
    if enabled {
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        if (response.clicked() || response.dragged())
            && let Some(pos) = response.interact_pointer_pos()
        {
            idx = (0..=last)
                .min_by(|a, b| (x_of(*a) - pos.x).abs().total_cmp(&(x_of(*b) - pos.x).abs()))
                .unwrap_or(idx);
        }
        let mut step = 0i64;
        if response.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                );
            });
            let presses = |k: Key| ui.input(|i| i64::try_from(i.num_presses(k)).unwrap_or(0));
            step += presses(Key::ArrowRight) + presses(Key::ArrowUp)
                - presses(Key::ArrowLeft)
                - presses(Key::ArrowDown);
            step += 3 * (presses(Key::PageUp) - presses(Key::PageDown));
            if presses(Key::Home) > 0 {
                idx = 0;
            }
            if presses(Key::End) > 0 {
                idx = last;
            }
        }
        ui.input(|i| {
            step +=
                i64::try_from(i.num_accesskit_action_requests(response.id, Action::Increment)).unwrap_or(0);
            step -=
                i64::try_from(i.num_accesskit_action_requests(response.id, Action::Decrement)).unwrap_or(0);
            for r in i.accesskit_action_requests(response.id, Action::SetValue) {
                if let Some(egui::accesskit::ActionData::NumericValue(v)) = r.data
                    && v.is_finite()
                {
                    // The nearest stop to the number asked for.
                    let v = v.clamp(0.0, f64::from(stop_f32(last)));
                    let off = |i: usize| (f64::from(stop_f32(i)) - v).abs();
                    idx = (0..=last).min_by(|a, b| off(*a).total_cmp(&off(*b))).unwrap_or(idx);
                }
            }
        });
        if step != 0 {
            let to = i64::try_from(idx)
                .unwrap_or(0)
                .saturating_add(step)
                .clamp(0, i64::try_from(last).unwrap_or(0));
            idx = usize::try_from(to).unwrap_or(idx);
        }
    }
    let idx = idx.min(last);
    if idx != old {
        response.mark_changed();
    }
    *index = idx;
    if ui.is_rect_visible(rect) {
        let v = ui.visuals();
        let w = ui.style().interact(&response);
        let painter = ui.painter();
        let frame =
            if response.has_focus() { v.selection.stroke } else { v.widgets.noninteractive.bg_stroke };
        painter.rect(rect, 3.0, v.extreme_bg_color, frame, egui::StrokeKind::Inside);
        let y = rect.center().y;
        let ink = if enabled { v.text_color() } else { v.weak_text_color() };
        painter.line_segment([egui::pos2(left, y), egui::pos2(left + span, y)], egui::Stroke::new(2.0, ink));
        for i in 0..=last {
            let x = x_of(i);
            painter
                .line_segment([egui::pos2(x, y + 4.0), egui::pos2(x, y + 8.0)], egui::Stroke::new(1.0, ink));
        }
        let thumb = Rect::from_center_size(egui::pos2(x_of(idx), y), vec2(9.0, 18.0));
        let fill = if enabled { w.bg_fill } else { v.widgets.inactive.weak_bg_fill };
        painter.rect(thumb, 2.0, fill, egui::Stroke::new(1.0, ink), egui::StrokeKind::Inside);
    }
    let value = labels.get(idx).copied().unwrap_or_default().to_owned();
    response.widget_info(|| {
        let mut info = WidgetInfo::slider(enabled, stop_f32(idx).into(), name);
        info.current_text_value = Some(value.clone());
        info
    });
    ui.ctx().accesskit_node_builder(response.id, |b| {
        b.set_min_numeric_value(0.0);
        b.set_max_numeric_value(stop_f32(last).into());
        b.set_numeric_value_step(1.0);
        if enabled {
            b.add_action(Action::SetValue);
            if idx < last {
                b.add_action(Action::Increment);
            }
            if idx > 0 {
                b.add_action(Action::Decrement);
            }
        }
    });
    response
}
