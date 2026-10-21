//! Insert › Table › Draw Table in the body: the table pencil and eraser. A pencil stroke outside a table
//! draws a new one-cell table as wide as the stroke; a stroke across a cell splits it (an up-and-down stroke
//! into two columns, a sideways one into two rows), so tables get irregular cells; the eraser merges a cell
//! with its neighbour to the right (or, failing that, below). Cells are placed on the grid the HTML table
//! model uses (spans skip taken slots), so splits and merges keep every row and column lined up.

use mailcraft_model::Block;
use mailcraft_model::rich::TableCell;

use crate::compose::{ComposeState, DocPos, table_cell};

/// Most grid columns a drawn table gets.
const MAX_COLS: usize = super::MAX_TABLE_COLS;
/// Most rows a drawn table gets.
const MAX_ROWS: usize = super::MAX_TABLE_ROWS;

/// Where a cell sits on the table grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    /// Row of the cell list and index in it.
    row: usize,
    idx: usize,
    /// First grid row and column, and the spans (clamped to the table).
    r0: usize,
    c0: usize,
    rs: usize,
    cs: usize,
}

/// The grid placement of every cell.
fn grid(rows: &[Vec<TableCell>]) -> Vec<Slot> {
    let n = rows.len();
    let mut taken: Vec<Vec<bool>> = vec![Vec::new(); n];
    let mut out = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        let mut c = 0usize;
        for (ci, cell) in row.iter().enumerate() {
            while taken.get(ri).and_then(|r| r.get(c)).copied().unwrap_or(false) {
                c += 1;
            }
            let cs = usize::try_from(cell.colspan.max(1)).unwrap_or(1).min(MAX_COLS);
            let rs = usize::try_from(cell.rowspan.max(1)).unwrap_or(1).min(n - ri);
            for r in ri..ri + rs {
                if let Some(t) = taken.get_mut(r) {
                    if t.len() < c + cs {
                        t.resize(c + cs, false);
                    }
                    for v in t.iter_mut().skip(c).take(cs) {
                        *v = true;
                    }
                }
            }
            out.push(Slot { row: ri, idx: ci, r0: ri, c0: c, rs, cs });
            c += cs;
            if c > MAX_COLS * 4 {
                break;
            }
        }
    }
    out
}

fn columns(slots: &[Slot]) -> usize {
    slots.iter().map(|s| s.c0 + s.cs).max().unwrap_or(0)
}

fn cell_mut(rows: &mut [Vec<TableCell>], s: Slot) -> Option<&mut TableCell> {
    rows.get_mut(s.row)?.get_mut(s.idx)
}

fn empty_like(cell: &TableCell, colspan: usize, rowspan: usize) -> TableCell {
    TableCell {
        colspan: u32::try_from(colspan).unwrap_or(1),
        rowspan: u32::try_from(rowspan).unwrap_or(1),
        background: cell.background,
        ..table_cell("")
    }
}

/// Splits cell `idx` of row `row` into two: side by side (`vertical`) or one above the other.
pub fn split(rows: &mut Vec<Vec<TableCell>>, row: usize, idx: usize, vertical: bool) -> bool {
    let slots = grid(rows);
    let Some(x) = slots.iter().copied().find(|s| s.row == row && s.idx == idx) else { return false };
    let Some(cell) = rows.get(row).and_then(|r| r.get(idx)).cloned() else { return false };
    if vertical {
        let half = cell.width_px.map(|w| (w / 2.0).round());
        if x.cs >= 2 {
            let k = x.cs.div_ceil(2);
            let mut new = empty_like(&cell, x.cs - k, x.rs);
            new.width_px = half;
            if let Some(c) = cell_mut(rows, x) {
                c.colspan = u32::try_from(k).unwrap_or(1);
                c.width_px = half;
            }
            if let Some(r) = rows.get_mut(row) {
                r.insert(idx + 1, new);
            }
            return true;
        }
        if columns(&slots) >= MAX_COLS {
            return false;
        }
        // A new grid column: every other cell over this column widens by one.
        for s in slots.iter().filter(|s| **s != x && s.c0 <= x.c0 && x.c0 < s.c0 + s.cs) {
            if let Some(c) = cell_mut(rows, *s) {
                c.colspan = c.colspan.saturating_add(1);
            }
        }
        let mut new = empty_like(&cell, 1, x.rs);
        new.width_px = half;
        if let Some(c) = cell_mut(rows, x) {
            c.width_px = half;
        }
        if let Some(r) = rows.get_mut(row) {
            r.insert(idx + 1, new);
        }
        return true;
    }
    if x.rs >= 2 {
        let k = x.rs.div_ceil(2);
        let target = x.r0 + k;
        let new = empty_like(&cell, x.cs, x.rs - k);
        if let Some(c) = cell_mut(rows, x) {
            c.rowspan = u32::try_from(k).unwrap_or(1);
        }
        // Into the row below, before the first cell that starts to its right.
        let at = slots.iter().filter(|s| s.row == target && s.c0 < x.c0).count();
        if let Some(r) = rows.get_mut(target) {
            r.insert(at.min(r.len()), new);
        }
        return true;
    }
    if rows.len() >= MAX_ROWS {
        return false;
    }
    // A new grid row under this one: every other cell across this row grows down by one.
    for s in slots.iter().filter(|s| **s != x && s.r0 <= x.r0 && x.r0 < s.r0 + s.rs) {
        if let Some(c) = cell_mut(rows, *s) {
            c.rowspan = c.rowspan.saturating_add(1);
        }
    }
    let mut new = empty_like(&cell, x.cs, 1);
    new.width_px = cell.width_px;
    rows.insert(row + 1, vec![new]);
    true
}

