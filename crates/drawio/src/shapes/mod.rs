// Derived from draw.io mxgraph/src/shape/mxShape.js, mxgraph/src/util/mxSvgCanvas2D.js, mxgraph/src/util/mxConstants.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Outlines of vertex shapes and edge lines, as path commands in absolute page coordinates,
//! before rotation. [`mxgraph`] holds the shapes mxGraph registers itself, [`grapheditor`] those
//! draw.io adds in `Shapes.js`.

mod grapheditor;
mod mxgraph;

pub use grapheditor::flex_arrow;
pub use mxgraph::edge_line;
pub(crate) use mxgraph::ellipse;

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
        "note" => grapheditor::note(bounds, style),
        "cylinder3" => grapheditor::cylinder(bounds, style),
        "curlyBracket" => grapheditor::curly_bracket(bounds, style),
        // `label`, `rectangle`, and the stand-in for every other shape.
        _ => vec![Part::body(mxgraph::rectangle(bounds, style))],
    }
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

/// A rectangle, clockwise from its top-left corner as SVG draws `<rect>`.
pub(crate) fn rect(b: Rect) -> Vec<PathCmd> {
    vec![
        PathCmd::MoveTo(Point::new(b.x, b.y)),
        PathCmd::LineTo(Point::new(b.right(), b.y)),
        PathCmd::LineTo(Point::new(b.right(), b.bottom())),
        PathCmd::LineTo(Point::new(b.x, b.bottom())),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    pub(super) fn style(s: &str, edge: bool) -> Resolved {
        Style::parse(s).resolve(edge)
    }

    pub(super) fn near(a: Point, b: Point) -> bool {
        a.distance(b) < 1e-9
    }

    pub(super) fn start(path: &[PathCmd]) -> Point {
        match path[0] {
            PathCmd::MoveTo(p) => p,
            other => panic!("a path starts with a move, not {other:?}"),
        }
    }

    pub(super) fn same_box(a: Rect, b: Rect) -> bool {
        near(Point::new(a.x, a.y), Point::new(b.x, b.y))
            && near(Point::new(a.w, a.h), Point::new(b.w, b.h))
    }

    pub(super) const BOX: Rect = Rect::new(10.0, 20.0, 100.0, 40.0);

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
