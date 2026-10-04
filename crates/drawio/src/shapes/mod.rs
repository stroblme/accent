// Derived from draw.io mxgraph/src/shape/mxShape.js, mxgraph/src/util/mxSvgCanvas2D.js, mxgraph/src/util/mxConstants.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Outlines of vertex shapes and edge lines, as path commands in absolute page coordinates,
//! before rotation. [`mxgraph`] holds the shapes mxGraph registers itself, [`grapheditor`] those
//! draw.io adds in `Shapes.js`.

mod constraints;
mod grapheditor;
mod mxgraph;

pub use constraints::constraints;
pub use grapheditor::flex_arrow;
pub use mxgraph::edge_line;
pub(crate) use mxgraph::ellipse;

use crate::geom::{self, PathCmd, Point, Rect};
use crate::scene::{Cap, Join};
use crate::style::{Color, Resolved};

/// One piece of a shape and how the scene paints it: its fill, and whether it is stroked.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub path: Vec<PathCmd>,
    pub fill: Fill,
    pub stroke: bool,
    pub pen: Pen,
}

/// How a part's stroke differs from the cell's, where the shape sets the canvas itself.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pen {
    pub cap: Cap,
    pub join: Join,
    /// Stroked dashed in this colour of the shape's own, whatever the cell's stroke, as a
    /// swimlane's `separatorColor`.
    pub dashed: Option<Color>,
}

/// What a [`Part`] is filled with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fill {
    None,
    /// The cell's fill: its `fillColor` or gradient.
    Cell,
    /// A colour of the shape's own at the cell's `fillOpacity`, as a swimlane's body takes its
    /// `swimlaneFillColor`.
    Own(Color),
    /// Shading over the body: black at this opacity, or white where it is negative
    /// (`darkOpacity`).
    Shade(f64),
}

impl Part {
    /// Filled and stroked: a shape's body.
    fn body(path: Vec<PathCmd>) -> Part {
        Part {
            path,
            fill: Fill::Cell,
            stroke: true,
            pen: Pen::default(),
        }
    }

    /// Stroked only: a detail drawn over the body, or a shape with no inside.
    fn line(path: Vec<PathCmd>) -> Part {
        Part {
            path,
            fill: Fill::None,
            stroke: true,
            pen: Pen::default(),
        }
    }

    /// Its stroke drawn with `pen`.
    fn with(self, pen: Pen) -> Part {
        Part { pen, ..self }
    }

