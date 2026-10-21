//! Wrap Text and Position: how the body text flows around a drawing ([`WrapStyle`]; the layout makes room,
//! see `mailcraft_layout`), In Line with Text (the drawing keeps its place in the text through a
//! `drawing:<id>` picture), the outline Tight and Through wrap to, and the Position gallery (with text
//! wrapping, at one of nine places on the page).

use serde_json::{Value, json};

use super::{DrawingKind, descendants, find, opt_str, rejoin, shift, targets, tidy};
use crate::cmd::insert::drawing as draw;
use crate::cmd::insert::shapes;
use crate::compose::{BodySelection, ComposeState, DocPos, PasteMode};
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::rich::{Drawing, WrapStyle};
use mailcraft_model::{Block, Doc, Inline, ParaStyle};

/// Most points of a wrap outline made from a shape.
const OUTLINE_POINTS: usize = 64;

fn layout(c: &ComposeState) -> mailcraft_layout::Layout {
    let width = c.view_width.filter(|w| w.is_finite() && *w > 40.0).unwrap_or(draw::DEFAULT_VIEW_WIDTH);
    let lo = mailcraft_layout::LayoutOptions { width, ..Default::default() };
    let fonts = mailcraft_fonts::FontDb::bundled();
    mailcraft_layout::layout_doc(&c.body, &fonts, &lo)
}

/// Where the In Line with Text drawings of `c` are shown: (id, (x, y, w, h)) px from the text area's corner.
pub fn inline_boxes(c: &ComposeState) -> Vec<(u32, (f32, f32, f32, f32))> {
    if !c.body.drawings.iter().any(|d| d.wrap == WrapStyle::Inline) {
        return Vec::new();
    }
    let l = layout(c);
    l.display
        .items
        .iter()
        .filter_map(|i| match i {
            mailcraft_layout::DisplayItem::Image { rect, src, .. } => {
                mailcraft_model::rich::inline_drawing_id(src).map(|id| (id, (rect.x, rect.y, rect.w, rect.h)))
            }
            _ => None,
        })
        .collect()
}

/// Where In Line with Text drawing `id` is shown.
pub(super) fn inline_box(c: &ComposeState, id: u32) -> Option<(f32, f32, f32, f32)> {
    inline_boxes(c).into_iter().find(|(i, _)| *i == id).map(|(_, b)| b)
}

/// The place in the text at (`x`, `y`).
fn text_at(c: &ComposeState, x: f32, y: f32) -> DocPos {
    let l = layout(c);
    l.hit_pos(mailcraft_geom::Point { x, y })
        .map(|t| DocPos::new(t.block_path, t.offset))
        .unwrap_or_else(|| c.body_start())
}

/// Puts the placeholder of drawing `d` into the text at `at` (the caret goes after it).
fn put_placeholder(c: &mut ComposeState, d: &Drawing, at: DocPos, src: &str) {
    c.selection = BodySelection::caret(at);
    let frag = Doc {
        blocks: vec![Block::Paragraph {
            style: ParaStyle::default(),
            inlines: vec![Inline::Image {
                src: src.to_owned(),
                alt: if d.alt.is_empty() { d.text.clone() } else { d.alt.clone() },
                width: Some(d.w),
                height: Some(d.h),
            }],
        }],
        ..Doc::default()
    };
    c.insert_fragment(&frag, PasteMode::KeepSource);
}

/// Moves In Line with Text drawing `id` to the place in the text at `at` (px). One undo step; false when
/// it is already there.
pub(super) fn move_inline(c: &mut ComposeState, id: u32, at: (f32, f32)) -> bool {
    let Some(d) = find(&c.body.drawings, id).cloned() else { return false };
    let target = text_at(c, at.0, at.1);
    let before = c.body.blocks.clone();
    let sel = c.selection.clone();
    c.checkpoint();
    // Put a stand-in at the new place first (offsets there still count the old one), then take the old
    // one out and name the stand-in.
    let moving = format!("drawing-moving:{id}");
    put_placeholder(c, &d, target, &moving);
    crate::compose::remove_placeholders(&mut c.body.blocks, id);
    crate::compose::replace_src(&mut c.body.blocks, &moving, &d.inline_src());
    c.selection = sel;
    if c.body.blocks == before {
        c.edit_undo.pop();
        return false;
    }
    c.ensure_body();
    c.dirty = true;
    true
}

