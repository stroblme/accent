// Derived from draw.io src/main/webapp/mxgraph/src/view/mxEdgeStyle.js, mxGraphView.js, mxGraph.js, src/main/webapp/mxgraph/src/shape/mxShape.js and js/grapheditor/Graph.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Edge routing: from the two ends, the style and the waypoints to the points an edge is drawn
//! through.
//!
//! [`route`] composes the steps as `mxGraphView.updateEdgeState` does, at zoom 1: ends pinned by
//! a connection constraint are placed first, the edge style then turns the waypoints into bends,
//! and the ends still floating are put on their terminals' outlines, facing their neighbours.
//! Functions are named after their JS originals and cite their lines in draw.io 31.4.5, so the
//! port can be compared against later releases.

mod orth;

use crate::geom::{self, Point, Rect};
use crate::model::{Cell, Page};
use crate::perimeter::{Outline, PerimeterKind};
use crate::shapes;
use crate::style::{Resolved, parse_num};
use orth::{directions, orth_connector};

/// A vertex an edge is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Terminal {
    /// Absolute and unrotated.
    pub bounds: Rect,
    /// Degrees, about the centre of `bounds`.
    pub rotation: f64,
    pub outline: Outline,
    /// The terminal's own `perimeterSpacing`.
    pub perimeter_spacing: f64,
    /// `routingCenterX`/`routingCenterY`: how far off the centre the edge styles route from, in
    /// shares of the width and height.
    pub routing: Point,
    /// The sides its `portConstraint` lets an edge leave by, as [`directions`] reads them; `None`
    /// leaves it to the edge.
    pub port_constraint: Option<u32>,
    /// The rotation a side constraint turns with, `portConstraintRotation=1`'s, else 0.
    pub port_rotation: f64,
    /// `legacyAnchorPoints=0`, which draw.io writes on a shape it flips: connection points are
    /// mirrored and turned as the shape is drawn rather than in its legacy order.
    pub anchors_as_drawn: bool,
    /// `anchorPointDirection=0`: connection points do not turn with the `direction`.
    pub anchors_unturned: bool,
}

impl Terminal {
    /// Vertex `cell` of `page` as an edge end, a port on another vertex included; `None` for an
    /// edge, a layer or a cell placed on an edge.
    pub(crate) fn of(page: &Page, cell: &Cell) -> Option<Terminal> {
        let bounds = page
            .absolute_rect(&cell.id)
            .or_else(|| port_rect(page, cell))?;
        let style = cell.style.resolve(false);
        let rotation = style.num("rotation", 0.0);
        Some(Terminal {
            bounds,
            rotation,
            outline: Outline::of(&style),
            perimeter_spacing: style.num("perimeterSpacing", 0.0),
            routing: Point::new(
                style.num("routingCenterX", 0.0),
                style.num("routingCenterY", 0.0),
            ),
            port_constraint: style.get("portConstraint").map(directions),
            port_rotation: match style.flag("portConstraintRotation", false) {
                true => rotation,
                false => 0.0,
            },
            anchors_as_drawn: !style.flag("legacyAnchorPoints", true),
            anchors_unturned: !style.flag("anchorPointDirection", true),
        })
    }

    /// Where the edge styles route from in `bounds`, the terminal's own or a rounded copy: the
    /// centre moved by its `routing` shares.
    // mxGraphView.getRoutingCenterX/Y, mxGraphView.js 1984-2001
    fn routing_centre(&self, bounds: Rect) -> Point {
        let c = bounds.centre();
        Point::new(
            c.x + self.routing.x * bounds.w,
            c.y + self.routing.y * bounds.h,
        )
    }
}

/// A vertex placed relative to the vertex it sits on, as a port is: a share of its parent's size
/// in, then its offset (`mxGraphView.updateVertexState`).
fn port_rect(page: &Page, cell: &Cell) -> Option<Rect> {
    let g = cell
        .geometry
        .as_ref()
        .filter(|g| cell.vertex && g.relative)?;
    let parent = page.absolute_rect(cell.parent.as_deref()?)?;
    let o = g.offset.unwrap_or_default();
    Some(Rect::new(
        parent.x + g.x * parent.w + o.x,
        parent.y + g.y * parent.h + o.y,
        g.width,
        g.height,
    ))
}

/// Everything routing one edge needs, in absolute page coordinates.
#[derive(Debug, Clone, Copy)]
pub struct EdgeInput<'a> {
    /// The edge's resolved style (edge style, constraints, spacings, jetty size, …).
    pub style: &'a Resolved,
    pub source: Option<Terminal>,
    pub target: Option<Terminal>,
    /// The cells `sourcePort` and `targetPort` name, which the edge style and the floating ends
    /// route from in their terminals' place (`mxGraphView.getTerminalPort`).
    pub source_port: Option<Terminal>,
    pub target_port: Option<Terminal>,
    /// Where a dangling end is (the geometry's `sourcePoint`/`targetPoint`).
    pub source_point: Option<Point>,
    pub target_point: Option<Point>,
    /// The geometry's waypoints, which the edge style reads as hints.
    pub waypoints: &'a [Point],
    /// Source and target are the same cell.
    pub is_loop: bool,
    /// The page's grid, a loop's default size.
    pub grid_size: f64,
}

/// The points the edge is drawn through, first end to last. Empty when an end has neither a
/// terminal nor a point of its own, an edge draw.io does not draw either.
// mxGraphView.updateEdgeState, mxGraphView.js 1141-1178
pub fn route(input: &EdgeInput) -> Vec<Point> {
    if (input.source.is_none() && input.source_point.is_none())
        || (input.target.is_none() && input.target_point.is_none())
    {
        return Vec::new();
    }
    let state = State {
        style: input.style,
        p0: fixed_terminal_point(input, true),
        pe: fixed_terminal_point(input, false),
        grid_size: input.grid_size,
        is_loop: input.is_loop && input.source.is_some(),
    };
    let mut pts = update_points(input, &state);
    if pts.len() < 2 {
        return Vec::new();
    }
    update_floating_terminal_points(input, &mut pts);
    match pts.into_iter().collect::<Option<Vec<Point>>>() {
        Some(pts) => get_waypoints(&pts),
        None => Vec::new(),
    }
}

/// What an edge style reads from the edge's cell state.
struct State<'a> {
    style: &'a Resolved,
    /// The fixed ends (`absolutePoints[0]` and the last), `None` while an end floats.
    p0: Option<Point>,
    pe: Option<Point>,
    grid_size: f64,
    /// Both ends on the same vertex (`source == target` in the JS).
    is_loop: bool,
}