    /// Filled only, with `fill`.
    fn filled(path: Vec<PathCmd>, fill: Fill) -> Part {
        Part {
            path,
            fill,
            stroke: false,
            pen: Pen::default(),
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

/// Whether [`vertex`] draws `shape` as itself: mxGraph's registry (mxCellRenderer.js 130-145)
/// and the shapes of draw.io's General palette (Sidebar.js `addGeneralPalette`). Anything else,
/// a stencil above all, is drawn by the scene as a stand-in.
// ponytail: mxGraph's `arrow` and `arrowConnector`, edge shapes older than `flexArrow`, are not
// ported; such an edge is drawn as a plain connector.
pub fn is_known(shape: &str) -> bool {
    matches!(
        shape,
        "label"
            | "rectangle"
            | "rect"
            | "ellipse"
            | "doubleEllipse"
            | "rhombus"
            | "triangle"
            | "hexagon"
            | "cloud"
            | "actor"
            | "cylinder"
            | "line"
            | "swimlane"
            | "text"
            | "image"
            | "connector"
            | "flexArrow"
            | "note"
            | "cylinder3"
            | "curlyBracket"
            | "process"
            | "process2"
            | "parallelogram"
            | "trapezoid"
            | "step"
            | "document"
            | "internalStorage"
            | "cube"
            | "tape"
            | "card"
            | "callout"
            | "wedgeCallout"
            | "umlActor"
            | "or"
            | "xor"
            | "dataStorage"
            | "message"
            | "partialRectangle"
            | "folder"
            | "component"
            | "plus"
            | "startState"
            | "endState"
            | "offPageConnector"
            | "waypoint"
    )
}

/// The parts of vertex `shape` drawn into `bounds` facing east, background first; the scene
/// mirrors and turns them into place ([`Placement`]). Empty for shapes with no outline of their
/// own (`text`, `image`).
pub fn vertex(shape: &str, bounds: Rect, style: &Resolved) -> Vec<Part> {
    use grapheditor as ge;
    let b = bounds;
    match shape {
        "text" | "image" => Vec::new(),
        "ellipse" => vec![Part::body(ellipse(b))],
        "doubleEllipse" => mxgraph::double_ellipse(b, style),
        "rhombus" => mxgraph::rhombus(b, style),
        "triangle" => mxgraph::triangle(b, style),
        "cloud" => mxgraph::cloud(b),
        "actor" => mxgraph::actor(b),
        "cylinder" => mxgraph::cylinder(b, style),
        "line" => mxgraph::line(b),
        "swimlane" => mxgraph::swimlane(b, style),
        "note" => ge::note(b, style),
        "cylinder3" => ge::cylinder(b, style),
        "curlyBracket" => ge::curly_bracket(b, style),
        "process" | "process2" => ge::process(b, style),
        "parallelogram" => ge::parallelogram(b, style),
        "trapezoid" => ge::trapezoid(b, style),
        "step" => ge::step(b, style),
        "hexagon" => ge::hexagon(b, style),
        "document" => ge::document(b, style),
        "internalStorage" => ge::internal_storage(b, style),
        "cube" => ge::cube(b, style),
        "tape" => ge::tape(b, style),
        "card" => ge::card(b, style),
        "callout" => ge::callout(b, style),
        "wedgeCallout" => ge::wedge_callout(b, style),
        "umlActor" => ge::uml_actor(b),
        "or" => ge::or(b),
        "xor" => ge::xor(b),
        "dataStorage" => ge::data_storage(b, style),
        "message" => ge::message(b),
        "partialRectangle" => ge::partial_rectangle(b, style),
        "folder" => ge::folder(b, style),
        "component" => ge::component(b, style),
        "plus" => ge::plus(b, style),
        "startState" => ge::state(b, false),
        "endState" => ge::state(b, true),
        "offPageConnector" => ge::off_page_connector(b, style),
        "waypoint" => ge::waypoint(b, style),
        // `label`, `rectangle`, `rect` (a name draw.io has no shape for, which it draws as its
        // fallback rectangle), and the stand-in for every other shape.
        _ => vec![Part::body(mxgraph::rectangle(b, style))],
    }
}

/// Where a label with `labelPosition` and `verticalLabelPosition` at their centre goes in
/// `rect`, the vertex's box: shapes with a part a label should keep clear of (a callout's tail,
/// a cube's sides under `boundedLbl=1`) give it less. `inverted` is a label laid across the
/// shape's height (`horizontal=0`), whose `rect` has been turned to match.
// mxShape.getLabelBounds, mxShape.js 436-478, and the shapes' getLabelBounds/getLabelMargins
pub fn label_bounds(shape: &str, rect: Rect, style: &Resolved, inverted: bool) -> Rect {
    use grapheditor as ge;
    match shape {
        "swimlane" => return mxgraph::swimlane_label(rect, style),
        "rhombus" if style.flag("double", false) => {
            return mxgraph::double_rhombus_label(rect, style);
        }
        "doubleEllipse" | "startState" | "endState" => {
            return mxgraph::double_ellipse_label(rect, style);
        }
        "process" | "process2" => return ge::process_label(rect, style),
        "tape" => return ge::tape_label(rect, style),
        _ => {}
    }
    let direction = Direction::of(style);
    // The margins are measured on the box as the shape stands.
    let measured = match inverted && !direction.vertical() {
        true => Rect::new(rect.x, rect.y, rect.h, rect.w),
        false => rect,
    };
    let bounded = style.flag("boundedLbl", false);
    let margins = match shape {
        "callout" => ge::callout_margins(style),
        _ if !bounded => return rect,
        "cube" => ge::cube_margins(style),
        "document" => ge::document_margins(measured, style),
        "cylinder" => mxgraph::cylinder_margins(measured, style),
        "cylinder3" => ge::cylinder_margins(measured, style),
        "folder" => ge::folder_margins(measured, style),
        _ => return rect,
    };
    let (mut flip_h, mut flip_v) = (style.flag("flipH", false), style.flag("flipV", false));
    let margins = match inverted {
        // The label's own quarter turn moves each margin to the next side.
        true => {
            std::mem::swap(&mut flip_h, &mut flip_v);
            Margins {
                left: margins.bottom,
                top: margins.left,
                right: margins.top,
                bottom: margins.right,
            }
        }
        false => margins,
    };
    directed_bounds(rect, margins, direction, flip_h, flip_v)
}

/// How far in from each side of a shape its label keeps, as the shape faces east.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Margins {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// `rect` less `m`, the margins of a shape facing east, turned with the shape's direction and
/// mirrored with its flips; each margin rounded and kept within the box.
// mxUtils.getDirectedBounds, mxUtils.js 3187-3238
pub(crate) fn directed_bounds(
    rect: Rect,
    m: Margins,
    direction: Direction,
    flip_h: bool,
    flip_v: bool,
) -> Rect {
    let clamp = |v: f64, max: f64| v.min(max).max(0.0).round();
    let mut m = Margins {
        left: clamp(m.left, rect.w),
        top: clamp(m.top, rect.h),
        right: clamp(m.right, rect.w),
        bottom: clamp(m.bottom, rect.h),
    };
    let vertical = direction.vertical();
    if (flip_v && vertical) || (flip_h && !vertical) {
        std::mem::swap(&mut m.left, &mut m.right);
    }
    if (flip_h && vertical) || (flip_v && !vertical) {
        std::mem::swap(&mut m.top, &mut m.bottom);
    }
    let m = match direction {
        Direction::East => m,
        Direction::South => Margins {
            top: m.left,
            left: m.bottom,
            right: m.top,
            bottom: m.right,
        },
        Direction::West => Margins {
            top: m.bottom,
            left: m.right,
            right: m.left,
            bottom: m.top,
        },
        Direction::North => Margins {
            top: m.right,
            left: m.top,
            right: m.bottom,
            bottom: m.left,
        },
    };
    Rect::new(
        rect.x + m.left,
        rect.y + m.top,
        rect.w - m.right - m.left,
        rect.h - m.bottom - m.top,
    )
}

/// Which way a shape faces (`direction`). Every shape is drawn facing east and turned a quarter
/// at a time, clockwise, for the others (`mxShape.getShapeRotation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    East,
    South,
    West,
    North,
}

impl Direction {
    pub fn of(style: &Resolved) -> Direction {
        match style.get("direction") {
            Some("south") => Direction::South,
            Some("west") => Direction::West,
            Some("north") => Direction::North,
            _ => Direction::East,
        }
    }

    /// How far the shape is turned from facing east, in degrees clockwise.
    pub fn degrees(self) -> f64 {
        match self {
            Direction::East => 0.0,
            Direction::South => 90.0,
            Direction::West => 180.0,
            Direction::North => 270.0,
        }
    }

    /// North and south, which draw the shape into its box turned a quarter
    /// (`mxShape.isPaintBoundsInverted`).
    pub fn vertical(self) -> bool {
        matches!(self, Direction::North | Direction::South)
    }
}

/// `b` turned a quarter about its centre: its width and height swapped (`mxRectangle.rotate90`).
pub(crate) fn rotate90(b: Rect) -> Rect {
    let t = (b.w - b.h) / 2.0;
    Rect::new(b.x + t, b.y - t, b.h, b.w)
}

/// How a vertex's shape goes onto the page: drawn facing east into [`Placement::bounds`], mirrored
/// by its flips, then turned about the centre by its rotation and direction (`mxShape.paint`,
/// `updateTransform` and `mxSvgCanvas2D.rotate`, whose flip-then-turn this composes).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Placement {
    /// What the shape is drawn into: the vertex's box, turned a quarter for a shape facing north
    /// or south.
    pub bounds: Rect,
    /// Mirrored across its vertical and horizontal middle, as drawn (`flipH`/`flipV`, swapped for
    /// a shape facing north or south).
    pub flip_h: bool,
    pub flip_v: bool,
    /// Degrees clockwise about the centre: `rotation` and the direction's quarter turns.
    pub degrees: f64,
}

impl Placement {
    pub fn of(rect: Rect, style: &Resolved) -> Placement {
        let direction = Direction::of(style);
        let (flip_h, flip_v) = (style.flag("flipH", false), style.flag("flipV", false));
        let degrees = style.num("rotation", 0.0) + direction.degrees();
        match direction.vertical() {
            true => Placement {
                bounds: rotate90(rect),
                flip_h: flip_v,
                flip_v: flip_h,
                degrees,
            },
            false => Placement {
                bounds: rect,
                flip_h,
                flip_v,
                degrees,
            },
        }
    }

