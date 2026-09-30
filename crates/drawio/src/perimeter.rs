// Derived from draw.io src/main/webapp/mxgraph/src/view/mxPerimeter.js, mxgraph/src/view/mxGraphView.js, mxgraph/src/util/mxUtils.js and js/grapheditor/Shapes.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Where an edge meets a vertex's outline.

use std::f64::consts::{FRAC_PI_2, PI};

use crate::geom::{Point, Rect};
use crate::shapes::{self, Direction, Margins};
use crate::style::{Resolved, parse_num};

/// A vertex's outline as an edge meets it: the `perimeter` its style names and what that reads
/// from the style.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Outline {
    pub kind: PerimeterKind,
    pub direction: Direction,
    /// `flipH`/`flipV` as written, not swapped for the direction.
    pub flip_h: bool,
    pub flip_v: bool,
    /// The style's `size`, which each perimeter reads with its shape's default.
    pub size: Option<f64>,
    /// `fixedSize=1`: `size` is in page units rather than a share of the width.
    pub fixed_size: bool,
}

/// The perimeters mxGraph and draw.io register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PerimeterKind {
    #[default]
    Rectangle,
    Ellipse,
    Rhombus,
    Triangle,
    Parallelogram,
    Trapezoid,
    Step,
    Hexagon,
    Callout,
}

impl Outline {
    pub fn of(style: &Resolved) -> Outline {
        Outline {
            kind: PerimeterKind::named(style.get("perimeter")),
            direction: Direction::of(style),
            flip_h: style.flag("flipH", false),
            flip_v: style.flag("flipV", false),
            size: style.get("size").and_then(parse_num),
            fixed_size: style.flag("fixedSize", false),
        }
    }

    /// The point on the outline filling `bounds` on the way to `next`, mirrored with the
    /// shape's flips; its centre if the perimeter meets nothing.
    // mxGraphView.getPerimeterPoint, mxGraphView.js 1700-1745
    pub fn point(&self, bounds: Rect, next: Point, orthogonal: bool) -> Point {
        let c = bounds.centre();
        let mirror = |p: Point| {
            Point::new(
                if self.flip_h { 2.0 * c.x - p.x } else { p.x },
                if self.flip_v { 2.0 * c.y - p.y } else { p.y },
            )
        };
        let next = mirror(next);
        let size = |fixed: f64, share: f64| match self.fixed_size {
            true => (self.size.unwrap_or(fixed), true),
            false => (self.size.unwrap_or(share), false),
        };
        let d = self.direction;
        let met = match self.kind {
            PerimeterKind::Rectangle => Some(rectangle(bounds, next, orthogonal)),
            PerimeterKind::Ellipse => Some(ellipse(bounds, next, orthogonal)),
            PerimeterKind::Rhombus => rhombus(bounds, next, orthogonal),
            PerimeterKind::Triangle => triangle(bounds, d, next, orthogonal),
            PerimeterKind::Parallelogram => {
                parallelogram(bounds, d, size(20.0, 0.2), next, orthogonal)
            }
            PerimeterKind::Trapezoid => trapezoid(bounds, d, size(20.0, 0.2), next, orthogonal),
            PerimeterKind::Step => step(bounds, d, size(20.0, 0.2), next, orthogonal),
            PerimeterKind::Hexagon => hexagon(bounds, d, size(20.0, 0.25), next, orthogonal),
            PerimeterKind::Callout => {
                let tail = self.size.unwrap_or(30.0).min(bounds.h).max(0.0);
                let m = Margins {
                    bottom: tail,
                    ..Margins::default()
                };
                let body = shapes::directed_bounds(bounds, m, d, self.flip_h, self.flip_v);
                Some(rectangle(body, next, orthogonal))
            }
        };
        met.map(mirror).unwrap_or(c)
    }
}

impl PerimeterKind {
    /// The perimeter registered under `name` (mxStyleRegistry.js 70-74, Shapes.js 3395-3673),
    /// the rectangle's for any other.
    // ponytail: mxGraph's own `hexagonPerimeter`, `centerPerimeter` and draw.io's lifeline,
    // orthogonal and backbone perimeters are met as the rectangle is.
    pub fn named(name: Option<&str>) -> PerimeterKind {
        match name {
            Some("ellipsePerimeter") => PerimeterKind::Ellipse,
            Some("rhombusPerimeter") => PerimeterKind::Rhombus,
            Some("trianglePerimeter") => PerimeterKind::Triangle,
            Some("parallelogramPerimeter") => PerimeterKind::Parallelogram,
            Some("trapezoidPerimeter") => PerimeterKind::Trapezoid,
            Some("stepPerimeter") => PerimeterKind::Step,
            Some("hexagonPerimeter2") => PerimeterKind::Hexagon,
            Some("calloutPerimeter") => PerimeterKind::Callout,
            _ => PerimeterKind::Rectangle,
        }
    }
}

