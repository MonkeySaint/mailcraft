//! Shape geometry for drawing objects (Insert › Shapes, Shape Format › Change Shape): every shape of the
//! gallery as polygons in its box, drawn for MailCraft from plain geometry (regular polygons, ellipse arcs,
//! Bézier curves). Names follow the gallery's observed labels; geometry ids are our own.
//!
//! A [`Geometry`] is a filled area (sub-paths filled together with the even-odd rule, so frames and rings
//! have holes), tinted faces drawn over it (cube sides, cylinder tops), open lines drawn with the outline
//! (lines, connectors, brackets, detail lines) and arrowheads.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

/// A point in px inside the shape's box.
pub type P = (f32, f32);

/// A shape's drawing in a `w`×`h` box.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    /// Filled area: sub-paths filled together (even-odd).
    pub fill: Vec<Vec<P>>,
    /// Faces filled over the area with the fill colour lightened (`> 0`) or darkened (`< 0`) by the factor
    /// (`0` = the fill colour itself); they count as part of the shape's area.
    pub faces: Vec<(Vec<P>, f32)>,
    /// Open lines drawn with the outline.
    pub lines: Vec<Vec<P>>,
    /// Arrowheads: (tip, the point the line comes from).
    pub arrows: Vec<(P, P)>,
}

impl Geometry {
    /// Whether the shape is only lines (no area to fill).
    pub fn is_line(&self) -> bool {
        self.fill.is_empty() && self.faces.is_empty()
    }
}

/// Gallery categories, in menu order.
pub const CATEGORIES: [&str; 8] = [
    "Lines",
    "Rectangles",
    "Basic Shapes",
    "Block Arrows",
    "Equation Shapes",
    "Flowchart",
    "Stars and Banners",
    "Callouts",
];

/// Every gallery shape: (id, label, category index into [`CATEGORIES`]).
pub const SHAPES: &[(&str, &str, usize)] = &[
    ("line", "Line", 0),
    ("lineArrow", "Line Arrow", 0),
    ("doubleArrow", "Line Arrow: Double", 0),
    ("elbowConnector", "Connector: Elbow", 0),
    ("elbowArrow", "Connector: Elbow Arrow", 0),
    ("elbowDoubleArrow", "Connector: Elbow Double-Arrow", 0),
    ("curvedConnector", "Connector: Curved", 0),
    ("curvedArrow", "Connector: Curved Arrow", 0),
    ("curvedDoubleArrow", "Connector: Curved Double-Arrow", 0),
    ("curve", "Curve", 0),
    ("freeform", "Freeform: Shape", 0),
    ("scribble", "Freeform: Scribble", 0),
    ("rectangle", "Rectangle", 1),
    ("roundedRectangle", "Rectangle: Rounded Corners", 1),
    ("snip1Rectangle", "Rectangle: Single Corner Snipped", 1),
    ("snip2SameRectangle", "Rectangle: Top Corners Snipped", 1),
    ("snip2DiagRectangle", "Rectangle: Diagonal Corners Snipped", 1),
    ("snipRoundRectangle", "Rectangle: Top Corners One Rounded and One Snipped", 1),
    ("round1Rectangle", "Rectangle: Single Corner Rounded", 1),
    ("round2SameRectangle", "Rectangle: Top Corners Rounded", 1),
    ("round2DiagRectangle", "Rectangle: Diagonal Corners Rounded", 1),
    ("textBox", "Text Box", 2),
    ("oval", "Oval", 2),
    ("triangle", "Isosceles Triangle", 2),
    ("rightTriangle", "Right Triangle", 2),
    ("parallelogram", "Parallelogram", 2),
    ("trapezoid", "Trapezoid", 2),
    ("diamond", "Diamond", 2),
    ("pentagon", "Pentagon", 2),
    ("hexagon", "Hexagon", 2),
    ("heptagon", "Heptagon", 2),
    ("octagon", "Octagon", 2),
    ("decagon", "Decagon", 2),
    ("dodecagon", "Dodecagon", 2),
    ("pie", "Partial Circle", 2),
    ("chord", "Chord", 2),
    ("teardrop", "Teardrop", 2),
    ("frame", "Frame", 2),
    ("halfFrame", "Half Frame", 2),
    ("lShape", "L-Shape", 2),
    ("diagonalStripe", "Diagonal Stripe", 2),
    ("cross", "Cross", 2),
    ("plaque", "Plaque", 2),
    ("can", "Cylinder", 2),
    ("cube", "Cube", 2),
    ("bevel", "Rectangle: Beveled", 2),
    ("donut", "Circle: Hollow", 2),
    ("blockArc", "Block Arc", 2),
    ("foldedCorner", "Rectangle: Folded Corner", 2),
    ("smiley", "Smiley Face", 2),
    ("heart", "Heart", 2),
    ("lightning", "Lightning Bolt", 2),
    ("sun", "Sun", 2),
    ("moon", "Moon", 2),
    ("cloud", "Cloud", 2),
    ("arc", "Arc", 2),
    ("doubleBracket", "Double Bracket", 2),
    ("doubleBrace", "Double Brace", 2),
    ("leftBracket", "Left Bracket", 2),
    ("rightBracket", "Right Bracket", 2),
    ("leftBrace", "Left Brace", 2),
    ("rightBrace", "Right Brace", 2),
    ("rightArrow", "Arrow: Right", 3),
    ("leftArrow", "Arrow: Left", 3),
    ("upArrow", "Arrow: Up", 3),
    ("downArrow", "Arrow: Down", 3),
    ("leftRightArrow", "Arrow: Left-Right", 3),
    ("upDownArrow", "Arrow: Up-Down", 3),
    ("quadArrow", "Arrow: Quad", 3),
    ("leftRightUpArrow", "Arrow: Left-Right-Up", 3),
    ("bentArrow", "Arrow: Bent", 3),
    ("uturnArrow", "Arrow: U-Turn", 3),
    ("leftUpArrow", "Arrow: Left-Up", 3),
    ("bentUpArrow", "Arrow: Bent-Up", 3),
    ("curvedRightArrow", "Arrow: Curved Right", 3),
    ("curvedLeftArrow", "Arrow: Curved Left", 3),
    ("curvedUpArrow", "Arrow: Curved Up", 3),
    ("curvedDownArrow", "Arrow: Curved Down", 3),
    ("stripedRightArrow", "Arrow: Striped Right", 3),
    ("notchedRightArrow", "Arrow: Notched Right", 3),
    ("homePlate", "Arrow: Pentagon", 3),
    ("chevron", "Arrow: Chevron", 3),
    ("rightArrowCallout", "Callout: Right Arrow", 3),
    ("downArrowCallout", "Callout: Down Arrow", 3),
    ("leftArrowCallout", "Callout: Left Arrow", 3),
    ("upArrowCallout", "Callout: Up Arrow", 3),
    ("leftRightArrowCallout", "Callout: Left-Right Arrow", 3),
    ("quadArrowCallout", "Callout: Quad Arrow", 3),
    ("circularArrow", "Arrow: Circular", 3),
    ("mathPlus", "Plus Sign", 4),
    ("mathMinus", "Minus Sign", 4),
    ("mathMultiply", "Multiplication Sign", 4),
    ("mathDivide", "Division Sign", 4),
    ("mathEqual", "Equals", 4),
    ("mathNotEqual", "Not Equal", 4),
    ("flowProcess", "Flowchart: Process", 5),
    ("flowAlternateProcess", "Flowchart: Alternate Process", 5),
    ("flowDecision", "Flowchart: Decision", 5),
    ("flowData", "Flowchart: Data", 5),
    ("flowPredefinedProcess", "Flowchart: Predefined Process", 5),
    ("flowInternalStorage", "Flowchart: Internal Storage", 5),
    ("flowDocument", "Flowchart: Document", 5),
    ("flowMultidocument", "Flowchart: Multidocument", 5),
    ("flowTerminator", "Flowchart: Terminator", 5),
    ("flowPreparation", "Flowchart: Preparation", 5),
    ("flowManualInput", "Flowchart: Manual Input", 5),
    ("flowManualOperation", "Flowchart: Manual Operation", 5),
    ("flowConnector", "Flowchart: Connector", 5),
    ("flowOffpageConnector", "Flowchart: Off-page Connector", 5),
    ("flowCard", "Flowchart: Card", 5),
    ("flowPunchedTape", "Flowchart: Punched Tape", 5),
    ("flowSummingJunction", "Flowchart: Summing Junction", 5),
    ("flowOr", "Flowchart: Or", 5),
    ("flowCollate", "Flowchart: Collate", 5),
    ("flowSort", "Flowchart: Sort", 5),
    ("flowExtract", "Flowchart: Extract", 5),
    ("flowMerge", "Flowchart: Merge", 5),
    ("flowStoredData", "Flowchart: Stored Data", 5),
    ("flowDelay", "Flowchart: Delay", 5),
    ("flowSequentialAccess", "Flowchart: Sequential Access Storage", 5),
    ("flowMagneticDisk", "Flowchart: Magnetic Disk", 5),
    ("flowDirectAccess", "Flowchart: Direct Access Storage", 5),
    ("flowDisplay", "Flowchart: Display", 5),
    ("explosion1", "Explosion: 8 Points", 6),
    ("explosion2", "Explosion: 14 Points", 6),
    ("star4", "Star: 4 Points", 6),
    ("star5", "Star: 5 Points", 6),
    ("star6", "Star: 6 Points", 6),
    ("star7", "Star: 7 Points", 6),
    ("star8", "Star: 8 Points", 6),
    ("star10", "Star: 10 Points", 6),
    ("star12", "Star: 12 Points", 6),
    ("star16", "Star: 16 Points", 6),
    ("star24", "Star: 24 Points", 6),
    ("star32", "Star: 32 Points", 6),
    ("ribbonUp", "Ribbon: Tilted Up", 6),
    ("ribbonDown", "Ribbon: Tilted Down", 6),
    ("curvedRibbonUp", "Ribbon: Curved and Tilted Up", 6),
    ("curvedRibbonDown", "Ribbon: Curved and Tilted Down", 6),
    ("verticalScroll", "Scroll: Vertical", 6),
    ("horizontalScroll", "Scroll: Horizontal", 6),
    ("wave", "Wave", 6),
    ("doubleWave", "Double Wave", 6),
    ("wedgeRectCallout", "Speech Bubble: Rectangle", 7),
    ("wedgeRoundRectCallout", "Speech Bubble: Rectangle with Corners Rounded", 7),
    ("wedgeEllipseCallout", "Speech Bubble: Oval", 7),
    ("cloudCallout", "Thought Bubble: Cloud", 7),
    ("lineCallout1", "Callout: Line", 7),
    ("lineCallout2", "Callout: Bent Line", 7),
    ("lineCallout3", "Callout: Double Bent Line", 7),
    ("accentCallout1", "Callout: Line with Accent Bar", 7),
    ("accentCallout2", "Callout: Bent Line with Accent Bar", 7),
    ("accentCallout3", "Callout: Double Bent Line with Accent Bar", 7),
    ("noBorderCallout1", "Callout: Line with No Border", 7),
    ("noBorderCallout2", "Callout: Bent Line with No Border", 7),
    ("noBorderCallout3", "Callout: Double Bent Line with No Border", 7),
    ("accentBorderCallout1", "Callout: Line with Border and Accent Bar", 7),
    ("accentBorderCallout2", "Callout: Bent Line with Border and Accent Bar", 7),
    ("accentBorderCallout3", "Callout: Double Bent Line with Border and Accent Bar", 7),
];

