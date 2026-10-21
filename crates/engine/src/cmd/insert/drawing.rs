//! Floating drawing objects of the compose body ([`mailcraft_model::Drawing`]): drawing them into pixels
//! (the editor shows the same pixels the recipient gets), the objects Insert › Shapes / Text Box / WordArt
//! create, and turning them into pictures in the text when a message is saved or sent.
//!
//! Sending: drawings that overlap each other (a canvas and what was drawn in it, a shape with a text box on
//! it) form one group; each group becomes one PNG, placed in its own paragraph before the first paragraph
//! that starts at or below the group's top, indented to the group's left edge. Mail programs do not
//! position floating objects reliably, so the text no longer flows around them: they sit between the
//! paragraphs where they were drawn. SVG pictures of the message are turned into PNG at the same time.

use mailcraft_geom::Color;
use mailcraft_model::rich::{Align, Drawing, DrawingKind, ShapeEffects, VAlign, WrapStyle};
use mailcraft_model::{Block, Doc, Inline, ParaStyle, TextStyle};

use super::shapes::{self, Geometry, P};
use super::{
    EFFECT_MARGIN, Effect, Img, MAX_RASTER_SIDE, Mask, RASTER_SCALE, composite, darken, dev, effect_style,
    lighten, translate, wordart_img,
};
use crate::compose::{ComposeState, InlineImage, ResolvedTheme};

pub mod fx;
pub use fx::{BEVELS, DASHES, GRADIENT_DIRECTIONS, WARPS, store_picture, valid_gradient};

/// Largest drawing side (px).
pub const MAX_DRAWING_SIDE: f32 = 1000.0;
/// Smallest drawing side (px).
pub const MIN_DRAWING_SIDE: f32 = 1.0;
/// Furthest a drawing may sit from the body's corner (px).
pub const MAX_DRAWING_POS: f32 = 100_000.0;
/// Most drawings in one body.
pub const MAX_DRAWINGS: usize = 500;
/// Longest text in a drawing (chars).
pub const MAX_DRAWING_TEXT: usize = 10_000;
/// Most hand-drawn points kept for a freeform or scribble.
pub const MAX_POINTS: usize = 2_000;
/// Most points of a wrap outline.
pub const MAX_WRAP_POINTS: usize = 256;
/// Largest picture fill kept on a drawing (base64 chars).
pub const MAX_PICTURE_FILL: usize = 3 << 20;
/// Longest side a picture fill is stored at (px).
pub const PICTURE_FILL_SIDE: u32 = 512;
/// Most stops of a gradient.
pub const MAX_GRADIENT_STOPS: usize = 10;
/// Default text box margins (left, top, right, bottom px).
pub const TEXT_BOX_INSET: [f32; 4] = [8.0, 4.0, 8.0, 4.0];
/// Shape effects offered by Shape Format › Shape Effects (`""` = the theme's effect style).
pub const EFFECTS: [&str; 7] = ["", "none", "shadow", "glow", "reflection", "bevel", "softEdges"];
/// Text effects offered by WordArt Styles › Text Effects (`""` = a WordArt style's own).
pub const TEXT_EFFECTS: [&str; 6] = ["", "none", "shadow", "glow", "reflection", "bevel"];
/// Longest side of a sent drawing group's picture (device px).
const MAX_GROUP_SIDE: f32 = 2400.0;
/// Width the body is laid out at when the editor has not reported one (px).
pub const DEFAULT_VIEW_WIDTH: f32 = 600.0;

/// A drawing in pixels: premultiplied RGBA8, `scale` device pixels per px; the image's top-left corner
/// is (`left`, `top`) px from the drawing's own corner (negative: room for shadows and glows).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub scale: f32,
    pub left: f32,
    pub top: f32,
}

impl Raster {
    /// Size in px.
    pub fn size(&self) -> (f32, f32) {
        (self.width as f32 / self.scale, self.height as f32 / self.scale)
    }

    /// PNG encoding.
    pub fn to_png(&self) -> Vec<u8> {
        mailcraft_render::Canvas { width: self.width, height: self.height, pixels: self.rgba.clone() }
            .to_png()
    }
}

fn fonts() -> &'static mailcraft_fonts::FontDb {
    static FONTS: std::sync::OnceLock<mailcraft_fonts::FontDb> = std::sync::OnceLock::new();
    FONTS.get_or_init(mailcraft_fonts::FontDb::bundled)
}

fn finite(v: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { fallback }
}

/// Keeps a drawing's numbers in range (hostile params, old saves): size, position, rotation, outline,
/// text length and points.
pub fn sanitize(d: &mut Drawing) {
    d.w = finite(d.w, MIN_DRAWING_SIDE, MAX_DRAWING_SIDE, 160.0);
    d.h = finite(d.h, MIN_DRAWING_SIDE, MAX_DRAWING_SIDE, 100.0);
    d.x = finite(d.x, -MAX_DRAWING_SIDE, MAX_DRAWING_POS, 0.0);
    d.y = finite(d.y, -MAX_DRAWING_SIDE, MAX_DRAWING_POS, 0.0);
    d.rotation = if d.rotation.is_finite() { d.rotation.rem_euclid(360.0) } else { 0.0 };
    d.outline_px = finite(d.outline_px, 0.0, 24.0, 1.0);
    if d.text.chars().count() > MAX_DRAWING_TEXT {
        d.text = d.text.chars().take(MAX_DRAWING_TEXT).collect();
    }
    d.points.truncate(MAX_POINTS);
    d.points.retain(|(x, y)| x.is_finite() && y.is_finite());
    for p in &mut d.points {
        *p = (p.0.clamp(0.0, 1.0), p.1.clamp(0.0, 1.0));
    }
    if let Some(s) = d.text_style.size_pt.as_mut() {
        *s = finite(*s, 1.0, 400.0, 11.0);
    }
    if d.shape.is_empty() || shapes::canonical(&d.shape).is_none() {
        d.shape = "rectangle".into();
    }
    if !valid_gradient(&d.gradient) {
        d.gradient.clear();
    }
    if !TEXT_EFFECTS.contains(&d.text_effect.as_str()) {
        d.text_effect.clear();
    }
    let unit = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    d.transparency = unit(d.transparency);
    d.line_transparency = unit(d.line_transparency);
    if !DASHES.contains(&d.line_dash.as_str()) {
        d.line_dash.clear();
    }
    if !DASHES.contains(&d.text_outline_dash.as_str()) {
        d.text_outline_dash.clear();
    }
    d.text_outline_px = finite(d.text_outline_px, 0.0, 12.0, 0.0);
    if !matches!(d.text_flow.as_str(), "" | "up" | "stacked") {
        d.text_flow.clear();
    }
    if !d.text_flow.is_empty() {
        d.vertical = false;
    }
    if !WARPS.iter().any(|(id, _)| *id == d.warp) {
        d.warp.clear();
    }
    if let Some(inset) = d.inset.as_mut() {
        for v in inset.iter_mut() {
            *v = finite(*v, 0.0, MAX_DRAWING_SIDE / 2.0, 0.0);
        }
    }
    d.wrap_points.truncate(MAX_WRAP_POINTS);
    d.wrap_points.retain(|(x, y)| x.is_finite() && y.is_finite());
    for p in &mut d.wrap_points {
        *p = (p.0.clamp(0.0, 1.0), p.1.clamp(0.0, 1.0));
    }
    if d.fill_picture.len() > MAX_PICTURE_FILL {
        d.fill_picture.clear();
    }
    if let Some(g) = d.gradient_fill.as_mut() {
        g.stops.truncate(MAX_GRADIENT_STOPS);
        g.stops.retain(|s| s.pos.is_finite());
        for s in &mut g.stops {
            s.pos = s.pos.clamp(0.0, 1.0);
        }
        g.angle = if g.angle.is_finite() { g.angle.rem_euclid(360.0) } else { 0.0 };
    }
    if d.gradient_fill.as_ref().is_some_and(|g| g.stops.is_empty()) {
        d.gradient_fill = None;
    }
    for fx in [d.fx.as_deref_mut(), d.text_fx.as_deref_mut()].into_iter().flatten() {
        sanitize_fx(fx);
    }
    if d.fx.as_deref().is_some_and(ShapeEffects::is_empty) {
        d.fx = None;
    }
    if d.text_fx.as_deref().is_some_and(ShapeEffects::is_empty) {
        d.text_fx = None;
    }
    d.name = d.name.chars().take(80).collect();
}

