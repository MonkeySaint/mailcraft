//! Insert › Equation: a small linear math notation laid out in two dimensions and drawn as a picture.
//!
//! Notation (a subset of the usual "linear format" conventions, written for MailCraft):
//! - `a/b` fraction; `x^2` superscript; `x_i` subscript (both: `x_i^2`); `√x` or `√(…)` square root;
//! - `( … )` groups; a group used as a fraction part or script loses its parentheses (`(a+b)/2`);
//! - letters are italic variables; `sin cos tan log ln exp lim max min det` are upright function names;
//!   digits are upright numbers; `+ − - = ± × ÷ < > ≤ ≥ ≠ ≈ →` get operator spacing;
//! - `∑ ∏` take their `_` and `^` limits below and above, `∫` beside.
//!
//! Layout is in px on a baseline (y grows downwards, the baseline at 0); text is shaped with the bundled
//! serif face through `mailcraft-layout` and the picture is rasterised with the shape renderer.

use super::{Mask, dev, text_list, translate};
use mailcraft_geom::{Color, Point};
use mailcraft_layout::{DisplayItem, DisplayList};
use mailcraft_model::TextStyle;

/// Longest equation text accepted (characters).
pub const MAX_EQUATION: usize = 400;
/// Deepest nesting of groups, scripts and fractions.
const MAX_DEPTH: usize = 24;
/// Base size (pt) and the smallest script size.
const BASE_PT: f32 = 15.0;
const MIN_PT: f32 = 7.0;
/// Room around the drawing (px).
const PAD: f32 = 6.0;
/// The math face (bundled substitutes resolve it).
const MATH_FONT: &str = "Cambria Math, Liberation Serif, serif";

const FUNCTIONS: [&str; 10] = ["sin", "cos", "tan", "log", "ln", "exp", "lim", "max", "min", "det"];

#[derive(Clone, Debug, PartialEq)]
enum Node {
    /// Text and whether it is italic.
    Text(String, bool),
    /// A binary or relation operator (spaced).
    Op(String),
    /// ∑ ∏ ∫.
    Big(char),
    Row(Vec<Node>),
    Frac(Box<Node>, Box<Node>),
    Scripts {
        base: Box<Node>,
        sub: Option<Box<Node>>,
        sup: Option<Box<Node>>,
    },
    Sqrt(Box<Node>),
    Paren(Box<Node>),
}

fn is_op(c: char) -> bool {
    "+-−=±×÷<>≤≥≠≈→∓·∙∈∉⊂⊃∪∩∧∨⇒⇔".contains(c)
}

