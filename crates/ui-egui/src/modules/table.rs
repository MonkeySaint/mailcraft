//! Module tables (People › Phone and List, the Tasks views): the column layout, the header (a click sorts by
//! the column, a second click reverses it) and the cells the tables share: Flag Status and Categories, which
//! take a click (the Quick Click flag and category), and the row font from Conditional Formatting.
//!
//! The columns, their order, the sort field and the rules come from the engine
//! (`mailcraft_engine::cmd::view::table`).

use egui::{Color32, CornerRadius, FontId, Id, Rect, Sense, Stroke, Ui, Vec2, WidgetType, pos2, vec2};
use serde_json::json;

use mailcraft_engine::cmd::view::table::{ModuleRowStyle, ShownColumn, SortField, TableColumn};

use super::common::{a11y, paint_text_clipped};
use crate::App;
use crate::list::{Look, Text};

/// Narrowest a column sharing the free space gets before others are dropped.
const FLEX_MIN: f32 = 120.0;
/// Narrowest a fixed text column is squeezed to.
const FIXED_MIN: f32 = 48.0;
/// Columns this narrow or narrower hold a glyph and never shrink.
const GLYPH_W: u16 = 30;

/// A column placed in the table: its label and alignment (Format Columns), x offset from the table's left
/// edge and width.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Laid {
    pub col: &'static TableColumn,
    pub label: String,
    pub align: &'static str,
    pub x: f32,
    pub w: f32,
}

/// Places `cols` in `width` px: glyph columns keep their width, fixed text columns take their preferred
/// width (squeezed down to a minimum when needed), the free space goes to the flex columns (Full Name,
/// Subject). When even that does not fit, columns that are not essential are dropped from the right.
pub(crate) fn layout(width: f32, cols: &[ShownColumn]) -> Vec<Laid> {
    let width = width.max(0.0);
    let mut shown: Vec<&'static TableColumn> = cols.iter().map(|c| c.col).collect();
    // A fixed text column is squeezed to 60 % of its width (at least FIXED_MIN) before columns are dropped.
    let min_w = |c: &TableColumn| (f32::from(c.width) * 0.6).max(FIXED_MIN);
    loop {
        let glyph: f32 =
            shown.iter().filter(|c| c.width > 0 && c.width <= GLYPH_W).map(|c| f32::from(c.width)).sum();
        let text: f32 = shown.iter().filter(|c| c.width > GLYPH_W).map(|c| f32::from(c.width)).sum();
        let text_min: f32 = shown.iter().filter(|c| c.width > GLYPH_W).map(|c| min_w(c)).sum();
        let flex_n = shown.iter().filter(|c| c.width == 0).count() as f32;
        let room = width - glyph - flex_n * FLEX_MIN;
        if room < text_min
            && let Some(pos) = shown.iter().rposition(|c| !c.essential)
        {
            // Drop the right-most column that is not essential and try again.
            shown.remove(pos);
            continue;
        }
        let scale = if text <= 0.0 || room >= text { 1.0 } else { (room / text).max(0.0) };
        let fixed: Vec<f32> = shown
            .iter()
            .map(|c| match c.width {
                0 => 0.0,
                w if w <= GLYPH_W => f32::from(w),
                w => (f32::from(w) * scale).max(min_w(c)),
            })
            .collect();
        let used: f32 = fixed.iter().sum();
        let flex_w = if flex_n > 0.0 { ((width - used) / flex_n).max(FLEX_MIN.min(width)) } else { 0.0 };
        let mut x = 0.0;
        let mut out = Vec::with_capacity(shown.len());
        for (c, w) in shown.into_iter().zip(fixed) {
            let w = if c.width == 0 { flex_w } else { w };
            let (label, align) = cols
                .iter()
                .find(|s| s.col.id == c.id)
                .map_or((c.label.to_owned(), "left"), |s| (s.label.clone(), s.align));
            out.push(Laid { col: c, label, align, x, w });
            x += w;
        }
        return out;
    }
}

/// Header height.
pub(crate) const HEAD_H: f32 = 24.0;