/// A connection constraint (`mxConnectionConstraint`): a point relative to the terminal's
/// bounds, an offset in page units, and whether the point is moved onto the outline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Constraint {
    /// In 0..1 of the terminal's bounds, (0, 0) its top-left.
    pub point: Point,
    pub dx: f64,
    pub dy: f64,
    pub perimeter: bool,
}

/// The connection points of vertex `id`, draw.io's snap points: where an edge end can be
/// pinned, each in page coordinates and turned with the shape, with the constraint that pins it
/// there. Empty for an edge, a layer and an unknown id.
///
/// A `points` style lists them; a list that does not parse gives none. Without one they are
/// the shape's own ([`shapes::constraints`]), measured as it faces east.
// Graph.getAllConnectionConstraints, Graph.js 18456-18519
pub fn anchors(page: &Page, id: &str) -> Vec<(Point, Constraint)> {
    let Some(cell) = page.cell(id) else {
        return Vec::new();
    };
    let Some(terminal) = Terminal::of(page, cell) else {
        return Vec::new();
    };
    let style = cell.style.resolve(false);
    let (w, h) = match terminal.outline.direction.vertical() {
        true => (terminal.bounds.h, terminal.bounds.w),
        false => (terminal.bounds.w, terminal.bounds.h),
    };
    let constraints = match style.get("points") {
        Some(list) => points_style(list).unwrap_or_default(),
        None => shapes::constraints(style.shape(), &style, w, h),
    };
    constraints
        .into_iter()
        .map(|c| (connection_point(&terminal, &c), c))
        .collect()
}

/// A `points` style: JSON of `[[x, y, perimeter, dx, dy], …]`, the last three optional. The
/// perimeter is on unless it is `0` (or `"0"`, or `false`), the offsets 0 unless given. `None`
/// for anything else.
// Graph.js 18462-18485
fn points_style(list: &str) -> Option<Vec<Constraint>> {
    let list: String = list.chars().filter(|c| !c.is_whitespace()).collect();
    let items = list.strip_prefix("[[")?.strip_suffix("]]")?;
    let value = |v: &str| parse_num(v.trim_matches('"'));
    let constraint = |item: &str| {
        let v: Vec<&str> = item.split(',').collect();
        let perimeter = v
            .get(2)
            .is_none_or(|&p| !(p == "\"0\"" || p == "false" || parse_num(p) == Some(0.0)));
        let offset = |i: usize| v.get(i).and_then(|&d| value(d)).unwrap_or(0.0);
        Some(Constraint {
            point: Point::new(value(v[0])?, value(v.get(1)?)?),
            dx: offset(3),
            dy: offset(4),
            perimeter,
        })
    };
    items.split("],[").map(constraint).collect()
}

/// The constraint the edge style sets for an end: `exitX`/`exitY`/`exitDx`/`exitDy`/
/// `exitPerimeter` for the source, the `entry` keys for the target. `None` without both x and y.
// mxGraph.getConnectionConstraint, mxGraph.js 7114-7146
fn connection_constraint(style: &Resolved, source: bool) -> Option<Constraint> {
    let [x, y, dx, dy, perimeter] = if source {
        ["exitX", "exitY", "exitDx", "exitDy", "exitPerimeter"]
    } else {
        ["entryX", "entryY", "entryDx", "entryDy", "entryPerimeter"]
    };
    let num = |key| style.get(key).and_then(parse_num);
    Some(Constraint {
        point: Point::new(num(x)?, num(y)?),
        dx: style.num(dx, 0.0),
        dy: style.num(dy, 0.0),
        perimeter: style.flag(perimeter, true),
    })
}

/// Where a constraint pins an end: its point in the terminal's perimeter bounds as the shape
/// faces east, then turned with the shape's direction and moved onto the outline if the
/// constraint says so, or else mirrored with its flips; then turned with the terminal. This is
/// draw.io's default order, which differs from how the shape itself is drawn; with
/// `legacyAnchorPoints=0` the point is mirrored, turned with the direction and then moved onto
/// the outline, as the shape is drawn. `anchorPointDirection=0` leaves the direction out.
// Graph.getLegacyConnectionPoint, Graph.js 27776-27900, and mxGraph.getConnectionPoint,
// mxGraph.js 7334-7449
fn connection_point(t: &Terminal, c: &Constraint) -> Point {
    let mut bounds = perimeter_bounds(t, 0.0);
    let centre = bounds.centre();
    let o = &t.outline;
    let quarter = match t.anchors_unturned {
        true => 0.0,
        false => o.direction.degrees(),
    };
    // The legacy order measures the point on the box as it faces only while it turns with it.
    if o.direction.vertical() && (t.anchors_as_drawn || !t.anchors_unturned) {
        bounds = shapes::rotate90(bounds);
    }
    let mut p = Point::new(
        bounds.x + c.point.x * bounds.w + c.dx,
        bounds.y + c.point.y * bounds.h + c.dy,
    );
    let (flip_h, flip_v) = match o.direction.vertical() {
        true => (o.flip_v, o.flip_h),
        false => (o.flip_h, o.flip_v),
    };
    let mirror = |p: Point| {
        Point::new(
            if flip_h { 2.0 * centre.x - p.x } else { p.x },
            if flip_v { 2.0 * centre.y - p.y } else { p.y },
        )
    };
    let mut turn = t.rotation;
    if t.anchors_as_drawn {
        p = geom::rotate(mirror(p), centre, quarter);
        if c.perimeter {
            p = perimeter_point(t, p, false, 0.0);
        }
    } else if c.perimeter {
        p = perimeter_point(t, geom::rotate(p, centre, quarter), false, 0.0);
    } else {
        turn += quarter;
        p = mirror(p);
    }
    geom::rotate(p, centre, turn)
}

/// An end placed before routing: a constrained end on its terminal, the centre of one with
/// `centerPerimeter`, or a dangling end's point. `None` leaves the end floating until the edge
/// style has run.
// mxGraphView.getFixedTerminalPoint, mxGraphView.js 1343-1368, and draw.io's centerPerimeter,
// Graph.js 16965-16980
fn fixed_terminal_point(input: &EdgeInput, source: bool) -> Option<Point> {
    let (terminal, point) = if source {
        (input.source, input.source_point)
    } else {
        (input.target, input.target_point)
    };
    match terminal {
        Some(t) if t.outline.kind == PerimeterKind::Center => Some(t.bounds.centre()),
        Some(t) => connection_constraint(input.style, source).map(|c| connection_point(&t, &c)),
        None => point,
    }
}

