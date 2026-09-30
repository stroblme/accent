//! Ink geometry: the curve through a stroke, the path a shape draws, and the hit test.
//!
//! Pure, and here rather than in the widget so Android draws the same curve through the same
//! functions.

use super::{InkStyle, Shape};

/// An affine map `[a, b, c, d, e, f]` in the PDF convention: `x' = a·x + c·y + e`,
/// `y' = b·x + d·y + f`.
pub type Matrix = [f32; 6];

pub const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

pub fn apply(m: Matrix, (x, y): (f32, f32)) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// The map that undoes `m`. Undefined for a map that flattens the plane, which no drag makes.
pub fn invert(m: Matrix) -> Matrix {
    let [a, b, c, d, e, f] = m;
    let det = a * d - b * c;
    [
        d / det,
        -b / det,
        -c / det,
        a / det,
        (c * f - d * e) / det,
        (b * e - a * f) / det,
    ]
}

/// One segment of an appearance path, in top-left page points: what every ink writer hands
/// [`PdfDoc::put_ink`], and what [`read_ink`] gives back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Seg {
    Move((f32, f32)),
    Line((f32, f32)),
    /// Two control points, then the end.
    Bezier((f32, f32), (f32, f32), (f32, f32)),
    Close,
}

/// A stroke's path and style as they were read back off its page: what
/// [`PdfDoc::redraw_ink`](super::PdfDoc::redraw_ink) needs to draw it again once the annotation
/// is gone.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawn {
    pub(super) segs: Vec<Seg>,
    pub(super) style: InkStyle,
    pub(super) extra: Extra,
}

/// What another editor's stroke carries besides what it draws, which a stroke drawn again puts
/// back: the strokes of its `/InkList`, in top-left page points, its note (`/Contents`) and its
/// author (`/T`). Nothing, for ours and for the pieces a cut leaves.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct Extra {
    pub(super) ink_list: Vec<Vec<(f32, f32)>>,
    pub(super) contents: Option<String>,
    pub(super) author: Option<String>,
}

impl Extra {
    /// The same under `m`: the `/InkList` goes with the stroke.
    pub(super) fn transformed(&self, m: Matrix) -> Extra {
        Extra {
            ink_list: (self.ink_list.iter())
                .map(|stroke| stroke.iter().map(|&p| apply(m, p)).collect())
                .collect(),
            ..self.clone()
        }
    }
}

impl Drawn {
    /// The same stroke along another path, drawn straight from point to point: a piece of it.
    pub(super) fn along(&self, points: &[(f32, f32)]) -> Drawn {
        Drawn {
            segs: polyline(points),
            style: self.style,
            extra: Extra::default(),
        }
    }
}

/// Straight lines through `points`, a lone point being a dot: a zero-length line, which the
/// round cap draws.
pub(super) fn polyline(points: &[(f32, f32)]) -> Vec<Seg> {
    let mut segs: Vec<Seg> = points
        .iter()
        .enumerate()
        .map(|(i, &p)| match i {
            0 => Seg::Move(p),
            _ => Seg::Line(p),
        })
        .collect();
    if let [Seg::Move(p)] = segs[..] {
        segs.push(Seg::Line(p));
    }
    segs
}

/// Drop points closer than `min` to the last one kept.
///
/// A pointer reports on every motion event, so a slow stroke arrives as a cloud of near-identical
/// points; thinning first is what keeps the curve below from wobbling between them.
pub fn thin(points: &[(f32, f32)], min: f32) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::new();
    for &p in points {
        let far = out
            .last()
            .is_none_or(|&(x, y)| (p.0 - x).hypot(p.1 - y) >= min);
        if far {
            out.push(p);
        }
    }
    // A stroke that never moved is still a dot, not nothing.
    if out.is_empty()
        && let Some(&p) = points.first()
    {
        out.push(p);
    }
    out
}

/// A Catmull-Rom spline through `points` as cubic Béziers: `[control 1, control 2, end]` per
/// segment, ready for `bezier_to`.
///
// ponytail: the ends repeat the first and last point rather than extrapolating a phantom one,
// which is the standard clamped form and means a stroke starts and ends exactly where the pointer
// did. Not Savitzky-Golay (what UNote uses): that wants a least-squares solve over a 31-sample
// window, lags the pointer by half of it, and has to be re-run over the whole stroke on every
// motion event. Catmull-Rom is local, closed-form, and its output is the argument `bezier_to`
// already takes.
pub fn catmull_rom(points: &[(f32, f32)]) -> Vec<[(f32, f32); 3]> {
    // No points is no curve. Said here rather than left to the caller: `len() - 1` below would
    // wrap, and this is public.
    let Some(last) = points.len().checked_sub(1) else {
        return Vec::new();
    };
    let at = |i: isize| points[(i.max(0) as usize).min(last)];
    (0..points.len().saturating_sub(1))
        .map(|i| {
            let i = i as isize;
            let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
            [
                (p1.0 + (p2.0 - p0.0) / 6.0, p1.1 + (p2.1 - p0.1) / 6.0),
                (p2.0 - (p3.0 - p1.0) / 6.0, p2.1 - (p3.1 - p1.1) / 6.0),
                p2,
            ]
        })
        .collect()
}

