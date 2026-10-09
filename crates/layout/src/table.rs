//! Tables: grid placement (colspan / rowspan), automatic column widths from min/max-content widths,
//! row heights, cell backgrounds and borders.

use mailcraft_geom::{Color, Rect};
use mailcraft_model::TextStyle;
use mailcraft_model::rich::TableCell;

use crate::DisplayItem;
use crate::block::{Ctx, Frame, Out, blocks_min_max, layout_blocks};

/// Columns beyond this are dropped.
pub(crate) const MAX_COLS: usize = 128;
/// Rows beyond this are dropped.
pub(crate) const MAX_ROWS: usize = 4096;
const MAX_ROWSPAN: usize = 1024;
/// Total grid slots that spanning cells may claim before further spans are ignored.
const SPAN_BUDGET: usize = 1 << 21;
/// Cell padding (px at 100 %).
pub(crate) const CELL_PADDING: f32 = 4.0;
pub(crate) const BORDER_COLOR: Color = Color::hex(0x808080);

/// A cell positioned in the grid.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Placed<'a> {
    pub row: usize,
    pub col: usize,
    pub rowspan: usize,
    pub colspan: usize,
    pub cell: &'a TableCell,
    /// Indices in the model (`rows[ri][ci]`), used for block paths.
    pub ri: usize,
    pub ci: usize,
}

/// Places cells in the grid following the HTML table model (cells skip slots taken by earlier rowspans).
pub(crate) fn place(rows: &[Vec<TableCell>]) -> (Vec<Placed<'_>>, usize, usize) {
    let nrows = rows.len().min(MAX_ROWS);
    let mut occ: Vec<Vec<bool>> = vec![Vec::new(); nrows];
    let mut budget = SPAN_BUDGET;
    let mut placed = Vec::new();
    let mut ncols = 0usize;
    for (ri, row) in rows.iter().take(nrows).enumerate() {
        let mut c = 0usize;
        for (ci, cell) in row.iter().enumerate() {
            while occ.get(ri).and_then(|r| r.get(c)).copied().unwrap_or(false) {
                c = c.saturating_add(1);
            }
            if c >= MAX_COLS {
                break;
            }
            let mut cs = usize::try_from(cell.colspan.max(1)).unwrap_or(1).min(MAX_COLS - c);
            let mut rs = usize::try_from(cell.rowspan.max(1)).unwrap_or(1).min(MAX_ROWSPAN).min(nrows - ri);
            let cost = cs.saturating_mul(rs);
            if cost > budget {
                cs = 1;
                rs = 1;
            }
            budget = budget.saturating_sub(cs.saturating_mul(rs));
            for rr in ri..ri.saturating_add(rs) {
                if let Some(r) = occ.get_mut(rr) {
                    let need = c.saturating_add(cs);
                    if r.len() < need {
                        r.resize(need, false);
                    }
                    for slot in r.iter_mut().skip(c).take(cs) {
                        *slot = true;
                    }
                }
            }
            placed.push(Placed { row: ri, col: c, rowspan: rs, colspan: cs, cell, ri, ci });
            ncols = ncols.max(c.saturating_add(cs));
            c = c.saturating_add(cs);
        }
    }
    (placed, ncols, nrows)
}

fn sum_range(v: &[f32], from: usize, len: usize) -> f32 {
    v.iter().skip(from).take(len).sum()
}