/// The terminal's bounds grown by `border` and by its own `perimeterSpacing`.
// mxGraphView.getPerimeterBounds, mxGraphView.js 1824-1834
fn perimeter_bounds(t: &Terminal, border: f64) -> Rect {
    // ponytail: a fixed-aspect stencil does not shrink the bounds (mxCellState.getPerimeterBounds).
    t.bounds.grow(border + t.perimeter_spacing)
}

/// The point on the terminal's outline on the way to `next`, or its centre if it has no size.
// mxGraphView.getPerimeterPoint, mxGraphView.js 1688-1753
fn perimeter_point(t: &Terminal, next: Point, orthogonal: bool, border: f64) -> Point {
    let bounds = perimeter_bounds(t, border);
    if bounds.w > 0.0 || bounds.h > 0.0 {
        t.outline.point(bounds, next, orthogonal)
    } else {
        t.bounds.centre()
    }
}

/// The edge styles mxGraph registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeStyle {
    EntityRelation,
    Loop,
    Elbow,
    SideToSide,
    TopToBottom,
    Orth,
    Segment,
}

impl EdgeStyle {
    /// The style registered under `name` (mxStyleRegistry.js 62-68).
    fn named(name: &str) -> Option<EdgeStyle> {
        Some(match name {
            "elbowEdgeStyle" => EdgeStyle::Elbow,
            "entityRelationEdgeStyle" => EdgeStyle::EntityRelation,
            "loopEdgeStyle" => EdgeStyle::Loop,
            "sideToSideEdgeStyle" => EdgeStyle::SideToSide,
            "topToBottomEdgeStyle" => EdgeStyle::TopToBottom,
            "orthogonalEdgeStyle" => EdgeStyle::Orth,
            "segmentEdgeStyle" => EdgeStyle::Segment,
            // ponytail: draw.io's `isometricEdgeStyle` is not ported and routes as no style.
            _ => return None,
        })
    }
}

/// `edgeStyle`, unless `noEdgeStyle` is set.
fn named_edge_style(style: &Resolved) -> Option<EdgeStyle> {
    if style.flag("noEdgeStyle", false) {
        return None;
    }
    EdgeStyle::named(style.get("edgeStyle")?)
}

/// The edge style that routes `input`: `loop` (the Loop style by default) for a loop, else
/// `edgeStyle`. `None` draws the edge straight through its waypoints.
// mxGraphView.getEdgeStyle, mxGraphView.js 1516-1542
fn get_edge_style(input: &EdgeInput) -> Option<EdgeStyle> {
    if is_loop_style_enabled(input) {
        return EdgeStyle::named(input.style.get("loop").unwrap_or("loopEdgeStyle"));
    }
    named_edge_style(input.style)
}

/// A loop takes the loop style unless it has two waypoints of its own, or is an
/// `orthogonalLoop` with a constrained end.
// mxGraphView.isLoopStyleEnabled, mxGraphView.js 1496-1509
fn is_loop_style_enabled(input: &EdgeInput) -> bool {
    let style = input.style;
    let constrained = connection_constraint(style, true).is_some()
        || connection_constraint(style, false).is_some();
    input.waypoints.len() < 2
        && (!style.flag("orthogonalLoop", false) || !constrained)
        && input.is_loop
        && input.source.is_some()
}

/// Whether floating ends are projected straight across onto the outline: the `orthogonal` key,
/// else whether the edge style draws horizontal and vertical segments only.
// mxGraph.isOrthogonal, mxGraph.js 8832-8845. Its getEdgeStyle is asked without the ends, so a
// loop counts by its `edgeStyle` too.
fn is_orthogonal(style: &Resolved) -> bool {
    if style.get("orthogonal").is_some() {
        return style.flag("orthogonal", false);
    }
    matches!(
        named_edge_style(style),
        Some(
            EdgeStyle::Segment
                | EdgeStyle::Elbow
                | EdgeStyle::SideToSide
                | EdgeStyle::TopToBottom
                | EdgeStyle::EntityRelation
                | EdgeStyle::Orth
        )
    )
}

/// The edge's points with the bends of its style between the fixed ends, `None` for an end still
/// floating.
// mxGraphView.updatePoints, mxGraphView.js 1414-1467
fn update_points(input: &EdgeInput, state: &State) -> Vec<Option<Point>> {
    let mut pts = vec![state.p0];
    // ponytail: fixed-aspect stencil bounds (updateBoundsFromStencil) are not routed around.
    let (src, trg, points) = (
        input.source_port.as_ref().or(input.source.as_ref()),
        input.target_port.as_ref().or(input.target.as_ref()),
        input.waypoints,
    );
    match get_edge_style(input) {
        Some(EdgeStyle::Loop) => loop_style(state, src, points, &mut pts),
        Some(EdgeStyle::Elbow) => elbow_connector(state, src, trg, points, &mut pts),
        Some(EdgeStyle::SideToSide) => elbow_segment(state, src, trg, points, &mut pts, false),
        Some(EdgeStyle::TopToBottom) => elbow_segment(state, src, trg, points, &mut pts, true),
        Some(EdgeStyle::Segment) => segment_connector(state, src, trg, points, &mut pts),
        Some(EdgeStyle::Orth) => orth_connector(state, src, trg, points, &mut pts),
        // ponytail: EntityRelation is not ported; such an edge keeps its waypoints as they are.
        Some(EdgeStyle::EntityRelation) | None => pts.extend(points.iter().copied().map(Some)),
    }
    pts.push(state.pe);
    pts
}

/// Puts the floating ends on their outlines, the target first, so that the source end of an edge
/// without bends faces the target's end rather than its centre.
// mxGraphView.updateFloatingTerminalPoints, mxGraphView.js 1556-1575
fn update_floating_terminal_points(input: &EdgeInput, pts: &mut [Option<Point>]) {
    let last = pts.len() - 1;
    if pts[last].is_none()
        && let Some(target) = &input.target
    {
        let start = input.target_port.as_ref().unwrap_or(target);
        let p = floating_terminal_point(input, start, input.source.as_ref(), false, pts);
        pts[last] = Some(p);
    }
    if pts[0].is_none()
        && let Some(source) = &input.source
    {
        let start = input.source_port.as_ref().unwrap_or(source);
        let p = floating_terminal_point(input, start, input.target.as_ref(), true, pts);
        pts[0] = Some(p);
    }
}