/// Older and shorter names still accepted, and what they mean now.
const ALIASES: [(&str, &str); 6] = [
    ("arrow", "rightArrow"),
    ("plus", "cross"),
    ("star", "star5"),
    ("callout", "wedgeRectCallout"),
    ("ellipse", "oval"),
    ("square", "rectangle"),
];

/// The gallery id `name` stands for (case-insensitive; older names included), with `circle` and `canvas`
/// kept as their own.
pub fn canonical(name: &str) -> Option<&'static str> {
    let n = name.trim();
    if let Some((_, to)) = ALIASES.iter().find(|(from, _)| from.eq_ignore_ascii_case(n)) {
        return Some(to);
    }
    if n.eq_ignore_ascii_case("circle") {
        return Some("circle");
    }
    if n.eq_ignore_ascii_case("canvas") {
        return Some("canvas");
    }
    SHAPES
        .iter()
        .find(|(id, label, _)| id.eq_ignore_ascii_case(n) || label.eq_ignore_ascii_case(n))
        .map(|s| s.0)
}

/// The gallery label of shape `id` (the id itself when it has none).
pub fn label(id: &str) -> &str {
    match id {
        "circle" => "Circle",
        "canvas" => "Drawing Canvas",
        _ => SHAPES.iter().find(|(i, ..)| *i == id).map_or(id, |s| s.1),
    }
}

/// Shapes drawn as lines only: they take the outline colour and no fill.
pub fn is_line_shape(id: &str) -> bool {
    matches!(
        id,
        "line"
            | "lineArrow"
            | "doubleArrow"
            | "elbowConnector"
            | "elbowArrow"
            | "elbowDoubleArrow"
            | "curvedConnector"
            | "curvedArrow"
            | "curvedDoubleArrow"
            | "curve"
            | "scribble"
            | "arc"
            | "doubleBracket"
            | "doubleBrace"
            | "leftBracket"
            | "rightBracket"
            | "leftBrace"
            | "rightBrace"
    )
}

/// The size a shape gets when it is inserted without one (px).
pub fn default_size(id: &str) -> (f32, f32) {
    match id {
        "circle"
        | "oval"
        | "donut"
        | "smiley"
        | "sun"
        | "pie"
        | "chord"
        | "teardrop"
        | "blockArc"
        | "flowConnector"
        | "flowOr"
        | "flowSummingJunction"
        | "circularArrow"
        | "cube"
        | "explosion1"
        | "explosion2" => (100.0, 100.0),
        id if id.starts_with("star") => (100.0, 100.0),
        id if id.starts_with("math") => (100.0, 100.0),
        "leftBracket" | "rightBracket" | "leftBrace" | "rightBrace" => (24.0, 100.0),
        "upArrow" | "downArrow" | "upDownArrow" | "verticalScroll" | "can" | "moon" => (80.0, 120.0),
        "canvas" => (480.0, 240.0),
        "textBox" => (240.0, 120.0),
        _ => (160.0, 100.0),
    }
}

/// Text area inside a shape's box: (x, y, w, h) in px.
pub fn text_rect(id: &str, w: f32, h: f32) -> (f32, f32, f32, f32) {
    let (fx, fy) = match id {
        "oval"
        | "circle"
        | "flowConnector"
        | "wedgeEllipseCallout"
        | "smiley"
        | "donut"
        | "sun"
        | "cloud"
        | "cloudCallout" => (0.15, 0.15),
        "diamond" | "flowDecision" | "triangle" | "star5" | "star4" | "star6" | "star7" | "star8"
        | "explosion1" | "explosion2" | "flowSort" | "flowExtract" | "flowMerge" => (0.25, 0.25),
        "can" | "flowMagneticDisk" => (0.05, 0.2),
        _ => (0.0, 0.0),
    };
    let (x, y) = (w * fx + 6.0, h * fy + 4.0);
    (x, y, (w - 2.0 * x).max(4.0), (h - 2.0 * y).max(4.0))
}

// ---------------------------------------------------------------------------------------------------------

fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<P> {
    arc(cx, cy, rx, ry, 0.0, 360.0)
}