/// Keeps effect options in range.
fn sanitize_fx(fx: &mut ShapeEffects) {
    let num = |v: &mut f32, hi: f32| *v = if v.is_finite() { v.clamp(0.0, hi) } else { 0.0 };
    if let Some(s) = fx.shadow.as_mut() {
        num(&mut s.transparency, 1.0);
        num(&mut s.blur, 60.0);
        num(&mut s.distance, 200.0);
        s.angle = if s.angle.is_finite() { s.angle.rem_euclid(360.0) } else { 45.0 };
    }
    if let Some(r) = fx.reflection.as_mut() {
        num(&mut r.transparency, 1.0);
        num(&mut r.size, 1.0);
        num(&mut r.distance, 100.0);
        num(&mut r.blur, 20.0);
    }
    if let Some(g) = fx.glow.as_mut() {
        num(&mut g.size, 60.0);
        num(&mut g.transparency, 1.0);
    }
    num(&mut fx.soft_edges, 50.0);
    num(&mut fx.bevel_size, 30.0);
    if !BEVELS.contains(&fx.bevel.as_str()) {
        fx.bevel.clear();
    }
    for v in [&mut fx.rot_x, &mut fx.rot_y] {
        *v = if v.is_finite() { v.rem_euclid(360.0) } else { 0.0 };
    }
    num(&mut fx.perspective, 120.0);
}

/// `c` lightened toward white by `t` (0..=1).
pub fn lighten_color(c: Color, t: f32) -> Color {
    lighten(c, t)
}

/// `c` darkened toward black by `t` (0..=1).
pub fn darken_color(c: Color, t: f32) -> Color {
    darken(c, t)
}

/// The effects a WordArt style gets from the drawing: its own text outline, text effect (or text effects
/// with options) and warp.
fn wordart_overrides(d: &Drawing) -> super::WordArtOverrides {
    super::WordArtOverrides {
        outline: d.text_outline,
        effect: d.text_effect.clone(),
        fx: d.text_fx.as_deref().cloned(),
        warp: d.warp.clone(),
        outline_px: d.text_outline_px,
    }
}

/// The effect a drawing is drawn with: its own, or the theme's effect style.
fn effect_of(d: &Drawing, theme: &ResolvedTheme) -> Effect {
    let base = Effect { opacity: 1.0, ..Effect::default() };
    let fx = match d.effect.as_str() {
        "none" => base,
        "shadow" => Effect { shadow: Some((3.0, 3.0, 0.45)), ..base },
        "glow" => Effect { glow: Some((6.0, 0.6)), ..base },
        "reflection" => Effect { reflection: true, ..base },
        "bevel" => Effect { inset: true, gloss: true, ..base },
        "softEdges" => Effect { soft: 4.0, ..base },
        _ => effect_style(theme.effects),
    };
    // The outline is the drawing's own.
    Effect { outline: 0.0, ..fx }
}

/// Display list filling every sub-path of `paths` together (even-odd), one span per device row.
fn fill_list(paths: &[Vec<P>], ox: f32, oy: f32, list: &mut mailcraft_layout::DisplayList) {
    use mailcraft_geom::Rect;
    use mailcraft_layout::DisplayItem;
    let edges: Vec<(P, P)> = paths
        .iter()
        .filter(|p| p.len() >= 3)
        .flat_map(|p| {
            p.iter().enumerate().map(move |(i, a)| {
                let b = p.get((i + 1) % p.len()).copied().unwrap_or(*a);
                ((a.0 + ox, a.1 + oy), (b.0 + ox, b.1 + oy))
            })
        })
        .filter(|(a, b)| a.0.is_finite() && a.1.is_finite() && b.0.is_finite() && b.1.is_finite())
        .collect();
    let (min_y, max_y) = edges
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), (a, b)| (lo.min(a.1).min(b.1), hi.max(a.1).max(b.1)));
    if edges.is_empty() || !min_y.is_finite() || !max_y.is_finite() {
        return;
    }
    let step = 1.0 / RASTER_SCALE;
    let mut y = (min_y * RASTER_SCALE).floor() / RASTER_SCALE;
    let mut rows = 0usize;
    let mut xs: Vec<f32> = Vec::new();
    while y < max_y && rows < 8_000 {
        let yc = y + step / 2.0;
        xs.clear();
        for (a, b) in &edges {
            if (a.1 <= yc && b.1 > yc) || (b.1 <= yc && a.1 > yc) {
                let t = (yc - a.1) / (b.1 - a.1);
                xs.push(a.0 + t * (b.0 - a.0));
            }
        }
        xs.sort_by(f32::total_cmp);
        for pair in xs.chunks(2) {
            if let [x0, x1] = pair
                && x1 > x0
            {
                list.items.push(DisplayItem::FillRect {
                    rect: Rect::new(*x0, y, x1 - x0, step),
                    color: Color::BLACK,
                });
            }
        }
        y += step;
        rows += 1;
    }
}

/// Display list of open lines `width` px wide (round joins).
fn line_list(lines: &[Vec<P>], width: f32, ox: f32, oy: f32, list: &mut mailcraft_layout::DisplayList) {
    use mailcraft_geom::Point;
    use mailcraft_layout::DisplayItem;
    for line in lines {
        for pair in line.windows(2) {
            if let [a, b] = pair {
                list.items.push(DisplayItem::Line {
                    from: Point::new(a.0 + ox, a.1 + oy),
                    to: Point::new(b.0 + ox, b.1 + oy),
                    color: Color::BLACK,
                    width,
                });
            }
        }
        if width > 2.0 {
            // Round joins so thick polylines have no notches.
            let r = width / 2.0;
            let dots: Vec<Vec<P>> = line
                .iter()
                .take(4_000)
                .map(|p| {
                    (0..12)
                        .map(|i| {
                            let a = std::f32::consts::TAU * i as f32 / 12.0;
                            (p.0 + r * a.cos(), p.1 + r * a.sin())
                        })
                        .collect()
                })
                .collect();
            for dot in &dots {
                fill_list(std::slice::from_ref(dot), ox, oy, list);
            }
        }
    }
}

/// Arrowhead triangles for `arrows`, sized for a `width` px line.
fn arrow_paths(arrows: &[(P, P)], width: f32) -> Vec<Vec<P>> {
    let len = (width * 4.0).max(8.0);
    arrows
        .iter()
        .filter_map(|(tip, from)| {
            let (dx, dy) = (tip.0 - from.0, tip.1 - from.1);
            let n = (dx * dx + dy * dy).sqrt();
            if !n.is_finite() || n < 0.01 {
                return None;
            }
            let (ux, uy) = (dx / n, dy / n);
            let base = (tip.0 - ux * len, tip.1 - uy * len);
            let half = len * 0.45;
            Some(vec![
                *tip,
                (base.0 - uy * half, base.1 + ux * half),
                (base.0 + uy * half, base.1 - ux * half),
            ])
        })
        .collect()
}

