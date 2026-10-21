//! Insert › Chart: charts drawn to PNG (every mail program shows PNG) from a recipe that is kept with the
//! picture, so the chart can be opened again, its data changed in the data sheet, and redrawn in a new
//! theme's colours.
//!
//! A recipe ([`ChartSpec`]) has a chart type, a title, category names and up to [`MAX_SERIES`] series of
//! values. Types: clustered and stacked column and bar, line, area, pie, doughnut, scatter (categories as x
//! values when they are numbers) and radar.

use mailcraft_geom::{Color, Point, Rect};
use mailcraft_layout::{DisplayItem, DisplayList};
use mailcraft_model::TextStyle;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RASTER_SCALE, darken, dev, lighten, text_list, translate};
use crate::compose::{ACCENT1, ResolvedTheme};

/// Chart types: (id, label).
pub const KINDS: [(&str, &str); 10] = [
    ("column", "Clustered Column"),
    ("stackedColumn", "Stacked Column"),
    ("bar", "Clustered Bar"),
    ("stackedBar", "Stacked Bar"),
    ("line", "Line with Markers"),
    ("area", "Area"),
    ("pie", "Pie"),
    ("doughnut", "Doughnut"),
    ("scatter", "Scatter"),
    ("radar", "Radar"),
];
/// Most categories (rows of the data sheet).
pub const MAX_CATEGORIES: usize = 100;
/// Most series (value columns of the data sheet).
pub const MAX_SERIES: usize = 12;
/// Longest category, series or title text (chars).
const MAX_LABEL: usize = 80;
/// Default picture size (px).
pub const DEFAULT_SIZE: (f32, f32) = (480.0, 300.0);

/// One series: its name and a value per category (`None` = blank cell).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub name: String,
    pub values: Vec<Option<f64>>,
}

/// What a chart is drawn from (kept with the picture).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChartSpec {
    pub kind: String,
    pub title: String,
    pub categories: Vec<String>,
    pub series: Vec<Series>,
    pub legend: bool,
    pub width: f32,
    pub height: f32,
}

impl Default for ChartSpec {
    fn default() -> Self {
        ChartSpec {
            kind: "column".into(),
            title: String::new(),
            categories: Vec::new(),
            series: Vec::new(),
            legend: true,
            width: DEFAULT_SIZE.0,
            height: DEFAULT_SIZE.1,
        }
    }
}

fn clip(s: &str) -> String {
    s.trim().chars().filter(|c| !c.is_control()).take(MAX_LABEL).collect()
}

fn cell_text(v: &Value) -> String {
    match v {
        Value::String(s) => clip(s),
        Value::Null => String::new(),
        other => clip(&other.to_string()),
    }
}

fn cell_num(v: &Value) -> Option<f64> {
    let n = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace(',', "").parse::<f64>().ok(),
        _ => None,
    }?;
    n.is_finite().then_some(n)
}

