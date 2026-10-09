//! Geometry and colour primitives (layer L0).
//!
//! All layout happens in logical pixels (`f32`, 1 px = 1/96 in at 100 % scaling). Font sizes are stored in points
//! and converted with [`pt_to_px`].

use serde::{Deserialize, Serialize};

/// Points per inch.
pub const PT_PER_INCH: f32 = 72.0;
/// Logical pixels per inch at 100 % scaling.
pub const PX_PER_INCH: f32 = 96.0;

/// Converts points to logical pixels.
pub fn pt_to_px(pt: f32) -> f32 {
    pt * PX_PER_INCH / PT_PER_INCH
}

/// Converts logical pixels to points.
pub fn px_to_pt(px: f32) -> f32 {
    px * PT_PER_INCH / PX_PER_INCH
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const ZERO: Point = Point { x: 0.0, y: 0.0 };
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    pub fn offset(self, dx: f32, dy: f32) -> Self {
        Self { x: self.x + dx, y: self.y + dy }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const ZERO: Size = Size { w: 0.0, h: 0.0 };
    pub const fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }
}

/// Edge insets (padding / margins).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Insets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Insets {
    pub const ZERO: Insets = Insets { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };
    pub const fn all(v: f32) -> Self {
        Self { left: v, top: v, right: v, bottom: v }
    }
    pub const fn symmetric(h: f32, v: f32) -> Self {
        Self { left: h, top: v, right: h, bottom: v }
    }
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }
}

/// Axis-aligned rectangle, origin top-left, y down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const ZERO: Rect = Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn from_origin_size(o: Point, s: Size) -> Self {
        Self { x: o.x, y: o.y, w: s.w, h: s.h }
    }
    pub fn origin(&self) -> Point {
        Point::new(self.x, self.y)
    }
    pub fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn center(&self) -> Point {
        Point::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.right() && p.y < self.bottom()
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    pub fn intersection(&self, o: &Rect) -> Option<Rect> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        (r > x && b > y).then(|| Rect::new(x, y, r - x, b - y))
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    pub fn inset(&self, i: Insets) -> Rect {
        Rect::new(
            self.x + i.left,
            self.y + i.top,
            (self.w - i.horizontal()).max(0.0),
            (self.h - i.vertical()).max(0.0),
        )
    }
    pub fn expand(&self, d: f32) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }
}

/// 8-bit sRGB colour with straight alpha.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    /// `0xRRGGBB`.
    pub const fn hex(v: u32) -> Self {
        Self::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
    pub fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }
    /// Parses `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` (leading `#` optional).
    pub fn parse_hex(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('#');
        let nib = |c: u8| -> Option<u8> { (c as char).to_digit(16).map(|d| d as u8) };
        let b = s.as_bytes();
        match b.len() {
            3 | 4 => {
                let mut v = [255u8; 4];
                for (i, c) in b.iter().enumerate() {
                    let n = nib(*c)?;
                    *v.get_mut(i)? = n * 17;
                }
                Some(Self::rgba(v[0], v[1], v[2], v[3]))
            }
            6 | 8 => {
                let mut v = [255u8; 4];
                for i in 0..b.len() / 2 {
                    let hi = nib(*b.get(2 * i)?)?;
                    let lo = nib(*b.get(2 * i + 1)?)?;
                    *v.get_mut(i)? = hi * 16 + lo;
                }
                Some(Self::rgba(v[0], v[1], v[2], v[3]))
            }
            _ => None,
        }
    }
    /// `#rrggbb` (alpha dropped when opaque, else `#rrggbbaa`).
    pub fn to_hex(&self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }
    /// Linear interpolation in sRGB space, `t` in 0..=1.
    pub fn lerp(self, o: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color::rgba(m(self.r, o.r), m(self.g, o.g), m(self.b, o.b), m(self.a, o.a))
    }
    /// Relative luminance (WCAG 2), 0..=1.
    pub fn luminance(&self) -> f32 {
        let ch = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(self.r) + 0.7152 * ch(self.g) + 0.0722 * ch(self.b)
    }
    /// WCAG contrast ratio between two colours (1..=21).
    pub fn contrast(&self, o: &Color) -> f32 {
        let (a, b) = (self.luminance(), o.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_round_trip() {
        assert_eq!(pt_to_px(12.0), 16.0);
        assert_eq!(px_to_pt(16.0), 12.0);
    }

    #[test]
    fn rect_ops() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert!(a.intersects(&b));
        assert_eq!(a.intersection(&b), Some(Rect::new(5.0, 5.0, 5.0, 5.0)));
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 15.0, 15.0));
        assert!(a.contains(Point::new(9.9, 0.0)));
        assert!(!a.contains(Point::new(10.0, 0.0)));
        assert_eq!(a.inset(Insets::all(2.0)), Rect::new(2.0, 2.0, 6.0, 6.0));
        assert_eq!(Rect::new(0.0, 0.0, 1.0, 1.0).intersection(&Rect::new(2.0, 2.0, 1.0, 1.0)), None);
    }

    #[test]
    fn colour_hex() {
        assert_eq!(Color::parse_hex("#58B8FE"), Some(Color::hex(0x58b8fe)));
        assert_eq!(Color::parse_hex("fff"), Some(Color::WHITE));
        assert_eq!(Color::parse_hex("#00000080"), Some(Color::rgba(0, 0, 0, 128)));
        assert_eq!(Color::parse_hex("#12"), None);
        assert_eq!(Color::parse_hex("zzzzzz"), None);
        assert_eq!(Color::hex(0x0a0b0c).to_hex(), "#0a0b0c");
        assert!((Color::BLACK.contrast(&Color::WHITE) - 21.0).abs() < 0.01);
    }
}