/// The text colour a drawing uses when its text has none: dark text in text boxes and canvases, light text
/// on filled shapes.
pub fn default_text_color(d: &Drawing, theme: &ResolvedTheme) -> Color {
    match d.kind {
        DrawingKind::Shape if d.fill.is_some() && !shapes::is_line_shape(&d.shape) => theme.color(1),
        _ => theme.color(0),
    }
}

/// The area the text of `d` is laid out in: (x, y, w, h) px from the drawing's corner. Text boxes keep
/// their margins ([`Drawing::inset`], default [`TEXT_BOX_INSET`]); shapes their shape's text area unless
/// margins were set.
pub fn text_area(d: &Drawing) -> (f32, f32, f32, f32) {
    let boxed = |[l, t, r, b]: [f32; 4]| (l, t, (d.w - l - r).max(4.0), (d.h - t - b).max(4.0));
    match (d.inset, d.kind) {
        (Some(inset), _) => boxed(inset),
        (None, DrawingKind::TextBox | DrawingKind::Canvas | DrawingKind::Group) => boxed(TEXT_BOX_INSET),
        (None, _) => shapes::text_rect(&d.shape, d.w, d.h),
    }
}

/// The drawing's text as a document laid out `width` px wide (lines split at `\n`; stacked text has one
/// letter per line).
pub fn text_layout(d: &Drawing, theme: &ResolvedTheme, width: f32) -> mailcraft_layout::Layout {
    let style = TextStyle {
        font_family: d.text_style.font_family.clone().or_else(|| Some(theme.body_font.clone())),
        size_pt: d.text_style.size_pt.or(Some(11.0)),
        color: None,
        ..d.text_style.clone()
    };
    let align = match d.kind {
        DrawingKind::Shape if d.align == Align::Start && d.text_style == TextStyle::default() => {
            Align::Center
        }
        _ if d.text_flow == "stacked" => Align::Center,
        _ => d.align,
    };
    let stacked: String;
    let text = if d.text_flow == "stacked" {
        stacked = d
            .text
            .chars()
            .filter(|c| *c != '\n')
            .take(2_000)
            .map(String::from)
            .collect::<Vec<_>>()
            .join("\n");
        stacked.as_str()
    } else {
        d.text.as_str()
    };
    let blocks = text
        .split('\n')
        .map(|line| Block::Paragraph {
            style: ParaStyle { align, ..ParaStyle::default() },
            inlines: if line.is_empty() {
                Vec::new()
            } else {
                vec![Inline::Text { text: line.to_owned(), style: style.clone() }]
            },
        })
        .collect();
    let doc = Doc { blocks, ..Doc::default() };
    let lo = mailcraft_layout::LayoutOptions { width, ..Default::default() };
    mailcraft_layout::layout_doc(&doc, fonts(), &lo)
}

/// Whether the text of `d` runs across the box's height (turned a quarter).
fn turned_text(d: &Drawing) -> bool {
    d.vertical || d.text_flow == "up"
}

/// Height (px) `d` needs for all of its text (Resize Shape to Fit Text): the text's height plus the
/// margins around the text area.
pub fn fitted_height(d: &Drawing, theme: &ResolvedTheme) -> Option<f32> {
    if d.text.trim().is_empty()
        || matches!(d.kind, DrawingKind::WordArt | DrawingKind::Canvas | DrawingKind::Group)
    {
        return None;
    }
    if turned_text(d) {
        return None;
    }
    let (_, _, tw, th) = text_area(d);
    let extra = (d.h - th).max(0.0);
    let h = text_layout(d, theme, tw).height;
    h.is_finite().then(|| (h + extra).clamp(MIN_DRAWING_SIDE, MAX_DRAWING_SIDE))
}

/// Coverage of the drawing's text laid out in its text area, with the drawing's corner at (`m`, `m`) on a
/// `dw`×`dh` device canvas.
fn text_mask(d: &Drawing, theme: &ResolvedTheme, m: f32, dw: usize, dh: usize) -> Option<Mask> {
    if d.text.trim().is_empty() {
        return None;
    }
    let (tx, ty, tw, th) = text_area(d);
    // Turned text is laid out along the box's height and turned a quarter into place.
    let turned = turned_text(d);
    let (tw, th) = if turned { (th, tw) } else { (tw, th) };
    let layout = text_layout(d, theme, tw);
    let valign = match (d.kind, d.valign) {
        // Text in shapes is centred vertically unless asked otherwise.
        (DrawingKind::Shape, VAlign::Top) if d.text_style == TextStyle::default() => VAlign::Middle,
        (_, v) => v,
    };
    let dy = match valign {
        VAlign::Top => 0.0,
        VAlign::Middle => ((th - layout.height) / 2.0).max(0.0),
        VAlign::Bottom => (th - layout.height).max(0.0),
    };
    let mut list = layout.display;
    list.items.retain(|i| !matches!(i, mailcraft_layout::DisplayItem::FillRect { .. }));
    let mask = if turned {
        // The text on its own canvas (run direction across), then each device pixel of the text area
        // takes the pixel it came from before the turn (clockwise for top to bottom, the other way for
        // bottom to top).
        translate(&mut list, 0.0, dy);
        let (sw, sh) = (dev(tw), dev(th));
        let flat = Mask::from_list(&list, sw, sh, fonts());
        let (x0, y0) = (((m + tx) * RASTER_SCALE) as i64, ((m + ty) * RASTER_SCALE) as i64);
        let (w_dev, h_dev) = (i64::try_from(sh).unwrap_or(0), i64::try_from(sw).unwrap_or(0));
        let up = d.text_flow == "up";
        Mask::new(dw, dh).map(|x, y| {
            let (lx, ly) = (x - x0, y - y0);
            if lx < 0 || ly < 0 || lx >= w_dev || ly >= h_dev {
                return 0;
            }
            if up { flat.get(h_dev - 1 - ly, lx) } else { flat.get(ly, w_dev - 1 - lx) }
        })
    } else {
        translate(&mut list, m + tx, m + ty + dy);
        let mask = Mask::from_list(&list, dw, dh, fonts());
        // Text stays inside the drawing's box.
        let (x0, y0) = (((m + tx) * RASTER_SCALE) as i64, ((m + ty) * RASTER_SCALE) as i64);
        let (x1, y1) =
            (((m + tx + tw) * RASTER_SCALE).ceil() as i64, ((m + ty + th) * RASTER_SCALE).ceil() as i64);
        mask.map(|x, y| if x >= x0 && x < x1 && y >= y0 && y < y1 { mask.get(x, y) } else { 0 })
    };
    Some(if d.warp.is_empty() { mask } else { fx::warp(&mask, &d.warp) })
}