impl ChartSpec {
    /// A kind id from an id or label (`None` when unknown).
    pub fn kind_id(name: &str) -> Option<&'static str> {
        KINDS
            .iter()
            .find(|(id, label)| {
                id.eq_ignore_ascii_case(name.trim()) || label.eq_ignore_ascii_case(name.trim())
            })
            .map(|k| k.0)
    }

    /// The data sheet as rows: a header row (blank, series names…), then a row per category.
    pub fn sheet(&self) -> Vec<Vec<String>> {
        let mut rows =
            vec![std::iter::once(String::new()).chain(self.series.iter().map(|s| s.name.clone())).collect()];
        for (i, cat) in self.categories.iter().enumerate() {
            let mut row = vec![cat.clone()];
            for s in &self.series {
                row.push(s.values.get(i).copied().flatten().map(fmt_num).unwrap_or_default());
            }
            rows.push(row);
        }
        rows
    }

    /// Reads a chart from command parameters: `sheet` (rows as [`ChartSpec::sheet`] writes them),
    /// `categories` + `series` ([{"name", "values"}]), or the single-series `data` ([[label, value], …] or
    /// {"labels", "values"}); plus `kind`, `title`, `legend`, `width`, `height`. `Ok(None)` when no data
    /// was given; an error when the data has nothing to draw.
    pub fn from_params(p: &Value) -> Result<Option<ChartSpec>, String> {
        let mut spec = ChartSpec::default();
        if let Some(k) = p.get("kind").and_then(Value::as_str) {
            spec.kind = Self::kind_id(k)
                .ok_or_else(|| {
                    format!(
                        "unknown chart type `{k}` ({})",
                        KINDS.iter().map(|k| k.0).collect::<Vec<_>>().join(", ")
                    )
                })?
                .to_owned();
        }
        spec.title = p.get("title").and_then(Value::as_str).map(clip).unwrap_or_default();
        if let Some(l) = p.get("legend").and_then(Value::as_bool) {
            spec.legend = l;
        }
        let num = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).map(|v| v as f32);
        spec.width = num("width").unwrap_or(DEFAULT_SIZE.0).clamp(240.0, 1000.0);
        spec.height = num("height").unwrap_or(DEFAULT_SIZE.1).clamp(160.0, 800.0);
        if let Some(rows) = p.get("sheet").and_then(Value::as_array) {
            let header: Vec<String> = rows
                .first()
                .and_then(Value::as_array)
                .map(|r| r.iter().skip(1).map(cell_text).collect())
                .unwrap_or_default();
            let ncols = header.len().min(MAX_SERIES);
            spec.series = (0..ncols)
                .map(|i| Series {
                    name: header
                        .get(i)
                        .filter(|n| !n.is_empty())
                        .cloned()
                        .unwrap_or_else(|| format!("Series {}", i + 1)),
                    values: Vec::new(),
                })
                .collect();
            for row in rows.iter().skip(1).filter_map(Value::as_array).take(MAX_CATEGORIES) {
                let cat = row.first().map(cell_text).unwrap_or_default();
                let vals: Vec<Option<f64>> = (0..ncols).map(|i| row.get(i + 1).and_then(cell_num)).collect();
                if cat.is_empty() && vals.iter().all(Option::is_none) {
                    continue;
                }
                spec.categories.push(cat);
                for (s, v) in spec.series.iter_mut().zip(vals) {
                    s.values.push(v);
                }
            }
        } else if let Some(series) = p.get("series").and_then(Value::as_array) {
            spec.categories = p
                .get("categories")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(cell_text).take(MAX_CATEGORIES).collect())
                .unwrap_or_default();
            for (i, s) in series.iter().take(MAX_SERIES).enumerate() {
                let name = s
                    .get("name")
                    .map(cell_text)
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| format!("Series {}", i + 1));
                let values: Vec<Option<f64>> = s
                    .get("values")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(cell_num).take(MAX_CATEGORIES).collect())
                    .unwrap_or_default();
                spec.series.push(Series { name, values });
            }
            let longest = spec.series.iter().map(|s| s.values.len()).max().unwrap_or(0);
            while spec.categories.len() < longest {
                spec.categories.push(format!("{}", spec.categories.len() + 1));
            }
        } else if let Some(data) = p.get("data") {
            let pairs: Vec<(String, f64)> = if let Some(rows) = data.as_array() {
                rows.iter()
                    .filter_map(|r| {
                        let r = r.as_array()?;
                        Some((cell_text(r.first()?), cell_num(r.get(1)?)?))
                    })
                    .collect()
            } else {
                let labels = data.get("labels").and_then(Value::as_array).cloned().unwrap_or_default();
                let values = data.get("values").and_then(Value::as_array).cloned().unwrap_or_default();
                labels.iter().zip(&values).filter_map(|(l, v)| Some((cell_text(l), cell_num(v)?))).collect()
            };
            let pairs: Vec<(String, f64)> = pairs.into_iter().take(MAX_CATEGORIES).collect();
            spec.categories = pairs.iter().map(|(l, _)| l.clone()).collect();
            spec.series = vec![Series {
                name: "Series 1".into(),
                values: pairs.iter().map(|(_, v)| Some(*v)).collect(),
            }];
            spec.legend = p.get("legend").and_then(Value::as_bool).unwrap_or(false);
        } else {
            return Ok(None);
        }
        for s in &mut spec.series {
            s.values.resize(spec.categories.len(), None);
        }
        if spec.categories.is_empty() || spec.series.iter().all(|s| s.values.iter().all(Option::is_none)) {
            return Err("the chart has nothing to show: enter at least one value".into());
        }
        Ok(Some(spec))
    }

    /// The kind's label ("Clustered Column").
    pub fn label(&self) -> &'static str {
        KINDS.iter().find(|(id, _)| *id == self.kind).map_or("Chart", |k| k.1)
    }
}

