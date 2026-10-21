//! Floating drawing objects in the compose body editor (text boxes, shapes, WordArt, drawing canvases; see
//! `mailcraft_engine::cmd::drawing`).
//!
//! The editor paints each drawing from the engine's pixels (`cmd::insert::drawing::raster`, the same picture
//! the recipient gets), behind the text or in front of it, and handles the pointer before the text does:
//! - with a drawing tool armed (Insert › Shapes, Draw Text Box, New Drawing Canvas), a drag draws the object
//!   (a click puts one of the default size there; a scribble follows the pointer);
//! - a click selects a drawing (frame and eight size handles), a drag moves it (a canvas carries what is
//!   in it), a handle drag resizes it; each ends in one command (`drawing.move`, `drawing.size`);
//! - a double-click (or Enter, F2, or typing) edits the text inside, in place; leaving the text commits it
//!   (`drawing.text`); a new text box or WordArt placeholder starts in this mode;
//! - with a drawing selected: Delete removes it, the arrow keys nudge it (Ctrl: by one pixel), Esc
//!   deselects; Esc also disarms a drawing tool.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use egui::{Color32, Event, Id, Key, Pos2, Rect, Response, Stroke, StrokeKind, Ui, pos2, vec2};
use serde_json::json;

use mailcraft_engine::cmd::insert::drawing::{self as engine_drawing, Raster};
use mailcraft_engine::cmd::insert::shapes;
use mailcraft_engine::compose::DocPos;
use mailcraft_model::rich::{Align, Drawing, DrawingKind};

use super::editor::EditorHost;

/// Side of a size handle (px on screen).
const HANDLE: f32 = 8.0;
/// Pointer travel below which a press is a click, not a drag (px on screen).
const CLICK_SLOP: f32 = 4.0;
/// Most drawing textures kept per compose.
const MAX_TEXTURES: usize = 400;

