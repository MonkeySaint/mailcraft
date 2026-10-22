//! Freeform outlines with curved segments (Edit Points): a path of points, each with an optional curve
//! handle before and after it (cubic Bezier segments), drawn as a polyline; splitting a curved segment; and
//! fitting curved segments to a preset shape's drawn outline, so that its curves stay curves (an oval
//! becomes four curved segments, a rounded rectangle four straight sides and four curved corners).

use mailcraft_model::rich::{PointHandles, PointKind};

use super::shapes::P;

/// Lines one curved segment is drawn with.
const SEGMENT_STEPS: usize = 24;
/// A turn sharper than this (degrees) at a vertex of a drawn outline is a corner.
const CORNER_TURN: f32 = 18.0;
/// Most turning (degrees) one fitted curved segment covers (a wavy stretch is cut after that much).
const MAX_SEGMENT_TURN: f32 = 90.0;
/// Interior turning (degrees) below which a run of a drawn outline is a straight segment.
const STRAIGHT_TURN: f32 = 1.0;

/// One point of a path, in the coordinates of its user (body px, or the drawing's box).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Node {
    pub at: P,
    pub kind: PointKind,
    /// Handle of the segment arriving at the point.
    pub before: Option<P>,
    /// Handle of the segment leaving the point.
    pub after: Option<P>,
}

impl Node {
    pub fn corner(at: P) -> Self {
        Node { at, ..Node::default() }
    }

    /// Moves the point with its handles.
    pub fn translate(&mut self, dx: f32, dy: f32) {
        self.at = (self.at.0 + dx, self.at.1 + dy);
        self.before = self.before.map(|h| (h.0 + dx, h.1 + dy));
        self.after = self.after.map(|h| (h.0 + dx, h.1 + dy));
    }
}

fn finite(p: P) -> bool {
    p.0.is_finite() && p.1.is_finite()
}

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

fn len(v: P) -> f32 {
    (v.0 * v.0 + v.1 * v.1).sqrt()
}

fn dist(a: P, b: P) -> f32 {
    len(sub(a, b))
}

/// `v` scaled to length 1 (`None` for a zero vector).
fn unit(v: P) -> Option<P> {
    let l = len(v);
    (l > 1e-6 && l.is_finite()).then(|| (v.0 / l, v.1 / l))
}

/// Angle between two directions, 0..=180 degrees.
fn turn_between(a: P, b: P) -> f32 {
    match (unit(a), unit(b)) {
        (Some(a), Some(b)) => (a.0 * b.0 + a.1 * b.1).clamp(-1.0, 1.0).acos().to_degrees(),
        _ => 0.0,
    }
}

