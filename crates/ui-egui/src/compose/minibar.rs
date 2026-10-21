//! The Mini Toolbar (File › Options › General › "Show Mini Toolbar on selection"): a small bar of the most used
//! text formatting commands that appears just above text selected with the mouse (a drag, a double or triple
//! click). It goes away when the selection changes or collapses, when you type, or when the pointer moves
//! well away from it; each button runs the same command as the ribbon (`format.bold`, `format.growFont`…)
//! on the compose selection.
//!
//! State (where it shows, for which selection) lives in egui temp memory per compose. The body editor calls
//! [`track`] after its own interaction and [`show`] after painting.

use egui::{Align2, CornerRadius, Id, Order, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use serde_json::json;

use mailcraft_engine::compose::BodySelection;

use super::editor::EditorHost;

/// Accessible name of the bar.
pub const LABEL: &str = "Mini Toolbar";
/// Buttons: (icon, accessible name / tooltip, command).
pub const BUTTONS: [(&str, &str, &str); 10] = [
    ("zoom-in", "Grow Font", "format.growFont"),
    ("zoom-out", "Shrink Font", "format.shrinkFont"),
    ("bold", "Bold", "format.bold"),
    ("italic", "Italic", "format.italic"),
    ("underline", "Underline", "format.underline"),
    ("highlight", "Text Highlight Color", "format.highlight"),
    ("font-color", "Font Color", "format.color"),
    ("bullets", "Bullets", "format.bullets"),
    ("numbering", "Numbering", "format.numbering"),
    ("format-painter", "Format Painter", "format.formatPainter"),
];
/// Button side (points).
const BTN: f32 = 26.0;
/// Gap between the bar and the selection.
const ABOVE: f32 = 8.0;
/// The bar goes away once the pointer is this far from it.
const FADE_DISTANCE: f32 = 140.0;

/// Where the bar shows and for which selection.
#[derive(Clone, Debug, Default)]
struct Bar {
    /// Bottom-left corner of the bar (screen points).
    at: Option<Pos2>,
    selection: BodySelection,
}

fn id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-minibar", cid))
}

fn load(ctx: &egui::Context, cid: u64) -> Bar {
    ctx.data(|d| d.get_temp::<Bar>(id(cid))).unwrap_or_default()
}

fn store(ctx: &egui::Context, cid: u64, bar: Bar) {
    ctx.data_mut(|d| d.insert_temp(id(cid), bar));
}

/// Whether the Mini Toolbar of compose `cid` is showing.
pub fn is_showing(ctx: &egui::Context, cid: u64) -> bool {
    load(ctx, cid).at.is_some()
}

/// The size of the bar.
fn bar_size() -> Vec2 {
    vec2(BTN * BUTTONS.len() as f32 + 8.0, BTN + 8.0)
}

/// Updates the bar after the editor's interaction this frame: `selected_at` is where a mouse selection was
/// just completed (pointer released after a drag, a double or triple click), `typed` whether the keyboard
/// edited or moved this frame.
pub(super) fn track(
    ctx: &egui::Context,
    host: &dyn EditorHost,
    cid: u64,
    selected_at: Option<Pos2>,
    typed: bool,
) {
    let Some(c) = host.compose(cid) else {
        return;
    };
    let mut bar = load(ctx, cid);
    if !host.mini_toolbar() || c.selection.is_collapsed() || typed {
        if bar.at.is_some() {
            store(ctx, cid, Bar::default());
        }
        return;
    }
    if let Some(p) = selected_at {
        bar = Bar { at: Some(pos2(p.x - BTN, p.y - ABOVE)), selection: c.selection.clone() };
        store(ctx, cid, bar);
        return;
    }
    // Another selection (Shift+arrows, a command) than the one it was shown for.
    if bar.at.is_some() && bar.selection != c.selection {
        store(ctx, cid, Bar::default());
    }
}

/// Paints the bar (when showing) and runs what its buttons ask for.
pub(super) fn show(ctx: &egui::Context, host: &mut dyn EditorHost, cid: u64) {
    let bar = load(ctx, cid);
    let Some(at) = bar.at else {
        return;
    };
    let size = bar_size();
    let screen = ctx.content_rect();
    // Keep it on screen: above the selection, or below the pointer when there is no room above.
    let mut min = pos2(at.x, at.y - size.y);
    if min.y < screen.top() {
        min.y = at.y + ABOVE * 3.0;
    }
    min.x = min.x.clamp(screen.left(), (screen.right() - size.x).max(screen.left()));
    let rect = Rect::from_min_size(min, size);
    // Fades out as the pointer leaves it.
    if let Some(p) = ctx.pointer_hover_pos()
        && rect.distance_to_pos(p) > FADE_DISTANCE
    {
        store(ctx, cid, Bar::default());
        return;
    }
    let (fill, stroke, ink, hover) = {
        let t = host.theme();
        (t.pane, t.separator, t.text, t.hover)
    };
    let mut pick: Option<&'static str> = None;
    egui::Area::new(id(cid).with("area")).order(Order::Foreground).fixed_pos(min).show(ctx, |ui| {
        let (frame, _) = ui.allocate_exact_size(size, Sense::hover());
        let group = ui.interact(frame, id(cid).with("group"), Sense::hover());
        group.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, LABEL));
        ui.painter().rect_filled(frame, CornerRadius::same(4), fill);
        ui.painter().rect_stroke(frame, CornerRadius::same(4), Stroke::new(1.0, stroke), StrokeKind::Inside);
        for (i, (icon, label, cmd)) in BUTTONS.iter().enumerate() {
            let r = Rect::from_min_size(frame.min + vec2(4.0 + BTN * i as f32, 4.0), Vec2::splat(BTN));
            let resp = ui.interact(r, id(cid).with(("btn", i)), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, *label));
            if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(3), hover);
            }
            if crate::icons::Icons::has(icon) {
                crate::icons::Icons::image(icon, 16.0, ink)
                    .paint_at(ui, Rect::from_center_size(r.center(), Vec2::splat(16.0)));
            } else {
                ui.painter().text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    label.get(..1).unwrap_or_default(),
                    egui::FontId::proportional(13.0),
                    ink,
                );
            }
            if resp.on_hover_text(*label).clicked() {
                pick = Some(cmd);
            }
        }
    });
    if let Some(cmd) = pick {
        host.run(cmd, json!({"compose": cid}));
        // Typing goes on in the body.
        ctx.memory_mut(|m| m.request_focus(super::editor::editor_id(cid)));
        // The selection may have changed shape (Bullets); keep the bar for what is selected now.
        if let Some(c) = host.compose(cid) {
            store(ctx, cid, Bar { at: Some(at), selection: c.selection.clone() });
        }
        ctx.request_repaint();
    }
}
