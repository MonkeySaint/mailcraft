//! Shape Effects and Text Effects with their galleries and options: the parameters of `drawing.effect` /
//! `drawing.textEffect` (`shadow`, `reflection`, `glow`, `softEdges`, `bevel`, `rotation3d`, `preset`) and
//! the named variants the ribbon galleries offer.

use mailcraft_geom::Color;
use mailcraft_model::rich::{Drawing, Glow, Reflection, Shadow, ShadowKind, ShapeEffects};
use serde_json::Value;

use crate::CmdError;
use crate::compose::ComposeState;

/// Shadow variants: (id, label, kind, angle°, distance px, blur px).
pub const SHADOWS: [(&str, &str, ShadowKind, f32, f32, f32); 23] = [
    ("outerBottomRight", "Outer: Down and Right", ShadowKind::Outer, 45.0, 3.0, 4.0),
    ("outerBottom", "Outer: Down", ShadowKind::Outer, 90.0, 3.0, 4.0),
    ("outerBottomLeft", "Outer: Down and Left", ShadowKind::Outer, 135.0, 3.0, 4.0),
    ("outerRight", "Outer: Right", ShadowKind::Outer, 0.0, 3.0, 4.0),
    ("outerCenter", "Outer: All Around", ShadowKind::Outer, 0.0, 0.0, 8.0),
    ("outerLeft", "Outer: Left", ShadowKind::Outer, 180.0, 3.0, 4.0),
    ("outerTopRight", "Outer: Up and Right", ShadowKind::Outer, 315.0, 3.0, 4.0),
    ("outerTop", "Outer: Up", ShadowKind::Outer, 270.0, 3.0, 4.0),
    ("outerTopLeft", "Outer: Up and Left", ShadowKind::Outer, 225.0, 3.0, 4.0),
    ("innerTopLeft", "Inner: Top Left", ShadowKind::Inner, 45.0, 3.0, 4.0),
    ("innerTop", "Inner: Top", ShadowKind::Inner, 90.0, 3.0, 4.0),
    ("innerTopRight", "Inner: Top Right", ShadowKind::Inner, 135.0, 3.0, 4.0),
    ("innerLeft", "Inner: Left", ShadowKind::Inner, 0.0, 3.0, 4.0),
    ("innerCenter", "Inner: All Around", ShadowKind::Inner, 0.0, 0.0, 9.0),
    ("innerRight", "Inner: Right", ShadowKind::Inner, 180.0, 3.0, 4.0),
    ("innerBottomLeft", "Inner: Bottom Left", ShadowKind::Inner, 315.0, 3.0, 4.0),
    ("innerBottom", "Inner: Bottom", ShadowKind::Inner, 270.0, 3.0, 4.0),
    ("innerBottomRight", "Inner: Bottom Right", ShadowKind::Inner, 225.0, 3.0, 4.0),
    ("perspectiveUpperLeft", "Perspective: Behind, Left", ShadowKind::Perspective, 180.0, 6.0, 6.0),
    ("perspectiveUpperRight", "Perspective: Behind, Right", ShadowKind::Perspective, 0.0, 6.0, 6.0),
    ("perspectiveBelow", "Perspective: Under", ShadowKind::Perspective, 90.0, 0.0, 6.0),
    ("perspectiveLowerLeft", "Perspective: In Front, Left", ShadowKind::Perspective, 135.0, 4.0, 6.0),
    ("perspectiveLowerRight", "Perspective: In Front, Right", ShadowKind::Perspective, 45.0, 4.0, 6.0),
];

/// Reflection variants: (id, label, size 0..1, distance px).
pub const REFLECTIONS: [(&str, &str, f32, f32); 9] = [
    ("tightTouching", "Short, Touching", 0.33, 0.0),
    ("halfTouching", "Half, Touching", 0.5, 0.0),
    ("fullTouching", "Full, Touching", 1.0, 0.0),
    ("tight4", "Short, 4 px Apart", 0.33, 4.0),
    ("half4", "Half, 4 px Apart", 0.5, 4.0),
    ("full4", "Full, 4 px Apart", 1.0, 4.0),
    ("tight8", "Short, 8 px Apart", 0.33, 8.0),
    ("half8", "Half, 8 px Apart", 0.5, 8.0),
    ("full8", "Full, 8 px Apart", 1.0, 8.0),
];

