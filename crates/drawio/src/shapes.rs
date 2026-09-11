// Derived from draw.io mxgraph/src/shape/mxRectangleShape.js, mxgraph/src/shape/mxEllipse.js, mxgraph/src/shape/mxActor.js, mxgraph/src/shape/mxShape.js, mxgraph/src/shape/mxPolyline.js, mxgraph/src/shape/mxArrowConnector.js, mxgraph/src/util/mxSvgCanvas2D.js, mxgraph/src/util/mxConstants.js, js/grapheditor/Shapes.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Outlines of vertex shapes and edge lines, as path commands in absolute page coordinates,
//! before rotation.

use crate::geom::{PathCmd, Point, Rect};
use crate::style::Resolved;

/// One piece of a shape and how the scene paints it: with the cell's fill, its stroke, or both.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub path: Vec<PathCmd>,
    pub fill: bool,
    pub stroke: bool,
}

impl Part {
    /// Filled and stroked: a shape's body.
    fn body(path: Vec<PathCmd>) -> Part {
        Part {
            path,
            fill: true,
            stroke: true,
        }
    }

    /// Stroked only: a detail drawn over the body, or a shape with no inside.
    fn line(path: Vec<PathCmd>) -> Part {
        Part {
            path,
            fill: false,
            stroke: true,
        }
    }
}

/// `mxConstants.LINE_ARCSIZE`: the corner size of rounded lines, and of rectangles with
/// `absoluteArcSize`, when the style gives no `arcSize`.
const LINE_ARCSIZE: f64 = 20.0;

/// `mxConstants.RECTANGLE_ROUNDING_FACTOR`: a rounded rectangle's corner as a share of its
/// shorter side when the style gives no `arcSize`.
const RECTANGLE_ROUNDING_FACTOR: f64 = 0.15;

/// `mxConstants.ARROW_SIZE`: a flex arrow's head is `ARROW_SIZE / 5 · 3` long by default.
const ARROW_SIZE: f64 = 30.0;

/// Control points this far towards the corner put a cubic's midpoint on the quarter circle.
const KAPPA: f64 = 4.0 / 3.0 * (std::f64::consts::SQRT_2 - 1.0);

/// Whether [`vertex`] draws `shape` as itself. Anything else is drawn by the scene as a
/// stand-in.
pub fn is_known(shape: &str) -> bool {
    matches!(
        shape,
        "label"
            | "rectangle"
            | "ellipse"
            | "text"
            | "image"
            | "note"
            | "cylinder3"
            | "curlyBracket"
            | "connector"
            | "flexArrow"
    )
}

/// The parts of vertex `shape` filling `bounds`, background first. Empty for shapes with no
/// outline of their own (`text`, `image`).
pub fn vertex(shape: &str, bounds: Rect, style: &Resolved) -> Vec<Part> {
    match shape {
        "text" | "image" => Vec::new(),
        "ellipse" => vec![Part::body(ellipse(bounds))],
        "note" => note(bounds, style),
        "cylinder3" => cylinder(bounds, style),
        "curlyBracket" => curly_bracket(bounds, style),
        // `label`, `rectangle`, and the stand-in for every other shape.
        _ => vec![Part::body(rectangle(bounds, style))],
    }
}

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