fn lerp(a: P, b: P, t: f32) -> P {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// The point at `t` (0..=1) of the cubic Bezier `p0` `c1` `c2` `p3`.
pub fn cubic_at(p0: P, c1: P, c2: P, p3: P, t: f32) -> P {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (a * p0.0 + b * c1.0 + c * c2.0 + d * p3.0, a * p0.1 + b * c1.1 + c * c2.1 + d * p3.1)
}

/// Splits a cubic Bezier at `t` (de Casteljau): (left handle 1, left handle 2, the point, right handle 1,
/// right handle 2).
pub fn split_cubic(p0: P, c1: P, c2: P, p3: P, t: f32) -> (P, P, P, P, P) {
    let (a, b, c) = (lerp(p0, c1, t), lerp(c1, c2, t), lerp(c2, p3, t));
    let (d, e) = (lerp(a, b, t), lerp(b, c, t));
    (a, d, lerp(d, e, t), e, c)
}

/// Segments of a path of `n` points (a closed path also runs from the last point back to the first).
pub fn segment_count(n: usize, closed: bool) -> usize {
    if closed && n >= 2 { n } else { n.saturating_sub(1) }
}

/// Segment `i` of the path: (start, handle 1, handle 2, end, curved). A segment is curved when either end
/// has a handle on its side; a missing handle is the end point itself.
pub fn segment(nodes: &[Node], i: usize, closed: bool) -> Option<(P, P, P, P, bool)> {
    let n = nodes.len();
    if i >= segment_count(n, closed) {
        return None;
    }
    let a = nodes.get(i)?;
    let b = nodes.get((i + 1) % n)?;
    let curved = a.after.is_some() || b.before.is_some();
    Some((a.at, a.after.unwrap_or(a.at), b.before.unwrap_or(b.at), b.at, curved))
}

/// The path as a polyline (a closed one without repeating its first point at the end).
pub fn flatten(nodes: &[Node], closed: bool) -> Vec<P> {
    let mut out: Vec<P> = Vec::new();
    let Some(first) = nodes.first() else { return out };
    out.push(first.at);
    for i in 0..segment_count(nodes.len(), closed) {
        let Some((p0, c1, c2, p3, curved)) = segment(nodes, i, closed) else { break };
        if curved {
            for k in 1..=SEGMENT_STEPS {
                out.push(cubic_at(p0, c1, c2, p3, k as f32 / SEGMENT_STEPS as f32));
            }
        } else {
            out.push(p3);
        }
    }
    if closed && out.len() > 1 && out.last().zip(out.first()).is_some_and(|(l, f)| dist(*l, *f) < 1e-3) {
        out.pop();
    }
    out.retain(|p| finite(*p));
    out
}

/// The parameter (0..=1) of the point of a cubic Bezier nearest `p`, kept off the very ends (a point added
/// there would sit on top of an existing one).
pub fn nearest_t(p0: P, c1: P, c2: P, p3: P, p: P) -> f32 {
    closest(p0, c1, c2, p3, p).0.clamp(0.02, 0.98)
}

/// The parameter (0..=1) of the point of a cubic Bezier nearest `p`, and its distance: a coarse search,
/// then two finer ones round the best spot (exact to a small fraction of the curve's length).
fn closest(p0: P, c1: P, c2: P, p3: P, p: P) -> (f32, f32) {
    const STEPS: usize = 64;
    let mut best = (0.5, f32::MAX);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..3 {
        for k in 0..=STEPS {
            let t = lo + (hi - lo) * k as f32 / STEPS as f32;
            let d = dist(cubic_at(p0, c1, c2, p3, t), p);
            if d < best.1 {
                best = (t, d);
            }
        }
        let span = (hi - lo) / STEPS as f32;
        (lo, hi) = ((best.0 - span).max(0.0), (best.0 + span).min(1.0));
    }
    best
}

/// Handle length of a curved segment with chord `chord` turning `theta` radians (exact for a circular
/// arc; a third of the chord for a nearly straight one).
fn handle_length(chord: f32, theta: f32) -> f32 {
    let l = if theta.abs() < 1e-3 {
        chord / 3.0
    } else {
        chord * (4.0 / 3.0) * (theta / 4.0).tan() / (2.0 * (theta / 2.0).sin()).max(1e-6)
    };
    if l.is_finite() { l.clamp(0.0, chord) } else { chord / 3.0 }
}

/// Handles for a curve from `p0` leaving along `ts` to `p3` arriving along `te` as a circular arc would have
/// them, stretched to where the two tangents meet: the handle is the same share of the way to that corner
/// at both ends. `None` when the tangents do not meet ahead of both ends.
fn arc_guess(p0: P, p3: P, ts: P, te: P) -> Option<(P, P)> {
    let d = sub(p3, p0);
    let det = ts.0 * te.1 - ts.1 * te.0;
    if det.abs() < 1e-4 {
        return None;
    }
    let a = (d.0 * te.1 - d.1 * te.0) / det;
    let b = (ts.0 * d.1 - ts.1 * d.0) / det;
    let theta = turn_between(ts, te).to_radians();
    if !(a > 0.0 && b > 0.0 && a.is_finite() && b.is_finite()) || theta >= 3.0 {
        return None;
    }
    let share = (4.0 / 3.0) * (theta / 4.0).tan() / (theta / 2.0).tan().max(1e-6);
    let share = if share.is_finite() { share.clamp(0.0, 1.0) } else { 2.0 / 3.0 };
    Some((
        (p0.0 + ts.0 * a * share, p0.1 + ts.1 * a * share),
        (p3.0 - te.0 * b * share, p3.1 - te.1 * b * share),
    ))
}

/// Distance from `p` to the line segment `a`–`b`.
fn to_edge(p: P, a: P, b: P) -> f32 {
    let ab = sub(b, a);
    let l2 = ab.0 * ab.0 + ab.1 * ab.1;
    let t = if l2 > 1e-12 { (((p.0 - a.0) * ab.0 + (p.1 - a.1) * ab.1) / l2).clamp(0.0, 1.0) } else { 0.0 };
    dist(p, lerp(a, b, t))
}

/// Distance from `p` to the cubic Bezier.
fn distance_to_cubic(p0: P, c1: P, c2: P, p3: P, p: P) -> f32 {
    closest(p0, c1, c2, p3, p).1
}

/// Fits curved and straight segments to one stretch of an outline (points `s..=e`, counted on past the end
/// of a closed outline). The pieces: (from, to, handles; `None` for a straight segment).
struct Fitter<'a> {
    at: &'a dyn Fn(usize) -> P,
    corner: &'a dyn Fn(usize) -> bool,
    turn: &'a dyn Fn(usize) -> f32,
    pieces: Vec<(usize, usize, Option<(P, P)>)>,
    budget: usize,
}

