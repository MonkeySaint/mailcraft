//! Offscreen rendering (layer L3): paints a [`mailcraft_layout::DisplayList`] into an RGBA pixmap with
//! tiny-skia, drawing glyph outlines from `mailcraft-fonts`. Used for headless screenshots (CLI `show`),
//! golden-image tests and print preview. The interactive UI paints the same display list with egui instead.
//!
//! Interface (frozen for commit 2): [`Canvas`], [`render`], [`Canvas::to_png`].
//!
//! Conventions:
//! - `width`×`height` is the canvas size in device pixels; display-list coordinates are logical px and are
//!   multiplied by `scale` before painting.
//! - Canvases larger than [`MAX_DIMENSION`] on either side or [`MAX_PIXELS`] in total are refused (an empty
//!   [`Canvas`] is returned).
//! - Axis-aligned lines, rectangle frames and text decorations are snapped to whole device pixels so 1 px rules
//!   stay crisp; everything else is antialiased.
//! - [`DisplayItem::StrokeRect`](mailcraft_layout::DisplayItem::StrokeRect) paints its border *inside* the
//!   rectangle (like a CSS border box).
//! - Non-finite or absurd coordinates are skipped or clipped; rendering never panics.

mod paint;
#[cfg(test)]
mod tests;

use mailcraft_fonts::FontDb;
use mailcraft_geom::Color;
use mailcraft_layout::DisplayList;

/// Largest accepted canvas side, in device pixels.
pub const MAX_DIMENSION: u32 = 16_384;
/// Largest accepted canvas area, in device pixels (256 Mi).
pub const MAX_PIXELS: u64 = 256 * 1024 * 1024;
/// Largest accepted decoded image side (images beyond this are drawn as placeholders).
pub const MAX_IMAGE_DIMENSION: u32 = 16_384;
/// Largest accepted decoded image area (64 Mi pixels).
pub const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;

/// RGBA8 premultiplied pixels, row-major.
#[derive(Clone, Debug, Default)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Canvas {
    /// PNG encoding of the canvas.
    ///
    /// Returns an empty vector for an empty canvas or when the pixel buffer does not match the dimensions.
    pub fn to_png(&self) -> Vec<u8> {
        let Some(expected) = byte_len(self.width, self.height) else {
            return Vec::new();
        };
        if self.pixels.len() != expected {
            return Vec::new();
        }
        match tiny_skia::PixmapRef::from_bytes(&self.pixels, self.width, self.height) {
            Some(pm) => pm.encode_png().unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// Unpremultiplied RGBA at (x, y), for tests.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Color> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = (y as usize).checked_mul(self.width as usize)?.checked_add(x as usize)?.checked_mul(4)?;
        let px = self.pixels.get(idx..idx.checked_add(4)?)?;
        let (r, g, b, a) = (*px.first()?, *px.get(1)?, *px.get(2)?, *px.get(3)?);
        Some(demultiply(r, g, b, a))
    }

    /// True when the canvas holds no pixels.
    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }

    /// Whole canvas as unpremultiplied RGBA8 (straight alpha), row-major.
    pub fn to_rgba_unpremultiplied(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len());
        for px in self.pixels.as_chunks::<4>().0 {
            let [r, g, b, a] = *px;
            let c = demultiply(r, g, b, a);
            out.extend_from_slice(&[c.r, c.g, c.b, c.a]);
        }
        out
    }
}

/// Converts one premultiplied RGBA8 pixel to straight alpha.
fn demultiply(r: u8, g: u8, b: u8, a: u8) -> Color {
    if a == 0 {
        return Color::rgba(0, 0, 0, 0);
    }
    if a == 255 {
        return Color::rgba(r, g, b, a);
    }
    let un = |c: u8| -> u8 {
        let a32 = u32::from(a);
        let v = (u32::from(c) * 255 + a32 / 2) / a32;
        u8::try_from(v.min(255)).unwrap_or(255)
    };
    Color::rgba(un(r), un(g), un(b), a)
}

/// Byte length of a `w`×`h` RGBA8 buffer, or `None` when empty or beyond the canvas caps.
fn byte_len(w: u32, h: u32) -> Option<usize> {
    if w == 0 || h == 0 || w > MAX_DIMENSION || h > MAX_DIMENSION {
        return None;
    }
    let px = u64::from(w).checked_mul(u64::from(h))?;
    if px > MAX_PIXELS {
        return None;
    }
    usize::try_from(px.checked_mul(4)?).ok()
}

/// Renders `list` onto a `width`×`height` canvas filled with `background`, scaled by `scale` (device pixels per
/// logical px). Images are drawn as placeholders (frame + alt text) unless `images` resolves them.
pub fn render(
    list: &DisplayList,
    fonts: &FontDb,
    width: u32,
    height: u32,
    scale: f32,
    background: Color,
    images: &dyn Fn(&str) -> Option<(u32, u32, Vec<u8>)>,
) -> Canvas {
    if byte_len(width, height).is_none() {
        return Canvas::default();
    }
    let Some(mut pixmap) = tiny_skia::Pixmap::new(width, height) else {
        return Canvas::default();
    };
    pixmap.fill(paint::sk_color(background));
    let mut painter = paint::Painter::new(&mut pixmap, fonts, sanitize_scale(scale), images);
    for item in &list.items {
        painter.item(item);
    }
    Canvas { width, height, pixels: pixmap.take() }
}

/// Non-finite or non-positive scales fall back to 1; the rest is clamped to a sane range.
fn sanitize_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 { scale.clamp(1.0 / 64.0, 64.0) } else { 1.0 }
}