/// The point on `bounds`' outline on the way to `next` (`mxPerimeter.RectanglePerimeter`).
/// `orthogonal` projects straight across instead of towards the centre.
// mxPerimeter.js 84-153
pub fn rectangle(bounds: Rect, next: Point, orthogonal: bool) -> Point {
    let c = bounds.centre();
    let alpha = (next.y - c.y).atan2(next.x - c.x);
    let beta = FRAC_PI_2 - alpha;
    let t = bounds.h.atan2(bounds.w);
    let mut p = if alpha < -PI + t || alpha > PI - t {
        // Left edge
        Point::new(bounds.x, c.y - bounds.w * alpha.tan() / 2.0)
    } else if alpha < -t {
        // Top edge
        Point::new(c.x - bounds.h * beta.tan() / 2.0, bounds.y)
    } else if alpha < t {
        // Right edge
        Point::new(bounds.right(), c.y + bounds.w * alpha.tan() / 2.0)
    } else {
        // Bottom edge
        Point::new(c.x + bounds.h * beta.tan() / 2.0, bounds.bottom())
    };
    if orthogonal {
        if next.x >= bounds.x && next.x <= bounds.right() {
            p.x = next.x;
        } else if next.y >= bounds.y && next.y <= bounds.bottom() {
            p.y = next.y;
        }
        if next.x < bounds.x {
            p.x = bounds.x;
        } else if next.x > bounds.right() {
            p.x = bounds.right();
        }
        if next.y < bounds.y {
            p.y = bounds.y;
        } else if next.y > bounds.bottom() {
            p.y = bounds.bottom();
        }
    }
    p
}

/// The same for the ellipse inscribed in `bounds` (`mxPerimeter.EllipsePerimeter`). A flat
/// ellipse, of no height or no width, is met at its centre; the JS comes to the centre too, or
/// to NaN.
// mxPerimeter.js 161-251
pub fn ellipse(bounds: Rect, next: Point, orthogonal: bool) -> Point {
    let (x, y) = (bounds.x, bounds.y);
    let (a, b) = (bounds.w / 2.0, bounds.h / 2.0);
    let (cx, cy) = (x + a, y + b);
    if a == 0.0 || b == 0.0 {
        return Point::new(cx, cy);
    }
    let (px, py) = (next.x, next.y);
    // The slope of the line through `next` and the centre, from whole-number offsets as in the JS.
    let dx = parse_int(px - cx);
    let dy = parse_int(py - cy);
    if dx == 0.0 && dy != 0.0 {
        return Point::new(cx, cy + b * dy / dy.abs());
    } else if dx == 0.0 && dy == 0.0 {
        return next;
    }
    if orthogonal {
        if py >= y && py <= bounds.bottom() {
            let ty = py - cy;
            let tx = or_zero((a * a * (1.0 - ty * ty / (b * b))).sqrt());
            return Point::new(if px <= x { cx - tx } else { cx + tx }, py);
        }
        if px >= x && px <= bounds.right() {
            let tx = px - cx;
            let ty = or_zero((b * b * (1.0 - tx * tx / (a * a))).sqrt());
            return Point::new(px, if py <= y { cy - ty } else { cy + ty });
        }
    }
    // The line y = d·x + h meets the ellipse twice; the end is the meeting nearer to `next`.
    let d = dy / dx;
    let h = cy - d * cx;
    let e = a * a * d * d + b * b;
    let f = -2.0 * cx * e;
    let g = a * a * d * d * cx * cx + b * b * cx * cx - a * a * b * b;
    let det = (f * f - 4.0 * e * g).sqrt();
    let x1 = (-f + det) / (2.0 * e);
    let x2 = (-f - det) / (2.0 * e);
    let p1 = Point::new(x1, d * x1 + h);
    let p2 = Point::new(x2, d * x2 + h);
    if p1.distance(next) < p2.distance(next) {
        p1
    } else {
        p2
    }
}