/// How far (px) a fitted curve may stray from the drawn outline.
const FIT_TOLERANCE: f32 = 0.6;
/// Rounds of moving the points' parameters along the curve while fitting one segment.
const REPARAMETERIZE: usize = 6;
/// Most steps of the handle-length search after that.
const PATTERN_STEPS: usize = 80;
/// Spots along a fitted curve checked against the outline.
const CURVE_CHECKS: usize = 16;
/// A fit this close (px) needs no more work.
const GOOD_FIT: f32 = 0.05;
/// Deepest the fitter splits one stretch.
const MAX_FIT_DEPTH: usize = 12;

impl Fitter<'_> {
    /// Direction the outline leaves point `i` in (`after`) or arrives at it in.
    fn tangent(&self, i: usize, after: bool) -> P {
        let at = self.at;
        let v = if (self.corner)(i) {
            if after { sub(at(i + 1), at(i)) } else { sub(at(i), at(i.wrapping_sub(1))) }
        } else {
            sub(at(i + 1), at(i.wrapping_sub(1)))
        };
        unit(v).unwrap_or((0.0, 0.0))
    }

    /// How far the cubic `at(s)` `c1` `c2` `at(e)` strays from the outline's points `s..=e`, both ways: the
    /// outline's points and the middles of its edges from the curve, and points along the curve from the
    /// outline. With the outline point nearest the worst spot (to split at).
    fn error(&self, s: usize, e: usize, c1: P, c2: P) -> (usize, f32) {
        let at = self.at;
        let (p0, p3) = (at(s), at(e));
        let mut worst = (s + (e - s) / 2, 0.0f32);
        let mut note = |j: usize, d: f32| {
            if d > worst.1 {
                worst = (j, d);
            }
        };
        for j in s..e {
            if j > s {
                note(j, distance_to_cubic(p0, c1, c2, p3, at(j)));
            }
            let mid = lerp(at(j), at(j + 1), 0.5);
            note(if j > s { j } else { j + 1 }, distance_to_cubic(p0, c1, c2, p3, mid));
        }
        let span = e - s;
        for k in 1..CURVE_CHECKS {
            let t = k as f32 / CURVE_CHECKS as f32;
            let q = cubic_at(p0, c1, c2, p3, t);
            let d = (s..e).map(|j| to_edge(q, at(j), at(j + 1))).fold(f32::MAX, f32::min);
            let near = s + ((t * span as f32).round() as usize).clamp(1, span.saturating_sub(1).max(1));
            note(near, d);
        }
        worst
    }

    /// Handles of one cubic through points `s..=e` leaving along `ts` and arriving along `te`: the handle
    /// lengths that fit the points best (least squares, starting from chord-length parameters and moving
    /// each point's parameter to its nearest spot on the curve a few times), else the circular-arc rule.
    fn handles(&self, s: usize, e: usize, ts: P, te: P) -> (P, P) {
        let at = self.at;
        let (p0, p3) = (at(s), at(e));
        let mut lengths = vec![0.0f32];
        for j in s + 1..=e {
            let l = lengths.last().copied().unwrap_or(0.0) + dist(at(j), at(j - 1));
            lengths.push(l);
        }
        let total = lengths.last().copied().unwrap_or(0.0).max(1e-6);
        let mut us: Vec<f32> = lengths.iter().map(|l| l / total).collect();
        let error = |c: (P, P)| self.error(s, e, c.0, c.1).1;
        let mut cur = self.solve(s, e, ts, te, &us);
        let (mut best, mut best_err) = (cur, error(cur));
        for _ in 0..REPARAMETERIZE {
            if best_err <= GOOD_FIT {
                return best;
            }
            let (c1, c2) = cur;
            for (k, j) in (s..=e).enumerate().skip(1) {
                if j == e {
                    break;
                }
                if let Some(u) = us.get_mut(k) {
                    *u = closest(p0, c1, c2, p3, at(j)).0;
                }
            }
            cur = self.solve(s, e, ts, te, &us);
            let err = error(cur);
            if err < best_err {
                (best, best_err) = (cur, err);
            }
        }
        // The arc rule through the corner where the two end tangents meet: exact for a stretch of an oval
        // (an oval is a stretched circle), which least squares can miss for a flat one.
        if let Some(guess) = arc_guess(p0, p3, ts, te) {
            let err = error(guess);
            if err < best_err {
                (best, best_err) = (guess, err);
            }
        }
        // Least squares leaves the worst point off by more than it need be: nudge the two handle lengths
        // while the worst distance shrinks (a pattern search over two numbers, so a bounded amount of work).
        let chord = dist(p0, p3);
        let (mut l1, mut l2) = (dist(best.0, p0), dist(best.1, p3));
        let handles =
            |l1: f32, l2: f32| ((p0.0 + ts.0 * l1, p0.1 + ts.1 * l1), (p3.0 - te.0 * l2, p3.1 - te.1 * l2));
        let mut step = chord * 0.05;
        for _ in 0..PATTERN_STEPS {
            if best_err <= GOOD_FIT || step < 0.005 {
                break;
            }
            // Along each length and along both diagonals (the good fits lie in a long narrow valley).
            let found = [
                (1.0, 0.0),
                (-1.0, 0.0),
                (0.0, 1.0),
                (0.0, -1.0),
                (1.0, 1.0),
                (-1.0, -1.0),
                (1.0, -1.0),
                (-1.0, 1.0),
            ]
            .into_iter()
            .map(|(a, b): (f32, f32)| (l1 + a * step, l2 + b * step))
            .filter(|(a, b)| *a > 0.0 && *b > 0.0 && *a < chord * 3.0 && *b < chord * 3.0)
            .map(|(a, b)| (a, b, error(handles(a, b))))
            .min_by(|x, y| x.2.total_cmp(&y.2))
            .filter(|(_, _, err)| *err < best_err);
            match found {
                Some((a, b, err)) => {
                    (l1, l2, best_err) = (a, b, err);
                    best = handles(a, b);
                }
                None => step /= 2.0,
            }
        }
        best
    }

    /// The least-squares handles for points `s..=e` at parameters `us`.
    fn solve(&self, s: usize, e: usize, ts: P, te: P, us: &[f32]) -> (P, P) {
        let at = self.at;
        let (p0, p3) = (at(s), at(e));
        let (mut c00, mut c01, mut c11, mut x0, mut x1) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for (k, j) in (s..=e).enumerate() {
            let u = us.get(k).copied().unwrap_or(0.0);
            let v = 1.0 - u;
            let (b0, b1, b2, b3) = (v * v * v, 3.0 * u * v * v, 3.0 * u * u * v, u * u * u);
            let a1 = (ts.0 * b1, ts.1 * b1);
            let a2 = (-te.0 * b2, -te.1 * b2);
            let d = at(j);
            let base = ((b0 + b1) * p0.0 + (b2 + b3) * p3.0, (b0 + b1) * p0.1 + (b2 + b3) * p3.1);
            let tmp = sub(d, base);
            c00 += a1.0 * a1.0 + a1.1 * a1.1;
            c01 += a1.0 * a2.0 + a1.1 * a2.1;
            c11 += a2.0 * a2.0 + a2.1 * a2.1;
            x0 += a1.0 * tmp.0 + a1.1 * tmp.1;
            x1 += a2.0 * tmp.0 + a2.1 * tmp.1;
        }
        let det = c00 * c11 - c01 * c01;
        let chord = dist(p0, p3);
        let (mut l1, mut l2) = if det.abs() > 1e-9 {
            ((x0 * c11 - x1 * c01) / det, (c00 * x1 - c01 * x0) / det)
        } else {
            (-1.0, -1.0)
        };
        let ok = |l: f32| l.is_finite() && l > chord * 1e-3 && l < chord * 3.0;
        if !ok(l1) || !ok(l2) {
            let l = handle_length(chord, turn_between(ts, te).to_radians());
            (l1, l2) = (l, l);
        }
        ((p0.0 + ts.0 * l1, p0.1 + ts.1 * l1), (p3.0 - te.0 * l2, p3.1 - te.1 * l2))
    }

    /// Fits points `s..=e`; false when that needs more pieces than the budget allows.
    fn piece(&mut self, s: usize, e: usize, depth: usize) -> bool {
        if self.pieces.len() >= self.budget {
            return false;
        }
        let at = self.at;
        let inner: f32 = (s + 1..e).map(self.turn).sum();
        if e <= s + 1 || inner < STRAIGHT_TURN {
            self.pieces.push((s, e, None));
            return true;
        }
        // A long straight edge among short curve steps (the side of a rounded rectangle) is a segment of
        // its own, even where a curve could run close to it.
        let mut edges: Vec<(usize, f32)> = (s..e).map(|k| (k, dist(at(k), at(k + 1)))).collect();
        let mut sorted: Vec<f32> = edges.iter().map(|x| x.1).collect();
        sorted.sort_by(f32::total_cmp);
        let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
        edges.sort_by(|a, b| b.1.total_cmp(&a.1));
        let long = edges.first().copied().filter(|(_, l)| *l >= 2.0 && *l >= 4.0 * median);
        let mut cuts: Vec<usize> = match long {
            Some((k, _)) => [k, k + 1].into_iter().filter(|c| *c > s && *c < e).collect(),
            None => Vec::new(),
        };
        if cuts.is_empty() {
            let (ts, te) = (self.tangent(s, true), self.tangent(e, false));
            let (c1, c2) = self.handles(s, e, ts, te);
            let (worst, err) = self.error(s, e, c1, c2);
            if err <= FIT_TOLERANCE || depth >= MAX_FIT_DEPTH {
                self.pieces.push((s, e, Some((c1, c2))));
                return true;
            }
            cuts.push(if worst > s && worst < e { worst } else { s + (e - s) / 2 });
        }
        let mut from = s;
        for c in cuts.into_iter().chain(std::iter::once(e)) {
            if !self.piece(from, c, depth + 1) {
                return false;
            }
            from = c;
        }
        true
    }
}

