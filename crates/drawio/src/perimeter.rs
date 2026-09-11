// Derived from draw.io src/main/webapp/mxgraph/src/view/mxPerimeter.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Where an edge meets a vertex's outline.

use std::f64::consts::{FRAC_PI_2, PI};

use crate::geom::{Point, Rect};

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

/// The same for the ellipse inscribed in `bounds` (`mxPerimeter.EllipsePerimeter`).
// mxPerimeter.js 161-251
pub fn ellipse(bounds: Rect, next: Point, orthogonal: bool) -> Point {
    let (x, y) = (bounds.x, bounds.y);
    let (a, b) = (bounds.w / 2.0, bounds.h / 2.0);
    let (cx, cy) = (x + a, y + b);
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
}