/// The diamond through the middles of `bounds`' sides (`mxPerimeter.RhombusPerimeter`).
// mxPerimeter.js 259-326
fn rhombus(bounds: Rect, next: Point, orthogonal: bool) -> Option<Point> {
    let (x, y, w, h) = (bounds.x, bounds.y, bounds.w, bounds.h);
    let Point { x: cx, y: cy } = bounds.centre();
    let (px, py) = (next.x, next.y);
    // Straight at a corner.
    if cx == px {
        return Some(Point::new(cx, if cy > py { y } else { y + h }));
    } else if cy == py {
        return Some(Point::new(if cx > px { x } else { x + w }, cy));
    }
    let (mut tx, mut ty) = (cx, cy);
    if orthogonal {
        if px >= x && px <= x + w {
            tx = px;
        } else if py >= y && py <= y + h {
            ty = py;
        }
    }
    // The side facing `next`.
    let (a, b) = match (px < cx, py < cy) {
        (true, true) => ((cx, y), (x, cy)),
        (true, false) => ((cx, y + h), (x, cy)),
        (false, true) => ((cx, y), (x + w, cy)),
        (false, false) => ((cx, y + h), (x + w, cy)),
    };
    intersection(
        next,
        Point::new(tx, ty),
        Point::new(a.0, a.1),
        Point::new(b.0, b.1),
    )
}

/// The triangle pointing `direction` in `bounds` (`mxPerimeter.TrianglePerimeter`): on its base
/// when `next` is behind it, else on the side facing `next`.
// mxPerimeter.js 334-484
fn triangle(bounds: Rect, direction: Direction, next: Point, orthogonal: bool) -> Option<Point> {
    let vertical = direction.vertical();
    let (x, y, w, h) = (bounds.x, bounds.y, bounds.w, bounds.h);
    let Point {
        x: mut cx,
        y: mut cy,
    } = bounds.centre();
    let p = Point::new;
    let (start, corner, end) = match direction {
        Direction::North => (p(x, y + h), p(cx, y), p(x + w, y + h)),
        Direction::South => (p(x, y), p(cx, y + h), p(x + w, y)),
        Direction::West => (p(x + w, y), p(x, cy), p(x + w, y + h)),
        Direction::East => (p(x, y), p(x + w, cy), p(x, y + h)),
    };
    let (dx, dy) = (next.x - cx, next.y - cy);
    let alpha = if vertical { dx.atan2(dy) } else { dy.atan2(dx) };
    let t = if vertical { w.atan2(h) } else { h.atan2(w) };
    let base = match direction {
        Direction::North | Direction::West => alpha > -t && alpha < t,
        _ => alpha < -PI + t || alpha > PI - t,
    };
    let result = if base {
        let across = match vertical {
            true => next.x >= start.x && next.x <= end.x,
            false => next.y >= start.y && next.y <= end.y,
        };
        if orthogonal && across {
            Some(match vertical {
                true => p(next.x, start.y),
                false => p(start.x, next.y),
            })
        } else {
            Some(match direction {
                Direction::North => p(x + w / 2.0 + h * alpha.tan() / 2.0, y + h),
                Direction::South => p(x + w / 2.0 - h * alpha.tan() / 2.0, y),
                Direction::West => p(x + w, y + h / 2.0 + w * alpha.tan() / 2.0),
                Direction::East => p(x, y + h / 2.0 - w * alpha.tan() / 2.0),
            })
        }
    } else {
        if orthogonal {
            // Aim from the point level with `next` rather than from the centre.
            if next.y >= y && next.y <= y + h {
                cx = match (vertical, direction) {
                    (true, _) => cx,
                    (false, Direction::West) => x + w,
                    (false, _) => x,
                };
                cy = next.y;
            } else if next.x >= x && next.x <= x + w {
                cx = next.x;
                cy = match (vertical, direction) {
                    (false, _) => cy,
                    (true, Direction::North) => y + h,
                    (true, _) => y,
                };
            }
        }
        let near_start = match vertical {
            true => next.x <= x + w / 2.0,
            false => next.y <= y + h / 2.0,
        };
        match near_start {
            true => intersection(next, p(cx, cy), start, corner),
            false => intersection(next, p(cx, cy), corner, end),
        }
    };
    Some(result.unwrap_or(p(cx, cy)))
}