/// Pixels of `d` in `theme` (with or without its text: the editor leaves the text out while it is being
/// typed in). `None` when there is nothing to draw.
pub fn raster(d: &Drawing, theme: &ResolvedTheme, with_text: bool) -> Option<Raster> {
    let mut d = d.clone();
    sanitize(&mut d);
    if d.kind == DrawingKind::Group {
        // A group draws nothing itself: its members do.
        return None;
    }
    let img = if d.kind == DrawingKind::WordArt {
        let text = if with_text { d.text.as_str() } else { "" };
        let text = if text.trim().is_empty() { " " } else { text };
        let (img, ..) = wordart_img(text, &d.word_art, theme, &d.text_style, &wordart_overrides(&d))?;
        (img, -EFFECT_MARGIN, -EFFECT_MARGIN)
    } else {
        shape_img(&d, theme, with_text)?
    };
    let (mut img, mut left, mut top) = img;
    // 3-D Rotation turns the whole picture (WordArt: its Text Effects' 3-D rotation).
    let turn3d = if d.kind == DrawingKind::WordArt { d.text_fx.as_deref() } else { d.fx.as_deref() };
    if let Some(fx3) = turn3d {
        let (cx, cy) = ((d.w / 2.0 - left) * RASTER_SCALE, (d.h / 2.0 - top) * RASTER_SCALE);
        if let Some((turned, ox, oy)) = fx::rotate_3d(&img, fx3, cx, cy) {
            img = turned;
            left += ox / RASTER_SCALE;
            top += oy / RASTER_SCALE;
        }
    }
    if d.rotation.abs() > 0.01 {
        let (cx, cy) = (d.w / 2.0 - left, d.h / 2.0 - top);
        let (rot, ox, oy) = rotate(&img, d.rotation, cx * RASTER_SCALE, cy * RASTER_SCALE);
        img = rot;
        left += ox / RASTER_SCALE;
        top += oy / RASTER_SCALE;
    }
    let rgba = img_rgba(&img);
    Some(Raster {
        width: u32::try_from(img.w).ok()?,
        height: u32::try_from(img.h).ok()?,
        rgba,
        scale: RASTER_SCALE,
        left,
        top,
    })
}