/// Glow sizes offered with each accent colour (px).
pub const GLOW_SIZES: [f32; 4] = [5.0, 8.0, 11.0, 18.0];

/// Soft Edges sizes (px).
pub const SOFT_EDGES: [f32; 6] = [1.0, 2.5, 5.0, 10.0, 25.0, 50.0];

/// 3-D Rotation variants: (id, label, about the vertical axis °, about the horizontal axis °, perspective °).
pub const ROTATIONS: [(&str, &str, f32, f32, f32); 12] = [
    ("isometricLeft", "Parallel: Isometric Left", 45.0, 35.0, 0.0),
    ("isometricRight", "Parallel: Isometric Right", 315.0, 35.0, 0.0),
    ("isometricTop", "Parallel: Isometric Top", 315.0, 325.0, 0.0),
    ("isometricBottom", "Parallel: Isometric Bottom", 45.0, 325.0, 0.0),
    ("parallelLeft", "Parallel: Turned Left", 30.0, 0.0, 0.0),
    ("parallelRight", "Parallel: Turned Right", 330.0, 0.0, 0.0),
    ("perspectiveLeft", "Perspective: Turned Left", 30.0, 0.0, 45.0),
    ("perspectiveRight", "Perspective: Turned Right", 330.0, 0.0, 45.0),
    ("perspectiveAbove", "Perspective: Tilted Back", 0.0, 40.0, 45.0),
    ("perspectiveBelow", "Perspective: Tilted Forward", 0.0, 320.0, 45.0),
    ("perspectiveLeftStrong", "Perspective: Far Left", 50.0, 10.0, 80.0),
    ("perspectiveRightStrong", "Perspective: Far Right", 310.0, 10.0, 80.0),
];

/// Number of Shape Effects › Preset combinations.
pub const PRESETS: usize = 12;

fn shadow_named(name: &str) -> Option<Shadow> {
    SHADOWS.iter().find(|s| s.0 == name).map(|(_, _, kind, angle, distance, blur)| Shadow {
        kind: *kind,
        angle: *angle,
        distance: *distance,
        blur: *blur,
        ..Shadow::default()
    })
}

fn reflection_named(name: &str) -> Option<Reflection> {
    REFLECTIONS.iter().find(|r| r.0 == name).map(|(_, _, size, distance)| Reflection {
        size: *size,
        distance: *distance,
        ..Reflection::default()
    })
}

fn rotation_named(name: &str) -> Option<(f32, f32, f32)> {
    ROTATIONS.iter().find(|r| r.0 == name).map(|(_, _, x, y, p)| (*x, *y, *p))
}

/// Shape Effects › Preset `n` (1..=[`PRESETS`]): a combination of effects in `accent`.
pub fn preset(n: u64, accent: Color) -> Option<ShapeEffects> {
    let sh = |name: &str| shadow_named(name);
    let refl = |name: &str| reflection_named(name);
    let mut fx = ShapeEffects::default();
    match n {
        1 => fx.shadow = sh("outerBottomRight"),
        2 => {
            fx.shadow = sh("outerBottomRight");
            fx.bevel = "circle".into();
        }
        3 => {
            fx.bevel = "circle".into();
            fx.reflection = refl("tightTouching");
        }
        4 => {
            fx.glow = Some(Glow { color: Some(accent), size: 8.0, transparency: 0.4 });
            fx.bevel = "softRound".into();
        }
        5 => {
            fx.shadow = sh("innerCenter");
            fx.bevel = "relaxedInset".into();
        }
        6 => fx.shadow = sh("perspectiveBelow"),
        7 => {
            fx.bevel = "angle".into();
            fx.shadow = sh("outerBottom");
        }
        8 => {
            fx.bevel = "convex".into();
            fx.reflection = refl("halfTouching");
        }
        9 => {
            fx.soft_edges = 5.0;
            fx.shadow = sh("outerCenter");
        }
        10 => {
            fx.bevel = "coolSlant".into();
            fx.glow = Some(Glow { color: Some(accent), size: 5.0, transparency: 0.4 });
        }
        11 => {
            (fx.rot_x, fx.rot_y, fx.perspective) = (30.0, 0.0, 45.0);
            fx.shadow = sh("outerBottomRight");
        }
        12 => {
            fx.bevel = "artDeco".into();
            (fx.rot_x, fx.rot_y, fx.perspective) = (45.0, 35.0, 0.0);
        }
        _ => return None,
    }
    Some(fx)
}