/// Fits a path to a drawn outline (`closed`: an area's edge): its sharp turns become corner points, its
/// straight stretches straight segments and its curves curved segments of at most a quarter turn each.
/// `None` when that needs more than `max_nodes` points.
pub fn fit(outline: &[P], closed: bool, max_nodes: usize) -> Option<Vec<Node>> {
    let mut pts: Vec<P> = Vec::new();
    for &p in outline.iter().filter(|p| finite(**p)) {
        if pts.last().is_some_and(|q| dist(*q, p) < 0.01) {
            continue;
        }
        pts.push(p);
    }
    if closed {
        while pts.len() > 1 && pts.last().zip(pts.first()).is_some_and(|(l, f)| dist(*l, *f) < 0.01) {
            pts.pop();
        }
    }
    let n = pts.len();
    if n < 2 {
        return Some(pts.into_iter().map(Node::corner).collect());
    }
    let at = |i: usize| pts.get(i % n).copied().unwrap_or_default();
    let turns: Vec<f32> = (0..n)
        .map(|i| {
            if !closed && (i == 0 || i + 1 == n) {
                180.0
            } else {
                turn_between(sub(at(i), at(i + n - 1)), sub(at(i + 1), at(i)))
            }
        })
        .collect();
    let turn = |i: usize| turns.get(i % n).copied().unwrap_or(0.0);
    let is_corner = |i: usize| turn(i) > CORNER_TURN;
    // `i - 1` of the fitter wraps: index `n - 1` stands before 0 on a closed outline.
    let at_w = |i: usize| if i == usize::MAX { at(n - 1) } else { at(i) };
    let mut breaks: Vec<usize> = (0..n).filter(|i| is_corner(*i)).collect();
    if breaks.is_empty() {
        // A closed outline without corners (an oval): start anywhere.
        breaks.push(0);
    }
    let runs = if closed { breaks.len() } else { breaks.len().saturating_sub(1) };
    let mut f = Fitter { at: &at_w, corner: &is_corner, turn: &turn, pieces: Vec::new(), budget: max_nodes };
    for k in 0..runs {
        let a = breaks.get(k).copied().unwrap_or(0);
        let b = breaks.get(k + 1).copied().unwrap_or_else(|| breaks.first().copied().unwrap_or(0) + n);
        // Each stretch handed to the fitter stays in one quadrant (it is cut where the outline is furthest
        // left, right, up or down), so it turns less than a quarter turn: an oval's four points sit at its
        // top, bottom and sides.
        let mut from = a;
        let mut acc = 0.0;
        let mut signs = (0i8, 0i8);
        for j in a..b {
            let d = sub(at(j + 1), at(j));
            let tiny = 1e-4 * len(d).max(1e-6);
            let sign = |v: f32| {
                if v > tiny {
                    1i8
                } else if v < -tiny {
                    -1
                } else {
                    0
                }
            };
            let (sx, sy) = (sign(d.0), sign(d.1));
            let flips =
                (sx != 0 && signs.0 != 0 && sx != signs.0) || (sy != 0 && signs.1 != 0 && sy != signs.1);
            if j > a {
                acc += turn(j);
            }
            if j > from && b - j >= 1 && (flips || acc >= MAX_SEGMENT_TURN) {
                if !f.piece(from, j, 0) {
                    return None;
                }
                from = j;
                acc = 0.0;
                signs = (0, 0);
            }
            if sx != 0 {
                signs.0 = sx;
            }
            if sy != 0 {
                signs.1 = sy;
            }
        }
        if !f.piece(from, b, 0) {
            return None;
        }
    }
    let pieces = f.pieces;
    if pieces.len() > max_nodes {
        return None;
    }
    // One point where each piece starts (and, for an open outline, one where the last ends).
    let mut nodes: Vec<Node> = pieces
        .iter()
        .map(|(s, _, _)| Node {
            at: at(*s),
            kind: if is_corner(*s) { PointKind::Corner } else { PointKind::Smooth },
            ..Node::default()
        })
        .collect();
    if !closed && let Some((_, e, _)) = pieces.last() {
        nodes.push(Node::corner(at(*e)));
    }
    let m = nodes.len();
    for (k, (_, _, h)) in pieces.iter().enumerate() {
        let Some((c1, c2)) = h else { continue };
        if let Some(node) = nodes.get_mut(k) {
            node.after = Some(*c1);
        }
        if let Some(node) = nodes.get_mut((k + 1) % m.max(1)) {
            node.before = Some(*c2);
        }
    }
    for node in &mut nodes {
        if node.kind != PointKind::Smooth {
            continue;
        }
        node.kind = match (node.before, node.after) {
            // In line with both handles: smooth when they are as long, else straight.
            (Some(b), Some(a)) => {
                let (lb, la) = (dist(b, node.at), dist(a, node.at));
                if (lb - la).abs() > 0.02 * lb.max(la).max(1e-3) {
                    PointKind::Straight
                } else {
                    PointKind::Smooth
                }
            }
            // Where a straight side runs into a curve without a kink.
            (Some(_), None) | (None, Some(_)) => PointKind::Straight,
            (None, None) => PointKind::Corner,
        };
    }
    Some(nodes)
}