/// A floating end: on the outline of `start`, the terminal or its port, facing the next point,
/// the edge's perimeter spacing kept free. A turned terminal is met on its turned outline.
// mxGraphView.getFloatingTerminalPoint, mxGraphView.js 1608-1638
fn floating_terminal_point(
    input: &EdgeInput,
    start: &Terminal,
    end: Option<&Terminal>,
    source: bool,
    pts: &[Option<Point>],
) -> Point {
    // ponytail: draw.io's `snapToPoint` (Graph.js) does not snap the end to a connection point.
    let centre = start.bounds.centre();
    let Some(next) = next_point(pts, end, source) else {
        return centre;
    };
    let next = geom::rotate(next, centre, -start.rotation);
    let spacing = if source {
        "sourcePerimeterSpacing"
    } else {
        "targetPerimeterSpacing"
    };
    let border = input.style.num("perimeterSpacing", 0.0) + input.style.num(spacing, 0.0);
    let orthogonal = start.rotation == 0.0 && is_orthogonal(input.style);
    let pt = perimeter_point(start, next, orthogonal, border);
    geom::rotate(pt, centre, start.rotation)
}

/// The point an end faces: its neighbour on the edge, else the opposite terminal's centre.
// mxGraphView.getNextPoint, mxGraphView.js 1879-1896
fn next_point(pts: &[Option<Point>], opposite: Option<&Terminal>, source: bool) -> Option<Point> {
    let point = if source { pts[1] } else { pts[pts.len() - 2] };
    point.or_else(|| opposite.map(|t| t.bounds.centre()))
}

/// The points without those less than a unit away from the point before them. The JS drops a
/// last end that close too; here the end stays, so the line and its arrow reach the terminal,
/// and the point before it goes instead.
// mxShape.getWaypoints, mxShape.js 984-1016
fn get_waypoints(pts: &[Point]) -> Vec<Point> {
    let far = |a: Point, b: Point| (a.x - b.x).abs() >= 1.0 || (a.y - b.y).abs() >= 1.0;
    let mut result = vec![pts[0]];
    for w in pts.windows(2) {
        if far(w[0], w[1]) {
            result.push(w[1]);
        }
    }
    let end = pts[pts.len() - 1];
    if !far(pts[pts.len() - 2], end) {
        if result.len() > 1 && !far(result[result.len() - 1], end) {
            result.pop();
        }
        result.push(end);
    }
    result
}

/// JavaScript's `Math.round`: halves round up, towards +∞.
fn js_round(v: f64) -> f64 {
    let floor = v.floor();
    if v - floor >= 0.5 { floor + 1.0 } else { floor }
}

/// `Math.round(v * 10) / 10`: the tenth OrthConnector and SegmentConnector snap their input and
/// their bends to (`scalePointArray`/`scaleCellState`, mxEdgeStyle.js 997-1054, at zoom 1).
fn round_tenth(v: f64) -> f64 {
    js_round(v * 10.0) / 10.0
}

fn round_point(p: Point) -> Point {
    Point::new(round_tenth(p.x), round_tenth(p.y))
}

fn round_rect(r: Rect) -> Rect {
    Rect::new(
        round_tenth(r.x),
        round_tenth(r.y),
        round_tenth(r.w),
        round_tenth(r.h),
    )
}

/// A self-loop: out of the source and back in, `segment` (by default the grid size) away from
/// it on the side `direction` names (the right for the default `west`), or through the first
/// waypoint.
// mxEdgeStyle.Loop, mxEdgeStyle.js 228-328
fn loop_style(
    state: &State,
    source: Option<&Terminal>,
    points: &[Point],
    result: &mut Vec<Option<Point>>,
) {
    if state.p0.is_some() && state.pe.is_some() {
        result.extend(points.iter().copied().map(Some));
        return;
    }
    let Some(source) = source else { return };
    let s = source.bounds;
    let centre = source.routing_centre(s);
    let pt = points.first().copied().filter(|p| !s.contains(*p));
    let seg = state.style.num("segment", state.grid_size);
    let dir = state.style.get("direction").unwrap_or("west");
    let (mut x, mut dx, mut y, mut dy) = (0.0, 0.0, 0.0, 0.0);
    if dir == "north" || dir == "south" {
        x = centre.x;
        dx = seg;
    } else {
        y = centre.y;
        dy = seg;
    }
    match pt {
        Some(p) if p.x >= s.x && p.x <= s.right() => {
            x = centre.x;
            dx = (x - p.x).abs().max(dy);
            y = p.y;
            dy = 0.0;
        }
        Some(p) => {
            x = p.x;
            dy = (y - p.y).abs().max(dy);
        }
        None => match dir {
            "north" => y = s.y - 2.0 * dx,
            "south" => y = s.bottom() + 2.0 * dx,
            "east" => x = s.x - 2.0 * dy,
            _ => x = s.right() + 2.0 * dy,
        },
    }
    result.push(Some(Point::new(x - dx, y - dy)));
    result.push(Some(Point::new(x + dx, y + dy)));
}

/// SideToSide, or TopToBottom for `elbow=vertical` and when the terminals leave no gap
/// between them across.
// mxEdgeStyle.ElbowConnector, mxEdgeStyle.js 338-390
fn elbow_connector(
    state: &State,
    source: Option<&Terminal>,
    target: Option<&Terminal>,
    points: &[Point],
    result: &mut Vec<Option<Point>>,
) {
    let (mut vertical, mut horizontal) = (false, false);
    if let (Some(s), Some(t)) = (source, target) {
        let (s, t) = (s.bounds, t.bounds);
        if let Some(pt) = points.first() {
            let (left, right) = (s.x.min(t.x), s.right().max(t.right()));
            let (top, bottom) = (s.y.min(t.y), s.bottom().max(t.bottom()));
            vertical = pt.y < top || pt.y > bottom;
            horizontal = pt.x < left || pt.x > right;
        } else {
            let (left, right) = (s.x.max(t.x), s.right().min(t.right()));
            vertical = left == right;
            if !vertical {
                let (top, bottom) = (s.y.max(t.y), s.bottom().min(t.bottom()));
                horizontal = top == bottom;
            }
        }
    }
    let vertical = !horizontal && (vertical || state.style.get("elbow") == Some("vertical"));
    elbow_segment(state, source, target, points, result, vertical);
}

