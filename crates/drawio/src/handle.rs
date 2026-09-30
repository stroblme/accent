// The handles here are derived from draw.io mxgraph/src/handler/mxEdgeHandler.js,
// mxEdgeSegmentHandler.js and mxElbowEdgeHandler.js, and js/grapheditor/Graph.js (Apache-2.0,
// Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for
// accent; see crates/drawio/NOTICE.
//! draw.io's handles between the ends of a selected edge: which ones its style gives it, where
//! they sit on its route, and the waypoints a drag of one leaves it with. All points are absolute
//! page units, `px` being a screen pixel's worth; the ends are [`crate::edit::set_end`]'s.

use crate::geom::{Point, Rect, distance_to_segment};
use crate::style::Resolved;

/// Which handles an edge carries between its ends (`mxGraph.createEdgeHandler`, and Graph.js
/// 18563 for a loop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// One on each segment, moving it across (`mxEdgeSegmentHandler`): orthogonal and segment
    /// edges, and a loop.
    Segments,
    /// One, the elbow's (`mxElbowEdgeHandler`): elbow, side-to-side, top-to-bottom and loop
    /// styles; `vertical` when it moves up and down.
    Elbow { vertical: bool },
    /// One on each waypoint (`mxEdgeHandler`), none for an entity relation; with `virtual_bends`,
    /// one halfway along each segment that adds a waypoint.
    Bends { inner: bool, virtual_bends: bool },
}

/// The handles `style` gives an edge; `is_loop` when both ends are on one shape.
pub fn kind(style: &Resolved, is_loop: bool) -> Kind {
    if is_loop {
        return Kind::Segments;
    }
    let edge_style = match style.flag("noEdgeStyle", false) {
        true => None,
        false => style.get("edgeStyle"),
    };
    match edge_style {
        Some("orthogonalEdgeStyle" | "segmentEdgeStyle") => Kind::Segments,
        Some("elbowEdgeStyle") => Kind::Elbow {
            vertical: style.get("elbow") == Some("vertical"),
        },
        Some("topToBottomEdgeStyle") => Kind::Elbow { vertical: true },
        Some("sideToSideEdgeStyle" | "loopEdgeStyle" | "isometricEdgeStyle") => {
            Kind::Elbow { vertical: false }
        }
        Some("entityRelationEdgeStyle") => Kind::Bends {
            inner: false,
            virtual_bends: false,
        },
        None | Some("none") => Kind::Bends {
            inner: true,
            virtual_bends: style.shape() != "arrow",
        },
        Some(_) => Kind::Bends {
            inner: true,
            virtual_bends: false,
        },
    }
}

/// A handle between the ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Knob {
    /// Segment `index` of [`segments`], moved across: sideways when `vertical`.
    Segment { index: usize, vertical: bool },
    /// The elbow.
    Elbow,
    /// Waypoint `index`.
    Bend(usize),
    /// Halfway along segment `index` of the route: a waypoint dragged out of it at `index`.
    Virtual(usize),
}

/// A shape an end is on: its box, and whether the end is pinned to one of its connection
/// points rather than floating on its outline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Terminal {
    pub rect: Rect,
    pub pinned: bool,
}