/// Merges cell `idx` of row `row` with the cell to its right (same rows), else with the one below it
/// (same columns). Their content is kept, one after the other.
pub fn merge(rows: &mut [Vec<TableCell>], row: usize, idx: usize) -> bool {
    let slots = grid(rows);
    let Some(x) = slots.iter().copied().find(|s| s.row == row && s.idx == idx) else { return false };
    let right = slots.iter().copied().find(|s| s.r0 == x.r0 && s.rs == x.rs && s.c0 == x.c0 + x.cs);
    let below = slots.iter().copied().find(|s| s.c0 == x.c0 && s.cs == x.cs && s.r0 == x.r0 + x.rs);
    let (other, sideways) = match (right, below) {
        (Some(r), _) => (r, true),
        (None, Some(b)) => (b, false),
        (None, None) => return false,
    };
    let Some(gone) = rows.get(other.row).and_then(|r| r.get(other.idx)).cloned() else { return false };
    if let Some(c) = cell_mut(rows, x) {
        if sideways {
            c.colspan = c.colspan.saturating_add(gone.colspan.max(1));
            c.width_px = match (c.width_px, gone.width_px) {
                (Some(a), Some(b)) => Some(a + b),
                _ => None,
            };
        } else {
            c.rowspan = c.rowspan.saturating_add(gone.rowspan.max(1));
        }
        let has_text = |b: &Block| !matches!(b, Block::Paragraph { inlines, .. } if inlines.is_empty());
        if gone.blocks.iter().any(has_text) {
            c.blocks.retain(has_text);
            c.blocks.extend(gone.blocks);
            if c.blocks.is_empty() {
                c.blocks.push(Block::text_paragraph(""));
            }
        }
    }
    if let Some(r) = rows.get_mut(other.row)
        && other.idx < r.len()
    {
        r.remove(other.idx);
    }
    true
}

/// The rows of the table at `path` (a top-level or nested table block).
fn table_rows<'a>(blocks: &'a mut [Block], path: &[usize]) -> Option<&'a mut Vec<Vec<TableCell>>> {
    let (&first, rest) = path.split_first()?;
    let b = blocks.get_mut(first)?;
    if rest.is_empty() {
        return match b {
            Block::Table { rows, .. } => Some(rows),
            _ => None,
        };
    }
    match b {
        Block::List { items, .. } => {
            let (&it, rest) = rest.split_first()?;
            table_rows(items.get_mut(it)?, rest)
        }
        Block::Quote { blocks } | Block::Container { blocks, .. } => table_rows(blocks, rest),
        Block::Table { rows, .. } => {
            let (&r, rest) = rest.split_first()?;
            let (&c, rest) = rest.split_first()?;
            table_rows(&mut rows.get_mut(r)?.get_mut(c)?.blocks, rest)
        }
        _ => None,
    }
}

/// What a pencil stroke or eraser click did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drawn {
    NewTable,
    Split,
    Merged,
    Nothing,
}

