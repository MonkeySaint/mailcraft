//! The compose's drawing selection and its In Line with Text drawings.
//!
//! An In Line with Text drawing ([`WrapStyle::Inline`]) keeps its place in the text through a picture whose
//! `src` is `drawing:<id>` ([`Drawing::inline_src`]); the layout gives it the room of a character and the
//! editor paints the drawing there. Edits that move text move the placeholder with it; afterwards
//! [`ComposeState::sync_inline_drawings`] mends what an edit left: a placeholder whose drawing is gone is
//! removed, a drawing whose placeholder was deleted goes too, a placeholder copied within the message gets
//! its own copy of the drawing, and every placeholder takes its drawing's size.

use std::collections::HashSet;

use mailcraft_model::rich::{Drawing, WrapStyle, inline_drawing_id};
use mailcraft_model::{Block, Doc, Inline};

use super::{ComposeState, EditRun, EditSnapshot, MAX_EDIT_UNDO};
use crate::compose::DocPos;

/// Deepest block nesting walked for placeholders.
const MAX_DEPTH: usize = 40;

/// Calls `f` on every inline list of `blocks` (paragraphs, headings, and those in lists, quotes, containers
/// and tables), in document order.
fn each_inlines(blocks: &mut [Block], depth: usize, f: &mut dyn FnMut(&mut Vec<Inline>)) {
    if depth > MAX_DEPTH {
        return;
    }
    for b in blocks {
        match b {
            Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => f(inlines),
            Block::List { items, .. } => {
                for it in items {
                    each_inlines(it, depth + 1, f);
                }
            }
            Block::Quote { blocks } | Block::Container { blocks, .. } => each_inlines(blocks, depth + 1, f),
            Block::Table { rows, .. } => {
                for cell in rows.iter_mut().flatten() {
                    each_inlines(&mut cell.blocks, depth + 1, f);
                }
            }
            Block::Pre { .. } | Block::Rule => {}
        }
    }
}

/// Calls `f` on every picture of `list` (inside links too) and drops those for which it returns false.
fn retain_images(list: &mut Vec<Inline>, depth: usize, f: &mut dyn FnMut(&mut Inline) -> bool) {
    if depth > MAX_DEPTH {
        return;
    }
    list.retain_mut(|i| match i {
        Inline::Image { .. } => f(i),
        Inline::Link { children, .. } => {
            retain_images(children, depth + 1, f);
            true
        }
        _ => true,
    });
}

/// Ids of the In Line with Text drawings whose placeholders are in `blocks`, in document order.
pub fn placeholder_ids(blocks: &[Block]) -> Vec<u32> {
    let mut blocks = blocks.to_vec();
    let mut out = Vec::new();
    each_inlines(&mut blocks, 0, &mut |list| {
        retain_images(list, 0, &mut |i| {
            if let Inline::Image { src, .. } = i
                && let Some(id) = inline_drawing_id(src)
            {
                out.push(id);
            }
            true
        });
    });
    out
}

/// Points the placeholders of drawing `old` in `blocks` at drawing `new`.
pub fn repoint_placeholders(blocks: &mut [Block], old: u32, new: u32) {
    each_inlines(blocks, 0, &mut |list| {
        retain_images(list, 0, &mut |i| {
            if let Inline::Image { src, .. } = i
                && inline_drawing_id(src) == Some(old)
            {
                *src = format!("{}{new}", mailcraft_model::rich::INLINE_DRAWING_PREFIX);
            }
            true
        });
    });
}

/// Points every picture of `blocks` whose `src` is `from` at `to`.
pub fn replace_src(blocks: &mut [Block], from: &str, to: &str) {
    each_inlines(blocks, 0, &mut |list| {
        retain_images(list, 0, &mut |i| {
            if let Inline::Image { src, .. } = i
                && src == from
            {
                *src = to.to_owned();
            }
            true
        });
    });
}

/// Sets the picture `src` of `blocks` to `width` × `height` and returns whether one was found.
pub fn resize_src(blocks: &mut [Block], src_of: &str, w: f32, h: f32) -> bool {
    let mut found = false;
    each_inlines(blocks, 0, &mut |list| {
        retain_images(list, 0, &mut |i| {
            if let Inline::Image { src, width, height, .. } = i
                && src == src_of
            {
                *width = Some(w);
                *height = Some(h);
                found = true;
            }
            true
        });
    });
    found
}

