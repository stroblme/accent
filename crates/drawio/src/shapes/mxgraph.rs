// Derived from draw.io mxgraph/src/shape/mxRectangleShape.js, mxgraph/src/shape/mxEllipse.js, mxgraph/src/shape/mxPolyline.js, mxgraph/src/util/mxSvgCanvas2D.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! The shapes mxGraph registers itself (mxCellRenderer.js 130-145), and its edge line.

use super::{LINE_ARCSIZE, RECTANGLE_ROUNDING_FACTOR, add_points, quarter, rect};
use crate::geom::{PathCmd, Point, Rect};
use crate::style::Resolved;

/// An edge's line through `points` (ends already shortened for markers): straight segments,
/// rounded corners (`rounded=1`) or a smooth curve (`curved=1`).
///
/// `mxPolyline.paintEdgeShape` and `paintLine` (mxPolyline.js 73-105).
// ponytail: `bezier=1` (`paintBezierLine`, which takes precedence over `curved`) is not read;
// such an edge is drawn curved or straight.
pub fn edge_line(points: &[Point], style: &Resolved) -> Vec<PathCmd> {
    if points.len() < 2 {
        return Vec::new();
    }
    if style.flag("curved", false) {
        return curved_line(points);
    }
    let arc = style.num("arcSize", LINE_ARCSIZE) / 2.0;
    add_points(points, style.flag("rounded", false), arc, false)
}

/// A smooth line through `pts`: quads with the waypoints as control points, meeting halfway
/// between them (`mxPolyline.paintCurvedLine`, mxPolyline.js 112-136). Needs two points.
fn curved_line(pts: &[Point]) -> Vec<PathCmd> {
    let n = pts.len();
    let mut path = vec![PathCmd::MoveTo(pts[0])];
    for pair in pts[1..n - 1].windows(2) {
        let (p0, p1) = (pair[0], pair[1]);
        path.push(PathCmd::QuadTo(
            p0,
            Point::new((p0.x + p1.x) / 2.0, (p0.y + p1.y) / 2.0),
        ));
    }
    path.push(PathCmd::QuadTo(pts[n - 2], pts[n - 1]));
    path
}

/// `label` and `rectangle`: `mxRectangleShape.paintBackground` (mxRectangleShape.js 62-88). A
/// relative `arcSize` is a percentage of the shorter side, an absolute one (`absoluteArcSize=1`)
/// twice the corner radius in page units.
pub(super) fn rectangle(b: Rect, style: &Resolved) -> Vec<PathCmd> {
    if !style.flag("rounded", false) {
        return rect(b);
    }
    let r = if style.flag("absoluteArcSize", false) {
        (b.w / 2.0)
            .min(b.h / 2.0)
            .min(style.num("arcSize", LINE_ARCSIZE) / 2.0)
    } else {
        let f = style.num("arcSize", RECTANGLE_ROUNDING_FACTOR * 100.0) / 100.0;
        (b.w * f).min(b.h * f)
    };
    round_rect(b, r)
}

/// `mxSvgCanvas2D.roundrect` (mxSvgCanvas2D.js 1633-1646) writes `<rect rx ry>`, so the corners
/// are quarter ellipses, their radii clamped to half the width and height, and no radius is a
/// plain rectangle. The path follows SVG's own for a rounded `<rect>`.
fn round_rect(b: Rect, r: f64) -> Vec<PathCmd> {
    if r <= 0.0 {
        return rect(b);
    }
    let (rx, ry) = (r.min(b.w / 2.0), r.min(b.h / 2.0));
    let (x0, y0, x1, y1) = (b.x, b.y, b.right(), b.bottom());
    let p = Point::new;
    vec![
        PathCmd::MoveTo(p(x0 + rx, y0)),
        PathCmd::LineTo(p(x1 - rx, y0)),
        quarter(p(x1 - rx, y0), p(x1, y0), p(x1, y0 + ry)),
        PathCmd::LineTo(p(x1, y1 - ry)),
        quarter(p(x1, y1 - ry), p(x1, y1), p(x1 - rx, y1)),
        PathCmd::LineTo(p(x0 + rx, y1)),
        quarter(p(x0 + rx, y1), p(x0, y1), p(x0, y1 - ry)),
        PathCmd::LineTo(p(x0, y0 + ry)),
        quarter(p(x0, y0 + ry), p(x0, y0), p(x0 + rx, y0)),
        PathCmd::Close,
    ]
}