/// The Shape Effects preset drawing `d` has (with `text`, its Text Effects preset), if any: what the
/// gallery checks. Presets are coloured as `drawing.effect` / `drawing.textEffect` colour them.
pub fn preset_of(d: &Drawing, c: &ComposeState, text: bool) -> Option<u64> {
    let fx = if text { d.text_fx.as_deref() } else { d.fx.as_deref() }?;
    let theme = c.theme_resolved().color(crate::compose::ACCENT1);
    let accent = if text { theme } else { d.fill.or(d.outline).unwrap_or(theme) };
    (1..=PRESETS as u64).find(|n| preset(*n, accent).as_ref() == Some(fx))
}

/// Whether `p` asks for an effect with options (any key this module reads).
pub fn has_options(p: &Value) -> bool {
    ["shadow", "reflection", "glow", "softEdges", "bevel", "bevelSize", "rotation3d", "preset"]
        .iter()
        .any(|k| p.get(*k).is_some())
}

/// A colour in an option object: `color` (CSS colour, `null` = automatic) or `theme` + `tint`.
fn option_color(c: &ComposeState, o: &Value) -> Result<Option<Option<Color>>, CmdError> {
    if let Some(key) = o.get("theme").and_then(Value::as_str) {
        let tint = o.get("tint").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|t| t.parse().ok())));
        let col = crate::cmd::format::theme_color(c, key, tint.unwrap_or(0))
            .ok_or_else(|| CmdError::Params(format!("unknown theme colour `{key}`")))?;
        return Ok(Some(Some(col)));
    }
    match o.get("color") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(t)) => mailcraft_html::parse_css_color(t)
            .map(|col| Some(Some(col)))
            .ok_or_else(|| CmdError::Params(format!("`{t}` is not a colour"))),
        Some(_) => Err(CmdError::Params("`color` must be a colour".into())),
    }
}

fn option_num(o: &Value, key: &str) -> Result<Option<f32>, CmdError> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .or_else(|| {
                v.as_str().and_then(|t| t.trim().trim_end_matches(['%', 'x', 'p']).trim().parse().ok())
            })
            .filter(|n| n.is_finite())
            .map(|n| Some(n as f32))
            .ok_or_else(|| CmdError::Params(format!("`{key}` must be a number"))),
    }
}

/// Transparency given as 0..1, or as a percentage (`"40%"`, or a number above 1).
fn transparency(o: &Value) -> Result<Option<f32>, CmdError> {
    let pct = o.get("transparency").and_then(Value::as_str).is_some_and(|t| t.trim_end().ends_with('%'));
    Ok(option_num(o, "transparency")?.map(|t| if pct || t > 1.0 { t / 100.0 } else { t }.clamp(0.0, 1.0)))
}