/// Per-column min and max widths (padding included).
fn columns(
    ctx: &Ctx<'_>,
    placed: &[Placed<'_>],
    ncols: usize,
    ts: &TextStyle,
    depth: usize,
) -> (Vec<f32>, Vec<f32>) {
    let pad = ctx.px(CELL_PADDING) * 2.0;
    let mut mins = vec![0.0f32; ncols];
    let mut maxs = vec![0.0f32; ncols];
    let mut spanning: Vec<(usize, usize, f32, f32)> = Vec::new();
    for p in placed {
        let (mut mn, mut mx) = blocks_min_max(ctx, &p.cell.blocks, ts, depth);
        mn += pad;
        mx += pad;
        if let Some(w) = p.cell.width_px.map(|w| ctx.px(w)).filter(|w| *w > 0.0) {
            mx = mn.max(w);
        }
        if p.colspan == 1 {
            if let Some(m) = mins.get_mut(p.col) {
                *m = m.max(mn);
            }
            if let Some(m) = maxs.get_mut(p.col) {
                *m = m.max(mx);
            }
        } else {
            spanning.push((p.col, p.colspan, mn, mx));
        }
    }
    spanning.sort_by_key(|s| s.1);
    for (col, span, mn, mx) in spanning {
        let n = span.max(1) as f32;
        let have = sum_range(&mins, col, span);
        if mn > have {
            let add = (mn - have) / n;
            for m in mins.iter_mut().skip(col).take(span) {
                *m += add;
            }
        }
        let have = sum_range(&maxs, col, span);
        if mx > have {
            let add = (mx - have) / n;
            for m in maxs.iter_mut().skip(col).take(span) {
                *m += add;
            }
        }
    }
    for (mx, mn) in maxs.iter_mut().zip(&mins) {
        if *mx < *mn {
            *mx = *mn;
        }
    }
    (mins, maxs)
}

/// Column widths for `target` px. `fill` stretches the columns to the full target (explicit table width).
pub(crate) fn distribute(mins: &[f32], maxs: &[f32], target: f32, fill: bool) -> Vec<f32> {
    let target = if target.is_finite() { target.max(0.0) } else { 0.0 };
    let sum_min: f32 = mins.iter().sum();
    let sum_max: f32 = maxs.iter().sum();
    let n = mins.len().max(1) as f32;
    let mut widths: Vec<f32> = if sum_max <= target {
        let extra = if fill { target - sum_max } else { 0.0 };
        maxs.iter().map(|mx| if sum_max > 0.0 { mx + extra * mx / sum_max } else { mx + extra / n }).collect()
    } else if sum_min < target {
        let t = (target - sum_min) / (sum_max - sum_min).max(f32::EPSILON);
        mins.iter().zip(maxs).map(|(mn, mx)| mn + (mx - mn) * t.clamp(0.0, 1.0)).collect()
    } else if sum_min > 0.0 {
        mins.iter().map(|mn| mn * target / sum_min).collect()
    } else {
        vec![0.0; mins.len()]
    };
    // Guard against float drift: never exceed the target.
    let total: f32 = widths.iter().sum();
    if total > target && total > 0.0 {
        let s = target / total * (1.0 - 1.0e-6);
        for w in &mut widths {
            *w *= s;
        }
    }
    widths
}

pub(crate) fn table_min_max(
    ctx: &Ctx<'_>,
    rows: &[Vec<TableCell>],
    _border: bool,
    width_px: Option<f32>,
    ts: &TextStyle,
    depth: usize,
) -> (f32, f32) {
    let (placed, ncols, _) = place(rows);
    let (mins, maxs) = columns(ctx, &placed, ncols, ts, depth);
    let mn: f32 = mins.iter().sum();
    let mut mx: f32 = maxs.iter().sum();
    if let Some(w) = width_px.map(|w| ctx.px(w)).filter(|w| *w > 0.0) {
        mx = mn.max(w);
    }
    (mn, mx)
}

pub(crate) fn layout_table(
    ctx: &Ctx<'_>,
    rows: &[Vec<TableCell>],
    border: bool,
    width_px: Option<f32>,
    ts: &TextStyle,
    path: &mut Vec<usize>,
    frame: Frame,
    out: &mut Out,
) -> f32 {
    let (placed, ncols, nrows) = place(rows);
    if placed.is_empty() {
        return 0.0;
    }
    let (mins, maxs) = columns(ctx, &placed, ncols, ts, frame.depth);
    let explicit = width_px.map(|w| ctx.px(w)).filter(|w| *w > 0.0);
    let target = explicit.map(|w| w.min(frame.w)).unwrap_or(frame.w);
    let widths = distribute(&mins, &maxs, target, explicit.is_some());
    let mut col_x = Vec::with_capacity(ncols);
    let mut acc = 0.0f32;
    for w in &widths {
        col_x.push(acc);
        acc += w;
    }
    let table_w = acc;
    let pad = ctx.px(CELL_PADDING);

    // Lay out every cell at the origin to learn its height.
    let mut laid: Vec<(Out, f32)> = Vec::with_capacity(placed.len());
    for p in &placed {
        let cell_w = sum_range(&widths, p.col, p.colspan);
        let inner_w = (cell_w - pad * 2.0).max(0.0);
        let mut tmp = Out::default();
        path.push(p.ri);
        path.push(p.ci);
        let inner = Frame { x: 0.0, y: 0.0, w: inner_w, depth: frame.depth.saturating_add(1), list_depth: 0 };
        let h = layout_blocks(ctx, &p.cell.blocks, ts, path, inner, &mut tmp);
        path.pop();
        path.pop();
        laid.push((tmp, h));
    }

    // Row heights: single-row cells first, then spanning cells top up their last row.
    let mut heights = vec![0.0f32; nrows];
    for (p, (_, h)) in placed.iter().zip(&laid) {
        if p.rowspan == 1
            && let Some(rh) = heights.get_mut(p.row)
        {
            *rh = rh.max(h + pad * 2.0);
        }
    }
    let mut spanning: Vec<(usize, &Placed<'_>, f32)> = placed
        .iter()
        .zip(&laid)
        .filter(|(p, _)| p.rowspan > 1)
        .map(|(p, (_, h))| (p.rowspan, p, *h))
        .collect();
    spanning.sort_by_key(|s| s.0);
    for (rs, p, h) in spanning {
        let need = h + pad * 2.0;
        let have = sum_range(&heights, p.row, rs);
        if need > have
            && let Some(last) = heights.get_mut(p.row.saturating_add(rs).saturating_sub(1))
        {
            *last += need - have;
        }
    }
    let mut row_y = Vec::with_capacity(nrows);
    let mut acc = 0.0f32;
    for h in &heights {
        row_y.push(acc);
        acc += h;
    }
    let table_h = acc;

    let stroke = ctx.px(1.0);
    // Borders go on top of every cell: a stroke straddles the cell edge, so a neighbour's background drawn
    // later would cover half of it.
    let mut borders: Vec<DisplayItem> = Vec::new();
    for (p, (content, h)) in placed.iter().zip(laid) {
        let cx = frame.x + col_x.get(p.col).copied().unwrap_or(0.0);
        let cy = frame.y + row_y.get(p.row).copied().unwrap_or(0.0);
        let cw = sum_range(&widths, p.col, p.colspan);
        let ch = sum_range(&heights, p.row, p.rowspan);
        let rect = Rect::new(cx, cy, cw, ch);
        if let Some(color) = p.cell.background {
            out.push(DisplayItem::FillRect { rect, color });
        }
        // Vertically centred, as in HTML's default for table cells.
        let free = (ch - pad * 2.0 - h).max(0.0);
        out.append(content, cx + pad, cy + pad + free / 2.0);
        if border {
            borders.push(DisplayItem::StrokeRect { rect, color: BORDER_COLOR, width: stroke });
        }
    }
    for b in borders {
        out.push(b);
    }
    if border {
        out.push(DisplayItem::StrokeRect {
            rect: Rect::new(frame.x, frame.y, table_w, table_h),
            color: BORDER_COLOR,
            width: stroke,
        });
    }
    table_h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(cs: u32, rs: u32) -> TableCell {
        TableCell { blocks: Vec::new(), colspan: cs, rowspan: rs, background: None, width_px: None }
    }

    #[test]
    fn placement_with_rowspan() {
        let rows = vec![vec![cell(1, 2), cell(1, 1)], vec![cell(1, 1)]];
        let (p, ncols, nrows) = place(&rows);
        assert_eq!((ncols, nrows), (2, 2));
        let second_row = p.iter().find(|p| p.ri == 1).map(|p| p.col);
        assert_eq!(second_row, Some(1));
    }

    #[test]
    fn placement_colspan_and_zero_spans() {
        let rows = vec![vec![cell(3, 0), cell(0, 1)]];
        let (p, ncols, _) = place(&rows);
        assert_eq!(ncols, 4);
        assert_eq!(p.get(1).map(|p| p.col), Some(3));
    }

    #[test]
    fn hostile_spans_are_capped() {
        let rows = vec![vec![cell(u32::MAX, u32::MAX); 300]; 3];
        let (_, ncols, nrows) = place(&rows);
        assert!(ncols <= MAX_COLS);
        assert_eq!(nrows, 3);
    }

    #[test]
    fn distribute_fits() {
        let w = distribute(&[10.0, 20.0], &[50.0, 100.0], 1000.0, false);
        assert_eq!(w, vec![50.0, 100.0]);
        let w = distribute(&[10.0, 20.0], &[50.0, 100.0], 90.0, false);
        assert!((w.iter().sum::<f32>() - 90.0).abs() < 1e-3);
        assert!(w.iter().zip([10.0, 20.0]).all(|(a, b)| *a >= b));
        let w = distribute(&[100.0, 100.0], &[200.0, 200.0], 50.0, false);
        assert!(w.iter().sum::<f32>() <= 50.0);
        let w = distribute(&[10.0, 10.0], &[20.0, 20.0], 100.0, true);
        assert!((w.iter().sum::<f32>() - 100.0).abs() < 1e-3);
        let w = distribute(&[0.0, 0.0], &[0.0, 0.0], 100.0, true);
        assert!((w.iter().sum::<f32>() - 100.0).abs() < 1e-3);
    }
}