/// A number as the chart writes it (no trailing zeros).
pub fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

/// Tick values covering [`lo`, `hi`] at a round step.
fn ticks(lo: f64, hi: f64) -> Vec<f64> {
    let (lo, hi) = if (hi - lo).abs() < f64::EPSILON { (lo.min(0.0), lo.max(0.0) + 1.0) } else { (lo, hi) };
    let raw = (hi - lo) / 5.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(mag * 10.0);
    if !step.is_finite() || step <= 0.0 {
        return vec![lo, hi];
    }
    let start = (lo / step).floor() * step;
    let mut out = Vec::new();
    let mut v = start;
    while v <= hi + step * 0.001 && out.len() < 50 {
        out.push(if v.abs() < step * 1e-9 { 0.0 } else { v });
        v += step;
    }
    if out.last().is_some_and(|l| *l < hi) {
        out.push(v);
    }
    out
}

struct Painter<'a> {
    list: DisplayList,
    fonts: &'a mailcraft_fonts::FontDb,
    font: String,
}

impl Painter<'_> {
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color) {
        let (x, w) = if w < 0.0 { (x + w, -w) } else { (x, w) };
        let (y, h) = if h < 0.0 { (y + h, -h) } else { (y, h) };
        if w > 0.0 && h > 0.0 {
            self.list.items.push(DisplayItem::FillRect { rect: Rect::new(x, y, w, h), color });
        }
    }

    fn line(&mut self, a: (f32, f32), b: (f32, f32), color: Color, width: f32) {
        self.list.items.push(DisplayItem::Line {
            from: Point::new(a.0, a.1),
            to: Point::new(b.0, b.1),
            color,
            width,
        });
    }

    fn polygon(&mut self, pts: &[(f32, f32)], color: Color) {
        let mut l = DisplayList::default();
        super::polygon_list(pts, &mut l);
        for mut item in l.items {
            if let DisplayItem::FillRect { color: c, .. } = &mut item {
                *c = color;
            }
            self.list.items.push(item);
        }
    }

    /// Text measured: (width, height).
    fn measure(&self, text: &str, size: f32) -> (f32, f32) {
        let ts =
            TextStyle { size_pt: Some(size), font_family: Some(self.font.clone()), ..Default::default() };
        let (_, _, _, w, h) = text_list(text, &ts, self.fonts);
        (w, h)
    }

    /// Text with its ink box anchored at (`x`, `y`): `ax`/`ay` 0 = left/top, 0.5 = centre, 1 = right/bottom.
    fn text(&mut self, text: &str, x: f32, y: f32, ax: f32, ay: f32, size: f32, color: Color, bold: bool) {
        if text.is_empty() {
            return;
        }
        let ts = TextStyle {
            size_pt: Some(size),
            bold: Some(bold),
            font_family: Some(self.font.clone()),
            ..Default::default()
        };
        let (mut l, x0, y0, w, h) = text_list(text, &ts, self.fonts);
        translate(&mut l, x - w * ax - x0, y - h * ay - y0);
        for mut item in l.items {
            if let DisplayItem::Text { color: c, .. } = &mut item {
                *c = color;
            }
            self.list.items.push(item);
        }
    }
}

