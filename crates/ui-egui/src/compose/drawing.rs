//! Floating drawing objects in the compose body editor (text boxes, shapes, WordArt, drawing canvases,
//! groups; see `mailcraft_engine::cmd::drawing`).
//!
//! The editor paints each drawing from the engine's pixels (`cmd::insert::drawing::raster`, the same picture
//! the recipient gets), behind the text or in front of it, and handles the pointer before the text does:
//! - with a drawing tool armed (Insert › Shapes, Draw Text Box, New Drawing Canvas), a drag draws the object
//!   (a click puts one of the default size there; a scribble follows the pointer); with Create Text Box
//!   Link armed, a click on an empty text box links it;
//! - a click selects a drawing (a drawing in a group selects the group; frame and eight size handles);
//!   Shift-click adds one to the selection, Ctrl-click adds or takes one out; a drag moves the selection (a
//!   canvas carries what is in it), a handle drag resizes it; each ends in one command (`drawing.move`,
//!   `drawing.size`); a drawing in line with the text is dropped at a place in the text;
//! - a drag from an empty place with Select Objects on, or inside a selected canvas, selects the drawings
//!   it goes round;
//! - with Edit Points on, the shape's points show and can be dragged; a double-click (or Ctrl-click) on an
//!   edge adds one, Delete removes the point clicked last (never the drawing), Ctrl-click a point also
//!   removes it;
//! - a double-click (or Enter, F2, or typing) edits the text inside, in place; leaving the text commits it
//!   (`drawing.text`); a new text box or WordArt placeholder starts in this mode; linked text boxes edit
//!   the chain's text;
//! - with drawings selected: Delete removes them, the arrow keys nudge them (Ctrl: by one pixel), Esc
//!   deselects (or ends Edit Points); Esc also disarms a drawing tool.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use egui::{Color32, Event, Id, Key, Pos2, Rect, Response, Stroke, StrokeKind, Ui, pos2, vec2};
use serde_json::json;

use mailcraft_engine::cmd::drawing as engine_cmd;
use mailcraft_engine::cmd::insert::drawing::{self as engine_drawing, Raster};
use mailcraft_engine::cmd::insert::freeform::{self, Node};
use mailcraft_engine::cmd::insert::shapes;
use mailcraft_engine::compose::{ComposeState, DocPos};
use mailcraft_layout::{DisplayItem, Layout};
use mailcraft_model::rich::{Align, Drawing, DrawingKind, PointKind, WrapStyle};

use super::editor::EditorHost;

/// Side of a size handle (px on screen).
const HANDLE: f32 = 8.0;
/// Side of an Edit Points handle (px on screen).
const POINT: f32 = 7.0;
/// Pointer travel below which a press is a click, not a drag (px on screen).
const CLICK_SLOP: f32 = 4.0;
/// Most drawing textures kept per compose.
const MAX_TEXTURES: usize = 400;
/// A press this close to a selected canvas's edge moves it; further in, a drag selects what is in it.
const CANVAS_EDGE: f32 = 8.0;

/// Where a size handle sits on the box: (x, y) as 0, 0.5 or 1 of the width / height.
const HANDLES: [(f32, f32); 8] =
    [(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (1.0, 0.5), (1.0, 1.0), (0.5, 1.0), (0.0, 1.0), (0.0, 0.5)];

#[derive(Clone, Copy, Debug, PartialEq)]
enum DragKind {
    Move,
    Resize(usize),
    /// An Edit Points point.
    Point(usize),
    /// The curve handle of an Edit Points point: (point, true for the one after it).
    Handle(usize, bool),
}

#[derive(Clone, Debug)]
struct Drag {
    id: u32,
    kind: DragKind,
    start: Pos2,
    /// The box when the drag started (px at 100 %).
    orig: [f32; 4],
    /// The box now.
    cur: [f32; 4],
    moved: bool,
    /// What a move carries along (the selection, with group members and canvas contents).
    moving: Vec<u32>,
    /// The drawings a move moves (the selection's top-level drawings).
    targets: Vec<u32>,
}

/// Text being typed into a drawing.
#[derive(Clone, Debug)]
struct Editing {
    id: u32,
    text: String,
    original: String,
    /// The text edit should take the focus (and select everything when `select_all`).
    focus: bool,
    select_all: bool,
}

#[derive(Clone, Debug, Default)]
struct Mem {
    drag: Option<Drag>,
    /// Drawing a new object: where the drag started and the points it passed (screen).
    rubber: Option<Vec<Pos2>>,
    /// Selecting by dragging round drawings: from, to (screen), and whether it adds to the selection.
    marquee: Option<(Pos2, Pos2, bool)>,
    editing: Option<Editing>,
    /// Drawing ids seen last frame (a new one that is selected starts in text mode).
    known: Option<HashSet<u32>>,
    max_id: u32,
    /// Edit Points: the point clicked last (drawing, point), which Delete removes.
    point: Option<(u32, usize)>,
}

fn mem_id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-drawings", cid))
}

/// Textures of drawn objects, by content key.
#[derive(Clone, Default)]
struct Textures {
    map: HashMap<u64, (egui::TextureHandle, [f32; 4])>,
}

fn textures_id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-drawing-textures", cid))
}

fn key_of(d: &Drawing, theme: &mailcraft_engine::compose::ResolvedTheme, with_text: bool) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut shape = d.clone();
    shape.x = 0.0;
    shape.y = 0.0;
    shape.id = 0;
    shape.name.clear();
    shape.group = None;
    shape.link_next = None;
    serde_json::to_string(&shape).unwrap_or_default().hash(&mut h);
    format!("{:?}{}{}", theme.colors, theme.effects, theme.body_font).hash(&mut h);
    with_text.hash(&mut h);
    h.finish()
}

/// The texture of `d` and its picture box relative to the drawing's corner ([left, top, w, h], px at 100 %).
fn texture(
    ctx: &egui::Context,
    cid: u64,
    d: &Drawing,
    theme: &mailcraft_engine::compose::ResolvedTheme,
    with_text: bool,
    used: &mut HashSet<u64>,
) -> Option<(egui::TextureHandle, [f32; 4])> {
    let key = key_of(d, theme, with_text);
    used.insert(key);
    if let Some(hit) =
        ctx.data(|m| m.get_temp::<Textures>(textures_id(cid))).and_then(|t| t.map.get(&key).cloned())
    {
        return Some(hit);
    }
    let r: Raster = engine_drawing::raster(d, theme, with_text)?;
    let size = [usize::try_from(r.width).ok()?, usize::try_from(r.height).ok()?];
    if r.rgba.len() != size[0].saturating_mul(size[1]).saturating_mul(4) {
        return None;
    }
    let img = egui::ColorImage::from_rgba_premultiplied(size, &r.rgba);
    let tex =
        ctx.load_texture(format!("mailcraft-drawing-{cid}-{key:016x}"), img, egui::TextureOptions::LINEAR);
    let (w, h) = r.size();
    let entry = (tex, [r.left, r.top, w, h]);
    ctx.data_mut(|m| {
        let t = m.get_temp_mut_or_default::<Textures>(textures_id(cid));
        if t.map.len() >= MAX_TEXTURES {
            t.map.clear();
        }
        t.map.insert(key, entry.clone());
    });
    Some(entry)
}

/// The drawings as the editor shows them: linked text boxes with their share of the text, drawings in
/// line with the text where `layout` (laid out at `zoom`) put their place. In-line drawings whose place is
/// not laid out are left out.
pub(crate) fn displayed(c: &ComposeState, layout: Option<&Layout>, zoom: f32) -> Vec<Drawing> {
    if c.body.drawings.is_empty() {
        return Vec::new();
    }
    let theme = c.theme_resolved();
    let mut v = engine_cmd::flow_text(&c.body.drawings, &theme);
    if v.iter().any(|d| d.wrap == WrapStyle::Inline) {
        let zoom = if zoom.is_finite() && zoom > 0.0 { zoom } else { 1.0 };
        let mut places: HashMap<u32, [f32; 4]> = HashMap::new();
        for item in layout.map(|l| l.display.items.as_slice()).unwrap_or_default() {
            if let DisplayItem::Image { rect, src, .. } = item
                && let Some(id) = mailcraft_model::rich::inline_drawing_id(src)
            {
                places.entry(id).or_insert([rect.x / zoom, rect.y / zoom, rect.w / zoom, rect.h / zoom]);
            }
        }
        v.retain_mut(|d| {
            if d.wrap != WrapStyle::Inline {
                return true;
            }
            match places.get(&d.id) {
                Some(b) => {
                    (d.x, d.y) = (b[0], b[1]);
                    true
                }
                None => false,
            }
        });
    }
    v
}

