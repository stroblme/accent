// Derived from draw.io mxgraph/src/shape/mxRectangleShape.js, mxgraph/src/shape/mxEllipse.js, mxgraph/src/shape/mxDoubleEllipse.js, mxgraph/src/shape/mxRhombus.js, mxgraph/src/shape/mxTriangle.js, mxgraph/src/shape/mxCloud.js, mxgraph/src/shape/mxActor.js, mxgraph/src/shape/mxCylinder.js, mxgraph/src/shape/mxLine.js, mxgraph/src/shape/mxSwimlane.js, mxgraph/src/shape/mxPolyline.js, mxgraph/src/util/mxSvgCanvas2D.js and js/grapheditor/Shapes.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! The shapes mxGraph registers itself (mxCellRenderer.js 130-145), and its edge line.

use super::{
    Direction, Fill, LINE_ARCSIZE, Margins, Part, RECTANGLE_ROUNDING_FACTOR, add_points, polygon,
    polyline, quarter, rect,
};
use crate::geom::{PathCmd, Point, Rect};
use crate::style::Resolved;

/// `mxConstants.DEFAULT_STARTSIZE`: a swimlane's title bar when the style gives no `startSize`.
const DEFAULT_STARTSIZE: f64 = 40.0;

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

/// `doubleEllipse`: an ellipse with a second one inside it, `margin` in, by default a little
/// more than the stroke (`mxDoubleEllipse`, mxDoubleEllipse.js 83-107).
pub(super) fn double_ellipse(b: Rect, style: &Resolved) -> Vec<Part> {
    let m = double_ellipse_margin(b, style);
    let mut parts = vec![Part::body(ellipse(b))];
    let inner = Rect::new(b.x + m, b.y + m, b.w - 2.0 * m, b.h - 2.0 * m);
    if inner.w > 0.0 && inner.h > 0.0 {
        parts.push(Part::line(ellipse(inner)));
    }
    parts
}

/// The label of a `doubleEllipse` keeps inside the inner one (mxDoubleEllipse.js 118-124).
pub(super) fn double_ellipse_label(rect: Rect, style: &Resolved) -> Rect {
    let m = double_ellipse_margin(rect, style);
    Rect::new(rect.x + m, rect.y + m, rect.w - 2.0 * m, rect.h - 2.0 * m)
}

fn double_ellipse_margin(b: Rect, style: &Resolved) -> f64 {
    let fallback = (3.0 + style.num("strokeWidth", 1.0)).min((b.w / 5.0).min(b.h / 5.0));
    style.num("margin", fallback)
}

/// `rhombus`: a diamond through the middles of the sides (`mxRhombus.paintVertexShape`,
/// mxRhombus.js 64-75).
// ponytail: draw.io's `double=1` second diamond inside (Shapes.js 2371-2420) is not drawn.
pub(super) fn rhombus(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let pts = [(w / 2.0, 0.0), (w, h / 2.0), (w / 2.0, h), (0.0, h / 2.0)];
    vec![Part::body(polygon(b, style, &pts, &[]))]
}

/// `triangle`: pointing east from its left side (`mxTriangle.redrawPath`, mxTriangle.js 51-55).
pub(super) fn triangle(b: Rect, style: &Resolved) -> Vec<Part> {
    let pts = [(0.0, 0.0), (b.w, b.h / 2.0), (0.0, b.h)];
    vec![Part::body(polygon(b, style, &pts, &[]))]
}

/// `cloud` (`mxCloud.redrawPath`, mxCloud.js 51-61).
pub(super) fn cloud(b: Rect) -> Vec<Part> {
    let p = |x: f64, y: f64| Point::new(b.x + x * b.w, b.y + y * b.h);
    let curve = |c1: (f64, f64), c2: (f64, f64), to: (f64, f64)| {
        PathCmd::CurveTo(p(c1.0, c1.1), p(c2.0, c2.1), p(to.0, to.1))
    };
    vec![Part::body(vec![
        PathCmd::MoveTo(p(0.25, 0.25)),
        curve((0.05, 0.25), (0.0, 0.5), (0.16, 0.55)),
        curve((0.0, 0.66), (0.18, 0.9), (0.31, 0.8)),
        curve((0.4, 1.0), (0.7, 1.0), (0.8, 0.8)),
        curve((1.0, 0.8), (1.0, 0.6), (0.875, 0.5)),
        curve((1.0, 0.3), (0.8, 0.1), (0.625, 0.2)),
        curve((0.5, 0.05), (0.3, 0.05), (0.25, 0.25)),
        PathCmd::Close,
    ])]
}