/// The boxes the elbow styles route between, each with the point it is routed from: a fixed
/// end is a point.
fn elbow_ends(
    state: &State,
    source: Option<&Terminal>,
    target: Option<&Terminal>,
) -> Option<((Rect, Point), (Rect, Point))> {
    let end = |fixed: Option<Point>, t: Option<&Terminal>| match fixed {
        Some(p) => Some((Rect::new(p.x, p.y, 0.0, 0.0), p)),
        None => t.map(|t| (t.bounds, t.routing_centre(t.bounds))),
    };
    Some((end(state.p0, source)?, end(state.pe, target)?))
}

/// SideToSide: a vertical segment halfway between the terminals or at the waypoint's x, met
/// at the height of each terminal's centre, or of the waypoint where it is within the
/// terminal's height. With `vertical`, TopToBottom: the same with x and y swapped.
// mxEdgeStyle.SideToSide and TopToBottom, mxEdgeStyle.js 398-576
fn elbow_segment(
    state: &State,
    source: Option<&Terminal>,
    target: Option<&Terminal>,
    points: &[Point],
    result: &mut Vec<Option<Point>>,
    vertical: bool,
) {
    // Worked out as SideToSide; TopToBottom's input and output have x and y swapped.
    let swap = |p: Point| if vertical { Point::new(p.y, p.x) } else { p };
    let swap_rect = |r: Rect| {
        if vertical {
            Rect::new(r.y, r.x, r.h, r.w)
        } else {
            r
        }
    };
    let pt = points.first().copied().map(swap);
    let Some(((s, sc), (t, tc))) = elbow_ends(state, source, target) else {
        return;
    };
    let (s, t) = (swap_rect(s), swap_rect(t));
    let (sc, tc) = (swap(sc), swap(tc));
    let l = s.x.max(t.x);
    let r = s.right().min(t.right());
    let x = match pt {
        Some(p) => p.x,
        None => js_round(r + (l - r) / 2.0),
    };
    let meet = |b: Rect, centre: Point| match pt {
        Some(p) if p.y >= b.y && p.y <= b.bottom() => p.y,
        _ => centre.y,
    };
    let outside = |y: f64| !t.contains(Point::new(x, y)) && !s.contains(Point::new(x, y));
    for y in [meet(s, sc), meet(t, tc)] {
        if outside(y) {
            result.push(Some(swap(Point::new(x, y))));
        }
    }
    if result.len() == 1 {
        let y = match pt {
            Some(p) if outside(p.y) => p.y,
            Some(_) => return,
            None => {
                let top = s.y.max(t.y);
                let bottom = s.bottom().min(t.bottom());
                top + (bottom - top) / 2.0
            }
        };
        result.push(Some(swap(Point::new(x, y))));
    }
}