/// Draws the column header across `rect`; returns the column clicked (to sort by it).
pub(crate) fn header(
    ui: &mut Ui,
    app: &mut App,
    rect: Rect,
    laid: &[Laid],
    sort: Option<&SortField>,
    salt: &str,
) -> Option<&'static str> {
    let mut clicked = None;
    for l in laid {
        let cell = Rect::from_min_size(pos2(rect.min.x + l.x, rect.min.y), vec2(l.w.max(1.0), rect.height()));
        let resp = ui.interact(cell, Id::new((salt, "col", l.col.id)), Sense::click());
        let sorted = sort.filter(|f| f.column == l.col.id);
        let state = match sorted {
            Some(f) if f.descending => ", sorted descending",
            Some(_) => ", sorted ascending",
            None => "",
        };
        a11y(&resp, WidgetType::Button, &format!("{}{state}", l.label));
        if resp.hovered() {
            ui.painter().rect_filled(cell, CornerRadius::same(2), app.theme.hover);
        }
        let glyph = l.col.width > 0 && l.col.width <= GLYPH_W;
        let text = if glyph { "" } else { l.label.as_str() };
        let icon = match l.col.id {
            "flagStatus" => Some("flag"),
            "icon" => Some("view"),
            "complete" => Some("check"),
            _ => None,
        };
        if let Some(icon) = icon.filter(|_| glyph) {
            app.icons.paint(
                ui,
                Rect::from_center_size(cell.center(), Vec2::splat(12.0)),
                icon,
                app.theme.text_muted,
            );
        } else {
            paint_text_clipped(
                ui,
                pos2(cell.min.x + 4.0, cell.center().y - 7.0),
                text,
                FontId::proportional(11.5),
                app.theme.text_muted,
                l.w - (if sorted.is_some() { 18.0 } else { 6.0 }),
            );
        }
        if let Some(f) = sorted {
            let arrow = Rect::from_center_size(pos2(cell.max.x - 8.0, cell.center().y), Vec2::splat(10.0));
            app.icons.paint(
                ui,
                arrow,
                if f.descending { "chevron-down" } else { "chevron-up" },
                app.theme.text_muted,
            );
        }
        if l.x > 0.0 {
            ui.painter().vline(cell.min.x, cell.y_range().shrink(5.0), Stroke::new(1.0, app.theme.separator));
        }
        if resp.clicked() {
            clicked = Some(l.col.id);
        }
    }
    ui.painter().hline(rect.x_range(), rect.max.y - 1.0, Stroke::new(1.0, app.theme.separator));
    clicked
}

/// A header click: sort by the column, or reverse the sort when it already sorts by it.
pub(crate) fn sort_by_column(app: &mut App, sort: Option<&SortField>, col: &str) {
    let dir = match sort {
        Some(f) if f.column == col && !f.descending => "desc",
        _ => "asc",
    };
    app.run("view.sortBy", json!({ "field": col, "dir": dir }));
}

/// How a flag cell looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlagLook {
    None,
    Flagged,
    Complete,
}

/// A clickable Flag Status cell (announced as `label`); returns true when clicked. `hovered` shows the
/// outline flag on an unflagged row so the click target can be found.
pub(crate) fn flag_cell(
    ui: &mut Ui,
    app: &mut App,
    cell: Rect,
    look: FlagLook,
    hovered: bool,
    salt: (&str, u64),
    label: &str,
) -> bool {
    let target = Rect::from_center_size(cell.center(), vec2(cell.width().min(26.0), cell.height().min(24.0)));
    let resp = ui.interact(target, Id::new(("mc-table-flag", salt.0, salt.1)), Sense::click());
    let tip = match look {
        FlagLook::None => "Flag (Quick Click)",
        FlagLook::Flagged => "Mark complete",
        FlagLook::Complete => "Clear flag",
    };
    a11y(&resp, WidgetType::Button, label);
    let icon = Rect::from_center_size(cell.center(), Vec2::splat(14.0));
    match look {
        FlagLook::Flagged => app.icons.paint(ui, icon, "flag", app.theme.flag),
        FlagLook::Complete => app.icons.paint(ui, icon, "flag-complete", app.theme.complete),
        FlagLook::None if hovered || resp.hovered() => {
            app.icons.paint(ui, icon, "flag", app.theme.text_muted.gamma_multiply(0.6));
        }
        FlagLook::None => {}
    }
    if resp.hovered() {
        ui.painter().rect_stroke(
            target.shrink(1.0),
            3.0,
            Stroke::new(1.0, app.theme.separator),
            egui::StrokeKind::Inside,
        );
    }
    resp.on_hover_text(tip).clicked()
}