/// `actor`: a bust, a head on shoulders (`mxActor.redrawPath`, mxActor.js 78-88).
pub(super) fn actor(b: Rect) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let width = w / 3.0;
    let neck = 2.0 * h / 5.0;
    vec![Part::body(vec![
        PathCmd::MoveTo(p(0.0, h)),
        PathCmd::CurveTo(p(0.0, 3.0 * h / 5.0), p(0.0, neck), p(w / 2.0, neck)),
        PathCmd::CurveTo(
            p(w / 2.0 - width, neck),
            p(w / 2.0 - width, 0.0),
            p(w / 2.0, 0.0),
        ),
        PathCmd::CurveTo(
            p(w / 2.0 + width, 0.0),
            p(w / 2.0 + width, neck),
            p(w / 2.0, neck),
        ),
        PathCmd::CurveTo(p(w, neck), p(w, 3.0 * h / 5.0), p(w, h)),
        PathCmd::Close,
    ])]
}

/// `cylinder`, mxGraph's own can: a body whose top and bottom bulge by a fifth of the height (at
/// most 40), or by `size` of it, and the front of the top stroked over it (`mxCylinder`,
/// mxCylinder.js 75-139, with draw.io's `size`, Shapes.js 1463-1475).
pub(super) fn cylinder(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let dy = cylinder_size(h, style);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![
        Part::body(vec![
            PathCmd::MoveTo(p(0.0, dy)),
            PathCmd::CurveTo(p(0.0, -dy / 3.0), p(w, -dy / 3.0), p(w, dy)),
            PathCmd::LineTo(p(w, h - dy)),
            PathCmd::CurveTo(p(w, h + dy / 3.0), p(0.0, h + dy / 3.0), p(0.0, h - dy)),
            PathCmd::Close,
        ]),
        Part::line(vec![
            PathCmd::MoveTo(p(0.0, dy)),
            PathCmd::CurveTo(p(0.0, 2.0 * dy), p(w, 2.0 * dy), p(w, dy)),
        ]),
    ]
}

fn cylinder_size(h: f64, style: &Resolved) -> f64 {
    match style.get("size").and_then(crate::style::parse_num) {
        Some(size) => h * size.clamp(0.0, 1.0),
        None => CYLINDER_MAX_HEIGHT.min((h / 5.0).round()),
    }
}

/// `mxCylinder.prototype.maxHeight`.
const CYLINDER_MAX_HEIGHT: f64 = 40.0;

/// With `boundedLbl=1` a `cylinder`'s label starts below its top (Shapes.js 1477-1487).
pub(super) fn cylinder_margins(rect: Rect, style: &Resolved) -> Margins {
    let size = style.num("size", 0.15) * 2.0;
    Margins {
        top: CYLINDER_MAX_HEIGHT.min(rect.h * size),
        ..Margins::default()
    }
}

/// `line`: a stroke across the middle (`mxLine.paintVertexShape`, mxLine.js 71-90).
pub(super) fn line(b: Rect) -> Vec<Part> {
    let mid = b.y + b.h / 2.0;
    vec![Part::line(polyline(&[
        Point::new(b.x, mid),
        Point::new(b.right(), mid),
    ]))]
}