struct Parser {
    chars: Vec<char>,
    i: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn skip_spaces(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.i += 1;
        }
    }

    /// A row up to `close` (consumed) or the end.
    fn row(&mut self, close: Option<char>) -> Node {
        let mut items: Vec<Node> = Vec::new();
        // Where the current operand run starts (after the last operator or space): a fraction's numerator is
        // the run before the slash ("2x/3", "n!/k!").
        let mut run_start = 0usize;
        self.depth += 1;
        loop {
            let before = self.i;
            self.skip_spaces();
            if self.i != before {
                run_start = items.len();
            }
            let Some(c) = self.peek() else { break };
            if Some(c) == close {
                self.i += 1;
                break;
            }
            if c == ')' && close.is_none() {
                // A stray closing parenthesis is shown as it is.
                self.i += 1;
                items.push(Node::Text(")".into(), false));
                continue;
            }
            if c == '/' && items.len() > run_start {
                self.i += 1;
                let mut run: Vec<Node> = items.drain(run_start.min(items.len())..).collect();
                let num = if run.len() == 1 {
                    run.pop().map(unwrap_paren).unwrap_or(Node::Row(Vec::new()))
                } else {
                    Node::Row(run)
                };
                let den = self.operand();
                items.push(Node::Frac(Box::new(num), Box::new(den)));
                continue;
            }
            let n = self.scripted();
            let ends_run = matches!(n, Node::Op(_) | Node::Big(_));
            items.push(n);
            if ends_run {
                run_start = items.len();
            }
        }
        self.depth = self.depth.saturating_sub(1);
        if items.len() == 1 { items.pop().unwrap_or(Node::Row(Vec::new())) } else { Node::Row(items) }
    }

    /// A primary with its scripts.
    fn scripted(&mut self) -> Node {
        let base = self.primary();
        let (mut sub, mut sup) = (None, None);
        loop {
            match self.peek() {
                Some('^') if sup.is_none() => {
                    self.i += 1;
                    sup = Some(Box::new(unwrap_paren(self.primary())));
                }
                Some('_') if sub.is_none() => {
                    self.i += 1;
                    sub = Some(Box::new(unwrap_paren(self.primary())));
                }
                _ => break,
            }
        }
        if sub.is_none() && sup.is_none() { base } else { Node::Scripts { base: Box::new(base), sub, sup } }
    }

    /// A fraction part or script: one primary (with scripts), parentheses dropped.
    fn operand(&mut self) -> Node {
        self.skip_spaces();
        unwrap_paren(self.scripted())
    }

    fn primary(&mut self) -> Node {
        let Some(c) = self.peek() else { return Node::Row(Vec::new()) };
        self.i += 1;
        if self.depth >= MAX_DEPTH {
            return Node::Text(c.to_string(), false);
        }
        self.depth += 1;
        let n = self.primary_of(c);
        self.depth = self.depth.saturating_sub(1);
        n
    }

    fn primary_of(&mut self, c: char) -> Node {
        match c {
            '(' => Node::Paren(Box::new(self.row(Some(')')))),
            '{' => self.row(Some('}')),
            '√' => {
                self.skip_spaces();
                let inner = self.operand();
                Node::Sqrt(Box::new(inner))
            }
            '∑' | '∏' | '∫' => Node::Big(c),
            c if c.is_ascii_digit() || c == '.' => {
                let mut s = c.to_string();
                while let Some(d) = self.peek().filter(|d| d.is_ascii_digit() || *d == '.') {
                    s.push(d);
                    self.i += 1;
                }
                Node::Text(s, false)
            }
            c if c.is_ascii_alphabetic() => {
                let mut s = c.to_string();
                let mut j = self.i;
                while let Some(d) = self.chars.get(j).filter(|d| d.is_ascii_alphabetic()) {
                    s.push(*d);
                    j += 1;
                }
                // A function name, else just this one letter (a variable).
                match FUNCTIONS.iter().find(|f| s.starts_with(*f)) {
                    Some(f) => {
                        self.i += f.len() - 1;
                        Node::Text((*f).to_owned(), false)
                    }
                    None => Node::Text(c.to_string(), true),
                }
            }
            c if c.is_alphabetic() => Node::Text(c.to_string(), c.is_lowercase()),
            '-' => Node::Op("\u{2212}".into()),
            c if is_op(c) => Node::Op(c.to_string()),
            c => Node::Text(c.to_string(), false),
        }
    }
}

fn unwrap_paren(n: Node) -> Node {
    match n {
        Node::Paren(inner) => *inner,
        other => other,
    }
}

/// Parses the linear notation.
fn parse(text: &str) -> Node {
    let chars: Vec<char> = text.chars().take(MAX_EQUATION).collect();
    Parser { chars, i: 0, depth: 0 }.row(None)
}

/// A laid-out piece: width, height above and below the baseline, and what to draw (baseline at y = 0).
#[derive(Clone, Debug, Default)]
struct MBox {
    w: f32,
    asc: f32,
    desc: f32,
    list: DisplayList,
}

impl MBox {
    /// Places `other` with its origin at (`x`, `y`) inside `self`.
    fn put(&mut self, mut other: MBox, x: f32, y: f32) {
        translate(&mut other.list, x, y);
        self.list.items.extend(other.list.items);
        self.w = self.w.max(x + other.w);
        self.asc = self.asc.max(other.asc - y);
        self.desc = self.desc.max(other.desc + y);
    }
}

struct Ctx<'a> {
    fonts: &'a mailcraft_fonts::FontDb,
}