/// A draw.io perimeter's `size`: page units with `fixedSize` (at most `max`), else a share of
/// `extent`.
fn inset((size, fixed): (f64, bool), extent: f64, max: f64) -> f64 {
    match fixed {
        true => size.min(max).max(0.0),
        false => extent * size.clamp(0.0, 1.0),
    }
}

/// `ParallelogramPerimeter`: leaning right, or down when facing north or south.
// Shapes.js 3398-3456
fn parallelogram(
    b: Rect,
    direction: Direction,
    size: (f64, bool),
    next: Point,
    orthogonal: bool,
) -> Option<Point> {
    let (x, y, w, h) = (b.x, b.y, b.w, b.h);
    let pts = match direction.vertical() {
        true => {
            let dy = inset(size, h, h);
            [
                (x, y),
                (x + w, y + dy),
                (x + w, y + h),
                (x, y + h - dy),
                (x, y),
            ]
        }
        false => {
            let dx = inset(size, w, w * 0.5);
            [
                (x + dx, y),
                (x + w, y),
                (x + w - dx, y + h),
                (x, y + h),
                (x + dx, y),
            ]
        }
    };
    polygon_point(b, &pts, next, orthogonal)
}

/// `TrapezoidPerimeter`: its short side the way it faces.
// Shapes.js 3461-3529
fn trapezoid(
    b: Rect,
    direction: Direction,
    size: (f64, bool),
    next: Point,
    orthogonal: bool,
) -> Option<Point> {
    let (x, y, w, h) = (b.x, b.y, b.w, b.h);
    let pts = match direction {
        Direction::East => {
            let dx = inset(size, w, w * 0.5);
            [
                (x + dx, y),
                (x + w - dx, y),
                (x + w, y + h),
                (x, y + h),
                (x + dx, y),
            ]
        }
        Direction::West => {
            let dx = inset(size, w, w);
            [
                (x, y),
                (x + w, y),
                (x + w - dx, y + h),
                (x + dx, y + h),
                (x, y),
            ]
        }
        Direction::North => {
            let dy = inset(size, h, h);
            [
                (x, y + dy),
                (x + w, y),
                (x + w, y + h),
                (x, y + h - dy),
                (x, y + dy),
            ]
        }
        Direction::South => {
            let dy = inset(size, h, h);
            [
                (x, y),
                (x + w, y + dy),
                (x + w, y + h - dy),
                (x, y + h),
                (x, y),
            ]
        }
    };
    polygon_point(b, &pts, next, orthogonal)
}

/// `StepPerimeter`: a chevron pointing the way it faces.
// Shapes.js 3534-3606
fn step(
    b: Rect,
    direction: Direction,
    size: (f64, bool),
    next: Point,
    orthogonal: bool,
) -> Option<Point> {
    let (x, y, w, h) = (b.x, b.y, b.w, b.h);
    let Point { x: cx, y: cy } = b.centre();
    let pts = match direction {
        Direction::East => {
            let dx = inset(size, w, w);
            [
                (x, y),
                (x + w - dx, y),
                (x + w, cy),
                (x + w - dx, y + h),
                (x, y + h),
                (x + dx, cy),
                (x, y),
            ]
        }
        Direction::West => {
            let dx = inset(size, w, w);
            [
                (x + dx, y),
                (x + w, y),
                (x + w - dx, cy),
                (x + w, y + h),
                (x + dx, y + h),
                (x, cy),
                (x + dx, y),
            ]
        }
        Direction::North => {
            let dy = inset(size, h, h);
            [
                (x, y + dy),
                (cx, y),
                (x + w, y + dy),
                (x + w, y + h),
                (cx, y + h - dy),
                (x, y + h),
                (x, y + dy),
            ]
        }
        Direction::South => {
            let dy = inset(size, h, h);
            [
                (x, y),
                (cx, y + dy),
                (x + w, y),
                (x + w, y + h - dy),
                (cx, y + h),
                (x, y + h - dy),
                (x, y),
            ]
        }
    };
    polygon_point(b, &pts, next, orthogonal)
}

