//! Ink geometry: the curve through a stroke, the path a shape draws, and the hit test.
//!
//! Pure, and here rather than in the widget so Android draws the same curve through the same
//! functions.

use super::Shape;

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
    let at = |i: isize| points[(i.max(0) as usize).min(points.len() - 1)];
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

/// The path as one polyline for [`hit`]: a curve is sampled at three points before its end, and
/// a close goes back to where the sub-path began.
///
// ponytail: one polyline, so a second sub-path is joined to the first by a segment nobody drew.
// Our paths have one sub-path; another editor's multi-stroke `/Ink` gains a false edge.
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
                out.extend([0.25, 0.5, 0.75].map(|t| cubic(last, c1, c2, end, t)));
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

/// Whether `at` lies within `radius` of the polyline through `points` — the eraser's hit test.
pub fn hit(points: &[(f32, f32)], at: (f32, f32), radius: f32) -> bool {
    let near = |a: (f32, f32), b: (f32, f32)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        // A degenerate segment is its own endpoint.
        let t = match len2 > f32::EPSILON {
            true => (((at.0 - a.0) * dx + (at.1 - a.1) * dy) / len2).clamp(0.0, 1.0),
            false => 0.0,
        };
        (at.0 - (a.0 + t * dx)).hypot(at.1 - (a.1 + t * dy)) <= radius
    };
    match points {
        [] => false,
        [only] => near(*only, *only),
        _ => points.windows(2).any(|w| near(w[0], w[1])),
    }
}