/// The outline Tight and Through wrapping follow: the shape's outline as fractions of its box (at most
/// [`OUTLINE_POINTS`] points); a box for text boxes, WordArt, canvases and groups.
pub(crate) fn outline_points(d: &Drawing) -> Vec<(f32, f32)> {
    let boxed = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    if d.kind != DrawingKind::Shape {
        return boxed;
    }
    let Some(g) = shapes::geometry(&d.shape, d.w, d.h, &d.points) else { return boxed };
    let path = g
        .fill
        .iter()
        .chain(g.faces.iter().map(|(p, _)| p))
        .max_by_key(|p| p.len())
        .cloned()
        .unwrap_or_default();
    if path.len() < 3 {
        return boxed;
    }
    let step = path.len().div_ceil(OUTLINE_POINTS).max(1);
    let (w, h) = (d.w.max(1.0), d.h.max(1.0));
    path.iter().step_by(step).map(|(x, y)| ((x / w).clamp(0.0, 1.0), (y / h).clamp(0.0, 1.0))).collect()
}

fn wrap_named(name: &str) -> Option<(WrapStyle, bool)> {
    Some(match name {
        "inline" => (WrapStyle::Inline, false),
        "square" => (WrapStyle::Square, false),
        "tight" => (WrapStyle::Tight, false),
        "through" => (WrapStyle::Through, false),
        "topBottom" => (WrapStyle::TopBottom, false),
        "behind" => (WrapStyle::Front, true),
        "front" => (WrapStyle::Front, false),
        _ => return None,
    })
}

/// The Wrap Text name of a drawing's wrapping.
pub fn wrap_name(d: &Drawing) -> &'static str {
    match (d.wrap, d.behind_text) {
        (WrapStyle::Inline, _) => "inline",
        (WrapStyle::Square, _) => "square",
        (WrapStyle::Tight, _) => "tight",
        (WrapStyle::Through, _) => "through",
        (WrapStyle::TopBottom, _) => "topBottom",
        (WrapStyle::Front, true) => "behind",
        (WrapStyle::Front, false) => "front",
    }
}

/// Gives drawing `id` wrapping `wrap` (behind the text when `behind`). The caller checkpoints.
fn set_wrap(c: &mut ComposeState, id: u32, wrap: WrapStyle, behind: bool) {
    let Some(d) = find(&c.body.drawings, id).cloned() else { return };
    let was_inline = d.wrap == WrapStyle::Inline;
    let shown = if was_inline { inline_box(c, id) } else { None };
    if was_inline && wrap != WrapStyle::Inline {
        crate::compose::remove_placeholders(&mut c.body.blocks, id);
        c.ensure_body();
    }
    if wrap == WrapStyle::Inline && !was_inline {
        let at = text_at(c, d.x, d.y);
        let sel = c.selection.clone();
        put_placeholder(c, &d, at, &d.inline_src());
        c.selection = sel;
    }
    if let Some(m) = c.body.drawings.iter_mut().find(|m| m.id == id) {
        if let Some((x, y, ..)) = shown {
            (m.x, m.y) = (x, y);
        }
        m.wrap = wrap;
        m.behind_text = behind && wrap != WrapStyle::Inline;
        if wrap == WrapStyle::Inline {
            m.rotation = 0.0;
            m.canvas = None;
        }
        m.wrap_points = if matches!(wrap, WrapStyle::Tight | WrapStyle::Through) {
            outline_points(m)
        } else {
            Vec::new()
        };
    }
    if wrap != WrapStyle::Inline {
        rejoin(c, id);
    }
}

/// Checks that drawing `d` can take wrapping `wrap` on its own.
fn can_wrap(ds: &[Drawing], d: &Drawing, wrap: WrapStyle) -> Result<(), CmdError> {
    if d.group.is_some_and(|g| find(ds, g).is_some()) {
        return Err(CmdError::Failed("a drawing in a group wraps with its group: select the group".into()));
    }
    if d.canvas.is_some() && d.kind != DrawingKind::Canvas {
        return Err(CmdError::Failed("a drawing in a drawing canvas wraps with the canvas".into()));
    }
    if wrap == WrapStyle::Inline && matches!(d.kind, DrawingKind::Group | DrawingKind::Canvas) {
        return Err(CmdError::Failed(
            "groups and drawing canvases float; only a single drawing goes in line with the text".into(),
        ));
    }
    Ok(())
}

/// Wrap Text: `wrap` = inline, square, tight, through, topBottom, behind or front.
pub(super) fn wrap_text(s: &mut Session, p: &Value) -> CmdResult {
    let name = opt_str(p, "wrap").unwrap_or("");
    let Some((wrap, behind)) = wrap_named(name) else {
        return Err(CmdError::Params(format!(
            "unknown wrapping `{name}` (inline, square, tight, through, topBottom, behind, front)"
        )));
    };
    let (cid, ids) = targets(s, p)?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    for id in &ids {
        let d = find(&c.body.drawings, *id)
            .ok_or_else(|| CmdError::Params(format!("no drawing with id {id}")))?;
        can_wrap(&c.body.drawings, d, wrap)?;
    }
    let changed = ids.iter().any(|id| {
        find(&c.body.drawings, *id)
            .is_some_and(|d| d.wrap != wrap || d.behind_text != (behind && wrap != WrapStyle::Inline))
    });
    if changed {
        c.checkpoint();
        for id in &ids {
            set_wrap(c, *id, wrap, behind);
        }
        tidy(&mut c.body.drawings);
        c.dirty = true;
    }
    Ok(json!({"wrap": name, "changed": changed, "drawings": ids}))
}

