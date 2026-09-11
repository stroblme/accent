//! Which cell is under a point, and which cells a rubber band takes.

use std::collections::HashMap;

use crate::geom::{self, PathCmd, Point, Rect};
use crate::model::CellId;
use crate::scene::{Prim, Scene};

impl Scene {
    /// The topmost cell under `p`, locked layers skipped. A stroke counts within `tolerance`
    /// page units (the canvas passes a few screen pixels divided by its zoom).
    pub fn hit(&self, p: Point, tolerance: f64) -> Option<&str> {
        self.prims
            .iter()
            .rev()
            .find(|prim| !prim.locked() && hits(prim, p, tolerance))
            .map(Prim::cell)
    }

    /// The box around everything `id` paints.
    pub fn bounds_of(&self, id: &str) -> Option<Rect> {
        self.prims
            .iter()
            .filter(|prim| prim.cell() == id)
            .map(Prim::bounds)
            .reduce(|a, b| a.union(&b))
    }

    /// Every unlocked cell whose painting lies wholly inside `band`, in paint order.
    pub fn cells_in(&self, band: Rect) -> Vec<CellId> {
        let mut cells: Vec<(&str, Rect)> = Vec::new();
        let mut index: HashMap<&str, usize> = HashMap::new();
        for prim in self.prims.iter().filter(|prim| !prim.locked()) {
            let bounds = prim.bounds();
            match index.get(prim.cell()) {
                Some(&i) => cells[i].1 = cells[i].1.union(&bounds),
                None => {
                    index.insert(prim.cell(), cells.len());
                    cells.push((prim.cell(), bounds));
                }
            }
        }
        cells
            .into_iter()
            .filter(|(_, bounds)| band.contains_rect(bounds))
            .map(|(id, _)| id.to_string())
            .collect()
    }
}

/// Whether `prim` takes a click at `p`. A closed outline takes it anywhere inside, filled or
/// not, as draw.io's `pointerEvents` default has it; any outline takes it on its stroke, which
/// reaches half the stroke width or `tolerance`, whichever is more.
fn hits(prim: &Prim, p: Point, tolerance: f64) -> bool {
    match prim {
        Prim::Path { path, stroke, .. } => {
            let lines = geom::flatten(path);
            let reach = stroke
                .as_ref()
                .map_or(0.0, |s| s.width / 2.0)
                .max(tolerance);
            (path.contains(&PathCmd::Close) && inside(&lines, p)) || near(&lines, p, reach)
        }
        Prim::Text {
            rect,
            anchor,
            rotation,
            ..
        } => rect
            .grow(tolerance)
            .contains(geom::rotate(p, *anchor, -rotation)),
        Prim::Image { rect, rotation, .. } => {
            rect.contains(geom::rotate(p, rect.centre(), -rotation))
        }
    }
}

/// Even-odd: whether a ray from `p` crosses the polylines an odd number of times. Each one is
/// closed back to its start, as a fill closes it.
fn inside(lines: &[Vec<Point>], p: Point) -> bool {
    let mut odd = false;
    for line in lines {
        for (a, b) in line.iter().zip(line.iter().cycle().skip(1)) {
            if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y) {
                odd = !odd;
            }
        }
    }
    odd
}