/// Points along an ellipse arc from `a0` to `a1` degrees (clockwise on screen, 0° = +x).
fn arc(cx: f32, cy: f32, rx: f32, ry: f32, a0: f32, a1: f32) -> Vec<P> {
    let steps = (((a1 - a0).abs() / 360.0) * 96.0).ceil().clamp(2.0, 96.0) as usize;
    (0..=steps)
        .map(|i| {
            let a = (a0 + (a1 - a0) * i as f32 / steps as f32).to_radians();
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

fn bezier(out: &mut Vec<P>, p0: P, p1: P, p2: P, p3: P) {
    for i in 1..=16 {
        let t = i as f32 / 16.0;
        let u = 1.0 - t;
        let x = u * u * u * p0.0 + 3.0 * u * u * t * p1.0 + 3.0 * u * t * t * p2.0 + t * t * t * p3.0;
        let y = u * u * u * p0.1 + 3.0 * u * u * t * p1.1 + 3.0 * u * t * t * p2.1 + t * t * t * p3.1;
        out.push((x, y));
    }
}

/// A regular polygon (or a star with `inner` radius ratio) filling the box, first point at the top.
fn regular(w: f32, h: f32, n: usize, inner: Option<f32>) -> Vec<P> {
    let k = if inner.is_some() { n * 2 } else { n };
    (0..k)
        .map(|i| {
            let a = TAU * i as f32 / k as f32 - FRAC_PI_2;
            let r = match inner {
                Some(ri) if i % 2 == 1 => ri,
                _ => 1.0,
            };
            (w / 2.0 + w / 2.0 * r * a.cos(), h / 2.0 + h / 2.0 * r * a.sin())
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Corner {
    Square,
    Round,
    Snip,
    Concave,
}

/// A rectangle with each corner (top-left, top-right, bottom-right, bottom-left) square, rounded, snipped or
/// cut in, at radius `r`.
fn corners(x: f32, y: f32, w: f32, h: f32, kinds: [Corner; 4], r: f32) -> Vec<P> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let spots = [(x, y, 180.0f32), (x + w, y, 270.0), (x + w, y + h, 0.0), (x, y + h, 90.0)];
    let mut out = Vec::new();
    for ((cx, cy, start), kind) in spots.into_iter().zip(kinds) {
        // Unit vectors from the corner along the two edges it joins (in drawing order).
        let (inx, iny) = match start as i32 {
            180 => ((0.0, 1.0), (1.0, 0.0)),
            270 => ((-1.0, 0.0), (0.0, 1.0)),
            0 => ((0.0, -1.0), (-1.0, 0.0)),
            _ => ((1.0, 0.0), (0.0, -1.0)),
        };
        let a = (cx + inx.0 * r, cy + inx.1 * r);
        let b = (cx + iny.0 * r, cy + iny.1 * r);
        match kind {
            Corner::Square => out.push((cx, cy)),
            Corner::Snip => {
                out.push(a);
                out.push(b);
            }
            Corner::Round => {
                let (ox, oy) = (cx + (inx.0 + iny.0) * r, cy + (inx.1 + iny.1) * r);
                out.extend(arc(ox, oy, r, r, start, start + 90.0));
            }
            Corner::Concave => {
                // An arc around the corner itself, from the first edge point to the second (90°).
                let a0 = inx.1.atan2(inx.0).to_degrees();
                let mut diff = iny.1.atan2(iny.0).to_degrees() - a0;
                if diff > 180.0 {
                    diff -= 360.0;
                } else if diff < -180.0 {
                    diff += 360.0;
                }
                out.extend(arc(cx, cy, r, r, a0, a0 + diff));
            }
        }
    }
    out
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Vec<P> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
}

fn map(pts: &[P], w: f32, h: f32) -> Vec<P> {
    pts.iter().map(|(x, y)| (x * w, y * h)).collect()
}

/// Line ends from `points` (two or more fractions of the box), or the default diagonal.
fn ends(points: &[P], w: f32, h: f32, default: [P; 2]) -> (P, P) {
    let (a, b) = match (points.first(), points.last()) {
        (Some(a), Some(b)) if points.len() >= 2 => (*a, *b),
        _ => (default[0], default[1]),
    };
    ((a.0 * w, a.1 * h), (b.0 * w, b.1 * h))
}

fn star_points(id: &str) -> Option<(usize, f32)> {
    Some(match id {
        "star4" => (4, 0.38),
        "star5" => (5, 0.4),
        "star6" => (6, 0.55),
        "star7" => (7, 0.6),
        "star8" => (8, 0.65),
        "star10" => (10, 0.7),
        "star12" => (12, 0.75),
        "star16" => (16, 0.8),
        "star24" => (24, 0.84),
        "star32" => (32, 0.86),
        _ => return None,
    })
}

/// A band along an ellipse arc (outer radii `rx`×`ry`, `t` thick) from `a0` to `a1`, with an arrowhead at
/// the `a1` end.
fn curved_band(cx: f32, cy: f32, rx: f32, ry: f32, t: f32, a0: f32, a1: f32) -> Vec<P> {
    let outer = arc(cx, cy, rx, ry, a0, a1);
    let inner = arc(cx, cy, (rx - t).max(1.0), (ry - t).max(1.0), a0, a1);
    let mut out = outer.clone();
    // Arrowhead at the end: wider than the band, pointing along the arc.
    let end = a1.to_radians();
    let dir = if a1 > a0 { 1.0 } else { -1.0 };
    let (mx, my) = (cx + (rx - t / 2.0) * end.cos(), cy + (ry - t / 2.0) * end.sin());
    let (nx, ny) = (end.cos(), end.sin());
    let (tx, ty) = (-end.sin() * dir, end.cos() * dir);
    let head = t * 1.6;
    out.push((mx + nx * (head / 2.0 + t / 2.0), my + ny * (head / 2.0 + t / 2.0)));
    out.push((mx + tx * head, my + ty * head));
    out.push((mx - nx * (head / 2.0 + t / 2.0), my - ny * (head / 2.0 + t / 2.0)));
    out.extend(inner.into_iter().rev());
    out
}

/// Geometry of shape `id` in a `w`×`h` box; `points` are the freeform / line points (fractions of the
/// box) when the shape was drawn by hand. `None` for an unknown id.
pub fn geometry(id: &str, w: f32, h: f32, points: &[P]) -> Option<Geometry> {
    let w = if w.is_finite() { w.max(1.0) } else { 1.0 };
    let h = if h.is_finite() { h.max(1.0) } else { 1.0 };
    let m = w.min(h);
    let u = |pts: &[P]| map(pts, w, h);
    let mut g = Geometry::default();
    let fill = |pts: Vec<P>| Geometry { fill: vec![pts], ..Geometry::default() };
    let sq = Corner::Square;
    let rd = Corner::Round;
    let sn = Corner::Snip;
    let r = m * 0.16;
    let arrow_line = |g: &mut Geometry, pts: Vec<P>, start: bool, end: bool| {
        if end && let (Some(tip), Some(from)) = (pts.last(), pts.iter().rev().nth(1)) {
            g.arrows.push((*tip, *from));
        }
        if start && let (Some(tip), Some(from)) = (pts.first(), pts.get(1)) {
            g.arrows.push((*tip, *from));
        }
        g.lines.push(pts);
    };
    Some(match id {
        // Lines.
        "line" | "lineArrow" | "doubleArrow" => {
            let (a, b) = ends(points, w, h, [(0.0, 1.0), (1.0, 0.0)]);
            arrow_line(&mut g, vec![a, b], id == "doubleArrow", id != "line");
            g
        }
        "elbowConnector" | "elbowArrow" | "elbowDoubleArrow" => {
            let (a, b) = ends(points, w, h, [(0.0, 0.0), (1.0, 1.0)]);
            let mx = (a.0 + b.0) / 2.0;
            arrow_line(
                &mut g,
                vec![a, (mx, a.1), (mx, b.1), b],
                id == "elbowDoubleArrow",
                id != "elbowConnector",
            );
            g
        }
        "curvedConnector" | "curvedArrow" | "curvedDoubleArrow" => {
            let (a, b) = ends(points, w, h, [(0.0, 0.0), (1.0, 1.0)]);
            let mx = (a.0 + b.0) / 2.0;
            let mut pts = vec![a];
            bezier(&mut pts, a, (mx, a.1), (mx, b.1), b);
            arrow_line(&mut g, pts, id == "curvedDoubleArrow", id != "curvedConnector");
            g
        }
        "curve" => {
            let mut pts = vec![(0.0, h)];
            if points.len() >= 3 {
                pts = u(points);
            } else {
                bezier(&mut pts, (0.0, h), (w * 0.3, -h * 0.6), (w * 0.7, h * 1.6), (w, 0.0));
            }
            g.lines.push(pts);
            g
        }
        "scribble" => {
            let pts = if points.len() >= 2 {
                u(points)
            } else {
                (0..=40)
                    .map(|i| {
                        let t = i as f32 / 40.0;
                        (t * w, h / 2.0 + h * 0.4 * (t * TAU * 1.5).sin())
                    })
                    .collect()
            };
            g.lines.push(pts);
            g
        }
        "freeform" => {
            if points.len() >= 3 {
                fill(u(points))
            } else {
                fill(u(&[(0.1, 0.2), (0.55, 0.0), (1.0, 0.35), (0.8, 1.0), (0.3, 0.85), (0.0, 0.6)]))
            }
        }
        "arc" => {
            g.lines.push(arc(0.0, h, w, h, 270.0, 360.0));
            g
        }
        // Rectangles.
        "rectangle" | "textBox" | "flowProcess" | "canvas" => fill(rect(0.0, 0.0, w, h)),
        "roundedRectangle" | "flowAlternateProcess" => fill(corners(0.0, 0.0, w, h, [rd; 4], r)),
        "snip1Rectangle" => fill(corners(0.0, 0.0, w, h, [sq, sn, sq, sq], r)),
        "snip2SameRectangle" => fill(corners(0.0, 0.0, w, h, [sn, sn, sq, sq], r)),
        "snip2DiagRectangle" => fill(corners(0.0, 0.0, w, h, [sn, sq, sn, sq], r)),
        "snipRoundRectangle" => fill(corners(0.0, 0.0, w, h, [rd, sn, sq, sq], r)),
        "round1Rectangle" => fill(corners(0.0, 0.0, w, h, [sq, rd, sq, sq], r)),
        "round2SameRectangle" => fill(corners(0.0, 0.0, w, h, [rd, rd, sq, sq], r)),
        "round2DiagRectangle" => fill(corners(0.0, 0.0, w, h, [rd, sq, rd, sq], r)),
        // Basic shapes.
        "oval" | "flowConnector" => fill(ellipse(w / 2.0, h / 2.0, w / 2.0, h / 2.0)),
        "circle" => fill(ellipse(w / 2.0, h / 2.0, m / 2.0, m / 2.0)),
        "triangle" | "flowExtract" => fill(u(&[(0.5, 0.0), (1.0, 1.0), (0.0, 1.0)])),
        "flowMerge" => fill(u(&[(0.0, 0.0), (1.0, 0.0), (0.5, 1.0)])),
        "rightTriangle" => fill(u(&[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)])),
        "parallelogram" | "flowData" => fill(u(&[(0.25, 0.0), (1.0, 0.0), (0.75, 1.0), (0.0, 1.0)])),
        "trapezoid" => fill(u(&[(0.25, 0.0), (0.75, 0.0), (1.0, 1.0), (0.0, 1.0)])),
        "diamond" | "flowDecision" => fill(u(&[(0.5, 0.0), (1.0, 0.5), (0.5, 1.0), (0.0, 0.5)])),
        "pentagon" => fill(regular(w, h, 5, None)),
        "hexagon" => fill(u(&[(0.25, 0.0), (0.75, 0.0), (1.0, 0.5), (0.75, 1.0), (0.25, 1.0), (0.0, 0.5)])),
        "flowPreparation" => {
            fill(u(&[(0.2, 0.0), (0.8, 0.0), (1.0, 0.5), (0.8, 1.0), (0.2, 1.0), (0.0, 0.5)]))
        }
        "heptagon" => fill(regular(w, h, 7, None)),
        "octagon" => fill(corners(0.0, 0.0, w, h, [sn; 4], m * 0.29)),
        "decagon" => fill(regular(w, h, 10, None)),
        "dodecagon" => fill(regular(w, h, 12, None)),
        "pie" => {
            let mut pts = vec![(w / 2.0, h / 2.0)];
            pts.extend(arc(w / 2.0, h / 2.0, w / 2.0, h / 2.0, 0.0, 270.0));
            fill(pts)
        }
        "chord" => fill(arc(w / 2.0, h / 2.0, w / 2.0, h / 2.0, 45.0, 270.0)),
        "teardrop" => {
            let mut pts = arc(w / 2.0, h / 2.0, w / 2.0, h / 2.0, 0.0, 270.0);
            pts.push((w, 0.0));
            fill(pts)
        }
        "frame" => {
            let t = m * 0.12;
            Geometry { fill: vec![rect(0.0, 0.0, w, h), rect(t, t, w - 2.0 * t, h - 2.0 * t)], ..g }
        }
        "halfFrame" => {
            let t = m * 0.33;
            fill(vec![
                (0.0, 0.0),
                (w, 0.0),
                (w - t * w / h.max(1.0) * 0.5, t),
                (t, t),
                (t, h - t * 0.5),
                (0.0, h),
            ])
        }
        "lShape" => {
            let t = m * 0.5;
            fill(vec![(0.0, 0.0), (t, 0.0), (t, h - t), (w, h - t), (w, h), (0.0, h)])
        }
        "diagonalStripe" => fill(u(&[(0.0, 0.5), (0.5, 0.0), (1.0, 0.0), (0.0, 1.0)])),
        "cross" => {
            let (a, b) = (0.25, 0.75);
            fill(u(&[
                (a, 0.0),
                (b, 0.0),
                (b, a),
                (1.0, a),
                (1.0, b),
                (b, b),
                (b, 1.0),
                (a, 1.0),
                (a, b),
                (0.0, b),
                (0.0, a),
                (a, a),
            ]))
        }
        "plaque" => fill(corners(0.0, 0.0, w, h, [Corner::Concave; 4], r)),
        "can" | "flowMagneticDisk" => {
            let ry = (h * 0.12).min(w * 0.3);
            let mut body = arc(w / 2.0, ry, w / 2.0, ry, 180.0, 360.0);
            body.extend(arc(w / 2.0, h - ry, w / 2.0, ry, 0.0, 180.0));
            g.fill.push(body);
            g.faces.push((ellipse(w / 2.0, ry, w / 2.0, ry), 0.25));
            g
        }
        "flowDirectAccess" => {
            let rx = (w * 0.12).min(h * 0.3);
            let mut body = arc(rx, h / 2.0, rx, h / 2.0, 90.0, 270.0);
            body.extend(arc(w - rx, h / 2.0, rx, h / 2.0, 270.0, 450.0));
            g.fill.push(body);
            g.faces.push((ellipse(w - rx, h / 2.0, rx, h / 2.0), 0.25));
            g
        }
        "cube" => {
            let d = m * 0.25;
            g.fill.push(vec![(0.0, d), (d, 0.0), (w, 0.0), (w, h - d), (w - d, h), (0.0, h)]);
            g.faces.push((vec![(0.0, d), (d, 0.0), (w, 0.0), (w - d, d)], 0.25));
            g.faces.push((vec![(w - d, d), (w, 0.0), (w, h - d), (w - d, h)], -0.2));
            g.lines.push(vec![(0.0, d), (w - d, d), (w - d, h)]);
            g.lines.push(vec![(w - d, d), (w, 0.0)]);
            g
        }
        "bevel" => {
            let d = m * 0.125;
            g.fill.push(rect(0.0, 0.0, w, h));
            g.faces.push((vec![(0.0, 0.0), (w, 0.0), (w - d, d), (d, d)], 0.3));
            g.faces.push((vec![(0.0, 0.0), (d, d), (d, h - d), (0.0, h)], 0.15));
            g.faces.push((vec![(w, 0.0), (w, h), (w - d, h - d), (w - d, d)], -0.15));
            g.faces.push((vec![(0.0, h), (d, h - d), (w - d, h - d), (w, h)], -0.3));
            g
        }
        "donut" => {
            let t = m * 0.25;
            Geometry {
                fill: vec![
                    ellipse(w / 2.0, h / 2.0, w / 2.0, h / 2.0),
                    ellipse(w / 2.0, h / 2.0, (w / 2.0 - t).max(1.0), (h / 2.0 - t).max(1.0)),
                ],
                ..g
            }
        }
        "blockArc" => {
            let t = m * 0.25;
            let mut pts = arc(w / 2.0, h / 2.0, w / 2.0, h / 2.0, 180.0, 360.0);
            pts.extend(arc(w / 2.0, h / 2.0, (w / 2.0 - t).max(1.0), (h / 2.0 - t).max(1.0), 360.0, 180.0));
            fill(pts)
        }
        "foldedCorner" => {
            let f = m * 0.17;
            g.fill.push(vec![(0.0, 0.0), (w, 0.0), (w, h - f), (w - f, h), (0.0, h)]);
            g.faces.push((vec![(w, h - f), (w - f, h), (w - f * 0.8, h - f * 0.8)], -0.25));
            g
        }
        "smiley" => {
            g.fill.push(ellipse(w / 2.0, h / 2.0, w / 2.0, h / 2.0));
            g.faces.push((ellipse(w * 0.33, h * 0.37, w * 0.06, h * 0.06), -0.4));
            g.faces.push((ellipse(w * 0.67, h * 0.37, w * 0.06, h * 0.06), -0.4));
            g.lines.push(arc(w / 2.0, h * 0.45, w * 0.28, h * 0.3, 25.0, 155.0));
            g
        }
        "heart" => {
            let s = |x: f32, y: f32| (x / 160.0 * w, y / 100.0 * h);
            let mut v = vec![s(80.0, 96.0)];
            bezier(&mut v, s(80.0, 96.0), s(20.0, 62.0), s(0.0, 36.0), s(20.0, 14.0));
            bezier(&mut v, s(20.0, 14.0), s(38.0, -2.0), s(66.0, 2.0), s(80.0, 24.0));
            bezier(&mut v, s(80.0, 24.0), s(94.0, 2.0), s(122.0, -2.0), s(140.0, 14.0));
            bezier(&mut v, s(140.0, 14.0), s(160.0, 36.0), s(140.0, 62.0), s(80.0, 96.0));
            fill(v)
        }
        "lightning" => fill(u(&[
            (0.40, 0.0),
            (0.72, 0.0),
            (0.53, 0.37),
            (0.82, 0.37),
            (0.26, 1.0),
            (0.42, 0.55),
            (0.16, 0.55),
        ])),
        "sun" => {
            g.fill.push(ellipse(w / 2.0, h / 2.0, w * 0.25, h * 0.25));
            for i in 0..8 {
                let a = TAU * i as f32 / 8.0;
                let side = PI / 16.0;
                let p = |r: f32, da: f32| {
                    (w / 2.0 + w / 2.0 * r * (a + da).cos(), h / 2.0 + h / 2.0 * r * (a + da).sin())
                };
                g.fill.push(vec![p(0.62, -side), p(1.0, 0.0), p(0.62, side)]);
            }
            g
        }
        "moon" => {
            // The outer half circle (bottom → left → top), then back down along a narrower one.
            let mut pts = arc(w, h / 2.0, w, h / 2.0, 90.0, 270.0);
            pts.extend(arc(w, h / 2.0, w * 0.55, h / 2.0, 270.0, 90.0));
            fill(pts)
        }
        "cloud" | "cloudCallout" => {
            let bumps = 9.0;
            let pts: Vec<P> = (0..180)
                .map(|i| {
                    let a = TAU * i as f32 / 180.0;
                    let r = 0.86 + 0.14 * (a * bumps / 2.0).sin().abs();
                    (w / 2.0 + w / 2.0 * 0.92 * r * a.cos(), h * 0.42 + h * 0.42 * 0.92 * r * a.sin())
                })
                .collect();
            g.fill.push(pts);
            if id == "cloudCallout" {
                g.fill.push(ellipse(w * 0.2, h * 0.86, w * 0.05, h * 0.05));
                g.fill.push(ellipse(w * 0.1, h * 0.96, w * 0.03, h * 0.03));
            } else {
                g.fill.push(ellipse(w * 0.5, h * 0.84, w * 0.3, h * 0.15));
            }
            g
        }
        "doubleBracket" | "leftBracket" | "rightBracket" => {
            let single = id != "doubleBracket";
            let r2 = if single { (w * 0.5).min(h * 0.1) } else { (w * 0.1).min(h * 0.1) };
            let arm = if single { w } else { r2 * 1.5 };
            // "[" along the left edge with arms reaching `arm` to the right; "]" mirrored at the right edge.
            let left = || {
                let mut v = vec![(arm, 0.0)];
                v.extend(arc(r2, r2, r2, r2, 270.0, 180.0));
                v.extend(arc(r2, h - r2, r2, r2, 180.0, 90.0));
                v.push((arm, h));
                v
            };
            let right = || {
                let mut v = vec![(w - arm, 0.0)];
                v.extend(arc(w - r2, r2, r2, r2, 270.0, 360.0));
                v.extend(arc(w - r2, h - r2, r2, r2, 0.0, 90.0));
                v.push((w - arm, h));
                v
            };
            match id {
                "leftBracket" => g.lines.push(left()),
                "rightBracket" => g.lines.push(right()),
                _ => {
                    g.lines.push(left());
                    g.lines.push(right());
                }
            }
            g
        }
        "doubleBrace" | "leftBrace" | "rightBrace" => {
            let bw = if id == "doubleBrace" { (w * 0.15).max(4.0) } else { w };
            let brace = |x0: f32, flip: bool| {
                // A "{" whose tip points left (or "}" pointing right) spanning `bw`.
                let (a, b) = if flip { (x0 + bw, x0) } else { (x0, x0 + bw) };
                let mid = (a + b) / 2.0;
                let mut v = vec![(b, 0.0)];
                bezier(&mut v, (b, 0.0), (mid, 0.0), (mid, h * 0.05), (mid, h * 0.15));
                v.push((mid, h * 0.4));
                bezier(&mut v, (mid, h * 0.4), (mid, h * 0.48), (a, h * 0.5), (a, h * 0.5));
                bezier(&mut v, (a, h * 0.5), (mid, h * 0.52), (mid, h * 0.6), (mid, h * 0.6));
                v.push((mid, h * 0.85));
                bezier(&mut v, (mid, h * 0.85), (mid, h * 0.95), (mid, h), (b, h));
                v
            };
            match id {
                "leftBrace" => g.lines.push(brace(0.0, false)),
                "rightBrace" => g.lines.push(brace(0.0, true)),
                _ => {
                    g.lines.push(brace(0.0, false));
                    g.lines.push(brace(w - bw, true));
                }
            }
            g
        }
        // Block arrows.
        "rightArrow" => {
            fill(u(&[(0.0, 0.25), (0.6, 0.25), (0.6, 0.0), (1.0, 0.5), (0.6, 1.0), (0.6, 0.75), (0.0, 0.75)]))
        }
        "leftArrow" => {
            fill(u(&[(1.0, 0.25), (0.4, 0.25), (0.4, 0.0), (0.0, 0.5), (0.4, 1.0), (0.4, 0.75), (1.0, 0.75)]))
        }
        "upArrow" => {
            fill(u(&[(0.25, 1.0), (0.25, 0.4), (0.0, 0.4), (0.5, 0.0), (1.0, 0.4), (0.75, 0.4), (0.75, 1.0)]))
        }
        "downArrow" => {
            fill(u(&[(0.25, 0.0), (0.25, 0.6), (0.0, 0.6), (0.5, 1.0), (1.0, 0.6), (0.75, 0.6), (0.75, 0.0)]))
        }
        "leftRightArrow" => fill(u(&[
            (0.0, 0.5),
            (0.25, 0.0),
            (0.25, 0.25),
            (0.75, 0.25),
            (0.75, 0.0),
            (1.0, 0.5),
            (0.75, 1.0),
            (0.75, 0.75),
            (0.25, 0.75),
            (0.25, 1.0),
        ])),
        "upDownArrow" => fill(u(&[
            (0.5, 0.0),
            (1.0, 0.25),
            (0.75, 0.25),
            (0.75, 0.75),
            (1.0, 0.75),
            (0.5, 1.0),
            (0.0, 0.75),
            (0.25, 0.75),
            (0.25, 0.25),
            (0.0, 0.25),
        ])),
        "quadArrow" | "quadArrowCallout" => {
            let callout = id == "quadArrowCallout";
            let (a, b) = if callout { (0.3, 0.7) } else { (0.42, 0.58) };
            let (c, d) = (0.36, 0.64);
            let mut pts = vec![(0.5, 0.0), (0.64, 0.14), (0.58, 0.14)];
            if callout {
                pts = vec![
                    (0.5, 0.0),
                    (0.64, 0.12),
                    (0.56, 0.12),
                    (0.56, a),
                    (b, a),
                    (b, 0.44),
                    (0.88, 0.44),
                    (0.88, 0.36),
                    (1.0, 0.5),
                ];
                pts.extend([
                    (0.88, 0.64),
                    (0.88, 0.56),
                    (b, 0.56),
                    (b, b),
                    (0.56, b),
                    (0.56, 0.88),
                    (0.64, 0.88),
                    (0.5, 1.0),
                ]);
                pts.extend([
                    (0.36, 0.88),
                    (0.44, 0.88),
                    (0.44, b),
                    (a, b),
                    (a, 0.56),
                    (0.12, 0.56),
                    (0.12, 0.64),
                    (0.0, 0.5),
                ]);
                pts.extend([
                    (0.12, 0.36),
                    (0.12, 0.44),
                    (a, 0.44),
                    (a, a),
                    (0.44, a),
                    (0.44, 0.12),
                    (0.36, 0.12),
                ]);
                fill(u(&pts))
            } else {
                pts.extend([(0.58, a), (0.86, a), (0.86, c), (1.0, 0.5), (0.86, d), (0.86, b), (0.58, b)]);
                pts.extend([(0.58, 0.86), (0.64, 0.86), (0.5, 1.0), (0.36, 0.86), (0.42, 0.86), (0.42, b)]);
                pts.extend([
                    (0.14, b),
                    (0.14, d),
                    (0.0, 0.5),
                    (0.14, c),
                    (0.14, a),
                    (0.42, a),
                    (0.42, 0.14),
                    (0.36, 0.14),
                ]);
                fill(u(&pts))
            }
        }
        "leftRightUpArrow" => fill(u(&[
            (0.5, 0.0),
            (0.66, 0.2),
            (0.57, 0.2),
            (0.57, 0.62),
            (0.8, 0.62),
            (0.8, 0.5),
            (1.0, 0.75),
            (0.8, 1.0),
            (0.8, 0.88),
            (0.2, 0.88),
            (0.2, 1.0),
            (0.0, 0.75),
            (0.2, 0.5),
            (0.2, 0.62),
            (0.43, 0.62),
            (0.43, 0.2),
            (0.34, 0.2),
        ])),
        "bentArrow" => fill(u(&[
            (0.0, 1.0),
            (0.0, 0.45),
            (0.15, 0.25),
            (0.7, 0.25),
            (0.7, 0.0),
            (1.0, 0.37),
            (0.7, 0.75),
            (0.7, 0.5),
            (0.28, 0.5),
            (0.25, 0.55),
            (0.25, 1.0),
        ])),
        "uturnArrow" => fill(u(&[
            (0.0, 1.0),
            (0.0, 0.35),
            (0.3, 0.0),
            (0.6, 0.0),
            (0.82, 0.3),
            (0.82, 0.55),
            (1.0, 0.55),
            (0.7, 0.85),
            (0.4, 0.55),
            (0.58, 0.55),
            (0.58, 0.32),
            (0.52, 0.24),
            (0.36, 0.24),
            (0.25, 0.38),
            (0.25, 1.0),
        ])),
        "leftUpArrow" => fill(u(&[
            (0.0, 0.8),
            (0.2, 0.6),
            (0.2, 0.72),
            (0.72, 0.72),
            (0.72, 0.2),
            (0.6, 0.2),
            (0.8, 0.0),
            (1.0, 0.2),
            (0.88, 0.2),
            (0.88, 0.88),
            (0.2, 0.88),
            (0.2, 1.0),
        ])),
        "bentUpArrow" => fill(u(&[
            (0.0, 0.72),
            (0.66, 0.72),
            (0.66, 0.25),
            (0.5, 0.25),
            (0.78, 0.0),
            (1.0, 0.25),
            (0.9, 0.25),
            (0.9, 1.0),
            (0.0, 1.0),
        ])),
        "curvedRightArrow" => fill(curved_band(w, h / 2.0, w, h / 2.0, m * 0.25, 210.0, 140.0)),
        "curvedLeftArrow" => fill(curved_band(0.0, h / 2.0, w, h / 2.0, m * 0.25, 330.0, 400.0)),
        "curvedUpArrow" => fill(curved_band(w / 2.0, 0.0, w / 2.0, h, m * 0.25, 160.0, 50.0)),
        "curvedDownArrow" => fill(curved_band(w / 2.0, h, w / 2.0, h, m * 0.25, 200.0, 310.0)),
        "circularArrow" => {
            fill(curved_band(w / 2.0, h / 2.0, w / 2.0 * 0.85, h / 2.0 * 0.85, m * 0.18, 150.0, 400.0))
        }
        "stripedRightArrow" => {
            g.fill.push(u(&[
                (0.2, 0.25),
                (0.6, 0.25),
                (0.6, 0.0),
                (1.0, 0.5),
                (0.6, 1.0),
                (0.6, 0.75),
                (0.2, 0.75),
            ]));
            g.fill.push(rect(0.0, h * 0.25, w * 0.04, h * 0.5));
            g.fill.push(rect(w * 0.08, h * 0.25, w * 0.08, h * 0.5));
            g
        }
        "notchedRightArrow" => fill(u(&[
            (0.0, 0.25),
            (0.6, 0.25),
            (0.6, 0.0),
            (1.0, 0.5),
            (0.6, 1.0),
            (0.6, 0.75),
            (0.0, 0.75),
            (0.15, 0.5),
        ])),
        "homePlate" => fill(u(&[(0.0, 0.0), (0.75, 0.0), (1.0, 0.5), (0.75, 1.0), (0.0, 1.0)])),
        "chevron" => fill(u(&[(0.0, 0.0), (0.75, 0.0), (1.0, 0.5), (0.75, 1.0), (0.0, 1.0), (0.25, 0.5)])),
        "rightArrowCallout" => fill(u(&[
            (0.0, 0.0),
            (0.6, 0.0),
            (0.6, 0.35),
            (0.8, 0.35),
            (0.8, 0.2),
            (1.0, 0.5),
            (0.8, 0.8),
            (0.8, 0.65),
            (0.6, 0.65),
            (0.6, 1.0),
            (0.0, 1.0),
        ])),
        "leftArrowCallout" => fill(u(&[
            (1.0, 0.0),
            (0.4, 0.0),
            (0.4, 0.35),
            (0.2, 0.35),
            (0.2, 0.2),
            (0.0, 0.5),
            (0.2, 0.8),
            (0.2, 0.65),
            (0.4, 0.65),
            (0.4, 1.0),
            (1.0, 1.0),
        ])),
        "downArrowCallout" => fill(u(&[
            (0.0, 0.0),
            (1.0, 0.0),
            (1.0, 0.6),
            (0.65, 0.6),
            (0.65, 0.8),
            (0.8, 0.8),
            (0.5, 1.0),
            (0.2, 0.8),
            (0.35, 0.8),
            (0.35, 0.6),
            (0.0, 0.6),
        ])),
        "upArrowCallout" => fill(u(&[
            (0.0, 1.0),
            (1.0, 1.0),
            (1.0, 0.4),
            (0.65, 0.4),
            (0.65, 0.2),
            (0.8, 0.2),
            (0.5, 0.0),
            (0.2, 0.2),
            (0.35, 0.2),
            (0.35, 0.4),
            (0.0, 0.4),
        ])),
        "leftRightArrowCallout" => fill(u(&[
            (0.0, 0.5),
            (0.15, 0.2),
            (0.15, 0.35),
            (0.3, 0.35),
            (0.3, 0.0),
            (0.7, 0.0),
            (0.7, 0.35),
            (0.85, 0.35),
            (0.85, 0.2),
            (1.0, 0.5),
            (0.85, 0.8),
            (0.85, 0.65),
            (0.7, 0.65),
            (0.7, 1.0),
            (0.3, 1.0),
            (0.3, 0.65),
            (0.15, 0.65),
            (0.15, 0.8),
        ])),
        // Equation shapes.
        "mathPlus" => {
            let (a, b) = (0.38, 0.62);
            let (c, d) = (0.12, 0.88);
            fill(u(&[
                (a, c),
                (b, c),
                (b, a),
                (d, a),
                (d, b),
                (b, b),
                (b, d),
                (a, d),
                (a, b),
                (c, b),
                (c, a),
                (a, a),
            ]))
        }
        "mathMinus" => fill(u(&[(0.12, 0.38), (0.88, 0.38), (0.88, 0.62), (0.12, 0.62)])),
        "mathMultiply" => {
            let s = 0.1;
            fill(u(&[
                (0.15, 0.15 + s),
                (0.15 + s, 0.15),
                (0.5, 0.5 - s),
                (0.85 - s, 0.15),
                (0.85, 0.15 + s),
                (0.5 + s, 0.5),
                (0.85, 0.85 - s),
                (0.85 - s, 0.85),
                (0.5, 0.5 + s),
                (0.15 + s, 0.85),
                (0.15, 0.85 - s),
                (0.5 - s, 0.5),
            ]))
        }
        "mathDivide" => {
            g.fill.push(u(&[(0.12, 0.42), (0.88, 0.42), (0.88, 0.58), (0.12, 0.58)]));
            g.fill.push(ellipse(w / 2.0, h * 0.22, w * 0.09, h * 0.09));
            g.fill.push(ellipse(w / 2.0, h * 0.78, w * 0.09, h * 0.09));
            g
        }
        "mathEqual" | "mathNotEqual" => {
            g.fill.push(u(&[(0.12, 0.24), (0.88, 0.24), (0.88, 0.42), (0.12, 0.42)]));
            g.fill.push(u(&[(0.12, 0.58), (0.88, 0.58), (0.88, 0.76), (0.12, 0.76)]));
            if id == "mathNotEqual" {
                g.faces.push((u(&[(0.6, 0.05), (0.7, 0.1), (0.4, 0.95), (0.3, 0.9)]), 0.0));
            }
            g
        }
        // Flowchart.
        "flowPredefinedProcess" => {
            g.fill.push(rect(0.0, 0.0, w, h));
            g.lines.push(vec![(w * 0.12, 0.0), (w * 0.12, h)]);
            g.lines.push(vec![(w * 0.88, 0.0), (w * 0.88, h)]);
            g
        }
        "flowInternalStorage" => {
            g.fill.push(rect(0.0, 0.0, w, h));
            g.lines.push(vec![(w * 0.2, 0.0), (w * 0.2, h)]);
            g.lines.push(vec![(0.0, h * 0.2), (w, h * 0.2)]);
            g
        }
        "flowDocument" | "flowMultidocument" => {
            let doc = |x0: f32, y0: f32, dw: f32, dh: f32| {
                let mut v = vec![(x0, y0), (x0 + dw, y0), (x0 + dw, y0 + dh * 0.85)];
                bezier(
                    &mut v,
                    (x0 + dw, y0 + dh * 0.85),
                    (x0 + dw * 0.75, y0 + dh * 0.7),
                    (x0 + dw * 0.4, y0 + dh * 1.12),
                    (x0, y0 + dh * 0.9),
                );
                v
            };
            if id == "flowMultidocument" {
                let (dw, dh) = (w * 0.84, h * 0.84);
                g.lines.push(vec![(w * 0.16, h * 0.08), (w * 0.16, 0.0), (w, 0.0), (w, dh * 0.85)]);
                g.lines.push(vec![
                    (w * 0.08, h * 0.16),
                    (w * 0.08, h * 0.08),
                    (w * 0.92, h * 0.08),
                    (w * 0.92, h * 0.08 + dh * 0.85),
                ]);
                g.fill.push(doc(0.0, h * 0.16, dw, dh));
            } else {
                g.fill.push(doc(0.0, 0.0, w, h));
            }
            g
        }
        "flowTerminator" => fill(corners(0.0, 0.0, w, h, [rd; 4], h / 2.0)),
        "flowManualInput" => fill(u(&[(0.0, 0.25), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)])),
        "flowManualOperation" => fill(u(&[(0.0, 0.0), (1.0, 0.0), (0.8, 1.0), (0.2, 1.0)])),
        "flowOffpageConnector" => fill(u(&[(0.0, 0.0), (1.0, 0.0), (1.0, 0.8), (0.5, 1.0), (0.0, 0.8)])),
        "flowCard" => fill(corners(0.0, 0.0, w, h, [sn, sq, sq, sq], m * 0.25)),
        "flowPunchedTape" => {
            let mut v = vec![(0.0, h * 0.1)];
            bezier(&mut v, (0.0, h * 0.1), (w * 0.25, h * 0.3), (w * 0.25, -h * 0.1), (w * 0.5, h * 0.1));
            bezier(&mut v, (w * 0.5, h * 0.1), (w * 0.75, h * 0.3), (w * 0.75, -h * 0.1), (w, h * 0.1));
            v.push((w, h * 0.9));
            bezier(&mut v, (w, h * 0.9), (w * 0.75, h * 0.7), (w * 0.75, h * 1.1), (w * 0.5, h * 0.9));
            bezier(&mut v, (w * 0.5, h * 0.9), (w * 0.25, h * 0.7), (w * 0.25, h * 1.1), (0.0, h * 0.9));
            fill(v)
        }
        "flowSummingJunction" | "flowOr" => {
            g.fill.push(ellipse(w / 2.0, h / 2.0, w / 2.0, h / 2.0));
            if id == "flowOr" {
                g.lines.push(vec![(w / 2.0, 0.0), (w / 2.0, h)]);
                g.lines.push(vec![(0.0, h / 2.0), (w, h / 2.0)]);
            } else {
                let k = std::f32::consts::FRAC_1_SQRT_2 / 2.0;
                g.lines.push(vec![(w * (0.5 - k), h * (0.5 - k)), (w * (0.5 + k), h * (0.5 + k))]);
                g.lines.push(vec![(w * (0.5 + k), h * (0.5 - k)), (w * (0.5 - k), h * (0.5 + k))]);
            }
            g
        }
        "flowCollate" => {
            g.fill.push(u(&[(0.0, 0.0), (1.0, 0.0), (0.5, 0.5)]));
            g.fill.push(u(&[(0.5, 0.5), (1.0, 1.0), (0.0, 1.0)]));
            g
        }
        "flowSort" => {
            g.fill.push(u(&[(0.5, 0.0), (1.0, 0.5), (0.5, 1.0), (0.0, 0.5)]));
            g.lines.push(vec![(0.0, h / 2.0), (w, h / 2.0)]);
            g
        }
        "flowStoredData" => {
            // Rounded on the left, hollowed on the right.
            let rx = w * 0.15;
            let mut pts = arc(rx, h / 2.0, rx, h / 2.0, 270.0, 90.0);
            pts.extend(arc(w, h / 2.0, rx, h / 2.0, 90.0, 270.0));
            fill(pts)
        }
        "flowDelay" => {
            let mut pts = vec![(0.0, 0.0)];
            pts.extend(arc(w / 2.0, h / 2.0, w / 2.0, h / 2.0, 270.0, 450.0));
            pts.push((0.0, h));
            fill(pts)
        }
        "flowSequentialAccess" => {
            g.fill.push(ellipse(w / 2.0, h / 2.0, m / 2.0, h / 2.0));
            g.faces.push((rect(w / 2.0, h - h * 0.08, w / 2.0, h * 0.08), 0.0));
            g
        }
        "flowDisplay" => {
            let mut pts = vec![(0.0, h / 2.0), (w * 0.15, 0.0), (w * 0.85, 0.0)];
            pts.extend(arc(w * 0.85, h / 2.0, w * 0.15, h / 2.0, 270.0, 450.0));
            pts.push((w * 0.15, h));
            fill(pts)
        }
        // Stars and banners.
        "explosion1" => fill(u(&[
            (0.5, 0.0),
            (0.6, 0.28),
            (0.9, 0.1),
            (0.76, 0.38),
            (1.0, 0.5),
            (0.74, 0.6),
            (0.88, 0.92),
            (0.58, 0.72),
            (0.45, 1.0),
            (0.38, 0.72),
            (0.1, 0.86),
            (0.24, 0.58),
            (0.0, 0.42),
            (0.26, 0.34),
            (0.14, 0.06),
            (0.4, 0.24),
        ])),
        "explosion2" => fill(u(&[
            (0.42, 0.0),
            (0.52, 0.22),
            (0.72, 0.04),
            (0.7, 0.28),
            (0.98, 0.18),
            (0.8, 0.42),
            (1.0, 0.56),
            (0.76, 0.62),
            (0.84, 0.86),
            (0.6, 0.74),
            (0.54, 1.0),
            (0.42, 0.78),
            (0.22, 0.94),
            (0.24, 0.7),
            (0.0, 0.72),
            (0.16, 0.54),
            (0.02, 0.36),
            (0.24, 0.34),
            (0.18, 0.1),
            (0.36, 0.24),
        ])),
        id if star_points(id).is_some() => {
            let (n, inner) = star_points(id).unwrap_or((5, 0.4));
            fill(regular(w, h, n, Some(inner)))
        }
        "ribbonUp" | "ribbonDown" | "curvedRibbonUp" | "curvedRibbonDown" => {
            let up = id.ends_with("Up");
            let curved = id.starts_with("curved");
            let (top, bot) = if up { (0.0, 0.75) } else { (0.25, 1.0) };
            let band = |x0: f32, x1: f32| -> Vec<P> {
                let mut v = vec![(x0 * w, (top + 0.0) * h)];
                if curved {
                    bezier(
                        &mut v,
                        (x0 * w, top * h),
                        (x0 * w + (x1 - x0) * w * 0.3, (top + if up { 0.12 } else { -0.12 }) * h),
                        (x1 * w - (x1 - x0) * w * 0.3, (top + if up { 0.12 } else { -0.12 }) * h),
                        (x1 * w, top * h),
                    );
                } else {
                    v.push((x1 * w, top * h));
                }
                v.push((x1 * w, bot * h));
                if curved {
                    let b = bot * h;
                    let dy = if up { 0.12 } else { -0.12 } * h;
                    bezier(
                        &mut v,
                        (x1 * w, b),
                        (x1 * w - (x1 - x0) * w * 0.3, b + dy),
                        (x0 * w + (x1 - x0) * w * 0.3, b + dy),
                        (x0 * w, b),
                    );
                } else {
                    v.push((x0 * w, bot * h));
                }
                v
            };
            let (ey0, ey1) = if up { (0.25, 1.0) } else { (0.0, 0.75) };
            // The ends behind the band.
            g.faces.push((
                u(&[(0.0, ey0), (0.2, ey0), (0.2, ey1), (0.0, ey1), (0.08, (ey0 + ey1) / 2.0)]),
                -0.2,
            ));
            g.faces.push((
                u(&[(1.0, ey0), (0.8, ey0), (0.8, ey1), (1.0, ey1), (0.92, (ey0 + ey1) / 2.0)]),
                -0.2,
            ));
            g.fill.push(band(0.15, 0.85));
            g
        }
        "verticalScroll" => {
            let t = m * 0.12;
            g.fill.push(corners(t, t, w - 2.0 * t, h - 2.0 * t, [rd, sq, rd, sq], t));
            g.faces.push((ellipse(w / 2.0, t, w / 2.0 - t * 0.2, t), -0.15));
            g.faces.push((ellipse(w / 2.0, h - t, w / 2.0 - t * 0.2, t), -0.15));
            g
        }
        "horizontalScroll" => {
            let t = m * 0.12;
            g.fill.push(corners(t, t, w - 2.0 * t, h - 2.0 * t, [sq, rd, sq, rd], t));
            g.faces.push((ellipse(t, h / 2.0, t, h / 2.0 - t * 0.2), -0.15));
            g.faces.push((ellipse(w - t, h / 2.0, t, h / 2.0 - t * 0.2), -0.15));
            g
        }
        "wave" | "doubleWave" => {
            let waves = if id == "wave" { 1.0 } else { 2.0 };
            let amp = h * 0.1;
            let n = 48;
            let mut v: Vec<P> = (0..=n)
                .map(|i| {
                    let t = i as f32 / n as f32;
                    (t * w, amp + amp * (t * TAU * waves).sin())
                })
                .collect();
            v.extend((0..=n).rev().map(|i| {
                let t = i as f32 / n as f32;
                (t * w, h - amp + amp * (t * TAU * waves).sin())
            }));
            fill(v)
        }
        // Callouts.
        "wedgeRectCallout" | "wedgeRoundRectCallout" => {
            let bh = h * 0.75;
            let mut v = if id == "wedgeRoundRectCallout" {
                corners(0.0, 0.0, w, bh, [rd, rd, sq, sq], m * 0.12)
            } else {
                vec![(0.0, 0.0), (w, 0.0)]
            };
            let r2 = m * 0.12;
            if id == "wedgeRoundRectCallout" {
                v.truncate(v.len().saturating_sub(2));
                v.extend(arc(w - r2, bh - r2, r2, r2, 0.0, 90.0));
            } else {
                v.push((w, bh));
            }
            v.extend([(w * 0.42, bh), (w * 0.2, h), (w * 0.25, bh)]);
            if id == "wedgeRoundRectCallout" {
                v.extend(arc(r2, bh - r2, r2, r2, 90.0, 180.0));
            } else {
                v.push((0.0, bh));
            }
            fill(v)
        }
        "wedgeEllipseCallout" => {
            let (cx, cy, rx, ry) = (w / 2.0, h * 0.42, w / 2.0, h * 0.42);
            let mut v = arc(cx, cy, rx, ry, 120.0, 460.0);
            v.push((w * 0.15, h));
            fill(v)
        }
        id if id.contains("Callout") && id.ends_with(['1', '2', '3']) => {
            let bent = id.ends_with('2') || id.ends_with('3');
            let double = id.ends_with('3');
            let border = !id.starts_with("noBorder") && !id.starts_with("accentCallout");
            let accent = id.starts_with("accent");
            let (bx, bw, bh) = (w * 0.3, w * 0.7, h * 0.7);
            let body = rect(bx, 0.0, bw, bh);
            if border {
                g.fill.push(body);
            } else {
                g.faces.push((body, 0.0));
            }
            let start = if accent { (bx - w * 0.05, bh * 0.3) } else { (bx, bh * 0.5) };
            if accent {
                g.lines.push(vec![(bx - w * 0.05, 0.0), (bx - w * 0.05, bh)]);
            }
            let mut line = vec![start];
            if bent {
                line.push((w * 0.15, start.1));
            }
            if double {
                line.push((w * 0.15, h * 0.8));
            }
            line.push((0.0, h));
            g.lines.push(line);
            g
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gallery_shape_has_geometry_inside_its_box() {
        for (id, label, cat) in SHAPES {
            assert!(*cat < CATEGORIES.len(), "{id}");
            assert!(!label.is_empty());
            let g = geometry(id, 160.0, 100.0, &[]).unwrap_or_else(|| panic!("{id} has no geometry"));
            assert!(!g.fill.is_empty() || !g.lines.is_empty() || !g.faces.is_empty(), "{id} draws nothing");
            for p in g.fill.iter().chain(g.lines.iter()).chain(g.faces.iter().map(|f| &f.0)).flatten() {
                assert!(p.0.is_finite() && p.1.is_finite(), "{id}");
                assert!(
                    p.0 >= -1.0 && p.0 <= 161.0 && p.1 >= -20.0 && p.1 <= 121.0,
                    "{id} point {p:?} outside"
                );
            }
            assert_eq!(canonical(id), Some(*id));
            assert_eq!(canonical(label), Some(*id), "{label}");
            assert_eq!(is_line_shape(id), g.is_line(), "{id}");
        }
        assert_eq!(canonical("arrow"), Some("rightArrow"));
        assert_eq!(canonical("star"), Some("star5"));
        assert_eq!(canonical("nope"), None);
        let ids: std::collections::HashSet<_> = SHAPES.iter().map(|s| s.0).collect();
        assert_eq!(ids.len(), SHAPES.len(), "ids are unique");
        assert!(SHAPES.len() >= 150, "{}", SHAPES.len());
    }

    #[test]
    fn hand_drawn_points_and_odd_sizes() {
        let g = geometry("scribble", 100.0, 50.0, &[(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]).unwrap();
        assert_eq!(g.lines[0], vec![(0.0, 0.0), (50.0, 50.0), (100.0, 0.0)]);
        let g = geometry("freeform", 10.0, 10.0, &[(0.0, 0.0), (1.0, 0.0), (0.5, 1.0)]).unwrap();
        assert_eq!(g.fill[0].len(), 3);
        // A line drawn right to left keeps its direction and gets the arrowhead at its end.
        let g = geometry("lineArrow", 100.0, 10.0, &[(1.0, 0.0), (0.0, 1.0)]).unwrap();
        assert_eq!(g.arrows, vec![((0.0, 10.0), (100.0, 0.0))]);
        for id in ["oval", "star32", "cloud", "line"] {
            for (w, h) in [(0.0, 0.0), (f32::NAN, 5.0), (1e9, 1e9), (-5.0, 3.0)] {
                let g = geometry(id, w, h, &[(f32::NAN, 0.0)]).unwrap();
                assert!(g.fill.iter().chain(g.lines.iter()).flatten().all(|p| !p.0.is_nan() || id == "line"));
            }
        }
    }
}