/// Screen rectangle of a box (px at 100 %) laid over the body at `origin`, zoomed.
fn screen(b: [f32; 4], origin: Pos2, zoom: f32) -> Rect {
    Rect::from_min_size(origin + vec2(b[0], b[1]) * zoom, vec2(b[2], b[3]) * zoom)
}

fn bounds(d: &Drawing) -> [f32; 4] {
    [d.x, d.y, d.w, d.h]
}

/// The box a drawing is shown at this frame (its drag preview while dragging).
fn shown_box(m: &Mem, d: &Drawing) -> [f32; 4] {
    let Some(drag) = &m.drag else { return bounds(d) };
    match drag.kind {
        DragKind::Resize(_) if drag.id == d.id => drag.cur,
        DragKind::Move if drag.moving.contains(&d.id) => {
            let (dx, dy) = (drag.cur[0] - drag.orig[0], drag.cur[1] - drag.orig[1]);
            [d.x + dx, d.y + dy, d.w, d.h]
        }
        _ => bounds(d),
    }
}

/// The selected drawings of `c` among `shown`, the one picked last first.
fn selection(c: &ComposeState, shown: &[Drawing]) -> Vec<u32> {
    c.selected_drawings().into_iter().filter(|id| shown.iter().any(|d| d.id == *id)).collect()
}

/// Lines one curved segment is drawn with on screen.
const CURVE_STEPS: usize = 24;

/// A drawing being point-edited, on screen: its points with their curve handles, and each segment as a
/// polyline (segment `i` runs from point `i` to the next).
#[derive(Clone, Debug, Default)]
struct PointView {
    id: u32,
    nodes: Vec<Node>,
    segs: Vec<Vec<Pos2>>,
    closed: bool,
}

impl PointView {
    fn points(&self) -> Vec<Pos2> {
        self.nodes.iter().map(|n| pos2(n.at.0, n.at.1)).collect()
    }

    /// The curve handle of point `point` under `p`: (point, true for the one after it).
    fn handle_at(&self, point: usize, p: Pos2) -> Option<(usize, bool)> {
        let n = self.nodes.get(point)?;
        let near = |h: Option<(f32, f32)>| h.is_some_and(|h| pos2(h.0, h.1).distance(p) <= POINT);
        if near(n.after) {
            Some((point, true))
        } else if near(n.before) {
            Some((point, false))
        } else {
            None
        }
    }

    /// The segment nearest `p` (within a few px): the point it starts from.
    fn segment_at(&self, p: Pos2) -> Option<usize> {
        self.segs
            .iter()
            .enumerate()
            .filter_map(|(i, line)| {
                let d = line
                    .windows(2)
                    .filter_map(|w| Some(to_segment(p, *w.first()?, *w.get(1)?)))
                    .fold(f32::MAX, f32::min);
                (d <= 5.0).then_some((i, d))
            })
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .map(|(i, _)| i)
    }
}

/// Distance from `p` to the line from `a` to `b`.
fn to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t).distance(p)
}

/// The points, handles and segments of drawing `d` (shown in box `b`) on screen; a point or handle being
/// dragged (`drag`) shows where it is going.
fn point_view(d: &Drawing, b: [f32; 4], origin: Pos2, zoom: f32, drag: Option<&Drag>) -> PointView {
    let (x, y) = (origin.x + b[0] * zoom, origin.y + b[1] * zoom);
    let mut nodes = freeform::nodes_from(&d.points, &d.handles, x, y, b[2] * zoom, b[3] * zoom);
    if let Some(drag) = drag.filter(|g| g.id == d.id && g.moved) {
        let (dx, dy) = ((drag.cur[0] - drag.orig[0]) * zoom, (drag.cur[1] - drag.orig[1]) * zoom);
        match drag.kind {
            DragKind::Point(i) => {
                if let Some(n) = nodes.get_mut(i) {
                    n.translate(dx, dy);
                }
            }
            DragKind::Handle(i, after) => {
                if let Some(n) = nodes.get_mut(i) {
                    let h = if after { &mut n.after } else { &mut n.before };
                    *h = h.map(|h| (h.0 + dx, h.1 + dy));
                    freeform::constrain(n, after);
                }
            }
            DragKind::Move | DragKind::Resize(_) => {}
        }
    }
    let closed = d.shape == "freeform";
    let segs = (0..freeform::segment_count(nodes.len(), closed))
        .filter_map(|i| freeform::segment(&nodes, i, closed))
        .map(|(p0, c1, c2, p3, curved)| {
            if curved {
                (0..=CURVE_STEPS)
                    .map(|k| freeform::cubic_at(p0, c1, c2, p3, k as f32 / CURVE_STEPS as f32))
                    .map(|q| pos2(q.0, q.1))
                    .collect()
            } else {
                vec![pos2(p0.0, p0.1), pos2(p3.0, p3.1)]
            }
        })
        .collect();
    PointView { id: d.id, nodes, segs, closed }
}

/// What a right-click in Edit Points is on: a point (with how it joins its segments) or a segment.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PointMenu {
    pub id: u32,
    pub point: Option<(usize, PointKind)>,
    /// The segment and whether it is curved.
    pub segment: Option<(usize, bool)>,
    pub closed: bool,
}

/// Right-click at `p` in compose `cid`: the point or segment of the point-edited drawing under it (the point
/// becomes the picked one, its handles showing).
pub(crate) fn point_menu_at(
    ctx: &egui::Context,
    host: &dyn EditorHost,
    cid: u64,
    p: Pos2,
    origin: Pos2,
    zoom: f32,
    shown: &[Drawing],
) -> Option<PointMenu> {
    let c = host.compose(cid)?;
    let id = c.edit_points?;
    let d = shown.iter().find(|d| d.id == id)?;
    let view = point_view(d, bounds(d), origin, zoom, None);
    let point = point_at(&view.points(), p);
    let segment = if point.is_none() { view.segment_at(p) } else { None };
    if point.is_none() && segment.is_none() {
        return None;
    }
    let mut m: Mem = ctx.data(|x| x.get_temp::<Mem>(mem_id(cid))).unwrap_or_default();
    if let Some(i) = point {
        m.point = Some((id, i));
        store(ctx, cid, m);
    }
    let curved = |i: usize| freeform::segment(&view.nodes, i, view.closed).is_some_and(|s| s.4);
    Some(PointMenu {
        id,
        point: point.and_then(|i| view.nodes.get(i).map(|n| (i, n.kind))),
        segment: segment.map(|i| (i, curved(i))),
        closed: view.closed,
    })
}

/// The Edit Points part of the context menu: the `drawing.editPoints` params picked.
pub(crate) fn point_menu_ui(ui: &mut Ui, at: &PointMenu) -> Option<serde_json::Value> {
    let mut pick = None;
    let id = at.id;
    let mut item = |ui: &mut Ui, enabled: bool, label: &str, p: serde_json::Value| {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            pick = Some(p);
        }
    };
    if let Some((i, _)) = at.segment {
        item(ui, true, "Add Point", json!({"id": id, "insert": i}));
    }
    if let Some((i, _)) = at.point {
        item(ui, true, "Delete Point", json!({"id": id, "delete": i}));
    }
    match (at.closed, at.segment) {
        (true, Some((i, _))) => item(ui, true, "Open Path", json!({"id": id, "path": "open", "segment": i})),
        (true, None) => item(ui, true, "Open Path", json!({"id": id, "path": "open"})),
        (false, _) => item(ui, true, "Close Path", json!({"id": id, "path": "close"})),
    }
    ui.separator();
    if let Some((i, kind)) = at.point {
        for (label, k, name) in [
            ("Smooth Point", PointKind::Smooth, "smooth"),
            ("Straight Point", PointKind::Straight, "straight"),
            ("Corner Point", PointKind::Corner, "corner"),
        ] {
            item(ui, kind != k, label, json!({"id": id, "point": i, "pointType": name}));
        }
    }
    if let Some((i, curved)) = at.segment {
        item(ui, curved, "Straight Segment", json!({"id": id, "segment": i, "segmentType": "straight"}));
        item(ui, !curved, "Curved Segment", json!({"id": id, "segment": i, "segmentType": "curved"}));
    }
    ui.separator();
    item(ui, true, "Exit Edit Points", json!({"id": id, "on": false}));
    pick
}

/// Forgets the Edit Points point picked last in compose `cid` (after a menu changed the points).
pub(crate) fn forget_point(ctx: &egui::Context, cid: u64) {
    let mut m: Mem = ctx.data(|x| x.get_temp::<Mem>(mem_id(cid))).unwrap_or_default();
    m.point = None;
    store(ctx, cid, m);
}