/// Position: `inline`, or one of nine places on the page (`topLeft` … `bottomRight`) with text wrapping
/// (Square, unless the drawing already wraps).
pub(super) fn position(s: &mut Session, p: &Value) -> CmdResult {
    let at = opt_str(p, "at").unwrap_or("").to_owned();
    if at == "inline" {
        let mut q = p.clone();
        if let Some(o) = q.as_object_mut() {
            o.insert("wrap".into(), json!("inline"));
        }
        return wrap_text(s, &q);
    }
    let (row, col) = match at.split_at(at.find(|ch: char| ch.is_ascii_uppercase()).unwrap_or(at.len())) {
        (r @ ("top" | "middle" | "bottom"), c @ ("Left" | "Center" | "Right")) => {
            (r.to_owned(), c.to_owned())
        }
        _ => {
            return Err(CmdError::Params(format!(
                "unknown position `{at}` (inline, topLeft, topCenter, topRight, middleLeft, middleCenter, middleRight, bottomLeft, bottomCenter, bottomRight)"
            )));
        }
    };
    let (cid, ids) = targets(s, p)?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let id = ids.first().copied().ok_or_else(|| CmdError::Disabled("select a drawing first".into()))?;
    let d = find(&c.body.drawings, id)
        .cloned()
        .ok_or_else(|| CmdError::Params(format!("no drawing with id {id}")))?;
    can_wrap(&c.body.drawings, &d, WrapStyle::Square)?;
    c.checkpoint();
    if !d.wrap.wraps() {
        set_wrap(c, id, WrapStyle::Square, false);
    }
    let d = find(&c.body.drawings, id).cloned().unwrap_or(d);
    let width = c.view_width.filter(|w| w.is_finite() && *w > 40.0).unwrap_or(draw::DEFAULT_VIEW_WIDTH);
    // The page: as tall as the text laid out without the drawing in the way.
    let mut plain = c.body.clone();
    plain.drawings.clear();
    let fonts = mailcraft_fonts::FontDb::bundled();
    let lo = mailcraft_layout::LayoutOptions { width, ..Default::default() };
    let height = mailcraft_layout::layout_doc(&plain, &fonts, &lo).height.max(d.h);
    let nx = match col.as_str() {
        "Left" => 0.0,
        "Center" => (width - d.w) / 2.0,
        _ => width - d.w,
    }
    .max(0.0);
    let ny = match row.as_str() {
        "top" => 0.0,
        "middle" => (height - d.h) / 2.0,
        _ => height - d.h,
    }
    .max(0.0);
    let moving: Vec<u32> = std::iter::once(id).chain(descendants(&c.body.drawings, id)).collect();
    shift(&mut c.body.drawings, &moving, nx - d.x, ny - d.y);
    tidy(&mut c.body.drawings);
    c.dirty = true;
    Ok(
        json!({"drawing": id, "x": nx, "y": ny, "wrap": wrap_name(&find(&c.body.drawings, id).cloned().unwrap_or_default())}),
    )
}

#[cfg(test)]
mod tests {
    use super::super::super::compose::tests::{open, session};
    use super::*;

    fn get(s: &Session, cid: u64, id: u32) -> Drawing {
        find(&s.composes[&cid].body.drawings, id).cloned().unwrap()
    }

    #[test]
    fn wrapping_makes_the_text_flow_around_the_drawing() {
        let mut s = session();
        let cid = open(&mut s);
        s.composes.get_mut(&cid).unwrap().view_width = Some(500.0);
        let words = "word ".repeat(150);
        s.run("compose.insertText", &json!({"compose": cid, "text": words})).unwrap();
        let r = s
            .run("insert.shape", &json!({"compose": cid, "kind": "oval", "x": 0, "y": 0, "w": 120, "h": 120}))
            .unwrap();
        let id = r["drawing"].as_u64().unwrap() as u32;
        let first_x = |s: &Session| layout(&s.composes[&cid]).index.lines.first().map(|l| l.rect.x).unwrap();
        assert_eq!(first_x(&s), 0.0, "in front of the text: no room is made");
        s.run("drawing.wrapText", &json!({"compose": cid, "wrap": "square"})).unwrap();
        assert!(first_x(&s) > 120.0, "square: beside the box");
        s.run("drawing.wrapText", &json!({"compose": cid, "wrap": "tight"})).unwrap();
        assert!(get(&s, cid, id).wrap_points.len() >= 8, "tight follows the oval");
        s.run("drawing.wrapText", &json!({"compose": cid, "wrap": "topBottom"})).unwrap();
        let l = layout(&s.composes[&cid]);
        assert!(l.index.lines.first().unwrap().rect.y >= 120.0, "top and bottom: below it");
        s.run("drawing.wrapText", &json!({"compose": cid, "wrap": "behind"})).unwrap();
        assert!(get(&s, cid, id).behind_text);
        assert_eq!(first_x(&s), 0.0);
        // Undo goes back one wrapping at a time.
        s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
        assert_eq!(get(&s, cid, id).wrap, WrapStyle::TopBottom);
        assert!(s.run("drawing.wrapText", &json!({"compose": cid, "wrap": "sideways"})).is_err());
    }