/// A clickable Categories cell (colour blocks and names); a click applies the Quick Click category.
pub(crate) fn categories_cell(
    ui: &mut Ui,
    app: &mut App,
    cell: Rect,
    cats: &[String],
    hovered: bool,
    salt: (&str, u64),
) -> bool {
    let resp =
        ui.interact(cell.shrink2(vec2(1.0, 2.0)), Id::new(("mc-table-cats", salt.0, salt.1)), Sense::click());
    a11y(&resp, WidgetType::Button, if cats.is_empty() { "No Categories" } else { "Categories" });
    let mut x = cell.min.x + 4.0;
    for name in cats.iter().take(3) {
        let color = mailcraft_model::Category::find(&app.s.mailbox.categories, name)
            .map(|c| c.color)
            .filter(|c| *c != mailcraft_model::CategoryColor::None)
            .map(|c| super::common::geom_color(c.rgb()));
        if let Some(color) = color {
            let block = Rect::from_min_size(pos2(x, cell.center().y - 6.0), vec2(8.0, 12.0));
            ui.painter().rect_filled(block, 2.0, color);
            x += 11.0;
        }
    }
    if cats.is_empty() {
        if hovered || resp.hovered() {
            app.icons.paint(
                ui,
                Rect::from_center_size(pos2(x + 6.0, cell.center().y), Vec2::splat(13.0)),
                "category-tag",
                app.theme.text_muted,
            );
        }
    } else {
        paint_text_clipped(
            ui,
            pos2(x + 2.0, cell.center().y - 7.0),
            &cats.join(", "),
            FontId::proportional(12.0),
            app.theme.text_muted,
            (cell.max.x - x - 6.0).max(4.0),
        );
    }
    if resp.hovered() {
        ui.painter().rect_stroke(
            cell.shrink(1.0),
            3.0,
            Stroke::new(1.0, app.theme.separator),
            egui::StrokeKind::Inside,
        );
    }
    resp.on_hover_text("Quick Click category").clicked()
}

/// Text colour and font effects of a row: Conditional Formatting, with the built-in Overdue / Completed
/// rules taking the Options › Tasks colours (`overdue`, `done`).
pub(crate) fn row_look(
    style: &ModuleRowStyle,
    base: Color32,
    overdue: Color32,
    done: Color32,
) -> (Color32, Look) {
    let color = if let Some(hex) = style.font.color.as_deref() {
        Color32::from_hex(hex.trim()).unwrap_or(base)
    } else if style.completed {
        done
    } else if style.overdue {
        overdue
    } else {
        base
    };
    let look = Look {
        bold: style.font.bold,
        italic: style.font.italic,
        underline: style.font.underline,
        strike: style.font.strike,
    };
    (color, look)
}

/// Paints one line of styled text in a cell, clipped to it and aligned as Format Columns says.
pub(crate) fn styled_cell(
    ui: &Ui,
    cell: Rect,
    text: &str,
    size: f32,
    look: Look,
    color: Color32,
    align: &str,
) {
    let t = Text::styled(ui, text, size, look, color, (cell.width() - 8.0).max(1.0), 1, None);
    let x = match align {
        "right" => (cell.max.x - 4.0 - t.width()).max(cell.min.x + 4.0),
        "center" => (cell.center().x - t.width() / 2.0).max(cell.min.x + 4.0),
        _ => cell.min.x + 4.0,
    };
    t.paint_left(ui.painter(), cell, x);
}

/// The glyph of the Icon column.
pub(crate) fn paint_icon(ui: &Ui, app: &mut App, cell: Rect, icon: &str) {
    app.icons.paint(ui, Rect::from_center_size(cell.center(), Vec2::splat(14.0)), icon, app.theme.text_muted);
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcraft_engine::cmd::view::table::{CONTACT_COLUMNS, TASK_COLUMNS};

    fn cols(all: &'static [TableColumn], ids: &[&str]) -> Vec<ShownColumn> {
        ids.iter()
            .filter_map(|id| all.iter().find(|c| c.id == *id))
            .map(|col| ShownColumn { col, label: col.label.to_owned(), align: "left" })
            .collect()
    }

    #[test]
    fn layout_fills_the_width_and_drops_extras_when_narrow() {
        let c = cols(&TASK_COLUMNS, &["icon", "complete", "subject", "status", "dueDate", "flagStatus"]);
        let wide = layout(900.0, &c);
        assert_eq!(wide.len(), 6);
        let end = wide.last().map(|l| l.x + l.w).unwrap();
        assert!((end - 900.0).abs() < 1.0, "the subject takes the free space: {end}");
        assert!(wide.iter().any(|l| l.col.id == "subject" && l.w > 300.0));
        // Narrow: Status (not essential) goes first; the essentials stay.
        let narrow = layout(300.0, &c);
        assert!(narrow.iter().all(|l| l.col.essential));
        assert!(narrow.iter().any(|l| l.col.id == "flagStatus"));
        // Fixed text columns squeeze before anything is dropped.
        let p = cols(&CONTACT_COLUMNS, &["icon", "fullName", "company", "fileAs", "email", "flagStatus"]);
        let mid = layout(560.0, &p);
        assert_eq!(mid.len(), 6);
        assert!(mid.iter().all(|l| l.w >= FIXED_MIN - 0.1 || l.col.width <= GLYPH_W));
        assert!(layout(0.0, &p).iter().all(|l| l.w.is_finite()));
        assert!(layout(500.0, &[]).is_empty());
    }
}