/// The handles of an edge of `kind` routed along `route`, each where it sits and whether it
/// is faded: a virtual bend, and a handle of an edge with no waypoints of its own (draw.io's
/// `virtualBendOpacity`).
pub fn knobs(kind: Kind, route: &[Point], has_waypoints: bool) -> Vec<(Knob, Point, bool)> {
    if route.len() < 2 {
        return Vec::new();
    }
    let mid = |a: Point, b: Point| Point::new(a.x + (b.x - a.x) / 2.0, a.y + (b.y - a.y) / 2.0);
    match kind {
        // mxEdgeSegmentHandler.createBends, redrawInnerBends
        Kind::Segments => {
            let mut pts = segments(route);
            let straight = pts.len() == 4 && pts[1].distance(pts[2]).round() == 0.0;
            // A straight edge's middle handle goes to the middle of its line.
            if straight {
                let (first, last) = (pts[0], pts[3]);
                if (first.y - last.y).round() == 0.0 {
                    let cx = first.x + (last.x - first.x) / 2.0;
                    pts[1].x = cx;
                    pts[2].x = cx;
                } else {
                    let cy = first.y + (last.y - first.y) / 2.0;
                    pts[1].y = cy;
                    pts[2].y = cy;
                }
            }
            (0..pts.len() - 1)
                .map(|i| {
                    let mut vertical = (pts[i].x - pts[i + 1].x).round() == 0.0;
                    // A segment of no length takes its way from the next.
                    if (pts[i].y - pts[i + 1].y).round() == 0.0 && i + 2 < pts.len() {
                        vertical = (pts[i].x - pts[i + 2].x).round() == 0.0;
                    }
                    let faded = match straight {
                        true => i != 1,
                        false => !has_waypoints,
                    };
                    let knob = Knob::Segment { index: i, vertical };
                    (knob, mid(pts[i], pts[i + 1]), faded)
                })
                .collect()
        }
        // mxElbowEdgeHandler.redrawInnerBends: on the edge, between its inner points.
        Kind::Elbow { .. } => {
            let at = mid(route[1], route[route.len() - 2]);
            vec![(Knob::Elbow, at, !has_waypoints)]
        }
        // mxEdgeHandler.createBends, createVirtualBends
        Kind::Bends {
            inner,
            virtual_bends,
        } => {
            let bends = (1..route.len() - 1)
                .filter(|_| inner)
                .map(|i| (Knob::Bend(i - 1), route[i], false));
            let virtuals = (1..route.len())
                .filter(|_| virtual_bends)
                .map(|i| (Knob::Virtual(i - 1), mid(route[i - 1], route[i]), true));
            bends.chain(virtuals).collect()
        }
    }
}

/// The points a segment handler works on (`mxEdgeSegmentHandler.getCurrentPoints`): the route,
/// a straight one given a middle segment of no length, so it has three handles.
pub fn segments(route: &[Point]) -> Vec<Point> {
    let n = route.len();
    let aligned = |a: Point, b: Point, c: Point, tol: f64| {
        ((a.x - b.x).abs() < tol && (b.x - c.x).abs() < tol)
            || ((a.y - b.y).abs() < tol && (b.y - c.y).abs() < tol)
    };
    if n == 2 || (n == 3 && aligned(route[0], route[1], route[2], 1.0)) {
        let (first, last) = (route[0], route[n - 1]);
        let c = Point::new(
            first.x + (last.x - first.x) / 2.0,
            first.y + (last.y - first.y) / 2.0,
        );
        return vec![first, c, c, last];
    }
    route.to_vec()
}

/// The pointer as an edge handle takes it (`mxEdgeHandler.getPointForEvent`): an axis within
/// 2 px of a shape's middle or of a point of the route (a floating end's excepted) goes onto it,
/// and else onto the grid.
pub fn aim(
    p: Point,
    route: &[Point],
    ends: [Option<Terminal>; 2],
    grid: Option<f64>,
    px: f64,
) -> Point {
    let tolerance = 2.0 * px;
    let (mut at, mut x_set, mut y_set) = (p, false, false);
    let mut snap_to = |q: Point| {
        if (at.x - q.x).abs() < tolerance {
            at.x = q.x;
            x_set = true;
        }
        if (at.y - q.y).abs() < tolerance {
            at.y = q.y;
            y_set = true;
        }
    };
    for t in ends.iter().flatten() {
        snap_to(t.rect.centre());
    }
    let floating = |t: &Option<Terminal>| t.is_some_and(|t| !t.pinned);
    for (i, q) in route.iter().enumerate() {
        let end = (i == 0 && floating(&ends[0])) || (i + 1 == route.len() && floating(&ends[1]));
        if !end {
            snap_to(*q);
        }
    }
    let snap = |v: f64, g: f64| (v / g).round() * g;
    if let Some(g) = grid {
        if !x_set {
            at.x = snap(at.x, g);
        }
        if !y_set {
            at.y = snap(at.y, g);
        }
    }
    at
}