impl Ctx<'_> {
    fn text(&self, s: &str, pt: f32, italic: bool) -> MBox {
        let style = TextStyle {
            size_pt: Some(pt),
            italic: Some(italic),
            font_family: Some(MATH_FONT.into()),
            ..Default::default()
        };
        let (mut list, x0, y0, w, h) = text_list(s, &style, self.fonts);
        let base = list
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text { origin, .. } => Some(origin.y),
                _ => None,
            })
            .unwrap_or(y0 + h * 0.8);
        translate(&mut list, -x0, -base);
        MBox { w, asc: (base - y0).max(0.0), desc: (y0 + h - base).max(0.0), list }
    }

    fn em(pt: f32) -> f32 {
        pt * 4.0 / 3.0
    }

    fn lay(&self, n: &Node, pt: f32) -> MBox {
        let em = Self::em(pt);
        let small = (pt * 0.72).max(MIN_PT);
        match n {
            Node::Text(s, italic) => self.text(s, pt, *italic),
            Node::Op(s) => {
                let mut b = MBox::default();
                let t = self.text(s, pt, false);
                let gap = em * 0.22;
                let w = t.w;
                b.put(t, gap, 0.0);
                b.w = w + 2.0 * gap;
                b
            }
            Node::Big(c) => self.text(&c.to_string(), pt * 1.45, false),
            Node::Row(items) => {
                let mut b = MBox::default();
                let mut x = 0.0;
                for (i, it) in items.iter().enumerate() {
                    // A sign at the start or after another operator is unary: no spacing.
                    let unary = matches!(it, Node::Op(_))
                        && (i == 0
                            || matches!(items.get(i.saturating_sub(1)), Some(Node::Op(_) | Node::Big(_))));
                    let piece = if unary {
                        let Node::Op(s) = it else { continue };
                        self.text(s, pt, false)
                    } else {
                        self.lay(it, pt)
                    };
                    let w = piece.w;
                    b.put(piece, x, 0.0);
                    x += w;
                    // A function name is followed by a thin space.
                    if matches!(it, Node::Text(s, false) if FUNCTIONS.contains(&s.as_str())) {
                        x += em * 0.15;
                    }
                }
                b.w = x;
                b
            }
            Node::Frac(num, den) => {
                let fpt = (pt * 0.9).max(MIN_PT);
                let (nb, db) = (self.lay(num, fpt), self.lay(den, fpt));
                let w = nb.w.max(db.w) + em * 0.3;
                let axis = em * 0.27;
                let gap = em * 0.12;
                let bar = (em / 18.0).max(1.0);
                let mut b = MBox::default();
                let (nw, ndesc, dw, dasc) = (nb.w, nb.desc, db.w, db.asc);
                b.put(nb, (w - nw) / 2.0, -axis - gap - ndesc - bar / 2.0);
                b.put(db, (w - dw) / 2.0, -axis + gap + dasc + bar / 2.0);
                b.list.items.push(DisplayItem::Line {
                    from: Point::new(0.0, -axis),
                    to: Point::new(w, -axis),
                    color: Color::BLACK,
                    width: bar,
                });
                b.w = w + em * 0.1;
                b
            }
            Node::Scripts { base, sub, sup } => {
                let limits = matches!(**base, Node::Big('∑' | '∏'));
                let bb = self.lay(base, pt);
                let sb = sub.as_ref().map(|s| self.lay(s, small));
                let pb = sup.as_ref().map(|s| self.lay(s, small));
                let mut b = MBox::default();
                if limits {
                    let w = bb.w.max(sb.as_ref().map_or(0.0, |s| s.w)).max(pb.as_ref().map_or(0.0, |s| s.w));
                    let (basc, bdesc, bw) = (bb.asc, bb.desc, bb.w);
                    b.put(bb, (w - bw) / 2.0, 0.0);
                    if let Some(s) = sb {
                        let sw = s.w;
                        let y = bdesc + s.asc + em * 0.05;
                        b.put(s, (w - sw) / 2.0, y);
                    }
                    if let Some(p) = pb {
                        let pw = p.w;
                        let y = -basc - p.desc - em * 0.05;
                        b.put(p, (w - pw) / 2.0, y);
                    }
                    b.w = w + em * 0.1;
                } else {
                    let (basc, bw) = (bb.asc, bb.w);
                    b.put(bb, 0.0, 0.0);
                    let x = bw + em * 0.04;
                    let mut w = bw;
                    if let Some(p) = pb {
                        let pw = p.w;
                        let y = -(basc * 0.5).max(em * 0.35);
                        b.put(p, x, y);
                        w = w.max(x + pw);
                    }
                    if let Some(s) = sb {
                        let sw = s.w;
                        b.put(s, x, em * 0.22);
                        w = w.max(x + sw);
                    }
                    b.w = w + em * 0.05;
                }
                b
            }
            Node::Sqrt(inner) => {
                let ib = self.lay(inner, pt);
                let gap = em * 0.1;
                let stroke = (em / 16.0).max(1.0);
                let sign = em * 0.55;
                let top = -(ib.asc + gap);
                let bottom = ib.desc;
                let iw = ib.w;
                let mut b = MBox::default();
                b.put(ib, sign + em * 0.05, 0.0);
                let line = |a: (f32, f32), z: (f32, f32), width: f32| DisplayItem::Line {
                    from: Point::new(a.0, a.1),
                    to: Point::new(z.0, z.1),
                    color: Color::BLACK,
                    width,
                };
                let mid = (top + bottom) / 2.0 + em * 0.1;
                b.list.items.push(line((0.0, mid), (sign * 0.3, mid - em * 0.06), stroke));
                b.list.items.push(line((sign * 0.3, mid - em * 0.06), (sign * 0.55, bottom), stroke * 1.6));
                b.list.items.push(line((sign * 0.55, bottom), (sign, top), stroke));
                b.list.items.push(line((sign, top), (sign + em * 0.1 + iw, top), stroke));
                b.asc = b.asc.max(-top + stroke);
                b.desc = b.desc.max(bottom);
                b.w = sign + em * 0.15 + iw;
                b
            }
            Node::Paren(inner) => {
                let ib = self.lay(inner, pt);
                let content = ib.asc + ib.desc;
                // Taller content gets taller parentheses, centred on it.
                let ppt = if content > em * 1.3 { (pt * content / (em * 1.1)).min(pt * 4.0) } else { pt };
                let open = self.text("(", ppt, false);
                let close = self.text(")", ppt, false);
                let centre = (ib.desc - ib.asc) / 2.0;
                let shift = centre - (open.desc - open.asc) / 2.0;
                let mut b = MBox::default();
                let ow = open.w;
                let iw = ib.w;
                b.put(open, 0.0, if ppt > pt { shift } else { 0.0 });
                b.put(ib, ow, 0.0);
                b.put(close, ow + iw, if ppt > pt { shift } else { 0.0 });
                b
            }
        }
    }
}