/// Paints the drawings of compose `cid` (as [`displayed`] gives them) behind the text (`behind`) or in front
/// of it.
pub(crate) fn paint(
    ui: &Ui,
    host: &dyn EditorHost,
    cid: u64,
    origin: Pos2,
    zoom: f32,
    behind: bool,
    shown: &[Drawing],
) {
    let ctx = ui.ctx().clone();
    let Some(c) = host.compose(cid) else { return };
    if shown.is_empty() {
        return;
    }
    let m: Mem = ctx.data(|d| d.get_temp::<Mem>(mem_id(cid))).unwrap_or_default();
    let theme = c.theme_resolved();
    let mut used = HashSet::new();
    let hidden = super::panes::hidden_objects(&ctx, cid);
    let hidden_ids: HashSet<u32> = shown
        .iter()
        .filter(|d| {
            is_hidden(&hidden, d.id)
                || d.group.is_some_and(|_| is_hidden(&hidden, engine_cmd::root_of(shown, d.id)))
        })
        .map(|d| d.id)
        .collect();
    for d in shown.iter().filter(|d| d.behind_text == behind && !hidden_ids.contains(&d.id)) {
        let b = shown_box(&m, d);
        let editing_this = m.editing.as_ref().is_some_and(|e| e.id == d.id);
        // While the text is typed, the picture is drawn without it (the text edit shows it).
        if let Some((tex, pic)) = texture(&ctx, cid, d, &theme, !editing_this, &mut used) {
            // Resizing previews the old picture stretched to the new box.
            let (sx, sy) = (b[2] / d.w.max(1.0), b[3] / d.h.max(1.0));
            let r = Rect::from_min_size(
                origin + vec2(b[0] + pic[0] * sx, b[1] + pic[1] * sy) * zoom,
                vec2(pic[2] * sx, pic[3] * sy) * zoom,
            );
            ui.painter().image(
                tex.id(),
                r,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        if d.kind == DrawingKind::Canvas {
            // The canvas frame shows while composing only.
            let r = screen(b, origin, zoom);
            ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_gray(0xb0)), StrokeKind::Inside);
        }
    }
    if behind {
        return;
    }
    let accent = host.theme().accent;
    // The selection: a frame round each selected drawing; handles on one picked alone.
    let selected = selection(c, shown);
    for (k, id) in selected.iter().enumerate() {
        let Some(sel) = shown.iter().find(|d| d.id == *id) else { continue };
        let b = shown_box(&m, sel);
        let r = screen(b, origin, zoom);
        let stroke = if sel.kind == DrawingKind::Group {
            Stroke::new(1.0, accent.gamma_multiply(0.8))
        } else {
            Stroke::new(1.0, accent)
        };
        ui.painter().rect_stroke(r, 0.0, stroke, StrokeKind::Outside);
        if c.edit_points == Some(*id) {
            // Edit Points: the outline (curved segments followed), its points, and the curve handles of
            // the point picked last.
            let view = point_view(sel, b, origin, zoom, m.drag.as_ref());
            for line in &view.segs {
                ui.painter().add(egui::Shape::dashed_line(line, Stroke::new(1.0, accent), 4.0, 3.0));
            }
            let picked = m.point.filter(|(pid, _)| *pid == view.id).map(|(_, i)| i);
            if let Some(n) = picked.and_then(|i| view.nodes.get(i)) {
                let at = pos2(n.at.0, n.at.1);
                for h in [n.before, n.after].into_iter().flatten() {
                    let hp = pos2(h.0, h.1);
                    ui.painter().line_segment([at, hp], Stroke::new(1.0, accent));
                    ui.painter().circle_filled(hp, POINT / 2.0, Color32::WHITE);
                    ui.painter().circle_stroke(hp, POINT / 2.0, Stroke::new(1.0, accent));
                }
            }
            for (i, p) in view.points().into_iter().enumerate() {
                let hr = Rect::from_center_size(p, vec2(POINT, POINT));
                let fill = if picked == Some(i) { accent } else { Color32::BLACK };
                ui.painter().rect_filled(hr, 0.0, fill);
                ui.painter().rect_stroke(hr, 0.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
            }
            continue;
        }
        if k == 0 && selected.len() == 1 && m.editing.is_none() {
            for (hx, hy) in HANDLES {
                let at = pos2(r.left() + r.width() * hx, r.top() + r.height() * hy);
                let hr = Rect::from_center_size(at, vec2(HANDLE, HANDLE));
                ui.painter().rect_filled(hr, 1.0, Color32::WHITE);
                ui.painter().rect_stroke(hr, 1.0, Stroke::new(1.0, accent), StrokeKind::Inside);
            }
        }
    }
    // Linked text boxes: a small mark where the text goes on.
    if let Some(first) = selected.first().and_then(|id| shown.iter().find(|d| d.id == *id))
        && let Some(next) = first.link_next.and_then(|n| shown.iter().find(|d| d.id == n))
    {
        let (a, b) = (screen(bounds(first), origin, zoom), screen(bounds(next), origin, zoom));
        ui.painter().add(egui::Shape::dashed_line(
            &[a.right_bottom(), b.left_top()],
            Stroke::new(1.0, accent.gamma_multiply(0.6)),
            5.0,
            4.0,
        ));
    }
    // The drag round drawings that selects them.
    if let Some((a, b, _)) = m.marquee {
        let r = Rect::from_two_pos(a, b);
        ui.painter().rect_filled(r, 0.0, accent.gamma_multiply(0.12));
        ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, accent), StrokeKind::Inside);
    }
    // The object being drawn.
    if let Some(pts) = &m.rubber
        && let (Some(a), Some(b)) = (pts.first(), pts.last())
    {
        let tool = c.draw_tool.as_deref().unwrap_or_default();
        if tool.ends_with("scribble") || tool.ends_with("freeform") {
            ui.painter().add(egui::Shape::line(pts.clone(), Stroke::new(2.0, accent)));
        } else if tool == "tablePencil" {
            ui.painter().line_segment([*a, *b], Stroke::new(1.5, accent));
        } else if tool == "tableEraser" {
            ui.painter().circle_stroke(*b, 6.0, Stroke::new(1.5, accent));
        } else {
            ui.painter().rect_stroke(
                Rect::from_two_pos(*a, *b),
                0.0,
                Stroke::new(1.0, accent),
                StrokeKind::Inside,
            );
        }
    }
    if !used.is_empty() {
        ctx.data_mut(|m| {
            let t = m.get_temp_mut_or_default::<Textures>(textures_id(cid));
            if t.map.len() > used.len() * 4 + 32 {
                t.map.retain(|k, _| used.contains(k));
            }
        });
    }
}

/// Whether the Selection pane hid drawing `id` from view.
fn is_hidden(hidden: &[String], id: u32) -> bool {
    !hidden.is_empty()
        && hidden.iter().any(|h| h.strip_prefix("drawing:").and_then(|n| n.parse().ok()) == Some(id))
}

/// The drawing under screen point `p` (front first; drawings behind the text only in Select Objects mode).
/// Groups draw nothing themselves: the drawing in them is hit.
fn hit(drawings: &[Drawing], p: Pos2, origin: Pos2, zoom: f32, behind_too: bool) -> Option<u32> {
    let lx = (p.x - origin.x) / zoom;
    let ly = (p.y - origin.y) / zoom;
    let slack = 4.0 / zoom;
    let inside = |d: &Drawing| {
        if d.kind == DrawingKind::Group {
            return false;
        }
        // Thin lines get a wider target.
        let (w, h) = (d.w.max(8.0 / zoom), d.h.max(8.0 / zoom));
        let (x, y) = (d.x + (d.w - w) / 2.0, d.y + (d.h - h) / 2.0);
        lx >= x - slack && lx <= x + w + slack && ly >= y - slack && ly <= y + h + slack
    };
    // Shapes in a canvas sit above it: the last (front-most) hit wins.
    let front = drawings.iter().rev().filter(|d| !d.behind_text).find(|d| inside(d)).map(|d| d.id);
    front.or_else(|| {
        behind_too
            .then(|| drawings.iter().rev().filter(|d| d.behind_text).find(|d| inside(d)).map(|d| d.id))
            .flatten()
    })
}

/// The size handle of the selected drawing under `p`.
fn handle_at(b: [f32; 4], p: Pos2, origin: Pos2, zoom: f32) -> Option<usize> {
    let r = screen(b, origin, zoom);
    HANDLES.iter().position(|(hx, hy)| {
        let at = pos2(r.left() + r.width() * hx, r.top() + r.height() * hy);
        Rect::from_center_size(at, vec2(HANDLE + 4.0, HANDLE + 4.0)).contains(p)
    })
}

/// The point of a point-edited drawing under `p`.
fn point_at(pts: &[Pos2], p: Pos2) -> Option<usize> {
    pts.iter().position(|q| Rect::from_center_size(*q, vec2(POINT + 4.0, POINT + 4.0)).contains(p))
}