/// A polyline with each corner rounded by `arc` when `rounded` (`mxShape.addPoints`), closed
/// back to its first point when `close`.
///
/// mxShape.js 1232-1323, without its `exclude` and `initialMove` arguments. Each corner becomes
/// a line stopping `arc` short of it (at most half the segment) and a quad through the corner to
/// `arc` along the next segment.
pub fn add_points(points: &[Point], rounded: bool, arc: f64, close: bool) -> Vec<PathCmd> {
    let Some(&pe) = points.last() else {
        return Vec::new();
    };
    let mut pts = points.to_vec();
    // A virtual waypoint halfway along the closing segment, so the first corner is rounded too.
    if close && rounded {
        let p0 = pts[0];
        let wp = Point::new(pe.x + (p0.x - pe.x) / 2.0, pe.y + (p0.y - pe.y) / 2.0);
        pts.insert(0, wp);
    }
    let n = pts.len();
    let mut pt = pts[0];
    let mut path = vec![PathCmd::MoveTo(pt)];
    let mut i = 1;
    while i < if close { n } else { n - 1 } {
        let mut tmp = pts[i % n];
        let (dx, dy) = (pt.x - tmp.x, pt.y - tmp.y);
        if rounded && (dx != 0.0 || dy != 0.0) {
            let dist = dx.hypot(dy);
            let k = arc.min(dist / 2.0) / dist;
            path.push(PathCmd::LineTo(Point::new(tmp.x + dx * k, tmp.y + dy * k)));
            // The next point not on top of this corner.
            let mut next = pts[(i + 1) % n];
            while i < n - 2 && (next.x - tmp.x).round() == 0.0 && (next.y - tmp.y).round() == 0.0 {
                next = pts[(i + 2) % n];
                i += 1;
            }
            let (dx, dy) = (next.x - tmp.x, next.y - tmp.y);
            let dist = dx.hypot(dy).max(1.0);
            let k = arc.min(dist / 2.0) / dist;
            let end = Point::new(tmp.x + dx * k, tmp.y + dy * k);
            path.push(PathCmd::QuadTo(tmp, end));
            tmp = end;
        } else {
            path.push(PathCmd::LineTo(tmp));
        }
        pt = tmp;
        i += 1;
    }
    path.push(if close {
        PathCmd::Close
    } else {
        PathCmd::LineTo(pe)
    });
    path
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

/// A `flexArrow` edge: a filled, stroked band along `points` with its heads.
///
/// `mxArrowConnector.paintEdgeShape` (mxArrowConnector.js 124-417) with `FlexArrowShape`'s
/// widths (Shapes.js 3990-4018): the band's outbound side is walked first, then its inbound side
/// back, each waypoint's corner on the mitre of the two segments.
// ponytail: `rounded=1` joins (quads at each bend) and `curved=1` (a fine polyline from
// `getCurvePoints`, trimmed for the heads) are drawn straight and mitred. The JS also lowers
// the miter limit to 1.42 on bent arrows and restrokes the heads at 4; `Part` has no miter
// limit, so the scene's own applies.
pub fn flex_arrow(points: &[Point], style: &Resolved, stroke_width: f64) -> Vec<Part> {
    let (Some(&p0), Some(&p1), Some(&pe)) = (points.first(), points.get(1), points.last()) else {
        return Vec::new();
    };
    let edge_width = style.num("width", 10.0) + (stroke_width - 1.0).max(0.0);
    let start_width = edge_width + style.num("startWidth", 20.0) + stroke_width;
    let end_width = edge_width + style.num("endWidth", 20.0) + stroke_width;
    // `none` is gone from a resolved style, so any value left is a head.
    let marker_start = style.get("startArrow").is_some();
    let marker_end = style.get("endArrow").is_some();
    // `arrowSpacing` is `mxConstants.ARROW_SPACING`, 0.
    let spacing = stroke_width / 2.0;
    let start_size = style.num("startSize", ARROW_SIZE / 5.0) * 3.0 + stroke_width;
    let end_size = style.num("endSize", ARROW_SIZE / 5.0) * 3.0 + stroke_width;

    let (dx, dy) = (p1.x - p0.x, p1.y - p0.y);
    let dist = dx.hypot(dy);
    if dist == 0.0 {
        return Vec::new();
    }
    let (mut nx, mut ny) = (dx / dist, dy / dist);
    let (mut nx1, mut ny1) = (nx, ny);
    let (orthx, orthy) = (edge_width * ny, -edge_width * nx);
    let mut path = Vec::new();
    // The inbound side's corners, drawn in reverse once the far end is reached.
    let mut inbound = Vec::new();

    if marker_start {
        let head = arrow_head(p0, (nx, ny), spacing, start_size, edge_width, start_width);
        path.push(PathCmd::MoveTo(head[0]));
        path.extend(head[1..].iter().map(|&p| PathCmd::LineTo(p)));
    } else {
        let out_start = Point::new(
            p0.x + orthx / 2.0 + spacing * nx,
            p0.y + orthy / 2.0 + spacing * ny,
        );
        let in_end = Point::new(
            p0.x - orthx / 2.0 + spacing * nx,
            p0.y - orthy / 2.0 + spacing * ny,
        );
        path.push(PathCmd::MoveTo(in_end));
        path.push(PathCmd::LineTo(out_start));
    }

    for w in points.windows(3) {
        let (dx1, dy1) = (w[2].x - w[1].x, w[2].y - w[1].y);
        let dist1 = dx1.hypot(dy1);
        if dist1 == 0.0 {
            continue;
        }
        (nx1, ny1) = (dx1 / dist1, dy1 / dist1);
        // The cosine of half the bend: how much further out than half the width the mitre lies.
        let angle_factor = ((nx * nx1 + ny * ny1 + 1.0) / 2.0).sqrt().max(0.06);
        let (nx2, ny2) = (nx + nx1, ny + ny1);
        let dist2 = nx2.hypot(ny2);
        if dist2 == 0.0 {
            continue;
        }
        let (nx2, ny2) = (nx2 / dist2, ny2 / dist2);
        let d = edge_width / 2.0 / angle_factor;
        path.push(PathCmd::LineTo(Point::new(
            w[1].x + ny2 * d,
            w[1].y - nx2 * d,
        )));
        inbound.push(Point::new(w[1].x - ny2 * d, w[1].y + nx2 * d));
        (nx, ny) = (nx1, ny1);
    }

    let (orthx, orthy) = (edge_width * ny1, -edge_width * nx1);
    if marker_end {
        let head = arrow_head(pe, (-nx, -ny), spacing, end_size, edge_width, end_width);
        path.extend(head.map(PathCmd::LineTo));
    } else {
        path.push(PathCmd::LineTo(Point::new(
            pe.x - spacing * nx1 + orthx / 2.0,
            pe.y - spacing * ny1 + orthy / 2.0,
        )));
        path.push(PathCmd::LineTo(Point::new(
            pe.x - spacing * nx1 - orthx / 2.0,
            pe.y - spacing * ny1 - orthy / 2.0,
        )));
    }
    path.extend(inbound.into_iter().rev().map(PathCmd::LineTo));
    path.push(PathCmd::Close);
    vec![Part::body(path)]
}

/// A flex arrow's head at `pt`, `n` pointing from `pt` into the band
/// (`mxArrowConnector.paintMarker`, mxArrowConnector.js 424-443): out from the band's side to the
/// head's width, to the tip, and back in to the other side.
fn arrow_head(
    pt: Point,
    (nx, ny): (f64, f64),
    spacing: f64,
    size: f64,
    edge_width: f64,
    arrow_width: f64,
) -> [Point; 5] {
    let (orthx, orthy) = (edge_width * ny / 2.0, -edge_width * nx / 2.0);
    // `orth / widthArrowRatio` in the JS, without dividing by a zero-width band.
    let (wingx, wingy) = (arrow_width * ny / 2.0, -arrow_width * nx / 2.0);
    let (spacex, spacey) = ((spacing + size) * nx, (spacing + size) * ny);
    [
        Point::new(pt.x - orthx + spacex, pt.y - orthy + spacey),
        Point::new(pt.x - wingx + spacex, pt.y - wingy + spacey),
        Point::new(pt.x + spacing * nx, pt.y + spacing * ny),
        Point::new(pt.x + wingx + spacex, pt.y + wingy + spacey),
        Point::new(pt.x + orthx + spacex, pt.y + orthy + spacey),
    ]
}

/// `label` and `rectangle`: `mxRectangleShape.paintBackground` (mxRectangleShape.js 62-88). A
/// relative `arcSize` is a percentage of the shorter side, an absolute one (`absoluteArcSize=1`)
/// twice the corner radius in page units.
fn rectangle(b: Rect, style: &Resolved) -> Vec<PathCmd> {
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

/// A rectangle, clockwise from its top-left corner as SVG draws `<rect>`.
fn rect(b: Rect) -> Vec<PathCmd> {
    vec![
        PathCmd::MoveTo(Point::new(b.x, b.y)),
        PathCmd::LineTo(Point::new(b.right(), b.y)),
        PathCmd::LineTo(Point::new(b.right(), b.bottom())),
        PathCmd::LineTo(Point::new(b.x, b.bottom())),
        PathCmd::Close,
    ]
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

/// A quarter ellipse from `from` to `to` as a cubic, bulging towards `corner` of the box it
/// fills. It stands in for SVG's arcs and `arcTo` (`mxUtils.arcToCurves`) for quarter turns.
fn quarter(from: Point, corner: Point, to: Point) -> PathCmd {
    let pull = |p: Point| {
        Point::new(
            p.x + (corner.x - p.x) * KAPPA,
            p.y + (corner.y - p.y) * KAPPA,
        )
    };
    PathCmd::CurveTo(pull(from), pull(to), to)
}

/// `note`: a sheet with its top-right corner folded down by `size` (`NoteShape`, Shapes.js
/// 758-812): the outline, then the fold stroked over it.
// ponytail: `darkOpacity`, a translucent fill of the fold, is ignored; its default 0 draws none.
fn note(b: Rect, style: &Resolved) -> Vec<Part> {
    let s = style.num("size", 30.0).min(b.h).min(b.w).max(0.0);
    let (w, h) = (b.w, b.h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![
        Part::body(vec![
            PathCmd::MoveTo(p(0.0, 0.0)),
            PathCmd::LineTo(p(w - s, 0.0)),
            PathCmd::LineTo(p(w, s)),
            PathCmd::LineTo(p(w, h)),
            PathCmd::LineTo(p(0.0, h)),
            PathCmd::LineTo(p(0.0, 0.0)),
            PathCmd::Close,
        ]),
        Part::line(vec![
            PathCmd::MoveTo(p(w - s, 0.0)),
            PathCmd::LineTo(p(w - s, s)),
            PathCmd::LineTo(p(w, s)),
        ]),
    ]
}

/// `cylinder3`: a can whose top and bottom are half ellipses `size` high (`CylinderShape3`,
/// Shapes.js 921-982). With `lid` (the default) the body's top is the rim's back half and its
/// front half is stroked over the body; without, the top is open and only the front half shows.
/// Each `arcTo` is a quarter ellipse, drawn with [`quarter`].
fn cylinder(b: Rect, style: &Resolved) -> Vec<Part> {
    let size = style.num("size", 15.0).min(b.h * 0.5).max(0.0);
    if size == 0.0 {
        return vec![Part::body(rect(b))];
    }
    let (w, h) = (b.w, b.h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let lid = style.flag("lid", true);
    let mut body = if lid {
        vec![
            PathCmd::MoveTo(p(0.0, size)),
            quarter(p(0.0, size), p(0.0, 0.0), p(w / 2.0, 0.0)),
            quarter(p(w / 2.0, 0.0), p(w, 0.0), p(w, size)),
        ]
    } else {
        vec![
            PathCmd::MoveTo(p(0.0, 0.0)),
            quarter(p(0.0, 0.0), p(0.0, size), p(w / 2.0, size)),
            quarter(p(w / 2.0, size), p(w, size), p(w, 0.0)),
        ]
    };
    body.extend([
        PathCmd::LineTo(p(w, h - size)),
        quarter(p(w, h - size), p(w, h), p(w / 2.0, h)),
        quarter(p(w / 2.0, h), p(0.0, h), p(0.0, h - size)),
        PathCmd::Close,
    ]);
    let mut parts = vec![Part::body(body)];
    if lid {
        parts.push(Part::line(vec![
            PathCmd::MoveTo(p(w, size)),
            quarter(p(w, size), p(w, 2.0 * size), p(w / 2.0, 2.0 * size)),
            quarter(p(w / 2.0, 2.0 * size), p(0.0, 2.0 * size), p(0.0, size)),
        ]));
    }
    parts
}

/// `curlyBracket`: an open `{` with its tip on the left edge and its spine `size` of the width
/// in (`CurlyBracketShape`, Shapes.js 1638-1658, painted by `mxActor.paintVertexShape`,
/// mxActor.js 64-70). It has no fill.
fn curly_bracket(b: Rect, style: &Resolved) -> Vec<Part> {
    let s = b.w * style.num("size", 0.5).clamp(0.0, 1.0);
    let (w, h) = (b.w, b.h);
    let arc = style.num("arcSize", LINE_ARCSIZE) / 2.0;
    let pts = [
        (w, 0.0),
        (s, 0.0),
        (s, h / 2.0),
        (0.0, h / 2.0),
        (s, h / 2.0),
        (s, h),
        (w, h),
    ]
    .map(|(x, y)| Point::new(b.x + x, b.y + y));
    vec![Part::line(add_points(
        &pts,
        style.flag("rounded", false),
        arc,
        false,
    ))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::path_bounds;
    use crate::style::Style;

    fn style(s: &str, edge: bool) -> Resolved {
        Style::parse(s).resolve(edge)
    }

    fn near(a: Point, b: Point) -> bool {
        a.distance(b) < 1e-9
    }

    fn start(path: &[PathCmd]) -> Point {
        match path[0] {
            PathCmd::MoveTo(p) => p,
            other => panic!("a path starts with a move, not {other:?}"),
        }
    }

    fn same_box(a: Rect, b: Rect) -> bool {
        near(Point::new(a.x, a.y), Point::new(b.x, b.y))
            && near(Point::new(a.w, a.h), Point::new(b.w, b.h))
    }

    const BOX: Rect = Rect::new(10.0, 20.0, 100.0, 40.0);

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
    fn add_points_rounds_each_corner() {
        let square =
            [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)].map(|(x, y)| Point::new(x, y));
        let path = add_points(&square, true, 2.0, true);
        assert!(
            near(start(&path), Point::new(0.0, 5.0)),
            "starts on the closing side"
        );
        let controls: Vec<Point> = path
            .iter()
            .filter_map(|c| match c {
                PathCmd::QuadTo(c, _) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(controls, square.to_vec());
        assert_eq!(path.last(), Some(&PathCmd::Close));
        assert_eq!(
            path[1],
            PathCmd::LineTo(Point::new(0.0, 2.0)),
            "stops arc short"
        );
        let plain = add_points(&square[..3], false, 2.0, false);
        assert_eq!(
            plain,
            vec![
                PathCmd::MoveTo(square[0]),
                PathCmd::LineTo(square[1]),
                PathCmd::LineTo(square[2]),
            ]
        );
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

    #[test]
    fn note_has_a_fold() {
        let parts = vertex(
            "note",
            Rect::new(0.0, 0.0, 100.0, 60.0),
            &style("shape=note;", false),
        );
        assert_eq!(parts.len(), 2);
        assert!(parts[0].fill && parts[0].stroke);
        assert!(!parts[1].fill && parts[1].stroke);
        assert_eq!(
            parts[1].path,
            vec![
                PathCmd::MoveTo(Point::new(70.0, 0.0)),
                PathCmd::LineTo(Point::new(70.0, 30.0)),
                PathCmd::LineTo(Point::new(100.0, 30.0)),
            ]
        );
    }

    #[test]
    fn cylinder_body_spans_the_bounds() {
        let parts = vertex("cylinder3", BOX, &style("shape=cylinder3;size=10;", false));
        assert_eq!(parts.len(), 2);
        assert!(same_box(path_bounds(&parts[0].path).unwrap(), BOX));
        assert!(!parts[1].fill && parts[1].stroke);
        let lid = path_bounds(&parts[1].path).unwrap();
        assert!(
            (lid.bottom() - (BOX.y + 20.0)).abs() < 1e-9,
            "the lid's front reaches 2·size down"
        );
        let open = vertex("cylinder3", BOX, &style("shape=cylinder3;lid=0;", false));
        assert_eq!(open.len(), 1);
    }

    #[test]
    fn curly_bracket_is_unfilled() {
        let parts = vertex("curlyBracket", BOX, &style("shape=curlyBracket;", false));
        assert_eq!(parts.len(), 1);
        assert!(!parts[0].fill && parts[0].stroke);
        assert!(
            same_box(path_bounds(&parts[0].path).unwrap(), BOX),
            "unrounded, the tip touches the left side"
        );
    }

    #[test]
    fn flex_arrow_is_a_closed_band() {
        let pts = [Point::new(0.0, 0.0), Point::new(100.0, 0.0)];
        let parts = flex_arrow(&pts, &style("shape=flexArrow;", true), 1.0);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].fill && parts[0].stroke);
        let path = &parts[0].path;
        assert!(matches!(path[0], PathCmd::MoveTo(_)));
        assert_eq!(path.last(), Some(&PathCmd::Close));
        // Band 10 wide from half a stroke in; the default end head (endArrow=classic) is
        // 10 + 20 + 1 wide with its tip half a stroke short of the end.
        let b = path_bounds(path).unwrap();
        assert!(same_box(b, Rect::new(0.5, -15.5, 99.0, 31.0)), "{b:?}");
        let bent = [
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 100.0),
        ];
        let path = &flex_arrow(&bent, &style("shape=flexArrow;endArrow=none;", true), 1.0)[0].path;
        let on_path = |q: Point| {
            path.iter()
                .any(|c| matches!(c, PathCmd::LineTo(p) if near(*p, q)))
        };
        assert!(
            on_path(Point::new(95.0, 5.0)) && on_path(Point::new(105.0, -5.0)),
            "both sides meet on the mitre: {path:?}"
        );
    }

    #[test]
    fn unknown_shapes_are_not_known() {
        assert!(is_known("note") && is_known("label") && is_known("flexArrow"));
        assert!(!is_known("rhombus") && !is_known("swimlane") && !is_known(""));
        assert_eq!(
            vertex("rhombus", BOX, &style("rhombus;", false)),
            vec![Part::body(rect(BOX))],
            "a stand-in is the plain rectangle"
        );
        assert!(vertex("text", BOX, &style("text;", false)).is_empty());
    }
}