/// Keeps the handle on `side` (true: after) of a smooth or straight point in line with the other one:
/// a smooth point's other handle mirrors it, a straight point's keeps its own length. Corner points are left
/// alone.
pub fn constrain(node: &mut Node, moved_after: bool) {
    if node.kind == PointKind::Corner {
        return;
    }
    let (moved, other) = if moved_after { (node.after, node.before) } else { (node.before, node.after) };
    // A straight segment on the other side stays straight.
    let (Some(moved), Some(other)) = (moved, other) else { return };
    let Some(dir) = unit(sub(node.at, moved)) else { return };
    let l = if node.kind == PointKind::Straight { dist(other, node.at) } else { dist(moved, node.at) };
    let opposite = Some((node.at.0 + dir.0 * l, node.at.1 + dir.1 * l));
    if moved_after {
        node.before = opposite;
    } else {
        node.after = opposite;
    }
}

/// Gives point `i` of the path handles in line with its neighbours (for a smooth or straight point that
/// had none), a third of each neighbouring segment long.
pub fn add_handles(nodes: &mut [Node], i: usize, closed: bool) {
    let n = nodes.len();
    let Some(node) = nodes.get(i).copied() else { return };
    let prev = if i > 0 || closed { nodes.get((i + n - 1) % n.max(1)).map(|p| p.at) } else { None };
    let next = if i + 1 < n || closed { nodes.get((i + 1) % n.max(1)).map(|p| p.at) } else { None };
    let dir = match (prev, next) {
        (Some(p), Some(q)) => unit(sub(q, p)),
        (None, Some(q)) => unit(sub(q, node.at)),
        (Some(p), None) => unit(sub(node.at, p)),
        (None, None) => None,
    };
    let Some(dir) = dir else { return };
    let Some(slot) = nodes.get_mut(i) else { return };
    if let Some(p) = prev
        && slot.before.is_none()
    {
        let l = dist(p, node.at) / 3.0;
        slot.before = Some((node.at.0 - dir.0 * l, node.at.1 - dir.1 * l));
    }
    if let Some(q) = next
        && slot.after.is_none()
    {
        let l = dist(q, node.at) / 3.0;
        slot.after = Some((node.at.0 + dir.0 * l, node.at.1 + dir.1 * l));
    }
}