/// Where a size handle sits on the box: (x, y) as 0, 0.5 or 1 of the width / height.
const HANDLES: [(f32, f32); 8] =
    [(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (1.0, 0.5), (1.0, 1.0), (0.5, 1.0), (0.0, 1.0), (0.0, 0.5)];

#[derive(Clone, Copy, Debug, PartialEq)]
enum DragKind {
    Move,
    Resize(usize),
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
    editing: Option<Editing>,
    /// Drawing ids seen last frame (a new one that is selected starts in text mode).
    known: Option<HashSet<u32>>,
    max_id: u32,
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

/// Screen rectangle of a box (px at 100 %) laid over the body at `origin`, zoomed.
fn screen(b: [f32; 4], origin: Pos2, zoom: f32) -> Rect {
    Rect::from_min_size(origin + vec2(b[0], b[1]) * zoom, vec2(b[2], b[3]) * zoom)
}

fn bounds(d: &Drawing) -> [f32; 4] {
    [d.x, d.y, d.w, d.h]
}

/// The box a drawing is shown at this frame (its drag preview while dragging).
fn shown_box(m: &Mem, d: &Drawing, all: &[Drawing]) -> [f32; 4] {
    let Some(drag) = &m.drag else { return bounds(d) };
    if drag.id == d.id {
        return drag.cur;
    }
    // A canvas being moved carries what was drawn in it.
    if drag.kind == DragKind::Move
        && d.canvas == Some(drag.id)
        && all.iter().any(|c| c.id == drag.id && c.kind == DrawingKind::Canvas)
    {
        let (dx, dy) = (drag.cur[0] - drag.orig[0], drag.cur[1] - drag.orig[1]);
        return [d.x + dx, d.y + dy, d.w, d.h];
    }
    bounds(d)
}

/// Paints the drawings of compose `cid` behind the text (`behind`) or in front of it.
pub(crate) fn paint(ui: &Ui, host: &dyn EditorHost, cid: u64, origin: Pos2, zoom: f32, behind: bool) {
    let ctx = ui.ctx().clone();
    let Some(c) = host.compose(cid) else { return };
    if c.body.drawings.is_empty() {
        return;
    }
    let m: Mem = ctx.data(|d| d.get_temp::<Mem>(mem_id(cid))).unwrap_or_default();
    let theme = c.theme_resolved();
    let mut used = HashSet::new();
    let hidden = super::panes::hidden_objects(&ctx, cid);
    for d in c.body.drawings.iter().filter(|d| d.behind_text == behind && !is_hidden(&hidden, d.id)) {
        let b = shown_box(&m, d, &c.body.drawings);
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
    if !behind {
        // The selection frame and handles.
        if let Some(sel) = c.selected_drawing.and_then(|id| c.body.drawings.iter().find(|d| d.id == id)) {
            let r = screen(shown_box(&m, sel, &c.body.drawings), origin, zoom);
            let accent = host.theme().accent;
            ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, accent), StrokeKind::Outside);
            if m.editing.is_none() {
                for (hx, hy) in HANDLES {
                    let at = pos2(r.left() + r.width() * hx, r.top() + r.height() * hy);
                    let hr = Rect::from_center_size(at, vec2(HANDLE, HANDLE));
                    ui.painter().rect_filled(hr, 1.0, Color32::WHITE);
                    ui.painter().rect_stroke(hr, 1.0, Stroke::new(1.0, accent), StrokeKind::Inside);
                }
            }
        }
        // The object being drawn.
        if let Some(pts) = &m.rubber
            && let (Some(a), Some(b)) = (pts.first(), pts.last())
        {
            let accent = host.theme().accent;
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
fn hit(drawings: &[Drawing], p: Pos2, origin: Pos2, zoom: f32, behind_too: bool) -> Option<u32> {
    let lx = (p.x - origin.x) / zoom;
    let ly = (p.y - origin.y) / zoom;
    let slack = 4.0 / zoom;
    let inside = |d: &Drawing| {
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

/// Starts typing into drawing `id`.
fn start_editing(host: &dyn EditorHost, cid: u64, m: &mut Mem, id: u32, select_all: bool) {
    let Some(d) = host.compose(cid).and_then(|c| c.body.drawings.iter().find(|d| d.id == id)) else { return };
    if d.kind == DrawingKind::Canvas {
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

/// Handles the pointer and keys for the drawings of compose `cid`. Returns true when the pointer belongs to
/// the drawings this frame (the text does not see the press, drag or click).
pub(crate) fn interact(
    ui: &mut Ui,
    host: &mut dyn EditorHost,
    cid: u64,
    resp: &Response,
    origin: Pos2,
    zoom: f32,
    select_objects: bool,
    text_at: &dyn Fn(Pos2) -> DocPos,
) -> bool {
    let ctx = ui.ctx().clone();
    let mut m: Mem = ctx.data(|d| d.get_temp::<Mem>(mem_id(cid))).unwrap_or_default();
    let Some(c) = host.compose(cid) else { return false };
    let drawings = c.body.drawings.clone();
    // Drawings hidden in the Selection pane are not picked by the pointer.
    let hidden = super::panes::hidden_objects(&ctx, cid);
    let shown: Vec<Drawing> = drawings.iter().filter(|d| !is_hidden(&hidden, d.id)).cloned().collect();
    let tool = c.draw_tool.clone();
    let selected = c.selected_drawing.filter(|id| drawings.iter().any(|d| d.id == *id));
    // A new text box or WordArt placeholder, just inserted and selected, starts in text mode.
    let ids: HashSet<u32> = drawings.iter().map(|d| d.id).collect();
    if let (Some(known), Some(sel)) = (&m.known, selected)
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
        if hit(&shown, p, origin, zoom, true) == Some(e.id) {
            consumed = true;
        } else {
            commit(host, cid, &mut m);
        }
    }

    if tool.is_none() && m.editing.is_none() {
        // Press: a handle of the selection, else a drawing, else the text (deselects).
        if pressed
            && resp.hovered()
            && let Some(p) = pointer
        {
            let sel_box = selected.and_then(|id| drawings.iter().find(|d| d.id == id)).map(bounds);
            if let (Some(id), Some(b), Some(h)) =
                (selected, sel_box, sel_box.and_then(|b| handle_at(b, p, origin, zoom)))
            {
                m.drag =
                    Some(Drag { id, kind: DragKind::Resize(h), start: p, orig: b, cur: b, moved: false });
                consumed = true;
            } else if let Some(id) = hit(&shown, p, origin, zoom, select_objects) {
                if selected != Some(id) {
                    host.run("drawing.select", with_compose(cid, json!({"id": id})));
                }
                let b = drawings.iter().find(|d| d.id == id).map(bounds).unwrap_or_default();
                m.drag = Some(Drag { id, kind: DragKind::Move, start: p, orig: b, cur: b, moved: false });
                consumed = true;
            } else if selected.is_some() {
                host.run("drawing.select", with_compose(cid, json!({"id": null})));
            }
            if consumed {
                resp.request_focus();
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
                        DragKind::Move => [drag.orig[0] + dx, drag.orig[1] + dy, drag.orig[2], drag.orig[3]],
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
                    match d.kind {
                        DragKind::Move => {
                            host.run("drawing.move", with_compose(cid, json!({"id": d.id, "x": x, "y": y})))
                        }
                        DragKind::Resize(_) => host.run(
                            "drawing.size",
                            with_compose(cid, json!({"id": d.id, "x": x, "y": y, "w": w, "h": h})),
                        ),
                    };
                }
            }
        }
        if resp.double_clicked()
            && let Some(p) = pointer
            && let Some(id) = hit(&shown, p, origin, zoom, select_objects)
        {
            m.drag = None;
            start_editing(&*host, cid, &mut m, id, false);
            consumed = true;
        }
        if resp.hovered()
            && let Some(p) = resp.hover_pos()
        {
            let sel_box = selected.and_then(|id| drawings.iter().find(|d| d.id == id)).map(bounds);
            if let Some(h) = sel_box.and_then(|b| handle_at(b, p, origin, zoom)) {
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
        // Keys with a drawing selected (and the body focused).
        if let Some(id) = selected
            && resp.has_focus()
        {
            keys(ui, host, cid, &mut m, id, mods);
        }
    }
    if tool.is_some() && resp.has_focus() {
        let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if esc {
            m.rubber = None;
            host.run("drawing.tool", with_compose(cid, json!({"tool": null})));
        }
    }
    edit_text(ui, host, cid, &mut m, origin, zoom);
    store(&ctx, cid, m);
    consumed
}

/// Delete, arrows, Esc, Enter / F2 and typing with drawing `id` selected. Only these keys with their own
/// modifiers are taken (Ctrl+Enter still sends, Ctrl+Delete still reaches the body); typing is taken only by a
/// drawing that holds text.
fn keys(ui: &mut Ui, host: &mut dyn EditorHost, cid: u64, m: &mut Mem, id: u32, mods: egui::Modifiers) {
    let holds_text = host
        .compose(cid)
        .and_then(|c| c.body.drawings.iter().find(|d| d.id == id))
        .is_some_and(|d| d.kind != DrawingKind::Canvas);
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
    ui.input_mut(|i| {
        i.events.retain(|e| match e {
            Event::Key { key, pressed: true, modifiers, .. } if ours(key, modifiers) => {
                let step = if modifiers.command { 1.0 } else { 8.0 };
                match key {
                    Key::Delete | Key::Backspace => acts.push(("drawing.delete", json!({"id": id}))),
                    Key::ArrowLeft => acts.push(("drawing.move", json!({"id": id, "dx": -step}))),
                    Key::ArrowRight => acts.push(("drawing.move", json!({"id": id, "dx": step}))),
                    Key::ArrowUp => acts.push(("drawing.move", json!({"id": id, "dy": -step}))),
                    Key::ArrowDown => acts.push(("drawing.move", json!({"id": id, "dy": step}))),
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
        host.run(cmd, with_compose(cid, p));
    }
    if edit || !typed.is_empty() {
        start_editing(&*host, cid, m, id, false);
        if let Some(e) = m.editing.as_mut() {
            e.text.push_str(&typed);
        }
    }
}

/// The text edit laid over a drawing whose text is being typed.
fn edit_text(ui: &mut Ui, host: &mut dyn EditorHost, cid: u64, m: &mut Mem, origin: Pos2, zoom: f32) {
    let Some(e) = m.editing.as_mut() else { return };
    let Some(c) = host.compose(cid) else { return };
    let Some(d) = c.body.drawings.iter().find(|d| d.id == e.id).cloned() else {
        m.editing = None;
        return;
    };
    let theme = c.theme_resolved();
    let (tx, ty, tw, th) = match d.kind {
        DrawingKind::TextBox | DrawingKind::Canvas => (8.0, 4.0, (d.w - 16.0).max(4.0), (d.h - 8.0).max(4.0)),
        DrawingKind::WordArt => (0.0, 0.0, d.w.max(40.0), d.h),
        DrawingKind::Shape => shapes::text_rect(&d.shape, d.w, d.h),
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
        let all = vec![a, b, line, behind];
        let o = pos2(0.0, 0.0);
        assert_eq!(hit(&all, pos2(75.0, 75.0), o, 1.0, false), Some(2));
        assert_eq!(hit(&all, pos2(10.0, 10.0), o, 1.0, false), Some(1));
        assert_eq!(hit(&all, pos2(350.0, 13.0), o, 1.0, false), Some(3));
        assert_eq!(
            hit(&all, pos2(520.0, 20.0), o, 1.0, false),
            None,
            "behind the text: the text gets the click"
        );
        assert_eq!(hit(&all, pos2(520.0, 20.0), o, 1.0, true), Some(4), "Select Objects reaches it");
        assert_eq!(hit(&all, pos2(150.0, 20.0), pos2(100.0, 0.0), 2.0, false), Some(1), "origin and zoom");
        assert_eq!(handle_at([0.0, 0.0, 100.0, 50.0], pos2(101.0, 51.0), o, 1.0), Some(4));
    }
}