/// The equation drawn as a PNG: (png, width, height in px). `None` for empty text or a drawing too large.
pub fn equation_png(text: &str) -> Option<(Vec<u8>, f32, f32)> {
    if text.trim().is_empty() {
        return None;
    }
    let fonts = mailcraft_fonts::FontDb::bundled();
    let cx = Ctx { fonts: &fonts };
    let b = cx.lay(&parse(text.trim()), BASE_PT);
    let (w, h) = (b.w + 2.0 * PAD, b.asc + b.desc + 2.0 * PAD);
    if !(w.is_finite() && h.is_finite())
        || w <= 2.0 * PAD
        || w > super::MAX_RASTER_SIDE
        || h > super::MAX_RASTER_SIDE
    {
        return None;
    }
    let mut list = b.list;
    translate(&mut list, PAD, PAD + b.asc);
    let mask = Mask::from_list(&list, dev(w), dev(h), &fonts);
    let mut img = super::Img::new(dev(w), dev(h));
    img.paint(&mask, 1.0, &|_, _| Color::BLACK);
    Some((img.to_png(), w.ceil(), h.ceil()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fractions_scripts_roots_and_groups() {
        assert!(matches!(parse("a/b"), Node::Frac(..)));
        let Node::Row(items) = parse("x=(−b±√(b^2−4ac))/(2a)") else { panic!() };
        assert!(
            matches!(items.last(), Some(Node::Frac(num, den)) if matches!(**num, Node::Row(_)) && matches!(**den, Node::Row(_)))
        );
        assert!(matches!(parse("x_i^2"), Node::Scripts { sub: Some(_), sup: Some(_), .. }));
        assert!(matches!(parse("√x"), Node::Sqrt(_)));
        let Node::Row(items) = parse("2+3x/4") else { panic!() };
        assert!(
            matches!(items.last(), Some(Node::Frac(num, _)) if matches!(**num, Node::Row(ref r) if r.len() == 2)),
            "{items:?}"
        );
        let Node::Row(items) = parse("sin x") else { panic!() };
        assert_eq!(items.first(), Some(&Node::Text("sin".into(), false)));
        assert_eq!(parse("3.14"), Node::Text("3.14".into(), false));
        assert_eq!(parse("y"), Node::Text("y".into(), true));
    }

    #[test]
    fn hostile_input_does_not_crash() {
        for t in [
            "",
            "/",
            "^^^",
            "((((((((((",
            "))))",
            "a/",
            "/b",
            "√",
            "x^",
            "_",
            &"(".repeat(5000),
            &"a/".repeat(500),
        ] {
            let _ = parse(t);
            let _ = equation_png(t);
        }
    }

    #[test]
    fn a_fraction_is_taller_than_a_line_and_wider_scripts_widen() {
        let (_, w1, h1) = equation_png("x").unwrap();
        let (_, _, h2) = equation_png("1/2").unwrap();
        assert!(h2 > h1, "{h1} {h2}");
        let (png, w3, _) = equation_png("x^2+y^2=r^2").unwrap();
        assert!(w3 > w1 * 3.0);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(equation_png("   ").is_none());
    }
}