/// The waypoints while segment `index` of [`segments`] is dragged to `point`
/// (`mxEdgeSegmentHandler.getPreviewPoints`): the inner points, the dragged segment's two moved
/// across to the pointer. One left inside the shape at either end is the pointer twice.
pub fn segment_points(
    pts: &[Point],
    index: usize,
    point: Point,
    ends: [Option<Terminal>; 2],
) -> Vec<Point> {
    let Some(&first) = pts.first() else {
        return Vec::new();
    };
    let mut last = first;
    let mut result = Vec::new();
    for (i, &p) in pts.iter().enumerate().skip(1) {
        let mut pt = p;
        if i == index + 1 {
            // Within a unit: a fixed end's point and the waypoint lined up with it.
            if (last.x - pt.x).abs() < 1.0 {
                last.x = point.x;
                pt.x = point.x;
            }
            if (last.y - pt.y).abs() < 1.0 {
                last.y = point.y;
                pt.y = point.y;
            }
            // The segment's start moved with it.
            if let Some(start) = result.last_mut() {
                *start = last;
            }
        }
        if i + 1 < pts.len() {
            result.push(pt);
        }
        last = pt;
    }
    if let [only] = result.as_slice()
        && ends.iter().flatten().any(|t| t.rect.contains(*only))
    {
        result = vec![point, point];
    }
    result
}

/// The waypoints a segment drag leaves (`mxEdgeSegmentHandler.updatePreviewState`): the corners
/// of `routed`, the edge as routed through [`segment_points`], a straight run's inner points
/// merged away. A straight edge keeps the pointer twice, and one turned from straight up and
/// down to routed keeps its ends' heights (`before` being its route when the drag began).
pub fn merged_points(
    routed: &[Point],
    point: Point,
    before: &[Point],
    ends: [Option<Terminal>; 2],
    px: f64,
) -> Vec<Point> {
    let same = |a: f64, b: f64| ((a - b) / px).round() == 0.0;
    let mut result = Vec::new();
    for w in routed.windows(3) {
        let (p0, p1, p2) = (w[0], w[1], w[2]);
        // Merges adjacent segments only if more than 2 to allow for straight edges.
        if (!same(p0.x, p1.x) || !same(p1.x, p2.x)) && (!same(p0.y, p1.y) || !same(p1.y, p2.y)) {
            result.push(p1);
        }
    }
    let (Some(&first), Some(&last)) = (routed.first(), routed.last()) else {
        return result;
    };
    if result.is_empty() && (same(first.x, last.x) || same(first.y, last.y)) {
        return vec![point, point];
    }
    if let ([Some(source), Some(target)], 5, 2, Some(b0), Some(be)) = (
        ends,
        routed.len(),
        result.len(),
        before.first(),
        before.last(),
    ) && same(b0.x, be.x)
    {
        // A pinned end keeps its connection point's height, a floating one its shape's middle.
        let y0 = if source.pinned {
            first.y
        } else {
            source.rect.centre().y
        };
        let ye = if target.pinned {
            last.y
        } else {
            target.rect.centre().y
        };
        return vec![Point::new(point.x, y0), Point::new(point.x, ye)];
    }
    result
}