/// The box after dragging handle `h` by (`dx`, `dy`) px at 100 %.
fn resized(orig: [f32; 4], h: usize, dx: f32, dy: f32, keep_ratio: bool) -> [f32; 4] {
    let [mut x, mut y, mut w, mut hh] = orig;
    let (hx, hy) = HANDLES.get(h).copied().unwrap_or((1.0, 1.0));
    if hx == 0.0 {
        x += dx;
        w -= dx;
    } else if hx == 1.0 {
        w += dx;
    }
    if hy == 0.0 {
        y += dy;
        hh -= dy;
    } else if hy == 1.0 {
        hh += dy;
    }
    if keep_ratio && orig[2] > 0.0 && orig[3] > 0.0 {
        let k = (w / orig[2]).max(hh / orig[3]);
        let (nw, nh) = (orig[2] * k, orig[3] * k);
        if hx == 0.0 {
            x = orig[0] + orig[2] - nw;
        }
        if hy == 0.0 {
            y = orig[1] + orig[3] - nh;
        }
        w = nw;
        hh = nh;
    }
    let min = 4.0;
    if w < min {
        if hx == 0.0 {
            x = orig[0] + orig[2] - min;
        }
        w = min;
    }
    if hh < min {
        if hy == 0.0 {
            y = orig[1] + orig[3] - min;
        }
        hh = min;
    }
    [x, y, w, hh]
}

fn store(ctx: &egui::Context, cid: u64, m: Mem) {
    ctx.data_mut(|d| d.insert_temp(mem_id(cid), m));
}

fn with_compose(cid: u64, mut v: serde_json::Value) -> serde_json::Value {
    if let Some(o) = v.as_object_mut() {
        o.insert("compose".into(), json!(cid));
    }
    v
}

/// Commits the text being typed (if it changed) and leaves text mode.
fn commit(host: &mut dyn EditorHost, cid: u64, m: &mut Mem) {
    if let Some(e) = m.editing.take()
        && e.text != e.original
    {
        host.run("drawing.text", with_compose(cid, json!({"id": e.id, "text": e.text})));
    }
}

/// Hands the text being typed into a drawing of compose `cid` to the engine (`drawing.text`) without ending
/// the edit, so a command run meanwhile (Send, Save, AutoSave, closing the window) sees it.
pub(crate) fn flush(ctx: &egui::Context, host: &mut dyn EditorHost, cid: u64) {
    let Some(mut m) = ctx.data(|d| d.get_temp::<Mem>(mem_id(cid))) else { return };
    let Some(e) = m.editing.as_mut().filter(|e| e.text != e.original) else { return };
    let (id, text) = (e.id, e.text.clone());
    e.original = text.clone();
    store(ctx, cid, m);
    host.run("drawing.text", with_compose(cid, json!({"id": id, "text": text})));
}

fn edit_focus_id() -> Id {
    Id::new("mailcraft-compose-drawing-edit-focus")
}

/// The compose whose drawing text has the keyboard focus, if any (formatting shortcuts then do not reach the
/// body, and in a window drawn inside the main one the keys still belong to that compose).
pub(crate) fn text_focus_compose(ctx: &egui::Context) -> Option<u64> {
    let focused = ctx.memory(|m| m.focused())?;
    ctx.data(|d| d.get_temp::<(Id, u64)>(edit_focus_id()))
        .filter(|(id, _)| *id == focused)
        .map(|(_, cid)| cid)
}

/// Whether the keyboard focus is in the text of a drawing.
pub(crate) fn text_focused(ctx: &egui::Context) -> bool {
    text_focus_compose(ctx).is_some()
}

/// Starts typing into drawing `id` (a linked text box: into its chain's text, kept in the first box).
fn start_editing(host: &dyn EditorHost, cid: u64, m: &mut Mem, id: u32, select_all: bool) {
    let Some(c) = host.compose(cid) else { return };
    let id = engine_cmd::chain_head(&c.body.drawings, id);
    let Some(d) = c.body.drawings.iter().find(|d| d.id == id) else { return };
    if matches!(d.kind, DrawingKind::Canvas | DrawingKind::Group) {
        return;
    }
    m.editing = Some(Editing { id, text: d.text.clone(), original: d.text.clone(), focus: true, select_all });
}

/// Finishes a drag that drew a new object with the armed tool.
fn draw_object(host: &mut dyn EditorHost, cid: u64, tool: &str, pts: &[Pos2], origin: Pos2, zoom: f32) {
    let (Some(a), Some(b)) = (pts.first(), pts.last()) else { return };
    let r = Rect::from_two_pos(*a, *b);
    let clicked = r.width() < CLICK_SLOP && r.height() < CLICK_SLOP;
    let x = (r.left() - origin.x) / zoom;
    let y = (r.top() - origin.y) / zoom;
    let (w, h) = (r.width() / zoom, r.height() / zoom);
    let mut p = serde_json::Map::new();
    p.insert("x".into(), json!(x));
    p.insert("y".into(), json!(y));
    if !clicked {
        p.insert("w".into(), json!(w.max(4.0)));
        p.insert("h".into(), json!(h.max(4.0)));
    }
    let (cmd, mut params) = match tool {
        "textBox" => ("insert.textBox", json!({"preset": "draw"})),
        "verticalTextBox" => ("insert.textBox", json!({"preset": "draw", "vertical": true})),
        "canvas" => ("insert.shape", json!({"kind": "canvas"})),
        "scribble" | "freeform" => ("insert.shape", json!({"kind": tool})),
        t => ("insert.shape", json!({"kind": t.strip_prefix("shape:").unwrap_or("rectangle")})),
    };
    let kind = params.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
    if !clicked && (kind == "scribble" || kind == "freeform") {
        // The path the pointer took, as fractions of its box.
        let (bw, bh) = (r.width().max(1.0), r.height().max(1.0));
        let step = (pts.len() / 400).max(1);
        let rel: Vec<serde_json::Value> = pts
            .iter()
            .step_by(step)
            .map(|q| json!([((q.x - r.left()) / bw).clamp(0.0, 1.0), ((q.y - r.top()) / bh).clamp(0.0, 1.0)]))
            .collect();
        p.insert("points".into(), json!(rel));
    } else if !clicked && shapes::is_line_shape(&kind) {
        // Lines run from where the drag started to where it ended.
        let fx = |v: f32, lo: f32| if (v - lo).abs() < 0.5 { 0.0 } else { 1.0 };
        p.insert(
            "points".into(),
            json!([[fx(a.x, r.left()), fx(a.y, r.top())], [fx(b.x, r.left()), fx(b.y, r.top())]]),
        );
    }
    if let Some(o) = params.as_object_mut() {
        for (k, v) in p {
            o.insert(k, v);
        }
    }
    host.run(cmd, with_compose(cid, params));
}

/// A table pencil stroke (or eraser click) from the text position where it started.
fn table_stroke(
    host: &mut dyn EditorHost,
    cid: u64,
    erase: bool,
    pts: &[Pos2],
    zoom: f32,
    text_at: &dyn Fn(Pos2) -> DocPos,
) {
    let (Some(a), Some(b)) = (pts.first(), pts.last()) else { return };
    let at = text_at(*a);
    let action = if erase { "erase" } else { "stroke" };
    host.run(
        "insert.table",
        with_compose(
            cid,
            json!({"mode": "draw", "action": action, "path": at.path, "offset": at.offset,
                   "dx": (b.x - a.x) / zoom, "dy": (b.y - a.y) / zoom}),
        ),
    );
}

