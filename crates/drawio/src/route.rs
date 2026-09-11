//! Edge routing: from the two ends, the style and the waypoints to the points an edge is drawn
//! through.

use crate::geom::{Point, Rect};
use crate::style::Resolved;

/// Which outline an end attaches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PerimeterKind {
    #[default]
    Rectangle,
    Ellipse,
}

/// A vertex an edge is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Terminal {
    /// Absolute and unrotated.
    pub bounds: Rect,
    /// Degrees, about the centre of `bounds`.
    pub rotation: f64,
    pub perimeter: PerimeterKind,
    /// The terminal's own `perimeterSpacing`.
    pub perimeter_spacing: f64,
}

/// Everything routing one edge needs, in absolute page coordinates.
#[derive(Debug, Clone, Copy)]
pub struct EdgeInput<'a> {
    /// The edge's resolved style (edge style, constraints, spacings, jetty size, …).
    pub style: &'a Resolved,
    pub source: Option<Terminal>,
    pub target: Option<Terminal>,
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

/// The points the edge is drawn through, first end to last. Empty when neither end is known.
pub fn route(input: &EdgeInput) -> Vec<Point> {
    let _ = input;
    todo!("route::route")
}
