//! Create Text Box Link: the text that does not fit in a text box flows on into the next box of its chain
//! ([`Drawing::link_next`]). The text belongs to the first box of the chain; [`flow_text`] works out what
//! each box shows, for the editor and for the pictures sent.

use serde_json::{Value, json};

use super::{DrawingKind, find, opt_bool, target};
use crate::cmd::insert::drawing as draw;
use crate::compose::ResolvedTheme;
use crate::{CmdError, CmdResult, Session};
use mailcraft_model::rich::Drawing;

/// Longest chain followed.
const MAX_CHAIN: usize = 64;

/// Whether `d` can hold linked text (a text box or a shape that is not a line).
fn holds_text(d: &Drawing) -> bool {
    match d.kind {
        DrawingKind::TextBox => true,
        DrawingKind::Shape => !crate::cmd::insert::shapes::is_line_shape(&d.shape),
        _ => false,
    }
}

/// The first box of the chain drawing `id` is in.
pub fn chain_head(ds: &[Drawing], id: u32) -> u32 {
    let mut at = id;
    for _ in 0..MAX_CHAIN {
        match ds.iter().find(|d| d.link_next == Some(at)) {
            Some(prev) if prev.id != id => at = prev.id,
            _ => break,
        }
    }
    at
}

/// The boxes of the chain starting at `head`, in order.
fn chain(ds: &[Drawing], head: u32) -> Vec<u32> {
    let mut out = vec![head];
    let mut at = head;
    while out.len() < MAX_CHAIN {
        let Some(next) = find(ds, at).and_then(|d| d.link_next).filter(|n| !out.contains(n)) else { break };
        if find(ds, next).is_none() {
            break;
        }
        out.push(next);
        at = next;
    }
    out
}

/// Splits `text` where box `d` is full: (what it shows, what flows on).
fn split_at_fit(d: &Drawing, text: &str, theme: &ResolvedTheme) -> (String, String) {
    let mut probe = Drawing { text: text.to_owned(), vertical: false, text_flow: String::new(), ..d.clone() };
    probe.warp.clear();
    let (_, _, tw, th) = draw::text_area(&probe);
    let layout = draw::text_layout(&probe, theme, tw);
    let Some(line) = layout.index.lines.iter().find(|l| l.rect.y + l.rect.h > th + 0.5) else {
        return (text.to_owned(), String::new());
    };
    let block = line.block_path.first().copied().unwrap_or(0);
    // Char index of the line's start in the whole text (blocks are the lines of the text).
    let before: usize = text.split('\n').take(block).map(|l| l.chars().count() + 1).sum();
    let at = before.saturating_add(line.start);
    let cut = text.char_indices().nth(at).map_or(text.len(), |(b, _)| b);
    let (shown, rest) = text.split_at(cut);
    (shown.trim_end_matches('\n').to_owned(), rest.to_owned())
}

/// `ds` with the text of every linked chain shared out: each box shows what fits in it and the rest goes
/// on to the next one (the last box shows all that is left).
pub fn flow_text(ds: &[Drawing], theme: &ResolvedTheme) -> Vec<Drawing> {
    let mut out = ds.to_vec();
    if !ds.iter().any(|d| d.link_next.is_some()) {
        return out;
    }
    let heads: Vec<u32> = ds
        .iter()
        .filter(|d| d.link_next.is_some() && !ds.iter().any(|o| o.link_next == Some(d.id)))
        .map(|d| d.id)
        .collect();
    for head in heads {
        let ids = chain(ds, head);
        let mut rest = find(ds, head).map(|d| d.text.clone()).unwrap_or_default();
        for (k, id) in ids.iter().enumerate() {
            let Some(d) = out.iter_mut().find(|d| d.id == *id) else { continue };
            if k + 1 == ids.len() {
                d.text = std::mem::take(&mut rest);
            } else {
                let (shown, more) = split_at_fit(d, &rest, theme);
                d.text = shown;
                rest = more;
            }
        }
    }
    out
}