/// `HexagonPerimeter2`: draw.io's hexagon, pointed left and right, or up and down when facing
/// north or south.
// Shapes.js 3611-3671
fn hexagon(
    b: Rect,
    direction: Direction,
    size: (f64, bool),
    next: Point,
    orthogonal: bool,
) -> Option<Point> {
    let (x, y, w, h) = (b.x, b.y, b.w, b.h);
    let Point { x: cx, y: cy } = b.centre();
    let pts = match direction.vertical() {
        true => {
            let dy = inset(size, h, h);
            [
                (cx, y),
                (x + w, y + dy),
                (x + w, y + h - dy),
                (cx, y + h),
                (x, y + h - dy),
                (x, y + dy),
                (cx, y),
            ]
        }
        false => {
            let dx = inset(size, w, w);
            [
                (x + dx, y),
                (x + w - dx, y),
                (x + w, cy),
                (x + w - dx, y + h),
                (x + dx, y + h),
                (x, cy),
                (x + dx, y),
            ]
        }
    };
    polygon_point(b, &pts, next, orthogonal)
}

/// Where the line from the centre of `b` (level with `next` across it when `orthogonal`) to
/// `next` crosses the closed polygon `pts`, nearest `next` (the tail of draw.io's polygon
/// perimeters and `mxUtils.getPerimeterPoint`, mxUtils.js 3247-3270).
fn polygon_point(b: Rect, pts: &[(f64, f64)], next: Point, orthogonal: bool) -> Option<Point> {
    let mut from = b.centre();
    if orthogonal {
        if next.x < b.x || next.x > b.right() {
            from.y = next.y;
        } else {
            from.x = next.x;
        }
    }
    pts.windows(2)
        .filter_map(|s| {
            let (a, b) = (Point::new(s[0].0, s[0].1), Point::new(s[1].0, s[1].1));
            intersection(a, b, from, next)
        })
        .min_by(|p, q| p.distance(next).total_cmp(&q.distance(next)))
}

/// Where segments `a0`–`a1` and `b0`–`b1` cross, allowing for rounding at their ends
/// (`mxUtils.intersection`, mxUtils.js 3873-3896).
fn intersection(a0: Point, a1: Point, b0: Point, b1: Point) -> Option<Point> {
    let denom = (b1.y - b0.y) * (a1.x - a0.x) - (b1.x - b0.x) * (a1.y - a0.y);
    let ua = ((b1.x - b0.x) * (a0.y - b0.y) - (b1.y - b0.y) * (a0.x - b0.x)) / denom;
    let ub = ((a1.x - a0.x) * (a0.y - b0.y) - (a1.y - a0.y) * (a0.x - b0.x)) / denom;
    let eps = 0.000001;
    let within = |u: f64| (-eps..=1.0 + eps).contains(&u);
    (within(ua) && within(ub))
        .then(|| Point::new(a0.x + ua * (a1.x - a0.x), a0.y + ua * (a1.y - a0.y)))
}

/// JavaScript's `parseInt` of a number: its digits before the point, so truncation towards zero,
/// except that a number under 1e-6 prints in exponent form (`1.4e-14`) and reads as its first
/// digit. The JS feeds rounding noise through this too, so it is kept.
fn parse_int(v: f64) -> f64 {
    if v != 0.0 && v.abs() < 1e-6 {
        let first = format!("{:e}", v.abs()).as_bytes()[0] - b'0';
        return f64::from(first).copysign(v);
    }
    v.trunc()
}