/// The path a shape draws, starting with a move.
pub(super) fn segments_of(shape: Shape) -> Vec<Seg> {
    match shape {
        Shape::Line { a, b } => vec![Seg::Move(a), Seg::Line(b)],
        Shape::Rect(r) => vec![
            Seg::Move((r.left, r.top)),
            Seg::Line((r.right, r.top)),
            Seg::Line((r.right, r.bottom)),
            Seg::Line((r.left, r.bottom)),
            Seg::Close,
        ],
        Shape::Circle {
            centre: (cx, cy),
            radius: r,
        } => {
            // Four quarter arcs, with the control distance that puts a cubic closest to a circle.
            let k = r * 0.551_915;
            vec![
                Seg::Move((cx - r, cy)),
                Seg::Bezier((cx - r, cy - k), (cx - k, cy - r), (cx, cy - r)),
                Seg::Bezier((cx + k, cy - r), (cx + r, cy - k), (cx + r, cy)),
                Seg::Bezier((cx + r, cy + k), (cx + k, cy + r), (cx, cy + r)),
                Seg::Bezier((cx - k, cy + r), (cx - r, cy + k), (cx - r, cy)),
                Seg::Close,
            ]
        }
    }
}

/// The same path under `m`. Control points go with the rest: a Bézier is affine-invariant.
pub(super) fn transformed(segs: &[Seg], m: Matrix) -> Vec<Seg> {
    segs.iter()
        .map(|seg| match *seg {
            Seg::Move(p) => Seg::Move(apply(m, p)),
            Seg::Line(p) => Seg::Line(apply(m, p)),
            Seg::Bezier(c1, c2, end) => Seg::Bezier(apply(m, c1), apply(m, c2), apply(m, end)),
            Seg::Close => Seg::Close,
        })
        .collect()
}

/// Every point a path names, control points included.
pub(super) fn points_of(segs: &[Seg]) -> impl Iterator<Item = (f32, f32)> + '_ {
    segs.iter().flat_map(|seg| match *seg {
        Seg::Move(p) | Seg::Line(p) => vec![p],
        Seg::Bezier(c1, c2, end) => vec![c1, c2, end],
        Seg::Close => vec![],
    })
}

/// How far a flattened curve strays from the curve, at most, in page points: half a pixel at the
/// viewer's largest zoom, so a stroke redrawn straight between its flattened points — which is
/// what a cut leaves of it — lies on the one it came from.
const FLAT: f32 = 0.05;

/// The shortest piece a cut keeps, in page points. Anything shorter would draw as a dot of the
/// stroke's width under its round cap: a crumb nobody meant to leave.
const CRUMB: f32 = 1.0;

/// The path as one polyline for [`hit`] and [`cut`]: a curve is walked in as many straight steps
/// as keep it within [`FLAT`] of itself, and a close goes back to where the sub-path began.
///
// ponytail: one polyline, so a second sub-path is joined to the first by a segment nobody drew.
// Our paths have one sub-path; another editor's multi-stroke `/Ink` gains a false edge, and is
// not cut (see `InkShape::cuttable`).
pub(super) fn flatten(segs: &[Seg]) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    let mut start = None;
    let mut last = (0.0, 0.0);
    for seg in segs {
        match *seg {
            Seg::Move(p) => {
                start = Some(p);
                last = p;
                out.push(p);
            }
            Seg::Line(p) => {
                last = p;
                out.push(p);
            }
            Seg::Bezier(c1, c2, end) => {
                // Wang's bound: a cubic whose control points bend by at most `bend` stays within
                // FLAT of the chords of `n` equal steps once n² ≥ 3·bend / (4·FLAT).
                let bend = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
                    (a.0 - 2.0 * b.0 + c.0).hypot(a.1 - 2.0 * b.1 + c.1)
                };
                let most = bend(last, c1, c2).max(bend(c1, c2, end));
                let n = ((0.75 * most / FLAT).sqrt().ceil() as usize).max(1);
                out.extend((1..n).map(|i| cubic(last, c1, c2, end, i as f32 / n as f32)));
                last = end;
                out.push(end);
            }
            Seg::Close => {
                if let Some(p) = start {
                    last = p;
                    out.push(p);
                }
            }
        }
    }
    out
}

/// A point on a cubic Bézier.
fn cubic(p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32), t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    (
        w[0] * p0.0 + w[1] * p1.0 + w[2] * p2.0 + w[3] * p3.0,
        w[0] * p0.1 + w[1] * p1.1 + w[2] * p2.1 + w[3] * p3.1,
    )
}

/// Whether `at` lies within `radius` of the polyline through `points` — the Adjust tool's hit
/// test.
pub fn hit(points: &[(f32, f32)], at: (f32, f32), radius: f32) -> bool {
    swept(points, at, at, radius)
}

