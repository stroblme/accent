// Derived from draw.io mxgraph/src/shape/mxMarker.js, js/grapheditor/Shapes.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Arrow heads on the ends of an edge.

use crate::geom::{PathCmd, Point, Rect};
use crate::shapes;

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
///
/// `mxMarker.createMarker` with the heads mxMarker.js and Shapes.js register. Open arrows and
/// the half circle are never filled; the others are when `filled`.
// ponytail: the other registered heads (`baseDash`, `doubleBlock`, `manyOptional` in
// mxMarker.js; `dash`, `box`, `cross`, `circle`, `circlePlus`, `async`, `openAsync`,
// `mermaidExtension`, `mermaidDiamond` in Shapes.js; the `ER…` heads of shapes/er/mxER.js) are
// not drawn and leave the line uncut. Each is a short port in the style of those below.
pub fn marker(
    kind: &str,
    end: &mut Point,
    unit: Point,
    size: f64,
    stroke_width: f64,
    filled: bool,
) -> Option<Marker> {
    let (path, filled) = match kind {
        "classic" | "classicThin" | "block" | "blockThin" => {
            let notched = kind.starts_with("classic");
            let width_factor = if kind.ends_with("Thin") { 3.0 } else { 2.0 };
            let path = arrow(end, unit, size, stroke_width, notched, width_factor);
            (path, filled)
        }
        "open" | "openThin" => {
            let width_factor = if kind == "openThin" { 3.0 } else { 2.0 };
            (
                open_arrow(end, unit, size, stroke_width, width_factor),
                false,
            )
        }
        "oval" => (oval(end, unit, size), filled),
        "diamond" | "diamondThin" => (
            diamond(end, unit, size, stroke_width, kind == "diamond"),
            filled,
        ),
        "halfCircle" => (half_circle(end, unit, size, stroke_width), false),
        _ => return None,
    };
    Some(Marker { path, filled })
}

/// `classic` (`notched`, with its back cut in to ¾ of its length) and `block`, `width_factor`
/// 3 for their thin forms (`createArrow`, mxMarker.js 51-98). The tip sits back from `pe` by
/// the stroke's reach past a 26.565° point, `sw · 1.118`, and the line stops at the notch or
/// the back.
fn arrow(
    pe: &mut Point,
    unit: Point,
    size: f64,
    sw: f64,
    notched: bool,
    width_factor: f64,
) -> Vec<PathCmd> {
    let (end_offset_x, end_offset_y) = (unit.x * sw * 1.118, unit.y * sw * 1.118);
    let (unit_x, unit_y) = (unit.x * (size + sw), unit.y * (size + sw));
    let pt = Point::new(pe.x - end_offset_x, pe.y - end_offset_y);
    let f = if notched { 3.0 / 4.0 } else { 1.0 };
    pe.x += -unit_x * f - end_offset_x;
    pe.y += -unit_y * f - end_offset_y;

    let mut path = vec![
        PathCmd::MoveTo(pt),
        PathCmd::LineTo(Point::new(
            pt.x - unit_x - unit_y / width_factor,
            pt.y - unit_y + unit_x / width_factor,
        )),
    ];
    if notched {
        path.push(PathCmd::LineTo(Point::new(
            pt.x - unit_x * 3.0 / 4.0,
            pt.y - unit_y * 3.0 / 4.0,
        )));
    }
    path.push(PathCmd::LineTo(Point::new(
        pt.x + unit_y / width_factor - unit_x,
        pt.y - unit_y - unit_x / width_factor,
    )));
    path.push(PathCmd::Close);
    path
}

/// `open` and `openThin`: the two sides of an arrow without its back (`createOpenArrow`,
/// mxMarker.js 105-136). The line runs on towards the tip, which sits `sw · 1.118` back from
/// `pe`, and ends as far again short of it.
fn open_arrow(pe: &mut Point, unit: Point, size: f64, sw: f64, width_factor: f64) -> Vec<PathCmd> {
    let (end_offset_x, end_offset_y) = (unit.x * sw * 1.118, unit.y * sw * 1.118);
    let (unit_x, unit_y) = (unit.x * (size + sw), unit.y * (size + sw));
    let pt = Point::new(pe.x - end_offset_x, pe.y - end_offset_y);
    pe.x += -end_offset_x * 2.0;
    pe.y += -end_offset_y * 2.0;

    vec![
        PathCmd::MoveTo(Point::new(
            pt.x - unit_x - unit_y / width_factor,
            pt.y - unit_y + unit_x / width_factor,
        )),
        PathCmd::LineTo(pt),
        PathCmd::LineTo(Point::new(
            pt.x + unit_y / width_factor - unit_x,
            pt.y - unit_y - unit_x / width_factor,
        )),
    ]
}

/// `oval`: a circle `size` across centred on the end, the line stopping at its edge
/// (mxMarker.js 141-162).
fn oval(pe: &mut Point, unit: Point, size: f64) -> Vec<PathCmd> {
    let a = size / 2.0;
    let pt = *pe;
    pe.x -= unit.x * a;
    pe.y -= unit.y * a;
    shapes::ellipse(Rect::new(pt.x - a, pt.y - a, size, size))
}

