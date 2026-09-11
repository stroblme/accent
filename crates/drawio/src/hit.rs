//! Which cell is under a point, and which cells a rubber band takes.

use crate::geom::{Point, Rect};
use crate::model::CellId;
use crate::scene::Scene;

impl Scene {
    /// The topmost cell under `p`, locked layers skipped. A stroke counts within `tolerance`
    /// page units (the canvas passes a few screen pixels divided by its zoom).
    pub fn hit(&self, p: Point, tolerance: f64) -> Option<&str> {
        let _ = (p, tolerance);
        todo!("Scene::hit")
    }

    /// The box around everything `id` paints.
    pub fn bounds_of(&self, id: &str) -> Option<Rect> {
        let _ = id;
        todo!("Scene::bounds_of")
    }

    /// Every unlocked cell whose painting lies wholly inside `band`, in paint order.
    pub fn cells_in(&self, band: Rect) -> Vec<CellId> {
        let _ = band;
        todo!("Scene::cells_in")
    }
}