/// Applies the effect options of `p` to `fx` (starting from `fx` as it is). `accent` colours presets.
pub fn apply(fx: &mut ShapeEffects, p: &Value, c: &ComposeState, accent: Color) -> Result<(), CmdError> {
    if let Some(v) = p.get("preset") {
        match v.as_u64() {
            Some(0) => *fx = ShapeEffects::default(),
            Some(n) => {
                *fx = preset(n, accent)
                    .ok_or_else(|| CmdError::Params(format!("`preset` is 0 (none) or 1 to {PRESETS}")))?;
            }
            None => return Err(CmdError::Params("`preset` must be a number".into())),
        }
    }
    match p.get("shadow") {
        None => {}
        Some(Value::Null) => fx.shadow = None,
        Some(Value::String(name)) if name.is_empty() || name == "none" => fx.shadow = None,
        Some(Value::String(name)) => {
            let keep = fx.shadow;
            let mut s =
                shadow_named(name).ok_or_else(|| CmdError::Params(format!("unknown shadow `{name}`")))?;
            // A new direction keeps the colour and transparency chosen before.
            if let Some(k) = keep {
                s.color = k.color;
                s.transparency = k.transparency;
            }
            fx.shadow = Some(s);
        }
        Some(o @ Value::Object(_)) => {
            let mut s = fx.shadow.unwrap_or_default();
            if let Some(kind) = o.get("kind").and_then(Value::as_str) {
                s.kind = match kind {
                    "outer" => ShadowKind::Outer,
                    "inner" => ShadowKind::Inner,
                    "perspective" => ShadowKind::Perspective,
                    other => return Err(CmdError::Params(format!("unknown shadow kind `{other}`"))),
                };
            }
            if let Some(col) = option_color(c, o)? {
                s.color = col;
            }
            if let Some(t) = transparency(o)? {
                s.transparency = t;
            }
            if let Some(v) = option_num(o, "blur")? {
                s.blur = v;
            }
            if let Some(v) = option_num(o, "angle")? {
                s.angle = v;
            }
            if let Some(v) = option_num(o, "distance")? {
                s.distance = v;
            }
            fx.shadow = Some(s);
        }
        Some(_) => return Err(CmdError::Params("`shadow` is a shadow name, options or null".into())),
    }
    match p.get("reflection") {
        None => {}
        Some(Value::Null) => fx.reflection = None,
        Some(Value::String(name)) if name.is_empty() || name == "none" => fx.reflection = None,
        Some(Value::String(name)) => {
            fx.reflection = Some(
                reflection_named(name)
                    .ok_or_else(|| CmdError::Params(format!("unknown reflection `{name}`")))?,
            );
        }
        Some(o @ Value::Object(_)) => {
            let mut r = fx.reflection.unwrap_or_default();
            if let Some(t) = transparency(o)? {
                r.transparency = t;
            }
            if let Some(v) = option_num(o, "size")? {
                r.size = if v > 1.0 { v / 100.0 } else { v };
            }
            if let Some(v) = option_num(o, "distance")? {
                r.distance = v;
            }
            if let Some(v) = option_num(o, "blur")? {
                r.blur = v;
            }
            fx.reflection = Some(r);
        }
        Some(_) => return Err(CmdError::Params("`reflection` is a reflection name, options or null".into())),
    }
    match p.get("glow") {
        None => {}
        Some(Value::Null) => fx.glow = None,
        Some(Value::String(name)) if name.is_empty() || name == "none" => fx.glow = None,
        Some(o @ Value::Object(_)) => {
            let mut g = fx.glow.unwrap_or_default();
            if let Some(col) = option_color(c, o)? {
                g.color = col;
            }
            if let Some(v) = option_num(o, "size")? {
                g.size = v;
            }
            if let Some(t) = transparency(o)? {
                g.transparency = t;
            }
            fx.glow = Some(g);
        }
        Some(_) => return Err(CmdError::Params("`glow` is glow options or null".into())),
    }
    if let Some(v) = option_num(p, "softEdges")? {
        fx.soft_edges = v.max(0.0);
    }
    match p.get("bevel") {
        None => {}
        Some(Value::Null) => fx.bevel.clear(),
        Some(Value::String(b)) if b == "none" => fx.bevel.clear(),
        Some(Value::String(b)) if crate::cmd::insert::drawing::BEVELS.contains(&b.as_str()) => {
            fx.bevel.clone_from(b)
        }
        Some(other) => return Err(CmdError::Params(format!("unknown bevel {other}"))),
    }
    if let Some(v) = option_num(p, "bevelSize")? {
        fx.bevel_size = v.max(0.0);
    }
    match p.get("rotation3d") {
        None => {}
        Some(Value::Null) => (fx.rot_x, fx.rot_y, fx.perspective) = (0.0, 0.0, 0.0),
        Some(Value::String(name)) if name.is_empty() || name == "none" => {
            (fx.rot_x, fx.rot_y, fx.perspective) = (0.0, 0.0, 0.0);
        }
        Some(Value::String(name)) => {
            (fx.rot_x, fx.rot_y, fx.perspective) = rotation_named(name)
                .ok_or_else(|| CmdError::Params(format!("unknown 3-D rotation `{name}`")))?;
        }
        Some(o @ Value::Object(_)) => {
            if let Some(v) = option_num(o, "x")? {
                fx.rot_x = v;
            }
            if let Some(v) = option_num(o, "y")? {
                fx.rot_y = v;
            }
            if let Some(v) = option_num(o, "perspective")? {
                fx.perspective = v;
            }
        }
        Some(_) => return Err(CmdError::Params("`rotation3d` is a rotation name, options or null".into())),
    }
    Ok(())
}