/// The waypoints while a bend is dragged to `point` (`mxEdgeHandler.getPreviewPoints`): waypoint
/// `Knob::Bend` moved, or one inserted at `Knob::Virtual`. A bend dropped on another handle
/// (`handles`, within `reach`) goes, and so does one that straightens its segment within
/// `tolerance` of the line between its neighbours, the route's ends taken at their shapes'
/// middles where they float.
#[allow(clippy::too_many_arguments)]
pub fn bend_points(
    waypoints: &[Point],
    route: &[Point],
    knob: Knob,
    point: Point,
    handles: &[Point],
    ends: [Option<Terminal>; 2],
    reach: f64,
    tolerance: f64,
) -> Vec<Point> {
    let mut points = waypoints.to_vec();
    if points.is_empty() {
        return vec![point];
    }
    let (index, inserted) = match knob {
        Knob::Virtual(i) => {
            let i = i.min(points.len());
            points.insert(i, point);
            (i, true)
        }
        Knob::Bend(i) | Knob::Segment { index: i, .. } => (i.min(points.len() - 1), false),
        Knob::Elbow => (0, false),
    };
    // Dropped on another handle: gone.
    let on = |q: &Point| (q.x - point.x).abs() <= reach && (q.y - point.y).abs() <= reach;
    if handles.iter().any(on) {
        points.remove(index);
        return points;
    }
    if !inserted {
        let mut abs = route.to_vec();
        if index + 1 < abs.len() {
            abs[index + 1] = point;
        }
        let floating = |t: &Option<Terminal>| t.filter(|t| !t.pinned).map(|t| t.rect.centre());
        if let (Some(c), Some(p)) = (floating(&ends[0]), abs.first_mut()) {
            *p = c;
        }
        if let (Some(c), Some(p)) = (floating(&ends[1]), abs.last_mut()) {
            *p = c;
        }
        let k = index + 1;
        if k + 1 < abs.len() && distance_to_segment(point, abs[k - 1], abs[k + 1]) < tolerance {
            points.remove(index);
            return points;
        }
        points[index] = point;
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn the_style_picks_the_handles() {
        let of = |s: &str| kind(&Style::parse(s).resolve(true), false);
        assert_eq!(of("edgeStyle=orthogonalEdgeStyle;"), Kind::Segments);
        assert_eq!(
            of("edgeStyle=elbowEdgeStyle;elbow=vertical;"),
            Kind::Elbow { vertical: true }
        );
        let straight = Kind::Bends {
            inner: true,
            virtual_bends: true,
        };
        assert_eq!(of(""), straight);
        assert_eq!(of("edgeStyle=orthogonalEdgeStyle;noEdgeStyle=1;"), straight);
        let loop_style = kind(&Style::parse("").resolve(true), true);
        assert_eq!(loop_style, Kind::Segments);
    }

    #[test]
    fn a_straight_edge_has_three_segment_handles_the_middle_one_full() {
        let knobs = knobs(Kind::Segments, &[p(0.0, 0.0), p(100.0, 0.0)], false);
        let at: Vec<(Point, bool)> = knobs.iter().map(|k| (k.1, k.2)).collect();
        assert_eq!(
            at,
            [
                (p(25.0, 0.0), true),
                (p(50.0, 0.0), false),
                (p(75.0, 0.0), true)
            ]
        );
    }

    #[test]
    fn a_dragged_segment_moves_across_and_the_corners_are_kept() {
        // An orthogonal edge: right, down, right.
        let route = [p(0.0, 0.0), p(50.0, 0.0), p(50.0, 80.0), p(100.0, 80.0)];
        let pts = segments(&route);
        // The vertical middle segment dragged to x = 70.
        let moved = segment_points(&pts, 1, p(70.0, 40.0), [None, None]);
        assert_eq!(moved, [p(70.0, 0.0), p(70.0, 80.0)]);
        // Routed through those, the corners are the waypoints, the ends not among them.
        let routed = [p(0.0, 0.0), p(70.0, 0.0), p(70.0, 80.0), p(100.0, 80.0)];
        let kept = merged_points(&routed, p(70.0, 40.0), &route, [None, None], 1.0);
        assert_eq!(kept, [p(70.0, 0.0), p(70.0, 80.0)]);
        // Dragged straight: the pointer twice.
        let line = [p(0.0, 0.0), p(70.0, 0.0), p(100.0, 0.0)];
        let kept = merged_points(&line, p(70.0, 0.0), &route, [None, None], 1.0);
        assert_eq!(kept, [p(70.0, 0.0), p(70.0, 0.0)]);
    }

    #[test]
    fn a_bend_is_moved_added_and_removed() {
        let route = [p(0.0, 0.0), p(50.0, 50.0), p(100.0, 0.0)];
        let waypoints = [p(50.0, 50.0)];
        let drag = |knob, at: Point| {
            bend_points(
                &waypoints,
                &route,
                knob,
                at,
                &[route[0]],
                [None, None],
                4.0,
                4.0,
            )
        };
        assert_eq!(drag(Knob::Bend(0), p(50.0, 60.0)), [p(50.0, 60.0)]);
        assert_eq!(
            drag(Knob::Virtual(1), p(80.0, 40.0)),
            [p(50.0, 50.0), p(80.0, 40.0)]
        );
        // Onto the line between its neighbours, or onto the source's handle: gone.
        assert!(drag(Knob::Bend(0), p(50.0, 2.0)).is_empty());
        assert!(drag(Knob::Bend(0), p(1.0, 1.0)).is_empty());
        let elbow = bend_points(
            &[],
            &route,
            Knob::Elbow,
            p(9.0, 9.0),
            &[],
            [None, None],
            4.0,
            4.0,
        );
        assert_eq!(elbow, [p(9.0, 9.0)]);
    }
}