/// Whether the pointer, moved in a straight line from `from` to `to`, passed within `radius` of
/// the polyline through `points` — the eraser's hit test.
///
/// The line and not only its ends: a drag reports once a frame, so a quick pass lands one report
/// either side of a thin stroke and neither of them near it.
pub fn swept(points: &[(f32, f32)], from: (f32, f32), to: (f32, f32), radius: f32) -> bool {
    match points {
        [] => false,
        [only] => apart((*only, *only), (from, to)) <= radius,
        _ => points
            .windows(2)
            .any(|w| apart((w[0], w[1]), (from, to)) <= radius),
    }
}

type Segment = ((f32, f32), (f32, f32));

/// How close two segments come: nothing where they cross, else the nearest any end of one comes
/// to the other.
fn apart((a, b): Segment, (c, d): Segment) -> f32 {
    // Which side of the line through `p` and `q` the point `r` is on, by the sign.
    let side = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| {
        (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0)
    };
    if side(a, b, c) * side(a, b, d) < 0.0 && side(c, d, a) * side(c, d, b) < 0.0 {
        return 0.0;
    }
    [
        to_segment(a, (c, d)),
        to_segment(b, (c, d)),
        to_segment(c, (a, b)),
        to_segment(d, (a, b)),
    ]
    .into_iter()
    .fold(f32::MAX, f32::min)
}

/// How far `p` is from the segment `a`–`b`.
fn to_segment(p: (f32, f32), (a, b): Segment) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    // A degenerate segment is its own endpoint.
    let t = match len2 > f32::EPSILON {
        true => (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0),
        false => 0.0,
    };
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// What is left of the polyline through `points` once the pointer, moved in a straight line from
/// `from` to `to`, has taken everything within `reach` of its path: the runs outside that reach,
/// in order and without the crumbs. `None` when the path came nowhere near, which leaves the
/// stroke alone; nothing at all when it took the lot.
pub fn cut(
    points: &[(f32, f32)],
    from: (f32, f32),
    to: (f32, f32),
    reach: f32,
) -> Option<Vec<Vec<(f32, f32)>>> {
    let path = (from, to);
    // A dot is taken whole or not at all.
    if let [only] = points {
        return (to_segment(*only, path) <= reach).then(Vec::new);
    }
    let mut runs = Vec::new();
    let mut run: Vec<(f32, f32)> = Vec::new();
    let mut touched = false;
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let Some((enter, leave)) = within((a, b), path, reach) else {
            if run.is_empty() {
                run.push(a);
            }
            run.push(b);
            continue;
        };
        touched = true;
        if enter > 0.0 {
            if run.is_empty() {
                run.push(a);
            }
            run.push(lerp(a, b, enter));
        }
        runs.push(std::mem::take(&mut run));
        if leave < 1.0 {
            run = vec![lerp(a, b, leave), b];
        }
    }
    if !touched {
        return None;
    }
    runs.push(run);
    // A closed path cut once is one piece, not two that meet where it began.
    let closed = points.len() > 2 && points.first() == points.last();
    let ends = |run: &Vec<(f32, f32)>| (run.first().copied(), run.last().copied());
    if closed
        && runs.len() > 1
        && ends(&runs[0]).0 == points.first().copied()
        && runs.last().and_then(|r| ends(r).1) == points.last().copied()
    {
        let first = runs.remove(0);
        if let Some(last) = runs.last_mut() {
            last.extend(first.into_iter().skip(1));
        }
    }
    runs.retain(|run| length(run) >= CRUMB);
    Some(runs)
}

/// Which part of the segment `a`–`b` lies within `reach` of `path`, as the span of it from `a`
/// (0) to `b` (1), or nothing. The distance to a segment is convex along another, so that part
/// is one span: its nearest point is found by narrowing thirds, and each end by halving.
fn within((a, b): Segment, path: Segment, reach: f32) -> Option<(f32, f32)> {
    let d = |t: f32| to_segment(lerp(a, b, t), path);
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..40 {
        let (l, h) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
        match d(l) < d(h) {
            true => hi = h,
            false => lo = l,
        }
    }
    let nearest = (lo + hi) / 2.0;
    if d(nearest) > reach {
        return None;
    }
    // From the nearest point out towards `end`, the last place still within reach.
    let edge = |end: f32| {
        if d(end) <= reach {
            return end;
        }
        let (mut inside, mut outside) = (nearest, end);
        for _ in 0..24 {
            let mid = (inside + outside) / 2.0;
            match d(mid) <= reach {
                true => inside = mid,
                false => outside = mid,
            }
        }
        inside
    };
    Some((edge(0.0), edge(1.0)))
}

/// The point `t` of the way from `a` to `b`.
fn lerp(a: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1))
}

/// How long the polyline through `points` is.
fn length(points: &[(f32, f32)]) -> f32 {
    points
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
        .sum()
}