/// What a move of the drawings `targets` carries: them and what goes with them.
fn moving_set(all: &[Drawing], targets: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for t in targets {
        for id in std::iter::once(*t).chain(engine_cmd::descendants(all, *t)) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// The top-level drawings (not in a group) wholly inside the screen rectangle `r`, not hidden.
fn inside_rect(shown: &[Drawing], r: Rect, origin: Pos2, zoom: f32, hidden: &[String]) -> Vec<u32> {
    shown
        .iter()
        .filter(|d| d.group.is_none() && !is_hidden(hidden, d.id) && d.wrap != WrapStyle::Inline)
        .filter(|d| r.contains_rect(screen(bounds(d), origin, zoom)))
        .map(|d| d.id)
        .collect()
}

/// Handles the pointer and keys for the drawings of compose `cid` ([`displayed`] as `shown`). Returns true
/// when the pointer belongs to the drawings this frame (the text does not see the press, drag or click).
pub(crate) fn interact(
    ui: &mut Ui,
    host: &mut dyn EditorHost,
    cid: u64,
    resp: &Response,
    origin: Pos2,
    zoom: f32,
    select_objects: bool,
    text_at: &dyn Fn(Pos2) -> DocPos,
    shown_all: &[Drawing],
) -> bool {
    let ctx = ui.ctx().clone();
    let mut m: Mem = ctx.data(|d| d.get_temp::<Mem>(mem_id(cid))).unwrap_or_default();
    let Some(c) = host.compose(cid) else { return false };
    let drawings = shown_all.to_vec();
    // Drawings hidden in the Selection pane are not picked by the pointer.
    let hidden = super::panes::hidden_objects(&ctx, cid);
    let shown: Vec<Drawing> = drawings.iter().filter(|d| !is_hidden(&hidden, d.id)).cloned().collect();
    let tool = c.draw_tool.clone();
    let edit_points = c.edit_points;
    let selected = selection(c, &drawings);
    let primary = selected.first().copied();
    // A new text box or WordArt placeholder, just inserted and selected, starts in text mode.
    let ids: HashSet<u32> = c.body.drawings.iter().map(|d| d.id).collect();
    if let (Some(known), Some(sel)) = (&m.known, c.selected_drawing)
        && !known.contains(&sel)
        && sel > m.max_id
        && let Some(d) = drawings.iter().find(|d| d.id == sel)
    {
        let placeholder = d.kind == DrawingKind::WordArt && d.text == "Your text here";
        if (d.kind == DrawingKind::TextBox && d.text.is_empty()) || placeholder {
            commit(host, cid, &mut m);
            start_editing(&*host, cid, &mut m, sel, placeholder);
        }
    }
    m.max_id = m.max_id.max(ids.iter().copied().max().unwrap_or(0));
    m.known = Some(ids);
    if let Some(e) = &m.editing
        && !drawings.iter().any(|d| d.id == e.id)
    {
        m.editing = None;
    }
    let (pressed, down, released, mods) = ui.input(|i| {
        (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released(), i.modifiers)
    });
    let pointer = resp.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.interact_pos()));
    let mut consumed = false;

    // Create Text Box Link: the next click on a text box links it.
    if let Some(from) =
        tool.as_deref().and_then(|t| t.strip_prefix("link:")).and_then(|n| n.parse::<u32>().ok())
    {
        if resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Alias);
        }
        if pressed
            && resp.hovered()
            && let Some(p) = pointer
        {
            commit(host, cid, &mut m);
            match hit(&shown, p, origin, zoom, true) {
                Some(to) => host.run("drawing.linkTextBox", with_compose(cid, json!({"id": from, "to": to}))),
                None => host.run("drawing.tool", with_compose(cid, json!({"tool": null}))),
            };
            resp.request_focus();
            consumed = true;
        }
        if resp.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            host.run("drawing.tool", with_compose(cid, json!({"tool": null})));
        }
        store(&ctx, cid, m);
        return consumed;
    }

    // Drawing a new object.
    if let Some(t) = tool.as_deref() {
        if resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if pressed
            && resp.hovered()
            && let Some(p) = pointer
        {
            commit(host, cid, &mut m);
            m.rubber = Some(vec![p]);
            resp.request_focus();
        }
        if let Some(pts) = m.rubber.as_mut() {
            consumed = true;
            if down
                && let Some(p) = pointer
                && pts.last().is_none_or(|l| l.distance(p) >= 1.0)
                && pts.len() < 20_000
            {
                pts.push(p);
            }
            if released || !down {
                let pts = m.rubber.take().unwrap_or_default();
                if t == "tablePencil" || t == "tableEraser" {
                    table_stroke(host, cid, t == "tableEraser", &pts, zoom, text_at);
                } else {
                    draw_object(host, cid, t, &pts, origin, zoom);
                }
            }
        }
    } else {
        m.rubber = None;
    }

    // Text being typed: a press outside the drawing ends it; one inside it stays with the drawing.
    if let Some(e) = &m.editing
        && pressed
        && let Some(p) = pointer
    {
        if hit(&shown, p, origin, zoom, true).is_some_and(|h| engine_cmd::chain_head(&drawings, h) == e.id) {
            consumed = true;
        } else {
            commit(host, cid, &mut m);
        }
    }

    if tool.is_none() && m.editing.is_none() {
        let sel_box = |id: Option<u32>| id.and_then(|id| drawings.iter().find(|d| d.id == id)).map(bounds);
        let point_edit = edit_points
            .filter(|e| primary == Some(*e))
            .and_then(|e| drawings.iter().find(|d| d.id == e))
            .map(|d| point_view(d, bounds(d), origin, zoom, None));
        // Press: a curve handle of the picked point, an Edit Points point, a handle of the selection, else a
        // drawing, else the text (deselects).
        if pressed
            && resp.hovered()
            && let Some(p) = pointer
        {
            let add = mods.shift;
            let toggle = mods.command || mods.ctrl;
            let picked = m.point.filter(|(pid, _)| point_edit.as_ref().is_some_and(|v| v.id == *pid));
            let on_curve_handle =
                point_edit.as_ref().zip(picked).and_then(|(v, (_, i))| v.handle_at(i, p).map(|h| (v.id, h)));
            let on_point = point_edit
                .as_ref()
                .filter(|_| on_curve_handle.is_none())
                .and_then(|v| point_at(&v.points(), p).map(|i| (v.id, i)));
            let on_edge = point_edit
                .as_ref()
                .filter(|_| toggle && on_point.is_none() && on_curve_handle.is_none())
                .and_then(|v| v.segment_at(p).map(|i| (v.id, i)));
            let handle = (selected.len() == 1 && point_edit.is_none())
                .then(|| sel_box(primary).and_then(|b| handle_at(b, p, origin, zoom)))
                .flatten();
            let local = ((p.x - origin.x) / zoom, (p.y - origin.y) / zoom);
            if let Some((id, (i, after))) = on_curve_handle {
                let b = sel_box(Some(id)).unwrap_or_default();
                m.drag = Some(Drag {
                    id,
                    kind: DragKind::Handle(i, after),
                    start: p,
                    orig: b,
                    cur: b,
                    moved: false,
                    moving: Vec::new(),
                    targets: Vec::new(),
                });
                consumed = true;
            } else if let Some((id, i)) = on_point {
                if toggle {
                    m.point = None;
                    host.run("drawing.editPoints", with_compose(cid, json!({"id": id, "delete": i})));
                } else {
                    m.point = Some((id, i));
                    let b = sel_box(Some(id)).unwrap_or_default();
                    m.drag = Some(Drag {
                        id,
                        kind: DragKind::Point(i),
                        start: p,
                        orig: b,
                        cur: b,
                        moved: false,
                        moving: Vec::new(),
                        targets: Vec::new(),
                    });
                }
                consumed = true;
            } else if let Some((id, i)) = on_edge {
                host.run(
                    "drawing.editPoints",
                    with_compose(cid, json!({"id": id, "insert": i, "x": local.0, "y": local.1})),
                );
                consumed = true;
            } else if let (Some(id), Some(b), Some(h)) = (primary, sel_box(primary), handle) {
                m.drag = Some(Drag {
                    id,
                    kind: DragKind::Resize(h),
                    start: p,
                    orig: b,
                    cur: b,
                    moved: false,
                    moving: vec![id],
                    targets: vec![id],
                });
                consumed = true;
            } else if let Some(leaf) = hit(&shown, p, origin, zoom, select_objects) {
                let id = engine_cmd::root_of(&drawings, leaf);
                let d = drawings.iter().find(|d| d.id == id).cloned().unwrap_or_default();
                // Inside a selected canvas (away from its edge) a drag selects what is in it.
                let canvas_inner = d.kind == DrawingKind::Canvas
                    && selected.contains(&id)
                    && leaf == id
                    && screen(bounds(&d), origin, zoom).shrink(CANVAS_EDGE).contains(p);
                if canvas_inner && !add && !toggle {
                    m.marquee = Some((p, p, false));
                } else {
                    let targets = if add || toggle {
                        let p = if toggle {
                            json!({"id": leaf, "toggle": true})
                        } else {
                            json!({"id": leaf, "add": true})
                        };
                        host.run("drawing.select", with_compose(cid, p));
                        host.compose(cid).map(ComposeState::selected_drawings).unwrap_or_default()
                    } else if selected.contains(&id) {
                        selected.clone()
                    } else {
                        host.run("drawing.select", with_compose(cid, json!({"id": leaf})));
                        vec![id]
                    };
                    if targets.contains(&id) {
                        let moving = moving_set(&drawings, &targets);
                        m.drag = Some(Drag {
                            id,
                            kind: DragKind::Move,
                            start: p,
                            orig: bounds(&d),
                            cur: bounds(&d),
                            moved: false,
                            moving,
                            targets,
                        });
                    }
                }
                consumed = true;
            } else if select_objects {
                // Select Objects: a drag on an empty place selects the drawings it goes round.
                m.marquee = Some((p, p, add || toggle));
                consumed = true;
            } else if !selected.is_empty() {
                host.run("drawing.select", with_compose(cid, json!({"id": null})));
            }
            if consumed {
                resp.request_focus();
            }
        }
        if let Some((a, _, add)) = m.marquee {
            consumed = true;
            if let Some(p) = pointer {
                m.marquee = Some((a, p, add));
            }
            if released || !down {
                let (a, b, add) = m.marquee.take().unwrap_or((a, a, add));
                let r = Rect::from_two_pos(a, b);
                if r.width() >= CLICK_SLOP || r.height() >= CLICK_SLOP {
                    let ids = inside_rect(&drawings, r, origin, zoom, &hidden);
                    if ids.is_empty() && !add {
                        host.run("drawing.select", with_compose(cid, json!({"id": null})));
                    } else if !ids.is_empty() {
                        host.run("drawing.select", with_compose(cid, json!({"ids": ids, "add": add})));
                    }
                } else if !add {
                    host.run("drawing.select", with_compose(cid, json!({"id": null})));
                }
            }
        }
        if let Some(drag) = m.drag.as_mut() {
            consumed = true;
            if let Some(p) = pointer {
                let (dx, dy) = ((p.x - drag.start.x) / zoom, (p.y - drag.start.y) / zoom);
                if (p - drag.start).length() >= CLICK_SLOP {
                    drag.moved = true;
                }
                if drag.moved {
                    drag.cur = match drag.kind {
                        DragKind::Move | DragKind::Point(_) | DragKind::Handle(..) => {
                            [drag.orig[0] + dx, drag.orig[1] + dy, drag.orig[2], drag.orig[3]]
                        }
                        DragKind::Resize(h) => {
                            let wordart =
                                drawings.iter().any(|d| d.id == drag.id && d.kind == DrawingKind::WordArt);
                            resized(drag.orig, h, dx, dy, mods.shift || wordart)
                        }
                    };
                }
            }
            if released || !down {
                let d = m.drag.take();
                if let Some(d) = d.filter(|d| d.moved && d.cur != d.orig) {
                    let [x, y, w, h] = d.cur;
                    let inline = drawings.iter().any(|x| x.id == d.id && x.wrap == WrapStyle::Inline);
                    match d.kind {
                        DragKind::Move if inline => {
                            // Dropped at a place in the text.
                            let at = pointer
                                .map_or((x, y), |p| ((p.x - origin.x) / zoom, (p.y - origin.y) / zoom));
                            host.run(
                                "drawing.move",
                                with_compose(cid, json!({"id": d.id, "x": at.0, "y": at.1})),
                            )
                        }
                        DragKind::Move => host.run(
                            "drawing.move",
                            with_compose(
                                cid,
                                json!({"ids": d.targets, "dx": x - d.orig[0], "dy": y - d.orig[1]}),
                            ),
                        ),
                        DragKind::Resize(_) => host.run(
                            "drawing.size",
                            with_compose(cid, json!({"id": d.id, "x": x, "y": y, "w": w, "h": h})),
                        ),
                        DragKind::Point(i) => {
                            let to = pointer.map(|p| ((p.x - origin.x) / zoom, (p.y - origin.y) / zoom));
                            to.and_then(|(px, py)| {
                                host.run(
                                    "drawing.editPoints",
                                    with_compose(cid, json!({"id": d.id, "point": i, "x": px, "y": py})),
                                )
                            })
                        }
                        DragKind::Handle(i, after) => {
                            // Where the handle was (body px), moved as far as the pointer went.
                            let was = drawings.iter().find(|x| x.id == d.id).and_then(|x| {
                                let nodes = freeform::nodes_from(&x.points, &x.handles, x.x, x.y, x.w, x.h);
                                nodes.get(i).and_then(|n| if after { n.after } else { n.before })
                            });
                            was.and_then(|(hx, hy)| {
                                let (px, py) = (hx + x - d.orig[0], hy + y - d.orig[1]);
                                let side = if after { "after" } else { "before" };
                                host.run(
                                    "drawing.editPoints",
                                    with_compose(
                                        cid,
                                        json!({"id": d.id, "handle": i, "side": side, "x": px, "y": py}),
                                    ),
                                )
                            })
                        }
                    };
                }
            }
        }
        if resp.double_clicked()
            && let Some(p) = pointer
        {
            // Edit Points: a double-click on an edge adds a point there; one on a point keeps it picked.
            let on_points = point_edit
                .as_ref()
                .map(|v| (v.id, point_at(&v.points(), p), v.segment_at(p)))
                .filter(|(_, pt, edge)| pt.is_some() || edge.is_some());
            if let Some((id, pt, edge)) = on_points {
                m.drag = None;
                if let (None, Some(i)) = (pt, edge) {
                    let local = ((p.x - origin.x) / zoom, (p.y - origin.y) / zoom);
                    host.run(
                        "drawing.editPoints",
                        with_compose(cid, json!({"id": id, "insert": i, "x": local.0, "y": local.1})),
                    );
                    m.point = None;
                }
                consumed = true;
            } else if let Some(id) = hit(&shown, p, origin, zoom, select_objects) {
                m.drag = None;
                start_editing(&*host, cid, &mut m, id, false);
                consumed = true;
            }
        }
        if resp.hovered()
            && let Some(p) = resp.hover_pos()
        {
            let on_point = point_edit.as_ref().is_some_and(|v| {
                point_at(&v.points(), p).is_some()
                    || m.point.is_some_and(|(pid, i)| pid == v.id && v.handle_at(i, p).is_some())
            });
            let handle = (selected.len() == 1 && point_edit.is_none())
                .then(|| sel_box(primary).and_then(|b| handle_at(b, p, origin, zoom)))
                .flatten();
            if on_point {
                ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                consumed = true;
            } else if let Some(h) = handle {
                let icon = match h {
                    0 | 4 => egui::CursorIcon::ResizeNwSe,
                    2 | 6 => egui::CursorIcon::ResizeNeSw,
                    1 | 5 => egui::CursorIcon::ResizeVertical,
                    _ => egui::CursorIcon::ResizeHorizontal,
                };
                ctx.set_cursor_icon(icon);
                consumed = true;
            } else if hit(&shown, p, origin, zoom, select_objects).is_some() {
                ctx.set_cursor_icon(egui::CursorIcon::Move);
                consumed = true;
            }
        }
        // Keys with drawings selected (and the body focused).
        if !selected.is_empty() && resp.has_focus() {
            keys(ui, host, cid, &mut m, &selected, edit_points.is_some(), mods);
        }
    }
    if tool.is_some() && resp.has_focus() {
        let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if esc {
            m.rubber = None;
            host.run("drawing.tool", with_compose(cid, json!({"tool": null})));
        }
    }
    edit_text(ui, host, cid, &mut m, origin, zoom, &drawings);
    store(&ctx, cid, m);
    consumed
}