/// Whether `p` is within `reach` of a segment of the polylines.
fn near(lines: &[Vec<Point>], p: Point, reach: f64) -> bool {
    lines
        .iter()
        .flat_map(|line| line.windows(2))
        .any(|s| geom::distance_to_segment(p, s[0], s[1]) <= reach)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{Align, Font, Paint, Stroke, VAlign};
    use crate::shapes::rect as outline;
    use crate::style::Color;

    fn path(cell: &str, path: Vec<PathCmd>, filled: bool, width: f64) -> Prim {
        Prim::Path {
            cell: cell.into(),
            locked: false,
            path,
            fill: filled.then_some(Paint::Solid(Color::WHITE)),
            stroke: Some(Stroke {
                color: Color::BLACK,
                width,
                dash: None,
            }),
            opacity: 1.0,
            shadow: false,
        }
    }

    fn text(cell: &str, rect: Rect, anchor: Point, rotation: f64) -> Prim {
        Prim::Text {
            cell: cell.into(),
            locked: false,
            rect,
            anchor,
            align: Align::Center,
            valign: VAlign::Middle,
            wrap: true,
            rotation,
            font: Font {
                size: 12.0,
                family: "Helvetica".into(),
                color: Color::BLACK,
                bold: false,
                italic: false,
                underline: false,
            },
            runs: Vec::new(),
            background: None,
            border: None,
            opacity: 1.0,
        }
    }

    fn locked(mut prim: Prim) -> Prim {
        match &mut prim {
            Prim::Path { locked, .. } | Prim::Text { locked, .. } | Prim::Image { locked, .. } => {
                *locked = true
            }
        }
        prim
    }

    fn scene(prims: Vec<Prim>) -> Scene {
        Scene {
            prims,
            ..Scene::default()
        }
    }

    #[test]
    fn topmost_unlocked_prim_wins() {
        let s = scene(vec![
            path("a", outline(Rect::new(0.0, 0.0, 100.0, 100.0)), true, 1.0),
            path("b", outline(Rect::new(50.0, 50.0, 100.0, 100.0)), true, 1.0),
            locked(path(
                "c",
                outline(Rect::new(50.0, 50.0, 100.0, 100.0)),
                true,
                1.0,
            )),
        ]);
        assert_eq!(s.hit(Point::new(75.0, 75.0), 2.0), Some("b"));
        assert_eq!(s.hit(Point::new(25.0, 25.0), 2.0), Some("a"));
        assert_eq!(s.hit(Point::new(200.0, 200.0), 2.0), None);
    }

    #[test]
    fn an_unfilled_closed_shape_is_hit_inside() {
        let r = Rect::new(0.0, 0.0, 100.0, 100.0);
        let s = scene(vec![path("box", outline(r), false, 1.0)]);
        assert_eq!(s.hit(Point::new(50.0, 50.0), 2.0), Some("box"));
        assert_eq!(s.hit(Point::new(102.0, 50.0), 3.0), Some("box"));
        assert_eq!(s.hit(Point::new(110.0, 50.0), 3.0), None);
        let mut open = outline(r);
        open.pop();
        let s = scene(vec![path("u", open, false, 1.0)]);
        assert_eq!(
            s.hit(Point::new(50.0, 50.0), 2.0),
            None,
            "the same outline left open is only a line"
        );
    }

    #[test]
    fn an_open_line_is_hit_within_tolerance() {
        let line = vec![
            PathCmd::MoveTo(Point::new(0.0, 0.0)),
            PathCmd::LineTo(Point::new(100.0, 0.0)),
        ];
        let thin = scene(vec![path("e", line.clone(), false, 1.0)]);
        assert_eq!(thin.hit(Point::new(50.0, 2.0), 3.0), Some("e"));
        assert_eq!(thin.hit(Point::new(50.0, 5.0), 3.0), None);
        let thick = scene(vec![path("e", line, false, 12.0)]);
        assert_eq!(
            thick.hit(Point::new(50.0, 5.0), 3.0),
            Some("e"),
            "half the stroke counts"
        );
    }

    #[test]
    fn rotated_text_is_hit_in_its_own_frame() {
        // 100 × 20 turned a quarter about its centre stands 20 wide and 100 tall.
        let s = scene(vec![text(
            "t",
            Rect::new(0.0, 0.0, 100.0, 20.0),
            Point::new(50.0, 10.0),
            90.0,
        )]);
        assert_eq!(s.hit(Point::new(50.0, -30.0), 0.0), Some("t"));
        assert_eq!(s.hit(Point::new(90.0, 10.0), 0.0), None);
    }

    #[test]
    fn band_takes_only_whole_cells() {
        let s = scene(vec![
            path("a", outline(Rect::new(10.0, 10.0, 30.0, 30.0)), true, 1.0),
            path("b", outline(Rect::new(60.0, 10.0, 30.0, 30.0)), true, 1.0),
            text(
                "a",
                Rect::new(10.0, 20.0, 30.0, 10.0),
                Point::new(25.0, 25.0),
                0.0,
            ),
            // b's label hangs out of the band.
            text(
                "b",
                Rect::new(60.0, 45.0, 30.0, 80.0),
                Point::new(75.0, 85.0),
                0.0,
            ),
            locked(path(
                "c",
                outline(Rect::new(20.0, 60.0, 10.0, 10.0)),
                true,
                1.0,
            )),
        ]);
        assert_eq!(s.cells_in(Rect::new(0.0, 0.0, 100.0, 100.0)), ["a"]);
    }

    #[test]
    fn bounds_of_unions_a_cells_prims() {
        let s = scene(vec![
            path("a", outline(Rect::new(0.0, 0.0, 10.0, 10.0)), true, 2.0),
            path("b", outline(Rect::new(50.0, 50.0, 10.0, 10.0)), true, 2.0),
            text(
                "a",
                Rect::new(20.0, 0.0, 10.0, 30.0),
                Point::new(25.0, 15.0),
                0.0,
            ),
        ]);
        assert_eq!(s.bounds_of("a"), Some(Rect::new(-1.0, -1.0, 31.0, 31.0)));
        assert_eq!(s.bounds_of("z"), None);
    }
}