/// The effects with options a legacy effect name stands for (so options can be added to it).
pub fn from_legacy(effect: &str) -> ShapeEffects {
    let mut fx = ShapeEffects::default();
    match effect {
        "shadow" => fx.shadow = shadow_named("outerBottomRight"),
        "glow" => fx.glow = Some(Glow { size: 6.0, ..Glow::default() }),
        "reflection" => fx.reflection = reflection_named("halfTouching"),
        "bevel" => fx.bevel = "circle".into(),
        "softEdges" => fx.soft_edges = 4.0,
        _ => {}
    }
    fx
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn options_and_variants_build_effects() {
        let c = ComposeState::default();
        let accent = Color::rgb(0, 0, 200);
        let mut fx = ShapeEffects::default();
        apply(&mut fx, &json!({"shadow": "innerTop"}), &c, accent).unwrap();
        assert_eq!(fx.shadow.unwrap().kind, ShadowKind::Inner);
        apply(
            &mut fx,
            &json!({"shadow": {"color": "#ff0000", "transparency": "30%", "distance": 9}}),
            &c,
            accent,
        )
        .unwrap();
        let s = fx.shadow.unwrap();
        assert_eq!((s.color, s.distance), (Some(Color::rgb(255, 0, 0)), 9.0));
        assert!((s.transparency - 0.3).abs() < 1e-6);
        apply(&mut fx, &json!({"shadow": "outerBottom"}), &c, accent).unwrap();
        assert_eq!(fx.shadow.unwrap().color, Some(Color::rgb(255, 0, 0)), "a new direction keeps the colour");
        apply(
            &mut fx,
            &json!({"glow": {"theme": "accent2", "size": 11}, "softEdges": 5, "bevel": "angle"}),
            &c,
            accent,
        )
        .unwrap();
        assert_eq!(fx.glow.unwrap().size, 11.0);
        assert!(fx.glow.unwrap().color.is_some());
        assert_eq!((fx.soft_edges, fx.bevel.as_str()), (5.0, "angle"));
        apply(&mut fx, &json!({"rotation3d": "perspectiveLeft", "reflection": "full8"}), &c, accent).unwrap();
        assert_eq!((fx.rot_x, fx.perspective), (30.0, 45.0));
        assert_eq!(fx.reflection.unwrap().distance, 8.0);
        apply(&mut fx, &json!({"shadow": null, "glow": null, "reflection": null, "rotation3d": null, "bevel": null, "softEdges": 0}), &c, accent)
            .unwrap();
        assert!(fx.is_empty());
        for n in 1..=PRESETS as u64 {
            assert!(!preset(n, accent).unwrap().is_empty(), "preset {n}");
        }
        for bad in [
            json!({"shadow": "sideways"}),
            json!({"bevel": "wobbly"}),
            json!({"preset": 99}),
            json!({"glow": 5}),
        ] {
            assert!(apply(&mut fx, &bad, &c, accent).is_err(), "{bad}");
        }
    }
}