/// A table pencil stroke that started at `at` and went (`dx`, `dy`) px; with `erase`, an eraser click at
/// `at`. One undo step when something changed.
pub fn stroke(c: &mut ComposeState, at: DocPos, dx: f32, dy: f32, erase: bool) -> Drawn {
    if !c.leaf_paths().contains(&at.path) {
        return Drawn::Nothing;
    }
    let before = c.selection.clone();
    c.set_caret(at);
    let cell = c.cell_at_caret();
    let (dx, dy) = (if dx.is_finite() { dx } else { 0.0 }, if dy.is_finite() { dy } else { 0.0 });
    match cell {
        Some((table, row, col)) => {
            if !erase && dx.abs().max(dy.abs()) < 8.0 {
                c.selection = before;
                return Drawn::Nothing;
            }
            let mut body = c.body.clone();
            let Some(rows) = table_rows(&mut body.blocks, &table) else { return Drawn::Nothing };
            let done = if erase { merge(rows, row, col) } else { split(rows, row, col, dy.abs() > dx.abs()) };
            if !done {
                return Drawn::Nothing;
            }
            c.checkpoint();
            c.body = body;
            // The caret goes back to the start of the cell it was in (paths may have moved).
            let start = c.body_start();
            c.set_caret(DocPos::new(table.iter().copied().chain([row, col, 0]).collect(), 0));
            if !c.leaf_paths().contains(&c.selection.focus.path) {
                c.set_caret(start);
            }
            c.dirty = true;
            if erase { Drawn::Merged } else { Drawn::Split }
        }
        None if erase => Drawn::Nothing,
        None => {
            let w = dx.abs();
            if w < 24.0 {
                c.selection = before;
                return Drawn::Nothing;
            }
            let mut cell = table_cell("");
            cell.width_px = Some(w.round().clamp(24.0, 2000.0));
            c.checkpoint();
            c.insert_blocks(vec![Block::Table {
                borders: None,
                rows: vec![vec![cell]],
                border: true,
                width_px: Some(w.round().clamp(24.0, 2000.0)),
            }]);
            c.dirty = true;
            Drawn::NewTable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(rows: usize, cols: usize) -> Vec<Vec<TableCell>> {
        (0..rows).map(|_| (0..cols).map(|_| table_cell("")).collect()).collect()
    }

    fn shape(rows: &[Vec<TableCell>]) -> Vec<Vec<(u32, u32)>> {
        rows.iter().map(|r| r.iter().map(|c| (c.colspan, c.rowspan)).collect()).collect()
    }

    #[test]
    fn splitting_a_cell_keeps_the_grid_lined_up() {
        let mut rows = t(2, 2);
        assert!(split(&mut rows, 0, 0, true), "two columns out of one");
        assert_eq!(shape(&rows), vec![vec![(1, 1), (1, 1), (1, 1)], vec![(2, 1), (1, 1)]]);
        assert_eq!(columns(&grid(&rows)), 3);
        assert!(split(&mut rows, 1, 0, true), "a two-column cell splits within its span");
        assert_eq!(shape(&rows)[1], vec![(1, 1), (1, 1), (1, 1)]);
        let mut rows = t(2, 2);
        assert!(split(&mut rows, 0, 1, false), "two rows out of one");
        assert_eq!(shape(&rows), vec![vec![(1, 2), (1, 1)], vec![(1, 1)], vec![(1, 1), (1, 1)]]);
        let g = grid(&rows);
        assert!(g.iter().any(|s| s.row == 1 && s.c0 == 1), "the new cell sits under the split one");
        assert!(split(&mut rows, 0, 0, false), "a two-row cell splits within its span");
        assert_eq!(shape(&rows), vec![vec![(1, 1), (1, 1)], vec![(1, 1), (1, 1)], vec![(1, 1), (1, 1)]]);
    }

    #[test]
    fn erasing_merges_with_the_neighbour() {
        let mut rows = t(2, 3);
        if let Some(c) = rows[0].get_mut(1) {
            c.blocks = vec![Block::text_paragraph("kept")];
        }
        assert!(merge(&mut rows, 0, 0));
        assert_eq!(shape(&rows)[0], vec![(2, 1), (1, 1)]);
        assert_eq!(Block::text_paragraph("kept"), rows[0][0].blocks[0]);
        // The last cell of a row merges with the one below it.
        assert!(merge(&mut rows, 0, 1));
        assert_eq!(shape(&rows), vec![vec![(2, 1), (1, 2)], vec![(1, 1), (1, 1)]]);
        assert!(!merge(&mut rows, 1, 1), "nothing to its right or below");
        assert!(!merge(&mut rows, 9, 0) && !split(&mut rows, 0, 9, true));
    }

    #[test]
    fn hostile_spans_do_not_crash() {
        let mut rows = t(2, 2);
        rows[0][0].colspan = u32::MAX;
        rows[0][1].rowspan = u32::MAX;
        let _ = grid(&rows);
        let _ = split(&mut rows, 0, 0, true);
        let _ = split(&mut rows, 0, 1, false);
        let _ = merge(&mut rows, 1, 0);
        let mut empty: Vec<Vec<TableCell>> = Vec::new();
        assert!(!split(&mut empty, 0, 0, true));
    }
}