/// `Math.sqrt(…) || 0`: a root of a negative number is 0.
fn or_zero(v: f64) -> f64 {
    if v.is_nan() { 0.0 } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: Point, b: Point) -> bool {
        a.distance(b) < 1e-9
    }

    #[test]
    fn rect_perimeter_hits_the_side_facing_next() {
        let r = Rect::new(0.0, 0.0, 80.0, 40.0);
        // Towards a point up and to the right, the line through the centre leaves by the top.
        let p = rectangle(r, Point::new(60.0, -20.0), false);
        assert!(near(p, Point::new(50.0, 0.0)), "{p:?}");
        let p = rectangle(r, Point::new(200.0, 20.0), false);
        assert!(near(p, Point::new(80.0, 20.0)), "{p:?}");
        let p = rectangle(r, Point::new(-40.0, 50.0), false);
        assert!(near(p, Point::new(0.0, 35.0)), "{p:?}");
    }

    #[test]
    fn rect_perimeter_orthogonal_projects_across() {
        let r = Rect::new(0.0, 0.0, 80.0, 40.0);
        // Straight up from a point above the box, not towards its centre.
        let p = rectangle(r, Point::new(10.0, -50.0), true);
        assert!(near(p, Point::new(10.0, 0.0)), "{p:?}");
        let p = rectangle(r, Point::new(150.0, 5.0), true);
        assert!(near(p, Point::new(80.0, 5.0)), "{p:?}");
        // Beyond a corner it clamps to the corner.
        let p = rectangle(r, Point::new(150.0, 90.0), true);
        assert!(near(p, Point::new(80.0, 40.0)), "{p:?}");
    }

    #[test]
    fn ellipse_perimeter_lands_on_the_ellipse() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        let on_ellipse = |p: Point| {
            let (u, v) = ((p.x - 50.0) / 50.0, (p.y - 25.0) / 25.0);
            (u * u + v * v - 1.0).abs() < 1e-9
        };
        for next in [
            Point::new(200.0, 80.0),
            Point::new(-30.0, 10.0),
            Point::new(70.0, -90.0),
        ] {
            let p = ellipse(r, next, false);
            assert!(on_ellipse(p), "{next:?} -> {p:?}");
            // On the side facing `next`.
            assert!((p.x - 50.0) * (next.x - 50.0) > 0.0, "{next:?} -> {p:?}");
        }
        // Straight below the centre (within a unit, truncated to 0) it is the bottom.
        let p = ellipse(r, Point::new(50.7, 300.0), false);
        assert!(near(p, Point::new(50.0, 50.0)), "{p:?}");
        // Orthogonal: straight across at the height of `next`.
        let p = ellipse(r, Point::new(200.0, 25.0), true);
        assert!(near(p, Point::new(100.0, 25.0)), "{p:?}");
    }

    #[test]
    fn parse_int_reads_exponent_noise_as_a_digit() {
        assert_eq!(parse_int(-3.7), -3.0);
        assert_eq!(parse_int(1.4e-14), 1.0);
        assert_eq!(parse_int(-7.1e-15), -7.0);
        assert_eq!(parse_int(0.5), 0.0);
    }

    fn outline(kind: PerimeterKind) -> Outline {
        Outline {
            kind,
            ..Outline::default()
        }
    }

    #[test]
    fn a_rhombus_is_met_on_the_side_facing_next() {
        let r = Rect::new(0.0, 0.0, 80.0, 40.0);
        let diamond = outline(PerimeterKind::Rhombus);
        assert!(near(
            diamond.point(r, Point::new(200.0, 20.0), false),
            Point::new(80.0, 20.0)
        ));
        let p = diamond.point(r, Point::new(60.0, -20.0), false);
        assert!(near(p, Point::new(48.0, 4.0)), "{p:?}");
    }

    #[test]
    fn a_triangle_is_met_on_its_base_behind_and_its_sides_ahead_and_mirrors() {
        let r = Rect::new(0.0, 0.0, 80.0, 40.0);
        let east = outline(PerimeterKind::Triangle);
        let p = east.point(r, Point::new(-50.0, 20.0), false);
        assert!(near(p, Point::new(0.0, 20.0)), "{p:?}");
        let p = east.point(r, Point::new(200.0, 20.0), false);
        assert!(near(p, Point::new(80.0, 20.0)), "{p:?}");
        // Flipped, it points west: from the right, `next` meets the base.
        let west = Outline {
            flip_h: true,
            ..east
        };
        let p = west.point(r, Point::new(200.0, 30.0), false);
        assert!(near(p, Point::new(80.0, 22.5)), "{p:?}");
    }

    #[test]
    fn polygon_perimeters_follow_the_shapes_they_outline() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        let p = outline(PerimeterKind::Parallelogram).point(r, Point::new(-100.0, 25.0), false);
        assert!(
            near(p, Point::new(10.0, 25.0)),
            "the leaning left side: {p:?}"
        );
        let fixed = Outline {
            kind: PerimeterKind::Parallelogram,
            size: Some(40.0),
            fixed_size: true,
            ..Outline::default()
        };
        let p = fixed.point(r, Point::new(-100.0, 25.0), false);
        assert!(near(p, Point::new(20.0, 25.0)), "40 units in: {p:?}");
        let p = outline(PerimeterKind::Hexagon).point(r, Point::new(-100.0, 25.0), false);
        assert!(near(p, Point::new(0.0, 25.0)), "{p:?}");
        // A callout is met on its box, above the tail.
        let p = outline(PerimeterKind::Callout).point(r, Point::new(50.0, 200.0), false);
        assert!(near(p, Point::new(50.0, 20.0)), "{p:?}");
    }
}