/// Horizontal and vertical segments in turn through the waypoints (the hints), the first
/// direction read from how the first and last hint line up with the ends.
// mxEdgeStyle.SegmentConnector, mxEdgeStyle.js 591-892
fn segment_connector(
    state: &State,
    source_scaled: Option<&Terminal>,
    target_scaled: Option<&Terminal>,
    control_hints: &[Point],
    result: &mut Vec<Option<Point>>,
) {
    let tol = 1.0;
    // `pts[0]` and `pts[lastInx]` of the JS.
    let start = state.p0.map(round_point);
    let end = state.pe.map(round_point);
    let source = source_scaled.map(|t| round_rect(t.bounds));
    let target = target_scaled.map(|t| round_rect(t.bounds));
    let routing = |t: Option<&Terminal>, b: Option<Rect>| Some(t?.routing_centre(b?));
    let mut temp_points: Vec<Point> = Vec::new();
    // Whether the first segment outgoing from the source end is horizontal
    let mut horizontal = true;
    // Adds the first point
    let Some(mut pt) = start.or(routing(source_scaled, source)) else {
        return;
    };
    // Without hints the last point lines up with the first along a horizontal.
    let mut hint = pt;
    // The end the last bend snaps to; the JS declares it among the hints only.
    let mut pe = None;

    // Adds the waypoints
    if !control_hints.is_empty() {
        let mut hints = control_hints.to_vec();
        // Aligns source and target hint to fixed points
        let first = &mut hints[0];
        if (first.x - pt.x).abs() < tol {
            first.x = pt.x;
        }
        if (first.y - pt.y).abs() < tol {
            first.y = pt.y;
        }
        pe = end;
        if let (Some(e), Some(last)) = (pe, hints.last_mut()) {
            if (last.x - e.x).abs() < tol {
                last.x = e.x;
            }
            if (last.y - e.y).abs() < tol {
                last.y = e.y;
            }
        }
        hint = hints[0];

        let mut current_term = if start.is_some() { None } else { source };
        let mut current_pt = start;
        let mut current_hint = hint;
        // Check for alignment with fixed points and with channels at source and target segments
        // only. (The JS also tests a fixed end against the channels here, which cannot hold: the
        // channels are only looked at for a floating end.)
        for i in 0..2 {
            let fixed_vert_align = current_pt.is_some_and(|p| p.x == current_hint.x);
            let fixed_hoz_align = current_pt.is_some_and(|p| p.y == current_hint.y);
            let in_hoz_chan =
                current_term.is_some_and(|t| current_hint.y >= t.y && current_hint.y <= t.bottom());
            let in_vert_chan =
                current_term.is_some_and(|t| current_hint.x >= t.x && current_hint.x <= t.right());
            let hoz_chan = fixed_hoz_align || (current_pt.is_none() && in_hoz_chan);
            let vert_chan = fixed_vert_align || (current_pt.is_none() && in_vert_chan);
            // A hint in both channels of a floating end, or on a fixed end, tells nothing at the
            // source: the orientation is worked out from the target end.
            let undecided = (hoz_chan && vert_chan) || (fixed_vert_align && fixed_hoz_align);
            if !(i == 0 && undecided) && (vert_chan || hoz_chan) {
                horizontal = hoz_chan;
                if i == 1 {
                    // Work back from target end
                    horizontal = if hints.len().is_multiple_of(2) {
                        hoz_chan
                    } else {
                        vert_chan
                    };
                }
                break;
            }
            current_term = if end.is_some() { None } else { target };
            current_pt = end;
            if let Some(&last) = hints.last() {
                current_hint = last;
            }
            if fixed_vert_align && fixed_hoz_align {
                hints.remove(0);
            }
        }

        let leaves_source = if horizontal {
            start.is_some_and(|p| p.y != hint.y)
                || (start.is_none() && source.is_some_and(|s| hint.y < s.y || hint.y > s.bottom()))
        } else {
            start.is_some_and(|p| p.x != hint.x)
                || (start.is_none() && source.is_some_and(|s| hint.x < s.x || hint.x > s.right()))
        };
        if leaves_source {
            temp_points.push(if horizontal {
                Point::new(pt.x, hint.y)
            } else {
                Point::new(hint.x, pt.y)
            });
        }
        if horizontal {
            pt.y = hint.y;
        } else {
            pt.x = hint.x;
        }
        for &h in &hints {
            horizontal = !horizontal;
            hint = h;
            if horizontal {
                pt.y = h.y;
            } else {
                pt.x = h.x;
            }
            temp_points.push(pt);
        }
    }

    // Adds the last point
    if let Some(pt) = end.or(routing(target_scaled, target)) {
        let enters_target = if horizontal {
            end.is_some_and(|e| e.y != hint.y)
                || (end.is_none() && target.is_some_and(|t| hint.y < t.y || hint.y > t.bottom()))
        } else {
            end.is_some_and(|e| e.x != hint.x)
                || (end.is_none() && target.is_some_and(|t| hint.x < t.x || hint.x > t.right()))
        };
        if enters_target {
            temp_points.push(if horizontal {
                Point::new(pt.x, hint.y)
            } else {
                Point::new(hint.x, pt.y)
            });
        }
    }

    // Keeps bends inside the shape for self-loops with innerLoopWaypoints
    if !(state.is_loop && state.style.num("innerLoopWaypoints", 0.0) == 1.0) {
        // Removes bends inside the source terminal
        if start.is_none()
            && let Some(s) = source
        {
            while temp_points.first().is_some_and(|p| s.contains(*p)) {
                temp_points.remove(0);
            }
        }
        // Removes bends inside the target terminal
        if end.is_none()
            && let Some(t) = target
        {
            while temp_points.last().is_some_and(|p| t.contains(*p)) {
                temp_points.pop();
            }
        }
    }

    // Scales and smoothens edges, adding a bend only if it is a unit away from the last
    let mut last_pushed = result.first().copied().flatten();
    for p in temp_points {
        let p = round_point(p);
        if last_pushed.is_none_or(|l| (l.x - p.x).abs() >= tol || (l.y - p.y).abs() >= 1.0) {
            result.push(Some(p));
            last_pushed = Some(p);
        }
    }

    // Removes last point if inside tolerance with end point
    if let (Some(e), Some(Some(l))) = (pe, result.last().copied())
        && (e.x - l.x).abs() <= tol
        && (e.y - l.y).abs() <= tol
    {
        result.pop();
        // Lines up second last point in result with end point
        if let Some(Some(l)) = result.last_mut() {
            if (l.x - e.x).abs() < tol {
                l.x = e.x;
            }
            if (l.y - e.y).abs() < tol {
                l.y = e.y;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::orth::{EAST, NORTH, SOUTH, port_constraints};
    use super::*;
    use crate::model::Cell;
    use crate::style::Style;
    use crate::style::presets::{EdgeKind, constrained, edge};

    fn rect(x: f64, y: f64) -> Terminal {
        Terminal {
            bounds: Rect::new(x, y, 80.0, 40.0),
            ..Terminal::default()
        }
    }

    fn input<'a>(style: &'a Resolved, source: Terminal, target: Terminal) -> EdgeInput<'a> {
        EdgeInput {
            style,
            source: Some(source),
            target: Some(target),
            source_port: None,
            target_port: None,
            source_point: None,
            target_point: None,
            waypoints: &[],
            is_loop: false,
            grid_size: 10.0,
        }
    }

    fn orthogonal() -> Resolved {
        Style::parse(&edge(EdgeKind::Orthogonal, true)).resolve(true)
    }

    #[track_caller]
    fn assert_points(actual: &[Point], expected: &[(f64, f64)]) {
        let close = actual.len() == expected.len()
            && actual
                .iter()
                .zip(expected)
                .all(|(a, &(x, y))| a.distance(Point::new(x, y)) < 1e-9);
        assert!(close, "{actual:?} is not {expected:?}");
    }

    #[test]
    fn horizontal_pair_routes_straight_across() {
        let style = orthogonal();
        let pts = route(&input(&style, rect(0.0, 0.0), rect(200.0, 0.0)));
        assert_points(&pts, &[(80.0, 20.0), (200.0, 20.0)]);
    }

    #[test]
    fn offset_pair_routes_with_one_jog() {
        // Heights still overlapping: out to the right, across halfway, in from the left.
        let style = orthogonal();
        let pts = route(&input(&style, rect(0.0, 0.0), rect(200.0, 30.0)));
        assert_points(
            &pts,
            &[(80.0, 20.0), (140.0, 20.0), (140.0, 50.0), (200.0, 50.0)],
        );
    }

    #[test]
    fn diagonal_pair_routes_with_one_corner() {
        // Apart both ways, draw.io prefers the two-segment route into the target's top.
        let style = orthogonal();
        let pts = route(&input(&style, rect(0.0, 0.0), rect(200.0, 100.0)));
        assert_points(&pts, &[(80.0, 20.0), (240.0, 20.0), (240.0, 100.0)]);
    }

    #[test]
    fn exit_constraint_pins_the_side() {
        let style = Style::parse(&format!(
            "{}exitX=0.5;exitY=1;exitDx=0;exitDy=0;",
            edge(EdgeKind::Orthogonal, true)
        ))
        .resolve(true);
        let pts = route(&input(&style, rect(0.0, 0.0), rect(200.0, 100.0)));
        assert_points(
            &pts,
            &[(40.0, 40.0), (40.0, 70.0), (240.0, 70.0), (240.0, 100.0)],
        );
    }

    #[test]
    fn waypoints_are_honoured() {
        let style = orthogonal();
        let hint = [Point::new(140.0, 60.0)];
        let pts = route(&EdgeInput {
            waypoints: &hint,
            ..input(&style, rect(0.0, 0.0), rect(200.0, 100.0))
        });
        assert_points(
            &pts,
            &[
                (40.0, 40.0),
                (40.0, 60.0),
                (140.0, 60.0),
                (140.0, 120.0),
                (200.0, 120.0),
            ],
        );
    }

    #[test]
    fn side_to_side_crosses_halfway_or_at_the_waypoint() {
        let style = Style::parse("edgeStyle=sideToSideEdgeStyle;").resolve(true);
        let pts = route(&input(&style, rect(0.0, 0.0), rect(200.0, 100.0)));
        assert_points(
            &pts,
            &[(80.0, 20.0), (140.0, 20.0), (140.0, 120.0), (200.0, 120.0)],
        );
        let hint = [Point::new(150.0, 30.0)];
        let pts = route(&EdgeInput {
            waypoints: &hint,
            ..input(&style, rect(0.0, 0.0), rect(200.0, 100.0))
        });
        assert_points(
            &pts,
            &[(80.0, 30.0), (150.0, 30.0), (150.0, 120.0), (200.0, 120.0)],
        );
    }

    #[test]
    fn top_to_bottom_crosses_halfway_or_at_the_waypoint() {
        let style = Style::parse("edgeStyle=topToBottomEdgeStyle;").resolve(true);
        let pts = route(&input(&style, rect(0.0, 0.0), rect(200.0, 100.0)));
        assert_points(
            &pts,
            &[(40.0, 40.0), (40.0, 70.0), (240.0, 70.0), (240.0, 100.0)],
        );
        let hint = [Point::new(60.0, 80.0)];
        let pts = route(&EdgeInput {
            waypoints: &hint,
            ..input(&style, rect(0.0, 0.0), rect(200.0, 100.0))
        });
        assert_points(
            &pts,
            &[(60.0, 40.0), (60.0, 80.0), (240.0, 80.0), (240.0, 100.0)],
        );
    }

    #[test]
    fn a_port_constraint_picks_the_side_an_end_leaves_by() {
        let style = orthogonal();
        let north = Terminal {
            port_constraint: Some(NORTH),
            ..rect(0.0, 0.0)
        };
        let pts = route(&input(&style, north, rect(200.0, 100.0)));
        // Up out of the top, its jetty (`auto`, 20 without a start arrow) above it.
        assert_points(&pts[..2], &[(40.0, 0.0), (40.0, -20.0)]);
        // Turned with a shape that is turned a quarter, north is east.
        let turned = Terminal {
            port_rotation: 90.0,
            ..north
        };
        assert_eq!(port_constraints(&turned, &style, true), EAST);
        // The edge's own constraint for the end, where the terminal has none.
        let style = Style::parse("sourcePortConstraint=south;").resolve(true);
        assert_eq!(port_constraints(&rect(0.0, 0.0), &style, true), SOUTH);
    }

    #[test]
    fn the_elbow_styles_route_from_the_routing_centre() {
        let style = Style::parse("edgeStyle=sideToSideEdgeStyle;").resolve(true);
        let high = Terminal {
            routing: Point::new(0.0, -0.5),
            ..rect(0.0, 0.0)
        };
        let pts = route(&input(&style, high, rect(200.0, 100.0)));
        assert_points(
            &pts,
            &[(80.0, 0.0), (140.0, 0.0), (140.0, 120.0), (200.0, 120.0)],
        );
    }

    #[test]
    fn an_edge_to_a_port_leaves_from_the_port() {
        let mut page = shape("");
        let mut port = Cell::new_vertex("p", "a", Rect::new(1.0, 0.5, 20.0, 20.0), "", "");
        let g = port.geometry.as_mut().unwrap();
        g.relative = true;
        g.offset = Some(Point::new(-10.0, -10.0));
        page.cells.push(port);
        let r = Rect::new(200.0, 0.0, 80.0, 40.0);
        page.cells.push(Cell::new_vertex("b", "1", r, "", ""));
        let ends = |id| (Some(id), Point::default());
        page.cells.push(Cell::new_edge(
            "e",
            "1",
            ends("a"),
            ends("b"),
            "sourcePort=p;",
        ));
        let scene = crate::scene(&page);
        // Out of the port's right side, centred on the right of `a`.
        assert_points(&scene.route("e").unwrap()[..1], &[(90.0, 20.0)]);
    }

    #[test]
    fn edge_style_none_is_straight_through_waypoints() {
        let style = Style::parse("edgeStyle=none;").resolve(true);
        let bend = [Point::new(140.0, 100.0)];
        let pts = route(&EdgeInput {
            waypoints: &bend,
            ..input(&style, rect(0.0, 0.0), rect(200.0, 0.0))
        });
        // Each end on the line from its centre to the bend.
        assert_points(&pts, &[(65.0, 40.0), (140.0, 100.0), (215.0, 40.0)]);
    }

    #[test]
    fn dangling_end_uses_the_terminal_point() {
        let style = Style::parse("").resolve(true);
        let pts = route(&EdgeInput {
            target: None,
            target_point: Some(Point::new(300.0, 20.0)),
            ..input(&style, rect(0.0, 0.0), rect(0.0, 0.0))
        });
        assert_points(&pts, &[(80.0, 20.0), (300.0, 20.0)]);
        let pts = route(&EdgeInput {
            target: None,
            ..input(&style, rect(0.0, 0.0), rect(0.0, 0.0))
        });
        assert!(pts.is_empty(), "an end nowhere is not drawn: {pts:?}");
    }

    #[test]
    fn loop_routes_outside_the_source() {
        let style = orthogonal();
        let pts = route(&EdgeInput {
            is_loop: true,
            ..input(&style, rect(0.0, 0.0), rect(0.0, 0.0))
        });
        // Out of the right side, a grid size beyond it and back.
        assert_points(
            &pts,
            &[(80.0, 10.0), (100.0, 10.0), (100.0, 30.0), (80.0, 30.0)],
        );
    }

    #[test]
    fn perimeter_spacing_moves_the_end_out() {
        let style = Style::parse("perimeterSpacing=5;").resolve(true);
        let target = Terminal {
            perimeter_spacing: 3.0,
            ..rect(200.0, 0.0)
        };
        let pts = route(&input(&style, rect(0.0, 0.0), target));
        assert_points(&pts, &[(85.0, 20.0), (192.0, 20.0)]);
    }

    #[test]
    fn rotated_terminal_attaches_on_its_rotated_outline() {
        // Turned a quarter, the 80×40 source stands 40 wide and 80 tall about (40, 20).
        let style = Style::parse("").resolve(true);
        let source = Terminal {
            rotation: 90.0,
            ..rect(0.0, 0.0)
        };
        let pts = route(&input(&style, source, rect(200.0, 0.0)));
        assert_points(&pts, &[(60.0, 20.0), (200.0, 20.0)]);
    }

    #[test]
    fn a_constraint_that_is_not_a_finite_number_leaves_the_end_floating() {
        let style = Style::parse("exitX=1e999;exitY=0.5;exitPerimeter=0;").resolve(true);
        let pts = route(&input(&style, rect(0.0, 0.0), rect(200.0, 0.0)));
        assert_points(&pts, &[(80.0, 20.0), (200.0, 20.0)]);
    }

    #[test]
    fn a_flat_ellipse_is_met_at_its_centre() {
        let style = Style::parse("").resolve(true);
        let flat = Terminal {
            bounds: Rect::new(0.0, 20.0, 80.0, 0.0),
            outline: Outline {
                kind: PerimeterKind::Ellipse,
                ..Outline::default()
            },
            ..Terminal::default()
        };
        let bend = [Point::new(140.0, 20.0)];
        let pts = route(&EdgeInput {
            waypoints: &bend,
            ..input(&style, flat, rect(200.0, 0.0))
        });
        assert_points(&pts, &[(40.0, 20.0), (140.0, 20.0), (200.0, 20.0)]);
    }

    /// A page holding vertex `a` in `style`, 80 × 40 at the origin like [`rect`].
    fn shape(style: &str) -> Page {
        let mut page = Page::blank("P", "p");
        let r = Rect::new(0.0, 0.0, 80.0, 40.0);
        page.cells.push(Cell::new_vertex("a", "1", r, style, ""));
        page
    }

    /// The anchor of `a` whose constraint is at (`x`, `y`) of its bounds.
    fn anchor(page: &Page, x: f64, y: f64) -> (Point, Constraint) {
        let all = anchors(page, "a");
        let at = all.into_iter().find(|(_, c)| c.point == Point::new(x, y));
        at.expect("an anchor there")
    }

    #[test]
    fn a_rectangle_has_draw_ios_sixteen_anchors() {
        let page = shape("");
        assert_eq!(anchors(&page, "a").len(), 16);
        assert_points(&[anchor(&page, 1.0, 0.5).0], &[(80.0, 20.0)]);
        assert!(anchors(&page, "1").is_empty() && anchors(&page, "nope").is_empty());
    }

    #[test]
    fn an_ellipses_corner_anchors_lie_on_it() {
        let page = shape("ellipse;");
        assert_eq!(anchors(&page, "a").len(), 8);
        for (x, y) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            let p = anchor(&page, x, y).0;
            let (u, v) = ((p.x - 40.0) / 40.0, (p.y - 20.0) / 20.0);
            assert!((u * u + v * v - 1.0).abs() < 1e-9, "{p:?}");
        }
    }

    #[test]
    fn a_points_style_replaces_the_shapes_anchors() {
        let page = shape("points=[[0,0.5],[1, 0.5, 0, 5, -5]];");
        let all = anchors(&page, "a");
        assert_eq!(all.len(), 2);
        assert_points(&[all[0].0, all[1].0], &[(0.0, 20.0), (85.0, 15.0)]);
        assert!(all[0].1.perimeter && !all[1].1.perimeter);
        for broken in ["points=[[0,0.5],[1", "points=[];"] {
            assert!(anchors(&shape(broken), "a").is_empty(), "{broken}");
        }
    }

    #[test]
    fn a_turned_shapes_anchors_turn_with_it() {
        let page = shape("rotation=90;");
        assert_points(&[anchor(&page, 1.0, 0.5).0], &[(40.0, 60.0)]);
    }

    #[test]
    fn a_shapes_own_anchors_face_mirror_and_meet_its_outline_with_it() {
        let diamond = shape("rhombus;");
        assert_eq!(anchors(&diamond, "a").len(), 8);
        assert_points(&[anchor(&diamond, 0.0, 0.0).0], &[(20.0, 10.0)]);
        // A triangle's tip, facing south, is the middle of the bottom.
        let down = shape("triangle;direction=south;");
        assert_points(&[anchor(&down, 1.0, 0.5).0], &[(40.0, 40.0)]);
        // A stick figure's left hand, mirrored.
        let mirrored = shape("shape=umlActor;flipH=1;");
        assert_points(&[anchor(&mirrored, 0.25, 0.1).0], &[(60.0, 4.0)]);
        assert_eq!(anchors(&shape("shape=note;"), "a").len(), 11);
    }

    #[test]
    fn a_flipped_shapes_anchors_follow_it_once_legacy_anchors_are_off() {
        // `a` in `style` pinned at (1, 0.25) of the box it is drawn in.
        let pin = |style: &str, perimeter: bool| {
            let page = shape(style);
            let t = Terminal::of(&page, page.cell("a").unwrap()).unwrap();
            let c = Constraint {
                point: Point::new(1.0, 0.25),
                dx: 0.0,
                dy: 0.0,
                perimeter,
            };
            connection_point(&t, &c)
        };
        // A triangle facing south, its tip at (40, 40), and flipped: the legacy order leaves
        // the flip out of a point on the outline, the drawn one mirrors it with the shape.
        let style = "triangle;direction=south;flipH=1;";
        assert_points(&[pin(style, true)], &[(50.0, 30.0)]);
        let drawn = format!("{style}legacyAnchorPoints=0;");
        assert_points(&[pin(&drawn, true)], &[(30.0, 30.0)]);
        // Unturned, a fixed point is measured on the box as it stands.
        let unturned = "triangle;direction=south;anchorPointDirection=0;";
        assert_points(&[pin(unturned, false)], &[(80.0, 10.0)]);
    }

    #[test]
    fn an_end_on_a_center_perimeter_is_its_centre() {
        let style = Style::parse("edgeStyle=orthogonalEdgeStyle;").resolve(true);
        let dot = Terminal {
            outline: Outline {
                kind: PerimeterKind::Center,
                ..Outline::default()
            },
            ..rect(200.0, 100.0)
        };
        let pts = route(&input(&style, rect(0.0, 0.0), dot));
        assert_eq!(pts.last(), Some(&Point::new(240.0, 120.0)));
    }

    #[test]
    fn an_edge_pinned_to_an_anchor_leaves_from_it() {
        let (at, c) = anchor(&shape(""), 1.0, 0.5);
        let style = constrained(&edge(EdgeKind::Orthogonal, true), Some(&c), None);
        let style = Style::parse(&style).resolve(true);
        let pts = route(&input(&style, rect(0.0, 0.0), rect(200.0, 100.0)));
        assert_points(&pts[..1], &[(at.x, at.y)]);
    }
}