/// `swimlane`: a title bar `startSize` high (wide with `horizontal=0`) over a body, which is
/// filled with `swimlaneFillColor` if at all and otherwise takes clicks only on its stroke, and
/// the divider between them (`mxSwimlane.paintVertexShape`, `paintSwimlane`,
/// `paintRoundedSwimlane` and `paintDivider`, mxSwimlane.js 247-633).
// ponytail: `footerSize`, `separatorColor`, the title's image and `glass` are not drawn.
pub(super) fn swimlane(b: Rect, style: &Resolved) -> Vec<Part> {
    let start = style.num("startSize", DEFAULT_STARTSIZE).max(0.0);
    if start == 0.0 && !style.flag("fixedHeader", true) {
        return Vec::new();
    }
    let (w, h) = (b.w, b.h);
    let horizontal = style.flag("horizontal", true);
    let start = start.min(if horizontal { h } else { w });
    let r = match style.flag("rounded", false) {
        true => swimlane_arc(w, h, start, style)
            .min(start)
            .min(if horizontal { h } else { w } - start),
        false => 0.0,
    };
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let (head, body) = match (horizontal, r > 0.0) {
        (true, false) => (
            polyline(&[p(0.0, start), p(0.0, 0.0), p(w, 0.0), p(w, start)]),
            polyline(&[p(0.0, start), p(0.0, h), p(w, h), p(w, start)]),
        ),
        (false, false) => (
            polyline(&[p(start, 0.0), p(0.0, 0.0), p(0.0, h), p(start, h)]),
            polyline(&[p(start, 0.0), p(w, 0.0), p(w, h), p(start, h)]),
        ),
        (true, true) => {
            let rw = (w / 2.0).min(r);
            (
                vec![
                    PathCmd::MoveTo(p(w, start)),
                    PathCmd::LineTo(p(w, r)),
                    PathCmd::QuadTo(p(w, 0.0), p(w - rw, 0.0)),
                    PathCmd::LineTo(p(rw, 0.0)),
                    PathCmd::QuadTo(p(0.0, 0.0), p(0.0, r)),
                    PathCmd::LineTo(p(0.0, start)),
                ],
                vec![
                    PathCmd::MoveTo(p(0.0, start)),
                    PathCmd::LineTo(p(0.0, h - r)),
                    PathCmd::QuadTo(p(0.0, h), p(rw, h)),
                    PathCmd::LineTo(p(w - rw, h)),
                    PathCmd::QuadTo(p(w, h), p(w, h - r)),
                    PathCmd::LineTo(p(w, start)),
                ],
            )
        }
        (false, true) => {
            let rh = (h / 2.0).min(r);
            (
                vec![
                    PathCmd::MoveTo(p(start, 0.0)),
                    PathCmd::LineTo(p(r, 0.0)),
                    PathCmd::QuadTo(p(0.0, 0.0), p(0.0, rh)),
                    PathCmd::LineTo(p(0.0, h - rh)),
                    PathCmd::QuadTo(p(0.0, h), p(r, h)),
                    PathCmd::LineTo(p(start, h)),
                ],
                vec![
                    PathCmd::MoveTo(p(start, h)),
                    PathCmd::LineTo(p(w - r, h)),
                    PathCmd::QuadTo(p(w, h), p(w, h - rh)),
                    PathCmd::LineTo(p(w, rh)),
                    PathCmd::QuadTo(p(w, 0.0), p(w - r, 0.0)),
                    PathCmd::LineTo(p(start, 0.0)),
                ],
            )
        }
    };
    let mut parts = vec![Part {
        path: head,
        fill: Fill::Cell,
        stroke: style.flag("swimlaneHead", true),
    }];
    if start < if horizontal { h } else { w } {
        let lane = style.color("swimlaneFillColor");
        let stroke = style.flag("swimlaneBody", true);
        let fill = lane.map_or(Fill::None, Fill::Own);
        if stroke || lane.is_some() {
            parts.push(Part {
                path: body,
                fill,
                stroke,
            });
        }
    }
    if style.flag("swimlaneLine", true) && start != 0.0 {
        let divider = match horizontal {
            true => [p(0.0, start), p(w, start)],
            false => [p(start, 0.0), p(start, h)],
        };
        parts.push(Part::line(polyline(&divider)));
    }
    parts
}

/// A rounded swimlane's corner radius (`mxSwimlane.getSwimlaneArcSize`, mxSwimlane.js 177-191).
fn swimlane_arc(w: f64, h: f64, start: f64, style: &Resolved) -> f64 {
    match style.flag("absoluteArcSize", false) {
        true => (w / 2.0)
            .min(h / 2.0)
            .min(style.num("arcSize", LINE_ARCSIZE) / 2.0),
        false => start * style.num("arcSize", RECTANGLE_ROUNDING_FACTOR * 100.0) / 100.0 * 3.0,
    }
}

