//! CSS borders → [`BorderEdge`]s: the `border` shorthands, the per-side and per-part longhands and the
//! Word-style `mso-border-…-alt` declarations the serializer writes for the lines paragraphs sharing one
//! box leave out.

use crate::css::{self, Props};
use mailcraft_model::{BorderEdge, BorderLine};

/// Sides in CSS box order.
pub(crate) const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

/// Widest border accepted (px).
const MAX_BORDER_PX: f32 = 100.0;

#[derive(Clone, Debug, Default)]
struct Parts {
    /// `Some(None)`: `none` / `hidden`.
    style: Option<Option<BorderLine>>,
    width: Option<f32>,
    color: Option<Option<mailcraft_geom::Color>>,
}

impl Parts {
    fn edge(&self) -> Option<BorderEdge> {
        let line = self.style.flatten()?;
        let w = self.width.unwrap_or(3.0);
        if !w.is_finite() || w <= 0.0 {
            return None;
        }
        Some(BorderEdge::from_thickness(line, self.color.flatten(), w.min(MAX_BORDER_PX)))
    }
}

fn style_of(t: &str) -> Option<Option<BorderLine>> {
    let t = t.trim().to_ascii_lowercase();
    if t == "none" || t == "hidden" {
        return Some(None);
    }
    BorderLine::from_name(&t).filter(|_| t != "single").map(Some)
}

fn width_of(t: &str, font_px: f32) -> Option<f32> {
    match t.trim().to_ascii_lowercase().as_str() {
        "thin" => Some(1.0),
        "medium" => Some(3.0),
        "thick" => Some(5.0),
        "0" => Some(0.0),
        v => css::length_px(v, font_px).filter(|w| w.is_finite() && *w >= 0.0),
    }
}

/// Applies a `border` / `border-side` shorthand (`1px solid #ccc`, any order).
fn shorthand(parts: &mut Parts, v: &str, font_px: f32) {
    // A shorthand resets what it leaves out.
    *parts = Parts { style: Some(None), width: None, color: Some(None) };
    for t in css::split_top(v.trim(), ' ') {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(s) = style_of(t) {
            parts.style = Some(s);
        } else if let Some(w) = width_of(t, font_px) {
            parts.width = Some(w);
        } else if let Some(c) = css::parse_color(t) {
            parts.color = Some((c.a > 0).then_some(c));
        }
    }
}

/// The value for side `i` (CSS box order) of a 1-4 value property.
fn box_value(v: &str, i: usize) -> Option<String> {
    let vals: Vec<&str> =
        css::split_top(v.trim(), ' ').into_iter().filter(|t| !t.trim().is_empty()).collect();
    let pick = match (vals.len(), i) {
        (0, _) => return None,
        (1, _) => 0,
        (2, 0 | 2) | (3, 0) => 0,
        (2, _) | (3, 1 | 3) => 1,
        (3, _) => 2,
        (_, k) => k.min(3),
    };
    vals.get(pick).map(|s| s.trim().to_owned())
}

/// The border edges an element declares, in CSS box order (top, right, bottom, left).
pub(crate) fn edges(p: &Props, font_px: f32) -> [Option<BorderEdge>; 4] {
    let mut parts: [Parts; 4] = Default::default();
    if let Some(v) = p.get("border") {
        for part in &mut parts {
            shorthand(part, v, font_px);
        }
    }
    for (i, part) in parts.iter_mut().enumerate() {
        if let Some(s) = p.get("border-style").and_then(|v| box_value(v, i)).and_then(|t| style_of(&t)) {
            part.style = Some(s);
        }
        if let Some(w) =
            p.get("border-width").and_then(|v| box_value(v, i)).and_then(|t| width_of(&t, font_px))
        {
            part.width = Some(w);
        }
        if let Some(c) =
            p.get("border-color").and_then(|v| box_value(v, i)).and_then(|t| css::parse_color(&t))
        {
            part.color = Some(Some(c));
        }
    }
    for (side, part) in SIDES.iter().zip(parts.iter_mut()) {
        if let Some(v) = p.get(&format!("border-{side}")) {
            shorthand(part, v, font_px);
        }
        if let Some(s) = p.get(&format!("border-{side}-style")).and_then(style_of) {
            part.style = Some(s);
        }
        if let Some(w) = p.get(&format!("border-{side}-width")).and_then(|t| width_of(t, font_px)) {
            part.width = Some(w);
        }
        if let Some(c) = p.get(&format!("border-{side}-color")).and_then(css::parse_color) {
            part.color = Some(Some(c));
        }
    }
    let mut out: [Option<BorderEdge>; 4] = Default::default();
    for (o, part) in out.iter_mut().zip(parts.iter()) {
        *o = part.edge();
    }
    out
}

/// An edge kept in `mso-border-{side}-alt` (a line a shared paragraph box leaves out): `None` when not
/// declared, `Some(None)` for `none`.
pub(crate) fn alt_edge(p: &Props, side: &str, font_px: f32) -> Option<Option<BorderEdge>> {
    let v = p.get(&format!("mso-border-{side}-alt"))?;
    let mut part = Parts::default();
    shorthand(&mut part, v, font_px);
    Some(part.edge())
}

/// The line between paragraphs sharing a box (`mso-border-between`).
pub(crate) fn between_edge(p: &Props, font_px: f32) -> Option<BorderEdge> {
    let v = p.get("mso-border-between")?;
    let mut part = Parts::default();
    shorthand(&mut part, v, font_px);
    part.edge()
}

/// Whether the element declares a drop shadow (the Shadow border setting).
pub(crate) fn has_shadow(p: &Props) -> bool {
    p.get("box-shadow").is_some_and(|v| {
        let v = v.trim();
        !v.is_empty() && !v.eq_ignore_ascii_case("none")
    })
}

/// The CSS properties this module reads (for the property allow-list).
pub(crate) fn is_border_property(p: &str) -> bool {
    if matches!(
        p,
        "border" | "border-style" | "border-width" | "border-color" | "box-shadow" | "mso-border-between"
    ) {
        return true;
    }
    if let Some(rest) = p.strip_prefix("mso-border-") {
        return SIDES.iter().any(|s| rest == format!("{s}-alt"));
    }
    let Some(rest) = p.strip_prefix("border-") else {
        return false;
    };
    SIDES.iter().any(|s| {
        rest == *s || rest.strip_prefix(s).is_some_and(|t| matches!(t, "-style" | "-width" | "-color"))
    })
}