/// Colour of series (or pie slice) `i` in `theme`: the six accents, then lighter and darker turns.
fn series_color(theme: &ResolvedTheme, i: usize) -> Color {
    let base = theme.color(ACCENT1 + i % 6);
    match (i / 6) % 3 {
        1 => lighten(base, 0.4),
        2 => darken(base, 0.3),
        _ => base,
    }
}

fn arc_pts(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
    let steps = (((a1 - a0).abs() / std::f32::consts::TAU) * 96.0).ceil().max(2.0) as usize;
    (0..=steps)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// The chart drawn as PNG in `theme`'s colours and body font, at the recipe's size.
pub fn png(spec: &ChartSpec, theme: &ResolvedTheme) -> Option<Vec<u8>> {
    let fonts = mailcraft_fonts::FontDb::bundled();
    let (w, h) = (
        if spec.width.is_finite() { spec.width.clamp(240.0, 1000.0) } else { DEFAULT_SIZE.0 },
        if spec.height.is_finite() { spec.height.clamp(160.0, 800.0) } else { DEFAULT_SIZE.1 },
    );
    let mut p = Painter { list: DisplayList::default(), fonts: &fonts, font: theme.body_font.clone() };
    let ink = Color::rgb(0x40, 0x40, 0x40);
    let grid = Color::rgb(0xd9, 0xd9, 0xd9);
    let axis = Color::rgb(0xa6, 0xa6, 0xa6);
    let mut top = 10.0;
    if !spec.title.is_empty() {
        p.text(&spec.title, w / 2.0, 10.0, 0.5, 0.0, 14.0, ink, false);
        top += p.measure(&spec.title, 14.0).1 + 8.0;
    }
    let round = matches!(spec.kind.as_str(), "pie" | "doughnut");
    // Legend: the series, or the slices of a pie.
    let entries: Vec<String> =
        if round { spec.categories.clone() } else { spec.series.iter().map(|s| s.name.clone()).collect() };
    let mut right = w - 10.0;
    if spec.legend && !entries.is_empty() {
        let lw = entries.iter().map(|e| p.measure(e, 9.0).0).fold(0.0f32, f32::max).min(w * 0.3) + 20.0;
        let row = 16.0;
        let shown = entries.len().min(((h - top - 10.0) / row).floor().max(1.0) as usize);
        let y0 = top + ((h - top - 10.0) - row * shown as f32).max(0.0) / 2.0;
        for (i, e) in entries.iter().take(shown).enumerate() {
            let y = y0 + row * i as f32;
            p.rect(w - lw - 6.0, y + 3.0, 9.0, 9.0, series_color(theme, i));
            p.text(e, w - lw + 8.0, y + 7.5, 0.0, 0.5, 9.0, ink, false);
        }
        right = w - lw - 16.0;
    }
    let bottom = h - 10.0;
    let ns = spec.series.len().max(1);
    let nc = spec.categories.len().max(1);
    let val = |s: &Series, i: usize| s.values.get(i).copied().flatten();
    match spec.kind.as_str() {
        "pie" | "doughnut" => {
            let s = spec.series.first()?;
            let vals: Vec<f64> = (0..nc).map(|i| val(s, i).unwrap_or(0.0).max(0.0)).collect();
            let total: f64 = vals.iter().sum();
            if total <= 0.0 {
                return None;
            }
            let r = ((right - 10.0).min(bottom - top) / 2.0).max(10.0);
            let (cx, cy) = (10.0 + (right - 10.0) / 2.0, top + (bottom - top) / 2.0);
            let mut a0 = -std::f32::consts::FRAC_PI_2;
            for (i, v) in vals.iter().enumerate() {
                let frac = (*v / total) as f32;
                if frac <= 0.0 {
                    continue;
                }
                let a1 = a0 + frac * std::f32::consts::TAU;
                let outer = arc_pts(cx, cy, r, a0, a1);
                let shape: Vec<(f32, f32)> = if spec.kind == "doughnut" {
                    let mut v = outer;
                    v.extend(arc_pts(cx, cy, r * 0.5, a1, a0));
                    v
                } else if frac >= 0.9999 {
                    outer
                } else {
                    std::iter::once((cx, cy)).chain(outer).collect()
                };
                p.polygon(&shape, series_color(theme, i));
                // A thin white edge between slices.
                let inner = if spec.kind == "doughnut" { r * 0.5 } else { 0.0 };
                p.line(
                    (cx + inner * a0.cos(), cy + inner * a0.sin()),
                    (cx + r * a0.cos(), cy + r * a0.sin()),
                    Color::WHITE,
                    1.5,
                );
                if frac > 0.06 {
                    let mid = (a0 + a1) / 2.0;
                    let rr = if spec.kind == "doughnut" { r * 0.75 } else { r * 0.62 };
                    let pct = format!("{}%", (frac * 100.0).round());
                    p.text(&pct, cx + rr * mid.cos(), cy + rr * mid.sin(), 0.5, 0.5, 9.0, Color::WHITE, true);
                }
                a0 = a1;
            }
        }
        "radar" => {
            let all: Vec<f64> = spec.series.iter().flat_map(|s| s.values.iter().flatten().copied()).collect();
            let hi = all.iter().copied().fold(0.0f64, f64::max);
            let t = ticks(0.0, hi);
            let max = t.last().copied().unwrap_or(1.0).max(f64::MIN_POSITIVE);
            let label_h = p.measure("Ag", 9.0).1 + 4.0;
            let r = ((right - 10.0).min(bottom - top) / 2.0 - label_h).max(10.0);
            let (cx, cy) = (10.0 + (right - 10.0) / 2.0, top + (bottom - top) / 2.0);
            let angle =
                |i: usize| -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * i as f32 / nc as f32;
            for tv in &t {
                let rr = r * (*tv / max) as f32;
                let ring: Vec<(f32, f32)> =
                    (0..=nc).map(|i| (cx + rr * angle(i).cos(), cy + rr * angle(i).sin())).collect();
                for pair in ring.windows(2) {
                    if let [a, b] = pair {
                        p.line(*a, *b, grid, 1.0);
                    }
                }
            }
            for (i, cat) in spec.categories.iter().enumerate() {
                let a = angle(i);
                p.line((cx, cy), (cx + r * a.cos(), cy + r * a.sin()), axis, 1.0);
                let (lx, ly) = (cx + (r + 4.0) * a.cos(), cy + (r + 4.0) * a.sin());
                p.text(cat, lx, ly, 0.5 - 0.5 * a.cos(), 0.5 - 0.5 * a.sin(), 9.0, ink, false);
            }
            for (si, s) in spec.series.iter().enumerate() {
                let col = series_color(theme, si);
                let pts: Vec<(f32, f32)> = (0..nc)
                    .map(|i| {
                        let rr = r * (val(s, i).unwrap_or(0.0).max(0.0) / max) as f32;
                        (cx + rr * angle(i).cos(), cy + rr * angle(i).sin())
                    })
                    .collect();
                p.polygon(&pts, Color::rgba(col.r, col.g, col.b, 60));
                for k in 0..pts.len() {
                    if let (Some(a), Some(b)) = (pts.get(k), pts.get((k + 1) % pts.len())) {
                        p.line(*a, *b, col, 2.0);
                    }
                }
            }
        }
        _ => {
            let horizontal = matches!(spec.kind.as_str(), "bar" | "stackedBar");
            let stacked = matches!(spec.kind.as_str(), "stackedColumn" | "stackedBar");
            let scatter = spec.kind == "scatter";
            // Value range.
            let (mut lo, mut hi) = (0.0f64, 0.0f64);
            for i in 0..nc {
                if stacked {
                    let pos: f64 = spec.series.iter().filter_map(|s| val(s, i)).filter(|v| *v > 0.0).sum();
                    let neg: f64 = spec.series.iter().filter_map(|s| val(s, i)).filter(|v| *v < 0.0).sum();
                    hi = hi.max(pos);
                    lo = lo.min(neg);
                } else {
                    for s in &spec.series {
                        if let Some(v) = val(s, i) {
                            hi = hi.max(v);
                            lo = lo.min(v);
                        }
                    }
                }
            }
            let t = ticks(lo, hi);
            let (vmin, vmax) = (t.first().copied().unwrap_or(lo), t.last().copied().unwrap_or(hi));
            let span = (vmax - vmin).max(f64::MIN_POSITIVE);
            // Scatter x values: the categories as numbers when they all are, else 1, 2, 3, …
            let xs: Vec<f64> = {
                let parsed: Vec<Option<f64>> = spec
                    .categories
                    .iter()
                    .map(|c| c.trim().parse::<f64>().ok().filter(|v| v.is_finite()))
                    .collect();
                if parsed.iter().all(Option::is_some) && !parsed.is_empty() {
                    parsed.into_iter().flatten().collect()
                } else {
                    (1..=nc).map(|i| i as f64).collect()
                }
            };
            let xt = ticks(
                xs.iter().copied().fold(f64::MAX, f64::min).min(0.0),
                xs.iter().copied().fold(f64::MIN, f64::max),
            );
            let (xmin, xmax) = (xt.first().copied().unwrap_or(0.0), xt.last().copied().unwrap_or(1.0));
            let xspan = (xmax - xmin).max(f64::MIN_POSITIVE);
            let tick_w = t.iter().map(|v| p.measure(&fmt_num(*v), 9.0).0).fold(0.0f32, f32::max);
            let label_h = p.measure("Ag", 9.0).1;
            let cat_w =
                spec.categories.iter().map(|c| p.measure(c, 9.0).0).fold(0.0f32, f32::max).min(w * 0.3);
            let (pl, pt, pr, pb) = if horizontal {
                (10.0 + cat_w + 6.0, top, right, bottom - label_h - 6.0)
            } else {
                (10.0 + tick_w + 6.0, top, right, bottom - label_h - 6.0)
            };
            if pr - pl < 20.0 || pb - pt < 20.0 {
                return None;
            }
            // Value → position along the value axis.
            let vpos = |v: f64| -> f32 {
                let f = ((v - vmin) / span) as f32;
                if horizontal { pl + f * (pr - pl) } else { pb - f * (pb - pt) }
            };
            for tv in &t {
                let at = vpos(*tv);
                if horizontal {
                    p.line((at, pt), (at, pb), grid, 1.0);
                    p.text(&fmt_num(*tv), at, pb + 4.0, 0.5, 0.0, 9.0, ink, false);
                } else {
                    p.line((pl, at), (pr, at), grid, 1.0);
                    p.text(&fmt_num(*tv), pl - 4.0, at, 1.0, 0.5, 9.0, ink, false);
                }
            }
            if scatter {
                // The x axis is numeric too.
                let xpos = |x: f64| pl + ((x - xmin) / xspan) as f32 * (pr - pl);
                for tv in &xt {
                    p.text(&fmt_num(*tv), xpos(*tv), pb + 4.0, 0.5, 0.0, 9.0, ink, false);
                }
                p.line((pl, pb), (pr, pb), axis, 1.0);
                for (si, s) in spec.series.iter().enumerate() {
                    let col = series_color(theme, si);
                    for (i, x) in xs.iter().enumerate() {
                        if let Some(v) = val(s, i) {
                            let (px, py) = (xpos(*x), vpos(v));
                            p.polygon(&arc_pts(px, py, 3.5, 0.0, std::f32::consts::TAU), col);
                        }
                    }
                }
                return render(&p.list, &fonts, w, h);
            }
            let zero = vpos(0.0f64.clamp(vmin, vmax));
            let slots = if horizontal { pb - pt } else { pr - pl };
            let slot = slots / nc as f32;
            for (i, cat) in spec.categories.iter().enumerate() {
                let mid = slot * (i as f32 + 0.5);
                if horizontal {
                    p.text(cat, pl - 6.0, pt + mid, 1.0, 0.5, 9.0, ink, false);
                } else if slot >= 18.0 || i % ((18.0 / slot).ceil().max(1.0) as usize) == 0 {
                    p.text(cat, pl + mid, pb + 4.0, 0.5, 0.0, 9.0, ink, false);
                }
            }
            match spec.kind.as_str() {
                "line" | "area" => {
                    let area = spec.kind == "area";
                    for (si, s) in spec.series.iter().enumerate().rev() {
                        let col = series_color(theme, si);
                        let pts: Vec<(f32, f32)> = (0..nc)
                            .filter_map(|i| Some((pl + slot * (i as f32 + 0.5), vpos(val(s, i)?))))
                            .collect();
                        if area && pts.len() >= 2 {
                            let mut poly = pts.clone();
                            if let (Some(last), Some(first)) = (pts.last(), pts.first()) {
                                poly.push((last.0, zero));
                                poly.push((first.0, zero));
                            }
                            p.polygon(&poly, Color::rgba(col.r, col.g, col.b, 200));
                        }
                        for pair in pts.windows(2) {
                            if let [a, b] = pair {
                                p.line(*a, *b, col, 2.25);
                            }
                        }
                        if !area {
                            for (x, y) in &pts {
                                p.rect(x - 3.0, y - 3.0, 6.0, 6.0, col);
                            }
                        }
                    }
                }
                _ => {
                    let group = slot * 0.7;
                    let bar = if stacked { group } else { group / ns as f32 };
                    for i in 0..nc {
                        let (mut up, mut down) = (0.0f64, 0.0f64);
                        for (si, s) in spec.series.iter().enumerate() {
                            let Some(v) = val(s, i) else { continue };
                            let col = series_color(theme, si);
                            let (from, to) = if stacked {
                                if v >= 0.0 {
                                    up += v;
                                    (up - v, up)
                                } else {
                                    down += v;
                                    (down - v, down)
                                }
                            } else {
                                (0.0, v)
                            };
                            let offset = slot * i as f32
                                + (slot - group) / 2.0
                                + if stacked { 0.0 } else { bar * si as f32 };
                            let (a, b) = (vpos(from.clamp(vmin, vmax)), vpos(to.clamp(vmin, vmax)));
                            if horizontal {
                                p.rect(a, pt + offset, b - a, bar * 0.92, col);
                            } else {
                                p.rect(pl + offset, b, bar * 0.92, a - b, col);
                            }
                        }
                    }
                }
            }
            if horizontal {
                p.line((zero, pt), (zero, pb), axis, 1.0);
            } else {
                p.line((pl, zero), (pr, zero), axis, 1.0);
            }
        }
    }
    render(&p.list, &fonts, w, h)
}

fn render(list: &DisplayList, fonts: &mailcraft_fonts::FontDb, w: f32, h: f32) -> Option<Vec<u8>> {
    let (dw, dh) = (u32::try_from(dev(w)).ok()?, u32::try_from(dev(h)).ok()?);
    let canvas = mailcraft_render::render(list, fonts, dw, dh, RASTER_SCALE, Color::WHITE, &|_| None);
    let png = canvas.to_png();
    (!png.is_empty()).then_some(png)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn theme() -> ResolvedTheme {
        crate::compose::ThemeState::default().resolve()
    }

    #[test]
    fn data_forms_and_the_sheet() {
        let a =
            ChartSpec::from_params(&json!({"data": [["Q1", 10], ["Q2", "14"], ["bad"]]})).unwrap().unwrap();
        assert_eq!(a.categories, vec!["Q1", "Q2"]);
        assert_eq!(a.series[0].values, vec![Some(10.0), Some(14.0)]);
        let b =
            ChartSpec::from_params(&json!({"data": {"labels": ["a", "b"], "values": [1, 2]}, "kind": "Pie"}))
                .unwrap()
                .unwrap();
        assert_eq!(b.kind, "pie");
        let sheet = json!([["", "North", "South"], ["Jan", 3, 4], ["Feb", "5", ""], ["", null, null]]);
        let c = ChartSpec::from_params(&json!({"sheet": sheet, "kind": "stackedColumn", "title": "Sales"}))
            .unwrap()
            .unwrap();
        assert_eq!(c.series.len(), 2);
        assert_eq!(c.series[1].values, vec![Some(4.0), None]);
        assert_eq!(c.sheet(), vec![vec!["", "North", "South"], vec!["Jan", "3", "4"], vec!["Feb", "5", ""]]);
        let d = ChartSpec::from_params(
            &json!({"categories": ["x"], "series": [{"name": "S", "values": [1, 2, 3]}]}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(d.categories, vec!["x", "2", "3"]);
        assert!(ChartSpec::from_params(&json!({})).unwrap().is_none());
        assert!(ChartSpec::from_params(&json!({"data": []})).is_err());
        assert!(ChartSpec::from_params(&json!({"data": [["a", 1]], "kind": "sparkline"})).is_err());
        // Limits.
        let many: Vec<Value> = (0..500).map(|i| json!([format!("c{i}"), i])).collect();
        assert_eq!(
            ChartSpec::from_params(&json!({"data": many})).unwrap().unwrap().categories.len(),
            MAX_CATEGORIES
        );
    }

    #[test]
    fn every_kind_draws_a_png() {
        let sheet = json!([["", "A", "B", "C"], ["1", 3, -2, 5], ["2", 4, 6, null], ["3", 0.5, 2, 9]]);
        let mut pngs = Vec::new();
        for (kind, _) in KINDS {
            let spec = ChartSpec::from_params(&json!({"sheet": sheet, "kind": kind, "title": "T"}))
                .unwrap()
                .unwrap();
            let png = png(&spec, &theme()).unwrap_or_else(|| panic!("{kind}"));
            assert!(png.starts_with(b"\x89PNG"), "{kind}");
            pngs.push(png);
        }
        let unique: std::collections::HashSet<_> = pngs.iter().collect();
        assert_eq!(unique.len(), KINDS.len(), "every type looks different");
        // Nothing positive to show in a pie.
        let zero = ChartSpec::from_params(&json!({"data": [["a", 0]], "kind": "pie"})).unwrap().unwrap();
        assert!(png(&zero, &theme()).is_none());
        // Odd numbers do not crash.
        let odd = ChartSpec::from_params(&json!({"data": [["a", 1e300], ["b", -1e300], ["c", 1e-300]]}))
            .unwrap()
            .unwrap();
        let _ = png(&odd, &theme());
    }

    #[test]
    fn ticks_are_round() {
        assert_eq!(ticks(0.0, 14.0), vec![0.0, 5.0, 10.0, 15.0]);
        assert_eq!(ticks(-3.0, 9.0), vec![-5.0, -2.5, 0.0, 2.5, 5.0, 7.5, 10.0]);
        assert_eq!(ticks(0.0, 0.0), vec![0.0, 0.2, 0.4, 0.6000000000000001, 0.8, 1.0]);
        assert_eq!(fmt_num(2.50), "2.5");
        assert_eq!(fmt_num(3.0), "3");
    }
}