    /// Where point `p` of the shape as drawn lands on the page.
    pub fn apply(&self, p: Point) -> Point {
        let c = self.bounds.centre();
        let x = if self.flip_h { 2.0 * c.x - p.x } else { p.x };
        let y = if self.flip_v { 2.0 * c.y - p.y } else { p.y };
        geom::rotate(Point::new(x, y), c, self.degrees)
    }
}

/// A polyline with each corner rounded by `arc` when `rounded` (`mxShape.addPoints`), closed
/// back to its first point when `close`.
///
/// mxShape.js 1232-1323, without its `initialMove` argument. Each corner becomes a line stopping
/// `arc` short of it (at most half the segment) and a quad through the corner to `arc` along the
/// next segment.
pub fn add_points(points: &[Point], rounded: bool, arc: f64, close: bool) -> Vec<PathCmd> {
    add_points_except(points, rounded, arc, close, &[])
}

/// [`add_points`] leaving the corners at the indices in `exclude` sharp, as a callout's tail is.
fn add_points_except(
    points: &[Point],
    rounded: bool,
    arc: f64,
    close: bool,
    exclude: &[usize],
) -> Vec<PathCmd> {
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
        if rounded && (dx != 0.0 || dy != 0.0) && !exclude.contains(&(i - 1)) {
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

/// A closed polygon through `pts`, each given from the top-left of `b`, its corners rounded by
/// half the `arcSize` with `rounded=1` except those in `exclude` (the `addPoints` call of
/// `redrawPath` in the shapes built on `mxActor`).
fn polygon(b: Rect, style: &Resolved, pts: &[(f64, f64)], exclude: &[usize]) -> Vec<PathCmd> {
    let pts: Vec<Point> = pts
        .iter()
        .map(|&(x, y)| Point::new(b.x + x, b.y + y))
        .collect();
    let arc = style.num("arcSize", LINE_ARCSIZE) / 2.0;
    add_points_except(&pts, style.flag("rounded", false), arc, true, exclude)
}

/// Straight lines through `pts`, open.
fn polyline(pts: &[Point]) -> Vec<PathCmd> {
    add_points(pts, false, 0.0, false)
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
        assert!(is_known("note") && is_known("rhombus") && is_known("rect"));
        assert!(!is_known("umlLifeline") && !is_known("mxgraph.aws4.lambda") && !is_known(""));
        assert_eq!(
            vertex("umlLifeline", BOX, &style("shape=umlLifeline;", false)),
            vec![Part::body(rect(BOX))],
            "a stand-in is the plain rectangle"
        );
        assert!(vertex("text", BOX, &style("text;", false)).is_empty());
    }

    #[test]
    fn a_shape_is_mirrored_then_turned_into_place() {
        // A triangle's tip, drawn at the middle of the right side.
        let tip = |s: &str| {
            let r = Rect::new(0.0, 0.0, 80.0, 40.0);
            Placement::of(r, &style(s, false)).apply(Point::new(r.right(), r.centre().y))
        };
        assert!(near(tip(""), Point::new(80.0, 20.0)));
        assert!(near(tip("flipH=1;"), Point::new(0.0, 20.0)));
        // Facing south, the shape is drawn 40 wide and 80 high about the same centre, then
        // turned a quarter: its tip ends at the middle of the bottom.
        let place = Placement::of(
            Rect::new(0.0, 0.0, 80.0, 40.0),
            &style("direction=south;", false),
        );
        assert_eq!(place.bounds, Rect::new(20.0, -20.0, 40.0, 80.0));
        assert!(near(
            place.apply(Point::new(60.0, 20.0)),
            Point::new(40.0, 40.0)
        ));
        // flipV mirrors the shape facing south top to bottom on the page, so its tip points up,
        // and a quarter turn more takes it to the right.
        let place = Placement::of(
            Rect::new(0.0, 0.0, 80.0, 40.0),
            &style("direction=south;flipV=1;rotation=90;", false),
        );
        assert!(near(
            place.apply(Point::new(60.0, 20.0)),
            Point::new(60.0, 20.0)
        ));
    }

    #[test]
    fn label_margins_turn_and_mirror_with_the_shape() {
        let r = Rect::new(0.0, 0.0, 100.0, 60.0);
        let bounds = |s: &str| label_bounds("document", r, &style(s, false), false);
        assert_eq!(bounds("shape=document;"), r, "only with boundedLbl");
        let east = bounds("shape=document;boundedLbl=1;");
        assert_eq!(east, Rect::new(0.0, 0.0, 100.0, 42.0), "above the wave");
        let flipped = bounds("shape=document;boundedLbl=1;flipV=1;");
        assert_eq!(flipped, Rect::new(0.0, 18.0, 100.0, 42.0));
        let south = bounds("shape=document;boundedLbl=1;direction=south;");
        assert_eq!(
            south,
            Rect::new(18.0, 0.0, 82.0, 60.0),
            "the wave turned to the left"
        );
    }
}