/// Removes the placeholders of drawing `id` from `blocks`.
pub fn remove_placeholders(blocks: &mut [Block], id: u32) {
    each_inlines(blocks, 0, &mut |list| {
        retain_images(
            list,
            0,
            &mut |i| !matches!(i, Inline::Image { src, .. } if inline_drawing_id(src) == Some(id)),
        );
    });
}

/// An edit made of several commands that is all or nothing and one undo step: what
/// [`ComposeState::begin_steps`] set aside for [`ComposeState::end_steps`].
pub(crate) struct Steps {
    before: EditSnapshot,
    undo: Vec<EditSnapshot>,
    redo: Vec<EditSnapshot>,
    dirty: bool,
    edit_rev: u64,
    edit_run: Option<(EditRun, DocPos, u64)>,
    selected: Option<u32>,
    also: Vec<u32>,
}

impl ComposeState {
    /// Starts an edit of several commands (Format Shape's OK): the undo and redo history are set aside, so
    /// the steps record theirs on an empty history, and the state before is kept for a failure.
    pub(crate) fn begin_steps(&mut self) -> Steps {
        Steps {
            before: self.snapshot(),
            undo: std::mem::take(&mut self.edit_undo),
            redo: std::mem::take(&mut self.edit_redo),
            dirty: self.dirty,
            edit_rev: self.edit_rev,
            edit_run: self.edit_run.clone(),
            selected: self.selected_drawing,
            also: self.also_selected.clone(),
        }
    }

    /// Ends what [`ComposeState::begin_steps`] started. `ok`: the steps are kept as one undo step (the
    /// state before the first of them; none when none changed anything). Otherwise everything is put back
    /// as it was, history included.
    pub(crate) fn end_steps(&mut self, st: Steps, ok: bool) {
        let pushed = std::mem::replace(&mut self.edit_undo, st.undo);
        if !ok {
            self.sync_bookmarks();
            self.restore(st.before);
            self.sync_bookmarks();
            self.edit_redo = st.redo;
            self.dirty = st.dirty;
            self.edit_rev = st.edit_rev;
            self.edit_run = st.edit_run;
            self.selected_drawing = st.selected;
            self.also_selected = st.also;
            return;
        }
        match pushed.into_iter().next() {
            Some(first) => {
                self.edit_undo.push(first);
                if self.edit_undo.len() > MAX_EDIT_UNDO {
                    self.edit_undo.remove(0);
                }
                self.edit_redo.clear();
                self.edit_run = None;
            }
            None => self.edit_redo = st.redo,
        }
    }