    #[test]
    fn in_line_with_text_sits_in_the_text_and_moves_with_it() {
        let mut s = session();
        let cid = open(&mut s);
        s.run("compose.insertText", &json!({"compose": cid, "text": "Before after"})).unwrap();
        let r = s
            .run(
                "insert.shape",
                &json!({"compose": cid, "kind": "rectangle", "x": 0, "y": 0, "w": 40, "h": 30}),
            )
            .unwrap();
        let id = r["drawing"].as_u64().unwrap() as u32;
        s.run("drawing.wrapText", &json!({"compose": cid, "wrap": "inline"})).unwrap();
        let c = &s.composes[&cid];
        assert_eq!(crate::compose::placeholder_ids(&c.body.blocks), vec![id]);
        let (_, (_, _, w, h)) = inline_boxes(c)[0];
        assert_eq!((w, h), (40.0, 30.0));
        // Text typed before it pushes it along.
        let x0 = inline_boxes(&s.composes[&cid])[0].1.0;
        let c = s.composes.get_mut(&cid).unwrap();
        c.selection = BodySelection::caret(c.body_start());
        s.run("compose.insertText", &json!({"compose": cid, "text": "More "})).unwrap();
        let x1 = inline_boxes(&s.composes[&cid])[0].1.0;
        assert!(x1 > x0, "{x0} → {x1}");
        // Resizing it resizes its place in the text.
        s.run("drawing.size", &json!({"compose": cid, "id": id, "w": 80, "h": 30})).unwrap();
        s.run("compose.insertText", &json!({"compose": cid, "text": "x"})).unwrap();
        assert_eq!(inline_boxes(&s.composes[&cid])[0].1.2, 80.0);
        // Sending puts the drawing's picture where it sits in the text.
        let f = draw::flattened(&s.composes[&cid]).unwrap();
        assert!(f.body.drawings.is_empty());
        assert_eq!(f.inline_images.len(), 1);
        assert!(crate::compose::placeholder_ids(&f.body.blocks).is_empty());
        let html = mailcraft_html::doc_to_html(&f.body);
        assert!(html.contains("cid:"), "{html}");
        // Deleting the text it sits in deletes it.
        let c = s.composes.get_mut(&cid).unwrap();
        c.checkpoint();
        c.body.blocks = vec![Block::text_paragraph("gone")];
        c.checkpoint();
        assert!(c.body.drawings.is_empty());
        // Back to floating: it keeps where it was shown.
        s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
        s.run("compose.editUndo", &json!({"compose": cid})).unwrap();
        let shown = inline_box(&s.composes[&cid], id).unwrap();
        s.run("drawing.wrapText", &json!({"compose": cid, "id": id, "wrap": "square"})).unwrap();
        let d = get(&s, cid, id);
        assert_eq!((d.x, d.y), (shown.0, shown.1));
        assert!(crate::compose::placeholder_ids(&s.composes[&cid].body.blocks).is_empty());
    }

    #[test]
    fn position_puts_the_drawing_on_the_page_with_wrapping() {
        let mut s = session();
        let cid = open(&mut s);
        s.composes.get_mut(&cid).unwrap().view_width = Some(600.0);
        let r = s
            .run(
                "insert.shape",
                &json!({"compose": cid, "kind": "rectangle", "x": 0, "y": 0, "w": 100, "h": 50}),
            )
            .unwrap();
        let id = r["drawing"].as_u64().unwrap() as u32;
        s.run("drawing.position", &json!({"compose": cid, "at": "topRight"})).unwrap();
        let d = get(&s, cid, id);
        assert_eq!((d.x, d.y, d.wrap), (500.0, 0.0, WrapStyle::Square));
        s.run("drawing.position", &json!({"compose": cid, "at": "topCenter"})).unwrap();
        assert_eq!(get(&s, cid, id).x, 250.0);
        assert!(s.run("drawing.position", &json!({"compose": cid, "at": "nowhere"})).is_err());
    }
}
