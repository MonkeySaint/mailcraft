//! Memo Style: a band with the user's name and a thick rule at the top of every page, then each item: the
//! header block (bold labels — From, Sent, To, Cc, Subject, Attachments — with wrapped values), a rule, and the
//! body laid out with `mailcraft-layout` across as many pages as it needs.

use mailcraft_fonts::FontDb;
use mailcraft_geom::Color;
use mailcraft_layout::{DisplayItem, LayoutOptions, layout_doc};
use mailcraft_model::Doc;

use super::flow::{self, Flow};
use super::text::{self, Style};
use super::{Frame, MAX_PAGES, PageSetup, PrintContext, hrule};

/// One printed item: header fields (label without the colon, value) and the body.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemoItem {
    pub fields: Vec<(String, String)>,
    pub body: Doc,
}

/// Name band text size (pt) and the rule under it (px).
const NAME_PT: f32 = 14.0;
const NAME_RULE: f32 = 3.0;
/// Header block text size (pt).
const FIELD_PT: f32 = 10.0;
/// Gap between the label column and the values.
const LABEL_GAP: f32 = 12.0;
/// Most header fields printed per item.
const MAX_FIELDS: usize = 32;
/// Most wrapped lines per header value.
const MAX_FIELD_LINES: usize = 20;
/// Space between items printed on the same page.
const ITEM_GAP: f32 = 28.0;

pub(crate) fn pages(
    items: &[MemoItem],
    setup: &PageSetup,
    ctx: &PrintContext,
    frame: &Frame,
    fonts: &FontDb,
) -> Vec<Vec<DisplayItem>> {
    let body = frame.body;
    let name = Style::new(fonts, NAME_PT, true);
    let label = Style::new(fonts, FIELD_PT, true);
    let value = Style::new(fonts, FIELD_PT, false);
    let line_h = value.line_height(fonts).max(label.line_height(fonts));

    // Band at the top of every page.
    let mut band = Vec::new();
    let mut band_h = 0.0;
    if !ctx.user_name.trim().is_empty() {
        let n = text::ellipsize(fonts, &name, &ctx.user_name, body.w);
        band.push(name.text_at(fonts, body.x, body.y, &n));
        band_h = name.line_height(fonts) + 2.0;
    }
    band.push(hrule(body.x, body.x + body.w, body.y + band_h, NAME_RULE, Color::BLACK));
    band_h += NAME_RULE + 12.0;

    let mut flow = Flow::default();
    for (i, item) in items.iter().take(MAX_PAGES).enumerate() {
        if i > 0 {
            if setup.new_page_per_item {
                flow.page_break();
            } else {
                flow.gap(ITEM_GAP);
            }
        }
        let fields: Vec<&(String, String)> = item.fields.iter().take(MAX_FIELDS).collect();
        let label_w = fields
            .iter()
            .map(|(l, _)| label.measure(fonts, &format!("{}:", text::clean(l))))
            .fold(0.0f32, f32::max)
            .min(body.w / 3.0);
        let value_x = label_w + LABEL_GAP;
        let value_w = (body.w - value_x).max(body.w / 3.0);
        for (l, v) in fields {
            let lines = text::wrap(fonts, &value, v, value_w, MAX_FIELD_LINES);
            let mut row = vec![label.text_at(
                fonts,
                0.0,
                0.0,
                &text::ellipsize(fonts, &label, &format!("{l}:"), label_w),
            )];
            for (k, l) in lines.iter().enumerate() {
                row.push(value.text_at(fonts, value_x, k as f32 * line_h, l));
            }
            let rows = lines.len().max(1) as f32;
            // Each row is unbreakable; shift x into the body area when slicing.
            flow.push_block(row, rows * line_h + 2.0);
        }
        flow.gap(4.0);
        flow.push_block(vec![hrule(0.0, body.w, 0.0, 1.0, Color::rgb(0x60, 0x60, 0x60))], 1.0);
        flow.gap(14.0);
        let opts = LayoutOptions { width: body.w, ..LayoutOptions::default() };
        let layout = layout_doc(&item.body, fonts, &opts);
        flow.push_layout(&layout, 0.0);
    }

    let avail = body.h - band_h;
    flow.slices(avail, MAX_PAGES)
        .into_iter()
        .map(|(y0, y1)| {
            let mut page = band.clone();
            page.extend(flow::slice(&flow.items, y0, y1, body.x, body.y + band_h));
            page
        })
        .collect()
}
