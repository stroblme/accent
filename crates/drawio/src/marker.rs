//! Arrow heads on the ends of an edge.

use crate::geom::{PathCmd, Point};

/// An arrow head's outline, to be stroked like its edge (never dashed) and filled with the
/// edge's stroke colour when `filled`.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub path: Vec<PathCmd>,
    pub filled: bool,
}

/// The head `kind` (`classic`, `block`, `open`, `oval`, `diamond`, `halfCircle`, …) at `end`,
/// pointing along `unit` (the unit vector from the previous point towards `end`). `end` is moved
/// back to where the line should stop so it does not poke through the head. `None`, and `end`
/// left alone, for `none` and every kind not drawn.
pub fn marker(
    kind: &str,
    end: &mut Point,
    unit: Point,
    size: f64,
    stroke_width: f64,
    filled: bool,
) -> Option<Marker> {
    let _ = (kind, end, unit, size, stroke_width, filled);
    todo!("marker::marker")
}