    /// Every selected drawing that is still in the body: the one picked last first.
    pub fn selected_drawings(&self) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for id in self.selected_drawing.iter().chain(self.also_selected.iter()) {
            if !out.contains(id) && self.body.drawings.iter().any(|d| d.id == *id) {
                out.push(*id);
            }
        }
        out
    }

    /// Selects drawing `id` alone (or none).
    pub fn select_drawing(&mut self, id: Option<u32>) {
        self.selected_drawing = id;
        self.also_selected.clear();
    }

    /// Mends the In Line with Text drawings after an edit of the text (see the module documentation).
    pub(crate) fn sync_inline_drawings(&mut self) {
        let has_inline = self.body.drawings.iter().any(|d| d.wrap == WrapStyle::Inline);
        let drawings = &mut self.body.drawings;
        let mut seen: HashSet<u32> = HashSet::new();
        let mut copies: Vec<Drawing> = Vec::new();
        let mut next = drawings.iter().map(|d| d.id).max().unwrap_or(0);
        let mut any_placeholder = false;
        each_inlines(&mut self.body.blocks, 0, &mut |list| {
            retain_images(list, 0, &mut |i| {
                let Inline::Image { src, width, height, alt } = i else { return true };
                let Some(id) = inline_drawing_id(src) else { return true };
                any_placeholder = true;
                let Some(d) = drawings.iter().find(|d| d.id == id && d.wrap == WrapStyle::Inline) else {
                    return false;
                };
                if seen.insert(id) {
                    *width = Some(d.w);
                    *height = Some(d.h);
                    return true;
                }
                // A second placeholder of the same drawing (copied and pasted): it gets its own drawing.
                if next >= u32::MAX - 1 || drawings.len().saturating_add(copies.len()) >= 500 {
                    return false;
                }
                next += 1;
                let copy = Drawing { id: next, group: None, link_next: None, ..d.clone() };
                *src = copy.inline_src();
                *width = Some(copy.w);
                *height = Some(copy.h);
                if alt.is_empty() {
                    alt.clone_from(&copy.alt);
                }
                seen.insert(next);
                copies.push(copy);
                true
            });
        });
        if !has_inline && !any_placeholder {
            return;
        }
        drawings.extend(copies);
        drawings.retain(|d| d.wrap != WrapStyle::Inline || seen.contains(&d.id));
        if let Some(sel) = self.selected_drawing
            && !self.body.drawings.iter().any(|d| d.id == sel)
        {
            self.select_drawing(None);
        }
    }

    /// The In Line with Text drawings a fragment of this body shows (copy): those whose placeholders it
    /// holds.
    pub(crate) fn fragment_drawings(&self, frag: &Doc) -> Vec<Drawing> {
        let ids = placeholder_ids(&frag.blocks);
        self.body
            .drawings
            .iter()
            .filter(|d| d.wrap == WrapStyle::Inline && ids.contains(&d.id))
            .cloned()
            .collect()
    }

    /// Takes in the In Line with Text drawings a pasted fragment brings, each under a new id (its
    /// placeholders in `frag` are pointed at it). Floating drawings of a fragment are not pasted.
    pub(crate) fn adopt_fragment_drawings(&mut self, frag: &mut Doc) {
        let incoming = std::mem::take(&mut frag.drawings);
        for d in incoming.into_iter().filter(|d| d.wrap == WrapStyle::Inline) {
            if self.body.drawings.len() >= 500 {
                break;
            }
            let id = self.body.drawings.iter().map(|d| d.id).max().map_or(1, |m| m.saturating_add(1));
            repoint_placeholders(&mut frag.blocks, d.id, id);
            self.body.drawings.push(Drawing { id, group: None, link_next: None, canvas: None, ..d });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_model::ParaStyle;

    fn placeholder(id: u32) -> Inline {
        Inline::Image { src: format!("drawing:{id}"), alt: String::new(), width: None, height: None }
    }

    fn para(inlines: Vec<Inline>) -> Block {
        Block::Paragraph { style: ParaStyle::default(), inlines }
    }

    #[test]
    fn placeholders_and_inline_drawings_are_kept_in_step() {
        let mut c = ComposeState::default();
        let inline = |id| Drawing { id, wrap: WrapStyle::Inline, w: 40.0, h: 30.0, ..Drawing::default() };
        c.body.drawings = vec![inline(1), inline(2), Drawing { id: 3, ..Drawing::default() }];
        // Drawing 1 is shown twice, 2 lost its placeholder, 9 never existed.
        c.body.blocks = vec![para(vec![
            placeholder(1),
            Inline::Text { text: "x".into(), style: Default::default() },
            placeholder(1),
            placeholder(9),
        ])];
        c.select_drawing(Some(2));
        c.sync_inline_drawings();
        let ids: Vec<u32> = c.body.drawings.iter().map(|d| d.id).collect();
        assert_eq!(ids, vec![1, 3, 4], "2 went with its placeholder; the copy of 1 is 4");
        assert_eq!(placeholder_ids(&c.body.blocks), vec![1, 4]);
        assert_eq!(c.selected_drawing, None);
        let Block::Paragraph { inlines, .. } = &c.body.blocks[0] else { panic!() };
        assert!(matches!(&inlines[0], Inline::Image { width: Some(w), .. } if *w == 40.0));
    }

    #[test]
    fn selection_lists_the_picked_drawing_first() {
        let mut c = ComposeState::default();
        c.body.drawings = (1..=3).map(|id| Drawing { id, ..Drawing::default() }).collect();
        c.selected_drawing = Some(2);
        c.also_selected = vec![1, 2, 7];
        assert_eq!(c.selected_drawings(), vec![2, 1]);
        c.select_drawing(Some(3));
        assert_eq!(c.selected_drawings(), vec![3]);
    }
}