/// The nodes of points and handles given as fractions of a `w`×`h` box at (`x`, `y`).
pub fn nodes_from(points: &[P], handles: &[PointHandles], x: f32, y: f32, w: f32, h: f32) -> Vec<Node> {
    let abs = |p: P| (x + p.0 * w, y + p.1 * h);
    points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let hd = handles.get(i).copied().unwrap_or_default();
            Node { at: abs(*p), kind: hd.kind, before: hd.before.map(abs), after: hd.after.map(abs) }
        })
        .collect()
}

/// The box round the drawn path: (x, y, w, h).
pub fn bounds(nodes: &[Node], closed: bool) -> Option<(f32, f32, f32, f32)> {
    let line = flatten(nodes, closed);
    let mut it = line.iter().chain(nodes.iter().map(|n| &n.at)).filter(|p| finite(**p));
    let first = *it.next()?;
    let (x0, y0, x1, y1) = it.fold((first.0, first.1, first.0, first.1), |a, p| {
        (a.0.min(p.0), a.1.min(p.1), a.2.max(p.0), a.3.max(p.1))
    });
    Some((x0, y0, x1 - x0, y1 - y0))
}

/// Furthest a handle may lie outside the box, in box sizes (a guard against hostile data).
pub const HANDLE_REACH: f32 = 8.0;

