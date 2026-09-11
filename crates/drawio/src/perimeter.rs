//! Where an edge meets a vertex's outline.

use crate::geom::{Point, Rect};

/// The point on `bounds`' outline on the way to `next` (`mxPerimeter.RectanglePerimeter`).
/// `orthogonal` projects straight across instead of towards the centre.
pub fn rectangle(bounds: Rect, next: Point, orthogonal: bool) -> Point {
    let _ = (bounds, next, orthogonal);
    todo!("perimeter::rectangle")
}

/// The same for the ellipse inscribed in `bounds` (`mxPerimeter.EllipsePerimeter`).
pub fn ellipse(bounds: Rect, next: Point, orthogonal: bool) -> Point {
    let _ = (bounds, next, orthogonal);
    todo!("perimeter::ellipse")
}