/// `diamond` and `diamondThin` (`diamond`, mxMarker.js 220-262): the tip back from `pe` by the
/// stroke's reach past a 45° point (`1/√2`, written 0.7071 in the JS, or `0.9862` for the thin
/// one), the line stopping at the far corner.
fn diamond(pe: &mut Point, unit: Point, size: f64, sw: f64, wide: bool) -> Vec<PathCmd> {
    let sw_factor = if wide {
        std::f64::consts::FRAC_1_SQRT_2
    } else {
        0.9862
    };
    let (end_offset_x, end_offset_y) = (unit.x * sw * sw_factor, unit.y * sw * sw_factor);
    let (unit_x, unit_y) = (unit.x * (size + sw), unit.y * (size + sw));
    let pt = Point::new(pe.x - end_offset_x, pe.y - end_offset_y);
    pe.x += -unit_x - end_offset_x;
    pe.y += -unit_y - end_offset_y;
    // How wide the diamond is against its length.
    let tk = if wide { 2.0 } else { 3.4 };

    vec![
        PathCmd::MoveTo(pt),
        PathCmd::LineTo(Point::new(
            pt.x - unit_x / 2.0 - unit_y / tk,
            pt.y + unit_x / tk - unit_y / 2.0,
        )),
        PathCmd::LineTo(Point::new(pt.x - unit_x, pt.y - unit_y)),
        PathCmd::LineTo(Point::new(
            pt.x - unit_x / 2.0 + unit_y / tk,
            pt.y - unit_y / 2.0 - unit_x / tk,
        )),
        PathCmd::Close,
    ]
}

/// `halfCircle`: a cup open towards the end, its bottom where the line now stops, `size + sw + 1`
/// back from it (Shapes.js 6611-6628).
fn half_circle(pe: &mut Point, unit: Point, size: f64, sw: f64) -> Vec<PathCmd> {
    let (nx, ny) = (unit.x * (size + sw + 1.0), unit.y * (size + sw + 1.0));
    let pt = *pe;
    pe.x -= nx;
    pe.y -= ny;
    // The JS draws with `pe` after it has been moved, as here.
    vec![
        PathCmd::MoveTo(Point::new(pt.x - ny, pt.y + nx)),
        PathCmd::QuadTo(Point::new(pe.x - ny, pe.y + nx), *pe),
        PathCmd::QuadTo(
            Point::new(pe.x + ny, pe.y - nx),
            Point::new(pt.x + ny, pt.y - nx),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::path_bounds;

    const RIGHT: Point = Point::new(1.0, 0.0);

    /// `kind` at (100, 0) pointing right, size 6 and stroke 1: the head and where the line ends.
    fn head(kind: &str, filled: bool) -> (Option<Marker>, Point) {
        let mut end = Point::new(100.0, 0.0);
        let m = marker(kind, &mut end, RIGHT, 6.0, 1.0, filled);
        (m, end)
    }

    fn points(path: &[PathCmd]) -> usize {
        path.iter()
            .filter(|c| matches!(c, PathCmd::MoveTo(_) | PathCmd::LineTo(_)))
            .count()
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn classic_shortens_the_line_and_has_four_points() {
        let (m, end) = head("classic", true);
        let m = m.unwrap();
        // pe -= unit · ((size + sw) · ¾ + sw · 1.118)
        assert!(
            near(end.x, 100.0 - (7.0 * 0.75 + 1.118)) && end.y == 0.0,
            "{end:?}"
        );
        assert_eq!(points(&m.path), 4);
        assert_eq!(m.path.last(), Some(&PathCmd::Close));
        assert!(m.filled);
        assert_eq!(
            m.path[2],
            PathCmd::LineTo(end),
            "the line stops at the notch"
        );
    }

    #[test]
    fn block_is_closed_and_filled() {
        let (m, end) = head("block", true);
        let m = m.unwrap();
        assert!(near(end.x, 100.0 - (7.0 + 1.118)));
        assert_eq!(points(&m.path), 3);
        assert_eq!(m.path.last(), Some(&PathCmd::Close));
        assert!(m.filled);
        assert!(!head("block", false).0.unwrap().filled);
    }

    #[test]
    fn open_is_never_filled() {
        let (m, end) = head("open", true);
        let m = m.unwrap();
        assert!(!m.filled);
        assert!(!m.path.contains(&PathCmd::Close));
        assert!(near(end.x, 100.0 - 2.0 * 1.118));
    }

    #[test]
    fn oval_is_a_circle_at_the_end() {
        let (m, end) = head("oval", true);
        let m = m.unwrap();
        assert!(m.filled);
        assert!(near(end.x, 97.0));
        let b = path_bounds(&m.path).unwrap();
        assert!(
            near(b.x, 97.0) && near(b.y, -3.0) && near(b.w, 6.0) && near(b.h, 6.0),
            "{b:?}"
        );
    }

    #[test]
    fn half_circle_is_open() {
        let (m, end) = head("halfCircle", true);
        let m = m.unwrap();
        assert!(!m.filled);
        assert!(!m.path.contains(&PathCmd::Close));
        assert!(near(end.x, 92.0), "size + sw + 1 back: {end:?}");
        assert!(matches!(m.path[1], PathCmd::QuadTo(_, p) if p == end));
    }

    #[test]
    fn unknown_marker_is_none() {
        for kind in ["none", "", "ERmany", "box", "baseDash"] {
            let (m, end) = head(kind, true);
            assert!(m.is_none(), "{kind}");
            assert_eq!(end, Point::new(100.0, 0.0), "{kind} leaves the end alone");
        }
    }
}