/// The points and handles of `nodes` as fractions of the box (`x`, `y`, `w`, `h`); handles of plain corner
/// points with straight segments are left out (an empty list when every point is plain).
pub fn to_fractions(nodes: &[Node], x: f32, y: f32, w: f32, h: f32) -> (Vec<P>, Vec<PointHandles>) {
    let (w, h) = (w.max(1e-3), h.max(1e-3));
    let rel = |p: P| (((p.0 - x) / w), ((p.1 - y) / h));
    let clamp_h =
        |p: P| (p.0.clamp(-HANDLE_REACH, 1.0 + HANDLE_REACH), p.1.clamp(-HANDLE_REACH, 1.0 + HANDLE_REACH));
    let points: Vec<P> = nodes
        .iter()
        .map(|n| {
            let r = rel(n.at);
            (r.0.clamp(0.0, 1.0), r.1.clamp(0.0, 1.0))
        })
        .collect();
    let handles: Vec<PointHandles> = nodes
        .iter()
        .map(|n| PointHandles {
            kind: n.kind,
            before: n.before.map(rel).filter(|p| finite(*p)).map(clamp_h),
            after: n.after.map(rel).filter(|p| finite(*p)).map(clamp_h),
        })
        .collect();
    let handles = if handles.iter().all(PointHandles::is_plain) { Vec::new() } else { handles };
    (points, handles)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circle(cx: f32, cy: f32, r: f32, steps: usize) -> Vec<P> {
        (0..steps)
            .map(|i| {
                let a = i as f32 / steps as f32 * std::f32::consts::TAU;
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    }

    #[test]
    fn a_circle_fits_as_four_smooth_curved_segments_that_stay_round() {
        let nodes = fit(&circle(50.0, 50.0, 40.0, 96), true, 48).unwrap();
        assert_eq!(nodes.len(), 4, "{nodes:?}");
        // No kinks: every point keeps its two handles in line.
        assert!(nodes.iter().all(|n| n.kind != PointKind::Corner && n.before.is_some() && n.after.is_some()));
        // Every drawn point of the fitted path lies on the circle (within a fraction of a pixel).
        let line = flatten(&nodes, true);
        assert!(line.len() > 50);
        let worst = line.iter().map(|p| (dist(*p, (50.0, 50.0)) - 40.0).abs()).fold(0.0, f32::max);
        assert!(worst < 0.3, "off the circle by {worst}");
    }

    #[test]
    fn a_square_fits_as_four_corners_with_straight_sides() {
        let sq = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let nodes = fit(&sq, true, 48).unwrap();
        assert_eq!(nodes.len(), 4);
        assert!(nodes.iter().all(|n| n.kind == PointKind::Corner && n.before.is_none() && n.after.is_none()));
        assert_eq!(flatten(&nodes, true), sq.to_vec());
    }

    #[test]
    fn a_rounded_corner_is_one_curved_segment_between_straight_sides() {
        // A square's top right corner rounded with radius 10, drawn in 24 steps.
        let mut outline = vec![(0.0, 0.0), (40.0, 0.0)];
        for i in 1..24 {
            let a = (270.0 + 90.0 * i as f32 / 24.0).to_radians();
            outline.push((40.0 + 10.0 * a.cos(), 10.0 + 10.0 * a.sin()));
        }
        outline.extend([(50.0, 10.0), (50.0, 50.0), (0.0, 50.0)]);
        let nodes = fit(&outline, true, 48).unwrap();
        let curved = (0..segment_count(nodes.len(), true))
            .filter(|i| segment(&nodes, *i, true).is_some_and(|s| s.4))
            .count();
        assert_eq!((nodes.len(), curved), (5, 1), "{nodes:?}");
    }

    #[test]
    fn splitting_keeps_the_curve() {
        let (p0, c1, c2, p3) = ((0.0, 0.0), (0.0, 50.0), (100.0, 50.0), (100.0, 0.0));
        let (l1, l2, mid, r1, r2) = split_cubic(p0, c1, c2, p3, 0.3);
        assert!(dist(mid, cubic_at(p0, c1, c2, p3, 0.3)) < 1e-3);
        assert!(dist(cubic_at(p0, l1, l2, mid, 0.5), cubic_at(p0, c1, c2, p3, 0.15)) < 1e-3);
        assert!(dist(cubic_at(mid, r1, r2, p3, 0.5), cubic_at(p0, c1, c2, p3, 0.65)) < 1e-3);
        assert!((nearest_t(p0, c1, c2, p3, mid) - 0.3).abs() < 0.02);
    }

    #[test]
    fn smooth_and_straight_points_keep_their_handles_in_line() {
        let mut n = Node {
            at: (0.0, 0.0),
            kind: PointKind::Smooth,
            before: Some((-5.0, 0.0)),
            after: Some((10.0, 0.0)),
        };
        n.after = Some((0.0, 8.0));
        constrain(&mut n, true);
        assert_eq!(n.before, Some((0.0, -8.0)), "mirrored, same length");
        n.kind = PointKind::Straight;
        n.before = Some((-3.0, 0.0));
        constrain(&mut n, false);
        assert_eq!(n.after, Some((8.0, 0.0)), "in line, keeping its own length");
        n.kind = PointKind::Corner;
        n.after = Some((1.0, 1.0));
        constrain(&mut n, true);
        assert_eq!(n.before, Some((-3.0, 0.0)), "a corner's handles are independent");
    }

    #[test]
    fn hostile_outlines_do_not_panic() {
        assert!(fit(&[], true, 48).unwrap().is_empty());
        assert_eq!(fit(&[(f32::NAN, 1.0), (1.0, 1.0)], false, 48).unwrap().len(), 1);
        let spiky: Vec<P> =
            (0..500).map(|i| if i % 2 == 0 { (i as f32, 0.0) } else { (i as f32, 10.0) }).collect();
        assert!(fit(&spiky, false, 48).is_none(), "too many corners for the cap");
        assert!(flatten(&[], true).is_empty());
        assert!(segment(&[Node::corner((0.0, 0.0))], 0, true).is_none());
        let mut one = [Node::corner((1.0, 1.0))];
        add_handles(&mut one, 0, true);
        add_handles(&mut one, 5, true);
    }
}
