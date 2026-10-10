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
    let text_w = galley.as_ref().map_or(0.0, |g| g.size().x);
    let h = ui.spacing().interact_size.y.max(24.0);
    let mut w = PAD + if icon.is_empty() { 0.0 } else { ICON } + PAD;
    if galley.is_some() {
        w += if icon.is_empty() { text_w } else { GAP + text_w };
    }
    if chevron {
        w += 12.0;
    }
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

/// A rounded content pane frame (folder pane, message list, reading pane, module views).
pub fn pane_frame(theme: &crate::theme::Theme, fill: Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(crate::theme::size::PANE_RADIUS))
        .stroke(egui::Stroke::new(if theme.dark { 0.0 } else { 1.0 }, theme.separator))
}