/// Delete, arrows, Esc, Enter / F2 and typing with drawings `selected` (the one picked last first). Only
/// these keys with their own modifiers are taken (Ctrl+Enter still sends, Ctrl+Delete still reaches the
/// body); typing is taken only by a drawing that holds text.
fn keys(
    ui: &mut Ui,
    host: &mut dyn EditorHost,
    cid: u64,
    m: &mut Mem,
    selected: &[u32],
    editing_points: bool,
    mods: egui::Modifiers,
) {
    let Some(&id) = selected.first() else { return };
    let holds_text = selected.len() == 1
        && host
            .compose(cid)
            .and_then(|c| c.body.drawings.iter().find(|d| d.id == id))
            .is_some_and(|d| !matches!(d.kind, DrawingKind::Canvas | DrawingKind::Group));
    let mut acts: Vec<(&str, serde_json::Value)> = Vec::new();
    let mut edit = false;
    let mut typed = String::new();
    // Plain or with Shift (Enter, F2, Delete, Backspace, Esc); plain or with Ctrl (arrows: Ctrl nudges by 1 px).
    let plain = |m: &egui::Modifiers| !m.command && !m.ctrl && !m.alt && !m.mac_cmd;
    let nudge = |m: &egui::Modifiers| !m.alt && !m.shift;
    let ours = |key: &Key, m: &egui::Modifiers| match key {
        Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown => nudge(m),
        Key::Delete | Key::Backspace | Key::Escape | Key::F2 => plain(m),
        Key::Enter => plain(m) && holds_text,
        _ => false,
    };
    let ids = json!(selected);
    // Edit Points: Delete takes away the point clicked last, never the drawing.
    let point = m.point.filter(|(pid, _)| *pid == id).map(|(_, i)| i);
    ui.input_mut(|i| {
        i.events.retain(|e| match e {
            Event::Key { key, pressed: true, modifiers, .. } if ours(key, modifiers) => {
                let step = if modifiers.command { 1.0 } else { 8.0 };
                match key {
                    Key::Delete | Key::Backspace if editing_points => {
                        if let Some(at) = point {
                            acts.push(("drawing.editPoints", json!({"id": id, "delete": at})));
                        }
                    }
                    Key::Delete | Key::Backspace => acts.push(("drawing.delete", json!({"ids": ids}))),
                    Key::ArrowLeft => acts.push(("drawing.move", json!({"ids": ids, "dx": -step}))),
                    Key::ArrowRight => acts.push(("drawing.move", json!({"ids": ids, "dx": step}))),
                    Key::ArrowUp => acts.push(("drawing.move", json!({"ids": ids, "dy": -step}))),
                    Key::ArrowDown => acts.push(("drawing.move", json!({"ids": ids, "dy": step}))),
                    Key::Escape if editing_points => {
                        acts.push(("drawing.editPoints", json!({"id": id, "on": false})));
                    }
                    Key::Escape => acts.push(("drawing.select", json!({"id": null}))),
                    _ => edit = true,
                }
                false
            }
            Event::Key { key, pressed: false, modifiers, .. } if ours(key, modifiers) => false,
            Event::Text(t) if !mods.command && !mods.ctrl && holds_text => {
                typed.push_str(t);
                false
            }
            _ => true,
        })
    });
    for (cmd, p) in acts {
        if p.get("delete").is_some() {
            // The points after it moved down one: nothing is picked now.
            m.point = None;
        }
        host.run(cmd, with_compose(cid, p));
    }
    if (edit || !typed.is_empty()) && holds_text {
        start_editing(&*host, cid, m, id, false);
        if let Some(e) = m.editing.as_mut() {
            e.text.push_str(&typed);
        }
    }
}

