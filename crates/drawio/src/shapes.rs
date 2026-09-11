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

/// Whether [`vertex`] draws `shape` as itself. Anything else is drawn by the scene as a
/// stand-in.
pub fn is_known(shape: &str) -> bool {
    let _ = shape;
    todo!("shapes::is_known")
}

/// The parts of vertex `shape` filling `bounds`, background first. Empty for shapes with no
/// outline of their own (`text`, `image`).
pub fn vertex(shape: &str, bounds: Rect, style: &Resolved) -> Vec<Part> {
    let _ = (shape, bounds, style);
    todo!("shapes::vertex")
}

/// An edge's line through `points` (ends already shortened for markers): straight segments,
/// rounded corners (`rounded=1`) or a smooth curve (`curved=1`).
pub fn edge_line(points: &[Point], style: &Resolved) -> Vec<PathCmd> {
    let _ = (points, style);
    todo!("shapes::edge_line")
}

/// A `flexArrow` edge: a filled, stroked band along `points` with its heads.
pub fn flex_arrow(points: &[Point], style: &Resolved, stroke_width: f64) -> Vec<Part> {
    let _ = (points, style, stroke_width);
    todo!("shapes::flex_arrow")
}

/// A polyline with each corner rounded by `arc` when `rounded` (`mxShape.addPoints`), closed
/// back to its first point when `close`.
pub fn add_points(points: &[Point], rounded: bool, arc: f64, close: bool) -> Vec<PathCmd> {
    let _ = (points, rounded, arc, close);
    todo!("shapes::add_points")
}