/// Create Text Box Link: links the text box to `to` (an empty text box not in a chain yet); without `to`
/// the next click on a text box links it (the tool `link:<id>`). `break` breaks the link to the next box
/// (its text goes back into the boxes before).
pub(super) fn link_text_box(s: &mut Session, p: &Value) -> CmdResult {
    let (cid, id) = target(s, p)?;
    let c = s.composes.get_mut(&cid).ok_or_else(|| CmdError::Disabled("no compose".into()))?;
    let d = find(&c.body.drawings, id)
        .cloned()
        .ok_or_else(|| CmdError::Params(format!("no drawing with id {id}")))?;
    if opt_bool(p, "break") == Some(true) {
        if d.link_next.is_none() {
            return Ok(json!({"drawing": id, "changed": false}));
        }
        c.checkpoint();
        if let Some(m) = c.body.drawings.iter_mut().find(|m| m.id == id) {
            m.link_next = None;
        }
        c.dirty = true;
        return Ok(json!({"drawing": id, "changed": true}));
    }
    if !holds_text(&d) {
        return Err(CmdError::Failed("only text boxes and shapes that hold text can be linked".into()));
    }
    let Some(to) = p.get("to").filter(|v| !v.is_null()) else {
        // The next click picks the box to link to.
        c.draw_tool = Some(format!("link:{id}"));
        return Ok(json!({"armed": format!("link:{id}")}));
    };
    let to = super::parse_id(to)?;
    let t = find(&c.body.drawings, to)
        .cloned()
        .ok_or_else(|| CmdError::Params(format!("no drawing with id {to}")))?;
    if to == id || !holds_text(&t) {
        return Err(CmdError::Failed("link to another text box".into()));
    }
    if !t.text.trim().is_empty() {
        return Err(CmdError::Failed("the text box to link to must be empty".into()));
    }
    if c.body.drawings.iter().any(|o| o.link_next == Some(to)) {
        return Err(CmdError::Failed("that text box already takes text from another one".into()));
    }
    if chain(&c.body.drawings, to).contains(&id) {
        return Err(CmdError::Failed("that would link the text back into itself".into()));
    }
    c.checkpoint();
    if let Some(m) = c.body.drawings.iter_mut().find(|m| m.id == id) {
        m.link_next = Some(to);
    }
    if c.draw_tool.as_deref().is_some_and(|t| t.starts_with("link:")) {
        c.draw_tool = None;
    }
    c.dirty = true;
    Ok(json!({"drawing": id, "linked": to}))
}

#[cfg(test)]
mod tests {
    use super::super::super::compose::tests::{open, session};
    use super::*;

    #[test]
    fn linked_text_boxes_share_their_text() {
        let mut s = session();
        let cid = open(&mut s);
        let long = (1..=30).map(|n| format!("Line {n}")).collect::<Vec<_>>().join("\n");
        let r = s
            .run("insert.textBox", &json!({"compose": cid, "text": long, "x": 0, "y": 0, "w": 200, "h": 80}))
            .unwrap();
        let a = r["drawing"].as_u64().unwrap() as u32;
        let r = s
            .run("insert.textBox", &json!({"compose": cid, "text": "", "x": 300, "y": 0, "w": 200, "h": 400}))
            .unwrap();
        let b = r["drawing"].as_u64().unwrap() as u32;
        // Without `to` the tool waits for the box to click.
        let r = s.run("drawing.linkTextBox", &json!({"compose": cid, "id": a})).unwrap();
        assert_eq!(r["armed"], format!("link:{a}"));
        s.run("drawing.linkTextBox", &json!({"compose": cid, "id": a, "to": b})).unwrap();
        assert_eq!(s.composes[&cid].draw_tool, None);
        let c = &s.composes[&cid];
        let theme = c.theme_resolved();
        let flowed = flow_text(&c.body.drawings, &theme);
        let (ta, tb) = (&find(&flowed, a).unwrap().text, &find(&flowed, b).unwrap().text);
        assert!(ta.starts_with("Line 1\n") && !ta.contains("Line 30"), "{ta}");
        assert!(tb.ends_with("Line 30") && !tb.contains("Line 1\n"), "{tb}");
        assert_eq!(format!("{ta}\n{tb}"), find(&c.body.drawings, a).unwrap().text, "nothing lost or doubled");
        // Editing the second box edits the chain's text (kept in the first).
        s.run("drawing.text", &json!({"compose": cid, "id": b, "text": "Short"})).unwrap();
        assert_eq!(find(&s.composes[&cid].body.drawings, a).unwrap().text, "Short");
        assert_eq!(find(&s.composes[&cid].body.drawings, b).unwrap().text, "");
        // Refused: a box with text, a link back, linking to itself.
        let r = s.run("insert.textBox", &json!({"compose": cid, "text": "Full", "x": 0, "y": 300})).unwrap();
        let full = r["drawing"].as_u64().unwrap();
        assert!(s.run("drawing.linkTextBox", &json!({"compose": cid, "id": b, "to": full})).is_err());
        assert!(s.run("drawing.linkTextBox", &json!({"compose": cid, "id": b, "to": a})).is_err());
        assert!(s.run("drawing.linkTextBox", &json!({"compose": cid, "id": a, "to": a})).is_err());
        // Break Link.
        s.run("drawing.linkTextBox", &json!({"compose": cid, "id": a, "break": true})).unwrap();
        assert_eq!(find(&s.composes[&cid].body.drawings, a).unwrap().link_next, None);
        // Deleting a linked box drops the link.
        s.run("drawing.linkTextBox", &json!({"compose": cid, "id": a, "to": b})).unwrap();
        s.run("drawing.delete", &json!({"compose": cid, "id": b})).unwrap();
        assert_eq!(find(&s.composes[&cid].body.drawings, a).unwrap().link_next, None);
    }
}