/// The ellipse filling `b` (`mxEllipse.paintVertexShape`, mxEllipse.js 44-56, drawn as SVG's
/// `<ellipse>`, mxSvgCanvas2D.js 1653-1663): four quarters from the right-hand side's midpoint,
/// clockwise, as SVG draws it.
pub(crate) fn ellipse(b: Rect) -> Vec<PathCmd> {
    let c = b.centre();
    let (right, bottom) = (Point::new(b.right(), c.y), Point::new(c.x, b.bottom()));
    let (left, top) = (Point::new(b.x, c.y), Point::new(c.x, b.y));
    vec![
        PathCmd::MoveTo(right),
        quarter(right, Point::new(b.right(), b.bottom()), bottom),
        quarter(bottom, Point::new(b.x, b.bottom()), left),
        quarter(left, Point::new(b.x, b.y), top),
        quarter(top, Point::new(b.right(), b.y), right),
        PathCmd::Close,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::path_bounds;
    use crate::shapes::tests::{BOX, near, same_box, start, style};
    use crate::shapes::vertex;

    #[test]
    fn rounded_rect_arc_follows_arc_size() {
        let path = &vertex("rectangle", BOX, &style("rounded=1;", false))[0].path;
        assert!(
            near(start(path), Point::new(16.0, 20.0)),
            "15 % of the shorter side"
        );
        let curves = path.iter().filter(|c| matches!(c, PathCmd::CurveTo(..)));
        assert_eq!(curves.count(), 4);
        let path = &vertex("rectangle", BOX, &style("rounded=1;arcSize=50;", false))[0].path;
        assert!(near(start(path), Point::new(30.0, 20.0)));
        let path = &vertex("rectangle", BOX, &style("rounded=0;", false))[0].path;
        assert_eq!(path, &rect(BOX));
    }

    #[test]
    fn absolute_arc_size_is_in_units() {
        let s = style("rounded=1;absoluteArcSize=1;arcSize=10;", false);
        assert!(near(
            start(&vertex("label", BOX, &s)[0].path),
            Point::new(15.0, 20.0)
        ));
        let s = style("rounded=1;absoluteArcSize=1;", false);
        assert!(
            near(
                start(&vertex("label", BOX, &s)[0].path),
                Point::new(20.0, 20.0)
            ),
            "LINE_ARCSIZE by default"
        );
    }

    #[test]
    fn ellipse_is_four_curves_through_the_side_midpoints() {
        let path = &vertex("ellipse", BOX, &style("ellipse;", false))[0].path;
        let ends: Vec<Point> = path
            .iter()
            .filter_map(|c| match c {
                PathCmd::CurveTo(_, _, p) => Some(*p),
                _ => None,
            })
            .collect();
        let expected = [(60.0, 60.0), (10.0, 40.0), (60.0, 20.0), (110.0, 40.0)];
        assert_eq!(ends.len(), 4);
        for (p, (x, y)) in ends.iter().zip(expected) {
            assert!(near(*p, Point::new(x, y)), "{p:?}");
        }
        assert!(near(start(path), Point::new(110.0, 40.0)));
        assert!(same_box(path_bounds(path).unwrap(), BOX));
    }

    #[test]
    fn curved_line_is_quads_through_midpoints() {
        let pts =
            [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (20.0, 10.0)].map(|(x, y)| Point::new(x, y));
        let path = edge_line(&pts, &style("curved=1;", true));
        assert_eq!(
            path,
            vec![
                PathCmd::MoveTo(pts[0]),
                PathCmd::QuadTo(pts[1], Point::new(10.0, 5.0)),
                PathCmd::QuadTo(pts[2], pts[3]),
            ]
        );
    }
}