/// A swimlane's label sits in its title bar, at the end the flips and direction put it
/// (`mxSwimlane.getLabelBounds`, mxSwimlane.js 110-158).
pub(super) fn swimlane_label(rect: Rect, style: &Resolved) -> Rect {
    let start = style.num("startSize", DEFAULT_STARTSIZE).max(0.0);
    if start == 0.0 && !style.flag("fixedHeader", true) {
        return rect;
    }
    let (flip_h, flip_v) = (style.flag("flipH", false), style.flag("flipV", false));
    let direction = Direction::of(style);
    let turned = matches!(direction, Direction::South | Direction::West);
    let across = style.flag("horizontal", true) != direction.vertical();
    let far = (!across && flip_h != turned) || (across && flip_v != turned);
    let mut b = rect;
    if !direction.vertical() {
        let bar = b.h.min(start);
        if far {
            b.y += b.h - bar;
        }
        b.h = bar;
    } else {
        let bar = b.w.min(start);
        if far {
            b.x += b.w - bar;
        }
        b.w = bar;
    }
    b
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

    /// The points a path's lines and moves end on, curves left out.
    fn corners(path: &[PathCmd]) -> Vec<Point> {
        path.iter()
            .filter_map(|c| match c {
                PathCmd::MoveTo(p) | PathCmd::LineTo(p) => Some(*p),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn polygons_run_through_their_corners_and_round_them_on_request() {
        let r = Rect::new(0.0, 0.0, 80.0, 40.0);
        let diamond = &vertex("rhombus", r, &style("rhombus;", false))[0].path;
        let p = Point::new;
        assert_eq!(
            corners(diamond),
            [p(40.0, 0.0), p(80.0, 20.0), p(40.0, 40.0), p(0.0, 20.0)]
        );
        let triangle = &vertex("triangle", r, &style("triangle;", false))[0].path;
        assert_eq!(
            corners(triangle),
            [p(0.0, 0.0), p(80.0, 20.0), p(0.0, 40.0)]
        );
        let rounded = &vertex("rhombus", r, &style("rhombus;rounded=1;", false))[0].path;
        let quads = rounded.iter().filter(|c| matches!(c, PathCmd::QuadTo(..)));
        assert_eq!(quads.count(), 4);
    }

    #[test]
    fn a_double_ellipse_rings_its_label() {
        let parts = vertex(
            "doubleEllipse",
            BOX,
            &style("ellipse;shape=doubleEllipse;", false),
        );
        assert_eq!(parts.len(), 2);
        // 3 + the stroke's 1 in, less than a fifth of the height.
        let inner = path_bounds(&parts[1].path).unwrap();
        assert!(
            same_box(inner, Rect::new(14.0, 24.0, 92.0, 32.0)),
            "{inner:?}"
        );
        let s = style("shape=doubleEllipse;", false);
        assert_eq!(double_ellipse_label(BOX, &s), inner);
    }

    #[test]
    fn a_cylinder_bulges_a_fifth_of_its_height_or_its_size() {
        let parts = vertex("cylinder", BOX, &style("shape=cylinder;", false));
        assert!(same_box(path_bounds(&parts[0].path).unwrap(), BOX));
        // The front's curve dips three quarters of the way to its controls, `dy` further down.
        let front = path_bounds(&parts[1].path).unwrap();
        assert!(
            (front.bottom() - (BOX.y + 8.0 * 1.75)).abs() < 1e-9,
            "{front:?}"
        );
        let tall = vertex("cylinder", BOX, &style("shape=cylinder;size=0.5;", false));
        let front = path_bounds(&tall[1].path).unwrap();
        assert!(
            (front.bottom() - (BOX.y + 20.0 * 1.75)).abs() < 1e-9,
            "{front:?}"
        );
    }

    #[test]
    fn a_swimlane_has_a_title_a_body_and_a_divider() {
        let r = Rect::new(0.0, 0.0, 200.0, 100.0);
        let parts = swimlane(r, &style("swimlane;", false));
        assert_eq!(parts.len(), 3);
        let p = Point::new;
        assert_eq!(
            corners(&parts[0].path),
            [p(0.0, 23.0), p(0.0, 0.0), p(200.0, 0.0), p(200.0, 23.0)]
        );
        assert!(parts[0].fill == Fill::Cell && parts[0].stroke);
        assert_eq!(parts[1].fill, Fill::None, "an empty lane takes no fill");
        assert_eq!(corners(&parts[2].path), [p(0.0, 23.0), p(200.0, 23.0)]);
        let lane = swimlane(r, &style("swimlane;swimlaneFillColor=#ff0000;", false));
        assert!(matches!(lane[1].fill, Fill::Own(_)));
        let side = swimlane(r, &style("swimlane;horizontal=0;startSize=30;", false));
        assert_eq!(corners(&side[2].path), [p(30.0, 0.0), p(30.0, 100.0)]);
        // The label takes the title bar, at the bottom once flipped.
        let s = style("swimlane;", false);
        assert_eq!(swimlane_label(r, &s), Rect::new(0.0, 0.0, 200.0, 23.0));
        let s = style("swimlane;flipV=1;", false);
        assert_eq!(swimlane_label(r, &s), Rect::new(0.0, 77.0, 200.0, 23.0));
    }
}