/// The text edit laid over a drawing whose text is being typed.
fn edit_text(
    ui: &mut Ui,
    host: &mut dyn EditorHost,
    cid: u64,
    m: &mut Mem,
    origin: Pos2,
    zoom: f32,
    shown: &[Drawing],
) {
    let Some(e) = m.editing.as_mut() else { return };
    let Some(c) = host.compose(cid) else { return };
    let Some(mut d) = c.body.drawings.iter().find(|d| d.id == e.id).cloned() else {
        m.editing = None;
        return;
    };
    // Where it is shown (in-line drawings sit where the text put them).
    if let Some(s) = shown.iter().find(|s| s.id == d.id) {
        (d.x, d.y) = (s.x, s.y);
    }
    let theme = c.theme_resolved();
    let (tx, ty, tw, th) = match d.kind {
        DrawingKind::WordArt => (0.0, 0.0, d.w.max(40.0), d.h),
        _ => engine_drawing::text_area(&d),
    };
    let r = Rect::from_min_size(origin + vec2(d.x + tx, d.y + ty) * zoom, vec2(tw, th) * zoom);
    let pt = d.text_style.size_pt.unwrap_or(if d.kind == DrawingKind::WordArt { 36.0 } else { 11.0 });
    let px = (pt * 96.0 / 72.0 * zoom).clamp(4.0, 400.0);
    let col = d.text_style.color.unwrap_or_else(|| match d.kind {
        DrawingKind::WordArt => theme.color(0),
        _ => engine_drawing::default_text_color(&d, &theme),
    });
    let align = match d.align {
        Align::Center => egui::Align::Center,
        Align::End => egui::Align::RIGHT,
        _ if d.kind == DrawingKind::Shape => egui::Align::Center,
        _ => egui::Align::LEFT,
    };
    let id = Id::new(("mailcraft-drawing-text", cid, e.id));
    let te = egui::TextEdit::multiline(&mut e.text)
        .id(id)
        .frame(egui::Frame::NONE)
        .font(egui::FontId::proportional(px))
        .text_color(Color32::from_rgb(col.r, col.g, col.b))
        .horizontal_align(align)
        .desired_width(r.width())
        .desired_rows(1);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).id_salt(("mailcraft-drawing-edit", cid)));
    let out = child.add_sized(r.size(), te);
    if e.focus {
        // The caret goes after the text (or the placeholder is selected, to be typed over).
        out.request_focus();
        let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        let n = e.text.chars().count();
        let from = if e.select_all { 0 } else { n };
        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(from),
            egui::text::CCursor::new(n),
        )));
        state.store(ui.ctx(), id);
        e.focus = false;
        e.select_all = false;
    }
    if out.has_focus() {
        ui.ctx().data_mut(|d| d.insert_temp(edit_focus_id(), (id, cid)));
    }
    let esc = out.has_focus() && ui.input(|i| i.key_pressed(Key::Escape));
    if esc || out.lost_focus() {
        commit(host, cid, m);
        super::editor::refocus_body(ui.ctx(), cid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_resize_from_their_side() {
        let b = [10.0, 10.0, 100.0, 50.0];
        assert_eq!(resized(b, 4, 20.0, 10.0, false), [10.0, 10.0, 120.0, 60.0], "bottom-right grows");
        assert_eq!(
            resized(b, 0, 20.0, 10.0, false),
            [30.0, 20.0, 80.0, 40.0],
            "top-left shrinks from its corner"
        );
        assert_eq!(resized(b, 1, 50.0, -10.0, false), [10.0, 0.0, 100.0, 60.0], "top edge: height only");
        assert_eq!(resized(b, 3, -500.0, 0.0, false), [10.0, 10.0, 4.0, 50.0], "never smaller than 4 px");
        let k = resized(b, 4, 100.0, 0.0, true);
        assert_eq!((k[2], k[3]), (200.0, 100.0), "Shift keeps the proportions");
    }

    #[test]
    fn hits_prefer_the_front_drawing_and_thin_lines_are_easy_to_catch() {
        let a = Drawing { id: 1, x: 0.0, y: 0.0, w: 100.0, h: 100.0, ..Drawing::default() };
        let b = Drawing { id: 2, x: 50.0, y: 50.0, w: 100.0, h: 100.0, ..Drawing::default() };
        let line = Drawing { id: 3, x: 300.0, y: 10.0, w: 100.0, h: 0.0, ..Drawing::default() };
        let behind =
            Drawing { id: 4, x: 500.0, y: 0.0, w: 50.0, h: 50.0, behind_text: true, ..Drawing::default() };
        let group = Drawing {
            id: 5,
            kind: DrawingKind::Group,
            x: 0.0,
            y: 0.0,
            w: 600.0,
            h: 600.0,
            ..Drawing::default()
        };
        let all = vec![a, b, line, behind, group];
        let o = pos2(0.0, 0.0);
        assert_eq!(hit(&all, pos2(75.0, 75.0), o, 1.0, false), Some(2));
        assert_eq!(hit(&all, pos2(10.0, 10.0), o, 1.0, false), Some(1));
        assert_eq!(hit(&all, pos2(350.0, 13.0), o, 1.0, false), Some(3));
        assert_eq!(
            hit(&all, pos2(520.0, 20.0), o, 1.0, false),
            None,
            "behind the text: the text gets the click (a group draws nothing to hit)"
        );
        assert_eq!(hit(&all, pos2(520.0, 20.0), o, 1.0, true), Some(4), "Select Objects reaches it");
        assert_eq!(hit(&all, pos2(150.0, 20.0), pos2(100.0, 0.0), 2.0, false), Some(1), "origin and zoom");
        assert_eq!(handle_at([0.0, 0.0, 100.0, 50.0], pos2(101.0, 51.0), o, 1.0), Some(4));
    }

    #[test]
    fn edit_points_find_points_and_edges() {
        let tri = |shape: &str| Drawing {
            id: 1,
            shape: shape.into(),
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
            points: vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
            ..Drawing::default()
        };
        let o = pos2(0.0, 0.0);
        let closed = point_view(&tri("freeform"), [0.0, 0.0, 100.0, 100.0], o, 1.0, None);
        let open = point_view(&tri("scribble"), [0.0, 0.0, 100.0, 100.0], o, 1.0, None);
        assert_eq!(point_at(&closed.points(), pos2(99.0, 2.0)), Some(1));
        assert_eq!(point_at(&closed.points(), pos2(50.0, 50.0)), None);
        assert_eq!(closed.segment_at(pos2(50.0, 2.0)), Some(0));
        assert_eq!(closed.segment_at(pos2(50.0, 49.0)), Some(2), "the closing edge");
        assert_eq!(open.segment_at(pos2(50.0, 49.0)), None);
    }

    /// Release check: Edit Points drew and hit-tested straight lines between the points; a curved segment is
    /// now drawn and picked where it runs, and the picked point's curve handles can be caught.
    #[test]
    fn curved_segments_are_hit_where_they_run_and_handles_are_caught() {
        use mailcraft_model::rich::PointHandles;
        // The top side bent upward by its two handles (drawn at 100 % in a 100 × 100 box at 0, 0).
        let d = Drawing {
            id: 1,
            shape: "freeform".into(),
            w: 100.0,
            h: 100.0,
            points: vec![(0.0, 0.5), (1.0, 0.5), (1.0, 1.0), (0.0, 1.0)],
            handles: vec![
                PointHandles { kind: PointKind::Corner, before: None, after: Some((0.25, 0.0)) },
                PointHandles { kind: PointKind::Corner, before: Some((0.75, 0.0)), after: None },
                PointHandles::default(),
                PointHandles::default(),
            ],
            ..Drawing::default()
        };
        let v = point_view(&d, [0.0, 0.0, 100.0, 100.0], pos2(0.0, 0.0), 1.0, None);
        assert_eq!(v.segs.len(), 4);
        assert!(v.segs[0].len() > 2, "the curve is drawn in steps");
        // The bulge peaks at y = 50 - 0.75 * 50 = 12.5: hit there, not on the straight chord at y = 50.
        assert_eq!(v.segment_at(pos2(50.0, 13.0)), Some(0));
        assert_eq!(v.segment_at(pos2(50.0, 50.0)), None);
        assert_eq!(v.handle_at(0, pos2(25.0, 1.0)), Some((0, true)));
        assert_eq!(v.handle_at(1, pos2(75.0, 0.0)), Some((1, false)));
        assert_eq!(v.handle_at(2, pos2(75.0, 0.0)), None, "only the picked point's handles");
        // A handle being dragged shows its curve where it is going.
        let drag = Drag {
            id: 1,
            kind: DragKind::Handle(0, true),
            start: pos2(25.0, 0.0),
            orig: [0.0, 0.0, 100.0, 100.0],
            cur: [0.0, 40.0, 100.0, 100.0],
            moved: true,
            moving: Vec::new(),
            targets: Vec::new(),
        };
        let moved = point_view(&d, [0.0, 0.0, 100.0, 100.0], pos2(0.0, 0.0), 1.0, Some(&drag));
        assert_eq!(moved.nodes[0].after, Some((25.0, 40.0)));
        assert!(moved.segment_at(pos2(50.0, 13.0)).is_none());
    }

    /// The Edit Points context menu: a point offers its point types (not the one it has), a segment its
    /// segment types, and each item asks `drawing.editPoints` for the change.
    #[test]
    fn the_edit_points_menu_offers_what_the_spot_can_change() {
        let mut app = crate::App::new(mailcraft_engine::sample::sample_session());
        let cid = app
            .run("mail.newEmail", json!({}))
            .and_then(|v| v.get("compose").and_then(serde_json::Value::as_u64))
            .unwrap();
        let r = app
            .run("insert.shape", json!({"compose": cid, "kind": "oval", "x": 0, "y": 0, "w": 200, "h": 100}));
        let id = r.and_then(|r| r["drawing"].as_u64()).unwrap() as u32;
        app.run("drawing.editPoints", json!({"compose": cid, "id": id, "on": true}));
        let ctx = egui::Context::default();
        let shown = app.s.composes[&cid].body.drawings.clone();
        // The oval's right-hand point, at (200, 50).
        let at = point_menu_at(&ctx, &app, cid, pos2(200.0, 50.0), pos2(0.0, 0.0), 1.0, &shown).unwrap();
        assert_eq!(at.point.map(|(i, _)| i), Some(0));
        assert!(at.point.is_some_and(|(_, k)| k != PointKind::Corner));
        assert!(at.closed && at.segment.is_none());
        let m: Mem = ctx.data(|x| x.get_temp::<Mem>(mem_id(cid))).unwrap_or_default();
        assert_eq!(m.point, Some((id, 0)), "a right-click picks the point");
        // Halfway along the curved bottom right segment.
        let seg = point_menu_at(&ctx, &app, cid, pos2(170.71, 85.36), pos2(0.0, 0.0), 1.0, &shown).unwrap();
        assert_eq!(seg.segment, Some((0, true)));
        assert!(point_menu_at(&ctx, &app, cid, pos2(100.0, 50.0), pos2(0.0, 0.0), 1.0, &shown).is_none());

        // Corner Point from the menu, then Straight Segment, then Open Path.
        let click = |at: &PointMenu, label: &str| {
            let mut pick = None;
            let mut h = egui_kittest::Harness::new_ui(|ui| {
                if let Some(p) = point_menu_ui(ui, at) {
                    pick = Some(p);
                }
            });
            h.run();
            egui_kittest::kittest::Queryable::get_by_label(&h, label).click();
            h.run();
            drop(h);
            pick
        };
        let p = click(&at, "Corner Point").unwrap();
        assert_eq!(p, json!({"id": id, "point": 0, "pointType": "corner"}));
        app.run("drawing.editPoints", with_compose(cid, p)).unwrap();
        let p = click(&seg, "Straight Segment").unwrap();
        app.run("drawing.editPoints", with_compose(cid, p)).unwrap();
        let d = app.s.composes[&cid].body.drawings.iter().find(|d| d.id == id).cloned().unwrap();
        assert!(d.handles.first().is_some_and(|h| h.kind == PointKind::Corner && h.after.is_none()));
        let p = click(&seg, "Open Path").unwrap();
        assert_eq!(p, json!({"id": id, "path": "open", "segment": 0}));
        app.run("drawing.editPoints", with_compose(cid, p)).unwrap();
        assert_eq!(app.s.composes[&cid].body.drawings.iter().find(|d| d.id == id).unwrap().shape, "scribble");
    }

    /// Presses Delete with drawing `id` selected (`editing_points`: Edit Points is on).
    fn press_delete(app: &mut crate::App, cid: u64, m: &mut Mem, id: u32, editing_points: bool) {
        let mut input = egui::RawInput::default();
        input.events.push(Event::Key {
            key: Key::Delete,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let ctx = egui::Context::default();
        let mut out =
            ctx.run_ui(input, |ui| keys(ui, app, cid, m, &[id], editing_points, egui::Modifiers::NONE));
        out.textures_delta.clear();
    }

    /// Regression: Delete in Edit Points deleted the whole drawing.
    #[test]
    fn delete_in_edit_points_takes_a_point_not_the_drawing() {
        use mailcraft_engine::compose::{ComposeKind, ComposeState};
        let mut app = crate::App::new(mailcraft_engine::sample::sample_session());
        let cid = app.s.open_compose(ComposeState { kind: ComposeKind::New, ..ComposeState::default() });
        let r = app.run("insert.shape", json!({"compose": cid, "kind": "rectangle", "x": 0, "y": 0}));
        let id = r.and_then(|r| r["drawing"].as_u64()).unwrap() as u32;
        app.run("drawing.editPoints", json!({"compose": cid, "id": id, "on": true}));
        let points = |app: &crate::App| {
            app.s.composes[&cid].body.drawings.iter().find(|d| d.id == id).map(|d| d.points.len())
        };
        let before = points(&app).unwrap();
        assert!(before >= 4);
        let mut m = Mem::default();
        press_delete(&mut app, cid, &mut m, id, true);
        assert_eq!(points(&app), Some(before), "no point picked: the drawing and its points stay");
        m.point = Some((id, 1));
        press_delete(&mut app, cid, &mut m, id, true);
        assert_eq!(points(&app), Some(before - 1), "the point clicked last goes");
        assert!(m.point.is_none());
        // Without Edit Points, Delete deletes the drawing.
        press_delete(&mut app, cid, &mut m, id, false);
        assert_eq!(points(&app), None);
    }

    #[test]
    fn a_marquee_picks_the_drawings_it_goes_round() {
        let a = Drawing { id: 1, x: 10.0, y: 10.0, w: 20.0, h: 20.0, ..Drawing::default() };
        let b = Drawing { id: 2, x: 100.0, y: 10.0, w: 20.0, h: 20.0, ..Drawing::default() };
        let member =
            Drawing { id: 3, x: 12.0, y: 12.0, w: 5.0, h: 5.0, group: Some(9), ..Drawing::default() };
        let all = vec![a, b, member];
        let r = Rect::from_min_max(pos2(0.0, 0.0), pos2(50.0, 50.0));
        assert_eq!(inside_rect(&all, r, pos2(0.0, 0.0), 1.0, &[]), vec![1]);
        assert!(inside_rect(&all, r, pos2(0.0, 0.0), 1.0, &["drawing:1".into()]).is_empty());
    }
}