/// Premultiplied RGBA8 of an image.
fn img_rgba(img: &Img) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.px.len() * 4);
    for p in &img.px {
        for v in p {
            out.push((v.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    out
}

/// `img` turned `deg` degrees clockwise about device point (`cx`, `cy`): the new image and where its corner
/// lies relative to the old one (device px).
fn rotate(img: &Img, deg: f32, cx: f32, cy: f32) -> (Img, f32, f32) {
    let (s, c) = deg.to_radians().sin_cos();
    let corners = [(0.0, 0.0), (img.w as f32, 0.0), (0.0, img.h as f32), (img.w as f32, img.h as f32)];
    let turned: Vec<P> = corners
        .iter()
        .map(|(x, y)| (cx + (x - cx) * c - (y - cy) * s, cy + (x - cx) * s + (y - cy) * c))
        .collect();
    let (x0, y0, x1, y1) = turned.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |(a, b, e, f), p| {
        (a.min(p.0), b.min(p.1), e.max(p.0), f.max(p.1))
    });
    let max = MAX_RASTER_SIDE * RASTER_SCALE * 1.5;
    let (w, h) = (((x1 - x0).ceil().clamp(1.0, max)) as usize, ((y1 - y0).ceil().clamp(1.0, max)) as usize);
    let mut out = Img::new(w, h);
    let get = |x: i64, y: i64| -> [f32; 4] {
        if x < 0 || y < 0 || x >= img.w as i64 || y >= img.h as i64 {
            return [0.0; 4];
        }
        img.px.get((y as usize) * img.w + x as usize).copied().unwrap_or([0.0; 4])
    };
    for oy in 0..h {
        for ox in 0..w {
            // Inverse rotation back into the source, bilinear.
            let (px, py) = (ox as f32 + x0 + 0.5 - cx, oy as f32 + y0 + 0.5 - cy);
            let sx = cx + px * c + py * s - 0.5;
            let sy = cy - px * s + py * c - 0.5;
            let (fx, fy) = (sx.floor(), sy.floor());
            let (tx, ty) = (sx - fx, sy - fy);
            let (ix, iy) = (fx as i64, fy as i64);
            let (a, b, e, f) = (get(ix, iy), get(ix + 1, iy), get(ix, iy + 1), get(ix + 1, iy + 1));
            if let Some(dst) = out.px.get_mut(oy * w + ox) {
                for ((((d, a), b), e), f) in dst.iter_mut().zip(a).zip(b).zip(e).zip(f) {
                    let top = a * (1.0 - tx) + b * tx;
                    let bot = e * (1.0 - tx) + f * tx;
                    *d = top * (1.0 - ty) + bot * ty;
                }
            }
        }
    }
    (out, x0, y0)
}

/// Mirrors geometry `g` of a `w`×`h` box left to right and / or top to bottom.
fn flip_geometry(g: &mut Geometry, w: f32, h: f32, fh: bool, fv: bool) {
    let f = |p: &mut P| {
        if fh {
            p.0 = w - p.0;
        }
        if fv {
            p.1 = h - p.1;
        }
    };
    g.fill.iter_mut().flatten().for_each(f);
    g.faces.iter_mut().flat_map(|(p, _)| p.iter_mut()).for_each(f);
    g.lines.iter_mut().flatten().for_each(f);
    for (a, b) in &mut g.arrows {
        f(a);
        f(b);
    }
}

/// Room (px) the effects with options need around a `d.h` px tall drawing.
fn fx_room(fx: &ShapeEffects, h: f32) -> f32 {
    let mut r: f32 = 0.0;
    if let Some(s) = fx.shadow {
        r = r.max(s.distance + s.blur + 2.0);
        if s.kind == mailcraft_model::rich::ShadowKind::Perspective {
            r = r.max(h * 0.7 + s.distance);
        }
    }
    if let Some(g) = fx.glow {
        r = r.max(g.size + 2.0);
    }
    r.min(MAX_DRAWING_SIDE / 2.0)
}

/// `c` with `transparency` (0..=1) more see-through.
fn faded(c: Color, transparency: f32) -> Color {
    let a = f32::from(c.a) * (1.0 - transparency.clamp(0.0, 1.0));
    c.with_alpha(a.round().clamp(0.0, 255.0) as u8)
}

/// Stripes that cut an outline into dashes where no path is at hand (text outlines): coverage at (`x`, `y`).
fn dash_stripes(dash: &str, x: i64, y: i64) -> bool {
    let (on, off) = match dash {
        "roundDot" | "squareDot" => (2, 2),
        "dash" | "dashDot" => (6, 4),
        "longDash" | "longDashDot" | "longDashDotDot" => (12, 4),
        _ => return true,
    };
    (x + y).rem_euclid(on + off) < on
}

/// A shape, text box or canvas drawn: the picture and its corner relative to the drawing's (px).
fn shape_img(d: &Drawing, theme: &ResolvedTheme, with_text: bool) -> Option<(Img, f32, f32)> {
    let shape = if d.kind == DrawingKind::TextBox { "textBox" } else { d.shape.as_str() };
    let mut g: Geometry = shapes::geometry(shape, d.w, d.h, &d.points)?;
    if d.flip_h || d.flip_v {
        flip_geometry(&mut g, d.w, d.h, d.flip_h, d.flip_v);
    }
    // Effects with options replace the effect preset and the theme's effect style.
    let own: Option<&ShapeEffects> = d.fx.as_deref();
    let fx = if own.is_some() { Effect { opacity: 1.0, ..Effect::default() } } else { effect_of(d, theme) };
    let m = EFFECT_MARGIN.max(own.map_or(0.0, |o| fx_room(o, d.h))) + d.outline_px;
    let extra = match own.and_then(|o| o.reflection) {
        Some(r) => fx::reflection_room(&r, d.h),
        None if fx.reflection => d.h * 0.5,
        None => 0.0,
    };
    let (w, h) = (d.w + 2.0 * m, d.h + 2.0 * m + extra);
    let (dw, dh) = (dev(w), dev(h));
    let mut img = Img::new(dw, dh);
    let fill = d.fill.filter(|_| !g.is_line()).map(|c| faded(c, d.transparency));
    let picture =
        (!g.is_line() && !d.fill_picture.is_empty()).then(|| fx::Picture::decode(&d.fill_picture)).flatten();
    let mut area_list = mailcraft_layout::DisplayList::default();
    fill_list(&g.fill, m, m, &mut area_list);
    let area = Mask::from_list(&area_list, dw, dh, fonts());
    let mut face_masks = Vec::new();
    for (path, tint) in &g.faces {
        let mut l = mailcraft_layout::DisplayList::default();
        fill_list(std::slice::from_ref(path), m, m, &mut l);
        face_masks.push((Mask::from_list(&l, dw, dh, fonts()), *tint));
    }
    // The whole area (fill and faces) for the outline and effects.
    let whole =
        face_masks.iter().fold(area.clone(), |acc, (f, _)| acc.map(|x, y| acc.get(x, y).max(f.get(x, y))));
    let whole = match own {
        Some(o) if o.soft_edges > 0.0 => fx::soften(&whole, o),
        _ => whole,
    };
    let s = RASTER_SCALE;
    let outline = d.outline.filter(|_| d.outline_px > 0.0).map(|c| faded(c, d.line_transparency));
    let dashes = !d.line_dash.is_empty();
    let ring = |mask: &Mask| mask.minus(&mask.eroded(((d.outline_px * s / 2.0).round() as i64).max(1)));
    let mut strokes = mailcraft_layout::DisplayList::default();
    let lw = d.outline_px.max(0.5);
    line_list(&fx::dashed(&g.lines, false, &d.line_dash, lw), lw, m, m, &mut strokes);
    let heads = arrow_paths(&g.arrows, lw);
    fill_list(&heads, m, m, &mut strokes);
    let stroke_mask = (!strokes.items.is_empty()).then(|| Mask::from_list(&strokes, dw, dh, fonts()));
    // A dashed outline follows the shape's outline path; a solid one is the ring inside its edge.
    let outline_mask = |whole: &Mask| -> Mask {
        if !dashes {
            return ring(whole);
        }
        // Dashes along the edge, kept inside it like the solid ring.
        let mut l = mailcraft_layout::DisplayList::default();
        line_list(&fx::dashed(&g.fill, true, &d.line_dash, lw), lw, m, m, &mut l);
        let dashes = Mask::from_list(&l, dw, dh, fonts());
        dashes.map(|x, y| dashes.get(x, y).min(whole.get(x, y)))
    };
    let bx = (m * s, m * s, (m + d.w) * s, (m + d.h) * s);
    match fill.or(picture.as_ref().map(|_| Color::WHITE)) {
        Some(color) => {
            if let Some(o) = own {
                fx::paint_under(&mut img, &whole, color, o);
                img.paint(&whole, 1.0, &|_, _| color);
            } else {
                composite(&mut img, (0, 0), &whole, color, &fx);
            }
            if let Some(gf) = &d.gradient_fill {
                let t = d.transparency;
                img.paint(&whole, 1.0, &|x, y| faded(fx::stops_gradient(gf, bx, x as f32, y as f32), t));
            } else if !d.gradient.is_empty() {
                let kind = d.gradient.as_str();
                img.paint(&whole, 1.0, &|x, y| fx::preset_gradient(kind, color, bx, x as f32, y as f32));
            }
            if let Some(pic) = &picture {
                let (bw, bh) = ((bx.2 - bx.0).max(1.0), (bx.3 - bx.1).max(1.0));
                let t = d.transparency;
                img.paint(&whole, 1.0, &|x, y| {
                    faded(pic.sample((x as f32 - bx.0) / bw, (y as f32 - bx.1) / bh), t)
                });
            }
            for (mask, tint) in &face_masks {
                let c = if *tint > 0.0 {
                    lighten(color, *tint)
                } else if *tint < 0.0 {
                    darken(color, -tint)
                } else {
                    color
                };
                img.paint(mask, 1.0, &|_, _| c);
            }
        }
        None if d.kind != DrawingKind::Canvas => {
            // No fill: the effects follow the outline (or the lines).
            if let Some(o) = outline {
                let mut base = if g.is_line() { Mask::new(dw, dh) } else { outline_mask(&whole) };
                if let Some(sm) = &stroke_mask {
                    base = base.map(|x, y| base.get(x, y).max(sm.get(x, y)));
                }
                match own {
                    Some(fo) => fx::paint_under(&mut img, &base, o, fo),
                    None => {
                        composite(&mut img, (0, 0), &base, o, &Effect { gradient: 0.0, gloss: false, ..fx })
                    }
                }
            }
        }
        None => {}
    }
    if let Some(o) = outline {
        if !g.is_line() {
            img.paint(&outline_mask(&whole), 1.0, &|_, _| o);
        }
        if let Some(sm) = &stroke_mask {
            img.paint(sm, 1.0, &|_, _| o);
        }
    }
    if let Some(o) = own {
        let base = fill.or(outline).unwrap_or_else(|| theme.color(crate::compose::ACCENT1));
        let shape_mask = if fill.is_some() || picture.is_some() { whole.clone() } else { area.clone() };
        fx::paint_over(&mut img, &shape_mask, base, o);
    }
    if with_text && let Some(tm) = text_mask(d, theme, m, dw, dh) {
        paint_text(&mut img, &tm, d, theme);
    }
    if let Some(r) = own.and_then(|o| o.reflection) {
        let (top, axis) = ((m * s) as usize, ((m + d.h) * s).round() as usize);
        fx::reflect(&mut img, top, axis, &r);
    }
    Some((img, -m, -m))
}

/// Paints text coverage `tm` of drawing `d` with its colour, outline and text effects.
fn paint_text(img: &mut Img, tm: &Mask, d: &Drawing, theme: &ResolvedTheme) {
    let s = RASTER_SCALE;
    let col = d.text_style.color.unwrap_or_else(|| default_text_color(d, theme));
    let px = |v: f32| (v * s).round() as i64;
    let tfx = d.text_fx.as_deref().map(|t| ShapeEffects {
        // Bevels on letters are narrow.
        bevel_size: if t.bevel_size > 0.0 { t.bevel_size.min(3.0) } else { 0.8 },
        reflection: None,
        ..t.clone()
    });
    match &tfx {
        Some(t) => fx::paint_under(img, tm, col, t),
        None => match d.text_effect.as_str() {
            "shadow" => img.paint(&tm.shifted(px(1.5), px(1.5)).blurred(px(1.0)), 0.45, &|_, _| Color::BLACK),
            "glow" => img.paint(&tm.dilated(px(2.0)).blurred(px(1.5)), 0.6, &|_, _| lighten(col, 0.5)),
            _ => {}
        },
    }
    if let Some(o) = d.text_outline {
        let weight = if d.text_outline_px > 0.0 { d.text_outline_px / 2.0 } else { 0.75 };
        let ring = tm.dilated(px(weight).max(1));
        let ring = if d.text_outline_dash.is_empty() {
            ring
        } else {
            let dash = d.text_outline_dash.as_str();
            ring.map(|x, y| if dash_stripes(dash, x, y) || tm.get(x, y) > 0 { ring.get(x, y) } else { 0 })
        };
        img.paint(&ring, 1.0, &|_, _| o);
    }
    img.paint(tm, 1.0, &|_, _| col);
    match &tfx {
        Some(t) => fx::paint_over(img, tm, col, t),
        None if d.text_effect == "bevel" => {
            img.paint(&tm.minus(&tm.shifted(px(0.6), px(0.6))), 0.7, &|_, _| Color::WHITE);
        }
        None => {}
    }
}

// ---------------------------------------------------------------------------------------------------------
// New drawings
// ---------------------------------------------------------------------------------------------------------

/// The next free drawing id of `doc`.
pub fn next_id(doc: &Doc) -> u32 {
    doc.drawings.iter().map(|d| d.id).max().map_or(1, |m| m.saturating_add(1))
}

/// A new shape `id` in the theme's colours (accent fill, darker outline, light text; lines in the accent).
pub fn new_shape(theme: &ResolvedTheme, id: &str) -> Drawing {
    let accent = theme.color(crate::compose::ACCENT1);
    let (w, h) = shapes::default_size(id);
    let line = shapes::is_line_shape(id);
    let mut d = Drawing {
        kind: DrawingKind::Shape,
        shape: id.to_owned(),
        w,
        h,
        fill: (!line).then_some(accent),
        outline: Some(if line { accent } else { darken(accent, 0.35) }),
        outline_px: if line { 1.5 } else { 1.0 },
        alt: shapes::label(id).to_owned(),
        ..Drawing::default()
    };
    match id {
        "canvas" => {
            d.kind = DrawingKind::Canvas;
            d.fill = None;
            d.outline = None;
        }
        "textBox" => d = new_text_box(theme, ""),
        _ => {}
    }
    d
}

/// A new text box holding `text`: white with a thin dark outline and dark text.
pub fn new_text_box(theme: &ResolvedTheme, text: &str) -> Drawing {
    Drawing {
        kind: DrawingKind::TextBox,
        shape: "rectangle".into(),
        w: 240.0,
        h: 120.0,
        fill: Some(theme.color(1)),
        outline: Some(theme.color(0)),
        outline_px: 1.0,
        effect: "none".into(),
        text: text.to_owned(),
        ..Drawing::default()
    }
}

/// A new WordArt drawing of `text` in `style`, sized to its letters.
pub fn new_word_art(theme: &ResolvedTheme, text: &str, style: &str) -> Drawing {
    let mut d = Drawing {
        kind: DrawingKind::WordArt,
        shape: "rectangle".into(),
        text: text.to_owned(),
        word_art: style.to_owned(),
        outline: None,
        fill: None,
        alt: text.to_owned(),
        ..Drawing::default()
    };
    fit_word_art(&mut d, theme);
    d
}

/// Sizes a WordArt drawing's box to its letters (they decide its size; resizing changes the font size).
pub fn fit_word_art(d: &mut Drawing, theme: &ResolvedTheme) {
    if d.kind != DrawingKind::WordArt {
        return;
    }
    let text = if d.text.trim().is_empty() { " " } else { d.text.as_str() };
    if let Some((_, w, h)) = wordart_img(text, &d.word_art, theme, &d.text_style, &wordart_overrides(d)) {
        // The picture is the letters, the margins and any reflection below.
        let refl = match d.text_fx.as_deref() {
            Some(f) => f.reflection.map(|r| fx::reflection_room(&r, 1.0).clamp(0.05, 1.2)),
            None if d.text_effect.is_empty() => {
                super::parse_wordart(&d.word_art, theme).reflection.then_some(0.6)
            }
            None => (d.text_effect == "reflection").then_some(0.6),
        };
        let m = EFFECT_MARGIN;
        let ink_h = h - 2.0 * m;
        let ink_h = ink_h / (1.0 + refl.unwrap_or(0.0));
        d.w = (w - 2.0 * m).clamp(MIN_DRAWING_SIDE, MAX_DRAWING_SIDE);
        d.h = ink_h.clamp(MIN_DRAWING_SIDE, MAX_DRAWING_SIDE);
    }
}

/// Where the caret is in the body laid out at the editor's width (px): new drawings go there.
pub fn caret_point(c: &ComposeState) -> (f32, f32) {
    let width = c.view_width.filter(|w| w.is_finite() && *w > 40.0).unwrap_or(DEFAULT_VIEW_WIDTH);
    let lo = mailcraft_layout::LayoutOptions { width, ..Default::default() };
    let layout = mailcraft_layout::layout_doc(&c.body, fonts(), &lo);
    let pos = mailcraft_layout::TextPos {
        block_path: c.selection.focus.path.clone(),
        offset: c.selection.focus.offset,
    };
    layout.caret_rect(&pos).map_or((0.0, 0.0), |r| (r.x.max(0.0), r.y.max(0.0)))
}

/// The front-most drawing canvas whose box holds all of `d` (a canvas is never inside another).
pub fn canvas_around(drawings: &[Drawing], d: &Drawing) -> Option<u32> {
    if d.kind == DrawingKind::Canvas {
        return None;
    }
    drawings.iter().rev().find(|o| o.kind == DrawingKind::Canvas && o.id != d.id && d.inside(o)).map(|o| o.id)
}

/// Puts `d` into the body at (`x`, `y`) (or at the caret), selects it and returns its id. Checkpoints first.
pub fn add(
    c: &mut ComposeState,
    mut d: Drawing,
    at: Option<(f32, f32)>,
    checkpoint: bool,
) -> Result<u32, String> {
    if c.body.drawings.len() >= MAX_DRAWINGS {
        return Err(format!("a message holds at most {MAX_DRAWINGS} drawings"));
    }
    let (x, y) = at.unwrap_or_else(|| {
        let (cx, cy) = caret_point(c);
        let width = c.view_width.unwrap_or(DEFAULT_VIEW_WIDTH);
        (cx.min((width - d.w).max(0.0)), cy)
    });
    d.x = x;
    d.y = y;
    sanitize(&mut d);
    if checkpoint {
        c.checkpoint();
    }
    d.id = next_id(&c.body);
    d.canvas = None;
    d.canvas = canvas_around(&c.body.drawings, &d);
    let id = d.id;
    c.body.drawings.push(d);
    c.select_drawing(Some(id));
    c.draw_tool = None;
    c.dirty = true;
    Ok(id)
}

// ---------------------------------------------------------------------------------------------------------
// Sending: drawings and SVG pictures become PNG pictures in the text
// ---------------------------------------------------------------------------------------------------------

/// Groups of drawings whose pictures overlap or that are in the same group (`keys`: the outermost group of
/// each, if any), as indexes into `boxes`, each in back-to-front order.
fn groups(boxes: &[(f32, f32, f32, f32)], keys: &[Option<u32>]) -> Vec<Vec<usize>> {
    let n = boxes.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        let mut guard = 0;
        while let Some(&p) = parent.get(i) {
            if p == i || guard > parent.len() {
                break;
            }
            i = p;
            guard += 1;
        }
        i
    }
    for i in 0..n {
        for j in (i + 1)..n {
            let (Some(a), Some(b)) = (boxes.get(i), boxes.get(j)) else { continue };
            let overlap = a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3;
            let grouped = matches!((keys.get(i), keys.get(j)), (Some(Some(x)), Some(Some(y))) if x == y);
            if overlap || grouped {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                if let Some(p) = parent.get_mut(ri) {
                    *p = rj;
                }
            }
        }
    }
    let mut out: Vec<(usize, Vec<usize>)> = Vec::new();
    for i in 0..n {
        let r = root(&mut parent, i);
        match out.iter_mut().find(|(k, _)| *k == r) {
            Some((_, v)) => v.push(i),
            None => out.push((r, vec![i])),
        }
    }
    out.into_iter().map(|(_, v)| v).collect()
}

/// Draws `src` onto the premultiplied canvas `dst` (`dw`×`dh`) with its corner at device (`ox`, `oy`),
/// scaled by `ratio` (destination device px per source device px), bilinear.
fn blit(dst: &mut [u8], dw: usize, dh: usize, src: &Raster, ox: f32, oy: f32, ratio: f32) {
    let (sw, sh) = (src.width as usize, src.height as usize);
    if sw == 0 || sh == 0 || !ratio.is_finite() || ratio <= 0.0 {
        return;
    }
    let get = |x: i64, y: i64, k: usize| -> f32 {
        if x < 0 || y < 0 || x as usize >= sw || y as usize >= sh {
            return 0.0;
        }
        f32::from(src.rgba.get(((y as usize) * sw + x as usize) * 4 + k).copied().unwrap_or(0))
    };
    let x0 = ox.floor().max(0.0) as usize;
    let y0 = oy.floor().max(0.0) as usize;
    let x1 = ((ox + sw as f32 * ratio).ceil().max(0.0) as usize).min(dw);
    let y1 = ((oy + sh as f32 * ratio).ceil().max(0.0) as usize).min(dh);
    for y in y0..y1 {
        for x in x0..x1 {
            let sx = (x as f32 + 0.5 - ox) / ratio - 0.5;
            let sy = (y as f32 + 0.5 - oy) / ratio - 0.5;
            let (fx, fy) = (sx.floor(), sy.floor());
            let (tx, ty) = (sx - fx, sy - fy);
            let (ix, iy) = (fx as i64, fy as i64);
            let mut px = [0.0f32; 4];
            for (k, v) in px.iter_mut().enumerate() {
                let top = get(ix, iy, k) * (1.0 - tx) + get(ix + 1, iy, k) * tx;
                let bot = get(ix, iy + 1, k) * (1.0 - tx) + get(ix + 1, iy + 1, k) * tx;
                *v = top * (1.0 - ty) + bot * ty;
            }
            let a = px[3] / 255.0;
            if a <= 0.0 {
                continue;
            }
            let at = (y * dw + x) * 4;
            if let Some(d) = dst.get_mut(at..at + 4) {
                for (dk, sk) in d.iter_mut().zip(px) {
                    let v = sk + f32::from(*dk) * (1.0 - a);
                    *dk = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

/// A group of drawings as one picture: PNG, its box in the body (x, y, w, h px) and its alt text.
fn group_picture(
    drawings: &[&Drawing],
    rasters: &[&Raster],
) -> Option<(Vec<u8>, (f32, f32, f32, f32), String)> {
    let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (d, r) in drawings.iter().zip(rasters) {
        let (rw, rh) = r.size();
        let (x, y) = (d.x + r.left, d.y + r.top);
        b = (b.0.min(x), b.1.min(y), b.2.max(x + rw), b.3.max(y + rh));
    }
    let (bw, bh) = (b.2 - b.0, b.3 - b.1);
    if !(bw.is_finite() && bh.is_finite() && bw > 0.0 && bh > 0.0) {
        return None;
    }
    let scale = RASTER_SCALE.min(MAX_GROUP_SIDE / bw.max(bh));
    let (dw, dh) = ((bw * scale).ceil().max(1.0) as usize, (bh * scale).ceil().max(1.0) as usize);
    let mut px = vec![0u8; dw.checked_mul(dh)?.checked_mul(4)?];
    for (d, r) in drawings.iter().zip(rasters) {
        let ox = (d.x + r.left - b.0) * scale;
        let oy = (d.y + r.top - b.1) * scale;
        blit(&mut px, dw, dh, r, ox, oy, scale / r.scale);
    }
    // An empty canvas sends nothing.
    if !px.chunks(4).any(|p| p.get(3).is_some_and(|a| *a > 0)) {
        return None;
    }
    let png = mailcraft_render::Canvas {
        width: u32::try_from(dw).ok()?,
        height: u32::try_from(dh).ok()?,
        pixels: px,
    }
    .to_png();
    let alt: Vec<String> = drawings
        .iter()
        .filter_map(|d| {
            let t = if d.alt.trim().is_empty() { d.text.trim() } else { d.alt.trim() };
            (!t.is_empty()).then(|| t.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .collect();
    let alt = if alt.is_empty() { "Drawing".to_owned() } else { alt.join("; ") };
    (!png.is_empty()).then_some((png, (b.0, b.1, bw, bh), alt))
}

/// A drawing's picture cut to what it paints: PNG and its size in px.
fn cropped_png(r: &Raster) -> Option<(Vec<u8>, f32, f32)> {
    let (w, h) = (r.width as usize, r.height as usize);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if r.rgba.get((y * w + x) * 4 + 3).is_some_and(|a| *a > 0) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut px = Vec::with_capacity(cw.checked_mul(ch)?.checked_mul(4)?);
    for y in y0..y1 {
        let from = (y * w + x0) * 4;
        px.extend_from_slice(r.rgba.get(from..from + cw * 4)?);
    }
    let png = mailcraft_render::Canvas {
        width: u32::try_from(cw).ok()?,
        height: u32::try_from(ch).ok()?,
        pixels: px,
    }
    .to_png();
    (!png.is_empty()).then(|| (png, cw as f32 / r.scale, ch as f32 / r.scale))
}

/// Tops (px) of the body's top-level blocks laid out at `width` (`None` for blocks without lines).
fn block_tops(doc: &Doc, width: f32) -> Vec<Option<f32>> {
    let lo = mailcraft_layout::LayoutOptions { width, ..Default::default() };
    let layout = mailcraft_layout::layout_doc(doc, fonts(), &lo);
    let mut tops = vec![None; doc.blocks.len()];
    for line in &layout.index.lines {
        if let Some(i) = line.block_path.first()
            && let Some(t) = tops.get_mut(*i)
        {
            *t = Some(t.map_or(line.rect.y, |v: f32| v.min(line.rect.y)));
        }
    }
    tops
}

/// The compose as it is sent or saved: its drawings turned into pictures in the text and its SVG pictures
/// into PNG. `None` when it has neither.
pub fn flattened(c: &ComposeState) -> Option<ComposeState> {
    let svg = |i: &InlineImage| i.mime_type.eq_ignore_ascii_case("image/svg+xml");
    if c.body.drawings.is_empty() && !c.inline_images.iter().any(svg) {
        return None;
    }
    let mut f = c.clone();
    for img in &mut f.inline_images {
        if !svg(img) {
            continue;
        }
        if let Some(p) = mailcraft_render::svg::svg_to_png(&img.data, RASTER_SCALE) {
            img.data = p.png;
            img.mime_type = "image/png".into();
            let stem = img.filename.rsplit_once('.').map_or(img.filename.as_str(), |(s, _)| s).to_owned();
            img.filename = format!("{stem}.png");
        }
    }
    let drawings = std::mem::take(&mut f.body.drawings);
    if drawings.is_empty() {
        return Some(f);
    }
    let theme = c.theme_resolved();
    // Linked text boxes show their share of the text.
    let drawings = crate::cmd::drawing::flow_text(&drawings, &theme);
    // In line with the text: the drawing's picture takes the place of its placeholder.
    for d in drawings.iter().filter(|d| d.wrap == WrapStyle::Inline) {
        let src = d.inline_src();
        let Some((png, w, h)) = raster(d, &theme, true).and_then(|r| cropped_png(&r)) else {
            crate::compose::remove_placeholders(&mut f.body.blocks, d.id);
            continue;
        };
        let cid = f.next_content_id();
        f.inline_images.push(InlineImage {
            content_id: cid.clone(),
            filename: format!("drawing{:03}.png", f.inline_images.len().saturating_add(1)),
            mime_type: "image/png".into(),
            data: png,
        });
        crate::compose::resize_src(&mut f.body.blocks, &src, w.round().max(1.0), h.round().max(1.0));
        crate::compose::replace_src(&mut f.body.blocks, &src, &format!("cid:{cid}"));
    }
    // Placeholders whose drawing is gone send nothing.
    for id in crate::compose::placeholder_ids(&f.body.blocks) {
        crate::compose::remove_placeholders(&mut f.body.blocks, id);
    }
    let mut kept: Vec<(&Drawing, Raster)> = Vec::new();
    for d in drawings.iter().filter(|d| d.wrap != WrapStyle::Inline) {
        if let Some(r) = raster(d, &theme, true) {
            kept.push((d, r));
        }
    }
    let boxes: Vec<(f32, f32, f32, f32)> = kept
        .iter()
        .map(|(d, r)| {
            let (w, h) = r.size();
            (d.x + r.left, d.y + r.top, d.x + r.left + w, d.y + r.top + h)
        })
        .collect();
    // Drawings in one group go in one picture even when apart.
    let keys: Vec<Option<u32>> =
        kept.iter().map(|(d, _)| d.group.map(|_| crate::cmd::drawing::root_of(&drawings, d.id))).collect();
    let width = c.view_width.filter(|w| w.is_finite() && *w > 40.0).unwrap_or(DEFAULT_VIEW_WIDTH);
    let tops = block_tops(&f.body, width);
    let mut placed: Vec<(usize, f32, Block)> = Vec::new();
    for group in groups(&boxes, &keys) {
        let ds: Vec<&Drawing> = group.iter().filter_map(|i| kept.get(*i).map(|k| k.0)).collect();
        let rs: Vec<&Raster> = group.iter().filter_map(|i| kept.get(*i).map(|k| &k.1)).collect();
        let Some((png, (x, y, w, h), alt)) = group_picture(&ds, &rs) else { continue };
        let cid = f.next_content_id();
        f.inline_images.push(InlineImage {
            content_id: cid.clone(),
            filename: format!("drawing{:03}.png", f.inline_images.len().saturating_add(1)),
            mime_type: "image/png".into(),
            data: png,
        });
        let at = tops.iter().position(|t| t.is_some_and(|t| t >= y - 1.0)).unwrap_or(f.body.blocks.len());
        let para = Block::Paragraph {
            style: ParaStyle { indent_px: x.max(0.0).round(), ..ParaStyle::default() },
            inlines: vec![Inline::Image {
                src: format!("cid:{cid}"),
                alt,
                width: Some(w.round().max(1.0)),
                height: Some(h.round().max(1.0)),
            }],
        };
        placed.push((at, y, para));
    }
    // Bottom-most first, so earlier insertions do not move later anchors; same anchor: lower first.
    placed.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));
    for (at, _, para) in placed {
        let at = at.min(f.body.blocks.len());
        f.body.blocks.insert(at, para);
    }
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> ResolvedTheme {
        crate::compose::ThemeState::default().resolve()
    }

    fn alpha_at(r: &Raster, x: f32, y: f32) -> u8 {
        let px = ((x - r.left) * r.scale) as usize;
        let py = ((y - r.top) * r.scale) as usize;
        r.rgba.get((py * r.width as usize + px) * 4 + 3).copied().unwrap_or(0)
    }

    #[test]
    fn shapes_text_boxes_and_word_art_draw() {
        let t = theme();
        let mut d = new_shape(&t, "oval");
        d.text = "Hello".into();
        let r = raster(&d, &t, true).unwrap();
        assert!(r.left < 0.0 && r.top < 0.0, "room for effects");
        assert!(alpha_at(&r, 80.0, 50.0) > 200, "the middle is filled");
        assert_eq!(alpha_at(&r, 2.0, 2.0), 0, "the corner of an oval is empty");
        let plain = raster(&d, &t, false).unwrap();
        assert_ne!(r.rgba, plain.rgba, "the text is drawn");
        // Every gallery shape draws something.
        for (id, ..) in shapes::SHAPES {
            let d = new_shape(&t, id);
            let r = raster(&d, &t, true).unwrap_or_else(|| panic!("{id}"));
            assert!(r.rgba.chunks(4).any(|p| p[3] > 0), "{id} is blank");
        }
        let tb = new_text_box(&t, "Line one\nLine two");
        let r = raster(&tb, &t, true).unwrap();
        assert!(alpha_at(&r, 120.0, 100.0) > 200, "a text box is filled white");
        let wa = new_word_art(&t, "Title", "Fill: Black, Text color 1; Shadow");
        assert!(wa.w > 40.0 && wa.h > 20.0, "{} {}", wa.w, wa.h);
        assert!(raster(&wa, &t, true).is_some());
        // An empty canvas draws nothing visible but still has pixels to place.
        let canvas = new_shape(&t, "canvas");
        let r = raster(&canvas, &t, true).unwrap();
        assert!(r.rgba.chunks(4).all(|p| p[3] == 0));
    }

    #[test]
    fn rotation_turns_the_picture() {
        let t = theme();
        let mut d = new_shape(&t, "rectangle");
        d.w = 200.0;
        d.h = 20.0;
        let flat = raster(&d, &t, true).unwrap();
        d.rotation = 90.0;
        let turned = raster(&d, &t, true).unwrap();
        let (fw, fh) = flat.size();
        let (tw, th) = turned.size();
        assert!(fw > fh && th > tw, "{fw}x{fh} → {tw}x{th}");
        // Still centred on the box.
        assert!((turned.left + tw / 2.0 - 100.0).abs() < 2.0 && (turned.top + th / 2.0 - 10.0).abs() < 2.0);
    }

    #[test]
    fn hostile_numbers_are_kept_in_range() {
        let t = theme();
        let mut d = Drawing {
            w: f32::NAN,
            h: 1e12,
            x: f32::INFINITY,
            y: -1e9,
            rotation: f32::NAN,
            outline_px: 1e9,
            shape: "no such shape".into(),
            points: vec![(f32::NAN, 1.0), (5.0, -3.0)],
            text_style: TextStyle { size_pt: Some(f32::NAN), ..TextStyle::default() },
            ..Drawing::default()
        };
        sanitize(&mut d);
        assert_eq!((d.w, d.h), (160.0, MAX_DRAWING_SIDE));
        assert_eq!((d.x, d.y), (0.0, -MAX_DRAWING_SIDE));
        assert_eq!(d.rotation, 0.0);
        assert_eq!(d.shape, "rectangle");
        assert_eq!(d.points, vec![(1.0, 0.0)]);
        assert!(raster(&d, &t, true).is_some());
    }

    #[test]
    fn an_empty_canvas_sends_no_picture() {
        let t = theme();
        let mut c = ComposeState::default();
        c.body.blocks.push(Block::text_paragraph("Hi"));
        c.body.drawings.push(Drawing { id: 1, ..new_shape(&t, "canvas") });
        let f = flattened(&c).unwrap();
        assert!(f.body.drawings.is_empty());
        assert!(f.inline_images.is_empty());
        assert_eq!(f.body.blocks, c.body.blocks);
        // With a shape in it, the canvas and the shape are one picture.
        c.body.drawings.push(Drawing { id: 2, x: 20.0, y: 20.0, w: 40.0, h: 40.0, ..new_shape(&t, "oval") });
        let f = flattened(&c).unwrap();
        assert_eq!(f.inline_images.len(), 1);
        assert_eq!(f.body.blocks.len(), 2);
    }

    #[test]
    fn overlapping_drawings_form_one_group() {
        let g = groups(
            &[
                (0.0, 0.0, 10.0, 10.0),
                (50.0, 50.0, 60.0, 60.0),
                (5.0, 5.0, 55.0, 55.0),
                (100.0, 0.0, 110.0, 5.0),
            ],
            &[],
        );
        assert_eq!(g, vec![vec![0, 1, 2], vec![3]]);
    }
}
